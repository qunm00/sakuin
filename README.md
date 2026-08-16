# Sakuin

> **Sakuin** (索引) is Japanese for "index".

Sakuin is a Rust library for indexing a workspace of Markdown files into a
single SQLite database. It scans a directory, parses every file into
structured data — frontmatter, headings, links (including wikilinks), tags,
and full-text body — and keeps the index live as the filesystem changes.

It is designed for applications that work with Markdown repositories: wikis,
note-taking apps, documentation sites, static-site generators, and knowledge
bases. Instead of re-implementing scanning, parsing, and indexing, use Sakuin.

## Features

- **Scan** — Recursively walks a workspace, respecting `.gitignore` and
  skipping hidden files, and discovers every `.md` file.
- **Parse** — Extracts YAML frontmatter, tags, ATX and Setext headings,
  links (inline, reference, autolink, image), native wikilinks
  (`[[target]]` and `[[target|label]]`), and plain-text body content.
- **Store** — Everything lives in a single SQLite database with a
  deterministic schema: `files`, `headings`, `links`, `tags`, and an FTS5
  full-text index.
- **Live sync** — Watches the workspace for changes and applies incremental
  updates, so the index never needs a full re-scan on every edit.
- **Query** — Typed, read-only queries: files, headings, tags, links,
  backlinks, and ranked full-text search with snippets.
- **Deterministic** — The same workspace always produces the same index.
  Ordering, casing, and anchor generation are consistent.

## Quick start

Add Sakuin to your `Cargo.toml`:

```toml
[dependencies]
sakuin-md = "0.1"
```

Open an index for a workspace and do a full scan:

```rust
use sakuin_md::indexer::Indexer;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut indexer = Indexer::open(Path::new("./notes"), Path::new("sakuin.db"))?;
    indexer.scan_full()?;
    Ok(())
}
```

Keep the index live while the workspace changes:

```rust
use sakuin_md::indexer::Indexer;
use std::path::Path;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut indexer = Indexer::open(Path::new("./notes"), Path::new("sakuin.db"))?;
    indexer.scan_full()?;
    indexer.listen()?;

    loop {
        indexer.poll()?;
        std::thread::sleep(Duration::from_millis(100));
    }
}
```

There is also a runnable example:

```console
$ cargo run --example basic-index
```

## Querying the index

`Query` gives you read-only, typed access to the index:

```rust
use sakuin_md::indexer::Indexer;
use sakuin_md::LinkFilter;
use sakuin_md::parser::LinkType;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let indexer = Indexer::open(Path::new("./notes"), Path::new("sakuin.db"))?;

    // All files, or files matching a glob pattern.
    let all_files = indexer.query().files(None)?;
    let docs_only = indexer.query().files(Some("docs/*.md"))?;

    // Files tagged with a specific tag.
    let rust_notes = indexer.query().files_by_tag("rust")?;

    // Table of contents for one file.
    let file_id = indexer.query().files(None)?.first().map(|f| f.id);
    if let Some(id) = file_id {
        let toc = indexer.query().toc(id)?;
        println!("headings: {toc:?}");
    }

    // Backlinks: which files reference "docs"?
    let backlinks = indexer.query().backlinks("docs")?;

    // Full-text search, ranked by BM25 with snippets.
    let results = indexer.query().search("concurrency")?;
    for hit in results {
        println!("{}: {}", hit.relative_path, hit.snippet);
    }

    // Links, filtered by origin file and/or link type.
    let wikilinks = indexer.query().links(
        LinkFilter::new().with_link_type(LinkType::Wikilink),
    )?;
    Ok(())
}
```

## How it works

The library is split into six modules, all reachable from the crate root:

| Module | Responsibility |
|---|---|
| `scanner::Scanner` | Walks a directory tree and lists `.md` files, respecting `.gitignore`. |
| `parser::Parser` | Turns raw file content into `ParseResult` (frontmatter, tags, headings, links, body). |
| `store::IndexStore` | SQLite persistence: upsert/delete files, replace child rows, run migrations. |
| `watcher::FileWatcher` | Background filesystem watcher emitting `WatchEvent`s with relative paths. |
| `query::Query` | Read-only typed queries over the index. |
| `indexer::Indexer` | Orchestrator tying scan + watch + store + query together. |

The index lives in a single SQLite file:

- `files` — one row per file (relative path, absolute path, content hash,
  frontmatter, size, timestamps).
- `headings` — level, text, GitHub-style anchor, byte position.
- `links` — link type (`inline`, `reference`, `wikilink`, `autolink`,
  `image`), target, text, byte position.
- `tags` — tags parsed from frontmatter.
- `fts` — FTS5 virtual table over `title` and `body` for full-text search.

Schema migrations are embedded and applied automatically when the store is
opened, so the database is always upgraded to the current schema.

## Determinism

Given the same workspace, Sakuin produces the same index every time:

- Files are processed in a stable, OS-independent order.
- Content hashes are SHA-256.
- Anchor slugs follow a fixed algorithm (lowercase, strip punctuation,
  collapse whitespace to `-`, deduplicate with `-1`, `-2`, …).
- The schema is created with explicit, deterministic DDL.

The only value allowed to vary between runs is the `indexed_at` timestamp.

## Documentation

- [Architecture & design](docs/design.md)
- [Implementation plan](docs/plan.md)
- API docs: `cargo doc --open`