//! Reader body: layout (HTML or text) cached per width, readable colors,
//! inline images and links footer.

use std::collections::HashMap;
use std::io::Cursor;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui_image::sliced::SlicedProtocol;

use orion_core::config::HtmlColors;
use orion_core::model::MessageView;
use orion_html::{LayoutOptions, Rendered, StyledLine};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMode {
    Html,
    Text,
    /// The HTML as drawn by a browser engine (see `snapshot`): one tall image.
    Original,
}

/// Inline image: (index in the document, width, height) in cells.
pub type InlineKey = (usize, u16, u16);

pub enum InlineImage {
    Pending,
    Ready(Box<SlicedProtocol>),
    Failed(String),
}

pub struct Body {
    pub mode: BodyMode,
    /// Per HTML image: the attachment holding it (`cid:`) and its size in pixels.
    pub image_att: Vec<Option<usize>>,
    pub image_px: Vec<Option<(u32, u32)>>,
    pub cache: Option<(u16, BodyMode, Rendered)>,
    pub links: Vec<String>,
    pub inline: HashMap<InlineKey, InlineImage>,
    /// Browser rendering of the HTML (PNG and its pixel size), once made.
    pub snapshot: Option<(Vec<u8>, (u32, u32))>,
    snapshot_doc: Option<orion_html::Doc>,
}

impl Body {
    pub fn new(view: &MessageView, prefer_html: bool) -> Body {
        let (image_att, image_px) = match &view.html_doc {
            Some(doc) => doc
                .images
                .iter()
                .map(|img| {
                    let att = view.attachments.iter().position(|a| {
                        a.is_image()
                            && (img.content_id().is_some_and(|cid| a.content_id.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(cid)))
                                || (!img.src.is_empty() && a.name == img.src))
                    });
                    let px = att.and_then(|i| {
                        image::ImageReader::new(Cursor::new(&view.attachments[i].data))
                            .with_guessed_format()
                            .ok()?
                            .into_dimensions()
                            .ok()
                    });
                    (att, px)
                })
                .unzip(),
            None => (Vec::new(), Vec::new()),
        };
        let mode = if prefer_html && view.html_doc.is_some() { BodyMode::Html } else { BodyMode::Text };
        Body { mode, image_att, image_px, cache: None, links: Vec::new(), inline: HashMap::new(), snapshot: None, snapshot_doc: None }
    }

    /// Image attachments that are not already shown inside the displayed HTML.
    pub fn pane_images(&self, view: &MessageView) -> Vec<usize> {
        view.attachments
            .iter()
            .enumerate()
            .filter(|(i, a)| a.is_image() && !(self.mode != BodyMode::Text && self.image_att.contains(&Some(*i))))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn toggle_mode(&mut self, view: &MessageView) -> bool {
        if view.html_doc.is_none() {
            return false;
        }
        self.mode = match self.mode {
            BodyMode::Html => BodyMode::Text,
            BodyMode::Text | BodyMode::Original => BodyMode::Html,
        };
        self.cache = None;
        self.inline.clear();
        true
    }

    /// Shows the browser rendering (`Some`) or goes back to the HTML view.
    pub fn set_original(&mut self, snapshot: Option<(Vec<u8>, (u32, u32))>) {
        if let Some(s) = snapshot {
            self.snapshot = Some(s);
        }
        self.mode = if self.mode == BodyMode::Original || self.snapshot.is_none() { BodyMode::Html } else { BodyMode::Original };
        if self.mode == BodyMode::Original && self.snapshot_doc.is_none() {
            self.snapshot_doc = Some(orion_html::Doc::parse(r#"<img src="cid:orion-original" alt="original view">"#));
        }
        self.cache = None;
        self.inline.clear();
    }

    /// Bytes of a document image: the snapshot in the original view, else an
    /// attachment of the message.
    pub fn image_data(&self, view: &MessageView, image: usize) -> Option<Vec<u8>> {
        if self.mode == BodyMode::Original {
            return self.snapshot.as_ref().map(|s| s.0.clone());
        }
        let att = self.image_att.get(image).copied().flatten()?;
        Some(view.attachments[att].data.clone())
    }

    /// Lays out for `width` if needed.
    pub fn ensure_layout(&mut self, view: &MessageView, width: u16, cell: (u16, u16), max_rows: u16, markers: bool) {
        let fresh = self.cache.as_ref().is_some_and(|(w, m, _)| *w == width && *m == self.mode);
        if !fresh {
            let original = self.mode == BodyMode::Original;
            let doc = match (self.mode, &view.html_doc, &self.snapshot_doc) {
                (BodyMode::Original, _, Some(d)) => d,
                (BodyMode::Html, Some(d), _) => d,
                _ => &view.text_doc,
            };
            let px = &self.image_px;
            let snap = self.snapshot.as_ref().map(|s| s.1);
            let lookup = |i: usize| if original { snap } else { px.get(i).copied().flatten() };
            // The rendering is as tall as the message: no height cap.
            let max_rows = if original { u16::MAX } else { max_rows };
            let opts = LayoutOptions { cell, max_image_rows: max_rows, link_markers: markers, image_px: &lookup };
            let mut r = doc.layout(width, &opts);
            // Links stay usable in the original view: those of the HTML.
            let links_doc = if original { view.html_doc.as_ref().unwrap_or(doc) } else { doc };
            self.links = links_doc.links.clone();
            if !self.links.is_empty() {
                let dim = orion_html::TextStyle { dim: true, ..Default::default() };
                let span = |t: String, bold: bool| orion_html::Span { text: t, style: orion_html::TextStyle { bold, ..dim.clone() } };
                r.lines.push(StyledLine::default());
                r.lines.push(StyledLine { spans: vec![span("Links (o + number to open)".into(), true)] });
                for (i, url) in self.links.iter().enumerate() {
                    r.lines.push(StyledLine { spans: vec![span(format!("[{}] {url}", i + 1), false)] });
                }
            }
            self.cache = Some((width, self.mode, r));
        }
    }

    /// Width (in cells) of the latest layout.
    pub fn width(&self) -> Option<u16> {
        self.cache.as_ref().map(|c| c.0)
    }

    /// Latest layout (empty if nothing has been laid out yet).
    pub fn rendered(&self) -> &Rendered {
        static EMPTY: std::sync::OnceLock<Rendered> = std::sync::OnceLock::new();
        self.cache.as_ref().map(|c| &c.2).unwrap_or_else(|| EMPTY.get_or_init(Rendered::default))
    }
}

/// Converts a laid-out line to ratatui, applying the color policy.
pub fn to_line(l: &StyledLine, colors: HtmlColors, term_bg: orion_html::Color) -> Line<'static> {
    let spans = l
        .spans
        .iter()
        .map(|s| {
            let st = &s.style;
            let mut style = Style::new();
            let bg = if colors == HtmlColors::Full { st.bg } else { None };
            let readable = |fg: orion_html::Color| match fg {
                orion_html::Color::Ansi(_) => true,
                rgb => rgb.contrast(bg.unwrap_or(term_bg)).is_none_or(|c| c >= if bg.is_some() { 2.0 } else { 3.0 }),
            };
            if let Some(fg) = st.fg.filter(|&c| colors != HtmlColors::None && readable(c)) {
                style = style.fg(color(fg));
            }
            if let Some(bg) = bg {
                style = style.bg(color(bg));
            }
            let mut m = Modifier::empty();
            m.set(Modifier::BOLD, st.bold);
            m.set(Modifier::ITALIC, st.italic);
            m.set(Modifier::UNDERLINED, st.underline);
            m.set(Modifier::CROSSED_OUT, st.strike);
            m.set(Modifier::DIM, st.dim);
            Span::styled(s.text.clone(), style.add_modifier(m))
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn color(c: orion_html::Color) -> Color {
    match c {
        orion_html::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        // The renderer's basic colours (quote levels, links) take the theme's.
        orion_html::Color::Ansi(n) => {
            let t = crate::theme::theme();
            match n {
                1 | 9 => t.error,
                2 | 10 => t.ok,
                3 | 11 => t.warn,
                4 | 12 => t.info,
                5 | 13 => t.accent,
                6 | 14 => t.accent2,
                8 => t.overlay,
                n => Color::Indexed(n),
            }
        }
    }
}

/// Schemes allowed to be opened from an email.
pub fn safe_url(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    (l.starts_with("https://") || l.starts_with("http://") || l.starts_with("mailto:")) && !url.chars().any(|c| c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orion_html::{Color as HC, TextStyle};

    fn line(fg: HC) -> StyledLine {
        StyledLine { spans: vec![orion_html::Span { text: "x".into(), style: TextStyle { fg: Some(fg), ..Default::default() } }] }
    }

    #[test]
    fn unreadable_colors_are_dropped() {
        let dark = HC::Rgb(20, 20, 20);
        // Black text on a dark terminal: the default color is used.
        assert_eq!(to_line(&line(HC::Rgb(0, 0, 0)), HtmlColors::Auto, dark).spans[0].style.fg, None);
        assert_eq!(to_line(&line(HC::Rgb(255, 80, 80)), HtmlColors::Auto, dark).spans[0].style.fg, Some(Color::Rgb(255, 80, 80)));
        assert_eq!(to_line(&line(HC::Rgb(255, 80, 80)), HtmlColors::None, dark).spans[0].style.fg, None);
        assert_eq!(to_line(&line(HC::Ansi(2)), HtmlColors::Auto, dark).spans[0].style.fg, Some(crate::theme::theme().ok), "basic colours take the theme's");
    }

    #[test]
    fn urls() {
        assert!(safe_url("https://example.com"));
        assert!(safe_url("mailto:a@b.c"));
        assert!(!safe_url("file:///etc/passwd"));
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("https://x\n--evil"));
    }
}
