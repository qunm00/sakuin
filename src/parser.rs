/// Parses a single Markdown file into its structured components.
pub struct Parser;

impl Parser {
    pub fn new() -> Self {
        Self
    }

    /// Parse the given Markdown source and return extracted data.
    pub fn parse(&self, source: &str) -> ParseResult {
        // TODO: Phase 1 — implement frontmatter, headings, links, text extraction
        ParseResult {
            frontmatter: None,
            headings: Vec::new(),
            links: Vec::new(),
            body: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct ParseResult {
    pub frontmatter: Option<String>,
    pub headings: Vec<Heading>,
    pub links: Vec<Link>,
    pub body: String,
}

#[derive(Debug)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub anchor: String,
    pub position: usize,
}

#[derive(Debug)]
pub struct Link {
    pub link_type: LinkType,
    pub target: String,
    pub text: Option<String>,
    pub position: usize,
}

#[derive(Debug)]
pub enum LinkType {
    Inline,
    Reference,
    Wikilink,
    Autolink,
    Image,
}
