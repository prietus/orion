//! Well-known providers (ported from relaymail's `ProviderProfile`): servers,
//! and how to sign in — an app password page to open, or OAuth2.

use orion_core::config::{OAuthProvider, TlsMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
}

impl Server {
    fn new(host: &str, port: u16, tls: TlsMode) -> Self {
        Server { host: host.into(), port, tls }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub name: &'static str,
    pub imap: Server,
    pub pop3: Option<Server>,
    pub smtp: Server,
    /// Shown when the provider is detected (why the normal password will not work).
    pub advisory: Option<&'static str>,
    /// Page where the user generates an app password.
    pub app_password_url: Option<&'static str>,
    /// Sign in with OAuth2 instead of a password.
    pub oauth: Option<OAuthProvider>,
}

/// Looks up a provider by the address's domain (case-insensitive).
pub fn by_domain(domain: &str) -> Option<Provider> {
    use TlsMode::{Starttls, Tls};
    let d = domain.trim().to_ascii_lowercase();
    Some(match d.as_str() {
        "icloud.com" | "me.com" | "mac.com" => Provider {
            name: "iCloud",
            imap: Server::new("imap.mail.me.com", 993, Tls),
            pop3: None,
            smtp: Server::new("smtp.mail.me.com", 587, Starttls),
            advisory: Some("iCloud requires an app-specific password: account.apple.com → Sign-In and Security → App-Specific Passwords."),
            app_password_url: Some("https://account.apple.com/account/manage"),
            oauth: None,
        },
        "gmail.com" | "googlemail.com" => Provider {
            name: "Gmail",
            imap: Server::new("imap.gmail.com", 993, Tls),
            pop3: Some(Server::new("pop.gmail.com", 995, Tls)),
            smtp: Server::new("smtp.gmail.com", 465, Tls),
            advisory: Some("Gmail needs 2-step verification and an app password: myaccount.google.com → Security → App passwords."),
            app_password_url: Some("https://myaccount.google.com/apppasswords"),
            oauth: None,
        },
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" | "outlook.es" | "hotmail.es" | "live.es" => Provider {
            name: "Outlook",
            imap: Server::new("outlook.office365.com", 993, Tls),
            pop3: None, // POP3 over OAuth needs another permission; IMAP is the supported path
            smtp: Server::new("smtp.office365.com", 587, Starttls),
            advisory: Some("Sign in with Microsoft — Outlook no longer accepts plain passwords for IMAP/SMTP."),
            app_password_url: None,
            oauth: Some(OAuthProvider::Microsoft),
        },
        "yahoo.com" | "yahoo.es" => Provider {
            name: "Yahoo",
            imap: Server::new("imap.mail.yahoo.com", 993, Tls),
            pop3: Some(Server::new("pop.mail.yahoo.com", 995, Tls)),
            smtp: Server::new("smtp.mail.yahoo.com", 465, Tls),
            advisory: Some("Yahoo requires an app password generated in your account's security settings."),
            app_password_url: Some("https://login.yahoo.com/account/security/app-passwords"),
            oauth: None,
        },
        "fastmail.com" | "fastmail.fm" => Provider {
            name: "Fastmail",
            imap: Server::new("imap.fastmail.com", 993, Tls),
            pop3: Some(Server::new("pop.fastmail.com", 995, Tls)),
            smtp: Server::new("smtp.fastmail.com", 465, Tls),
            advisory: Some("Fastmail requires an app password generated in your account settings."),
            app_password_url: Some("https://app.fastmail.com/settings/security"),
            oauth: None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup() {
        let p = by_domain("ICloud.com").unwrap();
        assert_eq!((p.name, p.imap.host.as_str(), p.smtp.port), ("iCloud", "imap.mail.me.com", 587));
        assert!(p.app_password_url.is_some() && p.oauth.is_none());
        let o = by_domain("hotmail.es").unwrap();
        assert_eq!(o.oauth, Some(OAuthProvider::Microsoft));
        assert!(o.app_password_url.is_none());
        assert!(by_domain("example.org").is_none());
    }
}
