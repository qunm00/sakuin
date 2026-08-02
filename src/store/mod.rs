/// SQLite-backed persistence for the index.
///
/// The `IndexStore` manages all CRUD operations against the SQLite database,
/// including schema creation, file upsert/delete, and replacement of child
/// rows (headings, links, tags, FTS entries) for a given file.
mod helpers;
mod migration;

use migration::new_migrations;
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::Path;

use crate::parser::{Heading, Link, LinkType};

// ── FileInfo ─────────────────────────────────────────

/// Metadata about an indexed file.
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub relative_path: String,
    pub absolute_path: String,
    pub hash: String,
    pub frontmatter: Option<String>,
    pub size_bytes: u64,
    pub modified_at: String,
}

impl FileInfo {
    /// Build a `FileInfo` from file metadata.
    ///
    /// Hashes the full byte content, reads the file's modification time
    /// from the filesystem (falling back to the current time), and records
    /// the byte size.
    pub fn from_content(
        relative_path: String,
        absolute_path: String,
        content: &[u8],
        frontmatter: Option<String>,
    ) -> Self {
        let hash = helpers::compute_hash(content);
        let size_bytes = content.len() as u64;

        let modified_at = std::fs::metadata(&absolute_path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(helpers::format_system_time)
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());

        Self {
            relative_path,
            absolute_path,
            hash,
            frontmatter,
            size_bytes,
            modified_at,
        }
    }
}

// ── IndexStore ─────────────────────────────────────────

/// SQLite-backed persistence for the index.
pub struct IndexStore {
    pub(crate) conn: Connection,
}

impl IndexStore {
    /// Open (or create) the SQLite database at `path`.
    ///
    /// WAL mode and foreign keys are enabled, and schema migrations are
    /// run automatically to bring the database up to date.
    ///
    /// Returns an error if the database was created by a newer version of
    /// the code (downgrade guard).
    pub fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        new_migrations().to_latest(&mut conn)?;
        Ok(Self { conn })
    }

    // ── Writes ───────────────────────────────────────

    /// Insert a new file record, or update an existing one identified by
    /// `relative_path`.
    ///
    /// Returns the file's database id.
    pub fn upsert_file(&mut self, file: &FileInfo) -> Result<i64, Box<dyn std::error::Error>> {
        self.conn.execute(
            "INSERT INTO files (relative_path, absolute_path, hash, frontmatter, size_bytes, modified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(relative_path) DO UPDATE SET
                 absolute_path = excluded.absolute_path,
                 hash = excluded.hash,
                 frontmatter = excluded.frontmatter,
                 size_bytes = excluded.size_bytes,
                 modified_at = excluded.modified_at,
                 indexed_at = datetime('now')",
            params![
                file.relative_path,
                file.absolute_path,
                file.hash,
                file.frontmatter,
                file.size_bytes as i64,
                file.modified_at,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Delete the file identified by `relative_path` and all of its child
    /// rows (headings, links, tags, FTS entry) via `ON DELETE CASCADE`.
    pub fn delete_file(&mut self, relative_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.conn.execute(
            "DELETE FROM files WHERE relative_path = ?1",
            params![relative_path],
        )?;
        Ok(())
    }

    /// Replace all headings for a file.
    ///
    /// Existing headings are removed first, then the new set is inserted.
    /// This is done in a single transaction.
    pub fn set_headings(
        &mut self,
        file_id: i64,
        headings: &[Heading],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM headings WHERE file_id = ?1", params![file_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO headings (file_id, level, text, anchor, position)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for h in headings {
                stmt.execute(params![
                    file_id,
                    h.level,
                    h.text,
                    h.anchor,
                    h.position as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace all links for a file.
    pub fn set_links(
        &mut self,
        file_id: i64,
        links: &[Link],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM links WHERE file_id = ?1", params![file_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO links (file_id, link_type, target, anchor, text, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for link in links {
                stmt.execute(params![
                    file_id,
                    link.link_type.as_str(),
                    link.target,
                    link.target, // anchor field: same as target for now
                    link.text,
                    link.position as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace all tags for a file.
    pub fn set_tags(
        &mut self,
        file_id: i64,
        tags: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM tags WHERE file_id = ?1", params![file_id])?;
        {
            let mut stmt = tx.prepare("INSERT INTO tags (file_id, tag) VALUES (?1, ?2)")?;
            for tag in tags {
                stmt.execute(params![file_id, tag])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Set or update the FTS entry for a file.
    ///
    /// Because FTS5 does not support `ON DELETE CASCADE` natively, the old
    /// row is deleted first via `DELETE` on the fts table.
    pub fn set_fts(
        &mut self,
        file_id: i64,
        title: &str,
        body: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.conn
            .execute("DELETE FROM fts WHERE file_id = ?1", params![file_id])?;
        self.conn.execute(
            "INSERT INTO fts (file_id, title, body) VALUES (?1, ?2, ?3)",
            params![file_id, title, body],
        )?;
        Ok(())
    }

    /// Convenience: upsert a file and set all of its child data in one call.
    ///
    /// Tags are extracted from the frontmatter automatically via
    /// `crate::parser::extract_tags_from_frontmatter`.
    pub fn index_file(
        &mut self,
        file_info: &FileInfo,
        headings: &[Heading],
        links: &[Link],
        tags: &[String],
        title: &str,
        body: &str,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let file_id = self.upsert_file(file_info)?;
        self.set_headings(file_id, headings)?;
        self.set_links(file_id, links)?;
        self.set_tags(file_id, tags)?;
        self.set_fts(file_id, title, body)?;
        Ok(file_id)
    }

    // ── Reads ────────────────────────────────────────

    /// Look up a file's database id by `relative_path`.
    ///
    /// Returns `None` if the file does not exist in the index.
    pub fn get_file_id(
        &self,
        relative_path: &str,
    ) -> Result<Option<i64>, Box<dyn std::error::Error>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM files WHERE relative_path = ?1")?;
        let mut rows = stmt.query(params![relative_path])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Retrieve a file record by `relative_path`.
    pub fn get_file_by_path(
        &self,
        relative_path: &str,
    ) -> Result<Option<FileInfo>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT relative_path, absolute_path, hash, frontmatter, size_bytes, modified_at
             FROM files WHERE relative_path = ?1",
        )?;
        let mut rows = stmt.query(params![relative_path])?;

        match rows.next()? {
            Some(row) => Ok(Some(FileInfo {
                relative_path: row.get(0)?,
                absolute_path: row.get(1)?,
                hash: row.get(2)?,
                frontmatter: row.get(3)?,
                size_bytes: row.get::<_, i64>(4)? as u64,
                modified_at: row.get(5)?,
            })),
            None => Ok(None),
        }
    }

    /// List all indexed files, sorted by `relative_path`.
    pub fn get_all_files(&self) -> Result<Vec<FileInfo>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT relative_path, absolute_path, hash, frontmatter, size_bytes, modified_at
             FROM files ORDER BY relative_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FileInfo {
                relative_path: row.get(0)?,
                absolute_path: row.get(1)?,
                hash: row.get(2)?,
                frontmatter: row.get(3)?,
                size_bytes: row.get::<_, i64>(4)? as u64,
                modified_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Retrieve all headings for a file, ordered by position.
    pub fn get_headings_by_file(
        &self,
        file_id: i64,
    ) -> Result<Vec<Heading>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT level, text, anchor, position FROM headings
             WHERE file_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![file_id], |row| {
            Ok(Heading {
                level: row.get(0)?,
                text: row.get(1)?,
                anchor: row.get(2)?,
                position: row.get::<_, isize>(3)? as usize,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Retrieve all links for a file, ordered by position.
    pub fn get_links_by_file(&self, file_id: i64) -> Result<Vec<Link>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT link_type, target, text, position FROM links
             WHERE file_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![file_id], |row| -> Result<Link, rusqlite::Error> {
            let link_type_str: String = row.get(0)?;
            let link_type = link_type_from_str(&link_type_str)
                .map_err(rusqlite::Error::InvalidParameterName)?;
            Ok(Link {
                link_type,
                target: row.get(1)?,
                text: row.get(2)?,
                position: row.get::<_, isize>(3)? as usize,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Retrieve all tags for a file.
    pub fn get_tags_by_file(
        &self,
        file_id: i64,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE file_id = ?1 ORDER BY tag")?;
        let rows = stmt.query_map(params![file_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Return a read-only [`Query`](crate::query::Query) handle over this
    /// store's connection.
    ///
    /// The handle borrows the store, so no mutations can be performed while
    /// it is alive.
    pub fn query(&self) -> crate::query::Query<'_> {
        crate::query::Query::new(&self.conn)
    }
}

// ── Helper: LinkType deserialization ─────────────────

/// Convert a SQLite link_type string back into a `LinkType`.
pub(crate) fn link_type_from_str(s: &str) -> Result<LinkType, String> {
    match s {
        "inline" => Ok(LinkType::Inline),
        "reference" => Ok(LinkType::Reference),
        "wikilink" => Ok(LinkType::Wikilink),
        "autolink" => Ok(LinkType::Autolink),
        "image" => Ok(LinkType::Image),
        other => Err(format!("unknown link_type: {other}")),
    }
}

// ── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    // ── Schema ──────────────────────────────────────

    #[test]
    fn open_database_creates_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = IndexStore::open(&db_path).unwrap();

        // Verify tables exist
        let tables: Vec<String> = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(
            tables.contains(&"files".to_string()),
            "expected files table, got {tables:?}"
        );
        assert!(
            tables.contains(&"headings".to_string()),
            "expected headings table, got {tables:?}"
        );
        assert!(
            tables.contains(&"links".to_string()),
            "expected links table, got {tables:?}"
        );
        assert!(
            tables.contains(&"tags".to_string()),
            "expected tags table, got {tables:?}"
        );
        assert!(
            tables.contains(&"fts".to_string()),
            "expected fts table, got {tables:?}"
        );
    }

    #[test]
    fn open_database_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // First open
        let store = IndexStore::open(&db_path).unwrap();
        let first_open: Vec<String> = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(store);

        // Second open on the same database should not fail and schema
        // should still be intact.
        let store = IndexStore::open(&db_path).unwrap();
        let second_open: Vec<String> = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(first_open, second_open, "schema changed after second open");
    }

    // ── Files: upsert and query ──────────────────────

    #[test]
    fn upsert_and_get_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "test.md".to_string(),
            "/tmp/test.md".to_string(),
            b"hello world",
            None,
        );
        let id = store.upsert_file(&fi).unwrap();
        assert!(id > 0);

        let fetched = store
            .get_file_by_path("test.md")
            .unwrap()
            .expect("file should exist after upsert");
        assert_eq!(fetched.relative_path, "test.md");
        assert_eq!(fetched.hash, fi.hash);
        assert_eq!(fetched.size_bytes, 11);
    }

    #[test]
    fn upsert_updates_existing() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi1 = FileInfo::from_content(
            "test.md".to_string(),
            "/tmp/test.md".to_string(),
            b"version 1",
            None,
        );
        store.upsert_file(&fi1).unwrap();

        let fi2 = FileInfo::from_content(
            "test.md".to_string(),
            "/tmp/test.md".to_string(),
            b"version 2 with longer content",
            None,
        );
        store.upsert_file(&fi2).unwrap();

        let fetched = store
            .get_file_by_path("test.md")
            .unwrap()
            .expect("file should still exist after reindex");
        assert_eq!(fetched.hash, fi2.hash);
        assert_eq!(fetched.size_bytes, 29);
    }

    #[test]
    fn get_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        for name in &["b.md", "a.md", "c.md"] {
            let fi =
                FileInfo::from_content(name.to_string(), format!("/tmp/{name}"), b"content", None);
            store.upsert_file(&fi).unwrap();
        }

        let all = store.get_all_files().unwrap();
        assert_eq!(all.len(), 3);
        // Sorted by relative_path
        assert_eq!(all[0].relative_path, "a.md");
        assert_eq!(all[1].relative_path, "b.md");
        assert_eq!(all[2].relative_path, "c.md");
    }

    #[test]
    fn get_file_id_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = IndexStore::open(&db_path).unwrap();
        assert!(store.get_file_id("nonexistent.md").unwrap().is_none());
    }

    // ── Headings ────────────────────────────────────

    #[test]
    fn set_and_get_headings() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "headings_test.md".to_string(),
            "/tmp/headings_test.md".to_string(),
            b"",
            None,
        );
        let file_id = store.upsert_file(&fi).unwrap();

        let headings = vec![
            Heading {
                level: 1,
                text: "Introduction".to_string(),
                anchor: "introduction".to_string(),
                position: 0,
            },
            Heading {
                level: 2,
                text: "Details".to_string(),
                anchor: "details".to_string(),
                position: 50,
            },
        ];
        store.set_headings(file_id, &headings).unwrap();

        let stored = store.get_headings_by_file(file_id).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].text, "Introduction");
        assert_eq!(stored[1].text, "Details");
    }

    #[test]
    fn set_headings_replaces_old() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "headings_repl.md".to_string(),
            "/tmp/headings_repl.md".to_string(),
            b"",
            None,
        );
        let file_id = store.upsert_file(&fi).unwrap();

        // Set two headings first
        store
            .set_headings(
                file_id,
                &[
                    Heading {
                        level: 1,
                        text: "First".to_string(),
                        anchor: "first".to_string(),
                        position: 0,
                    },
                    Heading {
                        level: 2,
                        text: "Second".to_string(),
                        anchor: "second".to_string(),
                        position: 10,
                    },
                ],
            )
            .unwrap();

        // Replace with a single heading — proves old ones are deleted, not just overwritten
        store
            .set_headings(
                file_id,
                &[Heading {
                    level: 1,
                    text: "Replacement".to_string(),
                    anchor: "replacement".to_string(),
                    position: 0,
                }],
            )
            .unwrap();

        let stored = store.get_headings_by_file(file_id).unwrap();
        assert_eq!(
            stored.len(),
            1,
            "expected only one heading after replacement"
        );
        assert_eq!(stored[0].text, "Replacement");
    }

    // ── Links ────────────────────────────────────────

    #[test]
    fn set_and_get_links() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "links_test.md".to_string(),
            "/tmp/links_test.md".to_string(),
            b"",
            None,
        );
        let file_id = store.upsert_file(&fi).unwrap();

        let links = vec![
            Link {
                link_type: LinkType::Inline,
                target: "https://example.com".to_string(),
                text: Some("Example".to_string()),
                position: 0,
            },
            Link {
                link_type: LinkType::Wikilink,
                target: "other-page".to_string(),
                text: Some("other-page".to_string()),
                position: 20,
            },
        ];
        store.set_links(file_id, &links).unwrap();

        let stored = store.get_links_by_file(file_id).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].link_type, LinkType::Inline);
        assert_eq!(stored[0].target, "https://example.com");
        assert_eq!(stored[1].link_type, LinkType::Wikilink);
        assert_eq!(stored[1].target, "other-page");
    }

    // ── Tags ────────────────────────────────────────

    #[test]
    fn set_and_get_tags() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "tagged.md".to_string(),
            "/tmp/tagged.md".to_string(),
            b"",
            None,
        );
        let file_id = store.upsert_file(&fi).unwrap();

        store
            .set_tags(file_id, &["rust".to_string(), "markdown".to_string()])
            .unwrap();

        let stored = store.get_tags_by_file(file_id).unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.contains(&"rust".to_string()));
        assert!(stored.contains(&"markdown".to_string()));

        // Replace
        store.set_tags(file_id, &["updated".to_string()]).unwrap();
        let stored2 = store.get_tags_by_file(file_id).unwrap();
        assert_eq!(stored2, vec!["updated"]);
    }

    // ── FTS ─────────────────────────────────────────

    #[test]
    fn set_fts_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "fts_test.md".to_string(),
            "/tmp/fts_test.md".to_string(),
            b"",
            None,
        );
        let file_id = store.upsert_file(&fi).unwrap();

        store
            .set_fts(
                file_id,
                "My Title",
                "The quick brown fox jumps over the lazy dog.",
            )
            .unwrap();

        let results: Vec<(i64, String)> = store
            .conn
            .prepare(
                "SELECT file_id, snippet(fts, 2, '<b>', '</b>', '...', 16)
                 FROM fts WHERE fts MATCH 'fox'",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, file_id);
        assert!(results[0].1.contains("fox"));
    }

    // ── Cascade delete ──────────────────────────────

    #[test]
    fn delete_file_cascades() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let fi = FileInfo::from_content(
            "cascade.md".to_string(),
            "/tmp/cascade.md".to_string(),
            b"",
            None,
        );
        let file_id = store.upsert_file(&fi).unwrap();

        store
            .set_headings(
                file_id,
                &[Heading {
                    level: 1,
                    text: "Test".to_string(),
                    anchor: "test".to_string(),
                    position: 0,
                }],
            )
            .unwrap();
        store.set_tags(file_id, &["tag1".to_string()]).unwrap();
        store.set_fts(file_id, "Test", "body").unwrap();

        // Delete and verify cascade
        store.delete_file("cascade.md").unwrap();
        assert!(store.get_file_by_path("cascade.md").unwrap().is_none());
        assert!(store.get_headings_by_file(file_id).unwrap().is_empty());
        assert!(store.get_tags_by_file(file_id).unwrap().is_empty());
    }

    // ── Integration: index_file convenience ─────────

    #[test]
    fn index_file_full_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();

        let root = dir.path();
        let relative_path = "post.md";
        let content = "---\ntitle: My Post\ntags:\n  - rust\n  - tutorial\n---\n\n# My Post\n\nHello **world**!";
        let absolute_path = root.join(relative_path);
        fs::write(&absolute_path, content).unwrap();

        // Parse with our parser
        let parser = crate::parser::Parser::new();
        let result = parser.parse(content);

        let file_info = FileInfo::from_content(
            relative_path.to_string(),
            absolute_path.to_string_lossy().to_string(),
            content.as_bytes(),
            result.frontmatter.clone(),
        );

        let tags = crate::parser::extract_tags_from_frontmatter(result.frontmatter.as_deref());
        let title = result
            .headings
            .first()
            .map(|h| h.text.as_str())
            .unwrap_or(relative_path);

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

        // Verify stored data
        let stored_file = store.get_file_by_path(relative_path).unwrap().unwrap();
        assert_eq!(stored_file.hash, file_info.hash);

        let file_id = store.get_file_id(relative_path).unwrap().unwrap();
        let stored_headings = store.get_headings_by_file(file_id).unwrap();
        assert_eq!(stored_headings.len(), 1);
        assert_eq!(stored_headings[0].text, "My Post");

        let stored_tags = store.get_tags_by_file(file_id).unwrap();
        assert!(stored_tags.contains(&"rust".to_string()));
        assert!(stored_tags.contains(&"tutorial".to_string()));
    }

    #[test]
    fn index_multi_file_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut store = IndexStore::open(&db_path).unwrap();
        let parser = crate::parser::Parser::new();

        // Create 4 markdown files
        let files: Vec<(&str, &str)> = vec![
            ("index.md", "# Home\n\nWelcome to [[docs]]."),
            (
                "docs.md",
                "---\ntags: [documentation]\n---\n\n# Docs\n\nSee [[index]] for home.",
            ),
            (
                "about.md",
                "---\ntags: [meta]\n---\n\n# About\n\nNo outgoing links.",
            ),
            (
                "draft.md",
                "---\ntags: [wip, draft]\n---\n\n# Draft\n\nUnfinished.",
            ),
        ];

        for (name, content) in &files {
            let path = dir.path().join(name);
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(content.as_bytes()).unwrap();
        }

        // Index each file
        for (name, content) in &files {
            let absolute_path = dir.path().join(name);
            let content_str = content;
            let result = parser.parse(content_str);

            let file_info = FileInfo::from_content(
                name.to_string(),
                absolute_path.to_string_lossy().to_string(),
                content_str.as_bytes(),
                result.frontmatter.clone(),
            );

            let tags = crate::parser::extract_tags_from_frontmatter(result.frontmatter.as_deref());
            let title = result
                .headings
                .first()
                .map(|h| h.text.as_str())
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

        // Verify all files indexed
        let all_files = store.get_all_files().unwrap();
        assert_eq!(all_files.len(), 4);

        // Verify headigs per file
        for (name, _) in &files {
            let file_id = store.get_file_id(name).unwrap().unwrap();
            let headings = store.get_headings_by_file(file_id).unwrap();
            assert!(!headings.is_empty(), "{name} has no headings");
        }

        // Verify tags
        let docs_id = store.get_file_id("docs.md").unwrap().unwrap();
        let docs_tags = store.get_tags_by_file(docs_id).unwrap();
        assert_eq!(docs_tags, vec!["documentation"]);

        let draft_id = store.get_file_id("draft.md").unwrap().unwrap();
        let draft_tags = store.get_tags_by_file(draft_id).unwrap();
        assert_eq!(draft_tags.len(), 2);
        assert!(draft_tags.contains(&"wip".to_string()));

        // Verify wikilinks
        let index_id = store.get_file_id("index.md").unwrap().unwrap();
        let index_links = store.get_links_by_file(index_id).unwrap();
        assert!(index_links.iter().any(|l| l.target == "docs"));

        let docs_id2 = store.get_file_id("docs.md").unwrap().unwrap();
        let docs_links = store.get_links_by_file(docs_id2).unwrap();
        assert!(docs_links.iter().any(|l| l.target == "index"));
    }
}
