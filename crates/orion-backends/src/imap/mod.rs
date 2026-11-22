//! IMAP4rev1 backend (async-imap on tokio, run in the account thread).
//!
//! Incremental sync based on UIDVALIDITY/UIDNEXT:
//! - Different UIDVALIDITY (or first time) → full download in batches.
//! - Same → only the new UIDs and the flags of the already known ones.

mod stream;
pub mod utf7;

use std::collections::HashSet;
use std::time::Duration;

use async_imap::types::{Fetch, NameAttribute};
use async_imap::{Client, Session};
use futures::TryStreamExt;
use tokio::net::TcpStream;
use tokio::runtime::Runtime;

use orion_core::config::{ImapConfig, TlsMode};
use orion_core::model::{Envelope, Flag, Flags, MailboxInfo, MessageId, SyncEvent, SyncResult, SyncState};
use orion_core::parse::envelope_from_headers;
use orion_core::{Backend, Error, Result};

use stream::{tls_wrap, ImapStream};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const OP_TIMEOUT: Duration = Duration::from_secs(180);
const BATCH: u32 = 500;
const HEADER_QUERY: &str = "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE CONTENT-TYPE MESSAGE-ID IN-REPLY-TO REFERENCES)])";

type ImapSession = Session<ImapStream>;

pub struct ImapBackend {
    cfg: ImapConfig,
    rt: Runtime,
    session: Option<ImapSession>,
    /// Mailbox selected in the current session.
    selected: Option<String>,
    password: Option<String>,
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

fn imap_err(e: async_imap::error::Error) -> Error {
    use async_imap::error::Error as E;
    match e {
        E::Io(e) => Error::Network(e.to_string()),
        E::ConnectionLost => Error::Network("connection lost".into()),
        E::No(m) | E::Bad(m) => Error::Protocol(m),
        other => Error::Protocol(other.to_string()),
    }
}

/// Runs an operation on the session; if the connection dropped, reconnects,
/// re-selects the mailbox (if given) and retries once.
macro_rules! with_session {
    ($self:ident, $mailbox:expr, |$s:ident| $body:expr) => {{
        let mailbox: Option<&str> = $mailbox;
        let mut attempt = 0;
        loop {
            $self.ensure_connected()?;
            let needs_select = mailbox.is_some_and(|m| $self.selected.as_deref() != Some(m));
            let $s = $self.session.as_mut().expect("session");
            let r: Result<_> = $self.rt.block_on(async {
                tokio::time::timeout(OP_TIMEOUT, async {
                    if needs_select {
                        $s.select(mailbox.unwrap_or_default()).await.map_err(imap_err)?;
                    }
                    $body.await
                })
                .await
                .unwrap_or_else(|_| Err(net("timed out")))
            });
            match r {
                Err(Error::Network(_)) if attempt == 0 => {
                    $self.session = None;
                    $self.selected = None;
                    attempt += 1;
                }
                Err(e) => break Err(e),
                Ok(v) => {
                    if needs_select {
                        $self.selected = mailbox.map(str::to_string);
                    }
                    break Ok(v);
                }
            }
        }
    }};
}

/// One FETCH response, decoded.
#[derive(Debug, Default)]
struct Fetched {
    uid: Option<u32>,
    flags: Flags,
    size: Option<u32>,
    header: Option<Vec<u8>>,
    body: Option<Vec<u8>>,
}

fn decode_fetch(attrs: &[async_imap::imap_proto::AttributeValue<'_>]) -> Fetched {
    use async_imap::imap_proto::{AttributeValue as A, MessageSection, SectionPath};
    let mut f = Fetched::default();
    for a in attrs {
        match a {
            A::Uid(u) => f.uid = Some(*u),
            A::Rfc822Size(n) => f.size = Some(*n),
            A::Flags(list) => {
                for fl in list {
                    match fl.to_ascii_lowercase().as_str() {
                        "\\seen" => f.flags.seen = true,
                        "\\answered" => f.flags.answered = true,
                        "\\flagged" => f.flags.flagged = true,
                        "\\deleted" => f.flags.deleted = true,
                        "\\draft" => f.flags.draft = true,
                        _ => {}
                    }
                }
            }
            A::BodySection { section: Some(SectionPath::Full(MessageSection::Header)), data: Some(d), .. } | A::Rfc822Header(Some(d)) => {
                f.header = Some(d.to_vec())
            }
            A::BodySection { section: None, data: Some(d), .. } | A::Rfc822(Some(d)) => f.body = Some(d.to_vec()),
            _ => {}
        }
    }
    f
}

/// Servers that rate-limit (Outlook: "Request is throttled. Suggested Backoff
/// Time: N milliseconds") answer NO; how long to wait before retrying.
fn throttle_backoff(info: &str) -> Option<Duration> {
    let l = info.to_ascii_lowercase();
    if !(l.contains("throttl") || l.contains("too many") || l.contains("try again later") || l.contains("[limit]")) {
        return None;
    }
    let ms = l
        .split("millisecond")
        .next()
        .and_then(|before| before.split(|c: char| !c.is_ascii_digit()).rfind(|n| !n.is_empty()))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(2000);
    Some(Duration::from_millis(ms.clamp(500, 20_000)))
}

/// FETCH/UID FETCH that reads every response up to its own tagged reply, so a
/// NO/BAD or a closed connection is an error instead of a silently short
/// answer (async-imap's fetch streams swallow both). Throttling is waited out.
async fn fetch_cmd(s: &mut ImapSession, set: &str, query: &str, uid: bool) -> Result<Vec<Fetched>> {
    use async_imap::imap_proto::{Response, Status};
    let cmd = format!("{}FETCH {set} {query}", if uid { "UID " } else { "" });
    let mut attempts = 0;
    loop {
        let id = s.run_command(&cmd).await.map_err(imap_err)?;
        let mut out = Vec::new();
        let (ok, info) = loop {
            let Some(resp) = s.read_response().await.map_err(net)? else {
                return Err(net("connection closed in the middle of a FETCH"));
            };
            match resp.parsed() {
                Response::Fetch(_, attrs) => out.push(decode_fetch(attrs)),
                Response::Done { tag, status, outcome } if *tag == id => {
                    break (matches!(status, Status::Ok), outcome.information.as_deref().unwrap_or("").to_string());
                }
                Response::Data { status: Status::Bye, .. } => return Err(net("the server closed the session")),
                _ => {}
            }
        };
        if ok {
            return Ok(out);
        }
        match throttle_backoff(&info) {
            Some(wait) if attempts < 5 => {
                attempts += 1;
                tokio::time::sleep(wait).await;
            }
            _ => return Err(Error::Protocol(format!("FETCH refused by the server: {info}"))),
        }
    }
}

impl ImapBackend {
    pub fn new(cfg: ImapConfig) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(net)?;
        Ok(ImapBackend { cfg, rt, session: None, selected: None, password: None })
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        let cfg = self.cfg.clone();
        let attempt = |this: &mut Self, cred: Credential| {
            this.rt.block_on(async {
                tokio::time::timeout(CONNECT_TIMEOUT, connect(&cfg, &cred))
                    .await
                    .unwrap_or_else(|_| Err(net(format!("timed out connecting to {}", cfg.host))))
            })
        };
        let session = if let Some(provider) = cfg.oauth {
            // OAuth2: a cached access token may have been revoked early; on
            // rejection, force a refresh and try once more.
            let token = crate::oauth::access_token(provider, &cfg.account)?;
            match attempt(self, Credential::Bearer(token)) {
                Err(Error::Auth(_)) => {
                    crate::oauth::invalidate(&cfg.account);
                    let token = crate::oauth::access_token(provider, &cfg.account)?;
                    attempt(self, Credential::Bearer(token))
                }
                other => other,
            }
        } else {
            if self.password.is_none() {
                self.password = Some(self.cfg.resolve_password()?);
            }
            let pass = self.password.clone().unwrap_or_default();
            attempt(self, Credential::Password(pass))
        };
        match session {
            Ok(s) => {
                self.session = Some(s);
                self.selected = None;
                Ok(())
            }
            Err(e @ Error::Auth(_)) => {
                // Wrong password: the command will be asked again next time.
                self.password = None;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// Selects a mailbox (read/write) if it was not already selected.
    /// Returns (UIDVALIDITY, UIDNEXT, EXISTS).
    fn select(&mut self, mailbox: &str) -> Result<(u64, Option<u64>, u32)> {
        let name = mailbox.to_string();
        let mb = with_session!(self, None, |s| async { s.select(&name).await.map_err(imap_err) })?;
        self.selected = Some(mailbox.to_string());
        Ok((mb.uid_validity.unwrap_or(0) as u64, mb.uid_next.map(|u| u as u64), mb.exists))
    }

    fn fetch_envelopes(&mut self, mailbox: &str, set: String, uid: bool) -> Result<Vec<Envelope>> {
        with_session!(self, Some(mailbox), |s| async {
            let fetches = fetch_cmd(s, &set, HEADER_QUERY, uid).await?;
            Ok(fetches.iter().filter_map(to_envelope).collect::<Vec<_>>())
        })
    }
}

enum Credential {
    Password(String),
    /// OAuth2 access token, sent with SASL XOAUTH2.
    Bearer(String),
}

/// SASL XOAUTH2: one message with the user and token; if the server answers
/// with an error challenge (JSON), an empty response ends the exchange.
struct XOAuth2 {
    user: String,
    token: String,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> String {
        if self.sent {
            return String::new();
        }
        self.sent = true;
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

async fn connect(cfg: &ImapConfig, cred: &Credential) -> Result<ImapSession> {
    let addr = (cfg.host.as_str(), cfg.port());
    let tcp = TcpStream::connect(addr).await.map_err(|e| net(format!("{}:{}: {e}", cfg.host, cfg.port())))?;
    tcp.set_nodelay(true).ok();
    let client = match cfg.tls {
        TlsMode::Tls => {
            let mut c = Client::new(tls_wrap(tcp, &cfg.host).await?);
            c.read_response().await.map_err(net)?;
            c
        }
        TlsMode::None => {
            let mut c = Client::new(ImapStream::Plain(tcp));
            c.read_response().await.map_err(net)?;
            c
        }
        TlsMode::Starttls => {
            let mut plain = Client::new(tcp);
            plain.read_response().await.map_err(net)?;
            plain.run_command_and_check_ok("STARTTLS", None).await.map_err(imap_err)?;
            Client::new(tls_wrap(plain.into_inner(), &cfg.host).await?)
        }
    };
    let auth_err = |e: async_imap::error::Error| match e {
        async_imap::error::Error::No(m) | async_imap::error::Error::Bad(m) => Error::Auth(m),
        other => imap_err(other),
    };
    match cred {
        Credential::Password(p) => client.login(&cfg.user, p).await.map_err(|(e, _)| auth_err(e)),
        Credential::Bearer(token) => client
            .authenticate("XOAUTH2", XOAuth2 { user: cfg.user.clone(), token: token.clone(), sent: false })
            .await
            .map_err(|(e, _)| auth_err(e)),
    }
}

fn to_envelope(f: &Fetched) -> Option<Envelope> {
    let uid = f.uid? as u64;
    let header = f.header.as_deref().unwrap_or_default();
    let mut env = envelope_from_headers(MessageId(uid), header, f.size.unwrap_or(0) as usize);
    env.flags = f.flags;
    Some(env)
}

fn flag_name(f: Flag) -> &'static str {
    match f {
        Flag::Seen => "\\Seen",
        Flag::Answered => "\\Answered",
        Flag::Flagged => "\\Flagged",
        Flag::Deleted => "\\Deleted",
        Flag::Draft => "\\Draft",
    }
}

/// Compacts UIDs into an IMAP set: 1,2,3,7 → "1:3,7".
pub fn uid_set(ids: &[MessageId]) -> String {
    let mut v: Vec<u64> = ids.iter().map(|i| i.0).collect();
    v.sort_unstable();
    v.dedup();
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        while i + 1 < v.len() && v[i + 1] == v[i] + 1 {
            i += 1;
        }
        out.push(if v[i] == start { start.to_string() } else { format!("{start}:{}", v[i]) });
        i += 1;
    }
    out.join(",")
}

/// Inbox, then special-use folders, then the rest.
/// Removes the messages marked `\Deleted`: only `set` with UIDPLUS.
async fn expunge(s: &mut ImapSession, set: &str, uidplus: bool) -> Result<()> {
    if uidplus {
        let _: Vec<_> = s.uid_expunge(set).await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
    } else {
        let _: Vec<_> = s.expunge().await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
    }
    Ok(())
}

impl ImapBackend {
    /// A special-use mailbox (RFC 6154), else one with a usual name.
    fn special_mailbox(&mut self, is: impl Fn(&NameAttribute<'_>) -> bool + Send + Sync, usual: &[&str]) -> Result<Option<String>> {
        let names: Vec<(String, bool)> = with_session!(self, None, |s| async {
            let list: Vec<_> = s.list(None, Some("*")).await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
            Ok(list
                .iter()
                .filter(|n| !n.attributes().iter().any(|a| matches!(a, NameAttribute::NoSelect)))
                .map(|n| (n.name().to_string(), n.attributes().iter().any(&is)))
                .collect::<Vec<_>>())
        })?;
        if let Some((name, _)) = names.iter().find(|(_, special)| *special) {
            return Ok(Some(name.clone()));
        }
        Ok(usual
            .iter()
            .find_map(|u| names.iter().find(|(n, _)| n.eq_ignore_ascii_case(u) || utf7::decode(n).eq_ignore_ascii_case(u)))
            .map(|(n, _)| n.clone()))
    }
}

fn mailbox_rank(id: &str, attrs: &[NameAttribute<'_>]) -> u8 {
    if id.eq_ignore_ascii_case("INBOX") {
        return 0;
    }
    for a in attrs {
        match a {
            NameAttribute::Drafts => return 1,
            NameAttribute::Sent => return 2,
            NameAttribute::Archive => return 3,
            NameAttribute::Junk => return 4,
            NameAttribute::Trash => return 5,
            _ => {}
        }
    }
    9
}

/// Result of waiting in IMAP IDLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEvent {
    /// The server reported a change (new message, expunge, flags...).
    Changed,
    /// Nothing happened within the timeout.
    Timeout,
}

impl ImapBackend {
    /// Waits in IDLE (RFC 2177) on `mailbox` until the server reports a change
    /// or `timeout` passes. Use on a dedicated connection: it blocks.
    pub fn idle_wait(&mut self, mailbox: &str, timeout: Duration) -> Result<IdleEvent> {
        if self.selected.as_deref() != Some(mailbox) {
            self.select(mailbox)?;
        }
        let session = self.session.take().expect("connected after select");
        self.selected = None;
        let result = self.rt.block_on(async move {
            let mut handle = session.idle();
            handle.init().await.map_err(imap_err)?;
            let response = {
                let (wait, stop) = handle.wait_with_timeout(timeout);
                let r = wait.await.map_err(imap_err)?;
                drop(stop);
                r
            };
            let session = tokio::time::timeout(Duration::from_secs(30), handle.done())
                .await
                .map_err(|_| net("timed out leaving IDLE"))?
                .map_err(imap_err)?;
            Ok::<_, Error>((session, response))
        });
        match result {
            Ok((session, response)) => {
                self.session = Some(session);
                self.selected = Some(mailbox.to_string());
                match response {
                    async_imap::extensions::idle::IdleResponse::NewData(_) => Ok(IdleEvent::Changed),
                    async_imap::extensions::idle::IdleResponse::Timeout => Ok(IdleEvent::Timeout),
                    // We never interrupt it ourselves: this means the stream ended.
                    async_imap::extensions::idle::IdleResponse::ManualInterrupt => Err(net("connection closed during IDLE")),
                }
            }
            Err(e) => {
                self.session = None;
                Err(e)
            }
        }
    }
}

impl Backend for ImapBackend {
    fn mailboxes(&mut self) -> Result<Vec<MailboxInfo>> {
        let names: Vec<(String, String, u8)> = with_session!(self, None, |s| async {
            let list: Vec<_> = s.list(None, Some("*")).await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
            Ok(list
                .iter()
                .filter(|n| !n.attributes().iter().any(|a| matches!(a, NameAttribute::NoSelect)))
                .map(|n| {
                    let id = n.name().to_string();
                    let mut name = utf7::decode(&id);
                    if let Some(d) = n.delimiter().filter(|d| *d != "/") {
                        name = name.replace(d, "/");
                    }
                    let rank = mailbox_rank(&id, n.attributes());
                    (id, name, rank)
                })
                .collect::<Vec<_>>())
        })?;
        let mut out = Vec::with_capacity(names.len());
        for (id, name, rank) in names {
            let quoted = id.clone();
            let st = with_session!(self, None, |s| async { s.status(&quoted, "(MESSAGES UNSEEN)").await.map_err(imap_err) });
            let (total, unread) = match st {
                Ok(m) => (m.exists as usize, m.unseen.unwrap_or(0) as usize),
                Err(Error::Protocol(_)) => (0, 0),
                Err(e) => return Err(e),
            };
            out.push((rank, MailboxInfo { id, name, total, unread }));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
        Ok(out.into_iter().map(|(_, m)| m).collect())
    }

    fn envelopes(&mut self, mailbox: &str) -> Result<Vec<Envelope>> {
        match self.sync(mailbox, None, &HashSet::new(), &mut |_| {})? {
            SyncResult::Full { envelopes, .. } => Ok(envelopes),
            SyncResult::Delta { added, .. } => Ok(added),
        }
    }

    fn sync(
        &mut self,
        mailbox: &str,
        known: Option<&SyncState>,
        known_ids: &HashSet<MessageId>,
        progress: &mut dyn FnMut(SyncEvent<'_>),
    ) -> Result<SyncResult> {
        let (validity, uid_next, exists) = self.select(mailbox)?;
        let same = |k: &&SyncState| k.validity == validity && validity != 0;

        if let Some(k) = known.filter(same).filter(|k| k.next > 0) {
            // Incremental: new messages + flags of the known ones.
            let mut added = Vec::new();
            if uid_next.is_none_or(|n| n > k.next) {
                added = self.fetch_envelopes(mailbox, format!("{}:*", k.next.max(1)), true)?;
                // "n:*" always returns the last message even if it is below n.
                added.retain(|e| e.id.0 >= k.next);
            }
            let mut existing = Vec::new();
            if k.next > 1 && exists > 0 {
                let set = format!("1:{}", k.next - 1);
                existing = with_session!(self, Some(mailbox), |s| async {
                    let f = fetch_cmd(s, &set, "(UID FLAGS)", true).await?;
                    Ok(f.iter().filter_map(|f| Some((MessageId(f.uid? as u64), Some(f.flags)))).collect::<Vec<_>>())
                })?;
                existing.retain(|(id, _)| id.0 < k.next);
            }
            // Consistency check: what we received must match EXISTS. If not
            // (truncated response, concurrent changes), nothing is applied.
            if added.len() + existing.len() != exists as usize {
                return Err(Error::Protocol(format!(
                    "inconsistent sync of {mailbox}: {} + {} ≠ {exists}; will retry",
                    added.len(),
                    existing.len()
                )));
            }
            let max_uid = added.iter().map(|e| e.id.0 + 1).max().unwrap_or(k.next);
            let next = uid_next.unwrap_or(max_uid).max(max_uid);
            return Ok(SyncResult::Delta { added, existing, state: SyncState { validity, next } });
        }

        if known.filter(same).is_some() {
            // Resuming an interrupted first sync (state "next = 0"): list every
            // UID with its flags (cheap) and fetch headers only for the missing ones.
            let all: Vec<(u32, Flags)> = if exists == 0 {
                Vec::new()
            } else {
                with_session!(self, Some(mailbox), |s| async {
                    let f = fetch_cmd(s, "1:*", "(UID FLAGS)", true).await?;
                    Ok(f.iter().filter_map(|f| Some((f.uid?, f.flags))).collect::<Vec<_>>())
                })?
            };
            let missing: Vec<MessageId> = all.iter().map(|(u, _)| MessageId(*u as u64)).filter(|id| !known_ids.contains(id)).collect();
            let existing: Vec<(MessageId, Option<Flags>)> =
                all.iter().filter(|(u, _)| known_ids.contains(&MessageId(*u as u64))).map(|(u, f)| (MessageId(*u as u64), Some(*f))).collect();
            let total = missing.len();
            progress(SyncEvent::Progress { done: 0, total });
            let mut added = Vec::with_capacity(total);
            for chunk in missing.chunks(BATCH as usize) {
                let batch = self.fetch_envelopes(mailbox, uid_set(chunk), true)?;
                progress(SyncEvent::Batch { validity, envelopes: &batch });
                added.extend(batch);
                progress(SyncEvent::Progress { done: added.len(), total });
            }
            if added.len() + existing.len() != all.len() {
                return Err(Error::Protocol(format!("incomplete download of {mailbox}: {} of {}; will retry", added.len() + existing.len(), all.len())));
            }
            let max_uid = all.iter().map(|(u, _)| *u as u64 + 1).max().unwrap_or(1);
            let next = uid_next.unwrap_or(max_uid).max(max_uid);
            return Ok(SyncResult::Delta { added, existing, state: SyncState { validity, next } });
        }

        // Full sync, in batches of sequence numbers. Each batch is handed out
        // as soon as it arrives, so it can be stored and shown right away and an
        // interrupted sync resumes instead of starting over.
        let total = exists as usize;
        let mut envelopes = Vec::with_capacity(total);
        let mut start = 1u32;
        progress(SyncEvent::Progress { done: 0, total });
        while start <= exists {
            let end = (start + BATCH - 1).min(exists);
            let batch = self.fetch_envelopes(mailbox, format!("{start}:{end}"), false)?;
            progress(SyncEvent::Batch { validity, envelopes: &batch });
            envelopes.extend(batch);
            progress(SyncEvent::Progress { done: envelopes.len(), total });
            start = end + 1;
        }
        if envelopes.len() != total {
            return Err(Error::Protocol(format!(
                "incomplete download of {mailbox}: {} of {total}; will retry",
                envelopes.len()
            )));
        }
        let max_uid = envelopes.iter().map(|e| e.id.0 + 1).max().unwrap_or(1);
        let state = (validity != 0).then(|| SyncState { validity, next: uid_next.unwrap_or(max_uid).max(max_uid) });
        Ok(SyncResult::Full { envelopes, state })
    }

    fn fetch_raw(&mut self, mailbox: &str, id: MessageId) -> Result<Vec<u8>> {
        let uid = id.0.to_string();
        let raw = with_session!(self, Some(mailbox), |s| async {
            let f = fetch_cmd(s, &uid, "(UID BODY.PEEK[])", true).await?;
            Ok(f.into_iter().find(|f| f.uid == Some(id.0 as u32)).and_then(|f| f.body))
        })?;
        raw.ok_or(Error::MessageNotFound(id.0))
    }

    fn set_flag(&mut self, mailbox: &str, ids: &[MessageId], flag: Flag, on: bool) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let set = uid_set(ids);
        let query = format!("{}FLAGS.SILENT ({})", if on { '+' } else { '-' }, flag_name(flag));
        with_session!(self, Some(mailbox), |s| async {
            let _: Vec<Fetch> = s.uid_store(&set, &query).await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
            s.noop().await.map_err(imap_err)?;
            Ok(())
        })
    }

    fn find_sent_mailbox(&mut self) -> Result<Option<String>> {
        const USUAL: [&str; 8] = ["Sent", "Sent Items", "Sent Messages", "Sent Mail", "INBOX.Sent", "INBOX/Sent", "Enviados", "Elementos enviados"];
        self.special_mailbox(|a| matches!(a, NameAttribute::Sent), &USUAL)
    }

    fn sent_mailbox(&mut self) -> Result<Option<String>> {
        if let Some(name) = self.find_sent_mailbox()? {
            return Ok(Some(name));
        }
        with_session!(self, None, |s| async {
            s.create("Sent").await.map_err(imap_err)?;
            let _ = s.subscribe("Sent").await;
            Ok(())
        })?;
        Ok(Some("Sent".into()))
    }

    fn trash_mailbox(&mut self) -> Result<Option<String>> {
        const USUAL: [&str; 10] = [
            "Trash",
            "Deleted Items",
            "Deleted Messages",
            "Deleted",
            "Bin",
            "INBOX.Trash",
            "INBOX/Trash",
            "Papelera",
            "Elementos eliminados",
            "[Gmail]/Trash",
        ];
        self.special_mailbox(|a| matches!(a, NameAttribute::Trash), &USUAL)
    }

    /// `UID MOVE` (RFC 6851) when the server has it; else copy, mark
    /// `\Deleted` and expunge only those (`UID EXPUNGE`, RFC 4315) or, as a
    /// last resort, everything marked deleted.
    fn move_messages(&mut self, mailbox: &str, ids: &[MessageId], dest: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let (set, dest) = (uid_set(ids), dest.to_string());
        with_session!(self, Some(mailbox), |s| async {
            let caps = s.capabilities().await.map_err(imap_err)?;
            if caps.has_str("MOVE") {
                s.uid_mv(&set, &dest).await.map_err(imap_err)?;
            } else {
                s.uid_copy(&set, &dest).await.map_err(imap_err)?;
                let _: Vec<Fetch> = s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
                expunge(s, &set, caps.has_str("UIDPLUS")).await?;
            }
            Ok(())
        })
    }

    fn delete_messages(&mut self, mailbox: &str, ids: &[MessageId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let set = uid_set(ids);
        with_session!(self, Some(mailbox), |s| async {
            let caps = s.capabilities().await.map_err(imap_err)?;
            let _: Vec<Fetch> = s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
            expunge(s, &set, caps.has_str("UIDPLUS")).await
        })
    }

    /// Everything marked `\\Deleted`, then expunged: the folder is meant to end empty.
    fn empty_mailbox(&mut self, mailbox: &str) -> Result<()> {
        with_session!(self, Some(mailbox), |s| async {
            let _: Vec<Fetch> = s.uid_store("1:*", "+FLAGS.SILENT (\\Deleted)").await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
            let _: Vec<_> = s.expunge().await.map_err(imap_err)?.try_collect().await.map_err(imap_err)?;
            Ok(())
        })
    }

    fn append(&mut self, mailbox: &str, raw: &[u8], seen: bool) -> Result<()> {
        let (mb, data) = (mailbox.to_string(), raw.to_vec());
        with_session!(self, None, |s| async { s.append(&mb, seen.then_some("(\\Seen)"), None, &data).await.map_err(imap_err) })
    }

    fn cacheable(&self) -> bool {
        true
    }
}

impl Drop for ImapBackend {
    fn drop(&mut self) {
        if let Some(mut s) = self.session.take() {
            let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), s.logout()).await });
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_sets() {
        let ids: Vec<MessageId> = [7, 1, 2, 3, 9, 10, 3].into_iter().map(MessageId).collect();
        assert_eq!(uid_set(&ids), "1:3,7,9:10");
    }
}
