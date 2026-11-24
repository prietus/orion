use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{Error, Result};

/// Complete configuration (`~/.config/orion/config.toml`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub pgp: PgpConfig,
    pub daemon: DaemonConfig,
    pub contacts: ContactsConfig,
    /// Action -> list of keys. Replaces the default keys of that action.
    pub keys: BTreeMap<String, Vec<String>>,
    #[serde(rename = "account")]
    pub accounts: Vec<AccountConfig>,
}

/// Where address suggestions come from (see `orion contacts`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContactsConfig {
    /// In order: "eds" (GNOME/Evolution address books, including GNOME Online
    /// Accounts), "mail" (people who wrote to you), "vcard:<file or folder>",
    /// "command:<cmd>" (prints "email<TAB>name" lines, the mutt query format).
    pub sources: Vec<String>,
}

impl Default for ContactsConfig {
    fn default() -> Self {
        ContactsConfig { sources: vec!["eds".into(), "mail".into()] }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub image_protocol: ImageProtocol,
    /// Automatically show the image pane if the message has images.
    pub auto_show_images: bool,
    pub sidebar_width: u16,
    /// Height of the image pane inside the reader, as a percentage.
    pub image_pane_percent: u16,
    /// List order: "date-desc" or "date-asc".
    pub sort: SortOrder,
    /// Show the HTML version when there is one (otherwise plain text).
    pub prefer_html: bool,
    /// HTML colours: "auto" (readable text, no backgrounds), "full" or "none".
    pub html_colors: HtmlColors,
    /// Maximum height of an image inside a message, in lines.
    pub max_image_rows: u16,
    /// Append `[n]` after each link.
    pub link_markers: bool,
    /// A coloured chip with the sender's initials before each message (the
    /// same person always gets the same colour).
    pub avatars: bool,
    /// Program used to open links (receives the URL as its last argument).
    pub open_command: String,
    /// Editor for writing mail (may include arguments, e.g. "code --wait").
    /// Default: $VISUAL, $EDITOR, then the first installed of nano, nvim, vim, vi.
    pub editor: Option<String>,
    /// Images on the internet (not attached): "ask" (L loads them for one
    /// message), "always" or "never". Loading tells the sender you opened it.
    pub remote_images: RemoteImages,
    /// Browser used for the original view (`V`); default: the first of
    /// chromium, chromium-browser, google-chrome-stable found in PATH.
    pub html_renderer: Option<String>,
    /// Colours: "mocha" (default), "macchiato", "frappe", "latte" (the
    /// Catppuccin flavours) or "terminal" (the terminal's own 16 colours).
    pub theme: String,
    /// Nerd Font icons (folders, attachments, encryption...); false = plain symbols.
    pub icons: bool,
    /// Message list: "two-line" (sender and date, subject below) or "compact".
    pub list_style: ListStyle,
    /// "auto" (three panes — folders, list, message — when the window is
    /// wide enough), "three-pane" or "stacked" (the message replaces the list).
    pub layout: LayoutMode,
    /// Seconds a previewed message must stay on screen to be marked read
    /// (0 = at once). Opening it with Enter marks it at once.
    pub mark_read_after: u64,
    /// Writing mail: "builtin" (Orion's panel: address suggestions, Vim-like
    /// body editor; Ctrl-E opens $EDITOR) or "editor" (straight to $EDITOR).
    pub composer: Composer,
    /// Group the list into conversations (t expands one, T toggles grouping).
    pub threads: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Composer {
    #[default]
    Builtin,
    Editor,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutMode {
    #[default]
    Auto,
    ThreePane,
    Stacked,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListStyle {
    #[default]
    TwoLine,
    Compact,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteImages {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PgpConfig {
    pub enabled: bool,
    /// GnuPG executable.
    pub gpg: String,
    /// Alternative GNUPGHOME (defaults to GnuPG's own).
    pub homedir: Option<PathBuf>,
    /// "orion": Orion asks for the PIN (loopback); "agent": gpg-agent's pinentry asks
    /// (graphical pinentry only: a terminal one would break the screen).
    pub pinentry: PinentryMode,
    /// Maximum seconds per operation (includes waiting for the YubiKey touch).
    pub timeout: u64,
    /// New messages start with "sign" turned on.
    pub sign_by_default: bool,
    /// New messages start with "encrypt" turned on when every recipient has a key.
    pub encrypt_when_possible: bool,
    /// Where `k` looks for missing keys after WKD. keys.openpgp.org only
    /// publishes addresses whose owner confirmed them by email.
    pub keyservers: Vec<String>,
    /// Send our public key in an `Autocrypt:` header so others can reply encrypted.
    pub autocrypt: bool,
}

impl Default for PgpConfig {
    fn default() -> Self {
        PgpConfig {
            enabled: true,
            gpg: "gpg".into(),
            homedir: None,
            pinentry: PinentryMode::Orion,
            timeout: 60,
            sign_by_default: false,
            encrypt_when_possible: false,
            keyservers: vec!["hkps://keys.openpgp.org".into()],
            autocrypt: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinentryMode {
    #[default]
    Orion,
    Agent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HtmlColors {
    #[default]
    Auto,
    Full,
    None,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            image_protocol: ImageProtocol::Auto,
            auto_show_images: true,
            sidebar_width: 26,
            image_pane_percent: 50,
            sort: SortOrder::DateDesc,
            prefer_html: true,
            html_colors: HtmlColors::Auto,
            max_image_rows: 30,
            link_markers: true,
            avatars: true,
            open_command: "xdg-open".into(),
            editor: None,
            remote_images: RemoteImages::Ask,
            html_renderer: None,
            theme: "mocha".into(),
            icons: true,
            list_style: ListStyle::TwoLine,
            layout: LayoutMode::Auto,
            mark_read_after: 2,
            composer: Composer::Builtin,
            threads: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageProtocol {
    #[default]
    Auto,
    Sixel,
    Kitty,
    Iterm2,
    Halfblocks,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortOrder {
    #[default]
    DateDesc,
    DateAsc,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountConfig {
    pub name: String,
    /// Outgoing server (SMTP). JMAP accounts send through JMAP instead.
    #[serde(default)]
    pub smtp: Option<SmtpConfig>,
    /// Sender identity, e.g. `"Ana García <ana@example.org>"`. Default: the
    /// account user if it is an address.
    #[serde(default)]
    pub from: Option<String>,
    /// Text appended to new messages after "-- ".
    #[serde(default)]
    pub signature: Option<String>,
    /// OpenPGP key to sign with (fingerprint, key id or address). Default:
    /// the key of the From address.
    #[serde(default)]
    pub pgp_key: Option<String>,
    /// Keep a copy of sent mail in the server's Sent mailbox (IMAP APPEND).
    /// Default: true, except for Gmail (it saves sent mail by itself).
    #[serde(default)]
    pub save_sent: Option<bool>,
    /// Mailboxes checked for new mail on every poll (id or display name,
    /// case-insensitive). Empty = the inbox (first mailbox); `["*"]` = all.
    #[serde(default)]
    pub watch: Vec<String>,
    #[serde(flatten)]
    pub kind: AccountKind,
}

impl AccountConfig {
    pub fn new(name: impl Into<String>, kind: AccountKind) -> Self {
        let mut a = AccountConfig { name: name.into(), watch: Vec::new(), smtp: None, from: None, signature: None, pgp_key: None, save_sent: None, kind };
        a.finalize();
        a
    }

    /// The From address to send with.
    pub fn sender(&self) -> Option<String> {
        if let Some(f) = &self.from {
            return Some(f.clone());
        }
        match &self.kind {
            AccountKind::Imap(c) | AccountKind::Pop3(c) if c.user.contains('@') => Some(c.user.clone()),
            AccountKind::Jmap(c) => c.user.clone().filter(|u| u.contains('@')),
            _ => None,
        }
    }

    /// Whether to append sent mail to the Sent mailbox.
    pub fn saves_sent(&self) -> bool {
        self.save_sent.unwrap_or_else(|| match &self.kind {
            AccountKind::Imap(c) => !c.host.eq_ignore_ascii_case("imap.gmail.com"),
            _ => true,
        })
    }

    /// Copies the account name into the backend settings (it is the key of
    /// the account's secrets in the system keyring).
    pub fn finalize(&mut self) {
        match &mut self.kind {
            AccountKind::Imap(c) | AccountKind::Pop3(c) => c.account = self.name.clone(),
            AccountKind::Jmap(c) => c.account = self.name.clone(),
            AccountKind::Mbox { .. } => {}
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: TlsMode,
}

/// OAuth2 identity providers Orion can sign in with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Microsoft,
}

/// `orion daemon`: background sync, notifications and the local API.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// Unix socket path (default: $XDG_RUNTIME_DIR/orion.sock).
    pub socket: Option<PathBuf>,
    /// Desktop notification for new mail.
    pub notify: bool,
    /// Notification program; receives the title and body as arguments.
    pub notify_command: String,
    /// Shell command run on new mail. Details are passed in environment
    /// variables (ORION_ACCOUNT, ORION_MAILBOX, ORION_COUNT, ORION_FROM,
    /// ORION_SUBJECT, ORION_UNREAD), never interpolated into the command.
    pub on_new_mail: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig { socket: None, notify: true, notify_command: "notify-send".into(), on_new_mail: None }
    }
}

impl DaemonConfig {
    pub fn socket_path(&self) -> PathBuf {
        if let Some(p) = &self.socket {
            return p.clone();
        }
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("orion.sock"),
            None => {
                let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
                std::env::temp_dir().join(format!("orion-{user}.sock"))
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum AccountKind {
    /// `path` may be an mbox file or a directory containing several.
    Mbox { path: PathBuf },
    Imap(ImapConfig),
    /// POP3 takes the same fields as IMAP (default ports 995 with `tls`, 110 otherwise).
    Pop3(ImapConfig),
    Jmap(JmapConfig),
}

/// JMAP (RFC 8620/8621) account.
#[derive(Debug, Clone, Deserialize)]
pub struct JmapConfig {
    /// Session URL, or the server base URL (then `/.well-known/jmap` is used).
    pub url: String,
    /// User for HTTP Basic auth (not needed with `auth = "bearer"`).
    pub user: Option<String>,
    /// Password or token in plain text (prefer `password_command`).
    pub password: Option<String>,
    /// Command printing the password or the bearer token.
    pub password_command: Option<String>,
    #[serde(default)]
    pub auth: JmapAuth,
    #[serde(default = "default_poll")]
    pub poll_interval: u64,
    #[serde(default = "yes")]
    pub cache_bodies: bool,
    /// Account name (filled in on load).
    #[serde(skip)]
    pub account: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JmapAuth {
    #[default]
    Basic,
    Bearer,
}

impl JmapConfig {
    pub fn resolve_password(&self) -> Result<String> {
        resolve_secret(self.password.as_deref(), self.password_command.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImapConfig {
    pub host: String,
    /// Defaults to 993 with `tls`, 143 with `starttls`/`none`.
    pub port: Option<u16>,
    #[serde(default)]
    pub tls: TlsMode,
    pub user: String,
    /// Plain-text password (discouraged; prefer `password_command`).
    pub password: Option<String>,
    /// Command whose output (first line) is the password, e.g. `pass show mail/work`.
    pub password_command: Option<String>,
    /// Seconds between checks for new mail.
    #[serde(default = "default_poll")]
    pub poll_interval: u64,
    /// Keep downloaded bodies in the local cache.
    #[serde(default = "yes")]
    pub cache_bodies: bool,
    /// IMAP IDLE: a second connection waits for the server to announce new
    /// mail, so it arrives instantly instead of at the next poll.
    #[serde(default = "yes")]
    pub idle: bool,
    /// Sign in with OAuth2 (XOAUTH2) instead of a password. Tokens live in
    /// the system keyring (`orion account add` stores them).
    #[serde(default)]
    pub oauth: Option<OAuthProvider>,
    /// Account name (filled in on load; the keyring key).
    #[serde(skip)]
    pub account: String,
}

/// Password from a command (first line of its output) or from the plain value.
pub fn resolve_secret(password: Option<&str>, command: Option<&str>) -> Result<String> {
    if let Some(cmd) = command {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| Error::Auth(format!("could not run password_command: {e}")))?;
        if !out.status.success() {
            return Err(Error::Auth(format!("password_command failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
        }
        let s = String::from_utf8_lossy(&out.stdout);
        return Ok(s.lines().next().unwrap_or_default().to_string());
    }
    password.map(str::to_string).ok_or_else(|| Error::Auth("missing password or password_command".into()))
}

fn default_poll() -> u64 {
    120
}
fn yes() -> bool {
    true
}

impl ImapConfig {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            TlsMode::Tls => 993,
            TlsMode::Starttls | TlsMode::None => 143,
        })
    }

    pub fn port_for(&self, protocol: &str) -> u16 {
        if protocol == "pop3" { self.pop3_port() } else { self.port() }
    }

    pub fn pop3_port(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            TlsMode::Tls => 995,
            TlsMode::Starttls | TlsMode::None => 110,
        })
    }

    /// Obtains the password by running `password_command` if set.
    pub fn resolve_password(&self) -> Result<String> {
        resolve_secret(self.password.as_deref(), self.password_command.as_deref())
    }

}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    #[default]
    Tls,
    Starttls,
    /// Unencrypted: for local testing only.
    None,
}

impl AccountKind {
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            AccountKind::Mbox { .. } => 30,
            AccountKind::Imap(c) | AccountKind::Pop3(c) => c.poll_interval.max(15),
            AccountKind::Jmap(c) => c.poll_interval.max(15),
        })
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("orion").join("config.toml"))
    }

    /// Loads the configuration. If `path` is `None` and the default file does not
    /// exist, returns the default configuration.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let (path, required) = match path {
            Some(p) => (p.to_path_buf(), true),
            None => match Self::default_path() {
                Some(p) => (p, false),
                None => return Ok(Self::default()),
            },
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(Error::io(path, e)),
        };
        let mut cfg: Config = toml::from_str(&text)
            .map_err(|e| Error::Config { path: path.clone(), msg: e.to_string() })?;
        for acc in &mut cfg.accounts {
            if let AccountKind::Mbox { path } = &mut acc.kind {
                *path = expand_tilde(path);
            }
            acc.finalize();
        }
        if let Some(h) = &mut cfg.pgp.homedir {
            *h = expand_tilde(h);
        }
        if let Some(s) = &mut cfg.daemon.socket {
            *s = expand_tilde(s);
        }
        Ok(cfg)
    }
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| p.to_path_buf()),
        Err(_) => p.to_path_buf(),
    }
}

pub const EXAMPLE_CONFIG: &str = r#"# ~/.config/orion/config.toml

[ui]
# auto | sixel | kitty | iterm2 | halfblocks | none
image_protocol = "auto"
auto_show_images = true
sidebar_width = 26
image_pane_percent = 50
# date-desc | date-asc
sort = "date-desc"
# native HTML (true) or plain text (false); "v" toggles inside the reader
prefer_html = true
# auto (readable colours, no backgrounds) | full | none
html_colors = "auto"
max_image_rows = 30
link_markers = true
# a coloured chip with the sender's initials in the list and the reader
avatars = true
# colours: "mocha", "macchiato", "frappe", "latte" (Catppuccin) or "terminal"
theme = "mocha"
# Nerd Font icons (needs a Nerd Font or "Symbols Nerd Font" installed)
icons = true
# message list: "two-line" or "compact"
list_style = "two-line"
# "auto" (folders | list | message when the window is wide), "three-pane", "stacked"
layout = "auto"
# seconds a previewed message must be shown to count as read (0 = at once)
mark_read_after = 2
# writing mail: "builtin" (Orion's panel with address suggestions; Ctrl-E
# opens $EDITOR) or "editor" (straight to $EDITOR)
composer = "builtin"
# group the message list into conversations (t expands one, T toggles)
threads = true
# images hosted on the internet: "ask" (press L in a message), "always", "never"
remote_images = "ask"
open_command = "xdg-open"
# editor for writing mail (default: $VISUAL, $EDITOR, then nano/nvim/vim/vi)
# editor = "nvim"

[pgp]
enabled = true
gpg = "gpg"
# homedir = "~/.gnupg"
# orion: Orion asks for the YubiKey PIN / passphrase
# agent: gpg-agent's pinentry asks (use only with a graphical pinentry)
pinentry = "orion"
# maximum seconds per operation (includes waiting for you to touch the YubiKey)
timeout = 60
# where missing keys are looked up after WKD (keys.openpgp.org verifies addresses)
keyservers = ["hkps://keys.openpgp.org"]
# send your public key in an Autocrypt header (others can then reply encrypted)
autocrypt = true

# Background daemon: `orion daemon` (see contrib/orion.service)
[contacts]
# address suggestions when writing mail, in order of preference:
#   "eds"            GNOME / Evolution address books (incl. GNOME Online Accounts)
#   "mail"           people who wrote to you (from Orion's cache)
#   "vcard:~/Contacts"            a .vcf file or folder (vdirsyncer, khard...)
#   "command:khard email --parsable"   lines "email<TAB>name" (mutt query format)
sources = ["eds", "mail"]

[daemon]
# socket = "/run/user/1000/orion.sock"
notify = true
notify_command = "notify-send"
# Runs on new mail; details come in ORION_ACCOUNT, ORION_MAILBOX, ORION_COUNT,
# ORION_FROM, ORION_SUBJECT and ORION_UNREAD environment variables.
# on_new_mail = "paplay /usr/share/sounds/freedesktop/stereo/message.oga"

# Keys: action = ["key", ...]. Only list the ones you change.
# Format: "j", "G", "enter", "esc", "tab", "space", "up", "pgdown", "ctrl-d", "alt-x", "f5"
[keys]
# down = ["j", "down"]
# quit = ["q"]

[[account]]
name = "local"
backend = "mbox"
# An mbox file or a directory of mbox files
path = "~/Mail"

[[account]]
name = "work"
backend = "imap"
host = "imap.example.com"
# port = 993
# tls | starttls | none
tls = "tls"
user = "me@example.com"
password_command = "pass show mail/work"
# seconds between checks for new mail
poll_interval = 120
# mailboxes checked for new mail (default: the inbox; ["*"] = all)
watch = ["INBOX", "Lists/rust"]
cache_bodies = true
# IMAP IDLE: new mail arrives instantly (uses a second connection)
idle = true

[[account]]
name = "old-isp"
backend = "pop3"
host = "pop.example.net"
tls = "tls"
user = "me"
password_command = "pass show mail/isp"

[[account]]
name = "fastmail"
backend = "jmap"
url = "https://api.fastmail.com/jmap/session"
# basic (user + password) | bearer (password_command prints the token)
auth = "bearer"
password_command = "pass show mail/fastmail-token"
"#;

/// Path of the local cache (sqlite).
pub fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("orion").join("cache.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg: Config = toml::from_str(EXAMPLE_CONFIG).unwrap();
        assert_eq!(cfg.accounts.len(), 4);
        assert!(matches!(&cfg.accounts[2].kind, AccountKind::Pop3(c) if c.pop3_port() == 995));
        assert!(matches!(&cfg.accounts[3].kind, AccountKind::Jmap(c) if c.auth == JmapAuth::Bearer));
        match &cfg.accounts[1].kind {
            AccountKind::Imap(c) => {
                assert_eq!(cfg.accounts[1].watch, ["INBOX", "Lists/rust"]);
                assert_eq!(c.port(), 993);
                assert_eq!(c.password_command.as_deref(), Some("pass show mail/work"));
            }
            _ => panic!("expected imap"),
        }
        assert_eq!(cfg.ui.image_protocol, ImageProtocol::Auto);
    }
}
