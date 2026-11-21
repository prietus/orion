//! Integration test of the IMAP backend against `scripts/fake_imap.py`.
//! Skipped when python3 is not available.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use orion_backends::imap::ImapBackend;
use orion_core::config::{ImapConfig, TlsMode};
use orion_core::model::{Flag, MessageId, SyncResult};
use orion_core::{Backend, Error};

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
        let parts: Vec<&str> = line.split_whitespace().collect();
        let port = parts[1].parse().ok()?;
        let admin = BufReader::new(TcpStream::connect(("127.0.0.1", parts[3].parse::<u16>().ok()?)).ok()?);
        Some(Fake { child, port, admin })
    }

    fn admin(&mut self, cmd: &str) -> String {
        writeln!(self.admin.get_mut(), "{cmd}").unwrap();
        let mut r = String::new();
        self.admin.read_line(&mut r).unwrap();
        r.trim().to_string()
    }

    fn cfg(&self, password: &str) -> ImapConfig {
        ImapConfig {
            host: "127.0.0.1".into(),
            port: Some(self.port),
            tls: TlsMode::None,
            user: "yo".into(),
            password: None,
            password_command: Some(format!("echo {password}")),
            poll_interval: 60,
            cache_bodies: true,
            idle: false,
            oauth: None,
            account: String::new(),
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn imap_full_flow() {
    let Some(mut fake) = Fake::start() else {
        eprintln!("python3 not available: test skipped");
        return;
    };
    let mut b = ImapBackend::new(fake.cfg("secreta")).unwrap();
    let none = std::collections::HashSet::new();

    // Mailboxes: INBOX first, then Sent (\Sent, UTF-7 name), \Noselect skipped.
    let mbs = b.mailboxes().unwrap();
    let names: Vec<&str> = mbs.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["INBOX", "Envíados", "Archivo", "Archivo/2024"]);
    assert_eq!((mbs[0].total, mbs[0].unread), (3, 2));
    assert_eq!(mbs[1].id, "Env&AO0-ados");

    // Full sync.
    let SyncResult::Full { envelopes, state } = b.sync("INBOX", None, &none, &mut |_| {}).unwrap() else { panic!("expected Full") };
    let state = state.unwrap();
    assert_eq!((state.validity, state.next), (42, 4));
    assert_eq!(envelopes.len(), 3);
    let e2 = envelopes.iter().find(|e| e.id == MessageId(2)).unwrap();
    assert_eq!(e2.subject, "Segundo con ñ");
    assert_eq!(e2.from_name, "José", "must decode RFC 2047");
    assert!(!e2.flags.seen);
    assert!(envelopes.iter().find(|e| e.id == MessageId(3)).unwrap().flags.flagged);

    // Full body without marking it as read.
    let raw = b.fetch_raw("INBOX", MessageId(2)).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Cuerpo del segundo"));

    // Flags to the server + changes on the server → delta.
    b.set_flag("INBOX", &[MessageId(2)], Flag::Seen, true).unwrap();
    fake.admin("ADD INBOX Nuevo mensaje");
    fake.admin("DEL INBOX 3");
    fake.admin("FLAG INBOX 1 \\Flagged");
    let SyncResult::Delta { added, existing, state: st2 } = b.sync("INBOX", Some(&state), &none, &mut |_| {}).unwrap() else {
        panic!("expected Delta")
    };
    assert_eq!(added.len(), 1);
    assert_eq!((added[0].id, added[0].subject.as_str()), (MessageId(4), "Nuevo mensaje"));
    let ids: Vec<u64> = existing.iter().map(|(i, _)| i.0).collect();
    assert_eq!(ids, [1, 2], "message 3 was deleted");
    assert!(existing[0].1.unwrap().flagged && existing[1].1.unwrap().seen);
    assert_eq!(st2.next, 5);

    // No changes: the delta does not download headers again.
    let before = fake.admin("COUNT UID FETCH");
    let SyncResult::Delta { added, .. } = b.sync("INBOX", Some(&st2), &none, &mut |_| {}).unwrap() else { panic!() };
    assert!(added.is_empty());
    let after = fake.admin("COUNT UID FETCH");
    assert_eq!(after.parse::<u32>().unwrap() - before.parse::<u32>().unwrap(), 1, "only the flags FETCH");

    // Dropped connection: reconnects and retries transparently.
    fake.admin("KICK");
    let raw = b.fetch_raw("INBOX", MessageId(1)).unwrap();
    assert!(!raw.is_empty());

    // Connection dropped right before a delta: must reconnect and return the right
    // delta, never an empty "existing" (which would wipe the cache).
    fake.admin("KICK");
    let SyncResult::Delta { added, existing, .. } = b.sync("INBOX", Some(&st2), &none, &mut |_| {}).unwrap() else { panic!() };
    assert!(added.is_empty());
    assert_eq!(existing.len(), 3);

    // UIDVALIDITY changes → full sync.
    fake.admin("VALIDITY INBOX 99");
    let mut progress = Vec::new();
    let r = b.sync("INBOX", Some(&st2), &none, &mut |e| if let orion_core::model::SyncEvent::Progress { done, total } = e { progress.push((done, total)) }).unwrap();
    let SyncResult::Full { envelopes, state } = r else { panic!("expected Full") };
    assert_eq!(envelopes.len(), 3);
    assert_eq!(state.unwrap().validity, 99);
    assert_eq!(progress.last(), Some(&(3, 3)));

    // Wrong password → authentication error.
    let mut bad = ImapBackend::new(fake.cfg("mala")).unwrap();
    assert!(matches!(bad.mailboxes(), Err(Error::Auth(_))));
}

#[test]
fn imap_throttling_errors_and_resume() {
    use orion_core::model::SyncEvent;
    let Some(mut fake) = Fake::start() else {
        eprintln!("python3 not available: test skipped");
        return;
    };
    let mut b = ImapBackend::new(fake.cfg("secreta")).unwrap();
    let none = std::collections::HashSet::new();

    // Throttled twice (Outlook-style NO): waited out, then the full sync completes,
    // handing out header batches as they arrive.
    fake.admin("THROTTLE 2");
    let t = std::time::Instant::now();
    let mut batches = Vec::new();
    let r = b
        .sync("INBOX", None, &none, &mut |e| {
            if let SyncEvent::Batch { validity, envelopes } = e {
                batches.push((validity, envelopes.len()));
            }
        })
        .unwrap();
    let SyncResult::Full { envelopes, state } = r else { panic!("expected Full") };
    assert_eq!(envelopes.len(), 3);
    assert_eq!(batches, [(42, 3)], "one batch for 3 messages, tagged with UIDVALIDITY");
    assert!(t.elapsed() >= std::time::Duration::from_millis(1100), "the suggested backoff was respected");
    let state = state.unwrap();

    // Any other NO is an error with the server's text — never a silently short result.
    fake.admin("FAILFETCH 1");
    let e = b.sync("INBOX", None, &none, &mut |_| {}).unwrap_err();
    assert!(e.to_string().contains("Internal server error"), "{e}");

    // Resuming an interrupted first sync (state next = 0): only the missing headers
    // are downloaded; the cached ones just get their flags.
    let known: std::collections::HashSet<MessageId> = [MessageId(1)].into_iter().collect();
    let before = fake.admin("COUNT UID FETCH").parse::<u32>().unwrap();
    let resume = orion_core::model::SyncState { validity: state.validity, next: 0 };
    let r = b.sync("INBOX", Some(&resume), &known, &mut |_| {}).unwrap();
    let SyncResult::Delta { added, existing, state: st } = r else { panic!("expected Delta") };
    let mut ids: Vec<u64> = added.iter().map(|e| e.id.0).collect();
    ids.sort();
    assert_eq!(ids, [2, 3]);
    assert_eq!(existing.iter().map(|(i, f)| (i.0, f.is_some())).collect::<Vec<_>>(), [(1, true)]);
    assert_eq!(st.next, 4, "resumed sync ends with a normal state");
    let after = fake.admin("COUNT UID FETCH").parse::<u32>().unwrap();
    assert_eq!(after - before, 2, "one cheap UID/FLAGS listing + one header fetch for the missing ones");
}
