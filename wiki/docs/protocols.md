# Accounts and protocols

`orion account add` writes these for you; this page is for doing it by hand
or understanding what was written. Everything goes in
`~/.config/orion/config.toml`.

## Backends

| Backend | Folders | Flags | New mail | Notes |
|---|---|---|---|---|
| `imap` | all, nested, international names | on the server | **IDLE** (instant) + polling | incremental sync (UIDVALIDITY / UIDNEXT) |
| `jmap` | all, nested, roles | keywords on the server | polling | Basic or Bearer auth; session discovery |
| `pop3` | INBOX | kept in the local cache | polling | only new headers are downloaded |
| `mbox` | a file or a directory | this session only | polling | read-only |

TLS always uses rustls with the system certificate store: `tls = "tls"`
(implicit TLS), `"starttls"`, or `"none"` (local testing only).

## IMAP

```toml
[[account]]
name = "work"
backend = "imap"
host = "imap.example.com"
tls = "tls"                       # tls (993) | starttls (143) | none
user = "me@example.com"
password_command = "secret-tool lookup service orion account work kind password"
from = "Me <me@example.com>"
signature = "Me"
idle = true                       # a second connection waits for new mail
poll_interval = 120               # seconds between checks
watch = ["INBOX"]                 # folders the daemon watches; ["*"] = all

[account.smtp]
host = "smtp.example.com"
port = 465
tls = "tls"
```

### Outlook / Hotmail / Live

```toml
[[account]]
name = "outlook"
backend = "imap"
host = "outlook.office365.com"
tls = "tls"
user = "you@outlook.com"
oauth = "microsoft"
```

Sign in with `orion account login outlook`: open `microsoft.com/devicelogin`,
enter the code, approve it. IMAP and SMTP then use XOAUTH2 and the token
refreshes by itself.

### Gmail, iCloud, Yahoo, Fastmail

They need an **app password** (your normal password is refused);
`orion account add` opens the page where you create one.

## JMAP

```toml
[[account]]
name = "fastmail"
backend = "jmap"
url = "https://api.fastmail.com/jmap/session"   # or just https://host
auth = "bearer"                                   # "basic" needs `user`
password_command = "pass show mail/fastmail-token"
```

JMAP accounts send through JMAP (no SMTP section needed).

## POP3

```toml
[[account]]
name = "isp"
backend = "pop3"
host = "pop.example.net"
tls = "tls"
user = "me"
password_command = "pass show mail/isp"
```

POP3 keeps read/flagged marks in Orion's cache (the protocol has none). `d`
deletes on the server; there is no Trash.

## mbox

```sh
orion --mbox ~/Mail/archive.mbox
```

## Passwords

Orion never stores a password in its configuration. Either:

- `password_command = "…"` — any command that prints it (`pass`, `secret-tool`,
  `gpg -d`…). `orion account add` stores it in the system keyring and writes
  the matching `secret-tool` command.
- `oauth = "microsoft"` for Outlook.
- `password = "…"` exists for testing only.

## Your own server

For a domain of your own, `orion account add` looks for:

1. its autoconfig file, Thunderbird's format: `https://autoconfig.<domain>/mail/config-v1.1.xml`
   or `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`;
2. DNS SRV records: `_imaps._tcp`, `_submissions._tcp`, `_imap._tcp`,
   `_submission._tcp`, `_pop3s._tcp`.

Servers outside your domain must be confirmed, unless DNSSEC validated them
on this machine. If a server's certificate is valid for another name of your
domain (DNS says `example.org`, the certificate `mail.example.org`), Orion
offers to use that name.

Recommended SRV records:

```
_imaps._tcp.example.org.        SRV 10 1 993 mail.example.org.
_submissions._tcp.example.org.  SRV 10 1 465 mail.example.org.
_imap._tcp.example.org.         SRV  0 0   0 .      ; not offered
_submission._tcp.example.org.   SRV  0 0   0 .      ; not offered
```

The target must be a name the certificate covers, and `.` (a lone dot)
means "not offered".
