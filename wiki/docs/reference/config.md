# Configuration file

Orion reads `~/.config/orion/config.toml` (`--config` for another path).
This is the complete example, as printed by `orion --print-config`; every
option is documented in it. Create yours with:

```sh
orion --print-config > ~/.config/orion/config.toml
```

```toml
# ~/.config/orion/config.toml

[ui]
# auto | sixel | kitty | iterm2 | halfblocks | none
image_protocol = "auto"
auto_show_images = true
sidebar_width = 26
image_pane_percent = 50
# date-desc | date-asc
sort = "date-desc"
# native HTML (true) or plain text (false); "v" toggles inside the reader
prefer_html = true
# auto (readable colours, no backgrounds) | full | none
html_colors = "auto"
max_image_rows = 30
link_markers = true
# a coloured chip with the sender's initials in the list and the reader
avatars = true
# colours: "mocha", "macchiato", "frappe", "latte" (Catppuccin) or "terminal"
theme = "mocha"
# Nerd Font icons (needs a Nerd Font or "Symbols Nerd Font" installed)
icons = true
# message list: "two-line" or "compact"
list_style = "two-line"
# "auto" (folders | list | message when the window is wide), "three-pane", "stacked"
layout = "auto"
# seconds a previewed message must be shown to count as read (0 = at once)
mark_read_after = 2
# writing mail: "builtin" (Orion's panel with address suggestions; Ctrl-E
# opens $EDITOR) or "editor" (straight to $EDITOR)
composer = "builtin"
# group the message list into conversations (t expands one, T toggles)
threads = true
# images hosted on the internet: "ask" (press L in a message), "always", "never"
remote_images = "ask"
open_command = "xdg-open"
# editor for writing mail (default: $VISUAL, $EDITOR, then nano/nvim/vim/vi)
# editor = "nvim"

[pgp]
enabled = true
gpg = "gpg"
# homedir = "~/.gnupg"
# orion: Orion asks for the YubiKey PIN / passphrase
# agent: gpg-agent's pinentry asks (use only with a graphical pinentry)
pinentry = "orion"
# maximum seconds per operation (includes waiting for you to touch the YubiKey)
timeout = 60
# where missing keys are looked up after WKD (keys.openpgp.org verifies addresses)
keyservers = ["hkps://keys.openpgp.org"]
# send your public key in an Autocrypt header (others can then reply encrypted)
autocrypt = true

# Background daemon: `orion daemon` (see contrib/orion.service)
[contacts]
# address suggestions when writing mail, in order of preference:
#   "eds"            GNOME / Evolution address books (incl. GNOME Online Accounts)
#   "mail"           people who wrote to you (from Orion's cache)
#   "vcard:~/Contacts"            a .vcf file or folder (vdirsyncer, khard...)
#   "command:khard email --parsable"   lines "email<TAB>name" (mutt query format)
sources = ["eds", "mail"]

[daemon]
# socket = "/run/user/1000/orion.sock"
notify = true
notify_command = "notify-send"
# Runs on new mail; details come in ORION_ACCOUNT, ORION_MAILBOX, ORION_COUNT,
# ORION_FROM, ORION_SUBJECT and ORION_UNREAD environment variables.
# on_new_mail = "paplay /usr/share/sounds/freedesktop/stereo/message.oga"

# Keys: action = ["key", ...]. Only list the ones you change.
# Format: "j", "G", "enter", "esc", "tab", "space", "up", "pgdown", "ctrl-d", "alt-x", "f5"
[keys]
# down = ["j", "down"]
# quit = ["q"]

[[account]]
name = "local"
backend = "mbox"
# An mbox file or a directory of mbox files
path = "~/Mail"

[[account]]
name = "work"
backend = "imap"
host = "imap.example.com"
# port = 993
# tls | starttls | none
tls = "tls"
user = "me@example.com"
password_command = "pass show mail/work"
# seconds between checks for new mail
poll_interval = 120
# mailboxes checked for new mail (default: the inbox; ["*"] = all)
watch = ["INBOX", "Lists/rust"]
cache_bodies = true
# IMAP IDLE: new mail arrives instantly (uses a second connection)
idle = true

[[account]]
name = "old-isp"
backend = "pop3"
host = "pop.example.net"
tls = "tls"
user = "me"
password_command = "pass show mail/isp"

[[account]]
name = "fastmail"
backend = "jmap"
url = "https://api.fastmail.com/jmap/session"
# basic (user + password) | bearer (password_command prints the token)
auth = "bearer"
password_command = "pass show mail/fastmail-token"
```
