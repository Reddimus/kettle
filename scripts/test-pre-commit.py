#!/usr/bin/env python3
"""Exercise the hook's path classification without running the Rust gate."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


class PreCommitTests(unittest.TestCase):
    def test_embedded_inputs_trigger_gate(self):
        paths = (
            "README.md",
            "docs/kettle.example.config",
            "docs/CONFIG.md",
            "docs/AGENT.md",
            "docs/SHELL-INTEGRATION.md",
            "docs/examples/init.lua",
            "packaging/linux/kettle-256.png",
            "packaging/linux/kettle-light-256.png",
            "packaging/linux/kettle.desktop",
            "packaging/linux/kettle.1",
            "packaging/macos/Info.plist",
            "packaging/windows/kettle.ico",
        )
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            for name, body in (
                ("git", 'printf "%s\\n" "$KETTLE_TEST_PATH"'),
                ("cargo", "exit 73"),
            ):
                executable = directory / name
                executable.write_text("#!/bin/sh\n" + body + "\n")
                executable.chmod(0o755)
            for path in (*paths, "docs/TESTING.md", "packaging/linux/README.md"):
                with self.subTest(path=path):
                    result = subprocess.run(
                        ["bash", str(ROOT / ".githooks/pre-commit")],
                        env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                             "KETTLE_TEST_PATH": path},
                        capture_output=True, text=True, check=False, timeout=10,
                    )
                    self.assertEqual(result.returncode, 73 if path in paths else 0,
                                     f"incorrect gate classification for {path}: {result.stderr}")


if __name__ == "__main__":
    unittest.main()
