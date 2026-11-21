//! OpenPGP for Orion through GnuPG: decrypt, verify, encrypt and sign, with
//! local keys or keys on OpenPGP cards (YubiKey, Nitrokey...) via scdaemon.

pub mod accepted;
pub mod autocrypt;
pub mod gpg;
pub mod mime;
pub mod outgoing;

pub use gpg::{describe_card, CardInfo, Gpg, GpgError};
pub use mime::open_message;
