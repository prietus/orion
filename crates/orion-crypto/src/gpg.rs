//! Wrapper around `gpg` using its machine interface (`--status-fd`).
//!
//! Secret-key operations (including keys on a YubiKey/OpenPGP card, via
//! scdaemon) are first attempted without prompting (`--pinentry-mode error`):
//! if the agent already has the PIN cached, they succeed. Otherwise
//! [`GpgError::NeedPin`] is returned and the UI asks for it; the retry passes it
//! via loopback, so no terminal pinentry ever breaks the screen.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use orion_core::config::PinentryMode;
use orion_core::model::{SigStatus, SignatureInfo, Trust};

#[derive(Debug, Clone)]
pub struct Gpg {
    pub program: String,
    pub home: Option<PathBuf>,
    pub pinentry: PinentryMode,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpgError {
    /// The executable was not found.
    NotFound(String),
    NeedPin,
    BadPin,
    PinBlocked,
    CardMissing,
    NotConfirmed,
    NoSecretKey,
    Canceled,
    Timeout,
    Failed(String),
}

impl std::fmt::Display for GpgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpgError::NotFound(p) => write!(f, "could not find {p}"),
            GpgError::NeedPin => write!(f, "PIN or passphrase required"),
            GpgError::BadPin => write!(f, "wrong PIN or passphrase"),
            GpgError::PinBlocked => write!(f, "PIN blocked"),
            GpgError::CardMissing => write!(f, "card not available"),
            GpgError::NotConfirmed => write!(f, "operation not confirmed on the card"),
            GpgError::NoSecretKey => write!(f, "secret key not available"),
            GpgError::Canceled => write!(f, "canceled"),
            GpgError::Timeout => write!(f, "timed out"),
            GpgError::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// Result of decrypting (or of processing a clearsigned text).
#[derive(Debug, Clone, Default)]
pub struct Decrypted {
    pub plaintext: Vec<u8>,
    /// Key IDs of the recipients (`ENC_TO`).
    pub recipients: Vec<String>,
    pub encrypted: bool,
    pub signatures: Vec<SignatureInfo>,
}

/// A `[GNUPG:] KEYWORD args...` line.
#[derive(Debug, Clone)]
pub struct Status {
    pub keyword: String,
    pub args: Vec<String>,
}

pub struct Output {
    pub stdout: Vec<u8>,
    pub status: Vec<Status>,
    pub stderr: String,
    pub success: bool,
}

/// A public key in the keyring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    pub fingerprint: String,
    /// The user id that matched the address.
    pub uid: String,
    /// gpg validity: 'f' full, 'u' ultimate, 'm' marginal, '-'/'q'/'o' unknown.
    pub validity: char,
}

impl KeyInfo {
    pub fn certified(&self) -> bool {
        matches!(self.validity, 'f' | 'u')
    }
}

/// Fingerprint in groups of four, as people compare them.
pub fn pretty_fingerprint(fpr: &str) -> String {
    fpr.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// Information about an OpenPGP card (`gpg --card-status`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardInfo {
    pub reader: String,
    pub vendor: String,
    pub serial: String,
    pub holder: String,
    /// Fingerprints of the signing, encryption and authentication keys.
    pub fingerprints: [Option<String>; 3],
    /// Remaining attempts: user PIN, reset code, admin PIN.
    pub pin_retries: [Option<u8>; 3],
    pub signatures_made: Option<u32>,
}

impl Gpg {
    pub fn new(program: impl Into<String>) -> Self {
        Gpg { program: program.into(), home: None, pinentry: PinentryMode::Orion, timeout: Duration::from_secs(60) }
    }

    pub fn from_config(cfg: &orion_core::config::PgpConfig) -> Self {
        Gpg {
            program: cfg.gpg.clone(),
            home: cfg.homedir.clone(),
            pinentry: cfg.pinentry,
            timeout: Duration::from_secs(cfg.timeout.max(5)),
        }
    }

    /// Runs gpg. If `pin` is given it is passed via loopback (`--passphrase-fd 0`)
    /// and the data goes into a private temporary file; otherwise via stdin.
    pub fn run(&self, args: &[&str], input: &[u8], pin: Option<&str>) -> Result<Output, GpgError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(["--batch", "--no-tty", "--status-fd", "2", "--no-auto-key-retrieve", "--exit-on-status-write-error"]);
        if let Some(home) = &self.home {
            cmd.env("GNUPGHOME", home);
        }
        // Stable English messages (status lines already are).
        cmd.env("LC_ALL", "C");
        let mut _tmp = None;
        let stdin_data: Zeroizing<Vec<u8>> = match (pin, self.pinentry) {
            (Some(pin), _) => {
                let dir = tempfile::Builder::new().prefix("orion-gpg-").tempdir().map_err(|e| GpgError::Failed(e.to_string()))?;
                let path = dir.path().join("in");
                std::fs::write(&path, input).map_err(|e| GpgError::Failed(e.to_string()))?;
                // With a file argument gpg would write sign/encrypt output to
                // "<file>.asc"; force stdout like the stdin path.
                cmd.args(["--pinentry-mode", "loopback", "--passphrase-fd", "0", "--output", "-"]);
                cmd.args(args);
                cmd.arg(&path);
                _tmp = Some(dir);
                Zeroizing::new(format!("{pin}\n").into_bytes())
            }
            (None, PinentryMode::Orion) => {
                cmd.args(["--pinentry-mode", "error"]);
                cmd.args(args);
                Zeroizing::new(input.to_vec())
            }
            (None, PinentryMode::Agent) => {
                cmd.args(args);
                Zeroizing::new(input.to_vec())
            }
        };
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => GpgError::NotFound(self.program.clone()),
            _ => GpgError::Failed(format!("{}: {e}", self.program)),
        })?;

        // Write and reads on threads: otherwise a large output would block the pipes.
        let mut stdin = child.stdin.take().expect("stdin");
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&stdin_data);
        });
        let mut out = child.stdout.take().expect("stdout");
        let mut err = child.stderr.take().expect("stderr");
        let reader_out = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = out.read_to_end(&mut v);
            v
        });
        let reader_err = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = err.read_to_end(&mut v);
            v
        });

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GpgError::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(GpgError::Failed(e.to_string())),
            }
        };
        let _ = writer.join();
        let stdout = reader_out.join().unwrap_or_default();
        let stderr_raw = reader_err.join().unwrap_or_default();
        let mut statuses = Vec::new();
        let mut human = String::new();
        for line in String::from_utf8_lossy(&stderr_raw).lines() {
            match line.strip_prefix("[GNUPG:] ") {
                Some(rest) => {
                    let mut it = rest.split(' ');
                    let keyword = it.next().unwrap_or_default().to_string();
                    statuses.push(Status { keyword, args: it.map(str::to_string).collect() });
                }
                None => {
                    human.push_str(line);
                    human.push('\n');
                }
            }
        }
        Ok(Output { stdout, status: statuses, stderr: human, success: status.success() })
    }

    /// Decrypts (or processes a clearsigned message). On error it also returns
    /// the recipients, so we know which key/card to ask for.
    pub fn decrypt(&self, data: &[u8], pin: Option<&str>) -> Result<Decrypted, (GpgError, Vec<String>)> {
        let out = self.run(&["--decrypt"], data, pin).map_err(|e| (e, Vec::new()))?;
        let encrypted = out.status.iter().any(|s| matches!(s.keyword.as_str(), "ENC_TO" | "BEGIN_DECRYPTION"));
        let recipients: Vec<String> = out.status.iter().filter(|s| s.keyword == "ENC_TO").filter_map(|s| s.args.first().cloned()).collect();
        let ok = out.status.iter().any(|s| s.keyword == "DECRYPTION_OKAY");
        let signatures = parse_signatures(&out.status);
        if (encrypted && ok) || (!encrypted && !signatures.is_empty() && !out.stdout.is_empty()) {
            return Ok(Decrypted { plaintext: out.stdout, recipients, encrypted, signatures });
        }
        Err((classify(&out), recipients))
    }

    /// Verifies a detached signature (PGP/MIME `multipart/signed`).
    pub fn verify_detached(&self, signed: &[u8], signature: &[u8]) -> Result<Vec<SignatureInfo>, GpgError> {
        let dir = tempfile::Builder::new().prefix("orion-gpg-").tempdir().map_err(|e| GpgError::Failed(e.to_string()))?;
        let sig = dir.path().join("sig.asc");
        std::fs::write(&sig, signature).map_err(|e| GpgError::Failed(e.to_string()))?;
        let sig = sig.to_string_lossy().into_owned();
        // The signed data goes through stdin ("-").
        let out = self.run(&["--verify", &sig, "-"], signed, None)?;
        let sigs = parse_signatures(&out.status);
        if sigs.is_empty() {
            return Err(classify(&out));
        }
        Ok(sigs)
    }

    /// Encrypts to `recipients` (and signs with `sign_as` if given). Armored output.
    pub fn encrypt(&self, data: &[u8], recipients: &[&str], sign_as: Option<&str>, pin: Option<&str>) -> Result<Vec<u8>, GpgError> {
        let mut args = vec!["--armor", "--trust-model", "auto"];
        for r in recipients {
            args.extend(["--recipient", r]);
        }
        if let Some(s) = sign_as {
            args.extend(["--local-user", s, "--sign"]);
        }
        args.push("--encrypt");
        let out = self.run(&args, data, pin)?;
        if out.success && !out.stdout.is_empty() && out.status.iter().any(|s| s.keyword == "END_ENCRYPTION") {
            return Ok(out.stdout);
        }
        Err(classify(&out))
    }

    /// Armored detached signature (for `multipart/signed`).
    pub fn sign_detached(&self, data: &[u8], signer: &str, pin: Option<&str>) -> Result<Vec<u8>, GpgError> {
        self.sign_detached_micalg(data, signer, pin).map(|(sig, _)| sig)
    }

    /// Like `sign_detached`, also returning the `micalg` value for the hash
    /// gpg actually used (RFC 3156 requires it to match).
    pub fn sign_detached_micalg(&self, data: &[u8], signer: &str, pin: Option<&str>) -> Result<(Vec<u8>, &'static str), GpgError> {
        let out = self.run(&["--armor", "--local-user", signer, "--detach-sign"], data, pin)?;
        if let (true, Some(created)) = (out.success && !out.stdout.is_empty(), out.status.iter().find(|s| s.keyword == "SIG_CREATED")) {
            // SIG_CREATED <type> <pk_algo> <hash_algo> <class> <timestamp> <fpr>
            let micalg = match created.args.get(2).map(String::as_str) {
                Some("2") => "pgp-sha1",
                Some("8") => "pgp-sha256",
                Some("9") => "pgp-sha384",
                Some("10") => "pgp-sha512",
                Some("11") => "pgp-sha224",
                _ => "pgp-sha256",
            };
            return Ok((out.stdout, micalg));
        }
        Err(classify(&out))
    }

    /// Addresses (from `addrs`) without a usable encryption key in the keyring.
    pub fn missing_keys(&self, addrs: &[String]) -> Vec<String> {
        addrs
            .iter()
            .filter(|a| {
                let Ok(out) = self.run(&["--with-colons", "--list-keys", "--", &format!("<{a}>")], b"", None) else { return true };
                let text = String::from_utf8_lossy(&out.stdout);
                // A usable key: some pub/sub key with the encrypt capability, not
                // revoked (r), expired (e), disabled (d) or invalid (i).
                !text.lines().any(|l| {
                    let f: Vec<&str> = l.split(':').collect();
                    matches!(f.first(), Some(&"pub") | Some(&"sub"))
                        && !matches!(f.get(1), Some(&"r") | Some(&"e") | Some(&"d") | Some(&"i"))
                        && f.get(11).is_some_and(|caps| caps.contains('E') || caps.contains('e'))
                })
            })
            .cloned()
            .collect()
    }

    /// The usable encryption key for an address, with its validity in the
    /// keyring ('f'/'u' = certified; anything else = not verified).
    pub fn encryption_key(&self, addr: &str) -> Option<KeyInfo> {
        // An address is matched exactly; anything else (fingerprint, key id) as is.
        let spec = if addr.contains('@') { format!("<{addr}>") } else { addr.to_string() };
        let out = self.run(&["--with-colons", "--fixed-list-mode", "--list-keys", "--", &spec], b"", None).ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut found: Option<KeyInfo> = None;
        let mut current: Option<KeyInfo> = None;
        let mut usable = false;
        let mut want_fpr = false;
        let flush = |k: Option<KeyInfo>, usable: bool, found: &mut Option<KeyInfo>| {
            if let Some(k) = k.filter(|_| usable)
                && found.is_none()
            {
                *found = Some(k);
            }
        };
        for line in text.lines() {
            let f: Vec<&str> = line.split(':').collect();
            match f.first().copied() {
                Some("pub") => {
                    flush(current.take(), usable, &mut found);
                    let bad = matches!(f.get(1), Some(&"r") | Some(&"e") | Some(&"d") | Some(&"i"));
                    usable = !bad && f.get(11).is_some_and(|c| c.contains('E'));
                    current = Some(KeyInfo { fingerprint: String::new(), uid: String::new(), validity: f.get(1).and_then(|v| v.chars().next()).unwrap_or('-') });
                    want_fpr = true;
                }
                Some("fpr") if want_fpr => {
                    if let Some(k) = current.as_mut() {
                        k.fingerprint = f.get(9).unwrap_or(&"").to_string();
                    }
                    want_fpr = false;
                }
                Some("uid") => {
                    if let Some(k) = current.as_mut().filter(|k| k.uid.is_empty()) {
                        let uid = f.get(9).unwrap_or(&"").replace("\\x3a", ":");
                        if !addr.contains('@') || uid.to_ascii_lowercase().contains(&addr.to_ascii_lowercase()) {
                            k.uid = uid;
                            k.validity = f.get(1).and_then(|v| v.chars().next()).unwrap_or(k.validity);
                        }
                    }
                }
                Some("sub") => want_fpr = false,
                _ => {}
            }
        }
        flush(current, usable, &mut found);
        found
    }

    /// Looks up keys for `addrs`: first WKD (published by the address's own
    /// domain), then each keyserver in order. Only run when the user asks: it
    /// contacts those servers. Returns the addresses for which a key arrived.
    pub fn locate_keys(&self, addrs: &[String], keyservers: &[String]) -> Vec<String> {
        addrs
            .iter()
            .filter(|a| {
                let wkd = self.run(&["--auto-key-locate", "clear,wkd", "--locate-external-keys", "--", a], b"", None).is_ok_and(|o| o.success);
                wkd || keyservers.iter().any(|ks| {
                    self.run(&["--auto-key-locate", "clear,keyserver", "--keyserver", ks, "--locate-external-keys", "--", a], b"", None)
                        .is_ok_and(|o| o.success)
                })
            })
            .cloned()
            .collect()
    }

    /// Encrypts to exact keys (fingerprints). With `trust_listed`, gpg is told
    /// to trust them without certification: only used for fingerprints the
    /// user explicitly accepted, never for "any key with this address".
    pub fn encrypt_to_keys(&self, data: &[u8], fingerprints: &[String], trust_listed: bool, sign_as: Option<&str>, pin: Option<&str>) -> Result<Vec<u8>, GpgError> {
        let mut args: Vec<&str> = vec!["--armor", "--trust-model", if trust_listed { "always" } else { "auto" }];
        // A primary-key fingerprint pins the certificate; gpg picks its
        // encryption subkey ("FPR!" would demand the primary key itself).
        for f in fingerprints {
            args.extend(["--recipient", f.as_str()]);
        }
        if let Some(s) = sign_as {
            args.extend(["--local-user", s, "--sign"]);
        }
        args.push("--encrypt");
        let out = self.run(&args, data, pin)?;
        if out.success && !out.stdout.is_empty() && out.status.iter().any(|s| s.keyword == "END_ENCRYPTION") {
            return Ok(out.stdout);
        }
        Err(classify(&out))
    }

    /// Keys inside `data` without importing them: (fingerprint, first user id).
    pub fn inspect_keys(&self, data: &[u8]) -> Vec<(String, String)> {
        let Ok(out) = self.run(&["--with-colons", "--import-options", "show-only", "--import"], data, None) else { return Vec::new() };
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut keys: Vec<(String, String)> = Vec::new();
        let mut want_fpr = false;
        for l in text.lines() {
            let f: Vec<&str> = l.split(':').collect();
            match f.first().copied() {
                Some("pub") => {
                    keys.push((String::new(), String::new()));
                    want_fpr = true;
                }
                Some("fpr") if want_fpr => {
                    if let Some(k) = keys.last_mut() {
                        k.0 = f.get(9).unwrap_or(&"").to_string();
                    }
                    want_fpr = false;
                }
                Some("sub") => want_fpr = false,
                Some("uid") => {
                    if let Some(k) = keys.last_mut().filter(|k| k.1.is_empty()) {
                        k.1 = f.get(9).unwrap_or(&"").replace("\\x3a", ":");
                    }
                }
                _ => {}
            }
        }
        keys.retain(|k| !k.0.is_empty());
        keys
    }

    /// Imports public keys; returns the fingerprints gpg accepted.
    pub fn import_keys(&self, data: &[u8]) -> Result<Vec<String>, GpgError> {
        let out = self.run(&["--import-options", "import-minimal", "--import"], data, None)?;
        let fprs: Vec<String> = out.status.iter().filter(|s| s.keyword == "IMPORT_OK").filter_map(|s| s.args.get(1).cloned()).collect();
        if fprs.is_empty() {
            return Err(classify(&out));
        }
        Ok(fprs)
    }

    /// Downloads one exact key by fingerprint from the first keyserver that has it.
    pub fn recv_key(&self, fingerprint: &str, keyservers: &[String]) -> Result<Vec<String>, GpgError> {
        let mut last = GpgError::Failed("no keyserver configured".into());
        for ks in keyservers {
            match self.run(&["--keyserver", ks, "--recv-keys", "--", fingerprint], b"", None) {
                Ok(out) => {
                    let fprs: Vec<String> = out.status.iter().filter(|s| s.keyword == "IMPORT_OK").filter_map(|s| s.args.get(1).cloned()).collect();
                    if fprs.iter().any(|f| f.eq_ignore_ascii_case(fingerprint)) {
                        return Ok(fprs);
                    }
                    last = GpgError::Failed(format!("{ks} does not have key {fingerprint}"));
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Minimal binary export of a public key, for the `Autocrypt:` header.
    pub fn export_minimal(&self, spec: &str) -> Option<Vec<u8>> {
        let out = self.run(&["--export-options", "export-minimal", "--export", "--", spec], b"", None).ok()?;
        (out.success && !out.stdout.is_empty()).then_some(out.stdout)
    }

    /// The main user id of the key that owns a (sub)key fingerprint, e.g. the
    /// signature key a card reports.
    pub fn owner_of(&self, fingerprint: &str) -> Option<String> {
        let out = self.run(&["--with-colons", "--list-keys", "--", fingerprint], b"", None).ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.starts_with("uid:"))
            .and_then(|l| l.split(':').nth(9))
            .map(|u| u.replace("\\x3a", ":"))
    }

    /// Card serial of the secret signing key for a user id (e.g. an address).
    pub fn card_serial_for_user(&self, user: &str) -> Option<String> {
        let out = self.run(&["--with-colons", "--list-secret-keys", "--", &format!("<{user}>")], b"", None).ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.split(':').map(str::to_string).collect::<Vec<_>>())
            .filter(|f| matches!(f.first().map(String::as_str), Some("sec") | Some("ssb")))
            .filter(|f| f.get(11).is_some_and(|c| c.contains('s') || c.contains('S')))
            .find_map(|f| f.get(14).cloned().filter(|s| s.len() > 1))
    }

    /// If the secret (sub)key `key_id` lives on a card, returns the card's serial number.
    pub fn card_serial_for(&self, key_id: &str) -> Option<String> {
        let out = self.run(&["--with-colons", "--list-secret-keys", "--", key_id], b"", None).ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let key_id = key_id.to_ascii_uppercase();
        text.lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .filter(|f| matches!(f.first(), Some(&"sec") | Some(&"ssb")))
            .find(|f| f.get(4).is_some_and(|id| id.ends_with(&key_id) || key_id.ends_with(id)))
            .and_then(|f| f.get(14).map(|s| s.to_string()))
            .filter(|s| s.len() > 1) // "+" = local key, "#" = not available
    }

    pub fn card_status(&self) -> Result<Option<CardInfo>, GpgError> {
        let out = self.run(&["--with-colons", "--card-status"], b"", None)?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.success {
            // Without a card gpg emits CARDCTRL 4 (or 5 with no reader) and still an "AID:::" line.
            return match classify(&out) {
                GpgError::CardMissing => Ok(None),
                GpgError::Failed(ref m) if m.contains("No such device") || m.contains("not available") => Ok(None),
                other => Err(other),
            };
        }
        let info = parse_card_status(&text);
        Ok((!info.serial.is_empty() || !info.reader.is_empty()).then_some(info))
    }
}

impl CardInfo {
    /// "YubiKey #12345678": from the full AID if that is what we have, else
    /// from the vendor name gpg reports next to the short serial.
    pub fn name(&self) -> String {
        if self.serial.len() == 32 {
            return describe_card(&self.serial);
        }
        let vendor = self.vendor.to_ascii_lowercase();
        let kind = if vendor.contains("yubico") {
            "YubiKey".to_string()
        } else if vendor.contains("nitrokey") {
            "Nitrokey".to_string()
        } else if self.vendor.is_empty() {
            "OpenPGP card".to_string()
        } else {
            format!("{} card", self.vendor)
        };
        format!("{kind} #{}", self.serial.trim_start_matches('0'))
    }
}

/// Human-readable card name from the OpenPGP serial number (AID).
pub fn describe_card(serial: &str) -> String {
    // AID: D2 76 00 01 24 01 | version (2) | manufacturer (2) | serial (4) | 00 00
    if serial.len() == 32 && serial.to_ascii_uppercase().starts_with("D27600012401") {
        let manufacturer = &serial[16..20];
        let number = serial[20..28].trim_start_matches('0');
        let name = match manufacturer {
            "0006" => "YubiKey",
            "000F" | "000f" => "Nitrokey",
            "0005" => "ZeitControl",
            _ => "OpenPGP card",
        };
        return format!("{name} #{number}");
    }
    format!("card {serial}")
}

pub fn parse_card_status(text: &str) -> CardInfo {
    let mut c = CardInfo::default();
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        let get = |i: usize| f.get(i).copied().unwrap_or_default().to_string();
        match f.first().copied().unwrap_or_default() {
            "Reader" => c.reader = get(1),
            "vendor" => c.vendor = get(2),
            "serial" => c.serial = get(1),
            "name" => c.holder = format!("{} {}", get(1), get(2)).trim().to_string(),
            "fpr" => {
                for (i, slot) in c.fingerprints.iter_mut().enumerate() {
                    let v = get(i + 1);
                    *slot = (!v.is_empty()).then_some(v);
                }
            }
            "pinretry" => {
                for (i, slot) in c.pin_retries.iter_mut().enumerate() {
                    *slot = get(i + 1).parse().ok();
                }
            }
            "sigcount" => c.signatures_made = get(1).parse().ok(),
            _ => {}
        }
    }
    c
}

pub fn parse_signatures(status: &[Status]) -> Vec<SignatureInfo> {
    let mut sigs: Vec<SignatureInfo> = Vec::new();
    for s in status {
        let arg = |i: usize| s.args.get(i).cloned().unwrap_or_default();
        let rest = |i: usize| s.args.get(i..).map(|r| r.join(" ")).unwrap_or_default();
        let new = |status: SigStatus| SignatureInfo {
            status,
            key_id: arg(0),
            fingerprint: None,
            signer: rest(1),
            trust: Trust::Undefined,
            created: None,
        };
        match s.keyword.as_str() {
            "GOODSIG" => sigs.push(new(SigStatus::Good)),
            "BADSIG" => sigs.push(new(SigStatus::Bad)),
            "EXPSIG" => sigs.push(new(SigStatus::Expired)),
            "EXPKEYSIG" => sigs.push(new(SigStatus::ExpiredKey)),
            "REVKEYSIG" => sigs.push(new(SigStatus::RevokedKey)),
            "ERRSIG" => {
                // ERRSIG <keyid> <algo> <hash> <class> <time> <rc> <fpr>
                let mut sig = new(if arg(5) == "9" { SigStatus::UnknownKey } else { SigStatus::Error });
                sig.signer = String::new();
                sig.created = arg(4).parse().ok();
                sig.fingerprint = s.args.get(6).filter(|f| f.len() >= 32).cloned();
                sigs.push(sig);
            }
            "VALIDSIG" => {
                if let Some(last) = sigs.last_mut() {
                    last.fingerprint = Some(arg(0));
                    last.created = arg(2).parse().ok();
                }
            }
            t if t.starts_with("TRUST_") => {
                if let Some(last) = sigs.last_mut() {
                    last.trust = match t {
                        "TRUST_ULTIMATE" => Trust::Ultimate,
                        "TRUST_FULLY" => Trust::Full,
                        "TRUST_MARGINAL" => Trust::Marginal,
                        "TRUST_NEVER" => Trust::Never,
                        _ => Trust::Undefined,
                    };
                }
            }
            _ => {}
        }
    }
    sigs
}

/// Maps libgpg-error codes (low 16 bits) to useful cases.
fn classify(out: &Output) -> GpgError {
    let mut no_seckey = false;
    for s in &out.status {
        match s.keyword.as_str() {
            "ERROR" | "FAILURE" => {
                let code = s.args.get(1).and_then(|c| c.parse::<u32>().ok()).unwrap_or(0) & 0xFFFF;
                match code {
                    85 | 178 => return GpgError::NeedPin,
                    11 | 87 => return GpgError::BadPin,
                    130 => return GpgError::PinBlocked,
                    91 | 108 | 109 | 110 | 111 | 112 => return GpgError::CardMissing,
                    114 | 131 => return GpgError::NotConfirmed,
                    62 => return GpgError::Timeout,
                    99 | 198 => return GpgError::Canceled,
                    _ => {}
                }
            }
            "NO_SECKEY" => no_seckey = true,
            "CARDCTRL" if s.args.first().is_some_and(|a| a == "4" || a == "5") => return GpgError::CardMissing,
            "NO_DATA" => return GpgError::Failed("no OpenPGP data found".into()),
            "INV_RECP" => return GpgError::Failed(format!("invalid recipient: {}", s.args.get(1).cloned().unwrap_or_default())),
            "INV_SGNR" => return GpgError::Failed(format!("invalid signer: {}", s.args.get(1).cloned().unwrap_or_default())),
            _ => {}
        }
    }
    if no_seckey {
        return GpgError::NoSecretKey;
    }
    let msg = out.stderr.lines().rev().find(|l| l.starts_with("gpg:")).unwrap_or("gpg failed").trim_start_matches("gpg: ");
    GpgError::Failed(msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_names() {
        assert_eq!(describe_card("D2760001240103040006123456780000"), "YubiKey #12345678");
        assert_eq!(describe_card("D276000124010303000F001234560000"), "Nitrokey #123456");
    }

    #[test]
    fn card_status_colons() {
        let txt = "Reader:Yubico YubiKey OTP FIDO CCID 00 00:AID:D2760001240103040006123456780000:openpgp-card\n\
                   version:0304\nvendor:0006:Yubico:\nserial:12345678:\nname:Ana:García:\n\
                   fpr:AAAA:BBBB::\npinretry:3:0:3:\nsigcount:42:::\n";
        let c = parse_card_status(txt);
        assert_eq!(c.vendor, "Yubico");
        assert_eq!(c.holder, "Ana García");
        assert_eq!(c.fingerprints, [Some("AAAA".into()), Some("BBBB".into()), None]);
        assert_eq!(c.pin_retries, [Some(3), Some(0), Some(3)]);
        assert_eq!(c.signatures_made, Some(42));
    }
}

#[cfg(test)]
mod card_name_tests {
    use super::CardInfo;

    #[test]
    fn card_names() {
        let short = CardInfo { vendor: "Yubico".into(), serial: "12345678".into(), ..Default::default() };
        assert_eq!(short.name(), "YubiKey #12345678");
        let aid = CardInfo { serial: "D2760001240100000006123456780000".into(), ..Default::default() };
        assert_eq!(aid.name(), "YubiKey #12345678");
        let other = CardInfo { vendor: "ACME".into(), serial: "0042".into(), ..Default::default() };
        assert_eq!(other.name(), "ACME card #42");
    }

    #[test]
    fn card_matches_recipient() {
        let card = CardInfo { fingerprints: [None, Some("AAAABBBBCCCCDDDDEEEEFFFF00001111BDF6440F385C464F".into()), None], ..Default::default() };
        assert!(crate::mime::card_matches(&card, &["BDF6440F385C464F".into()]));
        assert!(!crate::mime::card_matches(&card, &["15C387D909802741".into()]));
        assert!(!crate::mime::card_matches(&CardInfo::default(), &["BDF6440F385C464F".into()]));
    }
}
