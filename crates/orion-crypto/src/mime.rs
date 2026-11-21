//! OpenPGP in messages: PGP/MIME (RFC 3156) and inline PGP.

use mail_parser::{MessageParser, MessagePart};

use orion_core::model::{CryptoProblem, CryptoReport, KeyOffer, KeySource, MessageView, SigStatus};
use orion_core::parse::message_view;

use crate::gpg::{describe_card, Gpg, GpgError};

enum Kind {
    Plain,
    Encrypted(Vec<u8>),
    Signed { signed: Vec<u8>, signature: Vec<u8> },
}

fn content_type(p: &MessagePart<'_>) -> (String, Option<String>) {
    match p.content_type() {
        Some(ct) => (
            format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or_default()).to_ascii_lowercase(),
            ct.attribute("protocol").map(|s| s.to_ascii_lowercase()),
        ),
        None => ("text/plain".into(), None),
    }
}

fn classify(raw: &[u8]) -> Kind {
    let Some(msg) = MessageParser::new().parse(raw) else { return Kind::Plain };
    let root = msg.root().root_part();
    let (ct, protocol) = content_type(&root);
    let children: Vec<MessagePart<'_>> = root.children().collect();
    match (ct.as_str(), protocol.as_deref()) {
        ("multipart/encrypted", Some("application/pgp-encrypted")) => {
            let body = children
                .iter()
                .find(|c| content_type(c).0 == "application/octet-stream")
                .or(children.get(1))
                .map(|c| c.decoded().into_owned());
            match body {
                Some(b) => Kind::Encrypted(b),
                None => Kind::Plain,
            }
        }
        ("multipart/signed", Some("application/pgp-signature")) if children.len() >= 2 => Kind::Signed {
            signed: canonical_crlf(children[0].raw()),
            signature: children[1].decoded().into_owned(),
        },
        _ => Kind::Plain,
    }
}

/// RFC 3156: signed data is compared with CRLF line endings. In mbox files they are usually LF.
pub fn canonical_crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 30);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// Opens a message: decrypts and/or verifies it if it uses OpenPGP. Without
/// `gpg` (PGP disabled) it behaves like `message_view`.
pub fn open_message(gpg: Option<&Gpg>, raw: &[u8], pin: Option<&str>) -> Option<MessageView> {
    let Some(g) = gpg else { return message_view(raw) };
    let mut view = open_inner(g, raw, pin)?;
    view.key_offer = key_offer(g, &view);
    Some(view)
}

/// Whether the message brings the sender's key (Autocrypt header, attached
/// key, or the fingerprint of an unknown signature) that is not yet usable
/// for replying. Never imports anything by itself: the user decides.
fn key_offer(gpg: &Gpg, view: &MessageView) -> Option<KeyOffer> {
    let addr = view.from.rsplit_once('<').map(|(_, a)| a.trim_end_matches('>').trim().to_string()).unwrap_or_else(|| view.from.trim().to_string());
    if !addr.contains('@') {
        return None;
    }
    let accepted = crate::accepted::AcceptedKeys::load(&crate::accepted::default_path());
    let usable = |fpr: &str| {
        gpg.encryption_key(&addr).is_some_and(|k| k.fingerprint.eq_ignore_ascii_case(fpr) && (k.certified() || accepted.contains(&addr, fpr)))
    };
    let claims_addr = |uid: &str| uid.to_ascii_lowercase().contains(&addr.to_ascii_lowercase());
    // 1. Autocrypt header, 2. attached key: only keys that claim the sender's address.
    let embedded = crate::autocrypt::parse(&view.raw_headers, &addr)
        .map(KeySource::Autocrypt)
        .into_iter()
        .chain(view.attachments.iter().filter(|a| a.mime == "application/pgp-keys").map(|a| KeySource::Attachment(a.data.clone())));
    for source in embedded {
        let data = match &source {
            KeySource::Autocrypt(d) | KeySource::Attachment(d) => d.clone(),
            KeySource::Keyserver => continue,
        };
        if let Some((fpr, uid)) = gpg.inspect_keys(&data).into_iter().find(|(_, uid)| claims_addr(uid)) {
            return (!usable(&fpr)).then_some(KeyOffer { addr: addr.clone(), fingerprint: fpr, uid, source });
        }
    }
    // 3. Signed with a key we do not have: its exact fingerprint is in the signature.
    let sig = view.crypto.as_ref()?.signatures.iter().find(|s| s.status == SigStatus::UnknownKey)?;
    let fpr = sig.fingerprint.clone()?;
    Some(KeyOffer { addr, fingerprint: fpr, uid: String::new(), source: KeySource::Keyserver })
}

fn open_inner(gpg: &Gpg, raw: &[u8], pin: Option<&str>) -> Option<MessageView> {
    match classify(raw) {
        Kind::Plain => {
            let mut view = message_view(raw)?;
            inline_pgp(gpg, &mut view, pin);
            Some(view)
        }
        Kind::Signed { signed, signature } => {
            let mut view = message_view(raw)?;
            view.attachments.retain(|a| a.mime != "application/pgp-signature");
            let mut report = CryptoReport { format: "PGP/MIME", ..Default::default() };
            match gpg.verify_detached(&signed, &signature) {
                Ok(sigs) => report.signatures = sigs,
                Err(e) => report.problem = Some(problem(gpg, e, &[])),
            }
            view.crypto = Some(report);
            Some(view)
        }
        Kind::Encrypted(ciphertext) => {
            let outer = message_view(raw)?;
            let mut report = CryptoReport { encrypted: true, format: "PGP/MIME", ..Default::default() };
            match gpg.decrypt(&ciphertext, pin) {
                Ok(d) => {
                    report.decrypted = true;
                    report.key_location = card_location(gpg, &d.recipients);
                    report.recipients = d.recipients;
                    report.signatures = d.signatures;
                    // The decrypted content is a MIME entity; it may itself be signed.
                    let mut inner = match classify(&d.plaintext) {
                        Kind::Signed { signed, signature } => {
                            let mut v = message_view(&d.plaintext)?;
                            v.attachments.retain(|a| a.mime != "application/pgp-signature");
                            match gpg.verify_detached(&signed, &signature) {
                                Ok(s) => report.signatures.extend(s),
                                Err(e) => report.problem = Some(problem(gpg, e, &[])),
                            }
                            v
                        }
                        _ => message_view(&d.plaintext)?,
                    };
                    // Protected headers: the inner subject wins if present.
                    let subject = if inner.subject != "(no subject)" && !inner.subject.is_empty() { inner.subject.clone() } else { outer.subject.clone() };
                    inner.subject = subject;
                    inner.from = outer.from.clone();
                    inner.to = outer.to.clone();
                    inner.cc = outer.cc.clone();
                    inner.reply_to = outer.reply_to.clone();
                    inner.date = outer.date.clone();
                    inner.message_id = outer.message_id.clone();
                    inner.raw_headers = outer.raw_headers.clone();
                    inner.crypto = Some(report);
                    Some(inner)
                }
                Err((e, recipients)) => {
                    report.problem = Some(problem(gpg, e, &recipients));
                    report.key_location = card_location(gpg, &recipients);
                    report.recipients = recipients;
                    let mut view = outer;
                    view.text.clear();
                    view.text_doc = orion_html::Doc::from_plain("");
                    view.html = None;
                    view.html_doc = None;
                    view.attachments.clear();
                    view.crypto = Some(report);
                    Some(view)
                }
            }
        }
    }
}

/// Inline PGP: armored blocks inside the plain text.
fn inline_pgp(gpg: &Gpg, view: &mut MessageView, pin: Option<&str>) {
    let (begin, end, encrypted) = if view.text.contains("-----BEGIN PGP MESSAGE-----") {
        ("-----BEGIN PGP MESSAGE-----", "-----END PGP MESSAGE-----", true)
    } else if view.text.contains("-----BEGIN PGP SIGNED MESSAGE-----") {
        ("-----BEGIN PGP SIGNED MESSAGE-----", "-----END PGP SIGNATURE-----", false)
    } else {
        return;
    };
    let Some(start) = view.text.find(begin) else { return };
    let Some(stop) = view.text[start..].find(end).map(|i| start + i + end.len()) else { return };
    let block = view.text[start..stop].to_string();
    let mut report = CryptoReport { encrypted, format: "inline PGP", ..Default::default() };
    match gpg.decrypt(block.as_bytes(), pin) {
        Ok(d) => {
            report.decrypted = encrypted;
            report.key_location = card_location(gpg, &d.recipients);
            report.recipients = d.recipients;
            report.signatures = d.signatures;
            let plain = String::from_utf8_lossy(&d.plaintext).replace('\r', "");
            view.text.replace_range(start..stop, plain.trim_end());
        }
        Err((e, recipients)) => {
            report.problem = Some(problem(gpg, e, &recipients));
            report.key_location = card_location(gpg, &recipients);
            report.recipients = recipients;
            if encrypted {
                view.text.replace_range(start..stop, "");
            }
        }
    }
    view.text_doc = orion_html::Doc::from_plain(&view.text);
    // With inline PGP the HTML part (if any) is the unprocessed version.
    view.html = None;
    view.html_doc = None;
    view.crypto = Some(report);
}

fn card_location(gpg: &Gpg, recipients: &[String]) -> Option<String> {
    recipients.iter().find_map(|r| gpg.card_serial_for(r)).map(|s| describe_card(&s))
}

/// "Carlos <c@x.org> (key 0123…)" for each recipient key id, plus the card it
/// lives on when gpg knows it.
fn describe_recipients(gpg: &Gpg, recipients: &[String]) -> String {
    let mut parts: Vec<String> = recipients
        .iter()
        .map(|r| match gpg.owner_of(r) {
            Some(owner) => format!("{owner} (key {r})"),
            None => format!("key {r} (not in your keyring)"),
        })
        .collect();
    parts.dedup();
    let mut s = parts.join(", ");
    if let Some(card) = card_location(gpg, recipients) {
        s.push_str(&format!(", on {card}"));
    }
    s
}

/// The connected card and whose key it carries.
fn describe_inserted(gpg: &Gpg, card: &crate::CardInfo) -> String {
    let owner = card
        .fingerprints
        .iter()
        .flatten()
        .find_map(|f| gpg.owner_of(f))
        .or_else(|| (!card.holder.is_empty()).then(|| format!("{} (its public key is not in your keyring)", card.holder)));
    match owner {
        Some(o) => format!("{} holds the key of {o}", card.name()),
        None => format!("{} holds keys that are not in your keyring", card.name()),
    }
}

/// Whether the card's decryption key is one of the message's recipients.
pub fn card_matches(card: &crate::CardInfo, recipients: &[String]) -> bool {
    let Some(enc) = card.fingerprints[1].as_deref() else { return false };
    recipients.iter().any(|r| !r.is_empty() && enc.to_ascii_uppercase().ends_with(&r.to_ascii_uppercase()))
}

fn problem(gpg: &Gpg, e: GpgError, recipients: &[String]) -> CryptoProblem {
    // No key, or "insert the card": if some *other* card is connected, say so,
    // and whose it is, instead of a bare "secret key not available".
    // Before asking for a PIN, make sure the card that holds the key is there:
    // asking for the PIN of an unplugged YubiKey is just confusing.
    let on_card = card_location(gpg, recipients);
    if matches!(e, GpgError::NoSecretKey | GpgError::CardMissing) || (matches!(e, GpgError::NeedPin) && on_card.is_some()) {
        match gpg.card_status() {
            Ok(Some(card)) if !card_matches(&card, recipients) => {
                return CryptoProblem::WrongCard { needed: describe_recipients(gpg, recipients), inserted: describe_inserted(gpg, &card) };
            }
            Ok(None) if on_card.is_some() => return CryptoProblem::CardMissing(on_card.unwrap_or_default()),
            _ => {}
        }
    }
    match e {
        GpgError::NeedPin => CryptoProblem::NeedPin {
            prompt: match card_location(gpg, recipients) {
                Some(card) => format!("PIN for {card}"),
                None => match recipients.first() {
                    Some(k) => format!("passphrase for key {k}"),
                    None => "passphrase".into(),
                },
            },
        },
        GpgError::BadPin => CryptoProblem::BadPin {
            retries: gpg.card_status().ok().flatten().and_then(|c| c.pin_retries[0]),
        },
        GpgError::PinBlocked => CryptoProblem::PinBlocked,
        GpgError::CardMissing => CryptoProblem::CardMissing(card_location(gpg, recipients).unwrap_or_else(|| "the card".into())),
        GpgError::NotConfirmed => CryptoProblem::NotConfirmed,
        GpgError::NoSecretKey => CryptoProblem::NoSecretKey(vec![describe_recipients(gpg, recipients)]),
        GpgError::NotFound(_) => CryptoProblem::GpgMissing,
        other => CryptoProblem::Failed(other.to_string()),
    }
}
