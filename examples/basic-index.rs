/// Basic usage example for Sakuin.
///
/// Run with: `cargo run --example basic-index`
use std::path::Path;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open (or create) an index database for the current workspace.
    let mut indexer = sakuin_md::indexer::Indexer::open(Path::new("."), Path::new("sakuin.db"))?;

    // 2. Full re-index: scan the workspace and index every Markdown file.
    indexer.scan_full()?;
    println!("Indexing complete.");

    // 3. Keep the index live by watching the workspace for changes.
    indexer.listen()?;

    // 4. Apply filesystem changes as they arrive.
    loop {
        indexer.poll()?;
        std::thread::sleep(Duration::from_millis(100));
    }
}
