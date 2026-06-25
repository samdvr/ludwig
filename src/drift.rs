use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::ProjectError;
use crate::parser;
use crate::plan;
use crate::project::{Project, SpecState};
use crate::spec::Document;

/// `ludwig-spec: <id>@<version> hash=<sha>` — captures id, version, hash. The id
/// pattern allows `/` so that sub-game spec ids (e.g. `auth/login`) round-trip.
pub static TRAILING_COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ludwig-spec:\s+(?P<id>[\w\-/]+)@(?P<version>\d+)\s+hash=(?P<hash>[A-Fa-f0-9]+)")
        .unwrap()
});

pub fn parse_trailing(content: &str) -> Option<TrailingStamp<'_>> {
    let caps = TRAILING_COMMENT_RE.captures(content)?;
    Some(TrailingStamp {
        id: caps.name("id")?.as_str(),
        version: caps.name("version")?.as_str().parse().ok()?,
        hash: caps.name("hash")?.as_str(),
    })
}

#[derive(Debug)]
pub struct TrailingStamp<'a> {
    pub id: &'a str,
    pub version: u32,
    pub hash: &'a str,
}

/// SHA-256 of file body with the trailing `ludwig-spec:` line removed.
pub fn body_sha(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let stripped = strip_trailing_comment(text);
    let mut hasher = Sha256::new();
    hasher.update(stripped.as_bytes());
    Some(hex(&hasher.finalize()))
}

pub fn strip_trailing_comment(content: &str) -> String {
    // Drop any line that matches the `ludwig-spec:` stamp regardless of position.
    // The scaffold currently emits the stamp on the first line, while hand-written
    // files often place it last; both should hash the same body.
    let kept: Vec<&str> =
        content.lines().filter(|line| !TRAILING_COMMENT_RE.is_match(line)).collect();
    let mut out = kept.join("\n");
    if content.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

// -- drift report ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDriftStatus {
    /// Stamp matches and the file body is unchanged since the last verify.
    Ok,
    /// `implements:` names a file that doesn't exist on disk.
    Missing,
    /// File exists but has no trailing `ludwig-spec:` comment.
    Unstamped,
    /// Stamp references a different spec or an older hash.
    StaleStamp,
    /// Stamp is current but the file body changed since the last verify.
    BodyChanged,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDrift {
    pub path: String,
    pub status: FileDriftStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriftReport {
    pub id: String,
    pub spec_version: u32,
    pub spec_hash: String,
    pub stored_hash: Option<String>,
    pub canonical_mode: String,
    pub files: Vec<FileDrift>,
}

pub fn report(project: &Project, id_or_path: &str) -> Result<DriftReport, ProjectError> {
    let path = project
        .find_spec_path(id_or_path)
        .ok_or_else(|| ProjectError::new(format!("no spec found with id-or-path {id_or_path:?}")))?;
    let doc = parser::parse_file(&path)
        .map_err(|e| ProjectError::new(format!("{}: {}", path.display(), e.message)))?;
    let state = project.load_state().unwrap_or_default();
    let entry = state.specs.get(doc.id()).cloned();

    let mut files: Vec<FileDrift> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for pat in &doc.frontmatter.implements {
        for p in matched_files(project, pat) {
            if !seen.insert(p.clone()) {
                continue;
            }
            files.push(file_status(project, &p, &doc, entry.as_ref()));
        }
    }

    Ok(DriftReport {
        id: doc.id().to_string(),
        spec_version: doc.version(),
        spec_hash: doc.canonical_hash(),
        stored_hash: doc.stored_hash().map(|s| s.to_string()),
        canonical_mode: project.canonical_mode().to_string(),
        files,
    })
}

fn matched_files(project: &Project, pattern: &str) -> Vec<PathBuf> {
    if plan::contains_glob(pattern) {
        plan::glob_expand(&project.root, pattern)
    } else {
        // Even when the path doesn't exist we still surface it so the caller can
        // emit a `Missing` drift entry rather than silently dropping the glob.
        vec![project.root.join(pattern)]
    }
}

fn file_status(project: &Project, path: &Path, doc: &Document, entry: Option<&SpecState>) -> FileDrift {
    let rel = path
        .strip_prefix(&project.root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    if !path.is_file() {
        return FileDrift {
            path: rel,
            status: FileDriftStatus::Missing,
            detail: Some("declared in implements: but not on disk".to_string()),
        };
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return FileDrift {
                path: rel,
                status: FileDriftStatus::Missing,
                detail: Some(format!("read failed: {e}")),
            };
        }
    };
    let stamp = match parse_trailing(&content) {
        Some(s) => s,
        None => {
            return FileDrift {
                path: rel,
                status: FileDriftStatus::Unstamped,
                detail: Some("no ludwig-spec: trailing comment; regenerate or stamp".to_string()),
            };
        }
    };
    if stamp.id != doc.id() {
        return FileDrift {
            path: rel,
            status: FileDriftStatus::StaleStamp,
            detail: Some(format!(
                "stamped for {}@{}, expected {}",
                stamp.id,
                stamp.version,
                doc.id()
            )),
        };
    }
    let current_hash = doc.canonical_hash();
    if stamp.hash != current_hash {
        return FileDrift {
            path: rel,
            status: FileDriftStatus::StaleStamp,
            detail: Some(format!(
                "spec changed since this file was generated ({} → {})",
                short(stamp.hash),
                short(&current_hash),
            )),
        };
    }
    // Body-change check requires a stored fingerprint.
    let body = match body_sha_from_str(&content) {
        Some(s) => s,
        None => return FileDrift { path: rel, status: FileDriftStatus::Ok, detail: None },
    };
    if let Some(prev) = entry.and_then(|e| e.implementing_files.get(&rel))
        && *prev != body
    {
        return FileDrift {
            path: rel,
            status: FileDriftStatus::BodyChanged,
            detail: Some("file body edited since last verify".to_string()),
        };
    }
    FileDrift { path: rel, status: FileDriftStatus::Ok, detail: None }
}

fn body_sha_from_str(content: &str) -> Option<String> {
    let stripped = strip_trailing_comment(content);
    let mut hasher = Sha256::new();
    hasher.update(stripped.as_bytes());
    Some(hex(&hasher.finalize()))
}

/// Update state.json with the verified spec hash and file fingerprints.
pub fn record(project: &Project, doc: &Document, files: &[PathBuf]) -> Result<(), ProjectError> {
    let mut state = project.load_state()?;
    let mut implementing_files: BTreeMap<String, String> = BTreeMap::new();
    for path in files {
        if let Some(sha) = body_sha(path) {
            let rel = path
                .strip_prefix(&project.root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            implementing_files.insert(rel, sha);
        }
    }
    state.specs.insert(
        doc.id().to_string(),
        SpecState {
            version: doc.version(),
            hash: doc.canonical_hash(),
            implementing_files,
        },
    );
    project.write_state(&state)
}

pub fn render_text(report: &DriftReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} (v{}, hash={})\n",
        report.id,
        report.spec_version,
        short(&report.spec_hash)
    ));
    if report.files.is_empty() {
        out.push_str("  no implementing files declared\n");
        return out;
    }
    for f in &report.files {
        let mark = match f.status {
            FileDriftStatus::Ok => "ok  ",
            FileDriftStatus::Missing => "miss",
            FileDriftStatus::Unstamped => "no# ",
            FileDriftStatus::StaleStamp => "old ",
            FileDriftStatus::BodyChanged => "edit",
        };
        out.push_str(&format!("  {mark} {}", f.path));
        if let Some(d) = &f.detail {
            out.push_str(&format!(" — {d}"));
        }
        out.push('\n');
    }
    out
}

fn short(h: &str) -> &str {
    if h.len() >= 7 { &h[..7] } else { h }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
