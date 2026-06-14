#!/usr/bin/env python3
"""Generate kettle's sample animated background: a dark, subtle, slowly-twinkling
starfield with one slow shooting star. Seamless loop, MIT (part of kettle).

Why a starfield? The community consensus for a *good* terminal animated
background is **slow, dark, subtle** — it must recede behind your text, not
fight it (WezTerm users routinely drop GIF playback to 0.2x speed). A uniform
field of tiny stars is also the only kind of image that looks right at *every*
aspect ratio and resolution: stretched from 4:3 to 32:9 there is no geometry to
distort, and soft upscaling on a 4K display just reads as a calmer sky. Bright,
busy scientific-viz loops (nebulae, accretion disks) do the opposite.

Output is bounded to stay under kettle's 256 MB decoded-animation cap
(W*H*4*frames). Re-run to regenerate; tweak the constants to taste.

Usage:
    python scripts/gen-starfield.py [OUTPUT.gif]      # default: ./space-starfield.gif

Requires Pillow (`pip install pillow`).
"""
import math
import os
import random
import sys

from PIL import Image, ImageDraw

# 16:9 is the central common aspect, so stretch toward 4:3 (~0.75x) and 21:9
# (~1.3x) is minimal — and imperceptible for tiny round dots.
# 1600*900*4*40 = 230 MB decoded, under kettle's 256 MB cap.
W, H = 1600, 900
NFRAMES = 40          # 4.0 s loop at 10 fps
FPS = 10
NSTARS = 120
random.seed(20260614)  # deterministic output

BG = (10, 10, 18)      # near-black with a faint blue — sits under any dark theme


def star_color():
    r = random.random()
    if r < 0.7:
        return (200, 214, 240)   # cool white-blue
    if r < 0.9:
        return (220, 224, 235)   # near white
    return (240, 226, 200)       # faint warm


class Star:
    __slots__ = ("x", "y", "size", "peak", "glow", "amp", "phase", "steady", "base")

    def __init__(self):
        self.x = random.uniform(0, W)
        self.y = random.uniform(0, H)
        self.size = random.choices([1, 1, 1, 2, 2, 3], [5, 5, 5, 3, 2, 1])[0]
        rr = random.random()
        if rr < 0.6:
            self.peak = random.uniform(0.18, 0.38)   # faint (most stars)
        elif rr < 0.9:
            self.peak = random.uniform(0.40, 0.65)   # medium
        else:
            self.peak = random.uniform(0.70, 1.0)    # few bright
        self.glow = self.size >= 3 and self.peak > 0.7
        self.steady = random.random() < 0.55         # ~half never twinkle
        self.amp = 0.0 if self.steady else random.uniform(0.25, 0.6)
        self.phase = random.uniform(0, 2 * math.pi)
        self.base = star_color()


stars = [Star() for _ in range(NSTARS)]

# One subtle shooting star, fully faded in and out inside the loop window so the
# loop stays seamless (nothing visible at frame 0 / NFRAMES).
SS_START, SS_LEN = 14, 12
sx0, sy0 = random.uniform(W * 0.1, W * 0.5), random.uniform(H * 0.05, H * 0.35)
sdx, sdy = random.uniform(150, 210), random.uniform(70, 105)


def render(fi):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    t = fi / NFRAMES
    for s in stars:
        if s.steady:
            k = s.peak
        else:
            k = s.peak * (1 - s.amp + s.amp * 0.5 * (1 + math.sin(2 * math.pi * t + s.phase)))
        col = tuple(int(BG[i] + (s.base[i] - BG[i]) * k) for i in range(3))
        if s.glow:
            g = tuple(int(BG[i] + (s.base[i] - BG[i]) * k * 0.25) for i in range(3))
            d.ellipse([s.x - 3, s.y - 3, s.x + 3, s.y + 3], fill=g)
        r = s.size / 2.0
        d.ellipse([s.x - r, s.y - r, s.x + r, s.y + r], fill=col)
    if SS_START <= fi < SS_START + SS_LEN:
        p = (fi - SS_START) / SS_LEN
        fade = math.sin(math.pi * p)
        hx, hy = sx0 + sdx * p, sy0 + sdy * p
        tail = 36
        for j in range(tail):
            tp = j / tail
            px, py = hx - sdx * 0.06 * tp, hy - sdy * 0.06 * tp
            a = fade * (1 - tp) * 0.85
            col = tuple(int(BG[i] + (235 - BG[i]) * a) for i in range(3))
            d.ellipse([px - 0.8, py - 0.8, px + 0.8, py + 0.8], fill=col)
    return img


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "space-starfield.gif"
    out = os.path.expanduser(out)
    if os.path.dirname(out):
        os.makedirs(os.path.dirname(out), exist_ok=True)
    frames = [render(i) for i in range(NFRAMES)]
    frames[0].save(
        out, save_all=True, append_images=frames[1:],
        duration=int(1000 / FPS), loop=0, optimize=True, disposal=2,
    )
    print(f"wrote {out} ({os.path.getsize(out) // 1024} KB, {W}x{H}, {NFRAMES} frames)")


if __name__ == "__main__":
    main()
