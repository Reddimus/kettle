#!/usr/bin/env python3
"""Exercise portable recipes with captured native-command arguments."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(shutil.which("just"), "requires just")
class JustRecipeTests(unittest.TestCase):
    def test_windows_titlebar_check_uses_python(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            source = (ROOT / "Justfile").read_text().replace(
                'os_family() == "windows"', 'os_family() == "unix"'
            )
            justfile = directory / "Justfile"
            justfile.write_text(source)
            for name, status in (("python", 0), ("python3", 73)):
                executable = directory / name
                executable.write_text(f"#!/bin/sh\nexit {status}\n")
                executable.chmod(0o755)
            result = subprocess.run(
                ["just", "--justfile", str(justfile), "native-titlebar-self-test"],
                env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}", "TEMP": raw},
                capture_output=True, text=True, check=False, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_screenshot_paths_are_one_argument(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            log = directory / "arguments.json"
            cargo = directory / "cargo"
            cargo.write_text(
                f"#!{sys.executable}\nimport json, os, sys\n"
                "from pathlib import Path\n"
                "Path(os.environ['KETTLE_TEST_LOG']).write_text(json.dumps(sys.argv[1:]))\n"
            )
            cargo.chmod(0o755)
            environment = {**os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                           "KETTLE_TEST_LOG": str(log), "TEMP": raw}
            for shell in ("sh", "pwsh"):
                if not shutil.which(shell):
                    continue
                source = (ROOT / "Justfile").read_text()
                if shell == "pwsh":
                    source = source.replace('os_family() == "windows"', 'os_family() == "unix"')
                justfile = directory / "Justfile"
                justfile.write_text(source)
                for recipe, flag in (("screenshot", "--screenshot"), ("menu", "--screenshot-menu")):
                    for filename in ("kettle frame.png", "kettle frame's $literal.png"):
                        with self.subTest(shell=shell, recipe=recipe, filename=filename):
                            output = str(directory / filename)
                            result = subprocess.run(
                                ["just", "--justfile", str(justfile), "--shell", shell,
                                 "--clear-shell-args", f"--shell-arg={'-Command' if shell == 'pwsh' else '-c'}",
                                 recipe, output],
                                env=environment, capture_output=True, text=True, check=False, timeout=15,
                            )
                            self.assertEqual(result.returncode, 0, result.stderr)
                            self.assertEqual(json.loads(log.read_text())[-2:], [flag, output])


if __name__ == "__main__":
    unittest.main()
