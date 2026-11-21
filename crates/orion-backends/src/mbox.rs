//! mbox backend (mboxrd variant): a single file or a directory with several.
//!
//! The file is memory-mapped and the `From ` separators are located with
//! `memchr`; headers are parsed in parallel. If the file changes on disk
//! (size or mtime) it is re-indexed.

use std::collections::HashMap;
use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use memchr::memmem;
use memmap2::Mmap;
use rayon::prelude::*;

use orion_core::model::{Envelope, MailboxInfo, MessageId};
use orion_core::parse::envelope_from_headers;
use orion_core::{Backend, Error, Result};

pub struct MboxBackend {
    root: PathBuf,
    cache: HashMap<String, Indexed>,
}

struct Indexed {
    map: Option<Mmap>,
    len: u64,
    mtime: Option<SystemTime>,
    ranges: Vec<Range<usize>>,
    envelopes: Vec<Envelope>,
}

impl Indexed {
    fn bytes(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }
}

impl MboxBackend {
    pub fn new(root: PathBuf) -> Self {
        Self { root, cache: HashMap::new() }
    }

    fn mailbox_paths(&self) -> Result<Vec<(String, String)>> {
        let meta = std::fs::metadata(&self.root).map_err(|e| Error::io(&self.root, e))?;
        if meta.is_file() {
            let name = self
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "mbox".into());
            return Ok(vec![(self.root.to_string_lossy().into_owned(), name)]);
        }
        let mut out = Vec::new();
        collect_mboxes(&self.root, &self.root, 0, &mut out)?;
        out.sort_by(|a, b| mailbox_rank(&a.1).cmp(&mailbox_rank(&b.1)).then(a.1.cmp(&b.1)));
        Ok(out)
    }

    fn indexed(&mut self, mailbox: &str) -> Result<&Indexed> {
        let path = Path::new(mailbox);
        let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
        let (len, mtime) = (meta.len(), meta.modified().ok());
        let fresh = self
            .cache
            .get(mailbox)
            .is_some_and(|ix| ix.len == len && ix.mtime == mtime);
        if !fresh {
            let ix = index_file(path, len, mtime)?;
            self.cache.insert(mailbox.to_string(), ix);
        }
        Ok(&self.cache[mailbox])
    }
}

impl Backend for MboxBackend {
    fn mailboxes(&mut self) -> Result<Vec<MailboxInfo>> {
        let mut out = Vec::new();
        for (id, name) in self.mailbox_paths()? {
            let ix = self.indexed(&id)?;
            out.push(MailboxInfo {
                total: ix.envelopes.len(),
                unread: ix.envelopes.iter().filter(|e| !e.flags.seen).count(),
                id,
                name,
            });
        }
        Ok(out)
    }

    fn envelopes(&mut self, mailbox: &str) -> Result<Vec<Envelope>> {
        Ok(self.indexed(mailbox)?.envelopes.clone())
    }

    fn fetch_raw(&mut self, mailbox: &str, id: MessageId) -> Result<Vec<u8>> {
        let ix = self.indexed(mailbox)?;
        let range = ix
            .ranges
            .get(id.0 as usize)
            .ok_or(Error::MessageNotFound(id.0))?
            .clone();
        Ok(unescape_from(&ix.bytes()[range]))
    }
}

fn index_file(path: &Path, len: u64, mtime: Option<SystemTime>) -> Result<Indexed> {
    let map = if len == 0 {
        None
    } else {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        // SAFETY: the map is read-only. If another process truncates the file while
        // we read it the behaviour is undefined; this is the same trade-off other
        // clients using mmap accept. We detect changes by size/mtime and re-index.
        Some(unsafe { Mmap::map(&file) }.map_err(|e| Error::io(path, e))?)
    };
    let bytes = map.as_deref().unwrap_or(&[]);
    let ranges = split_mbox(bytes);
    let envelopes = ranges
        .par_iter()
        .enumerate()
        .map(|(i, r)| envelope_from_headers(MessageId(i as u64), &bytes[r.clone()], r.len()))
        .collect();
    Ok(Indexed { map, len, mtime, ranges, envelopes })
}

/// Returns the content range of each message (without the `From ` line).
pub fn split_mbox(data: &[u8]) -> Vec<Range<usize>> {
    let mut starts = Vec::new();
    if data.starts_with(b"From ") {
        starts.push(0);
    }
    starts.extend(memmem::find_iter(data, b"\nFrom ").map(|p| p + 1));

    let mut out = Vec::with_capacity(starts.len());
    for (i, &sep) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(data.len());
        let body_start = match memchr::memchr(b'\n', &data[sep..end]) {
            Some(nl) => sep + nl + 1,
            None => end,
        };
        // The format adds a blank line before the next separator.
        let mut body_end = end;
        if data[body_start..body_end].ends_with(b"\n\n") {
            body_end -= 1;
        } else if data[body_start..body_end].ends_with(b"\r\n\r\n") {
            body_end -= 2;
        }
        out.push(body_start..body_end);
    }
    out
}

/// mboxrd: lines `>From `, `>>From `... lose one `>`.
pub fn unescape_from(data: &[u8]) -> Vec<u8> {
    if memmem::find(data, b">From ").is_none() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    for line in data.split_inclusive(|&b| b == b'\n') {
        let quotes = line.iter().take_while(|&&b| b == b'>').count();
        if quotes > 0 && line[quotes..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

fn collect_mboxes(base: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, String)>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let fname = entry.file_name();
        if fname.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() && depth < 4 {
            collect_mboxes(base, &path, depth + 1, out)?;
        } else if ft.is_file() && looks_like_mbox(&path) {
            let name = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().into_owned();
            out.push((path.to_string_lossy().into_owned(), name));
        }
    }
    Ok(())
}

fn looks_like_mbox(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 5];
    match File::open(path) {
        Ok(mut f) => match f.read(&mut buf) {
            Ok(0) => path.extension().is_some_and(|e| e == "mbox"),
            Ok(n) => &buf[..n] == b"From ",
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Inbox first, then the rest alphabetically ("entrada" is the Spanish name).
fn mailbox_rank(name: &str) -> u8 {
    match name.to_ascii_lowercase().trim_end_matches(".mbox") {
        "inbox" | "entrada" => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBOX: &[u8] = b"From a@x Mon Jan  1 00:00:00 2024\nSubject: one\nStatus: RO\n\nhello\n>From here\n\n\
From b@x Mon Jan  1 00:00:00 2024\nSubject: two\n\nbye\n";

    #[test]
    fn splits_and_unescapes() {
        let r = split_mbox(MBOX);
        assert_eq!(r.len(), 2);
        assert_eq!(&MBOX[r[0].clone()], b"Subject: one\nStatus: RO\n\nhello\n>From here\n");
        assert_eq!(unescape_from(&MBOX[r[0].clone()]), b"Subject: one\nStatus: RO\n\nhello\nFrom here\n");
        assert_eq!(&MBOX[r[1].clone()], b"Subject: two\n\nbye\n");
    }

    #[test]
    fn backend_roundtrip() {
        let dir = std::env::temp_dir().join(format!("orion-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("inbox");
        std::fs::write(&f, MBOX).unwrap();
        let mut b = MboxBackend::new(dir.clone());
        let mbs = b.mailboxes().unwrap();
        assert_eq!(mbs.len(), 1);
        assert_eq!((mbs[0].total, mbs[0].unread), (2, 1));
        let envs = b.envelopes(&mbs[0].id).unwrap();
        assert_eq!(envs[1].subject, "two");
        let raw = b.fetch_raw(&mbs[0].id, MessageId(1)).unwrap();
        assert!(raw.ends_with(b"bye\n"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
