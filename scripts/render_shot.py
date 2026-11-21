#!/usr/bin/env python3
"""Renders a screen dumped by the `screenshots` test (JSON cells) as a PNG,
with JetBrains Mono for text and Symbols Nerd Font for icons.
Usage: render_shot.py screen.json out.png"""
import json, sys
from PIL import Image, ImageDraw, ImageFont

BG, FG = "#1e1e2e", "#cdd6f4"   # Catppuccin Mocha base/text (the terminal's colours)
SIZE = 15
mono = ImageFont.truetype("/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf", SIZE)
bold = ImageFont.truetype("/usr/share/fonts/TTF/JetBrainsMono-Bold.ttf", SIZE)
icons = ImageFont.truetype("/usr/share/fonts/TTF/SymbolsNerdFont-Regular.ttf", SIZE)
cw = round(mono.getlength("M"))
ch = SIZE + 7

rows = json.load(open(sys.argv[1]))
img = Image.new("RGB", (cw * len(rows[0]) + 16, ch * len(rows) + 16), BG)
d = ImageDraw.Draw(img)
for y, row in enumerate(rows):
    for x, (sym, fg, bg, b, dim, strike) in enumerate(row):
        px, py = 8 + x * cw, 8 + y * ch
        if bg and bg.startswith("#"):
            d.rectangle([px, py, px + cw, py + ch], fill=bg)
        if sym.strip():
            cp = ord(sym[0])
            font = icons if 0xE000 <= cp <= 0xF8FF or cp >= 0xF0000 else (bold if b else mono)
            color = fg if fg and fg.startswith("#") else FG
            if dim:
                color = "#6c7086"
            # Box-drawing characters are stretched to the full cell height.
            if 0x2500 <= cp <= 0x257F:
                d.text((px, py + ch / 2), sym, font=font, fill=color, anchor="lm")
            else:
                d.text((px, py + 3), sym, font=font, fill=color)
            if strike:
                d.line([px, py + ch // 2, px + cw, py + ch // 2], fill=color)
img.save(sys.argv[2])
