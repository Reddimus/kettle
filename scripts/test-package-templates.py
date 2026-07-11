#!/usr/bin/env python3
"""Hermetic tests for release package-template rendering."""

from __future__ import annotations

import hashlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("render-package-templates.py")


class PackageTemplateTests(unittest.TestCase):
    def run_renderer(
        self, root: Path, version: str = "9.8.7"
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--version",
                version,
                "--macos-archive",
                str(root / "mac.zip"),
                "--linux-x86-64-archive",
                str(root / "linux.tar.gz"),
                "--output-dir",
                str(root / "out"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_renders_exact_archive_hashes_deterministically(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            macos = b"macos release archive\n"
            linux = b"linux release archive\n"
            (root / "mac.zip").write_bytes(macos)
            (root / "linux.tar.gz").write_bytes(linux)

            first = self.run_renderer(root)
            self.assertEqual(first.returncode, 0, first.stderr)
            formula = (root / "out" / "kettle.rb").read_text(encoding="utf-8")
            pkgbuild = (root / "out" / "PKGBUILD").read_text(encoding="utf-8")
            first_bytes = (formula.encode(), pkgbuild.encode())

            self.assertIn('version "9.8.7"', formula)
            self.assertIn(hashlib.sha256(macos).hexdigest(), formula)
            self.assertIn(hashlib.sha256(linux).hexdigest(), formula)
            self.assertIn("pkgver=9.8.7", pkgbuild)
            self.assertIn(hashlib.sha256(linux).hexdigest(), pkgbuild)
            self.assertNotIn("@VERSION@", formula + pkgbuild)

            second = self.run_renderer(root)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first_bytes[0], (root / "out" / "kettle.rb").read_bytes())
            self.assertEqual(first_bytes[1], (root / "out" / "PKGBUILD").read_bytes())

    def test_rejects_non_stable_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "mac.zip").write_bytes(b"mac")
            (root / "linux.tar.gz").write_bytes(b"linux")
            result = self.run_renderer(root, "9.8.7-rc.1")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid stable version", result.stderr)


if __name__ == "__main__":
    unittest.main()
