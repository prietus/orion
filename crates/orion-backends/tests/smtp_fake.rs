//! SMTP client against `scripts/fake_smtp.py` with real TLS (throwaway CA).
//! Own test binary: it sets SSL_CERT_FILE for the process.

mod common;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use orion_backends::smtp::{send, SmtpAuth, SmtpSettings};
use orion_core::config::TlsMode;
use orion_core::Error;

struct Server {
    child: Child,
    port: u16,
    tls_port: u16,
    out: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Server {
    fn start(certs: Option<&Path>, extra: &[&str]) -> Option<Server> {
        let out = tempfile::tempdir().ok()?;
        let mut cmd = Command::new("python3");
        cmd.arg("-I").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_smtp.py")).arg(out.path());
        if let Some(d) = certs {
            cmd.arg("--cert").arg(d.join("server.crt")).arg("--key").arg(d.join("server.key"));
        }
        let mut child = cmd.args(extra).stdout(Stdio::piped()).spawn().ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        let f: Vec<&str> = line.split_whitespace().collect();
        Some(Server { port: f[1].parse().ok()?, tls_port: f[3].parse().ok()?, child, out })
    }

    fn messages(&self) -> Vec<serde_json::Value> {
        let mut files: Vec<_> = std::fs::read_dir(self.out.path()).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        files.iter().map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()).collect()
    }

    fn settings(&self, tls: TlsMode, auth: SmtpAuth) -> SmtpSettings {
        let port = if tls == TlsMode::Tls { self.tls_port } else { self.port };
        SmtpSettings { host: "127.0.0.1".into(), port, tls, auth }
    }
}

fn pw(p: &str) -> SmtpAuth {
    SmtpAuth::Password { user: "ana".into(), password: p.into() }
}

#[test]
fn smtp_submission() {
    if Command::new("openssl").arg("version").output().is_err() {
        eprintln!("openssl not available: test skipped");
        return;
    }
    let certs = tempfile::tempdir().unwrap();
    common::make_certs(certs.path());
    // SAFETY: single test in this binary, set before any TLS use.
    unsafe { std::env::set_var("SSL_CERT_FILE", certs.path().join("ca.crt")) };
    let Some(srv) = Server::start(Some(certs.path()), &["--size", "4096"]) else {
        eprintln!("python3 not available: test skipped");
        return;
    };

    let message = "From: ana@example.org\r\nTo: bob@example.org\r\nSubject: hi\r\n\r\nline one\r\n.starts with a dot\r\n..two dots\r\nñ end\r\n";
    let rcpts = vec!["bob@example.org".to_string(), "hidden-bcc@example.org".to_string()];

    // STARTTLS + AUTH PLAIN; the data arrives byte for byte (dot-stuffing undone by the server).
    send(&srv.settings(TlsMode::Starttls, pw("s3cret")), "ana@example.org", &rcpts, message.as_bytes()).unwrap();
    // Implicit TLS + XOAUTH2.
    send(&srv.settings(TlsMode::Tls, SmtpAuth::XOAuth2 { user: "ana@example.org".into(), token: "tok-123".into() }), "ana@example.org", &rcpts[..1], b"Subject: x\r\n\r\ny\r\n").unwrap();
    let msgs = srv.messages();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["from"], "ana@example.org");
    assert_eq!(msgs[0]["rcpts"], serde_json::json!(["bob@example.org", "hidden-bcc@example.org"]));
    assert_eq!(msgs[0]["data"], message);
    assert_eq!(msgs[0]["tls"], true, "STARTTLS was really used");
    assert_eq!(msgs[1]["tls"], true);

    // Credentials.
    let e = send(&srv.settings(TlsMode::Starttls, pw("wrong")), "ana@example.org", &rcpts, b"x").unwrap_err();
    assert!(matches!(e, Error::Auth(_)), "{e:?}");
    let e = send(&srv.settings(TlsMode::Tls, SmtpAuth::XOAuth2 { user: "a".into(), token: "expired".into() }), "ana@example.org", &rcpts, b"x").unwrap_err();
    assert!(matches!(e, Error::Auth(_)), "{e:?}");

    // A rejected recipient aborts the whole message and is named.
    let bad = vec!["bob@example.org".to_string(), "reject-me@example.org".to_string()];
    let e = send(&srv.settings(TlsMode::Starttls, pw("s3cret")), "ana@example.org", &bad, b"Subject: x\r\n\r\ny\r\n").unwrap_err();
    assert!(e.to_string().contains("reject-me@example.org (550") && e.to_string().contains("nothing was sent"), "{e}");

    // The server's SIZE limit is honoured before transferring anything.
    let big = vec![b'a'; 5000];
    let e = send(&srv.settings(TlsMode::Starttls, pw("s3cret")), "ana@example.org", &rcpts, &big).unwrap_err();
    assert!(e.to_string().contains("exceeds the server limit"), "{e}");
    assert_eq!(srv.messages().len(), 2, "nothing else was delivered");

    // LOGIN when PLAIN is not offered.
    let login_only = Server::start(Some(certs.path()), &["--mechs", "LOGIN"]).unwrap();
    send(&login_only.settings(TlsMode::Tls, pw("s3cret")), "ana@example.org", &rcpts[..1], b"Subject: l\r\n\r\nz\r\n").unwrap();
    assert_eq!(login_only.messages().len(), 1);

    // STARTTLS requested but not offered: refuse to send credentials in clear text.
    let no_tls = Server::start(None, &[]).unwrap();
    let e = send(&no_tls.settings(TlsMode::Starttls, pw("s3cret")), "ana@example.org", &rcpts, b"x").unwrap_err();
    assert!(e.to_string().contains("does not offer STARTTLS"), "{e}");
    assert!(no_tls.messages().is_empty());
}
