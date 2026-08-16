//! Workspace scanning: discover Markdown files under a directory.

use crate::helpers::sort_relative_paths;
use std::path::{Path, PathBuf};
/// Recursively walks a directory tree and yields relative paths of `.md` files.
///
/// Uses the `ignore` crate to respect `.gitignore` patterns automatically.
/// Files are returned in alphabetical order for deterministic output.
pub struct Scanner {
    root: PathBuf,
}

impl Scanner {
    /// Create a new `Scanner` rooted at `root`.
    ///
    /// The root path is canonicalised so that relative paths are consistently
    /// computed from the same base.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }

    /// Walk the directory and return a list of relative paths to `.md` files.
    ///
    /// Hidden files and directories (names starting with `.`) are skipped.
    /// The returned list is sorted alphabetically for determinism.
    pub fn scan(&self) -> Vec<PathBuf> {
        self.walk(&self.root)
    }

    /// Walk the subtree at `relative_path` and return workspace-relative
    /// paths to `.md` files beneath it.
    ///
    /// Uses the same ignore rules (`.gitignore`, hidden files) and ordering
    /// as [`Scanner::scan`], so incremental indexing matches a full scan.
    pub fn scan_under(&self, relative_path: &Path) -> Vec<PathBuf> {
        self.walk(&self.root.join(relative_path))
    }

    /// Shared walk: collect `.md` file paths under `walk_root`, made
    /// relative to the scanner's root and sorted deterministically.
    fn walk(&self, walk_root: &Path) -> Vec<PathBuf> {
        let mut results = Vec::new();

        let walker = ignore::WalkBuilder::new(walk_root)
            .standard_filters(true) // respect .gitignore
            .hidden(true) // skip hidden files/dirs (names starting with `.`)
            .build();

        for entry in walker {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                        && path.is_file()
                        && let Ok(relative) = path.strip_prefix(&self.root)
                    {
                        results.push(relative.to_owned());
                    }
                }
                Err(err) => {
                    log::warn!("Scanner: error walking entry: {err}");
                }
            }
        }

        // Stable ordering for determinism.
        sort_relative_paths(&mut results);

        results
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn create_temp_workspace(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut f = fs::File::create(&path).unwrap();
            writeln!(f, "# {}", file).unwrap();
        }
        dir
    }

    #[test]
    fn scan_finds_markdown_files() {
        let dir = create_temp_workspace(&["hello.md", "sub/dir/test.md", "other.txt", "readme.md"]);
        let scanner = Scanner::new(dir.path());
        let mut files = scanner.scan();
        files.sort();

        assert_eq!(files.len(), 3);
        assert!(files.contains(&PathBuf::from("hello.md")));
        assert!(files.contains(&PathBuf::from("sub/dir/test.md")));
        assert!(files.contains(&PathBuf::from("readme.md")));
    }

    #[test]
    fn scan_skips_non_markdown() {
        let dir = create_temp_workspace(&["index.md", "style.css", "main.rs", "data.json"]);
        let scanner = Scanner::new(dir.path());
        let files = scanner.scan();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0], PathBuf::from("index.md"));
    }

    #[test]
    fn scan_skips_hidden_files() {
        let dir = create_temp_workspace(&["visible.md", ".hidden.md", ".dotdir/secret.md"]);
        let scanner = Scanner::new(dir.path());
        let files = scanner.scan();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0], PathBuf::from("visible.md"));
    }

    #[test]
    fn scan_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let scanner = Scanner::new(dir.path());
        let files = scanner.scan();

        assert!(files.is_empty());
    }

    #[test]
    fn scan_returns_sorted_paths() {
        let dir = create_temp_workspace(&["zeta.md", "alpha.md", "beta.md", "gamma/deep.md"]);
        let scanner = Scanner::new(dir.path());
        let files = scanner.scan();

        assert_eq!(files.len(), 4);
        // Alphabetical order
        assert_eq!(files[0], PathBuf::from("alpha.md"));
        assert_eq!(files[1], PathBuf::from("beta.md"));
        assert_eq!(files[2], PathBuf::from("gamma/deep.md"));
        assert_eq!(files[3], PathBuf::from("zeta.md"));
    }
}
