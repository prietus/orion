//! Downloading a message's remote images, only when the user asks for it:
//! fetching them tells the sender the message was opened (and from where).

use std::time::Duration;

/// Hard limits, so a hostile message cannot make us download without end.
pub const MAX_IMAGES: usize = 60;
const MAX_BYTES_EACH: u64 = 5 * 1024 * 1024;
const MAX_BYTES_TOTAL: u64 = 40 * 1024 * 1024;

/// A downloaded image: the URL it came from, its MIME type and bytes.
pub struct Fetched {
    pub url: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// Whether an `<img>` is worth downloading: http(s), and not a tracking
/// pixel (declared 1–2 px wide or high).
pub fn wanted(src: &str, width: Option<u32>, height: Option<u32>) -> bool {
    let s = src.trim().to_ascii_lowercase();
    (s.starts_with("https://") || s.starts_with("http://")) && !width.is_some_and(|w| w <= 2) && !height.is_some_and(|h| h <= 2)
}

/// Downloads the images (a few at a time). No cookies, no Referer; only
/// `image/*` answers are kept. Returns what worked and how many failed.
pub fn fetch_images(urls: &[String]) -> (Vec<Fetched>, usize) {
    let mut urls: Vec<String> = urls.to_vec();
    urls.dedup();
    urls.truncate(MAX_IMAGES);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
        .user_agent("Mozilla/5.0")
        .max_redirects(5)
        .build()
        .into();
    let results: Vec<Option<Fetched>> = std::thread::scope(|s| {
        urls.chunks(urls.len().div_ceil(6).max(1))
            .map(|chunk| {
                let agent = agent.clone();
                s.spawn(move || chunk.iter().map(|u| fetch_one(&agent, u)).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut total = 0u64;
    let mut failed = 0;
    let mut ok = Vec::new();
    for r in results {
        match r {
            Some(f) if total + f.data.len() as u64 <= MAX_BYTES_TOTAL => {
                total += f.data.len() as u64;
                ok.push(f);
            }
            _ => failed += 1,
        }
    }
    (ok, failed)
}

fn fetch_one(agent: &ureq::Agent, url: &str) -> Option<Fetched> {
    let mut res = agent.get(url).header("Accept", "image/*").call().ok()?;
    let mime = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if !mime.starts_with("image/") {
        return None;
    }
    let data = res.body_mut().with_config().limit(MAX_BYTES_EACH).read_to_vec().ok()?;
    Some(Fetched { url: url.to_string(), mime, data })
}

#[cfg(test)]
mod tests {
    use super::wanted;

    #[test]
    fn which_images() {
        assert!(wanted("https://cdn.example.com/a.png", Some(600), None));
        assert!(!wanted("https://t.example.com/open.gif", Some(1), Some(1)), "tracking pixel");
        assert!(!wanted("cid:logo@x", None, None));
        assert!(!wanted("data:image/png;base64,AAA", None, None));
        assert!(!wanted("javascript:alert(1)", None, None));
    }
}
