//! Local API of `orion daemon`: a Unix socket speaking JSON, one object per line.
//!
//! Requests: `{"cmd":"status"}`, `{"cmd":"sync"}`, `{"cmd":"subscribe"}`, `{"cmd":"ping"}`.
//! Replies: `{"ok":true,...}` or `{"ok":false,"error":"..."}`.
//! After `subscribe` the connection receives events (`{"event":"new_mail",...}`,
//! `{"event":"counts",...}`, `{"event":"error",...}`) until it is closed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Status,
    Sync,
    /// Re-read the counts from the shared cache (no network): another Orion
    /// changed flags and already wrote them there and to the server.
    Reload,
    Subscribe,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

impl Reply {
    pub fn ok() -> Self {
        Reply { ok: true, error: None, status: None }
    }
    pub fn err(e: impl Into<String>) -> Self {
        Reply { ok: false, error: Some(e.into()), status: None }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    /// Unread messages in watched mailboxes (what a status bar should show).
    pub unread: usize,
    /// Unread messages in all mailboxes.
    pub unread_all: usize,
    pub accounts: Vec<AccountStatus>,
    /// The newest unread messages of the watched mailboxes (at most 10).
    #[serde(default)]
    pub messages: Vec<UnreadMessage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnreadMessage {
    pub account: String,
    pub mailbox: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountStatus {
    pub name: String,
    pub error: Option<String>,
    /// Current activity ("syncing INBOX…"), empty when idle.
    pub activity: String,
    /// Unix time of the last successful mailbox sync.
    pub last_sync: Option<u64>,
    pub mailboxes: Vec<MailboxStatus>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxStatus {
    pub id: String,
    pub name: String,
    pub total: usize,
    pub unread: usize,
    pub watched: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    NewMail { account: String, mailbox: String, messages: Vec<NewMessage> },
    Counts { account: String, unread: usize, mailboxes: Vec<MailboxStatus> },
    Error { account: String, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewMessage {
    pub from: String,
    pub subject: String,
    pub date: String,
}
