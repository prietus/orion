//! cargo run --release -p orion-backends --example index_bench -- <mbox>
use orion_core::Backend;
use orion_core::model::MessageId;

fn main() {
    let path = std::env::args().nth(1).expect("path to an mbox");
    let mut b = orion_backends::mbox::MboxBackend::new(path.into());
    let t = std::time::Instant::now();
    let mbs = b.mailboxes().unwrap();
    let t_index = t.elapsed();
    let t = std::time::Instant::now();
    let envs = b.envelopes(&mbs[0].id).unwrap();
    let t_cached = t.elapsed();
    let t = std::time::Instant::now();
    let raw = b.fetch_raw(&mbs[0].id, MessageId(envs.len() as u64 / 2)).unwrap();
    let view = orion_core::parse::message_view(&raw).unwrap();
    let t_open = t.elapsed();
    println!("{} messages ({} unread)", mbs[0].total, mbs[0].unread);
    println!("initial index: {t_index:?}  ·  cached list: {t_cached:?}  ·  open message: {t_open:?}");
    println!("sample: {:?} / {}", envs[7].subject, view.subject);
}
