//! CSS subset that matters for email: colours, weight, style, decoration,
//! display/visibility, alignment and white-space. `<style>` sheets with
//! selectors (no @media), approximate specificity and `!important`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Rgb(u8, u8, u8),
    /// Colour from the terminal's 16-colour palette (0-15).
    Ansi(u8),
}

impl Color {
    /// Relative luminance (WCAG) for contrast checks.
    pub fn luminance(self) -> Option<f32> {
        let Color::Rgb(r, g, b) = self else { return None };
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        Some(0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b))
    }

    pub fn contrast(self, other: Color) -> Option<f32> {
        let (a, b) = (self.luminance()?, other.luminance()?);
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        Some((hi + 0.05) / (lo + 0.05))
    }
}

pub fn parse_color(v: &str) -> Option<Color> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let h = |s: &str| u8::from_str_radix(s, 16).ok();
        return match hex.len() {
            3 | 4 => {
                let c: Vec<u8> = hex.chars().take(3).map(|c| h(&c.to_string()).map(|x| x * 17)).collect::<Option<_>>()?;
                Some(Color::Rgb(c[0], c[1], c[2]))
            }
            6 | 8 => Some(Color::Rgb(h(&hex[0..2])?, h(&hex[2..4])?, h(&hex[4..6])?)),
            _ => None,
        };
    }
    if let Some(args) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")) {
        let args = args.trim_end_matches(')');
        let parts: Vec<&str> = args.split([',', ' ', '/']).filter(|s| !s.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        if parts.len() >= 4 && parts[3].trim().parse::<f32>().is_ok_and(|a| a == 0.0) {
            return None; // fully transparent
        }
        let comp = |s: &str| -> Option<u8> {
            match s.strip_suffix('%') {
                Some(p) => p.trim().parse::<f32>().ok().map(|p| (p.clamp(0.0, 100.0) * 2.55).round() as u8),
                None => s.trim().parse::<f32>().ok().map(|n| n.clamp(0.0, 255.0).round() as u8),
            }
        };
        return Some(Color::Rgb(comp(parts[0])?, comp(parts[1])?, comp(parts[2])?));
    }
    let rgb = match v.as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "navy" => (0, 0, 128),
        "yellow" => (255, 255, 0),
        "orange" => (255, 165, 0),
        "purple" => (128, 0, 128),
        "fuchsia" | "magenta" => (255, 0, 255),
        "aqua" | "cyan" => (0, 255, 255),
        "teal" => (0, 128, 128),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "silver" => (192, 192, 192),
        "gray" | "grey" => (128, 128, 128),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "gainsboro" => (220, 220, 220),
        "whitesmoke" => (245, 245, 245),
        "darkblue" => (0, 0, 139),
        "darkred" => (139, 0, 0),
        "darkgreen" => (0, 100, 0),
        "royalblue" => (65, 105, 225),
        "steelblue" => (70, 130, 180),
        "dodgerblue" => (30, 144, 255),
        "deepskyblue" => (0, 191, 255),
        "skyblue" => (135, 206, 235),
        "lightblue" => (173, 216, 230),
        "crimson" => (220, 20, 60),
        "tomato" => (255, 99, 71),
        "orangered" => (255, 69, 0),
        "gold" => (255, 215, 0),
        "goldenrod" => (218, 165, 32),
        "indigo" => (75, 0, 130),
        "violet" => (238, 130, 238),
        "pink" => (255, 192, 203),
        "hotpink" => (255, 105, 180),
        "brown" => (165, 42, 42),
        "chocolate" => (210, 105, 30),
        "tan" => (210, 180, 140),
        "beige" => (245, 245, 220),
        "ivory" => (255, 255, 240),
        "linen" => (250, 240, 230),
        "seagreen" => (46, 139, 87),
        "forestgreen" => (34, 139, 34),
        "limegreen" => (50, 205, 50),
        "slategray" | "slategrey" => (112, 128, 144),
        "lightslategray" | "lightslategrey" => (119, 136, 153),
        "darkslategray" | "darkslategrey" => (47, 79, 79),
        _ => return None,
    };
    Some(Color::Rgb(rgb.0, rgb.1, rgb.2))
}

/// Properties we care about, already interpreted. `None` = not specified.
#[derive(Debug, Clone, Default)]
pub struct Decls {
    pub color: Option<Option<Color>>,
    pub background: Option<Option<Color>>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub display: Option<Display>,
    pub hidden: Option<bool>,
    /// `font-size` ≤ 1px: this element's own text is invisible, but children
    /// that set a real size again are shown (email templates put
    /// `font-size:0` on column wrappers to remove the gaps between columns).
    pub tiny: Option<bool>,
    pub align: Option<Align>,
    pub pre: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    /// table, table-row...: the element's own kind is kept.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

pub fn parse_align(v: &str) -> Option<Align> {
    match v.trim().to_ascii_lowercase().as_str() {
        "left" | "start" | "justify" => Some(Align::Left),
        "center" | "middle" | "-webkit-center" => Some(Align::Center),
        "right" | "end" => Some(Align::Right),
        _ => None,
    }
}

impl Decls {
    /// Overlays `other` on top of `self`.
    pub fn merge(&mut self, other: &Decls) {
        macro_rules! m { ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* } }
        m!(color, background, bold, italic, underline, strike, display, hidden, tiny, align, pre);
    }
}

/// Parses a declaration list. Returns (normal, !important).
pub fn parse_decls(text: &str) -> (Decls, Decls) {
    let mut normal = Decls::default();
    let mut important = Decls::default();
    // Several properties combined decide whether a block is hidden (the "preheader" trick).
    let mut zero_height = false;
    let mut overflow_hidden = false;
    for decl in text.split(';') {
        let Some((prop, value)) = decl.split_once(':') else { continue };
        let prop = prop.trim().to_ascii_lowercase();
        let mut value = value.trim().to_string();
        let target = if let Some(v) = value.strip_suffix("!important").or_else(|| value.strip_suffix("! important")) {
            value = v.trim().to_string();
            &mut important
        } else {
            &mut normal
        };
        let lv = value.to_ascii_lowercase();
        match prop.as_str() {
            "color" => {
                if !matches!(lv.as_str(), "inherit" | "initial" | "unset" | "currentcolor") {
                    target.color = Some(parse_color(&lv));
                }
            }
            "background-color" | "background" => {
                let c = lv.split_whitespace().find_map(parse_color).or_else(|| parse_color(&lv));
                if c.is_some() || lv.contains("transparent") || lv == "none" {
                    target.background = Some(c);
                }
            }
            "font-weight" => {
                target.bold = Some(match lv.as_str() {
                    "bold" | "bolder" => true,
                    "normal" | "lighter" => false,
                    n => n.parse::<u32>().is_ok_and(|n| n >= 600),
                })
            }
            "font-style" => target.italic = Some(lv.contains("italic") || lv.contains("oblique")),
            "font" => {
                if lv.contains("bold") {
                    target.bold = Some(true);
                }
                if lv.contains("italic") {
                    target.italic = Some(true);
                }
            }
            "text-decoration" | "text-decoration-line" => {
                target.underline = Some(lv.contains("underline"));
                target.strike = Some(lv.contains("line-through"));
            }
            "display" => {
                target.display = Some(match lv.as_str() {
                    "none" => Display::None,
                    "block" | "flex" | "grid" | "list-item" | "flow-root" => Display::Block,
                    "inline" | "inline-block" | "inline-flex" => Display::Inline,
                    _ => Display::Other,
                })
            }
            "visibility" => target.hidden = Some(lv == "hidden" || lv == "collapse"),
            "opacity" => {
                if lv.parse::<f32>().is_ok_and(|o| o == 0.0) {
                    target.hidden = Some(true);
                }
            }
            "mso-hide" => {
                if lv == "all" {
                    target.hidden = Some(true);
                }
            }
            "font-size" => {
                let n: String = lv.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                if let Ok(n) = n.parse::<f32>() {
                    target.tiny = Some(n <= 1.0 && !lv.ends_with("em") && !lv.ends_with('%'));
                }
            }
            "max-height" | "height" => {
                if lv.trim_end_matches("px") == "0" {
                    zero_height = true;
                }
            }
            "overflow" => overflow_hidden = lv == "hidden",
            "text-align" => {
                if let Some(a) = parse_align(&lv) {
                    target.align = Some(a);
                }
            }
            "white-space" => target.pre = Some(lv.starts_with("pre")),
            _ => {}
        }
    }
    if zero_height && overflow_hidden {
        normal.hidden = Some(true);
    }
    (normal, important)
}

/// A stylesheet rule.
pub struct Rule {
    pub selector: String,
    pub specificity: (u32, u32, u32),
    pub normal: Decls,
    pub important: Decls,
}

/// Splits a stylesheet into rules, ignoring @media, @font-face, etc.
pub fn parse_stylesheet(css: &str) -> Vec<Rule> {
    let css = strip_comments(css);
    let mut rules = Vec::new();
    let mut rest = css.as_str();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if rest.starts_with('@') {
            // @import ...; or @media {...} with nested braces.
            let semi = rest.find(';');
            let brace = rest.find('{');
            match (semi, brace) {
                (Some(s), Some(b)) if s < b => rest = &rest[s + 1..],
                (_, Some(b)) => rest = skip_block(&rest[b..]),
                (Some(s), None) => rest = &rest[s + 1..],
                (None, None) => break,
            }
            continue;
        }
        let Some(open) = rest.find('{') else { break };
        let Some(close) = rest[open..].find('}') else { break };
        let selectors = &rest[..open];
        let body = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let (normal, important) = parse_decls(body);
        for sel in selectors.split(',') {
            let sel = sel.trim();
            // Dynamic pseudo-classes and pseudo-elements don't apply to a static document.
            if sel.is_empty() || sel.contains("::") || [":hover", ":active", ":focus", ":visited"].iter().any(|p| sel.contains(p)) {
                continue;
            }
            rules.push(Rule { selector: sel.to_string(), specificity: specificity(sel), normal: normal.clone(), important: important.clone() });
        }
    }
    rules
}

fn skip_block(s: &str) -> &str {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i + 2..].find("*/").map(|j| &rest[i + 2 + j + 2..]).unwrap_or("");
    }
    out.push_str(rest);
    // <style> contents are sometimes wrapped in <!-- -->.
    out.replace("<!--", "").replace("-->", "")
}

/// Approximate specificity: (ids, classes/attributes/pseudo-classes, types).
pub fn specificity(sel: &str) -> (u32, u32, u32) {
    let (mut a, mut b, mut c) = (0, 0, 0);
    let mut prev_boundary = true;
    for ch in sel.chars() {
        match ch {
            '#' => a += 1,
            '.' | '[' | ':' => b += 1,
            ch if ch.is_ascii_alphabetic() && prev_boundary => c += 1,
            _ => {}
        }
        prev_boundary = matches!(ch, ' ' | '>' | '+' | '~' | '(');
    }
    (a, b, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(parse_color("#1A2b3C"), Some(Color::Rgb(26, 43, 60)));
        assert_eq!(parse_color("rgb(10, 20, 30)"), Some(Color::Rgb(10, 20, 30)));
        assert_eq!(parse_color("rgba(10,20,30,0)"), None);
        assert_eq!(parse_color("RoyalBlue"), Some(Color::Rgb(65, 105, 225)));
        assert_eq!(parse_color("bogus"), None);
        let c = Color::Rgb(0, 0, 0).contrast(Color::Rgb(255, 255, 255)).unwrap();
        assert!((c - 21.0).abs() < 0.1);
    }

    #[test]
    fn declarations() {
        let (n, i) = parse_decls("color:#f00; font-weight: 700 ; display:none !important; text-align:center");
        assert_eq!(n.color, Some(Some(Color::Rgb(255, 0, 0))));
        assert_eq!(n.bold, Some(true));
        assert_eq!(n.align, Some(Align::Center));
        assert_eq!(i.display, Some(Display::None));
        let (n, _) = parse_decls("max-height:0; overflow:hidden;");
        assert_eq!(n.hidden, Some(true));
        let (n, _) = parse_decls("font-size:0px");
        assert_eq!((n.hidden, n.tiny), (None, Some(true)));
        let (n, _) = parse_decls("font-size:0.9em");
        assert_eq!((n.hidden, n.tiny), (None, Some(false)));
        let (n, _) = parse_decls("font-size:16px");
        assert_eq!(n.tiny, Some(false));
    }

    #[test]
    fn stylesheet() {
        let rules = parse_stylesheet(
            "/* c */ @import url(x.css); @media (max-width:600px){ .a{color:red} } \
             p.x, #y { color: blue } a:hover{color:red} <!-- .z{font-weight:bold} -->",
        );
        let sels: Vec<_> = rules.iter().map(|r| (r.selector.as_str(), r.specificity)).collect();
        assert_eq!(sels, [("p.x", (0, 1, 1)), ("#y", (1, 0, 0)), (".z", (0, 1, 0))]);
    }
}
