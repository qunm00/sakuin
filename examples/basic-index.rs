/// Basic usage example for Sakuin.
///
/// Run with: `cargo run --example basic-index`
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open or create an index database
    let _store = sakuin::store::IndexStore::open(Path::new("sakuin.db"))?;

    // 2. Scan a directory for Markdown files
    let scanner = sakuin::scanner::Scanner::new(Path::new("."));
    let files = scanner.scan();
    println!("Found {} Markdown files", files.len());

    // 3. Parse each file
    for path in &files {
        println!("  - {}", path.display());
    }

    // 4. Full re-index
    let indexer = sakuin::indexer::Indexer::new();
    indexer.scan_full()?;

    println!("Indexing complete.");
    Ok(())
}
