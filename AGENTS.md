## Learned User Preferences

- Don't abbreviate variable names; use full descriptive names (e.g., `relative_path` instead of `rel_path`, `absolute_path` instead of `abs_path`).
- Never commit code without asking the user first. Always ask before running `git commit` (or any operation that creates commits).

## Learned Workspace Facts

- Sakuin is a Rust library (not a binary crate) for indexing Markdown workspaces into a SQLite database.
- Documentation is split into `docs/design.md` (architecture & design) and `docs/plan.md` (implementation roadmap).
- Core dependencies: `rusqlite` (bundled SQLite), `pulldown-cmark` (Markdown parsing), `notify` (file watching), `serde`/`serde_yaml`, `sha2`, `ignore`, `log`.
- SQLite schema includes tables: `files` (with `relative_path`, `absolute_path`, `hash`, `frontmatter`, `size_bytes`, `modified_at`, `indexed_at`), `headings` (level, text, anchor, position), `links` (link_type, target, anchor, text, position), `tags` (tag), and an FTS5 virtual table `fts` (file_id, title, body).
- Module structure: `scanner`, `parser`, `store`, `query`, `watcher`, `indexer` under `src/`.
- Example binary at `examples/basic-index.rs` showing the intended usage flow.
- CI pipeline runs on GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo build --all-targets`, `cargo test` on push/PR to `main`.
- `.pi/state/` is gitignored; `AGENTS.md` is committed as shared agent knowledge.

## Phase 1 Implementation Details

### Scanner (`src/scanner.rs`)
- Uses `ignore::WalkBuilder` with `standard_filters(true)` (respects `.gitignore`) and `hidden(true)` (skips dotfiles).
- Filters to files with `.md` extension (case-insensitive).
- Returns relative paths sorted alphabetically (case-insensitive component comparison).
- Logs errors for inaccessible paths via `log::warn!`.
- Unit tests cover: finding markdown files, skipping non-markdown, skipping hidden files, empty directories, sorted output.

### Parser (`src/parser.rs`)
- **Frontmatter**: Manual extraction of `---\n...\n---` pattern at file start. Returns raw YAML text and byte offset where content begins. Handles `\r\n` line endings.
- **Wikilinks**: Uses pulldown-cmark 0.13's native wikilink support via `Options::ENABLE_WIKILINKS`. The parser emits `Tag::Link` with `LinkType::WikiLink { has_pothole: bool }` for `[[target]]` and `[[target|label]]` patterns. No pre-processing needed.
- **Headings**: Collects from `Tag::Heading` events. Handles ATX and setext headings (via pulldown-cmark). Text is concatenated from nested `Text`/`Code` events (strips formatting markers).
- **Links**: Collects from `Tag::Link` and `Tag::Image` events. Converts pulldown-cmark `LinkType` to local `LinkType` enum (Inline, Reference, Autolink, Image, Wikilink).
- **Body text**: Concatenates all `Text`, `Code`, `SoftBreak`, `HardBreak` events for FTS.
- **Anchor slug generation**: GitHub-compatible algorithm: lowercase, strip HTML tags, replace non-alphanumeric with hyphens, collapse hyphens, trim, deduplicate with `-1`, `-2`, etc.
- Public types: `ParseResult`, `Heading`, `Link`, `LinkType`.
- Unit tests cover: frontmatter (with/without, empty, CRLF, unclosed), ATX headings, headings with formatting, anchor slugs (punctuation, dedup, whitespace, HTML tags, trimming), inline links, reference links, autolinks, images, wikilinks (basic, alias, multiple, mixed with regular links, empty), body text extraction, empty document, complex multi-element document.

### WikiLink Strategy
- Uses pulldown-cmark 0.13's native `Options::ENABLE_WIKILINKS` extension.
- `Tag::Link` events with `LinkType::WikiLink { has_pothole: bool }` are checked directly — no pre-processing or URI scheme hacks needed.
- `has_pothole: false` for `[[target]]`, `has_pothole: true` for `[[target|label]]`.

### Progress
- Phase 0: ✅ (project scaffolding)
- Phase 1: ✅ (Scanner, Parser, wikilink support, unit tests)
- Phase 2: ✅ (IndexStore, SQLite, schema migration, FTS5, tags extraction)
- Phase 3: ✅ (Query API, FTS5 search, backlinks, query tests)
- Phase 4: ❌ (FileWatcher)
- Phase 5: ❌ (Determinism & property tests)
- Phase 6: ❌ (Polish & documentation)
