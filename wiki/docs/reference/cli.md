# Command line

## orion

```
A fast terminal mail client: IMAP, POP3, JMAP and mbox, native HTML and images, OpenPGP with YubiKey

Usage: orion [OPTIONS] [COMMAND]

Commands:
  daemon    Run in the background: sync, detect new mail, notify and serve the local API
  status    Show unread counts from the running daemon
  watch     Print daemon events (new mail, counts, errors) as they happen
  sync      Ask the daemon to check for new mail now
  contacts  Search the address book ([contacts] sources): "Name <email>" lines
  account   Add, sign in, test and list accounts
  help      Print this message or the help of the given subcommand(s)

Options:
  -c, --config <CONFIG>  Configuration file (default: ~/.config/orion/config.toml)
  -m, --mbox <MBOX>      Open an mbox file (or a directory) directly, without a configuration
      --cache <CACHE>    SQLite cache path (default: ~/.cache/orion/cache.sqlite)
      --no-cache         Do not use the local cache
      --print-config     Print an example configuration and exit
      --print-keys       Print the default key bindings (a Markdown table) and exit
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
  -V, --version          Print version
```

## orion account

```
Add, sign in, test and list accounts

Usage: orion account [OPTIONS] <COMMAND>

Commands:
  add    Add an account: detects the provider, opens the app-password page or signs in with Microsoft, tests the connection and saves it
  login  Sign in again (expired Microsoft sign-in, new app password)
  test   Connect to the accounts and report mailbox counts
  list   List the configured accounts
  help   Print this message or the help of the given subcommand(s)

Options:
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
```

## orion status

```
Show unread counts from the running daemon

Usage: orion status [OPTIONS]

Options:
      --json             Machine-readable JSON
      --waybar           Output for a Waybar custom module ({"text","tooltip","class"})
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
```

## orion watch

```
Print daemon events (new mail, counts, errors) as they happen

Usage: orion watch [OPTIONS]

Options:
      --json             One JSON object per line
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
```

## orion sync

```
Ask the daemon to check for new mail now

Usage: orion sync [OPTIONS]

Options:
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
```

## orion daemon

```
Run in the background: sync, detect new mail, notify and serve the local API

Usage: orion daemon [OPTIONS]

Options:
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
```

## orion contacts

```
Search the address book ([contacts] sources): "Name <email>" lines

Usage: orion contacts [OPTIONS] [QUERY]

Arguments:
  [QUERY]  Text to look for (in names and addresses); all contacts if omitted

Options:
      --socket <SOCKET>  Daemon socket (default: $XDG_RUNTIME_DIR/orion.sock, or [daemon] socket)
  -h, --help             Print help
```

