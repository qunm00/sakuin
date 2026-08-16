//! # Sakuin — Markdown workspace indexer
//!
//! **Sakuin** (索引, Japanese for "index") is a library for indexing a
//! directory of Markdown files into a single SQLite database. It scans the
//! workspace, parses every file into structured data (frontmatter, headings,
//! links, tags, full-text body), and keeps that index live as the filesystem
//! changes.
//!
//! It is designed for applications that work with Markdown repositories —
//! wikis, note-taking apps, documentation sites, knowledge bases — so they
//! don't have to re-implement scanning, parsing, and indexing.
//!
//! ## Quick start
//!
//! The [`Indexer`] ties everything together: open an index for a workspace,
//! run a full scan, watch for changes, and query the results.
//!
//! ```no_run
//! use std::path::Path;
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut indexer = sakuin_md::indexer::Indexer::open(Path::new("."), Path::new("sakuin.db"))?;
//! indexer.scan_full()?;
//! indexer.listen()?;
//!
//! loop {
//!     indexer.poll()?;
//!     std::thread::sleep(Duration::from_millis(100));
//! }
//! # }
//! ```
//!
//! See [`Indexer`] and [`Query`] for the full API, or run the included
//! example with `cargo run --example basic-index`.
//!
//! [`Indexer`]: crate::indexer::Indexer
//! [`Query`]: crate::query::Query

#![warn(missing_docs)]

pub mod indexer;
pub mod parser;
pub mod query;
pub mod scanner;
pub mod store;
pub mod watcher;

mod helpers;

pub use query::LinkType;
pub use query::{FileEntry, HeadingEntry, LinkEntry, LinkFilter, Query, SearchResult};
pub use watcher::WatchEvent;
