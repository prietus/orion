//! Remote image downloads against a local HTTP server.
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

fn serve() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut path = String::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            r.read_line(&mut path).unwrap();
            let mut referer = false;
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).unwrap() == 0 || l == "\r\n" {
                    break;
                }
                referer |= l.to_ascii_lowercase().starts_with("referer:") || l.to_ascii_lowercase().starts_with("cookie:");
            }
            let (ctype, body): (&str, Vec<u8>) = if referer {
                ("text/plain", b"leaked".to_vec())
            } else if path.contains("/logo.png") {
                ("image/png", b"\x89PNG\r\n\x1a\nfake-png-bytes".to_vec())
            } else {
                ("text/html", b"<html>not an image</html>".to_vec())
            };
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
    });
    (base, hits)
}

#[test]
fn downloads_only_images() {
    let (base, hits) = serve();
    let urls = vec![format!("{base}/logo.png"), format!("{base}/page.html")];
    let (ok, failed) = orion_backends::remote::fetch_images(&urls);
    assert_eq!(ok.len(), 1, "only the image");
    assert_eq!(failed, 1, "the HTML page is rejected");
    assert_eq!(ok[0].mime, "image/png");
    assert!(ok[0].data.starts_with(b"\x89PNG"));
    assert_eq!(ok[0].url, urls[0]);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}
