#!/usr/bin/env python3
"""Exercise pinned Unicode table generation, verification and stale-output refusal."""

from contextlib import redirect_stdout
import importlib.util
from io import StringIO
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("generate-grapheme-tables.py")
spec = importlib.util.spec_from_file_location("grapheme_generator", SCRIPT)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class GraphemeTables(unittest.TestCase):
    def generated_ranges(self, output, name):
        match = re.search(rf"\bconst {name}:.*?= &\[\n(.*?)\n\];", output, re.S)
        self.assertIsNotNone(match, f"missing generated {name} table")
        return [
            (int(a, 16), int(b, 16))
            for a, b in re.findall(r"\(0x([0-9A-F]+), 0x([0-9A-F]+)\)", match[1])
        ]

    def invoke(self, output, check=False):
        arguments = [str(SCRIPT), *(["--check"] if check else [])]
        with patch.object(generator, "OUTPUT", output), patch("sys.argv", arguments):
            with redirect_stdout(StringIO()):
                generator.main()

    def test_generation_reproduces_the_checked_in_tables_and_verifies_them(self):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / "tables.rs"
            self.invoke(output)
            self.assertEqual(output.read_bytes(), generator.OUTPUT.read_bytes())
            self.invoke(output, check=True)

    def test_width_properties_include_extend_mc_and_registered_variations(self):
        marks = list(generator.spacing_marks())
        self.assertTrue(any(a <= 0xBBE <= b for a, b, _ in marks))
        self.assertTrue(any(a <= 0x93E <= b for a, b, _ in marks))
        self.assertFalse(any(a <= 0xE33 <= b for a, b, _ in marks))
        variations = list(generator.records("emoji-variation-bases.txt"))
        self.assertEqual(sum(b - a + 1 for a, b, _ in variations), 371)
        self.assertTrue(any(a <= 0x231A <= b for a, b, _ in variations))
        self.assertTrue(any(a <= 0x1F21A <= b for a, b, _ in variations))

    def test_virama_width_exceptions_preserve_full_spacing_mark_classification(self):
        output = generator.generate()
        exceptions = {
            cp
            for a, b in self.generated_ranges(output, "VIRAMA_SPACING_MARK")
            for cp in range(a, b + 1)
        }
        self.assertEqual(
            exceptions, {0x1B44, 0xA9C0, 0x111C0, 0x11235, 0x1134D, 0x116B6}
        )
        marks = {
            cp
            for a, b in self.generated_ranges(output, "SPACING_MARK")
            for cp in range(a, b + 1)
        }
        self.assertLessEqual(exceptions, marks)
        self.assertTrue({0xBBE, 0x1B43, 0x1BAA, 0x1BF2, 0x1BF3} <= marks)

    def test_virama_table_intersects_partial_ranges_without_including_neighbors(self):
        original_records = generator.records

        def records(name):
            if name == "IndicSyllabicCategory.txt":
                return iter(
                    [
                        (0x1B44, 0x1B44, ["Virama"]),
                        (3, 8, ["Virama"]),
                        (12, 18, ["Invisible_Stacker"]),
                        (19, 20, ["Pure_Killer"]),
                    ]
                )
            return original_records(name)

        marks = [(0x1B43, 0x1B44, ""), (16, 20, ""), (2, 4, ""), (8, 10, "")]
        with (
            patch.object(generator, "records", records),
            patch.object(generator, "spacing_marks", side_effect=lambda: iter(marks)),
        ):
            output = generator.generate()
        self.assertEqual(
            self.generated_ranges(output, "VIRAMA_SPACING_MARK"),
            [(3, 4), (8, 8), (16, 18), (0x1B44, 0x1B44)],
        )
        self.assertEqual(
            self.generated_ranges(output, "SPACING_MARK"),
            [(2, 4), (8, 10), (16, 20), (0x1B43, 0x1B44)],
        )

    def test_verification_refuses_missing_and_modified_tables(self):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / "tables.rs"
            with self.assertRaises(SystemExit):
                self.invoke(output, check=True)
            self.invoke(output)
            output.write_text("// stale output\n", encoding="utf-8")
            with self.assertRaises(SystemExit):
                self.invoke(output, check=True)


if __name__ == "__main__":
    unittest.main()
