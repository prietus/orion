//! Command-line clients of the daemon API: `orion status`, `orion watch`, `orion sync`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::ipc::{Command, DaemonEvent, Reply, Status};

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    pub fn connect(socket: &Path) -> Result<Client> {
        Self::connect_timeout(socket, Duration::from_secs(10))
    }

    fn connect_timeout(socket: &Path, timeout: Duration) -> Result<Client> {
        let stream = UnixStream::connect(socket)
            .with_context(|| format!("orion daemon is not running (no socket at {})", socket.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Client { writer: stream.try_clone()?, reader: BufReader::new(stream) })
    }

    pub fn request(&mut self, cmd: &Command) -> Result<Reply> {
        writeln!(self.writer, "{}", serde_json::to_string(cmd)?)?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        if line.is_empty() {
            bail!("the daemon closed the connection");
        }
        let reply: Reply = serde_json::from_str(&line).context("invalid reply from the daemon")?;
        if !reply.ok {
            bail!("daemon error: {}", reply.error.unwrap_or_default());
        }
        Ok(reply)
    }

    /// Sends a raw line and returns the reply without interpreting `ok` (for tests and debugging).
    #[cfg(test)]
    pub fn request_raw(&mut self, line: &str) -> Result<Reply> {
        writeln!(self.writer, "{line}")?;
        let mut reply = String::new();
        self.reader.read_line(&mut reply)?;
        Ok(serde_json::from_str(&reply)?)
    }

    pub fn status(&mut self) -> Result<Status> {
        self.request(&Command::Status)?.status.context("reply without status")
    }

    /// Subscribes and calls `f` for every event until the connection closes.
    pub fn subscribe(mut self, mut f: impl FnMut(&str, DaemonEvent) -> Result<()>) -> Result<()> {
        self.request(&Command::Subscribe)?;
        self.writer.set_read_timeout(None)?;
        self.reader.get_ref().set_read_timeout(None)?;
        for line in self.reader.lines() {
            let line = line?;
            if let Ok(ev) = serde_json::from_str::<DaemonEvent>(&line) {
                f(&line, ev)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusFormat {
    Human,
    Json,
    /// Waybar custom module: {"text","tooltip","class"}.
    Waybar,
}

pub fn print_status(socket: &Path, format: StatusFormat) -> Result<()> {
    let status = match Client::connect(socket).and_then(|mut c| c.status()) {
        Ok(s) => s,
        // A status bar should not show an error dialog: report "offline" instead.
        Err(_) if format == StatusFormat::Waybar => {
            println!(r#"{{"text":"✉ ?","tooltip":"orion daemon is not running","class":"offline"}}"#);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    match format {
        StatusFormat::Json => println!("{}", serde_json::to_string_pretty(&status)?),
        StatusFormat::Waybar => {
            // Only what is new: the unread messages (subject, sender, mailbox),
            // and account errors. Nothing new → empty tooltip.
            let mut tooltip: Vec<String> =
                status.messages.iter().map(|m| format!("{}\n    {} · {}/{}", m.subject, m.from, m.account, m.mailbox)).collect();
            if status.unread > status.messages.len() && !status.messages.is_empty() {
                tooltip.push(format!("+{} more", status.unread - status.messages.len()));
            }
            for a in &status.accounts {
                if let Some(e) = &a.error {
                    tooltip.push(format!("{}: {e}", a.name));
                }
            }
            let class = if status.accounts.iter().any(|a| a.error.is_some()) {
                "error"
            } else if status.unread > 0 {
                "unread"
            } else {
                "idle"
            };
            let out = serde_json::json!({
                "text": format!("✉ {}", status.unread),
                "tooltip": tooltip.join("\n"),
                "class": class,
                // For bars that build their own popover (contrib/waymobar.hs).
                "unread": status.unread,
                "messages": status.messages,
                "errors": status.accounts.iter().filter_map(|a| a.error.as_ref().map(|e| format!("{}: {e}", a.name))).collect::<Vec<_>>(),
            });
            println!("{out}");
        }
        StatusFormat::Human => {
            for a in &status.accounts {
                let when = a.last_sync.map(|t| format!("synced {} ago", ago(t))).unwrap_or_else(|| "not synced yet".into());
                println!("{}  ({when}{})", a.name, if a.activity.is_empty() { String::new() } else { format!(", {}", a.activity) });
                if let Some(e) = &a.error {
                    println!("  ! {e}");
                }
                for m in &a.mailboxes {
                    let mark = if m.watched { "*" } else { " " };
                    println!("  {mark} {:<30} {:>6} unread / {:>7} total", m.name, m.unread, m.total);
                }
            }
            println!("{} unread in watched mailboxes (*), {} in total", status.unread, status.unread_all);
        }
    }
    Ok(())
}

pub fn watch(socket: &Path, json: bool) -> Result<()> {
    let client = Client::connect(socket)?;
    client.subscribe(|line, ev| {
        if json {
            println!("{line}");
        } else {
            match ev {
                DaemonEvent::NewMail { account, mailbox, messages } => {
                    for m in messages {
                        println!("new  {account}/{mailbox}  {}  {} — {}", m.date, m.from, m.subject);
                    }
                }
                DaemonEvent::Counts { account, unread, .. } => println!("count {account}: {unread} unread (watched)"),
                DaemonEvent::Error { account, message } => println!("error {account}: {message}"),
            }
        }
        std::io::stdout().flush()?;
        Ok(())
    })
}

pub fn sync(socket: &Path) -> Result<()> {
    Client::connect(socket)?.request(&Command::Sync)?;
    Ok(())
}

fn ago(t: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(t);
    let s = now.saturating_sub(t);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// Socket of the daemon to tell about local changes (set by the interactive
/// UI; unset in the daemon itself, so it never notifies itself).
pub static NOTIFY_SOCKET: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Tells a running daemon to re-read the counts from the cache (fire and
/// forget, in the background; nothing happens if no daemon runs).
pub fn notify_daemon() {
    let Some(socket) = NOTIFY_SOCKET.get().filter(|s| s.exists()).cloned() else { return };
    std::thread::spawn(move || {
        if let Ok(mut c) = Client::connect_timeout(&socket, Duration::from_secs(2)) {
            let _ = c.request(&Command::Reload);
        }
    });
}
