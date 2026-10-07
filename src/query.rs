//! Read-only query interface over the index.
//!
//! [`Query`] provides typed, read-only access to a Sakuin index. Obtain a
//! handle either from an [`IndexStore`](crate::store::IndexStore) via
//! [`IndexStore::query`](crate::store::IndexStore::query), or directly from
//! a `rusqlite::Connection` via [`Query::new`].

use rusqlite::{Connection, params};

pub use crate::parser::LinkType;

use crate::store::link_type_from_str;

/// A read-only handle over an indexed workspace.
///
/// Every method executes read-only SQL against the underlying SQLite
/// connection; it is not possible to mutate the index through a `Query`.
pub struct Query<'conn> {
    conn: &'conn Connection,
}

impl<'conn> Query<'conn> {
    /// Create a query handle over an existing SQLite connection.
    ///
    /// The connection must point at a database created by
    /// [`IndexStore::open`](crate::store::IndexStore::open) (i.e. one that
    /// has the Sakuin schema applied). Prefer
    /// [`IndexStore::query`](crate::store::IndexStore::query) when you have
    /// a store at hand.
    pub fn new(conn: &'conn Connection) -> Self {
        Self { conn }
    }

    /// All indexed files, optionally filtered by a glob pattern.
    ///
    /// `pattern` follows SQLite `GLOB` syntax: `*` matches any sequence of
    /// characters (including `/`), `?` matches a single character. Matching
    /// is case-sensitive and performed against `relative_path`. Pass `None`
    /// to return every indexed file. Results are sorted by `relative_path`.
    pub fn files(&self, pattern: Option<&str>) -> rusqlite::Result<Vec<FileEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT id, relative_path, frontmatter, hash, size_bytes, indexed_at
             FROM files
             WHERE (?1 IS NULL OR relative_path GLOB ?1)
             ORDER BY relative_path",
        )?;
        let rows = statement.query_map(params![pattern], file_entry_from_row)?;
        rows.collect()
    }

    /// Files whose raw frontmatter text contains `key: value`.
    ///
    /// Matching is a case-insensitive substring search over the stored
    /// frontmatter YAML text, so it works for scalar values (`title: Docs`)
    /// and, more loosely, for anything rendered on one line. For tag lists
    /// (which are stored as YAML arrays), prefer [`Query::files_by_tag`].
    /// Results are sorted by `relative_path`.
    pub fn files_with_frontmatter(
        &self,
        key: &str,
        value: &str,
    ) -> rusqlite::Result<Vec<FileEntry>> {
        let pattern = format!("%{}: {}%", escape_like(key), escape_like(value));
        let mut statement = self.conn.prepare(
            "SELECT id, relative_path, frontmatter, hash, size_bytes, indexed_at
             FROM files
             WHERE frontmatter IS NOT NULL AND frontmatter LIKE ?1 ESCAPE '\\'
             ORDER BY relative_path",
        )?;
        let rows = statement.query_map(params![pattern], file_entry_from_row)?;
        rows.collect()
    }

    /// Files tagged with the given tag.
    ///
    /// Tags come from the `tags` key in the frontmatter. Results are sorted
    /// by `relative_path`.
    pub fn files_by_tag(&self, tag: &str) -> rusqlite::Result<Vec<FileEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT f.id, f.relative_path, f.frontmatter, f.hash, f.size_bytes, f.indexed_at
             FROM files f
             JOIN tags t ON t.file_id = f.id
             WHERE t.tag = ?1
             ORDER BY f.relative_path",
        )?;
        let rows = statement.query_map(params![tag], file_entry_from_row)?;
        rows.collect()
    }

    /// All headings in a file, or across all files.
    ///
    /// Pass `Some(file_id)` to restrict to a single file, or `None` to get
    /// every heading in the index. Results are ordered by file, then by
    /// position within the source document.
    pub fn headings(&self, file_id: Option<i64>) -> rusqlite::Result<Vec<HeadingEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT id, file_id, level, text, anchor
             FROM headings
             WHERE (?1 IS NULL OR file_id = ?1)
             ORDER BY file_id, position",
        )?;
        let rows = statement.query_map(params![file_id], heading_entry_from_row)?;
        rows.collect()
    }

    /// Table of contents for a single file.
    ///
    /// Returns the file's headings in document order (by position), which
    /// for most files is a depth-first listing suitable for a TOC.
    pub fn toc(&self, file_id: i64) -> rusqlite::Result<Vec<HeadingEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT id, file_id, level, text, anchor
             FROM headings
             WHERE file_id = ?1
             ORDER BY position",
        )?;
        let rows = statement.query_map(params![file_id], heading_entry_from_row)?;
        rows.collect()
    }

    /// Links matching the given filter.
    ///
    /// Use [`LinkFilter::new`] for every link, then narrow with
    /// [`LinkFilter::with_file_id`] and [`LinkFilter::with_link_type`].
    /// Results are ordered by file, then by position within the source
    /// document.
    pub fn links(&self, filter: LinkFilter) -> rusqlite::Result<Vec<LinkEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT id, file_id, link_type, target, text
             FROM links
             WHERE (?1 IS NULL OR file_id = ?1)
               AND (?2 IS NULL OR link_type = ?2)
             ORDER BY file_id, position",
        )?;
        let rows = statement.query_map(
            params![
                filter.file_id,
                filter.link_type.map(|link_type| link_type.as_str())
            ],
            link_entry_from_row,
        )?;
        rows.collect()
    }

    /// Backlinks: links whose target equals `target`.
    ///
    /// A "backlink" is any stored link — wikilink, inline, image, etc. —
    /// whose `target` field matches exactly. This is useful for answering
    /// "which files reference `docs`?" in a wiki. Results are ordered by
    /// file, then by position within the source document.
    pub fn backlinks(&self, target: &str) -> rusqlite::Result<Vec<LinkEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT id, file_id, link_type, target, text
             FROM links
             WHERE target = ?1
             ORDER BY file_id, position",
        )?;
        let rows = statement.query_map(params![target], link_entry_from_row)?;
        rows.collect()
    }

    /// Full-text search across all indexed files.
    ///
    /// Runs an FTS5 `MATCH` query over titles and bodies, returning results
    /// ranked best-first by BM25 score. `snippet` is a short excerpt of the
    /// matching body text with matches wrapped in `<b>` tags; `rank` is the
    /// raw FTS5 BM25 score, where *lower* values indicate better matches.
    ///
    /// An empty or whitespace-only query returns no results. Invalid FTS5
    /// query syntax (for example an unbalanced `"`) is returned as an error.
    pub fn search(&self, query: &str) -> rusqlite::Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self.conn.prepare(
            "SELECT f.id, f.relative_path,
                    CASE WHEN fts.title IS NULL OR fts.title = ''
                         THEN f.relative_path ELSE fts.title END,
                    snippet(fts, 3, '<b>', '</b>', '...', 16),
                    bm25(fts)
             FROM fts
             JOIN files f ON f.id = fts.file_id
             WHERE fts MATCH ?1
             ORDER BY bm25(fts), f.relative_path",
        )?;
        let rows = statement.query_map(params![query], |row| {
            Ok(SearchResult {
                file_id: row.get(0)?,
                relative_path: row.get(1)?,
                title: row.get(2)?,
                snippet: row.get(3)?,
                rank: row.get(4)?,
            })
        })?;
        rows.collect()
    }
}

// ── Search options ─────────────────────────────────────

/// Which columns a search query is allowed to match.
///
/// These map onto the columns of the `fts` FTS5 table introduced by migration
/// `002-fts-fields`: `path`, `title`, and `body`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchField {
    /// The file's path relative to the workspace root (`files.relative_path`).
    Path,
    /// The document title.
    Title,
    /// The indexed body text.
    Body,
}

/// Tuning knobs for [`Query::search_with_options`].
///
/// [`SearchOptions::new`] (and [`Default`]) search every field, are
/// case-insensitive, return every match, and build a 16-token snippet — that
/// is, the behaviour of [`Query::search`].
///
/// ```
/// # use sakuin_md::{SearchOptions, SearchField};
/// let fields = [SearchField::Path];
/// let options = SearchOptions::new().with_limit(20).with_fields(&fields);
/// assert_eq!(options.limit, Some(20));
/// assert_eq!(options.offset, 0);
/// ```
#[derive(Debug, Clone)]
pub struct SearchOptions<'a> {
    /// Maximum number of results. `None` means unlimited, which is the
    /// behaviour of the un-paged [`Query::search`].
    pub limit: Option<usize>,
    /// Number of leading results to skip. Combined with `limit` this pages
    /// through the result set; an `offset` past the end returns no results.
    pub offset: usize,
    /// Columns the query may match. An empty slice means every column.
    pub fields: &'a [SearchField],
    /// Treat the query as a prefix query, so `docu` also matches `document`.
    pub prefix: bool,
    /// Require the matched text to have the same case as the query. Useful for
    /// code identifiers, where `Indexer` and `indexer` are different things.
    pub case_sensitive: bool,
    /// Approximate snippet length, in FTS5 tokens. `0` suppresses snippets
    /// entirely and yields an empty `SearchResult::snippet`.
    pub snippet_tokens: usize,
}

impl Default for SearchOptions<'_> {
    fn default() -> Self {
        Self {
            limit: None,
            offset: 0,
            fields: &[],
            prefix: false,
            case_sensitive: false,
            snippet_tokens: 16,
        }
    }
}

impl<'a> SearchOptions<'a> {
    /// Options equivalent to the current behaviour of [`Query::search`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of results.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set the number of leading results to skip.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Restrict matching to `fields`. An empty slice means every field.
    pub fn with_fields(mut self, fields: &'a [SearchField]) -> Self {
        self.fields = fields;
        self
    }

    /// Set whether the query is a prefix query.
    pub fn with_prefix(mut self, prefix: bool) -> Self {
        self.prefix = prefix;
        self
    }

    /// Set whether matching is case-sensitive.
    pub fn with_case_sensitive(mut self, case_sensitive: bool) -> Self {
        self.case_sensitive = case_sensitive;
        self
    }

    /// Set the approximate snippet length, in tokens. `0` suppresses snippets.
    pub fn with_snippet_tokens(mut self, snippet_tokens: usize) -> Self {
        self.snippet_tokens = snippet_tokens;
        self
    }
}

/// Error type for the typed search API.
///
/// This exists so that a caller can tell "the user typed something FTS5 cannot
/// parse" apart from "the database failed", without matching on message text.
#[derive(Debug)]
pub enum SearchError {
    /// The query was empty or whitespace only, or produced an FTS5 syntax
    /// error. The payload is a human-readable explanation.
    InvalidQuery(String),
    /// The underlying SQLite operation failed for a reason unrelated to the
    /// query text.
    Backend(rusqlite::Error),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::InvalidQuery(message) => write!(formatter, "invalid query: {message}"),
            SearchError::Backend(error) => write!(formatter, "search failed: {error}"),
        }
    }
}

impl std::error::Error for SearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SearchError::InvalidQuery(_) => None,
            SearchError::Backend(error) => Some(error),
        }
    }
}

impl From<rusqlite::Error> for SearchError {
    fn from(error: rusqlite::Error) -> Self {
        if is_query_syntax_error(&error) {
            SearchError::InvalidQuery(error.to_string())
        } else {
            SearchError::Backend(error)
        }
    }
}

/// Decide whether a SQLite error is the caller's fault (bad query text) rather
/// than a backend failure.
///
/// This is a **heuristic on message text**, because FTS5 reports syntax
/// problems through `SQLITE_ERROR` with no dedicated extended code. The
/// patterns below are the ones the bundled SQLite actually produces; a future
/// SQLite version may word them differently, so the unit tests assert against
/// messages captured from this SQLite build and should be re-captured on an
/// upgrade. Anything unrecognised is treated as a backend error, which is the
/// safe direction: a mis-classified *backend* error merely shows up as an
/// `InvalidQuery`, whereas the reverse would hide real failures.
///
/// Known messages, from `libsqlite3-sys` 0.38.x (SQLite 3.53.x):
///
/// | Query | Message |
/// | --- | --- |
/// | `"`, `a b"` | `unterminated string` |
/// | `NEAR/2` | `fts5: syntax error near "/"` |
/// | `a AND`, `(a` | `fts5: syntax error near ""` |
/// | `*` | `unknown special query: ` |
/// | `""` (empty phrase) | `fts5: syntax error near ""` |
///
/// Deliberately *not* matched: `no such table: …` and `no such column: …`.
/// Those indicate a schema or code problem, not a bad query.
fn is_query_syntax_error(error: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(_, Some(message)) = error else {
        return false;
    };
    let message = message.to_lowercase();
    [
        "fts5:",
        "malformed match",
        "unterminated string",
        "unknown special query",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

// ── Data types ─────────────────────────────────────────

/// Metadata about an indexed file, as returned by the query API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The file's database id.
    pub id: i64,
    /// The file's path relative to the workspace root.
    pub relative_path: String,
    /// Raw YAML frontmatter, if the file had any.
    pub frontmatter: Option<String>,
    /// SHA-256 hash of the file content.
    pub hash: String,
    /// Size of the file in bytes.
    pub size_bytes: i64,
    /// When the file was last indexed (`YYYY-MM-DD HH:MM:SS` UTC).
    pub indexed_at: String,
}

/// A heading in an indexed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingEntry {
    /// The heading row's database id.
    pub id: i64,
    /// The id of the file this heading belongs to.
    pub file_id: i64,
    /// The heading level, from `1` to `6`.
    pub level: u8,
    /// The heading text.
    pub text: String,
    /// The GitHub-style anchor slug for this heading.
    pub anchor: String,
}

/// A link stored in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEntry {
    /// The link row's database id.
    pub id: i64,
    /// The id of the file this link originates from.
    pub file_id: i64,
    /// What kind of link this is.
    pub link_type: LinkType,
    /// The link destination (URL or wikilink target).
    pub target: String,
    /// The link text / label, if any.
    pub text: Option<String>,
}

/// A full-text search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// The id of the matching file.
    pub file_id: i64,
    /// The matching file's path relative to the workspace root.
    pub relative_path: String,
    /// The file's title (first heading, falling back to the path).
    pub title: String,
    /// An excerpt of the matching body text, with matches wrapped in `<b>`.
    pub snippet: String,
    /// The FTS5 BM25 score; lower values are better matches.
    pub rank: f64,
}

/// Filter for [`Query::links`].
///
/// Both fields are optional: `None` means "any". Use the builder methods to
/// narrow the filter.
#[derive(Debug, Clone, Default)]
pub struct LinkFilter {
    /// Only links originating from the file with this id.
    pub file_id: Option<i64>,
    /// Only links of this type.
    pub link_type: Option<LinkType>,
}

impl LinkFilter {
    /// An empty filter: matches every link.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict to links originating from the file with this id.
    pub fn with_file_id(mut self, file_id: i64) -> Self {
        self.file_id = Some(file_id);
        self
    }

    /// Restrict to links of the given type.
    pub fn with_link_type(mut self, link_type: LinkType) -> Self {
        self.link_type = Some(link_type);
        self
    }
}

// ── Row mapping helpers ───────────────────────────────

fn file_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileEntry> {
    Ok(FileEntry {
        id: row.get(0)?,
        relative_path: row.get(1)?,
        frontmatter: row.get(2)?,
        hash: row.get(3)?,
        size_bytes: row.get(4)?,
        indexed_at: row.get(5)?,
    })
}

fn heading_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeadingEntry> {
    Ok(HeadingEntry {
        id: row.get(0)?,
        file_id: row.get(1)?,
        level: row.get::<_, i64>(2)? as u8,
        text: row.get(3)?,
        anchor: row.get(4)?,
    })
}

fn link_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkEntry> {
    let link_type = link_type_from_str(&row.get::<_, String>(2)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(LinkEntry {
        id: row.get(0)?,
        file_id: row.get(1)?,
        link_type,
        target: row.get(3)?,
        text: row.get(4)?,
    })
}

/// Escape `LIKE` wildcards (`%`, `_`) and the escape character itself.
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Escape arbitrary user input into a single FTS5 phrase query.
///
/// The result is safe to bind to a `MATCH ?` parameter for any input: the
/// user can never inject FTS5 operators, so `AND`, `OR`, `NOT`, `NEAR`, `*`,
/// `:`, `-` and friends are matched as literal text.
///
/// The transformation is:
///
/// 1. NUL (`\0`) characters are removed. SQLite refuses to store or compare
///    strings containing NUL, and an embedded NUL would truncate the term.
/// 2. Leading, trailing, and repeated whitespace is collapsed to single
///    spaces, so that the phrase's tokens line up with the caller's words and
///    no empty tokens are produced.
/// 3. Every `"` is doubled. FTS5 escapes a quote inside a string by
///    doubling it; `"` has no other meaning inside a quoted string.
/// 4. The result is wrapped in `"`. With `prefix`, a `*` is appended
///    *outside* the closing quote (`"rust"*`), which makes only the final
///    token a prefix match.
///
/// The output is a phrase, not a bareword, so it is never affected by the
/// caller's surrounding text. Input that is empty or entirely whitespace
/// yields `""`, a zero-token phrase that FTS5 accepts and that matches
/// nothing; callers that want a blank query to be a no-op should reject it up
/// front rather than rely on that — see [`Query::search`], which returns an
/// empty result for blank input.
///
/// # Examples
///
/// ```
/// # use sakuin_md::query::quote_query;
/// assert_eq!(quote_query("rust notes"), "\"rust notes\"");
/// assert_eq!(quote_query("  rust   notes "), "\"rust notes\"");
/// assert_eq!(quote_query("say \"hi\""), "\"say \"\"hi\"\"\"");
/// ```
pub fn quote_query(input: &str) -> String {
    // Remove NUL before splitting: it is not whitespace, so it would
    // otherwise survive into the middle of a token.
    let without_nul: String = input
        .chars()
        .filter(|character| *character != '\0')
        .collect();

    // `split_whitespace` both trims and collapses runs of (Unicode)
    // whitespace, which is exactly the normalisation the phrase needs.
    let normalized = without_nul
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");

    let escaped = normalized.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Escape user input into an FTS5 phrase query, optionally a prefix query.
///
/// See [`quote_query`] for the escaping rules. With `prefix`, a `*` is
/// appended outside the closing quote so that the final token matches any
/// word starting with it.
///
/// # Examples
///
/// ```
/// # use sakuin_md::query::quote_query_prefix;
/// assert_eq!(quote_query_prefix("docu", true), "\"docu\"*");
/// assert_eq!(quote_query_prefix("docu", false), "\"docu\"");
/// ```
pub fn quote_query_prefix(input: &str, prefix: bool) -> String {
    let phrase = quote_query(input);
    if prefix { format!("{phrase}*") } else { phrase }
}

// ── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Parser, extract_tags_from_frontmatter};
    use crate::store::{FileInfo, IndexStore};
    use std::error::Error;

    // ── SearchOptions ────────────────────────────────

    #[test]
    fn search_options_defaults_match_todays_behaviour() {
        let options = SearchOptions::new();
        assert_eq!(options.limit, None);
        assert_eq!(options.offset, 0);
        assert!(options.fields.is_empty());
        assert!(!options.prefix);
        assert!(!options.case_sensitive);
        assert_eq!(options.snippet_tokens, 16);
        assert_eq!(options.limit, SearchOptions::default().limit);
        assert_eq!(
            options.snippet_tokens,
            SearchOptions::default().snippet_tokens
        );
    }

    #[test]
    fn search_options_builders_set_each_field() {
        let fields = [SearchField::Path, SearchField::Title];
        let options = SearchOptions::new()
            .with_limit(5)
            .with_offset(10)
            .with_fields(&fields)
            .with_prefix(true)
            .with_case_sensitive(true)
            .with_snippet_tokens(3);
        assert_eq!(options.limit, Some(5));
        assert_eq!(options.offset, 10);
        assert_eq!(options.fields, &fields);
        assert!(options.prefix);
        assert!(options.case_sensitive);
        assert_eq!(options.snippet_tokens, 3);
    }

    #[test]
    fn search_options_builders_can_be_overridden() {
        // Builders consume and return `self`, so the last call wins.
        let options = SearchOptions::new()
            .with_prefix(true)
            .with_prefix(false)
            .with_limit(1)
            .with_limit(2);
        assert!(!options.prefix);
        assert_eq!(options.limit, Some(2));
    }

    // ── SearchError ──────────────────────────────────

    #[test]
    fn search_error_display_mentions_the_problem() {
        let invalid = SearchError::InvalidQuery("unterminated string".to_string());
        assert!(invalid.to_string().contains("invalid query"));
        assert!(invalid.to_string().contains("unterminated string"));
        assert!(invalid.source().is_none());
    }

    #[test]
    fn search_error_backend_exposes_the_rusqlite_error_as_source() {
        let error = SearchError::Backend(rusqlite::Error::InvalidColumnIndex(3));
        assert!(error.to_string().contains("search failed"));
        assert!(error.source().is_some());
    }

    #[test]
    fn search_error_from_rusqlite_classifies_syntax_errors() {
        let error: SearchError = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("fts5: syntax error near \"\"".to_string()),
        )
        .into();
        assert!(matches!(error, SearchError::InvalidQuery(_)));
    }

    #[test]
    fn search_error_from_rusqlite_keeps_backend_errors_as_backend() {
        let error: SearchError = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("no such table: files".to_string()),
        )
        .into();
        assert!(matches!(error, SearchError::Backend(_)));
    }

    // ── is_query_syntax_error ────────────────────────

    /// Feed `query` to the real FTS5 table and classify whatever comes back.
    fn classify_fts_error(store: &IndexStore, query: &str) -> bool {
        let error = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts WHERE fts MATCH ?1",
                params![query],
                |row| row.get::<_, i64>(0),
            )
            .expect_err("probe query was expected to fail");
        is_query_syntax_error(&error)
    }

    #[test]
    fn is_query_syntax_error_accepts_the_messages_fts5_actually_produces() {
        // These are the exact messages the bundled SQLite produces; see the
        // table on `is_query_syntax_error`. Re-capture them when SQLite is
        // upgraded.
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(&mut store, &[("notes/rust.md", "# Rust\n\nthe fox jumps")]);

        for query in [
            "\"",
            "\"unterminated",
            "a b\"",
            "NEAR/2",
            "a AND",
            "(a",
            "a OR",
            "*",
            "\"\"\"",
            "",
        ] {
            assert!(
                classify_fts_error(&store, query),
                "query {query:?} should be classified as a query error"
            );
        }
    }

    #[test]
    fn is_query_syntax_error_rejects_real_backend_failures() {
        let dir = tempfile::tempdir().unwrap();
        let store = IndexStore::open(&dir.path().join("test.db")).unwrap();

        // A missing table is a backend problem, not a user mistake.
        let missing_table = store
            .conn
            .query_row("SELECT COUNT(*) FROM no_such_table", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect_err("query against a missing table must fail");
        assert!(!is_query_syntax_error(&missing_table));

        // So is an unknown column filter in the query.
        let missing_column = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts WHERE fts MATCH 'nosuchcol:fox'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect_err("unknown column filter must fail");
        assert!(!is_query_syntax_error(&missing_column));

        // And so is a non-SQLite rusqlite error.
        assert!(!is_query_syntax_error(
            &rusqlite::Error::InvalidColumnIndex(3)
        ));
    }

    // ── quote_query ──────────────────────────────────

    #[test]
    fn quote_query_wraps_input_in_a_phrase() {
        assert_eq!(quote_query("rust"), "\"rust\"");
        assert_eq!(quote_query("rust notes"), "\"rust notes\"");
    }

    #[test]
    fn quote_query_trims_and_collapses_whitespace() {
        assert_eq!(quote_query("  rust  "), "\"rust\"");
        assert_eq!(quote_query("rust   notes"), "\"rust notes\"");
        assert_eq!(quote_query("\t rust \n notes \r\n"), "\"rust notes\"");
    }

    #[test]
    fn quote_query_strips_nul_bytes() {
        assert_eq!(quote_query("ru\0st"), "\"rust\"");
        assert_eq!(quote_query("\0rust\0"), "\"rust\"");
    }

    #[test]
    fn quote_query_doubles_quotes() {
        assert_eq!(quote_query("\""), "\"\"\"\"");
        assert_eq!(quote_query("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn quote_query_leaves_fts_operators_as_literal_text() {
        // Inside a phrase none of these are operators.
        for input in [
            "AND", "OR", "NOT", "NEAR", "*", "^", ":", "-", "(", ")", "rust*",
        ] {
            let quoted = quote_query(input);
            assert!(
                quoted.starts_with('"') && quoted.ends_with('"'),
                "{input:?} was not quoted: {quoted:?}"
            );
        }
        assert_eq!(quote_query("AND OR"), "\"AND OR\"");
        assert_eq!(quote_query("a*"), "\"a*\"");
    }

    #[test]
    fn quote_query_leaves_backslashes_untouched() {
        // FTS5 strings have no backslash escape; only `""` is an escape.
        assert_eq!(quote_query(r"a\b"), r#""a\b""#);
    }

    #[test]
    fn quote_query_preserves_unicode() {
        assert_eq!(quote_query("日本語 ノート"), "\"日本語 ノート\"");
        assert_eq!(quote_query("héllo wörld"), "\"héllo wörld\"");
    }

    #[test]
    fn quote_query_of_blank_input_is_an_empty_phrase() {
        // FTS5 accepts the zero-token phrase `""`; it simply matches nothing.
        // Callers still short-circuit blank input (see `Query::search`), but
        // the escaper does not need to defend against it.
        assert_eq!(quote_query(""), "\"\"");
        assert_eq!(quote_query("   \t\n"), "\"\"");
    }

    #[test]
    fn quote_query_prefix_appends_star_outside_the_quote() {
        assert_eq!(quote_query_prefix("docu", true), "\"docu\"*");
        assert_eq!(quote_query_prefix("docu", false), "\"docu\"");
        assert_eq!(quote_query_prefix("rust notes", true), "\"rust notes\"*");
    }

    #[test]
    fn quote_query_prefix_escapes_before_appending_the_star() {
        // A user-supplied `*` must not be promoted into the prefix marker.
        assert_eq!(quote_query_prefix("a*", true), "\"a*\"*");
        assert_eq!(
            quote_query_prefix("say \"hi\"", true),
            "\"say \"\"hi\"\"\"*"
        );
    }

    #[test]
    fn quote_query_output_is_accepted_by_fts5() {
        // The real check: every quoted form above must parse, whatever the
        // user typed. This runs against the bundled SQLite via the store.
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(&mut store, &[("notes/rust.md", "# Rust\n\nthe fox jumps")]);

        // Blank input is deliberately absent: `Query::search` returns an empty
        // result for it before any SQL is built, so it never reaches FTS5.
        for input in [
            "\"",
            "NEAR",
            "rust*",
            r"a\b",
            "rust OR fox",
            "日本語",
            "\"\"\"",
        ] {
            let phrase = quote_query(input);
            match store.conn.query_row(
                "SELECT COUNT(*) FROM fts WHERE fts MATCH ?1",
                params![&phrase],
                |row| row.get::<_, i64>(0),
            ) {
                Ok(count) => assert!(count >= 0),
                Err(error) => panic!("MATCH {phrase:?} (from {input:?}) failed: {error}"),
            }
        }
    }

    /// Write and index a set of markdown files into a fresh store.
    fn index_workspace(store: &mut IndexStore, files: &[(&str, &str)]) {
        let parser = Parser::new();
        for (name, content) in files {
            let result = parser.parse(content);
            let absolute_path = format!("/virtual/{name}");
            let file_info = FileInfo::from_content(
                name.to_string(),
                absolute_path,
                content.as_bytes(),
                result.frontmatter.clone(),
            );
            let tags = extract_tags_from_frontmatter(result.frontmatter.as_deref());
            let title = result
                .headings
                .first()
                .map(|heading| heading.text.as_str())
                .unwrap_or(name);
            store
                .index_file(
                    &file_info,
                    &result.headings,
                    &result.links,
                    &tags,
                    title,
                    &result.body,
                )
                .unwrap();
        }
    }

    /// Build a store pre-populated with a small multi-file workspace.
    fn workspace() -> (tempfile::TempDir, IndexStore) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();
        index_workspace(
            &mut store,
            &[
                ("index.md", "# Home\n\nWelcome to [[docs]]."),
                (
                    "docs.md",
                    "---\ntitle: Docs\ntags: [documentation]\n---\n\n# Docs\n\nSee [[index]] for home.",
                ),
                (
                    "about.md",
                    "---\ntags: meta\n---\n\n# About\n\nNo outgoing links.",
                ),
                (
                    "draft.md",
                    "---\ntags: [wip, draft]\n---\n\n# Draft\n\nUnfinished.",
                ),
                (
                    "notes/rust.md",
                    "---\ntags: [programming, rust]\n---\n\n# Rust Notes\n\nThe quick brown fox jumps over the lazy dog.",
                ),
            ],
        );
        (dir, store)
    }

    // ── Constructor ────────────────────────────────────

    #[test]
    fn query_from_store_and_connection_agree() {
        let (_dir, store) = workspace();
        let query_from_store = store.query();
        let query_from_connection = Query::new(&store.conn);

        assert_eq!(
            query_from_store.files(None).unwrap(),
            query_from_connection.files(None).unwrap()
        );
    }

    // ── files ──────────────────────────────────────────

    #[test]
    fn files_returns_all_sorted_by_path() {
        let (_dir, store) = workspace();
        let query = store.query();

        let files = query.files(None).unwrap();
        assert_eq!(files.len(), 5);
        let paths: Vec<&str> = files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "about.md",
                "docs.md",
                "draft.md",
                "index.md",
                "notes/rust.md"
            ]
        );
    }

    #[test]
    fn files_filters_by_glob_pattern() {
        let (_dir, store) = workspace();
        let query = store.query();

        // `*` crosses path separators, so `*.md` matches every file.
        assert_eq!(query.files(Some("*.md")).unwrap().len(), 5);

        let files = query.files(Some("notes/*.md")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "notes/rust.md");

        assert!(query.files(Some("no-such-file-*")).unwrap().is_empty());
    }

    #[test]
    fn files_entry_fields() {
        let (_dir, store) = workspace();
        let query = store.query();

        let files = query.files(Some("docs.md")).unwrap();
        assert_eq!(files.len(), 1);
        let entry = &files[0];
        assert!(entry.id > 0);
        assert_eq!(entry.relative_path, "docs.md");
        assert_eq!(
            entry.frontmatter.as_deref(),
            Some("title: Docs\ntags: [documentation]")
        );
        assert!(!entry.hash.is_empty());
        assert!(entry.size_bytes > 0);
        assert!(!entry.indexed_at.is_empty());
    }

    // ── files_with_frontmatter ─────────────────────────

    #[test]
    fn files_with_frontmatter_matches_key_value() {
        let (_dir, store) = workspace();
        let query = store.query();

        let files = query.files_with_frontmatter("title", "Docs").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "docs.md");

        assert!(
            query
                .files_with_frontmatter("title", "Missing")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn files_with_frontmatter_excludes_files_without_it() {
        let (_dir, store) = workspace();
        let query = store.query();

        // `index.md` has no frontmatter; only `about.md` has `tags: [meta]`.
        let files = query.files_with_frontmatter("tags", "meta").unwrap();
        let paths: Vec<&str> = files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect();
        assert_eq!(paths, ["about.md"]);
    }

    // ── files_by_tag ───────────────────────────────────

    #[test]
    fn files_by_tag_returns_matching_files() {
        let (_dir, store) = workspace();
        let query = store.query();

        let files = query.files_by_tag("documentation").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "docs.md");

        let files = query.files_by_tag("rust").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "notes/rust.md");

        assert!(query.files_by_tag("missing-tag").unwrap().is_empty());
    }

    // ── headings ───────────────────────────────────────

    #[test]
    fn headings_for_a_single_file() {
        let (_dir, store) = workspace();
        let query = store.query();
        let file_id = store.get_file_id("docs.md").unwrap().unwrap();

        let headings = query.headings(Some(file_id)).unwrap();
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].file_id, file_id);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "Docs");
        assert_eq!(headings[0].anchor, "docs");
    }

    #[test]
    fn headings_across_all_files() {
        let (_dir, store) = workspace();
        let query = store.query();

        let headings = query.headings(None).unwrap();
        assert_eq!(headings.len(), 5); // one heading per workspace file
    }

    #[test]
    fn headings_for_unknown_file_is_empty() {
        let (_dir, store) = workspace();
        let query = store.query();

        assert!(query.headings(Some(99_999)).unwrap().is_empty());
    }

    // ── toc ────────────────────────────────────────────

    #[test]
    fn toc_returns_headings_in_document_order() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();
        index_workspace(
            &mut store,
            &[(
                "guide.md",
                "# Introduction\n\nintro text\n\n## Setup\n\nsetup text\n\n### Details\n\nmore text\n\n## Conclusion\n\nend text",
            )],
        );
        let query = store.query();
        let file_id = store.get_file_id("guide.md").unwrap().unwrap();

        let toc = query.toc(file_id).unwrap();
        let texts: Vec<&str> = toc.iter().map(|heading| heading.text.as_str()).collect();
        let levels: Vec<u8> = toc.iter().map(|heading| heading.level).collect();
        assert_eq!(texts, ["Introduction", "Setup", "Details", "Conclusion"]);
        assert_eq!(levels, [1, 2, 3, 2]);
    }

    // ── links ──────────────────────────────────────────

    #[test]
    fn links_without_filter_returns_everything() {
        let (_dir, store) = workspace();
        let query = store.query();

        let links = query.links(LinkFilter::new()).unwrap();
        // index.md -> docs (wikilink); docs.md -> index (wikilink).
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn links_filtered_by_file() {
        let (_dir, store) = workspace();
        let query = store.query();
        let file_id = store.get_file_id("index.md").unwrap().unwrap();

        let links = query
            .links(LinkFilter::new().with_file_id(file_id))
            .unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].file_id, file_id);
        assert_eq!(links[0].link_type, LinkType::Wikilink);
        assert_eq!(links[0].target, "docs");
        assert_eq!(links[0].text.as_deref(), Some("docs"));
    }

    #[test]
    fn links_filtered_by_type() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();
        index_workspace(
            &mut store,
            &[(
                "mixed.md",
                "A [site](https://example.com) and [[wiki-page]] and ![img](pic.png).",
            )],
        );
        let query = store.query();

        let wikilinks = query
            .links(LinkFilter::new().with_link_type(LinkType::Wikilink))
            .unwrap();
        assert_eq!(wikilinks.len(), 1);
        assert_eq!(wikilinks[0].target, "wiki-page");

        let inlines = query
            .links(LinkFilter::new().with_link_type(LinkType::Inline))
            .unwrap();
        assert_eq!(inlines.len(), 1);
        assert_eq!(inlines[0].target, "https://example.com");

        let images = query
            .links(LinkFilter::new().with_link_type(LinkType::Image))
            .unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].target, "pic.png");

        // Combined file + type filter.
        let file_id = store.get_file_id("mixed.md").unwrap().unwrap();
        let combined = query
            .links(
                LinkFilter::new()
                    .with_file_id(file_id)
                    .with_link_type(LinkType::Inline),
            )
            .unwrap();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].target, "https://example.com");
    }

    // ── backlinks ──────────────────────────────────────

    #[test]
    fn backlinks_find_files_referencing_a_target() {
        let (_dir, store) = workspace();
        let query = store.query();

        // docs.md links to [[index]].
        let backlinks = query.backlinks("index").unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].target, "index");
        assert_eq!(backlinks[0].link_type, LinkType::Wikilink);
        let docs_file_id = store.get_file_id("docs.md").unwrap().unwrap();
        assert_eq!(backlinks[0].file_id, docs_file_id);

        // index.md links to [[docs]].
        let backlinks = query.backlinks("docs").unwrap();
        assert_eq!(backlinks.len(), 1);

        assert!(query.backlinks("nobody-links-here").unwrap().is_empty());
    }

    // ── search ─────────────────────────────────────────

    #[test]
    fn search_returns_ranked_results() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();
        index_workspace(
            &mut store,
            &[
                (
                    "fox-heavy.md",
                    "# Fox Notes\n\nfox fox fox fox — many foxes here.",
                ),
                ("fox-light.md", "# Light\n\nThe fox is a small animal."),
                ("unrelated.md", "# Unrelated\n\nNothing about animals here."),
            ],
        );
        let query = store.query();

        let results = query.search("fox").unwrap();
        assert_eq!(results.len(), 2);
        // The file with more occurrences ranks first (lower BM25 score).
        assert_eq!(results[0].relative_path, "fox-heavy.md");
        assert_eq!(results[1].relative_path, "fox-light.md");

        // BM25 scores are ordered ascending; every snippet mentions the term.
        assert!(results[0].rank <= results[1].rank);
        for result in &results {
            assert!(result.snippet.contains("fox"));
        }
    }

    #[test]
    fn search_title_falls_back_to_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();
        index_workspace(
            &mut store,
            &[("plain.md", "Just some zebra text, nothing more.")],
        );
        let file_id = store.get_file_id("plain.md").unwrap().unwrap();
        // Simulate a caller that stored no title (e.g. a file without headings).
        store
            .set_fts(file_id, "", "Just some zebra text, nothing more.")
            .unwrap();
        let query = store.query();

        let results = query.search("zebra").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative_path, "plain.md");
        assert_eq!(results[0].title, "plain.md"); // empty title -> path fallback
    }

    #[test]
    fn search_with_no_matches_returns_empty() {
        let (_dir, store) = workspace();
        let query = store.query();

        assert!(query.search("zebra").unwrap().is_empty());
    }

    #[test]
    fn search_with_empty_query_returns_empty() {
        let (_dir, store) = workspace();
        let query = store.query();

        assert!(query.search("").unwrap().is_empty());
        assert!(query.search("   ").unwrap().is_empty());
    }

    #[test]
    fn search_with_invalid_syntax_returns_error() {
        let (_dir, store) = workspace();
        let query = store.query();

        // An unbalanced quote is a syntax error in FTS5.
        assert!(query.search("\"unterminated").is_err());
    }
}
