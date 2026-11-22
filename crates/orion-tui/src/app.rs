//! Application state. It does no I/O: it talks to the account threads through
//! channels and reacts to their events.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use orion_core::config::{Config, SortOrder};
use orion_core::model::{CryptoProblem, Envelope, Flag, MailboxInfo, MessageId, MessageView};
use zeroize::Zeroizing;

use crate::body::{safe_url, Body, BodyMode, InlineImage, InlineKey};
use crate::keys::{Action, Keymap};
use crate::worker::{Event, Request};

pub struct Account {
    pub name: String,
    pub tx: Sender<Request>,
    pub mailboxes: Vec<MailboxInfo>,
    pub error: Option<String>,
    pub progress: String,
}

/// Stable mailbox identity: (account index, mailbox id).
pub type MailboxKey = (usize, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteState {
    NotLoaded,
    Loading,
    Loaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    List,
    Reader,
}

/// What the PIN being typed is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPurpose {
    /// Decrypting the open message.
    Unlock,
    /// Signing the message being sent.
    Send,
}

pub enum Mode {
    Normal,
    Search(String),
    /// Asking for the number of the link to open.
    OpenLink(String),
    /// Asking for the card PIN or the passphrase (shown masked).
    Pin { prompt: String, pin: Zeroizing<String>, purpose: PinPurpose },
    /// Popup with the OpenPGP card status.
    Card(Vec<String>),
    /// "Delete for good?" waiting for y/n.
    ConfirmDelete { prompt: String, id: MessageId, key: MailboxKey },
    /// List of the open message's attachments (save / open).
    Attachments { sel: usize },
    /// "Empty this folder?" waiting for y/n.
    ConfirmEmpty { prompt: String, key: MailboxKey },
    /// Choosing the folder to move a message to: filter and selection.
    MoveTo { id: MessageId, key: MailboxKey, filter: String, sel: usize },
    Help,
}

/// A row of the message list.
#[derive(Debug, Clone, Default)]
pub struct RowInfo {
    /// For the row that stands for a conversation of several messages: all
    /// of them (indices into `envelopes`, newest first). Empty otherwise.
    pub members: Vec<usize>,
    /// One of the older messages of an expanded conversation.
    pub child: bool,
    /// The conversation's key (its oldest message).
    pub key: MessageId,
    pub expanded: bool,
}

pub struct Reader {
    /// The message's id in the list.
    pub id: MessageId,
    /// Its id where it really is (`mailbox`): differs for sent mail shown
    /// inside a conversation.
    pub remote_id: MessageId,
    pub mailbox: MailboxKey,
    /// `None` while downloading.
    pub view: Option<MessageView>,
    /// Laid-out body (HTML or text); present once `view` is loaded.
    pub body: Option<Body>,
    pub error: Option<String>,
    /// Decryption in progress after entering the PIN.
    pub unlocking: bool,
    pub scroll: u16,
    pub max_scroll: u16,
    pub page: u16,
    pub show_headers: bool,
    /// Indices of the attachments that are images.
    pub images: Vec<usize>,
    pub image_idx: usize,
    /// Remote images: not asked, downloading, or loaded.
    pub remote: RemoteState,
    /// Chromium is drawing the original view.
    pub rendering: bool,
    pub show_images: bool,
    pub image: Option<ImageSlot>,
}

pub struct ImageSlot {
    pub attachment: usize,
    /// `None` while decoding in the background.
    pub state: Option<Result<StatefulProtocol, String>>,
}

pub struct App {
    pub cfg: Config,
    pub gpg: Option<orion_crypto::Gpg>,
    pub keymap: Keymap,
    pub picker: Option<Picker>,
    pub events: Sender<Event>,
    pub accounts: Vec<Account>,
    /// Selectable mailboxes in the sidebar.
    pub sidebar: Vec<MailboxKey>,
    pub sidebar_sel: usize,
    pub sidebar_state: ListState,
    pub current: Option<MailboxKey>,
    /// The displayed list comes from the cache and is still syncing.
    pub loading: bool,
    pub envelopes: Vec<Envelope>,
    /// Indices into `envelopes`, sorted and filtered.
    pub visible: Vec<usize>,
    /// What each row of `visible` is (a conversation, one of its messages…).
    pub rows: Vec<RowInfo>,
    /// Conversations shown expanded (by their oldest message).
    pub expanded: std::collections::HashSet<MessageId>,
    /// How many of `envelopes` belong to the open mailbox; the rest are your
    /// sent messages, shown inside its conversations.
    pub native: usize,
    /// The account's sent mail, as last received (mailbox, messages).
    related: Option<(MailboxKey, Vec<Envelope>)>,
    /// List id of a sent message → where it really is (mailbox, id there).
    foreign: HashMap<MessageId, (MailboxKey, MessageId)>,
    pub list_state: TableState,
    pub list_page: u16,
    pub filter: String,
    pub reader: Option<Reader>,
    pub focus: Focus,
    pub mode: Mode,
    pub status: String,
    pub quit: bool,
    /// Requests a full-screen repaint (needed when hiding sixel images).
    pub needs_clear: bool,
    /// Message being written (review screen).
    pub compose: Option<crate::compose_ui::Compose>,
    /// Draft file the main loop must open in `$EDITOR`.
    pub pending_editor: Option<std::path::PathBuf>,
    pub next_ticket: u64,
    /// Last sync failure of the open mailbox (shown until a sync succeeds).
    pub sync_error: Option<String>,
    /// Three panes (folders | list | message): set by the drawing code from
    /// the window width and `[ui] layout`.
    pub wide: bool,
    /// Debounce counter of the preview that follows the list selection.
    preview_seq: u64,
    /// Group the list into conversations.
    pub threads: bool,
    /// Address book for suggestions ([contacts] sources), loaded at startup.
    pub contacts: Vec<crate::contacts::Contact>,
}

impl App {
    pub fn new(cfg: Config, keymap: Keymap, picker: Option<Picker>, events: Sender<Event>, accounts: Vec<Account>) -> Self {
        let threads = cfg.ui.threads;
        let mut app = App {
            cfg,
            gpg: None,
            keymap,
            picker,
            events,
            accounts,
            sidebar: Vec::new(),
            sidebar_sel: 0,
            sidebar_state: ListState::default(),
            current: None,
            loading: false,
            envelopes: Vec::new(),
            visible: Vec::new(),
            list_state: TableState::default(),
            list_page: 10,
            filter: String::new(),
            reader: None,
            focus: Focus::List,
            mode: Mode::Normal,
            status: String::new(),
            quit: false,
            needs_clear: false,
            compose: None,
            pending_editor: None,
            next_ticket: 0,
            sync_error: None,
            wide: false,
            preview_seq: 0,
            contacts: Vec::new(),
            threads: true,
            rows: Vec::new(),
            expanded: Default::default(),
            native: 0,
            related: None,
            foreign: HashMap::new(),
        };
        app.threads = threads;
        app
    }

    pub fn mailbox(&self, key: &MailboxKey) -> Option<&MailboxInfo> {
        self.accounts.get(key.0)?.mailboxes.iter().find(|m| m.id == key.1)
    }

    pub fn current_mailbox(&self) -> Option<&MailboxInfo> {
        self.current.as_ref().and_then(|k| self.mailbox(k))
    }

    pub fn selected_envelope(&self) -> Option<usize> {
        self.list_state.selected().and_then(|i| self.visible.get(i).copied())
    }

    fn envelope_index(&self, id: MessageId) -> Option<usize> {
        self.envelopes.iter().position(|e| e.id == id)
    }

    pub(crate) fn request(&self, account: usize, req: Request) {
        if let Some(acc) = self.accounts.get(account) {
            let _ = acc.tx.send(req);
        }
    }

    // --------------------------------------------------------------- events

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Input(crossterm::event::Event::Key(k)) if k.kind == crossterm::event::KeyEventKind::Press => self.on_key(k),
            Event::Input(_) => {}
            Event::Mailboxes { account, list } => self.on_mailboxes(account, list),
            Event::Envelopes { account, mailbox, envelopes, from_cache } => {
                if self.current.as_ref() == Some(&(account, mailbox)) {
                    if !from_cache {
                        self.sync_error = None;
                    }
                    self.set_envelopes(envelopes);
                    self.loading = from_cache;
                }
            }
            Event::Message { account, mailbox, id, result } => self.on_message(account, mailbox, id, result),
            Event::Progress { account, text } => {
                if let Some(a) = self.accounts.get_mut(account) {
                    a.progress = text;
                }
            }
            Event::Error { account, text } => {
                let name = self.accounts.get(account).map(|a| a.name.clone()).unwrap_or_default();
                self.status = format!("error [{name}]: {text}");
                if let Some(a) = self.accounts.get_mut(account)
                    && a.mailboxes.is_empty() {
                        a.error = Some(text);
                    }
            }
            Event::NewMail { account, mailbox_name, envelopes, .. } => {
                let name = self.accounts.get(account).map(|a| a.name.as_str()).unwrap_or_default();
                let n = envelopes.len();
                self.status = match &envelopes[..] {
                    [e] => format!("📬 new mail in {name}/{mailbox_name}: {} — {}", e.sender(), e.subject),
                    _ => format!("📬 {n} new messages in {name}/{mailbox_name}"),
                };
            }
            Event::FlagFailed { account, mailbox, ids, flag, on } => {
                if self.current.as_ref() == Some(&(account, mailbox)) {
                    for id in ids {
                        self.apply_flag_locally(id, flag, !on);
                    }
                }
            }
            Event::Sent { ticket, result } => self.on_sent(ticket, result),
            Event::SyncFailed { account, mailbox, error } => {
                if self.current.as_ref() == Some(&(account, mailbox)) {
                    self.loading = false;
                    self.sync_error = Some(error);
                }
            }
            Event::KeysLocated { untrusted, missing } => self.on_keys_located(untrusted, missing),
            Event::Snapshot { id, result } => self.on_snapshot(id, result),
            Event::Preview { seq } => self.on_preview(seq),
            Event::Contacts(list) => self.contacts = list,
            Event::PreviewRead { id } => self.on_preview_read(id),
            Event::Removed { account, mailbox, result } => self.on_removed(account, mailbox, result),
            Event::Related { account, mailbox, envelopes } => {
                self.related = Some(((account, mailbox), envelopes));
                if self.current.as_ref().is_some_and(|c| c.0 == account) {
                    let natives = self.envelopes[..self.native].to_vec();
                    self.set_envelopes(natives);
                }
            }
            Event::Emptied { account, mailbox, result } => {
                self.status = match result {
                    Ok(()) => "folder emptied".into(),
                    Err(e) => {
                        if self.current.as_ref() == Some(&(account, mailbox)) {
                            self.reload();
                        }
                        format!("error: {e}")
                    }
                };
            }
            Event::RemoteImages { account, mailbox, id, images, failed } => self.on_remote_images(account, mailbox, id, images, failed),
            Event::OtpIgnored => {
                self.status = "ignored a YubiKey OTP (touching it types a code); \
                               no touch is needed unless the key blinks — `ykman config usb --disable OTP` turns it off"
                    .into();
            }
            Event::KeyImported(result) => {
                self.status = match result {
                    Ok(m) => m,
                    Err(e) => format!("error: {e}"),
                };
                // Open the message again: the signature can now be verified.
                if let Some(r) = &self.reader {
                    let (account, mailbox, id) = (r.mailbox.0, r.mailbox.1.clone(), r.remote_id);
                    self.request(account, Request::Fetch { mailbox, id });
                }
            }
            Event::Card(result) => {
                self.status.clear();
                let lines = match result {
                    Ok(Some(c)) => {
                        let fp = |f: &Option<String>| f.clone().unwrap_or_else(|| "—".into());
                        let n = |v: Option<u8>| v.map(|v| v.to_string()).unwrap_or_else(|| "?".into());
                        vec![
                            format!("Card:        {}", c.name()),
                            format!("Reader:      {}", c.reader),
                            format!("Vendor:      {}", c.vendor),
                            format!("Cardholder:  {}", if c.holder.is_empty() { "—" } else { &c.holder }),
                            format!("Signature:   {}", fp(&c.fingerprints[0])),
                            format!("Encryption:  {}", fp(&c.fingerprints[1])),
                            format!("Auth:        {}", fp(&c.fingerprints[2])),
                            format!("PIN retries: user {} · reset {} · admin {}", n(c.pin_retries[0]), n(c.pin_retries[1]), n(c.pin_retries[2])),
                            format!("Signatures made: {}", c.signatures_made.map(|v| v.to_string()).unwrap_or_else(|| "?".into())),
                        ]
                    }
                    Ok(None) => vec!["No OpenPGP card connected.".into(), String::new(), "Plug in the YubiKey and press the key again.".into()],
                    Err(e) => vec![format!("Error querying the card: {e}")],
                };
                self.mode = Mode::Card(lines);
            }
            Event::InlineImage { id, mode, key, result } => {
                let Some(r) = &mut self.reader else { return };
                if r.id != id {
                    return;
                }
                if let Some(body) = r.body.as_mut().filter(|b| b.mode == mode) {
                    body.inline.insert(key, match result {
                        Ok(p) => InlineImage::Ready(p),
                        Err(e) => InlineImage::Failed(e),
                    });
                }
            }
            Event::Image { id, attachment, result } => {
                let Some(r) = &mut self.reader else { return };
                let Some(slot) = &mut r.image else { return };
                if r.id != id || slot.attachment != attachment {
                    return;
                }
                slot.state = Some(match (result, &self.picker) {
                    (Ok(img), Some(p)) => Ok(p.new_resize_protocol(img)),
                    (Err(e), _) => Err(e),
                    (Ok(_), None) => Err("no image protocol".into()),
                });
                self.needs_clear = true;
            }
        }
    }

    fn on_mailboxes(&mut self, account: usize, list: Vec<MailboxInfo>) {
        let Some(acc) = self.accounts.get_mut(account) else { return };
        acc.mailboxes = list;
        acc.error = None;
        let selected = self.sidebar.get(self.sidebar_sel).cloned();
        self.sidebar = self
            .accounts
            .iter()
            .enumerate()
            .flat_map(|(a, acc)| acc.mailboxes.iter().map(move |m| (a, m.id.clone())))
            .collect();
        self.sidebar_sel = selected
            .and_then(|k| self.sidebar.iter().position(|x| *x == k))
            .unwrap_or(0)
            .min(self.sidebar.len().saturating_sub(1));
        // First available mailbox: open it automatically.
        if self.current.is_none() && !self.sidebar.is_empty() {
            self.open_mailbox(self.sidebar_sel);
        }
        // While the current mailbox lives in the UI, its counters are computed here.
        self.update_current_counts();
    }

    fn set_envelopes(&mut self, envelopes: Vec<Envelope>) {
        let keep = self.selected_envelope().map(|i| self.envelopes[i].id);
        self.native = envelopes.len();
        self.envelopes = envelopes;
        // Your sent mail joins (only to complete conversations; see refresh_visible).
        self.foreign.clear();
        if let (Some(cur), Some((key, sent))) = (&self.current, &self.related)
            && key.0 == cur.0
            && key.1 != cur.1
        {
            for (n, e) in sent.iter().enumerate() {
                let list_id = MessageId(u64::MAX - n as u64);
                self.foreign.insert(list_id, (key.clone(), e.id));
                self.envelopes.push(Envelope { id: list_id, from_name: "me".into(), ..e.clone() });
            }
        }
        self.refresh_visible();
        let pos = keep.and_then(|id| self.visible.iter().position(|&i| self.envelopes[i].id == id));
        self.list_state.select(match pos {
            Some(p) => Some(p),
            None if self.visible.is_empty() => None,
            None => Some(self.list_state.selected().unwrap_or(0).min(self.visible.len() - 1)),
        });
        self.update_current_counts();
    }

    fn update_current_counts(&mut self) {
        let Some((a, id)) = self.current.clone() else { return };
        if self.loading {
            return;
        }
        let unread = self.envelopes[..self.native].iter().filter(|e| !e.flags.seen).count();
        let total = self.native;
        if let Some(mb) = self.accounts.get_mut(a).and_then(|acc| acc.mailboxes.iter_mut().find(|m| m.id == id)) {
            mb.unread = unread;
            mb.total = total;
        }
    }

    fn on_message(&mut self, account: usize, mailbox: String, id: MessageId, result: Result<Box<MessageView>, String>) {
        let auto = self.cfg.ui.auto_show_images && self.picker.is_some();
        let prefer_html = self.cfg.ui.prefer_html;
        let Some(r) = &mut self.reader else { return };
        if r.remote_id != id || r.mailbox != (account, mailbox) {
            return;
        }
        match result {
            Ok(view) => {
                let body = Body::new(&view, prefer_html);
                r.images = body.pane_images(&view);
                r.show_images = auto && !r.images.is_empty();
                r.body = Some(body);
                if r.unlocking && view.crypto.as_ref().is_some_and(|c| c.decrypted) {
                    self.status.clear();
                }
                r.unlocking = false;
                let problem = view.crypto.as_ref().filter(|c| c.encrypted && !c.decrypted).and_then(|c| c.problem.clone());
                r.view = Some(*view);
                if self.cfg.ui.remote_images == orion_core::config::RemoteImages::Always {
                    self.load_remote_images();
                }
                self.load_image();
                // If the PIN is needed, ask for it right away.
                if self.focus == Focus::Reader {
                    match problem {
                        Some(CryptoProblem::NeedPin { prompt }) => self.ask_pin(prompt),
                        Some(CryptoProblem::BadPin { retries }) => {
                            self.status = match retries {
                                Some(n) => format!("wrong PIN: {n} attempts left"),
                                None => "wrong PIN or passphrase".into(),
                            };
                            self.ask_pin("PIN / passphrase".into());
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                r.error = Some(e);
                r.unlocking = false;
            }
        }
    }

    fn ask_pin(&mut self, prompt: String) {
        self.mode = Mode::Pin { prompt, pin: Zeroizing::new(String::new()), purpose: PinPurpose::Unlock };
    }

    /// Decryption problem of the open message, if any.
    fn locked_problem(&self) -> Option<CryptoProblem> {
        let c = self.reader.as_ref()?.view.as_ref()?.crypto.as_ref()?;
        (c.encrypted && !c.decrypted).then(|| c.problem.clone()).flatten()
    }

    fn unlock(&mut self, pin: Option<Zeroizing<String>>) {
        let Some(r) = &mut self.reader else { return };
        r.unlocking = true;
        r.view = None;
        r.body = None;
        let (account, mailbox, id) = (r.mailbox.0, r.mailbox.1.clone(), r.remote_id);
        self.request(account, Request::Unlock { mailbox, id, pin });
    }

    /// Imports the key the open message offers (Autocrypt, attachment or
    /// keyserver by fingerprint) and accepts it for that sender's address.
    fn import_sender_key(&mut self) {
        let Some(offer) = self.reader.as_ref().and_then(|r| r.view.as_ref()).and_then(|v| v.key_offer.clone()) else {
            self.status = "this message offers no new key".into();
            return;
        };
        let Some(gpg) = self.gpg.clone() else {
            self.status = "OpenPGP is disabled in the configuration".into();
            return;
        };
        let keyservers = self.cfg.pgp.keyservers.clone();
        self.status = format!("importing the key for {} ({})…", offer.addr, offer.source.label());
        let tx = self.events.clone();
        std::thread::spawn(move || {
            use orion_core::model::KeySource;
            let imported = match &offer.source {
                KeySource::Autocrypt(d) | KeySource::Attachment(d) => gpg.import_keys(d),
                KeySource::Keyserver => gpg.recv_key(&offer.fingerprint, &keyservers),
            };
            let result = match imported {
                Err(e) => Err(format!("could not import the key: {e}")),
                Ok(fprs) if !fprs.iter().any(|f| f.eq_ignore_ascii_case(&offer.fingerprint)) => {
                    Err(format!("the key {} was not among the imported ones", orion_crypto::gpg::pretty_fingerprint(&offer.fingerprint)))
                }
                Ok(_) => match gpg.encryption_key(&offer.addr) {
                    // Only accept it for replying if the key really carries that address.
                    Some(k) if k.fingerprint.eq_ignore_ascii_case(&offer.fingerprint) => {
                        match orion_crypto::accepted::AcceptedKeys::accept(&orion_crypto::accepted::default_path(), &offer.addr, &k.fingerprint) {
                            Ok(()) => Ok(format!("key for {} imported and accepted ({}): replies can be encrypted", offer.addr, orion_crypto::gpg::pretty_fingerprint(&k.fingerprint))),
                            Err(e) => Err(format!("imported, but could not save the acceptance: {e}")),
                        }
                    }
                    _ => Ok(format!("key imported (the signature can be checked), but it does not carry {}: not used to encrypt to that address", offer.addr)),
                },
            };
            let _ = tx.send(Event::KeyImported(result));
        });
    }

    /// `http(s)` images of the open message, minus tracking pixels.
    pub fn remote_image_urls(view: &MessageView) -> Vec<String> {
        let Some(doc) = &view.html_doc else { return Vec::new() };
        let mut urls: Vec<String> =
            doc.images.iter().filter(|i| orion_backends::remote::wanted(&i.src, i.width, i.height)).map(|i| i.src.trim().to_string()).collect();
        urls.sort();
        urls.dedup();
        urls
    }

    fn load_remote_images(&mut self) {
        if self.cfg.ui.remote_images == orion_core::config::RemoteImages::Never {
            self.status = "remote images are disabled (remote_images = \"never\")".into();
            return;
        }
        let Some(r) = &mut self.reader else { return };
        let Some(view) = &r.view else { return };
        if r.remote != RemoteState::NotLoaded {
            return;
        }
        let urls = Self::remote_image_urls(view);
        if urls.is_empty() {
            self.status = "this message has no remote images".into();
            return;
        }
        r.remote = RemoteState::Loading;
        let (account, mailbox, id) = (r.mailbox.0, r.mailbox.1.clone(), r.id);
        let tx = self.events.clone();
        std::thread::spawn(move || {
            let (fetched, failed) = orion_backends::remote::fetch_images(&urls);
            let images = fetched
                .into_iter()
                .map(|f| orion_core::model::Attachment { name: f.url, mime: f.mime, inline: true, content_id: None, data: f.data })
                .collect();
            let _ = tx.send(Event::RemoteImages { account, mailbox, id, images, failed });
        });
    }

    fn on_remote_images(&mut self, account: usize, mailbox: String, id: MessageId, images: Vec<orion_core::model::Attachment>, failed: usize) {
        let prefer_html = self.cfg.ui.prefer_html;
        let Some(r) = &mut self.reader else { return };
        if r.id != id || r.mailbox != (account, mailbox) {
            return;
        }
        let Some(view) = &mut r.view else { return };
        let n = images.len();
        view.attachments.extend(images);
        let mode = r.body.as_ref().map(|b| b.mode);
        // In the original view, draw it again with the images.
        let redraw_original = mode == Some(BodyMode::Original);
        let mode = if redraw_original { Some(BodyMode::Html) } else { mode };
        let mut body = Body::new(view, prefer_html);
        if let Some(m) = mode {
            body.mode = m;
        }
        r.images = body.pane_images(view);
        r.body = Some(body);
        r.remote = RemoteState::Loaded;
        self.needs_clear = true;
        self.status = if failed > 0 { format!("{n} remote image(s) loaded, {failed} failed") } else { format!("{n} remote image(s) loaded") };
        if redraw_original {
            self.original_view();
        }
    }

    fn card_status(&mut self) {
        let Some(gpg) = self.gpg.clone() else {
            self.status = "PGP is disabled in the configuration".into();
            return;
        };
        self.status = "querying the card…".into();
        let tx = self.events.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Event::Card(gpg.card_status().map_err(|e| e.to_string())));
        });
    }

    // -------------------------------------------------------------- actions

    fn open_mailbox(&mut self, sel: usize) {
        let Some(key) = self.sidebar.get(sel).cloned() else { return };
        self.sidebar_sel = sel;
        if self.current.as_ref() != Some(&key) {
            self.current = Some(key.clone());
            self.envelopes.clear();
            self.native = 0;
            self.visible.clear();
            self.list_state.select(None);
            self.filter.clear();
            self.close_reader();
        }
        self.loading = true;
        self.sync_error = None;
        self.request(key.0, Request::Open { mailbox: key.1 });
    }

    fn ask_delete_forever(&mut self, id: MessageId, key: MailboxKey) {
        let subject = self.envelope_index(id).map(|i| self.envelopes[i].subject.clone()).unwrap_or_default();
        let subject: String = if subject.chars().count() > 50 { subject.chars().take(49).chain(['…']).collect() } else { subject };
        self.mode = Mode::ConfirmDelete { prompt: format!("delete “{subject}” for good? It cannot be undone"), id, key };
    }

    /// Trash or Spam (by name or role): where deleting means for good.
    pub fn is_disposal_folder(&self, key: &MailboxKey) -> bool {
        let name = self.mailbox(key).map(|m| m.name.clone()).unwrap_or_default();
        matches!(crate::theme::Icon::for_mailbox(&key.1, &name), crate::theme::Icon::Trash | crate::theme::Icon::Junk)
    }

    /// Folders a message can be moved to: the account's other mailboxes,
    /// matching the filter, best first. (id, name)
    pub fn move_targets(&self, key: &MailboxKey, filter: &str) -> Vec<(String, String)> {
        let q = filter.to_lowercase();
        let mut v: Vec<(u8, String, String)> = self
            .accounts
            .get(key.0)
            .map(|a| &a.mailboxes[..])
            .unwrap_or_default()
            .iter()
            .filter(|m| m.id != key.1)
            .filter_map(|m| crate::filepicker::rank(&m.name.to_lowercase(), &q).map(|r| (r, m.id.clone(), m.name.clone())))
            .collect();
        v.sort_by_key(|(r, _, _)| *r);
        v.into_iter().map(|(_, id, name)| (id, name)).collect()
    }

    /// Takes the message out of the list at once and asks the account to
    /// delete/move it; if that fails the list is reloaded (`on_removed`).
    fn remove_message(&mut self, id: MessageId, key: MailboxKey, how: crate::worker::Removal) {
        if self.current.as_ref() == Some(&key) {
            let pos = self.list_state.selected();
            if let Some(i) = self.envelope_index(id) {
                self.envelopes.remove(i);
                if i < self.native {
                    self.native -= 1;
                }
            }
            self.refresh_visible();
            self.list_state.select(if self.visible.is_empty() { None } else { Some(pos.unwrap_or(0).min(self.visible.len() - 1)) });
            self.update_current_counts();
        }
        // The open message is gone: show the next one, or close the reader.
        if self.reader.as_ref().is_some_and(|r| r.id == id) {
            match self.selected_envelope() {
                Some(i) if self.focus == Focus::Reader => self.open_message(i),
                Some(_) if self.wide => {
                    self.reader = None;
                    self.schedule_preview();
                }
                _ => {
                    self.reader = None;
                    if self.focus == Focus::Reader {
                        self.focus = Focus::List;
                    }
                }
            }
            self.needs_clear = true;
        }
        self.status = match &how {
            crate::worker::Removal::Trash => "moving to the Trash…".into(),
            crate::worker::Removal::Forever => "deleting…".into(),
            crate::worker::Removal::MoveTo(_) => "moving…".into(),
        };
        let (key, remote) = self.locate(&key, id);
        self.request(key.0, Request::Remove { mailbox: key.1, ids: vec![remote], how });
    }

    fn on_removed(&mut self, account: usize, mailbox: String, result: Result<Option<String>, String>) {
        let name = |id: &str| self.accounts.get(account).and_then(|a| a.mailboxes.iter().find(|m| m.id == id)).map(|m| m.name.clone()).unwrap_or_else(|| id.to_string());
        self.status = match result {
            Ok(Some(dest)) => format!("moved to {}", name(&dest)),
            Ok(None) => "deleted for good".into(),
            Err(e) => {
                // Bring the message back: the list comes again from the account.
                if self.current.as_ref() == Some(&(account, mailbox)) {
                    self.reload();
                }
                format!("error: {e}")
            }
        };
    }

    fn reload(&mut self) {
        for acc in &self.accounts {
            let _ = acc.tx.send(Request::Refresh);
        }
        self.status = "updating…".into();
    }

    fn refresh_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        // Only the open mailbox's own messages are listed (and searched).
        let mut v: Vec<usize> = (0..self.native)
            .filter(|&i| {
                needle.is_empty() || {
                    let e = &self.envelopes[i];
                    e.subject.to_lowercase().contains(&needle)
                        || e.from_name.to_lowercase().contains(&needle)
                        || e.from_addr.to_lowercase().contains(&needle)
                }
            })
            .collect();
        let envs = &self.envelopes;
        let newer = |a: usize, b: usize| envs[b].timestamp.cmp(&envs[a].timestamp).then(envs[b].id.cmp(&envs[a].id));
        let desc = self.cfg.ui.sort == SortOrder::DateDesc;
        if self.threads {
            // One row per conversation (its newest message), placed by that
            // message's date; expanded ones list the older messages below.
            // Your sent mail is grouped too, but a conversation is only shown
            // if it has a message of this mailbox.
            let with_sent: Vec<usize> = v.iter().copied().chain(self.native..envs.len()).collect();
            let native = self.native;
            let mut groups: Vec<Vec<usize>> = crate::threads::group(envs, &with_sent).into_iter().filter(|g| g.iter().any(|&i| i < native)).collect();
            groups.sort_by(|a, b| if desc { newer(a[0], b[0]) } else { newer(b[0], a[0]) });
            let (mut visible, mut rows) = (Vec::with_capacity(v.len()), Vec::with_capacity(v.len()));
            for g in groups {
                let key = envs[*g.last().expect("non-empty")].id;
                let open = g.len() > 1 && self.expanded.contains(&key);
                visible.push(g[0]);
                rows.push(RowInfo { members: if g.len() > 1 { g.clone() } else { Vec::new() }, child: false, key, expanded: open });
                if open {
                    for &i in &g[1..] {
                        visible.push(i);
                        rows.push(RowInfo { members: Vec::new(), child: true, key, expanded: true });
                    }
                }
            }
            self.visible = visible;
            self.rows = rows;
        } else {
            v.sort_by(|&a, &b| if desc { newer(a, b) } else { newer(b, a) });
            self.rows = v.iter().map(|&i| RowInfo { members: Vec::new(), child: false, key: envs[i].id, expanded: false }).collect();
            self.visible = v;
        }
        let sel = self.list_state.selected().unwrap_or(0);
        self.list_state.select(if self.visible.is_empty() { None } else { Some(sel.min(self.visible.len() - 1)) });
    }

    /// Shows the selected message in the third pane shortly after the
    /// selection stops moving (holding `j` does not download every message).
    fn schedule_preview(&mut self) {
        if !self.wide || self.focus != Focus::List {
            return;
        }
        self.preview_seq += 1;
        let (seq, tx) = (self.preview_seq, self.events.clone());
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = tx.send(Event::Preview { seq });
        });
    }

    fn on_preview(&mut self, seq: u64) {
        if seq != self.preview_seq || !self.wide || self.focus != Focus::List {
            return;
        }
        let Some(i) = self.selected_envelope() else { return };
        if self.reader.as_ref().is_some_and(|r| r.id == self.envelopes[i].id) {
            return;
        }
        self.show_message(i, false);
    }

    fn on_preview_read(&mut self, id: MessageId) {
        let Some((key, remote)) = self.reader.as_ref().filter(|r| r.id == id).map(|r| (r.mailbox.clone(), r.remote_id)) else { return };
        if self.envelope_index(id).is_some_and(|i| !self.envelopes[i].flags.seen) {
            self.apply_flag_locally(id, Flag::Seen, true);
            self.request(key.0, Request::SetFlag { mailbox: key.1, ids: vec![remote], flag: Flag::Seen, on: true, quiet: true });
        }
    }

    /// Whether a row of the list is one of your sent messages.
    pub fn is_sent(&self, index: usize) -> bool {
        index >= self.native
    }

    /// Where a listed message really is: the open mailbox, or Sent for your
    /// replies shown in its conversations.
    fn locate(&self, key: &MailboxKey, id: MessageId) -> (MailboxKey, MessageId) {
        self.foreign.get(&id).cloned().unwrap_or_else(|| (key.clone(), id))
    }

    fn open_message(&mut self, env_idx: usize) {
        self.show_message(env_idx, true);
    }

    /// Opens a message: `focus` (Enter) moves the focus to it and marks it
    /// read at once; a preview keeps the focus on the list and marks it read
    /// after `mark_read_after` seconds on screen.
    fn show_message(&mut self, env_idx: usize, focus: bool) {
        let Some(current) = self.current.clone() else { return };
        let id = self.envelopes[env_idx].id;
        let (key, remote_id) = self.locate(&current, id);
        if self.reader.as_ref().is_some_and(|r| r.show_images) {
            self.needs_clear = true;
        }
        self.reader = Some(Reader {
            id,
            remote_id,
            mailbox: key.clone(),
            view: None,
            body: None,
            error: None,
            unlocking: false,
            scroll: 0,
            max_scroll: 0,
            page: 10,
            show_headers: false,
            images: Vec::new(),
            image_idx: 0,
            remote: RemoteState::NotLoaded,
            rendering: false,
            show_images: false,
            image: None,
        });
        if focus {
            self.focus = Focus::Reader;
        }
        self.request(key.0, Request::Fetch { mailbox: key.1.clone(), id: remote_id });
        if self.envelopes[env_idx].flags.seen {
            return;
        }
        let delay = self.cfg.ui.mark_read_after;
        if focus || delay == 0 {
            self.apply_flag_locally(id, Flag::Seen, true);
            self.request(key.0, Request::SetFlag { mailbox: key.1, ids: vec![remote_id], flag: Flag::Seen, on: true, quiet: true });
        } else {
            let tx = self.events.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(delay));
                let _ = tx.send(Event::PreviewRead { id });
            });
        }
    }

    fn apply_flag_locally(&mut self, id: MessageId, flag: Flag, on: bool) {
        if let Some(i) = self.envelope_index(id) {
            self.envelopes[i].flags.set(flag, on);
            self.update_current_counts();
        }
    }

    /// Message that flag changes act on: the open one or the selected one.
    fn target_message(&self) -> Option<MessageId> {
        match (&self.reader, self.focus) {
            (Some(r), Focus::Reader) => Some(r.id),
            _ => self.selected_envelope().map(|i| self.envelopes[i].id),
        }
    }

    fn toggle_flag(&mut self, flag: Flag) {
        let (Some(id), Some(key)) = (self.target_message(), self.current.clone()) else { return };
        let Some(i) = self.envelope_index(id) else { return };
        let on = !self.envelopes[i].flags.get(flag);
        self.apply_flag_locally(id, flag, on);
        let (key, remote) = self.locate(&key, id);
        self.request(key.0, Request::SetFlag { mailbox: key.1, ids: vec![remote], flag, on, quiet: false });
    }

    fn load_image(&mut self) {
        let Some(r) = &mut self.reader else { return };
        let Some(view) = &r.view else { return };
        if !r.show_images || self.picker.is_none() {
            return;
        }
        let Some(&att) = r.images.get(r.image_idx) else { return };
        if r.image.as_ref().is_some_and(|s| s.attachment == att) {
            return;
        }
        r.image = Some(ImageSlot { attachment: att, state: None });
        // Decoding can take tens of ms (large JPEGs): keep it off the UI thread.
        let data = view.attachments[att].data.clone();
        let (tx, id) = (self.events.clone(), r.id);
        std::thread::spawn(move || {
            let result = image::load_from_memory(&data).map_err(|e| e.to_string());
            let _ = tx.send(Event::Image { id, attachment: att, result });
        });
        self.needs_clear = true;
    }

    /// Starts encoding the inline images about to be shown.
    pub fn request_inline_images(&mut self, keys: Vec<InlineKey>) {
        let Some(picker) = self.picker.clone() else { return };
        let Some(r) = &mut self.reader else { return };
        let (Some(view), Some(body)) = (&r.view, &mut r.body) else { return };
        for key in keys {
            if body.inline.contains_key(&key) {
                continue;
            }
            let Some(data) = body.image_data(view, key.0) else { continue };
            body.inline.insert(key, InlineImage::Pending);
            let (tx, id, picker, mode) = (self.events.clone(), r.id, picker.clone(), body.mode);
            std::thread::spawn(move || {
                let result = image::load_from_memory(&data).map_err(|e| e.to_string()).and_then(|img| {
                    ratatui_image::sliced::SlicedProtocol::new_with_resize(
                        &picker,
                        img,
                        ratatui::layout::Size::new(key.1, key.2),
                        ratatui_image::Resize::Fit(None),
                    )
                    .map(Box::new)
                    .map_err(|e| e.to_string())
                });
                let _ = tx.send(Event::InlineImage { id, mode, key, result });
            });
        }
    }

    /// `V`: the HTML as a browser draws it (Chromium, offline), or back.
    fn original_view(&mut self) {
        let cell_w = self.picker.as_ref().map(|p| p.font_size().width as u32);
        let browser = crate::snapshot::find_browser(self.cfg.ui.html_renderer.as_deref());
        let Some(r) = &mut self.reader else { return };
        let (Some(view), Some(body)) = (&r.view, &mut r.body) else { return };
        let Some(html) = view.html.clone() else {
            self.status = "this message has no HTML version".into();
            return;
        };
        if body.mode == BodyMode::Original || body.snapshot.is_some() {
            body.set_original(None);
            r.scroll = 0;
            self.needs_clear = true;
            return;
        }
        let Some(cell_w) = cell_w else {
            self.status = "the original view needs a terminal with images (sixel/kitty)".into();
            return;
        };
        let Some(browser) = browser else {
            self.status = "the original view needs Chromium: install it, or set [ui] html_renderer".into();
            return;
        };
        if r.rendering {
            return;
        }
        r.rendering = true;
        let width = body.width().unwrap_or(100) as u32 * cell_w;
        let page = crate::snapshot::prepare_with(&html, &view.attachments, self.cfg.ui.link_markers);
        let (tx, id) = (self.events.clone(), r.id);
        self.status = "drawing the original with Chromium…".into();
        std::thread::spawn(move || {
            let result = crate::snapshot::render(&browser, &page, width).map(|s| (s.png, s.size));
            let _ = tx.send(Event::Snapshot { id, result });
        });
    }

    fn on_snapshot(&mut self, id: MessageId, result: Result<(Vec<u8>, (u32, u32)), String>) {
        let Some(r) = &mut self.reader else { return };
        if r.id != id {
            return;
        }
        r.rendering = false;
        match result {
            Ok(snap) => {
                if let Some(body) = &mut r.body {
                    body.set_original(Some(snap));
                    r.scroll = 0;
                    self.needs_clear = true;
                    self.status = "original view · V: back to the text rendering".into();
                }
            }
            Err(e) => self.status = format!("original view: {e}"),
        }
    }

    fn toggle_view(&mut self) {
        let Some(r) = &mut self.reader else { return };
        let (Some(view), Some(body)) = (&r.view, &mut r.body) else { return };
        if !body.toggle_mode(view) {
            self.status = "this message has no HTML version".into();
            return;
        }
        r.images = body.pane_images(view);
        r.image_idx = 0;
        r.image = None;
        r.scroll = 0;
        if r.show_images && r.images.is_empty() {
            r.show_images = false;
        }
        self.needs_clear = true;
        self.load_image();
    }

    fn links(&self) -> &[String] {
        self.reader.as_ref().and_then(|r| r.body.as_ref()).map(|b| b.links.as_slice()).unwrap_or(&[])
    }

    fn open_link(&mut self, input: &str) {
        let links = self.links();
        let n = match input.trim() {
            "" if links.len() == 1 => 1,
            s => match s.parse::<usize>() {
                Ok(n) => n,
                Err(_) => {
                    self.status = "type the link number".into();
                    return;
                }
            },
        };
        let Some(url) = links.get(n.wrapping_sub(1)).cloned() else {
            self.status = format!("link {n} does not exist");
            return;
        };
        if !safe_url(&url) {
            self.status = format!("link not allowed: {url}");
            return;
        }
        let mut parts = self.cfg.ui.open_command.split_whitespace();
        let Some(prog) = parts.next() else { return };
        let res = std::process::Command::new(prog)
            .args(parts)
            .arg(&url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        self.status = match res {
            Ok(_) => format!("opening {url}"),
            Err(e) => format!("error: could not run {prog}: {e}"),
        };
    }

    /// Attachments of the open message the user can save: everything but
    /// the remote images Orion downloaded itself.
    pub fn saveable_attachments(&self) -> Vec<&orion_core::model::Attachment> {
        let Some(view) = self.reader.as_ref().and_then(|r| r.view.as_ref()) else { return Vec::new() };
        view.attachments.iter().filter(|a| !a.name.starts_with("http://") && !a.name.starts_with("https://")).collect()
    }

    /// Saves one attachment (or all) to the downloads folder, never
    /// overwriting: "name (1).pdf" if "name.pdf" exists.
    fn save_attachments(&mut self, which: Option<usize>) {
        let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| std::path::PathBuf::from("."));
        let list = self.saveable_attachments();
        let chosen: Vec<_> = match which {
            Some(i) => list.get(i).into_iter().copied().collect(),
            None => list,
        };
        let mut saved = Vec::new();
        let mut error = None;
        for a in chosen {
            match write_unique(&dir, &a.name, &a.data) {
                Ok(p) => saved.push(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                Err(e) => error = Some(e.to_string()),
            }
        }
        let shown = dir.strip_prefix(dirs::home_dir().unwrap_or_default()).map(|p| format!("~/{}", p.display())).unwrap_or_else(|_| dir.display().to_string());
        self.status = match (saved.len(), error) {
            (_, Some(e)) => format!("error: could not save to {shown}: {e}"),
            (1, None) => format!("saved {} to {shown}", saved[0]),
            (n, None) => format!("saved {n} attachments to {shown}"),
        };
    }

    /// Opens an attachment with `open_command` (xdg-open), from a private copy
    /// in ~/.cache/orion/open.
    fn open_attachment(&mut self, i: usize) {
        let Some(a) = self.saveable_attachments().get(i).map(|a| (a.name.clone(), a.data.clone())) else { return };
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("orion").join("open");
        let path = match std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)))
            .and_then(|_| write_unique(&dir, &a.0, &a.1))
        {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        };
        let mut parts = self.cfg.ui.open_command.split_whitespace();
        let Some(prog) = parts.next() else { return };
        let child = std::process::Command::new(prog)
            .args(parts)
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        self.status = match child {
            Ok(mut c) => {
                std::thread::spawn(move || c.wait());
                format!("opening {}", a.0)
            }
            Err(e) => format!("error: could not run {prog}: {e}"),
        };
    }

    /// Esc / q in the message: in three panes the message stays on the right
    /// and the focus goes back to the list; otherwise it closes.
    fn leave_reader(&mut self) {
        if self.wide {
            self.focus = Focus::List;
        } else {
            self.close_reader();
        }
    }

    fn close_reader(&mut self) {
        if self.reader.take().is_some_and(|r| r.show_images) {
            self.needs_clear = true;
        }
        if self.focus == Focus::Reader {
            self.focus = Focus::List;
        }
    }

    // ------------------------------------------------------------- keyboard

    pub fn on_key(&mut self, ev: KeyEvent) {
        match &mut self.mode {
            Mode::Search(q) => {
                match ev.code {
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        q.pop();
                        self.filter = q.clone();
                    }
                    KeyCode::Char(c) => {
                        q.push(c);
                        self.filter = q.clone();
                    }
                    _ => {}
                }
                self.refresh_visible();
                return;
            }
            Mode::OpenLink(q) => {
                match ev.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let q = std::mem::take(q);
                        self.mode = Mode::Normal;
                        self.open_link(&q);
                    }
                    KeyCode::Backspace => {
                        q.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => q.push(c),
                    _ => {}
                }
                return;
            }
            Mode::Pin { pin, purpose, .. } => {
                let purpose = *purpose;
                match ev.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let pin = std::mem::replace(pin, Zeroizing::new(String::new()));
                        self.mode = Mode::Normal;
                        match purpose {
                            PinPurpose::Unlock => self.unlock(Some(pin)),
                            PinPurpose::Send => self.compose_send(Some(pin)),
                        }
                    }
                    KeyCode::Backspace => {
                        pin.pop();
                    }
                    KeyCode::Char(c) => pin.push(c),
                    _ => {}
                }
                return;
            }
            Mode::Attachments { sel } => {
                let i = *sel;
                self.mode = Mode::Normal;
                let n = self.saveable_attachments().len();
                match ev.code {
                    KeyCode::Up | KeyCode::Char('k') => self.mode = Mode::Attachments { sel: i.saturating_sub(1) },
                    KeyCode::Down | KeyCode::Char('j') => self.mode = Mode::Attachments { sel: (i + 1).min(n.saturating_sub(1)) },
                    KeyCode::Enter => self.save_attachments(Some(i)),
                    KeyCode::Char('a') => self.save_attachments(None),
                    KeyCode::Char('o') => self.open_attachment(i),
                    _ => {}
                }
                return;
            }
            Mode::ConfirmEmpty { key, .. } => {
                let key = key.clone();
                self.mode = Mode::Normal;
                if matches!(ev.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    if self.current.as_ref() == Some(&key) {
                        self.envelopes.clear();
                        self.native = 0;
                        self.refresh_visible();
                        self.list_state.select(None);
                        self.update_current_counts();
                        if self.reader.as_ref().is_some_and(|r| r.mailbox == key) {
                            self.close_reader();
                        }
                    }
                    self.status = "emptying…".into();
                    self.request(key.0, Request::Empty { mailbox: key.1 });
                } else {
                    self.status = "not emptied".into();
                }
                return;
            }
            Mode::ConfirmDelete { id, key, .. } => {
                let (id, key) = (*id, key.clone());
                self.mode = Mode::Normal;
                if matches!(ev.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    self.remove_message(id, key, crate::worker::Removal::Forever);
                } else {
                    self.status = "not deleted".into();
                }
                return;
            }
            Mode::MoveTo { .. } => {
                let Mode::MoveTo { id, key, mut filter, mut sel } = std::mem::replace(&mut self.mode, Mode::Normal) else { unreachable!() };
                let n = self.move_targets(&key, &filter).len();
                let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
                match ev.code {
                    KeyCode::Esc => return,
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Down => sel = (sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Char('p') if ctrl => sel = sel.saturating_sub(1),
                    KeyCode::Char('n') if ctrl => sel = (sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Backspace => {
                        filter.pop();
                        sel = 0;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        filter.push(c);
                        sel = 0;
                    }
                    KeyCode::Enter => {
                        if let Some(dest) = self.move_targets(&key, &filter).get(sel).map(|m| m.0.clone()) {
                            self.remove_message(id, key, crate::worker::Removal::MoveTo(dest));
                        }
                        return;
                    }
                    _ => {}
                }
                self.mode = Mode::MoveTo { id, key, filter, sel };
                return;
            }
            Mode::Help | Mode::Card(_) => {
                self.mode = Mode::Normal;
                return;
            }
            Mode::Normal => {}
        }
        if self.compose.is_some() {
            self.status.clear();
            self.compose_key(&ev);
            return;
        }
        if let Some(action) = self.keymap.action(&ev) {
            // The status message lasts until the next action.
            self.status.clear();
            self.on_action(action);
        }
    }

    fn on_action(&mut self, action: Action) {
        use Action::*;
        match action {
            ForceQuit => self.quit = true,
            Quit => match self.focus {
                Focus::Reader => self.leave_reader(),
                _ => self.quit = true,
            },
            Help => self.mode = Mode::Help,
            Search => {
                self.close_reader();
                self.focus = Focus::List;
                self.mode = Mode::Search(self.filter.clone());
            }
            Reload => self.reload(),
            ToggleThread => {
                let Some(row) = self.list_state.selected().and_then(|s| self.rows.get(s)).cloned() else { return };
                if row.members.is_empty() && !row.child {
                    self.status = "this message is not part of a conversation".into();
                    return;
                }
                if !self.expanded.remove(&row.key) {
                    self.expanded.insert(row.key);
                }
                self.refresh_visible();
                // Stay on the conversation's row.
                if let Some(pos) = self.rows.iter().position(|r| r.key == row.key && !r.child) {
                    self.list_state.select(Some(pos));
                }
            }
            Threads => {
                let keep = self.selected_envelope().map(|i| self.envelopes[i].id);
                self.threads = !self.threads;
                self.refresh_visible();
                if let Some(pos) = keep.and_then(|id| self.visible.iter().position(|&i| self.envelopes[i].id == id)) {
                    self.list_state.select(Some(pos));
                }
                self.status = if self.threads { "grouped by conversation".into() } else { "one row per message".into() };
            }
            SaveAttachments => {
                if self.saveable_attachments().is_empty() {
                    self.status = if self.reader.is_some() { "this message has no attachments".into() } else { "open a message first".into() };
                } else {
                    self.mode = Mode::Attachments { sel: 0 };
                }
            }
            Delete => {
                if let (Some(id), Some(key)) = (self.target_message(), self.current.clone()) {
                    // In the Trash (or Spam) there is nowhere else to go: ask to delete for good.
                    if self.is_disposal_folder(&key) {
                        self.ask_delete_forever(id, key);
                    } else {
                        self.remove_message(id, key, crate::worker::Removal::Trash);
                    }
                }
            }
            EmptyFolder => match self.current.clone() {
                Some(key) if self.is_disposal_folder(&key) => {
                    let (name, n) = self.mailbox(&key).map(|m| (m.name.clone(), m.total)).unwrap_or_default();
                    let n = n.max(self.envelopes.len());
                    self.mode = Mode::ConfirmEmpty { prompt: format!("delete all {n} message(s) in {name} for good? It cannot be undone"), key };
                }
                Some(_) => self.status = "only the Trash and Spam folders can be emptied".into(),
                None => {}
            },
            DeleteForever => {
                if let (Some(id), Some(key)) = (self.target_message(), self.current.clone()) {
                    self.ask_delete_forever(id, key);
                }
            }
            MoveTo => {
                if let (Some(id), Some(key)) = (self.target_message(), self.current.clone()) {
                    self.mode = Mode::MoveTo { id, key, filter: String::new(), sel: 0 };
                }
            }
            Compose => self.start_compose(crate::compose_ui::ComposeKind::New),
            Reply => self.start_compose(crate::compose_ui::ComposeKind::Reply),
            ReplyAll => self.start_compose(crate::compose_ui::ComposeKind::ReplyAll),
            Forward => self.start_compose(crate::compose_ui::ComposeKind::Forward),
            FocusNext => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::List,
                    Focus::List if self.reader.is_some() => Focus::Reader,
                    _ => Focus::Sidebar,
                }
            }
            Back => match self.focus {
                Focus::Reader => self.leave_reader(),
                Focus::List => {
                    if !self.filter.is_empty() {
                        self.filter.clear();
                        self.refresh_visible();
                    } else {
                        self.focus = Focus::Sidebar;
                    }
                }
                Focus::Sidebar => {}
            },
            Open => match self.focus {
                Focus::Sidebar => {
                    self.open_mailbox(self.sidebar_sel);
                    self.focus = Focus::List;
                }
                Focus::List => {
                    if let Some(i) = self.selected_envelope() {
                        // Already previewed: go into it (and count it as read).
                        if self.reader.as_ref().is_some_and(|r| r.id == self.envelopes[i].id) {
                            self.focus = Focus::Reader;
                            self.on_preview_read(self.envelopes[i].id);
                        } else {
                            self.open_message(i);
                        }
                    }
                }
                Focus::Reader => match self.locked_problem() {
                    Some(CryptoProblem::NeedPin { prompt }) => self.ask_pin(prompt),
                    Some(CryptoProblem::BadPin { .. }) => self.ask_pin("PIN / passphrase".into()),
                    Some(CryptoProblem::CardMissing(_) | CryptoProblem::WrongCard { .. } | CryptoProblem::NotConfirmed | CryptoProblem::Failed(_)) => self.unlock(None),
                    _ => {}
                },
            },
            NextMessage | PrevMessage => {
                let delta = if action == NextMessage { 1 } else { -1 };
                self.move_list(delta);
                if self.focus == Focus::Reader
                    && let Some(i) = self.selected_envelope() {
                        self.open_message(i);
                    }
            }
            ToggleView => self.toggle_view(),
            OriginalView => self.original_view(),
            CardStatus => self.card_status(),
            ImportKey => self.import_sender_key(),
            OpenLink => {
                if self.links().is_empty() {
                    self.status = "this message has no links".into();
                } else {
                    self.mode = Mode::OpenLink(String::new());
                }
            }
            ToggleRead => self.toggle_flag(Flag::Seen),
            ToggleFlagged => self.toggle_flag(Flag::Flagged),
            ToggleHeaders => {
                if let Some(r) = &mut self.reader {
                    r.show_headers = !r.show_headers;
                    r.scroll = 0;
                }
            }
            LoadRemote => self.load_remote_images(),
            ToggleImages => {
                if let Some(r) = &mut self.reader {
                    if r.images.is_empty() {
                        self.status = "this message has no images".into();
                    } else if self.picker.is_none() {
                        self.status = "images are disabled (image_protocol = \"none\")".into();
                    } else {
                        r.show_images = !r.show_images;
                        self.needs_clear = true;
                        self.load_image();
                    }
                }
            }
            NextImage | PrevImage => {
                if let Some(r) = &mut self.reader {
                    let n = r.images.len();
                    if n > 0 {
                        r.image_idx = if action == NextImage { (r.image_idx + 1) % n } else { (r.image_idx + n - 1) % n };
                        r.show_images = true;
                        self.load_image();
                    }
                }
            }
            Down => self.scroll(1),
            Up => self.scroll(-1),
            PageDown => self.scroll(self.page() as i32),
            PageUp => self.scroll(-(self.page() as i32)),
            Top => self.scroll(i32::MIN / 2),
            Bottom => self.scroll(i32::MAX / 2),
        }
    }

    fn page(&self) -> u16 {
        match (self.focus, &self.reader) {
            (Focus::Reader, Some(r)) => r.page.max(1),
            _ => (self.list_page / 2).max(1),
        }
    }

    fn scroll(&mut self, delta: i32) {
        match self.focus {
            Focus::Sidebar => {
                if self.sidebar.is_empty() {
                    return;
                }
                let n = self.sidebar.len() as i32;
                self.sidebar_sel = (self.sidebar_sel as i32 + delta).clamp(0, n - 1) as usize;
            }
            Focus::List => {
                self.move_list(delta);
                self.schedule_preview();
            }
            Focus::Reader => {
                let sixel = self.picker.as_ref().is_some_and(|p| p.protocol_type() == ratatui_image::picker::ProtocolType::Sixel);
                if let Some(r) = &mut self.reader {
                    let before = r.scroll;
                    r.scroll = (r.scroll as i32 + delta).clamp(0, r.max_scroll as i32) as u16;
                    // Sixel pixels are not erased by text drawn over them: when
                    // images move, redraw everything (see `needs_clear`).
                    let images = r.body.as_ref().is_some_and(|b| !b.rendered().placements.is_empty());
                    if sixel && images && r.scroll != before {
                        self.needs_clear = true;
                    }
                }
            }
        }
    }

    fn move_list(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let n = self.visible.len() as i32;
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        self.list_state.select(Some((cur + delta).clamp(0, n - 1) as usize));
    }
}

/// A file name from a message is untrusted: keep only its last component,
/// without control characters or a leading dot (no hidden files).
pub fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "attachment".into() } else { clean }
}

/// Writes `data` as `dir/name`, or "name (1).ext", "name (2).ext"… if taken.
fn write_unique(dir: &std::path::Path, name: &str, data: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let name = safe_file_name(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for n in 0..1000 {
        let candidate = if n == 0 { dir.join(&name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        // create_new: never overwrite, even if another program creates it meanwhile.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut f) => {
                f.write_all(data)?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

#[cfg(test)]
mod attachment_tests {
    use super::{safe_file_name, write_unique};

    #[test]
    fn names_and_collisions() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\x\\evil.exe"), "evil.exe");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("a\u{0}b\nc.pdf"), "abc.pdf");
        assert_eq!(safe_file_name(""), "attachment");
        let d = tempfile::tempdir().unwrap();
        let a = write_unique(d.path(), "contrato.pdf", b"1").unwrap();
        let b = write_unique(d.path(), "contrato.pdf", b"2").unwrap();
        let c = write_unique(d.path(), "../contrato.pdf", b"3").unwrap();
        assert_eq!(a.file_name().unwrap(), "contrato.pdf");
        assert_eq!(b.file_name().unwrap(), "contrato (1).pdf");
        assert_eq!(c.file_name().unwrap(), "contrato (2).pdf");
        assert_eq!(std::fs::read(&a).unwrap(), b"1", "never overwritten");
        assert_eq!(c.parent().unwrap(), d.path(), "stays in the folder");
    }
}
