//! Colours and icons of the interface.
//!
//! Themes are the four Catppuccin flavours (catppuccin.com/palette) plus
//! "terminal", which only uses the terminal's own 16 colours. Icons are Nerd
//! Font glyphs (Material Design set), with plain fallbacks when
//! `[ui] icons = false`.

use std::sync::OnceLock;

use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    /// Main text.
    pub text: Color,
    /// Secondary text (subjects of read mail, labels).
    pub subtext: Color,
    /// Faint text (dates, hints, separators).
    pub overlay: Color,
    /// Borders of unfocused panes.
    pub border: Color,
    /// Background of the selected row.
    pub selection: Color,
    /// Background of the selected row while its pane is focused.
    pub selection_focused: Color,
    /// A slightly raised background (popups, status pills).
    pub surface: Color,
    /// Focus, titles, links.
    pub accent: Color,
    /// Second accent (account names, keys in hints).
    pub accent2: Color,
    pub unread: Color,
    pub flagged: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub info: Color,
    /// Colours that tell things apart (accounts, senders): never carry a
    /// meaning by themselves, only "this one, not that one".
    pub identity: Vec<Color>,
}

impl Theme {
    pub fn named(name: &str) -> Option<Theme> {
        Some(match name.to_ascii_lowercase().as_str() {
            "mocha" | "catppuccin" | "catppuccin-mocha" => catppuccin(MOCHA),
            "macchiato" | "catppuccin-macchiato" => catppuccin(MACCHIATO),
            "frappe" | "frappé" | "catppuccin-frappe" => catppuccin(FRAPPE),
            "latte" | "catppuccin-latte" => catppuccin(LATTE),
            "terminal" | "ansi" => Theme {
                text: Color::Reset,
                subtext: Color::Gray,
                overlay: Color::DarkGray,
                border: Color::DarkGray,
                selection: Color::Indexed(236),
                selection_focused: Color::Indexed(238),
                surface: Color::Indexed(235),
                accent: Color::Cyan,
                accent2: Color::Magenta,
                unread: Color::Yellow,
                flagged: Color::LightRed,
                ok: Color::Green,
                warn: Color::Yellow,
                error: Color::Red,
                info: Color::Blue,
                identity: vec![Color::Blue, Color::Green, Color::Yellow, Color::Magenta, Color::Cyan, Color::LightRed, Color::LightBlue, Color::LightGreen],
            },
            _ => return None,
        })
    }
}

/// Catppuccin palette: the colours this module uses, in RGB.
struct Palette {
    text: u32,
    subtext0: u32,
    overlay0: u32,
    surface0: u32,
    surface1: u32,
    mantle: u32,
    mauve: u32,
    blue: u32,
    lavender: u32,
    peach: u32,
    pink: u32,
    green: u32,
    yellow: u32,
    red: u32,
    teal: u32,
    sapphire: u32,
}

const MOCHA: Palette = Palette {
    text: 0xcdd6f4,
    subtext0: 0xa6adc8,
    overlay0: 0x6c7086,
    surface0: 0x313244,
    surface1: 0x45475a,
    mantle: 0x181825,
    mauve: 0xcba6f7,
    blue: 0x89b4fa,
    lavender: 0xb4befe,
    peach: 0xfab387,
    pink: 0xf5c2e7,
    green: 0xa6e3a1,
    yellow: 0xf9e2af,
    red: 0xf38ba8,
    teal: 0x94e2d5,
    sapphire: 0x74c7ec,
};
const MACCHIATO: Palette = Palette {
    text: 0xcad3f5,
    subtext0: 0xa5adcb,
    overlay0: 0x6e738d,
    surface0: 0x363a4f,
    surface1: 0x494d64,
    mantle: 0x1e2030,
    mauve: 0xc6a0f6,
    blue: 0x8aadf4,
    lavender: 0xb7bdf8,
    peach: 0xf5a97f,
    pink: 0xf5bde6,
    green: 0xa6da95,
    yellow: 0xeed49f,
    red: 0xed8796,
    teal: 0x8bd5ca,
    sapphire: 0x7dc4e4,
};
const FRAPPE: Palette = Palette {
    text: 0xc6d0f5,
    subtext0: 0xa5adce,
    overlay0: 0x737994,
    surface0: 0x414559,
    surface1: 0x51576d,
    mantle: 0x292c3c,
    mauve: 0xca9ee6,
    blue: 0x8caaee,
    lavender: 0xbabbf1,
    peach: 0xef9f76,
    pink: 0xf4b8e4,
    green: 0xa6d189,
    yellow: 0xe5c890,
    red: 0xe78284,
    teal: 0x81c8be,
    sapphire: 0x85c1dc,
};
const LATTE: Palette = Palette {
    text: 0x4c4f69,
    subtext0: 0x6c6f85,
    overlay0: 0x9ca0b0,
    surface0: 0xccd0da,
    surface1: 0xbcc0cc,
    mantle: 0xe6e9ef,
    mauve: 0x8839ef,
    blue: 0x1e66f5,
    lavender: 0x7287fd,
    peach: 0xfe640b,
    pink: 0xea76cb,
    green: 0x40a02b,
    yellow: 0xdf8e1d,
    red: 0xd20f39,
    teal: 0x179299,
    sapphire: 0x209fb5,
};

fn rgb(c: u32) -> Color {
    Color::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn catppuccin(p: Palette) -> Theme {
    Theme {
        text: rgb(p.text),
        subtext: rgb(p.subtext0),
        overlay: rgb(p.overlay0),
        border: rgb(p.surface1),
        selection: rgb(p.surface0),
        selection_focused: rgb(p.surface1),
        surface: rgb(p.mantle),
        accent: rgb(p.mauve),
        accent2: rgb(p.lavender),
        unread: rgb(p.peach),
        flagged: rgb(p.pink),
        ok: rgb(p.green),
        warn: rgb(p.yellow),
        error: rgb(p.red),
        info: rgb(p.blue),
        identity: [p.blue, p.green, p.peach, p.pink, p.teal, p.yellow, p.sapphire, p.mauve, p.red, p.lavender].into_iter().map(rgb).collect(),
    }
}

impl Theme {
    /// The colour of the n-th account (they cycle).
    pub fn account(&self, index: usize) -> Color {
        self.identity[index % self.identity.len()]
    }

    /// A stable colour for a person: the same address always gets the same one.
    pub fn person(&self, address: &str) -> Color {
        // FNV-1a over the lower-cased address: stable across runs and machines.
        let mut h: u32 = 0x811c9dc5;
        for b in address.trim().to_ascii_lowercase().bytes() {
            h = (h ^ b as u32).wrapping_mul(0x01000193);
        }
        self.identity[h as usize % self.identity.len()]
    }
}

/// "AG" for "Ana García", "A" for "amazon.es": what an avatar shows.
pub fn initials(name: &str, address: &str) -> String {
    let source = if name.trim().is_empty() { address.split('@').next().unwrap_or("") } else { name.trim() };
    let mut words = source.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty());
    let first = words.next().and_then(|w| w.chars().next());
    let second = words.next().and_then(|w| w.chars().next());
    match (first, second) {
        (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".into(),
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();
static ICONS: OnceLock<bool> = OnceLock::new();

/// Sets the theme and icon style once, at startup. Unknown names fall back
/// to Mocha (the returned warning says so).
pub fn init(name: &str, icons: bool) -> Option<String> {
    let (theme, warning) = match Theme::named(name) {
        Some(t) => (t, None),
        None => (Theme::named("mocha").expect("built in"), Some(format!("unknown theme \"{name}\": using mocha"))),
    };
    let _ = THEME.set(theme);
    let _ = ICONS.set(icons);
    warning
}

pub fn theme() -> &'static Theme {
    THEME.get_or_init(|| Theme::named("mocha").expect("built in"))
}

fn icons() -> bool {
    *ICONS.get_or_init(|| true)
}

/// Icons, as Nerd Font glyphs or plain text.
pub enum Icon {
    Account,
    Inbox,
    Sent,
    Drafts,
    Archive,
    Junk,
    Trash,
    Folder,
    Unread,
    Flagged,
    Answered,
    Attachment,
    Encrypted,
    Syncing,
}

impl Icon {
    pub fn glyph(&self) -> &'static str {
        let nerd = icons();
        match self {
            Icon::Account => if nerd { "\u{f0009}" } else { "@" },
            Icon::Inbox => if nerd { "\u{f0687}" } else { "▪" },
            Icon::Sent => if nerd { "\u{f048a}" } else { "➚" },
            Icon::Drafts => if nerd { "\u{f03eb}" } else { "✎" },
            Icon::Archive => if nerd { "\u{f003c}" } else { "▤" },
            Icon::Junk => if nerd { "\u{f073a}" } else { "⊘" },
            Icon::Trash => if nerd { "\u{f01b4}" } else { "✕" },
            Icon::Folder => if nerd { "\u{f024b}" } else { "▸" },
            Icon::Unread => "●",
            Icon::Flagged => if nerd { "\u{f04ce}" } else { "★" },
            Icon::Answered => if nerd { "\u{f045a}" } else { "↩" },
            Icon::Attachment => if nerd { "\u{f03e2}" } else { "+" },
            Icon::Encrypted => if nerd { "\u{f033e}" } else { "⚿" },
            Icon::Syncing => if nerd { "\u{f04e6}" } else { "⟳" },
        }
    }

    /// The icon for a mailbox, from its role as the name suggests it.
    pub fn for_mailbox(id: &str, name: &str) -> Icon {
        let leaf = name.rsplit('/').next().unwrap_or(name).to_lowercase();
        let any = |names: &[&str]| names.iter().any(|n| leaf == *n);
        if id.eq_ignore_ascii_case("INBOX") {
            Icon::Inbox
        } else if any(&["sent", "sent items", "sent messages", "sent mail", "enviados", "elementos enviados"]) {
            Icon::Sent
        } else if any(&["drafts", "draft", "borradores"]) {
            Icon::Drafts
        } else if any(&["trash", "deleted", "deleted items", "deleted messages", "bin", "papelera", "elementos eliminados"]) {
            Icon::Trash
        } else if any(&["junk", "spam", "junk e-mail", "correo no deseado", "bulk mail"]) {
            Icon::Junk
        } else if any(&["archive", "archivo", "archives", "all mail", "todos"]) {
            Icon::Archive
        } else {
            Icon::Folder
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_and_icons() {
        for n in ["mocha", "macchiato", "frappe", "latte", "terminal"] {
            assert!(Theme::named(n).is_some(), "{n}");
        }
        assert!(Theme::named("nope").is_none());
        assert_eq!(Theme::named("mocha").unwrap().accent, Color::Rgb(0xcb, 0xa6, 0xf7));
        let t = Theme::named("mocha").unwrap();
        assert_eq!(t.person("Ana@Example.org"), t.person("ana@example.org "), "stable, case-insensitive");
        assert_ne!(t.account(0), t.account(1));
        assert_eq!(t.account(0), t.account(t.identity.len()));
        assert_eq!(initials("Ana García", "x@y"), "AG");
        assert_eq!(initials("", "amazon@x.org"), "A");
        assert_eq!(initials("\"Ruiz, Dana\"", "d@x"), "RD");
        assert_eq!(initials("", ""), "?");
        assert!(matches!(Icon::for_mailbox("INBOX", "INBOX"), Icon::Inbox));
        assert!(matches!(Icon::for_mailbox("x", "Papelera"), Icon::Trash));
        assert!(matches!(Icon::for_mailbox("x", "[Gmail]/Spam"), Icon::Junk));
        assert!(matches!(Icon::for_mailbox("x", "Trabajo/work"), Icon::Folder));
    }
}
