#!/usr/bin/env python3
"""Run the interactive Fish fixtures with a nontrivial integration path."""

import importlib.util
import os
from pathlib import Path
import shutil
import sys
import tempfile
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
    def test_interactive_default_and_vi_source_paths_with_spaces(self):
        with tempfile.TemporaryDirectory() as raw:
            integration = Path(raw) / "Kettle Source's $integration"
            integration.mkdir()
            shutil.copy2(MODULE.INTEGRATION / "kettle.fish", integration / "kettle.fish")
            with mock.patch.object(MODULE, "INTEGRATION", integration):
                MODULE.check_fish(shutil.which("fish"))


if __name__ == "__main__":
    unittest.main()
