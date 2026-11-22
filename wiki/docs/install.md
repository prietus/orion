# Installation

## Build from source

Orion is a single binary. You need **Rust 1.98 or newer**
([rustup](https://rustup.rs) installs it).

```sh
git clone https://github.com/prietus/orion
cd orion
cargo build --release
install -Dm755 target/release/orion ~/.local/bin/orion
```

Make sure `~/.local/bin` is in your `PATH`. To always run the latest build
while you follow the repository, a symlink works too:

```sh
ln -sf "$PWD/target/release/orion" ~/.local/bin/orion
```

## Optional companions

Orion works without any of these; each one unlocks a feature.

| Package | What it enables |
|---|---|
| `gnupg` | OpenPGP: decrypt, verify, sign, encrypt; YubiKey through `scdaemon` |
| `chromium` | the *original view* of HTML mail (`V`) |
| a Nerd Font, or *Symbols Nerd Font* | icons for folders, attachments, encryption |
| `noto-fonts-cjk` | Chinese, Japanese and Korean text |
| `libsecret` (`secret-tool`) | storing passwords and OAuth tokens in the system keyring |
| `evolution-data-server` | address suggestions from GNOME Online Accounts |
| `libnotify` (`notify-send`) | desktop notifications from the daemon |
| `xdg-utils` (`xdg-open`) | opening links and attachments |

On Arch Linux:

```sh
sudo pacman -S gnupg chromium ttf-nerd-fonts-symbols noto-fonts-cjk libsecret libnotify xdg-utils
```

## A terminal with graphics

Images need a terminal that speaks a graphics protocol:

- **kitty protocol**: kitty, ghostty, WezTerm — the smoothest.
- **sixel**: foot, WezTerm, xterm (with sixel enabled), mlterm.
- Anything else falls back to *halfblocks* (coloured characters).

Orion detects the protocol by itself; `[ui] image_protocol` forces one.

## Updating

```sh
git pull && cargo build --release
```

If the [daemon](daemon.md) is running, restart it after updating, so that
the UI and the daemon are the same version:

```sh
pkill -x orion; orion daemon &
```
