//! `orion daemon`: syncs in the background (the same account threads the UI
//! uses), detects new mail, notifies and serves the local API.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use orion_core::config::Config;
use orion_core::model::{Envelope, MailboxInfo};

use crate::ipc::{AccountStatus, Command, DaemonEvent, MailboxStatus, NewMessage, Reply, Status, UnreadMessage};
use crate::worker::{self, resolve_watch, Event, Request};

struct Shared {
    /// The cache the account threads fill: where the unread list comes from.
    cache: Option<PathBuf>,
    accounts: Vec<AccountState>,
    subscribers: Vec<Sender<String>>,
}

struct AccountState {
    name: String,
    watch: Vec<String>,
    mailboxes: Vec<MailboxInfo>,
    error: Option<String>,
    activity: String,
    last_sync: Option<u64>,
}

impl Shared {
    fn status(&self) -> Status {
        let accounts: Vec<AccountStatus> = self
            .accounts
            .iter()
            .map(|a| AccountStatus {
                name: a.name.clone(),
                error: a.error.clone(),
                activity: a.activity.clone(),
                last_sync: a.last_sync,
                mailboxes: mailbox_status(a),
            })
            .collect();
        let unread = accounts.iter().flat_map(|a| &a.mailboxes).filter(|m| m.watched).map(|m| m.unread).sum();
        let unread_all = accounts.iter().flat_map(|a| &a.mailboxes).map(|m| m.unread).sum();
        let messages = self.unread_messages(&accounts);
        Status { version: env!("CARGO_PKG_VERSION").into(), unread, unread_all, accounts, messages }
    }

    /// Newest unread messages of the watched mailboxes, from the cache.
    fn unread_messages(&self, accounts: &[AccountStatus]) -> Vec<UnreadMessage> {
        const MAX: usize = 10;
        let Some(store) = self.cache.as_deref().and_then(|p| orion_store::Store::open(p).ok()) else { return Vec::new() };
        let mut out = Vec::new();
        for a in accounts {
            for m in a.mailboxes.iter().filter(|m| m.watched && m.unread > 0) {
                for e in store.recent_unread(&a.name, &m.id, MAX).unwrap_or_default() {
                    out.push(UnreadMessage {
                        account: a.name.clone(),
                        mailbox: m.name.clone(),
                        from: e.sender().to_string(),
                        subject: e.subject.clone(),
                        date: e.date.clone(),
                        timestamp: e.timestamp,
                    });
                }
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        out.truncate(MAX);
        out
    }

    /// Sends an event line to every subscriber, dropping the ones that left.
    fn broadcast(&mut self, ev: &DaemonEvent) {
        let line = serde_json::to_string(ev).expect("serializable event");
        self.subscribers.retain(|s| s.send(line.clone()).is_ok());
    }
}

fn mailbox_status(a: &AccountState) -> Vec<MailboxStatus> {
    let watched = resolve_watch(&a.watch, &a.mailboxes);
    a.mailboxes
        .iter()
        .map(|m| MailboxStatus { id: m.id.clone(), name: m.name.clone(), total: m.total, unread: m.unread, watched: watched.contains(&m.id) })
        .collect()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Binds the socket, refusing to start if another daemon answers on it.
fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!("another orion daemon is already running on {}", path.display());
        }
        // Stale socket from a daemon that did not shut down cleanly.
        std::fs::remove_file(path).with_context(|| format!("removing stale socket {}", path.display()))?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    // Only the owner may talk to the daemon.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Runs the daemon until `stop` becomes true (SIGTERM/SIGINT in `main`).
pub fn run(cfg: Config, cache: Option<PathBuf>, socket: PathBuf, stop: Arc<AtomicBool>) -> Result<()> {
    if cfg.accounts.is_empty() {
        bail!("no accounts configured");
    }
    let listener = bind(&socket)?;
    let (tx, rx) = mpsc::channel::<Event>();
    let gpg = None; // the daemon never opens message bodies
    let workers: Vec<Sender<Request>> = cfg
        .accounts
        .iter()
        .enumerate()
        .map(|(i, acc)| worker::spawn_with(i, acc.clone(), cache.clone(), gpg.clone(), tx.clone(), worker::Options { watch_on_start: true }))
        .collect();
    let shared = Arc::new(Mutex::new(Shared {
        cache: cache.clone(),
        accounts: cfg
            .accounts
            .iter()
            .map(|a| AccountState { name: a.name.clone(), watch: a.watch.clone(), mailboxes: Vec::new(), error: None, activity: "starting…".into(), last_sync: None })
            .collect(),
        subscribers: Vec::new(),
    }));

    {
        let shared = shared.clone();
        std::thread::Builder::new().name("api".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let (shared, workers) = (shared.clone(), workers.clone());
                std::thread::spawn(move || {
                    let _ = serve_client(stream, shared, workers);
                });
            }
        })?;
    }

    eprintln!("orion daemon: listening on {}", socket.display());
    while !stop.load(Ordering::Relaxed) {
        let ev = match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        handle_event(&cfg, &shared, ev);
    }
    let _ = std::fs::remove_file(&socket);
    eprintln!("orion daemon: stopped");
    Ok(())
}

fn handle_event(cfg: &Config, shared: &Mutex<Shared>, ev: Event) {
    let mut s = shared.lock().expect("daemon state");
    match ev {
        Event::Mailboxes { account, list } => {
            let Some(a) = s.accounts.get_mut(account) else { return };
            let before: Vec<(String, usize)> = a.mailboxes.iter().map(|m| (m.id.clone(), m.unread)).collect();
            a.mailboxes = list;
            a.error = None;
            let after: Vec<(String, usize)> = a.mailboxes.iter().map(|m| (m.id.clone(), m.unread)).collect();
            if before != after {
                let mailboxes = mailbox_status(a);
                let unread = mailboxes.iter().filter(|m| m.watched).map(|m| m.unread).sum();
                let ev = DaemonEvent::Counts { account: a.name.clone(), unread, mailboxes };
                s.broadcast(&ev);
            }
        }
        Event::Envelopes { account, from_cache: false, .. } => {
            if let Some(a) = s.accounts.get_mut(account) {
                a.last_sync = Some(now());
            }
        }
        Event::Progress { account, text } => {
            if let Some(a) = s.accounts.get_mut(account) {
                a.activity = text;
            }
        }
        Event::Error { account, text } => {
            let Some(a) = s.accounts.get_mut(account) else { return };
            eprintln!("orion daemon: [{}] {text}", a.name);
            a.error = Some(text.clone());
            let ev = DaemonEvent::Error { account: a.name.clone(), message: text };
            s.broadcast(&ev);
        }
        Event::NewMail { account, mailbox, mailbox_name, envelopes } => {
            let Some(a) = s.accounts.get(account) else { return };
            let name = a.name.clone();
            let unread = a.mailboxes.iter().find(|m| m.id == mailbox).map(|m| m.unread).unwrap_or(envelopes.len());
            let messages: Vec<NewMessage> = envelopes
                .iter()
                .map(|e| NewMessage { from: e.sender().to_string(), subject: e.subject.clone(), date: e.date.clone() })
                .collect();
            s.broadcast(&DaemonEvent::NewMail { account: name.clone(), mailbox: mailbox_name.clone(), messages });
            drop(s);
            notify(cfg, &name, &mailbox_name, &envelopes, unread);
        }
        _ => {}
    }
}

/// Desktop notification and user hook. Neither waits: a thread reaps the child.
fn notify(cfg: &Config, account: &str, mailbox: &str, envelopes: &[Envelope], unread: usize) {
    let n = envelopes.len();
    let first = &envelopes[0];
    if cfg.daemon.notify {
        let title = if n == 1 { format!("New mail · {account}/{mailbox}") } else { format!("{n} new messages · {account}/{mailbox}") };
        let body: Vec<String> = envelopes.iter().take(5).map(|e| format!("{} — {}", e.sender(), e.subject)).collect();
        let mut parts = cfg.daemon.notify_command.split_whitespace();
        if let Some(prog) = parts.next() {
            let mut cmd = std::process::Command::new(prog);
            cmd.args(parts);
            if prog.ends_with("notify-send") {
                cmd.arg("--app-name=Orion");
            }
            cmd.arg(title).arg(body.join("\n"));
            spawn_detached(cmd);
        }
    }
    if let Some(hook) = &cfg.daemon.on_new_mail {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(hook)
            .env("ORION_ACCOUNT", account)
            .env("ORION_MAILBOX", mailbox)
            .env("ORION_COUNT", n.to_string())
            .env("ORION_FROM", first.sender())
            .env("ORION_SUBJECT", &first.subject)
            .env("ORION_UNREAD", unread.to_string());
        spawn_detached(cmd);
    }
}

fn spawn_detached(mut cmd: std::process::Command) {
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("orion daemon: could not run {:?}: {e}", cmd.get_program()),
    }
}

fn serve_client(stream: UnixStream, shared: Arc<Mutex<Shared>>, workers: Vec<Sender<Request>>) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Command>(&line) {
            Err(e) => Reply::err(format!("invalid request: {e}")),
            Ok(Command::Ping) => Reply::ok(),
            Ok(Command::Status) => Reply { status: Some(shared.lock().expect("daemon state").status()), ..Reply::ok() },
            Ok(Command::Sync) => {
                for w in &workers {
                    let _ = w.send(Request::Refresh);
                }
                Reply::ok()
            }
            Ok(Command::Reload) => {
                for w in &workers {
                    let _ = w.send(Request::Recount);
                }
                Reply::ok()
            }
            Ok(Command::Subscribe) => {
                let (tx, rx) = mpsc::channel::<String>();
                shared.lock().expect("daemon state").subscribers.push(tx);
                writeln!(writer, "{}", serde_json::to_string(&Reply::ok()).expect("json"))?;
                // From here on this connection only receives events.
                for ev in rx {
                    writeln!(writer, "{ev}")?;
                }
                return Ok(());
            }
        };
        writeln!(writer, "{}", serde_json::to_string(&reply).expect("json"))?;
    }
    Ok(())
}
