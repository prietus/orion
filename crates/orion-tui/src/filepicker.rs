//! File browser for choosing attachments: no external tools needed.
//!
//! Type to filter (fuzzy), arrows to move, Enter opens a folder or attaches,
//! Space/Tab marks several files, Backspace goes up. A typed or pasted path
//! (also `file://…`, as terminals paste on drag and drop) is used as is.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Folder of the last attachment, to start there next time.
static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
const MAX_ENTRIES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Cancel,
    Attach(Vec<PathBuf>),
}

pub struct FilePicker {
    pub dir: PathBuf,
    entries: Vec<Entry>,
    pub filter: String,
    pub sel: usize,
    pub marked: Vec<PathBuf>,
    pub hidden: bool,
    pub error: Option<String>,
}

impl FilePicker {
    /// Starts in the last folder used, else ~/Downloads, else home.
    pub fn open() -> FilePicker {
        let last = LAST_DIR.lock().ok().and_then(|l| l.clone()).filter(|d| d.is_dir());
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let downloads = dirs::download_dir().filter(|d| d.is_dir());
        Self::at(last.or(downloads).unwrap_or(home))
    }

    pub fn at(dir: PathBuf) -> FilePicker {
        let mut p = FilePicker { dir, entries: Vec::new(), filter: String::new(), sel: 0, marked: Vec::new(), hidden: false, error: None };
        p.load();
        p
    }

    fn load(&mut self) {
        self.entries.clear();
        self.error = None;
        match std::fs::read_dir(&self.dir) {
            Ok(rd) => {
                for e in rd.flatten().take(MAX_ENTRIES) {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !self.hidden && name.starts_with('.') {
                        continue;
                    }
                    // Follows symlinks: a link to a folder is a folder.
                    let meta = std::fs::metadata(e.path()).ok();
                    let dir = meta.as_ref().is_some_and(|m| m.is_dir());
                    self.entries.push(Entry { name, dir, size: meta.map(|m| m.len()).unwrap_or(0) });
                }
                self.entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            }
            Err(e) => self.error = Some(format!("{}: {e}", self.dir.display())),
        }
        self.sel = 0;
    }

    /// Whether the filter is a path rather than a name to search for.
    fn path_mode(&self) -> bool {
        let f = self.filter.trim_start_matches(['\'', '"']);
        f.starts_with('/') || f.starts_with('~') || f.starts_with("file://")
    }

    /// Entries to show: ".." first, then the ones matching the filter, best first.
    pub fn visible(&self) -> Vec<&Entry> {
        static UP: Entry = Entry { name: String::new(), dir: true, size: 0 };
        if self.path_mode() {
            return Vec::new();
        }
        let q = self.filter.to_lowercase();
        let mut v: Vec<(u8, &Entry)> = self.entries.iter().filter_map(|e| rank(&e.name.to_lowercase(), &q).map(|r| (r, e))).collect();
        v.sort_by_key(|(r, _)| *r); // stable: keeps folders first, then by name
        let mut out: Vec<&Entry> = Vec::with_capacity(v.len() + 1);
        if q.is_empty() && self.dir.parent().is_some() {
            out.push(&UP);
        }
        out.extend(v.into_iter().map(|(_, e)| e));
        out
    }

    fn go(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.filter.clear();
        self.load();
    }

    fn up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let from = self.dir.file_name().map(|n| n.to_string_lossy().into_owned());
            self.go(parent);
            // Keep the cursor on the folder we came from.
            if let Some(from) = from {
                self.sel = self.visible().iter().position(|e| e.name == from).unwrap_or(0);
            }
        }
    }

    fn attach(&self, paths: Vec<PathBuf>) -> Outcome {
        if let Some(d) = paths.first().and_then(|p| p.parent())
            && let Ok(mut l) = LAST_DIR.lock() {
                *l = Some(d.to_path_buf());
            }
        Outcome::Attach(paths)
    }

    pub fn key(&mut self, ev: KeyEvent) -> Outcome {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.visible().len();
        match ev.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if ctrl => return Outcome::Cancel,
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Char('p') if ctrl => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('n') if ctrl => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(10),
            KeyCode::PageDown => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = n.saturating_sub(1),
            KeyCode::Char('t') if ctrl => {
                self.hidden = !self.hidden;
                self.load();
            }
            KeyCode::Left if self.filter.is_empty() => self.up(),
            KeyCode::Backspace if self.filter.is_empty() => self.up(),
            KeyCode::Backspace => {
                self.filter.pop();
                self.sel = 0;
            }
            KeyCode::Right => {
                if let Some(e) = self.visible().get(self.sel).filter(|e| e.dir && !e.name.is_empty()) {
                    let d = self.dir.join(&e.name);
                    self.go(d);
                }
            }
            KeyCode::Char(' ') | KeyCode::Tab if !self.path_mode() => {
                if let Some(e) = self.visible().get(self.sel).filter(|e| !e.dir) {
                    let p = self.dir.join(&e.name);
                    match self.marked.iter().position(|m| *m == p) {
                        Some(i) => {
                            self.marked.remove(i);
                        }
                        None => self.marked.push(p),
                    }
                }
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::Enter => return self.enter(),
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.sel = 0;
            }
            _ => {}
        }
        Outcome::Continue
    }

    fn enter(&mut self) -> Outcome {
        if self.path_mode() {
            let p = expand(&self.filter);
            if p.is_dir() {
                self.go(p);
            } else if p.is_file() {
                return self.attach(vec![p]);
            } else {
                self.error = Some(format!("{} does not exist", p.display()));
            }
            return Outcome::Continue;
        }
        let Some(e) = self.visible().get(self.sel).map(|e| (*e).clone()) else {
            if !self.marked.is_empty() {
                return self.attach(self.marked.clone());
            }
            return Outcome::Continue;
        };
        if e.name.is_empty() {
            self.up();
        } else if e.dir {
            let d = self.dir.join(&e.name);
            self.go(d);
        } else if self.marked.is_empty() {
            return self.attach(vec![self.dir.join(&e.name)]);
        } else {
            return self.attach(self.marked.clone());
        }
        Outcome::Continue
    }
}

/// How well `name` matches `q` (lower is better), or `None`: prefix, then
/// substring, then the letters in order ("fact24" → "factura_2024.pdf").
pub(crate) fn rank(name: &str, q: &str) -> Option<u8> {
    let q: String = q.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    if name.starts_with(&q) {
        return Some(0);
    }
    if name.contains(&q) {
        return Some(1);
    }
    let mut chars = name.chars();
    q.chars().all(|c| chars.any(|n| n == c)).then_some(2)
}

/// A typed or pasted path: `~`, quotes, `file://` and `%20` handled.
pub fn expand(raw: &str) -> PathBuf {
    let s = raw.trim().trim_matches(['\'', '"']);
    let s = s.strip_prefix("file://").map(percent_decode).unwrap_or_else(|| s.to_string());
    // Shells and some terminals escape spaces when pasting a path.
    let s = s.replace("\\ ", " ");
    match s.strip_prefix('~') {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest.trim_start_matches('/'))).unwrap_or_else(|| PathBuf::from(&s)),
        None => PathBuf::from(s),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn human_size(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.0} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    fn typ(p: &mut FilePicker, s: &str) {
        for c in s.chars() {
            assert_eq!(p.key(k(KeyCode::Char(c))), Outcome::Continue);
        }
    }

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Documents/2024")).unwrap();
        std::fs::write(d.path().join("Documents/2024/factura_2024.pdf"), b"pdf").unwrap();
        std::fs::write(d.path().join("Documents/notes.txt"), b"n").unwrap();
        std::fs::write(d.path().join("photo one.jpg"), b"jpg").unwrap();
        std::fs::write(d.path().join(".secret"), b"s").unwrap();
        d
    }

    #[test]
    fn browse_filter_and_attach() {
        let d = tree();
        let mut p = FilePicker::at(d.path().to_path_buf());
        let names: Vec<String> = p.visible().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["", "Documents", "photo one.jpg"], "'..' first, folders first, no hidden files");
        // Fuzzy filter, then into folders with Enter.
        typ(&mut p, "doc");
        assert_eq!(p.visible()[0].name, "Documents");
        p.key(k(KeyCode::Enter));
        assert_eq!(p.dir, d.path().join("Documents"));
        assert!(p.filter.is_empty());
        typ(&mut p, "24");
        p.key(k(KeyCode::Enter));
        typ(&mut p, "fct24");
        assert_eq!(p.key(k(KeyCode::Enter)), Outcome::Attach(vec![d.path().join("Documents/2024/factura_2024.pdf")]));
        // Backspace first erases the filter ("fct24"), then goes up, keeping
        // the cursor on the folder we came from.
        for _ in 0..5 {
            p.key(k(KeyCode::Backspace));
        }
        assert!(p.filter.is_empty());
        assert_eq!(p.dir, d.path().join("Documents/2024"));
        p.key(k(KeyCode::Backspace));
        assert_eq!(p.dir, d.path().join("Documents"));
        assert_eq!(p.visible()[p.sel].name, "2024");
        // Ctrl-T shows hidden files.
        p.key(k(KeyCode::Backspace));
        p.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(p.visible().iter().any(|e| e.name == ".secret"));
    }

    #[test]
    fn mark_several() {
        let d = tree();
        let mut p = FilePicker::at(d.path().join("Documents"));
        // ".." , "2024", "notes.txt"
        p.key(k(KeyCode::End));
        p.key(k(KeyCode::Char(' ')));
        assert_eq!(p.marked, [d.path().join("Documents/notes.txt")]);
        p.key(k(KeyCode::Home));
        p.key(k(KeyCode::Down)); // on the folder: Enter opens it, marks are kept
        p.key(k(KeyCode::Enter));
        p.key(k(KeyCode::Down));
        p.key(k(KeyCode::Char(' ')));
        assert_eq!(
            p.key(k(KeyCode::Enter)),
            Outcome::Attach(vec![d.path().join("Documents/notes.txt"), d.path().join("Documents/2024/factura_2024.pdf")])
        );
    }

    #[test]
    fn typed_and_pasted_paths() {
        let d = tree();
        let mut p = FilePicker::at(PathBuf::from("/"));
        // Drag and drop pastes a quoted path or a file:// URI.
        let photo = d.path().join("photo one.jpg");
        typ(&mut p, &format!("'{}'", photo.display()));
        assert_eq!(p.key(k(KeyCode::Enter)), Outcome::Attach(vec![photo.clone()]));
        let mut p = FilePicker::at(PathBuf::from("/"));
        typ(&mut p, &format!("file://{}", photo.display().to_string().replace(' ', "%20")));
        assert_eq!(p.key(k(KeyCode::Enter)), Outcome::Attach(vec![photo]));
        // A folder path navigates there.
        let mut p = FilePicker::at(PathBuf::from("/"));
        typ(&mut p, &d.path().join("Documents").display().to_string());
        assert_eq!(p.key(k(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(p.dir, d.path().join("Documents"));
        assert_eq!(expand("~/x").file_name().unwrap(), "x");
        assert_eq!(expand("/tmp/a\\ b"), PathBuf::from("/tmp/a b"));
    }

    #[test]
    fn ranking() {
        assert_eq!(rank("factura_2024.pdf", "fact"), Some(0));
        assert_eq!(rank("mi_factura.pdf", "fact"), Some(1));
        assert_eq!(rank("factura_2024.pdf", "fct24"), Some(2));
        assert_eq!(rank("notes.txt", "fct"), None);
        assert_eq!(human_size(2048), "2 KB");
    }
}
