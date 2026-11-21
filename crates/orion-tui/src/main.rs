mod app;
mod body;
mod client;
mod contacts;
mod filepicker;
mod compose_panel;
mod compose_ui;
mod daemon;
mod ipc;
mod keys;
mod setup;
mod snapshot;
mod theme;
mod ui;
mod worker;

#[cfg(test)]
mod e2e;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ratatui_image::picker::{Picker, ProtocolType};

use orion_core::config::{cache_path, AccountConfig, AccountKind, Config, ImageProtocol, EXAMPLE_CONFIG};

use app::{Account, App};
use keys::Keymap;
use worker::Event;

/// Terminal mail client.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Configuration file (default: ~/.config/orion/config.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Open an mbox file (or a directory) directly, without a configuration.
    #[arg(short, long)]
    mbox: Option<PathBuf>,
    /// SQLite cache path (default: ~/.cache/orion/cache.sqlite).
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Do not use the local cache.
    #[arg(long)]
    no_cache: bool,
    /// Print an example configuration and exit.
    #[arg(long)]
    print_config: bool,
    /// Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run in the background: sync, detect new mail, notify and serve the local API.
    Daemon,
    /// Show unread counts from the running daemon.
    Status {
        /// Machine-readable JSON.
        #[arg(long, conflicts_with = "waybar")]
        json: bool,
        /// Output for a Waybar custom module ({"text","tooltip","class"}).
        #[arg(long)]
        waybar: bool,
    },
    /// Print daemon events (new mail, counts, errors) as they happen.
    Watch {
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Ask the daemon to check for new mail now.
    Sync,
    /// Search the address book ([contacts] sources): "Name <email>" lines.
    Contacts {
        /// Text to look for (in names and addresses); all contacts if omitted.
        query: Option<String>,
    },
    /// Add, sign in, test and list accounts.
    #[command(subcommand)]
    Account(AccountCmd),
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Add an account: detects the provider, opens the app-password page or
    /// signs in with Microsoft, tests the connection and saves it.
    Add {
        /// Email address (asked if omitted).
        email: Option<String>,
    },
    /// Sign in again (expired Microsoft sign-in, new app password).
    Login { name: String },
    /// Connect to the accounts and report mailbox counts.
    Test { name: Option<String> },
    /// List the configured accounts.
    List,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.print_config {
        print!("{EXAMPLE_CONFIG}");
        return Ok(());
    }

    // `account add` is how a configuration gets created: a missing file is fine there.
    let creating = matches!(args.command, Some(Cmd::Account(AccountCmd::Add { .. })));
    let mut cfg = match Config::load(args.config.as_deref()) {
        Err(orion_core::Error::Io { source, .. }) if creating && source.kind() == std::io::ErrorKind::NotFound => Config::default(),
        other => other.context("loading the configuration")?,
    };
    if let Some(path) = args.mbox {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "mbox".into());
        cfg.accounts = vec![AccountConfig::new(name, AccountKind::Mbox { path })];
    }
    let cache = if args.no_cache { None } else { args.cache.clone().or_else(cache_path) };
    let socket = args.socket.clone().unwrap_or_else(|| cfg.daemon.socket_path());
    if let Some(Cmd::Account(cmd)) = &args.command {
        let path = args.config.clone().or_else(Config::default_path).context("no configuration directory")?;
        let mut io = setup::Terminal;
        let deps = setup::Deps::real();
        return match cmd {
            AccountCmd::Add { email } => setup::add(&mut io, &deps, &path, &cfg, email.clone()).map(|name| {
                println!("Try it with `orion account test {name}`, or just run `orion`.");
            }),
            AccountCmd::Login { name } => setup::login(&mut io, &deps, &cfg, name),
            AccountCmd::Test { name } => setup::test(&mut io, &deps, &cfg, name.as_deref()),
            AccountCmd::List => {
                setup::list(&mut io, &cfg);
                Ok(())
            }
        };
    }
    // The API clients only need the socket, not accounts.
    match args.command {
        Some(Cmd::Status { json, waybar }) => {
            let format = if json {
                client::StatusFormat::Json
            } else if waybar {
                client::StatusFormat::Waybar
            } else {
                client::StatusFormat::Human
            };
            return client::print_status(&socket, format);
        }
        Some(Cmd::Watch { json }) => return client::watch(&socket, json),
        Some(Cmd::Sync) => return client::sync(&socket),
        Some(Cmd::Contacts { ref query }) => {
            let (all, warnings) = contacts::load(&cfg.contacts.sources, cache.as_deref());
            for w in warnings {
                eprintln!("{w}");
            }
            let found: Vec<&contacts::Contact> = match query {
                Some(q) => contacts::search(&all, q, usize::MAX),
                None => all.iter().collect(),
            };
            for c in found {
                println!("{}", c.display());
            }
            return Ok(());
        }
        _ => {}
    }
    if cfg.accounts.is_empty() {
        let path = Config::default_path().map(|p| p.display().to_string()).unwrap_or_default();
        anyhow::bail!(
            "no accounts configured.\n  Use `orion --mbox <file>` or create {path}\n  (`orion --print-config` prints an example)"
        );
    }
    if let Some(Cmd::Daemon) = args.command {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
            signal_hook::flag::register(sig, stop.clone()).context("installing signal handlers")?;
        }
        return daemon::run(cfg, cache, socket, stop);
    }
    // Reading or flagging mail here updates the shared cache: a running daemon
    // is told to re-read it, so the status bar follows at once.
    if cache.is_some() {
        let _ = client::NOTIFY_SOCKET.set(socket.clone());
    }
    let (keymap, mut warnings) = Keymap::new(&cfg.keys);
    warnings.extend(theme::init(&cfg.ui.theme, cfg.ui.icons));

    let (tx, rx) = mpsc::channel::<Event>();
    let gpg = cfg.pgp.enabled.then(|| orion_crypto::Gpg::from_config(&cfg.pgp));

    // Terminal and graphics detection first (it reads stdin), then the threads.
    let mut terminal = ratatui::init();
    let picker = make_picker(cfg.ui.image_protocol);

    let accounts: Vec<Account> = cfg
        .accounts
        .iter()
        .enumerate()
        .map(|(i, acc)| Account {
            name: acc.name.clone(),
            tx: worker::spawn(i, acc.clone(), cache.clone(), gpg.clone(), tx.clone()),
            mailboxes: Vec::new(),
            error: None,
            progress: "connecting…".into(),
        })
        .collect();

    let input_tx = tx.clone();
    let input = std::sync::Arc::new(InputControl::default());
    let ctl = input.clone();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            use std::sync::atomic::Ordering;
            // ORION_KEYLOG=<file>: timing of each key (gap in ms and its kind,
            // never the character itself), to tune the YubiKey OTP detection.
            let mut keylog = std::env::var_os("ORION_KEYLOG").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
            let mut last_key = std::time::Instant::now();
            loop {
                // While $EDITOR runs the terminal belongs to it: do not read keys.
                if ctl.paused.load(Ordering::Acquire) {
                    ctl.idle.store(true, Ordering::Release);
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                ctl.idle.store(false, Ordering::Release);
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => {
                        // Hold keys back only while they could still be a
                        // YubiKey OTP (modhex typed in a burst); anything else
                        // is delivered at once.
                        let mut burst = Vec::new();
                        let mut failed = false;
                        loop {
                            match crossterm::event::read() {
                                Ok(ev) => burst.push(ev),
                                Err(_) => {
                                    failed = true;
                                    break;
                                }
                            }
                            if !could_be_otp(&burst) || !matches!(crossterm::event::poll(Duration::from_millis(OTP_GAP_MS)), Ok(true)) {
                                break;
                            }
                        }
                        if let Some(f) = keylog.as_mut() {
                            use std::io::Write;
                            let kinds: String = burst
                                .iter()
                                .map(|e| match e {
                                    crossterm::event::Event::Key(k) if k.kind == crossterm::event::KeyEventKind::Release => 'r',
                                    crossterm::event::Event::Key(k) if k.code == crossterm::event::KeyCode::Enter => 'E',
                                    crossterm::event::Event::Key(k) if is_modhex(&k.code) => 'm',
                                    crossterm::event::Event::Key(_) => 'o',
                                    _ => '.',
                                })
                                .collect();
                            let _ = writeln!(f, "+{}ms burst={} kinds={kinds} otp={}", last_key.elapsed().as_millis(), burst.len(), is_otp_burst(&burst));
                            last_key = std::time::Instant::now();
                        }
                        let events = if is_otp_burst(&burst) { vec![Event::OtpIgnored] } else { burst.into_iter().map(Event::Input).collect() };
                        if events.into_iter().any(|e| input_tx.send(e).is_err()) || failed {
                            break;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
        .context("input thread")?;

    // The address book loads in the background (EDS may need a moment).
    {
        let (sources, cache, tx) = (cfg.contacts.sources.clone(), cache.clone(), tx.clone());
        std::thread::spawn(move || {
            let (list, _) = contacts::load(&sources, cache.as_deref());
            let _ = tx.send(Event::Contacts(list));
        });
    }
    let mut app = App::new(cfg, keymap, picker, tx, accounts);
    app.gpg = gpg;
    if !warnings.is_empty() {
        app.status = warnings.join("; ");
    }
    let result = run(&mut terminal, &mut app, rx, &input);
    ratatui::restore();
    result
}

/// Keys arriving closer than this are one burst (typed by a device, not a
/// person). Generous: through USB passthrough a YubiKey types ~1 key / 16 ms.
const OTP_GAP_MS: u64 = 60;
const MODHEX: &str = "cbdefghijklnrtuv";

fn pressed_keys(events: &[crossterm::event::Event]) -> Option<Vec<crossterm::event::KeyCode>> {
    use crossterm::event::{Event as Ev, KeyEventKind};
    let mut keys = Vec::new();
    for e in events {
        match e {
            Ev::Key(k) if k.kind == KeyEventKind::Release => {}
            Ev::Key(k) => keys.push(k.code),
            _ => return None,
        }
    }
    Some(keys)
}

fn is_modhex(code: &crossterm::event::KeyCode) -> bool {
    matches!(code, crossterm::event::KeyCode::Char(c) if MODHEX.contains(c.to_ascii_lowercase()))
}

/// Whether the keys so far may still be the start of an OTP. A held key
/// (auto-repeat: one character over and over) is not, so scrolling with a held
/// `j` is never delayed past the first keys.
fn could_be_otp(events: &[crossterm::event::Event]) -> bool {
    let Some(keys) = pressed_keys(events) else { return false };
    if keys.len() > 64 || !keys.iter().all(is_modhex) {
        return false;
    }
    let head = &keys[..keys.len().min(12)];
    let mut distinct: Vec<_> = head.to_vec();
    distinct.sort_by_key(|k| format!("{k:?}"));
    distinct.dedup();
    !(head.len() >= 4 && distinct.len() == 1) && !(head.len() == 12 && distinct.len() < 4)
}

/// A YubiKey "touch" in OTP mode types 32–48 modhex characters (`cbdefghijklnrtuv`)
/// and Enter. Delivered as keys, they would run commands (`c` = compose…).
fn is_otp_burst(events: &[crossterm::event::Event]) -> bool {
    let Some(keys) = pressed_keys(events) else { return false };
    let chars = keys.strip_suffix(&[crossterm::event::KeyCode::Enter]).unwrap_or(&keys);
    let mut distinct: Vec<_> = chars.iter().map(|k| format!("{k:?}")).collect();
    distinct.sort();
    distinct.dedup();
    (32..=64).contains(&chars.len()) && chars.iter().all(is_modhex) && distinct.len() >= 6
}

#[cfg(test)]
mod otp_tests {
    use super::is_otp_burst;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn keys(s: &str, enter: bool) -> Vec<Event> {
        let mut v: Vec<Event> = s.chars().map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))).collect();
        if enter {
            v.push(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        }
        v
    }

    #[test]
    fn yubikey_otp_is_recognised() {
        // A real-shaped Yubico OTP: 12 public id + 32 one-time modhex characters.
        assert!(is_otp_burst(&keys("ccccccbdefghijklnrtuvcbdefghijklnrtuvcbdefgh", true)));
        assert!(is_otp_burst(&keys("ccccccbdefghijklnrtuvcbdefghijklnrtuvcbdefgh", false)));
        // The one from the screenshot (its first `c` had opened the composer).
        assert!(is_otp_burst(&keys("ccccbhgtunvhcbtjjfjnfhfnevbrileedtduunlnihr", true)));
        // Ordinary key presses, even repeated quickly, are not.
        assert!(!is_otp_burst(&keys(&"j".repeat(40), false)), "held j");
        assert!(!super::could_be_otp(&keys("jjjj", false)), "held j is delivered at once");
        assert!(super::could_be_otp(&keys("ccccbhgtunvh", false)));
        assert!(!super::could_be_otp(&keys("cq", false)), "non-modhex key flushes");
        assert!(!is_otp_burst(&keys("jjjj", false)));
        assert!(!is_otp_burst(&keys("c", false)));
        // Long bursts with non-modhex characters (e.g. pasted text) are not either.
        assert!(!is_otp_burst(&keys("hello world, this is pasted text from somewhere", true)));
    }
}

/// Detects the graphics protocol (sixel/kitty/iTerm2) by querying the terminal.
/// Must be called after entering the alternate screen and before reading events.
fn make_picker(pref: ImageProtocol) -> Option<Picker> {
    if pref == ImageProtocol::None {
        return None;
    }
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let forced = match pref {
        ImageProtocol::Sixel => Some(ProtocolType::Sixel),
        ImageProtocol::Kitty => Some(ProtocolType::Kitty),
        ImageProtocol::Iterm2 => Some(ProtocolType::Iterm2),
        ImageProtocol::Halfblocks => Some(ProtocolType::Halfblocks),
        ImageProtocol::Auto | ImageProtocol::None => None,
    };
    if let Some(p) = forced {
        picker.set_protocol_type(p);
    }
    Some(picker)
}

/// Lets the main loop hand the terminal to `$EDITOR`: the input thread stops
/// reading keys while `paused`, and confirms it with `idle`.
#[derive(Default)]
struct InputControl {
    paused: std::sync::atomic::AtomicBool,
    idle: std::sync::atomic::AtomicBool,
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

/// Which editor to run: the config, then $VISUAL, $EDITOR, then the first
/// installed friendly editor. Plain `vi` is last: on many systems it is
/// `vim -C` with no configuration, where Backspace does not erase.
fn resolve_editor(configured: Option<&str>, env: &dyn Fn(&str) -> Option<String>, installed: &dyn Fn(&str) -> bool) -> String {
    let set = |v: Option<String>| v.filter(|e| !e.trim().is_empty());
    set(configured.map(str::to_string))
        .or_else(|| set(env("VISUAL")))
        .or_else(|| set(env("EDITOR")))
        .or_else(|| ["nano", "nvim", "vim", "vi"].iter().find(|e| installed(e)).map(|e| e.to_string()))
        .unwrap_or_else(|| "vi".into())
}

/// Suspends the TUI, runs the editor on `path`, and restores it.
fn run_editor(terminal: &mut ratatui::DefaultTerminal, input: &InputControl, path: &std::path::Path, configured: Option<&str>) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    input.paused.store(true, Ordering::Release);
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while !input.idle.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    ratatui::restore();
    let editor = resolve_editor(configured, &|var| std::env::var(var).ok(), &on_path);
    // Through sh so EDITOR may carry arguments ("code --wait"); the path is a
    // separate argument, never interpolated into the command.
    let status = std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("orion-editor").arg(path).status();
    *terminal = ratatui::init();
    input.idle.store(false, Ordering::Release);
    input.paused.store(false, Ordering::Release);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{editor} exited with {s}")),
        Err(e) => Err(format!("cannot run {editor}: {e}")),
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, rx: mpsc::Receiver<Event>, input: &InputControl) -> Result<()> {
    let sixel = app.picker.as_ref().is_some_and(|p| p.protocol_type() == ProtocolType::Sixel);
    let mut last_heavy = Instant::now() - SIXEL_FRAME;
    while !app.quit {
        if let Some(path) = app.pending_editor.take() {
            let configured = app.cfg.ui.editor.clone();
            let result = run_editor(terminal, input, &path, configured.as_deref());
            app.after_editor(result);
            app.needs_clear = true;
        }
        let clear = std::mem::take(&mut app.needs_clear);
        if clear {
            // Shown at once (synchronized update): no flicker from the blank frame.
            let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::BeginSynchronizedUpdate);
            // A blank frame forces every cell to be rewritten, erasing sixel
            // leftovers. (Terminal::clear() queries the cursor position and would
            // fail on terminals that do not answer.)
            terminal.draw(|f| f.render_widget(ratatui::widgets::Clear, f.area()))?;
        }
        terminal.draw(|f| ui::draw(f, app))?;
        if clear {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EndSynchronizedUpdate);
        }
        // Block until the next event and process the pending burst before redrawing.
        let heavy_frame = clear && sixel;
        if heavy_frame {
            last_heavy = Instant::now();
        }
        let Ok(ev) = rx.recv() else { break };
        app.on_event(ev);
        for ev in rx.try_iter().take(256) {
            app.on_event(ev);
            if app.quit {
                break;
            }
        }
        // A full sixel redraw is several MB the terminal must decode (foot
        // takes tens of ms): while keys keep coming (a held `j`), gather
        // them and draw once, instead of queueing frames it cannot keep up with.
        if sixel && app.needs_clear && !app.quit {
            let until = last_heavy + SIXEL_FRAME;
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(ev) => app.on_event(ev),
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

/// Minimum time between two full sixel redraws.
const SIXEL_FRAME: Duration = Duration::from_millis(120);

#[cfg(test)]
mod editor_tests {
    use super::resolve_editor;

    #[test]
    fn editor_choice() {
        let none = |_: &str| None;
        let only = |names: &'static [&'static str]| move |p: &str| names.contains(&p);
        assert_eq!(resolve_editor(Some("hx"), &|_| Some("vim".into()), &only(&["nano"])), "hx", "config wins");
        assert_eq!(resolve_editor(None, &|v| (v == "EDITOR").then(|| "emacs -nw".into()), &only(&["nano"])), "emacs -nw");
        assert_eq!(resolve_editor(None, &|v| (v == "VISUAL").then(|| " ".into()), &only(&["vi", "nano"])), "nano", "blank vars ignored");
        assert_eq!(resolve_editor(None, &none, &only(&["vi", "vim", "nano"])), "nano", "plain vi only as last resort");
        assert_eq!(resolve_editor(None, &none, &only(&["vi"])), "vi");
    }
}
