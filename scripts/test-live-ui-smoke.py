#!/usr/bin/env python3
"""Portable regressions for live UI smoke orchestration and ownership checks."""

import ast
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import struct
import sys
import tempfile
import unittest
import zlib
from unittest.mock import MagicMock, Mock, PropertyMock, patch

spec = importlib.util.spec_from_file_location(
    "live_ui_regressions", Path(__file__).with_name("check-live-ui-smoke.py")
)
checker = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = checker
spec.loader.exec_module(checker)


class LiveUiTests(unittest.TestCase):
    def test_linux_stat_accepts_non_ascii_comm(self):
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "stat"
            record.write_bytes(b"123 (name ) \xff\xc3\xa9) S 42 " + b"0 " * 17 + b"999\n")
            with (
                patch.object(checker.platform, "system", return_value="Linux"),
                patch.object(checker, "Path", return_value=record),
                patch.object(os, "pidfd_open", return_value=77, create=True),
                patch.object(signal, "pidfd_send_signal", create=True),
                patch.object(os, "close") as close,
            ):
                try:
                    handle = checker.StableProcessHandle.open(123)
                    self.assertEqual(handle.identity, (123, 999))
                    self.assertEqual(checker._linux_process_parent(123), 42)
                    handle.close()
                except UnicodeDecodeError as error:
                    self.fail(f"process comm must remain undecoded: {error}")
                close.assert_called_once_with(77)

    def test_repository_rejects_hidden_worktree_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            def git(*args):
                subprocess.run(["git", "-C", directory, *args], check=True,
                               capture_output=True)
            git("init", "-q")
            source = repository / "tracked.rs"
            source.write_bytes(b"original")
            git("add", "tracked.rs")
            for flag in ("--assume-unchanged", "--skip-worktree"):
                with self.subTest(flag=flag):
                    git("update-index", flag, "tracked.rs")
                    source.write_bytes(b"hidden edit")
                    with self.assertRaisesRegex(RuntimeError, "suppresses worktree inspection"):
                        checker._repository_source_identity_impl(repository)
                    git("update-index", "--no-assume-unchanged", "--no-skip-worktree", "tracked.rs")
                    source.write_bytes(b"original")

    def test_windows_nvim_uses_runtime_data_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "sandbox"
            root.mkdir()
            source = Path(directory) / "installed"
            plugin = source / "lazy" / "lazyvcs.nvim"
            plugin.mkdir(parents=True)
            (plugin / "native.lua").write_bytes(b"return {}")
            target = checker.AgentShellTarget(nvim_data=str(source),
                                              astro_config=str(source / "absent"))
            with (
                patch.object(checker.AgentShellTarget, "powershell", new_callable=PropertyMock,
                             return_value=True),
                patch.object(checker.AgentShellTarget, "validate_native_sandbox_path", return_value=root),
            ):
                target.prepare_nvim_sandbox_host(str(root))
                copied = root / "data" / "nvim-data" / "lazy" / "lazyvcs.nvim" / "native.lua"
                self.assertTrue(copied.is_file(), "Windows runtime data must be in data/nvim-data")
                self.assertTrue(target.nvim_snapshot_identity(str(root))["present"])
                (root / "run" / "lazyvcs-loaded-source").write_bytes(str(copied).encode() + b"\n")
                self.assertEqual(target.lazyvcs_loaded_source_identity(str(root))["module_relative"],
                                 "native.lua")
            command = checker.lazyvcs_sidebar_command("repo", "READY", windows=True)
            self.assertIn("/nvim-data/lazy/lazyvcs.nvim", command)
            self.assertIn("/nvim/lazy/lazyvcs.nvim",
                          checker.lazyvcs_sidebar_command("repo", "READY", windows=False))

    def test_readiness_marker_uses_lf_on_windows(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "run").mkdir()
            marker = root / "run" / "ready"
            marker.write_bytes(b"READY\n")
            with (
                patch.object(checker.AgentShellTarget, "validate_native_sandbox_path", return_value=root),
                patch.object(os, "linesep", "\r\n"),
            ):
                try:
                    checker.AgentShellTarget().wait_for_nvim_sandbox_marker(
                        directory, "ready", "READY", timeout_s=0.1)
                except RuntimeError as error:
                    self.fail(f"LF marker from Neovim must be accepted: {error}")
                marker.write_bytes(b"READY\r\n")
                with self.assertRaisesRegex(RuntimeError, "unexpected"):
                    checker.AgentShellTarget().wait_for_nvim_sandbox_marker(
                        directory, "ready", "READY", timeout_s=0.1)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "requires Unix FIFOs")
    def test_identity_replacement_with_fifo_never_blocks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "record"
            real_open = os.open
            parent_fd = real_open(root, os.O_RDONLY)
            self.addCleanup(os.close, parent_fd)
            for reader in (
                lambda: checker._regular_file_digest(path, checker.SnapshotCopyBudget()),
                lambda: checker._regular_file_digest_at(parent_fd, path.name, path,
                                                       path.stat(), checker.SnapshotCopyBudget()),
                lambda: checker.read_bounded_regular_text(path),
            ):
                with self.subTest(reader=reader):
                    if path.exists():
                        path.unlink()
                    path.write_bytes(b"READY\n")
                    def replace_before_open(name, flags, **kwargs):
                        path.unlink()
                        os.mkfifo(path)
                        self.assertTrue(flags & os.O_NONBLOCK,
                                        "identity open can block on a replacement FIFO")
                        return real_open(name, flags, **kwargs)
                    with patch.object(os, "open", side_effect=replace_before_open):
                        with self.assertRaisesRegex(RuntimeError, "non-regular|changed while opening"):
                            reader()

    def test_wsl_preflight_closes_exited_pidfd_once(self):
        with patch.object(checker.AgentShellTarget, "run_command",
                          return_value=subprocess.CompletedProcess([], 0, "", "")) as run:
            checker.AgentShellTarget(mode="wsl").require_wsl_pidfd_cleanup()
        program = run.call_args_list[1].args[0][2]
        definition = next(node for node in ast.parse(program).body
                          if isinstance(node, ast.FunctionDef) and node.name == "exact_env_fds")
        fake_os = Mock()
        fake_os.stat.return_value.st_uid = 501
        fake_os.geteuid.return_value = 501
        fake_os.pidfd_open.return_value = 77
        closed = set()
        def close(fd):
            if fd in closed:
                raise OSError("pidfd closed twice")
            closed.add(fd)
        fake_os.close.side_effect = close
        namespace = {"os": fake_os, "glob": Mock(), "root": "/tmp/fixture",
                     "alive": lambda _fd: False}
        namespace["glob"].glob.return_value = ["/proc/123/environ"]
        exec(compile(ast.Module(body=[definition], type_ignores=[]), "<preflight>", "exec"), namespace)
        try:
            self.assertEqual(namespace["exact_env_fds"](), [])
        except OSError as error:
            self.fail(str(error))
        fake_os.close.assert_called_once_with(77)

    # The cleanup sends SIGSTOP and SIGKILL, which Windows does not define.
    @unittest.skipIf(sys.platform == "win32", "no SIGSTOP or SIGKILL on Windows")
    def test_darwin_descendant_pid_reuse_is_not_signalled(self):
        for relationship in ("reparented", "ancestor_reused", "candidate_reused", "owned_chain"):
            with self.subTest(relationship=relationship):
                anchor = Mock(pid=100, identity=(100, 1))
                descendants = {pid: Mock(pid=pid, identity=(pid, 2)) for pid in (200, 300)}
                for handle in [anchor, *descendants.values()]:
                    handle.matches_current.return_value = True
                scans = 0
                def snapshot(**_kwargs):
                    nonlocal scans
                    scans += 1
                    parents = {200: 100, 300: 200}
                    if scans == 1:
                        return parents, {100, 200, 300}
                    if scans <= 3:
                        if relationship == "reparented":
                            parents[200] = 999
                        elif relationship == "ancestor_reused":
                            anchor.matches_current.return_value = False
                        elif relationship == "candidate_reused":
                            descendants[200 if scans == 2 else 300].matches_current.return_value = False
                        return parents, {100, 200, 300}
                    return {}, set()
                ticks = iter(index * 0.01 for index in range(2000))
                with (
                    patch.object(checker.platform, "system", return_value="Darwin"),
                    patch.object(checker.AgentShellTarget, "_native_process_snapshot", side_effect=snapshot),
                    patch.object(checker.AgentShellTarget, "native_nvim_sandbox_handles",
                                 side_effect=[{anchor.identity: anchor}] + [{}] * 100),
                    patch.object(checker, "_open_stable_process_if_present",
                                 side_effect=lambda pid, _description: descendants[pid]),
                    patch.object(checker, "_process_state", return_value="Z"),
                    patch.object(checker.time, "monotonic", side_effect=lambda: next(ticks)),
                    patch.object(checker.time, "sleep"),
                ):
                    checker.AgentShellTarget.terminate_native_nvim_sandbox_processes(Path("sandbox"))
                for handle in descendants.values():
                    if relationship == "owned_chain":
                        self.assertEqual(handle.signal.call_args_list,
                                         [unittest.mock.call(signal.SIGSTOP), unittest.mock.call(signal.SIGKILL)])
                    else:
                        self.assertFalse(handle.signal.called,
                                         "reused descendant PID without current ancestry was signalled")
                    handle.close.assert_called_once()

    @unittest.skipIf(os.name == "nt", "requires Unix PTY wrapper")
    def test_pty_wrapper_excludes_zombie_session_members(self):
        live = checker.LiveKettle("unused", Path("unused"), Path("unused"))
        try:
            argv, _env = live._prepare_unix_pty_tracker(["unused", "-e", "/bin/sh"])
            wrapper = Path(argv[argv.index("-e") + 1]).read_text()
            polling = [node for node in ast.parse(wrapper).body if isinstance(node, ast.While)][-1]
            calls = 0
            def inventory(argv, **_kwargs):
                nonlocal calls
                calls += 1
                states = "200 Z\n201 S\n" if calls == 1 else "200 Z\n"
                output = states if "stat=" in " ".join(argv) else "".join(
                    line.split()[0] + "\n" for line in states.splitlines())
                return Mock(pid=333, returncode=0, communicate=lambda: (output, ""))
            def sleep(_seconds):
                self.assertLess(calls, 2, "PTY wrapper waited for a zombie session member")
            fake_os = Mock()
            fake_os.getpid.return_value = 100
            fake_os.getsid.return_value = 100
            namespace = {"os": fake_os, "subprocess": Mock(Popen=inventory),
                         "time": Mock(sleep=sleep), "payload_signal": None,
                         "payload_status": 7, "session": 100}
            with self.assertRaises(SystemExit) as exited:
                exec(compile(ast.Module(body=[polling], type_ignores=[]), "<wrapper>", "exec"), namespace)
            self.assertEqual(exited.exception.code, 7)
            self.assertEqual(calls, 2)
        finally:
            live._run_post_exit_cleanups()

    def test_claude_probe_translates_host_checkout_for_wsl(self):
        target = Mock(mode="wsl")
        target.run_command.return_value = subprocess.CompletedProcess([], 0, "/mnt/d/my checkout\n", "")
        class Launched(Exception):
            pass
        live = Mock()
        def launch(method, *, params, **_kwargs):
            self.assertEqual(method, "send_text")
            self.assertIn("cd '/mnt/d/my checkout'", params["text"],
                          "Claude probe used the host path inside WSL")
            raise Launched()
        live.ctl.side_effect = launch
        with (
            patch.object(checker, "AgentShellTarget", return_value=target),
            patch.object(subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 0, "D:\\my checkout\n", "")),
        ):
            with self.assertRaises(Launched):
                checker.probe_claude_diff_panel(live, Path("unused"), [])
        target.run_command.assert_called_once_with(
            ["wslpath", "-a", "-u", "D:\\my checkout"], timeout=15)

    @unittest.skipIf(os.name == "nt", "requires a POSIX shell")
    def test_underline_fixture_has_plain_path_neighbors(self):
        with tempfile.TemporaryDirectory() as directory:
            command = checker.underline_command(Path(directory), None)
            output = subprocess.run(["/bin/sh", "-c",
                "git() { :; }; delta() { cat; }; less() { cat; }; " + command],
                check=True, capture_output=True, text=True).stdout.splitlines()
        checked = 0
        for index, line in enumerate(output):
            if not line.startswith(("PATH_POSIX_SENTINEL_", "PATH_WIN_SENTINEL_")):
                continue
            neighbors = output[max(0, index - 1):index] + output[index + 1:index + 2]
            self.assertTrue(any("PATH_SPACER_" in neighbor for neighbor in neighbors),
                            "path fixture has no plain neighbor for the leak check")
            checked += 1
        self.assertEqual(checked, 240)

    def test_underline_analyzer_rejects_spacer_leak(self):
        import inspect
        tree = ast.parse(inspect.getsource(checker.run_underline))
        body = tree.body[0].body
        start = next(i for i, node in enumerate(body) if isinstance(node, ast.AnnAssign)
                     and isinstance(node.target, ast.Name) and node.target.id == "top_sentinels")
        analysis = ast.Module(body=body[start:-1], type_ignores=[])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def png_chunk(kind, payload):
                body = kind + payload
                return (struct.pack(">I", len(payload)) + body
                        + struct.pack(">I", zlib.crc32(body)))

            for frame in range(1, 9):
                number = [1, 2, 3, 4, 5, 4, 3, 2][frame - 1]
                lines = [f"UNDERLINE_SENTINEL_{number:03}", "PATH_SPACER_BEFORE",
                         "PATH_POSIX_SENTINEL_001 crates/kettle-ui/src/app.rs:1:1",
                         "PATH_SPACER_AFTER"]
                cells = [{"row": row, "col": col, "ch": ch, "any_underline": row == 0}
                         for row, line in enumerate(lines) for col, ch in enumerate(line)]
                (root / f"cells-{frame}.json").write_text(json.dumps({"rows": 4, "cols": 80, "cells": cells}))
                (root / f"geometry-{frame}.json").write_text(json.dumps({
                    "content": {"x": 0, "y": 0}, "cell": {"width": 8, "height": 10}}))
                pixels = [bytearray(640 * 4) for _ in range(40)]
                path_start = lines[2].index("crates/")
                for row, columns in ((0, range(22)), (2, range(path_start, len(lines[2]))),
                                     (3, range(path_start, len(lines[2])))):
                    for col in columns:
                        x = col * 8 + 4
                        pixels[row * 10 + 8][x * 4:x * 4 + 4] = b"\xff" * 4
                (root / f"frame-{frame}.png").write_bytes(
                    b"\x89PNG\r\n\x1a\n"
                    + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 640, 40, 8, 6, 0, 0, 0))
                    + png_chunk(b"IDAT", zlib.compress(b"".join(b"\0" + row for row in pixels)))
                    + png_chunk(b"IEND", b""))
            namespace = dict(checker.__dict__, out=root, svn_enabled=False)
            with self.assertRaisesRegex(SystemExit, "path underline leaked to adjacent row"):
                exec(compile(analysis, "<underline-analysis>", "exec"), namespace)

    def test_nondumpable_child_publishes_readiness_after_prctl(self):
        import ctypes
        import inspect
        program = next(node.value for node in ast.walk(ast.parse(
            inspect.getsource(checker.live_helper_selftest))) if isinstance(node, ast.Constant)
            and isinstance(node.value, str) and "child=os.fork()" in node.value
            and "prctl(4,0,0,0,0)" in node.value)
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "orphan.pid"
            def disable(*_args):
                self.assertFalse(record.exists(), "readiness preceded disabled dumpability")
                return 0
            class Ready(Exception):
                pass
            def sleep(_seconds):
                self.assertTrue(record.exists(), "nondumpable child did not publish readiness")
                self.assertEqual(record.read_text().strip(), str(os.getpid()))
                raise Ready()
            with (
                patch.object(os, "fork", return_value=0, create=True),
                patch.object(os, "setsid", create=True),
                patch.object(ctypes, "CDLL", return_value=Mock(prctl=disable)),
                patch.object(checker.time, "sleep", side_effect=sleep),
                patch.object(sys, "argv", ["fixture", str(record)]),
            ):
                with self.assertRaises(Ready):
                    exec(program, {})

    def test_cursor_wait_uses_unique_runtime_only_marker(self):
        import inspect
        function = ast.parse(inspect.getsource(checker.run_agent_tui)).body[0]
        loop = next(node for node in ast.walk(function) if isinstance(node, ast.For)
                    and isinstance(node.target, ast.Tuple)
                    and [item.id for item in node.target.elts] == ["queued_input", "label"])
        stop = next(i for i, node in enumerate(loop.body) if isinstance(node, ast.Assign)
                    and any(isinstance(target, ast.Name) and target.id == "fixture_screen"
                            for target in node.targets))
        loop.body = loop.body[:stop]
        commands = []
        markers = []
        live = Mock()
        def ctl(method, *, params, **_kwargs):
            if method == "send_text":
                commands.append(params["text"])
        def wait(marker, **_kwargs):
            self.assertNotIn(marker, commands[-1], "cursor readiness matched the echoed command")
            self.assertNotIn(marker, markers, "cursor readiness reused an earlier frame marker")
            markers.append(marker)
        live.ctl.side_effect = ctl
        live.wait_for_text.side_effect = wait
        namespace = dict(checker.__dict__, live=live, shell_target=Mock(powershell=True))
        exec(compile(ast.Module(body=[loop], type_ignores=[]), "<cursor-fixture>", "exec"), namespace)
        self.assertEqual(len(markers), 2)

    def test_tmux_probe_cleans_socket_after_failed_startup(self):
        for outcome in (subprocess.TimeoutExpired("tmux", 5),
                        subprocess.CompletedProcess([], 1, "", "startup failed")):
            with self.subTest(outcome=outcome):
                target = Mock()
                target.command_available.return_value = True
                target.require_command_path.return_value = "/usr/bin/python3"
                target.run_command.side_effect = [subprocess.CompletedProcess([], 0, "tmux 3.4", ""), outcome]
                with patch.object(checker, "cleanup_tmux_server") as cleanup:
                    if isinstance(outcome, Exception):
                        with self.assertRaises(subprocess.TimeoutExpired):
                            checker.probe_tmux_sixel_capability(target)
                    else:
                        self.assertIn("could not start", checker.probe_tmux_sixel_capability(target)["reason"])
                    self.assertEqual(cleanup.call_count, 1,
                                     "private tmux socket cleanup was skipped after startup failure")

    def test_tmux_cleanup_preserves_startup_exception(self):
        target = Mock()
        target.require_command_path.return_value = "/usr/bin/python3"
        original = subprocess.TimeoutExpired("tmux", 5)
        target.run_command.side_effect = [subprocess.CompletedProcess([], 0, "tmux 3.4", ""), original]
        with patch.object(checker, "cleanup_tmux_server", side_effect=RuntimeError("cleanup failed")):
            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                checker.probe_tmux_sixel_capability(target)
        self.assertIs(raised.exception, original)

    def test_split_fixture_reads_all_pane_pages_and_restarts_stale_snapshot(self):
        import inspect
        function = ast.parse(inspect.getsource(checker.run_search_selection)).body[0]
        locate = next(node for node in function.body if isinstance(node, ast.FunctionDef)
                      and node.name == "pane_text_point")
        namespace = dict(checker.__dict__)
        exec(compile(ast.Module(body=[locate], type_ignores=[]), "<split-fixture>", "exec"), namespace)
        live = checker.LiveKettle("unused", Path("unused"), Path("unused"))
        starts = 0
        pages = []
        def response(method, params=None):
            nonlocal starts
            if method == "ui_geometry":
                return {"cell": {"width": 8, "height": 10}, "padding": {"x": 0, "y": 0},
                        "pane_titlebars": [{"pane": 9, "pane_rect": {"x": 0, "y": 0},
                                            "rect": {"y": 0, "height": 10}}]}
            self.assertEqual(params["pane"], 9)
            pages.append(dict(params))
            if "cursor" not in params:
                starts += 1
                return {"cells": [], "rows": 30, "cols": 80, "next_cursor": 1536,
                        "snapshot": f"snapshot-{starts}"}
            if params["snapshot"] == "snapshot-1":
                raise SystemExit("stale_snapshot")
            return {"cells": [{"row": 29, "col": col + 3, "ch": ch}
                              for col, ch in enumerate("MARKER")], "next_cursor": None}
        ticks = iter(range(100))
        with (
            patch.object(live, "json_ctl", side_effect=response),
            patch.object(checker.time, "monotonic", side_effect=lambda: next(ticks)),
            patch.object(checker.time, "sleep"),
        ):
            try:
                self.assertEqual(namespace["pane_text_point"](live, 9, "MARKER"), (26.0, 305.0))
            except SystemExit as error:
                self.fail(f"split fixture ignored later cell pages: {error}")
        self.assertEqual(starts, 2)
        self.assertEqual(pages[-1], {"pane": 9, "limit": 1536, "cursor": 1536,
                                   "snapshot": "snapshot-2"})

    def test_receipts_reap_clipboard_when_kettle_teardown_raises(self):
        import inspect
        for function in (checker.run_image_paste_receipt, checker.run_video_paste_receipt):
            with self.subTest(scenario=function.__name__):
                body = ast.parse(inspect.getsource(function)).body[0].body
                finalizer = next(node.finalbody for node in body if isinstance(node, ast.Try)
                                 and node.finalbody)
                owner = Mock()
                owner.__exit__ = Mock(side_effect=RuntimeError("Kettle teardown failed"))
                clipboard = Mock()
                clipboard.poll.return_value = None
                namespace = dict(checker.__dict__, live=object(), live_owner=owner,
                                 clipboard_owner=clipboard)
                with self.assertRaisesRegex(RuntimeError, "Kettle teardown failed"):
                    exec(compile(ast.Module(body=finalizer, type_ignores=[]), "<receipt-cleanup>", "exec"), namespace)
                self.assertEqual(clipboard.terminate.call_count, 1,
                                 "clipboard provider survived failed Kettle teardown")
                clipboard.wait.assert_called_once_with(timeout=2)

    def test_windows_title_fixtures_submit_with_enter_key(self):
        class Submitted(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scenarios = [lambda: checker.run_tab_title("unused", root),
                         lambda: checker.run_split_titlebar_position(
                             kettle="unused", cfg=root / "config", out=root,
                             extra_args=[], command="fixture", marker="READY", expected_path="cwd",
                             truncated_title="title", title_at_bottom=False, assert_semantics=Mock())]
            for scenario in scenarios:
                with self.subTest(scenario=scenario):
                    owner = MagicMock()
                    live = owner.__enter__.return_value
                    def ctl(method, *, params, **_kwargs):
                        if method == "send_text":
                            self.assertFalse(params["text"].endswith("\n"),
                                             "Windows title command used LF instead of Enter")
                        else:
                            self.assertEqual((method, params), ("send_keys", {"keys": ["enter"]}))
                            raise Submitted()
                    live.ctl.side_effect = ctl
                    with (
                        patch.object(checker.platform, "system", return_value="Windows"),
                        patch.object(checker, "LiveKettle", return_value=owner),
                    ):
                        with self.assertRaises(Submitted):
                            scenario()

    def test_native_dock_targets_retained_candidate_and_refuses_ambiguity(self):
        spec = importlib.util.spec_from_file_location(
            "native_dock_test", Path(__file__).with_name("macos-dock.py"))
        native = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native)
        candidate = Path('/private/tmp/Candidate.app')

        class Fixture(native.Dock):
            def __init__(self, candidates):
                self.owned = []
                self.ax = Mock()
                self.ax.AXUIElementCreateApplication.return_value = 1
                self.cf = Mock()
                self.cf.CFBooleanGetValue.side_effect = bool
                self.actions = []
                self.data = {
                    1: {'AXChildren': [2]},
                    2: {'AXChildren': [3] + list(range(4, 4 + candidates))},
                    3: {'AXURL': Path('/Applications/kettle.app'), 'AXIsApplicationRunning': True},
                    10: {'AXRole': 'AXMenu', 'AXChildren': [11]},
                    11: {'AXTitle': 'New Window'},
                }
                for tile in range(4, 4 + candidates):
                    self.data[tile] = {'AXURL': candidate, 'AXIsApplicationRunning': True,
                                       'AXChildren': [10]}

            def attr(self, element, name):
                return self.data.get(element, {}).get(name)

            def children(self, element):
                return self.attr(element, 'AXChildren') or []

            def typed(self, value, kind):
                return isinstance(value, bool) if kind == 'Boolean' else False

            def text(self, value):
                return value

            def path(self, value):
                return value

            def action(self, element, name):
                self.actions.append((element, name))

        with patch.object(subprocess, 'check_output', return_value='123'), \
                patch.object(native.time, 'sleep'), \
                patch.object(native.os, 'getuid', return_value=501, create=True), \
                patch.object(native, 'same_bundle', side_effect=lambda left, right: left == right):
            for count in (2,):
                fixture = Fixture(count)
                with self.assertRaisesRegex(RuntimeError, 'ambiguous'):
                    fixture.command(candidate, 'New Window', Mock())
                self.assertEqual(fixture.actions, [])
            fixture = Fixture(1)
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                fixture.command(candidate, 'New Window', Mock(side_effect=RuntimeError('identity changed')))
            self.assertEqual(fixture.actions, [])
            fixture = Fixture(1)
            original_children = fixture.children
            reads = []
            def failing_children(element):
                if element == 4:
                    reads.append(element)
                    if len(reads) == 2:
                        raise RuntimeError('AX read failed')
                return original_children(element)
            fixture.children = failing_children
            with self.assertRaisesRegex(RuntimeError, 'AX read failed'):
                fixture.command(candidate, 'New Window', Mock())
            self.assertEqual(fixture.actions, [(4, 'AXShowMenu'), (10, 'AXCancel')])
            fixture = Fixture(0)
            self.assertEqual(fixture.command(candidate, 'New Window', Mock()), 'not-found')
            self.assertEqual(fixture.actions, [])
            fixture = Fixture(1)
            self.assertEqual(fixture.command(candidate, 'New Tab', Mock()), 'not-found')
            self.assertEqual(fixture.actions, [(4, 'AXShowMenu'), (10, 'AXCancel')])
            fixture = Fixture(1)
            validate = Mock(side_effect=[None, RuntimeError('identity changed')])
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                fixture.command(candidate, 'New Window', validate)
            self.assertEqual(fixture.actions, [(4, 'AXShowMenu'), (10, 'AXCancel')])
            fixture = Fixture(1)
            self.assertEqual(fixture.command(candidate, 'New Window', Mock()), 'clicked')
            self.assertEqual(fixture.actions, [(4, 'AXShowMenu'), (11, 'AXPress')])

    def test_dock_candidate_requires_exact_bundle_layout(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "Candidate.app/Contents/MacOS/kettle"
            binary.parent.mkdir(parents=True)
            binary.touch()
            self.assertEqual(checker.dock_candidate_bundle(str(binary)), binary.parents[2].resolve())
            self.assertIsNone(checker.dock_candidate_bundle(str(binary.parent / "worker")))
            self.assertIsNone(checker.dock_candidate_bundle("target/release/kettle"))

    def test_dock_bundle_allows_other_sessions_but_bare_binary_refuses(self):
        class Launched(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "Candidate.app/Contents/MacOS/kettle"
            binary.parent.mkdir(parents=True)
            binary.touch()
            with (
                patch.object(checker.platform, "system", return_value="Darwin"),
                patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "999\n", "")),
                patch.object(checker, "LiveKettle", side_effect=Launched()) as launch,
                patch.object(checker, "load_candidate_dock",
                             return_value=Mock(candidate_pids=Mock(return_value=[]))),
            ):
                with self.assertRaisesRegex(SystemExit, "another kettle"):
                    checker.run_dock_menu("target/release/kettle", Path(directory) / "bare")
                launch.assert_not_called()
                with self.assertRaises(Launched):
                    checker.run_dock_menu(str(binary), Path(directory) / "bundle")
                launch.assert_called_once()

    def test_dock_existing_candidate_refuses_before_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'Candidate.app/Contents/MacOS/kettle'
            binary.parent.mkdir(parents=True)
            binary.touch()
            with (
                patch.object(checker.platform, 'system', return_value='Darwin'),
                patch.object(subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '999', '')),
                patch.object(checker, 'load_candidate_dock',
                             return_value=Mock(candidate_pids=Mock(return_value=[999]))),
                patch.object(checker, 'LiveKettle') as launch,
            ):
                with self.assertRaisesRegex(SystemExit, 'already running'):
                    checker.run_dock_menu(str(binary), Path(directory) / 'out')
                launch.assert_not_called()

    def test_dock_identity_matches_native_paths_by_file_identity(self):
        native = checker.load_candidate_dock()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate = root / 'Candidate.app'
            candidate.mkdir()
            alias = root / 'alias.app'
            alias.symlink_to(candidate, target_is_directory=True)
            other = root / 'Other.app'
            other.mkdir()
            self.assertTrue(native.same_bundle(alias, candidate))
            self.assertFalse(native.same_bundle(other, candidate))
            self.assertFalse(native.same_bundle(None, candidate))
            result = subprocess.CompletedProcess([], 0, json.dumps([
                [999, str(other)], [123, str(alias)], [456, str(candidate)]]), '')
            with (
                patch.object(subprocess, 'run', return_value=result),
                patch.object(native.os, 'getuid', return_value=501, create=True),
            ):
                self.assertEqual(native.candidate_pids(candidate), [123, 456])

    def test_interaction_and_dock_pin_english_menu_labels(self):
        class Configured(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            def launch(_kettle, cfg, *_args, **_kwargs):
                self.assertIn("language = en", cfg.read_text().splitlines(),
                              "English menu assertions inherited the host language")
                raise Configured()
            for scenario in (checker.run_interaction, checker.run_dock_menu):
                with (
                    self.subTest(scenario=scenario.__name__),
                    patch.object(checker.platform, "system", return_value="Darwin"),
                    patch.dict(os.environ, {"LANG": "es_ES.UTF-8"}),
                    patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")),
                    patch.object(checker, "LiveKettle", side_effect=launch),
                ):
                    with self.assertRaises(Configured):
                        scenario("unused", Path(directory))

    def test_interaction_fixtures_select_posix_shell(self):
        class Launched(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            def launch(_kettle, _cfg, *_args, **kwargs):
                self.assertEqual(kwargs.get("extra_args"), ["-e", "bash", "--noprofile", "--norc"],
                                 "POSIX fixture inherited a possibly incompatible login shell")
                raise Launched()
            for scenario in (checker.run_interaction, checker.run_hover_wheel):
                with (
                    self.subTest(scenario=scenario.__name__),
                    patch.object(checker.platform, "system", return_value="Darwin"),
                    patch.dict(os.environ, {"SHELL": "/usr/local/bin/fish"}),
                    patch.object(checker, "LiveKettle", side_effect=launch),
                ):
                    with self.assertRaises(Launched):
                        scenario("unused", Path(directory))

    def test_theme_picker_pins_english_menu_labels(self):
        class Configured(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            def launch(_kettle, cfg, *_args, **_kwargs):
                self.assertIn("language = en", cfg.read_text().splitlines(),
                              "Theme menu assertion inherited the host language")
                raise Configured()
            with (
                patch.dict(os.environ, {"LANG": "es_ES.UTF-8"}),
                patch.object(checker, "LiveKettle", side_effect=launch),
            ):
                with self.assertRaises(Configured):
                    checker.run_theme_picker("unused", Path(directory))

    def test_toast_lifetime_includes_screenshot_time(self):
        import inspect
        function = ast.parse(inspect.getsource(checker._run_update_toast_fixture)).body[0]
        body = next(node.body for node in function.body if isinstance(node, ast.With))
        start = next(i for i, node in enumerate(body) if isinstance(node, ast.Assign)
                     and any(isinstance(target, ast.Name) and target.id == "toast" for target in node.targets))
        stop = next(i for i, node in enumerate(body) if isinstance(node, ast.Assert)
                    and "unexpected expiry" in ast.unparse(node))
        now = 10.0
        def until(_live, check, *_args):
            nonlocal now
            if check is checker.presented_update_toast:
                return {"message": "fixture"}
            now = 17.0
            return True
        def screenshot(_path):
            nonlocal now
            now += 3.0
        live = Mock()
        live.screenshot.side_effect = screenshot
        live.json_ctl.return_value = {"tab_bar": {"update_chip": None}}
        namespace = dict(checker.__dict__, live=live, until=until, before={},
                         terminal=lambda _: {}, out=Path("unused"),
                         time=Mock(monotonic=lambda: now))
        exec(compile(ast.Module(body=body[start:stop + 1], type_ignores=[]), "<toast-lifetime>", "exec"), namespace)
        self.assertEqual(namespace["elapsed"], 7.0)

    @unittest.skipUnless(sys.platform == "linux", "requires Linux process metadata")
    def test_linux_child_with_non_ascii_process_name(self):
        child = subprocess.Popen([
            sys.executable, "-c",
            "import ctypes,time; "
            "assert ctypes.CDLL(None).prctl(15,b'comm ) \\xff\\xc3\\xa9',0,0,0)==0; "
            "print('READY',flush=True); time.sleep(60)"], stdout=subprocess.PIPE)
        try:
            self.assertEqual(child.stdout.readline(), b"READY\n")
            handle = checker.StableProcessHandle.open(child.pid)
            try:
                self.assertTrue(handle.matches_current())
                self.assertEqual(checker._linux_process_parent(child.pid), os.getpid())
            finally:
                handle.close()
        finally:
            child.kill()
            child.wait(timeout=3)
            child.stdout.close()

    @unittest.skipUnless(sys.platform == "linux", "requires Linux orphan adoption")
    def test_pty_payload_completion_with_unreaped_background_child(self):
        live = checker.LiveKettle("unused", Path("unused"), Path("unused"))
        program = r"""
import ctypes,os,signal,subprocess,sys,time
assert ctypes.CDLL(None).prctl(36,1,0,0,0)==0
payload = '''
import os,sys,time
child=os.fork()
if child==0:
    time.sleep(0.15)
    os._exit(0)
with open(sys.argv[1],'w') as record: record.write(str(child))
raise SystemExit(7)
'''
wrapper=subprocess.Popen([sys.argv[1],sys.executable,'-c',payload,sys.argv[2]],start_new_session=True)
background=None
try:
    deadline=time.monotonic()+3
    while True:
        try:
            with open(sys.argv[2]) as record: value=record.read()
            if value:
                background=int(value)
                break
        except FileNotFoundError: pass
        assert time.monotonic()<deadline, 'payload did not publish its descendant'
        time.sleep(0.01)
    assert wrapper.wait(timeout=3)==7, 'wrapper lost the payload exit code'
    with open(f'/proc/{background}/stat','rb') as record:
        assert record.read().rsplit(b')',1)[1].split()[0]==b'Z', 'fixture must remain unreaped'
finally:
    if wrapper.poll() is None:
        wrapper.kill()
        wrapper.wait(timeout=3)
    if background is not None:
        try:
            if os.waitpid(background,os.WNOHANG)[0]==0:
                os.kill(background,signal.SIGKILL)
                os.waitpid(background,0)
        except ChildProcessError: pass
"""
        try:
            argv, env = live._prepare_unix_pty_tracker(["unused", "-e", "/bin/sh"])
            with tempfile.TemporaryDirectory() as directory:
                result = subprocess.run([
                    sys.executable, "-c", program, argv[argv.index("-e") + 1],
                    str(Path(directory) / "background.pid")], env=env,
                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
        finally:
            live._run_post_exit_cleanups()


if __name__ == "__main__":
    unittest.main()
