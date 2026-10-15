//! Benchmarks: full re-index vs. incremental single-file update.
//!
//! Run with `cargo bench`. Both benches operate on the same synthetic
//! workspace so the timings are directly comparable: re-indexing the whole
//! workspace should cost roughly N times a single incremental update.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use sakuin_md::SearchOptions;
use sakuin_md::indexer::Indexer;
use sakuin_md::watcher::WatchEvent;
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// Number of Markdown files in the synthetic workspace.
const FILE_COUNT: usize = 200;

/// Populate `dir` with `count` Markdown files under `notes/`.
fn build_workspace(dir: &Path, count: usize) {
    for index in 0..count {
        let next = (index + 1) % count;
        let content = format!(
            "---\ntags: [bench, note_{index}]\n---\n\n# Note {index}\n\n\
             Body of note {index} with a [[link-{next}]] and a [site](https://example.com).\n\n\
             ## Section {index}\n\nCode: `fn main()`.\n"
        );
        let path = dir.join(format!("notes/note-{index:03}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

/// Remove the SQLite database and its WAL sidecar files so the next `open`
/// starts from scratch.
fn remove_index_files(db: &Path) {
    for path in [
        db.to_path_buf(),
        db.with_extension("db-wal"),
        db.with_extension("db-shm"),
    ] {
        let _ = std::fs::remove_file(path);
    }
}

fn bench_full_reindex(c: &mut Criterion) {
    let workspace = tempfile::tempdir().unwrap();
    build_workspace(workspace.path(), FILE_COUNT);
    let db = workspace.path().join("full.db");

    let mut group = c.benchmark_group("indexing");
    group.bench_function("full re-index", |b| {
        b.iter_batched(
            || {
                remove_index_files(&db);
                Indexer::open(workspace.path(), &db).unwrap()
            },
            |mut indexer| {
                indexer.scan_full().unwrap();
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_incremental_update(c: &mut Criterion) {
    let workspace = tempfile::tempdir().unwrap();
    build_workspace(workspace.path(), FILE_COUNT);
    let db = workspace.path().join("incremental.db");
    let mut indexer = Indexer::open(workspace.path(), &db).unwrap();
    indexer.scan_full().unwrap();

    let target = workspace.path().join("notes/note-000.md");

    let mut group = c.benchmark_group("indexing");
    group.bench_function("incremental update (one file)", |b| {
        b.iter(|| {
            std::fs::write(&target, "# Note touched\n\nUpdated content.\n").unwrap();
            indexer.apply_event(WatchEvent::Changed(PathBuf::from("notes/note-000.md")));
            black_box(&indexer);
        })
    });
    group.finish();
}

/// Search cost on the same synthetic workspace: a plain exact-token query
/// against a prefix query.
///
/// A prefix query has to walk the term index instead of looking terms up, so
/// it is expected to cost more than the exact query; the point of the bench is
/// to make that visible if it changes.
fn bench_search(c: &mut Criterion) {
    let workspace = tempfile::tempdir().unwrap();
    build_workspace(workspace.path(), FILE_COUNT);
    let db = workspace.path().join("search.db");
    let mut indexer = Indexer::open(workspace.path(), &db).unwrap();
    indexer.scan_full().unwrap();
    let query = indexer.query();

    let mut group = c.benchmark_group("search");
    group.bench_function("exact query", |b| {
        b.iter(|| black_box(query.try_search("scanner").unwrap()))
    });
    group.bench_function("search_prefix", |b| {
        b.iter(|| {
            black_box(
                query
                    .search_with_options("scan", &SearchOptions::new().with_prefix(true))
                    .unwrap(),
            )
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_full_reindex,
    bench_incremental_update,
    bench_search
);
criterion_main!(benches);
