#!/usr/bin/env python3
"""Hermetic tests for make-update-manifest.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("make-update-manifest.py")
SPEC = importlib.util.spec_from_file_location("make_update_manifest", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ManifestTests(unittest.TestCase):
    def assets(self, root: Path):
        result = []
        for target, name in MODULE.EXPECTED_NAMES.items():
            path = root / name
            path.write_bytes((target + "\n").encode("ascii"))
            result.append((target, path))
        return result

    def test_manifest_is_complete_deterministic_and_hashed(self):
        with tempfile.TemporaryDirectory() as raw:
            assets = self.assets(Path(raw))
            first = MODULE.build_manifest("v2.35.0", "2026-07-11T00:00:00Z", assets)
            second = MODULE.build_manifest(
                "v2.35.0", "2026-07-11T00:00:00Z", list(reversed(assets))
            )
            self.assertEqual(first, second)
            self.assertEqual(first["version"], "2.35.0")
            self.assertEqual(len(first["assets"]), 3)
            self.assertTrue(all(len(asset["sha256"]) == 64 for asset in first["assets"]))

    def test_rejects_prerelease_missing_and_misnamed_assets(self):
        with tempfile.TemporaryDirectory() as raw:
            assets = self.assets(Path(raw))
            with self.assertRaises(ValueError):
                MODULE.build_manifest("v2.35.0-rc.1", "now", assets)
            with self.assertRaises(ValueError):
                MODULE.build_manifest("v2.35.0", "now", assets[:-1])
            wrong = list(assets)
            wrong[0] = (wrong[0][0], Path(raw) / "wrong.zip")
            wrong[0][1].write_bytes(b"wrong")
            with self.assertRaises(ValueError):
                MODULE.build_manifest("v2.35.0", "now", wrong)


if __name__ == "__main__":
    unittest.main()
