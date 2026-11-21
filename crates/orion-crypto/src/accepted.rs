//! Keys the user accepted for encryption without a certification: the
//! address ↔ fingerprint pairs they compared with the owner. Stored one per
//! line ("address fingerprint") in the data directory.

use std::io::Write;
use std::path::{Path, PathBuf};

pub fn default_path() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("orion").join("accepted-keys")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcceptedKeys(Vec<(String, String)>);

impl AcceptedKeys {
    pub fn load(path: &Path) -> AcceptedKeys {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        AcceptedKeys(
            text.lines()
                .filter_map(|l| {
                    let mut it = l.split_whitespace();
                    Some((it.next()?.to_ascii_lowercase(), it.next()?.to_ascii_uppercase()))
                })
                .collect(),
        )
    }

    pub fn contains(&self, addr: &str, fingerprint: &str) -> bool {
        self.0.iter().any(|(a, f)| a.eq_ignore_ascii_case(addr) && f.eq_ignore_ascii_case(fingerprint))
    }

    pub fn accept(path: &Path, addr: &str, fingerprint: &str) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        if Self::load(path).contains(addr, fingerprint) {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
        writeln!(f, "{} {}", addr.to_ascii_lowercase(), fingerprint.to_ascii_uppercase())
    }
}
