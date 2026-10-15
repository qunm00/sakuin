//! End-to-end tests for the configurable search API.
//!
//! These build a temp workspace and index it through the **public**
//! [`Indexer`] API only — no store internals — which is the layer that catches
//! a migration which works in unit tests but leaves `fts` unpopulated for
//! files indexed through the watcher path.

use sakuin_md::indexer::Indexer;
use sakuin_md::watcher::WatchEvent;
use sakuin_md::{SearchError, SearchField, SearchOptions};
use std::path::Path;
use std::time::{Duration, Instant};

/// Build a workspace and index it with a full scan.
fn scan_workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, Indexer) {
    let dir = tempfile::tempdir().unwrap();
    for (relative_path, content) in files {
        let path = dir.path().join(relative_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }
    let mut indexer = Indexer::open(dir.path(), &dir.path().join("index.db")).unwrap();
    indexer.scan_full().unwrap();
    (dir, indexer)
}

/// A workspace exercising titles, bodies, and paths.
fn sample_workspace() -> (tempfile::TempDir, Indexer) {
    scan_workspace(&[
        (
            "notes/rust.md",
            "---\ntitle: Rust Notes\n---\n\n# Rust Notes\n\nThe quick brown fox jumps over the lazy dog.",
        ),
        (
            "guide/indexing.md",
            "---\ntitle: Indexing Guide\n---\n\n# Indexing Guide\n\nHow the scanner walks a workspace.",
        ),
        (
            "misc/other.md",
            "---\ntitle: Other\n---\n\n# Other\n\nNothing relevant here.",
        ),
    ])
}

#[test]
fn try_search_finds_indexed_content() {
    let (_dir, indexer) = sample_workspace();
    let query = indexer.query();

    let results = query.try_search("fox").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].relative_path, "notes/rust.md");
    assert!(results[0].snippet.contains("fox"));
}

#[test]
fn field_restriction_works_end_to_end() {
    let (_dir, indexer) = sample_workspace();
    let query = indexer.query();

    // `rust` is in the path of one file and in titles/bodies too.
    let fields = [SearchField::Path];
    let paths = query
        .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
        .unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].relative_path, "notes/rust.md");

    let fields = [SearchField::Title];
    let titles = query
        .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
        .unwrap();
    assert_eq!(titles.len(), 1);

    // `fox` appears only in a body.
    let bodies = {
        let fields = [SearchField::Body];
        query
            .search_with_options("fox", &SearchOptions::new().with_fields(&fields))
            .unwrap()
    };
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].relative_path, "notes/rust.md");
}

#[test]
fn paging_works_end_to_end() {
    let (_dir, indexer) = scan_workspace(&[
        ("a.md", "# A\n\nA shared zebra appears here."),
        ("b.md", "# B\n\nAnother shared zebra appears here."),
        ("c.md", "# C\n\nYet another shared zebra here."),
        ("d.md", "# D\n\nA final shared zebra here."),
    ]);
    let query = indexer.query();

    let all = query.try_search("zebra").unwrap();
    assert_eq!(all.len(), 4);

    let mut paged = Vec::new();
    for offset in [0, 2, 4] {
        let page = query
            .search_with_options(
                "zebra",
                &SearchOptions::new().with_offset(offset).with_limit(2),
            )
            .unwrap();
        paged.extend(page);
    }
    let names = |results: &[sakuin_md::SearchResult]| -> Vec<String> {
        results.iter().map(|r| r.relative_path.clone()).collect()
    };
    assert_eq!(names(&paged), names(&all));
}

#[test]
fn prefix_and_case_sensitivity_work_end_to_end() {
    let (_dir, indexer) = sample_workspace();
    let query = indexer.query();

    // `gui*` matches the stem of `guide`/`indexing`'s document text.
    let prefixed = query
        .search_with_options("gui", &SearchOptions::new().with_prefix(true))
        .unwrap();
    assert!(!prefixed.is_empty());

    // Case folding is the default; the GLOB re-check narrows it.
    assert_eq!(query.try_search("RUST").unwrap().len(), 1);
    assert!(
        query
            .search_with_options("RUST", &SearchOptions::new().with_case_sensitive(true))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn snippet_length_is_configurable_end_to_end() {
    let (_dir, indexer) = sample_workspace();
    let query = indexer.query();

    let with_snippet = query.try_search("fox").unwrap();
    assert!(!with_snippet[0].snippet.is_empty());

    let without = query
        .search_with_options("fox", &SearchOptions::new().with_snippet_tokens(0))
        .unwrap();
    assert!(without[0].snippet.is_empty());
}

#[test]
fn blank_and_hostile_queries_never_error_end_to_end() {
    let (_dir, indexer) = sample_workspace();
    let query = indexer.query();

    assert!(query.try_search("").unwrap().is_empty());
    assert!(query.try_search("   \t ").unwrap().is_empty());

    for input in [
        "\"unterminated",
        "NEAR/2",
        "a AND",
        "(a",
        "*",
        "title:rust",
        "^rust",
        "rust -notes",
        "\u{0}rust",
        "🦀 rust",
    ] {
        match query.try_search(input) {
            Ok(_) => {}
            Err(SearchError::InvalidQuery(message)) => {
                panic!("{input:?} should be escaped, not rejected: {message}")
            }
            Err(other) => panic!("{input:?} produced an unexpected error: {other}"),
        }
    }
}

#[test]
fn files_indexed_through_the_watcher_get_a_searchable_path() {
    // Regression guard for migration 002: `set_fts` populates `path` from
    // `files`, so a file that only enters the index through a watcher event
    // must be findable by its path, not just by its body text.
    let (dir, mut indexer) = sample_workspace();

    // Written under the temp workspace, not the process working directory.
    let absolute_new_path = dir.path().join("added/watched.md");
    std::fs::create_dir_all(absolute_new_path.parent().unwrap()).unwrap();
    std::fs::write(
        &absolute_new_path,
        "# Watched\n\nA brand new note about penguins.",
    )
    .unwrap();
    indexer.apply_event(WatchEvent::Changed(
        Path::new("added/watched.md").to_path_buf(),
    ));

    let query = indexer.query();
    let fields = [SearchField::Path];
    let results = query
        .search_with_options("watched", &SearchOptions::new().with_fields(&fields))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].relative_path, "added/watched.md");

    assert_eq!(query.try_search("penguins").unwrap().len(), 1);
}

#[test]
fn removed_files_disappear_from_search_end_to_end() {
    let (_dir, mut indexer) = sample_workspace();
    assert_eq!(indexer.query().try_search("fox").unwrap().len(), 1);

    let removed = Path::new("notes/rust.md");
    indexer.apply_event(WatchEvent::Removed(removed.to_path_buf()));

    let query = indexer.query();
    assert!(query.try_search("fox").unwrap().is_empty());
    let fields = [SearchField::Path];
    assert!(
        query
            .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_full_rescan_reaches_the_same_state_as_incremental_updates() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("notes")).unwrap();
    std::fs::write(
        dir.path().join("notes/one.md"),
        "# One\n\nA zebra wanders by.",
    )
    .unwrap();

    let mut indexer = Indexer::open(dir.path(), &dir.path().join("index.db")).unwrap();
    indexer.scan_full().unwrap();

    // Add a second file through the watcher.
    std::fs::write(
        dir.path().join("notes/two.md"),
        "# Two\n\nAnother zebra appears.",
    )
    .unwrap();
    indexer.apply_event(WatchEvent::Changed(Path::new("notes/two.md").to_path_buf()));

    let incremental = indexer.query().try_search("zebra").unwrap();
    assert_eq!(incremental.len(), 2);

    // A rescan must converge to the same result set.
    indexer.scan_full().unwrap();
    let rescanned = indexer.query().try_search("zebra").unwrap();
    let names = |results: &[sakuin_md::SearchResult]| -> Vec<String> {
        results.iter().map(|r| r.relative_path.clone()).collect()
    };
    assert_eq!(names(&rescanned), names(&incremental));
}

#[test]
fn reopening_the_database_preserves_the_migrated_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("notes")).unwrap();
    std::fs::write(
        dir.path().join("notes/rust.md"),
        "---\ntitle: Rust Notes\n---\n\n# Rust Notes\n\nThe fox.",
    )
    .unwrap();
    let db_path = dir.path().join("index.db");

    {
        let mut indexer = Indexer::open(dir.path(), &db_path).unwrap();
        indexer.scan_full().unwrap();
        assert_eq!(indexer.query().try_search("fox").unwrap().len(), 1);
    }

    // Re-opening runs migrations again; they must be no-ops, and the data
    // (including the `path` column) must survive.
    let indexer = Indexer::open(dir.path(), &db_path).unwrap();
    let query = indexer.query();
    let fields = [SearchField::Path];
    let by_path = query
        .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
        .unwrap();
    assert_eq!(by_path.len(), 1);
    assert_eq!(by_path[0].relative_path, "notes/rust.md");
}

#[test]
fn a_watched_workspace_stays_consistent_for_a_short_while() {
    // A light smoke test of the live path: files added while watching become
    // searchable, and the whole thing stays error-free.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("start.md"), "# Start\n\nA zebra begins.").unwrap();

    let mut indexer = Indexer::open(dir.path(), &dir.path().join("index.db")).unwrap();
    indexer.scan_full().unwrap();
    indexer.listen().unwrap();

    let added = dir.path().join("added.md");
    std::fs::write(&added, "# Added\n\nA second zebra arrives.").unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        indexer.poll().unwrap();
        if indexer.query().try_search("arrives").unwrap().len() == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not pick up the file"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
