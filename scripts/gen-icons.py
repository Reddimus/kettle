#!/usr/bin/env python3
"""kettle — regenerate every icon source and raster from one geometry model.

This script owns the shared ``>_`` terminal mark and emits the compatible
pre-rounded Linux/Windows vector plus native light and dark Icon Composer
artwork that Apple's asset compiler masks for every macOS surface. It then
renders the committed platform rasters with Pillow at 4x
supersampling (2048 px canvas, LANCZOS downscale) and writes:

  - packaging/linux/kettle.svg                            (generated source)
  - packaging/macos/AppIcon.icon/{icon.json,Assets/kettle-{light,dark}.svg}
    plus Assets/kettle.svg as an unreferenced dark compatibility alias
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

# One two-color identity. Light appearance is the exact color swap.
DARK_FACE = (0x1A, 0x1B, 0x26, 255)
DARK_ACCENT = (0x7A, 0xA2, 0xF7, 255)
LIGHT_FACE = DARK_ACCENT
LIGHT_ACCENT = DARK_FACE

S = 4  # supersample factor over the 512 viewBox → 2048 px canvas

CANVAS = 512
OUTER_BOX = (32, 32, 480, 480)
OUTER_RADIUS = 92
FACE_BOX = (54, 54, 458, 458)
FACE_RADIUS = 70
MAC_FACE_BOX = (48, 48, 464, 464)
MAC_FACE_RADIUS = 48
PROMPT_POINTS = ((128, 172), (228, 256), (128, 340))
PROMPT_STROKE = 38
CARET_BOX = (270, 320, 387, 352)
# Icon Composer maps SVG layers into a conservative foreground safe area. At
# 2x the inset face lands 24 px inside the system-owned 256 px mask; its 24 px
# rendered radius follows the same offset from the outer continuous corner.
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


Primitive = RectPrimitive | PolylinePrimitive


def primitive_bounds(
    primitives: tuple[PolylinePrimitive, ...],
) -> tuple[int, int, int, int]:
    """Return pixel-conservative bounds for supplied foreground strokes."""
    xs: list[int] = []
    ys: list[int] = []
    for primitive in primitives:
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
    if not xs:
        raise ValueError("cannot measure an empty primitive set")
    return (
        math.floor(min(xs)),
        math.floor(min(ys)),
        math.ceil(max(xs)),
        math.ceil(max(ys)),
    )


def terminal_primitives(
    accent: tuple[int, int, int, int] | None = None,
) -> tuple[PolylinePrimitive, ...]:
    """The historical ``>_`` mark, drawn as geometry rather than a font."""
    accent = DARK_ACCENT if accent is None else accent
    return (
        PolylinePrimitive(PROMPT_POINTS, accent, PROMPT_STROKE),
        PolylinePrimitive(
            ((CARET_BOX[0], CARET_BOX[1]), (CARET_BOX[2], CARET_BOX[1])),
            accent,
            CARET_BOX[3] - CARET_BOX[1],
            "round",
        ),
    )


def compact_terminal_primitives(
    accent: tuple[int, int, int, int] | None = None,
) -> tuple[PolylinePrimitive, ...]:
    """Thicker, wider ``>_`` strokes for 16 px and 24 px rasters."""
    accent = DARK_ACCENT if accent is None else accent
    return (
        PolylinePrimitive(((160, 208), (202, 256), (160, 304)), accent, 42),
        PolylinePrimitive(((246, 292), (358, 292)), accent, 34, "butt"),
    )


def compact_icon_primitives() -> tuple[Primitive, ...]:
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, DARK_ACCENT),
        RectPrimitive(FACE_BOX, FACE_RADIUS, DARK_FACE),
        *compact_terminal_primitives(DARK_ACCENT),
    )


def icon_primitives() -> tuple[Primitive, ...]:
    """Return the compatible geometry consumed by Linux/Windows outputs."""
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, DARK_ACCENT),
        RectPrimitive(FACE_BOX, FACE_RADIUS, DARK_FACE),
        *terminal_primitives(DARK_ACCENT),
    )


def light_icon_primitives() -> tuple[Primitive, ...]:
    """Return an exact color inversion of the dark shared geometry."""
    return (
        RectPrimitive(OUTER_BOX, OUTER_RADIUS, LIGHT_ACCENT),
        RectPrimitive(FACE_BOX, FACE_RADIUS, LIGHT_FACE),
        *terminal_primitives(LIGHT_ACCENT),
    )


def icon_composer_primitives(dark: bool) -> tuple[Primitive, ...]:
    """Adaptive field, inset face and mark; macOS supplies only the mask."""
    outer, face = (DARK_ACCENT, DARK_FACE) if dark else (LIGHT_ACCENT, LIGHT_FACE)
    return (
        RectPrimitive((0, 0, CANVAS, CANVAS), 0, outer),
        RectPrimitive(MAC_FACE_BOX, MAC_FACE_RADIUS, face),
        *terminal_primitives(outer),
    )


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
    points = " ".join(f"{x},{y}" for x, y in primitive.points)
    return (
        f'  <polyline points="{points}" fill="none" '
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


def icon_composer_svg_source(dark: bool) -> str:
    body = [
        primitive_svg(primitive) for primitive in icon_composer_primitives(dark)
    ]
    appearance = "dark" if dark else "light"
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        f'<!-- {appearance} artwork for AppIcon.icon. macOS supplies the outer mask. -->\n'
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" '
        f'height="{CANVAS}" viewBox="0 0 {CANVAS} {CANVAS}">\n'
        + "\n".join(body)
        + "\n</svg>\n"
    )


def icon_composer_json_source() -> str:
    return f"""{{
  "fill" : {{
    "solid" : "{srgb_source(LIGHT_ACCENT)}"
  }},
  "groups" : [
    {{
      "layers" : [
        {{
          "hidden" : false,
          "image-name-specializations" : [
            {{
              "value" : "kettle-light.svg"
            }},
            {{
              "appearance" : "dark",
              "value" : "kettle-dark.svg"
            }}
          ],
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
            points = [(round(x * S), round(y * S)) for x, y in primitive.points]
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


def render_icon_at_size(
    master: Image.Image,
    px: int,
    compact_master: Image.Image | None = None,
) -> Image.Image:
    """Render the optical-size variant appropriate for a fixed-size output."""
    source = master
    if px == 16:
        source = (
            compact_master
            if compact_master is not None
            else render_primitives(compact_icon_primitives())
        )
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
    compact_master = render_primitives(compact_icon_primitives())
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
    for appearance in ("light", "dark"):
        composer_svg = composer_assets / f"kettle-{appearance}.svg"
        composer_svg.write_bytes(
            icon_composer_svg_source(appearance == "dark").encode("utf-8")
        )
        written.append(
            (
                composer_svg,
                f"packaging/macos/AppIcon.icon/Assets/kettle-{appearance}.svg",
            )
        )
    legacy_composer_svg = composer_assets / "kettle.svg"
    legacy_composer_svg.write_bytes(icon_composer_svg_source(True).encode("utf-8"))
    written.append(
        (
            legacy_composer_svg,
            "packaging/macos/AppIcon.icon/Assets/kettle.svg",
        )
    )
    composer_json = icon_composer / "icon.json"
    composer_json.write_bytes(icon_composer_json_source().encode("utf-8"))
    written.append((composer_json, "packaging/macos/AppIcon.icon/icon.json"))

    for px in LINUX_SIZES:
        path = linux_dir / f"kettle-{px}.png"
        render_icon_at_size(master, px, compact_master).save(path)
        written.append((path, f"packaging/linux/kettle-{px}.png"))
        if not quiet:
            print(f"wrote {path}")

    light_path = linux_dir / "kettle-light-256.png"
    scaled(render_light_master(), 256).save(light_path)
    written.append((light_path, "packaging/linux/kettle-light-256.png"))
    if not quiet:
        print(f"wrote {light_path}")

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
            render_icon_at_size(master, px, compact_master).save(iconset_dir / name)
            written.append(
                (iconset_dir / name, f"packaging/macos/kettle.iconset/{name}")
            )
        if not quiet:
            print(f"wrote icon_{base}x{base}.png (+@2x)")

    ico_path.parent.mkdir(parents=True, exist_ok=True)
    write_ico(
        ico_path,
        [
            (px, render_icon_at_size(master, px, compact_master))
            for px in LINUX_SIZES
        ],
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
