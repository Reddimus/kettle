#!/usr/bin/env python3
"""GUI-free regressions for cursor blink smoke geometry, samples and captures."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("cursor_smoke", Path(__file__).with_name("check-cursor-blink-layer-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class CaptureClock:
    """A deterministic blinking window, without a GUI or child processes."""
    pid = 456

    def __init__(self, query_delays=(), capture_delays=(), renderers=(), exits=()):
        self.now = 0.15
        self.query_delays = iter(query_delays)
        self.capture_delays = iter(capture_delays)
        # Per-query renderer and exit count; afterwards the layer, unchanged.
        self.renderers = iter(renderers)
        self.exits = iter(exits)
        self.queries = 0
        self.captures = []

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += max(seconds, 0.001)

    def phase(self):
        return int(self.now / 0.3) % 2 == 0

    def json_ctl(self, method):
        self.queries += 1
        phase = self.phase()
        edge = (int(self.now / 0.3) + 1) * 0.3
        wait_ms = int((edge - self.now) * 1000)
        self.now += next(self.query_delays, 0)
        return {"cursor_blink": {"renderer": next(self.renderers, "layer"), "next_edge_ms": wait_ms,
                                 "phase_on": phase, "handoffs": 1, "exits": next(self.exits, 0), "hides": 0}}

    @staticmethod
    def native_visible_window_ids(pid):
        return {123}

    def capture(self, argv, **kwargs):
        path = Path(argv[-1])
        assert kwargs.get("umask") == 0o077
        assert path.stat().st_mode & 0o777 == 0o600
        self.captures.append((path, self.now, self.queries))
        delay = next(self.capture_delays, 0.005)
        timeout = kwargs.get("timeout")
        if timeout is not None and delay > timeout:
            # As subprocess.run does: the child is killed at the timeout, and
            # reaping it takes a moment more.
            self.now += timeout + 0.001
            raise smoke.subprocess.TimeoutExpired(argv, timeout)
        self.now += delay
        path.write_text(str(self.phase()))

    def patches(self):
        from contextlib import ExitStack
        stack = ExitStack()
        stack.enter_context(patch.object(smoke.time, "monotonic", self.monotonic))
        stack.enter_context(patch.object(smoke.time, "sleep", self.sleep))
        stack.enter_context(patch.object(smoke.subprocess, "run", self.capture))
        return stack


class CursorSmoke(unittest.TestCase):
    def test_cursor_rect_uses_the_wire_object_and_row_column_order(self):
        class Live:
            def json_ctl(self, method):
                return {
                    "read_screen": {"cursor": [3, 4]},
                    "ui_geometry": {"panes": [{"focused": True, "rect": {"x": 10, "y": 20}}],
                                    "cell": {"width": 8, "height": 16}, "scale_factor": 2,
                                    "padding": {"x": 2, "y": 3}},
                }[method]
        self.assertEqual(smoke.expected_cursor_rect(Live()), (46, 74, 8, 16))

    def test_idle_metrics_require_three_seconds(self):
        first = {"t": 10, "footprint_mib": 50, "wakeups": 4, "cpu_ns": 0}
        for span in [0, 1, 2.99]:
            with self.assertRaises(SystemExit):
                smoke.idle_metrics([first, {**first, "t": 10 + span}])
        last = {"t": 14, "footprint_mib": 60, "wakeups": 6, "cpu_ns": 400_000}
        self.assertEqual(smoke.idle_metrics([first, last]),
                         {"span_s": 4, "peak_mib": 60, "wakeups_per_s": 0.5, "cpu_percent": 0.01})

    def test_pixel_captures_are_private_and_removed_on_success_or_failure(self):
        for fails in [False, True]:
            with tempfile.TemporaryDirectory() as directory:
                seen = []
                def compare(helpers, kettle, out):
                    seen.append(out)
                    self.assertEqual(out.stat().st_mode & 0o777, 0o700)
                    clock = CaptureClock()
                    with clock.patches():
                        paths = smoke.capture_phases(clock, clock, out, "private", "layer")
                    self.assertEqual(len(paths), 2)
                    for path in paths.values():
                        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                    if fails:
                        raise SystemExit("pixel mismatch")
                    return {"same": True}
                with patch.object(smoke, "compare_pixels", compare):
                    if fails:
                        with self.assertRaises(SystemExit):
                            smoke.run_pixels(None, "unused", Path(directory))
                    else:
                        self.assertEqual(smoke.run_pixels(None, "unused", Path(directory)), {"same": True})
                self.assertFalse(seen[0].exists())

    def test_pixel_capture_retries_delayed_response_and_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            # The first reply arrives in the opposite phase. The first capture
            # spans two edges and ends in the same phase, so phase equality
            # alone cannot prove that capture is safe.
            clock = CaptureClock(query_delays=[0.2], capture_delays=[0.65])
            with clock.patches():
                paths = smoke.capture_phases(clock, clock, Path(directory), "delay", "layer")
            self.assertEqual(set(paths), {False, True})
            self.assertGreater(clock.captures[0][2], 1, "stale reply must be retried before capture")
            self.assertEqual(len(clock.captures), 3, "the long capture must be discarded and retried")
            for phase, path in paths.items():
                self.assertEqual(path.read_text(), str(phase), "capture must match its phase label")
            self.assertEqual(set(Path(directory).glob("*.png")), set(paths.values()))

    def test_pixel_capture_rejects_persistent_delay_within_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            clock = CaptureClock(capture_delays=[0.65] * 30)
            with clock.patches(), self.assertRaisesRegex(SystemExit, "could not capture both phases"):
                smoke.capture_phases(clock, clock, Path(directory), "slow", "layer")
            self.assertLess(clock.now, 11.0)
            self.assertEqual(list(Path(directory).glob("*.png")), [])

    def test_pixel_capture_cut_off_by_the_deadline_reports_the_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            # A capture still running when the budget ends is killed, and the
            # smoke says it could not capture rather than raising a timeout.
            clock = CaptureClock(capture_delays=[30.0])
            with clock.patches(), self.assertRaisesRegex(SystemExit, "could not capture both phases"):
                smoke.capture_phases(clock, clock, Path(directory), "cut-off", "layer")
            self.assertEqual(len(clock.captures), 1)
            self.assertEqual(list(Path(directory).glob("*.png")), [])

    def test_pixel_capture_requires_the_expected_renderer_throughout(self):
        with tempfile.TemporaryDirectory() as directory:
            # A window that went back to the GPU blink never certifies the layer.
            clock = CaptureClock(renderers=["gpu"] * 10_000)
            with clock.patches(), self.assertRaisesRegex(SystemExit, "could not capture both phases"):
                smoke.capture_phases(clock, clock, Path(directory), "gpu-only", "layer")
            self.assertEqual(clock.captures, [])
        with tempfile.TemporaryDirectory() as directory:
            # The blink left the layer and came back during the first capture:
            # both reads say layer, but the exit count moved, so it is retried.
            clock = CaptureClock(exits=[0, 1])
            with clock.patches():
                paths = smoke.capture_phases(clock, clock, Path(directory), "exited", "layer")
            self.assertEqual(set(paths), {False, True})
            self.assertEqual(len(clock.captures), 3, "the capture that spanned an exit must be retried")
            self.assertEqual(set(Path(directory).glob("*.png")), set(paths.values()))

    def test_captures_must_show_the_blink_before_they_can_match(self):
        surface = {"width": 4, "height": 2}
        rect = (1, 0, 2, 2)
        def image(lit):
            row = bytearray(16)
            if lit:
                row[4:12] = b"\xff" * 8
            return (4, 2, [bytes(row), bytes(row)])
        blank = {layer: {True: image(False), False: image(False)} for layer in (True, False)}
        for style in ("opaque", "translucent"):
            with self.assertRaisesRegex(SystemExit, "show no blink"):
                smoke.check_captures("blank", style, blank, rect, surface)
        # The GPU run alone not blinking fails too.
        half = {True: {True: image(True), False: image(False)}, False: {True: image(False), False: image(False)}}
        with self.assertRaisesRegex(SystemExit, "GPU captures show no blink"):
            smoke.check_captures("half", "opaque", half, rect, surface)
        good = {layer: {True: image(True), False: image(False)} for layer in (True, False)}
        for style in ("opaque", "translucent"):
            results = smoke.check_captures("good", style, good, rect, surface)
            self.assertTrue(all(results.values()))
            self.assertIn("good-layer-blinks", results)
            self.assertIn("good-on", results)
        with self.assertRaisesRegex(SystemExit, "capture does not match"):
            smoke.check_captures("size", "opaque", good, rect, {"width": 5, "height": 2})
        differs = {True: {True: image(True), False: image(False)},
                   False: {True: (4, 2, [b"\xff" * 16, b"\xff" * 16]), False: image(False)}}
        with self.assertRaisesRegex(SystemExit, "differs between the layer and the GPU blink"):
            smoke.check_captures("differs", "opaque", differs, rect, surface)


if __name__ == "__main__":
    unittest.main()
