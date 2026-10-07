-- 002-fts-fields: make the FTS index searchable by file path.
--
-- `fts` gains a `path` column so queries can be restricted to (or weighted
-- towards) a file's path relative to the workspace root.
--
-- FTS5 virtual tables cannot be ALTERed, so this is a rebuild: create a
-- scratch table with the new shape, copy into it, drop the old one, rename.
-- The rename also renames the FTS5 shadow tables, which is why the scratch
-- table's name does not survive.
--
-- The scratch table is dropped and recreated unconditionally rather than
-- created with `IF NOT EXISTS`. A rebuild migration must be idempotent by
-- rebuilding: if an earlier attempt left `fts_new` behind, reusing it would
-- promote a stale, partial index over the live one.
--
-- The copy uses an inner JOIN on `files`, so an `fts` row with no matching
-- file record is discarded. That is deliberate: such rows are unreachable
-- through every query path, which joins `fts` to `files` on `file_id`.
--
-- Case-sensitive matching is deliberately NOT handled here.  An earlier draft
-- of this migration added a second table using
-- `tokenize='unicode61 case_sensitive 1'`, but that option does not exist:
-- `case_sensitive` is a `trigram`-only tokenizer option.  `unicode61` accepts
-- only `remove_diacritics`, `tokenchars`, `separators`, and `categories`, and
-- folds case unconditionally.  See docs/features/full-text-search-options.md
-- for the FTS-candidates-plus-GLOB approach that replaced it.

DROP TABLE IF EXISTS fts_new;

CREATE VIRTUAL TABLE fts_new USING fts5(
    file_id UNINDEXED,
    path,
    title,
    body,
    tokenize='porter unicode61'
);

INSERT INTO fts_new (file_id, path, title, body)
SELECT fts.file_id, files.relative_path, fts.title, fts.body
FROM fts
JOIN files ON files.id = fts.file_id;

DROP TABLE fts;

ALTER TABLE fts_new RENAME TO fts;
