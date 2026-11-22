//! One thread per account: it owns the backend and its cache connection,
//! serves requests from the UI and periodically checks for new mail.
//! The UI never waits on the network.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;

use orion_core::config::{AccountConfig, AccountKind};
use orion_core::model::{Envelope, Flag, MailboxInfo, MessageId, MessageView, SyncEvent, SyncResult};
use orion_core::{Backend, Error};
use orion_store::Store;

/// What to do with messages that leave a mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// Into the account's Trash (refused if there is none, or already there).
    Trash,
    /// Deleted for good.
    Forever,
    /// Into another mailbox (its id).
    MoveTo(String),
}

pub enum Request {
    /// Re-lists mailboxes and syncs the open and watched mailboxes.
    Refresh,
    /// Does nothing (lets helper threads notice that the account thread is gone).
    Ping,
    /// Builds (signing/encrypting as asked), sends and keeps a copy in Sent.
    Send { draft: Box<orion_core::compose::Draft>, pin: Option<zeroize::Zeroizing<String>>, ticket: u64 },
    /// Opens a mailbox: first whatever is cached, then syncs.
    Open { mailbox: String },
    Fetch { mailbox: String, id: MessageId },
    /// Retries decrypting a message, with a PIN/passphrase if given.
    Unlock { mailbox: String, id: MessageId, pin: Option<zeroize::Zeroizing<String>> },
    /// `quiet`: do not report if the backend does not support changes (e.g. marking read in mbox).
    SetFlag { mailbox: String, ids: Vec<MessageId>, flag: Flag, on: bool, quiet: bool },
    /// Re-read the mailbox counters from the cache (no network).
    Recount,
    /// Deletes every message of a mailbox for good.
    Empty { mailbox: String },
    /// Deletes or moves messages (see `Removal`).
    Remove { mailbox: String, ids: Vec<MessageId>, how: Removal },
}

pub enum Event {
    Input(crossterm::event::Event),
    /// The address book, loaded in the background.
    Contacts(Vec<crate::contacts::Contact>),
    /// Time to preview the selected message (debounce `seq`).
    Preview { seq: u64 },
    /// A previewed message has been shown long enough to count as read.
    PreviewRead { id: MessageId },
    /// Browser rendering of a message (PNG, pixel size).
    Snapshot { id: MessageId, result: Result<(Vec<u8>, (u32, u32)), String> },
    /// Result of emptying a mailbox.
    Emptied { account: usize, mailbox: String, result: Result<(), String> },
    /// Result of deleting/moving messages: `Ok(destination)` (`None` = gone
    /// for good) or the error (the list is reloaded to bring them back).
    Removed { account: usize, mailbox: String, result: Result<Option<String>, String> },
    /// Remote images downloaded for one message.
    RemoteImages { account: usize, mailbox: String, id: MessageId, images: Vec<orion_core::model::Attachment>, failed: usize },
    /// Keystrokes a YubiKey typed by itself (OTP on touch) were discarded.
    OtpIgnored,
    Mailboxes { account: usize, list: Vec<MailboxInfo> },
    Envelopes { account: usize, mailbox: String, envelopes: Vec<Envelope>, from_cache: bool },
    Message { account: usize, mailbox: String, id: MessageId, result: Result<Box<MessageView>, String> },
    /// Progress text (empty = idle).
    Progress { account: usize, text: String },
    Error { account: usize, text: String },
    /// A mailbox sync failed (shown in the list until the next successful sync).
    SyncFailed { account: usize, mailbox: String, error: String },
    /// Unread messages that arrived in a watched mailbox since the last sync.
    NewMail { account: usize, mailbox: String, mailbox_name: String, envelopes: Vec<Envelope> },
    FlagFailed { account: usize, mailbox: String, ids: Vec<MessageId>, flag: Flag, on: bool },
    Image { id: MessageId, attachment: usize, result: Result<image::DynamicImage, String> },
    Card(Result<Option<orion_crypto::CardInfo>, String>),
    /// Result of importing a sender's key.
    KeyImported(Result<String, String>),
    Sent { ticket: u64, result: Result<Option<String>, SendError> },
    /// Result of a key lookup: keys now in the keyring that still need the
    /// user's acceptance (address, fingerprint, user id), and addresses with none.
    KeysLocated { untrusted: Vec<(String, String, String)>, missing: Vec<String> },
    InlineImage {
        id: MessageId,
        /// View it was made for: a late result for another view is dropped.
        mode: crate::body::BodyMode,
        key: crate::body::InlineKey,
        result: Result<Box<ratatui_image::sliced::SlicedProtocol>, String>,
    },
}

/// How the account thread behaves.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Sync the watched mailboxes right at startup (the daemon). The UI leaves
    /// it to the first poll, because it is about to open the inbox anyway.
    pub watch_on_start: bool,
}

/// Why a message was not sent (the draft is kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    NeedPin { prompt: String },
    BadPin,
    MissingKeys(Vec<String>),
    /// Keys present but not verified: (address, fingerprint, user id).
    UntrustedKeys(Vec<(String, String, String)>),
    Other(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::NeedPin { prompt } => write!(f, "{prompt} needed to sign"),
            SendError::BadPin => write!(f, "wrong PIN or passphrase"),
            SendError::MissingKeys(k) => write!(f, "no public key for {}", k.join(", ")),
            SendError::UntrustedKeys(k) => write!(f, "keys not verified yet for {}", k.iter().map(|x| x.0.as_str()).collect::<Vec<_>>().join(", ")),
            SendError::Other(m) => write!(f, "{m}"),
        }
    }
}

pub fn spawn(
    index: usize,
    account: AccountConfig,
    cache: Option<PathBuf>,
    gpg: Option<orion_crypto::Gpg>,
    events: Sender<Event>,
) -> Sender<Request> {
    spawn_with(index, account, cache, gpg, events, Options::default())
}

pub fn spawn_with(
    index: usize,
    account: AccountConfig,
    cache: Option<PathBuf>,
    gpg: Option<orion_crypto::Gpg>,
    events: Sender<Event>,
    options: Options,
) -> Sender<Request> {
    let (tx, rx) = mpsc::channel();
    if let AccountKind::Imap(cfg) = &account.kind
        && cfg.idle
    {
        spawn_idle(index, &account, cfg.clone(), tx.clone(), events.clone());
    }
    thread::Builder::new()
        .name(format!("account-{}", account.name))
        .spawn(move || {
            let mut w = match Worker::new(index, account, cache, gpg, events.clone()) {
                Ok(w) => w,
                Err(e) => {
                    let _ = events.send(Event::Error { account: index, text: e.to_string() });
                    return;
                }
            };
            w.run(rx, options);
        })
        .expect("spawn account thread");
    tx
}

/// IMAP IDLE on a second connection: when the server announces a change in
/// the watched mailbox, asks the account thread to sync. Reconnects with
/// backoff; gives up if the server lacks IDLE or rejects the credentials.
fn spawn_idle(index: usize, account: &AccountConfig, cfg: orion_core::config::ImapConfig, worker: Sender<Request>, events: Sender<Event>) {
    use orion_backends::imap::{IdleEvent, ImapBackend};
    let mailbox = match account.watch.first() {
        Some(m) if m != "*" => m.clone(),
        _ => "INBOX".to_string(),
    };
    let name = account.name.clone();
    let _ = thread::Builder::new().name(format!("idle-{name}")).spawn(move || {
        let Ok(mut backend) = ImapBackend::new(cfg) else { return };
        let mut backoff = std::time::Duration::from_secs(5);
        loop {
            match backend.idle_wait(&mailbox, std::time::Duration::from_secs(25 * 60)) {
                Ok(IdleEvent::Changed) => {
                    backoff = std::time::Duration::from_secs(5);
                    if worker.send(Request::Refresh).is_err() {
                        return; // the account thread is gone
                    }
                }
                Ok(IdleEvent::Timeout) => backoff = std::time::Duration::from_secs(5),
                Err(Error::Auth(_)) => return, // the account thread reports it
                Err(Error::Protocol(m)) if m.to_ascii_lowercase().contains("idle") => {
                    let _ = events.send(Event::Error { account: index, text: format!("IDLE not available, polling instead: {m}") });
                    return;
                }
                Err(_) => {
                    // Network trouble: wait and retry (the regular poll keeps working meanwhile).
                    thread::sleep(backoff);
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(300));
                    if worker.send(Request::Ping).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

struct Worker {
    index: usize,
    name: String,
    backend: Box<dyn Backend>,
    store: Option<Store>,
    cache_bodies: bool,
    poll: std::time::Duration,
    events: Sender<Event>,
    current: Option<String>,
    gpg: Option<orion_crypto::Gpg>,
    watch: Vec<String>,
    account: AccountConfig,
    /// Last known mailbox list (to resolve `watch`).
    mailboxes: Vec<MailboxInfo>,
    /// Message ids seen per mailbox, to detect new mail.
    known: HashMap<String, HashSet<MessageId>>,
}

/// Resolves `watch` patterns against a mailbox list: empty = the first
/// mailbox (backends list the inbox first), `*` = all, otherwise id or
/// display name, case-insensitive.
pub fn resolve_watch(patterns: &[String], mailboxes: &[MailboxInfo]) -> Vec<String> {
    if patterns.is_empty() {
        return mailboxes.first().map(|m| vec![m.id.clone()]).unwrap_or_default();
    }
    if patterns.iter().any(|p| p == "*") {
        return mailboxes.iter().map(|m| m.id.clone()).collect();
    }
    mailboxes
        .iter()
        .filter(|m| patterns.iter().any(|p| p.eq_ignore_ascii_case(&m.id) || p.eq_ignore_ascii_case(&m.name)))
        .map(|m| m.id.clone())
        .collect()
}

impl Worker {
    fn new(
        index: usize,
        account: AccountConfig,
        cache: Option<PathBuf>,
        gpg: Option<orion_crypto::Gpg>,
        events: Sender<Event>,
    ) -> orion_core::Result<Self> {
        let backend = orion_backends::open(&account)?;
        let store = match (backend.cacheable(), cache) {
            (true, Some(path)) => match Store::open(&path) {
                Ok(s) => Some(s),
                Err(e) => {
                    let _ = events.send(Event::Error { account: index, text: format!("cache disabled: {e}") });
                    None
                }
            },
            _ => None,
        };
        let cache_bodies = match &account.kind {
            AccountKind::Imap(c) | AccountKind::Pop3(c) => c.cache_bodies,
            AccountKind::Jmap(c) => c.cache_bodies,
            AccountKind::Mbox { .. } => false,
        };
        Ok(Worker {
            index,
            name: account.name.clone(),
            poll: account.kind.poll_interval(),
            backend,
            store,
            cache_bodies,
            events,
            current: None,
            gpg,
            watch: account.watch.clone(),
            account: account.clone(),
            mailboxes: Vec::new(),
            known: HashMap::new(),
        })
    }

    fn send(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    fn progress(&self, text: impl Into<String>) {
        self.send(Event::Progress { account: self.index, text: text.into() });
    }

    fn error(&self, e: impl std::fmt::Display) {
        self.send(Event::Error { account: self.index, text: e.to_string() });
    }

    fn run(&mut self, rx: Receiver<Request>, options: Options) {
        // Whatever is cached is shown before touching the network.
        if let Some(list) = self.store.as_ref().and_then(|s| s.mailboxes(&self.name).ok()).filter(|l| !l.is_empty()) {
            self.send(Event::Mailboxes { account: self.index, list });
        }
        self.refresh_mailboxes();
        if options.watch_on_start {
            self.sync_watched();
        }

        loop {
            let first = match rx.recv_timeout(self.poll) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => {
                    self.refresh_mailboxes();
                    self.sync_watched();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // Coalesce the pending burst; of several opens only the last one counts.
            let mut batch = vec![first];
            batch.extend(rx.try_iter());
            let last_open = batch.iter().rposition(|r| matches!(r, Request::Open { .. }));
            let last_refresh = batch.iter().rposition(|r| matches!(r, Request::Refresh));
            for (i, req) in batch.into_iter().enumerate() {
                match req {
                    Request::Open { .. } if Some(i) != last_open => {}
                    Request::Refresh if Some(i) != last_refresh => {}
                    Request::Open { mailbox } => {
                        self.current = Some(mailbox.clone());
                        self.open(&mailbox);
                    }
                    Request::Refresh => {
                        self.refresh_mailboxes();
                        self.sync_watched();
                    }
                    Request::Ping => {}
                    Request::Send { draft, pin, ticket } => {
                        let result = self.send_message(&draft, pin.as_deref().map(String::as_str));
                        self.progress("");
                        self.send(Event::Sent { ticket, result });
                    }
                    Request::Fetch { mailbox, id } => self.fetch(&mailbox, id, None),
                    Request::Unlock { mailbox, id, pin } => self.fetch(&mailbox, id, pin.as_deref().map(String::as_str)),
                    Request::SetFlag { mailbox, ids, flag, on, quiet } => self.set_flag(&mailbox, ids, flag, on, quiet),
                    Request::Remove { mailbox, ids, how } => self.remove(&mailbox, ids, how),
                    Request::Empty { mailbox } => self.empty(&mailbox),
                    Request::Recount => {
                        if let Some(list) = self.store.as_ref().and_then(|s| s.mailboxes(&self.name).ok()) {
                            self.send(Event::Mailboxes { account: self.index, list });
                        }
                    }
                }
            }
        }
    }

    /// Syncs the watched mailboxes plus the open one (each only once).
    fn sync_watched(&mut self) {
        let mut targets = resolve_watch(&self.watch, &self.mailboxes);
        if let Some(cur) = &self.current
            && !targets.contains(cur)
        {
            targets.push(cur.clone());
        }
        for m in targets {
            self.sync(&m, false);
        }
    }

    /// Reports unread messages that were not there in the previous sync. The
    /// first sync of a mailbox only records the baseline.
    fn detect_new(&mut self, mailbox: &str, envelopes: &[Envelope]) {
        let ids: HashSet<MessageId> = envelopes.iter().map(|e| e.id).collect();
        if let Some(prev) = self.known.get(mailbox) {
            let new: Vec<Envelope> = envelopes.iter().filter(|e| !prev.contains(&e.id) && !e.flags.seen).cloned().collect();
            if !new.is_empty() {
                let mailbox_name = self.mailboxes.iter().find(|m| m.id == mailbox).map(|m| m.name.clone()).unwrap_or_else(|| mailbox.to_string());
                self.send(Event::NewMail { account: self.index, mailbox: mailbox.to_string(), mailbox_name, envelopes: new });
            }
        }
        self.known.insert(mailbox.to_string(), ids);
    }

    fn send_message(&mut self, draft: &orion_core::compose::Draft, pin: Option<&str>) -> Result<Option<String>, SendError> {
        use orion_backends::smtp::{self, SmtpAuth, SmtpSettings};
        use orion_crypto::outgoing::{self, OutgoingError};
        let from = orion_core::compose::addr_spec(&draft.from);
        if draft.sign || draft.encrypt {
            self.progress("signing/encrypting… (touch the YubiKey if it blinks)");
        }
        let mut built = outgoing::build(self.gpg.as_ref(), draft, pin);
        let signer = draft.signing_key.clone().unwrap_or_else(|| from.clone());
        if let (Err(OutgoingError::Gpg(m)), Some(g)) = (&built, &self.gpg)
            && m.starts_with("invalid signer")
        {
            // gpg only knows which keys live on a card after reading it once
            // (that creates the "stubs"): do it and try again, then explain.
            self.progress("reading the YubiKey / OpenPGP card…");
            let card = g.card_status().ok().flatten();
            built = outgoing::build(self.gpg.as_ref(), draft, pin);
            if let Err(OutgoingError::Gpg(m)) = &built
                && m.starts_with("invalid signer")
            {
                return Err(SendError::Other(match card {
                    Some(c) => {
                        let owner = c.fingerprints[0].as_deref().and_then(|f| g.owner_of(f));
                        format!(
                            "the connected {} holds the key of {}, not one for {signer}. Plug in the card with that key, or set `pgp_key` for this account",
                            c.name(),
                            owner.unwrap_or_else(|| "another identity (its public key is not in the keyring)".into())
                        )
                    }
                    None => format!("no secret key for {signer} on this machine: plug in the YubiKey that holds it (or set `pgp_key` for this account)"),
                }));
            }
        }
        let raw = built.map_err(|e| match e {
            OutgoingError::NeedPin => {
                let card = self.gpg.as_ref().and_then(|g| g.card_serial_for_user(&from)).map(|s| orion_crypto::describe_card(&s));
                SendError::NeedPin { prompt: card.map(|c| format!("PIN for {c}")).unwrap_or_else(|| format!("passphrase for {from}")) }
            }
            OutgoingError::BadPin => SendError::BadPin,
            OutgoingError::MissingKeys(k) => SendError::MissingKeys(k),
            OutgoingError::UntrustedKeys(k) => SendError::UntrustedKeys(k.into_iter().map(|(a, k)| (a, k.fingerprint, k.uid)).collect()),
            OutgoingError::Gpg(m) => SendError::Other(m),
        })?;
        let rcpts = orion_core::compose::recipients(draft);
        if rcpts.is_empty() {
            return Err(SendError::Other("no recipients".into()));
        }
        self.progress("sending…");
        let other = |e: Error| SendError::Other(e.to_string());
        if self.backend.submit(&raw, &from, &rcpts).map_err(other)? {
            return Ok(None); // JMAP: the server files it in Sent itself
        }
        let smtp_cfg = self
            .account
            .smtp
            .clone()
            .ok_or_else(|| SendError::Other(format!("no outgoing server for `{}`: add an [account.smtp] section", self.account.name)))?;
        let auth = |fresh: bool| -> Result<SmtpAuth, SendError> {
            Ok(match &self.account.kind {
                AccountKind::Imap(c) | AccountKind::Pop3(c) => match c.oauth {
                    Some(p) => {
                        if fresh {
                            orion_backends::oauth::invalidate(&c.account);
                        }
                        SmtpAuth::XOAuth2 { user: c.user.clone(), token: orion_backends::oauth::access_token(p, &c.account).map_err(other)? }
                    }
                    None => SmtpAuth::Password { user: c.user.clone(), password: c.resolve_password().map_err(other)? },
                },
                _ => SmtpAuth::None,
            })
        };
        let settings = |auth| SmtpSettings { host: smtp_cfg.host.clone(), port: smtp_cfg.port, tls: smtp_cfg.tls, auth };
        match smtp::send(&settings(auth(false)?), &from, &rcpts, &raw) {
            Ok(()) => {}
            // An OAuth token may be revoked before it expires: refresh once.
            Err(Error::Auth(_)) if matches!(&self.account.kind, AccountKind::Imap(c) | AccountKind::Pop3(c) if c.oauth.is_some()) => {
                smtp::send(&settings(auth(true)?), &from, &rcpts, &raw).map_err(other)?
            }
            Err(e) => return Err(other(e)),
        }
        // Sent: a failure to file the copy must not look like a failure to send.
        if self.account.saves_sent() {
            self.progress("saving a copy in Sent…");
            let copy = self.backend.sent_mailbox().and_then(|m| match m {
                Some(m) => self.backend.append(&m, &raw, true),
                None => Ok(()),
            });
            if let Err(e) = copy {
                return Ok(Some(format!("sent, but the copy could not be saved in Sent: {e}")));
            }
        }
        Ok(None)
    }

    fn refresh_mailboxes(&mut self) {
        self.progress("updating mailboxes…");
        match self.backend.mailboxes() {
            Ok(list) => {
                self.mailboxes = list.clone();
                let mut out = list;
                if let Some(store) = &mut self.store {
                    if let Err(e) = store.save_mailboxes(&self.name, &out) {
                        let _ = self.events.send(Event::Error { account: self.index, text: e.to_string() });
                    }
                    // Servers without flags (POP3): unread counts come from the cache.
                    if self.backend.local_flags() && store.recount(&self.name).is_ok()
                        && let Ok(l) = store.mailboxes(&self.name) {
                        out = l;
                    }
                }
                self.send(Event::Mailboxes { account: self.index, list: out });
            }
            Err(e) => self.error(e),
        }
        self.progress("");
    }

    fn open(&mut self, mailbox: &str) {
        if let Some(store) = &self.store {
            match store.envelopes(&self.name, mailbox) {
                Ok(envelopes) => self.send(Event::Envelopes { account: self.index, mailbox: mailbox.into(), envelopes, from_cache: true }),
                Err(e) => self.error(e),
            }
        }
        self.sync(mailbox, false);
    }

    /// Syncs a mailbox and sends the resulting list. If a delta turns out
    /// inconsistent, a full sync is attempted once.
    fn sync(&mut self, mailbox: &str, force_full: bool) {
        let known = if force_full {
            None
        } else {
            self.store.as_ref().and_then(|s| s.sync_state(&self.name, mailbox).ok().flatten())
        };
        let known_ids: HashSet<MessageId> = self
            .store
            .as_ref()
            .and_then(|s| s.envelopes(&self.name, mailbox).ok())
            .map(|envs| envs.into_iter().map(|e| e.id).collect())
            .unwrap_or_default();
        self.progress(format!("syncing {mailbox}…"));
        let (index, events, mb, name) = (self.index, self.events.clone(), mailbox.to_string(), self.name.clone());
        let store = &mut self.store;
        let mut shown: Vec<Envelope> = Vec::new();
        let mut report = move |ev: SyncEvent<'_>| match ev {
            SyncEvent::Progress { done, total } => {
                if total > 0 {
                    let _ = events.send(Event::Progress { account: index, text: format!("{mb}: {done}/{total} headers") });
                }
            }
            // Long first sync: keep and show what has arrived so far.
            SyncEvent::Batch { validity, envelopes } => {
                let partial = match store {
                    Some(st) => match st.add_partial(&name, &mb, validity, envelopes).and_then(|_| st.envelopes(&name, &mb)) {
                        Ok(all) => all,
                        Err(e) => {
                            let _ = events.send(Event::Error { account: index, text: e.to_string() });
                            return;
                        }
                    },
                    None => {
                        shown.extend_from_slice(envelopes);
                        shown.clone()
                    }
                };
                let _ = events.send(Event::Envelopes { account: index, mailbox: mb.clone(), envelopes: partial, from_cache: true });
            }
        };
        let result = self.backend.sync(mailbox, known.as_ref(), &known_ids, &mut report);
        drop(report);
        self.progress("");
        let result = match result {
            Ok(r) => r,
            Err(Error::Protocol(_)) if known.is_some() && !force_full => return self.sync(mailbox, true),
            Err(e) => {
                self.send(Event::SyncFailed { account: self.index, mailbox: mailbox.into(), error: e.to_string() });
                return self.error(e);
            }
        };
        let envelopes = match &mut self.store {
            Some(store) => {
                if let Err(e) = store.apply_sync(&self.name, mailbox, &result) {
                    return self.error(e);
                }
                if self.backend.local_flags() {
                    let _ = store.recount(&self.name);
                }
                if let Ok(list) = store.mailboxes(&self.name) {
                    let _ = self.events.send(Event::Mailboxes { account: self.index, list });
                }
                match store.envelopes(&self.name, mailbox) {
                    Ok(e) => e,
                    Err(e) => return self.error(e),
                }
            }
            None => match result {
                SyncResult::Full { envelopes, .. } => envelopes,
                SyncResult::Delta { added, .. } => added,
            },
        };
        self.detect_new(mailbox, &envelopes);
        self.send(Event::Envelopes { account: self.index, mailbox: mailbox.into(), envelopes, from_cache: false });
    }

    fn fetch(&mut self, mailbox: &str, id: MessageId, pin: Option<&str>) {
        let cached = self.store.as_ref().and_then(|s| s.body(&self.name, mailbox, id).ok().flatten());
        let raw = match cached {
            Some(raw) => Ok(raw),
            None => {
                self.progress("downloading message…");
                let r = self.backend.fetch_raw(mailbox, id);
                self.progress("");
                if let (Ok(raw), Some(store), true) = (&r, &self.store, self.cache_bodies)
                    && let Err(e) = store.save_body(&self.name, mailbox, id, raw) {
                        self.error(e);
                    }
                r
            }
        };
        let result = raw.map_err(|e| e.to_string()).and_then(|raw| {
            let encrypted = memchr_contains(&raw, b"application/pgp-encrypted") || memchr_contains(&raw, b"-----BEGIN PGP MESSAGE-----");
            if encrypted && self.gpg.is_some() {
                self.progress("decrypting… (touch the YubiKey if it blinks)");
            }
            let view = orion_crypto::open_message(self.gpg.as_ref(), &raw, pin);
            if encrypted {
                self.progress("");
            }
            view.map(Box::new).ok_or_else(|| "could not parse the message".to_string())
        });
        self.send(Event::Message { account: self.index, mailbox: mailbox.into(), id, result });
    }

    fn empty(&mut self, mailbox: &str) {
        let result = self.backend.empty_mailbox(mailbox).map_err(|e| e.to_string());
        if result.is_ok()
            && let Some(store) = &mut self.store
        {
            match store.clear_mailbox(&self.name, mailbox).and_then(|_| store.mailboxes(&self.name)) {
                Ok(list) => {
                    self.send(Event::Mailboxes { account: self.index, list });
                    crate::client::notify_daemon();
                }
                Err(e) => self.error(e),
            }
        }
        self.send(Event::Emptied { account: self.index, mailbox: mailbox.into(), result });
    }

    fn remove(&mut self, mailbox: &str, ids: Vec<MessageId>, how: Removal) {
        let result: Result<Option<String>, String> = match &how {
            Removal::Trash => match self.backend.trash_mailbox() {
                Ok(Some(trash)) if trash != mailbox => self.backend.move_messages(mailbox, &ids, &trash).map(|_| Some(trash)).map_err(|e| e.to_string()),
                Ok(Some(_)) => Err("already in the Trash: D deletes it for good".into()),
                Ok(None) => Err("this account has no Trash: D deletes it for good".into()),
                Err(e) => Err(e.to_string()),
            },
            Removal::Forever => self.backend.delete_messages(mailbox, &ids).map(|_| None).map_err(|e| e.to_string()),
            Removal::MoveTo(dest) => self.backend.move_messages(mailbox, &ids, dest).map(|_| Some(dest.clone())).map_err(|e| e.to_string()),
        };
        if result.is_ok()
            && let Some(store) = &mut self.store
        {
            match store.remove(&self.name, mailbox, &ids).and_then(|_| store.mailboxes(&self.name)) {
                Ok(list) => {
                    self.send(Event::Mailboxes { account: self.index, list });
                    crate::client::notify_daemon();
                }
                Err(e) => self.error(e),
            }
        }
        self.send(Event::Removed { account: self.index, mailbox: mailbox.into(), result });
    }

    fn set_flag(&mut self, mailbox: &str, ids: Vec<MessageId>, flag: Flag, on: bool, quiet: bool) {
        match self.backend.set_flag(mailbox, &ids, flag, on) {
            Ok(()) => {
                if let Some(store) = &mut self.store {
                    let r = store.set_flag(&self.name, mailbox, &ids, flag, on).and_then(|_| store.mailboxes(&self.name));
                    match r {
                        Ok(list) => {
                            self.send(Event::Mailboxes { account: self.index, list });
                            // The daemon (status bar) reads the same cache: tell it.
                            crate::client::notify_daemon();
                        }
                        Err(e) => self.error(e),
                    }
                }
            }
            // Read-only backend: the change stays in memory.
            Err(Error::Unsupported(msg)) => {
                if !quiet {
                    self.error(format!("{msg}: the change only lasts this session"));
                }
            }
            Err(e) => {
                self.error(e);
                self.send(Event::FlagFailed { account: self.index, mailbox: mailbox.into(), ids, flag, on });
            }
        }
    }
}

fn memchr_contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}
