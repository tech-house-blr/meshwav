#!/usr/bin/env python3
"""Renders the Meshwav icon PNGs from the same geometry as assets/icon.svg and the Android vector.
Run from the repo root: python3 assets/make_icons.py (needs Pillow)."""
from PIL import Image, ImageDraw

BG = (0x14, 0x16, 0x18, 255)
FG = (0xC8, 0xFF, 0x2E, 255)
# The mark on a 108 unit canvas (the Android adaptive icon grid): an M drawn as a waveform.
PTS = [(32, 72), (32, 38), (54, 62), (76, 38), (76, 72)]
STROKE = 9


def render(size, background=True, rounded=True, scale=0.78):
    ss = 4
    n = size * ss
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if background:
        d.rounded_rectangle([0, 0, n - 1, n - 1], radius=int(n * 0.22) if rounded else 0, fill=BG)
    # centre the 108 canvas, shrunk so the mark keeps clear of the edges
    k = n / 108 * scale
    off = n * (1 - scale) / 2
    pts = [(off + x * k, off + y * k) for x, y in PTS]
    w = STROKE * k
    d.line(pts, fill=FG, width=int(w), joint="curve")
    r = w / 2
    for x, y in (pts[0], pts[-1]):
        d.ellipse([x - r, y - r, x + r, y + r], fill=FG)
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    render(256).save("crates/linux-receiver-gui/assets/icon-256.png")
    render(64, background=False, scale=1.0).save("crates/linux-receiver-gui/assets/tray-64.png")
    for s in (16, 32, 48, 64):
        render(s).save(f"/tmp/preview-{s}.png")
