use std::path::{Path, PathBuf};

/// Recursively walks a directory tree and yields relative paths of `.md` files.
pub struct Scanner {
    root: PathBuf,
}

impl Scanner {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_owned() }
    }

    /// Walk the directory and return a list of relative paths to `.md` files.
    pub fn scan(&self) -> Vec<PathBuf> {
        // TODO: Phase 1 — implement recursive walk with .gitignore support
        Vec::new()
    }
}
