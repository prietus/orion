# HTML and images

## The text rendering

Orion lays out HTML itself (no w3m or lynx) for the terminal grid:

- CSS from `<style>` blocks (selectors with specificity, `!important`) and
  inline `style=`; legacy attributes (`bgcolor`, `<font color>`, `align`).
- Tables with automatic column widths, like a browser: newsletter layouts are
  placed side by side when they fit; data tables get separators.
- Paragraphs, headings, nested lists, quotes, `pre`, rules.
- Hidden text typical of newsletters (`display:none` preheaders, `max-height:0`…)
  is dropped. Email templates that use `font-size:0` on column wrappers are
  handled.
- Colours: `[ui] html_colors = "auto"` keeps the colours that are readable on
  your terminal's background and drops backgrounds; `"full"` keeps them all,
  `"none"` drops them.

`v` toggles between the HTML and the plain-text version of a message.

## The original view

A terminal grid cannot show a newsletter exactly as designed — fonts, sizes,
pixel positions. **`V`** draws the HTML with headless Chromium into an image
and shows it with sixel/kitty graphics. `V` again goes back.

- It is an **image**: text cannot be selected, but every link carries its
  number in a small badge where it is, so `o` + number opens it, just like in
  the text view (`[ui] link_markers = false` hides the badges).
- It needs `chromium` (or set `[ui] html_renderer` to another browser).
- It is **offline and locked down**: scripts are removed, a
  Content-Security-Policy forbids scripts and every network load, Chromium
  runs with a dead proxy and a throwaway profile. Only images embedded in the
  message, and remote ones you loaded with `L`, are shown.

## Images

Images attached to a message (or embedded in its HTML) are shown inline with
the terminal's graphics protocol: kitty, sixel or iTerm2, halfblocks
elsewhere. `i` shows or hides the image pane; `[` and `]` move between images.

### Remote images

Images hosted on the internet are **not downloaded by default**: fetching
them tells the sender you opened the message, when and from where. The
message says how many there are:

```
Images  17 on the internet, not loaded · L: load (the sender will know you opened it)
```

`L` downloads them for that message only — no cookies, no Referer, only
`image/*` answers, size limits, and 1×1 tracking pixels skipped.
`[ui] remote_images = "always"` loads them every time, `"never"` disables `L`.

## Terminals

| Terminal | Protocol | Notes |
|---|---|---|
| kitty, ghostty, WezTerm | kitty | the smoothest scrolling |
| foot | sixel | each scroll step redraws the image; Orion batches the steps |
| xterm | sixel | needs sixel enabled |
| others | halfblocks | coloured characters, always works |

`[ui] image_protocol` forces one (`"auto"`, `"kitty"`, `"sixel"`, `"iterm2"`,
`"halfblocks"`, `"none"`).
