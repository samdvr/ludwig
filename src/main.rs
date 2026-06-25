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
    Parse {
        path: Option<PathBuf>,
        /// Suppress per-file `ok` output; only print errors and the summary.
        /// Handy for pre-commit hooks that only need the exit code + diagnostics.
        #[arg(long, default_value_t = false)]
        quiet: bool,
    },

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
        /// Emit reports as a JSON array on stdout instead of human-formatted text.
        /// `latest.md` is still written under `.ludwig/reports/`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Show drift between specs and code.
    Diff {
        id: Option<String>,
        #[arg(long, default_value_t = false)]
        all: bool,
        /// Emit drift reports as a JSON array on stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
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

    /// Move an existing spec to a different game (or to the specs root).
    Move {
        slug: String,
        /// Destination game. Omit to move to the specs root.
        #[arg(long = "to-game")]
        to_game: Option<String>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
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
        Cmd::Parse { path, quiet } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let paths = paths_for(&project, path)?;
            let mut failures = 0u32;
            for p in &paths {
                match parser::parse_file(p) {
                    Ok(doc) => {
                        if !quiet {
                            println!(
                                "  ok  {}  ({} v{})",
                                rel(&project, p),
                                doc.id(),
                                doc.version()
                            );
                        }
                    }
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
                if !quiet {
                    println!("Parsed {} spec(s); no structural errors.", paths.len());
                }
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
        Cmd::Verify { id, all, emit_judgment_prompts, ingest_judgments, json } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let v = verify::Verify::new(&project);
            if let Some(path) = ingest_judgments {
                v.ingest_judgments(&path)?;
                if !json {
                    println!("Ingested judgments from {}.", path.display());
                } else {
                    println!("{}", serde_json::json!({ "ingested_from": path.display().to_string() }));
                }
                return Ok(ExitCode::SUCCESS);
            }
            let ids = resolve_ids(&project, id, all)?;
            let mut all_prompts: Vec<verify::JudgmentPrompt> = Vec::new();
            let mut all_reports: Vec<verify::Report> = Vec::new();
            let mut total_pass = 0u32;
            let mut total_fail = 0u32;
            let mut total_pending = 0u32;
            let mut total_skip = 0u32;
            let mut specs_failed = 0u32;
            for spec_id in &ids {
                let report = v.run(
                    spec_id,
                    RunOptions { emit_judgment_prompts },
                )?;
                if emit_judgment_prompts {
                    all_prompts.extend(report.judgment_prompts.iter().cloned());
                } else if json {
                    all_reports.push(report.clone());
                } else {
                    print!("{}", verify::render_text(&report));
                }
                total_pass += report.summary.pass;
                total_fail += report.summary.fail;
                total_pending += report.summary.pending;
                total_skip += report.summary.skip;
                if report.summary.fail > 0 {
                    specs_failed += 1;
                }
            }
            if emit_judgment_prompts {
                println!("{}", serde_json::to_string_pretty(&all_prompts)?);
            } else if json {
                println!("{}", serde_json::to_string_pretty(&all_reports)?);
            } else if ids.len() > 1 {
                // Only print aggregate when the user asked for more than one spec
                // (--all, or multiple ids). For a single id the per-spec footer
                // already carries the same information.
                println!(
                    "\n{} specs verified ({} ok, {} with failures) — checks: pass={} fail={} pending={} skip={}",
                    ids.len(),
                    ids.len() as u32 - specs_failed,
                    specs_failed,
                    total_pass,
                    total_fail,
                    total_pending,
                    total_skip,
                );
            }
            Ok(if specs_failed > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
        }
        Cmd::Diff { id, all, json } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let ids = resolve_ids(&project, id, all)?;
            let mut reports: Vec<drift::DriftReport> = Vec::new();
            for spec_id in ids {
                let report = drift::report(&project, &spec_id)?;
                if json {
                    reports.push(report);
                } else {
                    print!("{}", drift::render_text(&report));
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
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
            let existing_specs_owned = project.list_existing_specs();
            let existing_specs: Vec<prompts::ExistingSpec<'_>> = existing_specs_owned
                .iter()
                .map(|(id, title, status)| prompts::ExistingSpec {
                    id,
                    title,
                    status,
                })
                .collect();
            let existing_games = project.list_existing_games();
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
            let peers_owned = project.peer_specs_for(game.as_deref());
            let peers: Vec<prompts::PeerSpec<'_>> = peers_owned
                .iter()
                .map(|(id, title)| prompts::PeerSpec { id, title })
                .collect();
            let glossary = project.glossary_for(game.as_deref());
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
        Cmd::Move { slug, to_game, force } => {
            let project = Project::discover(std::env::current_dir()?)?;
            let target = scaffold::move_spec(&project, &slug, to_game.as_deref(), force)?;
            let rel = target.strip_prefix(&project.root).unwrap_or(&target);
            println!("Moved {} → {}", slug, rel.display());
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
