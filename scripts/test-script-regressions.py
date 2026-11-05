#!/usr/bin/env python3
"""Portable regressions for smoke and benchmark harnesses."""

import ast
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

SCRIPTS = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), SCRIPTS / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ScriptRegressionTests(unittest.TestCase):
    def test_windows_job_wait_uses_active_process_accounting(self):
        live = load('check-live-ui-smoke')
        job = object.__new__(live.WindowsKillJob)
        job._job = 1
        job._kernel32 = Mock()
        job._kernel32.WaitForSingleObject.return_value = 0x102
        job.active_processes = Mock(side_effect=[1, 0])
        with patch.object(live.time, 'sleep'):
            job.wait_empty(1)
        self.assertEqual(job.active_processes.call_count, 2)
        job.active_processes = Mock(return_value=1)
        with self.assertRaisesRegex(RuntimeError, 'still contains 1'):
            job.wait_empty(0)

    def test_image_paste_relative_output_passes_absolute_fixture_paths(self):
        check = load('check-image-paste-parity')
        class StopFixture(Exception):
            pass
        helpers = Mock()
        helpers.LiveKettle.side_effect = StopFixture
        with tempfile.TemporaryDirectory(dir=SCRIPTS.parent) as raw:
            relative = Path(raw).relative_to(Path.cwd()) / 'output'
            with patch.object(sys, 'argv', ['check', '--kettle', 'kettle', '--out-dir', str(relative)]), \
                 patch.object(check.platform, 'system', return_value='Darwin'), \
                 patch.object(check.importlib.util, 'spec_from_file_location', return_value=Mock()), \
                 patch.object(check.importlib.util, 'module_from_spec', return_value=helpers):
                with self.assertRaises(StopFixture):
                    check.main()
            args = helpers.LiveKettle.call_args
            self.assertTrue(Path(args.args[1]).is_absolute(), 'relative config path')
            for value in args.kwargs['extra_args'][2:]:
                self.assertTrue(Path(value).is_absolute(), 'relative pane fixture path')

    def test_cli_uses_cargo_reported_executable(self):
        cli = load('check-cli-smoke')
        executable = SCRIPTS / 'configured-target' / 'host-triple' / 'debug' / 'kettle'
        message = json.dumps({'reason': 'compiler-artifact',
                              'target': {'name': 'kettle', 'kind': ['bin']},
                              'executable': str(executable)})
        class StopFixture(Exception):
            pass
        with patch.object(cli, 'expected_git_identity', return_value='identity'), \
             patch.object(cli.subprocess, 'run', return_value=Mock(stdout=message)) as build, \
             patch.object(cli, 'private_scratch', side_effect=StopFixture):
            with self.assertRaises(StopFixture):
                cli.main()
        self.assertEqual(cli.EXE, executable)
        self.assertIn('--message-format=json-render-diagnostics', build.call_args.args[0])

    def test_search_result_waits_for_highlight_geometry_after_navigation(self):
        live = load('check-live-ui-smoke')
        pending = {'has_match': True, 'status': 'Match', 'match_rects': []}
        ready = {**pending, 'match_rects': [{'pane': 1, 'rect': {'x': 8, 'y': 16, 'width': 40, 'height': 16}}]}
        fixture = Mock()
        fixture.json_ctl.side_effect = [
            {'search': pending}, {'text': 'wanted'},
            {'search': ready}, {'text': 'wanted'},
        ]
        with patch.object(live, 'screen_text', return_value='wanted'), \
             patch.object(live.time, 'sleep'):
            geometry, _ = live.wait_for_search_result(fixture, 'wanted')
        self.assertEqual(geometry['search'], ready)
        self.assertEqual(fixture.json_ctl.call_count, 4)

    def test_search_result_without_highlight_geometry_still_times_out(self):
        live = load('check-live-ui-smoke')
        fixture = Mock()
        fixture.json_ctl.side_effect = [
            {'search': {'has_match': True, 'status': 'Match', 'match_rects': []}},
            {'text': 'wanted'},
        ]
        with patch.object(live, 'screen_text', return_value='wanted'), \
             patch.object(live.time, 'sleep'), \
             patch.object(live.time, 'monotonic', side_effect=[0, 0, 13]):
            with self.assertRaisesRegex(SystemExit, 'timed out'):
                live.wait_for_search_result(fixture, 'wanted')

    def correlation_refusal(self):
        return subprocess.CompletedProcess([], 1, '',
            'kettle ctl: server error [bad_params]: pane cannot correlate this command; '
            'use a supported bundled shell integration '
            '(Bash multiline input also requires bracketed paste)\n')

    def test_bash_multiline_admission_waits_through_readline_paste_mode_transition(self):
        check = load('check-command-correlation')
        fixture = Mock()
        value = {'exit_code': 0, 'timed_out': False, 'output': 'only target'}
        fixture.ctl.side_effect = [
            subprocess.CompletedProcess([], 1, '', 'server error [busy]'),
            self.correlation_refusal(),
            subprocess.CompletedProcess([], 0, json.dumps(value), ''),
        ]
        records = []
        with patch.object(check.time, 'sleep'):
            actual, attempts = check.run_correlated_command(
                fixture, 'bash', 'first\nsecond', records=records)
        self.assertEqual(actual, value)
        self.assertEqual(attempts, 3)
        self.assertEqual([r.get('refused_before_input', False) for r in records], [True, True, False])
        self.assertTrue(all(c.kwargs['params']['require_correlation'] for c in fixture.ctl.call_args_list))

    def test_unrelated_correlation_refusals_are_never_retried(self):
        check = load('check-command-correlation')
        for label, command, error in [
            ('fish', 'first\nsecond', self.correlation_refusal()),
            ('zsh', 'first\nsecond', self.correlation_refusal()),
            ('bash32', 'first\nsecond', self.correlation_refusal()),
            ('bash', 'single line', self.correlation_refusal()),
            ('bash', 'first\nsecond', subprocess.CompletedProcess([], 1, '', 'server error [bad_params]: invalid argument')),
        ]:
            with self.subTest(label=label, command=command):
                fixture = Mock()
                fixture.ctl.return_value = error
                with self.assertRaises(RuntimeError), patch.object(check.time, 'sleep') as sleep:
                    check.run_correlated_command(fixture, label, command)
                self.assertEqual(fixture.ctl.call_count, 1)
                sleep.assert_not_called()

    def test_permanently_disabled_paste_mode_still_fails_the_bounded_probe(self):
        check = load('check-command-correlation')
        fixture = Mock()
        fixture.ctl.return_value = self.correlation_refusal()
        with patch.object(check.time, 'monotonic', side_effect=[0, 0, 6]), \
             patch.object(check.time, 'sleep'), \
             self.assertRaisesRegex(RuntimeError, 'never admitted'):
            check.run_correlated_command(fixture, 'bash', 'first\nsecond')
        self.assertEqual(fixture.ctl.call_count, 2)

    def test_an_executed_command_failure_is_returned_without_retry(self):
        check = load('check-command-correlation')
        fixture = Mock()
        value = {'exit_code': 1, 'timed_out': True, 'output': 'partial'}
        fixture.ctl.return_value = subprocess.CompletedProcess([], 0, json.dumps(value), '')
        actual, attempts = check.run_correlated_command(fixture, 'bash', 'first\nsecond')
        self.assertEqual(actual, value)
        self.assertEqual(attempts, 1)
        self.assertEqual(fixture.ctl.call_count, 1)

    def test_modal_startup_failure_preserves_clipboard(self):
        modal = load('check-modal-shortcuts')
        with tempfile.TemporaryDirectory() as raw, \
             patch.object(modal, 'RecordedLive', side_effect=RuntimeError('startup')), \
             patch.object(subprocess, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'startup'):
                modal.check('kettle', Path(raw) / 'out')
        run.assert_not_called()

    def test_modal_probe_restores_snapshot_on_failure_and_declines_unsafe_snapshot(self):
        modal = load('check-modal-shortcuts')
        snapshot = {'ok': True, 'count': 42, 'items': [
            [['public.utf8-plain-text', 'aGk='], ['public.rtf', 'AP8=']],
            [['public.png', 'AQI=']]]}
        with patch.object(modal, 'pasteboard', side_effect=[snapshot, {'ok': True}]) as board:
            with self.assertRaisesRegex(RuntimeError, 'probe'):
                with modal.clipboard_probe('hint') as enabled:
                    self.assertTrue(enabled)
                    raise RuntimeError('probe')
        self.assertEqual(board.call_args.args[0],
                         {'op': 'restore', 'snapshot': snapshot, 'text': 'hint'})
        with patch.object(modal, 'pasteboard', return_value={'ok': False, 'reason': 'unreadable type'}) as board, \
             contextlib.redirect_stdout(io.StringIO()) as output:
            with modal.clipboard_probe('hint') as enabled:
                self.assertFalse(enabled)
        self.assertEqual(board.call_count, 1)
        self.assertIn('clipboard probe skipped: unreadable type', output.getvalue())

    @unittest.skipUnless(sys.platform == 'darwin', 'JXA pasteboard requires macOS')
    def test_modal_native_pasteboard_restores_all_types_and_skips_foreign_changes(self):
        modal = load('check-modal-shortcuts')
        name = f'kettle-test-{os.getpid()}'
        available = modal.pasteboard({'op': 'snapshot', 'name': name})
        if not available['ok'] and available['reason'] == 'pasteboard service unavailable':
            self.skipTest(available['reason'])
        def write(text, probe=False):
            # The user's contents: two items, one with a binary type. The
            # probe's copy, like Kettle's, is a single text item.
            extra = '' if probe else '''item.setDataForType($.NSData.alloc.initWithBase64EncodedStringOptions('AP8=', 0), 'test.binary');
const second = $.NSPasteboardItem.alloc.init;
second.setStringForType('second item', 'public.utf8-plain-text');
'''
            source = '''ObjC.import('AppKit');
const board = $.NSPasteboard.pasteboardWithName(%s);
board.clearContents;
const item = $.NSPasteboardItem.alloc.init;
item.setStringForType(%s, 'public.utf8-plain-text');
const items = $.NSMutableArray.alloc.init;
items.addObject(item);
''' + extra + ('' if probe else 'items.addObject(second);\n') + 'board.writeObjects(items);\n'
            subprocess.run(['osascript', '-l', 'JavaScript', '-e', source % (json.dumps(name), json.dumps(text))],
                           capture_output=True, check=True, timeout=10)
        def request(op, **values):
            return modal.pasteboard(dict(op=op, name=name, **values))
        write('original')
        snapshot = request('snapshot')
        self.assertTrue(snapshot['ok'], snapshot)
        write('hint', probe=True)
        restored = request('restore', snapshot=snapshot, text='hint')
        self.assertTrue(restored['ok'], restored)
        self.assertEqual(request('snapshot')['items'], snapshot['items'])
        snapshot = request('snapshot')
        write('foreign')
        self.assertFalse(request('restore', snapshot=snapshot, text='hint')['ok'])
        self.assertEqual(dict(request('snapshot')['items'][0])['public.utf8-plain-text'], 'Zm9yZWlnbg==')

    def assert_english_config(self, name):
        tree = ast.parse((SCRIPTS / (name + '.py')).read_text())
        configs = [ast.literal_eval(node.args[0]) for node in ast.walk(tree)
                   if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                   and node.func.attr == 'write_text' and isinstance(node.func.value, ast.Name)
                   and node.func.value.id == 'config']
        self.assertTrue(configs)
        for config in configs:
            values = dict(line.split(' = ', 1) for line in config.splitlines() if ' = ' in line)
            self.assertEqual(values.get('language'), 'en', name)

    def test_card_and_confirmation_config_language(self):
        for name in ('check-agent-cards-smoke', 'check-confirmation-smoke'):
            with self.subTest(name=name):
                self.assert_english_config(name)

    def test_image_paste_config_language(self):
        self.assert_english_config('check-image-paste-parity')

    def test_lane_startup_failure_reaps_child_and_closes_log(self):
        media = load('check-lane-media-smoke')
        for failed_probe in (1, 2):
            with self.subTest(probe=failed_probe), tempfile.TemporaryDirectory() as raw:
                process = Mock(pid=123)
                log = io.BytesIO()
                with patch.object(media.subprocess, 'Popen', return_value=process), \
                     patch.object(media, 'open', return_value=log, create=True), \
                     patch.object(media, 'wait_for', side_effect=([RuntimeError('startup')] if failed_probe == 1
                                                               else [True, RuntimeError('startup')])):
                    with self.assertRaisesRegex(RuntimeError, 'startup'):
                        media.launch(Path(raw), 'en')
                process.terminate.assert_called_once()
                process.wait.assert_called()
                self.assertTrue(log.closed)

    def test_lane_show_quotes_staged_paths(self):
        media = load('check-lane-media-smoke')
        media.KETTLE = "/tmp/space and quote/kettle"
        path = Path("/tmp/media dir/$(touch nope).png")
        calls = []
        with patch.object(media, 'screen_text', return_value=''), \
             patch.object(media, 'ctl', side_effect=lambda *args: calls.append(args)), \
             patch.object(media, 'wait_for'):
            media.show(123, path)
        command = calls[0][-1]
        self.assertTrue(command.endswith('\n'))
        self.assertEqual(shlex.split(command), [media.KETTLE, 'show', str(path)])

    def test_mermaid_keeps_failure_before_launch_skip(self):
        mermaid = load('check-mermaid')
        with tempfile.TemporaryDirectory() as raw:
            sources = [Path(raw) / name for name in ('broken.md', 'later.md')]
            for source in sources:
                source.write_text('```mermaid\ninvalid\n```\n')
            output = io.StringIO()
            with patch.object(sys, 'argv', ['check-mermaid.py']), \
                 patch.dict(os.environ, {'KETTLE_MERMAID_REQUIRED': '0'}), \
                 patch.object(mermaid, 'tracked_markdown', return_value=sources), \
                 patch.object(mermaid.shutil, 'which', return_value='npx'), \
                 patch.object(mermaid, 'find_chrome', return_value='chrome'), \
                 patch.object(mermaid.time, 'sleep'), \
                 patch.object(mermaid.subprocess, 'run', side_effect=[
                     subprocess.CompletedProcess([], 1, '', 'diagram parser error'),
                     *[subprocess.CompletedProcess([], 1, '', 'Failed to launch ChromeLauncher')] * 3]), \
                 contextlib.redirect_stderr(output), contextlib.redirect_stdout(io.StringIO()):
                code = mermaid.main()
            self.assertEqual(code, 1, 'known diagram failure was skipped')
            self.assertIn('diagram parser error', output.getvalue())

    def test_username_completion_prefix_is_native_singleton(self):
        shell = load('check-shell-integration')
        def complete(args, **kwargs):
            line = args[-1]
            rows = '~kevin\tUser\n~keith\tUser\n' if line == 'echo ~ke' else (
                '~kevin\tUser\n' if line == 'echo ~kev' else '')
            return subprocess.CompletedProcess(args, 0, rows, '')
        with patch.object(shell.subprocess, 'run', side_effect=complete):
            self.assertEqual(shell.username_completion_prefix('fish', 'kevin'), 'kev')
        with patch.object(shell.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')):
            self.assertIsNone(shell.username_completion_prefix('fish', 'kevin'))

    def test_churn_uses_compatible_shell_and_requires_acknowledgement(self):
        split = load('check-split-repro')
        with patch.object(split.subprocess, 'run') as run, \
             patch.object(split.time, 'sleep'), \
             patch.object(split, 'ctl', return_value={'text': 'KETTLE_CHURN_READY'}):
            split.start_workload('kettle', 123, False)
        sent = json.loads(run.call_args.args[0][-1])['text']
        self.assertEqual(shlex.split(sent)[0:2], ['/bin/sh', '-c'])
        with patch.object(split.subprocess, 'run'), \
             patch.object(split.time, 'sleep'), \
             patch.object(split.time, 'monotonic', side_effect=[0, 1, 30]), \
             patch.object(split, 'ctl', return_value={'text': 'echo KETTLE_CHURN_READY'}):
            with self.assertRaisesRegex(RuntimeError, 'churn.*start'):
                split.start_workload('kettle', 123, False)

    def test_flood_timeout_and_echoed_marker_fail_with_cleanup(self):
        stress = load('check-stress-live')
        for screen in ('unfinished flood', 'head -c 50000000; echo DONE-S1'):
            with self.subTest(screen=screen):
                live = Mock()
                live.json_ctl.return_value = {'text': screen}
                with patch.object(stress, 'probe_during', return_value=0.1), \
                     patch.object(stress.time, 'sleep'), \
                     patch.object(stress.time, 'monotonic', side_effect=[0, 1, 121]):
                    with self.assertRaisesRegex(RuntimeError, 'S1 flood did not complete'):
                        stress.s1_urandom(live)
                live.ctl.assert_any_call('send_keys', params={'keys': ['ctrl+c']})

    def run_pane_drag_block(self, marker, files):
        script = (SCRIPTS / 'check-pane-drag-smoke.sh').read_text()
        blocks = [part.split('\nPY\n', 1)[0]
                  for part in script.split("<<'PY'\n")[1:]]
        block, = [code for code in blocks if marker in code]
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for name, value in files.items():
                (root / name).write_text(json.dumps(value))
            result = subprocess.run([sys.executable, '-', raw, '3'], input=block,
                                    capture_output=True, text=True, timeout=10)
            outputs = {p.name: json.loads(p.read_text()) for p in root.glob('*.json')}
        return result, outputs

    def pane_drag_fixture(self):
        return [
            {'id': 1, 'rect': {'x': 0, 'y': 0, 'width': 101, 'height': 50}},
            {'id': 3, 'rect': {'x': 0, 'y': 50, 'width': 101, 'height': 50}},
            {'id': 2, 'rect': {'x': 101, 'y': 0, 'width': 100, 'height': 100}},
        ]

    def test_pane_drag_targets_right_column_when_left_is_wider(self):
        result, outputs = self.run_pane_drag_block(
            '(out / "target.json").write_text', {'panes.json': self.pane_drag_fixture()})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(outputs['target.json']['id'], 2)
        point = json.loads(result.stdout)
        self.assertGreater(point['x'], 101)
        self.assertLess(point['x'], 201)

    def test_pane_drag_rejects_reordered_ids_without_the_right_edge_layout(self):
        panes = self.pane_drag_fixture()
        files = {
            'panes.json': panes,
            'target.json': panes[2],
            'geometry-pressed.json': {'pane_drag_armed': True, 'pane_drag_pane': 3},
            'geometry-jittered.json': {'pane_drag_armed': True},
            'geometry-dragging.json': {
                'pane_drag_live': True, 'pane_drag_target': {'pane': 2, 'edge': 'right'}},
            'geometry-released.json': {'panes': [panes[0], panes[2], panes[1]]},
        }
        valid = [
            {'id': 1, 'rect': {'x': 0, 'y': 0, 'width': 101, 'height': 100}},
            {'id': 2, 'rect': {'x': 101, 'y': 0, 'width': 50, 'height': 100}},
            {'id': 3, 'rect': {'x': 151, 'y': 0, 'width': 50, 'height': 100}},
        ]
        result, _ = self.run_pane_drag_block('before = [p[', {
            **files, 'geometry-released.json': {'panes': valid}})
        self.assertEqual(result.returncode, 0, result.stderr)
        result, _ = self.run_pane_drag_block('before = [p[', files)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('right-edge layout', result.stderr)

    def test_control_storm_accepts_darwin_closed_peer_error_only_on_darwin(self):
        stress = load('check-stress-live')
        error = 'control I/O error: Socket is not connected (os error 57)'

        class ClosedPeer:
            def json_ctl(self, _method):
                raise SystemExit(error)

        for host in ('darwin', 'linux', 'win32'):
            with self.subTest(host=host), patch.object(stress.sys, 'platform', host):
                row = stress.s10_ctl_storm(ClosedPeer())
            self.assertEqual(row['ctl_ok'], 0)
            self.assertEqual(row['ctl_refused_at_cap'], 1280 if host == 'darwin' else 0)
            self.assertEqual(row['ctl_errors'], 0 if host == 'darwin' else 1280)
        error = 'control I/O error: another transport failure'
        with patch.object(stress.sys, 'platform', 'darwin'):
            self.assertEqual(stress.s10_ctl_storm(ClosedPeer())['ctl_errors'], 1280)

    def test_video_receipt_paste_accepts_wrapping_and_requires_both_files(self):
        live = load('check-live-ui-smoke')

        class Captured(Exception):
            pass

        for screen, complete in (('first-vi\ndeo.mp4 second-video.we\nbm', True),
                                 ('first-video.mp4', False)):
            with self.subTest(complete=complete), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                videos = [root / 'one.mp4', root / 'two.webm']
                for path in videos:
                    path.write_bytes(b'fixture')
                fixture = Mock()
                fixture.json_ctl.side_effect = lambda method, *_args, **_kwargs: (
                    {'text': screen} if method == 'read_screen' else {'window_focused': True})
                receipt = {'count': 2, 'openable': False, 'rect': {'x': 0, 'y': 0, 'width': 32, 'height': 32},
                           'preview_rect': {'x': 0, 'y': 0, 'width': 16, 'height': 16}}
                with patch.object(live.platform, 'system', return_value='Linux'), \
                     patch.dict(os.environ, {'KETTLE_SMOKE_VIDEO_FIXTURES': os.pathsep.join(map(str, videos))}), \
                     patch.object(live, 'LiveKettle', return_value=contextlib.nullcontext(fixture)), \
                     patch.object(live, 'set_file_list_clipboard', return_value=None), \
                     patch.object(live, 'wait_for_media_worker'), \
                     patch.object(live, 'wait_for_media_receipt', return_value=({}, receipt)), \
                     patch.object(live, 'capture_receipt_lane', side_effect=Captured):
                    if complete:
                        with self.assertRaises(Captured):
                            live.run_video_paste_receipt('kettle', root, isolated_clipboard=True)
                    else:
                        with self.assertRaisesRegex(SystemExit, 'copied file list'):
                            live.run_video_paste_receipt('kettle', root, isolated_clipboard=True)

    def test_raw_paste_startup_failure_does_not_touch_the_clipboard(self):
        live = load('check-live-ui-smoke')
        with tempfile.TemporaryDirectory() as raw, \
             patch.object(live.platform, 'system', return_value='Darwin'), \
             patch.object(live, 'LiveKettle', side_effect=RuntimeError('startup')), \
             patch.object(live.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'')) as run:
            with self.assertRaisesRegex(RuntimeError, 'startup'):
                live.run_raw_paste('kettle', Path(raw))
        run.assert_not_called()

    def test_raw_paste_requires_an_isolated_linux_clipboard_before_launch(self):
        live = load('check-live-ui-smoke')
        fixture = Mock()
        fixture.json_ctl.side_effect = lambda method, *_args, **_kwargs: (
            {'text': 'KETTLE_RAW_PASTE_ONE\nKETTLE_RAW_PASTE_TWO'}
            if method == 'read_screen' else {})
        with tempfile.TemporaryDirectory() as raw, \
             patch.object(live.platform, 'system', return_value='Linux'), \
             patch.object(live, 'LiveKettle', return_value=contextlib.nullcontext(fixture)) as start, \
             patch.object(live, 'set_text_clipboard', return_value=None) as write, \
             patch.object(live.time, 'sleep'):
            with self.assertRaisesRegex(SystemExit, 'isolated-clipboard'):
                live.run_raw_paste('kettle', Path(raw))
            start.assert_not_called()
            write.assert_not_called()
            live.run_raw_paste('kettle', Path(raw), isolated_clipboard=True)
            write.assert_called_once()

    def test_raw_paste_requires_execution_output_instead_of_command_echo(self):
        live = load('check-live-ui-smoke')
        markers = ('KETTLE_RAW_PASTE_ONE', 'KETTLE_RAW_PASTE_TWO')
        executed = '\n'.join(markers)
        echoed = '\n'.join(f"printf '\\n%s\\n' {marker}" for marker in markers) * 2
        for screen, ran in ((executed, True), (echoed, False)):
            with self.subTest(executed=ran), tempfile.TemporaryDirectory() as raw:
                fixture = Mock()
                fixture.json_ctl.side_effect = lambda method, *_args, **_kwargs: (
                    {'text': screen} if method == 'read_screen' else {})
                with patch.object(live.platform, 'system', return_value='Linux'), \
                     patch.object(live, 'LiveKettle', return_value=contextlib.nullcontext(fixture)), \
                     patch.object(live, 'set_text_clipboard', return_value=None), \
                     patch.object(live.time, 'sleep'), \
                     patch.object(live.time, 'monotonic', side_effect=[0, 11]):
                    if ran:
                        live.run_raw_paste('kettle', Path(raw), isolated_clipboard=True)
                    else:
                        with self.assertRaisesRegex(SystemExit, 'pasted lines did not run'):
                            live.run_raw_paste('kettle', Path(raw), isolated_clipboard=True)

    def test_raw_paste_declines_an_uncapturable_mac_clipboard(self):
        live = load('check-live-ui-smoke')
        fixture = Mock()
        fixture.json_ctl.side_effect = lambda method, *_args, **_kwargs: (
            {'text': 'KETTLE_RAW_PASTE_ONE\nKETTLE_RAW_PASTE_TWO'}
            if method == 'read_screen' else {})
        tools = Mock()
        tools.clipboard_probe.return_value = contextlib.nullcontext(False)
        with tempfile.TemporaryDirectory() as raw, \
             patch.object(live.platform, 'system', return_value='Darwin'), \
             patch.object(live, 'LiveKettle', return_value=contextlib.nullcontext(fixture)), \
             patch.object(live, 'clipboard_tools', return_value=tools, create=True), \
             patch.object(live, 'set_text_clipboard', return_value=None) as write, \
             patch.object(live.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'')), \
             patch.object(live.time, 'sleep'):
            with self.assertRaisesRegex(SystemExit, 'clipboard snapshot'):
                live.run_raw_paste('kettle', Path(raw))
            write.assert_not_called()

    def test_text_clipboard_failed_write_reaps_its_owned_provider(self):
        live = load('check-live-ui-smoke')
        owner = Mock()
        owner.stdin.write.side_effect = BrokenPipeError('provider exited')
        owner.poll.return_value = None
        with patch.object(live.platform, 'system', return_value='Linux'), \
             patch.dict(os.environ, {'WAYLAND_DISPLAY': 'private-fixture'}), \
             patch.object(live.shutil, 'which', return_value='/fixture/wl-copy'), \
             patch.object(live.subprocess, 'Popen', return_value=owner), \
             patch.object(live.time, 'sleep'):
            with self.assertRaises(BrokenPipeError):
                live.set_text_clipboard('fixture')
        owner.terminate.assert_called_once()
        owner.wait.assert_called_once_with(timeout=3)
        owner.stdin.close.assert_called_once()

    def test_clipboard_replacing_cases_require_isolation_before_any_side_effect(self):
        live = load('check-live-ui-smoke')
        with tempfile.TemporaryDirectory() as source:
            videos = [Path(source) / 'one.mp4', Path(source) / 'two.webm']
            for path in videos:
                path.write_bytes(b'fixture')
            for host in ('Darwin', 'Linux'):
                for name in ('interaction', 'search_selection', 'image_paste_receipt', 'video_paste_receipt'):
                    with self.subTest(host=host, case=name), tempfile.TemporaryDirectory() as raw, \
                         patch.object(live.platform, 'system', return_value=host), \
                         patch.dict(os.environ, {'KETTLE_SMOKE_VIDEO_FIXTURES': os.pathsep.join(map(str, videos))}), \
                         patch.object(live, 'LiveKettle', side_effect=SystemExit('unsafe launch attempted')) as start, \
                         patch.object(live, 'set_bitmap_clipboard') as bitmap, \
                         patch.object(live, 'set_file_list_clipboard') as files:
                        with self.assertRaisesRegex(SystemExit, 'isolated-clipboard'):
                            getattr(live, 'run_' + name)('kettle', Path(raw))
                        self.assertEqual(list(Path(raw).iterdir()), [])
                        start.assert_not_called()
                        bitmap.assert_not_called()
                        files.assert_not_called()

    def test_selection_autoscroll_fixture_does_not_auto_copy_to_the_desktop(self):
        live = load('check-live-ui-smoke')

        class Configured(Exception):
            pass

        def launch(_kettle, config, *_args, **_kwargs):
            self.assertIn('copy-on-select = false', config.read_text().splitlines())
            raise Configured()

        with tempfile.TemporaryDirectory() as raw, \
             patch.object(live.platform, 'system', return_value='Linux'), \
             patch.object(live, 'LiveKettle', side_effect=launch):
            with self.assertRaises(Configured):
                live.run_selection_autoscroll('kettle', Path(raw))

    def test_tearoff_failure_releases_button(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            tools = root / 'bin'
            tools.mkdir()
            fake = r'''import json, os, signal, sys
from pathlib import Path
name = Path(sys.argv[0]).name
args = sys.argv[1:]
root = Path(os.environ['TEAR_FIXTURE'])
if name == 'xdotool':
    with (root / 'calls').open('a') as log:
        log.write(' '.join(args) + '\n')
    if args[0] == 'search': print('123')
elif name == 'xwininfo':
    print('Absolute upper-left X: 0\nAbsolute upper-left Y: 0')
elif name == 'sleep':
    if args == ['0.15']: sys.exit(9)
elif 'events' in args:
    signal.pause()
elif 'ui_geometry' in args:
    print(json.dumps({'tab_bar': {'height': 20, 'segments': [
        {'rect': {'x': 0, 'width': 100}}, {'rect': {'x': 100, 'width': 100}}]}}))
elif 'list_tabs' in args:
    print(json.dumps({'tabs': [{'window': 1}, {'window': 1}]}))
'''
            for name in ('kettle', 'xdotool', 'xwininfo', 'sleep'):
                path = tools / name
                path.write_text('#!' + sys.executable + '\n' + fake)
                path.chmod(0o755)
            env = dict(os.environ, DISPLAY=':fixture', WAYLAND_DISPLAY='',
                       TEAR_FIXTURE=str(root), KETTLE_BIN=str(tools / 'kettle'),
                       KETTLE_DIAG_DIR=str(root / 'out'), PATH=str(tools) + os.pathsep + os.environ['PATH'])
            result = subprocess.run(['bash', str(SCRIPTS / 'check-tearoff-live-smoke.sh')],
                                    env=env, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 9, result.stderr)
            calls = (root / 'calls').read_text().splitlines()
            self.assertIn('mousedown 1', calls)
            self.assertIn('mouseup 1', calls, 'failure left the synthetic button held')

    def test_online_installer_skips_darwin_before_probing_tools(self):
        online = load('test-install-online')
        with patch.object(online.platform, 'system', return_value='Darwin'), \
             patch.object(online.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, 'GNU tar', '')) as run:
            with self.assertRaisesRegex(unittest.SkipTest, 'Linux'):
                online.OnlineInstallerTests.setUpClass()
            run.assert_not_called()

    def test_benchmark_uses_private_cleaned_screenshots(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / 'scripts').mkdir()
            shutil.copyfile(SCRIPTS / 'bench.sh', root / 'scripts/bench.sh')
            binary = root / 'target/release/kettle'
            binary.parent.mkdir(parents=True)
            binary.write_text('#!' + sys.executable + '\nimport json, os, sys\n'
                              'with open(os.environ["BENCH_CALLS"], "a") as log:\n'
                              '    log.write(json.dumps(sys.argv[1:]) + "\\n")\n')
            binary.chmod(0o755)
            tools = root / 'bin'
            tools.mkdir()
            timer = tools / 'gtime'
            timer.write_text('#!/bin/sh\nshift 2\nout=$2\nshift 2\n"$@"\nprintf "0.1 100\\n" > "$out"\n')
            timer.chmod(0o755)
            calls = root / 'calls'
            result = subprocess.run(['bash', str(root / 'scripts/bench.sh')],
                                    env=dict(os.environ, BENCH_CALLS=str(calls),
                                             PATH=str(tools) + os.pathsep + os.environ['PATH']),
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            captures = [Path(args[1]) for args in map(json.loads, calls.read_text().splitlines())
                        if args[0] in ('--screenshot', '--screenshot-menu')]
            self.assertEqual(len(captures), 10)
            self.assertTrue(all(p.parent.name.startswith('kettle-bench.') for p in captures), captures)
            self.assertEqual(len({p.parent for p in captures}), 1)
            self.assertFalse(captures[0].parent.exists(), 'private directory survived exit')


if __name__ == '__main__':
    unittest.main()
