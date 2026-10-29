#!/usr/bin/env python3
"""Portable regression tests for macos-compare.sh's embedded score step."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
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


class ApplePayloadTests(unittest.TestCase):
    def test_empty_completion_marker_is_polled_until_written(self):
        source = SCRIPT.read_text()
        start = source.index('cat > "$apple_run" <<\'PY\'\n') + len('cat > "$apple_run" <<\'PY\'\n')
        namespace = {"__name__": "apple_run_test"}
        exec(compile(source[start:source.index("\nPY\n", start)], str(SCRIPT), "exec"), namespace)
        namespace['process_ids'] = lambda name: set()
        namespace['close_window'] = lambda *args: None
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            marker = root / 'marker'
            fd = os.open(marker, os.O_CREAT | os.O_WRONLY, 0o600)
            def launch(*args):
                marker.touch()
                return subprocess.CompletedProcess([], 0, 'window id 7', '')
            namespace['osascript'] = launch
            argv = ['apple-run', '--app', 'terminal', '--timeout', '1', '--launch-timeout', '1',
                    '--pid-log', str(root / 'pids'), '--marker-dir', raw, '--window-log', str(root / 'windows'),
                    '--', '/bin/true']
            with mock.patch.dict(os.environ, {'APPLE_WINDOW_LOG': str(root / 'windows')}), \
                    mock.patch.object(sys, 'argv', argv), mock.patch.object(tempfile, 'mkstemp', return_value=(fd, str(marker))), \
                    mock.patch.object(namespace['time'], 'sleep', side_effect=lambda _: marker.write_text('0\n')):
                self.assertEqual(namespace['main'](), 0, 'an unpublished status is not an invalid status')

    @unittest.skipUnless(shutil.which("fish"), "Fish unavailable")
    def test_terminal_payload_records_status_under_fish(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        start = source.index('cat > "$apple_run" <<\'PY\'\n')
        start += len('cat > "$apple_run" <<\'PY\'\n')
        end = source.index("\nPY\n", start)
        namespace = {"__name__": "apple_run_test"}
        exec(compile(source[start:end], str(SCRIPT), "exec"), namespace)
        sentinel = "argument with spaces, 'quotes', $dollars and ; semicolons"

        def launch(lines, timeout):
            command = json.loads(lines[0].split("do script ", 1)[1])
            result = subprocess.run(
                [shutil.which("fish"), "--no-config", "-c", command],
                capture_output=True, text=True, timeout=2,
            )
            self.assertIn(sentinel, result.stdout, result.stderr)
            return subprocess.CompletedProcess([], 0, "tab 1 of window id 7\n", "")

        namespace["osascript"] = launch
        namespace["process_ids"] = lambda name: set()
        namespace["close_window"] = lambda app, window: None
        with tempfile.TemporaryDirectory(prefix="apple payload ' ") as raw:
            root = Path(raw)
            arguments = [
                "apple-run.py", "--app", "terminal", "--timeout", "0.2",
                "--launch-timeout", "2", "--pid-log", str(root / "pids"),
                "--marker-dir", str(root), "--window-log", str(root / "windows"),
                "--", sys.executable, "-c",
                "import sys; print(sys.argv[1]); sys.exit(7)", sentinel,
            ]
            with mock.patch.dict(os.environ, {"APPLE_WINDOW_LOG": str(root / "windows")}):
                with mock.patch.object(sys, "argv", arguments):
                    self.assertEqual(namespace["main"](), 7)


class ProcessCleanupTests(unittest.TestCase):
    def registry(self):
        source = SCRIPT.read_text()
        marker = 'cat > "$process_registry" <<\'PY\'\n'
        start = source.index(marker) + len(marker)
        namespace = {'__name__': 'registry_test'}
        exec(compile(source[start:source.index('\nPY\n', start)], str(SCRIPT), 'exec'), namespace)
        return namespace

    def test_registry_rechecks_identity_before_each_signal_and_unregisters(self):
        registry = self.registry()
        with tempfile.TemporaryDirectory() as raw:
            log = Path(raw) / 'pids'
            with mock.patch.object(registry['subprocess'], 'run', side_effect=[
                subprocess.CompletedProcess([], 0, 'original start\n', ''),
                subprocess.CompletedProcess([], 0, 'original start\n', ''),
                subprocess.CompletedProcess([], 0, 'reused start\n', ''),
            ]), mock.patch.object(registry['os'], 'kill') as kill:
                start = registry['register'](log, 123)
                self.assertEqual(log.read_text(), '123\toriginal start\n')
                registry['update'](log, 'signal-all', sig='TERM')
                registry['update'](log, 'signal-all', sig='KILL')
                kill.assert_called_once_with(123, registry['signal'].SIGTERM)
            registry['update'](log, 'unregister', 123, start)
            self.assertEqual(log.read_text(), '')

    def test_cleanup_does_not_signal_a_reused_pid(self):
        source = SCRIPT.read_text()
        start = source.index('cleanup() {')
        end = source.index('\ntrap cleanup EXIT', start)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            log = root / 'pids'
            log.write_text('123\tWed Oct 28 10:00:00 2026\n')
            # Use a shell kill double so the regression cannot signal a real PID.
            prelude = '''kill() { printf '%s\\n' "$*" >> "$calls"; }
sleep() { :; }
'''
            # The historical implementation logged just the numeric PID.
            if 'process_registry=' not in source:
                log.write_text('123\n')
            else:
                marker = 'cat > "$process_registry" <<\'PY\'\n'
                helper_start = source.index(marker) + len(marker)
                (root / 'registry.py').write_text(source[helper_start:source.index('\nPY\n', helper_start)])
            (root / 'ps').write_text('#!/bin/sh\necho checked >> "$ps_calls"\n'
                                    'printf "Thu Oct 29 10:00:00 2026\\n"\n')
            (root / 'ps').chmod(0o755)
            (root / 'python3').write_text('#!' + sys.executable + '\nimport os, runpy, sys\n'
                'def record(*args):\n'
                '    with open(os.environ["calls"], "a") as log: log.write(str(args))\n'
                'os.kill = record\nsys.argv = sys.argv[1:]\nrunpy.run_path(sys.argv[0], run_name="__main__")\n')
            (root / 'python3').chmod(0o755)
            program = prelude + source[start:end] + '\ncleanup\n'
            result = subprocess.run(['bash', '-c', program], env=dict(
                os.environ, tmp_dir=str(root / 'unused'), peer_pid_log=str(log),
                idle_stop_log=str(root / 'stops'), apple_window_log=str(root / 'windows'),
                process_registry=str(root / 'registry.py'), calls=str(root / 'calls'),
                ps_calls=str(root / 'ps-calls'),
                PATH=str(root) + os.pathsep + os.environ['PATH']),
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((root / 'calls').exists(), 'cleanup signaled a PID with a different start time')
            self.assertEqual((root / 'ps-calls').read_text().splitlines(), ['checked', 'checked'])


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
