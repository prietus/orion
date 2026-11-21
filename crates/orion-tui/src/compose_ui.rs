//! Writing mail: drafts are edited in `$EDITOR`, then reviewed here (send,
//! edit again, attach, sign/encrypt) and sent by the account thread.
//!
//! The draft file lives in the data directory, so a crash or a failed send
//! never loses what was written; it is deleted only after a successful send.

use std::io::Write;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};
use zeroize::Zeroizing;

use orion_core::compose::{self, Draft};

use crate::app::{App, Mode, PinPurpose};
use crate::worker::{Event, Request, SendError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

pub struct Compose {
    pub account: usize,
    pub draft: Draft,
    /// Last template text (kept as written when it does not parse).
    pub text: String,
    pub path: PathBuf,
    pub error: Option<String>,
    pub info: Option<String>,
    pub sending: bool,
    pub ticket: u64,
    pub encrypt_touched: bool,
    pub missing_keys: Vec<String>,
    /// Keys found but not verified, waiting for `A`: (address, fingerprint, user id).
    pub untrusted: Vec<(String, String, String)>,
    /// File browser open to choose attachments.
    pub picker: Option<crate::filepicker::FilePicker>,
    pub confirm_discard: bool,
}

pub fn drafts_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("orion").join("drafts")
}

/// Creates a private (0600) draft file.
fn new_draft_file(text: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = drafts_dir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let path = dir.join(format!("{stamp}-{}.eml", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    f.write_all(text.as_bytes())?;
    Ok(path)
}

impl App {
    pub fn start_compose(&mut self, kind: ComposeKind) {
        let account = match (kind, &self.reader, &self.current) {
            (ComposeKind::New, _, Some(c)) => c.0,
            (ComposeKind::New, _, None) => 0,
            (_, Some(r), _) => r.mailbox.0,
            (_, None, _) => {
                self.status = "open a message first".into();
                return;
            }
        };
        let Some(acc) = self.cfg.accounts.get(account) else {
            self.status = "no account to send from".into();
            return;
        };
        let Some(from) = acc.sender() else {
            self.status = format!("`{}` has no sender address: set `from = \"Name <you@example.org>\"`", acc.name);
            return;
        };
        let me = vec![from.clone()];
        let sig = acc.signature.as_deref();
        let view = self.reader.as_ref().and_then(|r| r.view.as_ref());
        let mut draft = match (kind, view) {
            (ComposeKind::New, _) => compose::new_message(&from, sig),
            (ComposeKind::Reply, Some(v)) => compose::reply(v, &from, &me, false, sig),
            (ComposeKind::ReplyAll, Some(v)) => compose::reply(v, &from, &me, true, sig),
            (ComposeKind::Forward, Some(v)) => compose::forward(v, &from, sig),
            (_, None) => {
                self.status = "the message is still loading".into();
                return;
            }
        };
        let pgp = self.gpg.is_some();
        draft.sign = pgp && self.cfg.pgp.sign_by_default;
        draft.signing_key = self.cfg.accounts.get(account).and_then(|a| a.pgp_key.clone());
        // Empty value = "add our key as an Autocrypt header if we have one" (filled in when sending).
        draft.autocrypt = (pgp && self.cfg.pgp.autocrypt).then(String::new);
        // Answering something that came encrypted: keep the conversation encrypted.
        let was_encrypted = matches!(kind, ComposeKind::Reply | ComposeKind::ReplyAll) && view.and_then(|v| v.crypto.as_ref()).is_some_and(|c| c.encrypted);
        draft.encrypt = pgp && was_encrypted;
        let text = compose::to_template(&draft);
        let path = match new_draft_file(&text) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("error: cannot create the draft file: {e}");
                return;
            }
        };
        self.compose = Some(Compose {
            account,
            draft,
            text,
            path: path.clone(),
            error: None,
            info: None,
            sending: false,
            ticket: 0,
            encrypt_touched: was_encrypted,
            missing_keys: Vec::new(),
            untrusted: Vec::new(),
            picker: None,
            confirm_discard: false,
        });
        self.pending_editor = Some(path);
    }

    /// Called by the main loop when `$EDITOR` exits.
    pub fn after_editor(&mut self, editor_ok: Result<(), String>) {
        let encrypt_when_possible = self.cfg.pgp.encrypt_when_possible;
        let gpg = self.gpg.clone();
        let Some(c) = &mut self.compose else { return };
        if let Err(e) = editor_ok {
            c.error = Some(format!("the editor failed: {e}"));
            return;
        }
        let text = match std::fs::read_to_string(&c.path) {
            Ok(t) => t,
            Err(e) => {
                c.error = Some(format!("cannot read the draft: {e}"));
                return;
            }
        };
        c.text = text;
        match compose::from_template(&c.text, &c.draft) {
            Ok(d) => {
                c.draft = d;
                c.error = None;
                if !c.encrypt_touched
                    && encrypt_when_possible
                    && let Some(g) = &gpg
                {
                    let mut all = compose::recipients(&c.draft);
                    all.push(compose::addr_spec(&c.draft.from));
                    c.draft.encrypt = !all.is_empty() && g.missing_keys(&all).is_empty();
                }
            }
            Err(e) => c.error = Some(format!("{e} — press e to fix it")),
        }
        self.needs_clear = true;
    }

    fn rewrite_draft_file(&mut self) {
        let Some(c) = &mut self.compose else { return };
        if c.error.is_none() {
            c.text = compose::to_template(&c.draft);
        }
        if let Err(e) = std::fs::write(&c.path, &c.text) {
            c.error = Some(format!("cannot write the draft: {e}"));
        }
    }

    pub fn compose_send(&mut self, pin: Option<Zeroizing<String>>) {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let Some(c) = &mut self.compose else { return };
        if c.error.is_some() {
            return;
        }
        if compose::recipients(&c.draft).is_empty() {
            c.error = Some("add at least one recipient (press e)".into());
            return;
        }
        c.sending = true;
        c.ticket = ticket;
        c.info = Some("sending…".into());
        let (account, draft) = (c.account, Box::new(c.draft.clone()));
        self.request(account, Request::Send { draft, pin, ticket });
    }

    pub fn on_sent(&mut self, ticket: u64, result: Result<Option<String>, SendError>) {
        let Some(c) = &mut self.compose else { return };
        if c.ticket != ticket {
            return;
        }
        c.sending = false;
        match result {
            Ok(warning) => {
                let subject = c.draft.subject.clone();
                let _ = std::fs::remove_file(&c.path);
                self.compose = None;
                self.needs_clear = true;
                self.status = match warning {
                    Some(w) => format!("✉ {w}"),
                    None => format!("✉ sent: {subject}"),
                };
            }
            Err(SendError::NeedPin { prompt }) => {
                c.info = None;
                self.mode = Mode::Pin { prompt, pin: Zeroizing::new(String::new()), purpose: PinPurpose::Send };
            }
            Err(SendError::BadPin) => {
                c.info = None;
                self.status = "wrong PIN or passphrase".into();
                self.mode = Mode::Pin { prompt: "PIN / passphrase to sign".into(), pin: Zeroizing::new(String::new()), purpose: PinPurpose::Send };
            }
            Err(SendError::MissingKeys(k)) => {
                c.error = None;
                c.missing_keys = k.clone();
                c.info = Some(format!("no public key for {} — k: look them up (WKD, keyservers) · E: send unencrypted", k.join(", ")));
            }
            Err(SendError::UntrustedKeys(k)) => {
                c.error = None;
                c.untrusted = k;
                c.info = Some("these keys are not verified: compare each fingerprint with its owner, then A accepts them · E: send unencrypted".into());
            }
            Err(SendError::Other(m)) => {
                c.info = None;
                c.error = Some(format!("not sent: {m} (the draft is kept in {})", c.path.display()));
            }
        }
    }

    pub fn on_keys_located(&mut self, untrusted: Vec<(String, String, String)>, missing: Vec<String>) {
        let Some(c) = &mut self.compose else { return };
        c.missing_keys = missing.clone();
        c.untrusted = untrusted;
        c.info = Some(match (c.untrusted.is_empty(), missing.is_empty()) {
            (true, true) => "found every key: press y to send encrypted".into(),
            (false, true) => "keys found: compare each fingerprint with its owner, then A accepts them".into(),
            (true, false) => format!("no key published for {}", missing.join(", ")),
            (false, false) => format!("keys found (A accepts after checking fingerprints); still missing {}", missing.join(", ")),
        });
    }

    /// Keys while the review screen is open. Returns false if not handled.
    pub fn compose_key(&mut self, ev: &KeyEvent) -> bool {
        let Some(c) = &mut self.compose else { return false };
        if let Some(picker) = &mut c.picker {
            match picker.key(*ev) {
                crate::filepicker::Outcome::Continue => {}
                crate::filepicker::Outcome::Cancel => c.picker = None,
                crate::filepicker::Outcome::Attach(paths) => {
                    c.picker = None;
                    let mut names = Vec::new();
                    for p in paths {
                        match compose::load_attachment(&p) {
                            Ok(a) => {
                                names.push(a.name.clone());
                                c.draft.attachments.push(a);
                            }
                            Err(e) => c.error = Some(e),
                        }
                    }
                    if !names.is_empty() {
                        c.info = Some(format!("attached {}", names.join(", ")));
                        self.rewrite_draft_file();
                    }
                }
            }
            return true;
        }
        if c.sending {
            return true; // wait for the result
        }
        let confirm = std::mem::replace(&mut c.confirm_discard, false);
        match ev.code {
            KeyCode::Char('y') | KeyCode::Char('s') => self.compose_send(None),
            KeyCode::Char('e') => {
                c.info = None;
                self.rewrite_draft_file();
                self.pending_editor = self.compose.as_ref().map(|c| c.path.clone());
            }
            KeyCode::Char('a') => c.picker = Some(crate::filepicker::FilePicker::open()),
            KeyCode::Char('d') => {
                if let Some(a) = c.draft.attachments.pop() {
                    c.info = Some(format!("removed {}", a.name));
                    self.rewrite_draft_file();
                }
            }
            KeyCode::Char('S') => {
                if self.gpg.is_none() {
                    c.error = Some("OpenPGP is disabled in the configuration".into());
                } else {
                    c.draft.sign = !c.draft.sign;
                }
            }
            KeyCode::Char('E') => {
                if self.gpg.is_none() {
                    c.error = Some("OpenPGP is disabled in the configuration".into());
                } else {
                    c.draft.encrypt = !c.draft.encrypt;
                    c.encrypt_touched = true;
                    if !c.draft.encrypt {
                        c.missing_keys.clear();
                        c.info = None;
                    }
                }
            }
            KeyCode::Char('k') if !c.missing_keys.is_empty() => {
                if let Some(g) = self.gpg.clone() {
                    let addrs = c.missing_keys.clone();
                    let keyservers = self.cfg.pgp.keyservers.clone();
                    c.info = Some(format!("looking up keys for {} (WKD, {})…", addrs.join(", "), keyservers.join(", ")));
                    let tx = self.events.clone();
                    std::thread::spawn(move || {
                        let found = g.locate_keys(&addrs, &keyservers);
                        let missing = addrs.iter().filter(|a| !found.contains(a)).cloned().collect();
                        // Downloaded keys are never certified: they need the user's acceptance.
                        let untrusted = found
                            .iter()
                            .filter_map(|a| g.encryption_key(a).filter(|k| !k.certified()).map(|k| (a.clone(), k.fingerprint, k.uid)))
                            .collect();
                        let _ = tx.send(Event::KeysLocated { untrusted, missing });
                    });
                }
            }
            KeyCode::Char('A') if !c.untrusted.is_empty() => {
                let path = orion_crypto::accepted::default_path();
                let mut failed = None;
                for (addr, fpr, _) in &c.untrusted {
                    if let Err(e) = orion_crypto::accepted::AcceptedKeys::accept(&path, addr, fpr) {
                        failed = Some(e.to_string());
                    }
                }
                match failed {
                    Some(e) => c.error = Some(format!("cannot save the accepted keys: {e}")),
                    None => {
                        c.info = Some(format!("accepted {} key(s): press y to send encrypted", c.untrusted.len()));
                        c.untrusted.clear();
                    }
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if confirm {
                    let path = c.path.clone();
                    self.compose = None;
                    self.needs_clear = true;
                    self.status = format!("draft kept in {}", path.display());
                } else {
                    c.confirm_discard = true;
                    c.info = Some("press q again to close (the draft file is kept)".into());
                }
            }
            _ => {}
        }
        true
    }
}
