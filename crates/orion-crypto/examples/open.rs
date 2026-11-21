//! Diagnostic: open a message file with the default gpg and print the crypto result.
//! Usage: cargo run -p orion-crypto --example open -- FILE
fn main() {
    let path = std::env::args().nth(1).expect("FILE");
    let raw = std::fs::read(path).expect("read");
    let gpg = orion_crypto::Gpg::new("gpg");
    let view = orion_crypto::open_message(Some(&gpg), &raw, None).expect("parse");
    match view.crypto {
        Some(c) => println!("{:#?}", c.problem),
        None => println!("not encrypted"),
    }
}
