//! JMAP backend (RFC 8620 core, RFC 8621 mail) over HTTPS with `ureq`.
//!
//! Message ids are a stable hash of the JMAP Email id; the mapping back is
//! rebuilt from `Email/query` whenever it is missing. Each sync lists the
//! mailbox ids (cheap), fetches full headers only for unknown messages and
//! just `keywords` for known ones.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{json, Value};

use orion_core::config::{JmapAuth, JmapConfig};
use orion_core::model::{Envelope, Flag, Flags, MailboxInfo, MessageId, SyncEvent, SyncResult, SyncState};
use orion_core::{Backend, Error, Result};

use crate::stable_id;

const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"];
const MAX_MESSAGE: u64 = 200 * 1024 * 1024;

struct Session {
    api_url: String,
    download_url: String,
    upload_url: String,
    account_id: String,
    max_get: usize,
}

pub struct JmapBackend {
    cfg: JmapConfig,
    agent: ureq::Agent,
    auth: Option<String>,
    session: Option<Session>,
    /// Hash → JMAP Email id.
    ids: HashMap<MessageId, String>,
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

fn http_err(e: ureq::Error) -> Error {
    match e {
        ureq::Error::StatusCode(401) | ureq::Error::StatusCode(403) => Error::Auth("the server rejected the credentials".into()),
        ureq::Error::StatusCode(code) => Error::Protocol(format!("HTTP {code}")),
        other => net(other),
    }
}

impl JmapBackend {
    pub fn new(cfg: JmapConfig) -> Self {
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .tls_config(tls)
            .build()
            .into();
        JmapBackend { cfg, agent, auth: None, session: None, ids: HashMap::new() }
    }

    fn auth_header(&mut self) -> Result<String> {
        if let Some(a) = &self.auth {
            return Ok(a.clone());
        }
        let secret = self.cfg.resolve_password()?;
        let header = match self.cfg.auth {
            JmapAuth::Bearer => format!("Bearer {secret}"),
            JmapAuth::Basic => {
                let user = self.cfg.user.clone().ok_or_else(|| Error::Auth("JMAP basic auth needs `user`".into()))?;
                format!("Basic {}", base64(format!("{user}:{secret}").as_bytes()))
            }
        };
        self.auth = Some(header.clone());
        Ok(header)
    }

    fn session(&mut self) -> Result<&Session> {
        if self.session.is_none() {
            let auth = self.auth_header()?;
            let mut url = session_url(&self.cfg.url);
            // Follow redirects by hand: credentials only go to the same origin
            // (scheme + host + port), and never from https down to http.
            let origin = origin_of(&url);
            let mut hops = 0;
            let mut res = loop {
                let res = self
                    .agent
                    .get(&url)
                    .config()
                    .max_redirects(0)
                    .http_status_as_error(false)
                    .build()
                    .header("Authorization", &auth)
                    .header("Accept", "application/json")
                    .call()
                    .map_err(http_err)?;
                let status = res.status().as_u16();
                if (300..400).contains(&status) {
                    let location = res.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default();
                    let next = absolute(&url, location);
                    hops += 1;
                    if hops > 5 || origin_of(&next) != origin {
                        return Err(Error::Protocol(format!("refusing to follow the JMAP session redirect to {next}")));
                    }
                    url = next;
                    continue;
                }
                if status == 401 || status == 403 {
                    self.auth = None;
                    return Err(Error::Auth("the server rejected the credentials".into()));
                }
                if status >= 400 {
                    return Err(Error::Protocol(format!("JMAP session: HTTP {status}")));
                }
                break res;
            };
            let v: Value = res.body_mut().read_json().map_err(|e| Error::Protocol(format!("invalid JMAP session: {e}")))?;
            let account_id = v["primaryAccounts"]["urn:ietf:params:jmap:mail"]
                .as_str()
                .ok_or_else(|| Error::Protocol("the server has no JMAP mail account".into()))?
                .to_string();
            let api_url = absolute(&url, v["apiUrl"].as_str().ok_or_else(|| Error::Protocol("session without apiUrl".into()))?);
            let download_url = absolute(&url, v["downloadUrl"].as_str().ok_or_else(|| Error::Protocol("session without downloadUrl".into()))?);
            let upload_url = absolute(&url, v["uploadUrl"].as_str().unwrap_or_default());
            let max_get = v["capabilities"]["urn:ietf:params:jmap:core"]["maxObjectsInGet"].as_u64().unwrap_or(500).clamp(1, 500) as usize;
            self.session = Some(Session { api_url, download_url, upload_url, account_id, max_get });
        }
        Ok(self.session.as_ref().expect("session"))
    }

    /// One JMAP request with several method calls; returns the arguments of each response.
    fn call(&mut self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>> {
        self.call_using(&USING, calls)
    }

    fn call_using(&mut self, using: &[&str], calls: Vec<(&str, Value)>) -> Result<Vec<Value>> {
        let auth = self.auth_header()?;
        let api = self.session()?.api_url.clone();
        let method_calls: Vec<Value> = calls.iter().enumerate().map(|(i, (name, args))| json!([name, args, format!("c{i}")])).collect();
        let body = json!({ "using": using, "methodCalls": method_calls });
        let res = self.agent.post(&api).header("Authorization", &auth).send_json(&body);
        let mut res = match res {
            Err(ureq::Error::StatusCode(401)) => {
                // Token or session expired: rediscover next time.
                self.auth = None;
                self.session = None;
                return Err(Error::Auth("the server rejected the credentials".into()));
            }
            r => r.map_err(http_err)?,
        };
        let v: Value = res.body_mut().read_json().map_err(|e| Error::Protocol(format!("invalid JMAP response: {e}")))?;
        let responses = v["methodResponses"].as_array().ok_or_else(|| Error::Protocol("response without methodResponses".into()))?;
        let mut out = Vec::with_capacity(calls.len());
        for (i, r) in responses.iter().enumerate() {
            let name = r[0].as_str().unwrap_or_default();
            if name == "error" {
                let kind = r[1]["type"].as_str().unwrap_or("unknown");
                let desc = r[1]["description"].as_str().unwrap_or_default();
                return Err(Error::Protocol(format!("JMAP {} failed: {kind} {desc}", calls.get(i).map(|c| c.0).unwrap_or("?"))));
            }
            out.push(r[1].clone());
        }
        Ok(out)
    }

    fn account(&mut self) -> Result<String> {
        Ok(self.session()?.account_id.clone())
    }

    /// All Email ids of a mailbox (paginated `Email/query`).
    fn query_ids(&mut self, mailbox: &str) -> Result<Vec<String>> {
        let account = self.account()?;
        let mut ids = Vec::new();
        loop {
            let r = self.call(vec![(
                "Email/query",
                json!({
                    "accountId": account,
                    "filter": { "inMailbox": mailbox },
                    "sort": [{ "property": "receivedAt", "isAscending": false }],
                    "position": ids.len(),
                    "limit": 1000,
                    "calculateTotal": true,
                }),
            )])?;
            let page: Vec<String> = r[0]["ids"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let total = r[0]["total"].as_u64().map(|t| t as usize);
            let done = page.is_empty() || total.is_some_and(|t| ids.len() + page.len() >= t);
            ids.extend(page);
            if done {
                break;
            }
        }
        for id in &ids {
            self.ids.insert(stable_id(id), id.clone());
        }
        Ok(ids)
    }

    fn get(&mut self, ids: &[String], properties: &[&str]) -> Result<Vec<Value>> {
        let account = self.account()?;
        let max = self.session()?.max_get;
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(max) {
            let r = self.call(vec![("Email/get", json!({ "accountId": account, "ids": chunk, "properties": properties }))])?;
            out.extend(r[0]["list"].as_array().cloned().unwrap_or_default());
        }
        Ok(out)
    }

    fn jmap_id(&mut self, mailbox: &str, id: MessageId) -> Result<String> {
        if !self.ids.contains_key(&id) {
            self.query_ids(mailbox)?;
        }
        self.ids.get(&id).cloned().ok_or(Error::MessageNotFound(id.0))
    }
}

impl Backend for JmapBackend {
    fn mailboxes(&mut self) -> Result<Vec<MailboxInfo>> {
        let account = self.account()?;
        let r = self.call(vec![(
            "Mailbox/get",
            json!({ "accountId": account, "ids": null, "properties": ["id", "name", "parentId", "role", "totalEmails", "unreadEmails", "sortOrder"] }),
        )])?;
        let list = r[0]["list"].as_array().cloned().unwrap_or_default();
        let by_id: HashMap<&str, &Value> = list.iter().filter_map(|m| Some((m["id"].as_str()?, m))).collect();
        let path = |m: &Value| {
            let mut parts = vec![m["name"].as_str().unwrap_or_default().to_string()];
            let mut parent = m["parentId"].as_str();
            let mut guard = 0;
            while let (Some(p), true) = (parent, guard < 32) {
                let Some(pm) = by_id.get(p) else { break };
                parts.push(pm["name"].as_str().unwrap_or_default().to_string());
                parent = pm["parentId"].as_str();
                guard += 1;
            }
            parts.reverse();
            parts.join("/")
        };
        let rank = |m: &Value| match m["role"].as_str() {
            Some("inbox") => 0,
            Some("drafts") => 1,
            Some("sent") => 2,
            Some("archive") => 3,
            Some("junk") => 4,
            Some("trash") => 5,
            _ => 9,
        };
        let mut out: Vec<(u8, MailboxInfo)> = list
            .iter()
            .filter_map(|m| {
                Some((
                    rank(m),
                    MailboxInfo {
                        id: m["id"].as_str()?.to_string(),
                        name: path(m),
                        total: m["totalEmails"].as_u64().unwrap_or(0) as usize,
                        unread: m["unreadEmails"].as_u64().unwrap_or(0) as usize,
                    },
                ))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
        Ok(out.into_iter().map(|(_, m)| m).collect())
    }

    fn envelopes(&mut self, mailbox: &str) -> Result<Vec<Envelope>> {
        match self.sync(mailbox, None, &HashSet::new(), &mut |_| {})? {
            SyncResult::Full { envelopes, .. } => Ok(envelopes),
            SyncResult::Delta { added, .. } => Ok(added),
        }
    }

    fn sync(
        &mut self,
        mailbox: &str,
        _known: Option<&SyncState>,
        known_ids: &HashSet<MessageId>,
        progress: &mut dyn FnMut(SyncEvent<'_>),
    ) -> Result<SyncResult> {
        let all = self.query_ids(mailbox)?;
        let (old, new): (Vec<String>, Vec<String>) = all.into_iter().partition(|id| known_ids.contains(&stable_id(id)));
        let total = new.len();
        progress(SyncEvent::Progress { done: 0, total });
        let mut added = Vec::with_capacity(total);
        let max = self.session()?.max_get;
        for chunk in new.chunks(max) {
            let list = self.get(chunk, &["id", "from", "subject", "receivedAt", "sentAt", "keywords", "size", "hasAttachment", "messageId", "inReplyTo", "references"])?;
            added.extend(list.iter().filter_map(to_envelope));
            progress(SyncEvent::Progress { done: added.len(), total });
        }
        let existing = self
            .get(&old, &["id", "keywords"])?
            .iter()
            .filter_map(|m| Some((stable_id(m["id"].as_str()?), Some(keywords(&m["keywords"])))))
            .collect();
        Ok(SyncResult::Delta { added, existing, state: SyncState { validity: 1, next: 1 } })
    }

    fn fetch_raw(&mut self, mailbox: &str, id: MessageId) -> Result<Vec<u8>> {
        let jid = self.jmap_id(mailbox, id)?;
        let list = self.get(std::slice::from_ref(&jid), &["blobId"])?;
        let blob = list.first().and_then(|m| m["blobId"].as_str()).ok_or(Error::MessageNotFound(id.0))?.to_string();
        let auth = self.auth_header()?;
        let s = self.session()?;
        let url = s
            .download_url
            .replace("{accountId}", &pct(&s.account_id))
            .replace("{blobId}", &pct(&blob))
            .replace("{type}", &pct("message/rfc822"))
            .replace("{name}", "message.eml");
        let mut res = self.agent.get(&url).header("Authorization", &auth).call().map_err(http_err)?;
        res.body_mut().with_config().limit(MAX_MESSAGE).read_to_vec().map_err(net)
    }

    fn find_sent_mailbox(&mut self) -> Result<Option<String>> {
        let account = self.account()?;
        let r = self.call(vec![("Mailbox/get", json!({ "accountId": account, "ids": null, "properties": ["id", "role"] }))])?;
        Ok(r[0]["list"].as_array().and_then(|l| l.iter().find(|m| m["role"] == "sent")).and_then(|m| m["id"].as_str()).map(str::to_string))
    }

    fn trash_mailbox(&mut self) -> Result<Option<String>> {
        let account = self.account()?;
        let r = self.call(vec![("Mailbox/get", json!({ "accountId": account, "ids": null, "properties": ["id", "role"] }))])?;
        Ok(r[0]["list"].as_array().and_then(|l| l.iter().find(|m| m["role"] == "trash")).and_then(|m| m["id"].as_str()).map(str::to_string))
    }

    /// An Email can be in several mailboxes: leave `mailbox`, join `dest`.
    fn move_messages(&mut self, mailbox: &str, ids: &[MessageId], dest: &str) -> Result<()> {
        let account = self.account()?;
        let mut update = serde_json::Map::new();
        for id in ids {
            let jid = self.jmap_id(mailbox, *id)?;
            update.insert(jid, json!({ format!("mailboxIds/{mailbox}"): Value::Null, format!("mailboxIds/{dest}"): true }));
        }
        let r = self.call(vec![("Email/set", json!({ "accountId": account, "update": update }))])?;
        if let Some(failed) = r[0]["notUpdated"].as_object().filter(|m| !m.is_empty()) {
            return Err(Error::Protocol(format!("JMAP did not move {} message(s): {}", failed.len(), Value::Object(failed.clone()))));
        }
        Ok(())
    }

    fn empty_mailbox(&mut self, mailbox: &str) -> Result<()> {
        let account = self.account()?;
        let all = self.query_ids(mailbox)?;
        for chunk in all.chunks(500) {
            let r = self.call(vec![("Email/set", json!({ "accountId": account, "destroy": chunk }))])?;
            if let Some(failed) = r[0]["notDestroyed"].as_object().filter(|m| !m.is_empty()) {
                return Err(Error::Protocol(format!("JMAP did not delete {} message(s)", failed.len())));
            }
        }
        Ok(())
    }

    fn delete_messages(&mut self, mailbox: &str, ids: &[MessageId]) -> Result<()> {
        let account = self.account()?;
        let jids = ids.iter().map(|id| self.jmap_id(mailbox, *id)).collect::<Result<Vec<_>>>()?;
        let r = self.call(vec![("Email/set", json!({ "accountId": account, "destroy": jids }))])?;
        if let Some(failed) = r[0]["notDestroyed"].as_object().filter(|m| !m.is_empty()) {
            return Err(Error::Protocol(format!("JMAP did not delete {} message(s): {}", failed.len(), Value::Object(failed.clone()))));
        }
        Ok(())
    }

    fn set_flag(&mut self, mailbox: &str, ids: &[MessageId], flag: Flag, on: bool) -> Result<()> {
        let keyword = match flag {
            Flag::Seen => "$seen",
            Flag::Answered => "$answered",
            Flag::Flagged => "$flagged",
            Flag::Draft => "$draft",
            Flag::Deleted => return Err(Error::Unsupported("JMAP has no \\Deleted flag")),
        };
        let account = self.account()?;
        let mut update = serde_json::Map::new();
        for id in ids {
            let jid = self.jmap_id(mailbox, *id)?;
            update.insert(jid, json!({ format!("keywords/{keyword}"): if on { json!(true) } else { Value::Null } }));
        }
        let r = self.call(vec![("Email/set", json!({ "accountId": account, "update": update }))])?;
        if let Some(failed) = r[0]["notUpdated"].as_object().filter(|m| !m.is_empty()) {
            return Err(Error::Protocol(format!("JMAP did not update {} message(s): {}", failed.len(), Value::Object(failed.clone()))));
        }
        Ok(())
    }

    /// RFC 8621 §7: upload, import into Drafts, submit; on success the server
    /// moves it to Sent and clears `$draft`.
    fn submit(&mut self, raw: &[u8], from: &str, rcpts: &[String]) -> Result<bool> {
        let auth = self.auth_header()?;
        let s = self.session()?;
        let (account, upload) = (s.account_id.clone(), s.upload_url.replace("{accountId}", &pct(&s.account_id)));
        if upload.is_empty() {
            return Err(Error::Protocol("the JMAP session has no uploadUrl".into()));
        }
        let mut res = self
            .agent
            .post(&upload)
            .header("Authorization", &auth)
            .header("Content-Type", "message/rfc822")
            .send(raw)
            .map_err(http_err)?;
        let up: Value = res.body_mut().read_json().map_err(|e| Error::Protocol(format!("invalid upload response: {e}")))?;
        let blob = up["blobId"].as_str().ok_or_else(|| Error::Protocol("upload response without blobId".into()))?.to_string();

        let r = self.call_using(
            &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"],
            vec![
                ("Identity/get", json!({ "accountId": account, "ids": null })),
                ("Mailbox/get", json!({ "accountId": account, "ids": null, "properties": ["id", "role"] })),
            ],
        )?;
        let identity = r[0]["list"]
            .as_array()
            .and_then(|l| l.iter().find(|i| i["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(from))).or_else(|| l.first()))
            .and_then(|i| i["id"].as_str())
            .ok_or_else(|| Error::Protocol(format!("the JMAP account has no identity for {from}")))?
            .to_string();
        let role = |name: &str| {
            r[1]["list"].as_array().and_then(|l| l.iter().find(|m| m["role"] == name)).and_then(|m| m["id"].as_str()).map(str::to_string)
        };
        let drafts = role("drafts").ok_or_else(|| Error::Protocol("the JMAP account has no Drafts mailbox".into()))?;
        let mut on_success = serde_json::Map::new();
        on_success.insert(format!("mailboxIds/{drafts}"), Value::Null);
        on_success.insert("keywords/$draft".into(), Value::Null);
        if let Some(sent) = role("sent") {
            on_success.insert(format!("mailboxIds/{sent}"), json!(true));
        }
        let r = self.call_using(
            &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"],
            vec![
                (
                    "Email/import",
                    json!({ "accountId": account, "emails": { "out": { "blobId": blob, "mailboxIds": { drafts.clone(): true }, "keywords": { "$draft": true, "$seen": true } } } }),
                ),
                (
                    "EmailSubmission/set",
                    json!({
                        "accountId": account,
                        "create": { "sub": {
                            "identityId": identity,
                            "emailId": "#out",
                            "envelope": { "mailFrom": { "email": from }, "rcptTo": rcpts.iter().map(|r| json!({ "email": r })).collect::<Vec<_>>() },
                        } },
                        "onSuccessUpdateEmail": { "#sub": Value::Object(on_success) },
                    }),
                ),
            ],
        )?;
        if let Some(err) = r[0]["notCreated"]["out"].as_object() {
            return Err(Error::Protocol(format!("JMAP refused the message: {}", Value::Object(err.clone()))));
        }
        if let Some(err) = r[1]["notCreated"]["sub"].as_object() {
            return Err(Error::Protocol(format!("JMAP refused to send: {} {}", err.get("type").and_then(Value::as_str).unwrap_or(""), err.get("description").and_then(Value::as_str).unwrap_or(""))));
        }
        Ok(true)
    }

    fn cacheable(&self) -> bool {
        true
    }
}

fn keywords(k: &Value) -> Flags {
    let has = |name: &str| k.get(name).and_then(Value::as_bool).unwrap_or(false);
    Flags { seen: has("$seen"), answered: has("$answered"), flagged: has("$flagged"), deleted: false, draft: has("$draft") }
}

fn to_envelope(m: &Value) -> Option<Envelope> {
    let id = m["id"].as_str()?;
    let from = m["from"].as_array().and_then(|a| a.first());
    let date = m["sentAt"].as_str().or(m["receivedAt"].as_str()).unwrap_or_default();
    let (timestamp, date) = match mail_parser::DateTime::parse_rfc3339(date) {
        Some(d) => (
            m["receivedAt"].as_str().and_then(mail_parser::DateTime::parse_rfc3339).unwrap_or(d).to_timestamp(),
            format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute),
        ),
        None => (0, String::new()),
    };
    Some(Envelope {
        id: stable_id(id),
        from_name: from.and_then(|f| f["name"].as_str()).unwrap_or_default().to_string(),
        from_addr: from.and_then(|f| f["email"].as_str()).unwrap_or_default().to_string(),
        subject: m["subject"].as_str().filter(|s| !s.is_empty()).unwrap_or("(no subject)").to_string(),
        timestamp,
        date,
        flags: keywords(&m["keywords"]),
        size: m["size"].as_u64().unwrap_or(0) as usize,
        has_attachments: m["hasAttachment"].as_bool().unwrap_or(false),
        message_id: ids(&m["messageId"]).into_iter().next().unwrap_or_default(),
        parents: {
            let mut p = ids(&m["references"]);
            for id in ids(&m["inReplyTo"]) {
                if !p.contains(&id) {
                    p.push(id);
                }
            }
            p
        },
    })
}

/// JMAP gives message ids as arrays of strings (without angle brackets).
fn ids(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// `https://host` → `https://host/.well-known/jmap`; a full session URL is kept.
fn session_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let path_start = trimmed.find("://").map(|i| i + 3).and_then(|i| trimmed[i..].find('/').map(|j| i + j));
    match path_start {
        None => format!("{trimmed}/.well-known/jmap"),
        Some(_) => trimmed.to_string(),
    }
}

/// Resolves `apiUrl`/`downloadUrl`, which may be relative to the session URL.
fn absolute(base: &str, url: &str) -> String {
    if url.contains("://") {
        return url.to_string();
    }
    let origin_end = base.find("://").map(|i| i + 3).and_then(|i| base[i..].find('/').map(|j| i + j)).unwrap_or(base.len());
    format!("{}/{}", &base[..origin_end], url.trim_start_matches('/'))
}

/// `scheme://host:port` of a URL, lowercased.
fn origin_of(url: &str) -> String {
    let after = url.find("://").map(|i| i + 3).unwrap_or(0);
    let end = url[after..].find('/').map(|j| after + j).unwrap_or(url.len());
    url[..end].to_ascii_lowercase()
}

/// Percent-encoding for URI template values (RFC 6570 simple expansion).
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(base64(b"ana:s3cret"), "YW5hOnMzY3JldA==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(session_url("https://jmap.example.com"), "https://jmap.example.com/.well-known/jmap");
        assert_eq!(session_url("https://api.fastmail.com/jmap/session"), "https://api.fastmail.com/jmap/session");
        assert_eq!(absolute("http://127.0.0.1:8080/.well-known/jmap", "/api/"), "http://127.0.0.1:8080/api/");
        assert_eq!(pct("a b/ç"), "a%20b%2F%C3%A7");
        assert_eq!(origin_of("https://Mail.Example.com:443/jmap/x"), "https://mail.example.com:443");
        assert_ne!(origin_of("https://a.com/x"), origin_of("http://a.com/x"));
        let f = keywords(&json!({"$seen": true, "$flagged": true}));
        assert!(f.seen && f.flagged && !f.answered);
    }
}
