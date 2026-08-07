#!/usr/bin/env python3
"""Portable regression tests for macos-compare.sh's embedded score step."""

import json
import subprocess
import sys
import tempfile
import unittest
from dataclasses import dataclass
from pathlib import Path


SCRIPT = Path(__file__).with_name("macos-compare.sh")


def embedded_scorer() -> str:
    source = SCRIPT.read_text(encoding="utf-8")
    invocation = "score_status=0\npython3 - "
    start = source.index(invocation)
    start = source.index("<<'PY' || score_status=$?\n", start)
    start += len("<<'PY' || score_status=$?\n")
    end = source.index('\nPY\n\necho ""\necho "results:"', start)
    return source[start:end]


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value) + "\n", encoding="utf-8")


@dataclass
class ScoreResult:
    returncode: int
    stdout: str
    stderr: str
    score: dict[str, object]


def run_score(terminals: list[str]) -> ScoreResult:
    with tempfile.TemporaryDirectory() as scratch:
        root = Path(scratch)
        startup = root / "startup.json"
        flood = root / "flood.json"
        ansi = root / "ansi.json"
        rss_tsv = root / "rss.tsv"
        rss_json = root / "rss.json"
        idle_tsv = root / "idle.tsv"
        idle_json = root / "idle.json"
        peer_status = root / "peer-status.tsv"
        metric_skips = root / "metric-skips.tsv"
        live = root / "live.json"
        score = root / "score.json"

        timing_rows = [
            {"command": name, "median": float(index + 1)}
            for index, name in enumerate(terminals)
        ]
        for path in (startup, flood, ansi):
            write_json(path, {"results": timing_rows})
        rss_tsv.write_text(
            "".join(
                f"{name}\t{(index + 1) * 1024}\n"
                for index, name in enumerate(terminals)
            ),
            encoding="utf-8",
        )
        idle_tsv.write_text(
            "".join(
                f"{name}\t{float(index + 1)}\n"
                for index, name in enumerate(terminals)
            ),
            encoding="utf-8",
        )
        all_peers = [
            "kettle",
            "ghostty",
            "kitty",
            "wezterm",
            "alacritty",
            "terminal",
            "iterm2",
        ]
        peer_status.write_text(
            "".join(
                f"{name}\t{'active' if name in terminals else 'skipped'}\t"
                f"{'not present in fixture' if name not in terminals else ''}\n"
                for name in all_peers
            ),
            encoding="utf-8",
        )
        metric_skips.write_text("", encoding="utf-8")
        write_json(live, {"error": "advisory probe omitted from score fixture"})

        completed = subprocess.run(
            [
                sys.executable,
                "-",
                str(startup),
                str(flood),
                str(ansi),
                str(rss_tsv),
                str(rss_json),
                str(idle_tsv),
                str(idle_json),
                str(peer_status),
                str(metric_skips),
                str(live),
                str(score),
            ],
            input=embedded_scorer(),
            text=True,
            capture_output=True,
            check=False,
        )
        return ScoreResult(
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            score=json.loads(score.read_text(encoding="utf-8")),
        )


class MacosCompareScoreTests(unittest.TestCase):
    def test_kettle_only_metrics_are_ineligible(self) -> None:
        completed = run_score(["kettle"])
        self.assertEqual(completed.returncode, 1)
        score = completed.score
        self.assertFalse(score["passed"])
        self.assertEqual(score["measured_metric_count"], 5)
        self.assertEqual(score["eligible_metric_count"], 0)
        self.assertEqual(score["ineligible_metric_count"], 5)
        self.assertEqual(score["top_half_metric_count"], 0)
        for metric in score["metrics"].values():
            self.assertEqual(metric["terminal_count"], 1)
            self.assertEqual(metric["competitor_count"], 0)
            self.assertFalse(metric["eligible_for_pass"])
            self.assertFalse(metric["kettle_top_half"])
        self.assertEqual(completed.stdout.count("real competitors measured: 0"), 5)
        self.assertIn("INELIGIBLE - 0 real competitors measured", completed.stdout)

    def test_one_real_competitor_makes_each_metric_eligible(self) -> None:
        completed = run_score(["kettle", "ghostty"])
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        score = completed.score
        self.assertTrue(score["passed"])
        self.assertEqual(score["eligible_metric_count"], 5)
        self.assertEqual(score["ineligible_metric_count"], 0)
        for metric in score["metrics"].values():
            self.assertEqual(metric["terminal_count"], 2)
            self.assertEqual(metric["competitor_count"], 1)
            self.assertTrue(metric["eligible_for_pass"])


if __name__ == "__main__":
    unittest.main()
