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
