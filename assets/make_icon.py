"""Generates assets/app.ico: the Claude Usage Widget app icon.

A charcoal rounded square holding a six-spoke orange asterisk inside a teal progress
ring (with a dim track for the unfilled part), plus a faint glow. Every size is drawn
on its own, 16x supersampled, with thicker strokes and no glow at the small sizes, so
16/24/32 px stay crisp instead of being shrunk from 256.

Run:  .venv\\Scripts\\python.exe assets\\make_icon.py   (also writes app_preview.png)
"""
import math
import os
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
SS = 16  # supersampling factor

BG_TOP, BG_BOTTOM = (0x1C, 0x1C, 0x22), (0x0B, 0x0B, 0x0E)   # around #111114
TEAL = (0x35, 0xD6, 0xB0)
ORANGE_TOP, ORANGE_BOTTOM = (0xF2, 0x8F, 0x55), (0xDC, 0x62, 0x2E)


def params(size):
    """Proportions per size: small icons get bolder strokes and less detail."""
    # keep a clear gap between the spoke tips (spoke + spoke_w/2) and the ring's inner edge
    # (ring_r - ring_w) at every size
    if size <= 20:
        return dict(pad=0.02, radius=0.20, ring_r=0.40, ring_w=0.12, spoke=0.155, spoke_w=0.12, glow=0, track=0.30, border=0)
    if size <= 32:
        return dict(pad=0.03, radius=0.21, ring_r=0.38, ring_w=0.10, spoke=0.165, spoke_w=0.095, glow=0, track=0.26, border=0.0)
    if size <= 48:
        return dict(pad=0.04, radius=0.22, ring_r=0.34, ring_w=0.085, spoke=0.175, spoke_w=0.075, glow=0.5, track=0.22, border=0.012)
    return dict(pad=0.055, radius=0.225, ring_r=0.30, ring_w=0.068, spoke=0.18, spoke_w=0.06, glow=1.0, track=0.18, border=0.008)


def vgradient(w, h, top, bottom):
    g = Image.new("RGB", (1, 256))
    for y in range(256):
        t = y / 255
        g.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return g.resize((w, h), Image.BILINEAR)


def capped_arc(draw, c, r, w, start, end, fill):
    """Arc from `start` to `end` degrees (0 = 12 o'clock, clockwise) with round caps."""
    box = (c - r, c - r, c + r, c + r)
    draw.arc(box, start - 90, end - 90, fill=fill, width=round(w))
    for a in (start, end):
        rad = math.radians(a)
        x, y = c + (r - w / 2) * math.sin(rad), c - (r - w / 2) * math.cos(rad)
        draw.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=fill)


def render(size):
    p = params(size)
    S = size * SS
    c = S / 2
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # rounded-square container with a subtle vertical gradient
    pad = round(S * p["pad"])
    box = (pad, pad, S - pad - 1, S - pad - 1)
    rad = round((S - 2 * pad) * p["radius"])
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, rad, fill=255)
    bg = vgradient(S, S, BG_TOP, BG_BOTTOM).convert("RGBA")
    out.paste(bg, (0, 0), mask)

    # the ring: dim track + teal progress arc (~300°) leaving an intentional gap
    rr, rw = S * p["ring_r"], S * p["ring_w"]
    ring = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    rd = ImageDraw.Draw(ring)
    rd.ellipse((c - rr, c - rr, c + rr, c + rr), outline=TEAL + (round(255 * p["track"]),), width=round(rw))
    arc = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    capped_arc(ImageDraw.Draw(arc), c, rr, rw, 0, 300, TEAL + (255,))

    # six-spoke asterisk with round ends and a top-to-bottom orange gradient
    spokes = Image.new("L", (S, S), 0)
    sd = ImageDraw.Draw(spokes)
    L, sw = S * p["spoke"], S * p["spoke_w"]
    for ang in (0, 60, 120):
        a = math.radians(ang)
        dx, dy = L * math.sin(a), -L * math.cos(a)
        sd.line((c - dx, c - dy, c + dx, c + dy), fill=255, width=round(sw))
        for sx, sy in ((c - dx, c - dy), (c + dx, c + dy)):
            sd.ellipse((sx - sw / 2, sy - sw / 2, sx + sw / 2, sy + sw / 2), fill=255)
    sd.ellipse((c - sw * 0.75, c - sw * 0.75, c + sw * 0.75, c + sw * 0.75), fill=255)  # clean hub
    orange = vgradient(S, S, ORANGE_TOP, ORANGE_BOTTOM).convert("RGBA")
    star = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    star.paste(orange, (0, 0), spokes)

    # faint glow (large sizes only), kept inside the container
    if p["glow"]:
        glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        glow.alpha_composite(arc)
        glow.alpha_composite(star)
        glow = glow.filter(ImageFilter.GaussianBlur(S * 0.035))
        a = glow.getchannel("A").point(lambda v: int(v * 0.42 * p["glow"]))
        glow.putalpha(ImageChops.multiply(a, mask))
        out.alpha_composite(glow)

    out.alpha_composite(ring)
    out.alpha_composite(arc)
    out.alpha_composite(star)

    # hairline highlight so the dark tile reads on a dark taskbar
    if p["border"]:
        hl = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        ImageDraw.Draw(hl).rounded_rectangle(box, rad, outline=(255, 255, 255, 30), width=max(1, round(S * p["border"])))
        out.alpha_composite(hl)
    return out.resize((size, size), Image.LANCZOS)


def main():
    images = {s: render(s) for s in SIZES}
    ico = os.path.join(HERE, "app.ico")
    big = images[256]
    big.save(ico, format="ICO", sizes=[(s, s) for s in SIZES],
             append_images=[images[s] for s in SIZES if s != 256])
    print("wrote", ico, "sizes", SIZES)
    if len(sys.argv) > 1:
        # preview: 256 px plus the small sizes at 1:1 and 4x, on dark and on light
        prev = Image.new("RGBA", (1100, 560), (32, 32, 36, 255))
        prev.paste(Image.new("RGBA", (550, 560), (236, 237, 241, 255)), (550, 0))
        for x0 in (0, 550):
            prev.alpha_composite(images[256], (x0 + 30, 30))
            x = x0 + 320
            for s in (48, 32, 24, 16):
                prev.alpha_composite(images[s], (x, 40))
                x += s + 18
            x = x0 + 30
            for s in (48, 32, 24, 16):
                prev.alpha_composite(images[s].resize((s * 4, s * 4), Image.NEAREST), (x, 300))
                x += s * 4 + 16
        prev.save(sys.argv[1])
        print("preview", sys.argv[1])


if __name__ == "__main__":
    main()
