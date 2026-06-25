use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ProjectError;
use crate::parser;

pub const CONFIG_FILE: &str = "ludwig.yml";
pub const STATE_FILE: &str = "state.json";
pub const DEFAULT_SPECS_DIR: &str = "specs";
pub const DEFAULT_STATE_DIR: &str = ".ludwig";

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_canonical")]
    pub canonical: String,
    #[serde(default = "default_specs_dir")]
    pub specs_dir: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
}

fn default_canonical() -> String { "spec".to_string() }
fn default_specs_dir() -> String { DEFAULT_SPECS_DIR.to_string() }
fn default_state_dir() -> String { DEFAULT_STATE_DIR.to_string() }

impl Default for Config {
    fn default() -> Self {
        Self {
            canonical: default_canonical(),
            specs_dir: default_specs_dir(),
            state_dir: default_state_dir(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub specs: BTreeMap<String, SpecState>,
    #[serde(default)]
    pub judgments: BTreeMap<String, JudgmentVerdict>,
    #[serde(default)]
    pub last_run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecState {
    pub version: u32,
    pub hash: String,
    #[serde(default)]
    pub implementing_files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgmentVerdict {
    pub verdict: String,
    pub rationale: Option<String>,
    pub spec_id: Option<String>,
    pub spec_hash: Option<String>,
}

impl Project {
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let start_abs = canonicalize_or_use(start.as_ref());
        let mut cursor = start_abs.clone();
        loop {
            let candidate = cursor.join(CONFIG_FILE);
            if candidate.is_file() {
                return Self::open(&cursor);
            }
            match cursor.parent() {
                Some(parent) if parent != cursor => cursor = parent.to_path_buf(),
                _ => break,
            }
        }
        Err(ProjectError::new(format!(
            "no {CONFIG_FILE} found in {} or any parent directory; run `ludwig init`",
            start_abs.display()
        )))
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let root = canonicalize_or_use(root.as_ref());
        let config = load_config(&root)?;
        Ok(Self { root, config })
    }

    pub fn specs_dir(&self) -> PathBuf { self.root.join(&self.config.specs_dir) }
    pub fn state_dir(&self) -> PathBuf { self.root.join(&self.config.state_dir) }
    pub fn state_path(&self) -> PathBuf { self.state_dir().join(STATE_FILE) }
    pub fn reports_dir(&self) -> PathBuf { self.state_dir().join("reports") }
    pub fn cache_dir(&self) -> PathBuf { self.state_dir().join("cache") }
    pub fn pending_dir(&self) -> PathBuf { self.state_dir().join("pending") }
    pub fn canonical_mode(&self) -> &str { &self.config.canonical }

    /// All `*.spec.md` files under specs_dir, sorted.
    pub fn spec_paths(&self) -> Vec<PathBuf> {
        let dir = self.specs_dir();
        if !dir.is_dir() {
            return Vec::new();
        }
        let mut out: Vec<PathBuf> = walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".spec.md"))
                    .unwrap_or(false)
            })
            .map(|e| e.into_path())
            .collect();
        out.sort();
        out
    }

    /// Look up a spec by id, or treat the argument as a path.
    pub fn find_spec_path(&self, id_or_path: &str) -> Option<PathBuf> {
        let pathish = Path::new(id_or_path);
        if pathish.is_absolute() && pathish.is_file() {
            return Some(pathish.to_path_buf());
        }
        let rooted = self.root.join(pathish);
        if rooted.is_file() {
            return Some(rooted);
        }
        for p in self.spec_paths() {
            if let Ok(doc) = parser::parse_file(&p)
                && doc.id() == id_or_path
            {
                return Some(p);
            }
        }
        None
    }

    pub fn load_state(&self) -> Result<State, ProjectError> {
        let path = self.state_path();
        if !path.is_file() {
            return Ok(State::default());
        }
        let bytes = fs::read(&path)
            .map_err(|e| ProjectError::new(format!("read {}: {e}", path.display())))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| ProjectError::new(format!("{} is not valid JSON: {e}", path.display())))
    }

    pub fn write_state(&self, state: &State) -> Result<(), ProjectError> {
        let dir = self.state_dir();
        fs::create_dir_all(&dir)
            .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", dir.display())))?;
        let mut bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| ProjectError::new(format!("serialize state: {e}")))?;
        bytes.push(b'\n');
        fs::write(self.state_path(), &bytes)
            .map_err(|e| ProjectError::new(format!("write state: {e}")))
    }

    /// List the `(id, title)` of every spec under `specs/<game>/` (or directly
    /// under `specs/` if `game_name` is None). Used by CLI and MCP to populate
    /// the "peer specs" context block in a drafting prompt.
    pub fn peer_specs_for(&self, game_name: Option<&str>) -> Vec<(String, String)> {
        let dir = match game_name {
            Some(g) => self.specs_dir().join(g),
            None => self.specs_dir(),
        };
        if !dir.is_dir() {
            return Vec::new();
        }
        let mut out: Vec<(String, String)> = Vec::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                if !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".spec.md"))
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

    /// Return the resolved glossary for `specs/<game>/_game.md`, or an empty
    /// list if no game is named or the manifest is absent. Used by CLI and MCP
    /// to populate the "glossary" context block in a drafting prompt.
    pub fn glossary_for(&self, game_name: Option<&str>) -> Vec<(String, String)> {
        let Some(g) = game_name else { return Vec::new() };
        let manifest = self.specs_dir().join(g).join(crate::game::Game::MANIFEST_FILE);
        if !manifest.is_file() {
            return Vec::new();
        }
        match crate::game::Game::load(&manifest, self) {
            Ok(game) => game.glossary.into_iter().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// List `(id, title, status_str)` of every parseable spec in the project.
    /// Convenience for prompt-builders that need a stable, pre-sorted summary.
    pub fn list_existing_specs(&self) -> Vec<(String, String, String)> {
        self.spec_paths()
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
            .collect()
    }

    /// List the sub-directory names directly under `specs/` — each one is a
    /// candidate language-game even if it does not yet have a `_game.md`.
    pub fn list_existing_games(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Ok(rd) = fs::read_dir(self.specs_dir()) {
            for e in rd.flatten() {
                if e.path().is_dir()
                    && let Some(n) = e.file_name().to_str()
                {
                    out.push(n.to_string());
                }
            }
        }
        out.sort();
        out
    }
}

fn load_config(root: &Path) -> Result<Config, ProjectError> {
    let path = root.join(CONFIG_FILE);
    let raw = fs::read_to_string(&path)
        .map_err(|e| ProjectError::new(format!("read {}: {e}", path.display())))?;
    if raw.trim().is_empty() {
        return Ok(Config::default());
    }
    let parsed: serde_yaml::Value = serde_yaml::from_str(&raw)
        .map_err(|e| ProjectError::new(format!("{CONFIG_FILE} invalid: {e}")))?;
    if !parsed.is_mapping() {
        return Err(ProjectError::new(format!("{CONFIG_FILE} must contain a mapping")));
    }
    // Merge with defaults: deserialize directly, missing keys default.
    let cfg: Config = serde_yaml::from_value(parsed)
        .map_err(|e| ProjectError::new(format!("{CONFIG_FILE} schema: {e}")))?;
    Ok(cfg)
}

fn canonicalize_or_use(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
