#!/usr/bin/env python3
"""Cross-platform live UI diagnostics for Kettle.

The shell scripts remain the Unix-friendly entrypoints. This script exists so
Windows `just` recipes can run the same live tab/underline checks without Bash.
It intentionally uses only Python stdlib plus `kettle ctl`.
"""

from __future__ import annotations

import argparse
import contextlib
import csv
import errno
import hashlib
import io
import json
import math
import os
import platform
import plistlib
import posixpath
import queue
import re
import secrets
import shlex
import shutil
import signal
import subprocess
import stat
import struct
import sys
import tempfile
import threading
import time
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, Callable, ClassVar, Dict, List, Optional, Sequence, Set, Tuple


NVIM_SNAPSHOT_MAX_ENTRIES = 100_000
NVIM_SNAPSHOT_MAX_BYTES = 2 * 1024 * 1024 * 1024
NVIM_SNAPSHOT_MAX_FILE_BYTES = 256 * 1024 * 1024
NVIM_SNAPSHOT_MAX_DEPTH = 64
NVIM_SNAPSHOT_TAR_OVERHEAD_BYTES = (
    NVIM_SNAPSHOT_MAX_ENTRIES * 1024 + 1024 * 1024
)
COPY_CHUNK_BYTES = 1024 * 1024
PROVENANCE_MAX_ENTRIES = 100_000
PROVENANCE_MAX_BYTES = 2 * 1024 * 1024 * 1024
PROVENANCE_MAX_FILE_BYTES = 256 * 1024 * 1024
PROVENANCE_MAX_PATH_LIST_BYTES = 16 * 1024 * 1024
PROVENANCE_TIMEOUT_S = 120.0
PROVENANCE_WORKER_ARG = "--internal-repository-provenance-worker"
PROVENANCE_SABOTAGE_WORKER_ARG = "--internal-repository-provenance-timeout-probe"
PROVENANCE_ANCHOR_PROBE_ARG = "--internal-repository-anchor-probe"
PTY_TRACKER_MAX_BYTES = 64 * 1024
PTY_TRACKER_MAX_RECORDS = 4096
PTY_TRACKER_MAX_PID = (1 << 31) - 1
PTY_TRACKER_SCAN_TIMEOUT_S = 2.0
PTY_TRACKER_FINALIZE_TIMEOUT_S = 5.0
PTY_PROCESS_LIST_MAX_BYTES = 8 * 1024 * 1024
HIDDEN_WINDOW_SCREENSHOT_MESSAGE = (
    "target window is minimized, hidden, or not yet shown; "
    "restore it before capturing"
)
SPLIT_TITLEBAR_COLOR_HEX = {
    "transmit": "#1a7f37",
    "receive": "#0969da",
    "inactive": "#6e7781",
    "grid": "#101010",
}


def is_optional_remote_windows_screenshot_error(
    system: str,
    environment: Dict[str, str],
    *,
    stdout: str,
    stderr: str,
) -> bool:
    """Match only the one non-visual Windows SSH state the smoke permits."""
    if system != "Windows" or not any(
        environment.get(name) for name in ("SSH_CONNECTION", "SSH_CLIENT")
    ):
        return False
    expected = (
        "kettle ctl: server error [busy]: "
        + HIDDEN_WINDOW_SCREENSHOT_MESSAGE
        + "\n"
    )
    return stdout == "" and stderr == expected


@dataclass
class SnapshotCopyBudget:
    max_entries: int = NVIM_SNAPSHOT_MAX_ENTRIES
    max_bytes: int = NVIM_SNAPSHOT_MAX_BYTES
    max_file_bytes: int = NVIM_SNAPSHOT_MAX_FILE_BYTES
    max_depth: int = NVIM_SNAPSHOT_MAX_DEPTH
    entries: int = 0
    bytes: int = 0

    def add_entry(self, source: Path) -> None:
        self.entries += 1
        if self.entries > self.max_entries:
            raise RuntimeError(
                "Neovim snapshot exceeds the "
                f"{self.max_entries} entry limit at {source}"
            )

    def add_file(self, source: Path, size: int) -> None:
        if size < 0 or size > self.max_file_bytes:
            raise RuntimeError(
                "Neovim snapshot file exceeds the "
                f"{self.max_file_bytes} byte per-file limit: {source} ({size} bytes)"
            )
        if self.bytes + size > self.max_bytes:
            raise RuntimeError(
                "Neovim snapshot exceeds the "
                f"{self.max_bytes} aggregate byte limit at {source}"
            )
        self.bytes += size


@dataclass
class RepositoryProvenanceBudget:
    max_entries: int = PROVENANCE_MAX_ENTRIES
    max_scan_entries: int = PROVENANCE_MAX_ENTRIES * 2
    max_bytes: int = PROVENANCE_MAX_BYTES
    max_file_bytes: int = PROVENANCE_MAX_FILE_BYTES
    timeout_s: float = PROVENANCE_TIMEOUT_S
    entries: int = 0
    bytes: int = 0
    started: float = field(default_factory=time.monotonic)

    def remaining(self) -> float:
        remaining = self.timeout_s - (time.monotonic() - self.started)
        if remaining <= 0:
            raise RuntimeError("repository provenance exceeded its time limit")
        return remaining

    def add_bytes(self, amount: int, source: object) -> None:
        if amount < 0 or self.bytes + amount > self.max_bytes:
            raise RuntimeError(
                "repository provenance exceeds the "
                f"{self.max_bytes} aggregate byte limit at {source}"
            )
        self.bytes += amount
        self.remaining()

    def add_file_entry(self, path: Path) -> None:
        self.entries += 1
        if self.entries > self.max_entries:
            raise RuntimeError(
                "repository provenance exceeds the "
                f"{self.max_entries} file limit at {path}"
            )

    def add_file(self, path: Path, size: int) -> None:
        self.add_file_entry(path)
        if size < 0 or size > self.max_file_bytes:
            raise RuntimeError(
                "repository provenance file exceeds the "
                f"{self.max_file_bytes} byte limit: {path} ({size} bytes)"
            )
        self.add_bytes(size, path)


def run(
    argv: List[str],
    *,
    timeout: Optional[float] = None,
    capture: bool = True,
    env: Optional[Dict[str, str]] = None,
) -> subprocess.CompletedProcess:
    return subprocess.run(
        argv,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        timeout=timeout,
        check=False,
        env=env,
    )


def remaining_before(deadline: float, action: str, *, cap: float) -> float:
    """Return a positive per-operation timeout within one absolute deadline."""
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError(f"timed out while {action}")
    return min(cap, remaining)


def terminate_owned_process_group(
    process: subprocess.Popen, *, grace_s: float = 0.5
) -> None:
    """Terminate one Unix process group created by this helper.

    The group is established by ``start_new_session=True`` before Kettle starts.
    portable-pty deliberately creates a second session for each shell, so this
    helper owns only the outer application group; PTY jobs are handled by
    ``terminate_owned_pty_session`` first.
    """
    if os.name == "nt":
        raise RuntimeError("Unix process-group cleanup is unavailable on Windows")
    group = process.pid
    # Do not call poll/wait between the two signals. Even if the leader exits
    # on TERM, retaining its zombie keeps the PID/process-group id unavailable
    # for reuse until the final wait below.
    if process.returncode is None:
        try:
            os.killpg(group, signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            # Darwin reports EPERM when the session contains only exited,
            # unsignalable zombies. No unrelated process can join this new
            # session, so that state is equivalent to an empty live group.
            pass
        time.sleep(grace_s)
        try:
            os.killpg(group, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    try:
        process.wait(timeout=max(1.0, grace_s))
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(
            f"owned process-group leader {process.pid} did not exit"
        ) from error


@dataclass
class StableProcessHandle:
    """A signal target whose identity cannot be redirected by PID reuse.

    Linux pidfds and Darwin audit tokens both name a process instance rather
    than a number in the process table. The live-smoke cleanup deliberately
    refuses the ordinary numeric fallback: failure cleanup is exactly where a
    stale PID must not become a signal to an unrelated developer process.
    """

    pid: int
    identity: Tuple[int, ...]
    pidfd: Optional[int] = None
    audit_token: Optional[Tuple[int, ...]] = None

    @classmethod
    def open(cls, pid: int) -> "StableProcessHandle":
        if pid <= 1:
            raise RuntimeError(f"refusing unsafe process id {pid}")
        system = platform.system()
        if system == "Linux":
            if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
                raise RuntimeError("native Linux cleanup requires Python pidfd support")
            stat_path = Path(f"/proc/{pid}/stat")
            before = stat_path.read_text(encoding="ascii")
            pidfd = os.pidfd_open(pid, 0)
            try:
                after = stat_path.read_text(encoding="ascii")

                def start_time(value: str) -> int:
                    # Field 2 (`comm`) is parenthesized and may contain spaces.
                    # The remainder begins at field 3, making starttime field 22
                    # index 19 after the final close parenthesis.
                    fields = value.rsplit(")", 1)[1].split()
                    if len(fields) < 20:
                        raise ValueError("short /proc stat record")
                    return int(fields[19])

                before_start = start_time(before)
                after_start = start_time(after)
            except BaseException:
                os.close(pidfd)
                raise
            if before_start != after_start:
                os.close(pidfd)
                raise ProcessLookupError(pid)
            return cls(pid=pid, identity=(pid, after_start), pidfd=pidfd)
        if system == "Darwin":
            import ctypes

            library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
            task_self = ctypes.c_uint.in_dll(library, "mach_task_self_").value
            library.task_name_for_pid.argtypes = [
                ctypes.c_uint,
                ctypes.c_int,
                ctypes.POINTER(ctypes.c_uint),
            ]
            library.task_name_for_pid.restype = ctypes.c_int
            library.task_info.argtypes = [
                ctypes.c_uint,
                ctypes.c_int,
                ctypes.POINTER(ctypes.c_uint32),
                ctypes.POINTER(ctypes.c_uint),
            ]
            library.task_info.restype = ctypes.c_int
            library.mach_port_deallocate.argtypes = [ctypes.c_uint, ctypes.c_uint]
            library.mach_port_deallocate.restype = ctypes.c_int
            port = ctypes.c_uint()
            result = library.task_name_for_pid(task_self, pid, ctypes.byref(port))
            if result != 0:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    raise ProcessLookupError(pid) from None
                except PermissionError as error:
                    raise RuntimeError(
                        f"could not prove whether Darwin process {pid} vanished"
                    ) from error
                raise RuntimeError(
                    f"could not retain Darwin task name for {pid}: kern_return={result}"
                )
            token = (ctypes.c_uint32 * 8)()
            count = ctypes.c_uint(8)
            # TASK_AUDIT_TOKEN. Its pidversion field distinguishes a reused PID.
            result = library.task_info(port.value, 15, token, ctypes.byref(count))
            library.mach_port_deallocate(task_self, port.value)
            if result != 0:
                raise RuntimeError(f"could not retain Darwin audit token for {pid}")
            identity = tuple(int(value) for value in token)
            return cls(pid=pid, identity=identity, audit_token=identity)
        raise RuntimeError(
            f"identity-stable Unix cleanup is unsupported on {system or os.name}"
        )

    def signal(self, signal_number: int) -> bool:
        """Signal this exact process, returning false only when it is gone."""
        try:
            if self.pidfd is not None:
                signal.pidfd_send_signal(self.pidfd, signal_number)
                return True
            if self.audit_token is not None:
                import ctypes

                library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
                library.proc_signal_with_audittoken.argtypes = [
                    ctypes.POINTER(ctypes.c_uint32),
                    ctypes.c_int,
                ]
                library.proc_signal_with_audittoken.restype = ctypes.c_int
                token = (ctypes.c_uint32 * 8)(*self.audit_token)
                if library.proc_signal_with_audittoken(token, signal_number) == 0:
                    return True
                error = ctypes.get_errno()
                if error == getattr(os, "ESRCH", 3):
                    return False
                raise OSError(error, os.strerror(error))
        except ProcessLookupError:
            return False
        raise RuntimeError(f"process {self.pid} has no stable signal handle")

    def matches_current(self) -> bool:
        """Whether the numeric PID still denotes the retained identity.

        False proves that the retained instance is no longer at this PID, either
        because the PID vanished or because it was reused. Permission, parser,
        and platform errors are uncertainty, not absence. A caller that also
        owns an append-only numeric record must reopen and independently classify
        the replacement in the same pass rather than silently forgetting it.
        """
        try:
            current = StableProcessHandle.open(self.pid)
        except ProcessLookupError:
            return False
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ESRCH):
                return False
            raise RuntimeError(
                f"could not verify retained process {self.pid}: {error}"
            ) from error
        except (RuntimeError, ValueError) as error:
            raise RuntimeError(
                f"could not verify retained process {self.pid}: {error}"
            ) from error
        try:
            return current.identity == self.identity
        finally:
            current.close()

    def close(self) -> None:
        if self.pidfd is not None:
            os.close(self.pidfd)
            self.pidfd = None


def _is_process_disappearance(error: BaseException) -> bool:
    """Whether ``error`` proves a sampled numeric PID no longer exists."""
    return isinstance(error, ProcessLookupError) or (
        isinstance(error, OSError) and error.errno in (errno.ENOENT, errno.ESRCH)
    )


def _open_stable_process_if_present(
    pid: int, description: str
) -> Optional[StableProcessHandle]:
    """Open ``pid`` or skip only an error that proves it disappeared."""
    try:
        return StableProcessHandle.open(pid)
    except OSError as error:
        if _is_process_disappearance(error):
            return None
        raise RuntimeError(f"could not retain {description} {pid}: {error}") from error


def _linux_process_parent(pid: int) -> int:
    """Read one Linux parent id without being confused by spaces in comm."""
    value = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
    fields = value.rsplit(")", 1)[1].split()
    if len(fields) < 2:
        raise RuntimeError(f"short /proc stat record for {pid}")
    return int(fields[1])


@dataclass
class LinuxSubreaperScope:
    """Make escaped Kettle descendants remain owned by this test process.

    A plugin can call ``setsid`` and later outlive Neovim and Kettle. Process
    groups and a one-time ancestry snapshot cannot contain that transition.
    Linux's child-subreaper contract moves each resulting orphan to this
    process instead of PID 1; a stable-handle baseline then distinguishes the
    already-running Kettle child from newly adopted descendants without ever
    trusting a reusable numeric PID.
    """

    baseline: Set[Tuple[int, ...]]
    closed: bool = False

    # PR_SET_CHILD_SUBREAPER is process-global. Scopes can overlap when a test
    # creates more than one disposable editor tree, so only the last close may
    # restore the state observed by the first acquire. The lock also makes a
    # failed verification/restore atomic with respect to another acquisition.
    _state_lock: ClassVar[threading.RLock] = threading.RLock()
    _active_scopes: ClassVar[int] = 0
    _original_state: ClassVar[Optional[int]] = None

    @staticmethod
    def _library() -> object:
        import ctypes

        library = ctypes.CDLL(None, use_errno=True)
        library.prctl.argtypes = [
            ctypes.c_int,
            ctypes.c_ulong,
            ctypes.c_ulong,
            ctypes.c_ulong,
            ctypes.c_ulong,
        ]
        library.prctl.restype = ctypes.c_int
        return library

    @classmethod
    def _get(cls) -> int:
        import ctypes

        library = cls._library()
        current = ctypes.c_int()
        result = library.prctl(
            37, ctypes.addressof(current), 0, 0, 0
        )  # PR_GET_CHILD_SUBREAPER
        if result != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        return int(current.value)

    @classmethod
    def _set(cls, value: int) -> None:
        import ctypes

        library = cls._library()
        result = library.prctl(36, value, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER
        if result != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))

    @classmethod
    def acquire(cls) -> "LinuxSubreaperScope":
        if platform.system() != "Linux":
            raise RuntimeError("child-subreaper containment is Linux-only")
        with cls._state_lock:
            first = cls._active_scopes == 0
            previous = cls._get()
            if not first and previous != 1:
                raise RuntimeError(
                    "Linux child-subreaper state changed while a scope was active"
                )
            if first:
                cls._original_state = previous
            changed = first and previous == 0
            acquired: List[StableProcessHandle] = []
            try:
                if changed:
                    cls._set(1)
                if cls._get() != 1:
                    raise RuntimeError("Linux refused child-subreaper containment")

                result = run(["ps", "-axo", "pid=,ppid=,uid="], timeout=5)
                if result.returncode != 0:
                    raise RuntimeError(
                        f"could not inventory subreaper children: {result.stderr}"
                    )
                baseline: Set[Tuple[int, ...]] = set()
                owner = os.getpid()
                uid = os.getuid()
                for line in result.stdout.splitlines():
                    fields = line.strip().split()
                    if len(fields) != 3:
                        continue
                    try:
                        pid, parent, process_uid = map(int, fields)
                    except ValueError:
                        continue
                    if parent != owner or process_uid != uid:
                        continue
                    handle = _open_stable_process_if_present(
                        pid, "existing subreaper child"
                    )
                    if handle is None:
                        continue
                    acquired.append(handle)
                    if (
                        handle.matches_current()
                        and _linux_process_parent(pid) == owner
                    ):
                        baseline.add(handle.identity)
                close_errors = _close_stable_process_handles(acquired)
                acquired.clear()
                if close_errors:
                    raise RuntimeError(
                        "could not close subreaper baseline handles: "
                        + "; ".join(str(error) for error in close_errors)
                    )
                cls._active_scopes += 1
                return cls(baseline=baseline)
            except BaseException as error:
                close_errors = _close_stable_process_handles(acquired)
                restore_error: Optional[BaseException] = None
                if first:
                    original = cls._original_state
                    try:
                        if original is not None:
                            cls._set(original)
                            if cls._get() != original:
                                raise RuntimeError(
                                    "Linux refused to restore child-subreaper state"
                                )
                    except BaseException as caught:
                        restore_error = caught
                    finally:
                        cls._original_state = None
                details = [*close_errors]
                if restore_error is not None:
                    details.append(restore_error)
                if details:
                    raise RuntimeError(
                        f"{error}; subreaper rollback failures: "
                        + "; ".join(str(item) for item in details)
                    ) from error
                raise

    def was_present_at_acquire(self, handle: StableProcessHandle) -> bool:
        """Compare process instances, not reusable numeric PIDs."""
        return handle.identity in self.baseline

    def adopted_roots(
        self,
        parents: Dict[int, int],
        owned: Set[int],
        *,
        deadline: Optional[float] = None,
    ) -> Dict[Tuple[int, ...], StableProcessHandle]:
        """Retain new direct children through identity and parent rechecks."""
        if self.closed:
            raise RuntimeError("Linux subreaper scope is already closed")
        roots: Dict[Tuple[int, ...], StableProcessHandle] = {}
        acquired: List[StableProcessHandle] = []
        owner = os.getpid()
        try:
            for pid in owned:
                if deadline is not None:
                    remaining_before(deadline, "retaining adopted children", cap=5)
                if parents.get(pid) != owner:
                    continue
                handle = _open_stable_process_if_present(
                    pid, "adopted subreaper child"
                )
                if handle is None:
                    continue
                acquired.append(handle)
                if (
                    not handle.matches_current()
                    or _linux_process_parent(pid) != owner
                    or self.was_present_at_acquire(handle)
                ):
                    continue
                if (
                    _process_state(handle, deadline=deadline) or ""
                ).startswith("Z"):
                    # Adopted children are ours to reap as well as signal. A
                    # zombie otherwise remains a direct child forever and
                    # makes the quiet scan rediscover the same dead instance.
                    with contextlib.suppress(ChildProcessError, ProcessLookupError):
                        os.waitpid(pid, os.WNOHANG)
                    continue
                roots[handle.identity] = handle

            root_handles = {id(handle) for handle in roots.values()}
            rejected = [handle for handle in acquired if id(handle) not in root_handles]
            close_errors = _close_stable_process_handles(rejected)
            if close_errors:
                # The returned roots have not transferred to the caller yet.
                close_errors.extend(_close_stable_process_handles(roots))
                acquired.clear()
                roots.clear()
                raise RuntimeError(
                    "could not close rejected subreaper handles: "
                    + "; ".join(str(error) for error in close_errors)
                )
            return roots
        except BaseException:
            _close_stable_process_handles(acquired)
            raise

    def close(self) -> None:
        with type(self)._state_lock:
            if self.closed:
                return
            if type(self)._active_scopes <= 0:
                raise RuntimeError("Linux subreaper scope count underflow")
            if type(self)._active_scopes > 1:
                type(self)._active_scopes -= 1
                self.closed = True
                return

            original = type(self)._original_state
            if original is None:
                raise RuntimeError("Linux subreaper original state is missing")
            # Do not mark this scope closed until both the mutation and its
            # verification succeed. A transient failure can then be retried by
            # the caller instead of permanently stranding process-global state.
            try:
                type(self)._set(original)
                if type(self)._get() != original:
                    raise RuntimeError(
                        "Linux refused to restore child-subreaper state"
                    )
            except BaseException as error:
                if original == 0:
                    try:
                        # Keep the still-active scope's contract intact until a
                        # later close retries the restoration.
                        type(self)._set(1)
                    except BaseException as recovery_error:
                        raise RuntimeError(
                            f"{error}; could not re-enable child-subreaper state: "
                            f"{recovery_error}"
                        ) from error
                raise
            type(self)._active_scopes = 0
            type(self)._original_state = None
            self.closed = True


def _close_stable_process_handles(
    handles: object,
) -> List[BaseException]:
    """Close every handle even when one close operation itself fails."""
    errors: List[BaseException] = []
    values = handles.values() if isinstance(handles, dict) else handles
    for handle in values:
        try:
            handle.close()
        except BaseException as error:
            errors.append(error)
    return errors


def _process_state(
    handle: StableProcessHandle, *, deadline: Optional[float] = None
) -> Optional[str]:
    """Read state, then prove the sampled PID still names ``handle``."""
    timeout = (
        remaining_before(deadline, "reading retained process state", cap=2)
        if deadline is not None
        else 2
    )
    sampled = run(["ps", "-o", "stat=", "-p", str(handle.pid)], timeout=timeout)
    if sampled.returncode != 0 or not sampled.stdout.strip():
        return None
    if not handle.matches_current():
        return None
    return sampled.stdout.strip()


def _session_process_handles(
    anchor: StableProcessHandle, session_id: int
) -> Dict[Tuple[int, ...], StableProcessHandle]:
    """Open stable handles for every current member of one anchored session."""
    if not anchor.matches_current() or os.getsid(anchor.pid) != session_id:
        raise RuntimeError(f"PTY session {session_id} lost its retained anchor")
    listed = run(["ps", "-axo", "pid="], timeout=5)
    if listed.returncode != 0:
        raise RuntimeError(f"could not enumerate PTY session: {listed.stderr}")
    handles: Dict[Tuple[int, ...], StableProcessHandle] = {}
    for field in listed.stdout.split():
        try:
            pid = int(field)
        except ValueError:
            continue
        try:
            if os.getsid(pid) != session_id:
                continue
        except ProcessLookupError:
            continue
        except OSError as error:
            close_errors = _close_stable_process_handles(handles)
            detail = (
                "; retained-handle close failures: "
                + "; ".join(str(item) for item in close_errors)
                if close_errors
                else ""
            )
            raise RuntimeError(
                f"could not inspect PTY session member {pid}: {error}{detail}"
            ) from error
        try:
            handle = StableProcessHandle.open(pid)
        except (ProcessLookupError, OSError, RuntimeError) as error:
            try:
                still_member = os.getsid(pid) == session_id
            except ProcessLookupError:
                still_member = False
            if still_member:
                close_errors = _close_stable_process_handles(handles)
                detail = (
                    "; retained-handle close failures: "
                    + "; ".join(str(item) for item in close_errors)
                    if close_errors
                    else ""
                )
                raise RuntimeError(
                    f"could not retain PTY session member {pid}: {error}{detail}"
                ) from error
            continue
        try:
            current_member = os.getsid(pid) == session_id and handle.matches_current()
        except ProcessLookupError:
            current_member = False
        except (OSError, RuntimeError, ValueError) as error:
            close_errors = _close_stable_process_handles([handle, *handles.values()])
            detail = (
                "; stable-handle close failures: "
                + "; ".join(str(item) for item in close_errors)
                if close_errors
                else ""
            )
            raise RuntimeError(
                f"could not recheck PTY session member {pid}: {error}{detail}"
            ) from error
        if not current_member:
            close_errors = _close_stable_process_handles([handle])
            if close_errors:
                retained_errors = _close_stable_process_handles(handles)
                raise RuntimeError(
                    f"could not close stale PTY session member {pid}: "
                    + "; ".join(
                        str(item) for item in [*close_errors, *retained_errors]
                    )
                )
            continue
        handles[handle.identity] = handle
    if anchor.identity not in handles:
        close_errors = _close_stable_process_handles(handles)
        detail = (
            "; stable-handle close failures: "
            + "; ".join(str(item) for item in close_errors)
            if close_errors
            else ""
        )
        raise RuntimeError(
            f"PTY session {session_id} lost its retained anchor{detail}"
        )
    return handles


def terminate_owned_pty_session(
    anchor: StableProcessHandle, *, grace_s: float = 0.5
) -> None:
    """Stop every job in a portable-pty session before Kettle exits.

    Interactive shells put foreground jobs in new process groups, so killing
    either Kettle's outer group or the shell's group alone misses Neovim and
    plugin descendants. Stop all groups while the shell leader still anchors
    the session, then terminate non-shell jobs before killing the leader last.
    """
    if os.name == "nt":
        raise RuntimeError("PTY-session cleanup is unavailable on Windows")

    session_leader = anchor.pid
    if not anchor.matches_current() or os.getsid(session_leader) != session_leader:
        raise RuntimeError(f"PTY session {session_leader} lost its retained anchor")

    # Stop the anchor first, then every member through an identity-stable handle.
    # Re-scan until no newly discovered member remains and every retained live
    # process is observed stopped. A running process never gets to fork after a
    # scan that cleanup calls stable.
    retained: Dict[Tuple[int, ...], StableProcessHandle] = {
        anchor.identity: anchor
    }
    cleanup_error: Optional[BaseException] = None
    try:
        if not anchor.signal(signal.SIGSTOP):
            raise RuntimeError(f"PTY session {session_leader} anchor exited")
        for _attempt in range(8):
            scanned = _session_process_handles(anchor, session_leader)
            new_handles: List[StableProcessHandle] = []
            duplicate_handles: List[StableProcessHandle] = []
            for identity, handle in scanned.items():
                if identity in retained:
                    duplicate_handles.append(handle)
                    continue
                # Transfer every acquired handle into the finalizer-owned set
                # before any close or signal. If either operation raises, later
                # members of this same scan must still be killed and closed.
                retained[identity] = handle
                new_handles.append(handle)
            duplicate_close_errors: List[BaseException] = []
            for handle in duplicate_handles:
                try:
                    handle.close()
                except BaseException as error:
                    duplicate_close_errors.append(error)
            if duplicate_close_errors:
                raise RuntimeError(
                    "could not close duplicate PTY process handles: "
                    + "; ".join(str(error) for error in duplicate_close_errors)
                )
            for handle in new_handles:
                handle.signal(signal.SIGSTOP)
            all_stopped = True
            for handle in retained.values():
                state = _process_state(handle)
                if state is not None and not state.startswith(("T", "Z")):
                    all_stopped = False
            if not new_handles and all_stopped:
                break
            time.sleep(0.02)
        else:
            raise RuntimeError(f"PTY session {session_leader} did not quiesce")
    except BaseException as error:
        cleanup_error = error
    finally:
        # Even a failed enumeration has already stopped at least the anchor.
        # Never abandon those processes in T state: kill every retained instance
        # and the wrapper anchor before surfacing the original error.
        kill_errors: List[BaseException] = []
        for identity, handle in retained.items():
            if identity == anchor.identity:
                continue
            try:
                handle.signal(signal.SIGKILL)
            except BaseException as error:
                kill_errors.append(error)
        try:
            anchor.signal(signal.SIGKILL)
        except BaseException as error:
            kill_errors.append(error)
        for identity, handle in retained.items():
            if identity != anchor.identity:
                try:
                    handle.close()
                except BaseException as error:
                    kill_errors.append(error)
    del grace_s
    if cleanup_error is not None:
        if kill_errors:
            raise RuntimeError(
                f"{cleanup_error}; forced PTY cleanup also failed: "
                + "; ".join(str(error) for error in kill_errors)
            ) from cleanup_error
        raise cleanup_error
    if kill_errors:
        raise RuntimeError(
            "forced PTY cleanup failed: "
            + "; ".join(str(error) for error in kill_errors)
        )


def process_exited_without_reaping(process: subprocess.Popen) -> bool:
    """Observe a Unix child exit while preserving its PID/group anchor."""
    if os.name == "nt":
        return process.poll() is not None
    if process.returncode is not None:
        return True
    result = os.waitid(
        os.P_PID,
        process.pid,
        os.WEXITED | os.WNOHANG | os.WNOWAIT,
    )
    return result is not None


def native_visible_window_ids(pid: int) -> Set[int]:
    """Return OS-owned visible top-level windows for one process.

    Control-protocol inventories describe Kettle's logical map. This independent
    inventory is what proves a removed ``WindowState`` did not leave a mapped
    native window behind.
    """
    system = platform.system()
    if system == "Windows":
        import ctypes
        from ctypes import wintypes

        user32 = ctypes.WinDLL("user32", use_last_error=True)
        callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        user32.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
        user32.EnumWindows.restype = wintypes.BOOL
        user32.GetWindowThreadProcessId.argtypes = [
            wintypes.HWND,
            ctypes.POINTER(wintypes.DWORD),
        ]
        user32.GetWindowThreadProcessId.restype = wintypes.DWORD
        user32.IsWindowVisible.argtypes = [wintypes.HWND]
        user32.IsWindowVisible.restype = wintypes.BOOL
        user32.GetClassNameW.argtypes = [
            wintypes.HWND,
            wintypes.LPWSTR,
            ctypes.c_int,
        ]
        user32.GetClassNameW.restype = ctypes.c_int
        found: Set[int] = set()

        @callback_type
        def collect(window: int, _parameter: int) -> bool:
            owner = wintypes.DWORD()
            user32.GetWindowThreadProcessId(window, ctypes.byref(owner))
            if owner.value == pid and user32.IsWindowVisible(window):
                class_name = ctypes.create_unicode_buffer(256)
                user32.GetClassNameW(window, class_name, len(class_name))
                # winit creates one visible 16x16 message target per event
                # loop on Windows. It owns no user surface but EnumWindows and
                # IsWindowVisible both include it, so counting it makes one
                # real Kettle window look like two. Match the framework's
                # explicit class instead of using a size/title heuristic that
                # could hide a legitimate tiny or untitled Kettle window.
                if class_name.value != "Winit Thread Event Target":
                    found.add(int(window))
            return True

        if not user32.EnumWindows(collect, 0):
            raise ctypes.WinError(ctypes.get_last_error())
        return found

    if system == "Darwin":
        import ctypes

        core_graphics = ctypes.CDLL(
            "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"
        )
        core_foundation = ctypes.CDLL(
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
        )
        core_graphics.CGWindowListCopyWindowInfo.argtypes = [
            ctypes.c_uint32,
            ctypes.c_uint32,
        ]
        core_graphics.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
        core_foundation.CFArrayGetCount.argtypes = [ctypes.c_void_p]
        core_foundation.CFArrayGetCount.restype = ctypes.c_long
        core_foundation.CFArrayGetValueAtIndex.argtypes = [
            ctypes.c_void_p,
            ctypes.c_long,
        ]
        core_foundation.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
        core_foundation.CFDictionaryGetValue.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        core_foundation.CFDictionaryGetValue.restype = ctypes.c_void_p
        core_foundation.CFNumberGetValue.argtypes = [
            ctypes.c_void_p,
            ctypes.c_long,
            ctypes.c_void_p,
        ]
        core_foundation.CFNumberGetValue.restype = ctypes.c_bool
        core_foundation.CFRelease.argtypes = [ctypes.c_void_p]

        def cg_key(name: str) -> ctypes.c_void_p:
            return ctypes.c_void_p.in_dll(core_graphics, name)

        owner_key = cg_key("kCGWindowOwnerPID")
        number_key = cg_key("kCGWindowNumber")
        layer_key = cg_key("kCGWindowLayer")

        def number(dictionary: int, key: ctypes.c_void_p) -> Optional[int]:
            value = core_foundation.CFDictionaryGetValue(dictionary, key)
            if not value:
                return None
            output = ctypes.c_longlong()
            # kCFNumberSInt64Type = 4.
            if not core_foundation.CFNumberGetValue(value, 4, ctypes.byref(output)):
                return None
            return int(output.value)

        # On-screen + excluding desktop elements. Layer zero is an ordinary
        # application window rather than a tooltip/menu owned by the process.
        windows = core_graphics.CGWindowListCopyWindowInfo((1 << 0) | (1 << 4), 0)
        if not windows:
            raise RuntimeError("CGWindowListCopyWindowInfo returned no inventory")
        found = set()
        try:
            for index in range(core_foundation.CFArrayGetCount(windows)):
                item = core_foundation.CFArrayGetValueAtIndex(windows, index)
                if number(item, owner_key) == pid and number(item, layer_key) == 0:
                    window_number = number(item, number_key)
                    if window_number is not None:
                        found.add(window_number)
        finally:
            core_foundation.CFRelease(windows)
        return found

    if system == "Linux":
        xdotool = shutil.which("xdotool")
        if xdotool is None:
            raise RuntimeError(
                "window-close-isolation requires xdotool for an independent "
                "native X11 window inventory"
            )
        result = run(
            [xdotool, "search", "--onlyvisible", "--pid", str(pid)],
            timeout=5,
        )
        if result.returncode == 1 and not result.stdout.strip():
            return set()
        if result.returncode != 0:
            raise RuntimeError(
                "xdotool could not enumerate native Kettle windows: "
                f"rc={result.returncode} stderr={result.stderr.strip()!r}"
            )
        try:
            return {int(line) for line in result.stdout.splitlines() if line.strip()}
        except ValueError as error:
            raise RuntimeError(
                f"xdotool returned malformed window ids: {result.stdout!r}"
            ) from error

    raise RuntimeError(f"native window inventory is unsupported on {system}")


def wait_for_native_window_ids(
    pid: int,
    accept: Callable[[Set[int]], bool],
    *,
    label: str,
    timeout_s: float = 8.0,
) -> Set[int]:
    deadline = time.monotonic() + timeout_s
    observed: Set[int] = set()
    while time.monotonic() < deadline:
        observed = native_visible_window_ids(pid)
        if accept(observed):
            return observed
        time.sleep(0.05)
    raise RuntimeError(
        f"timed out waiting for {label}; native window ids were {sorted(observed)}"
    )


@dataclass
class NvimSidebarWaitState:
    """Pure state machine behind the LazyVCS marker/pager wait."""

    marker: str
    quiet_s: float
    stable_key: Optional[Tuple[str, str, str, int]] = None
    stable_since: Optional[float] = None
    pager_key: Optional[Tuple[str, str]] = None
    pager_dismissed_at: Optional[float] = None

    def observe(
        self,
        visible: str,
        snapshot: str,
        cursor: object,
        history_size: int,
        now: float,
    ) -> Tuple[bool, bool]:
        """Return ``(ready, dismiss_pager)`` for one screen observation."""
        prompt = "Press ENTER or type command to continue" in visible
        pager_key = (snapshot, visible)
        dismiss = prompt and (
            pager_key != self.pager_key
            or self.pager_dismissed_at is None
            or now - self.pager_dismissed_at >= 0.5
        )
        if dismiss:
            self.pager_dismissed_at = now
        self.pager_key = pager_key if prompt else None
        key = (snapshot, visible, json.dumps(cursor, sort_keys=True), history_size)
        if self.marker not in visible or prompt:
            self.stable_key = None
            self.stable_since = None
            return False, dismiss
        if key != self.stable_key:
            self.stable_key = key
            self.stable_since = now
            return self.quiet_s <= 0, dismiss
        if self.stable_since is None:
            self.stable_since = now
        return now - self.stable_since >= self.quiet_s, dismiss


def path_is_link(path: Path) -> bool:
    """Recognize symlinks and Windows reparse points without following them.

    ``Path.is_junction`` exists only on Python 3.12+, while the live-smoke
    helper has no such interpreter floor. Windows exposes the reparse bit on
    ``lstat`` metadata on every supported Python, so cleanup remains fail-closed
    on older runners too.
    """
    if path.is_symlink():
        return True
    if platform.system() == "Windows":
        try:
            attributes = int(getattr(os.lstat(path), "st_file_attributes"))
        except FileNotFoundError:
            return False
        return bool(
            attributes & int(getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))
        )
    is_junction = getattr(path, "is_junction", None)
    return callable(is_junction) and bool(is_junction())


def assert_no_reparse_ancestry(path: Path) -> None:
    absolute = Path(os.path.abspath(path))
    ancestry = [absolute, *absolute.parents]
    for candidate in reversed(ancestry):
        if candidate.exists() and path_is_link(candidate):
            raise RuntimeError(
                f"refusing live-smoke path with reparse ancestry: {candidate}"
            )


def windows_system_executable(*relative_parts: str) -> str:
    system_root = os.environ.get("SYSTEMROOT")
    if system_root:
        candidate = Path(system_root).joinpath(*relative_parts)
        if candidate.is_file():
            assert_no_reparse_ancestry(candidate)
            return str(candidate.resolve())
    fallback = shutil.which(relative_parts[-1])
    if fallback is None:
        raise RuntimeError(
            f"required Windows system executable is missing: {relative_parts[-1]}"
        )
    fallback_path = Path(fallback)
    assert_no_reparse_ancestry(fallback_path)
    return str(fallback_path.resolve())


class WindowsKillJob:
    """Contain one worker tree in a kill-on-close Windows Job Object."""

    def __init__(self, *, named: bool = False) -> None:
        if platform.system() != "Windows":
            raise RuntimeError("Windows Job Objects are unavailable on this platform")
        import ctypes
        from ctypes import wintypes

        class IoCounters(ctypes.Structure):
            _fields_ = [(name, ctypes.c_ulonglong) for name in (
                "ReadOperationCount",
                "WriteOperationCount",
                "OtherOperationCount",
                "ReadTransferCount",
                "WriteTransferCount",
                "OtherTransferCount",
            )]

        class BasicLimits(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_longlong),
                ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class ExtendedLimits(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BasicLimits),
                ("IoInfo", IoCounters),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        class BasicAccounting(ctypes.Structure):
            _fields_ = [
                ("TotalUserTime", ctypes.c_longlong),
                ("TotalKernelTime", ctypes.c_longlong),
                ("ThisPeriodTotalUserTime", ctypes.c_longlong),
                ("ThisPeriodTotalKernelTime", ctypes.c_longlong),
                ("TotalPageFaultCount", wintypes.DWORD),
                ("TotalProcesses", wintypes.DWORD),
                ("ActiveProcesses", wintypes.DWORD),
                ("TotalTerminatedProcesses", wintypes.DWORD),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        kernel32.CreateJobObjectW.restype = wintypes.HANDLE
        kernel32.SetInformationJobObject.argtypes = [
            wintypes.HANDLE,
            ctypes.c_int,
            ctypes.c_void_p,
            wintypes.DWORD,
        ]
        kernel32.SetInformationJobObject.restype = wintypes.BOOL
        kernel32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        kernel32.AssignProcessToJobObject.restype = wintypes.BOOL
        kernel32.QueryInformationJobObject.argtypes = [
            wintypes.HANDLE,
            ctypes.c_int,
            ctypes.c_void_p,
            wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
        ]
        kernel32.QueryInformationJobObject.restype = wintypes.BOOL
        kernel32.TerminateJobObject.argtypes = [wintypes.HANDLE, wintypes.UINT]
        kernel32.TerminateJobObject.restype = wintypes.BOOL
        kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel32.WaitForSingleObject.restype = wintypes.DWORD
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle.restype = wintypes.BOOL

        name = f"Local\\KettleSmoke-{secrets.token_hex(16)}" if named else None
        ctypes.set_last_error(0)
        job = kernel32.CreateJobObjectW(None, name)
        if not job:
            raise ctypes.WinError(ctypes.get_last_error())
        if name is not None and ctypes.get_last_error() == 183:  # ERROR_ALREADY_EXISTS
            kernel32.CloseHandle(job)
            raise RuntimeError("an unpredictable Windows Job Object name collided")
        limits = ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = 0x00002000  # KILL_ON_JOB_CLOSE
        if not kernel32.SetInformationJobObject(
            job, 9, ctypes.byref(limits), ctypes.sizeof(limits)
        ):
            error = ctypes.WinError(ctypes.get_last_error())
            kernel32.CloseHandle(job)
            raise error
        self._ctypes = ctypes
        self._kernel32 = kernel32
        self._job = job
        self._name = name
        self._basic_accounting_type = BasicAccounting
        self._dword_type = wintypes.DWORD

    def assign(self, process: subprocess.Popen) -> None:
        raw_process = getattr(process, "_handle", None)
        if raw_process is None or not self._kernel32.AssignProcessToJobObject(
            self._job, raw_process
        ):
            raise self._ctypes.WinError(self._ctypes.get_last_error())

    def powershell_assign_current_process_command(self) -> str:
        """Build an in-pane assignment with no reusable numeric PID handoff."""
        if self._name is None:
            raise RuntimeError("PowerShell self-assignment requires a named Job Object")
        source = """using System;
using System.Runtime.InteropServices;
public static class KettleSmokeNativeJob {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern IntPtr OpenJobObject(uint access, bool inherit, string name);
  [DllImport("kernel32.dll", SetLastError=true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
  [DllImport("kernel32.dll")]
  public static extern IntPtr GetCurrentProcess();
  [DllImport("kernel32.dll", SetLastError=true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool CloseHandle(IntPtr handle);
}"""
        source_one_line = " ".join(source.splitlines())
        return (
            "if (-not ('KettleSmokeNativeJob' -as [type])) { "
            f"Add-Type -TypeDefinition {shell_quote(source_one_line, windows=True)} "
            "-ErrorAction Stop; }; "
            "$KettleSmokeJob=[KettleSmokeNativeJob]::OpenJobObject(1,$false,"
            f"{shell_quote(self._name, windows=True)}); "
            "if ($KettleSmokeJob -eq [IntPtr]::Zero) { "
            "throw [ComponentModel.Win32Exception]::new("
            "[Runtime.InteropServices.Marshal]::GetLastWin32Error()); }; "
            "try { if (-not [KettleSmokeNativeJob]::AssignProcessToJobObject("
            "$KettleSmokeJob,[KettleSmokeNativeJob]::GetCurrentProcess())) { "
            "throw [ComponentModel.Win32Exception]::new("
            "[Runtime.InteropServices.Marshal]::GetLastWin32Error()); } } "
            "finally { if (-not [KettleSmokeNativeJob]::CloseHandle("
            "$KettleSmokeJob)) { throw [ComponentModel.Win32Exception]::new("
            "[Runtime.InteropServices.Marshal]::GetLastWin32Error()); } }"
        )

    def active_processes(self) -> int:
        if self._job is None:
            raise RuntimeError("Windows Job Object is already closed")
        accounting = self._basic_accounting_type()
        returned = self._dword_type()
        if not self._kernel32.QueryInformationJobObject(
            self._job,
            1,  # JobObjectBasicAccountingInformation
            self._ctypes.byref(accounting),
            self._ctypes.sizeof(accounting),
            self._ctypes.byref(returned),
        ):
            raise self._ctypes.WinError(self._ctypes.get_last_error())
        return int(accounting.ActiveProcesses)

    def terminate(self) -> None:
        if self._job is not None and not self._kernel32.TerminateJobObject(
            self._job, 1
        ):
            raise self._ctypes.WinError(self._ctypes.get_last_error())

    def wait_empty(self, timeout_s: float = 5.0) -> None:
        """Wait for the Job's kernel object to signal that every process exited."""
        if self._job is None:
            raise RuntimeError("Windows Job Object is already closed")
        timeout_ms = max(0, min(round(timeout_s * 1000), 0xFFFFFFFE))
        result = self._kernel32.WaitForSingleObject(self._job, timeout_ms)
        if result == 0:  # WAIT_OBJECT_0
            active = self.active_processes()
            if active != 0:
                raise RuntimeError(
                    "Windows Job was signaled while it still contained "
                    f"{active} process(es)"
                )
            return
        if result == 0x00000102:  # WAIT_TIMEOUT
            raise RuntimeError(
                "Windows Job still contains "
                f"{self.active_processes()} process(es) after {timeout_s:g}s"
            )
        if result == 0xFFFFFFFF:  # WAIT_FAILED
            raise self._ctypes.WinError(self._ctypes.get_last_error())
        raise RuntimeError(f"unexpected Windows Job wait result: 0x{result:08x}")

    def close(self) -> None:
        if self._job is None:
            return
        job = self._job
        self._job = None
        if not self._kernel32.CloseHandle(job):
            raise self._ctypes.WinError(self._ctypes.get_last_error())


def _terminate_windows_job(job: WindowsKillJob) -> None:
    """Terminate and close a Job Object, reporting every cleanup failure."""
    errors: List[BaseException] = []
    try:
        job.terminate()
    except BaseException as error:
        errors.append(error)
    if not errors:
        try:
            # TerminateJobObject requests asynchronous termination. Deletion may
            # begin only after accounting proves every process has actually left.
            job.wait_empty()
        except BaseException as error:
            errors.append(error)
    try:
        # KILL_ON_JOB_CLOSE is an independent fail-closed stop path.
        job.close()
    except BaseException as error:
        errors.append(error)
    if errors:
        raise RuntimeError(
            "Windows Job cleanup failed: " + "; ".join(str(error) for error in errors)
        )


def _drain_then_remove(
    drain: Callable[[], None], remove: Callable[[], None]
) -> None:
    """Enforce the sandbox lifecycle: descendants drain before deletion."""
    drain()
    remove()


def _remove_tree_by_fd(root: Path) -> None:
    """Remove one Unix tree without re-resolving a checked ancestor.

    Every recursive open is relative to a retained directory descriptor and
    rejects links. Permission restoration uses ``fchmod`` only after the opened
    inode matches the no-follow observation; unlink/rmdir likewise operate
    relative to the held parent. A same-user process can race names, but no
    mutation occurs until the intended directory object is bound to an fd.

    A directory without enough permission to be opened is retained and cleanup
    fails closed. That is preferable to a path-based chmod: Python does not
    provide portable no-follow ``chmod(dir_fd=...)`` support on every Unix, and
    mutating a name before opening it lets a replacement receive the chmod.
    """
    if os.name == "nt":
        raise RuntimeError("descriptor-relative tree removal is Unix-only")
    try:
        expected_root = root.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISLNK(expected_root.st_mode) or not stat.S_ISDIR(expected_root.st_mode):
        raise RuntimeError(f"sandbox root is not a plain directory: {root}")
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    parent_fd = os.open(root.parent, flags)
    root_fd: Optional[int] = None

    def remove_contents(directory_fd: int, display: Path) -> None:
        with os.scandir(directory_fd) as scanned:
            entries = list(scanned)
        for entry in entries:
            entry_path = display / entry.name
            before = entry.stat(follow_symlinks=False)
            if stat.S_ISDIR(before.st_mode):
                child_fd = os.open(entry.name, flags, dir_fd=directory_fd)
                try:
                    opened = os.fstat(child_fd)
                    if (opened.st_dev, opened.st_ino) != (
                        before.st_dev,
                        before.st_ino,
                    ):
                        raise RuntimeError(
                            f"sandbox directory changed while opening: {entry_path}"
                        )
                    os.fchmod(child_fd, stat.S_IRWXU)
                    remove_contents(child_fd, entry_path)
                    # POSIX permits removing an empty directory while it is
                    # open. Keep the descriptor until the name is gone so no
                    # replacement can receive a later operation through it.
                    os.rmdir(entry.name, dir_fd=directory_fd)
                finally:
                    os.close(child_fd)
            else:
                # unlink never follows a symlink, FIFO, socket, or device.
                os.unlink(entry.name, dir_fd=directory_fd)

    try:
        root_fd = os.open(root.name, flags, dir_fd=parent_fd)
        opened_root = os.fstat(root_fd)
        if (opened_root.st_dev, opened_root.st_ino) != (
            expected_root.st_dev,
            expected_root.st_ino,
        ):
            raise RuntimeError(f"sandbox root changed while opening: {root}")
        os.fchmod(root_fd, stat.S_IRWXU)
        remove_contents(root_fd, root)
        os.rmdir(root.name, dir_fd=parent_fd)
    finally:
        if root_fd is not None:
            os.close(root_fd)
        os.close(parent_fd)


class RepositoryWorkerTimeout(RuntimeError):
    """A hard provenance timeout whose detached reaper remains observable."""

    def __init__(self, message: str, reaped: threading.Event) -> None:
        super().__init__(message)
        self.reaped = reaped


def windows_current_user_sid() -> str:
    cp = run(
        [
            windows_system_executable("System32", "whoami.exe"),
            "/user",
            "/fo",
            "csv",
            "/nh",
        ],
        timeout=10,
        capture=True,
    )
    if cp.returncode != 0:
        raise RuntimeError(
            "could not resolve the current Windows user SID: "
            f"rc={cp.returncode} stderr={cp.stderr.strip()!r}"
        )
    try:
        row = next(csv.reader([cp.stdout.strip()]))
    except (csv.Error, StopIteration) as error:
        raise RuntimeError(
            f"could not parse the current Windows user SID: {cp.stdout!r}"
        ) from error
    if len(row) != 2 or re.fullmatch(r"S-\d+(?:-\d+)+", row[1]) is None:
        raise RuntimeError(
            f"whoami returned an invalid Windows user SID: {cp.stdout!r}"
        )
    return row[1]


def harden_windows_private_directory(path: Path) -> None:
    """Replace inherited grants with current-user and SYSTEM full control."""
    if platform.system() != "Windows":
        raise RuntimeError("Windows private-directory hardening requires Windows")
    assert_no_reparse_ancestry(path)
    if path_is_link(path) or not path.is_dir():
        raise RuntimeError(f"refusing non-directory or linked private path: {path}")
    sid = windows_current_user_sid()
    acl_script = r"""
$ErrorActionPreference = 'Stop'
$path = $env:KETTLE_PRIVATE_DIRECTORY
$user = [System.Security.Principal.SecurityIdentifier]::new(
    $env:KETTLE_PRIVATE_USER_SID
)
$system = [System.Security.Principal.SecurityIdentifier]::new('S-1-5-18')
$acl = [System.Security.AccessControl.DirectorySecurity]::new()
$acl.SetOwner($user)
$acl.SetAccessRuleProtection($true, $false)
$inheritance = (
    [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
    [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
)
$propagation = [System.Security.AccessControl.PropagationFlags]::None
$allow = [System.Security.AccessControl.AccessControlType]::Allow
foreach ($identity in @($user, $system)) {
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $identity,
        [System.Security.AccessControl.FileSystemRights]::FullControl,
        $inheritance,
        $propagation,
        $allow
    )
    [void]$acl.AddAccessRule($rule)
}
[System.IO.Directory]::SetAccessControl($path, $acl)
$verified = [System.IO.Directory]::GetAccessControl($path)
$protected = $verified.AreAccessRulesProtected
if (-not $protected) {
    throw 'private directory still inherits access rules'
}
$owner = $verified.GetOwner(
    [System.Security.Principal.SecurityIdentifier]
).Value
if ($owner -ne $user.Value) {
    throw "private directory owner mismatch: $owner"
}
$rules = @(
    $verified.GetAccessRules(
        $true,
        $true,
        [System.Security.Principal.SecurityIdentifier]
    )
)
if ($rules.Count -ne 2) {
    throw "private directory has $($rules.Count) explicit access rules"
}
$expected = @($user.Value, $system.Value)
foreach ($rule in $rules) {
    if (
        $rule.IdentityReference.Value -notin $expected -or
        $rule.IsInherited -or
        $rule.AccessControlType -ne $allow -or
        $rule.FileSystemRights -ne
            [System.Security.AccessControl.FileSystemRights]::FullControl -or
        $rule.InheritanceFlags -ne $inheritance -or
        $rule.PropagationFlags -ne $propagation
    ) {
        throw "unexpected private-directory access rule: $rule"
    }
}
Write-Output 'PRIVATE_ACL_OK'
"""
    child_env = os.environ.copy()
    child_env["KETTLE_PRIVATE_DIRECTORY"] = str(path)
    child_env["KETTLE_PRIVATE_USER_SID"] = sid
    cp = run(
        [
            windows_system_executable(
                "System32",
                "WindowsPowerShell",
                "v1.0",
                "powershell.exe",
            ),
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            acl_script,
        ],
        timeout=30,
        capture=True,
        env=child_env,
    )
    if cp.returncode != 0 or cp.stdout.strip() != "PRIVATE_ACL_OK":
        raise RuntimeError(
            "could not protect Windows live-smoke directory: "
            f"rc={cp.returncode} stdout={cp.stdout.strip()!r} "
            f"stderr={cp.stderr.strip()!r}"
        )
    assert_no_reparse_ancestry(path)
    if path_is_link(path) or not path.is_dir():
        raise RuntimeError(
            f"Windows live-smoke directory changed during hardening: {path}"
        )


def windows_live_smoke_parent() -> Path:
    local_app_data = os.environ.get("LOCALAPPDATA")
    if not local_app_data:
        raise RuntimeError("LOCALAPPDATA is required for Windows live UI smokes")
    local_app_data_path = Path(os.path.abspath(local_app_data))
    assert_no_reparse_ancestry(local_app_data_path)
    parent = local_app_data_path.resolve()
    if path_is_link(local_app_data_path) or not parent.is_dir():
        raise RuntimeError(
            f"refusing linked or missing Windows local app-data root: {parent}"
        )
    app_root = parent / "kettle"
    app_root.mkdir(exist_ok=True)
    assert_no_reparse_ancestry(app_root)
    if path_is_link(app_root) or app_root.resolve().parent != parent:
        raise RuntimeError(
            f"refusing linked or escaped Windows Kettle data root: {app_root}"
        )
    return app_root.resolve()


def create_windows_private_directory(prefix: str) -> Path:
    if re.fullmatch(r"kettle-[a-z0-9-]+-", prefix) is None:
        raise ValueError(f"refusing unsafe Windows private-directory prefix: {prefix}")
    parent = windows_live_smoke_parent()
    root = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    try:
        if path_is_link(root):
            raise RuntimeError(f"private directory is unexpectedly linked: {root}")
        resolved = root.resolve()
        if resolved.parent != parent or not resolved.name.startswith(prefix):
            raise RuntimeError(f"private directory escaped its parent: {resolved}")
        harden_windows_private_directory(resolved)
        return resolved
    except Exception:
        shutil.rmtree(root, ignore_errors=True)
        raise


def create_default_diagnostic_root(*, windows: Optional[bool] = None) -> Path:
    use_windows = platform.system() == "Windows" if windows is None else windows
    if use_windows:
        if platform.system() != "Windows":
            raise RuntimeError(
                "a Windows diagnostic root can only be created on Windows"
            )
        return create_windows_private_directory("kettle-live-ui-diagnostics-")
    root = Path("target/diagnostics").resolve()
    root.mkdir(parents=True, exist_ok=True)
    return root


def release_kettle_artifact_from_messages(messages: str) -> Path:
    """Return Cargo's exact release executable from JSON build messages."""
    executables: List[Path] = []
    for line in messages.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        if not isinstance(target, dict):
            continue
        kinds = target.get("kind")
        executable = message.get("executable")
        if (
            target.get("name") == "kettle"
            and isinstance(kinds, list)
            and "bin" in kinds
            and isinstance(executable, str)
        ):
            path = Path(executable).resolve()
            if path not in executables:
                executables.append(path)
    if len(executables) != 1:
        raise RuntimeError(
            "cargo did not report exactly one kettle release executable "
            f"(found {len(executables)}: {executables})"
        )
    return executables[0]


def resolve_release_kettle() -> str:
    """Build and select the current checkout's actual Cargo artifact."""
    if shutil.which("cargo") is None:
        raise RuntimeError("--cargo-release requires cargo on the host PATH")
    cp = run(
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "-p",
            "kettle",
            "--message-format=json-render-diagnostics",
        ],
        timeout=None,
        capture=True,
    )
    if cp.returncode != 0:
        rendered: List[str] = []
        for line in cp.stdout.splitlines():
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            diagnostic = message.get("message")
            if isinstance(diagnostic, dict) and isinstance(
                diagnostic.get("rendered"), str
            ):
                rendered.append(diagnostic["rendered"])
        detail = "".join(rendered[-5:]) or cp.stdout[-4000:]
        raise RuntimeError(
            "failed to build the current checkout's release executable:\n"
            f"{detail}\n{cp.stderr[-4000:]}"
        )
    executable = release_kettle_artifact_from_messages(cp.stdout)
    if not executable.is_file():
        raise RuntimeError(f"Cargo-reported Kettle executable is missing: {executable}")
    print(
        f"live-ui smoke: Cargo selected release executable {executable}",
        file=sys.stderr,
    )
    return str(executable)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        while True:
            chunk = fh.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def stream_git_output(
    repository: Path,
    args: List[str],
    budget: RepositoryProvenanceBudget,
    consume: Callable[[bytes], None],
) -> None:
    """Stream one Git query through a single aggregate time/byte budget."""
    process = subprocess.Popen(
        [
            "git",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-C",
            str(repository),
            *args,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    errors: List[BaseException] = []
    stderr = bytearray()

    def read_stdout() -> None:
        assert process.stdout is not None
        try:
            while chunk := process.stdout.read(COPY_CHUNK_BYTES):
                budget.add_bytes(len(chunk), f"git {' '.join(args)}")
                consume(chunk)
        except BaseException as error:
            errors.append(error)
            process.kill()

    def read_stderr() -> None:
        assert process.stderr is not None
        while chunk := process.stderr.read(4096):
            # Retain only a bounded diagnostic tail while continuing to drain.
            stderr.extend(chunk)
            if len(stderr) > 16 * 1024:
                del stderr[: len(stderr) - 16 * 1024]

    stdout_thread = threading.Thread(target=read_stdout, daemon=True)
    stderr_thread = threading.Thread(target=read_stderr, daemon=True)
    stdout_thread.start()
    stderr_thread.start()
    try:
        returncode = process.wait(timeout=budget.remaining())
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.wait(timeout=5)
        raise RuntimeError(
            f"git {' '.join(args)} exceeded the repository provenance time limit"
        ) from error
    finally:
        stdout_thread.join(timeout=5)
        stderr_thread.join(timeout=5)
    if stdout_thread.is_alive() or stderr_thread.is_alive():
        raise RuntimeError(f"git {' '.join(args)} output reader did not finish")
    if errors:
        raise RuntimeError(str(errors[0])) from errors[0]
    if returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} failed while recording provenance: "
            + bytes(stderr).decode("utf-8", "replace")[-2000:]
        )


@contextlib.contextmanager
def held_repository_directory(repository: Path, relative: Path):
    """Hold one repository directory chain without following links.

    Unix resolves each component relative to an already-open directory. Windows
    retains no-delete-share handles for every component and rejects reparse
    points, so later leaf lookup cannot be redirected through a junction.
    """
    if relative.is_absolute() or ".." in relative.parts:
        raise RuntimeError(f"unsafe repository directory: {relative}")
    parts = tuple(part for part in relative.parts if part not in ("", "."))
    if platform.system() == "Windows":
        import ctypes
        from ctypes import wintypes

        class FileAttributeTagInfo(ctypes.Structure):
            _fields_ = [
                ("file_attributes", wintypes.DWORD),
                ("reparse_tag", wintypes.DWORD),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.LPVOID,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        ]
        kernel32.CreateFileW.restype = wintypes.HANDLE
        kernel32.GetFileInformationByHandleEx.argtypes = [
            wintypes.HANDLE,
            ctypes.c_int,
            wintypes.LPVOID,
            wintypes.DWORD,
        ]
        kernel32.GetFileInformationByHandleEx.restype = wintypes.BOOL
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        invalid = wintypes.HANDLE(-1).value
        handles: List[object] = []
        current = repository
        try:
            for part in (None, *parts):
                if part is not None:
                    current /= part
                handle = kernel32.CreateFileW(
                    str(current),
                    0x0080,  # FILE_READ_ATTRIBUTES
                    0x0001 | 0x0002,  # share read/write, deliberately not delete
                    None,
                    3,  # OPEN_EXISTING
                    0x02000000 | 0x00200000,  # BACKUP_SEMANTICS | OPEN_REPARSE_POINT
                    None,
                )
                if handle == invalid:
                    raise OSError(
                        ctypes.get_last_error(),
                        f"cannot hold repository directory {current}",
                    )
                handles.append(handle)
                info = FileAttributeTagInfo()
                if not kernel32.GetFileInformationByHandleEx(
                    handle, 9, ctypes.byref(info), ctypes.sizeof(info)
                ):
                    raise OSError(
                        ctypes.get_last_error(),
                        f"cannot inspect repository directory {current}",
                    )
                if info.file_attributes & 0x0400:  # FILE_ATTRIBUTE_REPARSE_POINT
                    raise RuntimeError(
                        f"repository provenance rejects a linked directory: {current}"
                    )
            yield None, current
        finally:
            for handle in reversed(handles):
                kernel32.CloseHandle(handle)
        return

    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptors: List[int] = []
    try:
        descriptor = os.open(repository, flags)
        descriptors.append(descriptor)
        for part in parts:
            descriptor = os.open(part, flags, dir_fd=descriptor)
            descriptors.append(descriptor)
        yield descriptor, repository.joinpath(*parts)
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def repository_file_digest(
    repository: Path, relative: Path, budget: RepositoryProvenanceBudget
) -> Tuple[int, bytes]:
    """Hash one untracked regular file through a retained directory chain."""
    path = repository / relative
    parent = relative.parent
    name = relative.name
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    digest = hashlib.sha256()
    read_bytes = 0
    with held_repository_directory(repository, parent) as (parent_fd, _parent_path):
        if parent_fd is None:
            before = path.lstat()
            if path_is_link(path):
                raise RuntimeError(
                    f"repository provenance rejects a special file: {path}"
                )
            fd = os.open(path, flags)
        else:
            before = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
            fd = os.open(name, flags, dir_fd=parent_fd)
        try:
            if not stat.S_ISREG(before.st_mode):
                raise RuntimeError(
                    f"repository provenance rejects a special file: {path}"
                )
            opened = os.fstat(fd)
            if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (
                before.st_dev,
                before.st_ino,
            ):
                raise RuntimeError(f"repository file changed while opening: {path}")
            # The retained handle is the stability boundary.  On NTFS,
            # ``lstat`` and the immediately following ``fstat`` can report
            # different ``st_ctime_ns`` values for an untouched file.  The
            # pathname snapshot still prevents an object substitution between
            # discovery and open; using the handle on both sides of the read
            # catches any mutation while hashing without that Windows false
            # positive.
            budget.add_file(path, opened.st_size)
            while chunk := os.read(fd, COPY_CHUNK_BYTES):
                read_bytes += len(chunk)
                if read_bytes > opened.st_size:
                    raise RuntimeError(f"repository file grew while hashing: {path}")
                digest.update(chunk)
                budget.remaining()
            after = os.fstat(fd)
        finally:
            os.close(fd)
    stable = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    changed = [
        name
        for name in stable
        if getattr(opened, name) != getattr(after, name)
    ]
    if read_bytes != opened.st_size:
        changed.append("bytes_read")
    if changed:
        raise RuntimeError(
            f"repository file changed while hashing ({', '.join(changed)}): {path}"
        )
    return read_bytes, digest.digest()


def _repository_source_identity_impl(
    repository: Path, budget: Optional[RepositoryProvenanceBudget] = None
) -> Dict[str, object]:
    """Fingerprint source under one bounded streaming budget."""
    digest = hashlib.sha256()
    active_budget = budget or RepositoryProvenanceBudget()
    def git_path_list(args: List[str]) -> bytearray:
        output = bytearray()

        def collect_paths(chunk: bytes) -> None:
            if len(output) + len(chunk) > PROVENANCE_MAX_PATH_LIST_BYTES:
                raise RuntimeError(
                    "repository provenance exceeds the "
                    f"{PROVENANCE_MAX_PATH_LIST_BYTES} byte pathname limit"
                )
            output.extend(chunk)

        stream_git_output(repository, args, active_budget, collect_paths)
        if output and output[-1] != 0:
            raise RuntimeError(
                f"git returned a truncated pathname list for {' '.join(args)}"
            )
        return output

    # Status is provenance too: it distinguishes clean from dirty trees and is
    # compared before/after the live run. Keep it inside this contained worker
    # instead of running a second, capturing `git status` in the parent. The
    # NUL form has no quoting ambiguity; each record and rename companion
    # consumes the same traversal cap used by the filesystem walk.
    status_digest = hashlib.sha256()
    status_bytes = 0
    status_entries = 0
    status_last_byte: Optional[int] = None

    def consume_status(chunk: bytes) -> None:
        nonlocal status_bytes, status_entries, status_last_byte
        status_bytes += len(chunk)
        if status_bytes > PROVENANCE_MAX_PATH_LIST_BYTES:
            raise RuntimeError(
                "repository provenance exceeds the "
                f"{PROVENANCE_MAX_PATH_LIST_BYTES} byte status limit"
            )
        status_entries += chunk.count(b"\0")
        if status_entries > active_budget.max_scan_entries:
            raise RuntimeError(
                "repository provenance exceeds the "
                f"{active_budget.max_scan_entries} status-entry limit"
            )
        status_digest.update(chunk)
        if chunk:
            status_last_byte = chunk[-1]

    stream_git_output(
        repository,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        active_budget,
        consume_status,
    )
    if status_bytes and status_last_byte != 0:
        raise RuntimeError("git returned a truncated repository status")

    tracked = git_path_list(["ls-files", "-z", "--cached"])
    tracked_paths: Set[bytes] = set()
    for raw_path in tracked.split(b"\0"):
        if raw_path and bytes(raw_path) not in tracked_paths:
            tracked_path = bytes(raw_path)
            tracked_paths.add(tracked_path)
            active_budget.add_file_entry(Path(os.fsdecode(tracked_path)))

    queries = (
        [
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ],
        [
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ],
    )
    for args in queries:
        encoded = "\0".join(args).encode("utf-8")
        digest.update(b"git-diff\0" + len(encoded).to_bytes(8, "big") + encoded)
        stream_git_output(repository, list(args), active_budget, digest.update)

    # Porcelain names alone do not notice edits to an already-untracked file.
    # The NUL list has its own memory cap; file bodies are opened and streamed
    # one at a time, rejecting links/devices/FIFOs rather than blocking on them.

    ignored = git_path_list(
        [
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ]
    )
    ignored_paths = {
        os.fsdecode(bytes(path)).replace(os.sep, "/").rstrip("/")
        for path in ignored.split(b"\0")
        if path
    }

    # Git intentionally omits FIFOs/devices/sockets from `ls-files --others`.
    # Walk the non-ignored tree under the same file/time bound so such an entry
    # is rejected rather than silently absent from the identity.
    scanned = 0
    pending = [Path()]
    while pending:
        relative_dir = pending.pop()
        with held_repository_directory(repository, relative_dir) as (
            directory_fd,
            current_path,
        ):
            scan_target: object = directory_fd if directory_fd is not None else current_path
            with os.scandir(scan_target) as entries:
                for entry in entries:
                    active_budget.remaining()
                    scanned += 1
                    if scanned > active_budget.max_scan_entries:
                        raise RuntimeError(
                            "repository provenance exceeds the "
                            f"{active_budget.max_scan_entries} worktree-entry scan limit"
                        )
                    relative_path = relative_dir / entry.name
                    relative = relative_path.as_posix()
                    if relative == ".git" or relative.rstrip("/") in ignored_paths:
                        continue
                    candidate = repository / relative_path
                    linked = entry.is_symlink()
                    if platform.system() == "Windows":
                        linked = linked or path_is_link(candidate)
                    if linked:
                        raise RuntimeError(
                            f"repository provenance rejects a linked entry: {candidate}"
                        )
                    try:
                        info = entry.stat(follow_symlinks=False)
                    except OSError as error:
                        raise RuntimeError(
                            f"cannot inspect repository entry {candidate}: {error}"
                        ) from error
                    if stat.S_ISDIR(info.st_mode):
                        pending.append(relative_path)
                    elif not stat.S_ISREG(info.st_mode):
                        raise RuntimeError(
                            f"repository provenance rejects a special file: {candidate}"
                        )

    untracked = git_path_list(
        ["ls-files", "-z", "--others", "--exclude-standard"],
    )
    for raw_path in untracked.split(b"\0"):
        if not raw_path:
            continue
        raw_path = bytes(raw_path)
        relative = Path(os.fsdecode(raw_path))
        if relative.is_absolute() or ".." in relative.parts:
            raise RuntimeError(f"git returned an unsafe untracked path: {relative}")
        size, file_digest = repository_file_digest(
            repository, relative, active_budget
        )
        digest.update(b"untracked\0")
        digest.update(len(raw_path).to_bytes(8, "big"))
        digest.update(raw_path)
        digest.update(size.to_bytes(8, "big"))
        digest.update(file_digest)
    return {
        "source_state_sha256": digest.hexdigest(),
        "git_dirty": status_entries > 0,
        "git_status_sha256": status_digest.hexdigest(),
        "git_status_entries": status_entries,
    }


def _repository_provenance_worker(argv: List[str]) -> int:
    """Run the filesystem pass in a process the caller can time out hard."""
    if len(argv) != 2:
        print("repository provenance worker expects a path and budget", file=sys.stderr)
        return 2
    try:
        limits = json.loads(argv[1])
        budget = RepositoryProvenanceBudget(
            max_entries=int(limits["max_entries"]),
            max_scan_entries=int(limits["max_scan_entries"]),
            max_bytes=int(limits["max_bytes"]),
            max_file_bytes=int(limits["max_file_bytes"]),
            timeout_s=float(limits["timeout_s"]),
        )
        identity = _repository_source_identity_impl(Path(argv[0]), budget)
    except BaseException as error:
        print(f"{type(error).__name__}: {error}", file=sys.stderr)
        return 1
    print(json.dumps(identity, sort_keys=True, separators=(",", ":")))
    return 0


def _repository_provenance_timeout_probe(argv: List[str]) -> int:
    """Test-only worker that creates a descendant and never exits itself."""
    if len(argv) != 1:
        print("repository provenance timeout probe expects a pid record", file=sys.stderr)
        return 2
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
    )
    record = Path(argv[0])
    staged = record.with_name(f".{record.name}.{os.getpid()}.tmp")
    staged.write_text(f"{os.getpid()} {child.pid}\n", encoding="ascii")
    os.replace(staged, record)
    while True:
        time.sleep(60)


def _start_unix_repository_group_anchor() -> Optional[int]:
    """Keep an internal worker's private process-group id owned until cleanup.

    ``Popen.communicate`` reaps a completed worker before the parent can kill
    ordinary helpers which inherited its process group.  Without a remaining
    member the numeric PGID can be reused in that interval, turning ``killpg``
    into a signal to an unrelated process.  This silent child closes every
    controller pipe and remains in the group until the controller kills it, so
    the kernel cannot recycle the PGID after the worker leader is reaped.
    """
    if os.name == "nt":
        return None
    previous_hangup = signal.signal(signal.SIGHUP, signal.SIG_IGN)
    anchor = os.fork()
    if anchor != 0:
        signal.signal(signal.SIGHUP, previous_hangup)
        return anchor
    # The group leader exits after publishing its result. Ignore the orphaned
    # session's hangup so this anchor lives exactly until the controller's
    # SIGKILL; otherwise the PGID becomes reusable before cleanup begins.
    for descriptor in (0, 1, 2):
        with contextlib.suppress(OSError):
            os.close(descriptor)
    while True:
        signal.pause()


def _eventually_reap_process(
    process: subprocess.Popen, reaped: threading.Event
) -> None:
    """Reap a worker asynchronously after its containing tree was killed."""
    did_reap = False
    try:
        try:
            # Resume a timed-out communicate so its Windows pipe-reader
            # threads finish as well as the process handle itself.
            process.communicate()
        except (OSError, ValueError):
            process.wait()
        did_reap = True
    except BaseException:
        # The event is an ownership proof used by timeout tests and callers.
        # Signalling it after both reap paths failed would turn a leaked process
        # handle into a passing cleanup assertion. The daemon reaper cannot make
        # this exceptional object safe, so leave the event unset and let the
        # observer report the failed transfer.
        pass
    finally:
        _close_repository_worker_streams(process)
        if did_reap:
            reaped.set()


def _close_repository_worker_streams(process: subprocess.Popen) -> None:
    for stream in (process.stdin, process.stdout, process.stderr):
        if stream is not None:
            with contextlib.suppress(OSError, ValueError):
                stream.close()


def _stop_repository_worker(
    worker: subprocess.Popen,
    job: Optional[WindowsKillJob],
    *,
    terminate_job: bool = True,
) -> Tuple[List[BaseException], threading.Event]:
    """Kill the entire worker tree without adding a second blocking deadline."""
    errors: List[BaseException] = []
    if job is not None:
        if terminate_job:
            try:
                job.terminate()
            except BaseException as error:
                errors.append(error)
        try:
            # KILL_ON_JOB_CLOSE is a second fail-closed termination path.
            job.close()
        except BaseException as error:
            errors.append(error)
    else:
        try:
            os.killpg(worker.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except BaseException as error:
            errors.append(error)
            with contextlib.suppress(OSError):
                worker.kill()
    if worker.stdin is not None:
        with contextlib.suppress(OSError, ValueError):
            worker.stdin.close()
    reaped = threading.Event()
    if worker.returncode is not None:
        # `communicate`/`wait` already reaped a completed worker. On Unix the
        # internal worker's still-live group anchor keeps this PGID unavailable
        # for reuse until the kill above. Ownership transfer must not add a
        # fresh timeout after the caller's absolute deadline.
        _close_repository_worker_streams(worker)
        reaped.set()
    else:
        threading.Thread(
            target=_eventually_reap_process,
            args=(worker, reaped),
            name="kettle-provenance-reaper",
            daemon=True,
        ).start()
    return errors, reaped


def _run_repository_worker(argv: List[str], timeout_s: float) -> Tuple[int, str, str]:
    """Launch, contain, handshake, and bound one provenance worker tree."""
    deadline = time.monotonic() + timeout_s
    launched: "queue.Queue[Tuple[Optional[subprocess.Popen], Optional[WindowsKillJob], Optional[BaseException]]]" = queue.Queue(maxsize=1)
    abandoned = threading.Event()
    accepted = threading.Event()

    def launch() -> None:
        worker: Optional[subprocess.Popen] = None
        job: Optional[WindowsKillJob] = None
        try:
            if os.name == "nt":
                job = WindowsKillJob()
            worker = subprocess.Popen(
                argv,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
                start_new_session=os.name != "nt",
            )
            if job is not None:
                # The worker blocks on stdin until assignment succeeds, so it
                # cannot create Git/timeout-probe descendants outside the job.
                job.assign(worker)
            launched.put((worker, job, None))
            while not accepted.wait(0.01):
                if abandoned.is_set():
                    _stop_repository_worker(worker, job)
                    return
        except BaseException as error:
            if worker is not None:
                _stop_repository_worker(worker, job)
            elif job is not None:
                with contextlib.suppress(BaseException):
                    job.close()
            launched.put((None, None, error))

    threading.Thread(
        target=launch,
        name="kettle-provenance-launcher",
        daemon=True,
    ).start()
    try:
        worker, job, launch_error = launched.get(
            timeout=max(0.0, deadline - time.monotonic())
        )
    except queue.Empty as error:
        abandoned.set()
        raise RuntimeError(
            "repository provenance exceeded its hard time limit during launch"
        ) from error
    accepted.set()
    if launch_error is not None:
        raise RuntimeError(
            f"repository provenance worker could not start: {launch_error}"
        ) from launch_error
    assert worker is not None
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        abandoned.set()
        cleanup_errors, reaped = _stop_repository_worker(worker, job)
        detail = (
            ": " + "; ".join(str(error) for error in cleanup_errors)
            if cleanup_errors
            else ""
        )
        raise RepositoryWorkerTimeout(
            "repository provenance exceeded its hard time limit during launch"
            + detail,
            reaped,
        )
    try:
        stdout, stderr = worker.communicate(input="1", timeout=remaining)
    except subprocess.TimeoutExpired as error:
        cleanup_errors, reaped = _stop_repository_worker(worker, job)
        detail = (
            ": " + "; ".join(str(item) for item in cleanup_errors)
            if cleanup_errors
            else ""
        )
        raise RepositoryWorkerTimeout(
            "repository provenance exceeded its hard time limit" + detail,
            reaped,
        ) from error
    except BaseException as error:
        cleanup_errors, _reaped = _stop_repository_worker(worker, job)
        detail = (
            ": " + "; ".join(str(item) for item in cleanup_errors)
            if cleanup_errors
            else ""
        )
        raise RuntimeError(
            f"repository provenance worker communication failed: {error}{detail}"
        ) from error
    assert worker.returncode is not None
    # A Git operation may have launched a helper which outlives the worker.
    # User-configured fsmonitor processes are disabled at launch. Closing the
    # Windows Job kills the remaining tree; on Unix, kill the private process
    # group after every completed result, including nonzero results. POSIX
    # cannot contain a deliberately detached `setsid` descendant, so this is a
    # cleanup boundary for ordinary inherited-group helpers, not a sandbox.
    cleanup_errors, reaped = _stop_repository_worker(worker, job)
    if cleanup_errors:
        raise RuntimeError(
            "repository provenance worker cleanup failed: "
            + "; ".join(str(item) for item in cleanup_errors)
        )
    if not reaped.is_set():
        raise RuntimeError("completed repository provenance worker was not reaped")
    return worker.returncode, stdout, stderr


def _internal_repository_worker_argv(
    worker_arg: str, *arguments: object
) -> List[str]:
    """Start an internal worker without importing user-controlled Python code.

    Windows cannot assign a process to a Job Object until after process
    creation.  ``-I -S`` closes the only Python-level pre-assignment execution
    paths: environment/user-site configuration, ``sitecustomize``, and ``.pth``
    files.  The worker then blocks on the explicit stdin handshake before it
    can launch Git or a timeout-probe descendant.
    """
    return [
        sys.executable,
        "-I",
        "-S",
        str(Path(__file__).resolve()),
        worker_arg,
        *(str(argument) for argument in arguments),
    ]


def isolated_python_shebang() -> str:
    """Shebang for an internal script with no user Python startup hooks."""
    interpreter = shlex.join([str(Path(sys.executable).resolve()), "-I", "-S"])
    return f"#!/usr/bin/env -S {interpreter}"


def repository_source_identity(
    repository: Path, budget: Optional[RepositoryProvenanceBudget] = None
) -> Dict[str, object]:
    """Fingerprint source and status under one contained absolute deadline."""
    active_budget = budget or RepositoryProvenanceBudget()
    limits = {
        "max_entries": active_budget.max_entries,
        "max_scan_entries": active_budget.max_scan_entries,
        "max_bytes": active_budget.max_bytes,
        "max_file_bytes": active_budget.max_file_bytes,
        "timeout_s": active_budget.timeout_s,
    }
    returncode, stdout, stderr = _run_repository_worker(
        _internal_repository_worker_argv(
            PROVENANCE_WORKER_ARG,
            repository,
            json.dumps(limits, separators=(",", ":")),
        ),
        active_budget.remaining(),
    )
    # The first stderr line carries the worker's typed error. A successful,
    # fully typed identity is the only zero-status output accepted below.
    if returncode != 0:
        raise RuntimeError(
            "repository provenance worker failed: " + stderr.strip()[-4000:]
        )
    try:
        identity = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            "repository provenance worker returned invalid JSON: "
            f"{stdout.strip()!r}"
        ) from error
    if not isinstance(identity, dict):
        raise RuntimeError("repository provenance worker returned a non-object")
    expected = {
        "source_state_sha256": str,
        "git_dirty": bool,
        "git_status_sha256": str,
        "git_status_entries": int,
    }
    for field_name, field_type in expected.items():
        if not isinstance(identity.get(field_name), field_type):
            raise RuntimeError(
                f"repository provenance worker returned an invalid {field_name}"
            )
    for digest_field in ("source_state_sha256", "git_status_sha256"):
        if re.fullmatch(r"[0-9a-f]{64}", str(identity[digest_field])) is None:
            raise RuntimeError(
                "repository provenance worker returned an invalid "
                f"{digest_field}: {identity[digest_field]!r}"
            )
    if int(identity["git_status_entries"]) < 0:
        raise RuntimeError(
            "repository provenance worker returned a negative status entry count"
        )
    return identity


def repository_source_sha256(
    repository: Path, budget: Optional[RepositoryProvenanceBudget] = None
) -> str:
    """Return the bounded source digest from the contained identity worker."""
    return str(repository_source_identity(repository, budget)["source_state_sha256"])


def _regular_file_digest(
    path: Path, budget: SnapshotCopyBudget, *, count_entry: bool = True
) -> Tuple[int, str]:
    """Hash one stable regular file without following a link at the leaf."""
    before = path.lstat()
    if path_is_link(path) or not stat.S_ISREG(before.st_mode):
        raise RuntimeError(f"identity only accepts a regular file: {path}")
    if count_entry:
        budget.add_entry(path)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    digest = hashlib.sha256()
    read_bytes = 0
    try:
        opened = os.fstat(fd)
        if not stat.S_ISREG(opened.st_mode):
            raise RuntimeError(f"identity opened a non-regular file: {path}")
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise RuntimeError(f"identity path changed while opening: {path}")
        budget.add_file(path, opened.st_size)
        while chunk := os.read(fd, COPY_CHUNK_BYTES):
            read_bytes += len(chunk)
            if read_bytes > opened.st_size:
                raise RuntimeError(f"identity file grew while hashing: {path}")
            digest.update(chunk)
        after = os.fstat(fd)
    finally:
        os.close(fd)

    stable_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    if read_bytes != opened.st_size or any(
        getattr(opened, field) != getattr(after, field) for field in stable_fields
    ):
        raise RuntimeError(f"identity file changed while hashing: {path}")
    return read_bytes, digest.hexdigest()


def _regular_file_digest_at(
    parent_fd: int,
    name: str,
    display_path: Path,
    before: os.stat_result,
    budget: SnapshotCopyBudget,
) -> Tuple[int, str]:
    """Hash a file relative to a retained parent directory descriptor."""
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(name, flags, dir_fd=parent_fd)
    digest = hashlib.sha256()
    read_bytes = 0
    try:
        opened = os.fstat(fd)
        if not stat.S_ISREG(opened.st_mode):
            raise RuntimeError(f"identity opened a non-regular file: {display_path}")
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise RuntimeError(f"identity path changed while opening: {display_path}")
        budget.add_file(display_path, opened.st_size)
        while chunk := os.read(fd, COPY_CHUNK_BYTES):
            read_bytes += len(chunk)
            if read_bytes > opened.st_size:
                raise RuntimeError(f"identity file grew while hashing: {display_path}")
            digest.update(chunk)
        after = os.fstat(fd)
    finally:
        os.close(fd)
    fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    if read_bytes != opened.st_size or any(
        getattr(opened, field) != getattr(after, field) for field in fields
    ):
        raise RuntimeError(f"identity file changed while hashing: {display_path}")
    return read_bytes, digest.hexdigest()


def regular_file_identity(path: Path) -> Dict[str, object]:
    """Identify exact executable bytes under the snapshot's regular-file limits."""
    resolved = path.resolve(strict=True)
    size, digest = _regular_file_digest(resolved, SnapshotCopyBudget())
    return {
        "path": str(resolved),
        "bytes": size,
        "sha256": digest,
    }


def read_bounded_regular_text(path: Path, *, max_bytes: int = 4096) -> str:
    """Read a small stable regular file without following its final component."""
    before = path.lstat()
    if (
        path_is_link(path)
        or not stat.S_ISREG(before.st_mode)
        or before.st_size > max_bytes
    ):
        raise RuntimeError(f"expected a small regular identity record: {path}")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        opened = os.fstat(fd)
        if not stat.S_ISREG(opened.st_mode) or (
            opened.st_dev,
            opened.st_ino,
        ) != (before.st_dev, before.st_ino):
            raise RuntimeError(f"identity record changed while opening: {path}")
        data = os.read(fd, max_bytes + 1)
        if len(data) > max_bytes or os.read(fd, 1):
            raise RuntimeError(f"identity record exceeds {max_bytes} bytes: {path}")
        after = os.fstat(fd)
    finally:
        os.close(fd)
    fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(opened, field) != getattr(after, field) for field in fields):
        raise RuntimeError(f"identity record changed while reading: {path}")
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError(f"identity record is not UTF-8: {path}") from error


def _bounded_sorted_directory_entries(
    directory: Path,
    budget: SnapshotCopyBudget,
    scan_factory: Optional[Callable[[Path], object]] = None,
) -> List[os.DirEntry]:
    """Retain no more directory entries than the budget permits."""
    entries: List[os.DirEntry] = []
    scanner = scan_factory or os.scandir
    with scanner(directory) as scanned:  # type: ignore[attr-defined]
        for entry in scanned:  # type: ignore[union-attr]
            budget.add_entry(Path(entry.path))
            entries.append(entry)
    entries.sort(key=lambda entry: entry.name)
    return entries


def regular_tree_identity(
    root: Path, budget: Optional[SnapshotCopyBudget] = None
) -> Dict[str, object]:
    """Hash a bounded regular tree without following links or special files."""
    try:
        root_stat = root.lstat()
    except FileNotFoundError:
        return {"path": str(root), "present": False}
    if path_is_link(root) or not stat.S_ISDIR(root_stat.st_mode):
        raise RuntimeError(f"identity root is not a plain directory: {root}")

    active_budget = budget or SnapshotCopyBudget()
    active_budget.add_entry(root)
    digest = hashlib.sha256()
    files = 0

    def record_entry(kind: bytes, relative_path: Path, content: bytes = b"") -> None:
        relative = relative_path.as_posix().encode("utf-8")
        digest.update(kind)
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(content)

    def walk_by_path(directory: Path, relative_dir: Path, depth: int) -> None:
        nonlocal files
        if depth > active_budget.max_depth:
            raise RuntimeError(
                f"Neovim snapshot exceeds the {active_budget.max_depth} "
                f"depth limit at {directory}"
            )
        before = directory.lstat()
        if path_is_link(directory) or not stat.S_ISDIR(before.st_mode):
            raise RuntimeError(f"identity encountered a linked directory: {directory}")
        # Count before retaining. A single enormous directory can hold at most
        # the remaining permitted entries in memory, rather than being fully
        # materialized before the limit is checked.
        entries = _bounded_sorted_directory_entries(directory, active_budget)
        for entry in entries:
            path = Path(entry.path)
            relative_path = relative_dir / entry.name
            entry_stat = entry.stat(follow_symlinks=False)
            if path_is_link(path):
                raise RuntimeError(f"identity rejects a symlink or junction: {path}")
            if stat.S_ISDIR(entry_stat.st_mode):
                record_entry(b"d", relative_path)
                walk_by_path(path, relative_path, depth + 1)
                continue
            if not stat.S_ISREG(entry_stat.st_mode):
                raise RuntimeError(f"identity rejects a non-regular entry: {path}")
            file_size, file_digest = _regular_file_digest(
                path, active_budget, count_entry=False
            )
            record_entry(b"f", relative_path, bytes.fromhex(file_digest))
            files += 1
        after = directory.lstat()
        fields = ("st_dev", "st_ino", "st_mtime_ns", "st_ctime_ns")
        if any(getattr(before, field) != getattr(after, field) for field in fields):
            raise RuntimeError(f"identity directory changed while hashing: {directory}")

    def walk_by_fd(
        directory_fd: int, directory: Path, relative_dir: Path, depth: int
    ) -> None:
        nonlocal files
        if depth > active_budget.max_depth:
            raise RuntimeError(
                f"Neovim snapshot exceeds the {active_budget.max_depth} "
                f"depth limit at {directory}"
            )
        before = os.fstat(directory_fd)
        entries: List[os.DirEntry] = []
        with os.scandir(directory_fd) as scanned:
            for entry in scanned:
                active_budget.add_entry(directory / entry.name)
                entries.append(entry)
        entries.sort(key=lambda entry: entry.name)
        directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        directory_flags |= getattr(os, "O_NOFOLLOW", 0)
        for entry in entries:
            display_path = directory / entry.name
            relative_path = relative_dir / entry.name
            entry_stat = entry.stat(follow_symlinks=False)
            if stat.S_ISLNK(entry_stat.st_mode):
                raise RuntimeError(
                    f"identity rejects a symlink or junction: {display_path}"
                )
            if stat.S_ISDIR(entry_stat.st_mode):
                child_fd = os.open(
                    entry.name, directory_flags, dir_fd=directory_fd
                )
                try:
                    opened = os.fstat(child_fd)
                    if (opened.st_dev, opened.st_ino) != (
                        entry_stat.st_dev,
                        entry_stat.st_ino,
                    ):
                        raise RuntimeError(
                            f"identity directory changed while opening: {display_path}"
                        )
                    record_entry(b"d", relative_path)
                    walk_by_fd(child_fd, display_path, relative_path, depth + 1)
                finally:
                    os.close(child_fd)
                continue
            if not stat.S_ISREG(entry_stat.st_mode):
                raise RuntimeError(
                    f"identity rejects a non-regular entry: {display_path}"
                )
            _file_size, file_digest = _regular_file_digest_at(
                directory_fd,
                entry.name,
                display_path,
                entry_stat,
                active_budget,
            )
            record_entry(b"f", relative_path, bytes.fromhex(file_digest))
            files += 1
        after = os.fstat(directory_fd)
        fields = ("st_dev", "st_ino", "st_mtime_ns", "st_ctime_ns")
        if any(getattr(before, field) != getattr(after, field) for field in fields):
            raise RuntimeError(f"identity directory changed while hashing: {directory}")

    if os.name == "nt":
        # Windows has no Python dir_fd/openat surface. Snapshot roots are still
        # reparse-checked at every level; native Windows process containment is
        # provided by the retained kill-on-close Job. Unix uses the stronger
        # descriptor-relative walk below because detached daemons can escape a
        # process group there.
        walk_by_path(root, Path(), 0)
    else:
        directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        directory_flags |= getattr(os, "O_NOFOLLOW", 0)
        root_fd = os.open(root, directory_flags)
        try:
            opened_root = os.fstat(root_fd)
            if (opened_root.st_dev, opened_root.st_ino) != (
                root_stat.st_dev,
                root_stat.st_ino,
            ):
                raise RuntimeError(f"identity root changed while opening: {root}")
            walk_by_fd(root_fd, root, Path(), 0)
        finally:
            os.close(root_fd)
    return {
        "path": str(root),
        "present": True,
        "files": files,
        "bytes": active_budget.bytes,
        "sha256": digest.hexdigest(),
    }


def agent_tui_provenance(
    kettle: str, shell_target: "AgentShellTarget"
) -> Dict[str, object]:
    """Record enough identity to tie ignored live artifacts to one build."""
    executable = Path(kettle)
    if not executable.is_file():
        resolved = shutil.which(kettle)
        if resolved is None:
            raise RuntimeError(
                f"cannot resolve kettle executable for provenance: {kettle}"
            )
        executable = Path(resolved)
    executable = executable.resolve()
    version = run([str(executable), "--version"], timeout=10)
    script = Path(__file__).resolve()
    repository = script.parent.parent
    commit = run(
        ["git", "-C", str(repository), "rev-parse", "HEAD"], timeout=10
    )
    repository_identity = repository_source_identity(repository)
    nvim_status, nvim_path = shell_target.command_resolution("nvim")
    nvim_version = (
        shell_target.run_command(["nvim", "--version"], timeout=15)
        if nvim_status == "available"
        else None
    )
    nvim_file = (
        shell_target.nvim_file_identity(nvim_path)
        if nvim_status == "available" and nvim_path is not None
        else None
    )
    return {
        "executable": str(executable),
        "executable_sha256": sha256_file(executable),
        "version": version.stdout.strip() if version.returncode == 0 else None,
        "version_stderr": (
            version.stderr.strip() if version.returncode != 0 else None
        ),
        "harness": str(script),
        "harness_sha256": sha256_file(script),
        "git_commit": commit.stdout.strip() if commit.returncode == 0 else None,
        "git_dirty": repository_identity["git_dirty"],
        "git_status_sha256": repository_identity["git_status_sha256"],
        "git_status_entries": repository_identity["git_status_entries"],
        "source_state_sha256": repository_identity["source_state_sha256"],
        "target": {
            "mode": shell_target.mode,
            "wsl_distro": shell_target.wsl_distro,
            "nvim_path": nvim_path,
            "nvim_file": nvim_file,
            "nvim_version": (
                nvim_version.stdout.splitlines()[0]
                if nvim_version is not None
                and nvim_version.returncode == 0
                and nvim_version.stdout.splitlines()
                else None
            ),
            "nvim_config_source": shell_target.nvim_config_source(),
            "nvim_data_source": shell_target.nvim_data_source(),
        },
    }


def require_cmd(cmd: str) -> None:
    if shutil.which(cmd) is None:
        raise SystemExit(f"live-ui smoke: cannot run ({cmd} not found)")


def missing_commands(*commands: str) -> List[str]:
    return [command for command in commands if shutil.which(command) is None]


def read_rgba_png(path: Path) -> Tuple[int, int, List[bytes]]:
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos = 8
    width = height = None
    raw = b""
    while pos < len(data):
        n = struct.unpack(">I", data[pos : pos + 4])[0]
        pos += 4
        typ = data[pos : pos + 4]
        pos += 4
        chunk = data[pos : pos + n]
        pos += n + 4
        if typ == b"IHDR":
            width, height, bit_depth, color_type, _, _, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
            if (bit_depth, color_type, interlace) != (8, 6, 0):
                raise SystemExit(f"{path}: expected non-interlaced 8-bit RGBA PNG")
        elif typ == b"IDAT":
            raw += chunk
        elif typ == b"IEND":
            break
    if width is None or height is None:
        raise SystemExit(f"{path}: missing IHDR")

    decoded = zlib.decompress(raw)
    bpp = 4
    stride = width * bpp
    rows: List[bytes] = []
    prev = [0] * stride
    i = 0
    for _ in range(height):
        filt = decoded[i]
        i += 1
        cur = list(decoded[i : i + stride])
        i += stride
        recon = [0] * stride
        for x, value in enumerate(cur):
            left = recon[x - bpp] if x >= bpp else 0
            up = prev[x]
            up_left = prev[x - bpp] if x >= bpp else 0
            if filt == 0:
                out = value
            elif filt == 1:
                out = value + left
            elif filt == 2:
                out = value + up
            elif filt == 3:
                out = value + ((left + up) // 2)
            elif filt == 4:
                p = left + up - up_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                predictor = left if pa <= pb and pa <= pc else (up if pb <= pc else up_left)
                out = value + predictor
            else:
                raise SystemExit(f"{path}: unsupported PNG filter {filt}")
            recon[x] = out & 0xFF
        rows.append(bytes(recon))
        prev = recon
    return width, height, rows


def capture_receipt_lane(
    live: LiveKettle,
    target: Path,
    rect: Dict[str, object],
) -> None:
    """Ask Kettle to persist only the receipt, never the full terminal."""

    try:
        x = math.ceil(float(rect["x"]))
        y = math.ceil(float(rect["y"]))
        right = math.floor(float(rect["x"]) + float(rect["width"]))
        bottom = math.floor(float(rect["y"]) + float(rect["height"]))
        if right <= x or bottom <= y:
            raise ValueError("empty receipt crop")
        crop = {
            "crop_x": x,
            "crop_y": y,
            "crop_width": right - x,
            "crop_height": bottom - y,
            "path": str(target),
        }
    except (KeyError, TypeError, ValueError) as error:
        raise SystemExit(f"live-ui smoke: malformed receipt crop: {rect}") from error
    live.ctl("screenshot", params=crop, timeout=12)


def rgba_difference_count(
    left: Tuple[int, int, List[bytes]],
    right: Tuple[int, int, List[bytes]],
    *,
    rect: Optional[Dict[str, object]] = None,
    outside_rect: bool = False,
) -> int:
    """Count pixels changed between equal RGBA screenshots in/outside `rect`."""
    left_width, left_height, left_rows = left
    right_width, right_height, right_rows = right
    if (left_width, left_height) != (right_width, right_height):
        raise SystemExit(
            "live-ui smoke: screenshots changed dimensions while comparing rendered state: "
            f"{left_width}x{left_height} != {right_width}x{right_height}"
        )

    bounds: Optional[Tuple[int, int, int, int]] = None
    if rect is not None:
        try:
            x0 = max(0, int(float(rect["x"])))
            y0 = max(0, int(float(rect["y"])))
            x1 = min(left_width, int(float(rect["x"]) + float(rect["width"]) + 0.999))
            y1 = min(left_height, int(float(rect["y"]) + float(rect["height"]) + 0.999))
        except (KeyError, TypeError, ValueError) as error:
            raise SystemExit(f"live-ui smoke: malformed comparison rectangle: {rect}") from error
        if x0 >= x1 or y0 >= y1:
            raise SystemExit(f"live-ui smoke: empty comparison rectangle: {rect}")
        bounds = (x0, y0, x1, y1)

    changed = 0
    for y, (left_row, right_row) in enumerate(zip(left_rows, right_rows)):
        for x in range(left_width):
            inside = bounds is None or (
                bounds[0] <= x < bounds[2] and bounds[1] <= y < bounds[3]
            )
            if outside_rect:
                inside = not inside
            if not inside:
                continue
            start = x * 4
            if left_row[start : start + 4] != right_row[start : start + 4]:
                changed += 1
    return changed


def rgba_card_difference_count(
    left: Tuple[int, int, List[bytes]],
    right: Tuple[int, int, List[bytes]],
) -> int:
    """Compare opaque card crops even when their state changes dimensions."""

    left_width, left_height, left_rows = left
    right_width, right_height, right_rows = right
    common_width = min(left_width, right_width)
    common_height = min(left_height, right_height)
    changed = abs(left_width * left_height - right_width * right_height)
    for y in range(common_height):
        left_row = left_rows[y]
        right_row = right_rows[y]
        for x in range(common_width):
            start = x * 4
            if left_row[start : start + 4] != right_row[start : start + 4]:
                changed += 1
    return changed


def parse_hex_rgb(value: str) -> Tuple[int, int, int]:
    if re.fullmatch(r"#[0-9a-fA-F]{6}", value) is None:
        raise ValueError(f"expected #rrggbb color, got {value!r}")
    return tuple(int(value[offset : offset + 2], 16) for offset in (1, 3, 5))  # type: ignore[return-value]


def split_titlebar_number(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise RuntimeError(f"split-titlebar smoke: {label} is not numeric: {value!r}")
    number = float(value)
    if not math.isfinite(number):
        raise RuntimeError(f"split-titlebar smoke: {label} is not finite: {value!r}")
    return number


def split_titlebar_rect(value: object, label: str) -> Tuple[float, float, float, float]:
    if not isinstance(value, dict):
        raise RuntimeError(f"split-titlebar smoke: {label} is not a rectangle: {value!r}")
    rect = tuple(
        split_titlebar_number(value.get(field), f"{label}.{field}")
        for field in ("x", "y", "width", "height")
    )
    if rect[2] <= 0.0 or rect[3] <= 0.0:
        raise RuntimeError(f"split-titlebar smoke: {label} is empty: {value!r}")
    return rect  # type: ignore[return-value]


def exact_rgb_patch(
    rgba_rows: List[bytes],
    width: int,
    height: int,
    x: float,
    y: float,
    expected: Tuple[int, int, int],
    label: str,
) -> Dict[str, object]:
    """Check a 3x3 opaque patch centered on a geometry-derived interior point."""
    center_x = int(math.floor(x))
    center_y = int(math.floor(y))
    pixels: List[Tuple[int, int, int, int]] = []
    for sample_y in range(center_y - 1, center_y + 2):
        for sample_x in range(center_x - 1, center_x + 2):
            if not (0 <= sample_x < width and 0 <= sample_y < height):
                raise RuntimeError(
                    f"split-titlebar smoke: {label} sample escaped the screenshot "
                    f"at ({sample_x}, {sample_y}) in {width}x{height}"
                )
            offset = sample_x * 4
            pixels.append(tuple(rgba_rows[sample_y][offset : offset + 4]))  # type: ignore[arg-type]
    expected_rgba = (*expected, 255)
    mismatches = [pixel for pixel in pixels if pixel != expected_rgba]
    if mismatches:
        observed = sorted(set(pixels))
        raise RuntimeError(
            f"split-titlebar smoke: {label} expected {expected_rgba}, "
            f"observed {observed}"
        )
    return {
        "x": center_x,
        "y": center_y,
        "pixels": len(pixels),
        "rgb": "#{:02x}{:02x}{:02x}".format(*expected),
    }


def analyze_split_titlebar_frame(
    geometry: Dict[str, object],
    width: int,
    height: int,
    rgba_rows: List[bytes],
    *,
    title_at_bottom: bool,
    broadcast: bool,
) -> Dict[str, object]:
    """Validate one real split-titlebar frame from geometry and exact pixels.

    The title label contract begins with two blank cells. Sampling the center of
    the first one avoids shell-controlled text, group/bell icons, and the
    one-pixel focus accent. The neighboring body sample stays in the configured
    vertical padding between the titlebar and the terminal grid.
    """
    surface = geometry.get("surface")
    if not isinstance(surface, dict):
        raise RuntimeError("split-titlebar smoke: ui_geometry omitted surface")
    surface_width = int(split_titlebar_number(surface.get("width"), "surface.width"))
    surface_height = int(
        split_titlebar_number(surface.get("height"), "surface.height")
    )
    if (width, height) != (surface_width, surface_height):
        raise RuntimeError(
            "split-titlebar smoke: screenshot/surface dimensions differ: "
            f"png={width}x{height} geometry={surface_width}x{surface_height}"
        )
    if len(rgba_rows) != height or any(len(row) != width * 4 for row in rgba_rows):
        raise RuntimeError("split-titlebar smoke: malformed RGBA screenshot rows")

    cell = geometry.get("cell")
    padding = geometry.get("padding")
    if not isinstance(cell, dict) or not isinstance(padding, dict):
        raise RuntimeError("split-titlebar smoke: ui_geometry omitted cell/padding")
    cell_width = split_titlebar_number(cell.get("width"), "cell.width")
    cell_height = split_titlebar_number(cell.get("height"), "cell.height")
    padding_y = split_titlebar_number(padding.get("y"), "padding.y")
    if cell_width < 6.0 or cell_height < 6.0 or padding_y < 6.0:
        raise RuntimeError(
            "split-titlebar smoke: cell/padding metrics are too small for the "
            f"non-glyph pixel oracle: cell={cell_width}x{cell_height} "
            f"padding_y={padding_y}"
        )

    titlebars = geometry.get("pane_titlebars")
    if not isinstance(titlebars, list) or len(titlebars) < 2:
        raise RuntimeError(
            f"split-titlebar smoke: expected at least two pane titlebars: {titlebars!r}"
        )
    focused = [
        titlebar
        for titlebar in titlebars
        if isinstance(titlebar, dict) and titlebar.get("focused") is True
    ]
    if len(focused) != 1:
        raise RuntimeError(
            f"split-titlebar smoke: expected one focused titlebar: {titlebars!r}"
        )

    colors = {
        name: parse_hex_rgb(value)
        for name, value in SPLIT_TITLEBAR_COLOR_HEX.items()
    }
    tolerance = 0.75
    pane_analysis: List[Dict[str, object]] = []
    seen_panes: Set[object] = set()
    for index, titlebar in enumerate(titlebars):
        if not isinstance(titlebar, dict):
            raise RuntimeError(
                f"split-titlebar smoke: malformed titlebar[{index}]: {titlebar!r}"
            )
        pane_id = titlebar.get("pane")
        if (
            isinstance(pane_id, bool)
            or not isinstance(pane_id, int)
            or pane_id in seen_panes
        ):
            raise RuntimeError(
                f"split-titlebar smoke: invalid/duplicate titlebar pane id: {pane_id!r}"
            )
        seen_panes.add(pane_id)
        bar_x, bar_y, bar_width, bar_height = split_titlebar_rect(
            titlebar.get("rect"), f"pane_titlebars[{index}].rect"
        )
        pane_x, pane_y, pane_width, pane_height = split_titlebar_rect(
            titlebar.get("pane_rect"), f"pane_titlebars[{index}].pane_rect"
        )
        if (
            abs(bar_x - pane_x) > tolerance
            or abs(bar_width - pane_width) > tolerance
            or abs(bar_height - (cell_height + 6.0)) > tolerance
        ):
            raise RuntimeError(
                "split-titlebar smoke: titlebar span/height drifted from pane/cell "
                f"geometry: titlebar={titlebar!r} cell_height={cell_height}"
            )
        expected_bar_y = (
            pane_y + pane_height - bar_height if title_at_bottom else pane_y
        )
        if abs(bar_y - expected_bar_y) > tolerance:
            edge = "bottom" if title_at_bottom else "top"
            raise RuntimeError(
                f"split-titlebar smoke: {edge} titlebar is on the wrong pane edge: "
                f"titlebar={titlebar!r}"
            )

        columns_value = titlebar.get("cols")
        rows_value = titlebar.get("rows")
        if (
            isinstance(columns_value, bool)
            or not isinstance(columns_value, int)
            or columns_value < 1
            or isinstance(rows_value, bool)
            or not isinstance(rows_value, int)
            or rows_value < 1
        ):
            raise RuntimeError(
                f"split-titlebar smoke: invalid pane grid dimensions: {titlebar!r}"
            )
        columns = columns_value
        rows = rows_value
        grid_origin_y = pane_y + padding_y + (
            0.0 if title_at_bottom else bar_height
        )
        grid_end_y = grid_origin_y + rows * cell_height
        grid_limit_y = (
            bar_y - padding_y
            if title_at_bottom
            else pane_y + pane_height - padding_y
        )
        remainder = grid_limit_y - grid_end_y
        if remainder < -tolerance or remainder >= cell_height + tolerance:
            raise RuntimeError(
                "split-titlebar smoke: PTY grid does not fit the title-position-aware "
                f"body: pane={pane_id!r} origin={grid_origin_y} end={grid_end_y} "
                f"limit={grid_limit_y} rows={rows}"
            )

        fitted_title = titlebar.get("fitted_title")
        if not isinstance(fitted_title, str) or not fitted_title.startswith("  "):
            raise RuntimeError(
                "split-titlebar smoke: title label lost its two-cell sampling gutter: "
                f"{titlebar!r}"
            )
        sample_x = bar_x + cell_width * 0.5
        sample_y = bar_y + bar_height * 0.5
        state = (
            "transmit"
            if titlebar.get("focused") is True
            else ("receive" if broadcast else "inactive")
        )
        titlebar_sample = exact_rgb_patch(
            rgba_rows,
            width,
            height,
            sample_x,
            sample_y,
            colors[state],
            f"pane {pane_id} {state} titlebar",
        )

        grid_side_y = (
            bar_y - padding_y * 0.5
            if title_at_bottom
            else bar_y + bar_height + padding_y * 0.5
        )
        grid_side_sample = exact_rgb_patch(
            rgba_rows,
            width,
            height,
            sample_x,
            grid_side_y,
            colors["grid"],
            f"pane {pane_id} grid-side edge",
        )
        pane_analysis.append(
            {
                "pane": pane_id,
                "focused": titlebar.get("focused") is True,
                "state": state,
                "titlebar_rect": titlebar.get("rect"),
                "pane_rect": titlebar.get("pane_rect"),
                "grid": {
                    "cols": columns,
                    "rows": rows,
                    "origin_y": grid_origin_y,
                    "end_y": grid_end_y,
                    "body_limit_y": grid_limit_y,
                },
                "titlebar_sample": titlebar_sample,
                "grid_side_sample": grid_side_sample,
            }
        )

    return {
        "title_at_bottom": title_at_bottom,
        "broadcast": broadcast,
        "surface": {"width": width, "height": height},
        "colors": SPLIT_TITLEBAR_COLOR_HEX,
        "panes": pane_analysis,
    }


def analyze_split_titlebar_png(
    geometry: Dict[str, object],
    screenshot: Path,
    *,
    title_at_bottom: bool,
    broadcast: bool,
) -> Dict[str, object]:
    width, height, rgba_rows = read_rgba_png(screenshot)
    return analyze_split_titlebar_frame(
        geometry,
        width,
        height,
        rgba_rows,
        title_at_bottom=title_at_bottom,
        broadcast=broadcast,
    )


def bright_at(rgba_rows: List[bytes], x: int, y: int) -> bool:
    if y < 0 or y >= len(rgba_rows) or x < 0:
        return False
    row = rgba_rows[y]
    if x * 4 + 3 >= len(row):
        return False
    off = x * 4
    r, g, b, a = row[off : off + 4]
    return a > 0 and (r * 299 + g * 587 + b * 114) >= 140_000


def bright_pixel_count(rgba_rows: List[bytes]) -> int:
    total = 0
    for row in rgba_rows:
        for off in range(0, len(row), 4):
            r, g, b, a = row[off : off + 4]
            if a > 0 and (r * 299 + g * 587 + b * 114) >= 140_000:
                total += 1
    return total


def magenta_block_metrics(rgba_rows: List[bytes]) -> Tuple[int, int]:
    """Return the widest magenta run and longest stack of qualifying rows."""
    widest = 0
    stacked = 0
    longest_stack = 0
    for row in rgba_rows:
        current = 0
        row_widest = 0
        for off in range(0, len(row), 4):
            r, g, b, a = row[off : off + 4]
            if a > 0 and r >= 200 and g <= 80 and b >= 200:
                current += 1
                row_widest = max(row_widest, current)
            else:
                current = 0
        widest = max(widest, row_widest)
        if row_widest >= 12:
            stacked += 1
            longest_stack = max(longest_stack, stacked)
        else:
            stacked = 0
    return widest, longest_stack


def added_magenta_pixel_count(
    before_rows: List[bytes], after_rows: List[bytes]
) -> int:
    """Count newly magenta pixels at the same coordinates in two screenshots."""
    if len(before_rows) != len(after_rows):
        raise ValueError("screenshot heights differ")
    added = 0
    for before, after in zip(before_rows, after_rows, strict=True):
        if len(before) != len(after):
            raise ValueError("screenshot widths differ")
        for off in range(0, len(after), 4):
            br, bg, bb, ba = before[off : off + 4]
            ar, ag, ab, aa = after[off : off + 4]
            before_magenta = (
                ba > 0 and br >= 200 and bg <= 80 and bb >= 200
            )
            after_magenta = (
                aa > 0 and ar >= 200 and ag <= 80 and ab >= 200
            )
            if after_magenta and not before_magenta:
                added += 1
    return added


def bright_pixels_in_rect(rgba_rows: List[bytes], x0: float, y0: float, x1: float, y1: float) -> int:
    total = 0
    y_start = max(0, int(y0))
    y_end = min(len(rgba_rows), int(y1))
    for y in range(y_start, y_end):
        row = rgba_rows[y]
        x_start = max(0, int(x0))
        x_end = min(len(row) // 4, int(x1))
        for x in range(x_start, x_end):
            off = x * 4
            r, g, b, a = row[off : off + 4]
            if a > 0 and (r * 299 + g * 587 + b * 114) >= 140_000:
                total += 1
    return total


def shell_quote(text: str, *, windows: Optional[bool] = None) -> str:
    use_windows = platform.system() == "Windows" if windows is None else windows
    if use_windows:
        return "'" + text.replace("'", "''") + "'"
    return "'" + text.replace("'", "'\"'\"'") + "'"


@dataclass(frozen=True)
class AgentShellTarget:
    """The shell exercised by the agent/TUI live-window smoke.

    `wsl` is deliberately a Windows-host mode: the Windows Kettle binary still
    owns the window and ConPTY, while `wsl.exe` owns the Linux shell and its
    tools. Keeping this as an explicit target avoids accidentally building
    PowerShell commands merely because the Python helper itself runs on
    Windows.
    """

    mode: str = "native"
    wsl_distro: Optional[str] = None
    astro_config: Optional[str] = None
    nvim_data: Optional[str] = None

    @property
    def powershell(self) -> bool:
        return self.mode == "native" and platform.system() == "Windows"

    @property
    def label(self) -> str:
        if self.mode == "wsl" and self.wsl_distro:
            safe_distro = re.sub(r"[^A-Za-z0-9_.-]+", "-", self.wsl_distro).strip("-")
            return f"wsl-{safe_distro or 'distro'}"
        return self.mode

    def wsl_base_argv(self) -> List[str]:
        argv = ["wsl.exe"]
        if self.wsl_distro:
            argv += ["--distribution", self.wsl_distro]
        return argv + ["--cd", "~"]

    def launch_args(self) -> List[str]:
        if self.mode == "wsl":
            return [
                "-e",
                *self.wsl_base_argv(),
                "--exec",
                "bash",
                "--noprofile",
                "--norc",
            ]
        if self.powershell:
            return ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
        # The commands generated by this harness use Bash/POSIX syntax. Do not
        # inherit an arbitrary login shell such as fish or Nushell and then
        # feed it `export`, shell functions, and POSIX loops.
        return ["-e", "bash", "--noprofile", "--norc"]

    def target_join(self, base: str, *parts: str) -> str:
        """Join paths using the target shell, not the Python host, syntax."""
        if self.powershell:
            return str(Path(base).joinpath(*parts))
        return posixpath.join(base, *parts)

    def host_argv(self, argv: List[str]) -> List[str]:
        if self.mode == "wsl":
            return [*self.wsl_base_argv(), "--exec", *argv]
        return argv

    @staticmethod
    def is_wsl_host_tool_path(path: str) -> bool:
        normalized = path.replace("\\", "/")
        return re.match(r"^/mnt/[A-Za-z](?:/|$)", normalized) is not None

    def posix_path_setup(self, *, keep_windows_host_paths: bool = False) -> str:
        # Deterministic non-rc shell, while retaining the usual user-local
        # install locations for rustup, standalone Claude, and npm CLIs. WSL
        # appends the Windows PATH by default; remove those mount entries so a
        # Windows .exe/shim cannot masquerade as a tool installed in the
        # selected Linux distribution.
        prefix = (
            'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:'
            '$HOME/.npm-global/bin:$PATH"; unset HISTFILE'
        )
        if self.mode != "wsl" or keep_windows_host_paths:
            return prefix
        return (
            'KETTLE_SMOKE_LINUX_PATH=""; KETTLE_SMOKE_PATH_REST=$PATH; '
            "while :; do "
            'case "$KETTLE_SMOKE_PATH_REST" in '
            '*:*) KETTLE_SMOKE_PATH_ENTRY=${KETTLE_SMOKE_PATH_REST%%:*}; '
            'KETTLE_SMOKE_PATH_REST=${KETTLE_SMOKE_PATH_REST#*:} ;; '
            '*) KETTLE_SMOKE_PATH_ENTRY=$KETTLE_SMOKE_PATH_REST; '
            'KETTLE_SMOKE_PATH_REST= ;; esac; '
            'case "$KETTLE_SMOKE_PATH_ENTRY" in '
            "/mnt/[A-Za-z]|/mnt/[A-Za-z]/*|'') ;; "
            '*) KETTLE_SMOKE_LINUX_PATH="${KETTLE_SMOKE_LINUX_PATH}'
            '${KETTLE_SMOKE_LINUX_PATH:+:}$KETTLE_SMOKE_PATH_ENTRY" ;; esac; '
            '[ -n "$KETTLE_SMOKE_PATH_REST" ] || break; done; '
            'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:'
            '$HOME/.npm-global/bin${KETTLE_SMOKE_LINUX_PATH:+:'
            '$KETTLE_SMOKE_LINUX_PATH}"; '
            "unset KETTLE_SMOKE_LINUX_PATH KETTLE_SMOKE_PATH_REST "
            "KETTLE_SMOKE_PATH_ENTRY HISTFILE"
        )

    def initial_shell_command(self) -> Optional[str]:
        if self.powershell:
            return None
        return self.posix_path_setup()

    def posix_script_argv(self, script: str) -> List[str]:
        """Run a script in the same deterministic Bash dialect as the pane."""
        if self.powershell:
            raise ValueError("POSIX scripts are not valid for PowerShell targets")
        return self.host_argv(
            [
                "bash",
                "--noprofile",
                "--norc",
                "-c",
                f"{self.posix_path_setup()}; {script}",
            ]
        )

    def command_argv(self, argv: List[str]) -> List[str]:
        """Build a host command with the target shell's effective PATH."""
        if self.powershell:
            return argv
        return self.posix_script_argv(f"exec {shlex.join(argv)}")

    def _posix_command_path_script(
        self, command: str, *, keep_windows_host_paths: bool
    ) -> str:
        script = (
            f"{self.posix_path_setup(keep_windows_host_paths=keep_windows_host_paths)}; "
            f"KETTLE_SMOKE_COMMAND=$(command -v -- {shlex.quote(command)}) "
            "|| exit 127; "
            'case "$KETTLE_SMOKE_COMMAND" in /*) ;; *) exit 126 ;; esac; '
        )
        if self.mode == "wsl":
            # WSL's target is a separate filesystem namespace, so resolve
            # there. GNU readlink is part of the Linux environment and lets us
            # reject a target-side shim that ultimately points into /mnt/c.
            script += (
                'KETTLE_SMOKE_COMMAND=$(readlink -f -- "$KETTLE_SMOKE_COMMAND") '
                "|| exit 125; "
            )
        script += "printf '%s\\n' \"$KETTLE_SMOKE_COMMAND\""
        return script

    def _posix_command_path(
        self, command: str, *, keep_windows_host_paths: bool
    ) -> Optional[str]:
        script = self._posix_command_path_script(
            command, keep_windows_host_paths=keep_windows_host_paths
        )
        cp = run(
            self.host_argv(
                ["bash", "--noprofile", "--norc", "-c", script]
            ),
            timeout=60 if self.mode == "wsl" else 30,
        )
        paths = cp.stdout.splitlines() if cp.returncode == 0 else []
        if len(paths) != 1:
            return None
        if self.mode == "wsl":
            return paths[0]
        # Native Unix/macOS shares the helper's filesystem. Resolve through
        # Python rather than assuming GNU `readlink -f`, which is unavailable
        # in the default macOS userland.
        try:
            return str(Path(paths[0]).resolve(strict=True))
        except OSError:
            return None

    def command_resolution(self, command: str) -> Tuple[str, Optional[str]]:
        """Classify a tool as target-native, Windows-host, or missing."""
        if self.powershell:
            path = shutil.which(command)
            return ("available", path) if path else ("missing", None)

        path = self._posix_command_path(
            command, keep_windows_host_paths=False
        )
        if path is not None:
            if self.mode == "wsl" and self.is_wsl_host_tool_path(path):
                return "windows-host", path
            return "available", path

        if self.mode == "wsl":
            host_path = self._posix_command_path(
                command, keep_windows_host_paths=True
            )
            if host_path is not None and self.is_wsl_host_tool_path(host_path):
                return "windows-host", host_path
        return "missing", None

    def command_available(self, command: str) -> bool:
        status, _path = self.command_resolution(command)
        return status == "available"

    def command_unavailable_reason(self, command: str) -> str:
        status, path = self.command_resolution(command)
        if status == "windows-host":
            return (
                f"resolved to Windows-host tool {path}; install {command} "
                f"inside the {self.label} target"
            )
        if status == "available":
            return f"{command} is available"
        return f"not on {self.label} PATH"

    def require_command_path(self, command: str) -> str:
        status, path = self.command_resolution(command)
        if status != "available" or path is None:
            raise RuntimeError(
                f"required target command {command!r} unavailable: "
                f"{self.command_unavailable_reason(command)}"
            )
        return path

    def run_command(
        self, argv: List[str], *, timeout: float = 10
    ) -> subprocess.CompletedProcess:
        return run(self.command_argv(argv), timeout=timeout, capture=True)

    def nvim_config_source(self) -> str:
        if self.astro_config:
            return self.astro_config
        if self.mode == "wsl":
            return "${XDG_CONFIG_HOME:-$HOME/.config}/nvim"
        if self.powershell:
            config_home = os.environ.get("XDG_CONFIG_HOME")
            if config_home:
                return str(Path(config_home) / "nvim")
            return str(Path(os.environ.get("LOCALAPPDATA", tempfile.gettempdir())) / "nvim")
        return str(
            Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
            / "nvim"
        )

    def configured_nvim_available(self) -> bool:
        source = self.nvim_config_source()
        if self.mode == "wsl":
            source_expr = (
                self.posix_path_expression(source) if self.astro_config else source
            )
            cp = run(
                self.posix_script_argv(f"test -d {source_expr}"),
                timeout=60,
            )
            return cp.returncode == 0
        return Path(source).expanduser().is_dir()

    def nvim_data_source(self) -> str:
        if self.nvim_data:
            return self.nvim_data
        if self.mode == "wsl":
            return "${XDG_DATA_HOME:-$HOME/.local/share}/nvim"
        if self.powershell:
            return str(
                Path(os.environ.get("LOCALAPPDATA", tempfile.gettempdir()))
                / "nvim-data"
            )
        return str(
            Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share"))
            / "nvim"
        )

    @staticmethod
    def posix_path_expression(path: str) -> str:
        """Quote an explicit target-shell path while allowing a leading `~/`."""
        if path == "~":
            return '"$HOME"'
        if path.startswith("~/"):
            return f'"$HOME"/{shlex.quote(path[2:])}'
        return shlex.quote(path)

    @staticmethod
    def path_is_link(path: Path) -> bool:
        """Recognize links and Windows junctions without following them."""
        return path_is_link(path)

    @classmethod
    def validate_native_sandbox_path(cls, sandbox_path: str) -> Path:
        candidate = Path(sandbox_path)
        if cls.path_is_link(candidate):
            raise ValueError(
                f"refusing linked Neovim sandbox path: {candidate}"
            )
        root = candidate.resolve()
        expected_parent = (
            windows_live_smoke_parent()
            if platform.system() == "Windows"
            else Path(tempfile.gettempdir()).resolve()
        )
        if (
            root.parent != expected_parent
            or not root.name.startswith("kettle-agent-tui-")
        ):
            raise ValueError(f"refusing unsafe Neovim sandbox path: {root}")
        return root

    def create_nvim_sandbox_host(self) -> str:
        """Create an unpredictable owner-private sandbox on the target host."""
        if self.mode == "wsl":
            self.require_wsl_pidfd_cleanup()
            cp = run(
                self.host_argv(
                    [
                        "bash",
                        "--noprofile",
                        "--norc",
                        "-c",
                        (
                            "umask 077; "
                            "root=$(mktemp -d "
                            "/tmp/kettle-agent-tui-XXXXXXXXXX) || exit 1; "
                            'chmod 700 -- "$root" || { '
                            'rm -rf -- "$root"; exit 1; }; '
                            "printf '%s\\n' \"$root\""
                        ),
                    ]
                ),
                timeout=60,
            )
            paths = cp.stdout.splitlines() if cp.returncode == 0 else []
            if len(paths) != 1:
                raise RuntimeError(
                    "failed to create WSL Neovim sandbox: "
                    f"stdout={cp.stdout!r} stderr={cp.stderr!r}"
                )
            self.validate_wsl_sandbox_path(paths[0])
            return paths[0]

        root = (
            create_windows_private_directory("kettle-agent-tui-")
            if platform.system() == "Windows"
            else Path(tempfile.mkdtemp(prefix="kettle-agent-tui-"))
        )
        try:
            root.chmod(0o700)
            return str(self.validate_native_sandbox_path(str(root)))
        except Exception:
            shutil.rmtree(root, ignore_errors=True)
            raise

    @classmethod
    def assert_snapshot_has_no_links(cls, root: Path) -> None:
        """Reject a copied tree that still points outside the snapshot."""
        for current, directories, files in os.walk(root, followlinks=False):
            current_path = Path(current)
            for name in [*directories, *files]:
                candidate = current_path / name
                if cls.path_is_link(candidate):
                    raise RuntimeError(
                        "Neovim snapshot retained a symlink or junction: "
                        f"{candidate}"
                    )

    @classmethod
    def _copy_bounded_regular_file(
        cls, source: Path, target: Path, budget: SnapshotCopyBudget
    ) -> None:
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
        flags |= getattr(os, "O_NONBLOCK", 0)
        descriptor = os.open(source, flags)
        try:
            source_stat = os.fstat(descriptor)
            if not stat.S_ISREG(source_stat.st_mode):
                raise RuntimeError(
                    f"Neovim snapshot source is not a regular file: {source}"
                )
            size = int(source_stat.st_size)
            budget.add_file(source, size)
            with os.fdopen(descriptor, "rb", closefd=False) as source_file:
                with target.open("xb") as target_file:
                    remaining = size
                    while remaining > 0:
                        chunk = source_file.read(min(COPY_CHUNK_BYTES, remaining))
                        if not chunk:
                            raise RuntimeError(
                                "Neovim snapshot source shrank while being copied: "
                                f"{source}"
                            )
                        target_file.write(chunk)
                        remaining -= len(chunk)
                    if source_file.read(1):
                        raise RuntimeError(
                            "Neovim snapshot source grew while being copied: "
                            f"{source}"
                        )
            executable = bool(
                source_stat.st_mode
                & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
            )
            target.chmod(0o700 if executable else 0o600)
        finally:
            os.close(descriptor)

    @classmethod
    def _copy_bounded_regular_directory(
        cls,
        source: Path,
        target: Path,
        snapshot_root: Path,
        budget: SnapshotCopyBudget,
        active_directories: Set[Tuple[int, int, str]],
        depth: int,
    ) -> None:
        if depth > budget.max_depth:
            raise RuntimeError(
                "Neovim snapshot exceeds the "
                f"{budget.max_depth} directory depth limit at {source}"
            )
        source_stat = source.stat()
        if not stat.S_ISDIR(source_stat.st_mode):
            raise RuntimeError(
                f"Neovim snapshot source is not a directory: {source}"
            )
        resolved = source.resolve(strict=True)
        try:
            snapshot_root.relative_to(resolved)
        except ValueError:
            pass
        else:
            raise RuntimeError(
                "Neovim snapshot source contains its destination: "
                f"{source} -> {resolved}"
            )
        identity = (
            int(source_stat.st_dev),
            int(source_stat.st_ino),
            os.path.normcase(str(resolved)),
        )
        if identity in active_directories:
            raise RuntimeError(
                f"Neovim snapshot source contains a directory cycle: {source}"
            )
        active_directories.add(identity)
        try:
            target.mkdir(mode=0o700)
            with os.scandir(source) as entries:
                for entry in entries:
                    source_entry = Path(entry.path)
                    # Preserve the repository marker and refs lazy.nvim uses
                    # to recognize an installed, pinned plugin, but omit the
                    # object database: it is not Neovim runtime and a single
                    # pack can dwarf the complete checked-out plugin.
                    if (
                        entry.name == "objects"
                        and source_entry.parent.name == ".git"
                    ):
                        continue
                    target_entry = target / entry.name
                    budget.add_entry(source_entry)
                    try:
                        entry_stat = entry.stat(follow_symlinks=True)
                    except OSError as error:
                        raise RuntimeError(
                            "cannot resolve Neovim snapshot entry "
                            f"{source_entry}: {error}"
                        ) from error
                    if stat.S_ISDIR(entry_stat.st_mode):
                        cls._copy_bounded_regular_directory(
                            source_entry,
                            target_entry,
                            snapshot_root,
                            budget,
                            active_directories,
                            depth + 1,
                        )
                    elif stat.S_ISREG(entry_stat.st_mode):
                        cls._copy_bounded_regular_file(
                            source_entry, target_entry, budget
                        )
                    else:
                        raise RuntimeError(
                            "Neovim snapshot only accepts regular files and "
                            f"directories: {source_entry}"
                        )
        finally:
            active_directories.remove(identity)

    @classmethod
    def copy_bounded_regular_tree(
        cls,
        source: Path,
        target: Path,
        budget: SnapshotCopyBudget,
    ) -> None:
        """Dereference a tree without unbounded recursion or special files."""
        if target.exists() or cls.path_is_link(target):
            raise RuntimeError(
                f"Neovim snapshot destination already exists: {target}"
            )
        budget.add_entry(source)
        cls._copy_bounded_regular_directory(
            source,
            target,
            target.resolve(),
            budget,
            set(),
            0,
        )

    @staticmethod
    def wsl_bounded_copy_function() -> str:
        """Bash helper matching the native bounded regular-tree copy."""
        return (
            "kettle_smoke_copy_tree() { "
            "local KETTLE_COPY_SOURCE KETTLE_COPY_TARGET "
            "KETTLE_COPY_SOURCE_REAL KETTLE_COPY_TARGET_REAL "
            "KETTLE_COPY_FIFO KETTLE_COPY_MANIFEST KETTLE_COPY_BAD "
            "KETTLE_COPY_SIZES KETTLE_COPY_FIND_PID KETTLE_COPY_STATUS "
            "KETTLE_COPY_TYPE KETTLE_COPY_PATH KETTLE_COPY_REL KETTLE_COPY_DEPTH "
            "KETTLE_COPY_TAIL KETTLE_COPY_SIZE KETTLE_COPY_ACTUAL "
            "KETTLE_COPY_REMAINING KETTLE_COPY_STREAM_LIMIT; "
            "local -a KETTLE_COPY_PIPE_STATUS; "
            "KETTLE_COPY_SOURCE=$1; KETTLE_COPY_TARGET=$2; "
            "KETTLE_COPY_ENTRIES=$((KETTLE_COPY_ENTRIES + 1)); "
            f"if [ \"$KETTLE_COPY_ENTRIES\" -gt {NVIM_SNAPSHOT_MAX_ENTRIES} ]; then "
            'printf "snapshot exceeds entry limit at %s\\n" '
            '"$KETTLE_COPY_SOURCE" >&2; return 1; fi; '
            '[ -d "$KETTLE_COPY_SOURCE" ] || { '
            'printf "snapshot source is not a directory: %s\\n" '
            '"$KETTLE_COPY_SOURCE" >&2; return 1; }; '
            '[ ! -e "$KETTLE_COPY_TARGET" ] && '
            '[ ! -L "$KETTLE_COPY_TARGET" ] || { '
            'printf "snapshot destination exists: %s\\n" '
            '"$KETTLE_COPY_TARGET" >&2; return 1; }; '
            'KETTLE_COPY_SOURCE_REAL=$(readlink -f -- "$KETTLE_COPY_SOURCE") '
            "|| return 1; "
            'KETTLE_COPY_TARGET_REAL=$(readlink -f -- "$KETTLE_COPY_TARGET") '
            "|| return 1; "
            'case "$KETTLE_COPY_TARGET_REAL/" in '
            '"$KETTLE_COPY_SOURCE_REAL/"*) '
            'printf "snapshot source contains destination: %s\\n" '
            '"$KETTLE_COPY_SOURCE" >&2; return 1 ;; esac; '
            "KETTLE_COPY_SOURCE=$KETTLE_COPY_SOURCE_REAL; "
            'KETTLE_COPY_FIFO="$KETTLE_SMOKE_ROOT/run/find-$RANDOM-$$"; '
            'KETTLE_COPY_MANIFEST="$KETTLE_SMOKE_ROOT/run/manifest-$RANDOM-$$"; '
            'KETTLE_COPY_BAD="$KETTLE_SMOKE_ROOT/run/bad-$RANDOM-$$"; '
            'KETTLE_COPY_SIZES="$KETTLE_SMOKE_ROOT/run/sizes-$RANDOM-$$"; '
            ': >"$KETTLE_COPY_MANIFEST" || return 1; '
            'chmod 600 -- "$KETTLE_COPY_MANIFEST" || return 1; '
            'mkfifo -m 600 -- "$KETTLE_COPY_FIFO" || { '
            'rm -f -- "$KETTLE_COPY_MANIFEST"; return 1; }; '
            'find -L "$KETTLE_COPY_SOURCE" -mindepth 1 '
            "-path '*/.git/objects' -prune -o -printf '%y\\0%s\\0%p\\0' "
            '>"$KETTLE_COPY_FIFO" & KETTLE_COPY_FIND_PID=$!; '
            "KETTLE_COPY_STATUS=0; "
            "while IFS= read -r -d '' KETTLE_COPY_TYPE && "
            "IFS= read -r -d '' KETTLE_COPY_SIZE && "
            "IFS= read -r -d '' KETTLE_COPY_PATH; do "
            "KETTLE_COPY_ENTRIES=$((KETTLE_COPY_ENTRIES + 1)); "
            f"if [ \"$KETTLE_COPY_ENTRIES\" -gt {NVIM_SNAPSHOT_MAX_ENTRIES} ]; then "
            'printf "snapshot exceeds entry limit at %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break; fi; '
            'case "$KETTLE_COPY_PATH" in "$KETTLE_COPY_SOURCE"/*) '
            'KETTLE_COPY_REL=${KETTLE_COPY_PATH#"$KETTLE_COPY_SOURCE"/} ;; '
            '*) printf "snapshot traversal escaped source: %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break ;; esac; '
            "KETTLE_COPY_DEPTH=1; KETTLE_COPY_TAIL=$KETTLE_COPY_REL; "
            'while [ "${KETTLE_COPY_TAIL#*/}" != "$KETTLE_COPY_TAIL" ]; do '
            "KETTLE_COPY_DEPTH=$((KETTLE_COPY_DEPTH + 1)); "
            'KETTLE_COPY_TAIL=${KETTLE_COPY_TAIL#*/}; done; '
            f"if [ \"$KETTLE_COPY_DEPTH\" -gt {NVIM_SNAPSHOT_MAX_DEPTH} ]; then "
            'printf "snapshot exceeds depth limit at %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break; fi; '
            'if [ "$KETTLE_COPY_TYPE" = d ]; then :; '
            'elif [ "$KETTLE_COPY_TYPE" = f ]; then '
            'case "$KETTLE_COPY_SIZE" in ""|*[!0-9]*) '
            'printf "invalid snapshot file size: %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break ;; esac; '
            f"if [ \"$KETTLE_COPY_SIZE\" -gt {NVIM_SNAPSHOT_MAX_FILE_BYTES} ]; then "
            'printf "snapshot file exceeds per-file limit: %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break; fi; '
            "KETTLE_COPY_BYTES=$((KETTLE_COPY_BYTES + KETTLE_COPY_SIZE)); "
            f"if [ \"$KETTLE_COPY_BYTES\" -gt {NVIM_SNAPSHOT_MAX_BYTES} ]; then "
            'printf "snapshot exceeds aggregate byte limit at %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break; fi; '
            "else "
            'printf "snapshot rejects non-regular entry: %s\\n" '
            '"$KETTLE_COPY_PATH" >&2; KETTLE_COPY_STATUS=1; break; fi; '
            "printf '%s\\0' \"$KETTLE_COPY_REL\" "
            '>>"$KETTLE_COPY_MANIFEST" || { '
            "KETTLE_COPY_STATUS=1; break; }; "
            'done <"$KETTLE_COPY_FIFO"; '
            'if [ "$KETTLE_COPY_STATUS" -ne 0 ]; then '
            'kill "$KETTLE_COPY_FIND_PID" 2>/dev/null || true; '
            'wait "$KETTLE_COPY_FIND_PID" 2>/dev/null || true; '
            "else wait \"$KETTLE_COPY_FIND_PID\" || KETTLE_COPY_STATUS=1; fi; "
            'rm -f -- "$KETTLE_COPY_FIFO"; '
            'if [ "$KETTLE_COPY_STATUS" -ne 0 ]; then '
            'rm -f -- "$KETTLE_COPY_MANIFEST"; return 1; fi; '
            'mkdir -m 700 -- "$KETTLE_COPY_TARGET" || { '
            'rm -f -- "$KETTLE_COPY_MANIFEST"; return 1; }; '
            f"KETTLE_COPY_REMAINING=$(({NVIM_SNAPSHOT_MAX_BYTES} - "
            "KETTLE_COPY_ACTUAL_BYTES)); "
            f"KETTLE_COPY_STREAM_LIMIT=$((KETTLE_COPY_REMAINING + "
            f"{NVIM_SNAPSHOT_TAR_OVERHEAD_BYTES})); "
            '(cd "$KETTLE_COPY_SOURCE" && '
            "timeout 300 tar --null --verbatim-files-from --no-recursion "
            '--dereference -cf - -T "$KETTLE_COPY_MANIFEST") '
            '| head -c "$KETTLE_COPY_STREAM_LIMIT" '
            f'| (ulimit -f {NVIM_SNAPSHOT_MAX_FILE_BYTES // 1024} && '
            'tar --no-same-owner --no-same-permissions -xf - '
            '-C "$KETTLE_COPY_TARGET"); '
            'KETTLE_COPY_PIPE_STATUS=("${PIPESTATUS[@]}"); '
            'if [ "${KETTLE_COPY_PIPE_STATUS[0]}" -ne 0 ] || '
            '[ "${KETTLE_COPY_PIPE_STATUS[1]}" -ne 0 ] || '
            '[ "${KETTLE_COPY_PIPE_STATUS[2]}" -ne 0 ]; then '
            'printf "bounded snapshot archive copy failed: %s\\n" '
            '"${KETTLE_COPY_PIPE_STATUS[*]}" >&2; '
            'rm -f -- "$KETTLE_COPY_MANIFEST"; return 1; fi; '
            'find "$KETTLE_COPY_TARGET" ! -type d ! -type f '
            '-print -quit >"$KETTLE_COPY_BAD" || return 1; '
            'if [ -s "$KETTLE_COPY_BAD" ]; then '
            'printf "snapshot archive produced a non-regular entry\\n" >&2; '
            'rm -f -- "$KETTLE_COPY_MANIFEST" "$KETTLE_COPY_BAD"; '
            "return 1; fi; "
            f'find "$KETTLE_COPY_TARGET" -type f -size +{NVIM_SNAPSHOT_MAX_FILE_BYTES}c '
            '-print -quit >"$KETTLE_COPY_BAD" || return 1; '
            'if [ -s "$KETTLE_COPY_BAD" ]; then '
            'printf "snapshot archive exceeded the per-file limit\\n" >&2; '
            'rm -f -- "$KETTLE_COPY_MANIFEST" "$KETTLE_COPY_BAD"; '
            "return 1; fi; "
            'find "$KETTLE_COPY_TARGET" -type f -printf "%s\\n" '
            '>"$KETTLE_COPY_SIZES" || return 1; '
            'KETTLE_COPY_ACTUAL=$(awk \'{ total += $1 } '
            'END { printf "%.0f\\n", total }\' "$KETTLE_COPY_SIZES") '
            "|| return 1; "
            "KETTLE_COPY_ACTUAL_BYTES=$((KETTLE_COPY_ACTUAL_BYTES + "
            "KETTLE_COPY_ACTUAL)); "
            f'if [ "$KETTLE_COPY_ACTUAL_BYTES" -gt {NVIM_SNAPSHOT_MAX_BYTES} ]; then '
            'printf "snapshot archive exceeded the aggregate byte limit\\n" >&2; '
            'rm -f -- "$KETTLE_COPY_MANIFEST" "$KETTLE_COPY_BAD" '
            '"$KETTLE_COPY_SIZES"; return 1; fi; '
            'chmod -R u+rwX,go-rwx -- "$KETTLE_COPY_TARGET" || return 1; '
            'rm -f -- "$KETTLE_COPY_MANIFEST" "$KETTLE_COPY_BAD" '
            '"$KETTLE_COPY_SIZES"; '
            'return "$KETTLE_COPY_STATUS"; }; '
        )

    def prepare_nvim_sandbox_host(self, sandbox_path: str) -> None:
        """Populate a native sandbox without preserving links to live state."""
        if self.mode == "wsl":
            return
        root = self.validate_native_sandbox_path(sandbox_path)
        if not root.is_dir() or self.path_is_link(root):
            raise ValueError(f"Neovim sandbox is not a plain directory: {root}")

        for name in ("home", "config", "data", "state", "cache", "run"):
            (root / name).mkdir(mode=0o700)

        budget = SnapshotCopyBudget()
        config_source = Path(self.nvim_config_source()).expanduser()
        if config_source.is_dir():
            self.copy_bounded_regular_tree(
                config_source,
                root / "config" / "nvim",
                budget,
            )

        data_source = Path(self.nvim_data_source()).expanduser()
        data_target = root / "data" / "nvim"
        data_target.mkdir(mode=0o700)
        for name in ("lazy", "site"):
            source = data_source / name
            if source.is_dir():
                self.copy_bounded_regular_tree(
                    source,
                    data_target / name,
                    budget,
                )

        self.assert_snapshot_has_no_links(root)

    def nvim_snapshot_identity(self, sandbox_path: str) -> Dict[str, object]:
        """Identify the LazyVCS files the configured Neovim probe will load."""
        if self.mode != "wsl":
            root = self.validate_native_sandbox_path(sandbox_path)
            return regular_tree_identity(
                root / "data" / "nvim" / "lazy" / "lazyvcs.nvim"
            )
        self.validate_wsl_sandbox_path(sandbox_path)
        plugin = posixpath.join(
            sandbox_path, "data", "nvim", "lazy", "lazyvcs.nvim"
        )
        result = self.run_command(
            ["python3", "-c", self.bounded_identity_code(), "tree", plugin],
            timeout=120,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"could not identify WSL LazyVCS snapshot: {result.stderr}"
            )
        return json.loads(result.stdout)

    def nvim_file_identity(self, path: str) -> Dict[str, object]:
        """Identify the exact target-side Neovim executable bytes."""
        if self.mode != "wsl":
            return regular_file_identity(Path(path))
        result = self.run_command(
            ["python3", "-c", self.bounded_identity_code(), "file", path],
            timeout=120,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"could not identify WSL Neovim executable: {result.stderr}"
            )
        return json.loads(result.stdout)

    def lazyvcs_loaded_source_identity(
        self, sandbox_path: str
    ) -> Dict[str, object]:
        """Prove LazyVCS loaded a module from the copied plugin snapshot."""
        if self.mode != "wsl":
            root = self.validate_native_sandbox_path(sandbox_path)
            plugin_root = (
                root / "data" / "nvim" / "lazy" / "lazyvcs.nvim"
            ).resolve(strict=True)
            record = root / "run" / "lazyvcs-loaded-source"
            lines = read_bounded_regular_text(record).splitlines()
            if len(lines) != 1 or not lines[0]:
                raise RuntimeError(f"invalid LazyVCS module record: {record}")
            source = Path(lines[0]).resolve(strict=True)
            try:
                relative = source.relative_to(plugin_root)
            except ValueError as error:
                raise RuntimeError(
                    f"LazyVCS loaded outside its snapshot: {source}"
                ) from error
            return {
                "plugin_root": str(plugin_root),
                "module_source": str(source),
                "module_relative": relative.as_posix(),
                "module_file": regular_file_identity(source),
            }

        self.validate_wsl_sandbox_path(sandbox_path)
        code = """\
import json,os,stat,sys
root=sys.argv[1]
record=os.path.join(root,'run','lazyvcs-loaded-source')
before=os.lstat(record)
if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode) or before.st_size>4096: raise RuntimeError('invalid LazyVCS module record')
fd=os.open(record,os.O_RDONLY|getattr(os,'O_NOFOLLOW',0))
try:
  opened=os.fstat(fd)
  if not stat.S_ISREG(opened.st_mode) or (opened.st_dev,opened.st_ino)!=(before.st_dev,before.st_ino): raise RuntimeError('LazyVCS module record changed while opening')
  data=os.read(fd,4097)
  if len(data)>4096 or os.read(fd,1): raise RuntimeError('LazyVCS module record is too large')
  after=os.fstat(fd)
finally: os.close(fd)
stable=('st_dev','st_ino','st_size','st_mtime_ns','st_ctime_ns')
if any(getattr(opened,x)!=getattr(after,x) for x in stable): raise RuntimeError('LazyVCS module record changed while reading')
lines=data.decode().splitlines()
if len(lines)!=1 or not lines[0]: raise RuntimeError('invalid LazyVCS module record contents')
plugin=os.path.realpath(os.path.join(root,'data','nvim','lazy','lazyvcs.nvim'))
source=os.path.realpath(lines[0])
if os.path.commonpath((plugin,source))!=plugin or source==plugin: raise RuntimeError(f'LazyVCS loaded outside its snapshot: {source}')
print(json.dumps({'plugin_root':plugin,'module_source':source,'module_relative':os.path.relpath(source,plugin).replace(os.sep,'/')}))
"""
        located = self.run_command(
            ["python3", "-c", code, sandbox_path], timeout=30
        )
        if located.returncode != 0:
            raise RuntimeError(
                f"could not validate the loaded WSL LazyVCS module: {located.stderr}"
            )
        result = json.loads(located.stdout)
        result["module_file"] = self.nvim_file_identity(
            str(result["module_source"])
        )
        return result

    @staticmethod
    def sandbox_marker_wait_code() -> str:
        """Target-side stable-file wait used by the WSL Neovim probe."""
        return """\
import os,stat,sys,time
root,name,expected,timeout=sys.argv[1:5]
path=os.path.join(root,'run',name)
deadline=time.monotonic()+float(timeout)
while time.monotonic()<deadline:
  try: before=os.lstat(path)
  except FileNotFoundError:
    time.sleep(0.05); continue
  if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode) or before.st_size>4096: raise RuntimeError('invalid sandbox marker')
  fd=os.open(path,os.O_RDONLY|getattr(os,'O_NOFOLLOW',0))
  try:
    opened=os.fstat(fd)
    if not stat.S_ISREG(opened.st_mode) or (opened.st_dev,opened.st_ino)!=(before.st_dev,before.st_ino): raise RuntimeError('sandbox marker changed while opening')
    data=os.read(fd,4097)
    if len(data)>4096 or os.read(fd,1): raise RuntimeError('sandbox marker is too large')
    after=os.fstat(fd)
  finally: os.close(fd)
  stable=('st_dev','st_ino','st_size','st_mtime_ns','st_ctime_ns')
  if any(getattr(opened,x)!=getattr(after,x) for x in stable): raise RuntimeError('sandbox marker changed while reading')
  if data.decode('utf-8')!=expected+'\\n': raise RuntimeError('unexpected sandbox marker contents')
  print('SANDBOX_MARKER_OK'); raise SystemExit(0)
raise RuntimeError('timed out waiting for sandbox marker')
"""

    def wait_for_nvim_sandbox_marker(
        self,
        sandbox_path: str,
        name: str,
        expected: str,
        *,
        timeout_s: float = 10.0,
    ) -> None:
        """Wait for an exact regular marker without following runtime links."""
        if re.fullmatch(r"[a-z0-9-]+", name) is None:
            raise ValueError(f"invalid Neovim sandbox marker name: {name!r}")
        if re.fullmatch(r"[A-Z0-9_]+", expected) is None:
            raise ValueError("invalid Neovim sandbox marker contents")
        if self.mode == "wsl":
            self.validate_wsl_sandbox_path(sandbox_path)
            waited = self.run_command(
                [
                    "python3",
                    "-I",
                    "-S",
                    "-c",
                    self.sandbox_marker_wait_code(),
                    sandbox_path,
                    name,
                    expected,
                    str(timeout_s),
                ],
                timeout=timeout_s + 5.0,
            )
            if waited.returncode != 0 or waited.stdout.strip() != "SANDBOX_MARKER_OK":
                raise RuntimeError(
                    "configured Neovim readiness marker failed: "
                    f"stdout={waited.stdout!r} stderr={waited.stderr!r}"
                )
            return

        root = self.validate_native_sandbox_path(sandbox_path)
        marker = root / "run" / name
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                value = read_bounded_regular_text(marker)
            except FileNotFoundError:
                time.sleep(0.05)
                continue
            if value != expected + os.linesep:
                raise RuntimeError(
                    f"unexpected configured Neovim readiness marker: {marker}"
                )
            return
        raise RuntimeError(
            f"timed out waiting for configured Neovim readiness marker: {marker}"
        )

    @staticmethod
    def bounded_identity_code() -> str:
        """Python run inside WSL for the same bounded, no-follow identity."""
        return f"""\
import hashlib,json,os,stat,sys
kind,root=sys.argv[1:3]
MAX_ENTRIES={NVIM_SNAPSHOT_MAX_ENTRIES}
MAX_BYTES={NVIM_SNAPSHOT_MAX_BYTES}
MAX_FILE={NVIM_SNAPSHOT_MAX_FILE_BYTES}
MAX_DEPTH={NVIM_SNAPSHOT_MAX_DEPTH}
entries=0
total=0
files=0
tree_hash=hashlib.sha256()
def record_entry(kind,relative,content=b''):
  encoded=relative.replace(os.sep,'/').encode()
  tree_hash.update(kind); tree_hash.update(len(encoded).to_bytes(8,'big')); tree_hash.update(encoded); tree_hash.update(content)
def add_entry(path):
  global entries
  entries+=1
  if entries>MAX_ENTRIES: raise RuntimeError(f'identity exceeds entry limit at {{path}}')
def bounded_children(directory):
  children=[]
  with os.scandir(directory) as scan:
    for entry in scan:
      add_entry(entry.path); children.append(entry)
  children.sort(key=lambda e:e.name)
  return children
def file_digest(path,count_entry=True):
  global total
  before=os.lstat(path)
  if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode): raise RuntimeError(f'identity only accepts a regular file: {{path}}')
  if count_entry: add_entry(path)
  flags=os.O_RDONLY|getattr(os,'O_NOFOLLOW',0)
  fd=os.open(path,flags)
  h=hashlib.sha256(); read_bytes=0
  try:
    opened=os.fstat(fd)
    if not stat.S_ISREG(opened.st_mode) or (opened.st_dev,opened.st_ino)!=(before.st_dev,before.st_ino): raise RuntimeError(f'identity path changed while opening: {{path}}')
    if opened.st_size<0 or opened.st_size>MAX_FILE: raise RuntimeError(f'identity file exceeds per-file limit: {{path}}')
    if total+opened.st_size>MAX_BYTES: raise RuntimeError(f'identity exceeds aggregate byte limit at {{path}}')
    total+=opened.st_size
    while True:
      chunk=os.read(fd,1048576)
      if not chunk: break
      read_bytes+=len(chunk)
      if read_bytes>opened.st_size: raise RuntimeError(f'identity file grew while hashing: {{path}}')
      h.update(chunk)
    after=os.fstat(fd)
  finally:
    os.close(fd)
  stable=('st_dev','st_ino','st_size','st_mtime_ns','st_ctime_ns')
  if read_bytes!=opened.st_size or any(getattr(opened,x)!=getattr(after,x) for x in stable): raise RuntimeError(f'identity file changed while hashing: {{path}}')
  return read_bytes,h.digest(),h.hexdigest()
def walk(directory,relative,depth):
  global files
  if depth>MAX_DEPTH: raise RuntimeError(f'identity exceeds depth limit at {{directory}}')
  before=os.lstat(directory)
  if stat.S_ISLNK(before.st_mode) or not stat.S_ISDIR(before.st_mode): raise RuntimeError(f'identity encountered a linked directory: {{directory}}')
  children=bounded_children(directory)
  for entry in children:
    path=entry.path; rel=os.path.join(relative,entry.name)
    info=entry.stat(follow_symlinks=False)
    if stat.S_ISLNK(info.st_mode): raise RuntimeError(f'identity rejects a symlink: {{path}}')
    if stat.S_ISDIR(info.st_mode):
      record_entry(b'd',rel)
      walk(path,rel,depth+1)
    elif stat.S_ISREG(info.st_mode):
      size,digest,_=file_digest(path,False)
      record_entry(b'f',rel,digest)
      files+=1
    else: raise RuntimeError(f'identity rejects a non-regular entry: {{path}}')
  after=os.lstat(directory)
  stable=('st_dev','st_ino','st_mtime_ns','st_ctime_ns')
  if any(getattr(before,x)!=getattr(after,x) for x in stable): raise RuntimeError(f'identity directory changed while hashing: {{directory}}')
if kind=='tree':
  try: root_stat=os.lstat(root)
  except FileNotFoundError:
    print(json.dumps({{'path':root,'present':False}})); raise SystemExit(0)
  if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode): raise RuntimeError(f'identity root is not a plain directory: {{root}}')
  add_entry(root); walk(root,'',0)
  result={{'path':root,'present':True,'files':files,'bytes':total,'sha256':tree_hash.hexdigest()}}
elif kind=='file':
  size,_raw,digest=file_digest(root)
  result={{'path':root,'bytes':size,'sha256':digest}}
elif kind=='entry-cap-probe':
  class ProbeEntry:
    path='probe-entry'
    name='probe-entry'
  class ProbeScan:
    def __enter__(self): return self
    def __exit__(self,*_args): return False
    def __iter__(self): return self
    def __next__(self):
      if getattr(self,'seen',False): raise AssertionError('entry iterator consumed beyond cap')
      self.seen=True; return ProbeEntry()
  real_scandir=os.scandir
  entries=MAX_ENTRIES
  try:
    os.scandir=lambda _path: ProbeScan()
    bounded_children(root)
  except RuntimeError as error:
    if 'entry limit' not in str(error): raise
    result={{'pre_materialization_cap':True}}
  finally:
    os.scandir=real_scandir
else: raise RuntimeError(f'unknown identity kind: {{kind}}')
print(json.dumps(result,sort_keys=True))
"""

    def nvim_sandbox_setup_command(
        self, marker: str, *, sandbox_path: str
    ) -> str:
        """Activate a prepared sandbox; WSL also copies target-side inputs."""
        source = self.nvim_config_source()
        data_source = self.nvim_data_source()
        if self.powershell:
            root = self.validate_native_sandbox_path(sandbox_path)
            marker_left, marker_right = split_marker(marker)
            return (
                "$KettleSmokeRoot="
                f"{shell_quote(str(root), windows=True)}; "
                "if (-not (Test-Path -LiteralPath $KettleSmokeRoot "
                "-PathType Container)) { "
                "throw 'prepared Neovim sandbox is missing'; }; "
                "$env:HOME=Join-Path $KettleSmokeRoot 'home'; "
                "$env:USERPROFILE=$env:HOME; "
                "$env:XDG_CONFIG_HOME=Join-Path $KettleSmokeRoot 'config'; "
                "$env:XDG_DATA_HOME=Join-Path $KettleSmokeRoot 'data'; "
                "$env:XDG_STATE_HOME=Join-Path $KettleSmokeRoot 'state'; "
                "$env:XDG_CACHE_HOME=Join-Path $KettleSmokeRoot 'cache'; "
                "$env:XDG_RUNTIME_DIR=Join-Path $KettleSmokeRoot 'run'; "
                "$env:KETTLE_SMOKE_ROOT=$KettleSmokeRoot; "
                "$env:LANG='C'; $env:LC_ALL='C'; "
                "Write-Output ("
                f"{shell_quote(marker_left, windows=True)} + "
                f"{shell_quote(marker_right, windows=True)})"
            )

        if self.mode == "wsl":
            self.validate_wsl_sandbox_path(sandbox_path)
            source_expr = (
                self.posix_path_expression(source)
                if self.astro_config
                else source
            )
            data_source_expr = (
                self.posix_path_expression(data_source)
                if self.nvim_data
                else data_source
            )
            copy_commands = (
                self.wsl_bounded_copy_function()
                + "KETTLE_COPY_ENTRIES=0; KETTLE_COPY_BYTES=0; "
                + "KETTLE_COPY_ACTUAL_BYTES=0; "
                f"KETTLE_NVIM_SOURCE={source_expr}; "
                f"KETTLE_NVIM_DATA_SOURCE={data_source_expr}; "
                'if [ -d "$KETTLE_NVIM_SOURCE" ]; then '
                'kettle_smoke_copy_tree "$KETTLE_NVIM_SOURCE" '
                '"$KETTLE_SMOKE_ROOT/config/nvim" || return 1; fi; '
                'mkdir -p "$KETTLE_SMOKE_ROOT/data/nvim" || return 1; '
                "for name in lazy site; do "
                'if [ -d "$KETTLE_NVIM_DATA_SOURCE/$name" ]; then '
                'kettle_smoke_copy_tree '
                '"$KETTLE_NVIM_DATA_SOURCE/$name" '
                '"$KETTLE_SMOKE_ROOT/data/nvim/$name" || return 1; '
                "fi; done; "
            )
        else:
            root = self.validate_native_sandbox_path(sandbox_path)
            sandbox_path = str(root)
            copy_commands = ""

        common_setup = (
            f"KETTLE_SMOKE_ROOT={shlex.quote(sandbox_path)}; "
            '[ -d "$KETTLE_SMOKE_ROOT" ] || return 1; '
            'mkdir -p "$KETTLE_SMOKE_ROOT/home" '
            '"$KETTLE_SMOKE_ROOT/config" "$KETTLE_SMOKE_ROOT/data" '
            '"$KETTLE_SMOKE_ROOT/state" "$KETTLE_SMOKE_ROOT/cache" '
            '"$KETTLE_SMOKE_ROOT/run" || return 1; '
            'chmod 700 "$KETTLE_SMOKE_ROOT/home" '
            '"$KETTLE_SMOKE_ROOT/config" "$KETTLE_SMOKE_ROOT/data" '
            '"$KETTLE_SMOKE_ROOT/state" "$KETTLE_SMOKE_ROOT/cache" '
            '"$KETTLE_SMOKE_ROOT/run" || return 1; '
        )
        marker_left, marker_right = split_marker(marker)
        activation = (
            'export HOME="$KETTLE_SMOKE_ROOT/home" '
            'XDG_CONFIG_HOME="$KETTLE_SMOKE_ROOT/config" '
            'XDG_DATA_HOME="$KETTLE_SMOKE_ROOT/data" '
            'XDG_STATE_HOME="$KETTLE_SMOKE_ROOT/state" '
            'XDG_CACHE_HOME="$KETTLE_SMOKE_ROOT/cache"; '
            'export XDG_RUNTIME_DIR="$KETTLE_SMOKE_ROOT/run" '
            'KETTLE_SMOKE_ROOT LANG=C LC_ALL=C; '
            "printf '%s%s\\n' "
            f"{shlex.quote(marker_left)} {shlex.quote(marker_right)}; "
        )
        return (
            "kettle_smoke_setup_nvim() { "
            + common_setup
            + copy_commands
            + activation
            + "}; kettle_smoke_setup_nvim; "
            + "KETTLE_SMOKE_SETUP_STATUS=$?; "
            + "unset -f kettle_smoke_setup_nvim kettle_smoke_copy_tree "
            + "2>/dev/null; "
            + "unset KETTLE_COPY_ENTRIES KETTLE_COPY_BYTES "
            + "KETTLE_COPY_ACTUAL_BYTES "
            + "KETTLE_NVIM_SOURCE KETTLE_NVIM_DATA_SOURCE name; "
            + 'if [ "$KETTLE_SMOKE_SETUP_STATUS" -eq 0 ]; then '
            + "unset KETTLE_SMOKE_SETUP_STATUS; "
            + "else unset KETTLE_SMOKE_SETUP_STATUS; false; fi"
        )

    def nvim_sandbox_release_command(self, marker: str) -> str:
        """Mark pane work complete without deleting a still-live sandbox.

        Host-side post-exit cleanup first drains exact-environment editor
        daemons through identity-stable handles and only then removes the tree.
        Deleting it from the pane would race a detached plugin helper that can
        still hold or recreate paths beneath it.
        """
        marker_left, marker_right = split_marker(marker)
        if self.powershell:
            return (
                "Write-Output ("
                f"{shell_quote(marker_left, windows=True)} + "
                f"{shell_quote(marker_right, windows=True)})"
            )
        return (
            "printf '%s%s\\n' "
            f"{shlex.quote(marker_left)} {shlex.quote(marker_right)}"
        )

    @staticmethod
    def validate_wsl_sandbox_path(sandbox_path: str) -> None:
        if not re.fullmatch(
            r"/tmp/kettle-agent-tui-[A-Za-z0-9_.-]+", sandbox_path
        ):
            raise ValueError(f"refusing unsafe WSL sandbox path: {sandbox_path}")

    @staticmethod
    def wsl_pidfd_cleanup_code() -> str:
        """Python run inside WSL to drain exact-env processes by pidfd."""
        return (
            "import glob,os,select,signal,sys,time\n"
            "root,scope=sys.argv[1:3]\n"
            "needle=('XDG_CONFIG_HOME='+root+'/config').encode()\n"
            "MAX_ENV=4*1024*1024\n"
            "def alive(fd):\n"
            "  poll=select.poll(); poll.register(fd,select.POLLIN); return not poll.poll(0)\n"
            "def scan():\n"
            "  targets=[]\n"
            "  try:\n"
            "    if scope=='pidfile':\n"
            "      record=root+'/run/nvim.pid'\n"
            "      try:\n"
            "        flags=os.O_RDONLY|os.O_NONBLOCK|getattr(os,'O_NOFOLLOW',0)\n"
            "        fd=os.open(record,flags)\n"
            "        try:\n"
            "          meta=os.fstat(fd)\n"
            "          if not __import__('stat').S_ISREG(meta.st_mode) or meta.st_uid!=os.geteuid() or meta.st_nlink!=1 or meta.st_size>64: raise ValueError('unsafe pid record')\n"
            "          raw=os.read(fd,65).strip()\n"
            "        finally: os.close(fd)\n"
            "        if not raw.isascii() or not raw.isdigit(): raise ValueError('invalid pid')\n"
            "        pids=[int(raw)]\n"
            "      except FileNotFoundError: pids=[]\n"
            "    elif scope=='all':\n"
            "      pids=[]\n"
            "      for path in glob.glob('/proc/[0-9]*/environ'):\n"
            "        try:\n"
            "          if os.stat(os.path.dirname(path)).st_uid==os.geteuid(): pids.append(int(path.split('/')[2]))\n"
            "        except FileNotFoundError: pass\n"
            "    else: raise RuntimeError('unknown cleanup scope: '+scope)\n"
            "    for pid in pids:\n"
            "      envpath=f'/proc/{pid}/environ'; fd=None\n"
            "      try:\n"
            "        fd=os.pidfd_open(pid,0)\n"
            "        if not alive(fd): continue\n"
            "        with open(envpath,'rb') as stream: data=stream.read(MAX_ENV+1)\n"
            "        if len(data)>MAX_ENV: raise RuntimeError(f'oversized environment for pid {pid}')\n"
            "        if alive(fd) and needle in data.split(b'\\0'):\n"
            "          targets.append((pid,fd)); fd=None\n"
            "      except (FileNotFoundError,ProcessLookupError): pass\n"
            "      finally:\n"
            "        if fd is not None:\n"
            "          try: os.close(fd)\n"
            "          except OSError: pass\n"
            "  except BaseException:\n"
            "    for _pid,fd in targets:\n"
            "      try: os.close(fd)\n"
            "      except OSError: pass\n"
            "    raise\n"
            "  return targets\n"
            "overall=time.monotonic()+8.0; quiet=None\n"
            "while time.monotonic()<overall:\n"
            "  targets=scan()\n"
            "  if not targets:\n"
            "    if quiet is None: quiet=time.monotonic()\n"
            "    if time.monotonic()-quiet>=0.3: raise SystemExit(0)\n"
            "    time.sleep(0.05); continue\n"
            "  quiet=None\n"
            "  errors=[]\n"
            "  try:\n"
            "    for pid,fd in targets:\n"
            "      try: signal.pidfd_send_signal(fd,signal.SIGTERM)\n"
            "      except ProcessLookupError: pass\n"
            "      except BaseException as error: errors.append(f'TERM {pid}: {error}')\n"
            "    term_deadline=time.monotonic()+0.5\n"
            "    while time.monotonic()<term_deadline and any(alive(f) for _,f in targets): time.sleep(0.05)\n"
            "    for pid,fd in targets:\n"
            "      if alive(fd):\n"
            "        try: signal.pidfd_send_signal(fd,signal.SIGKILL)\n"
            "        except ProcessLookupError: pass\n"
            "        except BaseException as error: errors.append(f'KILL {pid}: {error}')\n"
            "  finally:\n"
            "    for pid,fd in targets:\n"
            "      try: os.close(fd)\n"
            "      except BaseException as error: errors.append(f'close {pid}: {error}')\n"
            "  if errors: raise RuntimeError('; '.join(errors))\n"
            "raise SystemExit('sandbox process set did not quiesce')\n"
        )

    def require_wsl_pidfd_cleanup(self) -> None:
        """Probe pidfd support and exercise decoy plus spawn-during-drain."""
        if self.mode != "wsl":
            return
        probe = self.run_command(
            [
                "python3",
                "-c",
                (
                    "import os,signal; "
                    "assert hasattr(os,'pidfd_open'); "
                    "assert hasattr(signal,'pidfd_send_signal'); "
                    "fd=os.pidfd_open(os.getpid(),0); os.close(fd)"
                ),
            ],
            timeout=15,
        )
        if probe.returncode != 0:
            raise RuntimeError(
                "selected WSL distro needs Python 3 with pidfd_open and "
                f"pidfd_send_signal support: {probe.stderr.strip()}"
            )
        token = secrets.token_hex(8)
        fixture = f"/tmp/kettle-pidfd-selftest-{token}"
        decoy_root = f"{fixture}-decoy"
        spawn_code = (
            "import os,signal,subprocess,time\n"
            "root=os.environ['KETTLE_PIDFD_ROOT']\n"
            "def stop(_sig,_frame):\n"
            "  child=subprocess.Popen(['/bin/sleep','60'],env=os.environ.copy())\n"
            "  open(root+'/child','w').write(str(child.pid))\n"
            "  raise SystemExit(0)\n"
            "signal.signal(signal.SIGTERM,stop)\n"
            "while True: time.sleep(1)\n"
        )
        # This program is passed through as a string literal in the assembled
        # exercise below, so compiling only that outer program does not parse
        # the child. Compile both generated programs on every host; ordinary CI
        # then catches a broken WSL fixture before a Windows runner needs WSL.
        compile(spawn_code, "<wsl-pidfd-spawn>", "exec")
        exercise_code = f"""\
import glob,os,select,shutil,signal,subprocess,sys,time
root,decoy_root=sys.argv[1:3]
cleanup_code={self.wsl_pidfd_cleanup_code()!r}
spawn_code={spawn_code!r}
os.mkdir(root,0o700)
target_env=os.environ.copy(); target_env['XDG_CONFIG_HOME']=root+'/config'; target_env['KETTLE_PIDFD_ROOT']=root
decoy_env=os.environ.copy(); decoy_env['XDG_CONFIG_HOME']=decoy_root+'/config'
target=None; decoy=None; nondumpable=None; target_fd=None; decoy_fd=None; nondumpable_fd=None
def alive(fd):
  if fd is None: return False
  poll=select.poll(); poll.register(fd,select.POLLIN); return not poll.poll(0)
def exact_env_fds():
  found=[]; needle=('XDG_CONFIG_HOME='+root+'/config').encode()
  for envpath in glob.glob('/proc/[0-9]*/environ'):
    fd=None
    try:
      pid=int(envpath.split('/')[2])
      if os.stat(os.path.dirname(envpath)).st_uid!=os.geteuid(): continue
      fd=os.pidfd_open(pid,0)
      if not alive(fd): os.close(fd); continue
      with open(envpath,'rb') as stream: data=stream.read(4*1024*1024+1)
      if len(data)>4*1024*1024: raise RuntimeError('oversized fixture environment')
      env=data.split(b'\\0')
      if alive(fd) and needle in env: found.append(fd); fd=None
    except (FileNotFoundError,ProcessLookupError): pass
    finally:
      if fd is not None: os.close(fd)
  return found
try:
  target=subprocess.Popen([sys.executable,'-c',spawn_code],env=target_env)
  target_fd=os.pidfd_open(target.pid,0)
  decoy=subprocess.Popen(['/bin/sleep','60'],env=decoy_env)
  decoy_fd=os.pidfd_open(decoy.pid,0)
  time.sleep(0.1)
  cleaned=subprocess.run([sys.executable,'-c',cleanup_code,root,'all'],capture_output=True,text=True,timeout=15)
  if cleaned.returncode!=0: raise RuntimeError(f'cleanup failed: {{cleaned.stderr}}')
  target.wait(timeout=3)
  if alive(target_fd): raise RuntimeError('target pidfd stayed live after reap')
  if not alive(decoy_fd): raise RuntimeError('decoy was killed')
  if not os.path.isfile(root+'/child'): raise RuntimeError('TERM handler did not spawn its child')
  remaining=exact_env_fds()
  try:
    if remaining: raise RuntimeError('exact-env descendant survived cleanup')
  finally:
    for fd in remaining: os.close(fd)
  nondumpable_code="import ctypes,time; ctypes.CDLL(None).prctl(4,0,0,0,0); time.sleep(60)"
  nondumpable=subprocess.Popen([sys.executable,'-c',nondumpable_code],env=target_env)
  nondumpable_fd=os.pidfd_open(nondumpable.pid,0)
  time.sleep(0.1)
  refused=subprocess.run([sys.executable,'-c',cleanup_code,root,'all'],capture_output=True,text=True,timeout=15)
  if refused.returncode==0: raise RuntimeError('unreadable same-user process was declared drained')
  if not alive(nondumpable_fd): raise RuntimeError('unreadable process was signalled without an exact match')
  if not os.path.isdir(root): raise RuntimeError('sandbox disappeared after a failed drain')
finally:
  cleanup_errors=[]
  for name,process,fd in (('target',target,target_fd),('decoy',decoy,decoy_fd),('nondumpable',nondumpable,nondumpable_fd)):
    try:
      if process is not None and process.poll() is None:
        if fd is not None:
          try: signal.pidfd_send_signal(fd,signal.SIGKILL)
          except ProcessLookupError: pass
          except BaseException as error: cleanup_errors.append(f'{{name}} pidfd kill: {{error}}')
        if process.poll() is None: process.kill()
        process.wait(timeout=3)
    except BaseException as error: cleanup_errors.append(f'{{name}} reap: {{error}}')
    finally:
      if fd is not None:
        try: os.close(fd)
        except BaseException as error: cleanup_errors.append(f'{{name}} close: {{error}}')

  drained=False
  try:
    final_cleanup=subprocess.run([sys.executable,'-c',cleanup_code,root,'all'],capture_output=True,text=True,timeout=15)
    if final_cleanup.returncode!=0: cleanup_errors.append('final drain: '+final_cleanup.stderr)
    else: drained=True
  except BaseException as error: cleanup_errors.append(f'final drain: {{error}}')
  if drained:
    try: shutil.rmtree(root)
    except FileNotFoundError: pass
    except BaseException as error: cleanup_errors.append(f'rmtree: {{error}}')
  if cleanup_errors: raise RuntimeError('; '.join(cleanup_errors))
"""
        # Compile the exact assembled preflight, not only its component
        # snippets. A tab in this f-string once made every Windows/WSL smoke
        # fail before the capability fixture could start while host CI passed.
        compile(exercise_code, "<wsl-pidfd-exercise>", "exec")
        exercised = self.run_command(
            ["python3", "-c", exercise_code, fixture, decoy_root], timeout=30
        )
        if exercised.returncode != 0:
            raise RuntimeError(
                "WSL pidfd cleanup failed its decoy/spawn-race self-test: "
                f"stdout={exercised.stdout!r} stderr={exercised.stderr!r}"
            )

    def terminate_nvim_sandbox_host(self, sandbox_path: str) -> None:
        """Terminate only Neovim processes using this WSL smoke sandbox."""
        if self.mode != "wsl":
            raise ValueError("targeted host-side Neovim termination requires WSL")
        self.validate_wsl_sandbox_path(sandbox_path)
        cp = self.run_command(
            [
                "python3",
                "-c",
                self.wsl_pidfd_cleanup_code(),
                sandbox_path,
                "pidfile",
            ],
            timeout=15,
        )
        if cp.returncode != 0:
            raise RuntimeError(
                f"failed to stop WSL Neovim in {sandbox_path}: {cp.stderr}"
            )

    def cleanup_nvim_sandbox_host(
        self,
        sandbox_path: str,
        *,
        windows_job: Optional[WindowsKillJob] = None,
        linux_subreaper: Optional[LinuxSubreaperScope] = None,
    ) -> None:
        """Drain sandbox descendants, then remove the disposable tree."""
        if self.mode == "wsl":
            self.validate_wsl_sandbox_path(sandbox_path)
            stopped = self.run_command(
                [
                    "python3",
                    "-c",
                    self.wsl_pidfd_cleanup_code(),
                    sandbox_path,
                    "all",
                ],
                timeout=15,
            )
            if stopped.returncode != 0:
                raise RuntimeError(
                    "failed to stop WSL Neovim sandbox "
                    f"{sandbox_path}: {stopped.stderr}"
                )
            removed = self.run_command(
                [
                    "bash",
                    "--noprofile",
                    "--norc",
                    "-c",
                    'rm -rf -- "$1" && [ ! -e "$1" ]',
                    "kettle-cleanup",
                    sandbox_path,
                ],
                timeout=120,
            )
            if removed.returncode != 0:
                raise RuntimeError(
                    "failed to remove WSL Neovim sandbox "
                    f"{sandbox_path}: {removed.stderr}"
                )
            return

        root = self.validate_native_sandbox_path(sandbox_path)
        if os.name == "nt" and windows_job is None:
            raise RuntimeError(
                "native Windows Neovim cleanup requires its retained Job Object"
            )

        def make_windows_tree_removable(directory: Path) -> None:
            """Restore owner access without following runtime-created links."""
            directory.chmod(stat.S_IRWXU)
            with os.scandir(directory) as entries:
                for entry in entries:
                    entry_path = Path(entry.path)
                    if entry.is_symlink() or self.path_is_link(entry_path):
                        # Descendants have already drained and the sandbox root
                        # is owner-private. Remove the link object itself; never
                        # recurse into or chmod its target. Configured Neovim
                        # legitimately creates links such as mason/bin tools at
                        # runtime, so refusing them leaks the whole sandbox.
                        if entry.is_symlink():
                            entry_path.unlink()
                        else:
                            # Windows junctions carry the reparse bit but
                            # DirEntry.is_symlink() is false. RemoveDirectoryW
                            # (via rmdir) deletes the junction, not its target.
                            entry_path.rmdir()
                        continue
                    entry_mode = entry.stat(follow_symlinks=False).st_mode
                    if stat.S_ISDIR(entry_mode):
                        make_windows_tree_removable(entry_path)
                    else:
                        entry_path.chmod(stat.S_IWRITE | stat.S_IREAD)

        def remove_tree() -> None:
            nonlocal root
            last_error: Optional[OSError] = None
            for _attempt in range(5):
                root = self.validate_native_sandbox_path(sandbox_path)
                if not root.exists():
                    return
                try:
                    if os.name == "nt":
                        # The retained Job has reached zero active processes,
                        # so no sandbox process can race these Windows path
                        # operations. Reparse points are still removed as leaf
                        # objects rather than followed.
                        make_windows_tree_removable(root)
                        shutil.rmtree(root)
                    else:
                        _remove_tree_by_fd(root)
                    return
                except OSError as error:
                    last_error = error
                    time.sleep(0.2)
            raise RuntimeError(
                f"failed to remove native Neovim sandbox {root}: {last_error}"
            )

        if os.name == "nt":
            assert windows_job is not None
            drain = lambda: _terminate_windows_job(windows_job)
        else:
            if windows_job is not None:
                raise RuntimeError("a Windows Job Object was supplied on a Unix host")
            drain = lambda: self.terminate_native_nvim_sandbox_processes(
                root, linux_subreaper=linux_subreaper
            )
        _drain_then_remove(drain, remove_tree)

    @staticmethod
    def _darwin_process_environment(pid: int) -> Optional[Set[bytes]]:
        """Read one process's real NUL-delimited environment with sysctl."""
        import ctypes

        library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        library.sysctl.argtypes = [
            ctypes.POINTER(ctypes.c_int),
            ctypes.c_uint,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_size_t),
            ctypes.c_void_p,
            ctypes.c_size_t,
        ]
        library.sysctl.restype = ctypes.c_int
        mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2, pid
        size = ctypes.c_size_t()
        if library.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0:
            error = ctypes.get_errno()
            if error in (getattr(os, "ESRCH", 3), getattr(os, "EINVAL", 22)):
                return None
            raise OSError(error, os.strerror(error), pid)
        if size.value < ctypes.sizeof(ctypes.c_int) or size.value > 4 * 1024 * 1024:
            raise RuntimeError(
                f"invalid Darwin process-environment size for {pid}: {size.value}"
            )
        buffer = (ctypes.c_ubyte * size.value)()
        if library.sysctl(mib, 3, buffer, ctypes.byref(size), None, 0) != 0:
            error = ctypes.get_errno()
            if error in (getattr(os, "ESRCH", 3), getattr(os, "EINVAL", 22)):
                return None
            raise OSError(error, os.strerror(error), pid)
        data = bytes(buffer[: size.value])
        argc = struct.unpack_from("=i", data)[0]
        if argc < 0 or argc > 1_000_000:
            raise RuntimeError(f"invalid Darwin process argc for {pid}: {argc}")
        cursor = ctypes.sizeof(ctypes.c_int)

        def skip_string(offset: int) -> int:
            end = data.find(b"\0", offset)
            if end < 0:
                raise RuntimeError(f"truncated Darwin process arguments for {pid}")
            return end + 1

        cursor = skip_string(cursor)  # executable path
        while cursor < len(data) and data[cursor] == 0:
            cursor += 1
        for _ in range(argc):
            cursor = skip_string(cursor)
        while cursor < len(data) and data[cursor] == 0:
            cursor += 1
        environment: Set[bytes] = set()
        for value in data[cursor:].split(b"\0"):
            if value:
                environment.add(value)
        return environment

    @classmethod
    def _native_process_environment(
        cls,
        pid: int,
        *,
        protected_candidates: Optional[Set[int]] = None,
    ) -> Optional[Set[bytes]]:
        """Read only the process environment, never argv rendered by ``ps``."""
        if platform.system() == "Linux":
            process = Path(f"/proc/{pid}")
            try:
                if process.stat().st_uid != os.getuid():
                    return set()
                with (process / "environ").open("rb") as stream:
                    data = stream.read(4 * 1024 * 1024 + 1)
            except (FileNotFoundError, ProcessLookupError):
                return None
            except PermissionError as error:
                # Exact environment membership is the only ownership proof for
                # a daemon that deliberately escaped the PTY session. Linux's
                # child-subreaper scope provides the second, independent
                # relationship used by cleanup; this primitive still defaults
                # to fail-closed when a caller has no such scope.
                #
                # Do not apply that uncertainty to every same-user process:
                # hosted runners and desktop sessions contain unrelated
                # nondumpable services, and one such service would otherwise
                # disable every smoke before Neovim even starts. A readable
                # exact-marker match is still found regardless of ancestry.
                if (
                    protected_candidates is not None
                    and pid not in protected_candidates
                ):
                    return set()
                raise RuntimeError(
                    f"could not inspect same-user process environment {pid}"
                ) from error
            if len(data) > 4 * 1024 * 1024:
                raise RuntimeError(f"oversized process environment for {pid}")
            return {value for value in data.split(b"\0") if value}
        if platform.system() == "Darwin":
            return cls._darwin_process_environment(pid)
        raise RuntimeError(
            f"exact process-environment inspection is unsupported on {platform.system()}"
        )

    @staticmethod
    def _native_process_snapshot(
        *, deadline: Optional[float] = None
    ) -> Tuple[Dict[int, int], Set[int]]:
        timeout = (
            remaining_before(deadline, "inventorying sandbox processes", cap=5)
            if deadline is not None
            else 5
        )
        result = run(["ps", "-axo", "pid=,ppid=,uid="], timeout=timeout)
        if result.returncode != 0:
            raise RuntimeError(
                f"could not audit Neovim sandbox processes: {result.stderr}"
            )
        owner = os.getuid()
        parents: Dict[int, int] = {}
        owned: Set[int] = set()
        for line in result.stdout.splitlines():
            fields = line.strip().split()
            if len(fields) != 3:
                continue
            try:
                pid, parent, uid = map(int, fields)
            except ValueError:
                continue
            if uid == owner:
                parents[pid] = parent
                owned.add(pid)
        return parents, owned

    @classmethod
    def native_nvim_sandbox_processes(
        cls,
        root: Path,
        *,
        owned: Optional[Set[int]] = None,
        deadline: Optional[float] = None,
        retained_by_pid: Optional[Dict[int, StableProcessHandle]] = None,
    ) -> Set[int]:
        """Find same-user processes carrying both exact sandbox variables."""
        if os.name == "nt":
            return set()
        if owned is None:
            _parents, owned = cls._native_process_snapshot(deadline=deadline)
        needles = {
            os.fsencode(f"KETTLE_SMOKE_ROOT={root}"),
            os.fsencode(f"XDG_CONFIG_HOME={root / 'config'}"),
        }
        matches: Set[int] = set()
        for pid in owned:
            if deadline is not None:
                remaining_before(deadline, "inspecting sandbox environments", cap=5)
            retained = (retained_by_pid or {}).get(pid)
            if retained is not None and retained.matches_current():
                # SIGSTOP can make Darwin's process-environment sysctl return
                # EIO. This exact instance is already held and will be killed;
                # only a reused numeric PID needs fresh environment proof.
                continue
            environment = cls._native_process_environment(
                pid,
                # Linux containment below proves unreadable descendants by
                # parentage. Unrelated protected desktop/runner services are
                # not ownership evidence and must not disable every smoke.
                protected_candidates=set(),
            )
            if environment is not None and needles.issubset(environment):
                matches.add(pid)
        return matches

    @classmethod
    def native_nvim_sandbox_handles(
        cls,
        root: Path,
        *,
        owned: Optional[Set[int]] = None,
        deadline: Optional[float] = None,
        retained_by_pid: Optional[Dict[int, StableProcessHandle]] = None,
    ) -> Dict[Tuple[int, ...], StableProcessHandle]:
        """Retain exact-env matches without carrying a numeric PID forward."""
        handles: Dict[Tuple[int, ...], StableProcessHandle] = {}
        needles = {
            os.fsencode(f"KETTLE_SMOKE_ROOT={root}"),
            os.fsencode(f"XDG_CONFIG_HOME={root / 'config'}"),
        }
        for pid in cls.native_nvim_sandbox_processes(
            root,
            owned=owned,
            deadline=deadline,
            retained_by_pid=retained_by_pid,
        ):
            if deadline is not None:
                remaining_before(deadline, "retaining sandbox processes", cap=5)
            try:
                handle = StableProcessHandle.open(pid)
            except (OSError, RuntimeError) as error:
                # A disappearing candidate is benign. A still-matching process
                # for which no identity-stable handle could be acquired is not:
                # treating it as absent would allow the quiet scan to remove a
                # sandbox that process is still using.
                environment = cls._native_process_environment(pid)
                if environment is not None and needles.issubset(environment):
                    close_errors = _close_stable_process_handles(handles)
                    detail = (
                        "; retained-handle close failures: "
                        + "; ".join(str(item) for item in close_errors)
                        if close_errors
                        else ""
                    )
                    raise RuntimeError(
                        f"could not retain matching sandbox process {pid}: {error}"
                        + detail
                    ) from error
                continue
            # Re-read the environment after acquiring the handle, then prove
            # the PID still denotes that handle. This binds the match to the
            # process instance signalled below, even across immediate PID reuse.
            try:
                environment = cls._native_process_environment(pid)
                current_match = (
                    environment is not None
                    and needles.issubset(environment)
                    and handle.matches_current()
                )
            except BaseException as error:
                close_errors = _close_stable_process_handles(
                    [handle, *handles.values()]
                )
                if close_errors:
                    raise RuntimeError(
                        f"{error}; stable-handle close failures: "
                        + "; ".join(str(item) for item in close_errors)
                    ) from error
                raise
            if not current_match:
                close_errors = _close_stable_process_handles([handle])
                if close_errors:
                    raise RuntimeError(
                        "could not close a stale sandbox process handle: "
                        + "; ".join(str(item) for item in close_errors)
                    )
                continue
            handles[handle.identity] = handle
        return handles

    @classmethod
    def terminate_native_nvim_sandbox_processes(
        cls,
        root: Path,
        *,
        linux_subreaper: Optional[LinuxSubreaperScope] = None,
    ) -> None:
        """Freeze and drain escaped editor daemons before sandbox removal.

        Exact environment matches cover Darwin and ordinary Linux daemons. On
        Linux, a child-subreaper scope additionally turns every orphan escaped
        from Kettle into a new direct child of this harness. Those roots are
        retained by process-instance identity, stopped first, and repeatedly
        walked until neither they nor an exact match can fork a missed child.
        """
        if platform.system() == "Linux" and linux_subreaper is None:
            raise RuntimeError("Linux Neovim cleanup requires its subreaper scope")
        deadline = time.monotonic() + 8.0
        quiet_since: Optional[float] = None
        last_observed: Set[int] = set()
        while time.monotonic() < deadline:
            retained: Dict[Tuple[int, ...], StableProcessHandle] = {}
            # Every handle acquired in this pass transfers here before any
            # signal or duplicate close. The finalizer therefore reaches the
            # complete batch even when an identity check or signal fails.
            owned_handles: List[StableProcessHandle] = []
            freeze_error: Optional[BaseException] = None
            try:
                for _attempt in range(8):
                    remaining_before(deadline, "draining sandbox processes", cap=8)
                    parents, owned = cls._native_process_snapshot(deadline=deadline)
                    scanned = cls.native_nvim_sandbox_handles(
                        root,
                        owned=owned,
                        deadline=deadline,
                        retained_by_pid={
                            handle.pid: handle for handle in retained.values()
                        },
                    )
                    owned_handles.extend(scanned.values())
                    batches = [scanned]
                    if linux_subreaper is not None:
                        adopted = linux_subreaper.adopted_roots(
                            parents, owned, deadline=deadline
                        )
                        owned_handles.extend(adopted.values())
                        batches.append(adopted)

                    added = 0
                    new_handles: List[
                        Tuple[Tuple[int, ...], StableProcessHandle]
                    ] = []
                    for batch in batches:
                        for identity, handle in batch.items():
                            if identity in retained:
                                continue
                            retained[identity] = handle
                            added += 1
                            new_handles.append((identity, handle))

                    anchor_pids = {handle.pid for handle in retained.values()}
                    children: Dict[int, List[int]] = {}
                    for pid, parent in parents.items():
                        children.setdefault(parent, []).append(pid)
                    descendants: Set[int] = set()
                    pending = list(anchor_pids)
                    while pending:
                        parent = pending.pop()
                        for pid in children.get(parent, []):
                            if pid not in descendants:
                                descendants.add(pid)
                                pending.append(pid)
                    for pid in descendants:
                        remaining_before(
                            deadline, "retaining sandbox descendants", cap=8
                        )
                        handle = _open_stable_process_if_present(
                            pid, "sandbox descendant"
                        )
                        if handle is None:
                            continue
                        owned_handles.append(handle)
                        if not handle.matches_current():
                            continue
                        if platform.system() == "Linux":
                            current_parent = _linux_process_parent(pid)
                            if current_parent not in anchor_pids | descendants:
                                continue
                        if handle.identity not in retained:
                            retained[handle.identity] = handle
                            added += 1
                            new_handles.append((handle.identity, handle))

                    last_observed = {handle.pid for handle in retained.values()}
                    # The complete exact/adopted/descendant batch is now owned.
                    # A process that vanished through its stable handle stays in
                    # that batch for one final close; any reparented child is a
                    # new subreaper root on the next scan.
                    for _identity, handle in new_handles:
                        handle.signal(signal.SIGSTOP)
                    all_stopped = all(
                        (state := _process_state(handle, deadline=deadline)) is None
                        or state.startswith(("T", "Z"))
                        for handle in retained.values()
                    )
                    if not retained or (added == 0 and all_stopped):
                        break
                    time.sleep(
                        min(
                            0.02,
                            remaining_before(
                                deadline, "waiting for sandbox processes to stop", cap=0.02
                            ),
                        )
                    )
                else:
                    raise RuntimeError("Neovim sandbox process tree did not quiesce")
            except BaseException as error:
                freeze_error = error

            if not retained and freeze_error is None:
                close_errors = _close_stable_process_handles(owned_handles)
                if close_errors:
                    raise RuntimeError(
                        "native Neovim sandbox handle close failed: "
                        + "; ".join(str(error) for error in close_errors)
                    )
                if quiet_since is None:
                    quiet_since = time.monotonic()
                if time.monotonic() - quiet_since >= 0.3:
                    return
                time.sleep(
                    min(
                        0.05,
                        remaining_before(
                            deadline, "confirming an empty sandbox process tree", cap=0.05
                        ),
                    )
                )
                continue
            quiet_since = None
            errors: List[BaseException] = []
            try:
                # Never resume a stopped TERM handler: it could fork the exact
                # late child this stable enumeration exists to prevent.
                for handle in retained.values():
                    try:
                        handle.signal(signal.SIGKILL)
                    except BaseException as error:
                        errors.append(error)
            finally:
                for handle in owned_handles:
                    try:
                        handle.close()
                    except BaseException as error:
                        errors.append(error)
            if freeze_error is not None:
                errors.insert(0, freeze_error)
            if errors:
                raise RuntimeError(
                    "native Neovim sandbox cleanup failed: "
                    + "; ".join(str(error) for error in errors)
                )
        raise RuntimeError(
            "Neovim sandbox processes did not quiesce within 8 seconds for "
            f"{root}: last observed {sorted(last_observed)}"
        )


def _create_owned_nvim_sandbox(
    shell_target: AgentShellTarget,
    register_cleanup: Callable[[Callable[[], None]], None],
    job_factory: Callable[..., WindowsKillJob] = WindowsKillJob,
) -> Tuple[str, Optional[WindowsKillJob]]:
    """Acquire containment before creating a sandbox and immediately own both."""
    windows_job: Optional[WindowsKillJob] = None
    linux_subreaper: Optional[LinuxSubreaperScope] = None
    if shell_target.powershell:
        # Job construction is the containment precondition.  Creating the
        # sandbox first stranded it whenever CreateJobObject/limit setup failed.
        windows_job = job_factory(named=True)
    elif shell_target.mode == "native" and platform.system() == "Linux":
        # Acquire orphan adoption before Neovim or a configured plugin starts.
        # The current direct-child baseline contains Kettle itself; anything
        # newly adopted after Kettle exits is therefore still harness-owned.
        linux_subreaper = LinuxSubreaperScope.acquire()
    try:
        sandbox_path = shell_target.create_nvim_sandbox_host()
    except BaseException as error:
        rollback_errors: List[BaseException] = []
        if windows_job is not None:
            try:
                windows_job.close()
            except BaseException as rollback_error:
                rollback_errors.append(rollback_error)
        if linux_subreaper is not None:
            try:
                linux_subreaper.close()
            except BaseException as rollback_error:
                rollback_errors.append(rollback_error)
        if rollback_errors:
            raise RuntimeError(
                f"{error}; Neovim containment rollback failures: "
                + "; ".join(str(item) for item in rollback_errors)
            ) from error
        raise

    def cleanup(
        path: str = sandbox_path,
        job: Optional[WindowsKillJob] = windows_job,
        subreaper: Optional[LinuxSubreaperScope] = linux_subreaper,
    ) -> None:
        try:
            shell_target.cleanup_nvim_sandbox_host(
                path,
                windows_job=job,
                linux_subreaper=subreaper,
            )
        finally:
            if subreaper is not None:
                subreaper.close()
    try:
        register_cleanup(cleanup)
    except BaseException as error:
        try:
            cleanup()
        except BaseException as cleanup_error:
            raise RuntimeError(
                "could not register Neovim sandbox cleanup and immediate "
                f"cleanup also failed: {cleanup_error}"
            ) from error
        raise
    return sandbox_path, windows_job


class LiveKettle:
    def __init__(
        self,
        kettle: str,
        cfg: Path,
        log: Path,
        extra_args: Optional[List[str]] = None,
        extra_env: Optional[Dict[str, Optional[str]]] = None,
    ):
        self.kettle = kettle
        self.cfg = cfg
        self.log = log
        self.extra_args = extra_args or []
        self.extra_env = extra_env or {}
        self.proc: Optional[subprocess.Popen] = None
        self._tracker_owner_pid: Optional[int] = None
        self._post_exit_cleanup: List[Callable[[], None]] = []
        self._pty_sessions: Set[int] = set()
        self._tracker_sessions: Dict[int, StableProcessHandle] = {}
        self._pty_sessions_by_pane: Dict[int, int] = {}
        self._pty_session_file: Optional[Path] = None
        self._pty_session_fd: Optional[int] = None

    def _close_pty_session_file(self) -> None:
        fd, self._pty_session_fd = self._pty_session_fd, None
        if fd is not None:
            os.close(fd)

    def _prepare_unix_pty_tracker(
        self, argv: List[str]
    ) -> Tuple[List[str], Optional[Dict[str, str]]]:
        """Make the PTY child report its session before running its payload."""
        if os.name == "nt":
            return argv, None

        env = os.environ.copy()
        real_shell = env.get("SHELL") or "/bin/sh"
        root = Path(tempfile.mkdtemp(prefix="kettle-live-ui-shell-"))
        # Context-manager entry can fail after this point, before __enter__ has
        # a chance to return.  Transfer ownership immediately so the startup
        # failure path removes the directory even when chmod/write/chmod fails.
        self.add_post_exit_cleanup(lambda path=root: shutil.rmtree(path))
        root.chmod(0o700)
        shell_name = Path(real_shell).name
        if re.fullmatch(r"[A-Za-z0-9._+-]+", shell_name) is None:
            shell_name = "sh"
        # Keep the original basename so Kettle's shell-integration selection
        # still recognizes zsh/bash/fish; only the path is a wrapper.
        wrapper = root / shell_name
        sessions = root / "sessions"
        wrapper.write_text(
            isolated_python_shebang()
            + """
import contextlib
import os
import signal
import stat
import subprocess
import sys
import time

os.umask(0o077)
sessions = os.environ.pop("KETTLE_SMOKE_PTY_SESSIONS")
real_shell = os.environ.pop("KETTLE_SMOKE_REAL_SHELL")
reject_tcsetpgrp = os.environ.pop("KETTLE_SMOKE_TEST_REJECT_TCSETPGRP", None)
os.environ["SHELL"] = real_shell

flags = os.O_WRONLY | os.O_APPEND
for name in ("O_CLOEXEC", "O_NOFOLLOW"):
    flags |= getattr(os, name, 0)
tracker_fd = os.open(sessions, flags)
try:
    metadata = os.fstat(tracker_fd)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or metadata.st_nlink != 1
        or metadata.st_mode & 0o077
    ):
        raise RuntimeError("unsafe PTY session ownership record")
    record = f"{os.getpid()}\\n".encode("ascii")
    if os.write(tracker_fd, record) != len(record):
        raise RuntimeError("short write to PTY session ownership record")
finally:
    os.close(tracker_fd)

arguments = sys.argv[1:]
if arguments and not arguments[0].startswith("-"):
    target = arguments
else:
    target = [real_shell, *arguments]

# portable-pty made this wrapper the session leader. Keep that exact process
# alive after the payload shell exits so failure cleanup always retains a
# stable session anchor. The child owns the foreground process group, preserving
# ordinary shell job control and the basename-based integration decision Kettle
# made before launching us.
for name in ("SIGTTOU", "SIGTTIN", "SIGTSTP"):
    if hasattr(signal, name):
        signal.signal(getattr(signal, name), signal.SIG_IGN)
handoff_read, handoff_write = os.pipe()
child = os.fork()
if child == 0:
    os.close(handoff_write)
    os.setpgid(0, 0)
    # Do not let the payload touch the controlling terminal until the session
    # leader has made this process group foreground.  Letting both processes
    # race tcsetpgrp allowed the child, after restoring SIGTTOU, to stop itself
    # forever as a background group.
    while True:
        try:
            if os.read(handoff_read, 1) == b"1":
                break
            raise RuntimeError("PTY foreground handoff closed before completion")
        except InterruptedError:
            continue
    os.close(handoff_read)
    for name in ("SIGTTOU", "SIGTTIN", "SIGTSTP"):
        if hasattr(signal, name):
            signal.signal(getattr(signal, name), signal.SIG_DFL)
    os.execvp(target[0], target)

os.close(handoff_read)
try:
    os.setpgid(child, child)
except (ChildProcessError, PermissionError) as error:
    try:
        if os.getpgid(child) != child:
            raise error
    except ProcessLookupError:
        raise error

# `isatty` only says fd 0 refers to a terminal device. A process started with
# `setsid` can inherit such a descriptor without owning that terminal; in that
# case `tcsetpgrp` fails with ENOTTY. Portable-pty normally gives this wrapper
# a real controlling terminal, but treating an unowned tty like redirected
# input keeps explicit-command and diagnostic launches usable without weakening
# the synchronized handoff when job control actually exists.
controls_stdin = False
if os.isatty(0):
    try:
        os.tcgetpgrp(0)
        controls_stdin = True
    except OSError:
        pass
try:
    if controls_stdin:
        if reject_tcsetpgrp == "1":
            raise OSError("intentional PTY foreground handoff failure")
        os.tcsetpgrp(0, child)
except BaseException:
    try:
        os.close(handoff_write)
    finally:
        with contextlib.suppress(ProcessLookupError):
            os.kill(child, signal.SIGKILL)
        while True:
            try:
                os.waitpid(child, 0)
                break
            except InterruptedError:
                continue
    raise
else:
    try:
        os.write(handoff_write, b"1")
    finally:
        os.close(handoff_write)
while True:
    try:
        waited, status = os.waitpid(child, 0)
        if waited == child:
            break
    except InterruptedError:
        continue
if controls_stdin:
    try:
        os.tcsetpgrp(0, os.getpgrp())
    except OSError:
        pass
payload_signal = os.WTERMSIG(status) if os.WIFSIGNALED(status) else None
payload_status = os.WEXITSTATUS(status) if os.WIFEXITED(status) else None
session = os.getsid(0)
while True:
    # Preserve ordinary shell/explicit-command completion. The wrapper remains
    # only while a same-session background process still needs an identity-stable
    # cleanup anchor, then returns the payload's real status.
    members = subprocess.Popen(
        ["ps", "-axo", "pid="],
        text=True,
        encoding="ascii",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    output, _ = members.communicate()
    alive = members.returncode != 0
    if members.returncode == 0:
        for line in output.splitlines():
            fields = line.split()
            if len(fields) == 1:
                try:
                    candidate = int(fields[0])
                    if candidate not in (os.getpid(), members.pid) and os.getsid(candidate) == session:
                        alive = True
                        break
                except (ProcessLookupError, ValueError):
                    continue
    if not alive:
        if payload_signal is not None:
            try:
                signal.signal(payload_signal, signal.SIG_DFL)
            except (OSError, RuntimeError, ValueError):
                pass
            os.kill(os.getpid(), payload_signal)
            os._exit(128 + payload_signal)
        raise SystemExit(payload_status)
    time.sleep(0.05)
""",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        flags = os.O_RDWR | os.O_CREAT | os.O_EXCL
        for name in ("O_CLOEXEC", "O_NOFOLLOW"):
            flags |= getattr(os, name, 0)
        self._pty_session_fd = os.open(sessions, flags, 0o600)
        self.add_post_exit_cleanup(self._close_pty_session_file)
        self._pty_session_file = sessions

        if Path(real_shell).resolve() == wrapper.resolve():
            real_shell = "/bin/sh"
        env["KETTLE_SMOKE_PTY_SESSIONS"] = str(sessions)
        env["KETTLE_SMOKE_REAL_SHELL"] = real_shell

        tracked = list(argv)
        try:
            execute_at = tracked.index("-e")
        except ValueError:
            # portable-pty's default program honors SHELL. The wrapper then
            # execs the original shell with the same login/integration args.
            env["SHELL"] = str(wrapper)
        else:
            if execute_at + 1 >= len(tracked):
                raise ValueError("live-ui -e requires a command to track")
            # Explicit commands bypass SHELL; put the same reporting wrapper in
            # front of that command without changing its argv.
            tracked.insert(execute_at + 1, str(wrapper))
        return tracked, env

    def _open_owned_tracker_session(
        self, session: int, *, deadline: Optional[float] = None
    ) -> Optional[StableProcessHandle]:
        """Retain a direct PTY child through an identity-stable OS handle."""
        if self._tracker_owner_pid is None or session <= 1:
            return None
        if deadline is None:
            deadline = time.monotonic() + PTY_TRACKER_SCAN_TIMEOUT_S

        def is_live_owned_session() -> bool:
            def process_still_exists() -> bool:
                try:
                    # Signal 0 performs no state change; it only distinguishes
                    # a vanished PID from an uncertain ownership probe.
                    os.kill(session, 0)
                    return True
                except ProcessLookupError:
                    return False
                except PermissionError as error:
                    raise RuntimeError(
                        f"permission denied while probing PTY session {session}"
                    ) from error

            if not process_still_exists():
                return False
            parent = run(
                ["ps", "-o", "ppid=", "-p", str(session)],
                timeout=remaining_before(
                    deadline,
                    f"inspecting PTY session {session}",
                    cap=2.0,
                ),
            )
            if parent.returncode != 0:
                if not process_still_exists():
                    return False
                raise RuntimeError(
                    f"could not inspect live PTY session {session}: "
                    f"rc={parent.returncode} stderr={parent.stderr.strip()!r}"
                )
            try:
                parent_pid = int(parent.stdout.strip())
            except ValueError as error:
                if not process_still_exists():
                    return False
                raise RuntimeError(
                    f"malformed parent record for live PTY session {session}: "
                    f"{parent.stdout!r}"
                ) from error
            try:
                leader = os.getsid(session) == session
            except ProcessLookupError:
                return False
            except OSError as error:
                raise RuntimeError(
                    f"could not inspect session identity for {session}: {error}"
                ) from error
            return parent_pid == self._tracker_owner_pid and leader

        for attempt in range(3):
            remaining_before(
                deadline,
                f"retaining PTY session {session}",
                cap=PTY_TRACKER_SCAN_TIMEOUT_S,
            )
            handle: Optional[StableProcessHandle] = None
            try:
                handle = StableProcessHandle.open(session)
            except (OSError, RuntimeError) as error:
                # A stale append-only record is benign. A live wrapper that is
                # still our direct child is not: silently omitting it leaves a
                # PTY session outside Kettle's outer process group with no safe
                # signal target. The numeric check decides only whether to fail;
                # it is never retained or used to signal the process.
                try:
                    still_owned = is_live_owned_session()
                except (
                    OSError,
                    RuntimeError,
                    subprocess.TimeoutExpired,
                ) as check_error:
                    raise RuntimeError(
                        f"could not retain or verify reported PTY session {session}: "
                        f"{error}; ownership check: {check_error}"
                    ) from error
                if still_owned:
                    raise RuntimeError(
                        f"could not retain live owned PTY session {session}: {error}"
                    ) from error
                return None
            try:
                owned = is_live_owned_session()
                same_identity = handle.matches_current() if owned else True
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                handle.close()
                raise RuntimeError(
                    f"could not verify retained PTY session {session}: {error}"
                ) from error
            if not owned:
                handle.close()
                return None
            if same_identity:
                return handle

            # The append-only record may now name a new direct child after PID
            # reuse. The retained old instance is safe to close, but the numeric
            # record is not safe to forget: reopen and re-run the independent
            # parent/session checks in this same pass.
            handle.close()
            if attempt == 2:
                raise RuntimeError(
                    f"PTY session {session} changed identity repeatedly while retaining it"
                )
        raise AssertionError("unreachable")

    def _remember_tracker_sessions(self, *, deadline: Optional[float] = None) -> None:
        """Retain every safely owned PTY session reported by the wrapper."""
        if self._pty_session_file is None or self._pty_session_fd is None:
            return
        if deadline is None:
            deadline = time.monotonic() + PTY_TRACKER_SCAN_TIMEOUT_S
        try:
            held = os.fstat(self._pty_session_fd)
            named = os.lstat(self._pty_session_file)
            if (
                not stat.S_ISREG(held.st_mode)
                or held.st_uid != os.geteuid()
                or held.st_nlink != 1
                or held.st_mode & 0o077
                or (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
            ):
                raise RuntimeError("ownership record is not the retained private file")
            if held.st_size > PTY_TRACKER_MAX_BYTES:
                raise RuntimeError(
                    f"ownership record exceeds {PTY_TRACKER_MAX_BYTES} bytes"
                )
            data = os.pread(
                self._pty_session_fd, PTY_TRACKER_MAX_BYTES + 1, 0
            )
            if len(data) > PTY_TRACKER_MAX_BYTES:
                raise RuntimeError(
                    f"ownership record exceeds {PTY_TRACKER_MAX_BYTES} bytes"
                )
        except (OSError, RuntimeError) as error:
            raise RuntimeError(
                f"could not read PTY session ownership record {self._pty_session_file}: {error}"
            ) from error

        records = data.split(b"\n")
        errors: List[str] = []
        if records[-1] == b"":
            records.pop()
        elif records:
            errors.append("ownership record ends with an incomplete line")
            records.pop()
        if len(records) > PTY_TRACKER_MAX_RECORDS:
            raise RuntimeError(
                "PTY session ownership record contains more than "
                f"{PTY_TRACKER_MAX_RECORDS} records"
            )
        max_pid_digits = len(str(PTY_TRACKER_MAX_PID))
        seen_records: Set[int] = set()
        for line_number, line in enumerate(records, 1):
            if (
                not line
                or len(line) > max_pid_digits
                or line[:1] == b"0"
                or not line.isdigit()
            ):
                errors.append(
                    f"invalid PTY session ownership record at line {line_number}: "
                    f"{line[:80]!r}"
                )
                continue
            session = int(line)
            if session <= 1:
                errors.append(
                    f"PTY session ownership record at line {line_number} is not "
                    f"a valid child PID: {session}"
                )
                continue
            if session > PTY_TRACKER_MAX_PID:
                errors.append(
                    f"PTY session ownership record at line {line_number} exceeds "
                    f"the pid_t limit: {session}"
                )
                continue
            if session in seen_records or session in self._tracker_sessions:
                continue
            seen_records.add(session)
            if time.monotonic() >= deadline:
                errors.append("PTY session ownership scan exceeded its time limit")
                break
            try:
                handle = self._open_owned_tracker_session(
                    session, deadline=deadline
                )
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                errors.append(
                    f"could not retain reported PTY session {session}: {error}"
                )
                continue
            if handle is not None:
                # Publish immediately. If a later verification becomes
                # uncertain, startup cleanup still owns this exact handle.
                self._tracker_sessions[session] = handle
                self._pty_sessions.add(session)

        # Stable handles make PID reuse harmless, but an exited wrapper can no
        # longer anchor its session. The wrapper deliberately outlives its
        # payload, so dropping one here is an explicit cleanup failure rather
        # than the ordinary shell-exit path that used to strand descendants.
        for session, handle in list(self._tracker_sessions.items()):
            if time.monotonic() >= deadline:
                errors.append("PTY session revalidation exceeded its time limit")
                break
            try:
                if not handle.matches_current():
                    replacement = self._open_owned_tracker_session(
                        session, deadline=deadline
                    )
                    if replacement is None:
                        handle.close()
                        del self._tracker_sessions[session]
                        self._pty_sessions.discard(session)
                    else:
                        # Publish the replacement before releasing the stale handle;
                        # cleanup always owns one exact identity even if close fails.
                        self._tracker_sessions[session] = replacement
                        self._pty_sessions.add(session)
                        handle.close()
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                errors.append(f"could not revalidate PTY session {session}: {error}")
        self._pty_sessions.update(self._tracker_sessions)
        if errors:
            shown = errors[:8]
            if len(errors) > len(shown):
                shown.append(f"and {len(errors) - len(shown)} more error(s)")
            raise RuntimeError("; ".join(shown))

    def _remember_direct_child_sessions(self, *, deadline: float) -> None:
        """Retain escaped session leaders after Kettle can no longer spawn."""
        if self._tracker_owner_pid is None:
            return
        listed = run(
            ["ps", "-axo", "pid=,ppid="],
            timeout=remaining_before(
                deadline,
                "listing Kettle's direct children",
                cap=2.0,
            ),
        )
        if listed.returncode != 0:
            raise RuntimeError(
                "could not list Kettle's direct children: "
                f"rc={listed.returncode} stderr={listed.stderr.strip()!r}"
            )
        if len(listed.stdout.encode("utf-8")) > PTY_PROCESS_LIST_MAX_BYTES:
            raise RuntimeError(
                f"process inventory exceeds {PTY_PROCESS_LIST_MAX_BYTES} bytes"
            )
        errors: List[str] = []
        candidates: Set[int] = set()
        for line_number, line in enumerate(listed.stdout.splitlines(), 1):
            fields = line.split()
            if len(fields) != 2:
                errors.append(
                    f"malformed process inventory at line {line_number}: {line[:80]!r}"
                )
                continue
            try:
                pid, parent = (int(field) for field in fields)
            except ValueError:
                errors.append(
                    f"malformed process inventory at line {line_number}: {line[:80]!r}"
                )
                continue
            if parent == self._tracker_owner_pid and 1 < pid <= PTY_TRACKER_MAX_PID:
                candidates.add(pid)

        for session in sorted(candidates):
            if session in self._tracker_sessions:
                continue
            if time.monotonic() >= deadline:
                errors.append("direct-child session scan exceeded its time limit")
                break
            try:
                if os.getsid(session) != session:
                    # A child still in Kettle's outer group was frozen with it
                    # and cannot escape before that complete group is killed.
                    continue
            except ProcessLookupError:
                continue
            except OSError as error:
                errors.append(
                    f"could not inspect direct child session {session}: {error}"
                )
                continue
            try:
                handle = self._open_owned_tracker_session(
                    session, deadline=deadline
                )
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                errors.append(
                    f"could not retain direct child session {session}: {error}"
                )
                continue
            if handle is not None:
                self._tracker_sessions[session] = handle
                self._pty_sessions.add(session)

        if errors:
            shown = errors[:8]
            if len(errors) > len(shown):
                shown.append(f"and {len(errors) - len(shown)} more error(s)")
            raise RuntimeError("; ".join(shown))

    def _freeze_outer_process_group(self) -> None:
        """Stop Kettle from creating another detached PTY before final drain."""
        if self.proc is None or self.proc.returncode is not None:
            return
        try:
            os.killpg(self.proc.pid, signal.SIGSTOP)
        except ProcessLookupError:
            return
        except PermissionError:
            if process_exited_without_reaping(self.proc):
                return
            raise

    def _finalize_tracker_sessions(self, *, deadline: float) -> None:
        """Close the append race after Kettle's spawning group is frozen."""
        errors: List[BaseException] = []
        try:
            self._freeze_outer_process_group()
        except BaseException as error:
            errors.append(error)
        try:
            self._remember_tracker_sessions(
                deadline=min(
                    deadline, time.monotonic() + PTY_TRACKER_SCAN_TIMEOUT_S
                )
            )
        except BaseException as error:
            errors.append(error)
        try:
            self._remember_direct_child_sessions(deadline=deadline)
        except BaseException as error:
            errors.append(error)
        # A directly retained wrapper may have been between setsid and its
        # single append. Re-read once after that scan; Kettle remains stopped,
        # so no new PTY can begin after this point.
        try:
            self._remember_tracker_sessions(
                deadline=min(
                    deadline, time.monotonic() + PTY_TRACKER_SCAN_TIMEOUT_S
                )
            )
        except BaseException as error:
            errors.append(error)
        if errors:
            raise RuntimeError(
                "final PTY tracker drain failed: "
                + "; ".join(str(error) for error in errors)
            )

    def wait_for_tracker_sessions(self, minimum: int, timeout_s: float = 5.0) -> None:
        """Wait until every newly requested Unix PTY has a stable owner.

        A control action returning only proves Kettle accepted it; the PTY
        wrapper may not have run far enough to append its session yet. Tests
        that are about an unexpected Kettle exit must not trigger that exit
        until the wrapper's identity-stable cleanup handle is retained.
        """
        if os.name == "nt":
            raise RuntimeError("PTY session tracker waits are Unix-only")
        if self.proc is None:
            raise RuntimeError("Kettle is not running")
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            self._remember_tracker_sessions()
            if len(self._tracker_sessions) >= minimum:
                return
            if process_exited_without_reaping(self.proc):
                raise RuntimeError(
                    "Kettle exited before its PTY session wrapper was retained"
                )
            time.sleep(0.02)
        raise RuntimeError(
            "timed out retaining PTY session wrapper(s): "
            f"expected at least {minimum}, got {len(self._tracker_sessions)}"
        )

    def _run_post_exit_cleanups(self) -> List[BaseException]:
        cleanup_errors: List[BaseException] = []
        cleanups, self._post_exit_cleanup = self._post_exit_cleanup, []
        for cleanup in reversed(cleanups):
            try:
                cleanup()
            except BaseException as error:
                cleanup_errors.append(error)
                print(
                    f"live-ui smoke: post-exit cleanup failed: {error}",
                    file=sys.stderr,
                )
        return cleanup_errors

    def _cleanup_failed_startup(self) -> List[BaseException]:
        """Release every owner acquired before ``__enter__`` can return."""
        errors: List[BaseException] = []
        try:
            self._terminate_process()
        except BaseException as error:
            errors.append(error)
        finally:
            errors.extend(self._run_post_exit_cleanups())
        return errors

    def _control_pty_inventory(self, *, timeout: float) -> Optional[str]:
        """Return a pane inventory without letting a wedged ctl block cleanup."""
        try:
            probe = self.ctl(
                "list_panes", raw=True, allow_fail=True, timeout=timeout
            )
        except (OSError, subprocess.TimeoutExpired):
            return None
        return probe.stdout if probe.returncode == 0 else None

    def __enter__(self) -> "LiveKettle":
        # Machine-local escape hatch. Every scenario writes its own minimal
        # config, which means it inherits none of the developer's real settings
        # — including a pinned `gpu-device-id`/`gpu-vendor-id`. On a dual-GPU
        # laptop that silently drops the harness onto the integrated GPU, where
        # a driver fault can abort the process before the control server ever
        # comes up (an 0xC0000005 with an empty log). Appending extra config
        # here lets such a machine run the live smokes without hardcoding one
        # developer's hardware into the repo. Unset in CI, so it is a no-op.
        config_additions: List[str] = []
        extra_cfg = os.environ.get("KETTLE_SMOKE_EXTRA_CONFIG", "").strip()
        if extra_cfg:
            config_additions.append(extra_cfg.replace("\\n", "\n").strip())
        if config_additions:
            with self.cfg.open("a", encoding="utf-8") as fh:
                fh.write("\n" + "\n".join(config_additions) + "\n")
        log_f = self.log.open("wb")
        try:
            argv, launch_env = self._prepare_unix_pty_tracker(
                [
                    self.kettle,
                    "--config",
                    str(self.cfg),
                    "--agent-server",
                    "full",
                    *self.extra_args,
                ]
            )
            if self.extra_env:
                if launch_env is None:
                    launch_env = os.environ.copy()
                for name, value in self.extra_env.items():
                    if value is None:
                        launch_env.pop(name, None)
                    else:
                        launch_env[name] = value
            self.proc = subprocess.Popen(
                argv,
                stdout=log_f,
                stderr=subprocess.STDOUT,
                # Own Kettle's outer session from process creation. portable-pty's
                # shell sessions are separate and are inventoried through ctl,
                # frozen, and terminated before this outer group in cleanup.
                start_new_session=os.name != "nt",
                env=launch_env,
            )
            self._tracker_owner_pid = self.proc.pid
        except BaseException:
            cleanup_errors = self._cleanup_failed_startup()
            if cleanup_errors:
                print(
                    "live-ui smoke: launch cleanup failed: "
                    + "; ".join(str(error) for error in cleanup_errors),
                    file=sys.stderr,
                )
            raise
        finally:
            log_f.close()
        return self._finish_startup()

    def _finish_startup(self) -> "LiveKettle":
        """Route every post-launch probe failure through owned cleanup."""
        try:
            return self._await_control_server()
        except BaseException as error:
            cleanup_errors = self._cleanup_failed_startup()
            if isinstance(error, (KeyboardInterrupt, GeneratorExit, SystemExit)):
                if cleanup_errors:
                    print(
                        "live-ui smoke: interrupted startup cleanup failed: "
                        + "; ".join(str(item) for item in cleanup_errors),
                        file=sys.stderr,
                    )
                raise
            message = f"live-ui smoke: startup probe failed: {error}"
            if cleanup_errors:
                message += "\ncleanup errors: " + "; ".join(
                    str(item) for item in cleanup_errors
                )
            raise SystemExit(message) from error

    def _await_control_server(self) -> "LiveKettle":
        """Complete startup after launch; the caller owns failure cleanup."""
        assert self.proc is not None
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            self._remember_tracker_sessions()
            if process_exited_without_reaping(self.proc):
                raise RuntimeError(
                    "live-ui smoke: kettle exited before control server came up\n"
                    + self.log.read_text(errors="replace")
                )
            inventory = self._control_pty_inventory(
                timeout=max(0.1, min(1.0, deadline - time.monotonic()))
            )
            if inventory is not None:
                self._remember_pty_sessions(inventory)
                return self
            time.sleep(0.1)
        raise RuntimeError("live-ui smoke: timed out waiting for control server")

    def _terminate_process(self) -> None:
        if self.proc is None:
            return
        if os.name == "nt":
            if self.proc.poll() is None:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait(timeout=5)
        else:
            session_errors: List[BaseException] = []
            cleanup_deadline = time.monotonic() + PTY_TRACKER_FINALIZE_TIMEOUT_S
            try:
                # Refresh while the control server and PTY leaders are alive,
                # but never let a wedged control client skip process cleanup.
                try:
                    self._remember_tracker_sessions()
                except Exception as error:
                    session_errors.append(error)
                inventory = self._control_pty_inventory(timeout=1)
                if inventory is not None:
                    self._remember_pty_sessions(inventory)
            finally:
                try:
                    self._finalize_tracker_sessions(deadline=cleanup_deadline)
                except Exception as error:
                    session_errors.append(error)
                for session in sorted(self._pty_sessions):
                    try:
                        anchor = self._tracker_sessions.get(session)
                        if anchor is None:
                            raise RuntimeError(
                                f"PTY session {session} has no stable tracker handle"
                            )
                        terminate_owned_pty_session(anchor)
                    except Exception as error:
                        session_errors.append(error)
                session_errors.extend(
                    _close_stable_process_handles(self._tracker_sessions)
                )
                self._tracker_sessions.clear()
                try:
                    terminate_owned_process_group(self.proc)
                except Exception as error:
                    session_errors.append(error)
            if session_errors:
                raise RuntimeError(
                    "live-ui smoke: PTY-session cleanup failed: "
                    + "; ".join(str(error) for error in session_errors)
                )

    def _remember_pty_sessions(self, payload: str) -> None:
        """Record pane sessions without erasing a transiently unavailable pid."""
        try:
            result = json.loads(payload)
        except json.JSONDecodeError:
            return
        sessions_by_pane: Dict[int, int] = {}
        unresolved = False
        for pane in result.get("panes", []):
            if not isinstance(pane, dict):
                continue
            pane_id = pane.get("id")
            if not isinstance(pane_id, int):
                continue
            child_pid = pane.get("child_pid")
            # Control output is useful for pane association, not ownership.
            # Only the independent wrapper record can introduce a session,
            # after its direct-parent check and stable handle acquisition.
            if isinstance(child_pid, int) and child_pid in self._tracker_sessions:
                sessions_by_pane[pane_id] = child_pid
            elif pane_id in self._pty_sessions_by_pane:
                sessions_by_pane[pane_id] = self._pty_sessions_by_pane[pane_id]
            else:
                unresolved = True
        sessions = set(sessions_by_pane.values())
        sessions.update(self._tracker_sessions)
        if unresolved:
            # On the first inventory the wrapper is the only independent
            # source for a contended child lock. Do not throw that anchor away.
            sessions.update(self._pty_sessions)
        self._pty_sessions_by_pane = sessions_by_pane
        self._pty_sessions = sessions

    def __exit__(
        self, exc_type: object, _exc_value: object, _traceback: object
    ) -> None:
        errors: List[Exception] = []
        try:
            self._terminate_process()
        except Exception as error:
            errors.append(error)
            print(f"live-ui smoke: process cleanup failed: {error}", file=sys.stderr)
        finally:
            errors.extend(self._run_post_exit_cleanups())
        if errors and exc_type is None:
            raise RuntimeError(
                "live-ui smoke: cleanup failed: "
                + "; ".join(str(error) for error in errors)
            )

    def add_post_exit_cleanup(self, cleanup: Callable[[], None]) -> None:
        self._post_exit_cleanup.append(cleanup)

    @property
    def pid(self) -> int:
        assert self.proc is not None
        return int(self.proc.pid)

    def ctl(
        self,
        method: str,
        *,
        params: Optional[Dict[str, object]] = None,
        raw: bool = False,
        allow_fail: bool = False,
        timeout: float = 10,
    ) -> subprocess.CompletedProcess:
        argv = [self.kettle, "ctl", "--pid", str(self.pid), method]
        if params is not None:
            argv += ["--json", json.dumps(params)]
        if raw:
            argv.append("--raw")
        cp = run(argv, timeout=timeout)
        if cp.returncode != 0 and not allow_fail:
            raise SystemExit(f"kettle ctl {method} failed:\n{cp.stderr}\n{cp.stdout}")
        return cp

    def json_ctl(self, method: str, params: Optional[Dict[str, object]] = None) -> Dict[str, object]:
        return json.loads(self.ctl(method, params=params, raw=True).stdout)

    def read_cells(self) -> Dict[str, object]:
        """Read the complete visible grid through the bounded control API."""
        for attempt in range(5):
            try:
                result = self.json_ctl("read_cells", {"limit": 1536})
                cells = list(result.get("cells", []))
                cursor = result.get("next_cursor")
                snapshot = result.get("snapshot")
                while cursor is not None:
                    page = self.json_ctl(
                        "read_cells",
                        {"cursor": cursor, "limit": 1536, "snapshot": snapshot},
                    )
                    cells.extend(page.get("cells", []))
                    cursor = page.get("next_cursor")
                result["cells"] = cells
                result["next_cursor"] = None
                result["truncated"] = False
                return result
            except SystemExit as error:
                if "stale_snapshot" not in str(error) or attempt == 4:
                    raise
                time.sleep(0.05)
        raise AssertionError("unreachable")

    def screenshot(self, path: Path) -> None:
        self.ctl("screenshot", params={"full_window": True, "path": str(path)}, timeout=12)

    def screenshot_if_visible(self, path: Path) -> bool:
        """Capture an optional diagnostic without weakening visual scenarios.

        The touchpad scenario proves scroll behavior through the control-plane
        grid state; its PNGs are supporting artifacts, not assertions. A
        Windows GUI started from an SSH session can be fully alive yet unmapped
        on the interactive desktop, in which case Kettle intentionally refuses
        capture. Only that precise state is optional. All renderer, transport,
        and filesystem errors still fail the smoke.
        """
        result = self.ctl(
            "screenshot",
            params={"full_window": True, "path": str(path)},
            raw=True,
            timeout=12,
            allow_fail=True,
        )
        if result.returncode == 0:
            return True
        if is_optional_remote_windows_screenshot_error(
            platform.system(),
            dict(os.environ),
            stdout=result.stdout,
            stderr=result.stderr,
        ):
            print(
                "live-ui smoke: optional touchpad screenshot skipped because "
                "the remote Windows window is not mapped"
            )
            return False
        raise SystemExit(f"kettle ctl screenshot failed:\n{result.stderr}\n{result.stdout}")

    def wait_for_text(self, text: str, timeout_ms: int = 8000, quiet_ms: int = 200) -> None:
        result = json.loads(
            self.ctl(
                "wait_for",
                params={"text": text, "timeout_ms": timeout_ms, "quiet_ms": quiet_ms},
                raw=True,
                timeout=(timeout_ms / 1000.0) + 5.0,
            ).stdout
        )
        if not result.get("matched"):
            raise SystemExit(f"live-ui smoke: timed out waiting for {text!r}: {result}")

    def wait_for_nvim_sidebar_evidence(
        self,
        repo_name: str,
        fixture_token: str,
        timeout_ms: int = 120000,
        quiet_ms: int = 500,
    ) -> Tuple[Dict[str, object], Dict[str, object]]:
        """Wait for stable LazyVCS/editor cells, dismissing Neovim's pager.

        A configured plugin loaded by LazyVCS may emit an unrelated warning.
        Neovim covers the whole grid with its hit-enter prompt until one Enter
        is received, hiding the sidebar even though it rendered successfully.
        Polling also lets the explicit failure message surface immediately.
        Readiness itself is the cell-grid contract rather than a token written
        into the plugin-owned sidebar buffer, which LazyVCS may refresh later.
        """
        deadline = time.monotonic() + timeout_ms / 1000.0
        state = NvimSidebarWaitState(repo_name, quiet_ms / 1000.0)
        last_screen: Dict[str, object] = {}
        last_missing: List[str] = ["no observation"]
        while time.monotonic() < deadline:
            last_screen = self.json_ctl("read_screen")
            visible = str(last_screen.get("text", ""))
            if "KETTLE_LAZYVCS_SIDEBAR_ABSENT" in visible:
                raise SystemExit(
                    "agent-tui smoke: LazyVCS sidebar did not render:\n" + visible
                )
            ready, dismiss = state.observe(
                visible,
                str(last_screen.get("snapshot", "")),
                last_screen.get("cursor"),
                int(last_screen.get("history_size", 0)),
                time.monotonic(),
            )
            if dismiss:
                self.ctl("send_keys", params={"keys": ["enter"]})
            if ready:
                cells = self.read_cells()
                last_missing = lazyvcs_screen_evidence(
                    cells,
                    repo_name=repo_name,
                    fixture_token=fixture_token,
                )
                if not last_missing:
                    return cells, last_screen
            time.sleep(0.1)
        raise SystemExit(
            "live-ui smoke: timed out waiting for stable LazyVCS evidence "
            f"for {repo_name!r}: missing={last_missing} screen={last_screen}"
        )


class EventStream:
    def __init__(self, live: LiveKettle, path: Path):
        self.path = path
        self.proc = subprocess.Popen(
            [live.kettle, "ctl", "--pid", str(live.pid), "events"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )
        self.lines: List[str] = []
        self.stderr_lines: List[str] = []
        self._events: "queue.Queue[Dict[str, object]]" = queue.Queue()
        self._stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self._stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self._stdout_thread.start()
        self._stderr_thread.start()

    def _read_stdout(self) -> None:
        if self.proc.stdout is None:
            return
        for raw_line in self.proc.stdout:
            line = raw_line.strip()
            if not line:
                continue
            self.lines.append(line)
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(ev, dict):
                self._events.put(ev)

    def _read_stderr(self) -> None:
        if self.proc.stderr is None:
            return
        for line in self.proc.stderr:
            self.stderr_lines.append(line.rstrip("\n"))

    def wait_for(
        self,
        event_name: str,
        expected_data: Dict[str, object],
        timeout_s: float = 8.0,
    ) -> Dict[str, object]:
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            if self.proc.poll() is not None and self._events.empty():
                break
            try:
                ev = self._events.get(timeout=min(0.1, max(0.01, deadline - time.monotonic())))
            except queue.Empty:
                continue
            if ev.get("event") != event_name:
                continue
            data = ev.get("data")
            if not isinstance(data, dict):
                continue
            if all(data.get(k) == v for k, v in expected_data.items()):
                return ev
        self.close()
        raise SystemExit(
            f"live-ui smoke: did not observe {event_name} event with {expected_data}; "
            f"events={self.lines} stderr={self.stderr_lines}"
        )

    def close(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=3)
        self._stdout_thread.join(timeout=1)
        self._stderr_thread.join(timeout=1)
        self.path.write_text(("\n".join(self.lines) + "\n") if self.lines else "")
        if self.stderr_lines:
            self.path.with_suffix(".stderr.txt").write_text("\n".join(self.stderr_lines) + "\n")


def rect_center(rect: Dict[str, float]) -> Tuple[float, float]:
    return rect["x"] + rect["width"] / 2, rect["y"] + rect["height"] / 2


def rect_contains(rect: Dict[str, float], x: int, y: int, margin: int = 3) -> bool:
    return (
        x >= rect["x"] - margin
        and x < rect["x"] + rect["width"] + margin
        and y >= rect["y"] - margin
        and y < rect["y"] + rect["height"] + margin
    )


def changed_pixels(a_path: Path, b_path: Path, y0: float, y1: float) -> List[Tuple[int, int]]:
    aw, ah, a = read_rgba_png(a_path)
    bw, bh, b = read_rgba_png(b_path)
    if (aw, ah) != (bw, bh):
        raise SystemExit("live-ui smoke: screenshot dimensions changed")
    changed: List[Tuple[int, int]] = []
    for y in range(max(0, int(y0)), min(ah, int(y1))):
        ra = a[y]
        rb = b[y]
        for x in range(aw):
            off = x * 4
            if ra[off : off + 4] != rb[off : off + 4]:
                changed.append((x, y))
    return changed


def selection_drag_points(cells: Dict[str, object], content: Dict[str, float]) -> Tuple[float, float, float, float]:
    rows = max(1, int(cells.get("rows", 1)))
    cols = max(1, int(cells.get("cols", 1)))
    cell_w = float(content["width"]) / cols
    cell_h = float(content["height"]) / rows
    by_row: Dict[int, List[int]] = {}
    for cell in cells.get("cells", []):  # type: ignore[assignment]
        ch = str(cell.get("ch", ""))
        if ch.strip():
            by_row.setdefault(int(cell["row"]), []).append(int(cell["col"]))
    candidates = [(len(cols_for_row), row, sorted(cols_for_row)) for row, cols_for_row in by_row.items()]
    candidates.sort(reverse=True)
    for count, row, cols_for_row in candidates:
        if count < 8:
            break
        left = max(0, cols_for_row[0])
        right = min(cols - 1, max(cols_for_row[-1], left + 6))
        if right > left:
            y = float(content["y"]) + (row + 0.5) * cell_h
            x0 = float(content["x"]) + (left + 0.25) * cell_w
            x1 = float(content["x"]) + (right + 0.75) * cell_w
            return x0, y, x1, y
    raise SystemExit("interaction smoke: could not find a visible text row for selection drag")


def macos_window_frame(pid: int) -> Tuple[float, float, float, float]:
    # System Events can report zero accessibility windows for a valid custom
    # winit NSWindow. CoreGraphics owns the screen-space bounds used by the
    # native event injector and sees that window regardless of its AX shape.
    # This is a geometry lookup only; the scenario activates the app with its
    # positive-control titlebar click before it sends the selection press.
    require_cmd("swift")
    script = r"""
import CoreGraphics
import Foundation

guard let rawPID = ProcessInfo.processInfo.environment["KETTLE_SMOKE_PID"],
      let pid = Int32(rawPID) else {
    FileHandle.standardError.write(Data("missing KETTLE_SMOKE_PID\n".utf8))
    exit(2)
}
let rows = CGWindowListCopyWindowInfo(
    [.optionOnScreenOnly, .excludeDesktopElements],
    kCGNullWindowID
)! as! [[String: Any]]
let matches = rows.compactMap { row -> (CGRect, CGFloat)? in
    guard (row[kCGWindowOwnerPID as String] as? Int32) == pid,
          (row[kCGWindowLayer as String] as? Int) == 0,
          let raw = row[kCGWindowBounds as String] as? NSDictionary else {
        return nil
    }
    var rect = CGRect.zero
    guard CGRectMakeWithDictionaryRepresentation(raw as CFDictionary, &rect),
          rect.width > 0, rect.height > 0 else {
        return nil
    }
    return (rect, rect.width * rect.height)
}.sorted { $0.1 > $1.1 }
guard let rect = matches.first?.0 else {
    FileHandle.standardError.write(
        Data("no visible layer-0 CoreGraphics window for pid \(pid)\n".utf8)
    )
    exit(2)
}
print("\(rect.minX),\(rect.minY),\(rect.width),\(rect.height)")
"""
    deadline = time.monotonic() + 5.0
    while True:
        try:
            result = subprocess.run(
                ["swift", "-e", script],
                capture_output=True,
                text=True,
                timeout=8,
                check=False,
                env={**os.environ, "KETTLE_SMOKE_PID": str(pid)},
            )
        except subprocess.TimeoutExpired as error:
            raise SystemExit(
                "selection-autoscroll smoke: timed out reading the native "
                "macOS window frame from CoreGraphics"
            ) from error
        if result.returncode == 0:
            break
        if time.monotonic() >= deadline:
            raise SystemExit(
                "selection-autoscroll smoke: could not read the native macOS "
                "window frame after waiting for CoreGraphics: "
                + result.stderr.strip()
            )
        time.sleep(0.1)
    values = [float(value.strip()) for value in result.stdout.strip().split(",")]
    if len(values) != 4:
        raise SystemExit(
            f"selection-autoscroll smoke: malformed macOS window frame {result.stdout!r}"
        )
    return values[0], values[1], values[2], values[3]


MACOS_LEFT_MOUSE_DOWN = 1
MACOS_LEFT_MOUSE_UP = 2
MACOS_MOUSE_MOVED = 5
MACOS_LEFT_MOUSE_DRAGGED = 6


def macos_mouse_event(event_type: int, x: float, y: float) -> None:
    import ctypes

    class CGPoint(ctypes.Structure):
        _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]

    core_graphics = ctypes.CDLL(
        "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"
    )
    core_foundation = ctypes.CDLL(
        "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
    )
    core_graphics.CGEventCreateMouseEvent.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        CGPoint,
        ctypes.c_uint32,
    ]
    core_graphics.CGEventCreateMouseEvent.restype = ctypes.c_void_p
    core_graphics.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
    core_graphics.CGPreflightPostEventAccess.restype = ctypes.c_bool
    core_foundation.CFRelease.argtypes = [ctypes.c_void_p]
    if not core_graphics.CGPreflightPostEventAccess():
        raise SystemExit(
            "selection-autoscroll smoke: grant Accessibility access to the invoking terminal"
        )
    event = core_graphics.CGEventCreateMouseEvent(None, event_type, CGPoint(x, y), 0)
    if not event:
        raise SystemExit("selection-autoscroll smoke: CGEventCreateMouseEvent failed")
    core_graphics.CGEventPost(0, event)
    core_foundation.CFRelease(event)


def text_cell_point(
    cells: Dict[str, object],
    geometry: Dict[str, object],
    needle: str,
    *,
    at_end: bool = False,
) -> Tuple[float, float]:
    rows = max(1, int(cells.get("rows", 1)))
    cols = max(1, int(cells.get("cols", 1)))
    grid = [[" " for _ in range(cols)] for _ in range(rows)]
    for cell in cells.get("cells", []):  # type: ignore[assignment]
        row = int(cell.get("row", -1))
        col = int(cell.get("col", -1))
        if 0 <= row < rows and 0 <= col < cols:
            ch = str(cell.get("ch", " "))
            grid[row][col] = ch[0] if ch else " "
    for row, chars in enumerate(grid):
        text = "".join(chars)
        start = text.find(needle)
        if start < 0:
            continue
        col = start + (len(needle) - 1 if at_end else 0)
        content = geometry["content"]  # type: ignore[index]
        cell = geometry["cell"]  # type: ignore[index]
        padding = geometry["padding"]  # type: ignore[index]
        cell_w = float(cell["width"])  # type: ignore[index]
        cell_h = float(cell["height"])  # type: ignore[index]
        padding_x = float(padding["x"])  # type: ignore[index]
        padding_y = float(padding["y"])  # type: ignore[index]
        x_bias = 0.75 if at_end else 0.25
        return (
            float(content["x"]) + padding_x + (col + x_bias) * cell_w,  # type: ignore[index]
            float(content["y"]) + padding_y + (row + 0.5) * cell_h,  # type: ignore[index]
        )
    raise SystemExit(f"interaction smoke: could not locate visible marker {needle!r}")


def wait_for_text_cell_point(
    live: "LiveKettle",
    needle: str,
    *,
    at_end: bool = False,
    timeout: float = 3.0,
) -> Tuple[float, float]:
    """Wait until a terminal-state change has reached the rendered cell grid."""
    deadline = time.monotonic() + timeout
    while True:
        cells = live.read_cells()
        geometry = live.json_ctl("ui_geometry")
        try:
            return text_cell_point(cells, geometry, needle, at_end=at_end)
        except SystemExit:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.05)


def visible_context_row(geometry: Dict[str, object], label: str) -> Dict[str, object]:
    menu = geometry.get("context_menu")
    if not isinstance(menu, dict):
        raise SystemExit(f"interaction smoke: no context menu while looking for {label!r}")
    rows = [
        row for row in menu.get("rows", [])  # type: ignore[union-attr]
        if row.get("label") == label and row.get("dispatchable")
    ]
    if len(rows) != 1:
        raise SystemExit(f"interaction smoke: expected one dispatchable {label!r} row, got {rows}")
    return rows[0]


def modal_open(geometry: Dict[str, object], name: str) -> bool:
    modals = geometry.get("modals", {})
    return isinstance(modals, dict) and bool(modals.get(name))


def rect_intersects(a: Dict[str, object], b: Dict[str, object]) -> bool:
    ax0 = float(a.get("x", 0.0))
    ay0 = float(a.get("y", 0.0))
    ax1 = ax0 + float(a.get("width", 0.0))
    ay1 = ay0 + float(a.get("height", 0.0))
    bx0 = float(b.get("x", 0.0))
    by0 = float(b.get("y", 0.0))
    bx1 = bx0 + float(b.get("width", 0.0))
    by1 = by0 + float(b.get("height", 0.0))
    return ax0 < bx1 and ax1 > bx0 and ay0 < by1 and ay1 > by0


def wait_for_resize(
    live: LiveKettle,
    before_width: int,
    before_height: int,
    before_cols: int,
    before_rows: int,
    timeout_s: float = 8.0,
) -> Tuple[Dict[str, object], Dict[str, object]]:
    deadline = time.monotonic() + timeout_s
    last_geo: Dict[str, object] = {}
    last_cells: Dict[str, object] = {}
    while time.monotonic() < deadline:
        last_geo = live.json_ctl("ui_geometry")
        last_cells = live.read_cells()
        surface = last_geo.get("surface", {})
        width = int(surface.get("width", 0))
        height = int(surface.get("height", 0))
        cols = int(last_cells.get("cols", 0))
        rows = int(last_cells.get("rows", 0))
        if (
            (width, height) != (before_width, before_height)
            and (cols, rows) != (before_cols, before_rows)
            and last_geo.get("resize_overlay")
            and cols > 0
            and rows > 0
        ):
            return last_geo, last_cells
        time.sleep(0.1)
    raise SystemExit(
        "interaction smoke: resize did not settle: "
        f"before_surface=({before_width},{before_height}) "
        f"last_surface={last_geo.get('surface')} "
        f"before_cells=({before_cols},{before_rows}) "
        f"last_cells=({last_cells.get('cols')},{last_cells.get('rows')}) "
        f"resize_overlay={last_geo.get('resize_overlay')}"
    )


def active_rect(geometry: Dict[str, object]) -> Tuple[Dict[str, float], int]:
    tab_bar = geometry["tab_bar"]  # type: ignore[index]
    active = [s for s in tab_bar["segments"] if s.get("active")]  # type: ignore[index]
    if len(active) != 1:
        raise SystemExit(f"live-ui smoke: expected one active tab, got {active}")
    return active[0]["rect"], int(active[0]["index"])


def tab_segment_layout_error(
    tab_bar: Dict[str, object], *, tolerance: float = 1.0
) -> Optional[str]:
    raw_segments = tab_bar.get("segments")
    if not isinstance(raw_segments, list) or not raw_segments:
        return "tab bar has no segments"
    try:
        rects = [segment["rect"] for segment in raw_segments]
        widths = [float(rect["width"]) for rect in rects]
        xs = [float(rect["x"]) for rect in rects]
    except (KeyError, TypeError, ValueError):
        return "tab bar contains malformed segment rectangles"
    if any(width <= 0.0 for width in widths):
        return f"tab segment widths must be positive: {widths}"

    first_x = xs[0]
    first_width = widths[0]
    for index, x in enumerate(xs):
        expected_x = first_x + first_width * index
        if abs(x - expected_x) > tolerance:
            return (
                "tab segment boundary is not aligned to the common seam: "
                f"index={index} x={x} expected={expected_x}"
            )

    # Legacy layouts divided the entire strip into equal tabs.
    if all(abs(width - first_width) <= tolerance for width in widths[1:]):
        return None

    if not all(
        abs(width - first_width) <= tolerance for width in widths[1:-1]
    ):
        return f"non-final tab segments are not equal: {widths}"
    try:
        new_tab = tab_bar["new_tab"]
        new_tab_menu = tab_bar["new_tab_menu"]
        new_tab_width = float(new_tab["width"])
        new_tab_menu_width = float(new_tab_menu["width"])
        new_tab_x = float(new_tab["x"])
        new_tab_menu_x = float(new_tab_menu["x"])
    except (KeyError, TypeError, ValueError):
        return "tab bar contains malformed new-tab button rectangles"
    button_width = new_tab_width + new_tab_menu_width
    expected_last_width = first_width - button_width
    if expected_last_width <= 0.0 or abs(widths[-1] - expected_last_width) > tolerance:
        return (
            "final tab does not reserve the new-tab button strip: "
            f"width={widths[-1]} expected={expected_last_width}"
        )
    button_left = min(
        x
        for x, width in (
            (new_tab_x, new_tab_width),
            (new_tab_menu_x, new_tab_menu_width),
        )
        if width > 0.0
    )
    last_right = xs[-1] + widths[-1]
    if abs(last_right - button_left) > tolerance:
        return (
            "final tab does not meet the new-tab button strip: "
            f"right={last_right} strip_left={button_left}"
        )
    return None


def tab_bar_geometry_signature(geometry: Dict[str, object]) -> Tuple[object, ...]:
    tab_bar = geometry.get("tab_bar")
    if not isinstance(tab_bar, dict):
        raise RuntimeError("tabbar smoke: ui_geometry has no tab_bar")
    segments = tab_bar.get("segments")
    if not isinstance(segments, list):
        raise RuntimeError("tabbar smoke: ui_geometry has malformed segments")
    segment_signature = []
    for segment in segments:
        if not isinstance(segment, dict) or not isinstance(segment.get("rect"), dict):
            raise RuntimeError("tabbar smoke: ui_geometry has malformed segment")
        rect = segment["rect"]
        segment_signature.append(
            (
                segment.get("index"),
                segment.get("active"),
                segment.get("title"),
                segment.get("fitted_title"),
                segment.get("path"),
                rect.get("x"),
                rect.get("width"),
            )
        )
    return tuple(segment_signature)


def wait_for_stable_tab_bar(
    read_geometry: Callable[[], Dict[str, object]],
    *,
    timeout_seconds: float = 5.0,
    quiet_seconds: float = 0.5,
    poll_seconds: float = 0.05,
    monotonic: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> Dict[str, object]:
    if timeout_seconds <= 0 or quiet_seconds < 0 or poll_seconds <= 0:
        raise ValueError("tab-bar stability waits require positive bounded timing")
    started = monotonic()
    stable_since = started
    last_signature: Optional[Tuple[object, ...]] = None
    last_geometry: Dict[str, object] = {}
    polls = 0
    while True:
        last_geometry = read_geometry()
        signature = tab_bar_geometry_signature(last_geometry)
        polls += 1
        now = monotonic()
        if signature != last_signature:
            last_signature = signature
            stable_since = now
        elif now - stable_since >= quiet_seconds:
            return last_geometry
        if now - started >= timeout_seconds:
            raise RuntimeError(
                "tabbar smoke: timed out waiting for pre-input title geometry "
                f"to settle after {polls} polls; signature={last_signature!r}"
            )
        sleep(min(poll_seconds, timeout_seconds - (now - started)))


def run_tabbar(kettle: str, root: Path) -> Path:
    out = root / f"tabbar-click-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "tab-bar-position = top",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 220",
                "window-height = 30",
            ]
        )
        + "\n"
    )
    with LiveKettle(kettle, cfg, out / "kettle.log") as live:
        for i in (1, 2):
            geo = live.json_ctl("ui_geometry")
            (out / f"geometry-plus-{i}.json").write_text(json.dumps(geo, indent=2) + "\n")
            x, y = rect_center(geo["tab_bar"]["new_tab"])  # type: ignore[index]
            live.ctl("send_mouse", params={"event": "click", "x": x, "y": y, "button": "left"})
            time.sleep(0.2)

        tabs = live.json_ctl("list_tabs")
        (out / "tabs-created.json").write_text(json.dumps(tabs, indent=2) + "\n")
        if len(tabs.get("tabs", [])) < 3:
            raise SystemExit("tabbar smoke: expected at least 3 tabs")

        before = wait_for_stable_tab_bar(
            lambda: live.json_ctl("ui_geometry")
        )
        (out / "geometry-before-press.json").write_text(json.dumps(before, indent=2) + "\n")
        live.screenshot(out / "before-press.png")
        layout_error = tab_segment_layout_error(before["tab_bar"])  # type: ignore[arg-type,index]
        if layout_error is not None:
            raise SystemExit(
                f"tabbar smoke: invalid homogeneous segment layout: {layout_error}"
            )
        seg = before["tab_bar"]["segments"][1]["rect"]  # type: ignore[index]
        x, y = rect_center(seg)
        live.ctl("send_mouse", params={"event": "press", "x": x, "y": y, "button": "left"})
        time.sleep(0.1)
        pressed = live.json_ctl("ui_geometry")
        (out / "geometry-pressed.json").write_text(json.dumps(pressed, indent=2) + "\n")
        live.screenshot(out / "pressed.png")
        cx, cy = pressed["cursor"]  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "move", "x": float(cx) + 6.0, "y": cy})
        time.sleep(0.1)
        jittered = live.json_ctl("ui_geometry")
        (out / "geometry-jittered.json").write_text(json.dumps(jittered, indent=2) + "\n")
        live.screenshot(out / "jittered.png")
        cx, cy = jittered["cursor"]  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "release", "x": cx, "y": cy, "button": "left"})
        time.sleep(0.1)
        released = live.json_ctl("ui_geometry")
        (out / "geometry-released.json").write_text(json.dumps(released, indent=2) + "\n")
        live.screenshot(out / "released.png")

    before_active = [s for s in before["tab_bar"]["segments"] if s.get("active")]  # type: ignore[index]
    if len(before_active) != 1:
        raise SystemExit(f"tabbar smoke: expected one resting active tab, got {before_active}")
    before_active = before_active[0]
    resting_full = before_active["rect"]

    if not pressed.get("tab_drag_active") or not pressed.get("tab_drag_armed"):
        raise SystemExit("tabbar smoke: press did not remain click-armed")
    if pressed.get("tab_drag_visible"):
        raise SystemExit("tabbar smoke: drag ghost became visible during a plain click")
    if not jittered.get("tab_drag_active") or not jittered.get("tab_drag_armed"):
        raise SystemExit("tabbar smoke: small tab-click jitter promoted to drag")
    if jittered.get("tab_drag_visible"):
        raise SystemExit("tabbar smoke: drag ghost became visible during small click jitter")
    active = [s for s in pressed["tab_bar"]["segments"] if s.get("active")]  # type: ignore[index]
    if len(active) != 1:
        raise SystemExit(f"tabbar smoke: expected one active tab after press, got {active}")
    if released.get("tab_drag_active") or released.get("tab_drag_armed") or released.get("tab_drag_visible"):
        raise SystemExit("tabbar smoke: release left drag state latched")

    before_rect, before_idx = active_rect(before)
    pressed_active_rect, pressed_idx = active_rect(pressed)
    pressed_active = [s for s in pressed["tab_bar"]["segments"] if s.get("active")][0]  # type: ignore[index]
    pressed_close = pressed_active["close"]
    bar = pressed["tab_bar"]  # type: ignore[index]
    y0 = float(bar["y"])
    y1 = y0 + float(bar["height"])
    changed = changed_pixels(out / "before-press.png", out / "pressed.png", y0, y1)
    outside = [
        (x, y)
        for x, y in changed
        if not (
            rect_contains(before_rect, x, y)
            or rect_contains(pressed_active_rect, x, y)
            or rect_contains(pressed_close, x, y)
        )
    ]
    if outside:
        xs = [x for x, _ in outside]
        ys = [y for _, y in outside]
        raise SystemExit(
            "tabbar smoke: press changed pixels outside old/new active tab rects: "
            f"outside={len(outside)} bbox=({min(xs)},{min(ys)},{max(xs)+1},{max(ys)+1})"
        )

    chrome_rects = [s["rect"] for s in bar["segments"]]  # type: ignore[index]
    chrome_rects.append(bar["new_tab"])
    if bar["new_tab_menu"]["width"] > 0:
        chrome_rects.append(bar["new_tab_menu"])
    release_changed = changed_pixels(out / "pressed.png", out / "released.png", y0, y1)
    release_outside = [
        (x, y)
        for x, y in release_changed
        if not any(rect_contains(rect, x, y) for rect in chrome_rects)
    ]
    if release_outside:
        raise SystemExit("tabbar smoke: release changed pixels outside tab chrome")

    analysis = {
        "before_active": before_idx,
        "pressed_active": pressed_idx,
        "before_active_rect": before_rect,
        "before_active_hit_rect": resting_full,
        "pressed_active_rect": pressed_active_rect,
        "press_changed_pixels": len(changed),
        "press_outside_allowed_rects": len(outside),
        "release_changed_pixels": len(release_changed),
        "release_outside_chrome_pixels": len(release_outside),
    }
    (out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    return out


def make_git_fixture(repo: Path) -> None:
    require_cmd("git")
    require_cmd("delta")
    require_cmd("less")
    repo.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.email", "kettle-smoke@example.invalid"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "Kettle Smoke"], cwd=repo, check=True)
    base = "".join(f"stable line {i:03d}\n" for i in range(1, 181))
    (repo / "fixture.txt").write_text(base)
    probe = repo / "crates" / "kettle-ui" / "src" / "app.rs"
    probe.parent.mkdir(parents=True, exist_ok=True)
    probe.write_text("// local-path underline fixture\n")
    subprocess.run(["git", "add", "fixture.txt", "crates/kettle-ui/src/app.rs"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "base"], cwd=repo, check=True)
    changed = []
    for i in range(1, 181):
        if i % 3 == 0:
            changed.append(f"changed underlined token_{i:03d} and link https://example.invalid/{i:03d}\n")
        else:
            changed.append(f"stable line {i:03d}\n")
    (repo / "fixture.txt").write_text("".join(changed))


def make_svn_fixture(checkout: Path) -> bool:
    if shutil.which("svn") is None or shutil.which("svnadmin") is None:
        return False
    repo = checkout.parent / "svnrepo"
    subprocess.run(["svnadmin", "create", str(repo)], check=True)
    subprocess.run(["svn", "checkout", repo.resolve().as_uri(), str(checkout)], check=True, stdout=subprocess.DEVNULL)
    base = "".join(f"stable svn line {i:03d}\n" for i in range(1, 181))
    (checkout / "fixture.txt").write_text(base)
    subprocess.run(["svn", "add", "fixture.txt"], cwd=checkout, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(
        ["svn", "commit", "-m", "base"],
        cwd=checkout,
        check=True,
        stdout=subprocess.DEVNULL,
    )
    changed = []
    for i in range(1, 181):
        if i % 4 == 0:
            changed.append(f"svn changed underlined token_{i:03d} and link https://example.invalid/svn/{i:03d}\n")
        else:
            changed.append(f"stable svn line {i:03d}\n")
    (checkout / "fixture.txt").write_text("".join(changed))
    return True


def underline_command(repo: Path, svn_checkout: Optional[Path]) -> str:
    if platform.system() == "Windows":
        repo_s = str(repo).replace("'", "''")
        svn_marker = ""
        svn_diff_part = ""
        if svn_checkout is not None:
            svn_s = str(svn_checkout).replace("'", "''")
            svn_marker = "Write-Output ('SVN_DELTA_' + 'FIXTURE_BEGIN'); "
            svn_diff_part = (
                f"Set-Location -LiteralPath '{svn_s}'; "
                "svn diff | delta --paging=never --line-numbers; "
                f"Set-Location -LiteralPath '{repo_s}'; "
            )
        return (
            f"Set-Location -LiteralPath '{repo_s}'; "
            "$esc=[char]27; "
            "& { Write-Output ('GIT_DELTA_' + 'FIXTURE_BEGIN'); "
            f"{svn_marker}"
            "1..120 | ForEach-Object { "
            "if ($_ % 2 -eq 1) { '{0}[4mUNDERLINE_{2}_{1:D3}{0}[24m link https://example.invalid/{1:D3}' -f $esc,$_,'SENTINEL' } "
            "else { 'PLAIN_{1}_{0:D3} link https://example.invalid/{0:D3}' -f $_,'SENTINEL' }; "
            "'PATH_POSIX_SENTINEL_{0:D3} crates/kettle-ui/src/app.rs:{0}:1' -f $_; "
            r"'PATH_WIN_SENTINEL_{0:D3} C:\src\kettle\crates\kettle-ui\src\app.rs:{0}:1' -f $_ }; "
            "git diff --color=always | delta --paging=never --line-numbers; "
            f"{svn_diff_part}"
            "} | less -R"
        )
    repo_s = str(repo).replace("'", "'\"'\"'")
    svn_marker = ""
    svn_diff_part = ""
    if svn_checkout is not None:
        svn_s = str(svn_checkout).replace("'", "'\"'\"'")
        svn_marker = "printf '%s%s\\n' 'SVN_DELTA_' 'FIXTURE_BEGIN'; "
        svn_diff_part = f"( cd '{svn_s}' && svn diff | delta --paging=never --line-numbers ); "
    return (
        f"cd '{repo_s}' && {{ printf '%s%s\\n' 'GIT_DELTA_' 'FIXTURE_BEGIN'; "
        f"{svn_marker}"
        "for i in $(seq 1 120); do "
        "if [ $((i % 2)) -eq 1 ]; then "
        "printf '\\033[4mUNDERLINE_%s_%03d\\033[24m link https://example.invalid/%03d\\n' SENTINEL \"$i\" \"$i\"; "
        "else printf 'PLAIN_%s_%03d link https://example.invalid/%03d\\n' SENTINEL \"$i\" \"$i\"; fi; "
        "printf 'PATH_POSIX_SENTINEL_%03d crates/kettle-ui/src/app.rs:%d:1\\n' \"$i\" \"$i\"; "
        "printf 'PATH_WIN_SENTINEL_%03d %s:%d:1\\n' \"$i\" 'C:\\src\\kettle\\crates\\kettle-ui\\src\\app.rs' \"$i\"; "
        "done; git diff --color=always | delta --paging=never --line-numbers; "
        f"{svn_diff_part}"
        "} | less -R"
    )


def run_underline(kettle: str, root: Path) -> Path:
    out = root / f"underline-scroll-{time.strftime('%Y%m%d-%H%M%S')}"
    repo = out / "repo"
    make_git_fixture(repo)
    svn_checkout = out / "svn-checkout"
    svn_enabled = make_svn_fixture(svn_checkout)
    svn_fixture = svn_checkout if svn_enabled else None
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = off",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #080808",
                "foreground = #f8f8f8",
                "minimum-contrast = 0",
                "window-padding-x = 0",
                "window-padding-y = 0",
                "window-width = 100",
                "window-height = 32",
            ]
        )
        + "\n"
    )
    extra_args = ["-d", str(repo)]
    if platform.system() == "Windows":
        extra_args.extend(["-e", "powershell.exe", "-NoLogo", "-NoProfile"])
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live.ctl("send_text", params={"text": underline_command(repo, svn_fixture)})
        live.ctl("send_keys", params={"keys": ["enter"]})
        live.ctl("wait_for", params={"text": "GIT_DELTA_FIXTURE_BEGIN", "timeout_ms": 8000, "quiet_ms": 250})
        if svn_enabled:
            live.ctl("wait_for", params={"text": "SVN_DELTA_FIXTURE_BEGIN", "timeout_ms": 8000, "quiet_ms": 250})
        live.ctl("wait_for", params={"text": "UNDERLINE_SENTINEL", "timeout_ms": 8000, "quiet_ms": 250})
        for i in range(1, 9):
            geo = live.json_ctl("ui_geometry")
            (out / f"geometry-{i}.json").write_text(json.dumps(geo, indent=2) + "\n")
            cells = live.read_cells()
            (out / f"cells-{i}.json").write_text(json.dumps(cells))
            live.screenshot(out / f"frame-{i}.png")
            if i < 8:
                keys = ["j"] * 6 if i < 5 else ["k"] * 6
                live.ctl("send_keys", params={"keys": keys}, timeout=6)
            time.sleep(0.08)

    top_sentinels: List[int] = []
    underline_frames = 0
    path_overlay_frames = 0
    analysis: List[Dict[str, object]] = []
    for i in range(1, 9):
        data = json.loads((out / f"cells-{i}.json").read_text())
        cols = max(1, int(data.get("cols", 1)))
        rows_n = max(1, int(data.get("rows", 1)))
        rows: Dict[int, List[Tuple[int, str]]] = {}
        underline_rows: Set[int] = set()
        underline_cols: Dict[int, List[int]] = {}
        for c in data.get("cells", []):
            row = int(c["row"])
            col = int(c["col"])
            rows.setdefault(row, []).append((col, c.get("ch", "")))
            if c.get("any_underline"):
                underline_rows.add(row)
                underline_cols.setdefault(row, []).append(col)
        if underline_rows:
            underline_frames += 1
        found: List[Tuple[int, int]] = []
        plain_found: List[Tuple[int, int]] = []
        path_found: List[Tuple[int, str, int, int]] = []
        text_by_row: Dict[int, str] = {}
        for row, row_cells in sorted(rows.items()):
            text = "".join(ch for _, ch in sorted(row_cells))
            text_by_row[row] = text
            underline = re.search(r"\bUNDERLINE_SENTINEL_(\d{3})\b", text)
            if underline:
                num = int(underline.group(1))
                found.append((row, num))
            plain = re.search(r"\bPLAIN_SENTINEL_(\d{3})\b", text)
            if plain:
                num = int(plain.group(1))
                plain_found.append((row, num))
            for marker, probe in (
                ("PATH_POSIX_SENTINEL_", "crates/kettle-ui/src/app.rs"),
                ("PATH_WIN_SENTINEL_", r"C:\src\kettle\crates\kettle-ui\src\app.rs"),
            ):
                rendered = re.search(
                    rf"\b{re.escape(marker)}\d{{3}}\s+({re.escape(probe)}:\d+:1)\b",
                    text,
                )
                if rendered:
                    start, end = rendered.span(1)
                    path_found.append((row, marker.rstrip("_"), start, end - 1))
        if not found:
            raise SystemExit(f"underline smoke: no sentinel text visible in cells-{i}.json")
        if not path_found:
            raise SystemExit(f"underline smoke: no path sentinel text visible in cells-{i}.json")
        top_sentinels.append(found[0][1])
        width, height, rgba_rows = read_rgba_png(out / f"frame-{i}.png")
        geo = json.loads((out / f"geometry-{i}.json").read_text())
        content = geo.get("content", {})
        cell = geo.get("cell", {})
        origin_x = float(content.get("x", 0.0))
        origin_y = float(content.get("y", 0.0))
        cell_w = float(cell.get("width") or (float(content.get("width", width)) / cols))
        cell_h = float(cell.get("height") or (float(content.get("height", height)) / rows_n))
        pixel_rows = []
        for row, number in found[:8]:
            sample_cols = sorted(underline_cols.get(row, []))[:22]
            if not sample_cols:
                raise SystemExit(f"underline smoke: row {row} has no underline attrs")
            baseline = int(round(origin_y + row * cell_h + cell_h - 2.0))
            best = 0
            best_y = baseline
            for y in range(baseline - 2, baseline + 3):
                hits = sum(1 for col in sample_cols if bright_at(rgba_rows, int(origin_x + (col + 0.5) * cell_w), y))
                if hits > best:
                    best = hits
                    best_y = y
            if best < max(8, int(len(sample_cols) * 0.60)):
                raise SystemExit(f"underline smoke: rendered underline not aligned on frame {i} row {row}")
            pixel_rows.append({"row": row, "sentinel": number, "underline_pixel_hits": best, "sampled_columns": len(sample_cols), "pixel_y": best_y})
        plain_pixel_rows = []
        for row, number in plain_found[:8]:
            sample_cols = list(range(0, 18))
            baseline = int(round(origin_y + row * cell_h + cell_h - 2.0))
            best = 0
            best_y = baseline
            for y in range(baseline - 2, baseline + 3):
                hits = sum(1 for col in sample_cols if bright_at(rgba_rows, int(origin_x + (col + 0.5) * cell_w), y))
                if hits > best:
                    best = hits
                    best_y = y
            max_plain_hits = max(16, int(len(sample_cols) * 0.90))
            if best > max_plain_hits:
                raise SystemExit(f"underline smoke: underline leaked onto plain row on frame {i} row {row}")
            plain_pixel_rows.append({"row": row, "sentinel": number, "baseline_pixel_hits": best, "sampled_columns": len(sample_cols), "pixel_y": best_y, "near_solid_threshold": max_plain_hits})
        path_pixel_rows = []
        for row, marker, start_col, end_col in path_found[:10]:
            sample_cols = list(range(start_col, min(end_col + 1, start_col + 36)))
            baseline = int(round(origin_y + row * cell_h + cell_h - 2.0))
            best = 0
            best_y = baseline
            for y in range(baseline - 2, baseline + 3):
                hits = sum(1 for col in sample_cols if bright_at(rgba_rows, int(origin_x + (col + 0.5) * cell_w), y))
                if hits > best:
                    best = hits
                    best_y = y
            threshold = max(10, int(len(sample_cols) * 0.65))
            if best < threshold:
                raise SystemExit(
                    f"underline smoke: path underline not aligned on frame {i} row {row} "
                    f"{marker} hits={best}/{len(sample_cols)} threshold={threshold}"
                )
            path_overlay_frames += 1
            leak_checks = []
            for neighbor in (row - 1, row + 1):
                if neighbor < 0 or neighbor >= rows_n:
                    continue
                neighbor_text = text_by_row.get(neighbor, "")
                if (
                    "PATH_POSIX_SENTINEL_" in neighbor_text
                    or "PATH_WIN_SENTINEL_" in neighbor_text
                    or "http://" in neighbor_text
                    or "https://" in neighbor_text
                    or neighbor in underline_rows
                ):
                    continue
                neighbor_baseline = int(round(origin_y + neighbor * cell_h + cell_h - 2.0))
                neighbor_best = 0
                neighbor_y = neighbor_baseline
                for y in range(neighbor_baseline - 2, neighbor_baseline + 3):
                    hits = sum(1 for col in sample_cols if bright_at(rgba_rows, int(origin_x + (col + 0.5) * cell_w), y))
                    if hits > neighbor_best:
                        neighbor_best = hits
                        neighbor_y = y
                leak_threshold = max(10, int(len(sample_cols) * 0.65))
                if neighbor_best >= leak_threshold:
                    raise SystemExit(
                        f"underline smoke: path underline leaked to adjacent row on frame {i} "
                        f"row {row}->{neighbor} hits={neighbor_best}/{len(sample_cols)}"
                    )
                leak_checks.append({"row": neighbor, "hits": neighbor_best, "pixel_y": neighbor_y, "threshold": leak_threshold})
            path_pixel_rows.append({
                "row": row,
                "marker": marker,
                "start_col": start_col,
                "end_col": end_col,
                "underline_pixel_hits": best,
                "sampled_columns": len(sample_cols),
                "pixel_y": best_y,
                "threshold": threshold,
                "adjacent_rows": leak_checks,
            })
        analysis.append({"frame": i, "top_sentinel": found[0][1], "cell": {"width": cell_w, "height": cell_h}, "content": content, "underline_rows": sorted(underline_rows), "sentinels": [{"row": r, "number": n} for r, n in found], "plain_sentinels": [{"row": r, "number": n} for r, n in plain_found], "path_sentinels": [{"row": r, "marker": m, "start_col": s, "end_col": e} for r, m, s, e in path_found], "pixel_rows": pixel_rows, "plain_pixel_rows": plain_pixel_rows, "path_pixel_rows": path_pixel_rows})
    if underline_frames == 0:
        raise SystemExit("underline smoke: no underlined cells observed")
    if path_overlay_frames == 0:
        raise SystemExit("underline smoke: no autodetected path underlines observed")
    if not (top_sentinels[0] < top_sentinels[4] and top_sentinels[-1] < top_sentinels[4]):
        raise SystemExit(f"underline smoke: down/up scroll sequence failed: {top_sentinels}")
    (out / "analysis.json").write_text(json.dumps({"frames": 8, "underline_frames": underline_frames, "path_overlay_frames": path_overlay_frames, "top_sentinels": top_sentinels, "delta_fixtures": {"git": True, "svn": svn_enabled}, "frames_detail": analysis}, indent=2) + "\n")
    return out


def live_state_screenshot_path(out: Path, label: str) -> Path:
    return out / f"{label}.png"


def live_transition_screenshot_path(out: Path, label: str) -> Path:
    return out / f"{label}-transition.png"


def capture_live_state(
    live: LiveKettle,
    out: Path,
    label: str,
    *,
    cells: Optional[Dict[str, object]] = None,
    screen: Optional[Dict[str, object]] = None,
) -> Dict[str, object]:
    cells = cells if cells is not None else live.read_cells()
    (out / f"{label}.cells.json").write_text(json.dumps(cells, indent=2) + "\n")
    screen = screen if screen is not None else live.json_ctl("read_screen")
    (out / f"{label}.screen.json").write_text(json.dumps(screen, indent=2) + "\n")
    shot = live_state_screenshot_path(out, label)
    live.screenshot(shot)

    width, height, rgba_rows = read_rgba_png(shot)
    non_space = 0
    for cell in cells.get("cells", []):
        if str(cell.get("ch", "")).strip():
            non_space += 1
    bright = bright_pixel_count(rgba_rows)
    if non_space < 12:
        raise SystemExit(f"agent-tui smoke: {label} has too few non-space cells ({non_space})")
    if bright < 250:
        raise SystemExit(f"agent-tui smoke: {label} screenshot looks blank ({bright} bright pixels)")
    return {
        "label": label,
        "screenshot": str(shot),
        "width": width,
        "height": height,
        "non_space_cells": non_space,
        "bright_pixels": bright,
    }


def screen_text(screen: Dict[str, object]) -> str:
    return str(screen.get("text", screen.get("screen", "")))


def wrapped_literal_pattern(value: str) -> str:
    """A literal token with terminal line-wrap whitespace allowed anywhere."""
    return r"\s*".join(re.escape(character) for character in value)


MANAGED_PASTE_PATH_RE = re.compile(
    r"(?is)(?:[a-z]\s*:\s*[\\/]\s*|/)"
    r"(?:[^'\"\r\n]|[\r\n]\s*)*?"
    + wrapped_literal_pattern("kettle-paste-")
    + r"\s*\d(?:\s*\d)*\s*-\s*\d(?:\s*\d)*"
    + r"\s*[\\/]\s*\d(?:\s*\d){3}\s*\.\s*p\s*n\s*g"
)


def redact_managed_paste_paths(value: object) -> object:
    """Remove private bitmap paths before a live screen enters diagnostics."""
    if isinstance(value, str):
        return MANAGED_PASTE_PATH_RE.sub("<managed-image-path>", value)
    if isinstance(value, list):
        return [redact_managed_paste_paths(item) for item in value]
    if isinstance(value, dict):
        return {key: redact_managed_paste_paths(item) for key, item in value.items()}
    return value


def contains_managed_paste_marker(value: object) -> bool:
    """Find the managed marker in raw strings before JSON escapes wraps."""
    if isinstance(value, str):
        return "kettle-paste-" in re.sub(r"\s+", "", value)
    if isinstance(value, list):
        return any(contains_managed_paste_marker(item) for item in value)
    if isinstance(value, dict):
        return any(contains_managed_paste_marker(item) for item in value.values())
    return False


def shell_clear_line_keys(system: str) -> List[str]:
    """Portable default binding for clearing an unsubmitted shell line."""
    return ["escape"] if system == "Windows" else ["ctrl+u"]


def write_image_receipt_fixture(path: Path, width: int = 640, height: int = 360) -> None:
    """Write a dependency-free RGBA PNG with visually distinct regions."""

    def chunk(kind: bytes, payload: bytes) -> bytes:
        body = kind + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    rows = bytearray()
    colors = ((137, 180, 250, 255), (166, 227, 161, 255), (245, 194, 231, 255))
    for y in range(height):
        rows.append(0)
        for x in range(width):
            band = min(2, x * 3 // width)
            r, g, b, a = colors[band]
            if (x // 32 + y // 32) % 2:
                r, g, b = max(0, r - 28), max(0, g - 28), max(0, b - 28)
            rows.extend((r, g, b, a))
    data = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(rows), level=6))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(data)


def write_linux_video_thumbnail_cache(video: Path, cache: Path) -> None:
    """Seed the standard thumbnail cache without adding a test dependency."""

    def chunk(kind: bytes, payload: bytes) -> bytes:
        body = kind + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    uri = video.resolve().as_uri()
    mtime = str(video.stat().st_mtime_ns // 1_000_000_000)
    digest = hashlib.md5(uri.encode("utf-8"), usedforsecurity=False).hexdigest()
    directory = cache / "thumbnails" / "xx-large"
    directory.mkdir(parents=True, mode=0o700)
    os.chmod(cache, 0o700)
    os.chmod(cache / "thumbnails", 0o700)
    os.chmod(directory, 0o700)

    width, height = 256, 144
    rows = bytearray()
    colors = ((137, 180, 250, 255), (166, 227, 161, 255), (245, 194, 231, 255))
    for y in range(height):
        rows.append(0)
        for x in range(width):
            r, g, b, a = colors[min(2, x * 3 // width)]
            if (x // 16 + y // 16) % 2:
                r, g, b = max(0, r - 28), max(0, g - 28), max(0, b - 28)
            rows.extend((r, g, b, a))
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"tEXt", b"Thumb::URI\x00" + uri.encode("latin-1"))
        + chunk(b"tEXt", b"Thumb::MTime\x00" + mtime.encode("ascii"))
        + chunk(b"IDAT", zlib.compress(bytes(rows), level=6))
        + chunk(b"IEND", b"")
    )
    target = directory / f"{digest}.png"
    target.write_bytes(png)
    os.chmod(target, 0o600)


def set_bitmap_clipboard(path: Path) -> Optional[subprocess.Popen]:
    """Replace the desktop clipboard with one PNG bitmap for an explicit smoke."""
    system = platform.system()
    if system == "Darwin":
        require_cmd("osascript")
        cp = run(
            [
                "osascript",
                "-e",
                "on run argv",
                "-e",
                "set imageFile to POSIX file (item 1 of argv)",
                "-e",
                "set the clipboard to (read imageFile as «class PNGf»)",
                "-e",
                "end run",
                str(path),
            ]
        )
    elif system == "Windows":
        env = os.environ.copy()
        env["KETTLE_RECEIPT_FIXTURE"] = str(path)
        cp = run(
            [
                "powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-STA",
                "-Command",
                (
                    "Add-Type -AssemblyName System.Drawing; "
                    "Add-Type -AssemblyName System.Windows.Forms; "
                    "$image=[System.Drawing.Image]::FromFile($env:KETTLE_RECEIPT_FIXTURE); "
                    "try {[System.Windows.Forms.Clipboard]::SetImage($image)} "
                    "finally {$image.Dispose()}"
                ),
            ],
            env=env,
        )
    elif os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-copy"):
        # Wayland clients own clipboard data and serve it on demand; no server
        # stores the bytes for them. Keep one provider alive until Kettle reads
        # the image, then --paste-once makes it exit. The caller retains the
        # process as a bounded cleanup fallback.
        source = path.open("rb")
        try:
            owner = subprocess.Popen(
                [
                    "wl-copy",
                    "--foreground",
                    "--paste-once",
                    "--type",
                    "image/png",
                ],
                stdin=source,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
        finally:
            source.close()
        time.sleep(0.1)
        if owner.poll() not in (None, 0):
            stderr = (owner.stderr.read() if owner.stderr else b"").decode(
                "utf-8", errors="replace"
            )
            raise SystemExit(
                "image-paste-receipt smoke: Wayland clipboard provider failed:\n"
                + stderr
            )
        return owner
    elif os.environ.get("DISPLAY") and shutil.which("xclip"):
        cp = run(
            [
                "xclip",
                "-selection",
                "clipboard",
                "-target",
                "image/png",
                "-in",
                str(path),
            ]
        )
    else:
        raise SystemExit(
            "image-paste-receipt smoke: no bitmap clipboard writer; install "
            "wl-clipboard on Wayland or xclip on X11"
        )
    if cp.returncode != 0:
        raise SystemExit(
            "image-paste-receipt smoke: could not set bitmap clipboard:\n"
            f"{cp.stderr}\n{cp.stdout}"
        )
    return None


def set_file_list_clipboard(paths: Sequence[Path]) -> Optional[subprocess.Popen]:
    """Replace the desktop clipboard with an explicit local file list."""
    if not paths:
        raise SystemExit("video-paste-receipt smoke: file list is empty")
    paths = [path.resolve() for path in paths]
    system = platform.system()
    if system == "Darwin":
        require_cmd("swift")
        # AppleScript's `set the clipboard to {POSIX file ...}` writes a
        # generic list flavor that file-list readers do not recognize. AppKit
        # NSURL pasteboard objects match Finder's file-copy contract.
        env = os.environ.copy()
        env["KETTLE_RECEIPT_FILES"] = json.dumps([str(path) for path in paths])
        cp = run(
            [
                "swift",
                "-e",
                (
                    'import AppKit; import Foundation; let data=ProcessInfo.processInfo.environment['
                    '"KETTLE_RECEIPT_FILES"]!.data(using:.utf8)!; '
                    "let paths=try! JSONSerialization.jsonObject(with:data) as! [String]; "
                    "let board=NSPasteboard.general; board.clearContents(); "
                    "let urls=paths.map { NSURL(fileURLWithPath:$0) }; "
                    "guard board.writeObjects(urls) else { exit(2) }"
                ),
            ],
            env=env,
        )
    elif system == "Windows":
        env = os.environ.copy()
        env["KETTLE_RECEIPT_FILES"] = json.dumps([str(path) for path in paths])
        cp = run(
            [
                "powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-STA",
                "-Command",
                (
                    "Add-Type -AssemblyName System.Windows.Forms; "
                    "$items=New-Object System.Collections.Specialized.StringCollection; "
                    "(ConvertFrom-Json $env:KETTLE_RECEIPT_FILES) | "
                    "ForEach-Object {[void]$items.Add([string]$_)}; "
                    "[System.Windows.Forms.Clipboard]::SetFileDropList($items)"
                ),
            ],
            env=env,
        )
    elif os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-copy"):
        owner = subprocess.Popen(
            [
                "wl-copy",
                "--foreground",
                "--paste-once",
                "--type",
                "text/uri-list",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        assert owner.stdin is not None
        owner.stdin.write("\n".join(path.as_uri() for path in paths).encode("utf-8"))
        owner.stdin.close()
        time.sleep(0.1)
        if owner.poll() not in (None, 0):
            stderr = (owner.stderr.read() if owner.stderr else b"").decode(
                "utf-8", errors="replace"
            )
            raise SystemExit(
                "video-paste-receipt smoke: Wayland clipboard provider failed:\n"
                + stderr
            )
        return owner
    elif os.environ.get("DISPLAY") and shutil.which("xclip"):
        owner = subprocess.Popen(
            ["xclip", "-selection", "clipboard", "-target", "text/uri-list", "-in"],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        assert owner.stdin is not None
        owner.stdin.write("\n".join(path.as_uri() for path in paths).encode("utf-8"))
        owner.stdin.close()
        time.sleep(0.1)
        if owner.poll() not in (None, 0):
            stderr = (owner.stderr.read() if owner.stderr else b"").decode(
                "utf-8", errors="replace"
            )
            raise SystemExit(
                "video-paste-receipt smoke: X11 clipboard provider failed:\n" + stderr
            )
        return owner
    else:
        raise SystemExit(
            "video-paste-receipt smoke: no file-list clipboard writer; install "
            "wl-clipboard on Wayland or xclip on X11"
        )
    if cp.returncode != 0:
        raise SystemExit(
            "video-paste-receipt smoke: could not set file-list clipboard:\n"
            f"{cp.stderr}\n{cp.stdout}"
        )
    return None


def wait_for_media_receipt(
    live: LiveKettle,
    *,
    kind: str,
    expanded: Optional[bool],
    preview_ready: Optional[bool] = None,
    timeout_s: float = 8.0,
) -> Tuple[Dict[str, object], Dict[str, object]]:
    deadline = time.monotonic() + timeout_s
    last: Dict[str, object] = {}
    while time.monotonic() < deadline:
        geometry = live.json_ctl("ui_geometry")
        value = geometry.get("media_paste_receipt")
        if (
            isinstance(value, dict)
            and value.get("kind") == kind
            and (expanded is None or value.get("expanded") is expanded)
            and (preview_ready is None or value.get("preview_ready") is preview_ready)
        ):
            return geometry, value
        last = geometry
        time.sleep(0.05)
    raise SystemExit(
        "media-paste-receipt smoke: timed out waiting for "
        f"kind={kind} expanded={expanded} preview_ready={preview_ready}: "
        f"{last.get('media_paste_receipt')!r}"
    )


def wait_for_image_receipt(
    live: LiveKettle, *, expanded: Optional[bool], timeout_s: float = 8.0
) -> Tuple[Dict[str, object], Dict[str, object]]:
    return wait_for_media_receipt(
        live, kind="image", expanded=expanded, timeout_s=timeout_s
    )


def live_shell_command(live: LiveKettle, command: str, marker: str, timeout_ms: int = 10000) -> None:
    live.ctl("send_text", params={"text": command})
    live.ctl("send_keys", params={"keys": ["enter"]})
    live.wait_for_text(marker, timeout_ms=timeout_ms, quiet_ms=250)


def focus_live_kettle_window(
    live: LiveKettle, desktop_point: Optional[Tuple[float, float]] = None
) -> None:
    """Ask the desktop to activate the exact Kettle process under test."""
    live.json_ctl("perform_action", {"action": "focus_window"})
    if platform.system() == "Darwin" and shutil.which("swift"):
        env = os.environ.copy()
        env["KETTLE_SMOKE_PID"] = str(live.pid)
        run(
            [
                "swift",
                "-e",
                (
                    "import AppKit; import Foundation; "
                    "let pid=pid_t(ProcessInfo.processInfo.environment[\"KETTLE_SMOKE_PID\"]!)!; "
                    "guard let app=NSRunningApplication(processIdentifier:pid) else { exit(2) }; "
                    "guard app.activate(options:.activateIgnoringOtherApps) else { exit(3) }"
                ),
            ],
            env=env,
        )
    if platform.system() == "Darwin" and shutil.which("osascript"):
        # AppKit can decline an activation request from a background CLI
        # process. System Events targets the same pid and mirrors a user click
        # without relying on an app bundle name.
        run(
            [
                "osascript",
                "-e",
                (
                    'tell application "System Events" to set frontmost of '
                    f"(first process whose unix id is {live.pid}) to true"
                ),
            ]
        )
    if platform.system() == "Darwin" and shutil.which("swift"):
        env = os.environ.copy()
        env["KETTLE_SMOKE_PID"] = str(live.pid)
        if desktop_point:
            env["KETTLE_SMOKE_CLICK_X"] = str(desktop_point[0])
            env["KETTLE_SMOKE_CLICK_Y"] = str(desktop_point[1])
        run(
            [
                "swift",
                "-e",
                (
                    "import CoreGraphics; import Foundation; "
                    "let env=ProcessInfo.processInfo.environment; let pid=Int32(env[\"KETTLE_SMOKE_PID\"]!)!; "
                    "let info=CGWindowListCopyWindowInfo([.optionOnScreenOnly,.excludeDesktopElements], "
                    "kCGNullWindowID) as! [[String:Any]]; "
                    "let owned=info.first { ($0[kCGWindowOwnerPID as String] as? Int32)==pid "
                    "&& ($0[kCGWindowLayer as String] as? Int)==0 }; "
                    "let rect=owned.flatMap { ($0[kCGWindowBounds as String] as? CFDictionary) "
                    ".flatMap { CGRect(dictionaryRepresentation:$0) } }; "
                    "let point=rect.map { CGPoint(x:$0.midX,y:$0.midY) } ?? "
                    "CGPoint(x:Double(env[\"KETTLE_SMOKE_CLICK_X\"] ?? \"120\")!, "
                    "y:Double(env[\"KETTLE_SMOKE_CLICK_Y\"] ?? \"120\")!); "
                    "CGEvent(mouseEventSource:nil, mouseType:.leftMouseDown, "
                    "mouseCursorPosition:point, mouseButton:.left)?.post(tap:.cghidEventTap); "
                    "CGEvent(mouseEventSource:nil, mouseType:.leftMouseUp, "
                    "mouseCursorPosition:point, mouseButton:.left)?.post(tap:.cghidEventTap)"
                ),
            ],
            env=env,
        )


def wait_for_search_result(
    live: LiveKettle,
    expected_text: str,
    *,
    timeout_s: float = 12.0,
) -> Tuple[Dict[str, object], Dict[str, object]]:
    """Wait for Search to focus and reveal one exact scrollback fixture row."""
    deadline = time.monotonic() + timeout_s
    last_geometry: Dict[str, object] = {}
    last_screen: Dict[str, object] = {}
    while time.monotonic() < deadline:
        last_geometry = live.json_ctl("ui_geometry")
        last_screen = live.json_ctl("read_screen")
        search = last_geometry.get("search")
        if (
            isinstance(search, dict)
            and search.get("has_match") is True
            and search.get("status") in {"Match", "Wrapped"}
            and expected_text in screen_text(last_screen)
        ):
            return last_geometry, last_screen
        time.sleep(0.05)
    raise SystemExit(
        "search-history smoke: timed out waiting for a focused historical match: "
        f"expected={expected_text!r} search={last_geometry.get('search')} "
        f"display_offset={last_screen.get('display_offset')} "
        f"screen={screen_text(last_screen)!r}"
    )


def wait_for_search_no_match(
    live: LiveKettle,
    *,
    timeout_s: float = 12.0,
) -> Tuple[Dict[str, object], Dict[str, object]]:
    """Wait for a settled no-match result without changing Search layout."""
    deadline = time.monotonic() + timeout_s
    last_geometry: Dict[str, object] = {}
    last_screen: Dict[str, object] = {}
    while time.monotonic() < deadline:
        last_geometry = live.json_ctl("ui_geometry")
        last_screen = live.json_ctl("read_screen")
        search = last_geometry.get("search")
        if (
            isinstance(search, dict)
            and search.get("has_match") is False
            and search.get("status") == "No match"
        ):
            return last_geometry, last_screen
        time.sleep(0.05)
    raise SystemExit(
        "search-history smoke: timed out waiting for a settled no-match state: "
        f"search={last_geometry.get('search')} screen={screen_text(last_screen)!r}"
    )


def command_with_marker(
    command: str, marker: str, *, windows: Optional[bool] = None
) -> str:
    split = max(1, len(marker) // 2)
    left = marker[:split]
    right = marker[split:]
    use_windows = platform.system() == "Windows" if windows is None else windows
    if use_windows:
        return (
            f"{command}; Write-Output "
            f"({shell_quote(left, windows=True)} + {shell_quote(right, windows=True)})"
        )
    return (
        f"{command}; printf '%s\\n' "
        f"{shell_quote(left, windows=False)}{shell_quote(right, windows=False)}"
    )


def first_lines_command(
    command: str, lines: int = 22, *, windows: Optional[bool] = None
) -> str:
    use_windows = platform.system() == "Windows" if windows is None else windows
    if use_windows:
        return f"{command} | Select-Object -First {lines}"
    return f"{command} | sed -n '1,{lines}p'"


def prompt_marker_command(
    marker: str, *, windows: Optional[bool] = None
) -> str:
    split = max(1, len(marker) // 2)
    left = marker[:split]
    right = marker[split:]
    use_windows = platform.system() == "Windows" if windows is None else windows
    if use_windows:
        return (
            "$arrow=[char]0x279c; "
            "Write-Output ($arrow + '  ~ ' + "
            f"({shell_quote(left, windows=True)} + {shell_quote(right, windows=True)}))"
        )
    return (
        "printf '\\342\\236\\234  ~ %s\\n' "
        f"{shell_quote(left, windows=False)}{shell_quote(right, windows=False)}"
    )


def codex_cursor_fixture_command(
    *, queued_input: bool, windows: Optional[bool] = None
) -> Tuple[str, int, int]:
    row = 6
    text = "queued work" if queued_input else "Explain this codebase"
    col = 3 + len(text) if queued_input else 3
    use_windows = platform.system() == "Windows" if windows is None else windows
    if use_windows:
        style = "" if queued_input else "$esc + '[2m' + "
        reset = "" if queued_input else "+ $esc + '[22m'"
        command = (
            "chcp.com 65001 > $null; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); "
            "$esc=[char]27; $bullet=[char]0x2022; $chevron=[char]0x203a; $dot=[char]0xb7; "
            "$frame=$esc + '[2J' + $esc + '[HOpenAI Codex (v0.144.0)' + "
            "$esc + '[3;2H' + $bullet + ' Working (2s ' + $bullet + ' esc to interrupt)' + "
            "$esc + '[6;1H' + $esc + '[1m' + $chevron + $esc + '[22m ' + "
            f"{style}'{text}'{reset} + "
            "$esc + '[8;3Hgpt-5.5 high ' + $dot + ' ~' + "
            f"$esc + '[{row};{col}H' + $esc + '[?25h'; "
            "[Console]::Write($frame); "
            "Start-Sleep -Seconds 20"
        )
    else:
        style = "" if queued_input else "\\033[2m"
        reset = "" if queued_input else "\\033[22m"
        command = (
            "printf '\\033[2J\\033[HOpenAI Codex (v0.144.0)"
            "\\033[3;2H• Working (2s • esc to interrupt)"
            f"\\033[6;1H\\033[1m›\\033[22m {style}{text}{reset}"
            "\\033[8;3Hgpt-5.5 high · ~"
            f"\\033[{row};{col}H\\033[?25h'; sleep 20"
        )
    return command, row - 1, col - 1


def notification_command(title: str, body: str, marker: str) -> str:
    marker_left, marker_right = split_marker(marker)
    if platform.system() == "Windows":
        return (
            "$esc=[char]27; $bel=[char]7; "
            f"[Console]::Write($esc + ']777;notify;' + {shell_quote(title)} + ';' + {shell_quote(body)} + $bel); "
            "Write-Output ("
            f"{shell_quote(marker_left, windows=True)} + "
            f"{shell_quote(marker_right, windows=True)})"
        )
    return (
        "printf '\\033]777;notify;%s;%s\\007' "
        f"{shell_quote(title)} {shell_quote(body)}; "
        "printf '%s%s\\n' "
        f"{shell_quote(marker_left)} {shell_quote(marker_right)}"
    )


def cwd_title_command(
    expected_path: str,
    title: str,
    marker: str,
    *,
    sleep_seconds: int = 5,
    windows: Optional[bool] = None,
) -> str:
    """Build a command that `cd`s into `expected_path`, reports it as the
    pane's cwd, sets `title` via OSC 2, emits `marker`, then sleeps so the
    reported cwd/title stay put while the smoke polls
    `list_panes`/`ui_geometry`.

    Windows: `Set-Location` into the fixture dir, then report `$PWD.Path`
    (native, backslash-separated, exactly what `Set-Location` resolved to)
    via OSC 9;9 — the ConEmu/Windows Terminal "set working directory"
    convention kettle's VT engine takes VERBATIM, no `file://` URI encoding
    or separator translation (see `kettle-vt::extract::parse_osc9_9`). That
    keeps the reported cwd byte-identical to `expected_path` (also
    native-backslash), which matters because `abbreviate_home` does a
    literal `$HOME`-prefix string match. Mirrors `notification_command`'s
    `[Console]::Write` shape.
    POSIX: `cd` then `printf`, reporting via OSC 7 (`file://` URI) and
    substituting the shell's own `$PWD`, as the pre-existing Unix-only
    smokes already did.

    `windows` follows `agent_auth_command`'s override pattern: `None` (the
    default at every call site) defers to the real host, while an explicit
    `True`/`False` lets `live_helper_selftest` exercise both command shapes
    from a single host OS.
    """
    use_windows = platform.system() == "Windows" if windows is None else windows
    marker_left, marker_right = split_marker(marker)
    if use_windows:
        return (
            f"Set-Location {shell_quote(expected_path)}; "
            "$esc=[char]27; $bel=[char]7; "
            "[Console]::Write($esc + ']9;9;\"' + $PWD.Path + '\"' + $bel + $esc + ']2;' + "
            f"{shell_quote(title)} + $bel); "
            "Write-Output ("
            f"{shell_quote(marker_left, windows=True)} + "
            f"{shell_quote(marker_right, windows=True)}); "
            f"Start-Sleep -Seconds {sleep_seconds}"
        )
    return (
        f"cd {shell_quote(expected_path)}; "
        f"printf '\\033]7;file://localhost%s\\007\\033]2;{title}\\007"
        "%s%s\\n' \"$PWD\" "
        f"{shell_quote(marker_left)} {shell_quote(marker_right)}; "
        f"sleep {sleep_seconds}"
    )


def env_flag(name: str) -> bool:
    value = os.environ.get(name, "").strip().lower()
    return value not in ("", "0", "false", "no", "off")


def env_strict(name: str) -> bool:
    return os.environ.get(name, "").strip().lower() in ("required", "strict", "fail")


def split_marker(marker: str) -> Tuple[str, str]:
    midpoint = max(1, len(marker) // 2)
    return marker[:midpoint], marker[midpoint:]


def agent_auth_command(
    tool: str,
    marker: str,
    output_marker: str,
    done_marker: str,
    *,
    windows: Optional[bool] = None,
) -> str:
    prompt = f"Reply exactly {marker} and nothing else."
    if tool == "codex":
        argv = [
            "codex",
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--color",
            "never",
            prompt,
        ]
    elif tool == "claude":
        argv = [
            "claude",
            "--print",
            "--output-format",
            "text",
            "--max-budget-usd",
            "0.25",
            prompt,
        ]
    else:
        raise ValueError(f"unsupported agent auth tool: {tool}")

    use_windows = platform.system() == "Windows" if windows is None else windows
    output_left, output_right = split_marker(output_marker)
    if use_windows:
        ps_argv = " ".join(shell_quote(part, windows=True) for part in argv)
        done = shell_quote(done_marker, windows=True)
        return (
            "$tmp=[System.IO.Path]::GetTempFileName(); $rc=125; "
            "try { "
            "$LASTEXITCODE=$null; "
            f"& {ps_argv} *> $tmp; "
            "$rc=if ($null -eq $LASTEXITCODE) { 125 } else { [int]$LASTEXITCODE }; "
            "} catch { "
            "$rc=125; $_ | Out-File -LiteralPath $tmp -Append -Encoding utf8; "
            "}; "
            "Write-Output "
            f"({shell_quote(output_left, windows=True)} + "
            f"{shell_quote(output_right, windows=True)}); "
            "Get-Content -LiteralPath $tmp; "
            "Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue; "
            f"Write-Output ({done} + ':' + $rc)"
        )
    sh_argv = " ".join(shlex.quote(part) for part in argv)
    return (
        "tmp=$(mktemp); "
        f"{sh_argv} >\"$tmp\" 2>&1; "
        "rc=$?; "
        f"printf '\\n%s%s\\n' {shlex.quote(output_left)} {shlex.quote(output_right)}; "
        "cat \"$tmp\"; "
        "rm -f \"$tmp\"; "
        f"printf '\\n%s:%s\\n' {shlex.quote(done_marker)} \"$rc\""
    )


def done_marker_status(text: str, done_marker: str) -> Optional[int]:
    prefix = f"{done_marker}:"
    for line in reversed(text.splitlines()):
        stripped = line.strip()
        if stripped.startswith(prefix):
            try:
                return int(stripped[len(prefix) :].strip())
            except ValueError:
                return None
    return None


def agent_output_contains_marker(
    text: str, output_marker: str, done_marker: str, expected_marker: str
) -> bool:
    lines = text.splitlines()
    output_start = None
    for index, line in enumerate(lines):
        if line.strip() == output_marker:
            output_start = index + 1
    if output_start is None:
        return False

    done_prefix = f"{done_marker}:"
    for line in lines[output_start:]:
        stripped = line.strip()
        if stripped.startswith(done_prefix):
            return False
        if stripped == expected_marker:
            return True
    return False


def process_pid_is_running(pid: int) -> bool:
    """Whether a test PID still names a non-zombie process on this platform."""
    if platform.system() == "Windows":
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel32.OpenProcess.restype = wintypes.HANDLE
        kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel32.WaitForSingleObject.restype = wintypes.DWORD
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle.restype = wintypes.BOOL
        handle = kernel32.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
        if not handle:
            return False
        try:
            return kernel32.WaitForSingleObject(handle, 0) == 0x00000102
        finally:
            kernel32.CloseHandle(handle)
    sampled = run(["ps", "-o", "stat=", "-p", str(pid)], timeout=2)
    state = sampled.stdout.strip()
    return sampled.returncode == 0 and bool(state) and not state.startswith("Z")


def live_helper_selftest() -> None:
    with tempfile.TemporaryDirectory(prefix="kettle-receipt-fixture-") as temp:
        fixture = Path(temp) / "fixture.png"
        write_image_receipt_fixture(fixture, 64, 36)
        width, height, rows = read_rgba_png(fixture)
        assert (width, height, len(rows)) == (64, 36, 36)

    redacted = redact_managed_paste_paths(
        {
            "unix": "'/var/folders/private/T/kettle\n-paste-123-456/0001.png'",
            "wrapped_unix": "'/private/T/kettle-pa\nste-123-456/0001.png'",
            "windows": "C:\\Users\\private\\AppData\\Local\\kettle-past\ne-123-456\\0001.png",
            "keep": "ordinary terminal output",
        }
    )
    assert isinstance(redacted, dict)
    serialized = json.dumps(redacted)
    assert not contains_managed_paste_marker(redacted)
    assert "private" not in serialized
    assert redacted["keep"] == "ordinary terminal output"
    assert shell_clear_line_keys("Windows") == ["escape"]
    assert shell_clear_line_keys("Darwin") == ["ctrl+u"]
    assert shell_clear_line_keys("Linux") == ["ctrl+u"]

    hidden_error = (
        "kettle ctl: server error [busy]: " + HIDDEN_WINDOW_SCREENSHOT_MESSAGE + "\n"
    )
    assert is_optional_remote_windows_screenshot_error(
        "Windows",
        {"SSH_CONNECTION": "192.0.2.1 50000 192.0.2.2 22"},
        stdout="",
        stderr=hidden_error,
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Windows", {}, stdout="", stderr=hidden_error
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Linux",
        {"SSH_CONNECTION": "192.0.2.1 50000 192.0.2.2 22"},
        stdout="",
        stderr=hidden_error,
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Windows",
        {"SSH_CLIENT": "192.0.2.1 50000 22"},
        stdout="",
        stderr=hidden_error.replace("[busy]", "[internal]"),
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Windows",
        {"SSH_CLIENT": "192.0.2.1 50000 22"},
        stdout="",
        stderr=hidden_error.replace("restore it", "try to restore it"),
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Windows",
        {"SSH_CLIENT": "192.0.2.1 50000 22"},
        stdout='{"error":"unrelated"}\n',
        stderr=hidden_error,
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Windows",
        {"SSH_CLIENT": "192.0.2.1 50000 22"},
        stdout="",
        stderr="\n" + hidden_error,
    )
    assert not is_optional_remote_windows_screenshot_error(
        "Windows",
        {"SSH_CLIENT": "192.0.2.1 50000 22"},
        stdout="",
        stderr=hidden_error + "\n",
    )

    class Pre312WindowsPath:
        def is_symlink(self) -> bool:
            return False

    class ReparseMetadata:
        st_file_attributes = 0x400

    original_platform_system = platform.system
    original_lstat = os.lstat
    platform.system = lambda: "Windows"  # type: ignore[assignment]
    os.lstat = lambda _path: ReparseMetadata()  # type: ignore[assignment]
    try:
        assert path_is_link(Pre312WindowsPath())  # type: ignore[arg-type]
    finally:
        os.lstat = original_lstat  # type: ignore[assignment]
        platform.system = original_platform_system  # type: ignore[assignment]

    black = bytes([0, 0, 0, 255] * 2)
    changed_top = bytes([255, 0, 0, 255, 0, 0, 0, 255])
    changed_bottom = bytes([0, 0, 0, 255, 0, 255, 0, 255])
    base_pixels = (2, 2, [black, black])
    changed_pixels = (2, 2, [changed_top, changed_bottom])
    top_left = {"x": 0, "y": 0, "width": 1, "height": 1}
    assert rgba_difference_count(base_pixels, changed_pixels) == 2
    assert rgba_difference_count(base_pixels, changed_pixels, rect=top_left) == 1
    assert (
        rgba_difference_count(
            base_pixels,
            changed_pixels,
            rect=top_left,
            outside_rect=True,
        )
        == 1
    )
    one_pixel = (1, 1, [bytes([0, 0, 0, 255])])
    assert rgba_card_difference_count(base_pixels, one_pixel) == 3

    assert macos_session_locked(
        plistlib.dumps([{"IOConsoleLocked": True}])
    ) is True
    assert macos_session_locked(
        plistlib.dumps([{"IOConsoleLocked": False}])
    ) is False
    assert macos_session_locked(plistlib.dumps({"IOConsoleLocked": False})) is False
    assert macos_session_locked(
        plistlib.dumps(
            [
                {
                    "IOConsoleUsers": [
                        {"CGSSessionScreenIsLocked": False},
                        {"CGSSessionScreenIsLocked": True},
                    ]
                }
            ]
        )
    ) is True
    assert macos_session_locked(b"not a plist") is None

    # Windows Job assignment necessarily happens after CreateProcess.  Prove
    # the internal-worker argv prevents Python startup hooks from executing in
    # that interval; a plain interpreter is the discriminating negative control.
    with tempfile.TemporaryDirectory(prefix="kettle-python-startup-") as fixture:
        startup_root = Path(fixture)
        startup_marker = startup_root / "sitecustomize-ran"
        (startup_root / "sitecustomize.py").write_text(
            "import os,pathlib\n"
            "pathlib.Path(os.environ['KETTLE_SITE_MARKER']).write_text('ran')\n",
            encoding="utf-8",
        )
        startup_env = os.environ.copy()
        startup_env["PYTHONPATH"] = str(startup_root)
        startup_env["KETTLE_SITE_MARKER"] = str(startup_marker)
        plain_start = subprocess.run(
            [sys.executable, "-c", "pass"],
            env=startup_env,
            timeout=10,
            check=False,
        )
        assert plain_start.returncode == 0
        assert startup_marker.is_file(), (
            "the startup-hook negative control did not execute sitecustomize"
        )
        startup_marker.unlink()
        isolated_start = subprocess.run(
            _internal_repository_worker_argv("--help"),
            env=startup_env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
        assert isolated_start.returncode == 0
        assert not startup_marker.exists(), (
            "an internal provenance worker executed user Python startup code"
        )

    artifact_root = Path("artifacts")
    state_shot = live_state_screenshot_path(artifact_root, "search-open")
    transition_shot = live_transition_screenshot_path(artifact_root, "search-open")
    assert state_shot == artifact_root / "search-open.png"
    assert transition_shot == artifact_root / "search-open-transition.png"
    assert state_shot != transition_shot

    # Provenance must notice content changes even when porcelain reports the
    # same already-dirty pathname before and after the mutation.
    with tempfile.TemporaryDirectory(prefix="kettle-provenance-") as fixture:
        repository = Path(fixture)
        assert run(["git", "init", "-q", str(repository)]).returncode == 0
        tracked = repository / "tracked.txt"
        tracked.write_text("base\n", encoding="utf-8")
        attributes = repository / ".gitattributes"
        attributes.write_text("tracked.txt diff=must-not-run\n", encoding="utf-8")
        assert run(
            ["git", "-C", str(repository), "add", "tracked.txt", ".gitattributes"]
        ).returncode == 0
        assert run(
            [
                "git",
                "-C",
                str(repository),
                "config",
                "diff.must-not-run.textconv",
                "kettle-smoke-textconv-must-not-run",
            ]
        ).returncode == 0
        if os.name != "nt":
            # A completed leader is reaped by communicate. The internal anchor
            # must still reserve the private PGID until cleanup, or this numeric
            # kill target can be redirected by immediate PID/PGID reuse. Signal
            # the exact anchor with HUP while its leader is still blocked, too:
            # removing the inherited HUP ignore must make this test fail safely
            # while the live leader still reserves the group.
            with tempfile.TemporaryDirectory(
                prefix="kettle-provenance-anchor-"
            ) as anchor_fixture:
                anchor_record = Path(anchor_fixture) / "anchor-pid"
                anchored_worker = subprocess.Popen(
                    _internal_repository_worker_argv(
                        PROVENANCE_ANCHOR_PROBE_ARG, anchor_record
                    ),
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    start_new_session=True,
                )
                anchor_handle: Optional[StableProcessHandle] = None
                anchor_errors: List[BaseException] = []
                anchor_reaped = threading.Event()
                try:
                    anchor_deadline = time.monotonic() + 5
                    while (
                        time.monotonic() < anchor_deadline
                        and not anchor_record.is_file()
                    ):
                        if anchored_worker.poll() is not None:
                            break
                        time.sleep(0.01)
                    assert anchor_record.is_file(), (
                        "the internal provenance worker did not publish its group anchor"
                    )
                    anchor_pid = int(anchor_record.read_text(encoding="ascii"))
                    anchor_handle = StableProcessHandle.open(anchor_pid)
                    assert anchor_handle.signal(signal.SIGHUP)
                    time.sleep(0.05)
                    assert anchor_handle.matches_current(), (
                        "the internal provenance group anchor did not survive leader HUP"
                    )
                    anchor_stdout, anchor_stderr = anchored_worker.communicate(
                        input="1", timeout=5
                    )
                    assert anchored_worker.returncode == 0, anchor_stderr
                    assert anchor_stdout == ""
                    os.killpg(anchored_worker.pid, 0)
                finally:
                    anchor_errors, anchor_reaped = _stop_repository_worker(
                        anchored_worker, None
                    )
                    if anchor_handle is not None:
                        anchor_handle.close()
            assert not anchor_errors and anchor_reaped.is_set()
            group_gone_deadline = time.monotonic() + 3
            while True:
                try:
                    os.killpg(anchored_worker.pid, 0)
                except ProcessLookupError:
                    break
                except PermissionError:
                    # Darwin reports EPERM while the killed orphan is a zombie
                    # waiting for launchd to reap it; the PGID is still reserved
                    # and cannot target a live unrelated process in that state.
                    pass
                if time.monotonic() >= group_gone_deadline:
                    raise AssertionError(
                        "the internal provenance group anchor survived cleanup"
                    )
                time.sleep(0.02)
        tracked.write_text("dirty one\n", encoding="utf-8")
        first = repository_source_sha256(repository)
        tracked.write_text("dirty two\n", encoding="utf-8")
        second = repository_source_sha256(repository)
        assert first != second, "an edit within an already-dirty file was invisible"
        try:
            repository_source_sha256(
                repository,
                RepositoryProvenanceBudget(max_entries=1),
            )
        except RuntimeError as error:
            assert "file limit" in str(error)
        else:
            raise AssertionError(
                "tracked/index files did not consume the global provenance file limit"
            )
        untracked = repository / "untracked.txt"
        untracked.write_text("one\n", encoding="utf-8")
        dirty_identity = repository_source_identity(repository)
        assert dirty_identity["git_dirty"] is True
        assert int(dirty_identity["git_status_entries"]) > 0
        assert re.fullmatch(
            r"[0-9a-f]{64}", str(dirty_identity["git_status_sha256"])
        )
        first = repository_source_sha256(repository)
        untracked.write_text("two\n", encoding="utf-8")
        second = repository_source_sha256(repository)
        assert first != second, "an edit within an untracked file was invisible"
        try:
            repository_source_sha256(
                repository,
                RepositoryProvenanceBudget(max_entries=0),
            )
        except RuntimeError as error:
            assert "file limit" in str(error)
        else:
            raise AssertionError("repository provenance file limit was not enforced")
        try:
            repository_source_sha256(
                repository,
                RepositoryProvenanceBudget(max_bytes=1),
            )
        except RuntimeError as error:
            assert "aggregate byte limit" in str(error)
        else:
            raise AssertionError("repository provenance byte limit was not enforced")
        real_open = os.open
        real_close = os.close
        opened_fds: Set[int] = set()

        def tracking_open(*args: object, **kwargs: object) -> int:
            fd = real_open(*args, **kwargs)  # type: ignore[arg-type]
            opened_fds.add(fd)
            return fd

        def tracking_close(fd: int) -> None:
            real_close(fd)
            opened_fds.discard(fd)

        os.open = tracking_open  # type: ignore[assignment]
        os.close = tracking_close  # type: ignore[assignment]
        try:
            try:
                repository_file_digest(
                    repository,
                    Path("untracked.txt"),
                    RepositoryProvenanceBudget(max_bytes=0),
                )
            except RuntimeError as error:
                assert "aggregate byte limit" in str(error)
            else:
                raise AssertionError("the direct provenance byte limit did not fire")
        finally:
            os.open = real_open  # type: ignore[assignment]
            os.close = real_close  # type: ignore[assignment]
        assert not opened_fds, (
            "a rejected untracked file leaked descriptors: "
            f"{sorted(opened_fds)}"
        )
        started = time.monotonic()
        try:
            repository_source_sha256(
                repository,
                RepositoryProvenanceBudget(timeout_s=0.001),
            )
        except RuntimeError as error:
            assert "time limit" in str(error)
        else:
            raise AssertionError("the parent-side provenance deadline did not fire")
        assert time.monotonic() - started < 2.0, (
            "repository provenance exceeded its parent-enforced timeout"
        )
        timeout_record = repository / "timeout-probe-pids"
        started = time.monotonic()
        timeout_failure: Optional[RepositoryWorkerTimeout] = None
        try:
            _run_repository_worker(
                _internal_repository_worker_argv(
                    PROVENANCE_SABOTAGE_WORKER_ARG,
                    timeout_record,
                ),
                2.0,
            )
        except RepositoryWorkerTimeout as error:
            timeout_failure = error
            assert "hard time limit" in str(error)
        else:
            raise AssertionError("a blocked worker tree escaped its hard deadline")
        assert time.monotonic() - started < 2.5, (
            "blocked provenance cleanup added an unbounded teardown wait"
        )
        assert timeout_record.is_file(), (
            "the timeout probe never reached its blocked worker/child state"
        )
        timeout_pids = [
            int(value) for value in timeout_record.read_text(encoding="ascii").split()
        ]
        assert len(timeout_pids) == 2
        tree_deadline = time.monotonic() + 3.0
        while time.monotonic() < tree_deadline and any(
            process_pid_is_running(pid) for pid in timeout_pids
        ):
            time.sleep(0.05)
        assert not [pid for pid in timeout_pids if process_pid_is_running(pid)], (
            "the timed-out provenance worker or its descendant survived"
        )
        assert timeout_failure is not None and timeout_failure.reaped.wait(3), (
            "the detached provenance owner did not finish reaping the worker"
        )

        if os.name == "nt":
            close_only_record = repository / "close-only-probe-pids"
            close_only_worker = subprocess.Popen(
                _internal_repository_worker_argv(
                    PROVENANCE_SABOTAGE_WORKER_ARG,
                    close_only_record,
                ),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            close_only_job = WindowsKillJob()
            close_only_job.assign(close_only_worker)
            assert close_only_worker.stdin is not None
            close_only_worker.stdin.write("1")
            close_only_worker.stdin.flush()
            close_only_deadline = time.monotonic() + 3
            while (
                time.monotonic() < close_only_deadline
                and (
                    not close_only_record.is_file()
                    or not close_only_record.read_text(encoding="ascii").endswith("\n")
                )
            ):
                time.sleep(0.05)
            assert close_only_record.is_file(), (
                "the Job close-only probe never reached its child state"
            )
            close_only_pids = [
                int(value)
                for value in close_only_record.read_text(encoding="ascii").split()
            ]
            assert len(close_only_pids) == 2, (
                "the Job close-only probe did not record both worker and child"
            )
            close_errors, close_reaped = _stop_repository_worker(
                close_only_worker, close_only_job, terminate_job=False
            )
            assert not close_errors, close_errors
            assert close_reaped.wait(3), (
                "KILL_ON_JOB_CLOSE did not let the close-only worker reap"
            )
            assert not [
                pid for pid in close_only_pids if process_pid_is_running(pid)
            ], "the Job close-only worker or child survived"

            # The configured-editor path must prove the exact in-pane process
            # can self-assign without a reusable ctl PID, and that deletion does
            # not begin until Job accounting reaches zero. Hold one sandbox file
            # without delete sharing so a premature rmtree is a real Windows
            # failure rather than an abstract callback-order assertion.
            native_job_target = AgentShellTarget(mode="native")
            native_job = WindowsKillJob(named=True)
            native_job_root = Path(native_job_target.create_nvim_sandbox_host())
            held_file = native_job_root / "held-by-contained-powershell"
            held_file.write_text("fixture\n", encoding="utf-8")
            ready_file = native_job_root / "job-self-assignment-ready"
            powershell = windows_system_executable(
                "System32", "WindowsPowerShell", "v1.0", "powershell.exe"
            )
            native_job_process = subprocess.Popen(
                [
                    powershell,
                    "-NoLogo",
                    "-NoProfile",
                    "-Command",
                    native_job.powershell_assign_current_process_command()
                    + "; $KettleSmokeHeld=[IO.File]::Open("
                    + shell_quote(str(held_file), windows=True)
                    + ",[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,"
                    + "[IO.FileShare]::None); [IO.File]::WriteAllText("
                    + shell_quote(str(ready_file), windows=True)
                    + ",'ready'); Start-Sleep -Seconds 60",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            ready_deadline = time.monotonic() + 15
            while (
                time.monotonic() < ready_deadline
                and not ready_file.is_file()
                and native_job_process.poll() is None
            ):
                time.sleep(0.02)
            if not ready_file.is_file():
                native_job_process.kill()
                stdout, stderr = native_job_process.communicate(timeout=3)
                raise AssertionError(
                    "PowerShell did not self-assign to the named Job: "
                    f"stdout={stdout!r} stderr={stderr!r}"
                )
            assert native_job.active_processes() >= 1
            real_wait_empty = native_job.wait_empty
            real_rmtree = shutil.rmtree
            drain_finished: List[bool] = []

            # Exercise Windows deletion itself, not just callback order.  The
            # contained process deliberately opened this file without delete
            # sharing, so a pre-drain tree removal must be refused by the OS.
            try:
                real_rmtree(native_job_root)
            except OSError:
                pass
            else:
                raise AssertionError(
                    "Windows removed a sandbox before its contained Job drained"
                )
            assert native_job_root.exists() and held_file.exists(), (
                "the pre-drain deletion probe did not retain the locked sandbox"
            )

            def record_real_job_drain(timeout_s: float = 5.0) -> None:
                real_wait_empty(timeout_s)
                native_job_process.poll()
                assert native_job_process.returncode is not None, (
                    "Job accounting reached zero before its contained process exited"
                )
                drain_finished.append(True)

            def reject_premature_sandbox_remove(path: object, *args: object, **kwargs: object) -> None:
                assert drain_finished == [True], (
                    "sandbox removal began before the real Windows Job drained"
                )
                real_rmtree(path, *args, **kwargs)  # type: ignore[arg-type]

            native_job.wait_empty = record_real_job_drain  # type: ignore[method-assign]
            shutil.rmtree = reject_premature_sandbox_remove  # type: ignore[assignment]
            try:
                native_job_target.cleanup_nvim_sandbox_host(
                    str(native_job_root), windows_job=native_job
                )
                native_job_process.wait(timeout=3)
            finally:
                shutil.rmtree = real_rmtree
                if native_job_process.returncode is None:
                    native_job_process.kill()
                    native_job_process.wait(timeout=3)
                if native_job_root.exists():
                    real_rmtree(native_job_root)
            assert not native_job_root.exists()

            # Junctions are a separate Windows reparse type: DirEntry.is_symlink
            # is false for them. Cleanup must remove the junction itself without
            # walking it or clearing read-only bits on its external target.
            junction_job: Optional[WindowsKillJob] = None
            junction_root: Optional[Path] = None
            junction_target: Optional[Path] = None
            external_file: Optional[Path] = None
            junction: Optional[Path] = None
            try:
                junction_job = WindowsKillJob(named=True)
                junction_root = Path(
                    native_job_target.create_nvim_sandbox_host()
                )
                junction_target = Path(
                    native_job_target.create_nvim_sandbox_host()
                )
                external_file = junction_target / "must-stay-read-only"
                external_file.write_text("external\n", encoding="utf-8")
                external_file.chmod(stat.S_IREAD)
                junction = junction_root / "external-junction"
                linked = subprocess.run(
                    [
                        windows_system_executable("System32", "cmd.exe"),
                        "/d",
                        "/c",
                        "mklink",
                        "/J",
                        str(junction),
                        str(junction_target),
                    ],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                assert linked.returncode == 0, (linked.stdout, linked.stderr)
                assert native_job_target.path_is_link(junction)
                native_job_target.cleanup_nvim_sandbox_host(
                    str(junction_root), windows_job=junction_job
                )
                assert not junction_root.exists()
                assert external_file.exists()
                assert external_file.stat().st_mode & stat.S_IWRITE == 0, (
                    "sandbox cleanup changed permissions through a junction"
                )
            finally:
                if junction_job is not None:
                    with contextlib.suppress(BaseException):
                        junction_job.close()
                if junction is not None and native_job_target.path_is_link(junction):
                    junction.rmdir()
                if external_file is not None and external_file.exists():
                    external_file.chmod(stat.S_IWRITE | stat.S_IREAD)
                if junction_root is not None and junction_root.exists():
                    real_rmtree(junction_root)
                if junction_target is not None and junction_target.exists():
                    real_rmtree(junction_target)

        if os.name != "nt":
            real_popen = subprocess.Popen
            real_killpg = os.killpg
            communication_actions: List[str] = []

            class BrokenCommunicationWorker:
                pid = 987654321
                returncode = None

                def __init__(self, *_args: object, **_kwargs: object) -> None:
                    self.stdin = io.StringIO()
                    self.stdout = io.StringIO()
                    self.stderr = io.StringIO()
                    self.communications = 0

                def communicate(
                    self, input: Optional[str] = None, timeout: Optional[float] = None
                ) -> Tuple[str, str]:
                    del input, timeout
                    self.communications += 1
                    if self.communications == 1:
                        raise OSError("intentional communicate failure")
                    self.returncode = -9
                    communication_actions.append("reaped")
                    return "", ""

                def wait(self, timeout: Optional[float] = None) -> int:
                    del timeout
                    self.returncode = -9
                    communication_actions.append("waited")
                    return -9

                def kill(self) -> None:
                    communication_actions.append("killed")

            broken_worker: Optional[BrokenCommunicationWorker] = None

            def broken_popen(*args: object, **kwargs: object) -> BrokenCommunicationWorker:
                nonlocal broken_worker
                broken_worker = BrokenCommunicationWorker(*args, **kwargs)
                return broken_worker

            subprocess.Popen = broken_popen  # type: ignore[assignment]
            os.killpg = lambda pid, sig: communication_actions.append(  # type: ignore[assignment]
                f"killpg:{pid}:{sig}"
            )
            try:
                try:
                    _run_repository_worker(["unused"], 2.0)
                except RuntimeError as error:
                    assert "intentional communicate failure" in str(error)
                else:
                    raise AssertionError(
                        "an unexpected worker communication error was discarded"
                    )
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline and "reaped" not in communication_actions:
                    time.sleep(0.01)
                assert communication_actions[0].startswith("killpg:"), (
                    "a communication failure did not terminate the worker group"
                )
                assert "reaped" in communication_actions, (
                    "a communication failure did not transfer ownership to the reaper"
                )
            finally:
                subprocess.Popen = real_popen  # type: ignore[assignment]
                os.killpg = real_killpg  # type: ignore[assignment]

            class UnreapableWorker:
                stdin = None
                stdout = None
                stderr = None

                def communicate(self) -> Tuple[str, str]:
                    raise OSError("intentional reap failure")

                def wait(self) -> int:
                    raise OSError("intentional wait failure")

            false_reaped = threading.Event()
            _eventually_reap_process(  # type: ignore[arg-type]
                UnreapableWorker(), false_reaped
            )
            assert not false_reaped.is_set(), (
                "the reaper reported success after both wait paths failed"
            )

            completed_failure_record = repository / "completed-failure-child"
            completed_failure_code = (
                "import pathlib,subprocess,sys; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import time; time.sleep(60)'], stdin=subprocess.DEVNULL, "
                "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); "
                "pathlib.Path(sys.argv[1]).write_text(str(child.pid), encoding='ascii'); "
                "raise SystemExit(7)"
            )
            returncode, _stdout, _stderr = _run_repository_worker(
                [sys.executable, "-c", completed_failure_code, str(completed_failure_record)],
                3.0,
            )
            assert returncode == 7
            completed_failure_pid = int(
                completed_failure_record.read_text(encoding="ascii")
            )
            completed_failure_deadline = time.monotonic() + 3
            while (
                time.monotonic() < completed_failure_deadline
                and process_pid_is_running(completed_failure_pid)
            ):
                time.sleep(0.05)
            assert not process_pid_is_running(completed_failure_pid), (
                "a descendant of a completed nonzero worker escaped containment"
            )

        real_stream_git_output = globals()["stream_git_output"]
        status_consumed_past_limit = False

        def status_entry_sentinel(
            _repository: Path,
            args: List[str],
            _budget: RepositoryProvenanceBudget,
            consume: Callable[[bytes], None],
        ) -> None:
            nonlocal status_consumed_past_limit
            assert args[:2] == ["status", "--porcelain=v1"]
            consume(b"?? first\0")
            status_consumed_past_limit = True
            raise AssertionError("status output was consumed past its entry cap")

        globals()["stream_git_output"] = status_entry_sentinel
        try:
            try:
                _repository_source_identity_impl(
                    repository,
                    RepositoryProvenanceBudget(max_scan_entries=0),
                )
            except RuntimeError as error:
                assert "status-entry limit" in str(error)
            else:
                raise AssertionError("the streaming status entry cap did not fire")
        finally:
            globals()["stream_git_output"] = real_stream_git_output
        assert not status_consumed_past_limit, (
            "status traversal continued after crossing its record cap"
        )
        try:
            repository_source_sha256(
                repository,
                RepositoryProvenanceBudget(max_scan_entries=0),
            )
        except RuntimeError as error:
            assert "status-entry limit" in str(error)
        else:
            raise AssertionError(
                "repository status entry limit was not enforced while streaming"
            )
        link = repository / "linked-directory"
        try:
            link.symlink_to(repository, target_is_directory=True)
        except (NotImplementedError, OSError):
            pass
        else:
            try:
                repository_source_sha256(repository)
            except RuntimeError as error:
                assert "linked entry" in str(error)
            else:
                raise AssertionError(
                    "repository provenance must reject a linked directory"
                )
            finally:
                link.unlink()
        if hasattr(os, "mkfifo"):
            fifo = repository / "untracked-fifo"
            os.mkfifo(fifo)
            try:
                try:
                    repository_source_sha256(repository)
                except RuntimeError as error:
                    assert "special file" in str(error)
                else:
                    raise AssertionError(
                        "repository provenance must reject an untracked FIFO"
                    )
            finally:
                fifo.unlink()

    def titlebar_fixture_geometry(title_at_bottom: bool) -> Dict[str, object]:
        bar_y = 120.0 if title_at_bottom else 20.0
        return {
            "surface": {"width": 220, "height": 140},
            "cell": {"width": 8.0, "height": 14.0},
            "padding": {"x": 8.0, "y": 8.0},
            "pane_titlebars": [
                {
                    "pane": 1,
                    "focused": True,
                    "rect": {
                        "x": 0.0,
                        "y": bar_y,
                        "width": 110.0,
                        "height": 20.0,
                    },
                    "pane_rect": {
                        "x": 0.0,
                        "y": 20.0,
                        "width": 110.0,
                        "height": 120.0,
                    },
                    "fitted_title": "  fixture-one",
                    "cols": 11,
                    "rows": 6,
                },
                {
                    "pane": 2,
                    "focused": False,
                    "rect": {
                        "x": 110.0,
                        "y": bar_y,
                        "width": 110.0,
                        "height": 20.0,
                    },
                    "pane_rect": {
                        "x": 110.0,
                        "y": 20.0,
                        "width": 110.0,
                        "height": 120.0,
                    },
                    "fitted_title": "  fixture-two",
                    "cols": 11,
                    "rows": 6,
                },
            ],
        }

    def titlebar_fixture_rows(
        geometry: Dict[str, object], broadcast: bool
    ) -> List[bytes]:
        background = parse_hex_rgb(SPLIT_TITLEBAR_COLOR_HEX["grid"])
        raw_rows = [
            bytearray((*background, 255) * 220)
            for _ in range(140)
        ]
        titlebars = geometry["pane_titlebars"]
        assert isinstance(titlebars, list)
        for titlebar in titlebars:
            assert isinstance(titlebar, dict)
            x, y, rect_width, rect_height = split_titlebar_rect(
                titlebar["rect"], "fixture.titlebar"
            )
            state = (
                "transmit"
                if titlebar["focused"]
                else ("receive" if broadcast else "inactive")
            )
            color = parse_hex_rgb(SPLIT_TITLEBAR_COLOR_HEX[state])
            for pixel_y in range(int(y), int(y + rect_height)):
                for pixel_x in range(int(x), int(x + rect_width)):
                    offset = pixel_x * 4
                    raw_rows[pixel_y][offset : offset + 4] = bytes((*color, 255))
        return [bytes(row) for row in raw_rows]

    for fixture_at_bottom in (False, True):
        fixture_geometry = titlebar_fixture_geometry(fixture_at_bottom)
        for fixture_broadcast in (False, True):
            fixture_rows = titlebar_fixture_rows(
                fixture_geometry, fixture_broadcast
            )
            fixture_analysis = analyze_split_titlebar_frame(
                fixture_geometry,
                220,
                140,
                fixture_rows,
                title_at_bottom=fixture_at_bottom,
                broadcast=fixture_broadcast,
            )
            states = {
                pane["state"]
                for pane in fixture_analysis["panes"]  # type: ignore[union-attr]
            }
            expected_states = {
                "transmit",
                "receive" if fixture_broadcast else "inactive",
            }
            assert states == expected_states

    wrong_edge_geometry = titlebar_fixture_geometry(True)
    wrong_edge_titlebars = wrong_edge_geometry["pane_titlebars"]
    assert isinstance(wrong_edge_titlebars, list)
    for wrong_edge_titlebar in wrong_edge_titlebars:
        assert isinstance(wrong_edge_titlebar, dict)
        wrong_edge_rect = wrong_edge_titlebar["rect"]
        assert isinstance(wrong_edge_rect, dict)
        wrong_edge_rect["y"] = 20.0
    try:
        analyze_split_titlebar_frame(
            wrong_edge_geometry,
            220,
            140,
            titlebar_fixture_rows(wrong_edge_geometry, False),
            title_at_bottom=True,
            broadcast=False,
        )
    except RuntimeError as error:
        assert "wrong pane edge" in str(error)
    else:
        raise AssertionError("bottom titlebar fixture accepted a top-edge rectangle")

    wrong_color_geometry = titlebar_fixture_geometry(False)
    wrong_color_rows = [
        bytearray(row)
        for row in titlebar_fixture_rows(wrong_color_geometry, False)
    ]
    wrong_color_offset = 4 * 4
    wrong_color_rows[30][wrong_color_offset : wrong_color_offset + 4] = (
        b"\x00\x00\x00\xff"
    )
    try:
        analyze_split_titlebar_frame(
            wrong_color_geometry,
            220,
            140,
            [bytes(row) for row in wrong_color_rows],
            title_at_bottom=False,
            broadcast=False,
        )
    except RuntimeError as error:
        assert "transmit titlebar" in str(error)
    else:
        raise AssertionError("titlebar fixture accepted the wrong configured color")

    legacy_tab_bar = {
        "segments": [
            {"rect": {"x": 0.0, "width": 100.0}},
            {"rect": {"x": 100.0, "width": 100.0}},
            {"rect": {"x": 200.0, "width": 100.0}},
        ],
        "new_tab_menu": {"x": 300.0, "width": 0.0},
        "new_tab": {"x": 300.0, "width": 0.0},
    }
    aligned_tab_bar = {
        "segments": [
            {"rect": {"x": 0.0, "width": 100.0}},
            {"rect": {"x": 100.0, "width": 100.0}},
            {"rect": {"x": 200.0, "width": 60.0}},
        ],
        "new_tab_menu": {"x": 260.0, "width": 20.0},
        "new_tab": {"x": 280.0, "width": 20.0},
    }
    assert tab_segment_layout_error(legacy_tab_bar) is None
    assert tab_segment_layout_error(aligned_tab_bar) is None
    invalid_boundary = json.loads(json.dumps(aligned_tab_bar))
    invalid_boundary["segments"][1]["rect"]["x"] = 97.0
    assert "boundary" in (tab_segment_layout_error(invalid_boundary) or "")
    invalid_last = json.loads(json.dumps(aligned_tab_bar))
    invalid_last["segments"][-1]["rect"]["width"] = 65.0
    assert "reserve" in (tab_segment_layout_error(invalid_last) or "")
    invalid_width = json.loads(json.dumps(aligned_tab_bar))
    invalid_width["segments"][0]["rect"]["width"] = 0.0
    assert "positive" in (tab_segment_layout_error(invalid_width) or "")

    class FakeClock:
        def __init__(self) -> None:
            self.value = 0.0

        def monotonic(self) -> float:
            return self.value

        def sleep(self, seconds: float) -> None:
            self.value += seconds

    geometry_a = {"tab_bar": json.loads(json.dumps(aligned_tab_bar))}
    geometry_b = {"tab_bar": json.loads(json.dumps(aligned_tab_bar))}
    geometry_a["tab_bar"]["segments"][0]["title"] = "starting"
    geometry_b["tab_bar"]["segments"][0]["title"] = "settled"
    stable_sequence = [geometry_a, geometry_b, geometry_b, geometry_b]
    stable_clock = FakeClock()

    def read_stabilizing_geometry() -> Dict[str, object]:
        if len(stable_sequence) > 1:
            return stable_sequence.pop(0)
        return stable_sequence[0]

    settled = wait_for_stable_tab_bar(
        read_stabilizing_geometry,
        timeout_seconds=1.0,
        quiet_seconds=0.2,
        poll_seconds=0.1,
        monotonic=stable_clock.monotonic,
        sleep=stable_clock.sleep,
    )
    assert settled["tab_bar"]["segments"][0]["title"] == "settled"

    timeout_clock = FakeClock()
    timeout_poll = 0

    def read_changing_geometry() -> Dict[str, object]:
        nonlocal timeout_poll
        timeout_poll += 1
        geometry = json.loads(json.dumps(geometry_a))
        geometry["tab_bar"]["segments"][0]["title"] = f"title-{timeout_poll}"
        return geometry

    try:
        wait_for_stable_tab_bar(
            read_changing_geometry,
            timeout_seconds=0.25,
            quiet_seconds=0.2,
            poll_seconds=0.1,
            monotonic=timeout_clock.monotonic,
            sleep=timeout_clock.sleep,
        )
    except RuntimeError as error:
        assert "timed out" in str(error)
        assert "signature=" in str(error)
    else:
        raise AssertionError("changing tab titles did not hit the stability deadline")

    portable_diagnostics = create_default_diagnostic_root(windows=False)
    assert portable_diagnostics == Path("target/diagnostics").resolve()
    try:
        create_windows_private_directory("../escaped-")
    except ValueError:
        pass
    else:
        raise AssertionError("unsafe Windows private-directory prefix was accepted")
    if platform.system() == "Windows":
        windows_diagnostics = create_default_diagnostic_root()
        try:
            assert windows_diagnostics.parent == windows_live_smoke_parent()
            assert windows_diagnostics.name.startswith(
                "kettle-live-ui-diagnostics-"
            )
            assert not path_is_link(windows_diagnostics)
        finally:
            if (
                windows_diagnostics.parent == windows_live_smoke_parent()
                and windows_diagnostics.name.startswith(
                    "kettle-live-ui-diagnostics-"
                )
            ):
                shutil.rmtree(windows_diagnostics)

    marker = "KETTLE_AGENT_AUTH_EXPECTED"
    output_marker = "KETTLE_AGENT_AUTH_OUTPUT_BEGIN"
    done_marker = "KETTLE_AGENT_AUTH_DONE"

    windows_command = agent_auth_command(
        "codex", marker, output_marker, done_marker, windows=True
    )
    unix_command = agent_auth_command(
        "claude", marker, output_marker, done_marker, windows=False
    )
    assert "New-TemporaryFile" not in windows_command
    assert "[System.IO.Path]::GetTempFileName()" in windows_command
    assert "$LASTEXITCODE=$null" in windows_command
    assert output_marker not in windows_command
    assert output_marker not in unix_command

    false_positive = "\n".join(
        [
            f"PS> command 'Reply exactly {marker} and nothing else.'",
            output_marker,
            "New-TemporaryFile: command not found",
            f"{done_marker}:0",
        ]
    )
    assert done_marker_status(false_positive, done_marker) == 0
    assert not agent_output_contains_marker(
        false_positive, output_marker, done_marker, marker
    )

    success = "\n".join(
        [
            f"PS> command 'Reply exactly {marker} and nothing else.'",
            output_marker,
            "agent diagnostic output",
            marker,
            f"{done_marker}:0",
        ]
    )
    assert done_marker_status(success, done_marker) == 0
    assert agent_output_contains_marker(success, output_marker, done_marker, marker)
    assert done_marker_status(f"{done_marker}:17", done_marker) == 17
    assert done_marker_status("no completion marker", done_marker) is None

    # Cargo's JSON artifact is authoritative: it preserves custom target
    # directories, configured target triples, and Windows's `.exe` suffix.
    cargo_fixture = Path(tempfile.gettempdir()) / "custom-target" / (
        "kettle.exe" if platform.system() == "Windows" else "kettle"
    )
    cargo_messages = "\n".join(
        [
            json.dumps(
                {
                    "reason": "compiler-artifact",
                    "target": {"name": "kettle_ui", "kind": ["lib"]},
                    "executable": None,
                }
            ),
            json.dumps(
                {
                    "reason": "compiler-artifact",
                    "target": {"name": "kettle", "kind": ["bin"]},
                    "executable": str(cargo_fixture),
                }
            ),
        ]
    )
    assert release_kettle_artifact_from_messages(cargo_messages) == (
        cargo_fixture.resolve()
    )

    # cwd_title_command: the tab-title/split-titlebar fixtures used to be
    # POSIX-only. Exercise both command shapes from whichever host actually
    # runs this self-test, the same `windows=`-override pattern
    # `agent_auth_command` already uses above.
    cwd_marker = "KETTLE_CWD_TITLE_TEST_MARKER"
    cwd_title = "..PI-1/platform"
    win_cwd_command = cwd_title_command(
        r"C:\Users\test\kettle-fixture", cwd_title, cwd_marker, windows=True
    )
    posix_cwd_command = cwd_title_command(
        "/tmp/kettle-fixture", cwd_title, cwd_marker, windows=False
    )
    # Windows: OSC 9;9 (native path, verbatim) + OSC 2, NOT the OSC 7
    # `file://` URI shape (which would require a separator/percent-encoding
    # translation `abbreviate_home`'s literal `$HOME`-prefix match can't
    # tolerate).
    assert "Set-Location" in win_cwd_command
    assert "[Console]::Write" in win_cwd_command
    assert "]9;9;" in win_cwd_command
    assert "]2;" in win_cwd_command
    assert "file://" not in win_cwd_command
    cwd_marker_left, cwd_marker_right = split_marker(cwd_marker)
    assert cwd_marker not in win_cwd_command
    assert shell_quote(cwd_marker_left, windows=True) in win_cwd_command
    assert shell_quote(cwd_marker_right, windows=True) in win_cwd_command
    assert "Start-Sleep -Seconds 5" in win_cwd_command
    # POSIX: unchanged `cd` + `printf` OSC 7 shape.
    assert "printf" in posix_cwd_command
    assert "file://localhost" in posix_cwd_command
    assert "Set-Location" not in posix_cwd_command
    assert "[Console]::Write" not in posix_cwd_command
    assert cwd_marker not in posix_cwd_command
    assert shell_quote(cwd_marker_left) in posix_cwd_command
    assert shell_quote(cwd_marker_right) in posix_cwd_command
    assert "sleep 5" in posix_cwd_command

    notification_marker = "KETTLE_NOTIFICATION_COMPLETE"
    notification = notification_command("title", "body", notification_marker)
    notification_left, notification_right = split_marker(notification_marker)
    assert notification_marker not in notification
    assert shell_quote(notification_left) in notification
    assert shell_quote(notification_right) in notification

    # The host OS and target shell dialect are separate decisions. In
    # particular, Windows Kettle -> WSL must construct POSIX commands and must
    # launch a deterministic shell without sourcing or editing user rc files.
    native_target = AgentShellTarget(mode="native")
    wsl_target = AgentShellTarget(
        mode="wsl",
        wsl_distro="Ubuntu Test",
        astro_config="/home/test/.config/nvim",
    )
    assert wsl_target.wsl_base_argv() == [
        "wsl.exe",
        "--distribution",
        "Ubuntu Test",
        "--cd",
        "~",
    ]
    assert wsl_target.launch_args() == [
        "-e",
        "wsl.exe",
        "--distribution",
        "Ubuntu Test",
        "--cd",
        "~",
        "--exec",
        "bash",
        "--noprofile",
        "--norc",
    ]
    assert wsl_target.host_argv(["tmux", "-V"])[-3:] == [
        "--exec",
        "tmux",
        "-V",
    ]
    wsl_tmux_argv = wsl_target.command_argv(["tmux", "-V"])
    assert wsl_tmux_argv[:5] == [
        "wsl.exe",
        "--distribution",
        "Ubuntu Test",
        "--cd",
        "~",
    ]
    assert wsl_tmux_argv[-5:-2] == ["bash", "--noprofile", "--norc"]
    assert "exec tmux -V" in wsl_tmux_argv[-1]
    assert ".npm-global/bin" in wsl_tmux_argv[-1]
    assert "/mnt/[A-Za-z]/*" in wsl_target.posix_path_setup()
    assert "KETTLE_SMOKE_LINUX_PATH" in wsl_target.posix_path_setup()
    assert "KETTLE_SMOKE_LINUX_PATH" not in wsl_target.posix_path_setup(
        keep_windows_host_paths=True
    )
    assert "readlink -f" in wsl_target._posix_command_path_script(
        "nvim", keep_windows_host_paths=False
    )
    assert "readlink -f" not in native_target._posix_command_path_script(
        "nvim", keep_windows_host_paths=False
    )
    assert wsl_target.is_wsl_host_tool_path(
        "/mnt/c/Program Files/nodejs/nvim.exe"
    )
    assert wsl_target.is_wsl_host_tool_path("/mnt/C/tools/tmux.exe")
    assert not wsl_target.is_wsl_host_tool_path("/home/test/bin/nvim")
    assert not wsl_target.is_wsl_host_tool_path("/mnt/container/bin/nvim")

    class WindowsHostOnlyTarget(AgentShellTarget):
        def _posix_command_path(
            self, command: str, *, keep_windows_host_paths: bool
        ) -> Optional[str]:
            del command
            return (
                "/mnt/c/Program Files/nodejs/codex.exe"
                if keep_windows_host_paths
                else None
            )

    host_only = WindowsHostOnlyTarget(mode="wsl")
    assert not host_only.command_available("codex")
    assert "Windows-host tool /mnt/c/" in host_only.command_unavailable_reason(
        "codex"
    )
    if platform.system() == "Windows":
        assert native_target.launch_args() == [
            "-e",
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
        ]
    else:
        assert native_target.launch_args() == [
            "-e",
            "bash",
            "--noprofile",
            "--norc",
        ]

    wsl_marker_command = command_with_marker(
        "codex --version", "KETTLE_WSL_MARKER", windows=wsl_target.powershell
    )
    assert "printf" in wsl_marker_command
    assert "Write-Output" not in wsl_marker_command
    assert "KETTLE_WSL_MARKER" not in wsl_marker_command
    assert "sed -n" in first_lines_command(
        "claude --print --help", windows=wsl_target.powershell
    )
    assert "Select-Object" not in first_lines_command(
        "claude --print --help", windows=wsl_target.powershell
    )
    assert "printf" in prompt_marker_command(
        "KETTLE_WSL_PROMPT", windows=wsl_target.powershell
    )
    wsl_auth = agent_auth_command(
        "codex",
        marker,
        output_marker,
        done_marker,
        windows=wsl_target.powershell,
    )
    assert "mktemp" in wsl_auth
    assert "GetTempFileName" not in wsl_auth

    sandbox_marker = "KETTLE_NVIM_SANDBOX_READY"
    sandbox_path = "/tmp/kettle-agent-tui-A1b2C3d4E5"
    wsl_target.validate_wsl_sandbox_path(sandbox_path)
    wsl_sandbox = wsl_target.nvim_sandbox_setup_command(
        sandbox_marker, sandbox_path=sandbox_path
    )
    assert "cp -aL" not in wsl_sandbox
    assert "find -L" in wsl_sandbox
    assert "-printf '%y\\0%s\\0%p\\0'" in wsl_sandbox
    assert "stat -Lc" not in wsl_sandbox
    assert "tar --null" in wsl_sandbox
    assert "head -c" in wsl_sandbox
    assert "KETTLE_COPY_ACTUAL_BYTES" in wsl_sandbox
    assert "snapshot rejects non-regular entry" in wsl_sandbox
    assert str(NVIM_SNAPSHOT_MAX_ENTRIES) in wsl_sandbox
    assert str(NVIM_SNAPSHOT_MAX_BYTES) in wsl_sandbox
    assert str(NVIM_SNAPSHOT_MAX_FILE_BYTES) in wsl_sandbox
    assert str(NVIM_SNAPSHOT_MAX_DEPTH) in wsl_sandbox
    assert "/home/test/.config/nvim" in wsl_sandbox
    assert "${XDG_DATA_HOME:-$HOME/.local/share}/nvim" in wsl_sandbox
    assert sandbox_path in wsl_sandbox
    assert "mktemp" not in wsl_sandbox
    assert 'HOME="$KETTLE_SMOKE_ROOT/home"' in wsl_sandbox
    assert 'XDG_CONFIG_HOME="$KETTLE_SMOKE_ROOT/config"' in wsl_sandbox
    assert 'XDG_DATA_HOME="$KETTLE_SMOKE_ROOT/data"' in wsl_sandbox
    assert "KETTLE_SMOKE_ROOT LANG=C LC_ALL=C" in wsl_sandbox
    assert ".bashrc" not in wsl_sandbox
    assert ".zshrc" not in wsl_sandbox
    assert sandbox_marker not in wsl_sandbox
    tilde_target = AgentShellTarget(
        mode="wsl",
        astro_config="~/.config/nvim",
        nvim_data="~/.local/share/nvim",
    )
    tilde_sandbox = tilde_target.nvim_sandbox_setup_command(
        sandbox_marker, sandbox_path="/tmp/kettle-agent-tui-tilde"
    )
    assert 'KETTLE_NVIM_SOURCE="$HOME"/.config/nvim' in tilde_sandbox
    assert 'KETTLE_NVIM_DATA_SOURCE="$HOME"/.local/share/nvim' in tilde_sandbox
    cleanup_marker = "KETTLE_NVIM_SANDBOX_CLEAN"
    cleanup_command = wsl_target.nvim_sandbox_release_command(cleanup_marker)
    assert "rm -rf --" not in cleanup_command
    assert "Remove-Item" not in cleanup_command
    assert cleanup_marker not in cleanup_command
    try:
        wsl_target.cleanup_nvim_sandbox_host("/tmp/not-a-kettle-sandbox")
    except ValueError:
        pass
    else:
        raise AssertionError("unsafe WSL cleanup path must be rejected")
    try:
        wsl_target.terminate_nvim_sandbox_host("/tmp/not-a-kettle-sandbox")
    except ValueError:
        pass
    else:
        raise AssertionError("unsafe WSL process target must be rejected")
    wsl_cleanup_code = wsl_target.wsl_pidfd_cleanup_code()
    compile(wsl_cleanup_code, "<wsl-pidfd-cleanup>", "exec")
    assert "pidfd_open" in wsl_cleanup_code
    assert "pidfd_send_signal" in wsl_cleanup_code
    assert "XDG_CONFIG_HOME=" in wsl_cleanup_code
    assert "scope=='pidfile'" in wsl_cleanup_code
    assert "/comm" not in wsl_cleanup_code
    assert "kill -TERM" not in wsl_cleanup_code
    if platform.system() == "Linux":
        import socket

        with tempfile.TemporaryDirectory(
            prefix="kettle-wsl-pid-record-"
        ) as temporary:
            fixture_root = Path(temporary)
            run_root = fixture_root / "run"
            run_root.mkdir()
            record = run_root / "nvim.pid"

            def require_rejected_record(kind: str) -> None:
                started = time.monotonic()
                rejected = subprocess.run(
                    [sys.executable, "-c", wsl_cleanup_code, str(fixture_root), "pidfile"],
                    capture_output=True,
                    text=True,
                    timeout=2,
                    check=False,
                )
                assert rejected.returncode != 0, (
                    f"a {kind} WSL PID record was accepted: {rejected.stdout!r}"
                )
                assert time.monotonic() - started < 2, (
                    f"a {kind} WSL PID record blocked before rejection"
                )

            os.mkfifo(record, 0o600)
            require_rejected_record("FIFO")
            record.unlink()

            target = fixture_root / "target.pid"
            target.write_text(f"{os.getpid()}\n", encoding="ascii")
            record.symlink_to(target)
            require_rejected_record("symlink")
            record.unlink()

            endpoint = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                endpoint.bind(str(record))
                require_rejected_record("Unix socket")
            finally:
                endpoint.close()
                record.unlink(missing_ok=True)
    compile(wsl_target.bounded_identity_code(), "<wsl-bounded-identity>", "exec")

    class CompileOnlyWslTarget(AgentShellTarget):
        def __init__(self) -> None:
            super().__init__(mode="wsl", wsl_distro="CompileFixture")
            self.calls = 0

        def run_command(
            self, argv: List[str], *, timeout: float
        ) -> subprocess.CompletedProcess:
            self.calls += 1
            return subprocess.CompletedProcess(argv, 0, "", "")

    compile_only_wsl = CompileOnlyWslTarget()
    compile_only_wsl.require_wsl_pidfd_cleanup()
    assert compile_only_wsl.calls == 2, (
        "the assembled WSL pidfd exercise was not reached after its API probe"
    )
    compile(
        AgentShellTarget.sandbox_marker_wait_code(),
        "<wsl-sandbox-marker-wait>",
        "exec",
    )

    # Native snapshots are created with an unpredictable tempfile name and
    # dereference both a symlinked config root and nested file links when the
    # host permits creating them. The copied tree may never retain a route back
    # to live configuration.
    with tempfile.TemporaryDirectory(prefix="kettle-nvim-fixture-") as fixture:
        fixture_root = Path(fixture)
        real_config = fixture_root / "real-config"
        real_config.mkdir()
        (real_config / "init.lua").write_text(
            "-- isolated fixture\n", encoding="utf-8"
        )
        external = fixture_root / "external.lua"
        external.write_text("-- external fixture\n", encoding="utf-8")
        nested_link_created = False
        try:
            (real_config / "linked.lua").symlink_to(external)
            nested_link_created = True
        except (NotImplementedError, OSError):
            pass

        config_source = fixture_root / "config-link"
        root_link_created = False
        try:
            config_source.symlink_to(real_config, target_is_directory=True)
            root_link_created = True
        except (NotImplementedError, OSError):
            config_source = real_config

        data_source = fixture_root / "nvim-data"
        (data_source / "lazy" / "fixture").mkdir(parents=True)
        (data_source / "lazy" / "fixture" / "plugin.lua").write_text(
            "-- plugin fixture\n", encoding="utf-8"
        )
        lazyvcs_module = (
            data_source
            / "lazy"
            / "lazyvcs.nvim"
            / "lua"
            / "lazyvcs"
            / "source_control"
            / "native.lua"
        )
        lazyvcs_module.parent.mkdir(parents=True)
        lazyvcs_module.write_text("return {}\n", encoding="utf-8")
        snapshot_target = AgentShellTarget(
            mode="native",
            astro_config=str(config_source),
            nvim_data=str(data_source),
        )
        snapshot_subreaper = (
            LinuxSubreaperScope.acquire()
            if platform.system() == "Linux"
            else None
        )
        native_sandbox_path = snapshot_target.create_nvim_sandbox_host()
        snapshot_cleanup_job = WindowsKillJob() if os.name == "nt" else None
        runtime_link_created = False
        try:
            snapshot_target.prepare_nvim_sandbox_host(native_sandbox_path)
            native_root = Path(native_sandbox_path)
            assert native_root.name.startswith("kettle-agent-tui-")
            assert (native_root / "config" / "nvim" / "init.lua").is_file()
            ready_record = native_root / "run" / "selftest-ready"
            ready_record.write_text("SELFTEST_READY\n", encoding="ascii")
            snapshot_target.wait_for_nvim_sandbox_marker(
                native_sandbox_path,
                "selftest-ready",
                "SELFTEST_READY",
                timeout_s=0.1,
            )
            linked_ready = native_root / "run" / "linked-ready"
            try:
                linked_ready.symlink_to(external)
            except (NotImplementedError, OSError):
                pass
            else:
                try:
                    snapshot_target.wait_for_nvim_sandbox_marker(
                        native_sandbox_path,
                        "linked-ready",
                        "SELFTEST_READY",
                        timeout_s=0.1,
                    )
                except RuntimeError as error:
                    assert "small regular identity record" in str(error)
                else:
                    raise AssertionError("a linked readiness marker was accepted")
            assert (
                native_root
                / "data"
                / "nvim"
                / "lazy"
                / "fixture"
                / "plugin.lua"
            ).is_file()
            copied_lazyvcs_module = (
                native_root
                / "data"
                / "nvim"
                / "lazy"
                / "lazyvcs.nvim"
                / "lua"
                / "lazyvcs"
                / "source_control"
                / "native.lua"
            )
            (native_root / "run" / "lazyvcs-loaded-source").write_text(
                str(copied_lazyvcs_module) + "\n", encoding="utf-8"
            )
            loaded_identity = snapshot_target.lazyvcs_loaded_source_identity(
                native_sandbox_path
            )
            assert loaded_identity["module_relative"] == (
                "lua/lazyvcs/source_control/native.lua"
            )
            assert loaded_identity["module_file"]["sha256"]
            (native_root / "run" / "lazyvcs-loaded-source").write_text(
                str(external) + "\n", encoding="utf-8"
            )
            try:
                snapshot_target.lazyvcs_loaded_source_identity(
                    native_sandbox_path
                )
            except RuntimeError as error:
                assert "outside its snapshot" in str(error)
            else:
                raise AssertionError(
                    "a LazyVCS module outside the copied tree must be rejected"
                )
            if root_link_created:
                assert not snapshot_target.path_is_link(
                    native_root / "config" / "nvim"
                )
            if nested_link_created:
                copied_link = native_root / "config" / "nvim" / "linked.lua"
                assert copied_link.read_text(encoding="utf-8") == (
                    "-- external fixture\n"
                )
                assert not snapshot_target.path_is_link(copied_link)
            native_setup = snapshot_target.nvim_sandbox_setup_command(
                sandbox_marker, sandbox_path=native_sandbox_path
            )
            assert "HOME" in native_setup
            assert "XDG_CONFIG_HOME" in native_setup
            assert "Copy-Item" not in native_setup
            readonly_cleanup_fixture = native_root / "read-only-cleanup-fixture"
            readonly_cleanup_fixture.write_text("fixture\n", encoding="utf-8")
            readonly_cleanup_dir = native_root / "read-only-cleanup-directory"
            readonly_cleanup_dir.mkdir()
            (readonly_cleanup_dir / "fixture").write_text(
                "fixture\n", encoding="utf-8"
            )
            if platform.system() == "Windows":
                readonly_cleanup_fixture.chmod(stat.S_IREAD)
            else:
                readonly_cleanup_dir.chmod(stat.S_IREAD | stat.S_IWRITE)
            runtime_link = native_root / "runtime-created-link"
            try:
                runtime_link.symlink_to(external)
                runtime_link_created = True
            except (NotImplementedError, OSError):
                pass
        finally:
            try:
                snapshot_target.cleanup_nvim_sandbox_host(
                    native_sandbox_path,
                    windows_job=snapshot_cleanup_job,
                    linux_subreaper=snapshot_subreaper,
                )
            finally:
                if snapshot_subreaper is not None:
                    snapshot_subreaper.close()
        assert not Path(native_sandbox_path).exists()
        if runtime_link_created:
            assert external.read_text(encoding="utf-8") == "-- external fixture\n"

        wait_state = NvimSidebarWaitState("SIDEBAR_MARKER", 0.5)
        assert wait_state.observe("SIDEBAR_MARKER", "frame-a", [0, 0], 0, 0.0) == (
            False,
            False,
        )
        # A changed frame resets the quiet period instead of inheriting the
        # first marker timestamp.
        assert wait_state.observe(
            "SIDEBAR_MARKER changed", "frame-b", [0, 0], 0, 0.4
        ) == (
            False,
            False,
        )
        # Cursor/history changes count as activity even when the rendered text
        # and text-derived snapshot are identical.
        assert wait_state.observe(
            "SIDEBAR_MARKER changed", "frame-b", [0, 1], 0, 0.8
        ) == (
            False,
            False,
        )
        assert wait_state.observe(
            "SIDEBAR_MARKER changed", "frame-b", [0, 1], 0, 1.31
        ) == (
            True,
            False,
        )
        pager = "Press ENTER or type command to continue"
        pager_state = NvimSidebarWaitState("SIDEBAR_MARKER", 0.0)
        assert pager_state.observe(pager, "same-frame", [0, 0], 0, 0.0) == (False, True)
        assert pager_state.observe(pager, "same-frame", [0, 0], 0, 0.1) == (False, False)
        # A second prompt can replace the first without an observable clear
        # frame. A changed prompt and a still-covered prompt after the bounded
        # retry interval both get another Enter.
        assert pager_state.observe(pager + " 2", "second-frame", [0, 0], 0, 0.2) == (
            False,
            True,
        )
        assert pager_state.observe(pager + " 2", "second-frame", [0, 0], 0, 0.8) == (
            False,
            True,
        )
        assert pager_state.observe("cleared", "clear-frame", [0, 0], 0, 0.9) == (
            False,
            False,
        )
        # An identical pager frame after a visible absence is a new prompt and
        # must be dismissed again.
        assert pager_state.observe(pager, "same-frame", [0, 0], 0, 1.0) == (False, True)

        cleanup_probe = LiveKettle("unused", Path("unused"), Path("unused"))
        cleanup_ran: List[bool] = []

        def fail_process_cleanup() -> None:
            raise RuntimeError("intentional termination failure")

        cleanup_probe._terminate_process = fail_process_cleanup  # type: ignore[method-assign]
        cleanup_probe.add_post_exit_cleanup(lambda: cleanup_ran.append(True))
        with contextlib.redirect_stderr(io.StringIO()):
            try:
                cleanup_probe.__exit__(None, None, None)
            except RuntimeError as error:
                assert "intentional termination failure" in str(error)
            else:
                raise AssertionError("a process cleanup failure must still be reported")
        assert cleanup_ran == [True], "post-exit cleanup was skipped after termination failed"

        cleanup_order: List[str] = []

        def record_remove() -> None:
            assert cleanup_order == ["drain"], (
                "sandbox removal ran before descendant drain completed"
            )
            cleanup_order.append("remove")

        _drain_then_remove(lambda: cleanup_order.append("drain"), record_remove)
        assert cleanup_order == ["drain", "remove"]

        # Once Popen succeeds, every startup-probe exception must cross the
        # same cleanup boundary as a timeout. Context-manager __exit__ is never
        # invoked when __enter__ raises, so this is the only owner of the live
        # process and registered temporary paths in that case.
        startup_probe = LiveKettle("unused", Path("unused"), Path("unused"))
        startup_process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            start_new_session=os.name != "nt",
        )
        startup_probe.proc = startup_process
        startup_probe._tracker_owner_pid = startup_process.pid
        startup_cleanup: List[str] = []
        startup_probe.add_post_exit_cleanup(
            lambda: startup_cleanup.append("temporary-path")
        )

        def fail_startup_identity_probe() -> None:
            raise RuntimeError("intentional startup identity uncertainty")

        startup_probe._remember_tracker_sessions = (  # type: ignore[method-assign]
            fail_startup_identity_probe
        )
        try:
            startup_probe._finish_startup()
        except SystemExit as error:
            assert "intentional startup identity uncertainty" in str(error)
        else:
            raise AssertionError("a startup identity failure escaped cleanup")
        startup_process.wait(timeout=3)
        assert startup_cleanup == ["temporary-path"]

        interrupted_probe = LiveKettle("unused", Path("unused"), Path("unused"))
        interrupted_process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            start_new_session=os.name != "nt",
        )
        interrupted_probe.proc = interrupted_process
        interrupted_probe._tracker_owner_pid = interrupted_process.pid
        interrupt_cleanup: List[str] = []
        interrupted_probe.add_post_exit_cleanup(
            lambda: interrupt_cleanup.append("temporary-path")
        )

        def interrupt_control_wait() -> LiveKettle:
            raise KeyboardInterrupt

        interrupted_probe._await_control_server = (  # type: ignore[method-assign]
            interrupt_control_wait
        )
        try:
            interrupted_probe._finish_startup()
        except KeyboardInterrupt:
            pass
        else:
            raise AssertionError("an interrupted __enter__ escaped owned cleanup")
        interrupted_process.wait(timeout=3)
        assert interrupt_cleanup == ["temporary-path"]

        # Ownership starts at mkdir, not after the wrapper has been written.
        # Inject that exact startup failure and prove the registered cleanup
        # removes the otherwise stranded owner-private directory.
        if os.name != "nt":
            failed_wrapper = LiveKettle("unused", Path("unused"), Path("unused"))
            real_path_write_text = Path.write_text
            failed_wrapper_roots: List[Path] = []

            def reject_wrapper_write(
                path: Path, *_args: object, **_kwargs: object
            ) -> int:
                if path.parent.name.startswith("kettle-live-ui-shell-"):
                    failed_wrapper_roots.append(path.parent)
                    raise OSError("intentional PTY wrapper write failure")
                return real_path_write_text(  # type: ignore[arg-type]
                    path, *_args, **_kwargs
                )

            Path.write_text = reject_wrapper_write  # type: ignore[assignment]
            try:
                try:
                    failed_wrapper._prepare_unix_pty_tracker(["unused"])
                except OSError as error:
                    assert "intentional PTY wrapper write failure" in str(error)
                else:
                    raise AssertionError("the PTY wrapper write failure did not fire")
            finally:
                Path.write_text = real_path_write_text  # type: ignore[assignment]
            assert (
                len(failed_wrapper_roots) == 1 and failed_wrapper_roots[0].exists()
            )
            failed_wrapper._run_post_exit_cleanups()
            assert not failed_wrapper_roots[0].exists(), (
                "a PTY tracker startup failure stranded its private directory"
            )

        class OwnedSandboxFixture:
            powershell = True

            def __init__(self) -> None:
                self.created = False
                self.cleaned = False
                self.path: Optional[Path] = None

            def create_nvim_sandbox_host(self) -> str:
                self.created = True
                self.path = Path(tempfile.mkdtemp(prefix="kettle-agent-tui-"))
                return str(self.path)

            def cleanup_nvim_sandbox_host(
                self,
                path: str,
                *,
                windows_job: Optional[object] = None,
                linux_subreaper: Optional[LinuxSubreaperScope] = None,
            ) -> None:
                self.cleaned = True
                assert linux_subreaper is None
                if windows_job is not None:
                    windows_job.close()
                shutil.rmtree(path)

        job_failure_target = OwnedSandboxFixture()

        def reject_job_creation(**_kwargs: object) -> WindowsKillJob:
            raise OSError("intentional Job construction failure")

        try:
            _create_owned_nvim_sandbox(  # type: ignore[arg-type]
                job_failure_target,
                lambda _cleanup: None,
                reject_job_creation,
            )
        except OSError as error:
            assert "intentional Job construction failure" in str(error)
        else:
            raise AssertionError("the injected Job construction failure did not fire")
        assert not job_failure_target.created, (
            "the Neovim sandbox was created before Windows containment existed"
        )

        class OwnedSandboxJobFixture:
            def __init__(self, **_kwargs: object) -> None:
                self.closed = False

            def close(self) -> None:
                self.closed = True

        class FailingSandboxFixture(OwnedSandboxFixture):
            def create_nvim_sandbox_host(self) -> str:
                self.created = True
                raise OSError("intentional sandbox creation failure")

        sandbox_failure_jobs: List[OwnedSandboxJobFixture] = []

        def make_sandbox_failure_job(**kwargs: object) -> OwnedSandboxJobFixture:
            job = OwnedSandboxJobFixture(**kwargs)
            sandbox_failure_jobs.append(job)
            return job

        try:
            _create_owned_nvim_sandbox(  # type: ignore[arg-type]
                FailingSandboxFixture(),
                lambda _cleanup: None,
                make_sandbox_failure_job,  # type: ignore[arg-type]
            )
        except OSError as error:
            assert "intentional sandbox creation failure" in str(error)
        else:
            raise AssertionError("the injected sandbox creation failure did not fire")
        assert len(sandbox_failure_jobs) == 1 and sandbox_failure_jobs[0].closed, (
            "a sandbox creation failure leaked its already-created Windows Job"
        )

        class FailingNativeSandboxFixture:
            powershell = False
            mode = "native"

            @staticmethod
            def create_nvim_sandbox_host() -> str:
                raise OSError("intentional native sandbox creation failure")

        class FailingSubreaperFixture:
            @staticmethod
            def close() -> None:
                raise OSError("intentional subreaper rollback failure")

        original_platform_system = platform.system
        original_subreaper_acquire = LinuxSubreaperScope.__dict__["acquire"]
        platform.system = lambda: "Linux"  # type: ignore[assignment]
        LinuxSubreaperScope.acquire = classmethod(  # type: ignore[method-assign]
            lambda _cls: FailingSubreaperFixture()
        )
        try:
            try:
                _create_owned_nvim_sandbox(  # type: ignore[arg-type]
                    FailingNativeSandboxFixture(), lambda _cleanup: None
                )
            except RuntimeError as error:
                assert "intentional native sandbox creation failure" in str(error)
                assert "intentional subreaper rollback failure" in str(error)
            else:
                raise AssertionError("a failed subreaper rollback was suppressed")
        finally:
            platform.system = original_platform_system  # type: ignore[assignment]
            LinuxSubreaperScope.acquire = original_subreaper_acquire

        registration_target = OwnedSandboxFixture()
        registration_jobs: List[OwnedSandboxJobFixture] = []

        def make_fixture_job(**kwargs: object) -> OwnedSandboxJobFixture:
            job = OwnedSandboxJobFixture(**kwargs)
            registration_jobs.append(job)
            return job

        def reject_cleanup_registration(_cleanup: Callable[[], None]) -> None:
            raise RuntimeError("intentional cleanup registration failure")

        try:
            _create_owned_nvim_sandbox(  # type: ignore[arg-type]
                registration_target,
                reject_cleanup_registration,
                make_fixture_job,  # type: ignore[arg-type]
            )
        except RuntimeError as error:
            assert "intentional cleanup registration failure" in str(error)
        else:
            raise AssertionError("the injected cleanup registration failure did not fire")
        assert registration_target.created and registration_target.cleaned
        assert registration_target.path is not None
        assert not registration_target.path.exists()
        assert len(registration_jobs) == 1 and registration_jobs[0].closed

        if platform.system() != "Windows":
            import pty

            tracker = LiveKettle("unused", Path("unused"), Path("unused"))
            tracker._tracker_owner_pid = os.getpid()
            tracked_argv, tracked_env = tracker._prepare_unix_pty_tracker(
                [
                    "unused",
                    "-e",
                    sys.executable,
                    "-c",
                    "import time; time.sleep(60)",
                ]
            )
            execute_at = tracked_argv.index("-e")
            with tempfile.TemporaryDirectory(
                prefix="kettle-pty-python-startup-"
            ) as hook_fixture:
                hook_root = Path(hook_fixture)
                hook_marker = hook_root / "sitecustomize-ran"
                (hook_root / "sitecustomize.py").write_text(
                    "import os,pathlib\n"
                    "pathlib.Path(os.environ['KETTLE_PTY_SITE_MARKER']).write_text('ran')\n",
                    encoding="utf-8",
                )
                hook_env = dict(tracked_env or os.environ)
                hook_env["PYTHONPATH"] = str(hook_root)
                hook_env["KETTLE_PTY_SITE_MARKER"] = str(hook_marker)
                control = subprocess.run(
                    [sys.executable, "-c", "pass"],
                    env=hook_env,
                    timeout=5,
                    check=False,
                )
                assert control.returncode == 0 and hook_marker.is_file()
                hook_marker.unlink()
                isolated_wrapper = subprocess.run(
                    [tracked_argv[execute_at + 1], "/usr/bin/true"],
                    env=hook_env,
                    start_new_session=True,
                    timeout=5,
                    check=False,
                )
                assert isolated_wrapper.returncode == 0
                assert not hook_marker.exists(), (
                    "the PTY ownership wrapper ran user Python startup code "
                    "before recording its session"
                )
                # A tty descriptor can be inherited after setsid without being
                # this process's controlling terminal. `isatty(0)` remains
                # true, but tcsetpgrp must not be attempted. This is distinct
                # from the real controlling-terminal handoff exercised below.
                unowned_master, unowned_slave = pty.openpty()
                try:
                    unowned_tty = subprocess.run(
                        [tracked_argv[execute_at + 1], "/usr/bin/true"],
                        env=tracked_env,
                        stdin=unowned_slave,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.PIPE,
                        start_new_session=True,
                        timeout=5,
                        check=False,
                        text=True,
                    )
                finally:
                    os.close(unowned_slave)
                    os.close(unowned_master)
                assert unowned_tty.returncode == 0, unowned_tty.stderr
            payload_env = dict(tracked_env or os.environ)
            payload_env["KETTLE_SMOKE_TEST_REJECT_TCSETPGRP"] = (
                "payload-must-not-see-this"
            )
            payload_environment = subprocess.run(
                [
                    tracked_argv[execute_at + 1],
                    sys.executable,
                    "-c",
                    (
                        "import json,os; print(json.dumps({name: os.environ.get(name) "
                        "for name in ('SHELL','KETTLE_SMOKE_PTY_SESSIONS',"
                        "'KETTLE_SMOKE_REAL_SHELL','KETTLE_SMOKE_TEST_REJECT_TCSETPGRP')}))"
                    ),
                ],
                env=payload_env,
                start_new_session=True,
                timeout=3,
                check=False,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert payload_environment.returncode == 0, payload_environment.stderr
            observed_payload_env = json.loads(payload_environment.stdout)
            assert observed_payload_env == {
                "SHELL": tracked_env["KETTLE_SMOKE_REAL_SHELL"],
                "KETTLE_SMOKE_PTY_SESSIONS": None,
                "KETTLE_SMOKE_REAL_SHELL": None,
                "KETTLE_SMOKE_TEST_REJECT_TCSETPGRP": None,
            }, observed_payload_env

            record_probe = LiveKettle("unused", Path("unused"), Path("unused"))
            _record_argv, record_env = record_probe._prepare_unix_pty_tracker(
                ["unused"]
            )
            assert record_env is not None
            assert record_probe._pty_session_file is not None
            retained_records: List[int] = []

            class RecordProbeHandle:
                def __init__(self, pid: int) -> None:
                    self.pid = pid
                    self.identity = (pid,)
                    self.closed = False

                @staticmethod
                def matches_current() -> bool:
                    return True

                def close(self) -> None:
                    self.closed = True

            def remember_probe_record(
                session: int, *, deadline: Optional[float] = None
            ) -> RecordProbeHandle:
                assert deadline is not None
                retained_records.append(session)
                return RecordProbeHandle(session)

            record_probe._open_owned_tracker_session = remember_probe_record  # type: ignore[method-assign]
            record_probe._pty_session_file.write_bytes(b"malformed\n123\n")
            try:
                record_probe._remember_tracker_sessions()
            except RuntimeError as error:
                assert "invalid PTY session ownership record at line 1" in str(error)
            else:
                raise AssertionError("a malformed PTY tracker record was accepted")
            assert retained_records == [123], (
                "a malformed record prevented a later valid session from being retained"
            )
            retained_probe_handle = record_probe._tracker_sessions.get(123)
            assert isinstance(retained_probe_handle, RecordProbeHandle), (
                "a later valid tracker record was checked but not published"
            )
            record_probe._tracker_sessions.clear()
            record_probe._pty_sessions.clear()
            retained_probe_handle.close()

            retained_records.clear()
            record_probe._pty_session_file.write_bytes(b"321\n" * 4096)
            record_probe._open_owned_tracker_session = (  # type: ignore[method-assign]
                lambda session, *, deadline=None: (
                    retained_records.append(session), None
                )[1]
            )
            record_probe._remember_tracker_sessions()
            assert retained_records == [321], (
                "duplicate tracker records repeated the ownership probe"
            )

            retained_records.clear()
            record_probe._pty_session_file.write_bytes(b"654\n")
            try:
                record_probe._remember_tracker_sessions(deadline=time.monotonic())
            except RuntimeError as error:
                assert "scan exceeded its time limit" in str(error)
            else:
                raise AssertionError("the PTY tracker ignored its absolute deadline")
            assert not retained_records, (
                "an expired tracker budget still started an ownership probe"
            )

            record_probe._pty_session_file.write_bytes(b"2\n" * 4097)
            try:
                record_probe._remember_tracker_sessions()
            except RuntimeError as error:
                assert f"more than {PTY_TRACKER_MAX_RECORDS} records" in str(error)
            else:
                raise AssertionError("the PTY tracker record-count limit was ignored")

            retained_records.clear()
            record_probe._open_owned_tracker_session = remember_probe_record  # type: ignore[method-assign]
            record_probe._pty_session_file.write_bytes(
                b"1\n2147483648\n2147483647\n789"
            )
            try:
                record_probe._remember_tracker_sessions()
            except RuntimeError as error:
                detail = str(error)
                assert "not a valid child PID: 1" in detail
                assert "exceeds the pid_t limit: 2147483648" in detail
                assert "ends with an incomplete line" in detail
            else:
                raise AssertionError("invalid or incomplete PTY records were accepted")
            assert retained_records == [PTY_TRACKER_MAX_PID]
            assert PTY_TRACKER_MAX_PID in record_probe._tracker_sessions
            record_probe._tracker_sessions.clear()
            record_probe._pty_sessions.clear()

            record_probe._pty_session_file.write_bytes(
                b"1\n" * (PTY_TRACKER_MAX_BYTES // 2 + 1)
            )
            try:
                record_probe._remember_tracker_sessions()
            except RuntimeError as error:
                assert f"exceeds {PTY_TRACKER_MAX_BYTES} bytes" in str(error)
            else:
                raise AssertionError("an oversized PTY tracker record was accepted")

            record_target = record_probe._pty_session_file.with_name("replacement")
            record_target.write_bytes(b"456\n")
            record_probe._pty_session_file.unlink()
            record_probe._pty_session_file.symlink_to(record_target)
            try:
                record_probe._remember_tracker_sessions()
            except RuntimeError as error:
                assert "not the retained private file" in str(error)
            else:
                raise AssertionError("a replaced PTY tracker pathname was accepted")
            assert record_target.read_bytes() == b"456\n"
            replaced_writer = subprocess.run(
                [record_env["SHELL"], "/usr/bin/true"],
                env=record_env,
                start_new_session=True,
                timeout=3,
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            assert replaced_writer.returncode != 0, (
                "the PTY wrapper followed a replaced tracker pathname"
            )
            assert record_target.read_bytes() == b"456\n"
            assert not record_probe._run_post_exit_cleanups()
            ordinary_exit = subprocess.run(
                [
                    tracked_argv[execute_at + 1],
                    sys.executable,
                    "-c",
                    "raise SystemExit(7)",
                ],
                env=tracked_env,
                start_new_session=True,
                timeout=3,
                check=False,
            )
            assert ordinary_exit.returncode == 7, (
                "the PTY tracker hid ordinary payload completion/status: "
                f"{ordinary_exit.returncode}"
            )
            signalled_exit = subprocess.run(
                [
                    tracked_argv[execute_at + 1],
                    sys.executable,
                    "-c",
                    "import os,signal; os.kill(os.getpid(),signal.SIGTERM)",
                ],
                env=tracked_env,
                start_new_session=True,
                timeout=3,
                check=False,
            )
            assert signalled_exit.returncode == -signal.SIGTERM, (
                "the PTY tracker converted signal termination into an exit code: "
                f"{signalled_exit.returncode}"
            )
            # Use a real controlling terminal.  The old wrapper restored
            # SIGTTOU in the child and let parent/child race tcsetpgrp; the
            # losing child stopped forever before exec.  A synchronized parent
            # handoff must reliably make the payload group foreground.
            import fcntl
            import select
            import termios

            pty_launcher = (
                "import fcntl,os,sys,termios\n"
                "slave=int(sys.argv[1])\n"
                "os.setsid()\n"
                "fcntl.ioctl(slave,termios.TIOCSCTTY,0)\n"
                "[os.dup2(slave,fd) for fd in (0,1,2)]\n"
                "os.close(slave) if slave>2 else None\n"
                "os.execv(sys.argv[2],sys.argv[2:])\n"
            )
            pty_payload = (
                "import os; "
                "assert os.tcgetpgrp(0)==os.getpgrp(); "
                "print('KETTLE_PTY_HANDOFF_OK',flush=True)"
            )
            for _attempt in range(8):
                master_fd, slave_fd = pty.openpty()
                controlling = subprocess.Popen(
                    [
                        sys.executable,
                        "-c",
                        pty_launcher,
                        str(slave_fd),
                        tracked_argv[execute_at + 1],
                        sys.executable,
                        "-c",
                        pty_payload,
                    ],
                    env=tracked_env,
                    close_fds=True,
                    pass_fds=(slave_fd,),
                )
                os.close(slave_fd)
                pty_output = bytearray()
                try:
                    pty_deadline = time.monotonic() + 5
                    while time.monotonic() < pty_deadline:
                        readable, _, _ = select.select([master_fd], [], [], 0.05)
                        if readable:
                            try:
                                chunk = os.read(master_fd, 4096)
                            except OSError as error:
                                if error.errno != errno.EIO:
                                    raise
                                break
                            if not chunk:
                                break
                            pty_output.extend(chunk)
                        # Process exit and PTY output publication are not one
                        # event. Linux can report the former before the final
                        # bytes (or terminal EIO) become readable, so keep
                        # draining the master until EOF/EIO or the deadline.
                    controlling.wait(timeout=1)
                finally:
                    os.close(master_fd)
                    if controlling.returncode is None:
                        controlling.kill()
                        controlling.wait(timeout=3)
                assert controlling.returncode == 0, pty_output.decode(
                    "utf-8", errors="replace"
                )
                assert b"KETTLE_PTY_HANDOFF_OK" in pty_output

            # A failed foreground handoff must not release the child barrier.
            # Otherwise the restored SIGTTIN stops the background payload and
            # leaves the session leader blocked forever in waitpid.
            master_fd, slave_fd = pty.openpty()
            rejected_env = dict(tracked_env or os.environ)
            rejected_env["KETTLE_SMOKE_TEST_REJECT_TCSETPGRP"] = "1"
            rejected_handoff = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    pty_launcher,
                    str(slave_fd),
                    tracked_argv[execute_at + 1],
                    sys.executable,
                    "-c",
                    "print('KETTLE_PTY_HANDOFF_MUST_NOT_RUN',flush=True)",
                ],
                env=rejected_env,
                close_fds=True,
                pass_fds=(slave_fd,),
            )
            os.close(slave_fd)
            rejected_output = bytearray()
            try:
                rejected_deadline = time.monotonic() + 5
                while time.monotonic() < rejected_deadline:
                    readable, _, _ = select.select([master_fd], [], [], 0.05)
                    if readable:
                        try:
                            chunk = os.read(master_fd, 4096)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            break
                        if not chunk:
                            break
                        rejected_output.extend(chunk)
                rejected_handoff.wait(timeout=1)
            finally:
                os.close(master_fd)
                if rejected_handoff.returncode is None:
                    rejected_handoff.kill()
                    rejected_handoff.wait(timeout=3)
            assert rejected_handoff.returncode != 0
            assert b"KETTLE_PTY_HANDOFF_MUST_NOT_RUN" not in rejected_output

            # A pane can finish portable-pty's setsid after the first tracker
            # snapshot. Inventory is the injected race point: the new wrapper
            # becomes visible only after that first read. Freezing Kettle and
            # draining the tracker again must retain and terminate it before
            # the outer process group is killed.
            race_probe = LiveKettle("unused", Path("unused"), Path("unused"))
            race_probe.proc = object()  # type: ignore[assignment]
            race_events: List[str] = []
            race_state = {"inventory": False, "frozen": False}

            class RaceProbeHandle:
                pid = 4242
                identity = (4242,)

                @staticmethod
                def matches_current() -> bool:
                    return True

                @staticmethod
                def close() -> None:
                    race_events.append("close")

            def remember_raced_session(*, deadline: Optional[float] = None) -> None:
                assert deadline is None or deadline > time.monotonic()
                race_events.append("tracker")
                if race_state["inventory"] and race_state["frozen"]:
                    race_probe._tracker_sessions.setdefault(4242, RaceProbeHandle())  # type: ignore[arg-type]
                    race_probe._pty_sessions.add(4242)

            def inject_append_during_inventory(*, timeout: float) -> str:
                assert timeout == 1
                race_events.append("inventory")
                race_state["inventory"] = True
                return json.dumps({"panes": []})

            def freeze_race_owner() -> None:
                race_events.append("freeze")
                race_state["frozen"] = True

            race_probe._remember_tracker_sessions = remember_raced_session  # type: ignore[method-assign]
            race_probe._control_pty_inventory = inject_append_during_inventory  # type: ignore[method-assign]
            race_probe._freeze_outer_process_group = freeze_race_owner  # type: ignore[method-assign]
            race_probe._remember_direct_child_sessions = (  # type: ignore[method-assign]
                lambda *, deadline: race_events.append("direct")
            )
            original_terminate_session = globals()["terminate_owned_pty_session"]
            original_terminate_group = globals()["terminate_owned_process_group"]
            terminated_race_sessions: List[int] = []
            globals()["terminate_owned_pty_session"] = (
                lambda handle: terminated_race_sessions.append(handle.pid)
            )
            globals()["terminate_owned_process_group"] = (
                lambda _process: race_events.append("outer")
            )
            try:
                race_probe._terminate_process()
            finally:
                globals()["terminate_owned_pty_session"] = original_terminate_session
                globals()["terminate_owned_process_group"] = original_terminate_group
            assert terminated_race_sessions == [4242], (
                "a PTY appended after the first snapshot escaped final cleanup"
            )
            assert race_events[:5] == [
                "tracker",
                "inventory",
                "freeze",
                "tracker",
                "direct",
            ], race_events
            assert race_events[-2:] == ["close", "outer"], race_events

            tracked = subprocess.Popen(
                tracked_argv[execute_at + 1 :],
                env=tracked_env,
                start_new_session=True,
            )
            tracked_late: Optional[subprocess.Popen] = None
            anchored_exit: Optional[subprocess.Popen] = None
            anchored_descendant: Optional[int] = None
            anchored_descendant_handle: Optional[StableProcessHandle] = None
            try:
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    tracker._remember_tracker_sessions()
                    if tracked.pid in tracker._pty_sessions:
                        break
                    time.sleep(0.01)
                assert tracked.pid in tracker._pty_sessions, (
                    "the PTY wrapper did not anchor its session before payload startup"
                )
                stale_identity = tracker._tracker_sessions[tracked.pid]
                stale_identity.matches_current = lambda: False  # type: ignore[method-assign]
                tracker._remember_tracker_sessions()
                replacement_identity = tracker._tracker_sessions.get(tracked.pid)
                assert replacement_identity is not None
                assert replacement_identity is not stale_identity, (
                    "a reused tracker PID was forgotten instead of reopened in the same pass"
                )
                assert replacement_identity.matches_current(), (
                    "the tracker PID replacement was not independently revalidated"
                )
                # A live wrapper reported by the owner-private session file
                # must never disappear merely because stable-handle retention
                # failed. Releasing the already-held handle recreates the
                # first-observation path; the injected refusal must fail closed,
                # then a normal retry must retain the same live wrapper.
                retained = tracker._tracker_sessions.pop(tracked.pid)
                tracker._pty_sessions.discard(tracked.pid)
                retained.close()
                original_open = StableProcessHandle.__dict__["open"]

                def reject_live_tracker_handle(
                    cls: type[StableProcessHandle], pid: int
                ) -> StableProcessHandle:
                    if pid == tracked.pid:
                        raise OSError("intentional live tracker handle failure")
                    return original_open.__func__(cls, pid)

                StableProcessHandle.open = classmethod(reject_live_tracker_handle)
                try:
                    try:
                        tracker._remember_tracker_sessions()
                    except RuntimeError as error:
                        assert "could not retain live owned PTY session" in str(error)
                    else:
                        raise AssertionError(
                            "a live PTY wrapper with no stable handle looked absent"
                        )
                    assert tracked.poll() is None
                    assert tracked.pid not in tracker._tracker_sessions
                finally:
                    StableProcessHandle.open = original_open
                tracker._remember_tracker_sessions()
                assert tracked.pid in tracker._tracker_sessions

                # A retained process whose identity cannot be re-opened is not
                # dead. The stable handle must remain owned while the probe
                # fails closed, so startup/exit cleanup can still signal that
                # exact process instance after the transient error clears.
                StableProcessHandle.open = classmethod(reject_live_tracker_handle)
                try:
                    try:
                        tracker._remember_tracker_sessions()
                    except RuntimeError as error:
                        assert "could not verify retained process" in str(error)
                    else:
                        raise AssertionError(
                            "an unverifiable retained PTY wrapper looked absent"
                        )
                    assert tracked.pid in tracker._tracker_sessions
                finally:
                    StableProcessHandle.open = original_open

                # Failure of the independent parent/session query is also
                # uncertainty. With no stable handle available it must abort,
                # never turn a live direct child into a stale-record skip.
                retained = tracker._tracker_sessions.pop(tracked.pid)
                tracker._pty_sessions.discard(tracked.pid)
                retained.close()
                original_run = globals()["run"]

                def fail_live_parent_probe(
                    _argv: List[str], **_kwargs: object
                ) -> subprocess.CompletedProcess:
                    return subprocess.CompletedProcess([], 1, "", "intentional ps failure")

                StableProcessHandle.open = classmethod(reject_live_tracker_handle)
                globals()["run"] = fail_live_parent_probe
                try:
                    try:
                        tracker._remember_tracker_sessions()
                    except RuntimeError as error:
                        assert "could not retain or verify reported PTY session" in str(
                            error
                        )
                    else:
                        raise AssertionError(
                            "a live PTY wrapper with an uncertain parent looked absent"
                        )
                finally:
                    globals()["run"] = original_run
                    StableProcessHandle.open = original_open
                tracker._remember_tracker_sessions()
                assert tracked.pid in tracker._tracker_sessions
                tracker._remember_pty_sessions(
                    json.dumps({"panes": [{"id": 7, "child_pid": tracked.pid}]})
                )
                # The append-only tracker must remain active after the first
                # successful control inventory; later panes are the exact case
                # the old `_control_inventory_seen` shortcut discarded.
                tracked_late = subprocess.Popen(
                    tracked_argv[execute_at + 1 :],
                    env=tracked_env,
                    start_new_session=True,
                )
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    tracker._remember_tracker_sessions()
                    if tracked_late.pid in tracker._pty_sessions:
                        break
                    time.sleep(0.01)
                assert tracked_late.pid in tracker._pty_sessions, (
                    "a PTY session created after control startup was not retained"
                )
                # The payload can exit while a background job remains. The
                # wrapper itself must keep the session identity alive so that
                # cleanup can retain stable handles for that descendant.
                descendant_record = Path(fixture) / "anchored-descendant"
                anchored_exit = subprocess.Popen(
                    [
                        tracked_argv[execute_at + 1],
                        sys.executable,
                        "-c",
                        (
                            "import os,signal,sys,time\n"
                            "child=os.fork()\n"
                            "if child == 0:\n"
                            " os.setpgid(0,0); signal.signal(signal.SIGHUP,signal.SIG_IGN); time.sleep(60)\n"
                            "tmp=sys.argv[1]+'.tmp'\n"
                            "with open(tmp,'w') as output: output.write(str(child))\n"
                            "os.replace(tmp,sys.argv[1])\n"
                        ),
                        str(descendant_record),
                    ],
                    env=tracked_env,
                    start_new_session=True,
                )
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline and not descendant_record.exists():
                    time.sleep(0.01)
                anchored_descendant = int(descendant_record.read_text())
                anchored_descendant_handle = StableProcessHandle.open(
                    anchored_descendant
                )
                time.sleep(0.05)
                assert anchored_exit.poll() is None, (
                    "the PTY tracker exited with its payload and lost the session anchor"
                )
                tracker._remember_tracker_sessions()
                assert anchored_exit.pid in tracker._tracker_sessions
                tracker._remember_pty_sessions(
                    json.dumps({"panes": [{"id": 7, "child_pid": None}]})
                )
                assert tracked.pid in tracker._pty_sessions, (
                    "a transient child lock erased the retained session"
                )
                terminate_owned_pty_session(
                    tracker._tracker_sessions[tracked.pid], grace_s=0.1
                )
                tracked.wait(timeout=3)
                terminate_owned_pty_session(
                    tracker._tracker_sessions[tracked_late.pid], grace_s=0.1
                )
                tracked_late.wait(timeout=3)
                terminate_owned_pty_session(
                    tracker._tracker_sessions[anchored_exit.pid], grace_s=0.1
                )
                anchored_exit.wait(timeout=3)
                status = _process_state(anchored_descendant_handle) or ""
                assert not status or status.startswith("Z"), (
                    f"descendant outlived its exited payload and cleanup: {status}"
                )
                tracker._remember_tracker_sessions()
                tracker._remember_pty_sessions(json.dumps({"panes": []}))
                assert not tracker._pty_sessions, "a closed pane retained a stale pid"
            finally:
                if tracked.returncode is None:
                    terminate_owned_process_group(tracked, grace_s=0.1)
                if tracked_late is not None and tracked_late.returncode is None:
                    terminate_owned_process_group(tracked_late, grace_s=0.1)
                if anchored_exit is not None and anchored_exit.returncode is None:
                    terminate_owned_process_group(anchored_exit, grace_s=0.1)
                if anchored_descendant_handle is not None:
                    try:
                        anchored_descendant_handle.signal(signal.SIGKILL)
                    except (OSError, RuntimeError):
                        pass
                    anchored_descendant_handle.close()
                tracker._run_post_exit_cleanups()

            failed_enumeration = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)"],
                start_new_session=True,
            )
            failed_handle = StableProcessHandle.open(failed_enumeration.pid)
            original_enumerator = _session_process_handles

            def fail_session_enumeration(
                _anchor: StableProcessHandle, _session: int
            ) -> Dict[Tuple[int, ...], StableProcessHandle]:
                raise RuntimeError("intentional session enumeration failure")

            globals()["_session_process_handles"] = fail_session_enumeration
            try:
                try:
                    terminate_owned_pty_session(failed_handle, grace_s=0.1)
                except RuntimeError as error:
                    assert "intentional session enumeration failure" in str(error)
                else:
                    raise AssertionError("a session enumeration failure must be reported")
                failed_enumeration.wait(timeout=3)
            finally:
                globals()["_session_process_handles"] = original_enumerator
                failed_handle.close()
                if failed_enumeration.returncode is None:
                    terminate_owned_process_group(failed_enumeration, grace_s=0.1)

            recheck_batch = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,time\n"
                        "child=os.fork()\n"
                        "if child == 0: time.sleep(60); raise SystemExit(0)\n"
                        "print(child,flush=True)\n"
                        "time.sleep(60)\n"
                    ),
                ],
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            assert recheck_batch.stdout is not None
            recheck_child = int(recheck_batch.stdout.readline().strip())
            recheck_anchor = StableProcessHandle.open(recheck_batch.pid)
            original_open = StableProcessHandle.__dict__["open"]
            original_run = globals()["run"]
            opened_for_recheck: List[StableProcessHandle] = []
            recheck_closes: List[int] = []
            closed_recheck_handles: Set[int] = set()

            def open_with_recheck_failure(
                cls: type[StableProcessHandle], pid: int
            ) -> StableProcessHandle:
                handle = original_open.__func__(cls, pid)
                opened_for_recheck.append(handle)
                original_close = handle.close

                def record_close() -> None:
                    recheck_closes.append(pid)
                    closed_recheck_handles.add(id(handle))
                    original_close()

                handle.close = record_close  # type: ignore[method-assign]
                if pid == recheck_child:

                    def fail_recheck() -> bool:
                        raise RuntimeError("intentional internal identity uncertainty")

                    handle.matches_current = fail_recheck  # type: ignore[method-assign]
                return handle

            def list_only_recheck_session(
                argv: List[str], **kwargs: object
            ) -> subprocess.CompletedProcess:
                if argv == ["ps", "-axo", "pid="]:
                    return subprocess.CompletedProcess(
                        argv,
                        0,
                        f"{recheck_batch.pid}\n{recheck_child}\n",
                        "",
                    )
                return original_run(argv, **kwargs)

            StableProcessHandle.open = classmethod(open_with_recheck_failure)
            globals()["run"] = list_only_recheck_session
            try:
                try:
                    _session_process_handles(recheck_anchor, recheck_batch.pid)
                except RuntimeError as error:
                    assert "intentional internal identity uncertainty" in str(error)
                else:
                    raise AssertionError(
                        "an internal PTY identity error leaked the acquired batch"
                    )
                assert recheck_batch.pid in recheck_closes
                assert recheck_child in recheck_closes
                assert all(
                    id(handle) in closed_recheck_handles
                    for handle in opened_for_recheck
                ), "an internal recheck error leaked part of its stable-handle batch"
            finally:
                globals()["run"] = original_run
                StableProcessHandle.open = original_open
                for retained in opened_for_recheck:
                    retained.close()
                recheck_anchor.close()
                if recheck_batch.returncode is None:
                    terminate_owned_process_group(recheck_batch, grace_s=0.1)

            getsid_batch = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,time\n"
                        "child=os.fork()\n"
                        "if child == 0: time.sleep(60); raise SystemExit(0)\n"
                        "print(child,flush=True)\n"
                        "time.sleep(60)\n"
                    ),
                ],
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            assert getsid_batch.stdout is not None
            getsid_child = int(getsid_batch.stdout.readline().strip())
            getsid_anchor = StableProcessHandle.open(getsid_batch.pid)
            original_open = StableProcessHandle.__dict__["open"]
            original_getsid = os.getsid
            original_run = globals()["run"]
            getsid_closes: List[int] = []

            def track_getsid_open(
                cls: type[StableProcessHandle], pid: int
            ) -> StableProcessHandle:
                handle = original_open.__func__(cls, pid)
                original_close = handle.close

                def record_close() -> None:
                    getsid_closes.append(pid)
                    original_close()

                handle.close = record_close  # type: ignore[method-assign]
                return handle

            def fail_late_getsid(pid: int) -> int:
                if pid == getsid_child:
                    raise PermissionError("intentional preliminary getsid failure")
                return original_getsid(pid)

            def list_getsid_session(
                argv: List[str], **kwargs: object
            ) -> subprocess.CompletedProcess:
                if argv == ["ps", "-axo", "pid="]:
                    return subprocess.CompletedProcess(
                        argv,
                        0,
                        f"{getsid_batch.pid}\n{getsid_child}\n",
                        "",
                    )
                return original_run(argv, **kwargs)

            StableProcessHandle.open = classmethod(track_getsid_open)
            os.getsid = fail_late_getsid  # type: ignore[assignment]
            globals()["run"] = list_getsid_session
            try:
                try:
                    _session_process_handles(getsid_anchor, getsid_batch.pid)
                except RuntimeError as error:
                    assert "intentional preliminary getsid failure" in str(error)
                else:
                    raise AssertionError(
                        "a preliminary getsid error leaked the acquired handle batch"
                    )
                assert getsid_batch.pid in getsid_closes, (
                    "a preliminary getsid error did not close prior retained handles"
                )
            finally:
                globals()["run"] = original_run
                os.getsid = original_getsid  # type: ignore[assignment]
                StableProcessHandle.open = original_open
                getsid_anchor.close()
                if getsid_batch.returncode is None:
                    terminate_owned_process_group(getsid_batch, grace_s=0.1)

            partial_batch = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,time\n"
                        "children=[]\n"
                        "for _ in range(2):\n"
                        " child=os.fork()\n"
                        " if child == 0: time.sleep(60); raise SystemExit(0)\n"
                        " children.append(child)\n"
                        "print(*children,flush=True)\n"
                        "time.sleep(60)\n"
                    ),
                ],
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            assert partial_batch.stdout is not None
            partial_children = [
                int(value) for value in partial_batch.stdout.readline().split()
            ]
            partial_anchor = StableProcessHandle.open(partial_batch.pid)
            observations = [StableProcessHandle.open(pid) for pid in partial_children]
            anchor_duplicate = StableProcessHandle.open(partial_batch.pid)
            partial_handles = [StableProcessHandle.open(pid) for pid in partial_children]
            original_bad_signal = partial_handles[0].signal

            def fail_first_stop(signal_number: int) -> bool:
                if signal_number == signal.SIGSTOP:
                    raise RuntimeError("intentional mid-batch signal failure")
                return original_bad_signal(signal_number)

            partial_handles[0].signal = fail_first_stop  # type: ignore[method-assign]

            def partial_enumerator(
                _anchor: StableProcessHandle, _session: int
            ) -> Dict[Tuple[int, ...], StableProcessHandle]:
                return {
                    anchor_duplicate.identity: anchor_duplicate,
                    partial_handles[0].identity: partial_handles[0],
                    partial_handles[1].identity: partial_handles[1],
                }

            globals()["_session_process_handles"] = partial_enumerator
            try:
                try:
                    terminate_owned_pty_session(partial_anchor, grace_s=0.1)
                except RuntimeError as error:
                    assert "intentional mid-batch signal failure" in str(error)
                else:
                    raise AssertionError("a mid-batch signal failure must be reported")
                partial_batch.wait(timeout=3)
                for observed in observations:
                    state = _process_state(observed) or ""
                    assert not state or state.startswith("Z"), (
                        "a later retained session member escaped failed cleanup: "
                        f"{state}"
                    )
            finally:
                globals()["_session_process_handles"] = original_enumerator
                partial_anchor.close()
                anchor_duplicate.close()
                for retained in partial_handles:
                    retained.close()
                for observed in observations:
                    observed.close()
                if partial_batch.returncode is None:
                    terminate_owned_process_group(partial_batch, grace_s=0.1)

            close_batch = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,time\n"
                        "children=[]\n"
                        "for _ in range(2):\n"
                        " child=os.fork()\n"
                        " if child == 0: time.sleep(60); raise SystemExit(0)\n"
                        " children.append(child)\n"
                        "print(*children,flush=True)\n"
                        "time.sleep(60)\n"
                    ),
                ],
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            assert close_batch.stdout is not None
            close_children = [
                int(value) for value in close_batch.stdout.readline().split()
            ]
            close_anchor = StableProcessHandle.open(close_batch.pid)
            close_observations = [
                StableProcessHandle.open(pid) for pid in close_children
            ]
            close_anchor_duplicate = StableProcessHandle.open(close_batch.pid)
            close_handles = [
                StableProcessHandle.open(pid) for pid in close_children
            ]
            original_duplicate_close = close_anchor_duplicate.close
            original_first_close = close_handles[0].close
            original_second_close = close_handles[1].close
            close_attempts: List[str] = []

            def fail_duplicate_close() -> None:
                close_attempts.append("duplicate")
                raise OSError("intentional duplicate close failure")

            def fail_final_close() -> None:
                close_attempts.append("first-final")
                raise OSError("intentional final close failure")

            def record_later_close() -> None:
                close_attempts.append("second-final")
                original_second_close()

            close_anchor_duplicate.close = fail_duplicate_close  # type: ignore[method-assign]
            close_handles[0].close = fail_final_close  # type: ignore[method-assign]
            close_handles[1].close = record_later_close  # type: ignore[method-assign]

            def close_failure_enumerator(
                _anchor: StableProcessHandle, _session: int
            ) -> Dict[Tuple[int, ...], StableProcessHandle]:
                return {
                    close_anchor_duplicate.identity: close_anchor_duplicate,
                    close_handles[0].identity: close_handles[0],
                    close_handles[1].identity: close_handles[1],
                }

            globals()["_session_process_handles"] = close_failure_enumerator
            try:
                try:
                    terminate_owned_pty_session(close_anchor, grace_s=0.1)
                except RuntimeError as error:
                    assert "intentional duplicate close failure" in str(error)
                    assert "intentional final close failure" in str(error)
                else:
                    raise AssertionError("PTY handle-close failures were discarded")
                close_batch.wait(timeout=3)
                assert close_attempts == [
                    "duplicate",
                    "first-final",
                    "second-final",
                ], close_attempts
                for observed in close_observations:
                    state = _process_state(observed) or ""
                    assert not state or state.startswith("Z"), (
                        "a retained process escaped after a handle-close failure: "
                        f"{state}"
                    )
            finally:
                globals()["_session_process_handles"] = original_enumerator
                close_anchor.close()
                close_anchor_duplicate.close = original_duplicate_close  # type: ignore[method-assign]
                close_anchor_duplicate.close()
                close_handles[0].close = original_first_close  # type: ignore[method-assign]
                close_handles[0].close()
                close_handles[1].close = original_second_close  # type: ignore[method-assign]
                close_handles[1].close()
                for observed in close_observations:
                    observed.close()
                if close_batch.returncode is None:
                    terminate_owned_process_group(close_batch, grace_s=0.1)

            wedged = LiveKettle("unused", Path("unused"), Path("unused"))
            outer = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)"],
                start_new_session=True,
            )
            wedged.proc = outer
            wedged._tracker_owner_pid = outer.pid
            wedged_close_attempts: List[str] = []

            class WedgedCloseHandle:
                def __init__(self, name: str, fail: bool = False) -> None:
                    self.name = name
                    self.fail = fail

                def close(self) -> None:
                    wedged_close_attempts.append(self.name)
                    if self.fail:
                        raise OSError("intentional tracker close failure")

            wedged._tracker_sessions = {  # type: ignore[assignment]
                1: WedgedCloseHandle("first", fail=True),
                2: WedgedCloseHandle("second"),
            }

            def timeout_ctl(*_args: object, **_kwargs: object) -> subprocess.CompletedProcess:
                raise subprocess.TimeoutExpired("kettle ctl", 0.01)

            wedged.ctl = timeout_ctl  # type: ignore[method-assign]
            try:
                assert wedged._control_pty_inventory(timeout=0.01) is None
                try:
                    wedged._terminate_process()
                except RuntimeError as error:
                    assert "intentional tracker close failure" in str(error)
                else:
                    raise AssertionError("a tracker close failure was discarded")
                outer.wait(timeout=3)
                assert wedged_close_attempts == ["first", "second"]
                assert not wedged._tracker_sessions
            finally:
                if outer.returncode is None:
                    terminate_owned_process_group(outer, grace_s=0.1)

            # Observe an exited leader without wait/poll, then prove outer-group
            # cleanup still reaches the descendant whose PGID is anchored by
            # that unreaped leader. This is the exact failure path used by the
            # multi-window smoke when Kettle exits unexpectedly.
            exited_group = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,signal,time\n"
                        "child=os.fork()\n"
                        "if child == 0:\n"
                        " signal.signal(signal.SIGHUP,signal.SIG_IGN); time.sleep(60)\n"
                        "print(child,flush=True)\n"
                    ),
                ],
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            assert exited_group.stdout is not None
            exited_descendant = int(exited_group.stdout.readline().strip())
            exited_descendant_handle = StableProcessHandle.open(exited_descendant)
            try:
                deadline = time.monotonic() + 3
                while (
                    time.monotonic() < deadline
                    and not process_exited_without_reaping(exited_group)
                ):
                    time.sleep(0.01)
                assert process_exited_without_reaping(exited_group)
                assert exited_group.returncode is None, (
                    "the non-reaping exit probe consumed the process-group anchor"
                )
                terminate_owned_process_group(exited_group, grace_s=0.1)
                state = _process_state(exited_descendant_handle) or ""
                assert not state or state.startswith("Z"), (
                    f"outer-group descendant survived leader exit cleanup: {state}"
                )
            finally:
                if exited_group.returncode is None:
                    terminate_owned_process_group(exited_group, grace_s=0.1)
                try:
                    exited_descendant_handle.signal(signal.SIGKILL)
                except (OSError, RuntimeError):
                    pass
                exited_descendant_handle.close()

            # A newly requested pane may not schedule its wrapper before the
            # action response returns. The close smoke must poll for the new
            # stable owner instead of sampling the append file once.
            wait_target = LiveKettle(
                "unused", fixture_root / "wait-config", fixture_root / "wait-log"
            )
            wait_target.proc = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)"],
                start_new_session=True,
            )
            remember_calls = 0

            def publish_tracker_after_delay() -> None:
                nonlocal remember_calls
                remember_calls += 1
                if remember_calls == 3:
                    wait_target._tracker_sessions[4242] = object()  # type: ignore[assignment]

            wait_target._remember_tracker_sessions = (  # type: ignore[method-assign]
                publish_tracker_after_delay
            )
            try:
                wait_target.wait_for_tracker_sessions(1, timeout_s=1)
                assert remember_calls == 3
            finally:
                terminate_owned_process_group(wait_target.proc, grace_s=0.1)

            sandbox_target = AgentShellTarget(mode="native")
            sandbox_root = Path(
                tempfile.mkdtemp(prefix="kettle-agent-tui-space path-")
            )
            sandbox_root.chmod(0o700)
            sandbox_target.validate_native_sandbox_path(str(sandbox_root))
            if platform.system() == "Linux":
                original_path_open = Path.open

                def deny_process_environment(
                    path: Path, *args: object, **kwargs: object
                ) -> IO[bytes]:
                    if path == Path(f"/proc/{os.getpid()}/environ"):
                        raise PermissionError("intentional unreadable environment")
                    return original_path_open(path, *args, **kwargs)  # type: ignore[return-value]

                Path.open = deny_process_environment  # type: ignore[assignment]
                try:
                    assert sandbox_target._native_process_environment(
                        os.getpid(), protected_candidates=set()
                    ) == set()
                    try:
                        sandbox_target._native_process_environment(os.getpid())
                    except RuntimeError as error:
                        assert "could not inspect same-user process environment" in str(
                            error
                        )
                    else:
                        raise AssertionError(
                            "an unreadable same-uid process environment must fail closed"
                        )
                finally:
                    Path.open = original_path_open  # type: ignore[assignment]
                subreaper_before = LinuxSubreaperScope._get()
                subreaper_probe = LinuxSubreaperScope.acquire()
                nested_subreaper_probe = LinuxSubreaperScope.acquire()
                assert LinuxSubreaperScope._get() == 1
                subreaper_probe.close()
                assert LinuxSubreaperScope._get() == 1, (
                    "an outer close disabled subreaping while a nested scope was active"
                )
                nested_subreaper_probe.close()
                assert LinuxSubreaperScope._get() == subreaper_before, (
                    "the Linux containment scope changed process-global state "
                    "after it closed"
                )

                # Two process instances can share one numeric PID over time.
                # The baseline decision must compare the retained identity, not
                # the reusable number; this directly models the reuse boundary
                # without depending on the host wrapping its PID space on cue.
                reuse_scope = LinuxSubreaperScope(baseline={(4242, 10)})
                reused_instance = StableProcessHandle(4242, (4242, 20))
                assert not reuse_scope.was_present_at_acquire(reused_instance)
                original_instance = StableProcessHandle(4242, (4242, 10))
                assert reuse_scope.was_present_at_acquire(original_instance)

                original_stable_open = StableProcessHandle.__dict__["open"]

                def fail_stable_open(
                    cls: type[StableProcessHandle], pid: int
                ) -> StableProcessHandle:
                    del cls, pid
                    raise OSError(errno.EMFILE, "intentional handle exhaustion")

                StableProcessHandle.open = classmethod(fail_stable_open)
                try:
                    try:
                        _open_stable_process_if_present(4242, "fixture process")
                    except RuntimeError as error:
                        assert "intentional handle exhaustion" in str(error)
                    else:
                        raise AssertionError(
                            "a stable-handle resource failure looked like process exit"
                        )
                finally:
                    StableProcessHandle.open = original_stable_open

                assert LinuxSubreaperScope._active_scopes == 0
                original_subreaper_get = LinuxSubreaperScope.__dict__["_get"]
                original_subreaper_set = LinuxSubreaperScope.__dict__["_set"]
                simulated_state = {"value": 0, "get_calls": 0}

                def failing_verify_get(cls: type[LinuxSubreaperScope]) -> int:
                    del cls
                    simulated_state["get_calls"] += 1
                    if simulated_state["get_calls"] == 2:
                        raise OSError(errno.EIO, "intentional verification failure")
                    return simulated_state["value"]

                def simulated_set(
                    cls: type[LinuxSubreaperScope], value: int
                ) -> None:
                    del cls
                    simulated_state["value"] = value

                LinuxSubreaperScope._get = classmethod(failing_verify_get)
                LinuxSubreaperScope._set = classmethod(simulated_set)
                try:
                    try:
                        LinuxSubreaperScope.acquire()
                    except OSError as error:
                        assert "intentional verification failure" in str(error)
                    else:
                        raise AssertionError(
                            "a failed subreaper verification did not abort acquisition"
                        )
                    assert simulated_state["value"] == 0
                    assert LinuxSubreaperScope._active_scopes == 0
                    assert LinuxSubreaperScope._original_state is None
                finally:
                    LinuxSubreaperScope._get = original_subreaper_get
                    LinuxSubreaperScope._set = original_subreaper_set

                simulated_state = {"value": 0, "restore_failures": 1}

                def simulated_get(cls: type[LinuxSubreaperScope]) -> int:
                    del cls
                    return simulated_state["value"]

                def retryable_set(
                    cls: type[LinuxSubreaperScope], value: int
                ) -> None:
                    del cls
                    if value == 0 and simulated_state["restore_failures"]:
                        simulated_state["restore_failures"] -= 1
                        raise OSError(errno.EIO, "intentional restoration failure")
                    simulated_state["value"] = value

                LinuxSubreaperScope._get = classmethod(simulated_get)
                LinuxSubreaperScope._set = classmethod(retryable_set)
                try:
                    retryable_scope = LinuxSubreaperScope.acquire()
                    try:
                        retryable_scope.close()
                    except OSError as error:
                        assert "intentional restoration failure" in str(error)
                    else:
                        raise AssertionError(
                            "a failed subreaper restoration looked successful"
                        )
                    assert not retryable_scope.closed
                    assert LinuxSubreaperScope._active_scopes == 1
                    retryable_scope.close()
                    assert retryable_scope.closed
                    assert simulated_state["value"] == 0
                finally:
                    LinuxSubreaperScope._get = original_subreaper_get
                    LinuxSubreaperScope._set = original_subreaper_set
                    LinuxSubreaperScope._active_scopes = 0
                    LinuxSubreaperScope._original_state = None
            matching_env = os.environ.copy()
            matching_env["KETTLE_SMOKE_ROOT"] = str(sandbox_root)
            matching_env["XDG_CONFIG_HOME"] = str(sandbox_root / "config")
            other_env = os.environ.copy()
            other_env["KETTLE_SMOKE_ROOT"] = str(sandbox_root) + "-other"
            other_env["XDG_CONFIG_HOME"] = str(sandbox_root) + "-other/config"
            matching = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)"],
                env=matching_env,
                start_new_session=True,
            )
            other = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    "import time; time.sleep(60)",
                    f"KETTLE_SMOKE_ROOT={sandbox_root}",
                    f"XDG_CONFIG_HOME={sandbox_root / 'config'}",
                ],
                env=other_env,
                start_new_session=True,
            )
            linux_scope = (
                LinuxSubreaperScope.acquire()
                if platform.system() == "Linux"
                else None
            )
            nondumpable_handle: Optional[StableProcessHandle] = None
            try:
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    if matching.pid in sandbox_target.native_nvim_sandbox_processes(
                        sandbox_root
                    ):
                        break
                    time.sleep(0.05)
                assert matching.pid in sandbox_target.native_nvim_sandbox_processes(
                    sandbox_root
                )
                assert other.pid not in sandbox_target.native_nvim_sandbox_processes(
                    sandbox_root
                )
                if platform.system() == "Linux" and os.geteuid() != 0:
                    orphan_record = sandbox_root / "run" / "orphan.pid"
                    orphan_record.parent.mkdir(exist_ok=True)
                    launcher = subprocess.Popen(
                        [
                            sys.executable,
                            "-c",
                            (
                                "import ctypes,os,sys,time\n"
                                "child=os.fork()\n"
                                "if child:\n"
                                " open(sys.argv[1],'w').write(str(child)+'\\n')\n"
                                " os._exit(0)\n"
                                "os.setsid()\n"
                                "assert ctypes.CDLL(None).prctl(4,0,0,0,0) == 0\n"
                                "time.sleep(60)\n"
                            ),
                            str(orphan_record),
                        ],
                        env=matching_env,
                        start_new_session=True,
                    )
                    launcher.wait(timeout=3)
                    nondumpable_pid = int(orphan_record.read_text(encoding="ascii"))
                    nondumpable_handle = StableProcessHandle.open(nondumpable_pid)
                    assert _linux_process_parent(nondumpable_pid) == os.getpid(), (
                        "the detached descendant was not adopted by the subreaper"
                    )
                    assert (
                        nondumpable_pid
                        not in sandbox_target.native_nvim_sandbox_processes(
                            sandbox_root
                        )
                    ), "unreadable environment was mistaken for exact ownership"
                    assert linux_scope is not None
                original_open = StableProcessHandle.__dict__["open"]

                def reject_matching_handle(
                    cls: type[StableProcessHandle], pid: int
                ) -> StableProcessHandle:
                    if pid == matching.pid:
                        raise OSError("intentional stable-handle acquisition failure")
                    return original_open.__func__(cls, pid)

                StableProcessHandle.open = classmethod(reject_matching_handle)
                try:
                    try:
                        sandbox_target.native_nvim_sandbox_handles(sandbox_root)
                    except RuntimeError as error:
                        assert "could not retain matching sandbox process" in str(error)
                    else:
                        raise AssertionError(
                            "a matching process with no stable handle looked drained"
                        )
                finally:
                    StableProcessHandle.open = original_open
                sandbox_target.terminate_native_nvim_sandbox_processes(
                    sandbox_root,
                    linux_subreaper=linux_scope,
                )
                matching.wait(timeout=3)
                if nondumpable_handle is not None:
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline and not (
                        (_process_state(nondumpable_handle) or "").startswith("Z")
                        or not nondumpable_handle.signal(0)
                    ):
                        time.sleep(0.02)
                    assert (_process_state(nondumpable_handle) or "").startswith("Z") or not (
                        nondumpable_handle.signal(0)
                    ), "the adopted nondumpable descendant survived cleanup"
                assert other.poll() is None, "sandbox cleanup killed a decoy"
            finally:
                if nondumpable_handle is not None:
                    with contextlib.suppress(OSError, RuntimeError):
                        nondumpable_handle.signal(signal.SIGKILL)
                    nondumpable_handle.close()
                if matching.returncode is None:
                    terminate_owned_process_group(matching, grace_s=0.1)
                if other.returncode is None:
                    terminate_owned_process_group(other, grace_s=0.1)
                try:
                    sandbox_target.cleanup_nvim_sandbox_host(
                        str(sandbox_root), linux_subreaper=linux_scope
                    )
                finally:
                    if linux_scope is not None:
                        linux_scope.close()

            # The normal in-pane completion command is only a release marker.
            # It must leave both a detached exact-environment daemon and its
            # tree untouched until host cleanup can retain, drain, and remove
            # them in that order.
            normal_root = sandbox_target.validate_native_sandbox_path(
                tempfile.mkdtemp(prefix="kettle-agent-tui-normal-cleanup-")
            )
            normal_root.chmod(0o700)
            normal_env = os.environ.copy()
            normal_env["KETTLE_SMOKE_ROOT"] = str(normal_root)
            normal_env["XDG_CONFIG_HOME"] = str(normal_root / "config")
            normal_scope = (
                LinuxSubreaperScope.acquire()
                if platform.system() == "Linux"
                else None
            )
            normal_daemon = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)"],
                env=normal_env,
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    if normal_daemon.pid in sandbox_target.native_nvim_sandbox_processes(
                        normal_root
                    ):
                        break
                    time.sleep(0.05)
                assert (
                    normal_daemon.pid
                    in sandbox_target.native_nvim_sandbox_processes(normal_root)
                )
                released = subprocess.run(
                    [
                        "/bin/sh",
                        "-c",
                        sandbox_target.nvim_sandbox_release_command(
                            "KETTLE_NORMAL_SANDBOX_RELEASED"
                        ),
                    ],
                    env=normal_env,
                    capture_output=True,
                    text=True,
                    timeout=3,
                    check=False,
                )
                assert released.returncode == 0
                assert "KETTLE_NORMAL_SANDBOX_RELEASED" in released.stdout
                assert normal_daemon.poll() is None
                assert normal_root.exists(), (
                    "the pane deleted the sandbox before its detached daemon drained"
                )
                released_handles = sandbox_target.native_nvim_sandbox_handles(
                    normal_root
                )
                try:
                    assert any(
                        handle.pid == normal_daemon.pid
                        for handle in released_handles.values()
                    ), "the released daemon could not be retained for host cleanup"
                finally:
                    close_errors = _close_stable_process_handles(released_handles)
                    assert not close_errors
                sandbox_target.cleanup_nvim_sandbox_host(
                    str(normal_root), linux_subreaper=normal_scope
                )
                normal_daemon.wait(timeout=3)
                assert not normal_root.exists()
            finally:
                if normal_daemon.returncode is None:
                    terminate_owned_process_group(normal_daemon, grace_s=0.1)
                try:
                    if normal_root.exists():
                        sandbox_target.cleanup_nvim_sandbox_host(
                            str(normal_root), linux_subreaper=normal_scope
                        )
                finally:
                    if normal_scope is not None:
                        normal_scope.close()

            cleanup_actions: List[str] = []

            class CleanupHandle:
                def __init__(self, name: str) -> None:
                    self.name = name
                    self.pid = 9_999_990 if name == "first" else 9_999_991

                def signal(self, number: int) -> bool:
                    label = signal.Signals(number).name
                    cleanup_actions.append(f"{self.name}:{label}")
                    if self.name == "first" and number == signal.SIGSTOP:
                        raise OSError("intentional STOP failure")
                    return True

                def close(self) -> None:
                    cleanup_actions.append(f"{self.name}:close")
                    if self.name == "first":
                        raise OSError("intentional close failure")

            original_handles = AgentShellTarget.__dict__["native_nvim_sandbox_handles"]
            original_processes = AgentShellTarget.__dict__["native_nvim_sandbox_processes"]
            fake_handles = {
                (1,): CleanupHandle("first"),
                (2,): CleanupHandle("second"),
            }
            AgentShellTarget.native_nvim_sandbox_handles = classmethod(
                lambda _cls, _root, **_kwargs: fake_handles  # type: ignore[method-assign]
            )
            AgentShellTarget.native_nvim_sandbox_processes = classmethod(
                lambda _cls, _root, **_kwargs: set()  # type: ignore[method-assign]
            )

            class EmptySubreaper:
                def adopted_roots(
                    self,
                    _parents: Dict[int, int],
                    _owned: Set[int],
                    **_kwargs: object,
                ) -> Dict[Tuple[int, ...], StableProcessHandle]:
                    return {}

            try:
                try:
                    sandbox_target.terminate_native_nvim_sandbox_processes(
                        Path(tempfile.gettempdir()) / "kettle-agent-tui-error-probe",
                        linux_subreaper=(
                            EmptySubreaper()  # type: ignore[arg-type]
                            if platform.system() == "Linux"
                            else None
                        ),
                    )
                except RuntimeError as error:
                    assert "intentional STOP failure" in str(error)
                    assert "intentional close failure" in str(error)
                else:
                    raise AssertionError("native cleanup failures were discarded")
            finally:
                AgentShellTarget.native_nvim_sandbox_handles = original_handles
                AgentShellTarget.native_nvim_sandbox_processes = original_processes
            assert cleanup_actions == [
                "first:SIGSTOP",
                "first:SIGKILL",
                "second:SIGKILL",
                "first:close",
                "second:close",
            ], cleanup_actions

            duplicate_actions: List[str] = []

            class DuplicateHandle:
                def __init__(self, name: str, pid: int) -> None:
                    self.name = name
                    self.pid = pid

                def signal(self, number: int) -> bool:
                    duplicate_actions.append(
                        f"{self.name}:{signal.Signals(number).name}"
                    )
                    return True

                def close(self) -> None:
                    duplicate_actions.append(f"{self.name}:close")
                    if self.name == "duplicate-first":
                        raise OSError("intentional duplicate close failure")

            duplicate_scan = 0

            def duplicate_handle_batch(
                _cls: type[AgentShellTarget], _root: Path, **_kwargs: object
            ) -> Dict[Tuple[int, ...], DuplicateHandle]:
                nonlocal duplicate_scan
                duplicate_scan += 1
                prefix = "canonical" if duplicate_scan == 1 else "duplicate"
                return {
                    (1,): DuplicateHandle(f"{prefix}-first", 9_999_992),
                    (2,): DuplicateHandle(f"{prefix}-second", 9_999_993),
                }

            AgentShellTarget.native_nvim_sandbox_handles = classmethod(
                duplicate_handle_batch  # type: ignore[method-assign]
            )
            AgentShellTarget.native_nvim_sandbox_processes = classmethod(
                lambda _cls, _root, **_kwargs: set()  # type: ignore[method-assign]
            )
            try:
                try:
                    sandbox_target.terminate_native_nvim_sandbox_processes(
                        Path(tempfile.gettempdir()) / "kettle-agent-tui-duplicate-probe",
                        linux_subreaper=(
                            EmptySubreaper()  # type: ignore[arg-type]
                            if platform.system() == "Linux"
                            else None
                        ),
                    )
                except RuntimeError as error:
                    assert "intentional duplicate close failure" in str(error)
                else:
                    raise AssertionError("a duplicate handle close failure was discarded")
            finally:
                AgentShellTarget.native_nvim_sandbox_handles = original_handles
                AgentShellTarget.native_nvim_sandbox_processes = original_processes
            assert duplicate_scan == 2
            assert duplicate_actions.index("duplicate-first:close") < (
                duplicate_actions.index("duplicate-second:close")
            ), duplicate_actions
            assert "canonical-first:SIGKILL" in duplicate_actions
            assert "canonical-second:SIGKILL" in duplicate_actions

            decoy_env = os.environ.copy()
            decoy_env["XDG_CONFIG_HOME"] = "/tmp/kettle-agent-tui-decoy/config"
            decoy = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)"],
                env=decoy_env,
                start_new_session=True,
            )
            term_handler_ran = fixture_root / "term-handler-ran"
            session = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,signal,sys,time\n"
                        "job=os.fork()\n"
                        "if job == 0:\n"
                        "  os.setpgid(0,0)\n"
                        "  def on_term(_signum,_frame):\n"
                        "    open(sys.argv[1],'w').write('resumed')\n"
                        "    child=os.fork()\n"
                        "    if child == 0:\n"
                        "      os.setpgid(0,0); time.sleep(60)\n"
                        "  signal.signal(signal.SIGTERM,on_term)\n"
                        "  grand=os.fork()\n"
                        "  if grand == 0: time.sleep(60)\n"
                        "  print(os.getpid(),grand,flush=True)\n"
                        "  time.sleep(60)\n"
                        "time.sleep(60)\n"
                    ),
                    str(term_handler_ran),
                ],
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            try:
                assert session.stdout is not None
                job_pid, descendant_pid = map(
                    int, session.stdout.readline().strip().split()
                )
                assert os.getsid(job_pid) == session.pid
                assert os.getpgid(job_pid) == job_pid
                session_anchor = StableProcessHandle.open(session.pid)
                terminate_owned_pty_session(session_anchor, grace_s=0.1)
                session_anchor.close()
                session.wait(timeout=3)
                assert not term_handler_ran.exists(), (
                    "cleanup resumed a TERM handler that could spawn a new group"
                )
                assert decoy.poll() is None, "an unrelated env-match was killed"
                for pid in (job_pid, descendant_pid):
                    status = run(
                        ["ps", "-p", str(pid), "-o", "stat="], timeout=2
                    ).stdout.strip()
                    assert not status or status.startswith("Z"), (
                        f"PTY-session job {pid} survived: {status}"
                    )
            finally:
                if session.returncode is None:
                    try:
                        session_anchor = StableProcessHandle.open(session.pid)
                        terminate_owned_pty_session(session_anchor, grace_s=0.1)
                        session_anchor.close()
                    except RuntimeError:
                        terminate_owned_process_group(session, grace_s=0.1)
                terminate_owned_process_group(decoy, grace_s=0.1)

    # Limits are checked while traversing and before any file body can grow
    # the snapshot without bound.
    with tempfile.TemporaryDirectory(
        prefix="kettle-nvim-limit-fixture-"
    ) as fixture:
        fixture_root = Path(fixture)
        identity_source = fixture_root / "identity-source"
        identity_source.mkdir()
        identity_file = identity_source / "plugin.lua"
        identity_file.write_bytes(b"return true\n")
        identity = regular_tree_identity(identity_source)
        assert identity["present"] is True
        assert identity["files"] == 1
        assert identity["bytes"] == len(b"return true\n")
        assert regular_file_identity(identity_file)["sha256"]
        target_identity = run(
            [
                sys.executable,
                "-c",
                AgentShellTarget.bounded_identity_code(),
                "tree",
                str(identity_source),
            ],
            timeout=10,
        )
        assert target_identity.returncode == 0, target_identity.stderr
        assert json.loads(target_identity.stdout)["sha256"] == identity["sha256"]
        empty_directory = identity_source / "empty"
        empty_directory.mkdir()
        identity_with_empty = regular_tree_identity(identity_source)
        assert identity_with_empty["files"] == identity["files"]
        assert identity_with_empty["bytes"] == identity["bytes"]
        assert identity_with_empty["sha256"] != identity["sha256"], (
            "adding an empty directory did not change the native tree identity"
        )
        target_with_empty = run(
            [
                sys.executable,
                "-c",
                AgentShellTarget.bounded_identity_code(),
                "tree",
                str(identity_source),
            ],
            timeout=10,
        )
        assert target_with_empty.returncode == 0, target_with_empty.stderr
        assert (
            json.loads(target_with_empty.stdout)["sha256"]
            == identity_with_empty["sha256"]
        ), "the WSL identity code did not hash the same directory entries"
        empty_directory.rmdir()
        assert regular_tree_identity(identity_source)["sha256"] == identity["sha256"], (
            "removing an empty directory did not restore the tree identity"
        )
        try:
            regular_tree_identity(
                identity_source, SnapshotCopyBudget(max_entries=1)
            )
        except RuntimeError as error:
            assert "entry limit" in str(error)
        else:
            raise AssertionError("identity traversal must enforce its entry limit")

        class SentinelEntry:
            path = str(identity_file)
            name = identity_file.name

        class SentinelScan:
            def __init__(self) -> None:
                self.consumed = 0

            def __enter__(self) -> "SentinelScan":
                return self

            def __exit__(self, *_args: object) -> bool:
                return False

            def __iter__(self) -> "SentinelScan":
                return self

            def __next__(self) -> SentinelEntry:
                self.consumed += 1
                if self.consumed > 1:
                    raise AssertionError("directory iterator consumed beyond cap")
                return SentinelEntry()

        sentinel_scan = SentinelScan()
        try:
            _bounded_sorted_directory_entries(
                identity_source,
                SnapshotCopyBudget(max_entries=0),
                lambda _directory: sentinel_scan,
            )
        except RuntimeError as error:
            assert "entry limit" in str(error)
        else:
            raise AssertionError("bounded entry collection accepted an excess entry")
        assert sentinel_scan.consumed == 1, (
            "identity traversal read another entry after the cap fired"
        )

        generated_cap_probe = run(
            [
                sys.executable,
                "-c",
                AgentShellTarget.bounded_identity_code(),
                "entry-cap-probe",
                "ignored",
            ],
            timeout=10,
        )
        assert generated_cap_probe.returncode == 0, generated_cap_probe.stderr
        assert json.loads(generated_cap_probe.stdout) == {
            "pre_materialization_cap": True
        }

        identity_link = identity_source / "linked.lua"
        identity_link_created = False
        try:
            identity_link.symlink_to(identity_file)
            identity_link_created = True
        except (NotImplementedError, OSError):
            pass
        if identity_link_created:
            try:
                regular_tree_identity(identity_source)
            except RuntimeError as error:
                assert "symlink" in str(error) or "junction" in str(error)
            else:
                raise AssertionError("identity traversal must reject links")
            identity_link.unlink()

        if os.name != "nt":
            # Swap a checked child directory for a link immediately before the
            # descriptor-relative open. A path walk follows the replacement and
            # hashes the outside sentinel; openat + O_NOFOLLOW must refuse it.
            swap_root = fixture_root / "identity-swap-root"
            swap_child = swap_root / "child"
            held_child = swap_root / "held-child"
            outside = fixture_root / "identity-swap-outside"
            swap_child.mkdir(parents=True)
            outside.mkdir()
            (swap_child / "inside").write_text("inside", encoding="utf-8")
            outside_sentinel = outside / "outside"
            outside_sentinel.write_text("outside", encoding="utf-8")
            original_os_open = os.open
            swapped = False

            def swap_ancestor_before_open(
                path: object,
                flags: int,
                mode: int = 0o777,
                *,
                dir_fd: Optional[int] = None,
            ) -> int:
                nonlocal swapped
                if path == "child" and dir_fd is not None and not swapped:
                    swapped = True
                    swap_child.rename(held_child)
                    swap_child.symlink_to(outside, target_is_directory=True)
                return original_os_open(path, flags, mode, dir_fd=dir_fd)  # type: ignore[arg-type]

            os.open = swap_ancestor_before_open  # type: ignore[assignment]
            try:
                try:
                    regular_tree_identity(swap_root)
                except (OSError, RuntimeError):
                    pass
                else:
                    raise AssertionError(
                        "identity followed an ancestor replaced with a symlink"
                    )
            finally:
                os.open = original_os_open  # type: ignore[assignment]
            assert outside_sentinel.read_text(encoding="utf-8") == "outside"
            swap_child.unlink()
            held_child.rename(swap_child)

            # The remover binds and validates the directory before fchmod or
            # deletion. Swap after its no-follow stat but before open; the
            # outside tree must be untouched and cleanup must report uncertainty.
            removal_root = fixture_root / "removal-swap-root"
            removal_child = removal_root / "child"
            removal_held = removal_root / "held-child"
            removal_outside = fixture_root / "removal-swap-outside"
            removal_child.mkdir(parents=True)
            removal_outside.mkdir()
            removal_sentinel = removal_outside / "outside"
            removal_sentinel.write_text("outside", encoding="utf-8")
            swapped = False

            def swap_cleanup_ancestor(
                path: object,
                flags: int,
                mode: int = 0o777,
                *,
                dir_fd: Optional[int] = None,
            ) -> int:
                nonlocal swapped
                if path == "child" and dir_fd is not None and not swapped:
                    swapped = True
                    removal_child.rename(removal_held)
                    removal_child.symlink_to(
                        removal_outside, target_is_directory=True
                    )
                return original_os_open(path, flags, mode, dir_fd=dir_fd)  # type: ignore[arg-type]

            os.open = swap_cleanup_ancestor  # type: ignore[assignment]
            try:
                try:
                    _remove_tree_by_fd(removal_root)
                except (OSError, RuntimeError):
                    pass
                else:
                    raise AssertionError(
                        "cleanup followed an ancestor replaced with a symlink"
                    )
            finally:
                os.open = original_os_open  # type: ignore[assignment]
            assert swapped, "cleanup sabotage never reached the child open"
            assert removal_sentinel.read_text(encoding="utf-8") == "outside"
            if removal_child.is_symlink():
                removal_child.unlink()
            if removal_held.exists():
                removal_held.rename(removal_child)
            shutil.rmtree(removal_root)

            # A symlink is not the only replacement a same-UID process can
            # make. Substitute a hard link to an external regular file after
            # the directory stat. O_DIRECTORY must reject it before fchmod, so
            # the external inode's mode cannot change.
            hardlink_root = fixture_root / "removal-hardlink-root"
            hardlink_child = hardlink_root / "child"
            hardlink_held = hardlink_root / "held-child"
            hardlink_external = fixture_root / "removal-hardlink-external"
            hardlink_child.mkdir(parents=True)
            hardlink_external.write_text("outside", encoding="utf-8")
            hardlink_external.chmod(0o600)
            swapped = False

            def swap_cleanup_to_hardlink(
                path: object,
                flags: int,
                mode: int = 0o777,
                *,
                dir_fd: Optional[int] = None,
            ) -> int:
                nonlocal swapped
                if path == "child" and dir_fd is not None and not swapped:
                    swapped = True
                    hardlink_child.rename(hardlink_held)
                    os.link(hardlink_external, hardlink_child)
                return original_os_open(path, flags, mode, dir_fd=dir_fd)  # type: ignore[arg-type]

            os.open = swap_cleanup_to_hardlink  # type: ignore[assignment]
            try:
                try:
                    _remove_tree_by_fd(hardlink_root)
                except (OSError, RuntimeError):
                    pass
                else:
                    raise AssertionError(
                        "cleanup accepted a directory replaced by a hard link"
                    )
            finally:
                os.open = original_os_open  # type: ignore[assignment]
            assert swapped, "hard-link sabotage never reached the child open"
            assert hardlink_external.stat().st_mode & 0o777 == 0o600
            if hardlink_child.is_file():
                hardlink_child.unlink()
            if hardlink_held.exists():
                hardlink_held.rename(hardlink_child)
            shutil.rmtree(hardlink_root)
            hardlink_external.unlink()

            # No path-based chmod is allowed in the descriptor remover. Apart
            # from being racy, Python cannot provide its no-follow form on every
            # supported Unix. A hostile replacement makes any call here fail.
            chmod_root = fixture_root / "fd-chmod-root"
            (chmod_root / "child").mkdir(parents=True)
            (chmod_root / "child" / "leaf").write_text("leaf", encoding="utf-8")
            original_os_chmod = os.chmod

            def reject_path_chmod(*_args: object, **_kwargs: object) -> None:
                raise AssertionError("descriptor cleanup used path-based chmod")

            os.chmod = reject_path_chmod  # type: ignore[assignment]
            try:
                _remove_tree_by_fd(chmod_root)
            finally:
                os.chmod = original_os_chmod  # type: ignore[assignment]
            assert not chmod_root.exists()

        source = fixture_root / "source"
        source.mkdir()
        (source / "too-large.lua").write_bytes(b"12345")
        try:
            AgentShellTarget.copy_bounded_regular_tree(
                source,
                fixture_root / "copy-too-large",
                SnapshotCopyBudget(max_file_bytes=4),
            )
        except RuntimeError as error:
            assert "per-file limit" in str(error)
        else:
            raise AssertionError("oversized snapshot file must be rejected")

        (source / "second.lua").write_text("-- second\n", encoding="utf-8")
        try:
            AgentShellTarget.copy_bounded_regular_tree(
                source,
                fixture_root / "copy-too-many",
                SnapshotCopyBudget(max_entries=1),
            )
        except RuntimeError as error:
            assert "entry limit" in str(error)
        else:
            raise AssertionError("snapshot entry limit must be enforced")

        cycle = source / "cycle"
        cycle_created = False
        try:
            cycle.symlink_to(source, target_is_directory=True)
            cycle_created = True
        except (NotImplementedError, OSError):
            pass
        if cycle_created:
            try:
                AgentShellTarget.copy_bounded_regular_tree(
                    source,
                    fixture_root / "copy-cycle",
                    SnapshotCopyBudget(),
                )
            except (OSError, RuntimeError) as error:
                assert "cycle" in str(error).lower() or "resolve" in str(error).lower()
            else:
                raise AssertionError("snapshot directory cycle must be rejected")

        deep_source = fixture_root / "deep-source"
        (deep_source / "one" / "two").mkdir(parents=True)
        try:
            AgentShellTarget.copy_bounded_regular_tree(
                deep_source,
                fixture_root / "copy-too-deep",
                SnapshotCopyBudget(max_depth=1),
            )
        except RuntimeError as error:
            assert "depth limit" in str(error)
        else:
            raise AssertionError("snapshot depth limit must be enforced")

        aggregate_source = fixture_root / "aggregate-source"
        aggregate_source.mkdir()
        (aggregate_source / "one").write_bytes(b"123")
        (aggregate_source / "two").write_bytes(b"456")
        try:
            AgentShellTarget.copy_bounded_regular_tree(
                aggregate_source,
                fixture_root / "copy-too-large-in-aggregate",
                SnapshotCopyBudget(max_bytes=5),
            )
        except RuntimeError as error:
            assert "aggregate byte limit" in str(error)
        else:
            raise AssertionError("snapshot aggregate byte limit must be enforced")

        metadata_source = fixture_root / "metadata-source"
        (metadata_source / ".git" / "objects" / "pack").mkdir(parents=True)
        (metadata_source / ".git" / "objects" / "pack" / "large.pack").write_bytes(
            b"ignored"
        )
        (metadata_source / ".git" / "HEAD").write_bytes(b"x\n")
        (metadata_source / "runtime.lua").write_bytes(b"ok")
        metadata_copy = fixture_root / "copy-without-git-objects"
        AgentShellTarget.copy_bounded_regular_tree(
            metadata_source,
            metadata_copy,
            SnapshotCopyBudget(max_file_bytes=2),
        )
        assert (metadata_copy / "runtime.lua").read_bytes() == b"ok"
        assert (metadata_copy / ".git" / "HEAD").read_bytes() == b"x\n"
        assert not (metadata_copy / ".git" / "objects").exists()
        assert (
            "-path '*/.git/objects' -prune"
            in AgentShellTarget.wsl_bounded_copy_function()
        )

        if hasattr(os, "mkfifo"):
            special_source = fixture_root / "special-source"
            special_source.mkdir()
            os.mkfifo(special_source / "fifo")
            try:
                regular_tree_identity(special_source)
            except RuntimeError as error:
                assert "non-regular" in str(error)
            else:
                raise AssertionError("identity traversal must reject special files")
            try:
                AgentShellTarget.copy_bounded_regular_tree(
                    special_source,
                    fixture_root / "copy-special",
                    SnapshotCopyBudget(),
                )
            except RuntimeError as error:
                assert "regular files and directories" in str(error)
            else:
                raise AssertionError("snapshot special files must be rejected")

    nvim_marker = "KETTLE_NVIM_RUNTIME_ONLY"
    marker_command = nvim_marker_command(
        nvim_marker, False, windows=False
    )
    assert "nvim --clean -n" in marker_command
    assert "nvim.pid" in marker_command
    assert nvim_marker not in marker_command
    left_marker = "KETTLE_ASTRO_LEFT_RUNTIME"
    right_marker = "KETTLE_ASTRO_RIGHT_RUNTIME"
    split_command = nvim_split_command(
        left_marker, right_marker, True, windows=False
    )
    assert "nvim -n" in split_command
    assert "nvim.pid" in split_command
    assert left_marker not in split_command
    assert right_marker not in split_command

    # LazyVCS leg. These are pure string builders, so the shape they produce is
    # checkable on every platform even though the live probe needs a window.
    lazyvcs_marker = "KETTLE_LAZYVCS_RUNTIME"
    lazyvcs_repo = "/tmp/kettle-smoke-lazyvcs"
    fixture_token = "A1B2C3D4"
    setup_posix = lazyvcs_repo_setup_command(
        lazyvcs_repo, fixture_token, windows=False
    )
    assert "git init -q ." in setup_posix
    # An unstaged edit is the whole point: without it the sidebar renders no
    # changed files and no gutter signs, and the probe would assert nothing.
    assert "git commit -q -m base" in setup_posix
    assert setup_posix.count("tracked.txt") >= 3
    setup_windows = lazyvcs_repo_setup_command(
        lazyvcs_repo, fixture_token, windows=True
    )
    assert "Set-Content" in setup_windows and "Invoke-Git init -q ." in setup_windows

    sidebar_posix = lazyvcs_sidebar_command(
        lazyvcs_repo, lazyvcs_marker, windows=False
    )
    # `nvim -n`, not `--clean`: the configured runtime is what has LazyVCS.
    assert "nvim -n" in sidebar_posix and "--clean" not in sidebar_posix
    assert "+silent! LazyVCS blame toggle" in sidebar_posix
    assert "+silent! LazyVCS sidebar open" in sidebar_posix
    # The marker must reach the buffer as an expression, never as a literal, or
    # `wait_for_text` matches the command echo instead of the rendered buffer.
    assert lazyvcs_marker not in sidebar_posix
    # Discovery is asynchronous; without this wait the probe races the render.
    assert "lazyvcs_discovering" in sidebar_posix
    assert "vim.wait(30000" in sidebar_posix
    assert "vim.uv.fs_realpath" in sidebar_posix
    assert "debug.getinfo(native._state, 'S')" in sidebar_posix
    assert "lazyvcs-loaded-source" in sidebar_posix
    assert '"+language messages C"' in sidebar_posix
    assert "nvim.pid" in sidebar_posix
    assert "vim.api.nvim_buf_get_name(0)" in sidebar_posix
    assert "vim.fs.basename(expected)" in sidebar_posix
    assert "state_path == expected and exact_repo" in sidebar_posix
    assert "s.lazyvcs_repo_specs" in sidebar_posix
    # The marker must be conditional. Written unconditionally, the probe passes
    # when `:LazyVCS` is missing, the plugin fails to load, or discovery times
    # out -- Neovim reports the error and runs the next `+` command anyway.
    assert "pcall(require, 'lazyvcs.source_control.native')" in sidebar_posix
    assert "rendered and" in sidebar_posix
    # Neither outcome token may appear literally in the echoed shell command:
    # the waiter reads the terminal while that command is still being entered.
    # Seeing a literal failure token there used to abort before Neovim even ran.
    assert "KETTLE_LAZYVCS_SIDEBAR_ABSENT" not in sidebar_posix
    # The marker gate uses stable, visible state only. Gutter and blame are
    # validated from the terminal grid by `lazyvcs_screen_evidence`; coupling
    # this command to LazyVCS's private caches or extmark namespaces duplicated
    # that proof and failed even while the required UI was visibly present.
    assert "lazyvcs_repo_cache" not in sidebar_posix
    assert "nvim_get_namespaces" not in sidebar_posix
    assert "nvim_buf_get_extmarks" not in sidebar_posix
    assert "vim.fs.normalize(vim.uv.fs_realpath" in sidebar_posix
    # This expression is parsed by Lua. Reusing `nvim_string_expression`
    # emitted Vimscript's single-dot concatenation and made Neovim stop at
    # E5107 before it ever inspected LazyVCS state.
    assert (
        nvim_lua_string_expression("AB")
        == "string.char(65) .. string.char(66)"
    )
    assert " . " not in sidebar_posix
    # LazyVCS owns and asynchronously refreshes its sidebar buffer. An atomic
    # regular marker therefore carries the internal-state result while
    # independent cell assertions prove the sidebar and editor contents.
    assert "'/run/lazyvcs-ready'" in sidebar_posix
    assert "vim.uv.fs_rename(ready_tmp, ready)" in sidebar_posix
    assert "vim.api.nvim_echo({{message, 'None'}}, false, {})" in sidebar_posix
    assert "nvim_buf_set_lines" not in sidebar_posix
    assert "vim.bo[target].modifiable" not in sidebar_posix
    # A failure string that CONTAINED the marker would still satisfy
    # `wait_for_text`, turning the failure branch back into a false pass.
    assert lazyvcs_marker not in "KETTLE_LAZYVCS_SIDEBAR_ABSENT"

    def cell_fixture(lines: List[str]) -> Dict[str, object]:
        rows = len(lines)
        cols = max(len(line) for line in lines)
        return {
            "rows": rows,
            "cols": cols,
            "cells": [
                {"row": row, "col": col, "ch": ch}
                for row, line in enumerate(lines)
                for col, ch in enumerate(line.ljust(cols))
            ],
        }

    valid_lazyvcs_lines = [
        f"{lazyvcs_marker:<38}│",
        f"{'Changes (1)':<38}│   1   FIRST_A1B2C3D4   KTLBL,",
        f"{'lazyvcs-smoke-repo':<38}│   1 ▎ CHANGED_A1B2C3D4",
    ]
    valid_lazyvcs_screen = cell_fixture(valid_lazyvcs_lines)
    assert not lazyvcs_screen_evidence(
        valid_lazyvcs_screen,
        repo_name="lazyvcs-smoke-repo",
        fixture_token=fixture_token,
    )
    # Tokens on the sidebar side cannot substitute for editor-buffer rows.
    sabotaged = cell_fixture(
        [
            f"{lazyvcs_marker:<38}│",
            f"{'Changes (1) FIRST_A1B2C3D4 KTLBL':<38}│",
            f"{'lazyvcs-smoke-repo CHANGED_A1B2C3D4 ▎':<38}│",
        ]
    )
    assert lazyvcs_screen_evidence(
        sabotaged,
        repo_name="lazyvcs-smoke-repo",
        fixture_token=fixture_token,
    ) == ["changed-row-gutter", "fixture-row-blame"]
    review_counterexample = cell_fixture(
        [
            f"{lazyvcs_marker:<38}│",
            f"{'Changes (1)':<38}│   99 ▎ CHANGED",
            f"{'lazyvcs-smoke-repo':<38}│   88 first KTLBL",
        ]
    )
    assert lazyvcs_screen_evidence(
        review_counterexample,
        repo_name="lazyvcs-smoke-repo",
        fixture_token=fixture_token,
    ) == ["changed-row-gutter", "fixture-row-blame"]
    inconsistent_divider = cell_fixture(
        [
            f"{lazyvcs_marker:<37}│",
            f"{'Changes (1)':<38}│   1   FIRST_A1B2C3D4   KTLBL,",
            f"{'lazyvcs-smoke-repo':<39}│   1 ▎ CHANGED_A1B2C3D4",
        ]
    )
    assert lazyvcs_screen_evidence(
        inconsistent_divider,
        repo_name="lazyvcs-smoke-repo",
        fixture_token=fixture_token,
    ) == ["sidebar-editor-divider"]

    wsl_paths = AgentShellTarget(mode="wsl", wsl_distro="Fixture")
    wsl_repo = wsl_paths.target_join(
        "/tmp/kettle-agent-tui-Fixture", "lazyvcs-smoke-repo"
    )
    assert wsl_repo == "/tmp/kettle-agent-tui-Fixture/lazyvcs-smoke-repo"
    wsl_sidebar = lazyvcs_sidebar_command(
        wsl_repo, lazyvcs_marker, windows=False
    )
    assert (
        "/tmp/kettle-agent-tui-Fixture/lazyvcs-smoke-repo/tracked.txt"
        in wsl_sidebar
    )
    assert "\\tmp\\kettle-agent" not in wsl_sidebar

    # PowerShell: `$ErrorActionPreference` does not cover native executables, so
    # each git call needs an explicit exit-code check or a failed setup reaches
    # `Pop-Location` looking like success.
    assert "$LASTEXITCODE" in setup_windows
    assert "finally { Pop-Location }" in setup_windows

    first_socket = new_tmux_socket_name()
    second_socket = new_tmux_socket_name()
    assert first_socket != second_socket
    assert re.fullmatch(r"kettle-smoke-[0-9a-f]{24}", first_socket)
    assert parse_tmux_version("tmux 3.4") == (3, 4)
    assert parse_tmux_version("tmux 3.6b") == (3, 6)
    assert parse_tmux_version("tmux next-3.7") is None
    assert tmux_da1_has_sixel("1b5b3f313b323b3463") is True
    assert tmux_da1_has_sixel("1b5b3f313b3263") is False
    assert tmux_da1_has_sixel("not-hex") is None
    assert tmux_cell_size_from_reply("1b5b363b31383b3974") == (9, 18)
    assert tmux_cell_size_from_reply("1b5b363b303b3074") == (0, 0)
    assert tmux_cell_size_from_reply("not-hex") is None
    query_code = terminal_query_python_code(
        b"\x1b[c", "KETTLE_TMUX_DA1=", b"c"
    )
    assert "KETTLE_TMUX_DA1=" not in query_code
    assert "while len(data)<64" in query_code
    assert "deadline=time.monotonic()+2" in query_code
    assert "finally:" in query_code
    assert "termios.tcsetattr" in query_code
    target_bash = "/nix/store/test-bash/bin/bash"
    session_command = tmux_session_command(first_socket, target_bash)
    sixel_session_command = tmux_session_command(
        first_socket, target_bash, sixel=True
    )
    split_commands = tmux_split_commands(
        first_socket, target_bash, "LEFT", "RIGHT"
    )
    assert target_bash in session_command
    assert " -T sixel new-session " in sixel_session_command
    assert " -T sixel " not in session_command
    assert split_commands[0][-1] == target_bash
    assert split_commands[0][-1] != "/bin/bash"
    assert target_bash in split_commands[1][-1]
    marker_commands = [split_commands[3][-2], split_commands[4][-2]]
    assert "LEFT" not in marker_commands[0]
    assert "RIGHT" not in marker_commands[1]
    sixel_marker = "KETTLE_TMUX_SIXEL_RUNTIME"
    sixel_command = tmux_sixel_marker_command(sixel_marker)
    assert "#1;2;100;0;100!24~-!24~" in sixel_command
    assert sixel_marker not in sixel_command
    cell_query = tmux_cell_size_query_command("/usr/bin/python3")
    assert "/usr/bin/python3" in cell_query
    assert "KETTLE_TMUX_CELL=" not in cell_query
    assert "1b5b313674" in cell_query

    black_row = bytes([0, 0, 0, 255] * 16)
    magenta_row = bytes(
        [0, 0, 0, 255] * 2
        + [255, 0, 255, 255] * 12
        + [0, 0, 0, 255] * 2
    )
    before_rows = [black_row] * 8
    after_rows = [black_row] + [magenta_row] * 6 + [black_row]
    assert magenta_block_metrics(after_rows) == (12, 6)
    assert added_magenta_pixel_count(before_rows, after_rows) == 72

    class FakeTmuxCapabilityTarget:
        def __init__(
            self, version: str, da1_hex: str, format_value: Optional[str]
        ):
            self.version = version
            self.da1_hex = da1_hex
            self.format_value = format_value
            self.calls: List[List[str]] = []

        def command_available(self, command: str) -> bool:
            return command == "python3"

        def require_command_path(self, command: str) -> str:
            assert command == "python3"
            return "/usr/bin/python3"

        def run_command(
            self, argv: List[str], *, timeout: float
        ) -> subprocess.CompletedProcess:
            self.calls.append(argv)
            assert timeout == 5
            if argv == ["tmux", "-V"]:
                return subprocess.CompletedProcess(argv, 0, self.version + "\n", "")
            if "new-session" in argv:
                assert "while len(data)<64" in argv[-1]
                assert "termios.tcsetattr" in argv[-1]
                return subprocess.CompletedProcess(argv, 0, "", "")
            if "capture-pane" in argv:
                return subprocess.CompletedProcess(
                    argv, 0, f"KETTLE_TMUX_DA1={self.da1_hex}\n", ""
                )
            if "display-message" in argv:
                value = "" if self.format_value is None else self.format_value
                return subprocess.CompletedProcess(argv, 0, value + "\n", "")
            if argv[-1] == "kill-server":
                return subprocess.CompletedProcess(argv, 0, "", "")
            raise AssertionError(f"unexpected fake tmux command: {argv}")

    enabled_tmux = FakeTmuxCapabilityTarget(
        "tmux 3.6b", "1b5b3f313b323b3463", "1"
    )
    enabled_capability = probe_tmux_sixel_capability(
        enabled_tmux  # type: ignore[arg-type]
    )
    assert enabled_capability["supported"] is True
    assert enabled_capability["sixel_support_format"] == "1"
    disabled_tmux = FakeTmuxCapabilityTarget(
        "tmux 3.4", "1b5b3f313b3263", None
    )
    disabled_capability = probe_tmux_sixel_capability(
        disabled_tmux  # type: ignore[arg-type]
    )
    assert disabled_capability["supported"] is False
    assert "--enable-sixel" in str(disabled_capability["reason"])
    conflicting_tmux = FakeTmuxCapabilityTarget(
        "tmux 3.6", "1b5b3f313b323b3463", "0"
    )
    conflicting_capability = probe_tmux_sixel_capability(
        conflicting_tmux  # type: ignore[arg-type]
    )
    assert conflicting_capability["supported"] is None
    assert conflicting_capability["capability_source"] == "conflicting-probes"
    try:
        cleanup_tmux_server(wsl_target, "unsafe")
    except ValueError:
        pass
    else:
        raise AssertionError("unsafe tmux cleanup socket must be rejected")

    class FakeTmuxTarget:
        def __init__(self, result: subprocess.CompletedProcess):
            self.result = result
            self.calls = 0

        def run_command(
            self, argv: List[str], *, timeout: float
        ) -> subprocess.CompletedProcess:
            assert argv[-1] == "kill-server"
            assert timeout == 5
            self.calls += 1
            return self.result

    success_target = FakeTmuxTarget(
        subprocess.CompletedProcess([], 0, "", "")
    )
    cleanup_tmux_server(success_target, first_socket)  # type: ignore[arg-type]
    assert success_target.calls == 1
    absent_target = FakeTmuxTarget(
        subprocess.CompletedProcess([], 1, "", "no server running")
    )
    cleanup_tmux_server(absent_target, first_socket)  # type: ignore[arg-type]
    failed_target = FakeTmuxTarget(
        subprocess.CompletedProcess([], 1, "", "permission denied")
    )
    try:
        cleanup_tmux_server(failed_target, first_socket)  # type: ignore[arg-type]
    except RuntimeError as error:
        assert "permission denied" in str(error)
    else:
        raise AssertionError("unexpected tmux cleanup failure must be fatal")


def nvim_string_expression(
    value: str, *, windows: Optional[bool] = None
) -> str:
    """Build a Vimscript string without embedding the awaited value."""
    if len(value) < 2:
        raise ValueError("Neovim smoke markers must contain at least two characters")
    left, right = split_marker(value)
    return (
        f"{shell_quote(left, windows=windows)} . "
        f"{shell_quote(right, windows=windows)}"
    )


def nvim_lua_string_expression(value: str) -> str:
    """Build a Lua string expression without embedding the awaited value.

    The live smoke waits for the resulting marker in Kettle's grid. Keeping the
    literal out of Neovim's command line prevents an echoed command from
    satisfying that wait before the buffer is actually updated. Byte-valued
    `string.char` calls also avoid asking a shell quote to double as a Lua
    quote; most importantly, Lua concatenates with `..`, not Vimscript's `.`.
    """
    if len(value) < 2:
        raise ValueError(
            "Neovim smoke markers must contain at least two characters"
        )
    left, right = split_marker(value)

    def char_expression(part: str) -> str:
        encoded = part.encode("utf-8")
        return "string.char(" + ",".join(str(byte) for byte in encoded) + ")"

    return f"{char_expression(left)} .. {char_expression(right)}"


def nvim_pid_record_arg() -> str:
    """Record the exact Neovim PID from inside the sandboxed process."""
    return (
        '--cmd "lua local root = vim.env.KETTLE_SMOKE_ROOT; '
        "assert(type(root) == 'string' and root ~= ''); "
        "assert(vim.fn.writefile({tostring(vim.fn.getpid())}, "
        "root .. '/run/nvim.pid') == 0)\""
    )


def nvim_marker_command(
    marker: str, configured: bool, *, windows: Optional[bool] = None
) -> str:
    base = "nvim -n" if configured else "nvim --clean -n"
    marker_expression = nvim_string_expression(marker, windows=windows)
    return (
        f'{base} {nvim_pid_record_arg()} "+set termguicolors" '
        f'"+call setline(1, {marker_expression})" '
        '"+normal! gg"'
    )


def lazyvcs_repo_setup_command(
    repo: str, fixture_token: str, *, windows: Optional[bool] = None
) -> str:
    """Shell command creating a one-file Git repository with an unstaged edit.

    The edit is what makes the probe meaningful: LazyVCS only draws gutter
    signs and a populated sidebar when something has actually changed.
    """
    if re.fullmatch(r"[A-Z0-9]{8,32}", fixture_token) is None:
        raise ValueError("LazyVCS fixture token must be 8-32 uppercase alphanumerics")
    quoted = shell_quote(repo, windows=windows)
    first = f"FIRST_{fixture_token}"
    changed = f"CHANGED_{fixture_token}"
    third = f"THIRD_{fixture_token}"
    if windows:
        # `$ErrorActionPreference='Stop'` does not apply to native executables,
        # so each `git` is followed by an explicit `$LASTEXITCODE` check --
        # otherwise a failed `git init` still reaches `Pop-Location` and the
        # harness cannot tell a partial repository from a complete one. The
        # `finally` guarantees the location is restored either way.
        return (
            "$ErrorActionPreference='Stop'; "
            "function Invoke-Git { git @args; "
            "if ($LASTEXITCODE -ne 0) { throw \"git $args failed: $LASTEXITCODE\" } }; "
            f"New-Item -ItemType Directory -Force {quoted} | Out-Null; "
            f"Push-Location {quoted}; "
            "try { "
            "Invoke-Git init -q .; "
            "Invoke-Git config user.name KTLBL; "
            "Invoke-Git config user.email kettle-smoke@example.invalid; "
            f"Set-Content -Path tracked.txt -Value '{first}','SECOND','{third}'; "
            "Invoke-Git add tracked.txt; Invoke-Git commit -q -m base; "
            f"Set-Content -Path tracked.txt -Value '{first}','{changed}','{third}' "
            "} finally { Pop-Location }"
        )
    return (
        f"mkdir -p {quoted} && cd {quoted} && "
        "git init -q . && "
        "git config user.name KTLBL && "
        "git config user.email kettle-smoke@example.invalid && "
        f"printf '{first}\\nSECOND\\n{third}\\n' > tracked.txt && "
        "git add tracked.txt && git commit -q -m base && "
        f"printf '{first}\\n{changed}\\n{third}\\n' > tracked.txt"
    )


def lazyvcs_sidebar_command(
    repo: str,
    marker: str,
    *,
    windows: Optional[bool] = None,
) -> str:
    """Open a file with LazyVCS loaded, show the sidebar, and report readiness.

    Exercises the parts of LazyVCS that depend on the terminal rather than on
    Neovim: the sidebar's Nerd Font icons, the box-drawing gutter sign glyphs
    (default add/change is U+2503), and inline blame virtual text. Kettle
    bundles JetBrains Mono Nerd Font, so a missing glyph here is a kettle
    rendering defect rather than a font-installation problem on the runner.

    Discovery is asynchronous, so the sidebar's first frame reads
    "Discovering repositories..." -- wait for it to settle before echoing the
    marker, or the probe races the very rendering it means to check.
    """
    marker_expression = nvim_lua_string_expression(marker)
    failure_marker = "KETTLE_LAZYVCS_SIDEBAR_ABSENT"
    failure_expression = nvim_lua_string_expression(failure_marker)
    repo_expression = nvim_lua_string_expression(repo)
    tracked_path = (
        str(Path(repo) / "tracked.txt")
        if windows
        else posixpath.join(repo, "tracked.txt")
    )
    tracked = shell_quote(tracked_path, windows=windows)
    # The marker is echoed ONLY when the sidebar really rendered a repository.
    #
    # Writing it unconditionally after the wait would make the probe pass when
    # `:LazyVCS` does not exist, when the plugin fails to load, or when
    # discovery times out -- Neovim reports the error and carries on to the next
    # `+` command regardless, so `wait_for_text` would find the marker and
    # conclude the sidebar rendered. A distinct failure string is echoed
    # instead, so the timeout that follows carries the reason in the captured
    # grid rather than just "marker not found".
    check = (
        "+lua local ok, native = pcall(require, 'lazyvcs.source_control.native'); "
        "local info = ok and debug.getinfo(native._state, 'S') or nil; "
        "local source = info and info.source or nil; "
        "source = type(source) == 'string' and source:sub(1, 1) == '@' "
        "and source:sub(2) or nil; "
        "local loaded = source and vim.uv.fs_realpath(source) or nil; "
        "local plugin_root = vim.uv.fs_realpath(vim.env.XDG_DATA_HOME "
        ".. '/nvim/lazy/lazyvcs.nvim'); "
        "local loaded_cmp = loaded; local root_cmp = plugin_root; "
        "if vim.fn.has('win32') == 1 then "
        "loaded_cmp = loaded_cmp and loaded_cmp:lower() or nil; "
        "root_cmp = root_cmp and root_cmp:lower() or nil; end; "
        "local separator = package.config:sub(1, 1); "
        "local prefix = root_cmp and (root_cmp:sub(-1) == separator "
        "and root_cmp or root_cmp .. separator) or nil; "
        "local inside = loaded_cmp ~= nil and root_cmp ~= nil "
        "and (loaded_cmp == root_cmp or loaded_cmp:sub(1, #prefix) == prefix); "
        "local record = vim.env.KETTLE_SMOKE_ROOT .. '/run/lazyvcs-loaded-source'; "
        "local recorded = inside and vim.fn.writefile({loaded}, record) == 0; "
        "local s = ok and native._state() or nil; "
        f"local expected = vim.fs.normalize(vim.uv.fs_realpath({repo_expression}) "
        f"or {repo_expression}); "
        "local settled = s ~= nil and vim.wait(30000, function() "
        "local lines = type(s.bufnr) == 'number' "
        "and vim.api.nvim_buf_is_valid(s.bufnr) "
        "and vim.api.nvim_buf_get_lines(s.bufnr, 0, -1, false) or {}; "
        "local sidebar = table.concat(lines, '\\n'); "
        "local state_path = s.path and vim.fs.normalize(vim.uv.fs_realpath(s.path) or s.path) or nil; "
        "local exact_repo = false; for _, spec in ipairs(s.lazyvcs_repo_specs or {}) do "
        "local root = spec.root and vim.fs.normalize(vim.uv.fs_realpath(spec.root) or spec.root) or nil; "
        "if root == expected then exact_repo = true; break; end; end; "
        "return s.lazyvcs_discovering ~= true "
        "and state_path == expected and exact_repo "
        "and sidebar:find(vim.fs.basename(expected), 1, true) ~= nil "
        "and vim.fs.normalize(vim.api.nvim_buf_get_name(0)) "
        "== vim.fs.normalize(expected .. '/tracked.txt') "
        "end, 25); "
        "local rendered = settled and recorded "
        "and vim.api.nvim_buf_is_valid(s.bufnr); "
        "local ready = vim.env.KETTLE_SMOKE_ROOT .. '/run/lazyvcs-ready'; "
        "local ready_tmp = ready .. '.tmp-' .. tostring(vim.fn.getpid()); "
        f"local ready_written = rendered and vim.fn.writefile({{{marker_expression}}}, ready_tmp) == 0; "
        "if ready_written then ready_written = vim.uv.fs_chmod(ready_tmp, 384) ~= nil; end; "
        "if ready_written then ready_written = vim.uv.fs_rename(ready_tmp, ready) ~= nil; end; "
        "if not ready_written then pcall(vim.uv.fs_unlink, ready_tmp); end; "
        f"local message = rendered and ready_written and {marker_expression} "
        f"or ({failure_expression} .. ' ok=' .. tostring(ok) "
        ".. ' state=' .. tostring(s ~= nil) .. ' settled=' .. tostring(settled) "
        ".. ' loaded=' .. tostring(loaded) .. ' recorded=' .. tostring(recorded) "
        ".. ' ready=' .. tostring(ready_written)); "
        # Do not insert the result into LazyVCS's asynchronously refreshed
        # sidebar buffer. The atomic regular file is the internal-state proof;
        # independent cell-grid assertions below prove what actually rendered.
        "if not ready_written then vim.api.nvim_echo({{message, 'None'}}, false, {}); end"
    )
    return (
        f'nvim -n {tracked} {nvim_pid_record_arg()} '
        '"+language messages C" "+set termguicolors" '
        # Loading LazyVCS also activates unrelated configured completion
        # sources. A warning from one of those sources must not leave Neovim's
        # hit-enter pager covering the sidebar; the explicit state check below
        # is the authoritative success/failure report for this probe.
        '"+silent! LazyVCS blame toggle" '
        '"+silent! LazyVCS sidebar open" '
        '"+wincmd p" '
        '"+normal! gg" '
        f'"{check}"'
    )


def lazyvcs_screen_evidence(
    cells: Dict[str, object], *, repo_name: str, fixture_token: str
) -> List[str]:
    """Return missing evidence associated with one cell-proven split column."""
    rows = int(cells.get("rows", 0))
    cols = int(cells.get("cols", 0))
    grid = [[" " for _col in range(cols)] for _row in range(rows)]
    divider_counts: Dict[int, int] = {}
    for cell in cells.get("cells", []):
        if not isinstance(cell, dict):
            continue
        row = cell.get("row")
        col = cell.get("col")
        ch = cell.get("ch")
        if not isinstance(row, int) or not isinstance(col, int) or not isinstance(ch, str):
            continue
        if 0 <= row < rows and 0 <= col < cols:
            grid[row][col] = ch
            if ch == "│":
                divider_counts[col] = divider_counts.get(col, 0) + 1
    missing: List[str] = []
    if not divider_counts:
        return ["sidebar-editor-divider"]
    divider_col, divider_rows = max(
        divider_counts.items(), key=lambda item: (item[1], -item[0])
    )
    if divider_rows < 2 or sum(
        1 for count in divider_counts.values() if count == divider_rows
    ) != 1:
        return ["sidebar-editor-divider"]
    sidebar_lines = ["".join(row[:divider_col]) for row in grid]
    editor_lines = ["".join(row[divider_col + 1 :]) for row in grid]
    sidebar_text = "\n".join(sidebar_lines)
    editor_text = "\n".join(editor_lines)
    for token in (repo_name, "Changes (1)"):
        if token not in sidebar_text:
            missing.append(f"sidebar:{token}")
    changed = re.escape(f"CHANGED_{fixture_token}")
    first = re.escape(f"FIRST_{fixture_token}")
    if not any(
        re.search(rf"\b\d+\s+[┃▎]\s+{changed}\b", line)
        for line in editor_lines
    ):
        missing.append("changed-row-gutter")
    if not any(
        re.search(rf"\b\d+\s+{first}\s+KTLBL\b", line)
        for line in editor_lines
    ):
        missing.append("fixture-row-blame")
    return missing


def cell_grid_text(cells: Dict[str, object]) -> str:
    """Render one `read_cells` response for diagnostics without another read."""
    rows = int(cells.get("rows", 0))
    cols = int(cells.get("cols", 0))
    grid = [[" " for _col in range(cols)] for _row in range(rows)]
    for cell in cells.get("cells", []):
        if not isinstance(cell, dict):
            continue
        row = cell.get("row")
        col = cell.get("col")
        ch = cell.get("ch")
        if isinstance(row, int) and isinstance(col, int) and isinstance(ch, str):
            if 0 <= row < rows and 0 <= col < cols:
                grid[row][col] = ch
    return "\n".join("".join(row).rstrip() for row in grid)


def nvim_split_command(
    left_marker: str,
    right_marker: str,
    configured: bool,
    *,
    windows: Optional[bool] = None,
) -> str:
    base = "nvim -n" if configured else "nvim --clean -n"
    left_expression = nvim_string_expression(left_marker, windows=windows)
    left_line_expression = nvim_string_expression(
        left_marker + "_LINE_2", windows=windows
    )
    right_expression = nvim_string_expression(right_marker, windows=windows)
    right_line_expression = nvim_string_expression(
        right_marker + "_LINE_2", windows=windows
    )
    return (
        f'{base} {nvim_pid_record_arg()} '
        '"+set termguicolors cursorline laststatus=2" '
        f'"+call setline(1, [{left_expression}, {left_line_expression}])" '
        '"+vsplit" '
        '"+wincmd l" '
        '"+enew" '
        f'"+call setline(1, [{right_expression}, {right_line_expression}])" '
        '"+wincmd h"'
    )


def exit_nvim(live: LiveKettle) -> None:
    # Configured distributions can surface a plugin error behind Neovim's
    # hit-enter prompt even after the requested marker is visible. Enter first
    # dismisses that prompt (and is harmless in a normal-mode buffer), then the
    # remaining keys force every window to close. Keep these as separate PTY
    # writes because Neovim deliberately flushes queued typeahead when it
    # dismisses the prompt.
    live.ctl(
        "send_keys",
        params={"keys": ["enter"]},
        timeout=8,
    )
    time.sleep(0.2)
    live.ctl(
        "send_keys",
        params={"keys": ["escape", ":", "q", "a", "l", "l", "!", "enter"]},
        timeout=8,
    )


def exit_nvim_to_shell(
    live: LiveKettle,
    shell_target: AgentShellTarget,
    sandbox_path: str,
    exit_probe: str,
    shell_marker: str,
) -> None:
    exit_nvim(live)
    time.sleep(0.6)
    marked_command = command_with_marker(
        exit_probe,
        shell_marker,
        windows=shell_target.powershell,
    )
    try:
        live_shell_command(
            live,
            marked_command,
            shell_marker,
            timeout_ms=2500 if shell_target.mode == "wsl" else 10000,
        )
    except SystemExit as error:
        if "timed out waiting" not in str(error):
            raise
        # A configured distribution can expose more than one asynchronous
        # hit-enter prompt. Retry the bounded normal-mode quit sequence before
        # escalating to target-specific process cleanup.
        for _attempt in range(2):
            exit_nvim(live)
            time.sleep(0.6)
            try:
                live_shell_command(
                    live,
                    marked_command,
                    shell_marker,
                    timeout_ms=10000,
                )
                return
            except SystemExit as retry_error:
                if "timed out waiting" not in str(retry_error):
                    raise
                error = retry_error
        if shell_target.mode != "wsl":
            raise error
        # An isolated AstroNvim config may surface repeated asynchronous plugin
        # errors behind hit-enter prompts. Stop only Neovim processes carrying
        # this unique sandbox environment, then verify the shell responds.
        shell_target.terminate_nvim_sandbox_host(sandbox_path)
        time.sleep(0.5)
        live_shell_command(
            live,
            marked_command,
            shell_marker,
            timeout_ms=10000,
        )


def new_tmux_socket_name() -> str:
    return f"kettle-smoke-{secrets.token_hex(12)}"


def parse_tmux_version(output: str) -> Optional[Tuple[int, int]]:
    match = re.fullmatch(
        r"tmux ([0-9]+)\.([0-9]+)(?:[a-z])?(?:[-+][A-Za-z0-9._-]+)?",
        output.strip(),
    )
    if match is None:
        return None
    return int(match.group(1)), int(match.group(2))


def tmux_da1_has_sixel(reply_hex: str) -> Optional[bool]:
    try:
        reply = bytes.fromhex(reply_hex)
    except ValueError:
        return None
    match = re.fullmatch(rb"\x1b\[\?([0-9]+(?:;[0-9]+)*)c", reply)
    if match is None:
        return None
    features = match.group(1).decode("ascii").split(";")
    return "4" in features


def tmux_cell_size_from_reply(reply_hex: str) -> Optional[Tuple[int, int]]:
    try:
        reply = bytes.fromhex(reply_hex)
    except ValueError:
        return None
    match = re.fullmatch(rb"\x1b\[6;([0-9]+);([0-9]+)t", reply)
    if match is None:
        return None
    height = int(match.group(1))
    width = int(match.group(2))
    return width, height


def terminal_query_python_code(
    query: bytes, marker: str, terminator: bytes
) -> str:
    if (
        not query
        or len(query) > 32
        or re.fullmatch(r"[A-Z0-9_]+=", marker) is None
        or len(terminator) != 1
    ):
        raise ValueError("invalid bounded terminal-query fixture")
    split = max(1, len(marker) // 2)
    left, right = marker[:split], marker[split:]
    script = (
        "import os,select,termios,time,tty\n"
        "old=termios.tcgetattr(0)\n"
        "data=bytearray()\n"
        "deadline=time.monotonic()+2\n"
        "try:\n"
        "    tty.setraw(0)\n"
        f"    os.write(1,bytes.fromhex({query.hex()!r}))\n"
        "    while len(data)<64:\n"
        "        remaining=deadline-time.monotonic()\n"
        "        if remaining<=0:\n"
        "            break\n"
        "        ready,_,_=select.select([0],[],[],remaining)\n"
        "        if not ready:\n"
        "            break\n"
        "        chunk=os.read(0,64-len(data))\n"
        "        if not chunk:\n"
        "            break\n"
        "        data.extend(chunk)\n"
        f"        if data.endswith(bytes.fromhex({terminator.hex()!r})):\n"
        "            break\n"
        "finally:\n"
        "    termios.tcsetattr(0,termios.TCSADRAIN,old)\n"
        f"print({left!r}+{right!r}+data.hex())"
    )
    return f"exec({script!r})"


def probe_tmux_sixel_capability(
    shell_target: AgentShellTarget,
) -> Dict[str, object]:
    """Detect tmux's compile-time SIXEL gate through its inner DA1 reply."""
    version_cp = shell_target.run_command(["tmux", "-V"], timeout=5)
    version_text = version_cp.stdout.strip()
    result: Dict[str, object] = {
        "version": version_text,
        "supported": None,
        "capability_source": "unverified",
    }
    version = parse_tmux_version(version_text)
    if version_cp.returncode != 0 or version is None:
        result["reason"] = (
            "could not parse tmux -V output: "
            f"rc={version_cp.returncode} output={version_text!r}"
        )
        return result
    result["version_major"] = version[0]
    result["version_minor"] = version[1]
    if version < (3, 4):
        result.update(
            {
                "supported": False,
                "capability_source": "version",
                "reason": "tmux SIXEL requires tmux 3.4 or newer",
            }
        )
        return result
    if not shell_target.command_available("python3"):
        result["reason"] = (
            "python3 is unavailable, so the tmux build's inner DA1 response "
            "could not be queried"
        )
        return result

    socket = new_tmux_socket_name()
    python = shell_target.require_command_path("python3")
    probe_code = terminal_query_python_code(
        b"\x1b[c", "KETTLE_TMUX_DA1=", b"c"
    )
    pane_command = f"{shlex.join([python, '-c', probe_code])}; sleep 15"
    capture = ""
    started = False
    try:
        start_cp = shell_target.run_command(
            [
                "tmux",
                "-L",
                socket,
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "kettle_sixel_probe",
                pane_command,
            ],
            timeout=5,
        )
        if start_cp.returncode != 0:
            result["reason"] = (
                "could not start the private tmux capability probe: "
                f"rc={start_cp.returncode} stderr={start_cp.stderr.strip()!r}"
            )
            return result
        started = True
        for _ in range(30):
            capture_cp = shell_target.run_command(
                [
                    "tmux",
                    "-L",
                    socket,
                    "capture-pane",
                    "-p",
                    "-t",
                    "kettle_sixel_probe:0.0",
                    "-S",
                    "-10",
                ],
                timeout=5,
            )
            if capture_cp.returncode == 0:
                capture = capture_cp.stdout
                if "KETTLE_TMUX_DA1=" in capture:
                    break
            time.sleep(0.1)
        match = re.search(r"KETTLE_TMUX_DA1=([0-9a-f]*)", capture)
        if match is None:
            result["reason"] = (
                "the private tmux pane did not return a bounded DA1 response"
            )
            return result
        reply_hex = match.group(1)
        supported = tmux_da1_has_sixel(reply_hex)
        result["da1_hex"] = reply_hex
        if supported is None:
            result["reason"] = (
                "tmux returned a malformed DA1 response; build capability "
                "remains unverified"
            )
            return result

        result.update(
            {
                "supported": supported,
                "capability_source": "inner-da1-feature-4",
            }
        )
        if version >= (3, 6):
            format_cp = shell_target.run_command(
                [
                    "tmux",
                    "-L",
                    socket,
                    "display-message",
                    "-p",
                    "#{sixel_support}",
                ],
                timeout=5,
            )
            format_value = format_cp.stdout.strip()
            result["sixel_support_format"] = format_value
            if (
                format_cp.returncode != 0
                or format_value not in {"0", "1"}
                or (format_value == "1") != supported
            ):
                result.update(
                    {
                        "supported": None,
                        "capability_source": "conflicting-probes",
                        "reason": (
                            "tmux's DA1 response disagrees with its "
                            "#{sixel_support} build-capability format"
                        ),
                    }
                )
                return result
        if not supported:
            result["reason"] = (
                "tmux was not built with --enable-sixel "
                "(inner DA1 omits feature code 4)"
            )
        return result
    finally:
        if started:
            cleanup_tmux_server(shell_target, socket)


def cleanup_tmux_server(
    shell_target: AgentShellTarget, tmux_socket: str
) -> None:
    """Stop one private tmux server or prove that it is already absent."""
    if re.fullmatch(r"kettle-smoke-[0-9a-f]{24}", tmux_socket) is None:
        raise ValueError(f"refusing unsafe tmux socket name: {tmux_socket}")
    cp = shell_target.run_command(
        ["tmux", "-L", tmux_socket, "kill-server"], timeout=5
    )
    if cp.returncode == 0:
        return
    diagnostic = f"{cp.stdout}\n{cp.stderr}".lower()
    server_absent = (
        "no server running" in diagnostic
        or "failed to connect to server" in diagnostic
        or (
            "error connecting to" in diagnostic
            and "no such file or directory" in diagnostic
        )
    )
    if server_absent:
        return
    raise RuntimeError(
        "failed to clean up private tmux server "
        f"{tmux_socket}: rc={cp.returncode} "
        f"stdout={cp.stdout!r} stderr={cp.stderr!r}"
    )


def tmux_session_command(
    tmux_socket: str, bash_path: str, *, sixel: bool = False
) -> str:
    command = shlex.join([bash_path, "--noprofile", "--norc"])
    feature = "-T sixel " if sixel else ""
    return (
        f"tmux -L {shlex.quote(tmux_socket)} -f /dev/null "
        f"{feature}"
        "new-session -A -s kettle_smoke "
        f"{shlex.quote(command)}"
    )


def tmux_sixel_marker_command(marker: str) -> str:
    # Two 24x6 full-column bands form a 24x12 pure-magenta raster. The marker
    # is assembled separately so shell command echo cannot satisfy wait_for.
    # Clear first so a long capability-query command cannot leave the fixture
    # on the bottom row where the image and marker would immediately scroll.
    return (
        "printf '\\033[2J\\033[H"
        "\\033Pq#1;2;100;0;100!24~-!24~\\033\\\\'; "
        + posix_runtime_marker_command(marker)
    )


def tmux_cell_size_query_command(python_path: str) -> str:
    code = terminal_query_python_code(
        b"\x1b[16t", "KETTLE_TMUX_CELL=", b"t"
    )
    return shlex.join([python_path, "-c", code])


def posix_runtime_marker_command(marker: str) -> str:
    left, right = split_marker(marker)
    return (
        "printf '%s%s\\n' "
        f"{shell_quote(left, windows=False)} "
        f"{shell_quote(right, windows=False)}"
    )


def tmux_split_commands(
    tmux_socket: str,
    bash_path: str,
    left_marker: str,
    right_marker: str,
) -> List[List[str]]:
    command = shlex.join([bash_path, "--noprofile", "--norc"])
    return [
        [
            "tmux",
            "-L",
            tmux_socket,
            "set-option",
            "-g",
            "default-shell",
            bash_path,
        ],
        [
            "tmux",
            "-L",
            tmux_socket,
            "set-option",
            "-g",
            "default-command",
            command,
        ],
        [
            "tmux",
            "-L",
            tmux_socket,
            "split-window",
            "-h",
            "-t",
            "kettle_smoke:0.0",
        ],
        [
            "tmux",
            "-L",
            tmux_socket,
            "send-keys",
            "-t",
            "kettle_smoke:0.0",
            posix_runtime_marker_command(left_marker),
            "C-m",
        ],
        [
            "tmux",
            "-L",
            tmux_socket,
            "send-keys",
            "-t",
            "kettle_smoke:0.1",
            posix_runtime_marker_command(right_marker),
            "C-m",
        ],
    ]


def run_agent_tui(
    kettle: str, root: Path, shell_target: AgentShellTarget
) -> Path:
    out = root / (
        f"agent-tui-{shell_target.label}-{time.strftime('%Y%m%d-%H%M%S')}"
    )
    out.mkdir(parents=True, exist_ok=True)
    provenance_before = agent_tui_provenance(kettle, shell_target)
    provenance_path = out / "provenance.json"
    provenance_path.write_text(
        json.dumps({"before": provenance_before}, indent=2) + "\n",
        encoding="utf-8",
    )
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #090909",
                "foreground = #f5f5f5",
                "minimum-contrast = 0",
                "window-padding-x = 8",
                "window-padding-y = 8",
                "cursor-blink = false",
                "window-width = 120",
                "window-height = 36",
            ]
        )
        + "\n"
    )
    extra_args = shell_target.launch_args()
    states: List[Dict[str, object]] = []
    lazyvcs_snapshot_after: Optional[Dict[str, object]] = None
    lazyvcs_loaded_source_after: Optional[Dict[str, object]] = None
    probes: List[Dict[str, object]] = []
    run_auth_smoke = env_flag("KETTLE_AGENT_AUTH_SMOKE")
    require_auth_smoke = env_strict("KETTLE_AGENT_AUTH_SMOKE")
    nvim_available = shell_target.command_available("nvim")
    configured_nvim_available = (
        nvim_available and shell_target.configured_nvim_available()
    )
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        initial_command = shell_target.initial_shell_command()
        if initial_command is not None:
            setup_marker = "KETTLE_AGENT_TUI_SHELL_TARGET_READY"
            live_shell_command(
                live,
                command_with_marker(
                    initial_command,
                    setup_marker,
                    windows=shell_target.powershell,
                ),
                setup_marker,
            )

        marker = "KETTLE_AGENT_TUI_SHELL_SMOKE"
        shell_probe = (
            "Write-Output shell-live-ok"
            if shell_target.powershell
            else "printf 'shell-live-ok\\n'"
        )
        live_shell_command(
            live,
            command_with_marker(
                shell_probe, marker, windows=shell_target.powershell
            ),
            marker,
        )
        states.append(capture_live_state(live, out, "shell"))
        probes.append(
            {
                "name": "shell",
                "status": "ok",
                "mode": shell_target.mode,
                "distro": shell_target.wsl_distro,
            }
        )

        prompt_marker = "KETTLE_AGENT_TUI_PROMPT_SHAPE"
        live_shell_command(
            live,
            prompt_marker_command(
                prompt_marker, windows=shell_target.powershell
            ),
            prompt_marker,
        )
        prompt_screen = live.json_ctl("read_screen")
        prompt_text = screen_text(prompt_screen)
        if shell_target.powershell:
            prompt_visible = prompt_marker in prompt_text
        else:
            prompt_visible = f"\u279c  ~ {prompt_marker}" in prompt_text
        if not prompt_visible:
            raise SystemExit("agent-tui smoke: prompt-shaped marker is not visible")
        states.append(capture_live_state(live, out, "prompt-shape"))
        probes.append({"name": "prompt-shape", "status": "ok"})

        if shell_target.powershell:
            for queued_input, label in (
                (False, "codex-active-placeholder-cursor"),
                (True, "codex-active-queued-input-cursor"),
            ):
                command, cursor_row, cursor_col = codex_cursor_fixture_command(
                    queued_input=queued_input, windows=True
                )
                live.ctl("send_text", params={"text": command}, timeout=8)
                live.ctl("send_keys", params={"keys": ["enter"]}, timeout=8)
                live.wait_for_text("gpt-5.5 high", timeout_ms=12000, quiet_ms=250)
                fixture_screen = live.json_ctl("read_screen")
                fixture_cursor = fixture_screen.get("cursor")
                if fixture_cursor != [cursor_row, cursor_col] or not fixture_screen.get(
                    "cursor_visible"
                ):
                    raise SystemExit(
                        f"agent-tui smoke: {label} left the wrong parsed cursor: "
                        f"cursor={fixture_cursor} visible={fixture_screen.get('cursor_visible')}"
                    )
                state = capture_live_state(live, out, label)
                geo = live.json_ctl("ui_geometry")
                content = geo.get("content")
                cell = geo.get("cell")
                if not isinstance(content, dict) or not isinstance(cell, dict):
                    raise SystemExit(f"agent-tui smoke: missing geometry for {label}: {geo}")
                shot = Path(str(state["screenshot"]))
                _width, _height, rgba_rows = read_rgba_png(shot)
                cell_w = float(cell.get("width", 8.0))
                cell_h = float(cell.get("height", 16.0))
                padding = geo.get("padding", {"x": 8.0, "y": 8.0})
                if not isinstance(padding, dict):
                    raise SystemExit(f"agent-tui smoke: invalid padding geometry for {label}")
                x0 = (
                    float(content.get("x", 0.0))
                    + float(padding.get("x", 0.0))
                    + cursor_col * cell_w
                    + 1.0
                )
                y0 = (
                    float(content.get("y", 0.0))
                    + float(padding.get("y", 0.0))
                    + cursor_row * cell_h
                    + 1.0
                )
                bright_cell = bright_pixels_in_rect(
                    rgba_rows,
                    x0,
                    y0,
                    x0 + cell_w - 2.0,
                    y0 + cell_h - 2.0,
                )
                threshold = max(24, int(cell_w * cell_h * 0.16))
                cursor_drawn = bright_cell > threshold
                if cursor_drawn != queued_input:
                    raise SystemExit(
                        f"agent-tui smoke: {label} draw decision is wrong "
                        f"({bright_cell} bright pixels; threshold {threshold})"
                    )
                probes.append(
                    {
                        "name": label,
                        "status": "ok",
                        "cursor": fixture_cursor,
                        "cursor_drawn": cursor_drawn,
                        "bright_pixels": bright_cell,
                        "threshold": threshold,
                    }
                )
                states.append(state)
                live.ctl("send_keys", params={"keys": ["ctrl+c"]}, timeout=8)
                time.sleep(0.3)
        elif platform.system() == "Windows":
            for label in (
                "codex-active-placeholder-cursor",
                "codex-active-queued-input-cursor",
            ):
                probes.append(
                    {
                        "name": label,
                        "status": "skipped",
                        "reason": "native PowerShell/ConPTY cursor fixture; WSL is a separate target",
                    }
                )

        for tool in ("codex", "claude"):
            if not shell_target.command_available(tool):
                probes.append(
                    {
                        "name": tool,
                        "status": "skipped",
                        "reason": shell_target.command_unavailable_reason(tool),
                    }
                )
                continue
            marker = f"KETTLE_AGENT_TUI_{tool.upper()}_SMOKE"
            live_shell_command(
                live,
                command_with_marker(
                    f"{tool} --version",
                    marker,
                    windows=shell_target.powershell,
                ),
                marker,
                timeout_ms=12000,
            )
            states.append(capture_live_state(live, out, tool))
            probes.append({"name": tool, "status": "ok"})
            if tool == "codex":
                help_label = "codex-exec-help"
                help_marker = "KETTLE_AGENT_TUI_CODEX_EXEC_HELP"
                expected = "Run Codex non-interactively"
                help_command = first_lines_command(
                    "codex exec --help", windows=shell_target.powershell
                )
            else:
                help_label = "claude-print-help"
                help_marker = "KETTLE_AGENT_TUI_CLAUDE_PRINT_HELP"
                expected = "non-interactive output"
                help_command = first_lines_command(
                    "claude --print --help", windows=shell_target.powershell
                )
            live_shell_command(
                live,
                command_with_marker(
                    help_command,
                    help_marker,
                    windows=shell_target.powershell,
                ),
                help_marker,
                timeout_ms=12000,
            )
            # High-DPI windows can have fewer visible rows than the 22-line
            # help excerpt. Include bounded scrollback so the lead-line
            # assertion does not depend on physical DPI or font metrics.
            help_screen = live.json_ctl(
                "read_screen", params={"scrollback_lines": 80}
            )
            if expected not in screen_text(help_screen):
                raise SystemExit(f"agent-tui smoke: {help_label} did not render expected help text")
            states.append(capture_live_state(live, out, help_label))
            probes.append({"name": help_label, "status": "ok"})
            if run_auth_smoke:
                auth_label = f"{tool}-auth-session"
                auth_marker = f"KETTLE_AGENT_TUI_{tool.upper()}_AUTH_SESSION"
                output_marker = f"KETTLE_AGENT_TUI_{tool.upper()}_AUTH_OUTPUT_BEGIN"
                done_marker = f"KETTLE_AGENT_TUI_{tool.upper()}_AUTH_DONE"
                live.ctl(
                    "send_text",
                    params={
                        "text": agent_auth_command(
                            tool,
                            auth_marker,
                            output_marker,
                            done_marker,
                            windows=shell_target.powershell,
                        )
                    },
                    timeout=8,
                )
                live.ctl("send_keys", params={"keys": ["enter"]}, timeout=8)
                # The bare marker appears in the shell's echoed command before
                # the agent starts. The emitted marker includes `:<exit-code>`;
                # wait for that shape so agent probes remain serialized.
                live.wait_for_text(f"{done_marker}:", timeout_ms=180000, quiet_ms=500)
                auth_screen = live.json_ctl("read_screen", params={"scrollback_lines": 240})
                auth_text = screen_text(auth_screen)
                rc = done_marker_status(auth_text, done_marker)
                marker_emitted = agent_output_contains_marker(
                    auth_text, output_marker, done_marker, auth_marker
                )
                status = "ok" if rc == 0 and marker_emitted else "auth_failed"
                reason = None
                if rc is None:
                    status = "marker_missing"
                    reason = "done marker exit status was not visible in read_screen"
                elif rc != 0:
                    reason = f"{tool} exited {rc}; likely missing/expired external authentication"
                elif not marker_emitted:
                    status = "marker_missing"
                    reason = (
                        f"{tool} exited 0 but expected auth marker was not emitted "
                        "inside the framed agent output"
                    )
                state = capture_live_state(live, out, auth_label)
                states.append(state)
                probe = {"name": auth_label, "status": status, "exit_code": rc}
                if reason is not None:
                    probe["reason"] = reason
                probes.append(probe)
                if status != "ok" and require_auth_smoke:
                    raise SystemExit(f"agent-tui smoke: {auth_label} failed: {reason}")

        if shell_target.powershell or not shell_target.command_available("tmux"):
            reason = (
                "native Windows shell target"
                if shell_target.powershell
                else shell_target.command_unavailable_reason("tmux")
            )
            probes.append(
                {"name": "tmux", "status": "skipped", "reason": reason}
            )
            probes.append(
                {"name": "tmux-split", "status": "skipped", "reason": reason}
            )
            probes.append(
                {"name": "tmux-sixel", "status": "skipped", "reason": reason}
            )
        else:
            tmux_sixel = probe_tmux_sixel_capability(shell_target)
            tmux_sixel_supported = tmux_sixel.get("supported") is True
            if tmux_sixel_supported:
                probes.append(
                    {
                        "name": "tmux-sixel-capability",
                        "status": "ok",
                        **tmux_sixel,
                    }
                )
            else:
                probes.append(
                    {
                        "name": "tmux-sixel-capability",
                        "status": "skipped",
                        **tmux_sixel,
                    }
                )
            tmux_socket = new_tmux_socket_name()
            tmux_bash = shell_target.require_command_path("bash")
            live.add_post_exit_cleanup(
                lambda target=shell_target, socket=tmux_socket: cleanup_tmux_server(
                    target, socket
                )
            )
            # Keep markers below one half-pane on a small/HiDPI smoke window.
            # `wait_for` reads physical grid rows; a marker soft-wrapped by the
            # tmux split is intentionally not rejoined into one text match.
            tmux_marker = "KTL_TMUX_OK"
            tmux_left_marker = "KTL_TMUX_LEFT"
            tmux_right_marker = "KTL_TMUX_RIGHT"
            live.ctl(
                "send_text",
                params={
                    "text": tmux_session_command(
                        tmux_socket,
                        tmux_bash,
                        sixel=tmux_sixel_supported,
                    )
                },
            )
            live.ctl("send_keys", params={"keys": ["enter"]})
            time.sleep(1.0)
            live.ctl(
                "send_text",
                params={
                    "text": posix_runtime_marker_command(tmux_marker)
                },
            )
            live.ctl("send_keys", params={"keys": ["enter"]})
            live.wait_for_text(tmux_marker, timeout_ms=12000, quiet_ms=500)
            states.append(capture_live_state(live, out, "tmux"))
            if tmux_sixel_supported:
                tmux_python = shell_target.require_command_path("python3")
                live.ctl(
                    "send_text",
                    params={
                        "text": tmux_cell_size_query_command(tmux_python)
                    },
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                live.wait_for_text(
                    "KETTLE_TMUX_CELL=", timeout_ms=12000, quiet_ms=500
                )
                cell_screen = live.json_ctl("read_screen")
                cell_match = re.search(
                    r"KETTLE_TMUX_CELL=([0-9a-f]*)",
                    screen_text(cell_screen),
                )
                cell_size = (
                    tmux_cell_size_from_reply(cell_match.group(1))
                    if cell_match is not None
                    else None
                )
                if cell_size is None:
                    probes.append(
                        {
                            "name": "tmux-sixel",
                            "status": "skipped",
                            "reason": (
                                "tmux's runtime cell-pixel geometry response "
                                "was missing or malformed"
                            ),
                        }
                    )
                else:
                    cell_width, cell_height = cell_size
                    tmux_sixel_marker = "KTL_TMUX_SIXEL_OK"
                    live.ctl(
                        "send_text",
                        params={
                            "text": tmux_sixel_marker_command(
                                tmux_sixel_marker
                            )
                        },
                    )
                    live.ctl("send_keys", params={"keys": ["enter"]})
                    live.wait_for_text(
                        tmux_sixel_marker, timeout_ms=12000, quiet_ms=500
                    )
                    sixel_screen = live.json_ctl("read_screen")
                    if cell_width == 0 or cell_height == 0:
                        if "SIXEL IMAGE (" not in screen_text(sixel_screen):
                            raise SystemExit(
                                "agent-tui smoke: tmux reported zero pixel "
                                "geometry but did not expose its SIXEL text "
                                "fallback"
                            )
                        states.append(
                            capture_live_state(
                                live, out, "tmux-sixel-fallback"
                            )
                        )
                        probes.append(
                            {
                                "name": "tmux-sixel",
                                "status": "skipped",
                                "reason": (
                                    "tmux reported zero outer-terminal pixel "
                                    "cell geometry; its SIXEL text fallback "
                                    "was observed"
                                ),
                                "cell_width": cell_width,
                                "cell_height": cell_height,
                                "fallback_observed": True,
                            }
                        )
                    else:
                        sixel_state = capture_live_state(
                            live, out, "tmux-sixel"
                        )
                        (
                            before_width,
                            before_height,
                            before_rows,
                        ) = read_rgba_png(
                            live_state_screenshot_path(out, "tmux")
                        )
                        (
                            after_width,
                            after_height,
                            after_rows,
                        ) = read_rgba_png(
                            live_state_screenshot_path(out, "tmux-sixel")
                        )
                        if (before_width, before_height) != (
                            after_width,
                            after_height,
                        ):
                            raise SystemExit(
                                "agent-tui smoke: tmux SIXEL screenshots "
                                "changed size"
                            )
                        widest, stacked = magenta_block_metrics(after_rows)
                        added = added_magenta_pixel_count(
                            before_rows, after_rows
                        )
                        if widest < 12 or stacked < 6 or added < 64:
                            raise SystemExit(
                                "agent-tui smoke: build-capable tmux did not "
                                "render the 24x12 SIXEL fixture through Kettle "
                                f"(widest={widest}, stacked={stacked}, "
                                f"added={added})"
                            )
                        sixel_state.update(
                            {
                                "magenta_widest_run": widest,
                                "magenta_stacked_rows": stacked,
                                "magenta_pixels_added": added,
                            }
                        )
                        states.append(sixel_state)
                        probes.append(
                            {
                                "name": "tmux-sixel",
                                "status": "ok",
                                "terminal_feature": "sixel",
                                "fixture": "24x12-magenta",
                                "cell_width": cell_width,
                                "cell_height": cell_height,
                                "magenta_widest_run": widest,
                                "magenta_stacked_rows": stacked,
                                "magenta_pixels_added": added,
                            }
                        )
            else:
                probes.append(
                    {
                        "name": "tmux-sixel",
                        "status": "skipped",
                        "reason": tmux_sixel.get(
                            "reason",
                            "tmux SIXEL build capability was not verified",
                        ),
                    }
                )
            tmux_cmds = tmux_split_commands(
                tmux_socket,
                tmux_bash,
                tmux_left_marker,
                tmux_right_marker,
            )
            for cmd in tmux_cmds:
                cp = shell_target.run_command(cmd, timeout=5)
                if cp.returncode != 0:
                    raise SystemExit(
                        "agent-tui smoke: tmux split workflow failed:\n"
                        + " ".join(cmd)
                        + f"\nstdout={cp.stdout}\nstderr={cp.stderr}"
                    )
            live.wait_for_text(tmux_left_marker, timeout_ms=12000, quiet_ms=500)
            live.wait_for_text(tmux_right_marker, timeout_ms=12000, quiet_ms=500)
            tmux_split_screen = live.json_ctl("read_screen")
            tmux_split_text = screen_text(tmux_split_screen)
            (out / "tmux-split.screen.json").write_text(json.dumps(tmux_split_screen, indent=2) + "\n")
            if tmux_left_marker not in tmux_split_text or tmux_right_marker not in tmux_split_text:
                raise SystemExit("agent-tui smoke: tmux split markers are not both visible")
            states.append(capture_live_state(live, out, "tmux-split"))
            live.ctl("send_text", params={"text": "exit"})
            live.ctl("send_keys", params={"keys": ["enter"]})
            cleanup_tmux_server(shell_target, tmux_socket)
            tmux_exit_marker = "KETTLE_AGENT_TUI_TMUX_EXITED"
            time.sleep(0.5)
            live_shell_command(
                live,
                command_with_marker(
                    "printf 'tmux-exited\\n'",
                    tmux_exit_marker,
                    windows=False,
                ),
                tmux_exit_marker,
                timeout_ms=12000,
            )
            probes.append({"name": "tmux", "status": "ok"})
            probes.append({"name": "tmux-split", "status": "ok"})

        if not nvim_available:
            reason = shell_target.command_unavailable_reason("nvim")
            for label in (
                "nvim-clean",
                "nvim-configured",
                "nvim-lazyvcs-sidebar",
                "nvim-split-clean",
                "nvim-split-configured",
            ):
                probes.append(
                    {"name": label, "status": "skipped", "reason": reason}
                )
        else:
            sandbox_marker = "KETTLE_AGENT_TUI_NVIM_SANDBOX_READY"
            # On Windows, acquire the unpredictable named Job before the
            # sandbox exists.  The exact PowerShell pane then joins it without
            # a reusable ctl PID; Neovim and all later descendants inherit the
            # retained containment boundary.  The helper also registers
            # cleanup before returning either owned resource to this caller.
            sandbox_path, windows_sandbox_job = _create_owned_nvim_sandbox(
                shell_target, live.add_post_exit_cleanup
            )
            if windows_sandbox_job is not None:
                containment_marker = "KETTLE_AGENT_TUI_WINDOWS_JOB_READY"
                live_shell_command(
                    live,
                    command_with_marker(
                        windows_sandbox_job.powershell_assign_current_process_command(),
                        containment_marker,
                        windows=True,
                    ),
                    containment_marker,
                )
            shell_target.prepare_nvim_sandbox_host(sandbox_path)
            live_shell_command(
                live,
                shell_target.nvim_sandbox_setup_command(
                    sandbox_marker, sandbox_path=sandbox_path
                ),
                sandbox_marker,
                timeout_ms=360000,
            )
            for label, configured in (("nvim-clean", False), ("nvim-configured", True)):
                if configured and not configured_nvim_available:
                    probes.append(
                        {
                            "name": label,
                            "status": "skipped",
                            "reason": (
                                "no configured Neovim/AstroNvim directory at "
                                f"{shell_target.nvim_config_source()}"
                            ),
                        }
                    )
                    continue
                marker = f"KETTLE_AGENT_TUI_{label.replace('-', '_').upper()}_SMOKE"
                live.ctl(
                    "send_text",
                    params={
                        "text": nvim_marker_command(
                            marker,
                            configured,
                            windows=shell_target.powershell,
                        )
                    },
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                # A copied AstroNvim config may bootstrap its plugin tree into
                # the disposable XDG data directory on first use. Keep clean
                # Neovim fast while allowing that isolated startup to finish.
                nvim_timeout_ms = 120000 if configured else 18000
                live.wait_for_text(
                    marker, timeout_ms=nvim_timeout_ms, quiet_ms=500
                )
                states.append(capture_live_state(live, out, label))
                shell_marker = f"{marker}_EXITED"
                exit_probe = (
                    "Write-Output nvim-exited"
                    if shell_target.powershell
                    else "printf 'nvim-exited\\n'"
                )
                exit_nvim_to_shell(
                    live,
                    shell_target,
                    sandbox_path,
                    exit_probe,
                    shell_marker,
                )
                probes.append({"name": label, "status": "ok"})
            # Configured Neovim may bootstrap lazy.nvim and LazyVCS into the
            # disposable data directory on its first startup. Establish the
            # provenance baseline only after that warm-up, then require the
            # copied plugin to exist before asking it to render anything.
            if configured_nvim_available:
                provenance_before["lazyvcs_snapshot"] = (
                    shell_target.nvim_snapshot_identity(sandbox_path)
                )
                if not provenance_before["lazyvcs_snapshot"].get("present"):
                    raise SystemExit(
                        "agent-tui smoke: configured Neovim did not provide "
                        "LazyVCS inside the prepared snapshot"
                    )
                provenance_path.write_text(
                    json.dumps({"before": provenance_before}, indent=2) + "\n",
                    encoding="utf-8",
                )
            # LazyVCS leg. Only meaningful against the configured runtime,
            # because that is what has the plugin; skipped otherwise rather
            # than silently passing.
            if not configured_nvim_available:
                probes.append(
                    {
                        "name": "nvim-lazyvcs-sidebar",
                        "status": "skipped",
                        "reason": (
                            "no configured Neovim/AstroNvim directory at "
                            f"{shell_target.nvim_config_source()}"
                        ),
                    }
                )
            else:
                fixture_token = secrets.token_hex(4).upper()
                lazyvcs_repo = shell_target.target_join(
                    sandbox_path, f"lazyvcs-smoke-{fixture_token.lower()}"
                )
                live.ctl(
                    "send_text",
                    params={
                        "text": lazyvcs_repo_setup_command(
                            lazyvcs_repo,
                            fixture_token,
                            windows=shell_target.powershell,
                        )
                    },
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                setup_marker = "KETTLE_LAZYVCS_REPO_READY"
                ready_probe = command_with_marker(
                    "Write-Output lazyvcs-repository-ready"
                    if shell_target.powershell
                    else "printf 'lazyvcs-repository-ready\\n'",
                    setup_marker,
                    windows=shell_target.powershell,
                )
                live.ctl("send_text", params={"text": ready_probe})
                live.ctl("send_keys", params={"keys": ["enter"]})
                live.wait_for_text(setup_marker, timeout_ms=60000, quiet_ms=500)

                marker = "KETTLE_AGENT_TUI_LAZYVCS_SMOKE"
                live.ctl(
                    "send_text",
                    params={
                        "text": lazyvcs_sidebar_command(
                            lazyvcs_repo,
                            marker,
                            windows=shell_target.powershell,
                        )
                    },
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                repo_name = (
                    posixpath.basename(lazyvcs_repo)
                    if shell_target.mode == "wsl"
                    else Path(lazyvcs_repo).name
                )
                # Same budget as `nvim-configured`: a copied AstroNvim tree may
                # bootstrap its plugins into the disposable XDG data dir first.
                lazyvcs_cells, lazyvcs_screen = (
                    live.wait_for_nvim_sidebar_evidence(
                        repo_name,
                        fixture_token,
                        timeout_ms=120000,
                        quiet_ms=500,
                    )
                )
                shell_target.wait_for_nvim_sandbox_marker(
                    sandbox_path,
                    "lazyvcs-ready",
                    marker,
                    timeout_s=10.0,
                )
                lazyvcs_text = cell_grid_text(lazyvcs_cells)
                missing = lazyvcs_screen_evidence(
                    lazyvcs_cells,
                    repo_name=repo_name,
                    fixture_token=fixture_token,
                )
                if missing:
                    raise SystemExit(
                        "agent-tui smoke: LazyVCS rendered without the expected "
                        "repository, change, gutter, and blame evidence: "
                        f"missing={missing} screen={lazyvcs_text!r}"
                    )
                provenance_before["lazyvcs_loaded_source"] = (
                    shell_target.lazyvcs_loaded_source_identity(sandbox_path)
                )
                provenance_path.write_text(
                    json.dumps({"before": provenance_before}, indent=2) + "\n",
                    encoding="utf-8",
                )
                states.append(
                    capture_live_state(
                        live,
                        out,
                        "nvim-lazyvcs-sidebar",
                        cells=lazyvcs_cells,
                        screen=lazyvcs_screen,
                    )
                )
                exit_nvim_to_shell(
                    live,
                    shell_target,
                    sandbox_path,
                    (
                        "Write-Output lazyvcs-exited"
                        if shell_target.powershell
                        else "printf 'lazyvcs-exited\\n'"
                    ),
                    f"{marker}_EXITED",
                )
                probes.append({"name": "nvim-lazyvcs-sidebar", "status": "ok"})
            for label, configured in (
                ("nvim-split-clean", False),
                ("nvim-split-configured", True),
            ):
                if configured and not configured_nvim_available:
                    probes.append(
                        {
                            "name": label,
                            "status": "skipped",
                            "reason": (
                                "no configured Neovim/AstroNvim directory at "
                                f"{shell_target.nvim_config_source()}"
                            ),
                        }
                    )
                    continue
                base = label.replace("-", "_").upper()
                # A 120x36 logical window can be only ~61 columns on a HiDPI
                # display; each Neovim split is then about 30 cells wide.
                # `wait_for` matches physical rows rather than rejoining soft
                # wraps, so keep each split marker well below that boundary.
                mode_tag = "CFG" if configured else "CLEAN"
                left_marker = f"KTL_NV_{mode_tag}_L"
                right_marker = f"KTL_NV_{mode_tag}_R"
                live.ctl(
                    "send_text",
                    params={
                        "text": nvim_split_command(
                            left_marker,
                            right_marker,
                            configured,
                            windows=shell_target.powershell,
                        )
                    },
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                nvim_timeout_ms = 120000 if configured else 30000
                live.wait_for_text(
                    left_marker, timeout_ms=nvim_timeout_ms, quiet_ms=500
                )
                live.wait_for_text(
                    right_marker, timeout_ms=nvim_timeout_ms, quiet_ms=500
                )
                split_screen = live.json_ctl("read_screen")
                split_text = screen_text(split_screen)
                if left_marker not in split_text or right_marker not in split_text:
                    raise SystemExit(f"agent-tui smoke: {label} split markers are not both visible")
                states.append(capture_live_state(live, out, label))
                shell_marker = f"KETTLE_AGENT_TUI_{base}_EXITED"
                exit_nvim_to_shell(
                    live,
                    shell_target,
                    sandbox_path,
                    (
                        "Write-Output nvim-split-exited"
                        if shell_target.powershell
                        else "printf 'nvim-split-exited\\n'"
                    ),
                    shell_marker,
                )
                probes.append({"name": label, "status": "ok"})
            lazyvcs_snapshot_after = shell_target.nvim_snapshot_identity(
                sandbox_path
            )
            if "lazyvcs_loaded_source" in provenance_before:
                lazyvcs_loaded_source_after = (
                    shell_target.lazyvcs_loaded_source_identity(sandbox_path)
                )
            cleanup_marker = "KETTLE_AGENT_TUI_NVIM_SANDBOX_RELEASED"
            live_shell_command(
                live,
                shell_target.nvim_sandbox_release_command(cleanup_marker),
                cleanup_marker,
                timeout_ms=120000,
            )

    provenance_after = agent_tui_provenance(kettle, shell_target)
    if "lazyvcs_snapshot" in provenance_before:
        provenance_after["lazyvcs_snapshot"] = lazyvcs_snapshot_after
    if "lazyvcs_loaded_source" in provenance_before:
        provenance_after["lazyvcs_loaded_source"] = (
            lazyvcs_loaded_source_after
        )
    stable_fields = (
        "executable",
        "executable_sha256",
        "harness",
        "harness_sha256",
        "git_commit",
        "git_dirty",
        "git_status_sha256",
        "git_status_entries",
        "source_state_sha256",
        "target",
        "lazyvcs_snapshot",
        "lazyvcs_loaded_source",
    )
    changed = [
        field
        for field in stable_fields
        if provenance_before.get(field) != provenance_after.get(field)
    ]
    provenance_path.write_text(
        json.dumps(
            {
                "before": provenance_before,
                "after": provenance_after,
                "stable": not changed,
                "changed_fields": changed,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    if changed:
        raise SystemExit(
            "agent-tui smoke: executable/harness provenance changed during run: "
            + ", ".join(changed)
        )

    ok = [p for p in probes if p.get("status") == "ok"]
    if not ok:
        raise SystemExit("agent-tui smoke: no probes ran")
    (out / "analysis.json").write_text(
        json.dumps(
            {
                "provenance": provenance_before,
                "shell": {
                    "mode": shell_target.mode,
                    "wsl_distro": shell_target.wsl_distro,
                    "configured_nvim_source": shell_target.nvim_config_source(),
                    "nvim_data_source": shell_target.nvim_data_source(),
                    "configured_nvim_copied_to_sandbox": configured_nvim_available,
                },
                "probes": probes,
                "states": states,
            },
            indent=2,
        )
        + "\n"
    )
    return out


def run_search_history(kettle: str, root: Path) -> Path:
    """Exercise Ctrl+Shift+F against matches that exist only in scrollback."""
    out = root / f"search-history-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "record = off",
                "background = #090909",
                "foreground = #f5f5f5",
                "minimum-contrast = 0",
                "window-padding-x = 8",
                "window-padding-y = 8",
                "window-width = 100",
                "window-height = 30",
                "scrollback = 5000",
                "scrollback-bytes = 0",
                "search-wrap = true",
                "invert-search = false",
                "search-case-sensitive = always",
            ]
        )
        + "\n"
    )

    query = "KETTLE_SEARCH_HISTORY_HIT"
    fixtures = [
        "KETTLE_SEARCH_HISTORY_HIT_OLD",
        "KETTLE_SEARCH_HISTORY_HIT_MIDDLE",
        "KETTLE_SEARCH_HISTORY_HIT_NEW",
    ]
    done = "KETTLE_SEARCH_HISTORY_FIXTURE_DONE"
    if platform.system() == "Windows":
        fill_command = (
            "$esc=[char]27; [Console]::Write($esc + '[2J' + $esc + '[3J' + $esc + '[H'); "
            "1..1800 | ForEach-Object { "
            "if ($_ -eq 75) { Write-Output ('KETTLE_SEARCH_' + 'HISTORY_HIT_OLD') } "
            "elseif ($_ -eq 1050) { Write-Output ('KETTLE_SEARCH_' + 'HISTORY_HIT_MIDDLE') } "
            "elseif ($_ -eq 1650) { Write-Output ('KETTLE_SEARCH_' + 'HISTORY_HIT_NEW') } "
            "else { 'KETTLE_SEARCH_FILL_{0:D4}' -f $_ } }; "
            "Write-Output ('KETTLE_SEARCH_HISTORY_FIXTURE_' + 'DONE')"
        )
        extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
    else:
        fill_command = (
            "printf '\\033[2J\\033[3J\\033[H'; "
            "for i in $(seq 1 1800); do case \"$i\" in "
            "75) printf '%s%s\\n' KETTLE_SEARCH_ HISTORY_HIT_OLD ;; "
            "1050) printf '%s%s\\n' KETTLE_SEARCH_ HISTORY_HIT_MIDDLE ;; "
            "1650) printf '%s%s\\n' KETTLE_SEARCH_ HISTORY_HIT_NEW ;; "
            "*) printf 'KETTLE_SEARCH_FILL_%04d\\n' \"$i\" ;; esac; done; "
            "printf '%s%s\\n' KETTLE_SEARCH_HISTORY_FIXTURE_ DONE"
        )
        extra_args = []

    states: List[Dict[str, object]] = []
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live_shell_command(live, fill_command, done, timeout_ms=20000)
        bottom = live.json_ctl("read_screen")
        (out / "bottom.screen.json").write_text(json.dumps(bottom, indent=2) + "\n")
        if int(bottom.get("display_offset", -1)) != 0:
            raise SystemExit(
                "search-history smoke: fixture did not settle at the live bottom: "
                f"display_offset={bottom.get('display_offset')}"
            )
        if int(bottom.get("history_size", 0)) < 1700:
            raise SystemExit(
                "search-history smoke: fixture did not create enough scrollback: "
                f"history_size={bottom.get('history_size')}"
            )
        if query in screen_text(bottom):
            raise SystemExit("search-history smoke: a search fixture is still visible at the bottom")
        states.append(capture_live_state(live, out, "bottom"))

        binding = live.json_ctl(
            "dispatch_keybind",
            {"logical": "f", "mods": "ctrl+shift"},
        )
        (out / "ctrl-shift-f.dispatch.json").write_text(
            json.dumps(binding, indent=2) + "\n"
        )
        if binding.get("dispatched") is not True or binding.get("action") != "StartSearch":
            raise SystemExit(
                "search-history smoke: Ctrl+Shift+F did not resolve to StartSearch: "
                f"{binding}"
            )

        opened = live.json_ctl("ui_geometry")
        (out / "search-open.geometry.json").write_text(json.dumps(opened, indent=2) + "\n")
        search_open = opened.get("search")
        if not isinstance(search_open, dict) or not modal_open(opened, "search"):
            raise SystemExit("search-history smoke: Ctrl+Shift+F did not open Search")
        expected_controls = {
            "editor",
            "previous",
            "next",
            "wrap",
            "case",
            "invert",
            "close",
        }
        controls = {
            str(control.get("name"))
            for control in search_open.get("controls", [])
            if isinstance(control, dict)
        }
        if controls != expected_controls:
            raise SystemExit(
                "search-history smoke: search controls do not match the interactive surface: "
                f"{sorted(controls)}"
            )
        states.append(capture_live_state(live, out, "search-open"))

        typed = live.json_ctl("dispatch_ui_key", {"keys": list(query)})
        (out / "query.dispatch.json").write_text(json.dumps(typed, indent=2) + "\n")
        if int(typed.get("keys", 0)) != len(query) or typed.get("open") is not True:
            raise SystemExit(f"search-history smoke: query input was not fully applied: {typed}")

        old_geo, old_screen = wait_for_search_result(live, fixtures[0])
        (out / "old.geometry.json").write_text(json.dumps(old_geo, indent=2) + "\n")
        (out / "old.screen.json").write_text(json.dumps(old_screen, indent=2) + "\n")
        states.append(capture_live_state(live, out, "old-match"))

        live.json_ctl("dispatch_ui_key", {"keys": ["enter"]})
        middle_geo, middle_screen = wait_for_search_result(live, fixtures[1])
        (out / "middle.geometry.json").write_text(json.dumps(middle_geo, indent=2) + "\n")
        (out / "middle.screen.json").write_text(json.dumps(middle_screen, indent=2) + "\n")
        states.append(capture_live_state(live, out, "middle-match"))

        live.json_ctl("dispatch_ui_key", {"keys": ["enter"]})
        new_geo, new_screen = wait_for_search_result(live, fixtures[2])
        (out / "new.geometry.json").write_text(json.dumps(new_geo, indent=2) + "\n")
        (out / "new.screen.json").write_text(json.dumps(new_screen, indent=2) + "\n")
        states.append(capture_live_state(live, out, "new-match"))

        live.json_ctl("dispatch_ui_key", {"keys": ["shift+enter"]})
        reverse_geo, reverse_screen = wait_for_search_result(live, fixtures[1])
        (out / "reverse.geometry.json").write_text(json.dumps(reverse_geo, indent=2) + "\n")
        (out / "reverse.screen.json").write_text(json.dumps(reverse_screen, indent=2) + "\n")
        states.append(capture_live_state(live, out, "reverse-middle-match"))

        offsets = [
            int(old_screen.get("display_offset", 0)),
            int(middle_screen.get("display_offset", 0)),
            int(new_screen.get("display_offset", 0)),
        ]
        if not (offsets[0] > offsets[1] > offsets[2] > 0):
            raise SystemExit(
                "search-history smoke: forward navigation did not move oldest-to-newest "
                f"through scrollback: offsets={offsets}"
            )
        reverse_offset = int(reverse_screen.get("display_offset", 0))
        if reverse_offset <= offsets[2]:
            raise SystemExit(
                "search-history smoke: Shift+Enter did not navigate back toward older history: "
                f"new={offsets[2]} reverse={reverse_offset}"
            )

        for label, geometry in [
            ("old", old_geo),
            ("middle", middle_geo),
            ("new", new_geo),
            ("reverse", reverse_geo),
        ]:
            search = geometry.get("search")
            if not isinstance(search, dict) or search.get("has_match") is not True:
                raise SystemExit(f"search-history smoke: {label} result lost focused-match state")
            if query in json.dumps(search):
                raise SystemExit("search-history smoke: ui_geometry exposed the private query")

        search_rect = reverse_geo.get("search", {}).get("rect")
        if not isinstance(search_rect, dict):
            raise SystemExit("search-history smoke: focused result omitted the search rectangle")
        open_pixels = read_rgba_png(live_state_screenshot_path(out, "search-open"))
        old_pixels = read_rgba_png(live_state_screenshot_path(out, "old-match"))
        chrome_changed = rgba_difference_count(
            open_pixels,
            old_pixels,
            rect=search_rect,
        )
        if chrome_changed < 100:
            raise SystemExit(
                "search-history smoke: typed query and result status did not visibly "
                "render inside the unchanged Search rectangle "
                f"({chrome_changed} changed pixels)"
            )

        match_rects = reverse_geo.get("search", {}).get("match_rects")
        if not isinstance(match_rects, list) or not match_rects:
            raise SystemExit("search-history smoke: focused result omitted match pixel rectangles")
        impossible_query = "KETTLE_SEARCH_NO_SUCH_MATCH_9F7C"
        select_all = "cmd+a" if platform.system() == "Darwin" else "ctrl+a"
        live.json_ctl(
            "dispatch_ui_key",
            {"keys": [select_all, *list(impossible_query)]},
        )
        no_match_geo, no_match_screen = wait_for_search_no_match(live)
        (out / "no-match.geometry.json").write_text(
            json.dumps(no_match_geo, indent=2) + "\n"
        )
        (out / "no-match.screen.json").write_text(
            json.dumps(no_match_screen, indent=2) + "\n"
        )
        if (
            int(no_match_screen.get("display_offset", -1)) != reverse_offset
            or int(no_match_screen.get("rows", -1)) != int(reverse_screen.get("rows", -2))
        ):
            raise SystemExit(
                "search-history smoke: no-match control changed the viewport/layout, so "
                "the focused-match pixel comparison would not be like-for-like: "
                f"match={reverse_offset}/{reverse_screen.get('rows')} "
                f"no-match={no_match_screen.get('display_offset')}/{no_match_screen.get('rows')}"
            )
        states.append(capture_live_state(live, out, "no-match"))

        reverse_pixels = read_rgba_png(
            live_state_screenshot_path(out, "reverse-middle-match")
        )
        no_match_pixels = read_rgba_png(live_state_screenshot_path(out, "no-match"))
        highlight_changed = 0
        highlight_area = 0
        for item in match_rects:
            rect = item.get("rect") if isinstance(item, dict) else None
            if not isinstance(rect, dict):
                raise SystemExit(f"search-history smoke: malformed match rectangle: {item}")
            highlight_changed += rgba_difference_count(
                reverse_pixels,
                no_match_pixels,
                rect=rect,
            )
            highlight_area += max(1, int(float(rect["width"]) * float(rect["height"])))
        highlight_threshold = max(100, highlight_area // 5)
        if highlight_changed < highlight_threshold:
            raise SystemExit(
                "search-history smoke: focused-match state changed in the control plane "
                "but its reported cell rectangles did not visibly change against an "
                "identical-layout no-match capture "
                f"({highlight_changed}/{highlight_threshold} changed pixels)"
            )

        live.json_ctl("dispatch_ui_key", {"keys": ["escape"]})
        closed = live.json_ctl("ui_geometry")
        (out / "search-closed.geometry.json").write_text(json.dumps(closed, indent=2) + "\n")
        if modal_open(closed, "search"):
            raise SystemExit("search-history smoke: Escape did not close Search")

    (out / "analysis.json").write_text(
        json.dumps(
            {
                "platform": platform.system(),
                "display": os.environ.get("DISPLAY"),
                "wayland_display": os.environ.get("WAYLAND_DISPLAY"),
                "keybind": binding,
                "query_bytes": len(query.encode("utf-8")),
                "fixture_lines": 1800,
                "history_size": int(bottom.get("history_size", 0)),
                "forward_offsets": offsets,
                "rendered_pixels": {
                    "search_chrome_changed": chrome_changed,
                    "match_highlight_changed": highlight_changed,
                    "match_highlight_threshold": highlight_threshold,
                    "match_rectangles": len(match_rects),
                },
                "reverse_offset": reverse_offset,
                "statuses": [
                    old_geo["search"]["status"],  # type: ignore[index]
                    middle_geo["search"]["status"],  # type: ignore[index]
                    new_geo["search"]["status"],  # type: ignore[index]
                    reverse_geo["search"]["status"],  # type: ignore[index]
                ],
                "states": states,
            },
            indent=2,
        )
        + "\n"
    )
    return out


def run_image_paste_receipt(kettle: str, root: Path) -> Path:
    """Drive bitmap clipboard paste through the live UI and capture its states."""
    out = root / f"image-paste-receipt-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "record = off",
                "paste-images = on",
                "paste-image-preview = on",
                "window-width = 92",
                "window-height = 28",
                "window-position-x = 80",
                "window-position-y = 80",
            ]
        )
        + "\n"
    )
    fixture = out / "clipboard-fixture.png"
    write_image_receipt_fixture(fixture)
    clipboard_owner = set_bitmap_clipboard(fixture)

    states: List[Dict[str, object]] = []
    shell_args = (
        ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
        if platform.system() == "Windows"
        else ["-e", "bash", "--noprofile", "--norc"]
    )
    live_owner = LiveKettle(kettle, cfg, out / "kettle.log", extra_args=shell_args)
    live: Optional[LiveKettle] = None
    try:
        live = live_owner.__enter__()
        live.wait_for_text(
            "PS " if platform.system() == "Windows" else "bash-", timeout_ms=12000
        )
        resized = live.json_ctl("resize_window", {"width": 900, "height": 600})
        applied = resized.get("applied", {})
        expected_surface = (int(applied.get("width", 900)), int(applied.get("height", 600)))
        resize_deadline = time.monotonic() + 8.0
        while time.monotonic() < resize_deadline:
            surface = live.json_ctl("ui_geometry").get("surface", {})
            if (surface.get("width"), surface.get("height")) == expected_surface:
                break
            time.sleep(0.05)
        else:
            raise SystemExit(
                "image-paste-receipt smoke: window did not settle at "
                f"{expected_surface[0]}x{expected_surface[1]}"
            )
        geometry = live.json_ctl("ui_geometry")
        if geometry.get("window_focused") is not True:
            focus_live_kettle_window(live)
            focus_deadline = time.monotonic() + 5.0
            while time.monotonic() < focus_deadline:
                if live.json_ctl("ui_geometry").get("window_focused") is True:
                    break
                time.sleep(0.05)
            else:
                live.screenshot(out / "focus-failed-window.png")
                raise SystemExit(
                    "image-paste-receipt smoke: compositor refused to focus the live window"
                )
        ready = "KETTLE_IMAGE_RECEIPT_READY"
        ready_command = (
            f"Write-Output {ready}"
            if platform.system() == "Windows"
            else f"printf '%s\\n' {ready}"
        )
        live_shell_command(live, ready_command, ready)
        dispatched = live.json_ctl("perform_action", {"action": "paste"})
        (out / "paste.dispatch.json").write_text(json.dumps(dispatched, indent=2) + "\n")

        expanded_geo, expanded = wait_for_image_receipt(live, expanded=True)
        if (expanded.get("original_width"), expanded.get("original_height")) != (640, 360):
            raise SystemExit(
                "image-paste-receipt smoke: receipt lost source dimensions: "
                f"{expanded!r}"
            )
        if any(private in json.dumps(expanded) for private in ("kettle-paste-", ".png")):
            raise SystemExit(
                "image-paste-receipt smoke: ui_geometry exposed the retained path"
            )

        screen = live.json_ctl("read_screen")
        if not contains_managed_paste_marker(screen):
            raise SystemExit(
                "image-paste-receipt smoke: pane did not receive the managed image path"
            )
        redacted_screen = redact_managed_paste_paths(screen)
        if contains_managed_paste_marker(redacted_screen):
            raise SystemExit(
                "image-paste-receipt smoke: managed path survived diagnostics redaction"
            )

        (out / "expanded.geometry.json").write_text(
            json.dumps(expanded_geo, indent=2) + "\n"
        )
        receipt_lane = expanded.get("rect")
        if not isinstance(receipt_lane, dict):
            raise SystemExit("image-paste-receipt smoke: receipt omitted its rectangle")
        wait_for_image_receipt(live, expanded=True)
        expanded_shot = out / "expanded.png"
        capture_receipt_lane(live, expanded_shot, receipt_lane)
        states.append({"label": "expanded", "screenshot": str(expanded_shot)})

        compact_geo, compact = wait_for_image_receipt(live, expanded=False)
        (out / "compact.geometry.json").write_text(json.dumps(compact_geo, indent=2) + "\n")
        compact_rect = compact.get("rect")
        if not isinstance(compact_rect, dict):
            raise SystemExit("image-paste-receipt smoke: compact receipt omitted its rectangle")
        compact_shot = out / "compact.png"
        capture_receipt_lane(live, compact_shot, compact_rect)
        states.append({"label": "compact", "screenshot": str(compact_shot)})

        live.ctl(
            "send_mouse",
            params={
                "event": "move",
                "x": float(compact_rect["x"]) + float(compact_rect["width"]) / 2.0,
                "y": float(compact_rect["y"]) + float(compact_rect["height"]) / 2.0,
            },
        )
        hovered_geo, hovered = wait_for_image_receipt(live, expanded=True)
        (out / "hovered.geometry.json").write_text(json.dumps(hovered_geo, indent=2) + "\n")
        hovered_rect = hovered.get("rect")
        if not isinstance(hovered_rect, dict):
            raise SystemExit("image-paste-receipt smoke: hovered receipt omitted its rectangle")
        hovered_shot = out / "hovered.png"
        capture_receipt_lane(live, hovered_shot, hovered_rect)
        states.append({"label": "hovered", "screenshot": str(hovered_shot)})

        expanded_pixels = read_rgba_png(expanded_shot)
        compact_pixels = read_rgba_png(compact_shot)
        hovered_pixels = read_rgba_png(hovered_shot)
        if rgba_card_difference_count(expanded_pixels, compact_pixels) < 500:
            raise SystemExit(
                "image-paste-receipt smoke: expanded and compact frames are visually unchanged"
            )
        if hovered.get("image_rect") is None:
            raise SystemExit("image-paste-receipt smoke: hover did not restore the thumbnail")
        if rgba_card_difference_count(hovered_pixels, compact_pixels) < 500:
            raise SystemExit(
                "image-paste-receipt smoke: hovered and compact frames are visually unchanged"
            )

        dismiss = hovered.get("dismiss_rect")
        if not isinstance(dismiss, dict):
            raise SystemExit("image-paste-receipt smoke: receipt omitted its dismiss target")
        live.ctl(
            "send_mouse",
            params={
                "event": "click",
                "button": 0,
                "x": float(dismiss["x"]) + float(dismiss["width"]) / 2.0,
                "y": float(dismiss["y"]) + float(dismiss["height"]) / 2.0,
            },
        )
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline:
            if live.json_ctl("ui_geometry").get("image_paste_receipt") is None:
                break
            time.sleep(0.05)
        else:
            raise SystemExit("image-paste-receipt smoke: dismiss button left the receipt visible")

        # A receipt describes the command line at paste time. A later edit must
        # dismiss it instead of leaving success chrome for a path the user
        # removed. Paste once more, then clear that line through the ctl key
        # path so automation and native input share the same contract.
        live.json_ctl("perform_action", {"action": "paste"})
        wait_for_image_receipt(live, expanded=True)
        live.ctl("send_keys", params={"keys": shell_clear_line_keys(platform.system())})
        clear_deadline = time.monotonic() + 3.0
        while time.monotonic() < clear_deadline:
            geometry = live.json_ctl("ui_geometry")
            screen = live.json_ctl("read_screen")
            if (
                geometry.get("image_paste_receipt") is None
                and not contains_managed_paste_marker(screen)
            ):
                break
            time.sleep(0.05)
        else:
            raise SystemExit(
                "image-paste-receipt smoke: later input left stale receipt or path text"
            )

        serialized_screen = json.dumps(redacted_screen, indent=2)
        (out / "screen.json").write_text(serialized_screen + "\n")
    finally:
        if live is not None:
            live_owner.__exit__(*sys.exc_info())
        if clipboard_owner is not None and clipboard_owner.poll() is None:
            clipboard_owner.terminate()
            try:
                clipboard_owner.wait(timeout=2)
            except subprocess.TimeoutExpired:
                clipboard_owner.kill()
                clipboard_owner.wait(timeout=2)

    (out / "analysis.json").write_text(
        json.dumps(
            {
                "platform": platform.platform(),
                "clipboard_replaced": True,
                "source_dimensions": [640, 360],
                "states": states,
            },
            indent=2,
        )
        + "\n"
    )
    return out


def run_video_paste_receipt(kettle: str, root: Path) -> Path:
    """Paste an explicit video file list and capture the native poster card."""
    out = root / f"video-paste-receipt-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    out.chmod(0o700)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "record = off",
                "paste-files = on",
                "paste-video-preview = on",
                "window-width = 92",
                "window-height = 28",
                "window-position-x = 80",
                "window-position-y = 80",
            ]
        )
        + "\n"
    )
    fixtures = [out / "first-video.mp4", out / "second-video.webm"]
    fixture_override = os.environ.get("KETTLE_SMOKE_VIDEO_FIXTURES")
    if fixture_override:
        sources = [Path(value) for value in fixture_override.split(os.pathsep) if value]
        if len(sources) != 2 or any(not source.is_file() for source in sources):
            raise SystemExit(
                "video-paste-receipt smoke: KETTLE_SMOKE_VIDEO_FIXTURES must "
                "name exactly two existing files"
            )
        for source, destination in zip(sources, fixtures):
            shutil.copy2(source, destination)
    else:
        require_cmd("ffmpeg")
        commands = [
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=24",
                "-t",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-y",
                str(fixtures[0]),
            ],
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x89b4fa:size=480x270:rate=24",
                "-t",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-y",
                str(fixtures[1]),
            ],
        ]
        for command in commands:
            generated = run(command)
            if generated.returncode != 0:
                raise SystemExit(
                    "video-paste-receipt smoke: ffmpeg fixture generation failed:\n"
                    f"{generated.stderr}\n{generated.stdout}"
                )
    for fixture in fixtures:
        fixture.chmod(0o600)
    launch_env: Dict[str, Optional[str]] = {}
    if platform.system() == "Linux":
        cache = out / "xdg-cache"
        write_linux_video_thumbnail_cache(fixtures[0], cache)
        launch_env["XDG_CACHE_HOME"] = str(cache)
    clipboard_owner = set_file_list_clipboard(fixtures)

    states: List[Dict[str, object]] = []
    shell_args = (
        ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
        if platform.system() == "Windows"
        else ["-e", "bash", "--noprofile", "--norc"]
    )
    live_owner = LiveKettle(
        kettle,
        cfg,
        out / "kettle.log",
        extra_args=shell_args,
        extra_env=launch_env,
    )
    live: Optional[LiveKettle] = None
    try:
        live = live_owner.__enter__()
        live.wait_for_text(
            "PS " if platform.system() == "Windows" else "bash-", timeout_ms=12000
        )
        live.json_ctl("resize_window", {"width": 900, "height": 600})
        if live.json_ctl("ui_geometry").get("window_focused") is not True:
            focus_live_kettle_window(live, desktop_point=(120.0, 120.0))
            focus_deadline = time.monotonic() + 5.0
            while time.monotonic() < focus_deadline:
                if live.json_ctl("ui_geometry").get("window_focused") is True:
                    break
                time.sleep(0.05)
            else:
                live.screenshot(out / "focus-failed-window.png")
                raise SystemExit(
                    "video-paste-receipt smoke: compositor refused to focus the live window"
                )
        ready = "KETTLE_VIDEO_RECEIPT_READY"
        ready_command = (
            f"Write-Output {ready}"
            if platform.system() == "Windows"
            else f"printf '%s\\n' {ready}"
        )
        live_shell_command(live, ready_command, ready)
        dispatched = live.json_ctl("perform_action", {"action": "paste"})
        (out / "paste.dispatch.json").write_text(json.dumps(dispatched, indent=2) + "\n")

        expanded_geo, expanded = wait_for_media_receipt(
            live, kind="video", expanded=True, preview_ready=True
        )
        if expanded.get("count") != 2:
            raise SystemExit(
                "video-paste-receipt smoke: one clipboard batch did not retain both videos: "
                f"{expanded!r}"
            )
        if expanded.get("openable") is not False:
            raise SystemExit(
                "video-paste-receipt smoke: video card exposed an unsafe path-based open action"
            )
        serialized = json.dumps(expanded)
        if any(str(path) in serialized for path in fixtures):
            raise SystemExit("video-paste-receipt smoke: ui_geometry exposed a source path")
        if expanded.get("original_width") is not None or expanded.get("original_height") is not None:
            raise SystemExit(
                "video-paste-receipt smoke: generic video diagnostics exposed image-only dimensions"
            )
        rect = expanded.get("rect")
        if not isinstance(rect, dict) or not isinstance(expanded.get("preview_rect"), dict):
            raise SystemExit("video-paste-receipt smoke: expanded card omitted its poster geometry")
        live.ctl(
            "send_mouse",
            params={
                "event": "move",
                "x": float(rect["x"]) + float(rect["width"]) / 2.0,
                "y": float(rect["y"]) + float(rect["height"]) / 2.0,
            },
        )
        screen = live.json_ctl("read_screen")
        if not any(path.name in json.dumps(screen) for path in fixtures):
            raise SystemExit("video-paste-receipt smoke: pane did not receive the copied file list")

        (out / "expanded.geometry.json").write_text(
            json.dumps(expanded_geo, indent=2) + "\n"
        )
        expanded_shot = out / "expanded.png"
        capture_receipt_lane(live, expanded_shot, rect)
        states.append({"label": "expanded-native-poster", "screenshot": str(expanded_shot)})

        live.ctl("send_mouse", params={"event": "move", "x": 8.0, "y": 8.0})
        compact_geo, compact = wait_for_media_receipt(
            live, kind="video", expanded=False, preview_ready=True
        )
        (out / "compact.geometry.json").write_text(json.dumps(compact_geo, indent=2) + "\n")
        compact_rect = compact.get("rect")
        if not isinstance(compact_rect, dict):
            raise SystemExit("video-paste-receipt smoke: compact card omitted its rectangle")
        compact_shot = out / "compact.png"
        capture_receipt_lane(live, compact_shot, compact_rect)
        states.append({"label": "compact", "screenshot": str(compact_shot)})

        live.ctl(
            "send_mouse",
            params={
                "event": "move",
                "x": float(compact_rect["x"]) + float(compact_rect["width"]) / 2.0,
                "y": float(compact_rect["y"]) + float(compact_rect["height"]) / 2.0,
            },
        )
        hovered_geo, hovered = wait_for_media_receipt(
            live, kind="video", expanded=True, preview_ready=True
        )
        (out / "hovered.geometry.json").write_text(json.dumps(hovered_geo, indent=2) + "\n")
        hovered_rect = hovered.get("rect")
        if not isinstance(hovered_rect, dict):
            raise SystemExit("video-paste-receipt smoke: hovered card omitted its rectangle")
        hovered_shot = out / "hovered.png"
        capture_receipt_lane(live, hovered_shot, hovered_rect)
        states.append({"label": "hover-expanded", "screenshot": str(hovered_shot)})

        if rgba_card_difference_count(
            read_rgba_png(expanded_shot), read_rgba_png(compact_shot)
        ) < 500:
            raise SystemExit("video-paste-receipt smoke: expanded and compact frames are unchanged")
        dismiss = hovered.get("dismiss_rect")
        if not isinstance(dismiss, dict):
            raise SystemExit("video-paste-receipt smoke: card omitted its dismiss target")
        live.ctl(
            "send_mouse",
            params={
                "event": "click",
                "button": 0,
                "x": float(dismiss["x"]) + float(dismiss["width"]) / 2.0,
                "y": float(dismiss["y"]) + float(dismiss["height"]) / 2.0,
            },
        )
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline:
            if live.json_ctl("ui_geometry").get("media_paste_receipt") is None:
                break
            time.sleep(0.05)
        else:
            raise SystemExit("video-paste-receipt smoke: dismiss left the card visible")

        # Re-paste and edit the command line only after the visual states and
        # dismiss target have been checked. Input must then clear both the
        # receipt and the file-list text instead of leaving stale success UI.
        live.json_ctl("perform_action", {"action": "paste"})
        wait_for_media_receipt(live, kind="video", expanded=True, preview_ready=True)
        live.ctl("send_keys", params={"keys": shell_clear_line_keys(platform.system())})
        clear_deadline = time.monotonic() + 3.0
        while time.monotonic() < clear_deadline:
            geometry = live.json_ctl("ui_geometry")
            screen = live.json_ctl("read_screen")
            if (
                geometry.get("media_paste_receipt") is None
                and not any(path.name in json.dumps(screen) for path in fixtures)
            ):
                break
            time.sleep(0.05)
        else:
            raise SystemExit(
                "video-paste-receipt smoke: later input left stale receipt or path text"
            )
    finally:
        if live is not None:
            live_owner.__exit__(*sys.exc_info())
        if clipboard_owner is not None and clipboard_owner.poll() is None:
            clipboard_owner.terminate()
            try:
                clipboard_owner.wait(timeout=2)
            except subprocess.TimeoutExpired:
                clipboard_owner.kill()
                clipboard_owner.wait(timeout=2)

    (out / "analysis.json").write_text(
        json.dumps(
            {
                "platform": platform.platform(),
                "clipboard_file_count": len(fixtures),
                "native_poster": True,
                "states": states,
            },
            indent=2,
        )
        + "\n"
    )
    return out


def exercise_hovered_pane_wheel(live: LiveKettle, out: Path) -> Dict[str, int]:
    """Prove terminal wheel routing follows hover without moving focus."""
    split_geo = live.json_ctl("ui_geometry")
    split_bars = sorted(
        split_geo.get("pane_titlebars", []),  # type: ignore[union-attr]
        key=lambda bar: float(bar["pane_rect"]["x"]),
    )
    if len(split_bars) != 2:
        raise SystemExit(
            f"hover-wheel smoke: expected two active split titlebars, got {split_bars}"
        )
    left_bar, right_bar = split_bars
    left_id = int(left_bar["pane"])
    right_id = int(right_bar["pane"])

    for pane_id, prefix in [(left_id, "LEFT"), (right_id, "RIGHT")]:
        done = f"KETTLE_HOVER_WHEEL_{prefix}_DONE"
        if platform.system() == "Windows":
            fill_body = (
                "$esc=[char]27; [Console]::Write($esc + '[2J' + $esc + '[3J' + $esc + '[H'); "
                f"1..140 | ForEach-Object {{ 'KETTLE_HOVER_WHEEL_{prefix}_{{0:D3}}' -f $_ }}"
            )
        else:
            fill_body = (
                "printf '\\033[2J\\033[3J\\033[H'; "
                f"for i in $(seq 1 140); do printf 'KETTLE_HOVER_WHEEL_{prefix}_%03d\\n' \"$i\"; done"
            )
        fill = command_with_marker(fill_body, done)
        live.ctl("send_text", params={"pane": pane_id, "text": fill})
        live.ctl("send_keys", params={"pane": pane_id, "keys": ["enter"]})
        waited = json.loads(
            live.ctl(
                "wait_for",
                params={
                    "pane": pane_id,
                    "text": done,
                    "timeout_ms": 12000,
                    "quiet_ms": 200,
                },
                raw=True,
                timeout=17.0,
            ).stdout
        )
        if not waited.get("matched"):
            raise SystemExit(
                f"hover-wheel smoke: pane {pane_id} did not build scrollback: {waited}"
            )

    def pane_center(bar: Dict[str, object]) -> Tuple[float, float]:
        rect = bar["pane_rect"]  # type: ignore[index]
        return rect_center(rect)  # type: ignore[arg-type]

    left_x, left_y = pane_center(left_bar)
    right_x, right_y = pane_center(right_bar)
    live.ctl(
        "send_mouse",
        params={"event": "click", "x": left_x, "y": left_y, "button": "left"},
    )
    time.sleep(0.15)
    focused = live.json_ctl("list_panes")
    focused_ids = [
        int(pane["id"])
        for pane in focused.get("panes", [])
        if pane.get("focused")
    ]
    if focused_ids != [left_id]:
        raise SystemExit(
            f"hover-wheel smoke: could not focus left split {left_id}: {focused_ids}"
        )

    live.ctl("send_mouse", params={"event": "move", "x": right_x, "y": right_y})
    live.ctl("send_mouse", params={"event": "wheel", "wheel_lines": 24})
    time.sleep(0.2)
    left_scrolled = live.json_ctl("read_screen", {"pane": left_id})
    right_scrolled = live.json_ctl("read_screen", {"pane": right_id})
    focused_after_wheel = live.json_ctl("list_panes")
    (out / "hover-wheel-left.screen.json").write_text(
        json.dumps(left_scrolled, indent=2) + "\n"
    )
    (out / "hover-wheel-right.screen.json").write_text(
        json.dumps(right_scrolled, indent=2) + "\n"
    )
    (out / "hover-wheel-focus.json").write_text(
        json.dumps(focused_after_wheel, indent=2) + "\n"
    )
    focused_ids = [
        int(pane["id"])
        for pane in focused_after_wheel.get("panes", [])
        if pane.get("focused")
    ]
    if int(left_scrolled.get("display_offset", 0)) != 0:
        raise SystemExit("hover-wheel smoke: hovered right wheel scrolled the focused left pane")
    right_offset = int(right_scrolled.get("display_offset", 0))
    if right_offset <= 0:
        raise SystemExit("hover-wheel smoke: hovered right wheel did not scroll the right pane")
    if focused_ids != [left_id]:
        raise SystemExit(
            "hover-wheel smoke: wheel changed keyboard focus "
            f"from {left_id} to {focused_ids}"
        )
    live.ctl("send_mouse", params={"event": "wheel", "wheel_lines": -240})
    time.sleep(0.15)
    if int(live.json_ctl("read_screen", {"pane": right_id}).get("display_offset", 0)) != 0:
        raise SystemExit("hover-wheel smoke: hovered right wheel did not return to live bottom")
    return {"left_pane": left_id, "right_pane": right_id, "right_offset": right_offset}


def run_hover_wheel(kettle: str, root: Path) -> Path:
    """Focused live scenario for adapters that cannot copy a swapchain image."""
    out = root / f"hover-wheel-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "window-width = 110",
                "window-height = 34",
            ]
        )
        + "\n"
    )
    extra_args = (
        ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
        if platform.system() == "Windows"
        else []
    )
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        marker = "KETTLE_HOVER_WHEEL_BASELINE"
        command = (
            "Write-Output hover-wheel-baseline"
            if platform.system() == "Windows"
            else "printf 'hover-wheel-baseline\\n'"
        )
        live_shell_command(live, command_with_marker(command, marker), marker)
        live.json_ctl("perform_action", {"action": "split_right"})
        for _ in range(50):
            geometry = live.json_ctl("ui_geometry")
            if len(geometry.get("pane_titlebars", [])) == 2:  # type: ignore[arg-type]
                break
            time.sleep(0.1)
        else:
            raise SystemExit("hover-wheel smoke: split did not produce two panes")
        analysis = exercise_hovered_pane_wheel(live, out)
        (out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    return out


def run_interaction(kettle: str, root: Path) -> Path:
    out = root / f"interaction-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #090909",
                "foreground = #f5f5f5",
                "minimum-contrast = 0",
                "window-padding-x = 8",
                "window-padding-y = 8",
                "window-width = 110",
                "window-height = 34",
            ]
        )
        + "\n"
    )
    extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"] if platform.system() == "Windows" else []
    states: List[Dict[str, object]] = []
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        marker = "KETTLE_INTERACTION_BASELINE"
        live_shell_command(live, command_with_marker("printf 'interaction-baseline\\n'" if platform.system() != "Windows" else "Write-Output interaction-baseline", marker), marker)
        states.append(capture_live_state(live, out, "baseline"))

        paste_marker = "KETTLE_INTERACTION_PASTE_DONE"
        if platform.system() == "Windows":
            paste_body = "Write-Output PASTE_LINE_ONE; Write-Output PASTE_LINE_TWO"
        else:
            paste_body = "printf '%s\\n' PASTE_LINE_ONE PASTE_LINE_TWO"
        paste_text = command_with_marker(paste_body, paste_marker)
        live.ctl("send_text", params={"text": paste_text})
        live.ctl("send_keys", params={"keys": ["enter"]})
        live.wait_for_text(paste_marker, timeout_ms=10000, quiet_ms=250)
        paste_screen = live.json_ctl("read_screen")
        if "PASTE_LINE_ONE" not in screen_text(paste_screen) or "PASTE_LINE_TWO" not in screen_text(paste_screen):
            raise SystemExit("interaction smoke: multiline paste/send_text marker was not visible")
        states.append(capture_live_state(live, out, "paste"))

        scroll_marker = "KETTLE_INTERACTION_SCROLL_100"
        scroll_done = "KETTLE_INTERACTION_SCROLL_DONE"
        if platform.system() == "Windows":
            scroll_body = (
                "$esc=[char]27; [Console]::Write($esc + '[2J' + $esc + '[3J' + $esc + '[H'); "
                "1..140 | ForEach-Object { 'KETTLE_INTERACTION_SCROLL_{0:D3}' -f $_ }"
            )
        else:
            scroll_body = "printf '\\033[2J\\033[3J\\033[H'; for i in $(seq 1 140); do printf 'KETTLE_INTERACTION_SCROLL_%03d\\n' \"$i\"; done"
        scroll_cmd = command_with_marker(scroll_body, scroll_done)
        live_shell_command(live, scroll_cmd, scroll_done, timeout_ms=12000)
        live.screenshot(out / "scroll-bottom.png")
        bottom = live.json_ctl("read_screen")
        (out / "scroll-bottom.screen.json").write_text(json.dumps(bottom, indent=2) + "\n")
        if int(bottom.get("display_offset", 0)) != 0:
            raise SystemExit(f"interaction smoke: expected bottom display_offset 0, got {bottom.get('display_offset')}")

        # Reproduce the complete Terminator-style select-all workflow against
        # real scrollback and assert the exact range selected.
        home = live.json_ctl("dispatch_keybind", {"logical": "home", "mods": "shift"})
        if home.get("action") != "SelectToTop":
            raise SystemExit(f"interaction smoke: Shift+Home did not dispatch select_to_top: {home}")
        time.sleep(0.15)
        (out / "selection-after-home.screen.json").write_text(
            json.dumps(live.json_ctl("read_screen"), indent=2) + "\n"
        )
        first_x, first_y = wait_for_text_cell_point(
            live,
            "KETTLE_INTERACTION_SCROLL_001",
        )
        live.ctl(
            "send_mouse",
            params={"event": "click", "x": first_x, "y": first_y, "button": "left"},
        )
        (out / "selection-after-first-click.screen.json").write_text(
            json.dumps(live.json_ctl("read_screen"), indent=2) + "\n"
        )

        end = live.json_ctl("dispatch_keybind", {"logical": "end", "mods": "shift"})
        if end.get("action") != "SelectToBottom":
            raise SystemExit(f"interaction smoke: Shift+End did not dispatch select_to_bottom: {end}")
        time.sleep(0.15)
        (out / "selection-after-end.screen.json").write_text(
            json.dumps(live.json_ctl("read_screen"), indent=2) + "\n"
        )
        last_marker = scroll_done
        last_x, last_y = wait_for_text_cell_point(
            live,
            last_marker,
            at_end=True,
        )
        live.ctl(
            "send_mouse",
            params={
                "event": "click",
                "x": last_x,
                "y": last_y,
                "button": "left",
                "mods": "shift",
            },
        )
        selected = live.json_ctl("read_screen", {"include_selection": True})
        (out / "selection-shift-workflow.screen.json").write_text(
            json.dumps(selected, indent=2) + "\n"
        )
        selection_text = str(selected.get("selection", "")).replace("\r\n", "\n").rstrip("\n")
        expected_selection = "\n".join(
            [f"KETTLE_INTERACTION_SCROLL_{index:03d}" for index in range(1, 141)]
            + [last_marker]
        )
        if selection_text != expected_selection:
            raise SystemExit(
                "interaction smoke: Shift+Home/End/Shift+click selected the wrong range\n"
                f"expected={expected_selection!r}\nactual={selection_text!r}"
            )
        live.json_ctl("perform_action", {"action": "copy"})
        live.screenshot(out / "selection-shift-workflow.png")

        live.ctl("send_mouse", params={"event": "wheel", "wheel_lines": 24})
        time.sleep(0.15)
        scrolled = live.json_ctl("read_screen")
        (out / "scroll-up.screen.json").write_text(json.dumps(scrolled, indent=2) + "\n")
        live.screenshot(out / "scroll-up.png")
        if int(scrolled.get("display_offset", 0)) <= 0:
            raise SystemExit("interaction smoke: mouse wheel did not move into scrollback")
        if scroll_marker not in screen_text(scrolled):
            raise SystemExit("interaction smoke: scrolled view did not reveal early scrollback marker")
        live.ctl("send_mouse", params={"event": "wheel", "wheel_lines": -240})
        time.sleep(0.15)
        returned = live.json_ctl("read_screen")
        (out / "scroll-return.screen.json").write_text(json.dumps(returned, indent=2) + "\n")
        if int(returned.get("display_offset", 0)) != 0:
            raise SystemExit("interaction smoke: wheel down did not return to live bottom")
        states.append(capture_live_state(live, out, "scroll-return"))

        geo = live.json_ctl("ui_geometry")
        content = geo["content"]  # type: ignore[index]
        selection_cells = live.read_cells()
        (out / "selection-target.cells.json").write_text(json.dumps(selection_cells, indent=2) + "\n")
        sx0, sy0, sx1, sy1 = selection_drag_points(selection_cells, content)  # type: ignore[arg-type]
        live.screenshot(out / "selection-before.png")
        live.ctl("send_mouse", params={"event": "press", "x": sx0, "y": sy0, "button": "left"})
        time.sleep(0.05)
        live.ctl("send_mouse", params={"event": "move", "x": sx1, "y": sy1})
        time.sleep(0.15)
        live.screenshot(out / "selection-drag.png")
        live.ctl("send_mouse", params={"event": "release", "x": sx1, "y": sy1, "button": "left"})
        selection_changes = len(changed_pixels(out / "selection-before.png", out / "selection-drag.png", float(content["y"]), float(content["y"]) + float(content["height"])))  # type: ignore[index]
        if selection_changes < 50:
            raise SystemExit(f"interaction smoke: selection drag changed too few pixels ({selection_changes})")

        tabs_before = live.json_ctl("list_tabs")
        geo = live.json_ctl("ui_geometry")
        nx, ny = rect_center(geo["tab_bar"]["new_tab"])  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "click", "x": nx, "y": ny, "button": "left"})
        time.sleep(0.4)
        tabs_after = live.json_ctl("list_tabs")
        (out / "tabs-before.json").write_text(json.dumps(tabs_before, indent=2) + "\n")
        (out / "tabs-after.json").write_text(json.dumps(tabs_after, indent=2) + "\n")
        if len(tabs_after.get("tabs", [])) <= len(tabs_before.get("tabs", [])):
            raise SystemExit("interaction smoke: tab-bar + button did not create a tab")
        tab_marker = "KETTLE_INTERACTION_NEW_TAB"
        live_shell_command(live, command_with_marker("printf 'new-tab-live\\n'" if platform.system() != "Windows" else "Write-Output new-tab-live", tab_marker), tab_marker)
        states.append(capture_live_state(live, out, "new-tab"))

        geo = live.json_ctl("ui_geometry")
        content = geo["content"]  # type: ignore[index]
        mx = float(content["x"]) + min(80.0, float(content["width"]) / 2.0)
        my = float(content["y"]) + min(80.0, float(content["height"]) / 2.0)
        live.screenshot(out / "menu-before.png")
        live.ctl("send_mouse", params={"event": "click", "x": mx, "y": my, "button": "right"})
        time.sleep(0.2)
        menu_geo = live.json_ctl("ui_geometry")
        (out / "menu-geometry.json").write_text(json.dumps(menu_geo, indent=2) + "\n")
        live.screenshot(out / "menu-open.png")
        menu_changes = len(changed_pixels(out / "menu-before.png", out / "menu-open.png", 0.0, float(geo["surface"]["height"])))  # type: ignore[index]
        if menu_changes < 100:
            raise SystemExit(f"interaction smoke: right-click menu produced too few changed pixels ({menu_changes})")
        menu = menu_geo.get("context_menu")
        if not menu:
            raise SystemExit("interaction smoke: right-click did not expose context_menu geometry")
        settings_row = visible_context_row(menu_geo, "Settings…")
        settings_x, settings_y = rect_center(settings_row["rect"])  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "click", "x": settings_x, "y": settings_y, "button": "left"})
        time.sleep(0.3)
        settings_geo = live.json_ctl("ui_geometry")
        (out / "settings-open.geometry.json").write_text(json.dumps(settings_geo, indent=2) + "\n")
        live.screenshot(out / "settings-open.png")
        if not modal_open(settings_geo, "settings"):
            raise SystemExit("interaction smoke: Settings row did not open the settings modal")
        settings_changes = len(changed_pixels(out / "menu-before.png", out / "settings-open.png", 0.0, float(geo["surface"]["height"])))  # type: ignore[index]
        if settings_changes < 500:
            raise SystemExit(f"interaction smoke: settings overlay changed too few pixels ({settings_changes})")
        settings_surface = settings_geo["surface"]  # type: ignore[index]
        close_x = float(settings_surface["width"]) - 2.0  # type: ignore[index]
        close_y = float(settings_surface["height"]) - 2.0  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "move", "x": close_x, "y": close_y})
        live.ctl("send_mouse", params={"event": "press", "x": close_x, "y": close_y, "button": "left"})
        live.ctl("send_mouse", params={"event": "release", "x": close_x, "y": close_y, "button": "left"})
        time.sleep(0.2)
        settings_closed_geo = live.json_ctl("ui_geometry")
        (out / "settings-closed.geometry.json").write_text(json.dumps(settings_closed_geo, indent=2) + "\n")
        if modal_open(settings_closed_geo, "settings"):
            raise SystemExit("interaction smoke: click outside settings did not close the modal")

        live.ctl("send_mouse", params={"event": "click", "x": mx, "y": my, "button": "right"})
        time.sleep(0.2)
        menu_geo = live.json_ctl("ui_geometry")
        (out / "menu-reopened.geometry.json").write_text(json.dumps(menu_geo, indent=2) + "\n")
        split_rows = [visible_context_row(menu_geo, "Split Right")]
        panes_before_split = live.json_ctl("list_panes")
        (out / "panes-before-split.json").write_text(json.dumps(panes_before_split, indent=2) + "\n")
        split_x, split_y = rect_center(split_rows[0]["rect"])  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "click", "x": split_x, "y": split_y, "button": "left"})
        time.sleep(0.8)
        panes_after_split = live.json_ctl("list_panes")
        (out / "panes-after-split.json").write_text(json.dumps(panes_after_split, indent=2) + "\n")
        if len(panes_after_split.get("panes", [])) <= len(panes_before_split.get("panes", [])):
            raise SystemExit("interaction smoke: Split Right context-menu row did not create a pane")
        split_marker = "KETTLE_INTERACTION_SPLIT_RIGHT"
        live_shell_command(live, command_with_marker("printf 'split-right-live\\n'" if platform.system() != "Windows" else "Write-Output split-right-live", split_marker), split_marker)
        states.append(capture_live_state(live, out, "split-right"))

        # Keep the same focused scenario embedded in the broad interaction
        # walk, while also exposing it alone for virtual surfaces that cannot
        # be copied into the screenshot pipeline.
        exercise_hovered_pane_wheel(live, out)

        before_resize_geo = live.json_ctl("ui_geometry")
        before_resize_cells = live.read_cells()
        (out / "resize-before.geometry.json").write_text(json.dumps(before_resize_geo, indent=2) + "\n")
        (out / "resize-before.cells.json").write_text(json.dumps(before_resize_cells, indent=2) + "\n")
        surface = before_resize_geo["surface"]  # type: ignore[index]
        before_w = int(surface["width"])  # type: ignore[index]
        before_h = int(surface["height"])  # type: ignore[index]
        before_cols = int(before_resize_cells.get("cols", 0))
        before_rows = int(before_resize_cells.get("rows", 0))
        target_w = before_w + 120
        target_h = before_h + 72
        live.ctl("resize_window", params={"width": target_w, "height": target_h})
        resized_geo, resized_cells = wait_for_resize(live, before_w, before_h, before_cols, before_rows)
        (out / "resize-after.geometry.json").write_text(json.dumps(resized_geo, indent=2) + "\n")
        (out / "resize-after.cells.json").write_text(json.dumps(resized_cells, indent=2) + "\n")
        states.append(capture_live_state(live, out, "resize-after"))

        notify_title = "KETTLE_NOTIFY_TITLE"
        notify_body = "KETTLE_NOTIFY_BODY"
        notify_marker = "KETTLE_INTERACTION_NOTIFY_DONE"
        events = EventStream(live, out / "notification-events.jsonl")
        try:
            time.sleep(0.3)
            live_shell_command(
                live,
                notification_command(notify_title, notify_body, notify_marker),
                notify_marker,
                timeout_ms=10000,
            )
            notification_event = events.wait_for(
                "protocol_notification",
                {"title": notify_title, "body": notify_body},
                timeout_s=8.0,
            )
        finally:
            events.close()
        (out / "notification-event.json").write_text(json.dumps(notification_event, indent=2) + "\n")
        states.append(capture_live_state(live, out, "notification"))

        hint_marker = "KETTLE_INTERACTION_HINT_URL_READY"
        hint_url = "https://example.com/kettle-live-smoke"
        if platform.system() == "Windows":
            hint_cmd = f"Write-Output {shell_quote(hint_url)}"
        else:
            hint_cmd = f"printf '%s\\n' {shell_quote(hint_url)}"
        live_shell_command(live, command_with_marker(hint_cmd, hint_marker), hint_marker)
        hint_ready = live.json_ctl("read_screen")
        (out / "hint-target.screen.json").write_text(json.dumps(hint_ready, indent=2) + "\n")
        if hint_url not in screen_text(hint_ready):
            raise SystemExit("interaction smoke: hint URL target is not visible before hint mode")

        palette_before = live.json_ctl("ui_geometry")
        menu_x, menu_y = rect_center(palette_before["tab_bar"]["new_tab_menu"])  # type: ignore[index]
        live.screenshot(out / "palette-before.png")
        live.ctl("send_mouse", params={"event": "click", "x": menu_x, "y": menu_y, "button": "left"})
        time.sleep(0.2)
        new_tab_menu_geo = live.json_ctl("ui_geometry")
        (out / "new-tab-menu.geometry.json").write_text(json.dumps(new_tab_menu_geo, indent=2) + "\n")
        live.screenshot(out / "new-tab-menu.png")
        palette_row = visible_context_row(new_tab_menu_geo, "Command palette")
        palette_x, palette_y = rect_center(palette_row["rect"])  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "click", "x": palette_x, "y": palette_y, "button": "left"})
        time.sleep(0.3)
        palette_geo = live.json_ctl("ui_geometry")
        (out / "palette-open.geometry.json").write_text(json.dumps(palette_geo, indent=2) + "\n")
        live.screenshot(out / "palette-open.png")
        if not modal_open(palette_geo, "palette"):
            raise SystemExit("interaction smoke: Command palette row did not open the palette modal")
        palette_changes = len(changed_pixels(out / "palette-before.png", out / "palette-open.png", 0.0, float(palette_before["surface"]["height"])))  # type: ignore[index]
        if palette_changes < 250:
            raise SystemExit(f"interaction smoke: command palette changed too few pixels ({palette_changes})")
        states.append(capture_live_state(live, out, "command-palette"))

        live.ctl("perform_action", params={"action": "start_search"})
        time.sleep(0.3)
        search_geo = live.json_ctl("ui_geometry")
        (out / "search-open.geometry.json").write_text(json.dumps(search_geo, indent=2) + "\n")
        search_transition_shot = live_transition_screenshot_path(out, "search-open")
        live.screenshot(search_transition_shot)
        if not modal_open(search_geo, "search"):
            raise SystemExit("interaction smoke: perform_action start_search did not open search")
        if modal_open(search_geo, "palette"):
            raise SystemExit("interaction smoke: search action did not close the command palette")
        search_changes = len(changed_pixels(out / "palette-open.png", search_transition_shot, 0.0, float(palette_geo["surface"]["height"])))  # type: ignore[index]
        if search_changes < 250:
            raise SystemExit(f"interaction smoke: search overlay changed too few pixels ({search_changes})")
        states.append(capture_live_state(live, out, "search-open"))

        modal_sequence = [
            ("ssh", "ssh_launcher", "ssh-launcher"),
            ("open_layout_picker", "layout_picker", "layout-picker"),
            ("hint_mode", "hint_mode", "hint-mode"),
            ("edit_window_title", "title_edit", "title-edit-window"),
            ("edit_tab_title", "title_edit", "title-edit-tab"),
            ("edit_pane_title", "title_edit", "title-edit-pane"),
        ]
        modal_flags: Dict[str, object] = {}
        previous_shot = out / "search-open.png"
        previous_geo = search_geo
        for action_name, modal_name, label in modal_sequence:
            live.ctl("perform_action", params={"action": action_name})
            time.sleep(0.3)
            modal_geo = live.json_ctl("ui_geometry")
            (out / f"{label}.geometry.json").write_text(json.dumps(modal_geo, indent=2) + "\n")
            modal_shot = live_transition_screenshot_path(out, label)
            live.screenshot(modal_shot)
            if not modal_open(modal_geo, modal_name):
                raise SystemExit(f"interaction smoke: perform_action {action_name} did not open {modal_name}")
            if modal_name == "title_edit":
                title_edit = modal_geo.get("title_edit")
                content = modal_geo.get("content")
                if not isinstance(title_edit, dict) or not isinstance(title_edit.get("rect"), dict):
                    raise SystemExit(f"interaction smoke: {label} has no title_edit rect")
                if not isinstance(content, dict):
                    raise SystemExit(f"interaction smoke: {label} has no content rect")
                if rect_intersects(title_edit["rect"], content):  # type: ignore[index]
                    raise SystemExit(
                        f"interaction smoke: {label} title edit overlaps terminal content: "
                        f"title={title_edit['rect']} content={content}"
                    )
                tab_bar = modal_geo.get("tab_bar")
                if not isinstance(tab_bar, dict):
                    raise SystemExit(f"interaction smoke: {label} has no tab_bar geometry")
                if tab_bar.get("segments"):
                    raise SystemExit(
                        f"interaction smoke: {label} title edit overlaps tab text: "
                        f"segments={tab_bar.get('segments')}"
                    )
                for button_name in ("new_tab", "new_tab_menu", "scroll_left", "scroll_right"):
                    button = tab_bar.get(button_name)
                    if isinstance(button, dict) and float(button.get("width", 0.0)) > 0.0:
                        raise SystemExit(
                            f"interaction smoke: {label} title edit exposes {button_name}: {button}"
                        )
            changed = len(changed_pixels(previous_shot, modal_shot, 0.0, float(previous_geo["surface"]["height"])))  # type: ignore[index]
            if changed < 100:
                raise SystemExit(f"interaction smoke: {label} changed too few pixels ({changed})")
            modal_flags[label] = {
                "action": action_name,
                "changed_pixels": changed,
                "modals": modal_geo.get("modals"),
            }
            states.append(capture_live_state(live, out, label))
            previous_shot = modal_shot
            previous_geo = modal_geo

    (out / "analysis.json").write_text(
        json.dumps(
            {
                "states": states,
                "menu_changed_pixels": menu_changes,
                "settings_changed_pixels": settings_changes,
                "palette_changed_pixels": palette_changes,
                "search_changed_pixels": search_changes,
                "selection_changed_pixels": selection_changes,
                "scroll_offset": int(scrolled.get("display_offset", 0)),
                "tabs_before": len(tabs_before.get("tabs", [])),
                "tabs_after": len(tabs_after.get("tabs", [])),
                "panes_before_split": len(panes_before_split.get("panes", [])),
                "panes_after_split": len(panes_after_split.get("panes", [])),
                "resize_before_surface": before_resize_geo["surface"],
                "resize_after_surface": resized_geo["surface"],
                "resize_before_cells": {
                    "cols": before_resize_cells.get("cols"),
                    "rows": before_resize_cells.get("rows"),
                },
                "resize_requested_surface": {
                    "width": target_w,
                    "height": target_h,
                },
                "resize_after_cells": {
                    "cols": resized_cells.get("cols"),
                    "rows": resized_cells.get("rows"),
                },
                "resize_overlay": resized_geo.get("resize_overlay"),
                "settings_modal_after_open": settings_geo.get("modals"),
                "settings_modal_after_close": settings_closed_geo.get("modals"),
                "palette_modal_after_open": palette_geo.get("modals"),
                "search_modal_after_open": search_geo.get("modals"),
                "extra_modal_states": modal_flags,
                "palette_row_rect": palette_row["rect"],
                "menu_split_right_rect": split_rows[0]["rect"],
                "notification_event": notification_event,
            },
            indent=2,
        )
        + "\n"
    )
    return out


def run_selection_autoscroll(kettle: str, root: Path) -> Path:
    out = root / f"selection-autoscroll-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "text-renderer = grid",
                "tab-bar = always",
                "tab-bar-position = bottom",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "window-width = 90",
                "window-height = 28",
                "window-position-x = 160",
                "window-position-y = 160",
            ]
        )
        + "\n"
    )
    extra_args = (
        ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
        if platform.system() == "Windows"
        else ["-e", "bash", "--noprofile", "--norc"]
    )
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live.wait_for_text(
            "PS " if platform.system() == "Windows" else "bash-", timeout_ms=12000
        )
        # A visible startup prompt can race the first injected command on a
        # freshly mapped window. Cancel once to establish a fresh prompt before
        # installing the scrollback fixture.
        live.ctl("send_keys", params={"keys": ["ctrl+c"]})
        time.sleep(0.1)
        marker = "KETTLE_SELECTION_AUTOSCROLL_DONE"
        if platform.system() == "Windows":
            body = "1..30 | ForEach-Object { 'KETTLE_SELECTION_AUTOSCROLL_{0:D3}' -f $_ }"
        else:
            body = (
                "for i in $(seq 1 30); do "
                "printf 'KETTLE_SELECTION_AUTOSCROLL_%03d\\n' \"$i\"; done"
            )
        live_shell_command(live, command_with_marker(body, marker), marker, timeout_ms=12000)
        prompt_prefix = "PS " if platform.system() == "Windows" else "bash-"
        prompt_deadline = time.monotonic() + 3.0
        before: Dict[str, object] = {}
        while time.monotonic() < prompt_deadline:
            before = live.json_ctl("read_screen")
            lines = str(before.get("text", "")).rstrip().splitlines()
            if (
                int(before.get("display_offset", -1)) == 0
                and lines
                and lines[-1].startswith(prompt_prefix)
            ):
                break
            time.sleep(0.05)
        else:
            raise SystemExit(
                "selection-autoscroll smoke: fixture did not settle at a fresh live prompt: "
                f"{before}"
            )

        geometry = live.json_ctl("ui_geometry")
        content = geometry["content"]
        if float(content["y"]) != 0.0:
            raise SystemExit(
                "selection-autoscroll smoke: content must begin at the client "
                f"top so the inert probe crosses the client boundary: {content}"
            )
        cells = live.read_cells()
        start_x, start_y, end_x, _ = selection_drag_points(cells, content)
        cell_width = float(content["width"]) / max(1, int(cells.get("cols", 1)))
        pointer_x = min(start_x + cell_width * 2.0, end_x)
        surface = geometry["surface"]
        native_window_frame: Optional[List[float]] = None
        focus_point: Optional[Tuple[float, float]] = None
        if platform.system() == "Darwin":
            window_x, window_y, window_width, window_height = macos_window_frame(live.pid)
            native_window_frame = [window_x, window_y, window_width, window_height]
            scale_x = float(surface["width"]) / window_width
            scale_y = scale_x
            client_origin_y = (
                window_y + window_height - float(surface["height"]) / scale_y
            )
            pointer_x = window_x + pointer_x / scale_x
            pointer_y = client_origin_y + start_y / scale_y
            focus_point = (window_x + window_width / 2.0, window_y + 12.0)

            def emit_mouse(event_type: int, x: float, y: float) -> None:
                macos_mouse_event(event_type, x, y)

            pointer_driver = "macos-native"
        else:
            scale_x = 1.0
            scale_y = 1.0
            client_origin_y = 0.0
            pointer_y = start_y

            def emit_mouse(event_type: int, x: float, y: float) -> None:
                if event_type == MACOS_LEFT_MOUSE_DOWN:
                    live.ctl(
                        "send_mouse",
                        params={"event": "press", "x": x, "y": y, "button": "left"},
                    )
                elif event_type == MACOS_LEFT_MOUSE_UP:
                    live.ctl(
                        "send_mouse",
                        params={"event": "release", "x": x, "y": y, "button": "left"},
                    )
                else:
                    live.ctl("send_mouse", params={"event": "move", "x": x, "y": y})

            pointer_driver = "ctl-portable"
        cell_height = (
            float(content["height"]) / max(1, int(cells.get("rows", 1))) / scale_y
        )
        mouse_pressed = False
        last_pointer = (pointer_x, pointer_y)

        def post_mouse(event_type: int, x: float, y: float) -> None:
            nonlocal mouse_pressed, last_pointer
            emit_mouse(event_type, x, y)
            last_pointer = (x, y)
            if event_type == MACOS_LEFT_MOUSE_DOWN:
                mouse_pressed = True
            elif event_type == MACOS_LEFT_MOUSE_UP:
                mouse_pressed = False

        def wait_for_drag_state(label: str, predicate: Callable[[int], bool]) -> Dict[str, object]:
            deadline = time.monotonic() + 3.0
            last: Dict[str, object] = {}
            while time.monotonic() < deadline:
                last = live.json_ctl("read_screen", {"include_selection": True})
                selection = last.get("selection")
                if (
                    predicate(int(last.get("display_offset", 0)))
                    and isinstance(selection, str)
                    and selection
                ):
                    return last
                time.sleep(0.05)
            raise SystemExit(
                f"selection-autoscroll smoke: timed out waiting for {label}: {last}"
            )

        armed: Dict[str, object] = {}
        content_top_y = client_origin_y + float(content["y"]) / scale_y
        inert_edge_y = content_top_y + 1.0
        inert_outside_y = content_top_y - 0.5
        upper_edge_y = client_origin_y + (float(content["y"]) + 2.0) / scale_y
        try:
            # The inner zone is drag-only. A press held at the edge must not
            # scroll until pointer motion turns it into a selection drag. Cross
            # above the client boundary by half a point too. Native capture may
            # deliver that as an out-of-client move rather than CursorLeft, and
            # either must stay inert below the two-point threshold. A posted
            # macOS activation click can occasionally be swallowed;
            # retry only when the positive control proves the pane press never
            # landed. A delivered press that scrolls still fails immediately.
            held_click: Dict[str, object] = {}
            for _ in range(3):
                if focus_point is not None:
                    post_mouse(MACOS_MOUSE_MOVED, *focus_point)
                    post_mouse(MACOS_LEFT_MOUSE_DOWN, *focus_point)
                    post_mouse(MACOS_LEFT_MOUSE_UP, *focus_point)
                    time.sleep(0.2)
                post_mouse(MACOS_MOUSE_MOVED, pointer_x, inert_edge_y)
                post_mouse(MACOS_LEFT_MOUSE_DOWN, pointer_x, inert_edge_y)
                # Exercise the native duplicate-event case deterministically,
                # then add a small inward jitter and cross the top by half a
                # coordinate unit. The farthest position stays below the
                # two-logical-point threshold on the native macOS driver and
                # on the scale >= 1 hosted portable legs. Pure behavior tests
                # cover representative positive and invalid display scales.
                post_mouse(MACOS_LEFT_MOUSE_DRAGGED, pointer_x, inert_edge_y)
                post_mouse(MACOS_LEFT_MOUSE_DRAGGED, pointer_x, inert_edge_y + 1.0)
                post_mouse(MACOS_LEFT_MOUSE_DRAGGED, pointer_x, inert_outside_y)
                time.sleep(0.2)
                held_click = live.json_ctl("read_screen")
                post_mouse(MACOS_LEFT_MOUSE_UP, pointer_x, inert_outside_y)
                if int(held_click.get("display_offset", 0)) != 0:
                    raise SystemExit(
                        "selection-autoscroll smoke: sub-threshold boundary jitter "
                        "scrolled before any drag"
                    )
                if held_click.get("selection_present"):
                    break
            else:
                raise SystemExit(
                    "selection-autoscroll smoke: the edge press never started a "
                    "selection after three attempts, so the inert check proved nothing"
                )
            for _ in range(3):
                if focus_point is not None:
                    post_mouse(MACOS_MOUSE_MOVED, *focus_point)
                    post_mouse(MACOS_LEFT_MOUSE_DOWN, *focus_point)
                    post_mouse(MACOS_LEFT_MOUSE_UP, *focus_point)
                    time.sleep(0.2)
                post_mouse(MACOS_MOUSE_MOVED, pointer_x, pointer_y)
                post_mouse(MACOS_LEFT_MOUSE_DOWN, pointer_x, pointer_y)
                post_mouse(
                    MACOS_LEFT_MOUSE_DRAGGED,
                    pointer_x,
                    pointer_y - cell_height * 2.0,
                )
                time.sleep(0.15)
                armed = live.json_ctl("read_screen", {"include_selection": True})
                if isinstance(armed.get("selection"), str) and armed["selection"]:
                    break
                post_mouse(
                    MACOS_LEFT_MOUSE_UP, pointer_x, pointer_y - cell_height * 2.0
                )
            else:
                raise SystemExit(
                    "selection-autoscroll smoke: pointer did not arm a selection after three attempts "
                    f"(driver={pointer_driver}, frame={native_window_frame}, "
                    f"surface={surface}, scale={(scale_x, scale_y)}, "
                    f"start={(pointer_x, pointer_y)}, state={armed})"
                )
            for step in range(1, 19):
                y = pointer_y + (upper_edge_y - pointer_y) * step / 18.0
                post_mouse(MACOS_LEFT_MOUSE_DRAGGED, pointer_x, y)
                time.sleep(1.0 / 60.0)
            during = wait_for_drag_state(
                "the upper edge to enter scrollback", lambda value: value > 0
            )
            offset = int(during.get("display_offset", 0))
            lower_edge_y = client_origin_y + (
                float(content["y"]) + float(content["height"]) - 2.0
            ) / scale_y
            for step in range(1, 25):
                y = upper_edge_y + (lower_edge_y - upper_edge_y) * step / 24.0
                post_mouse(MACOS_LEFT_MOUSE_DRAGGED, pointer_x, y)
                time.sleep(1.0 / 60.0)
            down = wait_for_drag_state(
                "the lower edge to reach the live bottom", lambda value: value == 0
            )
            down_offset = int(down.get("display_offset", 0))
            post_mouse(MACOS_LEFT_MOUSE_UP, pointer_x, lower_edge_y)
        finally:
            if mouse_pressed:
                try:
                    emit_mouse(MACOS_LEFT_MOUSE_UP, *last_pointer)
                except BaseException as error:
                    print(
                        f"selection-autoscroll smoke: could not release the native mouse button: {error}",
                        file=sys.stderr,
                    )
        (out / "analysis.json").write_text(
            json.dumps(
                {
                    "display_offset_before": int(before.get("display_offset", 0)),
                    "display_offset_while_holding_edge_click": int(
                        held_click.get("display_offset", 0)
                    ),
                    "display_offset_while_dragging_above": offset,
                    "display_offset_while_dragging_below": down_offset,
                    "selection": during.get("selection", ""),
                    "selection_present_after_return": down.get("selection_present", False),
                    "pointer_driver": pointer_driver,
                    "native_window_frame": native_window_frame,
                    "surface_scale": [scale_x, scale_y],
                },
                indent=2,
            )
            + "\n"
        )
        if offset <= 0:
            raise SystemExit(
                "selection-autoscroll smoke: dragging above the pane did not move into scrollback"
            )
        if not (
            isinstance(during.get("selection"), str)
            and during["selection"]
            and isinstance(down.get("selection"), str)
            and down["selection"]
        ):
            raise SystemExit(
                "selection-autoscroll smoke: viewport moved without preserving selected text"
            )
        if down_offset != 0:
            raise SystemExit(
                "selection-autoscroll smoke: dragging below the pane did not return to the live bottom "
                f"(above={offset}, below={down_offset})"
            )
    return out


def run_tearoff(kettle: str, root: Path) -> Path:
    """Tier-1 deterministic tear-off: the mouseless `move_tab_to_new_window`
    action must detach the active tab into a second live window (PTYs
    intact) and broadcast `tab_moved`. The mouse-driven tear/re-dock
    gesture needs REAL pointer input (the ctl `send_mouse` path cannot
    reach `maybe_tear_off` by design) — that lives in
    scripts/check-tearoff-live-smoke.sh, not here."""
    out = root / f"tearoff-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "tab-bar-position = top",
                "detachable-tabs = true",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 120",
                "window-height = 30",
            ]
        )
        + "\n"
    )
    with LiveKettle(kettle, cfg, out / "kettle.log") as live:
        events = EventStream(live, out / "events.ndjson")
        try:
            live.ctl("perform_action", params={"action": "new_tab"})
            tabs = live.json_ctl("list_tabs")
            rows = [t for t in tabs.get("tabs", []) if isinstance(t, dict)]
            if len(rows) != 2 or {t.get("window") for t in rows} != {1}:
                raise SystemExit(f"tearoff smoke: expected 2 tabs in window 1, got {tabs}")
            live.screenshot(out / "before-tear.png")

            # v2.40.0: the new diagnostic surface the gesture smokes key off.
            bar = live.json_ctl("ui_geometry").get("tab_bar", {})
            for key in ("tear_lift", "dock_highlighted", "band"):
                if key not in bar:
                    raise SystemExit(f"tearoff smoke: ui_geometry tab_bar missing {key!r}: {bar}")
            if bar.get("tear_lift") != 0.0 or bar.get("dock_highlighted") is not False:
                raise SystemExit(f"tearoff smoke: idle drag diagnostics not at rest: {bar}")

            live.ctl("perform_action", params={"action": "move_tab_to_new_window"})
            moved = events.wait_for("tab_moved", {"from_window": 1})
            to_window = moved.get("data", {}).get("to_window")
            for _ in range(50):
                tabs = live.json_ctl("list_tabs")
                rows = [t for t in tabs.get("tabs", []) if isinstance(t, dict)]
                windows = {t.get("window") for t in rows}
                if len(rows) == 2 and len(windows) == 2:
                    break
                time.sleep(0.1)
            else:
                raise SystemExit(f"tearoff smoke: tab did not land in a second window: {tabs}")
            if to_window not in windows:
                raise SystemExit(
                    f"tearoff smoke: tab_moved reported to_window={to_window}, windows={windows}"
                )
            live.screenshot(out / "after-tear.png")
            (out / "tabs.json").write_text(json.dumps(tabs, indent=2) + "\n")
            (out / "analysis.json").write_text(
                json.dumps(
                    {
                        "tab_moved": moved,
                        "windows": sorted(str(w) for w in windows),
                        "diagnostic_keys": sorted(bar.keys()),
                    },
                    indent=2,
                )
                + "\n"
            )
        finally:
            events.close()
    return out


def run_window_close_isolation(kettle: str, root: Path) -> Path:
    """Terminate one detached window's child and prove its sibling survives.

    The reported failure involved Codex, but the child program is immaterial to
    Kettle's reap/window-lifecycle path. Using the native shell makes the test
    deterministic on every platform while exercising the same PTY exit event
    that an exited CLI produces.
    """
    launch_env: Dict[str, Optional[str]] = {}
    if platform.system() == "Linux":
        if not os.environ.get("DISPLAY"):
            raise SystemExit(
                "window-close-isolation smoke: Linux requires an X11 display; "
                "native Wayland surfaces have no portable independent window inventory"
            )
        require_cmd("xdotool")
        # xdotool can inventory only X11 windows. A Wayland login often exports
        # both backends, and winit 0.30 intentionally prefers Wayland. Remove
        # only the Wayland selectors from this child's environment so winit
        # selects the retained DISPLAY and the independent assertion can see
        # the same native windows Kettle created.
        launch_env["WAYLAND_DISPLAY"] = None
        launch_env["WAYLAND_SOCKET"] = None

    out = root / f"window-close-isolation-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "tab-bar-position = top",
                "detachable-tabs = true",
                "ask-before-closing = never",
                "exit-action = close",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "window-width = 120",
                "window-height = 30",
            ]
        )
        + "\n"
    )
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_env=launch_env) as live:
        initial = live.json_ctl("list_tabs")
        initial_rows = [
            row for row in initial.get("tabs", []) if isinstance(row, dict)
        ]
        if len(initial_rows) != 1:
            raise SystemExit(
                "window-close-isolation smoke: expected one initial tab, "
                f"got {initial}"
            )
        original_window = initial_rows[0].get("window")
        original_pane = initial_rows[0].get("focused_pane")
        if not isinstance(original_window, int) or not isinstance(original_pane, int):
            raise SystemExit(
                "window-close-isolation smoke: initial inventory is malformed: "
                f"{initial_rows[0]}"
            )
        native_before = wait_for_native_window_ids(
            live.pid,
            lambda windows: len(windows) == 1,
            label="the initial native Kettle window",
        )

        tracked_before = 0
        if os.name != "nt":
            live.wait_for_tracker_sessions(1)
            tracked_before = len(live._tracker_sessions)

        live.ctl("perform_action", params={"action": "new_tab"})
        if os.name != "nt":
            live.wait_for_tracker_sessions(tracked_before + 1)
        live.ctl("perform_action", params={"action": "move_tab_to_new_window"})
        split: Dict[str, object] = {}
        split_rows: List[Dict[str, object]] = []
        for _ in range(80):
            split = live.json_ctl("list_tabs")
            split_rows = [
                row for row in split.get("tabs", []) if isinstance(row, dict)
            ]
            if len(split_rows) == 2 and len({row.get("window") for row in split_rows}) == 2:
                break
            time.sleep(0.1)
        else:
            raise SystemExit(
                "window-close-isolation smoke: tab did not detach into a second window: "
                f"{split}"
            )

        detached = [row for row in split_rows if row.get("window") != original_window]
        if len(detached) != 1 or not isinstance(detached[0].get("focused_pane"), int):
            raise SystemExit(
                "window-close-isolation smoke: detached inventory is malformed: "
                f"{split_rows}"
            )
        detached_window = detached[0]["window"]
        detached_pane = detached[0]["focused_pane"]
        native_split = wait_for_native_window_ids(
            live.pid,
            lambda windows: native_before < windows and len(windows) == 2,
            label="the detached native Kettle window",
        )
        detached_native = native_split - native_before
        if len(detached_native) != 1:
            raise SystemExit(
                "window-close-isolation smoke: native detach inventory was ambiguous: "
                f"before={sorted(native_before)} after={sorted(native_split)}"
            )

        # Terminate the detached child, rather than clicking chrome, so the
        # asynchronous PTY reap path is the one under test. That is the path a
        # CLI exiting inside one of several Kettle windows takes.
        # `send_text` is literal. A line feed submits a Unix shell line but is
        # not the Enter key ConPTY expects, so the Windows run previously sat
        # here without ever exercising the child-exit path. Type the command,
        # then encode Enter through the pane's live terminal mode just as a
        # real key press does.
        live.ctl("send_text", params={"pane": detached_pane, "text": "exit"})
        live.ctl(
            "send_keys",
            params={"pane": detached_pane, "keys": ["enter"]},
        )
        remaining: Dict[str, object] = {}
        for _ in range(100):
            exited = (
                live.proc.poll() is not None
                if os.name == "nt"
                else process_exited_without_reaping(live.proc)
            )
            if exited:
                raise SystemExit(
                    "window-close-isolation smoke: the Kettle process exited with a "
                    "sibling window still expected"
                )
            remaining = live.json_ctl("list_tabs")
            rows = [
                row for row in remaining.get("tabs", []) if isinstance(row, dict)
            ]
            if len(rows) == 1 and rows[0].get("window") == original_window:
                break
            time.sleep(0.1)
        else:
            raise SystemExit(
                "window-close-isolation smoke: detached child exit did not remove only "
                f"its own window: before={split} after={remaining}"
            )

        state = live.json_ctl("get_state")
        if state.get("windows") != 1:
            raise SystemExit(
                "window-close-isolation smoke: detached native window remained mapped: "
                f"{state}"
            )
        stale_geometry = live.ctl(
            "ui_geometry",
            params={"window": detached_window},
            raw=True,
            allow_fail=True,
        )
        stale_detail = f"{stale_geometry.stderr}\n{stale_geometry.stdout}"
        if stale_geometry.returncode == 0 or "no window with id" not in stale_detail:
            raise SystemExit(
                "window-close-isolation smoke: detached window still answered an exact "
                f"geometry query: {stale_detail}"
            )
        native_after = wait_for_native_window_ids(
            live.pid,
            lambda windows: windows == native_before,
            label="native destruction of only the detached Kettle window",
        )

        marker = "KETTLE_WINDOW_CLOSE_SIBLING_SURVIVED"
        command = command_with_marker(
            "Write-Output sibling-window-live"
            if platform.system() == "Windows"
            else "printf 'sibling-window-live\\n'",
            marker,
        )
        live.ctl("send_text", params={"pane": original_pane, "text": command})
        live.ctl(
            "send_keys",
            params={"pane": original_pane, "keys": ["enter"]},
        )
        sibling_screen: Dict[str, object] = {}
        for _ in range(50):
            sibling_screen = live.json_ctl(
                "read_screen",
                params={"pane": original_pane, "scrollback_lines": 20},
            )
            if marker in str(sibling_screen.get("text", "")):
                break
            time.sleep(0.1)
        else:
            raise SystemExit(
                "window-close-isolation smoke: surviving sibling no longer accepted "
                f"terminal input: {sibling_screen}"
            )

        (out / "analysis.json").write_text(
            json.dumps(
                {
                    "original_window": original_window,
                    "original_pane": original_pane,
                    "closed_window": detached_window,
                    "closed_pane": detached_pane,
                    "native_windows_before": sorted(native_before),
                    "native_windows_detached": sorted(native_split),
                    "native_windows_after": sorted(native_after),
                    "closed_native_window": sorted(detached_native)[0],
                    "tabs_before_close": split,
                    "tabs_after_close": remaining,
                    "sibling_marker": marker,
                },
                indent=2,
            )
            + "\n"
        )
    return out


def run_tab_title(kettle: str, root: Path) -> Path:
    out = root / f"tab-title-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "tab-bar-position = top",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 220",
                "window-height = 30",
            ]
        )
        + "\n"
    )
    nested = out / "fixture" / "Repos" / "SPI-1" / "platform"
    nested.mkdir(parents=True, exist_ok=True)
    expected_path = str(nested)
    home = os.path.expanduser("~")
    expected_display = "~" + expected_path[len(home) :] if expected_path.startswith(home + os.sep) else expected_path
    marker = "KETTLE_TAB_TITLE_READY"
    title = "..PI-1/platform"
    command = cwd_title_command(expected_path, title, marker)
    extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"] if platform.system() == "Windows" else []

    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live.ctl("send_text", params={"text": command + "\n"})
        panes: Dict[str, object] = {}
        for _ in range(50):
            panes = live.json_ctl("list_panes")
            pane_rows = panes.get("panes", [])
            focused = [p for p in pane_rows if isinstance(p, dict) and p.get("focused")]
            if (
                len(focused) == 1
                and focused[0].get("title") == title
                and focused[0].get("cwd") == expected_path
            ):
                break
            time.sleep(0.1)
        else:
            screen = live.json_ctl("read_screen", params={"scrollback_lines": 20})
            raise SystemExit(
                "tab-title smoke: pane title/cwd did not settle; "
                f"panes={panes} screen={screen.get('text')!r}"
            )

        tabs = live.json_ctl("list_tabs")
        geo: Dict[str, object] = {}
        for _ in range(20):
            geo = live.json_ctl("ui_geometry")
            segments = geo.get("tab_bar", {}).get("segments", [])  # type: ignore[union-attr]
            active = [s for s in segments if isinstance(s, dict) and s.get("active")]
            if active and active[0].get("fitted_title") == expected_display:
                break
            time.sleep(0.1)

        (out / "panes.json").write_text(json.dumps(panes, indent=2) + "\n")
        (out / "tabs.json").write_text(json.dumps(tabs, indent=2) + "\n")
        (out / "geometry.json").write_text(json.dumps(geo, indent=2) + "\n")

    pane_rows = panes.get("panes", [])
    focused = [p for p in pane_rows if isinstance(p, dict) and p.get("focused")]
    if len(focused) != 1:
        raise SystemExit(f"tab-title smoke: expected one focused pane, got {focused}")
    pane = focused[0]
    if pane.get("title") != title:
        raise SystemExit(f"tab-title smoke: raw pane title did not preserve shell title: {pane}")
    if pane.get("cwd") != expected_path:
        raise SystemExit(
            f"tab-title smoke: pane cwd did not track the shell-reported cwd: "
            f"got {pane.get('cwd')!r}, expected {expected_path!r}"
        )

    tab_rows = tabs.get("tabs", [])
    active_tabs = [t for t in tab_rows if isinstance(t, dict) and t.get("active")]
    if len(active_tabs) != 1:
        raise SystemExit(f"tab-title smoke: expected one active tab, got {active_tabs}")
    if active_tabs[0].get("title") != "platform":
        raise SystemExit(f"tab-title smoke: semantic tab title not normalized: {active_tabs[0]}")

    segments = geo.get("tab_bar", {}).get("segments", [])  # type: ignore[union-attr]
    active_segments = [s for s in segments if isinstance(s, dict) and s.get("active")]
    if len(active_segments) != 1:
        raise SystemExit(f"tab-title smoke: expected one active segment, got {active_segments}")
    seg = active_segments[0]
    if seg.get("title") != "platform":
        raise SystemExit(f"tab-title smoke: geometry title not normalized: {seg}")
    if seg.get("path") != expected_display:
        raise SystemExit(f"tab-title smoke: geometry path missing cwd metadata: {seg}")
    if seg.get("fitted_title") != expected_display:
        raise SystemExit(f"tab-title smoke: wide tab did not fit full cwd: {seg}")
    return out


def run_split_titlebar_position(
    kettle: str,
    cfg: Path,
    out: Path,
    extra_args: List[str],
    command: str,
    marker: str,
    expected_path: str,
    truncated_title: str,
    title_at_bottom: bool,
    assert_semantics: Callable[
        [Dict[str, object], Dict[str, object], str], None
    ],
) -> Dict[str, object]:
    position = "bottom" if title_at_bottom else "top"
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live.ctl("send_text", params={"text": command + "\n"})
        live.wait_for_text(marker)
        initial: Dict[str, object] = {}
        for _ in range(50):
            initial = live.json_ctl("list_panes")
            initial_rows = initial.get("panes", [])
            focused = [
                pane
                for pane in initial_rows
                if isinstance(pane, dict) and pane.get("focused")
            ]
            if (
                len(focused) == 1
                and focused[0].get("title") == truncated_title
                and focused[0].get("cwd") == expected_path
            ):
                break
            time.sleep(0.1)
        else:
            screen = live.json_ctl("read_screen", params={"scrollback_lines": 20})
            raise SystemExit(
                f"split-titlebar smoke ({position}): pane title/cwd did not "
                f"settle; panes={initial} screen={screen.get('text')!r}"
            )

        live.json_ctl("perform_action", params={"action": "split_right"})
        panes: Dict[str, object] = {}
        inactive_geometry: Dict[str, object] = {}
        for _ in range(40):
            panes = live.json_ctl("list_panes")
            pane_rows = panes.get("panes", [])
            inactive_geometry = live.json_ctl("ui_geometry")
            titlebars = inactive_geometry.get("pane_titlebars", [])
            if (
                isinstance(pane_rows, list)
                and len(pane_rows) >= 2
                and isinstance(titlebars, list)
                and len(titlebars) >= 2
            ):
                break
            time.sleep(0.1)

        assert_semantics(panes, inactive_geometry, position)
        inactive_screenshot = out / "inactive.png"
        live.screenshot(inactive_screenshot)
        try:
            inactive_analysis = analyze_split_titlebar_png(
                inactive_geometry,
                inactive_screenshot,
                title_at_bottom=title_at_bottom,
                broadcast=False,
            )
        except RuntimeError as error:
            raise SystemExit(str(error)) from error

        action = live.json_ctl(
            "perform_action", params={"action": "toggle_broadcast_all"}
        )
        receiving_geometry = live.json_ctl("ui_geometry")
        assert_semantics(panes, receiving_geometry, position)
        receiving_screenshot = out / "receiving.png"
        live.screenshot(receiving_screenshot)
        try:
            receiving_analysis = analyze_split_titlebar_png(
                receiving_geometry,
                receiving_screenshot,
                title_at_bottom=title_at_bottom,
                broadcast=True,
            )
        except RuntimeError as error:
            raise SystemExit(str(error)) from error

        (out / "panes.json").write_text(
            json.dumps(panes, indent=2) + "\n", encoding="utf-8"
        )
        (out / "geometry-inactive.json").write_text(
            json.dumps(inactive_geometry, indent=2) + "\n",
            encoding="utf-8",
        )
        (out / "geometry-receiving.json").write_text(
            json.dumps(receiving_geometry, indent=2) + "\n",
            encoding="utf-8",
        )
        (out / "broadcast-action.json").write_text(
            json.dumps(action, indent=2) + "\n", encoding="utf-8"
        )

    return {
        "position": position,
        "title_at_bottom": title_at_bottom,
        "artifacts": {
            "inactive_png": inactive_screenshot.name,
            "receiving_png": receiving_screenshot.name,
            "inactive_geometry": "geometry-inactive.json",
            "receiving_geometry": "geometry-receiving.json",
        },
        "inactive": inactive_analysis,
        "receiving": receiving_analysis,
    }


def run_split_titlebar(kettle: str, root: Path) -> Path:
    out = root / (
        f"split-titlebar-{time.strftime('%Y%m%d-%H%M%S')}-"
        f"{secrets.token_hex(4)}"
    )
    out.mkdir(parents=True, exist_ok=True)
    nested = (
        out
        / "fixture"
        / "Repos"
        / "SPI-1"
        / "flight-event-line-server-go"
    )
    nested.mkdir(parents=True, exist_ok=True)
    expected_path = str(nested)
    marker = "KETTLE_SPLIT_TITLEBAR_READY"
    truncated_title = "..PI-1/flight-event-line-server-go"
    command = cwd_title_command(expected_path, truncated_title, marker)
    extra_args = (
        ["-e", "powershell.exe", "-NoLogo", "-NoProfile"]
        if platform.system() == "Windows"
        else []
    )

    def assert_semantics(
        panes: Dict[str, object],
        geometry: Dict[str, object],
        position: str,
    ) -> None:
        pane_rows = panes.get("panes", [])
        if not isinstance(pane_rows, list) or len(pane_rows) < 2:
            raise SystemExit(
                f"split-titlebar smoke ({position}): split did not create two "
                f"panes: {panes}"
            )
        if not any(
            isinstance(pane, dict) and pane.get("title") == truncated_title
            for pane in pane_rows
        ):
            raise SystemExit(
                f"split-titlebar smoke ({position}): raw truncated title was not "
                f"preserved: {panes}"
            )
        titlebars = geometry.get("pane_titlebars", [])
        if not isinstance(titlebars, list) or len(titlebars) < 2:
            raise SystemExit(
                f"split-titlebar smoke ({position}): missing pane titlebar "
                f"diagnostics: {geometry}"
            )
        for titlebar in titlebars:
            if not isinstance(titlebar, dict):
                raise SystemExit(
                    f"split-titlebar smoke ({position}): malformed titlebar: "
                    f"{titlebar}"
                )
            if titlebar.get("path") != expected_path:
                raise SystemExit(
                    f"split-titlebar smoke ({position}): titlebar path did not "
                    f"track cwd: {titlebar}"
                )
            fitted = titlebar.get("fitted_title")
            bar_rect = titlebar.get("rect")
            cell = geometry.get("cell")
            bar_width = (
                bar_rect.get("width") if isinstance(bar_rect, dict) else None
            )
            cell_width = cell.get("width") if isinstance(cell, dict) else None
            valid_metrics = (
                isinstance(bar_width, (int, float))
                and not isinstance(bar_width, bool)
                and isinstance(cell_width, (int, float))
                and not isinstance(cell_width, bool)
                and math.isfinite(float(bar_width))
                and math.isfinite(float(cell_width))
                and float(bar_width) > 0.0
                and float(cell_width) > 0.0
            )
            title_budget = (
                math.floor(float(bar_width) / float(cell_width))
                if valid_metrics
                else 0
            )
            full_path_fits = (
                title_budget > 0 and len(expected_path) + 2 <= title_budget
            )
            fitted_path = fitted.strip() if isinstance(fitted, str) else ""
            if (
                not fitted_path
                or (full_path_fits and fitted_path != expected_path)
                or (
                    not full_path_fits
                    and not fitted_path.endswith(Path(expected_path).name)
                )
            ):
                raise SystemExit(
                    f"split-titlebar smoke ({position}): titlebar did not fit "
                    f"authoritative cwd path/leaf: {titlebar}"
                )
            if fitted_path.startswith(".."):
                raise SystemExit(
                    f"split-titlebar smoke ({position}): rendered truncated shell "
                    f"title: {titlebar}"
                )

    runs: List[Dict[str, object]] = []
    for title_at_bottom in (False, True):
        position = "bottom" if title_at_bottom else "top"
        position_out = out / position
        position_out.mkdir()
        cfg = position_out / "config"
        cfg.write_text(
            "\n".join(
                [
                    "agent-server = full",
                    "tab-bar = always",
                    "tab-bar-position = top",
                    "status-bar = off",
                    "show-titlebar = true",
                    f"title-at-bottom = {str(title_at_bottom).lower()}",
                    "title-hide-sizetext = true",
                    "icon-bell = false",
                    "scrollbar = never",
                    "padding-x = 8",
                    "padding-y = 8",
                    "handle-size = 1",
                    "unfocused-split-opacity = 1.0",
                    "inactive-color-offset = 1.0",
                    "inactive-bg-color-offset = 1.0",
                    "restore-session = false",
                    "update-check = false",
                    f"title-transmit-bg-color = {SPLIT_TITLEBAR_COLOR_HEX['transmit']}",
                    f"title-receive-bg-color = {SPLIT_TITLEBAR_COLOR_HEX['receive']}",
                    f"title-inactive-bg-color = {SPLIT_TITLEBAR_COLOR_HEX['inactive']}",
                    "title-transmit-fg-color = #ffffff",
                    "title-receive-fg-color = #ffffff",
                    "title-inactive-fg-color = #ffffff",
                    f"background = {SPLIT_TITLEBAR_COLOR_HEX['grid']}",
                    "foreground = #f4f4f4",
                    "window-width = 240",
                    "window-height = 60",
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        run_evidence = run_split_titlebar_position(
            kettle,
            cfg,
            position_out,
            extra_args,
            command,
            marker,
            expected_path,
            truncated_title,
            title_at_bottom,
            assert_semantics,
        )
        runs.append(run_evidence)

    (out / "analysis.json").write_text(
        json.dumps({"runs": runs}, indent=2) + "\n", encoding="utf-8"
    )
    return out


def run_zoom_keybind(kettle: str, root: Path) -> Path:
    out = root / f"zoom-keybind-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "font-size = 13",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 100",
                "window-height = 28",
            ]
        )
        + "\n"
    )

    def font_size(geo: Dict[str, object]) -> float:
        cell = geo.get("cell")
        if not isinstance(cell, dict):
            raise SystemExit(f"zoom-keybind smoke: missing cell geometry: {geo}")
        value = cell.get("font_size")
        if not isinstance(value, (int, float)):
            raise SystemExit(f"zoom-keybind smoke: missing font_size: {geo}")
        return float(value)

    def wait_font(live: LiveKettle, expected: float) -> Dict[str, object]:
        last: Dict[str, object] = {}
        for _ in range(30):
            last = live.json_ctl("ui_geometry")
            if abs(font_size(last) - expected) < 0.01:
                return last
            time.sleep(0.1)
        raise SystemExit(
            f"zoom-keybind smoke: expected font size {expected}, got {font_size(last)}"
        )

    steps = [
        (
            "physical-equal-shift",
            {"logical": "unidentified", "physical": "Equal", "mods": "ctrl+shift"},
            14.0,
            "IncreaseFontSize",
        ),
        (
            "numpad-subtract",
            {"logical": "unidentified", "physical": "NumpadSubtract", "mods": "ctrl"},
            13.0,
            "DecreaseFontSize",
        ),
        (
            "numpad-add",
            {"logical": "unidentified", "physical": "NumpadAdd", "mods": "ctrl"},
            14.0,
            "IncreaseFontSize",
        ),
        (
            "digit-reset",
            {"logical": "unidentified", "physical": "Digit0", "mods": "ctrl"},
            13.0,
            "ResetFontSize",
        ),
    ]

    analysis: Dict[str, object] = {"steps": []}
    with LiveKettle(kettle, cfg, out / "kettle.log") as live:
        initial = wait_font(live, 13.0)
        (out / "geometry-initial.json").write_text(json.dumps(initial, indent=2) + "\n")
        for label, params, expected, action in steps:
            result = live.json_ctl("dispatch_keybind", params=params)
            (out / f"{label}.dispatch.json").write_text(json.dumps(result, indent=2) + "\n")
            if result.get("action") != action or result.get("dispatched") is not True:
                raise SystemExit(
                    f"zoom-keybind smoke: {label} dispatched wrong action: {result}"
                )
            geo = wait_font(live, expected)
            (out / f"{label}.geometry.json").write_text(json.dumps(geo, indent=2) + "\n")
            analysis["steps"].append(
                {
                    "label": label,
                    "params": params,
                    "dispatch": result,
                    "font_size": font_size(geo),
                }
            )
    (out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    return out


def run_split_exit_resize(kettle: str, root: Path) -> Path:
    """A split closed by its own shell must hand its rows back.

    `Mux::reap` prunes the dead pane and promotes the sibling into the whole
    rectangle. The renderer paints from a live layout, so the survivor looks
    right immediately whether or not its PTY moved; only the grid and the tty
    winsize tell the truth. Splitting away from an agent CLI and typing `exit`
    used to leave it painting into the box it had inside the split.

    Two reap sites can service this, the one in `redraw` and the one in
    `about_to_wait_inner`, and whichever runs first wins. This scenario cannot
    say which; the source guard `every_reap_site_schedules_the_survivor_resize`
    pins both. What only a live window can prove is that a self-exiting PTY
    drives a real resize on the pane that inherits its space.

    Nothing between the `exit` and the assertion dispatches an action. The tail
    of `handle_action` marks a resize after any action at all, so a stray
    keystroke or menu call would schedule the resize this test is trying to
    catch the absence of, and the whole thing would pass against the bug.
    """
    out = root / f"split-exit-resize-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                # A split gives every pane a titlebar and a single pane has
                # none, so the survivor only returns to its exact starting rows
                # if the inset is given back too.
                "show-titlebar = true",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 100",
                "window-height = 40",
            ]
        )
        + "\n"
    )

    def panes_of(live: LiveKettle) -> List[Dict[str, object]]:
        listed = live.json_ctl("list_panes")
        value = listed.get("panes")
        return [p for p in value if isinstance(p, dict)] if isinstance(value, list) else []

    def wait_panes(
        live: LiveKettle, want: int, why: str, timeout: float = 10.0
    ) -> List[Dict[str, object]]:
        deadline = time.monotonic() + timeout
        seen: List[Dict[str, object]] = []
        while time.monotonic() < deadline:
            seen = panes_of(live)
            if len(seen) == want:
                return seen
            time.sleep(0.1)
        raise SystemExit(
            f"split-exit-resize smoke: expected {want} pane(s) {why}, saw {len(seen)}: {seen}"
        )

    def size_of(panes: List[Dict[str, object]], pane_id: object) -> Tuple[int, int]:
        for pane in panes:
            if pane.get("id") == pane_id:
                return int(pane.get("cols", 0)), int(pane.get("rows", 0))
        raise SystemExit(f"split-exit-resize smoke: pane {pane_id} vanished: {panes}")

    analysis: Dict[str, object] = {}
    with LiveKettle(kettle, cfg, out / "kettle.log") as live:
        before = wait_panes(live, 1, "before the split")
        base_id = before[0].get("id")
        baseline = size_of(before, base_id)
        analysis["baseline"] = {"id": base_id, "cols": baseline[0], "rows": baseline[1]}

        live.json_ctl("perform_action", {"action": "split_down"})
        split = wait_panes(live, 2, "after split_down")
        split_size = size_of(split, base_id)
        if split_size[1] >= baseline[1]:
            raise SystemExit(
                "split-exit-resize smoke: the split did not shrink the source "
                f"pane, so the test proves nothing: baseline={baseline} "
                f"split={split_size}"
            )
        new_id = next((p.get("id") for p in split if p.get("focused") is True), None)
        if new_id is None or new_id == base_id:
            raise SystemExit(f"split-exit-resize smoke: no new focused pane: {split}")
        analysis["split"] = {"new_id": new_id, "cols": split_size[0], "rows": split_size[1]}

        # From here to the assertion: reads only.
        live.ctl("send_text", params={"pane": new_id, "text": "exit\r"})
        wait_panes(live, 1, "after the split shell exited")

        deadline = time.monotonic() + 10.0
        observed = split_size
        while time.monotonic() < deadline:
            observed = size_of(panes_of(live), base_id)
            if observed == baseline:
                break
            time.sleep(0.1)
        analysis["after_close"] = {"cols": observed[0], "rows": observed[1]}
        if observed != baseline:
            raise SystemExit(
                "split-exit-resize smoke: the surviving pane kept its split "
                f"size. baseline={baseline} inside the split={split_size} "
                f"after the split closed={observed}. The pane that inherited "
                "the space was never resized, so its child got no SIGWINCH."
            )

        # The grid alone is necessary but not sufficient: `try_resize_geometry`
        # commits local geometry even when the native resize returns an error.
        # Ask the tty itself what size it thinks it is.
        if platform.system() != "Windows":
            token = "KETTLE_WINSZ" + "_OK"
            live.ctl(
                "send_text",
                params={
                    "pane": base_id,
                    "text": f"printf '{token} %s\\n' \"$(stty size | tr ' ' x)\"\r",
                },
            )
            want = f"{baseline[1]}x{baseline[0]}"
            pattern = re.compile(re.escape(token) + r"\s+(\d+)x(\d+)")
            reported = None
            deadline = time.monotonic() + 10.0
            while time.monotonic() < deadline:
                found = pattern.findall(screen_text(live.json_ctl("read_screen")))
                if found:
                    reported = f"{found[-1][0]}x{found[-1][1]}"
                    break
                time.sleep(0.1)
            analysis["tty_winsize"] = {"reported": reported, "expected": want}
            if reported != want:
                raise SystemExit(
                    "split-exit-resize smoke: the grid came back but the tty "
                    f"winsize did not. stty size reported {reported}, expected "
                    f"{want}. The ioctl never reached the child."
                )
            live.screenshot(out / "after-close.png")

    (out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    return out


def run_touchpad_scroll(kettle: str, root: Path) -> Path:
    """Reproduce a Windows Precision Touchpad scroll gesture end to end.

    Precision touchpads emit a stream of WM_MOUSEWHEEL messages carrying far
    less than WHEEL_DELTA(120) each, which winit reports as fractional
    `LineDelta` notches. Before v2.41.0 kettle quantized every event on its own
    and each one rounded to zero, so touchpad scrolling was completely dead —
    and no test caught it, because the only synthetic wheel path
    (`wheel_lines`) injected pre-quantized whole lines and therefore skipped the
    broken conversion entirely.

    This drives the raw `wheel_delta` form, which runs the real sub-detent
    accumulator end to end: ctl -> WheelAccum -> dispatch -> scroll_display.

    Note it cannot be run against a pre-fix binary as a bisect probe, because
    `wheel_delta` shipped WITH the fix and an older build rejects the parameter
    outright. The numeric defect itself is pinned by the unit test
    `wheel_accum_carries_sub_notch_residue` in crates/kettle-ui/src/input.rs,
    which fails the moment the per-event rounding is reintroduced. This
    scenario's job is to prove the whole live path is wired up.
    """
    out = root / f"touchpad-scroll-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "font-size = 13",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 100",
                "window-height = 28",
            ]
        )
        + "\n"
    )

    # One real gesture's worth of motion: ~0.08 of a detent per event, which is
    # what a slow two-finger drag actually produces.
    step = 0.08
    events = 60

    analysis: Dict[str, object] = {"delta_per_event": step, "events": events}
    with LiveKettle(kettle, cfg, out / "kettle.log") as live:
        marker = "KETTLE_TOUCHPAD_SCROLL_DONE"
        if platform.system() == "Windows":
            fill_body = (
                "$esc=[char]27; [Console]::Write($esc + '[2J' + $esc + '[3J' + $esc + '[H'); "
                "1..140 | ForEach-Object { 'KETTLE_TOUCHPAD_SCROLL_{0:D3}' -f $_ }"
            )
        else:
            fill_body = (
                "printf '\\033[2J\\033[3J\\033[H'; "
                "for i in $(seq 1 140); do printf 'KETTLE_TOUCHPAD_SCROLL_%03d\\n' \"$i\"; done"
            )
        fill_cmd = command_with_marker(fill_body, marker)
        live_shell_command(live, fill_cmd, marker, timeout_ms=12000)
        bottom = live.json_ctl("read_screen")
        (out / "bottom.screen.json").write_text(json.dumps(bottom, indent=2) + "\n")
        if int(bottom.get("display_offset", 0)) != 0:
            raise SystemExit(
                f"touchpad smoke: expected bottom display_offset 0, got {bottom.get('display_offset')}"
            )
        bottom_screenshot = live.screenshot_if_visible(out / "bottom.png")

        # Scroll back with sub-detent events only. No single one of these can
        # move the viewport on its own; only the accumulated residue can.
        for _ in range(events):
            live.ctl("send_mouse", params={"event": "wheel", "wheel_delta": step})
        time.sleep(0.2)
        scrolled = live.json_ctl("read_screen")
        (out / "scrolled.screen.json").write_text(json.dumps(scrolled, indent=2) + "\n")
        scrolled_screenshot = live.screenshot_if_visible(out / "scrolled.png")
        analysis["screenshots_captured"] = bottom_screenshot and scrolled_screenshot
        offset = int(scrolled.get("display_offset", 0))
        analysis["display_offset_after_gesture"] = offset
        if offset <= 0:
            raise SystemExit(
                "touchpad smoke: sub-detent wheel deltas did not scroll at all "
                f"(display_offset={offset}). This is the precision-touchpad "
                "regression: every event quantized to zero and the residue was "
                "discarded."
            )
        # 60 x 0.08 detents = 4.8 detents = ~14 lines at the default multiplier.
        # Bound it loosely so the guard survives float slack, but tightly enough
        # to catch an accumulator that over- or under-drains by a wide margin.
        if not (8 <= offset <= 20):
            raise SystemExit(
                f"touchpad smoke: gesture scrolled {offset} lines, expected ~14 "
                "(60 events x 0.08 detents x 3 lines/detent)"
            )

        # The mirror gesture must land back exactly at the live bottom, proving
        # the residue is symmetric and nothing is silently dropped.
        for _ in range(events):
            live.ctl("send_mouse", params={"event": "wheel", "wheel_delta": -step})
        time.sleep(0.2)
        returned = live.json_ctl("read_screen")
        (out / "returned.screen.json").write_text(json.dumps(returned, indent=2) + "\n")
        analysis["display_offset_after_return"] = int(returned.get("display_offset", 0))
        if int(returned.get("display_offset", 0)) != 0:
            raise SystemExit(
                "touchpad smoke: mirrored gesture did not return to the live bottom "
                f"(display_offset={returned.get('display_offset')})"
            )

        # A whole detent still behaves exactly as it always did: 3 lines.
        live.ctl("send_mouse", params={"event": "wheel", "wheel_delta": 1.0})
        time.sleep(0.15)
        one = live.json_ctl("read_screen")
        (out / "one-detent.screen.json").write_text(json.dumps(one, indent=2) + "\n")
        analysis["display_offset_one_detent"] = int(one.get("display_offset", 0))
        if int(one.get("display_offset", 0)) != 3:
            raise SystemExit(
                "touchpad smoke: one whole detent must still scroll exactly 3 lines, got "
                f"{one.get('display_offset')}"
            )
    (out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    return out


def macos_session_locked(ioreg_plist: bytes) -> Optional[bool]:
    """Extract the macOS console-lock state from `ioreg -a` output."""
    try:
        roots = plistlib.loads(ioreg_plist)
    except (plistlib.InvalidFileException, ValueError):
        return None
    if isinstance(roots, dict):
        roots = [roots]
    if not isinstance(roots, list):
        return None
    observed_unlocked = False
    for root in roots:
        if not isinstance(root, dict):
            continue
        root_locked = root.get("IOConsoleLocked")
        if isinstance(root_locked, bool):
            return root_locked
        users = root.get("IOConsoleUsers")
        if not isinstance(users, list):
            continue
        for user in users:
            if not isinstance(user, dict):
                continue
            locked = user.get("CGSSessionScreenIsLocked")
            if locked is True:
                return True
            if locked is False:
                observed_unlocked = True
    return False if observed_unlocked else None


def live_session_failure_reason() -> Optional[str]:
    """Return why the live-UI smoke cannot run here, or None if it can.

    macOS has no DISPLAY/WAYLAND_DISPLAY -- it draws through the Quartz window
    server -- so the original X11/Wayland-only check skipped unconditionally on
    Darwin. `just agent-tui-smoke` therefore exited 0 having tested nothing,
    which reads identically to a pass. A live smoke now fails closed when it
    cannot prove that a usable graphical session exists.

    On Darwin the question is whether this process can reach the window server
    at all. `launchctl managername` answers it: a logged-in GUI session reports
    `Aqua`, while an SSH session reports `Background` or `StandardIO`. An Aqua
    bootstrap can still be locked, in which case Metal cannot present a live
    drawable, so the console lock state is a second required precondition.
    """
    system = platform.system()
    if system == "Windows":
        return None
    if system == "Darwin":
        try:
            manager = subprocess.run(
                ["launchctl", "managername"],
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            return "launchctl managername is unavailable, cannot prove a GUI session"
        managername = manager.stdout.strip()
        if manager.returncode != 0:
            return "launchctl managername failed, cannot prove a GUI session"
        if managername != "Aqua":
            return f"no macOS GUI session (launchctl managername = {managername or 'unknown'!s})"
        try:
            console = subprocess.run(
                ["ioreg", "-n", "Root", "-d", "1", "-a"],
                capture_output=True,
                timeout=5,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            return "ioreg is unavailable, cannot prove the macOS session is unlocked"
        if console.returncode != 0:
            return "ioreg failed, cannot prove the macOS session is unlocked"
        locked = macos_session_locked(console.stdout)
        if locked is None:
            return "macOS console lock state is unavailable"
        if locked:
            return "macOS console session is locked"
        try:
            wake = subprocess.run(
                ["caffeinate", "-u", "-t", "1"],
                capture_output=True,
                timeout=5,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            return "caffeinate is unavailable, cannot wake the macOS display"
        if wake.returncode != 0:
            return "caffeinate could not wake the macOS display"
        return None
    if os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"):
        return None
    return "no DISPLAY or WAYLAND_DISPLAY"


def main() -> int:
    if len(sys.argv) >= 2 and sys.argv[1] == PROVENANCE_ANCHOR_PROBE_ARG:
        anchor = _start_unix_repository_group_anchor()
        if anchor is None or len(sys.argv) != 3:
            print("repository anchor probe requires Unix and one record", file=sys.stderr)
            return 2
        record = Path(sys.argv[2])
        staged = record.with_name(f".{record.name}.{os.getpid()}.tmp")
        staged.write_text(f"{anchor}\n", encoding="ascii")
        os.replace(staged, record)
        if sys.stdin.read(1) != "1":
            print("repository anchor probe handshake failed", file=sys.stderr)
            return 2
        return 0
    if len(sys.argv) >= 2 and sys.argv[1] == PROVENANCE_WORKER_ARG:
        _start_unix_repository_group_anchor()
        if sys.stdin.read(1) != "1":
            print("repository provenance worker handshake failed", file=sys.stderr)
            return 2
        return _repository_provenance_worker(sys.argv[2:])
    if len(sys.argv) >= 2 and sys.argv[1] == PROVENANCE_SABOTAGE_WORKER_ARG:
        _start_unix_repository_group_anchor()
        if sys.stdin.read(1) != "1":
            print("repository provenance timeout probe handshake failed", file=sys.stderr)
            return 2
        return _repository_provenance_timeout_probe(sys.argv[2:])
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "case",
        choices=[
            "tabbar",
            "tab-title",
            "tearoff",
            "split-titlebar",
            "split-exit-resize",
            "zoom-keybind",
            "underline",
            "agent-tui",
            "search-history",
            "image-paste-receipt",
            "video-paste-receipt",
            "interaction",
            "selection-autoscroll",
            "hover-wheel",
            "window-close-isolation",
            "touchpad-scroll",
            "session-check",
            "self-test",
            "all",
        ],
    )
    kettle_source = parser.add_mutually_exclusive_group()
    kettle_source.add_argument(
        "--kettle", default=os.environ.get("KETTLE_BIN", "kettle")
    )
    kettle_source.add_argument(
        "--cargo-release",
        action="store_true",
        help=(
            "build kettle --release and select the exact executable reported "
            "by Cargo (honors CARGO_TARGET_DIR and configured target triples)"
        ),
    )
    parser.add_argument(
        "--out-dir",
        default=os.environ.get("KETTLE_DIAG_DIR"),
        help=(
            "artifact directory (default: an owner-private LocalAppData "
            "directory on Windows, target/diagnostics elsewhere)"
        ),
    )
    parser.add_argument(
        "--shell-mode",
        choices=["native", "wsl"],
        default="native",
        help=(
            "shell target for agent-tui: PowerShell on native Windows, "
            "non-rc Bash on native Unix, or Windows Kettle launching non-rc "
            "Bash through wsl.exe"
        ),
    )
    parser.add_argument(
        "--wsl-distro",
        default=os.environ.get("KETTLE_SMOKE_WSL_DISTRO"),
        help="optional WSL distribution name (defaults to the user's WSL default)",
    )
    parser.add_argument(
        "--astro-config",
        default=os.environ.get("KETTLE_SMOKE_ASTRO_CONFIG"),
        help=(
            "configured Neovim/AstroNvim directory in the target shell; the "
            "harness dereferences links into an isolated copy before use"
        ),
    )
    parser.add_argument(
        "--nvim-data",
        default=os.environ.get("KETTLE_SMOKE_NVIM_DATA"),
        help=(
            "Neovim data directory in the target shell; installed plugin "
            "runtime is copied into the disposable smoke sandbox"
        ),
    )
    args = parser.parse_args()

    if args.case == "self-test":
        live_helper_selftest()
        print("live-ui helper self-test: OK")
        return 0

    failure_reason = live_session_failure_reason()
    if failure_reason is not None:
        print(f"live-ui smoke: cannot run ({failure_reason})", file=sys.stderr)
        return 1
    if args.case == "session-check":
        print("live-ui smoke: graphical session ready")
        return 0

    if args.shell_mode != "native" and args.case != "agent-tui":
        parser.error("--shell-mode applies only to the agent-tui case")
    if args.wsl_distro and args.shell_mode != "wsl":
        parser.error("--wsl-distro requires --shell-mode wsl")
    if args.shell_mode == "wsl" and platform.system() != "Windows":
        parser.error("--shell-mode wsl requires the helper to run on Windows")
    if args.shell_mode == "wsl":
        require_cmd("wsl.exe")

    shell_target = AgentShellTarget(
        mode=args.shell_mode,
        wsl_distro=args.wsl_distro,
        astro_config=args.astro_config,
        nvim_data=args.nvim_data,
    )

    if args.cargo_release:
        args.kettle = resolve_release_kettle()

    if args.out_dir:
        root = Path(args.out_dir).resolve()
        root.mkdir(parents=True, exist_ok=True)
    else:
        root = create_default_diagnostic_root()
    if args.case in ("tabbar", "all"):
        out = run_tabbar(args.kettle, root)
        print(f"tabbar-click smoke: OK artifacts={out}")
    if args.case in ("tab-title", "all"):
        out = run_tab_title(args.kettle, root)
        print(f"tab-title smoke: OK artifacts={out}")
    if args.case in ("tearoff", "all"):
        out = run_tearoff(args.kettle, root)
        print(f"tearoff smoke: OK artifacts={out}")
    if args.case in ("split-titlebar", "all"):
        out = run_split_titlebar(args.kettle, root)
        print(f"split-titlebar smoke: OK artifacts={out}")
    if args.case in ("split-exit-resize", "all"):
        out = run_split_exit_resize(args.kettle, root)
        print(f"split-exit-resize smoke: OK artifacts={out}")
    if args.case in ("zoom-keybind", "all"):
        out = run_zoom_keybind(args.kettle, root)
        print(f"zoom-keybind smoke: OK artifacts={out}")
    if args.case in ("underline", "all"):
        missing = missing_commands("git", "delta", "less")
        if missing:
            raise SystemExit(
                "underline-scroll smoke: cannot run "
                f"({', '.join(missing)} not found)"
            )
        else:
            out = run_underline(args.kettle, root)
            print(f"underline-scroll smoke: OK artifacts={out}")
    if args.case in ("agent-tui", "all"):
        out = run_agent_tui(args.kettle, root, shell_target)
        print(f"agent-tui smoke: OK artifacts={out}")
    if args.case in ("search-history", "all"):
        out = run_search_history(args.kettle, root)
        print(f"search-history smoke: OK artifacts={out}")
    if args.case == "image-paste-receipt":
        out = run_image_paste_receipt(args.kettle, root)
        print(f"image-paste-receipt smoke: OK artifacts={out}")
    if args.case == "video-paste-receipt":
        out = run_video_paste_receipt(args.kettle, root)
        print(f"video-paste-receipt smoke: OK artifacts={out}")
    if args.case in ("interaction", "all"):
        out = run_interaction(args.kettle, root)
        print(f"interaction smoke: OK artifacts={out}")
    if args.case == "selection-autoscroll":
        out = run_selection_autoscroll(args.kettle, root)
        print(f"selection-autoscroll smoke: OK artifacts={out}")
    if args.case == "hover-wheel":
        out = run_hover_wheel(args.kettle, root)
        print(f"hover-wheel smoke: OK artifacts={out}")
    if args.case == "window-close-isolation":
        out = run_window_close_isolation(args.kettle, root)
        print(f"window-close-isolation smoke: OK artifacts={out}")
    if args.case in ("touchpad-scroll", "all"):
        out = run_touchpad_scroll(args.kettle, root)
        print(f"touchpad-scroll smoke: OK artifacts={out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
