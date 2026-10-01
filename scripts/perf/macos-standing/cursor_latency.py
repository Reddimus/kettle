"""Cursor-only method, deadline and optional C2 exit wire. No app operations."""
import json
import math
import re
import struct

CONTRACT = 'cursor_exit_v1'
ENABLE = b'\x1b[?25h\x1b[1 q\x1b[2;3H'


def method(payload='block', gap=None, first=None):
    cursor = payload == 'cursor'
    limits = (2000, 2400) if cursor else (100, 300)
    if gap is not None:
        if not re.fullmatch(r'[0-9]+:[0-9]+', gap):
            raise ValueError('latency gap requires MIN:MAX')
        limits = tuple(map(int, gap.split(':')))
    first = (2000 if cursor else 0) if first is None else first
    if not (1 <= limits[0] <= limits[1] <= 5000) or not (0 <= first <= 10000):
        raise ValueError('latency gap outside bounded range')
    if cursor and (limits[0] < 1500 or first < 1500):
        raise ValueError('cursor latency requires at least 1500 ms quiet')
    return {'payload': payload, 'gap_ms': list(limits), 'first_gap_ms': first}


def budget(options):
    # Six 250 ms calibration samples, setup, worst-case censor/stability per
    # key, initial quiet time, and a final gap retained by the block probe.
    stream = options['warmup'] + options['keys']
    gap = options.get('gap_ms', [2000, 2400] if options.get('payload') == 'cursor' else [100, 300])[1] / 1000
    first = options.get('first_gap_ms', 2000 if options.get('payload') == 'cursor' else 0) / 1000
    probe = 120 + first + (6 + stream) * (gap + options['censor_ms'] / 1000 + .2)
    return {'probe_s': probe, 'wait_s': probe + 15, 'launch_s': max(600, probe + 60)}


def object_pairs(pairs):
    obj = {}
    for key, value in pairs:
        if key in obj:
            raise ValueError('duplicate cursor exit field')
        obj[key] = value
    return obj


def integer(value):
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= 2**64 - 1


def parse_exits(text, launch_id, window_id, probe, payload, warmup, measured, percentile):
    expected = warmup + measured
    result = dict(cursor_exit_available=False, cursor_exit_capability=None,
        cursor_exit_expected=expected, cursor_exit_count=0, cursor_exit_measured_count=0,
        cursor_exit_records=[], cursor_exit_p50_us=None, cursor_exit_p95_us=None,
        cursor_exit_max_us=None, cursor_exit_valid=False)
    wire = []
    for line in text.splitlines(keepends=True):
        if 'cursor_exit_' not in line and 'exit_frame_us=' not in line:
            continue
        if not line.startswith(CONTRACT + ' ') or not line.endswith('\n') or len(line) > 4096:
            raise ValueError('malformed or legacy cursor exit record')
        try:
            obj = json.loads(line[len(CONTRACT)+1:], object_pairs_hook=object_pairs)
        except (ValueError, TypeError):
            raise ValueError('malformed cursor exit JSON') from None
        if not isinstance(obj, dict):
            raise ValueError('cursor exit record is not an object')
        wire.append(obj)
    if not wire:
        return result
    header = wire[0]
    if (set(header) != {'event','launch_id','pane_id','window_id','clock','first_key_seq'}
            or header['event'] != 'capability' or header['launch_id'] != launch_id
            or not integer(header['pane_id']) or header['window_id'] != window_id
            or not integer(header['window_id']) or header['clock'] != 'CLOCK_UPTIME_RAW'
            or header['first_key_seq'] != 7 or not integer(header['first_key_seq'])):
        raise ValueError('cursor exit capability identity mismatch')
    inputs, exits, seen = [], [], set()
    last_seq = 6 + expected
    for record in wire[1:]:
        seq = record.get('key_seq')
        if (record.get('launch_id') != launch_id
                or record.get('pane_id') != header['pane_id'] or not integer(record.get('pane_id'))
                or not integer(seq) or not 1 <= seq <= last_seq or seq in seen):
            raise ValueError('cursor input/exit identity or sequence mismatch')
        seen.add(seq)
        if record.get('event') == 'input':
            if set(record) != {'event','launch_id','pane_id','key_seq'} or seq >= header['first_key_seq']:
                raise ValueError('cursor input in exit range or invalid schema')
            inputs.append(record)
        elif record.get('event') == 'exit' and seq >= header['first_key_seq']:
            exits.append(record)
        else:
            raise ValueError('cursor input/exit event or calibration mismatch')
    if seen != set(range(1, last_seq + 1)) or len(inputs) != 6:
        raise ValueError('cursor input/exit sequence gap')
    if len(exits) != expected:
        raise ValueError('cursor exit stream count mismatch')
    samples = probe.get('samples', [])
    if len(samples) != expected or set(payload) != set(range(1, 7 + expected)):
        raise ValueError('cursor exit key stream incomplete')
    epoch = probe.get('typing_epoch') or {}
    offsets = []
    for field in ('clock_before', 'clock_after'):
        check = epoch.get(field) or {}
        if (not all(integer(check.get(k)) for k in ('raw_before_ns','raw_after_ns','mach_ns'))
                or not 0 <= check['raw_after_ns']-check['raw_before_ns'] <= 1_000_000):
            raise ValueError('cursor exit clock conversion missing')
        offsets.append(check['raw_before_ns'] + (check['raw_after_ns']-check['raw_before_ns'])//2 - check['mach_ns'])
    if (epoch.get('clock') != 'CLOCK_UPTIME_RAW' or epoch.get('probe_clock') != 'mach_absolute_time_ns'
            or epoch.get('window_id') != window_id or epoch.get('guards_ok') is not True
            or abs(offsets[0]-offsets[1]) > 1_000_000):
        raise ValueError('cursor exit clock/window mismatch')
    if (not integer(epoch.get('typing_end_ns'))
            or epoch['clock_before']['raw_after_ns'] > samples[0].get('t_post', 0) + offsets[0]
            or epoch['clock_after']['raw_before_ns'] < epoch['typing_end_ns']):
        raise ValueError('cursor clock checks do not bracket key epoch')
    ends = []
    for i, (record, sample) in enumerate(zip(exits, samples)):
        seq = 7 + i
        if (set(record) != {'event','launch_id','pane_id','key_seq','t_start_ns','t_end_ns','total_frame_us','layer_active'}
                or record['event'] != 'exit' or record['launch_id'] != launch_id
                or record['pane_id'] != header['pane_id'] or not integer(record['pane_id'])
                or record['key_seq'] != seq or not integer(record['key_seq'])
                or record['layer_active'] is not True
                or sample.get('seq') != seq or sample.get('warmup') is not (i < warmup)
                or not integer(sample.get('t_post'))
                or not all(integer(record[k]) for k in ('t_start_ns','t_end_ns','total_frame_us'))):
            raise ValueError('cursor exit key/pane/order mismatch')
        nbytes, read, written = payload[seq]
        post = sample['t_post'] + offsets[0]
        # The app latches the posted j's payload sequence at input acceptance.
        # Exit entry can precede the PTY echo, so it need not follow read/write.
        bound = (samples[i+1]['t_post'] + offsets[0] if i+1 < expected
                 and integer(samples[i+1].get('t_post')) else epoch.get('typing_end_ns', 0))
        start, end = record['t_start_ns'], record['t_end_ns']
        if (nbytes != 1 or not post <= read <= written < bound or not post <= start <= end < bound
                or (ends and start < ends[-1])
                or record['total_frame_us'] != (end-start+999)//1000):
            raise ValueError('cursor exit duration/key interval mismatch')
        ends.append(end)
    values = [r['total_frame_us'] for r in exits[warmup:]]
    result.update(cursor_exit_available=True, cursor_exit_capability=CONTRACT,
        cursor_exit_count=len(exits), cursor_exit_measured_count=len(values), cursor_exit_records=exits,
        cursor_exit_p50_us=percentile(values, .5), cursor_exit_p95_us=percentile(values, .95),
        cursor_exit_max_us=max(values), cursor_exit_valid=True)
    return result


def pooled(rows, percentile):
    values = [r['total_frame_us'] for row in rows
        if not row.get('warmup') and not row.get('error') and not row.get('killed')
        and not row.get('seq_mismatch') and row.get('cursor_exit_valid') is True
        for r in row['cursor_exit_records'][row['cursor_exit_count']-row['cursor_exit_measured_count']:]]
    if not values:
        return None
    p95 = percentile(values, .95)
    return {'count': len(values), 'p50_us': percentile(values, .5), 'p95_us': p95,
            'max_us': max(values), 'p95_le_4000': p95 <= 4000}


def read_payload(data):
    if len(data) % 32:
        raise ValueError('torn cursor payload log')
    records = {}
    for seq, size, read, written in struct.iter_unpack('<4Q', data):
        if seq in records:
            raise ValueError('duplicate cursor payload sequence')
        records[seq] = (size, read, written)
    return records


def validate_stream(probe, records, warmup, measured):
    samples = probe.get('samples') or []
    if (probe.get('calibration_keys') != 6 or len(samples) != warmup + measured
            or set(records) != set(range(1, 7 + warmup + measured))
            or any(size != 1 for size, read, written in records.values())):
        raise ValueError('cursor stream coverage mismatch')
    for i, sample in enumerate(samples):
        if sample.get('seq') != i + 7 or sample.get('warmup') is not (i < warmup):
            raise ValueError('cursor stream order/warmup mismatch')


def complete_exits(rows, planned):
    counted = [r for r in rows if not r.get('warmup')]
    return bool(planned) and len(counted) == planned and all(
        r.get('cursor_exit_valid') is True and not r.get('error') and not r.get('killed')
        and not r.get('seq_mismatch') and r.get('cursor_exit_expected') == r.get('cursor_exit_count')
        and r.get('cursor_exit_measured_count', 0) > 0 for r in counted)
