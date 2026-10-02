"""Frozen control and publication contract. Local analysis, never app execution."""
from __future__ import annotations

import copy
import datetime
import hashlib
import json
import math
import re
import statistics
from pathlib import Path

CONTRACT = 'publication_v1'
HEX = re.compile(r'[0-9a-f]{64}')
TOKEN = re.compile(r'\{\{cell:([^|{}]+)\|([^{}]+)\}\}')
GLOBAL = ('harness_tree', 'hw_model', 'macos_build', 'display', 'fd_limit', 'activate')
TOOLS = {'startup': ('launch', 'stamp'), 'idle': ('launch', 'stamp', 'memsample'),
         'flood-memory': ('launch', 'stamp', 'memsample'),
         'vtebench': ('launch', 'stamp', 'vtebench'),
         'latency': ('launch', 'keyblock', 'latency-probe', 'observer'),
         'latency-cursor': ('launch', 'keyblock', 'latency-probe'),
         'output-memory': ('launch', 'printing', 'observer'),
         'blink-window': ('launch', 'stamp', 'observer')}


def strict_json(path, legacy=False):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('duplicate JSON field')
            result[key] = value
        return result
    def invalid(value):
        if legacy:
            return float(value)
        raise ValueError('nonfinite JSON value')
    result = json.loads(Path(path).read_text(), object_pairs_hook=pairs, parse_constant=invalid)
    if not legacy or modern(result):
        def finite(value):
            if isinstance(value, float) and not math.isfinite(value):
                raise ValueError('nonfinite frozen JSON value')
            if isinstance(value, dict):
                for item in value.values(): finite(item)
            elif isinstance(value, list):
                for item in value: finite(item)
        finite(result)
    return result


def json_value(value):
    if isinstance(value, float) and not math.isfinite(value):
        return 'nan' if math.isnan(value) else 'inf' if value > 0 else '-inf'
    if isinstance(value, dict):
        return {k: json_value(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [json_value(v) for v in value]
    return value


def canonical(value):
    return json.dumps(json_value(value), sort_keys=True, indent=2, allow_nan=False) + '\n'


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def number(value):
    return isinstance(value, (float, int)) and not isinstance(value, bool) and math.isfinite(value)


def public(value):
    """Refuse private strings rather than copying them into a public artifact."""
    if isinstance(value, dict):
        for key, item in value.items():
            public(key)
            public(item)
    elif isinstance(value, list):
        for item in value:
            public(item)
    elif isinstance(value, str) and (re.search(r'/(?:Users|home)/|[^\s]+@[^\s]+|(?:Developer ID|TeamIdentifier|Authority)=?', value)
                                   or '\x00' in value):
        raise ValueError('private data in public report input')
    return value


def modern(results):
    return results.get('evidence_contract') == 'hc-v1' or 'contracts' in (results.get('meta') or {})


def method(meta, workload):
    common = {'clock': 'CLOCK_UPTIME_RAW', 'grid': [120, 36], 'validity': 'hc-v1',
              'estimator_policy': meta.get('statistics_policy', 'current')}
    knobs = {'startup': ('warmup',), 'idle': ('idle_settle', 'idle_window'),
             'flood-memory': ('flood_offsets',), 'vtebench': ('vtebench_seconds', 'vtebench_unit')}
    common.update({k: meta.get(k) for k in knobs.get(workload, ())})
    if workload in ('latency', 'latency-cursor'):
        options = meta.get(workload) or {}
        cursor = workload == 'latency-cursor'
        common.update({k: options.get(k) for k in ('keys', 'warmup', 'censor_ms', 'inject', 'signed')})
        common.update(payload=options.get('payload', 'cursor' if cursor else 'block'),
                      gap_ms=options.get('gap_ms', [2000, 2400] if cursor else [100, 300]),
                      first_gap_ms=options.get('first_gap_ms', 2000 if cursor else 0),
                      probe_clock='mach_absolute_time_ns')
        if cursor:
            common.update(exit_logs=options.get('exit_logs', False),
                          exit_contract=options.get('exit_contract', 'cursor_exit_v1'))
        if not cursor:
            common['typing_memory'] = options.get('typing_memory')
    if workload in ('output-memory', 'blink-window'):
        options = meta.get('output_blink') or {}
        common.update(contract=options.get('contract'), sample_ms=options.get('sample_ms'))
        if workload == 'output-memory':
            common.update(payload_sha256=options.get('printing_payload_sha256'), period_ms=100,
                          duration_s=8, sample_offset_s=6, selection='first whole query at/after offset')
        else:
            common.update(settle_s=options.get('settle_s'), window_s=options.get('window_s'), origin='launch')
    if workload == 'flood-memory':
        common.update(sample_ms=100, selection='first at/after done offset')
    # Explicit producer extensions must be compared too; an adapter version is
    # not evidence that a collector or logging method has been calibrated.
    common['declared'] = (meta.get('contracts') or {}).get(workload)
    return common


def contracts(meta, workloads):
    # Exclude the declared extension from its own generated record.
    original = {k: v for k, v in meta.items() if k != 'contracts'}
    return {w: method(original, w) for w in workloads}


def identity_reasons(h, session, workload):
    meta = session['results'].get('meta') or {}
    reasons = []
    if not HEX.fullmatch(str(meta.get('harness_tree', ''))) or meta.get('harness_dirty') is not False:
        reasons.append('unverified harness identity')
    for key in ('hw_model', 'macos_build', 'display', 'fd_limit'):
        if not meta.get(key):
            reasons.append('missing ' + key)
    for name in session['names']:
        ident = (meta.get('identity') or {}).get(name) or {}
        if not HEX.fullmatch(str(ident.get('sha256', ''))):
            reasons.append('unverified terminal identity')
        closure = (meta.get('config_closures') or {}).get(name) or {}
        if not HEX.fullmatch(str(closure.get('sha256', ''))):
            reasons.append('unverified config closure')
    tools = meta.get('tool_hashes') or {}
    for tool in TOOLS.get(workload, ()):
        if not HEX.fullmatch(str(tools.get(tool, ''))):
            reasons.append('unverified ' + tool + ' artifact')
    if workload in ('latency', 'latency-cursor'):
        artifact = (meta.get('tool_artifacts') or {}).get('latency-probe')
        if not h.hc.typing_artifact_valid(artifact) or tools.get('latency-probe') != (artifact or {}).get('bundle_sha256'):
            reasons.append('unverified probe bundle')
    return sorted(set(reasons))


def match_reasons(h, control, reference, workload):
    a, b = control['results']['meta'], reference['results']['meta']
    reasons = identity_reasons(h, control, workload) + identity_reasons(h, reference, workload)
    reasons += ['changed ' + key for key in GLOBAL if a.get(key) != b.get(key)]
    if method(a, workload) != method(b, workload):
        reasons.append('changed metric method')
    for tool in TOOLS.get(workload, ()):
        if (a.get('tool_hashes') or {}).get(tool) != (b.get('tool_hashes') or {}).get(tool):
            reasons.append('changed ' + tool + ' artifact')
    if workload in ('latency', 'latency-cursor') and (a.get('tool_artifacts') or {}).get('latency-probe') != (b.get('tool_artifacts') or {}).get('latency-probe'):
        reasons.append('changed probe receipt')
    baseline = 'kettle-a' if reference['ab'] else 'kettle'
    if ((a.get('config_closures') or {}).get('kettle-a') != (b.get('config_closures') or {}).get(baseline)
            or (a.get('configs') or {}).get('kettle-a', '') != (b.get('configs') or {}).get(baseline, '')):
        reasons.append('baseline config mismatch')
    return sorted(set(reasons))


def analyses(h, session):
    return h.analyze(session['results'], session['names'], session['ab'])


def control_header(h, control):
    meta = control['results'].get('meta') or {}
    if not (control['countable'] and control['ab'] and control['names'] == ['kettle-a', 'kettle-b']
            and meta.get('kind', 'ordinary') == 'ordinary' and not (meta.get('latency-cursor') or {}).get('exit_logs')):
        raise SystemExit('control is not a complete ordinary A/A')
    if (control['setup']['identity'].get('kettle-a') != control['setup']['identity'].get('kettle-b')
            or control['configs'].get('kettle-a', '') != control['configs'].get('kettle-b', '')
            or (meta.get('config_closures') or {}).get('kettle-a') != (meta.get('config_closures') or {}).get('kettle-b')):
        raise SystemExit('A/A sides differ in build or config closure')


def coverage(h, sessions, aa, check_campaign=True):
    analyzed = [(s, analyses(h, s)) for s in sessions]
    metrics = {entry['descriptor']['id']: entry['descriptor'] for _, a in analyzed
               for info in a.values() for entry in info['metrics'].values()}
    control = h.load_session(Path(aa)) if aa else None
    controls = {}
    if control:
        control_header(h, control)
        controls = {e['descriptor']['id']: e for info in analyses(h, control).values() for e in info['metrics'].values()}
    report = {}
    for key, descriptor in sorted(metrics.items()):
        workload, field = key.split('.', 1)
        item = {'metric': key, 'unit': descriptor['unit'], 'aa_kind': descriptor['aa_kind'],
                'status': 'unavailable', 'reason': 'diagnostic or distribution only',
                'control': None, 'method': None, 'pairs': 0, 'planned': 0, 'gate': None}
        report[key] = item
        if descriptor['aa_kind'] == 'none':
            continue
        item.update(status='missing', reason='A/A missing')
        if not control or key not in controls:
            continue
        entry = controls[key]
        planned = control['rounds'].get(workload, 0)
        item.update(control=control['dir'], planned=planned, method=method(control['results']['meta'], workload))
        references = [s for s, a in analyzed if field in a.get(workload, {}).get('metrics', {})]
        reasons = sorted({r for s in references for r in match_reasons(h, control, s, workload)})
        stats = entry.get('ab') or {}
        pairs = stats.get('n', 0)
        item['pairs'] = pairs
        local = entry.get('metric_countable') or {}
        if not planned or pairs != planned or not all(local.get(n, {}).get('countable') for n in control['names']):
            reasons.append('incomplete control pairs')
        if workload in ('latency', 'latency-cursor'):
            options = control['results']['meta'].get(workload) or {}
            runs = control['results']['workloads'][workload]
            if any(len(h.latency_keys(r, options.get('censor_ms', 500)) or []) != options.get('keys')
                   for rows in runs.values() for r in rows if not r.get('warmup')):
                reasons.append('incomplete control keys')
        if reasons:
            item['reason'] = 'A/A missing: ' + ', '.join(sorted(set(reasons)))
            continue
        if descriptor['aa_kind'] == 'latency-difference':
            gate = h.latency_aa_gate(stats)
            gate['contains_one'] = gate['contains_one'] and abs(stats['diff']) <= 1
            interval = [stats['diff_low'], stats['diff_high']]
        else:
            values = entry['values']
            pairs_raw = [(x, y) for x, y in zip(values['kettle-a'], values['kettle-b']) if x is not None and y is not None]
            # Equal zeros have no measured logarithmic spread. A finite [1,1]
            # fallback cannot calibrate a future positive rate.
            if all(x == 0 and y == 0 for x, y in pairs_raw) or not all(number(stats.get(k)) for k in ('ratio', 'low', 'high')):
                item.update(status='uncalibrated', reason='unbounded or all-zero ratio control')
                continue
            gate = h.aa_gate(stats)
            interval = [stats['low'], stats['high']]
        item.update(status='passed' if gate['contains_one'] else 'failed', reason=None if gate['contains_one'] else 'A/A failed',
                    gate=gate, interval=interval, difference=entry.get('ab_diff'),
                    statistics=entry['statistics']['current'])
    failures = []
    if control and check_campaign:
        full = coverage(h, [control], aa, check_campaign=False)
        failures = sorted(k for k, v in full['metrics'].items() if v['status'] == 'failed')
        if failures:
            for item in report.values():
                if item['aa_kind'] != 'none' and item['status'] == 'passed':
                    item.update(status='failed', reason='ordinary A/A campaign failed')
    return public({'schema': 1, 'contract': CONTRACT, 'metrics': report, 'campaign_failures': failures,
                   'capabilities': {'native_pty': 'unavailable on legacy control',
                                    'cursor_exit_v1': 'unavailable on legacy control',
                                    'renderer_trace': 'producer agreement required'}})


def selected(items, limit):
    first = {}
    def instant(value):
        moment = datetime.datetime.fromisoformat(value.get('started') or value['date'])
        return moment if moment.tzinfo else moment.replace(tzinfo=datetime.timezone.utc)
    for item in sorted(items, key=lambda v: (instant(v), v['date'], v['label'])):
        if item.get('countable') and item['date'] not in first:
            first[item['date']] = item
    return list(first.values())[:limit]


def cursor_frames(h, sessions):
    result = {}
    for name in sorted({n for s in sessions for n in s['results']['workloads'].get('latency-cursor', {})}):
        values, reasons = [], []
        source = []
        for s in sessions:
            rows = s['results']['workloads'].get('latency-cursor', {}).get(name, [])
            if not rows:
                continue
            source.append(s['dir'])
            planned = s['rounds'].get('latency-cursor', 0)
            measured = [r for r in rows if not r.get('warmup')]
            if not planned or len(measured) != planned:
                reasons.append('incomplete exit rounds')
            for row in measured:
                records = row.get('cursor_exit_records') or []
                count, n = row.get('cursor_exit_count'), row.get('cursor_exit_measured_count', 0)
                if not row.get('cursor_exit_available'):
                    reasons.append('exit capability unavailable')
                elif (row.get('cursor_exit_valid') is not True or row.get('error') or row.get('killed')
                      or row.get('seq_mismatch') or count != row.get('cursor_exit_expected')
                      or count != len(records) or n != (s['results']['meta'].get('latency-cursor') or {}).get('keys')
                      or not 0 < n <= len(records) or row.get('cursor_exit_capability') != 'cursor_exit_v1'):
                    reasons.append('incomplete exit stream')
                else:
                    durations = [r.get('total_frame_us') for r in records[-n:]]
                    if not all(number(v) and v >= 0 for v in durations):
                        reasons.append('malformed exit duration')
                    else:
                        values.extend(durations)
        complete = bool(values) and not reasons
        result[name] = {'unit': 'us', 'capability': 'cursor_exit_v1', 'status': 'supported' if complete else 'unavailable',
                        'reasons': sorted(set(reasons)), 'calibrated_by_legacy_control': False, 'source_sessions': source,
                        'n': len(values), 'p50_us': h.percentile(values, .5) if complete else None,
                        'p95_us': h.percentile(values, .95) if complete else None,
                        'max_us': max(values) if complete else None,
                        'p95_le_4000': h.percentile(values, .95) <= 4000 if complete else None}
    return result


def combine(h, folders, aa, legacy):
    sessions = [h.load_session(Path(p)) for p in folders]
    candidates = sessions + ([h.load_session(Path(aa))] if aa else [])
    if any(s['results'].get('meta', {}).get('kind') == 'observer-pilot' for s in candidates):
        raise SystemExit('observer pilot (diagnostic) cannot enter --combine or --aa')
    if not any(modern(s['results']) for s in sessions):
        return legacy(folders, aa)
    if not all(modern(s['results']) for s in sessions):
        raise SystemExit('publication sets cannot mix frozen and historical methods')
    if len({s['label'] for s in sessions}) != len(sessions) or len({s['dir'] for s in sessions}) != len(sessions):
        raise SystemExit('publication session IDs must be unique')
    for s in sessions:
        public({'label': s['label'], 'dir': s['dir'], 'date': s['date']})
    # Preserve the existing strict across-date set comparison.
    combined = legacy(folders)
    report = coverage(h, sessions, aa)
    analyzed = [(s, analyses(h, s)) for s in sessions]
    ab = sessions[0]['ab']
    for s, analysis in analyzed:
        for workload, info in analysis.items():
            for field, entry in info['metrics'].items():
                key, descriptor = entry['descriptor']['id'], entry['descriptor']
                row = combined['rows'].setdefault(key, {'terminals': {}, 'sessions': [], 'per_session': []})
                row['descriptor'] = descriptor
                row['terminals'] = {}  # Only the first three/two eligible dates contribute.
                if not any(per['label'] == s['label'] for per in row['per_session']):
                    row['per_session'].append({'label': s['label'], 'estimates': {n: t['estimate'] for n, t in entry['terminals'].items()},
                                               'metric_countable': entry['metric_countable'], 'statistics': entry['statistics']})
                per = next(p for p in row['per_session'] if p['label'] == s['label'])
                per.update(date=s['date'], started=s['started'], source_session=s['dir'])
                reasons = identity_reasons(h, s, workload)
                for name, local in per['metric_countable'].items():
                    local = copy.deepcopy(local)
                    if descriptor['kind'] in ('latency', 'distribution'):
                        rows = s['results']['workloads'][workload].get(name, [])
                        valid = [h.latency_keys(r, (s['results']['meta'].get(workload) or {}).get('censor_ms', 500))
                                 for r in rows if not r.get('warmup')]
                        options = s['results']['meta'].get(workload) or {}
                        if (len(valid) != s['rounds'].get(workload) or not valid
                                or any(v is None or len(v) != options.get('keys') for v in valid)):
                            local['reasons'].append('incomplete publication keys/launches')
                        if name in s['names'] and not h.latency_standing(rows, options.get('censor_ms', 500), s['rounds'].get(workload, 0))['ranked']:
                            local['reasons'].append('latency censor/frame validity failed')
                        local['keys'] = sum(len(v) for v in valid if v)
                        local['launches'] = sum(v is not None for v in valid)
                    local['reasons'] = sorted(set(local['reasons'] + reasons))
                    local['countable'] = local['countable'] and not local['reasons']
                    per['metric_countable'][name] = local
                for comparison in row['sessions']:
                    if comparison['label'] == s['label']:
                        compared = s['names'] if ab else [s['names'][0], comparison.get('peer')]
                        comparison['countable'] = comparison['countable'] and all(per['metric_countable'].get(n, {}).get('countable') for n in compared)
                        comparison['statistics'] = per['statistics']
    for key, row in combined['rows'].items():
        descriptor = row['descriptor']
        for name in sorted({n for p in row['per_session'] for n in p['estimates']}):
            eligible = [dict(p, countable=p['metric_countable'].get(name, {}).get('countable', False)) for p in row['per_session']]
            chosen = selected(eligible, 2 if ab else 3)
            if chosen:
                values = [p['estimates'][name] for p in chosen]
                row['terminals'][name] = {'estimates': values, 'published': statistics.median(values),
                    'min': min(values), 'max': max(values), 'source_sessions': [p['source_session'] for p in chosen],
                    'dates': [p['date'] for p in chosen], 'n': [p['metric_countable'][name]['n'] for p in chosen],
                    'keys': [p['metric_countable'][name].get('keys') for p in chosen]}
        control = report['metrics'][key]
        row['coverage'] = control
        row.pop('aa', None)
        row.pop('claim', None)
        row.pop('verdict', None)
        if descriptor['aa_kind'] == 'none':
            row['verdict'] = {'verdict': 'diagnostic only'}
        elif control['status'] != 'passed':
            reason = 'A/A failed' if control['status'] == 'failed' else 'A/A missing'
            row['verdict'] = {'verdict': reason}
            if not ab:
                row['claim'] = {'label': reason}
        elif ab:
            row['aa'] = control['gate']
            row['verdict'] = (h.latency_ab_verdict(row['sessions'], control['gate']['gate_ms'])
                if descriptor['aa_kind'] == 'latency-difference' else h.ab_verdict(row['sessions'], control['gate']['gate']))
        elif row['sessions'] and descriptor['publication_role'] == 'standing':
            row['aa'] = control['gate']
            row['claim'] = h.claim(row['sessions'])
        # Missing controls retain descriptive per-session values but no public
        # headline. A filled template cannot resurrect this descriptive value.
        if control['status'] not in ('passed',) and descriptor['aa_kind'] != 'none':
            for terminal in row['terminals'].values():
                terminal.pop('published', None)
    combined.update(schema=3, publication_contract=CONTRACT, aa_coverage=report, mode='ab' if ab else 'standing',
                    cursor_exit_frames=cursor_frames(h, sessions))
    view = copy.deepcopy(combined)
    view.pop('cursor_exit_frames', None)
    for row in view['rows'].values():
        row['terminals'] = {n: t for n, t in row['terminals'].items() if 'published' in t}
    combined['markdown'] = h.combined_markdown(view, ab)
    combined['markdown'] += '\n## Shared control coverage\n\n| metric | unit | status | pairs | reason |\n|---|---|---|---:|---|\n'
    for key, item in report['metrics'].items():
        combined['markdown'] += f"| {key} | {item['unit']} | {item['status']} | {item['pairs']}/{item['planned']} | {item['reason'] or '-'} |\n"
    for name, frames in combined['cursor_exit_frames'].items():
        combined['markdown'] += f"\n{name} complete exit frames: {frames['p95_us']} us p95, n={frames['n']}; {frames['status']}. Legacy A/A does not calibrate app logging.\n"
    return public(combined)


PILOT_BOUNDS = {'typing': {'mean_ms': [-1., 1.]},
                'printing': {'printing_mib': [-.5, .5]},
                'blink': {'cpu_percent': [-.01, .01], 'wakeups_per_second': [-.1, .1]}}
PILOT_WORKLOADS = {'typing': 'latency', 'printing': 'output-memory', 'blink': 'blink-window'}
PILOT_INVALID_REASONS = frozenset({
    'designated printing query missing or late', 'known focus change during interval',
    'window not visible during interval', 'printing window not visible', 'designated query lost focus',
    'blink window not visible', 'blink boundary missing or late', 'readiness missed launch boundary',
    'native query late', 'native query failed or target exited', 'native focus notification overflow',
    'process lifetime identity missing or changed', 'stale focus check', 'nonmonotonic native trace',
    'active blink unproven', 'active blink disabled-default', 'off-arm query after done missing'})
# Equivalence is the two one-sided tests at 5 % each: the paired Student-t
# 90 % interval must sit inside the bounds. Up to 5 % of the planned pairs may
# be invalid, and only for the desktop's reasons below (focus moving, a window
# covering the measured one, input from outside the harness), never for the
# observer's or the terminal's; every invalid pair stays in the report.
PILOT_LEVEL = .90
PILOT_INVALID_SHARE = .05
PILOT_DESKTOP_REASONS = frozenset({
    'known focus change during interval', 'window not visible during interval', 'printing window not visible',
    'designated query lost focus', 'blink window not visible', 'typing window not visible',
    'probe saw focus, cover or foreign input'})
# The latency probe's own guards, as its failures name them.
PROBE_DESKTOP_FAILURE = re.compile(
    r'latency probe: (?:not frontmost(?:, and the titlebar is covered)?|focus changed before a key \(.*\)'
    r'|the measured window is not on screen|a window \(pid [0-9]+, layer -?[0-9]+\) covers the block'
    r'|foreign input|focus changed or a window covered the block during a sample)')
COST_FIELDS = ('cpu_ns', 'wakeups', 'query_count', 'query_duration_median_ms',
               'query_duration_max_ms', 'deadline_lateness_max_ms',
               'target_cpu_delta_ns', 'target_wakeups_delta')


def observer_pilot_report(h, results):
    meta = results.get('meta') or {}
    declaration = meta.get('observer_pilot') or {}
    kind, planned = declaration.get('kind'), declaration.get('pairs')
    if (kind not in PILOT_BOUNDS or type(planned) is not int or planned < 2
            or declaration.get('bounds') != PILOT_BOUNDS[kind]
            or set(results.get('workloads', {})) != {PILOT_WORKLOADS[kind]}):
        raise ValueError('observer pilot declaration invalid')
    workload, bounds = PILOT_WORKLOADS[kind], PILOT_BOUNDS[kind]
    terminals = results.get('terminals')
    rows = results['workloads'][workload]
    if (not isinstance(terminals, list) or not terminals or len(set(terminals)) != len(terminals)
            or set(rows) != set(terminals)):
        raise ValueError('observer pilot terminals invalid')
    public(terminals)
    reports = {}
    for terminal in terminals:
        attempts = rows[terminal]
        if not isinstance(attempts, list):
            raise ValueError('observer pilot attempts invalid')
        grouped = {i: {'on': [], 'off': []} for i in range(planned)}
        for row in attempts:
            if not isinstance(row, dict):
                raise ValueError('observer pilot attempt invalid')
            pair, arm = row.get('observer_pair'), row.get('observer_arm')
            if type(pair) is not int or pair not in grouped or arm not in ('on', 'off'):
                raise ValueError('observer pilot pair identity invalid')
            grouped[pair][arm].append(row)
        metrics = {}
        for field, (low_bound, high_bound) in bounds.items():
            values = {'on': [], 'off': []}
            invalid = {}
            for pair, arms in grouped.items():
                # Each arm keeps its first reason. A pair counts against the
                # desktop allowance only if neither arm failed otherwise.
                reasons = []
                selected = {}
                for arm in ('on', 'off'):
                    reason = None
                    launches = arms[arm]
                    if len(launches) != 1:
                        reasons.append('missing arm' if not launches else 'duplicate arm')
                        continue
                    row = launches[0]
                    expected_order = int(arm != ('on' if pair % 2 == 0 else 'off'))
                    if type(row.get('observer_order')) is not int or row['observer_order'] != expected_order:
                        reason = reason or 'invalid arm order'
                    if row.get('killed') or row.get('warmup') or row.get('seq_mismatch'):
                        reason = reason or 'failed arm'
                    elif 'error' in row:
                        desktop = (kind == 'typing' and isinstance(row['error'], str)
                                   and PROBE_DESKTOP_FAILURE.fullmatch(row['error']))
                        reason = reason or ('probe saw focus, cover or foreign input' if desktop else 'failed arm')
                    if kind == 'typing':
                        options = meta.get('latency') or {}
                        keys = h.latency_keys(row, options.get('censor_ms', 500))
                        value = statistics.mean(keys) if keys else None
                        if keys is None or len(keys) != options.get('keys'):
                            reason = reason or 'incomplete typing keys'
                        # An on arm counts only if its observer ran through
                        # the whole typing epoch, not just its readiness query.
                        # Its focus checks can see the desktop interrupt between
                        # the probe's own; only those reasons keep their name.
                        if arm == 'on' and row.get('typing_memory_valid') is not True:
                            observed = row.get('typing_memory_reason')
                            reason = reason or (observed if observed in PILOT_DESKTOP_REASONS
                                                else 'on-arm observer evidence invalid')
                    else:
                        evidence = (row.get('metric_validity') or {}).get(field) or {}
                        value = row.get(field)
                        failure = h.metric_reason(h.metric_descriptor(workload, field), workload, row)
                        if evidence.get('valid') is not True or failure is not None:
                            reason = reason or (failure if failure in PILOT_INVALID_REASONS else 'invalid metric evidence')
                    if not number(value) or value < 0:
                        reason = reason or 'metric unavailable'
                    selected[arm] = value
                    if reason:
                        reasons.append(reason)
                if reasons:
                    reason = next((r for r in reasons if r not in PILOT_DESKTOP_REASONS), reasons[0])
                    invalid[reason] = invalid.get(reason, 0) + 1
                    continue
                for arm in ('on', 'off'):
                    values[arm].append(selected[arm])
            n = len(values['on'])
            difference = h.paired_difference(values['off'], values['on'], PILOT_LEVEL) if n >= 2 else {}
            allowed = math.floor(PILOT_INVALID_SHARE * planned + 1e-9)
            if meta.get('complete') is not True:
                reason = 'pilot incomplete'
            elif any(r not in PILOT_DESKTOP_REASONS for r in invalid):
                reason = 'invalid pairs not caused by the desktop'
            elif planned - n > allowed:
                reason = 'insufficient valid pairs'
            elif not (difference and low_bound <= difference['low'] <= difference['high'] <= high_bound):
                reason = 'interval outside equivalence bounds'
            else:
                reason = None
            metrics[field] = {'bounds': [low_bound, high_bound], 'valid_pairs': n,
                'allowed_invalid_pairs': allowed, 'invalid_pairs_by_reason': invalid, 'difference': difference,
                'arm_medians': {arm: statistics.median(v) if v else None for arm, v in values.items()},
                'equivalent': reason is None, 'reason': reason}
        costs = {}
        for arm in ('on', 'off'):
            costs[arm] = {}
            for field in COST_FIELDS:
                v = [(r.get('observer_cost') or {}).get(field) for r in attempts if r.get('observer_arm') == arm]
                v = [x for x in v if number(x) and x >= 0]
                costs[arm][field] = {'n': len(v), 'median': statistics.median(v) if v else None,
                                     'max': max(v) if v else None}
        reports[terminal] = {'metrics': metrics, 'observer_cost': costs,
                             'equivalent': all(m['equivalent'] for m in metrics.values())}
    return public({'schema': 1, 'kind': 'observer-pilot', 'countable': False,
                   'pilot': kind, 'pairs': planned,
                   'interval_policy': 'TOST at 5% each side: paired Student-t 90% inside the bounds',
                   'terminals': reports, 'equivalent': all(r['equivalent'] for r in reports.values())})


def observer_control(h, folder):
    session = h.load_session(Path(folder))
    meta = session['results'].get('meta') or {}
    if meta.get('kind') == 'observer-pilot':
        return observer_pilot_report(h, session['results'])
    if (meta.get('kind') != 'observer-control' or meta.get('startup_phases') != 'b'
            or not session['ab'] or session['names'] != ['kettle-a', 'kettle-b']
            or set(session['results']['workloads']) != {'startup'} or meta.get('rounds', {}).get('startup') != 30
            or meta.get('refusals') or meta.get('bare') or meta.get('footprint_detail') or meta.get('complete') is not True
            or not h.rounds_complete(session['results'], meta) or identity_reasons(h, session, 'startup')
            or session['setup']['identity'].get('kettle-a') != session['setup']['identity'].get('kettle-b')
            or (meta.get('config_closures') or {}).get('kettle-a') != (meta.get('config_closures') or {}).get('kettle-b')
            or session['configs'].get('kettle-a', '') != session['configs'].get('kettle-b', '')):
        raise ValueError('not a verified complete stamp observer control')
    rows = session['results']['workloads']['startup']
    measured_b = [r for r in rows['kettle-b'] if not r.get('warmup')]
    measured_a = [r for r in rows['kettle-a'] if not r.get('warmup')]
    required = {'main', 'run_with', 'event_loop_built', 'config_loaded', 'app_built', 'pane_spawned', 'resumed', 'window_created', 'gpu_ready', 'window_revealed', 'first_frame'}
    # Intervals the parser derives from ordered endpoints; attribution needs both.
    ordered = ('event_loop_build_ms', 'gpu_init_ms')

    def stamped(r):
        stamps = r.get('startup_stamps_ns') or {}
        evidence = r.get('startup_stamp_evidence')
        if not isinstance(evidence, dict) or not required <= set(stamps):
            return False
        # The retained parser output must describe these very stamps, each a
        # real (positive) clock reading.
        if (evidence.get('startup_stamps_ns') or {}) != stamps or not all(
                h.evidence_uint(stamps[key]) and stamps[key] > 0 for key in required):
            return False
        if evidence.get('malformed_lines') or evidence.get('invalid_phases'):
            return False
        validity = evidence.get('metric_validity') or {}
        return all((validity.get(field) or {}).get('state') == 'supported' for field in ordered)

    if (any(r.get('startup_stamps_ns') or any(k.startswith('phase_') for k in r) for r in measured_a)
            or not all(stamped(r) for r in measured_b)):
        raise ValueError('stamp on/off evidence incomplete')
    metrics = analyses(h, session)['startup']['metrics']
    reports = {}
    for field in ('child_ms', 'window_ms'):
        entry = metrics[field]
        ratio, diff = entry.get('ab') or {}, entry.get('ab_diff') or {}
        valid = ratio.get('n') == 30 and diff.get('n') == 30
        equivalent = valid and .97 <= ratio['low'] <= ratio['high'] <= 1.03 and -1 <= diff['low'] <= diff['high'] <= 1
        reports[field] = {'unit': 'ms', 'ratio': ratio, 'difference': diff, 'equivalent': equivalent,
                          'difference_bounds_ms': [-1, 1], 'ratio_bounds': [.97, 1.03]}
    return public({'schema': 1, 'kind': 'observer-control', 'countable': False,
                   'phase_attribution_allowed': all(r['equivalent'] for r in reports.values()), 'metrics': reports})


def display(value, unit):
    if isinstance(value, str):
        return value
    digits = 4 if unit == 'percentage points' else 3 if unit == '/s' else 2 if unit == 'ratio' else 1
    return f'{value:.{digits}f}'


def publication_values(combined):
    if combined.get('publication_contract') != CONTRACT:
        raise ValueError('publication requires the frozen schema and verified controls')
    cells = []
    for metric, row in sorted(combined['rows'].items()):
        d = row['descriptor']
        coverage = row['coverage']
        for terminal, values in sorted(row['terminals'].items()):
            enough = len(values.get('dates', [])) >= (2 if combined['mode'] == 'ab' else 3)
            eligible = coverage['status'] == 'passed' and enough and 'published' in values
            if d['kind'] == 'distribution':
                parent = combined['rows'].get(metric.split('.')[0] + '.mean_ms', {})
                eligible = enough and 'published' in values and parent.get('coverage', {}).get('status') == 'passed'
            elif d['aa_kind'] == 'none':
                eligible = False
            for field in ('published', 'min', 'max'):
                value = values.get(field) if eligible else None
                cells.append({'id': f'{metric}:{terminal}:{field}', 'unit': d['unit'],
                    'source_metric': metric, 'source_sessions': values['source_sessions'], 'estimator': d['estimate'],
                    'value': value, 'display': display(value, d['unit']) if value is not None else 'not measured',
                    'claim': row.get('claim', {}).get('label'), 'rank': row.get('claim', {}).get('rank'),
                    'n': values['n'], 'keys': values['keys'], 'status': 'available' if eligible else 'unavailable',
                    'caveat': None if eligible else coverage.get('reason') or 'insufficient publication dates',
                    'claim_kind': d['claim_kind'], 'calibration_metric': metric.split('.')[0] + '.mean_ms' if d['kind'] == 'distribution' else metric})
        label = (row.get('claim') or {}).get('label')
        if label:
            eligible = coverage['status'] == 'passed' and label not in ('insufficient sessions', 'A/A missing', 'A/A failed')
            cells.append({'id': f'{metric}:claim:label', 'unit': 'label', 'source_metric': metric,
                'source_sessions': [s['label'] for s in selected(row['sessions'], 3)],
                'estimator': 'first three dates; current CI and 80 percent wins', 'value': label if eligible else None,
                'display': label if eligible else 'not measured', 'claim': label, 'rank': None, 'n': [], 'keys': [],
                'status': 'available' if eligible else 'unavailable', 'caveat': None if eligible else 'insufficient calibrated claim'})
        verdict = row.get('verdict') or {}
        for field, unit in (('headline', 'ratio'), ('headline_ms', 'ms')):
            if field in verdict:
                value = verdict[field]
                cells.append({'id': f'{metric}:ab:{field}', 'unit': unit, 'source_metric': metric,
                    'source_sessions': [s['label'] for s in selected(row['sessions'], 2)],
                    'estimator': d['comparison'], 'value': value, 'display': display(value, unit),
                    'claim': verdict['verdict'], 'rank': None, 'n': [s['n'] for s in selected(row['sessions'], 2)],
                    'keys': [], 'status': 'available', 'caveat': None})
    for terminal, frames in sorted(combined['cursor_exit_frames'].items()):
        for field in ('p50_us', 'p95_us', 'max_us'):
            cells.append({'id': f'latency-cursor.exit_frame:{terminal}:{field}', 'unit': 'us',
                'source_metric': 'latency-cursor.exit_frame', 'source_sessions': frames['source_sessions'],
                'estimator': 'pooled measured complete exit frames', 'value': frames[field],
                'display': display(frames[field], 'us') if frames[field] is not None else 'not measured',
                'claim': None, 'rank': None, 'n': frames['n'], 'keys': frames['n'],
                'status': 'diagnostic' if frames['status'] == 'supported' else 'unavailable',
                'caveat': 'app logging requires separate observer validation; external cursor control cannot calibrate exit frames'})
    return public({'schema': 1, 'contract': CONTRACT, 'combined_sha256': digest({k: v for k, v in combined.items() if k != 'markdown'}),
                   'cells': cells})


def fill(template, values):
    if values.get('contract') != CONTRACT or values.get('schema') != 1:
        raise ValueError('unsupported publication values')
    cells = {}
    for cell in values['cells']:
        if cell['id'] in cells:
            raise ValueError('duplicate cell ID')
        cells[cell['id']] = cell
    seen = set()
    def replace(match):
        key, unit = match.groups()
        if key in seen:
            raise ValueError('duplicate template ID')
        seen.add(key)
        if key not in cells:
            raise ValueError('unknown cell ID')
        cell = cells[key]
        if cell['unit'] != unit:
            raise ValueError('cell unit mismatch')
        if cell['status'] != 'available' or cell['value'] is None or cell.get('caveat'):
            raise ValueError('required cell unavailable')
        if cell['display'] != display(cell['value'], unit):
            raise ValueError('cell display mismatch')
        return cell['display']
    rendered = TOKEN.sub(replace, template)
    if '{{' in rendered or '}}' in rendered:
        raise ValueError('unresolved template token')
    return public(rendered)
