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
    parser.add_argument('--output-tests-json', required=True, type=Path)
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
               KETTLE_TEST_PCM_DELAYED_CLIP=str(fixture / 'delayed-audio.mkv'),
               KETTLE_TEST_VIDEO_FIXTURES=str(fixture))
    runtime = Path('/run/user') / str(os.geteuid())
    socket = runtime / 'pulse/native'
    service = None
    service_log = None
    def start_service(stalled):
        nonlocal service, service_log
        if runtime.is_symlink() or runtime.stat().st_uid != os.geteuid():
            raise RuntimeError('audio fixture needs an owned /run/user/UID directory')
        (runtime / 'pulse').mkdir(mode=0o700, exist_ok=True)
        service_log = (args.out_dir / ('pulse-stalled.log' if stalled else 'pulse.log')).open('xb')
        # -n prevents default.pa from loading another server and sink.
        service = subprocess.Popen([
            'pulseaudio', '-n', '--fail=yes', '--daemonize=no', '--use-pid-file=no', '--exit-idle-time=-1',
            '--disable-shm=yes', '--file=/dev/null', '--log-target=stderr', '--log-level=debug',
            '--load=module-null-sink sink_name=kettle_ci' + ('' if stalled else ' norewinds=1'),
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

    def stop_service():
        nonlocal service, service_log
        if service is not None:
            if service.poll() is None:
                service.terminate()
                try:
                    service.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.wait(timeout=5)
            result.setdefault('audio_service_exits', []).append({'pid': service.pid, 'exit': service.returncode})
            service = None
        if service_log is not None:
            service_log.close()
            service_log = None
        save()

    def stage(name, binary, options, count, environment=env):
        if service is not None and service.poll() is not None:
            raise RuntimeError('owned audio service exited before native tests')
        command = [binary, *options, '--nocapture', '--test-threads=1']
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   env=environment, start_new_session=True)
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

    try:
        native = executable(args.worker_tests_json, 'native_audio')
        stalled_case = 'linux_client_keeps_video_when_the_audio_clock_stalls'
        if not socket.exists():
            # The default null sink initially queues two seconds of silence.
            # Keep that real failure case, then use its 50 ms mode for playback.
            start_service(True)
            stage('stalled-clock', native, ['--ignored', '--exact', stalled_case], 1,
                  dict(env, KETTLE_TEST_STALLED_AUDIO='1'))
            stop_service()
            if socket.exists():
                raise RuntimeError('closed owned audio service left its socket')
            start_service(False)
        else:
            result['reused_audio_service'] = True
            result['stalled_clock_unrun'] = 'needs an owned cold Pulse null sink'
        stages = [
            # The audio-path decoder tests. HEIF and AVIF stills need ffmpeg 7.1,
            # newer than this runner's, and are no part of the audio path.
            ('decoder', executable(args.decoder_tests_json, 'ffmpeg'),
             ['--skip', 'real_ffmpeg_draws_heif_and_avif_stills'], 25),
            ('playback', native, ['--ignored', '--skip', stalled_case], 5),
            ('broker', executable(args.worker_tests_json, 'process_boundary'),
             ['--ignored', '--exact', 'linux_pcm_only_broker_plays_pauses_and_closes'], 1),
            ('output', executable(args.output_tests_json, 'kettle_media_native'),
             ['--ignored', '--exact', 'pulse::tests::real_service_corks_mutes_and_retires_one_generation'], 1),
        ]
        for name, binary, options, count in stages:
            stage(name, binary, options, count)
        result['pass'] = all(stage['pass'] for stage in result['stages'])
        if not result['pass']:
            raise RuntimeError('native stages did not run every mandatory test successfully')
    finally:
        stop_service()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
