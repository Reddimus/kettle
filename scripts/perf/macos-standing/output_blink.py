"""Optional launch-clock collectors. No GUI calls occur on import."""
import hashlib
import json
import math
import os
import re
import statistics
import stat
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


def read_jsonl(path, limit=2000, byte_limit=4 * 1024 * 1024):
    if path.stat().st_size > byte_limit:
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


def focus_unknown(samples):
    """Whether a focus check could not read the window server (no frontmost
    app, the window missing from the list, unreadable bounds). That proves
    nothing about the desktop, so it is the observer's failure, not a focus
    change, and is judged ahead of the desktop's reasons."""
    return any(s[key].get('known') is not True for s in samples for key in ('focus_before', 'focus_after'))


# A hidden window that no record blames on another process: it may be the
# terminal's own dialog or second window, so it is never the desktop's.
UNPROVEN = 'window hidden, desktop cause unproven'
# Rows built under the rules below. Only they can carry the desktop's reasons
# into an observer pilot; older rows named those reasons without the owners.
ATTRIBUTION_CONTRACT = 1


def other_process(owner, pid):
    return integer(owner) and owner > 0 and owner != pid


def check_held(c, pid, window):
    """A check that saw the target app in front with the measured window on top."""
    return (c.get('known') is True and c.get('valid') is True and c.get('frontmost_pid') == pid
            and c.get('top_window') == window and c.get('target_window') == window)


def desktop_hid(records, pid, window):
    """Whether the focus records that failed prove the desktop interrupted.
    An activation proves it when it names another app; the target's own
    activation proves nothing. A check proves it only while the measured
    window is still the target's (target_owner) and another process is in
    front, on top or over the window; one that shows the target's own second
    window on top or its own dialog over the window blames the target, and
    so does one that names nobody."""
    proven = False
    for c in records:
        if 'frontmost_pid' not in c:
            proven = proven or other_process(c.get('pid'), pid)
            continue
        # The observer records a cover only while the target is in front and
        # on top, so the target's own dialog names nobody else and fails here.
        if (c.get('known') is not True or c.get('target_owner') != pid
                or (c.get('top_window') != window and c.get('top_owner') == pid)
                or not any(other_process(c.get(key), pid) for key in ('frontmost_pid', 'top_owner', 'cover_owner'))):
            return False
        proven = True
    return proven


def focus_verdict(samples, start, end, judged, pid, window, hidden_name):
    """The one focus verdict of a row, from every record that bears on it:
    the activations and checks between start and end, and every check of the
    `judged` samples. None while focus held; "focus evidence unavailable"
    when a check could not read the window server; the desktop's reason only
    when every failing record proves it (desktop_hid); UNPROVEN otherwise.
    No record is skipped because another already failed."""
    checks = {id(c): c for s in samples for c in (s['focus_before'], s['focus_after']) if start <= c['t_ns'] <= end}
    inside = set(checks)
    checks.update({id(c): c for s in judged for c in (s['focus_before'], s['focus_after'])})
    if any(c.get('known') is not True for c in checks.values()):
        return 'focus evidence unavailable'
    changes = [c for s in samples for c in s['focus_changes'] if start <= c['t_ns'] <= end]
    failing = {k: c for k, c in checks.items() if not check_held(c, pid, window)}
    if not changes and not failing:
        return None
    if not desktop_hid([*changes, *failing.values()], pid, window):
        return UNPROVEN
    if changes:
        return 'known focus change during interval'
    return 'window not visible during interval' if inside & set(failing) else hidden_name


def observed_until(samples, pid, window, sample_ms, until):
    """The observer's own failure over a round that failed at `until`: its
    trace, an unreadable focus check, a broken cadence, or coverage that ends
    before the failure. None when it watched throughout."""
    reason = trace_reason(samples, pid, window)
    if reason:
        return reason
    if focus_unknown(samples):
        return 'focus evidence unavailable'
    if any(b['scheduled_ns'] - a['scheduled_ns'] != sample_ms * 1_000_000 for a, b in zip(samples, samples[1:])):
        return 'typing observer interval mismatch'
    watched = [s for s in samples if s['query_end_ns'] <= until]
    return coverage_reason(watched, samples[0]['query_start_ns'], until)


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


def printing_row(records, samples, pid, window, observer_off=False):
    fields = ('printing_mib', 'printing_max_mib')
    row = {'lines_expected': 80, 'printing_payload_sha256': PRINT_SHA256,
           'timeline': samples, 'printing_log': records, 'attribution_contract': ATTRIBUTION_CONTRACT}
    if observer_off:
        row['coverage_waived'] = 'observer-pilot off arm'
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
        reason = None if observer_off else coverage_reason(active, began, done)
        # Without a record after done, a late focus change would go unseen.
        if observer_off and not any(s['query_start_ns'] >= done for s in samples):
            reason = reason or 'off-arm query after done missing'
        # Designated query is selected before focus eligibility. Never shop
        # for a lower later sample after the designated query loses focus.
        sample = next((s for s in samples if s['query_start_ns'] >= began + 6_000_000_000), None)
        if sample is None or sample['query_end_ns'] >= done or sample['query_end_ns'] > began + 6_250_000_000:
            raise ValueError('designated printing query missing or late')
        row.update(printing_sample_ns=sample['query_start_ns'], printing_sample_end_ns=sample['query_end_ns'],
                   printing_lateness_ms=(sample['query_end_ns']-began-6_000_000_000)/1e6,
                   printing_focus=visible(sample, pid, window))
        # The desktop's reasons (focus moving, the window hidden) come last,
        # so they never stand in for a failure of the observer, harness or
        # terminal. The off arm also judges its readiness query and its
        # record after done, outside the output.
        reason = reason or focus_verdict(samples, began, done, samples if observer_off else [sample], pid, window,
                                         'printing window not visible' if observer_off else 'designated query lost focus')
        if reason:
            raise ValueError(reason)
        row.update(printing_mib=sample['footprint']/MIB, printing_max_mib=sample['max_footprint']/MIB)
    except (KeyError, TypeError, IndexError, ValueError) as exc:
        reason = str(exc)
    return validity(row, 'output-memory', fields, reason)


def blink_row(samples, started, ready, pid, window, activity='unproven', evidence=None,
              settle=2.5, duration=6.0, observer_off=False):
    fields = ('footprint_mib', 'cpu_percent', 'wakeups_per_second',
              'blink_median_footprint_mib', 'blink_peak_mib')
    start, end = started + int(settle * 1e9), started + int((settle + duration) * 1e9)
    row = dict(started_ns=started, blink_start_ns=start, blink_end_ns=end, blink_origin='launch',
               settle_s=settle, window_s=duration, ready_ns=ready, timeline=samples,
               blink_activity=activity, blink_evidence=evidence, attribution_contract=ATTRIBUTION_CONTRACT)
    if observer_off:
        row['coverage_waived'] = 'observer-pilot off arm'
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
            reason = ((None if observer_off else coverage_reason(retained, first['query_start_ns'], final['query_end_ns']))
                      or focus_verdict(samples, start, final['focus_after']['t_ns'],
                                       samples if observer_off else retained, pid, window, 'blink window not visible'))
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


def select_validation(paths, setup):
    """The first named validation that verifies this row's own setup.

    A validation certifies one binary/config/display setup, so a Kettle A/B
    names one file per side and each row takes its own. Returns the chosen
    path (None when nothing verified), the activity and its evidence."""
    paths = [paths] if isinstance(paths, (str, Path)) else list(paths or [])
    first = None
    for path in paths:
        activity, evidence = blink_evidence(path, setup)
        if activity == 'verified':
            return path, activity, evidence
        first = first or (None, activity, evidence)
    return first or (None, 'unproven', None)


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
    for path in (context, barrier, log, timeline, Path(str(timeline) + ".self.json"), *receipts):
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
            before = select_validation(options.get('before_path'), setup)[0] if options.get('before_path') else None
            result.update(contract=CONTRACT, setup=setup, validation_id=str(started),
                          before_sha256=hashlib.sha256(Path(before).read_bytes()).hexdigest() if before else None,
                          target_window_id=window)
            result['validation_reason'] = validation_reason(result, setup)
            if options.get('before_path') and before is None:
                result['validation_reason'] = 'before-validation setup mismatch or invalid capture'
            if result['validation_reason']:
                result['error'] = result['validation_reason']
            return result
        sample_ms = options.get('sample_ms', 100)
        count = math.ceil((11 if workload == 'output-memory' else options['window']) * 1000 / sample_ms) + 2
        offsets = sparse_offsets(workload, options) if options.get('observer_arm') == 'off' else None
        observer = start_observer(runner, process, context, [str(runner.probes['observer']), str(pid),
                    str(window), str(timeline), str(origin), str(sample_ms), str(len(offsets) if offsets else count)]
                    + ([','.join(map(str, offsets))] if offsets else []))
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
            if offsets:
                # The off arm's last query follows done and carries any late
                # focus notification; stop the observer only after it.
                final = origin + offsets[-1] * 1_000_000 + LATENESS_NS
                while now_ns() < final and (not timeline.exists()
                                             or timeline.read_bytes().count(b'\n') < len(offsets)):
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
            result = printing_row(read_jsonl(log, 82), raw, pid, window, options.get('observer_arm') == 'off')
        else:
            if options.get('disabled', False):
                activity, evidence = blink_evidence(None, setup, True)
            else:
                activity, evidence = select_validation(options.get('validation'), setup)[1:]
            result = blink_row(raw, started, ready, pid, window, activity, evidence, options['settle'], options['window'], options.get('observer_arm') == 'off')
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


TYPING_CONTRACT = 'hc-typing-memory-v1'


def typing_method(sample_ms=100):
    return dict(contract=TYPING_CONTRACT, clock='CLOCK_UPTIME_RAW',
                probe_clock='mach_absolute_time_ns', sample_interval_ms=sample_ms,
                minimum_samples=5, minimum_coverage=.8, maximum_gap_ms=250,
                clock_tolerance_ns=1_000_000)


def typing_artifact_valid(artifact):
    return isinstance(artifact, dict) and all(isinstance(artifact.get(k), str)
        and len(artifact[k]) == 64 and all(c in '0123456789abcdef' for c in artifact[k])
        for k in ('bundle_sha256', 'executable_sha256', 'source_sha256'))


def typing_epoch(probe, pid, window):
    epoch = probe.get('typing_epoch')
    if not isinstance(epoch, dict):
        raise ValueError('typing epoch unavailable')
    if (epoch.get('contract') != TYPING_CONTRACT or epoch.get('clock') != 'CLOCK_UPTIME_RAW'
            or epoch.get('probe_clock') != 'mach_absolute_time_ns'):
        raise ValueError('typing clock contract mismatch')
    offsets = []
    for key in ('clock_before', 'clock_after'):
        check = epoch.get(key)
        if not isinstance(check, dict) or any(not integer(check.get(f)) for f in
                ('raw_before_ns', 'raw_after_ns', 'mach_ns')):
            raise ValueError('typing clock conversion unavailable')
        a, b = check['raw_before_ns'], check['raw_after_ns']
        if not 0 <= b-a <= 1_000_000:
            raise ValueError('typing clock check too wide')
        offsets.append(a + (b-a)//2 - check['mach_ns'])
    if abs(offsets[1]-offsets[0]) > 1_000_000:
        raise ValueError('typing clock epoch drift')
    start, end = epoch.get('typing_start_ns'), epoch.get('typing_end_ns')
    if not integer(start) or not integer(end) or start >= end:
        raise ValueError('typing epoch bounds invalid')
    if any(not integer(epoch.get(f)) for f in ('start_mach_ns', 'end_mach_ns')):
        raise ValueError('typing Mach bounds missing')
    if start != epoch['start_mach_ns'] + offsets[0] or end != epoch['end_mach_ns'] + offsets[0]:
        raise ValueError('typing clock conversion mismatch')
    if not epoch['clock_before']['raw_after_ns'] <= start < end <= epoch['clock_after']['raw_before_ns']:
        raise ValueError('typing clock checks do not bracket epoch')
    measured = [s for s in probe.get('samples', []) if not s.get('warmup')]
    if (not measured or measured[0].get('t_post') != epoch['start_mach_ns']
            or not integer(measured[-1].get('t_post')) or measured[-1]['t_post'] > epoch['end_mach_ns']
            or probe.get('error') or epoch.get('guards_ok') is not True
            or epoch.get('pid') != pid or epoch.get('window_id') != window or not window):
        raise ValueError('typing probe guards/window mismatch')
    return epoch


def typing_memory_row(probe, samples, pid, window, sample_ms, artifact, observer_reason=None):
    fields = ('typing_footprint_mib', 'typing_observed_peak_mib', 'typing_max_footprint_mib')
    row = dict(typing_sample_interval_ms=sample_ms, typing_sample_count=0,
               typing_expected_samples=0, typing_coverage=0., typing_timeline=samples,
               typing_tool_artifact=artifact, typing_window_id=window, typing_pid=pid,
               attribution_contract=ATTRIBUTION_CONTRACT)
    reason = observer_reason
    try:
        if observer_reason == 'observer off (pilot arm)':
            raise ValueError(observer_reason)
        epoch = typing_epoch(probe, pid, window)
        row.update(typing_start_ns=epoch['typing_start_ns'], typing_end_ns=epoch['typing_end_ns'],
                   typing_clock=epoch)
        start, end = epoch['typing_start_ns'], epoch['typing_end_ns']
        if sample_ms != 100:
            raise ValueError('diagnostic typing sample interval')
        if not typing_artifact_valid(artifact):
            raise ValueError('verified typing probe artifact unavailable')
        if reason:
            raise ValueError(reason)
        reason = trace_reason(samples, pid, window)
        if reason:
            raise ValueError(reason)
        if any(b['scheduled_ns']-a['scheduled_ns'] != sample_ms * 1_000_000
               for a,b in zip(samples, samples[1:])):
            raise ValueError('typing observer interval mismatch')
        active = [s for s in samples if start <= s['query_start_ns'] <= s['query_end_ns'] <= end]
        expected = math.ceil((end-start)/(sample_ms*1_000_000))
        row.update(typing_sample_count=len(active), typing_expected_samples=expected,
                   typing_coverage=len(active)/expected)
        if len(active) < 5:
            raise ValueError('insufficient typing memory duration')
        # The last query can end inside the epoch with its focus check after
        # it, so the measured queries' checks are judged as well.
        reason = coverage_reason(active, start, end) or focus_verdict(samples, start, end, active, pid, window,
                                                                     'typing window not visible')
        if reason:
            raise ValueError(reason)
        values = [s['footprint']/MIB for s in active]
        row.update(typing_footprint_mib=statistics.median(values), typing_observed_peak_mib=max(values),
                   typing_max_footprint_mib=max(s['max_footprint']/MIB for s in active))
    except (ValueError, KeyError, TypeError) as exc:
        reason = str(exc)
    row.update(typing_memory_valid=reason is None, typing_memory_reason=reason)
    for field in fields:
        if reason:
            row[field] = None
        row.setdefault('metric_validity', {})[field] = dict(valid=reason is None, reason=reason,
            capability_version=TYPING_CONTRACT,
            expected=max(5, math.ceil(row['typing_expected_samples'] * .8)), observed=row['typing_sample_count'])
    return row


def sparse_offsets(workload, options):
    # Printing: readiness, a burst that always holds the designated 6 s query,
    # and one query after done whose record carries any focus notification
    # from the rest of the output, as the dense arm's records would.
    if workload == 'output-memory':
        return [0, *range(5900, 6501, 100), 8600]
    return [0, math.ceil(options['window'] * 1000)]


def observer_cost(timeline, observer_off=False):
    """Numeric allowlist only; native paths and error contents stay private."""
    result = dict(cpu_ns=0 if observer_off else None, wakeups=0 if observer_off else None,
                  query_count=0 if observer_off else None, query_duration_median_ms=None,
                  query_duration_max_ms=None, deadline_lateness_max_ms=None,
                  target_cpu_delta_ns=None, target_wakeups_delta=None)
    if observer_off:
        return result
    try:
        path = Path(str(timeline) + '.self.json')
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > 4096:
                raise ValueError('self-cost is not a bounded regular file')
            raw = stream.read(4097)
        if len(raw) > 4096:
            raise ValueError('self-cost exceeds bound')
        data = json.loads(raw)
        if not isinstance(data, dict) or any(not integer(data.get(k)) or data[k] > 2**64-1
                for k in ('cpu_ns', 'wakeups', 'query_count')) or data.get('query_count', 0) > 12000:
            raise ValueError('invalid self-cost')
        result.update({k: data[k] for k in ('cpu_ns', 'wakeups', 'query_count')})
    except (OSError, ValueError, TypeError, RecursionError, OverflowError):
        pass
    try:
        samples = read_jsonl(timeline, 12000, 32 * 1024 * 1024)
        durations, lateness = [], []
        for sample in samples:
            a, b, deadline = (sample[k] for k in ('query_start_ns', 'query_end_ns', 'scheduled_ns'))
            if not all(integer(v) for v in (a, b, deadline)) or not deadline <= a <= b:
                raise ValueError('invalid query')
            durations.append((b-a)/1e6)
            lateness.append((a-deadline)/1e6)
        result.update(query_duration_median_ms=statistics.median(durations),
                      query_duration_max_ms=max(durations), deadline_lateness_max_ms=max(lateness))
        for source, dest in (('cpu_ns', 'target_cpu_delta_ns'), ('wakeups', 'target_wakeups_delta')):
            values = [s.get(source) for s in samples]
            if all(integer(v) for v in values) and all(a <= b for a, b in zip(values, values[1:])):
                result[dest] = values[-1] - values[0] if len(values) >= 2 else None
    except (OSError, ValueError, KeyError, TypeError, RecursionError, OverflowError, statistics.StatisticsError):
        pass
    return result
