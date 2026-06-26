use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::ProjectError;
use crate::game::Game;
use crate::parser;
use crate::project::Project;
use crate::spec::{Classifier, GherkinKeyword, Status};

#[derive(Debug, Serialize)]
pub struct Brief {
    pub spec: SpecBrief,
    pub game: Game,
    pub depends_on: Vec<DependencyEntry>,
    pub implementing_files: Vec<FileFingerprint>,
    pub regenerating: RegenHint,
}

#[derive(Debug, Serialize)]
pub struct SpecBrief {
    pub id: String,
    pub title: String,
    pub version: u32,
    pub status: Status,
    pub canonical_hash: String,
    pub path: String,
    pub intent: String,
    pub behaviors: Vec<BriefBehavior>,
    pub examples: Vec<BriefExample>,
    pub invariants: Vec<BriefInvariant>,
    pub non_goals: String,
    pub implementation_notes: String,
}

#[derive(Debug, Serialize)]
pub struct BriefBehavior {
    pub tag: Option<String>,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct BriefExample {
    pub name: String,
    pub steps: Vec<BriefStep>,
}

#[derive(Debug, Serialize)]
pub struct BriefStep {
    pub keyword: GherkinKeyword,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct BriefInvariant {
    pub classifier: Classifier,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct DependencyEntry {
    pub id: String,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileFingerprint {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RegenHint {
    Fresh {
        fresh: bool,
    },
    Stale {
        fresh: bool,
        previous_version: u32,
        current_version: u32,
    },
}

pub fn brief_for(project: &Project, id: &str) -> Result<Brief, ProjectError> {
    let path = project
        .find_spec_path(id)
        .ok_or_else(|| ProjectError::new(format!("no spec found with id {id:?}")))?;
    brief_for_path(project, &path)
}

/// Build the generation brief from an already-resolved spec path. The MCP layer
/// calls this with the path returned by its confinement check so a single
/// `spec.plan` request does not re-scan and re-parse every spec twice.
pub fn brief_for_path(project: &Project, path: &std::path::Path) -> Result<Brief, ProjectError> {
    let doc = parser::parse_file(path)
        .map_err(|e| ProjectError::new(format!("{}: {}", path.display(), e.message)))?;
    let game = Game::for_spec(project, path);
    let deps = resolve_dependencies(project, &doc.frontmatter.depends_on);

    let rel_path = path
        .strip_prefix(&project.root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();

    let spec = SpecBrief {
        id: doc.id().to_string(),
        title: doc.frontmatter.title.clone(),
        version: doc.version(),
        status: doc.frontmatter.status,
        canonical_hash: doc.canonical_hash(),
        path: rel_path,
        intent: doc.intent.clone(),
        behaviors: doc
            .behaviors
            .iter()
            .map(|b| BriefBehavior {
                tag: b.tag.clone(),
                text: b.text.clone(),
            })
            .collect(),
        examples: doc
            .examples
            .iter()
            .map(|e| BriefExample {
                name: e.name.clone(),
                steps: e
                    .steps
                    .iter()
                    .map(|s| BriefStep {
                        keyword: s.keyword,
                        text: s.text.clone(),
                    })
                    .collect(),
            })
            .collect(),
        invariants: doc
            .invariants
            .iter()
            .map(|i| BriefInvariant {
                classifier: i.classifier,
                text: i.text.clone(),
            })
            .collect(),
        non_goals: doc.non_goals.clone(),
        implementation_notes: doc.implementation_notes.clone(),
    };

    let implementing_files = existing_implementing_files(project, &doc.frontmatter.implements);
    let regenerating = regenerating_hint(project, doc.id(), doc.version());

    Ok(Brief {
        spec,
        game,
        depends_on: deps,
        implementing_files,
        regenerating,
    })
}

fn resolve_dependencies(project: &Project, start: &[String]) -> Vec<DependencyEntry> {
    let mut seen: BTreeMap<String, DependencyEntry> = BTreeMap::new();
    let mut queue: Vec<String> = start.to_vec();
    while let Some(dep_id) = queue.pop() {
        if seen.contains_key(&dep_id) {
            continue;
        }
        let path = project.find_spec_path(&dep_id);
        match path.and_then(|p| parser::parse_file(&p).ok().map(|d| (p, d))) {
            None => {
                seen.insert(
                    dep_id.clone(),
                    DependencyEntry {
                        id: dep_id,
                        found: false,
                        title: None,
                        version: None,
                        intent: None,
                        path: None,
                    },
                );
            }
            Some((p, dep_doc)) => {
                let rel = p
                    .strip_prefix(&project.root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                seen.insert(
                    dep_id.clone(),
                    DependencyEntry {
                        id: dep_id,
                        found: true,
                        title: Some(dep_doc.frontmatter.title.clone()),
                        version: Some(dep_doc.version()),
                        intent: Some(dep_doc.intent.clone()),
                        path: Some(rel),
                    },
                );
                queue.extend(dep_doc.frontmatter.depends_on.iter().cloned());
            }
        }
    }
    seen.into_values().collect()
}

fn existing_implementing_files(project: &Project, globs: &[String]) -> Vec<FileFingerprint> {
    let mut out: Vec<FileFingerprint> = Vec::new();
    for pat in globs {
        // `implements:` patterns are spec-controlled. Refuse any that are
        // absolute or contain a `..` component so a malicious or careless spec
        // can't fingerprint (and thereby leak the size/sha of) files outside the
        // project tree. The glob branch already walks only under `root`, but the
        // exact-path branch would otherwise read whatever `root.join(pat)`
        // resolves to.
        if crate::util::pattern_escapes_root(pat) {
            continue;
        }
        let full = project.root.join(pat);
        let pat_str = full.to_string_lossy().into_owned();
        // Use a tiny glob substitute: only match exact paths (no globbing characters)
        // *or* shell-style trailing-* expansion by walking the directory. For v0.1 the
        // existing tests use exact paths and `src/foo.*` style — implement the
        // exact-path case first, defer the wildcard case to drift/verify.
        if !contains_glob(&pat_str) {
            if let Ok(meta) = std::fs::metadata(&full)
                && meta.is_file()
                && let Ok(bytes) = fs::read(&full)
            {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let digest = hasher.finalize();
                out.push(FileFingerprint {
                    path: full
                        .strip_prefix(&project.root)
                        .unwrap_or(&full)
                        .to_string_lossy()
                        .into_owned(),
                    size: meta.len(),
                    sha256: crate::util::hex(&digest),
                });
            }
        } else {
            for matched in glob_expand(&project.root, pat) {
                if let Ok(meta) = std::fs::metadata(&matched)
                    && meta.is_file()
                    && let Ok(bytes) = fs::read(&matched)
                {
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    let digest = hasher.finalize();
                    out.push(FileFingerprint {
                        path: matched
                            .strip_prefix(&project.root)
                            .unwrap_or(&matched)
                            .to_string_lossy()
                            .into_owned(),
                        size: meta.len(),
                        sha256: crate::util::hex(&digest),
                    });
                }
            }
        }
    }
    out
}

fn regenerating_hint(project: &Project, id: &str, current_version: u32) -> RegenHint {
    let state = project.load_state().unwrap_or_default();
    match state.specs.get(id) {
        None => RegenHint::Fresh { fresh: true },
        Some(entry) if entry.version == current_version => RegenHint::Fresh { fresh: true },
        Some(entry) => RegenHint::Stale {
            fresh: false,
            previous_version: entry.version,
            current_version,
        },
    }
}

pub(crate) fn contains_glob(pat: &str) -> bool {
    pat.contains('*') || pat.contains('?')
}

/// Directories `glob_expand` never descends into: build output, VCS internals,
/// and Ludwig's own state dir. Matching by leaf name keeps it simple and covers
/// the common cases; a spec's `implements:` is never expected to point inside one.
fn is_pruned_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | ".ludwig" | "node_modules")
    )
}

/// Hand-rolled glob: only supports `*` (any chars except `/`), `**` (any chars),
/// and `?` (single char except `/`). Adequate for the patterns the spec system
/// uses (`src/foo/*.rs`, `src/foo.*`). Bracket character classes are NOT
/// supported — `[` and `]` are escaped so they are matched literally.
pub(crate) fn glob_expand(root: &std::path::Path, pattern: &str) -> Vec<PathBuf> {
    let regex_str = glob_to_regex(pattern);
    let re = match regex::Regex::new(&regex_str) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        // Prune build/VCS/state dirs so a `**` pattern doesn't descend into
        // (and fingerprint) generated artifacts, git internals, or Ludwig's own
        // bookkeeping — none of which a spec's `implements:` should match. This
        // also keeps the walk cheap on large trees.
        .filter_entry(|e| !is_pruned_dir(e))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        if re.is_match(&rel) {
            out.push(entry.into_path());
        }
    }
    out.sort();
    out
}

fn glob_to_regex(pat: &str) -> String {
    let mut out = String::from("^");
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                out.push_str(".*");
                i += 2;
                continue;
            }
            out.push_str("[^/]*");
        } else if c == '?' {
            out.push_str("[^/]");
        } else if matches!(
            c,
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' | '{' | '}' | '[' | ']'
        ) {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
        i += 1;
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use crate::util::pattern_escapes_root;

    // spec-controlled `implements:` patterns must not be able to
    // reach outside the project tree.
    #[test]
    fn rejects_parent_and_absolute_patterns() {
        assert!(pattern_escapes_root("../secrets.txt"));
        assert!(pattern_escapes_root("src/../../etc/passwd"));
        assert!(pattern_escapes_root("/etc/passwd"));
    }

    #[test]
    fn allows_in_tree_patterns() {
        assert!(!pattern_escapes_root("src/lib.rs"));
        assert!(!pattern_escapes_root("src/adapters/*.rs"));
        assert!(!pattern_escapes_root("crate/sub/mod.rs"));
    }
}
