# OpenPGP and YubiKey

Orion uses your **GnuPG** setup (through `gpg --status-fd`), so it works with
the keys you already have — on disk or on a **YubiKey**, Nitrokey or any
OpenPGP card through `scdaemon`.

## Reading

- PGP/MIME (RFC 3156) signed and encrypted messages, protected subjects, and
  inline PGP (`BEGIN PGP MESSAGE`).
- When an encrypted message is opened, Orion tries to decrypt it at once. If
  the card's PIN is cached (a YubiKey keeps it while plugged in), it just
  works; otherwise Orion asks for the PIN **in its own status bar**, masked,
  and hands it to gpg — no pinentry window breaking the terminal.
- In the three-pane preview, encrypted messages do not ask for the PIN;
  `Enter` into the message does.
- Signatures show as valid / tampered / unknown key / expired / revoked, with
  the trust level.
- The decrypted text only lives in memory: the cache keeps the encrypted
  message.

### Clear errors

Orion explains what went wrong instead of a bare "decryption failed":

- wrong PIN (with the attempts left), blocked PIN;
- the card is not plugged in — "plug in YubiKey #12345678 and press Enter";
- **the wrong card is plugged in** — which key the message needs, on which
  card, and whose key the connected card holds;
- touch not confirmed; no secret key for the message.

`Y` shows the connected card: holder, fingerprints, PIN attempts left.

## Writing

`Ctrl-G` signs, `Ctrl-K` encrypts (`S` / `E` on the review screen).

- Signed messages are PGP/MIME `multipart/signed`, with the `micalg` gpg
  really used.
- Encrypted messages are also encrypted to **you**, so your copy in Sent stays
  readable, and their subject is protected (`...` outside).
- `[pgp] sign_by_default = true` signs everything; `encrypt_when_possible =
  true` encrypts when every recipient has a key.
- Replying to an encrypted message starts with encryption on.
- `pgp_key` (per account) signs with another key than the one of your From
  address.

### Recipients without a usable key

- **No key**: Orion names them. `Ctrl-L` (`k`) looks them up over WKD and the
  `[pgp] keyservers` (default `hkps://keys.openpgp.org`).
- **Found, not verified**: Orion shows each fingerprint. Compare it with its
  owner, then `Ctrl-Y` (`A`) accepts it. Accepted keys are remembered in
  `~/.local/share/orion/accepted-keys`, and messages are encrypted to that
  exact fingerprint.

## Learning keys from mail

When a message brings its sender's key — an **Autocrypt** header, an attached
key, or just the fingerprint of an unknown signature (then fetched from a
keyserver) — the message shows it with its fingerprint:

```
Key  sender's key available (Autocrypt): 69D7 CB9A … · Ana García · I: import and use it for replies
```

`I` imports it and accepts it for that address — only if the key really
carries the address. Nothing is ever imported automatically: a forged From
could otherwise plant a key.

Orion also sends **your** key in an `Autocrypt:` header (`[pgp] autocrypt =
true`), so other clients can answer you encrypted.

## YubiKey tips

- **Touching the key types a code?** That is the YubiKey's OTP feature. Orion
  recognises those bursts and ignores them, but you can switch OTP off for good:
  `ykman config usb --disable OTP`.
- The PIN stays unlocked while the key is plugged in. To lock it again without
  unplugging: `gpgconf --reload scdaemon`.
- To require a touch for every decryption: `ykman openpgp keys set-touch dec on`.
- First time on a new machine: `gpg --card-status` (or just using the card in
  Orion) links the card's keys to your public key.
