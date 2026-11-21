#!/usr/bin/env python3
"""Check native fullscreen admission and failure diagnostics without a desktop."""

import importlib.util
import json
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


class BorderlessWindow(Window):
    def __init__(self, restored_height=480):
        super().__init__({"width": 740, "height": 480})
        self.fullscreen = False
        self.restored_height = restored_height

    def json_ctl(self, method, params=None):
        if method == "perform_action":
            self.actions.append(params["action"])
            self.fullscreen = not self.fullscreen
            self.surface = ({"width": 1024, "height": 768} if self.fullscreen else
                            {"width": 740, "height": self.restored_height})
            return {}
        assert method == "ui_geometry"
        return {"surface": self.surface, "fullscreen": self.fullscreen}


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

    def test_borderless_restore_rejects_pixels_from_a_temporary_caption(self):
        window = BorderlessWindow(restored_height=512)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(AssertionError, "did not restore.*740.*480"):
                checker.programmatic_fullscreen_roundtrip(window, Path(directory))
            trace = json.loads((Path(directory) / "borderless-fullscreen-state.json").read_text())
            self.assertEqual(trace["after"]["surface"]["height"], 512)
            # The window server's own bounds sit beside the surface, so a stale
            # surface and a window that never left fullscreen read apart.
            self.assertIn("appkit_bounds_after", trace)
        self.assertEqual(window.actions, ["toggle_fullscreen", "toggle_fullscreen"])

    def test_borderless_restore_uses_one_action_per_transition(self):
        window = BorderlessWindow()
        with tempfile.TemporaryDirectory() as directory:
            result = checker.programmatic_fullscreen_roundtrip(window, Path(directory))
        self.assertEqual(result["restored_surface"], {"width": 740, "height": 480})
        self.assertEqual(window.actions, ["toggle_fullscreen", "toggle_fullscreen"])


class TitlebarGeometryTests(unittest.TestCase):
    def fixture(self, scale=1):
        def rect(x, y, width, height):
            return dict(x=x * scale, y=y * scale, width=width * scale, height=height * scale)
        return {
            "scale_factor": scale, "fullscreen": False,
            "surface": {"width": 800 * scale, "height": 600 * scale},
            "content": rect(0, 28, 800, 572),
            "tab_bar": {"in_titlebar": True, "y": 0, "height": 28 * scale,
                        "band": rect(78, 0, 722, 28),
                        "new_tab": rect(736, 0, 28, 28),
                        "new_tab_menu": rect(764, 0, 28, 28),
                        "segments": [{"rect": rect(78, 0, 560, 28),
                                      "title_rect": rect(86, 0, 544, 28),
                                      "close": rect(638, 0, 0, 0),
                                      "hint": None, "hovered": False,
                                      "fitted_title": "",
                                      "label": "Kettle caption fixture"}]},
        }

    def test_single_tab_rejects_idle_close_and_shortcut_hint(self):
        for control in ("close", "hint"):
            geometry = self.fixture()
            tab = geometry["tab_bar"]["segments"][0]
            tab["hovered"] = False
            tab["close"] = dict(x=560, y=0, width=0, height=0)
            tab["hint"] = None
            if control == "close":
                tab["close"].update(x=532, width=28, height=28)
            else:
                tab["hint"] = {"text": "Cmd+1", "rect": dict(x=492, y=0, width=40, height=28)}
            with self.assertRaises(AssertionError):
                checker.titlebar_geometry(geometry)

    def test_single_tab_hover_and_two_tabs_allow_controls(self):
        for scale in (1, 2):
            for count in (1, 2):
                geometry = self.fixture(scale)
                tab = geometry["tab_bar"]["segments"][0]
                tab["hovered"] = count == 1
                tab["close"].update(x=452 * scale, width=28 * scale, height=28 * scale)
                if count == 2:
                    def segment(x, index):
                        return dict(tab, hovered=False,
                                    rect=dict(x=x * scale, y=0, width=280 * scale, height=28 * scale),
                                    title_rect=dict(x=(x + 8) * scale, y=0,
                                                    width=188 * scale, height=28 * scale),
                                    close=dict(x=(x + 252) * scale, y=0,
                                               width=28 * scale, height=28 * scale),
                                    hint={"text": f"Cmd+{index}", "rect": dict(
                                        x=(x + 204) * scale, y=0, width=40 * scale, height=28 * scale)})
                    geometry["tab_bar"]["segments"] = [segment(78, 1), segment(358, 2)]
                checker.titlebar_geometry(geometry)

    def test_tabs_start_the_lane_split_it_evenly_and_leave_a_drag_lane(self):
        def shift(g):
            g["tab_bar"]["segments"][0]["rect"].update(x=320)
            g["tab_bar"]["segments"][0]["title_rect"].update(x=328)
        def crowd(g):
            g["tab_bar"]["segments"][0]["rect"].update(width=680)
            g["tab_bar"]["segments"][0]["title_rect"].update(width=600)
        def uneven(g):
            tab = g["tab_bar"]["segments"][0]
            other = dict(tab, rect=dict(tab["rect"], x=338, width=300),
                         title_rect=dict(tab["title_rect"], x=346, width=284))
            tab["rect"].update(width=260)
            tab["title_rect"].update(width=244)
            g["tab_bar"]["segments"].append(other)
        def scrolled_into_the_lane(g):
            g["tab_bar"]["scroll_right"] = dict(x=700, y=0, width=28, height=28)
        for mutate in (shift, crowd, uneven, scrolled_into_the_lane):
            geometry = self.fixture()
            mutate(geometry)
            with self.assertRaises(AssertionError, msg=mutate.__name__):
                checker.titlebar_geometry(geometry)
        # `›` right after the tabs shown keeps the lane.
        geometry = self.fixture()
        geometry["tab_bar"]["scroll_right"] = dict(x=638, y=0, width=28, height=28)
        geometry["tab_bar"]["segments"][0]["rect"].update(width=532)
        geometry["tab_bar"]["segments"][0]["title_rect"].update(width=516)
        checker.titlebar_geometry(geometry)

    def test_titlebar_geometry_uses_scaled_band(self):
        for scale in (1, 2):
            geometry = self.fixture(scale)
            checker.titlebar_geometry(geometry)
            geometry["fullscreen"] = True
            geometry["tab_bar"]["band"]["x"] = 0
            # Without the traffic lights the lone tab starts at the edge.
            with self.assertRaises(AssertionError):
                checker.titlebar_geometry(geometry)
            tab = geometry["tab_bar"]["segments"][0]
            tab["rect"].update(x=0)
            tab["title_rect"].update(x=8 * scale)
            checker.titlebar_geometry(geometry)

    def test_settings_gear_must_sit_apart_from_the_new_tab_pair(self):
        for scale in (1, 2):
            def gear(x, width=28):
                return dict(x=x * scale, y=0, width=width * scale, height=28 * scale)

            geometry = self.fixture(scale)
            geometry["tab_bar"]["settings"] = gear(0, 0)
            checker.titlebar_geometry(geometry)
            geometry["tab_bar"]["settings"] = gear(700)
            checker.titlebar_geometry(geometry)
            for bad in (gear(704), gear(700, 16), gear(70)):
                geometry["tab_bar"]["settings"] = bad
                with self.assertRaises(AssertionError):
                    checker.titlebar_geometry(geometry)

    def test_titlebar_geometry_rejects_overlap_missing_title_and_small_controls(self):
        for mutate in (
            lambda g: g["tab_bar"]["new_tab_menu"].update(x=765),
            lambda g: g["content"].update(y=0),
            lambda g: g["tab_bar"]["segments"][0]["rect"].update(x=0),
            lambda g: g["tab_bar"]["segments"][0]["title_rect"].update(x=0),
            lambda g: g["tab_bar"]["segments"][0].update(label=" "),
            lambda g: g["tab_bar"]["new_tab"].update(width=16),
        ):
            geometry = self.fixture()
            mutate(geometry)
            with self.assertRaises(AssertionError):
                checker.titlebar_geometry(geometry)


if __name__ == "__main__":
    unittest.main()
