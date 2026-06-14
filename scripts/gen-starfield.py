#!/usr/bin/env python3
"""Generate kettle's sample animated background: a slow forward-flight starfield.

Stars emerge near the center and drift outward to the edges as the camera moves
forward — the classic "warp at low speed" look — kept slow, sparse, and dark so
terminal text stays readable. Modeled in SCREEN SPACE (each star has an angle +
a radial progress p that cycles 0->1 over the loop), so on-screen density and
brightness are directly controlled and the field looks right at any aspect ratio.

Seamless loop: a star's brightness fades to 0 at both ends of p (center and
edge), so the wrap is invisible. Output is bounded under kettle's 256 MB
decoded-animation cap (W*H*4*frames). MIT — part of kettle. Requires Pillow.

Why this look? The community keeps terminal backgrounds that *recede*: slow,
dark, subtle (WezTerm users drop GIFs to 0.2x). A drifting starfield reads as
alive without the busyness of a bright scene/scientific-viz loop, and a uniform
radial field survives any aspect-ratio stretch.

Usage:
    python scripts/gen-starfield.py [OUTPUT.gif]      # default: ./space-starfield.gif
"""
import math
import os
import random
import sys

from PIL import Image, ImageDraw

# 1920x1080 * 4 * 32 = 253 MB decoded, under kettle's 256 MB cap. 16:9 source.
W, H = 1920, 1080
NFRAMES = 32
FPS = 8                  # 4.0 s loop — slow, gentle drift
NSTARS = 90
SMIN, SMAX = 1.4, 3.6    # star radius grows from far (center) to near (edge)
RADIAL_EASE = 1.7        # >1: slow near center, faster near the edge (perspective)
random.seed(20260614)    # deterministic output

BG = (10, 10, 18)        # near-black, faint blue — sits under any dark theme


def star_color():
    r = random.random()
    if r < 0.72:
        return (216, 226, 246)   # cool white-blue
    if r < 0.9:
        return (234, 234, 242)   # near white
    return (246, 230, 206)       # faint warm


cx, cy = W / 2.0, H / 2.0
RMAX = math.hypot(cx, cy) * 1.04  # reach the corners
stars = [dict(
    th=random.uniform(0, 2 * math.pi),
    p0=random.uniform(0, 1),
    peak=random.choices(
        [random.uniform(0.60, 0.74), random.uniform(0.80, 0.92), random.uniform(0.96, 1.0)],
        [4, 3, 3])[0],
    base=star_color(),
) for _ in range(NSTARS)]


def render(fi):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    for s in stars:
        p = (s["p0"] + fi / NFRAMES) % 1.0
        r = RMAX * (p ** RADIAL_EASE)
        # Flat-top fade: full brightness across the mid-travel, easing to 0 only
        # at the center (emerging) and the edge (passing) so the loop is seamless.
        fade = min(1.0, math.sin(math.pi * p) * 1.7)
        k = s["peak"] * fade
        if k <= 0.02:
            continue
        x = cx + r * math.cos(s["th"])
        y = cy + r * math.sin(s["th"])
        if x < -4 or x > W + 4 or y < -4 or y > H + 4:
            continue
        size = SMIN + (SMAX - SMIN) * p
        c = tuple(int(BG[i] + (s["base"][i] - BG[i]) * k) for i in range(3))
        if size >= 2.6:                  # faint glow on the closest stars
            g = tuple(int(BG[i] + (s["base"][i] - BG[i]) * k * 0.22) for i in range(3))
            d.ellipse([x - size, y - size, x + size, y + size], fill=g)
        rr = max(0.6, size / 2.0)
        d.ellipse([x - rr, y - rr, x + rr, y + rr], fill=c)
    return img


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "space-starfield.gif"
    out = os.path.expanduser(out)
    if os.path.dirname(out):
        os.makedirs(os.path.dirname(out), exist_ok=True)
    frames = [render(i) for i in range(NFRAMES)]
    frames[0].save(out, save_all=True, append_images=frames[1:],
                   duration=int(1000 / FPS), loop=0, optimize=True, disposal=2)
    print(f"wrote {out} ({os.path.getsize(out) // 1024} KB, {W}x{H}, {NFRAMES} frames, "
          f"{W*H*4*NFRAMES/1024/1024:.0f} MB decoded)")


if __name__ == "__main__":
    main()
