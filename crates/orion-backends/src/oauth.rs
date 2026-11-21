//! OAuth2 for Microsoft (Outlook/Hotmail/Live and Microsoft 365).
//!
//! Sign-in uses the **device code flow** (RFC 8628): Orion shows a short code,
//! the user opens Microsoft's page on any device, signs in and approves with
//! Authenticator. It is the right flow for a terminal (works over SSH too).
//! Same app registration, scopes and token handling as relaymail's
//! `OAuthService` (public client, no secret; `offline_access` for a refresh
//! token; Microsoft rotates refresh tokens on every refresh).
//!
//! Tokens are stored as JSON in the system keyring. Refreshing is serialized
//! across processes (UI + daemon) with a lock file, and the keyring is re-read
//! under the lock, so two processes never burn the same rotating token.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use orion_core::config::OAuthProvider;
use orion_core::{Error, Result};

use crate::secrets::{self, Kind};

/// relaymail's public client id (multi-tenant + personal accounts, public
/// client flows enabled). Shipping it is fine: there is no secret; the flow
/// itself proves the caller.
const MICROSOFT_CLIENT_ID: &str = "a6cad0e7-2633-4f74-a279-00687be080f0";
const MICROSOFT_SCOPES: &str =
    "offline_access openid profile email https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send";

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub device: String,
    pub token: String,
    pub client_id: String,
    pub scopes: String,
}

impl Endpoints {
    pub fn for_provider(p: OAuthProvider) -> Endpoints {
        match p {
            OAuthProvider::Microsoft => {
                let base = test_base().unwrap_or_else(|| "https://login.microsoftonline.com/common/oauth2/v2.0".into());
                Endpoints {
                    device: format!("{base}/devicecode"),
                    token: format!("{base}/token"),
                    client_id: MICROSOFT_CLIENT_ID.into(),
                    scopes: MICROSOFT_SCOPES.into(),
                }
            }
        }
    }
}

/// Tests point Microsoft's endpoints at a fake server (debug builds only).
fn test_base() -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var("ORION_OAUTH_TEST_BASE").ok().filter(|s| !s.is_empty())
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time when the access token stops working.
    pub expires_at: u64,
}

impl TokenSet {
    /// Expired or about to (one minute of margin).
    pub fn needs_refresh(&self) -> bool {
        now() + 60 >= self.expires_at
    }
}

#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .into()
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, Value)> {
    let mut res = agent().post(url).send_form(form.iter().copied()).map_err(|e| Error::Network(e.to_string()))?;
    let status = res.status().as_u16();
    let v: Value = res.body_mut().read_json().map_err(|e| Error::Protocol(format!("invalid OAuth response (HTTP {status}): {e}")))?;
    Ok((status, v))
}

fn token_from(v: &Value, previous_refresh: Option<&str>) -> Result<TokenSet> {
    let access = v["access_token"].as_str().ok_or_else(|| Error::Protocol("token response without access_token".into()))?;
    // If the provider did not rotate the refresh token, keep the old one.
    let refresh = v["refresh_token"].as_str().or(previous_refresh).ok_or_else(|| Error::Protocol("token response without refresh_token (offline_access missing?)".into()))?;
    let expires_in = v["expires_in"].as_u64().or_else(|| v["expires_in"].as_str().and_then(|s| s.parse().ok())).unwrap_or(3600);
    Ok(TokenSet { access_token: access.into(), refresh_token: refresh.into(), expires_at: now() + expires_in })
}

/// Step 1 of the device flow: get the code to show the user.
pub fn start_device_flow(ep: &Endpoints) -> Result<DeviceCode> {
    let (status, v) = post_form(&ep.device, &[("client_id", &ep.client_id), ("scope", &ep.scopes)])?;
    if status != 200 {
        return Err(Error::Auth(format!("Microsoft refused the sign-in request: {}", v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("unknown error"))));
    }
    let s = |k: &str| v[k].as_str().map(str::to_string).ok_or_else(|| Error::Protocol(format!("device code response without {k}")));
    Ok(DeviceCode {
        device_code: s("device_code")?,
        user_code: s("user_code")?,
        verification_uri: s("verification_uri").or_else(|_| s("verification_url"))?,
        expires_in: v["expires_in"].as_u64().unwrap_or(900),
        interval: v["interval"].as_u64().unwrap_or(5).max(1),
    })
}

/// Step 2: poll until the user approves (or declines, or the code expires).
/// Returns the tokens and the signed-in address. `sleep` is injectable for tests.
pub fn finish_device_flow(ep: &Endpoints, dc: &DeviceCode, sleep: &mut dyn FnMut(Duration)) -> Result<(TokenSet, String)> {
    let deadline = now() + dc.expires_in;
    let mut interval = dc.interval;
    loop {
        sleep(Duration::from_secs(interval));
        let (status, v) = post_form(
            &ep.token,
            &[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("client_id", &ep.client_id), ("device_code", &dc.device_code)],
        )?;
        if status == 200 {
            let tokens = token_from(&v, None)?;
            let email = v["id_token"].as_str().and_then(email_from_id_token).ok_or_else(|| Error::Protocol("sign-in succeeded but no email address was returned".into()))?;
            return Ok((tokens, email));
        }
        match v["error"].as_str().unwrap_or_default() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "authorization_declined" | "access_denied" => return Err(Error::Auth("sign-in was declined".into())),
            "expired_token" | "code_expired" => return Err(Error::Auth("the code expired before sign-in finished; try again".into())),
            other => return Err(Error::Auth(format!("sign-in failed: {} {}", other, v["error_description"].as_str().unwrap_or_default()))),
        }
        if now() > deadline {
            return Err(Error::Auth("the code expired before sign-in finished; try again".into()));
        }
    }
}

pub fn refresh(ep: &Endpoints, tokens: &TokenSet) -> Result<TokenSet> {
    let (status, v) = post_form(
        &ep.token,
        &[("grant_type", "refresh_token"), ("client_id", &ep.client_id), ("refresh_token", &tokens.refresh_token), ("scope", &ep.scopes)],
    )?;
    if status != 200 {
        let err = v["error"].as_str().unwrap_or("unknown");
        return Err(match err {
            "invalid_grant" | "interaction_required" => {
                Error::Auth("the Microsoft sign-in expired or was revoked: run `orion account login <name>`".into())
            }
            _ => Error::Auth(format!("token refresh failed: {err} {}", v["error_description"].as_str().unwrap_or_default())),
        });
    }
    token_from(&v, Some(&tokens.refresh_token))
}

/// Email address from a JWT `id_token` (claims `email`, `preferred_username`
/// or `upn`). The signature is not checked: the token came over TLS straight
/// from the token endpoint and is only used to show/prefill the address.
pub fn email_from_id_token(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let v: Value = serde_json::from_slice(&base64url_decode(payload)?).ok()?;
    ["email", "preferred_username", "upn"].iter().find_map(|k| v[*k].as_str().filter(|s| s.contains('@')).map(str::to_string))
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.trim_end_matches('=').as_bytes() {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ------------------------------------------------------- token management

static CACHE: Mutex<Option<HashMap<String, TokenSet>>> = Mutex::new(None);

pub fn save(account: &str, tokens: &TokenSet) -> Result<()> {
    let json = serde_json::to_string(tokens).expect("serializable");
    secrets::store(account, Kind::OAuth, &format!("Orion OAuth tokens ({account})"), &json)
}

pub fn load(account: &str) -> Result<Option<TokenSet>> {
    Ok(secrets::lookup(account, Kind::OAuth)?.and_then(|j| serde_json::from_str(&j).ok()))
}

/// A valid access token for `account`, refreshing (and persisting the
/// rotated refresh token) when needed.
pub fn access_token(provider: OAuthProvider, account: &str) -> Result<String> {
    let mut cache = CACHE.lock().expect("oauth cache");
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(t) = map.get(account).filter(|t| !t.needs_refresh()) {
        return Ok(t.access_token.clone());
    }
    // Serialize refreshes between processes; whoever waits re-reads the keyring.
    let _lock = lock_file(account)?;
    let stored = load(account)?.ok_or_else(|| Error::Auth(format!("no Microsoft sign-in stored for `{account}`: run `orion account login {account}`")))?;
    let tokens = if stored.needs_refresh() {
        let fresh = refresh(&Endpoints::for_provider(provider), &stored)?;
        save(account, &fresh)?;
        fresh
    } else {
        stored
    };
    map.insert(account.to_string(), tokens.clone());
    Ok(tokens.access_token)
}

/// Forget the cached access token (the server rejected it): the next call refreshes.
pub fn invalidate(account: &str) {
    if let Some(map) = CACHE.lock().expect("oauth cache").as_mut()
        && let Some(t) = map.get_mut(account)
    {
        t.expires_at = 0;
    }
    if let Ok(Some(mut t)) = load(account) {
        t.expires_at = 0;
        let _ = save(account, &t);
    }
}

fn lock_file(account: &str) -> Result<std::fs::File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("orion-oauth-{account}.lock"));
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| Error::io(&path, e))?;
    f.lock().map_err(|e| Error::io(&path, e))?;
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_token_email() {
        // header.payload.signature with payload {"preferred_username":"ana@outlook.com"}
        let payload = "eyJwcmVmZXJyZWRfdXNlcm5hbWUiOiJhbmFAb3V0bG9vay5jb20ifQ";
        assert_eq!(email_from_id_token(&format!("x.{payload}.y")).as_deref(), Some("ana@outlook.com"));
        assert_eq!(email_from_id_token("garbage"), None);
    }

    #[test]
    fn expiry() {
        let t = TokenSet { access_token: "a".into(), refresh_token: "r".into(), expires_at: now() + 30 };
        assert!(t.needs_refresh());
        let t = TokenSet { expires_at: now() + 3600, ..t };
        assert!(!t.needs_refresh());
    }
}
