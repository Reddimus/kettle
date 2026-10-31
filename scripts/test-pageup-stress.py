#!/usr/bin/env python3
"""Portable negative controls for the native page-up qualification."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "pageup_stress", Path(__file__).with_name("check-pageup-stress.py")
)
stress = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stress)


class FakeLive:
    def __init__(self, fail_mode=None, fail_direction=None):
        self.offset = 0
        self.fail_mode = fail_mode
        self.fail_direction = fail_direction

    def ctl(self, method, *, params):
        if method != "send_text":
            raise AssertionError(method)

    def move(self, mode, up):
        if (mode, up) != (self.fail_mode, self.fail_direction):
            self.offset = 24 if up else 0

    def json_ctl(self, method, params=None):
        if method == "read_screen":
            return {"text": "PAGEUP_OUTPUT_DONE_00\n$ ", "history_size": 1605,
                    "display_offset": self.offset}
        if method == "perform_action":
            self.move("control", params["action"] == "scroll_page_up")
            return {}
        if method == "dispatch_keybind":
            up = params["logical"] == "pageup"
            self.move("keybind", up)
            return dict(action="ScrollPageUp" if up else "ScrollPageDown")
        raise AssertionError(method)

    def screenshot(self, _path):
        pass

    def key(self, _live, code, flags):
        if flags != 1 << 17:
            raise AssertionError(flags)
        self.move("native", code == 116)


class PageUpTests(unittest.TestCase):
    def test_stalled_version_probe_times_out(self):
        with patch.object(stress, "VERSION_TIMEOUT_S", 0.03):
            with self.assertRaises(subprocess.TimeoutExpired):
                stress.probe_version([sys.executable, "-c", "import time; time.sleep(0.3)"])

    def test_startup_failure_keeps_load_and_error(self):
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw) / "result"
            with patch.object(stress.platform, "system", return_value="Darwin"), \
                    patch.object(stress, "probe_version", return_value="fixture"), \
                    patch.object(stress.os, "getloadavg", return_value=(1.0, 2.0, 3.0), create=True), \
                    patch.object(stress.modal, "RecordedLive") as context:
                context.return_value.__enter__.side_effect = RuntimeError("startup failed")
                with self.assertRaisesRegex(RuntimeError, "startup failed"):
                    stress.check(Path(sys.executable), out, 1)
            result = json.loads((out / "results.json").read_text())[0]
            self.assertEqual(result["result"], "FAIL")
            self.assertEqual(result["load_on_failure"], [1.0, 2.0, 3.0])
            self.assertIn("startup failed", result["error"])

    def test_ready_marker_must_be_output_not_an_echo(self):
        live = FakeLive()
        with patch.object(live, "json_ctl", return_value={
            "text": "printf 'PAGEUP_OUTPUT_DONE_00'", "history_size": 1605,
        }):
            with self.assertRaisesRegex(RuntimeError, "fixture is not ready"):
                stress.wait_for_scrollback(live, "PAGEUP_OUTPUT_DONE_00", timeout=0)

    def test_ready_marker_without_history_is_rejected(self):
        live = FakeLive()
        with patch.object(live, "json_ctl", return_value={
            "text": "PAGEUP_OUTPUT_DONE_00\n", "history_size": 0,
        }):
            with self.assertRaisesRegex(RuntimeError, "fixture is not ready"):
                stress.wait_for_scrollback(live, "PAGEUP_OUTPUT_DONE_00", timeout=0)

    def test_each_input_path_must_move_both_directions(self):
        for mode in ["control", "keybind", "native"]:
            for direction in [True, False]:
                with self.subTest(mode=mode, up=direction), tempfile.TemporaryDirectory() as raw:
                    live = FakeLive(mode, direction)
                    with patch.object(stress.time, "sleep"):
                        with self.assertRaisesRegex(RuntimeError, mode + " page-"):
                            stress.exercise(live, Path(raw), 0, live.key)

    def test_success_measures_all_paths(self):
        live = FakeLive()
        with tempfile.TemporaryDirectory() as raw, patch.object(stress.time, "sleep"):
            pairs = stress.exercise(live, Path(raw), 0, live.key)
        self.assertEqual(len(pairs), 20)
        self.assertEqual({p["mode"] for p in pairs}, {"control", "keybind", "native"})
        self.assertTrue(all(p["up_offset"] > 0 and p["down_offset"] == 0 for p in pairs))


if __name__ == "__main__":
    unittest.main()
