//! Address book for suggestions while writing mail.
//!
//! Sources (`[contacts] sources`, in order of preference for names):
//! - `eds`: Evolution Data Server, i.e. GNOME's address books, including the
//!   ones of GNOME Online Accounts (CardDAV, Google…). Read over D-Bus with
//!   the user's session: Orion never sees those accounts' passwords.
//! - `mail`: people who wrote to you, from Orion's cache (ranked by how
//!   often and how recently).
//! - `vcard:<path>`: a `.vcf` file or a folder of them (vdirsyncer, khard,
//!   exported from a phone).
//! - `command:<cmd>`: a program printing `email<TAB>name` lines (the mutt
//!   query format: `khard email --parsable`, `abook --mutt-query ''`…).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub email: String,
    /// Higher is better: address-book entries, then how often they wrote.
    pub score: u32,
}

impl Contact {
    /// `Name <email>`, or just the address when there is no name.
    pub fn display(&self) -> String {
        if self.name.is_empty() || self.name.eq_ignore_ascii_case(&self.email) {
            self.email.clone()
        } else if self.name.chars().any(|c| ",;<>@\"".contains(c)) {
            format!("\"{}\" <{}>", self.name.replace('"', ""), self.email)
        } else {
            format!("{} <{}>", self.name, self.email)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Eds,
    Mail,
    VCard(PathBuf),
    Command(String),
}

impl Source {
    pub fn parse(s: &str) -> Option<Source> {
        let s = s.trim();
        Some(match s.split_once(':') {
            None if s == "eds" || s == "gnome" || s == "evolution" => Source::Eds,
            None if s == "mail" => Source::Mail,
            Some(("vcard", p)) => Source::VCard(expand(p)),
            Some(("command", c)) => Source::Command(c.trim().to_string()),
            _ => return None,
        })
    }
}

fn expand(p: &str) -> PathBuf {
    match p.trim().strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p.trim()),
    }
}

/// Reads every source. Failures are returned as warnings, never fatal.
pub fn load(sources: &[String], cache: Option<&Path>) -> (Vec<Contact>, Vec<String>) {
    let mut all: Vec<Contact> = Vec::new();
    let mut warnings = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        // Earlier sources win on names: give them a bonus.
        let bonus = 1000 * (sources.len() - i) as u32;
        let got = match Source::parse(s) {
            Some(Source::Eds) => eds::contacts(),
            Some(Source::Mail) => from_mail(cache),
            Some(Source::VCard(p)) => from_vcards(&p),
            Some(Source::Command(c)) => from_command(&c),
            None => Err(format!("unknown contacts source \"{s}\" (eds, mail, vcard:<path>, command:<cmd>)")),
        };
        match got {
            Ok(list) => all.extend(list.into_iter().map(|mut c| {
                c.score += bonus;
                c
            })),
            Err(e) => warnings.push(format!("contacts ({s}): {e}")),
        }
    }
    (merge(all), warnings)
}

/// One entry per address (case-insensitive): the best-scored name wins,
/// scores add up.
fn merge(list: Vec<Contact>) -> Vec<Contact> {
    let mut by_email: HashMap<String, Contact> = HashMap::new();
    for c in list {
        let email = c.email.trim().to_string();
        if !email.contains('@') || email.contains(char::is_whitespace) {
            continue;
        }
        match by_email.entry(email.to_ascii_lowercase()) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let cur = e.get_mut();
                if cur.name.is_empty() || (c.score > cur.score && !c.name.is_empty()) {
                    cur.name = c.name.clone();
                }
                cur.score = cur.score.saturating_add(c.score);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Contact { email, ..c });
            }
        }
    }
    let mut v: Vec<Contact> = by_email.into_values().collect();
    v.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.email.cmp(&b.email)));
    v
}

/// Contacts matching `query` (words in the name, or the address), best first.
pub fn search<'a>(contacts: &'a [Contact], query: &str, limit: usize) -> Vec<&'a Contact> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u8, &Contact)> = contacts
        .iter()
        .filter_map(|c| {
            let (name, email) = (fold(&c.name), c.email.to_lowercase());
            let q = fold(&q);
            let rank = if name.split_whitespace().any(|w| w.starts_with(&q)) || email.starts_with(&q) {
                0
            } else if name.contains(&q) || email.contains(&q) {
                1
            } else {
                return None;
            };
            Some((rank, c))
        })
        .collect();
    hits.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then(b.score.cmp(&a.score)));
    hits.into_iter().take(limit).map(|(_, c)| c).collect()
}

/// Lower case without accents, so "garcia" finds "García".
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

// ------------------------------------------------------------------ mail

/// Automated senders are not people to write to.
fn automated(addr: &str) -> bool {
    let local = addr.split('@').next().unwrap_or("").to_lowercase();
    ["noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "mailer-daemon", "postmaster", "notifications", "notification", "bounce"]
        .iter()
        .any(|p| local.contains(p))
}

fn from_mail(cache: Option<&Path>) -> Result<Vec<Contact>, String> {
    let Some(path) = cache.filter(|p| p.exists()) else { return Ok(Vec::new()) };
    let store = orion_store::Store::open(path).map_err(|e| e.to_string())?;
    let senders = store.senders(5000).map_err(|e| e.to_string())?;
    Ok(senders
        .into_iter()
        .filter(|(_, addr, _)| !automated(addr))
        .map(|(name, email, count)| Contact { name, email, score: count.min(999) as u32 })
        .collect())
}

// ----------------------------------------------------------------- vCard

fn from_vcards(path: &Path) -> Result<Vec<Contact>, String> {
    let files: Vec<PathBuf> = if path.is_dir() {
        std::fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vcf")))
            .collect()
    } else {
        vec![path.to_path_buf()]
    };
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        out.extend(parse_vcards(&text));
    }
    Ok(out)
}

/// Name and e-mail addresses of each card (RFC 6350, folded lines).
pub fn parse_vcards(text: &str) -> Vec<Contact> {
    let mut lines: Vec<String> = Vec::new();
    for l in text.lines() {
        match l.strip_prefix([' ', '\t']) {
            Some(cont) if !lines.is_empty() => lines.last_mut().expect("non-empty").push_str(cont),
            _ => lines.push(l.trim_end_matches('\r').to_string()),
        }
    }
    let mut out = Vec::new();
    let (mut name, mut emails): (String, Vec<String>) = (String::new(), Vec::new());
    for l in lines {
        let Some((key, value)) = l.split_once(':') else { continue };
        // "item1.EMAIL;TYPE=INTERNET" → "EMAIL"
        let prop = key.split(';').next().unwrap_or("").rsplit('.').next().unwrap_or("").to_ascii_uppercase();
        match prop.as_str() {
            "BEGIN" => (name, emails) = (String::new(), Vec::new()),
            "FN" => name = unescape(value),
            "N" if name.is_empty() => {
                let parts: Vec<&str> = value.split(';').collect();
                name = format!("{} {}", parts.get(1).unwrap_or(&""), parts.first().unwrap_or(&"")).trim().to_string();
            }
            "EMAIL" => emails.push(value.trim().to_string()),
            "END" => {
                for e in emails.drain(..) {
                    out.push(Contact { name: name.clone(), email: e, score: 1 });
                }
            }
            _ => {}
        }
    }
    out
}

fn unescape(v: &str) -> String {
    v.replace("\\,", ",").replace("\\;", ";").replace("\\n", " ").replace("\\\\", "\\").trim().to_string()
}

// --------------------------------------------------------------- command

fn from_command(cmd: &str) -> Result<Vec<Contact>, String> {
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("failed").to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let email = f.next()?.trim();
            email.contains('@').then(|| Contact { email: email.to_string(), name: f.next().unwrap_or("").trim().to_string(), score: 1 })
        })
        .collect())
}

// ------------------------------------------------------------------- EDS

mod eds {
    use super::{parse_vcards, Contact};
    use zbus::blocking::{fdo::ObjectManagerProxy, Connection, Proxy};
    use zbus::zvariant::OwnedValue;

    const SOURCES: &str = "org.gnome.evolution.dataserver.Sources5";
    const BOOKS: &str = "org.gnome.evolution.dataserver.AddressBook10";

    /// Every enabled address book of the session's Evolution Data Server.
    pub fn contacts() -> Result<Vec<Contact>, String> {
        let conn = Connection::session().map_err(|e| format!("no D-Bus session: {e}"))?;
        let om = ObjectManagerProxy::builder(&conn)
            .destination(SOURCES)
            .and_then(|b| b.path("/org/gnome/evolution/dataserver/SourceManager"))
            .and_then(|b| b.build())
            .map_err(|e| e.to_string())?;
        let objects = om.get_managed_objects().map_err(|e| format!("Evolution Data Server is not available: {e}"))?;
        let mut out = Vec::new();
        for ifaces in objects.values() {
            let Some(src) = ifaces.get("org.gnome.evolution.dataserver.Source") else { continue };
            let get = |k: &str| src.get(k).and_then(|v: &OwnedValue| String::try_from(v.clone()).ok()).unwrap_or_default();
            let (uid, data) = (get("UID"), get("Data"));
            if !data.contains("[Address Book]") || data.lines().any(|l| l.trim() == "Enabled=false") {
                continue;
            }
            // One unreachable book (offline account) must not hide the others.
            if let Ok(list) = book_contacts(&conn, &uid) {
                out.extend(list);
            }
        }
        Ok(out)
    }

    fn book_contacts(conn: &Connection, uid: &str) -> zbus::Result<Vec<Contact>> {
        let factory = Proxy::new(conn, BOOKS, "/org/gnome/evolution/dataserver/AddressBookFactory", "org.gnome.evolution.dataserver.AddressBookFactory")?;
        let (path, bus): (String, String) = factory.call("OpenAddressBook", &(uid,))?;
        let book = Proxy::new(conn, bus.as_str(), path.as_str(), "org.gnome.evolution.dataserver.AddressBook")?;
        let _: Vec<String> = book.call("Open", &())?;
        // Ask remote books (CardDAV…) to sync; the first time, wait a little
        // for them to arrive. Local books do not support it: ignored.
        let refreshed: zbus::Result<()> = book.call("Refresh", &());
        let mut cards: Vec<String> = book.call("GetContactList", &("(exists \"email\")",))?;
        if refreshed.is_ok() && cards.is_empty() {
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                cards = book.call("GetContactList", &("(exists \"email\")",))?;
                if !cards.is_empty() {
                    break;
                }
            }
        }
        let _: zbus::Result<()> = book.call("Close", &());
        Ok(cards.iter().flat_map(|c| parse_vcards(c)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcards() {
        let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ana Garc\r\n ía\r\nitem1.EMAIL;TYPE=INTERNET:ana@example.org\r\nEMAIL:ana.work@example.com\r\nEND:VCARD\r\n\
                    BEGIN:VCARD\r\nN:Ruiz;Dana;;;\r\nEMAIL;TYPE=HOME:dana@example.net\r\nEND:VCARD\r\nBEGIN:VCARD\r\nFN:No mail\r\nEND:VCARD\r\n";
        let c = parse_vcards(text);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].name.as_str(), c[0].email.as_str()), ("Ana García", "ana@example.org"));
        assert_eq!(c[1].email, "ana.work@example.com");
        assert_eq!(c[2].name, "Dana Ruiz");
    }

    #[test]
    fn merging_and_search() {
        let c = |n: &str, e: &str, s| Contact { name: n.into(), email: e.into(), score: s };
        let merged = merge(vec![
            c("", "ANA@example.org", 5),
            c("Ana García", "ana@example.org", 2000),
            c("Luis Martín", "luis@example.net", 3),
            c("Lucía Pérez", "lucia@example.com", 40),
            c("bad", "not an address", 1),
        ]);
        assert_eq!(merged.len(), 3, "same address merged, invalid dropped");
        assert_eq!(merged[0].name, "Ana García");
        assert_eq!(merged[0].score, 2005);
        let names = |q| search(&merged, q, 10).iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("lu"), ["Lucía Pérez", "Luis Martín"], "by score");
        assert_eq!(names("garcia"), ["Ana García"], "accents ignored");
        assert_eq!(names("example.net"), ["Luis Martín"]);
        assert!(names("").is_empty());
        assert_eq!(merged[0].display(), "Ana García <ANA@example.org>");
        assert_eq!(c("Ruiz, Dana", "d@x.org", 0).display(), "\"Ruiz, Dana\" <d@x.org>");
    }

    #[test]
    fn sources_and_command() {
        assert_eq!(Source::parse("eds"), Some(Source::Eds));
        assert_eq!(Source::parse("command:khard email --parsable"), Some(Source::Command("khard email --parsable".into())));
        assert!(matches!(Source::parse("vcard:~/Contacts"), Some(Source::VCard(_))));
        assert_eq!(Source::parse("bogus"), None);
        let got = from_command("printf 'Searching...\\nana@example.org\\tAna\\tother\\n'").unwrap();
        assert_eq!(got, [Contact { name: "Ana".into(), email: "ana@example.org".into(), score: 1 }]);
        assert!(automated("noreply@github.com") && !automated("ana@example.org"));
    }
}
