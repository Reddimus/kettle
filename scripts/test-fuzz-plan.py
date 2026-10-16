#!/usr/bin/env python3
"""Tests for the fuzz workflow's run planner."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).with_name("fuzz-plan.py")
SPEC = importlib.util.spec_from_file_location("fuzz_plan", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
PLAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLAN)

TABLE = json.dumps(
    {
        "targets": [
            {"name": "wire", "crates": ["kettle-media"], "nightly": 1200},
            {"name": "render", "crates": ["kettle-media-render"], "nightly": 1200, "sunday": 3600},
            {"name": "term", "crates": ["kettle-core", "kettle-vt"], "nightly": 600},
        ]
    }
)
MONDAY, SUNDAY = 0, 6


def seconds_by_target(include: list[dict]) -> dict[str, int]:
    return {row["target"]: row["seconds"] for row in include}


class Plan(unittest.TestCase):
    def setUp(self) -> None:
        self.targets = PLAN.load_targets(TABLE)

    def test_a_pull_request_runs_every_target_for_a_minute(self) -> None:
        include = PLAN.plan(self.targets, "pull_request", MONDAY)
        self.assertEqual(seconds_by_target(include), {"wire": 60, "render": 60, "term": 60})
        self.assertFalse(any(row["minimize"] for row in include))

    def test_a_night_runs_nightly_times_and_sunday_runs_longer_and_minimizes(self) -> None:
        weekday = PLAN.plan(self.targets, "schedule", MONDAY)
        self.assertEqual(seconds_by_target(weekday), {"wire": 1200, "render": 1200, "term": 600})
        self.assertFalse(any(row["minimize"] for row in weekday))
        sunday = PLAN.plan(self.targets, "schedule", SUNDAY)
        self.assertEqual(seconds_by_target(sunday), {"wire": 1200, "render": 3600, "term": 600})
        self.assertTrue(all(row["minimize"] for row in sunday))

    def test_a_dispatch_runs_what_it_names_for_as_long_as_it_asks_up_to_five_hours(self) -> None:
        include = PLAN.plan(self.targets, "workflow_dispatch", MONDAY, "term  wire", "7200")
        self.assertEqual(seconds_by_target(include), {"wire": 7200, "term": 7200})
        everything = PLAN.plan(self.targets, "workflow_dispatch", MONDAY, "", "")
        self.assertEqual(set(seconds_by_target(everything).values()), {3600})
        self.assertEqual(len(everything), 3)
        capped = PLAN.plan(self.targets, "workflow_dispatch", MONDAY, "wire", "999999")
        self.assertEqual(capped[0]["seconds"], 18000)
        # Five hours of fuzzing still ends inside a hosted job's six.
        self.assertLessEqual(capped[0]["timeout"], 360)

    def test_a_dispatch_refuses_unknown_targets_and_bad_seconds(self) -> None:
        for targets, seconds in [("wire nope", ""), ("", "0"), ("", "-5"), ("", "1e3"), ("", "1234567")]:
            with self.subTest(targets=targets, seconds=seconds):
                with self.assertRaises(PLAN.PlanError):
                    PLAN.plan(self.targets, "workflow_dispatch", MONDAY, targets, seconds)

    def test_rows_carry_their_seed_crates_and_a_job_timeout(self) -> None:
        include = PLAN.plan(self.targets, "schedule", MONDAY)
        term = next(row for row in include if row["target"] == "term")
        self.assertEqual(term["crates"], "-p kettle-core -p kettle-vt")
        self.assertEqual(term["timeout"], 10 + PLAN.OVERHEAD_MINUTES)

    def test_a_malformed_table_is_refused(self) -> None:
        for table in [
            "{}",
            '{"targets": [{"name": "Bad", "crates": ["kettle-media"], "nightly": 600}]}',
            '{"targets": [{"name": "a", "crates": [], "nightly": 600}]}',
            '{"targets": [{"name": "a", "crates": ["serde"], "nightly": 600}]}',
            '{"targets": [{"name": "a", "crates": ["kettle-media"], "nightly": 5}]}',
            '{"targets": [{"name": "a", "crates": ["kettle-media"], "nightly": 600, "sunday": 99999}]}',
            '{"targets": [{"name": "a", "crates": ["kettle-media"], "nightly": 600},'
            ' {"name": "a", "crates": ["kettle-media"], "nightly": 600}]}',
        ]:
            with self.subTest(table=table):
                with self.assertRaises(PLAN.PlanError):
                    PLAN.load_targets(table)

    def test_bin_names_reads_every_bin_block(self) -> None:
        manifest = (
            '[package]\nname = "kettle-fuzz"\n\n[[bin]]\nname = "wire"\npath = "a.rs"\n\n'
            '[[bin]]\nname = "render"\npath = "b.rs"\n'
        )
        self.assertEqual(PLAN.bin_names(manifest), {"wire", "render"})

    def test_the_repository_table_matches_the_fuzz_manifest(self) -> None:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--event", "schedule", "--weekday", "0"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith("matrix="))
        include = json.loads(result.stdout.removeprefix("matrix="))["include"]
        self.assertTrue(include)


if __name__ == "__main__":
    unittest.main()
