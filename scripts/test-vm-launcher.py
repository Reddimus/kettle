#!/usr/bin/env python3
"""Focused regression tests for the QEMU Ubuntu ARM launcher."""

from __future__ import annotations

import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time
import unittest


sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
LAUNCHER = ROOT / "scripts" / "vm" / "run-ubuntu-arm.sh"


@unittest.skipIf(os.name == "nt", "the launcher targets QEMU/HVF on macOS")
class VmLauncherTests(unittest.TestCase):
    def fixture(self, root):
        vm = root / 'vm'
        vm.mkdir()
        for name in ('ubuntu-arm.qcow2', 'efi_vars.fd'):
            (vm / name).write_bytes(b'fixture')
        code = root / 'code.fd'
        code.write_bytes(b'fixture')
        fake_bin = root / 'bin'
        fake_bin.mkdir()
        qemu = fake_bin / 'qemu-system-aarch64'
        qemu.write_text('#!/bin/sh\nexit 0\n')
        qemu.chmod(0o755)
        return {**os.environ, 'KETTLE_VM_DIR': str(vm), 'KETTLE_VM_EFI_CODE': str(code),
                'PATH': str(fake_bin) + os.pathsep + os.environ['PATH']}

    def test_socket_inspection_errors_preserve_the_socket(self):
        for status, diagnostic in ((1, 'inspection failed'), (3, ''), (127, '')):
            with self.subTest(status=status), tempfile.TemporaryDirectory(dir='/tmp', prefix='kvm-') as raw:
                root = Path(raw)
                env = self.fixture(root)
                lsof = root / 'bin' / 'lsof'
                lsof.write_text(f'#!/bin/sh\necho "{diagnostic}" >&2\nexit {status}\n')
                lsof.chmod(0o755)
                path = root / 'vm' / 'qemu-guest-agent.sock'
                path.touch()
                # The sandbox forbids AF_UNIX bind; only replace the type query.
                shell_env = root / 'bash-env'
                shell_env.write_text('function [ { if [[ "$1" == -S ]]; then return 0; fi; builtin [ "$@"; }\n')
                env['BASH_ENV'] = str(shell_env)
                result = subprocess.run(['bash', str(LAUNCHER)], env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, 'inspection failure was treated as an inactive socket')
                self.assertTrue(path.exists(), 'inspection failure unlinked the socket')

    def test_failed_firmware_copy_is_not_reused(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            env = self.fixture(root)
            variables = root / 'vm' / 'efi_vars.fd'
            variables.unlink()
            copier = root / 'bin' / 'cp'
            copier.write_text('#!/bin/sh\nprintf partial > "$2"\nexit 1\n')
            copier.chmod(0o755)
            result = subprocess.run(['bash', str(LAUNCHER)], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(variables.exists(), 'failed copy published reusable partial firmware')
            self.assertEqual(list((root / 'vm').glob('efi_vars.fd.*')), [])

    def test_concurrent_initialization_preserves_the_winning_firmware(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            env = self.fixture(root)
            variables = root / 'vm' / 'efi_vars.fd'
            variables.unlink()
            template = root / 'template.fd'
            template.write_bytes(b'template')
            env['KETTLE_VM_EFI_VARS_TEMPLATE'] = str(template)
            ready = root / 'ready'
            resume = root / 'resume'
            copier = root / 'bin' / 'cp'
            copier.write_text('#!' + sys.executable + '\n' + '''
import os, shutil, sys, time
from pathlib import Path
shutil.copyfile(sys.argv[1], sys.argv[2])
if os.environ.get('COPY_WAIT'):
    Path(os.environ['COPY_READY']).touch()
    deadline = time.monotonic() + 10
    while not Path(os.environ['COPY_RESUME']).exists():
        if time.monotonic() >= deadline:
            sys.exit(1)
        time.sleep(0.01)
''')
            copier.chmod(0o755)
            with subprocess.Popen(
                ['bash', str(LAUNCHER)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                env={**env, 'COPY_WAIT': '1', 'COPY_READY': str(ready),
                     'COPY_RESUME': str(resume)},
            ) as delayed:
                try:
                    deadline = time.monotonic() + 5
                    while not ready.exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertTrue(ready.exists(), 'delayed launcher never reached the copy')
                    winner = subprocess.run(
                        ['bash', str(LAUNCHER)], env=env, capture_output=True,
                        text=True, timeout=5,
                    )
                    self.assertEqual(winner.returncode, 0, winner.stderr)
                    inode = variables.stat().st_ino
                    variables.write_bytes(b'live firmware writes')
                finally:
                    resume.touch()
                    _, error = delayed.communicate(timeout=5)
                self.assertEqual(delayed.returncode, 0, error)
            self.assertEqual(variables.stat().st_ino, inode, 'concurrent seed replaced live firmware inode')
            self.assertEqual(variables.read_bytes(), b'live firmware writes')
            self.assertEqual(list((root / 'vm').glob('efi_vars.fd.*')), [])

    def test_ssh_forward_binds_only_to_loopback(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            vm_dir = root / "vm"
            vm_dir.mkdir()
            (vm_dir / "ubuntu-arm.qcow2").write_bytes(b"fixture")
            (vm_dir / "efi_vars.fd").write_bytes(b"fixture")
            efi_code = root / "efi-code.fd"
            efi_code.write_bytes(b"fixture")

            arguments = root / "qemu-arguments"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake_qemu = fake_bin / "qemu-system-aarch64"
            fake_qemu.write_text(
                "#!/usr/bin/env bash\nprintf '%s\\0' \"$@\" > \"$KETTLE_VM_ARGS\"\n",
                encoding="utf-8",
            )
            fake_qemu.chmod(fake_qemu.stat().st_mode | stat.S_IXUSR)

            environment = os.environ.copy()
            environment.update(
                {
                    "KETTLE_VM_ARGS": str(arguments),
                    "KETTLE_VM_DIR": str(vm_dir),
                    "KETTLE_VM_EFI_CODE": str(efi_code),
                    "KETTLE_VM_SSH_PORT": "43210",
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                }
            )
            result = subprocess.run(
                ["bash", str(LAUNCHER)],
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            qemu_arguments = [
                value.decode("utf-8")
                for value in arguments.read_bytes().split(b"\0")
                if value
            ]
            netdev = qemu_arguments.index("-netdev")
            self.assertEqual(
                qemu_arguments[netdev + 1],
                "user,id=net0,hostfwd=tcp:127.0.0.1:43210-:22",
            )

    def test_qemu_option_paths_escape_commas(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            env = self.fixture(root)
            vm = root / 'vm, data'
            (root / 'vm').rename(vm)
            code = root / 'code,, firmware.fd'
            (root / 'code.fd').rename(code)
            arguments = root / 'arguments'
            env.update(KETTLE_VM_DIR=str(vm), KETTLE_VM_EFI_CODE=str(code),
                       KETTLE_VM_ARGS=str(arguments))
            (root / 'bin' / 'qemu-system-aarch64').write_text(
                '#!/bin/sh\nprintf "%s\\0" "$@" > "$KETTLE_VM_ARGS"\n'
            )
            result = subprocess.run(
                ['bash', str(LAUNCHER)], env=env, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            args = arguments.read_bytes().decode().split('\0')[:-1]
            drives = [args[i + 1] for i, arg in enumerate(args) if arg == '-drive']
            escaped_vm = root / 'vm,, data'
            self.assertEqual(drives, [
                f'if=pflash,format=raw,readonly=on,file={root / "code,,,, firmware.fd"}',
                f'if=pflash,format=raw,file={escaped_vm / "efi_vars.fd"}',
                f'if=none,id=hd0,file={escaped_vm / "ubuntu-arm.qcow2"},format=qcow2,discard=unmap,cache=writeback',
            ])
            self.assertEqual(args[args.index('-chardev') + 1],
                             f'socket,path={escaped_vm / "qemu-guest-agent.sock"},server=on,wait=off,id=qga0')


if __name__ == "__main__":
    unittest.main()
