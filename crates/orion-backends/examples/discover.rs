//! cargo run -p orion-backends --example discover -- <email>
fn main() {
    let email = std::env::args().nth(1).expect("email address");
    let domain = email.rsplit('@').next().unwrap();
    for name in ["_imaps", "_imap", "_submissions", "_submission"] {
        println!("{name}._tcp.{domain}: {:?}", orion_backends::autoconfig::srv_lookup(&format!("{name}._tcp.{domain}")));
    }
    println!("{:#?}", orion_backends::autoconfig::discover(domain, &email));
}
