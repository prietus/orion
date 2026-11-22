//! "Original" view: the message's HTML drawn by a real browser engine
//! (headless Chromium) into an image, shown with sixel/kitty.
//!
//! Locked down: `<script>` is removed and a Content-Security-Policy forbids
//! scripts and every network load (only `data:` images, inline styles and
//! fonts). Embedded (`cid:`) images and remote images the user already
//! loaded with `L` are inlined as `data:` URIs; nothing else is fetched.
//! Chromium also runs with a dead proxy and a throwaway profile.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use orion_core::model::Attachment;

/// CSS pixels of the viewport height: longer messages are cut there.
const VIEWPORT_HEIGHT: u32 = 6000;
const TIMEOUT: Duration = Duration::from_secs(40);

const CSP: &str = r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline' data:; font-src data:">"#;

pub struct Snapshot {
    pub png: Vec<u8>,
    pub size: (u32, u32),
}

/// The browser to use: the configured one, else the first Chromium found.
pub fn find_browser(configured: Option<&str>) -> Option<String> {
    let candidates: Vec<&str> = match configured {
        Some(c) => vec![c],
        None => vec!["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"],
    };
    candidates.into_iter().find(|c| which(c)).map(str::to_string)
}

fn which(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// The HTML as Chromium gets it: no scripts, the CSP first in `<head>`, and
/// the images we have (embedded or already downloaded) inlined.
#[cfg(test)]
pub fn prepare(html: &str, attachments: &[Attachment]) -> String {
    prepare_with(html, attachments, false)
}

/// Like `prepare`; `numbers` adds each link's number (as `o` uses it) as a
/// small badge, drawn by the browser where the link is.
pub fn prepare_with(html: &str, attachments: &[Attachment], numbers: bool) -> String {
    let html = if numbers { orion_html::number_links(html, &badge_style()) } else { html.to_string() };
    let mut out = strip_scripts(&html);
    for a in attachments.iter().filter(|a| a.is_image()) {
        let data = format!("data:{};base64,{}", a.mime, base64(&a.data));
        let mut targets = Vec::new();
        if let Some(cid) = &a.content_id {
            targets.push(format!("cid:{cid}"));
        }
        if a.name.starts_with("http://") || a.name.starts_with("https://") {
            targets.push(a.name.clone());
            targets.push(a.name.replace('&', "&amp;"));
        }
        for t in targets {
            out = out.replace(&format!("\"{t}\""), &format!("\"{data}\"")).replace(&format!("'{t}'"), &format!("'{data}'"));
        }
    }
    insert_csp(&out)
}

/// The link-number badge: the theme's accent, small, raised like a footnote.
fn badge_style() -> String {
    let hex = |c: ratatui::style::Color| match c {
        ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#cba6f7".into(),
    };
    let t = crate::theme::theme();
    format!(
        "display:inline-block;margin:0 0 0 3px;padding:0 5px;border-radius:7px;background:{};color:#1e1e2e;\
         font:bold 11px/15px sans-serif;letter-spacing:0;text-decoration:none;text-transform:none;vertical-align:super;\
         box-shadow:0 0 0 1px #1e1e2e",
        hex(t.accent)
    )
}

fn strip_scripts(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(start) = lower[i..].find("<script").map(|p| p + i) {
        out.push_str(&html[i..start]);
        i = match lower[start..].find("</script").map(|p| p + start) {
            Some(end) => lower[end..].find('>').map(|p| p + end + 1).unwrap_or(html.len()),
            None => html.len(),
        };
    }
    out.push_str(&html[i..]);
    out
}

/// The CSP must come before anything that loads: right after `<head …>`, or
/// in a `<head>` of its own after `<html …>`, or at the very start.
fn insert_csp(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let after = |tag: &str| lower.find(tag).and_then(|p| lower[p..].find('>').map(|q| p + q + 1));
    match after("<head") {
        Some(p) => format!("{}{CSP}{}", &html[..p], &html[p..]),
        None => match after("<html") {
            Some(p) => format!("{}<head>{CSP}</head>{}", &html[..p], &html[p..]),
            None => format!("<head>{CSP}</head>{html}"),
        },
    }
}

/// Renders the page `width` device pixels wide. Newsletters are designed for
/// ~600–800 CSS px, so the viewport stays there and the scale factor makes
/// up the difference (sharp text, not a tiny page in a sea of white).
pub fn render(browser: &str, html: &str, width: u32) -> Result<Snapshot, String> {
    // Beyond 3× the page gets wider instead (very wide panes).
    let css_width = width.clamp(480, 900).max(width.div_ceil(3));
    let scale = (width as f32 / css_width as f32).max(1.0);
    let dir = tempfile::Builder::new().prefix("orion-render-").tempdir().map_err(|e| e.to_string())?;
    let page = dir.path().join("message.html");
    let shot = dir.path().join("shot.png");
    std::fs::write(&page, html).map_err(|e| e.to_string())?;
    let mut child = Command::new(browser)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-sync",
            "--disable-background-networking",
            "--disable-component-update",
            "--hide-scrollbars",
            // No network even if something slipped past the CSP.
            "--proxy-server=127.0.0.1:9",
            "--proxy-bypass-list=<-loopback>",
            "--default-background-color=ffffffff",
        ])
        .arg(format!("--user-data-dir={}", dir.path().join("profile").display()))
        .arg(format!("--window-size={css_width},{VIEWPORT_HEIGHT}"))
        .arg(format!("--force-device-scale-factor={scale}"))
        .arg(format!("--screenshot={}", shot.display()))
        .arg(format!("file://{}", page.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{browser}: {e}"))?;
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{browser} took more than {}s", TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let log = reader.join().unwrap_or_default();
    let img = image::open(&shot).map_err(|_| {
        let last = log.lines().rfind(|l| !l.contains("dbus") && !l.trim().is_empty()).unwrap_or("no screenshot written").to_string();
        format!("{browser} failed: {last}")
    })?;
    let img = trim_bottom(img.to_rgb8());
    let size = (img.width(), img.height());
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(Snapshot { png, size })
}

/// The viewport is taller than most messages: drop the empty rows at the
/// bottom (the color of the last row), keeping a small margin.
fn trim_bottom(img: image::RgbImage) -> image::RgbImage {
    let (w, h) = img.dimensions();
    if h == 0 {
        return img;
    }
    let bg = *img.get_pixel(w / 2, h - 1);
    let row_empty = |y: u32| (0..w).all(|x| *img.get_pixel(x, y) == bg);
    let mut last = h - 1;
    while last > 0 && row_empty(last) {
        last -= 1;
    }
    let keep = (last + 24).min(h);
    image::imageops::crop_imm(&img, 0, 0, w, keep.max(1)).to_image()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(name: &str, cid: Option<&str>) -> Attachment {
        Attachment { name: name.into(), mime: "image/png".into(), inline: true, content_id: cid.map(Into::into), data: vec![1, 2, 3] }
    }

    #[test]
    fn locks_down_the_html() {
        let html = r#"<html><HEAD><title>x</title></head><body><SCRIPT>alert(1)</script><p>hi</p><script src="x.js"></script>
            <img src="cid:logo@x"><img src='https://cdn.example.com/a.png?x=1&amp;y=2'><img src="https://track.example.com/p.gif"></body></html>"#;
        let atts = [att("logo.png", Some("logo@x")), att("https://cdn.example.com/a.png?x=1&y=2", None)];
        let out = prepare(html, &atts);
        assert!(!out.to_ascii_lowercase().contains("<script"), "{out}");
        assert!(out.contains("<p>hi</p>"));
        assert!(out.find("Content-Security-Policy").unwrap() < out.find("<title>").unwrap(), "CSP first in head");
        assert_eq!(out.matches("data:image/png;base64,AQID").count(), 2, "cid and loaded remote image inlined");
        assert!(out.contains("https://track.example.com/p.gif"), "not loaded: left for the CSP to block");
        // Without <head> or <html>.
        assert!(prepare("<p>x</p>", &[]).starts_with("<head><meta http-equiv=\"Content-Security-Policy\""));
        assert!(prepare("<html lang=en><body>x", &[]).starts_with("<html lang=en><head><meta"));
    }

    #[test]
    fn link_numbers_match_the_text_view() {
        // Hidden links and other schemes are not numbered, as in the text view.
        let html = r#"<html><body><a href="https://one.example">One</a> <div style="display:none"><a href="https://hidden.example">x</a></div>
            <a href="javascript:alert(1)">bad</a> <a href="mailto:ana@example.org">Two</a> <a href="https://three.example"><img src="cid:x"></a></body></html>"#;
        let doc = orion_html::Doc::parse(html);
        assert_eq!(doc.links, ["https://one.example", "mailto:ana@example.org", "https://three.example"]);
        let out = prepare_with(html, &[], true);
        let badge = |n: usize| format!(">{n}</span></a>");
        assert!(out.contains("One<span style=") && out.contains(&badge(1)), "{out}");
        assert!(out.contains("Two<span") && out.contains(&badge(2)));
        assert!(out.contains(&badge(3)));
        assert!(!out.contains(">4</span>"), "only the three counted links");
        assert!(out.contains("Content-Security-Policy"), "still locked down");
        assert!(!prepare_with(html, &[], false).contains("<span style"), "off when link markers are off");
    }

    #[test]
    fn trims_empty_bottom() {
        let mut img = image::RgbImage::from_pixel(10, 1000, image::Rgb([255, 255, 255]));
        img.put_pixel(3, 100, image::Rgb([0, 0, 0]));
        assert_eq!(trim_bottom(img).height(), 124);
    }

    #[test]
    fn renders_with_chromium_offline() {
        let Some(browser) = find_browser(None) else {
            eprintln!("chromium not installed: test skipped");
            return;
        };
        // A local server that must never be contacted.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let html = format!(
            r#"<html><body style="margin:0;background:#fff"><div style="background:#76b900;height:300px">x</div>
               <img src="http://127.0.0.1:{port}/leak.png"><script>document.body.style.background='red'</script></body></html>"#
        );
        let snap = render(&browser, &prepare(&html, &[]), 800).unwrap();
        assert_eq!(snap.size.0, 800);
        // A very wide pane (ghostty, 150 columns × 21 px) is filled too.
        let wide = render(&browser, &prepare(&html, &[]), 3150).unwrap();
        assert!((3140..=3160).contains(&wide.size.0), "{:?}", wide.size);
        assert!(snap.size.1 >= 300 && snap.size.1 < 1000, "trimmed: {:?}", snap.size);
        let img = image::load_from_memory(&snap.png).unwrap().to_rgb8();
        assert_eq!(*img.get_pixel(10, 10), image::Rgb([0x76, 0xb9, 0x00]), "drawn by the engine");
        assert!(listener.accept().is_err(), "nothing may reach the network");
    }
}

/// Diagnostic: `ORION_SNAPSHOT_EML=msg.eml ORION_SNAPSHOT_OUT=out.png cargo test -p orion-tui snapshot_file -- --ignored`
#[cfg(test)]
#[test]
#[ignore]
fn snapshot_file() {
    let (Ok(eml), Ok(out)) = (std::env::var("ORION_SNAPSHOT_EML"), std::env::var("ORION_SNAPSHOT_OUT")) else { return };
    let raw = std::fs::read(eml).unwrap();
    let view = orion_core::parse::message_view(&raw).unwrap();
    let html = view.html.clone().expect("html part");
    let width: u32 = std::env::var("ORION_SNAPSHOT_WIDTH").ok().and_then(|w| w.parse().ok()).unwrap_or(1200);
    let snap = render(&find_browser(None).expect("chromium"), &prepare_with(&html, &view.attachments, true), width).unwrap();
    eprintln!("size {:?}", snap.size);
    std::fs::write(out, snap.png).unwrap();
}
