#!/usr/bin/env python3
"""Focused regression tests for the QEMU Ubuntu ARM launcher."""

from __future__ import annotations

import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
LAUNCHER = ROOT / "scripts" / "vm" / "run-ubuntu-arm.sh"


@unittest.skipIf(os.name == "nt", "the launcher targets QEMU/HVF on macOS")
class VmLauncherTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
