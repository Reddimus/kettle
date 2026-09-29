#!/usr/bin/env python3
"""GUI-free checks for macos-standing.py's parsing, statistics, and vtebench fix."""

from __future__ import annotations

import importlib.util
import math
import shutil
import sys
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
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


class RoundStatistics(unittest.TestCase):
    def test_ratio_of_rounds_is_round_paired_seeded_and_counts_wins(self) -> None:
        base = [10.0, 10.0, 10.0, 10.0]
        test = [9.0, 9.0, 11.0, 9.0]
        stats = standing.ratio_of_rounds(base, test)
        self.assertAlmostEqual(stats["ratio"], 0.95)
        self.assertEqual((stats["wins"], stats["n"]), (3, 4))
        self.assertLessEqual(stats["low"], stats["ratio"])
        self.assertLessEqual(stats["ratio"], stats["high"])
        self.assertEqual(stats, standing.ratio_of_rounds(base, test), "same seed, same interval")
        # Rounds pair by index: a missing round drops its pair, not a shift.
        shifted = standing.ratio_of_rounds([10.0, None, 10.0], [9.0, 1.0, 11.0])
        self.assertEqual(shifted["n"], 2)
        self.assertAlmostEqual(shifted["ratio"], 1.0)

    def test_zero_values_stay_in_the_pairs(self) -> None:
        # A terminal with no wakeups in a round won that round; dropping the
        # pair would change n and the 80 % threshold.
        stats = standing.paired([1.0, 0.0, 1.0], [0.0, 0.0, 2.0])
        self.assertEqual((stats["n"], stats["wins"]), (3, 1))
        self.assertEqual(stats["ratio"], 1.0)
        behind = standing.ratio_of_rounds([0.0], [0.5])
        self.assertEqual(behind["high"], math.inf)
        self.assertEqual(behind["wins"], 0)

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

    def test_a_probe_that_ignores_stop_is_killed_with_its_group_and_reported(self) -> None:
        import os
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "stubborn.sh"
            # Ignores SIGTERM and starts a child in its own process group.
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
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)

    def test_a_terminal_the_probe_had_to_kill_is_not_a_clean_stop(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            probe = work / "killer.sh"
            # Stops when asked, but reports that it had to SIGKILL the terminal.
            probe.write_text('#!/bin/sh\ntrap \'echo "{\\"killed\\": true}" > "$1"; exit 0\' TERM\n'
                             'while :; do sleep 0.05; done\n')
            probe.chmod(0o755)
            runner = standing.Runner({"launch": probe, "stamp": probe}, work, {"kettle": "/bin/true"})
            process = runner.launch("kettle", "true", 1)
            time.sleep(0.3)
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
            import os
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)

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
                       "Time Machine backup running", "load 2.50", "scripts/perf has local changes",
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
        self.assertIsNotNone(standing.harness_revision()["harness_tree"])

    def test_session_metadata_keys_are_distinct(self) -> None:
        # A repeated key in the metadata literal silently drops the first value.
        import ast
        import inspect

        tree = ast.parse(inspect.getsource(standing.main).lstrip())
        for node in ast.walk(tree):
            if isinstance(node, ast.Dict):
                keys = [k.value for k in node.keys if isinstance(k, ast.Constant)]
                self.assertEqual(len(keys), len(set(keys)), f"duplicate keys in {keys}")

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


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v"])
