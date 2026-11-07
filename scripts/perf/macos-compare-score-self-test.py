#!/usr/bin/env python3
"""Portable regression tests for macos-compare.sh's embedded score step."""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
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
    def test_failed_pid_baseline_aborts_before_launch(self):
        source = SCRIPT.read_text()
        marker = 'cat > "$apple_run" <<\'PY\'\n'
        start = source.index(marker) + len(marker)
        namespace = {"__name__": "apple_run_test"}
        exec(compile(source[start:source.index("\nPY\n", start)], str(SCRIPT), "exec"), namespace)
        launch = mock.Mock(return_value=subprocess.CompletedProcess([], 1, '', 'fixture'))
        namespace["osascript"] = launch
        namespace["append_pids"] = mock.Mock()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            argv = ['apple-run', '--app', 'terminal', '--timeout', '1', '--launch-timeout', '1',
                    '--pid-log', str(root / 'pids'), '--marker-dir', raw, '--window-log', str(root / 'windows'),
                    '--', '/bin/true']
            for failure in (subprocess.TimeoutExpired('pgrep', 2),
                            subprocess.CompletedProcess([], 2, '', 'failed')):
                launch.reset_mock()
                with self.subTest(failure=failure), \
                        mock.patch.dict(os.environ, {'APPLE_WINDOW_LOG': str(root / 'windows')}), \
                        mock.patch.object(sys, 'argv', argv), \
                        mock.patch.object(subprocess, 'run', side_effect=[failure,
                                          subprocess.CompletedProcess([], 0, '123\n', '')]):
                    try:
                        namespace['main']()
                    except (RuntimeError, OSError):
                        pass
                    self.assertFalse(launch.called, 'an unknown PID baseline must prevent launch')
                    self.assertFalse((root / 'pids').exists())

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


class AppleWindowCleanupTests(unittest.TestCase):
    def setUp(self):
        source = SCRIPT.read_text(encoding="utf-8")
        marker = 'cat > "$apple_run" <<\'PY\'\n'
        start = source.index(marker) + len(marker)
        self.namespace = {"__name__": "apple_run_test"}
        exec(compile(source[start:source.index("\nPY\n", start)], str(SCRIPT), "exec"), self.namespace)
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.marker = self.root / "result"
        self.windows = self.root / "windows"
        self.windows.write_text("")
        self.app = "terminal"
        self.launch_stdout = "tab 1 of window id 7331\n"
        self.queries = [subprocess.CompletedProcess([], 0, "1001\n", ""),
                        subprocess.CompletedProcess([], 0, "1001\n2002\n", "")]
        self.query_count = 0
        self.query_window_logs = []
        self.launches = []
        self.closes = []
        self.close_result = subprocess.CompletedProcess([], 0, "", "")
        self.register = mock.Mock()
        self.namespace["append_pids"] = self.register
        for target, name in ((subprocess, "Popen"), (os, "kill"), (os, "killpg"),
                             (os, "system"), (os, "posix_spawn"), (os, "posix_spawnp")):
            if hasattr(target, name):
                self.enterContext(mock.patch.object(target, name, side_effect=AssertionError(
                    f"unexpected native call: {name}")))
        self.enterContext(mock.patch.object(subprocess, "run", side_effect=self.run_native_double))
        self.enterContext(mock.patch.object(time, "sleep", side_effect=AssertionError("unexpected polling")))
        self.enterContext(mock.patch.dict(os.environ, {"APPLE_WINDOW_LOG": str(self.windows)}))
        self.enterContext(mock.patch.object(tempfile, "mkstemp", side_effect=lambda **kwargs: (
            os.open(self.marker, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), str(self.marker))))

    def run_native_double(self, argv, **kwargs):
        app_name = "Terminal" if self.app == "terminal" else "iTerm2"
        if argv == ["pgrep", "-x", app_name]:
            self.assertEqual(kwargs["timeout"], 2)
            self.assertFalse(kwargs["check"])
            self.query_window_logs.append(self.windows.read_text() if self.windows.exists() else "")
            result = self.queries[self.query_count]
            self.query_count += 1
        elif argv[0] == "osascript":
            if not self.launches:
                self.assertEqual(self.query_count, 1, "launch needs an initial ownership snapshot")
                self.assertEqual(kwargs["timeout"], 1)
                self.launches.append(argv)
                self.marker.write_text("7\n")
                result = subprocess.CompletedProcess(argv, 0, self.launch_stdout, "")
            else:
                self.assertEqual(kwargs["timeout"], 5)
                self.closes.append(argv)
                result = self.close_result
        else:
            raise AssertionError(f"unexpected native command: {argv}")
        if isinstance(result, BaseException):
            raise result
        return result

    def run_main(self):
        argv = ["apple-run", "--app", self.app, "--timeout", "1", "--launch-timeout", "1",
                "--pid-log", str(self.root / "pids"), "--marker-dir", str(self.root),
                "--window-log", str(self.windows), "--", "/bin/true"]
        with mock.patch.object(sys, "argv", argv):
            return self.namespace["main"]()

    def assert_window_closed(self):
        if self.app == "terminal":
            window_id = "7331"
            command = 'tell application "Terminal" to close window id 7331 saving no'
        else:
            window_id = "fixture-window-7331"
            command = 'tell application "iTerm2" to close (first window whose id is "fixture-window-7331")'
        self.assertEqual(self.closes, [["osascript", "-e", command]],
                         "the returned window must be closed even when the post-launch PID query fails")
        self.assertEqual(self.windows.read_text(), f"{self.app}\t{window_id}\n")
        self.assertEqual(self.query_window_logs[-1], f"{self.app}\t{window_id}\n",
                         "record the returned window before the post-launch query")
        self.assertFalse(self.marker.exists(), "cleanup must remove the completion marker")

    def test_post_launch_pid_timeout_closes_recorded_window(self):
        self.queries[1] = subprocess.TimeoutExpired("pgrep", 2)
        with self.assertRaisesRegex(RuntimeError, "Terminal PID query timed out"):
            self.run_main()
        self.assert_window_closed()
        self.register.assert_not_called()

    def test_post_launch_pid_error_closes_recorded_window(self):
        self.queries[1] = subprocess.CompletedProcess([], 2, "", "fixture error")
        with self.assertRaisesRegex(RuntimeError, "Terminal PID query failed: 2"):
            self.run_main()
        self.assert_window_closed()
        self.register.assert_not_called()

    def test_post_launch_pid_oserror_closes_recorded_window(self):
        self.queries[1] = OSError("fixture query error")
        with self.assertRaisesRegex(OSError, "fixture query error"):
            self.run_main()
        self.assert_window_closed()

    def test_failed_initial_query_never_launches(self):
        for failure, error in ((subprocess.TimeoutExpired("pgrep", 2), RuntimeError),
                               (subprocess.CompletedProcess([], 2, "", "fixture error"), RuntimeError),
                               (OSError("fixture query error"), OSError)):
            with self.subTest(failure=failure):
                self.query_count = 0
                self.queries[0] = failure
                with self.assertRaises(error):
                    self.run_main()
                self.assertEqual(self.launches, [])
                self.assertEqual(self.closes, [])
                self.assertEqual(self.windows.read_text(), "")
                self.assertFalse(self.marker.exists())
                self.register.assert_not_called()

    def test_success_closes_window_and_registers_only_new_pids(self):
        self.assertEqual(self.run_main(), 7)
        self.assert_window_closed()
        self.register.assert_called_once_with(self.root / "pids", {2002})

    def test_iterm_pid_error_closes_recorded_window(self):
        self.app = "iterm2"
        self.launch_stdout = "fixture-window-7331\n"
        self.queries[1] = subprocess.CompletedProcess([], 2, "", "fixture error")
        with self.assertRaisesRegex(RuntimeError, "iTerm2 PID query failed: 2"):
            self.run_main()
        self.assert_window_closed()

    def test_no_initial_processes_still_launches_a_new_window(self):
        self.queries[0] = subprocess.CompletedProcess([], 1, "", "")
        self.assertEqual(self.run_main(), 7)
        self.assert_window_closed()
        self.register.assert_called_once_with(self.root / "pids", {1001, 2002})

    def test_malformed_launch_output_does_not_close_an_owner_window(self):
        for output in ("", "tab 1 of an unknown window\n"):
            with self.subTest(output=output):
                self.query_count = 0
                self.launches.clear()
                self.launch_stdout = output
                self.assertEqual(self.run_main(), 7)
                self.assertEqual(self.closes, [])
                self.assertEqual(self.windows.read_text(), "")
                self.assertFalse(self.marker.exists())

    def test_cleanup_failure_retains_window_log_and_query_error(self):
        for failure in (subprocess.TimeoutExpired("osascript", 5),
                        subprocess.CompletedProcess([], 1, "", "fixture close failure")):
            with self.subTest(failure=failure):
                self.query_count = 0
                self.launches.clear()
                self.closes.clear()
                self.windows.write_text("")
                self.marker.unlink(missing_ok=True)
                self.queries[1] = subprocess.TimeoutExpired("pgrep", 2)
                self.close_result = failure
                with self.assertRaisesRegex(RuntimeError, "Terminal PID query timed out"):
                    self.run_main()
                self.assert_window_closed()

    def test_registration_error_still_closes_recorded_window(self):
        self.register.side_effect = RuntimeError("fixture registration failure")
        with self.assertRaisesRegex(RuntimeError, "fixture registration failure"):
            self.run_main()
        self.assert_window_closed()

    def test_window_log_error_still_closes_returned_window(self):
        self.windows = self.root / "missing" / "windows"
        with self.assertRaises(FileNotFoundError):
            self.run_main()
        self.assertEqual(self.closes, [["osascript", "-e",
                         'tell application "Terminal" to close window id 7331 saving no']])
        self.assertFalse(self.marker.exists())


class ProcessCleanupTests(unittest.TestCase):
    def embedded(self, variable, delimiter):
        source = SCRIPT.read_text()
        marker = f'cat > "${variable}" <<\'{delimiter}\'\n'
        start = source.index(marker) + len(marker)
        return source[start:source.index('\n' + delimiter + '\n', start)]

    def test_fast_exit_before_registration_uses_valid_completion_marker(self):
        for marker, expected in [('0\n', 0), ('7\n', 7), ('oops7\n', 1), ('', 1)]:
            with self.subTest(marker=marker), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                wrapper = root / 'bounded-run'
                wrapper.write_text(self.embedded('bounded_run', 'EOF'))
                registry = root / 'registry.py'
                registry.write_text('import time\ntime.sleep(0.3)\n')
                child = root / 'child.py'
                child.write_text('import os, sys\nfrom pathlib import Path\n'
                                 'Path(os.environ["COMPLETION_MARKER"]).write_text(sys.argv[1])\n')
                result = subprocess.run(
                    ['bash', str(wrapper), '2', str(root / 'pids'), sys.executable, str(child), marker],
                    env=dict(os.environ, PROCESS_REGISTRY=str(registry), MARKER_DIR=raw),
                    capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, expected,
                                 'completed child was rejected before identity registration: ' + result.stderr)
                self.assertFalse(list(root.glob('direct-result-*')))

    def test_registration_timeout_reaps_unregistered_child(self):
        for behavior in ('timeout', 'empty'):
            with self.subTest(behavior=behavior), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                wrapper = root / 'bounded-run'
                wrapper.write_text(self.embedded('bounded_run', 'EOF'))
                registry = root / 'registry.py'
                registry.write_text(self.embedded('process_registry', 'PY'))
                # Let the real registry hit its timeout, or return no identity.
                ps = root / 'ps'
                ps.write_text('#!' + sys.executable + '\nimport os, time\nfrom pathlib import Path\n'
                              'deadline = time.monotonic() + 1\n'
                              'while not Path(os.environ["FIXTURE_CHILD_PID"]).exists() and time.monotonic() < deadline:\n'
                              '    time.sleep(0.01)\n' +
                              ('time.sleep(3)\n' if behavior == 'timeout' else ''))
                ps.chmod(0o755)
                child = root / 'child.py'
                child.write_text('import os, signal, sys\nfrom pathlib import Path\n'
                                 'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                                 'staged = Path(sys.argv[1] + ".tmp")\n'
                                 'staged.write_text(str(os.getpid()))\n'
                                 'os.replace(staged, sys.argv[1])  # readers never see a partial pid\n'
                                 'signal.pause()\n')
                pid_file = root / 'child-pid'
                process = subprocess.Popen(
                    ['bash', str(wrapper), '10', str(root / 'pids'), sys.executable, str(child), str(pid_file)],
                    env=dict(os.environ, PROCESS_REGISTRY=str(registry), MARKER_DIR=raw, FIXTURE_CHILD_PID=str(pid_file),
                             PATH=str(root) + os.pathsep + os.environ['PATH']),
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                child_pid = None
                try:
                    deadline = time.monotonic() + 5
                    while not pid_file.exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertTrue(pid_file.exists(), 'child did not start')
                    # The child renames a complete pid into place, so the
                    # name appearing means its contents are final.
                    child_pid = int(pid_file.read_text())
                    self.assertNotEqual(process.wait(timeout=10), 0)
                    with self.assertRaises(ProcessLookupError, msg='registration failure left a terminal running'):
                        os.kill(child_pid, 0)
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
                    if child_pid is not None:
                        try:
                            os.kill(child_pid, 9)
                        except ProcessLookupError:
                            pass

    def test_shutdown_reaps_registered_child_when_identity_probe_times_out(self):
        for shutdown in ('timeout', 'completion', 'signal'):
            with self.subTest(shutdown=shutdown), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                wrapper = root / 'bounded-run'
                wrapper.write_text(self.embedded('bounded_run', 'EOF'))
                registry = root / 'registry.py'
                registry.write_text(self.embedded('process_registry', 'PY'))
                # Registration succeeds; every later identity probe times out.
                ps = root / 'ps'
                ps.write_text('#!' + sys.executable + '\nimport os, time\nfrom pathlib import Path\n'
                              'root = Path(os.environ["MARKER_DIR"])\n'
                              'deadline = time.monotonic() + 2\n'
                              'while not (root / "child-pid").exists() and time.monotonic() < deadline:\n'
                              '    time.sleep(0.01)\n'
                              'with (root / "ps-calls").open("a") as log: log.write("checked\\n")\n'
                              'if (root / "pids").read_text(): time.sleep(3)\n'
                              'print("Wed Oct 28 10:00:00 2026")\n')
                ps.chmod(0o755)
                # Observe the polling loop before delivering a shutdown signal.
                sleep = root / 'sleep'
                sleep.write_text('#!/bin/sh\n: > "$MARKER_DIR/polling"\nexec "$FIXTURE_SLEEP" "$@"\n')
                sleep.chmod(0o755)
                child = root / 'child.py'
                child.write_text('import os, signal, sys\nfrom pathlib import Path\n'
                                 'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                                 'staged = Path(sys.argv[1] + ".tmp")\n'
                                 'staged.write_text(str(os.getpid()))\n'
                                 'os.replace(staged, sys.argv[1])  # readers never see a partial pid\n'
                                 'if sys.argv[2] == "completion":\n'
                                 '    Path(os.environ["COMPLETION_MARKER"]).write_text("7\\n")\n'
                                 'signal.pause()\n')
                log = root / 'pids'
                pid_file = root / 'child-pid'
                with (root / 'stderr').open('w+') as stderr:
                    process = subprocess.Popen(
                        ['bash', str(wrapper), '1', str(log), sys.executable, str(child), str(pid_file), shutdown],
                        env=dict(os.environ, PROCESS_REGISTRY=str(registry), MARKER_DIR=raw,
                                 FIXTURE_SLEEP=shutil.which('sleep'),
                                 PATH=str(root) + os.pathsep + os.environ['PATH']),
                        stdout=subprocess.DEVNULL, stderr=stderr,
                    )
                    child_pid = None
                    try:
                        deadline = time.monotonic() + 5
                        while (not log.exists() or not log.read_text()) and time.monotonic() < deadline:
                            time.sleep(0.01)
                        self.assertTrue(log.exists() and log.read_text(), 'registration did not succeed')
                        child_pid = int(pid_file.read_text())
                        self.assertTrue(log.read_text().startswith(f'{child_pid}\t'))
                        if shutdown == 'signal':
                            while not (root / 'polling').exists() and time.monotonic() < deadline:
                                time.sleep(0.01)
                            self.assertTrue((root / 'polling').exists(), 'wrapper did not start polling')
                            process.send_signal(signal.SIGTERM)
                        try:
                            status = process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            self.fail('registered child shutdown exceeded its deadline')
                        stderr.seek(0)
                        diagnostic = stderr.read()
                        self.assertEqual(status, 7 if shutdown == 'completion' else 124, diagnostic)
                        if shutdown != 'completion':
                            self.assertIn('command timed out after 1s', diagnostic)
                        self.assertEqual(log.read_text(), '', 'child identity was not unregistered')
                        self.assertFalse(list(root.glob('direct-result-*')), 'completion marker leaked')
                        with self.assertRaises(ProcessLookupError, msg='shutdown left a terminal running'):
                            os.kill(child_pid, 0)
                    finally:
                        if child_pid is None and pid_file.exists():
                            child_pid = int(pid_file.read_text())
                        if child_pid is not None:
                            try:
                                os.kill(child_pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=5)

    def registry(self):
        namespace = {'__name__': 'registry_test'}
        exec(compile(self.embedded('process_registry', 'PY'), str(SCRIPT), 'exec'), namespace)
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
