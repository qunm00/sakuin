# Sakuin — Implementation Plan

> Implementation roadmap for the Sakuin Markdown workspace indexer.
> See [design.md](design.md) for the full architecture and design documentation.

---

## Table of Contents

1. [Implementation Phases](#implementation-phases)
2. [Testing Strategy](#testing-strategy)
3. [Future Work](#future-work)

---

## Implementation Phases

### Phase 0 — Project scaffolding ✅

- [x] Set up Rust project with edition 2024.
- [x] Add dependencies: `rusqlite` (bundled), `pulldown-cmark`, `notify`,
      `serde`, `serde_yaml`, `sha2`, `ignore`, `log`.
- [x] Define workspace layout (lib crate with example in `examples/`).
- [x] Configure CI (GitHub Actions: `cargo test`, `cargo clippy`, `cargo fmt`).

### Phase 1 — Core scanning & parsing ✅

- [x] Implement `Scanner` — recursive dir walk, `.gitignore` support.
- [x] Implement `Parser` — frontmatter extraction, heading collection,
      link collection, text extraction.
- [x] Wikilink support (`[[target]]` and `[[target|label]]`).
- [x] Write unit tests for parser against known Markdown samples.

### Phase 2 — SQLite storage

- [ ] Implement `IndexStore` — schema creation, CRUD for files, headings,
      links, tags.
- [ ] Implement FTS5 table and text indexing.
- [ ] Write integration tests: index a sample workspace, query it, verify
      results match expected values.

### Phase 3 — Query API

- [ ] Implement all `Query` methods.
- [ ] Implement `search()` with FTS5.
- [ ] Implement `backlinks()`.
- [ ] Write query tests.

### Phase 4 — Filesystem watching

- [ ] Implement `FileWatcher` using `notify`.
- [ ] Implement incremental update logic.
- [ ] Test with simulated filesystem events (`tempfile`).

### Phase 5 — Determinism & correctness

- [ ] Add deterministic-ordering tests.
- [ ] Add property-based tests (e.g., `proptest`) for idempotency.
- [ ] Benchmark full re-index vs. incremental update.

### Phase 6 — Polish & documentation

- [ ] Write API docs (`#![warn(missing_docs)]`).
- [ ] Write a comprehensive `README.md` with usage examples.
- [ ] Publish to crates.io (optional).

---

## Testing Strategy

### Unit tests

- Parser: known Markdown snippets → expected headings/links/text.
- Scanner: temp directory with specific file layout → expected file list.
- Slug generation: various heading texts → expected anchors.

### Integration tests

- Build a temp workspace with 5–10 Markdown files covering all features
  (frontmatter, headings, wikilinks, images, references).
- Full scan → query and assert counts, values, relationships.
- Modify a file → incremental update → re-query.
- Delete a file → incremental update → gone from index.

### Determinism tests

- Scan the same temp directory twice in a row. Compare the SQLite databases
  (ignoring timestamps and auto-incremented IDs). Assert equality.

### Property-based tests (Phase 5)

- Generate random Markdown files → index → re-index → same result.
- Generate random sequences of file modifications → index state converges.

---

## Future Work

- **Cross-file reference resolution** — Resolve wikilink targets to actual file
  paths; detect broken links.
- **Graph export** — Export the link graph as adjacency lists for external
  analysis.
- **Multi-root workspaces** — Index several directories under one SQLite
  database.
- **Custom frontmatter schemas** — Allow applications to register typed
  frontmatter fields for querying.
- **Larger-file streaming** — Use `pulldown-cmark`'s streaming API for files
  too large to fit in memory (rare for Markdown, but possible).
- **Rebuild from FTS content** — If the raw files are lost, reconstruct a
  searchable index from the stored FTS data (privacy-sensitive, opt-in).
- **Language detection in fenced code blocks** — Skip or index code blocks
  based on language for smarter search.

---

*This plan is a living document. Update it as the project evolves.*
