//! `orion account …`: adding accounts the easy way (modelled on relaymail's
//! account setup). Known providers are recognised from the address; for
//! iCloud/Gmail/Yahoo/Fastmail the app-password page is opened; Outlook signs
//! in with Microsoft (device code + Authenticator). Other domains are looked up
//! privately (their own autoconfig and DNS SRV). The connection is tested
//! before anything is saved; secrets go to the system keyring, never to the
//! config file.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use orion_backends::autoconfig::Discovered;
use orion_backends::oauth::{self, Endpoints, TokenSet};
use orion_backends::providers::{self, Server};
use orion_backends::secrets::{self, Kind};
use orion_core::config::{AccountConfig, AccountKind, Config, ImapConfig, OAuthProvider, SmtpConfig, TlsMode};

/// Terminal interaction (scripted in tests).
pub trait Io {
    fn say(&mut self, text: &str);
    fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String>;
    fn secret(&mut self, prompt: &str) -> Result<String>;
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool>;
    fn open_url(&mut self, url: &str);
}

type DiscoverFn = Box<dyn Fn(&str, &str) -> Discovered>;
type ConnectFn = Box<dyn Fn(&AccountConfig) -> orion_core::Result<(usize, usize)>>;
type StoreSecretFn = Box<dyn Fn(&str, Kind, &str, &str) -> orion_core::Result<()>>;

/// Everything with side effects outside the terminal (replaced in tests).
pub struct Deps {
    pub discover: DiscoverFn,
    /// Connects and returns (mailboxes, unread).
    pub connect: ConnectFn,
    pub store_secret: StoreSecretFn,
    pub oauth_endpoints: Box<dyn Fn(OAuthProvider) -> Endpoints>,
    pub sleep: Box<dyn Fn(Duration)>,
}

impl Deps {
    pub fn real() -> Deps {
        Deps {
            discover: Box::new(orion_backends::autoconfig::discover),
            connect: Box::new(|acc| {
                let mut b = orion_backends::open(acc)?;
                let mbs = b.mailboxes()?;
                Ok((mbs.len(), mbs.iter().map(|m| m.unread).sum()))
            }),
            store_secret: Box::new(secrets::store),
            oauth_endpoints: Box::new(Endpoints::for_provider),
            sleep: Box::new(std::thread::sleep),
        }
    }
}

pub struct Terminal;

impl Io for Terminal {
    fn say(&mut self, text: &str) {
        println!("{text}");
    }

    fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
        match default {
            Some(d) => print!("{prompt} [{d}]: "),
            None => print!("{prompt}: "),
        }
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            bail!("cancelled");
        }
        let line = line.trim().to_string();
        Ok(if line.is_empty() { default.unwrap_or_default().to_string() } else { line })
    }

    fn secret(&mut self, prompt: &str) -> Result<String> {
        Ok(rpassword::prompt_password(format!("{prompt}: "))?)
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        let answer = self.ask(&format!("{prompt} {}", if default { "[Y/n]" } else { "[y/N]" }), None)?;
        Ok(match answer.to_ascii_lowercase().as_str() {
            "" => default,
            a => a.starts_with('y') || a.starts_with('s'),
        })
    }

    fn open_url(&mut self, url: &str) {
        let ok = std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok();
        if !ok {
            println!("  (could not open a browser; open the link above by hand)");
        }
    }
}

fn tls_name(t: TlsMode) -> &'static str {
    match t {
        TlsMode::Tls => "TLS",
        TlsMode::Starttls => "STARTTLS",
        TlsMode::None => "no encryption",
    }
}

fn show(s: &Server) -> String {
    format!("{}:{} ({})", s.host, s.port, tls_name(s.tls))
}

/// What the wizard decided before writing anything.
struct Plan {
    email: String,
    name: String,
    pop3: bool,
    incoming: Server,
    smtp: Option<Server>,
    user: String,
    oauth: Option<OAuthProvider>,
    password: Option<String>,
    tokens: Option<TokenSet>,
}

impl Plan {
    fn account(&self, password_in_memory: bool) -> AccountConfig {
        let imap = ImapConfig {
            host: self.incoming.host.clone(),
            port: Some(self.incoming.port),
            tls: self.incoming.tls,
            user: self.user.clone(),
            password: if password_in_memory { self.password.clone() } else { None },
            password_command: (!password_in_memory && self.password.is_some()).then(|| secrets::lookup_command(&self.name)),
            poll_interval: 120,
            cache_bodies: true,
            idle: true,
            oauth: self.oauth,
            account: self.name.clone(),
        };
        let kind = if self.pop3 { AccountKind::Pop3(imap) } else { AccountKind::Imap(imap) };
        let mut a = AccountConfig::new(self.name.clone(), kind);
        a.smtp = self.smtp.as_ref().map(|s| SmtpConfig { host: s.host.clone(), port: s.port, tls: s.tls });
        a
    }
}

pub fn add(io: &mut dyn Io, deps: &Deps, config_path: &Path, existing: &Config, email: Option<String>) -> Result<String> {
    let email = match email {
        Some(e) => e,
        None => io.ask("Email address", None)?,
    };
    let email = email.trim().to_string();
    let Some((_, domain)) = email.rsplit_once('@').filter(|(l, d)| !l.is_empty() && d.contains('.')) else {
        bail!("`{email}` does not look like an email address");
    };
    let domain = domain.to_ascii_lowercase();

    // ---- servers
    let known = providers::by_domain(&domain);
    let (incoming, pop3, smtp, default_name, ask_user) = if let Some(p) = &known {
        io.say(&format!("Detected {}: incoming {} · outgoing {}", p.name, show(&p.imap), show(&p.smtp)));
        (p.imap.clone(), false, Some(p.smtp.clone()), p.name.to_ascii_lowercase(), false)
    } else {
        io.say(&format!("Looking up the mail servers of {domain} (asking only {domain} and its DNS)…"));
        let d = (deps.discover)(&domain, &email);
        let label = domain.split('.').next().unwrap_or("mail").to_string();
        match pick_discovered(io, &domain, d)? {
            Some((incoming, pop3, smtp)) => (incoming, pop3, smtp, label, true),
            None => {
                let (incoming, pop3, smtp) = manual_servers(io, &domain)?;
                (incoming, pop3, smtp, label, true)
            }
        }
    };

    // ---- name
    let mut suggestion = default_name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).collect::<String>();
    let mut n = 2;
    while existing.accounts.iter().any(|a| a.name == suggestion) {
        suggestion = format!("{default_name}{n}");
        n += 1;
    }
    let name = loop {
        let name = io.ask("Account name", Some(&suggestion))?;
        if !secrets::valid_name(&name) {
            io.say("  Use letters, digits, '.', '_' or '-' (max 64).");
        } else if existing.accounts.iter().any(|a| a.name == name) {
            io.say(&format!("  There is already an account called `{name}`."));
        } else {
            break name;
        }
    };
    let user = if ask_user { io.ask("Username", Some(&email))? } else { email.clone() };

    let mut plan = Plan { email: email.clone(), name, pop3, incoming, smtp, user, oauth: None, password: None, tokens: None };

    // ---- credentials + connection test
    if let Some(p) = known.as_ref().filter(|p| p.oauth.is_some()) {
        if let Some(a) = p.advisory {
            io.say(a);
        }
        let provider = p.oauth.expect("oauth provider");
        let (tokens, signed_in) = oauth_sign_in(io, deps, provider)?;
        if !signed_in.eq_ignore_ascii_case(&email) {
            io.say(&format!("  Signed in as {signed_in} (not {email}); using {signed_in}."));
            plan.email = signed_in.clone();
            plan.user = signed_in;
        }
        plan.oauth = Some(provider);
        // Tokens must be in the keyring for the connection test (the backend reads them there).
        (deps.store_secret)(&plan.name, Kind::OAuth, &format!("Orion OAuth tokens ({})", plan.name), &serde_json::to_string(&tokens)?)?;
        plan.tokens = Some(tokens);
        test_connection(io, deps, &mut plan, &domain)?;
    } else {
        if let Some(p) = &known {
            if let Some(a) = p.advisory {
                io.say(a);
            }
            if let Some(url) = p.app_password_url {
                io.say(&format!("  App passwords: {url}"));
                if io.confirm("Open that page in your browser now?", true)? {
                    io.open_url(url);
                }
            }
        }
        let label = if known.as_ref().is_some_and(|p| p.app_password_url.is_some()) { "App password" } else { "Password" };
        let mut tries = 0;
        loop {
            let pw = io.secret(&format!("{label} for {}", plan.user))?;
            if pw.is_empty() {
                bail!("no password given");
            }
            plan.password = Some(pw);
            match test_connection(io, deps, &mut plan, &domain) {
                Ok(()) => break,
                Err(e) if tries < 2 && e.to_string().starts_with("authentication") => {
                    tries += 1;
                    io.say("  Try again.");
                }
                Err(e) => return Err(e),
            }
        }
    }

    // ---- save
    if let Some(pw) = &plan.password {
        (deps.store_secret)(&plan.name, Kind::Password, &format!("Orion: {}", plan.email), pw)?;
        io.say("Saved the password in the system keyring.");
    }
    append_account(config_path, &plan)?;
    io.say(&format!("Added account `{}` to {}.", plan.name, config_path.display()));
    Ok(plan.name)
}

fn pick_discovered(io: &mut dyn Io, domain: &str, d: Discovered) -> Result<Option<(Server, bool, Option<Server>)>> {
    if d.is_empty() {
        io.say("  Nothing found.");
        return Ok(None);
    }
    let verified = if d.dnssec { ", DNSSEC-verified" } else { "" };
    io.say(&format!("Found via {}{verified}:", d.source));
    if let Some(s) = &d.imap {
        io.say(&format!("  IMAP  {}", show(s)));
    }
    if let Some(s) = &d.pop3 {
        io.say(&format!("  POP3  {}", show(s)));
    }
    if let Some(s) = &d.smtp {
        io.say(&format!("  SMTP  {}", show(s)));
    }
    let foreign = d.foreign_hosts(domain);
    if !foreign.is_empty() {
        io.say(&format!(
            "  ⚠ {} is outside {domain} and the DNS answer is not verified. Only continue if you expect your mail to be hosted there.",
            foreign.join(", ")
        ));
        if !io.confirm("Use these servers?", false)? {
            return Ok(None);
        }
    } else if !io.confirm("Use these servers?", true)? {
        return Ok(None);
    }
    let pop3 = match (&d.imap, &d.pop3) {
        (Some(_), Some(_)) => io.ask("Protocol (imap/pop3)", Some("imap"))?.eq_ignore_ascii_case("pop3"),
        (None, Some(_)) => true,
        _ => false,
    };
    let incoming = if pop3 { d.pop3.clone() } else { d.imap.clone() }.expect("checked above");
    Ok(Some((incoming, pop3, d.smtp)))
}

fn manual_servers(io: &mut dyn Io, domain: &str) -> Result<(Server, bool, Option<Server>)> {
    io.say("Enter the servers by hand.");
    let pop3 = io.ask("Protocol (imap/pop3)", Some("imap"))?.eq_ignore_ascii_case("pop3");
    let host = io.ask("Incoming server", Some(&format!("mail.{domain}")))?;
    let tls = ask_tls(io)?;
    let default_port = match (pop3, tls) {
        (false, TlsMode::Tls) => "993",
        (false, _) => "143",
        (true, TlsMode::Tls) => "995",
        (true, _) => "110",
    };
    let port: u16 = io.ask("Port", Some(default_port))?.parse().context("invalid port")?;
    let smtp_host = io.ask("Outgoing (SMTP) server (- to skip)", Some(&host))?;
    let smtp = if smtp_host.is_empty() || smtp_host == "-" {
        None
    } else {
        let port: u16 = io.ask("SMTP port", Some("465"))?.parse().context("invalid port")?;
        Some(Server { host: smtp_host, port, tls: if port == 465 { TlsMode::Tls } else { TlsMode::Starttls } })
    };
    Ok((Server { host, port, tls }, pop3, smtp))
}

fn ask_tls(io: &mut dyn Io) -> Result<TlsMode> {
    loop {
        match io.ask("Encryption (tls/starttls/none)", Some("tls"))?.to_ascii_lowercase().as_str() {
            "tls" | "ssl" => return Ok(TlsMode::Tls),
            "starttls" => return Ok(TlsMode::Starttls),
            "none" => {
                io.say("  ⚠ Without encryption the password travels in clear text. Only for local test servers.");
                if io.confirm("Continue without encryption?", false)? {
                    return Ok(TlsMode::None);
                }
            }
            _ => io.say("  Answer tls, starttls or none."),
        }
    }
}

fn oauth_sign_in(io: &mut dyn Io, deps: &Deps, provider: OAuthProvider) -> Result<(TokenSet, String)> {
    let ep = (deps.oauth_endpoints)(provider);
    let dc = oauth::start_device_flow(&ep).map_err(|e| anyhow!("{e}"))?;
    io.say(&format!("To sign in, open {} and enter the code  {}", dc.verification_uri, dc.user_code));
    io.say("  (any device works, e.g. your phone; approve it in Microsoft Authenticator if asked)");
    if io.confirm("Open it in your browser now?", true)? {
        io.open_url(&dc.verification_uri);
    }
    io.say("Waiting for you to approve the sign-in…");
    let (tokens, email) = oauth::finish_device_flow(&ep, &dc, &mut |d| (deps.sleep)(d)).map_err(|e| anyhow!("{e}"))?;
    io.say(&format!("✔ Signed in as {email}"));
    Ok((tokens, email))
}

/// Names a certificate *is* valid for, from a rustls name-mismatch error
/// ("… certificate is only valid for DnsName(\"mail.example.org\")…").
pub fn certificate_names(error: &str) -> Vec<String> {
    if !error.contains("not valid for name") {
        return Vec::new();
    }
    error
        .split("DnsName(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_string)
        .collect()
}

/// Tests the connection. If the server's certificate is valid but for another
/// name *inside the user's own domain* (typically DNS points to example.org
/// while the certificate says mail.example.org), offers to use that name and
/// retries without asking for the password again. Names outside the domain
/// are never offered: that would be how an impostor looks.
fn test_connection(io: &mut dyn Io, deps: &Deps, plan: &mut Plan, domain: &str) -> Result<()> {
    loop {
        io.say(&format!("Testing the connection to {}…", plan.incoming.host));
        let e = match (deps.connect)(&plan.account(true)) {
            Ok((mailboxes, unread)) => {
                io.say(&format!("✔ Connected: {mailboxes} mailbox(es), {unread} unread."));
                return Ok(());
            }
            Err(e @ orion_core::Error::Auth(_)) => {
                io.say(&format!("✘ {e}"));
                return Err(anyhow!("{e}"));
            }
            Err(e) => e,
        };
        let in_domain = |n: &String| {
            let n = n.to_ascii_lowercase();
            n == domain || n.ends_with(&format!(".{domain}"))
        };
        let candidate = certificate_names(&e.to_string()).into_iter().find(|n| in_domain(n) && !n.eq_ignore_ascii_case(&plan.incoming.host));
        if let Some(name) = candidate {
            let old = plan.incoming.host.clone();
            io.say(&format!(
                "  The server's certificate is valid, but for {name}, not for {old}. Your DNS (or the address you typed) points to {old}; \
                 the lasting fix is to point it to {name}."
            ));
            if io.confirm(&format!("Use {name} instead?"), true)? {
                plan.incoming.host = name.clone();
                if let Some(smtp) = plan.smtp.as_mut().filter(|s| s.host.eq_ignore_ascii_case(&old)) {
                    smtp.host = name;
                }
                continue;
            }
        }
        io.say(&format!("✘ {e}"));
        return Err(anyhow!("could not connect: {e}"));
    }
}

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn tls_value(t: TlsMode) -> &'static str {
    match t {
        TlsMode::Tls => "tls",
        TlsMode::Starttls => "starttls",
        TlsMode::None => "none",
    }
}

/// Appends the account to the config file (creating it if needed), keeping
/// everything already there. The result is parsed again; on error the file is
/// restored.
fn append_account(path: &Path, plan: &Plan) -> Result<()> {
    let mut block = format!(
        "\n# Added by `orion account add`.\n[[account]]\nname = {}\nbackend = \"{}\"\nhost = {}\nport = {}\ntls = \"{}\"\nuser = {}\n",
        q(&plan.name),
        if plan.pop3 { "pop3" } else { "imap" },
        q(&plan.incoming.host),
        plan.incoming.port,
        tls_value(plan.incoming.tls),
        q(&plan.user),
    );
    if plan.oauth.is_some() {
        block.push_str("oauth = \"microsoft\"\n");
    } else {
        block.push_str(&format!("password_command = {}\n", q(&secrets::lookup_command(&plan.name))));
    }
    if let Some(s) = &plan.smtp {
        block.push_str(&format!("\n[account.smtp]\nhost = {}\nport = {}\ntls = \"{}\"\n", q(&s.host), s.port, tls_value(s.tls)));
    }
    let original = std::fs::read_to_string(path).ok();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let new = format!("{}{block}", original.as_deref().unwrap_or("# Orion configuration (see `orion --print-config`)\n"));
    std::fs::write(path, &new).with_context(|| format!("writing {}", path.display()))?;
    match Config::load(Some(path)) {
        Ok(cfg) if cfg.accounts.iter().any(|a| a.name == plan.name) => Ok(()),
        other => {
            match &original {
                Some(o) => std::fs::write(path, o)?,
                None => std::fs::remove_file(path)?,
            }
            Err(anyhow!("the updated configuration did not validate, left unchanged: {:?}", other.err()))
        }
    }
}

/// `orion account login <name>`: signs in again (OAuth) or replaces the stored password.
pub fn login(io: &mut dyn Io, deps: &Deps, cfg: &Config, name: &str) -> Result<()> {
    let acc = cfg.accounts.iter().find(|a| a.name == name).ok_or_else(|| anyhow!("no account called `{name}`"))?;
    let imap = match &acc.kind {
        AccountKind::Imap(c) | AccountKind::Pop3(c) => c,
        _ => bail!("`{name}` does not use a password or OAuth sign-in managed by Orion"),
    };
    if let Some(provider) = imap.oauth {
        let (tokens, email) = oauth_sign_in(io, deps, provider)?;
        if !email.eq_ignore_ascii_case(&imap.user) {
            io.say(&format!("  ⚠ Signed in as {email}, but the account uses {}.", imap.user));
        }
        (deps.store_secret)(name, Kind::OAuth, &format!("Orion OAuth tokens ({name})"), &serde_json::to_string(&tokens)?)?;
    } else if imap.password_command.as_deref() == Some(secrets::lookup_command(name).as_str()) {
        let pw = io.secret(&format!("New password for {}", imap.user))?;
        let mut test = acc.clone();
        if let AccountKind::Imap(c) | AccountKind::Pop3(c) = &mut test.kind {
            c.password = Some(pw.clone());
            c.password_command = None;
        }
        let (mbs, unread) = (deps.connect)(&test).map_err(|e| anyhow!("{e}"))?;
        io.say(&format!("✔ Connected: {mbs} mailbox(es), {unread} unread."));
        (deps.store_secret)(name, Kind::Password, &format!("Orion: {}", imap.user), &pw)?;
    } else {
        bail!("`{name}` gets its password from its own password_command; update it there");
    }
    io.say(&format!("Updated the credentials of `{name}`."));
    Ok(())
}

/// `orion account test [name]`: connects and reports counts (never message contents).
pub fn test(io: &mut dyn Io, deps: &Deps, cfg: &Config, name: Option<&str>) -> Result<()> {
    let accounts: Vec<&AccountConfig> = cfg.accounts.iter().filter(|a| name.is_none_or(|n| a.name == n)).collect();
    if accounts.is_empty() {
        bail!("no matching account");
    }
    let mut failed = 0;
    for acc in accounts {
        let t = std::time::Instant::now();
        match (deps.connect)(acc) {
            Ok((mbs, unread)) => io.say(&format!("✔ {}: {mbs} mailbox(es), {unread} unread ({:.1}s)", acc.name, t.elapsed().as_secs_f32())),
            Err(e) => {
                failed += 1;
                io.say(&format!("✘ {}: {e}", acc.name));
            }
        }
    }
    if failed > 0 {
        bail!("{failed} account(s) failed");
    }
    Ok(())
}

pub fn list(io: &mut dyn Io, cfg: &Config) {
    for a in &cfg.accounts {
        let detail = match &a.kind {
            AccountKind::Mbox { path } => format!("mbox  {}", path.display()),
            AccountKind::Imap(c) | AccountKind::Pop3(c) => {
                let proto = if matches!(a.kind, AccountKind::Pop3(_)) { "pop3" } else { "imap" };
                let auth = if c.oauth.is_some() { "OAuth (Microsoft)" } else if c.password_command.is_some() { "password_command" } else { "password in config" };
                format!("{proto}  {}@{}:{} ({})  {auth}", c.user, c.host, c.port_for(proto), tls_name(c.tls))
            }
            AccountKind::Jmap(c) => format!("jmap  {}", c.url),
        };
        io.say(&format!("{:<14} {detail}", a.name));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    /// Scripted terminal: answers are consumed in order by ask/secret/confirm.
    pub struct Script {
        pub answers: VecDeque<String>,
        pub said: Vec<String>,
        pub opened: Vec<String>,
        pub secrets_asked: usize,
    }

    impl Script {
        pub fn new(answers: &[&str]) -> Script {
            Script { answers: answers.iter().map(|s| s.to_string()).collect(), said: Vec::new(), opened: Vec::new(), secrets_asked: 0 }
        }
        fn next(&mut self, what: &str) -> Result<String> {
            self.answers.pop_front().ok_or_else(|| anyhow!("script ran out of answers at: {what}"))
        }
        pub fn transcript(&self) -> String {
            self.said.join("\n")
        }
    }

    impl Io for Script {
        fn say(&mut self, text: &str) {
            self.said.push(text.to_string());
        }
        fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
            let a = self.next(prompt)?;
            Ok(if a.is_empty() { default.unwrap_or_default().to_string() } else { a })
        }
        fn secret(&mut self, prompt: &str) -> Result<String> {
            self.secrets_asked += 1;
            self.next(prompt)
        }
        fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
            let a = self.next(prompt)?;
            Ok(if a.is_empty() { default } else { a.starts_with('y') })
        }
        fn open_url(&mut self, url: &str) {
            self.opened.push(url.to_string());
        }
    }

    pub type Stored = Rc<RefCell<Vec<(String, Kind, String)>>>;

    pub fn deps(connect: impl Fn(&AccountConfig) -> orion_core::Result<(usize, usize)> + 'static, discovered: Discovered) -> (Deps, Stored) {
        let stored: Stored = Rc::new(RefCell::new(Vec::new()));
        let s = stored.clone();
        let deps = Deps {
            discover: Box::new(move |_, _| discovered.clone()),
            connect: Box::new(connect),
            store_secret: Box::new(move |acc, kind, _, secret| {
                s.borrow_mut().push((acc.to_string(), kind, secret.to_string()));
                Ok(())
            }),
            oauth_endpoints: Box::new(Endpoints::for_provider),
            sleep: Box::new(|_| {}),
        };
        (deps, stored)
    }

    fn tmp_config() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orion").join("config.toml");
        (dir, path)
    }

    #[test]
    fn icloud_with_app_password() {
        let (_d, path) = tmp_config();
        let (deps, stored) = deps(
            |acc| {
                let AccountKind::Imap(c) = &acc.kind else { panic!("imap expected") };
                assert_eq!((c.host.as_str(), c.port, c.password.as_deref()), ("imap.mail.me.com", Some(993), Some("abcd-efgh-ijkl-mnop")));
                Ok((7, 3))
            },
            Discovered::default(),
        );
        let mut io = Script::new(&["", "y", "abcd-efgh-ijkl-mnop"]);
        let name = add(&mut io, &deps, &path, &Config::default(), Some("ana@icloud.com".into())).unwrap();
        assert_eq!(name, "icloud");
        assert_eq!(io.opened, ["https://account.apple.com/account/manage"]);
        assert!(io.transcript().contains("Detected iCloud"));
        assert!(io.transcript().contains("✔ Connected: 7 mailbox(es), 3 unread."));
        assert_eq!(&*stored.borrow(), &[("icloud".to_string(), Kind::Password, "abcd-efgh-ijkl-mnop".to_string())]);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("abcd-efgh"), "the password never reaches the config file");
        let cfg = Config::load(Some(&path)).unwrap();
        let acc = &cfg.accounts[0];
        let AccountKind::Imap(c) = &acc.kind else { panic!() };
        assert_eq!(c.password_command.as_deref(), Some(secrets::lookup_command("icloud").as_str()));
        assert_eq!(acc.smtp, Some(SmtpConfig { host: "smtp.mail.me.com".into(), port: 587, tls: TlsMode::Starttls }));

        // A second iCloud account gets a different suggested name.
        let mut io = Script::new(&["", "n", "pw2"]);
        let (deps2, _) = super::tests::deps(|_| Ok((1, 0)), Discovered::default());
        let name = add(&mut io, &deps2, &path, &cfg, Some("otra@me.com".into())).unwrap();
        assert_eq!(name, "icloud2");
        assert!(io.opened.is_empty());
        assert_eq!(Config::load(Some(&path)).unwrap().accounts.len(), 2);
    }

    #[test]
    fn wrong_password_is_asked_again() {
        let (_d, path) = tmp_config();
        let attempts = Rc::new(RefCell::new(0));
        let a = attempts.clone();
        let (deps, stored) = deps(
            move |_| {
                *a.borrow_mut() += 1;
                if *a.borrow() == 1 { Err(orion_core::Error::Auth("invalid credentials".into())) } else { Ok((3, 1)) }
            },
            Discovered::default(),
        );
        let mut io = Script::new(&["gmail-ana", "n", "wrong", "right"]);
        add(&mut io, &deps, &path, &Config::default(), Some("ana@gmail.com".into())).unwrap();
        assert_eq!(io.secrets_asked, 2);
        assert_eq!(stored.borrow()[0].2, "right", "only the working password is stored");
    }

    #[test]
    fn own_domain_with_unverified_foreign_host_needs_confirmation() {
        let (_d, path) = tmp_config();
        let found = Discovered {
            imap: Some(Server { host: "imap.hosting.net".into(), port: 993, tls: TlsMode::Tls }),
            pop3: None,
            smtp: Some(Server { host: "smtp.hosting.net".into(), port: 465, tls: TlsMode::Tls }),
            source: "DNS SRV",
            dnssec: false,
        };
        // Default answer to "Use these servers?" is no → falls back to manual entry.
        let (deps, _) = deps(|_| Ok((1, 0)), found.clone());
        let mut io = Script::new(&["", "imap", "mail.example.net", "tls", "", "", "", "", "", "pw"]);
        add(&mut io, &deps, &path, &Config::default(), Some("me@example.net".into())).unwrap();
        assert!(io.transcript().contains("⚠ imap.hosting.net, smtp.hosting.net is outside example.net"));
        let cfg = Config::load(Some(&path)).unwrap();
        let AccountKind::Imap(c) = &cfg.accounts[0].kind else { panic!() };
        assert_eq!(c.host, "mail.example.net", "manual entry was used");

        // DNSSEC-verified: no warning, default yes.
        let (_d2, path2) = tmp_config();
        let (deps, _) = super::tests::deps(|_| Ok((1, 0)), Discovered { dnssec: true, ..found });
        let mut io = Script::new(&["", "", "", "pw"]);
        add(&mut io, &deps, &path2, &Config::default(), Some("me@example.net".into())).unwrap();
        assert!(io.transcript().contains("DNSSEC-verified") && !io.transcript().contains('⚠'));
    }

    #[test]
    fn certificate_for_another_name_in_the_domain() {
        // Exactly what rustls says for example.net (DNS SRV → example.net, certificate → mail.example.net).
        let mismatch = |host: &str, valid: &str| {
            orion_core::Error::Network(format!(
                "TLS with {host}: invalid peer certificate: certificate not valid for name \"{host}\"; certificate is only valid for DnsName(\"{valid}\")"
            ))
        };
        assert_eq!(certificate_names(&mismatch("example.net", "mail.example.net").to_string()), ["mail.example.net"]);
        assert!(certificate_names("connection refused").is_empty());

        let found = Discovered {
            imap: Some(Server { host: "example.net".into(), port: 993, tls: TlsMode::Tls }),
            pop3: None,
            smtp: Some(Server { host: "example.net".into(), port: 465, tls: TlsMode::Tls }),
            source: "DNS SRV",
            dnssec: false,
        };
        let (_d, path) = tmp_config();
        let (deps, _) = deps(
            move |acc| {
                let AccountKind::Imap(c) = &acc.kind else { panic!() };
                if c.host == "example.net" { Err(mismatch("example.net", "mail.example.net")) } else { Ok((4, 2)) }
            },
            found.clone(),
        );
        // use servers, name, username, password, then "Use mail.example.net instead?" → yes
        let mut io = Script::new(&["", "", "", "pw", ""]);
        add(&mut io, &deps, &path, &Config::default(), Some("dana@example.net".into())).unwrap();
        assert!(io.transcript().contains("valid, but for mail.example.net, not for example.net"), "{}", io.transcript());
        assert_eq!(io.secrets_asked, 1, "the password is not asked again");
        let cfg = Config::load(Some(&path)).unwrap();
        let AccountKind::Imap(c) = &cfg.accounts[0].kind else { panic!() };
        assert_eq!(c.host, "mail.example.net");
        assert_eq!(cfg.accounts[0].smtp.as_ref().unwrap().host, "mail.example.net", "SMTP pointed to the same host: fixed too");

        // A certificate for a name outside the domain is never offered.
        let (_d2, path2) = tmp_config();
        let (deps, _) = super::tests::deps(move |_| Err(mismatch("example.net", "evil.example.com")), found);
        let mut io = Script::new(&["", "", "", "pw"]);
        assert!(add(&mut io, &deps, &path2, &Config::default(), Some("dana@example.net".into())).is_err());
        assert!(!io.transcript().contains("instead"), "{}", io.transcript());
        assert!(!path2.exists(), "nothing saved");
    }

    #[test]
    fn outlook_signs_in_with_microsoft() {
        use std::io::BufRead;
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fake_oauth.py");
        let Ok(mut child) = std::process::Command::new("python3").arg("-I").arg(script).stdout(std::process::Stdio::piped()).spawn() else {
            eprintln!("python3 not available: test skipped");
            return;
        };
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let port = line.split_whitespace().nth(1).unwrap().to_string();
        let (_d, path) = tmp_config();
        let (mut deps, stored) = deps(
            |acc| {
                let AccountKind::Imap(c) = &acc.kind else { panic!() };
                assert_eq!((c.oauth, c.password.as_deref(), c.host.as_str()), (Some(OAuthProvider::Microsoft), None, "outlook.office365.com"));
                Ok((12, 4))
            },
            Discovered::default(),
        );
        deps.oauth_endpoints = Box::new(move |_| Endpoints {
            device: format!("http://127.0.0.1:{port}/common/oauth2/v2.0/devicecode"),
            token: format!("http://127.0.0.1:{port}/common/oauth2/v2.0/token"),
            client_id: "test".into(),
            scopes: "offline_access openid email".into(),
        });
        let mut io = Script::new(&["", "y"]);
        add(&mut io, &deps, &path, &Config::default(), Some("ana@outlook.com".into())).unwrap();
        let _ = child.kill();
        assert!(io.transcript().contains("enter the code  ABCD-1234"));
        assert!(io.transcript().contains("✔ Signed in as ana@outlook.com"));
        assert_eq!(io.secrets_asked, 0, "no password is ever asked for Outlook");
        assert_eq!(io.opened.len(), 1);
        let s = stored.borrow();
        assert_eq!((s[0].0.as_str(), s[0].1), ("outlook", Kind::OAuth));
        assert!(s[0].2.contains("\"refresh_token\":\"R1\""));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("oauth = \"microsoft\"") && !text.contains("password_command"));
        assert!(!text.contains("R1") && !text.contains("A1"), "tokens never reach the config file");
    }
}
