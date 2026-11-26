"""Generates assets/app.ico: the Claude Usage Widget app icon.

Light design: a soft white rounded tile holding an orange sunburst inside a light-grey
ring that's partly filled in orange (a usage meter). Every size is drawn on its own,
16x supersampled: large sizes get the layered tile and a 12-spoke sunburst; small sizes
get one tile, bolder strokes and fewer spokes, so 16/24/32 px stay crisp.

Run:  .venv\\Scripts\\python.exe assets\\make_icon.py [preview.png]
"""
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
SS = 16  # supersampling factor

OUTER = (0xF1, 0xF1, 0xF2)       # outer tile
INNER = (0xFC, 0xFC, 0xFC)       # inner tile
TRACK = (0xE8, 0xE8, 0xE9)       # unfilled ring
EDGE = (0xD6, 0xD6, 0xD9)        # hairline so the tile reads on light backgrounds
ORANGE = (0xE5, 0x7E, 0x4B)
ARC_DEG = 145                    # filled part of the ring, from 12 o'clock clockwise

# 12 spokes with slightly varied lengths, for a hand-drawn sunburst feel
SPOKES_12 = [1.00, 0.86, 0.96, 0.82, 0.98, 0.88, 1.00, 0.84, 0.95, 0.83, 0.97, 0.87]


def params(size):
    if size <= 20:
        return dict(layered=False, pad=0.03, radius=0.22, ring_r=0.40, ring_w=0.13, spokes=6, spoke=0.17, spoke_w=0.12, edge=0.05)
    if size <= 32:
        return dict(layered=False, pad=0.04, radius=0.22, ring_r=0.37, ring_w=0.105, spokes=8, spoke=0.175, spoke_w=0.085, edge=0.035)
    if size <= 64:
        return dict(layered=False, pad=0.05, radius=0.22, ring_r=0.335, ring_w=0.075, spokes=12, spoke=0.185, spoke_w=0.04, edge=0.02)
    return dict(layered=True, pad=0.04, radius=0.20, ring_r=0.285, ring_w=0.052, spokes=12, spoke=0.165, spoke_w=0.025, edge=0.006)


def round_line(d, x0, y0, x1, y1, w, fill):
    d.line((x0, y0, x1, y1), fill=fill, width=round(w))
    for x, y in ((x0, y0), (x1, y1)):
        d.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=fill)


def render(size):
    p = params(size)
    S = size * SS
    c = S / 2
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(out)

    pad = S * p["pad"]
    outer = (pad, pad, S - pad, S - pad)
    orad = (S - 2 * pad) * p["radius"]
    if p["layered"]:
        # outer tile, then an inset white tile with a soft shadow (as in the reference)
        d.rounded_rectangle(outer, orad, fill=OUTER + (255,), outline=EDGE + (255,), width=max(1, round(S * p["edge"])))
        inset = S * 0.165
        inner = (inset, inset, S - inset, S - inset)
        irad = (S - 2 * inset) * 0.17
        sh = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        ImageDraw.Draw(sh).rounded_rectangle((inner[0], inner[1] + S * 0.012, inner[2], inner[3] + S * 0.012), irad, fill=(0, 0, 0, 34))
        out.alpha_composite(sh.filter(ImageFilter.GaussianBlur(S * 0.018)))
        d = ImageDraw.Draw(out)
        d.rounded_rectangle(inner, irad, fill=INNER + (255,))
    else:
        d.rounded_rectangle(outer, orad, fill=INNER + (255,), outline=EDGE + (255,), width=max(1, round(S * p["edge"])))

    # ring: grey track + orange arc with round caps
    rr, rw = S * p["ring_r"], S * p["ring_w"]
    d.ellipse((c - rr, c - rr, c + rr, c + rr), outline=TRACK + (255,), width=round(rw))
    d.arc((c - rr, c - rr, c + rr, c + rr), -90, ARC_DEG - 90, fill=ORANGE + (255,), width=round(rw))
    for a in (0, ARC_DEG):
        t = math.radians(a)
        x, y = c + (rr - rw / 2) * math.sin(t), c - (rr - rw / 2) * math.cos(t)
        d.ellipse((x - rw / 2, y - rw / 2, x + rw / 2, y + rw / 2), fill=ORANGE + (255,))

    # sunburst: spokes radiating from a small open hub
    n = p["spokes"]
    L, w = S * p["spoke"], S * p["spoke_w"]
    hub = L * (0.05 if n == 12 else 0.0)
    for i in range(n):
        a = math.radians(i * 360 / n)
        k = SPOKES_12[i] if n == 12 else 1.0
        sx, sy = math.sin(a), -math.cos(a)
        round_line(d, c + hub * sx, c + hub * sy, c + L * k * sx, c + L * k * sy, w, ORANGE + (255,))
    return out.resize((size, size), Image.LANCZOS)


def main():
    images = {s: render(s) for s in SIZES}
    ico = os.path.join(HERE, "app.ico")
    images[256].save(ico, format="ICO", sizes=[(s, s) for s in SIZES],
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
