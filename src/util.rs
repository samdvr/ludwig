use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::plan;
use crate::project::Project;

/// A kebab-case slug: one or more `/`-separated segments, each a run of
/// lowercase letters/digits with internal dashes (no leading/trailing dash, no
/// empty segment). `/` lets sub-game ids (`auth/login`) round-trip. A single
/// character (`a`, `1`) is valid; `a//b`, `-a`, `a/`, and `a/-b` are not. The
/// single source of truth shared by `scaffold::validate_slug` (user-supplied
/// slugs) and the parser (a spec's frontmatter `id`).
static SLUG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?(/[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$").unwrap()
});

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

/// Return `true` if `path` resolves outside `root` once symlinks are followed.
/// [`pattern_escapes_root`] only inspects a pattern's textual components; this is
/// the complementary *runtime* check for a concrete, existing path that may be
/// (or may live under) a symlink pointing out of the project tree. Without it, a
/// spec's `implements:` could name an in-tree path like `src/innocent.rs` that is
/// actually a symlink to `/etc/passwd`, and the file read that follows would leak
/// out-of-tree content. Both `root` and `path` are canonicalized so the
/// comparison is link- and `..`-free. A path that cannot be canonicalized is
/// treated as escaping; callers only invoke this for paths they have already
/// confirmed exist, so this is a conservative default.
pub fn resolved_path_escapes_root(root: &Path, path: &Path) -> bool {
    let Ok(real_path) = path.canonicalize() else {
        return true;
    };
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    !real_path.starts_with(&real_root)
}

/// Render `path` as a project-root-relative string using forward slashes on
/// every platform. Root-relative paths are part of the JSON contract surfaced
/// over MCP (`spec.list`/`read`/`diff`/`plan`/`verify`, etc.) and persisted in
/// reports and `state.json`; without this, Windows would emit backslashes and
/// diverge from the cross-platform contract that clients — and the project's
/// own `state.json` fingerprints — assume. Falls back to the full path when it
/// is not under `root`.
pub fn rel_str(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let s = rel.to_string_lossy().into_owned();
    // Only Windows uses a non-`/` separator; on Unix a literal backslash is a
    // legal filename character and must be left untouched.
    #[cfg(windows)]
    let s = s.replace('\\', "/");
    s
}

/// Hex-encode a byte slice — used wherever Ludwig prints or persists a SHA-256
/// digest (spec hash, file fingerprint, body sha). Keeps one definition so the
/// digest format can't drift across modules.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `write!` into the preallocated buffer — no per-byte String allocation.
        let _ = write!(s, "{b:02x}");
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
        plan::glob_expand(project, pattern)
    } else {
        let p = project.root.join(pattern);
        if p.is_file() {
            // The path exists. Guard against an in-tree path that is itself a
            // symlink (or sits under a symlinked directory) resolving outside the
            // project: `is_file()` and the subsequent read both follow links,
            // which would let a spec read out-of-tree content. `pattern_escapes_root`
            // only inspects the textual pattern, so this is the orthogonal check.
            if resolved_path_escapes_root(&project.root, &p) {
                Vec::new()
            } else {
                vec![p]
            }
        } else if include_missing {
            // Genuinely absent: hand the projected path back so drift can report
            // it as `Missing`. Nothing is read, so there is no escape to guard.
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
