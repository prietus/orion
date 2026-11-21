//! Built-in compose panel: address fields with suggestions from the address
//! book, the subject, and the body in a Vim-like editor (edtui). Ctrl-E still
//! opens everything in `$EDITOR` and comes back here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::{EditorEventHandler, EditorMode, EditorState, Lines};

use orion_core::compose::{self, Draft};

use crate::contacts::{self, Contact};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

impl Field {
    pub const ALL: [Field; 5] = [Field::To, Field::Cc, Field::Bcc, Field::Subject, Field::Body];

    pub fn label(self) -> &'static str {
        match self {
            Field::To => "To",
            Field::Cc => "Cc",
            Field::Bcc => "Bcc",
            Field::Subject => "Subject",
            Field::Body => "Body",
        }
    }
}

pub struct Panel {
    pub field: Field,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    /// What is being typed in the current address field.
    pub input: String,
    pub subject: String,
    pub body: EditorState,
    handler: EditorEventHandler,
    pub suggestions: Vec<Contact>,
    pub sel: usize,
}

/// What a key asks the compose screen to do beyond editing.
#[derive(Debug, PartialEq, Eq)]
pub enum PanelAction {
    None,
    Send,
    ExternalEditor,
    Attach,
    DropAttachment,
    ToggleSign,
    ToggleEncrypt,
    LookupKeys,
    AcceptKeys,
    Close,
}

impl Panel {
    pub fn new(d: &Draft) -> Panel {
        let mut body = EditorState::new(Lines::from(d.body.as_str()));
        // Replies start writing above the quote, in insert mode; new mail in To.
        let field = if d.to.is_empty() { Field::To } else { Field::Body };
        if field == Field::Body {
            body.mode = EditorMode::Insert;
        }
        Panel {
            field,
            to: d.to.clone(),
            cc: d.cc.clone(),
            bcc: d.bcc.clone(),
            input: String::new(),
            subject: d.subject.clone(),
            body,
            handler: EditorEventHandler::vim_mode(),
            suggestions: Vec::new(),
            sel: 0,
        }
    }

    /// Copies what was typed into the draft (a half-typed address counts).
    pub fn apply(&mut self, d: &mut Draft) {
        self.commit_input(None);
        d.to = self.to.clone();
        d.cc = self.cc.clone();
        d.bcc = self.bcc.clone();
        d.subject = self.subject.trim().to_string();
        d.body = self.body.lines.to_string();
    }

    pub fn list(&self, f: Field) -> &Vec<String> {
        match f {
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
            _ => &self.to,
        }
    }

    fn list_mut(&mut self, f: Field) -> &mut Vec<String> {
        match f {
            Field::Cc => &mut self.cc,
            Field::Bcc => &mut self.bcc,
            _ => &mut self.to,
        }
    }

    fn is_address(f: Field) -> bool {
        matches!(f, Field::To | Field::Cc | Field::Bcc)
    }

    /// Adds the highlighted suggestion, else what was typed if it is an
    /// address. Returns whether something was added.
    fn commit_input(&mut self, suggestion: Option<usize>) -> bool {
        if !Self::is_address(self.field) {
            return false;
        }
        let chosen = suggestion.and_then(|i| self.suggestions.get(i)).map(Contact::display);
        let typed = self.input.trim().trim_matches([',', ';']).trim().to_string();
        let value = match chosen {
            Some(v) => v,
            None if typed.is_empty() => return false,
            None => match compose::parse_addresses(&typed) {
                Ok(list) if !list.is_empty() => {
                    let field = self.field;
                    self.list_mut(field).extend(list);
                    self.input.clear();
                    self.suggestions.clear();
                    return true;
                }
                _ => return false,
            },
        };
        let field = self.field;
        let list = self.list_mut(field);
        if !list.iter().any(|a| compose::addr_spec(a).eq_ignore_ascii_case(&compose::addr_spec(&value))) {
            list.push(value);
        }
        self.input.clear();
        self.suggestions.clear();
        true
    }

    fn refresh_suggestions(&mut self, book: &[Contact]) {
        let taken: Vec<String> = self.to.iter().chain(&self.cc).chain(&self.bcc).map(|a| compose::addr_spec(a).to_lowercase()).collect();
        self.suggestions = contacts::search(book, &self.input, 12).into_iter().filter(|c| !taken.contains(&c.email.to_lowercase())).take(6).cloned().collect();
        self.sel = 0;
    }

    fn move_field(&mut self, delta: i32) {
        // Text that is not an address stays where it is (and so does the
        // cursor), instead of being lost on the way to another field.
        if !self.commit_input(None) && Self::is_address(self.field) && !self.input.trim().is_empty() {
            return;
        }
        let i = Field::ALL.iter().position(|f| *f == self.field).unwrap_or(0) as i32;
        self.field = Field::ALL[(i + delta).rem_euclid(Field::ALL.len() as i32) as usize];
        self.suggestions.clear();
    }

    pub fn key(&mut self, ev: KeyEvent, book: &[Contact]) -> PanelAction {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        // Commands, from anywhere.
        if ctrl {
            let action = match ev.code {
                KeyCode::Char('s') => PanelAction::Send,
                KeyCode::Char('e') => PanelAction::ExternalEditor,
                KeyCode::Char('a') => PanelAction::Attach,
                KeyCode::Char('x') => PanelAction::DropAttachment,
                KeyCode::Char('g') => PanelAction::ToggleSign,
                KeyCode::Char('k') => PanelAction::ToggleEncrypt,
                KeyCode::Char('l') => PanelAction::LookupKeys,
                KeyCode::Char('y') => PanelAction::AcceptKeys,
                KeyCode::Char('q') => PanelAction::Close,
                _ => PanelAction::None,
            };
            if action != PanelAction::None {
                return action;
            }
        }
        if ev.code == KeyCode::BackTab {
            self.move_field(-1);
            return PanelAction::None;
        }
        match self.field {
            Field::Body => {
                // Tab leaves the body only outside insert mode (in insert it is text).
                if ev.code == KeyCode::Tab && self.body.mode != EditorMode::Insert {
                    self.move_field(1);
                } else {
                    self.handler.on_key_event(ev, &mut self.body);
                }
            }
            Field::Subject => match ev.code {
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.move_field(1),
                KeyCode::Up => self.move_field(-1),
                KeyCode::Backspace => {
                    self.subject.pop();
                }
                KeyCode::Esc => return PanelAction::Close,
                KeyCode::Char(c) if !ctrl => self.subject.push(c),
                _ => {}
            },
            _ => match ev.code {
                KeyCode::Down if !self.suggestions.is_empty() => self.sel = (self.sel + 1).min(self.suggestions.len() - 1),
                KeyCode::Up if !self.suggestions.is_empty() => self.sel = self.sel.saturating_sub(1),
                KeyCode::Char('n') if ctrl && !self.suggestions.is_empty() => self.sel = (self.sel + 1).min(self.suggestions.len() - 1),
                KeyCode::Char('p') if ctrl => self.sel = self.sel.saturating_sub(1),
                KeyCode::Down => self.move_field(1),
                KeyCode::Up => self.move_field(-1),
                KeyCode::Enter | KeyCode::Tab => {
                    let pick = (!self.suggestions.is_empty()).then_some(self.sel);
                    if !self.commit_input(pick) || ev.code == KeyCode::Enter && self.input.is_empty() && pick.is_none() {
                        self.move_field(1);
                    }
                }
                KeyCode::Char(',') | KeyCode::Char(';') => {
                    let pick = (!self.suggestions.is_empty()).then_some(self.sel);
                    self.commit_input(pick);
                }
                KeyCode::Backspace => {
                    if self.input.pop().is_none() {
                        let field = self.field;
                        self.list_mut(field).pop();
                    }
                    self.refresh_suggestions(book);
                }
                KeyCode::Esc if !self.suggestions.is_empty() => self.suggestions.clear(),
                KeyCode::Esc => return PanelAction::Close,
                KeyCode::Char(c) if !ctrl => {
                    self.input.push(c);
                    self.refresh_suggestions(book);
                }
                _ => {}
            },
        }
        PanelAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    fn typ(p: &mut Panel, s: &str, book: &[Contact]) {
        for c in s.chars() {
            p.key(k(KeyCode::Char(c)), book);
        }
    }

    fn book() -> Vec<Contact> {
        vec![
            Contact { name: "Ana García".into(), email: "ana@example.org".into(), score: 50 },
            Contact { name: "Luis Martín".into(), email: "luis@example.net".into(), score: 30 },
            Contact { name: "Lucía Pérez".into(), email: "lucia@example.com".into(), score: 40 },
        ]
    }

    #[test]
    fn addresses_with_suggestions() {
        let book = book();
        let mut p = Panel::new(&Draft::default());
        assert_eq!(p.field, Field::To);
        typ(&mut p, "lu", &book);
        assert_eq!(p.suggestions.iter().map(|c| c.email.as_str()).collect::<Vec<_>>(), ["lucia@example.com", "luis@example.net"]);
        p.key(k(KeyCode::Down), &book);
        p.key(k(KeyCode::Enter), &book);
        assert_eq!(p.to, ["Luis Martín <luis@example.net>"]);
        assert!(p.input.is_empty() && p.field == Field::To, "stays to add more");
        // Typed address + comma; then the suggestion list skips who is already there.
        typ(&mut p, "bob@example.com,", &book);
        assert_eq!(p.to.len(), 2);
        typ(&mut p, "lu", &book);
        assert_eq!(p.suggestions.len(), 1, "Luis is already a recipient");
        p.key(k(KeyCode::Tab), &book);
        assert_eq!(p.to.len(), 3);
        // Backspace on an empty input removes the last one.
        p.key(k(KeyCode::Backspace), &book);
        assert_eq!(p.to.len(), 2);
        // Enter on an empty field goes on: Cc, Bcc, Subject.
        for _ in 0..3 {
            p.key(k(KeyCode::Enter), &book);
        }
        assert_eq!(p.field, Field::Subject);
        typ(&mut p, "Hola", &book);
        p.key(k(KeyCode::Enter), &book);
        assert_eq!(p.field, Field::Body);
        // The body is a Vim editor: i to insert.
        typ(&mut p, "iQué tal", &book);
        let mut d = Draft::default();
        p.apply(&mut d);
        assert_eq!(d.subject, "Hola");
        assert_eq!(d.body, "Qué tal");
        assert_eq!(d.to, ["Luis Martín <luis@example.net>", "bob@example.com"]);
        // Commands.
        assert_eq!(p.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL), &book), PanelAction::Send);
        assert_eq!(p.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL), &book), PanelAction::ExternalEditor);
    }

    #[test]
    fn text_that_is_not_an_address_is_kept() {
        let mut p = Panel::new(&Draft::default());
        typ(&mut p, "Prueba", &[]);
        p.key(k(KeyCode::Tab), &[]);
        p.key(k(KeyCode::Enter), &[]);
        assert_eq!((p.field, p.input.as_str()), (Field::To, "Prueba"), "not lost, not moved");
        for _ in 0..6 {
            p.key(k(KeyCode::Backspace), &[]);
        }
        p.key(k(KeyCode::Tab), &[]);
        assert_eq!(p.field, Field::Cc);
    }

    #[test]
    fn replies_start_in_the_body() {
        let d = Draft { to: vec!["ana@example.org".into()], body: "\n\n> quoted".into(), ..Default::default() };
        let p = Panel::new(&d);
        assert_eq!(p.field, Field::Body);
        assert_eq!(p.body.mode, EditorMode::Insert);
        // A half-typed address is not lost when sending.
        let mut p = Panel::new(&Draft::default());
        typ(&mut p, "carol@example.org", &[]);
        let mut d = Draft::default();
        p.apply(&mut d);
        assert_eq!(d.to, ["carol@example.org"]);
    }
}
