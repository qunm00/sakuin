//! Top-level orchestrator tying together scanning, parsing, storage, and
//! watching.
//!
//! [`Indexer::scan_full`] performs a full re-index of the workspace;
//! [`Indexer::listen`] starts a filesystem watcher whose events are applied
//! incrementally by [`Indexer::poll`]. A read-only [`Query`] handle is
//! available via [`Indexer::query`].

use crate::parser::{extract_tags_from_frontmatter, Parser};
use crate::query::Query;
use crate::scanner::Scanner;
use crate::store::{FileInfo, IndexStore};
use crate::watcher::{is_markdown, FileWatcher, WatchEvent};
use std::path::{Path, PathBuf};

/// Orchestrates the full indexing pipeline over a workspace.
///
/// Combine a full scan with live watching:
///
/// 1. [`Indexer::open`] to open (or create) the index for a workspace;
/// 2. [`Indexer::scan_full`] for a full re-index;
/// 3. [`Indexer::listen`] + [`Indexer::poll`] to apply filesystem
///    changes incrementally;
/// 4. [`Indexer::query`] for a read-only query handle.
pub struct Indexer {
    root: PathBuf,
    store: IndexStore,
    watcher: Option<FileWatcher>,
}

impl Indexer {
    /// Open (or create) the index database at `db_path` for the workspace
    /// rooted at `root`.
    ///
    /// The root is canonicalised so that scanning and watching agree on a
    /// single base path. Fails if `root` does not exist.
    pub fn open(root: &Path, db_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let root = root.canonicalize()?;
        let store = IndexStore::open(db_path)?;
        Ok(Self {
            root,
            store,
            watcher: None,
        })
    }

    /// Full re-index of the workspace.
    ///
    /// Scans the root, indexes every Markdown file, and removes index
    /// entries for files that no longer exist on disk.
    pub fn scan_full(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let files = Scanner::new(&self.root).scan();

        for relative_path in &files {
            if let Err(error) = self.index_file(relative_path) {
                log::warn!(
                    "Indexer: failed to index {}: {error}",
                    relative_path.display()
                );
            }
        }

        // Drop entries for files that are no longer present in the workspace.
        let indexed_files = self.store.get_all_files()?;
        for file in indexed_files {
            let relative_path = Path::new(&file.relative_path);
            if !files.iter().any(|path| path == relative_path) {
                self.store.delete_file(&file.relative_path)?;
            }
        }
        Ok(())
    }

    /// Start the filesystem watcher on the workspace root. Idempotent.
    pub fn listen(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.watcher.is_none() {
            let mut watcher = FileWatcher::new(&self.root);
            watcher.watch()?;
            self.watcher = Some(watcher);
        }
        Ok(())
    }

    /// Apply any pending filesystem events to the index.
    ///
    /// Call this periodically (e.g. from an event loop) after
    /// [`Indexer::listen`]. If the watcher has stopped, this falls back to a
    /// full re-scan so the index does not silently go stale.
    pub fn poll(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Drain pending events and detect a dead watcher in one pass.
        let (events, watcher_stopped) = match &self.watcher {
            Some(watcher) => match watcher.poll() {
                Ok(events) => (events, false),
                Err(_) => (Vec::new(), true),
            },
            None => (Vec::new(), false),
        };

        for event in events {
            self.apply_event(event);
        }

        if watcher_stopped {
            log::error!(
                "Indexer: file watcher is no longer running; falling back to full re-scan"
            );
            self.watcher = None;
            self.scan_full()?;
        }
        Ok(())
    }

    /// Read-only query handle over the index.
    pub fn query(&self) -> Query<'_> {
        self.store.query()
    }

    // ── Incremental updates ──────────────────────────

    /// Apply a single watch event to the index.
    ///
    /// - `Changed` re-parses the file, or walks a directory subtree.
    /// - `Removed` deletes the file (or every file beneath a directory).
    /// - `Renamed` deletes the old path and indexes the new one.
    fn apply_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Changed(relative_path) => {
                self.index_path(&relative_path);
            }
            WatchEvent::Removed(relative_path) => {
                self.remove_from_index(&relative_path);
            }
            WatchEvent::Renamed(old_path, new_path) => {
                self.remove_from_index(&old_path);
                self.index_path(&new_path);
            }
        }
    }

    /// Index the file at `relative_path`, or every Markdown file beneath it
    /// when the path is a directory. Missing paths are ignored.
    fn index_path(&mut self, relative_path: &Path) {
        let absolute_path = self.root.join(relative_path);
        if !absolute_path.exists() {
            return;
        }
        if absolute_path.is_dir() {
            // A directory change (e.g. a directory renamed into the
            // workspace) means every file beneath it needs re-indexing.
            self.index_subtree(relative_path);
        } else if is_markdown(relative_path) {
            self.index_file(relative_path).unwrap_or_else(|error| {
                log::error!(
                    "Indexer: failed to re-index {}: {error}",
                    relative_path.display()
                );
            });
        }
    }

    /// Parse `relative_path` and store (or refresh) its index rows.
    fn index_file(&mut self, relative_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let absolute_path = self.root.join(relative_path);
        let content = std::fs::read(&absolute_path)?;
        let source = String::from_utf8_lossy(&content);
        let result = Parser::new().parse(&source);
        let tags = extract_tags_from_frontmatter(result.frontmatter.as_deref());
        let relative_path_string = relative_path.to_string_lossy().into_owned();
        let title = match result.headings.first() {
            Some(heading) => heading.text.clone(),
            None => relative_path_string.clone(),
        };
        let file_info = FileInfo::from_content(
            relative_path_string,
            absolute_path.to_string_lossy().into_owned(),
            &content,
            result.frontmatter.clone(),
        );
        self.store.index_file(
            &file_info,
            &result.headings,
            &result.links,
            &tags,
            &title,
            &result.body,
        )?;
        Ok(())
    }

    /// Index every Markdown file at or below `relative_path`.
    ///
    /// Walks only the affected subtree (instead of the whole workspace),
    /// reusing the scanner's walk so the same ignore rules (`.gitignore`,
    /// hidden files) and ordering apply as in a full scan.
    fn index_subtree(&mut self, relative_path: &Path) {
        let files = Scanner::new(&self.root).scan_under(relative_path);
        for relative_path in &files {
            if let Err(error) = self.index_file(relative_path) {
                log::error!(
                    "Indexer: failed to index {}: {error}",
                    relative_path.display()
                );
            }
        }
    }

    /// Remove `relative_path` from the index, cascading to every indexed
    /// file beneath it (a removed directory).
    fn remove_from_index(&mut self, relative_path: &Path) {
        let prefix = format!("{}/", relative_path.to_string_lossy());
        let files = match self.store.get_all_files() {
            Ok(files) => files,
            Err(error) => {
                log::error!("Indexer: failed to list files for removal: {error}");
                Vec::new()
            }
        };
        for file in files {
            // Skip entries outside the removed subtree.
            if !file.relative_path.starts_with(&prefix) {
                continue;
            }
            if let Err(error) = self.store.delete_file(&file.relative_path) {
                log::error!(
                    "Indexer: failed to remove {}: {error}",
                    file.relative_path
                );
            }
        }
        if let Err(error) = self.store.delete_file(&relative_path.to_string_lossy()) {
            log::error!(
                "Indexer: failed to remove {} from index: {error}",
                relative_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temporary workspace populated with the given files.
    fn create_workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
        }
        dir
    }

    fn db_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("test.db")
    }

    fn indexed_paths(indexer: &Indexer) -> Vec<String> {
        indexer
            .query()
            .files(None)
            .unwrap()
            .into_iter()
            .map(|file| file.relative_path)
            .collect()
    }

    // ── scan_full ─────────────────────────────────────

    #[test]
    fn scan_full_indexes_every_markdown_file() {
        let dir = create_workspace(&[
            ("index.md", "# Home\n\nWelcome to [[docs]]."),
            ("docs.md", "---\ntags: [documentation]\n---\n\n# Docs\n\nSee [[index]]."),
            ("notes/rust.md", "# Rust\n\nThe quick brown fox."),
            ("readme.txt", "not markdown"),
        ]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        assert_eq!(
            indexed_paths(&indexer),
            vec![
                "docs.md".to_string(),
                "index.md".to_string(),
                "notes/rust.md".to_string(),
            ]
        );
    }

    #[test]
    fn scan_full_removes_stale_entries() {
        let dir = create_workspace(&[("a.md", "# A"), ("b.md", "# B")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();
        assert_eq!(indexed_paths(&indexer).len(), 2);

        // Remove a file from disk and re-scan: its entry must disappear.
        std::fs::remove_file(dir.path().join("a.md")).unwrap();
        indexer.scan_full().unwrap();
        assert_eq!(indexed_paths(&indexer), vec!["b.md".to_string()]);
    }

    // ── Incremental events (simulated filesystem changes) ──

    #[test]
    fn changed_event_indexes_new_file() {
        let dir = create_workspace(&[("a.md", "# A")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        std::fs::write(dir.path().join("b.md"), "# B\n").unwrap();
        indexer.apply_event(WatchEvent::Changed(PathBuf::from("b.md")));

        assert_eq!(
            indexed_paths(&indexer),
            vec!["a.md".to_string(), "b.md".to_string()]
        );
    }

    #[test]
    fn changed_event_reindexes_modified_file() {
        let dir = create_workspace(&[("a.md", "# Old title")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        std::fs::write(dir.path().join("a.md"), "# New title\n").unwrap();
        indexer.apply_event(WatchEvent::Changed(PathBuf::from("a.md")));

        let headings = indexer.query().headings(None).unwrap();
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].text, "New title");
    }

    #[test]
    fn changed_event_for_missing_file_is_a_noop() {
        let dir = create_workspace(&[("a.md", "# A")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        // The file was created and removed again within the debounce window.
        indexer.apply_event(WatchEvent::Changed(PathBuf::from("ghost.md")));
        assert_eq!(indexed_paths(&indexer), vec!["a.md".to_string()]);
    }

    #[test]
    fn changed_event_for_non_markdown_file_is_a_noop() {
        let dir = create_workspace(&[("a.md", "# A")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        std::fs::write(dir.path().join("notes.txt"), "hi").unwrap();
        indexer.apply_event(WatchEvent::Changed(PathBuf::from("notes.txt")));
        assert_eq!(indexed_paths(&indexer), vec!["a.md".to_string()]);
    }

    #[test]
    fn removed_event_deletes_file() {
        let dir = create_workspace(&[("a.md", "# A"), ("b.md", "# B")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        indexer.apply_event(WatchEvent::Removed(PathBuf::from("a.md")));
        assert_eq!(indexed_paths(&indexer), vec!["b.md".to_string()]);
    }

    #[test]
    fn removed_directory_cascades_to_children() {
        let dir = create_workspace(&[
            ("a.md", "# A"),
            ("notes/one.md", "# One"),
            ("notes/two.md", "# Two"),
        ]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        std::fs::remove_dir_all(dir.path().join("notes")).unwrap();
        indexer.apply_event(WatchEvent::Removed(PathBuf::from("notes")));

        assert_eq!(indexed_paths(&indexer), vec!["a.md".to_string()]);
    }

    #[test]
    fn renamed_event_moves_file_in_index() {
        let dir = create_workspace(&[("old.md", "# Old")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        std::fs::rename(
            dir.path().join("old.md"),
            dir.path().join("new.md"),
        )
        .unwrap();
        indexer.apply_event(WatchEvent::Renamed(
            PathBuf::from("old.md"),
            PathBuf::from("new.md"),
        ));

        assert_eq!(indexed_paths(&indexer), vec!["new.md".to_string()]);
    }

    #[test]
    fn changed_directory_indexes_subtree() {
        let dir = create_workspace(&[("top.md", "# Top")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();

        // Simulate a directory renamed into the workspace: the watcher emits
        // a single Changed event for the directory.
        std::fs::create_dir(dir.path().join("incoming")).unwrap();
        std::fs::write(dir.path().join("incoming/a.md"), "# Incoming A").unwrap();
        std::fs::write(dir.path().join("incoming/b.md"), "# Incoming B").unwrap();
        indexer.apply_event(WatchEvent::Changed(PathBuf::from("incoming")));

        assert_eq!(
            indexed_paths(&indexer),
            vec![
                "incoming/a.md".to_string(),
                "incoming/b.md".to_string(),
                "top.md".to_string(),
            ]
        );
    }

    // ── listen + poll (real filesystem events) ────────

    #[test]
    fn listen_and_poll_apply_real_changes() {
        let dir = create_workspace(&[("a.md", "# A")]);
        let mut indexer = Indexer::open(dir.path(), &db_path(&dir)).unwrap();
        indexer.scan_full().unwrap();
        indexer.listen().unwrap();

        std::fs::write(dir.path().join("b.md"), "# B\n").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            indexer.poll().unwrap();
            if indexed_paths(&indexer).len() == 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the watcher to pick up b.md"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        std::fs::remove_file(dir.path().join("b.md")).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            indexer.poll().unwrap();
            if indexed_paths(&indexer).len() == 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the watcher to pick up the removal"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
