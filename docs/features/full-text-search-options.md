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
| `case_sensitive: true` | Post-filters the FTS candidates with case-sensitive `GLOB` (see §3) |
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

One capability needs data the table does not have: the **path** column. The
**case-sensitive** variant turned out not to need a schema change at all — see
"Case sensitivity" below.

### Migration `002-fts-fields/up.sql`

```sql
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
```

`002-fts-fields/down.sql` recreates the original two-column table.

### Case sensitivity — no second table

An earlier draft of this plan added an `fts_case` mirror table using
`tokenize = "unicode61 … case_sensitive 1"`. **That option does not exist.**
`case_sensitive` is a `trigram`-only tokenizer option. In the bundled SQLite
(`libsqlite3-sys` 0.38.2, SQLite 3.53.2) `fts5UnicodeCreate` accepts exactly
four options — `remove_diacritics`, `tokenchars`, `separators`,
`categories` — and folds case unconditionally through a hardcoded fold table.
Creating the table fails outright with *"error in tokenizer constructor"*.

Since `porter` also folds case, there is no way to make a `unicode61` FTS5
table case-sensitive, and therefore no table-based design for this feature.

The approach actually used is a **two-stage query**:

1. `fts MATCH ?` generates candidates with the normal case-insensitive index
   (cheap, uses the inverted index).
2. A case-sensitive `GLOB` filter re-checks the candidate rows' stored text.
   `GLOB` is case-sensitive where `LIKE` is not.

```sql
SELECT … FROM fts JOIN files f ON f.id = fts.file_id
WHERE fts.file_id IN (SELECT file_id FROM fts WHERE fts MATCH ?1)
  AND (fts.title GLOB ?2 OR fts.body GLOB ?2 OR f.relative_path GLOB ?2)
```

with `?2` bound to `*query*`. Verified against the bundled SQLite: searching
`indexer` returns the row containing `Indexer`, and the `*Indexer*` GLOB filter
selects it while `*indexer*` selects nothing.

Trade-offs:

- **Candidate set is broader than the final result**, so `limit`/`offset` must
  be applied *after* the `GLOB` filter, not pushed into SQL. The `GLOB` step
  degrades to a scan of the candidate rows; that is fine at typical candidate
  sizes but is O(candidates), not O(log n).
- **Substring, not token, matching.** `GLOB` matches anywhere in the text,
  whereas FTS5 matches whole tokens. For code identifiers that is arguably the
  more useful behaviour, but it is a semantic difference and must be
  documented on the option.
- **Wildcards in the query need escaping** — a user searching for `*` would
  otherwise produce a degenerate pattern. `quote_query` must escape
  `*`/`?`/`[` in the GLOB pattern (but *not* in the FTS5 MATCH argument).

### Other trade-offs

- **Index rebuild required** for existing databases. `IndexStore::open`
  already runs `rusqlite_migration`, so the upgrade is automatic; no
  re-indexing is needed because migration 002 backfills from `files`.

---

## 4. Implementation steps

### Step 1 — Migration ✅
- [x] `migrations/002-fts-fields/{up,down}.sql` as above.
- [x] Confirm `rusqlite_migration`'s `from-directory` picks up the new folder
      automatically (it does — no registry change needed).
- [x] Store-side tests: `migration_002_adds_path_column_to_fts` asserts the
      column layout; `migration_002_backfills_path_into_existing_fts_rows`
      builds a database at version 1, populates it, and asserts the upgrade
      backfills `path` and leaves the path searchable.
- [x] Interim shim so the tree stays green: `set_fts` now looks up
      `relative_path` from `files` and writes it, and the two `snippet(fts, 2, …)`
      call sites (in `query.rs` and `store/mod.rs` tests) moved to column `3`,
      since `body` shifted from index 2 to 3. Verified with the full CI gate
      on a real toolchain: 125 tests pass, `cargo fmt --check` and
      `cargo clippy -D warnings` clean.

### Step 2 — Store write path (`src/store/profile` cleanup) ✅ (done as part of step 1)
- [x] `set_fts(file_id, title, body)` looks up `relative_path` from `files` and
      writes `(file_id, path, title, body)`. Public signature unchanged.
- [x] `delete_file` already deletes from `fts` explicitly; no second table to
      keep in sync. The original concern about `fts_case` is moot.

### Step 3 — `quote_query` (`src/query.rs`) ✅
- [x] Implementation:
      1. `\0` characters are stripped first (they are not whitespace, so they
         would otherwise survive inside a token).
      2. `split_whitespace().join(" ")` — this trims *and* collapses internal
         whitespace runs to single spaces in one step.
      3. Every `"` is doubled (FTS5 string escaping is `""` inside a phrase).
      4. Wrap the result in `"`; `quote_query_prefix(input, prefix)` appends
         `*` **outside** the closing quote: `"rust"*`.
- [x] Both functions are `pub` in `src/query.rs` and documented (`#![warn(
      missing_docs)]` is on). They are reachable as
      `sakuin_md::query::quote_query` because `query` is a `pub mod`; the
      crate-root re-export is deferred to Step 4.
- [x] Pure unit tests in `src/query.rs`: whitespace collapsing, NUL
      stripping, quote doubling, `AND`/`OR`/`NOT`/`NEAR`/`*`/`^`/`:`/`-`/
      parens as literal phrase text, backslashes left untouched, unicode, the
      prefix shape, and the documented blank-input caveat.
- [x] One non-pure test (`quote_query_output_is_accepted_by_fts5`) runs each
      quoted form through the bundled SQLite, so the escaping is verified
      against the real tokenizer rather than only against expected strings.
- [x] Verified with `cargo fmt --check`, `cargo clippy --all-targets
      -- -D warnings`, and `cargo test`: 135 tests + 3 doctests pass, fmt and
      clippy clean (run inside the `sakuin-dev` image built from the root
      `Dockerfile`; see `docs/development.md`). `rustfmt` reformatted the
      prefix helper and two test assertions, and the blank-input test caught
      a wrong expectation of mine (`quote_query("")` is `""`, not `""""`).

### Step 4 — `SearchField` / `SearchOptions` / `SearchError` ✅
- [x] New types in `src/query.rs`, re-exported from `src/lib.rs` alongside
      the existing `pub use query::{…}` list (`quote_query` and
      `quote_query_prefix` are re-exported too, so applications never need to
      reach into the module).
- [x] `#![warn(missing_docs)]` is on: every public item and every enum variant
      has a doc comment.
- [x] `SearchError::InvalidQuery` is produced for: blank query, and backend
      errors recognised as FTS5 syntax errors — via the private helper
      `fn is_query_syntax_error(error: &rusqlite::Error) -> bool`, which
      matches on `rusqlite::Error::SqliteFailure(_, Some(message))`.
- [x] The heuristics were **captured from the bundled SQLite** with a
      throwaway probe rather than guessed. The real messages are:

      | Query | Message | Matched? |
      | --- | --- | --- |
      | `"`, `a b"` | `unterminated string` | yes (`unterminated string`) |
      | `NEAR/2` | `fts5: syntax error near "/"` | yes (`fts5:`) |
      | `a AND`, `(a`, `""` (empty phrase) | `fts5: syntax error near ""` | yes (`fts5:`) |
      | `*` | `unknown special query: ` | yes (`unknown special query`) |
      | `nosuchcol:fox` | `no such column: nosuchcol` | **no** → `Backend` |
      | missing table | `no such table: files` | **no** → `Backend` |

      `malformed match` is also matched defensively; the bundled SQLite does
      not currently emit it.
- [x] The doc comment on `is_query_syntax_error` carries the message table,
      states that it is a heuristic that must be re-captured on a SQLite
      upgrade, and explains that unrecognised errors fall back to `Backend`
      because that is the safe direction.
- [x] Classifier tests run the queries through the real FTS5 table and assert
      on the resulting classification, rather than on hand-written
      `SqliteFailure` values alone.
- [x] Verified: `cargo fmt --check`, `cargo clippy --all-targets
      -- -D warnings`, and `cargo test` — 144 tests + 4 doctests pass.
      The run corrected a factual error: FTS5 *accepts* the two-character
      phrase `""` (a zero-token phrase matching nothing) — only `""""` is a
      syntax error — so the `quote_query` doc no longer claims blank input
      produces an unmatchable query.

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
        is now `3` after migration 002; compute it from the field list instead
        of hard-coding.
      - When `snippet_tokens == 0`, bind an empty snippet via
        `CASE WHEN ?tokens = 0 THEN '' ELSE snippet(...) END` to keep a single
        prepared-statement shape (simpler than two SQL strings).
      - `bm25` weights: per-column weights `[0.6, 0.9, 1.0]` for
        (path, title, body) by default; a future `SearchOptions::with_weights`
        is out of scope. Document that `rank` semantics are unchanged
        (lower is better) even though the weight vector differs.
- [ ] Case sensitivity: when `case_sensitive` is set, add the `GLOB` filter
      from §3 to the `WHERE` clause, with a `*`-escaped pattern bound as a
      parameter. Because it filters candidates, `LIMIT`/`OFFSET` can still be
      pushed into SQL — the `GLOB` runs before `LIMIT` in the same statement.
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
      options; schema bullet updated (`fts` now carries `path`).
- [ ] `examples/basic-index.rs`: demonstrate paging (`limit`/`offset`) and a
      prefix query.
- [ ] `benches/bench_index.rs`: add a `search_prefix` criterion.
- [ ] `docs/design.md`: note the `path` column in the storage section.

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
  reverse for the default. Also test a `*` in the query to confirm it cannot
  corrupt the `GLOB` pattern.
- `snippet_tokens`: `0` → empty snippet; larger values produce longer
  snippets (compare lengths on a long-body fixture).
- Errors: `""`/`"   "` → `Ok(empty)`; `"unterminated` → `InvalidQuery`;
  closed connection → `Backend`.
- Regression: results from `search_with_options` are a prefix of results from
  the old `search` for the same query, for the default options.

### Integration tests (`tests/search_options.rs`, new)
Build a temp workspace via the public `Indexer` API (no store internals), then
assert the same matrix end-to-end. This is the layer that catches a migration
that works in unit tests but leaves `path` empty for files indexed through the
watcher path.

### Property tests (`proptest`, already a dev-dependency)
- `quote_query(p)` output always parses: feeding arbitrary input to
  `search_with_options` never yields `InvalidQuery`.
- `limit = n, offset = k` returns exactly `min(n, max(0, total - k))` rows.
- Ranking is deterministic across repeated identical queries.

### CI
`cargo fmt --check`, `cargo clippy -D warnings`, `cargo build --all-targets`,
`cargo test`. These can be run locally — see `docs/development.md` for
environment setup. Verified green on this machine (2026-09-23): 125 tests
pass, fmt and clippy clean.

---

## 6. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Case-sensitive `GLOB` filter is O(candidates) | Only paid when `case_sensitive` is explicitly requested; default path is unchanged. Document as a known cost. |
| `GLOB` is substring- not token-matching | Documented on the option; arguably desirable for code identifiers. |
| `bm25` weight change alters `rank` values | Relative ordering is preserved (body still heaviest); document, and add a regression test on ordering, not on exact score |
| FTS5 syntax-error detection is a string heuristic | Single helper, unit-tested against the bundled SQLite, comment warns about SQLite upgrades |
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
