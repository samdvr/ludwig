use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::ParseError;
use crate::project::Project;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Game {
    pub name: String,
    pub path: String,
    pub glossary: BTreeMap<String, String>,
    pub inherits: Vec<String>,
}

impl Game {
    pub const MANIFEST_FILE: &'static str = "_game.md";

    /// Resolve the game for a spec by walking up from the spec's dir to the project's
    /// specs_dir. Inner games override outer ones for the *name*; glossaries are merged
    /// with outer entries appearing first so inner definitions win.
    pub fn for_spec(project: &Project, spec_path: &Path) -> Self {
        let abs = absolute(spec_path);
        let mut stack: Vec<Game> = Vec::new();
        let specs_root = absolute(&project.specs_dir());

        let mut cursor = abs.parent().map(|p| p.to_path_buf());
        while let Some(dir) = cursor {
            // Only walk while we're still inside (or at) the specs root.
            if !path_starts_with(&dir, &specs_root) {
                break;
            }
            let manifest = dir.join(Self::MANIFEST_FILE);
            if manifest.is_file() {
                if let Ok(game) = Self::load(&manifest, project) {
                    stack.insert(0, game);
                }
            }
            if dir == specs_root {
                break;
            }
            cursor = dir.parent().map(|p| p.to_path_buf());
        }
        resolve(stack)
    }

    pub fn load(manifest_path: &Path, project: &Project) -> Result<Self, ParseError> {
        let raw = fs::read_to_string(manifest_path)
            .map_err(|e| ParseError::at(Some(manifest_path), format!("read failed: {e}")))?;
        let (front, body) = split_frontmatter(&raw, manifest_path)?;
        let name = match front.get("name") {
            Some(serde_yaml::Value::String(s)) => s.clone(),
            _ => manifest_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
        };
        let inherits = match front.get("inherits") {
            Some(serde_yaml::Value::Sequence(seq)) => seq
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        };
        let glossary = parse_glossary(&body);
        let rel = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .strip_prefix(&project.root)
            .unwrap_or_else(|_| Path::new("."))
            .to_string_lossy()
            .into_owned();
        Ok(Self { name, path: rel, glossary, inherits })
    }

    pub fn root() -> Self {
        Self {
            name: "(root)".to_string(),
            path: ".".to_string(),
            glossary: BTreeMap::new(),
            inherits: Vec::new(),
        }
    }
}

fn resolve(stack: Vec<Game>) -> Game {
    if stack.is_empty() {
        return Game::root();
    }
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    for g in &stack {
        for (k, v) in &g.glossary {
            merged.insert(k.clone(), v.clone());
        }
    }
    let mut last = stack.into_iter().last().unwrap();
    last.glossary = merged;
    last
}

fn split_frontmatter(
    text: &str,
    source: &Path,
) -> Result<(BTreeMap<String, serde_yaml::Value>, String), ParseError> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    if lines.first().map(|l| l.trim()) != Some("---") {
        return Ok((BTreeMap::new(), normalized));
    }
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_idx = Some(i);
            break;
        }
    }
    let end = end_idx.ok_or_else(|| {
        ParseError::at(Some(source), "unterminated _game.md frontmatter")
    })?;
    let front_yaml = lines[1..end].join("\n");
    let body = if end + 1 <= lines.len() {
        lines[end + 1..].join("\n")
    } else {
        String::new()
    };
    let parsed: serde_yaml::Value = serde_yaml::from_str(&front_yaml)
        .unwrap_or(serde_yaml::Value::Null);
    let map = if let serde_yaml::Value::Mapping(m) = parsed {
        let mut out = BTreeMap::new();
        for (k, v) in m {
            if let serde_yaml::Value::String(s) = k {
                out.insert(s, v);
            }
        }
        out
    } else {
        BTreeMap::new()
    };
    Ok((map, body))
}

/// Glossary entries look like `- **Term**: definition.` — anything else is ignored.
fn parse_glossary(body: &str) -> BTreeMap<String, String> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\s*[-*+]\s+\*\*([^*]+)\*\*\s*:\s*(.+)$").unwrap()
    });
    let mut out = BTreeMap::new();
    for line in body.split('\n') {
        if let Some(c) = RE.captures(line) {
            let term = c.get(1).unwrap().as_str().trim().to_string();
            let defn = c.get(2).unwrap().as_str().trim().to_string();
            out.insert(term, defn);
        }
    }
    out
}

fn absolute(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn path_starts_with(child: &Path, parent: &Path) -> bool {
    child.starts_with(parent)
}
