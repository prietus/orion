//! OAuth2 end to end: device code sign-in against `scripts/fake_oauth.py`,
//! tokens in the real system keyring (under a test-only service name), IMAP
//! XOAUTH2 against `scripts/fake_imap.py`, and refresh-token rotation.
//! Own test binary: it sets process-wide environment variables.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use orion_backends::imap::ImapBackend;
use orion_backends::oauth::{self, Endpoints};
use orion_backends::secrets::{self, Kind};
use orion_core::config::{ImapConfig, OAuthProvider, TlsMode};
use orion_core::Backend;

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn spawn(script: &str) -> Option<(Proc, Vec<String>)> {
    let path = format!("{}/../../scripts/{script}", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new("python3").arg("-I").arg(path).stdout(Stdio::piped()).spawn().ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
    Some((Proc(child), line.split_whitespace().map(str::to_string).collect()))
}

fn keyring_works() -> bool {
    Command::new("secret-tool").arg("--version").output().is_ok() && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
}

#[test]
fn microsoft_device_flow_xoauth2_and_rotation() {
    if !keyring_works() {
        eprintln!("no secret-tool / session bus: test skipped");
        return;
    }
    let Some((_oauth, o)) = spawn("fake_oauth.py") else {
        eprintln!("python3 not available: test skipped");
        return;
    };
    let (_imap, i) = spawn("fake_imap.py").unwrap();
    let oauth_port = &o[1];
    let (imap_port, admin_port): (u16, u16) = (i[1].parse().unwrap(), i[3].parse().unwrap());
    let mut admin = BufReader::new(TcpStream::connect(("127.0.0.1", admin_port)).unwrap());
    let mut imap_admin = |cmd: &str| {
        writeln!(admin.get_mut(), "{cmd}").unwrap();
        let mut r = String::new();
        admin.read_line(&mut r).unwrap();
    };

    let service = format!("orion-test-{}", std::process::id());
    // SAFETY: single test in this binary; set before any other thread reads them.
    unsafe {
        std::env::set_var("ORION_KEYRING_SERVICE", &service);
        std::env::set_var("ORION_OAUTH_TEST_BASE", format!("http://127.0.0.1:{oauth_port}/common/oauth2/v2.0"));
    }
    let account = "outlook-test";

    // 1. Device code sign-in (pending twice, then approved).
    let ep = Endpoints::for_provider(OAuthProvider::Microsoft);
    let dc = oauth::start_device_flow(&ep).unwrap();
    assert_eq!(dc.user_code, "ABCD-1234");
    let mut polls = 0;
    let (tokens, email) = oauth::finish_device_flow(&ep, &dc, &mut |_| polls += 1).unwrap();
    assert_eq!(polls, 3, "two authorization_pending, then success");
    assert_eq!(email, "ana@outlook.com");
    assert_eq!((tokens.access_token.as_str(), tokens.refresh_token.as_str()), ("A1", "R1"));
    oauth::save(account, &tokens).unwrap();
    assert!(secrets::lookup(account, Kind::OAuth).unwrap().unwrap().contains("R1"), "tokens are in the keyring");

    // 2. IMAP with XOAUTH2.
    imap_admin("TOKENS A1");
    let cfg = ImapConfig {
        host: "127.0.0.1".into(),
        port: Some(imap_port),
        tls: TlsMode::None,
        user: email.clone(),
        password: None,
        password_command: None,
        poll_interval: 60,
        cache_bodies: true,
        idle: false,
        oauth: Some(OAuthProvider::Microsoft),
        account: account.into(),
    };
    let mut b = ImapBackend::new(cfg.clone()).unwrap();
    assert!(!b.mailboxes().unwrap().is_empty());
    drop(b);

    // 3. The server stops accepting A1 (revoked early): the client refreshes,
    //    stores the rotated refresh token and reconnects transparently.
    imap_admin("TOKENS A2");
    let mut b = ImapBackend::new(cfg.clone()).unwrap();
    assert!(!b.mailboxes().unwrap().is_empty(), "reconnected with a refreshed token");
    let state: serde_json::Value = ureq::get(&format!("http://127.0.0.1:{oauth_port}/admin/state")).call().unwrap().body_mut().read_json().unwrap();
    assert_eq!(state["refreshes"], 1);
    let stored = oauth::load(account).unwrap().unwrap();
    assert_eq!((stored.access_token.as_str(), stored.refresh_token.as_str()), ("A2", "R2"), "rotated token persisted");

    // 4. A revoked refresh token is a clear auth error (re-login needed).
    oauth::save(account, &oauth::TokenSet { refresh_token: "R1".into(), expires_at: 0, ..stored }).unwrap();
    oauth::invalidate(account);
    let err = oauth::access_token(OAuthProvider::Microsoft, account).unwrap_err();
    assert!(err.to_string().contains("orion account login"), "{err}");

    secrets::clear(account, Kind::OAuth).unwrap();
    assert!(secrets::lookup(account, Kind::OAuth).unwrap().is_none(), "test entries removed from the keyring");
}
