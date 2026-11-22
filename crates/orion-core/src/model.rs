/// Opaque identifier of a message within a mailbox. Each backend decides what
/// it means (index in mbox, UID in IMAP, id in JMAP...).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub u64);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub deleted: bool,
    pub draft: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Seen,
    Answered,
    Flagged,
    Deleted,
    Draft,
}

impl Flags {
    pub fn get(&self, f: Flag) -> bool {
        match f {
            Flag::Seen => self.seen,
            Flag::Answered => self.answered,
            Flag::Flagged => self.flagged,
            Flag::Deleted => self.deleted,
            Flag::Draft => self.draft,
        }
    }

    pub fn set(&mut self, f: Flag, on: bool) {
        match f {
            Flag::Seen => self.seen = on,
            Flag::Answered => self.answered = on,
            Flag::Flagged => self.flagged = on,
            Flag::Deleted => self.deleted = on,
            Flag::Draft => self.draft = on,
        }
    }

    /// Compact representation for the cache.
    pub fn bits(&self) -> u8 {
        (self.seen as u8) | (self.answered as u8) << 1 | (self.flagged as u8) << 2 | (self.deleted as u8) << 3 | (self.draft as u8) << 4
    }

    pub fn from_bits(b: u8) -> Self {
        Flags {
            seen: b & 1 != 0,
            answered: b & 2 != 0,
            flagged: b & 4 != 0,
            deleted: b & 8 != 0,
            draft: b & 16 != 0,
        }
    }
}

/// Synchronisation state of a remote mailbox (IMAP: UIDVALIDITY + UIDNEXT).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncState {
    pub validity: u64,
    /// First identifier we do not know yet.
    pub next: u64,
}

/// What a backend reports while syncing.
#[derive(Debug)]
pub enum SyncEvent<'a> {
    Progress { done: usize, total: usize },
    /// Headers downloaded so far in a long first sync, so they can be stored
    /// and shown before the whole mailbox is done (and survive interruptions).
    Batch { validity: u64, envelopes: &'a [Envelope] },
}

#[derive(Debug, Clone)]
pub enum SyncResult {
    /// Replaces everything known about the mailbox.
    Full { envelopes: Vec<Envelope>, state: Option<SyncState> },
    /// Changes since the previous state: new messages and, for the already known
    /// ones, those that still exist with their current flags (any that are
    /// missing were deleted on the server).
    ///
    /// `None` flags mean the server does not know them (POP3): keep the local ones.
    Delta {
        added: Vec<Envelope>,
        existing: Vec<(MessageId, Option<Flags>)>,
        state: SyncState,
    },
}

#[derive(Debug, Clone)]
pub struct MailboxInfo {
    /// Stable identifier within the account (path, IMAP name...).
    pub id: String,
    /// Display name.
    pub name: String,
    pub total: usize,
    pub unread: usize,
}

/// The minimum needed to draw a row of the message list.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub id: MessageId,
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    /// UNIX seconds (UTC). 0 if there is no date.
    pub timestamp: i64,
    /// Date formatted in the message's own time zone.
    pub date: String,
    pub flags: Flags,
    pub size: usize,
    pub has_attachments: bool,
    /// `Message-ID` (without angle brackets), for grouping conversations.
    pub message_id: String,
    /// The messages it answers: `In-Reply-To` and `References`, oldest first.
    pub parents: Vec<String>,
}

impl Envelope {
    pub fn sender(&self) -> &str {
        if self.from_name.is_empty() { &self.from_addr } else { &self.from_name }
    }
}

/// Complete message ready to display.
#[derive(Debug, Clone)]
pub struct MessageView {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub reply_to: String,
    pub date: String,
    pub message_id: String,
    pub raw_headers: String,
    /// Plain-text body (if there is only HTML, converted to text).
    pub text: String,
    /// Original HTML body, if any.
    pub html: Option<String>,
    /// HTML already parsed with its styles, ready for layout.
    pub html_doc: Option<orion_html::Doc>,
    /// Text body as a document (coloured quotes, numbered URLs).
    pub text_doc: orion_html::Doc,
    pub attachments: Vec<Attachment>,
    /// OpenPGP result (encryption and/or signatures), if the message uses it.
    pub crypto: Option<CryptoReport>,
    /// The sender's public key can be imported from this message.
    pub key_offer: Option<KeyOffer>,
}

/// A sender key the user can import to reply signed/encrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOffer {
    /// Sender address the key is for.
    pub addr: String,
    pub fingerprint: String,
    /// User id of the key (as it claims).
    pub uid: String,
    pub source: KeySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// `Autocrypt:` header (binary key).
    Autocrypt(Vec<u8>),
    /// Attached `application/pgp-keys`.
    Attachment(Vec<u8>),
    /// Only the signature's issuer fingerprint: download it from a keyserver.
    Keyserver,
}

impl KeySource {
    pub fn label(&self) -> &'static str {
        match self {
            KeySource::Autocrypt(_) => "Autocrypt",
            KeySource::Attachment(_) => "attached key",
            KeySource::Keyserver => "keyserver",
        }
    }
}

/// What is known about a message's encryption and signatures.
#[derive(Debug, Clone, Default)]
pub struct CryptoReport {
    pub encrypted: bool,
    pub decrypted: bool,
    /// Key IDs the message is encrypted to.
    pub recipients: Vec<String>,
    /// Where the used or required private key lives ("YubiKey #12345678").
    pub key_location: Option<String>,
    pub signatures: Vec<SignatureInfo>,
    /// Why decryption/verification failed.
    pub problem: Option<CryptoProblem>,
    /// Format: "PGP/MIME" or "inline PGP".
    pub format: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoProblem {
    /// The PIN or passphrase is needed; `prompt` says which one.
    NeedPin { prompt: String },
    BadPin { retries: Option<u8> },
    PinBlocked,
    /// The card is missing (or it is a different one).
    CardMissing(String),
    /// The operation was not confirmed (touch on the YubiKey).
    NotConfirmed,
    NoSecretKey(Vec<String>),
    /// A card is connected, but it does not hold the key this message needs.
    WrongCard {
        /// Who/what the message is encrypted to (identity, key id, card if known).
        needed: String,
        /// The connected card and whose key it holds.
        inserted: String,
    },
    GpgMissing,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    pub status: SigStatus,
    pub key_id: String,
    pub fingerprint: Option<String>,
    /// Primary identity of the signing key.
    pub signer: String,
    pub trust: Trust,
    /// UNIX seconds.
    pub created: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigStatus {
    Good,
    Bad,
    /// Valid signature, but expired.
    Expired,
    /// Valid signature made with an expired key.
    ExpiredKey,
    /// Valid signature made with a revoked key.
    RevokedKey,
    /// We do not have the public key.
    UnknownKey,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trust {
    Ultimate,
    Full,
    Marginal,
    Never,
    #[default]
    Undefined,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub inline: bool,
    pub content_id: Option<String>,
    pub data: Vec<u8>,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
    pub fn size(&self) -> usize {
        self.data.len()
    }
}

pub fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes}B") } else { format!("{v:.1}{}", UNITS[u]) }
}
