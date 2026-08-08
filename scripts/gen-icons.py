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
ImageMagick/icotool, and an earlier Pillow-based reproduction was never
committed — this script makes the icon pipeline reproducible everywhere
Python + Pillow exist (`pip install Pillow`). `scripts/gen-icons.sh`
remains the rsvg-convert path for Linux hosts; either rasterizer is fine,
just commit one consistently.

All PNGs are 8-bit/color RGBA — GNOME Shell's loader silently fails on
16-bit PNGs (the v2.1.1 Super-key blank-icon bug).

Usage (from anywhere): python scripts/gen-icons.py
"""

from __future__ import annotations

import argparse
import struct
import sys
import tempfile
from pathlib import Path

try:
    from PIL import Image, ImageDraw
except ModuleNotFoundError:  # pragma: no cover - exercised by the CI wiring
    Image = None  # type: ignore[assignment]
    ImageDraw = None  # type: ignore[assignment]

ROOT = Path(__file__).resolve().parent.parent
LINUX = ROOT / "packaging" / "linux"
ICONSET = ROOT / "packaging" / "macos" / "kettle.iconset"
ICO = ROOT / "packaging" / "windows" / "kettle.ico"

# TokyoNight Night, matching the SVG (kettle's default theme since v2.28.0).
BASE = (0x1A, 0x1B, 0x26, 255)  # window fill (theme background)
ACCENT = (0x7A, 0xA2, 0xF7, 255)  # signature blue accent (border + caret)
TEXT = (0xC0, 0xCA, 0xF5, 255)  # prompt chevron (theme foreground)

S = 4  # supersample factor over the 512 viewBox → 2048 px canvas


def render_master() -> Image.Image:
    """The 2048x2048 master frame, mirroring kettle.svg's geometry."""
    img = Image.new("RGBA", (512 * S, 512 * S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Outer window: rect x/y=32 w/h=448 rx=64, stroke #7aa2f7 width 20.
    # Emulate the centered SVG stroke with two rounded rects: the stroke's
    # outer extent filled with the blue accent, then the inner fill on top.
    d.rounded_rectangle(
        [22 * S, 22 * S, 490 * S, 490 * S], radius=74 * S, fill=ACCENT
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

    # Underscore caret `_`: rect x=296 y=326 w=120 h=28 rx=8, blue accent.
    d.rounded_rectangle(
        [296 * S, 326 * S, 416 * S, 354 * S], radius=8 * S, fill=ACCENT
    )
    return img


def scaled(master: Image.Image, px: int) -> Image.Image:
    out = master.resize((px, px), Image.LANCZOS)
    assert out.mode == "RGBA", "icons must stay 8-bit/color RGBA"
    return out


LINUX_SIZES = [16, 24, 32, 48, 64, 128, 256]
ICONSET_BASES = [16, 32, 128, 256, 512]


def emit(
    linux_dir: Path,
    iconset_dir: Path,
    ico_path: Path,
    quiet: bool = False,
) -> list[tuple[Path, str]]:
    """Write every icon artifact under the given roots.

    Returns the (path, label) pairs written, so `--check` can compare the same
    set it would have produced.
    """
    master = render_master()
    written: list[tuple[Path, str]] = []

    linux_dir.mkdir(parents=True, exist_ok=True)
    for px in LINUX_SIZES:
        path = linux_dir / f"kettle-{px}.png"
        scaled(master, px).save(path)
        written.append((path, f"packaging/linux/kettle-{px}.png"))
        if not quiet:
            print(f"wrote {path}")

    iconset_dir.mkdir(parents=True, exist_ok=True)
    for base in ICONSET_BASES:
        names_and_sizes = (
            (f"icon_{base}x{base}.png", base),
            (f"icon_{base}x{base}@2x.png", base * 2),
        )
        for name, px in names_and_sizes:
            scaled(master, px).save(iconset_dir / name)
            written.append(
                (iconset_dir / name, f"packaging/macos/kettle.iconset/{name}")
            )
        if not quiet:
            print(f"wrote icon_{base}x{base}.png (+@2x)")

    ico_path.parent.mkdir(parents=True, exist_ok=True)
    ico_sizes = [(px, px) for px in LINUX_SIZES]
    scaled(master, 256).save(ico_path, format="ICO", sizes=ico_sizes)
    written.append((ico_path, "packaging/windows/kettle.ico"))
    if not quiet:
        print(f"wrote {ico_path}")

    # Pillow silently drops requested sizes larger than the source image. Keep
    # this verification beside the request so both generation and --check fail
    # if the encoder emits an incomplete container.
    with ico_path.open("rb") as stream:
        header = stream.read(6)
    count = struct.unpack("<H", header[4:6])[0]
    if count != len(ico_sizes):
        raise RuntimeError(
            f"{ico_path} has {count} images, expected {len(ico_sizes)}"
        )
    if not quiet:
        print(f"kettle.ico OK ({count} resolutions)")
    return written


def png_encoding(path: Path) -> tuple[int, int]:
    """Return the PNG IHDR bit depth and color type."""
    with path.open("rb") as stream:
        header = stream.read(26)
    if (
        len(header) != 26
        or header[:8] != b"\x89PNG\r\n\x1a\n"
        or header[12:16] != b"IHDR"
    ):
        raise ValueError(f"{path} has no valid PNG IHDR")
    return header[24], header[25]


def same_pixels(a: Path, b: Path) -> bool:
    """Compare image format, required metadata, and decoded pixel content.

    Pillow's PNG/ICO encoders are not byte-identical across versions or
    platforms -- zlib settings and chunk ordering differ -- so a byte comparison
    fails on CI while passing locally, for images that are pixel-for-pixel
    identical. That is a gate that cries wolf, which trains people to ignore it.
    Decoding both sides compares the image content while retaining the PNG mode
    and bit depth constraints that prevent the GNOME blank-icon regression.

    Pillow exposes ICO resolutions through `ico.sizes()` and `ico.getimage()`;
    `n_frames` only reports the default (largest) image.
    """
    with Image.open(a) as left, Image.open(b) as right:
        if left.format != right.format:
            return False

        if left.format == "ICO":
            left_sizes = left.ico.sizes()
            right_sizes = right.ico.sizes()
            if left_sizes != right_sizes:
                return False
            for size in sorted(left_sizes):
                left_image = left.ico.getimage(size)
                right_image = right.ico.getimage(size)
                if (
                    left_image.size != right_image.size
                    or left_image.mode != right_image.mode
                    or left_image.tobytes() != right_image.tobytes()
                ):
                    return False
            return True

        if left.size != right.size or left.mode != right.mode:
            return False
        if left.format == "PNG" and png_encoding(a) != png_encoding(b):
            return False
        return left.tobytes() == right.tobytes()


def check() -> int:
    """Fail if any tracked raster has drifted from the Pillow generator.

    The 18 tracked rasters had no gate of any kind: `gen-icons.py` was run by
    hand and dispatched by no recipe or CI job, so an edit to the geometry — or
    a hand-touched PNG — could ship silently. The generator is deterministic
    (pure Pillow, fixed supersampling), so regenerating into a scratch tree and
    comparing decoded content and required image metadata is an exact check
    rather than a perceptual one.
    """
    with tempfile.TemporaryDirectory(prefix="kettle-icons-") as tmp:
        root = Path(tmp)
        produced = emit(
            root / "linux",
            root / "iconset",
            root / "windows" / "kettle.ico",
            quiet=True,
        )
        drifted: list[str] = []
        missing: list[str] = []
        for path, rel in produced:
            tracked = ROOT / rel
            if not tracked.exists():
                missing.append(rel)
            elif not same_pixels(path, tracked):
                drifted.append(rel)
    for rel in missing:
        print(f"ERROR: tracked icon missing: {rel}")
    for rel in drifted:
        print(f"ERROR: tracked icon differs from the generated one: {rel}")
    if missing or drifted:
        print(
            f"{len(missing) + len(drifted)} icon(s) out of date — "
            "run `python3 scripts/gen-icons.py` and commit the result"
        )
        return 1
    print(f"icons OK ({len(produced)} tracked rasters match the Pillow generator)")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the tracked rasters match the generator instead of rewriting them",
    )
    parser.add_argument(
        "--require-tooling",
        action="store_true",
        help=(
            "treat a missing Pillow as a failure rather than a skip. CI passes "
            "this so the gate cannot quietly stop running"
        ),
    )
    args = parser.parse_args()

    if Image is None:
        # Skipping locally is deliberate -- a contributor without Pillow should
        # not be blocked -- but a skip that CI accepts is just a gate that never
        # runs, which is the defect this check was added to close. CI passes
        # --require-tooling, so the skip cannot become permanent there.
        message = "Pillow is not installed (pip install Pillow)"
        if args.require_tooling:
            print(f"ERROR: {message}; --require-tooling makes this fatal")
            return 1
        print(f"SKIPPED: {message}")
        return 0

    try:
        if args.check:
            return check()
        emit(LINUX, ICONSET, ICO)
    except RuntimeError as error:
        print(f"ERROR: {error}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
