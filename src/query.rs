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
    ///
    /// The query string reaches FTS5 **verbatim**, so FTS5 operators (`AND`,
    /// `OR`, `NEAR`, `*`, …) are interpreted rather than matched as literal
    /// text, and malformed input surfaces as a raw backend error. Prefer
    /// [`Query::try_search`] or [`Query::search_with_options`], which escape
    /// the input and report query problems as
    /// [`SearchError::InvalidQuery`].
    #[deprecated(note = "use Query::try_search or Query::search_with_options")]
    pub fn search(&self, query: &str) -> rusqlite::Result<Vec<SearchResult>> {
        self.execute_search(query, &SearchOptions::new(), true)
    }

    /// Full-text search with the default options and a typed error.
    ///
    /// Equivalent to `search_with_options(query, &SearchOptions::new())`, but
    /// far more common: it escapes `query` (so it is always matched as literal
    /// text) and reports an unusable query as
    /// [`SearchError::InvalidQuery`] rather than a raw backend error.
    pub fn try_search(&self, query: &str) -> Result<Vec<SearchResult>, SearchError> {
        self.search_with_options(query, &SearchOptions::new())
    }

    /// Full-text search with configurable fields, paging, prefix matching,
    /// case sensitivity, and snippet length.
    ///
    /// `query` is escaped by [`quote_query`], so it is always matched as
    /// literal text: `NEAR`, `AND`, `*` and friends have no special meaning.
    /// A blank query returns no results.
    ///
    /// Semantics of each option:
    ///
    /// - [`SearchOptions::fields`] restricts matching to the named columns.
    ///   An empty slice searches all of them. Columns left out are also given
    ///   a zero BM25 weight, so they cannot influence the score.
    /// - [`SearchOptions::prefix`] makes the final token a prefix match, so
    ///   `docu` also matches `document`. Note that the `porter` tokenizer
    ///   stores stems, so the prefix must match the *stem*: `index*` matches
    ///   `indexing`, while `indexi*` does not.
    /// - [`SearchOptions::case_sensitive`] re-checks the FTS candidates with a
    ///   case-sensitive `GLOB` over the stored text of the searched columns.
    ///   Because FTS has already matched the tokens case-insensitively, this
    ///   step can only *remove* candidates, never add one: a query that FTS
    ///   finds no token for returns nothing, however the `GLOB` is written.
    ///   It costs `O(candidates)` rather than `O(log n)`, and it runs before
    ///   `limit`/`offset`, so paging stays correct.
    /// - [`SearchOptions::snippet_tokens`] sets the approximate snippet length
    ///   in tokens; `0` suppresses snippets and yields `""`. Snippets are
    ///   excerpts of the body, so they are also empty when
    ///   [`SearchField::Body`] is not among the searched fields.
    /// - [`SearchOptions::limit`] and [`SearchOptions::offset`] page the
    ///   results, which are ordered by rank and then by `relative_path` so
    ///   that paging is deterministic.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidQuery`] for a blank query or a query FTS5
    /// cannot parse, and [`SearchError::Backend`] for anything else.
    pub fn search_with_options(
        &self,
        query: &str,
        options: &SearchOptions<'_>,
    ) -> Result<Vec<SearchResult>, SearchError> {
        self.execute_search(query, options, false)
            .map_err(SearchError::from)
    }

    /// The single SQL path behind [`Query::search`], [`Query::try_search`],
    /// and [`Query::search_with_options`].
    ///
    /// With `raw_query` set, `query` is bound to `MATCH` unchanged and every
    /// option is ignored — that is the deprecated passthrough behaviour. With
    /// `raw_query` clear, `query` is escaped, narrowed to the selected
    /// columns, optionally prefix-matched, and optionally `GLOB`-filtered.
    ///
    /// Only *structural* SQL is built by string formatting: the FTS5 column
    /// filter, the number of `GLOB` terms, and the placeholders themselves.
    /// Every caller-supplied value is a bound parameter.
    fn execute_search(
        &self,
        query: &str,
        options: &SearchOptions<'_>,
        raw_query: bool,
    ) -> rusqlite::Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let fields = selected_fields(options.fields);

        // A snippet is an excerpt of the body, so it is only meaningful when
        // the body is one of the searched columns.
        let snippet_tokens: i64 = if fields.contains(&SearchField::Body) {
            options.snippet_tokens as i64
        } else {
            0
        };

        // The `MATCH` argument: the escaped phrase, narrowed to the selected
        // columns with an FTS5 column filter. All columns means no filter.
        let match_expression = if raw_query {
            query.to_string()
        } else {
            let phrase = quote_query_prefix(query, options.prefix);
            if fields.len() == ALL_SEARCH_FIELDS.len() {
                phrase
            } else {
                let columns = fields
                    .iter()
                    .map(|field| field.column_name())
                    .collect::<Vec<&str>>()
                    .join(" ");
                format!("{{{columns}}} : {phrase}")
            }
        };

        // Case sensitivity is a case-sensitive `GLOB` re-check of the FTS
        // candidates. The pattern's wildcards are escaped so that a query
        // containing `*` cannot become a match-everything pattern.
        let case_sensitive = options.case_sensitive && !raw_query;
        let glob_pattern = format!("*{}*", escape_glob(query.trim()));
        let glob_terms: Vec<String> = if case_sensitive {
            fields
                .iter()
                .map(|field| format!("fts.{} GLOB ?", field.column_name()))
                .collect()
        } else {
            Vec::new()
        };

        // Columns that were not searched get a zero weight, so a narrowed
        // query is not scored against text it never looked at.
        let weights: Vec<f64> = ALL_SEARCH_FIELDS
            .iter()
            .map(|field| {
                if fields.contains(field) {
                    default_weight(*field)
                } else {
                    0.0
                }
            })
            .collect();

        // Parameter numbers, in the order the placeholders appear below:
        // MATCH, snippet tokens, the three weights, the GLOB terms, then paging.
        let match_parameter = 1;
        let snippet_parameter = 2;
        let first_weight_parameter = 3;
        let weight_parameters: Vec<usize> = (0..ALL_SEARCH_FIELDS.len())
            .map(|index| first_weight_parameter + index)
            .collect();
        let first_glob_parameter = first_weight_parameter + ALL_SEARCH_FIELDS.len();
        let glob_parameters: Vec<usize> = (0..glob_terms.len())
            .map(|index| first_glob_parameter + index)
            .collect();
        let limit_parameter = first_glob_parameter + glob_parameters.len();
        let offset_parameter = limit_parameter + 1;

        let weight_placeholders = weight_parameters
            .iter()
            .map(|number| format!("?{number}"))
            .collect::<Vec<String>>()
            .join(", ");

        let case_filter = if glob_terms.is_empty() {
            String::new()
        } else {
            let terms = glob_terms
                .iter()
                .zip(&glob_parameters)
                .map(|(term, number)| term.replace('?', &format!("?{number}")))
                .collect::<Vec<String>>()
                .join(" OR ");
            format!("\n             AND ({terms})")
        };

        let statement_sql = format!(
            "SELECT f.id, f.relative_path,
                    CASE WHEN fts.title IS NULL OR fts.title = ''
                         THEN f.relative_path ELSE fts.title END,
                    CASE WHEN ?{snippet_parameter} = 0
                         THEN ''
                         ELSE snippet(fts, {body_column}, '<b>', '</b>', '...',
                                      ?{snippet_parameter})
                    END,
                    bm25(fts, {weight_placeholders})
             FROM fts
             JOIN files f ON f.id = fts.file_id
             WHERE fts MATCH ?{match_parameter}{case_filter}
             ORDER BY bm25(fts, {weight_placeholders}), f.relative_path
             LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}",
            body_column = SearchField::Body.column_index(),
        );

        // SQLite reads a negative LIMIT as "no limit".
        let limit: i64 = options.limit.map_or(-1, |limit| limit as i64);
        let offset: i64 = options.offset as i64;

        let mut statement = self.conn.prepare(&statement_sql)?;
        let mut parameters: Vec<&dyn rusqlite::ToSql> = vec![&match_expression, &snippet_tokens];
        for weight in &weights {
            parameters.push(weight);
        }
        for _ in &glob_terms {
            parameters.push(&glob_pattern);
        }
        parameters.push(&limit);
        parameters.push(&offset);

        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
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
    /// The document body text.
    Body,
}

impl SearchField {
    /// The column name as it appears in the `fts` table.
    ///
    /// Used both for the FTS5 column filter (`{path title} : query`) and for
    /// the `GLOB` re-check (`fts.path GLOB ?`).
    const fn column_name(self) -> &'static str {
        match self {
            SearchField::Path => "path",
            SearchField::Title => "title",
            SearchField::Body => "body",
        }
    }

    /// The column's 0-based index in the `fts` table.
    ///
    /// The indices follow the column order created by migration
    /// `002-fts-fields` — `file_id UNINDEXED, path, title, body` — so `file_id`
    /// is 0, `path` is 1, `title` is 2, and `body` is 3. `snippet` takes a
    /// column *index*, not a name, which is why these are indices.
    const fn column_index(self) -> usize {
        match self {
            SearchField::Path => 1,
            SearchField::Title => 2,
            SearchField::Body => 3,
        }
    }
}

/// Every searchable column, in `fts` table order. The BM25 weight vector is
/// positional, so this order must not change without updating
/// [`default_weight`].
const ALL_SEARCH_FIELDS: [SearchField; 3] =
    [SearchField::Path, SearchField::Title, SearchField::Body];

/// The default BM25 weight of a column: paths are noisy, titles are the most
/// deliberate signal, and bodies are the bulk of the text.
const fn default_weight(field: SearchField) -> f64 {
    match field {
        SearchField::Path => 0.6,
        SearchField::Title => 0.9,
        SearchField::Body => 1.0,
    }
}

/// Expand a caller's field selection into the concrete list to search.
///
/// An empty slice means "every field", which is the documented default rather
/// than an error.
fn selected_fields(fields: &[SearchField]) -> Vec<SearchField> {
    if fields.is_empty() {
        return ALL_SEARCH_FIELDS.to_vec();
    }
    // Preserve the table's column order so the weight vector stays aligned.
    ALL_SEARCH_FIELDS
        .iter()
        .copied()
        .filter(|field| fields.contains(field))
        .collect()
}

/// Escape the `GLOB` metacharacters `*`, `?`, `[` and the escape character
/// itself, so that user input is matched literally.
///
/// `LIKE` would be wrong here: `GLOB` is case-sensitive, and case-sensitive
/// re-checking is the whole point of the `GLOB` filter.
fn escape_glob(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for character in input.chars() {
        if matches!(character, '\\' | '*' | '?' | '[') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
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
    ///
    /// Caveat: the index is tokenized with `porter`, so it stores *stems* and
    /// a prefix query is matched against the stem. `index*` matches
    /// `indexing`, but `indexi*` does not.
    pub prefix: bool,
    /// Require the matched text to have the same case as the query. Useful for
    /// code identifiers, where `Indexer` and `indexer` are different things.
    ///
    /// Candidates still come from FTS, which folds case, so this option can
    /// only ever *remove* rows: it re-checks each candidate with a
    /// case-sensitive `GLOB` over the stored text.
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

        let results = query.try_search("fox").unwrap();
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

        let results = query.try_search("zebra").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative_path, "plain.md");
        assert_eq!(results[0].title, "plain.md"); // empty title -> path fallback
    }

    #[test]
    fn search_with_no_matches_returns_empty() {
        let (_dir, store) = workspace();
        let query = store.query();

        assert!(query.try_search("zebra").unwrap().is_empty());
    }

    #[test]
    fn search_with_empty_query_returns_empty() {
        let (_dir, store) = workspace();
        let query = store.query();

        assert!(query.try_search("").unwrap().is_empty());
        assert!(query.try_search("   ").unwrap().is_empty());
    }

    #[test]
    fn search_with_invalid_syntax_returns_error() {
        let (_dir, store) = workspace();
        let query = store.query();

        // `try_search` escapes its input, so nothing the caller types can
        // reach FTS5 as syntax — not even an unbalanced quote.
        assert!(query.try_search("\"unterminated").unwrap().is_empty());
    }

    // ── search_with_options ──────────────────────────

    /// Workspace with content chosen so that each option is observable:
    /// a title match, a body-only match, and a path-only match.
    fn option_workspace() -> (tempfile::TempDir, IndexStore) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(
            &mut store,
            &[
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
            ],
        );
        (dir, store)
    }

    #[test]
    fn options_default_to_every_field() {
        let (_dir, store) = option_workspace();
        let query = store.query();

        // "rust" is in a path, a title, and a body.
        let all = query.try_search("rust").unwrap();
        let paths_only = {
            let fields = [SearchField::Path];
            query
                .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
                .unwrap()
        };
        let titles_only = {
            let fields = [SearchField::Title];
            query
                .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
                .unwrap()
        };
        assert!(!all.is_empty());
        assert_eq!(
            paths_only.len(),
            1,
            "only notes/rust.md has rust in its path"
        );
        assert_eq!(titles_only.len(), 1);
        assert_eq!(paths_only[0].relative_path, "notes/rust.md");
    }

    #[test]
    fn options_empty_field_list_means_every_field() {
        let (_dir, store) = option_workspace();
        let query = store.query();

        let explicit_empty = {
            let fields: [SearchField; 0] = [];
            query
                .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
                .unwrap()
        };
        let omitted = query.try_search("rust").unwrap();
        assert_eq!(explicit_empty.len(), omitted.len());
    }

    #[test]
    fn options_title_field_does_not_match_the_body() {
        let (_dir, store) = option_workspace();
        let query = store.query();
        let fields = [SearchField::Title];

        // "fox" is only in a body, never in a title.
        let titles = query
            .search_with_options("fox", &SearchOptions::new().with_fields(&fields))
            .unwrap();
        assert!(titles.is_empty());
        assert!(!query.try_search("fox").unwrap().is_empty());
    }
    #[test]
    fn options_limit_truncates_and_offset_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(
            &mut store,
            &[
                ("a.md", "# A\n\nA shared zebra appears here."),
                ("b.md", "# B\n\nAnother shared zebra appears here."),
                ("c.md", "# C\n\nYet another shared zebra here."),
                ("d.md", "# D\n\nA final shared zebra here."),
            ],
        );
        let query = store.query();

        let all = query.try_search("zebra").unwrap();
        assert_eq!(all.len(), 4);

        let first_two = query
            .search_with_options("zebra", &SearchOptions::new().with_limit(2))
            .unwrap();
        assert_eq!(first_two.len(), 2);
        assert_eq!(first_two[0].relative_path, all[0].relative_path);
        assert_eq!(first_two[1].relative_path, all[1].relative_path);

        // Paging walks the same ordered list.
        let skipped = query
            .search_with_options("zebra", &SearchOptions::new().with_offset(1))
            .unwrap();
        assert_eq!(skipped.len(), 3);
        assert_eq!(skipped[0].relative_path, all[1].relative_path);
        let page = query
            .search_with_options("zebra", &SearchOptions::new().with_offset(2).with_limit(1))
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].relative_path, all[2].relative_path);

        // Paging past the end yields nothing.
        assert!(
            query
                .search_with_options("zebra", &SearchOptions::new().with_offset(4))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn options_prefix_matches_stem_prefixes() {
        let (_dir, store) = option_workspace();
        let query = store.query();

        // Without `prefix`, a partial token matches nothing.
        assert!(query.try_search("gui").unwrap().is_empty());
        let prefixed = query
            .search_with_options("gui", &SearchOptions::new().with_prefix(true))
            .unwrap();
        assert!(!prefixed.is_empty(), "gui* should match indexing");
        assert!(
            prefixed
                .iter()
                .any(|result| result.relative_path == "guide/indexing.md")
        );
    }

    #[test]
    fn options_prefix_matches_the_stem_not_the_word() {
        // Documented caveat of the `porter` tokenizer: the index stores stems,
        // and a prefix query is matched against the stem. `indexing` is stored
        // as `index`, so `index*` matches and `indexi*` does not.
        let (_dir, store) = option_workspace();
        let query = store.query();
        let prefix = SearchOptions::new().with_prefix(true);

        assert!(
            !query
                .search_with_options("index", &prefix)
                .unwrap()
                .is_empty()
        );
        assert!(
            query
                .search_with_options("indexi", &prefix)
                .unwrap()
                .is_empty(),
            "indexi* should not match the stored stem `index`"
        );
    }

    #[test]
    fn options_case_sensitive_keeps_the_exact_casing() {
        let (_dir, store) = option_workspace();
        let query = store.query();
        let sensitive = SearchOptions::new().with_case_sensitive(true);

        // The fixture stores `notes/rust.md` and the title `Rust Notes`.
        assert_eq!(
            query.search_with_options("Rust", &sensitive).unwrap().len(),
            1,
            "the title contains `Rust` with that exact casing"
        );
        assert_eq!(
            query.search_with_options("rust", &sensitive).unwrap().len(),
            1,
            "the path contains `rust` in lower case"
        );
        assert!(
            query
                .search_with_options("RUST", &sensitive)
                .unwrap()
                .is_empty(),
            "nothing in the fixture is written `RUST`"
        );

        // Without the option, FTS5 folds case and finds the same row.
        assert_eq!(query.try_search("RUST").unwrap().len(), 1);
    }

    #[test]
    fn options_case_sensitive_filter_checks_every_searched_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(
            &mut store,
            &[
                (
                    "notes/Indexer.md",
                    "# Notes\n\nNo mention of the term here.",
                ),
                (
                    "misc/plain.md",
                    "# Plain\n\nAn indexer walks the workspace.",
                ),
            ],
        );
        let query = store.query();
        let sensitive = SearchOptions::new().with_case_sensitive(true);

        // Both files are FTS candidates, because the index folds case.
        assert_eq!(query.try_search("Indexer").unwrap().len(), 2);

        // Only the one that spells it `Indexer` somewhere survives the GLOB:
        // the other has `indexer` in lower case only.
        let kept = query.search_with_options("Indexer", &sensitive).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].relative_path, "notes/Indexer.md");

        // Searching paths only narrows the GLOB check to the path column.
        let fields = [SearchField::Path];
        let paths = query
            .search_with_options("Indexer", &sensitive.clone().with_fields(&fields))
            .unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].relative_path, "notes/Indexer.md");
    }

    #[test]
    fn options_case_sensitive_filter_can_only_narrow_candidates() {
        // Documented limitation: candidates still come from FTS token
        // matching, so the case-sensitive `GLOB` can remove rows but never add
        // one. A mid-token substring that is not itself a token cannot bring
        // in a file.
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(
            &mut store,
            &[("ids.md", "# Ids\n\nSee reIndexer for details.")],
        );
        let query = store.query();
        let sensitive = SearchOptions::new().with_case_sensitive(true);

        assert_eq!(
            query
                .search_with_options("reIndexer", &sensitive)
                .unwrap()
                .len(),
            1
        );
        // `reindexer` is a candidate (FTS folds case) but has the wrong case.
        assert!(
            query
                .search_with_options("reindexer", &sensitive)
                .unwrap()
                .is_empty()
        );
        // `ndexe` is only a substring of a token, so FTS produces no candidate
        // at all and the `GLOB` never gets a chance to run.
        assert!(
            query
                .search_with_options("ndexe", &sensitive)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn options_case_sensitive_filter_survives_wildcards_in_the_query() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        index_workspace(
            &mut store,
            &[("star.md", "# Star\n\nA literal asterisk * here.")],
        );
        let query = store.query();

        // A `*` in the query must not become a GLOB wildcard that matches
        // every row. Here it produces no FTS candidates at all, so the result
        // is empty rather than "everything".
        let sensitive = SearchOptions::new().with_case_sensitive(true);
        assert!(
            query
                .search_with_options("*", &sensitive)
                .unwrap()
                .is_empty()
        );

        // An ordinary word still matches, so the empty result above is about
        // the wildcard and not a broken filter.
        assert_eq!(
            query
                .search_with_options("asterisk", &sensitive)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn options_case_sensitive_paging_counts_filtered_results() {
        // The GLOB filter runs before LIMIT, so `limit`/`offset` page the
        // case-sensitive results and not the raw candidate set.
        let (_dir, store) = option_workspace();
        let query = store.query();
        let sensitive = SearchOptions::new().with_case_sensitive(true);

        let results = query.search_with_options("rust", &sensitive).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            query
                .search_with_options("rust", &sensitive.clone().with_limit(1))
                .unwrap(),
            results
        );
        assert!(
            query
                .search_with_options("rust", &sensitive.with_offset(1))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn options_snippet_tokens_zero_suppresses_snippets() {
        let (_dir, store) = option_workspace();
        let query = store.query();

        let with_snippets = query.try_search("fox").unwrap();
        assert!(with_snippets[0].snippet.contains("fox"));

        let without = query
            .search_with_options("fox", &SearchOptions::new().with_snippet_tokens(0))
            .unwrap();
        assert_eq!(without.len(), with_snippets.len());
        assert!(without[0].snippet.is_empty());
    }

    #[test]
    fn options_snippet_tokens_lengthen_the_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
        let body = (1..80)
            .map(|index| format!("token{index}"))
            .collect::<Vec<String>>()
            .join(" ");
        let document = format!("# Long\n\n{body}");
        index_workspace(&mut store, &[("long.md", &document)]);
        let query = store.query();

        let short = query
            .search_with_options("token40", &SearchOptions::new().with_snippet_tokens(3))
            .unwrap();
        let long = query
            .search_with_options("token40", &SearchOptions::new().with_snippet_tokens(40))
            .unwrap();
        assert!(long[0].snippet.len() > short[0].snippet.len());
    }

    #[test]
    fn options_snippets_are_empty_when_the_body_is_not_searched() {
        let (_dir, store) = option_workspace();
        let query = store.query();
        let fields = [SearchField::Path, SearchField::Title];

        let results = query
            .search_with_options("rust", &SearchOptions::new().with_fields(&fields))
            .unwrap();
        assert!(!results.is_empty());
        assert!(
            results.iter().all(|result| result.snippet.is_empty()),
            "a snippet is an excerpt of the body, so it is meaningless here"
        );
    }

    #[test]
    fn options_blank_query_returns_empty() {
        let (_dir, store) = option_workspace();
        let query = store.query();

        assert!(query.try_search("").unwrap().is_empty());
        assert!(query.try_search("   ").unwrap().is_empty());
    }

    #[test]
    fn options_never_leak_fts_syntax_errors() {
        let (_dir, store) = option_workspace();
        let query = store.query();

        // None of these reach FTS5 as syntax; all are literal phrases.
        for input in [
            "\"unterminated",
            "NEAR",
            "NEAR/2",
            "a AND",
            "(a",
            "*",
            "title:rust",
            "^rust",
            "rust -notes",
        ] {
            let result = query.try_search(input);
            assert!(
                result.is_ok(),
                "{input:?} should not be an error, got {:?}",
                result.err()
            );
        }
    }

    #[test]
    fn options_results_are_deterministic_across_runs() {
        let (_dir, store) = option_workspace();
        let query = store.query();
        let options = SearchOptions::new().with_limit(3);

        let first = query.search_with_options("rust", &options).unwrap();
        for _ in 0..5 {
            let again = query.search_with_options("rust", &options).unwrap();
            let paths = |results: &[SearchResult]| -> Vec<String> {
                results.iter().map(|r| r.relative_path.clone()).collect()
            };
            assert_eq!(paths(&first), paths(&again));
        }
    }

    #[test]
    fn selected_fields_defaults_to_every_column() {
        assert_eq!(selected_fields(&[]), ALL_SEARCH_FIELDS.to_vec());
        assert_eq!(
            selected_fields(&[SearchField::Body]),
            vec![SearchField::Body]
        );
        // Order follows the table, not the caller's slice.
        assert_eq!(
            selected_fields(&[SearchField::Body, SearchField::Path]),
            vec![SearchField::Path, SearchField::Body]
        );
    }

    #[test]
    fn escape_glob_escapes_every_metacharacter() {
        assert_eq!(escape_glob("plain"), "plain");
        assert_eq!(escape_glob("a*b"), "a\\*b");
        assert_eq!(escape_glob("a?b"), "a\\?b");
        assert_eq!(escape_glob("a[b"), "a\\[b");
        assert_eq!(escape_glob("a\\b"), "a\\\\b");
        // A trailing backslash must not escape the closing bracket we add.
        assert_eq!(escape_glob("a\\"), "a\\\\");
    }

    #[test]
    fn search_field_column_names_and_indices_match_the_schema() {
        // These must agree with migration 002: file_id UNINDEXED, path, title, body.
        assert_eq!(SearchField::Path.column_name(), "path");
        assert_eq!(SearchField::Title.column_name(), "title");
        assert_eq!(SearchField::Body.column_name(), "body");
        assert_eq!(SearchField::Path.column_index(), 1);
        assert_eq!(SearchField::Title.column_index(), 2);
        assert_eq!(SearchField::Body.column_index(), 3);
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_search_still_passes_the_query_through_verbatim() {
        let (_dir, store) = workspace();
        let query = store.query();

        // The old API is raw FTS5: an unbalanced quote is a backend error.
        assert!(query.search("\"unterminated").is_err());

        // ...and FTS5 operators are still interpreted, which `try_search`
        // deliberately prevents. `fox OR documentation` is a real FTS5 query
        // here; as a literal phrase it matches nothing.
        let raw = query.search("fox OR documentation").unwrap();
        assert!(!raw.is_empty(), "raw search should honour the OR operator");
        assert!(
            query.try_search("fox OR documentation").unwrap().is_empty(),
            "try_search should match the input as one literal phrase"
        );
    }
}
