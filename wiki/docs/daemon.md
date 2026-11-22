# Daemon and status bars

`orion daemon` runs the same account threads as the interface, without a
screen. It:

- syncs your accounts and watches the **watched** folders for new mail
  (`watch = ["INBOX"]` per account by default, `["*"]` for all), instantly
  with IMAP IDLE;
- sends desktop notifications (`notify-send`);
- runs an optional `on_new_mail` hook;
- answers a small JSON API on a Unix socket, for status bars and scripts.

The first sync after starting only records what is there (no notification
storm).

## Starting it

From your window manager's startup — for example
[rivermonad](https://rivermonad.priet.us)'s `startupHook`:

```haskell
spawnOnce "orion daemon"
```

or as a systemd user service:

```sh
sed 's|%h/.cargo/bin/orion|%h/.local/bin/orion|' contrib/orion.service > ~/.config/systemd/user/orion.service
systemctl --user daemon-reload
systemctl --user enable --now orion.service
```

!!! warning "After updating Orion"
    Restart the daemon (`pkill -x orion; orion daemon &`) so the interface
    and the daemon are the same version.

## From the shell

```sh
orion status              # unread per account and folder
orion status --json       # everything, as JSON
orion status --waybar     # for a status bar (see below)
orion watch               # print events as they happen
orion sync                # check for new mail now
```

## Status bars

`orion status --waybar` prints one line of JSON:

```json
{"text":"✉ 3","tooltip":"…","class":"unread","unread":3,"messages":[…],"errors":[]}
```

- `class`: `idle`, `unread`, `error` or `offline` (daemon not running).
- `messages`: the newest unread messages of the watched folders (account,
  folder, from, subject, date), for bars that build their own popover.
- `tooltip`: those messages as plain text; empty when there is nothing new.

When you read or flag mail in Orion, it tells the daemon, so the bar follows
at once.

### Waybar

`contrib/waybar.jsonc`:

```jsonc
"custom/mail": {
    "exec": "orion status --waybar",
    "return-type": "json",
    "interval": 30,
    "on-click": "orion sync",
    "format": "{}"
}
```

### waymobar

`contrib/waymobar.hs` is a widget for
[waymobar](https://github.com/prietus/waymobar): an envelope with the unread
count, and on hover the new messages (subject, sender, folder). A click opens
Orion in foot.

## The hook

```toml
[daemon]
on_new_mail = "notify-send \"$ORION_FROM\" \"$ORION_SUBJECT\""
```

The details arrive as environment variables — `ORION_ACCOUNT`,
`ORION_MAILBOX`, `ORION_COUNT`, `ORION_FROM`, `ORION_SUBJECT`,
`ORION_UNREAD` — and are never pasted into the command, so a crafted subject
cannot inject shell code.

## The API

A Unix socket at `$XDG_RUNTIME_DIR/orion.sock` (only accessible by you),
JSON, one object per line:

```sh
$ echo '{"cmd":"status"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/orion.sock
{"ok":true,"status":{"version":"0.1.0","unread":3,"accounts":[…],"messages":[…]}}
```

| Request | Reply |
|---|---|
| `{"cmd":"status"}` | accounts, folders (total/unread/watched), last sync, errors, unread messages |
| `{"cmd":"sync"}` | checks every account now |
| `{"cmd":"reload"}` | re-reads the counts from the shared cache, without network |
| `{"cmd":"subscribe"}` | then a stream of `new_mail`, `counts` and `error` events |
| `{"cmd":"ping"}` | `{"ok":true}` |
