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


KEYBLOCK_SHA256 = "ff2c9a12acd3bd7512d01b30eb0349d4ac9e073bd1931d96718951b924cb9c43"


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
        self.assertEqual(standing.OPT_IN_WORKLOADS, ("latency",))
        import inspect

        source = inspect.getsource(standing.main)
        self.assertIn('parser.add_argument("--workloads", default=",".join(WORKLOADS))', inspect.getsource(standing))
        self.assertIn('build_probes(tools, latency="latency" in workloads', source)

    def test_keyblock_is_pinned(self) -> None:
        # Every terminal and every release runs these exact bytes.
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
        import inspect

        source = inspect.getsource(standing.Runner.latency)
        self.assertIn('"--deadline-ms", str(int(budget * 1000))', source)
        self.assertIn("budget + 15", source, "open is waited on longer than the probe may run")
        probe = (HERE / "macos-standing" / "latency-probe.swift").read_text()
        self.assertIn("let postNs = try guardedPost()", probe, "the post time is taken after the guards")
        guarded = probe.split("func guardedPost() throws -> UInt64 {")[1].split("\n    }\n")[0]
        order = [guarded.index(needle) for needle in (
            "guard let events = keyEvents()", "mayPost(", "gate.withLock", "deadlineMs", "let postNs = nowNs()",
            "post(events, options)")]
        self.assertNotIn("compactMap", probe.split("func keyEvents()")[1].split("\n}\n")[0],
                         "a key-down is never posted without its key-up")
        self.assertEqual(order, sorted(order), "events built first; deadline checked and time taken under the gate")
        self.assertEqual(probe.count("exit("), 4, "only finish() ends a run; the rest are the CLI modes")
        self.assertIn("gate.lock()\n    write(object, to: path)\n    exit(code)", probe)

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

        app = standing.build_latency_probe(self.root, None)
        sealed = subprocess.run(["codesign", "--verify", "--strict", "--deep", str(app)], capture_output=True, text=True)
        self.assertEqual(sealed.returncode, 0, "nothing may change inside the bundle after signing: " + sealed.stderr)
        self.assertTrue((self.root / "KettleLatencyProbe.build.json").exists())
        built = (app / "Contents" / "MacOS" / "latency-probe").stat().st_mtime_ns
        standing.build_latency_probe(self.root, None)
        self.assertEqual((app / "Contents" / "MacOS" / "latency-probe").stat().st_mtime_ns, built,
                         "an unchanged source and identity reuse the signed probe")
        done = subprocess.run([str(app / "Contents" / "MacOS" / "latency-probe"), "--self-test"],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        subprocess.run(["swiftc", "-typecheck", str(HERE / "macos-standing" / "latency-floor.swift")],
                       check=True, capture_output=True)


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
        return {"schema": 3, "context": "fixture", "meta": meta, "terminals": ["kettle-a", "kettle-b"],
                "workloads": {workload: {"kettle-a": runs(a), "kettle-b": runs(b)}}}

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
        good = {field: 0., "metric_validity": {field: self.validity()}}
        self.assertEqual(standing.metric_value(descriptor, "latency", good), 0.)
        for change in ({"capability_version": None}, {"observed": 4}, {"expected": None}, {"valid": False}):
            bad = {field: 0., "metric_validity": {field: self.validity(**change)}}
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



if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v"])
