//! HTML → our own tree with resolved styles.

use std::collections::HashMap;

use scraper::{ElementRef, Html, Node as HNode, Selector};

use crate::css::{self, parse_align, parse_color, parse_decls, parse_stylesheet, Align, Color, Decls, Display};
use crate::{Doc, ImageRef, TextStyle};

#[derive(Debug, Clone)]
pub(crate) enum Node {
    Text(String),
    El(El),
}

#[derive(Debug, Clone)]
pub(crate) struct El {
    pub kind: Kind,
    pub style: TextStyle,
    pub align: Align,
    pub pre: bool,
    /// Behaves as a block (by its kind or because it contains blocks).
    pub block: bool,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Block { margin: bool },
    Heading,
    Inline,
    Br,
    Hr,
    Img(usize),
    Link(usize),
    Quote,
    List { ordered: bool, start: u32 },
    Item,
    Table { border: bool },
    RowGroup,
    Row,
    Cell { colspan: u16 },
}

impl Kind {
    fn is_block(self) -> bool {
        !matches!(self, Kind::Inline | Kind::Br | Kind::Img(_) | Kind::Link(_))
    }
}

/// What is inherited from parents to children.
#[derive(Clone, Default)]
struct Inherited {
    style: TextStyle,
    align: Align,
    pre: bool,
    /// Text is invisible here (font-size ≤ 1px) unless a descendant resets it.
    tiny: bool,
}

struct Ctx {
    matched: HashMap<ego_id::Id, Vec<(u32, u32, u32, usize)>>,
    rules: Vec<css::Rule>,
    links: Vec<String>,
    images: Vec<ImageRef>,
}

/// `scraper` uses ego-tree; its `NodeId` serves as the key.
mod ego_id {
    pub type Id = ego_tree::NodeId;
}

pub(crate) fn parse_html(html: &str) -> Doc {
    let dom = Html::parse_document(html);
    let mut rules = Vec::new();
    if let Ok(sel) = Selector::parse("style") {
        for st in dom.select(&sel) {
            rules.extend(parse_stylesheet(&st.text().collect::<String>()));
        }
    }
    let mut matched: HashMap<ego_id::Id, Vec<(u32, u32, u32, usize)>> = HashMap::new();
    for (i, rule) in rules.iter().enumerate() {
        let Ok(sel) = Selector::parse(&rule.selector) else { continue };
        for el in dom.select(&sel) {
            let (a, b, c) = rule.specificity;
            matched.entry(el.id()).or_default().push((a, b, c, i));
        }
    }
    let mut ctx = Ctx { matched, rules, links: Vec::new(), images: Vec::new() };
    let body = Selector::parse("body").ok().and_then(|s| dom.select(&s).next()).unwrap_or_else(|| dom.root_element());
    let root = build_el(body, &Inherited::default(), &mut ctx).unwrap_or_else(empty_root);
    Doc { root, links: ctx.links, images: ctx.images }
}

fn empty_root() -> El {
    El { kind: Kind::Block { margin: false }, style: TextStyle::default(), align: Align::Left, pre: false, block: true, children: Vec::new() }
}

fn tag_decls(tag: &str) -> Decls {
    let mut d = Decls::default();
    match tag {
        "b" | "strong" | "th" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "dt" => d.bold = Some(true),
        "i" | "em" | "cite" | "var" | "dfn" | "address" => d.italic = Some(true),
        "u" | "ins" => d.underline = Some(true),
        "s" | "strike" | "del" => d.strike = Some(true),
        "pre" | "listing" | "xmp" | "plaintext" => d.pre = Some(true),
        "center" => d.align = Some(Align::Center),
        "mark" => d.background = Some(Some(Color::Rgb(255, 255, 0))),
        _ => {}
    }
    if tag == "th" {
        d.align = Some(Align::Center);
    }
    d
}

fn attr_decls(el: &ElementRef<'_>, tag: &str) -> Decls {
    let mut d = Decls::default();
    let v = el.value();
    if let Some(c) = v.attr("bgcolor").and_then(parse_color) {
        d.background = Some(Some(c));
    }
    if tag == "font"
        && let Some(c) = v.attr("color").and_then(parse_color) {
            d.color = Some(Some(c));
        }
    if let Some(a) = v.attr("align").and_then(parse_align) {
        // On tables and images "align" positions the element, not its text.
        if !matches!(tag, "table" | "img") {
            d.align = Some(a);
        }
    }
    if v.attr("hidden").is_some() {
        d.hidden = Some(true);
    }
    d
}

fn computed_decls(el: &ElementRef<'_>, tag: &str, ctx: &Ctx) -> Decls {
    let mut d = tag_decls(tag);
    d.merge(&attr_decls(el, tag));
    let (inline, inline_imp) = el.value().attr("style").map(parse_decls).unwrap_or_default();
    let mut sheet = ctx.matched.get(&el.id()).cloned().unwrap_or_default();
    sheet.sort();
    for &(_, _, _, i) in &sheet {
        d.merge(&ctx.rules[i].normal);
    }
    d.merge(&inline);
    for &(_, _, _, i) in &sheet {
        d.merge(&ctx.rules[i].important);
    }
    d.merge(&inline_imp);
    d
}

fn kind_for(tag: &str, el: &ElementRef<'_>, ctx: &mut Ctx) -> Option<Kind> {
    let attr = |n: &str| el.value().attr(n);
    Some(match tag {
        "head" | "script" | "style" | "title" | "meta" | "link" | "template" | "noscript" | "svg" | "object" | "iframe"
        | "input" | "select" | "textarea" | "button" | "map" | "area" | "base" => return None,
        "p" | "dl" | "figure" | "pre" | "listing" | "xmp" | "plaintext" | "fieldset" => Kind::Block { margin: true },
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Kind::Heading,
        "div" | "center" | "section" | "article" | "header" | "footer" | "nav" | "aside" | "main" | "body" | "html"
        | "form" | "address" | "dd" | "dt" | "caption" | "figcaption" | "details" | "summary" | "legend" => {
            Kind::Block { margin: false }
        }
        "blockquote" => Kind::Quote,
        "ul" | "menu" | "dir" => Kind::List { ordered: false, start: 1 },
        "ol" => Kind::List { ordered: true, start: attr("start").and_then(|s| s.trim().parse().ok()).unwrap_or(1) },
        "li" => Kind::Item,
        "table" => Kind::Table { border: attr("border").and_then(|b| b.trim().parse::<u32>().ok()).is_some_and(|b| b > 0) },
        "thead" | "tbody" | "tfoot" => Kind::RowGroup,
        "tr" => Kind::Row,
        "td" | "th" => Kind::Cell { colspan: attr("colspan").and_then(|c| c.trim().parse().ok()).unwrap_or(1).clamp(1, 64) },
        "br" => Kind::Br,
        "hr" => Kind::Hr,
        "img" => {
            let dim = |n: &str| attr(n).and_then(|v| v.trim().trim_end_matches("px").parse::<u32>().ok());
            let (w, h) = (dim("width"), dim("height"));
            // Tracking pixels and spacers.
            if w.is_some_and(|w| w <= 2) || h.is_some_and(|h| h <= 2) {
                return None;
            }
            ctx.images.push(ImageRef {
                src: attr("src").unwrap_or_default().trim().to_string(),
                alt: attr("alt").unwrap_or_default().trim().to_string(),
                width: w,
                height: h,
            });
            Kind::Img(ctx.images.len() - 1)
        }
        "a" => match attr("href").map(str::trim) {
            Some(href) if ["http://", "https://", "mailto:"].iter().any(|p| href.len() > p.len() && href[..p.len()].eq_ignore_ascii_case(p)) => {
                ctx.links.push(href.to_string());
                Kind::Link(ctx.links.len() - 1)
            }
            _ => Kind::Inline,
        },
        _ => Kind::Inline,
    })
}

fn build_el(el: ElementRef<'_>, inh: &Inherited, ctx: &mut Ctx) -> Option<El> {
    let tag = el.value().name().to_ascii_lowercase();
    let decls = computed_decls(&el, &tag, ctx);
    if decls.display == Some(Display::None) || decls.hidden == Some(true) {
        return None;
    }
    let mut kind = kind_for(&tag, &el, ctx)?;
    match decls.display {
        Some(Display::Block) if kind == Kind::Inline => kind = Kind::Block { margin: false },
        Some(Display::Inline) if matches!(kind, Kind::Block { .. }) => kind = Kind::Inline,
        _ => {}
    }

    let mut style = inh.style.clone();
    if let Some(c) = decls.color {
        style.fg = c;
    }
    if let Some(bg) = decls.background {
        style.bg = bg;
    }
    macro_rules! set { ($($f:ident),*) => { $( if let Some(v) = decls.$f { style.$f = v; } )* } }
    set!(bold, italic, underline, strike);
    if let Kind::Link(i) = kind {
        style.link = Some(i);
        if decls.underline.is_none() {
            style.underline = true;
        }
        if decls.color.is_none() {
            style.fg = Some(Color::Ansi(12));
        }
    }
    // As in browsers' quirks mode (the usual one for email), tables don't
    // inherit the text alignment around them.
    let inherited_align = if matches!(kind, Kind::Table { .. }) { Align::Left } else { inh.align };
    let next = Inherited {
        style: style.clone(),
        align: decls.align.unwrap_or(inherited_align),
        pre: decls.pre.unwrap_or(inh.pre),
        tiny: decls.tiny.unwrap_or(inh.tiny),
    };

    let mut children = Vec::new();
    for child in el.children() {
        match child.value() {
            HNode::Text(t) if !next.tiny => children.push(Node::Text(t.to_string())),
            HNode::Text(_) => {}
            HNode::Element(_) => {
                if let Some(c) = ElementRef::wrap(child).and_then(|c| build_el(c, &next, ctx)) {
                    children.push(Node::El(c));
                }
            }
            _ => {}
        }
    }
    let block = kind.is_block() || children.iter().any(|c| matches!(c, Node::El(e) if e.block));
    Some(El { kind, style, align: next.align, pre: next.pre, block, children })
}

// ---------------------------------------------------------------- plain text

pub(crate) fn from_plain(text: &str) -> Doc {
    const QUOTE: [u8; 4] = [2, 4, 5, 3]; // green, blue, magenta, yellow
    let mut links = Vec::new();
    let mut children = Vec::new();
    let span = |kind: Kind, style: TextStyle, children: Vec<Node>| {
        Node::El(El { kind, style, align: Align::Left, pre: true, block: false, children })
    };
    for (n, line) in text.lines().enumerate() {
        if n > 0 {
            children.push(span(Kind::Br, TextStyle::default(), Vec::new()));
        }
        let trimmed = line.trim_start();
        let depth = trimmed.chars().take_while(|&c| c == '>' || c == ' ').filter(|&c| c == '>').count();
        let mut base = TextStyle::default();
        if depth > 0 {
            base.fg = Some(Color::Ansi(QUOTE[(depth - 1) % 4]));
        } else if line == "-- " {
            base.dim = true;
        }
        let mut parts = Vec::new();
        let mut rest = line;
        while let Some((start, end)) = find_url(rest) {
            if start > 0 {
                parts.push(Node::Text(rest[..start].to_string()));
            }
            let url = &rest[start..end];
            links.push(url.to_string());
            let mut st = base.clone();
            st.link = Some(links.len() - 1);
            st.underline = true;
            st.fg = Some(Color::Ansi(12));
            parts.push(span(Kind::Link(links.len() - 1), st, vec![Node::Text(url.to_string())]));
            rest = &rest[end..];
        }
        if !rest.is_empty() {
            parts.push(Node::Text(rest.to_string()));
        }
        children.push(span(Kind::Inline, base, parts));
    }
    let root = El { kind: Kind::Block { margin: false }, style: TextStyle::default(), align: Align::Left, pre: true, block: true, children };
    Doc { root, links, images: Vec::new() }
}

/// Finds an http(s) URL in the text; returns (start, end).
fn find_url(s: &str) -> Option<(usize, usize)> {
    let start = ["https://", "http://"].iter().filter_map(|p| s.find(p)).min()?;
    let tail = &s[start..];
    let mut end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"')).unwrap_or(tail.len());
    // Trailing punctuation that is almost never part of the URL.
    while end > 0 && tail[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '\'']) {
        if tail[..end].ends_with(')') && tail[..end].contains('(') {
            break;
        }
        end -= 1;
    }
    (end > 8).then_some((start, start + end))
}
