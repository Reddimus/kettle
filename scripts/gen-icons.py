#!/usr/bin/env python3
"""kettle — regenerate every icon source and raster from one geometry model.

This script owns the shared terminal-face geometry and emits the compatible
pre-rounded Linux/Windows vector plus the native Icon Composer foreground that
Apple's asset compiler masks for every macOS surface. It then renders the
committed platform rasters with Pillow at 4x
supersampling (2048 px canvas, LANCZOS downscale) and writes:

  - packaging/linux/kettle.svg                            (generated source)
  - packaging/macos/AppIcon.icon/{icon.json,Assets/kettle.svg}
  - packaging/linux/kettle-{16,24,32,48,64,128,256}.png  (hicolor theme +
    the `include_bytes!` embedded winit window icon, which needs a binary
    rebuild to pick up)
  - packaging/macos/kettle.iconset/icon_*.png            (10 compatibility
    sources retained for downstream packagers)
  - packaging/windows/kettle.ico                          (exactly 7 sizes;
    CI asserts the resolution count)

Why Pillow and not rsvg-convert: the Windows dev host has no rsvg/
ImageMagick/icotool. One renderer also prevents the SVG and Pillow paths from
producing different committed pixels. `scripts/gen-icons.sh` is retained as a
compatibility wrapper around this script.

All PNGs are 8-bit/color RGBA — GNOME Shell's loader silently fails on
16-bit PNGs (the v2.1.1 Super-key blank-icon bug).

Usage (from anywhere): python scripts/gen-icons.py
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
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

CANVAS = 512
OUTER_BOX = (32, 32, 480, 480)
OUTER_RADIUS = 64
OUTER_STROKE = 20
FACE_BOX = (42, 42, 470, 470)
# At Icon Composer's 200% layer scale, the native mask renders with about a
# 45 px corner radius and the terminal face sits about 17 px inside it. The
# source radius below renders near 28 px: the outer radius minus the inset, so
# both curves share a center and the blue rim stays visually even.
FACE_RADIUS = 70
ICON_COMPOSER_SCALE = 2
PROMPT_POINTS = ((156, 168), (260, 256), (156, 344))
PROMPT_STROKE = 36
CARET_BOX = (296, 326, 416, 354)
CARET_RADIUS = 8


@dataclass(frozen=True)
class RectPrimitive:
    box: tuple[int, int, int, int]
    radius: int
    fill: tuple[int, int, int, int]
    stroke: tuple[int, int, int, int] | None = None
    stroke_width: int = 0


@dataclass(frozen=True)
class PolylinePrimitive:
    points: tuple[tuple[int, int], ...]
    stroke: tuple[int, int, int, int]
    stroke_width: int


Primitive = RectPrimitive | PolylinePrimitive


def icon_primitives() -> tuple[Primitive, ...]:
    """Return the compatible geometry consumed by Linux/Windows outputs."""
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, BASE, ACCENT, OUTER_STROKE),
        PolylinePrimitive(PROMPT_POINTS, TEXT, PROMPT_STROKE),
        RectPrimitive(CARET_BOX, CARET_RADIUS, ACCENT),
    )


def icon_composer_primitives() -> tuple[Primitive, ...]:
    """Artwork above Icon Composer's Kettle-blue system background.

    The document owns the full canvas fill and macOS owns the outer mask.
    Keeping both out of this SVG prevents the old double-rounded treatment and
    lets Finder, the Dock and the running application render one asset.
    """
    return (
        RectPrimitive(FACE_BOX, FACE_RADIUS, BASE),
        PolylinePrimitive(PROMPT_POINTS, TEXT, PROMPT_STROKE),
        RectPrimitive(CARET_BOX, CARET_RADIUS, ACCENT),
    )


def color_hex(color: tuple[int, int, int, int]) -> str:
    return "#" + "".join(f"{component:02x}" for component in color[:3])


def svg_source() -> str:
    """Return a vector artifact from the same primitives Pillow consumes."""
    body: list[str] = []
    for primitive in icon_primitives():
        if isinstance(primitive, RectPrimitive):
            left, top, right, bottom = primitive.box
            attributes = [
                f'x="{left}"', f'y="{top}"',
                f'width="{right - left}"', f'height="{bottom - top}"',
                f'rx="{primitive.radius}"', f'fill="{color_hex(primitive.fill)}"',
            ]
            if primitive.stroke is not None:
                attributes.extend((f'stroke="{color_hex(primitive.stroke)}"',
                                   f'stroke-width="{primitive.stroke_width}"'))
            body.append(f"  <rect {' '.join(attributes)}/>")
        else:
            points = " ".join(f"{x},{y}" for x, y in primitive.points)
            body.append(
                f'  <polyline points="{points}" fill="none" '
                f'stroke="{color_hex(primitive.stroke)}" '
                f'stroke-width="{primitive.stroke_width}" '
                'stroke-linecap="round" stroke-linejoin="round"/>'
            )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!-- Compatible pre-rounded source for Linux, Windows and macOS 11-15. Geometry '
        'below is generated from icon_primitives. -->\n'
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" '
        f'height="{CANVAS}" viewBox="0 0 {CANVAS} {CANVAS}">\n'
        + "\n".join(body)
        + "\n</svg>\n"
    )


def icon_composer_svg_source() -> str:
    body: list[str] = []
    for primitive in icon_composer_primitives():
        if isinstance(primitive, RectPrimitive):
            left, top, right, bottom = primitive.box
            body.append(
                f'  <rect x="{left}" y="{top}" width="{right - left}" '
                f'height="{bottom - top}" rx="{primitive.radius}" '
                f'fill="{color_hex(primitive.fill)}"/>'
            )
        else:
            points = " ".join(f"{x},{y}" for x, y in primitive.points)
            body.append(
                f'  <polyline points="{points}" fill="none" '
                f'stroke="{color_hex(primitive.stroke)}" '
                f'stroke-width="{primitive.stroke_width}" '
                'stroke-linecap="round" stroke-linejoin="round"/>'
            )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!-- Foreground for AppIcon.icon. Icon Composer supplies the blue '
        'background and macOS supplies the outer mask. -->\n'
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" '
        f'height="{CANVAS}" viewBox="0 0 {CANVAS} {CANVAS}">\n'
        + "\n".join(body)
        + "\n</svg>\n"
    )


def icon_composer_json_source() -> str:
    return f"""{{
  "fill" : {{
    "solid" : "srgb:0.47843,0.63529,0.96863,1.00000"
  }},
  "groups" : [
    {{
      "layers" : [
        {{
          "hidden" : false,
          "image-name" : "kettle.svg",
          "name" : "Kettle Terminal",
          "position" : {{
            "scale" : {ICON_COMPOSER_SCALE},
            "translation-in-points" : [
              0,
              0
            ]
          }}
        }}
      ],
      "shadow" : {{
        "kind" : "none",
        "opacity" : 0.5
      }},
      "specular" : false,
      "translucency" : {{
        "enabled" : false,
        "value" : 0.5
      }}
    }}
  ],
  "supported-platforms" : {{
    "circles" : [

    ],
    "squares" : "shared"
  }}
}}
"""


def render_primitives(primitives: tuple[Primitive, ...]) -> Image.Image:
    """Render the shared primitive model with Pillow."""
    img = Image.new("RGBA", (CANVAS * S, CANVAS * S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    for primitive in primitives:
        if isinstance(primitive, RectPrimitive):
            box = [coordinate * S for coordinate in primitive.box]
            if primitive.stroke is not None:
                half = primitive.stroke_width // 2
                outer = [
                    (primitive.box[0] - half) * S,
                    (primitive.box[1] - half) * S,
                    (primitive.box[2] + half) * S,
                    (primitive.box[3] + half) * S,
                ]
                d.rounded_rectangle(
                    outer,
                    radius=(primitive.radius + half) * S,
                    fill=primitive.stroke,
                )
                box = [
                    (primitive.box[0] + half) * S,
                    (primitive.box[1] + half) * S,
                    (primitive.box[2] - half) * S,
                    (primitive.box[3] - half) * S,
                ]
                radius = (primitive.radius - half) * S
            else:
                radius = primitive.radius * S
            d.rounded_rectangle(box, radius=radius, fill=primitive.fill)
        else:
            points = [(x * S, y * S) for x, y in primitive.points]
            width = primitive.stroke_width * S
            d.line(points, fill=primitive.stroke, width=width)
            for x, y in points:
                d.ellipse(
                    [x - width // 2, y - width // 2,
                     x + width // 2, y + width // 2],
                    fill=primitive.stroke,
                )
    return img


def render_master() -> Image.Image:
    return render_primitives(icon_primitives())


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
    linux_svg = linux_dir / "kettle.svg"
    # Write bytes, not text mode: Windows translates `\n` to CRLF in
    # `Path.write_text`, while .gitattributes forces the tracked SVGs to LF and
    # the drift gate deliberately compares vector bytes exactly.
    linux_svg.write_bytes(svg_source().encode("utf-8"))
    written.append((linux_svg, "packaging/linux/kettle.svg"))

    icon_composer = iconset_dir.parent / "AppIcon.icon"
    composer_assets = icon_composer / "Assets"
    composer_assets.mkdir(parents=True, exist_ok=True)
    composer_svg = composer_assets / "kettle.svg"
    composer_svg.write_bytes(icon_composer_svg_source().encode("utf-8"))
    written.append(
        (composer_svg, "packaging/macos/AppIcon.icon/Assets/kettle.svg")
    )
    composer_json = icon_composer / "icon.json"
    composer_json.write_bytes(icon_composer_json_source().encode("utf-8"))
    written.append((composer_json, "packaging/macos/AppIcon.icon/icon.json"))

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
            # Keep the former standalone iconset reproducible for downstream
            # packagers. The official .app now uses AppIcon.icon, whose actool
            # output includes its own previous-release fallback.
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


def same_artifact(generated: Path, tracked: Path) -> bool:
    if generated.suffix in {".svg", ".json"}:
        return generated.read_bytes() == tracked.read_bytes()
    return same_pixels(generated, tracked)


def check() -> int:
    """Fail if any tracked icon artifact has drifted from the generator.

    The tracked artifacts once had no gate of any kind: `gen-icons.py` was run
    by hand and dispatched by no recipe or CI job, so an edit to the geometry —
    or a hand-touched SVG/PNG — could ship silently. The generator is
    deterministic (pure Pillow, fixed supersampling), so regenerating into a
    scratch tree and comparing vector bytes, decoded content, and required image
    metadata is an exact check rather than a perceptual one.
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
            elif not same_artifact(path, tracked):
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
    print(f"icons OK ({len(produced)} tracked artifacts match the generator)")
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
