use std::collections::HashSet;

use crate::model::{Envelope, Flag, MailboxInfo, MessageId, SyncEvent, SyncResult, SyncState};
use crate::{Error, Result};

/// Common interface for every mail source (mbox, IMAP, POP3, JMAP...).
///
/// It is synchronous: each account lives in its own worker thread, so the UI
/// never blocks even when the backend does network I/O.
pub trait Backend: Send {
    /// Lists the mailboxes available in the account, with their counters.
    fn mailboxes(&mut self) -> Result<Vec<MailboxInfo>>;

    /// Summary headers of every message in a mailbox.
    fn envelopes(&mut self, mailbox: &str) -> Result<Vec<Envelope>>;

    /// Raw RFC 5322 message (without marking it as read).
    fn fetch_raw(&mut self, mailbox: &str, id: MessageId) -> Result<Vec<u8>>;

    /// Incremental synchronisation from a known state.
    /// `progress(done, total)` reports progress during long downloads.
    /// By default it returns the full list.
    /// `known_ids` are the messages already cached: backends without
    /// server-side sync state (POP3, JMAP) use them to fetch only new headers.
    fn sync(
        &mut self,
        mailbox: &str,
        _known: Option<&SyncState>,
        _known_ids: &HashSet<MessageId>,
        _progress: &mut dyn FnMut(SyncEvent<'_>),
    ) -> Result<SyncResult> {
        Ok(SyncResult::Full { envelopes: self.envelopes(mailbox)?, state: None })
    }

    /// Sets or clears a flag on the server.
    fn set_flag(&mut self, _mailbox: &str, _ids: &[MessageId], _flag: Flag, _on: bool) -> Result<()> {
        Err(Error::Unsupported("this backend is read-only"))
    }

    /// The mailbox where sent mail is kept (IMAP `\Sent`), creating one if
    /// the account has none. `None` if the protocol has no such thing.
    fn sent_mailbox(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    /// The account's sent-mail mailbox, if it has one (never created; for
    /// showing your replies in conversations).
    fn find_sent_mailbox(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    /// The account's trash mailbox, if it has one (never created).
    fn trash_mailbox(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    /// Moves messages to another mailbox of the same account.
    fn move_messages(&mut self, _mailbox: &str, _ids: &[MessageId], _dest: &str) -> Result<()> {
        Err(Error::Unsupported("this account cannot move messages"))
    }

    /// Deletes messages for good (IMAP expunge, POP3 DELE, JMAP destroy).
    fn delete_messages(&mut self, _mailbox: &str, _ids: &[MessageId]) -> Result<()> {
        Err(Error::Unsupported("this account cannot delete messages"))
    }

    /// Deletes every message of a mailbox for good (emptying the Trash).
    fn empty_mailbox(&mut self, _mailbox: &str) -> Result<()> {
        Err(Error::Unsupported("this account cannot empty folders"))
    }

    /// Stores a message in a mailbox (IMAP APPEND).
    fn append(&mut self, _mailbox: &str, _raw: &[u8], _seen: bool) -> Result<()> {
        Err(Error::Unsupported("this account cannot store messages"))
    }

    /// Sends through the account's own protocol (JMAP). Returns `false` when
    /// the protocol cannot send and SMTP must be used instead.
    fn submit(&mut self, _raw: &[u8], _from: &str, _rcpts: &[String]) -> Result<bool> {
        Ok(false)
    }

    /// The server keeps no flags (POP3): read/flagged live only in the local
    /// cache, so counts must be computed from it.
    fn local_flags(&self) -> bool {
        false
    }

    /// Whether envelopes and bodies should be kept in the local cache (remote backends).
    fn cacheable(&self) -> bool {
        false
    }
}
