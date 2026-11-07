#!/usr/bin/env python3
"""Portable behavioral tests; no compositor, GPU, D-Bus or Kettle is executed."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


capture = load('capture_for_wayland_test', 'capture-ui-states.py')
RUNNER_PATH = Path(__file__).with_name('run-ui-states-wayland.py')
runner = load('wayland_runner_test', RUNNER_PATH.name) if RUNNER_PATH.exists() else None


@contextlib.contextmanager
def proc_child_inventory(text):
    original_open = Path.open
    def open_path(path, *args, **kwargs):
        if path == Path('/proc/777/task/777/children'):
            return io.BytesIO(text.encode())
        return original_open(path, *args, **kwargs)
    with patch.object(Path, 'open', open_path):
        yield


class EnvironmentTests(unittest.TestCase):
    def test_fixture_keeps_private_socket_after_runtime_override_without_owner_inheritance(self):
        inherited = {'DISPLAY': ':owner', 'WAYLAND_DISPLAY': 'wayland-0',
                     'WAYLAND_SOCKET': '42', 'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/owner/bus',
                     'DBUS_SYSTEM_BUS_ADDRESS': 'unix:path=/owner/system-bus',
                     'KETTLE_CONFIG': '/owner/config',
                     'KETTLE_SMOKE_EXTRA_CONFIG': 'agent-server = full',
                     'SSH_AUTH_SOCK': '/owner/agent', 'BASH_ENV': '/owner/bashrc',
                     'FONTCONFIG_FILE': '/owner/fonts.conf', 'LD_PRELOAD': '/owner/inject.so'}
        with tempfile.TemporaryDirectory() as raw, patch.dict(os.environ, inherited, clear=True):
            root = Path(raw)
            home = root / 'fixture'
            home.mkdir(mode=0o700)
            # The old route applied only private_env to the inherited Xvfb environment.
            if runner is None:
                environment = dict(os.environ)
            else:
                environment = runner.make_environment(capture, root)
            for key, value in capture.private_env(home).items():
                if value is None:
                    environment.pop(key, None)
                else:
                    environment[key] = value
            for name in ('DISPLAY', 'WAYLAND_SOCKET', 'KETTLE_CONFIG',
                         'KETTLE_SMOKE_EXTRA_CONFIG', 'SSH_AUTH_SOCK', 'BASH_ENV', 'LD_PRELOAD'):
                with self.subTest(removed=name):
                    self.assertNotIn(name, environment)
            self.assertTrue(Path(environment['WAYLAND_DISPLAY']).is_absolute())
            self.assertNotEqual(Path(environment['WAYLAND_DISPLAY']).parent,
                                Path(environment['XDG_RUNTIME_DIR']))
            self.assertNotEqual(environment['FONTCONFIG_FILE'], '/owner/fonts.conf')
            self.assertNotIn('/owner/', environment['DBUS_SESSION_BUS_ADDRESS'])
            self.assertEqual(environment['DBUS_SYSTEM_BUS_ADDRESS'], environment['DBUS_SESSION_BUS_ADDRESS'])
            for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                        'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
                self.assertTrue(Path(environment[key]).is_dir())
                if os.name != 'nt':
                    self.assertEqual(Path(environment[key]).stat().st_mode & 0o777, 0o700)

ADAPTER = 'llvmpipe (LLVM 18.1.8, 256 bits)'
GPU = f'''Backend policy: GL
Backend: Gl
Backend fallback: no
Adapter: {ADAPTER}
Adapter type: Cpu
Driver: llvmpipe
Driver info: OpenGL ES 3.2 Mesa 24.0.9
'''
WESTON = f'''Loading module '/usr/lib/x86_64-linux-gnu/libweston-13/headless-backend.so'
Loading module '/usr/lib/x86_64-linux-gnu/libweston-13/gl-renderer.so'
GL version: OpenGL ES 3.2 Mesa 24.0.9
GL renderer: {ADAPTER}
'''


class FakeProcess:
    def __init__(self):
        self.pid = 321
        self.returncode = None
        self.signals = []
        self.waits = []
        self.timeouts = 0

    def poll(self):
        return self.returncode

    def wait(self, timeout):
        self.waits.append(timeout)
        if self.timeouts:
            self.timeouts -= 1
            raise subprocess.TimeoutExpired(['fixture'], timeout)
        self.returncode = 0
        return 0


class FakeHandle:
    identity = (321, 12345)

    def __init__(self, matches=True):
        self.matches = matches
        self.signals = []
        self.closed = False

    def matches_current(self):
        return self.matches

    def signal(self, number):
        self.signals.append(number)
        return True

    def close(self):
        self.closed = True


class FakeNative:
    def __init__(self, directory):
        self.directory = directory
        self.calls = []
        self.processes = []
        self.queries = {}
        self.cleaned = False
        self.clock = 0
        self.ready = True
        self.cleanup_error = False

    def system(self):
        return 'Linux'

    def now(self):
        return self.clock

    def sleep(self, seconds):
        self.clock += seconds

    def query(self, name, argv, timeout=20):
        self.calls.append((name, argv))
        if name in self.queries:
            return self.queries[name]
        if name == 'font-list':
            return ''.join(str(p.resolve()) + '\n' for p in (runner.ROOT / 'assets/fonts').rglob('*.ttf'))
        return {'source-head': 'a' * 40, 'source-diff': '', 'source-untracked': '',
                'gpu-info': GPU}.get(name, 'fixture version\n')

    def acquire_scope(self):
        self.calls.append(('scope', []))

    def start(self, name, argv):
        self.calls.append((name, argv))
        path = self.directory / (name + '.log')
        path.write_text(self.queries.get('weston-log', WESTON) if name == 'weston' else 'private bus\n')
        owned = SimpleNamespace(record={'name': name}, proc=FakeProcess(), path=path)
        self.processes.append(owned)
        return owned

    def socket_ready(self, path):
        self.calls.append(('socket', [str(path)]))
        return self.ready

    def remember_weston_children(self, weston):
        self.calls.append(('desktop-child', [weston.proc.pid]))
        return True

    def cleanup(self):
        self.cleaned = True
        if self.cleanup_error:
            raise RuntimeError('fixture cleanup failure')

    def drain_adopted_children(self):
        self.calls.append(('fixture-drain', []))


class SessionTests(unittest.TestCase):
    def session(self, configure=lambda native: None, live_adapter=ADAPTER, live_error=None,
                native_factory=FakeNative):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        session, diagnostics = root / 'private', root / 'diagnostics'
        session.mkdir(mode=0o700)
        diagnostics.mkdir(mode=0o700)
        kettle = root / 'kettle-fixture'
        kettle.write_bytes(b'fixture executable identity; never executed')
        native = native_factory(diagnostics)
        configure(native)
        events = []

        class FakeLive:
            def __init__(self, _kettle, cfg, log, extra_env=None):
                self.cfg, self.log, self.proc = cfg, log, FakeProcess()
                events.append(('environment', extra_env))

            def __enter__(self):
                events.append(('enter', self.cfg.read_text()))
                self.log.write_text(f'renderer: using software adapter {live_adapter} (GL)\n')
                if live_error == 'startup':
                    events.append(('cleanup-startup', None))
                    if hasattr(native, 'fixture_exited'):
                        native.fixture_exited()
                    raise RuntimeError('fixture startup failed')
                return self

            def screenshot(self, path):
                # A native screenshot would happen here. This fake never writes a PNG.
                events.append(('screenshot', path.name))
                if live_error == 'during-screenshot':
                    self.log.write_text('renderer: selected GPU Intel UHD (Integrated, GL)\n')

            def __exit__(self, *args):
                events.append(('cleanup', None))
                if live_error == 'cleanup-timeout' and not getattr(self, 'retried', False):
                    self.retried = True
                    raise runner.DeadlineExpired('deadline inside fixture cleanup')
                self.proc.returncode = 0
                if hasattr(native, 'fixture_exited'):
                    native.fixture_exited()

        smoke = SimpleNamespace(LiveKettle=FakeLive, live_shell_command=lambda *args, **kw: None)
        environment = runner.make_environment(capture, session)
        manifest = diagnostics / 'manifest.json'

        def fake_capture(smoke, binary, out, name, language, theme, size, geometry_only=False):
            cfg = session / 'fixture.cfg'
            cfg.write_text('record = false\n')
            with smoke.LiveKettle(binary, cfg, session / 'fixture.log',
                                  extra_env=capture.private_env(Path(environment['HOME']))) as live:
                if live_error == 'fontdb-fallback':
                    with live.log.open('a') as log:
                        log.write('Fallback to loading from known font dir paths.\n')
                if live_error == 'changed-adapter':
                    live.log.write_text('renderer: selected GPU Intel UHD (Integrated, GL)\n')
                if live_error == 'capture':
                    raise runner.DeadlineExpired('fixture capture deadline')
                live.screenshot(out / f'{name}.{language}.{theme}.{size}.fixture')
            return out / f'{name}.{language}.{theme}.{size}.fixture'

        argv = ['--out', str(root / 'captures'), '--languages', 'en,es',
                '--themes', 'dark,light', '--sizes', 'default,narrow']
        error = None
        before_env = dict(os.environ)
        before_argv = sys.argv
        with patch.object(capture, 'capture_state', side_effect=fake_capture), \
                patch.object(runner, 'bounded', lambda seconds: contextlib.nullcontext()), \
                patch.object(runner, 'file_limit', contextlib.nullcontext), \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                runner.run_session(capture, smoke, native, environment, session, diagnostics,
                                   argv, str(kettle), 1, 2)
            except RuntimeError as caught:
                error = caught
        self.assertEqual(dict(os.environ), before_env)
        self.assertIs(sys.argv, before_argv)
        self.assertTrue(native.cleaned)
        self.assertFalse(list(root.rglob('*.png')))
        return native, events, json.loads(manifest.read_text()), error, diagnostics

    def test_verified_session_reaches_all_80_fixtures_and_retains_native_provenance(self):
        native, events, manifest, error, diagnostics = self.session()
        self.assertIsNone(error)
        self.assertEqual(manifest['status'], 'passed')
        self.assertEqual(sum(event[0] == 'screenshot' for event in events), 80)
        self.assertEqual(sum(event[0] == 'cleanup' for event in events), 80)
        self.assertEqual(len(list(diagnostics.glob('fixture-*.log'))), 80)
        self.assertEqual(manifest['gpu']['Adapter'], ADAPTER)
        self.assertEqual(manifest['weston']['renderer'], ADAPTER)
        primary = {Path(p).name for p in manifest['font_files_sha256'] if Path(p).parent.name == 'fonts'}
        self.assertEqual(primary, {'JetBrainsMonoNerdFont-Regular.ttf', 'JetBrainsMonoNerdFont-Bold.ttf',
                                   'JetBrainsMonoNerdFont-Italic.ttf', 'JetBrainsMonoNerdFont-BoldItalic.ttf'})
        self.assertEqual(len(manifest['source']['binary_sha256']), 64)
        self.assertTrue(manifest['versions']['packages'])
        for event, value in events:
            if event == 'enter':
                self.assertIn('record = false', value)
                self.assertIn('gpu-force-software = true', value)
                self.assertNotIn('agent-server', value)
        weston = next(argv for name, argv in native.calls if name == 'weston')
        self.assertIn('--backend=headless-backend.so', weston)
        self.assertIn('--renderer=gl', weston)
        self.assertNotIn('--xwayland', weston)
        bus = next(argv for name, argv in native.calls if name == 'dbus')
        self.assertIn('--nofork', bus)

    def test_host_fonts_cannot_reach_capture_and_failure_keeps_diagnostics(self):
        def bad_fonts(native):
            native.queries['font-list'] = '/usr/share/fonts/owner-font.ttf\n'
        _, events, manifest, error, diagnostics = self.session(bad_fonts)
        self.assertIsNotNone(error)
        self.assertEqual(events, [])
        self.assertEqual(manifest['status'], 'failed')
        self.assertIn('font inventory', manifest['error'])
        self.assertTrue((diagnostics / 'manifest.json').exists())

    def test_actual_hardware_unknown_version_or_fallback_never_reaches_capture(self):
        for diagnostic in (GPU.replace(ADAPTER, 'Intel UHD'), GPU.replace('Cpu', 'IntegratedGpu'),
                           GPU.replace('Gl', 'Vulkan'), GPU.replace('fallback: no', 'fallback: yes'),
                           GPU.replace('OpenGL ES 3.2 Mesa 24.0.9', '<unknown>')):
            with self.subTest(diagnostic=diagnostic):
                def bad_gpu(native):
                    native.queries['gpu-info'] = diagnostic
                _, events, manifest, error, _ = self.session(bad_gpu)
                self.assertIsNotNone(error)
                self.assertEqual(events, [])
                self.assertEqual(manifest['status'], 'failed')

    def test_wrong_compositor_backend_renderer_or_version_never_reaches_capture(self):
        for log in (WESTON.replace('headless-backend', 'x11-backend'),
                    WESTON.replace(ADAPTER, 'Intel UHD'),
                    WESTON.replace('GL version:', 'unrelated:'),
                    'pixman renderer enabled\n', WESTON + "Loading module '/lib/drm-backend.so'\n"):
            with self.subTest(log=log):
                def bad_weston(native):
                    native.queries['weston-log'] = log
                _, events, manifest, error, diagnostics = self.session(bad_weston)
                self.assertIsNotNone(error)
                self.assertEqual(events, [])
                self.assertEqual(manifest['status'], 'failed')
                self.assertEqual((diagnostics / 'weston.log').read_text(), log)

    def test_live_adapter_mismatch_blocks_screenshot_but_runs_cleanup_and_retains_log(self):
        _, events, manifest, error, diagnostics = self.session(live_adapter='Intel UHD')
        self.assertIsNotNone(error)
        self.assertEqual(manifest['status'], 'failed')
        self.assertFalse(any(event == 'screenshot' for event, _ in events))
        self.assertTrue(any(event == 'cleanup' for event, _ in events))
        self.assertIn('Intel UHD', (diagnostics / 'fixture-000.log').read_text())

    def test_adapter_change_during_screenshot_fails_and_retains_diagnostics(self):
        _, events, manifest, error, diagnostics = self.session(live_error='during-screenshot')
        self.assertIsNotNone(error)
        self.assertEqual(manifest['status'], 'failed')
        self.assertEqual(sum(event == 'screenshot' for event, _ in events), 1)
        self.assertTrue(any(event == 'cleanup' for event, _ in events))
        self.assertIn('Intel UHD', (diagnostics / 'fixture-000.log').read_text())

    def test_startup_and_capture_exceptions_preserve_logs_and_unwind(self):
        for error_type in ('startup', 'capture', 'changed-adapter', 'cleanup-timeout', 'fontdb-fallback'):
            with self.subTest(error=error_type):
                _, events, manifest, error, diagnostics = self.session(live_error=error_type)
                self.assertIsNotNone(error)
                self.assertEqual(manifest['status'], 'failed')
                self.assertTrue(any(event.startswith('cleanup') for event, _ in events))
                self.assertTrue((diagnostics / 'fixture-000.log').exists())

    def test_startup_timeout_is_bounded_and_cleans_both_owned_processes(self):
        def never_ready(native):
            native.ready = False
        native, events, manifest, error, diagnostics = self.session(never_ready)
        self.assertIsNotNone(error)
        self.assertGreaterEqual(native.clock, 1)
        self.assertLess(native.clock, 1.1)
        self.assertEqual(events, [])
        self.assertIn('startup timed out', manifest['error'])
        self.assertTrue((diagnostics / 'weston.log').exists())
        self.assertEqual(len(native.processes), 2)

    def test_cleanup_failure_cannot_report_a_pass(self):
        def cannot_clean(native):
            native.cleanup_error = True
        _, _, manifest, error, _ = self.session(cannot_clean)
        self.assertIsNotNone(error)
        self.assertEqual(manifest['status'], 'failed')
        self.assertIn('cleanup failure', manifest['cleanup_error'])


class AdoptionKernel:
    """Synthetic process instances at the OS boundary, never host PID operations."""
    owner = 777

    def __init__(self):
        self.nodes, self.handles, self.waits, self.signals = {}, [], [], []
        self.next_pid = 10000
        self.snapshots = []
        self.peak_handles = 0
        self.clock = 0
        self.scope_closed = False

    def child(self, state='Z', parent=None):
        self.next_pid += 1
        pid = self.next_pid
        self.nodes[pid] = SimpleNamespace(pid=pid, identity=(pid, 12345), state=state,
                                          parent=self.owner if parent is None else parent,
                                          ignore_term=False, stubborn=False, descendants=[])
        return pid

    def open(self, pid):
        node = self.nodes[pid]
        kernel = self

        class Handle:
            def __init__(self):
                self.pid, self.identity, self.closed = pid, node.identity, False

            def matches_current(self):
                return pid in kernel.nodes and kernel.nodes[pid].identity == self.identity

            def signal(self, number):
                if not self.matches_current():
                    raise AssertionError('signal to a replacement process')
                kernel.signals.append((pid, number))
                if (node.state != 'Z' and not node.stubborn
                        and (not node.ignore_term or number == runner.signal.SIGKILL)):
                    node.state = 'Z'
                return True

            def close(self):
                self.closed = True

        handle = Handle()
        self.handles.append(handle)
        self.peak_handles = max(self.peak_handles, sum(not item.closed for item in self.handles))
        return handle

    def waitpid(self, pid, flags):
        assert flags == 1
        node = self.nodes[pid]
        assert node.parent == self.owner, 'wait for a foreign child'
        if node.state != 'Z':
            return 0, 0
        self.waits.append({'pid': pid, 'identity': node.identity, 'wait_status': 0})
        for descendant in node.descendants:
            self.nodes[descendant].parent = self.owner
        del self.nodes[pid]
        return pid, 0

    def children(self, path, *args, **kwargs):
        if path == Path(f'/proc/{self.owner}/task/{self.owner}/children'):
            pids = [pid for pid, node in self.nodes.items() if node.parent == self.owner]
            self.snapshots.append(pids)
            return ' '.join(map(str, pids))
        return self.original_read_text(path, *args, **kwargs)

    @contextlib.contextmanager
    def installed(self):
        self.original_read_text = Path.read_text
        original_open = Path.open
        def open_path(path, *args, **kwargs):
            if path == Path(f'/proc/{self.owner}/task/{self.owner}/children'):
                return io.BytesIO(self.children(path).encode())
            return original_open(path, *args, **kwargs)
        with patch.object(runner.os, 'getpid', return_value=self.owner), \
                patch.object(runner.os, 'waitpid', side_effect=self.waitpid, create=True), \
                patch.object(runner.os, 'WNOHANG', 1, create=True), \
                patch.object(Path, 'open', open_path), \
                patch.object(Path, 'read_text', lambda path, *args, **kw:
                             self.children(path, *args, **kw)):
            yield


class LifecycleNative(runner.Native):
    """Real lifecycle/cleanup with a portable kernel and native query boundary."""
    query = FakeNative.query
    socket_ready = FakeNative.socket_ready
    remember_weston_children = FakeNative.remember_weston_children
    system = staticmethod(lambda: 'Linux')

    def __init__(self, directory, kernel):
        smoke = SimpleNamespace(StableProcessHandle=SimpleNamespace(open=kernel.open),
                                _linux_process_parent=lambda pid: kernel.nodes[pid].parent)
        super().__init__(smoke, {}, directory)
        self.kernel = kernel
        self.calls, self.processes, self.queries = [], [], {}
        self.ready = True
        self.fixtures = 0
        self.fixture_pids = []
        self.burst = 3

    @property
    def cleaned(self):
        return self.kernel.scope_closed

    def now(self):
        return self.kernel.clock

    def sleep(self, seconds):
        self.kernel.clock += seconds

    def acquire_scope(self):
        baseline = {node.identity for node in self.kernel.nodes.values()}
        self.scope = SimpleNamespace(
            was_present_at_acquire=lambda handle: handle.identity in baseline,
            close=lambda: setattr(self.kernel, 'scope_closed', True))

    def start(self, name, argv):
        owned = FakeNative.start(self, name, argv)
        pid = self.kernel.child(state='S')
        owned.proc.pid = pid
        owned.handle = self.kernel.open(pid)
        owned.record.update(pid=pid, identity=owned.handle.identity)
        original_wait = owned.proc.wait

        def wait(timeout):
            result = original_wait(timeout)
            self.kernel.nodes.pop(pid, None)
            return result

        owned.proc.wait = wait
        self.records.append(owned.record)
        self.children.append(owned)
        return owned

    def ps(self, pid):
        return f'{pid} synthetic process\n' if pid in self.kernel.nodes else ''

    def fixture_exited(self):
        self.fixtures += 1
        self.fixture_pids.extend(self.kernel.child() for _ in range(self.burst))


class PortableLinuxSignals(unittest.TestCase):
    """Linux signal numbers belong to the synthetic kernel, on every host."""

    def setUp(self):
        for name, number in (('SIGTERM', 15), ('SIGKILL', 9)):
            constant = patch.object(runner.signal, name, number, create=True)
            constant.start()
            self.addCleanup(constant.stop)


class NativeLifecycleTests(PortableLinuxSignals):
    session = SessionTests.session
    def test_real_native_cleanup_drains_240_adopted_pty_exits_across_80_cases(self):
        kernel = AdoptionKernel()
        with kernel.installed():
            native, _, manifest, error, _ = self.session(
                native_factory=lambda directory: LifecycleNative(directory, kernel))
        self.assertIsNone(error)
        self.assertEqual(manifest['status'], 'passed')
        self.assertEqual(native.fixtures, 80)
        receipts = [item for item in native.records if item['name'] == 'adopted-child']
        self.assertEqual(len(receipts), 240)
        self.assertEqual({item['pid'] for item in receipts}, set(native.fixture_pids))
        self.assertTrue(all(item.get('reaped') and 'wait_status' in item
                            and not item['original_remaining'] for item in receipts))
        self.assertEqual({(item['pid'], tuple(item['identity']), item['wait_pid'], item['wait_status'])
                          for item in receipts},
                         {(pid, (pid, 12345), pid, 0) for pid in native.fixture_pids})
        self.assertEqual({item['pid'] for item in kernel.waits}, set(native.fixture_pids))
        self.assertEqual({pid for pid, _ in kernel.signals}, {10001, 10002})
        self.assertFalse(kernel.nodes)
        self.assertTrue(all(handle.closed for handle in kernel.handles))
        # Both private services remain alive across every fixture drain.
        self.assertTrue(all({10001, 10002} <= set(snapshot)
                            for snapshot in kernel.snapshots[:-1] if snapshot))
        self.assertLessEqual(max(map(len, kernel.snapshots)), 5)
        proof = os.environ.get('UI_WAYLAND_DRAIN_PROOF')
        if proof:
            Path(proof).write_text(json.dumps({'reap_receipts': kernel.waits,
                                              'records': native.records}, indent=2) + '\n')

    @contextlib.contextmanager
    def lifecycle(self):
        with tempfile.TemporaryDirectory() as raw:
            kernel = AdoptionKernel()
            native = LifecycleNative(Path(raw), kernel)
            with kernel.installed():
                native.acquire_scope()
                yield native, kernel

    def test_early_fixture_failures_and_run_deadline_drain_actual_adopted_children(self):
        for failure in ('startup', 'capture', 'cleanup-timeout', 'changed-adapter'):
            with self.subTest(failure=failure):
                kernel = AdoptionKernel()
                with kernel.installed():
                    native, _, manifest, error, _ = self.session(live_error=failure,
                        native_factory=lambda directory: LifecycleNative(directory, kernel))
                self.assertIsNotNone(error)
                self.assertEqual(manifest['status'], 'failed')
                self.assertTrue(native.fixture_pids)
                self.assertEqual({item['pid'] for item in kernel.waits}, set(native.fixture_pids))
                self.assertFalse(kernel.nodes)
                self.assertTrue(all(handle.closed for handle in kernel.handles))

    def test_compositor_startup_timeout_runs_real_final_cleanup_of_burst(self):
        kernel = AdoptionKernel()
        def unready(native):
            native.ready = False
            start = native.start
            def start_with_orphans(name, argv):
                owned = start(name, argv)
                if name == 'weston':
                    native.fixture_pids.extend(kernel.child() for _ in range(80))
                return owned
            native.start = start_with_orphans
        with kernel.installed():
            native, _, manifest, error, _ = self.session(unready,
                native_factory=lambda directory: LifecycleNative(directory, kernel))
        self.assertIsNotNone(error)
        self.assertIn('startup timed out', manifest['error'])
        self.assertEqual(len(kernel.waits), 80)
        self.assertFalse(kernel.nodes)
        self.assertTrue(native.cleaned)
        self.assertLessEqual(kernel.peak_handles, 34)  # 32 adopted plus both private services.

    def test_exception_after_last_fixture_keeps_failure_and_runs_real_cleanup(self):
        kernel = AdoptionKernel()
        def late_failure(native):
            original = native.fixture_exited
            def exited():
                original()
                if native.fixtures == 80:
                    raise RuntimeError('last fixture teardown exception')
            native.fixture_exited = exited
        with kernel.installed():
            native, _, manifest, error, _ = self.session(late_failure,
                native_factory=lambda directory: LifecycleNative(directory, kernel))
        self.assertIsNotNone(error)
        self.assertEqual(manifest['status'], 'failed')
        self.assertEqual(len(kernel.waits), 240)
        self.assertFalse(kernel.nodes)
        self.assertTrue(native.cleaned)

    def test_burst_drain_excludes_baseline_services_weston_and_foreign_parent(self):
        with self.lifecycle() as (native, kernel):
            baseline = kernel.child(state='S')
            native.acquire_scope()
            bus = native.start('dbus', ['synthetic'])
            weston = native.start('weston', ['synthetic'])
            client = kernel.child(state='S')
            handle = kernel.open(client)
            record = {'pid': client, 'identity': handle.identity, 'name': 'weston-desktop-shell'}
            native.records.append(record)
            native.weston_clients.append((handle, record))
            foreign = kernel.child(state='S', parent=999)
            adopted = {kernel.child() for _ in range(240)}
            native.drain_adopted_children()
            self.assertEqual({receipt['pid'] for receipt in kernel.waits}, adopted)
            self.assertEqual(set(kernel.nodes), {baseline, bus.proc.pid, weston.proc.pid, client, foreign})
            self.assertEqual(kernel.signals, [])
            self.assertLessEqual(kernel.peak_handles, 35)
            native.cleanup()
            self.assertEqual(set(kernel.nodes), {baseline, foreign})
            self.assertFalse({baseline, foreign} & {pid for pid, _ in kernel.signals})
            self.assertTrue(all(item.closed for item in kernel.handles))

    def test_final_cleanup_follows_new_adoptions_after_parent_wait(self):
        with self.lifecycle() as (native, kernel):
            parent = kernel.child(state='S')
            kernel.nodes[parent].ignore_term = True
            descendants = [kernel.child(parent=parent) for _ in range(80)]
            kernel.nodes[parent].descendants = descendants
            native.cleanup()
            self.assertEqual({item['pid'] for item in kernel.waits}, {parent, *descendants})
            self.assertEqual(kernel.signals, [(parent, runner.signal.SIGTERM),
                                               (parent, runner.signal.SIGKILL)])
            self.assertFalse(kernel.nodes)
            self.assertLessEqual(kernel.peak_handles, 32)

    def test_changed_adopted_identity_refuses_signal_and_wait_and_records_failure(self):
        with self.lifecycle() as (native, kernel):
            pid = kernel.child(state='S')
            native.remember_adopted_children()
            kernel.nodes[pid].identity = (pid, 99999)
            kernel.nodes[pid].parent = 999
            with self.assertRaisesRegex(RuntimeError, 'identity changed before wait'):
                native.cleanup()
            self.assertEqual(kernel.signals, [])
            self.assertEqual(kernel.waits, [])
            self.assertTrue(native.cleaned)
            self.assertIn('cleanup_error', native.records[0])
            self.assertTrue(all(handle.closed for handle in kernel.handles))

    def test_hung_adopted_child_fails_with_bounded_waits_and_closes_scope(self):
        with self.lifecycle() as (native, kernel):
            pid = kernel.child(state='S')
            kernel.nodes[pid].stubborn = True
            with self.assertRaisesRegex(RuntimeError, 'survived cleanup'):
                native.cleanup()
            self.assertEqual(kernel.signals, [(pid, runner.signal.SIGTERM),
                                               (pid, runner.signal.SIGKILL)])
            self.assertEqual(kernel.waits, [])
            self.assertLessEqual(kernel.clock, 6.1)
            self.assertTrue(native.cleaned)
            self.assertTrue(all(handle.closed for handle in kernel.handles))

    def test_adoption_inventory_still_has_a_byte_budget(self):
        with self.lifecycle() as (native, kernel), \
                patch.object(kernel, 'children', return_value='1 ' * 40000):
            with self.assertRaisesRegex(RuntimeError, 'exceeds 64 KiB'):
                native.cleanup()
            self.assertEqual(kernel.signals, [])
            self.assertEqual(kernel.waits, [])
            self.assertTrue(native.cleaned)

    def test_batch_exhaustion_records_unprocessed_identity_and_fails(self):
        with self.lifecycle() as (native, kernel), patch.object(runner, 'ADOPTED_BATCHES', 2):
            pids = {kernel.child() for _ in range(65)}
            with self.assertRaisesRegex(RuntimeError, 'exhausted'):
                native.cleanup()
            self.assertEqual(len(kernel.waits), 64)
            self.assertEqual(len(native.records), 65)
            self.assertEqual(set(kernel.nodes), pids - {item['pid'] for item in kernel.waits})
            self.assertIn('cleanup_error', native.records[-1])
            self.assertLessEqual(kernel.peak_handles, 32)
            self.assertTrue(all(handle.closed for handle in kernel.handles))
            self.assertTrue(native.cleaned)

    def test_shared_deadline_bounds_many_hung_children_and_closes_every_handle(self):
        with self.lifecycle() as (native, kernel):
            pids = {kernel.child(state='S') for _ in range(12)}
            for node in kernel.nodes.values():
                node.stubborn = True
            with self.assertRaisesRegex(RuntimeError, 'deadline'):
                native.cleanup()
            self.assertEqual(set(kernel.nodes), pids)
            self.assertLessEqual(kernel.clock, 30.1)
            self.assertTrue(all(handle.closed for handle in kernel.handles))
            self.assertTrue(native.cleaned)

    def test_diagnostic_error_does_not_skip_later_batches_or_accept_success(self):
        with self.lifecycle() as (native, kernel):
            pids = [kernel.child() for _ in range(80)]
            ps = native.ps
            def diagnostic(pid):
                if pid == pids[0]:
                    raise RuntimeError('synthetic ps failure')
                return ps(pid)
            native.ps = diagnostic
            with self.assertRaisesRegex(RuntimeError, 'synthetic ps failure'):
                native.cleanup()
            self.assertEqual({item['pid'] for item in kernel.waits}, set(pids))
            self.assertFalse(kernel.nodes)
            self.assertTrue(native.records[0]['reaped'])
            self.assertIn('cleanup_error', native.records[0])
            self.assertTrue(all(handle.closed for handle in kernel.handles))
            self.assertTrue(native.cleaned)

    def test_real_nested_subreaper_scope_remains_active_until_last_owner_closes(self):
        spec = importlib.util.spec_from_file_location(
            'wayland_nested_scope_fixture', Path(__file__).with_name('check-live-ui-smoke.py'))
        smoke = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = smoke
        self.addCleanup(sys.modules.pop, spec.name, None)
        spec.loader.exec_module(smoke)
        scope_type = smoke.LinuxSubreaperScope
        state, mutations = [0], []
        def set_state(value):
            state[0] = value
            mutations.append(value)
        with self.lifecycle() as (fixture, kernel), \
                patch.object(smoke.platform, 'system', return_value='Linux'), \
                patch.object(smoke.os, 'getuid', return_value=123, create=True), \
                patch.object(scope_type, '_get', side_effect=lambda: state[0]), \
                patch.object(scope_type, '_set', side_effect=set_state), \
                patch.object(smoke, 'run', return_value=SimpleNamespace(returncode=0, stdout='')), \
                patch.object(smoke, 'StableProcessHandle', SimpleNamespace(open=kernel.open)), \
                patch.object(smoke, '_linux_process_parent',
                             side_effect=lambda pid: kernel.nodes[pid].parent):
            parent = scope_type.acquire()
            native = runner.Native(smoke, {}, fixture.directory)
            native.ps = fixture.ps
            native.now, native.sleep = fixture.now, fixture.sleep
            inner = None
            try:
                native.acquire_scope()
                inner = scope_type.acquire()
                pid = kernel.child()
                inner.close()
                self.assertEqual(scope_type._active_scopes, 2)
                native.drain_adopted_children()
                self.assertEqual([item['pid'] for item in kernel.waits], [pid])
                self.assertEqual(state[0], 1)
                native.cleanup()
                self.assertEqual(scope_type._active_scopes, 1)
                self.assertEqual(state[0], 1)
            finally:
                if inner is not None:
                    inner.close()
                if native.scope is not None:
                    native.scope.close()
                parent.close()
            self.assertEqual(scope_type._active_scopes, 0)
            self.assertEqual(state[0], 0)
            self.assertEqual(mutations, [1, 0])


class ProcessTests(PortableLinuxSignals):
    def test_source_identity_rejects_device_input_and_enforces_regular_file_budget(self):
        with self.assertRaisesRegex(RuntimeError, 'regular file'):
            runner.digest(Path(os.devnull))
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / 'binary'
            path.write_bytes(b'12345')
            with self.assertRaisesRegex(RuntimeError, 'exceeds 4 bytes'):
                runner.digest(path, maximum=4)

    def owned(self, directory, matches=True):
        native = runner.Native(SimpleNamespace(), {}, directory)
        proc, handle = FakeProcess(), FakeHandle(matches)
        record = {'pid': proc.pid, 'name': 'fixture'}
        owned = SimpleNamespace(proc=proc, handle=handle, record=record)
        native.records.append(record)
        native.ps = lambda pid: f'{pid} fixture ps\n'
        return native, owned, handle

    def test_changed_identity_refuses_all_signals_and_keeps_failure_record(self):
        with tempfile.TemporaryDirectory() as raw:
            native, owned, handle = self.owned(Path(raw), matches=False)
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                native.stop(owned)
            self.assertEqual(handle.signals, [])
            self.assertEqual(owned.proc.waits, [])
            self.assertNotIn('reaped', owned.record)
            self.assertTrue((Path(raw) / 'processes.json').exists())

    def test_hung_child_escalates_only_verified_handle_and_waits_with_limits(self):
        with tempfile.TemporaryDirectory() as raw:
            native, owned, handle = self.owned(Path(raw))
            owned.proc.timeouts = 1
            # After wait reaps the original, the identity is no longer present.
            original_wait = owned.proc.wait
            def wait(timeout):
                result = original_wait(timeout)
                handle.matches = False
                return result
            owned.proc.wait = wait
            native.stop(owned)
            self.assertEqual(handle.signals, [runner.signal.SIGTERM, runner.signal.SIGKILL])
            self.assertEqual(owned.proc.waits, [3, 3])
            self.assertTrue(owned.record['reaped'])
            self.assertFalse(owned.record['original_remaining'])
            self.assertTrue(handle.closed)
            self.assertIn('ps_after_wait', owned.record)

    def test_failed_ps_diagnostic_does_not_skip_owned_process_cleanup(self):
        with tempfile.TemporaryDirectory() as raw:
            native, owned, handle = self.owned(Path(raw))
            def cannot_ps(pid):
                raise RuntimeError('fixture ps failure')
            native.ps = cannot_ps
            original_wait = owned.proc.wait
            def wait(timeout):
                result = original_wait(timeout)
                handle.matches = False
                return result
            owned.proc.wait = wait
            with self.assertRaisesRegex(RuntimeError, 'ps diagnostics failed'):
                native.stop(owned)
            self.assertEqual(handle.signals, [runner.signal.SIGTERM])
            self.assertTrue(owned.record['reaped'])
            self.assertTrue(handle.closed)
            self.assertEqual(len(owned.record['ps_errors']), 2)

    def test_reused_pid_between_term_and_kill_is_never_signalled(self):
        with tempfile.TemporaryDirectory() as raw:
            native, owned, handle = self.owned(Path(raw))
            def timeout_after_reuse(timeout):
                handle.matches = False
                raise subprocess.TimeoutExpired(['fixture'], timeout)
            owned.proc.wait = timeout_after_reuse
            error = None
            try:
                native.stop(owned)
            except Exception as caught:
                error = caught
            self.assertEqual(handle.signals, [runner.signal.SIGTERM])
            self.assertIsInstance(error, RuntimeError)
            self.assertIn('refusing SIGKILL', str(error))

    def test_weston_child_parent_must_match_before_it_can_be_owned(self):
        with tempfile.TemporaryDirectory() as raw:
            child = FakeHandle()
            child.pid = 444
            stable = SimpleNamespace(open=lambda pid: child)
            smoke = SimpleNamespace(StableProcessHandle=stable, _linux_process_parent=lambda pid: 999)
            native = runner.Native(smoke, {}, Path(raw))
            weston = SimpleNamespace(proc=FakeProcess(), handle=FakeHandle())
            with patch.object(Path, 'read_text', return_value='444'):
                with self.assertRaisesRegex(RuntimeError, 'parent changed'):
                    native.remember_weston_children(weston)
            self.assertEqual(native.weston_clients, [])
            self.assertEqual(child.signals, [])
            self.assertTrue(child.closed)

    def test_unexpected_weston_client_is_recorded_before_failure_for_safe_cleanup(self):
        with tempfile.TemporaryDirectory() as raw:
            child = FakeHandle()
            child.pid = 444
            stable = SimpleNamespace(open=lambda pid: child)
            smoke = SimpleNamespace(StableProcessHandle=stable, _linux_process_parent=lambda pid: 321)
            native = runner.Native(smoke, {}, Path(raw))
            native.ps = lambda pid: '444 fixture child'
            weston = SimpleNamespace(proc=FakeProcess(), handle=FakeHandle())
            with patch.object(Path, 'read_text', return_value='444'), \
                    patch.object(runner.os, 'readlink', return_value='/fixture/unexpected'):
                with self.assertRaisesRegex(RuntimeError, 'unexpected Weston child'):
                    native.remember_weston_children(weston)
            self.assertEqual(native.weston_clients[0][0], child)
            self.assertEqual(native.records[0]['executable'], '/fixture/unexpected')
            self.assertEqual(child.signals, [])
            self.assertFalse(child.closed)

    def test_only_owned_unix_sockets_satisfy_startup(self):
        native = runner.Native(SimpleNamespace(), {}, Path('.'))
        for mode, uid in ((0o100600, 123), (0o120777, 123), (0o140600, 999)):
            metadata = SimpleNamespace(st_mode=mode, st_uid=uid)
            with self.subTest(mode=mode, uid=uid), \
                    patch.object(runner.os, 'getuid', return_value=123, create=True), \
                    patch.object(Path, 'lstat', return_value=metadata):
                with self.assertRaisesRegex(RuntimeError, 'owned private Unix socket'):
                    native.socket_ready(Path('/fixture/socket'))
        with patch.object(runner.os, 'getuid', return_value=123, create=True), \
                patch.object(Path, 'lstat', return_value=SimpleNamespace(st_mode=0o140600, st_uid=123)):
            self.assertTrue(native.socket_ready(Path('/fixture/socket')))

    def test_adoption_excludes_preexisting_child_and_requires_runner_parent(self):
        with tempfile.TemporaryDirectory() as raw:
            handles = {}
            for pid in (444, 445, 446):
                handle = FakeHandle()
                handle.pid, handle.identity = pid, (pid, 12345)
                handles[pid] = handle
            smoke = SimpleNamespace(StableProcessHandle=SimpleNamespace(open=lambda pid: handles[pid]),
                                    _linux_process_parent=lambda pid: 777 if pid != 446 else 999)
            native = runner.Native(smoke, {}, Path(raw))
            native.scope = SimpleNamespace(was_present_at_acquire=lambda handle: handle.pid == 444)
            with patch.object(runner.os, 'getpid', return_value=777), \
                    proc_child_inventory('444 445 446'):
                native.remember_adopted_children()
            self.assertEqual([record['pid'] for record in native.records], [445])
            self.assertTrue(handles[444].closed)
            self.assertTrue(handles[446].closed)
            self.assertFalse(handles[445].closed)
            self.assertTrue(all(not handle.signals for handle in handles.values()))

    def test_failed_startup_adopted_child_is_waited_and_ps_checked(self):
        with tempfile.TemporaryDirectory() as raw:
            child = FakeHandle()
            child.pid, child.identity = 445, (445, 12345)
            smoke = SimpleNamespace(StableProcessHandle=SimpleNamespace(open=lambda pid: child),
                                    _linux_process_parent=lambda pid: 777)
            native = runner.Native(smoke, {}, Path(raw))
            events = []
            native.scope = SimpleNamespace(was_present_at_acquire=lambda handle: False,
                                           close=lambda: events.append('scope closed'))
            weston = SimpleNamespace(record={'name': 'weston'}, proc=FakeProcess())
            native.children = [weston]
            native.remember_weston_children = lambda owned: events.append('inventory before stop')
            native.stop = lambda owned: events.append('Weston stopped')
            native.ps = lambda pid: events.append(('ps', pid)) or ''
            waits = []
            def waitpid(pid, flags):
                waits.append((pid, flags))
                if len(waits) == 1:
                    return 0, 0
                child.matches = False
                return pid, 0
            with patch.object(runner.os, 'getpid', return_value=777), \
                    proc_child_inventory('445'), \
                    patch.object(runner.os, 'waitpid', side_effect=waitpid, create=True), \
                    patch.object(runner.os, 'WNOHANG', 1, create=True):
                native.cleanup()
            self.assertEqual(events[:2], ['inventory before stop', 'Weston stopped'])
            self.assertEqual(child.signals, [runner.signal.SIGTERM])
            self.assertEqual(waits, [(445, 1), (445, 1)])
            self.assertTrue(native.records[0]['reaped'])
            self.assertFalse(native.records[0]['original_remaining'])
            self.assertIn(('ps', 445), events)
            self.assertIn('scope closed', events)
            self.assertTrue(child.closed)

    def test_query_timeout_unwinds_through_identity_checked_cleanup(self):
        with tempfile.TemporaryDirectory() as raw:
            native, owned, handle = self.owned(Path(raw))
            owned.path = Path(raw) / 'query.log'
            owned.path.write_text('retained native failure diagnostics')
            def timeout_once(timeout):
                owned.proc.wait = lambda timeout: setattr(handle, 'matches', False)
                raise subprocess.TimeoutExpired(['fixture'], timeout)
            owned.proc.wait = timeout_once
            native.start = lambda name, argv: owned
            with self.assertRaises(subprocess.TimeoutExpired):
                native.query('fixture', ['fixture'], timeout=0.25)
            self.assertEqual(handle.signals, [runner.signal.SIGTERM])
            self.assertTrue(owned.record['reaped'])
            self.assertEqual(owned.path.read_text(), 'retained native failure diagnostics')

    def test_deadline_raises_through_context_cleanup_and_restores_handler(self):
        callbacks, timers, events = [], [], []
        def install(number, callback):
            callbacks.append(callback)
            return 'original handler'
        fake_signal = SimpleNamespace(SIGALRM=14, ITIMER_REAL=0, signal=install,
                                      setitimer=lambda kind, seconds: timers.append(seconds))
        with patch.object(runner, 'signal', fake_signal):
            with self.assertRaises(runner.DeadlineExpired):
                with runner.bounded(3):
                    try:
                        callbacks[0](14, None)
                        events.append('capture after deadline')
                    finally:
                        events.append('fixture cleanup')
        self.assertEqual(events, ['fixture cleanup'])
        self.assertEqual(timers, [3, 0])
        self.assertEqual(callbacks[-1], 'original handler')

    def test_non_linux_rejects_before_filesystem_or_process_side_effects(self):
        with patch.object(runner.subprocess, 'Popen') as launch, \
                patch.object(runner.tempfile, 'mkdtemp') as scratch, \
                contextlib.redirect_stderr(io.StringIO()):
            for system in ('Darwin', 'Windows'):
                with self.subTest(system=system), self.assertRaises(SystemExit):
                    runner.main([], system=lambda: system)
            launch.assert_not_called()
            scratch.assert_not_called()

    def test_overlap_and_output_symlink_reject_before_helper_or_processes(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            out = root / 'out'
            out.mkdir()
            marker = out / 'keep'
            marker.write_bytes(b'preserved')
            with patch.object(runner.subprocess, 'Popen') as launch, \
                    patch.object(runner.tempfile, 'mkdtemp') as scratch, \
                    contextlib.redirect_stderr(io.StringIO()):
                for args in (['--out', str(out), '--compare', str(out)],
                             ['--out', str(out), '--diagnostics', str(out / 'diagnostics')],
                             ['--out', str(out), '--compare', str(root)]):
                    with self.subTest(args=args), self.assertRaises(SystemExit):
                        runner.main(args, system=lambda: 'Linux')
                alias = root / 'output-link'
                try:
                    alias.symlink_to(out, target_is_directory=True)
                except (OSError, NotImplementedError):
                    if os.name != 'nt':
                        raise
                else:
                    with self.assertRaises(SystemExit):
                        runner.main(['--out', str(alias), '--diagnostics', str(root / 'diags')],
                                    system=lambda: 'Linux')
                    self.assertTrue(alias.is_symlink())
                self.assertEqual(marker.read_bytes(), b'preserved')
                launch.assert_not_called()
                scratch.assert_not_called()


if __name__ == '__main__':
    unittest.main()
