#!/usr/bin/env python3
"""kettle — regenerate every icon source and raster from one geometry model.

This script owns the shared literal terminal-kettle mark and emits the compatible
pre-rounded Linux/Windows vector plus the borderless native Icon Composer
foreground that Apple's asset compiler masks for every macOS surface. It then
renders the committed platform rasters with Pillow at 4x
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
from io import BytesIO
import math
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

# One two-color identity, inverted exactly for the light preview.
DARK_BACKGROUND = (0x1A, 0x1B, 0x26, 255)
DARK_ACCENT = (0x7A, 0xA2, 0xF7, 255)
LIGHT_BACKGROUND = DARK_ACCENT
LIGHT_ACCENT = DARK_BACKGROUND

S = 4  # supersample factor over the 512 viewBox → 2048 px canvas

CANVAS = 512
OUTER_BOX = (32, 32, 480, 480)
OUTER_RADIUS = 92
# Icon Composer measures this as a multiplier over its conservative default
# foreground scale. At 2x the mark occupies a little over half of the compiled
# icon while retaining generous clear space. The previous double-rounded tile—
# not this scale—was what collided optically with the Dock mask.
ICON_COMPOSER_SCALE = 2


@dataclass(frozen=True)
class RectPrimitive:
    box: tuple[int, int, int, int]
    radius: int
    fill: tuple[int, int, int, int]


@dataclass(frozen=True)
class PolylinePrimitive:
    points: tuple[tuple[int, int], ...]
    stroke: tuple[int, int, int, int]
    stroke_width: int
    linecap: str = "round"


Point = tuple[int, int]
CubicSegment = tuple[Point, Point, Point]


@dataclass(frozen=True)
class CubicPrimitive:
    start: Point
    segments: tuple[CubicSegment, ...]
    stroke: tuple[int, int, int, int]
    stroke_width: int
    linecap: str = "round"


MarkPrimitive = PolylinePrimitive | CubicPrimitive
Primitive = RectPrimitive | MarkPrimitive


def sampled_cubic_points(
    primitive: CubicPrimitive, steps_per_segment: int = 32
) -> tuple[tuple[float, float], ...]:
    """Sample one SVG cubic path for Pillow and geometry measurements."""
    points: list[tuple[float, float]] = [primitive.start]
    x0, y0 = primitive.start
    for control1, control2, end in primitive.segments:
        x1, y1 = control1
        x2, y2 = control2
        x3, y3 = end
        for step in range(1, steps_per_segment + 1):
            t = step / steps_per_segment
            inverse = 1 - t
            points.append(
                (
                    inverse**3 * x0
                    + 3 * inverse**2 * t * x1
                    + 3 * inverse * t**2 * x2
                    + t**3 * x3,
                    inverse**3 * y0
                    + 3 * inverse**2 * t * y1
                    + 3 * inverse * t**2 * y2
                    + t**3 * y3,
                )
            )
        x0, y0 = end
    return tuple(points)


def primitive_bounds(
    primitives: tuple[MarkPrimitive, ...],
) -> tuple[int, int, int, int]:
    """Return pixel-conservative bounds for supplied foreground strokes."""
    xs: list[int] = []
    ys: list[int] = []
    for primitive in primitives:
        if isinstance(primitive, PolylinePrimitive):
            half = primitive.stroke_width // 2
            xs.extend(
                coordinate
                for x, _ in primitive.points
                for coordinate in (x - half, x + half)
            )
            ys.extend(
                coordinate
                for _, y in primitive.points
                for coordinate in (y - half, y + half)
            )
        else:
            half = primitive.stroke_width / 2
            points = sampled_cubic_points(primitive, steps_per_segment=64)
            xs.extend(
                coordinate
                for x, _ in points
                for coordinate in (x - half, x + half)
            )
            ys.extend(
                coordinate
                for _, y in points
                for coordinate in (y - half, y + half)
            )
    if not xs:
        raise ValueError("cannot measure an empty primitive set")
    return (
        math.floor(min(xs)),
        math.floor(min(ys)),
        math.ceil(max(xs)),
        math.ceil(max(ys)),
    )


def kettle_primitives(
    accent: tuple[int, int, int, int] | None = None,
) -> tuple[MarkPrimitive, ...]:
    """The literal ``>(_)~`` terminal-kettle mark shared at 24 px and above.

    These are vector strokes rather than font glyphs, so the mark has identical
    metrics on macOS, Linux and Windows. Real cubic curves keep the parentheses
    and tilde typographic instead of turning them into a segmented, bulbous
    vessel. Every glyph stays opaque because partial alpha blurred small-size
    gaps and made half of the mark look disabled.
    """
    accent = DARK_ACCENT if accent is None else accent
    return (
        # Prompt chevron.
        PolylinePrimitive(((82, 218), (116, 256), (82, 294)), accent, 36),
        # Literal left parenthesis: a real cubic, not a seven-segment arc.
        CubicPrimitive(
            (198, 188),
            (((166, 216), (166, 296), (198, 324)),),
            accent,
            32,
        ),
        # Keep the underscore well above the parenthesis terminals so the
        # three strokes remain separate rather than fusing into a horseshoe.
        PolylinePrimitive(((210, 292), (290, 292)), accent, 28, "butt"),
        # Literal right parenthesis.
        CubicPrimitive(
            (304, 188),
            (((336, 216), (336, 296), (304, 324)),),
            accent,
            32,
        ),
        # Literal tilde: two cubic phases form one continuous typographic wave.
        CubicPrimitive(
            (374, 256),
            (
                ((382, 218), (396, 218), (404, 256)),
                ((412, 294), (426, 294), (434, 256)),
            ),
            accent,
            28,
        ),
    )


def compact_terminal_primitives(
    accent: tuple[int, int, int, int] | None = None,
) -> tuple[MarkPrimitive, ...]:
    """A two-stroke ``>_`` optical-size mark for the 16 px raster only.

    Five punctuation strokes cannot remain distinct in a sixteen-pixel tile;
    antialiasing fuses ``>(_)~`` into two blobs. The conventional terminal mark
    preserves the identity at that physical limit. Every 24 px and larger
    output retains the full kettle spelling.
    """
    accent = DARK_ACCENT if accent is None else accent
    return (
        PolylinePrimitive(((160, 208), (202, 256), (160, 304)), accent, 42),
        PolylinePrimitive(((246, 292), (358, 292)), accent, 34, "butt"),
    )


def compact_icon_primitives() -> tuple[Primitive, ...]:
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, DARK_BACKGROUND),
        *compact_terminal_primitives(),
    )


def icon_primitives() -> tuple[Primitive, ...]:
    """Return the compatible geometry consumed by Linux/Windows outputs."""
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, DARK_BACKGROUND),
        *kettle_primitives(DARK_ACCENT),
    )


def light_icon_primitives() -> tuple[Primitive, ...]:
    """Return an exact color inversion of the dark shared geometry."""
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, LIGHT_BACKGROUND),
        *kettle_primitives(LIGHT_ACCENT),
    )


def icon_composer_primitives() -> tuple[Primitive, ...]:
    """Artwork above Icon Composer's TokyoNight Night background.

    The document owns the full canvas fill and macOS owns the outer mask.
    Keeping both out of this SVG prevents the old double-rounded treatment and
    lets Finder, the Dock and the running application render one asset.
    """
    return kettle_primitives(DARK_ACCENT)


def color_hex(color: tuple[int, int, int, int]) -> str:
    return "#" + "".join(f"{component:02x}" for component in color[:3])


def srgb_source(color: tuple[int, int, int, int]) -> str:
    """Return Icon Composer's normalized sRGB tuple for an RGBA color."""
    return "srgb:" + ",".join(f"{component / 255:.5f}" for component in color)


def primitive_svg(primitive: Primitive) -> str:
    if isinstance(primitive, RectPrimitive):
        left, top, right, bottom = primitive.box
        attributes = [
            f'x="{left}"',
            f'y="{top}"',
            f'width="{right - left}"',
            f'height="{bottom - top}"',
            f'rx="{primitive.radius}"',
            f'fill="{color_hex(primitive.fill)}"',
        ]
        return f"  <rect {' '.join(attributes)}/>"
    if isinstance(primitive, PolylinePrimitive):
        points = " ".join(f"{x},{y}" for x, y in primitive.points)
        return (
            f'  <polyline points="{points}" fill="none" '
            f'stroke="{color_hex(primitive.stroke)}" '
            f'stroke-width="{primitive.stroke_width}" '
            f'stroke-linecap="{primitive.linecap}" stroke-linejoin="round"/>'
        )
    commands = [f"M {primitive.start[0]} {primitive.start[1]}"]
    commands.extend(
        f"C {control1[0]} {control1[1]} {control2[0]} {control2[1]} {end[0]} {end[1]}"
        for control1, control2, end in primitive.segments
    )
    return (
        f'  <path d="{" ".join(commands)}" fill="none" '
        f'stroke="{color_hex(primitive.stroke)}" '
        f'stroke-width="{primitive.stroke_width}" '
        f'stroke-linecap="{primitive.linecap}" stroke-linejoin="round"/>'
    )


def svg_source() -> str:
    """Return a vector artifact from the same primitives Pillow consumes."""
    body = [primitive_svg(primitive) for primitive in icon_primitives()]
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
    body = [primitive_svg(primitive) for primitive in icon_composer_primitives()]
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!-- Foreground for AppIcon.icon. Icon Composer supplies the dark '
        'background and macOS supplies the outer mask. -->\n'
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" '
        f'height="{CANVAS}" viewBox="0 0 {CANVAS} {CANVAS}">\n'
        + "\n".join(body)
        + "\n</svg>\n"
    )


def icon_composer_json_source() -> str:
    return f"""{{
  "fill" : {{
    "solid" : "{srgb_source(DARK_BACKGROUND)}"
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
    """Render the opaque shared primitive model."""
    img = Image.new("RGBA", (CANVAS * S, CANVAS * S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    for primitive in primitives:
        if isinstance(primitive, RectPrimitive):
            box = [coordinate * S for coordinate in primitive.box]
            d.rounded_rectangle(
                box,
                radius=primitive.radius * S,
                fill=primitive.fill,
            )
        else:
            source_points = (
                primitive.points
                if isinstance(primitive, PolylinePrimitive)
                else sampled_cubic_points(primitive)
            )
            points = [(round(x * S), round(y * S)) for x, y in source_points]
            width = primitive.stroke_width * S
            d.line(
                points,
                fill=primitive.stroke,
                width=width,
                joint="curve",
            )
            if primitive.linecap == "round":
                radius = width // 2
                for x, y in (points[0], points[-1]):
                    d.ellipse(
                        (x - radius, y - radius, x + radius, y + radius),
                        fill=primitive.stroke,
                    )
    return img


def render_master() -> Image.Image:
    return render_primitives(icon_primitives())


def render_light_master() -> Image.Image:
    return render_primitives(light_icon_primitives())


def scaled(master: Image.Image, px: int) -> Image.Image:
    out = master.resize((px, px), Image.LANCZOS)
    assert out.mode == "RGBA", "icons must stay 8-bit/color RGBA"
    return out


def render_icon_at_size(master: Image.Image, px: int) -> Image.Image:
    """Render the optical-size variant appropriate for a fixed-size output."""
    source = render_primitives(compact_icon_primitives()) if px == 16 else master
    return scaled(source, px)


LINUX_SIZES = [16, 24, 32, 48, 64, 128, 256]
ICONSET_BASES = [16, 32, 128, 256, 512]


def png_bytes(image: Image.Image) -> bytes:
    """Encode an RGBA image as a PNG without an intermediate resize."""
    output = BytesIO()
    image.save(output, format="PNG")
    return output.getvalue()


def write_ico(path: Path, images: list[tuple[int, Image.Image]]) -> None:
    """Write a PNG-backed ICO whose every size comes from the master directly."""
    payloads = [(size, png_bytes(image)) for size, image in images]
    offset = 6 + 16 * len(payloads)
    entries: list[bytes] = []
    for size, payload in payloads:
        encoded_size = 0 if size == 256 else size
        entries.append(
            struct.pack(
                "<BBBBHHII",
                encoded_size,
                encoded_size,
                0,
                0,
                1,
                32,
                len(payload),
                offset,
            )
        )
        offset += len(payload)
    path.write_bytes(
        struct.pack("<HHH", 0, 1, len(payloads))
        + b"".join(entries)
        + b"".join(payload for _, payload in payloads)
    )


def ico_png_encodings(path: Path) -> dict[tuple[int, int], tuple[int, int]]:
    """Return each embedded PNG's bit depth and color type by ICO size."""
    raw = path.read_bytes()
    if len(raw) < 6 or struct.unpack_from("<HH", raw, 0) != (0, 1):
        raise ValueError(f"{path} has no valid ICO header")
    count = struct.unpack_from("<H", raw, 4)[0]
    encodings: dict[tuple[int, int], tuple[int, int]] = {}
    for index in range(count):
        entry = struct.unpack_from("<BBBBHHII", raw, 6 + 16 * index)
        width, height = entry[0] or 256, entry[1] or 256
        length, offset = entry[6], entry[7]
        payload = raw[offset : offset + length]
        if (
            len(payload) < 26
            or payload[:8] != b"\x89PNG\r\n\x1a\n"
            or payload[12:16] != b"IHDR"
        ):
            raise ValueError(f"{path} size {width}x{height} is not PNG-backed")
        encodings[(width, height)] = (payload[24], payload[25])
    return encodings


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
        render_icon_at_size(master, px).save(path)
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
            render_icon_at_size(master, px).save(iconset_dir / name)
            written.append(
                (iconset_dir / name, f"packaging/macos/kettle.iconset/{name}")
            )
        if not quiet:
            print(f"wrote icon_{base}x{base}.png (+@2x)")

    ico_path.parent.mkdir(parents=True, exist_ok=True)
    write_ico(
        ico_path,
        [(px, render_icon_at_size(master, px)) for px in LINUX_SIZES],
    )
    written.append((ico_path, "packaging/windows/kettle.ico"))
    if not quiet:
        print(f"wrote {ico_path}")

    # Validate the container written above rather than trusting directory-entry
    # assembly: a malformed count makes the entire ICO ambiguous to consumers.
    with ico_path.open("rb") as stream:
        header = stream.read(6)
    count = struct.unpack("<H", header[4:6])[0]
    if count != len(LINUX_SIZES):
        raise RuntimeError(
            f"{ico_path} has {count} images, expected {len(LINUX_SIZES)}"
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
            try:
                encodings_match = ico_png_encodings(a) == ico_png_encodings(b)
            except ValueError:
                return False
            if left_sizes != right_sizes or not encodings_match:
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
