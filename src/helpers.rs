//! Shared helpers used across the crate.

use std::path::PathBuf;

/// Sort relative paths deterministically (case-insensitive,
/// component-by-component comparison).
///
/// Used by both [`crate::scanner::Scanner`] and subtree walks so that
/// incremental indexing produces the same ordering as a full scan.
pub(crate) fn sort_relative_paths(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        // Compare component-by-component for OS-independent ordering.
        let a_components: Vec<_> = a
            .components()
            .map(|component| component.as_os_str().to_ascii_lowercase())
            .collect();
        let b_components: Vec<_> = b
            .components()
            .map(|component| component.as_os_str().to_ascii_lowercase())
            .collect();
        a_components.cmp(&b_components)
    });
}
