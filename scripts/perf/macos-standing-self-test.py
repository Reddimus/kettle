#!/usr/bin/env python3
"""GUI-free checks for macos-standing.py's parsing, statistics, and vtebench fix."""

from __future__ import annotations

import importlib.util
import json as _json
import math
import os
import random
import shutil
import shlex
import signal
import subprocess
import sys
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SNAPSHOT_LAYOUT = (HERE / "snapshot.gitignore").is_file()
if SNAPSHOT_LAYOUT:
    # Per-file review snapshots have no workspace and must keep test writes local.
    import tempfile
    tempfile.tempdir = str(HERE / ".test-tmp")
    Path(tempfile.tempdir).mkdir(exist_ok=True)
    for variable, directory in (("TMPDIR", ".test-tmp"), ("CLANG_MODULE_CACHE_PATH", ".cache/clang"),
                                ("SWIFT_MODULECACHE_PATH", ".cache/swift")):
        cache = HERE / directory
        cache.mkdir(parents=True, exist_ok=True)
        os.environ[variable] = str(cache)


def gone_within(pid: int, timeout: float = 5.0) -> bool:
    """Whether pid stops existing within timeout. A child orphaned when its
    probe is SIGKILLed is reaped by launchd, not by the harness, so it can
    linger as a zombie for a moment after the probe itself is reaped. The
    timeout is far below the child's 60 s sleep, so this still proves the
    kill reached it."""
    import os

    deadline = time.monotonic() + timeout
    while True:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        if time.monotonic() > deadline:
            return False
        time.sleep(0.01)


json_dumps = _json.dumps
spec = importlib.util.spec_from_file_location("macos_standing", HERE / "macos-standing.py")
standing = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(standing)


class ParseDat(unittest.TestCase):
    def test_medians_per_benchmark_and_skips_missing_samples(self) -> None:
        dat = "dense_cells unicode\n10 7\n12 _\n11 9\n"
        self.assertEqual(standing.parse_dat(dat), {"dense_cells": 11, "unicode": 8})

    def test_empty_file_has_no_benchmarks(self) -> None:
        self.assertEqual(standing.parse_dat(""), {})


class Statistics(unittest.TestCase):
    def test_geometric_mean(self) -> None:
        self.assertAlmostEqual(standing.geometric_mean([2.0, 8.0]), 4.0)

    def test_paired_ratio_and_interval(self) -> None:
        a = [100.0, 102.0, 98.0, 101.0, 99.0]
        b = [60.0, 61.0, 59.0, 60.0, 60.5]
        stats = standing.paired(a, b)
        self.assertEqual(stats["n"], 5)
        self.assertLess(stats["low"], stats["ratio"])
        self.assertLess(stats["ratio"], stats["high"])
        self.assertAlmostEqual(stats["ratio"], 0.6, places=2)

    def test_paired_skips_missing_values(self) -> None:
        self.assertEqual(standing.paired([None], [1.0]), {})
        # A zero is a value, not a missing round (see test_zero_values_stay_in_the_pairs).
        self.assertEqual(standing.paired([0.0], [1.0])["ratio"], math.inf)

    def test_rounds_rotate_the_starting_terminal(self) -> None:
        names = ["kettle", "alacritty", "kitty"]
        self.assertEqual(standing.rotated(names, 0), names)
        self.assertEqual(standing.rotated(names, 1), ["alacritty", "kitty", "kettle"])
        self.assertEqual(standing.rotated(names, 3), names)


class LaunchFailure(unittest.TestCase):
    def test_a_failed_launch_reports_an_error_not_the_previous_round(self) -> None:
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            (work / "launch.json").write_text('{"window_ms": 1.0}')
            probe = work / "fail.sh"
            probe.write_text("#!/bin/sh\nexit 2\n")
            probe.chmod(0o755)
            stamp = work / "stamp.sh"
            stamp.write_text("#!/bin/sh\n")
            stamp.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": stamp}, work, {"kettle": "/bin/true"})
            process = runner.launch("kettle", "true", 1)
            result = runner.finish(process, 1)
            self.assertIn("error", result)
            self.assertNotIn("window_ms", result)


def dat(rows):
    """A vtebench DAT file from {benchmark: [samples]}; short columns pad with `_`."""
    names = list(rows)
    depth = max(len(values) for values in rows.values())
    lines = [" ".join(names)]
    for index in range(depth):
        lines.append(" ".join(str(rows[name][index]) if index < len(rows[name]) else "_" for name in names))
    return "\n".join(lines) + "\n"


class Summary(unittest.TestCase):
    def test_tables_report_medians_means_and_the_vtebench_geometric_mean(self) -> None:
        results = {
            "context": "test host",
            "workloads": {
                "startup": {
                    "kettle": [{"window_ms": 100.0, "killed": False, "cols": 123, "rows": 35},
                               {"window_ms": 120.0}],
                    "alacritty": [{"window_ms": 150.0}, {"error": "no window"}],
                },
                "vtebench": {
                    "kettle": [standing.vtebench_row(dat({"dense_cells": [8], "scrolling": [18]}), "ms")],
                    "alacritty": [standing.vtebench_row(dat({"dense_cells": [6], "scrolling": [40]}), "ms")],
                },
            },
        }
        text = standing.summarize(results, ["kettle", "alacritty"], ab=False)
        self.assertIn("| window_ms | 110.00 | 150.00 |", text)
        self.assertNotIn("killed", text, "booleans are not metrics")
        self.assertNotIn("| cols |", text, "the grid is not a metric")
        self.assertIn("Grid: kettle 123x35", text)
        self.assertIn("| **geometric mean** | 12.0 | 15.5 |", text)

    def test_a_config_only_ab_summarizes_b_over_a(self) -> None:
        # --kettle-b-config alone runs one binary twice: still an A/B.
        self.assertTrue(standing.is_ab({"kettle-a": "/k", "kettle-b": "/k"}))
        self.assertFalse(standing.is_ab({"kettle": "/k", "kettle-opaque": "/k"}))
        self.assertFalse(standing.is_ab({"kettle": "/k", "kettle-b": "/k"}), "a variant named b is not an A/B")

    def test_variant_names_cannot_collide_with_the_ab_sides(self) -> None:
        self.assertEqual(standing.variant_name("opaque"), "kettle-opaque")
        for reserved in ("a", "b", "", "A"):
            with self.assertRaises(ValueError):
                standing.variant_name(reserved)

    def test_ab_mode_reports_a_paired_ratio(self) -> None:
        results = {
            "context": "test host",
            "workloads": {
                "startup": {
                    "kettle-a": [{"window_ms": 200.0}, {"window_ms": 210.0}],
                    "kettle-b": [{"window_ms": 100.0}, {"window_ms": 105.0}],
                }
            },
        }
        text = standing.summarize(results, ["kettle-a", "kettle-b"], ab=True)
        self.assertIn("window_ms: B/A 0.500", text)
        self.assertIn("B lower in 2/2", text)

    def test_vtebench_rows_report_round_means_not_medians(self) -> None:
        # Whole-millisecond medians tie at 14/14 here while the means differ
        # by about 7 %: the artifact behind 4.7.0's cursor_motion "tie".
        kettle = standing.vtebench_row(dat({"cursor_motion": [14, 14, 14, 17, 17]}), "ms")
        alacritty = standing.vtebench_row(dat({"cursor_motion": [14, 14, 14, 14, 15]}), "ms")
        results = {"context": "t", "workloads": {"vtebench": {"kettle": [kettle], "alacritty": [alacritty]}}}
        text = standing.summarize(results, ["kettle", "alacritty"], ab=False)
        self.assertIn("| cursor_motion | 15.2 | 14.2 |", text)
        self.assertNotIn("| cursor_motion | 14.0 | 14.0 |", text)
        self.assertIn("| cursor_motion | alacritty | 1.070 |", text)

    def test_vtebench_ab_reports_paired_geomean_ratio(self) -> None:
        a = [standing.vtebench_row(dat({"x": [10, 10], "y": [40, 40]}), "ms"),
             standing.vtebench_row(dat({"x": [10, 10], "y": [40, 40]}), "ms")]
        b = [standing.vtebench_row(dat({"x": [5, 5], "y": [20, 20]}), "ms"),
             standing.vtebench_row(dat({"x": [5, 5], "y": [20, 20]}), "ms")]
        results = {"context": "t", "workloads": {"vtebench": {"kettle-a": a, "kettle-b": b}}}
        text = standing.summarize(results, ["kettle-a", "kettle-b"], ab=True)
        self.assertIn("geometric mean: B/A 0.500", text)
        self.assertIn("x: B/A 0.500", text)

    def test_idle_summary_uses_frontmost_rounds_only(self) -> None:
        # A round that ended behind another app idles differently (no blink),
        # so it must not enter the median. The count says how many counted.
        results = {"context": "t", "workloads": {"idle": {
            "kettle": [{"cpu_percent": 0.03, "frontmost": True}, {"cpu_percent": 0.001, "frontmost": False},
                       {"cpu_percent": 0.05, "frontmost": True}],
            "wezterm": [{"cpu_percent": 0.01, "frontmost": False}] * 3,
        }}}
        text = standing.summarize(results, ["kettle", "wezterm"], ab=False)
        self.assertIn("| cpu_percent | 0.04 | - |", text)
        self.assertIn("| frontmost rounds | 2/3 | 0/3 |", text)


class Grid(unittest.TestCase):
    def test_a_round_at_another_grid_fails_and_records_it(self) -> None:
        ok = standing.check_grid({"window_ms": 1.0}, (standing.COLS, standing.ROWS))
        self.assertEqual((ok["cols"], ok["rows"], ok["window_ms"]), (standing.COLS, standing.ROWS, 1.0))
        small = standing.check_grid({"window_ms": 1.0}, (99, 35))
        self.assertEqual(small, {"error": f"grid 99x35, not {standing.COLS}x{standing.ROWS}",
                                 "cols": 99, "rows": 35})
        # No stamp: the payload never ran.
        self.assertEqual(standing.check_grid({"window_ms": 1.0}, None), {"window_ms": 1.0})
        # A round that failed for another reason keeps its error and still
        # records its grid.
        self.assertEqual(standing.check_grid({"error": "x"}, (99, 35)), {"error": "x", "cols": 99, "rows": 35})

    def test_the_stamp_records_the_grid(self) -> None:
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            stamp = Path(tmp) / "stamp"
            self.assertIsNone(standing.stamp_grid(stamp))
            stamp.write_text("123456789 120 36\n")
            self.assertEqual(standing.stamp_grid(stamp), (120, 36))
            stamp.write_text("123456789\n")
            self.assertIsNone(standing.stamp_grid(stamp))

    def test_a_first_launch_at_another_grid_refuses_the_session(self) -> None:
        launched: set = set()
        self.assertIsNone(standing.first_launch_refusal("kettle", {"cols": 120, "rows": 36}, launched))
        self.assertEqual(standing.first_launch_refusal("ghostty", {"error": "grid 99x35, not 120x36",
                                                                   "cols": 99, "rows": 35}, launched),
                         "ghostty opened at 99x35, not 120x36")
        # Only the first launch decides; later rounds just fail on their own.
        self.assertIsNone(standing.first_launch_refusal("kettle", {"cols": 99, "rows": 35}, launched))
        # A launch that never ran its payload decides nothing yet.
        self.assertIsNone(standing.first_launch_refusal("kitty", {"error": "no window"}, launched))
        self.assertNotIn("kitty", launched)
        # A first launch that failed for another reason at the wrong grid
        # still refuses the session.
        failed = standing.check_grid({"error": "terminal exited before sampling"}, (99, 35))
        self.assertEqual(standing.first_launch_refusal("wezterm", failed, launched),
                         "wezterm opened at 99x35, not 120x36")

    def test_the_settled_grid_decides_and_a_moved_start_is_kept(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            self.assertEqual(standing.round_grid(work), (None, None))
            (work / "stamp").write_text("123 99 35\n")
            # The payload was stopped before it settled: the start stands in.
            self.assertEqual(standing.round_grid(work), ((99, 35), (99, 35)))
            (work / "grid").write_text("120 36 40\n")
            grid, start = standing.round_grid(work)
            self.assertEqual((grid, start), ((120, 36), (99, 35)))
            # Ghostty sometimes starts its child before its first resize: the
            # round counts, and the start grid is kept for the record.
            row = standing.check_grid({"window_ms": 1.0}, grid, start)
            self.assertEqual(row, {"window_ms": 1.0, "cols": 120, "rows": 36, "start_cols": 99, "start_rows": 35})
            self.assertIsNone(standing.first_launch_refusal("ghostty", row, set()))
            # A terminal that never reached the grid still fails.
            (work / "grid").write_text("99 35 5000\n")
            self.assertIn("error", standing.check_grid({"window_ms": 1.0}, *standing.round_grid(work)))

    def test_every_payload_settles_before_its_workload_but_startup_after_its_hold(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "probe.sh"
            probe.write_text("#!/bin/sh\nexit 0\n")
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": work / "stamp-probe"}, work, {"kettle": "/bin/true"})
            (work / "grid").write_text("99 35 0\n")
            runner.launch("kettle", "exec /bin/sleep 7", 1).wait(10)
            self.assertFalse((work / "grid").exists(), "a previous round's grid is cleared")
            runner.startup("kettle")
            scripts = {path.read_text() for path in work.glob("payload-*.sh")}
            settle = runner.settle_command()
            workload = next(text for text in scripts if "sleep 7" in text)
            self.assertLess(workload.index(settle), workload.index("exec /bin/sleep 7"))
            startup = next(text for text in scripts if "sleep 1" in text)
            self.assertEqual(startup.count(settle), 1)
            self.assertLess(startup.index("/bin/sleep 1"), startup.index(settle))

    def test_only_a_ghostty_launch_clears_its_saved_frame(self) -> None:
        import tempfile

        class Frame:
            cleared: list = []
            tracked: list = []

            def clear(self, seconds=None) -> None:
                Frame.cleared.append(seconds)

            def track(self, probe) -> None:
                Frame.tracked.append(probe.pid)

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "probe.sh"
            probe.write_text("#!/bin/sh\nexit 0\n")
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})
            runner.ghostty_frame = Frame()
            runner.launch("kettle", "", 1, argv=["/bin/true"]).wait(10)
            self.assertEqual((Frame.cleared, Frame.tracked), ([], []))
            probe = runner.launch("ghostty", "", 7, argv=["/bin/true"])
            probe.wait(10)
            # Announced with its timeout before the spawn, then named.
            self.assertEqual((Frame.cleared, Frame.tracked), ([7], [probe.pid]))


class RoundWait(unittest.TestCase):
    """wait_for_round on a fake clock: it never signals, only waits."""

    def wait(self, pgid, answers, until=10.0):
        # Whole milliseconds, so the sleeps add up exactly.
        ms = [0]
        asked = []

        def live(group):
            asked.append(group)
            return answers(ms[0] / 1000)

        over = standing.wait_for_round(pgid, until, clock=lambda: ms[0] / 1000,
                                       sleep=lambda seconds: ms.__setitem__(0, ms[0] + round(seconds * 1000)),
                                       live=live)
        return over, ms[0] / 1000, asked

    def test_it_ends_when_the_probes_group_empties(self) -> None:
        over, ended, asked = self.wait(4242, lambda now: now < 1.0)
        self.assertEqual((over, ended), (True, 1.0))
        self.assertEqual(set(asked), {4242})

    def test_a_group_still_live_or_unknown_at_the_cap_is_not_over(self) -> None:
        self.assertEqual(self.wait(4242, lambda now: True)[:2], (False, 10.0))
        self.assertEqual(self.wait(4242, lambda now: None)[:2], (False, 10.0))

    def test_a_launch_with_no_group_is_waited_out_for_its_grace(self) -> None:
        self.assertEqual(self.wait(None, lambda now: self.fail("no group to ask about")), (True, 10.0, []))

    def test_the_grace_runs_from_the_tracked_spawn_or_from_the_harness_going(self) -> None:
        grace, cap = standing.PROBE_STOP_GRACE, standing.ROUND_CAP
        # A clear that took long before the spawn cannot eat into the wait:
        # it counts from when the probe was named.
        self.assertEqual(standing.round_until(30, tracked=100.0, ended=500.0), 100.0 + 30 + grace + cap)
        # No group: the harness went around the spawn; its orphaned probe
        # stops the terminal within the grace of that.
        self.assertEqual(standing.round_until(30, tracked=None, ended=500.0), 500.0 + grace)


@unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang"), "needs macOS and clang")
class Settle(unittest.TestCase):
    """stamp --settle in a pty whose size this test sets."""

    @classmethod
    def setUpClass(cls) -> None:
        import tempfile
        cls.tmp = tempfile.TemporaryDirectory()
        cls.stamp = Path(cls.tmp.name) / "stamp"
        subprocess.run(["clang", "-O", "-o", str(cls.stamp), str(HERE / "macos-standing" / "stamp.c")],
                       check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.tmp.cleanup()

    def run_in_pty(self, size, resize_to, after: float, seconds: float):
        import fcntl
        import pty
        import struct
        import termios

        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", size[1], size[0], 0, 0))
        out = Path(self.tmp.name) / f"grid-{time.monotonic_ns()}"
        started = time.monotonic()
        process = subprocess.Popen([str(self.stamp), "--settle", "120", "36", str(seconds), str(out)],
                                   stdin=slave, stdout=subprocess.DEVNULL, cwd=self.tmp.name)
        if resize_to:
            time.sleep(after)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", resize_to[1], resize_to[0], 0, 0))
        os.close(slave)
        self.assertEqual(process.wait(10), 0)
        elapsed = time.monotonic() - started
        self.assertFalse(Path(str(out) + ".tmp").exists())
        cols, rows, waited = map(int, out.read_text().split())
        return (cols, rows), waited, elapsed

    def test_it_waits_for_the_first_resize(self) -> None:
        grid, waited, elapsed = self.run_in_pty((99, 35), (120, 36), 0.3, 5)
        self.assertEqual(grid, (120, 36))
        self.assertGreaterEqual(waited, 250)
        self.assertLess(elapsed, 3, "it ends at the resize, not at its limit")

    def test_it_ends_at_once_on_the_right_grid(self) -> None:
        grid, waited, _ = self.run_in_pty((120, 36), None, 0, 5)
        self.assertEqual(grid, (120, 36))
        self.assertLess(waited, 100)

    def test_a_terminal_that_never_resizes_records_its_own_grid(self) -> None:
        grid, waited, _ = self.run_in_pty((99, 35), None, 0, 0.5)
        self.assertEqual(grid, (99, 35))
        self.assertGreaterEqual(waited, 450)


@unittest.skipUnless(not SNAPSHOT_LAYOUT and sys.platform == "darwin" and shutil.which("defaults"),
                     "needs macOS user defaults; skipped in per-file snapshots")
class GhosttyFrame(unittest.TestCase):
    """GhosttyFrame against a scratch plist file, never Ghostty's domain; a
    file path keeps even an emptied domain out of ~/Library/Preferences."""

    KEY = standing.GHOSTTY_FRAME_KEY

    def setUp(self) -> None:
        import tempfile
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.domain = str(self.dir / "frame.plist")

    def frame(self):
        return standing.GhosttyFrame(self.domain, self.KEY)

    def value(self):
        return standing.read_default(self.domain, self.KEY)

    def wait_for_value(self, expected, timeout: float = 15.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and self.value() != expected:
            time.sleep(0.1)
        return self.value()

    def test_the_saved_frame_comes_back_with_its_types(self) -> None:
        saved = [0.0, 0.0, 961.0, 1050.0]
        standing.write_default(self.domain, self.KEY, saved)
        with self.frame() as frame:
            self.assertIsNone(self.value())
            # A measured Ghostty closing writes its own frame.
            standing.write_default(self.domain, self.KEY, [10.0, 20.0, 1000.0, 700.0])
            frame.clear()
            self.assertIsNone(self.value())
        restored = self.value()
        self.assertEqual(restored, saved)
        self.assertTrue(all(isinstance(value, float) for value in restored))
        self.assertEqual(frame.keeper.returncode, 0)

    def test_an_absent_frame_stays_absent(self) -> None:
        with self.frame():
            standing.write_default(self.domain, self.KEY, [1.0, 2.0, 3.0, 4.0])
        self.assertIsNone(self.value())

    def test_an_exception_in_the_session_restores_it(self) -> None:
        saved = [5.0, 6.0, 7.0, 8.0]
        standing.write_default(self.domain, self.KEY, saved)
        with self.assertRaises(RuntimeError):
            with self.frame():
                raise RuntimeError("a round crashed")
        self.assertEqual(self.value(), saved)

    def test_a_failed_export_changes_nothing(self) -> None:
        from unittest import mock

        saved = [1.0, 2.0, 3.0, 4.0]
        standing.write_default(self.domain, self.KEY, saved)
        failed = subprocess.CompletedProcess(["defaults"], 1, b"", b"")
        real_run = subprocess.run

        def export_fails(argv, *args, **kwargs):
            return failed if argv[1] == "export" else real_run(argv, *args, **kwargs)

        with mock.patch.object(standing.subprocess, "run", export_fails):
            with self.assertRaises(RuntimeError):
                with self.frame():
                    self.fail("the session must not start")
        self.assertEqual(self.value(), saved)

    def run_that_ends(self, ending: str) -> None:
        """A harness process that enters the frame, then ends as `ending`
        says; the keeper must put the saved value back."""
        saved = [9.0, 8.0, 7.0, 6.0]
        standing.write_default(self.domain, self.KEY, saved)
        script = ("import importlib.util, os, sys, time\n"
                  f"spec = importlib.util.spec_from_file_location('st', {str(HERE / 'macos-standing.py')!r})\n"
                  "st = importlib.util.module_from_spec(spec); spec.loader.exec_module(st)\n"
                  f"with st.GhosttyFrame({self.domain!r}, {self.KEY!r}) as frame:\n"
                  "    print('entered', frame.keeper.pid, flush=True)\n"
                  "    if sys.argv[1] == 'clear-in-flight':\n"
                  "        frame.keeper.stdin.write(b'clear 99\\n'); frame.keeper.stdin.flush(); os._exit(0)\n"
                  "    time.sleep(60)\n")
        run = subprocess.Popen([sys.executable, "-c", script, ending], stdout=subprocess.PIPE, text=True)
        self.addCleanup(run.stdout.close)
        entered, keeper_pid = run.stdout.readline().split()
        self.assertEqual(entered, "entered")
        keeper_pid = int(keeper_pid)
        if ending == "SIGKILL":
            run.send_signal(signal.SIGKILL)
        elif ending == "SIGTERM":
            run.send_signal(signal.SIGTERM)
        elif ending == "by-name":
            # A kill by name aimed at the harness also matches the keeper,
            # whose argv names the same script.
            run.send_signal(signal.SIGTERM)
            os.kill(keeper_pid, signal.SIGTERM)
        run.wait(10)
        self.assertEqual(self.wait_for_value(saved), saved)
        # The keeper (this test's grandchild) must be gone before the scratch
        # directory is, or its read-back could re-create it.
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            try:
                os.kill(keeper_pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.05)
        else:
            self.fail("the keeper outlived its run")

    def test_a_killed_run_is_restored_by_the_keeper(self) -> None:
        self.run_that_ends("SIGKILL")

    def test_a_terminated_run_is_restored_by_the_keeper(self) -> None:
        self.run_that_ends("SIGTERM")

    def test_a_clear_in_flight_when_the_run_dies_still_ends_restored(self) -> None:
        self.run_that_ends("clear-in-flight")

    def test_a_kill_by_name_that_also_hits_the_keeper_still_ends_restored(self) -> None:
        self.run_that_ends("by-name")

    def closing_ghostty(self, frame_value, after: float) -> str:
        """A stand-in for a measured Ghostty under its launch probe: it
        writes its own frame `after` seconds on, as Ghostty does when it
        closes, then exits."""
        return (f"import subprocess, time\ntime.sleep({after})\n"
                f"subprocess.run(['defaults', 'write', {self.domain!r}, {self.KEY!r}, "
                f"{standing.plist_fragment(frame_value)!r}], capture_output=True)\n")

    def run_that_ends_mid_round(self, ending: signal.Signals) -> None:
        """A harness process killed while a measured Ghostty still runs: the
        keeper must put the value back only once that Ghostty has written
        its own frame and gone."""
        saved = [9.0, 1.0, 9.0, 1.0]
        standing.write_default(self.domain, self.KEY, saved)
        ghostty = self.closing_ghostty([11.0, 22.0, 1000.0, 700.0], 2.0)
        script = ("import importlib.util, subprocess, sys, time\n"
                  f"spec = importlib.util.spec_from_file_location('st', {str(HERE / 'macos-standing.py')!r})\n"
                  "st = importlib.util.module_from_spec(spec); spec.loader.exec_module(st)\n"
                  f"with st.GhosttyFrame({self.domain!r}, {self.KEY!r}) as frame:\n"
                  "    frame.clear(30)\n"
                  f"    probe = subprocess.Popen([sys.executable, '-c', {ghostty!r}], start_new_session=True)\n"
                  "    frame.track(probe)\n"
                  "    print('entered', frame.keeper.pid, probe.pid, flush=True)\n"
                  "    time.sleep(60)\n")
        run = subprocess.Popen([sys.executable, "-c", script], stdout=subprocess.PIPE, text=True)
        self.addCleanup(run.stdout.close)
        entered, keeper_pid, probe_pid = run.stdout.readline().split()
        self.assertEqual(entered, "entered")
        run.send_signal(ending)
        run.wait(10)
        for pid in (int(probe_pid), int(keeper_pid)):
            self.assertTrue(gone_within(pid, 20), "the stand-in Ghostty and the keeper end")
        self.assertEqual(self.value(), saved)

    def test_a_killed_run_mid_round_is_restored_after_its_ghostty(self) -> None:
        self.run_that_ends_mid_round(signal.SIGKILL)

    def test_a_terminated_run_mid_round_is_restored_after_its_ghostty(self) -> None:
        self.run_that_ends_mid_round(signal.SIGTERM)

    def test_a_hung_up_run_mid_round_is_restored_after_its_ghostty(self) -> None:
        self.run_that_ends_mid_round(signal.SIGHUP)

    def test_an_interrupt_while_stopping_the_round_still_waits_for_its_ghostty(self) -> None:
        # A second Ctrl-C inside stop_current: the round's probe has been
        # told to stop, and its Ghostty writes its frame while closing,
        # after the session has already left for the restore.
        from unittest import mock

        saved = [3.0, 3.0, 8.0, 8.0]
        standing.write_default(self.domain, self.KEY, saved)
        closing = self.dir / "closing.sh"
        closing.write_text(f"sleep 1.5\ndefaults write {shlex.quote(self.domain)} {self.KEY} "
                           f"{shlex.quote(standing.plist_fragment([1.0, 2.0, 1000.0, 700.0]))}\n")
        probe = self.dir / "probe.sh"
        probe.write_text("#!/bin/sh\n"
                         f"trap '/bin/sh {shlex.quote(str(closing))}; exit 0' TERM\n"
                         ': > "$1.ready"\nwhile :; do sleep 0.05; done\n')
        probe.chmod(0o755)
        recovery = self.dir / "restore.txt"
        runner = standing.Runner({"launch": probe, "stamp": probe}, self.dir, {"kettle": "/bin/true"})
        with self.assertRaises(KeyboardInterrupt):
            with standing.GhosttyFrame(self.domain, self.KEY, recovery=recovery) as frame:
                runner.ghostty_frame = frame
                process = runner.launch("ghostty", "", 30, argv=["/bin/true"])
                ready = self.dir / "launch.json.ready"
                deadline = time.monotonic() + 10
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "the stand-in probe never set its trap")
                with mock.patch.object(process, "wait", side_effect=KeyboardInterrupt):
                    runner.stop_current()
        process.wait(10)
        self.assertEqual(self.value(), saved)
        self.assertFalse(recovery.exists())

    def test_a_lost_keeper_with_a_round_in_flight_restores_after_its_ghostty(self) -> None:
        # The harness restores by itself, and so must wait out the round too.
        saved = [2.0, 4.0, 6.0, 8.0]
        standing.write_default(self.domain, self.KEY, saved)
        ghostty = subprocess.Popen([sys.executable, "-c", self.closing_ghostty([5.0, 5.0, 900.0, 600.0], 1.5)],
                                   start_new_session=True)
        with self.frame() as frame:
            frame.clear(30)
            frame.track(ghostty)
            os.kill(frame.keeper.pid, signal.SIGKILL)
            frame.keeper.stdout.read()
        ghostty.wait(10)
        self.assertEqual(self.value(), saved)

    def test_a_late_restore_keeps_the_recovery_file_and_says_so(self) -> None:
        # The keeper put the value back while the round's group was still
        # live past its cap: that Ghostty may still write over it.
        import contextlib
        import io

        saved = [1.0, 3.0, 5.0, 7.0]
        standing.write_default(self.domain, self.KEY, saved)
        stand_in = ("import sys\n"
                    "for line in sys.stdin:\n"
                    "    verb, tag = line.split()[:2]\n"
                    "    print(('late' if verb == 'done' else 'ok'), tag, flush=True)\n"
                    "    if verb == 'done': break\n")
        keeper = subprocess.Popen([sys.executable, "-c", stand_in], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, start_new_session=True)
        recovery = self.dir / "restore.txt"
        recovery.write_text("defaults write ...\n")
        frame = standing.GhosttyFrame(self.domain, self.KEY, recovery=recovery)
        frame.saved, frame.active, frame.keeper = saved, True, keeper
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            frame._restore()
        self.assertTrue(recovery.exists())
        self.assertIn("still running", err.getvalue())
        self.assertEqual(keeper.returncode, 0, "no fallback kill for a restore the keeper made")

    def test_a_keeper_that_ends_mid_clear_leaves_the_clear_to_the_harness(self) -> None:
        saved = [8.0, 6.0, 4.0, 2.0]
        standing.write_default(self.domain, self.KEY, saved)
        # Reads the request, then ends before clearing.
        keeper = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.readline()"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, start_new_session=True)
        frame = self.frame()
        frame.saved, frame.active, frame.keeper = saved, True, keeper
        frame.clear(1)
        self.assertIsNone(self.value())
        frame._restore()
        self.assertEqual(self.value(), saved)

    def test_the_recovery_file_says_how_to_restore_until_the_value_is_back(self) -> None:
        saved = [1.0, 1.0, 2.0, 3.0]
        standing.write_default(self.domain, self.KEY, saved)
        recovery = self.dir / "restore.txt"
        with standing.GhosttyFrame(self.domain, self.KEY, recovery=recovery):
            self.assertIn(f"defaults write {self.domain} {self.KEY} '<array>", recovery.read_text())
        self.assertFalse(recovery.exists())
        self.assertEqual(self.value(), saved)

    def test_a_late_answer_is_not_taken_for_the_next_request(self) -> None:
        # A stand-in keeper speaking the same protocol answers the first
        # clear late; the harness must not take that answer for the next
        # request, and must still end through a clean "restored".
        saved = [4.0, 4.0, 2.0, 2.0]
        standing.write_default(self.domain, self.KEY, saved)
        fragment = standing.plist_fragment(saved)
        stand_in = ("import subprocess, sys, time\n"
                    "for line in sys.stdin:\n"
                    "    verb, tag = line.split()\n"
                    "    if verb == 'clear':\n"
                    "        time.sleep(1.0 if tag == '1' else 0)\n"
                    f"        subprocess.run(['defaults', 'delete', {self.domain!r}, {self.KEY!r}], capture_output=True)\n"
                    "        print('ok', tag, flush=True)\n"
                    "        continue\n"
                    f"    subprocess.run(['defaults', 'write', {self.domain!r}, {self.KEY!r}, {fragment!r}])\n"
                    "    print('restored', tag, flush=True)\n"
                    "    break\n")
        keeper = subprocess.Popen([sys.executable, "-c", stand_in], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, start_new_session=True)
        frame = self.frame()
        frame.WAIT = 0.3
        frame.saved, frame.active, frame.keeper = saved, True, keeper
        started = time.monotonic()
        frame.clear()  # "clear 1": the stand-in answers after the wait
        self.assertLess(time.monotonic() - started, 0.9)
        frame.WAIT = 5.0
        self.assertEqual(frame.request("clear", frame.WAIT), "ok")  # "clear 2", past the late "ok 1"
        frame._restore()
        self.assertEqual(keeper.returncode, 0, "ended through its own restore, not the kill path")
        self.assertEqual(self.value(), saved)

    def test_a_failing_direct_clear_does_not_end_the_session(self) -> None:
        from unittest import mock

        saved = [5.0, 5.0, 5.0, 5.0]
        standing.write_default(self.domain, self.KEY, saved)
        with self.frame() as frame:
            os.kill(frame.keeper.pid, signal.SIGKILL)
            frame.keeper.stdout.read()
            with mock.patch.object(standing, "set_default", side_effect=RuntimeError("defaults failed")):
                frame.clear()
        self.assertEqual(self.value(), saved)

    def test_a_second_interrupt_while_restoring_says_how_to_finish(self) -> None:
        import contextlib
        import io
        from unittest import mock

        saved = [7.0, 7.0, 7.0, 7.0]
        standing.write_default(self.domain, self.KEY, saved)
        err = io.StringIO()
        frame = self.frame()
        with contextlib.redirect_stderr(err), self.assertRaises(KeyboardInterrupt):
            with frame:
                os.kill(frame.keeper.pid, signal.SIGKILL)
                with mock.patch.object(standing, "group_has_live_members", side_effect=KeyboardInterrupt):
                    frame._restore()
        frame.keeper.wait(10)
        self.assertIn(f"restore it with: defaults write {self.domain} {self.KEY}", err.getvalue())

    def test_a_lost_keeper_leaves_the_restore_to_the_harness(self) -> None:
        saved = [3.0, 1.0, 4.0, 1.0]
        standing.write_default(self.domain, self.KEY, saved)
        with self.frame() as frame:
            # Killed, and left unreaped as a real lost keeper would be.
            os.kill(frame.keeper.pid, signal.SIGKILL)
            frame.keeper.stdout.read()
            frame.clear()
        self.assertEqual(self.value(), saved)

    def test_a_lost_keepers_leftover_writer_cannot_undo_the_restore(self) -> None:
        # The keeper died while a `defaults delete` it started was still
        # pending; that command must not land after the harness restores.
        # The writer waits for a file the test creates only once the restore
        # has returned, so it is pending then whatever the timing.
        saved = [2.0, 7.0, 1.0, 8.0]
        standing.write_default(self.domain, self.KEY, saved)
        go = self.dir / "go"
        late_delete = (f"while [ ! -e {shlex.quote(str(go))} ]; do sleep 0.02; done; "
                       f"defaults delete {shlex.quote(self.domain)} {self.KEY}")
        # Like a `defaults` run under capture_output, the leftover writer does
        # not hold the keeper's stdout, so the harness sees the keeper end at
        # once.
        stand_in = ("import subprocess, time\n"
                    f"subprocess.Popen(['/bin/sh', '-c', {late_delete!r}], stdout=subprocess.DEVNULL,"
                    " stderr=subprocess.DEVNULL)\n"
                    "print('started', flush=True)\n"
                    "time.sleep(60)\n")
        keeper = subprocess.Popen([sys.executable, "-c", stand_in], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, start_new_session=True)
        self.assertEqual(keeper.stdout.readline().strip(), b"started")
        standing.write_default(self.domain, self.KEY, None)
        os.kill(keeper.pid, signal.SIGKILL)
        frame = self.frame()
        frame.saved, frame.active, frame.keeper = saved, True, keeper
        frame._restore()
        go.touch()
        time.sleep(1.0)
        self.assertEqual(self.value(), saved)
        self.assertFalse(standing.group_has_live_members(keeper.pid))

    def test_an_unreadable_process_list_blocks_the_direct_restore(self) -> None:
        # If ps cannot say whether the lost keeper's group has emptied, a
        # writer may still be pending: print the command, write nothing.
        import contextlib
        import io
        from unittest import mock

        saved = [6.0, 2.0, 8.0, 3.0]
        standing.write_default(self.domain, self.KEY, saved)
        real_run = subprocess.run
        for failure in ("status", "raises"):
            standing.write_default(self.domain, self.KEY, None)
            keeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, start_new_session=True)
            os.kill(keeper.pid, signal.SIGKILL)

            def ps_fails(argv, *args, **kwargs):
                if argv[0] != "ps":
                    return real_run(argv, *args, **kwargs)
                if failure == "raises":
                    raise PermissionError("ps refused")
                return subprocess.CompletedProcess(argv, 1, "", "")

            frame = self.frame()
            frame.saved, frame.active, frame.keeper = saved, True, keeper
            err = io.StringIO()
            with mock.patch.object(standing.subprocess, "run", ps_fails), contextlib.redirect_stderr(err):
                frame._restore()
            self.assertIsNone(self.value(), failure)
            self.assertIn("restore it with: defaults write", err.getvalue(), failure)
            keeper.wait(10)

    def test_a_restore_that_cannot_write_says_how_to_finish_it(self) -> None:
        import contextlib
        import io

        saved = [1.5, 2.5, 3.5, 4.5]
        standing.write_default(self.domain, self.KEY, saved)
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            with self.frame():
                self.dir.chmod(0o500)
                self.addCleanup(self.dir.chmod, 0o700)
        self.assertIn(f"restore it with: defaults write {self.domain} {self.KEY} '<array>", err.getvalue())
        self.dir.chmod(0o700)
        # A delete that did not take is not reported as done.
        from unittest import mock
        real_run = subprocess.run
        standing.write_default(self.domain, self.KEY, saved)
        with mock.patch.object(standing.subprocess, "run",
                               lambda argv, *a, **k: subprocess.CompletedProcess(argv, 0, b"", b"")
                               if argv[1] == "delete" else real_run(argv, *a, **k)):
            self.assertFalse(standing.write_default(self.domain, self.KEY, None))


class RoundStatistics(unittest.TestCase):
    def test_paired_is_the_geometric_mean_ratio_of_paired_rounds(self) -> None:
        base = [10.0, 10.0, 10.0, 10.0]
        test = [9.0, 9.0, 11.0, 9.0]
        stats = standing.paired(base, test)
        self.assertAlmostEqual(stats["ratio"], math.exp((3 * math.log(0.9) + math.log(1.1)) / 4))
        self.assertEqual((stats["wins"], stats["n"]), (3, 4))
        self.assertLessEqual(stats["low"], stats["ratio"])
        self.assertLessEqual(stats["ratio"], stats["high"])
        # Rounds pair by index: a missing round drops its pair, not a shift.
        shifted = standing.paired([10.0, None, 10.0], [9.0, 1.0, 11.0])
        self.assertEqual(shifted["n"], 2)
        self.assertAlmostEqual(shifted["ratio"], math.sqrt(0.99))

    def test_zero_values_stay_in_the_pairs(self) -> None:
        # A terminal with no wakeups in a round won that round; dropping the
        # pair would change n and the 80 % threshold.
        stats = standing.paired([1.0, 0.0, 1.0], [0.0, 0.0, 2.0])
        self.assertEqual((stats["n"], stats["wins"]), (3, 1))
        self.assertEqual(stats["ratio"], 1.0)
        behind = standing.paired([0.0], [0.5])
        self.assertEqual(behind["high"], math.inf)
        self.assertEqual(behind["wins"], 0)

    def test_t_quantiles_match_published_tables(self) -> None:
        for p, df, value in [(0.975, 1, 12.706205), (0.975, 3, 3.182446), (0.975, 9, 2.262157),
                             (0.975, 29, 2.045230), (0.995, 9, 3.249836), (0.999, 9, 4.296806),
                             (0.975, 1000, 1.962339)]:
            self.assertAlmostEqual(standing.t_quantile(p, df), value, places=5, msg=(p, df))
        self.assertEqual(standing.t_interval([3.0]), (3.0, -math.inf, math.inf))

    def test_median_interval_uses_sign_test_order_statistics(self) -> None:
        # n=10: [x(2), x(9)] covers 97.9 %; n=30: [x(10), x(21)]; n=6: the
        # extremes cover 96.9 %. Below 6 rounds no pair of order statistics
        # reaches 95 % (the extremes of 5 cover 93.75 %), so nothing is claimed.
        self.assertEqual(standing.median_interval(list(range(10))), (4.5, 1, 8))
        self.assertEqual(standing.median_interval(list(range(30))), (14.5, 9, 20))
        self.assertEqual(standing.median_interval(list(range(6))), (2.5, 0, 5))
        self.assertEqual(standing.median_interval(list(range(5))), (2, -math.inf, math.inf))
        self.assertEqual(standing.median_interval([4.0]), (4.0, -math.inf, math.inf))

    def test_a_wide_interval_is_unbounded_not_an_overflow(self) -> None:
        # exp of a log bound past about 709.8 overflows a float.
        stats = standing.paired([1.0, 1.0], [1.0, 20000.0], family=12)
        self.assertEqual(stats["family_high"], math.inf)
        self.assertLess(stats["family_low"], 1.0)

    def test_vtebench_intervals_keep_their_coverage_over_ten_rounds(self) -> None:
        # Same-binary sessions: every benchmark's B/A interval should contain
        # 1 about 95 % of the time. A percentile bootstrap over 10 rounds
        # covered about 0.89.
        rng = random.Random(11)
        covered = sessions = 400
        for _ in range(sessions):
            rows = {name: [{"means_ms": {"unicode": 8.0 * math.exp(rng.gauss(0, 0.04))}} for _ in range(10)]
                    for name in ("kettle-a", "kettle-b")}
            stats = standing.analyze({"workloads": {"vtebench": rows}}, ["kettle-a", "kettle-b"], True)
            ab = stats["vtebench"]["metrics"]["unicode"]["ab"]
            covered -= not ab["low"] <= 1 <= ab["high"]
        self.assertGreaterEqual(covered / sessions, 0.92)

    def test_an_aa_of_twelve_benchmarks_is_judged_family_wise(self) -> None:
        # Twelve benchmarks with no real difference: judging each at 95 %
        # failed most same-binary A/As; the Bonferroni interval should pass
        # them about 95 % of the time.
        rng = random.Random(12)
        benches = [f"bench{i:02d}" for i in range(12)]
        passed = sessions = 150
        for _ in range(sessions):
            rows = {name: [{"means_ms": {bench: 10.0 * math.exp(rng.gauss(0, 0.04)) for bench in benches}}
                           for _ in range(10)] for name in ("kettle-a", "kettle-b")}
            metrics = standing.analyze({"workloads": {"vtebench": rows}}, ["kettle-a", "kettle-b"], True)
            gates = [standing.aa_gate(metrics["vtebench"]["metrics"][bench]["ab"]) for bench in benches]
            passed -= not all(gate["contains_one"] for gate in gates)
        self.assertGreaterEqual(passed / sessions, 0.85)
        # The gate still comes from the 95 % half-width.
        entry = metrics["vtebench"]["metrics"]["bench00"]["ab"]
        self.assertEqual(entry["family"], 12)
        self.assertLess(entry["family_low"], entry["low"])
        self.assertAlmostEqual(standing.aa_gate(entry)["half_width"], (entry["high"] - entry["low"]) / 2)
        self.assertNotIn("family", metrics["vtebench"]["metrics"]["geometric mean"]["ab"])

    def test_median_ci_brackets_the_median(self) -> None:
        stats = standing.median_ci([5.0, 1.0, 3.0, 2.0, 4.0])
        self.assertEqual((stats["median"], stats["n"]), (3.0, 5))
        self.assertLessEqual(stats["low"], 3.0)
        self.assertGreaterEqual(stats["high"], 3.0)

    def test_percentile_of_infinite_values_is_not_nan(self) -> None:
        self.assertEqual(standing.percentile([1.0, math.inf, math.inf], 0.9), math.inf)
        self.assertEqual(standing.percentile([2.0, 2.0], 0.5), 2.0)

    def test_published_json_has_no_infinity_tokens(self) -> None:
        import json

        text = standing.dumps({"high": math.inf, "low": -math.inf, "x": [math.nan, 1.0]})
        self.assertNotIn("Infinity", text)
        self.assertNotIn("NaN", text)
        self.assertEqual(json.loads(text), {"high": "inf", "low": "-inf", "x": ["nan", 1.0]})

    def test_distribution_stats(self) -> None:
        stats = standing.distribution([10.0, 20.0, 30.0, 40.0, 1000.0], censored=1)
        self.assertEqual((stats["mean"], stats["median"], stats["n"], stats["censored"]), (220.0, 30.0, 5, 1))
        self.assertAlmostEqual(stats["p95"], 808.0)
        self.assertAlmostEqual(stats["p99"], 961.6)


def session(date, ratio, low, high, wins=5, n=5, rank=1, countable=True):
    return {"date": date, "countable": countable, "ratio": ratio, "low": low, "high": high,
            "wins": wins, "n": n, "rank": rank}


class ClaimRule(unittest.TestCase):
    def test_three_clear_sessions_on_three_dates_are_first(self) -> None:
        sessions = [session(f"2026-10-0{day}", 0.9, 0.85, 0.95) for day in (1, 2, 3)]
        self.assertEqual(standing.claim(sessions)["label"], "1st")

    def test_first_needs_most_rounds_won_in_every_session(self) -> None:
        sessions = [session(f"2026-10-0{day}", 0.9, 0.85, 0.95) for day in (1, 2, 3)]
        sessions[1]["wins"] = 3
        self.assertEqual(standing.claim(sessions)["label"], "tied 1st")

    def test_one_straddling_session_is_a_tie(self) -> None:
        sessions = [session("2026-10-01", 0.9, 0.85, 0.95), session("2026-10-02", 0.98, 0.95, 1.02, wins=3),
                    session("2026-10-03", 0.9, 0.85, 0.95)]
        self.assertEqual(standing.claim(sessions)["label"], "tied 1st")

    def test_a_session_clearly_behind_gives_a_numeric_rank(self) -> None:
        sessions = [session("2026-10-01", 0.9, 0.85, 0.95), session("2026-10-02", 1.1, 1.05, 1.15, wins=0, rank=2),
                    session("2026-10-03", 0.9, 0.85, 0.95)]
        self.assertEqual(standing.claim(sessions)["label"], "2nd (varies)")
        behind = [session(f"2026-10-0{day}", 1.2, 1.1, 1.3, wins=0, rank=3) for day in (1, 2, 3)]
        self.assertEqual(standing.claim(behind)["label"], "3rd")

    def test_sessions_on_the_same_date_are_insufficient(self) -> None:
        sessions = [session("2026-10-01", 0.9, 0.85, 0.95), session("2026-10-01", 0.9, 0.85, 0.95),
                    session("2026-10-02", 0.9, 0.85, 0.95)]
        self.assertEqual(standing.claim(sessions)["label"], "insufficient sessions")

    def test_a_non_countable_session_is_ignored(self) -> None:
        sessions = [session(f"2026-10-0{day}", 0.9, 0.85, 0.95) for day in (1, 2, 3)]
        sessions.append(session("2026-10-04", 1.3, 1.2, 1.4, wins=0, rank=4, countable=False))
        self.assertEqual(standing.claim(sessions)["label"], "1st")

    def test_ab_verdict_needs_both_sessions_on_one_side_and_the_gate(self) -> None:
        lower = [{"date": "2026-10-01", "countable": True, "ratio": 0.90, "low": 0.88, "high": 0.92},
                 {"date": "2026-10-02", "countable": True, "ratio": 0.93, "low": 0.90, "high": 0.96}]
        self.assertEqual(standing.ab_verdict(lower)["verdict"], "lower")
        self.assertAlmostEqual(standing.ab_verdict(lower)["headline"], 0.93)
        self.assertEqual(standing.ab_verdict(lower, gate=0.10)["verdict"], "no change")
        mixed = [lower[0], {"date": "2026-10-02", "countable": True, "ratio": 0.99, "low": 0.97, "high": 1.01}]
        self.assertEqual(standing.ab_verdict(mixed)["verdict"], "no change")
        same_day = [lower[0], dict(lower[1], date="2026-10-01")]
        self.assertEqual(standing.ab_verdict(same_day)["verdict"], "insufficient sessions")

    def test_ab_verdict_reads_the_first_two_dated_sessions_only(self) -> None:
        # A later session can neither rescue nor sink the first two: no reruns
        # to improve a result.
        first_two = [{"date": "2026-10-01", "countable": True, "ratio": 0.90, "low": 0.88, "high": 0.92},
                     {"date": "2026-10-02", "countable": True, "ratio": 0.93, "low": 0.90, "high": 0.96}]
        later = {"date": "2026-10-03", "countable": True, "ratio": 1.0, "low": 0.97, "high": 1.03}
        verdict = standing.ab_verdict(first_two + [later])
        self.assertEqual((verdict["verdict"], verdict["headline"]), ("lower", 0.93))
        inconclusive_first = [dict(later, date="2026-10-01"), dict(first_two[0], date="2026-10-02"),
                              dict(first_two[1], date="2026-10-03")]
        self.assertEqual(standing.ab_verdict(inconclusive_first)["verdict"], "no change")
        noisy = dict(first_two[0], countable=False)
        self.assertEqual(standing.ab_verdict([noisy] + first_two[1:] + [dict(later, ratio=0.9, low=0.85,
                                                                          high=0.95)])["verdict"], "lower")

    def test_first_sessions_are_ordered_by_instant_not_by_text(self) -> None:
        # On the night clocks go back, 01:10-08:00 comes after 01:50-07:00.
        later = dict(session("2026-11-01", 1.2, 1.1, 1.3, wins=0, rank=2), started="2026-11-01T01:10:00-08:00")
        earlier = dict(session("2026-11-01", 0.9, 0.85, 0.95), started="2026-11-01T01:50:00-07:00")
        rest = [dict(session(f"2026-11-0{day}", 0.9, 0.85, 0.95), started=f"2026-11-0{day}T01:00:00-08:00")
                for day in (2, 3)]
        self.assertEqual(standing.claim([later, earlier] + rest)["label"], "1st")

    def test_claim_reads_the_first_session_of_each_of_the_first_three_dates(self) -> None:
        clear = [session(f"2026-10-0{day}", 0.9, 0.85, 0.95) for day in (1, 2, 3)]
        second_same_day = session("2026-10-01", 1.2, 1.1, 1.3, wins=0, rank=3)
        self.assertEqual(standing.claim([clear[0], second_same_day] + clear[1:])["label"], "1st")
        fourth_date = session("2026-10-04", 1.2, 1.1, 1.3, wins=0, rank=3)
        self.assertEqual(standing.claim(clear + [fourth_date])["label"], "1st")

    def test_aa_gate_is_the_larger_of_three_percent_and_twice_the_half_width(self) -> None:
        self.assertEqual(standing.aa_gate({"ratio": 1.0, "low": 0.99, "high": 1.01})["gate"], 0.03)
        wide = standing.aa_gate({"ratio": 1.0, "low": 0.96, "high": 1.04})
        self.assertAlmostEqual(wide["gate"], 0.08)
        self.assertTrue(wide["contains_one"])
        self.assertFalse(standing.aa_gate({"ratio": 1.1, "low": 1.05, "high": 1.15})["contains_one"])


class Microseconds(unittest.TestCase):
    BENCH_RS = "fn run() {\n            samples.push(duration.as_millis() as usize);\n}\n"

    def test_micros_patch_applies_once_and_refuses_drift(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            bench = Path(tmp) / "bench.rs"
            bench.write_text(self.BENCH_RS)
            standing.apply_micros_fix(bench)
            self.assertIn("duration.as_micros() as usize", bench.read_text())
            self.assertNotIn("as_millis", bench.read_text())
            # Applying it again finds no millisecond line: that must fail
            # rather than build an unpatched copy.
            with self.assertRaises(SystemExit) as raised:
                standing.apply_micros_fix(bench)
            self.assertIn(standing.VTEBENCH_MICROS_FIX[0], str(raised.exception))
            bench.write_text(self.BENCH_RS * 2)
            with self.assertRaises(SystemExit) as raised:
                standing.apply_micros_fix(bench)
            self.assertIn("found 2", str(raised.exception))

    def test_a_modified_vtebench_copy_is_detected(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp) / "copy"
            (tree / "src").mkdir(parents=True)
            (tree / "src" / "bench.rs").write_text("a")
            (tree / "target").mkdir()
            (tree / "target" / "junk").write_text("ignored")
            digest = standing.tree_digest(tree)
            (tree / "target" / "junk").write_text("changed build output is not source")
            self.assertEqual(standing.tree_digest(tree), digest)
            (tree / "src" / "bench.rs").write_text("b")
            self.assertNotEqual(standing.tree_digest(tree), digest)

    def test_the_copy_is_never_cleaned_through_a_symlink(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            outside = Path(tmp) / "outside"
            outside.mkdir()
            (outside / "keep").write_text("not the harness's")
            link = Path(tmp) / "vtebench-us"
            link.symlink_to(outside)
            with self.assertRaises(SystemExit):
                standing.reset_copy(link)
            self.assertEqual((outside / "keep").read_text(), "not the harness's")
            owned = Path(tmp) / "owned"
            (owned / "target").mkdir(parents=True)
            (owned / "src").mkdir()
            (owned / "stale").write_text("x")
            (owned / "linked").symlink_to(outside)
            standing.reset_copy(owned)
            self.assertEqual(sorted(p.name for p in owned.iterdir()), ["target"])
            self.assertEqual((outside / "keep").read_text(), "not the harness's")
            # A cached build directory that leads outside is dropped, not kept.
            (outside / "vtebench").write_text("someone else's binary")
            (owned / "target" / "release").symlink_to(outside)
            standing.reset_copy(owned)
            self.assertFalse((owned / "target" / "release").exists())
            self.assertEqual((outside / "vtebench").read_text(), "someone else's binary")

    def test_micros_dat_parses_to_ms(self) -> None:
        samples = standing.parse_dat_samples("cursor_motion dense\n13500 20000\n14500 _\n", "us")
        self.assertEqual(samples, {"cursor_motion": [13.5, 14.5], "dense": [20.0]})
        row = standing.vtebench_row("cursor_motion\n13500\n14500\n", "us")
        self.assertEqual(row["means_ms"], {"cursor_motion": 14.0})
        self.assertEqual(row["unit"], "us")


class Safety(unittest.TestCase):
    APP = "/Applications/kettle.app/Contents/MacOS/kettle"

    def test_build_runs_only_for_the_default_kettle_path(self) -> None:
        default = str(standing.DEFAULT_KETTLE)
        self.assertTrue(standing.needs_build(default, None, no_build=False))
        self.assertFalse(standing.needs_build(self.APP, None, no_build=False))
        self.assertFalse(standing.needs_build(default, None, no_build=True))
        self.assertFalse(standing.needs_build(default, self.APP, no_build=False))

    def test_bare_binary_needs_allow_bare(self) -> None:
        self.assertTrue(standing.in_app_bundle(Path(self.APP)))
        self.assertFalse(standing.in_app_bundle(standing.DEFAULT_KETTLE))
        standing.require_bundles({"kettle": self.APP}, allow_bare=False)
        with self.assertRaises(SystemExit) as raised:
            standing.require_bundles({"kettle-a": self.APP, "kettle-b": "/tmp/kettle"}, allow_bare=False)
        self.assertIn("/tmp/kettle", str(raised.exception))
        standing.require_bundles({"kettle-b": "/tmp/kettle"}, allow_bare=True)

    @unittest.skipUnless(sys.platform == "darwin", "bundles and codesign are macOS")
    def test_a_local_build_is_measured_inside_a_signed_bundle(self) -> None:
        if SNAPSHOT_LAYOUT:
            self.skipTest('packaging Info.plist needs the real repository')
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "kettle"
            binary.write_bytes(Path("/usr/bin/true").read_bytes())
            binary.chmod(0o755)
            bundled = standing.bundle_kettle(binary, Path(tmp) / "Local.app", template=None)
            self.assertTrue(standing.in_app_bundle(bundled))
            self.assertEqual(subprocess.run([str(bundled)]).returncode, 0)
            self.assertTrue((bundled.parents[1] / "Info.plist").exists())
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(bundled.parents[2])], check=True)

    def test_a_bundle_never_overwrites_its_template_or_binary(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            template = Path(tmp) / "Kettle.app"
            (template / "Contents" / "MacOS").mkdir(parents=True)
            binary = template / "Contents" / "MacOS" / "kettle"
            binary.write_bytes(b"x")
            for dest in (template, Path(tmp)):
                with self.assertRaises(SystemExit):
                    standing.bundle_kettle(binary, dest, template)
            self.assertTrue(binary.exists())

    @unittest.skipUnless(sys.platform == "darwin", "bundles and codesign are macOS")
    def test_a_bundle_never_touches_another_runs_work(self) -> None:
        if SNAPSHOT_LAYOUT:
            self.skipTest('packaging Info.plist needs the real repository')
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "kettle"
            binary.write_bytes(Path("/usr/bin/true").read_bytes())
            other = Path(tmp) / "Local.app.partial"
            other.mkdir()
            (other / "marker").write_text("another run")
            standing.bundle_kettle(binary, Path(tmp) / "Local.app", None)
            standing.bundle_kettle(binary, Path(tmp) / "Local.app", None)
            self.assertEqual((other / "marker").read_text(), "another run")
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["Local.app", "Local.app.partial", "kettle"])

    def test_a_bundle_destination_that_is_a_symlink_is_refused(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            outside = Path(tmp) / "outside.app"
            outside.mkdir()
            (outside / "keep").write_text("not the harness's")
            link = Path(tmp) / "Local.app"
            link.symlink_to(outside)
            binary = Path(tmp) / "kettle"
            binary.write_bytes(b"x")
            with self.assertRaises(SystemExit):
                standing.bundle_kettle(binary, link, None)
            self.assertEqual((outside / "keep").read_text(), "not the harness's")

    @unittest.skipUnless(sys.platform == "darwin", "bundles and codesign are macOS")
    def test_a_failed_bundle_keeps_the_previous_one(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "Local.app"
            (dest / "Contents").mkdir(parents=True)
            (dest / "Contents" / "keep").write_text("previous")
            with self.assertRaises(Exception):
                standing.bundle_kettle(Path(tmp) / "missing-binary", dest, None)
            self.assertEqual((dest / "Contents" / "keep").read_text(), "previous")
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["Local.app"])

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("swiftc"), "needs macOS and swiftc")
    def test_stopping_the_launch_probe_stops_its_terminal_and_drops_the_pid_file(self) -> None:
        # The harness never signals a pid itself: after the probe reaps the
        # terminal, that pid can belong to anything.
        import json
        import os
        import signal
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "launch"
            subprocess.run(["swiftc", "-O", "-o", str(probe), str(HERE / "macos-standing" / "launch.swift")],
                           check=True, capture_output=True)
            stamp = work / "stamp"
            process = subprocess.Popen([str(probe), str(work / "out.json"), str(stamp), "60", "--",
                                        "/bin/sleep", "30"])
            pid_file = Path(str(stamp) + ".pid")
            deadline = time.monotonic() + 10
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            child = int(pid_file.read_text())
            process.send_signal(signal.SIGTERM)
            self.assertEqual(process.wait(timeout=10), 0)
            result = json.loads((work / "out.json").read_text())
            self.assertTrue(result["stopped"])
            self.assertFalse(result["killed"])
            self.assertFalse(pid_file.exists())
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("swiftc"), "needs macOS and swiftc")
    def test_a_launch_probe_whose_harness_is_gone_stops_its_terminal(self) -> None:
        # A harness killed mid-round must not leave its terminal running on
        # (a measured Ghostty would write its frame whenever it closed).
        import json
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "launch"
            subprocess.run(["swiftc", "-O", "-o", str(probe), str(HERE / "macos-standing" / "launch.swift")],
                           check=True, capture_output=True)
            stamp = work / "stamp"
            # A stand-in harness: it starts the probe in its own session, as
            # the Runner does, says the probe's pid, and waits to be killed.
            harness = subprocess.Popen(
                [sys.executable, "-c",
                 "import subprocess, sys, time\n"
                 "p = subprocess.Popen(sys.argv[1:], start_new_session=True)\n"
                 "print(p.pid, flush=True)\ntime.sleep(120)\n",
                 str(probe), str(work / "out.json"), str(stamp), "60", "--", "/bin/sleep", "60"],
                stdout=subprocess.PIPE, text=True)
            self.addCleanup(harness.stdout.close)
            probe_pid = int(harness.stdout.readline())
            pid_file = Path(str(stamp) + ".pid")
            deadline = time.monotonic() + 10
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            child = int(pid_file.read_text())
            harness.kill()
            harness.wait(10)
            # Far below the terminal's 60 s: the probe noticed its parent go.
            self.assertTrue(gone_within(probe_pid, 15), "the probe outlived its harness")
            self.assertTrue(gone_within(child, 5), "the terminal outlived its harness")
            result = json.loads((work / "out.json").read_text())
            self.assertTrue(result["stopped"])
            self.assertFalse(result["killed"])

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("swiftc"), "needs macOS and swiftc")
    def test_a_launch_probe_orphaned_before_it_starts_spawns_nothing(self) -> None:
        # The harness can die between spawning the probe and the probe's
        # first look at its parent; launchd has adopted it by then.
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "launch"
            subprocess.run(["swiftc", "-O", "-o", str(probe), str(HERE / "macos-standing" / "launch.swift")],
                           check=True, capture_output=True)
            stamp = work / "stamp"
            # A holder in its own session, which this test leaves unreaped
            # until the end so its group stays this test's to kill, forks a
            # middle process. That forks the probe's process, says its pid
            # and exits; the probe's process execs the probe only once it is
            # orphaned, still in the holder's group.
            helper = ("import os, sys, time\n"
                      "middle = os.fork()\n"
                      "if middle:\n"
                      "    os.waitpid(middle, 0); time.sleep(120); os._exit(0)\n"
                      "pid = os.fork()\n"
                      "if pid:\n"
                      "    print(pid, flush=True); os._exit(0)\n"
                      "null = os.open(os.devnull, os.O_RDWR)\n"
                      "os.dup2(null, 1); os.dup2(null, 2)\n"
                      "while os.getppid() != 1: time.sleep(0.01)\n"
                      "os.execv(sys.argv[1], sys.argv[1:])\n")
            holder = subprocess.Popen([sys.executable, "-c", helper, str(probe), str(work / "out.json"), str(stamp),
                                       "60", "--", "/bin/sleep", "60"], stdout=subprocess.PIPE, text=True,
                                      start_new_session=True)
            try:
                orphan = int(holder.stdout.readline())
                self.assertTrue(gone_within(orphan, 10), "the orphaned probe ends")
                self.assertFalse(Path(str(stamp) + ".pid").exists(), "no terminal was spawned")
            finally:
                # Whatever a failing probe left in the group goes with it.
                os.killpg(holder.pid, signal.SIGKILL)
                holder.wait(10)
                holder.stdout.close()

    def test_idle_and_latency_count_from_the_settled_grid(self) -> None:
        import inspect

        for method in (standing.Runner.idle, standing.Runner.latency):
            self.assertIn('self.wait_for(self.work / "grid"', inspect.getsource(method), method.__name__)

    def test_the_settled_grid_file_appears_whole(self) -> None:
        source = (HERE / "macos-standing" / "stamp.c").read_text()
        settle = source[source.index("static int settle("):source.index("int main(")]
        self.assertIn('"%s.tmp"', settle)
        self.assertIn("rename(tmp, argv[5])", settle)

    def test_a_probe_that_ignores_stop_is_killed_with_its_group_and_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "stubborn.sh"
            # Ignores SIGTERM and starts a child in the probe's process group.
            probe.write_text("#!/bin/sh\ntrap '' TERM\n/bin/sleep 60 &\necho $! > \"$2.pid\"\nwait\n")
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})
            process = runner.launch("kettle", "true", 1)
            pid_file = work / "stamp.pid"
            deadline = time.monotonic() + 5
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            child = int(pid_file.read_text())
            self.assertFalse(runner.stop(process, 0.5))
            self.assertIsNotNone(process.returncode, "the probe is reaped")
            self.assertTrue(gone_within(child), "the probe's child is killed with its group")

    def test_a_terminal_the_probe_had_to_kill_is_not_a_clean_stop(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "killer.sh"
            # Stops when asked, but reports that it had to SIGKILL the terminal.
            probe.write_text('#!/bin/sh\ntrap \'echo "{\\"killed\\": true}" > "$1"; exit 0\' TERM\n'
                             ': > "$1.ready"\nwhile :; do sleep 0.05; done\n')
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})
            process = runner.launch("kettle", "true", 1)
            # Signal only once the trap is set: a loaded machine can take
            # longer than any fixed sleep to start the shell.
            ready = work / "launch.json.ready"
            deadline = time.monotonic() + 10
            while not ready.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(ready.exists(), "the probe never set its trap")
            self.assertFalse(runner.stop(process, 5))

    def test_a_probe_that_never_finishes_is_cleaned_up_and_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "wedged.sh"
            probe.write_text("#!/bin/sh\ntrap '' TERM\n/bin/sleep 60 &\necho $! > \"$2.pid\"\nwait\n")
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})
            runner.grace = 0.2
            process = runner.launch("kettle", "true", 0.2)
            deadline = time.monotonic() + 5
            while not (work / "stamp.pid").exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            child = int((work / "stamp.pid").read_text())
            self.assertIn("error", runner.finish(process, 0.2))
            self.assertIsNotNone(process.returncode)
            self.assertTrue(gone_within(child), "the probe's child is killed with its group")

    def test_the_runner_stops_terminals_through_the_probe(self) -> None:
        import inspect

        source = inspect.getsource(standing.Runner.stop)
        self.assertNotIn("os.kill(", source)
        self.assertNotIn("/bin/kill", source)
        self.assertIn("send_signal", source)

    @unittest.skipUnless(sys.platform == "darwin", "codesign is macOS")
    def test_published_identity_has_no_path_or_team(self) -> None:
        public, local = standing.terminal_identity("/usr/bin/true")
        self.assertNotIn("path", public)
        self.assertNotIn("teamidentifier", public)
        self.assertIn("sha256", public)
        self.assertEqual(local["path"], "/usr/bin/true")

    def test_codesign_fields_parse_and_unset_team_is_none(self) -> None:
        text = ("Executable=/Applications/Alacritty.app/Contents/MacOS/alacritty\n"
                "Identifier=org.alacritty\nCandidateCDHash sha256=aaaa\nCDHash=4f1c2d\n"
                "Signature=adhoc\nTeamIdentifier=not set\n")
        self.assertEqual(standing.parse_codesign(text), {"cdhash": "4f1c2d", "teamidentifier": None})
        signed = "CDHash=99ab\nTeamIdentifier=ABCDE12345\n"
        self.assertEqual(standing.parse_codesign(signed), {"cdhash": "99ab", "teamidentifier": "ABCDE12345"})

    def test_a_session_never_writes_over_an_earlier_one(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            used = Path(tmp) / "s1"
            used.mkdir()
            (used / "results.json").write_text("{}")
            with self.assertRaises(SystemExit):
                standing.claim_out_dir(used)
            fresh = standing.claim_out_dir(Path(tmp) / "s2")
            self.assertTrue(fresh.is_dir())
            # Any directory that already exists is someone else's, even empty.
            with self.assertRaises(SystemExit):
                standing.claim_out_dir(fresh)
            default = standing.default_out_dir(Path(tmp))
            self.assertRegex(default.name, r"^\d{4}-\d{2}-\d{2}-\d{6}$")

    def test_results_are_rewritten_after_each_row(self) -> None:
        import json
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "results.json"
            results = {"schema": 2, "workloads": {"startup": {"kettle": []}}}
            recorder = standing.Recorder(path, results)
            recorder.write()
            results["workloads"]["startup"]["kettle"].append({"window_ms": 1.0})
            recorder.write()
            self.assertEqual(json.loads(path.read_text())["workloads"]["startup"]["kettle"], [{"window_ms": 1.0}])
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["results.json"])

    def test_the_harness_never_signals_a_process_by_name(self) -> None:
        # A name-based kill would also hit the terminal hosting the session
        # that runs the harness. Terminals are stopped by the pid launch wrote.
        import re

        for path in sorted(HERE.rglob("*")):
            if path.suffix not in (".py", ".sh", ".swift", ".c") or path.name == Path(__file__).name:
                continue
            text = path.read_text()
            for banned in (r"\bkillall\b", r"\bpkill\b", r"osascript[^\n]*\bquit\b",
                           r"runningApplications\(withBundleIdentifier"):
                self.assertIsNone(re.search(banned, text), f"{path.relative_to(HERE)} matches {banned}")


PS = """  123     1   0.0 /Applications/Ghostty.app/Contents/MacOS/ghostty
  130   123   0.0 /usr/bin/login
  140   130   1.5 /Users/someone/.local/bin/claude
  150   140   0.0 /bin/zsh
  160   150   0.4 /usr/bin/python3
  200     1  95.0 /Users/someone/.cargo/bin/cargo
  300     1   3.0 codex
  400   200  50.0 /Users/someone/.rustup/toolchains/stable/bin/rustc
  500     1   0.0 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
  600     1   0.2 /Applications/kitty.app/Contents/MacOS/kitty
  700     1  45.0 /Users/someone/.local/bin/claude
"""


class Preflight(unittest.TestCase):
    FIELD = {"/Applications/Ghostty.app/Contents/MacOS/ghostty": "ghostty",
             "/Applications/kitty.app/Contents/MacOS/kitty": "kitty"}

    def test_power_parsers(self) -> None:
        ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=7471203)\t7%; charging; 20:00 remaining present: true\n"
        battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t80%; discharging; present: true\n"
        self.assertEqual(standing.parse_pmset_batt(ac), {"source": "AC", "percent": 7})
        self.assertEqual(standing.parse_pmset_batt(battery), {"source": "Battery", "percent": 80})
        self.assertTrue(standing.parse_low_power("Currently in use:\n powermode            1\n"))
        self.assertTrue(standing.parse_low_power("Currently in use:\n lowpowermode         1\n"))
        self.assertFalse(standing.parse_low_power(
            "Currently in use:\n powermode            0\nBattery Power:\n powermode            1\n"))

    def test_low_power_mode_that_cannot_be_read_is_unknown(self) -> None:
        # A sandboxed `pmset -g` shows only system-wide settings.
        self.assertIsNone(standing.parse_low_power("System-wide power settings:\n SleepDisabled\t\t1\n"))
        custom = ("Battery Power:\n lowpowermode         1\n sleep 1\nAC Power:\n lowpowermode         0\n"
                  " sleep 0\n")
        self.assertFalse(standing.parse_low_power_custom(custom, "AC"))
        self.assertTrue(standing.parse_low_power_custom(custom, "Battery"))
        self.assertIsNone(standing.parse_low_power_custom("AC Power:\n sleep 0\n", "AC"))

    def test_lock_and_time_machine_parsers(self) -> None:
        self.assertTrue(standing.parse_ioreg_locked(
            '"IOConsoleUsers" = ({"CGSSessionScreenIsLocked"=Yes,"kCGSSessionOnConsoleKey"=Yes})'))
        self.assertFalse(standing.parse_ioreg_locked('"IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes})'))
        self.assertIsNone(standing.parse_ioreg_locked("+-o Root  <class IORegistryEntry>"))
        self.assertTrue(standing.parse_tmutil_running("{\n    Running = 1;\n}"))
        self.assertFalse(standing.parse_tmutil_running("{\n    Percent = \"-1\";\n    Running = 0;\n}"))

    def test_busy_tools_count_only_while_they_use_cpu(self) -> None:
        # An interactive codex session left open idles at a few percent; it is
        # recorded, not refused.
        procs = standing.parse_ps(PS)
        self.assertEqual([p["name"] for p in standing.busy_processes(procs)], ["cargo", "rustc", "claude"])
        self.assertIn({"name": "codex", "cpu": 3.0}, standing.tools_running(procs))

    def test_field_terminals_exclude_only_the_host_ancestor(self) -> None:
        procs = standing.parse_ps(PS)
        self.assertEqual(standing.ancestors(procs, 160), [150, 140, 130, 123])
        state = {"procs": procs, "self_pid": 160, "host_pid": 123, "field": self.FIELD,
                 "power": {"source": "AC", "percent": 50}, "low_power": False, "locked": False,
                 "time_machine": False, "load": [0.5, 0.5, 0.5], "harness_dirty": False,
                 "display": "1920 x 1080 px, 1920 x 1080 pt @ 60.00Hz"}
        refusals = standing.preflight_refusals(state)
        self.assertEqual(refusals, ["cargo, claude, rustc running", "field terminal already running: kitty (pid 600)"])
        not_host = dict(state, host_pid=600)
        self.assertIn("--host-pid 600 is not an ancestor of this harness", standing.preflight_refusals(not_host))
        no_host = dict(state, host_pid=None)
        self.assertIn("field terminal already running: ghostty (pid 123)", standing.preflight_refusals(no_host))

    def test_the_host_terminal_is_recorded_by_pid_and_name(self) -> None:
        procs = standing.parse_ps(PS)
        self.assertEqual(standing.host_terminal_of(procs, self.FIELD, 123), {"pid": 123, "name": "ghostty"})
        self.assertIsNone(standing.host_terminal_of(procs, self.FIELD, None))

    def test_display_mode_carries_resolution_scale_and_refresh(self) -> None:
        data = {"SPDisplaysDataType": [{"spdisplays_ndrvs": [
            {"_name": "Other", "spdisplays_main": "spdisplays_no", "_spdisplays_pixels": "3840 x 2160",
             "_spdisplays_resolution": "1920 x 1080 @ 120.00Hz"},
            {"_name": "ROG", "_spdisplays_display-serial-number": "1010101", "spdisplays_main": "spdisplays_yes",
             "_spdisplays_pixels": "1920 x 1080", "_spdisplays_resolution": "1920 x 1080 @ 60.00Hz"}]}]}
        self.assertEqual(standing.parse_display(data), "1920 x 1080 px, 1920 x 1080 pt @ 60.00Hz")
        self.assertIsNone(standing.parse_display({"SPDisplaysDataType": []}))

    def test_every_condition_refuses(self) -> None:
        state = {"procs": [], "self_pid": 1, "host_pid": None, "field": {},
                 "power": {"source": "Battery", "percent": 50}, "low_power": True, "locked": True,
                 "time_machine": True, "load": [2.5, 1.0, 1.0], "harness_dirty": True, "display": None}
        refusals = standing.preflight_refusals(state)
        for reason in ("battery power", "Low Power Mode is on", "screen is locked",
                       "Time Machine backup running", "load 2.50", "the harness has local changes",
                       "display mode unknown"):
            self.assertTrue(any(reason in r for r in refusals), f"{reason} not in {refusals}")

    def test_unreadable_state_refuses_instead_of_passing(self) -> None:
        state = {"procs": None, "self_pid": 1, "host_pid": None, "field": {},
                 "power": {"source": "unknown", "percent": None}, "low_power": None, "locked": None,
                 "time_machine": None, "load": [0.5, 0.5, 0.5], "harness_dirty": None, "display": None}
        refusals = standing.preflight_refusals(state)
        for reason in ("could not list processes", "power source unknown", "Low Power Mode unknown",
                       "screen lock state unknown", "Time Machine state unknown", "harness state unknown"):
            self.assertTrue(any(reason in r for r in refusals), f"{reason} not in {refusals}")

    def test_a_harness_git_cannot_read_is_unknown(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(standing.harness_revision(Path(tmp)), {"harness_tree": None, "harness_dirty": None})
        if SNAPSHOT_LAYOUT:
            self.skipTest("live harness revision needs the real repository")
        self.assertIsNotNone(standing.harness_revision()["harness_tree"])

    def test_everything_but_the_inert_files_decides_the_harness_revision(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)

            def git(*args: str) -> None:
                subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                                "-c", "commit.gpgsign=false", *args], check=True, capture_output=True)

            git("init", "-q")
            # Kettle's own ignore rules, which hide *.pyc and __pycache__.
            shutil.copy(HERE / "snapshot.gitignore" if SNAPSHOT_LAYOUT else HERE.parents[1] / ".gitignore",
                        repo / ".gitignore")
            perf = repo / "scripts" / "perf"
            (perf / "macos-standing").mkdir(parents=True)
            inert = ["README.md", "macos-standing-self-test.py", "macos-compare.sh",
                     "macos-standing/startup-phases.fixture"]
            for name in ["macos-standing.py", "macos-standing/stamp.c", *inert]:
                (perf / name).write_text(name)
            git("add", "-A")
            git("commit", "-q", "-m", "base")
            base = standing.harness_revision(repo)
            self.assertEqual(base["harness_dirty"], False)
            self.assertIsNotNone(base["harness_tree"])
            # An inert file, changed or committed: nothing moves.
            for name in inert:
                (perf / name).write_text("changed")
                self.assertEqual(standing.harness_revision(repo), base, name)
            git("commit", "-q", "-am", "inert")
            self.assertEqual(standing.harness_revision(repo), base)
            # Anything else, whatever its name or kind: a local change until
            # committed, then a new version.
            (perf / "pkg").mkdir()
            for name in ("macos-standing.py", "macos-standing/stamp.c", "sitecustomize.py", "statistics.py",
                         "pkg/caf\u00e9.json", "config.fixture -> active.json"):
                (perf / name).write_text("changed")
                self.assertTrue(standing.harness_revision(repo)["harness_dirty"], name)
                git("add", "-A")
                git("commit", "-q", "-m", "change")
                after = standing.harness_revision(repo)
                self.assertEqual(after["harness_dirty"], False, name)
                self.assertNotEqual(after["harness_tree"], base["harness_tree"], name)
                base = after
            # An ignored sourceless module counts; the bytecode cache and
            # Finder's metadata do not.
            (perf / "statistics.pyc").write_bytes(b"bytecode")
            self.assertTrue(standing.harness_revision(repo)["harness_dirty"])
            (perf / "statistics.pyc").unlink()
            (perf / "__pycache__").mkdir()
            (perf / "__pycache__" / "statistics.cpython-314.pyc").write_bytes(b"cache")
            (perf / ".DS_Store").write_bytes(b"finder")
            self.assertEqual(standing.harness_revision(repo), base)
            # An untracked symlink to a package.
            (perf / "sitecustomize").symlink_to(perf / "pkg")
            self.assertTrue(standing.harness_revision(repo)["harness_dirty"])
            (perf / "sitecustomize").unlink()
            # A rename from an inert name to one that counts.
            git("mv", "scripts/perf/macos-compare.sh", "scripts/perf/helper.sh")
            self.assertTrue(standing.harness_revision(repo)["harness_dirty"])
            git("mv", "scripts/perf/helper.sh", "scripts/perf/macos-compare.sh")
            self.assertEqual(standing.harness_revision(repo), base)
            # How Git quotes paths changes nothing.
            for quote in ("true", "false"):
                git("config", "core.quotePath", quote)
                self.assertEqual(standing.harness_revision(repo), base, quote)
            # An index flag that hides an edit from `git status` hides it
            # from nothing here.
            for flag in ("--assume-unchanged", "--skip-worktree"):
                git("update-index", flag, "scripts/perf/macos-standing.py")
                (perf / "macos-standing.py").write_text("edited under " + flag)
                edited = standing.harness_revision(repo)
                self.assertTrue(edited["harness_dirty"], flag)
                self.assertNotEqual(edited["harness_tree"], base["harness_tree"], flag)
                git("update-index", flag.replace("--", "--no-"), "scripts/perf/macos-standing.py")
                git("checkout", "--", "scripts/perf/macos-standing.py")
                self.assertEqual(standing.harness_revision(repo), base, flag)

    def test_the_walk_refuses_what_it_cannot_read_and_never_blocks(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            subprocess.run(["git", "init", "-q", tmp], check=True, capture_output=True)
            perf = repo / "scripts" / "perf"
            perf.mkdir(parents=True)
            # More than one read chunk: the streamed id is Git's.
            big = perf / "macos-standing.py"
            big.write_bytes(bytes(range(256)) * 12_000)
            subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                            "-c", "commit.gpgsign=false", "add", "-A"], check=True, capture_output=True)
            subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                            "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"], check=True, capture_output=True)
            self.assertEqual(standing.harness_revision(repo)["harness_dirty"], False)
            # A FIFO with no writer: listed, not opened. A subprocess bounds
            # the check, so a regression fails instead of hanging the suite.
            os.mkfifo(perf / "debug.pipe")
            probe = ("import importlib.util, sys; from pathlib import Path\n"
                     f"spec = importlib.util.spec_from_file_location('st', {str(HERE / 'macos-standing.py')!r})\n"
                     "st = importlib.util.module_from_spec(spec); spec.loader.exec_module(st)\n"
                     f"print(st.harness_revision(Path({tmp!r}))['harness_dirty'])\n")
            ran = subprocess.run([sys.executable, "-c", probe], capture_output=True, text=True, timeout=30)
            self.assertEqual(ran.stdout.strip(), "True", ran.stderr)
            (perf / "debug.pipe").unlink()
            # A directory it can enter but not list leaves the state unknown.
            hidden = perf / "statistics"
            hidden.mkdir()
            (hidden / "__init__.py").write_text("x = 1\n")
            hidden.chmod(0o300)
            try:
                self.assertEqual(standing.harness_revision(repo), {"harness_tree": None, "harness_dirty": None})
            finally:
                hidden.chmod(0o700)

    def test_a_sha256_repository_is_clean_when_it_matches_head(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            made = subprocess.run(["git", "init", "-q", "--object-format=sha256", tmp], capture_output=True)
            if made.returncode != 0:
                self.skipTest("this git cannot make a sha256 repository")
            perf = Path(tmp) / "scripts" / "perf"
            perf.mkdir(parents=True)
            (perf / "macos-standing.py").write_text("harness")
            subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                            "-c", "commit.gpgsign=false", "add", "-A"], check=True, capture_output=True)
            subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                            "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"], check=True, capture_output=True)
            revision = standing.harness_revision(Path(tmp))
            self.assertEqual(revision["harness_dirty"], False)
            (perf / "macos-standing.py").write_text("edited")
            self.assertTrue(standing.harness_revision(Path(tmp))["harness_dirty"])

    def test_session_metadata_keys_are_distinct(self) -> None:
        # A repeated key in the metadata literal silently drops the first value.
        import ast
        import inspect

        tree = ast.parse(inspect.getsource(standing.main).lstrip())
        for node in ast.walk(tree):
            if isinstance(node, ast.Dict):
                keys = [k.value for k in node.keys if isinstance(k, ast.Constant)]
                self.assertEqual(len(keys), len(set(keys)), f"duplicate keys in {keys}")

    def test_a_diagnostic_footprint_session_never_counts(self) -> None:
        self.assertFalse(standing.session_countable(
            {"refusals": [], "bare": False, "complete": True, "footprint_detail": True}))
        self.assertIn("footprint_detail", standing.SESSION_KEYS)

    def test_config_text_stays_out_of_published_results(self) -> None:
        public, local = standing.config_record({"kettle-a": "", "kettle-b": "background-image = /Users/me/secret.png"})
        self.assertNotIn("/Users/me", json_dumps(public))
        self.assertEqual(local["kettle-b"], "background-image = /Users/me/secret.png")
        self.assertEqual(public["kettle-a"], "")
        self.assertRegex(public["kettle-b"], r"^sha256:[0-9a-f]{64}$")

    def test_a_session_counts_only_when_clean_bundled_and_complete(self) -> None:
        self.assertTrue(standing.session_countable({"refusals": [], "bare": False, "complete": True}))
        self.assertFalse(standing.session_countable({"refusals": ["load 2.5"], "bare": False, "complete": True}))
        self.assertFalse(standing.session_countable({"refusals": [], "bare": True, "complete": True}))
        self.assertFalse(standing.session_countable({"refusals": [], "bare": False, "complete": False}))


def write_session(root: Path, name: str, meta, workloads, dats=None) -> Path:
    import json

    folder = root / name
    folder.mkdir()
    results = {"context": "test", "terminals": list(next(iter(workloads.values()))), "skipped": {},
               "workloads": workloads}
    if meta is not None:
        results.update({"schema": 2, "meta": meta})
    (folder / "results.json").write_text(json.dumps(results))
    for filename, text in (dats or {}).items():
        (folder / filename).write_text(text)
    return folder


class Combine(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_combine_reads_schema_1_dirs_and_labels_rows(self) -> None:
        # The 4.7.0 layout: rows hold whole-ms medians, the .dat files hold
        # the samples. Means must come from the .dat files.
        workloads = {
            "startup": {"kettle": [{"window_ms": 200.0}, {"window_ms": 210.0}, {"window_ms": 190.0}],
                        "alacritty": [{"window_ms": 180.0}, {"window_ms": 185.0}, {"window_ms": 175.0}]},
            "vtebench": {"kettle": [{"cursor_motion": 14.0}], "alacritty": [{"cursor_motion": 14.0}]},
        }
        dats = {"kettle-r0.dat": dat({"cursor_motion": [14, 14, 14, 17, 17]}),
                "alacritty-r0.dat": dat({"cursor_motion": [14, 14, 14, 14, 15]})}
        folder = write_session(self.root, "standing-470", None, workloads, dats)
        combined = standing.combine([folder])
        self.assertEqual(combined["sessions"][0]["dir"], "standing-470", "no absolute paths")
        self.assertEqual(combined["sessions"][0]["schema"], 1)
        self.assertFalse(combined["sessions"][0]["countable"])
        startup = combined["rows"]["startup.window_ms"]
        self.assertEqual(startup["per_session"][0]["estimates"], {"kettle": 200.0, "alacritty": 180.0})
        self.assertNotIn("published", startup["terminals"].get("kettle", {}), "schema 1 never publishes")
        motion = combined["rows"]["vtebench.cursor_motion"]
        self.assertAlmostEqual(motion["sessions"][0]["ratio"], 15.2 / 14.2)
        self.assertEqual(motion["claim"]["label"], "insufficient sessions")

    def test_a_schema_1_round_without_its_dat_file_is_not_read_as_means(self) -> None:
        workloads = {"vtebench": {"kettle": [{"cursor_motion": 14.0}], "alacritty": [{"cursor_motion": 14.0}]}}
        folder = write_session(self.root, "old", None, workloads, {"kettle-r0.dat": dat({"cursor_motion": [15]})})
        combined = standing.combine([folder])
        self.assertNotIn("alacritty", combined["rows"]["vtebench.cursor_motion"]["per_session"][0]["estimates"])

    def test_an_interrupted_session_never_counts(self) -> None:
        folders = []
        for day in (1, 2, 3):
            meta = {"date": f"2026-10-0{day}", "started": f"2026-10-0{day}T01:00:00", "refusals": [],
                    "bare": False, "complete": day != 2, "label": f"s{day}", "mode": "standing",
                    "rounds": {"startup": 5}, "warmup": 0}
            workloads = {"startup": {"kettle": [{"window_ms": 150.0, "child_ms": 200.0}] * 5,
                                     "wezterm": [{"window_ms": 170.0, "child_ms": 200.0}] * 5}}
            folders.append(write_session(self.root, f"s{day}", meta, workloads))
        combined = standing.combine(folders)
        self.assertEqual([s["countable"] for s in combined["sessions"]], [True, False, True])
        self.assertEqual(combined["rows"]["startup.window_ms"]["claim"]["label"], "insufficient sessions")

    def test_a_session_with_a_failed_round_never_counts(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "err", "mode": "standing", "rounds": {"startup": 2}, "warmup": 0}
        workloads = {"startup": {"kettle": [{"window_ms": 150.0}, {"error": "no window"}],
                                 "wezterm": [{"window_ms": 170.0}] * 2}}
        combined = standing.combine([write_session(self.root, "err", meta, workloads)])
        self.assertFalse(combined["sessions"][0]["countable"])

    def test_only_countable_sessions_feed_published_values(self) -> None:
        folders = []
        for day, kettle, countable in ((1, 150.0, True), (2, 999.0, False)):
            meta = {"date": f"2026-10-0{day}", "started": f"2026-10-0{day}T01:00:00", "refusals": [],
                    "bare": not countable, "complete": True, "label": f"s{day}", "mode": "standing",
                    "rounds": {"startup": 1}, "warmup": 0}
            folders.append(write_session(self.root, f"s{day}", meta, {"startup": {
                "kettle": [{"window_ms": kettle, "child_ms": 200.0}], "wezterm": [{"window_ms": 170.0, "child_ms": 200.0}]}}))
        row = standing.combine(folders)["rows"]["startup.window_ms"]
        self.assertEqual((row["terminals"]["kettle"]["published"], row["terminals"]["kettle"]["max"]), (150.0, 150.0))
        self.assertEqual([s["estimates"]["kettle"] for s in row["per_session"]], [150.0, 999.0])

    def test_an_aa_control_must_itself_count(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T00:00:00", "refusals": ["load 2.5 (limit 2.0)"],
                "bare": False, "complete": True, "label": "aa", "mode": "ab", "rounds": {"startup": 2}, "warmup": 0}
        aa = write_session(self.root, "aa", meta, {"startup": {
            "kettle-a": [{"child_ms": 200.0}] * 2, "kettle-b": [{"child_ms": 200.0}] * 2}})
        ab = write_session(self.root, "ab", dict(meta, refusals=[], label="ab"), {"startup": {
            "kettle-a": [{"child_ms": 200.0}] * 2, "kettle-b": [{"child_ms": 150.0}] * 2}})
        with self.assertRaises(SystemExit) as raised:
            standing.combine([ab], aa)
        self.assertIn("A/A", str(raised.exception))

    def test_sessions_from_different_setups_are_refused(self) -> None:
        folders = []
        for day, sha in ((1, "aa"), (2, "bb")):
            meta = {"date": f"2026-10-0{day}", "started": f"2026-10-0{day}T01:00:00", "refusals": [],
                    "bare": False, "complete": True, "label": f"s{day}", "mode": "standing",
                    "rounds": {"startup": 1}, "warmup": 0,
                    "identity": {"kettle": {"sha256": sha}, "wezterm": {"sha256": "ww"}}}
            folders.append(write_session(self.root, f"s{day}", meta, {"startup": {
                "kettle": [{"window_ms": 150.0, "child_ms": 200.0}], "wezterm": [{"window_ms": 170.0, "child_ms": 210.0}]}}))
        with self.assertRaises(SystemExit) as raised:
            standing.combine(folders)
        self.assertIn("identity", str(raised.exception))

    def test_an_aa_control_must_run_one_build_against_itself(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T00:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "aa", "mode": "ab", "rounds": {"startup": 2}, "warmup": 0,
                "identity": {"kettle-a": {"sha256": "old"}, "kettle-b": {"sha256": "new"}}}
        rows = {"startup": {"kettle-a": [{"child_ms": 200.0, "window_ms": 1.0}] * 2,
                            "kettle-b": [{"child_ms": 200.0, "window_ms": 1.0}] * 2}}
        aa = write_session(self.root, "aa", meta, rows)
        with self.assertRaises(SystemExit) as raised:
            standing.combine([write_session(self.root, "ab", dict(meta, label="ab"), rows)], aa)
        self.assertIn("same build", str(raised.exception))

    def test_a_row_the_aa_did_not_measure_gets_no_verdict(self) -> None:
        base = {"started": "2026-10-01T00:00:00", "refusals": [], "bare": False, "complete": True,
                "mode": "ab", "warmup": 0, "identity": {"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "x"}}}
        rounds = {"startup": 2, "idle": 2}
        aa = write_session(self.root, "aa", dict(base, date="2026-10-01", label="aa", rounds=rounds),
                           {"startup": {"kettle-a": [{"child_ms": 200.0, "window_ms": 1.0}] * 2,
                                        "kettle-b": [{"child_ms": 200.0, "window_ms": 1.0}] * 2}})
        folders = []
        for day in (1, 2):
            folders.append(write_session(self.root, f"ab{day}", dict(
                base, date=f"2026-10-0{day}", started=f"2026-10-0{day}T01:00:00", label=f"ab{day}",
                identity={"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "y"}}, rounds=rounds),
                {"idle": {"kettle-a": [{"cpu_percent": 0.2, "wakeups_per_second": 1.0, "footprint_mib": 30.0,
                                        "frontmost": True}] * 2,
                          "kettle-b": [{"cpu_percent": 0.1, "wakeups_per_second": 1.0, "footprint_mib": 30.0,
                                        "frontmost": True}] * 2}}))
        combined = standing.combine(folders, aa)
        self.assertEqual(combined["rows"]["idle.cpu_percent"]["verdict"]["verdict"], "A/A missing")

    def test_a_row_needs_most_rounds_paired_to_count(self) -> None:
        # Four of five idle rounds lost focus: one pair is not a session.
        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "thin", "mode": "standing", "rounds": {"idle": 5}, "warmup": 0}
        row = {"cpu_percent": 0.03, "wakeups_per_second": 0.5, "footprint_mib": 34.0}
        workloads = {"idle": {"kettle": [dict(row, frontmost=True)] + [dict(row, frontmost=False)] * 4,
                              "alacritty": [dict(row, frontmost=True)] * 5}}
        combined = standing.combine([write_session(self.root, "thin", meta, workloads)])
        self.assertTrue(combined["sessions"][0]["countable"])
        self.assertFalse(combined["rows"]["idle.cpu_percent"]["sessions"][0]["countable"])

    def test_an_aa_calibrates_ab_sessions_with_other_round_counts_and_workloads(self) -> None:
        # One A/A per harness version: a startup-only A/A at its own round
        # count gates a later A/B that ran more workloads at other counts.
        common = {"refusals": [], "bare": False, "complete": True, "mode": "ab", "warmup": 0,
                  "hw_model": "Mac17,6", "configs": {"kettle-a": "", "kettle-b": ""}}
        aa = write_session(self.root, "aa", dict(
            common, date="2026-10-01", started="2026-10-01T00:00:00", label="aa",
            rounds={"startup": 4, "idle": 5, "flood-memory": 5, "vtebench": 5},
            tool_hashes={"stamp": "s", "memsample": "m", "launch": "l"},
            identity={"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "x"}}), {"startup": {
                "kettle-a": [{"child_ms": 200.0 + i, "window_ms": 150.0} for i in range(4)],
                "kettle-b": [{"child_ms": 200.0 + i, "window_ms": 150.0} for i in range(4)]}})
        folders = [write_session(self.root, f"ab{day}", dict(
            common, date=f"2026-10-0{day}", started=f"2026-10-0{day}T01:00:00", label=f"ab{day}",
            rounds={"startup": 2, "idle": 2, "flood-memory": 2, "vtebench": 2},
            tool_hashes={"stamp": "s", "memsample": "m", "launch": "l", "vtebench": "v"},
            identity={"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "y"}}), {"startup": {
                "kettle-a": [{"child_ms": 200.0, "window_ms": 150.0}] * 2,
                "kettle-b": [{"child_ms": 150.0, "window_ms": 150.0}] * 2}}) for day in (1, 2)]
        combined = standing.combine(folders, aa)
        self.assertEqual(combined["rows"]["startup.child_ms"]["verdict"]["verdict"], "lower")
        # A tool both ran must still be the same binary.
        aa2 = write_session(self.root, "aa2", dict(
            common, date="2026-10-01", started="2026-10-01T00:00:00", label="aa2",
            rounds={"startup": 4}, tool_hashes={"stamp": "other", "memsample": "m", "launch": "l"},
            identity={"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "x"}}), {"startup": {
                "kettle-a": [{"child_ms": 200.0, "window_ms": 150.0}] * 4,
                "kettle-b": [{"child_ms": 200.0, "window_ms": 150.0}] * 4}})
        with self.assertRaises(SystemExit) as raised:
            standing.combine(folders, aa2)
        self.assertIn("tool_hashes", str(raised.exception))

    def test_an_aa_with_another_baseline_config_is_refused(self) -> None:
        base = {"started": "2026-10-01T00:00:00", "refusals": [], "bare": False, "complete": True, "mode": "ab",
                "warmup": 0, "rounds": {"startup": 2},
                "identity": {"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "x"}}}
        rows = {"startup": {"kettle-a": [{"child_ms": 200.0, "window_ms": 1.0}] * 2,
                            "kettle-b": [{"child_ms": 200.0, "window_ms": 1.0}] * 2}}
        aa = write_session(self.root, "aa", dict(base, date="2026-10-01", label="aa",
                                                 configs={"kettle-a": "font-size = 20", "kettle-b": "font-size = 20"}), rows)
        ab = write_session(self.root, "ab", dict(base, date="2026-10-01", label="ab",
                                                 configs={"kettle-a": "", "kettle-b": "cursor-blink = false"}), rows)
        with self.assertRaises(SystemExit) as raised:
            standing.combine([ab], aa)
        self.assertIn("baseline config", str(raised.exception))
        # The same baseline gates an A/B whose B side changes a setting.
        aa2 = write_session(self.root, "aa2", dict(base, date="2026-10-01", label="aa2",
                                                   configs={"kettle-a": "", "kettle-b": ""}), rows)
        standing.combine([ab], aa2)

    def test_only_documented_metrics_are_published(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "m", "mode": "standing", "rounds": {"startup": 1}, "warmup": 0}
        workloads = {"startup": {"kettle": [{"window_ms": 150.0, "child_ms": 200.0, "exit_ms": 1300.0}],
                                 "wezterm": [{"window_ms": 170.0, "child_ms": 210.0, "exit_ms": 1350.0}]}}
        rows = standing.combine([write_session(self.root, "m", meta, workloads)])["rows"]
        self.assertEqual(sorted(rows), ["startup.child_ms", "startup.window_ms"])

    def test_combined_output_never_overwrites(self) -> None:
        import argparse

        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "o", "mode": "standing", "rounds": {"startup": 1}, "warmup": 0}
        folder = write_session(self.root, "o", meta, {"startup": {"kettle": [{"window_ms": 1.0, "child_ms": 2.0}]}})
        args = argparse.Namespace(combine=[str(folder)], aa=None, out_dir=str(self.root / "out"))
        standing.run_combine(args)
        with self.assertRaises(SystemExit):
            standing.run_combine(args)

    def test_a_startup_round_the_probe_had_to_kill_is_incomplete(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "hung", "mode": "standing", "rounds": {"startup": 1}, "warmup": 0}
        workloads = {"startup": {"kettle": [{"window_ms": 150.0, "child_ms": 200.0, "killed": True}],
                                 "wezterm": [{"window_ms": 170.0, "child_ms": 210.0, "killed": False}]}}
        self.assertFalse(standing.combine([write_session(self.root, "hung", meta, workloads)])["sessions"][0]["countable"])

    def test_an_aa_row_with_thin_coverage_gives_no_gate(self) -> None:
        base = {"started": "2026-10-01T00:00:00", "refusals": [], "bare": False, "complete": True, "mode": "ab",
                "warmup": 0, "rounds": {"idle": 5}, "identity": {"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "x"}}}
        row = {"cpu_percent": 0.2, "wakeups_per_second": 1.0, "footprint_mib": 30.0}
        aa = write_session(self.root, "aa", dict(base, date="2026-10-01", label="aa"), {"idle": {
            "kettle-a": [dict(row, frontmost=True)] + [dict(row, frontmost=False)] * 4,
            "kettle-b": [dict(row, frontmost=True)] * 5}})
        folders = [write_session(self.root, f"ab{day}", dict(
            base, date=f"2026-10-0{day}", started=f"2026-10-0{day}T01:00:00", label=f"ab{day}",
            identity={"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "y"}}), {"idle": {
                "kettle-a": [dict(row, frontmost=True)] * 5,
                "kettle-b": [dict(row, cpu_percent=0.1, frontmost=True)] * 5}}) for day in (1, 2)]
        combined = standing.combine(folders, aa)
        self.assertEqual(combined["rows"]["idle.cpu_percent"]["verdict"]["verdict"], "A/A missing")

    def test_an_aa_from_another_setup_is_refused(self) -> None:
        base = {"started": "2026-10-01T00:00:00", "refusals": [], "bare": False, "complete": True, "mode": "ab",
                "warmup": 0, "rounds": {"startup": 2}, "hw_model": "Mac17,6",
                "identity": {"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "x"}}}
        rows = {"startup": {"kettle-a": [{"child_ms": 200.0, "window_ms": 1.0}] * 2,
                            "kettle-b": [{"child_ms": 200.0, "window_ms": 1.0}] * 2}}
        aa = write_session(self.root, "aa", dict(base, date="2026-10-01", label="aa", hw_model="Mac16,1"), rows)
        ab = write_session(self.root, "ab", dict(base, date="2026-10-01", label="ab",
                                                 identity={"kettle-a": {"sha256": "x"}, "kettle-b": {"sha256": "y"}}), rows)
        with self.assertRaises(SystemExit) as raised:
            standing.combine([ab], aa)
        self.assertIn("hw_model", str(raised.exception))

    def test_a_startup_round_without_a_time_is_incomplete(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "null", "mode": "standing", "rounds": {"startup": 1}, "warmup": 0}
        workloads = {"startup": {"kettle": [{"window_ms": None, "child_ms": 200.0}],
                                 "wezterm": [{"window_ms": 170.0, "child_ms": 210.0}]}}
        self.assertFalse(standing.combine([write_session(self.root, "null", meta, workloads)])["sessions"][0]["countable"])

    def test_a_session_missing_rounds_never_counts(self) -> None:
        meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                "complete": True, "label": "short", "mode": "standing", "rounds": {"startup": 5}, "warmup": 0}
        workloads = {"startup": {"kettle": [{"window_ms": 150.0}] * 4, "wezterm": [{"window_ms": 170.0}] * 5}}
        combined = standing.combine([write_session(self.root, "short", meta, workloads)])
        self.assertFalse(combined["sessions"][0]["countable"])

    def test_a_failed_aa_invalidates_the_verdict(self) -> None:
        aa_meta = {"date": "2026-10-01", "started": "2026-10-01T00:00:00", "refusals": [], "bare": False,
                   "complete": True, "label": "aa", "mode": "ab", "rounds": {"startup": 4}, "warmup": 0}
        aa = write_session(self.root, "aa", aa_meta, {"startup": {
            "kettle-a": [{"child_ms": 200.0 + i, "window_ms": 150.0} for i in range(4)],
            "kettle-b": [{"child_ms": 190.0 + i, "window_ms": 150.0} for i in range(4)]}})
        folders = []
        for day in (1, 2):
            meta = dict(aa_meta, date=f"2026-10-0{day}", started=f"2026-10-0{day}T01:00:00", label=f"ab{day}")
            folders.append(write_session(self.root, f"ab{day}", meta, {"startup": {
                "kettle-a": [{"child_ms": 200.0 + i, "window_ms": 150.0} for i in range(4)],
                "kettle-b": [{"child_ms": 150.0 + i, "window_ms": 150.0} for i in range(4)]}}))
        combined = standing.combine(folders, aa)
        self.assertEqual(combined["rows"]["startup.child_ms"]["verdict"]["verdict"], "A/A failed")

    def test_three_countable_sessions_earn_a_label(self) -> None:
        folders = []
        for day in (1, 2, 3):
            meta = {"date": f"2026-10-0{day}", "started": f"2026-10-0{day}T01:00:00", "refusals": [],
                    "bare": False, "complete": True, "label": f"s{day}", "mode": "standing",
                    "rounds": {"startup": 5}, "warmup": 0}
            workloads = {"startup": {
                "kettle": [{"window_ms": 150.0 + i, "child_ms": 200.0} for i in range(5)],
                "wezterm": [{"window_ms": 170.0 + i, "child_ms": 200.0} for i in range(5)],
                "alacritty": [{"window_ms": 180.0 + i, "child_ms": 200.0} for i in range(5)],
            }}
            folders.append(write_session(self.root, f"s{day}", meta, workloads))
        combined = standing.combine(folders)
        row = combined["rows"]["startup.window_ms"]
        self.assertEqual(row["claim"]["label"], "1st")
        self.assertEqual([s["peer"] for s in row["sessions"]], ["wezterm"] * 3)
        self.assertIn("| startup window_ms | 152.00 | 1st |", combined["markdown"])

    def test_ab_sessions_combine_to_a_verdict(self) -> None:
        folders = []
        for day in (1, 2):
            meta = {"date": f"2026-10-0{day}", "started": f"2026-10-0{day}T01:00:00", "refusals": [],
                    "bare": False, "complete": True, "label": f"ab{day}", "mode": "ab",
                    "rounds": {"startup": 4}, "warmup": 0}
            workloads = {"startup": {"kettle-a": [{"child_ms": 200.0 + i, "window_ms": 150.0} for i in range(4)],
                                     "kettle-b": [{"child_ms": 180.0 + i, "window_ms": 150.0} for i in range(4)]}}
            folders.append(write_session(self.root, f"ab{day}", meta, workloads))
        combined = standing.combine(folders)
        row = combined["rows"]["startup.child_ms"]
        self.assertEqual(row["verdict"]["verdict"], "lower")
        self.assertIn("startup child_ms", combined["markdown"])


class Payloads(unittest.TestCase):
    def runner(self, work: Path):
        probe = work / "probe.sh"
        probe.write_text("#!/bin/sh\nexit 2\n")
        probe.chmod(0o755)
        return standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})

    def test_payload_script_is_reused_across_launches(self) -> None:
        # macOS assesses a script the first time it runs (about 120 ms), so
        # a fresh script per launch adds that to every terminal's shell time.
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            runner = self.runner(work)
            for dat in ("a.dat", "b.dat"):
                runner.finish(runner.launch("kettle", 'exec true "$DAT"', 1, params={"DAT": dat}), 1)
            payloads = sorted(work.glob("payload-*.sh"))
            self.assertEqual(len(payloads), 1)
            self.assertIn(". ", payloads[0].read_text())
            self.assertEqual((work / "params").read_text(), "DAT=b.dat\n")

    def test_params_are_shell_quoted(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            runner = self.runner(work)
            runner.finish(runner.launch("kettle", "true", 1, params={"DAT": "/a b/it's.dat"}), 1)
            self.assertEqual((work / "params").read_text(), "DAT='/a b/it'\"'\"'s.dat'\n")

    def test_warmup_rows_are_excluded_from_statistics(self) -> None:
        results = {"context": "t", "workloads": {"startup": {
            "kettle": [{"child_ms": 900.0, "warmup": True}, {"child_ms": 100.0}, {"child_ms": 110.0}],
            "alacritty": [{"child_ms": 950.0, "warmup": True}, {"child_ms": 120.0}, {"child_ms": 130.0}],
        }}}
        text = standing.summarize(results, ["kettle", "alacritty"], ab=False)
        self.assertIn("| child_ms | 105.00 | 125.00 |", text)
        self.assertIn("| child_ms | alacritty | 0.840 | ", text)

    def test_per_kettle_configs_and_b_only_config(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            standing.write_configs(work, {"kettle-a": "", "kettle-b": "cursor-blink = false"})
            self.assertNotIn("cursor-blink", (work / "kettle-a.config").read_text())
            self.assertIn("cursor-blink = false\n", (work / "kettle-b.config").read_text())
            self.assertIn("window-width = 120", (work / "kettle-b.config").read_text())
            argv = standing.terminal_argv("kettle-b", work / "p.sh", work, {"kettle-a": "/k", "kettle-b": "/k"})
            self.assertEqual(argv[:3], ["/k", "--config", str(work / "kettle-b.config")])

    def test_variants_are_listed_but_never_ranked(self) -> None:
        results = {"context": "t", "unranked": ["kettle-opaque"], "workloads": {"startup": {
            "kettle": [{"window_ms": 200.0}], "kettle-opaque": [{"window_ms": 100.0}],
            "alacritty": [{"window_ms": 150.0}]}}}
        analysis = standing.analyze(results, ["kettle", "kettle-opaque", "alacritty"], ab=False)
        entry = analysis["startup"]["metrics"]["window_ms"]
        self.assertEqual((entry["best_other"], entry["rank"]), ("alacritty", 2))
        self.assertIn("kettle-opaque", entry["terminals"])

    def test_launch_metadata_is_not_a_metric(self) -> None:
        results = {"context": "t", "workloads": {"startup": {
            "kettle": [{"window_ms": 200.0, "started_ns": 12345678901234, "thermal_state": 1}]}}}
        self.assertNotIn("started_ns", standing.summarize(results, ["kettle"], ab=False))
        self.assertNotIn("thermal_state", standing.summarize(results, ["kettle"], ab=False))


MIB = 2 ** 20


def timeline(points):
    """[(seconds after done, MiB)] as memsample (t_ns, footprint, max) tuples, done at t=100 s."""
    return [(int((100 + t) * 1e9), int(mib * MIB), int(mib * MIB)) for t, mib in points]


class FloodTimeline(unittest.TestCase):
    DONE = int(100e9)

    def test_flood_row_reports_peak_done3_done20(self) -> None:
        # A blink plateau until 10 s after done, then the driver releases its
        # pools about a second after the last frame.
        points = [(-2, 200)] + [(t / 10, 360) for t in range(0, 110)] + [(11.2, 60)] + \
                 [(t / 10, 58) for t in range(113, 225)]
        row = standing.flood_row(timeline(points), self.DONE, (3, 20))
        self.assertEqual(row["peak_mib"], 360)
        self.assertEqual(row["done3_mib"], 360)
        self.assertEqual(row["done20_mib"], 58)
        self.assertAlmostEqual(row["release_s"], 11.2, places=3)
        self.assertTrue(row["done_ok"])

    def test_flood_offset_takes_first_sample_at_or_after_offset(self) -> None:
        row = standing.flood_row(timeline([(2.95, 100), (3.05, 90), (19.9, 80), (20.0, 70)]), self.DONE, (3, 20))
        self.assertEqual((row["done3_mib"], row["done20_mib"]), (90, 70))

    def test_peak_includes_the_lifetime_maximum(self) -> None:
        samples = [(int(100e9), 50 * MIB, 400 * MIB), (int(125e9), 40 * MIB, 400 * MIB)]
        self.assertEqual(standing.flood_row(samples, self.DONE, (3, 20))["peak_mib"], 400)

    def test_a_flood_that_never_finished_is_an_error(self) -> None:
        self.assertEqual(standing.flood_row(timeline([(1, 50)]), None, (3, 20)),
                         {"error": "the flood never finished"})

    def test_a_timeline_that_ends_before_an_offset_is_an_error(self) -> None:
        row = standing.flood_row(timeline([(0, 50), (5, 40)]), self.DONE, (3, 20))
        self.assertEqual(row["error"], "no sample at done+20 s")

    def test_every_terminal_gets_the_same_flood_columns(self) -> None:
        rows = {name: [standing.flood_row(timeline([(0, mib), (3, mib), (20, mib)]), self.DONE, (3, 20))]
                for name, mib in (("kettle", 300), ("alacritty", 70))}
        text = standing.summarize({"context": "t", "workloads": {"flood-memory": rows}}, ["kettle", "alacritty"], False)
        for column in ("peak_mib", "done3_mib", "done20_mib"):
            self.assertRegex(text, rf"\| {column} \| [0-9.]+ \| [0-9.]+ \|")
        self.assertNotIn("| release_s |", text.split("| metric | best other")[-1])

    def test_flood_columns_follow_the_chosen_offsets(self) -> None:
        self.assertEqual(standing.flood_metrics([3.0, 20.0]), ("peak_mib", "done3_mib", "done20_mib"))
        self.assertEqual(standing.flood_metrics([5.0, 30.0]), ("peak_mib", "done5_mib", "done30_mib"))
        rows = {"kettle": [standing.flood_row(timeline([(0, 300), (5, 300), (30, 60)]), self.DONE, (5, 30))]}
        results = {"context": "t", "meta": {"flood_offsets": [5.0, 30.0]}, "workloads": {"flood-memory": rows}}
        text = standing.summarize(results, ["kettle"], False)
        self.assertIn("| done5_mib |", text)
        self.assertIn("| done30_mib |", text)
        meta = {"rounds": {"flood-memory": 1}, "flood_offsets": [5.0, 30.0]}
        self.assertTrue(standing.rounds_complete(results, meta))

    def test_a_done_stamp_is_read_only_once_written(self) -> None:
        import tempfile
        import threading

        with tempfile.TemporaryDirectory() as tmp:
            done = Path(tmp) / "done"
            # Mid-write: the line so far, with no size fields yet.
            done.write_text("1234")
            threading.Timer(0.2, lambda: done.write_text("123456789 120 36\n")).start()
            self.assertEqual(standing.read_stamp(done, timeout=2), 123456789)
            empty = Path(tmp) / "never"
            empty.write_text("")
            self.assertIsNone(standing.read_stamp(empty, timeout=0.3))

    def test_the_sampler_writes_a_file_and_stops_before_the_terminal(self) -> None:
        import inspect

        source = inspect.getsource(standing.Runner.flood_memory)
        self.assertNotIn("subprocess.PIPE", source, "an undrained pipe stalls memsample")
        self.assertLess(source.index("self.end_sampler(loop)\n        if not self.stop(process, 60)"),
                        len(source), "the sampler must stop before the terminal is reaped")

    def test_footprint_graphics_categories(self) -> None:
        data = {"processes": [{"categories": {
            "Owned physical footprint (unmapped) (graphics)": {"dirty": 38 * 8 * MIB, "regions": 38},
            "IOAccelerator (graphics)": {"dirty": 4 * MIB, "regions": 12},
            "__DATA /usr/lib/libSystem.B.dylib": {"dirty": MIB, "regions": 1}}}]}
        self.assertEqual(standing.footprint_graphics(data), {
            "Owned physical footprint (unmapped) (graphics)": {"dirty_mib": 304.0, "regions": 38},
            "IOAccelerator (graphics)": {"dirty_mib": 4.0, "regions": 12}})


class IdleFocus(unittest.TestCase):
    def test_idle_round_needs_frontmost_at_every_check(self) -> None:
        first = {"cpu_ns": 0, "wakeups": 0, "footprint": 30 * MIB, "rss": 90 * MIB}
        second = {"cpu_ns": 30_000_000, "wakeups": 15, "footprint": 31 * MIB, "rss": 91 * MIB}
        row = standing.idle_row(first, second, 30.0, [True, False, True])
        self.assertFalse(row["frontmost"])
        self.assertEqual(row["frontmost_checks"], [True, False, True])
        self.assertAlmostEqual(row["cpu_percent"], 0.1)
        self.assertAlmostEqual(row["wakeups_per_second"], 0.5)
        self.assertTrue(standing.idle_row(first, second, 30.0, [True, True, True])["frontmost"])


class MemsampleLoop(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang"), "needs macOS and clang")
    def test_memsample_loop_mode_samples_own_process(self) -> None:
        import json
        import os
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "memsample"
            subprocess.run(["clang", "-O", "-o", str(binary), str(HERE / "macos-standing" / "memsample.c")],
                           check=True)
            out = subprocess.run([str(binary), str(os.getpid()), "20", "3"], capture_output=True, text=True,
                                 check=True).stdout
            lines = [json.loads(line) for line in out.splitlines()]
            self.assertEqual(len(lines), 3)
            self.assertTrue(lines[0]["t_ns"] < lines[1]["t_ns"] < lines[2]["t_ns"])
            self.assertEqual(lines[0]["pid"], os.getpid())
            one = json.loads(subprocess.run([str(binary), str(os.getpid())], capture_output=True, text=True).stdout)
            self.assertNotIn("t_ns", one, "one-shot output keeps its old shape")


class StartupPhases(unittest.TestCase):
    FIXTURE = (HERE / "macos-standing" / "startup-phases.fixture").read_text()

    def test_phase_lines_parse_relative_to_launch(self) -> None:
        # The same fixture crates/kettle-ui/src/startup_trace.rs tests against.
        phases = standing.parse_phases(self.FIXTURE, 900_000_000)
        self.assertEqual(phases["phase_main_ms"], 100.0)
        self.assertEqual(phases["phase_first_frame_ms"], 200.0)
        self.assertEqual(phases["startup_path"], "resumed_early")
        self.assertEqual(len([k for k in phases if k.startswith("phase_")]), 11)

    def test_finish_reads_the_stamps_kettle_printed(self) -> None:
        import json
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "probe.sh"
            probe.write_text("#!/bin/sh\nexit 0\n")
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})
            (work / "launch.json").write_text(json.dumps({"window_ms": 150.0, "started_ns": 900_000_000}))
            (work / "terminal.stderr").write_text(self.FIXTURE)
            process = __import__("subprocess").Popen(["/usr/bin/true"])
            result = runner.finish(process, 1)
            self.assertEqual(result["phase_first_frame_ms"], 200.0)
            self.assertEqual(result["startup_path"], "resumed_early")

    def launched_env(self, stamped: set, name: str, startup: bool = True, ambient: str = "") -> str:
        """RUST_LOG as the launch probe saw it for entry `name`, launched by
        a startup round or by another workload, with `ambient` as the
        harness's own RUST_LOG."""
        import os
        import tempfile
        from unittest import mock

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "probe.sh"
            probe.write_text(f'#!/bin/sh\nprintf "%s" "${{RUST_LOG-unset}}" > "{work}/env"\nexit 2\n')
            probe.chmod(0o755)
            kettle = {"kettle-a": "/bin/true", "kettle-b": "/bin/true"}
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, kettle)
            runner.phases = stamped
            env = dict(os.environ, RUST_LOG=ambient) if ambient else dict(os.environ)
            if not ambient:
                env.pop("RUST_LOG", None)
            with mock.patch.dict(os.environ, env, clear=True):
                if startup:
                    runner.startup(name)
                else:
                    runner.finish(runner.launch(name, "true", 1), 1)
            return (work / "env").read_text()

    def test_only_the_stamped_entries_get_the_filter(self) -> None:
        self.assertEqual(self.launched_env({"kettle-b"}, "kettle-b"), "warn,kettle::startup=info")
        self.assertNotIn("kettle::startup", self.launched_env({"kettle-b"}, "kettle-a"))
        self.assertNotIn("kettle::startup", self.launched_env(set(), "kettle-b"))

    def test_only_startup_rounds_are_stamped(self) -> None:
        # Idle, flood and vtebench rounds of a stamped entry run as shipped.
        self.assertEqual(self.launched_env({"kettle-b"}, "kettle-b", startup=False), "unset")

    def test_the_harness_log_filter_never_reaches_a_terminal(self) -> None:
        # With RUST_LOG=info in the shell, the stamps-off side would print
        # stamps too, and every terminal would log more than it ships doing.
        self.assertEqual(self.launched_env({"kettle-b"}, "kettle-a", ambient="info"), "unset")
        self.assertEqual(self.launched_env(set(), "alacritty", ambient="debug"), "unset")
        self.assertEqual(self.launched_env({"kettle-b"}, "kettle-b", ambient="debug"), "warn,kettle::startup=info")

    def test_sides_select_which_kettle_entries_are_stamped(self) -> None:
        ab = {"kettle-a": "/k", "kettle-b": "/k"}
        standing_entries = {"kettle": "/k", "kettle-opaque": "/k"}
        self.assertEqual(standing.stamped_entries("all", ab), {"kettle-a", "kettle-b"})
        self.assertEqual(standing.stamped_entries("b", ab), {"kettle-b"})
        self.assertEqual(standing.stamped_entries("all", standing_entries), {"kettle", "kettle-opaque"})
        self.assertEqual(standing.stamped_entries(None, ab), set())
        with self.assertRaises(ValueError):
            standing.stamped_entries("b", standing_entries)

    def test_a_stamped_session_never_counts(self) -> None:
        # The filter and the log lines change what Kettle does at startup.
        clean = {"refusals": [], "bare": False, "complete": True}
        self.assertTrue(standing.session_countable({**clean, "startup_phases": None}))
        for sides in ("all", "b"):
            self.assertFalse(standing.session_countable({**clean, "startup_phases": sides}))
        self.assertIn("startup_phases", standing.SESSION_KEYS)

    def test_one_sided_phases_leave_the_ab_comparison_empty(self) -> None:
        results = {"context": "t", "workloads": {"startup": {
            "kettle-a": [{"window_ms": 200.0, "child_ms": 250.0}, {"window_ms": 201.0, "child_ms": 251.0}],
            "kettle-b": [{"window_ms": 202.0, "child_ms": 252.0, "phase_resumed_ms": 40.0},
                         {"window_ms": 203.0, "child_ms": 253.0, "phase_resumed_ms": 41.0}]}}}
        metrics = standing.analyze(results, ["kettle-a", "kettle-b"], ab=True)["startup"]["metrics"]
        self.assertEqual(metrics["phase_resumed_ms"]["ab_diff"], {})
        self.assertTrue(metrics["child_ms"]["ab_diff"])
        standing.summarize(results, ["kettle-a", "kettle-b"], ab=True)

    def test_a_round_without_stamps_adds_no_phase_keys(self) -> None:
        self.assertEqual(standing.parse_phases("some other log line\n", 1), {})
        self.assertEqual(standing.parse_phases(self.FIXTURE, None), {})

    def test_phase_times_are_startup_metrics(self) -> None:
        results = {"context": "t", "workloads": {"startup": {
            "kettle-a": [{"window_ms": 200.0, "child_ms": 250.0, "phase_resumed_ms": 60.0}],
            "kettle-b": [{"window_ms": 190.0, "child_ms": 230.0, "phase_resumed_ms": 40.0}]}}}
        text = standing.summarize(results, ["kettle-a", "kettle-b"], ab=True)
        self.assertIn("| phase_resumed_ms | 60.00 | 40.00 |", text)
        self.assertIn("phase_resumed_ms: B/A 0.667", text)

    def test_phases_are_listed_in_the_order_they_happen(self) -> None:
        results = {"context": "t", "workloads": {"startup": {"kettle": [
            {"window_ms": 190.0, "child_ms": 120.0, "phase_main_ms": 10.0, "phase_app_built_ms": 64.0,
             "phase_first_frame_ms": 215.0, "phase_gpu_ready_ms": 158.0}]}}}
        text = standing.summarize(results, ["kettle"], ab=False)
        rows = [line.split("|")[1].strip() for line in text.splitlines() if line.startswith("| phase_")]
        self.assertEqual(rows, ["phase_main_ms", "phase_app_built_ms", "phase_gpu_ready_ms", "phase_first_frame_ms"])

    def test_paired_difference_is_the_mean_round_difference(self) -> None:
        stats = standing.paired_difference([200.0, 210.0, 205.0, 220.0], [180.0, 195.0, 185.0, 200.0])
        self.assertEqual((stats["diff"], stats["n"]), (-18.75, 4))
        self.assertLess(stats["low"], -18.75)
        self.assertGreater(stats["high"], -18.75)
        # Student t with 3 degrees of freedom: 3.182446 x sd / sqrt(n).
        self.assertAlmostEqual(stats["high"] - stats["diff"], 3.182446 * 2.5 / 2, places=5)

    def test_ab_startup_lines_give_the_difference_in_ms(self) -> None:
        results = {"context": "t", "workloads": {"startup": {
            "kettle-a": [{"window_ms": 200.0, "child_ms": 250.0}, {"window_ms": 210.0, "child_ms": 260.0}],
            "kettle-b": [{"window_ms": 200.0, "child_ms": 230.0}, {"window_ms": 210.0, "child_ms": 240.0}]}}}
        text = standing.summarize(results, ["kettle-a", "kettle-b"], ab=True)
        self.assertIn("child_ms: B/A 0.922", text)
        self.assertIn("B-A -20.0 ms", text)

    def test_standing_startup_rows_give_kettle_minus_the_best_peer(self) -> None:
        results = {"context": "t", "workloads": {"startup": {
            "kettle": [{"window_ms": 170.0, "child_ms": 200.0}, {"window_ms": 172.0, "child_ms": 204.0}],
            "wezterm": [{"window_ms": 160.0, "child_ms": 230.0}, {"window_ms": 162.0, "child_ms": 232.0}]}}}
        text = standing.summarize(results, ["kettle", "wezterm"], ab=False)
        self.assertIn("| child_ms | wezterm | 0.874 |", text)
        self.assertIn("-29.0 ms", text)


KEYBLOCK_SHA256 = "57720778868c53a6aa982ebc5cc720ed7cd3092bd009b59ec0efb021d2333150"


def latency_run(samples, censored=0, refresh=60, keys=None, **extra):
    """A latency round as Runner.latency records it."""
    return {"samples_ms": list(samples), "censored": censored, "keys": keys if keys is not None else len(samples) + censored,
            "mean_ms": sum(samples) / len(samples) if samples else None, "refresh_hz": refresh, **extra}


def latency_meta(day, rounds=2, **extra):
    return {"date": f"2026-10-0{day}", "started": f"2026-10-0{day}T01:00:00", "refusals": [], "bare": False,
            "complete": True, "label": f"s{day}", "mode": "standing", "rounds": {"latency": rounds}, "warmup": 0,
            "latency": {"keys": 4, "warmup": 1, "censor_ms": 500, "inject": "hid", "signed": "ad hoc"}, **extra}


class Latency(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_the_default_workloads_never_post_keys(self) -> None:
        self.assertEqual(standing.WORKLOADS, ("startup", "idle", "flood-memory", "vtebench"))
        self.assertNotIn("latency", standing.WORKLOADS)
        self.assertEqual(standing.OPT_IN_WORKLOADS, ("latency", "latency-cursor", "output-memory", "blink-window"))
        import inspect

        source = inspect.getsource(standing.standing_main)
        self.assertIn('parser.add_argument("--workloads", default=",".join(WORKLOADS))', inspect.getsource(standing))
        self.assertIn('build_probes(tools, latency=bool(set(workloads) & {"latency", "latency-cursor"})', source)

    def test_keyblock_is_pinned(self) -> None:
        # Pin the source artifact and execute the block payload's byte contract.
        TypingMemory().test_payload_initialization_and_frames()
        digest = standing.file_sha256(HERE / "macos-standing" / "keyblock.c")
        self.assertEqual(digest, KEYBLOCK_SHA256, "keyblock.c changed: re-pin it and start a new session set")

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang"), "needs macOS and clang")
    def test_keyblock_toggles_on_each_byte_and_logs_in_order(self) -> None:
        if SNAPSHOT_LAYOUT:
            self.skipTest('controlling-terminal native test needs an unrestricted runner')
        import os
        import pty
        import select
        import subprocess
        import time as clock

        binary = self.root / "keyblock"
        subprocess.run(["clang", "-O", "-o", str(binary), str(HERE / "macos-standing" / "keyblock.c")], check=True)
        log = self.root / "log"
        pid, fd = pty.fork()
        if pid == 0:
            os.execv(str(binary), [str(binary), str(log)])

        def read_until(marker: bytes, timeout: float = 5.0) -> bytes:
            data = b""
            deadline = clock.monotonic() + timeout
            while marker not in data and clock.monotonic() < deadline:
                if select.select([fd], [], [], 0.1)[0]:
                    data += os.read(fd, 4096)
            return data

        try:
            init = read_until(b"\x1b[0m", 5)
            self.assertIn(b"\x1b[?25l\x1b[2 q", init, "hides the cursor and makes it a steady block")
            self.assertIn(b"\x1b[27m", init, "starts with the block off")
            for expected in (b"\x1b[7m", b"\x1b[27m", b"\x1b[7m"):
                os.write(fd, b"j")
                frame = read_until(b"\x1b[0m")
                self.assertIn(expected, frame)
                self.assertEqual(frame.count(b"\x1b["), 9, "one write: 4 moves, 4 attributes, one reset")
        finally:
            os.close(fd)
            os.waitpid(pid, 0)
        records = standing.read_keyblock_log(log)
        self.assertEqual(sorted(records), [1, 2, 3])
        for seq in (1, 2, 3):
            nbytes, t_read, t_written = records[seq]
            self.assertEqual(nbytes, 1)
            self.assertLessEqual(t_read, t_written)
        self.assertLess(records[1][2], records[2][1])

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang"), "needs macOS and clang")
    def test_keyblock_cursor_mode_waits_on_a_real_terminal(self) -> None:
        # macOS poll() reports POLLNVAL for a /dev/tty descriptor; cursor mode
        # once polled it and exited at once in every terminal, so no live
        # cursor round could run.
        if SNAPSHOT_LAYOUT:
            self.skipTest('controlling-terminal native test needs an unrestricted runner')
        import os
        import pty
        import select
        import subprocess
        import time as clock

        binary = self.root / "keyblock"
        subprocess.run(["clang", "-O", "-o", str(binary), str(HERE / "macos-standing" / "keyblock.c")], check=True)
        log, control, ack = self.root / "log", self.root / "control", self.root / "ack"
        os.mkfifo(control, 0o600)
        pid, fd = pty.fork()
        if pid == 0:
            os.execv(str(binary), [str(binary), str(log), "cursor", str(control), str(ack), "20000"])

        def read_until(marker: bytes, timeout: float = 5.0) -> bytes:
            data = b""
            deadline = clock.monotonic() + timeout
            while marker not in data and clock.monotonic() < deadline:
                if select.select([fd], [], [], 0.1)[0]:
                    try:
                        data += os.read(fd, 4096)
                    except OSError:
                        break
            return data

        try:
            self.assertIn(b"\x1b[?25l\x1b[2 q", read_until(b"\x1b[0m"))
            clock.sleep(1.0)
            self.assertEqual(os.waitpid(pid, os.WNOHANG), (0, 0), "cursor mode must keep waiting")
            for _ in range(6):
                os.write(fd, b"j")
                self.assertIn(b"\x1b[0m", read_until(b"\x1b[0m"), "each calibration flip draws")
            # Nonblocking: a payload that has gone has no reader, and this
            # fails at once instead of waiting forever for one.
            request = os.open(control, os.O_WRONLY | os.O_NONBLOCK)
            try:
                os.write(request, b"ENABLE\n")
            finally:
                os.close(request)
            self.assertIn(b"\x1b[?25h\x1b[1 q\x1b[2;3H", read_until(b"\x1b[2;3H"))
            deadline = clock.monotonic() + 5
            while not ack.exists() and clock.monotonic() < deadline:
                clock.sleep(0.02)
            self.assertRegex(ack.read_text(), r"^ENABLED [0-9]+\n$")
            os.write(fd, b"j")
            self.assertIn(b"\x1b[2;3H", read_until(b"\x1b[2;3H"), "a measured flip parks the visible cursor")
            self.assertEqual(os.waitpid(pid, os.WNOHANG), (0, 0))
        finally:
            os.close(fd)
            os.waitpid(pid, 0)
        records = standing.read_keyblock_log(log)
        self.assertEqual(sorted(records), list(range(1, 8)))

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang"), "needs macOS and clang")
    def test_keyblock_cursor_mode_refuses_a_stdin_that_is_not_its_terminal(self) -> None:
        # It waits on standard input, so that must be the terminal it draws on.
        if SNAPSHOT_LAYOUT:
            self.skipTest('controlling-terminal native test needs an unrestricted runner')
        import os
        import pty
        import subprocess
        import time as clock

        binary = self.root / "keyblock"
        subprocess.run(["clang", "-O", "-o", str(binary), str(HERE / "macos-standing" / "keyblock.c")], check=True)
        control = self.root / "control"
        os.mkfifo(control, 0o600)
        pid, fd = pty.fork()
        if pid == 0:
            os.dup2(os.open(os.devnull, os.O_RDONLY), 0)
            os.execv(str(binary), [str(binary), str(self.root / "log"), "cursor", str(control),
                                   str(self.root / "ack"), "20000"])
        try:
            deadline = clock.monotonic() + 5
            status = (0, 0)
            while status == (0, 0) and clock.monotonic() < deadline:
                clock.sleep(0.02)
                status = os.waitpid(pid, os.WNOHANG)
            self.assertNotEqual(status, (0, 0), "a non-terminal stdin must refuse at once")
            self.assertEqual(os.waitstatus_to_exitcode(status[1]), 1)
        finally:
            os.close(fd)
            if status == (0, 0):
                os.kill(pid, 9)
                os.waitpid(pid, 0)

    def test_a_torn_log_record_is_dropped(self) -> None:
        path = self.root / "log"
        path.write_bytes(standing.KEYBLOCK_RECORD.pack(1, 1, 10, 20) + b"\x00" * 7)
        self.assertEqual(standing.read_keyblock_log(path), {1: (1, 10, 20)})
        self.assertEqual(standing.read_keyblock_log(self.root / "missing"), {})

    def test_every_terminal_gets_the_same_payload(self) -> None:
        bodies = {}

        class Recorder(standing.Runner):
            def launch(self, name, body, timeout, params=None, phases=False, argv=None):
                bodies[name] = (body, argv)
                return None

            def wait_for(self, path, timeout):
                return False

            def stop(self, process, timeout):
                return True

        runner = Recorder({"keyblock": Path("/k"), "latency-floor": Path("/f")}, self.root, {"kettle": "/x"})
        options = {"keys": 1, "warmup": 0, "censor_ms": 500, "inject": "hid"}
        for name in ("kettle", "kettle-opaque", "alacritty", "floor-ca"):
            self.assertIn("error", runner.latency(name, options, 7))
        self.assertEqual(len({bodies[name] for name in ("kettle", "kettle-opaque", "alacritty")}), 1)
        self.assertIsNone(bodies["kettle"][1])
        self.assertEqual(bodies["floor-ca"][1][:2], ["/f", "ca"], "a floor is its own window")

    def test_the_opaque_variant_and_floors_join_standing_sessions_only(self) -> None:
        self.assertIn("background-opacity = 1", standing.KETTLE_OPAQUE)
        self.assertIn("window-blur = false", standing.KETTLE_OPAQUE)
        self.assertEqual(standing.latency_entries(["kettle", "alacritty"], False, True, ["ca", "metal-sync"]),
                         ["kettle", "alacritty", "kettle-opaque", "floor-ca", "floor-metal-sync"])
        self.assertEqual(standing.latency_entries(["kettle", "alacritty"], False, False, []), ["kettle", "alacritty"])
        self.assertEqual(standing.latency_entries(["kettle-a", "kettle-b"], True, True, ["ca"]), ["kettle-a", "kettle-b"])

    def test_the_probe_plist_is_an_agent_with_the_granted_bundle_id(self) -> None:
        import plistlib

        info = plistlib.loads(standing.latency_probe_plist())
        self.assertEqual(info["CFBundleIdentifier"], "org.kettle.terminal.latency-probe")
        self.assertIs(info["LSUIElement"], True)
        self.assertEqual(info["CFBundleExecutable"], "latency-probe")

    def test_a_row_splits_each_key_into_input_and_output_halves(self) -> None:
        probe = {"samples": [
            {"seq": 7, "warmup": True, "t_post": 0, "display": 30_000_000, "arrival": 20_000_000, "censored": False},
            {"seq": 8, "warmup": False, "t_post": 100_000_000, "display": 130_000_000, "arrival": 119_000_000,
             "censored": False, "mixed": 1, "reverted": False},
            {"seq": 9, "warmup": False, "t_post": 200_000_000, "display": None, "arrival": None, "censored": True}],
            "display": {"refresh_hz": 60}, "activation": "already"}
        records = {seq: (1, 0, 0) for seq in range(1, 7)}
        records.update({7: (1, 4_000_000, 5_000_000), 8: (1, 104_000_000, 105_000_000), 9: (1, 204_000_000, 205_000_000)})
        row = standing.latency_row(probe, records, 500)
        self.assertEqual(row["samples_ms"], [30.0])
        self.assertEqual((row["censored"], row["keys"], row["mixed_frames"]), (1, 2, 1))
        self.assertEqual((row["input_ms"], row["output_ms"]), (4.0, 25.0))
        self.assertEqual((row["inputs_ms"], row["outputs_ms"]), ([4.0], [25.0]))
        self.assertEqual(row["mean_ms"], 265.0, "the censored key counts at the bound")
        self.assertEqual(row["p99_ms"], standing.percentile([30.0, 500.0], 0.99))
        self.assertEqual((row["leads_ms"], row["lead_out_of_range"]), ([11.0], 0))
        self.assertEqual((row["seq_mismatch"], row["refresh_hz"]), (0, 60))

    def test_sequence_mismatches_are_flagged(self) -> None:
        probe = {"samples": [{"seq": 7, "warmup": False, "t_post": 0, "display": 1_000_000, "censored": False}]}
        records = {seq: (1, 0, 0) for seq in range(1, 7)}
        records[7] = (2, 0, 0)  # two keys arrived in one read
        self.assertEqual(standing.latency_row(probe, records, 500)["seq_mismatch"], 1)
        records[7] = (1, 0, 0)
        records[8] = (1, 0, 0)  # a read nobody posted
        self.assertEqual(standing.latency_row(probe, records, 500)["seq_mismatch"], 1)
        del records[3]  # a calibration key that never arrived
        row = standing.latency_row(probe, records, 500)
        self.assertEqual(row["seq_mismatch"], 2)
        # The join of samples to records may be shifted: the round fails.
        self.assertFalse(standing.round_ok("latency", row))
        self.assertIsNone(standing.latency_keys(row, 500))

    def test_the_latency_ratio_accounts_for_the_baselines_spread(self) -> None:
        # Base rounds of 10 or 30 ms, test always 10 ms slower: the ratio of
        # means is uncertain even though the difference is exact.
        stats = standing.cluster_compare([[10.0]] * 4 + [[30.0]], [[20.0]] * 4 + [[40.0]])
        self.assertEqual((stats["diff"], stats["diff_low"], stats["diff_high"]), (10.0, 10.0, 10.0))
        self.assertLess(stats["low"], stats["ratio"])
        self.assertLess(stats["ratio"], stats["high"])
        self.assertLess(stats["low"], 1.5)
        self.assertGreater(stats["high"], 1.5)

    def test_the_latency_comparison_uses_round_means_and_counts_rounds(self) -> None:
        base = [[20.0 + (i + j) % 4 for j in range(30)] for i in range(6)]
        test = [[15.0 + (i + j) % 4 for j in range(30)] for i in range(6)]
        stats = standing.cluster_compare(base, test)
        self.assertEqual(stats, standing.cluster_compare(base, test))
        self.assertLess(stats["high"], 1.0)
        self.assertLess(stats["diff_high"], 0.0)
        self.assertAlmostEqual(stats["diff"], -5.0)
        self.assertEqual((stats["wins"], stats["n"]), (6, 6))
        # Two launches far apart: the keys look precise, the launches do not.
        wide = standing.cluster_compare([[10.0] * 20, [30.0] * 20] * 3, [[11.0] * 20, [29.0] * 20] * 3)
        self.assertLess(wide["low"], 1.0)
        self.assertGreater(wide["high"], 1.0)
        self.assertEqual(standing.cluster_compare([[1.0], None], [None, [2.0]]), {})

    def test_censored_keys_count_at_the_bound(self) -> None:
        self.assertEqual(standing.latency_keys(latency_run([10.0], censored=2), 500), [10.0, 500.0, 500.0])
        self.assertIsNone(standing.latency_keys({"error": "focus changed"}, 500))

    def test_ties_unranked_rows_and_floors(self) -> None:
        rounds = 10
        close = [latency_run([30.0 + (i + j) % 3 for j in range(20)]) for i in range(rounds)]
        # The same keys in another order: a tie.
        also_close = [latency_run([30.0 + (i + j + 1) % 3 for j in range(20)]) for i in range(rounds)]
        censored = [latency_run([20.0] * 19, censored=1) for _ in range(rounds)]
        floor = [latency_run([5.0] * 20) for _ in range(rounds)]
        results = {"context": "t", "unranked": ["floor-ca"],
                   "meta": {"rounds": {"latency": rounds}, "latency": {"censor_ms": 500}},
                   "workloads": {"latency": {"kettle": close, "wezterm": also_close, "kitty": censored,
                                             "floor-ca": floor}}}
        info = standing.analyze(results, ["kettle", "wezterm", "kitty"], False)["latency"]
        entry = info["metrics"]["mean_ms"]
        self.assertEqual(entry["best_other"], "wezterm", "the floor and the 5%-censored row are never ranked")
        self.assertEqual(entry["rank"], 1)
        self.assertLess(entry["vs_best"]["diff_low"], 0.0)
        self.assertGreater(entry["vs_best"]["diff_high"], 0.0, "within noise: a tie, not an order")
        self.assertFalse(info["standing"]["kitty"]["ranked"])
        self.assertEqual(info["entries"], ["kettle", "wezterm", "kitty", "floor-ca"])
        text = standing.summarize(results, ["kettle", "wezterm", "kitty"], False)
        self.assertIn("| floor-ca | 5.0 (5.0-5.0) |", text)
        self.assertIn("| kitty | ", text)
        self.assertIn(" unranked |", text)

    def test_a_terminal_that_lost_three_of_ten_rounds_is_not_measured(self) -> None:
        runs = [latency_run([30.0] * 5) for _ in range(7)] + [{"error": "latency probe: focus changed"}] * 3
        standing_row = standing.latency_standing(runs, 500, 10)
        self.assertFalse(standing_row["measured"])
        self.assertTrue(standing.latency_standing(runs[:9], 500, 10)["measured"])

    def test_latency_counts_on_its_own_and_per_entry(self) -> None:
        def session(name, day, kettle_rows, startup_rows):
            meta = latency_meta(day, rounds=10)
            meta["rounds"] = {"latency": 10, "startup": 2}
            workloads = {"startup": {"kettle": startup_rows, "wezterm": [{"window_ms": 170.0, "child_ms": 1.0}] * 2},
                         "latency": {"kettle": kettle_rows, "wezterm": [latency_run([31.0] * 4)] * 10}}
            return write_session(self.root, name, meta, workloads)

        good = [{"window_ms": 150.0, "child_ms": 1.0}] * 2
        lost = {"error": "latency probe: focus changed"}
        two_lost = [latency_run([30.0] * 4)] * 8 + [lost] * 2
        three_lost = [latency_run([30.0] * 4)] * 7 + [lost] * 3
        folders = [session("a", 1, two_lost, good), session("b", 2, three_lost, good),
                   session("c", 3, [latency_run([30.0] * 4)] * 10,
                           [{"window_ms": 150.0, "child_ms": 1.0}, {"error": "no window"}])]
        combined = standing.combine(folders)
        self.assertEqual([s["countable"] for s in combined["sessions"]], [True, True, False],
                         "a lost startup round costs the session its default rows only")
        latency = combined["rows"]["latency.mean_ms"]
        self.assertEqual([s["countable"] for s in latency["per_session"]], [True, True, True],
                         "lost latency rounds never void the other entries")
        self.assertEqual([s["label"] for s in latency["sessions"]], ["s1", "s3"],
                         "Kettle is compared with 2 of 10 rounds lost, not with 3")
        loaded = standing.load_session(folders[1])
        info = standing.analyze(loaded["results"], loaded["names"], False)["latency"]
        self.assertFalse(info["standing"]["kettle"]["measured"])
        self.assertTrue(info["standing"]["wezterm"]["measured"])

    def test_a_latency_ab_needs_both_sides_ranked(self) -> None:
        clean = [latency_run([30.0] * 99, keys=99) for _ in range(5)]
        censored = [latency_run([29.0] * 97, censored=2, keys=99) for _ in range(5)]
        results = {"context": "t", "meta": {"rounds": {"latency": 5}, "latency": {"censor_ms": 500}},
                   "workloads": {"latency": {"kettle-a": clean, "kettle-b": censored}}}
        entry = standing.analyze(results, ["kettle-a", "kettle-b"], True)["latency"]["metrics"]["mean_ms"]
        self.assertNotIn("ab", entry, "2 % censored on B: no verdict")
        results["workloads"]["latency"]["kettle-b"] = [latency_run([29.0] * 99, keys=99) for _ in range(5)]
        entry = standing.analyze(results, ["kettle-a", "kettle-b"], True)["latency"]["metrics"]["mean_ms"]
        self.assertIn("ab", entry)

    def test_the_latency_gate_is_in_ms(self) -> None:
        gate = standing.latency_aa_gate({"diff": -0.16, "diff_low": -0.93, "diff_high": 0.60})
        self.assertEqual((gate["contains_one"], gate["gate_ms"]), (True, 1.0))
        self.assertAlmostEqual(standing.latency_aa_gate({"diff": 0.7, "diff_low": 0.1, "diff_high": 1.3})["gate_ms"], 1.4)

        def sessions(diff, low, high):
            return [{"label": f"s{d}", "date": f"2026-10-0{d}", "countable": True, "ratio": 0.98,
                     "diff": diff, "diff_low": low, "diff_high": high} for d in (1, 2)]

        # 0.6 ms, well resolved, is a 6 % ratio at 10 ms: the ratio gate would
        # pass it; the ms gate does not.
        self.assertEqual(standing.latency_ab_verdict(sessions(-0.6, -0.8, -0.4), 1.0)["verdict"], "no change")
        self.assertEqual(standing.latency_ab_verdict(sessions(-1.5, -2.0, -1.0), 1.0)["verdict"], "lower")
        self.assertEqual(standing.latency_ab_verdict(sessions(1.5, 1.0, 2.0), 1.0)["verdict"], "higher")
        self.assertTrue(standing.latency_ab_verdict(sessions(0.2, -0.5, 0.9), 1.0)["no_regression"])
        self.assertFalse(standing.latency_ab_verdict(sessions(0.6, 0.0, 1.2), 1.0)["no_regression"])

    def test_session_percentiles_come_from_every_key(self) -> None:
        # 3 of 200 keys censored: p99 is the bound. The median of per-round
        # p99s would hide it.
        rounds = [latency_run([20.0] * 98, censored=2), latency_run([20.0] * 99, censored=1)]
        results = {"context": "t", "meta": {"rounds": {"latency": 2}, "latency": {"censor_ms": 500}},
                   "workloads": {"latency": {"kettle": rounds}}}
        metrics = standing.analyze(results, ["kettle"], False)["latency"]["metrics"]
        self.assertEqual(metrics["p99_ms"]["terminals"]["kettle"]["estimate"], 500.0)
        self.assertEqual(metrics["median_ms"]["terminals"]["kettle"]["estimate"], 20.0)

    def test_the_harness_waits_past_the_probe_deadline(self) -> None:
        CursorLatency().test_quiet_gaps_budget_and_selection()
        CursorLatency().test_handshake_hidden_calibration_and_guard_order()

    def test_keys_outside_the_arrival_window_unrank_a_row(self) -> None:
        probe = {"vsync": {"period_ns": 16_666_667}, "display": {"refresh_hz": 60}}
        self.assertEqual(standing.lead_out_of_range([13.0, -0.5, 40.0, 33.0], probe), 2)
        rounds = [latency_run([30.0] * 100, lead_out_of_range=2) for _ in range(2)]
        self.assertFalse(standing.latency_standing(rounds, 500, 2)["ranked"])
        rounds = [latency_run([30.0] * 100, lead_out_of_range=1) for _ in range(2)]
        self.assertTrue(standing.latency_standing(rounds, 500, 2)["ranked"])

    def test_no_regression_needs_one_session_and_is_printed(self) -> None:
        one = [{"label": "s1", "date": "2026-10-01", "countable": True, "ratio": 1.03,
                "diff": 0.8, "diff_low": 0.4, "diff_high": 1.2}]
        verdict = standing.latency_ab_verdict(one, 1.0)
        self.assertEqual((verdict["verdict"], verdict["no_regression"]), ("insufficient sessions", False))
        results = {"context": "t", "meta": {"rounds": {"latency": 5}, "latency": {"censor_ms": 500},
                                            "workload_countable": {"latency": True}},
                   "workloads": {"latency": {"kettle-a": [latency_run([30.0 + i % 3 for i in range(50)])] * 5,
                                             "kettle-b": [latency_run([33.0 + i % 3 for i in range(50)])] * 5}}}
        self.assertIn("no regression (difference interval tops out at +1 ms or less): NO",
                      standing.summarize(results, ["kettle-a", "kettle-b"], True))
        # A session that cannot count for latency decides nothing.
        results["meta"]["workload_countable"]["latency"] = False
        self.assertIn("not decided, since this session does not count for latency",
                      standing.summarize(results, ["kettle-a", "kettle-b"], True))

    def test_a_not_measured_entry_publishes_nothing(self) -> None:
        runs = [latency_run([30.0] * 4)] * 7 + [{"error": "latency probe: focus changed"}] * 3
        results = {"context": "t", "meta": {"rounds": {"latency": 10}, "latency": {"censor_ms": 500}},
                   "workloads": {"latency": {"kettle": [latency_run([31.0] * 4)] * 10, "wezterm": runs}}}
        metrics = standing.analyze(results, ["kettle", "wezterm"], False)["latency"]["metrics"]
        for metric in standing.LATENCY_METRICS:
            self.assertNotIn("wezterm", metrics[metric]["terminals"], metric)
        self.assertNotIn("vs_best", metrics["mean_ms"])

    def test_latency_flags_are_bounded(self) -> None:
        import contextlib
        import io
        from unittest import mock

        for flag, value, bound in (("--latency-keys", "0", "least"), ("--latency-censor-ms", "-5", "least"),
                                   ("--latency-warmup", "-1", "least"), ("--latency-rounds", "0", "least"),
                                   ("--latency-keys", "1001", "most"), ("--latency-censor-ms", "5001", "most"),
                                   ("--latency-warmup", "201", "most"), ("--rounds", "0", "least"),
                                   ("--rounds", "1001", "most")):
            argv = ["macos-standing.py", "--workloads", "latency", flag, value]
            with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()) as err, \
                    self.assertRaises(SystemExit):
                standing.main()
            self.assertIn(f"{flag} must be at {bound}", err.getvalue())
        # Before any early path acts on them.
        for early in (["--latency-check"], ["--combine", str(self.root / "none")], ["--preflight-only"]):
            argv = ["macos-standing.py", *early, "--latency-keys", "0"]
            with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()) as err, \
                    self.assertRaises(SystemExit):
                standing.main()
            self.assertIn("--latency-keys must be at least", err.getvalue(), early)

    def test_a_session_counting_only_for_latency_must_share_the_setup(self) -> None:
        good = latency_meta(1)
        other = latency_meta(2)
        other["latency"] = {**other["latency"], "keys": 50}
        rows = {"latency": {"kettle": [latency_run([30.0] * 4)] * 2, "wezterm": [latency_run([31.0] * 4)] * 2},
                "startup": {"kettle": [{"error": "no window"}], "wezterm": [{"window_ms": 1.0, "child_ms": 1.0}]}}
        other["rounds"] = {"latency": 2, "startup": 1}
        good["rounds"] = {"latency": 2, "startup": 1}
        first = write_session(self.root, "a", good, {**rows, "startup": {
            "kettle": [{"window_ms": 1.0, "child_ms": 1.0}], "wezterm": [{"window_ms": 1.0, "child_ms": 1.0}]}})
        second = write_session(self.root, "b", other, rows)
        with self.assertRaises(SystemExit):
            standing.combine([first, second])

    def test_a_refresh_rate_change_voids_latency(self) -> None:
        meta = latency_meta(1)
        results = {"workloads": {"latency": {"kettle": [latency_run([30.0]), latency_run([30.0], refresh=120)]}}}
        self.assertFalse(standing.workload_complete(results, meta, "latency"))
        results["workloads"]["latency"]["kettle"][1]["refresh_hz"] = 60
        self.assertTrue(standing.workload_complete(results, meta, "latency"))

    def test_an_aa_without_latency_still_calibrates_other_rows(self) -> None:
        aa_meta = {"date": "2026-10-01", "started": "2026-10-01T01:00:00", "refusals": [], "bare": False,
                   "complete": True, "label": "aa", "mode": "ab", "rounds": {"startup": 2}, "warmup": 0}
        aa = write_session(self.root, "aa", aa_meta, {"startup": {
            "kettle-a": [{"window_ms": 150.0, "child_ms": 1.0}] * 2, "kettle-b": [{"window_ms": 150.0, "child_ms": 1.0}] * 2}})
        ab_meta = {**latency_meta(2), "mode": "ab", "label": "ab"}
        ab_meta["rounds"] = {"latency": 2}
        ab = write_session(self.root, "ab", ab_meta, {"latency": {
            "kettle-a": [latency_run([30.0] * 4)] * 2, "kettle-b": [latency_run([29.0] * 4)] * 2}})
        standing.combine([ab], aa)  # no refusal: the A/A never ran latency
        other_meta = {**latency_meta(3), "mode": "ab", "label": "aa2"}
        other_meta["latency"] = {**other_meta["latency"], "keys": 50}
        other_meta["rounds"] = {"latency": 2}
        aa2 = write_session(self.root, "aa2", other_meta, {"latency": {
            "kettle-a": [latency_run([30.0] * 4)] * 2, "kettle-b": [latency_run([30.0] * 4)] * 2}})
        with self.assertRaises(SystemExit):
            standing.combine([ab], aa2)

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("swiftc"), "needs macOS and swiftc")
    def test_the_probe_app_is_sealed_and_passes_its_self_test(self) -> None:
        import subprocess

        app = standing.build_latency_probe(self.root, None, rebuild=True)
        sealed = subprocess.run(["codesign", "--verify", "--strict", "--deep", str(app)], capture_output=True, text=True)
        self.assertEqual(sealed.returncode, 0, "nothing may change inside the bundle after signing: " + sealed.stderr)
        self.assertTrue((self.root / "KettleLatencyProbe.build.json").exists())
        built = (app / "Contents" / "MacOS" / "latency-probe").stat().st_mtime_ns
        standing.build_latency_probe(self.root, None)
        self.assertEqual((app / "Contents" / "MacOS" / "latency-probe").stat().st_mtime_ns, built,
                         "an unchanged source and identity reuse the signed probe")
        self.assertEqual(standing.latency_probe_self_test(app), 0)
        original_identity = standing.validate_latency_probe(app)
        replacement = self.root / "replacement.app"
        shutil.copytree(app, replacement)
        (replacement / "Contents" / "Resources").mkdir(exist_ok=True)
        (replacement / "Contents" / "Resources" / "substitute-resource").write_bytes(b"validly signed substitute")
        subprocess.run(["codesign", "--force", "--sign", "-", "--identifier", standing.LATENCY_PROBE_ID,
                        str(replacement)], check=True, capture_output=True)
        standing.probe_signature(replacement)
        shutil.rmtree(app)
        replacement.rename(app)
        with self.assertRaises(RuntimeError):
            standing.validate_latency_probe(app)
        # Explicitly prepare after refusal, then exercise executable tamper.
        standing.build_latency_probe(self.root, None, rebuild=True)
        self.assertEqual(original_identity["source_sha256"], standing.validate_latency_probe(app)["source_sha256"])
        binary = app / "Contents" / "MacOS" / "latency-probe"
        with binary.open("ab") as stream:
            stream.write(b"tamper")
        from unittest import mock
        with self.assertRaises(RuntimeError):
            standing.build_latency_probe(self.root, None)
        # The real launch path refuses the tampered app before it spawns `open`.
        with mock.patch.object(standing, "run_owned_probe_open") as launch:
            with self.assertRaises(RuntimeError):
                standing.run_latency_probe(app, ["--check"], self.root, 5)
            launch.assert_not_called()
        subprocess.run(["swiftc", "-typecheck", str(HERE / "macos-standing" / "latency-floor.swift")],
                       check=True, capture_output=True)


class ProbeIntegrity(unittest.TestCase):
    """Portable filesystem/subprocess decisions. No invocation reaches macOS."""

    def setUp(self):
        import tempfile
        from unittest import mock
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.app = self.root / "KettleLatencyProbe.app"
        self.record = self.root / "KettleLatencyProbe.build.json"
        self.entitlements = {}
        self.seal_ok = True
        self.fail_build = False
        self.calls = []
        self.contract = {"source_sha256": standing.file_sha256(standing.PROBES / "latency-probe.swift"), "plist_sha256": "plist",
                         "command": ["compiler", "-O"], "compiler_path": "fixture-swiftc",
                         "compiler_version": "fixture", "sdk_path": "fixture-sdk",
                         "sdk_version": "fixture", "identity": "-"}
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(standing, "probe_build_contract", side_effect=lambda identity: {
            **self.contract, "identity": identity or "-"}).start()
        mock.patch.object(standing, "probe_command", side_effect=self.command).start()
        self.prepare()

    def command(self, argv, timeout=60):
        import plistlib
        self.calls.append(argv)
        if argv[0] == "fixture-swiftc":
            if self.fail_build:
                raise RuntimeError("interrupted fixture build")
            binary = Path(argv[argv.index("-o") + 1])
            binary.write_bytes(b"fixture executable")
            binary.chmod(0o755)
        elif argv[0] == "codesign":
            app = Path(argv[-1])
            if "--sign" in argv:
                resource = app / "Contents" / "_CodeSignature" / "CodeResources"
                resource.parent.mkdir()
                resource.write_bytes(b"fixture seal")
            if "--verify" in argv and not self.seal_ok:
                raise RuntimeError("invalid fixture seal")
            if "--verbose=4" in argv:
                return subprocess.CompletedProcess(argv, 0, b"", (
                    "Identifier=" + standing.LATENCY_PROBE_ID + "\nCDHash=" + "a" * 40 +
                    "\nSignature=adhoc\n").encode())
            if "-r-" in argv:
                return subprocess.CompletedProcess(argv, 0, b"", b'designated => identifier "fixture"\n')
            if "--entitlements" in argv:
                return subprocess.CompletedProcess(argv, 0, plistlib.dumps(self.entitlements), b"")
        return subprocess.CompletedProcess(argv, 0, b"", b"")

    def prepare(self):
        return standing.build_latency_probe(self.root, None, rebuild=True)

    def refuse_before_invocation(self):
        from unittest import mock
        for args in (["--check"], ["--request"], ["--self-test"], ["--out", str(self.root / "result")]):
            with self.subTest(args=args), mock.patch.object(standing, "run_owned_probe_open") as run, \
                    mock.patch.object(standing, "wait_for_text", return_value=True):
                with self.assertRaises(RuntimeError):
                    standing.run_latency_probe(self.app, args, self.root, 1)
                run.assert_not_called()
        with self.assertRaises(RuntimeError):
            standing.latency_probe_self_test(self.app)

    def test_legacy_bogus_cache_never_invokes(self):
        self.record.write_text(_json.dumps({"source": "source", "identity": "-"}))
        (self.app / "Contents/MacOS/latency-probe").write_bytes(b"bogus")
        self.refuse_before_invocation()

    def test_modified_executable_never_invokes(self):
        (self.app / "Contents/MacOS/latency-probe").write_bytes(b"replacement")
        self.refuse_before_invocation()

    def test_validly_signed_same_identifier_substitute_never_invokes(self):
        import shutil
        alternate = self.root / "alternate"
        alternate.mkdir()
        with standing.probe_lock(alternate):
            other = standing.build_latency_probe(alternate, None, rebuild=True)
        (other / "Contents/MacOS/latency-probe").write_bytes(b"different valid fixture executable")
        # The fixture codesign verifier accepts this substitute. Only the
        # prepared artifact receipt can distinguish it from the owned build.
        shutil.rmtree(self.app)
        other.rename(self.app)
        standing.probe_signature(self.app)
        self.refuse_before_invocation()

    def test_plist_entitlements_resources_and_extra_files_never_invoke(self):
        mutations = (lambda: (self.app / "Contents/Info.plist").write_bytes(b"changed plist"),
                     lambda: self.entitlements.update({"unexpected": True}),
                     lambda: (self.app / "Contents/_CodeSignature/CodeResources").write_bytes(b"changed seal"),
                     lambda: (self.app / "extra").write_bytes(b"unexpected"))
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                self.prepare()
                self.entitlements = {}
                mutate()
                self.refuse_before_invocation()
                self.entitlements = {}

    def test_missing_malformed_old_and_partial_receipts_never_invoke(self):
        for contents in (None, "{", "[]", '{"version":1}', '{"version":2}'):
            with self.subTest(contents=contents):
                self.prepare()
                if contents is None:
                    self.record.unlink()
                else:
                    self.record.write_text(contents)
                self.refuse_before_invocation()
        self.prepare()
        (self.app / "Contents/MacOS/latency-probe").unlink()
        self.refuse_before_invocation()

    def test_no_automatic_repair_or_receipt_promotion(self):
        self.record.write_text('{"source":"source","identity":"-"}')
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            standing.build_latency_probe(self.root, None)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.record.read_text(), '{"source":"source","identity":"-"}')

    def test_unchanged_cache_reuses_artifact_without_build_or_sign(self):
        first = standing.validate_latency_probe(self.app)
        inode = (self.app / "Contents/MacOS/latency-probe").stat().st_ino
        self.calls.clear()
        self.assertEqual(standing.build_latency_probe(self.root, None), self.app)
        self.assertEqual(first, standing.validate_latency_probe(self.app))
        self.assertEqual(inode, (self.app / "Contents/MacOS/latency-probe").stat().st_ino)
        self.assertFalse(any(c[0] == "fixture-swiftc" or "--sign" in c for c in self.calls))
        self.assertEqual(first["bundle_sha256"], standing.probe_bundle_snapshot(self.app)["bundle_sha256"])
        self.assertNotEqual(first["bundle_sha256"], first["source_sha256"])
        self.assertEqual(self.record.stat().st_mode & 0o777, 0o600)

    def test_build_contract_and_seal_mismatch_never_invoke(self):
        for field in ("source_sha256", "plist_sha256", "command", "compiler_path", "compiler_version", "sdk_path", "sdk_version"):
            with self.subTest(field=field):
                receipt = _json.loads(self.record.read_text())
                original = receipt["contract"][field]
                receipt["contract"][field] = "changed"
                self.record.write_text(_json.dumps(receipt))
                self.refuse_before_invocation()
                receipt["contract"][field] = original
                self.record.write_text(_json.dumps(receipt))
        self.seal_ok = False
        self.refuse_before_invocation()

    def test_interrupted_build_keeps_prior_artifact_and_no_partial_publish(self):
        before = self.record.read_bytes()
        self.fail_build = True
        with self.assertRaises(RuntimeError):
            self.prepare()
        self.assertEqual(self.record.read_bytes(), before)
        standing.validate_latency_probe(self.app)
        self.assertFalse(list(self.root.glob(".latency-build-*")))

    def test_partial_publication_is_refused_and_lock_released(self):
        from unittest import mock
        original = standing.os.replace
        def fail_receipt(source, dest):
            if Path(dest) == self.record:
                raise OSError("interrupted publication")
            original(source, dest)
        with mock.patch.object(standing.os, "replace", side_effect=fail_receipt), self.assertRaises(RuntimeError):
            self.prepare()
        self.refuse_before_invocation()
        self.assertEqual(standing._PROBE_LOCKS, {})
        self.prepare()

    def test_symlink_special_file_and_private_receipt_guards(self):
        for kind in ("symlink", "fifo", "receipt-link", "receipt-fifo", "receipt-public"):
            with self.subTest(kind=kind):
                self.prepare()
                extra = self.app / "extra"
                if kind == "symlink":
                    extra.symlink_to(self.record)
                elif kind == "fifo":
                    os.mkfifo(extra)
                elif kind.startswith("receipt-"):
                    if kind == "receipt-public":
                        self.record.chmod(0o644)
                    else:
                        backup = self.root / "receipt-backup"
                        backup.unlink(missing_ok=True)
                        self.record.rename(backup)
                        if kind == "receipt-link":
                            self.record.symlink_to(backup)
                        else:
                            os.mkfifo(self.record)
                self.refuse_before_invocation()

    def test_plist_replaced_by_fifo_after_snapshot_never_blocks(self):
        # The plist check uses the bytes the snapshot read through a checked
        # descriptor. Reopening the path would block on a FIFO swapped in
        # after the snapshot, with the preparation/use lock held.
        import threading
        from unittest import mock
        plist = self.app / "Contents" / "Info.plist"
        self.assertTrue(plist.is_file())
        original = standing.probe_bundle_snapshot
        swapped = []

        def snapshot_then_swap(app):
            result = original(app)
            if not swapped:
                plist.unlink()
                os.mkfifo(plist)
                swapped.append(True)
            return result
        outcome = []

        def verify():
            try:
                standing.probe_artifact(self.app)
                outcome.append("accepted")
            except RuntimeError:
                outcome.append("refused")
        with mock.patch.object(standing, "probe_bundle_snapshot", side_effect=snapshot_then_swap):
            worker = threading.Thread(target=verify, daemon=True)
            worker.start()
            worker.join(5)
            blocked = worker.is_alive()
            if blocked:
                # Release the stuck reader so the test itself never hangs.
                os.close(os.open(plist, os.O_WRONLY | os.O_NONBLOCK))
                worker.join(5)
        self.assertFalse(blocked, "verification blocked on a substituted FIFO")
        self.assertEqual(outcome, ["refused"])

    def test_mutation_during_hashing_is_refused(self):
        from unittest import mock
        original = standing.os.fstat
        count = 0
        def mutate(fd):
            nonlocal count
            count += 1
            if count == 1:
                (self.app / "Contents/MacOS/latency-probe").write_bytes(b"mutated while hashing")
            return original(fd)
        with mock.patch.object(standing.os, "fstat", side_effect=mutate), self.assertRaises(RuntimeError):
            standing.probe_bundle_snapshot(self.app)

    def test_same_bytes_replacement_during_use_is_refused(self):
        binary = self.app / "Contents/MacOS/latency-probe"
        with self.assertRaises(RuntimeError), standing.verified_probe_use(self.app):
            replacement = self.root / "replacement"
            replacement.write_bytes(binary.read_bytes())
            replacement.chmod(0o755)
            os.replace(replacement, binary)

    def test_post_invocation_tamper_is_refused(self):
        from unittest import mock
        def invocation(*args, **kwargs):
            (self.root / "probe.stdout").write_text("post events: granted")
            (self.app / "Contents/MacOS/latency-probe").write_bytes(b"tampered during use")
            return subprocess.CompletedProcess(args, 0)
        with mock.patch.object(standing, "run_owned_probe_open", side_effect=invocation) as run:
            with self.assertRaises(RuntimeError):
                standing.run_latency_probe(self.app, ["--check"], self.root, 1)
            self.assertEqual(run.call_count, 1)

    def test_duplicate_preparation_fails_without_spawning_and_use_blocks_rebuild(self):
        import fcntl
        from unittest import mock
        with mock.patch.object(standing, "probe_command") as command:
            with mock.patch.object(fcntl, "flock", side_effect=BlockingIOError), self.assertRaises(RuntimeError):
                self.prepare()
            command.assert_not_called()
        with standing.verified_probe_use(self.app), self.assertRaises(RuntimeError):
            self.prepare()
        self.assertEqual(standing._PROBE_LOCKS, {})

    def test_cancellation_releases_lock_and_checks_artifact(self):
        with self.assertRaises(KeyboardInterrupt), standing.verified_probe_use(self.app):
            raise KeyboardInterrupt()
        self.assertEqual(standing._PROBE_LOCKS, {})
        standing.validate_latency_probe(self.app)

    def test_rebuild_flag_is_checked_before_early_actions(self):
        import contextlib
        import io
        from unittest import mock
        for args in (["--rebuild-latency-probe"], ["--rebuild-latency-probe", "--combine", "none"],
                     ["--rebuild-latency-probe", "--latency-check", "--preflight-only"]):
            with mock.patch.object(sys, "argv", ["standing", *args]), contextlib.redirect_stderr(io.StringIO()), \
                    self.assertRaises(SystemExit) as error:
                standing.main()
            self.assertEqual(error.exception.code, 2)


    def test_owned_open_timeout_and_cancellation_reap_only_spawned_child(self):
        from unittest import mock
        for error in (subprocess.TimeoutExpired("open", 1), KeyboardInterrupt()):
            with self.subTest(error=type(error).__name__):
                child = mock.Mock()
                child.wait.side_effect = [error, subprocess.TimeoutExpired("open", 10), 0]
                with standing.probe_invocation_lease(self.root) as lease:
                    with mock.patch.object(standing.subprocess, "Popen", return_value=child) as spawn:
                        with self.assertRaises(type(error)):
                            standing.run_owned_probe_open(["open", "fixture"], lease, 1)
                        self.assertFalse(lease.exists())
                    spawn.assert_called_once()
                    child.kill.assert_called_once_with()
                    self.assertEqual(child.wait.call_count, 3)
                self.assertFalse(list(self.root.glob(".probe-use-*")))

    def test_repeated_cancellation_during_cleanup_still_reaps_the_child(self):
        # A real child: the first interrupt cancels the invocation, a second
        # one lands in the cleanup wait. The child must still be reaped, and
        # the first interrupt propagates.
        from unittest import mock
        interrupts = [KeyboardInterrupt("first"), KeyboardInterrupt("second")]
        spawned = []

        class Interrupted(subprocess.Popen):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, **kwargs)
                spawned.append(self)

            def wait(self, timeout=None):
                if interrupts:
                    raise interrupts.pop(0)
                return super().wait(timeout)

        def reap_own_child():
            for child in spawned:
                if child.poll() is None:
                    child.kill()
                    subprocess.Popen.wait(child)
        self.addCleanup(reap_own_child)
        with standing.probe_invocation_lease(self.root) as lease:
            with mock.patch.object(standing.subprocess, "Popen", Interrupted):
                with self.assertRaises(KeyboardInterrupt) as raised:
                    standing.run_owned_probe_open([sys.executable, "-c", "import time; time.sleep(30)"], lease, 30)
            self.assertFalse(lease.exists())
        self.assertEqual(str(raised.exception), "first")
        self.assertEqual(len(spawned), 1)
        self.assertIsNotNone(spawned[0].returncode, "the cancelled child was abandoned")

    def test_successful_owned_open_is_reaped_without_signal(self):
        from unittest import mock
        child = mock.Mock()
        with standing.probe_invocation_lease(self.root) as lease:
            with mock.patch.object(standing.subprocess, "Popen", return_value=child):
                standing.run_owned_probe_open(["open", "fixture"], lease, 1)
            child.wait.assert_called_once_with(timeout=1)
            child.kill.assert_not_called()
            self.assertTrue(lease.exists())
        self.assertFalse(lease.exists())

    def test_successful_invocation_passes_lease_and_revalidates(self):
        from unittest import mock
        validations = []
        original = standing.validate_latency_probe
        def validate(*args, **kwargs):
            validations.append(args)
            return original(*args, **kwargs)
        def invoke(command, lease, timeout):
            self.assertEqual(command[-2:], ["--lease-file", str(lease)])
            self.assertTrue(lease.exists())
            (self.root / "probe.stdout").write_text("post events: granted")
        with mock.patch.object(standing, "run_owned_probe_open", side_effect=invoke), \
                mock.patch.object(standing, "validate_latency_probe", side_effect=validate):
            self.assertEqual(standing.run_latency_probe(self.app, ["--check"], self.root, 1), "post events: granted")
        self.assertEqual(len(validations), 2)
        self.assertFalse(list(self.root.glob(".probe-use-*")))

    def test_public_artifact_excludes_private_build_and_signing_details(self):
        private = _json.loads(self.record.read_text())
        private["local"] = {"app": "/sentinel/private/home", "owner": "owner@sentinel.invalid"}
        self.record.write_text(_json.dumps(private))
        public = standing.validate_latency_probe(self.app)
        self.assertEqual(set(public), {"source_sha256", "bundle_sha256", "executable_sha256", "identifier", "cdhash", "mode"})
        self.assertNotIn("sentinel", _json.dumps(public))

    def test_preparation_runs_self_test_before_publication(self):
        from unittest import mock
        original = self.command
        def fail_test(argv, timeout=60):
            if "--self-test" in argv:
                raise RuntimeError("fixture self-test failed")
            return original(argv, timeout)
        receipt = self.record.read_bytes()
        with mock.patch.object(standing, "probe_command", side_effect=fail_test), self.assertRaises(RuntimeError):
            self.prepare()
        self.assertEqual(self.record.read_bytes(), receipt)
        standing.validate_latency_probe(self.app)


    def test_tool_hash_identifies_prepared_artifact_and_tracks_rebuild(self):
        first, artifacts = standing.probe_tool_identity({"latency-probe": self.app}, None)
        self.assertEqual(first["latency-probe"], artifacts["latency-probe"]["bundle_sha256"])
        self.assertNotEqual(first["latency-probe"], self.contract["source_sha256"])
        original = self.command
        def changed_build(argv, timeout=60):
            result = original(argv, timeout)
            if argv[0] == "fixture-swiftc":
                Path(argv[argv.index("-o") + 1]).write_bytes(b"new artifact with unchanged source")
            return result
        from unittest import mock
        with mock.patch.object(standing, "probe_command", side_effect=changed_build):
            self.prepare()
        second, _ = standing.probe_tool_identity({"latency-probe": self.app}, None)
        self.assertNotEqual(first, second)


    def test_replacement_between_artifact_check_and_identity_pin_is_refused(self):
        from unittest import mock
        original = standing.probe_artifact
        def replace_after_verification(app):
            artifact = original(app)
            (app / "Contents/MacOS/latency-probe").write_bytes(b"replacement after artifact verification")
            return artifact
        with mock.patch.object(standing, "probe_artifact", side_effect=replace_after_verification):
            with self.assertRaises(RuntimeError):
                standing.validate_latency_probe(self.app)


    def test_explicit_adhoc_identity_prepares_and_reuses(self):
        app = standing.build_latency_probe(self.root, "-", rebuild=True)
        artifact = standing.validate_latency_probe(app, "-")
        self.assertEqual(artifact["mode"], "ad hoc")
        self.assertEqual(standing.build_latency_probe(self.root, "-"), app)


    def test_rebased_blink_uses_verified_invocation_and_lease(self):
        from unittest import mock
        import fcntl
        validations = []
        original = standing.validate_latency_probe
        def validate(*args, **kwargs):
            validations.append(args)
            return original(*args, **kwargs)
        out = self.root / 'blink.json'
        def invoke(command, lease, timeout):
            self.assertIn('--blink-check', command)
            self.assertEqual(command[-2:], ['--lease-file', str(lease)])
            fd = os.open(lease, os.O_RDONLY)
            try:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                os.close(fd)
            out.write_text('{}')
        runner = standing.Runner({'latency-probe': self.app}, self.root, {})
        with mock.patch.object(standing, 'validate_latency_probe', side_effect=validate), \
                mock.patch.object(standing, 'run_owned_probe_open', side_effect=invoke) as spawn:
            runner.blink_probe(['--blink-check', '--out', str(out)], self.root, 1)
        self.assertEqual(len(validations), 2)
        self.assertEqual(spawn.call_count, 1)
        self.assertFalse(list(self.root.glob('.probe-use-*')))
        binary = self.app / 'Contents/MacOS/latency-probe'
        binary.write_bytes(b'changed')
        with mock.patch.object(standing, 'run_owned_probe_open') as spawn:
            with self.assertRaises(RuntimeError):
                runner.blink_probe(['--blink-check', '--out', str(out)], self.root, 1)
            spawn.assert_not_called()
        self.prepare()
        def mutate(command, lease, timeout):
            invoke(command, lease, timeout)
            binary.write_bytes(b'changed-during-blink')
        with mock.patch.object(standing, 'run_owned_probe_open', side_effect=mutate):
            with self.assertRaises(RuntimeError):
                runner.blink_probe(['--blink-check', '--out', str(out)], self.root, 1)
        self.assertFalse(list(self.root.glob('.probe-use-*')))


FLOOD_4K_SHA256 = "8f18d84dad9b7ab935be1aa827e9ce0d0cc97b0c2e75f08afaede576a8b08f5d"


class Flood(unittest.TestCase):
    def test_is_exactly_the_requested_size_and_the_same_every_run(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            first, second = Path(tmp) / "a", Path(tmp) / "b"
            standing.write_flood(first, 3 * (1 << 20) + 123)
            standing.write_flood(second, 3 * (1 << 20) + 123)
            data = first.read_bytes()
            self.assertEqual(len(data), 3 * (1 << 20) + 123)
            self.assertEqual(data, second.read_bytes())

    def test_first_bytes_are_pinned(self) -> None:
        # The text must not change when Python does: a changed flood would
        # silently break comparisons with earlier releases' figures.
        import hashlib
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "flood"
            standing.write_flood(path, 4096)
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
        self.assertEqual(digest, FLOOD_4K_SHA256)

    def test_is_printable_ascii_in_lines_that_fit_the_grid(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "flood"
            standing.write_flood(path, 1 << 20)
            text = path.read_bytes().decode("ascii")
        self.assertTrue(all(ch == "\n" or " " <= ch <= "~" for ch in text))
        self.assertLess(max(len(line) for line in text.split("\n")), standing.COLS)


class VtebenchCheckout(unittest.TestCase):
    """Runs against a local repository, so no network or cargo build."""

    def setUp(self) -> None:
        import subprocess
        import tempfile

        self.run_git = lambda *args, cwd: subprocess.run(
            ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
        ).stdout.strip()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.upstream = Path(self.tmp.name) / "upstream"
        self.upstream.mkdir()
        self.run_git("init", "--quiet", cwd=self.upstream)
        for name in ("first", "second"):
            (self.upstream / name).write_text(name)
            self.run_git("add", name, cwd=self.upstream)
            self.run_git("-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false",
                         "commit", "--quiet", "-m", name, cwd=self.upstream)
        self.first = self.run_git("rev-parse", "HEAD~1", cwd=self.upstream)
        self.checkout = Path(self.tmp.name) / "vtebench"

    def test_moves_a_fresh_clone_to_the_pinned_commit(self) -> None:
        moved = standing.checkout_vtebench(self.checkout, str(self.upstream), self.first)
        self.assertTrue(moved)
        self.assertEqual(self.run_git("rev-parse", "HEAD", cwd=self.checkout), self.first)
        self.assertFalse(standing.checkout_vtebench(self.checkout, str(self.upstream), self.first))

    def test_a_pin_that_names_no_commit_fails_with_its_value(self) -> None:
        # A correct 7-character prefix with a wrong remainder must fail here,
        # not when a full run reaches vtebench.
        bogus = self.first[:7] + "0" * 33
        with self.assertRaises(SystemExit) as raised:
            standing.checkout_vtebench(self.checkout, str(self.upstream), bogus)
        self.assertIn(bogus, str(raised.exception))

    def test_a_checkout_with_local_changes_is_refused(self) -> None:
        # Its edits would be built and benchmarked as if they were the pin.
        standing.checkout_vtebench(self.checkout, str(self.upstream), self.first)
        (self.checkout / "first").write_text("edited")
        with self.assertRaises(SystemExit) as raised:
            standing.checkout_vtebench(self.checkout, str(self.upstream), self.first)
        self.assertIn("local changes", str(raised.exception))
        self.assertEqual((self.checkout / "first").read_text(), "edited")

    def test_the_shipped_pin_is_a_full_commit_id(self) -> None:
        self.assertRegex(standing.VTEBENCH_REV, r"^[0-9a-f]{40}$")

    def test_the_workspace_excludes_the_checkout(self) -> None:
        # Cargo refuses to build a package inside a workspace root that does
        # not list it.
        import tomllib

        if SNAPSHOT_LAYOUT:
            self.skipTest("workspace excludes need the real repository Cargo.toml")
        manifest = tomllib.loads((standing.REPO / "Cargo.toml").read_text())
        for name in ("vtebench", "vtebench-us"):
            checkout = standing.REPO / "target" / "perf-tools" / "macos-standing" / name
            self.assertIn(
                checkout.relative_to(standing.REPO).as_posix(),
                manifest["workspace"]["exclude"],
            )


# The size lookup exactly as the pinned vtebench scripts spell it.
UPSTREAM_SIZE_SCRIPT = """#!/bin/sh
tty="/dev/$(ps -o tty= -p $$)"
columns=$(tput cols < $tty)
lines=$(tput lines < $tty)
printf "%s %s" "$columns" "$lines"
"""



def run_like_vtebench(script: Path, cols: int, rows: int) -> str:
    """Run `script` the way vtebench runs a benchmark script.

    It gets a pty of `cols` x `rows` as its controlling terminal, with stdin
    on /dev/null and stdout and stderr on pipes. The terminal keeps the tty
    open, as it would under vtebench; with no open descriptor macOS drops it.
    """
    import fcntl
    import os
    import pty
    import struct
    import termios

    out_r, out_w = os.pipe()
    pid, master = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        os.dup2(0, 3, inheritable=True)
        null = os.open("/dev/null", os.O_RDONLY)
        os.dup2(null, 0)
        os.dup2(out_w, 1)
        os.dup2(out_w, 2)
        os.execv(str(script), [str(script)])
    os.close(out_w)
    chunks = []
    while chunk := os.read(out_r, 65536):
        chunks.append(chunk)
    os.waitpid(pid, 0)
    os.close(master)
    return b"".join(chunks).decode()


class VtebenchSizeFix(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.source = Path(self.tmp.name) / "benchmarks"
        (self.source / "probe").mkdir(parents=True)
        (self.source / "probe" / "benchmark").write_text(UPSTREAM_SIZE_SCRIPT)
        (self.source / "probe" / "benchmark").chmod(0o755)
        (self.source / "top_region").mkdir()
        (self.source / "top_region" / "setup").write_text(
            '#!/bin/sh\n\nprintf "\\e[?1049h\\e[2;$(tput lines)r"\n'
        )
        (self.source / "top_region" / "setup").chmod(0o755)
        (self.source / "top_region" / "benchmark").symlink_to("../probe/benchmark")

    def test_the_patched_scripts_see_the_real_window_size(self) -> None:
        if SNAPSHOT_LAYOUT:
            self.skipTest('native ps/tty lookup needs an unrestricted runner')
        benchmarks = standing.prepare_benchmarks(self.source, Path(self.tmp.name) / "patched")
        self.assertEqual(run_like_vtebench(benchmarks / "probe" / "benchmark", 50, 20), "50 20")
        self.assertEqual(
            run_like_vtebench(benchmarks / "top_region" / "setup", 50, 20), "\x1b[?1049h\x1b[2;20r"
        )
        # A symlinked script is copied so it can be patched on its own.
        self.assertFalse((benchmarks / "top_region" / "benchmark").is_symlink())
        self.assertEqual(run_like_vtebench(benchmarks / "top_region" / "benchmark", 50, 20), "50 20")

    @unittest.skipUnless(sys.platform == "darwin", "the padded `ps` output is macOS behavior")
    def test_the_unpatched_lookup_finds_no_size_on_macos(self) -> None:
        # Why the copy exists: upstream's lookup reads nothing here, so the
        # scripts that need a size print nothing or the wrong escapes.
        size = run_like_vtebench(self.source / "probe" / "benchmark", 50, 20)
        self.assertNotEqual(size, "50 20")

    def test_a_size_lookup_the_fix_does_not_cover_is_refused(self) -> None:
        (self.source / "probe" / "benchmark").write_text("#!/bin/sh\nprintf %s $(tput cols)\n")
        with self.assertRaises(SystemExit) as raised:
            standing.prepare_benchmarks(self.source, Path(self.tmp.name) / "patched")
        self.assertIn("probe/benchmark", str(raised.exception))

    def test_a_benchmark_vtebench_dropped_is_reported(self) -> None:
        benchmarks = standing.prepare_benchmarks(self.source, Path(self.tmp.name) / "patched")
        self.assertEqual(standing.missing_benchmarks(benchmarks, {"probe": 7.0}), ["top_region"])
        self.assertEqual(
            standing.missing_benchmarks(benchmarks, {"probe": 7.0, "top_region": 9.0}), []
        )


class MetricContracts(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    @staticmethod
    def validity(valid=True, **extra):
        return {"valid": valid, "expected": 5, "observed": 5, "capability_version": "fixture-v1", **extra}

    def scalar_results(self, field="footprint_mib", workload="idle", a=None, b=None, optional=False):
        a = a or [10., 10., 10.]
        b = b or [11., 12., 18.]
        runs = lambda values: [{field: v, "frontmost": True, "cpu_percent": 1., "wakeups_per_second": 1.,
                               "child_ms": 10., "window_ms": 10., "footprint_mib": 10., field: v,
                               **({"metric_validity": {field: self.validity()}} if optional else {})} for v in values]
        meta = {"date": "2026-01-01", "complete": True, "mode": "ab", "rounds": {workload: 3}, "warmup": 0}
        results = {"schema": 3, "context": "fixture", "meta": meta, "terminals": ["kettle-a", "kettle-b"],
                   "workloads": {workload: {"kettle-a": runs(a), "kettle-b": runs(b)}}}
        if field.startswith("typing_"):
            # Typing values count only with the verified probe and method.
            artifact = {key: char * 64 for key, char in (
                ("source_sha256", "a"), ("bundle_sha256", "b"), ("executable_sha256", "c"))}
            meta["latency"] = {"typing_memory": standing.hc.typing_method()}
            meta["tool_artifacts"] = {"latency-probe": artifact}
            meta["tool_hashes"] = {"latency-probe": artifact["bundle_sha256"]}
            for rows in results["workloads"][workload].values():
                for row in rows:
                    row.update(tool_artifact=artifact, typing_tool_artifact=artifact)
        return results

    def store(self, results, name="fixture"):
        folder = self.root / name
        folder.mkdir()
        (folder / "results.json").write_text(json_dumps(results))
        return folder

    def test_typing_memory_has_ratio_gate_and_mib_difference(self):
        results = self.scalar_results("typing_footprint_mib", "latency", optional=True)
        for runs in results["workloads"]["latency"].values():
            for run in runs:
                run.update(latency_run([10., 10.]))
        control = self.scalar_results("typing_footprint_mib", "latency", b=[10., 10., 10.], optional=True)
        for runs in control["workloads"]["latency"].values():
            for run in runs:
                run.update(latency_run([10., 10.]))
        combined = standing.combine([self.store(results)], self.store(control, "aa"))
        row = combined["rows"]["latency.typing_footprint_mib"]
        self.assertEqual(row["descriptor"]["unit"], "MiB")
        self.assertEqual(row["descriptor"]["direction"], "lower")
        self.assertEqual(row["descriptor"]["analysis_kind"], "scalar")
        self.assertIn("gate", row["aa"])
        self.assertNotIn("gate_ms", row["aa"])
        self.assertAlmostEqual(row["sessions"][0]["difference"]["diff"], 11 / 3)
        self.assertNotIn("headline_ms", row["verdict"])

    def test_idle_and_startup_differences_survive_combine(self):
        for workload, field, unit in (("idle", "footprint_mib", "MiB"), ("startup", "child_ms", "ms")):
            results = self.scalar_results(field, workload)
            row = standing.combine([self.store(results, workload)])["rows"][f"{workload}.{field}"]
            self.assertEqual(row["descriptor"]["unit"], unit)
            self.assertAlmostEqual(row["sessions"][0]["difference"]["diff"], 11 / 3)
            self.assertEqual(row["per_session"][0]["ab_diff"]["n"], 3)

    def test_signed_slack_keeps_negative_difference_without_ratio(self):
        field = "fonts_ready_to_resumed_ms"
        results = self.scalar_results(field, "startup", a=[1., 2., 3.], b=[-1., -2., -3.], optional=True)
        entry = standing.analyze(results, results["terminals"], True)["startup"]["metrics"][field]
        self.assertEqual(entry["ab_diff"]["diff"], -4.)
        self.assertEqual(entry["descriptor"]["direction"], "none")
        self.assertNotIn("ab", entry)
        row = standing.combine([self.store(results)])["rows"]["startup." + field]
        self.assertEqual(row["per_session"][0]["ab_diff"]["diff"], -4.)
        self.assertEqual(row["verdict"]["verdict"], "diagnostic only")

    def test_adjacent_peer_ties_need_all_pairwise_intervals(self):
        names = ["kettle", "alacritty", "kitty"]
        rows = {"kettle": [latency_run([10., 10.]) for _ in range(6)],
                "alacritty": [latency_run([v, v]) for v in [10., 12., 10., 12., 10., 12.]],
                "kitty": [latency_run([20., 20.]) for _ in range(6)]}
        results = {"workloads": {"latency": rows}, "meta": {"rounds": {"latency": 6}}}
        entry = standing.analyze(results, names, False)["latency"]["metrics"]["mean_ms"]
        self.assertEqual(len(entry["pairwise"]), 3)
        self.assertEqual(entry["adjacent"], [{"base": "kettle", "test": "alacritty", "order": "tied"},
                                              {"base": "alacritty", "test": "kitty", "order": "ordered"}])
        pair = next(p for p in entry["pairwise"] if p["base"] == "alacritty")
        self.assertEqual(pair["current"]["diff"], 9.)
        self.assertGreater(pair["current"]["diff_low"], 0.)
        self.assertNotIn("plan", pair)
        results.update(schema=3, context="fixture", terminals=names)
        results["meta"].update(date="2026-01-01", complete=True, mode="standing")
        combined = standing.combine([self.store(results)])["rows"]["latency.mean_ms"]
        self.assertEqual(combined["per_session"][0]["pairwise"], entry["pairwise"])
        self.assertEqual(combined["per_session"][0]["adjacent"], entry["adjacent"])
        self.assertEqual(entry["rank"], 1, "legacy numeric rank remains unchanged")

    def test_metric_failure_does_not_discard_other_measurements(self):
        results = self.scalar_results("typing_footprint_mib", "latency", optional=True)
        for runs in results["workloads"]["latency"].values():
            for run in runs:
                run.update(latency_run([10., 10.]))
        results["workloads"]["latency"]["kettle-b"][1]["metric_validity"]["typing_footprint_mib"] = self.validity(False, reason="coverage")
        combined = standing.combine([self.store(results)])
        memory = combined["rows"]["latency.typing_footprint_mib"]
        timing = combined["rows"]["latency.mean_ms"]
        self.assertFalse(memory["sessions"][0]["countable"])
        self.assertTrue(timing["sessions"][0]["countable"])
        self.assertEqual(memory["per_session"][0]["metric_countable"]["kettle-b"]["failed"], 1)
        self.assertIn("coverage", memory["per_session"][0]["metric_countable"]["kettle-b"]["reasons"])

    def test_optional_fields_need_evidence_and_keep_valid_zero(self):
        descriptor = standing.metric_descriptor("latency", "typing_footprint_mib")
        field = "typing_footprint_mib"
        self.assertIsNone(standing.metric_value(descriptor, "latency", {field: 0.}))
        # Typing values also need valid latency guards and the verified probe.
        artifact = {key: char * 64 for key, char in (
            ("source_sha256", "a"), ("bundle_sha256", "b"), ("executable_sha256", "c"))}
        context = {**latency_run([10., 10.]), "tool_artifact": artifact, "typing_tool_artifact": artifact}
        good = {field: 0., "metric_validity": {field: self.validity()}, **context}
        self.assertEqual(standing.metric_value(descriptor, "latency", good), 0.)
        for change in ({"capability_version": None}, {"observed": 4}, {"expected": None}, {"valid": False}):
            bad = {field: 0., "metric_validity": {field: self.validity(**change)}, **context}
            self.assertIsNone(standing.metric_value(descriptor, "latency", bad))

    def test_nonfinite_samples_never_enter_analysis(self):
        for value in (math.nan, math.inf, -math.inf, True):
            self.assertIsNone(standing.row_value("startup", {"child_ms": value}, "child_ms"))
            self.assertIsNone(standing.latency_keys({"samples_ms": [value]}, 500.))
        self.assertIsNone(standing.latency_keys({"samples_ms": [1.], "censored": -1}, 500.))

    def test_schema3_and_old_schema_reads_do_not_invent_capabilities(self):
        for schema in (1, 2, 3):
            results = self.scalar_results("child_ms", "startup")
            results["schema"] = schema
            loaded = standing.load_session(self.store(results, str(schema)))
            self.assertEqual(loaded["schema"], schema)
            analysis = standing.analyze(loaded["results"], loaded["names"], True)
            self.assertNotIn("first_output_ms", analysis["startup"]["metrics"])
            self.assertNotIn("metric_validity", loaded["results"]["workloads"]["startup"]["kettle-a"][0])
            self.assertEqual(loaded["countable"], schema != 1)
        self.assertEqual(standing.SCHEMA, 3)
        self.assertEqual(standing.EVIDENCE_CONTRACT, "hc-v1")
        results["schema"] = 99
        with self.assertRaisesRegex(ValueError, "unsupported results schema"):
            standing.load_session(self.store(results, "unknown"))

    def test_zero_containing_ratio_control_keeps_legacy_gate(self):
        control = self.scalar_results("wakeups_per_second", a=[0., 1., 1., 1., 1.], b=[1.] * 5)
        control["meta"]["rounds"]["idle"] = 5
        folders = []
        for day in (2, 3):
            result = self.scalar_results("wakeups_per_second", a=[2.] * 5, b=[1.] * 5)
            result["meta"].update(date=f"2026-01-0{day}", rounds={"idle": 5})
            folders.append(self.store(result, str(day)))
        row = standing.combine(folders, self.store(control, "aa"))["rows"]["idle.wakeups_per_second"]
        self.assertTrue(row["aa"]["contains_one"])
        self.assertEqual(row["aa"]["half_width"], math.inf)
        self.assertEqual(row["aa"]["gate"], math.inf)
        self.assertEqual(row["verdict"], {"verdict": "no change", "headline": .5, "gate": math.inf})

    def test_current_warmup_exclusion_and_censor_bound(self):
        results = self.scalar_results("child_ms", "startup")
        results["workloads"]["startup"]["kettle-a"].insert(0, {"child_ms": 999., "warmup": True})
        entry = standing.analyze(results, results["terminals"], True)["startup"]["metrics"]["child_ms"]
        self.assertEqual(entry["values"]["kettle-a"], [None, 10., 10., 10.])
        rows = {"kettle": [latency_run([1.], censored=1)]}
        info = standing.analyze({"workloads": {"latency": rows}}, ["kettle"], False)
        current = info["latency"]["metrics"]["mean_ms"]["statistics"]["current"]["terminals"]["kettle"]
        self.assertEqual(current["estimate"], 250.5)
        self.assertEqual(current["n"], 1)
        # These rows distinguish the original decoder, which accepts negative
        # keys and coerces fractional/bool censor counts with int().
        for row in ({"samples_ms": [-1.]}, {"samples_ms": [1.], "censored": -1},
                    {"samples_ms": [1.], "censored": .5}, {"samples_ms": [1.], "censored": True}):
            self.assertIsNone(standing.latency_keys(row, 500.))
        for bound in (0., -1., math.inf):
            self.assertIsNone(standing.latency_keys({"samples_ms": [1.]}, bound))

    def test_interruption_and_first_dates_stay_metric_local(self):
        folders = []
        for i, (day, complete, valid) in enumerate((("2026-01-01", False, True),
                                                   ("2026-01-02", True, False),
                                                   ("2026-01-03", True, True),
                                                   ("2026-01-04", True, True))):
            r = self.scalar_results("typing_footprint_mib", "latency", a=[10.] * 5, b=[20.] * 5, optional=True)
            r["meta"].update(date=day, complete=complete, rounds={"latency": 5})
            for runs in r["workloads"]["latency"].values():
                for run in runs:
                    run.update(latency_run([10., 10.]))
                # Four valid pairs meet the 80% comparison threshold, while
                # the optional metric still lacks one required round.
                if not valid:
                    runs[1]["metric_validity"]["typing_footprint_mib"] = self.validity(False)
            folders.append(self.store(r, str(i)))
        row = standing.combine(folders)["rows"]["latency.typing_footprint_mib"]
        dates = standing.first_per_date(row["sessions"], 2)
        self.assertEqual([s["date"] for s in dates], ["2026-01-03", "2026-01-04"])
        self.assertFalse(row["sessions"][0]["countable"])
        self.assertEqual(row["sessions"][1]["n"], 4)
        self.assertFalse(row["sessions"][1]["countable"])


    def test_latency_adjacent_without_paired_launches_is_tied(self):
        names = ["kettle", "kitty"]
        # Descriptive entries can be ranked before completeness rejects the
        # session. Their available launch positions still need not overlap.
        rows = {"kettle": [latency_run([10.]), {"error": "missing"}],
                "kitty": [{"error": "missing"}, latency_run([20.])]}
        results = {"workloads": {"latency": rows}, "meta": {"rounds": {"latency": 6}}}
        entry = standing.analyze(results, names, False)["latency"]["metrics"]["mean_ms"]
        self.assertEqual(entry["pairwise"], [{"base": "kettle", "test": "kitty", "current": {}}])
        self.assertEqual(entry["adjacent"], [{"base": "kettle", "test": "kitty", "order": "tied"}])

    def test_scalar_adjacent_peers_use_current_ratio_intervals(self):
        names = ["kettle", "alacritty", "kitty"]
        values = {"kettle": [10.] * 6, "alacritty": [10., 12., 10., 12., 10., 12.], "kitty": [20.] * 6}
        results = {"schema": 3, "context": "fixture", "terminals": names,
                   "meta": {"rounds": {"idle": 6}, "date": "2026-01-01", "complete": True, "mode": "standing"},
                   "workloads": {"idle": {name: [{"frontmost": True, "footprint_mib": v,
                                                  "cpu_percent": 1., "wakeups_per_second": 1.} for v in vs]
                                          for name, vs in values.items()}}}
        entry = standing.analyze(results, names, False)["idle"]["metrics"]["footprint_mib"]
        self.assertEqual(entry["adjacent"], [{"base": "kettle", "test": "alacritty", "order": "tied"},
                                              {"base": "alacritty", "test": "kitty", "order": "ordered"}])
        self.assertEqual(len(entry["pairwise"]), 3)
        peer = next(p for p in entry["pairwise"] if p["base"] == "alacritty")
        self.assertAlmostEqual(peer["current"]["ratio"], (10 / 3) ** .5)
        self.assertGreater(peer["current"]["low"], 1.)
        combined = standing.combine([self.store(results)])["rows"]["idle.footprint_mib"]
        self.assertEqual(combined["per_session"][0]["adjacent"], entry["adjacent"])
        self.assertEqual(combined["per_session"][0]["pairwise"], entry["pairwise"])

    def test_only_current_statistics_are_emitted(self):
        results = self.scalar_results()
        combined = standing.combine([self.store(results)])
        analysis = standing.analyze(results, results["terminals"], True)
        for info in analysis.values():
            for entry in info["metrics"].values():
                self.assertEqual(set(entry["statistics"]), {"authoritative", "current"})
        for row in combined["rows"].values():
            for session in row["per_session"]:
                self.assertEqual(set(session["statistics"]), {"authoritative", "current"})
        rows = {"kettle": [latency_run([10., 12.]) for _ in range(3)]}
        latency = standing.analyze({"workloads": {"latency": rows}}, ["kettle"], False)
        for entry in latency["latency"]["metrics"].values():
            self.assertEqual(set(entry["statistics"]), {"authoritative", "current"})
        markdown = standing.summarize(results, results["terminals"], True, analysis) + combined["markdown"]
        self.assertNotIn("bootstrap", markdown)
        self.assertNotIn("statistics.plan", markdown)
        source = (HERE / "macos-standing.py").read_text()
        for removed in ("def plan_scalar", "def plan_latency", "PLAN_RESAMPLES", "def bootstrap_ci"):
            self.assertNotIn(removed, source)


    def test_vtebench_invalid_member_loses_aggregate_round(self):
        for value in ("nan", "inf", math.nan, math.inf, -math.inf):
            rows = [{"means_ms": {"x": 1., "y": 100.}} for _ in range(5)]
            rows[4]["means_ms"]["x"] = value
            results = {"schema": 3, "context": "fixture", "terminals": ["kettle"],
                       "meta": {"rounds": {"vtebench": 5}, "complete": True,
                                "date": "2026-01-01", "mode": "standing"},
                       "workloads": {"vtebench": {"kettle": rows}}}
            entry = standing.analyze(results, ["kettle"], False)["vtebench"]["metrics"]["geometric mean"]
            self.assertIsNone(entry["values"]["kettle"][4])
            for value in entry["values"]["kettle"][:4]:
                self.assertAlmostEqual(value, 10.)
            self.assertEqual(entry["terminals"]["kettle"]["n"], 4)
            local = entry["metric_countable"]["kettle"]
            self.assertEqual(local["failed"], 1)
            self.assertFalse(local["countable"])
            row = standing.combine([self.store(results, str(value) + str(len(list(self.root.iterdir()))))])["rows"]["vtebench.geometric mean"]
            self.assertNotIn("kettle", row["terminals"])
        # A missing member cannot change the benchmark set either.
        del rows[4]["means_ms"]["x"]
        values = standing.workload_metrics("vtebench", {"kettle": rows})
        self.assertIsNone(values["geometric mean"]["kettle"][4])

    def test_flood_generated_offset_spellings_have_contracts(self):
        offsets = [0., .00001, .000001, 1.25, 1000000., 1e20, -1e-5]
        fields = standing.flood_metrics(offsets)
        # Parsing a historical descriptor must also tolerate nonfinite :g
        # spellings. Offset validity is the collector's separate responsibility.
        for field in standing.flood_metrics(offsets + [math.inf, -math.inf, math.nan]):
            self.assertEqual(standing.metric_descriptor("flood-memory", field).unit, "MiB")
        results = {"schema": 2, "context": "fixture", "terminals": ["kettle"],
                   "meta": {"date": "2026-01-01", "complete": True, "mode": "standing",
                            "rounds": {"flood-memory": 3}, "flood_offsets": offsets},
                   "workloads": {"flood-memory": {"kettle": [dict.fromkeys(fields, 10.) for _ in range(3)]}}}
        analysis = standing.analyze(results, ["kettle"], False)
        self.assertEqual(tuple(analysis["flood-memory"]["metrics"]), fields)
        combined = standing.combine([self.store(results)])
        self.assertEqual(tuple(combined["rows"]), tuple("flood-memory." + field for field in fields))

    def test_adjacent_labels_follow_completed_tables(self):
        names = ["kettle", "kitty"]
        rows = {name: [{"child_ms": 10., "window_ms": 20., "first_output_ms": 30.,
                        "metric_validity": {"first_output_ms": self.validity()}} for _ in range(3)]
                for name in names}
        results = {"schema": 3, "context": "fixture", "terminals": names,
                   "meta": {"date": "2026-01-01", "complete": True, "mode": "standing",
                            "rounds": {"startup": 3}}, "workloads": {"startup": rows}}
        outputs = [standing.summarize(results, names, False), standing.combine([self.store(results)])["markdown"]]
        for output in outputs:
            lines = output.splitlines()
            first = next(i for i, line in enumerate(lines) if line.startswith("Adjacent "))
            last_table_row = max(i for i, line in enumerate(lines) if line.startswith("|"))
            self.assertGreater(first, last_table_row)
            self.assertEqual(lines[last_table_row + 1], "")
        # Schema 3 can expose contracts, but labels require a new metric.
        for runs in rows.values():
            for run in runs:
                del run["first_output_ms"]
        self.assertNotIn("Adjacent ", standing.summarize(results, names, False))
        self.assertNotIn("Adjacent ", standing.combine([self.store(results, "without-new")])["markdown"])

    def test_legacy_reports_do_not_add_fields_or_sections(self):
        results = {"schema": 2, "context": "fixture", "terminals": ["kettle", "kitty"],
                   "meta": {"date": "2026-01-01", "complete": True, "mode": "standing",
                            "rounds": {"startup": 3}},
                   "workloads": {"startup": {name: [{"child_ms": 10., "window_ms": 20.} for _ in range(3)]
                                             for name in ("kettle", "kitty")}}}
        self.assertEqual(standing.summarize(results, results["terminals"], False), '# macOS standing\n\nfixture\n\n## startup\n\n| metric | kettle | kitty |\n|---|---:|---:|\n| window_ms | 20.00 | 20.00 |\n| child_ms | 10.00 | 10.00 |\n\n| metric | best other | Kettle/other | 95% CI | Kettle lower in | Kettle-other (95% CI) |\n|---|---|---:|---|---:|---|\n| window_ms | kitty | 1.000 | 1.000-1.000 | 0/3 | +0.0 ms (+0.0 to +0.0) |\n| child_ms | kitty | 1.000 | 1.000-1.000 | 0/3 | +0.0 ms (+0.0 to +0.0) |\n')
        combined = standing.combine([self.store(results)])
        self.assertEqual(set(combined), {"sessions", "rows", "markdown"})
        for row in combined["rows"].values():
            self.assertEqual(set(row), {"terminals", "sessions", "per_session", "claim"})
            self.assertEqual(set(row["per_session"][0]), {"label", "countable", "estimates"})
            self.assertNotIn("difference", row["sessions"][0])
        self.assertNotIn("## Absolute differences", combined["markdown"])
        self.assertNotIn("Adjacent ", combined["markdown"])




class ConfigClosureTests(unittest.TestCase):
    """PR 3 regression cases use frozen files, never a terminal or user config."""
    def setUp(self):
        import tempfile
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # Resolved: macOS temp dirs sit under /var, a link to /private/var,
        # and the closure records resolved private paths.
        self.root = Path(self.tmp.name).resolve()
        self.source = self.root / 'wallpaper.png'
        self.source.write_bytes(b'wallpaper-one')
        self.sequence = 0

    def capture(self, configs=None, env=None):
        self.sequence += 1
        work = self.root / f'campaign-{self.sequence}'
        work.mkdir()
        configs = configs or {'kettle': 'background-image = wallpaper.png'}
        standing.write_configs(work, configs)
        return standing.ConfigClosure(work, configs, self.root, env or {'HOME': str(self.root)})

    def session(self, closure, name):
        folder = self.root / name
        folder.mkdir()
        names = ['kettle-a', 'kettle-b']
        results = {'schema': 3, 'context': 'fixture', 'terminals': names,
                   'meta': {'date': '2026-01-01', 'complete': True, 'mode': 'ab', 'label': name,
                            'rounds': {'idle': 3}, 'configs': standing.config_record({'kettle-a': '', 'kettle-b': ''})[0],
                            'config_closures': closure.public},
                   'workloads': {'idle': {n: [{'footprint_mib': 10., 'cpu_percent': 0.,
                                               'wakeups_per_second': 1.} for _ in range(3)] for n in names}}}
        standing.Recorder(folder / 'results.json', results).write()
        return folder

    def test_pr3_same_text_changed_wallpaper_changes_closure(self):
        text = {'kettle': 'background-image = wallpaper.png'}
        first = self.capture(text)
        legacy = standing.config_record(text)[0]
        self.source.write_bytes(b'wallpaper-two')
        second = self.capture(text)
        self.assertEqual(legacy, standing.config_record(text)[0])
        self.assertNotEqual(first.public['kettle']['sha256'], second.public['kettle']['sha256'])
        self.assertEqual(first.public['kettle']['assets'][0]['sha256'], __import__('hashlib').sha256(b'wallpaper-one').hexdigest())

    def test_pr3_b_only_change_preserves_a_and_calibration(self):
        other = self.root / 'other.png'
        other.write_bytes(b'b-one')
        same = {'kettle-a': 'background-image = wallpaper.png', 'kettle-b': 'background-image = wallpaper.png'}
        control = self.capture(same)
        text = {**same, 'kettle-b': 'background-image = other.png'}
        first = self.capture(text)
        other.write_bytes(b'b-two')
        second = self.capture(text)
        self.assertEqual(first.public['kettle-a'], second.public['kettle-a'])
        self.assertNotEqual(first.public['kettle-b'], second.public['kettle-b'])
        self.assertTrue(standing.config_closure_match(control.public, second.public))
        standing.combine([self.session(second, 'ab-b-change')], aa=self.session(control, 'aa'))

    def test_pr3_changed_a_rejects_old_calibration(self):
        text = {'kettle-a': 'background-image = wallpaper.png', 'kettle-b': 'background-image = wallpaper.png'}
        control = self.capture(text)
        self.source.write_bytes(b'changed-a')
        reference = self.capture(text)
        with self.assertRaisesRegex(SystemExit, 'baseline config'):
            standing.combine([self.session(reference, 'new-a')], aa=self.session(control, 'old-aa'))
        # A missing legacy closure cannot stand for captured bytes.
        self.assertFalse(standing.config_closure_match({}, reference.public))

    def test_pr3_aa_sides_must_have_same_closure(self):
        other = self.root / 'other.png'
        other.write_bytes(b'b')
        same = self.capture({'kettle-a': '', 'kettle-b': ''})
        unequal = self.capture({'kettle-a': '', 'kettle-b': 'background-image = other.png'})
        with self.assertRaisesRegex(SystemExit, 'same build and config'):
            standing.combine([self.session(same, 'reference')], aa=self.session(unequal, 'bad-aa'))

    def test_pr3_private_snapshot_locations_do_not_change_identity(self):
        first, second = self.capture(), self.capture()
        self.assertNotEqual(first.local['kettle']['mapping'][0]['snapshot'], second.local['kettle']['mapping'][0]['snapshot'])
        self.assertEqual(first.public, second.public)
        self.assertNotEqual(first.local['kettle']['generated'], second.local['kettle']['generated'])
        self.assertEqual(first.public['kettle']['assets'], [{'role': 'background-image', 'size': 13,
                         'sha256': __import__('hashlib').sha256(b'wallpaper-one').hexdigest()}])

    def test_pr3_alias_last_assignment_quotes_whitespace_and_empty(self):
        # The tokenizer keeps whitespace inside quotes, but kettle-config trims
        # background-image again after unquoting, so Kettle opens
        # wallpaper.png, not a file named with the padding.
        (self.root / '  wallpaper.png  ').write_bytes(b'padded name')
        closure = self.capture({'kettle': '# ignored\nbackground-image = absent.png\n BACKGROUND_IMAGE = "  wallpaper.png  "'})
        mapping = closure.local['kettle']['mapping']
        self.assertEqual(mapping[0]['original'], 'wallpaper.png')
        self.assertEqual(Path(mapping[0]['snapshot']).read_bytes(), b'wallpaper-one')
        self.assertIn('background-image = absent.png', closure.local['kettle']['generated'])
        # Only Rust's White_Space is trimmed: U+001C stays part of the name.
        (self.root / 'wallpaper.png\x1c').write_bytes(b'separator name')
        separator = self.capture({'kettle': 'background-image = "wallpaper.png\x1c"'})
        self.assertEqual(Path(separator.local['kettle']['mapping'][0]['snapshot']).read_bytes(), b'separator name')
        reset = self.capture({'kettle': "background-image = absent.png\nbackground_image = ''"})
        self.assertEqual(reset.public['kettle']['assets'], [])
        # A # inside a value is literal, and only one matched quote pair strips.
        (self.root / '#asset').write_bytes(b'hash')
        literal = self.capture({'kettle': "background_image = '#asset'"})
        self.assertEqual(literal.public['kettle']['assets'][0]['sha256'], __import__('hashlib').sha256(b'hash').hexdigest())

    def test_pr3_home_and_relative_resolution_match_renderer(self):
        home = self.capture({'kettle': 'background-image = ~/wallpaper.png'})
        relative = self.capture()
        self.assertEqual(home.public, relative.public)
        self.assertEqual(Path(home.local['kettle']['mapping'][0]['resolved']), self.source)
        for env in ({'HOME': '', 'USERPROFILE': str(self.root)}, {'APPDATA': str(self.root)}):
            fallback = self.capture({'kettle': 'background-image = ~/wallpaper.png'}, env)
            self.assertEqual(fallback.public, relative.public)
        with self.assertRaises(standing.ConfigClosureError):
            standing.config_asset_path('~/wallpaper.png', self.root, {})

    def test_pr3_tokenizer_matches_kettle_line_and_key_rules(self):
        # Expected entries follow kettle-config parse.rs: only "\n" ends a
        # line, str::trim strips Unicode White_Space (not \x1c-\x1f), keys
        # lowercase ASCII letters only, and a value is unquoted once after
        # trimming, with nothing trimmed inside the quotes.
        for separator in ('\v', '\f', '\r', '\x1c', '\x1d', '\x1e', '\x85', '\u2028', '\u2029'):
            with self.subTest(separator=repr(separator)):
                text = f'background-image = wallpaper.png{separator}background_image = other.png'
                self.assertEqual(standing.config_entries(text),
                                 [(0, 'background-image', f'wallpaper.png{separator}background_image = other.png')])
        self.assertEqual(standing.config_entries('background-image = wallpaper.png\x1c'),
                         [(0, 'background-image', 'wallpaper.png\x1c')])
        self.assertEqual(standing.config_entries('\u3000background_IMAGE\xa0=\u2003wallpaper.png\u3000\r\n'),
                         [(0, 'background-image', 'wallpaper.png')])
        self.assertEqual(standing.config_entries('background-image = " wallpaper.png "'),
                         [(0, 'background-image', ' wallpaper.png ')])
        # U+212A KELVIN SIGN lowercases to "k" in Unicode, not in ASCII: Kettle
        # leaves this key unknown, so its nonempty value is refused.
        with self.assertRaises(standing.ConfigClosureError):
            standing.config_entries('bac\u212aground-image = wallpaper.png')
        self.assertEqual(standing.config_lines('a\r\nb\n\nc'), ['a\r\n', 'b\n', '\n', 'c'])
        self.assertEqual(''.join(standing.config_lines('a\u2028b\n')), 'a\u2028b\n')

    def test_pr3_closure_captures_the_file_kettle_reads(self):
        for separator in ('\u2028', '\v', '\x85'):
            with self.subTest(separator=repr(separator)):
                value = f'wallpaper.png{separator}background_image = wallpaper.png'
                (self.root / value).write_bytes(b'intended image')
                closure = self.capture({'kettle': f'background-image = {value}'})
                snapshot = closure.local['kettle']['mapping'][0]['snapshot']
                self.assertEqual(Path(snapshot).read_bytes(), b'intended image')
                generated = closure.local['kettle']['generated']
                images = [value for _, key, value in standing.config_entries(generated) if key == 'background-image']
                self.assertEqual(images, [snapshot])

    def test_pr3_launch_uses_the_canonical_directory_the_closure_seals(self):
        from unittest import mock
        for name in ('a', 'b'):
            (self.root / name).mkdir()
        alias = self.root / 'current'
        alias.symlink_to(self.root / 'a', target_is_directory=True)
        out_dir = standing.claim_out_dir(alias / 'run')
        self.assertEqual(out_dir, self.root / 'a' / 'run')
        with standing.config_work_directory(out_dir) as work:
            standing.write_configs(work, {'kettle': ''})
            closure = standing.ConfigClosure(work, {'kettle': ''}, self.root)
            runner = standing.Runner({'launch': Path('/fixture-launch'), 'stamp': Path('/fixture-stamp')},
                                     work, {'kettle': '/fixture-kettle'})
            runner.config_closure = closure
            # Retarget the alias at a copy whose config names an uncaptured image.
            shutil.copytree(self.root / 'a' / 'run', self.root / 'b' / 'run')
            alternate = self.root / 'b' / 'run' / 'private-config' / 'kettle.config'
            alternate.chmod(0o600)
            alternate.write_text('background-image = uncaptured.png\n')
            alias.unlink()
            alias.symlink_to(self.root / 'b', target_is_directory=True)
            closure.check()
            with mock.patch.object(standing.subprocess, 'Popen', return_value=mock.Mock()) as spawned:
                runner.launch('kettle', 'true', 1)
            argv = spawned.call_args.args[0]
            launched = Path(argv[argv.index('--config') + 1])
            self.assertEqual(launched, closure.work / 'kettle.config')
            self.assertEqual(launched.read_text(), closure.local['kettle']['generated'])

    def test_pr3_persistent_inputs_reject_every_parser_true_alias(self):
        for key in ('restore-session', 'always_split_with_profile'):
            for value in ('true', 'on', 'yes', '1', 'enabled', 'enable', 'y', 'ENABLED', '" true "', "' enabled '"):
                with self.subTest(key=key, value=value), self.assertRaises(standing.ConfigClosureError):
                    self.capture({'kettle': key + ' = ' + value})
            for value in ('false', 'off', 'no', '0', 'disabled', 'disable', 'n'):
                self.capture({'kettle': key + ' = ' + value})

    def test_pr3_unresolved_environment_and_unknown_file_type_refuse(self):
        # Even an existing literal $HOME filename cannot certify an unresolved
        # environment reference as an ordinary declared asset.
        (self.root / '$HOME').mkdir()
        (self.root / '$HOME/wallpaper.png').write_bytes(b'literal-dollar')
        for text in ('background-image = $HOME/wallpaper.png', 'font-file = private-font.ttf',
                     'theme-file = private-theme', 'record-dir = recordings', 'shell = /bin/sh',
                     'env = HOME=/private/sentinel', 'trigger = x :: read-file', 'lua-script = input.lua'):
            with self.subTest(text=text), self.assertRaises(standing.ConfigClosureError):
                self.capture({'kettle': text})
        self.capture({'kettle': 'font-family = Family/Name\nhttp-proxy = https://example.invalid\nbackground-image = '})
        self.assertEqual(standing.CONFIG_FILE_KEYS, {'background-image': 'asset', 'record-dir': 'output-directory'})

    def test_pr3_symlink_capture_retarget_dangling_directory_fifo_unreadable(self):
        from unittest.mock import patch
        link = self.root / 'link.png'
        link.symlink_to(self.source)
        closure = self.capture({'kettle': 'background-image = link.png'})
        self.assertEqual(closure.public['kettle']['assets'][0]['sha256'], __import__('hashlib').sha256(b'wallpaper-one').hexdigest())
        second = self.root / 'second.png'
        second.write_bytes(b'two')
        real_open = standing.os.open
        def retarget(path, flags, *args, **kw):
            fd = real_open(path, flags, *args, **kw)
            link.unlink()
            link.symlink_to(second)
            return fd
        with patch.object(standing.os, 'open', side_effect=retarget), self.assertRaises(standing.ConfigClosureError):
            standing.config_file_bytes(link, 100, follow=True)
        dangling = self.root / 'dangling'
        dangling.symlink_to(self.root / 'absent')
        fifo = self.root / 'fifo'
        os.mkfifo(fifo)
        unreadable = self.root / 'unreadable'
        unreadable.write_bytes(b'no')
        unreadable.chmod(0)
        self.addCleanup(unreadable.chmod, 0o600)
        for source in (dangling, self.root, fifo, unreadable):
            with self.subTest(kind=source.name), patch.object(standing.os, 'open', wraps=real_open) as opened:
                with self.assertRaises(standing.ConfigClosureError):
                    standing.config_file_bytes(source, 100, follow=True)
                opened.assert_not_called()
        with patch.object(standing.os, 'open', side_effect=PermissionError('private sentinel')):
            with self.assertRaisesRegex(standing.ConfigClosureError, '^config closure: reference cannot be captured$'):
                standing.config_file_bytes(self.source, 100, follow=True)

    def test_pr3_unstable_reads_and_size_bounds(self):
        from unittest.mock import patch
        real = standing.os.fstat
        count = 0
        def changed(fd):
            nonlocal count
            count += 1
            if count == 2:
                self.source.write_bytes(b'changed-during-read')
            return real(fd)
        with patch.object(standing.os, 'fstat', side_effect=changed), self.assertRaises(standing.ConfigClosureError):
            standing.config_file_bytes(self.source, 100, follow=True)
        with self.assertRaises(standing.ConfigClosureError):
            standing.config_file_bytes(self.source, 2, follow=True)
        with self.assertRaises(standing.ConfigClosureError):
            standing.config_entries('x' * (standing.CONFIG_MAX_BYTES + 1))

    def test_pr3_include_cycle_and_undeclared_init_lua_refuse(self):
        # Neither spelling is supported by Kettle. Do not invent include semantics.
        (self.root / 'one').write_text('include = two\n')
        (self.root / 'two').write_text('include = one\n')
        with self.assertRaises(standing.ConfigClosureError):
            self.capture({'kettle': 'include = one'})
        for directory in ('xdg/kettle', '.'):
            closure = self.capture()
            script = closure.work / directory / 'init.lua'
            script.write_text('return {}')
            with self.assertRaisesRegex(standing.ConfigClosureError, 'undeclared'):
                closure.check()

    def test_kettle_runtime_spool_is_state_not_configuration(self):
        # Kettle 4.8.0 creates both files in its config directory at startup;
        # the first live pilot refused every row on them.
        closure = self.capture()
        kettle = closure.work / 'xdg/kettle'
        for name in ('remote.cmd', 'remote.cmd.lock'):
            (kettle / name).write_bytes(b'')
        closure.check()
        (kettle / 'remote.cmd').write_text('send-text-json "x"\n')
        with self.assertRaisesRegex(standing.ConfigClosureError, '^config closure: remote command spool not empty$'):
            closure.check()
        (kettle / 'remote.cmd').unlink()
        (kettle / 'remote.cmd').symlink_to(kettle / 'remote.cmd.lock')
        with self.assertRaisesRegex(standing.ConfigClosureError, 'not empty'):
            closure.check()
        (kettle / 'remote.cmd').unlink()
        (kettle / 'remote.cmd').mkdir()
        with self.assertRaisesRegex(standing.ConfigClosureError, 'not empty'):
            closure.check()
        (kettle / 'remote.cmd').rmdir()
        # Only these names, only in Kettle's directory.
        (kettle / 'session.json').write_bytes(b'')
        with self.assertRaisesRegex(standing.ConfigClosureError, 'undeclared'):
            closure.check()
        (kettle / 'session.json').unlink()
        closure.check()
        (closure.work / 'xdg/ghostty/remote.cmd').write_bytes(b'')
        with self.assertRaisesRegex(standing.ConfigClosureError, 'undeclared'):
            closure.check()

    def test_peer_config_directories_may_exist_only_empty(self):
        # kitty creates xdg/kitty even with --config NONE; the first live
        # sweep refused every kitty row on it.
        closure = self.capture()
        for name in ('kitty', 'wezterm', 'alacritty'):
            (closure.work / 'xdg' / name).mkdir()
        closure.check()
        (closure.work / 'xdg/kitty/kitty.conf').write_text('background #ff0000\n')
        with self.assertRaisesRegex(standing.ConfigClosureError, '^config closure: undeclared config root$'):
            closure.check()
        (closure.work / 'xdg/kitty/kitty.conf').unlink()
        closure.check()
        (closure.work / 'xdg/wezterm').rmdir()
        (closure.work / 'xdg/wezterm').write_text('')
        with self.assertRaisesRegex(standing.ConfigClosureError, 'undeclared config root'):
            closure.check()
        (closure.work / 'xdg/wezterm').unlink()
        (closure.work / 'xdg/wezterm').symlink_to(closure.work / 'xdg/kitty')
        with self.assertRaisesRegex(standing.ConfigClosureError, 'undeclared config root'):
            closure.check()
        (closure.work / 'xdg/wezterm').unlink()
        (closure.work / 'xdg/fish').mkdir()
        with self.assertRaisesRegex(standing.ConfigClosureError, 'undeclared config root'):
            closure.check()
        (closure.work / 'xdg/fish').rmdir()
        closure.check()

    def test_pr3_source_mutation_cannot_change_consumed_bytes(self):
        closure = self.capture()
        snapshot = Path(closure.local['kettle']['mapping'][0]['snapshot'])
        self.source.write_bytes(b'changed-after-capture')
        closure.check()
        self.assertEqual(snapshot.read_bytes(), b'wallpaper-one')
        self.assertEqual(len(closure.source_changes()), 1)
        self.assertIn(str(snapshot), (closure.work / 'kettle.config').read_text())
        self.assertEqual(snapshot.stat().st_mode & 0o777, 0o400)
        # The campaign retains these consumed bytes after its context exits.
        output = self.root / 'output'
        output.mkdir()
        with standing.config_work_directory(output) as work:
            standing.write_configs(work, {'kettle': 'background-image = wallpaper.png'})
            retained = standing.ConfigClosure(work, {'kettle': ''}, self.root)
            captured = Path(retained.local['kettle']['mapping'][0]['snapshot'])
        self.assertTrue(captured.is_file())

    def test_pr3_public_json_markdown_refusals_private_manifest(self):
        sentinel_home = self.root / 'sentinel-home'
        sentinel_home.mkdir()
        secret = sentinel_home / 'private-owner@example.invalid Signing Identity.png'
        secret.write_bytes(b'private-content')
        closure = self.capture({'kettle-a': '', 'kettle-b': 'background-image = ' + str(secret)})
        folder = self.session(closure, 'public')
        result = _json.loads((folder / 'results.json').read_text())
        public = (folder / 'results.json').read_text() + standing.summarize(result, result['terminals'], True)
        missing = 'background-image = ' + str(secret) + '-missing'
        try:
            self.capture({'kettle': missing})
        except standing.ConfigClosureError as error:
            public += str(error)
        for sentinel in (str(sentinel_home), 'private-owner@example.invalid', 'Signing Identity'):
            self.assertNotIn(sentinel, public)
            self.assertIn(sentinel, standing.dumps(closure.local))
        manifest = self.root / 'local-manifest.json'
        standing.private_json(manifest, closure.local)
        self.assertEqual(manifest.stat().st_mode & 0o777, 0o600)
        # Public report loading never opens the adjacent manifest.
        (folder / 'local-manifest.json').write_text('not public JSON')
        standing.load_session(folder)

    def test_pr3_campaign_mutation_keeps_raw_and_prevents_countability(self):
        for role in ('config', 'asset'):
            with self.subTest(role=role):
                closure = self.capture()
                results = {'meta': {'complete': False, 'refusals': [], 'countable': False,
                                    'rounds': {'idle': 1}}, 'workloads': {'idle': {'kettle': [{'footprint_mib': 10.}]}}}
                recorder = standing.Recorder(self.root / f'{role}-results.json', results)
                target = closure.work / 'kettle.config' if role == 'config' else next(closure.assets.iterdir())
                def collect():
                    target.chmod(0o600)
                    target.write_bytes(b'changed')
                    return {'footprint_mib': 12.}
                with self.assertRaisesRegex(SystemExit, 'campaign inputs changed'):
                    standing.config_campaign_row(closure, results, recorder, collect)
                raw = _json.loads(recorder.path.read_text())
                self.assertEqual(raw['workloads']['idle']['kettle'], [{'footprint_mib': 10.}])
                self.assertEqual(raw['config_invalid_rows'][0]['footprint_mib'], 12.)
                raw['meta']['complete'] = True
                self.assertFalse(standing.session_countable(raw['meta']))
                self.assertFalse(any(standing.workload_countable(raw, raw['meta']).values()))
                from unittest.mock import Mock
                subsequent = Mock()
                with self.assertRaises(SystemExit):
                    standing.config_campaign_row(closure, results, recorder, subsequent)
                subsequent.assert_not_called()

    def test_pr3_peer_layouts_and_launch_roots_are_isolated(self):
        from unittest.mock import patch, Mock
        closure = self.capture()
        env = closure.launch_environment({'HOME': 'preserved-home', 'XDG_CONFIG_HOME': 'user-config',
                     'GHOSTTY_CONFIG_DIR': 'user-ghostty', 'WEZTERM_CONFIG_FILE': 'user.lua', 'KITTY_CONFIG_DIRECTORY': 'user-kitty'})
        self.assertEqual(env['XDG_CONFIG_HOME'], str(closure.xdg))
        self.assertEqual(env['HOME'], 'preserved-home')
        self.assertNotIn('WEZTERM_CONFIG_FILE', env)
        self.assertNotIn('GHOSTTY_CONFIG_DIR', env)
        self.assertNotIn('KITTY_CONFIG_DIRECTORY', env)
        for name, flag, value in (('kitty', '--config', 'NONE'), ('alacritty', '--config-file', '/dev/null')):
            argv = standing.terminal_argv(name, closure.work / 'payload', closure.work, {})
            self.assertEqual(argv[argv.index(flag) + 1], value)
        self.assertIn('-n', standing.terminal_argv('wezterm', closure.work / 'payload', closure.work, {}))
        ghostty = standing.terminal_argv('ghostty', closure.work / 'payload', closure.work, {})
        self.assertEqual(ghostty[1], 'XDG_CONFIG_HOME=' + str(closure.xdg))
        runner = standing.Runner({'launch': Path('/fixture-launch'), 'stamp': Path('/fixture-stamp')}, closure.work, {'kettle': '/fixture-kettle'})
        runner.config_closure = closure
        with patch.object(standing.subprocess, 'Popen', return_value=Mock()) as spawn:
            runner.launch('kettle', 'true', 1)
        self.assertEqual(spawn.call_args.kwargs['cwd'], closure.cwd)
        self.assertEqual(spawn.call_args.kwargs['env']['XDG_CONFIG_HOME'], str(closure.xdg))
        (closure.xdg / 'ghostty' / 'extra').write_text('config-file = cycle')
        with self.assertRaises(standing.ConfigClosureError):
            closure.check()

    def test_pr3_counted_campaign_brackets_actual_rows(self):
        # Mock the app/environment at the entry point. A mutation in the real
        # collection callback must retain earlier data and abort the campaign.
        import contextlib
        import io
        from unittest.mock import patch, Mock
        output = self.root / 'main-campaign'
        args = ['standing', '--kettle', '/fixture/A.app/Contents/MacOS/kettle',
                '--kettle-b-config', 'background-image = ' + str(self.source),
                '--no-build', '--workloads', 'idle', '--rounds', '2', '--warmup', '0',
                '--fd-limit', '0', '--out-dir', str(output)]
        state = {'display': {}, 'power': {}, 'low_power': False, 'load': [0., 0.], 'procs': []}
        completed = []
        def idle(runner, name, *args):
            row = {'footprint_mib': 10., 'cpu_percent': 0., 'wakeups_per_second': 1.}
            completed.append(name)
            if len(completed) == 2:
                target = next(runner.config_closure.assets.iterdir())
                target.chmod(0o600)
                target.write_bytes(b'changed-in-actual-row')
            return row
        with contextlib.ExitStack() as stack:
            for target, value in (('sys.argv', args), ('sys.platform', 'darwin')):
                stack.enter_context(patch(target, value))
            for name, value in (('require_bundles', None), ('build_probes', {}),
                                ('collect_preflight', state), ('preflight_refusals', []),
                                ('command', 'fixture'), ('host_terminal_of', None),
                                ('harness_revision', {}), ('terminal_identity', ({'sha256': 'same'}, {}))):
                stack.enter_context(patch.object(standing, name, return_value=value))
            stack.enter_context(patch.object(standing.subprocess, 'Popen', return_value=Mock()))
            stack.enter_context(patch.object(standing.Runner, 'idle', idle))
            stack.enter_context(patch.object(standing.Runner, 'stop_current'))
            stack.enter_context(patch.object(standing.time, 'sleep'))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            with self.assertRaisesRegex(SystemExit, 'campaign inputs changed'):
                standing.main()
        raw = _json.loads((output / 'results.json').read_text())
        self.assertEqual(len(raw['workloads']['idle']['kettle-a']), 1)
        self.assertEqual(len(completed), 2)
        self.assertFalse(raw['meta']['complete'])
        self.assertFalse(standing.session_countable(raw['meta']))
        self.assertIn('config_closures', raw['meta'])
        self.assertEqual(raw['config_invalid_rows'][0]['footprint_mib'], 10.)
        local = _json.loads((output / 'local-manifest.json').read_text())
        self.assertIn('config_closure', local)
        self.assertTrue((output / 'private-config/assets').is_dir())

    def test_rebased_optional_campaign_brackets_actual_rows(self):
        for workload, validation in (('output-memory', False), ('blink-window', False), ('blink-window', True)):
            with self.subTest(workload=workload, validation=validation):
                self.optional_campaign(workload, validation)

    def optional_campaign(self, workload, validation):
        # Mock the app/environment at the entry point. A mutation in the real
        # collection callback must retain earlier data and abort the campaign.
        import contextlib
        import io
        from unittest.mock import patch, Mock
        output = self.root / ('optional-' + workload + str(validation))
        args = ['standing', '--kettle', '/fixture/A.app/Contents/MacOS/kettle',
                '--kettle-b-config', 'background-image = ' + str(self.source),
                '--no-build', '--workloads', workload, '--rounds', '2', '--warmup', '0',
                '--fd-limit', '0', '--out-dir', str(output)]
        if validation:
            args += ['--blink-validate-only', '--blink-cursor-rect', '1,2,3,4', '--blink-shape', 'block', '--blink-timeout', '10']
        state = {'display': {}, 'power': {}, 'low_power': False, 'load': [0., 0.], 'procs': []}
        completed = []
        def collect(runner, name, workload, options, keep, setup):
            self.assertIn(workload, ("output-memory", "blink-window"))
            self.assertEqual(runner.work, output.resolve() / "private-config")
            self.assertTrue(str(keep).startswith(str(output.resolve())))
            self.assertEqual(setup["config_sha256"], standing.hashlib.sha256(standing.dumps(runner.config_closure.public[name]).encode()).hexdigest())
            runner.observation_context = runner.work / "hc-launch.json"
            runner.launch(name, "true", 1)
            self.assertEqual(spawn.call_args.kwargs["env"]["KETTLE_HC_LAUNCH_CONTEXT"], str(runner.observation_context))
            self.assertEqual(spawn.call_args.kwargs["cwd"], runner.config_closure.cwd)
            self.assertEqual(spawn.call_args.kwargs["env"]["XDG_CONFIG_HOME"], str(runner.config_closure.xdg))
            row = {'footprint_mib': 10., 'cpu_percent': 0., 'wakeups_per_second': 1.}
            completed.append(name)
            if len(completed) == 2:
                target = next(runner.config_closure.assets.iterdir())
                target.chmod(0o600)
                target.write_bytes(b'changed-in-actual-row')
            return row
        with contextlib.ExitStack() as stack:
            for target, value in (('sys.argv', args), ('sys.platform', 'darwin')):
                stack.enter_context(patch(target, value))
            for name, value in (('require_bundles', None), ('build_probes', {'launch': Path('/fixture-launch'), 'stamp': Path('/fixture-stamp')}),
                                ('collect_preflight', state), ('preflight_refusals', []),
                                ('command', 'fixture'), ('display_mode', {}), ('host_terminal_of', None),
                                ('harness_revision', {}), ('terminal_identity', ({'sha256': 'same'}, {}))):
                stack.enter_context(patch.object(standing, name, return_value=value))
            stack.enter_context(patch.object(standing, 'probe_lock', return_value=contextlib.nullcontext()))
            stack.enter_context(patch.object(standing, 'validate_latency_probe', return_value={}))
            spawn = stack.enter_context(patch.object(standing.subprocess, 'Popen', return_value=Mock()))
            stack.enter_context(patch.object(standing.hc, 'build_helpers', return_value={}))
            stack.enter_context(patch.object(standing, 'file_sha256', return_value='fixture'))
            stack.enter_context(patch.object(standing.hc, 'collect', collect))
            stack.enter_context(patch.object(standing.Runner, 'stop_current'))
            stack.enter_context(patch.object(standing.time, 'sleep'))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            with self.assertRaisesRegex(SystemExit, 'campaign inputs changed'):
                standing.main()
        raw = _json.loads((output / 'results.json').read_text())
        self.assertEqual(len(raw['workloads'][workload]['kettle-a']), 1)
        self.assertEqual(len(completed), 2)
        self.assertFalse(raw['meta']['complete'])
        self.assertFalse(standing.session_countable(raw['meta']))
        self.assertIn('config_closures', raw['meta'])
        self.assertEqual(raw['config_invalid_rows'][0]['footprint_mib'], 10.)
        local = _json.loads((output / 'local-manifest.json').read_text())
        self.assertIn('config_closure', local)
        self.assertTrue((output / 'private-config/assets').is_dir())

    def test_pr3_existing_snapshot_must_match_addressed_bytes(self):
        import hashlib
        work = self.root / 'already-captured'
        work.mkdir()
        standing.write_configs(work, {'kettle': 'background-image = wallpaper.png'})
        assets = work / 'assets'
        assets.mkdir()
        (assets / hashlib.sha256(b'wallpaper-one').hexdigest()).write_bytes(b'wrong-existing-bytes')
        with self.assertRaisesRegex(standing.ConfigClosureError, 'captured asset changed'):
            standing.ConfigClosure(work, {'kettle': ''}, self.root)

    def test_pr3_variants_capture_assets_and_reject_private_role_names(self):
        name = standing.variant_name('wallpaper')
        closure = self.capture({'kettle': '', name: 'background_image = wallpaper.png',
                                'kettle-opaque': standing.KETTLE_OPAQUE})
        self.assertEqual(closure.public['kettle']['assets'], [])
        self.assertEqual(closure.public['kettle-opaque']['assets'], [])
        self.assertEqual(len(closure.public[name]['assets']), 1)
        for label in ('../outside', 'private-owner@example.invalid', 'Signing Identity'):
            with self.assertRaisesRegex(ValueError, '^--kettle-variant requires a nonreserved ASCII role name$'):
                standing.variant_name(label)

    def test_pr3_registry_matches_pinned_parser(self):
        import re
        parser = HERE.parents[1] / 'base/crates/kettle-config/src/lib.rs' if SNAPSHOT_LAYOUT else HERE.parents[1] / 'crates/kettle-config/src/lib.rs'
        if not parser.exists():
            self.skipTest('registry audit needs pinned config-parser source')
        text = parser.read_text().split('pub fn parse_collect(text: &str)')[1].split('\n    }\n')[0]
        text = re.sub(r'//[^\n]*', '', text)
        keys = set()
        for match in re.finditer(r'^ {16}("[a-z0-9_-]+"(?:\s*\|\s*"[a-z0-9_-]+")*)\s*=>', text, re.M):
            keys.update(k.replace('_', '-') for k in re.findall(r'"([a-z0-9_-]+)"', match[1]))
        self.assertEqual(standing.CONFIG_KEYS, keys)
        self.assertIn('cfg.background_image = e.value.trim().to_string()', text)
        self.assertIn('Some(PathBuf::from(dir))', text)


def legacy_output_bytes(module, case, root):
    """Replay fixed whole-file outputs through the repository's writers."""
    import copy
    result = copy.deepcopy(case["input"])
    folder = root / "legacy"
    folder.mkdir()
    (folder / "results.json").write_text(module.dumps(result))
    # Schema 1 dates a session by results.json's mtime in local time. Pin
    # local noon on the fixtures' date so the bytes match in every time zone
    # and on every day.
    noon = time.mktime((2026, 9, 30, 12, 0, 0, 0, 0, -1))
    os.utime(folder / "results.json", (noon, noon))
    for name, data in case.get("dat", {}).items():
        (folder / name).write_text(data)
    names = result["terminals"]
    ab = names == ["kettle-a", "kettle-b"]
    summary = module.summarize(result, names, ab) + "\n"
    combined = module.combine([folder])
    markdown = combined.pop("markdown")
    return {"results.json": (folder / "results.json").read_bytes(),
            "summary.md": summary.encode(), "combined.json": module.dumps(combined).encode(),
            "combined.md": markdown.encode()}


class LegacyOutputBytes(unittest.TestCase):
    def test_existing_whole_files_match_pinned_schema_1_2_3_outputs(self):
        import tempfile
        fixtures = _json.loads((HERE / "macos-standing" / "legacy-output-bytes.fixture").read_text())
        for case in fixtures:
            with self.subTest(case=case["name"]), tempfile.TemporaryDirectory() as tmp:
                output = legacy_output_bytes(standing, case, Path(tmp))
                for name, data in output.items():
                    # unittest checks run under -O too; no Python assert.
                    self.assertEqual(data, case["expected"][name].encode(), f"whole file {name}")


class NativeEvidence(unittest.TestCase):
    @staticmethod
    def row(cols=120, rows=36):
        geometry = {"cols": cols, "rows": rows, "pixel_width": 960, "pixel_height": 600}
        return {"cols": cols, "rows": rows, "start_cols": cols, "start_rows": rows,
                "launch_id": "launch-1", "pane_id": "pane-1", "started_ns": 10,
                "child_observed_ns": 100,
                "native_pty": {"version": "native_pty_v1", "clock": "CLOCK_UPTIME_RAW",
                    "launch_id": "launch-1", "pane_id": "pane-1", "complete": True,
                    "dropped": 0, "overflow": False, "recording_start_ns": 11,
                    "created_ns": 12, "initial_stage": "after_create_before_correction",
                    "initial": {**geometry, "t_ns": 13}, "recording_end_ns": 2_000_000_011,
                    "events": [], "event_count": 0,
                    "child_observation": {"child_observed_ns": 100, "start_cols": cols,
                                          "start_rows": rows, "sigwinch_count": 0},
                    "final": {**geometry, "t_ns": 2_000_000_011}}}

    def test_child_rejects_initial_mismatch_and_missing_observation(self):
        row = self.row(); row["start_cols"] = 119
        self.assertEqual(standing.startup_grid_evidence(row)["state"], "supported")
        self.assertEqual(standing.startup_grid_evidence(row, "child")["state"], "incomplete")
        row.pop("start_cols")
        self.assertEqual(standing.startup_grid_evidence(row, "child")["state"], "incomplete")
        self.assertEqual(standing.startup_grid_evidence(self.row(), "child")["state"], "supported")

    def test_delayed_child_rejects_native_initial_correction_for_both_sizes(self):
        for cols, rows, initial_cols, initial_rows in ((100, 30, 99, 30), (120, 36, 119, 36)):
            row = self.row(cols, rows)
            row["native_pty"]["initial"].update(cols=initial_cols, rows=initial_rows)
            corrected = dict(row["native_pty"]["final"])
            row["native_pty"].update(event_count=1, events=[{
                "seq": 1, "t_ns": 50, "launch_id": "launch-1", "pane_id": "pane-1",
                "requested": corrected, "observed": corrected, "outcome": "ok",
                "reason": "window", "native_error": None, "signal_sent": True}])
            self.assertEqual(standing.startup_grid_evidence(row, "child", cols, rows)["state"], "supported")
            self.assertEqual(standing.startup_grid_evidence(row, "native", cols, rows)["state"], "incomplete")

    def test_native_requires_two_seconds_of_actual_recording(self):
        row = self.row()
        row["native_pty"].update(recording_start_ns=1_999_000_011, created_ns=1_999_000_012)
        row["native_pty"]["initial"]["t_ns"] = 1_999_000_013
        row["child_observed_ns"] = 1_999_000_014
        row["native_pty"]["child_observation"]["child_observed_ns"] = row["child_observed_ns"]
        self.assertEqual(standing.startup_grid_evidence(row, "native")["state"], "incomplete")

    def test_native_complete_noop_and_legacy_absence(self):
        row = self.row()
        self.assertEqual(standing.startup_grid_evidence(row, "native")["state"], "supported")
        initial = row["native_pty"]["initial"]
        row["native_pty"]["events"] = [{"seq": 1, "t_ns": 200, "launch_id": "launch-1", "pane_id": "pane-1",
            "requested": initial, "observed": initial, "outcome": "noop", "reason": "window",
            "native_error": None, "signal_sent": False}]
        row["native_pty"]["event_count"] = 1
        self.assertEqual(standing.startup_grid_evidence(row, "native")["state"], "supported")
        row.pop("native_pty")
        self.assertEqual(standing.startup_grid_evidence(row, "native")["state"], "unavailable")

    def test_native_missing_failed_dropped_wrong_pane_overflow_never_pass(self):
        import copy
        complete = self.row()["native_pty"]
        event = {"seq": 1, "t_ns": 200, "launch_id": "launch-1", "pane_id": "pane-1",
                 "requested": complete["initial"], "observed": complete["initial"],
                 "outcome": "noop", "reason": "window", "native_error": None, "signal_sent": False}
        for field in ("recording_start_ns", "recording_end_ns", "initial", "final", "child_observation", "event_count", "created_ns", "initial_stage"):
            with self.subTest(missing=field):
                row = self.row(); row["native_pty"].pop(field)
                self.assertNotEqual(standing.startup_grid_evidence(row, "native")["state"], "supported")
        for field, value in (("dropped", 1), ("overflow", True), ("complete", False),
                             ("pane_id", "other"), ("clock", "CLOCK_MONOTONIC"),
                             ("event_count", 1), ("recording_end_ns", 200)):
            with self.subTest(field=field):
                row = self.row(); row["native_pty"][field] = value
                self.assertNotEqual(standing.startup_grid_evidence(row, "native")["state"], "supported")
        for field, value in (("outcome", "error"), ("seq", 2), ("pane_id", "other"),
                             ("signal_sent", True), ("native_error", 5), ("observed", None),
                             ("t_ns", 12), ("reason", "unrecorded")):
            row = self.row(); changed = copy.deepcopy(event); changed[field] = value
            row["native_pty"].update(events=[changed], event_count=1)
            self.assertNotEqual(standing.startup_grid_evidence(row, "native")["state"], "supported")
        row = self.row(); changed = copy.deepcopy(event); changed["requested"]["pixel_width"] += 1
        row["native_pty"].update(events=[changed], event_count=1)
        self.assertNotEqual(standing.startup_grid_evidence(row, "native")["state"], "supported")

    def test_native_initial_grid_alone_refuses(self):
        # Only the initial-grid check can refuse this: wrong initial and final
        # native grids, no events, and a correct child grid.
        row = self.row()
        for record in ("initial", "final"):
            row["native_pty"][record]["cols"] = 119
        self.assertEqual(standing.startup_grid_evidence(row, "native"),
                         {"state": "incomplete", "reason": "wrong initial native geometry", "provisional": True})

    def test_native_geometry_is_bounded_by_winsize(self):
        for width, state in ((65535, "supported"), (65536, "incomplete"), (10**400, "incomplete")):
            with self.subTest(width=width):
                row = self.row()
                for record in ("initial", "final"):
                    row["native_pty"][record]["pixel_width"] = width
                self.assertEqual(standing.startup_grid_evidence(row, "native")["state"], state)

    def test_malformed_phase_or_path_records_invalidate_their_intervals(self):
        good = self.phases({"window_created": 10, "gpu_ready": 20})
        self.assertEqual(standing.startup_phase_evidence(good, None)["durations"]["gpu_init_ms"], 10.)
        for record in ("startup phase=gpu_ready t_ns=-1", "startup phase=gpu_ready t_ns=oops",
                       "startup phase=gpu_ready",
                       "startup phase=gpu_ready t_ns=" + "9" * 5000 + " since_main_ms=0.00 thread=main"):
            with self.subTest(record=record[:40]):
                report = standing.startup_phase_evidence(good + "\n" + record, None)
                self.assertIsNone(report["durations"]["gpu_init_ms"])
                self.assertEqual(report["metric_validity"]["gpu_init_ms"]["state"], "unavailable")
        fonts = self.phases({"fonts_join_start": 10, "fonts_joined": 20})
        self.assertEqual(standing.startup_phase_evidence(fonts, None)["durations"]["fonts_join_wait_ms"], 10.)
        broken = standing.startup_phase_evidence(fonts + "\nstartup path=garbage", None)
        self.assertIsNone(broken["durations"]["fonts_join_wait_ms"])
        self.assertIsNone(broken["startup_path"])

    @staticmethod
    def phases(stamps, path="resumed_early"):
        return "\n".join(f"startup phase={name} t_ns={int(t * 1e6)} since_main_ms=0.00 thread={'fonts' if name in ('fonts_ready', 'fonts_enumerated') else 'main'}"
                         for name, t in stamps.items()) + f"\nstartup path={path}"

    def test_s2_preserves_raw_thread_path_and_leaves_unemitted_fields_unavailable(self):
        text = (HERE / "macos-standing" / "startup-font-phases.fixture").read_text()
        report = standing.startup_phase_evidence(text, 900_000_000)
        self.assertEqual(report["startup_stamps_ns"]["fonts_join_start"], 1_090_000_000)
        self.assertEqual(report["startup_phase_threads"]["fonts_ready"], "fonts")
        self.assertEqual(report["startup_path"], "resumed_early")
        self.assertEqual(report["durations"]["fonts_join_wait_ms"], 10.)
        self.assertIsNone(report["monitor_match"])
        self.assertIsNone(report["reported_fonts_wait_ms"])
        self.assertEqual(report["phase_ms"]["first_frame"], 240.)

    def test_font_and_gpu_durations_derive_per_round_before_medians(self):
        # medians(end)-medians(begin)=5; median(end-begin)=10.
        rounds = [standing.startup_phase_evidence(self.phases({"window_created": a, "gpu_ready": b,
                   "fonts_join_start": a, "fonts_joined": b}), None)
                  for a, b in ((1, 101), (101, 106), (201, 211))]
        summary = standing.startup_duration_summary(rounds)
        for field in ("fonts_join_wait_ms", "gpu_init_ms"):
            self.assertEqual(summary[field]["values"], [100., 5., 10.])
            self.assertEqual(summary[field]["median"], 10.)
            self.assertEqual(summary[field]["max"], 100.)
            self.assertAlmostEqual(summary[field]["p95"], 91.)
        signed = standing.startup_phase_evidence(self.phases({"fonts_ready": 20, "resumed": 10, "config_loaded": 30}), None)
        self.assertEqual(signed["durations"]["fonts_ready_to_resumed_ms"], -10.)
        self.assertEqual(signed["durations"]["fonts_ready_after_config_ms"], -10.)

    def test_missing_unordered_conflicting_and_malformed_endpoints_are_unavailable(self):
        for stamps in ({"window_created": 10}, {"window_created": 20, "gpu_ready": 10}):
            report = standing.startup_phase_evidence(self.phases(stamps), None)
            self.assertIsNone(report["durations"]["gpu_init_ms"])
        text = self.phases({"window_created": 10, "gpu_ready": 20})
        conflict = text + "\nstartup phase=gpu_ready t_ns=30000000 since_main_ms=0.00 thread=main"
        report = standing.startup_phase_evidence(conflict, None)
        self.assertEqual(report["startup_stamps_ns"]["gpu_ready"], 20_000_000)
        self.assertIsNone(report["durations"]["gpu_init_ms"])
        self.assertEqual(report["duplicate_stamps"], 1)
        malformed = text + "\nstartup phase=gpu_ready t_ns=30000000 since_main_ms=0.00 thread=bogus"
        self.assertIsNone(standing.startup_phase_evidence(malformed, None)["durations"]["gpu_init_ms"])
        no_threads = self.phases({"fonts_join_start": 10, "fonts_joined": 11}).replace(" thread=main", "")
        self.assertIsNone(standing.startup_phase_evidence(no_threads, None)["durations"]["fonts_join_wait_ms"])

    def test_s1_identical_and_unknown_optional_stamps_are_diagnostic_only(self):
        s1 = StartupPhases.FIXTURE
        expected = {f"phase_{name}_ms": 100. + index * 10. for index, name in enumerate(
            ("main", "run_with", "event_loop_built", "config_loaded", "app_built", "pane_spawned", "resumed", "window_created", "gpu_ready", "window_revealed", "first_frame"))}
        expected["startup_path"] = "resumed_early"
        self.assertEqual(standing.parse_phases(s1, 900_000_000), expected)
        report = standing.startup_phase_evidence(s1 + "\nstartup phase=future_device t_ns=1210000000 since_main_ms=210.00 thread=main", 900_000_000)
        self.assertEqual(report["unknown_stamps_ns"], {"future_device": 1_210_000_000})
        self.assertNotIn("future_device", report["phase_ms"])
        self.assertNotIn("future_device", report["durations"])

    @staticmethod
    def smoke():
        frames = {"count": 0, "p50": None, "p95": None, "max": 0}
        state = {"renderer": "layer", "handoffs": 1, "exits": 0, "hides": 0, "exit_frame_us": frames}
        return {"contract": {"handoff_after_s": .6, "idle": {"span_s": 3.5, "peak_mib": 75., "wakeups_per_s": 0., "cpu_percent": .01},
                             **{key: dict(state) for key in ("handoff", "rested", "after_reload", "after_key")}}}

    def test_native_layer_aggregates_cannot_certify_actual_interval_or_geometry_reads(self):
        for span in (2., 3.5):
            data = self.smoke(); data["contract"]["idle"]["span_s"] = span
            report = standing.cursor_layer_evidence(data)
            self.assertEqual(report["state"], "unavailable")
            self.assertEqual(report["aggregates"]["idle"]["peak_mib"], 75.)
            self.assertIsNone(report["interval_peak_mib"])
            self.assertIsNone(report["geometry_polling_valid"])
            self.assertIsNone(report["acceptance"])
        self.assertEqual(standing.cursor_layer_evidence({})["state"], "unavailable")
        data = self.smoke(); data["contract"]["idle"]["peak_mib"] = float("nan")
        self.assertEqual(standing.cursor_layer_evidence(data)["state"], "malformed")

    def test_unagreed_traces_cannot_invent_emit_or_echo_joins_or_clock_savings(self):
        # The proposed design JSONL is not the actual private producer format.
        for text in ("", '{"phase":"snapshot","clock":"CLOCK_UPTIME_RAW"}',
                     '{"phase":"emit_pane_glyphs","echo_seq":1}\n' * 2,
                     '{"phase":"main_prepare","status":"skipped"}',
                     '{"clock":"CLOCK_MONOTONIC","phase":"upload"}'):
            report = standing.renderer_trace_evidence(text)
            self.assertEqual(report["state"], "unavailable")
            self.assertIsNone(report["echo_durations"])
            self.assertIsNone(report["acceptance"])

    def test_first_output_never_promotes_first_frame_or_key_output(self):
        report = standing.startup_phase_evidence(self.phases({"first_frame": 10}), 0)
        self.assertEqual(report["phase_ms"]["first_frame"], 10.)
        self.assertIsNone(report["first_output_ms"])
        self.assertEqual(report["first_output_capability"], "unavailable")

    def test_postprocessing_cli_launches_nothing_and_redacts_private_inputs(self):
        import argparse
        import io
        import contextlib
        import tempfile
        from unittest import mock
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "results.json"
            row = self.row(); row["owner"] = "/private/sentinel/person@example.test"
            path.write_text(json_dumps({"workloads": {"startup": {"kettle": [row]}}}))
            phase = Path(tmp) / "phase.log"; phase.write_text(self.phases({"gpu_ready": 10}))
            layer = Path(tmp) / "layer.json"; layer.write_text(json_dumps(self.smoke()))
            trace = Path(tmp) / "trace.log"; trace.write_text(row["owner"])
            args = argparse.Namespace(startup_input=[str(path)], startup_grid_policy="native",
                                      startup_phase_input=[str(phase)], startup_started_ns=None,
                                      native_layer_input=[str(layer)], trace_input=[str(trace)])
            out = io.StringIO()
            with mock.patch.object(standing.subprocess, "Popen", side_effect=AssertionError("spawn")), contextlib.redirect_stdout(out):
                self.assertEqual(standing.run_evidence_postprocessing(args), 0)
            report = _json.loads(out.getvalue())
            self.assertFalse(report["countable"])
            self.assertNotIn("sentinel", out.getvalue())
            self.assertNotIn("example.test", out.getvalue())
            with mock.patch.object(sys, "argv", [str(HERE / "macos-standing.py"), "--startup-input", str(path), "--startup-grid-policy", "child"]), mock.patch.object(standing, "build_probes", side_effect=AssertionError("build")), mock.patch.object(standing.subprocess, "Popen", side_effect=AssertionError("spawn")), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(standing.main(), 0)

    def test_oversized_native_numeric_input_is_malformed_without_a_traceback(self):
        import io
        import contextlib
        import tempfile
        from unittest import mock
        data = self.smoke(); data["contract"]["idle"]["peak_mib"] = 10**400
        self.assertEqual(standing.cursor_layer_evidence(data)["state"], "malformed")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "huge.json"; path.write_text(json_dumps(data))
            out = io.StringIO()
            with mock.patch.object(sys, "argv", [str(HERE / "macos-standing.py"), "--native-layer-input", str(path)]), contextlib.redirect_stdout(out):
                self.assertEqual(standing.main(), 0)
            self.assertEqual(_json.loads(out.getvalue())["reports"][0]["evidence"]["state"], "malformed")

    def test_diagnostic_json_rejects_duplicate_nonfinite_and_wrong_top_level(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.json"
            for text in ('{"a":1,"a":2}', '{"a":NaN}', '[]'):
                path.write_text(text)
                with self.assertRaises(ValueError):
                    standing.diagnostic_json(path)


class CursorLayerEvidence(unittest.TestCase):
    @staticmethod
    def fixture():
        return _json.loads((HERE / "macos-standing/cursor-layer.fixture").read_text())

    @staticmethod
    def reaggregate(contract):
        # The producer uses first/last cumulative counters and current footprint.
        samples = contract["samples"]
        a, b = samples[0], samples[-1]
        span = b["t"] - a["t"]
        contract["stamps"].update(measure_start=a["t"], measure_end=b["t"])
        contract["idle"] = dict(span_s=span, peak_mib=max(s["footprint_mib"] for s in samples),
            wakeups_per_s=(b["wakeups"]-a["wakeups"])/span,
            cpu_percent=(b["cpu_ns"]-a["cpu_ns"])/(span*1e9)*100)

    def refuse(self, data, reason, state="incomplete"):
        report = standing.cursor_layer_evidence(data)
        self.assertEqual(report["state"], state, report)
        self.assertEqual(report["reason"], reason)
        self.assertIsNone(report.get("acceptance"))

    def test_positive_exact_producer_format(self):
        data = self.fixture(); c = data["contract"]
        report = standing.cursor_layer_evidence(data)
        self.assertEqual(report["state"], "supported", report)
        self.assertEqual(report["acceptance"]["verdict"], "pass")
        self.assertEqual(report["acceptance"]["thresholds"],
            dict(peak_mib=80.,wakeups_per_s=.5,cpu_percent=.02))
        self.assertEqual(report["interval_peak_mib"], 75.)
        self.assertEqual(report["interval_median_mib"], 72.5)
        self.assertEqual(report["samples"], 8)
        self.assertAlmostEqual(report["span_s"], 3.514)
        self.assertAlmostEqual(report["wakeups_per_s"], 1/3.514)
        self.assertAlmostEqual(report["cpu_percent"], .01)
        # Only the window the samples span is certified, inside the allowed one.
        self.assertEqual(report["covered_interval"], [c["samples"][0]["t"], c["samples"][-1]["t"]])
        self.assertEqual(report["allowed_interval"], [c["stamps"]["handoff_seen"]+1.5, 112.001])
        self.assertTrue(report["geometry_polling_valid"])
        # Lifetime maxima and unrelated numeric fields cannot enter the peak.
        for row in c["samples"]: row["max_footprint_mib"] = 99999.
        c["idle"]["median_mib"] = 99999.
        data["private"] = "private-sentinel"
        self.assertEqual(standing.cursor_layer_evidence(data), report)

    def test_short_span(self):
        data=self.fixture();c=data["contract"];c["samples"]=c["samples"][:6]
        self.reaggregate(c)
        self.refuse(data, "idle sample span below 3 seconds")

    def test_gap(self):
        data=self.fixture();c=data["contract"];c["samples"].pop(1)
        self.reaggregate(c)
        self.refuse(data, "sample cadence or gap outside declared period")

    def test_a_sample_outside_the_window_or_out_of_order_breaks_the_cadence(self):
        # The endpoints are pinned to in-window stamps, so a stray or repeated
        # sample time always breaks the fixed 0.5 s cadence.
        for t in (102.100,112.002):
            data=self.fixture();c=data["contract"];c["samples"][3]["t"]=t
            with self.subTest(t=t):self.refuse(data,"sample cadence or gap outside declared period")
        data=self.fixture();c=data["contract"];c["samples"][2]["t"]=c["samples"][1]["t"]
        self.refuse(data,"sample cadence or gap outside declared period")

    def test_geometry_exits(self):
        data=self.fixture();data["contract"]["geometry_reads"][12]["exits"]+=1
        self.refuse(data,"geometry polling changed renderer or counters")

    def test_geometry_renderer(self):
        data=self.fixture();data["contract"]["geometry_reads"][12]["renderer"]="gpu"
        self.refuse(data,"geometry polling changed renderer or counters")

    def test_aggregate_mismatch(self):
        for field in ("span_s","peak_mib","wakeups_per_s","cpu_percent"):
            data=self.fixture();data["contract"]["idle"][field]+=.01
            with self.subTest(field=field):self.refuse(data,"idle aggregate mismatch")

    def test_missing_stamp(self):
        for field in self.fixture()["contract"]["stamps"]:
            data=self.fixture();del data["contract"]["stamps"][field]
            with self.subTest(field=field):self.refuse(data,"missing native layer stamp")

    def test_legacy(self):
        data=self.fixture()
        for field in ("clock","stamps","samples","geometry_reads"):del data["contract"][field]
        self.refuse(data,"legacy aggregate-only native layer smoke","unavailable")

    def test_declared_period(self):
        data=self.fixture();data["contract"]["stamps"]["sample_period_s"]=1.
        self.refuse(data,"unexpected native layer sample period","malformed")
        # Declaring a long period cannot hide dropped samples, here including
        # the 75 MiB peak.
        data=self.fixture();c=data["contract"];c["samples"]=[c["samples"][0],c["samples"][-1]]
        c["stamps"]["sample_period_s"]=3.514;c["idle"]["peak_mib"]=74.
        self.refuse(data,"unexpected native layer sample period","malformed")

    def test_implausible_footprint_never_certifies(self):
        data=self.fixture();c=data["contract"]
        for row in c["samples"]:row["footprint_mib"]=1e308
        c["idle"]["peak_mib"]=1e308
        self.refuse(data,"implausible native layer footprint","malformed")

    def test_exit_summaries_must_be_complete_and_ordered(self):
        data=self.fixture();del data["contract"]["handoff"]["exit_frame_us"]["p50"]
        self.refuse(data,"exit frame summaries incomplete or unordered")
        data=self.fixture();data["contract"]["after_key"]["exit_frame_us"]=dict(count=2,p50=999,p95=1,max=0)
        self.refuse(data,"exit frame summaries incomplete or unordered")

    def test_measurement_stamps_and_bounds(self):
        for fields,reason in [
            ({"reload_written":112.},"unordered native layer stamps"),
            ({"measure_start":102.},"measurement outside handoff-to-timeout interval"),
            ({"timeout_s":5.},"measurement outside handoff-to-timeout interval"),
            ({"measure_end":106.},"sample endpoints disagree with stamps"),
            ({"rest_read":112.1},"rest read precedes timeout and last edge"),
        ]:
            data=self.fixture();data["contract"]["stamps"].update(fields)
            with self.subTest(fields=fields):self.refuse(data,reason)

    def test_geometry_order_and_count(self):
        for mutate,reason in [
            (lambda c:c["geometry_reads"].pop(),"expected twenty geometry reads"),
            (lambda c:c["geometry_reads"][0].update(t=c["stamps"]["measure_end"]),
                "geometry polling overlaps measurement or is unordered"),
            (lambda c:c["geometry_reads"][12].update(t=c["geometry_reads"][11]["t"]),
                "geometry polling overlaps measurement or is unordered"),
            (lambda c:c["geometry_reads"][12].update(handoffs=2),
                "geometry polling changed renderer or counters"),
            (lambda c:c["geometry_reads"][12].update(hides=1),
                "geometry polling changed renderer or counters"),
        ]:
            data=self.fixture();mutate(data["contract"])
            with self.subTest(reason=reason):self.refuse(data,reason)

    def test_states(self):
        for label,fields,reason in [
            ("handoff",{"fallback":"failed"},"layer handoff or fallback state unproven"),
            ("rested",{"phase_on":False},"rested state inconsistent with timeout"),
            ("rested",{"next_edge_ms":100},"rested state inconsistent with timeout"),
            ("rested",{"exits":1},"rested state inconsistent with timeout"),
            ("after_reload",{"exits":0},"reload state did not exit once and hand off again"),
            ("after_reload",{"handoffs":1},"reload state did not exit once and hand off again"),
            ("after_key",{"exits":1},"key state did not exit the reloaded layer"),
            ("after_key",{"renderer":"layer"},"key state did not exit the reloaded layer"),
        ]:
            data=self.fixture();data["contract"][label].update(fields)
            with self.subTest(label=label,fields=fields):self.refuse(data,reason)
        data=self.fixture();data["contract"]["after_key"].update(renderer="layer",handoffs=3)
        self.assertEqual(standing.cursor_layer_evidence(data)["state"],"supported")

    def test_counter_balance(self):
        for renderer in ("layer", "gpu"):
            data=self.fixture();data["contract"]["after_key"].update(renderer=renderer,handoffs=100)
            with self.subTest(renderer=renderer):
                self.refuse(data,"cursor state counters or exit history inconsistent")

    def test_exit_history_count(self):
        data=self.fixture();data["contract"]["after_key"]["exit_frame_us"] = dict(
            count=0,p50=None,p95=None,max=0)
        self.refuse(data,"cursor state counters or exit history inconsistent")

    def test_counters_and_handoff(self):
        data=self.fixture();data["contract"]["samples"][2]["cpu_ns"]=1.
        self.refuse(data,"nonmonotonic sample counters")
        data=self.fixture();data["contract"]["handoff_after_s"]+=.01
        self.refuse(data,"handoff aggregate mismatch")

    def test_partial_and_malformed(self):
        for field in ("clock","stamps","samples","geometry_reads"):
            data=self.fixture();del data["contract"][field]
            with self.subTest(field=field):self.refuse(data,"missing raw native layer evidence")
        for field,value,reason in [("clock","wall","unknown native layer clock"),
                ("stamps",[],"invalid native layer stamps"),
                ("samples",{},"invalid native layer samples"),
                ("geometry_reads",{},"invalid geometry reads")]:
            data=self.fixture();data["contract"][field]=value
            with self.subTest(field=field):self.refuse(data,reason,"malformed")
        for value in (True,float("nan"),float("inf"),-1.,10**400):
            data=self.fixture();data["contract"]["samples"][2]["cpu_ns"]=value
            with self.subTest(value=str(value)):self.refuse(data,"invalid native layer sample","malformed")

    def test_explicit_thresholds(self):
        for limits in (dict(max_footprint_mib=74.),dict(max_wakeups=.1),dict(max_cpu_percent=.009)):
            report=standing.cursor_layer_evidence(self.fixture(),**limits)
            with self.subTest(limits=limits):
                self.assertEqual(report["state"],"supported")
                self.assertEqual(report["acceptance"]["verdict"],"fail")
        c=self.fixture()["contract"];idle=c["idle"]
        report=standing.cursor_layer_evidence({"contract":c},max_footprint_mib=idle["peak_mib"],
            max_wakeups=idle["wakeups_per_s"],max_cpu_percent=idle["cpu_percent"])
        self.assertEqual(report["acceptance"]["verdict"],"pass")
        report=standing.cursor_layer_evidence(self.fixture(),max_footprint_mib=float("nan"))
        self.assertEqual(report["state"],"malformed")

    def test_postprocessing_thresholds_and_privacy(self):
        import contextlib,io,tempfile
        from unittest import mock
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/"layer.json";data=self.fixture()
            data["contract"]["stamps"]["private"]="private-sentinel"
            path.write_text(_json.dumps(data))
            out=io.StringIO()
            argv=[str(HERE/"macos-standing.py"),"--native-layer-input",str(path),
                  "--native-layer-max-footprint-mib","74","--native-layer-max-wakeups","0.1",
                  "--native-layer-max-cpu-percent","0.009"]
            with mock.patch.object(sys,"argv",argv),contextlib.redirect_stdout(out),\
                    mock.patch.object(standing.subprocess,"Popen",side_effect=AssertionError("spawn")),\
                    mock.patch.object(standing,"build_probes",side_effect=AssertionError("build")):
                self.assertEqual(standing.main(),0)
            report=_json.loads(out.getvalue());e=report["reports"][0]["evidence"]
            self.assertTrue(report["diagnostic_only"]);self.assertFalse(report["countable"])
            self.assertEqual(e["acceptance"]["verdict"],"fail")
            self.assertEqual(e["acceptance"]["thresholds"],dict(peak_mib=74.,wakeups_per_s=.1,cpu_percent=.009))
            self.assertNotIn("private-sentinel",out.getvalue())
            self.assertNotIn(str(path),out.getvalue())


class OutputBlink(unittest.TestCase):
    """Portable PR 4 contracts; native fixture tests never launch a GUI."""
    def native_fixture(self, name, data=None, offsets=False):
        """Compile the changed source and execute only its GUI-free entry."""
        import tempfile
        if sys.platform != 'darwin' or not shutil.which('clang') or not shutil.which('swiftc'):
            self.skipTest('native decision fixtures need macOS clang and swiftc')
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp)/name
            source = HERE/'macos-standing'
            commands = {
                'printing': ['clang', '-O2', str(source/'printing.c')],
                'observer': ['clang', '-O2', '-fobjc-arc', '-framework', 'AppKit',
                             '-framework', 'CoreGraphics', str(source/'observer.m')],
                'latency-probe': ['swiftc', '-O', str(source/'latency-probe.swift')],
            }
            build = subprocess.run([*commands[name], '-o', str(binary)], capture_output=True, text=True, timeout=60)
            self.assertEqual(build.returncode, 0, build.stderr)
            result = subprocess.run([str(binary), '--self-test'], input=data, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            if offsets:
                valid = [('0', 1), ('0,5900,6000,6100,6200,6300,6400,6500', 8), ('0,6000', 2),
                         (','.join(map(str, range(64))), 64), ('1,60000', 2), ('000000,000001', 2)]
                invalid = [('', 1), ('0,0', 2), ('1,0', 2), ('-1', 1), ('+1', 1), (' 1', 1),
                           ('1 ', 1), ('1,', 1), (',1', 1), ('1,,2', 2), ('1.0', 1), ('60001', 1),
                           ('999999999999999999999999999', 1), ('0,1', 1), ('0', 2),
                           (','.join(map(str, range(65))), 65), ('0', '1junk')]
                for text, count in valid + invalid:
                    parsed = subprocess.run([str(binary), '--self-test-offsets', text, str(count)],
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(parsed.returncode, 0 if (text,count) in valid else 2, (text,count,parsed.stderr))
                return result
            if name == 'latency-probe':
                for blink, expected in [(True,dict(posts=0,captures=1)),(False,dict(posts=1,captures=0))]:
                    output=Path(tmp)/'dispatch.json'
                    args=[str(binary),'--self-test-dispatch','--pid','42','--out',str(output)]
                    if blink:args+=['--blink-check','--window-id','7','--started-ns','100','--cursor-rect','1,2,3,4']
                    dispatched=subprocess.run(args,capture_output=True,text=True,timeout=10)
                    self.assertEqual(dispatched.returncode,0,dispatched.stderr)
                    self.assertEqual(_json.loads(output.read_text()),expected)
            return result

    def samples(self, origin=10_000_000_000, count=82):
        result = []
        for i in range(count):
            t = origin + i * 100_000_000
            c = dict(t_ns=t, known=True, valid=True, frontmost_pid=42, target_window=7, top_window=7)
            result.append(dict(query_start_ns=t+1000, query_end_ns=t+2000, t_ns=t+2000,
                scheduled_ns=t, pid=42, process_start_identity='123:45', rss=10,
                footprint=(100+i)*1048576, max_footprint=999*1048576,
                cpu_ns=i*100_000, wakeups=i, status='ok', focus_before=c,
                focus_after={**c, 't_ns': t+3000}, focus_changes=[]))
        return result

    def printing(self):
        began=10_000_000_000
        return ([{'began_ns':began}]+[dict(seq=i+1, deadline_ns=began+i*100_000_000,
            write_ns=began+i*100_000_000, write_end_ns=began+i*100_000_000+500)
            for i in range(80)]+[{'done_ns':began+8_000_000_000}])

    def test_optional_selection_and_defaults(self):
        import argparse
        self.assertEqual(standing.WORKLOADS, ('startup','idle','flood-memory','vtebench'))
        self.assertIn('output-memory', standing.OPT_IN_WORKLOADS)
        self.assertIn('blink-window', standing.OPT_IN_WORKLOADS)
        args=argparse.Namespace(rounds=None,startup_rounds=30,idle_rounds=5,flood_rounds=5,
            vtebench_rounds=5,latency_rounds=10,output_memory_rounds=10,blink_rounds=10,
            workloads='output-memory,blink-window')
        self.assertEqual(standing.resolve_rounds(args)['output-memory'],10)
        self.assertEqual(standing.resolve_rounds(args)['blink-window'],10)
        args.workloads=','.join(standing.WORKLOADS)
        self.assertEqual(standing.resolve_rounds(args),dict(startup=30,idle=5,**{'flood-memory':5},vtebench=5,latency=10))

    def test_cli_optional_names_and_numeric_validation_before_side_effects(self):
        from unittest.mock import patch
        import contextlib,io
        class ReachedPreparation(Exception):pass
        for workload in ('output-memory','blink-window'):
            with patch.object(sys,'argv',['standing','--workloads',workload,'--no-build','--peers','',
                                           '--kettle','fixture','--allow-bare']),patch.object(standing,'build_probes',side_effect=ReachedPreparation),patch.object(standing,'require_bundles'):
                with self.assertRaises(ReachedPreparation):standing.main()
        for flag,value in [('--blink-settle','nan'),('--blink-window','inf'),('--blink-rounds','0'),
                           ('--output-memory-rounds','0'),('--blink-timeout','-1'),('--blink-cursor-rect','1,2,300,4')]:
            with patch.object(sys,'argv',['standing',flag,value]),patch.object(standing.subprocess,'run') as run,patch.object(standing,'claim_out_dir') as claim,contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as raised:standing.main()
                self.assertEqual(raised.exception.code,2)
                run.assert_not_called();claim.assert_not_called()

    def test_printing_bytes_and_absolute_schedule(self):
        import hashlib
        expected=b''.join(f'{i:02d}: The quick brown fox jumps over the lazy dog 0123456789\n'.encode() for i in range(1,81))
        self.assertEqual(standing.hc.PRINT_BYTES,expected)
        self.assertEqual(standing.hc.PRINT_SHA256,hashlib.sha256(expected).hexdigest())
        result = self.native_fixture('printing')
        self.assertEqual(result.stdout.encode(), expected)
        records = [_json.loads(line) for line in result.stderr.splitlines()]
        self.assertEqual(len(records), 82)
        began = records[0]['began_ns']
        self.assertEqual(records[-1]['done_ns'], began + 8_000_000_000)
        for i, record in enumerate(records[1:-1]):
            self.assertEqual(record['seq'], i+1)
            self.assertEqual(record['deadline_ns'], began + i*100_000_000)
            self.assertEqual(record['write_ns'], began + i*100_000_000)
            self.assertEqual(record['write_end_ns'], record['write_ns']+30_000_000)
        self.assertTrue(standing.hc.printing_row(self.printing(),self.samples(),42,7)['printing_valid'])

    def test_first_designated_query_current_and_lifetime(self):
        samples=self.samples()
        samples[61]['footprint']=1*1048576
        row=standing.hc.printing_row(self.printing(),samples,42,7)
        self.assertTrue(row['printing_valid'],row)
        self.assertEqual(row['printing_mib'],160)
        self.assertEqual(row['printing_max_mib'],999)
        self.assertEqual(row['printing_sample_ns'],samples[60]['query_start_ns'])
        samples[60]['focus_before']['valid']=False
        self.assertFalse(standing.hc.printing_row(self.printing(),samples,42,7)['printing_valid'])

    def test_focus_race(self):
        samples=self.samples()
        # true 6.01 / focus loss 6.025 / query 6.09, never stale focus.
        t=10_000_000_000
        samples[60]['focus_before']['t_ns']=t+6_010_000_000
        samples[60]['query_start_ns']=t+6_090_000_000
        samples[60]['query_end_ns']=samples[60]['t_ns']=t+6_091_000_000
        samples[60]['focus_after']['t_ns']=t+6_092_000_000
        samples[60]['focus_changes']=[dict(t_ns=t+6_025_000_000,valid=False)]
        row=standing.hc.printing_row(self.printing(),samples,42,7)
        self.assertFalse(row['printing_valid'])
        self.assertIn('focus change',row['printing_reason'])

    def test_stale_and_nonmonotonic_focus(self):
        import copy
        for defect in ('stale-before', 'stale-first', 'late-after', 'backwards'):
            samples=copy.deepcopy(self.samples())
            if defect=='stale-before':samples[60]['focus_before']['t_ns']=0
            if defect=='stale-first':samples[0]['focus_before']['t_ns']=0
            if defect=='late-after':samples[0]['focus_after']['t_ns']+=250_000_000
            if defect=='backwards':
                # Each query is bracketed and fresh, but focus checks go back
                # behind the preceding sample's completed focus observation.
                samples[60]['focus_before']['t_ns']=samples[59]['focus_after']['t_ns']-1
            row=standing.hc.printing_row(self.printing(),samples,42,7)
            self.assertFalse(row['printing_valid'],defect)
            self.assertIn('focus',row['printing_reason'])

    def test_frame_freshness_and_public_linkage(self):
        import copy,tempfile
        self.assertIsNone(standing.hc.validation_reason(self.capture(),self.setup()))
        for defect in ('stale','missing','future','bool','float','backwards',
                       'private-id','private-digest','bad-frame-digest'):
            capture=copy.deepcopy(self.capture())
            if defect=='missing':del capture['frames'][30]['arrival_ns']
            if defect=='stale':
                for frame in capture['frames']:frame['arrival_ns']=0
            if defect=='future':capture['frames'][30]['arrival_ns']=capture['frames'][30]['t_ns']+1
            if defect=='bool':capture['frames'][30]['arrival_ns']=True
            if defect=='float':capture['frames'][30]['arrival_ns']=float(capture['frames'][30]['arrival_ns'])
            if defect=='backwards':capture['frames'][30]['arrival_ns']=capture['frames'][29]['arrival_ns']-1
            if defect=='private-id':capture['validation_id']='/Users/private-owner/private@email.test'
            if defect=='private-digest':capture['before_sha256']='Personal Signing Identity'
            if defect=='bad-frame-digest':capture['frames'][30]['sha256']='x'*64
            self.assertIsNotNone(standing.hc.validation_reason(capture,self.setup()),defect)
            with tempfile.TemporaryDirectory() as tmp:
                path=Path(tmp)/'validation';path.write_text(_json.dumps(capture))
                activity,evidence=standing.hc.blink_evidence(path,self.setup())
                self.assertEqual(activity,'unproven',defect)
                public=_json.dumps(evidence)
                self.assertNotIn('/Users/private-owner',public)
                self.assertNotIn('private@email.test',public)
                self.assertNotIn('Personal Signing Identity',public)
        capture=self.capture()
        capture['frames'][30]['arrival_ns']=capture['frames'][30]['t_ns']-250_000_000
        # Boundary freshness is valid when chronology is also preserved.
        for frame in capture['frames']:frame['arrival_ns']=frame['t_ns']-250_000_000
        self.assertIsNone(standing.hc.validation_reason(capture,self.setup()))

    def test_missing_trace_public_error(self):
        from unittest.mock import patch
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp)
            class Runner:
                def __init__(self):self.work=work;self.probes={'observer':'unused'}
                def launch(self,*args):
                    (work/'hc-launch.json').write_text(_json.dumps(dict(started_ns=10_000_000_000,pid=42,window_id=7)))
                    return object()
                def wait_for(self,*args):return True
                def end_sampler(self,*args):pass
                def stop(self,*args):return True
            with patch.object(standing.hc,'now_ns',side_effect=[11_000_000_000,19_000_000_000]),patch.object(standing.hc,'start_observer',return_value=object()):
                row=standing.hc.collect(Runner(),'fixture','blink-window',dict(activate=False,settle=2.5,window=6.),work/'saved',self.setup())
            self.assertEqual(row,{'error':'observer/payload trace invalid'})
            self.assertNotIn(tmp,_json.dumps(row))
            private=_json.loads((work/'saved.trace-error.json').read_text())
            self.assertEqual(private['timeline_path'],str(work/'hc-timeline.jsonl'))

    def test_wrong_window_cover_unknown(self):
        import copy
        for key,value in [('top_window',8),('known',False),('valid',False),('frontmost_pid',99),('target_window',8)]:
            samples=copy.deepcopy(self.samples())
            samples[60]['focus_after'][key]=value
            self.assertFalse(standing.hc.printing_row(self.printing(),samples,42,7)['printing_valid'],key)
        # Front-to-back synthetic CoreGraphics dictionaries, no window server.
        def window(number, owner=42, layer=0, alpha=1, x=0):
            return dict(kCGWindowNumber=number,kCGWindowOwnerPID=owner,kCGWindowLayer=layer,
                        kCGWindowAlpha=alpha,kCGWindowBounds=dict(X=x,Y=0,Width=600,Height=400))
        wanted=window(7)
        fixtures=[([wanted],42,True,True),([window(8),wanted],42,True,False),
                  ([window(9,owner=99,layer=1000),wanted],42,True,False),
                  ([window(9,layer=1000),wanted],42,True,False),
                  ([window(9,layer=1000,x=1000),wanted],42,True,True),
                  ([window(9,layer=1000,alpha=0),wanted],42,True,True),
                  ([wanted],99,True,False),([],42,False,False),
                  ([window(7,owner=99)],42,True,False)]
        for windows, front, known, valid in fixtures:
            result=self.native_fixture('observer',_json.dumps(dict(pid=42,target=7,front=front,windows=windows)))
            decision=_json.loads(result.stdout)
            self.assertEqual(decision['known'],known)
            self.assertEqual(decision['valid'],valid,windows)

    def test_payload_short_stalled_omitted_missing_done(self):
        import copy
        good=self.printing()
        for defect in ('short','overlong','stalled','omitted','missing-done'):
            records=copy.deepcopy(good)
            if defect=='short':records[-1]['done_ns']=15_000_000_000
            if defect=='overlong':records[-1]['done_ns']=18_300_000_000
            if defect=='stalled':
                records[-2]['write_end_ns']=18_160_000_000
                records[-1]['done_ns']=18_200_000_000
            if defect=='omitted':del records[25]
            if defect=='missing-done':records.pop()
            self.assertFalse(standing.hc.printing_row(records,self.samples(),42,7)['printing_valid'],defect)

    def test_blink_launch_origin_readiness_actual_span(self):
        samples=self.samples(origin=12_500_000_000,count=63)
        samples[60]['query_end_ns']+=10_000_000
        samples[60]['t_ns']=samples[60]['query_end_ns']
        samples[60]['focus_after']['t_ns']=samples[60]['query_end_ns']+1000
        row=standing.hc.blink_row(samples,10_000_000_000,12_000_000_000,42,7,'verified')
        self.assertTrue(row['blink_valid'],row)
        self.assertEqual(row['blink_start_ns'],12_500_000_000)
        self.assertEqual(row['blink_end_ns'],18_500_000_000)
        self.assertAlmostEqual(row['wakeups_per_second'],60/6.01)
        self.assertFalse(standing.hc.blink_row(samples,10_000_000_000,12_500_000_000,42,7,'verified')['blink_valid'])

    def setup(self):
        return dict(binary_sha256='b'*64,config_sha256='c'*64,display='d',settle_s=2.5,window_s=6.,
                    cursor_rect='1,2,3,4',shape='block',timeout_s=10.,native_display={'width_pt':1920})

    def capture(self):
        return dict(contract=standing.hc.CONTRACT,setup=self.setup(),start_ns=12_500_000_000,
            end_ns=18_500_000_000,validation_id='10000000000',before_sha256=None,started_ns=10_000_000_000,
            window_id=7,target_window_id=7,native_display={'width_pt':1920},cursor_rect=[1.,2.,3.,4.],
            frames=[dict(t_ns=12_500_000_000+i*100_000_000,arrival_ns=12_499_000_000+i*100_000_000,sha256=str(i//5%2)*64,visible=True,frame_status='complete') for i in range(61)])

    def test_each_ab_side_takes_its_own_validation(self):
        # A validation certifies one binary, so a Kettle A/B (the C2 gate,
        # D2's 4.8.0 against 4.9.0) names one file per side.
        import hashlib, tempfile
        side_b = dict(self.setup(), binary_sha256='e'*64)
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp)/'a.json', Path(tmp)/'b.json'
            a.write_text(_json.dumps(self.capture()))
            b.write_text(_json.dumps(dict(self.capture(), setup=side_b)))
            for setup, expected in ((self.setup(), a), (side_b, b)):
                for order in ([str(a), str(b)], [str(b), str(a)]):
                    path, activity, evidence = standing.hc.select_validation(order, setup)
                    self.assertEqual((Path(path), activity), (expected, 'verified'))
                    self.assertEqual(evidence['content_sha256'], hashlib.sha256(expected.read_bytes()).hexdigest())
            path, activity, evidence = standing.hc.select_validation([str(a)], side_b)
            self.assertEqual((path, activity), (None, 'unproven'))
            self.assertIsNotNone(evidence['reason'])
            self.assertEqual(standing.hc.select_validation(str(b), side_b)[1], 'verified')
            self.assertEqual(standing.hc.select_validation(None, side_b), (None, 'unproven', None))
            self.assertEqual(standing.hc.select_validation([], side_b), (None, 'unproven', None))

    def test_an_unreadable_validation_file_refuses_before_launch(self):
        import builtins, contextlib, io, tempfile
        from unittest.mock import patch
        real_open = builtins.open
        with tempfile.TemporaryDirectory() as tmp:
            locked = Path(tmp)/'locked.json'; locked.write_text('{}')
            def denied(path, *args, **kwargs):
                if str(path) == str(locked):
                    raise PermissionError('denied')
                return real_open(path, *args, **kwargs)
            with patch.object(sys, 'argv', ['standing', '--no-build', '--blink-validation', str(locked)]), \
                 patch.object(builtins, 'open', side_effect=denied), \
                 patch.object(standing, 'build_probes') as build, contextlib.redirect_stderr(io.StringIO()) as err:
                with self.assertRaises(SystemExit) as raised: standing.main()
            self.assertEqual(raised.exception.code, 2)
            self.assertIn('--blink-validation file is unreadable', err.getvalue())
            build.assert_not_called()

    def test_validation_files_repeat_and_refuse_a_missing_name(self):
        import contextlib, io, tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            real = Path(tmp)/'a.json'; real.write_text('{}')
            for flags in (['--blink-validation', str(real), '--blink-validation', str(Path(tmp)/'missing.json')],
                          ['--blink-validation', tmp],
                          ['--blink-validate-only', '--blink-cursor-rect', '1,2,3,4', '--blink-shape', 'block',
                           '--blink-timeout', '10', '--blink-validation-before', str(Path(tmp)/'missing.json')]):
                with self.subTest(flags=flags), patch.object(sys, 'argv', ['standing', '--no-build', *flags]), \
                     patch.object(standing, 'build_probes') as build, contextlib.redirect_stderr(io.StringIO()) as err:
                    with self.assertRaises(SystemExit) as raised: standing.main()
                    self.assertEqual(raised.exception.code, 2)
                    self.assertIn('--blink-validation', err.getvalue())
                    self.assertNotIn(tmp, err.getvalue())
                    build.assert_not_called()

    def test_blink_peak_activity_evidence_and_noninjecting(self):
        import tempfile,copy
        samples=self.samples(origin=12_500_000_000,count=63)
        row=standing.hc.blink_row(samples,10_000_000_000,12_000_000_000,42,7,'verified')
        self.assertEqual(row['blink_peak_mib'],160)
        self.assertNotEqual(row['blink_peak_mib'],999)
        for activity in ('unproven','disabled-default'):
            row=standing.hc.blink_row(samples,10_000_000_000,12_000_000_000,42,7,activity)
            self.assertTrue(row['blink_window_valid'])
            self.assertFalse(row['blink_valid'])
            self.assertIsNone(standing.metric_value(standing.metric_descriptor('blink-window','footprint_mib'),'blink-window',row))
            self.assertEqual(row['footprint_mib'],160)
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'validation.json';p.write_text(_json.dumps(self.capture()))
            self.assertEqual(standing.hc.blink_evidence(p,self.setup())[0],'verified')
            for field in ('binary_sha256','config_sha256','display','window_s'):
                setup=copy.deepcopy(self.setup());setup[field]=7. if field=='window_s' else 'd'*64 if field.endswith('sha256') else 'different'
                self.assertEqual(standing.hc.blink_evidence(p,setup)[0],'unproven')
        self.native_fixture('latency-probe')
        capture=self.capture();capture['frames'][-20:]=[dict(f,sha256='0'*64) for f in capture['frames'][-20:]]
        self.assertIsNotNone(standing.hc.validation_reason(capture,self.setup()))

    def test_timeline_bounds_and_native_errors(self):
        import copy,tempfile
        for defect in ('count','gap','late','timestamp','cpu','wakeups','identity','exit','malformed'):
            samples=copy.deepcopy(self.samples())
            if defect=='count':samples=samples[::2]
            if defect=='gap':del samples[20:24]
            if defect=='late':
                for sample in samples:sample['scheduled_ns']-=300_000_000
            if defect=='timestamp':samples[30]['query_start_ns']=samples[29]['query_start_ns']
            if defect=='cpu':samples[30]['cpu_ns']=0
            if defect=='wakeups':samples[30]['wakeups']=0
            if defect=='identity':samples[30]['process_start_identity']='other'
            if defect=='exit':samples[30]['status']='target-exited-or-query-failed'
            if defect=='malformed':samples[30]['rss']=True
            self.assertFalse(standing.hc.printing_row(self.printing(),samples,42,7)['printing_valid'],defect)
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'bad';p.write_text('{}\nnot-json\n')
            with self.assertRaises(ValueError):standing.hc.read_jsonl(p)
            p.write_text('{}\n'*2001)
            with self.assertRaises(ValueError):standing.hc.read_jsonl(p)

    def test_observer_reaped_before_launch_on_exception_timeout_cancel(self):
        from unittest.mock import patch
        import tempfile
        hc=standing.hc
        real_spawn=subprocess.Popen
        for exception in (RuntimeError('fixture'),subprocess.TimeoutExpired('fixture',1),KeyboardInterrupt()):
            with tempfile.TemporaryDirectory() as tmp:
                work=Path(tmp);events=[];children=[]
                class Runner:
                    def __init__(self):
                        self.work=work;self.probes={'printing':'unused','observer':'unused'}
                    def launch(self,*args):
                        child=real_spawn([sys.executable,'-c','import time;time.sleep(30)']);children.append(child)
                        (work/'grid').touch();(work/'hc-launch.json').write_text(_json.dumps(dict(started_ns=1,pid=42,window_id=7)))
                        return child
                    def wait_for(self,*args):return True
                    def activate(self):return True
                    def end_sampler(self,child):
                        events.append('observer');child.terminate();child.wait(timeout=5)
                    def stop(self,child,*args):
                        events.append('launch');child.terminate();child.wait(timeout=5);return True
                def observer(*args,**kwargs):
                    child=real_spawn([sys.executable,'-c','import time;time.sleep(30)']);children.append(child)
                    (work/'hc-timeline.jsonl').write_text(_json.dumps(self.samples()[0])+'\n')
                    return child
                try:
                    with patch.object(hc,'start_observer',side_effect=observer),patch.object(hc,'read_jsonl',side_effect=exception):
                        with self.assertRaises(type(exception)):
                            hc.collect(Runner(),'fixture','output-memory',dict(activate=False),work/'saved')
                    self.assertEqual(events,['observer','launch'])
                    self.assertTrue(all(c.poll() is not None for c in children))
                finally:
                    for child in children:
                        if child.poll() is None:child.terminate()
                        child.wait(timeout=5)

    def test_validation_nonobject_json_and_cadence_bounds(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'validation'
            for data in ('[]','null','42','"text"','{'):
                p.write_text(data)
                self.assertEqual(standing.hc.blink_evidence(p,self.setup())[0],'unproven')
        from unittest.mock import patch
        import contextlib,io
        for value in ('49','1001'):
            with patch.object(sys,'argv',['standing','--memory-sample-ms',value]),contextlib.redirect_stderr(io.StringIO()),patch.object(standing,'build_probes') as builds:
                with self.assertRaises(SystemExit) as raised:standing.main()
                self.assertEqual(raised.exception.code,2);builds.assert_not_called()

    def test_validation_override_identity_and_cleanup(self):
        import argparse,copy,tempfile
        args=argparse.Namespace(rounds=2,workloads=','.join(standing.WORKLOADS),blink_validate_only=True)
        self.assertEqual(standing.resolve_rounds(args)['blink-window'],2)
        for field,value in [('window_id',8),('target_window_id',8),('native_display',{'width_pt':1}),
                            ('cursor_rect',[9,9,9,9]),('started_ns',1)]:
            capture=copy.deepcopy(self.capture());capture[field]=value
            self.assertIsNotNone(standing.hc.validation_reason(capture,self.setup()),field)
        self.native_fixture('latency-probe')
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp)
            class Runner:
                def __init__(self):self.work=work;self.probes={}
                def launch(self,*args):
                    (work/'grid').touch()
                    (work/'hc-launch.json').write_text(_json.dumps(dict(started_ns=10_000_000_000,
                        pid=42,window_id=7,native_display={'width_pt':1920})))
                    return object()
                def wait_for(self,*args):return True
                def stop(self,*args):return False
                def blink_probe(self,*args):(work/'blink-capture.json').write_text(_json.dumps(test.capture()))
            from unittest.mock import patch
            test=self
            with patch.object(standing.hc,'now_ns',return_value=11_000_000_000):
                result=standing.hc.collect(Runner(),'fixture','blink-window',dict(activate=False,settle=2.5,
                    window=6.,validate_only=True,rect='1,2,3,4'),work/'evidence',self.setup())
            self.assertIn('cleanup failed',result.get('error',''))
            self.assertTrue(_json.loads((work/'evidence.json').read_text())['killed'])

    @unittest.skipUnless(sys.platform=='darwin' and shutil.which('swiftc'), 'needs macOS swiftc')
    def test_native_launch_retains_exited_target_until_observer_drain(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp);binary=work/'launch'
            subprocess.run(['swiftc','-O','-o',str(binary),str(HERE/'macos-standing'/'launch.swift')],check=True)
            context=work/'context';stamp=work/'stamp';env=dict(os.environ,KETTLE_HC_LAUNCH_CONTEXT=str(context))
            fixture=work/'fixture'
            code=work/'fixture.c';code.write_text('#include <unistd.h>\nint main(void){usleep(1500000);return 0;}\n')
            subprocess.run(['clang','-O','-o',str(fixture),str(code)],check=True)
            process=subprocess.Popen([str(binary),str(work/'result'),str(stamp),'3','--','/bin/sleep','.5'],env=env)
            try:
                deadline=time.monotonic()+2
                while time.monotonic()<deadline:
                    pid_file=Path(str(stamp)+'.pid')
                    if pid_file.exists() and pid_file.read_text().strip().isdigit():break
                    time.sleep(.01)
                pid=int(Path(str(stamp)+'.pid').read_text())
                # A command-line fixture standing in for the observer. Its PID
                # is owned by the same launch helper, which must reap it first.
                request=Path(str(context)+'.observer-request')
                request.write_text(_json.dumps([str(fixture),str(pid),'0','unused','0','100','5']))
                receipt=Path(str(context)+'.observer-reaped')
                process.wait(timeout=5)
                self.assertTrue(receipt.exists(), 'target released before observer reap')
                self.assertFalse(Path(str(stamp)+'.pid').exists())
            finally:
                if process.poll() is None:process.terminate()
                process.wait(timeout=5)
                # The deliberately reverted fixture observer exits by itself.
                # Never signal an orphan after its owner released the PID.
                time.sleep(1.6)

    @unittest.skipUnless(sys.platform=='darwin' and shutil.which('swiftc'), 'needs macOS swiftc')
    def test_native_launch_accepts_sparse_observer_request(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp);binary=work/'launch'
            subprocess.run(['swiftc','-O','-o',str(binary),str(HERE/'macos-standing'/'launch.swift')],check=True)
            context=work/'context';stamp=work/'stamp';env=dict(os.environ,KETTLE_HC_LAUNCH_CONTEXT=str(context))
            fixture=work/'fixture'
            code=work/'fixture.c';code.write_text('#include <unistd.h>\nint main(void){usleep(1500000);return 0;}\n')
            subprocess.run(['clang','-O','-o',str(fixture),str(code)],check=True)
            process=subprocess.Popen([str(binary),str(work/'result'),str(stamp),'3','--','/bin/sleep','.5'],env=env)
            try:
                deadline=time.monotonic()+2
                while time.monotonic()<deadline:
                    pid_file=Path(str(stamp)+'.pid')
                    if pid_file.exists() and pid_file.read_text().strip().isdigit():break
                    time.sleep(.01)
                pid=int(Path(str(stamp)+'.pid').read_text())
                # A command-line fixture standing in for the observer. Its PID
                # is owned by the same launch helper, which must reap it first.
                request=Path(str(context)+'.observer-request')
                request.write_text(_json.dumps([str(fixture),str(pid),'0','unused','0','100','5','0,100,200,300,400']))
                receipt=Path(str(context)+'.observer-reaped')
                process.wait(timeout=5)
                self.assertTrue(receipt.exists(), 'target released before observer reap')
                self.assertFalse(Path(str(stamp)+'.pid').exists())
            finally:
                if process.poll() is None:process.terminate()
                process.wait(timeout=5)
                # The deliberately reverted fixture observer exits by itself.
                # Never signal an orphan after its owner released the PID.
                time.sleep(1.6)

    @unittest.skipUnless(sys.platform=='darwin' and shutil.which('swiftc'), 'needs macOS swiftc')
    def test_native_launch_parent_loss_before_observer_registration(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp);binary=work/'launch'
            subprocess.run(['swiftc','-O','-o',str(binary),str(HERE/'macos-standing'/'launch.swift')],check=True)
            stamp=work/'stamp';result=work/'result';context=work/'context'
            owner_code='import subprocess,pathlib,time,os; p=subprocess.Popen('+repr([str(binary),str(result),str(stamp),'3','--','/bin/sleep','30'])+'); pathlib.Path('+repr(str(work/'launch.pid'))+').write_text(str(p.pid)); deadline=time.monotonic()+2\nwhile not pathlib.Path('+repr(str(stamp)+'.pid')+').exists() and time.monotonic()<deadline:time.sleep(.01)\n'
            env=dict(os.environ,KETTLE_HC_LAUNCH_CONTEXT=str(context))
            owner=subprocess.Popen([sys.executable,'-c',owner_code],env=env)
            owner.wait(timeout=5)
            deadline=time.monotonic()+5
            while not result.exists() and time.monotonic()<deadline:time.sleep(.01)
            self.assertTrue(result.exists(), 'orphan cleanup stuck before observer registration')
            self.assertFalse(Path(str(stamp)+'.pid').exists())
            launch_pid=int((work/'launch.pid').read_text())
            self.assertTrue(gone_within(launch_pid))

    @unittest.skipUnless(sys.platform=='darwin' and shutil.which('clang'), 'needs macOS clang')
    def test_native_printing_and_observer_owned_scratch(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp);helpers=standing.hc.build_helpers(HERE/'macos-standing',work)
            barrier=work/'go';log=work/'printing.jsonl';output=work/'output'
            children=[]
            try:
                with output.open('wb') as sink:
                    child=subprocess.Popen([str(helpers['printing']),str(barrier),str(log)],stdout=sink);children.append(child)
                target=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)']);children.append(target)
                trace=work/'observer.jsonl'
                observer=subprocess.Popen([str(helpers['observer']),str(target.pid),'0',str(trace),
                    str(standing.hc.now_ns()+1_000_000_000),'100','5'],
                    env=dict(os.environ,KETTLE_HC_OBSERVER_SELF_COST='1'));children.append(observer)
                observer.wait(timeout=5)
                rows=standing.hc.read_jsonl(trace)
                self.assertEqual(len(rows),5)
                cost=_json.loads(Path(str(trace)+'.self.json').read_text())
                self.assertEqual(set(cost),{'cpu_ns','wakeups','query_count'})
                self.assertEqual(cost['query_count'],5)
                self.assertTrue(all(type(v) is int and v >= 0 for v in cost.values()))
                self.assertTrue(all(s['status']=='ok' and s['pid']==target.pid for s in rows))
                self.assertIsNone(standing.hc.trace_reason(rows,target.pid,0))
                self.assertTrue(all(not standing.hc.visible(s,target.pid,0) for s in rows))
                target.terminate()
                # Retain the unreaped spawn handle through the final query.
                time.sleep(.05)
                dead=work/'dead.jsonl'
                observer=subprocess.Popen([str(helpers['observer']),str(target.pid),'0',str(dead),
                    str(standing.hc.now_ns()+1_000_000_000),'100','5']);children.append(observer)
                observer.wait(timeout=5)
                self.assertNotEqual(standing.hc.read_jsonl(dead)[0]['status'],'ok')
                target.wait(timeout=5)
                barrier.touch()
                deadline=time.monotonic()+10
                while time.monotonic()<deadline and 'done_ns' not in log.read_text():time.sleep(.02)
                records=standing.hc.read_jsonl(log,82)
                self.assertEqual(len(records),82)
                self.assertEqual(output.read_bytes(),standing.hc.PRINT_BYTES)
                began=records[0]['began_ns'];done=records[-1]['done_ns']
                self.assertGreaterEqual(done-began,8_000_000_000)
                self.assertLessEqual(done-began,8_250_000_000)
                self.assertEqual([r['seq'] for r in records[1:-1]],list(range(1,81)))
                self.assertTrue(all(r['deadline_ns']==began+(r['seq']-1)*100_000_000 for r in records[1:-1]))
            finally:
                for child in children:
                    if child.poll() is None:child.terminate()
                    child.wait(timeout=5)
                self.assertTrue(all(c.poll() is not None for c in children))


class RebaseOwnership(unittest.TestCase):
    def test_missing_owner_receipt_fails_without_retry_loop(self):
        import tempfile
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as tmp:
            observer = standing.hc.OwnedObserver(Path(tmp) / 'context', Mock())
            observer.launch.poll.return_value = 0
            with self.assertRaisesRegex(RuntimeError, 'without sampler reap receipt'):
                standing.Runner.end_sampler(observer)

    @unittest.skipUnless(sys.platform == 'darwin' and shutil.which('swiftc'), 'needs macOS swiftc')
    def test_owned_observer_reaps_after_repeated_cancellation(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            launch = work / 'launch'
            subprocess.run(['swiftc', '-O', '-o', str(launch), str(HERE / 'macos-standing/launch.swift')], check=True)
            helpers = standing.hc.build_helpers(HERE / 'macos-standing', work)
            context, stamp = work / 'context', work / 'stamp'
            process = subprocess.Popen([str(launch), str(work / 'result'), str(stamp), '30', '--', '/bin/sleep', '30'],
                env=dict(os.environ, KETTLE_HC_LAUNCH_CONTEXT=str(context)))
            try:
                deadline = time.monotonic() + 5
                while (not Path(str(stamp) + '.pid').exists() or not Path(str(stamp) + '.pid').read_text()) and time.monotonic() < deadline:
                    time.sleep(.01)
                pid = int(Path(str(stamp) + '.pid').read_text())
                observer = standing.hc.start_observer(None, process, context,
                    [str(helpers['observer']), str(pid), '0', str(work / 'timeline'), str(standing.hc.now_ns()), '100', '300'])
                deadline = time.monotonic() + 5
                while not observer.path('started').exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue(observer.path('started').exists())
                interrupts = [KeyboardInterrupt('first'), KeyboardInterrupt('second')]
                original = observer.wait
                def interrupted(timeout=None):
                    if interrupts:
                        raise interrupts.pop(0)
                    return original(timeout)
                observer.wait = interrupted
                standing.Runner.end_sampler(observer)
                self.assertFalse(interrupts)
                self.assertTrue(observer.path('reaped').exists())
                self.assertIsNone(process.poll())
                process.terminate()
                process.wait(timeout=15)
                self.assertFalse(Path(str(stamp) + '.pid').exists())
            finally:
                if process.poll() is None:
                    process.terminate()
                standing.reap_owned_child(process, 15)
            self.assertIsNotNone(process.returncode)


class TypingMemory(unittest.TestCase):
    def fixture(self):
        hc = standing.hc
        start, end, offset = 1_000_000_000, 2_000_000_000, 1000
        artifact = {key: char * 64 for key, char in (
            ('source_sha256', 'a'), ('bundle_sha256', 'b'), ('executable_sha256', 'c'))}
        epoch = dict(contract=hc.TYPING_CONTRACT, clock='CLOCK_UPTIME_RAW',
            probe_clock='mach_absolute_time_ns', typing_start_ns=start, typing_end_ns=end,
            start_mach_ns=start-offset, end_mach_ns=end-offset, pid=42, window_id=7, guards_ok=True)
        for key, at in (('clock_before', start-500_000_000), ('clock_after', end+500_000_000)):
            epoch[key] = dict(raw_before_ns=at, raw_after_ns=at+100,
                              mach_ns=at+50-offset)
        probe = dict(typing_epoch=epoch, samples=[dict(seq=27, warmup=False, t_post=start-offset,
                     display=start-offset+10_000_000, arrival=start-offset+5_000_000),
                     dict(seq=28, warmup=False, t_post=end-offset-550_000_000,
                          display=None, censored=True)], display={'refresh_hz':60})
        samples = []
        # Both boundary-straddling queries have huge footprints. Query cadence
        # remains regular even though they cannot enter the measured median.
        for i in range(13):
            a = start-101_000_000 + i*100_000_000
            b = a+2_000_000
            value = 9000 if i in (0,1,11,12) else 100 if i == 10 else 10+i
            def focus(at):
                return dict(t_ns=at, known=True, valid=True, frontmost_pid=42,
                            top_window=7, target_window=7)
            samples.append(dict(query_start_ns=a, query_end_ns=b, t_ns=b, scheduled_ns=a,
                pid=42, process_start_identity='owned:42', rss=value*hc.MIB,
                footprint=value*hc.MIB, max_footprint=9999*hc.MIB, cpu_ns=i*1000,
                wakeups=i, status='ok', focus_before=focus(a-100), focus_after=focus(b+100),
                focus_changes=[]))
        return probe, samples, artifact

    def memory(self, probe=None, samples=None, artifact=None, sample_ms=100):
        p,s,a = self.fixture()
        return standing.hc.typing_memory_row(p if probe is None else probe,
            s if samples is None else samples, 42, 7, sample_ms, a if artifact is None else artifact)

    def test_measured_median_and_boundary_exclusion(self):
        p,s,a = self.fixture()
        row = self.memory(p,s,a)
        self.assertTrue(row['typing_memory_valid'], row['typing_memory_reason'])
        self.assertEqual(row['typing_sample_count'], 9)
        self.assertEqual(row['typing_expected_samples'], 10)
        self.assertEqual(row['typing_coverage'], .9)
        self.assertEqual(row['typing_footprint_mib'], 16.)
        self.assertEqual(row['typing_observed_peak_mib'], 100.)
        self.assertEqual(row['typing_max_footprint_mib'], 9999.)
        for i in (0,1,11,12): s[i]['footprint'] = 123456*standing.hc.MIB
        self.assertEqual(self.memory(p,s,a)['typing_footprint_mib'], 16.)

    def test_censored_key_and_guards_keep_the_epoch(self):
        p,s,a = self.fixture()
        # 26 warmup/calibration records and two measured keys.
        records = {i:(1, 1_000_000_000, 1_000_000_000) for i in range(1,29)}
        p['samples'] = [dict(seq=i, warmup=True, t_post=800_000_000, display=810_000_000)
                        for i in range(7,27)] + p['samples']
        row = standing.latency_row(p, records, 500.)
        self.assertEqual((row['keys'], row['censored'], row['mean_ms']), (2,1,255.))
        self.assertEqual(standing.latency_keys(row,500), [10.,500.])
        memory = self.memory(p,s,a)
        self.assertTrue(memory['typing_memory_valid'])
        self.assertEqual(memory['typing_end_ns'],2_000_000_000)
        p['typing_epoch']['guards_ok'] = False
        self.assertFalse(self.memory(p,s,a)['typing_memory_valid'])

    def test_missing_coverage_is_metric_local(self):
        self.assertEqual(standing.hc.coverage_reason([{'query_end_ns':150_000_000+i*100_000_000}
            for i in range(7)],0,1_000_000_000),'insufficient timeline coverage')
        self.assertEqual(standing.hc.coverage_reason([{'query_end_ns':v} for v in
            (1_000_000,101_000_000,201_000_000,301_000_000,401_000_000,501_000_000,601_000_000,701_000_000,999_000_000)],
            0,1_000_000_000),'timeline gap exceeds 250 ms')
        p,s,a = self.fixture()
        for broken in ([], s[:4], s[:5]+s[8:], [r for i,r in enumerate(s) if i%2]):
            row = dict(samples_ms=[10.,20.], keys=2, censored=0, seq_mismatch=0, refresh_hz=60,
                       tool_artifact=a, **self.memory(p,broken,a))
            self.assertFalse(row['typing_memory_valid'])
            self.assertIsNone(standing.metric_value(standing.metric_descriptor('latency','typing_footprint_mib'),'latency',row))
            self.assertEqual(standing.latency_keys(row,500),[10.,20.])
            self.assertTrue(standing.round_ok('latency',row))
        tiny = self.fixture()[0]
        tiny['typing_epoch']['typing_end_ns'] = 1_200_000_000
        tiny['typing_epoch']['end_mach_ns'] = 1_199_999_000
        tiny['samples'][-1]['t_post'] = 1_149_999_000
        self.assertIn('duration',self.memory(tiny,s,a)['typing_memory_reason'])

    def results(self):
        _,_,artifact = self.fixture()
        row = dict(samples_ms=[10.,20.], keys=2, censored=0, seq_mismatch=0, refresh_hz=60,
                   tool_artifact=artifact, **self.memory())
        return dict(schema=3,context='fixture',terminals=['kettle-a','kettle-b'],
            meta=dict(date='2026-09-30',mode='ab',complete=True,rounds={'latency':3},
                latency=dict(censor_ms=500,typing_memory=standing.hc.typing_method()),
                tool_hashes={'latency-probe':artifact['bundle_sha256']},tool_artifacts={'latency-probe':artifact}),
            workloads={'latency':{'kettle-a':[dict(row) for _ in range(3)],
                                   'kettle-b':[dict(row,typing_footprint_mib=8.) for _ in range(3)]}})


    def test_unmarked_typing_values_still_need_verified_provenance(self):
        # Dropping the typing_memory_valid marker must not skip the method,
        # artifact and bundle checks while typing values remain.
        import tempfile
        result = self.results()
        source_only = {"source_sha256": "a" * 64}
        for rows in result["workloads"]["latency"].values():
            for row in rows:
                row.pop("typing_memory_valid", None)
                row["typing_footprint_mib"] = 16.0
                row["tool_artifact"] = source_only
                row["typing_tool_artifact"] = source_only
        result["meta"]["tool_artifacts"]["latency-probe"] = source_only
        result["meta"]["tool_hashes"]["latency-probe"] = "a" * 64
        info = standing.analyze(result, result["terminals"], True)["latency"]["metrics"]["typing_footprint_mib"]
        self.assertFalse(any(cell.get("countable") for cell in info["metric_countable"].values()),
                         info["metric_countable"])
        # Both the per-row and the per-session checks must fire.
        for cell in info["metric_countable"].values():
            self.assertIn("verified typing probe artifact mismatch", cell["reasons"])
            self.assertIn("typing memory method/artifact mismatch", cell["reasons"])
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder)
            (path / "results.json").write_text(_json.dumps(result))
            cell = standing.combine([path], aa=str(path))["rows"]["latency.typing_footprint_mib"]
            self.assertNotIn("aa", cell)
    def test_scalar_summary_combine_and_fill_input(self):
        import tempfile
        r = self.results()
        info = standing.analyze(r,r['terminals'],True)['latency']['metrics']['typing_footprint_mib']
        self.assertEqual(info['ab']['ratio'],.5)
        self.assertEqual(info['ab_diff']['diff'],-8.)
        self.assertEqual(info['descriptor']['unit'],'MiB')
        self.assertEqual(info['descriptor']['aa_kind'],'ratio')
        self.assertEqual(info['terminals']['kettle-a']['n'],3)
        self.assertIn('typing_footprint_mib (MiB)',standing.summarize(r,r['terminals'],True))
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp);(folder/'results.json').write_text(_json.dumps(r))
            result = standing.combine([folder],aa=str(folder))
            # This typed combined row is the deterministic publication/fill
            # input. The generic fill command is a later freeze PR.
            cell = result['rows']['latency.typing_footprint_mib']
            self.assertEqual(cell['descriptor']['unit'],'MiB')
            self.assertEqual(cell['per_session'][0]['ab_diff']['diff'],-8.)
            self.assertNotIn('gate_ms',cell)
            self.assertTrue(cell['per_session'][0]['metric_countable']['kettle-a']['countable'])
            self.assertEqual(cell['aa']['gate'], .03)
        # A floor has no memory row; opaque remains unranked for memory.
        r['terminals']=['kettle','kitty'];r['unranked']=['kettle-opaque','floor-layer']
        a,b = r['workloads']['latency'].values()
        r['workloads']['latency']={'kettle':a,'kitty':b,'kettle-opaque':a,'floor-layer':[
            dict(samples_ms=[10.],keys=1,refresh_hz=60) for _ in range(3)]}
        info=standing.analyze(r,r['terminals'],False)['latency']['metrics']['typing_footprint_mib']
        self.assertNotIn('floor-layer',info['terminals'])
        self.assertTrue(all('kettle-opaque' not in (c['base'],c['test']) for c in info['pairwise']))

    def test_clock_interval_and_bundle_reject_calibration(self):
        import copy,tempfile
        p,s,a = self.fixture()
        for field,value in (('clock','wall'),('probe_clock','CLOCK_UPTIME_RAW'),('typing_start_ns',999),('window_id',8)):
            changed=copy.deepcopy(p);changed['typing_epoch'][field]=value
            self.assertFalse(self.memory(changed,s,a)['typing_memory_valid'])
        for delta in (2_000_000,-2_000_000):
            changed=copy.deepcopy(p);changed['typing_epoch']['clock_after']['mach_ns']+=delta
            self.assertFalse(self.memory(changed,s,a)['typing_memory_valid'])
        changed=copy.deepcopy(p);changed['typing_epoch']['clock_before']['raw_after_ns']+=2_000_000
        changed['typing_epoch']['clock_before']['raw_before_ns']-=2_000_000
        self.assertFalse(self.memory(changed,s,a)['typing_memory_valid'])
        changed=copy.deepcopy(s);changed[4]['scheduled_ns']-=1
        self.assertIn('interval',self.memory(p,changed,a)['typing_memory_reason'])
        self.assertFalse(self.memory(sample_ms=50)['typing_memory_valid'])
        self.assertFalse(self.memory(artifact={'source_sha256':'a'*64})['typing_memory_valid'])
        for mutate in ('method','bundle','source-only'):
            r=self.results()
            if mutate=='method':r['meta']['latency']['typing_memory']['clock']='wall'
            elif mutate=='bundle':r['meta']['tool_hashes']['latency-probe']='a'*64
            else:r['meta']['tool_artifacts']['latency-probe']={'source_sha256':'a'*64}
            info=standing.analyze(r,r['terminals'],True)['latency']['metrics']['typing_footprint_mib']
            self.assertFalse(info['metric_countable']['kettle-a']['countable'])
            with tempfile.TemporaryDirectory() as tmp:
                folder=Path(tmp);(folder/'results.json').write_text(_json.dumps(r))
                self.assertNotIn('aa',standing.combine([folder],aa=str(folder))['rows']['latency.typing_footprint_mib'])

    def test_probe_epoch_placement_and_classifier_preserved(self):
        import tempfile
        if sys.platform != 'darwin' or not shutil.which('swiftc'):
            self.skipTest('typing campaign fixture needs macOS swiftc')
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp)/'latency-probe'
            build = subprocess.run(['swiftc', '-O', str(HERE/'macos-standing/latency-probe.swift'),
                                    '-o', str(binary)], capture_output=True, text=True, timeout=60)
            self.assertEqual(build.returncode, 0, build.stderr)
            output = Path(tmp)/'typing.json'
            run = subprocess.run([str(binary), '--self-test-typing', str(output)], capture_output=True, text=True, timeout=10)
            self.assertEqual(run.returncode, 0, run.stderr)
            row = _json.loads(output.read_text())
            self.assertEqual(row['calibration_posts'], 6)
            self.assertEqual(row['box'], [40,30,128,68])
            self.assertAlmostEqual(row['calibration'][0],230)
            self.assertAlmostEqual(row['calibration'][1],20)
            samples, events = row['samples'], row['events']
            self.assertEqual([s['seq'] for s in samples], list(range(7,29)))
            self.assertEqual([s['warmup'] for s in samples], [True]*20+[False]*2)
            self.assertEqual(row['start_mach_ns'], samples[20]['t_post'])
            self.assertEqual(row['typing_start_ns'], samples[20]['t_post']-300)
            self.assertEqual(samples[20]['display']-samples[20]['t_post'], 2_000_000)
            self.assertEqual(samples[20]['mixed'], 1)
            self.assertTrue(samples[21]['censored'])
            self.assertIsNone(samples[21]['display'])
            guard = [e for e in events if e['kind']=='guard'][-1]
            self.assertEqual(row['end_mach_ns'], guard['at'])
            self.assertEqual(row['typing_end_ns'], guard['at']-300)
            self.assertEqual(guard['at']-samples[21]['t_post'], 590_000_000)
            self.assertEqual(events[-2]['kind'], 'guard')
            self.assertEqual(events[-1]['kind'], 'sleep')
            self.assertEqual(events[-1]['at'], row['end_mach_ns'])
            # Independent SplitMix64 sequence fixes all 22 post-key gaps.
            state, gaps = 7, []
            mask = (1<<64)-1
            for _ in range(22):
                state = (state+0x9E3779B97F4A7C15)&mask
                z = ((state^(state>>30))*0xBF58476D1CE4E5B9)&mask
                z = ((z^(z>>27))*0x94D049BB133111EB)&mask
                gaps.append((100+((z^(z>>31))%201))*1_000_000)
            actual = [events[i+1]['ns'] for i,e in enumerate(events) if e['kind']=='guard']
            self.assertEqual(actual, gaps)
            failed = subprocess.run([str(binary),'--self-test-typing',str(output),'--fail-guard'],
                                    capture_output=True,text=True,timeout=10)
            self.assertEqual(failed.returncode,1)
            self.assertEqual(failed.stdout,'')
            rejected = _json.loads(output.read_text())
            self.assertEqual(set(rejected),{'error'})
            self.assertIn('synthetic focus loss',rejected['error'])

    def test_payload_initialization_and_frames(self):
        import tempfile
        if sys.platform != 'darwin' or not shutil.which('clang'):
            self.skipTest('payload fixture needs macOS clang')
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp); fixture=work/'payload.c'; binary=work/'payload'
            # Replace only OS calls, then execute the unchanged payload main.
            # The fake tty writes stdout; input is three scratch bytes then EOF.
            fixture.write_text(r'''#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
static int fixture_open(const char *path, int flags, ...) {
    return strcmp(path, "/dev/tty") == 0 ? 100 : open(path, flags, 0600);
}
static ssize_t fixture_read(int fd, void *buf, size_t n) {
    static int count=0;
    if (count++ == 3) return 0;
    *(char *)buf='j'; return 1;
}
static ssize_t fixture_write(int fd, const void *buf, size_t n) {
    return write(fd == 100 ? 1 : fd, buf, n);
}
static int fixture_tcgetattr(int fd, struct termios *t) { memset(t,0,sizeof *t); return 0; }
static int fixture_tcsetattr(int fd, int action, const struct termios *t) { return 0; }
static void (*fixture_signal(int sig, void (*handler)(int)))(int) { return handler; }
#define open fixture_open
#define read fixture_read
#define write fixture_write
#define tcgetattr fixture_tcgetattr
#define tcsetattr fixture_tcsetattr
#define signal fixture_signal
#define main payload_main
#include "''' + str(HERE/'macos-standing/keyblock.c') + r'''"
#undef main
int main(int argc, char **argv) { return payload_main(argc,argv); }
''')
            build=subprocess.run(['clang','-O','-o',str(binary),str(fixture)],capture_output=True,text=True,timeout=30)
            self.assertEqual(build.returncode,0,'native probe build failed: '+build.stderr)
            log=work/'log'
            run=subprocess.run([str(binary),str(log)],capture_output=True,timeout=10)
            self.assertEqual(run.returncode,0,run.stderr)
            def frame(on):
                return b''.join(f'\x1b[{r};53H\x1b[{7 if on else 27}m'.encode()+b' '*16
                                for r in range(17,21))+b'\x1b[0m'
            self.assertEqual(run.stdout,b'\x1b[?25l\x1b[2 q\x1b[0m\x1b[2J\x1b[H'+
                             frame(False)+frame(True)+frame(False)+frame(True))
            records=standing.read_keyblock_log(log)
            self.assertEqual(sorted(records),[1,2,3])
            for seq,(count,before,after) in records.items():
                self.assertEqual(count,1)
                self.assertLessEqual(before,after)

    def test_floor_has_no_observer_or_memory(self):
        import tempfile,contextlib
        from unittest.mock import Mock,patch
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp);runner=standing.Runner({k:work/k for k in ('latency-floor','observer','latency-probe')},work,{})
            runner.launch=Mock(return_value=Mock());runner.wait_for=Mock(return_value=True)
            runner.pid=Mock(return_value=42);runner.stop=Mock(return_value=True)
            p,_,a=self.fixture()
            p['samples'] = [dict(seq=i,warmup=True,t_post=800_000_000,display=810_000_000) for i in range(7,27)] + p['samples']
            def probe(*args):(work/'latency.json').write_text(_json.dumps(p))
            @contextlib.contextmanager
            def verified(*args):yield a
            with patch.object(standing.hc,'start_observer') as start,patch.object(standing,'run_latency_probe',side_effect=probe),\
                 patch.object(standing,'verified_probe_use',verified),patch.object(standing,'wait_for_text',return_value=True),\
                 patch.object(standing.time,'sleep'),patch.object(standing,'read_keyblock_log',return_value={i:(1,1_000_000_000,1_000_000_000) for i in range(1,29)}):
                row=runner.latency('floor-layer',dict(keys=2,warmup=20,censor_ms=500,inject='hid'),7,work/'keep.json')
            start.assert_not_called()
            self.assertTrue(runner.launch.call_args.kwargs['argv'])
            self.assertIsNone(runner.observation_context)
            self.assertEqual(standing.latency_keys(row,500),[10.,500.])
            self.assertFalse(any(k.startswith('typing_') for k in row))

    def test_invalid_launch_context_is_metric_local(self):
        import tempfile, contextlib
        from unittest.mock import Mock, patch
        for context in (None, [], 42, "path-private", {}, {'pid':42,'window_id':0}):
            with self.subTest(context=context), tempfile.TemporaryDirectory() as tmp:
                work=Path(tmp)
                runner=standing.Runner({k:work/k for k in ('keyblock','observer','latency-probe')},work,{})
                def launch(*args,**kwargs):
                    (work/'typing-launch.json').write_text(_json.dumps(context))
                    return Mock()
                runner.launch=Mock(side_effect=launch);runner.wait_for=Mock(return_value=True)
                runner.pid=Mock(return_value=42);runner.stop=Mock(return_value=True)
                runner.end_sampler=Mock()
                p,_,artifact=self.fixture()
                p['samples'] = [dict(seq=i,warmup=True,t_post=800_000_000,display=810_000_000)
                                for i in range(7,27)]+p['samples']
                def probe(*args):(work/'latency.json').write_text(_json.dumps(p))
                @contextlib.contextmanager
                def verified(*args):yield artifact
                with patch.object(standing.hc,'start_observer') as observer,\
                     patch.object(standing,'run_latency_probe',side_effect=probe),\
                     patch.object(standing,'verified_probe_use',verified),\
                     patch.object(standing,'wait_for_text',return_value=True),\
                     patch.object(standing.time,'sleep'),\
                     patch.object(standing,'read_keyblock_log',return_value={i:(1,1_000_000_000,1_000_000_000) for i in range(1,29)}):
                    row=runner.latency('kettle',dict(keys=2,warmup=20,censor_ms=500,inject='hid'),7)
                observer.assert_not_called();runner.end_sampler.assert_not_called()
                runner.stop.assert_called_once()
                self.assertNotIn('error',row)
                self.assertEqual(standing.latency_keys(row,500),[10.,500.])
                self.assertFalse(row['typing_memory_valid'])
                self.assertIsNone(row['typing_footprint_mib'])
                self.assertIsNone(runner.observation_context)

    def test_owned_observer_probe_failures_and_artifact_retention(self):
        import tempfile,contextlib
        from unittest.mock import Mock,patch
        for failure in (None,subprocess.TimeoutExpired('probe',1),RuntimeError('calibration failed'),KeyboardInterrupt()):
            with tempfile.TemporaryDirectory() as tmp:
                work=Path(tmp);runner=standing.Runner({k:work/k for k in ('keyblock','observer','latency-probe')},work,{})
                process,observer=Mock(),Mock();events=[]
                p,s,a=self.fixture()
                if failure is not None: p['error']='calibration failed'
                p['samples'] = [dict(seq=i,warmup=True,t_post=800_000_000,display=810_000_000) for i in range(7,27)] + p['samples']
                def launch(*args,**kwargs):
                    (work/'typing-launch.json').write_text(_json.dumps(dict(pid=42,window_id=7)))
                    return process
                runner.launch=Mock(side_effect=launch);runner.wait_for=Mock(return_value=True);runner.pid=Mock(return_value=42)
                runner.stop=Mock(side_effect=lambda *args:events.append('target') or True)
                runner.end_sampler=Mock(side_effect=lambda *args:events.append('observer'))
                def start(*args):
                    events.append('start');(work/'typing-memory.jsonl').write_text(''.join(_json.dumps(x)+'\n' for x in s))
                    return observer
                def probe(*args):
                    events.append('probe');(work/'latency.json').write_text(_json.dumps(p));(work/'keyblock.log').write_text('raw')
                    if failure:raise failure
                @contextlib.contextmanager
                def verified(*args):
                    events.append('verify-before')
                    try:yield a
                    finally:events.append('verify-after')
                with patch.object(standing.hc,'start_observer',side_effect=start),patch.object(standing,'run_latency_probe',side_effect=probe),\
                     patch.object(standing,'verified_probe_use',verified),patch.object(standing,'wait_for_text',return_value=True),\
                     patch.object(standing.time,'sleep'),patch.object(standing,'read_keyblock_log',return_value={i:(1,1_000_000_000,1_000_000_000) for i in range(1,29)}):
                    if failure and not isinstance(failure,subprocess.TimeoutExpired):
                        with self.assertRaises(type(failure)):runner.latency('kettle',dict(keys=2,warmup=20,censor_ms=500,inject='hid'),7,work/'keep.json')
                    else:
                        row=runner.latency('kettle',dict(keys=2,warmup=20,censor_ms=500,inject='hid'),7,work/'keep.json')
                        if failure is None:
                            self.assertNotIn('error',row)
                            self.assertEqual(standing.latency_keys(row,500),[10.,500.])
                            self.assertEqual(row['typing_footprint_mib'],16.)
                            self.assertEqual(row['keys'],2)
                            self.assertEqual(row['censored'],1)
                            import hashlib
                            self.assertEqual(row['typing_timeline_sha256'],hashlib.sha256((work/'keep.memory.jsonl').read_bytes()).hexdigest())
                            self.assertEqual(set(row['typing_artifacts']),{'probe','memory','keyblock','launch'})
                            self.assertTrue(all('/' not in v['name'] for v in row['typing_artifacts'].values()))
                        else:self.assertIn('error',row)
                self.assertEqual(events[-2:],['observer','target'])
                self.assertLess(events.index('start'),events.index('probe'))
                self.assertIn('verify-after',events)
                self.assertIsNone(runner.observation_context)
                self.assertTrue((work/'keep.memory.jsonl').is_file())
                self.assertTrue((work/'keep.keyblock.log').is_file())
                self.assertTrue((work/'keep.json').is_file())
                self.assertTrue((work/'keep.launch.json').is_file())


class CursorLatency(unittest.TestCase):
    def fixture(self, warmup=1, measured=3, durations=None):
        durations = durations or [9000] * warmup + [1000, 2000, 3000][:measured]
        launch, window, pane = '0123456789abcdef0123456789abcdef', 9, 2
        header = dict(event='capability', launch_id=launch, pane_id=pane, window_id=window,
                      clock='CLOCK_UPTIME_RAW', first_key_seq=7)
        samples, exits = [], []
        records = {i: (1, i, i) for i in range(1, 7)}
        for i, us in enumerate(durations):
            post = 1_000_000_000 + i * 3_000_000_000
            samples.append(dict(seq=i+7,warmup=i<warmup,t_post=post,censored=False,
                                display=post+10_000_000,arrival=post+9_000_000))
            records[i+7] = (1, post+1000, post+2000)
            exits.append(dict(event='exit',launch_id=launch,pane_id=pane,key_seq=i+7,
                t_start_ns=post+3000,t_end_ns=post+3000+us*1000,total_frame_us=us,layer_active=True))
        epoch = dict(clock='CLOCK_UPTIME_RAW',probe_clock='mach_absolute_time_ns',window_id=window,
            guards_ok=True,typing_end_ns=samples[-1]['t_post']+1_000_000_000,
            clock_before=dict(raw_before_ns=100,raw_after_ns=100,mach_ns=100),
            clock_after=dict(raw_before_ns=samples[-1]['t_post']+1_000_000_000,
                             raw_after_ns=samples[-1]['t_post']+1_000_000_000,
                             mach_ns=samples[-1]['t_post']+1_000_000_000))
        return header, exits, dict(samples=samples,typing_epoch=epoch,calibration_keys=6), records

    def inputs(self, header):
        return [dict(event='input', launch_id=header['launch_id'], pane_id=header['pane_id'], key_seq=i)
                for i in range(1, 7)]

    def wire(self, header, records):
        return ''.join('cursor_exit_v1 '+_json.dumps(r)+'\n' for r in [header,*records])

    def parse(self, header, exits, probe, records, warmup=1, measured=3):
        text = self.wire(header, [*self.inputs(header), *exits])
        return standing.cursor.parse_exits(text, header['launch_id'], 9, probe, records,
                                          warmup, measured, standing.percentile)

    def test_exit_coverage_and_identity(self):
        import copy
        h,e,p,r = self.fixture()
        row = self.parse(h,e,p,r)
        self.assertEqual((row['cursor_exit_count'],row['cursor_exit_measured_count']), (4,3))
        self.assertEqual(row['cursor_exit_max_us'],3000)
        self.assertEqual(row['cursor_exit_p95_us'],2900)
        self.assertTrue(standing.cursor.complete_exits([row],1))
        self.assertFalse(standing.cursor.complete_exits([row,{'cursor_exit_valid':False}],2))
        self.assertFalse(standing.cursor.complete_exits([row],2))
        self.assertFalse(standing.cursor.complete_exits([{**row,'error':'failed'}],1))
        fixture=HERE/'macos-standing/cursor-exits.fixture'
        self.assertEqual(standing.cursor.parse_exits(fixture.read_text(),h['launch_id'],9,p,r,1,3,standing.percentile),row)
        for label,change in [
            ('missing',lambda x:x.pop()), ('excess',lambda x:x.append(x[-1])),
            ('duplicate',lambda x:x.__setitem__(1,x[0])),
            ('shifted',lambda x:x[0].update(key_seq=8)),
            ('calibration',lambda x:x[0].update(key_seq=6)),
            ('negative',lambda x:x[0].update(total_frame_us=-1)),
            ('nonfinite',lambda x:x[0].update(total_frame_us=float('nan'))),
            ('fractional type',lambda x:x[0].update(total_frame_us=9000.0)),
            ('pane',lambda x:x[0].update(pane_id=3)),
            ('launch',lambda x:x[0].update(launch_id='other')),
            ('inactive',lambda x:x[0].update(layer_active=False)),
            ('endpoint',lambda x:x[0].update(t_end_ns=x[0]['t_start_ns']-1)),
            ('wrong interval',lambda x:x[0].update(t_start_ns=0)),
            ('wrong cost',lambda x:x[0].update(total_frame_us=100)),
        ]:
            x=copy.deepcopy(e);change(x)
            with self.subTest(label=label),self.assertRaises(ValueError):self.parse(h,x,p,r)
        for field,value in [('window_id',8),('pane_id',False),('clock','wall')]:
            with self.subTest(field=field),self.assertRaises(ValueError):self.parse({**h,field:value},e,p,r)
        for raw in ['cursor_exit_v1 '+_json.dumps(h), 'cursor_exit_v1 {bad}\n',
                    'exit_frame_us=100 render_frame_us=10\n',
                    'cursor_exit_v1 {"event":"exit","event":"exit"}\n']:
            with self.assertRaises(ValueError):standing.cursor.parse_exits(raw,h['launch_id'],9,p,r,1,3,standing.percentile)
        complete_text=self.wire(h,[*self.inputs(h),*e])
        with self.assertRaises(ValueError):standing.cursor.parse_exits(complete_text[:-1],h['launch_id'],9,p,r,1,3,standing.percentile)
        with self.assertRaises(ValueError):self.parse(h,[],p,r)
        missing=standing.cursor.parse_exits('ordinary warning\n',h['launch_id'],9,p,r,1,3,standing.percentile)
        self.assertFalse(missing['cursor_exit_available']);self.assertIsNone(missing['cursor_exit_p95_us'])
        with self.assertRaises(ValueError):self.parse(h,e[:1],p,r)
        for bad in [{**p,'samples':p['samples'][1:]}, {**p,'typing_epoch':{}},
                    {**p,'samples':[{**p['samples'][0],'warmup':False},*p['samples'][1:]]}]:
            with self.assertRaises(ValueError):self.parse(h,e,bad,r)

    def parse_wire(self, header, wire, probe, payload):
        return standing.cursor.parse_exits(self.wire(header, wire), header['launch_id'], 9,
                                           probe, payload, 1, 3, standing.percentile)

    def test_calibration_input_records_pass(self):
        h,e,p,r = self.fixture()
        row = self.parse_wire(h, [*self.inputs(h), *e], p, r)
        self.assertTrue(row['cursor_exit_valid'])
        self.assertEqual(row['cursor_exit_records'], e)
        # Input arrival order need not match key order when a frame coalesces.
        self.assertEqual(self.parse_wire(h, [*reversed(self.inputs(h)), *e], p, r), row)

    def test_cmd_c_after_final_exit_invalidates_campaign(self):
        h,e,p,r = self.fixture()
        extra = dict(event='input', launch_id=h['launch_id'], pane_id=h['pane_id'], key_seq=11)
        with self.assertRaises(ValueError):
            self.parse_wire(h, [*self.inputs(h), *e, extra], p, r)

    def test_input_in_measured_range_fails(self):
        h,e,p,r = self.fixture()
        cancelled = dict(event='input', launch_id=h['launch_id'], pane_id=h['pane_id'], key_seq=8)
        with self.assertRaises(ValueError):
            self.parse_wire(h, [*self.inputs(h), e[0], cancelled, *e[2:]], p, r)
        with self.assertRaises(ValueError):
            self.parse_wire(h, [*self.inputs(h), *e, cancelled], p, r)
        # Coalesced key 8 is logged before the earlier key 7's frame finishes.
        with self.assertRaises(ValueError):
            self.parse_wire(h, [*self.inputs(h), cancelled, e[0], *e[2:]], p, r)

    def test_input_exit_gaps_and_duplicates_fail(self):
        h,e,p,r = self.fixture()
        inputs = self.inputs(h)
        for label, wire in [
            ('missing calibration', [*inputs[1:], *e]),
            ('missing exit', [*inputs, *e[:2], e[-1]]),
            ('duplicate calibration', [*inputs, inputs[0], *e]),
            ('duplicate exit', [*inputs, *e, e[0]]),
            ('cross event duplicate', [*inputs, *e, {**inputs[0], 'key_seq':7}]),
            ('all calibration missing', e),
        ]:
            with self.subTest(label=label), self.assertRaises(ValueError):
                self.parse_wire(h, wire, p, r)

    def test_input_schema_and_identity_fail(self):
        h,e,p,r = self.fixture()
        for change in [dict(pane_id=3), dict(launch_id='other'), dict(pane_id=True),
                       dict(key_seq=True), dict(key_seq=0), dict(key_seq=1.0),
                       dict(key_seq=2**64), dict(extra=0), dict(event='exit')]:
            inputs = self.inputs(h)
            inputs[0] = {**inputs[0], **change}
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.parse_wire(h, [*inputs, *e], p, r)
        inputs = self.inputs(h); del inputs[0]['pane_id']
        with self.assertRaises(ValueError):
            self.parse_wire(h, [*inputs, *e], p, r)

    def test_cursor_stream_strict_payload(self):
        h,e,p,r=self.fixture()
        raw=b''.join(standing.KEYBLOCK_RECORD.pack(seq,*v) for seq,v in r.items())
        self.assertEqual(standing.cursor.read_payload(raw),r)
        standing.cursor.validate_stream(p,r,1,3)
        for data in [raw+b'x',raw+raw[:32]]:
            with self.assertRaises(ValueError):standing.cursor.read_payload(data)
        for bad in [{**p,'calibration_keys':5}, {**p,'samples':p['samples'][1:]},
                    {**p,'samples':[{**p['samples'][0],'seq':8},*p['samples'][1:]]}]:
            with self.assertRaises(ValueError):standing.cursor.validate_stream(bad,r,1,3)
        with self.assertRaises(ValueError):standing.cursor.validate_stream(p,{**r,7:(2,1,1)},1,3)

    def test_quiet_gaps_budget_and_selection(self):
        c=standing.cursor
        self.assertEqual(c.method('cursor'),dict(payload='cursor',gap_ms=[2000,2400],first_gap_ms=2000))
        self.assertEqual(c.method(),dict(payload='block',gap_ms=[100,300],first_gap_ms=0))
        for gap,first in [('1499:2400',2000),('2000:5001',2000),('2400:2000',2000),
                          ('2000:2400',1499),('1:2:3',2000)]:
            with self.assertRaises(ValueError):c.method('cursor',gap,first)
        options=dict(keys=1000,warmup=200,censor_ms=5000,**c.method('cursor','2000:5000',10000))
        b=c.budget(options)
        self.assertGreater(b['probe_s'],12000)
        self.assertGreater(b['launch_s'],b['wait_s']+15)
        self.assertGreater(b['wait_s'],b['probe_s'])
        self.assertEqual(standing.select_latency_workloads(['latency','latency-cursor'],'block',True),['latency','latency-cursor'])
        self.assertEqual(standing.select_latency_workloads(['latency'],'cursor',False),['latency-cursor'])
        with self.assertRaises(ValueError):standing.select_latency_workloads(['latency','latency-cursor'],'cursor',False)
        with self.assertRaises(ValueError):standing.select_latency_workloads(['latency'],'block',True)
        self.assertEqual(standing.latency_entries(['kettle','kitty'],False,True,['ca'],'cursor'),['kettle'])
        self.assertEqual(standing.latency_entries(['kettle-a','kettle-b'],True,True,['ca'],'cursor'),['kettle-a','kettle-b'])
        import argparse,contextlib,tempfile
        from unittest.mock import Mock,patch
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);runner=standing.Runner({k:root/k for k in ('keyblock','latency-probe')},root,{})
            def launch(name,body,lifetime,**kw):
                self.assertEqual(lifetime,b['launch_s'])
                self.assertIn(str(int(lifetime*1000)),body)
                (root/'typing-launch.json').write_text(_json.dumps(dict(pid=42,window_id=9)))
                return Mock()
            @contextlib.contextmanager
            def verified(*args):yield {}
            def run(probe,args,work,timeout):
                self.assertEqual(timeout,b['wait_s'])
                self.assertEqual(args[args.index('--deadline-ms')+1],str(int(b['probe_s']*1000)))
                self.assertEqual(args[args.index('--initial-gap-ms')+1],'10000')
                self.assertEqual(args[args.index('--gap-ms')+1],'2000:5000')
            with patch.object(runner,'launch',side_effect=launch),patch.object(runner,'wait_for',return_value=True), \
                 patch.object(runner,'pid',return_value=42),patch.object(runner,'stop',return_value=True), \
                 patch.object(standing,'verified_probe_use',verified),patch.object(standing,'run_latency_probe',side_effect=run), \
                 patch.object(standing,'wait_for_text',return_value=False),patch.object(standing.time,'sleep'), \
                 patch.object(standing.hc,'start_observer') as observer:
                runner.latency('kettle',{**options,'inject':'hid'},7)
                observer.assert_not_called()
        args=argparse.Namespace(rounds=None,workloads='latency,latency-cursor',startup_rounds=30,
            idle_rounds=5,flood_rounds=5,vtebench_rounds=5,latency_rounds=7,cursor_rounds=10)
        self.assertEqual(standing.resolve_rounds(args)['latency-cursor'],10)
        args.rounds=3;self.assertEqual(standing.resolve_rounds(args)['latency-cursor'],3)

    def test_handshake_hidden_calibration_and_guard_order(self):
        import tempfile
        if sys.platform != 'darwin' or not shutil.which('swiftc'):
            self.skipTest('cursor campaign fixture needs macOS swiftc')
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);binary=root/'probe'
            build=subprocess.run(['swiftc','-O',str(HERE/'macos-standing/latency-probe.swift'),
                '-o',str(binary)],capture_output=True,text=True,timeout=60)
            self.assertEqual(build.returncode,0,'native probe build failed: '+build.stderr)
            pure=subprocess.run([str(binary),'--self-test'],capture_output=True,text=True,timeout=10)
            self.assertEqual(pure.returncode,0,pure.stderr)
            for scenario,reason in [('normal',None),('late-query','deadline passed'),
                                    ('cancel-query','invocation cancelled'),('changed-ack','cursor acknowledgment changed')]:
                with self.subTest(scenario=scenario):
                    out=root/(scenario+'.json')
                    with standing.probe_invocation_lease(root) as lease:
                        run=subprocess.run([str(binary),'--self-test-cursor',str(out),scenario,str(lease)],
                            capture_output=True,text=True,timeout=10)
                    self.assertEqual(run.returncode,0,run.stderr)
                    row=_json.loads(out.read_text());events=row['events']
                    posts=[e for e in events if e['kind']=='post']
                    accepted=next(e for e in events if e['kind']=='accepted')
                    enabled=next(e for e in events if e['kind']=='enable')
                    self.assertEqual(accepted['posts'],6);self.assertEqual(enabled['posts'],6)
                    self.assertLess(events.index(accepted),events.index(enabled))
                    if reason:
                        self.assertIn('error',row,'guard failure must reject the stream')
                        self.assertEqual(row['error'],reason)
                        self.assertEqual(len(posts),6,'guard failure must prevent the stream key')
                    else:
                        self.assertEqual(row['initial_gap_ms'],2000)
                        self.assertEqual(posts[6]['at']-row['enabled_ns'],2_000_000_000)
                        self.assertEqual([r['seq'] for r in row['samples']],list(range(7,11)))
                        self.assertEqual([r['warmup'] for r in row['samples']],[True,True,False,False])
                        self.assertEqual(row['start_mach_ns'],posts[8]['at'])
                        self.assertEqual(row['end_mach_ns'],[e for e in events if e['kind']=='guard'][-1]['at'])
                        # Compute seeded gaps independently, rather than read source.
                        state=7;expected=[];mask=(1<<64)-1
                        for _ in range(4):
                            state=(state+0x9E3779B97F4A7C15)&mask
                            z=((state^(state>>30))*0xBF58476D1CE4E5B9)&mask
                            z=((z^(z>>27))*0x94D049BB133111EB)&mask
                            expected.append((2000+((z^(z>>31))%401))*1_000_000)
                        gaps=[events[i+1]['ns'] for i,e in enumerate(events) if e['kind']=='guard']
                        self.assertEqual(gaps,expected)
        self.test_native_cursor_handshake_on_owned_socket()

    def test_payload_mode_bytes_independently(self):
        self.test_native_cursor_handshake_on_owned_socket()
        import hashlib,re
        # Independent bytes for each mode, including all six hidden flips.
        def frame(on):
            return b''.join(f'\x1b[{row};53H\x1b[{7 if on else 27}m'.encode()+b' '*16 for row in range(17,21))+b'\x1b[0m'
        init=b'\x1b[?25l\x1b[2 q\x1b[0m\x1b[2J\x1b[H'+frame(False)
        expected=[init,*[frame(i%2==0) for i in range(6)],standing.cursor.ENABLE,frame(True)+b'\x1b[2;3H']
        hashes=[hashlib.sha256(v).hexdigest() for v in expected]
        self.assertEqual(hashes,_json.loads((HERE/'macos-standing/cursor-frames.fixture').read_text()))
        if sys.platform=='darwin' and shutil.which('clang'):
            import tempfile
            with tempfile.TemporaryDirectory() as tmp:
                path=Path(tmp);c=path/'frames.c';binary=path/'frames'
                c.write_text('#define main payload_main\n#include '+_json.dumps(str(HERE/'macos-standing/keyblock.c'))+
                    '\n#undef main\nint main(void) { char a[512],b[768]; size_t n;'
                    ' n=frame(a,sizeof a,0); fputs("\\033[?25l\\033[2 q\\033[0m\\033[2J\\033[H",stdout); fwrite(a,1,n,stdout); fputc(0,stdout);'
                    ' for(int i=0;i<6;i++){n=frame(a,sizeof a,i%2==0);n=cursor_frame(b,a,n,0);fwrite(b,1,n,stdout);fputc(0,stdout);}'
                    ' fputs("\\033[?25h\\033[1 q\\033[2;3H",stdout);fputc(0,stdout);'
                    ' n=frame(a,sizeof a,1);n=cursor_frame(b,a,n,1);fwrite(b,1,n,stdout);fputc(0,stdout);'
                    ' return !(may_enable(6,0) && !may_enable(5,0) && !may_enable(6,1));}\n')
                subprocess.run(['clang','-Wall','-Wextra','-Werror','-O','-o',str(binary),str(c)],check=True,capture_output=True)
                actual=subprocess.check_output([str(binary)]).split(b'\0')[:-1]
                self.assertEqual(actual,expected)

    def test_pooled_percentiles_warmup_and_gate(self):
        h,e,p,r=self.fixture();row=self.parse(h,e,p,r)
        warm={**row,'warmup':True}
        huge={**row,'error':'failed','cursor_exit_records':[{**e[-1],'total_frame_us':99999}]}
        pooled=standing.cursor.pooled([warm,row,row,huge],standing.percentile)
        self.assertEqual(pooled,dict(count=6,p50_us=2000.,p95_us=3000.,max_us=3000,p95_le_4000=True))
        row2={**row,'cursor_exit_count':2,'cursor_exit_measured_count':1,'cursor_exit_p95_us':9000,
              'cursor_exit_records':[e[0],{**e[1],'total_frame_us':9000}]}
        mixed=standing.cursor.pooled([row,row2],standing.percentile)
        self.assertAlmostEqual(mixed['p95_us'],8100)
        self.assertNotEqual(mixed['p95_us'],(2900+9000)/2)
        self.assertFalse(mixed['p95_le_4000'])
        boundary={**row2,'cursor_exit_records':[e[0],{**e[1],'total_frame_us':4000}]}
        self.assertTrue(standing.cursor.pooled([boundary],standing.percentile)['p95_le_4000'])

    def test_two_namespaces_unranked_cursor_and_no_typing_cells(self):
        rows={name:[latency_run([10.,20.]) for _ in range(3)] for name in ['kettle-a','kettle-b']}
        r=dict(schema=3,context='fixture',terminals=list(rows),meta=dict(rounds={'latency':3,'latency-cursor':3}),
               workloads={'latency':rows,'latency-cursor':rows})
        info=standing.analyze(r,r['terminals'],True)
        self.assertEqual(info['latency']['metrics']['mean_ms']['descriptor']['id'],'latency.mean_ms')
        self.assertEqual(info['latency-cursor']['metrics']['mean_ms']['descriptor']['id'],'latency-cursor.mean_ms')
        self.assertIn('ab',info['latency-cursor']['metrics']['mean_ms'])
        self.assertEqual(info['latency-cursor']['metrics']['mean_ms']['pairwise'],[])
        names=['kettle','kitty'];block=standing.latency_entries(names,False,True,['ca'])
        cursor=standing.latency_entries(names,False,True,['ca'],'cursor')
        self.assertEqual(standing.workload_entries('latency-cursor',names,block,cursor),['kettle'])
        self.assertEqual(standing.workload_entries('latency',names,block,cursor),['kettle','kitty','kettle-opaque','floor-ca'])
        self.assertNotIn('typing_footprint_mib',info['latency-cursor']['metrics'])
        self.assertIn('diagnostic and unranked',standing.summarize(r,r['terminals'],True))

    def test_cursor_runner_cancellation_closes_lease_before_target(self):
        # The production owned-open helper is already exercised with repeated
        # cancellation. Here select cursor specifically and verify teardown.
        import contextlib,tempfile
        from unittest.mock import Mock,patch
        self.test_cursor_success_links_exact_retained_bytes()
        for failure in [KeyboardInterrupt(),subprocess.TimeoutExpired('probe',1),RuntimeError('guard trip')]:
            with tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp);runner=standing.Runner({k:root/k for k in ('keyblock','latency-probe')},root,{})
                process=Mock();events=[]
                def launch(*args,**kwargs):
                    (root/'typing-launch.json').write_text(_json.dumps(dict(pid=42,window_id=9)))
                    return process
                @contextlib.contextmanager
                def verified(*args):
                    events.append('verify-before')
                    try:yield {}
                    finally:events.append('verify-after')
                def run(*args):
                    events.append('lease-closed');raise failure
                def stop(*args):events.append('target');return True
                with patch.object(runner,'launch',side_effect=launch),patch.object(runner,'wait_for',return_value=True),\
                     patch.object(runner,'pid',return_value=42),patch.object(runner,'stop',side_effect=stop),\
                     patch.object(standing,'verified_probe_use',verified),patch.object(standing,'run_latency_probe',side_effect=run),\
                     patch.object(standing,'wait_for_text',return_value=False),patch.object(standing.time,'sleep'),\
                     patch.object(standing.hc,'start_observer') as observer:
                    options=dict(keys=3,warmup=1,censor_ms=500,inject='hid',**standing.cursor.method('cursor'))
                    if isinstance(failure,subprocess.TimeoutExpired):self.assertIn('error',runner.latency('kettle',options,7))
                    else:
                        with self.assertRaises(type(failure)):runner.latency('kettle',options,7)
                    observer.assert_not_called()
                self.assertEqual(events,['verify-before','lease-closed','verify-after','target'])
                self.assertFalse((root/'cursor.control').exists());self.assertIsNone(runner.observation_context)

    def test_cursor_aa_method_is_distinct_and_gaps_are_guarded(self):
        import tempfile,copy
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);aa=root/'control';ab=root/'candidate';aa.mkdir();ab.mkdir()
            rows={n:[latency_run([10.,20.]) for _ in range(3)] for n in ['kettle-a','kettle-b']}
            base=dict(schema=3,context='fixture',terminals=list(rows),meta=dict(date='2026-09-30',label='fixture',
                complete=True,mode='ab',rounds={'latency-cursor':3},harness_clean=True,harness_tree='fixture',
                identity={'kettle-a':{'sha256':'same'},'kettle-b':{'sha256':'same'}},configs={},
                **{'latency-cursor':dict(keys=2,warmup=20,censor_ms=500,inject='hid',**standing.cursor.method('cursor'))}),
                workloads={'latency-cursor':rows})
            (aa/'results.json').write_text(_json.dumps(base));(ab/'results.json').write_text(_json.dumps(base))
            combined=standing.combine([ab],aa)
            self.assertIn('aa',combined['rows']['latency-cursor.mean_ms'])
            for field,value in [('gap_ms',[2000,2401]),('first_gap_ms',2001),('exit_logs',True)]:
                bad=copy.deepcopy(base);bad['meta']['latency-cursor'][field]=value
                (ab/'results.json').write_text(_json.dumps(bad))
                with self.assertRaises(SystemExit):standing.combine([ab],aa)
            block=copy.deepcopy(base);block['meta'].pop('latency-cursor');block['workloads']={'latency':rows};block['meta']['rounds']={'latency':3}
            (aa/'results.json').write_text(_json.dumps(block));(ab/'results.json').write_text(_json.dumps(base))
            self.assertNotIn('aa',standing.combine([ab],aa)['rows']['latency-cursor.mean_ms'])

    @unittest.skipUnless(sys.platform=='darwin' and shutil.which('clang'), 'needs macOS clang')
    def test_native_cursor_handshake_on_owned_socket(self):
        # Replace only the tty device and termios syscalls with a private
        # socket. The payload's real read/poll/control/log path still runs.
        import socket,select,tempfile
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source=root/'socket-payload.c';binary=root/'socket-payload'
            source.write_text('#include <fcntl.h>\n#include <termios.h>\n#include <unistd.h>\n'
                'int scratch_open(const char*,int,...);\nint scratch_get(int,struct termios*);\nint scratch_set(int,int,const struct termios*);\n'
                'int scratch_isatty(int);\npid_t scratch_sid(int);\n'
                '#define open scratch_open\n#define tcgetattr scratch_get\n#define tcsetattr scratch_set\n'
                '#define isatty scratch_isatty\n#define tcgetsid scratch_sid\n'
                '#define main payload_main\n#include '+_json.dumps(str(HERE/'macos-standing/keyblock.c'))+
                '\n#undef main\n#undef open\n#undef tcgetattr\n#undef tcsetattr\n#undef isatty\n#undef tcgetsid\n#include <stdarg.h>\n'
                # The socket stands in for this session's controlling terminal.
                'int scratch_isatty(int fd){(void)fd;return 1;}'
                'pid_t scratch_sid(int fd){(void)fd;return getsid(0);}'
                'int scratch_open(const char *p,int flags,...){if(!strcmp(p,"/dev/tty"))return dup(0);'
                'if(flags&O_CREAT){va_list ap;va_start(ap,flags);int mode=va_arg(ap,int);va_end(ap);return open(p,flags,mode);}return open(p,flags);}'
                'int scratch_get(int fd,struct termios *t){(void)fd;memset(t,0,sizeof *t);return 0;}'
                'int scratch_set(int fd,int action,const struct termios *t){(void)fd;(void)action;(void)t;return 0;}'
                'int main(int argc,char **argv){return payload_main(argc,argv);}\n')
            subprocess.run(['clang','-Wall','-Wextra','-Werror','-O','-o',str(binary),str(source)],check=True,capture_output=True)
            def frame(on):
                return b''.join(f'\x1b[{r};53H\x1b[{7 if on else 27}m'.encode()+b' '*16 for r in range(17,21))+b'\x1b[0m'
            init=b'\x1b[?25l\x1b[2 q\x1b[0m\x1b[2J\x1b[H'+frame(False)
            for case in ['enable','early','duplicate','missing','torn']:
                with self.subTest(case=case):
                    fifo=root/(case+'.fifo');ack=root/(case+'.ack');log=root/(case+'.log')
                    os.mkfifo(fifo,0o600);parent,child=socket.socketpair();parent.settimeout(3)
                    process=subprocess.Popen([str(binary),str(log),'cursor',str(fifo),str(ack),'1800'],
                        stdin=child,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
                    child.close()
                    def receive(expected):
                        actual=b''
                        while len(actual)<len(expected):
                            try:chunk=parent.recv(len(expected)-len(actual))
                            except TimeoutError:self.fail('timed out waiting for owned payload bytes')
                            self.assertTrue(chunk,'owned socket closed early');actual+=chunk
                        self.assertEqual(actual,expected)
                    try:
                        receive(init)
                        for i in range(5 if case=='early' else 6):
                            parent.sendall(b'j');receive(frame(i%2==0))
                        self.assertFalse(select.select([parent],[],[],.05)[0], 'read six must stay hidden')
                        if case=='missing':
                            self.assertEqual(process.wait(timeout=3),1)
                        else:
                            control=os.open(fifo,os.O_WRONLY|os.O_NONBLOCK)
                            try:os.write(control,b'BAD\n' if case=='torn' else b'ENABLE\n')
                            finally:os.close(control)
                            if case in ['early','torn']:
                                self.assertEqual(process.wait(timeout=3),1);self.assertFalse(ack.exists())
                            else:
                                receive(standing.cursor.ENABLE)
                                until=time.monotonic()+2
                                while not ack.exists() and time.monotonic()<until:time.sleep(.01)
                                self.assertRegex(ack.read_text(),r'^ENABLED [0-9]+\n$')
                                self.assertEqual(len(log.read_bytes()),6*32)
                                parent.sendall(b'j');receive(frame(True)+b'\x1b[2;3H')
                                if case=='duplicate':
                                    control=os.open(fifo,os.O_WRONLY|os.O_NONBLOCK)
                                    try:os.write(control,b'ENABLE\n')
                                    finally:os.close(control)
                                    self.assertEqual(process.wait(timeout=3),1)
                                else:
                                    parent.close();self.assertEqual(process.wait(timeout=3),1)
                        self.assertIsNotNone(process.returncode)
                    finally:
                        parent.close()
                        if process.poll() is None:process.kill()
                        process.wait(timeout=3);process.stderr.close()

    def test_cursor_success_links_exact_retained_bytes(self):
        import tempfile,contextlib,hashlib,copy
        from unittest.mock import Mock,patch
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);keep=root/'retained.json'
            runner=standing.Runner({k:root/k for k in ('keyblock','latency-probe')},root,{})
            process=Mock();h,e,p,r=self.fixture()
            (root/'cursor.ack.tmp').write_text('interrupted prior acknowledgment')
            def launch(*args,**kwargs):
                self.assertIn('cursor_exit_context',kwargs)
                self.assertFalse((root/'cursor.ack.tmp').exists())
                (root/'typing-launch.json').write_text(_json.dumps(dict(pid=42,window_id=9)))
                return process
            def run(*args):
                launch_id=_json.loads((root/'cursor-exit-context.json').read_text())['launch_id']
                header={**h,'launch_id':launch_id};exits=[{**x,'launch_id':launch_id} for x in e]
                (root/'terminal.stderr').write_text(self.wire(header,[*self.inputs(header),*exits]))
                (root/'latency.json').write_text(_json.dumps(p))
                (root/'keyblock.log').write_bytes(b''.join(standing.KEYBLOCK_RECORD.pack(seq,*v) for seq,v in r.items()))
                (root/'cursor.ack').write_text('ENABLED 100\n')
            @contextlib.contextmanager
            def verified(*args):yield dict(bundle_sha256='f'*64)
            with patch.object(runner,'launch',side_effect=launch),patch.object(runner,'wait_for',return_value=True),\
                 patch.object(runner,'pid',return_value=42),patch.object(runner,'stop',return_value=True),\
                 patch.object(standing,'run_latency_probe',side_effect=run),patch.object(standing,'verified_probe_use',verified),\
                 patch.object(standing.time,'sleep'),patch.object(standing,'wait_for_text',return_value=True),\
                 patch.object(standing.hc,'start_observer') as observer:
                row=runner.latency('kettle-b',dict(keys=3,warmup=1,censor_ms=500,inject='hid',
                    exit_logs=True,**standing.cursor.method('cursor')),7,keep)
                observer.assert_not_called()
            self.assertNotIn('error',row);self.assertTrue(row['cursor_exit_valid'])
            self.assertIn('cursor_artifacts',row)
            self.assertEqual(set(row['cursor_artifacts']),{'probe','keyblock','launch','ack','context','exits'})
            for artifact in row['cursor_artifacts'].values():
                self.assertNotIn('/',artifact['name'])
                self.assertEqual(artifact['sha256'],hashlib.sha256((root/artifact['name']).read_bytes()).hexdigest())
            self.assertFalse(any(k.startswith('typing_') for k in row),'cursor cannot supply typing-memory evidence')
            raw=(root/row['cursor_artifacts']['exits']['name']);raw.write_bytes(raw.read_bytes()+b'tamper')
            self.assertNotEqual(row['cursor_artifacts']['exits']['sha256'],hashlib.sha256(raw.read_bytes()).hexdigest())

    def test_existing_output_files_match_bytes_under_optimization(self):
        import tempfile
        fixture=HERE/'macos-standing/compatibility'
        for schema in [1,2,3]:
            with self.subTest(schema=schema),tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp)/'session';root.mkdir()
                data=(fixture/f'schema{schema}-results.json.fixture').read_bytes()
                (root/'results.json').write_bytes(data)
                # Pin schema-1's file-date reconstruction too: local noon on the
                # fixtures' date, so the date matches in every time zone.
                noon=time.mktime((2027,1,29,12,0,0,0,0,-1))
                os.utime(root/'results.json',(noon,noon))
                result=_json.loads(data);names=result['terminals']
                artifacts={'results.json':standing.dumps(result),
                    'analysis.json':standing.dumps(standing.analyze(result,names,True)),
                    'summary.md':standing.summarize(result,names,True)}
                combined=standing.combine([root]);markdown=combined.pop('markdown')
                artifacts.update({'combined.json':standing.dumps(combined),'combined.md':markdown})
                # Each fixture is the exact output plus one LF, which the
                # tracked-file audit requires of every text file.
                for name,text in artifacts.items():
                    self.assertEqual(text.encode()+b'\n',(fixture/f'schema{schema}-{name}.fixture').read_bytes(),name)

class PublicationFreeze(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import copy, tempfile
        cls.fixture = _json.loads((HERE/'macos-standing/publication-control.fixture').read_text())
        cls.temp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temp.name)
        cls.aa = cls.save('control', cls.fixture)
        cls.ab = []
        for i in range(2):
            r=copy.deepcopy(cls.fixture)
            r['meta'].update(date=f'2026-09-0{i+2}',started=f'2026-09-0{i+2}T12:00:00',label=f'ab-{i}')
            cls.ab.append(cls.save(f'ab-{i}',r))
        cls.combined = standing.combine(cls.ab,cls.aa)
        cls.values = standing.publication.publication_values(cls.combined)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    @classmethod
    def save(cls,name,result):
        folder=cls.root/name;folder.mkdir(exist_ok=True)
        (folder/'results.json').write_text(_json.dumps(result))
        return folder

    def control(self,reference=None,control=None):
        import copy
        a=copy.deepcopy(self.fixture) if control is None else control
        b=copy.deepcopy(self.fixture) if reference is None else reference
        aa=self.save('changed-control',a);ref=self.save('changed-reference',b)
        return standing.publication.coverage(standing._publication_host(),[standing.load_session(ref)],aa)['metrics']

    def test_full_control_units_and_metric_local_absence(self):
        import copy
        coverage=self.control()
        for metric,unit in [('startup.child_ms','ms'),('idle.cpu_percent','percentage points'),
             ('flood-memory.done3_mib','MiB'),('vtebench.dense','ms'),('latency.mean_ms','ms'),
             ('latency.typing_footprint_mib','MiB'),('output-memory.printing_mib','MiB'),
             ('blink-window.wakeups_per_second','/s'),('latency-cursor.mean_ms','ms')]:
            self.assertEqual((coverage[metric]['status'],coverage[metric]['unit']),('passed',unit))
        missing=copy.deepcopy(self.fixture)
        for runs in missing['workloads']['latency'].values():
            for row in runs:row.pop('typing_footprint_mib')
        changed=self.control(control=missing)
        self.assertEqual(changed['latency.typing_footprint_mib']['status'],'missing')
        self.assertEqual(changed['latency.mean_ms']['status'],'passed')
        self.assertEqual(changed['output-memory.printing_mib']['status'],'passed')

    def test_scalar_memory_and_distribution_have_distinct_gates(self):
        c=self.control()
        self.assertIn('gate',c['latency.typing_footprint_mib']['gate'])
        self.assertNotIn('gate_ms',c['latency.typing_footprint_mib']['gate'])
        for metric in ('p95_ms','median_ms','p99_ms','input_ms','output_ms'):
            self.assertIsNone(c['latency.'+metric]['gate'])
            self.assertEqual(c['latency.'+metric]['aa_kind'],'none')
            self.assertNotIn('no_regression',self.combined['rows']['latency.'+metric]['verdict'])
        self.assertEqual(self.combined['rows']['latency.mean_ms']['verdict']['no_regression'],True)

    def test_shared_control_subset_and_changed_contracts(self):
        import copy
        for mode in ('latency','latency-cursor'):
            ref=copy.deepcopy(self.fixture)
            ref['workloads']={mode:ref['workloads'][mode]}
            ref['meta']['rounds']={mode:3}
            ref['meta'].pop('latency-cursor' if mode=='latency' else 'latency')
            c=self.control(reference=ref)
            self.assertEqual(c[mode+'.mean_ms']['status'],'passed')
            for field,value in [('gap_ms',[2100,2400]),('first_gap_ms',2200),('keys',3)]:
                bad=copy.deepcopy(ref);bad['meta'][mode][field]=value
                self.assertEqual(self.control(reference=bad)[mode+'.mean_ms']['status'],'missing')
            for tool in ('latency-probe','keyblock'):
                bad=copy.deepcopy(ref);bad['meta']['tool_hashes'][tool]='0'*64
                self.assertEqual(self.control(reference=bad)[mode+'.mean_ms']['status'],'missing')
        for mutation in ('sampler','clock','receipt'):
            bad=copy.deepcopy(self.fixture)
            if mutation=='sampler':bad['meta']['tool_hashes']['observer']='0'*64
            elif mutation=='clock':bad['meta']['latency']['typing_memory']['clock']='wall'
            else:bad['meta']['tool_artifacts']['latency-probe']['executable_sha256']='0'*64
            self.assertEqual(self.control(reference=bad)['latency.typing_footprint_mib']['status'],'missing')

    def test_control_config_sides_and_intentional_b(self):
        import copy
        bad=copy.deepcopy(self.fixture);bad['meta']['config_closures']['kettle-b']={'sha256':'0'*64,'assets':[]}
        with self.assertRaises(SystemExit):self.control(control=bad)
        bad=copy.deepcopy(self.fixture);bad['meta']['config_closures']['kettle-a']={'sha256':'0'*64,'assets':[]}
        self.assertEqual(self.control(reference=bad)['startup.child_ms']['status'],'missing')
        good=copy.deepcopy(self.fixture);good['meta']['config_closures']['kettle-b']={'sha256':'0'*64,'assets':[]}
        self.assertEqual(self.control(reference=good)['startup.child_ms']['status'],'passed')

    def test_native_exit_capability_and_pooled_p95(self):
        import copy
        h=standing._publication_host()
        s=standing.load_session(self.aa)
        frames=standing.publication.cursor_frames(h,[s])
        self.assertIsNone(frames['kettle-b']['p95_us'])
        self.assertFalse(frames['kettle-b']['calibrated_by_legacy_control'])
        report=standing.publication.coverage(h,[s],self.aa)
        self.assertEqual(report['capabilities']['native_pty'],'unavailable on legacy control')
        self.assertEqual(report['capabilities']['renderer_trace'],'producer agreement required')
        raw=copy.deepcopy(self.fixture)
        for runs in raw['workloads']['latency-cursor'].values():
            for i,row in enumerate(runs):
                wire=CursorLatency()
                header,exits,probe,payload=wire.fixture(measured=2,durations=[999999,100+i,1000+100*i])
                parsed=wire.parse(header,exits,probe,payload,measured=2)
                self.assertEqual([r['key_seq'] for r in parsed['cursor_exit_records']],[7,8,9])
                self.assertEqual(parsed['cursor_exit_count'],3)
                row.update(parsed)
        session=standing.load_session(self.save('exit-frames',raw))
        frames=standing.publication.cursor_frames(h,[session])['kettle-b']
        self.assertAlmostEqual(frames['p95_us'],1175.)
        self.assertEqual(frames['n'],6)
        self.assertTrue(frames['p95_le_4000'])
        raw['workloads']['latency-cursor']['kettle-b'][1]['cursor_exit_count']=2
        frames=standing.publication.cursor_frames(h,[standing.load_session(self.save('exit-partial',raw))])['kettle-b']
        self.assertIsNone(frames['p95_le_4000'])

    def test_stamp_equivalence_explicit_path_and_no_countability(self):
        import copy
        raw=copy.deepcopy(self.fixture);raw['workloads']={'startup':raw['workloads']['startup']}
        raw['meta'].update(kind='observer-control',startup_phases='b',rounds={'startup':30})
        phases=('main','run_with','event_loop_built','config_loaded','app_built','pane_spawned','resumed','window_created','gpu_ready','window_revealed','first_frame')
        def evidence(step):
            # The collector's retained parser output, as it stores it.
            text='\n'.join(f'startup phase={phase} t_ns={1_000_000_000+step*i*1_000_000} since_main_ms=0.0 thread=main'
                            for i,phase in enumerate(phases))
            return standing.startup_phase_evidence(text,900_000_000)
        good=evidence(1)
        for name,runs in raw['workloads']['startup'].items():
            measured=[dict(runs[i%3+1],round_index=i) for i in range(30)]
            if name=='kettle-b':
                for row in measured:row.update(startup_stamps_ns=dict(good['startup_stamps_ns']),startup_stamp_evidence=copy.deepcopy(good))
            raw['workloads']['startup'][name]=[runs[0]]+measured
        path=self.save('stamp',raw)
        # Descending stamps: the parser marks the ordered intervals
        # unavailable, so they cannot authorize phase attribution.
        backwards=copy.deepcopy(raw);bad=evidence(-1)
        self.assertEqual(bad['metric_validity']['event_loop_build_ms']['reason'],'unordered endpoints')
        for row in backwards['workloads']['startup']['kettle-b'][1:]:row.update(startup_stamps_ns=dict(bad['startup_stamps_ns']),startup_stamp_evidence=bad)
        with self.assertRaises(ValueError):standing.publication.observer_control(standing._publication_host(),self.save('stamp-backwards',backwards))
        # Zero stamps with no retained parser evidence.
        zeros=copy.deepcopy(raw)
        for row in zeros['workloads']['startup']['kettle-b'][1:]:
            row['startup_stamps_ns']={phase:0 for phase in phases};row.pop('startup_stamp_evidence')
        with self.assertRaises(ValueError):standing.publication.observer_control(standing._publication_host(),self.save('stamp-zeros',zeros))
        self.assertFalse(standing.load_session(path)['countable'])
        report=standing.publication.observer_control(standing._publication_host(),path)
        self.assertTrue(report['phase_attribution_allowed'])
        self.assertFalse(report['countable'])
        invalid=copy.deepcopy(raw);invalid['workloads']['startup']['kettle-b'][1]['startup_stamps_ns']['first_frame']=None
        with self.assertRaises(ValueError):standing.publication.observer_control(standing._publication_host(),self.save('stamp-invalid-data',invalid))
        with self.assertRaises(SystemExit):self.control(control=raw)
        for row in raw['workloads']['startup']['kettle-b'][1:]:row['child_ms']+=2
        report=standing.publication.observer_control(standing._publication_host(),self.save('stamp-fail',raw))
        self.assertFalse(report['phase_attribution_allowed'])
        raw['meta']['complete']=False
        with self.assertRaises(ValueError):standing.publication.observer_control(standing._publication_host(),self.save('stamp-partial',raw))

    def test_all_zero_rates_are_uncalibrated(self):
        import copy
        raw=copy.deepcopy(self.fixture)
        for runs in raw['workloads']['idle'].values():
            for row in runs:row['wakeups_per_second']=0
        self.assertEqual(self.control(control=raw)['idle.wakeups_per_second']['status'],'uncalibrated')
        raw['workloads']['idle']['kettle-b'][0]['wakeups_per_second']=1
        self.assertEqual(self.control(control=raw)['idle.wakeups_per_second']['status'],'uncalibrated')

    def test_control_requires_every_pair_and_abs_latency_limit(self):
        import copy
        from unittest import mock
        raw=copy.deepcopy(self.fixture)
        raw['meta']['rounds']['latency']=10
        for name,runs in raw['workloads']['latency'].items():
            raw['workloads']['latency'][name]=[copy.deepcopy(runs[i%3]) for i in range(10)]
        raw['workloads']['latency']['kettle-b'][0]={'error':'lost'}
        self.assertEqual(self.control(control=raw)['latency.mean_ms']['status'],'missing')
        raw=copy.deepcopy(self.fixture)
        raw['meta']['rounds']['idle']=10
        for name,runs in raw['workloads']['idle'].items():
            raw['workloads']['idle'][name]=[copy.deepcopy(runs[i%3]) for i in range(10)]
        raw['workloads']['idle']['kettle-b'][0]['frontmost']=False
        self.assertEqual(self.control(control=raw)['idle.footprint_mib']['status'],'missing')
        # A broad interval containing zero still cannot admit an absolute
        # control shift greater than one millisecond.
        stats={'diff':1.5,'diff_low':-1.,'diff_high':4.}
        with mock.patch.object(standing,'latency_aa_gate',return_value=dict(contains_one=True,aa_diff_ms=1.5,gate_ms=3.)):
            original=standing.publication.analyses
            def changed(h,s):
                a=original(h,s)
                a['latency']['metrics']['mean_ms']['ab'].update(stats)
                return a
            with mock.patch.object(standing.publication,'analyses',side_effect=changed):
                self.assertEqual(self.control()['latency.mean_ms']['status'],'failed')

    def test_first_dates_and_partial_keys_do_not_become_full_n(self):
        import copy
        items=[dict(label='later',started='2026-09-01T13:00:00',date='2026-09-01',countable=True),
               dict(label='first',started='2026-09-01T12:00:00',date='2026-09-01',countable=True),
               dict(label='second',started='2026-09-02T12:00:00',date='2026-09-02',countable=True)]
        self.assertEqual([s['label'] for s in standing.publication.selected(items,2)],['first','second'])
        offsets=[dict(label='earlier',started='2026-09-01T11:00:00+02:00',date='2026-09-01',countable=True),
                 dict(label='later',started='2026-09-01T10:00:00+00:00',date='2026-09-01',countable=True)]
        self.assertEqual(standing.publication.selected(offsets,1)[0]['label'],'earlier')
        raw=copy.deepcopy(self.fixture)
        raw['meta']['latency']['keys']=100
        path=self.save('partial-n',raw)
        c=standing.combine([path],self.aa)
        per=c['rows']['latency.mean_ms']['per_session'][0]['metric_countable']['kettle-a']
        self.assertFalse(per['countable'])
        self.assertIsNone(c['rows']['latency.mean_ms']['verdict'].get('no_regression'))
        self.assertEqual(per['keys'],6)
        cells=standing.publication.publication_values(c)['cells']
        self.assertTrue(all(cell['status']!='available' for cell in cells if cell['source_metric']=='latency.mean_ms'))
        self.assertEqual(self.combined['rows']['startup.child_ms']['terminals']['kettle-a']['published'],101)

    def test_fill_unknown_duplicate_unit_unavailable_and_last_digit(self):
        import copy
        p=standing.publication
        cell=next(c for c in self.values['cells'] if c['id']=='startup.child_ms:kettle-a:published')
        self.assertEqual(cell['status'],'available')
        token='{{cell:'+cell['id']+'|ms}}'
        self.assertEqual(p.fill(token,self.values),'101.0')
        for template in ('{{cell:unknown|ms}}',token+' '+token,token.replace('|ms','|MiB'), '{{unknown}}'):
            with self.assertRaises(ValueError):p.fill(template,self.values)
        values=copy.deepcopy(self.values);values['cells'].append(dict(cell))
        with self.assertRaises(ValueError):p.fill(token,values)
        values=copy.deepcopy(self.values)
        next(c for c in values['cells'] if c['id']==cell['id'])['status']='unavailable'
        with self.assertRaises(ValueError):p.fill(token,values)
        values=copy.deepcopy(self.values)
        next(c for c in values['cells'] if c['id']==cell['id'])['display']='101.1'
        with self.assertRaises(ValueError):p.fill(token,values)

    def test_independent_spots_and_wrong_typing_interval_pooled_percentile(self):
        import copy,importlib.util
        spec=importlib.util.spec_from_file_location('independent',HERE/'macos-standing/publication_factcheck.py')
        facts=importlib.util.module_from_spec(spec);spec.loader.exec_module(facts)
        raw=[standing.publication.strict_json(path/'results.json') for path in self.ab]
        self.assertIn('latency.typing_footprint_mib',facts.spot_rows(raw))
        self.assertTrue(facts.verify_estimates(raw,self.combined))
        censored={'meta':{'label':'censored','latency':{'censor_ms':500}},'workloads':{'latency':{'kettle-a':[dict(samples_ms=[1.],censored=1)]}}}
        checked={'rows':{'latency.mean_ms':{'per_session':[dict(label='censored',estimates={'kettle-a':250.5},metric_countable={'kettle-a':{'countable':True}})]}}}
        self.assertTrue(facts.verify_estimates([censored],checked))
        bad=copy.deepcopy(raw);bad[0]['workloads']['latency']['kettle-a'][0]['typing_sample_interval_ms']=50
        with self.assertRaises(ValueError):facts.spot_rows(bad)
        bad=copy.deepcopy(self.combined)
        bad['rows']['latency.p95_ms']['per_session'][0]['estimates']['kettle-a']=12.9
        with self.assertRaises(ValueError):facts.verify_estimates(raw,bad)

    def test_current_intervals_are_the_only_publication_statistics(self):
        import importlib.util
        h=standing._publication_host()
        for folder in self.ab:
            session=standing.load_session(folder)
            analysis=standing.analyze(session['results'],session['names'],True)
            for workload,info in analysis.items():
                for field,entry in info['metrics'].items():
                    metric=entry['descriptor']['id']
                    per=next(p for p in self.combined['rows'][metric]['per_session'] if p['label']==session['label'])
                    self.assertEqual(set(per['statistics']),{'authoritative','current'})
                    self.assertEqual(per['statistics'],entry['statistics'])
        control_analysis=standing.analyze(self.fixture,self.fixture['terminals'],True)
        for metric,coverage in self.combined['aa_coverage']['metrics'].items():
            if coverage['status']=='passed':
                workload,field=metric.split('.',1)
                self.assertEqual(coverage['statistics'],control_analysis[workload]['metrics'][field]['statistics']['current'])
        self.assertEqual(self.values,standing.publication.publication_values(standing.combine(self.ab,self.aa)))
        # Verdicts must use the current paired launch-difference intervals.
        row=self.combined['rows']['latency-cursor.mean_ms']
        self.assertEqual(row['verdict'],standing.latency_ab_verdict(row['sessions'],row['coverage']['gate']['gate_ms']))
        token='{{cell:latency-cursor.mean_ms:kettle-a:published|ms}}'
        self.assertEqual(standing.publication.fill(token,self.values),'12.0')

    def test_cursor_publication_and_control_are_metric_local(self):
        import copy,importlib.util
        p=standing.publication
        row=self.combined['rows']['latency-cursor.mean_ms']
        self.assertEqual(row['coverage']['status'],'passed')
        self.assertEqual(row['coverage']['pairs'],3)
        for per in row['per_session']:
            self.assertEqual(per['metric_countable']['kettle-a']['keys'],6)
        cursor_cells=[c for c in self.values['cells'] if c['source_metric']=='latency-cursor.mean_ms']
        self.assertTrue(cursor_cells)
        self.assertTrue(all(c['status']=='available' for c in cursor_cells))
        self.assertNotIn('latency-cursor.typing_footprint_mib',self.combined['rows'])
        self.assertEqual(p.fill('{{cell:latency-cursor.p95_ms:kettle-a:published|ms}}',self.values),'13.8')
        for knob,value in [('gap_ms',[2000,2401]),('first_gap_ms',2001),
                           ('exit_logs',True),('exit_contract','cursor_exit_v2')]:
            raw=copy.deepcopy(self.fixture);raw['meta']['latency-cursor'][knob]=value
            with self.subTest(knob=knob):
                metrics=self.control(reference=raw)
                self.assertEqual(metrics['latency-cursor.mean_ms']['status'],'missing')
                self.assertEqual(metrics['latency.mean_ms']['status'],'passed')
        raw=copy.deepcopy(self.fixture);raw['meta']['latency-cursor']['keys']=100
        combined=standing.combine([self.save('cursor-partial',raw)],self.aa)
        per=combined['rows']['latency-cursor.mean_ms']['per_session'][0]
        self.assertFalse(per['metric_countable']['kettle-a']['countable'])
        values=p.publication_values(combined)
        with self.assertRaises(ValueError):p.fill('{{cell:latency-cursor.mean_ms:kettle-a:published|ms}}',values)
        spec=importlib.util.spec_from_file_location('cursor_facts',HERE/'macos-standing/publication_factcheck.py')
        facts=importlib.util.module_from_spec(spec);spec.loader.exec_module(facts)
        raw=[p.strict_json(folder/'results.json') for folder in self.ab]
        self.assertTrue(facts.verify_estimates(raw,self.combined))
        changed=copy.deepcopy(self.combined)
        changed['rows']['latency-cursor.p95_ms']['per_session'][0]['estimates']['kettle-a']=12.9
        with self.assertRaises(ValueError):facts.verify_estimates(raw,changed)

    def test_public_privacy_summary_combine_coverage_and_cells(self):
        import copy
        raw=copy.deepcopy(self.fixture);raw['context']='private /Users/sentinel-owner/file owner@sentinel.invalid'
        with self.assertRaises(ValueError):standing.summarize(raw,raw['terminals'],True)
        raw=copy.deepcopy(self.fixture);raw['meta']['label']='owner@sentinel.invalid'
        with self.assertRaises(ValueError):standing.combine([self.save('privacy',raw)],self.aa)
        for value in (self.combined,self.values,{'signing':'Developer ID Application: sentinel'}):
            if 'signing' in value:
                with self.assertRaises(ValueError):standing.publication.public(value)
            else:self.assertNotIn('/Users/',standing.publication.canonical(value))

    def test_missing_control_and_identity_cannot_fill(self):
        import copy
        raw=copy.deepcopy(self.fixture)
        missing=standing.combine([self.save('no-control',raw)])
        self.assertEqual(missing['rows']['startup.child_ms']['verdict']['verdict'],'A/A missing')
        values=standing.publication.publication_values(missing)
        with self.assertRaises(ValueError):standing.publication.fill('{{cell:startup.child_ms:kettle-a:published|ms}}',values)
        raw['meta']['identity']['kettle-a']={}
        self.assertEqual(self.control(reference=raw)['startup.child_ms']['status'],'missing')
        raw=copy.deepcopy(self.fixture);raw['meta']['tool_artifacts']['latency-probe']={'source_sha256':'a'*64}
        self.assertEqual(self.control(reference=raw)['latency.mean_ms']['status'],'missing')
        with self.assertRaises(ValueError):standing.publication.publication_values({'rows':{}})

    def test_caffeinate_interrupt_all_boundaries_and_repeated_cancel_reaps(self):
        # start_caffeinate has two lifetime boundaries: an interrupt after the
        # cleanup callback is registered, and one during registration. Both
        # reap the owned child, and so does a repeated cancellation.
        from unittest import mock
        import contextlib
        child=mock.Mock()
        with mock.patch.object(standing.subprocess,'Popen',return_value=child):
            try:
                with contextlib.ExitStack() as cleanup:
                    owned=standing.start_caffeinate(cleanup)
                    self.assertIs(owned,child)
                    raise KeyboardInterrupt('after registration')
            except KeyboardInterrupt:pass
        child.kill.assert_called_once();child.wait.assert_called_once()
        child=mock.Mock()
        cleanup=mock.Mock();cleanup.callback.side_effect=KeyboardInterrupt()
        with mock.patch.object(standing.subprocess,'Popen',return_value=child):
            with self.assertRaises(KeyboardInterrupt):standing.start_caffeinate(cleanup)
        child.kill.assert_called_once();child.wait.assert_called_once()
        child=mock.Mock();child.wait.side_effect=[KeyboardInterrupt(),None]
        standing.reap_owned_child(child,0)
        self.assertEqual(child.wait.call_count,2)

    def test_production_signals_never_select_processes_by_name(self):
        # This is the explicit source hygiene guard, separate from behavioral
        # cancellation fixtures above and in the collector tests.
        source=(HERE/'macos-standing.py').read_text()
        for forbidden in ('pkill','killall','kill -t','kill --name'):
            self.assertNotIn(forbidden,source)


    def test_three_standing_dates_select_first_session_and_no_later_cherry_pick(self):
        import copy
        folders=[]
        for i in range(5):
            raw=copy.deepcopy(self.fixture)
            raw['terminals']=['kettle','kitty'];raw['meta']['mode']='standing'
            day=min(i+1,4)
            hour=13 if i==1 else 12
            if i==1:day=1
            raw['meta'].update(label=f's-{i}',date=f'2026-09-0{day}',started=f'2026-09-0{day}T{hour}:00:00')
            raw['workloads']={'startup':{'kettle':raw['workloads']['startup']['kettle-a'],'kitty':raw['workloads']['startup']['kettle-b']}}
            raw['meta']['rounds']={'startup':3}
            for field in ('configs','config_closures','identity'):
                raw['meta'][field]={'kettle':raw['meta'][field]['kettle-a'],'kitty':raw['meta'][field]['kettle-b']}
            if i in (1,4):
                for row in raw['workloads']['startup']['kettle']:row['child_ms']=999
            folders.append(self.save(f's-{i}',raw))
        combined=standing.combine(folders,self.aa)
        value=combined['rows']['startup.child_ms']['terminals']['kettle']
        self.assertEqual(value['source_sessions'],['s-0','s-2','s-3'])
        self.assertEqual(value['published'],101)
        self.assertEqual(combined['rows']['startup.child_ms']['claim']['label'],'tied 1st')
        self.assertEqual(standing.publication.publication_values(combined),standing.publication.publication_values(combined))

    def test_publication_cli_refuses_tampered_data_and_checks_full_document(self):
        import contextlib,io,importlib.util,copy
        from unittest import mock
        spec=importlib.util.spec_from_file_location('publication_cli',HERE/'macos-standing-publication.py')
        cli=importlib.util.module_from_spec(spec);spec.loader.exec_module(cli)
        template=self.root/'template.md';template.write_text('Measured {{cell:startup.child_ms:kettle-a:published|ms}} ms.\n')
        values=self.root/'values.json';values.write_text(standing.publication.canonical(self.values))
        doc=self.root/'rendered.md';doc.unlink(missing_ok=True)
        def run(mode,extra=()):
            argv=['publication',mode,'--template',str(template),'--values',str(values),'--document',str(doc),
                  '--sessions',*[str(s) for s in self.ab],'--aa',str(self.aa),*extra]
            with mock.patch.object(sys,'argv',argv),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(io.StringIO()):
                return cli.main()
        self.assertEqual(run('fill'),0)
        self.assertEqual(doc.read_text(),'Measured 101.0 ms.\n')
        self.assertEqual(run('factcheck'),0)
        doc.write_text('Measured 101.1 ms.\n')
        with self.assertRaises(SystemExit) as error:run('factcheck')
        self.assertEqual(error.exception.code,1)
        doc.unlink()
        tampered=copy.deepcopy(self.values);tampered['cells'][0]['value']=999
        values.write_text(standing.publication.canonical(tampered))
        with self.assertRaises(SystemExit):run('fill')
        self.assertFalse(doc.exists())
        # Fresh raw data changing underneath the frozen extractor is refused.
        values.write_text(standing.publication.canonical(self.values))
        raw=standing.publication.strict_json(self.ab[0]/'results.json')
        raw['workloads']['startup']['kettle-a'][1]['child_ms']=300
        original=(self.ab[0]/'results.json').read_bytes()
        try:
            (self.ab[0]/'results.json').write_text(_json.dumps(raw))
            with self.assertRaises(SystemExit):run('fill')
        finally:(self.ab[0]/'results.json').write_bytes(original)

    def test_stamp_runtime_retains_evidence_without_changing_default_outputs(self):
        import tempfile
        from unittest import mock
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp)
            runner=standing.Runner({},work,{})
            (work/'launch.json').write_text(_json.dumps({'started_ns':900000000}))
            (work/'terminal.stderr').write_text((HERE/'macos-standing/startup-phases.fixture').read_text())
            process=mock.Mock();process.wait.return_value=0
            row=runner.finish(process,1)
            self.assertIn('first_frame',row['startup_stamps_ns'])
            self.assertEqual(row['startup_stamp_evidence']['malformed_lines'],0)
            process.wait.assert_called_once()


    def test_historical_nonfinite_fixture_does_not_weaken_frozen_reader(self):
        import copy
        raw=copy.deepcopy(self.fixture)
        raw['workloads']['vtebench']['kettle-a'][0]['means_ms']['dense']=float('nan')
        path=self.save('nonfinite-frozen',raw)
        with self.assertRaises(ValueError):standing.load_session(path)
        raw.pop('evidence_contract',None);raw['meta'].pop('contracts',None)
        path=self.save('nonfinite-historical',raw)
        self.assertTrue(standing.load_session(path))
        with self.assertRaises(ValueError):standing.publication.strict_json(path/'results.json')
        overflow=self.root/'overflow.json';overflow.write_text('{"value":1e999}')
        with self.assertRaises(ValueError):standing.publication.strict_json(overflow)

class ObserverPilot(unittest.TestCase):
    def fixture(self, kind='typing', differences=None, names=None):
        differences = [0.] * 10 if differences is None else differences
        names = ['kettle', 'ghostty'] if names is None else names
        p = standing.publication
        fields = p.PILOT_BOUNDS[kind]
        rows = {}
        for name in names:
            rows[name] = []
            for pair, diff in enumerate(differences):
                for order, arm in enumerate(('on', 'off') if pair % 2 == 0 else ('off', 'on')):
                    row = dict(observer_pair=pair, observer_arm=arm, observer_order=order)
                    for field in fields:
                        value = 10. + (diff if arm == 'on' else 0.)
                        if kind == 'typing':
                            row.update(samples_ms=[value] * 2, censored=0, keys=2)
                            # As collected: only the on arm runs the observer.
                            row.update(typing_memory_valid=arm == 'on',
                                       typing_memory_reason=None if arm == 'on' else 'observer off (pilot arm)')
                        else:
                            row[field] = value
                            row.setdefault('metric_validity', {})[field] = dict(valid=True, reason=None,
                                expected=1, observed=1, capability_version=standing.hc.CONTRACT)
                            row['blink_activity'] = 'verified'
                    row['observer_cost'] = dict(cpu_ns=100, wakeups=3, query_count=80,
                        query_duration_median_ms=.001, query_duration_max_ms=.002,
                        deadline_lateness_max_ms=.01, target_cpu_delta_ns=1000, target_wakeups_delta=10)
                    rows[name].append(row)
        return dict(schema=3, evidence_contract='hc-v1', terminals=names, context='fixture',
            meta=dict(kind='observer-pilot', observer_pilot=dict(kind=kind, pairs=len(differences), bounds=fields),
                      complete=True, countable=False, refusals=['observer pilot (diagnostic)'], date='2026-10-01',
                      mode='standing', rounds={p.PILOT_WORKLOADS[kind]: len(differences)*2},
                      latency=dict(keys=2, censor_ms=500)), workloads={p.PILOT_WORKLOADS[kind]: rows})

    def report(self, raw):
        return standing.publication.observer_pilot_report(standing._publication_host(), raw)

    def test_typing_on_arm_counts_only_with_its_observer_through_the_epoch(self):
        # Timing survives an observer that stopped after its readiness query;
        # that arm no longer measures "observer on", so its pair cannot count.
        for reason in ('insufficient typing memory duration', 'native query failed or target exited', None):
            raw = self.fixture()
            on = next(r for r in raw['workloads']['latency']['kettle'] if r['observer_pair'] == 3 and r['observer_arm'] == 'on')
            on.update(typing_memory_valid=False, typing_memory_reason=reason)
            report = self.report(raw)
            metric = report['terminals']['kettle']['metrics']['mean_ms']
            self.assertEqual(metric['invalid_pairs_by_reason'], {'on-arm observer evidence invalid': 1})
            self.assertEqual((metric['valid_pairs'], metric['equivalent']), (9, False))
            self.assertFalse(report['equivalent'])
            self.assertTrue(report['terminals']['ghostty']['equivalent'])
        # The off arm never runs an observer; its unavailable memory is expected.
        self.assertTrue(self.report(self.fixture())['equivalent'])

    def test_cli_selects_pilot_and_forces_block_entries(self):
        import contextlib, io
        from unittest.mock import patch
        class Prepared(Exception): pass
        for kind, workload in [('typing', 'latency'), ('printing', 'output-memory'), ('blink', 'blink-window')]:
            args = ['standing', '--observer-pilot', kind, '--no-build', '--peers', '', '--kettle', 'fixture', '--allow-bare']
            with patch.object(sys, 'argv', args), patch.object(sys, 'platform', 'darwin'), \
                 patch.object(standing, 'require_bundles'), \
                 patch.object(standing, 'resolve_rounds', wraps=standing.resolve_rounds) as resolve, \
                 patch.object(standing, 'latency_entries', wraps=standing.latency_entries) as entries, \
                 patch.object(standing, 'build_probes', side_effect=Prepared), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(Prepared): standing.main()
            options = resolve.call_args.args[0]
            self.assertEqual(options.workloads, workload)
            self.assertEqual(options.observer_pairs, 10)
            self.assertEqual(options.rounds, 20)
            self.assertEqual(options.latency_floors, '')
            self.assertFalse(options.latency_kettle_opaque)
            self.assertEqual(entries.call_args_list[0].args[2:4], (False, []))

    def test_mocked_campaign_metadata_artifacts_and_public_privacy(self):
        import contextlib, tempfile, io
        from unittest.mock import patch
        for kind in ('typing','printing','blink'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp).resolve();out=root/'pilot'
                workload=standing.publication.PILOT_WORKLOADS[kind]
                raw=self.fixture(kind, differences=[0.,0.], names=['kettle'])
                calls=[]
                def result(name,options,seed,keep):
                    pair=len(calls)//2;order=len(calls)%2
                    arm=options['observer_arm']
                    calls.append((name,arm,seed,dict(options)))
                    row=dict(raw['workloads'][workload]['kettle'][pair*2+order])
                    keep.write_text('private raw artifact')
                    (out/'private-config/grid').write_text('120 36\n')
                    return row
                def latency(runner,name,options,seed,keep):return result(name,options,seed,keep)
                def optional(runner,name,mode,options,keep,setup):
                    self.assertEqual(mode,workload)
                    return result(name,options,None,keep.with_suffix('.jsonl'))
                state=dict(display={'fixture':1},power={},low_power=False,load=[0.,0.],procs=[])
                args=['standing','--observer-pilot',kind,'--observer-pairs','2','--peers','',
                      '--kettle','/fixture/A.app/Contents/MacOS/kettle','--no-build','--fd-limit','0',
                      '--latency-keys','2','--out-dir',str(out)]
                with contextlib.ExitStack() as stack:
                    stack.enter_context(patch.object(sys,'argv',args))
                    stack.enter_context(patch.object(sys,'platform','darwin'))
                    stack.enter_context(patch.dict(os.environ,{'HOME':str(root)}))
                    for name,value in [('require_bundles',None),('build_probes',{}),('probe_tool_identity',({},{})),
                                       ('collect_preflight',state),('preflight_refusals',[]),('command','fixture'),
                                       ('harness_revision',{}),('terminal_identity',({'sha256':'same'},dict(path='/Users/private-owner/private@email.test'))),
                                       ('file_sha256','a'*64),('latency_grants',{'screen_recording':True,'accessibility':True}),
                                       ('validate_latency_probe',{}),('start_caffeinate',None)]:
                        stack.enter_context(patch.object(standing,name,return_value=value))
                    stack.enter_context(patch.object(standing,'probe_lock',return_value=contextlib.nullcontext()))
                    stack.enter_context(patch.object(standing.hc,'build_helpers',return_value={'latency-probe':Path('/fixture-probe')}))
                    stack.enter_context(patch.object(standing.Runner,'latency',latency))
                    stack.enter_context(patch.object(standing.hc,'collect',optional))
                    stack.enter_context(patch.object(standing.Runner,'stop_current'))
                    stack.enter_context(patch.object(standing.subprocess,'Popen',side_effect=AssertionError('no launches')))
                    stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                    self.assertEqual(standing.main(),0)
                measured=_json.loads((out/'results.json').read_text())
                self.assertEqual(measured['meta']['kind'],'observer-pilot')
                self.assertEqual(measured['meta']['observer_pilot'],raw['meta']['observer_pilot'])
                self.assertFalse(measured['meta']['countable'])
                self.assertTrue(measured['meta']['complete'])
                self.assertEqual(measured['meta']['rounds'],{workload:4})
                self.assertEqual(measured['terminals'],['kettle'])
                self.assertEqual([c[1] for c in calls],['on','off','off','on'])
                if kind=='typing':self.assertEqual([c[2] for c in calls],[standing.SEED*1000]*2+[standing.SEED*1000+1]*2)
                for filename in ('results.json','summary.md','observer-equivalence.json'):
                    text=(out/filename).read_text()
                    self.assertNotIn('private-owner',text)
                    self.assertNotIn('private@email.test',text)
                    self.assertNotIn(str(root),text)
                self.assertIn('private-owner',(out/'local-manifest.json').read_text())
                self.assertTrue(_json.loads((out/'observer-equivalence.json').read_text())['equivalent'])

    def test_self_cost_environment_is_pilot_only(self):
        import tempfile
        from unittest.mock import Mock, patch
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp)
            runner=standing.Runner({'launch':work/'launch','stamp':work/'stamp'},work,{'kettle':'fixture'})
            with patch.dict(os.environ,{'KETTLE_HC_OBSERVER_SELF_COST':'private-sentinel'}), \
                 patch.object(standing.subprocess,'Popen',return_value=Mock()) as spawn:
                runner.launch('kettle','true',1)
                self.assertNotIn('KETTLE_HC_OBSERVER_SELF_COST',spawn.call_args.kwargs['env'])
                runner.observer_pilot=True
                runner.launch('kettle','true',1)
                self.assertEqual(spawn.call_args.kwargs['env']['KETTLE_HC_OBSERVER_SELF_COST'],'1')

    def test_cli_conflicts_refuse_before_side_effects(self):
        import contextlib, io
        from unittest.mock import patch
        for extra in (['--observer-pairs', '1'], ['--observer-pairs', '0'], ['--kettle-b', 'B'],
                      ['--kettle-b-config', 'opacity=1'], ['--workloads', 'latency-cursor'],
                      ['--workloads', 'idle'], ['--latency-payload', 'cursor'], ['--rounds', '3'],
                      ['--latency-rounds', '10'], ['--blink-validate-only'], ['--observer-control', 'x'],
                      ['--combine', 'x'], ['--aa', 'x'], ['--startup-phases'], ['--latency-check']):
            with self.subTest(extra=extra), patch.object(sys, 'argv', ['standing', '--observer-pilot', 'typing', *extra]), \
                 patch.object(standing, 'build_probes') as build, contextlib.redirect_stderr(io.StringIO()) as err:
                with self.assertRaises(SystemExit) as raised: standing.main()
                self.assertEqual(raised.exception.code, 2)
                self.assertIn('observer', err.getvalue())
                self.assertNotIn('unrecognized arguments', err.getvalue())
                build.assert_not_called()

    def test_balanced_rotated_plan_and_identical_seeds(self):
        plan = list(standing.observer_plan(['kettle', 'ghostty', 'kitty'], 10))
        self.assertEqual(len(plan), 60)
        for i in range(10):
            batch = plan[i*6:(i+1)*6]
            self.assertEqual([r[0] for r in batch[::2]], standing.rotated(['kettle', 'ghostty', 'kitty'], i))
            for first, second in zip(batch[::2], batch[1::2]):
                self.assertEqual([first[1], second[1]], ['on', 'off'] if i % 2 == 0 else ['off', 'on'])
                self.assertEqual(first[2:], (i, 0, standing.SEED*1000+i))
                self.assertEqual(second[2:], (i, 1, standing.SEED*1000+i))

    def test_cancelled_attempt_is_retained_before_stopping(self):
        import tempfile, contextlib, io
        from unittest.mock import Mock, patch
        for exception in (KeyboardInterrupt(),SystemExit('/Users/private-owner/sentinel')):
            raw=self.fixture(names=['kettle'],differences=[0.,0.]);raw['workloads']={}
            recorder=Mock();closure=Mock();collect=Mock(side_effect=exception)
            with tempfile.TemporaryDirectory() as tmp, \
                 patch.object(standing,'round_grid',return_value=((120,36),(120,36))), \
                 contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(type(exception)):
                    standing.run_observer_pilot(raw,['kettle'],2,collect,closure,recorder,Path(tmp),Path(tmp))
            rows=raw['workloads']['latency']['kettle']
            self.assertEqual(len(rows),1)
            self.assertEqual(rows[0]['error'],'observer pilot collection cancelled')
            self.assertEqual(rows[0]['observer_arm'],'on')
            recorder.write.assert_called_once();collect.assert_called_once()
            self.assertNotIn('private-owner',_json.dumps(raw))

    def test_typing_on_observer_failure_cannot_pass_equivalence(self):
        import tempfile, contextlib, io
        from unittest.mock import Mock, patch
        for reason in ('typing observer readiness missing','typing timeline unavailable or invalid',
                       'typing launch context unavailable or invalid'):
            raw=self.fixture(names=['kettle'],differences=[0.,0.]);raw['workloads']={}
            def collect(name,arm,pair,seed):
                return dict(samples_ms=[1.,1.],censored=0,keys=2,
                    typing_memory_reason=reason if arm=='on' else 'observer off (pilot arm)')
            with tempfile.TemporaryDirectory() as tmp, \
                 patch.object(standing,'round_grid',return_value=((120,36),(120,36))), \
                 contextlib.redirect_stdout(io.StringIO()):
                standing.run_observer_pilot(raw,['kettle'],2,collect,Mock(),Mock(),Path(tmp),Path(tmp))
            report=self.report(raw)
            metric=report['terminals']['kettle']['metrics']['mean_ms']
            self.assertFalse(report['equivalent'])
            self.assertEqual(metric['valid_pairs'],0)
            self.assertEqual(metric['invalid_pairs_by_reason'],{'failed arm':2})
            self.assertEqual(metric['reason'],'insufficient valid pairs')
            self.assertEqual(sum('error' in r for r in raw['workloads']['latency']['kettle']),2)

    def test_runner_keeps_failures_without_retries_or_cutoff(self):
        import tempfile
        from unittest.mock import Mock, patch
        raw = self.fixture(names=['kettle', 'kitty'], differences=[0., 0.])
        raw['workloads'] = {}
        recorder, closure = Mock(), Mock()
        calls = []
        def collect(name, arm, pair, seed):
            calls.append((name, arm, pair, seed))
            (work/'typing-memory.jsonl.self.json').write_text(_json.dumps(dict(cpu_ns=10,wakeups=1,
                query_count=1,path='/Users/private-owner/sentinel')))
            if len(calls) == 1: raise RuntimeError('/Users/private-owner/sentinel')
            return {'error': 'calibration failed'}
        with tempfile.TemporaryDirectory() as tmp, patch.object(standing, 'round_grid', return_value=((120, 36), (120, 36))):
            work = Path(tmp)
            standing.run_observer_pilot(raw, raw['terminals'], 2, collect, closure, recorder, work, work)
            saved=list(work.glob('latency-*-p*-on.self.json'))
            self.assertEqual(len(saved),4)
            self.assertFalse(list(work.glob('latency-*-p*-off.self.json')))
            for path in saved:
                self.assertEqual(set(_json.loads(path.read_text())),{'cpu_ns','wakeups','query_count'})
                self.assertNotIn('private-owner',path.read_text())
        self.assertEqual(len(calls), 8)
        self.assertEqual(closure.check.call_count, 16)
        self.assertEqual(recorder.write.call_count, 8)
        self.assertEqual(sum(map(len, raw['workloads']['latency'].values())), 8)
        self.assertNotIn('private-owner', _json.dumps(raw))
        for row in raw['workloads']['latency']['kettle']:
            self.assertIn('observer_cost', row)
            self.assertIn('observer_order', row)

    def test_typing_off_failed_launch_keeps_memory_unavailable_reason(self):
        import tempfile
        from unittest.mock import Mock, patch
        with tempfile.TemporaryDirectory() as tmp:
            work=Path(tmp)
            runner=standing.Runner({'keyblock':work/'keyblock'},work,{})
            runner.launch=Mock(return_value=Mock());runner.wait_for=Mock(return_value=False)
            runner.pid=Mock(return_value=None);runner.stop=Mock(return_value=True)
            with patch.object(standing.hc,'start_observer') as observer:
                row=runner.latency('kettle',dict(keys=2,warmup=20,censor_ms=500,inject='hid',observer_arm='off'),1)
            observer.assert_not_called()
            self.assertIn('error',row)
            self.assertEqual(row['typing_memory_reason'],'observer off (pilot arm)')
            self.assertIsNone(row['typing_footprint_mib'])

    def test_analysis_counts_focus_reasons_and_redacts_private_evidence(self):
        raw=self.fixture('printing',names=['kettle'])
        rows=raw['workloads']['output-memory']['kettle']
        rows[0]['metric_validity']['printing_mib'].update(valid=False,reason='known focus change during interval')
        rows[2]['metric_validity']['printing_mib'].update(valid=False,reason='/Users/private-owner/private@email.test')
        report=self.report(raw)
        reasons=report['terminals']['kettle']['metrics']['printing_mib']['invalid_pairs_by_reason']
        self.assertEqual(reasons,{'known focus change during interval':1,'invalid metric evidence':1})
        self.assertNotIn('private-owner',_json.dumps(report))
        self.assertFalse(report['equivalent'])

    def test_typing_on_starts_observer_off_does_not(self):
        import tempfile, contextlib
        from unittest.mock import Mock, patch
        for arm in ('on', 'off', None):
            with self.subTest(arm=arm), tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                runner = standing.Runner({k: work/k for k in ('keyblock', 'observer', 'latency-probe')}, work, {})
                probe, samples, artifact = TypingMemory().fixture()
                probe['samples'] = [dict(seq=i, warmup=True, t_post=800_000_000, display=810_000_000)
                                    for i in range(7,27)] + probe['samples']
                def launch(*args, **kwargs):
                    (work/'typing-launch.json').write_text(_json.dumps(dict(pid=42, window_id=7)))
                    return Mock()
                def start(*args):
                    (work/'typing-memory.jsonl').write_text(''.join(_json.dumps(s)+'\n' for s in samples))
                    return Mock()
                def run_probe(*args): (work/'latency.json').write_text(_json.dumps(probe))
                @contextlib.contextmanager
                def verified(*args): yield artifact
                runner.launch = Mock(side_effect=launch)
                runner.wait_for = Mock(return_value=True)
                runner.pid = Mock(return_value=42)
                runner.stop = Mock(return_value=True)
                runner.end_sampler = Mock()
                options = dict(keys=2, warmup=20, censor_ms=500, inject='hid')
                if arm: options['observer_arm'] = arm
                with patch.object(standing.hc, 'start_observer', side_effect=start) as observer, \
                     patch.object(standing, 'run_latency_probe', side_effect=run_probe) as timing, \
                     patch.object(standing, 'verified_probe_use', verified), \
                     patch.object(standing, 'wait_for_text', return_value=True), \
                     patch.object(standing.time, 'sleep'), \
                     patch.object(standing, 'read_keyblock_log', return_value={i:(1,1_000_000_000,1_000_000_000) for i in range(1,29)}):
                    row = runner.latency('kettle', options, 123, work/'keep.json')
                self.assertEqual(observer.call_count, 0 if arm == 'off' else 1)
                self.assertEqual(standing.latency_keys(row, 500), [10., 500.])
                self.assertNotIn('observer_cost', row) # Attached by the pilot runner only.
                self.assertEqual(timing.call_args.args[1][timing.call_args.args[1].index('--seed')+1], '123')
                if arm == 'off':
                    self.assertEqual(row['typing_memory_reason'], 'observer off (pilot arm)')
                    self.assertIsNone(row['typing_footprint_mib'])
                    self.assertFalse((work/'keep.memory.jsonl').exists())
                else:
                    self.assertTrue(row['typing_memory_valid'])

    def test_sparse_printing_metric_designated_query_and_focus(self):
        import copy
        fixture = OutputBlink()
        samples = fixture.samples()
        sparse = [samples[0], *samples[59:66], samples[81]]
        records = fixture.printing()
        row = standing.hc.printing_row(records, sparse, 42, 7, observer_off=True)
        self.assertTrue(row['printing_valid'], row)
        self.assertEqual(row['printing_mib'], 160.)
        self.assertEqual(row['coverage_waived'], 'observer-pilot off arm')
        self.assertFalse(standing.hc.printing_row(records, sparse, 42, 7)['printing_valid'])
        for defect in ('missing', 'late', 'change', 'boundary', 'readiness-focus', 'no-record-after-done', 'late-change'):
            bad = copy.deepcopy(sparse)
            if defect == 'missing': bad = bad[:2]
            if defect == 'no-record-after-done': bad = bad[:-1]
            # Lost at 7 s and restored before the record after done reports it.
            if defect == 'late-change': bad[-1]['focus_changes'] = [dict(t_ns=17_000_000_000, valid=False)]
            if defect == 'late': bad = [bad[0], bad[1], *bad[5:]]
            if defect == 'change': bad[-1]['focus_changes'] = [dict(t_ns=12_000_000_000, valid=False)]
            if defect == 'boundary': bad[2]['focus_after']['valid'] = False
            if defect == 'readiness-focus': bad[0]['focus_before']['known'] = False
            got = standing.hc.printing_row(records, bad, 42, 7, observer_off=True)
            self.assertFalse(got['printing_valid'], defect)
            if defect in ('missing', 'late'): self.assertIn('designated', got['printing_reason'])
            if defect == 'no-record-after-done': self.assertEqual(got['printing_reason'], 'off-arm query after done missing')
            if defect == 'late-change': self.assertEqual(got['printing_reason'], 'known focus change during interval')

    def test_printing_off_arm_stops_its_observer_after_the_final_query(self):
        import tempfile
        from unittest.mock import patch
        fixture = OutputBlink()
        for arm, waits in (('off', True), ('on', False)):
            with self.subTest(arm=arm), tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                stopped = []
                clock = iter([11_000_000_000, 15_000_000_000, 19_000_000_000, 21_000_000_000])
                reads = []
                def now():
                    reads.append(1)
                    return next(clock)
                class Runner:
                    def __init__(self): self.work=work; self.probes={'observer':'observer', 'printing':'printing'}
                    def launch(self,*args):
                        (work/'hc-launch.json').write_text(_json.dumps(dict(started_ns=10_000_000_000,pid=42,window_id=7)))
                        return object()
                    def wait_for(self,*args): return True
                    def end_sampler(self,*args): stopped.append(len(reads))
                    def stop(self,*args): return True
                def observer(*args):
                    (work/'hc-timeline.jsonl').write_text('ready')
                    (work/'hc-printing.jsonl').write_text('done_ns')
                    return object()
                with patch.object(standing.hc, 'start_observer', side_effect=observer), \
                     patch.object(standing.hc, 'now_ns', side_effect=now), \
                     patch.object(standing.hc, 'read_jsonl', return_value=fixture.samples()):
                    standing.hc.collect(Runner(), 'fixture', 'output-memory',
                        dict(activate=False, settle=2.5, window=6., observer_arm=arm), work/'saved', fixture.setup())
                # origin = 12 s; the last off-arm query is due at 20.6 s, so the
                # observer may stop only once the clock reads past 20.85 s.
                self.assertEqual(stopped, [4] if waits else [1])

    def test_sparse_blink_boundaries_counters_and_focus(self):
        import copy
        samples = OutputBlink().samples(origin=12_500_000_000, count=61)
        sparse = [samples[0], samples[-1]]
        row = standing.hc.blink_row(sparse, 10_000_000_000, 11_000_000_000, 42, 7,
                                   'verified', observer_off=True)
        self.assertTrue(row['blink_valid'], row)
        self.assertEqual(row['footprint_mib'], 160.)
        self.assertAlmostEqual(row['cpu_percent'], .1)
        self.assertAlmostEqual(row['wakeups_per_second'], 10.)
        self.assertEqual(row['coverage_waived'], 'observer-pilot off arm')
        self.assertFalse(standing.hc.blink_row(sparse, 10_000_000_000, 11_000_000_000, 42, 7, 'verified')['blink_valid'])
        for defect in ('missing', 'late', 'change', 'first-focus', 'final-focus'):
            bad = copy.deepcopy(sparse)
            if defect == 'missing': bad.pop()
            if defect == 'late': bad = [bad[0], OutputBlink().samples(origin=18_800_000_000, count=1)[0]]
            if defect == 'change': bad[-1]['focus_changes'] = [dict(t_ns=15_000_000_000, valid=False)]
            if defect == 'first-focus': bad[0]['focus_before']['valid'] = False
            if defect == 'final-focus': bad[-1]['focus_after']['known'] = False
            self.assertFalse(standing.hc.blink_row(bad, 10_000_000_000, 11_000_000_000, 42, 7,
                                                 'verified', observer_off=True)['blink_valid'], defect)

    def test_sparse_offsets_reach_owned_observer(self):
        import tempfile
        from unittest.mock import patch
        fixture = OutputBlink()
        for workload, expected in [('output-memory', '0,5900,6000,6100,6200,6300,6400,6500,8600'), ('blink-window', '0,6000')]:
            with self.subTest(workload=workload), tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                class Runner:
                    def __init__(self): self.work=work; self.probes={'observer':'observer', 'printing':'printing'}
                    def launch(self,*args):
                        (work/'hc-launch.json').write_text(_json.dumps(dict(started_ns=10_000_000_000,pid=42,window_id=7)))
                        return object()
                    def wait_for(self,*args): return True
                    def end_sampler(self,*args): pass
                    def stop(self,*args): return True
                def observer(*args):
                    (work/'hc-timeline.jsonl').write_text('ready')
                    (work/'hc-printing.jsonl').write_text('done_ns')
                    return object()
                with patch.object(standing.hc, 'start_observer', side_effect=observer) as start, \
                     patch.object(standing.hc, 'now_ns', side_effect=[11_000_000_000,19_000_000_000,21_000_000_000]), \
                     patch.object(standing.hc, 'read_jsonl', return_value=fixture.samples()):
                    standing.hc.collect(Runner(), 'fixture', workload, dict(activate=False, settle=2.5, window=6., observer_arm='off'), work/'saved', fixture.setup())
                args = start.call_args.args[3]
                self.assertEqual(args[-1], expected)
                self.assertEqual(int(args[-2]), len(expected.split(',')))
                source = (HERE/'macos-standing/launch.swift').read_text()
                self.assertIn('args.count == 8', source)

    def test_native_offset_parser(self):
        # Reuse the existing macOS compiler/skip policy and pure native entry.
        fixture = dict(pid=42, target=7, front=42, windows=[])
        OutputBlink().native_fixture('observer', _json.dumps(fixture), offsets=True)

    def test_self_cost_numeric_allowlist_bounds_and_off_zero(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            timeline = Path(tmp)/'timeline'
            samples = OutputBlink().samples(count=3)
            timeline.write_text(''.join(_json.dumps(s)+'\n' for s in samples))
            sidecar = Path(str(timeline)+'.self.json')
            sidecar.write_text(_json.dumps(dict(cpu_ns=123, wakeups=4, query_count=3,
                                                path='/Users/private-owner/sentinel', error='private@email.test')))
            cost = standing.hc.observer_cost(timeline)
            self.assertEqual(cost['cpu_ns'],123)
            self.assertEqual(cost['query_count'],3)
            self.assertEqual(cost['target_wakeups_delta'],2)
            self.assertEqual(cost['target_cpu_delta_ns'],200_000)
            self.assertEqual(cost['query_duration_median_ms'],.001)
            self.assertEqual(cost['query_duration_max_ms'],.001)
            self.assertEqual(cost['deadline_lateness_max_ms'],.001)
            self.assertNotIn('private', _json.dumps(cost))
            sidecar.write_text('x'*4097)
            self.assertIsNone(standing.hc.observer_cost(timeline)['cpu_ns'])
            for data in ('['*1500+'0'+']'*1500,
                         _json.dumps(dict(cpu_ns=2**64,wakeups=0,query_count=1)),
                         _json.dumps(dict(cpu_ns=1,wakeups=0,query_count=12001))):
                sidecar.write_text(data)
                self.assertIsNone(standing.hc.observer_cost(timeline)['cpu_ns'])
            sidecar.unlink();sidecar.symlink_to(timeline)
            self.assertIsNone(standing.hc.observer_cost(timeline)['cpu_ns'])
            sidecar.unlink();os.mkfifo(sidecar)
            self.assertIsNone(standing.hc.observer_cost(timeline)['cpu_ns'])
            sidecar.unlink()
            self.assertEqual(standing.hc.observer_cost(timeline, True)['query_count'],0)
            self.assertEqual(standing.hc.observer_cost(timeline, True)['cpu_ns'],0)

    def test_analysis_student_t_inside_straddling_outside_and_short(self):
        for kind in ('typing', 'printing', 'blink'):
            for case, diffs, equivalent in [('inside',[0.]*10,True), ('straddling',[-.1,.1]*5,kind != 'blink'),
                                             ('outside',[2.]*10,False), ('short',[0.]*10,False)]:
                raw=self.fixture(kind,diffs)
                if case=='short': raw['workloads'][standing.publication.PILOT_WORKLOADS[kind]]['kettle'].pop()
                report=self.report(raw)
                self.assertEqual(report['equivalent'],equivalent,(kind,case,report))
                for field, metric in report['terminals']['kettle']['metrics'].items():
                    if case=='short':
                        self.assertEqual(metric['reason'],'insufficient valid pairs')
                        self.assertEqual(metric['valid_pairs'],9)
                        self.assertEqual(metric['invalid_pairs_by_reason'],{'missing arm':1})
                    if case=='inside':
                        self.assertEqual(metric['difference'],dict(diff=0.,low=0.,high=0.,n=10))
                    if case=='straddling':
                        mean,low,high=standing.t_interval(diffs)
                        self.assertAlmostEqual(metric['difference']['diff'],mean)
                        self.assertAlmostEqual(metric['difference']['low'],low)
                        self.assertAlmostEqual(metric['difference']['high'],high)

    def test_analysis_straddles_every_bound_and_failed_arm(self):
        import copy
        for kind, fields in standing.publication.PILOT_BOUNDS.items():
            for field, bounds in fields.items():
                for direction in (-1,1):
                    raw=self.fixture(kind, names=['kettle'])
                    rows=raw['workloads'][standing.publication.PILOT_WORKLOADS[kind]]['kettle']
                    diffs=[direction*bounds[1]+d for d in [-.02,.02]*5]
                    for row in rows:
                        if row['observer_arm']=='on':
                            value=10+diffs[row['observer_pair']]
                            if kind=='typing':row['samples_ms']=[value]*2
                            else:row[field]=value
                    metric=self.report(raw)['terminals']['kettle']['metrics'][field]
                    self.assertFalse(metric['equivalent'])
                    self.assertEqual(metric['reason'],'interval outside equivalence bounds')
            raw=self.fixture(kind)
            raw['workloads'][standing.publication.PILOT_WORKLOADS[kind]]['kettle'][0]['error']='/Users/private-owner/sentinel'
            report=self.report(raw)
            self.assertFalse(report['equivalent'])
            self.assertTrue(report['terminals']['ghostty']['equivalent'])
            for metric in report['terminals']['kettle']['metrics'].values():
                self.assertEqual(metric['invalid_pairs_by_reason'],{'failed arm':1})
                self.assertEqual(metric['reason'],'insufficient valid pairs')
            self.assertNotIn('private-owner',_json.dumps(report))

    def test_pilot_dispatch_countability_combine_aa_and_analysis_refusal(self):
        import tempfile
        from unittest.mock import patch
        raw=self.fixture()
        self.assertFalse(standing.session_countable(raw['meta']))
        with self.assertRaisesRegex(ValueError,'observer pilot'):standing.analyze(raw,raw['terminals'],False)
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)/'pilot';folder.mkdir()
            standing.Recorder(folder/'results.json',raw).write()
            (folder/'local-manifest.json').write_text('/Users/private-owner/private@email.test')
            report=standing.publication.observer_control(standing._publication_host(),folder)
            self.assertTrue(report['equivalent'])
            self.assertEqual(report['interval_policy'],'paired Student-t 95%')
            self.assertNotIn('private',_json.dumps(report))
            with self.assertRaisesRegex(SystemExit,'observer pilot'):standing.combine([folder])
            ordinary=Path(tmp)/'ordinary';ordinary.mkdir()
            other=self.fixture();other['meta']['kind']='ordinary'
            standing.Recorder(ordinary/'results.json',other).write()
            with self.assertRaisesRegex(SystemExit,'observer pilot'):standing.combine([ordinary],folder)
            out=Path(tmp)/'report'
            with patch.object(sys,'argv',['standing','--observer-control',str(folder),'--out-dir',str(out)]), \
                 patch.object(standing,'build_probes') as build:
                self.assertEqual(standing.main(),0)
            build.assert_not_called()
            self.assertNotIn('private',(out/'observer-equivalence.json').read_text())

    def test_invalid_evidence_duplicates_and_no_pair_replacement(self):
        for kind in ('typing','printing','blink'):
            raw=self.fixture(kind)
            rows=raw['workloads'][standing.publication.PILOT_WORKLOADS[kind]]['kettle']
            rows.append(dict(rows[0]))
            for metric in self.report(raw)['terminals']['kettle']['metrics'].values():
                self.assertEqual(metric['invalid_pairs_by_reason'],{'duplicate arm':1})
                self.assertEqual(metric['valid_pairs'],9)
            raw=self.fixture(kind)
            row=raw['workloads'][standing.publication.PILOT_WORKLOADS[kind]]['kettle'][0]
            if kind=='typing':row['samples_ms']=[1.]
            else:
                for evidence in row['metric_validity'].values():evidence['observed']=0
            self.assertFalse(self.report(raw)['equivalent'])


class GhosttyUserConfig(unittest.TestCase):
    def setUp(self):
        import tempfile
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve()
        self.user=self.root/'Library/Application Support/com.mitchellh.ghostty'
        self.user.mkdir(parents=True)
        self.sequence=0

    def capture(self, measured=('kettle','ghostty')):
        self.sequence+=1
        work=self.root/f'work-{self.sequence}';work.mkdir()
        standing.write_configs(work,{'kettle':''})
        return standing.ConfigClosure(work,{'kettle':''},self.root,{'HOME':str(self.root)},measured=measured)

    def test_absent_empty_private_state_and_public_privacy(self):
        closure=self.capture()
        self.assertEqual(len(closure.local['ghostty_user_config']),2)
        self.assertTrue(all(not s['present'] for s in closure.local['ghostty_user_config'].values()))
        for name in ('config','config.ghostty'):(self.user/name).touch()
        closure=self.capture();closure.check()
        for state in closure.local['ghostty_user_config'].values():
            self.assertEqual(state,dict(present=True,size=0,sha256=standing.hashlib.sha256(b'').hexdigest()))
        self.assertNotIn(str(self.root),_json.dumps(closure.public))
        self.assertNotIn('Application Support',_json.dumps(closure.public))

    def test_nonempty_and_symlink_refuse_fixed_public_reason(self):
        import contextlib,io
        for name in ('config','config.ghostty'):
            for defect in ('nonempty','symlink','directory','fifo'):
                path=self.user/name
                if defect=='nonempty':path.write_text('/Users/private-owner/private@email.test')
                if defect=='symlink':path.symlink_to(self.root/'missing')
                if defect=='directory':path.mkdir()
                if defect=='fifo':os.mkfifo(path)
                with self.subTest(name=name,defect=defect),self.assertRaises(standing.ConfigClosureError) as error:
                    self.capture()
                self.assertEqual(str(error.exception),'config closure: Ghostty user config would apply')
                self.assertNotIn(str(self.root),str(error.exception))
                if path.is_dir():path.rmdir()
                else:path.unlink()

    def test_mid_campaign_state_change_refuses(self):
        for state in ('absent-to-empty','empty-to-absent','empty-to-nonempty','empty-to-symlink'):
            path=self.user/'config'
            path.unlink(missing_ok=True)
            if state!='absent-to-empty':path.touch()
            closure=self.capture()
            if state=='absent-to-empty':path.touch()
            if state=='empty-to-absent':path.unlink()
            if state=='empty-to-nonempty':path.write_text('private@email.test')
            if state=='empty-to-symlink':path.unlink();path.symlink_to(self.root/'missing')
            with self.subTest(state=state),self.assertRaisesRegex(standing.ConfigClosureError,'Ghostty user config would apply'):
                closure.check()

    def test_unmeasured_ghostty_never_checked(self):
        from unittest.mock import patch
        (self.user/'config').write_text('sentinel')
        with patch.object(standing.ConfigClosure,'_ghostty_state',side_effect=AssertionError('must not check')):
            closure=self.capture(measured=('kettle','kitty'));closure.check()
        self.assertNotIn('ghostty_user_config',closure.local)

    def test_mid_campaign_refusal_outputs_no_private_data(self):
        from unittest.mock import Mock
        closure=self.capture()
        raw=ObserverPilot().fixture(names=['kettle','ghostty'])
        raw['meta']['complete']=False
        recorder=standing.Recorder(self.root/'results.json',raw)
        def collect():
            (self.user/'config').write_text('/Users/private-owner/private@email.test')
            return {'printing_mib':10.}
        with self.assertRaisesRegex(SystemExit,'Ghostty user config would apply'):
            standing.config_campaign_row(closure,raw,recorder,collect)
        results=(self.root/'results.json').read_text()
        summary=standing.summarize(raw,raw['terminals'],False,{})
        report=_json.dumps(ObserverPilot().report(raw))
        for output in (results,summary,report):
            self.assertNotIn(str(self.root),output)
            self.assertNotIn('private-owner',output)
            self.assertNotIn('private@email.test',output)
        self.assertEqual(raw['config_invalid_rows'][0]['printing_mib'],10.)


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v", *sys.argv[1:]])
