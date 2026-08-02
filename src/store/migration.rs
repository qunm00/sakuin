/// Schema migrations for the index store.
///
/// Migrations are loaded from the `migrations/` directory at compile time
/// via `include_dir`.  Each subdirectory follows the naming convention
/// `{nnn}-{description}/` and must contain at least an `up.sql` file.
use include_dir::{Dir, include_dir};
use rusqlite_migration::Migrations;

/// Embed the migrations directory at compile time.
static MIGRATION_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

/// Build a new `Migrations` set from the embedded migration directory.
///
/// Parsing the directory tree is cheap — call this on each `open()`.
pub fn new_migrations() -> Migrations<'static> {
    Migrations::from_directory(&MIGRATION_DIR).expect("invalid migration directory structure")
}
