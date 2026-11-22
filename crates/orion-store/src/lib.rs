//! Local SQLite cache: mailboxes, envelopes, sync state and bodies.
//!
//! Lets the UI show the last known state instantly while the account thread
//! syncs with the server. Each thread opens its own connection; WAL mode
//! allows concurrent reads and writes.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use orion_core::model::{Envelope, Flag, Flags, MailboxInfo, MessageId, SyncResult, SyncState};
use orion_core::{Error, Result};

const SCHEMA_VERSION: i64 = 2;

pub struct Store {
    conn: Connection,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::Cache(e.to_string())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db_err)?;
        // Switching to WAL takes an exclusive lock and does not wait on
        // busy_timeout: retry while another account thread is doing the same.
        let mut attempts = 0;
        loop {
            match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
                Ok(_) => break,
                Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::DatabaseBusy && attempts < 200 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => return Err(db_err(e)),
            }
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;").map_err(db_err)?;
        // Check and (re)create the schema atomically: with several threads
        // opening a new cache at once, only one may create it, and none may
        // drop tables another one is already using.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        Self::migrate(&tx)?;
        tx.commit().map_err(db_err)?;
        Ok(Store { conn })
    }

    fn migrate(conn: &Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(db_err)?;
        // Check the tables too, not only the number: an older Orion still
        // running (a daemon) may have rebuilt them its own way.
        let has_ids: bool = conn
            .query_row("SELECT count(*) FROM pragma_table_info('envelopes') WHERE name = 'msgid'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)
            .map_err(db_err)?;
        if version != SCHEMA_VERSION || !has_ids {
            // It is a cache: if the schema differs it is rebuilt from scratch.
            conn.execute_batch(&format!(
                "DROP TABLE IF EXISTS bodies;
                 DROP TABLE IF EXISTS envelopes;
                 DROP TABLE IF EXISTS mailboxes;
                 CREATE TABLE mailboxes (
                     account  TEXT NOT NULL,
                     id       TEXT NOT NULL,
                     name     TEXT NOT NULL,
                     position INTEGER NOT NULL,
                     total    INTEGER NOT NULL,
                     unread   INTEGER NOT NULL,
                     validity INTEGER,
                     next     INTEGER,
                     PRIMARY KEY (account, id)
                 );
                 CREATE TABLE envelopes (
                     account   TEXT NOT NULL,
                     mailbox   TEXT NOT NULL,
                     uid       INTEGER NOT NULL,
                     from_name TEXT NOT NULL,
                     from_addr TEXT NOT NULL,
                     subject   TEXT NOT NULL,
                     ts        INTEGER NOT NULL,
                     date      TEXT NOT NULL,
                     flags     INTEGER NOT NULL,
                     size      INTEGER NOT NULL,
                     has_att   INTEGER NOT NULL,
                     msgid     TEXT NOT NULL DEFAULT '',
                     refs      TEXT NOT NULL DEFAULT '',
                     PRIMARY KEY (account, mailbox, uid)
                 ) WITHOUT ROWID;
                 CREATE TABLE bodies (
                     account TEXT NOT NULL,
                     mailbox TEXT NOT NULL,
                     uid     INTEGER NOT NULL,
                     raw     BLOB NOT NULL,
                     PRIMARY KEY (account, mailbox, uid)
                 ) WITHOUT ROWID;
                 PRAGMA user_version = {SCHEMA_VERSION};"
            ))
            .map_err(db_err)?;
        }
        Ok(())
    }

    // ---------------------------------------------------------- mailboxes

    pub fn mailboxes(&self, account: &str) -> Result<Vec<MailboxInfo>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT id, name, total, unread FROM mailboxes WHERE account = ? ORDER BY position")
            .map_err(db_err)?;
        st.query_map([account], |r| {
            Ok(MailboxInfo { id: r.get(0)?, name: r.get(1)?, total: r.get::<_, i64>(2)? as usize, unread: r.get::<_, i64>(3)? as usize })
        })
        .map_err(db_err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(db_err)
    }

    /// Saves the mailbox list; deletes the ones that no longer exist (and their messages).
    pub fn save_mailboxes(&mut self, account: &str, list: &[MailboxInfo]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        {
            let existing: Vec<String> = {
                let mut st = tx.prepare("SELECT id FROM mailboxes WHERE account = ?").map_err(db_err)?;
                st.query_map([account], |r| r.get(0)).map_err(db_err)?.collect::<rusqlite::Result<_>>().map_err(db_err)?
            };
            for id in existing.iter().filter(|id| !list.iter().any(|m| &m.id == *id)) {
                for table in ["envelopes", "bodies"] {
                    tx.execute(&format!("DELETE FROM {table} WHERE account = ? AND mailbox = ?"), params![account, id])
                        .map_err(db_err)?;
                }
                tx.execute("DELETE FROM mailboxes WHERE account = ? AND id = ?", params![account, id]).map_err(db_err)?;
            }
            let mut st = tx
                .prepare(
                    "INSERT INTO mailboxes (account, id, name, position, total, unread) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (account, id) DO UPDATE SET name = ?3, position = ?4, total = ?5, unread = ?6",
                )
                .map_err(db_err)?;
            for (i, m) in list.iter().enumerate() {
                st.execute(params![account, m.id, m.name, i as i64, m.total as i64, m.unread as i64]).map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Recomputes total/unread of every synced mailbox from the cached
    /// envelopes (for backends whose server does not track flags).
    pub fn recount(&mut self, account: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE mailboxes SET
                   total = (SELECT count(*) FROM envelopes e WHERE e.account = mailboxes.account AND e.mailbox = mailboxes.id),
                   unread = (SELECT count(*) FROM envelopes e WHERE e.account = mailboxes.account AND e.mailbox = mailboxes.id AND e.flags & 1 = 0)
                 WHERE account = ?1 AND validity IS NOT NULL",
                params![account],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    pub fn sync_state(&self, account: &str, mailbox: &str) -> Result<Option<SyncState>> {
        self.conn
            .query_row(
                "SELECT validity, next FROM mailboxes WHERE account = ? AND id = ? AND validity IS NOT NULL",
                params![account, mailbox],
                |r| Ok(SyncState { validity: r.get::<_, i64>(0)? as u64, next: r.get::<_, i64>(1)? as u64 }),
            )
            .optional()
            .map_err(db_err)
    }

    // ----------------------------------------------------------- envelopes

    pub fn envelopes(&self, account: &str, mailbox: &str) -> Result<Vec<Envelope>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT uid, from_name, from_addr, subject, ts, date, flags, size, has_att, msgid, refs
                 FROM envelopes WHERE account = ? AND mailbox = ?",
            )
            .map_err(db_err)?;
        st.query_map(params![account, mailbox], |r| {
            Ok(Envelope {
                id: MessageId(r.get::<_, i64>(0)? as u64),
                from_name: r.get(1)?,
                from_addr: r.get(2)?,
                subject: r.get(3)?,
                timestamp: r.get(4)?,
                date: r.get(5)?,
                flags: Flags::from_bits(r.get::<_, i64>(6)? as u8),
                size: r.get::<_, i64>(7)? as usize,
                has_attachments: r.get(8)?,
                message_id: r.get(9)?,
                parents: r.get::<_, String>(10)?.split_whitespace().map(str::to_string).collect(),
            })
        })
        .map_err(db_err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(db_err)
    }

    /// The newest unread (and not deleted) messages of a mailbox.
    pub fn recent_unread(&self, account: &str, mailbox: &str, limit: usize) -> Result<Vec<Envelope>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT uid, from_name, from_addr, subject, ts, date, flags, size, has_att, msgid, refs
                 FROM envelopes WHERE account = ? AND mailbox = ? AND (flags & 9) = 0
                 ORDER BY ts DESC LIMIT ?",
            )
            .map_err(db_err)?;
        st.query_map(params![account, mailbox, limit as i64], |r| {
            Ok(Envelope {
                id: MessageId(r.get::<_, i64>(0)? as u64),
                from_name: r.get(1)?,
                from_addr: r.get(2)?,
                subject: r.get(3)?,
                timestamp: r.get(4)?,
                date: r.get(5)?,
                flags: Flags::from_bits(r.get::<_, i64>(6)? as u8),
                size: r.get::<_, i64>(7)? as usize,
                has_attachments: r.get(8)?,
                message_id: r.get(9)?,
                parents: r.get::<_, String>(10)?.split_whitespace().map(str::to_string).collect(),
            })
        })
        .map_err(db_err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(db_err)
    }

    /// Applies a sync result in a single transaction and updates the
    /// mailbox counters.
    pub fn apply_sync(&mut self, account: &str, mailbox: &str, result: &SyncResult) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        {
            let insert = |tx: &rusqlite::Transaction, e: &Envelope| -> Result<()> {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO envelopes
                     (account, mailbox, uid, from_name, from_addr, subject, ts, date, flags, size, has_att, msgid, refs)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .and_then(|mut st| {
                    st.execute(params![
                        account,
                        mailbox,
                        e.id.0 as i64,
                        e.from_name,
                        e.from_addr,
                        e.subject,
                        e.timestamp,
                        e.date,
                        e.flags.bits() as i64,
                        e.size as i64,
                        e.has_attachments,
                        e.message_id,
                        e.parents.join(" ")
                    ])
                })
                .map(|_| ())
                .map_err(db_err)
            };
            let state = match result {
                SyncResult::Full { envelopes, state } => {
                    tx.execute("DELETE FROM envelopes WHERE account = ? AND mailbox = ?", params![account, mailbox])
                        .map_err(db_err)?;
                    // If UIDVALIDITY changed, the stored bodies no longer match.
                    tx.execute("DELETE FROM bodies WHERE account = ? AND mailbox = ?", params![account, mailbox])
                        .map_err(db_err)?;
                    for e in envelopes {
                        insert(&tx, e)?;
                    }
                    *state
                }
                SyncResult::Delta { added, existing, state } => {
                    let known: Vec<i64> = {
                        let mut st = tx
                            .prepare_cached("SELECT uid FROM envelopes WHERE account = ? AND mailbox = ?")
                            .map_err(db_err)?;
                        st.query_map(params![account, mailbox], |r| r.get(0))
                            .map_err(db_err)?
                            .collect::<rusqlite::Result<_>>()
                            .map_err(db_err)?
                    };
                    let present: std::collections::HashSet<i64> = existing
                        .iter()
                        .map(|(id, _)| id.0 as i64)
                        .chain(added.iter().map(|e| e.id.0 as i64))
                        .collect();
                    let removed: Vec<MessageId> =
                        known.into_iter().filter(|u| !present.contains(u)).map(|u| MessageId(u as u64)).collect();
                    for e in added {
                        insert(&tx, e)?;
                    }
                    let mut upd = tx
                        .prepare_cached("UPDATE envelopes SET flags = ? WHERE account = ? AND mailbox = ? AND uid = ?")
                        .map_err(db_err)?;
                    for (id, f) in existing {
                        if let Some(f) = f {
                            upd.execute(params![f.bits() as i64, account, mailbox, id.0 as i64]).map_err(db_err)?;
                        }
                    }
                    for id in &removed {
                        for table in ["envelopes", "bodies"] {
                            tx.execute(
                                &format!("DELETE FROM {table} WHERE account = ? AND mailbox = ? AND uid = ?"),
                                params![account, mailbox, id.0 as i64],
                            )
                            .map_err(db_err)?;
                        }
                    }
                    Some(*state)
                }
            };
            tx.execute(
                "UPDATE mailboxes SET validity = ?3, next = ?4,
                   total = (SELECT count(*) FROM envelopes WHERE account = ?1 AND mailbox = ?2),
                   unread = (SELECT count(*) FROM envelopes WHERE account = ?1 AND mailbox = ?2 AND flags & 1 = 0)
                 WHERE account = ?1 AND id = ?2",
                params![account, mailbox, state.map(|s| s.validity as i64), state.map(|s| s.next as i64)],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    /// Stores part of an interrupted-able first sync. If the cached copy has
    /// another UIDVALIDITY it is dropped first. The mailbox is marked
    /// "incomplete" (next = 0) until the sync finishes and `apply_sync` runs.
    pub fn add_partial(&mut self, account: &str, mailbox: &str, validity: u64, envelopes: &[Envelope]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        {
            let current: Option<i64> = tx
                .query_row("SELECT validity FROM mailboxes WHERE account = ? AND id = ?", params![account, mailbox], |r| r.get(0))
                .optional()
                .map_err(db_err)?
                .flatten();
            if current != Some(validity as i64) {
                for table in ["envelopes", "bodies"] {
                    tx.execute(&format!("DELETE FROM {table} WHERE account = ? AND mailbox = ?"), params![account, mailbox]).map_err(db_err)?;
                }
            }
            tx.execute("UPDATE mailboxes SET validity = ?3, next = 0 WHERE account = ?1 AND id = ?2", params![account, mailbox, validity as i64])
                .map_err(db_err)?;
            let mut st = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO envelopes
                     (account, mailbox, uid, from_name, from_addr, subject, ts, date, flags, size, has_att, msgid, refs)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .map_err(db_err)?;
            for e in envelopes {
                st.execute(params![
                    account, mailbox, e.id.0 as i64, e.from_name, e.from_addr, e.subject, e.timestamp, e.date,
                    e.flags.bits() as i64, e.size as i64, e.has_attachments, e.message_id, e.parents.join(" ")
                ])
                .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Everyone who sent mail: (name, address, messages), most frequent first.
    pub fn senders(&self, limit: usize) -> Result<Vec<(String, String, u64)>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT max(from_name), from_addr, count(*) AS n FROM envelopes
                 WHERE from_addr LIKE '%@%' GROUP BY lower(from_addr)
                 ORDER BY n DESC, max(ts) DESC LIMIT ?",
            )
            .map_err(db_err)?;
        st.query_map(params![limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? as u64)))
            .map_err(db_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db_err)
    }

    /// Forgets every message of a mailbox (it was emptied on the server).
    pub fn clear_mailbox(&mut self, account: &str, mailbox: &str) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        for table in ["envelopes", "bodies"] {
            tx.execute(&format!("DELETE FROM {table} WHERE account = ? AND mailbox = ?"), params![account, mailbox]).map_err(db_err)?;
        }
        tx.execute("UPDATE mailboxes SET total = 0, unread = 0 WHERE account = ? AND id = ?", params![account, mailbox]).map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Forgets messages that were deleted or moved away on the server.
    pub fn remove(&mut self, account: &str, mailbox: &str, ids: &[MessageId]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        for table in ["envelopes", "bodies"] {
            let mut st = tx.prepare(&format!("DELETE FROM {table} WHERE account = ? AND mailbox = ? AND uid = ?")).map_err(db_err)?;
            for id in ids {
                st.execute(params![account, mailbox, id.0 as i64]).map_err(db_err)?;
            }
        }
        tx.execute(
            "UPDATE mailboxes SET
               total = (SELECT count(*) FROM envelopes WHERE account = ?1 AND mailbox = ?2),
               unread = (SELECT count(*) FROM envelopes WHERE account = ?1 AND mailbox = ?2 AND flags & 1 = 0)
             WHERE account = ?1 AND id = ?2",
            params![account, mailbox],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    pub fn set_flag(&mut self, account: &str, mailbox: &str, ids: &[MessageId], flag: Flag, on: bool) -> Result<()> {
        let bit = Flags::from_bits(0).with(flag).bits() as i64;
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        {
            let sql = if on {
                "UPDATE envelopes SET flags = flags | ? WHERE account = ? AND mailbox = ? AND uid = ?"
            } else {
                "UPDATE envelopes SET flags = flags & ~? WHERE account = ? AND mailbox = ? AND uid = ?"
            };
            let mut st = tx.prepare(sql).map_err(db_err)?;
            for id in ids {
                st.execute(params![bit, account, mailbox, id.0 as i64]).map_err(db_err)?;
            }
            tx.execute(
                "UPDATE mailboxes SET
                   unread = (SELECT count(*) FROM envelopes WHERE account = ?1 AND mailbox = ?2 AND flags & 1 = 0)
                 WHERE account = ?1 AND id = ?2",
                params![account, mailbox],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    // -------------------------------------------------------------- bodies

    pub fn body(&self, account: &str, mailbox: &str, id: MessageId) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT raw FROM bodies WHERE account = ? AND mailbox = ? AND uid = ?",
                params![account, mailbox, id.0 as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    pub fn save_body(&self, account: &str, mailbox: &str, id: MessageId, raw: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO bodies (account, mailbox, uid, raw) VALUES (?, ?, ?, ?)",
                params![account, mailbox, id.0 as i64, raw],
            )
            .map(|_| ())
            .map_err(db_err)
    }
}

trait FlagsExt {
    fn with(self, f: Flag) -> Self;
}

impl FlagsExt for Flags {
    fn with(mut self, f: Flag) -> Self {
        self.set(f, true);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(uid: u64, seen: bool) -> Envelope {
        Envelope {
            id: MessageId(uid),
            from_name: "Ana".into(),
            from_addr: "ana@x".into(),
            subject: format!("m{uid}"),
            timestamp: uid as i64,
            date: String::new(),
            flags: Flags { seen, ..Default::default() },
            size: 10,
            has_attachments: false,
            message_id: String::new(),
            parents: Vec::new(),
        }
    }

    fn mb(id: &str) -> MailboxInfo {
        MailboxInfo { id: id.into(), name: id.into(), total: 0, unread: 0 }
    }

    #[test]
    fn full_delta_flags_bodies() {
        let mut s = Store::open_in_memory().unwrap();
        s.save_mailboxes("a", &[mb("INBOX"), mb("Old")]).unwrap();
        assert_eq!(s.sync_state("a", "INBOX").unwrap(), None);

        let st1 = SyncState { validity: 7, next: 4 };
        s.apply_sync("a", "INBOX", &SyncResult::Full { envelopes: vec![env(1, true), env(2, false), env(3, false)], state: Some(st1) })
            .unwrap();
        assert_eq!(s.sync_state("a", "INBOX").unwrap(), Some(st1));
        let m = &s.mailboxes("a").unwrap()[0];
        assert_eq!((m.total, m.unread), (3, 2));

        s.save_body("a", "INBOX", MessageId(2), b"raw2").unwrap();
        let st2 = SyncState { validity: 7, next: 5 };
        s.apply_sync(
            "a",
            "INBOX",
            &SyncResult::Delta {
                added: vec![env(4, false)],
                existing: vec![
                    (MessageId(1), Some(Flags { seen: true, ..Default::default() })),
                    (MessageId(3), Some(Flags { seen: true, flagged: true, ..Default::default() })),
                ],
                state: st2,
            },
        )
        .unwrap();
        let mut envs = s.envelopes("a", "INBOX").unwrap();
        envs.sort_by_key(|e| e.id);
        assert_eq!(envs.iter().map(|e| e.id.0).collect::<Vec<_>>(), [1, 3, 4]);
        assert!(envs[1].flags.flagged);
        assert_eq!(s.body("a", "INBOX", MessageId(2)).unwrap(), None);
        assert_eq!(s.mailboxes("a").unwrap()[0].unread, 1);

        s.set_flag("a", "INBOX", &[MessageId(4)], Flag::Seen, true).unwrap();
        assert_eq!(s.mailboxes("a").unwrap()[0].unread, 0);
        s.set_flag("a", "INBOX", &[MessageId(1)], Flag::Seen, false).unwrap();
        assert_eq!(s.mailboxes("a").unwrap()[0].unread, 1);

        // Mailbox deleted on the server: it disappears along with its data.
        s.save_mailboxes("a", &[mb("INBOX")]).unwrap();
        assert_eq!(s.mailboxes("a").unwrap().len(), 1);
    }
}

#[cfg(test)]
mod schema {
    use super::*;

    #[test]
    fn a_cache_rebuilt_by_an_older_orion_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sqlite");
        drop(Store::open(&path).unwrap());
        // What an older version does: its own tables, with the new number left behind.
        let c = Connection::open(&path).unwrap();
        c.execute_batch(&format!(
            "DROP TABLE envelopes; CREATE TABLE envelopes (account TEXT, mailbox TEXT, uid INTEGER, from_name TEXT, from_addr TEXT, subject TEXT,
             ts INTEGER, date TEXT, flags INTEGER, size INTEGER, has_att INTEGER, PRIMARY KEY (account, mailbox, uid)); PRAGMA user_version = {SCHEMA_VERSION};"
        ))
        .unwrap();
        drop(c);
        let store = Store::open(&path).unwrap();
        assert!(store.envelopes("a", "INBOX").unwrap().is_empty(), "the tables work again");
    }
}

#[cfg(test)]
mod partial {
    use super::*;

    fn env(uid: u64) -> Envelope {
        Envelope {
            id: MessageId(uid),
            from_name: String::new(),
            from_addr: "a@b.c".into(),
            subject: format!("m{uid}"),
            timestamp: 0,
            date: String::new(),
            flags: Flags::default(),
            size: 1,
            has_attachments: false,
            message_id: String::new(),
            parents: Vec::new(),
        }
    }

    #[test]
    fn partial_first_sync_is_kept_and_resumable() {
        let mut s = Store::open_in_memory().unwrap();
        s.save_mailboxes("a", &[MailboxInfo { id: "INBOX".into(), name: "INBOX".into(), total: 0, unread: 0 }]).unwrap();
        s.add_partial("a", "INBOX", 7, &[env(1), env(2)]).unwrap();
        s.add_partial("a", "INBOX", 7, &[env(3)]).unwrap();
        assert_eq!(s.envelopes("a", "INBOX").unwrap().len(), 3, "batches accumulate");
        assert_eq!(s.sync_state("a", "INBOX").unwrap(), Some(SyncState { validity: 7, next: 0 }), "marked incomplete");
        // Another UIDVALIDITY: the old partial copy is useless and dropped.
        s.add_partial("a", "INBOX", 8, &[env(10)]).unwrap();
        assert_eq!(s.envelopes("a", "INBOX").unwrap().iter().map(|e| e.id.0).collect::<Vec<_>>(), [10]);
    }
}

#[cfg(test)]
mod concurrency {
    use super::*;

    /// Several account threads open a brand-new cache at the same time: all
    /// must succeed and nobody may wipe what another one already wrote.
    #[test]
    fn concurrent_open_of_new_cache() {
        for round in 0..5 {
            let dir = std::env::temp_dir().join(format!("orion-store-race-{}-{round}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let path = dir.join("cache.sqlite");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let (path, barrier) = (path.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        let mut s = Store::open(&path).map_err(|e| format!("thread {i}: {e}"))?;
                        let mb = MailboxInfo { id: "INBOX".into(), name: "INBOX".into(), total: 0, unread: 0 };
                        s.save_mailboxes(&format!("acc{i}"), &[mb]).map_err(|e| format!("thread {i}: {e}"))?;
                        Ok::<_, String>(())
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap().unwrap();
            }
            let s = Store::open(&path).unwrap();
            for i in 0..8 {
                assert_eq!(s.mailboxes(&format!("acc{i}")).unwrap().len(), 1, "round {round}: data of acc{i} was lost");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
