# Development

## Layout

```
crates/
  orion-core      model, Backend trait, configuration, MIME parsing, compose
  orion-backends  mbox, IMAP + IDLE, POP3, JMAP, SMTP, autoconfig, OAuth, keyring
  orion-store     SQLite cache: folders, headers, sync state, bodies
  orion-html      HTML + CSS → styled document → layout at a given width
  orion-crypto    GnuPG: decrypt, verify, sign, encrypt, cards; PGP/MIME, Autocrypt
  orion-tui       the `orion` binary: interface, account threads, daemon, wizard
scripts/          fake servers for the tests, screenshot renderer
contrib/          systemd unit, Waybar module, waymobar widget
wiki/             this site
```

## Runtime architecture

```
input thread ─────┐
account thread 1 ─┼──► event channel ──► UI loop (never blocks)
account thread N ─┘         ▲
image decoding, Chromium ───┘
```

Each account has its own thread with its backend and its SQLite connection.
The interface never waits for the network: it shows what is cached and updates
when the account thread reports back. The daemon runs the same account
threads without a screen.

## Tests

```sh
cargo test
```

The suite runs everything it can on the machine and skips, rather than fails,
what needs a missing tool:

- a real **Dovecot** (IMAP, POP3, IDLE, TLS and STARTTLS with a throwaway
  CA), started unprivileged on free localhost ports;
- real **GnuPG** in a temporary home;
- real **Chromium** for the original view (checking that nothing reaches the
  network);
- small fake servers in `scripts/` for edge cases (IMAP throttling, JMAP,
  SMTP, OAuth, HKP keyservers).

## Screenshots

The screenshots on this site are made from a test with invented data:

```sh
ORION_SHOT_DIR=/tmp/shots cargo test -p orion-tui screenshots -- --ignored
python3 scripts/render_shot.py /tmp/shots/list.json list.png
```

## This site

The site is [MkDocs Material](https://squidfunk.github.io/mkdocs-material/),
built from `wiki/`:

```sh
tools/wiki/publish.sh            # build into wiki/site and check it
tools/wiki/publish.sh --serve    # preview at http://127.0.0.1:8000
tools/wiki/publish.sh --upload   # build and publish
```

The configuration and key references are generated from Orion itself
(`orion --print-config`, `orion --print-keys`), so they cannot go stale.

## Contributing

Issues and pull requests are welcome on
[GitHub](https://github.com/prietus/orion). Please run `cargo test` and
`cargo clippy --all-targets` before sending a change.
