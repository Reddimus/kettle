#!/usr/bin/env python3
"""Hermetic paired-binary installation and removal tests."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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

    def test_documented_source_build_launches_with_a_sibling_worker(self):
        source = self.root / "source"
        release = source / "target/release"
        worker = source / "target/media-worker/kettle-media-worker"
        fake_bin = source / "fake-bin"
        for directory in (release, worker.parent, fake_bin):
            directory.mkdir(parents=True)
        worker.write_bytes(self.worker.read_bytes())
        worker.chmod(0o755)
        terminal = release / "kettle"
        terminal.write_text(
            '#!/bin/sh\nset -eu\n'
            'test -x "$(dirname "$0")/kettle-media-worker"\n',
            encoding="ascii",
        )
        terminal.chmod(0o755)
        cargo = fake_bin / "cargo"
        cargo.write_text("#!/bin/sh\nexit 0\n", encoding="ascii")
        cargo.chmod(0o755)
        recipe = (ROOT / "docs/INSTALL.md").read_text().split(
            "## From source\n", 1
        )[1].split("```sh\n", 1)[1].split("\n```", 1)[0].split(
            "\ncd kettle\n", 1
        )[1]
        environment = os.environ.copy()
        environment["PATH"] = f"{fake_bin}{os.pathsep}{environment['PATH']}"
        result = subprocess.run(
            ["bash", "-c", recipe], cwd=source, env=environment,
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            (release / "kettle-media-worker").read_bytes(), worker.read_bytes()
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

    def test_uninstall_smoke_distinguishes_legacy_and_current_locks(self):
        source = (ROOT / "scripts/check-linux-installers.sh").read_text()
        function = "assert_uninstalled_prefix() {" + source.split(
            "assert_uninstalled_prefix() {", 1
        )[1].split("\n}\n", 1)[0] + "\n}\n"
        program = "set -euo pipefail\nfail() { echo \"$*\" >&2; exit 1; }\n" + function
        program += 'assert_uninstalled_prefix "$1" "$2"\n'
        self.prefix.mkdir()
        lock = self.prefix / ".kettle-update.lock"

        def check(policy):
            return subprocess.run(
                ["bash", "-c", program, "lock-smoke", str(self.prefix), policy],
                capture_output=True, text=True, timeout=10,
            )

        self.assertEqual(check("removed").returncode, 0)
        lock.touch(mode=0o600)
        self.assertEqual(check("legacy-5.0.0").returncode, 0)
        self.assertNotEqual(check("removed").returncode, 0)
        lock.write_bytes(b"unexpected")
        self.assertNotEqual(check("legacy-5.0.0").returncode, 0)
        lock.unlink()
        target = self.root / "other"
        target.touch()
        lock.symlink_to(target)
        self.assertNotEqual(check("legacy-5.0.0").returncode, 0)
        target.unlink()
        self.assertNotEqual(check("removed").returncode, 0)

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("just"), "requires just on Linux")
    def test_local_install_recipes_build_both_binaries(self):
        expected = [
            "cargo build --locked --release -p kettle",
            "cargo build --locked --profile media-worker -p kettle-media-worker",
        ]
        for recipe in ("install-local", "install-recording"):
            with self.subTest(recipe=recipe):
                result = subprocess.run(
                    ["just", "--justfile", str(ROOT / "Justfile"), "--dry-run", recipe],
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = result.stderr.splitlines()
                self.assertEqual(commands[:2], expected)
                self.assertTrue(commands[2].startswith("./scripts/install.sh --skip-build"))
                self.assertEqual(
                    sum(command.startswith("cargo build ") for command in commands),
                    2,
                )

    def test_fifo_leaves_are_rejected_without_blocking(self):
        import sys

        self.assertEqual(self.run_installer().returncode, 0)
        child = """
import importlib.util
import sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("installer", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
try:
    if sys.argv[2] == "source":
        module._open_source(sys.argv[4])
    else:
        root = module._open_absolute_directory(sys.argv[3], create=False)
        manifest = module._read_manifest(root, sys.argv[3])
        if sys.argv[2] == "payload":
            module._verify_manifest_tree(root, manifest)
except module.InstallerError as error:
    print(error)
    sys.exit(0)
sys.exit(1)
"""
        for kind, path in [
            ("source", self.worker),
            ("payload", self.prefix / "bin/kettle-media-worker"),
            ("manifest", self.prefix / "share/kettle/install-files.json"),
        ]:
            with self.subTest(kind=kind):
                content = path.read_bytes()
                mode = path.stat().st_mode & 0o777
                path.unlink()
                os.mkfifo(path, mode)
                try:
                    result = subprocess.run(
                        [sys.executable, "-c", child,
                         str(ROOT / "scripts/install-unix.py"), kind, str(self.prefix), str(path)],
                        capture_output=True, text=True, timeout=2,
                    )
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertTrue(result.stdout.strip())
                finally:
                    path.unlink()
                    path.write_bytes(content)
                    path.chmod(mode)

    def helper(self, *files, fail_at=None, kill_at=None):
        """Run install-unix.py's install on `files`, a (path, bytes) list,
        then the files every install has. `fail_at` fails the nth file it
        publishes as a full disk would; `kill_at` ends the process right after
        the nth is in place, as a crash would."""
        files = (
            *files,
            ("bin/kettle", b"kettle"),
            ("share/kettle/install.sh", b"install.sh"),
            ("share/kettle/install-unix.py", b"install-unix.py"),
        )
        sources = self.root / "sources"
        sources.mkdir(exist_ok=True)
        arguments = [
            "install", "--prefix", str(self.prefix), "--channel", "local-dev",
            "--target", "linux", "--version", "5.0.0",
            "--desktop-template", str(ROOT / "packaging/linux/kettle.desktop"),
            "--desktop-binary", str(self.prefix / "bin/kettle"),
            "--desktop-icon", "kettle",
        ]
        for index, (relative, data) in enumerate(files):
            source = sources / f"{index}-{len(data)}"
            source.write_bytes(data)
            arguments += ["--file", relative, "0644", str(source)]
        child = """
import errno, importlib.util, os, sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("installer", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
fail_at, kill_at = int(sys.argv[2]), int(sys.argv[3])
publish = module._publish_source
calls = []
def failing(*arguments):
    calls.append(arguments)
    if len(calls) == fail_at:
        raise OSError(errno.ENOSPC, "No space left on device")
    published = publish(*arguments)
    if len(calls) == kill_at:
        os._exit(9)
    return published
module._publish_source = failing
sys.argv = ["install-unix.py", *sys.argv[4:]]
sys.exit(module.main())
"""
        return subprocess.run(
            [sys.executable, "-c", child, str(ROOT / "scripts/install-unix.py"),
             str(fail_at or 0), str(kill_at or 0), *arguments],
            capture_output=True, text=True, timeout=30,
        )

    def uninstall(self):
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/install-unix.py"), "uninstall",
             "--prefix", str(self.prefix)],
            capture_output=True, text=True, timeout=30,
        )

    def test_an_interrupted_upgrade_can_be_retried_and_removed(self):
        old = [("share/kettle/a", b"old a"), ("share/kettle/b", b"old b")]
        new = [("share/kettle/a", b"new a"), ("share/kettle/b", b"new b")]
        installed = self.helper(*old)
        self.assertEqual(installed.returncode, 0, installed.stderr)

        # The disk fills while the second file is written.
        cut = self.helper(*new, fail_at=2)
        self.assertNotEqual(cut.returncode, 0)
        self.assertIn("No space left", cut.stderr)
        self.assertEqual((self.prefix / "share/kettle/a").read_bytes(), b"new a")
        self.assertEqual((self.prefix / "share/kettle/b").read_bytes(), b"old b")

        retried = self.helper(*new)
        self.assertEqual(retried.returncode, 0, retried.stderr)
        self.assertEqual((self.prefix / "share/kettle/b").read_bytes(), b"new b")
        removed = self.uninstall()
        self.assertEqual(removed.returncode, 0, removed.stderr)
        self.assertFalse((self.prefix / "share/kettle/a").exists())

    def test_an_interrupted_first_install_can_be_retried_and_removed(self):
        files = [("share/kettle/a", b"a"), ("share/kettle/b", b"b")]
        cut = self.helper(*files, fail_at=2)
        self.assertNotEqual(cut.returncode, 0)
        self.assertEqual((self.prefix / "share/kettle/a").read_bytes(), b"a")
        retried = self.helper(*files)
        self.assertEqual(retried.returncode, 0, retried.stderr)
        removed = self.uninstall()
        self.assertEqual(removed.returncode, 0, removed.stderr)
        self.assertFalse((self.prefix / "share/kettle/a").exists())

    def test_a_killed_install_can_be_retried_or_removed(self):
        old = [("share/kettle/a", b"old a"), ("share/kettle/b", b"old b")]
        new = [("share/kettle/a", b"new a"), ("share/kettle/b", b"new b")]
        for then in ("retry", "uninstall"):
            with self.subTest(then=then):
                shutil.rmtree(self.prefix, ignore_errors=True)
                self.assertEqual(self.helper(*old).returncode, 0)
                # Killed with both new files in place and the old provenance.
                killed = self.helper(*new, kill_at=2)
                self.assertEqual(killed.returncode, 9, killed.stderr)
                self.assertEqual((self.prefix / "share/kettle/b").read_bytes(), b"new b")
                self.assertTrue((self.prefix / "share/kettle/install-pending.json").exists())
                if then == "retry":
                    retried = self.helper(*new)
                    self.assertEqual(retried.returncode, 0, retried.stderr)
                    self.assertFalse(
                        (self.prefix / "share/kettle/install-pending.json").exists()
                    )
                removed = self.uninstall()
                self.assertEqual(removed.returncode, 0, removed.stderr)
                self.assertFalse((self.prefix / "share/kettle").exists())

    def test_equal_bytes_alone_never_adopt_a_file(self):
        files = [("share/kettle/a", b"a")]
        # Someone else's file with exactly the bytes and mode Kettle would write.
        mine = self.prefix / "share/kettle/a"
        mine.parent.mkdir(parents=True)
        mine.write_bytes(b"a")
        mine.chmod(0o644)
        refused = self.helper(*files)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("unrecorded path", refused.stderr)
        self.assertEqual(mine.read_bytes(), b"a")

    def test_a_file_set_past_the_limit_is_refused_before_writing(self):
        first = [(f"share/kettle/one/{index}", b"1") for index in range(100)]
        second = [(f"share/kettle/two/{index}", b"2") for index in range(30)]
        self.assertEqual(self.helper(*first).returncode, 0)
        refused = self.helper(*second)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("file set exceeds its bounded limit", refused.stderr)
        self.assertFalse((self.prefix / "share/kettle/two").exists())
        removed = self.uninstall()
        self.assertEqual(removed.returncode, 0, removed.stderr)

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
