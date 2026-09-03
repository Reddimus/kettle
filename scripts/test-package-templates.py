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
                "--release-manifest",
                str(root / "manifest.json"),
                "--macos-archive",
                str(root / "mac.zip"),
                "--linux-aarch64-archive",
                str(root / "linux-aarch64.tar.gz"),
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
            manifest = b'{"schema_version":1}\n'
            macos = b"macos release archive\n"
            linux_aarch64 = b"linux aarch64 release archive\n"
            linux = b"linux release archive\n"
            (root / "manifest.json").write_bytes(manifest)
            (root / "mac.zip").write_bytes(macos)
            (root / "linux-aarch64.tar.gz").write_bytes(linux_aarch64)
            (root / "linux.tar.gz").write_bytes(linux)

            first = self.run_renderer(root)
            self.assertEqual(first.returncode, 0, first.stderr)
            formula = (root / "out" / "kettle.rb").read_text(encoding="utf-8")
            pkgbuild = (root / "out" / "PKGBUILD").read_text(encoding="utf-8")
            first_bytes = (formula.encode(), pkgbuild.encode())

            self.assertIn(
                '  url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-update-manifest.json",\n'
                "      using: :nounzip\n"
                f'  sha256 "{hashlib.sha256(manifest).hexdigest()}"',
                formula,
            )
            self.assertIn(
                '      url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-macos-universal.zip"\n'
                f'      sha256 "{hashlib.sha256(macos).hexdigest()}"',
                formula,
            )
            self.assertIn(
                '        url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-linux-aarch64.tar.gz"\n'
                f'        sha256 "{hashlib.sha256(linux_aarch64).hexdigest()}"',
                formula,
            )
            self.assertIn(
                '        url "https://github.com/Reddimus/kettle/releases/download/'
                'v9.8.7/kettle-linux-x86_64.tar.gz"\n'
                f'        sha256 "{hashlib.sha256(linux).hexdigest()}"',
                formula,
            )
            self.assertIn("pkgver=9.8.7", pkgbuild)
            self.assertIn(hashlib.sha256(linux).hexdigest(), pkgbuild)
            self.assertNotIn("@VERSION@", formula + pkgbuild)
            self.assertIn(
                '    share_dir = prefix/"share"\n'
                '    resource("binary").stage do\n'
                "      if OS.mac?",
                formula,
            )
            self.assertNotIn('\n      share = prefix/"share"\n', formula)
            self.assertIn(
                '(share_dir/"doc/kettle").install "#{doc_dir}/#{f}"',
                formula,
            )
            self.assertIn(
                '(share_dir/"doc/kettle").install "#{doc_dir}/shell-integration"',
                formula,
            )
            self.assertIn(
                '(share_dir/"doc/kettle/docs").install "#{doc_dir}/docs/changelog"',
                formula,
            )
            self.assertIn(
                'install -m644 docs/changelog/*.md',
                pkgbuild,
            )

            second = self.run_renderer(root)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first_bytes[0], (root / "out" / "kettle.rb").read_bytes())
            self.assertEqual(first_bytes[1], (root / "out" / "PKGBUILD").read_bytes())

    def test_rejects_non_stable_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "manifest.json").write_bytes(b"{}\n")
            (root / "mac.zip").write_bytes(b"mac")
            (root / "linux-aarch64.tar.gz").write_bytes(b"linux-aarch64")
            (root / "linux.tar.gz").write_bytes(b"linux")
            result = self.run_renderer(root, "9.8.7-rc.1")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid stable version", result.stderr)


if __name__ == "__main__":
    unittest.main()
