//! Secrets in the system keyring (Secret Service: GNOME Keyring, KWallet...)
//! through `secret-tool`. Secrets travel over stdin/stdout, never argv.
//!
//! Entries carry the attributes `service=orion account=<name> kind=<kind>`.

use std::io::Write;
use std::process::{Command, Stdio};

use orion_core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Password,
    OAuth,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Password => "password",
            Kind::OAuth => "oauth",
        }
    }
}

/// Keyring "service" attribute. Tests use their own to never touch real entries.
pub fn service() -> String {
    std::env::var("ORION_KEYRING_SERVICE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "orion".into())
}

fn tool() -> Command {
    Command::new("secret-tool")
}

fn missing(e: std::io::Error) -> Error {
    match e.kind() {
        std::io::ErrorKind::NotFound => Error::Auth("secret-tool not found: install libsecret (it stores passwords in the system keyring)".into()),
        _ => Error::Auth(format!("secret-tool: {e}")),
    }
}

pub fn store(account: &str, kind: Kind, label: &str, secret: &str) -> Result<()> {
    let mut child = tool()
        .args(["store", &format!("--label={label}"), "service", &service(), "account", account, "kind", kind.as_str()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(missing)?;
    child.stdin.take().expect("stdin").write_all(secret.as_bytes()).map_err(missing)?;
    let out = child.wait_with_output().map_err(missing)?;
    if !out.status.success() {
        return Err(Error::Auth(format!("could not store the secret in the keyring: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(())
}

pub fn lookup(account: &str, kind: Kind) -> Result<Option<String>> {
    let out = tool()
        .args(["lookup", "service", &service(), "account", account, "kind", kind.as_str()])
        .stdin(Stdio::null())
        .output()
        .map_err(missing)?;
    // secret-tool exits 1 with no output when the entry does not exist.
    if !out.status.success() || out.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string()))
}

pub fn clear(account: &str, kind: Kind) -> Result<()> {
    tool()
        .args(["clear", "service", &service(), "account", account, "kind", kind.as_str()])
        .stdin(Stdio::null())
        .output()
        .map_err(missing)?;
    Ok(())
}

/// `password_command` that reads an account password back from the keyring.
/// Account names are restricted to `[A-Za-z0-9._-]`, so no quoting is needed.
pub fn lookup_command(account: &str) -> String {
    debug_assert!(valid_name(account));
    let svc = service();
    format!("secret-tool lookup service {svc} account {account} kind password")
}

/// Account names double as keyring keys and file names: keep them simple.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}
