//! Diagnostic: render a message's HTML part as Orion would, as plain text.
//! Usage: cargo run -p orion-html --example render -- FILE.eml [WIDTH]
fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("FILE");
    let width: u16 = args.next().and_then(|w| w.parse().ok()).unwrap_or(100);
    let raw = std::fs::read(&path).expect("read");
    let msg = mail_parser::MessageParser::default().parse(&raw).expect("parse");
    let html = msg.body_html(0).expect("no html part");
    eprintln!("html: {} bytes", html.len());
    let doc = orion_html::Doc::parse(&html);
    let out = doc.layout(width, &Default::default());
    println!("{}", out.text());
}
