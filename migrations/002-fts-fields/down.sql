-- Inverse of 002-fts-fields/up.sql: rebuild `fts` without the `path` column,
-- discarding it. The file's path remains available in `files.relative_path`,
-- so nothing is lost that the schema does not already hold.
--
-- Like up.sql, the scratch table is dropped and recreated unconditionally so
-- that re-running this migration rebuilds rather than reuses a stale table.

DROP TABLE IF EXISTS fts_old;

CREATE VIRTUAL TABLE fts_old USING fts5(
    file_id UNINDEXED,
    title,
    body,
    tokenize='porter unicode61'
);

INSERT INTO fts_old (file_id, title, body)
SELECT file_id, title, body FROM fts;

DROP TABLE fts;

ALTER TABLE fts_old RENAME TO fts;
