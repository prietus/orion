# Privacy and security

Orion treats every message as content from a stranger.

## Nothing phones home

- **Remote images** are not downloaded unless you press `L` for that message
  (or set `remote_images = "always"`). When they are: no cookies, no Referer,
  only `image/*` answers, size limits, and 1×1 tracking pixels skipped.
- **Keys** are never fetched or imported on their own. Looking up a
  recipient's key (`Ctrl-L`) or importing a sender's (`I`) is always your
  action.
- **Account autodetection** for your own domain only asks that domain (its
  autoconfig file over HTTPS and its DNS) — never a third-party database.

## HTML is never executed

- The text rendering is Orion's own layout code: no browser, no JavaScript.
- The **original view** (`V`) uses Chromium, locked down: `<script>` removed,
  a Content-Security-Policy that forbids scripts and every network load, a
  dead proxy and a throwaway profile deleted afterwards. Tested with a local
  server that must never receive a request.
- Only `http`, `https` and `mailto` links can be opened.

## Secrets

- Passwords and OAuth tokens live in the **system keyring** (Secret Service)
  and travel to `secret-tool` over stdin, never on a command line.
- PINs typed in Orion go straight to gpg and are wiped from memory
  afterwards.
- Contacts from GNOME Online Accounts are read through Evolution Data Server:
  Orion never sees those accounts' passwords.

## Files

| What | Where | Access |
|---|---|---|
| configuration | `~/.config/orion/config.toml` | yours |
| cache (headers, bodies as received) | `~/.cache/orion/cache.sqlite` | yours |
| drafts | `~/.local/share/orion/drafts/` | `0600` / `0700` |
| accepted keys | `~/.local/share/orion/accepted-keys` | `0600` |
| daemon socket | `$XDG_RUNTIME_DIR/orion.sock` | `0600` |

Encrypted messages stay encrypted in the cache; the decrypted text only
exists in memory while you read it.

## Attachments and hooks

- Attachment names are sanitised: no paths, no hidden files, never overwriting
  an existing file.
- The daemon's `on_new_mail` hook gets message details as environment
  variables, never pasted into the command: a crafted subject cannot inject
  shell code.

## Network

- TLS everywhere with rustls and the system certificate store. STARTTLS never
  falls back to plain text, and data sent before the TLS handshake by a
  malicious server is rejected.
- A message is only sent if every recipient is accepted.
