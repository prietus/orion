//! Private server discovery for a domain, asking only the domain itself:
//! 1. its autoconfig file (Thunderbird format) over HTTPS,
//! 2. DNS SRV records (RFC 6186 / RFC 8314).
//!
//! Third-party databases are never queried. HTTPS only: an HTTP answer could
//! be forged to send the password to someone else's server.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use orion_core::config::TlsMode;

use crate::providers::Server;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    pub imap: Option<Server>,
    pub pop3: Option<Server>,
    pub smtp: Option<Server>,
    /// Where the answer came from ("autoconfig" or "DNS SRV").
    pub source: &'static str,
    /// Every SRV record used was DNSSEC-validated by a resolver on this machine.
    pub dnssec: bool,
}

impl Discovered {
    pub fn is_empty(&self) -> bool {
        self.imap.is_none() && self.pop3.is_none()
    }

    /// Hosts outside the email's domain that the user must confirm (RFC 6186
    /// §6): unauthenticated DNS could point anywhere. Empty when the answer is
    /// DNSSEC-validated (the domain owner signed the delegation) or came from
    /// the domain's own autoconfig over verified HTTPS.
    pub fn foreign_hosts(&self, domain: &str) -> Vec<String> {
        if self.dnssec || self.source == "autoconfig" {
            return Vec::new();
        }
        let d = domain.to_ascii_lowercase();
        [&self.imap, &self.pop3, &self.smtp]
            .into_iter()
            .flatten()
            .map(|s| s.host.to_ascii_lowercase())
            .filter(|h| h != &d && !h.ends_with(&format!(".{d}")))
            .collect()
    }
}

/// SRV answer and whether it can be trusted as DNSSEC-validated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SrvAnswer {
    pub records: Vec<Srv>,
    pub authenticated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Srv {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// Discovers servers for `email` (whose domain is `domain`).
pub fn discover(domain: &str, email: &str) -> Discovered {
    discover_with(domain, email, &fetch_https, &srv_lookup)
}

pub fn discover_with(domain: &str, email: &str, fetch: &dyn Fn(&str) -> Option<String>, srv: &dyn Fn(&str) -> SrvAnswer) -> Discovered {
    let enc: String = email.bytes().map(|b| if b.is_ascii_alphanumeric() || b"@.-_".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let urls = [
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={enc}"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
    ];
    for url in &urls {
        if let Some(xml) = fetch(url) {
            let mut d = parse_autoconfig(&xml);
            if !d.is_empty() {
                d.source = "autoconfig";
                if d.smtp.is_none() {
                    d.smtp = from_srv(domain, srv).smtp;
                }
                return d;
            }
        }
    }
    from_srv(domain, srv)
}

fn from_srv(domain: &str, srv: &dyn Fn(&str) -> SrvAnswer) -> Discovered {
    let all_authenticated = std::cell::Cell::new(true);
    let first = |service: &str, tls: TlsMode| {
        let answer = srv(&format!("{service}._tcp.{domain}"));
        let found = answer
            .records
            .into_iter()
            .find(|r| r.target != "." && !r.target.is_empty() && r.port != 0)
            .map(|r| Server { host: r.target.trim_end_matches('.').to_string(), port: r.port, tls });
        if found.is_some() && !answer.authenticated {
            all_authenticated.set(false);
        }
        found
    };
    // Implicit TLS is preferred over STARTTLS (RFC 8314).
    let imap = first("_imaps", TlsMode::Tls).or_else(|| first("_imap", TlsMode::Starttls));
    let pop3 = first("_pop3s", TlsMode::Tls).or_else(|| first("_pop3", TlsMode::Starttls));
    let smtp = first("_submissions", TlsMode::Tls).or_else(|| first("_submission", TlsMode::Starttls));
    let dnssec = all_authenticated.get() && (imap.is_some() || pop3.is_some());
    Discovered { imap, pop3, smtp, source: "DNS SRV", dnssec }
}

fn fetch_https(url: &str) -> Option<String> {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .https_only(true)
        .tls_config(tls)
        .build()
        .into();
    let mut res = agent.get(url).call().ok()?;
    res.body_mut().with_config().limit(256 * 1024).read_to_string().ok()
}

/// Minimal parser for Thunderbird's autoconfig format (`clientConfig` 1.1).
pub fn parse_autoconfig(xml: &str) -> Discovered {
    let mut d = Discovered::default();
    for (tag, kind, block) in blocks(xml) {
        let field = |name: &str| inner(block, name);
        let (Some(host), Some(port)) = (field("hostname"), field("port").and_then(|p| p.parse::<u16>().ok())) else { continue };
        let tls = match field("socketType").unwrap_or_default().to_ascii_uppercase().as_str() {
            "SSL" | "TLS" => TlsMode::Tls,
            "STARTTLS" => TlsMode::Starttls,
            _ => continue, // never offer plain-text servers from autoconfig
        };
        let server = Some(Server { host, port, tls });
        match (tag, kind.as_str()) {
            ("incomingServer", "imap") if d.imap.is_none() => d.imap = server,
            ("incomingServer", "pop3") if d.pop3.is_none() => d.pop3 = server,
            ("outgoingServer", "smtp") if d.smtp.is_none() => d.smtp = server,
            _ => {}
        }
    }
    d
}

/// `<incomingServer type="imap">...</incomingServer>` blocks, in order.
fn blocks(xml: &str) -> Vec<(&'static str, String, &str)> {
    let mut out = Vec::new();
    for tag in ["incomingServer", "outgoingServer"] {
        let mut rest = xml;
        while let Some(start) = rest.find(&format!("<{tag}")) {
            let after = &rest[start..];
            let Some(open_end) = after.find('>') else { break };
            let open = &after[..open_end];
            let kind = open
                .split("type=")
                .nth(1)
                .map(|v| v.trim_start_matches(['"', '\'']).split(['"', '\'']).next().unwrap_or_default().to_ascii_lowercase())
                .unwrap_or_default();
            let close = format!("</{tag}>");
            let Some(end) = after.find(&close) else { break };
            out.push((tag, kind, &after[open_end + 1..end]));
            rest = &after[end + close.len()..];
        }
    }
    // Keep document order within each tag; incoming before outgoing is fine.
    out
}

fn inner(block: &str, name: &str) -> Option<String> {
    let start = block.find(&format!("<{name}>"))? + name.len() + 2;
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim().to_string()).filter(|s| !s.is_empty())
}

// --------------------------------------------------------- tiny DNS client

/// SRV lookup against the system resolver (the `nameserver`s in
/// /etc/resolv.conf). Sorted by priority, then weight (highest first).
///
/// The query sets the AD bit (RFC 6840 §5.7) so a validating resolver says
/// whether it checked DNSSEC. That bit is only believed from a resolver on
/// this machine: from a remote one, over plain UDP, it could be forged along
/// with the records themselves.
pub fn srv_lookup(name: &str) -> SrvAnswer {
    for ns in nameservers() {
        if let Some((mut records, ad)) = query_srv(ns, name) {
            records.sort_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)));
            return SrvAnswer { records, authenticated: ad && ns.ip().is_loopback() };
        }
    }
    SrvAnswer::default()
}

fn nameservers() -> Vec<SocketAddr> {
    let conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let mut v: Vec<SocketAddr> = conf
        .lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|ip| ip.trim().split('%').next()?.parse::<std::net::IpAddr>().ok())
        .map(|ip| SocketAddr::new(ip, 53))
        .collect();
    if v.is_empty() {
        v.push("127.0.0.53:53".parse().expect("valid"));
    }
    v
}

fn query_srv(ns: SocketAddr, name: &str) -> Option<(Vec<Srv>, bool)> {
    let id: u16 = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.subsec_nanos() & 0xFFFF) as u16;
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x20, 0, 1, 0, 0, 0, 0, 0, 0]); // RD + AD, 1 question
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 33, 0, 1]); // root, QTYPE=SRV, QCLASS=IN
    let bind = if ns.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    sock.send_to(&q, ns).ok()?;
    let mut buf = [0u8; 4096];
    let (n, from) = sock.recv_from(&mut buf).ok()?;
    if from.ip() != ns.ip() {
        return None;
    }
    let records = parse_srv_response(&buf[..n], id)?;
    let ad = buf[3] & 0x20 != 0;
    Some((records, ad))
}

pub fn parse_srv_response(msg: &[u8], id: u16) -> Option<Vec<Srv>> {
    if msg.len() < 12 || u16::from_be_bytes([msg[0], msg[1]]) != id {
        return None;
    }
    let rcode = msg[3] & 0x0F;
    if rcode == 3 {
        return Some(Vec::new()); // NXDOMAIN: definitive "no records"
    }
    if rcode != 0 {
        return None;
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_name(msg, pos)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        pos = skip_name(msg, pos)?;
        let rtype = u16::from_be_bytes([*msg.get(pos)?, *msg.get(pos + 1)?]);
        let rdlen = u16::from_be_bytes([*msg.get(pos + 8)?, *msg.get(pos + 9)?]) as usize;
        let rdata = pos + 10;
        if rtype == 33 && rdlen >= 7 {
            let r = |i: usize| -> Option<u16> { Some(u16::from_be_bytes([*msg.get(rdata + i)?, *msg.get(rdata + i + 1)?])) };
            out.push(Srv { priority: r(0)?, weight: r(2)?, port: r(4)?, target: read_name(msg, rdata + 6)? });
        }
        pos = rdata + rdlen;
    }
    Some(out)
}

fn skip_name(msg: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            return Some(pos + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(pos + 2);
        }
        pos += 1 + len;
    }
}

fn read_name(msg: &[u8], mut pos: usize) -> Option<String> {
    let mut labels = Vec::new();
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            break;
        }
        if len & 0xC0 == 0xC0 {
            jumps += 1;
            if jumps > 16 {
                return None; // compression loop
            }
            pos = ((len & 0x3F) << 8) | *msg.get(pos + 1)? as usize;
            continue;
        }
        labels.push(String::from_utf8_lossy(msg.get(pos + 1..pos + 1 + len)?).into_owned());
        pos += 1 + len;
    }
    Some(if labels.is_empty() { ".".into() } else { labels.join(".") })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1"><emailProvider id="example.org">
  <incomingServer type="pop3"><hostname>pop.example.org</hostname><port>995</port><socketType>SSL</socketType></incomingServer>
  <incomingServer type='imap'>
    <hostname> mail.example.org </hostname><port>143</port><socketType>STARTTLS</socketType>
    <username>%EMAILADDRESS%</username>
  </incomingServer>
  <incomingServer type="imap"><hostname>plain.example.org</hostname><port>143</port><socketType>plain</socketType></incomingServer>
  <outgoingServer type="smtp"><hostname>smtp.example.org</hostname><port>465</port><socketType>SSL</socketType></outgoingServer>
</emailProvider></clientConfig>"#;

    #[test]
    fn autoconfig_xml() {
        let d = parse_autoconfig(XML);
        assert_eq!(d.imap, Some(Server { host: "mail.example.org".into(), port: 143, tls: TlsMode::Starttls }));
        assert_eq!(d.pop3.unwrap().port, 995);
        assert_eq!(d.smtp.unwrap().tls, TlsMode::Tls);
    }

    #[test]
    fn discovery_order_and_srv_fallback() {
        // Autoconfig answers: SRV only completes what is missing.
        let fetch = |url: &str| url.starts_with("https://autoconfig.example.org/").then(|| XML.replace("<outgoingServer", "<ignored").replace("</outgoingServer>", "</ignored>"));
        let srv = |name: &str| SrvAnswer {
            records: match name {
                "_submissions._tcp.example.org" => vec![Srv { priority: 0, weight: 0, port: 465, target: "out.example.org.".into() }],
                _ => vec![],
            },
            authenticated: false,
        };
        let d = discover_with("example.org", "ana@example.org", &fetch, &srv);
        assert_eq!(d.source, "autoconfig");
        assert_eq!(d.smtp.unwrap().host, "out.example.org");

        // No autoconfig: SRV, implicit TLS preferred; "." means "not offered".
        let answers = |authenticated: bool| {
            move |name: &str| SrvAnswer {
                records: match name {
                    "_imaps._tcp.example.org" => vec![Srv { priority: 0, weight: 0, port: 0, target: ".".into() }],
                    "_imap._tcp.example.org" => vec![Srv { priority: 10, weight: 5, port: 143, target: "imap.provider.net.".into() }],
                    _ => vec![],
                },
                authenticated,
            }
        };
        let d = discover_with("example.org", "ana@example.org", &|_| None, &answers(false));
        assert_eq!(d.source, "DNS SRV");
        assert!(!d.dnssec);
        assert_eq!(d.imap, Some(Server { host: "imap.provider.net".into(), port: 143, tls: TlsMode::Starttls }));
        assert_eq!(d.foreign_hosts("example.org"), ["imap.provider.net"], "hosts outside the domain need confirmation");

        // Same answer, DNSSEC-validated by a local resolver: the domain owner signed it.
        let d = discover_with("example.org", "ana@example.org", &|_| None, &answers(true));
        assert!(d.dnssec);
        assert!(d.foreign_hosts("example.org").is_empty());
    }

    #[test]
    fn dns_response_parsing() {
        // Response to _imaps._tcp.example.org SRV with one compressed answer.
        let mut m = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
        for l in ["_imaps", "_tcp", "example", "org"] {
            m.push(l.len() as u8);
            m.extend_from_slice(l.as_bytes());
        }
        m.extend_from_slice(&[0, 0, 33, 0, 1]);
        m.extend_from_slice(&[0xC0, 12, 0, 33, 0, 1, 0, 0, 0x0E, 0x10]);
        let mut rdata = vec![0, 10, 0, 20, 0x03, 0xE1];
        rdata.extend_from_slice(&[4]);
        rdata.extend_from_slice(b"mail");
        rdata.extend_from_slice(&[0xC0, 12 + 7 + 5]); // -> "example.org"
        m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        m.extend_from_slice(&rdata);
        let r = parse_srv_response(&m, 0x1234).unwrap();
        assert_eq!(r, [Srv { priority: 10, weight: 20, port: 993, target: "mail.example.org".into() }]);
        assert_eq!(parse_srv_response(&m, 0x9999), None, "id mismatch is ignored");
    }
}
