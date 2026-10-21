# Release Plan: sakuin-md v0.2.0

## Overview
Release sakuin-md v0.2.0 with full-text search enhancements and query options.

**Current version:** 0.1.0
**Target version:** 0.2.0
**Commits ahead:** 12 (all on main, ready to release)

## Key Changes in This Release

### Features
- **Full-text search with options** (`search_with_options`, `try_search`)
  - New `SearchOptions` struct supporting paging, prefix matching, and case sensitivity
  - `SearchField` enum to filter search scope (all, path, title, body)
  - `SearchError` type for proper error handling
  - FTS5 query escapers: `quote_query` and `quote_query_prefix`

- **FTS5 index improvements**
  - Migration 002: Added searchable `path` column to FTS5 index (backfilled from `files.relative_path`)
  - Enables path-aware search queries

### Testing
- Property-based tests for search options and queries using `proptest`

### Documentation
- Full-text search options documented with examples (paging, prefix matching, case sensitivity)
- Conventional commits guidelines added to developer documentation

### Infrastructure
- Container dev environment documentation (docker/dev setup)
- `.gitignore` updates for generated database files

## Release Checklist

### Phase 1: Preparation (Local)
- [ ] **Verify code quality** (must run locally or in CI)
  - [ ] `cargo fmt --check` — formatting validation
  - [ ] `cargo clippy --all-targets -- -D warnings` — linter (no warnings allowed)
  - [ ] `cargo build --all-targets` — compilation succeeds
  - [ ] `cargo test` — all 125+ tests pass
  
- [ ] **Review git log**
  - [ ] Confirm 12 commits are all merged to main
  - [ ] Verify commit messages follow Conventional Commits
  
- [ ] **Create CHANGELOG.md**
  - [ ] Document version 0.2.0 release date
  - [ ] List features, fixes, and breaking changes
  - [ ] Link to commit hashes where relevant

### Phase 2: Version Bump
- [ ] Update `Cargo.toml` version: `0.1.0` → `0.2.0`
- [ ] Commit with message: `chore(release): bump version to 0.2.0`
- [ ] **⚠️ DO NOT push yet** — wait for verification

### Phase 3: Verification (CI or Local)
- [ ] Push to origin/main
- [ ] Verify GitHub Actions passes (if configured):
  - [ ] `cargo fmt --check`
  - [ ] `cargo clippy --all-targets -- -D warnings`
  - [ ] `cargo build --all-targets`
  - [ ] `cargo test`

### Phase 4: Release Tag
- [ ] Create annotated git tag: `git tag -a v0.2.0 -m "Release version 0.2.0"`
- [ ] Push tag: `git push origin v0.2.0`
- [ ] Verify tag on GitHub

### Phase 5: Publish to crates.io
- [ ] Verify you are logged in: `cargo login` (if needed)
- [ ] Dry-run publish: `cargo publish --dry-run`
- [ ] Publish: `cargo publish`
- [ ] Verify on crates.io: https://crates.io/crates/sakuin-md/0.2.0

### Phase 6: Post-Release
- [ ] Announce release (if applicable: GitHub releases, social media, newsletter)
- [ ] Close any related issues/PRs

## Important Notes

- **Cargo not available in sandbox:** Full verification (fmt, clippy, build, test) must run on your local machine or GitHub Actions
- **CHANGELOG.md:** Will be created before version bump
- **Conventional Commits:** Already established in AGENTS.md; verify commit messages follow `type(scope): description`
- **Database migrations:** Migration 002 is safe for existing databases (backfill + new FTS column)

## Next Steps

1. Review this plan
2. Create CHANGELOG.md with release notes
3. Update Cargo.toml to v0.2.0
4. Run verification (local: `cargo test`, `cargo clippy`, `cargo fmt --check`)
5. Push and tag
6. Publish to crates.io

