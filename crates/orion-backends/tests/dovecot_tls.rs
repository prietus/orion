//! TLS and STARTTLS against a real Dovecot with a throwaway CA. Its own test
//! binary because it sets SSL_CERT_FILE for the whole process.

mod common;

use std::collections::HashSet;

use orion_backends::imap::ImapBackend;
use orion_backends::pop3::Pop3Backend;
use orion_core::config::TlsMode;
use orion_core::{Backend, Error};

use common::{Dovecot, PASSWORD};

#[test]
fn tls_and_starttls_against_dovecot() {
    let Some(dv) = Dovecot::start_tls() else {
        eprintln!("dovecot/openssl not installed: test skipped");
        return;
    };
    // Trust the test CA in this process only (the platform verifier reads SSL_CERT_FILE).
    // SAFETY: set before any TLS use, and this test binary runs a single test.
    unsafe { std::env::set_var("SSL_CERT_FILE", dv.ca_file.as_ref().unwrap()) };
    dv.deliver(None, "Over TLS", "secret body");

    let cases = [
        ("IMAP implicit TLS", TlsMode::Tls, dv.imaps_port, true),
        ("IMAP STARTTLS", TlsMode::Starttls, dv.imap_port, true),
        ("POP3 implicit TLS", TlsMode::Tls, dv.pop3s_port, false),
        ("POP3 STLS", TlsMode::Starttls, dv.pop3_port, false),
    ];
    for (name, tls, port, imap) in cases {
        let cfg = orion_core::config::ImapConfig { tls, port: Some(port), ..dv.imap_config(PASSWORD) };
        let mut b: Box<dyn Backend> = if imap { Box::new(ImapBackend::new(cfg).unwrap()) } else { Box::new(Pop3Backend::new(cfg)) };
        let mbs = b.mailboxes().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(mbs[0].total, 1, "{name}");
        let envs = b.envelopes("INBOX").unwrap();
        let raw = b.fetch_raw("INBOX", envs[0].id).unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("secret body"), "{name}");
        eprintln!("{name}: ok");
    }

    // The certificate is for 127.0.0.1 only: "localhost" must be rejected.
    let cfg = orion_core::config::ImapConfig {
        host: "localhost".into(),
        tls: TlsMode::Tls,
        port: Some(dv.imaps_port),
        ..dv.imap_config(PASSWORD)
    };
    let err = ImapBackend::new(cfg).unwrap().mailboxes().unwrap_err();
    assert!(matches!(&err, Error::Network(m) if m.to_lowercase().contains("certificate")), "{err:?}");
    let cfg = orion_core::config::ImapConfig { host: "localhost".into(), tls: TlsMode::Tls, port: Some(dv.pop3s_port), ..dv.pop3_config(PASSWORD) };
    let err = Pop3Backend::new(cfg).sync("INBOX", None, &HashSet::new(), &mut |_| {}).unwrap_err();
    assert!(matches!(&err, Error::Network(m) if m.to_lowercase().contains("certificate")), "{err:?}");
    eprintln!("hostname mismatch rejected: {err}");
}
