/// Filesystem watcher for live index synchronisation.
pub struct FileWatcher;

impl FileWatcher {
    pub fn new() -> Self {
        Self
    }

    /// Start watching the given directory.
    pub fn watch(&self, _root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: Phase 4 — implement notify-based watcher
        Ok(())
    }
}
