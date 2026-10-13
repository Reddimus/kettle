#!/usr/bin/env python3
"""Check native fullscreen admission and failure diagnostics without a desktop."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "titlebar_check", Path(__file__).with_name("check-native-titlebar.py")
)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class Window:
    pid = 123

    def __init__(self, surface):
        self.surface = surface
        self.actions = []

    def json_ctl(self, method, params=None):
        if method == "perform_action":
            self.actions.append(params["action"])
            return {}
        assert method == "ui_geometry"
        return {"surface": self.surface}


class FullscreenTests(unittest.TestCase):
    def setUp(self):
        clock = Clock()
        for name, value in (("monotonic", clock.monotonic), ("sleep", clock.sleep)):
            patcher = patch.object(checker.time, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_native_state_requires_a_boolean(self):
        window = Window({"width": 740, "height": 480})
        for value in (True, False):
            with patch.object(window, "json_ctl", return_value={"fullscreen": value}):
                self.assertEqual(checker.native_fullscreen(window), value)
        with patch.object(window, "json_ctl", return_value={"fullscreen": "false"}):
            with self.assertRaises(AssertionError):
                checker.native_fullscreen(window)

    def test_zoom_is_not_fullscreen_entry(self):
        window = Window({"width": 740, "height": 480})
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(checker.modal, "focus"), \
                patch.object(checker.modal.live_ui, "run") as run, \
                patch.object(checker, "native_fullscreen", return_value=False):
            result = run.return_value
            result.check_returncode.return_value = None
            def press(*_args, **_kwargs):
                window.surface = {"width": 800, "height": 500}
                return result
            run.side_effect = press
            with self.assertRaisesRegex(AssertionError, "native fullscreen did not become True"):
                checker.native_fullscreen_roundtrip(window, Path(directory))
        self.assertEqual(window.actions, [], "an unconfirmed entry must not toggle the window")

    def test_restoration_failure_reports_the_observed_size(self):
        window = Window({"width": 800, "height": 500})
        with patch.object(checker, "native_fullscreen", return_value=False):
            with self.assertRaisesRegex(AssertionError, "left native fullscreen.*800.*500"):
                checker.fullscreen_restore(window, {"width": 740, "height": 480})
        self.assertEqual(window.actions, ["toggle_fullscreen"])

    def test_geometry_alone_cannot_confirm_a_fullscreen_exit(self):
        wanted = {"width": 740, "height": 480}
        window = Window(wanted)
        with patch.object(checker, "native_fullscreen", return_value=True):
            with self.assertRaisesRegex(AssertionError, "native fullscreen did not become False"):
                checker.fullscreen_restore(window, wanted)
        self.assertEqual(window.actions, ["toggle_fullscreen"])

    def test_a_successful_restore_toggles_only_once(self):
        wanted = {"width": 740, "height": 480}
        window = Window(wanted)
        with patch.object(checker, "native_fullscreen", return_value=False):
            self.assertEqual(checker.fullscreen_restore(window, wanted), wanted)
        self.assertEqual(window.actions, ["toggle_fullscreen"])


if __name__ == "__main__":
    unittest.main()
