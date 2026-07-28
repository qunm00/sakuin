/// Helper utilities for the parser module.
use std::collections::HashSet;

// ── Tag extraction from YAML frontmatter ────────────────

/// Deserialization helper: a `tags` field that can be a single string
/// or a sequence of strings.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum TagsOrString {
    Sequence(Vec<String>),
    Single(String),
}

/// Deserialization helper: top-level frontmatter shape.
#[derive(serde::Deserialize)]
struct Frontmatter {
    #[serde(default)]
    tags: Option<TagsOrString>,
}

/// Extract tag strings from a YAML frontmatter string.
///
/// Looks for a `tags` key that is either a sequence of strings
/// or a single string.  Returns an empty `Vec` when the frontmatter
/// is `None` or contains no `tags` key.
pub fn extract_tags_from_frontmatter(frontmatter: Option<&str>) -> Vec<String> {
    let yaml_str = match frontmatter {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };

    let parsed: Frontmatter = match yaml_serde::from_str(yaml_str) {
        Ok(fm) => fm,
        Err(_) => return Vec::new(),
    };

    match parsed.tags {
        Some(TagsOrString::Sequence(tags)) => tags,
        Some(TagsOrString::Single(tag)) => vec![tag],
        None => Vec::new(),
    }
}

// ── Frontmatter extraction ───────────────────────────────

/// Extracts raw YAML frontmatter from the source.
///
/// Returns `(Some(raw_yaml), content_start_byte)` or `(None, 0)` if no frontmatter.
/// `content_start_byte` is the byte offset where the actual Markdown content begins
/// (after the closing `---` and any trailing blank lines).
pub(crate) fn extract_frontmatter(source: &str) -> (Option<String>, usize) {
    // Determine the opening delimiter length
    let opener_len = if source.starts_with("---\r\n") {
        5
    } else if source.starts_with("---\n") {
        4
    } else {
        return (None, 0);
    };

    // Find the closing "---" on its own line.
    // Look for "\n---" after the opener.
    let search_start = opener_len;
    if let Some(closing) = source[search_start..].find("\n---") {
        let closing_start = search_start + closing; // byte offset of '\n' before '---'

        let yaml_content = &source[opener_len..closing_start];
        let yaml_trimmed = yaml_content.trim();
        let yaml = if yaml_trimmed.is_empty() {
            None
        } else {
            Some(yaml_trimmed.to_string())
        };

        // Content starts after the closing "---" and its line ending
        let after_closing = closing_start + 4; // skip "\n---"
        // Skip any trailing blank lines (\n, \r\n, or multiple)
        let content_start = after_closing + consume_line_endings(&source[after_closing..]);

        (yaml, content_start)
    } else {
        // No closing delimiter — treat entire thing as content
        (None, 0)
    }
}

/// Consumes consecutive line ending sequences (`\n` or `\r\n`) and returns
/// the total number of bytes consumed.
pub(crate) fn consume_line_endings(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            i += 2;
        } else if bytes[i] == b'\n' {
            i += 1;
        } else {
            break;
        }
    }
    i
}

// ── Link type conversion ─────────────────────────────────

/// Converts a pulldown-cmark `LinkType` to our `LinkType`.
pub(crate) fn convert_link_type(link_type: &pulldown_cmark::LinkType) -> super::LinkType {
    use pulldown_cmark::LinkType as PcLinkType;
    match link_type {
        PcLinkType::Inline | PcLinkType::ReferenceUnknown | PcLinkType::ShortcutUnknown => super::LinkType::Inline,
        PcLinkType::Reference | PcLinkType::Collapsed | PcLinkType::CollapsedUnknown | PcLinkType::Shortcut => super::LinkType::Reference,
        PcLinkType::Autolink | PcLinkType::Email => super::LinkType::Autolink,
        PcLinkType::WikiLink { .. } => super::LinkType::Wikilink,
    }
}

// ── Anchor slug generation ──────────────────────────────

/// Generates a GitHub-compatible anchor slug from heading text.
///
/// Algorithm:
/// 1. Lowercase the text.
/// 2. Strip HTML tags.
/// 3. Replace any non-alphanumeric characters (except spaces and hyphens)
///    with nothing.
/// 4. Replace spaces with hyphens.
/// 5. Collapse multiple hyphens into one.
/// 6. Strip leading and trailing hyphens.
/// 7. If the resulting slug already exists (tracked via `used`),
///    append `-1`, `-2`, etc.
pub(crate) fn generate_anchor(text: &str, used: &mut HashSet<String>) -> String {
    let lower = text.to_lowercase();

    // Strip HTML tags (e.g., `<code>` → ``)
    let no_html = strip_html_tags(&lower);

    // Replace non-alphanumeric chars (except spaces and hyphens) with empty string,
    // and then collapse.
    let slug_base: String = no_html
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");

    // Collapse multiple hyphens
    let mut collapsed = String::with_capacity(slug_base.len());
    let mut prev_hyphen = false;
    for ch in slug_base.chars() {
        if ch == '-' {
            if !prev_hyphen {
                collapsed.push('-');
                prev_hyphen = true;
            }
        } else {
            collapsed.push(ch);
            prev_hyphen = false;
        }
    }

    // Strip leading/trailing hyphens
    let slug = collapsed.trim_matches('-').to_string();

    // Deduplicate
    if slug.is_empty() {
        return slug;
    }

    let mut candidate = slug.clone();
    let mut counter = 1;
    while used.contains(&candidate) {
        counter += 1;
        candidate = format!("{slug}-{counter}");
    }
    used.insert(candidate.clone());
    candidate
}

/// Strips HTML tags from a string (everything between `<` and `>`).
pub(crate) fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}