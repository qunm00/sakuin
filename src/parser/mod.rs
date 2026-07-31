mod helpers;

pub use helpers::extract_tags_from_frontmatter;
pub(crate) use helpers::{convert_link_type, extract_frontmatter, generate_anchor};

use std::collections::HashSet;

/// Parses a single Markdown file into its structured components.
///
/// The parser handles:
/// - Frontmatter (YAML between `---` delimiters at the top of the file)
/// - ATX headings (`# heading`)
/// - Setext headings (underlined with `===` or `---`)
/// - Standard inline links, reference links, autolinks, and images
/// - Wikilinks (`[[target]]` and `[[target|label]]`)
/// - Full-text body extraction
pub struct Parser;

impl Parser {
    pub fn new() -> Self {
        Self
    }

    /// Parse the given Markdown source and return extracted data.
    ///
    /// `source` must be the full, raw file content.
    pub fn parse(&self, source: &str) -> ParseResult {
        let (frontmatter, content_start) = extract_frontmatter(source);

        // Strip frontmatter for pulldown-cmark parsing so it doesn't
        // get interpreted as regular Markdown content.
        let body_source = &source[content_start..];

        // --- pulldown-cmark parsing ---
        // Use ENABLE_WIKILINKS so that `[[target]]` and `[[target|label]]`
        // patterns are natively parsed into Tag::Link with LinkType::WikiLink.
        let mut options = pulldown_cmark::Options::empty();
        options.insert(pulldown_cmark::Options::ENABLE_WIKILINKS);
        let parser = pulldown_cmark::Parser::new_ext(body_source, options);

        let mut headings: Vec<Heading> = Vec::new();
        let mut links: Vec<Link> = Vec::new();
        let mut body_parts: Vec<String> = Vec::new();
        let mut anchor_counts: HashSet<String> = HashSet::new();

        // State machine for tracking nested tag context.
        let mut in_heading: Option<(u8, Vec<String>, usize)> = None;
        let mut in_link: Option<(LinkType, String, Vec<String>, usize)> = None;
        let mut in_image: Option<(String, Vec<String>, usize)> = None;

        for event in parser {
            match event {
                pulldown_cmark::Event::Start(tag) => match tag {
                    pulldown_cmark::Tag::Heading {
                        level, ..
                    } => {
                        let numeric_level = match level {
                            pulldown_cmark::HeadingLevel::H1 => 1,
                            pulldown_cmark::HeadingLevel::H2 => 2,
                            pulldown_cmark::HeadingLevel::H3 => 3,
                            pulldown_cmark::HeadingLevel::H4 => 4,
                            pulldown_cmark::HeadingLevel::H5 => 5,
                            pulldown_cmark::HeadingLevel::H6 => 6,
                        };
                        // Estimate position: use the byte offset from the processed source.
                        // For simplicity we approximate; precise offset tracking can be
                        // added in a later phase.
                        let position = content_start; // placeholder — improved in follow-up
                        in_heading = Some((numeric_level, Vec::new(), position));
                    }

                    pulldown_cmark::Tag::Link {
                        link_type,
                        dest_url,
                        ..
                    } => {
                        let (our_type, target) = if matches!(
                            link_type,
                            pulldown_cmark::LinkType::WikiLink { .. }
                        ) {
                            (LinkType::Wikilink, dest_url.to_string())
                        } else {
                            let our_type = convert_link_type(&link_type);
                            (our_type, dest_url.to_string())
                        };
                        let position = content_start; // placeholder
                        in_link = Some((our_type, target, Vec::new(), position));
                    }

                    pulldown_cmark::Tag::Image {
                        link_type: _,
                        dest_url,
                        ..
                    } => {
                        let position = content_start; // placeholder
                        in_image = Some((dest_url.to_string(), Vec::new(), position));
                    }

                    _ => {}
                },

                pulldown_cmark::Event::End(tag_end) => match tag_end {
                    pulldown_cmark::TagEnd::Heading(..) => {
                        if let Some((level, text_parts, position)) = in_heading.take() {
                            let raw_text = text_parts.concat();
                            let text = raw_text.trim().to_string();
                            if !text.is_empty() {
                                let anchor = generate_anchor(&text, &mut anchor_counts);
                                headings.push(Heading {
                                    level,
                                    text,
                                    anchor,
                                    position,
                                });
                            }
                        }
                    }

                    pulldown_cmark::TagEnd::Image => {
                        if let Some((target, text_parts, position)) = in_image.take() {
                            let text = if text_parts.is_empty() {
                                None
                            } else {
                                Some(text_parts.concat())
                            };
                            links.push(Link {
                                link_type: LinkType::Image,
                                target,
                                text,
                                position,
                            });
                        }
                    }

                    pulldown_cmark::TagEnd::Link => {
                        if let Some((link_type, target, text_parts, position)) = in_link.take() {
                            // Autolinks (e.g. `<https://example.com>`) have no separate
                            // display text — the URL is both target and text.
                            let text = if matches!(link_type, LinkType::Autolink) {
                                None
                            } else if text_parts.is_empty() {
                                None
                            } else {
                                Some(text_parts.concat())
                            };
                            links.push(Link {
                                link_type,
                                target,
                                text,
                                position,
                            });
                        }
                    }

                    _ => {}
                },

                pulldown_cmark::Event::Text(text) => {
                    let text_str = text.to_string();
                    body_parts.push(text_str.clone());

                    if let Some((_, ref mut text_parts, _)) = in_heading {
                        text_parts.push(text_str.clone());
                    }
                    if let Some((_, _, ref mut text_parts, _)) = in_link {
                        text_parts.push(text_str.clone());
                    }
                    if let Some((_, ref mut text_parts, _)) = in_image {
                        text_parts.push(text_str.clone());
                    }
                }

                pulldown_cmark::Event::Code(text) => {
                    let code_text = text.to_string();
                    body_parts.push(code_text.clone());
                    
                    if let Some((_, ref mut text_parts, _)) = in_heading {
                        text_parts.push(code_text.clone());
                    }
                }

                pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => {
                    body_parts.push(" ".to_string());
                }

                _ => {}
            }
        }

        // Add any leftover unclosed context
        if let Some((level, text_parts, position)) = in_heading {
            let raw_text = text_parts.concat();
            let text = raw_text.trim().to_string();
            if !text.is_empty() {
                let anchor = generate_anchor(&text, &mut anchor_counts);
                headings.push(Heading {
                    level,
                    text,
                    anchor,
                    position,
                });
            }
        }
        if let Some((link_type, target, text_parts, position)) = in_link {
            let text = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.concat())
            };
            links.push(Link {
                link_type,
                target,
                text,
                position,
            });
        }
        if let Some((target, text_parts, position)) = in_image {
            let text = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.concat())
            };
            links.push(Link {
                link_type: LinkType::Image,
                target,
                text,
                position,
            });
        }

        let tags = extract_tags_from_frontmatter(frontmatter.as_deref());

        ParseResult {
            frontmatter,
            tags,
            headings,
            links,
            body: body_parts.concat(),
        }
    }
}

// ── Data types ─────────────────────────────────────────

/// The result of parsing a single Markdown file.
#[derive(Debug)]
pub struct ParseResult {
    pub frontmatter: Option<String>,
    pub tags: Vec<String>,
    pub headings: Vec<Heading>,
    pub links: Vec<Link>,
    pub body: String,
}

/// A heading found in a Markdown file.
#[derive(Debug)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub anchor: String,
    pub position: usize,
}

/// A link found in a Markdown file.
#[derive(Debug)]
pub struct Link {
    pub link_type: LinkType,
    pub target: String,
    pub text: Option<String>,
    pub position: usize,
}

/// The type of a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkType {
    Inline,
    Reference,
    Wikilink,
    Autolink,
    Image,
}

impl LinkType {
    /// Return the string representation used in the SQLite schema.
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkType::Inline => "inline",
            LinkType::Reference => "reference",
            LinkType::Wikilink => "wikilink",
            LinkType::Autolink => "autolink",
            LinkType::Image => "image",
        }
    }
}

// ── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Frontmatter ────────────────────────────────────

    #[test]
    fn parse_no_frontmatter() {
        let source = "# Hello\n\nThis is a paragraph.";
        let result = Parser::new().parse(source);
        assert!(result.frontmatter.is_none());
    }

    #[test]
    fn parse_with_frontmatter() {
        let source = "---\ntitle: Test\ntags:\n  - rust\n  - markdown\n---\n\n# Hello";
        let result = Parser::new().parse(source);
        assert_eq!(
            result.frontmatter.as_deref().unwrap(),
            "title: Test\ntags:\n  - rust\n  - markdown"
        );
        assert_eq!(result.tags, vec!["rust", "markdown"]);
    }

    #[test]
    fn parse_frontmatter_tags_single() {
        let source = "---\ntags: justone\n---\n\n# Hello";
        let result = Parser::new().parse(source);
        assert_eq!(result.tags, vec!["justone"]);
    }

    #[test]
    fn parse_frontmatter_tags_flow() {
        let source = "---\ntags: [tag1, tag2]\n---\n\n# Hello";
        let result = Parser::new().parse(source);
        assert_eq!(result.tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn parse_frontmatter_no_tags() {
        let source = "---\ntitle: Hello\n---\n\n# World";
        let result = Parser::new().parse(source);
        assert!(result.tags.is_empty());
    }

    #[test]
    fn parse_no_frontmatter_tags_empty() {
        let result = Parser::new().parse("# Hello");
        assert!(result.tags.is_empty());
    }

    #[test]
    fn parse_empty_frontmatter() {
        let source = "---\n---\n\n# Hello";
        let result = Parser::new().parse(source);
        assert!(result.frontmatter.is_none());
    }

    #[test]
    fn parse_frontmatter_without_closing() {
        // If there's no closing `---`, it's not treated as frontmatter.
        let source = "---\ntitle: Test\n\n# Hello";
        let result = Parser::new().parse(source);
        assert!(result.frontmatter.is_none());
    }

    #[test]
    fn parse_frontmatter_crlf() {
        let source = "---\r\ntitle: Test\r\n---\r\n\r\n# Hello";
        let result = Parser::new().parse(source);
        assert_eq!(result.frontmatter.as_deref().unwrap(), "title: Test");
        assert_eq!(result.headings.len(), 1);
        assert_eq!(result.headings[0].text, "Hello");
    }

    #[test]
    fn parse_frontmatter_multiple_blank_lines() {
        let source = "---\ntitle: Test\n---\n\n\n\n# Hello";
        let result = Parser::new().parse(source);
        assert_eq!(result.frontmatter.as_deref().unwrap(), "title: Test");
        assert_eq!(result.headings.len(), 1);
        assert_eq!(result.headings[0].text, "Hello");
    }

    #[test]
    fn parse_frontmatter_content_immediately_after_closing() {
        // No blank line between closing --- and content
        let source = "---\ntitle: Test\n---\n# Hello";
        let result = Parser::new().parse(source);
        assert_eq!(result.frontmatter.as_deref().unwrap(), "title: Test");
        assert_eq!(result.headings.len(), 1);
        assert_eq!(result.headings[0].text, "Hello");
    }

    // ── Headings ───────────────────────────────────────

    #[test]
    fn parse_atx_headings() {
        let source = "# Heading 1\n\n## Heading 2\n\n### Heading 3";
        let result = Parser::new().parse(source);
        assert_eq!(result.headings.len(), 3);
        assert_eq!(result.headings[0].level, 1);
        assert_eq!(result.headings[0].text, "Heading 1");
        assert_eq!(result.headings[1].level, 2);
        assert_eq!(result.headings[1].text, "Heading 2");
        assert_eq!(result.headings[2].level, 3);
        assert_eq!(result.headings[2].text, "Heading 3");
    }

    #[test]
    fn parse_heading_with_inline_formatting() {
        let source = "# Heading with **bold** and `code`";
        let result = Parser::new().parse(source);
        assert_eq!(result.headings.len(), 1);
        assert_eq!(result.headings[0].text, "Heading with bold and code");
    }

    #[test]
    fn parse_multiple_headings_same_level() {
        let source = "# A\n\n# B\n\n# C";
        let result = Parser::new().parse(source);
        assert_eq!(result.headings.len(), 3);
    }

    // ── Anchor slugs ───────────────────────────────────

    #[test]
    fn generate_anchor_simple() {
        let mut used = HashSet::new();
        let slug = generate_anchor("Hello World", &mut used);
        assert_eq!(slug, "hello-world");
    }

    #[test]
    fn generate_anchor_strips_punctuation() {
        let mut used = HashSet::new();
        let slug = generate_anchor("Hello, World! How's it going?", &mut used);
        assert_eq!(slug, "hello-world-hows-it-going");
    }

    #[test]
    fn generate_anchor_deduplicates() {
        let mut used = HashSet::new();
        let slug1 = generate_anchor("Hello World", &mut used);
        let slug2 = generate_anchor("Hello World", &mut used);
        assert_eq!(slug1, "hello-world");
        assert_eq!(slug2, "hello-world-2");
    }

    #[test]
    fn generate_anchor_collapses_whitespace() {
        let mut used = HashSet::new();
        let slug = generate_anchor("Hello    World", &mut used);
        assert_eq!(slug, "hello-world");
    }

    #[test]
    fn generate_anchor_strips_html() {
        let mut used = HashSet::new();
        let slug = generate_anchor("Hello <code>World</code>", &mut used);
        assert_eq!(slug, "hello-world");
    }

    #[test]
    fn generate_anchor_trims_dashes() {
        let mut used = HashSet::new();
        let slug = generate_anchor(" - Hello World - ", &mut used);
        assert_eq!(slug, "hello-world");
    }

    // ── Links ──────────────────────────────────────────

    #[test]
    fn parse_inline_links() {
        let source = "A [link](https://example.com) here.";
        let result = Parser::new().parse(source);
        assert_eq!(result.links.len(), 1);
        assert_eq!(result.links[0].link_type, LinkType::Inline);
        assert_eq!(result.links[0].target, "https://example.com");
        assert_eq!(result.links[0].text.as_deref(), Some("link"));
    }

    #[test]
    fn parse_reference_links() {
        let source = "[ref]: https://example.com\n\nA [ref] link.\n";
        let result = Parser::new().parse(source);
        assert_eq!(result.links.len(), 1);
        assert_eq!(result.links[0].link_type, LinkType::Reference);
        assert_eq!(result.links[0].target, "https://example.com");
        assert_eq!(result.links[0].text.as_deref(), Some("ref"));
    }

    #[test]
    fn parse_autolinks() {
        let source = "See <https://example.com> for more.";
        let result = Parser::new().parse(source);
        assert_eq!(result.links.len(), 1);
        assert_eq!(result.links[0].link_type, LinkType::Autolink);
        assert_eq!(result.links[0].target, "https://example.com");
        assert_eq!(result.links[0].text, None);
    }

    #[test]
    fn parse_images() {
        let source = "![alt text](image.png)";
        let result = Parser::new().parse(source);
        assert_eq!(result.links.len(), 1);
        assert_eq!(result.links[0].link_type, LinkType::Image);
        assert_eq!(result.links[0].target, "image.png");
        assert_eq!(result.links[0].text.as_deref(), Some("alt text"));
    }

    // ── Wikilinks ──────────────────────────────────────

    #[test]
    fn parse_wikilink_basic() {
        let source = "Link to [[target-page]] here.";
        let result = Parser::new().parse(source);
        let wl: Vec<_> = result.links.iter().filter(|l| l.link_type == LinkType::Wikilink).collect();
        assert_eq!(wl.len(), 1, "expected one wikilink");
        assert_eq!(wl[0].target, "target-page");
        assert_eq!(wl[0].text.as_deref(), Some("target-page"));
    }

    #[test]
    fn parse_wikilink_with_alias() {
        let source = "Link to [[target|display text]] here.";
        let result = Parser::new().parse(source);
        let wl: Vec<_> = result.links.iter().filter(|l| l.link_type == LinkType::Wikilink).collect();
        assert_eq!(wl.len(), 1);
        assert_eq!(wl[0].target, "target");
        assert_eq!(wl[0].text.as_deref(), Some("display text"));
    }

    #[test]
    fn parse_multiple_wikilinks() {
        let source = "[[page-a]] and [[page-b|label]]";
        let result = Parser::new().parse(source);
        let wl: Vec<_> = result.links.iter().filter(|l| l.link_type == LinkType::Wikilink).collect();
        assert_eq!(wl.len(), 2);
        assert_eq!(wl[0].target, "page-a");
        assert_eq!(wl[1].target, "page-b");
    }

    #[test]
    fn parse_wikilinks_mixed_with_regular_links() {
        let source = "A [[wiki]] and a [normal](https://example.com) link.";
        let result = Parser::new().parse(source);
        let wl: Vec<_> = result.links.iter().filter(|l| l.link_type == LinkType::Wikilink).collect();
        let normal: Vec<_> = result.links.iter().filter(|l| l.link_type == LinkType::Inline).collect();
        assert_eq!(wl.len(), 1);
        assert_eq!(normal.len(), 1);
    }

    #[test]
    fn parse_wikilink_empty() {
        // Empty target should be ignored
        let source = "[[]]";
        let result = Parser::new().parse(source);
        let wl: Vec<_> = result.links.iter().filter(|l| l.link_type == LinkType::Wikilink).collect();
        assert!(wl.is_empty(), "empty wikilink should be ignored");
    }

    // ── Body text ──────────────────────────────────────

    #[test]
    fn parse_body_text() {
        let source = "# Title\n\nA paragraph with *emphasis* and `code`.";
        let result = Parser::new().parse(source);
        assert!(result.body.contains("Title"));
        assert!(result.body.contains("paragraph"));
        assert!(result.body.contains("emphasis"));
        assert!(result.body.contains("code"));
    }

    #[test]
    fn parse_empty_document() {
        let result = Parser::new().parse("");
        assert!(result.frontmatter.is_none());
        assert!(result.tags.is_empty());
        assert!(result.headings.is_empty());
        assert!(result.links.is_empty());
        assert!(result.body.is_empty());
    }

    // ── Combined ───────────────────────────────────────

    #[test]
    fn parse_complex_document() {
        let source = r#"---
title: My Doc
tags:
  - test
---

# Introduction

This is a [[wikilink]] and a [normal link](https://example.com).

## Details

Some more text with `code`.

![image](photo.png)

See <https://auto.link> for more.
"#;
        let result = Parser::new().parse(source);

        // Frontmatter
        assert!(result.frontmatter.is_some());
        assert_eq!(result.tags, vec!["test"]);

        // Headings
        assert_eq!(result.headings.len(), 2);
        assert_eq!(result.headings[0].text, "Introduction");
        assert_eq!(result.headings[1].text, "Details");

        // Links
        assert_eq!(
            result.links.iter().filter(|l| l.link_type == LinkType::Wikilink).count(),
            1
        );
        assert_eq!(
            result.links.iter().filter(|l| l.link_type == LinkType::Inline).count(),
            1
        );
        assert_eq!(
            result.links.iter().filter(|l| l.link_type == LinkType::Image).count(),
            1
        );
        assert_eq!(
            result.links.iter().filter(|l| l.link_type == LinkType::Autolink).count(),
            1
        );

        // Body
        assert!(result.body.contains("Introduction"));
        assert!(result.body.contains("wikilink"));
        assert!(result.body.contains("normal link"));
        assert!(result.body.contains("image"));
        assert!(result.body.contains("auto.link"));
    }

    // ── Frontmatter extractor tests ────────────────────

    #[test]
    fn extract_frontmatter_simple() {
        let source = "---\ntitle: Test\n---\n\n# body";
        let (fm, start) = extract_frontmatter(source);
        assert_eq!(fm.as_deref().unwrap(), "title: Test");
        assert_eq!(&source[start..], "# body");
    }

    #[test]
    fn extract_frontmatter_no_frontmatter() {
        let source = "# Just content";
        let (fm, start) = extract_frontmatter(source);
        assert!(fm.is_none());
        assert_eq!(start, 0);
    }

    #[test]
    fn extract_frontmatter_triple_dash_in_content() {
        let source = "---\ntitle: Test\n---\n\n# body with --- inside";
        let (fm, start) = extract_frontmatter(source);
        assert_eq!(fm.as_deref().unwrap(), "title: Test");
        assert_eq!(&source[start..], "# body with --- inside");
    }

    #[test]
    fn extract_frontmatter_crlf_roundtrip() {
        let source = "---\r\ntitle: Test\r\n---\r\n\r\n# body";
        let (fm, start) = extract_frontmatter(source);
        assert_eq!(fm.as_deref().unwrap(), "title: Test");
        assert_eq!(&source[start..], "# body");
    }

    #[test]
    fn extract_frontmatter_with_extra_blank_lines() {
        let source = "---\ntitle: Test\n---\n\n\n\n# body";
        let (fm, start) = extract_frontmatter(source);
        assert_eq!(fm.as_deref().unwrap(), "title: Test");
        assert_eq!(&source[start..], "# body");
    }

    #[test]
    fn extract_frontmatter_no_trailing_newline() {
        let source = "---\ntitle: Test\n---# body";
        let (fm, start) = extract_frontmatter(source);
        assert_eq!(fm.as_deref().unwrap(), "title: Test");
        assert_eq!(&source[start..], "# body");
    }

    // ── extract_tags_from_frontmatter ────────────────

    #[test]
    fn extract_tags_yaml_sequence() {
        let tags = extract_tags_from_frontmatter(Some("tags:\n  - rust\n  - markdown"));
        assert_eq!(tags, vec!["rust", "markdown"]);
    }

    #[test]
    fn extract_tags_yaml_flow() {
        let tags = extract_tags_from_frontmatter(Some("tags: [rust, markdown]"));
        assert_eq!(tags, vec!["rust", "markdown"]);
    }

    #[test]
    fn extract_tags_yaml_single() {
        let tags = extract_tags_from_frontmatter(Some("tags: justone"));
        assert_eq!(tags, vec!["justone"]);
    }

    #[test]
    fn extract_tags_none() {
        let tags = extract_tags_from_frontmatter(None);
        assert!(tags.is_empty());
    }

    #[test]
    fn extract_tags_empty_string() {
        let tags = extract_tags_from_frontmatter(Some(""));
        assert!(tags.is_empty());
    }

    #[test]
    fn extract_tags_no_tags_key() {
        let tags = extract_tags_from_frontmatter(Some("title: Hello"));
        assert!(tags.is_empty());
    }

    #[test]
    fn extract_tags_invalid_yaml() {
        let tags = extract_tags_from_frontmatter(Some("tags: [unclosed"));
        assert!(tags.is_empty());
    }
}