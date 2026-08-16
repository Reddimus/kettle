#!/usr/bin/env python3
"""Regression tests for gen-icons.py."""

from __future__ import annotations

import binascii
from contextlib import ExitStack, redirect_stdout
from io import BytesIO, StringIO
import importlib.util
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib


sys.dont_write_bytecode = True

SCRIPT = Path(__file__).with_name("gen-icons.py")
SPEC = importlib.util.spec_from_file_location("gen_icons", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def flattened_pixels(image):
    """Return pixels across Pillow versions without a deprecation warning."""
    accessor = getattr(image, "get_flattened_data", None)
    return accessor() if accessor is not None else image.getdata()


def accent_components(image) -> list[int]:
    """Return 4-connected regions closer to the accent than the tile color."""
    background = MODULE.DARK_BACKGROUND
    accent = MODULE.DARK_ACCENT
    remaining = set()
    for y in range(image.height):
        for x in range(image.width):
            pixel = image.getpixel((x, y))
            accent_distance = sum((pixel[i] - accent[i]) ** 2 for i in range(3))
            background_distance = sum(
                (pixel[i] - background[i]) ** 2 for i in range(3)
            )
            if pixel[3] > 64 and accent_distance < background_distance:
                remaining.add((x, y))

    components = []
    while remaining:
        pending = [remaining.pop()]
        size = 1
        while pending:
            x, y = pending.pop()
            for neighbor in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
                if neighbor in remaining:
                    remaining.remove(neighbor)
                    pending.append(neighbor)
                    size += 1
        components.append(size)
    return sorted(components)


def png_chunk(kind: bytes, data: bytes) -> bytes:
    checksum = binascii.crc32(kind + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", checksum)


def encode_rgba16(image) -> bytes:
    """Encode exact 8-bit RGBA values as lossless 16-bit RGBA."""
    rgba = image.convert("RGBA")
    width, height = rgba.size
    pixels = rgba.tobytes()
    stride = width * 4
    rows = []
    for y in range(height):
        row = pixels[y * stride : (y + 1) * stride]
        rows.append(b"\x00" + b"".join(bytes((value, value)) for value in row))
    ihdr = struct.pack(">IIBBBBB", width, height, 16, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(b"".join(rows)))
        + png_chunk(b"IEND", b"")
    )


def encode_palette_rgba8(image) -> bytes:
    """Encode exact RGBA pixels as an indexed PNG with per-color alpha."""
    rgba = image.convert("RGBA")
    raw = rgba.tobytes()
    pixels = [tuple(raw[index : index + 4]) for index in range(0, len(raw), 4)]
    colors = list(dict.fromkeys(pixels))
    if len(colors) > 256:
        raise ValueError("fixture has too many colors for an exact palette PNG")
    indexes = {color: index for index, color in enumerate(colors)}
    paletted = MODULE.Image.new("P", rgba.size)
    paletted.putdata([indexes[color] for color in pixels])
    palette = [component for color in colors for component in color[:3]]
    paletted.putpalette(palette + [0] * (768 - len(palette)))
    encoded = BytesIO()
    paletted.save(
        encoded,
        format="PNG",
        transparency=bytes(color[3] for color in colors),
    )
    return encoded.getvalue()


def replace_ico_resolution(raw: bytes, size: int) -> bytes:
    """Change one ICO PNG payload while preserving every other resolution."""
    count = struct.unpack_from("<H", raw, 4)[0]
    entries = [
        list(struct.unpack_from("<BBBBHHII", raw, 6 + 16 * index))
        for index in range(count)
    ]
    payloads = [raw[entry[7] : entry[7] + entry[6]] for entry in entries]
    target = next(
        index
        for index, entry in enumerate(entries)
        if (entry[0] or 256, entry[1] or 256) == (size, size)
    )
    with MODULE.Image.open(BytesIO(payloads[target])) as source:
        changed = source.convert("RGBA")
    changed.putpixel((0, 0), (255, 0, 0, 255))
    encoded = BytesIO()
    changed.save(encoded, format="PNG")
    payloads[target] = encoded.getvalue()

    offset = 6 + 16 * count
    packed_entries = []
    for entry, payload in zip(entries, payloads):
        entry[6] = len(payload)
        entry[7] = offset
        packed_entries.append(struct.pack("<BBBBHHII", *entry))
        offset += len(payload)
    return raw[:6] + b"".join(packed_entries) + b"".join(payloads)


class MissingPillowTests(unittest.TestCase):
    def test_check_skips_before_annotations_are_evaluated(self):
        result = subprocess.run(
            [sys.executable, "-S", str(SCRIPT), "--check"],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("SKIPPED: Pillow is not installed", result.stdout)


@unittest.skipIf(MODULE.Image is None, "Pillow is not installed")
class PillowIconTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temporary.name)
        MODULE.emit(
            cls.root / "linux",
            cls.root / "iconset",
            cls.root / "windows" / "kettle.ico",
            quiet=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def test_normal_main_path_generates_and_verifies_ico(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with ExitStack() as stack:
                stack.enter_context(patch.object(MODULE, "LINUX", root / "linux"))
                stack.enter_context(
                    patch.object(MODULE, "ICONSET", root / "iconset")
                )
                stack.enter_context(
                    patch.object(MODULE, "ICO", root / "windows" / "kettle.ico")
                )
                stack.enter_context(patch.object(sys, "argv", [str(SCRIPT)]))
                stack.enter_context(redirect_stdout(StringIO()))
                self.assertEqual(MODULE.main(), 0)

    def test_macos_keeps_legacy_icon_and_has_native_composer_source(self):
        linux = MODULE.render_master()

        self.assertEqual(linux.getpixel((0, 0))[3], 0)

        legacy = self.root / "iconset" / "icon_512x512@2x.png"
        with MODULE.Image.open(legacy) as icon:
            self.assertEqual(icon.getpixel((0, 0))[3], 0)

        document = self.root / "AppIcon.icon" / "icon.json"
        artwork = self.root / "AppIcon.icon" / "Assets" / "kettle.svg"
        self.assertTrue(document.is_file())
        self.assertEqual(document.read_text(), MODULE.icon_composer_json_source())
        self.assertEqual(artwork.read_text(), MODULE.icon_composer_svg_source())
        self.assertIn('"solid"', document.read_text())
        self.assertIn(
            f'"solid" : "{MODULE.srgb_source(MODULE.DARK_BACKGROUND)}"',
            document.read_text(),
        )
        self.assertIn('"specular" : false', document.read_text())
        self.assertIn('"enabled" : false', document.read_text())
        self.assertIn('"scale" : 2', document.read_text())
        self.assertIn('"translation-in-points"', document.read_text())
        self.assertNotIn('x="0" y="0"', artwork.read_text())

    def test_macos_foreground_is_one_borderless_shared_terminal_mark(self):
        composer = MODULE.icon_composer_primitives()
        self.assertEqual(composer, MODULE.kettle_primitives())
        self.assertEqual(MODULE.icon_primitives()[1:], composer)

        # macOS owns the dark field and outer mask. No second face, border,
        # or pre-rounded tile may be reintroduced into the foreground: that
        # double curve is what looked nonparallel in the Dock.
        for primitive in composer:
            self.assertNotIsInstance(primitive, MODULE.RectPrimitive)
            self.assertLessEqual(primitive.stroke_width, 48)

        # Measure the geometry itself. A hand-maintained bounds tuple let an
        # artwork change keep passing with a stale safe-area assertion.
        left, top, right, bottom = MODULE.primitive_bounds(composer)
        self.assertGreaterEqual(left, 56)
        self.assertGreaterEqual(top, 144)
        self.assertLessEqual(right, MODULE.CANVAS - 56)
        self.assertLessEqual(bottom, MODULE.CANVAS - 144)
        self.assertAlmostEqual((left + right) / 2, MODULE.CANVAS / 2, delta=2)
        self.assertGreater(right - left, 2 * (bottom - top))

        # Pin the five semantic strokes rather than a font-rendered string:
        # prompt, left parenthesis, underscore, right parenthesis, tilde.
        # A literal text node would change shape with the build host's fonts.
        self.assertEqual(len(composer), 5)
        artwork = MODULE.icon_composer_svg_source()
        self.assertNotIn("<text", artwork)
        self.assertEqual(artwork.count("<polyline"), 2)
        self.assertEqual(artwork.count("<path"), 3)
        self.assertNotIn("stroke-opacity", artwork)

        # Curved typography is semantic, not a many-segment approximation. The
        # rejected shape used seven rounded line segments per parenthesis and
        # fused into a bulbous vessel after downsampling.
        self.assertIsInstance(composer[0], MODULE.PolylinePrimitive)
        self.assertIsInstance(composer[2], MODULE.PolylinePrimitive)
        for index in (1, 3, 4):
            self.assertIsInstance(composer[index], MODULE.CubicPrimitive)
        self.assertEqual(composer[2].points, ((210, 292), (290, 292)))
        self.assertEqual(composer[2].linecap, "butt")
        self.assertTrue(
            all(
                primitive.linecap == "round"
                for index, primitive in enumerate(composer)
                if index != 2
            )
        )

        # Every glyph stays fully opaque. Partial-alpha punctuation looked
        # disabled and lost contrast at small fixed sizes.
        self.assertTrue(
            all(primitive.stroke == MODULE.DARK_ACCENT for primitive in composer)
        )

    def test_16_pixel_artifact_uses_a_distinct_compact_terminal_mark(self):
        master = MODULE.render_master()
        icon = MODULE.render_icon_at_size(master, 16)
        self.assertEqual(len(MODULE.compact_terminal_primitives()), 2)
        self.assertEqual(len(accent_components(icon)), 2)
        self.assertNotEqual(icon.tobytes(), MODULE.scaled(master, 16).tobytes())

    def test_full_terminal_mark_is_five_components_in_the_24_pixel_artifact(self):
        icon = MODULE.render_icon_at_size(MODULE.render_master(), 24)
        self.assertEqual(len(MODULE.kettle_primitives()), 5)
        self.assertEqual(len(accent_components(icon)), 5)

    def test_light_preview_is_an_exact_color_inverse_of_dark_mode(self):
        dark = MODULE.render_master()
        light = MODULE.render_light_master()

        center = (MODULE.CANVAS * 2, MODULE.CANVAS * 2)
        self.assertEqual(dark.getpixel(center), MODULE.DARK_BACKGROUND)
        self.assertEqual(light.getpixel(center), MODULE.LIGHT_BACKGROUND)

        # Every stroke is composited into the opaque tile; neither variant may
        # reveal the desktop through the face.
        for image in (dark, light):
            self.assertEqual(
                image.getpixel((166 * MODULE.S, 256 * MODULE.S))[3],
                255,
            )

        # The supersampled masters contain no antialiasing yet, so every pixel
        # must be exactly transparent or one of the two swapped palette colors.
        # This also proves identical geometry: changing a path in only one
        # variant introduces a forbidden pixel pair at that location.
        pairs = set(zip(flattened_pixels(dark), flattened_pixels(light)))
        self.assertEqual(
            pairs,
            {
                ((0, 0, 0, 0), (0, 0, 0, 0)),
                (MODULE.DARK_BACKGROUND, MODULE.DARK_ACCENT),
                (MODULE.DARK_ACCENT, MODULE.DARK_BACKGROUND),
            },
        )

    def test_svg_sources_are_generated_from_the_shared_geometry(self):
        linux = self.root / "linux" / "kettle.svg"

        self.assertEqual(linux.read_text(), MODULE.svg_source())

        composer = self.root / "AppIcon.icon" / "Assets" / "kettle.svg"
        self.assertEqual(composer.read_text(), MODULE.icon_composer_svg_source())
        self.assertNotIn('width="512" height="512" rx="0"', composer.read_text())

        mutant = self.root / "composer-mutant.svg"
        mutated = composer.read_text().replace(
            'points="82,218 116,256 82,294"',
            'points="83,218 116,256 82,294"',
            1,
        )
        self.assertNotEqual(mutated, composer.read_text())
        mutant.write_bytes(mutated.encode("utf-8"))
        self.assertFalse(MODULE.same_artifact(composer, mutant))

    def test_svg_generation_never_uses_platform_text_newlines(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(
                Path,
                "write_text",
                side_effect=AssertionError("SVGs must be written as exact UTF-8 bytes"),
            ):
                MODULE.emit(
                    root / "linux",
                    root / "iconset",
                    root / "windows" / "kettle.ico",
                    quiet=True,
                )
            for svg in [
                root / "linux" / "kettle.svg",
                root / "AppIcon.icon" / "Assets" / "kettle.svg",
            ]:
                self.assertNotIn(b"\r", svg.read_bytes())

    def test_svg_and_raster_paths_consume_the_same_primitives(self):
        with patch.object(MODULE, "icon_primitives", wraps=MODULE.icon_primitives) as model:
            MODULE.svg_source()
            MODULE.render_master()
        self.assertEqual(model.call_args_list, [unittest.mock.call(), unittest.mock.call()])

    def test_compatibility_wrapper_forwards_arguments_and_status(self):
        if os.name == "nt" or shutil.which("bash") is None:
            self.skipTest("the compatibility wrapper is Unix-only")
        result = subprocess.run(
            ["bash", str(SCRIPT.with_name("gen-icons.sh")), "--check", "--require-tooling"],
            cwd=SCRIPT.parent.parent,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("tracked artifacts match the generator", result.stdout)

    def test_nonlargest_ico_resolution_is_compared(self):
        original = self.root / "windows" / "kettle.ico"
        mutant = self.root / "windows" / "mutant.ico"
        mutant.write_bytes(replace_ico_resolution(original.read_bytes(), 16))
        with MODULE.Image.open(original) as left, MODULE.Image.open(mutant) as right:
            self.assertEqual(
                left.ico.getimage((256, 256)).tobytes(),
                right.ico.getimage((256, 256)).tobytes(),
            )
        self.assertFalse(MODULE.same_pixels(original, mutant))

    def test_ico_sizes_match_the_direct_fixed_size_rasters(self):
        ico = self.root / "windows" / "kettle.ico"
        self.assertEqual(
            MODULE.ico_png_encodings(ico),
            {(size, size): (8, 6) for size in MODULE.LINUX_SIZES},
        )
        with MODULE.Image.open(ico) as container:
            for size in MODULE.LINUX_SIZES:
                linux = self.root / "linux" / f"kettle-{size}.png"
                with MODULE.Image.open(linux) as expected:
                    actual = container.ico.getimage((size, size))
                    self.assertEqual(actual.mode, "RGBA")
                    self.assertEqual(actual.tobytes(), expected.tobytes())

    def test_bmp_backed_ico_is_drift_not_an_uncaught_error(self):
        original = self.root / "windows" / "kettle.ico"
        bmp = self.root / "windows" / "bmp-backed.ico"
        MODULE.scaled(MODULE.render_master(), 256).save(
            bmp,
            format="ICO",
            bitmap_format="bmp",
            sizes=[(size, size) for size in MODULE.LINUX_SIZES],
        )
        self.assertFalse(MODULE.same_pixels(original, bmp))

    def test_icon_composer_fill_is_derived_from_the_palette(self):
        alternate = (0x24, 0x28, 0x3B, 255)
        with patch.object(MODULE, "DARK_BACKGROUND", alternate):
            document = MODULE.icon_composer_json_source()
        self.assertIn(MODULE.srgb_source(alternate), document)
        self.assertNotIn(MODULE.srgb_source((0x1A, 0x1B, 0x26, 255)), document)

    def test_full_and_compact_marks_both_follow_the_live_accent(self):
        alternate = (0x9E, 0xCE, 0x6A, 255)
        with patch.object(MODULE, "DARK_ACCENT", alternate):
            full = MODULE.kettle_primitives()
            compact = MODULE.compact_terminal_primitives()
        self.assertTrue(all(primitive.stroke == alternate for primitive in full))
        self.assertTrue(
            all(primitive.stroke == alternate for primitive in compact)
        )

    def test_missing_ico_resolutions_are_rejected(self):
        original = self.root / "windows" / "kettle.ico"
        four_sizes = self.root / "windows" / "four-sizes.ico"
        MODULE.scaled(MODULE.render_master(), 256).save(
            four_sizes,
            format="ICO",
            sizes=[(32, 32), (64, 64), (128, 128), (256, 256)],
        )
        self.assertFalse(MODULE.same_pixels(original, four_sizes))

    def test_png_bit_depth_is_compared_without_hiding_pixels(self):
        original = self.root / "linux" / "kettle-32.png"
        mutant = self.root / "linux" / "kettle-32-rgba16.png"
        with MODULE.Image.open(original) as image:
            expected_pixels = image.convert("RGBA").tobytes()
            mutant.write_bytes(encode_rgba16(image))
        with MODULE.Image.open(mutant) as image:
            self.assertEqual(image.convert("RGBA").tobytes(), expected_pixels)
        self.assertEqual(MODULE.png_encoding(original), (8, 6))
        self.assertEqual(MODULE.png_encoding(mutant), (16, 6))
        self.assertFalse(MODULE.same_pixels(original, mutant))

    def test_png_mode_is_compared_without_hiding_pixels(self):
        original = self.root / "linux" / "kettle-16.png"
        mutant = self.root / "linux" / "kettle-16-palette.png"
        with MODULE.Image.open(original) as image:
            expected_pixels = image.convert("RGBA").tobytes()
            mutant.write_bytes(encode_palette_rgba8(image))
        with MODULE.Image.open(mutant) as image:
            self.assertEqual(image.mode, "P")
            self.assertEqual(image.convert("RGBA").tobytes(), expected_pixels)
        self.assertEqual(MODULE.png_encoding(original), (8, 6))
        self.assertEqual(MODULE.png_encoding(mutant), (8, 3))
        self.assertFalse(MODULE.same_pixels(original, mutant))


if __name__ == "__main__":
    unittest.main()
