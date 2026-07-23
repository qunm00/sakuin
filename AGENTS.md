## Learned User Preferences

- Don't abbreviate variable names; use full descriptive names (e.g., `relative_path` instead of `rel_path`, `absolute_path` instead of `abs_path`).

## Learned Workspace Facts

- Sakuin is a Rust library (not a binary crate) for indexing Markdown workspaces into a SQLite database.
- Documentation is split into `docs/design.md` (architecture & design) and `docs/plan.md` (implementation roadmap).
- Core dependencies: `rusqlite` (bundled SQLite), `pulldown-cmark` (Markdown parsing), `notify` (file watching), `serde`/`serde_yaml`, `sha2`, `ignore`, `log`.
- SQLite schema includes tables: `files` (with `relative_path`, `absolute_path`, `hash`, `frontmatter`, `size_bytes`, `created_at`, `indexed_at`), `headings` (level, text, anchor, position), `links` (link_type, target, anchor, text, position), `tags` (tag), and an FTS5 virtual table `fts` (file_id, title, body).
- Module structure: `scanner`, `parser`, `store`, `query`, `watcher`, `indexer` under `src/`.
- Example binary at `examples/basic-index.rs` showing the intended usage flow.
- CI pipeline runs on GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo build --all-targets`, `cargo test` on push/PR to `main`.
- `.pi/state/` is gitignored; `AGENTS.md` is committed as shared agent knowledge.
