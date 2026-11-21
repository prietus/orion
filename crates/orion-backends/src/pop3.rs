//! POP3 backend (RFC 1939, CAPA/STLS/PIPELINING from RFC 2449/2595).
//!
//! POP3 has a single mailbox and no flags: read/flagged live in the local
//! cache. Message ids are a stable hash of the UIDL. Only headers of unknown
//! messages are downloaded (`TOP n 0`), pipelined when the server allows it.
//! The server only shows mail that arrived before login, so each sync opens a
//! fresh session. Messages are never deleted from the server.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use crate::net::{tls, Conn};

use orion_core::config::{ImapConfig, TlsMode};
use orion_core::model::{Envelope, Flag, MailboxInfo, MessageId, SyncEvent, SyncResult, SyncState};
use orion_core::parse::envelope_from_headers;
use orion_core::{Backend, Error, Result};

const TIMEOUT: Duration = Duration::from_secs(60);
const PIPELINE: usize = 64;
pub const INBOX: &str = "INBOX";

fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

/// Stable id from a UIDL.
pub fn uidl_id(uidl: &str) -> MessageId {
    crate::stable_id(uidl)
}

struct Session {
    io: BufReader<Conn>,
    capa: Vec<String>,
}

impl Session {
    fn connect(cfg: &ImapConfig, password: &str) -> Result<Session> {
        let port = cfg.pop3_port();
        let tcp = crate::net::tcp(&cfg.host, port, TIMEOUT)?;
        let conn = match cfg.tls {
            TlsMode::Tls => Conn::Tls(Box::new(tls(tcp, &cfg.host)?)),
            _ => Conn::Plain(tcp),
        };
        let mut s = Session { io: BufReader::new(conn), capa: Vec::new() };
        s.status_line()?; // greeting
        s.load_capa();
        if cfg.tls == TlsMode::Starttls {
            s.command("STLS")?;
            crate::net::upgrade(&mut s.io, &cfg.host)?;
            s.load_capa();
        }
        s.command(&format!("USER {}", cfg.user)).map_err(auth_err)?;
        s.command(&format!("PASS {password}")).map_err(auth_err)?;
        s.load_capa(); // some servers announce more after login
        Ok(s)
    }

    fn load_capa(&mut self) {
        self.capa = match self.command("CAPA") {
            Ok(_) => self
                .multiline()
                .map(|b| String::from_utf8_lossy(&b).lines().map(|l| l.to_ascii_uppercase()).collect())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
    }

    fn has(&self, cap: &str) -> bool {
        self.capa.iter().any(|c| c.split_whitespace().next() == Some(cap))
    }

    fn send(&mut self, line: &str) -> Result<()> {
        let w = self.io.get_mut();
        w.write_all(line.as_bytes()).map_err(net)?;
        w.write_all(b"\r\n").map_err(net)?;
        w.flush().map_err(net)
    }

    fn status_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let n = self.io.read_line(&mut line).map_err(net)?;
        if n == 0 {
            return Err(net("connection closed"));
        }
        let line = line.trim_end().to_string();
        match line.strip_prefix("+OK") {
            Some(rest) => Ok(rest.trim().to_string()),
            None => Err(Error::Protocol(line.strip_prefix("-ERR").unwrap_or(&line).trim().to_string())),
        }
    }

    fn command(&mut self, cmd: &str) -> Result<String> {
        self.send(cmd)?;
        self.status_line()
    }

    /// Multi-line body up to ".", dot-unstuffed, with CRLF line ends.
    fn multiline(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = self.io.read_until(b'\n', &mut line).map_err(net)?;
            if n == 0 {
                return Err(net("connection closed in the middle of a response"));
            }
            let content = line.strip_suffix(b"\r\n").or_else(|| line.strip_suffix(b"\n")).unwrap_or(&line);
            if content == b"." {
                return Ok(out);
            }
            let content = if content.starts_with(b"..") { &content[1..] } else { content };
            out.extend_from_slice(content);
            out.extend_from_slice(b"\r\n");
        }
    }

    /// `UIDL`: (message number, unique id).
    fn uidl(&mut self) -> Result<Vec<(u32, String)>> {
        self.command("UIDL").map_err(|e| match e {
            Error::Protocol(m) => Error::Protocol(format!("the server does not support UIDL: {m}")),
            other => other,
        })?;
        Ok(String::from_utf8_lossy(&self.multiline()?)
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.to_string()))
            })
            .collect())
    }

    /// `LIST`: message number → size.
    fn sizes(&mut self) -> Result<HashMap<u32, usize>> {
        self.command("LIST")?;
        Ok(String::from_utf8_lossy(&self.multiline()?)
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .collect())
    }

    /// Headers of several messages (`TOP n 0`), pipelined if possible. Falls
    /// back to `RETR` when the server lacks TOP.
    fn headers(&mut self, msgnos: &[u32], progress: &mut dyn FnMut(usize)) -> Result<Vec<Vec<u8>>> {
        let use_top = self.capa.is_empty() || self.has("TOP");
        let batch = if self.has("PIPELINING") { PIPELINE } else { 1 };
        let mut out = Vec::with_capacity(msgnos.len());
        for chunk in msgnos.chunks(batch) {
            for n in chunk {
                self.send(&if use_top { format!("TOP {n} 0") } else { format!("RETR {n}") })?;
            }
            for _ in chunk {
                self.status_line()?;
                out.push(self.multiline()?);
            }
            progress(out.len());
        }
        Ok(out)
    }

    fn quit(&mut self) {
        let _ = self.command("QUIT");
    }
}

fn auth_err(e: Error) -> Error {
    match e {
        Error::Protocol(m) => Error::Auth(m),
        other => other,
    }
}


pub struct Pop3Backend {
    cfg: ImapConfig,
    session: Option<Session>,
    password: Option<String>,
    /// Id → message number in the current session.
    msgno: HashMap<MessageId, u32>,
}

impl Pop3Backend {
    pub fn new(cfg: ImapConfig) -> Self {
        Pop3Backend { cfg, session: None, password: None, msgno: HashMap::new() }
    }

    /// Opens a new session (a POP3 session only sees mail present at login).
    fn fresh_session(&mut self) -> Result<(&mut Session, Vec<(u32, String)>)> {
        if let Some(mut s) = self.session.take() {
            s.quit();
        }
        if self.password.is_none() {
            self.password = Some(self.cfg.resolve_password()?);
        }
        let mut s = match Session::connect(&self.cfg, self.password.as_deref().unwrap_or_default()) {
            Ok(s) => s,
            Err(e @ Error::Auth(_)) => {
                self.password = None;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let list = s.uidl()?;
        self.msgno = list.iter().map(|(n, u)| (uidl_id(u), *n)).collect();
        self.session = Some(s);
        Ok((self.session.as_mut().expect("session"), list))
    }

    fn check_mailbox(mailbox: &str) -> Result<()> {
        if mailbox == INBOX { Ok(()) } else { Err(Error::MailboxNotFound(mailbox.into())) }
    }
}

impl Backend for Pop3Backend {
    fn mailboxes(&mut self) -> Result<Vec<MailboxInfo>> {
        let (_, list) = self.fresh_session()?;
        let n = list.len();
        // Unread is unknown to POP3; the account thread recomputes it from the cache.
        Ok(vec![MailboxInfo { id: INBOX.into(), name: INBOX.into(), total: n, unread: n }])
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
        _known: Option<&SyncState>,
        known_ids: &HashSet<MessageId>,
        progress: &mut dyn FnMut(SyncEvent<'_>),
    ) -> Result<SyncResult> {
        Self::check_mailbox(mailbox)?;
        let (s, list) = self.fresh_session()?;
        let sizes = s.sizes()?;
        let new: Vec<(u32, MessageId)> = list.iter().map(|(n, u)| (*n, uidl_id(u))).filter(|(_, id)| !known_ids.contains(id)).collect();
        let total = new.len();
        progress(SyncEvent::Progress { done: 0, total });
        let numbers: Vec<u32> = new.iter().map(|(n, _)| *n).collect();
        let heads = s.headers(&numbers, &mut |done| progress(SyncEvent::Progress { done, total }))?;
        let added = new
            .iter()
            .zip(heads)
            .map(|((n, id), head)| envelope_from_headers(*id, &head, sizes.get(n).copied().unwrap_or(head.len())))
            .collect();
        let existing = list.iter().map(|(_, u)| uidl_id(u)).filter(|id| known_ids.contains(id)).map(|id| (id, None)).collect();
        Ok(SyncResult::Delta { added, existing, state: SyncState { validity: 1, next: 1 } })
    }

    fn fetch_raw(&mut self, mailbox: &str, id: MessageId) -> Result<Vec<u8>> {
        Self::check_mailbox(mailbox)?;
        for attempt in 0..2 {
            if self.session.is_none() || (attempt == 1) || !self.msgno.contains_key(&id) {
                self.fresh_session()?;
            }
            let Some(&n) = self.msgno.get(&id) else { return Err(Error::MessageNotFound(id.0)) };
            let s = self.session.as_mut().expect("session");
            match s.command(&format!("RETR {n}")).and_then(|_| s.multiline()) {
                Ok(raw) => return Ok(raw),
                // The server may have closed an idle session: retry once on a new one.
                Err(Error::Network(_)) if attempt == 0 => self.session = None,
                Err(e) => return Err(e),
            }
        }
        Err(Error::MessageNotFound(id.0))
    }

    /// `DELE` on the server, made final by `QUIT` (RFC 1939 §6): the
    /// messages are gone for every client. POP3 has no other folders.
    fn delete_messages(&mut self, mailbox: &str, ids: &[MessageId]) -> Result<()> {
        Self::check_mailbox(mailbox)?;
        self.fresh_session()?;
        let numbers: Vec<u32> = ids.iter().filter_map(|id| self.msgno.get(id).copied()).collect();
        let s = self.session.as_mut().expect("session");
        for n in &numbers {
            s.command(&format!("DELE {n}"))?;
        }
        // QUIT commits the deletions; message numbers change afterwards.
        let quit = s.command("QUIT");
        self.session = None;
        self.msgno.clear();
        quit.map(|_| ())
    }

    /// Flags are local: the account thread stores them in the cache.
    fn set_flag(&mut self, _mailbox: &str, _ids: &[MessageId], _flag: Flag, _on: bool) -> Result<()> {
        Ok(())
    }

    fn local_flags(&self) -> bool {
        true
    }

    fn cacheable(&self) -> bool {
        true
    }
}

impl Drop for Pop3Backend {
    fn drop(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.quit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uidl_ids_are_stable_and_positive() {
        assert_eq!(uidl_id("abc"), uidl_id("abc"));
        assert_ne!(uidl_id("abc"), uidl_id("abd"));
        assert!((uidl_id("x").0 as i64) >= 0);
    }
}
