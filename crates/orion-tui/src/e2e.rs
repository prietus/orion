//! End-to-end tests: UI + account thread + SQLite cache + fake IMAP server
//! (`scripts/fake_imap.py`). Skipped without python3.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_image::picker::Picker;

use orion_core::config::{AccountConfig, AccountKind, Config, ImapConfig, TlsMode};

use crate::app::{Account, App, Focus};
use crate::keys::Keymap;
use crate::worker::{self, Event};

struct Fake {
    child: Child,
    port: u16,
    admin: BufReader<TcpStream>,
}

impl Fake {
    fn start() -> Option<Fake> {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_imap.py");
        let mut child = Command::new("python3").arg("-I").arg(script).stdout(Stdio::piped()).spawn().ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        let p: Vec<&str> = line.split_whitespace().collect();
        let admin = BufReader::new(TcpStream::connect(("127.0.0.1", p[3].parse::<u16>().ok()?)).ok()?);
        Some(Fake { child, port: p[1].parse().ok()?, admin })
    }

    fn admin(&mut self, cmd: &str) -> String {
        writeln!(self.admin.get_mut(), "{cmd}").unwrap();
        let mut r = String::new();
        self.admin.read_line(&mut r).unwrap();
        r.trim().to_string()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Ui {
    app: App,
    rx: mpsc::Receiver<Event>,
}

impl Ui {
    fn new(port: u16, cache: &std::path::Path) -> Ui {
        Self::with_imap(
            ImapConfig {
                host: "127.0.0.1".into(),
                port: Some(port),
                tls: TlsMode::None,
                user: "yo".into(),
                password: Some("secreta".into()),
                password_command: None,
                poll_interval: 3600,
                cache_bodies: true,
                idle: false,
                oauth: None,
                account: String::new(),
            },
            cache,
        )
    }

    fn with_imap(imap: ImapConfig, cache: &std::path::Path) -> Ui {
        let acc = AccountConfig {
            name: "fake".into(),
            watch: Vec::new(),
            smtp: None,
            from: None,
            signature: None,
            pgp_key: None,
            save_sent: None,
            kind: AccountKind::Imap(imap),
        };
        let (tx, rx) = mpsc::channel();
        let cfg = Config::default();
        let (keymap, _) = Keymap::new(&cfg.keys);
        let accounts = vec![Account {
            name: "fake".into(),
            tx: worker::spawn(0, acc, Some(cache.to_path_buf()), None, tx.clone()),
            mailboxes: Vec::new(),
            error: None,
            progress: String::new(),
        }];
        Ui { app: App::new(cfg, keymap, Some(Picker::halfblocks()), tx, accounts), rx }
    }

    fn until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond(&self.app) {
            match self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(ev) => self.app.on_event(ev),
                Err(_) => panic!("timed out waiting for: {what} (status: {})", self.app.status),
            }
        }
    }

    fn key(&mut self, c: KeyCode) {
        self.app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
    }

    fn select_subject(&mut self, subject: &str) {
        let pos = self.app.visible.iter().position(|&i| self.app.envelopes[i].subject == subject).expect(subject);
        self.app.list_state.select(Some(pos));
    }
}

#[test]
fn imap_cache_offline() {
    let Some(mut fake) = Fake::start() else {
        eprintln!("python3 not available: test skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("orion-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = dir.join("cache.sqlite");

    // 1) First run: no cache, sync against the server.
    {
        let mut ui = Ui::new(fake.port, &cache);
        ui.until("mailboxes", |a| a.accounts[0].mailboxes.len() == 4);
        let names: Vec<_> = ui.app.accounts[0].mailboxes.iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, ["INBOX", "Envíados", "Archivo", "Archivo/2024"]);
        ui.until("envelopes synced", |a| a.envelopes.len() == 3 && !a.loading);
        assert_eq!(ui.app.current_mailbox().unwrap().unread, 2);

        // Open the unread one: it is downloaded and marked read locally and on the server.
        ui.select_subject("Segundo con ñ");
        ui.key(KeyCode::Enter);
        assert_eq!(ui.app.focus, Focus::Reader);
        ui.until("body", |a| a.reader.as_ref().is_some_and(|r| r.view.is_some()));
        assert!(ui.app.reader.as_ref().unwrap().view.as_ref().unwrap().text.contains("Cuerpo del segundo"));
        assert_eq!(ui.app.current_mailbox().unwrap().unread, 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fake.admin("FLAGS INBOX 2").contains("\\Seen") {
            assert!(Instant::now() < deadline, "the server did not receive \\Seen");
            while let Ok(ev) = ui.rx.try_recv() {
                ui.app.on_event(ev);
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Important → \Flagged on the server.
        ui.key(KeyCode::Char('F'));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fake.admin("FLAGS INBOX 2").contains("\\Flagged") {
            assert!(Instant::now() < deadline, "the server did not receive \\Flagged");
            std::thread::sleep(Duration::from_millis(20));
        }

        // New mail + reload.
        ui.key(KeyCode::Esc);
        fake.admin("ADD INBOX Llega uno nuevo");
        ui.app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)); // reload
        ui.until("new message", |a| a.envelopes.iter().any(|e| e.subject == "Llega uno nuevo"));
        // The selection stays on the same message even though the list changed.
        let sel = ui.app.selected_envelope().unwrap();
        assert_eq!(ui.app.envelopes[sel].subject, "Segundo con ñ");
    }

    // 2) Server down: everything above comes from the cache.
    drop(fake);
    {
        let mut ui = Ui::new(1, &cache); // port 1: connection refused
        ui.until("envelopes from cache", |a| a.envelopes.len() == 4);
        assert!(ui.app.loading, "must show that it has not synced yet");
        ui.until("network error shown", |a| a.status.contains("error"));
        let e = ui.app.envelopes.iter().find(|e| e.subject == "Segundo con ñ").unwrap();
        assert!(e.flags.seen && e.flags.flagged, "the flags were stored in the cache");

        ui.select_subject("Segundo con ñ");
        ui.key(KeyCode::Enter);
        ui.until("body from cache", |a| a.reader.as_ref().is_some_and(|r| r.view.is_some() || r.error.is_some()));
        let r = ui.app.reader.as_ref().unwrap();
        assert!(r.error.is_none(), "{:?}", r.error);
        assert!(r.view.as_ref().unwrap().text.contains("Cuerpo del segundo"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- PGP

mod pgp {
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use orion_core::config::{AccountConfig, AccountKind, Config};
    use orion_crypto::Gpg;

    use crate::app::{Account, App, Focus, Mode};
    use crate::keys::Keymap;
    use crate::worker;

    fn gpg_cmd(home: &Path, args: &[&str]) -> String {
        let out = Command::new("gpg").env("GNUPGHOME", home).args(["--batch", "--quiet"]).args(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn new_key(home: &Path, uid: &str, pass: &str) {
        gpg_cmd(home, &["--pinentry-mode", "loopback", "--passphrase", pass, "--quick-gen-key", uid, "ed25519", "sign", "never"]);
        let fpr = gpg_cmd(home, &["--with-colons", "--list-keys", uid]);
        let fpr = fpr.lines().find(|l| l.starts_with("fpr:")).unwrap().split(':').nth(9).unwrap().to_string();
        gpg_cmd(home, &["--pinentry-mode", "loopback", "--passphrase", pass, "--quick-add-key", &fpr, "cv25519", "encr", "never"]);
    }

    struct Kill(std::path::PathBuf);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf").env("GNUPGHOME", &self.0).args(["--kill", "all"]).status();
        }
    }

    #[test]
    fn decrypt_with_pin_prompt() {
        if Command::new("gpg").arg("--version").output().is_err() {
            eprintln!("gpg not available: test skipped");
            return;
        }
        let dir = tempfile::Builder::new().prefix("orion-ui-gnupg-").tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let home = dir.path().join("gnupg");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let _kill = Kill(home.clone());
        new_key(&home, "Ana <ana@example.com>", "1234");
        new_key(&home, "Bob <bob@example.com>", "");
        let mut gpg = Gpg::new("gpg");
        gpg.home = Some(home.clone());

        // Message encrypted to Ana and signed by Bob, in PGP/MIME.
        let inner = "Content-Type: text/plain; charset=utf-8\n\nEl código de la caja fuerte es 4815.";
        let ct = gpg.encrypt(inner.as_bytes(), &["ana@example.com"], Some("bob@example.com"), None).unwrap();
        let _ = Command::new("gpgconf").env("GNUPGHOME", &home).args(["--kill", "gpg-agent"]).status();
        let mbox = format!(
            "From bob@example.com Thu Nov 20 10:00:00 2025\nFrom: Bob <bob@example.com>\nTo: ana@example.com\nSubject: Secreto\n\
             Date: Thu, 20 Nov 2025 10:00:00 +0100\nMIME-Version: 1.0\n\
             Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"E\"\n\n\
             --E\nContent-Type: application/pgp-encrypted\n\nVersion: 1\n\n--E\nContent-Type: application/octet-stream\n\n{}\n--E--\n",
            String::from_utf8(ct).unwrap().trim_end()
        );
        let path = dir.path().join("pgp.mbox");
        std::fs::write(&path, mbox).unwrap();

        let acc = AccountConfig::new("pgp", AccountKind::Mbox { path });
        let (tx, rx) = mpsc::channel();
        let cfg = Config::default();
        let (keymap, _) = Keymap::new(&cfg.keys);
        let accounts = vec![Account {
            name: "pgp".into(),
            tx: worker::spawn(0, acc, None, Some(gpg.clone()), tx.clone()),
            mailboxes: Vec::new(),
            error: None,
            progress: String::new(),
        }];
        let mut app = App::new(cfg, keymap, None, tx, accounts);
        app.gpg = Some(gpg);

        let until = |app: &mut App, what: &str, cond: &dyn Fn(&App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !cond(app) {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(ev) => app.on_event(ev),
                    Err(_) => panic!("timed out waiting for: {what} (status: {})", app.status),
                }
            }
        };
        let key = |app: &mut App, c: KeyCode| app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
        let typ = |app: &mut App, s: &str| s.chars().for_each(|c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        let screen = |app: &mut App| {
            let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
            t.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buf = t.backend().buffer();
            (0..buf.area.height)
                .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };

        until(&mut app, "list", &|a| a.envelopes.len() == 1 && !a.loading);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Reader);
        until(&mut app, "PIN prompt", &|a| matches!(a.mode, Mode::Pin { .. }));
        let s = screen(&mut app);
        println!("{s}");
        assert!(s.contains("passphrase for key"), "the prompt says which key");
        assert!(s.contains("press Enter and enter the"));
        assert!(!s.contains("4815"), "nothing decrypted yet");

        // Wrong PIN: it is masked and asked again.
        typ(&mut app, "9999");
        let s = screen(&mut app);
        assert!(s.contains("••••") && !s.contains("9999"), "the PIN is not shown");
        key(&mut app, KeyCode::Enter);
        assert!(screen(&mut app).contains("decrypting"));
        until(&mut app, "new prompt after wrong PIN", &|a| matches!(a.mode, Mode::Pin { .. }) && a.status.contains("wrong"));

        // Correct PIN.
        typ(&mut app, "1234");
        key(&mut app, KeyCode::Enter);
        until(&mut app, "decrypted", &|a| {
            a.reader.as_ref().and_then(|r| r.view.as_ref()).and_then(|v| v.crypto.as_ref()).is_some_and(|c| c.decrypted)
        });
        let s = screen(&mut app);
        println!("{s}");
        assert!(s.contains("El código de la caja fuerte es 4815."));
        assert!(s.contains("decrypted (PGP/MIME)"));
        assert!(!s.contains("wrong"), "the wrong-PIN warning disappears after success");
        assert!(s.contains("✔ valid · Bob <bob@example.com> · ultimate trust"));
        assert!(matches!(app.mode, Mode::Normal));

        // Card status: none is connected here.
        key(&mut app, KeyCode::Char('Y'));
        until(&mut app, "card popup", &|a| matches!(a.mode, Mode::Card(_)));
        let s = screen(&mut app);
        println!("{s}");
        // Depends on the machine: with a YubiKey plugged in, its details are shown.
        assert!(s.contains("No OpenPGP card connected") || (s.contains("Card:") && s.contains("PIN retries")), "{s}");
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }
}

// ------------------------------------------------------------------ daemon

mod daemon {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    use orion_core::config::{AccountConfig, AccountKind, Config, ImapConfig, TlsMode};

    use super::Fake;
    use crate::client::Client;
    use crate::ipc::{Command, DaemonEvent};

    fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn daemon_api_and_new_mail() {
        let Some(mut fake) = Fake::start() else {
            eprintln!("python3 not available: test skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("orion.sock");
        let hook_out = dir.path().join("hook.txt");
        let mut cfg = Config::default();
        cfg.daemon.notify = false;
        // The subject comes from the network: it must reach the hook as data, never as code.
        cfg.daemon.on_new_mail = Some(format!(
            r#"printf '%s|%s|%s|%s\n' "$ORION_ACCOUNT" "$ORION_MAILBOX" "$ORION_COUNT" "$ORION_SUBJECT" >> '{}'"#,
            hook_out.display()
        ));
        cfg.accounts = vec![AccountConfig::new(
            "fake",
            AccountKind::Imap(ImapConfig {
                host: "127.0.0.1".into(),
                port: Some(fake.port),
                tls: TlsMode::None,
                user: "yo".into(),
                password: Some("secreta".into()),
                password_command: None,
                poll_interval: 3600,
                cache_bodies: false,
                idle: false,
                oauth: None,
                account: String::new(),
            }),
        )];

        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (cfg, socket, stop) = (cfg.clone(), socket.clone(), stop.clone());
            let cache = dir.path().join("cache.sqlite");
            std::thread::spawn(move || crate::daemon::run(cfg, Some(cache), socket, stop))
        };

        // Initial sync: counts available, nothing notified (it is the baseline).
        let status = wait_for("initial status", || {
            let s = Client::connect(&socket).ok()?.status().ok()?;
            let acc = &s.accounts[0];
            (acc.last_sync.is_some() && !acc.mailboxes.is_empty()).then_some(s)
        });
        let inbox = status.accounts[0].mailboxes.iter().find(|m| m.name == "INBOX").unwrap();
        assert_eq!((inbox.total, inbox.unread, inbox.watched), (3, 2, true));
        assert_eq!(status.unread, 2, "only watched mailboxes count");
        assert!(status.accounts[0].mailboxes.iter().filter(|m| m.name != "INBOX").all(|m| !m.watched));
        assert!(!hook_out.exists(), "the first sync must not notify");

        // A second daemon on the same socket is refused.
        let err = crate::daemon::run(cfg.clone(), None, socket.clone(), Arc::new(AtomicBool::new(true))).unwrap_err();
        assert!(err.to_string().contains("already running"), "{err}");

        // Subscriber.
        let (ev_tx, ev_rx) = mpsc::channel();
        {
            let client = Client::connect(&socket).unwrap();
            std::thread::spawn(move || {
                let _ = client.subscribe(|_, ev| {
                    let _ = ev_tx.send(ev);
                    Ok(())
                });
            });
        }
        std::thread::sleep(Duration::from_millis(200));

        // New mail on the server + "sync" through the API.
        fake.admin("ADD INBOX Invoice $(touch /tmp/orion-pwned) `id`");
        Client::connect(&socket).unwrap().request(&Command::Sync).unwrap();

        let mut got_new = None;
        let mut got_counts = None;
        let deadline = Instant::now() + Duration::from_secs(15);
        while (got_new.is_none() || got_counts.is_none()) && Instant::now() < deadline {
            match ev_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(ev @ DaemonEvent::NewMail { .. }) => got_new = Some(ev),
                Ok(ev @ DaemonEvent::Counts { .. }) => got_counts = Some(ev),
                _ => {}
            }
        }
        match got_new.expect("new_mail event") {
            DaemonEvent::NewMail { account, mailbox, messages } => {
                assert_eq!((account.as_str(), mailbox.as_str(), messages.len()), ("fake", "INBOX", 1));
                assert_eq!(messages[0].subject, "Invoice $(touch /tmp/orion-pwned) `id`");
                assert_eq!(messages[0].from, "Ana");
            }
            _ => unreachable!(),
        }
        match got_counts.expect("counts event") {
            DaemonEvent::Counts { unread, .. } => assert_eq!(unread, 3),
            _ => unreachable!(),
        }

        // Hook: data intact and nothing executed.
        let line = wait_for("hook output", || std::fs::read_to_string(&hook_out).ok().filter(|s| s.ends_with('\n')));
        assert_eq!(line.trim(), "fake|INBOX|1|Invoice $(touch /tmp/orion-pwned) `id`");
        assert!(!std::path::Path::new("/tmp/orion-pwned").exists(), "the subject was executed!");

        // Status reflects the new count and lists the unread messages,
        // newest first, with their mailbox (what a status bar's popover shows).
        let status = Client::connect(&socket).unwrap().status().unwrap();
        assert_eq!(status.unread, 3);
        assert_eq!(status.messages.len(), 3, "{:?}", status.messages);
        assert!(status.messages.iter().all(|m| m.account == "fake" && m.mailbox == "INBOX"));
        assert!(status.messages.iter().any(|m| m.subject == "Invoice $(touch /tmp/orion-pwned) `id`"));
        assert!(status.messages.windows(2).all(|w| w[0].timestamp >= w[1].timestamp), "newest first");

        // Another Orion (the UI) marks one as read in the shared cache and tells
        // the daemon: the count drops without a network sync.
        {
            let inbox_id = status.accounts[0].mailboxes.iter().find(|m| m.name == "INBOX").unwrap().id.clone();
            let mut store = orion_store::Store::open(&dir.path().join("cache.sqlite")).unwrap();
            let first = store.recent_unread("fake", &inbox_id, 1).unwrap()[0].id;
            store.set_flag("fake", &inbox_id, &[first], orion_core::model::Flag::Seen, true).unwrap();
        }
        Client::connect(&socket).unwrap().request(&Command::Reload).unwrap();
        let after = wait_for("count after reload", || {
            let s = Client::connect(&socket).ok()?.status().ok()?;
            (s.unread == 2).then_some(s)
        });
        assert_eq!(after.messages.len(), 2);
        let err = Client::connect(&socket).unwrap().request_raw("{\"cmd\":\"rm -rf\"}").unwrap();
        assert!(!err.ok && err.error.unwrap().contains("invalid request"));

        // Clean shutdown removes the socket.
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap().unwrap();
        assert!(!socket.exists());
    }
}

// ---------------------------------------------- daemon: IDLE + POP3 + JMAP

#[path = "../../orion-backends/tests/common/mod.rs"]
pub(crate) mod dovecot;

mod protocols {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    use orion_core::config::{AccountConfig, AccountKind, Config, JmapAuth, JmapConfig};

    use super::dovecot::{Dovecot, PASSWORD};
    use crate::client::Client;
    use crate::ipc::DaemonEvent;

    #[test]
    fn daemon_with_idle_pop3_and_jmap() {
        let Some(dv) = Dovecot::start() else {
            eprintln!("dovecot not installed: test skipped");
            return;
        };
        let Ok(mut jmap) = Command::new("python3")
            .arg("-I")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_jmap.py"))
            .stdout(Stdio::piped())
            .spawn()
        else {
            eprintln!("python3 not available: test skipped");
            return;
        };
        let mut line = String::new();
        BufReader::new(jmap.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let jmap_port: u16 = line.split_whitespace().nth(1).unwrap().parse().unwrap();

        dv.deliver(None, "Old mail", "already there");
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("orion.sock");
        let mut cfg = Config::default();
        cfg.daemon.notify = false;
        let mut imap = dv.imap_config(PASSWORD);
        imap.poll_interval = 3600; // only IDLE can deliver the new mail in time
        let mut pop3 = dv.pop3_config(PASSWORD);
        pop3.poll_interval = 3600;
        cfg.accounts = vec![
            AccountConfig::new("imap", AccountKind::Imap(imap)),
            AccountConfig::new("pop3", AccountKind::Pop3(pop3)),
            AccountConfig::new(
                "jmap",
                AccountKind::Jmap(JmapConfig {
                    url: format!("http://127.0.0.1:{jmap_port}"),
                    user: Some("ana".into()),
                    password: Some("s3cret".into()),
                    password_command: None,
                    auth: JmapAuth::Basic,
                    poll_interval: 3600,
                    cache_bodies: true,
                    account: String::new(),
                }),
            ),
        ];
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (cfg, socket, stop) = (cfg.clone(), socket.clone(), stop.clone());
            let cache = dir.path().join("cache.sqlite");
            std::thread::spawn(move || crate::daemon::run(cfg, Some(cache), socket, stop))
        };

        // All three accounts report counts.
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            let s = Client::connect(&socket).ok().and_then(|mut c| c.status().ok());
            if let Some(s) = s.filter(|s| s.accounts.iter().all(|a| a.last_sync.is_some())) {
                break s;
            }
            assert!(Instant::now() < deadline, "accounts did not sync");
            std::thread::sleep(Duration::from_millis(100));
        };
        let inbox = |name: &str| status.accounts.iter().find(|a| a.name == name).unwrap().mailboxes[0].clone();
        assert_eq!((inbox("imap").total, inbox("imap").unread), (1, 1));
        assert_eq!((inbox("pop3").total, inbox("pop3").unread), (1, 1), "POP3 unread computed from the cache");
        assert_eq!((inbox("jmap").name.as_str(), inbox("jmap").total, inbox("jmap").unread), ("Inbox", 5, 4));
        assert_eq!(status.unread, 6);

        // New mail lands on the IMAP server: IDLE must notice without any poll or sync.
        let (tx, rx) = mpsc::channel();
        let client = Client::connect(&socket).unwrap();
        std::thread::spawn(move || {
            let _ = client.subscribe(|_, ev| {
                let _ = tx.send(ev);
                Ok(())
            });
        });
        std::thread::sleep(Duration::from_millis(500));
        let t = Instant::now();
        dv.deliver(None, "Pushed by IDLE", "instant");
        let ev = loop {
            match rx.recv_timeout(Duration::from_secs(15)).expect("no new_mail event: IDLE did not fire") {
                ev @ DaemonEvent::NewMail { .. } => break ev,
                _ => continue,
            }
        };
        let latency = t.elapsed();
        match ev {
            DaemonEvent::NewMail { account, messages, .. } => {
                assert_eq!(account, "imap");
                assert_eq!(messages[0].subject, "Pushed by IDLE");
            }
            _ => unreachable!(),
        }
        eprintln!("new mail notified through IDLE after {latency:?}");
        assert!(latency < Duration::from_secs(10));

        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap().unwrap();
        let _ = jmap.kill();
    }
}

// --------------------------------------- account wizard against real Dovecot

mod wizard {
    use super::dovecot::{Dovecot, PASSWORD};
    use crate::setup::{self, tests::Script};
    use orion_backends::autoconfig::Discovered;
    use orion_backends::secrets::{self, Kind};
    use orion_core::config::Config;

    /// Removes the keyring entry even if the test fails half-way.
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = secrets::clear(&self.0, Kind::Password);
        }
    }

    #[test]
    fn wizard_adds_own_server_and_account_test_works() {
        let Some(dv) = Dovecot::start() else {
            eprintln!("dovecot not installed: test skipped");
            return;
        };
        if std::process::Command::new("secret-tool").arg("--version").output().is_err() || std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            eprintln!("no keyring available: test skipped");
            return;
        }
        let name = format!("orion-e2e-{}", std::process::id());
        let _cleanup = Cleanup(name.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut deps = setup::Deps::real();
        deps.discover = Box::new(|_, _| Discovered::default()); // nothing published for this test domain
        let port = dv.imap_port.to_string();
        let mut io = Script::new(&[
            "imap", "127.0.0.1", "none", "y", &port, "-", // manual servers (plain: local test server)
            &name, "ana",                                  // account name, username
            "not-the-password", PASSWORD,                  // first try fails for real, second works
        ]);
        let added = setup::add(&mut io, &deps, &path, &Config::default(), Some("ana@test.invalid".into())).unwrap();
        assert_eq!(added, name);
        let t = io.transcript();
        assert!(t.contains("Nothing found.") && t.contains("Try again.") && t.contains("✔ Connected: 1 mailbox(es), 0 unread."), "{t}");

        // The saved account works on its own: password read back from the keyring.
        let cfg = Config::load(Some(&path)).unwrap();
        assert!(cfg.accounts[0].smtp.is_none());
        dv.deliver(None, "Hello wizard", "x");
        let mut io = Script::new(&[]);
        setup::test(&mut io, &setup::Deps::real(), &cfg, Some(&name)).unwrap();
        assert!(io.transcript().contains(&format!("✔ {name}: 1 mailbox(es), 1 unread")), "{}", io.transcript());
    }
}

// ----------------------------------------- writing and sending, end to end

mod sending {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use orion_backends::imap::ImapBackend;
    use orion_core::config::{AccountConfig, AccountKind, Config, SmtpConfig, TlsMode};
    use orion_core::model::SigStatus;
    use orion_core::Backend;
    use orion_crypto::Gpg;

    use super::dovecot::{Dovecot, PASSWORD};
    use crate::app::{Account, App, Mode, PinPurpose};
    use crate::keys::Keymap;
    use crate::worker;

    struct Kill(Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }

    fn gpg_home() -> Option<(tempfile::TempDir, Gpg)> {
        Command::new("gpg").arg("--version").output().ok()?;
        let dir = tempfile::Builder::new().prefix("orion-send-gnupg-").tempdir().ok()?;
        std::fs::set_permissions(dir.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).ok()?;
        let run = |args: &[&str]| {
            let out = Command::new("gpg").env("GNUPGHOME", dir.path()).args(["--batch", "--quiet", "--pinentry-mode", "loopback"]).args(args).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        for (uid, pass) in [("Ana <ana@example.org>", "1234"), ("Bob <bob@example.org>", "")] {
            run(&["--passphrase", pass, "--quick-gen-key", uid, "ed25519", "sign", "never"]);
            let fpr = run(&["--with-colons", "--list-keys", uid]);
            let fpr = fpr.lines().find(|l| l.starts_with("fpr:")).unwrap().split(':').nth(9).unwrap().to_string();
            run(&["--passphrase", pass, "--quick-add-key", &fpr, "cv25519", "encr", "never"]);
        }
        let _ = Command::new("gpgconf").env("GNUPGHOME", dir.path()).args(["--kill", "gpg-agent"]).status();
        let mut g = Gpg::new("gpg");
        g.home = Some(dir.path().to_path_buf());
        Some((dir, g))
    }

    #[test]
    fn reply_sign_send_and_keep_a_copy() {
        let Some(dv) = Dovecot::start() else {
            eprintln!("dovecot not installed: test skipped");
            return;
        };
        let Some((gpg_dir, gpg)) = gpg_home() else {
            eprintln!("gpg not available: test skipped");
            return;
        };
        let out = tempfile::tempdir().unwrap();
        let Ok(mut child) = Command::new("python3")
            .arg("-I")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_smtp.py"))
            .arg(out.path())
            .args(["--password", PASSWORD])
            .stdout(Stdio::piped())
            .spawn()
        else {
            eprintln!("python3 not available: test skipped");
            return;
        };
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let _smtp = Kill(child);
        let smtp_port: u16 = line.split_whitespace().nth(1).unwrap().parse().unwrap();

        // Bob writes to Ana.
        std::fs::write(
            dv.maildir(None).join("new").join("lunch.eml"),
            "From: Bob <bob@example.org>\r\nTo: Ana <ana@example.org>\r\nSubject: Lunch?\r\nMessage-ID: <lunch-1@example.org>\r\n\
             Date: Thu, 20 Nov 2025 12:00:00 +0100\r\n\r\nNoon at the usual place?\r\n",
        )
        .unwrap();

        let mut acc = AccountConfig::new("home", AccountKind::Imap(dv.imap_config(PASSWORD)));
        acc.from = Some("Ana <ana@example.org>".into());
        acc.smtp = Some(SmtpConfig { host: "127.0.0.1".into(), port: smtp_port, tls: TlsMode::None });
        let mut cfg = Config { accounts: vec![acc.clone()], ..Default::default() };
        // This test drives the $EDITOR flow; the built-in panel is tested at the end.
        cfg.ui.composer = orion_core::config::Composer::Editor;
        let (tx, rx) = mpsc::channel();
        let (keymap, _) = Keymap::new(&cfg.keys);
        let accounts = vec![Account {
            name: "home".into(),
            tx: worker::spawn(0, acc, None, Some(gpg.clone()), tx.clone()),
            mailboxes: Vec::new(),
            error: None,
            progress: String::new(),
        }];
        let mut app = App::new(cfg, keymap, None, tx, accounts);
        app.gpg = Some(gpg.clone());
        let until = |app: &mut App, what: &str, cond: &dyn Fn(&App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !cond(app) {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(ev) => app.on_event(ev),
                    Err(_) => panic!("timed out waiting for: {what} (status: {})", app.status),
                }
            }
        };
        let key = |app: &mut App, c: KeyCode| app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
        let typ = |app: &mut App, s: &str| s.chars().for_each(|c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));

        until(&mut app, "inbox", &|a| a.envelopes.len() == 1 && !a.loading);
        key(&mut app, KeyCode::Enter);
        until(&mut app, "message", &|a| a.reader.as_ref().is_some_and(|r| r.view.is_some()));

        // r → the editor gets a reply template.
        key(&mut app, KeyCode::Char('r'));
        let path = app.pending_editor.take().expect("the editor is requested");
        let template = std::fs::read_to_string(&path).unwrap();
        assert!(template.contains("From: Ana <ana@example.org>\nTo: Bob <bob@example.org>\n"), "{template}");
        assert!(template.contains("Subject: Re: Lunch?") && template.contains("> Noon at the usual place?"));
        let mode = std::fs::metadata(&path).unwrap();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777, 0o600, "drafts are private");
        // "The user writes" the answer.
        std::fs::write(&path, template.replacen("\n\n", "\n\nYes, see you there!\n", 1)).unwrap();
        app.after_editor(Ok(()));
        assert!(app.compose.as_ref().unwrap().error.is_none());

        // Sign (Ana's key has a passphrase) and send: the PIN is asked, then it goes out.
        key(&mut app, KeyCode::Char('S'));
        key(&mut app, KeyCode::Char('y'));
        until(&mut app, "PIN prompt for signing", &|a| matches!(a.mode, Mode::Pin { purpose: PinPurpose::Send, .. }));
        typ(&mut app, "1234");
        key(&mut app, KeyCode::Enter);
        until(&mut app, "sent", &|a| a.compose.is_none());
        assert!(app.status.starts_with("✉ sent: Re: Lunch?"), "{}", app.status);
        assert!(!path.exists(), "the draft is deleted once sent");

        // What reached the SMTP server: signed, threaded, verifiable.
        let files: Vec<_> = std::fs::read_dir(out.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let msg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
        assert_eq!(msg["from"], "ana@example.org");
        assert_eq!(msg["rcpts"], serde_json::json!(["bob@example.org"]));
        let data = msg["data"].as_str().unwrap();
        assert!(data.contains("multipart/signed") && data.contains("In-Reply-To: <lunch-1@example.org>"));
        let v = orion_crypto::open_message(Some(&gpg), data.as_bytes(), None).unwrap();
        assert_eq!(v.crypto.unwrap().signatures[0].status, SigStatus::Good);
        assert!(v.text.starts_with("Yes, see you there!"));

        // The copy is in Dovecot's Sent folder (created on the fly), marked read.
        let mut imap = ImapBackend::new(dv.imap_config(PASSWORD)).unwrap();
        let sent = imap.envelopes("Sent").unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].subject, "Re: Lunch?");
        assert!(sent[0].flags.seen);

        // New message, encrypted to someone without a key: named, then sent unencrypted.
        key(&mut app, KeyCode::Char('c'));
        let path = app.pending_editor.take().unwrap();
        let t = std::fs::read_to_string(&path).unwrap().replace("To: \n", "To: carol@example.org\n").replace("Subject: \n", "Subject: Hello Carol\n");
        std::fs::write(&path, t.replacen("\n\n", "\n\nHi!\n", 1)).unwrap();
        app.after_editor(Ok(()));
        key(&mut app, KeyCode::Char('E'));
        key(&mut app, KeyCode::Char('y'));
        until(&mut app, "missing key report", &|a| a.compose.as_ref().is_some_and(|c| !c.missing_keys.is_empty()));
        assert_eq!(app.compose.as_ref().unwrap().missing_keys, ["carol@example.org"]);
        key(&mut app, KeyCode::Char('E'));
        key(&mut app, KeyCode::Char('y'));
        until(&mut app, "sent", &|a| a.compose.is_none());
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 2);

        // A broken template is reported, not sent; `e` reopens the editor.
        key(&mut app, KeyCode::Char('c'));
        let path = app.pending_editor.take().unwrap();
        std::fs::write(&path, "From: Ana <ana@example.org>\nTo: not an address\n\nx").unwrap();
        app.after_editor(Ok(()));
        assert!(app.compose.as_ref().unwrap().error.as_ref().unwrap().contains("not a valid address"));
        key(&mut app, KeyCode::Char('y'));
        assert!(app.compose.as_ref().is_some_and(|c| !c.sending), "nothing is sent while the template is broken");
        key(&mut app, KeyCode::Char('e'));
        assert_eq!(app.pending_editor.as_deref(), Some(path.as_path()));
        let _ = std::fs::remove_file(&path);
        app.compose = None;
        app.pending_editor = None;

        // The built-in panel: address, subject, Vim body, Ctrl-S.
        app.cfg.ui.composer = orion_core::config::Composer::Builtin;
        key(&mut app, KeyCode::Char('c'));
        assert!(app.compose.as_ref().is_some_and(|c| c.panel.is_some()) && app.pending_editor.is_none());
        typ(&mut app, "carol@example.org");
        for _ in 0..4 {
            key(&mut app, KeyCode::Tab); // commit, then Cc, Bcc, Subject
        }
        typ(&mut app, "Via the panel");
        key(&mut app, KeyCode::Enter);
        typ(&mut app, "iHello from the panel");
        key(&mut app, KeyCode::Esc);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        until(&mut app, "sent from the panel", &|a| a.compose.is_none());
        let mut files: Vec<_> = std::fs::read_dir(out.path()).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(files.len(), 3);
        files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        let msg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(files.last().unwrap()).unwrap()).unwrap();
        assert_eq!(msg["rcpts"], serde_json::json!(["carol@example.org"]));
        let data = msg["data"].as_str().unwrap();
        assert!(data.contains("Subject: Via the panel") && data.contains("Hello from the panel"), "{data}");
        let _ = Command::new("gpgconf").env("GNUPGHOME", gpg_dir.path()).args(["--kill", "all"]).status();
    }
}

// ------------------------------------- delete / move against real Dovecot

mod removal {
    use super::dovecot::{Dovecot, PASSWORD};
    use super::Ui;
    use crate::app::Mode;
    use crossterm::event::KeyCode;
    use orion_backends::imap::ImapBackend;
    use orion_core::Backend;

    #[test]
    fn delete_move_and_delete_forever() {
        let Some(dv) = Dovecot::start() else {
            eprintln!("dovecot not installed: test skipped");
            return;
        };
        dv.create_folder("Trash");
        dv.create_folder("Archive");
        for s in ["Alpha", "Bravo", "Charlie", "Delta"] {
            dv.deliver(None, s, "body");
        }
        let dir = tempfile::tempdir().unwrap();
        let mut ui = Ui::with_imap(dv.imap_config(PASSWORD), &dir.path().join("cache.sqlite"));
        ui.until("synced", |a| a.envelopes.len() == 4 && !a.loading);
        let subjects = |b: &mut ImapBackend, mb: &str| -> Vec<String> {
            let mut v: Vec<String> = b.envelopes(mb).unwrap().into_iter().map(|e| e.subject).collect();
            v.sort();
            v
        };

        // d: to the Trash, gone from the list at once.
        ui.select_subject("Alpha");
        ui.key(KeyCode::Char('d'));
        assert_eq!(ui.app.envelopes.len(), 3, "removed from the list immediately");
        ui.until("trash done", |a| a.status.starts_with("moved to"));
        assert_eq!(ui.app.status, "moved to Trash");

        // m: choose the folder by typing part of its name.
        ui.select_subject("Bravo");
        ui.key(KeyCode::Char('m'));
        for c in "arch".chars() {
            ui.key(KeyCode::Char(c));
        }
        assert_eq!(ui.app.move_targets(&ui.app.current.clone().unwrap(), "arch")[0].1, "Archive");
        ui.key(KeyCode::Enter);
        ui.until("move done", |a| a.status.starts_with("moved to"));
        assert_eq!(ui.app.status, "moved to Archive");

        // D asks first; anything but y cancels.
        ui.select_subject("Charlie");
        ui.key(KeyCode::Char('D'));
        assert!(matches!(ui.app.mode, Mode::ConfirmDelete { .. }));
        ui.key(KeyCode::Char('n'));
        assert_eq!(ui.app.envelopes.len(), 2, "not deleted");
        ui.key(KeyCode::Char('D'));
        ui.key(KeyCode::Char('y'));
        ui.until("deleted", |a| a.status == "deleted for good");

        let mut b = ImapBackend::new(dv.imap_config(PASSWORD)).unwrap();
        assert_eq!(subjects(&mut b, "INBOX"), ["Delta"]);
        assert_eq!(subjects(&mut b, "Trash"), ["Alpha"]);
        assert_eq!(subjects(&mut b, "Archive"), ["Bravo"]);
        // The cache forgot them too (a restart shows the same list).
        let store = orion_store::Store::open(&dir.path().join("cache.sqlite")).unwrap();
        assert_eq!(store.envelopes("fake", "INBOX").unwrap().len(), 1);
        drop(store);

        // X only empties the Trash or Spam.
        ui.key(KeyCode::Char('X'));
        assert!(matches!(ui.app.mode, Mode::Normal));
        assert_eq!(ui.app.status, "only the Trash and Spam folders can be emptied");

        // In the Trash, d asks to delete for good; X empties it.
        dv.deliver(Some("Trash"), "Old one", "body");
        let trash = ui.app.sidebar.iter().position(|k| k.1 == "Trash").expect("Trash in the sidebar");
        ui.app.sidebar_sel = trash;
        ui.app.focus = crate::app::Focus::Sidebar;
        ui.key(KeyCode::Enter);
        ui.until("trash listed", |a| a.current.as_ref().is_some_and(|k| k.1 == "Trash") && a.envelopes.len() == 2 && !a.loading);
        ui.key(KeyCode::Char('d'));
        assert!(matches!(ui.app.mode, Mode::ConfirmDelete { .. }), "d in the Trash asks first");
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Char('X'));
        assert!(matches!(&ui.app.mode, Mode::ConfirmEmpty { prompt, .. } if prompt.contains("all 2 message")), "{:?}", ui.app.status);
        ui.key(KeyCode::Char('y'));
        assert!(ui.app.envelopes.is_empty());
        ui.until("emptied", |a| a.status == "folder emptied");
        assert!(subjects(&mut b, "Trash").is_empty(), "empty on the server");
        assert_eq!(subjects(&mut b, "INBOX"), ["Delta"], "nothing else touched");
    }
}

// --------------------------------------- conversations against real Dovecot

mod conversations {
    use super::dovecot::{Dovecot, PASSWORD};
    use super::Ui;
    use crossterm::event::KeyCode;

    #[test]
    fn replies_are_grouped_into_one_row() {
        let Some(dv) = Dovecot::start() else {
            eprintln!("dovecot not installed: test skipped");
            return;
        };
        let new = dv.maildir(None).join("new");
        let put = |name: &str, raw: &str| std::fs::write(new.join(name), raw).unwrap();
        put("1.a", "From: Ana <ana@x.org>\r\nSubject: Plan\r\nDate: Mon, 17 Nov 2025 10:00:00 +0000\r\nMessage-ID: <p1@x.org>\r\n\r\nfirst\r\n");
        put("2.b", "From: Luis <luis@x.org>\r\nSubject: Re: Plan\r\nDate: Tue, 18 Nov 2025 10:00:00 +0000\r\nMessage-ID: <p2@x.org>\r\nIn-Reply-To: <p1@x.org>\r\nReferences: <p1@x.org>\r\n\r\nsecond\r\n");
        put("3.c", "From: Bob <bob@x.org>\r\nSubject: Other\r\nDate: Wed, 19 Nov 2025 10:00:00 +0000\r\nMessage-ID: <o@x.org>\r\n\r\nother\r\n");
        let dir = tempfile::tempdir().unwrap();
        let mut ui = Ui::with_imap(dv.imap_config(PASSWORD), &dir.path().join("cache.sqlite"));
        ui.until("synced", |a| a.envelopes.len() == 3 && !a.loading);
        assert_eq!(ui.app.visible.len(), 2, "two rows: the conversation and the other message");
        let conv = ui.app.rows.iter().position(|r| r.members.len() == 2).expect("a two-message conversation");
        let head = ui.app.visible[conv];
        assert_eq!(ui.app.envelopes[head].subject, "Re: Plan", "shown by its newest message");
        ui.app.list_state.select(Some(conv));
        ui.key(KeyCode::Char('t'));
        assert_eq!(ui.app.visible.len(), 3, "expanded");
        assert!(ui.app.rows[conv + 1].child);
        ui.key(KeyCode::Char('t'));
        assert_eq!(ui.app.visible.len(), 2, "collapsed again");
        ui.key(KeyCode::Char('T'));
        assert_eq!(ui.app.visible.len(), 3, "grouping off: one row per message");
        // The ids came through the cache too (a restart groups the same way).
        let store = orion_store::Store::open(&dir.path().join("cache.sqlite")).unwrap();
        let envs = store.envelopes("fake", "INBOX").unwrap();
        assert!(envs.iter().any(|e| e.message_id == "p2@x.org" && e.parents == ["p1@x.org"]));
    }
}
