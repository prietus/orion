//! Writing messages: the `$EDITOR` template, reply/forward, and MIME building.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use mail_builder::headers::address::Address as BAddress;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::MimePart;
use mail_builder::MessageBuilder;
use mail_parser::{Address, MessageParser};

use crate::model::{Attachment, MessageView};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    /// Message-ID being answered (without angle brackets).
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub body: String,
    pub attachments: Vec<Attachment>,
    pub sign: bool,
    pub encrypt: bool,
    /// Key to sign with (and to encrypt our own copy to) instead of the From address's.
    pub signing_key: Option<String>,
    /// Value of an `Autocrypt:` header to add (our public key).
    pub autocrypt: Option<String>,
}

impl PartialEq for Attachment {
    fn eq(&self, o: &Self) -> bool {
        self.name == o.name && self.mime == o.mime && self.data == o.data
    }
}

// ---------------------------------------------------------------- template

pub const TEMPLATE_HELP: &str = "# Orion: edit the headers, write below the blank line. `Attach: <path>` adds a file; delete an Attach line to drop it.";

/// Text handed to `$EDITOR`.
pub fn to_template(d: &Draft) -> String {
    let mut t = format!("{TEMPLATE_HELP}\nFrom: {}\nTo: {}\nCc: {}\nBcc: {}\nSubject: {}\n", d.from, d.to.join(", "), d.cc.join(", "), d.bcc.join(", "), d.subject);
    for a in &d.attachments {
        t.push_str(&format!("Attach: {}\n", a.name));
    }
    t.push('\n');
    t.push_str(&d.body);
    t
}

/// Parses the edited template back. `previous` provides attachments listed by
/// name (kept as they are); any other `Attach:` value is a file to load.
pub fn from_template(text: &str, previous: &Draft) -> Result<Draft, String> {
    let text = text.replace("\r\n", "\n");
    let (head, body) = text.split_once("\n\n").unwrap_or((&text, ""));
    let mut d = Draft {
        body: body.to_string(),
        in_reply_to: previous.in_reply_to.clone(),
        references: previous.references.clone(),
        sign: previous.sign,
        encrypt: previous.encrypt,
        signing_key: previous.signing_key.clone(),
        autocrypt: previous.autocrypt.clone(),
        ..Default::default()
    };
    // Unfold continuation lines.
    let mut lines: Vec<String> = Vec::new();
    for line in head.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push(' ');
            last.push_str(line.trim());
            continue;
        }
        lines.push(line.to_string());
    }
    for line in lines {
        let (key, value) = line.split_once(':').ok_or_else(|| format!("not a header line: `{line}`"))?;
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "from" => d.from = value.to_string(),
            "to" => d.to = parse_addresses(value)?,
            "cc" => d.cc = parse_addresses(value)?,
            "bcc" => d.bcc = parse_addresses(value)?,
            "subject" => d.subject = value.to_string(),
            "attach" => {
                if value.is_empty() {
                    continue;
                }
                match previous.attachments.iter().find(|a| a.name == value) {
                    Some(a) => d.attachments.push(a.clone()),
                    None => d.attachments.push(load_attachment(Path::new(&expand_home(value)))?),
                }
            }
            other => return Err(format!("unknown header `{other}` (use From, To, Cc, Bcc, Subject, Attach)")),
        }
    }
    if parse_addresses(&d.from)?.len() != 1 {
        return Err("From must be exactly one address".into());
    }
    Ok(d)
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p),
    }
}

pub fn load_attachment(path: &Path) -> Result<Attachment, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot attach {}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "attachment".into());
    Ok(Attachment { mime: mime_for(&name).into(), name, inline: false, content_id: None, data })
}

pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "json" => "application/json",
        "csv" => "text/csv",
        "ics" => "text/calendar",
        "asc" => "application/pgp-keys",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        _ => "application/octet-stream",
    }
}

/// Splits and validates an address list ("Ana <a@x>, b@y"). Names with
/// commas must be quoted, as in any mail header.
pub fn parse_addresses(value: &str) -> Result<Vec<String>, String> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    let raw = format!("To: {value}\r\n\r\n");
    let msg = MessageParser::new().parse_headers(raw.as_bytes()).ok_or("invalid address list")?;
    let list = msg.root().to().ok_or_else(|| format!("invalid address list: `{value}`"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let mailboxes: Vec<_> = match item {
            Address::Mailbox(m) => vec![m],
            Address::Group(g) => g.mailboxes().collect(),
        };
        for m in mailboxes {
            let addr = m.address().unwrap_or_default();
            if !valid_addr(addr) {
                return Err(format!("`{}` is not a valid address", m.name().map(|n| format!("{n} <{addr}>")).unwrap_or_else(|| addr.to_string())));
            }
            out.push(match m.name().filter(|n| !n.is_empty()) {
                Some(n) if n.contains([',', '"', '<', '>', ';', ':', '@']) => format!("\"{}\" <{addr}>", n.replace('"', "'")),
                Some(n) => format!("{n} <{addr}>"),
                None => addr.to_string(),
            });
        }
    }
    Ok(out)
}

fn valid_addr(a: &str) -> bool {
    let Some((local, domain)) = a.rsplit_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !a.contains(char::is_whitespace) && !a.contains(['<', '>', ','])
}

/// Bare address (`ana@x`) from "Name <ana@x>" or "ana@x".
pub fn addr_spec(s: &str) -> String {
    match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => s[a + 1..b].trim().to_string(),
        _ => s.trim().to_string(),
    }
}

fn split_name(s: &str) -> (Option<String>, String) {
    let addr = addr_spec(s);
    let name = s.rfind('<').map(|i| s[..i].trim().trim_matches('"').to_string()).filter(|n| !n.is_empty());
    (name, addr)
}

// ------------------------------------------------------ reply and forward

/// Original headers needed to answer a message, re-parsed from the raw block.
struct Original {
    from: Vec<String>,
    to: Vec<String>,
    cc: Vec<String>,
    reply_to: Vec<String>,
    message_id: Option<String>,
    references: Vec<String>,
}

fn original(view: &MessageView) -> Original {
    let raw = format!("{}\r\n", view.raw_headers.trim_end());
    let parsed = MessageParser::new().parse_headers(raw.as_bytes());
    let list = |l: Option<mail_parser::AddressList<'_>>| -> Vec<String> {
        l.map(|l| {
            l.iter()
                .flat_map(|a| match a {
                    Address::Mailbox(m) => vec![m],
                    Address::Group(g) => g.mailboxes().collect(),
                })
                .filter_map(|m| {
                    let addr = m.address()?;
                    Some(match m.name().filter(|n| !n.is_empty()) {
                        Some(n) if n.contains([',', '"']) => format!("\"{}\" <{addr}>", n.replace('"', "'")),
                        Some(n) => format!("{n} <{addr}>"),
                        None => addr.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    };
    match parsed {
        Some(m) => {
            let r = m.root();
            Original {
                from: list(r.from()),
                to: list(r.to()),
                cc: list(r.cc()),
                reply_to: list(r.reply_to()),
                message_id: r.message_id().map(str::to_string),
                references: r.references().map(|l| l.iter().map(str::to_string).collect()).unwrap_or_default(),
            }
        }
        None => Original { from: Vec::new(), to: Vec::new(), cc: Vec::new(), reply_to: Vec::new(), message_id: None, references: Vec::new() },
    }
}

fn prefixed(subject: &str, prefix: &str) -> String {
    let lower = subject.to_ascii_lowercase();
    if lower.starts_with(&prefix.to_ascii_lowercase()) {
        subject.to_string()
    } else {
        format!("{prefix} {subject}")
    }
}

fn quote(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(|l| if l.is_empty() { ">".to_string() } else if l.starts_with('>') { format!(">{l}") } else { format!("> {l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A new message: just From and the signature.
pub fn new_message(from: &str, signature: Option<&str>) -> Draft {
    Draft { from: from.to_string(), body: signature_block(signature), ..Default::default() }
}

fn signature_block(sig: Option<&str>) -> String {
    match sig.filter(|s| !s.trim().is_empty()) {
        Some(s) => format!("\n\n-- \n{}\n", s.trim_end()),
        None => "\n".into(),
    }
}

/// Reply (or reply-all) to `view`. `me` are the sender's own addresses, never answered.
pub fn reply(view: &MessageView, from: &str, me: &[String], all: bool, signature: Option<&str>) -> Draft {
    let o = original(view);
    let is_me = |a: &str| me.iter().any(|m| addr_spec(m).eq_ignore_ascii_case(&addr_spec(a))) || addr_spec(from).eq_ignore_ascii_case(&addr_spec(a));
    let sent_by_me = o.from.iter().any(|f| is_me(f));
    let mut to: Vec<String> = if sent_by_me {
        o.to.clone() // replying to my own message: write to its recipients again
    } else if !o.reply_to.is_empty() {
        o.reply_to.clone()
    } else {
        o.from.clone()
    };
    to.retain(|a| !is_me(a) || sent_by_me);
    let mut cc = Vec::new();
    if all {
        for a in o.to.iter().chain(&o.cc) {
            let already = to.iter().chain(&cc).any(|x| addr_spec(x).eq_ignore_ascii_case(&addr_spec(a)));
            if !is_me(a) && !already {
                cc.push(a.clone());
            }
        }
    }
    let mut references = o.references.clone();
    if let Some(id) = &o.message_id {
        references.push(id.clone());
    }
    let who = o.from.first().cloned().unwrap_or_else(|| view.from.clone());
    let body = format!("\n\nOn {}, {who} wrote:\n{}\n{}", view.date, quote(&view.text), signature_block(signature).trim_start_matches('\n'));
    Draft {
        from: from.to_string(),
        to,
        cc,
        subject: prefixed(&view.subject, "Re:"),
        in_reply_to: o.message_id,
        references,
        body,
        ..Default::default()
    }
}

/// Forward `view` with its attachments.
pub fn forward(view: &MessageView, from: &str, signature: Option<&str>) -> Draft {
    let mut header = format!("---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n", view.from, view.date, view.subject, view.to);
    if !view.cc.is_empty() {
        header.push_str(&format!("Cc: {}\n", view.cc));
    }
    Draft {
        from: from.to_string(),
        subject: prefixed(&view.subject, "Fwd:"),
        body: format!("\n\n{header}\n{}\n{}", view.text.trim_end(), signature_block(signature).trim_start_matches('\n')),
        attachments: view.attachments.clone(),
        ..Default::default()
    }
}

// ------------------------------------------------------------- MIME build

/// Addresses for the SMTP envelope (To + Cc + Bcc, deduplicated).
pub fn recipients(d: &Draft) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in d.to.iter().chain(&d.cc).chain(&d.bcc) {
        let spec = addr_spec(a);
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&spec)) {
            out.push(spec);
        }
    }
    out
}

fn to_builder_list(list: &[String]) -> BAddress<'static> {
    BAddress::new_list(list.iter().map(|s| {
        let (name, addr) = split_name(s);
        BAddress::new_address(name, addr)
    }).collect())
}

/// The body entity: text/plain alone, or multipart/mixed with attachments.
/// With `protected_subject`, the Subject is repeated inside (it is hidden as
/// "..." outside when encrypting).
pub fn body_part(d: &Draft, protected_subject: bool) -> MimePart<'static> {
    let text = MimePart::new("text/plain", d.body.replace("\r\n", "\n"));
    let mut part = if d.attachments.is_empty() {
        text
    } else {
        let mut parts = vec![text];
        for a in &d.attachments {
            parts.push(MimePart::new(a.mime.clone(), a.data.clone()).attachment(a.name.clone()));
        }
        MimePart::new("multipart/mixed", parts)
    };
    if protected_subject {
        part = part.header("Subject", mail_builder::headers::text::Text::new(d.subject.clone()));
    }
    part
}

/// Full message with the given body entity. `subject` overrides the draft's
/// (used for "..." when the real one is protected inside).
pub fn message(d: &Draft, body: MimePart<'static>, subject: Option<&str>) -> Vec<u8> {
    let (from_name, from_addr) = split_name(&d.from);
    let mut b = MessageBuilder::new()
        .from(BAddress::new_address(from_name, from_addr.clone()))
        .subject(subject.unwrap_or(&d.subject).to_string())
        .message_id(new_message_id(&from_addr))
        .header("User-Agent", Raw::new(concat!("Orion/", env!("CARGO_PKG_VERSION"))));
    if let Some(ac) = &d.autocrypt {
        b = b.header("Autocrypt", Raw::new(ac.clone()));
    }
    if !d.to.is_empty() {
        b = b.to(to_builder_list(&d.to));
    }
    if !d.cc.is_empty() {
        b = b.cc(to_builder_list(&d.cc));
    }
    // Bcc is deliberately not written: those recipients only appear in the SMTP envelope.
    if let Some(id) = &d.in_reply_to {
        b = b.in_reply_to(id.clone());
    }
    if !d.references.is_empty() {
        b = b.references(d.references.clone());
    }
    b.body(body).write_to_vec().unwrap_or_default()
}

pub fn build_plain(d: &Draft) -> Vec<u8> {
    message(d, body_part(d, false), None)
}

/// `<random@sender-domain>`: never the local host name.
fn new_message_id(from_addr: &str) -> String {
    let domain = from_addr.rsplit_once('@').map(|(_, d)| d).filter(|d| !d.is_empty()).unwrap_or("orion.invalid");
    let mut h = RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let a = h.finish();
    let b = RandomState::new().build_hasher().finish();
    format!("{a:016x}{b:016x}@{domain}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::message_view;

    const ORIGINAL: &[u8] = b"From: \"Garcia, Ana\" <ana@example.org>\r\nTo: me@mine.org, Bob <bob@example.org>\r\nCc: carol@example.org, ME <ME@mine.org>\r\n\
Reply-To: list@example.org\r\nSubject: Plans\r\nMessage-ID: <orig-1@example.org>\r\nReferences: <root@example.org>\r\nDate: Thu, 20 Nov 2025 10:00:00 +0100\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\nShall we meet?\r\n\r\n> earlier quote\r\n";

    #[test]
    fn addresses() {
        assert_eq!(parse_addresses(r#""Garcia, Ana" <ana@x.org>, b@y.org"#).unwrap(), [r#""Garcia, Ana" <ana@x.org>"#, "b@y.org"]);
        assert_eq!(parse_addresses("Bob <bob@y.org>").unwrap(), ["Bob <bob@y.org>"]);
        assert!(parse_addresses("not an address").is_err());
        assert!(parse_addresses("a@b").is_err(), "domain without a dot");
        assert_eq!(addr_spec(r#""Garcia, Ana" <ana@x.org>"#), "ana@x.org");
    }

    #[test]
    fn template_roundtrip() {
        let mut d = new_message("Me <me@mine.org>", Some("Me\nmine.org"));
        d.to = vec!["Bob <bob@example.org>".into()];
        d.subject = "Hola ñ".into();
        d.attachments.push(Attachment { name: "a.txt".into(), mime: "text/plain".into(), inline: false, content_id: None, data: b"x".to_vec() });
        let t = to_template(&d);
        assert!(t.contains("Attach: a.txt\n\n") && t.contains("\n-- \nMe\nmine.org"));
        let back = from_template(&t, &d).unwrap();
        assert_eq!(back, d);

        // Editing: drop the attachment line, add a recipient, attach a real file.
        let dir = std::env::temp_dir().join(format!("orion-compose-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("report.pdf");
        std::fs::write(&file, b"%PDF-1.4").unwrap();
        let edited = t.replace("Attach: a.txt\n", &format!("Attach: {}\n", file.display())).replace("Cc: \n", "Cc: carol@example.org\n");
        let back = from_template(&edited, &d).unwrap();
        assert_eq!(back.cc, ["carol@example.org"]);
        assert_eq!(back.attachments.len(), 1);
        assert_eq!((back.attachments[0].name.as_str(), back.attachments[0].mime.as_str()), ("report.pdf", "application/pdf"));
        std::fs::remove_dir_all(dir).unwrap();

        assert!(from_template("From: me@mine.org\nX-Evil: 1\n\nbody", &d).unwrap_err().contains("unknown header"));
        assert!(from_template("From: me@mine.org\nAttach: /nonexistent/file\n\nbody", &d).unwrap_err().contains("cannot attach"));
    }

    #[test]
    fn reply_and_reply_all() {
        let view = message_view(ORIGINAL).unwrap();
        let me = vec!["me@mine.org".to_string()];
        let r = reply(&view, "Me <me@mine.org>", &me, false, None);
        assert_eq!(r.to, ["list@example.org"], "Reply-To wins");
        assert!(r.cc.is_empty());
        assert_eq!(r.subject, "Re: Plans");
        assert_eq!(r.in_reply_to.as_deref(), Some("orig-1@example.org"));
        assert_eq!(r.references, ["root@example.org", "orig-1@example.org"]);
        assert!(r.body.contains("> Shall we meet?\n>\n>> earlier quote"), "{}", r.body);
        assert!(r.body.contains("\"Garcia, Ana\" <ana@example.org> wrote:"));

        let all = reply(&view, "Me <me@mine.org>", &me, true, None);
        assert_eq!(all.cc, ["Bob <bob@example.org>", "carol@example.org"], "never myself (any case), no duplicates");
        assert_eq!(reply(&view, "Me <me@mine.org>", &me, false, None).subject, prefixed("Re: Plans", "Re:"));
    }

    #[test]
    fn build_and_parse_back() {
        let mut d = new_message("Ana García <ana@example.org>", None);
        d.to = vec![r#""Pérez, Bob" <bob@example.org>"#.into()];
        d.bcc = vec!["secret@example.org".into()];
        d.subject = "Informe ñandú".into();
        d.body = "Hola,\nadjunto el informe.\n".into();
        d.in_reply_to = Some("x1@example.org".into());
        d.attachments.push(Attachment { name: "informe.pdf".into(), mime: "application/pdf".into(), inline: false, content_id: None, data: vec![0, 1, 2, 255] });
        let raw = build_plain(&d);
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.to_ascii_lowercase().contains("\nbcc:") && !text.contains("secret@example.org"), "Bcc never appears in the message");
        assert!(text.contains("@example.org>") && !text.contains("@alarm"), "Message-ID uses the sender domain");
        assert_eq!(recipients(&d), ["bob@example.org", "secret@example.org"]);
        let v = message_view(&raw).unwrap();
        assert_eq!(v.subject, "Informe ñandú");
        assert_eq!(v.from, "Ana García <ana@example.org>");
        assert_eq!(v.to, "Pérez, Bob <bob@example.org>", "one recipient: the comma in the name did not split it");
        assert_eq!(v.text.trim(), "Hola,\nadjunto el informe.");
        assert_eq!(v.attachments[0].data, [0, 1, 2, 255]);
        assert!(v.raw_headers.contains("In-Reply-To: <x1@example.org>"));
    }
}
