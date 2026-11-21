//! Throwaway Dovecot server for integration tests: runs as the current user,
//! listens on free localhost ports, stores mail in a temporary Maildir.

#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const USER: &str = "ana";
pub const PASSWORD: &str = "s3cret-pass";

pub struct Dovecot {
    child: Child,
    pub dir: tempfile::TempDir,
    run_dir: PathBuf,
    pub imap_port: u16,
    pub pop3_port: u16,
    /// Implicit-TLS ports (only with `start_tls`).
    pub imaps_port: u16,
    pub pop3s_port: u16,
    /// CA certificate that signed the server certificate (only with `start_tls`).
    pub ca_file: Option<PathBuf>,
    counter: std::cell::Cell<u32>,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Dovecot {
    /// Starts Dovecot, or returns `None` (test skipped) if it is not installed.
    pub fn start() -> Option<Dovecot> {
        Self::start_inner(false)
    }

    /// Like `start`, plus TLS: a throwaway CA, a certificate for 127.0.0.1,
    /// STARTTLS on the plain ports and IMAPS/POP3S listeners.
    pub fn start_tls() -> Option<Dovecot> {
        Command::new("openssl").arg("version").output().ok()?;
        Self::start_inner(true)
    }

    fn start_inner(tls: bool) -> Option<Dovecot> {
        Command::new("dovecot").arg("--version").output().ok()?;
        let dir = tempfile::Builder::new().prefix("orion-dovecot-").tempdir().unwrap();
        // Unix socket paths are limited to ~108 bytes: keep the runtime dir short.
        let base = std::env::var("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let run_dir = base.join(format!("odv-{}-{}", std::process::id(), free_port()));
        std::fs::create_dir_all(&run_dir).unwrap();
        let (imap_port, pop3_port) = (free_port(), free_port());
        let (imaps_port, pop3s_port) = if tls { (free_port(), free_port()) } else { (0, 0) };
        let d = dir.path();
        let ssl = if tls {
            make_certs(d);
            format!(
                "ssl = yes\nssl_server_cert_file = {d}/server.crt\nssl_server_key_file = {d}/server.key\n",
                d = d.display()
            )
        } else {
            "ssl = no\n".to_string()
        };
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap().trim().to_string();
        let group = String::from_utf8(Command::new("id").arg("-gn").output().unwrap().stdout).unwrap().trim().to_string();
        std::fs::write(d.join("users"), format!("{USER}:{{PLAIN}}{PASSWORD}\n")).unwrap();
        let conf = format!(
            r#"dovecot_config_version = 2.4.0
dovecot_storage_version = 2.4.0
base_dir = {run}
state_dir = {d}/state
log_path = {d}/dovecot.log
default_internal_user = {user}
default_internal_group = {group}
default_login_user = {user}
protocols = imap pop3
listen = 127.0.0.1
{ssl}auth_allow_cleartext = yes
auth_mechanisms = plain login
passdb passwd-file {{
  passwd_file_path = {d}/users
}}
userdb static {{
  fields {{
    uid = {user}
    gid = {group}
    home = {d}/home/%{{user}}
  }}
}}
mail_driver = maildir
mail_path = {d}/home/%{{user}}/Maildir
pop3_uidl_format = %{{uid | hex(8)}}%{{uidvalidity | hex(8)}}
service imap-login {{
  chroot =
  inet_listener imap {{
    port = {imap_port}
  }}
  inet_listener imaps {{
    port = {imaps_port}
    ssl = yes
  }}
}}
service pop3-login {{
  chroot =
  inet_listener pop3 {{
    port = {pop3_port}
  }}
  inet_listener pop3s {{
    port = {pop3s_port}
    ssl = yes
  }}
}}
service anvil {{
  chroot =
}}
"#,
            run = run_dir.display(),
            d = d.display(),
        );
        std::fs::create_dir_all(d.join("state")).unwrap();
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(d.join("home").join(USER).join("Maildir").join(sub)).unwrap();
        }
        std::fs::write(d.join("dovecot.conf"), conf).unwrap();
        let child = Command::new("dovecot")
            .args(["-F", "-c"])
            .arg(d.join("dovecot.conf"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let ca_file = tls.then(|| dir.path().join("ca.crt"));
        let dv = Dovecot { child, dir, run_dir, imap_port, pop3_port, imaps_port, pop3s_port, ca_file, counter: std::cell::Cell::new(0) };
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", imap_port)).is_err() || std::net::TcpStream::connect(("127.0.0.1", pop3_port)).is_err() {
            assert!(Instant::now() < deadline, "dovecot did not start: {}", dv.log());
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(dv)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("dovecot.log")).unwrap_or_default()
    }

    pub fn maildir(&self, folder: Option<&str>) -> PathBuf {
        let base = self.dir.path().join("home").join(USER).join("Maildir");
        match folder {
            None => base,
            Some(f) => base.join(format!(".{f}")),
        }
    }

    /// Creates a Maildir++ folder (".Name"; "." separates levels).
    pub fn create_folder(&self, name: &str) {
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(self.maildir(Some(name)).join(sub)).unwrap();
        }
    }

    /// Delivers a message the way an MDA would (tmp/ then rename into new/).
    pub fn deliver(&self, folder: Option<&str>, subject: &str, body: &str) -> PathBuf {
        let n = self.counter.get() + 1;
        self.counter.set(n);
        let name = format!("{}.M{n}P{}.orion-test", 1_700_000_000 + n, std::process::id());
        let dir = self.maildir(folder);
        let raw = format!(
            "From: Bob <bob@example.org>\r\nTo: ana@example.org\r\nSubject: {subject}\r\nDate: Thu, 20 Nov 2025 10:{:02}:00 +0000\r\n\
             Message-ID: <{n}@orion-test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n",
            n % 60
        );
        let tmp = dir.join("tmp").join(&name);
        std::fs::write(&tmp, raw).unwrap();
        let dest = dir.join("new").join(&name);
        std::fs::rename(&tmp, &dest).unwrap();
        dest
    }

    /// Deletes a delivered message wherever Dovecot moved it (new/ or cur/).
    pub fn remove(&self, folder: Option<&str>, delivered: &Path) {
        let stem = delivered.file_name().unwrap().to_string_lossy().to_string();
        for sub in ["new", "cur"] {
            for e in std::fs::read_dir(self.maildir(folder).join(sub)).unwrap().flatten() {
                if e.file_name().to_string_lossy().starts_with(&stem) {
                    std::fs::remove_file(e.path()).unwrap();
                }
            }
        }
    }

    pub fn imap_config(&self, password: &str) -> orion_core::config::ImapConfig {
        orion_core::config::ImapConfig {
            host: "127.0.0.1".into(),
            port: Some(self.imap_port),
            tls: orion_core::config::TlsMode::None,
            user: USER.into(),
            password: Some(password.into()),
            password_command: None,
            poll_interval: 60,
            cache_bodies: true,
            idle: true,
            oauth: None,
            account: String::new(),
        }
    }

    pub fn pop3_config(&self, password: &str) -> orion_core::config::ImapConfig {
        orion_core::config::ImapConfig { port: Some(self.pop3_port), ..self.imap_config(password) }
    }
}

/// Test CA + server certificate valid only for IP 127.0.0.1.
pub fn make_certs(d: &Path) {
    let run = |args: &[&str]| {
        let out = Command::new("openssl").args(args).current_dir(d).output().unwrap();
        assert!(out.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(&["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "ca.key", "-out", "ca.crt", "-days", "2", "-subj", "/CN=Orion Test CA"]);
    run(&["req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "server.key", "-out", "server.csr", "-subj", "/CN=127.0.0.1"]);
    std::fs::write(d.join("ext.cnf"), "subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n").unwrap();
    run(&["x509", "-req", "-in", "server.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-out", "server.crt", "-days", "2", "-extfile", "ext.cnf"]);
}

impl Drop for Dovecot {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.run_dir);
    }
}
