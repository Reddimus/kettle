#!/usr/bin/env python3
"""kettle — regenerate every icon artifact from the SVG design (Pillow).

`packaging/linux/kettle.svg` is the single source of truth. This script
reproduces its geometry with Pillow at 4x supersampling (2048 px canvas,
LANCZOS downscale) and writes:

  - packaging/linux/kettle-{16,24,32,48,64,128,256}.png  (hicolor theme +
    the `include_bytes!` embedded winit window icon, which needs a binary
    rebuild to pick up)
  - packaging/macos/kettle.iconset/icon_*.png            (10 files; CI runs
    `iconutil -c icns` over them)
  - packaging/windows/kettle.ico                          (exactly 7 sizes;
    CI asserts the resolution count)

Why Pillow and not rsvg-convert: the Windows dev host has no rsvg/
ImageMagick/icotool, and the cycle-919 Pillow reproduction was never
committed — this script makes the icon pipeline reproducible everywhere
Python + Pillow exist (`pip install Pillow`). `scripts/gen-icons.sh`
remains the rsvg-convert path for Linux hosts; either rasterizer is fine,
just commit one consistently.

All PNGs are 8-bit/color RGBA — GNOME Shell's loader silently fails on
16-bit PNGs (the v2.1.1 Super-key blank-icon bug).

Usage (from anywhere): python scripts/gen-icons.py
"""

import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
LINUX = ROOT / "packaging" / "linux"
ICONSET = ROOT / "packaging" / "macos" / "kettle.iconset"
ICO = ROOT / "packaging" / "windows" / "kettle.ico"

# Catppuccin Mocha, matching the SVG.
BASE = (0x1E, 0x1E, 0x2E, 255)  # window fill
MAUVE = (0xCB, 0xA6, 0xF7, 255)  # signature accent (border + caret)
TEXT = (0xCD, 0xD6, 0xF4, 255)  # prompt chevron

S = 4  # supersample factor over the 512 viewBox → 2048 px canvas


def render_master() -> Image.Image:
    """The 2048x2048 master frame, mirroring kettle.svg's geometry."""
    img = Image.new("RGBA", (512 * S, 512 * S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Outer window: rect x/y=32 w/h=448 rx=64, stroke #cba6f7 width 20.
    # Emulate the centered SVG stroke with two rounded rects: the stroke's
    # outer extent filled mauve, then the inner fill on top.
    d.rounded_rectangle(
        [22 * S, 22 * S, 490 * S, 490 * S], radius=74 * S, fill=MAUVE
    )
    d.rounded_rectangle(
        [42 * S, 42 * S, 470 * S, 470 * S], radius=54 * S, fill=BASE
    )

    # Prompt chevron `>`: polyline (156,168)→(260,256)→(156,344),
    # stroke-width 36, round caps + joins (circles at every vertex).
    pts = [(156 * S, 168 * S), (260 * S, 256 * S), (156 * S, 344 * S)]
    w = 36 * S
    d.line(pts, fill=TEXT, width=w)
    for (x, y) in pts:
        d.ellipse([x - w // 2, y - w // 2, x + w // 2, y + w // 2], fill=TEXT)

    # Underscore caret `_`: rect x=296 y=326 w=120 h=28 rx=8, mauve.
    d.rounded_rectangle(
        [296 * S, 326 * S, 416 * S, 354 * S], radius=8 * S, fill=MAUVE
    )
    return img


def scaled(master: Image.Image, px: int) -> Image.Image:
    out = master.resize((px, px), Image.LANCZOS)
    assert out.mode == "RGBA", "icons must stay 8-bit/color RGBA"
    return out


def main() -> int:
    master = render_master()

    # Linux hicolor PNGs (also the embedded window icon's source).
    linux_sizes = [16, 24, 32, 48, 64, 128, 256]
    for px in linux_sizes:
        path = LINUX / f"kettle-{px}.png"
        scaled(master, px).save(path)
        print(f"wrote {path}")

    # macOS iconset (iconutil consumes the @2x naming).
    ICONSET.mkdir(parents=True, exist_ok=True)
    for base in [16, 32, 128, 256, 512]:
        scaled(master, base).save(ICONSET / f"icon_{base}x{base}.png")
        scaled(master, base * 2).save(ICONSET / f"icon_{base}x{base}@2x.png")
        print(f"wrote icon_{base}x{base}.png (+@2x)")

    # Windows .ico — exactly 7 sizes (CI asserts the count ≥4; the release
    # recipe ships 7 so every shell surface gets a crisp raster).
    ico_sizes = [(px, px) for px in linux_sizes]
    scaled(master, 256).save(ICO, format="ICO", sizes=ico_sizes)
    print(f"wrote {ICO}")

    # Self-verify: the .ico header's image count must equal the request —
    # Pillow silently drops sizes larger than the source image.
    with open(ICO, "rb") as f:
        header = f.read(6)
    count = struct.unpack("<H", header[4:6])[0]
    if count != len(ico_sizes):
        print(f"ERROR: kettle.ico has {count} images, expected {len(ico_sizes)}")
        return 1
    print(f"kettle.ico OK ({count} resolutions)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
