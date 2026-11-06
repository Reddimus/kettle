#!/usr/bin/env python3
"""Exercise pinned bidi table generation, verification and stale-output refusal."""

from contextlib import redirect_stdout
import importlib.util
from io import StringIO
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("generate-bidi-tables.py")
spec = importlib.util.spec_from_file_location("bidi_generator", SCRIPT)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class BidiTables(unittest.TestCase):
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

    def test_inputs_give_the_expected_tables(self):
        self.assertEqual(len(generator.mirrors()), 428)
        forms = dict(generator.forms())
        self.assertEqual(forms[0x628], [0xFE8F, 0xFE90, 0xFE91, 0xFE92])
        self.assertEqual(forms[0x627], [0xFE8D, 0xFE8E, None, None])
        # The lam-alef ligatures decompose to two letters: not single forms.
        self.assertNotIn(0x644, [base for base, slots in forms.items() if 0xFEFB in slots])

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
