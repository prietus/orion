# Keys

The default key bindings, as printed by `orion --print-keys`. Press `?` in
Orion to see them, and rebind any of them in the configuration file:

```toml
[keys]
delete = ["d", "delete"]
quit = ["q", "ctrl-q"]
```

| Keys | Action | Name in `[keys]` |
|---|---|---|
| `q` | quit (in the reader: go back) | `quit` |
| `ctrl-c` | quit immediately | `force_quit` |
| `j` `down` | move down | `down` |
| `k` `up` | move up | `up` |
| `g` `home` | go to top | `top` |
| `G` `end` | go to bottom | `bottom` |
| `ctrl-d` `pgdown` `space` | page down | `page_down` |
| `ctrl-u` `pgup` | page up | `page_up` |
| `enter` `l` `right` | open | `open` |
| `esc` `h` `left` | back / left pane | `back` |
| `tab` | switch pane | `focus_next` |
| `n` `J` | next message | `next_message` |
| `p` `K` | previous message | `prev_message` |
| `d` `delete` | move to the Trash | `delete` |
| `D` | delete for good (asks first) | `delete_forever` |
| `m` | move to another folder | `move` |
| `X` | empty the Trash or Spam folder (asks first) | `empty_folder` |
| `s` | save or open the attachments | `attachments` |
| `t` | expand / collapse the conversation | `toggle_thread` |
| `T` | group the list into conversations (on/off) | `threads` |
| `N` | mark read / unread | `toggle_read` |
| `F` | flag / unflag as important | `toggle_flag` |
| `v` | toggle HTML / text | `toggle_view` |
| `V` | show the HTML as designed (drawn by Chromium) | `original_view` |
| `I` | import the sender's key (to reply signed/encrypted) | `import_key` |
| `Y` | YubiKey / OpenPGP card status | `card_status` |
| `o` | open link (o + number + enter) | `open_link` |
| `H` | full headers | `toggle_headers` |
| `i` | show/hide images | `toggle_images` |
| `L` | download this message's remote images (the sender learns you opened it) | `load_remote_images` |
| `]` | next image | `next_image` |
| `[` | previous image | `prev_image` |
| `/` | filter by subject/sender | `search` |
| `ctrl-r` `f5` | reload | `reload` |
| `c` | write a new message | `compose` |
| `r` | reply | `reply` |
| `R` | reply to all | `reply_all` |
| `f` | forward | `forward` |
| `?` | help | `help` |

The compose panel, the file browser and the attachment list have their own
keys: see [Writing mail](../writing.md) and [Reading mail](../reading.md).
