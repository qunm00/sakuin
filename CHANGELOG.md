# Changelog

All notable changes to sakuin-md are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2024-10-07

### Added

- **Advanced search with options** via new `search_with_options` and `try_search` methods
  - `SearchOptions` struct to configure search behavior
  - `SearchField` enum to filter search scope: `All`, `Path`, `Title`, or `Body`
  - Support for result paging via `offset` and `limit` options
  - Support for prefix matching via FTS5 prefix queries (e.g., `word*`)
  - Support for case-sensitive post-filtering of FTS5 results
  - `SearchError` type for detailed error reporting

- **FTS5 query utilities**
  - `quote_query()` function to escape FTS5 special characters in queries
  - `quote_query_prefix()` function for safe prefix-match queries

- **FTS5 index improvements**
  - Migration 002: Added searchable `path` column to FTS5 virtual table
  - Path column automatically backfilled for existing databases
  - Enables efficient file path search without joining to `files` table

- **Property-based testing**
  - Comprehensive `proptest` test suite for `SearchOptions` and query building
  - Validates search behavior across randomized inputs and edge cases

- **Container development environment**
  - Docker configuration for isolated Rust development
  - Documentation for building and using the container toolchain

### Changed

- FTS5 index structure: `path` is now a searchable column (migration 002 handles upgrade)

### Fixed

- `.gitignore` now properly excludes generated SQLite database files (`*.db`, `*.db-shm`, `*.db-wal`)

### Documentation

- Full-text search options documented with usage examples
- Paging, prefix matching, and case sensitivity patterns explained
- Conventional Commits guidelines added to developer guidance

## [0.1.0] - 2024-08-16

### Added

- Initial release
- Core Markdown indexing into SQLite database
- Full-text search with FTS5
- Wikipedia-style wikilinks support
- Filesystem watching with live index updates
- Query API for headings, links, tags, and full-text search
- Support for frontmatter (YAML) extraction
- File change detection via content hashing
- SQLite schema with migrations
