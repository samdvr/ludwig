use std::path::PathBuf;

use crate::plan;
use crate::project::Project;

/// Hex-encode a byte slice — used wherever Ludwig prints or persists a SHA-256
/// digest (spec hash, file fingerprint, body sha). Keeps one definition so the
/// digest format can't drift across modules.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Expand `pattern` (a path or a hand-rolled glob from `implements:`) against
/// the project root.
///
/// `include_missing` controls the behavior when the pattern names a single
/// concrete file that doesn't exist on disk:
/// - `false` (used by verify / plan): return an empty list so the file is
///   simply absent from downstream processing.
/// - `true` (used by drift): return the projected path anyway so the caller
///   can surface it as a `Missing` drift entry rather than dropping it.
pub fn matched_files(project: &Project, pattern: &str, include_missing: bool) -> Vec<PathBuf> {
    if plan::contains_glob(pattern) {
        plan::glob_expand(&project.root, pattern)
    } else {
        let p = project.root.join(pattern);
        if p.is_file() || include_missing {
            vec![p]
        } else {
            Vec::new()
        }
    }
}
