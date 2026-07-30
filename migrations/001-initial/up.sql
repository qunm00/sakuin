CREATE TABLE IF NOT EXISTS files (
    id             INTEGER PRIMARY KEY,
    relative_path  TEXT NOT NULL UNIQUE,
    absolute_path  TEXT NOT NULL,
    hash           TEXT NOT NULL,
    frontmatter    TEXT,
    size_bytes     INTEGER NOT NULL,
    modified_at    TEXT NOT NULL,
    indexed_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS headings (
    id        INTEGER PRIMARY KEY,
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    level     INTEGER NOT NULL CHECK(level BETWEEN 1 AND 6),
    text      TEXT NOT NULL,
    anchor    TEXT NOT NULL,
    position  INTEGER NOT NULL,
    UNIQUE(file_id, position)
);

CREATE TABLE IF NOT EXISTS links (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    link_type   TEXT NOT NULL CHECK(link_type IN (
        'inline', 'reference', 'wikilink', 'autolink', 'image'
    )),
    target      TEXT NOT NULL,
    anchor      TEXT,
    text        TEXT,
    position    INTEGER NOT NULL,
    UNIQUE(file_id, position)
);

CREATE TABLE IF NOT EXISTS tags (
    id      INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    tag     TEXT NOT NULL,
    UNIQUE(file_id, tag)
);

CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
    file_id UNINDEXED,
    title,
    body,
    tokenize='porter unicode61'
);

CREATE INDEX IF NOT EXISTS idx_headings_file ON headings(file_id);
CREATE INDEX IF NOT EXISTS idx_links_file ON links(file_id);
CREATE INDEX IF NOT EXISTS idx_tags_file ON tags(file_id);
CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
