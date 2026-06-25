use std::path::Path;

/// Thin wrapper around `tempfile::TempDir` that preserves the project's
/// existing call sites (`TempDir::new("prefix")`, `.path()`). Existed before as
/// a hand-rolled helper; switched to the `tempfile` crate for correct cleanup
/// on Windows and a tested `Drop` implementation.
pub struct TempDir(tempfile::TempDir);

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create temp dir");
        TempDir(inner)
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}
