//! Native HTML renderer for the terminal.
//!
//! 1. [`Doc::parse`] turns HTML (html5ever) into our own tree with CSS styles
//!    already resolved. It is `Send`: it is built on the account thread.
//! 2. [`Doc::layout`] lays that tree out for a given width: word-wrapped
//!    paragraphs, lists, quotes, tables and images as blocks.
//!
//! The result ([`Rendered`]) does not depend on any UI library.

mod build;
pub mod css;
mod layout;

pub use css::{Align, Color};
pub use build::number_links;
pub use layout::LayoutOptions;

/// Style of a text fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub dim: bool,
    /// Index into [`Doc::links`].
    pub link: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: TextStyle,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyledLine {
    pub spans: Vec<Span>,
}

impl StyledLine {
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.text.as_str())).sum()
    }

    /// Plain text of the line (useful for tests and searching).
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Image placed in the laid-out document: it occupies `height` lines starting
/// at `line`, shifted `x` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub line: usize,
    pub x: u16,
    pub width: u16,
    pub height: u16,
    /// Index into [`Doc::images`].
    pub image: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub lines: Vec<StyledLine>,
    pub placements: Vec<Placement>,
}

impl Rendered {
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.text().trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct ImageRef {
    pub src: String,
    pub alt: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl ImageRef {
    /// `cid:` identifier (images embedded in the message itself).
    pub fn content_id(&self) -> Option<&str> {
        self.src.get(..4).filter(|p| p.eq_ignore_ascii_case("cid:")).map(|_| &self.src[4..])
    }
}

/// Document with resolved styles, ready to be laid out at any width.
#[derive(Debug, Clone)]
pub struct Doc {
    pub(crate) root: build::El,
    pub links: Vec<String>,
    pub images: Vec<ImageRef>,
}

impl Doc {
    pub fn parse(html: &str) -> Doc {
        build::parse_html(html)
    }

    /// Plain text as a document: keeps spaces and line breaks, colours quotes
    /// (`>`) by depth and turns URLs into numbered links.
    pub fn from_plain(text: &str) -> Doc {
        build::from_plain(text)
    }

    pub fn layout(&self, width: u16, opts: &LayoutOptions<'_>) -> Rendered {
        layout::layout(self, width.max(10), opts)
    }
}
