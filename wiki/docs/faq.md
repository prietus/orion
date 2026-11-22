# FAQ

### Chinese, Japanese or Korean text shows as boxes

Install a CJK font; the terminal and Chromium pick it up by themselves:

```sh
sudo pacman -S noto-fonts-cjk
```

Restart the terminal afterwards.

### Icons show as boxes or question marks

Install a Nerd Font, or just the symbols (`ttf-nerd-fonts-symbols` on Arch), or
switch icons off:

```toml
[ui]
icons = false
```

### Touching my YubiKey opens a new message

That was the YubiKey typing a one-time code (OTP), whose first letter `c`
means *write*. Orion now recognises those bursts and ignores them. To stop the
key from typing codes in every program: `ykman config usb --disable OTP`.

### Images leave garbage when scrolling in foot

foot uses sixel: images are pixels painted over the text, and every scroll
step means redrawing them. Orion repaints the screen in one synchronized
update and batches fast key repeats. kitty-protocol terminals (kitty,
ghostty, WezTerm) scroll images more smoothly.

### "cache: … has no column named …"

An older Orion — usually a daemon started before you updated — rebuilt the
cache in its own format. Restart the daemon so both are the same version:

```sh
pkill -x orion; orion daemon &
```

Current versions repair such a cache when they open it.

### My first sync is slow

The first time, Orion downloads the headers of every folder you open (and of
Sent, for conversations): about a minute for 8,000 messages on Outlook,
which also throttles fast clients. Afterwards only changes are fetched, and
Orion starts instantly from its cache.

### "invalid signer" / "the connected YubiKey holds the key of …"

The card plugged in does not hold the key of your From address. Plug in the
right card, or set `pgp_key` in that account to the key you want to sign
with.

### Can I click links in the original view?

It is an image, but every link carries its number in a small badge: `o`,
the number and `Enter` opens it.

### Where are my drafts?

In `~/.local/share/orion/drafts/`, until the message is sent.

### Does Orion mark everything read when I move through the list?

No: a previewed message counts as read after 2 seconds on screen
(`mark_read_after`). `Enter` marks it at once.
