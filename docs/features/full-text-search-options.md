# Feature Plan — Search Options (fields, limit/offset, prefix, case, snippets)

> Implementation plan for a configurable full-text search API in
> [`src/query.rs`](../../src/query.rs).
> Status: **planned** · Target crate version: 0.2.0 (breaking-ish, additive except schema)

---

## 1. Motivation

Today `Query::search(&str) -> rusqlite::Result<Vec<SearchResult>>` is a thin,
hard-coded wrapper over a single FTS5 `MATCH`: it always searches title +
body, always case-insensitively, always returns every hit, always builds a
16-token snippet, and it passes the caller's string straight to FTS5 — so a
user typing `foo "bar` produces a backend error rather than a usable query.

Applications embedding Sakuin (fuzzy finders, TUI pickers, MCP servers, web
search boxes) need:

- paging, because "give me the first 20 matches" is the common case;
- field restriction, to search paths only when the user types a `/`-prefixed
  query;
- prefix search for incremental type-ahead as the user types;
- case sensitivity for code identifiers (`Indexer` vs `indexer`);
- query escaping, so that any user input is treated as literal text;
- a typed error distinguishing "bad query" from "database problem".

---

## 2. Target API

```rust
/// Which FTS columns a query is allowed to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchField {
    Path,
    Title,
    Body,
}

/// Tuning knobs for [`Query::search_with_options`].
#[derive(Debug, Clone)]
pub struct SearchOptions<'a> {
    /// Maximum number of results. `None` = unlimited (today's behaviour).
    pub limit: Option<usize>,
    /// Number of leading results to skip (paging).
    pub offset: usize,
    /// Columns to search. Empty slice = all fields.
    pub fields: &'a [SearchField],
    /// Treat the query as a prefix query (`term` → `term*`).
    pub prefix: bool,
    /// Case-sensitive matching.
    pub case_sensitive: bool,
    /// Approximate snippet length, in FTS5 "tokens".
    pub snippet_tokens: usize,
}

impl Default for SearchOptions<'_> { /* fields = all, others off/zero */ }

impl<'a> SearchOptions<'a> {
    pub fn new() -> Self;                    // defaults above
    pub fn with_limit(mut self, limit: usize) -> Self;
    pub fn with_offset(mut self, offset: usize) -> Self;
    pub fn with_fields(mut self, fields: &'a [SearchField]) -> Self;
    pub fn with_prefix(mut self, prefix: bool) -> Self;
    pub fn with_case_sensitive(mut self, case_sensitive: bool) -> Self;
    pub fn with_snippet_tokens(mut self, snippet_tokens: usize) -> Self;
}

/// Error type for the typed search API.
#[derive(Debug)]
pub enum SearchError {
    /// The query text is empty/whitespace, or produced an FTS5 syntax error.
    InvalidQuery(String),
    /// The underlying SQLite operation failed.
    Backend(rusqlite::Error),
}

impl std::fmt::Display for SearchError { /* ... */ }
impl std::error::Error for SearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)>;
}
impl From<rusqlite::Error> for SearchError;

impl Query<'_> {
    /// Configurable, error-typed full-text search.
    pub fn search_with_options(
        &self,
        query: &str,
        options: &SearchOptions<'_>,
    ) -> Result<Vec<SearchResult>, SearchError>;

    /// `search` with the default options, but a typed error.
    pub fn try_search(&self, query: &str) -> Result<Vec<SearchResult>, SearchError>;

    /// Legacy raw-FTS5 entry point (unchanged signature, deprecated).
    pub fn search(&self, query: &str) -> rusqlite::Result<Vec<SearchResult>>;
}

/// Escape arbitrary user input into a single FTS5 phrase query.
pub fn quote_query(input: &str) -> String;
```

### Semantics (must hold, and be documented on each item)

| Option | Behaviour |
| --- | --- |
| `limit: None` | `LIMIT -1` → all matches (backwards compatible) |
| `limit: Some(n)` | `LIMIT n` |
| `offset: k` | `LIMIT … OFFSET k`; returns empty when `offset >= total` |
| `fields: []` | Treated as all fields (documented, not an error) |
| `fields: [Path]` | Matches tokens from the file's `relative_path` only |
| `prefix: true` | Query becomes a phrase with a trailing `*` on the last token: `"rust"*` |
| `case_sensitive: true` | Uses the case-sensitive FTS5 table (see §3) |
| `snippet_tokens: 0` | Snippet suppressed → `SearchResult::snippet` is `""` |
| empty/whitespace `query` | `Ok(vec![])` — same as today (not an error) |
| `""` inside query | Escaped by `quote_query`, never a syntax error |

Ordering stays `ORDER BY rank, f.relative_path` (deterministic, unchanged).

---

## 3. Schema changes

The current FTS5 table is:

```sql
CREATE VIRTUAL TABLE fts USING fts5(
    file_id UNINDEXED,
    title,
    body,
    tokenize='porter unicode61'
);
```

Two capabilities need data the table does not have: the **path** column and a
**case-sensitive** variant.

### Migration `002-fts-fields/up.sql`

```sql
-- 1. Case-insensitive table, now including the file path as a searchable column.
CREATE VIRTUAL TABLE IF NOT EXISTS fts_new USING fts5(
    file_id UNINDEXED,
    path,
    title,
    body,
    tokenize='porter unicode61'
);

INSERT INTO fts_new (file_id, path, title, body)
SELECT fts.file_id, files.relative_path, fts.title, fts.body
FROM fts JOIN files ON files.id = fts.file_id;

DROP TABLE fts;
ALTER TABLE fts_new RENAME TO fts;

-- 2. Case-sensitive mirror, same content, ASCII/unicode tokeniser without
--    case folding. `porter` is intentionally NOT used here: it folds case.
CREATE VIRTUAL TABLE IF NOT EXISTS fts_case USING fts5(
    file_id UNINDEXED,
    path,
    title,
    body,
    tokenize = "unicode61 tokenchars '_' remove_diacritics 2 case_sensitive 1"
);
INSERT INTO fts_case (file_id, path, title, body)
SELECT file_id, path, title, body FROM fts;
```

`002-fts-fields/down.sql` recreates the original two-column table and drops
`fts_case`.

### Trade-offs accepted

- **Storage doubles** for the FTS content. Accepted for now: it is the only
  portable way to get true case sensitivity in FTS5 (SQLite exposes
  `case_sensitive` only as a *table* option). Documented as a known cost; a
  future option could make `fts_case` opt-in at index build time.
- **Write path doubles.** `IndexStore::set_fts` and the delete helpers must
  touch both tables in the same transaction (see §4).
- **Index rebuild required** for existing databases. `IndexStore::open`
  already runs `rusqlite_migration`, so the upgrade is automatic; no
  re-indexing is needed because migration 002 backfills from `files`.

---

## 4. Implementation steps

### Step 1 — Migration
- [ ] `migrations/002-fts-fields/{up,down}.sql` as above.
- [ ] Confirm `rusqlite_migration`'s `from-directory` picks up the new folder
      automatically (it does — no registry change needed).
- [ ] Store-side test: open a store, index a file, assert the path is
      searchable and `fts_case` holds a row.

### Step 2 — Store write path (`src/store/mod.rs`)
- [ ] `set_fts(file_id, title, body)` gains the path internally: it looks up
      `relative_path` from `files` and writes `(file_id, path, title, body)`
      into **both** `fts` and `fts_case` in one transaction.
      To avoid changing the public signature, look up the path inside
      `set_fts`; if that costs a lookup on the hot path, add a
      `set_fts_with_path` and keep `set_fts` as a thin wrapper.
- [ ] Deletion of a file cascades into `fts`/`fts_case` — the tables are
      contentless-free (they store `file_id` as `UNINDEXED`), so the existing
      `ON DELETE CASCADE` from `files` does **not** apply. Add explicit
      `DELETE FROM fts WHERE file_id = ?` / `fts_case` wherever `set_fts`
      previously deleted, and verify the file-removal test still passes.

### Step 3 — `quote_query` (`src/query.rs`)
- [ ] Implementation:
      1. `input.trim()`; collapse internal whitespace runs to single spaces
         (FTS5 phrases do not span whitespace runs cleanly).
      2. Strip FTS5 control characters: `\0`.
      3. Double every `"` (FTS5 string escaping is `""` inside a phrase).
      4. Wrap the result in `"`; if `prefix`, append `*` **outside** the
         closing quote: `"rust"*`.
- [ ] Pure unit tests in `src/query.rs`: empty input, quotes, backslashes,
      `AND`/`OR`/`NEAR` as literals, unicode, `prefix` shape.

### Step 4 — `SearchField` / `SearchOptions` / `SearchError`
- [ ] New types in `src/query.rs`, re-exported from `src/lib.rs` alongside
      the existing `pub use query::{…}` list.
- [ ] `#![warn(missing_docs)]` is on: every public item and every enum variant
      needs a doc comment.
- [ ] `SearchError::InvalidQuery` is produced for: blank query, and backend
      errors recognised as FTS5 syntax errors — detect via
      `rusqlite::Error::SqliteFailure(_, Some(message))` where the message
      contains `"fts5"` / `"malformed MATCH"` / `"syntax error near"`.
      Everything else maps to `Backend`. Heuristic lives in one private helper
      `fn is_query_syntax_error(error: &rusqlite::Error) -> bool`, with a
      comment noting it is a heuristic and may need updating on SQLite bumps.
- [ ] Unit test for the classifier against messages produced by the bundled
      SQLite (unbalanced quote, `NEAR` misuse).

### Step 5 — `search_with_options`
- [ ] Build the SQL dynamically (the column list and the `bm25` weights are
      data-dependent), but keep **all user values bound as parameters** —
      the query string is the only interpolated value and it comes from
      `quote_query`.
- [ ] Shape:
      ```sql
      SELECT f.id, f.relative_path,
             CASE WHEN fts.title IS NULL OR fts.title = ''
                  THEN f.relative_path ELSE fts.title END,
             snippet(<table>, '<b>', '</b>', '...', ?tokens),
             bm25(<table>, ?w0, ?w1, ...)
      FROM <table>
      JOIN files f ON f.id = <table>.file_id
      WHERE <table> MATCH ?1
      ORDER BY bm25(<table>, ?w0, ...), f.relative_path
      LIMIT ?2 OFFSET ?3
      ```
      - `LIMIT ?2` is bound to `-1` when `limit` is `None` (`i64`).
      - `snippet(...)` and `bm25(...)` reference the table by its real name so
        the `column` argument of `snippet` is the *body* column index, which
        is now `3`; compute it from the field list instead of hard-coding.
      - When `snippet_tokens == 0`, bind an empty snippet via
        `CASE WHEN ?tokens = 0 THEN '' ELSE snippet(...) END` to keep a single
        prepared-statement shape (simpler than two SQL strings).
      - `bm25` weights: per-column weights `[0.6, 0.9, 1.0]` for
        (path, title, body) by default; a future `SearchOptions::with_weights`
        is out of scope. Document that `rank` semantics are unchanged
        (lower is better) even though the weight vector differs.
- [ ] Table selection: `fts_case` when `case_sensitive`, else `fts`.
- [ ] Blank query → `Ok(Vec::new())` before any SQL is built.
- [ ] Fields→column projection helper:
      `fn selected_fields(fields: &[SearchField]) -> Vec<SearchField>`
      returning the full default set when `fields` is empty.
- [ ] Row mapping reuses a new shared `search_result_from_row` so `search`
      and `search_with_options` cannot drift.

### Step 6 — `try_search` and `search`
- [ ] `try_search(q) = search_with_options(q, &SearchOptions::new())`.
- [ ] Keep `search` with its current signature and raw-FTS5 passthrough
      semantics; re-implement it as a thin call into the same internal
      `execute_search` with `raw_query: true` so the two never diverge.
      Mark it `#[deprecated(note = "use try_search / search_with_options")]`
      and add a migration note to the README. `examples/basic-index.rs` and
      `benches/bench_index.rs` are updated to `try_search`.

### Step 7 — Docs, examples, benches
- [ ] README: new "Search" section showing the builder and a table of
      options; schema bullet updated (`fts`, `fts_case`).
- [ ] `examples/basic-index.rs`: demonstrate paging (`limit`/`offset`) and a
      prefix query.
- [ ] `benches/bench_index.rs`: add a `search_prefix` criterion.
- [ ] `docs/design.md`: note the second FTS table in the storage section.

---

## 5. Testing strategy

### Unit tests (`src/query.rs` `#[cfg(test)]`)
- `quote_query`: table-driven cases (see Step 3).
- `search_with_options` default options equals `try_search` results.
- `limit`/`offset`: 0, 1, 5, `None`; `offset` past the end → empty; paging is
  stable (page 1 + page 2 has no overlap and no gaps, ordered by path).
- `fields`: `[Path]` matches only files whose path contains the term;
  `[Title]`; `[Body]`; `[]` == all three.
- `prefix: true` matches `docu` but plain search does not; `prefix` on a
  multi-word phrase still requires the full phrase (documented behaviour).
- `case_sensitive: true`: `Indexer` matches, `indexer` does not; and the
  reverse for the default.
- `snippet_tokens`: `0` → empty snippet; larger values produce longer
  snippets (compare lengths on a long-body fixture).
- Errors: `""`/`"   "` → `Ok(empty)`; `"unterminated` → `InvalidQuery`;
  closed connection → `Backend`.
- Regression: results from `search_with_options` are a prefix of results from
  the old `search` for the same query, for the default options.

### Integration tests (`tests/search_options.rs`, new)
Build a temp workspace via the public `Indexer` API (no store internals), then
assert the same matrix end-to-end. This is the layer that catches a migration
that works in unit tests but does not populate `fts_case` for files indexed
through the watcher path.

### Property tests (`proptest`, already a dev-dependency)
- `quote_query(p)` output always parses: feeding arbitrary input to
  `search_with_options` never yields `InvalidQuery`.
- `limit = n, offset = k` returns exactly `min(n, max(0, total - k))` rows.
- Ranking is deterministic across repeated identical queries.

### CI
Unchanged: `cargo fmt --check`, `cargo clippy -D warnings`,
`cargo build --all-targets`, `cargo test`. Cannot be run locally on this
machine (no Rust toolchain) — verification happens on the user's side.

---

## 6. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Migration doubles FTS storage | Documented; `fts_case` is a plain table that can be dropped and re-created by a future opt-in flag |
| FTS5 syntax-error detection is a string heuristic | Single helper, unit-tested against the bundled SQLite, comment warns about SQLite upgrades |
| `bm25` weight change alters `rank` values | Relative ordering is preserved (body still heaviest); document, and add a regression test on ordering, not on exact score |
| Deleting from a non-contentless FTS table on file removal | Explicit `DELETE ... WHERE file_id = ?` in the same transaction; covered by a new store test |
| Dynamic SQL invites injection | Only the quoted query string is interpolated; document and test with a payload like `"; DROP TABLE files; --` |
| `snippet()` column index shifts when `path` is inserted | Derive the index from the field list, and assert snippet content in tests (not just shape) |

---

## 7. Rollout / checklist summary

1. Migration `002-fts-fields` + store write path.
2. `quote_query` (pure, no dependencies) — safe to land first.
3. `SearchField`, `SearchOptions`, `SearchError` types.
4. `search_with_options`, `try_search`, shared row mapping.
5. Deprecate `search`; update example, bench, README, design docs.
6. Full test pass (`cargo test`, `cargo clippy -D warnings`, `cargo fmt`) on a
   machine with a Rust toolchain.

Out of scope for this feature: fuzzy/matching scoring, per-field weights,
searching frontmatter or tags, regex or glob search, highlighting customisation
beyond `<b>`/`</b>`, and a query parser for boolean operators in user input
(`quote_query` deliberately treats `AND`/`OR` as literals).
