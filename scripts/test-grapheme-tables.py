#!/usr/bin/env python3
"""Exercise pinned Unicode table generation, verification and stale-output refusal."""

from contextlib import redirect_stdout
import importlib.util
from io import StringIO
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("generate-grapheme-tables.py")
spec = importlib.util.spec_from_file_location("grapheme_generator", SCRIPT)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class GraphemeTables(unittest.TestCase):
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
