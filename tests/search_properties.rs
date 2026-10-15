//! Property tests for the search query escaping layer.
//!
//! The contract under test: whatever a user types must be matched as *literal
//! text*, never interpreted as FTS5 syntax. Two properties capture that:
//!
//! 1. `quote_query` always produces something FTS5 accepts as a `MATCH`
//!    argument (so no input can ever produce a syntax error).
//! 2. Escaping is faithful — the phrase is exactly the input with whitespace
//!    collapsed, and an escaped query cannot widen the result set.
//!
//! The raw `MATCH` is reached through the deprecated `Query::search`, which is
//! the public API that passes a query string to FTS5 verbatim. Without it the
//! stored `conn` would be needed, and integration tests cannot touch it.

#![allow(deprecated)]

use proptest::prelude::*;
use sakuin_md::query::{quote_query, quote_query_prefix};
use sakuin_md::store::{FileInfo, IndexStore};

/// Fragments chosen to be hostile: FTS5 operators, GLOB wildcards, quotes,
/// NUL, and non-ASCII text.
const FRAGMENTS: [&str; 32] = [
    "\"", "'", "*", "^", ":", "-", "(", ")", "[", "]", "{", "}", "\\", "/", ".", "%", "_", "=",
    "AND", "OR", "NOT", "NEAR", "a", "rust", "Zebra", "1", " ", "\t", "\n", "\0", "🦀", "é",
];

/// The indexed text in the property tests.
///
/// Deliberately a nonsense token: `porter` reduces ordinary English words
/// (`zebrae` stems to `zebra`), and `unicode61` folds diacritics, so a real
/// word would let several inputs match it for reasons that have nothing to do
/// with escaping. `qqzzz` is matched by exactly the token `qqzzz`.
const INDEXED_TEXT: &str = "qqzzz";

/// A hostile query: a random concatenation of fragments.
fn hostile_query() -> impl Strategy<Value = String> {
    prop::collection::vec(0..FRAGMENTS.len(), 0..8).prop_map(|indices| {
        indices
            .iter()
            .map(|index| FRAGMENTS[*index])
            .collect::<String>()
    })
}

/// A store holding exactly one file whose text is [`INDEXED_TEXT`].
fn indexed_store() -> (tempfile::TempDir, IndexStore) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::open(&dir.path().join("test.db")).unwrap();
    let file = FileInfo {
        relative_path: "only.md".to_string(),
        absolute_path: "/w/only.md".to_string(),
        hash: "prop".to_string(),
        frontmatter: None,
        size_bytes: 1,
        modified_at: "2024-01-01 00:00:00".to_string(),
    };
    let file_id = store.upsert_file(&file).unwrap();
    store.set_fts(file_id, "Only", INDEXED_TEXT).unwrap();
    (dir, store)
}

proptest! {
    /// Any input at all produces a `MATCH` argument FTS5 accepts.
    #[test]
    fn quote_query_always_parses(query in hostile_query()) {
        let (_dir, store) = indexed_store();
        let phrase = quote_query(&query);
        let result = store.query().search(&phrase);
        prop_assert!(
            result.is_ok(),
            "quote_query({query:?}) produced {phrase:?}, which FTS5 rejected: {:?}",
            result.err()
        );
    }

    /// The same holds with the prefix marker appended.
    #[test]
    fn quote_query_prefix_always_parses(query in hostile_query()) {
        let (_dir, store) = indexed_store();
        let phrase = quote_query_prefix(&query, true);
        let result = store.query().search(&phrase);
        prop_assert!(
            result.is_ok(),
            "quote_query_prefix({query:?}, true) produced {phrase:?}, \
             which FTS5 rejected: {:?}",
            result.err()
        );
    }

    /// Quoting is faithful: unescaping the phrase returns the input with
    /// whitespace collapsed and NUL removed — nothing added, nothing lost.
    #[test]
    fn quote_query_is_reversible(query in hostile_query()) {
        let phrase = quote_query(&query);
        prop_assert!(phrase.starts_with('"') && phrase.ends_with('"'));
        let inner = &phrase[1..phrase.len() - 1];
        let unescaped = inner.replace("\"\"", "\"");
        let expected = query
            .replace('\0', "")
            .split_whitespace()
            .collect::<Vec<&str>>()
            .join(" ");
        prop_assert_eq!(unescaped, expected);
    }

    /// An escaped query can never widen the result set. The single indexed
    /// row is returned only when the query literally spells out its text,
    /// which a bare `*` or an unquoted `OR` would otherwise manage.
    #[test]
    fn hostile_queries_cannot_widen_the_result_set(query in hostile_query()) {
        let (_dir, store) = indexed_store();
        let phrase = quote_query(&query);
        let results = store.query().search(&phrase);
        prop_assert!(results.is_ok());
        if results.unwrap().len() == 1 {
            // FTS5's tokenizer drops punctuation, so `*qqzzz` still matches the
            // text. Compare the alphanumeric residue of each whitespace-separated
            // token instead of the raw token.
            let tokens: Vec<String> = query
                .replace('\0', "")
                .split_whitespace()
                .map(|token| {
                    token
                        .chars()
                        .filter(|character| character.is_alphanumeric())
                        .collect::<String>()
                        .to_lowercase()
                })
                .collect();
            prop_assert!(
                tokens.contains(&INDEXED_TEXT.to_string()),
                "{query:?} matched without spelling the indexed text"
            );
        }
    }
}
