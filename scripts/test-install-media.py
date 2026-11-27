"""Hermetic paired-binary installation and removal tests."""

import json
import hashlib
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

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
        shutil.copytree(
            ROOT / "packaging" / "linux", self.package / "packaging" / "linux"
        )
        shutil.copytree(ROOT / "docs/licenses/tokyonight.nvim",
                        self.package / "docs/licenses/tokyonight.nvim")
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
            [
                "bash",
                str(self.package / "install.sh"),
                f"--prefix={self.prefix}",
                *arguments,
            ],
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )

    def test_managed_hash_stops_at_recorded_size_when_file_grows(self):
        spec = importlib.util.spec_from_file_location('installer', ROOT / 'scripts/install-unix.py')
        installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(installer)
        path = self.root / 'managed'
        path.write_bytes(b'old')
        path.chmod(0o600)
        record = {'path': 'managed', 'mode': 0o600, 'size': 3,
                  'sha256': hashlib.sha256(b'old').hexdigest()}
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        original_read = os.read
        requests, received = [], []
        def growing_read(handle, size):
            if not requests:
                with path.open('ab') as output:
                    output.write(b'x' * 65536)
            requests.append(size)
            data = original_read(handle, size)
            received.append(len(data))
            return data
        try:
            with patch.object(installer.os, 'read', side_effect=growing_read):
                with self.assertRaises(installer.InstallerError):
                    installer._verify_file(root_fd, record)
            self.assertLessEqual(sum(received), 4, 'hashing read past recorded size plus one byte')
            self.assertLessEqual(max(requests), 4)
        finally:
            os.close(root_fd)

    def test_source_build_uses_the_artifacts_it_installs(self):
        source = self.root / "source"
        scripts = source / "scripts"
        scripts.mkdir(parents=True)
        for name in ("install.sh", "install-unix.py"):
            shutil.copy2(self.package / name, scripts / name)
        shutil.copytree(self.package / "packaging", source / "packaging")
        shutil.copytree(self.package / "docs", source / "docs")
        for profile, binary in (
            ("release", "kettle"),
            ("media-worker", "kettle-media-worker"),
        ):
            stale = source / "target" / profile / binary
            stale.parent.mkdir(parents=True)
            stale.write_text("#!/bin/sh\nprintf 'kettle 0.0.0\\n'\n", encoding="ascii")
            stale.chmod(0o755)
        fake_bin = self.root / "fake-bin"
        fake_bin.mkdir()
        cargo = fake_bin / "cargo"
        cargo.write_text(
            "#!/bin/sh\nset -eu\n"
            "target_dir=${CARGO_TARGET_DIR:-target}\nprofile=debug\n"
            'while [ "$#" -gt 0 ]; do\n'
            '  case "$1" in\n'
            "    --target-dir) target_dir=$2; shift ;;\n"
            "    --release) profile=release ;;\n"
            "    --profile) profile=$2; shift ;;\n"
            "  esac\n  shift\ndone\n"
            'case "$profile" in\n'
            "  release) binary=kettle ;;\n"
            "  media-worker) binary=kettle-media-worker ;;\n"
            "  *) exit 2 ;;\n"
            "esac\n"
            'mkdir -p "$target_dir/$profile"\n'
            'cp "$KETTLE_TEST_PACKAGE/$binary" "$target_dir/$profile/$binary"\n',
            encoding="ascii",
        )
        cargo.chmod(0o755)
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                "CARGO_TARGET_DIR": str(self.root / "configured-target"),
                "KETTLE_TEST_PACKAGE": str(self.package),
            }
        )
        for arguments in ([], ["--skip-build"]):
            with self.subTest(arguments=arguments):
                result = subprocess.run(
                    [
                        "bash",
                        str(scripts / "install.sh"),
                        f"--prefix={self.prefix}",
                        *arguments,
                    ],
                    cwd=source,
                    env=environment,
                    capture_output=True,
                    check=False,
                    text=True,
                    timeout=30,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for name in ("kettle", "kettle-media-worker"):
                    self.assertEqual(
                        (self.prefix / "bin" / name).read_bytes(),
                        (self.package / name).read_bytes(),
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
            '#!/bin/sh\nset -eu\ntest -x "$(dirname "$0")/kettle-media-worker"\n',
            encoding="ascii",
        )
        terminal.chmod(0o755)
        cargo = fake_bin / "cargo"
        cargo.write_text("#!/bin/sh\nexit 0\n", encoding="ascii")
        cargo.chmod(0o755)
        recipe = (
            (ROOT / "docs/INSTALL.md")
            .read_text()
            .split("## From source\n", 1)[1]
            .split("```sh\n", 1)[1]
            .split("\n```", 1)[0]
            .split("\ncd kettle\n", 1)[1]
        )
        environment = os.environ.copy()
        environment["PATH"] = f"{fake_bin}{os.pathsep}{environment['PATH']}"
        result = subprocess.run(
            ["bash", "-c", recipe],
            cwd=source,
            env=environment,
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            (release / "kettle-media-worker").read_bytes(), worker.read_bytes()
        )

    def test_tokyonight_license_bytes_are_recorded_replaced_and_removed(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ("LICENSE", "README.md"):
            source = self.package / "docs/licenses/tokyonight.nvim" / name
            installed = self.prefix / "share/doc/kettle/docs/licenses/tokyonight.nvim" / name
            self.assertEqual(installed.read_bytes(), source.read_bytes())
            self.assertEqual(installed.stat().st_mode & 0o777, 0o644)
            source.write_bytes(source.read_bytes() + b"\nUpdated distribution fixture\n")
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        manifest = json.loads((self.prefix / "share/kettle/install-files.json").read_text())
        for name in ("LICENSE", "README.md"):
            relative = f"share/doc/kettle/docs/licenses/tokyonight.nvim/{name}"
            expected = (self.package / "docs/licenses/tokyonight.nvim" / name).read_bytes()
            self.assertEqual((self.prefix / relative).read_bytes(), expected)
            record = next(record for record in manifest["files"] if record["path"] == relative)
            self.assertEqual(record["sha256"], hashlib.sha256(expected).hexdigest())
            self.assertEqual(record["size"], len(expected))
        result = self.run_installer("--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.prefix / "share/doc/kettle/docs/licenses/tokyonight.nvim").exists())

    def test_tokyonight_license_sources_and_destinations_refuse_symlinks(self):
        source = self.package / "docs/licenses/tokyonight.nvim/LICENSE"
        outside = self.root / "outside"
        outside.mkdir()
        target = outside / "LICENSE"
        target.write_bytes(source.read_bytes())
        source.unlink()
        source.symlink_to(target)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.prefix / "bin/kettle").exists())
        source.unlink()
        source.write_bytes(target.read_bytes())
        parent = self.prefix / "share/doc/kettle/docs/licenses"
        parent.mkdir(parents=True)
        (parent / "tokyonight.nvim").symlink_to(outside, target_is_directory=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(target.read_bytes(), source.read_bytes())
        self.assertFalse((outside / "README.md").exists())
        self.assertFalse((self.prefix / "bin/kettle").exists())

    def test_installs_and_removes_both_binaries(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ("kettle", "kettle-media-worker"):
            installed = self.prefix / "bin" / name
            self.assertEqual(installed.read_bytes(), (self.package / name).read_bytes())
            self.assertEqual(installed.stat().st_mode & 0o777, 0o755)
        manifest = json.loads(
            (self.prefix / "share/kettle/install-files.json").read_text()
        )
        self.assertIn(
            "bin/kettle-media-worker", {record["path"] for record in manifest["files"]}
        )
        result = self.run_installer("--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.prefix / "bin/kettle").exists())
        self.assertFalse((self.prefix / "bin/kettle-media-worker").exists())

    def test_documented_uninstall_works_from_an_extracted_tarball(self):
        home = self.root / "home"
        home.mkdir(mode=0o700)
        self.prefix = home / ".local"
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        recipe = (
            (ROOT / "docs/INSTALL.md")
            .read_text()
            .split('type **"kettle"** to launch. To remove everything later:', 1)[1]
            .split("```sh\n", 1)[1]
            .split("\n```", 1)[0]
        )
        environment = os.environ.copy()
        environment["HOME"] = str(home)
        result = subprocess.run(
            ["bash", "-c", recipe],
            cwd=self.package,
            env=environment,
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for relative in (
            "bin/kettle",
            "bin/kettle-media-worker",
            "share/kettle/install.sh",
            "share/kettle/install-files.json",
        ):
            self.assertFalse((self.prefix / relative).exists(), relative)

    def test_uninstall_smoke_distinguishes_legacy_and_current_locks(self):
        source = (ROOT / "scripts/check-linux-installers.sh").read_text()
        function = (
            "assert_uninstalled_prefix() {"
            + source.split("assert_uninstalled_prefix() {", 1)[1].split("\n}\n", 1)[0]
            + "\n}\n"
        )
        program = 'set -euo pipefail\nfail() { echo "$*" >&2; exit 1; }\n' + function
        program += 'assert_uninstalled_prefix "$1" "$2"\n'
        self.prefix.mkdir()
        lock = self.prefix / ".kettle-update.lock"

        def check(policy):
            return subprocess.run(
                ["bash", "-c", program, "lock-smoke", str(self.prefix), policy],
                capture_output=True,
                check=False,
                text=True,
                timeout=10,
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

    @unittest.skipUnless(
        sys.platform == "linux" and shutil.which("just"), "requires just on Linux"
    )
    def test_local_install_recipes_build_both_binaries(self):
        expected = [
            "cargo build --locked --release -p kettle --target-dir target",
            "cargo build --locked --profile media-worker -p kettle-media-worker --target-dir target",
        ]
        for recipe in ("install-local", "install-recording"):
            with self.subTest(recipe=recipe):
                result = subprocess.run(
                    ["just", "--justfile", str(ROOT / "Justfile"), "--dry-run", recipe],
                    cwd=ROOT,
                    capture_output=True,
                    check=False,
                    text=True,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = result.stderr.splitlines()
                self.assertEqual(commands[:2], expected)
                self.assertTrue(
                    commands[2].startswith("./scripts/install.sh --skip-build")
                )
                self.assertEqual(
                    sum(command.startswith("cargo build ") for command in commands),
                    2,
                )

    @unittest.skipUnless(shutil.which("just"), "requires just")
    def test_local_install_ignores_external_target_directory(self):
        source = self.root / "recipes"
        scripts = source / "scripts"
        scripts.mkdir(parents=True)
        justfile = (ROOT / "Justfile").read_text()
        # Exercise the Linux recipes on either supported Unix development host.
        (source / "Justfile").write_text(justfile.replace("[linux]\ninstall-", "[unix]\ninstall-"))
        cargo = source / "cargo"
        cargo.write_text(
            "#!/bin/sh\nset -eu\n"
            'target_dir=$CARGO_TARGET_DIR\nprofile=release\nbinary=kettle\n'
            'while [ "$#" -gt 0 ]; do\n'
            'case "$1" in\n'
            '  --target-dir) target_dir=$2; shift ;;\n'
            '  --profile) profile=$2; binary=kettle-media-worker; shift ;;\n'
            'esac\nshift\ndone\n'
            'mkdir -p "$target_dir/$profile"\n'
            'printf fresh > "$target_dir/$profile/$binary"\n'
        )
        cargo.chmod(0o755)
        installer = scripts / "install.sh"
        installer.write_text(
            '#!/bin/sh\nset -eu\n'
            'for binary in target/release/kettle target/media-worker/kettle-media-worker; do\n'
            '  test "$(cat "$binary")" = fresh || { echo "stale artifact: $binary" >&2; exit 1; }\n'
            'done\n'
        )
        installer.chmod(0o755)
        for recipe in ("install-local", "install-recording"):
            with self.subTest(recipe=recipe):
                for relative in ("release/kettle", "media-worker/kettle-media-worker"):
                    stale = source / "target" / relative
                    stale.parent.mkdir(parents=True, exist_ok=True)
                    stale.write_text("stale")
                result = subprocess.run(
                    ["just", recipe], cwd=source,
                    env={**os.environ, "PATH": f"{source}:{os.environ['PATH']}",
                         "CARGO_TARGET_DIR": str(self.root / "external target")},
                    capture_output=True, text=True, check=False, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_source_growth_is_rejected_before_writing_past_the_plan(self):
        source = self.root / "growing-source"
        source.write_bytes(b"x" * 8)
        self.prefix.mkdir(mode=0o700)
        destination = self.prefix / "copy"
        destination.write_bytes(b"existing")
        child = """
import importlib.util, os, pathlib, sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("installer", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
source, prefix = sys.argv[2:]
module.MAX_FILE_BYTES = 64
expected_size, expected_digest = module._source_identity(source)
open_source = module._open_source
def growing_open(path):
    handle = open_source(path)
    with open(path, "ab") as output:
        output.write(b"y" * 80)
    return handle
module._open_source = growing_open
write_all = module._write_all
read = module.os.read
writes, reads = [], []
def record_write(handle, chunk):
    writes.append(len(chunk))
    write_all(handle, chunk)
def record_read(handle, maximum):
    reads.append(maximum)
    return read(handle, maximum)
module._write_all = record_write
module.os.read = record_read
root = module._open_absolute_directory(prefix, create=False)
try:
    try:
        module._publish_source(
            root, "copy", source, expected_size, expected_digest, 0o644, {}
        )
    except module.InstallerError:
        pass
    else:
        raise AssertionError("growing source was published")
finally:
    os.close(root)
assert sum(writes) <= expected_size, writes
assert reads and max(reads) <= expected_size + 1, reads
assert (pathlib.Path(prefix) / "copy").read_bytes() == b"existing"
assert not list(pathlib.Path(prefix).glob(".kettle-install-tmp-*"))
"""
        result = subprocess.run(
            [
                sys.executable,
                "-c",
                child,
                str(ROOT / "scripts/install-unix.py"),
                str(source),
                str(self.prefix),
            ],
            capture_output=True,
            check=False,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

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
                        [
                            sys.executable,
                            "-c",
                            child,
                            str(ROOT / "scripts/install-unix.py"),
                            kind,
                            str(self.prefix),
                            str(path),
                        ],
                        capture_output=True,
                        check=False,
                        text=True,
                        timeout=2,
                    )
                    self.assertEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
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
            "install",
            "--prefix",
            str(self.prefix),
            "--channel",
            "local-dev",
            "--target",
            "linux",
            "--version",
            "5.0.0",
            "--desktop-template",
            str(ROOT / "packaging/linux/kettle.desktop"),
            "--desktop-binary",
            str(self.prefix / "bin/kettle"),
            "--desktop-icon",
            "kettle",
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
            [
                sys.executable,
                "-c",
                child,
                str(ROOT / "scripts/install-unix.py"),
                str(fail_at or 0),
                str(kill_at or 0),
                *arguments,
            ],
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )

    def uninstall(self, kill_at=0, kill_path=None, installed=False):
        child = """
import importlib.util, os, sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("installer", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
unlink = module._unlink_recorded
count = 0
def interrupted(*arguments):
    global count
    unlink(*arguments)
    count += 1
    if str(count) == sys.argv[2] or arguments[1] == sys.argv[2]:
        os._exit(9)
module._unlink_recorded = interrupted
arguments = module._parser().parse_args(["uninstall", "--prefix", sys.argv[3]])
module._uninstall(arguments)
"""
        helper = str(self.prefix / "share/kettle/install-unix.py" if installed
                     else ROOT / "scripts/install-unix.py")
        command = ([sys.executable, "-c", child, helper, str(kill_path or kill_at), str(self.prefix)]
                   if kill_at or kill_path else [sys.executable, helper, "uninstall", "--prefix", str(self.prefix)])
        return subprocess.run(
            command,
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )

    def test_interrupted_uninstall_resumes_and_verifies_remaining_files(self):
        self.assertEqual(self.helper(("share/kettle/z", b"payload")).returncode, 0)
        cut = self.uninstall(kill_at=1)
        self.assertEqual(cut.returncode, 9, cut.stderr)
        resumed = self.uninstall()
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertFalse((self.prefix / "share/kettle").exists())

    def test_uninstall_interrupted_after_each_recovery_helper_deletion(self):
        for name in ('install.sh', 'install-unix.py', 'uninstall-pending.json'):
            with self.subTest(helper=name):
                installed = self.run_installer()
                self.assertEqual(installed.returncode, 0, installed.stderr)
                cut = self.uninstall(kill_path='share/kettle/' + name, installed=True)
                self.assertEqual(cut.returncode, 9, cut.stderr)
                self.assertFalse((self.prefix / 'share/kettle' / name).exists())
                journal = self.prefix / 'share/kettle/uninstall-pending.json'
                if journal.exists():
                    helper = self.prefix / 'share/kettle/install-unix.py'
                    self.assertTrue(helper.is_file(), 'blocking journal has no installed recovery command')
                    resumed = self.uninstall(installed=True)
                else:
                    # Once the recovery command is gone, reinstall must be available.
                    receipt = self.prefix / 'share/kettle/install-pending.json'
                    self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)
                    reinstalled = self.run_installer()
                    self.assertEqual(reinstalled.returncode, 0, reinstalled.stderr)
                    resumed = subprocess.run(
                        ['bash', str(self.prefix / 'share/kettle/install.sh'), '--uninstall'],
                        capture_output=True, text=True, timeout=30,
                    )
                self.assertEqual(resumed.returncode, 0, resumed.stderr)
                for relative in ('bin/kettle', 'share/kettle/install.sh',
                                 'share/kettle/install-unix.py',
                                 'share/kettle/uninstall-pending.json',
                                 'share/kettle/install-pending.json'):
                    self.assertFalse((self.prefix / relative).exists(), relative)

    def test_uninstall_journal_preserves_helpers_and_refuses_changed_remainder(self):
        self.assertEqual(self.helper(("share/kettle/z", b"payload")).returncode, 0)
        self.assertEqual(self.uninstall(kill_at=1).returncode, 9)
        journal = self.prefix / "share/kettle/uninstall-pending.json"
        self.assertEqual(journal.stat().st_mode & 0o777, 0o600)
        for name in ('install.sh', 'install-unix.py'):
            self.assertTrue((journal.parent / name).exists())
        refused = self.helper()
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('interrupted uninstall', refused.stderr)
        binary = self.prefix / "bin/kettle"
        binary.write_bytes(b"tamper")
        refused = self.uninstall()
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('changed content', refused.stderr)
        self.assertTrue(journal.exists())
        self.assertTrue((journal.parent / 'install.sh').exists())
        binary.write_bytes(b"kettle")
        self.assertEqual(self.uninstall().returncode, 0)

    def test_uninstall_resumes_after_manifest_removal(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        cut = self.uninstall(kill_path='share/kettle/install-files.json', installed=True)
        self.assertEqual(cut.returncode, 9, cut.stderr)
        self.assertFalse((self.prefix / "share/kettle/install-files.json").exists())
        self.assertTrue((self.prefix / "share/kettle/install-unix.py").exists())
        resumed = subprocess.run(
            ['bash', str(self.prefix / 'share/kettle/install.sh'), '--uninstall'],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertFalse((self.prefix / "share/kettle").exists())

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
                self.assertEqual(
                    (self.prefix / "share/kettle/b").read_bytes(), b"new b"
                )
                self.assertTrue(
                    (self.prefix / "share/kettle/install-pending.json").exists()
                )
                if then == "retry":
                    retried = self.helper(*new)
                    self.assertEqual(retried.returncode, 0, retried.stderr)
                    self.assertFalse(
                        (self.prefix / "share/kettle/install-pending.json").exists()
                    )
                removed = self.uninstall()
                self.assertEqual(removed.returncode, 0, removed.stderr)
                self.assertFalse((self.prefix / "share/kettle").exists())

    def test_repeated_interrupted_plans_keep_receipt_readable(self):
        for attempt in range(3):
            files = [(f'share/kettle/file-{index}', f'plan-{attempt}'.encode()) for index in range(100)]
            killed = self.helper(*files, kill_at=1)
            self.assertEqual(killed.returncode, 9, killed.stderr)
            receipt = json.loads((self.prefix / 'share/kettle/install-pending.json').read_text())
            self.assertLessEqual(len(receipt['files']), 256,
                                 'installer published a receipt its reader rejects')
        retried = self.helper(*files)
        self.assertEqual(retried.returncode, 0, retried.stderr)
        removed = self.uninstall()
        self.assertEqual(removed.returncode, 0, removed.stderr)

    def test_retry_preserves_pending_files_omitted_from_new_plan(self):
        self.assertEqual(self.helper().returncode, 0)
        path = "share/kettle/optional"
        killed = self.helper((path, b"optional"), kill_at=1)
        self.assertEqual(killed.returncode, 9, killed.stderr)
        retry = self.helper()
        self.assertEqual(retry.returncode, 0, retry.stderr)
        manifest = json.loads((self.prefix / "share/kettle/install-files.json").read_text())
        self.assertIn(path, [record["path"] for record in manifest["files"]])
        removed = self.uninstall()
        self.assertEqual(removed.returncode, 0, removed.stderr)
        self.assertFalse((self.prefix / path).exists())

    def interrupted_self_update(self, phase="rolling_back", files=None):
        """Leave the prefix as a self-update that failed part-way and could not
        finish rolling back leaves it (#846): the installer scripts it replaced
        first, one file it had only backed up, its journal and its backups."""
        manifest = json.loads(
            (self.prefix / "share/kettle/install-files.json").read_text()
        )
        records = {record["path"]: record for record in manifest["files"]}
        transaction = "4242-1793827181830941000"
        backup = self.prefix / f".kettle-update-backup-{transaction}"
        backup.mkdir()
        backup.chmod(0o700)
        marker = backup / ".kettle-update-backup.json"
        marker.write_text(json.dumps(
            {"schema": 2, "product": "kettle", "transaction_id": transaction}
        ))
        marker.chmod(0o600)
        entries = []
        for relative, replacement, state in files or (
            ("share/kettle/install.sh", b"#!/bin/sh\n# 9.9.9 installer\n", "installed"),
            ("share/kettle/install-unix.py", b"# 9.9.9 helper\n", "installed"),
            ("bin/kettle-media-worker", b"9.9.9 worker", "prepared"),
        ):
            path = self.prefix / relative
            record = records[relative]
            copy = backup / relative
            copy.parent.mkdir(parents=True, exist_ok=True)
            for directory in (copy.parent, copy.parent.parent):
                if directory != backup:
                    directory.chmod(0o755)
            copy.write_bytes(path.read_bytes())
            copy.chmod(0o600)
            if state == "installed":
                staged = path.with_name(path.name + ".staged")
                staged.write_bytes(replacement)
                staged.chmod(record["mode"])
                os.replace(staged, path)
            entries.append({
                "relative": relative,
                "existed": True,
                "previous_unix_mode": record["mode"],
                "previous_size": record["size"],
                "previous_sha256": record["sha256"],
                "replacement_size": len(replacement),
                "replacement_sha256": hashlib.sha256(replacement).hexdigest(),
                "state": state,
            })
        journal = self.prefix / ".kettle-update-journal.json"
        journal.write_text(json.dumps({
            "schema": 2,
            "transaction_id": transaction,
            "target_version": "9.9.9",
            "phase": phase,
            "backup_dir": backup.name,
            "entries": entries,
        }))
        journal.chmod(0o600)
        return journal, backup

    def test_an_interrupted_self_update_is_adopted_and_retired(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        journal, backup = self.interrupted_self_update()

        repaired = self.run_installer()

        self.assertEqual(repaired.returncode, 0, repaired.stderr)
        for name in ("install.sh", "install-unix.py"):
            self.assertEqual(
                (self.prefix / "share/kettle" / name).read_bytes(),
                (self.package / name).read_bytes(),
            )
        self.assertFalse(journal.exists(), "kettle would roll back over the install")
        self.assertFalse(backup.exists(), "the retired update's backups were kept")
        self.assertFalse((self.prefix / "share/kettle/install-pending.json").exists())
        again = self.run_installer()
        self.assertEqual(again.returncode, 0, again.stderr)
        removed = self.run_installer("--uninstall")
        self.assertEqual(removed.returncode, 0, removed.stderr)
        self.assertFalse((self.prefix / "share/kettle").exists())

    def test_a_duplicated_journal_key_stops_the_install(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        journal, _backup = self.interrupted_self_update()
        text = journal.read_text()
        journal.write_text(text[:-1] + ', "phase": "committed"}')
        refused = self.run_installer()
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("not one Kettle's self-updater wrote", refused.stderr)
        self.assertTrue(journal.exists())

    def test_an_interrupted_self_update_is_retired_before_the_install_writes(self):
        installed = self.helper()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        journal, backup = self.interrupted_self_update(files=(
            ("share/kettle/install.sh", b"#!/bin/sh\n# 9.9.9\n", "installed"),
            ("bin/kettle", b"9.9.9 kettle", "prepared"),
        ))

        # Killed right after the first file this install publishes.
        cut = self.helper(kill_at=1)

        self.assertEqual(cut.returncode, 9, cut.stderr)
        self.assertFalse(journal.exists(), "kettle would roll back over this install")
        self.assertFalse(backup.exists())
        retried = self.helper()
        self.assertEqual(retried.returncode, 0, retried.stderr)
        self.assertEqual((self.prefix / "share/kettle/install.sh").read_bytes(), b"install.sh")

    def test_a_write_the_journal_says_never_happened_is_not_adopted(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        replacement = b"#!/bin/sh\n# 9.9.9 installer\n"
        journal, _backup = self.interrupted_self_update(
            phase="applying",
            files=(("share/kettle/install.sh", replacement, "backing_up"),),
        )
        # The update was still backing this file up, so it never wrote these
        # bytes; whoever did is not the update.
        script = self.prefix / "share/kettle/install.sh"
        staged = script.with_name("install.sh.staged")
        staged.write_bytes(replacement)
        staged.chmod(script.stat().st_mode & 0o777)
        os.replace(staged, script)

        refused = self.run_installer()

        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("share/kettle/install.sh changed after an interrupted", refused.stderr)
        self.assertEqual(script.read_bytes(), replacement)
        self.assertTrue(journal.exists())

    def test_a_changed_journaled_install_record_is_refused(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        manifest = self.prefix / "share/kettle/install-files.json"
        journal, _backup = self.interrupted_self_update()
        document = json.loads(journal.read_text())
        current = manifest.read_bytes()
        document["entries"].append({
            "relative": "share/kettle/install-files.json",
            "existed": True,
            "previous_unix_mode": 0o644,
            "previous_size": len(current),
            "previous_sha256": hashlib.sha256(current).hexdigest(),
            "replacement_size": 2,
            "replacement_sha256": hashlib.sha256(b"{}").hexdigest(),
            "state": "installed",
        })
        journal.write_text(json.dumps(document))
        manifest.write_bytes(current + b"\n")

        refused = self.run_installer()

        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("install-files.json changed after an interrupted", refused.stderr)
        self.assertEqual(manifest.read_bytes(), current + b"\n")

    def test_a_record_listing_a_file_the_rollback_removed_is_repaired(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        added = b"a file only the newer release ships"
        journal, backup = self.interrupted_self_update()
        document = json.loads(journal.read_text())
        document["entries"].append({
            "relative": "share/kettle/added-by-update",
            "existed": False,
            "previous_size": None,
            "previous_sha256": None,
            "replacement_size": len(added),
            "replacement_sha256": hashlib.sha256(added).hexdigest(),
            "state": "restored",
        })
        journal.write_text(json.dumps(document))
        # The update's own record survived and still lists what rollback removed.
        manifest_path = self.prefix / "share/kettle/install-files.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["files"].append({
            "path": "share/kettle/added-by-update",
            "size": len(added),
            "sha256": hashlib.sha256(added).hexdigest(),
            "mode": 0o644,
        })
        manifest["files"].sort(key=lambda record: record["path"])
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        manifest_path.chmod(0o644)

        repaired = self.run_installer()

        self.assertEqual(repaired.returncode, 0, repaired.stderr)
        recorded = {
            record["path"]
            for record in json.loads(manifest_path.read_text())["files"]
        }
        self.assertNotIn("share/kettle/added-by-update", recorded)
        self.assertFalse(journal.exists() or backup.exists())

    def test_files_only_the_updater_writes_are_adopted_from_a_first_update(self):
        # install.sh never installs the license notices or the changelog
        # archives; the first self-update journals them with no record yet.
        for state in ("installed", "restored"):
            with self.subTest(state=state):
                if self.prefix.exists():
                    shutil.rmtree(self.prefix)
                installed = self.run_installer()
                self.assertEqual(installed.returncode, 0, installed.stderr)
                manifest_path = self.prefix / "share/kettle/install-files.json"
                recorded = {r["path"] for r in json.loads(manifest_path.read_text())["files"]}
                self.assertNotIn("share/doc/kettle/LICENSE", recorded)
                journal, backup = self.interrupted_self_update()
                document = json.loads(journal.read_text())
                written = {
                    "share/doc/kettle/LICENSE": b"license from the update",
                    "share/doc/kettle/docs/changelog/CHANGELOG-4.x.md": b"4.x",
                }
                for relative, data in written.items():
                    document["entries"].insert(0, {
                        "relative": relative,
                        "existed": False,
                        "previous_size": None,
                        "previous_sha256": None,
                        "replacement_size": len(data),
                        "replacement_sha256": hashlib.sha256(data).hexdigest(),
                        "state": state,
                    })
                    if state == "installed":
                        path = self.prefix / relative
                        path.parent.mkdir(parents=True, exist_ok=True)
                        for directory in (path.parent, *path.parent.parents):
                            if directory == self.prefix:
                                break
                            directory.chmod(0o755)
                        path.write_bytes(data)
                        path.chmod(0o644)
                if state == "restored":
                    document["phase"] = "rolling_back"
                journal.write_text(json.dumps(document))

                repaired = self.run_installer()

                self.assertEqual(repaired.returncode, 0, repaired.stderr)
                self.assertFalse(journal.exists() or backup.exists())
                recorded = {r["path"] for r in json.loads(manifest_path.read_text())["files"]}
                for relative in written:
                    self.assertEqual(relative in recorded, state == "installed", relative)

    def test_a_journal_naming_a_path_kettle_does_not_own_is_refused(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        other = self.prefix / "bin/othertool"
        other.write_bytes(b"someone else's tool")
        other.chmod(0o755)
        journal, _backup = self.interrupted_self_update()
        document = json.loads(journal.read_text())
        document["entries"].append({
            "relative": "bin/othertool",
            "existed": True,
            "previous_unix_mode": 0o755,
            "previous_size": other.stat().st_size,
            "previous_sha256": hashlib.sha256(other.read_bytes()).hexdigest(),
            "replacement_size": 1,
            "replacement_sha256": hashlib.sha256(b"x").hexdigest(),
            "state": "installed",
        })
        journal.write_text(json.dumps(document))

        refused = self.run_installer()

        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("names bin/othertool", refused.stderr)
        recorded = {
            record["path"]
            for record in json.loads(
                (self.prefix / "share/kettle/install-files.json").read_text()
            )["files"]
        }
        self.assertNotIn("bin/othertool", recorded)
        self.assertEqual(other.read_bytes(), b"someone else's tool")
        self.assertTrue(journal.exists())

    def test_a_backup_that_keeps_a_copy_stays_marked(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        journal, backup = self.interrupted_self_update(
            phase="applying",
            files=(
                ("share/kettle/install.sh", b"#!/bin/sh\n# 9.9.9\n", "installed"),
                ("bin/kettle-media-worker", b"9.9.9 worker", "backing_up"),
            ),
        )
        # The update stopped part-way through copying this backup.
        partial = backup / "bin/kettle-media-worker"
        partial.write_bytes(partial.read_bytes()[:2])

        repaired = self.run_installer()

        self.assertEqual(repaired.returncode, 0, repaired.stderr)
        self.assertFalse(journal.exists())
        self.assertFalse((backup / "share/kettle/install.sh").exists())
        self.assertTrue(partial.exists(), "a copy that does not match was removed")
        self.assertTrue(
            (backup / ".kettle-update-backup.json").exists(),
            "a backup that still holds a copy lost its marker",
        )

    def test_a_file_changed_after_an_interrupted_self_update_is_refused(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        journal, backup = self.interrupted_self_update()
        script = self.prefix / "share/kettle/install.sh"
        script.write_bytes(b"#!/bin/sh\n# edited by hand\n")

        refused = self.run_installer()

        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("share/kettle/install.sh changed after an interrupted", refused.stderr)
        self.assertEqual(script.read_bytes(), b"#!/bin/sh\n# edited by hand\n")
        self.assertTrue(journal.exists() and backup.exists())

    def test_a_committed_self_update_journal_is_left_to_kettle(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        journal, backup = self.interrupted_self_update(phase="committed")
        # A committed update also wrote the record that describes its files.
        manifest_path = self.prefix / "share/kettle/install-files.json"
        manifest = json.loads(manifest_path.read_text())
        written = {entry["relative"]: entry for entry in json.loads(journal.read_text())["entries"]}
        for record in manifest["files"]:
            entry = written.get(record["path"])
            if entry and entry["state"] == "installed":
                record["size"] = entry["replacement_size"]
                record["sha256"] = entry["replacement_sha256"]
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        manifest_path.chmod(0o644)

        result = self.run_installer()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(journal.exists(), "kettle confirms and removes a committed journal")
        self.assertTrue(backup.exists())

    def test_an_unreadable_self_update_journal_stops_the_install(self):
        installed = self.run_installer()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        script = self.prefix / "share/kettle/install.sh"
        before = script.read_bytes()
        journal, _backup = self.interrupted_self_update()
        valid = json.loads(journal.read_text())
        script.write_bytes(before)
        script.chmod(0o755)
        wrong_type = json.loads(json.dumps(valid))
        wrong_type["entries"][0]["previous_unix_mode"] = "0755"
        for document, mode, message in (
            ({"schema": 1, "backup_dir": ".kettle-update-backup-1", "entries": []},
             0o600, "start the installed kettle once"),
            (dict(valid, schema=2.0), 0o600, "start the installed kettle once"),
            ({"schema": 2}, 0o600, "not one Kettle's self-updater wrote"),
            (dict(valid, extra=1), 0o600, "not one Kettle's self-updater wrote"),
            (dict(valid, entries=valid["entries"] + valid["entries"][:1]), 0o600,
             "not one Kettle's self-updater wrote"),
            (dict(valid, phase=["rolling_back"]), 0o600, "not one Kettle's self-updater wrote"),
            (dict(valid, target_version="nine"), 0o600, "not one Kettle's self-updater wrote"),
            (dict(valid, phase="prepared", entries=[
                dict(valid["entries"][0], state="restored")]), 0o600,
             "not one Kettle's self-updater wrote"),
            (dict(valid, backup_dir=".kettle-update-backup-1-1"), 0o600,
             "not one Kettle's self-updater wrote"),
            (wrong_type, 0o600, "not one Kettle's self-updater wrote"),
            (valid, 0o644, "not an owned bounded regular file"),
        ):
            with self.subTest(document=document, mode=oct(mode)):
                journal.write_text(json.dumps(document))
                journal.chmod(mode)
                refused = self.run_installer()
                self.assertNotEqual(refused.returncode, 0)
                self.assertIn(message, refused.stderr)
                self.assertEqual(script.read_bytes(), before)

    def test_equal_bytes_alone_never_adopt_a_file(self):
        files = [("share/kettle/a", b"a")]
        # Someone else's file with exactly the bytes and mode Kettle would write.
        mine = self.prefix / "share/kettle/a"
        for directory in (self.prefix, self.prefix / "share", mine.parent):
            directory.mkdir(mode=0o700)
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
                self.assertEqual(
                    (self.prefix / "bin/kettle").read_bytes(), self.gui.read_bytes()
                )

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
