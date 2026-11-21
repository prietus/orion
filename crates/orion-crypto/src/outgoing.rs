//! Building outgoing PGP/MIME messages (RFC 3156): signed, encrypted, or both.

use mail_builder::headers::content_type::ContentType;
use mail_builder::mime::MimePart;

use orion_core::compose::{self, body_part, recipients, Draft};

use crate::accepted::AcceptedKeys;
use crate::gpg::{Gpg, GpgError, KeyInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutgoingError {
    /// The signing key needs its PIN/passphrase.
    NeedPin,
    BadPin,
    /// Recipients (or the sender) without a usable public key.
    MissingKeys(Vec<String>),
    /// Keys found but neither certified nor accepted by the user:
    /// (address, key) to show with its fingerprint.
    UntrustedKeys(Vec<(String, KeyInfo)>),
    Gpg(String),
}

impl std::fmt::Display for OutgoingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutgoingError::NeedPin => write!(f, "the signing key needs its PIN or passphrase"),
            OutgoingError::BadPin => write!(f, "wrong PIN or passphrase"),
            OutgoingError::MissingKeys(k) => write!(f, "no public key for: {}", k.join(", ")),
            OutgoingError::UntrustedKeys(k) => {
                write!(f, "keys not verified yet: {}", k.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>().join(", "))
            }
            OutgoingError::Gpg(m) => write!(f, "{m}"),
        }
    }
}

fn map(e: GpgError) -> OutgoingError {
    match e {
        GpgError::NeedPin => OutgoingError::NeedPin,
        GpgError::BadPin => OutgoingError::BadPin,
        other => OutgoingError::Gpg(other.to_string()),
    }
}

fn entity_bytes(part: MimePart<'static>) -> Vec<u8> {
    let mut out = Vec::new();
    part.write_part(&mut out);
    out
}

/// The raw message to send, applying the draft's `sign`/`encrypt` choices.
pub fn build(gpg: Option<&Gpg>, d: &Draft, pin: Option<&str>) -> Result<Vec<u8>, OutgoingError> {
    build_with(gpg, d, pin, &AcceptedKeys::load(&crate::accepted::default_path()))
}

pub fn build_with(gpg: Option<&Gpg>, d: &Draft, pin: Option<&str>, accepted: &AcceptedKeys) -> Result<Vec<u8>, OutgoingError> {
    // Autocrypt: attach our public key when the caller asked for it (draft.autocrypt
    // holds Some("") as a request) and we have one.
    let mut owned;
    let d = if d.autocrypt.as_deref() == Some("") {
        owned = d.clone();
        let me = compose::addr_spec(&d.from);
        let signer = d.signing_key.clone().unwrap_or_else(|| me.clone());
        let spec = if signer.contains('@') { format!("<{signer}>") } else { signer };
        owned.autocrypt = gpg.and_then(|g| g.export_minimal(&spec)).map(|key| crate::autocrypt::header_value(&me, &key));
        &owned
    } else {
        d
    };
    if !d.sign && !d.encrypt {
        return Ok(compose::build_plain(d));
    }
    let gpg = gpg.ok_or_else(|| OutgoingError::Gpg("OpenPGP is disabled in the configuration ([pgp] enabled = false)".into()))?;
    let me = compose::addr_spec(&d.from);
    // Who signs, and whose key our own copy is encrypted to.
    let signer = d.signing_key.clone().unwrap_or_else(|| me.clone());

    if d.encrypt {
        // Also to ourselves, or the copy in Sent could never be read again.
        let mut to = recipients(d);
        if !to.iter().any(|a| a.eq_ignore_ascii_case(&signer)) {
            to.push(signer.clone());
        }
        // Each address must map to a key that is certified in the keyring or
        // that the user accepted after comparing its fingerprint.
        let (mut missing, mut untrusted, mut fingerprints, mut trust_listed) = (Vec::new(), Vec::new(), Vec::new(), false);
        for addr in &to {
            match gpg.encryption_key(addr) {
                None => missing.push(addr.clone()),
                Some(k) if k.certified() => fingerprints.push(k.fingerprint),
                Some(k) if accepted.contains(addr, &k.fingerprint) => {
                    trust_listed = true;
                    fingerprints.push(k.fingerprint);
                }
                Some(k) => untrusted.push((addr.clone(), k)),
            }
        }
        if !missing.is_empty() {
            return Err(OutgoingError::MissingKeys(missing));
        }
        if !untrusted.is_empty() {
            return Err(OutgoingError::UntrustedKeys(untrusted));
        }
        // The real subject travels inside; outside it is "..." (protected headers).
        let inner = entity_bytes(body_part(d, true));
        let armored = gpg.encrypt_to_keys(&inner, &fingerprints, trust_listed, d.sign.then_some(signer.as_str()), pin).map_err(map)?;
        let body = MimePart::new(
            ContentType::new("multipart/encrypted").attribute("protocol", "application/pgp-encrypted"),
            vec![
                MimePart::new("application/pgp-encrypted", "Version: 1\r\n").header("Content-Description", mail_builder::headers::text::Text::new("PGP/MIME version identification")),
                MimePart::new(ContentType::new("application/octet-stream").attribute("name", "encrypted.asc"), String::from_utf8_lossy(&armored).into_owned())
                    .inline()
                    .header("Content-Description", mail_builder::headers::text::Text::new("OpenPGP encrypted message")),
            ],
        );
        return Ok(compose::message(d, body, Some("...")));
    }

    // Signed only: the signature covers the exact bytes of the inner entity.
    let inner = entity_bytes(body_part(d, false));
    let (sig, micalg) = gpg.sign_detached_micalg(&inner, &signer, pin).map_err(map)?;
    let body = MimePart::new(
        ContentType::new("multipart/signed").attribute("micalg", micalg).attribute("protocol", "application/pgp-signature"),
        vec![
            MimePart::raw(inner),
            MimePart::new(ContentType::new("application/pgp-signature").attribute("name", "signature.asc"), String::from_utf8_lossy(&sig).into_owned())
                .header("Content-Description", mail_builder::headers::text::Text::new("OpenPGP digital signature"))
                .attachment("signature.asc"),
        ],
    );
    Ok(compose::message(d, body, None))
}
