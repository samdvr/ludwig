use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::error::ProjectError;
use crate::game::Game;
use crate::parser;
use crate::project::Project;
use crate::spec::Status;

struct Entry {
    id: String,
    title: String,
    status: Status,
    path_rel_specs: String,
}

pub fn render(project: &Project) -> String {
    let mut groups: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    let specs_dir = project.specs_dir();

    for path in project.spec_paths() {
        let doc = match parser::parse_file(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let game = Game::for_spec(project, &path);
        let rel = path
            .strip_prefix(&specs_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        groups.entry(game.name).or_default().push(Entry {
            id: doc.id().to_string(),
            title: doc.frontmatter.title.clone(),
            status: doc.frontmatter.status,
            path_rel_specs: rel,
        });
    }

    let mut out = String::from("# Spec catalog\n\n");
    if groups.is_empty() {
        out.push_str("_No specs yet. Run `ludwig new <slug>` to create one._\n");
        return out;
    }

    for (game_name, mut entries) in groups {
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        out.push_str(&format!("## {game_name}\n\n"));
        out.push_str("| id | title | status | file |\n");
        out.push_str("|---|---|---|---|\n");
        for e in entries {
            out.push_str(&format!(
                "| `{}` | {} | {} | `{}` |\n",
                escape_md_cell(&e.id),
                escape_md_cell(&e.title),
                e.status.as_str(),
                escape_md_cell(&e.path_rel_specs),
            ));
        }
        out.push('\n');
    }
    out
}

/// Escape characters that would break a Markdown table row: `|` and `\` need
/// backslash-escaping, newlines must collapse to a single space.
fn escape_md_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

pub fn write(project: &Project) -> Result<PathBuf, ProjectError> {
    let content = render(project);
    let specs_dir = project.specs_dir();
    fs::create_dir_all(&specs_dir)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", specs_dir.display())))?;
    let target = specs_dir.join("_index.md");
    crate::util::atomic_write(&target, content.as_bytes())
        .map_err(|e| ProjectError::new(format!("write {}: {e}", target.display())))?;
    Ok(target)
}
