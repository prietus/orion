//! SMTP submission client (RFC 5321/6409): implicit TLS (465) or STARTTLS
//! (587), AUTH PLAIN/LOGIN/XOAUTH2, SIZE, 8BITMIME.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use orion_core::config::TlsMode;
use orion_core::{Error, Result};

use crate::net::{self, Conn};

const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub enum SmtpAuth {
    /// No authentication (only sensible for a local relay).
    None,
    Password { user: String, password: String },
    XOAuth2 { user: String, token: String },
}

#[derive(Debug, Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub auth: SmtpAuth,
}

struct Session {
    io: BufReader<Conn>,
    caps: Vec<String>,
}

fn net_err(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

/// A server reply: code and text lines.
#[derive(Debug)]
struct Reply {
    code: u16,
    text: String,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..400).contains(&self.code)
    }

    fn into_error(self, what: &str) -> Error {
        let msg = format!("{what}: {} {}", self.code, self.text);
        match self.code {
            530 | 534 | 535 | 538 => Error::Auth(msg),
            400..=499 => Error::Network(format!("{msg} (temporary failure, try again later)")),
            _ => Error::Protocol(msg),
        }
    }
}

impl Session {
    fn read_reply(&mut self) -> Result<Reply> {
        let mut text = Vec::new();
        loop {
            let mut line = String::new();
            if self.io.read_line(&mut line).map_err(net_err)? == 0 {
                return Err(net_err("the SMTP server closed the connection"));
            }
            let line = line.trim_end();
            if line.len() < 3 {
                return Err(Error::Protocol(format!("invalid SMTP reply: {line}")));
            }
            let code: u16 = line[..3].parse().map_err(|_| Error::Protocol(format!("invalid SMTP reply: {line}")))?;
            text.push(line.get(4..).unwrap_or_default().to_string());
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok(Reply { code, text: text.join(" ") });
            }
        }
    }

    fn send(&mut self, line: &str) -> Result<()> {
        let w = self.io.get_mut();
        w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\r\n")).and_then(|_| w.flush()).map_err(net_err)
    }

    fn command(&mut self, line: &str) -> Result<Reply> {
        self.send(line)?;
        self.read_reply()
    }

    fn ehlo(&mut self, name: &str) -> Result<()> {
        self.send(&format!("EHLO {name}"))?;
        let mut caps = Vec::new();
        loop {
            let mut line = String::new();
            if self.io.read_line(&mut line).map_err(net_err)? == 0 {
                return Err(net_err("the SMTP server closed the connection"));
            }
            let l = line.trim_end();
            if !l.starts_with("250") {
                return Err(Error::Protocol(format!("EHLO rejected: {l}")));
            }
            caps.push(l.get(4..).unwrap_or_default().to_ascii_uppercase());
            if l.as_bytes().get(3) != Some(&b'-') {
                break;
            }
        }
        self.caps = caps.into_iter().skip(1).collect(); // first line is the greeting text
        Ok(())
    }

    fn cap(&self, name: &str) -> Option<&str> {
        self.caps.iter().find(|c| c.split_whitespace().next() == Some(name)).map(String::as_str)
    }

    fn auth_mechanisms(&self) -> Vec<String> {
        self.cap("AUTH").map(|c| c.split_whitespace().skip(1).map(str::to_string).collect()).unwrap_or_default()
    }
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

/// CRLF line endings, dot-stuffing and the final terminator (RFC 5321 §4.5.2).
pub fn dot_stuff(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    let mut at_line_start = true;
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        if at_line_start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        at_line_start = b == b'\n';
        prev = b;
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

/// Sends `data` from `from` to `rcpts`. If the server rejects any recipient
/// nothing is sent and the error lists them.
pub fn send(s: &SmtpSettings, from: &str, rcpts: &[String], data: &[u8]) -> Result<()> {
    if rcpts.is_empty() {
        return Err(Error::Protocol("no recipients".into()));
    }
    let tcp = net::tcp(&s.host, s.port, TIMEOUT)?;
    let conn = match s.tls {
        TlsMode::Tls => Conn::Tls(Box::new(net::tls(tcp, &s.host)?)),
        _ => Conn::Plain(tcp),
    };
    let mut sess = Session { io: BufReader::new(conn), caps: Vec::new() };
    let greeting = sess.read_reply()?;
    if greeting.code != 220 {
        return Err(greeting.into_error("SMTP greeting"));
    }
    // Identify with the sender's domain, not the local host name.
    let helo = from.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_else(|| "localhost".into());
    sess.ehlo(&helo)?;
    if s.tls == TlsMode::Starttls {
        if sess.cap("STARTTLS").is_none() {
            return Err(net_err("the server does not offer STARTTLS; refusing to continue unencrypted"));
        }
        let r = sess.command("STARTTLS")?;
        if r.code != 220 {
            return Err(r.into_error("STARTTLS"));
        }
        net::upgrade(&mut sess.io, &s.host)?;
        sess.ehlo(&helo)?; // capabilities must be re-read after TLS
    }

    match &s.auth {
        SmtpAuth::None => {}
        SmtpAuth::XOAuth2 { user, token } => {
            let r = sess.command(&format!("AUTH XOAUTH2 {}", b64(format!("user={user}\x01auth=Bearer {token}\x01\x01").as_bytes())))?;
            let r = if r.code == 334 { sess.command("")? } else { r }; // error challenge → empty reply
            if r.code != 235 {
                return Err(Error::Auth(format!("XOAUTH2 rejected: {} {}", r.code, r.text)));
            }
        }
        SmtpAuth::Password { user, password } => {
            let mechs = sess.auth_mechanisms();
            let r = if mechs.iter().any(|m| m == "PLAIN") || mechs.is_empty() {
                sess.command(&format!("AUTH PLAIN {}", b64(format!("\0{user}\0{password}").as_bytes())))?
            } else if mechs.iter().any(|m| m == "LOGIN") {
                let r = sess.command("AUTH LOGIN")?;
                if r.code != 334 {
                    return Err(r.into_error("AUTH LOGIN"));
                }
                let r = sess.command(&b64(user.as_bytes()))?;
                if r.code != 334 {
                    return Err(r.into_error("AUTH LOGIN"));
                }
                sess.command(&b64(password.as_bytes()))?
            } else {
                return Err(Error::Auth(format!("no supported AUTH mechanism (server offers {})", mechs.join(" "))));
            };
            if r.code != 235 {
                return Err(Error::Auth(format!("{} {}", r.code, r.text)));
            }
        }
    }

    let body = dot_stuff(data);
    let mut mail_from = format!("MAIL FROM:<{from}>");
    if let Some(limit) = sess.cap("SIZE").and_then(|c| c.split_whitespace().nth(1)).and_then(|n| n.parse::<usize>().ok()).filter(|&n| n > 0) {
        if body.len() > limit {
            return Err(Error::Protocol(format!("the message ({} KB) exceeds the server limit ({} KB)", body.len() / 1024, limit / 1024)));
        }
        mail_from.push_str(&format!(" SIZE={}", body.len()));
    }
    if !data.is_ascii() && sess.cap("8BITMIME").is_some() {
        mail_from.push_str(" BODY=8BITMIME");
    }
    let r = sess.command(&mail_from)?;
    if !r.ok() {
        return Err(r.into_error("MAIL FROM"));
    }
    let mut rejected = Vec::new();
    for rcpt in rcpts {
        let r = sess.command(&format!("RCPT TO:<{rcpt}>"))?;
        if !r.ok() {
            rejected.push(format!("{rcpt} ({} {})", r.code, r.text));
        }
    }
    if !rejected.is_empty() {
        let _ = sess.command("RSET");
        let _ = sess.command("QUIT");
        return Err(Error::Protocol(format!("the server rejected: {}; nothing was sent", rejected.join(", "))));
    }
    let r = sess.command("DATA")?;
    if r.code != 354 {
        return Err(r.into_error("DATA"));
    }
    sess.io.get_mut().write_all(&body).and_then(|_| sess.io.get_mut().flush()).map_err(net_err)?;
    let r = sess.read_reply()?;
    if !r.ok() {
        return Err(r.into_error("message refused"));
    }
    let _ = sess.command("QUIT");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffing() {
        assert_eq!(dot_stuff(b"a\n.b\n..c\nd"), b"a\r\n..b\r\n...c\r\nd\r\n.\r\n");
        assert_eq!(dot_stuff(b".\r\n"), b"..\r\n.\r\n");
        assert_eq!(b64(b"\0ana\0s3cret"), "AGFuYQBzM2NyZXQ=");
    }
}
