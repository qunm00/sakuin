/// SQLite-backed persistence for the index.
pub struct IndexStore;

impl IndexStore {
    pub fn open(_path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        // TODO: Phase 2 — create schema, implement CRUD
        Ok(Self)
    }
}
