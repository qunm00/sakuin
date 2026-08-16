# Sakuin — Markdown Workspace Indexer

> **Sakuin** (索引) is Japanese for "index". This library provides a reusable,
> deterministic indexing engine for Markdown workspaces, backed by SQLite.

---

## Table of Contents

1. [Vision & Goals](#vision--goals)
2. [Non-Goals](#non-goals)
3. [Architecture Overview](#architecture-overview)
4. [SQLite Schema](#sqlite-schema)
5. [Core Components](#core-components)
6. [Query API](#query-api)
7. [Filesystem Watching & Synchronisation](#filesystem-watching--synchronisation)
8. [Determinism Guarantees](#determinism-guarantees)
9. [Design Decisions & Trade-offs](#design-decisions--trade-offs)

---

## Vision & Goals

**Sakuin** ingests a directory of Markdown files and builds a structured,
queryable index stored in SQLite. Applications that work with Markdown
repositories — wikis, note-taking apps, documentation sites, static-site
generators, knowledge-base tools — can use Sakuin to avoid re-implementing the
same scanning, parsing, and indexing logic.

### Goals

- **Scan & Index** — Recursively walk a directory, parse Markdown files,
  extract metadata (frontmatter), headings, links (wikilinks, Markdown links,
  auto-links), and full-text content.
- **Live Sync** — Watch the filesystem for changes and update the index
  incrementally without a full re-scan.
- **SQLite Backend** — All index data lives in a single SQLite file;
  deterministic schema.
- **Query API** — Simple, typed queries: files, headings, links, full-text
  search.
- **UI-independent** — Pure library with no GUI, no HTTP server, no framework
  coupling.
- **Deterministic** — Given the same workspace snapshot, the same index is
  produced. Ordering, casing, and normalisation are consistent.

---

## Non-Goals

- Rendering, previewing, or converting Markdown.
- Implementing a wiki or note-taking application.
- Providing a CLI or daemon (though one could be built on top).
- Handling non-Markdown file types (images, PDFs, etc.) beyond basic metadata.
- Network-based synchronisation or multi-user concurrency.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                      Application                        │
│  (notebook, wiki, doc-gen, …)                           │
└──────────────────┬──────────────────────────────────────┘
                   │
                   │  sakuin_md::Indexer
                   │  sakuin_md::Query
                   │  sakuin_md::Event
                   ▼
┌──────────────────────────────────────────────────────────┐
│                       Sakuin                             │
│                                                          │
│  ┌───────────┐  ┌──────────┐  ┌───────────────────┐      │
│  │ Scanner   │  │ Parser   │  │ FsWatcher         │      │
│  │ (walk dir)│  │ (markdown│  │ (notify crate)    │      │
│  │           │  │  -> AST) │  │                   │      │
│  └────┬──────┘  └────┬─────┘  └────┬──────────────┘      │
│       │              │              │                    │
│       ▼              ▼              ▼                    │
│  ┌─────────────────────────────────────────────────┐     │
│  │            IndexStore (SQLite)                  │     │
│  │  - upsert_file, delete_file                     │     │
│  │  - insert_heading, insert_link, insert_fts      │     │
│  │  - query methods                                │     │
│  └─────────────────────────────────────────────────┘     │
│                                                          │
│  ┌─────────────────────────────────────────────────┐     │
│  │               Query API                         │     │
│  │  files(), headings(), links(), search(), …      │     │
│  └─────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

### High-level data flow

1. **Scan** — Walk the directory tree, discover `.md` files.
2. **Parse** — Read each file, extract:
   - Frontmatter (YAML/Toml — TBD)
   - Headings (ATX & Setext)
   - Links (inline, reference, wikilinks `[[...]]`, autolinks)
   - Raw text for full-text search.
3. **Store** — Insert/update rows in SQLite.
4. **Watch** — Listen for `notify` events; on change, re-parse the affected
   file and upsert its index data.
5. **Query** — Application calls `Query` methods via a handle.

---

## SQLite Schema

The index is stored in a single SQLite database file. Every table includes a
`schema_version` row for migration checks.

```sql
-- Schema version tracking
CREATE TABLE IF NOT EXISTS _meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO _meta (key, value) VALUES ('schema_version', '1');

-- ── Files ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS files (
    id          INTEGER PRIMARY KEY,
    relative_path    TEXT NOT NULL UNIQUE,           -- relative to workspace root
    absolute_path    TEXT NOT NULL,
    hash        TEXT NOT NULL,                  -- SHA-256 of file content
    frontmatter TEXT,                           -- raw YAML/TOML string or NULL
    size_bytes  INTEGER NOT NULL,
    created_at  TEXT NOT NULL,                  -- ISO-8601 (file mtime or index time)
    indexed_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ── Headings ──────────────────────────────────────────
CREATE TABLE IF NOT EXISTS headings (
    id        INTEGER PRIMARY KEY,
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    level     INTEGER NOT NULL CHECK(level BETWEEN 1 AND 6),
    text      TEXT NOT NULL,
    anchor    TEXT NOT NULL,                    -- GitHub-style slug anchor
    position  INTEGER NOT NULL,                 -- byte offset in source
    UNIQUE(file_id, position)
);

-- ── Links ────────────────────────────────────────────
-- Stores every link found in Markdown files.
CREATE TABLE IF NOT EXISTS links (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    link_type   TEXT NOT NULL CHECK(link_type IN (
                    'inline', 'reference', 'wikilink', 'autolink', 'image'
                )),
    target      TEXT NOT NULL,                  -- the URL or wikilink target
    anchor      TEXT,                           -- optional #fragment
    text        TEXT,                           -- link text / label
    position    INTEGER NOT NULL,               -- byte offset in source
    UNIQUE(file_id, position)
);

-- ── Tags (from frontmatter `tags` array) ─────────────
CREATE TABLE IF NOT EXISTS tags (
    id      INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    tag     TEXT NOT NULL,
    UNIQUE(file_id, tag)
);

-- ── Full-Text Search ──────────────────────────────────
-- Using FTS5 for efficient content search.
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
    file_id UNINDEXED,
    title,
    body,
    tokenize='porter unicode61'
);

-- ── Indexes ───────────────────────────────────────────
CREATE INDEX IF NOT EXISTS idx_headings_file   ON headings(file_id);
CREATE INDEX IF NOT EXISTS idx_links_file      ON links(file_id);
CREATE INDEX IF NOT EXISTS idx_tags_file       ON tags(file_id);
CREATE INDEX IF NOT EXISTS idx_tags_tag        ON tags(tag);
```

> **Design note on `hash`:** Storing a content hash allows us to skip
> re-parsing unchanged files during a full re-scan. On incremental updates
> (filesystem watch), we trust the event and always re-parse.

---

## Core Components

### 1. `Scanner` — Directory traversal

| Aspect | Decision |
|---|---|
| Entry point | `Scanner::new(root: &Path) -> Scanner` |
| Behaviour | Recursively walks `root`, yields relative paths of `.md` files. |
| Ignore patterns | Respects `.gitignore` (opt-in, via `ignore` crate) or a provided glob list. |
| Error handling | Logs inaccessible paths, continues scanning. Returns a list of `(path, result)`. |

### 2. `Parser` — Markdown → structured data

| Aspect | Decision |
|---|---|
| Crate | [`pulldown-cmark`](https://crates.io/crates/pulldown-cmark) for fast, compliant CommonMark parsing. |
| Frontmatter | Optional. If a file starts with `---\n...\n---`, capture raw text; optionally parse with `serde_yaml` / `toml`. |
| Headings | Walk the event stream for `Event::Start(Heading(level, ..))`. Generate anchor slugs (GitHub-compatible: lowercase, strip punctuation, replace spaces with `-`). |
| Links | Collect `Event::Start(Link(..))`, `Event::Start(Image(..))`, and wikilinks via custom parser (see below). |
| Wikilinks | Use `pulldown-cmark` 0.13+'s native wikilink support via `Options::ENABLE_WIKILINKS`. Detects `[[target]]` and `[[target|label]]` directly as `Tag::Link` with `LinkType::WikiLink`. No pre-processing needed. |
| Text extraction | Concatenate all `Event::Text` for FTS body. |

#### Wikilink handling strategy

Pulldown-cmark 0.13+ includes native wikilink support via the
`Options::ENABLE_WIKILINKS` extension flag. When enabled, `[[target]]` and
`[[target|label]]` patterns are parsed directly into `Tag::Link` events with
`LinkType::WikiLink { has_pothole: bool }`. This eliminates the need for any
pre-processing or custom scanning — the parser handles wikilinks in the same
event stream as standard Markdown links.

### 3. `IndexStore` — SQLite persistence

| Aspect | Decision |
|---|---|
| Crate | [`rusqlite`](https://crates.io/crates/rusqlite) with bundled SQLite. |
| Opening | `IndexStore::open(path: &Path) -> Result<Self>`. Creates tables if missing. |
| Writes | Batch-insert: within a single file update, wrap all mutations in a transaction. |
| Deletion | On file delete or rename, cascade-delete via foreign keys. |
| Read path | Direct SQL queries exposed through `Query`. |

#### Methods

```rust
impl IndexStore {
    fn upsert_file(&mut self, file: &FileInfo) -> Result<i64>;
    fn delete_file(&mut self, relative_path: &str) -> Result<()>;
    fn set_headings(&mut self, file_id: i64, headings: &[Heading]) -> Result<()>;
    fn set_links(&mut self, file_id: i64, links: &[Link]) -> Result<()>;
    fn set_tags(&mut self, file_id: i64, tags: &[String]) -> Result<()>;
    fn set_fts(&mut self, file_id: i64, title: &str, body: &str) -> Result<()>;
}
```

Each `set_*` method deletes old rows for the given `file_id` and inserts new
ones inside the same transaction.

### 4. `FileWatcher` — Live synchronisation

| Aspect | Decision |
|---|---|
| Crate | [`notify`](https://crates.io/crates/notify) (debouncer recommended). |
| Strategy | Use `notify::recommended_debouncer` with ~200ms debounce. |
| Events handled | `Create`, `Modify`, `Remove` for `.md` files. |
| Filtering | Ignore non-Markdown files, hidden files/dirs. |
| Error recovery | On watch errors, log and fall back to periodic full-scan (configurable interval). |

The `FileWatcher` runs in a background thread. When an event fires, it sends a
message over a channel to the `Indexer`, which re-parses the affected file and
updates the store.

```rust
pub enum WatchEvent {
    Changed(PathBuf),   // relative path
    Renamed(PathBuf, PathBuf), // old, new
    Removed(PathBuf),
}
```

### 5. `Indexer` — Orchestrator

The top-level struct that ties everything together.

```rust
pub struct Indexer {
    root: PathBuf,
    store: IndexStore,
    watcher: Option<FileWatcher>,
}
```

Methods:

- `Indexer::open(root, db_path)` — Open or create the index.
- `Indexer::scan_full()` — Full re-index of the workspace.
- `Indexer::listen()` — Start filesystem watching.
- `Indexer::poll()` — Check for channel messages from the watcher and apply
  incremental updates.
- `Indexer::apply_event(event)` — Apply a single `WatchEvent`. Used by
  `poll()`; also callable directly when you bring your own event source.
- `Indexer::query()` — Return a `Query` handle.

---

## Query API

The `Query` object provides read-only access to the index. It borrows the
underlying SQLite connection.

```rust
pub struct Query<'conn> {
    conn: &'conn Connection,
}

impl<'conn> Query<'conn> {
    /// All indexed files, optionally filtered by a glob pattern.
    pub fn files(&self, pattern: Option<&str>) -> Result<Vec<FileEntry>>;

    /// Files whose frontmatter contains a specific key/value pair.
    pub fn files_with_frontmatter(&self, key: &str, value: &str) -> Result<Vec<FileEntry>>;

    /// Files tagged with the given tag.
    pub fn files_by_tag(&self, tag: &str) -> Result<Vec<FileEntry>>;

    /// All headings in a file, or across all files.
    pub fn headings(&self, file_id: Option<i64>) -> Result<Vec<HeadingEntry>>;

    /// Table of contents for a single file (list of headings in order).
    pub fn toc(&self, file_id: i64) -> Result<Vec<HeadingEntry>>;

    /// Links of a specific type (inline, wikilink, etc.) or from a specific file.
    pub fn links(&self, filter: LinkFilter) -> Result<Vec<LinkEntry>>;

    /// Backlinks: files that link *to* a given target (e.g., a wikilink target).
    pub fn backlinks(&self, target: &str) -> Result<Vec<LinkEntry>>;

    /// Full-text search across all indexed files.
    /// Returns ranked results using FTS5 BM25 scoring.
    pub fn search(&self, query: &str) -> Result<Vec<SearchResult>>;
}

// ── Data types ────────────────────────────────────────

pub struct FileEntry {
    pub id: i64,
    pub relative_path: String,
    pub frontmatter: Option<String>,
    pub hash: String,
    pub size_bytes: i64,
    pub indexed_at: String,
}

pub struct HeadingEntry {
    pub id: i64,
    pub file_id: i64,
    pub level: u8,
    pub text: String,
    pub anchor: String,
}

pub struct LinkEntry {
    pub id: i64,
    pub file_id: i64,
    pub link_type: LinkType,
    pub target: String,
    pub text: Option<String>,
}

pub enum LinkType { Inline, Reference, Wikilink, Autolink, Image }

pub struct SearchResult {
    pub file_id: i64,
    pub relative_path: String,
    pub title: String,
    pub snippet: String,      // FTS5 snippet
    pub rank: f64,
}
```

---

## Filesystem Watching & Synchronisation

### Watch lifecycle

1. `Indexer::listen()` spawns a `notify` debounced watcher on the workspace
   root. It communicates via an `mpsc` channel.
2. The application calls `Indexer::poll()` periodically (or integrates into
   its own event loop) to drain the channel and apply updates.
3. Alternatively, the application can pass a callback: `on_event(Box<dyn Fn(WatchEvent)>)`.

### Incremental update logic

| Event | Action |
|---|---|
| `Create(path)` | Parse file, `upsert_file`, set headings/links/tags/FTS. |
| `Modify(path)` | Same as Create. |
| `Remove(path)` | `delete_file` (cascade deletes headings, links, tags, FTS row). |
| `Rename(old, new)` | Delete old, insert new. |

### Race conditions

- A file may be modified while being parsed. Mitigation: take a content hash
  before parsing; after parsing, verify the hash still matches. If not,
  re-parse (up to a retry limit).
- Rapid successive events (e.g., editor saving + linting). Mitigation: the
  debouncer coalesces events within the window.

---

## Determinism Guarantees

Determinism means: given the same workspace (same files, same content,
same directory structure), `scan_full()` produces bit-identical SQLite output
(ignoring `indexed_at` timestamps and auto-increment IDs that may differ
across runs).

### Mechanisms

1. **Stable ordering** — Files are processed in alphabetical order by their
   relative path (OS-independent comparison).
2. **Stable hashing** — `SHA-256` of file content.
3. **Deterministic slug generation** — Anchor slugs follow a fixed algorithm:
   - Lowercase
   - Strip HTML tags (if any)
   - Collapse whitespace to `-`
   - Strip leading/trailing `-`
   - Deduplicate by appending `-1`, `-2`, etc. (same as GitHub).
4. **Deterministic schema DDL** — `CREATE TABLE IF NOT EXISTS` with explicit
   column order.
5. **No random or time-based values in data** — `indexed_at` is the only
   timestamp, and it's allowed to differ. For testing/comparison we can
   normalise it.

---



---

## Design Decisions & Trade-offs

| Decision | Rationale |
|---|---|
| **SQLite via `rusqlite`** | Zero-config, embedded, widely used, supports FTS5. No server process. |
| **`pulldown-cmark`** | The fastest CommonMark parser in Rust; well-maintained; event-based (streaming). |
| **Native wikilink support** | `pulldown-cmark` 0.13+ provides `Options::ENABLE_WIKILINKS` which natively parses `[[...]]` patterns. No custom pre-processing needed. |
| **Separate `set_*` methods vs. one `index_file`** | Separation allows re-indexing partial data (e.g., only headings) if an application wants to extend Sakuin. |
| **Content hash for dedup** | Avoids re-parsing unchanged files during full scan. Not strictly needed for incremental sync, but useful for "re-scan everything" commands. |
| **Debounced file watching** | Raw `notify` events can fire many times per save. Debouncing coalesces them into one update. |
| **No async runtime** | Keeps dependencies minimal. The watcher uses `std::thread` + `mpsc`. Applications that want async can wrap the sync API. |
| **FTS5 vs. custom inverted index** | FTS5 is mature, supports BM25 ranking, snippets, and is built into SQLite. No reason to roll our own. |
| **Edition 2024** | Rust 2024 edition is the latest. Using it signals a modern codebase and gives access to `unsafe`-related improvements, `gen` blocks (when stable), etc. |

---



---


---

*This document describes the architecture and design of Sakuin. See [plan.md](plan.md) for the implementation roadmap.*
