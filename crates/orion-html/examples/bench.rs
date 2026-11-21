//! cargo run --release -p orion-html --example bench
use std::time::Instant;
use orion_html::{Doc, LayoutOptions};

fn main() {
    let mut css = String::from("<style>");
    for i in 0..300 {
        css.push_str(&format!(".c{i} td.x{i} {{ color: #{:06x}; font-weight: bold }} #id{i} {{ text-align:center }}\n", i * 997));
    }
    css.push_str("</style>");
    let mut body = String::new();
    for i in 0..400 {
        body.push_str(&format!(
            r#"<table class="c{i}" width="100%"><tr><td width="20"></td><td class="x{i}"><table><tr><td><h3 id="id{i}">Article {i}</h3>
            <p>Body of article number {i} with <a href="https://example.com/{i}">a link</a> and enough <b>bold text</b> to wrap over several lines of content.</p>
            </td><td><img src="cid:img{i}" width="120"></td></tr></table></td></tr></table>"#
        ));
    }
    let html = format!("<html><head>{css}</head><body>{body}</body></html>");
    let t = Instant::now();
    let doc = Doc::parse(&html);
    let parse = t.elapsed();
    let px = |_: usize| Some((240u32, 160u32));
    let opts = LayoutOptions { image_px: &px, ..Default::default() };
    let t = Instant::now();
    let r = doc.layout(100, &opts);
    let lay = t.elapsed();
    println!("HTML {} KB · parse+CSS {parse:?} · layout {lay:?} · {} lines, {} images, {} links",
        html.len() / 1024, r.lines.len(), r.placements.len(), doc.links.len());
}
