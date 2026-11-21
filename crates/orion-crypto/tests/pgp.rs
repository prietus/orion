//! Tests against real GnuPG in a temporary GNUPGHOME. Skipped when gpg is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use orion_core::model::{CryptoProblem, SigStatus, Trust};
use orion_crypto::{open_message, Gpg};

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn gpg(&self) -> Gpg {
        let mut g = Gpg::new("gpg");
        g.home = Some(self.path().to_path_buf());
        g
    }

    fn cmd(&self, args: &[&str]) -> String {
        let out = Command::new("gpg").env("GNUPGHOME", self.path()).args(["--batch", "--quiet"]).args(args).output().expect("gpg");
        assert!(out.status.success(), "gpg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn new_key(&self, uid: &str, pass: Option<&str>) {
        let p = pass.unwrap_or("");
        self.cmd(&["--pinentry-mode", "loopback", "--passphrase", p, "--quick-gen-key", uid, "ed25519", "sign", "never"]);
        let fpr = self.fpr(uid);
        self.cmd(&["--pinentry-mode", "loopback", "--passphrase", p, "--quick-add-key", &fpr, "cv25519", "encr", "never"]);
    }

    fn fpr(&self, uid: &str) -> String {
        let out = self.cmd(&["--with-colons", "--list-keys", uid]);
        out.lines().find(|l| l.starts_with("fpr:")).unwrap().split(':').nth(9).unwrap().to_string()
    }

    /// Forgets cached passphrases (like unplugging the YubiKey).
    fn forget(&self) {
        let _ = Command::new("gpgconf").env("GNUPGHOME", self.path()).args(["--kill", "gpg-agent"]).status();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf").env("GNUPGHOME", self.path()).args(["--kill", "all"]).status();
    }
}

fn setup() -> Option<(Home, Home)> {
    Command::new("gpg").arg("--version").output().ok()?;
    let mk = || {
        let dir = tempfile::Builder::new().prefix("orion-gnupg-").tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        Home { dir }
    };
    let home = mk();
    home.new_key("Ana <ana@example.com>", Some("1234"));
    home.new_key("Bob <bob@example.com>", None);
    // Eve signs from another keyring: her public key is not in ours.
    let eve = mk();
    eve.new_key("Eve <eve@example.com>", None);
    Some((home, eve))
}

fn pgp_mime_encrypted(ciphertext: &[u8], subject: &str) -> Vec<u8> {
    let mut m = format!(
        "From: Bob <bob@example.com>\nTo: Ana <ana@example.com>\nSubject: {subject}\nDate: Thu, 20 Nov 2025 10:00:00 +0100\n\
         MIME-Version: 1.0\nContent-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"ENC\"\n\n\
         --ENC\nContent-Type: application/pgp-encrypted\n\nVersion: 1\n\n--ENC\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\n\n"
    )
    .into_bytes();
    m.extend_from_slice(ciphertext);
    m.extend_from_slice(b"\n--ENC--\n");
    m
}

fn pgp_mime_signed(entity: &str, signature: &[u8]) -> Vec<u8> {
    let mut m = format!(
        "From: Bob <bob@example.com>\nTo: Ana <ana@example.com>\nSubject: Signed\nDate: Thu, 20 Nov 2025 10:00:00 +0100\n\
         MIME-Version: 1.0\nContent-Type: multipart/signed; micalg=pgp-sha512; protocol=\"application/pgp-signature\"; boundary=\"SIG\"\n\n\
         --SIG\n{entity}\n--SIG\nContent-Type: application/pgp-signature; name=\"signature.asc\"\n\n"
    )
    .into_bytes();
    m.extend_from_slice(signature);
    m.extend_from_slice(b"\n--SIG--\n");
    m
}

#[test]
fn pgp_end_to_end() {
    let Some((home, eve)) = setup() else {
        eprintln!("gpg not available: test skipped");
        return;
    };
    let gpg = home.gpg();
    let entity = "Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: 8bit\n\nHi Ana, this is secret. ñ€";

    // ---- PGP/MIME encrypted to Ana (passphrase-protected key = "PIN"), signed by Bob.
    let inner = format!("Subject: Protected subject\n{entity}");
    let ct = gpg.encrypt(inner.as_bytes(), &["ana@example.com"], Some("bob@example.com"), None).unwrap();
    assert!(ct.starts_with(b"-----BEGIN PGP MESSAGE-----"));
    let msg = pgp_mime_encrypted(&ct, "...");
    home.forget();

    let v = open_message(Some(&gpg), &msg, None).unwrap();
    let r = v.crypto.as_ref().unwrap();
    assert!(r.encrypted && !r.decrypted);
    match &r.problem {
        Some(CryptoProblem::NeedPin { prompt }) => assert!(prompt.starts_with("passphrase for key"), "{prompt}"),
        other => panic!("expected NeedPin: {other:?}"),
    }
    assert!(v.text.is_empty() && v.attachments.is_empty(), "nothing from the encrypted part is shown as content");

    let v = open_message(Some(&gpg), &msg, Some("0000")).unwrap();
    assert!(matches!(v.crypto.as_ref().unwrap().problem, Some(CryptoProblem::BadPin { .. })));

    let v = open_message(Some(&gpg), &msg, Some("1234")).unwrap();
    let r = v.crypto.as_ref().unwrap();
    assert!(r.decrypted && r.problem.is_none(), "{:?}", r.problem);
    assert_eq!(v.subject, "Protected subject");
    assert_eq!(v.from, "Bob <bob@example.com>");
    assert!(v.text.contains("Hi Ana, this is secret. ñ€"));
    assert_eq!(r.signatures.len(), 1);
    assert_eq!(r.signatures[0].status, SigStatus::Good);
    assert_eq!(r.signatures[0].signer, "Bob <bob@example.com>");
    assert_eq!(r.signatures[0].trust, Trust::Ultimate);

    // PIN cached by the agent: no longer asked for.
    let v = open_message(Some(&gpg), &msg, None).unwrap();
    assert!(v.crypto.as_ref().unwrap().decrypted);

    // ---- multipart/signed by Bob, with LF line endings as in an mbox.
    let sig = gpg.sign_detached(&orion_crypto::mime::canonical_crlf(entity.as_bytes()), "bob@example.com", None).unwrap();
    let msg = pgp_mime_signed(entity, &sig);
    let v = open_message(Some(&gpg), &msg, None).unwrap();
    let r = v.crypto.as_ref().unwrap();
    assert!(!r.encrypted);
    assert_eq!(r.signatures.iter().map(|s| s.status).collect::<Vec<_>>(), [SigStatus::Good], "{r:?}");
    assert!(v.text.contains("this is secret"));
    assert!(v.attachments.iter().all(|a| a.mime != "application/pgp-signature"));

    // Tampered → bad signature.
    let tampered = pgp_mime_signed(&entity.replace("secret", "SECRET"), &sig);
    let v = open_message(Some(&gpg), &tampered, None).unwrap();
    assert_eq!(v.crypto.unwrap().signatures[0].status, SigStatus::Bad);

    // Signed by Eve (unknown key).
    let eve_sig = eve.gpg().sign_detached(&orion_crypto::mime::canonical_crlf(entity.as_bytes()), "eve@example.com", None).unwrap();
    let v = open_message(Some(&gpg), &pgp_mime_signed(entity, &eve_sig), None).unwrap();
    let s = &v.crypto.unwrap().signatures[0];
    assert_eq!(s.status, SigStatus::UnknownKey);
    assert_eq!(s.fingerprint.as_deref(), Some(eve.fpr("eve@example.com").as_str()));

    // ---- Inline PGP: encrypted to Bob (no passphrase) and clearsigned.
    let ct = gpg.encrypt("secret line ñ".as_bytes(), &["bob@example.com"], None, None).unwrap();
    let msg = format!(
        "From: Ana <ana@example.com>\nTo: bob@example.com\nSubject: Inline\nContent-Type: text/plain; charset=utf-8\n\nBefore\n{}\nAfter\n",
        String::from_utf8(ct).unwrap().trim_end()
    );
    let v = open_message(Some(&gpg), msg.as_bytes(), None).unwrap();
    assert_eq!(v.text.trim(), "Before\nsecret line ñ\nAfter");
    assert_eq!(v.crypto.as_ref().unwrap().format, "inline PGP");

    let clear = gpg.run(&["--clearsign", "--local-user", "bob@example.com"], "signed text\n".as_bytes(), None).unwrap().stdout;
    let msg = format!("From: Bob <bob@example.com>\nSubject: Clearsigned\n\n{}", String::from_utf8(clear).unwrap());
    let v = open_message(Some(&gpg), msg.as_bytes(), None).unwrap();
    assert_eq!(v.text.trim(), "signed text");
    assert_eq!(v.crypto.unwrap().signatures[0].status, SigStatus::Good);

    // ---- Without the secret key (encrypted only to Eve).
    let eve_pub = eve.cmd(&["--armor", "--export", "eve@example.com"]);
    let tmp = home.path().join("eve.asc");
    std::fs::write(&tmp, eve_pub).unwrap();
    home.cmd(&["--import", tmp.to_str().unwrap()]);
    let ct = gpg.run(&["--armor", "--trust-model", "always", "--recipient", "eve@example.com", "--encrypt"], b"para Eve", None).unwrap().stdout;
    let v = open_message(Some(&gpg), &pgp_mime_encrypted(&ct, "For Eve"), None).unwrap();
    assert!(matches!(v.crypto.unwrap().problem, Some(CryptoProblem::NoSecretKey(ref r)) if r.len() == 1));

    // ---- Ordinary message: no crypto report.
    let v = open_message(Some(&gpg), b"From: a@b.c\nSubject: plain\n\nhello\n", None).unwrap();
    assert!(v.crypto.is_none());
}

#[test]
fn gpg_missing() {
    let gpg = Gpg::new("/nonexistent/gpg");
    let msg = pgp_mime_encrypted(b"-----BEGIN PGP MESSAGE-----\n\nxx\n-----END PGP MESSAGE-----\n", "x");
    let v = open_message(Some(&gpg), &msg, None).unwrap();
    assert_eq!(v.crypto.unwrap().problem, Some(CryptoProblem::GpgMissing));
    let _ = PathBuf::new();
}

#[test]
fn outgoing_round_trip() {
    use orion_core::compose::Draft;
    use orion_core::model::Attachment;
    use orion_crypto::outgoing::{build, OutgoingError};

    let Some((home, _eve)) = setup() else {
        eprintln!("gpg not available: test skipped");
        return;
    };
    let gpg = home.gpg();
    let draft = Draft {
        from: "Bob <bob@example.com>".into(),
        to: vec!["Ana <ana@example.com>".into()],
        subject: "Secret plans ñ".into(),
        body: "Meet at noon.\n-- \nBob\n".into(),
        attachments: vec![Attachment { name: "map.png".into(), mime: "image/png".into(), inline: false, content_id: None, data: vec![137, 80, 78, 71, 0, 255] }],
        ..Default::default()
    };

    // Signed only (Bob's key has no passphrase).
    let raw = build(Some(&gpg), &Draft { sign: true, ..draft.clone() }, None).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("multipart/signed") && text.contains("protocol=\"application/pgp-signature\""));
    assert!(text.contains("micalg=\"pgp-sha512\"") || text.contains("micalg=pgp-sha512") || text.contains("micalg=\"pgp-sha256\""), "micalg from gpg: {}", &text[..text.find("\r\n\r\n").unwrap()]);
    let v = open_message(Some(&gpg), &raw, None).unwrap();
    let r = v.crypto.unwrap();
    assert_eq!(r.signatures.iter().map(|s| s.status).collect::<Vec<_>>(), [SigStatus::Good], "{r:?}");
    assert_eq!(v.subject, "Secret plans ñ");
    assert!(v.text.contains("Meet at noon."));
    assert_eq!(v.attachments.len(), 1, "signature.asc hidden, map.png kept");
    assert_eq!(v.attachments[0].data, [137, 80, 78, 71, 0, 255]);

    // Tampering with the signed part is detected.
    let tampered = String::from_utf8(raw.clone()).unwrap().replace("Meet at noon.", "Meet at nine.");
    assert_ne!(tampered.as_bytes(), &raw[..], "the text was really changed");
    let v = open_message(Some(&gpg), tampered.as_bytes(), None).unwrap();
    let r = v.crypto.unwrap();
    assert_eq!(r.signatures.first().map(|s| s.status), Some(SigStatus::Bad), "{r:?}");

    // Signed + encrypted to Ana (and to Bob himself).
    let raw = build(Some(&gpg), &Draft { sign: true, encrypt: true, ..draft.clone() }, None).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("multipart/encrypted") && text.contains("Subject: ...") && !text.contains("Secret plans"), "subject protected");
    assert!(!text.contains("Meet at noon"));
    home.forget();
    let v = open_message(Some(&gpg), &raw, Some("1234")).unwrap(); // Ana's passphrase
    let r = v.crypto.as_ref().unwrap();
    assert!(r.decrypted, "{:?}", r.problem);
    assert_eq!(v.subject, "Secret plans ñ", "protected subject restored");
    assert!(v.text.contains("Meet at noon."));
    assert_eq!(r.signatures[0].status, SigStatus::Good);
    assert_eq!(r.recipients.len(), 2, "encrypted to the recipient and to the sender");

    // Missing keys are named.
    let to_carol = Draft { encrypt: true, to: vec!["carol@nowhere.example".into(), "Ana <ana@example.com>".into()], ..draft.clone() };
    assert_eq!(build(Some(&gpg), &to_carol, None).unwrap_err(), OutgoingError::MissingKeys(vec!["carol@nowhere.example".into()]));

    // Signing with Ana's protected key asks for the PIN first.
    let as_ana = Draft { from: "Ana <ana@example.com>".into(), to: vec!["bob@example.com".into()], sign: true, ..draft };
    home.forget();
    assert_eq!(build(Some(&gpg), &as_ana, None).unwrap_err(), OutgoingError::NeedPin);
    assert_eq!(build(Some(&gpg), &as_ana, Some("0000")).unwrap_err(), OutgoingError::BadPin);
    let raw = build(Some(&gpg), &as_ana, Some("1234")).unwrap();
    let r = open_message(Some(&gpg), &raw, None).unwrap().crypto.unwrap();
    assert_eq!(r.signatures.first().map(|s| s.status), Some(SigStatus::Good), "{r:?}\n{}", String::from_utf8_lossy(&raw));
}

#[test]
fn uncertified_keys_need_acceptance() {
    use orion_core::compose::Draft;
    use orion_crypto::accepted::AcceptedKeys;
    use orion_crypto::outgoing::{build_with, OutgoingError};

    let Some((home, eve)) = setup() else {
        eprintln!("gpg not available: test skipped");
        return;
    };
    let gpg = home.gpg();
    // Eve's key arrives from outside (like a keyserver): present but not certified.
    let tmp = home.path().join("eve.asc");
    std::fs::write(&tmp, eve.cmd(&["--armor", "--export", "eve@example.com"])).unwrap();
    home.cmd(&["--import", tmp.to_str().unwrap()]);
    let eve_fpr = eve.fpr("eve@example.com");
    let key = gpg.encryption_key("eve@example.com").unwrap();
    assert_eq!(key.fingerprint, eve_fpr);
    assert!(!key.certified());
    assert!(gpg.encryption_key("bob@example.com").unwrap().certified(), "own keys are ultimately trusted");

    let draft = Draft { from: "Bob <bob@example.com>".into(), to: vec!["eve@example.com".into()], subject: "s".into(), body: "for Eve\n".into(), encrypt: true, ..Default::default() };
    let none = AcceptedKeys::default();
    match build_with(Some(&gpg), &draft, None, &none).unwrap_err() {
        OutgoingError::UntrustedKeys(k) => {
            assert_eq!(k.len(), 1);
            assert_eq!((k[0].0.as_str(), k[0].1.fingerprint.as_str()), ("eve@example.com", eve_fpr.as_str()));
        }
        other => panic!("expected UntrustedKeys, got {other:?}"),
    }

    // Accepting a *different* fingerprint for that address does not help.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("accepted-keys");
    AcceptedKeys::accept(&path, "eve@example.com", "0000000000000000000000000000000000000000").unwrap();
    assert!(matches!(build_with(Some(&gpg), &draft, None, &AcceptedKeys::load(&path)), Err(OutgoingError::UntrustedKeys(_))));

    // The right one: encrypted to exactly that key, and Eve can read it.
    AcceptedKeys::accept(&path, "Eve@Example.com", &eve_fpr.to_ascii_lowercase()).unwrap();
    let raw = build_with(Some(&gpg), &draft, None, &AcceptedKeys::load(&path)).unwrap();
    let v = open_message(Some(&eve.gpg()), &raw, None).unwrap();
    assert!(v.crypto.unwrap().decrypted);
    assert!(v.text.contains("for Eve"));
}

#[test]
fn sender_keys_from_autocrypt_and_keyserver() {
    use orion_core::compose::Draft;
    use orion_core::model::KeySource;
    use orion_crypto::accepted::AcceptedKeys;
    use orion_crypto::outgoing::build_with;
    use std::io::BufRead;

    let Some((home, eve)) = setup() else {
        eprintln!("gpg not available: test skipped");
        return;
    };
    let gpg = home.gpg();
    let eve_fpr = eve.fpr("eve@example.com");
    let draft = Draft {
        from: "Eve <eve@example.com>".into(),
        to: vec!["bob@example.com".into()],
        subject: "hello".into(),
        body: "Hi Bob\n".into(),
        sign: true,
        autocrypt: Some(String::new()),
        ..Default::default()
    };

    // 1. Eve writes with Autocrypt: her key travels in the header.
    let raw = build_with(Some(&eve.gpg()), &draft, None, &AcceptedKeys::default()).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Autocrypt: addr=eve@example.com; prefer-encrypt=mutual; keydata="), "{}", &text[..600.min(text.len())]);
    let v = open_message(Some(&gpg), &raw, None).unwrap();
    assert_eq!(v.crypto.as_ref().unwrap().signatures[0].status, SigStatus::UnknownKey);
    let offer = v.key_offer.clone().expect("the key is offered");
    assert_eq!(offer.fingerprint, eve_fpr);
    assert!(offer.uid.contains("eve@example.com") && matches!(offer.source, KeySource::Autocrypt(_)));
    let KeySource::Autocrypt(data) = &offer.source else { unreachable!() };
    assert!(gpg.import_keys(data).unwrap().iter().any(|f| f == &eve_fpr));
    let v = open_message(Some(&gpg), &raw, None).unwrap();
    assert_eq!(v.crypto.unwrap().signatures[0].status, SigStatus::Good, "verifiable once imported");

    // 2. An Autocrypt header whose addr is not the sender's is ignored (forgery).
    let forged = text.replace("From: \"Eve\" <eve@example.com>", "From: \"Bank\" <bank@example.com>").replace("From: Eve <eve@example.com>", "From: Bank <bank@example.com>");
    let fresh = home_without_eve();
    let v = open_message(Some(&fresh.gpg()), forged.as_bytes(), None).unwrap();
    assert!(!matches!(v.key_offer, Some(ref o) if matches!(o.source, KeySource::Autocrypt(_))), "{:?}", v.key_offer);

    // 3. Signed without Autocrypt: the signature's fingerprint is fetched from a keyserver.
    let no_ac = Draft { autocrypt: None, ..draft };
    let raw = build_with(Some(&eve.gpg()), &no_ac, None, &AcceptedKeys::default()).unwrap();
    let fresh = home_without_eve();
    let v = open_message(Some(&fresh.gpg()), &raw, None).unwrap();
    let offer = v.key_offer.expect("keyserver offer");
    assert_eq!((offer.fingerprint.as_str(), offer.source.clone()), (eve_fpr.as_str(), KeySource::Keyserver));
    let keydir = tempfile::tempdir().unwrap();
    std::fs::write(keydir.path().join(format!("{eve_fpr}.asc")), eve.cmd(&["--armor", "--export", "eve@example.com"])).unwrap();
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_hkp.py"))
        .arg(keydir.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let ks = format!("hkp://127.0.0.1:{}", line.split_whitespace().nth(1).unwrap());
    let got = fresh.gpg().recv_key(&eve_fpr, &[ks]);
    let _ = child.kill();
    let _ = child.wait();
    assert!(got.unwrap().iter().any(|f| f == &eve_fpr));
    let v = open_message(Some(&fresh.gpg()), &raw, None).unwrap();
    assert_eq!(v.crypto.unwrap().signatures[0].status, SigStatus::Good);
}

/// A keyring with Ana and Bob only (Eve unknown).
fn home_without_eve() -> Home {
    let dir = tempfile::Builder::new().prefix("orion-gnupg-").tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let h = Home { dir };
    h.new_key("Bob <bob@example.com>", None);
    h
}
