//! Mail backends: mbox and IMAP.

pub mod imap;
pub mod mbox;
pub(crate) mod net;
pub mod autoconfig;
pub mod jmap;
pub mod oauth;
pub mod pop3;
pub mod providers;
pub mod remote;
pub mod secrets;
pub mod smtp;
pub(crate) mod tls;

use orion_core::config::{AccountConfig, AccountKind};
use orion_core::Backend;

/// Creates the right backend for an account.
pub fn open(account: &AccountConfig) -> orion_core::Result<Box<dyn Backend>> {
    match &account.kind {
        AccountKind::Mbox { path } => Ok(Box::new(mbox::MboxBackend::new(path.clone()))),
        AccountKind::Imap(cfg) => Ok(Box::new(imap::ImapBackend::new(cfg.clone())?)),
        AccountKind::Pop3(cfg) => Ok(Box::new(pop3::Pop3Backend::new(cfg.clone()))),
        AccountKind::Jmap(cfg) => Ok(Box::new(jmap::JmapBackend::new(cfg.clone()))),
    }
}

/// Stable 63-bit message id from a server-side string id (FNV-1a), for
/// protocols whose ids are not numbers (POP3 UIDL, JMAP Email id).
pub fn stable_id(s: &str) -> orion_core::model::MessageId {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    orion_core::model::MessageId(h & (u64::MAX >> 1))
}
