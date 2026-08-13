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
        self.assertIn('"specular" : false', document.read_text())
        self.assertIn('"enabled" : false', document.read_text())
        self.assertIn('"scale" : 2', document.read_text())
        self.assertIn('"translation-in-points"', document.read_text())
        self.assertNotIn('x="0" y="0"', artwork.read_text())

    def test_macos_face_keeps_a_parallel_optical_safe_area(self):
        left, top, right, bottom = MODULE.FACE_BOX
        self.assertEqual(left + right, MODULE.CANVAS)
        self.assertEqual(top + bottom, MODULE.CANVAS)
        self.assertEqual(right - left, bottom - top)

        # Xcode 26 maps the 200% foreground into its 256 px compatibility
        # rendering with this centered 0.4 scale. The native mask begins near
        # x=26 and has a roughly 45 px corner radius. Pin both the visible rim
        # and the concentric-radius relationship that the Dock screenshot can
        # otherwise regress to a two-pixel, clipped-looking outline.
        compiled = lambda coordinate: 128 + (
            coordinate - 256
        ) * MODULE.XCODE_26_COMPILED_SCALE
        compiled_face = (round(compiled(left)), round(compiled(right)) - 1)
        self.assertEqual(compiled_face, (52, 203))
        self.assertEqual(MODULE.FACE_RADIUS, 47)
        rim = compiled(left) - MODULE.XCODE_26_MASK_EDGE
        inner_radius = MODULE.FACE_RADIUS * MODULE.XCODE_26_COMPILED_SCALE
        self.assertGreaterEqual(rim, 24)
        self.assertAlmostEqual(
            rim + inner_radius, MODULE.XCODE_26_MASK_RADIUS, delta=0.25
        )

        prompt_left = (
            min(x for x, _ in MODULE.PROMPT_POINTS) - MODULE.PROMPT_STROKE / 2
        )
        prompt_top = min(y for _, y in MODULE.PROMPT_POINTS) - MODULE.PROMPT_STROKE / 2
        prompt_bottom = max(y for _, y in MODULE.PROMPT_POINTS) + MODULE.PROMPT_STROKE / 2
        self.assertEqual(
            (prompt_left - left, prompt_top - top, bottom - prompt_bottom),
            (72, 84, 84),
        )
        caret_clearance = right - MODULE.ICON_COMPOSER_CARET_BOX[2]
        self.assertEqual(caret_clearance, 48)
        self.assertGreaterEqual(caret_clearance, MODULE.ICON_COMPOSER_MIN_GLYPH_CLEARANCE)

    def test_svg_sources_are_generated_from_the_shared_geometry(self):
        linux = self.root / "linux" / "kettle.svg"

        self.assertEqual(linux.read_text(), MODULE.svg_source())

        composer = self.root / "AppIcon.icon" / "Assets" / "kettle.svg"
        self.assertEqual(composer.read_text(), MODULE.icon_composer_svg_source())
        self.assertNotIn('width="512" height="512" rx="0"', composer.read_text())

        mutant = self.root / "composer-mutant.svg"
        face_radius = f'rx="{MODULE.FACE_RADIUS}"'
        mutant_radius = f'rx="{MODULE.FACE_RADIUS - 1}"'
        mutated = composer.read_text().replace(face_radius, mutant_radius, 1)
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
