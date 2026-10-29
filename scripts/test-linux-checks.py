#!/usr/bin/env python3
"""Offline regression tests for Linux native-check failure paths."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('linux_audio', ROOT / 'check-linux-audio.py')
AUDIO = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIO)


class LinuxCheckTests(unittest.TestCase):
    def test_audio_timeout_keeps_log_and_stage(self):
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw) / 'out'
            process = Mock(pid=123, returncode=-9)
            process.communicate.side_effect = [subprocess.TimeoutExpired('test', 120, output=b'partial output'),
                                               (b'partial output', None)]
            process.poll.side_effect = [None, -9]
            argv = ['check', '--worker', raw, '--worker-tests-json', raw,
                    '--decoder-tests-json', raw, '--output-tests-json', raw, '--out-dir', str(out)]
            with patch.object(sys, 'argv', argv), patch.object(AUDIO, 'executable', return_value='test'), \
                    patch.object(AUDIO.signal, 'signal'), patch.object(AUDIO.Path, 'exists', return_value=True), \
                    patch.object(AUDIO.subprocess, 'Popen', return_value=process), patch.object(AUDIO.os, 'killpg'):
                with self.assertRaises(subprocess.TimeoutExpired):
                    AUDIO.main()
            self.assertTrue((out / 'decoder.log').is_file(), 'timed-out stage log missing')
            self.assertEqual((out / 'decoder.log').read_bytes(), b'partial output')
            self.assertEqual(json.loads((out / 'result.json').read_text())['stages'],
                             [{'name': 'decoder', 'exit': -9, 'pass': False, 'timeout': True}])

    def test_remote_query_failure_is_not_missing_tag(self):
        source = (ROOT / 'check-linux-installers.sh').read_text()
        block = source[source.index('repo=${KETTLE_GITHUB_REPO'):source.index('# A signed tag')]
        for status in (2, 128):
            with self.subTest(status=status):
                command = f'git() {{ echo remote-diagnostic >&2; return {status}; }}\nfail() {{ echo "$*" >&2; exit 1; }}\nversion=1\n' + block
                result = subprocess.run(['sh', '-ec', command], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0 if status == 2 else 1, result.stdout)
                if status == 128:
                    self.assertIn('remote-diagnostic', result.stderr)


if __name__ == '__main__':
    unittest.main()
