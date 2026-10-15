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
  backlinks, and ranked full-text search with field restriction, paging,
  prefix matching, case sensitivity, and configurable snippets.
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
    let results = indexer.query().try_search("concurrency")?;
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

## Search

`Query::try_search` is the normal entry point. It escapes the query, so
whatever the user types is matched as **literal text** — `AND`, `OR`, `NEAR`,
`*`, and quotes have no special meaning, and no input can produce a query
error.

`Query::search_with_options` takes a `SearchOptions` for everything else:

```rust
use sakuin_md::{SearchField, SearchOptions};

// The first 20 hits, and then the next 20.
let page_one = query.search_with_options("rust", &SearchOptions::new().with_limit(20))?;
let page_two = query
    .search_with_options("rust", &SearchOptions::new().with_offset(20).with_limit(20))?;

// Type-ahead: `docu` also matches `document`.
let typeahead = query
    .search_with_options("docu", &SearchOptions::new().with_prefix(true))?;

// Search only file paths.
let paths_only = [SearchField::Path];
let by_path = query
    .search_with_options("guide", &SearchOptions::new().with_fields(&paths_only))?;

// Case-sensitive search, for code identifiers.
let exact_case = query
    .search_with_options("Indexer", &SearchOptions::new().with_case_sensitive(true))?;

// Longer or shorter snippets; 0 suppresses them entirely.
let long_snippets = query
    .search_with_options("fox", &SearchOptions::new().with_snippet_tokens(40))?;
```

| Option | Default | Behaviour |
|---|---|---|
| `limit` | `None` | Maximum number of results; `None` means all. |
| `offset` | `0` | Results to skip. Paging is deterministic. |
| `fields` | all | Columns to search: `Path`, `Title`, `Body`. Empty means all. |
| `prefix` | `false` | Match the last token as a prefix (`docu` → `document`). |
| `case_sensitive` | `false` | Keep only rows containing the query with that exact case. |
| `snippet_tokens` | `16` | Approximate snippet length in tokens; `0` gives `""`. |

Results are ordered by BM25 rank (lower is better) and then by
`relative_path`. Columns left out of `fields` get a zero BM25 weight, so they
cannot influence the score.

Two caveats worth knowing:

- **Prefix queries match stems, not words.** The index is tokenized with
  `porter`, so `index*` matches `indexing` but `indexi*` does not.
- **Case-sensitive search can only narrow.** Candidates come from FTS, which
  folds case; the case-sensitive `GLOB` re-check then removes rows. It cannot
  bring in a file that FTS found no token for.

### Migrating from `Query::search`

`Query::search` is deprecated. It still works and keeps its old behaviour —
the query string reaches FTS5 **verbatim**, so operators are interpreted and
malformed input is a backend error. `Query::try_search` is the drop-in
replacement:

| Old | New |
|---|---|
| `query.search("x")?` | `query.try_search("x")?` |
| error type `rusqlite::Result` | `Result<_, SearchError>` |

`SearchError` separates `InvalidQuery` (the query was blank) from `Backend`
(everything else), so callers no longer match on message text.

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
- `fts` — FTS5 virtual table over `path`, `title`, and `body` for full-text
  search.

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