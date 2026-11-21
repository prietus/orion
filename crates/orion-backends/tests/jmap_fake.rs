//! JMAP backend against `scripts/fake_jmap.py` (skipped without python3).

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use orion_backends::jmap::JmapBackend;
use orion_core::config::{JmapAuth, JmapConfig};
use orion_core::model::{Flag, MessageId, SyncResult};
use orion_core::{Backend, Error};

struct Fake {
    child: Child,
    port: u16,
}

impl Fake {
    fn start() -> Option<Fake> {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_jmap.py");
        let mut child = Command::new("python3").arg("-I").arg(script).stdout(Stdio::piped()).spawn().ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        Some(Fake { port: line.split_whitespace().nth(1)?.parse().ok()?, child })
    }

    fn admin(&self, body: serde_json::Value) -> serde_json::Value {
        let mut res = ureq::post(&format!("http://127.0.0.1:{}/admin", self.port)).send_json(&body).unwrap();
        res.body_mut().read_json().unwrap()
    }

    fn cfg(&self, auth: JmapAuth, secret: &str) -> JmapConfig {
        JmapConfig {
            // Base URL only: the client must discover /.well-known/jmap and follow its redirect.
            url: format!("http://127.0.0.1:{}", self.port),
            user: Some("ana".into()),
            password: Some(secret.into()),
            password_command: None,
            auth,
            poll_interval: 60,
            cache_bodies: true,
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
fn jmap_full_flow() {
    let Some(fake) = Fake::start() else {
        eprintln!("python3 not available: test skipped");
        return;
    };
    let mut b = JmapBackend::new(fake.cfg(JmapAuth::Basic, "s3cret"));

    let mbs = b.mailboxes().unwrap();
    let names: Vec<&str> = mbs.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Inbox", "Drafts", "Sent", "Lists", "Lists/rust"], "role order, then nested path");
    assert_eq!((mbs[0].total, mbs[0].unread), (5, 4));
    let inbox = mbs[0].id.clone();

    // First sync: 5 ids over 3 query pages, headers in Email/get chunks of 3.
    let mut progress = Vec::new();
    let r = b.sync(&inbox, None, &HashSet::new(), &mut |e| if let orion_core::model::SyncEvent::Progress { done, total } = e { progress.push((done, total)) }).unwrap();
    let SyncResult::Delta { added, existing, .. } = r else { panic!("JMAP returns deltas") };
    assert_eq!(added.len(), 5);
    assert!(existing.is_empty());
    assert_eq!(progress.last(), Some(&(5, 5)));
    let first = added.iter().find(|e| e.subject == "Inbox message 1").unwrap();
    assert!(first.flags.seen);
    assert_eq!((first.from_name.as_str(), first.from_addr.as_str()), ("Ana", "ana@example.org"));
    assert_eq!(first.date, "2025-11-20 10:01", "sentAt keeps the sender's local time");
    let known: HashSet<MessageId> = added.iter().map(|e| e.id).collect();

    // Full message through the (relative, templated) download URL; the id has '/' in it.
    let m3 = added.iter().find(|e| e.subject == "Inbox message 3").unwrap().id;
    let raw = String::from_utf8(b.fetch_raw(&inbox, m3).unwrap()).unwrap();
    assert!(raw.contains("Body of Inbox message 3"));

    // Flags go to the server as keywords.
    b.set_flag(&inbox, &[m3], Flag::Seen, true).unwrap();
    b.set_flag(&inbox, &[m3], Flag::Flagged, true).unwrap();
    let jid = fake.admin(serde_json::json!({"op": "find", "subject": "Inbox message 3"}))["id"].as_str().unwrap().to_string();
    let kw = fake.admin(serde_json::json!({"op": "keywords", "id": jid}));
    assert_eq!(kw["keywords"], serde_json::json!({"$seen": true, "$flagged": true}));
    b.set_flag(&inbox, &[m3], Flag::Flagged, false).unwrap();
    assert!(matches!(b.set_flag(&inbox, &[m3], Flag::Deleted, true), Err(Error::Unsupported(_))));

    // Incremental: new message headers only; deleted ones vanish; known flags refreshed.
    fake.admin(serde_json::json!({"op": "add", "mailbox": "mb-inbox", "subject": "Fresh"}));
    let gone = fake.admin(serde_json::json!({"op": "find", "subject": "Inbox message 5"}))["id"].as_str().unwrap().to_string();
    fake.admin(serde_json::json!({"op": "delete", "id": gone}));
    let before = fake.admin(serde_json::json!({"op": "stats"}))["Email/get ids"].as_u64().unwrap();
    let r = b.sync(&inbox, None, &known, &mut |_| {}).unwrap();
    let SyncResult::Delta { added, existing, .. } = r else { panic!() };
    assert_eq!(added.iter().map(|e| e.subject.as_str()).collect::<Vec<_>>(), ["Fresh"]);
    assert_eq!(existing.len(), 4);
    let f3 = existing.iter().find(|(i, _)| *i == m3).unwrap().1.unwrap();
    assert!(f3.seen && !f3.flagged);
    let after = fake.admin(serde_json::json!({"op": "stats"}))["Email/get ids"].as_u64().unwrap();
    assert_eq!(after - before, 5, "1 full header + 4 keyword-only gets");

    // Nested mailbox.
    let rust = mbs.iter().find(|m| m.name == "Lists/rust").unwrap().id.clone();
    assert_eq!(b.envelopes(&rust).unwrap()[0].subject, "Rust 2.0 released");

    // Bearer tokens work; wrong credentials are an auth error.
    let mut bearer = JmapBackend::new(fake.cfg(JmapAuth::Bearer, "tok-123"));
    assert_eq!(bearer.mailboxes().unwrap().len(), 5);
    let mut bad = JmapBackend::new(fake.cfg(JmapAuth::Basic, "nope"));
    assert!(matches!(bad.mailboxes(), Err(Error::Auth(_))), "{:?}", bad.mailboxes());
}

#[test]
fn jmap_submission() {
    let Some(fake) = Fake::start() else {
        eprintln!("python3 not available: test skipped");
        return;
    };
    let mut b = JmapBackend::new(fake.cfg(JmapAuth::Basic, "s3cret"));
    let raw = b"From: Ana <ana@example.org>\r\nTo: bob@example.org\r\nSubject: Sent over JMAP\r\n\r\nhello\r\n";
    let rcpts = vec!["bob@example.org".to_string(), "bcc@example.org".to_string()];
    assert!(b.submit(raw, "ana@example.org", &rcpts).unwrap(), "JMAP sends by itself");

    let state = fake.admin(serde_json::json!({"op": "submissions"}));
    let subs = state["submissions"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["identityId"], "id-ana");
    assert_eq!(subs[0]["envelope"]["rcptTo"], serde_json::json!([{"email": "bob@example.org"}, {"email": "bcc@example.org"}]));
    let email = &state["emails"][subs[0]["emailId"].as_str().unwrap()];
    assert_eq!(email["subject"], "Sent over JMAP");
    assert_eq!(email["mailboxIds"], serde_json::json!({"mb-sent": true}), "moved from Drafts to Sent");
    assert_eq!(email["keywords"], serde_json::json!({"$seen": true}), "$draft cleared");

    let e = b.submit(raw, "ana@example.org", &["reject@example.org".to_string()]).unwrap_err();
    assert!(e.to_string().contains("forbiddenToSend"), "{e}");
}
