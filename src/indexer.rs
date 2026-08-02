/// Top-level orchestrator tying together scanning, parsing, storage, and watching.
pub struct Indexer;

impl Default for Indexer {
    fn default() -> Self {
        Self::new()
    }
}

impl Indexer {
    pub fn new() -> Self {
        Self
    }

    /// Perform a full re-index of the workspace.
    pub fn scan_full(&self) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: Phase 1–3 — full scan pipeline
        Ok(())
    }
}
