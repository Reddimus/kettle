"""Optional launch-clock collectors. No GUI calls occur on import."""
import hashlib
import json
import math
import re
import statistics
import subprocess
import time
from pathlib import Path

CONTRACT = 'hc-output-blink-v1'
MIB = 1024 * 1024
PERIOD_NS = 100_000_000
LATENESS_NS = 250_000_000
PRINT_BYTES = b''.join(f'{i:02d}: The quick brown fox jumps over the lazy dog 0123456789\n'.encode('ascii')
                       for i in range(1, 81))
PRINT_SHA256 = 'ebc2217ecfea2a45a9f09d7ca1bbce2213542fe55248f6d26aa0a69f7a8ed2b0'
if hashlib.sha256(PRINT_BYTES).hexdigest() != PRINT_SHA256:
    raise RuntimeError('printing bytes differ from pinned payload')


def integer(value):
    return type(value) is int and value >= 0


def read_jsonl(path, limit=2000):
    if path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError('timeline exceeds byte bound')
    lines = path.read_text().splitlines()
    if not lines or len(lines) > limit:
        raise ValueError('timeline count out of bounds')
    rows = [json.loads(line) for line in lines]
    if any(not isinstance(row, dict) for row in rows):
        raise ValueError('native record is not an object')
    return rows


def trace_reason(samples, pid, window):
    last = None
    identity = None
    for sample in samples:
        for field in ('query_start_ns', 'query_end_ns', 't_ns', 'scheduled_ns', 'pid', 'rss',
                      'footprint', 'max_footprint', 'cpu_ns', 'wakeups'):
            if not integer(sample.get(field)):
                return 'malformed native record'
        if sample.get('focus_notifications_overflow') is True:
            return 'native focus notification overflow'
        if sample.get('status') != 'ok':
            return 'native query failed or target exited'
        if sample['pid'] != pid or not isinstance(sample.get('process_start_identity'), str):
            return 'process lifetime identity missing or changed'
        if identity is not None and sample['process_start_identity'] != identity:
            return 'process lifetime identity missing or changed'
        identity = sample['process_start_identity']
        a, b = sample['query_start_ns'], sample['query_end_ns']
        if not sample['scheduled_ns'] <= a <= b == sample['t_ns']:
            return 'invalid query bounds'
        if b - sample['scheduled_ns'] > LATENESS_NS:
            return 'native query late'
        if last and (a <= last['query_end_ns'] or sample['scheduled_ns'] <= last['scheduled_ns']
                     or any(sample[f] < last[f] for f in ('cpu_ns', 'wakeups', 'max_footprint'))):
            return 'nonmonotonic native trace'
        for key in ('focus_before', 'focus_after'):
            check = sample.get(key)
            if not isinstance(check, dict) or not integer(check.get('t_ns')):
                return 'malformed focus check'
            if check.get('target_window') != window:
                return 'wrong target window'
        if not sample['focus_before']['t_ns'] <= a <= b <= sample['focus_after']['t_ns']:
            return 'focus checks do not bracket query'
        before, after = sample['focus_before']['t_ns'], sample['focus_after']['t_ns']
        if a - before > LATENESS_NS or after - b > LATENESS_NS:
            return 'stale focus check'
        if last and before <= last['focus_after']['t_ns']:
            return 'nonmonotonic focus checks'
        changes = sample.get('focus_changes')
        if not isinstance(changes, list) or len(changes) > 64 or any(
                not isinstance(c, dict) or not integer(c.get('t_ns')) or c.get('valid') is not False
                for c in changes):
            return 'malformed focus notifications'
        last = sample
    return None if samples else 'empty native trace'


def visible(sample, pid, window):
    return all(c.get('known') is True and c.get('valid') is True
               and c.get('frontmost_pid') == pid and c.get('top_window') == window
               and c.get('target_window') == window
               for c in (sample['focus_before'], sample['focus_after']))


def focus_reason(samples, start, end, pid, window):
    for sample in samples:
        for c in sample['focus_changes']:
            if start <= c['t_ns'] <= end:
                return 'known focus change during interval'
        for key in ('focus_before', 'focus_after'):
            c = sample[key]
            if start <= c['t_ns'] <= end and not (c.get('known') is True and c.get('valid') is True
                   and c.get('frontmost_pid') == pid and c.get('top_window') == window):
                return 'window not visible during interval'
    return None


def coverage_reason(samples, start, end):
    # Include uncovered edges as well as interior gaps.
    if len(samples) < math.ceil((end - start) / PERIOD_NS * .8):
        return 'insufficient timeline coverage'
    times = [start] + [s['query_end_ns'] for s in samples] + [end]
    if any(b - a > LATENESS_NS for a, b in zip(times, times[1:])):
        return 'timeline gap exceeds 250 ms'
    return None


def validity(row, workload, fields, reason):
    for field in fields:
        row.setdefault(field, None)
    row['metric_validity'] = {f: {'valid': reason is None, 'reason': reason,
                                  'capability_version': CONTRACT, 'expected': 1,
                                  'observed': int(reason is None)} for f in fields}
    row['printing_valid' if workload == 'output-memory' else 'blink_valid'] = reason is None
    row['printing_reason' if workload == 'output-memory' else 'blink_reason'] = reason
    return row


def printing_row(records, samples, pid, window):
    fields = ('printing_mib', 'printing_max_mib')
    row = {'lines_expected': 80, 'printing_payload_sha256': PRINT_SHA256,
           'timeline': samples, 'printing_log': records}
    reason = trace_reason(samples, pid, window)
    try:
        began, done = records[0]['began_ns'], records[-1]['done_ns']
        lines = records[1:-1]
        if not integer(began) or not integer(done) or len(lines) != 80 or len(records) != 82:
            raise ValueError('printing incomplete')
        if not 8_000_000_000 <= done - began <= 8_250_000_000:
            raise ValueError('printing duration is not eight seconds')
        previous = began
        for i, line in enumerate(lines):
            deadline = began + i * PERIOD_NS
            if (line.get('seq') != i + 1 or line.get('deadline_ns') != deadline
                    or not integer(line.get('write_ns')) or not integer(line.get('write_end_ns'))
                    or not previous <= line['write_ns'] <= line['write_end_ns'] <= done
                    or not deadline <= line['write_ns'] <= line['write_end_ns'] <= deadline + LATENESS_NS):
                raise ValueError('printing schedule incomplete or stalled')
            previous = line['write_end_ns']
        row.update(began_ns=began, done_ns=done, printed_s=(done-began)/1e9,
                   lines_written=len(lines), line_timestamps_ns=[l['write_ns'] for l in lines])
        if reason:
            raise ValueError(reason)
        active = [s for s in samples if s['query_start_ns'] >= began and s['query_end_ns'] <= done]
        reason = coverage_reason(active, began, done) or focus_reason(samples, began, done, pid, window)
        # Designated query is selected before focus eligibility. Never shop
        # for a lower later sample after the designated query loses focus.
        sample = next((s for s in samples if s['query_start_ns'] >= began + 6_000_000_000), None)
        if sample is None or sample['query_end_ns'] >= done or sample['query_end_ns'] > began + 6_250_000_000:
            raise ValueError('designated printing query missing or late')
        row.update(printing_sample_ns=sample['query_start_ns'], printing_sample_end_ns=sample['query_end_ns'],
                   printing_lateness_ms=(sample['query_end_ns']-began-6_000_000_000)/1e6,
                   printing_focus=visible(sample, pid, window))
        reason = reason or (None if row['printing_focus'] else 'designated query lost focus')
        if reason:
            raise ValueError(reason)
        row.update(printing_mib=sample['footprint']/MIB, printing_max_mib=sample['max_footprint']/MIB)
    except (KeyError, TypeError, IndexError, ValueError) as exc:
        reason = str(exc)
    return validity(row, 'output-memory', fields, reason)


def blink_row(samples, started, ready, pid, window, activity='unproven', evidence=None,
              settle=2.5, duration=6.0):
    fields = ('footprint_mib', 'cpu_percent', 'wakeups_per_second',
              'blink_median_footprint_mib', 'blink_peak_mib')
    start, end = started + int(settle * 1e9), started + int((settle + duration) * 1e9)
    row = dict(started_ns=started, blink_start_ns=start, blink_end_ns=end, blink_origin='launch',
               settle_s=settle, window_s=duration, ready_ns=ready, timeline=samples,
               blink_activity=activity, blink_evidence=evidence)
    reason = trace_reason(samples, pid, window)
    if ready >= start:
        reason = reason or 'readiness missed launch boundary'
    if not reason:
        first = next((s for s in samples if s['query_start_ns'] >= start), None)
        final = next((s for s in samples if s['query_start_ns'] >= end), None)
        if not first or not final or first['query_end_ns'] > start + LATENESS_NS or final['query_end_ns'] > end + LATENESS_NS:
            reason = 'blink boundary missing or late'
        else:
            retained = [s for s in samples if first['query_start_ns'] <= s['query_start_ns'] <= final['query_start_ns']]
            reason = (coverage_reason(retained, first['query_start_ns'], final['query_end_ns'])
                      or focus_reason(samples, start, final['focus_after']['t_ns'], pid, window))
            if any(not visible(s, pid, window) for s in retained):
                reason = reason or 'blink window not visible'
            span = (final['query_end_ns'] - first['query_end_ns']) / 1e9
            if span <= 0:
                reason = reason or 'invalid counter span'
            if not reason:
                values = [s['footprint']/MIB for s in retained]
                row.update(footprint_mib=final['footprint']/MIB,
                           cpu_percent=(final['cpu_ns']-first['cpu_ns'])/1e9/span*100,
                           wakeups_per_second=(final['wakeups']-first['wakeups'])/span,
                           blink_median_footprint_mib=statistics.median(values), blink_peak_mib=max(values),
                           counter_start_ns=first['query_end_ns'], counter_end_ns=final['query_end_ns'],
                           counter_span_s=span, frontmost_checks=[s['focus_before'] for s in retained])
    # Descriptive quiet-window numbers survive. Active-blink eligibility does not.
    eligibility = reason or (None if activity == 'verified' else 'active blink ' + activity)
    validity(row, 'blink-window', fields, eligibility)
    for field in ('blink_median_footprint_mib', 'blink_peak_mib'):
        row['metric_validity'][field].update(valid=reason is None, reason=reason, observed=int(reason is None))
    row['blink_window_valid'] = reason is None
    return row


def validation_reason(capture, setup):
    if not isinstance(capture, dict):
        return 'blink validation is not an object'
    if (not setup.get('display') or not setup.get('shape') or setup.get('timeout_s') is None
            or any(not isinstance(setup.get(key), str) or len(setup[key]) != 64 for key in ('binary_sha256', 'config_sha256'))):
        return 'blink setup identity incomplete'
    if capture.get('contract') != CONTRACT or capture.get('setup') != setup or capture.get('error'):
        return 'blink validation contract mismatch'
    started = capture.get('started_ns')
    if (not integer(started) or capture.get('start_ns') != started + int(setup['settle_s'] * 1e9)
            or not integer(capture.get('window_id')) or capture['window_id'] <= 0
            or capture.get('target_window_id') != capture['window_id']
            or not setup.get('native_display') or capture.get('native_display') != setup['native_display']):
        return 'blink native origin/window/display mismatch'
    try:
        if capture.get('cursor_rect') != [float(v) for v in setup['cursor_rect'].split(',')]:
            return 'blink native cursor crop mismatch'
    except (KeyError, TypeError, AttributeError, ValueError):
        return 'blink cursor crop unavailable'
    if (not isinstance(capture.get('validation_id'), str)
            or re.fullmatch(r'[0-9]+', capture['validation_id']) is None
            or not digest(capture.get('before_sha256'), nullable=True)):
        return 'blink evidence linkage malformed'
    frames = capture.get('frames')
    if not isinstance(frames, list) or not 49 <= len(frames) <= 202:
        return 'blink capture coverage incomplete'
    start, end = capture.get('start_ns'), capture.get('end_ns')
    if not integer(start) or not integer(end) or end - start != int(setup['window_s'] * 1e9):
        return 'blink capture interval mismatch'
    hashes, times, arrivals = [], [], []
    for f in frames:
        if (not isinstance(f, dict) or not integer(f.get('t_ns')) or not isinstance(f.get('sha256'), str)
                or not digest(f['sha256']) or f.get('visible') is not True
                or f.get('frame_status') not in ('complete', 'idle')):
            return 'malformed blink frame'
        if not integer(f.get('arrival_ns')) or not 0 <= f['t_ns'] - f['arrival_ns'] <= LATENESS_NS:
            return 'blink frame missing or stale arrival'
        if arrivals and f['arrival_ns'] < arrivals[-1]:
            return 'nonmonotonic blink arrivals'
        hashes.append(f['sha256']); times.append(f['t_ns']); arrivals.append(f['arrival_ns'])
    if times[0] < start or times[-1] > end + LATENESS_NS or times[0] - start > LATENESS_NS or end - times[-1] > LATENESS_NS:
        return 'blink capture edges missing'
    if any(not 0 < b-a <= LATENESS_NS for a,b in zip(times,times[1:])):
        return 'blink capture gap'
    # Exactly two stable pixel states prevent animated text/noise from being
    # called cursor blink. Repeated transitions are needed near both ends.
    if len(set(hashes)) != 2:
        return 'cursor does not have two stable pixel states'
    transitions = [times[i] for i in range(1,len(times)) if hashes[i] != hashes[i-1]]
    if len(transitions) < 4 or transitions[1] > start + 2_000_000_000 or transitions[-2] < end - 2_000_000_000:
        return 'cursor stopped or blink transitions missing at edges'
    if any(b-a > 2_000_000_000 for a,b in zip(transitions,transitions[1:])):
        return 'cursor stopped inside validation interval'
    return None


def digest(value, nullable=False):
    return (nullable and value is None) or (isinstance(value, str)
            and re.fullmatch(r'[0-9a-f]{64}', value) is not None)


def blink_evidence(path, setup, disabled=False):
    if disabled:
        return 'disabled-default', None
    if not path:
        return 'unproven', None
    try:
        if Path(path).stat().st_size > 1024 * 1024:
            raise ValueError('blink validation exceeds byte bound')
        data = Path(path).read_bytes()
        capture = json.loads(data)
        if not isinstance(capture, dict):
            raise ValueError('blink validation is not an object')
        reason = validation_reason(capture, setup)
        evidence = {'content_sha256': hashlib.sha256(data).hexdigest(), 'reason': reason,
                    'validation_id': capture.get('validation_id') if isinstance(capture.get('validation_id'), str)
                        and re.fullmatch(r'[0-9]+', capture['validation_id']) else None,
                    'before_sha256': capture.get('before_sha256') if digest(capture.get('before_sha256')) else None,
                    'setup': setup}
        return ('unproven' if reason else 'verified'), evidence
    except (OSError, ValueError, KeyError, TypeError):
        return 'unproven', {'reason': 'blink validation unreadable'}


def build_helpers(probes, tools):
    tools.mkdir(parents=True, exist_ok=True)
    result = {}
    for name, suffix in (('printing', '.c'), ('observer', '.m')):
        source, binary = probes / (name + suffix), tools / name
        command = ['clang', '-O2', '-o', str(binary), str(source)]
        if suffix == '.m':
            command += ['-fobjc-arc', '-framework', 'AppKit', '-framework', 'CoreGraphics']
        subprocess.run(command, check=True)
        result[name] = binary
    return result


class OwnedObserver:
    """Receipt handle for a child spawned/reaped by the native launch owner."""
    def __init__(self, context, launch):
        self.context, self.launch = context, launch

    def path(self, suffix):
        return Path(str(self.context) + '.observer-' + suffix)

    def poll(self):
        return 0 if self.path('reaped').exists() else None

    def terminate(self):
        self.path('stop').touch()

    kill = terminate

    def wait(self, timeout=None):
        deadline = time.monotonic() + (timeout or 30)
        while self.poll() is None:
            if self.launch.poll() is not None:
                raise RuntimeError('launch owner exited without sampler reap receipt')
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired('owned observer drain', timeout)
            time.sleep(.01)
        return 0

    def reap(self):
        # The native owner services stop before releasing its target. Retry
        # cancellations, but let a missing ownership receipt fail explicitly.
        self.terminate()
        while True:
            try:
                self.wait(timeout=5)
                return
            except (KeyboardInterrupt, SystemExit):
                self.kill()


def start_observer(runner, process, context, args):
    observer = OwnedObserver(context, process)
    request = observer.path('request')
    temporary = request.with_suffix('.partial')
    temporary.write_text(json.dumps(args))
    temporary.replace(request)
    return observer


def now_ns():
    return time.clock_gettime_ns(time.CLOCK_UPTIME_RAW)


def collect(runner, name, workload, options, keep, setup=None):
    """One owned launch, with observer reaped first on every exit path."""
    work = runner.work
    context, barrier, log, timeline = [work / f'hc-{f}' for f in ('launch.json', 'barrier', 'printing.jsonl', 'timeline.jsonl')]
    receipts = [Path(str(context) + '.observer-' + suffix) for suffix in ('request','started','stop','reaped')]
    for path in (context, barrier, log, timeline, *receipts):
        path.unlink(missing_ok=True)
    body = (f'exec "{runner.probes["printing"]}" "{barrier}" "{log}"'
            if workload == 'output-memory' else 'exec /bin/sleep 30')
    process, observer = None, None
    result, raw, records = {}, [], []
    try:
        runner.observation_context = context
        process = runner.launch(name, body, 45)
        if not runner.wait_for(work / 'grid', 10) or not runner.wait_for(context, 2):
            return {'error': 'launch/window/grid readiness missing'}
        if options['activate'] and not runner.activate():
            return {'error': 'activation failed'}
        info = json.loads(context.read_text())
        started, pid, window = info['started_ns'], info['pid'], info['window_id']
        if setup is not None:
            setup = {**setup, 'native_display': info.get('native_display')}
        if options.get('validate_only') and not info.get('native_display'):
            return {'error': 'native display identity missing'}
        ready = now_ns()
        origin = ready + 1_000_000_000 if workload == 'output-memory' else started + int(options['settle'] * 1e9)
        if workload == 'blink-window' and ready >= origin:
            return {'error': 'readiness missed launch boundary', 'started_ns': started, 'ready_ns': ready}
        if options.get('validate_only'):
            out = work / 'blink-capture.json'
            out.unlink(missing_ok=True)
            args = ['--blink-check', '--pid', str(pid), '--out', str(out), '--started-ns', str(started),
                    '--blink-settle', str(options['settle']), '--blink-window', str(options['window']),
                    '--cursor-rect', options['rect'], '--window-id', str(window), '--deadline-ms', '30000']
            runner.blink_probe(args, work, 35)
            result = json.loads(out.read_text())
            result.update(contract=CONTRACT, setup=setup, validation_id=str(started),
                          before_sha256=options.get('before_sha256'), target_window_id=window)
            result['validation_reason'] = validation_reason(result, setup)
            if options.get('before_path') and blink_evidence(options['before_path'], setup)[0] != 'verified':
                result['validation_reason'] = 'before-validation setup mismatch or invalid capture'
            if result['validation_reason']:
                result['error'] = result['validation_reason']
            return result
        sample_ms = options.get('sample_ms', 100)
        count = math.ceil((11 if workload == 'output-memory' else options['window']) * 1000 / sample_ms) + 2
        observer = start_observer(runner, process, context, [str(runner.probes['observer']), str(pid),
                    str(window), str(timeline), str(origin), str(sample_ms), str(count)])
        if workload == 'output-memory':
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                if timeline.exists() and timeline.stat().st_size:
                    first = read_jsonl(timeline)
                    if trace_reason(first, pid, window) or not visible(first[0], pid, window):
                        return {'error': 'initial observer query invalid'}
                    break
                time.sleep(.01)
            else:
                return {'error': 'observer never confirmed readiness'}
            barrier.touch()
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if log.exists() and 'done_ns' in log.read_text():
                    break
                time.sleep(.02)
        else:
            deadline = started + int((options['settle'] + options['window']) * 1e9) + 300_000_000
            while now_ns() < deadline:
                time.sleep(.02)
    finally:
        try:
            if observer is not None:
                runner.end_sampler(observer)
        finally:
            if process is not None:
                clean = runner.stop(process, 30)
        if process is not None:
            if options.get('validate_only') and result:
                if not clean:
                    result.update(killed=True, error='validation target cleanup failed')
                keep.with_suffix('.json').write_text(json.dumps(result, sort_keys=True, indent=2) + '\n')
        runner.observation_context = None
        if timeline.exists():
            keep.with_suffix('.jsonl').write_bytes(timeline.read_bytes())
        if log.exists():
            keep.with_suffix('.printing.jsonl').write_bytes(log.read_bytes())
    try:
        raw = read_jsonl(timeline)
        if workload == 'output-memory':
            result = printing_row(read_jsonl(log, 82), raw, pid, window)
        else:
            activity, evidence = blink_evidence(options.get('validation'), setup, options.get('disabled', False))
            result = blink_row(raw, started, ready, pid, window, activity, evidence, options['settle'], options['window'])
        if not clean:
            result['killed'] = True
        return result
    except (OSError, ValueError, KeyError, TypeError) as exc:
        # Session artifacts are private; the public row has a logical reason.
        try:
            keep.with_suffix('.trace-error.json').write_text(json.dumps({
                'timeline_path': str(timeline), 'payload_path': str(log), 'detail': str(exc)
            }, sort_keys=True) + '\n')
        except OSError:
            pass
        return {'error': 'observer/payload trace invalid'}
