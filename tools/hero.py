# Usage: python3 -m venv .venv && .venv/bin/pip install pillow==12.* fonttools
#        cargo test -- --ignored export_hero_screen
#        .venv/bin/python -I tools/hero.py target/hero docs/hero.png FONTS_DIR
# FONTS_DIR holds JetBrainsMono-Regular.ttf, JetBrainsMono-Bold.ttf (Ghostty's
# default font) and SymbolsNerdFontMono-Regular.ttf (its built-in icons).
"""Draws one frame of `tuitube --demo`, as exported by the
`export_hero_screen` test, the way Ghostty shows it: every cell's character
in JetBrains Mono, box lines and blocks drawn as Ghostty draws them, and the
thumbnails and channel photos where the UI put them. The thumbnails are the
demo's own pictures, with a title over them as YouTube thumbnails have."""

import json
import os
import sys

from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFilter, ImageFont

SCREEN, OUT, FONTS = sys.argv[1], sys.argv[2], sys.argv[3]
PAD = 28
RADIUS = 26

screen = json.load(open(os.path.join(SCREEN, "screen.json")))
COLS, ROWS = screen["cols"], screen["rows"]
CW, CH = screen["cell"]
BG, FG = screen["bg"], screen["fg"]

regular = ImageFont.truetype(os.path.join(FONTS, "JetBrainsMono-Regular.ttf"), 26.6)
bold = ImageFont.truetype(os.path.join(FONTS, "JetBrainsMono-Bold.ttf"), 26.6)
icons = ImageFont.truetype(os.path.join(FONTS, "SymbolsNerdFontMono-Regular.ttf"), 26.6)
covered = set(TTFont(os.path.join(FONTS, "JetBrainsMono-Regular.ttf")).getBestCmap())
icon_cmap = set(TTFont(os.path.join(FONTS, "SymbolsNerdFontMono-Regular.ttf")).getBestCmap())
# What neither has (♪) comes from a system font, as Ghostty falls back.
FALLBACK = "/System/Library/Fonts/Apple Symbols.ttf"
fallback = ImageFont.truetype(FALLBACK, 30)
ascent, descent = regular.getmetrics()
BASELINE = (CH - (ascent + descent)) // 2 + ascent

width, height = COLS * CW + 2 * PAD, ROWS * CH + 2 * PAD
canvas = Image.new("RGB", (width, height), BG)
draw = ImageDraw.Draw(canvas)


def cell_box(x, y):
    left, top = PAD + x * CW, PAD + y * CH
    return left, top, left + CW, top + CH


def line_shape(ch, x, y, color):
    """Box lines and blocks, drawn to the cell's edges as Ghostty does, so
    they join up; True if `ch` is one."""
    left, top, right, bottom = cell_box(x, y)
    mx, my = left + CW // 2, top + CH // 2
    thin = 2

    class Pen:
        """Skips the empty bits of a corner (an arc that already reaches the
        cell's edge)."""

        @staticmethod
        def rectangle(box, fill):
            if box[2] >= box[0] and box[3] >= box[1]:
                draw.rectangle(box, fill=fill)

        arc = staticmethod(draw.arc)

    pen = Pen
    if ch == "─":
        pen.rectangle([left, my - 1, right - 1, my], fill=color)
    elif ch == "━":
        pen.rectangle([left, my - 2, right - 1, my + 1], fill=color)
    elif ch == "│":
        pen.rectangle([mx - 1, top, mx, bottom - 1], fill=color)
    elif ch in "╭╮╰╯":
        r = CW // 2
        # The arc's center, and the straight bits from its ends to the
        # cell's edges.
        cx = mx + r if ch in "╭╰" else mx - r
        cy = my + r if ch in "╭╮" else my - r
        box = [cx - r, cy - r, cx + r, cy + r]
        start = {"╭": 180, "╮": 270, "╰": 90, "╯": 0}[ch]
        pen.arc(box, start, start + 90, fill=color, width=thin)
        if ch in "╭╮":
            pen.rectangle([mx - 1, cy, mx, bottom - 1], fill=color)
        else:
            pen.rectangle([mx - 1, top, mx, cy], fill=color)
        if ch in "╭╰":
            pen.rectangle([cx, my - 1, right - 1, my], fill=color)
        else:
            pen.rectangle([left, my - 1, cx, my], fill=color)
    elif ch == "▀":
        pen.rectangle([left, top, right - 1, top + CH // 2 - 1], fill=color)
    elif ch == "▄":
        pen.rectangle([left, top + CH // 2, right - 1, bottom - 1], fill=color)
    elif ch == "▌":
        pen.rectangle([left, top, left + CW // 2 - 1, bottom - 1], fill=color)
    elif ch == "▏":
        pen.rectangle([left, top, left + 1, bottom - 1], fill=color)
    elif ch == "█":
        pen.rectangle([left, top, right - 1, bottom - 1], fill=color)
    else:
        return False
    return True


def draw_background(x, y, cell):
    bg = cell[2]
    left, top, right, bottom = cell_box(x, y)
    if bg:
        draw.rectangle([left, top, right - 1, bottom - 1], fill=bg)


def blank(x, y):
    return x >= COLS or cells[y * COLS + x][0] in ("", " ")


def draw_icon(x, y, ch, color):
    """A Nerd Font icon, as Ghostty draws one: as big as a cell's height
    allows, over two cells when the next one is empty, centered."""
    left, top, _, _ = cell_box(x, y)
    room = 2 * CW if blank(x + 1, y) else CW
    size = 26.6
    font = ImageFont.truetype(os.path.join(FONTS, "SymbolsNerdFontMono-Regular.ttf"), size)
    l, t, r, b = font.getbbox(ch)
    scale = min(room * 0.78 / max(r - l, 1), CH * 0.6 / max(b - t, 1))
    font = ImageFont.truetype(os.path.join(FONTS, "SymbolsNerdFontMono-Regular.ttf"), size * scale)
    l, t, r, b = font.getbbox(ch)
    gx = left + (room - (r - l)) / 2 - l
    gy = top + (CH - (b - t)) / 2 - t
    draw.text((gx, gy), ch, font=font, fill=color)


def draw_glyph(x, y, cell):
    ch, fg, _, heavy = cell
    if ch in ("", " "):
        return
    left, top, _, _ = cell_box(x, y)
    color = fg or FG
    if line_shape(ch, x, y, color):
        return
    point = ord(ch[0])
    if 0xE000 <= point <= 0xF8FF or point >= 0xF0000:
        draw_icon(x, y, ch, color)
        return
    font = bold if heavy else regular
    if point not in covered:
        font = icons if point in icon_cmap else fallback
    draw.text((left, top + BASELINE), ch, font=font, fill=color, anchor="ls")


def draw_cell(x, y, cell):
    draw_background(x, y, cell)
    draw_glyph(x, y, cell)


def rect_px(r):
    x, y, w, h = r
    return PAD + x * CW, PAD + y * CH, w * CW, h * CH


# Titles over the thumbnails, as YouTube thumbnails have them: the words,
# their color, and which side they go on.
TITLES = {
    "demoVideo00": (["FAST", "NOW?"], "#ffd23f", "right"),
    "demoVideo01": (["10 MIN", "FOCACCIA"], "#ffffff", "left"),
    "demoVideo03": (["1 + 1", "= ?"], "#ffffff", "left"),
    "demoVideo04": (["5 AM", "SUMMIT"], "#ffffff", "left"),
    "demoVideo05": (["42 KEYS"], "#ffffff", "top"),
    "demoVideo06": (["hjkl"], "#7ee787", "right"),
    "demoVideo07": (["WHY IT", "WIGGLES"], "#ffffff", "left"),
    "demoVideo08": (["30 M!"], "#ff4d4d", "left"),
}
IMPACT = "/System/Library/Fonts/Supplemental/Impact.ttf"


def thumbnail(video_id):
    art = Image.open(os.path.join(SCREEN, f"{video_id}.png")).convert("RGB")
    art = art.resize((1280, 720), Image.LANCZOS)
    title = TITLES.get(video_id)
    if not title:
        return art
    words, color, side = title
    pen = ImageDraw.Draw(art)
    size = 190 if len(words) > 1 else 220
    font = ImageFont.truetype(IMPACT, size)
    heights = [pen.textbbox((0, 0), w, font=font)[3] for w in words]
    total = sum(heights) - 30 * (len(words) - 1)
    y = (720 - total) // 2 if side != "top" else 40
    for word, h in zip(words, heights):
        w = pen.textlength(word, font=font)
        x = {"left": 60, "right": 1280 - 60 - w, "top": (1280 - w) / 2}[side]
        # A drop shadow, then the words with a thick dark edge.
        shadow = Image.new("RGBA", art.size, (0, 0, 0, 0))
        ImageDraw.Draw(shadow).text((x + 10, y + 12), word, font=font, fill=(0, 0, 0, 150))
        art.paste(shadow.filter(ImageFilter.GaussianBlur(8)), (0, 0), shadow.filter(ImageFilter.GaussianBlur(8)))
        pen.text((x, y), word, font=font, fill=color, stroke_width=12, stroke_fill="#111111")
        y += h - 30
    return art


AVATAR_COLORS = [
    ("#f38ba8", "#fab387"),
    ("#89b4fa", "#b4befe"),
    ("#a6e3a1", "#94e2d5"),
    ("#cba6f7", "#f5c2e7"),
    ("#f9e2af", "#fab387"),
    ("#74c7ec", "#89dceb"),
    ("#eba0ac", "#cba6f7"),
    ("#94e2d5", "#a6e3a1"),
]


def avatar(name, size):
    """A channel photo: a gradient circle with the name's initials."""
    a, b = AVATAR_COLORS[sum(map(ord, name)) % len(AVATAR_COLORS)]
    big = size * 4
    grad = Image.new("RGB", (big, big))
    ca = tuple(int(a[i : i + 2], 16) for i in (1, 3, 5))
    cb = tuple(int(b[i : i + 2], 16) for i in (1, 3, 5))
    px = grad.load()
    for yy in range(big):
        for xx in range(big):
            t = (xx + yy) / (2 * big)
            px[xx, yy] = tuple(round(ca[i] + (cb[i] - ca[i]) * t) for i in range(3))
    words = [w for w in name.replace("&", " ").split() if w[0].isalnum()]
    initials = "".join(w[0] for w in words[:2]).upper()
    font = ImageFont.truetype("/System/Library/Fonts/Supplemental/Arial Black.ttf", int(big * 0.36))
    pen = ImageDraw.Draw(grad)
    pen.text((big / 2, big / 2), initials, font=font, fill="#1e1e2e", anchor="mm")
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).ellipse([0, 0, big - 1, big - 1], fill=255)
    out = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    out.paste(grad, (0, 0), mask)
    return out.resize((size, size), Image.LANCZOS)


def fill(image, w, h):
    """`image` cropped from its middle to fill w×h, as tuitube does."""
    scale = max(w / image.width, h / image.height)
    resized = image.resize((round(image.width * scale), round(image.height * scale)), Image.LANCZOS)
    left, top = (resized.width - w) // 2, (resized.height - h) // 2
    return resized.crop((left, top, left + w, top + h))


in_image = set()
for image in screen["images"]:
    x, y, w, h = image["shown"]
    in_image.update((cx, cy) for cx in range(x, x + w) for cy in range(y, y + h))

cells = screen["cells"]
# Backgrounds first, then characters, so an icon wider than its cell isn't
# painted over by the next cell's background.
for y in range(ROWS):
    for x in range(COLS):
        if (x, y) not in in_image:
            draw_background(x, y, cells[y * COLS + x])
for y in range(ROWS):
    for x in range(COLS):
        if (x, y) not in in_image:
            draw_glyph(x, y, cells[y * COLS + x])

for image in screen["images"]:
    ax, ay, aw, ah = rect_px(image["area"])
    sx, sy, sw, sh = rect_px(image["shown"])
    if image["kind"] == "thumbnail":
        picture = fill(thumbnail(image["id"]), aw, ah)
        canvas.paste(picture.crop((0, 0, sw, sh)), (sx, sy))
    else:
        size = min(aw, ah)
        photo = avatar(image["name"], size)
        canvas.paste(photo, (ax + (aw - size) // 2, ay + (ah - size) // 2), photo)

# What the UI drew over the pictures: the length badges.
badge = [(x, y) for x, y in sorted(in_image) if cells[y * COLS + x][0] not in ("", " ")]
for x, y in badge:
    draw_background(x, y, cells[y * COLS + x])
for x, y in badge:
    draw_glyph(x, y, cells[y * COLS + x])

# Rounded corners, like a window.
mask = Image.new("L", canvas.size, 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, width - 1, height - 1], RADIUS, fill=255)
out = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
out.paste(canvas, (0, 0), mask)
os.makedirs(os.path.dirname(OUT) or ".", exist_ok=True)
out.save(OUT, optimize=True)
print(f"wrote {OUT} ({width}×{height})")
