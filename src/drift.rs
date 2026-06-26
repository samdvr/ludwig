use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::ProjectError;
use crate::parser;
use crate::project::{Project, SpecState};
use crate::spec::Document;

/// `ludwig-spec: <id>@<version> hash=<sha>` — captures id, version, hash. The id
/// pattern allows `/` so that sub-game spec ids (e.g. `auth/login`) round-trip.
/// Anchored to the start of a line (after optional whitespace and a single
/// comment marker) so a line that merely *mentions* the stamp text inside a
/// string or code is not mistaken for a stamp.
pub static TRAILING_COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^[ \t]*(?:#|//|/\*|\*|<!--|--|;|%)?[ \t]*ludwig-spec:\s+(?P<id>[\w\-/]+)@(?P<version>\d+)\s+hash=(?P<hash>[A-Fa-f0-9]+)",
    )
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
    Some(crate::util::hex(&hasher.finalize()))
}

pub fn strip_trailing_comment(content: &str) -> String {
    // Drop any line that matches the `ludwig-spec:` stamp regardless of position.
    // The scaffold currently emits the stamp on the first line, while hand-written
    // files often place it last; both should hash the same body.
    //
    // Line endings are deliberately normalized to LF here (via `lines()`): a pure
    // CRLF<->LF flip (e.g. a checkout under git `core.autocrlf`) is not a semantic
    // change to the implementation and must not register as `BodyChanged`. Both
    // the stored and the current fingerprint pass through this function, so the
    // comparison stays self-consistent.
    let kept: Vec<&str> =
        content.lines().filter(|line| !TRAILING_COMMENT_RE.is_match(line)).collect();
    let mut out = kept.join("\n");
    if content.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Replace every `// ludwig-spec: ...` line in `content` with a fresh stamp built
/// from `doc`. If no stamp line is present, returns the content unchanged — callers
/// that need to insert a stamp into an unstamped file should detect that case
/// separately and surface it as drift.
pub fn update_stamp_in_place(content: &str, doc: &Document) -> String {
    let new_stamp = format!(
        "// ludwig-spec: {}@{} hash={}",
        doc.id(),
        doc.version(),
        doc.canonical_hash()
    );
    let lines: Vec<String> = content
        .lines()
        .map(|line| {
            if TRAILING_COMMENT_RE.is_match(line) {
                new_stamp.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    let mut out = lines.join("\n");
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
    report_for_path(project, &path)
}

/// Build the drift report from an already-resolved spec path. The MCP layer
/// calls this with the path returned by its confinement check so a single
/// `spec.diff` request does not re-scan and re-parse every spec twice.
pub fn report_for_path(project: &Project, path: &Path) -> Result<DriftReport, ProjectError> {
    let doc = parser::parse_file(path)
        .map_err(|e| ProjectError::new(format!("{}: {}", path.display(), e.message)))?;
    let state = project.load_state().unwrap_or_default();
    let entry = state.specs.get(doc.id()).cloned();

    let mut files: Vec<FileDrift> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for pat in &doc.frontmatter.implements {
        for p in crate::util::matched_files(project, pat, true) {
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
        canonical_mode: project.canonical_mode().as_str().to_string(),
        files,
    })
}

fn file_status(project: &Project, path: &Path, doc: &Document, entry: Option<&SpecState>) -> FileDrift {
    let rel = crate::util::rel_str(&project.root, path);
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
        // Distinguish "spec body changed within the same version" from "spec
        // version was bumped" — the latter usually means the user already
        // acknowledged a breaking change and the regenerate step is overdue,
        // while the former is unexpected and worth a different warning.
        //
        // The *remedy* direction depends on the canonical mode: in `spec` mode
        // the code is stale and is regenerated from the spec; in `code` mode the
        // code leads, so a moved spec hash means the spec is the thing to
        // reconcile and re-verify (which re-stamps the file).
        let code_mode = project.canonical_mode().is_code();
        let detail = if stamp.version != doc.version() {
            if code_mode {
                format!(
                    "spec was bumped v{} → v{} ({} → {}); code is canonical here — reconcile the spec with the code, then re-verify to re-stamp",
                    stamp.version,
                    doc.version(),
                    short(stamp.hash),
                    short(&current_hash),
                )
            } else {
                format!(
                    "spec was bumped v{} → v{} since this file was generated ({} → {}); regenerate to update",
                    stamp.version,
                    doc.version(),
                    short(stamp.hash),
                    short(&current_hash),
                )
            }
        } else if code_mode {
            format!(
                "spec body changed within v{} ({} → {}); code is canonical here — reconcile the spec with the code, then re-verify to re-stamp",
                stamp.version,
                short(stamp.hash),
                short(&current_hash),
            )
        } else {
            format!(
                "spec body changed within v{} ({} → {}); bump version: in frontmatter or regenerate",
                stamp.version,
                short(stamp.hash),
                short(&current_hash),
            )
        };
        return FileDrift {
            path: rel,
            status: FileDriftStatus::StaleStamp,
            detail: Some(detail),
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
        // In `code` mode the edited code is the source of truth, so the spec is
        // what's now behind; in `spec` mode the edit is a drift away from the
        // canonical spec.
        let detail = if project.canonical_mode().is_code() {
            "code changed since last verify; the spec is now behind — update the spec to match the code, then re-verify"
        } else {
            "file body edited since last verify"
        };
        return FileDrift {
            path: rel,
            status: FileDriftStatus::BodyChanged,
            detail: Some(detail.to_string()),
        };
    }
    FileDrift { path: rel, status: FileDriftStatus::Ok, detail: None }
}

fn body_sha_from_str(content: &str) -> Option<String> {
    let stripped = strip_trailing_comment(content);
    let mut hasher = Sha256::new();
    hasher.update(stripped.as_bytes());
    Some(crate::util::hex(&hasher.finalize()))
}

/// Update state.json with the verified spec hash and file fingerprints. Also
/// snapshot the spec's canonical body into `.ludwig/cache/<id>@<version>.md`
/// so future runs can show a meaningful diff between historical versions.
pub fn record(project: &Project, doc: &Document, files: &[PathBuf]) -> Result<(), ProjectError> {
    let mut implementing_files: BTreeMap<String, String> = BTreeMap::new();
    for path in files {
        if let Some(sha) = body_sha(path) {
            let rel = crate::util::rel_str(&project.root, path);
            implementing_files.insert(rel, sha);
        }
    }
    // Lock-guarded read-modify-write so a concurrent verify/ingest can't clobber
    // this spec's recorded state (or have its own clobbered). See `mutate_state`.
    project.mutate_state(|state| {
        state.specs.insert(
            doc.id().to_string(),
            SpecState {
                version: doc.version(),
                hash: doc.canonical_hash(),
                implementing_files: std::mem::take(&mut implementing_files),
            },
        );
        Ok(())
    })?;
    cache_canonical_body(project, doc)?;
    Ok(())
}

/// Path to the cached canonical body for a given spec id + version, regardless
/// of whether that file currently exists on disk. Sub-game ids that contain
/// slashes (`auth/login`) round-trip safely by URL-encoding the slash.
pub fn cache_path(project: &Project, id: &str, version: u32) -> PathBuf {
    let safe_id = id.replace('/', "_");
    project.cache_dir().join(format!("{safe_id}@{version}.md"))
}

fn cache_canonical_body(project: &Project, doc: &Document) -> Result<(), ProjectError> {
    let cache_dir = project.cache_dir();
    fs::create_dir_all(&cache_dir)
        .map_err(|e| ProjectError::new(format!("mkdir {}: {e}", cache_dir.display())))?;
    let target = cache_path(project, doc.id(), doc.version());
    // Only write the first time we see this (id, version) pair. If the body
    // changed within the same version (which the version-mismatch heuristic in
    // file_status already flags), the original cache stays put — overwriting
    // would lose the historical snapshot we want to preserve.
    if target.is_file() {
        return Ok(());
    }
    fs::write(&target, doc.canonical_body.as_bytes())
        .map_err(|e| ProjectError::new(format!("write {}: {e}", target.display())))
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
    short_hash(h)
}

/// Return the first 7 chars of a hash for human-readable display, or the full
/// string if it is shorter. Uses `get(..7)` rather than `&h[..7]` so non-ASCII
/// input (should never happen for our hex hashes, but cheap insurance) doesn't
/// panic.
pub fn short_hash(h: &str) -> &str {
    h.get(..7).unwrap_or(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line that merely mentions the stamp text inside a string/code must not
    /// be mistaken for a real stamp.
    #[test]
    fn stamp_regex_ignores_midline_literal() {
        let content = "let s = \"ludwig-spec: fake@1 hash=deadbeef\";\n";
        assert!(parse_trailing(content).is_none());
        // Stripping the (non-)stamp leaves the line untouched.
        assert_eq!(strip_trailing_comment(content), content);
    }

    /// A real stamp on its own line (with or without a leading comment marker)
    /// is still detected, and a decoy literal alongside it is ignored.
    #[test]
    fn stamp_regex_matches_real_stamp_line() {
        let content = "pub fn f() {}\n// ludwig-spec: my-spec@3 hash=abc123\n";
        let stamp = parse_trailing(content).expect("real stamp detected");
        assert_eq!(stamp.id, "my-spec");
        assert_eq!(stamp.version, 3);
        assert_eq!(stamp.hash, "abc123");

        let mixed = "let s = \"ludwig-spec: fake@9 hash=dead\";\n// ludwig-spec: real@1 hash=beef\n";
        assert_eq!(parse_trailing(mixed).expect("stamp").id, "real");
    }
}
