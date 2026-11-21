//! Integration tests against a real Dovecot (skipped if it is not installed).

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use orion_backends::imap::{IdleEvent, ImapBackend};
use orion_backends::pop3::Pop3Backend;
use orion_core::model::{Flag, MessageId, SyncResult};
use orion_core::{Backend, Error};

use common::{Dovecot, PASSWORD};

fn ids(r: &SyncResult) -> (Vec<String>, Vec<MessageId>) {
    match r {
        SyncResult::Delta { added, existing, .. } => (added.iter().map(|e| e.subject.clone()).collect(), existing.iter().map(|(i, _)| *i).collect()),
        SyncResult::Full { envelopes, .. } => (envelopes.iter().map(|e| e.subject.clone()).collect(), Vec::new()),
    }
}

#[test]
fn pop3_against_dovecot() {
    let Some(dv) = Dovecot::start() else {
        eprintln!("dovecot not installed: test skipped");
        return;
    };
    dv.deliver(None, "First", "one");
    let second = dv.deliver(None, "Second", "two\r\n.dot-stuffed line\r\n..");
    dv.deliver(None, "Third", "three");

    let mut b = Pop3Backend::new(dv.pop3_config(PASSWORD));
    let mbs = b.mailboxes().unwrap();
    assert_eq!((mbs.len(), mbs[0].id.as_str(), mbs[0].total), (1, "INBOX", 3));

    let r = b.sync("INBOX", None, &HashSet::new(), &mut |_| {}).unwrap();
    let SyncResult::Delta { added, .. } = &r else { panic!("POP3 always returns a delta") };
    let mut subjects: Vec<_> = added.iter().map(|e| e.subject.as_str()).collect();
    subjects.sort();
    assert_eq!(subjects, ["First", "Second", "Third"]);
    assert!(added.iter().all(|e| !e.flags.seen && e.size > 0));
    let known: HashSet<MessageId> = added.iter().map(|e| e.id).collect();

    // Full body, with POP3 dot-unstuffing undone.
    let id2 = added.iter().find(|e| e.subject == "Second").unwrap().id;
    let raw = String::from_utf8(b.fetch_raw("INBOX", id2).unwrap()).unwrap();
    assert!(raw.contains("\r\n.dot-stuffed line\r\n..\r\n"), "{raw:?}");

    // Incremental: only the new header is downloaded, deletions are noticed.
    dv.deliver(None, "Fourth", "four");
    dv.remove(None, &second);
    let r = b.sync("INBOX", None, &known, &mut |_| {}).unwrap();
    let (added, existing) = ids(&r);
    assert_eq!(added, ["Fourth"]);
    assert_eq!(existing.len(), 2, "Second was removed");
    assert!(!existing.contains(&id2));
    match &r {
        SyncResult::Delta { existing, .. } => assert!(existing.iter().all(|(_, f)| f.is_none()), "POP3 never overrides local flags"),
        _ => unreachable!(),
    }

    // Flags are local (no error, nothing on the server).
    b.set_flag("INBOX", &[id2], Flag::Seen, true).unwrap();
    assert!(b.local_flags());

    let mut bad = Pop3Backend::new(dv.pop3_config("wrong"));
    assert!(matches!(bad.mailboxes(), Err(Error::Auth(_))), "{:?}", bad.mailboxes());
}

#[test]
fn imap_against_dovecot() {
    let Some(dv) = Dovecot::start() else {
        eprintln!("dovecot not installed: test skipped");
        return;
    };
    dv.create_folder("Lists.rust");
    dv.create_folder("Env&AO0-o");
    dv.create_folder("Trash");
    dv.deliver(None, "Hello", "body");
    dv.deliver(None, "Héllo ñ", "body");
    dv.deliver(Some("Lists.rust"), "Rust 2.0 released", "list mail");

    let mut b = ImapBackend::new(dv.imap_config(PASSWORD)).unwrap();
    let mbs = b.mailboxes().unwrap();
    let names: Vec<&str> = mbs.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names[0], "INBOX");
    assert!(names.contains(&"Lists/rust"), "hierarchy delimiter shown as '/': {names:?}");
    assert!(names.contains(&"Envío"), "modified UTF-7 decoded: {names:?}");
    let inbox = &mbs[0];
    assert_eq!((inbox.total, inbox.unread), (2, 2));

    let r = b.sync("INBOX", None, &HashSet::new(), &mut |_| {}).unwrap();
    let SyncResult::Full { envelopes, state } = r else { panic!("first sync is full") };
    let state = state.unwrap();
    assert_eq!(envelopes.len(), 2);
    assert!(envelopes.iter().any(|e| e.subject == "Héllo ñ"));

    // Flags go to the server and come back on the next delta.
    let id = envelopes[0].id;
    b.set_flag("INBOX", &[id], Flag::Seen, true).unwrap();
    b.set_flag("INBOX", &[id], Flag::Flagged, true).unwrap();
    dv.deliver(None, "Late arrival", "x");
    let r = b.sync("INBOX", Some(&state), &HashSet::new(), &mut |_| {}).unwrap();
    let SyncResult::Delta { added, existing, .. } = r else { panic!("expected delta") };
    assert_eq!(added.iter().map(|e| e.subject.as_str()).collect::<Vec<_>>(), ["Late arrival"]);
    let f = existing.iter().find(|(i, _)| *i == id).unwrap().1.unwrap();
    assert!(f.seen && f.flagged);

    let other = mbs.iter().find(|m| m.name == "Lists/rust").unwrap();
    let r = b.sync(&other.id, None, &HashSet::new(), &mut |_| {}).unwrap();
    assert_eq!(ids(&r).0, ["Rust 2.0 released"]);
    let raw = b.fetch_raw("INBOX", id).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("body"));
}

#[test]
fn imap_idle_against_dovecot() {
    let Some(dv) = Dovecot::start() else {
        eprintln!("dovecot not installed: test skipped");
        return;
    };
    let mut b = ImapBackend::new(dv.imap_config(PASSWORD)).unwrap();

    // Nothing happens: timeout.
    let t = Instant::now();
    assert_eq!(b.idle_wait("INBOX", Duration::from_millis(800)).unwrap(), IdleEvent::Timeout);
    assert!(t.elapsed() >= Duration::from_millis(700));

    // A delivery wakes it up long before the timeout.
    let delivery = {
        let path = dv.maildir(None);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            let tmp = path.join("tmp/idle.msg");
            std::fs::write(&tmp, "From: x@y\r\nSubject: wake up\r\n\r\nhi\r\n").unwrap();
            std::fs::rename(&tmp, path.join("new/idle.msg")).unwrap();
        })
    };
    let t = Instant::now();
    assert_eq!(b.idle_wait("INBOX", Duration::from_secs(60)).unwrap(), IdleEvent::Changed);
    let waited = t.elapsed();
    delivery.join().unwrap();
    assert!(waited < Duration::from_secs(10), "IDLE took {waited:?}");
    eprintln!("IDLE noticed the new message after {waited:?}");

    // The same connection keeps working after IDLE.
    let mbs = b.mailboxes().unwrap();
    assert_eq!(mbs[0].total, 1);
}

#[test]
fn imap_sent_copy_against_dovecot() {
    let Some(dv) = Dovecot::start() else {
        eprintln!("dovecot not installed: test skipped");
        return;
    };
    let mut b = ImapBackend::new(dv.imap_config(PASSWORD)).unwrap();
    // No Sent folder yet: Orion creates it.
    let sent = b.sent_mailbox().unwrap().unwrap();
    assert_eq!(sent, "Sent");
    assert!(b.mailboxes().unwrap().iter().any(|m| m.name == "Sent"));
    b.append(&sent, b"From: ana@x.org\r\nTo: bob@y.org\r\nSubject: My copy\r\n\r\nsent text\r\n", true).unwrap();
    let envs = b.envelopes(&sent).unwrap();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].subject, "My copy");
    assert!(envs[0].flags.seen, "the copy is stored as read");
    // Found again (not created twice) next time.
    assert_eq!(b.sent_mailbox().unwrap().unwrap(), "Sent");
}

#[test]
fn imap_delete_and_move_against_dovecot() {
    let Some(dv) = Dovecot::start() else {
        eprintln!("dovecot not installed: test skipped");
        return;
    };
    for s in ["Keep", "To trash", "To archive", "Gone"] {
        dv.deliver(None, s, "body");
    }
    // No Trash yet: nothing is created, `d` must refuse.
    assert_eq!(ImapBackend::new(dv.imap_config(PASSWORD)).unwrap().trash_mailbox().unwrap(), None);
    // (Folders made on disk are seen by sessions opened afterwards.)
    dv.create_folder("Trash");
    dv.create_folder("Archive");
    let mut b = ImapBackend::new(dv.imap_config(PASSWORD)).unwrap();
    assert_eq!(b.trash_mailbox().unwrap().as_deref(), Some("Trash"));

    let by_subject = |b: &mut ImapBackend, mb: &str| -> Vec<(String, MessageId)> {
        let mut v: Vec<_> = b.envelopes(mb).unwrap().into_iter().map(|e| (e.subject, e.id)).collect();
        v.sort();
        v
    };
    let inbox = by_subject(&mut b, "INBOX");
    let id = |s: &str| inbox.iter().find(|(x, _)| x == s).unwrap().1;

    b.move_messages("INBOX", &[id("To trash")], "Trash").unwrap();
    b.move_messages("INBOX", &[id("To archive")], "Archive").unwrap();
    b.delete_messages("INBOX", &[id("Gone")]).unwrap();

    let names = |v: Vec<(String, MessageId)>| v.into_iter().map(|(s, _)| s).collect::<Vec<_>>();
    assert_eq!(names(by_subject(&mut b, "INBOX")), ["Keep"], "only what stayed, nothing else expunged");
    assert_eq!(names(by_subject(&mut b, "Trash")), ["To trash"]);
    assert_eq!(names(by_subject(&mut b, "Archive")), ["To archive"]);
    // Deleting from the Trash is for good.
    let in_trash = by_subject(&mut b, "Trash")[0].1;
    b.delete_messages("Trash", &[in_trash]).unwrap();
    assert!(by_subject(&mut b, "Trash").is_empty());
}

#[test]
fn pop3_delete_against_dovecot() {
    let Some(dv) = Dovecot::start() else {
        eprintln!("dovecot not installed: test skipped");
        return;
    };
    dv.deliver(None, "Stays", "one");
    dv.deliver(None, "Deleted", "two");
    let mut b = Pop3Backend::new(dv.pop3_config(PASSWORD));
    let r = b.sync("INBOX", None, &HashSet::new(), &mut |_| {}).unwrap();
    let SyncResult::Delta { added, .. } = &r else { panic!() };
    let gone = added.iter().find(|e| e.subject == "Deleted").unwrap().id;
    assert_eq!(b.trash_mailbox().unwrap(), None, "POP3 has no Trash");
    b.delete_messages("INBOX", &[gone]).unwrap();
    // A new session sees only the other one: deleted on the server.
    let mut again = Pop3Backend::new(dv.pop3_config(PASSWORD));
    assert_eq!(again.mailboxes().unwrap()[0].total, 1);
}
