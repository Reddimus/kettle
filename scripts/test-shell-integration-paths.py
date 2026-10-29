#!/usr/bin/env python3
"""Regression tests for shell editor state and integration source paths."""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("check-shell-integration.py")
SPEC = importlib.util.spec_from_file_location("shell_check", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


@unittest.skipUnless(os.name == "posix" and shutil.which("fish"), "Unix Fish required")
class FishIntegrationPathTests(unittest.TestCase):
    def test_newline_to_space_edit_invalidates_completion_cycle(self):
        result = MODULE.run(
            [
                shutil.which("fish"), "--no-config", "-c", r'''
source $argv[1] >/dev/null
set -g KETTLE_BUFFER "echo x
he"
set -g KETTLE_CALLS 0
function commandline
    switch $argv[1]
        case -b -cp
            printf '%s\n' "$KETTLE_BUFFER"
        case -C
            string length -- "$KETTLE_BUFFER"
        case -ct
            printf he
        case -rt
            set -g KETTLE_BUFFER "$argv[-1]"
    end
end
function __kettle_completion_rows
    set -g KETTLE_CALLS (math $KETTLE_CALLS + 1)
    printf 'hello\tfirst\nhelp\tsecond\n'
end
__kettle_completion_cycle 1
set -g KETTLE_BUFFER 'echo x he'
__kettle_completion_cycle 1
printf 'PROVIDER_CALLS=%s;BUFFER=<%s>' $KETTLE_CALLS "$KETTLE_BUFFER"
''', str(MODULE.INTEGRATION / "kettle.fish"),
            ],
            "Fish multiline completion cycle", announce=False,
        )
        self.assertIn(b"PROVIDER_CALLS=2;BUFFER=<echo x he>", result,
                      "a newline-to-space edit must query the provider again")

    def test_interactive_default_and_vi_source_paths_with_spaces(self):
        with tempfile.TemporaryDirectory() as raw:
            integration = Path(raw) / "Kettle Source's $integration"
            integration.mkdir()
            shutil.copy2(MODULE.INTEGRATION / "kettle.fish", integration / "kettle.fish")
            with mock.patch.object(MODULE, "INTEGRATION", integration):
                MODULE.check_fish(shutil.which("fish"))


@unittest.skipUnless(os.name == "posix" and shutil.which("pwsh"), "Unix pwsh required")
class PowerShellAcceptanceTests(unittest.TestCase):
    def test_incomplete_block_does_not_start_command_during_editing(self):
        self.check_command_start("")

    def test_strict_mode_still_marks_and_submits_commands(self):
        # A valid line leaves an empty parse-error array, whose missing
        # IncompleteInput property strict mode refuses to read.
        self.check_command_start("Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; ")

    def check_command_start(self, preamble):
        import fcntl
        import pty
        import select
        import struct
        import termios

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
        integration = str(MODULE.INTEGRATION / "kettle.ps1").replace("'", "''")
        process = subprocess.Popen(
            [shutil.which("pwsh"), "-NoLogo", "-NoProfile", "-NoExit", "-Command",
             preamble + "Import-Module PSReadLine; "
             "Set-PSReadLineOption -HistorySaveStyle SaveNothing -PredictionSource None; "
             "[Microsoft.PowerShell.PSConsoleReadLine]::ClearHistory(); "
             f". '{integration}'; function global:prompt {{ 'KETTLE_READY> ' }}"],
            stdin=slave, stdout=slave, stderr=slave,
            env={**os.environ, "TERM": "xterm-256color"},
        )
        os.close(slave)

        def drain(seconds, until=None):
            output = b""
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if not select.select([master], [], [], min(0.1, max(0, deadline - time.monotonic())))[0]:
                    continue
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
                while b"\x1b[6n" in output:
                    output = output.replace(b"\x1b[6n", b"", 1)
                    os.write(master, b"\x1b[1;1R")
                if until is not None and until in output:
                    break
            return output

        try:
            ready = drain(15, b"KETTLE_READY>")
            self.assertIn(b"KETTLE_READY>", ready)
            drain(0.3)
            os.write(master, b"if ($true) {\r")
            editing = drain(0.6)
            self.assertNotIn(MODULE.OSC_C, editing,
                             "incomplete input must not emit a command-start mark")
            os.write(master, b"Write-Output ('KETTLE_' + 'ACCEPTED')\r")
            editing = drain(0.6)
            self.assertNotIn(MODULE.OSC_C, editing)
            os.write(master, b"}\r")
            submitted = drain(10, b"KETTLE_ACCEPTED")
            self.assertIn(b"KETTLE_ACCEPTED", submitted)
            self.assertEqual(submitted.count(MODULE.OSC_C), 1)
            # A one-line command is complete at once.
            drain(0.3)
            os.write(master, b"Write-Output ('KETTLE_' + 'SINGLE')\r")
            single = drain(10, b"KETTLE_SINGLE")
            self.assertIn(b"KETTLE_SINGLE", single)
            self.assertEqual(single.count(MODULE.OSC_C), 1)
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            os.close(master)


if __name__ == "__main__":
    unittest.main()
