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

    def test_uninstall_removes_an_idle_update_lock_and_preserves_other_files(self):
        self.assertEqual(self.run_installer().returncode, 0)
        lock = self.prefix / ".kettle-update.lock"
        lock.touch(mode=0o600)
        unrelated = self.prefix / "unrelated.txt"
        unrelated.write_bytes(b"keep")
        result = self.run_installer("--uninstall")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(lock.exists())
        self.assertEqual(unrelated.read_bytes(), b"keep")

    def test_a_held_update_lock_refuses_install_and_uninstall(self):
        import fcntl

        self.assertEqual(self.run_installer().returncode, 0)
        lock = self.prefix / ".kettle-update.lock"
        lock.touch(mode=0o600)
        managed = self.prefix / "share/kettle/install-files.json"
        before = managed.read_bytes()
        with lock.open("rb") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for arguments in [(), ("--uninstall",)]:
                result = self.run_installer(*arguments)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(managed.read_bytes(), before)
                self.assertEqual((self.prefix / "bin/kettle").read_bytes(), self.gui.read_bytes())

    def test_untrusted_update_lock_leaves_the_installation_untouched(self):
        for kind in ["contents", "mode", "symlink", "hardlink", "directory"]:
            with self.subTest(kind=kind):
                self.prefix = self.root / kind
                self.assertEqual(self.run_installer().returncode, 0)
                lock = self.prefix / ".kettle-update.lock"
                lock.unlink(missing_ok=True)
                unrelated = self.root / (kind + "-unrelated")
                unrelated.write_bytes(b"keep")
                if kind == "symlink":
                    lock.symlink_to(unrelated)
                elif kind == "hardlink":
                    os.link(unrelated, lock)
                elif kind == "directory":
                    lock.mkdir()
                else:
                    lock.write_bytes(b"foreign" if kind == "contents" else b"")
                    lock.chmod(0o600 if kind == "contents" else 0o666)
                managed = self.prefix / "share/kettle/install-files.json"
                before = managed.read_bytes()
                result = self.run_installer("--uninstall")
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(managed.read_bytes(), before)
                self.assertEqual(unrelated.read_bytes(), b"keep")


if __name__ == "__main__":
    unittest.main()
