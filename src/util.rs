use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::plan;
use crate::project::Project;

/// A kebab-case slug: lowercase letters/digits separated by dashes, with `/`
/// permitted so sub-game ids (`auth/login`) round-trip. The single source of
/// truth shared by `scaffold::validate_slug` (which validates user-supplied
/// slugs) and the parser (which now validates a spec's frontmatter `id`).
static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9\-/]*[a-z0-9]$").unwrap());

/// Return `true` if `s` is a well-formed kebab-case slug. A spec's `id` flows
/// into filesystem paths (the generated `tests/ludwig_<id>.rs`, the cache file
/// `.ludwig/cache/<id>@<v>.md`) and the judgment-key namespace, so validating
/// its shape at the parse boundary keeps a hand-authored or older-binary spec
/// from smuggling path separators, `..`, or other surprises into those paths.
pub fn is_valid_slug(s: &str) -> bool {
    SLUG_RE.is_match(s)
}

/// Return `true` if `pat` would resolve outside the project root: an absolute
/// path, a drive/UNC prefix, or any path containing a `..` (parent-dir)
/// component. Used to confine spec-controlled `implements:` patterns to the
/// project tree. The single source of truth shared by the parser (which rejects
/// escaping patterns at validation time), `matched_files` (which refuses to
/// expand them), and `plan` (which refuses to fingerprint them).
pub fn pattern_escapes_root(pat: &str) -> bool {
    Path::new(pat).components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

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
    // `implements:` patterns are spec-controlled and a spec can arrive from an
    // untrusted MCP client. Refuse any pattern that escapes the project tree so
    // verify/drift can never read (or probe the existence of) files outside the
    // project. The parser already rejects such patterns at validation time; this
    // is defense-in-depth for any spec that slipped through (e.g. persisted by an
    // older binary). See spec `mcp-path-confinement`.
    if pattern_escapes_root(pattern) {
        return Vec::new();
    }
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

/// Write `contents` to `path` atomically: serialize to a sibling temp file, then
/// rename it over the destination. A rename within the same directory is atomic
/// on POSIX and replaces the target on Windows, so a crash or full disk mid-write
/// can never leave a truncated file — a reader sees the old complete file or the
/// new one. Mirrors `Project::write_state` so user-authored files get the same
/// durability guarantee as Ludwig's own bookkeeping.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("ludwig");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    fs::write(&tmp, contents)?;
    if let Err(e) = fs::rename(&tmp, path) {
        // Best-effort cleanup so a failed rename doesn't strand the temp file.
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Write `contents` to `path`, choosing the right strategy for whether a clobber
/// is allowed:
/// - `overwrite == false`: create the file with `create_new`, which fails with
///   [`io::ErrorKind::AlreadyExists`] if it already exists. This closes the
///   TOCTOU window an `is_file()` pre-check leaves open — a file created by a
///   concurrent process between the check and the write can no longer be
///   silently clobbered.
/// - `overwrite == true`: replace the target atomically via [`atomic_write`].
pub fn write_guarded(path: &Path, contents: &[u8], overwrite: bool) -> io::Result<()> {
    if overwrite {
        atomic_write(path, contents)
    } else {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        f.write_all(contents)
    }
}
