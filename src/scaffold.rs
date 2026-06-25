use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{ParseError, ProjectError};
use crate::game::Game;
use crate::parser;
use crate::project::{self, Project, State};

pub const DEFAULT_CONFIG_YAML: &str = "\
# ludwig.yml — project configuration.
#
# canonical:  \"spec\" (default) or \"code\". Determines which side is the
#             source of truth when drift is detected.
# specs_dir:  directory containing .spec.md files. Defaults to \"specs\".
# state_dir:  directory for Ludwig's bookkeeping. Defaults to \".ludwig\".
canonical: spec
specs_dir: specs
state_dir: .ludwig
";

pub const GITIGNORE_LINES: &str = "\
# Ludwig
/.ludwig/cache/
/.ludwig/pending/
/.ludwig/reports/
";

/// Initialize a Ludwig project. Idempotent. Returns the list of paths that were created
/// or modified during this call.
pub fn init(root: &Path) -> Result<Vec<PathBuf>, ProjectError> {
    if !root.is_dir() {
        return Err(ProjectError::new(format!(
            "{} does not exist",
            root.display()
        )));
    }
    let mut written: Vec<PathBuf> = Vec::new();

    let config = root.join(project::CONFIG_FILE);
    if !config.is_file() {
        fs::write(&config, DEFAULT_CONFIG_YAML)
            .map_err(|e| ProjectError::new(format!("write {}: {e}", config.display())))?;
        written.push(config);
    }

    let specs = root.join(project::DEFAULT_SPECS_DIR);
    let specs_was_empty = !specs.is_dir() || dir_is_empty(&specs);
    fs::create_dir_all(&specs)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", specs.display())))?;
    if specs_was_empty {
        written.push(specs);
    }

    let state = root.join(project::DEFAULT_STATE_DIR);
    fs::create_dir_all(state.join("cache"))
        .map_err(|e| ProjectError::new(format!("mkdir cache: {e}")))?;
    fs::create_dir_all(state.join("reports"))
        .map_err(|e| ProjectError::new(format!("mkdir reports: {e}")))?;
    fs::create_dir_all(state.join("pending"))
        .map_err(|e| ProjectError::new(format!("mkdir pending: {e}")))?;
    let state_file = state.join(project::STATE_FILE);
    if !state_file.is_file() {
        let empty = State::default();
        let mut bytes = serde_json::to_vec_pretty(&empty)
            .map_err(|e| ProjectError::new(format!("serialize state.json: {e}")))?;
        bytes.push(b'\n');
        fs::write(&state_file, &bytes)
            .map_err(|e| ProjectError::new(format!("write {}: {e}", state_file.display())))?;
        written.push(state_file);
    }

    let gitignore = root.join(".gitignore");
    if gitignore.is_file() {
        let current = fs::read_to_string(&gitignore)
            .map_err(|e| ProjectError::new(format!("read .gitignore: {e}")))?;
        if !current.contains("# Ludwig") {
            let mut merged = current.trim_end().to_string();
            merged.push_str("\n\n");
            merged.push_str(GITIGNORE_LINES);
            fs::write(&gitignore, merged)
                .map_err(|e| ProjectError::new(format!("write .gitignore: {e}")))?;
            written.push(gitignore);
        }
    }

    // Register the Claude Code skill (idempotent: only writes if absent).
    let skills_dir = root.join(".claude").join("skills");
    fs::create_dir_all(&skills_dir)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", skills_dir.display())))?;
    let skill_path = skills_dir.join("ludwig.yaml");
    if !skill_path.is_file() {
        fs::write(&skill_path, crate::skill::manifest_yaml())
            .map_err(|e| ProjectError::new(format!("write {}: {e}", skill_path.display())))?;
        written.push(skill_path);
    }

    Ok(written)
}

pub fn new_spec(project: &Project, slug: &str, game: Option<&str>) -> Result<PathBuf, ProjectError> {
    validate_slug(slug)?;
    let dir = match game {
        Some(g) => project.specs_dir().join(g),
        None => project.specs_dir(),
    };
    fs::create_dir_all(&dir)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", dir.display())))?;
    let basename = Path::new(slug)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(slug);
    let target = dir.join(format!("{basename}.spec.md"));
    if target.is_file() {
        return Err(ProjectError::new(format!(
            "spec already exists: {}",
            target.display()
        )));
    }
    fs::write(&target, spec_template(slug))
        .map_err(|e| ProjectError::new(format!("write {}: {e}", target.display())))?;
    Ok(target)
}

pub fn spec_template(slug: &str) -> String {
    format!(
        "---
id: {slug}
title: TODO
status: draft
owners: []
implements: []
depends_on: []
version: 1
---

## Intent
TODO: Explain in 1–3 sentences why this exists. What problem does it solve,
and for whom? Avoid restating the title. Keep it between 20 and 250 words —
stubs are not specifications, and essays belong in Implementation notes.

## Behavior
- {{#b1}} TODO: the first thing this does, stated in present tense.

## Examples
```example name=\"happy path\"
Given TODO: initial state in plain English
When TODO: the call being made
Then TODO: the observable outcome
```

## Invariants
- {{deterministic}} TODO: a machine-checkable assertion over inputs/outputs.
"
    )
}

#[derive(Debug, thiserror::Error)]
pub enum WriteSpecError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Project(#[from] ProjectError),
}

/// Persist a complete spec markdown document. Parses through the validator first;
/// if validation fails, raises without writing.
pub fn write_spec(
    project: &Project,
    slug: &str,
    content: &str,
    game: Option<&str>,
    force: bool,
) -> Result<PathBuf, WriteSpecError> {
    validate_slug(slug).map_err(WriteSpecError::Project)?;
    let doc = parser::parse(content)?;
    if doc.id() != slug {
        return Err(WriteSpecError::Project(ProjectError::new(format!(
            "slug/id mismatch: requested slug={slug:?} but frontmatter id={:?}",
            doc.id()
        ))));
    }

    let dir = match game {
        Some(g) => project.specs_dir().join(g),
        None => project.specs_dir(),
    };
    fs::create_dir_all(&dir)
        .map_err(|e| WriteSpecError::Project(ProjectError::new(format!("mkdir {}: {e}", dir.display()))))?;

    let basename = Path::new(slug)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(slug);
    let target = dir.join(format!("{basename}.spec.md"));
    if target.is_file() && !force {
        let rel = target.strip_prefix(&project.root).unwrap_or(&target).to_path_buf();
        return Err(WriteSpecError::Project(ProjectError::new(format!(
            "spec already exists at {}; pass force: true to overwrite",
            rel.display()
        ))));
    }
    fs::write(&target, content)
        .map_err(|e| WriteSpecError::Project(ProjectError::new(format!("write {}: {e}", target.display()))))?;
    Ok(target)
}

#[derive(Debug, Default, Serialize)]
pub struct GameOptions<'a> {
    pub intent: Option<&'a str>,
    pub glossary: &'a [(String, String)],
    pub force: bool,
}

pub fn create_game(
    project: &Project,
    name: &str,
    intent: Option<&str>,
    glossary: &[(String, String)],
    force: bool,
) -> Result<PathBuf, ProjectError> {
    validate_slug(name)?;
    let dir = project.specs_dir().join(name);
    fs::create_dir_all(&dir)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", dir.display())))?;
    let target = dir.join(Game::MANIFEST_FILE);
    if target.is_file() && !force {
        let rel = target.strip_prefix(&project.root).unwrap_or(&target).to_path_buf();
        return Err(ProjectError::new(format!(
            "game manifest already exists at {}",
            rel.display()
        )));
    }
    fs::write(&target, render_game_manifest(name, intent, glossary))
        .map_err(|e| ProjectError::new(format!("write {}: {e}", target.display())))?;
    Ok(target)
}

pub fn render_game_manifest(name: &str, intent: Option<&str>, glossary: &[(String, String)]) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str("inherits: []\n");
    out.push_str("---\n");

    if let Some(text) = intent {
        let text = text.trim();
        if !text.is_empty() {
            out.push_str("\n## Intent\n");
            out.push_str(text);
            out.push('\n');
        }
    }

    out.push_str("\n## Glossary\n");
    if glossary.is_empty() {
        out.push_str("_No terms defined yet._\n");
    } else {
        for (term, defn) in glossary {
            out.push_str(&format!("- **{term}**: {defn}\n"));
        }
    }
    out
}

/// Move a spec from its current location to `specs/<to_game>/<slug>.spec.md`
/// (or the project's specs root if `to_game` is None). Reads the source,
/// re-validates the markdown so the moved spec still parses, then writes the
/// new file and removes the original. Returns the new path.
///
/// `force` overwrites a destination that already exists. The spec id must
/// match `slug` — moving cannot rename.
pub fn move_spec(
    project: &Project,
    slug: &str,
    to_game: Option<&str>,
    force: bool,
) -> Result<PathBuf, ProjectError> {
    validate_slug(slug)?;
    let source = project
        .find_spec_path(slug)
        .ok_or_else(|| ProjectError::new(format!("no spec found with id {slug:?}")))?;

    let content = fs::read_to_string(&source)
        .map_err(|e| ProjectError::new(format!("read {}: {e}", source.display())))?;
    let doc = parser::parse(&content)
        .map_err(|e| ProjectError::new(format!("source spec no longer parses: {}", e.message)))?;
    if doc.id() != slug {
        return Err(ProjectError::new(format!(
            "id mismatch: source frontmatter declares {:?}, requested {slug:?}",
            doc.id()
        )));
    }

    let dest_dir = match to_game {
        Some(g) => project.specs_dir().join(g),
        None => project.specs_dir(),
    };
    fs::create_dir_all(&dest_dir)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", dest_dir.display())))?;
    let basename = Path::new(slug)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(slug);
    let target = dest_dir.join(format!("{basename}.spec.md"));

    if source == target {
        return Ok(target);
    }
    if target.is_file() && !force {
        let rel = target.strip_prefix(&project.root).unwrap_or(&target).to_path_buf();
        return Err(ProjectError::new(format!(
            "destination already exists at {}; pass force: true to overwrite",
            rel.display()
        )));
    }

    fs::write(&target, &content)
        .map_err(|e| ProjectError::new(format!("write {}: {e}", target.display())))?;
    fs::remove_file(&source)
        .map_err(|e| ProjectError::new(format!("remove {}: {e}", source.display())))?;

    // Clean up the source game directory if it's now empty (and isn't the
    // specs root itself). Leaves a tidy specs/ tree without surprising the
    // user by removing anything they didn't explicitly own.
    if let Some(parent) = source.parent()
        && parent != project.specs_dir()
        && dir_is_empty(parent)
    {
        let _ = fs::remove_dir(parent);
    }

    Ok(target)
}

pub fn validate_slug(slug: &str) -> Result<(), ProjectError> {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^[a-z0-9][a-z0-9\-/]*[a-z0-9]$").unwrap());
    if !RE.is_match(slug) {
        return Err(ProjectError::new(format!(
            "slug must be kebab-case (lowercase, digits, dashes; slashes allowed for sub-games): {slug:?}"
        )));
    }
    Ok(())
}

fn dir_is_empty(p: &Path) -> bool {
    fs::read_dir(p).map(|mut it| it.next().is_none()).unwrap_or(true)
}
