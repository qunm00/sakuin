/// Basic usage example for Sakuin.
///
/// Run with: `cargo run --example basic-index`
use std::path::Path;
use std::time::Duration;

use sakuin_md::{SearchField, SearchOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open (or create) an index database for the current workspace.
    let mut indexer = sakuin_md::indexer::Indexer::open(Path::new("."), Path::new("sakuin.db"))?;

    // 2. Full re-index: scan the workspace and index every Markdown file.
    indexer.scan_full()?;
    println!("Indexing complete.");

    // 3. Search. `try_search` escapes the query, so whatever is typed is
    //    matched as literal text rather than as FTS5 syntax.
    let query = indexer.query();
    for hit in query.try_search("concurrency")? {
        println!("{}: {}", hit.relative_path, hit.snippet);
    }

    // 4. Configurable search: paging, prefix matching, field restriction,
    //    case sensitivity, and snippet length.
    let path_fields = [SearchField::Path];

    // The first 20 hits, paged.
    let first_page = query.search_with_options("rust", &SearchOptions::new().with_limit(20))?;
    println!("page 1: {} hits", first_page.len());

    // The next 20.
    let second_page =
        query.search_with_options("rust", &SearchOptions::new().with_offset(20).with_limit(20))?;
    println!("page 2: {} hits", second_page.len());

    // Type-ahead: `docu*` also matches `document`. Note that the index stores
    // stems, so the prefix must match the stem, not the whole word.
    let prefix_hits = query.search_with_options(
        "docu",
        &SearchOptions::new().with_prefix(true).with_limit(5),
    )?;
    for hit in prefix_hits {
        println!("prefix hit: {}", hit.relative_path);
    }

    // Search only in file paths.
    let path_hits =
        query.search_with_options("guide", &SearchOptions::new().with_fields(&path_fields))?;
    for hit in path_hits {
        println!("path hit: {}", hit.relative_path);
    }

    // 5. Keep the index live by watching the workspace for changes.
    indexer.listen()?;

    // 6. Apply filesystem changes as they arrive.
    loop {
        indexer.poll()?;
        std::thread::sleep(Duration::from_millis(100));
    }
}
