use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser as _;

use ludwig::{
    catalog, drift, mcp, parser, plan,
    project::Project,
    prompts,
    scaffold::{self, WriteSpecError},
    verify::{self, RunOptions},
};

#[derive(clap::Parser, Debug)]
#[command(name = "ludwig", version, about = "Specification-driven development")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Initialize Ludwig in the current directory.
    Init,

    /// Scaffold a new spec file.
    New {
        slug: String,
        #[arg(long)]
        game: Option<String>,
    },

    /// Parse one or all specs and report structural errors.
    Parse { path: Option<PathBuf> },

    /// Regenerate `specs/_index.md`.
    Catalog,

    /// Emit the generation brief for spec ID as JSON.
    Plan { id: String },

    /// Run the verification pipeline.
    Verify {
        id: Option<String>,
        #[arg(long, default_value_t = false)]
        all: bool,
        #[arg(long = "emit-judgment-prompts", default_value_t = false)]
        emit_judgment_prompts: bool,
        #[arg(long = "ingest-judgments")]
        ingest_judgments: Option<PathBuf>,
    },

    /// Show drift between specs and code.
    Diff {
        id: Option<String>,
        #[arg(long, default_value_t = false)]
        all: bool,
    },

    /// Start the MCP server in stdio mode.
    Mcp {
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Emit a prompt that decomposes a project description into specs.
    Decompose {
        #[arg(short = 'd', long)]
        description: Option<String>,
    },

    /// Emit a prompt for drafting a spec from a description.
    Propose {
        slug: String,
        #[arg(short = 'd', long)]
        description: String,
        #[arg(short = 'g', long)]
        game: Option<String>,
    },

    /// Validate a spec drafted by an agent (markdown on stdin) and persist it.
    #[command(name = "write-spec")]
    WriteSpec {
        slug: String,
        #[arg(short = 'g', long)]
        game: Option<String>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Create a language-game (specs/NAME/_game.md).
    #[command(name = "game-new")]
    GameNew {
        name: String,
        #[arg(short = 'i', long)]
        intent: Option<String>,
        #[arg(short = 'x', long = "glossary", value_parser = parse_kv, num_args = 0..)]
        glossary: Vec<(String, String)>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Print the Ludwig version.
    Version,
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once(':')
        .ok_or_else(|| format!("expected `Key:value`, got {s:?}"))?;
    Ok((k.trim().to_string(), v.trim().to_string()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.cmd {
        Cmd::Version => {
            println!("ludwig {}", ludwig::VERSION);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Init => {
            let cwd = std::env::current_dir()?;
            let written = scaffold::init(&cwd)?;
            if written.is_empty() {
                println!("Ludwig already initialized; nothing to do.");
            } else {
                println!("Wrote:");
                for p in written {
                    let rel = p.strip_prefix(&cwd).unwrap_or(&p);
                    println!("  {}", rel.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::New { slug, game } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let target = scaffold::new_spec(&project, &slug, game.as_deref())?;
            let rel = target.strip_prefix(&project.root).unwrap_or(&target);
            println!("Created {}", rel.display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Parse { path } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let paths = paths_for(&project, path)?;
            let mut failures = 0u32;
            for p in &paths {
                match parser::parse_file(p) {
                    Ok(doc) => println!(
                        "  ok  {}  ({} v{})",
                        rel(&project, p),
                        doc.id(),
                        doc.version()
                    ),
                    Err(e) => {
                        failures += 1;
                        println!("  err {}", rel(&project, p));
                        println!("      {}", e.message);
                    }
                }
            }
            if failures > 0 {
                Ok(ExitCode::from(1))
            } else {
                println!("Parsed {} spec(s); no structural errors.", paths.len());
                Ok(ExitCode::SUCCESS)
            }
        }
        Cmd::Catalog => {
            let project = Project::discover(std::env::current_dir()?)?;
            let target = catalog::write(&project)?;
            let rel = target.strip_prefix(&project.root).unwrap_or(&target);
            println!("Wrote {}", rel.display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Plan { id } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let brief = plan::brief_for(&project, &id)?;
            println!("{}", serde_json::to_string_pretty(&brief)?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Verify { id, all, emit_judgment_prompts, ingest_judgments } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let v = verify::Verify::new(&project);
            if let Some(path) = ingest_judgments {
                v.ingest_judgments(&path)?;
                println!("Ingested judgments from {}.", path.display());
                return Ok(ExitCode::SUCCESS);
            }
            let ids = resolve_ids(&project, id, all)?;
            let mut all_prompts: Vec<verify::JudgmentPrompt> = Vec::new();
            let mut any_failures = false;
            for spec_id in &ids {
                let report = v.run(
                    spec_id,
                    RunOptions { emit_judgment_prompts },
                )?;
                if emit_judgment_prompts {
                    all_prompts.extend(report.judgment_prompts.iter().cloned());
                } else {
                    print!("{}", verify::render_text(&report));
                }
                if report.summary.fail > 0 {
                    any_failures = true;
                }
            }
            if emit_judgment_prompts {
                println!("{}", serde_json::to_string_pretty(&all_prompts)?);
            }
            Ok(if any_failures { ExitCode::from(1) } else { ExitCode::SUCCESS })
        }
        Cmd::Diff { id, all } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let ids = resolve_ids(&project, id, all)?;
            for spec_id in ids {
                let report = drift::report(&project, &spec_id)?;
                print!("{}", drift::render_text(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mcp { root } => {
            mcp::Server::new(None, root).run()?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Decompose { description } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let description = match description {
                Some(d) => d,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            if description.trim().is_empty() {
                anyhow::bail!("no description provided (use --description or pipe on stdin)");
            }
            let existing_specs_owned: Vec<(String, String, String)> = project
                .spec_paths()
                .iter()
                .filter_map(|p| {
                    parser::parse_file(p).ok().map(|d| {
                        (
                            d.id().to_string(),
                            d.frontmatter.title.clone(),
                            d.frontmatter.status.as_str().to_string(),
                        )
                    })
                })
                .collect();
            let existing_specs: Vec<prompts::ExistingSpec<'_>> = existing_specs_owned
                .iter()
                .map(|(id, title, status)| prompts::ExistingSpec {
                    id,
                    title,
                    status,
                })
                .collect();
            let mut existing_games: Vec<String> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(project.specs_dir()) {
                for e in rd.flatten() {
                    if e.path().is_dir()
                        && let Some(name) = e.file_name().to_str()
                    {
                        existing_games.push(name.to_string());
                    }
                }
            }
            println!(
                "{}",
                prompts::project_decomposition(
                    description.trim(),
                    &existing_specs,
                    &existing_games,
                )
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Propose { slug, description, game } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let peers_owned = peer_specs_for(&project, game.as_deref());
            let peers: Vec<prompts::PeerSpec<'_>> = peers_owned
                .iter()
                .map(|(id, title)| prompts::PeerSpec { id, title })
                .collect();
            let glossary = glossary_for(&project, game.as_deref());
            println!(
                "{}",
                prompts::spec_from_description(
                    &slug,
                    &description,
                    game.as_deref(),
                    &peers,
                    &glossary,
                )
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::WriteSpec { slug, game, force } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            if content.trim().is_empty() {
                anyhow::bail!("no markdown on stdin");
            }
            match scaffold::write_spec(&project, &slug, &content, game.as_deref(), force) {
                Ok(target) => {
                    let rel = target.strip_prefix(&project.root).unwrap_or(&target);
                    println!("Wrote {}", rel.display());
                    Ok(ExitCode::SUCCESS)
                }
                Err(WriteSpecError::Parse(e)) => anyhow::bail!("{}", e.message),
                Err(WriteSpecError::Project(e)) => anyhow::bail!("{}", e.0),
            }
        }
        Cmd::GameNew { name, intent, glossary, force } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let target = scaffold::create_game(
                &project,
                &name,
                intent.as_deref(),
                &glossary,
                force,
            )?;
            let rel = target.strip_prefix(&project.root).unwrap_or(&target);
            println!("Wrote {}", rel.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn paths_for(project: &Project, path: Option<PathBuf>) -> anyhow::Result<Vec<PathBuf>> {
    match path {
        None => Ok(project.spec_paths()),
        Some(p) => {
            let p = if p.is_absolute() { p } else { project.root.join(p) };
            if !p.is_file() {
                anyhow::bail!("no such file: {}", p.display());
            }
            Ok(vec![p])
        }
    }
}

fn rel(project: &Project, path: &std::path::Path) -> String {
    path.strip_prefix(&project.root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn resolve_ids(
    project: &Project,
    id: Option<String>,
    all: bool,
) -> anyhow::Result<Vec<String>> {
    if all {
        let mut out: Vec<String> = Vec::new();
        for p in project.spec_paths() {
            if let Ok(doc) = parser::parse_file(&p) {
                out.push(doc.id().to_string());
            }
        }
        Ok(out)
    } else if let Some(id) = id {
        Ok(vec![id])
    } else {
        anyhow::bail!("specify an ID or pass --all");
    }
}

fn peer_specs_for(project: &Project, game_name: Option<&str>) -> Vec<(String, String)> {
    let dir = match game_name {
        Some(g) => project.specs_dir().join(g),
        None => project.specs_dir(),
    };
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if !p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".spec.md"))
                .unwrap_or(false)
            {
                continue;
            }
            if let Ok(doc) = parser::parse_file(&p) {
                out.push((doc.id().to_string(), doc.frontmatter.title.clone()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn glossary_for(project: &Project, game_name: Option<&str>) -> Vec<(String, String)> {
    let Some(g) = game_name else { return Vec::new() };
    let manifest = project
        .specs_dir()
        .join(g)
        .join(ludwig::game::Game::MANIFEST_FILE);
    if !manifest.is_file() {
        return Vec::new();
    }
    match ludwig::game::Game::load(&manifest, project) {
        Ok(game) => game.glossary.into_iter().collect(),
        Err(_) => Vec::new(),
    }
}
