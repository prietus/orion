//! cargo run -p orion-crypto --example locate -- <gnupghome> <addr>...
fn main() {
    let mut args = std::env::args().skip(1);
    let mut g = orion_crypto::Gpg::new("gpg");
    g.home = Some(args.next().expect("home").into());
    let addrs: Vec<String> = args.collect();
    let found = g.locate_keys(&addrs, &["hkps://keys.openpgp.org".to_string()]);
    for a in &addrs {
        match g.encryption_key(a) {
            Some(k) if found.contains(a) => println!("{a}: {} certified={} ({})", orion_crypto::gpg::pretty_fingerprint(&k.fingerprint), k.certified(), k.uid),
            _ => println!("{a}: not found"),
        }
    }
}
