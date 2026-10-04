#!/usr/bin/env python3
"""Hermetic paired-binary installation and removal tests."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(os.name == "posix", "Unix installer")
class MediaInstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.package = self.root / "package"
        self.package.mkdir()
        self.prefix = self.root / "prefix"
        for name in ("install.sh", "install-unix.py"):
            shutil.copy2(ROOT / "scripts" / name, self.package / name)
        shutil.copytree(ROOT / "packaging" / "linux", self.package / "packaging" / "linux")
        self.gui = self.package / "kettle"
        self.gui.write_text("#!/bin/sh\nprintf 'kettle 5.0.0\\n'\n", encoding="ascii")
        self.gui.chmod(0o755)
        self.worker = self.package / "kettle-media-worker"
        # Installation must copy the worker, without invoking its job protocol.
        self.worker.write_text("#!/bin/sh\nexit 97\n", encoding="ascii")
        self.worker.chmod(0o755)

    def tearDown(self):
        self.temporary.cleanup()

    def run_installer(self, *arguments):
        return subprocess.run(
            ["bash", str(self.package / "install.sh"), f"--prefix={self.prefix}", *arguments],
            capture_output=True,
            text=True,
            timeout=30,
        )

    def test_installs_and_removes_both_binaries(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ("kettle", "kettle-media-worker"):
            installed = self.prefix / "bin" / name
            self.assertEqual(installed.read_bytes(), (self.package / name).read_bytes())
            self.assertEqual(installed.stat().st_mode & 0o777, 0o755)
        manifest = json.loads((self.prefix / "share/kettle/install-files.json").read_text())
        self.assertIn("bin/kettle-media-worker", {record["path"] for record in manifest["files"]})
        result = self.run_installer("--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.prefix / "bin/kettle").exists())
        self.assertFalse((self.prefix / "bin/kettle-media-worker").exists())

    def test_missing_worker_leaves_prefix_untouched(self):
        self.worker.unlink()
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("kettle-media-worker", result.stderr)
        self.assertFalse(self.prefix.exists())


if __name__ == "__main__":
    unittest.main()
