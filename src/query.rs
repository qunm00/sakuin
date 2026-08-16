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
                    snippet(fts, 2, '<b>', '</b>', '...', 16),
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

// ── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Parser, extract_tags_from_frontmatter};
    use crate::store::{FileInfo, IndexStore};

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
