//! Configurable key map.

use std::collections::{BTreeMap, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

macro_rules! actions {
    ($($variant:ident => $name:literal [$($key:literal),*] $desc:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant),* }

        impl Action {
            pub const ALL: &[Action] = &[$(Action::$variant),*];
            pub fn name(self) -> &'static str {
                match self { $(Action::$variant => $name),* }
            }
            pub fn description(self) -> &'static str {
                match self { $(Action::$variant => $desc),* }
            }
            fn default_keys(self) -> &'static [&'static str] {
                match self { $(Action::$variant => &[$($key),*]),* }
            }
            fn from_name(name: &str) -> Option<Action> {
                match name { $($name => Some(Action::$variant),)* _ => None }
            }
        }
    };
}

actions! {
    Quit        => "quit"         ["q"]                       "quit (in the reader: go back)",
    ForceQuit   => "force_quit"   ["ctrl-c"]                  "quit immediately",
    Down        => "down"         ["j", "down"]               "move down",
    Up          => "up"           ["k", "up"]                 "move up",
    Top         => "top"          ["g", "home"]               "go to top",
    Bottom      => "bottom"       ["G", "end"]                "go to bottom",
    PageDown    => "page_down"    ["ctrl-d", "pgdown", "space"] "page down",
    PageUp      => "page_up"      ["ctrl-u", "pgup"]          "page up",
    Open        => "open"         ["enter", "l", "right"]     "open",
    Back        => "back"         ["esc", "h", "left"]        "back / left pane",
    FocusNext   => "focus_next"   ["tab"]                     "switch pane",
    NextMessage => "next_message" ["n", "J"]                  "next message",
    PrevMessage => "prev_message" ["p", "K"]                  "previous message",
    Delete      => "delete"       ["d", "delete"]             "move to the Trash",
    DeleteForever => "delete_forever" ["D"]                   "delete for good (asks first)",
    MoveTo      => "move"         ["m"]                       "move to another folder",
    EmptyFolder => "empty_folder" ["X"]                       "empty the Trash or Spam folder (asks first)",
    SaveAttachments => "attachments" ["s"]                    "save or open the attachments",
    ToggleThread => "toggle_thread" ["t"]                     "expand / collapse the conversation",
    Threads     => "threads"      ["T"]                       "group the list into conversations (on/off)",
    ToggleRead  => "toggle_read"  ["N"]                       "mark read / unread",
    ToggleFlagged => "toggle_flag" ["F"]                      "flag / unflag as important",
    ToggleView  => "toggle_view"  ["v"]                       "toggle HTML / text",
    OriginalView => "original_view" ["V"]                     "show the HTML as designed (drawn by Chromium)",
    ImportKey   => "import_key"   ["I"]                       "import the sender's key (to reply signed/encrypted)",
    CardStatus  => "card_status"  ["Y"]                       "YubiKey / OpenPGP card status",
    OpenLink    => "open_link"    ["o"]                       "open link (o + number + enter)",
    ToggleHeaders => "toggle_headers" ["H"]                   "full headers",
    ToggleImages  => "toggle_images"  ["i"]                   "show/hide images",
    LoadRemote    => "load_remote_images" ["L"]               "download this message's remote images (the sender learns you opened it)",
    NextImage   => "next_image"   ["]"]                       "next image",
    PrevImage   => "prev_image"   ["["]                       "previous image",
    Search      => "search"       ["/"]                       "filter by subject/sender",
    Reload      => "reload"       ["ctrl-r", "f5"]            "reload",
    Compose     => "compose"      ["c"]                       "write a new message",
    Reply       => "reply"        ["r"]                       "reply",
    ReplyAll    => "reply_all"    ["R"]                       "reply to all",
    Forward     => "forward"      ["f"]                       "forward",
    Help        => "help"         ["?"]                       "help",
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Chord {
    code: KeyCode,
    mods: KeyModifiers,
}

impl Chord {
    fn from_event(ev: &KeyEvent) -> Self {
        let mut mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        // For characters, uppercase is already part of the character itself.
        if matches!(ev.code, KeyCode::Char(_)) {
            mods.remove(KeyModifiers::SHIFT);
        }
        let code = match ev.code {
            KeyCode::BackTab => KeyCode::Tab,
            c => c,
        };
        if ev.code == KeyCode::BackTab {
            mods.insert(KeyModifiers::SHIFT);
        }
        Chord { code, mods }
    }

    fn parse(spec: &str) -> Option<Self> {
        let mut mods = KeyModifiers::NONE;
        let mut rest = spec;
        loop {
            let lower = rest.to_ascii_lowercase();
            if rest.len() > 1 && lower.starts_with("ctrl-") {
                mods |= KeyModifiers::CONTROL;
                rest = &rest[5..];
            } else if rest.len() > 1 && lower.starts_with("alt-") {
                mods |= KeyModifiers::ALT;
                rest = &rest[4..];
            } else if rest.len() > 1 && lower.starts_with("shift-") {
                mods |= KeyModifiers::SHIFT;
                rest = &rest[6..];
            } else {
                break;
            }
        }
        let mut chars = rest.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) => KeyCode::Char(c),
            _ => match rest.to_ascii_lowercase().as_str() {
                "enter" | "return" => KeyCode::Enter,
                "esc" | "escape" => KeyCode::Esc,
                "tab" => KeyCode::Tab,
                "space" => KeyCode::Char(' '),
                "backspace" => KeyCode::Backspace,
                "delete" | "del" => KeyCode::Delete,
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                "left" => KeyCode::Left,
                "right" => KeyCode::Right,
                "home" => KeyCode::Home,
                "end" => KeyCode::End,
                "pgup" | "pageup" => KeyCode::PageUp,
                "pgdown" | "pagedown" => KeyCode::PageDown,
                f if f.starts_with('f') => KeyCode::F(f[1..].parse().ok()?),
                _ => return None,
            },
        };
        if let KeyCode::Char(c) = code {
            if mods.contains(KeyModifiers::SHIFT) {
                mods.remove(KeyModifiers::SHIFT);
                return Some(Chord { code: KeyCode::Char(c.to_ascii_uppercase()), mods });
            }
            // With ctrl, the terminal sends the letter in lowercase.
            if mods.contains(KeyModifiers::CONTROL) {
                return Some(Chord { code: KeyCode::Char(c.to_ascii_lowercase()), mods });
            }
        }
        Some(Chord { code, mods })
    }
}

pub struct Keymap {
    map: HashMap<Chord, Action>,
    pub labels: BTreeMap<&'static str, Vec<String>>,
}

impl Keymap {
    /// Builds the map from the defaults and the configuration.
    /// Also returns warnings about invalid entries.
    pub fn new(user: &BTreeMap<String, Vec<String>>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut bindings: Vec<(Action, Vec<String>)> = Action::ALL
            .iter()
            .map(|a| (*a, a.default_keys().iter().map(|s| s.to_string()).collect()))
            .collect();
        for (name, keys) in user {
            match Action::from_name(name) {
                Some(a) => {
                    if let Some(b) = bindings.iter_mut().find(|(x, _)| *x == a) {
                        b.1 = keys.clone();
                    }
                }
                None => warnings.push(format!("unknown action in [keys]: {name}")),
            }
        }
        // User keys take precedence: they are removed from the default actions.
        let user_chords: Vec<Chord> = user.values().flatten().filter_map(|k| Chord::parse(k)).collect();
        let mut map = HashMap::new();
        let mut labels = BTreeMap::new();
        for (action, keys) in bindings {
            let customized = user.contains_key(action.name());
            let mut shown = Vec::new();
            for k in keys {
                match Chord::parse(&k) {
                    Some(ch) if customized || !user_chords.contains(&ch) => {
                        map.insert(ch, action);
                        shown.push(k);
                    }
                    Some(_) => {}
                    None => warnings.push(format!("invalid key \"{k}\" for {}", action.name())),
                }
            }
            labels.insert(action.name(), shown);
        }
        (Keymap { map, labels }, warnings)
    }

    pub fn action(&self, ev: &KeyEvent) -> Option<Action> {
        self.map.get(&Chord::from_event(ev)).copied()
    }

    pub fn label(&self, a: Action) -> String {
        self.labels.get(a.name()).and_then(|v| v.first()).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn defaults_and_overrides() {
        let mut user = BTreeMap::new();
        user.insert("quit".to_string(), vec!["x".to_string()]);
        user.insert("down".to_string(), vec!["ctrl-n".to_string()]);
        let (km, warn) = Keymap::new(&user);
        assert!(warn.is_empty());
        assert_eq!(km.action(&ev(KeyCode::Char('x'), KeyModifiers::NONE)), Some(Action::Quit));
        assert_eq!(km.action(&ev(KeyCode::Char('q'), KeyModifiers::NONE)), None);
        assert_eq!(km.action(&ev(KeyCode::Char('n'), KeyModifiers::CONTROL)), Some(Action::Down));
        assert_eq!(km.action(&ev(KeyCode::Char('G'), KeyModifiers::SHIFT)), Some(Action::Bottom));
        assert_eq!(km.action(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(Action::ForceQuit));
    }

    #[test]
    fn invalid_entries_warn() {
        let mut user = BTreeMap::new();
        user.insert("nope".to_string(), vec!["x".to_string()]);
        user.insert("up".to_string(), vec!["ctrl-".to_string(), "bogus".to_string()]);
        let (_, warn) = Keymap::new(&user);
        assert_eq!(warn.len(), 3);
    }
}
