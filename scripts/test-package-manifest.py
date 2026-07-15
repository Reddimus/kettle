#!/usr/bin/env python3
"""Hermetic tests for package-manifest.py."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("package-manifest.py")
SPEC = importlib.util.spec_from_file_location("package_manifest", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PackageManifestTests(unittest.TestCase):
    def package(self, root: Path) -> None:
        (root / "shell-integration").mkdir()
        binary = root / "kettle"
        binary.write_bytes(b"binary\n")
        binary.chmod(0o755)
        script = root / "shell-integration" / "kettle.sh"
        script.write_bytes(b"prompt\n")
        script.chmod(0o644)

    def test_generation_is_deterministic_and_verifiable(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            first = MODULE.build_manifest(root, "x86_64-unknown-linux-gnu", "2.36.0")
            second = MODULE.build_manifest(root, "x86_64-unknown-linux-gnu", "2.36.0")
            self.assertEqual(first, second)
            self.assertEqual(
                [item["path"] for item in first["files"]],
                ["kettle", "shell-integration/kettle.sh"],
            )
            self.assertEqual(first["files"][0]["mode"], 0o755)
            MODULE.generate(root, "x86_64-unknown-linux-gnu", "2.36.0")
            MODULE.verify(root, "x86_64-unknown-linux-gnu", "2.36.0")

            (root / "kettle").write_bytes(b"mutated\n")
            with self.assertRaisesRegex(ValueError, "does not match"):
                MODULE.verify(root, "x86_64-unknown-linux-gnu", "2.36.0")

    def test_windows_modes_are_null_and_identity_is_exact(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            manifest = MODULE.build_manifest(root, "x86_64-pc-windows-msvc", "2.36.0")
            self.assertTrue(all(item["mode"] is None for item in manifest["files"]))
            MODULE.generate(root, "x86_64-pc-windows-msvc", "2.36.0")
            with self.assertRaisesRegex(ValueError, "does not match"):
                MODULE.verify(root, "x86_64-pc-windows-msvc", "2.36.1")
            with self.assertRaisesRegex(ValueError, "stable"):
                MODULE.build_manifest(root, "x86_64-pc-windows-msvc", "2.36.0-rc.1")

    def test_rejects_symlinks_and_case_folded_duplicates(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            (root / "link").symlink_to("kettle")
            with self.assertRaisesRegex(ValueError, "non-regular"):
                MODULE.build_manifest(root, "x86_64-unknown-linux-gnu", "2.36.0")
            (root / "link").unlink()
            (root / "README").write_text("one", encoding="ascii")
            (root / "readme").write_text("two", encoding="ascii")
            with self.assertRaisesRegex(ValueError, "duplicate"):
                MODULE.build_manifest(root, "x86_64-unknown-linux-gnu", "2.36.0")

    def test_verifier_requires_canonical_manifest(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            manifest = MODULE.build_manifest(root, "x86_64-unknown-linux-gnu", "2.36.0")
            (root / MODULE.MANIFEST_NAME).write_text(json.dumps(manifest, indent=2), encoding="ascii")
            with self.assertRaisesRegex(ValueError, "canonical"):
                MODULE.verify(root, "x86_64-unknown-linux-gnu", "2.36.0")


if __name__ == "__main__":
    unittest.main()
