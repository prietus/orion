//! Conversion of raw messages into [`Envelope`] / [`MessageView`].

use mail_parser::{Address, AddressList, DateTime, MessageParser, MessageRef};

use crate::model::{Attachment, Envelope, Flags, MessageId, MessageView};

/// Builds an envelope from the headers only (fast: the body is not decoded).
pub fn envelope_from_headers(id: MessageId, raw: &[u8], size: usize) -> Envelope {
    let parser = MessageParser::new();
    let msg = parser.parse_headers(raw);
    let mut env = Envelope {
        id,
        from_name: String::new(),
        from_addr: String::new(),
        subject: String::new(),
        timestamp: 0,
        date: String::new(),
        flags: Flags::default(),
        size,
        has_attachments: false,
        message_id: String::new(),
        parents: Vec::new(),
    };
    let header_end = header_block(raw);
    env.flags = mbox_flags(header_end);
    env.has_attachments = guess_attachments(header_end);
    let Some(msg) = msg else { return env };
    let root = msg.root();
    if let Some(m) = root.from().and_then(|l| l.first()) {
        env.from_name = m.name().unwrap_or_default().to_string();
        env.from_addr = m.address().unwrap_or_default().to_string();
    }
    env.subject = root.subject().unwrap_or("(no subject)").to_string();
    if let Some(d) = root.date() {
        env.timestamp = d.to_timestamp();
        env.date = short_date(&d);
    }
    env.message_id = root.message_id().map(|id| id.trim_matches(['<', '>', ' ']).to_string()).unwrap_or_default();
    let mut parents: Vec<String> = Vec::new();
    for h in [root.references(), root.in_reply_to()].into_iter().flatten() {
        for id in h.iter() {
            let id = id.trim_matches(['<', '>', ' ']).to_string();
            if !id.is_empty() && id != env.message_id && !parents.contains(&id) {
                parents.push(id);
            }
        }
    }
    env.parents = parents;
    env
}

/// Full parse for the reader.
pub fn message_view(raw: &[u8]) -> Option<MessageView> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    let mut view = MessageView {
        subject: root.subject().unwrap_or("(no subject)").to_string(),
        from: fmt_list(root.from()),
        to: fmt_list(root.to()),
        cc: fmt_list(root.cc()),
        reply_to: fmt_list(root.reply_to()),
        date: root.date().map(|d| long_date(&d)).unwrap_or_default(),
        message_id: root.message_id().unwrap_or_default().to_string(),
        raw_headers: String::from_utf8_lossy(root.root_part().raw_headers()).replace('\r', ""),
        text: String::new(),
        html: None,
        html_doc: None,
        text_doc: orion_html::Doc::from_plain(""),
        attachments: Vec::new(),
        crypto: None,
        key_offer: None,
    };
    view.text = body_text(&root);
    view.text_doc = orion_html::Doc::from_plain(&view.text);
    let html: Vec<_> = root
        .html_body()
        .filter(|p| p.is_content_type("text", "html"))
        .filter_map(|p| p.text())
        .collect();
    if !html.is_empty() {
        let html = html.join("\n");
        view.html_doc = Some(orion_html::Doc::parse(&html));
        view.html = Some(html);
    }
    for part in root.attachments() {
        let mime = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(st) => format!("{}/{}", ct.ctype(), st),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".into())
            .to_ascii_lowercase();
        let name = part
            .attachment_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("part-{}.{}", part.id(), ext_for(&mime)));
        view.attachments.push(Attachment {
            name,
            inline: part.content_disposition().is_some_and(|d| d.is_inline()),
            content_id: part.content_id().map(|c| c.trim_matches(['<', '>']).to_string()),
            data: part.decoded().into_owned(),
            mime,
        });
    }
    Some(view)
}

fn body_text(root: &MessageRef<'_>) -> String {
    let mut out = String::new();
    for part in root.text_body() {
        let Some(t) = part.text() else { continue };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        if part.is_content_type("text", "html") {
            out.push_str(&crate::html::html_to_text(&t));
        } else {
            out.push_str(&t);
        }
    }
    out.retain(|c| c != '\r');
    out
}

fn fmt_list(list: Option<AddressList<'_>>) -> String {
    let Some(list) = list else { return String::new() };
    let mut parts = Vec::new();
    for item in list.iter() {
        match item {
            Address::Mailbox(m) => parts.push(fmt_mailbox(m.name(), m.address())),
            Address::Group(g) => {
                let members: Vec<_> = g.mailboxes().map(|m| fmt_mailbox(m.name(), m.address())).collect();
                parts.push(format!("{}: {};", g.name().unwrap_or(""), members.join(", ")));
            }
        }
    }
    parts.join(", ")
}

fn fmt_mailbox(name: Option<&str>, addr: Option<&str>) -> String {
    match (name, addr) {
        (Some(n), Some(a)) if !n.is_empty() => format!("{n} <{a}>"),
        (_, Some(a)) => a.to_string(),
        (Some(n), None) => n.to_string(),
        (None, None) => String::new(),
    }
}

fn short_date(d: &DateTime) -> String {
    format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute)
}

fn long_date(d: &DateTime) -> String {
    let sign = if d.tz_before_gmt { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {sign}{:02}{:02}",
        d.year, d.month, d.day, d.hour, d.minute, d.second, d.tz_hour, d.tz_minute
    )
}

fn ext_for(mime: &str) -> &str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/html" => "html",
        _ => "bin",
    }
}

/// Header block (up to the first blank line).
fn header_block(raw: &[u8]) -> &[u8] {
    let end = memchr::memmem::find(raw, b"\n\n")
        .or_else(|| memchr::memmem::find(raw, b"\r\n\r\n"))
        .unwrap_or(raw.len());
    &raw[..end]
}

/// Reads `Status:` / `X-Status:` (mutt/pine mbox convention).
fn mbox_flags(headers: &[u8]) -> Flags {
    let mut f = Flags::default();
    for line in headers.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(v) = strip_header(line, b"status:") {
            f.seen |= v.contains(&b'R');
        } else if let Some(v) = strip_header(line, b"x-status:") {
            f.answered |= v.contains(&b'A');
            f.flagged |= v.contains(&b'F');
            f.deleted |= v.contains(&b'D');
            f.draft |= v.contains(&b'T');
        }
    }
    f
}

fn guess_attachments(headers: &[u8]) -> bool {
    headers
        .split(|&b| b == b'\n')
        .filter_map(|l| strip_header(l, b"content-type:"))
        .any(|v| v.to_ascii_lowercase().windows(15).any(|w| w == b"multipart/mixed"))
}

fn strip_header<'a>(line: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    (line.len() >= name.len() && line[..name.len()].eq_ignore_ascii_case(name))
        .then(|| &line[name.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &[u8] = b"From: Ana <ana@example.com>\r\nTo: bob@example.com\r\nSubject: Hello\r\n\
Date: Tue, 1 Jul 2025 10:00:00 +0200\r\nStatus: RO\r\nX-Status: F\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
Body\r\n--b\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=\"x.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b--\r\n";

    #[test]
    fn envelope() {
        let e = envelope_from_headers(MessageId(1), RAW, RAW.len());
        assert_eq!(e.from_name, "Ana");
        assert_eq!(e.subject, "Hello");
        assert_eq!(e.date, "2025-07-01 10:00");
        assert!(e.flags.seen && e.flags.flagged && !e.flags.answered);
        assert!(e.has_attachments);
    }

    #[test]
    fn view() {
        let v = message_view(RAW).unwrap();
        assert_eq!(v.text.trim(), "Body");
        assert_eq!(v.from, "Ana <ana@example.com>");
        assert_eq!(v.attachments.len(), 1);
        assert!(v.attachments[0].is_image());
        assert_eq!(v.attachments[0].name, "x.png");
    }
}

#[cfg(test)]
mod thread_header_tests {
    use super::*;

    #[test]
    fn message_ids_for_threads() {
        let raw = b"From: Ana <ana@x.org>\r\nSubject: Re: Plan\r\nMessage-ID: <c@x.org>\r\nIn-Reply-To: <b@x.org>\r\nReferences: <a@x.org>\r\n <b@x.org>\r\n\r\n";
        let e = envelope_from_headers(MessageId(1), raw, 10);
        assert_eq!(e.message_id, "c@x.org");
        assert_eq!(e.parents, ["a@x.org", "b@x.org"], "References first, In-Reply-To not repeated");
        let e = envelope_from_headers(MessageId(2), b"Subject: x\r\n\r\n", 1);
        assert!(e.message_id.is_empty() && e.parents.is_empty());
    }
}
