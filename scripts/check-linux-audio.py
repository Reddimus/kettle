#!/usr/bin/env python3
"""Run mandatory native decoder and PCM tests against a local audio service."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time


def executable(log, name):
    artifacts = [json.loads(line) for line in log.read_text().splitlines()]
    found = {item['executable'] for item in artifacts
             if item.get('reason') == 'compiler-artifact'
             and item.get('target', {}).get('name') == name and item.get('executable')}
    if len(found) != 1:
        raise RuntimeError(f'expected one compiled {name} test binary')
    return next(iter(found))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', required=True, type=Path)
    parser.add_argument('--worker-tests-json', required=True, type=Path)
    parser.add_argument('--decoder-tests-json', required=True, type=Path)
    parser.add_argument('--out-dir', required=True, type=Path)
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=False, mode=0o700)
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt('native audio check interrupted')

    signal.signal(signal.SIGTERM, interrupted)
    deadline = time.monotonic() + 240
    result = {'pass': False, 'stages': [], 'owned': []}
    record = args.out_dir / 'result.json'

    def save():
        record.write_text(json.dumps(result, indent=2) + '\n')

    env = dict(os.environ)
    fixture = Path(__file__).resolve().parents[1] / 'crates/kettle-media-native/tests/fixtures/video'
    env.update(KETTLE_TEST_PCM_WORKER=str(args.worker.resolve()),
               KETTLE_TEST_PCM_CLIP=str(fixture / 'ticks.mp4'),
               KETTLE_TEST_PCM_LONG_CLIP=str(fixture / 'long-audio.mp4'),
               KETTLE_TEST_VIDEO_FIXTURES=str(fixture))
    runtime = Path('/run/user') / str(os.geteuid())
    socket = runtime / 'pulse/native'
    service = None
    service_log = None
    try:
        if not socket.exists():
            if runtime.is_symlink() or runtime.stat().st_uid != os.geteuid():
                raise RuntimeError('audio fixture needs an owned /run/user/UID directory')
            (runtime / 'pulse').mkdir(mode=0o700, exist_ok=True)
            service_log = (args.out_dir / 'pulse.log').open('xb')
            # -n prevents default.pa from loading another server and sink.
            service = subprocess.Popen([
                'pulseaudio', '-n', '--fail=yes', '--daemonize=no', '--use-pid-file=no', '--exit-idle-time=-1',
                '--disable-shm=yes', '--file=/dev/null', '--log-target=stderr',
                '--load=module-null-sink sink_name=kettle_ci',
                f'--load=module-native-protocol-unix auth-anonymous=1 socket={socket}',
            ], stdin=subprocess.DEVNULL, stdout=service_log, stderr=subprocess.STDOUT,
                env=env, start_new_session=True)
            result['owned'].append({'kind': 'pulse', 'pid': service.pid, 'pgid': service.pid})
            save()
            until = min(deadline, time.monotonic() + 5)
            while not socket.exists():
                if service.poll() is not None or time.monotonic() >= until:
                    raise RuntimeError('owned audio service did not become ready')
                time.sleep(0.02)
            if service.poll() is not None:
                raise RuntimeError('owned audio service exited during startup')
        else:
            result['reused_audio_service'] = True
        stages = [
            ('decoder', executable(args.decoder_tests_json, 'ffmpeg'), [], 19),
            ('playback', executable(args.worker_tests_json, 'native_audio'), ['--ignored'], 4),
            ('broker', executable(args.worker_tests_json, 'process_boundary'),
             ['--ignored', '--exact', 'linux_pcm_only_broker_plays_pauses_and_closes'], 1),
        ]
        for name, binary, options, count in stages:
            if service is not None and service.poll() is not None:
                raise RuntimeError('owned audio service exited before native tests')
            command = [binary, *options, '--nocapture', '--test-threads=1']
            process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                       env=env, start_new_session=True)
            result['owned'].append({'kind': name, 'pid': process.pid, 'pgid': process.pid})
            save()
            try:
                output, _ = process.communicate(timeout=min(120, max(0, deadline - time.monotonic())))
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=10)
            (args.out_dir / f'{name}.log').write_bytes(output)
            text = output.decode(errors='replace')
            matched = re.search(r'test result: ok\. (\d+) passed; 0 failed; 0 ignored;', text)
            passed = process.returncode == 0 and matched and int(matched[1]) == count and 'skip' not in text.lower()
            result['stages'].append({'name': name, 'exit': process.returncode, 'pass': bool(passed)})
            save()
            print(f'{name}: {"PASS" if passed else "FAIL"}', flush=True)
            if not passed:
                raise RuntimeError(f'{name} did not run all {count} mandatory native tests')
        result['pass'] = True
    finally:
        if service is not None:
            if service.poll() is None:
                service.terminate()
                try:
                    service.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.wait(timeout=5)
            result['audio_service_exit'] = service.returncode
        if service_log is not None:
            service_log.close()
        save()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
