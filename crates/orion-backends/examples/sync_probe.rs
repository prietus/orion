//! Diagnostic: runs the real IMAP sync of one mailbox for a configured account
//! and prints timings, counts and errors only (no message content).
//! cargo run --release -p orion-backends --example sync_probe -- <account> [mailbox]
use std::collections::HashSet;
use std::time::Instant;

use orion_core::config::{AccountKind, Config};
use orion_core::model::SyncResult;
use orion_core::Backend;

fn main() {
    let name = std::env::args().nth(1).expect("account name");
    let mailbox = std::env::args().nth(2).unwrap_or_else(|| "INBOX".into());
    let cfg = Config::load(None).expect("config");
    let acc = cfg.accounts.iter().find(|a| a.name == name).expect("account");
    let AccountKind::Imap(c) = &acc.kind else { panic!("IMAP account expected") };
    let mut b = orion_backends::imap::ImapBackend::new(c.clone()).unwrap();
    let t = Instant::now();
    let mut last = Instant::now();
    let r = b.sync(&mailbox, None, &HashSet::new(), &mut |e| {
        let orion_core::model::SyncEvent::Progress { done, total } = e else { return };
        eprintln!("{:>6.1}s  {done}/{total}  (+{:.1}s)", t.elapsed().as_secs_f32(), last.elapsed().as_secs_f32());
        last = Instant::now();
    });
    match r {
        Ok(SyncResult::Full { envelopes, .. }) => println!("OK: {} envelopes in {:.1}s", envelopes.len(), t.elapsed().as_secs_f32()),
        Ok(SyncResult::Delta { added, .. }) => println!("OK (delta): {} added", added.len()),
        Err(e) => println!("ERROR after {:.1}s: {e:?}", t.elapsed().as_secs_f32()),
    }
}
