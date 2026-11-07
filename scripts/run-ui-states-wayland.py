#!/usr/bin/env python3
"""Linux UI-state matrix in a private headless Weston GL/llvmpipe session.

No baselines are generated or promoted. Diagnostics are separate from capture
output so capture-ui-states.py's path guards and output lifecycle stay intact.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parent.parent
LIMIT = 16 * 1024 * 1024
ADOPTED_BATCH = 32
ADOPTED_BATCHES = 32
ADOPTED_INVENTORY_BYTES = 64 * 1024
GPU_CONFIG = 'gpu-backend = gl\ngpu-force-software = true\n'
SOURCES = [
    'https://wayland.pages.freedesktop.org/weston/toc/running-weston.html',
    'https://gitlab.freedesktop.org/wayland/weston/-/raw/13.0/man/weston.man',
    'https://gitlab.freedesktop.org/wayland/weston/-/raw/13.0/compositor/main.c',
    'https://gitlab.freedesktop.org/wayland/weston/-/raw/13.0/desktop-shell/shell.c',
]


class DeadlineExpired(BaseException):
    pass


@contextlib.contextmanager
def bounded(seconds):
    """Unwind LiveKettle's context on expiry, rather than killing its Python owner."""
    def expired(_number, _frame):
        raise DeadlineExpired(f'UI-state run exceeded {seconds}s')

    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def read_text(path):
    with Path(path).open('rb') as stream:
        data = stream.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise RuntimeError(f'diagnostic exceeds {LIMIT} bytes: {path}')
    return data.decode('utf-8', errors='replace')


def digest(path, maximum=512 * 1024 * 1024):
    value = hashlib.sha256()
    count = 0
    flags = os.O_RDONLY | getattr(os, 'O_NONBLOCK', 0) | getattr(os, 'O_NOFOLLOW', 0)
    descriptor = os.open(path, flags)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError(f'identity input must be a regular file: {path}')
        if info.st_size > maximum:
            raise RuntimeError(f'identity file exceeds {maximum} bytes: {path}')
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            count += len(block)
            if count > maximum:
                raise RuntimeError(f'identity file exceeds {maximum} bytes: {path}')
            value.update(block)
    return value.hexdigest()


@contextlib.contextmanager
def file_limit():
    """Apply the same file budget to Kettle/PTY children as to native probes."""
    import resource

    previous = resource.getrlimit(resource.RLIMIT_FSIZE)
    soft = LIMIT if previous[0] == resource.RLIM_INFINITY else min(LIMIT, previous[0])
    resource.setrlimit(resource.RLIMIT_FSIZE, (soft, previous[1]))
    try:
        yield
    finally:
        resource.setrlimit(resource.RLIMIT_FSIZE, previous)


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


def make_environment(capture, root):
    """Build an allowlist, then apply the helper's None-means-remove overrides."""
    home = root / 'home'
    home.mkdir(mode=0o700)
    environment = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8',
                   'TZ': 'UTC', 'TERM': 'xterm-256color',
                   'LIBGL_ALWAYS_SOFTWARE': '1', 'GALLIUM_DRIVER': 'llvmpipe',
                   'XDG_SESSION_TYPE': 'wayland', 'GDK_BACKEND': 'wayland',
                   'WINIT_UNIX_BACKEND': 'wayland',
                   'RUST_LOG': 'info', 'NO_AT_BRIDGE': '1',
                   'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
                   'GIT_TERMINAL_PROMPT': '0',
                   'FONTCONFIG_FILE': str(root / 'fonts.conf'),
                   'FONTCONFIG_PATH': str(root / 'fontconfig'),
                   'TMPDIR': str(root / 'tmp')}
    for key, value in capture.private_env(home).items():
        if value is not None:
            environment[key] = value
    # Each capture overrides XDG_RUNTIME_DIR again. An absolute socket survives.
    environment['WAYLAND_DISPLAY'] = str(Path(environment['XDG_RUNTIME_DIR']) / 'ui-wayland')
    environment['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(root / 'bus')
    environment['DBUS_SYSTEM_BUS_ADDRESS'] = environment['DBUS_SESSION_BUS_ADDRESS']
    environment['XDG_CONFIG_DIRS'] = str(root / 'config-dirs')
    environment['XDG_DATA_DIRS'] = str(root / 'data-dirs')
    environment['GSETTINGS_BACKEND'] = 'memory'
    for name in ('fontconfig', 'tmp', 'config-dirs', 'data-dirs'):
        (root / name).mkdir(mode=0o700)
    return environment


def configure_fonts(root, font_root=ROOT / 'assets/fonts'):
    fonts = sorted(font_root.rglob('*.ttf'))
    if len(fonts) > 32:
        raise RuntimeError('bundled font inventory exceeds 32 files')
    primary = sorted(path for path in fonts if path.parent == font_root)
    expected = {f'JetBrainsMonoNerdFont-{face}.ttf'
                for face in ('Regular', 'Bold', 'Italic', 'BoldItalic')}
    if {p.name for p in primary} != expected:
        raise RuntimeError('expected all four bundled JetBrains Mono TTFs')
    if any(p.is_symlink() or not p.is_file() for p in fonts):
        raise RuntimeError('bundled fonts must be regular non-symlink files')
    cache = root / 'font-cache'
    cache.mkdir(mode=0o700)
    config = root / 'fonts.conf'
    config.write_text('<?xml version="1.0"?>\n<!DOCTYPE fontconfig SYSTEM "fonts.dtd">\n'
                      '<fontconfig><reset-dirs/>\n'
                      f'<dir>{escape(str(font_root.resolve()))}</dir>\n'
                      f'<cachedir>{escape(str(cache))}</cachedir>\n'
                      '</fontconfig>\n', encoding='utf-8')
    return {str(p.resolve()): digest(p) for p in fonts}


def verify_fonts(listing, expected):
    actual = {str(Path(line).resolve()) for line in listing.splitlines() if line}
    if actual != set(expected):
        raise RuntimeError(f'font inventory differs from bundled fonts: {sorted(actual)}')
    return sorted(actual)


def verify_gpu(text):
    fields = dict(re.findall(r'^([^:\n]+):\s*([^\n]+)$', text, re.M))
    if (not re.match(r'^llvmpipe\b', fields.get('Adapter', ''), re.I)
            or fields.get('Adapter type') != 'Cpu' or fields.get('Backend') != 'Gl'
            or fields.get('Backend fallback') != 'no'
            or fields.get('Driver info', '') in ('', '<unknown>')):
        raise RuntimeError('GPU diagnostic must report actual GL llvmpipe CPU and driver version')
    return fields


def verify_weston(text):
    backend = re.findall(r'Loading module [^\n]*[/\\](headless-backend\.so)[\'\"]?', text)
    renderer = re.findall(r'GL renderer:\s*([^\n]+)', text)
    version = re.findall(r'GL version:\s*([^\n]+)', text)
    if not backend or not renderer or not version or not re.match(r'^llvmpipe\b', renderer[-1], re.I):
        raise RuntimeError('Weston must log the headless backend, GL renderer llvmpipe and GL version')
    if re.search(r'Loading module [^\n]*(?:drm|x11|wayland|rdp|vnc)-backend\.so', text):
        raise RuntimeError('Weston loaded an unrequested backend')
    return {'backend': backend[-1], 'renderer': renderer[-1], 'gl_version': version[-1]}


def verify_live(text, gpu):
    if 'Fallback to loading from known font dir paths.' in text:
        raise RuntimeError('live fontdb fell back to system font directories')
    choices = re.findall(r'(?:using software adapter|selected GPU) (.+) \(([^\n]+)\)', text)
    if not choices:
        raise RuntimeError('live Kettle did not report its selected adapter')
    for adapter, backend in choices:
        if adapter != gpu['Adapter'] or backend not in ('GL', 'Software, GL'):
            raise RuntimeError(f'live adapter differs from verified llvmpipe: {adapter} ({backend})')
    return choices


class Native:
    """Injectable OS boundary. Stable handles are the repository's pidfd helper."""
    system = staticmethod(platform.system)
    now = staticmethod(time.monotonic)
    sleep = staticmethod(time.sleep)

    def __init__(self, smoke, environment, directory):
        self.smoke, self.environment, self.directory = smoke, environment, directory
        self.records = []
        self.children = []
        self.weston_clients = []
        self.adopted_children = []
        self.scope = None

    def start(self, name, argv):
        import resource

        def limits():
            resource.setrlimit(resource.RLIMIT_FSIZE, (LIMIT, LIMIT))
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))

        path = self.directory / f'{name}.log'
        with path.open('wb') as log:
            proc = subprocess.Popen(argv, env=self.environment, cwd=ROOT,
                                    stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                    start_new_session=True, preexec_fn=limits)
        record = {'name': name, 'pid': proc.pid, 'argv': argv}
        self.records.append(record)
        save(self.directory / 'processes.json', self.records)
        owned = SimpleNamespace(proc=proc, handle=None, record=record, path=path)
        self.children.append(owned)
        # A direct unreaped child cannot have its PID reused. A fast exit can
        # disappear from pidfd_open before we sample it; wait still owns it.
        try:
            handle = self.smoke.StableProcessHandle.open(proc.pid)
        except (ProcessLookupError, FileNotFoundError):
            handle = None
        owned.handle = handle
        if handle is not None:
            record['identity'] = handle.identity
        return owned

    def ps(self, pid):
        # ps itself is bounded and reaped; no global process-table selection.
        probe = self.start(f'ps-{pid}-{len(self.records)}',
                           ['/bin/ps', '-p', str(pid), '-o', 'pid=,ppid=,stat=,args='])
        try:
            probe.proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            raise RuntimeError(f'ps timed out for recorded PID {pid}')
        finally:
            self.stop(probe, check_ps=False)
        return read_text(probe.path)

    def stop(self, owned, check_ps=True):
        record, proc, handle = owned.record, owned.proc, owned.handle
        if record.get('reaped'):
            return
        ps_errors = []
        try:
            if check_ps:
                try:
                    record['ps_before_cleanup'] = self.ps(proc.pid)
                except BaseException as error:
                    ps_errors.append(str(error))
            if proc.poll() is None:
                if handle is None or not handle.matches_current():
                    raise RuntimeError(f'identity changed for recorded PID {proc.pid}; refusing signal')
                handle.signal(signal.SIGTERM)
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    if not handle.matches_current():
                        raise RuntimeError(f'identity changed for PID {proc.pid}; refusing SIGKILL')
                    handle.signal(signal.SIGKILL)
                    proc.wait(timeout=3)
            else:
                proc.wait(timeout=3)
            record.update(exit=proc.returncode, reaped=True)
            if check_ps:
                try:
                    record['ps_after_wait'] = self.ps(proc.pid)
                except BaseException as error:
                    ps_errors.append(str(error))
                # PID reuse after wait is allowed; never signal a replacement.
                record['original_remaining'] = bool(handle and handle.matches_current())
                if record['original_remaining']:
                    raise RuntimeError(f'recorded process {proc.pid} survived wait')
            if ps_errors:
                record['ps_errors'] = ps_errors
                raise RuntimeError('process exited but ps diagnostics failed: ' + '; '.join(ps_errors))
        finally:
            if handle is not None:
                handle.close()
                owned.handle = None
            save(self.directory / 'processes.json', self.records)

    def query(self, name, argv, timeout=20):
        owned = self.start(name, argv)
        try:
            code = owned.proc.wait(timeout=timeout)
            if code != 0:
                raise RuntimeError(f'{name} exited {code}; see {owned.path}')
            return read_text(owned.path)
        finally:
            self.stop(owned)

    def socket_ready(self, path):
        try:
            info = path.lstat()
        except FileNotFoundError:
            return False
        if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid():
            raise RuntimeError(f'not an owned private Unix socket: {path}')
        return True

    def acquire_scope(self):
        self.scope = self.smoke.LinuxSubreaperScope.acquire()

    def remember_weston_children(self, weston):
        """Weston forks its desktop-shell client. Record only direct children."""
        if weston.handle is None or not weston.handle.matches_current():
            raise RuntimeError('Weston identity vanished during child inventory')
        path = Path(f'/proc/{weston.proc.pid}/task/{weston.proc.pid}/children')
        children = path.read_text().split()
        if not children:
            return False
        unexpected = len(children) != 1
        for raw in children:
            pid = int(raw)
            handle = self.smoke.StableProcessHandle.open(pid)
            if not handle.matches_current() or self.smoke._linux_process_parent(pid) != weston.proc.pid:
                handle.close()
                raise RuntimeError('Weston child identity/parent changed')
            executable = os.readlink(f'/proc/{pid}/exe')
            # fork precedes exec of the shell client. Do not mistake that
            # brief, still-Weston executable for a ready desktop-shell client.
            if Path(executable).name == 'weston' and len(children) == 1:
                handle.close()
                return False
            record = {'name': 'weston-client', 'pid': pid, 'identity': handle.identity,
                      'executable': executable, 'parent': weston.proc.pid}
            if any(previous.identity == handle.identity for previous, _ in self.weston_clients):
                handle.close()
            else:
                self.records.append(record)
                self.weston_clients.append((handle, record))
                save(self.directory / 'processes.json', self.records)
            if Path(executable).name != 'weston-desktop-shell':
                unexpected = True
            record['ps_before_cleanup'] = self.ps(pid)
            save(self.directory / 'processes.json', self.records)
        if unexpected:
            raise RuntimeError('unexpected Weston child executable/count; recorded for cleanup')
        record['name'] = 'weston-desktop-shell'
        return True

    def remember_adopted_children(self, *, deadline=None, attempted=()):
        """Retain at most 32 new direct children, excluding known service owners."""
        if self.scope is None:
            return
        if len(self.adopted_children) >= ADOPTED_BATCH:
            return
        owner = os.getpid()
        path = Path(f'/proc/{owner}/task/{owner}/children')
        with path.open('rb') as stream:
            data = stream.read(ADOPTED_INVENTORY_BYTES + 1)
        if len(data) > ADOPTED_INVENTORY_BYTES:
            raise RuntimeError('adopted child inventory exceeds 64 KiB')
        children = data.split()
        known = {owned.record.get('identity') for owned in self.children}
        known.update(handle.identity for handle, _ in self.weston_clients)
        known.update(handle.identity for handle, _ in self.adopted_children)
        known.update(attempted)
        for raw in children:
            if deadline is not None and self.now() >= deadline:
                raise RuntimeError('adopted child inventory exceeded cleanup deadline')
            pid = int(raw)
            try:
                handle = self.smoke.StableProcessHandle.open(pid)
            except (ProcessLookupError, FileNotFoundError):
                continue
            retained = False
            try:
                if (not handle.matches_current() or self.smoke._linux_process_parent(pid) != owner
                        or self.scope.was_present_at_acquire(handle) or handle.identity in known):
                    continue
                record = {'name': 'adopted-child', 'pid': pid, 'identity': handle.identity,
                          'parent': owner, 'ownership': 'new direct child in private subreaper scope'}
                self.records.append(record)
                self.adopted_children.append((handle, record))
                retained = True
                known.add(handle.identity)
                if len(self.adopted_children) >= ADOPTED_BATCH:
                    break
            finally:
                if not retained:
                    handle.close()
        save(self.directory / 'processes.json', self.records)

    def cleanup_child(self, handle, record, cleanup_deadline):
        """Wait only while the retained identity still owns this direct-child PID."""
        def reap():
            if not handle.matches_current():
                if record['name'] == 'adopted-child':
                    raise RuntimeError(f'identity changed before wait for adopted PID {handle.pid}')
                return True  # Weston may already have waited its own client.
            try:
                waited, status = os.waitpid(handle.pid, os.WNOHANG)
            except ChildProcessError:
                if record['name'] == 'adopted-child':
                    raise RuntimeError(f'adopted PID {handle.pid} is no longer our child') from None
                return False
            if waited == handle.pid:
                record.update(reaped=True, wait_pid=waited, wait_status=status)
                return True
            return False

        try:
            done = reap()
            for number in (signal.SIGTERM, signal.SIGKILL):
                if done:
                    break
                if self.now() >= cleanup_deadline:
                    raise RuntimeError('adopted child cleanup exceeded 30s deadline')
                if not handle.matches_current():
                    raise RuntimeError(f'identity changed for PID {handle.pid}; refusing signal')
                handle.signal(number)
                deadline = min(self.now() + 3, cleanup_deadline)
                while self.now() < deadline:
                    if reap():
                        done = True
                        break
                    self.sleep(0.05)
            record['original_remaining'] = handle.matches_current()
            if not done or record['original_remaining']:
                raise RuntimeError(f'recorded adopted child {handle.pid} survived cleanup')
            if self.now() >= cleanup_deadline:
                raise RuntimeError('adopted child diagnostics exceeded 30s deadline')
            record['ps_after_wait'] = self.ps(handle.pid)
        except BaseException as error:
            record['cleanup_error'] = str(error)
            raise
        finally:
            handle.close()
            save(self.directory / 'processes.json', self.records)

    def drain_adopted_children(self, *, deadline=None):
        """Drain between fixtures and on failure without stopping Weston or D-Bus."""
        deadline = self.now() + 30 if deadline is None else deadline
        errors = []
        attempted = set()
        for batch in range(ADOPTED_BATCHES + 1):
            inventory_failed = False
            try:
                self.remember_adopted_children(deadline=deadline, attempted=attempted)
            except BaseException as error:
                errors.append(str(error))
                inventory_failed = True
            pending, self.adopted_children = self.adopted_children, []
            if not pending:
                break
            if batch == ADOPTED_BATCHES:
                for handle, record in pending:
                    record['cleanup_error'] = 'adopted cleanup exhausted 32 batches'
                    handle.close()
                save(self.directory / 'processes.json', self.records)
                errors.append('adopted cleanup exhausted 32 batches; remaining children not accepted')
                break
            for handle, record in pending:
                attempted.add(handle.identity)
                try:
                    self.cleanup_child(handle, record, deadline)
                except BaseException as error:
                    errors.append(str(error))
            if inventory_failed:
                break
        if errors:
            raise RuntimeError('; '.join(errors))

    def cleanup(self):
        errors = []
        # Startup can fail after Weston forked its client but before readiness.
        for owned in self.children[:]:
            if owned.record['name'] == 'weston' and owned.proc.poll() is None:
                try:
                    self.remember_weston_children(owned)
                except BaseException as error:
                    errors.append(str(error))
        # Stop Weston first so its desktop client cannot respawn. The subreaper
        # then owns any surviving recorded client and can waitpid it directly.
        for owned in list(reversed(self.children)):
            try:
                self.stop(owned)
            except BaseException as error:
                errors.append(str(error))
        cleanup_deadline = self.now() + 30
        for handle, record in self.weston_clients:
            try:
                self.cleanup_child(handle, record, cleanup_deadline)
            except BaseException as error:
                errors.append(str(error))
        # Each batch re-inventories after waits, including newly orphaned
        # grandchildren. The time and batch budgets bound both depth and width.
        try:
            self.drain_adopted_children(deadline=cleanup_deadline)
        except BaseException as error:
            errors.append(str(error))
        if self.scope is not None:
            try:
                self.scope.close()
            except BaseException as error:
                errors.append(str(error))
        # ps receipts are themselves direct children, already waited above.
        save(self.directory / 'processes.json', self.records)
        if errors:
            raise RuntimeError('; '.join(errors))


def wait_ready(native, processes, paths, timeout):
    deadline = native.now() + timeout
    while native.now() < deadline:
        for owned in processes:
            if owned.proc.poll() is not None:
                raise RuntimeError(f'{owned.record["name"]} exited during startup')
        if (all(native.socket_ready(path) for path in paths)
                and native.remember_weston_children(processes[-1])):
            return
        native.sleep(0.05)
    raise RuntimeError('private compositor/bus startup timed out')


def guarded_smoke(smoke, gpu, directory, native):
    """Keep the original helper's PTY ownership and cleanup; retain every log."""
    class Guarded(smoke.LiveKettle):
        def __enter__(self):
            with self.cfg.open('a', encoding='utf-8') as config:
                config.write('\n' + GPU_CONFIG)
            try:
                live = super().__enter__()
            except BaseException:
                self.retain_and_drain()
                raise
            try:
                verify_live(read_text(self.log), gpu)
                return live
            except BaseException:
                # __enter__ failures do not receive a with-statement __exit__.
                self.__exit__(*sys.exc_info())
                raise

        def screenshot(self, path):
            verify_live(read_text(self.log), gpu)
            result = super().screenshot(path)
            verify_live(read_text(self.log), gpu)
            return result

        def __exit__(self, *args):
            try:
                try:
                    return super().__exit__(*args)
                except DeadlineExpired:
                    # An alarm in cleanup must not abandon its owned PTYs.
                    super().__exit__(*sys.exc_info())
                    raise
            finally:
                self.retain_and_drain()

        def retain_and_drain(self):
            try:
                self.retain()
            finally:
                native.drain_adopted_children()

        def retain(self):
            index = len(list(directory.glob('fixture-*.log')))
            target = directory / f'fixture-{index:03d}.log'
            if self.log.exists():
                target.write_text(read_text(self.log), encoding='utf-8')
            with (directory / 'fixtures.jsonl').open('a', encoding='utf-8') as output:
                output.write(json.dumps({'log': target.name, 'pid': self.proc.pid if self.proc else None,
                                         'cleanup': getattr(self, '_termination_receipt', None)}) + '\n')
    return SimpleNamespace(LiveKettle=Guarded, live_shell_command=smoke.live_shell_command)


def source_manifest(native, kettle):
    git = ['git', '-c', 'core.fsmonitor=false', '-C', str(ROOT)]
    head = native.query('source-head', [*git, 'rev-parse', 'HEAD']).strip()
    changes = native.query('source-diff', [*git, 'diff', '--no-ext-diff', '--no-textconv', '--binary', 'HEAD'])
    others = native.query('source-untracked', [*git, 'ls-files',
                                              '--others', '--exclude-standard', '-z'])
    files = {}
    names = [name for name in others.split('\0') if name]
    if len(names) > 4096:
        raise RuntimeError('untracked source inventory exceeds 4096 files')
    deadline = native.now() + 30
    total = 0
    for name in names:
        if not name:
            continue
        path = ROOT / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size > LIMIT:
            raise RuntimeError(f'unbounded/non-regular untracked source: {name}')
        total += path.stat().st_size
        if total > 128 * 1024 * 1024 or native.now() > deadline:
            raise RuntimeError('untracked source identity exceeded byte/time budget')
        files[name] = digest(path, maximum=LIMIT)
    return {'head': head, 'diff_sha256': hashlib.sha256(changes.encode()).hexdigest(),
            'untracked_sha256': files, 'binary_sha256': digest(kettle)}


def run_session(capture, smoke, native, environment, root, directory, argv, kettle,
                startup_timeout, run_timeout):
    manifest = {'status': 'failed', 'documentation': SOURCES, 'environment': environment,
                'platform': {'system': native.system(), 'release': platform.release(),
                             'machine': platform.machine(), 'python': platform.python_version()}}
    old_environment = dict(os.environ)
    old_argv = sys.argv
    error = None
    try:
        os.environ.clear()
        os.environ.update(environment)
        native.acquire_scope()
        manifest['source'] = source_manifest(native, kettle)
        manifest['versions'] = {
            'weston': native.query('weston-version', ['weston', '--version']).strip(),
            'kettle': native.query('kettle-version', [kettle, '--version']).strip(),
            'packages': native.query('package-versions', ['dpkg-query', '-W',
                '-f=${Package} ${Version}\n', 'weston', 'libgl1-mesa-dri',
                'libegl-mesa0', 'mesa-vulkan-drivers', 'fontconfig', 'libfontconfig1',
                'dbus-daemon']).strip()}
        fonts = configure_fonts(root)
        (directory / 'fonts.conf').write_bytes((root / 'fonts.conf').read_bytes())
        listing = native.query('font-list', ['fc-list', '--format', '%{file}\n'])
        manifest['font_files_sha256'] = fonts
        manifest['font_inventory'] = verify_fonts(listing, fonts)
        config = root / 'gpu.cfg'
        config.write_text('record = false\nrestore-session = false\ncheck-for-updates = false\n'
                          + GPU_CONFIG, encoding='utf-8')
        (directory / 'gpu.cfg').write_bytes(config.read_bytes())
        # No activation programs or inherited system/session bus configuration.
        bus_config = root / 'dbus.conf'
        bus_config.write_text('<busconfig><type>session</type><listen>'
                              + escape(environment['DBUS_SESSION_BUS_ADDRESS'])
                              + '</listen><auth>EXTERNAL</auth><policy context="default">'
                              '<allow send_destination="*" eavesdrop="true"/>'
                              '<allow eavesdrop="true"/><allow own="*"/>'
                              '</policy></busconfig>\n', encoding='utf-8')
        (directory / 'dbus.conf').write_bytes(bus_config.read_bytes())
        bus = native.start('dbus', ['dbus-daemon', '--nofork', '--nopidfile',
                                   '--config-file=' + str(bus_config)])
        weston_config = root / 'weston.ini'
        weston_config.write_text('[core]\nxwayland=false\n[shell]\nlocking=false\n'
                                 'panel-position=none\nstartup-animation=none\nanimation=none\n'
                                 'close-animation=none\n[input-method]\npath=\n', encoding='utf-8')
        (directory / 'weston.ini').write_bytes(weston_config.read_bytes())
        weston = native.start('weston', ['weston', '--backend=headless-backend.so', '--renderer=gl',
                              '--shell=desktop-shell.so', '--config=' + str(weston_config),
                              '--socket=' + Path(environment['WAYLAND_DISPLAY']).name,
                              '--width=1600', '--height=1000', '--scale=1', '--idle-time=0'])
        wait_ready(native, [bus, weston], [root / 'bus', Path(environment['WAYLAND_DISPLAY'])],
                   startup_timeout)
        manifest['weston'] = verify_weston(read_text(weston.path))
        manifest['gpu'] = verify_gpu(native.query('gpu-info', [kettle, '--config', str(config), '--gpu-info']))
        save(directory / 'manifest.json', manifest)
        sys.argv = ['capture-ui-states.py', *argv]
        with file_limit(), bounded(run_timeout):
            code = capture.main(smoke_loader=lambda: guarded_smoke(smoke, manifest['gpu'], directory, native))
        if code != 0:
            raise RuntimeError(f'UI-state capture/comparison returned {code}')
        # A compositor/bus crash after the socket appeared must not count as pass.
        if bus.proc.poll() is not None or weston.proc.poll() is not None:
            raise RuntimeError('private compositor/bus exited during captures')
        manifest['weston'] = verify_weston(read_text(weston.path))
        manifest['status'] = 'passed'
    except BaseException as caught:
        error = caught
        manifest['error'] = f'{type(caught).__name__}: {caught}'
    finally:
        try:
            native.cleanup()
        except BaseException as caught:
            manifest['cleanup_error'] = f'{type(caught).__name__}: {caught}'
            manifest['status'] = 'failed'
            error = error or caught
        sys.argv = old_argv
        os.environ.clear()
        os.environ.update(old_environment)
        save(directory / 'manifest.json', manifest)
    if error is not None:
        raise RuntimeError(f'{error}; diagnostics retained at {directory}') from error
    return 0


def main(argv=None, *, system=platform.system):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kettle', default=str(ROOT / 'target/release/kettle'))
    parser.add_argument('--out', default=str(ROOT / 'target/ui-states'))
    parser.add_argument('--compare', default='')
    parser.add_argument('--diagnostics', default=str(ROOT / 'target/ui-wayland-environment'))
    parser.add_argument('--startup-timeout', type=int, choices=range(1, 61), default=20, metavar='1..60')
    parser.add_argument('--run-timeout', type=int, choices=range(1, 901), default=900, metavar='1..900')
    args = parser.parse_args(argv)
    if system() != 'Linux':
        parser.error('private Weston UI-state runner is Linux-only; no process was launched')
    spec = importlib.util.spec_from_file_location('capture_ui_states', ROOT / 'scripts/capture-ui-states.py')
    capture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(capture)
    out, baseline, diagnostics = Path(args.out), Path(args.compare) if args.compare else None, Path(args.diagnostics)
    # Validate before creating diagnostics, loading the live helper, or starting
    # processes. The capture CLI repeats its unchanged output/baseline guards.
    paths = [out, diagnostics, *([baseline] if baseline is not None else [])]
    if out.is_symlink() or diagnostics.is_symlink():
        parser.error('--out/--diagnostics must not be a symlink')
    for index, first in enumerate(paths):
        for second in paths[index + 1:]:
            if (capture.directory_contains(first.resolve(), second.resolve())
                    or capture.directory_contains(second.resolve(), first.resolve())):
                parser.error('output, diagnostics and baseline directories must be disjoint')
    diagnostics.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory = Path(tempfile.mkdtemp(prefix='run-', dir=diagnostics.resolve()))
    print(f'UI Wayland diagnostics: {directory}', flush=True)
    # Short private paths avoid AF_UNIX's 108-byte limit even in long worktrees.
    root = Path(tempfile.mkdtemp(prefix='kettle-ui-wayland-', dir='/tmp'))
    previous_umask = os.umask(0o077)
    old_handlers = {}
    try:
        def interrupted(number, _frame):
            raise DeadlineExpired(f'interrupted by signal {number}')
        for number in (signal.SIGTERM, signal.SIGINT):
            old_handlers[number] = signal.signal(number, interrupted)
        environment = make_environment(capture, root)
        smoke = capture.load_live_smoke()
        native = Native(smoke, environment, directory)
        return run_session(capture, smoke, native, environment, root, directory,
                           ['--kettle', str(Path(args.kettle).resolve()), '--out', str(out),
                            '--languages', 'en,es', '--themes', 'dark,light',
                            '--sizes', 'default,narrow', *(['--compare', str(baseline)] if baseline else [])],
                           str(Path(args.kettle).resolve()), args.startup_timeout, args.run_timeout)
    finally:
        for number, handler in old_handlers.items():
            signal.signal(number, handler)
        os.umask(previous_umask)
        # Preserve the private root on failure as well as the diagnostics. Do
        # not remove sockets or scratch files if cleanup is incomplete.
        manifest = directory / 'manifest.json'
        if manifest.exists() and json.loads(manifest.read_text()).get('status') == 'passed':
            shutil.rmtree(root)


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(f'ui-wayland: {error}', file=sys.stderr)
        raise SystemExit(1)
