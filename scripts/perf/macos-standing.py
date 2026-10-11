#!/usr/bin/env python3
"""Measure Kettle against installed macOS terminals, or two Kettle builds.

Each terminal runs with its default configuration (the user's own config files
are bypassed) on a pinned 120x36 grid, and every command inside it goes
through one generated script so no terminal pays a different quoting or
argument path. Workloads:

  startup       spawn to the first on-screen window and to the child's first
                instruction, from one CLOCK_UPTIME_RAW clock (launch.swift and
                stamp.c), with no polling granularity
  idle          CPU share, wakeups per second, and phys_footprint (Activity
                Monitor's Memory, which includes GPU driver memory) of a window
                left alone, sampled over --idle-window seconds after
                --idle-settle seconds
  flood-memory  a 100 ms phys_footprint timeline while printing 32 MiB of
                seeded text and for 20 s after it, reported as the peak and
                memory 3 s and 20 s after the text ends
  vtebench      Alacritty's vtebench at a pinned revision, built from a copy
                that records microseconds instead of whole milliseconds, with
                its scripts' window-size lookup fixed for macOS
  output-memory (opt in) 80 numbered ASCII lines at absolute 100 ms deadlines;
                current/lifetime footprint at the designated six-second query
  blink-window  (opt in) quiet shipped cursor, launch +2.5..+8.5 seconds;
                separate noninjecting validation certifies the unchanged setup
  latency       (opt in with --workloads; never a default) keystroke to
                screen: KettleLatencyProbe posts the key j, and the time runs
                to the display time of the first captured frame showing the
                payload's block flipped (see latency-probe.swift)

Rounds rotate the terminal order so no terminal always runs first. Startup,
idle and flood rows report medians; vtebench reports the mean of each
benchmark's samples per round, then the mean over rounds. Kettle is compared
with the best other terminal round by round: the geometric mean of the
per-round ratios with a Student-t 95% interval on their logs, and a count of
the rounds Kettle won. With --kettle-b the run compares two Kettle builds and
reports B/A the same way.

Each workload reuses one payload script (values that change per launch go in
a sourced params file), since macOS assesses a new script the first time it
runs, and one discarded warm-up launch per terminal precedes the startup
rounds.

Idle numbers depend on focus: Kettle, Ghostty, and kitty blink the cursor only
in a focused window. Idle and flood windows are brought to the front by pid,
and an idle round counts only if its window was frontmost when settling began,
midway through sampling, and at the end.

Every run starts with a preflight that refuses noisy conditions (battery, Low
Power Mode, a locked screen, load, Time Machine, a build running, a measured
terminal already open). --host-pid names the one measured terminal allowed to
stay open: the one hosting the shell that runs this script. A session that
passes is countable; --combine merges countable sessions into published labels.

Results go to --out-dir as results.json, rewritten after every row, and
summary.md.
"""

import argparse
import base64
import contextlib
import datetime
import hashlib
import importlib.util
import json
import math
import os
import plistlib
import random
import re
import select
import resource
import shlex
import shutil
import signal
import stat
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import traceback
from pathlib import Path
from typing import Callable, Dict, Iterable, List, Optional, Sequence, Tuple

REPO = Path(__file__).resolve().parents[2]
PROBES = Path(__file__).resolve().parent / "macos-standing"
_hc_spec = importlib.util.spec_from_file_location("standing_output_blink", PROBES / "output_blink.py")
hc = importlib.util.module_from_spec(_hc_spec)
_hc_spec.loader.exec_module(hc)
_cursor_spec = importlib.util.spec_from_file_location("standing_cursor", PROBES / "cursor_latency.py")
cursor = importlib.util.module_from_spec(_cursor_spec)
_cursor_spec.loader.exec_module(cursor)
_publication_spec = importlib.util.spec_from_file_location("standing_publication", PROBES / "publication.py")
publication = importlib.util.module_from_spec(_publication_spec)
_publication_spec.loader.exec_module(publication)
DEFAULT_KETTLE = REPO / "target" / "release" / "kettle"
INSTALLED_KETTLE = "/Applications/kettle.app/Contents/MacOS/kettle"
VTEBENCH_URL = "https://github.com/alacritty/vtebench"
VTEBENCH_REV = "ead80032e57dee2e75f0b51f2ea67528647d9944"
COLS, ROWS = 120, 36
FLOOD_BYTES = 32 * 1024 * 1024
SCHEMA = 3
EVIDENCE_CONTRACT = "hc-v1"

APPS = {
    "alacritty": "/Applications/Alacritty.app/Contents/MacOS/alacritty",
    "kitty": "/Applications/kitty.app/Contents/MacOS/kitty",
    "wezterm": "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
    "ghostty": "/Applications/Ghostty.app/Contents/MacOS/ghostty",
}
WORKLOADS = ("startup", "idle", "flood-memory", "vtebench")
# Never in the default list: latency posts key presses, needs the probe's
# Screen Recording and Accessibility grants, and needs the machine to itself.
OPT_IN_WORKLOADS = ("latency", "latency-cursor", "output-memory", "blink-window")
# Publication defaults. Counts are multiples of five so a five-terminal
# rotation is balanced.
ROUNDS = {"startup": 30, "idle": 5, "flood-memory": 5, "vtebench": 5, "latency": 10}
# The metrics each workload reports; a round's other numbers (exit_ms, the
# resident size) are kept in results.json but never compared or published.
METRICS = {"startup": ("window_ms", "child_ms"), "idle": ("cpu_percent", "wakeups_per_second", "footprint_mib")}
DEFAULT_FLOOD_OFFSETS = (3.0, 20.0)
# The values a round must carry to count toward a complete session.
REQUIRED = {"startup": ("window_ms", "child_ms"), "idle": ("cpu_percent", "wakeups_per_second", "footprint_mib")}
# A row's comparison in a session counts only with this share of its rounds
# paired (an idle round that lost focus has no pair).
MIN_PAIRED_SHARE = 0.8
# Settings every session in a combined set must share; any change starts a
# new set.
SESSION_KEYS = ("harness_tree", "tool_hashes", "hw_model", "macos_build", "display", "fd_limit", "rounds", "warmup",
                "vtebench_seconds", "idle_settle", "idle_window", "flood_offsets", "activate", "configs",
                "footprint_detail", "startup_phases", "latency", "config_closures", "output_blink", "latency-cursor")
SESSION_KEYS += ("contracts", "tool_artifacts", "harness_dirty", "kind")
# Reported once per terminal rather than as metrics.
GRID_KEYS = ("cols", "rows")

# Latency: the probe's bundle id, which its TCC grants are keyed to with its
# signature; the floors (bare windows, reported and never ranked); the keys
# the probe's calibration posts before the measured ones (the payload's
# sequence numbers count them too); the payload's 32-byte log record.
LATENCY_PROBE_ID = "org.kettle.terminal.latency-probe"
LATENCY_FLOORS = ("ca", "metal-sync", "metal-nosync", "metal-sync-2")
LATENCY_CALIBRATION_KEYS = 6
KEYBLOCK_RECORD = struct.Struct("<4Q")
# A latency row with more of its keys censored than this, or with more of
# them outside the display-after-arrival window, is left unranked.
LATENCY_CENSOR_SHARE = 0.01
# ScreenCaptureKit hands a frame over before it is displayed; a key whose
# display time is more than this many refresh periods after the frame's
# arrival, or before it, came from a frame the probe cannot trust.
LATENCY_LEAD_PERIODS = 2
# An entry that loses this share of its latency rounds (focus changed, a
# window covered the block) is not measured in that session; the others still
# are, and the session's other workloads still count.
LATENCY_NOT_MEASURED_SHARE = 0.3
# Kettle's default is ranked; this variant is published beside it, unranked.
KETTLE_OPAQUE = "background-opacity = 1\nwindow-blur = false"

# Two-sided level of every interval. Rows tested together (vtebench's
# benchmarks in an A/A) also get a Bonferroni interval at 1 - (1 - LEVEL) / k.
LEVEL = 0.95
SEED = 7
CLAIM_SESSIONS = 3
CLAIM_WIN_SHARE = 0.8
LOAD_LIMIT = 2.0
# Build and review tools that make a session noisy while they work. They are
# matched by executable name only, and refuse a session only above this CPU
# share: a compiler runs far hotter, while an interactive session left open
# idles at a few percent. Every one found is recorded either way.
BUSY_TOOLS = ("cargo", "rustc", "clang", "swiftc", "swift-frontend", "ld", "xcodebuild", "codex", "claude")
BUSY_CPU_PERCENT = 10.0


def build_probes(tools: Path, latency: bool = False, sign_identity: Optional[str] = None,
                 rebuild_latency: bool = False) -> Dict[str, Path]:
    """Compile the probes once into `tools`; rebuild when a source is newer.
    With `latency`, also the keyblock payload, the floor, and the signed
    KettleLatencyProbe.app."""
    tools.mkdir(parents=True, exist_ok=True)
    built = {}
    helpers = [("stamp", "clang"), ("memsample", "clang"), ("launch", "swiftc")]
    if latency:
        helpers += [("keyblock", "clang"), ("latency-floor", "swiftc")]
    for name, compiler in helpers:
        source = PROBES / (f"{name}.swift" if compiler == "swiftc" else f"{name}.c")
        binary = tools / name
        if not binary.exists() or binary.stat().st_mtime < source.stat().st_mtime:
            command = [compiler, "-O", "-o", str(binary), str(source)]
            subprocess.run(command, check=True)
        built[name] = binary
    if latency:
        built["latency-probe"] = build_latency_probe(tools, sign_identity, rebuild_latency)
    return built


def latency_probe_plist() -> bytes:
    """Info.plist of KettleLatencyProbe.app: an agent app (no Dock icon, never
    frontmost) whose bundle id carries its TCC grants."""
    return plistlib.dumps({
        "CFBundleIdentifier": LATENCY_PROBE_ID,
        "CFBundleName": "KettleLatencyProbe",
        "CFBundleExecutable": "latency-probe",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "1",
        "CFBundleVersion": "1",
        "LSMinimumSystemVersion": "14.0",
        "LSUIElement": True,
    })


# The receipt is a local trust record for harness-owned builds. It protects
# against accidental swaps, not a user able to replace both it and this script.
_PROBE_LOCKS: dict = {}


def probe_refusal(reason: str) -> RuntimeError:
    return RuntimeError("latency probe: " + reason + "; prepare with --latency-check --rebuild-latency-probe")


@contextlib.contextmanager
def probe_lock(tools: Path):
    import fcntl

    key = str(tools.resolve())
    held = _PROBE_LOCKS.get(key)
    if held is not None:
        yield held
        return
    try:
        tools.mkdir(parents=True, exist_ok=True)
        flags = os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW
        fd = os.open(tools / "KettleLatencyProbe.lock", flags, 0o600)
    except OSError:
        raise probe_refusal("lock file unavailable") from None
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o077:
            raise probe_refusal("lock is not a private regular file")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise probe_refusal("preparation or use is already locked") from None
        held = {"digest": None, "identities": None}
        _PROBE_LOCKS[key] = held
        try:
            yield held
        finally:
            del _PROBE_LOCKS[key]
    finally:
        os.close(fd)


def probe_command(argv: List[str], timeout: float = 60) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(argv, check=True, capture_output=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError):
        # Compiler/signature diagnostics can contain private paths or identities.
        raise probe_refusal("build or verification command failed") from None


def probe_build_contract(identity: Optional[str]) -> dict:
    compiler = shutil.which("swiftc")
    if not compiler:
        raise probe_refusal("compiler unavailable")
    return {"source_sha256": file_sha256(PROBES / "latency-probe.swift"),
            "plist_sha256": hashlib.sha256(latency_probe_plist()).hexdigest(),
            "command": ["<compiler>", "-sdk", "<sdk>", "-O", "-o", "<executable>", "<source>"],
            "compiler_path": str(Path(compiler).resolve()),
            "compiler_version": probe_command(["swiftc", "--version"]).stdout.decode().strip(),
            "sdk_path": probe_command(["xcrun", "--sdk", "macosx", "--show-sdk-path"]).stdout.decode().strip(),
            "sdk_version": probe_command(["xcrun", "--sdk", "macosx", "--show-sdk-version"]).stdout.decode().strip(),
            "identity": identity or "-"}


def probe_file_identity(info: os.stat_result) -> tuple:
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def probe_bundle_snapshot(app: Path) -> dict:
    """Hash paths, modes and bytes without following links or opening devices.
    Recheck the complete tree and every inode after hashing."""
    def inventory() -> dict:
        entries = {}
        def visit(path: Path):
            info = path.lstat()
            if not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
                raise probe_refusal("bundle contains a link or special file")
            entries[path.relative_to(app).as_posix()] = probe_file_identity(info)
            if stat.S_ISDIR(info.st_mode):
                for child in sorted(path.iterdir()):
                    visit(child)
        visit(app)
        return entries

    try:
        before = inventory()
        digest = hashlib.sha256()
        files = {}
        for relative, info in sorted(before.items()):
            mode = stat.S_IMODE(info[2])
            digest.update(json.dumps([relative, mode], separators=(",", ":")).encode() + b"\0")
            if stat.S_ISREG(info[2]):
                fd = os.open(app / relative, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                with os.fdopen(fd, "rb") as stream:
                    if probe_file_identity(os.fstat(stream.fileno())) != info:
                        raise probe_refusal("bundle changed while hashing")
                    data = stream.read()
                    if probe_file_identity(os.fstat(stream.fileno())) != info:
                        raise probe_refusal("bundle changed while hashing")
                files[relative] = hashlib.sha256(data).hexdigest()
                digest.update(len(data).to_bytes(8, "big") + data)
        if inventory() != before:
            raise probe_refusal("bundle changed while hashing")
        return {"bundle_sha256": digest.hexdigest(), "files": files,
                "tree": {p: stat.S_IMODE(i[2]) for p, i in before.items()}, "identities": before}
    except OSError:
        raise probe_refusal("bundle missing or unreadable") from None


def probe_signature(app: Path) -> dict:
    probe_command(["codesign", "--verify", "--strict", "--deep", str(app)])
    details = probe_command(["codesign", "-d", "--verbose=4", str(app)]).stderr.decode()
    requirement_output = probe_command(["codesign", "-d", "-r-", str(app)])
    requirement_text = (requirement_output.stdout + requirement_output.stderr).decode()
    requirement = re.search(r"^(?:# )?designated => (.+)$", requirement_text, re.M)
    identifier = re.search(r"^Identifier=(.+)$", details, re.M)
    cdhash = re.search(r"^CDHash=([0-9a-fA-F]+)$", details, re.M)
    if not requirement or not identifier or identifier[1] != LATENCY_PROBE_ID or not cdhash:
        raise probe_refusal("signature metadata is incomplete or unexpected")
    entitlements = probe_command(["codesign", "-d", "--entitlements", "-", str(app)]).stdout
    try:
        entitlement_set = plistlib.loads(entitlements) if entitlements.strip() else {}
    except (ValueError, plistlib.InvalidFileException):
        raise probe_refusal("entitlements are malformed") from None
    if entitlement_set != {}:
        raise probe_refusal("unexpected entitlements")
    probe_command(["codesign", "--verify", "--strict", "--deep", "-R", "=" + requirement[1], str(app)])
    return {"identifier": identifier[1], "cdhash": cdhash[1].lower(),
            "mode": "ad hoc" if "Signature=adhoc" in details else "certificate",
            "requirement": requirement[1],
            "authorities": re.findall(r"^Authority=(.+)$", details, re.M),
            "team": re.findall(r"^TeamIdentifier=(.+)$", details, re.M), "entitlements": entitlement_set}


def probe_artifact(app: Path) -> dict:
    before = probe_bundle_snapshot(app)
    # Compare the bytes the snapshot read through a checked descriptor;
    # opening the path again could block on a substituted special file.
    plist = before["files"].get("Contents/Info.plist")
    if plist is None:
        raise probe_refusal("Info.plist missing")
    if plist != hashlib.sha256(latency_probe_plist()).hexdigest():
        raise probe_refusal("Info.plist differs from the build contract")
    executable = "Contents/MacOS/latency-probe"
    if executable not in before["files"] or not before["tree"][executable] & 0o111:
        raise probe_refusal("executable missing or not executable")
    signature = probe_signature(app)
    after = probe_bundle_snapshot(app)
    if before != after:
        raise probe_refusal("bundle changed during verification")
    return {"bundle_sha256": before["bundle_sha256"], "executable_sha256": before["files"][executable],
            "files": before["files"], "tree": before["tree"], "signature": signature}


def probe_receipt(record: Path) -> Tuple[bytes, tuple]:
    try:
        fd = os.open(record, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o077 or info.st_size > 1024 * 1024:
                raise probe_refusal("receipt is not a bounded private regular file")
            raw = stream.read()
            if probe_file_identity(info) != probe_file_identity(os.fstat(stream.fileno())):
                raise probe_refusal("receipt changed while reading")
        return raw, probe_file_identity(info)
    except OSError:
        raise probe_refusal("receipt missing or unreadable") from None


def validate_latency_probe(app: Path, identity: Optional[str] = None) -> dict:
    record = app.parent / "KettleLatencyProbe.build.json"
    with probe_lock(app.parent) as held:
        try:
            raw, receipt_identity = probe_receipt(record)
            receipt = json.loads(raw)
            if not isinstance(receipt, dict) or receipt.get("version") != 2:
                raise probe_refusal("cache has no version 2 build receipt")
            if receipt.get("contract") != probe_build_contract(identity):
                raise probe_refusal("build contract changed")
            before = probe_bundle_snapshot(app)
            artifact = probe_artifact(app)
            if receipt.get("artifact") != artifact:
                raise probe_refusal("artifact differs from the prepared build")
            if (raw, receipt_identity) != probe_receipt(record) or receipt_identity != probe_file_identity(record.lstat()):
                raise probe_refusal("receipt changed during verification")
            digest = artifact["bundle_sha256"]
            if held["digest"] is not None and held["digest"] != digest:
                raise probe_refusal("artifact changed during use")
            after = probe_bundle_snapshot(app)
            if before != after or after["bundle_sha256"] != digest:
                raise probe_refusal("bundle changed while validating the receipt")
            identities = after["identities"]
            if held["identities"] is not None and held["identities"] != identities:
                raise probe_refusal("bundle replaced during use")
            held["identities"] = identities
            held["digest"] = digest
            return {"source_sha256": receipt["contract"]["source_sha256"],
                    "bundle_sha256": digest, "executable_sha256": artifact["executable_sha256"],
                    **{key: artifact["signature"][key] for key in ("identifier", "cdhash", "mode")}}
        except (OSError, ValueError, TypeError, KeyError):
            raise probe_refusal("receipt missing or malformed") from None


def build_latency_probe(tools: Path, identity: Optional[str], rebuild: bool = False) -> Path:
    try:
        return prepare_latency_probe(tools, identity, rebuild)
    except OSError:
        raise probe_refusal("preparation or publication failed") from None


def prepare_latency_probe(tools: Path, identity: Optional[str], rebuild: bool) -> Path:
    app = tools / "KettleLatencyProbe.app"
    with probe_lock(tools) as held:
        if not rebuild:
            validate_latency_probe(app, identity)
            return app
        if held["digest"] is not None:
            raise probe_refusal("cannot rebuild an artifact in use")
        contract = probe_build_contract(identity)
        with tempfile.TemporaryDirectory(prefix=".latency-build-", dir=tools) as tmp:
            staging = Path(tmp) / app.name
            executable = staging / "Contents" / "MacOS" / "latency-probe"
            executable.parent.mkdir(parents=True)
            source = Path(tmp) / "latency-probe.swift"
            source.write_bytes((PROBES / "latency-probe.swift").read_bytes())
            if file_sha256(source) != contract["source_sha256"]:
                raise probe_refusal("source changed during preparation")
            probe_command([contract["compiler_path"], "-sdk", contract["sdk_path"], "-O", "-o", str(executable), str(source)])
            (staging / "Contents" / "Info.plist").write_bytes(latency_probe_plist())
            probe_command(["codesign", "--force", "--sign", identity or "-", "--identifier", LATENCY_PROBE_ID, str(staging)])
            artifact = probe_artifact(staging)
            probe_command([str(executable), "--self-test"])
            if probe_artifact(staging) != artifact or probe_build_contract(identity) != contract:
                raise probe_refusal("build changed during preparation")
            if artifact["signature"]["mode"] != ("certificate" if identity not in (None, "-") else "ad hoc"):
                raise probe_refusal("unexpected signature mode")
            receipt = Path(tmp) / "receipt.json"
            receipt.write_text(json.dumps({"version": 2, "contract": contract, "artifact": artifact,
                                           "local": {"app": str(app.resolve()), "source": str((PROBES / "latency-probe.swift").resolve())}},
                                          sort_keys=True))
            receipt.chmod(0o600)
            # No portable atomic rename covers two paths. Invalidate the old
            # receipt first and publish the new one last under the lock. A crash
            # in between leaves a cache that all readers refuse.
            record = tools / "KettleLatencyProbe.build.json"
            record.unlink(missing_ok=True)
            if app.exists() or app.is_symlink():
                os.replace(app, Path(tmp) / "retired.app")
            os.replace(staging, app)
            os.replace(receipt, record)
        validate_latency_probe(app, identity)
        return app


@contextlib.contextmanager
def verified_probe_use(app: Path):
    with probe_lock(app.parent):
        # The identity is private in the receipt. It is never inferred from
        # the cached signature to promote an unverified build.
        try:
            record = app.parent / "KettleLatencyProbe.build.json"
            receipt = json.loads(probe_receipt(record)[0])
            identity = receipt["contract"]["identity"]
        except (OSError, ValueError, KeyError, TypeError):
            raise probe_refusal("receipt missing or malformed") from None
        public = validate_latency_probe(app, None if identity == "-" else identity)
        try:
            yield public
        finally:
            validate_latency_probe(app, None if identity == "-" else identity)


def probe_tool_identity(probes: Dict[str, Path], identity: Optional[str]) -> Tuple[dict, dict]:
    hashes = {name: file_sha256(path) for name, path in probes.items() if name != "latency-probe"}
    artifacts = {}
    if "latency-probe" in probes:
        artifact = validate_latency_probe(probes["latency-probe"], identity)
        hashes["latency-probe"] = artifact["bundle_sha256"]
        artifacts["latency-probe"] = artifact
    return hashes, artifacts


def latency_probe_self_test(app: Path) -> int:
    with verified_probe_use(app):
        return probe_command([str(app / "Contents" / "MacOS" / "latency-probe"), "--self-test"]).returncode


def wait_for_text(path: Path, marker: str, timeout: float) -> bool:
    """Whether `marker` appears in `path` within `timeout` seconds."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            if marker in path.read_text(errors="replace"):
                return True
        except OSError:
            pass
        if time.monotonic() > deadline:
            return False
        time.sleep(0.1)


@contextlib.contextmanager
def probe_invocation_lease(work: Path):
    import fcntl

    fd, name = tempfile.mkstemp(prefix=".probe-use-", dir=work)
    path = Path(name)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield path
    finally:
        path.unlink(missing_ok=True)
        os.close(fd)


def run_owned_probe_open(command: List[str], lease: Path, timeout: float) -> None:
    """Own/reap only the open child. The probe watches the locked lease and
    stops posting/exits when cancellation removes it or its owner dies."""
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        process.wait(timeout=timeout)
    except BaseException:
        lease.unlink(missing_ok=True)
        reap_owned_child(process, 10)
        raise


def reap_owned_child(process: subprocess.Popen, grace: float) -> None:
    """Wait up to `grace` seconds for an owned child, then kill and reap it.
    A repeated cancellation (a second Ctrl-C) during the wait skips the rest
    of the grace period but never abandons the child; the caller re-raises
    the original exception once the child is reaped."""
    deadline = time.monotonic() + grace
    while True:
        try:
            remaining = deadline - time.monotonic()
            if remaining > 0:
                process.wait(timeout=remaining)
            else:
                process.kill()
                process.wait()
            return
        except BaseException:
            deadline = 0.0


def run_latency_probe(app: Path, args: List[str], work: Path, timeout: float) -> str:
    """Run the probe as its own app through `open`, so macOS holds the probe,
    not whatever launched this script, responsible for its grants, and it
    never becomes frontmost. Returns its stdout. `open -W` drops the exit
    status, and returns at once when the probe exits before `open` can
    attach to it, so callers wait for the probe's output instead."""
    with verified_probe_use(app):
        stdout, stderr = work / "probe.stdout", work / "probe.stderr"
        for path in (stdout, stderr):
            path.write_text("")
        started = time.monotonic()
        with probe_invocation_lease(work) as lease:
            run_owned_probe_open(["open", "-g", "-n", "-W", "--stdout", str(stdout), "--stderr", str(stderr), str(app),
                                  "--args", *args, "--lease-file", str(lease)], lease, timeout)
            # open may attach after the app already exited. Verify after its final
            # output, never while a measurement is still in flight.
            if "--out" in args:
                finished = wait_for_text(Path(args[args.index("--out") + 1]), "}", max(0.0, timeout - (time.monotonic() - started)))
            else:
                finished = wait_for_text(stdout, "post events:", 10)
            if not finished:
                raise probe_refusal("invocation did not finish")
            return stdout.read_text(errors="replace")


def latency_grants(app: Path, work: Path, request: bool = False) -> Dict[str, bool]:
    """Whether the probe holds Screen Recording and event posting. Asking
    (`request`) shows macOS's prompts; only --latency-check does that."""
    run_latency_probe(app, ["--request" if request else "--check"], work, 120)
    # Both verdicts print on one line, last.
    wait_for_text(work / "probe.stdout", "post events:", 10)
    out = (work / "probe.stdout").read_text(errors="replace")
    return {"screen_recording": "screen recording: granted" in out, "post_events": "post events: granted" in out}


FLOOD_WORDS = (
    "fn", "let", "mut", "self", "impl", "pub", "match", "Some(value)", "None", "Ok(())",
    "return", "&str", "Vec<u8>", "0x7f", "=>", "{", "}", "(", ");", "// note:",
    "error:", "warning:", "src/main.rs:42:7", "--flag=value", "\"quoted\"", "42",
)


def write_flood(path: Path, size: int = FLOOD_BYTES) -> None:
    """Write `size` bytes of plain text that is the same on every run.

    Lines stay under the 120-column grid so no terminal has to wrap them. The
    text is seeded, not read from the checkout, so every release's flood-memory
    run prints the same bytes.
    """
    # Only `random()` is guaranteed to give the same sequence for a seed on
    # every Python version; `choice` and `randrange` are not, so every pick
    # is derived from it.
    rng = random.Random(4096)

    def pick(n: int) -> int:
        return int(rng.random() * n)

    lines = []
    length = 0
    while length < 1 << 20:
        words: List[str] = []
        width = pick(118)
        while sum(len(word) + 1 for word in words) < width:
            words.append(FLOOD_WORDS[pick(len(FLOOD_WORDS))])
        line = " ".join(words)[:119] + "\n"
        lines.append(line)
        length += len(line)
    block = "".join(lines).encode("ascii")
    whole, rest = divmod(size, len(block))
    with path.open("wb") as out:
        for _ in range(whole):
            out.write(block)
        out.write(block[:rest])


def checkout_vtebench(checkout: Path, url: str = VTEBENCH_URL, rev: str = VTEBENCH_REV) -> bool:
    """Put `checkout` at `rev`, cloning if needed. True if it moved.

    A pin that names no real commit must fail here with its value, not as a
    bare `git checkout` error halfway through a long run.
    """
    if not checkout.exists():
        subprocess.run(["git", "clone", "--quiet", url, str(checkout)], check=True)

    def head() -> str:
        return subprocess.run(
            ["git", "-C", str(checkout), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()

    dirty = subprocess.run(
        ["git", "-C", str(checkout), "status", "--porcelain"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    if dirty:
        # A local edit would be built and benchmarked as if it were the pin.
        raise SystemExit(
            f"vtebench checkout {checkout} has local changes; commit, stash, or delete it:\n{dirty}"
        )
    if head() == rev:
        return False
    subprocess.run(["git", "-C", str(checkout), "fetch", "--quiet", "origin"], check=True)
    moved = subprocess.run(
        ["git", "-C", str(checkout), "checkout", "--quiet", rev], capture_output=True, text=True,
    )
    if moved.returncode != 0 or head() != rev:
        raise SystemExit(f"vtebench pin {rev} is not a commit in {url}: {moved.stderr.strip()}")
    return True


# vtebench records each sample as whole milliseconds, so two terminals 7 %
# apart can report the same median (4.7.0's cursor_motion). The copy it is
# built from records microseconds instead.
VTEBENCH_MICROS_FIX = (
    "samples.push(duration.as_millis() as usize);",
    "samples.push(duration.as_micros() as usize);",
)


def apply_micros_fix(bench_rs: Path) -> None:
    """Patch vtebench's sample line, which must appear exactly once."""
    old, new = VTEBENCH_MICROS_FIX
    text = bench_rs.read_text()
    found = text.count(old)
    if found != 1:
        raise SystemExit(f"{bench_rs}: expected `{old}` exactly once, found {found}")
    bench_rs.write_text(text.replace(old, new))


def tree_digest(root: Path) -> str:
    """sha256 over every source file's path and bytes, skipping build output."""
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts[0] in ("target", ".git") or path.name == ".kettle-source" or not path.is_file():
            continue
        digest.update(str(relative).encode() + b"\0" + path.read_bytes() + b"\0")
    return digest.hexdigest()


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as data:
        for block in iter(lambda: data.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def reset_copy(root: Path) -> None:
    """Empty the harness's own copy of vtebench, keeping its cargo target
    directory. A symlink or a file where the copy should be is refused, so
    cleaning never reaches anything the harness does not own."""
    if root.is_symlink() or (root.exists() and not root.is_dir()):
        raise SystemExit(f"{root} is not the harness's own directory; remove it and rerun")
    root.mkdir(exist_ok=True)
    for entry in root.iterdir():
        if entry.name == "target" and entry.is_dir() and not entry.is_symlink():
            # The build cache is kept only while it stays inside the copy.
            release = entry / "release"
            if release.is_symlink():
                release.unlink()
            continue
        if entry.is_dir() and not entry.is_symlink():
            shutil.rmtree(entry)
        else:
            entry.unlink()


def build_vtebench(tools: Path) -> Path:
    """Clone vtebench at the pinned revision and build its microsecond copy.

    The clean checkout stays untouched; `vtebench-us` beside it is rebuilt from
    it whenever the pin changes. Both sit inside Kettle's workspace root, so
    the root `Cargo.toml` lists them under `workspace.exclude`, and the
    self-test keeps the two in step.
    """
    checkout = tools / "vtebench"
    micros = tools / "vtebench-us"
    binary = micros / "target" / "release" / "vtebench"
    # The pin, the patched source's digest and the binary's hash from the last
    # build; any local edit to the copy or its binary forces a rebuild.
    marker = micros / ".kettle-source"
    moved = checkout_vtebench(checkout)
    try:
        recorded = json.loads(marker.read_text())
    except (OSError, json.JSONDecodeError):
        recorded = {}
    current = (recorded.get("rev") == VTEBENCH_REV and binary.exists()
               and recorded.get("source") == tree_digest(micros) and recorded.get("binary") == file_sha256(binary))
    if moved or not current:
        # Keep the cargo target directory so a pin bump rebuilds incrementally.
        reset_copy(micros)
        shutil.copytree(checkout, micros, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".git", "target"))
        apply_micros_fix(micros / "src" / "bench.rs")
        # Cargo decides freshness from its own records, so a binary replaced
        # by hand could survive a build; removing it forces the link.
        binary.unlink(missing_ok=True)
        subprocess.run(["cargo", "build", "--release", "--locked", "--quiet"], cwd=micros, check=True)
        marker.write_text(json.dumps({"rev": VTEBENCH_REV, "source": tree_digest(micros),
                                      "binary": file_sha256(binary)}))
    return binary


# vtebench's scripts read the window size with
#     tty="/dev/$(ps -o tty= -p $$)"; columns=$(tput cols < $tty)
# On macOS that fails twice. `ps` pads the name ("ttys001 "), so the path names
# no file. `tput` reads the size from stdout, which is vtebench's capture pipe,
# so it would fall back to 80x24 anyway. With no size, `cursor_motion` and
# `light_cells` print nothing and vtebench drops them, `dense_cells` shrinks to
# 26 cursor-home escapes, and the region setups set no region. These are the
# replacements from upstream's unmerged fix,
# https://github.com/alacritty/vtebench/pull/46, applied to a copy.
VTEBENCH_SIZE_FIX = (
    ('tty="/dev/$(ps -o tty= -p $$)"', 'tty="/dev/$(ps -o tty= -p $$ | tr -d "[:space:]")"'),
    ("columns=$(tput cols < $tty)", 'columns=$(stty size < $tty | cut -d" " -f2)'),
    ("lines=$(tput lines < $tty)", 'lines=$(stty size < $tty | cut -d" " -f1)'),
    (
        'printf "\\e[?1049h\\e[2;$(tput lines)r"',
        'tty="/dev/$(ps -o tty= -p $$ | tr -d "[:space:]")"\n'
        'lines=$(stty size < $tty | cut -d" " -f1)\n\n'
        'printf "\\e[?1049h\\e[2;${lines}r"',
    ),
)


def prepare_benchmarks(source: Path, dest: Path) -> Path:
    """Copy vtebench's benchmarks into `dest` with the macOS size fix applied.

    Symlinked scripts are copied as files so each can be patched on its own.
    Fails if a script still reads the size another way, so a new pin cannot
    bring the bug back unnoticed.
    """
    shutil.copytree(source, dest, symlinks=False)
    for script in sorted([*dest.glob("*/setup"), *dest.glob("*/benchmark")]):
        text = script.read_text()
        for old, new in VTEBENCH_SIZE_FIX:
            text = text.replace(old, new)
        if "tput" in text or "ps -o tty= -p $$)" in text:
            raise SystemExit(f"{script}: reads the window size in a way the macOS fix does not cover")
        script.write_text(text)
    return dest


def missing_benchmarks(benchmarks: Path, dat: Dict[str, float]) -> List[str]:
    """Benchmarks with no samples. vtebench drops a script that prints nothing."""
    return sorted(d.name for d in benchmarks.iterdir() if (d / "benchmark").exists() and d.name not in dat)


def shutdown_kind(ended: Optional[dict], clean: bool) -> str:
    """How a launch ended, from its helper's record: "stopped" when the stop
    ended it, "exited" when the terminal quit before the stop, "killed", or
    "unknown" when the record cannot say."""
    if not clean:
        return "killed"
    if (not isinstance(ended, dict) or type(ended.get("stopped")) is not bool
            or type(ended.get("killed")) is not bool):
        return "unknown"
    if ended["killed"]:
        return "killed"
    if ended["stopped"]:
        return "stopped"
    return "exited" if ended.get("exit_ms") is not None else "unknown"


def terminal_argv(name: str, script: Path, work: Path, kettle: Dict[str, str]) -> List[str]:
    """How each terminal runs `script` at the pinned grid with default settings."""
    if name in kettle:
        # `-e` hands what follows the script to it, which ignores it, and
        # AppKit reads `-Key value` pairs from the whole argv. Before 4.8.0
        # Kettle kept AppKit's persistent UI on: every round stopped here
        # counted as a crash while reopening windows, and AppKit then held the
        # next launch at a modal "reopen windows?" alert, so its payload never
        # ran. Ignoring the saved state skips only that restore; 4.7.0 still
        # idles with its persistence on, and later builds turn it off.
        return [kettle[name], "--config", str(work / f"{name}.config"), "-e", str(script),
                "-ApplePersistenceIgnoreState", "YES"]
    if name == "alacritty":
        return [APPS[name], "--config-file", "/dev/null",
                "-o", f"window.dimensions.columns={COLS}", "-o", f"window.dimensions.lines={ROWS}",
                "-e", str(script)]
    if name == "kitty":
        return [APPS[name], "--single-instance=no", "--config", "NONE",
                "-o", "remember_window_size=no", "-o", "macos_quit_when_last_window_closed=yes",
                "-o", f"initial_window_width={COLS}c", "-o", f"initial_window_height={ROWS}c",
                str(script)]
    if name == "wezterm":
        return [APPS[name], "-n", "--config", f"initial_cols={COLS}", "--config", f"initial_rows={ROWS}",
                "start", "--always-new-process", "--", str(script)]
    if name == "ghostty":
        # The macOS app rejects configuration flags on its command line and an
        # `-e` with more than one argument, so an isolated XDG config carries
        # the grid and bypasses the user's own config.
        return ["/usr/bin/env", f"XDG_CONFIG_HOME={work / 'xdg'}", APPS[name], "-e", str(script)]
    raise ValueError(name)


def write_configs(work: Path, kettle_configs: Optional[Dict[str, str]] = None) -> None:
    """One config file per Kettle entry: the shared base plus that entry's
    extra lines (an A/B's B side, or an unranked variant)."""
    base = ("agent-server = off\nrestore-session = false\nupdate-check = false\n"
            f"window-width = {COLS}\nwindow-height = {ROWS}\n")
    for name, extra in (kettle_configs or {"kettle": ""}).items():
        (work / f"{name}.config").write_text(base + (extra.strip() + "\n" if extra.strip() else ""))
    ghostty = work / "xdg" / "ghostty"
    ghostty.mkdir(parents=True, exist_ok=True)
    (ghostty / "config").write_text(
        f"window-width = {COLS}\nwindow-height = {ROWS}\nquit-after-last-window-closed = true\n"
    )


# Config closure follows kettle-config parse_collect/parse::parse and
# kettle-render::bg_image at ac0c4fe3. Kettle has no include syntax. Themes
# and font families are names, not files. record-dir is an output directory.
CONFIG_RESOLVER = "kettle-declarative-v1"
CONFIG_MAX_BYTES = 1024 * 1024
# Kettle creates its remote-command spool and that spool's lock beside its
# config at startup (kettle-ui app.rs remote watcher, kettle-state
# remote_command_lock_path). They are runtime state, not configuration. Empty
# regular files queue no command; anything else could feed one into the pane.
KETTLE_RUNTIME_FILES = frozenset({"remote.cmd", "remote.cmd.lock"})
# Peers whose own config the launch bypasses (--config NONE, -n,
# --config-file /dev/null) can still create their config directory under the
# managed XDG root: kitty does on every launch. An empty directory feeds them
# nothing; any entry inside it refuses.
PEER_RUNTIME_DIRS = frozenset({"kitty", "wezterm", "alacritty"})
ASSET_MAX_BYTES = 64 * 1024 * 1024
CONFIG_FILE_KEYS = {"background-image": "asset", "record-dir": "output-directory"}
# Canonical top-level parse_collect arms. Reject new nonempty keys until their
# dependencies have been audited; a suffix/path-string heuristic misses inputs.
CONFIG_KEYS = frozenset("""accent-color agent-badge agent-display agent-display-claude-code agent-display-codex agent-server allow-bold always-on-top always-split-with-profile
ask-before-closing audible-bell autoclean-groups background background-animation background-blur
background-color background-darkness background-image background-image-align-horiz
background-image-align-vert background-image-mode background-opacity background-type
backspace-binding bell bell-flash-intensity bold-is-bright borderless broadcast-default
case-sensitive cell-height cell-width check-for-updates chrome-background clear-select-on-copy
clipboard clipboard-paste-protection close-button-on-tab colorterm command command-notify-threshold
command-notify-threshold-ms completion-overlay copy-on-select copy-on-selection cursor-bg-color
cursor-blink cursor-blink-interval cursor-blink-timeout cursor-color cursor-color-default
cursor-fg-color cursor-shape cursor-style cursor-style-blink custom-command custom-url-handler
dark-theme delete-binding detachable-tabs disable-mouse-paste disable-mousewheel-zoom
enabled-plugins env exit-action extra-styling focus focused-split-color font font-family
font-family-bold font-family-bold-italic font-family-italic font-feature font-size force-no-bell
foreground foreground-color full-screen geometry-hinting gpu-backend gpu-device-id
gpu-force-software gpu-name gpu-power-preference gpu-vendor-id handle-size hide-from-taskbar
hide-on-lose-focus http-proxy icon-bell inactive-bg-color-offset inactive-color-offset invert-search
keybind keybind-yield language light-theme link-single-click log-strip-ansi login-shell lua-sandbox
macos-cursor-blink-layer macos-option-as-alt menu-item minimum-contrast modify-other-keys mouse-autohide mouse-hide
mouse-hide-while-typing mouse-scroll-multiplier new-tab-after-current-tab osc52 padding-x padding-y
palette paste-files paste-image paste-image-preview paste-images paste-video-preview
preview-lane-side putty-paste-style putty-paste-style-source-clipboard record record-dir record-max-bytes
record-max-directory-bytes record-max-files record-raw-input resize-overlay restore-session
scroll-multiplier scroll-on-input scroll-on-keystroke scroll-on-output scroll-tabbar scrollback
scrollback-byte-limit scrollback-bytes scrollback-infinite scrollback-limit scrollback-lines
scrollback-memory scrollbar scrollbar-width search-background search-case-sensitive
search-foreground search-wrap selection-background selection-foreground selection-word-chars
semantic-escape-chars shell shell-integration show-titlebar smart-copy split-divider-color
split-divider-color-focused split-to-group ssh-host status-bar statusbar sticky tab-bar
tab-bar-position tab-bar-width tab-format tab-min-width tab-position tab-silence-threshold
tab-silence-threshold-ms tab-title-format term text-renderer theme theme-mode theme-schedule
theme-schedule-lat theme-schedule-lon theme-schedule-long theme-schedule-longitude title-at-bottom
title-font title-format title-hide-sizetext title-inactive-bg-color title-inactive-fg-color
title-receive-bg-color title-receive-fg-color title-transmit-bg-color title-transmit-fg-color
title-use-system-font trigger unfocused-split-opacity update-check update-check-interval-hours
update-policy urgent-bell use-custom-command use-custom-url-handler use-system-font use-theme-colors
vim-menu-nav visible-bell window-blur window-height window-padding-x window-padding-y
window-position-x window-position-y window-state window-title-format window-width word-delimiters""".split())
CONFIG_DYNAMIC_KEYS = frozenset(("env", "command", "shell", "custom-command", "trigger",
                                 "menu-item", "keybind", "keybind-yield", "enabled-plugins",
                                 "record-dir", "custom-url-handler"))


class ConfigClosureError(ValueError):
    """Public refusals contain fixed roles/reasons, never local paths or text."""


# Kettle's tokenizer (kettle-config parse.rs `parse`): `str::lines` ends a
# line only at "\n", `str::trim` strips Unicode White_Space, keys lowercase
# ASCII letters only, and a value is unquoted once after trimming. Python's
# splitlines, strip and lower differ on all four, so the closure mirrors Rust.
RUST_WHITESPACE = ("\t\n\v\f\r \x85\xa0\u1680" + "".join(chr(c) for c in range(0x2000, 0x200B))
                   + "\u2028\u2029\u202f\u205f\u3000")
ASCII_LOWER = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")


def config_lines(text: str) -> list:
    """The template's lines as Kettle splits them, each keeping its "\n" so a
    rewrite can replace one line in place."""
    pieces = text.split("\n")
    return [piece + "\n" for piece in pieces[:-1]] + ([pieces[-1]] if pieces[-1] else [])


def config_unquote(value: str) -> str:
    if len(value) >= 2 and value[0] in ("'", '"') and value[-1] == value[0]:
        return value[1:-1]
    return value


def config_entries(text: str) -> list:
    if len(text.encode()) > CONFIG_MAX_BYTES:
        raise ConfigClosureError("config closure: template exceeds size bound")
    entries = []
    # Countable managed configs are the section-free declarative subset.
    for index, raw in enumerate(config_lines(text.removeprefix("\ufeff"))):
        line = raw.strip(RUST_WHITESPACE)
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") or "=" not in line:
            raise ConfigClosureError("config closure: unsupported structure or dependency")
        key, value = line.split("=", 1)
        key = key.strip(RUST_WHITESPACE).translate(ASCII_LOWER).replace("_", "-")
        value = config_unquote(value.strip(RUST_WHITESPACE))
        if key in ("include", "config-file", "lua-script") or not key:
            raise ConfigClosureError("config closure: undeclared include or script")
        if key in ("restore-session", "always-split-with-profile") and value.strip(RUST_WHITESPACE).translate(
                ASCII_LOWER) in ("true", "on", "yes", "1", "enabled", "enable", "y"):
            raise ConfigClosureError("config closure: undeclared persistent input")
        if value.strip(RUST_WHITESPACE) and (key not in CONFIG_KEYS or key in CONFIG_DYNAMIC_KEYS):
            raise ConfigClosureError("config closure: unsupported nonempty reference or dynamic setting")
        # The value exactly as Kettle passes it on, untrimmed after unquoting.
        entries.append((index, key, value))
    return entries


def config_asset_path(value: str, cwd: Path, environ: dict) -> Path:
    # The app expands only a leading ~/. Environment expansion is unsupported.
    if "$" in value or "\x00" in value or "\n" in value or "\r" in value:
        raise ConfigClosureError("config closure: unresolved reference")
    if value.startswith("~/"):
        home = next((environ[k] for k in ("HOME", "USERPROFILE", "APPDATA") if environ.get(k)), None)
        if home is None:
            raise ConfigClosureError("config closure: unavailable home expansion")
        value = home.rstrip("/\\") + "/" + value[2:]
    path = Path(value)
    return path if path.is_absolute() else cwd / path


def config_file_bytes(path: Path, limit: int, *, follow: bool = False) -> tuple:
    """Bounded nonblocking regular-file read, bound to the opened inode.

    Leaf asset symlinks may resolve to a regular file. Resolve again afterward
    to reject retargeting. O_NOFOLLOW/O_NONBLOCK close the FIFO swap window.
    """
    try:
        resolved = path.resolve(strict=True) if follow else path
        before = resolved.lstat()
        if not stat.S_ISREG(before.st_mode) or not before.st_mode & 0o444 or before.st_size > limit:
            raise ConfigClosureError("config closure: asset is not a readable bounded regular file")
        fd = os.open(resolved, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            opened = os.fstat(stream.fileno())
            if (not stat.S_ISREG(opened.st_mode) or opened.st_size > limit
                    or (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
                raise ConfigClosureError("config closure: reference changed before read")
            data = stream.read(limit + 1)
            after = os.fstat(stream.fileno())
        def identity(info):
            return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns, info.st_mode)
        if (not stat.S_ISREG(opened.st_mode) or len(data) > limit or len(data) != opened.st_size
                or identity(before) != identity(opened) or identity(opened) != identity(after)
                or identity(after) != identity(resolved.lstat())
                or (follow and path.resolve(strict=True) != resolved)):
            raise ConfigClosureError("config closure: unstable asset read")
        return data, {"resolved": str(resolved), "dev": opened.st_dev, "ino": opened.st_ino,
                      "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    except (OSError, RuntimeError, ValueError) as error:
        if isinstance(error, ConfigClosureError):
            raise
        raise ConfigClosureError("config closure: reference cannot be captured") from None


def private_json(path: Path, value: dict) -> None:
    # Atomic and private from creation, including an existing legacy manifest.
    fd, temporary = tempfile.mkstemp(prefix=".manifest-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            stream.write(dumps(value))
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


class ConfigClosure:
    """Private frozen inputs and public, location-independent method identity.

    Generated peer layouts contain no includes or executable configs. Their
    exact files and config bypass arguments are part of this resolver contract.
    User-supplied peer files are not accepted by this harness.
    """
    def __init__(self, work: Path, configs: Dict[str, str], cwd: Path, environ: Optional[dict] = None, measured: Sequence[str] = ()):
        self.work, self.cwd = work.resolve(), cwd.resolve()
        if any(c in str(self.work) for c in ("\n", "\r", "\x00")):
            raise ConfigClosureError("config closure: unsupported private path")
        self.environ = dict(os.environ if environ is None else environ)
        self.public, self.local, self.sealed = {}, {}, {}
        self.sources = []
        self.ghostty_user_state = {}
        if "ghostty" in measured:
            home = self.environ.get("HOME")
            if not home:
                raise ConfigClosureError("config closure: Ghostty user config would apply")
            root = Path(home) / "Library/Application Support/com.mitchellh.ghostty"
            self.ghostty_user_state = {root / name: self._ghostty_state(root / name)
                                       for name in ("config", "config.ghostty")}
            self.local["ghostty_user_config"] = {str(path): state
                                                for path, state in self.ghostty_user_state.items()}
        self.work.chmod(0o700)
        # XDG_CONFIG_HOME also controls Kettle's automatic init.lua discovery.
        self.xdg = self.work / "xdg"
        (self.xdg / "kettle").mkdir(parents=True, exist_ok=True)
        self.assets = self.work / "assets"
        self.assets.mkdir(mode=0o700, exist_ok=True)
        for directory in (self.xdg, self.xdg / "kettle", self.xdg / "ghostty", self.assets):
            directory.chmod(0o700)
        for name in configs:
            path = self.work / f"{name}.config"
            template = config_file_bytes(path, CONFIG_MAX_BYTES)[0].decode("utf-8")
            entries = config_entries(template)
            effective = {key: (index, value) for index, key, value in entries}
            lines = config_lines(template)
            logical = list(lines)
            assets, mapping = [], []
            selected = effective.get("background-image")
            # kettle-config trims this key's value again after unquoting
            # (`cfg.background_image = e.value.trim()`), so quoted padding
            # never reaches bg_image.rs, which ignores an empty path.
            if selected and selected[1].strip(RUST_WHITESPACE):
                index, value = selected[0], selected[1].strip(RUST_WHITESPACE)
                source = config_asset_path(value, self.cwd, self.environ)
                data, identity = config_file_bytes(source, ASSET_MAX_BYTES, follow=True)
                digest = identity["sha256"]
                snapshot = self.assets / digest
                if not snapshot.exists():
                    with snapshot.open("xb") as stream:
                        stream.write(data)
                    snapshot.chmod(0o400)
                self._seal(snapshot, ASSET_MAX_BYTES, digest)
                # Replace only the effective assignment. Earlier overridden
                # references are not consumed and remain in the template hash.
                prefix = lines[index].split("=", 1)[0] + "= "
                lines[index] = prefix + str(snapshot) + "\n"
                logical[index] = prefix + "@asset/background-image" + "\n"
                assets.append({"role": "background-image", "size": len(data), "sha256": digest})
                mapping.append({"role": "background-image", "original": value, "source": str(source),
                                "snapshot": str(snapshot), **identity})
                self.sources.append((source, digest, name))
            # Without references, generated bytes are preserved exactly.
            rendered = "".join(lines)
            if rendered != template:
                path.write_text(rendered)
            path.chmod(0o400)
            self._seal(path, CONFIG_MAX_BYTES)
            normalized = "".join(logical)
            record = {"resolver": CONFIG_RESOLVER, "template_sha256": hashlib.sha256(normalized.encode()).hexdigest(),
                      "assets": assets}
            record["sha256"] = hashlib.sha256(dumps(record).encode()).hexdigest()
            self.public[name] = record
            self.local[name] = {"template": template, "generated": rendered, "mapping": mapping}
        ghostty = self.xdg / "ghostty" / "config"
        data = config_file_bytes(ghostty, CONFIG_MAX_BYTES)[0]
        expected = (f"window-width = {COLS}\nwindow-height = {ROWS}\nquit-after-last-window-closed = true\n").encode()
        if data != expected:
            raise ConfigClosureError("config closure: unsupported peer include or setting")
        ghostty.chmod(0o400)
        self._seal(ghostty, CONFIG_MAX_BYTES)
        peer_templates = {"ghostty": data.decode(), "alacritty": "--config-file /dev/null",
                          "kitty": "--config NONE", "wezterm": "-n"}
        for name, template in peer_templates.items():
            # Include every managed CLI override, with only generated locations
            # replaced by logical placeholders. Executable identity is separate.
            template += "\n" + dumps(terminal_argv(name, Path("@payload"), Path("@managed"), {}))
            record = {"resolver": CONFIG_RESOLVER, "template_sha256": hashlib.sha256(template.encode()).hexdigest(),
                      "assets": []}
            record["sha256"] = hashlib.sha256(dumps(record).encode()).hexdigest()
            self.public[name] = record
        self.local["launch"] = {"cwd": str(self.cwd), "xdg": str(self.xdg)}
        self.check()

    @staticmethod
    def _ghostty_state(path: Path) -> dict:
        reason = "config closure: Ghostty user config would apply"
        try:
            info = path.lstat()
        except FileNotFoundError:
            return {"present": False, "size": None, "sha256": None}
        except OSError:
            raise ConfigClosureError(reason) from None
        if not stat.S_ISREG(info.st_mode) or info.st_size != 0:
            raise ConfigClosureError(reason)
        try:
            data, _ = config_file_bytes(path, 0)
        except (OSError, ConfigClosureError):
            raise ConfigClosureError(reason) from None
        return {"present": True, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}

    def _seal(self, path: Path, limit: int, expected_digest: Optional[str] = None) -> None:
        data, _ = config_file_bytes(path, limit)
        digest = hashlib.sha256(data).hexdigest()
        if expected_digest is not None and digest != expected_digest:
            raise ConfigClosureError("config closure: captured asset changed")
        self.sealed[path] = (digest, limit, path.stat().st_mode & 0o777)

    def launch_environment(self, environ: dict) -> dict:
        env = {k: v for k, v in environ.items() if k not in
               ("WEZTERM_CONFIG_FILE", "GHOSTTY_CONFIG_DIR", "KITTY_CONFIG_DIRECTORY")}
        env["XDG_CONFIG_HOME"] = str(self.xdg)
        return env

    def check(self) -> None:
        for path, sealed in self.ghostty_user_state.items():
            if self._ghostty_state(path) != sealed:
                raise ConfigClosureError("config closure: Ghostty user config would apply")
        try:
            # No declared include grammar exists in these generated layouts.
            # Refuse all added entries, including dangling links, before reading.
            expected = {self.xdg / "kettle": set(), self.xdg / "ghostty": {"config"}}
            roots = {p.name for p in self.xdg.iterdir()}
            if not {"kettle", "ghostty"} <= roots or not roots - {"kettle", "ghostty"} <= PEER_RUNTIME_DIRS:
                raise ConfigClosureError("config closure: undeclared config root")
            for name in roots - {"kettle", "ghostty"}:
                directory = self.xdg / name
                if directory.is_symlink() or not directory.is_dir() or any(directory.iterdir()):
                    raise ConfigClosureError("config closure: undeclared config root")
            for directory, names in expected.items():
                if directory.is_symlink():
                    raise ConfigClosureError("config closure: undeclared include or init script")
                present = {p.name for p in directory.iterdir()}
                runtime = present - names if directory == self.xdg / "kettle" else set()
                if present - runtime != names or not runtime <= KETTLE_RUNTIME_FILES:
                    raise ConfigClosureError("config closure: undeclared include or init script")
                for name in runtime:
                    info = (directory / name).lstat()
                    if not stat.S_ISREG(info.st_mode) or info.st_size:
                        raise ConfigClosureError("config closure: remote command spool not empty")
            if (self.work / "init.lua").exists() or (self.work / "init.lua").is_symlink():
                raise ConfigClosureError("config closure: undeclared init script")
            if self.xdg.is_symlink() or self.assets.is_symlink():
                raise ConfigClosureError("config closure: replaced private root")
            for path, (digest, limit, mode) in self.sealed.items():
                data, _ = config_file_bytes(path, limit)
                if hashlib.sha256(data).hexdigest() != digest or path.stat().st_mode & 0o777 != mode:
                    raise ConfigClosureError("config closure: sealed input changed")
        except OSError:
            raise ConfigClosureError("config closure: sealed input unavailable") from None

    def source_changes(self) -> list:
        changes = []
        for source, digest, name in self.sources:
            try:
                _, current = config_file_bytes(source, ASSET_MAX_BYTES, follow=True)
                changed = current["sha256"] != digest
            except ConfigClosureError:
                changed = True
            if changed:
                changes.append({"entry": name, "source": str(source), "captured_sha256": digest})
        return changes


def config_closure_match(control: dict, reference: dict) -> bool:
    """Old output has no closure proof; never reuse it for a captured asset."""
    if not control and not reference:
        return True
    if not control or not reference:
        return False
    baseline = reference.get("kettle-a", reference.get("kettle"))
    return (control.get("kettle-a") == baseline and
            all(control[name] == reference[name] for name in control.keys() & reference.keys()
                if name != "kettle-b"))


@contextlib.contextmanager
def config_work_directory(out_dir: Path):
    # Retain consumed assets/configs with raw data, under a private directory.
    work = out_dir / "private-config"
    work.mkdir(mode=0o700)
    yield work


def config_campaign_row(closure: ConfigClosure, results: dict, recorder, collect: Callable) -> dict:
    """Checks bracket collection, outside its measured epoch; retain old rows."""
    row = None
    try:
        closure.check()
        row = collect()
        closure.check()
        return row
    except ConfigClosureError as error:
        reason = (str(error) if str(error) == "config closure: Ghostty user config would apply"
                  else "config closure: campaign inputs changed")
        results["meta"]["refusals"].append(reason)
        results["meta"]["countable"] = False
        if row is not None:
            results.setdefault("config_invalid_rows", []).append({**row, "error": reason})
        recorder.write()
        raise SystemExit("refused: " + reason) from None


GHOSTTY_DOMAIN = "com.mitchellh.ghostty"
# Ghostty 1.3 opens every new window at the last window's frame, which it
# keeps in this user default, and ignores window-width/window-height when it
# is set. After the user tiles a Ghostty window, the measured Ghostty opens at
# that tile's grid instead of COLS x ROWS.
GHOSTTY_FRAME_KEY = "NSWindowLastPosition"


def read_default(domain: str, key: str):
    """A user default's typed value (None when it is not set), from `defaults
    export`, which keeps types that `defaults read` flattens to text. A
    missing domain exports an empty dictionary; a failed export raises, since
    reading it as "not set" would let a restore delete the real value."""
    exported = subprocess.run(["defaults", "export", domain, "-"], capture_output=True, timeout=10)
    if exported.returncode != 0:
        raise RuntimeError(f"defaults export {domain} failed with status {exported.returncode}")
    return plistlib.loads(exported.stdout).get(key)


def plist_fragment(value) -> str:
    """A value as the XML property-list fragment `defaults write` takes, which
    keeps its types (reals stay reals)."""
    document = plistlib.dumps(value, fmt=plistlib.FMT_XML).decode()
    return document.split("<plist version=\"1.0\">", 1)[1].rsplit("</plist>", 1)[0].strip()


def write_default(domain: str, key: str, value) -> bool:
    """Set a user default to a typed value, or delete it for None, and say
    whether reading it back shows the change. `defaults delete` also fails
    when the key is absent, so its status cannot tell."""
    if value is None:
        subprocess.run(["defaults", "delete", domain, key], capture_output=True, timeout=10)
    else:
        subprocess.run(["defaults", "write", domain, key, plist_fragment(value)], capture_output=True, timeout=10)
    return read_default(domain, key) == value


def set_default(domain: str, key: str, value, attempts: int = 2) -> bool:
    """write_default, tried again once: a running Ghostty can write the same
    default at any moment."""
    return any(write_default(domain, key, value) for _ in range(attempts))


# How long past its timeout, or past being told to stop, a launch probe can
# take to end: it gives its terminal 10 s, then 1 s from SIGTERM to SIGKILL.
PROBE_STOP_GRACE = 15.0


# How long a group that still holds a live process is waited for past the
# probe's own deadline: the probe itself failed, and the Ghostty it left may
# still close and write its frame.
ROUND_CAP = 600.0


def round_until(seconds: float, tracked: Optional[float], ended: float) -> float:
    """How long wait_for_round may wait for the Ghostty launch last
    announced, whose probe has timeout `seconds`. With the probe's group
    known, `tracked` is when it was learned, after the probe's spawn: the
    probe SIGKILLs a Ghostty still running by its own deadline (the spawn
    plus `seconds`, then at most 11 s), and the cap lies ROUND_CAP past that.
    With no group (the harness ended around the spawn), the grace runs from
    `ended`, when the harness was seen to go: a probe whose harness is gone
    stops its terminal as if told to."""
    if tracked is None:
        return ended + PROBE_STOP_GRACE
    return tracked + seconds + PROBE_STOP_GRACE + ROUND_CAP


def wait_for_round(pgid: Optional[int], until: float,
                   clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], None] = time.sleep,
                   live: Optional[Callable[[int], Optional[bool]]] = None) -> bool:
    """Wait until a measured Ghostty can no longer write its own frame over
    the one about to go back, and say whether that is known. Its launch probe
    leads a process group holding it and the Ghostty it spawned, and the
    probe exits only once that Ghostty has, so the round is over once the
    group has emptied; a group ps cannot judge counts as live. With no group,
    the wait lasts until `until` and counts as over. A group still live at
    `until` returns False. It sends no signal: once the harness is gone,
    nothing unreaped holds the group's id."""
    live = live or group_has_live_members
    while True:
        if pgid is not None and live(pgid) is False:
            return True
        if clock() >= until:
            return pgid is None
        sleep(0.1)


# The hidden mode in which this script runs as GhosttyFrame's keeper.
KEEP_DEFAULT_ARG = "--keep-default"


def keep_default(domain: str, key: str, encoded: str) -> int:
    """GhosttyFrame's keeper, a separate process in its own session and the
    only writer of the default while a session runs. "clear N" clears it and
    answers "ok N"; "clear N SECONDS" also says a Ghostty launch with that
    timeout follows, and "round N PGID" names its launch probe's process
    group. "done N", or end of input however the harness ended (SIGKILL
    included), waits until that Ghostty can no longer write its own frame
    (wait_for_round), puts the saved value back, answers "restored N",
    "late N" (restored, but that Ghostty had not been seen to end) or
    "not-restored N", and exits. One writer means no clear can still be in
    flight when the value goes back. It ignores SIGTERM, SIGINT and SIGHUP: a
    kill by name aimed at the harness also matches this process, whose argv
    names the same script, and only end of input may end it."""
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, signal.SIG_IGN)
    saved = plistlib.loads(base64.b64decode(encoded))["value"] if encoded != "absent" else None

    def say(text: str) -> None:
        # The harness may be gone, and its end of the pipe with it; the value
        # still has to go back.
        try:
            print(text, flush=True)
        except OSError:
            sys.stdout = open(os.devnull, "w")

    tag = "eof"
    launching = False
    seconds = 0.0
    pgid: Optional[int] = None
    tracked: Optional[float] = None
    for line in sys.stdin:
        words = line.split()
        if len(words) in (2, 3) and words[0] == "clear":
            try:
                set_default(domain, key, None)
            except (OSError, RuntimeError, subprocess.SubprocessError):
                pass
            if len(words) == 3:
                # The launch follows the answer.
                launching, pgid, tracked = True, None, None
                try:
                    seconds = max(0.0, float(words[2]))
                except ValueError:
                    seconds = 0.0
            say(f"ok {words[1]}")
            continue
        if len(words) == 3 and words[0] == "round":
            if words[2].isdigit() and int(words[2]) > 1:
                pgid, tracked = int(words[2]), time.monotonic()
            say(f"ok {words[1]}")
            continue
        if len(words) == 2 and words[0] == "done":
            tag = words[1]
        break
    over = True
    if launching:
        over = wait_for_round(pgid, round_until(seconds, tracked, time.monotonic()))
    try:
        restored = set_default(domain, key, saved)
    except (OSError, RuntimeError, subprocess.SubprocessError):
        restored = False
    say(f"{'not-restored' if not restored else 'restored' if over else 'late'} {tag}")
    return 0


class GhosttyFrame:
    """Around a session that measures Ghostty: save the user's last-window
    frame, clear it before every Ghostty launch so Ghostty opens at the
    configured grid, and put the saved value back when the session ends.
    Nothing changes unless the saved value could be read. A keeper process
    holds the saved value and makes every change, and puts the value back
    when the harness says so or ends any other way (an exception, SIGTERM,
    SIGKILL), so the harness needs no signal handlers. It learns of each
    Ghostty launch (clear, track), so that it puts the value back only once
    a measured Ghostty still running can no longer write its own frame over
    it. Until the value is back, `recovery` holds the command that restores
    it by hand. The user's own Ghostty windows are never touched; a new
    window they open mid-session would open at the harness's grid."""

    # Longer than the keeper's worst case for one request: two attempts, each
    # a `defaults` call and an export capped at 10 s.
    WAIT = 60.0

    def __init__(self, domain: str = GHOSTTY_DOMAIN, key: str = GHOSTTY_FRAME_KEY,
                 recovery: Optional[Path] = None):
        self.domain, self.key, self.recovery = domain, key, recovery
        self.saved = None
        self.active = False
        self.keeper: Optional[subprocess.Popen] = None
        self.sequence = 0
        self.pending = b""
        # The Ghostty launch last announced: its timeout, its probe, and when
        # the keeper was told of the probe.
        self.launching = False
        self.seconds = 0.0
        self.round: Optional[subprocess.Popen] = None
        self.tracked: Optional[float] = None

    def restore_command(self) -> str:
        return (f"defaults delete {self.domain} {self.key}" if self.saved is None
                else f"defaults write {self.domain} {self.key} '{plist_fragment(self.saved)}'")

    def __enter__(self) -> "GhosttyFrame":
        self.saved = read_default(self.domain, self.key)
        if self.recovery is not None:
            self.recovery.write_text(self.restore_command() + "\n")
        encoded = ("absent" if self.saved is None
                   else base64.b64encode(plistlib.dumps({"value": self.saved}, fmt=plistlib.FMT_BINARY)).decode())
        self.keeper = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), KEEP_DEFAULT_ARG, self.domain, self.key, encoded],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        self.active = True
        try:
            self.clear()
        except BaseException:
            self._restore()
            raise
        return self

    def request(self, verb: str, timeout: float, *args) -> str:
        """Send "verb N args..." and return the keeper's answer to that N, or
        "" if none comes within `timeout` or the keeper has ended. An answer
        that arrives late for an earlier request is skipped, never taken for
        this one. Reads the raw pipe, so select cannot miss a line already
        buffered."""
        if self.keeper.stdin.closed:
            raise BrokenPipeError("the keeper's input is closed")
        self.sequence += 1
        tag = str(self.sequence)
        try:
            self.keeper.stdin.write(" ".join([verb, tag, *map(str, args)]).encode() + b"\n")
            self.keeper.stdin.flush()
        except OSError:
            # The keeper is gone. Close the pipe now, or its unflushed bytes
            # fail again when the object is collected.
            try:
                self.keeper.stdin.close()
            except OSError:
                pass
            raise
        if verb == "done":
            self.keeper.stdin.close()
        fd = self.keeper.stdout.fileno()
        deadline = time.monotonic() + timeout
        while True:
            while b"\n" in self.pending:
                line, self.pending = self.pending.split(b"\n", 1)
                words = line.decode(errors="replace").split()
                if len(words) == 2 and words[1] == tag:
                    return words[0]
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return ""
            ready, _, _ = select.select([fd], [], [], remaining)
            if not ready:
                return ""
            chunk = os.read(fd, 4096)
            if not chunk:
                return ""
            self.pending += chunk

    def clear(self, seconds: Optional[float] = None) -> None:
        """Clear the default; with `seconds`, a Ghostty launch with that
        timeout follows at once."""
        # A clear that does not take shows up as a round at another grid,
        # which the grid check fails; it is not worth ending a session over.
        if not self.active:
            return
        if seconds is not None:
            self.launching, self.seconds, self.round, self.tracked = True, seconds, None, None
        try:
            answer = self.request("clear", self.WAIT, *([] if seconds is None else [seconds]))
        except OSError:
            answer = ""
        if answer != "ok":
            # The keeper is gone or stuck: clear directly (a clear still in
            # flight there clears too); the restore falls back as well.
            try:
                set_default(self.domain, self.key, None)
            except (OSError, RuntimeError, subprocess.SubprocessError):
                pass

    def track(self, probe: subprocess.Popen) -> None:
        """Name the launch probe of the Ghostty just announced by clear."""
        if not self.active:
            return
        self.round, self.tracked = probe, time.monotonic()
        try:
            self.request("round", self.WAIT, probe.pid)
        except OSError:
            pass

    def _wait_for_round(self) -> bool:
        """wait_for_round, here, for the fallback."""
        if not self.launching:
            return True
        return wait_for_round(self.round.pid if self.round is not None else None,
                              round_until(self.seconds, self.tracked, time.monotonic()))

    def _restore(self) -> None:
        if not self.active:
            return
        self.active = False
        try:
            # The keeper first waits out a Ghostty round still in flight.
            waiting = 0.0
            if self.launching:
                waiting = max(0.0, round_until(self.seconds, self.tracked, time.monotonic()) - time.monotonic())
                if self.round is not None and self.round.poll() is None:
                    print(f"waiting for the measured Ghostty to close before {self.key} goes back",
                          file=sys.stderr, flush=True)
            try:
                answer = self.request("done", self.WAIT + waiting)
            except OSError:
                answer = ""
            if answer in ("restored", "late"):
                try:
                    self.keeper.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    pass
                if answer == "restored":
                    self._restored()
                else:
                    self._report("a measured Ghostty was still running when the value went back")
                return
            # The keeper could not restore, or is gone or stuck: stop it and
            # whatever it started, and restore here only once none of them
            # can still write.
            try:
                stopped = self._stop_keeper()
            except Exception:
                stopped = False
            if not stopped:
                self._report("could not confirm that the processes the keeper started have stopped")
                return
            over = self._wait_for_round()
            try:
                restored = set_default(self.domain, self.key, self.saved)
            except Exception:
                restored = False
            if not restored:
                self._report("the value read back differs")
            elif over:
                self._restored()
            else:
                self._report("a measured Ghostty was still running when the value went back")
        except BaseException:
            # A second interrupt while restoring here: say how to finish.
            self._report("interrupted while restoring")
            raise
        finally:
            try:
                self.keeper.stdout.close()
            except OSError:
                pass

    def _restored(self) -> None:
        if self.recovery is not None:
            self.recovery.unlink(missing_ok=True)

    def _report(self, why: str) -> None:
        where = f" (also in {self.recovery})" if self.recovery is not None else ""
        print(f"could not restore {self.domain} {self.key} ({why}); restore it with: "
              f"{self.restore_command()}{where}", file=sys.stderr, flush=True)

    def _stop_keeper(self) -> bool:
        """Kill the keeper's process group (its own session: the keeper and
        any `defaults` command it started) and wait for it to empty. The
        keeper stays unreaped until then, so its pid, the group's id, cannot
        be reused by another process meanwhile. Returns whether the group
        emptied; if not, a writer may still be pending and nothing may be
        restored over it."""
        try:
            os.killpg(self.keeper.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            # macOS answers EPERM for a group whose only member is the
            # zombie leader; ps below tells whether anything live is left.
            pass
        deadline = time.monotonic() + 10
        while True:
            live = group_has_live_members(self.keeper.pid)
            if live is False:
                break
            # Live, or unknown because ps failed: never restore over a
            # writer that may still be pending.
            if live is None or time.monotonic() > deadline:
                return False
            time.sleep(0.05)
        try:
            self.keeper.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        return True

    def __exit__(self, *exc) -> None:
        self._restore()


def group_has_live_members(pgid: int) -> Optional[bool]:
    """Whether any process other than a zombie is still in process group
    `pgid`, from ps: a group whose only member is an unreaped zombie still
    answers kill(-pgid, 0), with success on some systems and EPERM on macOS.
    None when ps cannot say (it failed, was refused or timed out): the caller
    must then treat the group as possibly live."""
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # macOS: also the answer for a group holding only a zombie.
        pass
    try:
        listed = subprocess.run(["ps", "-A", "-o", "pgid=,stat="], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    if listed.returncode != 0 or not listed.stdout.strip():
        return None
    return any(fields[0] == str(pgid) and not fields[1].startswith("Z")
               for fields in (line.split() for line in listed.stdout.splitlines()) if len(fields) >= 2)


def stamp_grid(stamp: Path) -> Optional[Tuple[int, int]]:
    """The (cols, rows) the stamp probe recorded when the payload started, or
    None if it never ran."""
    try:
        fields = stamp.read_text().split()
        return int(fields[1]), int(fields[2])
    except (OSError, IndexError, ValueError):
        return None


# How long a payload waits for its terminal to reach COLS x ROWS: a terminal
# can start its child before its first resize.
SETTLE_SECONDS = 5


def settled_grid(path: Path) -> Optional[Tuple[int, int]]:
    """The (cols, rows) `stamp --settle` ended at, or None if it never ran."""
    try:
        fields = path.read_text().split()
        return int(fields[0]), int(fields[1])
    except (OSError, IndexError, ValueError):
        return None


def round_grid(work: Path) -> Tuple[Optional[Tuple[int, int]], Optional[Tuple[int, int]]]:
    """The grid a round's workload ran at, and the grid its payload started
    at. The settled grid decides; without one (the payload was stopped before
    it got there) the start grid stands in."""
    start = stamp_grid(work / "stamp")
    return settled_grid(work / "grid") or start, start


def check_grid(row: dict, grid: Optional[Tuple[int, int]],
               start: Optional[Tuple[int, int]] = None) -> dict:
    """Record the round's grid, also on a round that failed for another
    reason (a terminal's first launch decides whether the session can
    compare), and fail a round that ran at any grid but COLS x ROWS: a smaller
    grid does less work, so its numbers would not compare. A start grid that
    differs (the terminal resized after starting its child) is recorded, not
    failed."""
    if grid is None:
        return row
    moved = {"start_cols": start[0], "start_rows": start[1]} if start and start != grid else {}
    row = {**row, "cols": grid[0], "rows": grid[1], **moved}
    if grid != (COLS, ROWS) and "error" not in row:
        return {"error": f"grid {grid[0]}x{grid[1]}, not {COLS}x{ROWS}", "cols": grid[0], "rows": grid[1], **moved}
    return row


def first_launch_refusal(name: str, row: dict, launched: set) -> Optional[str]:
    """A terminal's first launch in a session shows the grid it opens at. At
    any grid but COLS x ROWS none of its rounds can compare, so the session is
    refused then, not after a night of rounds that cannot count."""
    if name in launched or not row.get("cols"):
        return None
    launched.add(name)
    if (row["cols"], row["rows"]) != (COLS, ROWS):
        return f"{name} opened at {row['cols']}x{row['rows']}, not {COLS}x{ROWS}"
    return None


class Runner:
    def __init__(self, probes: Dict[str, Path], work: Path, kettle: Dict[str, str]):
        self.probes = probes
        self.work = work
        self.kettle = kettle
        # The Kettle entries whose rounds print their startup phase stamps
        # (--startup-phases).
        self.phases: set = set()
        # How long past its own timeout a launch probe may take to report.
        self.grace = 15.0
        # Cleared before each Ghostty launch while a session measures Ghostty.
        self.ghostty_frame: Optional[GhosttyFrame] = None
        # The launch probe of the round in flight, stopped if the session ends
        # while it runs.
        self.current: Optional[subprocess.Popen] = None
        self.config_closure: Optional[ConfigClosure] = None
        self.observation_context: Optional[Path] = None
        self.blink_probe = lambda args, work, timeout: run_latency_probe(
            self.probes["latency-probe"], args, work, timeout)

    def script(self, body: str) -> Path:
        """One script per distinct body, reused across launches. macOS assesses
        a script the first time it runs, which costs about 120 ms, so a new
        script per launch would add that to every shell time."""
        text = "#!/bin/sh\n" + body + "\n"
        path = self.work / f"payload-{hashlib.sha256(text.encode()).hexdigest()[:16]}.sh"
        if not path.exists():
            path.write_text(text)
            path.chmod(0o755)
        return path

    def settle_command(self) -> str:
        """Waits for the terminal to reach COLS x ROWS and records the grid it
        ended at (round_grid)."""
        return f'"{self.probes["stamp"]}" --settle {COLS} {ROWS} {SETTLE_SECONDS} "{self.work / "grid"}"'

    def launch(self, name: str, body: str, timeout: float,
               params: Optional[Dict[str, str]] = None, phases: bool = False,
               argv: Optional[List[str]] = None, settle: bool = True, cursor_exit_context: Optional[Path] = None) -> subprocess.Popen:
        """Start `name` running `body` after the stamp and, with `settle`,
        after the terminal has reached COLS x ROWS; without it, `body` places
        settle_command itself. Values that change per launch go in a params
        file the script sources, so the script stays the same file. `argv`
        replaces the terminal and its payload (the latency floors, which are
        their own windows); nothing then writes the stamp."""
        stamp = self.work / "stamp"
        # A failed launch writes no result, so nothing from the previous round
        # may be left to be read in its place.
        for stale in (stamp, Path(str(stamp) + ".pid"), self.work / "grid", self.work / "done",
                      self.work / "launch.json"):
            stale.unlink(missing_ok=True)
        params_file = self.work / "params"
        params_file.write_text("".join(f"{key}={shlex.quote(value)}\n" for key, value in (params or {}).items()))
        frame = self.ghostty_frame if name == "ghostty" else None
        if frame is not None:
            frame.clear(timeout)
        if argv is None:
            settled = f"{self.settle_command()}\n" if settle else ""
            payload = self.script(f'"{self.probes["stamp"]}" "{stamp}"\n. "{params_file}"\n{settled}{body}')
            argv = terminal_argv(name, payload, self.work, self.kettle)
        # Every terminal runs with the log filter it ships with, whatever the
        # harness's own shell sets. Kettle's phase stamps go to its stderr,
        # and only a stamped entry's startup rounds get their filter.
        stderr_path = self.work / "terminal.stderr"
        stderr_path.unlink(missing_ok=True)
        stamped = phases and name in self.phases
        env = {key: value for key, value in os.environ.items()
               if key not in ("RUST_LOG", "KETTLE_HC_OBSERVER_SELF_COST")}
        if getattr(self, "observer_pilot", False):
            env["KETTLE_HC_OBSERVER_SELF_COST"] = "1"
        if self.config_closure is not None:
            env = self.config_closure.launch_environment(env)
        if self.observation_context is not None:
            env["KETTLE_HC_LAUNCH_CONTEXT"] = str(self.observation_context)
        if stamped:
            env["RUST_LOG"] = "warn,kettle::startup=info"
        if cursor_exit_context is not None:
            env["RUST_LOG"] = "warn,kettle::cursor_blink=info"
            env["KETTLE_CURSOR_EXIT_CONTEXT"] = str(cursor_exit_context)
        # Its own session, so the probe leads a process group holding only it
        # and what it starts; stop() can clean that group up if it must.
        with (stderr_path.open("w") if stamped or cursor_exit_context is not None else open(os.devnull, "w")) as stderr:
            self.current = subprocess.Popen(
                [str(self.probes["launch"]), str(self.work / "launch.json"), str(stamp), str(timeout), "--", *argv],
                stdout=subprocess.DEVNULL, stderr=stderr, start_new_session=True, env=env,
                cwd=self.config_closure.cwd if self.config_closure else None,
            )
        if frame is not None:
            frame.track(self.current)
        return self.current

    def stop_current(self) -> None:
        """Stop the round still in flight, if the session ended during it."""
        if self.current is not None and self.current.poll() is None:
            self.stop(self.current, 30)

    def latency(self, name: str, options: dict, seed: int, keep: Optional[Path] = None) -> dict:
        """One keystroke-to-screen round. The terminal runs keyblock (a floor
        is its own window), the probe measures its window, and the payload's
        log splits every key into its input and output halves. The terminal
        is stopped through its launch probe whatever the probe reports."""
        log, out = self.work / "keyblock.log", self.work / "latency.json"
        context, timeline = self.work / "typing-launch.json", self.work / "typing-memory.jsonl"
        floor_mode = name.startswith("floor-")
        cursor_mode = options.get("payload") == "cursor"
        observer_off = options.get("observer_arm") == "off"
        enable, ack = self.work / "cursor.control", self.work / "cursor.ack"
        exit_context = self.work / "cursor-exit-context.json"
        launch_id = os.urandom(16).hex() if cursor_mode else None
        if cursor_mode:
            for path in (enable, ack, Path(str(ack) + ".tmp"), exit_context):
                path.unlink(missing_ok=True)
            os.mkfifo(enable, 0o600)
            private_json(exit_context, {"contract": cursor.CONTRACT, "launch_id": launch_id,
                "calibration_keys": 6, "warmup": options["warmup"], "keys": options["keys"]})
        receipts = [Path(str(context) + ".observer-" + suffix)
                    for suffix in ("request", "started", "stop", "reaped")]
        for stale in (log, out, context, timeline, Path(str(timeline) + ".self.json"), *receipts):
            stale.unlink(missing_ok=True)
        pending_result = None
        def linked_result(value):
            nonlocal pending_result
            pending_result = value
            if observer_off and not floor_mode and not cursor_mode and "typing_memory_valid" not in value:
                value.update(hc.typing_memory_row({}, [], None, None, options.get("sample_ms", 100), {},
                                                 "observer off (pilot arm)"))
            if cursor_mode and keep:
                value["cursor_artifacts"] = {kind: {"name": path.name, "sha256": file_sha256(path)}
                    for kind, path in (("probe", keep), ("keyblock", keep.with_suffix(".keyblock.log")),
                        ("launch", keep.with_suffix(".launch.json")), ("ack", keep.with_suffix(".cursor.ack")),
                        ("context", keep.with_suffix(".cursor-context.json")),
                        ("exits", keep.with_suffix(".cursor-exits.log"))) if path.is_file()}
            return value
        process, observer, probe_done_ns = None, None, None
        deadlines = cursor.budget(options)
        budget = deadlines["probe_s"]
        try:
            self.observation_context = None if floor_mode else context
            if floor_mode:
                floor = [str(self.probes["latency-floor"]), name[len("floor-"):], str(log)]
                process = self.launch(name, "", deadlines["launch_s"], argv=floor)
                ready = self.wait_for(Path(str(self.work / "stamp") + ".pid"), 20)
            else:
                body = f'exec "{self.probes["keyblock"]}" "{log}"'
                if cursor_mode:
                    body += f' cursor "{enable}" "{ack}" {int(deadlines["launch_s"] * 1000)}'
                process = self.launch(name, body, deadlines["launch_s"],
                    **({"cursor_exit_context": exit_context} if cursor_mode and options.get("exit_logs") else {}))
                ready = self.wait_for(self.work / "grid", 30 + SETTLE_SECONDS)
            pid = self.pid()
            if not ready or pid is None:
                return linked_result({"error": "the terminal never ran its payload"})
            time.sleep(1.0)
            info, observer_reason = {}, None
            if not floor_mode:
                try:
                    if not self.wait_for(context, 2):
                        raise ValueError("typing launch context missing")
                    launch_info = json.loads(context.read_text())
                    if (not isinstance(launch_info, dict) or launch_info.get("pid") != pid
                            or not hc.integer(launch_info.get("window_id")) or launch_info["window_id"] == 0):
                        raise ValueError("typing launch window identity mismatch")
                    info = launch_info
                    sample_ms = options.get("sample_ms", 100)
                    if not cursor_mode and not observer_off:
                        observer = hc.start_observer(self, process, context, [str(self.probes["observer"]),
                            str(pid), str(info["window_id"]), str(timeline), str(hc.now_ns()),
                            str(sample_ms), str(math.ceil(deadlines["launch_s"] * 1000 / sample_ms))])
                    # Confirm a native query before any probe calibration.
                    if not cursor_mode and not observer_off and (not self.wait_for(timeline, 2) or not wait_for_text(timeline, "\n", 2)):
                        observer_reason = "typing observer readiness missing"
                except (OSError, ValueError, KeyError, TypeError) as exc:
                    observer_reason = "typing launch context unavailable or invalid"
            if cursor_mode and observer_reason:
                return linked_result({"error": "cursor launch window identity unavailable"})
            args = ["--pid", str(pid), "--out", str(out), "--keys", str(options["keys"]),
                    "--warmup", str(options["warmup"]), "--censor-ms", str(options["censor_ms"]),
                    "--seed", str(seed), "--inject", options["inject"], "--deadline-ms", str(int(budget * 1000))]
            if cursor_mode:
                args += ["--cursor-control", str(enable), "--cursor-ack", str(ack),
                         "--initial-gap-ms", str(options.get("first_gap_ms", 2000))]
            if cursor_mode or "gap_ms" in options:
                gaps = options.get("gap_ms", [2000, 2400] if cursor_mode else [100, 300])
                args += ["--gap-ms", f"{gaps[0]}:{gaps[1]}"]
            if not cursor_mode and options.get("first_gap_ms"):
                args += ["--initial-gap-ms", str(options["first_gap_ms"])]
            started = time.monotonic()
            try:
                with verified_probe_use(self.probes["latency-probe"]) as artifact:
                    run_latency_probe(self.probes["latency-probe"], args, self.work, budget + 15)
            except subprocess.TimeoutExpired:
                pass
            finished = wait_for_text(out, "}", max(1.0, budget + 15 - (time.monotonic() - started)))
            # When the probe had ended, on the observer's clock.
            probe_done_ns = hc.now_ns()
        finally:
            try:
                if observer is not None:
                    self.end_sampler(observer)
            finally:
                try:
                    if process is not None:
                        clean = self.stop(process, 30)
                finally:
                    self.observation_context = None
                    if cursor_mode:
                        enable.unlink(missing_ok=True)
                    if keep:
                        for source, dest in ((out, keep), (timeline, keep.with_suffix(".memory.jsonl")),
                                             (log, keep.with_suffix(".keyblock.log")),
                                             (context, keep.with_suffix(".launch.json"))):
                            if source.exists():
                                shutil.copyfile(source, dest)
                        if cursor_mode:
                            for source, suffix in ((ack, ".cursor.ack"), (exit_context, ".cursor-context.json"),
                                                   (self.work / "terminal.stderr", ".cursor-exits.log")):
                                if source.exists():
                                    shutil.copyfile(source, keep.with_suffix(suffix))
                    if pending_result is not None:
                        linked_result(pending_result)
        # The terminal has been stopped by now. A failed round still records
        # its target, how its terminal ended and its observer's own failure
        # up to the probe's end, so a desktop failure the probe reports cannot
        # stand for any of them.
        def failed(error: str) -> dict:
            row = {"error": error, "target_pid": pid, "attribution_contract": hc.ATTRIBUTION_CONTRACT,
                   **({} if clean else {"killed": True})}
            try:
                ended = json.loads((self.work / "launch.json").read_text())
            except (OSError, json.JSONDecodeError):
                ended = None
            row["shutdown"] = shutdown_kind(ended, clean)
            observed = observer_reason
            if observed is None and observer is not None:
                try:
                    samples = hc.read_jsonl(timeline, 12000, 32 * 1024 * 1024)
                    observed = hc.observed_until(samples, pid, info.get("window_id"),
                                                 options.get("sample_ms", 100), probe_done_ns)
                except (OSError, ValueError, TypeError, KeyError, IndexError):
                    observed = "typing timeline unavailable or invalid"
            if observed:
                row.update(typing_memory_valid=False, typing_memory_reason=observed)
            return linked_result(row)
        if not finished:
            return failed("the latency probe never finished")
        try:
            probe = json.loads(out.read_text())
        except (OSError, json.JSONDecodeError):
            return failed("the latency probe wrote no result")
        if "error" in probe:
            return failed(f"latency probe: {probe['error']}")
        payload_records = read_keyblock_log(log)
        if cursor_mode:
            try:
                payload_records = cursor.read_payload(log.read_bytes())
                cursor.validate_stream(probe, payload_records, options["warmup"], options["keys"])
            except (OSError, ValueError, TypeError, KeyError):
                return failed("cursor stream invalid")
        row = latency_row(probe, payload_records, options["censor_ms"])
        row["tool_artifact"] = artifact
        if cursor_mode:
            row.update(latency_payload="cursor", gap_ms=options.get("gap_ms", [2000, 2400]),
                       first_gap_ms=options.get("first_gap_ms", 2000))
            if options.get("exit_logs"):
                try:
                    with (self.work / "terminal.stderr").open("rb") as stderr:
                        raw = stderr.read(8 * 1024 * 1024 + 1)
                    if len(raw) > 8 * 1024 * 1024:
                        raise ValueError("cursor exit log exceeds bound")
                    row.update(cursor.parse_exits(raw.decode("utf-8"), launch_id, info.get("window_id"),
                        probe, payload_records, options["warmup"], options["keys"], percentile))
                except (OSError, ValueError, TypeError, KeyError) as exc:
                    row.update(error="cursor exit evidence invalid", cursor_exit_valid=False)
            else:
                row.update(cursor.parse_exits("", launch_id, info.get("window_id"), probe, payload_records,
                    options["warmup"], options["keys"], percentile))
        if not floor_mode and not cursor_mode:
            try:
                samples = [] if observer_off else hc.read_jsonl(timeline, 12000, 32 * 1024 * 1024)
            except (OSError, ValueError, TypeError) as exc:
                samples, observer_reason = [], "typing timeline unavailable or invalid"
            if observer_off:
                observer_reason = "observer off (pilot arm)"
            row.update(hc.typing_memory_row(probe, samples, pid, info.get("window_id"),
                       options.get("sample_ms", 100), row["tool_artifact"], observer_reason))
            if keep:
                row["typing_artifacts"] = {kind: {"name": path.name, "sha256": file_sha256(path)}
                    for kind, path in (("probe", keep), ("memory", keep.with_suffix(".memory.jsonl")),
                                       ("keyblock", keep.with_suffix(".keyblock.log")),
                                       ("launch", keep.with_suffix(".launch.json"))) if path.is_file()}
                if "memory" in row["typing_artifacts"]:
                    row["typing_timeline_artifact"] = row["typing_artifacts"]["memory"]["name"]
                    row["typing_timeline_sha256"] = row["typing_artifacts"]["memory"]["sha256"]
        if not clean:
            row["killed"] = True
        return linked_result(row)

    def kill_group(self, process: subprocess.Popen) -> None:
        """Kill a launch probe that stopped responding, with everything it
        started. The group is still ours to signal: the unreaped probe leads
        it, so its id cannot have been reused."""
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()

    def finish(self, process: subprocess.Popen, timeout: float) -> dict:
        try:
            returncode = process.wait(timeout=timeout + self.grace)
        except subprocess.TimeoutExpired:
            self.kill_group(process)
            return {"error": "the launch probe never finished"}
        launched = self.work / "launch.json"
        if returncode != 0 or not launched.exists():
            return {"error": f"launch probe exited {returncode} without a result"}
        result = json.loads(launched.read_text())
        stderr_path = self.work / "terminal.stderr"
        if stderr_path.exists():
            text = stderr_path.read_text(errors="replace")
            result.update(parse_phases(text, result.get("started_ns")))
            if "startup phase=" in text:
                evidence = startup_phase_evidence(text, result.get("started_ns"))
                result["startup_stamps_ns"] = evidence["startup_stamps_ns"]
                result["startup_stamp_evidence"] = evidence
        grid, start = round_grid(self.work)
        if grid:
            result["cols"], result["rows"] = grid
        if start and start != grid:
            result["start_cols"], result["start_rows"] = start
        return result

    def wait_for(self, path: Path, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists():
                return True
            time.sleep(0.02)
        return False

    def sample(self) -> Optional[dict]:
        pid_file = Path(str(self.work / "stamp") + ".pid")
        if not pid_file.exists():
            return None
        pid = pid_file.read_text().strip()
        out = subprocess.run([str(self.probes["memsample"]), pid], capture_output=True, text=True)
        return json.loads(out.stdout) if out.returncode == 0 else None

    def stop(self, process: subprocess.Popen, timeout: float) -> bool:
        """Ask the launch probe to stop its terminal; True if it did.

        Only the probe may signal the terminal: it has not reaped it while it
        runs, so the pid cannot belong to anything else, whereas a pid read
        from a file here could already be reused. A probe that does not stop
        in time is killed with its process group, which is still ours to
        signal because the unreaped probe leads it, and the round fails.
        """
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.kill_group(process)
            return False
        # The probe had to SIGKILL a terminal that ignored the stop: whatever
        # it was doing, the round is not clean.
        try:
            killed = json.loads((self.work / "launch.json").read_text()).get("killed", False)
        except (OSError, json.JSONDecodeError):
            killed = False
        return not killed and not Path(str(self.work / "stamp") + ".pid").exists()

    def frontmost_pid(self) -> Optional[int]:
        front = subprocess.run(["lsappinfo", "front"], capture_output=True, text=True).stdout.strip()
        info = subprocess.run(["lsappinfo", "info", "-only", "pid", front], capture_output=True, text=True).stdout
        digits = "".join(ch for ch in info.split("=")[-1] if ch.isdigit())
        return int(digits) if digits else None

    def pid(self) -> Optional[int]:
        pid_file = Path(str(self.work / "stamp") + ".pid")
        try:
            return int(pid_file.read_text())
        except (OSError, ValueError):
            return None

    def frontmost(self) -> bool:
        pid = self.pid()
        return pid is not None and self.frontmost_pid() == pid

    def activate(self) -> bool:
        """Bring the launched terminal to the front by its pid, as a click
        would: some terminals launched from a script stay behind the window
        that launched them."""
        pid = self.pid()
        if pid is None:
            return False
        for _ in range(3):
            if self.frontmost_pid() == pid:
                return True
            subprocess.run(["osascript", "-e", 'tell application "System Events" to set frontmost of '
                            f"(first process whose unix id is {pid}) to true"], capture_output=True)
            time.sleep(0.3)
        return self.frontmost_pid() == pid

    @staticmethod
    def end_sampler(loop: subprocess.Popen) -> None:
        """Stop a memsample loop this runner started, and reap it."""
        if loop.poll() is None:
            loop.terminate()
        if isinstance(loop, hc.OwnedObserver):
            loop.reap()
        else:
            reap_owned_child(loop, 5)

    def startup(self, name: str) -> dict:
        # Hold the window for a second so terminals that spawn the child before
        # showing a window still reach the window server. The child must start
        # at once, so the grid is read after the hold.
        process = self.launch(name, f"/bin/sleep 1\nexec {self.settle_command()}", 20, phases=True,
                              settle=False)
        return self.finish(process, 20)

    def idle(self, name: str, settle: float, window: float, activate: bool) -> dict:
        process = self.launch(name, f"exec /bin/sleep {settle + window + 5}",
                              settle + window + 20 + SETTLE_SECONDS)
        if not self.wait_for(self.work / "grid", 20 + SETTLE_SECONDS):
            self.stop(process, 30)
            return {"error": "the terminal never ran its payload"}
        activated = self.activate() if activate else None
        checks = [self.frontmost()]
        time.sleep(settle)
        first = self.sample()
        time.sleep(window / 2)
        checks.append(self.frontmost())
        time.sleep(window / 2)
        second = self.sample()
        checks.append(self.frontmost())
        if not self.stop(process, 30):
            return {"error": "the terminal did not stop"}
        if not first or not second:
            return {"error": "terminal exited before sampling"}
        return {**idle_row(first, second, window, checks), "activated": activated}

    def flood_memory(self, name: str, flood: Path, offsets: Sequence[float], activate: bool,
                     footprint_detail: bool) -> dict:
        done = self.work / "done"
        hold = max(offsets) + 10
        process = self.launch(name, f'cat "{flood}"\n"{self.probes["stamp"]}" "{done}"\nexec /bin/sleep {hold}',
                              150)
        pid = self.pid() if self.wait_for(Path(str(self.work / "stamp") + ".pid"), 20) else None
        if pid is None:
            self.stop(process, 30)
            return {"error": "the terminal never started"}
        # A 100 ms timeline from launch to past the last offset, written to a
        # file (a pipe left undrained would fill and stall the sampler).
        timeline = self.work / "timeline.jsonl"
        with timeline.open("w") as sink:
            loop = subprocess.Popen([str(self.probes["memsample"]), str(pid), "100", str(int((150 + hold) * 10))],
                                    stdout=sink, stderr=subprocess.DEVNULL)
        if not self.wait_for(self.work / "stamp", 20):
            self.end_sampler(loop)
            self.stop(process, 30)
            return {"error": "the terminal never ran its payload"}
        activated = self.activate() if activate else None
        done_ns = read_stamp(done, 100) if self.wait_for(done, 100) else None
        frontmost = self.frontmost()
        detail = {}
        if done_ns is not None:
            for offset in sorted(offsets):
                delay = (done_ns + offset * 1e9 - time.clock_gettime_ns(time.CLOCK_UPTIME_RAW)) / 1e9
                if delay > 0:
                    time.sleep(delay)
                if footprint_detail:
                    detail[f"{offset:g}"] = footprint_detail_at(pid, self.work)
            time.sleep(0.3)
        # The sampler goes first, while the launch probe still owns the
        # terminal: once the terminal is reaped its pid can be reused.
        self.end_sampler(loop)
        if not self.stop(process, 60):
            return {"error": "the terminal did not stop"}
        samples = [json.loads(line) for line in timeline.read_text().splitlines() if line.strip()]
        row = flood_row([(s["t_ns"], s["footprint"], s["max_footprint"]) for s in samples], done_ns, offsets)
        row.update({"frontmost": frontmost, "activated": activated})
        if detail:
            row["footprint_detail"] = detail
        return row

    def vtebench(self, name: str, vtebench: Path, benchmarks: Path, dat: Path, seconds: int) -> dict:
        process = self.launch(
            name, f'exec "{vtebench}" -s -b "{benchmarks}" --dat "$DAT" --max-secs {seconds}', 900,
            params={"DAT": str(dat)},
        )
        if self.finish(process, 900).get("killed", True):
            return {"error": "vtebench did not finish in time"}
        if not dat.exists():
            return {"error": "no vtebench output"}
        row = vtebench_row(dat.read_text(), "us")
        missing = missing_benchmarks(benchmarks, row["means_ms"])
        if missing:
            # A table without them would compare the rest as if nothing had
            # been dropped.
            raise SystemExit(f"vtebench in {name} produced no samples for {', '.join(missing)}")
        row["dat"] = dat.name
        return row


PHASE_LINE = re.compile(r"startup phase=(\w+) t_ns=(\d+)")
PATH_LINE = re.compile(r"startup path=(\w+)")


def stamped_entries(sides: Optional[str], kettle: Dict[str, str]) -> set:
    """The Kettle entries --startup-phases stamps: every one (`all`), or only
    the B side of an A/B (`b`), which with one build on both sides is the
    stamps-on against stamps-off control."""
    if not sides:
        return set()
    if sides == "b":
        if not is_ab(kettle):
            raise ValueError("--startup-phases b needs an A/B (--kettle-b or --kettle-b-config)")
        return {"kettle-b"}
    return set(kettle)


def parse_phases(text: str, started_ns: Optional[int]) -> dict:
    """Kettle's startup phase stamps (RUST_LOG=kettle::startup=info), as
    `phase_<name>_ms` since the launch probe spawned it, plus the pane's
    startup path. The format is pinned by startup-phases.fixture, which
    crates/kettle-ui/src/startup_trace.rs tests against too."""
    if started_ns is None:
        return {}
    phases: dict = {}
    for line in text.splitlines():
        found = PHASE_LINE.search(line)
        if found:
            phases[f"phase_{found.group(1)}_ms"] = (int(found.group(2)) - started_ns) / 1e6
        elif (found := PATH_LINE.search(line)):
            phases["startup_path"] = found.group(1)
    return phases


# Diagnostic adapters are deliberately separate from parse_phases and Runner.
# Their reports cannot change a historical row or make a session countable.
STARTUP_PHASES = (
    "main", "run_with", "event_loop_built", "config_loaded", "app_built",
    "fonts_enumerated", "fonts_ready", "display_read", "pane_spawned", "resumed",
    "fonts_join_start", "fonts_joined", "window_created", "gpu_ready",
    "window_revealed", "first_frame",
)
STARTUP_DURATIONS = {
    "fonts_join_wait_ms": ("fonts_join_start", "fonts_joined", True),
    "fonts_ready_to_resumed_ms": ("fonts_ready", "resumed", False),
    "fonts_ready_after_config_ms": ("config_loaded", "fonts_ready", False),
    "event_loop_build_ms": ("run_with", "event_loop_built", True),
    "gpu_init_ms": ("window_created", "gpu_ready", True),
}
STARTUP_STAMP = re.compile(
    r"startup phase=([a-z][a-z0-9_]{0,63}) t_ns=([0-9]+) "
    r"since_main_ms=(?:[0-9]+(?:\.[0-9]+)?|-)"
    r"(?: thread=(main|fonts))?$"
)
STARTUP_PATH = re.compile(
    r"startup path=(resumed_early|after_renderer|pre_launch|unknown)"
    r"(?: monitor_match=(true|false))?(?: fonts_wait_ms=([0-9]+\.[0-9]{2}))?$"
)
# The phase name alone, so a record whose timestamp is malformed still
# invalidates its phase instead of leaving an earlier stamp in force.
STARTUP_PHASE_NAME = re.compile(r"startup phase=([a-z][a-z0-9_]{0,63})\b")
# struct winsize holds unsigned shorts, so no PTY geometry exceeds this.
NATIVE_GEOMETRY_MAX = 65535


def evidence_state(state: str, reason: str, **fields) -> dict:
    return {"state": state, "reason": reason, **fields}


def evidence_uint(value) -> bool:
    return type(value) is int and 0 <= value < 2**64



def native_identity(value) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[1-9][0-9]{0,19}", value) is not None and int(value) < 2**64

def evidence_number(value) -> bool:
    return (type(value) is int and 0 <= value < 2**64) or (type(value) is float and math.isfinite(value) and value >= 0)


def startup_phase_evidence(text: str, started_ns: Optional[int]) -> dict:
    """Read S1/S2/S3 stderr, preserving first raw stamps and explicit threads.

    Optional summary fields stay unavailable when absent.
    Duplicate conflicts invalidate the affected derived interval, never replace
    the first stamp. Unknown stamps remain diagnostics, outside published phases.
    """
    stamps, threads, unknown, invalid = {}, {}, {}, set()
    paths, malformed, duplicates, path_malformed = [], 0, 0, False
    for line in text.splitlines():
        if "startup phase=" in line:
            found = STARTUP_STAMP.search(line.strip())
            if not found or len(found[2]) > 20 or not evidence_uint(int(found[2])) or int(found[2]) == 0:
                malformed += 1
                phase = STARTUP_PHASE_NAME.search(line)
                if phase and phase[1] in STARTUP_PHASES:
                    invalid.add(phase[1])
                continue
            name, raw, thread = found[1], int(found[2]), found[3]
            target = stamps if name in STARTUP_PHASES else unknown
            if name in target:
                duplicates += 1
                if target[name] != raw or (name in stamps and threads.get(name) != thread):
                    invalid.add(name)
                continue
            target[name] = raw
            if name in stamps and thread is not None:
                threads[name] = thread
                expected = "fonts" if name in ("fonts_ready", "fonts_enumerated") else "main"
                if thread != expected:
                    invalid.add(name)
        elif "startup path=" in line:
            found = STARTUP_PATH.search(line.strip())
            if found:
                wait = float(found[3]) if found[3] is not None else None
                if wait is not None and not math.isfinite(wait):
                    malformed += 1
                    path_malformed = True
                else:
                    paths.append((found[1], None if found[2] is None else found[2] == "true", wait))
            else:
                malformed += 1
                path_malformed = True
    durations, validity = {}, {}
    for field, (begin, end, ordered) in STARTUP_DURATIONS.items():
        reason = None
        if begin not in stamps or end not in stamps:
            reason = "missing endpoints"
        elif begin in invalid or end in invalid:
            reason = "conflicting or malformed endpoints"
        elif ordered and stamps[end] < stamps[begin]:
            reason = "unordered endpoints"
        elif field.startswith("fonts_") and (
                not threads.get(begin) or not threads.get(end) or len(set(paths)) != 1
                or paths[0][0] == "unknown" or path_malformed):
            reason = "missing thread/path attribution"
        durations[field] = None if reason else (stamps[end] - stamps[begin]) / 1e6
        validity[field] = evidence_state("unavailable" if reason else "supported", reason or ("ordered raw stamps" if ordered else "signed raw stamps"))
    return {
        "capability": "startup-stamps-s1-s2-s3", "startup_stamps_ns": stamps,
        "startup_phase_threads": threads, "unknown_stamps_ns": unknown,
        "startup_path": paths[0][0] if len(set(paths)) == 1 and not path_malformed else None,
        "phase_ms": {name: (raw - started_ns) / 1e6 for name, raw in stamps.items()}
                    if evidence_uint(started_ns) else {},
        "durations": durations, "metric_validity": validity,
        "malformed_lines": malformed, "duplicate_stamps": duplicates,
        "invalid_phases": sorted(invalid),
        "monitor_match": paths[0][1] if len(set(paths)) == 1 and not path_malformed else None,
        "reported_fonts_wait_ms": paths[0][2] if len(set(paths)) == 1 and not path_malformed else None,
        "first_output_ms": None, "first_output_origin": None,
        "first_output_endpoint": None, "first_output_capability": "unavailable",
    }



PRE_LAUNCH_FIT_DECLINE = re.compile(
    r"startup pre_launch declined=fit surface=(\d+)x(\d+) monitor=(\d+)x(\d+) scale=([0-9.]+)$")


def pre_launch_fit_decline(text: str) -> Optional[dict]:
    """Kettle's explained decline when the configured grid does not fit the
    display, or None. The phase and path parsers ignore this line."""
    found = [PRE_LAUNCH_FIT_DECLINE.search(line.strip()) for line in text.splitlines()
             if "startup pre_launch declined=" in line]
    if len(found) != 1 or found[0] is None:
        return None
    width, height, monitor_w, monitor_h, scale = found[0].groups()
    return {"surface": [int(width), int(height)], "monitor": [int(monitor_w), int(monitor_h)],
            "scale": float(scale)}


def require_pre_launch_startup(text: str, allow_fit_decline: bool = False) -> dict:
    """Pinned single-display macOS smoke acceptance, also for hidden windows.
    `allow_fit_decline` also accepts an explained decline for a grid that does
    not fit the display (a small CI screen), never a silent fallback."""
    if allow_fit_decline:
        decline = pre_launch_fit_decline(text)
        report = startup_phase_evidence(text, None)
        if (decline is not None and report["startup_path"] in ("resumed_early", "after_renderer")
                and "display_read" in report["startup_stamps_ns"]):
            report["pre_launch_declined"] = decline
            return report
    report = startup_phase_evidence(text, None)
    stamps = report["startup_stamps_ns"]
    names = ("display_read", "pane_spawned", "resumed", "window_created", "gpu_ready")
    if report["startup_path"] != "pre_launch" or report["monitor_match"] is not True or any(
            name not in stamps or name in report["invalid_phases"] for name in names) or not (
            stamps["display_read"] <= stamps["pane_spawned"] < stamps["resumed"] <= stamps["window_created"] <= stamps["gpu_ready"]):
        raise ValueError("missing or unordered pre-launch startup evidence")
    return report

def startup_duration_summary(rows: List[dict]) -> dict:
    """Derive each round first. Never subtract medians of cumulative endpoints."""
    result = {}
    for field in STARTUP_DURATIONS:
        values = [row["durations"][field] for row in rows if row["durations"][field] is not None]
        result[field] = {
            "values": values, "n": len(values), "missing": len(rows) - len(values),
            "median": statistics.median(values) if values else None,
            "p95": percentile(values, 0.95) if values else None,
            "max": max(values) if values else None,
        }
    result["thread_rows"] = sum(bool(row["startup_phase_threads"]) for row in rows)
    result["path_counts"] = {path: sum(row["startup_path"] == path for row in rows)
                             for path in ("resumed_early", "after_renderer", "pre_launch", "unknown")}
    return result


def startup_grid_evidence(row: dict, policy: str = "settled", cols: int = COLS,
                          rows: int = ROWS) -> dict:
    """Validate retained observations only. No launch, wait, polling or repair."""
    if policy not in ("settled", "child", "native"):
        raise ValueError("unknown startup grid policy")
    target = (cols, rows)
    if any(type(value) is not int or value <= 0 for value in target):
        raise ValueError("invalid target geometry")
    if row.get("error"):
        return evidence_state("incomplete", "startup row failed")
    if any(type(row.get(k)) is not int for k in ("cols", "rows")) or (row.get("cols"), row.get("rows")) != target:
        return evidence_state("incomplete", "missing or wrong settled grid")
    if policy == "settled":
        return evidence_state("supported", "settled grid")
    # Absence of start_cols does not mean it matched: old check_grid elides it.
    if any(type(row.get(k)) is not int for k in ("start_cols", "start_rows")) or (row.get("start_cols"), row.get("start_rows")) != target:
        return evidence_state("incomplete", "missing or wrong explicit child grid")
    if policy == "child":
        return evidence_state("supported", "explicit child and settled grids")
    return native_pty_evidence(row, cols, rows)


def native_pty_evidence(row: dict, cols: int = COLS, rows: int = ROWS) -> dict:
    """Provisional S3 v1, assembled from Kettle and the harness wrapper."""
    native = row.get("native_pty")
    if native is None:
        return evidence_state("unavailable", "native PTY recorder absent", provisional=True)
    bad = lambda reason: evidence_state("incomplete", reason, provisional=True)
    if not isinstance(native, dict) or native.get("version") != "native_pty_v1":
        return evidence_state("malformed", "unknown native PTY format", provisional=True)
    if native.get("clock") != "CLOCK_UPTIME_RAW":
        return bad("native clock mismatch")
    for key in ("launch_id", "pane_id"):
        if not native_identity(row.get(key)) or native.get(key) != row[key]:
            return bad("native identity mismatch")
    if native.get("complete") is not True or native.get("overflow") is not False or type(native.get("dropped")) is not int or native["dropped"] != 0:
        return bad("native recording incomplete")
    times = [row.get("started_ns"), native.get("recording_start_ns"),
             native.get("created_ns"), native.get("initial", {}).get("t_ns")
             if isinstance(native.get("initial"), dict) else None,
             row.get("child_observed_ns"), native.get("recording_end_ns")]
    if not all(evidence_uint(t) and t > 0 for t in times) or times != sorted(times) or times[-1] < times[1] + 2_000_000_000:
        return bad("missing or short recording endpoints")
    if native.get("initial_stage") != "after_create_before_correction":
        return bad("initial observation stage unproven")
    child = native.get("child_observation")
    if not isinstance(child, dict) or any(child.get(key) != row.get(key) for key in
            ("child_observed_ns", "start_cols", "start_rows")) or type(child.get("sigwinch_count")) is not int or child["sigwinch_count"] != 0:
        return bad("child observation linkage or SIGWINCH failed")
    if not all(evidence_uint(child.get(k)) for k in ("start_ns", "end_ns")) or not times[1] <= child["start_ns"] <= times[4] <= child["end_ns"] <= times[-1] or child["end_ns"] - times[4] < 2_000_000_000:
        return bad("child observation linkage or SIGWINCH failed")
    def geometry(value):
        keys = ("cols", "rows", "pixel_width", "pixel_height")
        return isinstance(value, dict) and all(
            type(value.get(k)) is int and (1 if k in ("cols", "rows") else 0) <= value[k] <= NATIVE_GEOMETRY_MAX for k in keys) and "error" in value and value["error"] is None
    initial = native.get("initial")
    if not geometry(initial) or (initial["cols"], initial["rows"]) != (cols, rows):
        return bad("wrong initial native geometry")
    if any(type(child.get(k)) is not int or child[k] != initial[k] for k in ("pixel_width", "pixel_height")):
        return bad("child and native pixel geometry mismatch")
    events = native.get("events")
    if not isinstance(events, list) or len(events) > 64 or type(native.get("event_count")) is not int or native["event_count"] != len(events):
        return bad("missing or dropped native events")
    previous, observed = times[3], initial
    geometry_keys = ("cols", "rows", "pixel_width", "pixel_height")
    for seq, event in enumerate(events, 1):
        if not isinstance(event, dict) or type(event.get("seq")) is not int or event["seq"] != seq or not evidence_uint(event.get("t_ns")) or not previous <= event["t_ns"] <= times[-1]:
            return bad("native sequence/time gap")
        if any(event.get(k) != row[k] for k in ("launch_id", "pane_id")):
            return bad("native event identity mismatch")
        if event.get("reason") not in ("resize",) or event.get("outcome") not in ("ok", "noop") or event.get("native_error") is not None:
            return bad("failed or unrecorded resize")
        if not geometry(event.get("requested")) or not geometry(event.get("observed")):
            return bad("missing resize geometry")
        if event.get("outcome") != "noop" or event.get("signal_sent") is not False:
            return bad("native signal delivery unproven")
        if any(event["requested"][k] != observed[k] or event["observed"][k] != observed[k] for k in geometry_keys):
            return bad("geometry-changing startup resize")
        previous, observed = event["t_ns"], event["observed"]
    final = native.get("final")
    if not geometry(final) or final.get("t_ns") != times[-1] or any(final[k] != observed[k] for k in geometry_keys):
        return bad("missing or inconsistent final geometry")
    return evidence_state("supported", "complete provisional native history", provisional=True, events=len(events))


# The C2 smoke's fixed sampling cadence and a physical bound on footprint.
NATIVE_LAYER_SAMPLE_PERIOD_S = 0.5
NATIVE_LAYER_MAX_FOOTPRINT_MIB = 1_048_576

NATIVE_RECORD_LIMIT = 64 * 1024


def native_json(text: str) -> dict:
    """Bound and reject duplicate members, nonfinite numbers and deep input."""
    if len(text.encode("utf-8")) > NATIVE_RECORD_LIMIT:
        raise ValueError("oversized native evidence")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate native member")
            result[key] = value
        return result
    def nonfinite(value):
        raise ValueError("nonfinite native value")
    try:
        value = json.loads(text, object_pairs_hook=unique, parse_constant=nonfinite)
    except (RecursionError, OverflowError) as error:
        raise ValueError("invalid native evidence") from error
    if not isinstance(value, dict):
        raise ValueError("native evidence must be an object")
    return value


def collect_native_pty(row: dict, text: str, child_text: str, *, enabled: bool = False) -> dict:
    """Join one private log record to its owned launch/pane and child session.

    Off returns the original row. Malformed evidence raises ValueError; valid
    incomplete/error histories are retained for the strict policy to refuse.
    The caller supplies decimal-string IDs from its spawn handle and list_panes.
    """
    if not enabled:
        return row
    if len(text.encode("utf-8")) > 1024 * 1024:
        raise ValueError("oversized native log")
    lines = [line for line in text.splitlines() if "native_pty=" in line]
    if len(lines) != 1 or lines[0].count("native_pty=") != 1:
        raise ValueError("missing or duplicate native record")
    native = native_json(lines[0].split("native_pty=", 1)[1])
    child = native_json(child_text)
    for key in ("launch_id", "pane_id"):
        identity = row.get(key)
        if not native_identity(identity) or native.get(key) != identity:
            raise ValueError("native identity mismatch")
    if native.get("version") != "native_pty_v1" or native.get("clock") != "CLOCK_UPTIME_RAW" or child.get("clock") != native["clock"] or "child_observation" in native:
        raise ValueError("native format mismatch")
    if type(native.get("child_pid")) is not int or native["child_pid"] <= 0 or type(child.get("session_id")) is not int or child["session_id"] != native["child_pid"] or type(child.get("pid")) is not int or child["pid"] <= 0:
        raise ValueError("child session mismatch")
    def geometry(value, timed=False):
        if not isinstance(value, dict) or (timed and not evidence_uint(value.get("t_ns"))):
            return False
        if type(value.get("error")) is int:
            return all(value.get(k) is None for k in ("cols", "rows", "pixel_width", "pixel_height"))
        return "error" in value and value["error"] is None and all(
            type(value.get(k)) is int and (1 if k in ("cols", "rows") else 0) <= value[k] <= NATIVE_GEOMETRY_MAX
            for k in ("cols", "rows", "pixel_width", "pixel_height"))
    if any(not evidence_uint(native.get(k)) for k in ("created_ns", "recording_start_ns", "recording_end_ns", "event_count", "dropped")) or any(type(native.get(k)) is not bool for k in ("complete", "overflow")) or native.get("initial_stage") != "after_create_before_correction" or not geometry(native.get("initial"), True) or not geometry(native.get("final"), True):
        raise ValueError("invalid native endpoints")
    events = native.get("events")
    if not isinstance(events, list) or len(events) > 64 or native["event_count"] != len(events):
        raise ValueError("invalid native event count")
    for seq, event in enumerate(events, 1):
        if not isinstance(event, dict) or type(event.get("seq")) is not int or event["seq"] != seq or not evidence_uint(event.get("t_ns")) or event.get("reason") != "resize" or event.get("outcome") not in ("ok", "noop", "error") or any(event.get(k) != row[k] for k in ("launch_id", "pane_id")) or "signal_sent" not in event or (event["signal_sent"] is not None and type(event["signal_sent"]) is not bool) or "native_error" not in event or (event["native_error"] is not None and type(event["native_error"]) is not int) or not geometry(event.get("requested")) or not geometry(event.get("observed")):
            raise ValueError("invalid native event")
    if any(not evidence_uint(child.get(k)) for k in ("start_ns", "t_ns", "end_ns", "sigwinch")) or any(type(child.get(k)) is not int or not (1 if k in ("cols", "rows") else 0) <= child[k] <= NATIVE_GEOMETRY_MAX for k in ("cols", "rows", "pixel_width", "pixel_height")):
        raise ValueError("invalid child observation")
    observation = {"child_observed_ns": child["t_ns"], "start_cols": child["cols"],
                   "start_rows": child["rows"], "sigwinch_count": child["sigwinch"],
                   "start_ns": child["start_ns"], "end_ns": child["end_ns"],
                   "pixel_width": child["pixel_width"], "pixel_height": child["pixel_height"]}
    return {**row, "start_cols": child["cols"], "start_rows": child["rows"],
            "child_observed_ns": child["t_ns"],
            "native_pty": {**native, "child_observation": observation}}


def private_native_text(path: Path, limit: int) -> str:
    """Read a bounded owner-private regular file without following symlinks."""
    import stat
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > limit:
            raise ValueError("unsafe native evidence file")
        with os.fdopen(fd, "rb", closefd=False) as source:
            data = source.read(limit + 1)
        if len(data) > limit:
            raise ValueError("oversized native evidence file")
        return data.decode("utf-8")
    finally:
        os.close(fd)


def cursor_layer_evidence(data: dict, *, max_footprint_mib: float = 80.0,
                          max_wakeups: float = 0.5, max_cpu_percent: float = 0.02) -> dict:
    """Certify the smoke's exported monotonic samples and state transitions.

    The certified figures cover the measured window the samples actually span
    (`covered_interval`), which must lie inside the hand-off+1.5 s to timeout
    window (`allowed_interval`), last at least 3 s and follow the producer's
    fixed 0.5 s cadence. Nothing outside that window is claimed. Geometry
    reads are controls after measurement, never footprint samples. Complete
    evidence can still fail the caller's inclusive resource thresholds.
    """
    if not isinstance(data, dict) or "contract" not in data:
        return evidence_state("unavailable", "native layer smoke absent")
    contract = data["contract"]
    if not isinstance(contract, dict):
        return evidence_state("malformed", "invalid native layer smoke")
    numbers = {}
    for key in ("handoff_after_s",):
        if not evidence_number(contract.get(key)):
            return evidence_state("malformed", "invalid handoff aggregate")
        numbers[key] = contract[key]
    idle = contract.get("idle")
    if not isinstance(idle, dict) or any(not evidence_number(idle.get(k)) for k in
            ("span_s", "peak_mib", "wakeups_per_s", "cpu_percent")):
        return evidence_state("malformed", "invalid idle aggregates")
    numbers["idle"] = {k: idle[k] for k in ("span_s", "peak_mib", "wakeups_per_s", "cpu_percent")}
    states = {}
    for label in ("handoff", "rested", "after_reload", "after_key"):
        state = contract.get(label)
        if not isinstance(state, dict) or state.get("renderer") not in ("layer", "gpu") or any(not evidence_uint(state.get(k)) for k in ("handoffs", "exits", "hides")):
            return evidence_state("malformed", "invalid cursor_blink state")
        frames = state.get("exit_frame_us")
        if not isinstance(frames, dict) or not evidence_uint(frames.get("count")) or any(
                frames.get(k) is not None and not evidence_number(frames[k]) for k in ("p50", "p95", "max")):
            return evidence_state("malformed", "invalid exit frame aggregates")
        if (frames["count"] == 0 and (frames.get("p50") is not None or frames.get("p95") is not None or frames.get("max") != 0)) or (frames["count"] > 0 and any(frames.get(k) is None for k in ("p50", "p95", "max"))):
            return evidence_state("malformed", "inconsistent exit frame coverage")
        states[label] = {"renderer": state["renderer"], **{k: state[k] for k in ("handoffs", "exits", "hides")},
                         "exit_frame_us": {k: frames.get(k) for k in ("count", "p50", "p95", "max")}}
    # Legacy files remain descriptive; partial raw exports must never certify.
    raw_fields = ("clock", "stamps", "samples", "geometry_reads")
    certify_ready = all(
        set(contract[label]["exit_frame_us"]) >= {"count", "p50", "p95", "max"}
        and (contract[label]["exit_frame_us"]["count"] == 0
             or contract[label]["exit_frame_us"]["p50"] <= contract[label]["exit_frame_us"]["p95"]
             <= contract[label]["exit_frame_us"]["max"])
        for label in states)
    details = dict(capability="cursor-layer-smoke-418", aggregates=numbers, states=states,
                   interval_peak_mib=None, geometry_polling_valid=None, acceptance=None)
    if not any(key in contract for key in raw_fields):
        return evidence_state("unavailable", "legacy aggregate-only native layer smoke", **details)
    bad = lambda reason: evidence_state("incomplete", reason, **details)
    if any(key not in contract for key in raw_fields):
        return bad("missing raw native layer evidence")
    if contract["clock"] != "python-monotonic-s":
        return evidence_state("malformed", "unknown native layer clock")
    if not certify_ready:
        return bad("exit frame summaries incomplete or unordered")
    stamps = contract["stamps"]
    stamp_keys = ("wait_started", "handoff_seen", "interval_ms", "timeout_s",
                  "measure_start", "measure_end", "sample_period_s", "rest_read",
                  "reload_written", "key_sent")
    if not isinstance(stamps, dict):
        return evidence_state("malformed", "invalid native layer stamps")
    if any(key not in stamps for key in stamp_keys):
        return bad("missing native layer stamp")
    if any(not evidence_number(stamps[key]) for key in stamp_keys) or any(
            stamps[key] <= 0 for key in ("interval_ms", "timeout_s", "sample_period_s")):
        return evidence_state("malformed", "invalid native layer stamp value")
    # The producer samples every 0.5 s. A declared period cannot widen the
    # allowed gaps and so hide missing samples.
    if stamps["sample_period_s"] != NATIVE_LAYER_SAMPLE_PERIOD_S:
        return evidence_state("malformed", "unexpected native layer sample period")
    s = stamps
    interval = s["interval_ms"] / 1000
    timeout_bound = s["wait_started"] + s["timeout_s"]
    interval_start = s["handoff_seen"] + 1.5
    if not (s["wait_started"] <= s["handoff_seen"] < s["measure_start"] <
            s["measure_end"] < s["rest_read"] < s["reload_written"] < s["key_sent"]):
        return bad("unordered native layer stamps")
    if not (s["measure_start"] >= interval_start and s["measure_end"] <= timeout_bound):
        return bad("measurement outside handoff-to-timeout interval")
    if s["rest_read"] < timeout_bound + 2 * interval:
        return bad("rest read precedes timeout and last edge")
    handoff_after = s["handoff_seen"] - s["wait_started"]
    if handoff_after > 3 * interval + 1.0:
        return bad("handoff exceeds entry bound")
    if not math.isclose(numbers["handoff_after_s"], handoff_after, rel_tol=1e-9, abs_tol=1e-9):
        return bad("handoff aggregate mismatch")

    samples = contract["samples"]
    if not isinstance(samples, list):
        return evidence_state("malformed", "invalid native layer samples")
    if len(samples) < 2:
        return bad("insufficient native layer samples")
    for sample in samples:
        if not isinstance(sample, dict) or any(not evidence_number(sample.get(key)) for key in
                ("t", "footprint_mib", "cpu_ns", "wakeups")):
            return evidence_state("malformed", "invalid native layer sample")
        # A process footprint above 1 TiB is not a measurement.
        if sample["footprint_mib"] > NATIVE_LAYER_MAX_FOOTPRINT_MIB:
            return evidence_state("malformed", "implausible native layer footprint")
    first, last = samples[0], samples[-1]
    if first["t"] != s["measure_start"] or last["t"] != s["measure_end"]:
        return bad("sample endpoints disagree with stamps")
    period = s["sample_period_s"]
    gap_limit = 1.5 * period
    gaps = []
    # The endpoints match the in-window stamps, and every gap is at least one
    # period, so every sample is ordered and inside the allowed window.
    for before, after in zip(samples, samples[1:]):
        gap = after["t"] - before["t"]
        if gap + 1e-9 < period or gap > gap_limit:
            return bad("sample cadence or gap outside declared period")
        if any(after[key] < before[key] for key in ("cpu_ns", "wakeups")):
            return bad("nonmonotonic sample counters")
        gaps.append(gap)
    span = last["t"] - first["t"]
    if span < 3.0:
        return bad("idle sample span below 3 seconds")
    computed = dict(span_s=span, peak_mib=max(sample["footprint_mib"] for sample in samples),
                    wakeups_per_s=(last["wakeups"] - first["wakeups"]) / span,
                    cpu_percent=(last["cpu_ns"] - first["cpu_ns"]) / (span * 1e9) * 100)
    if any(not evidence_number(value) for value in computed.values()):
        return evidence_state("malformed", "nonfinite native layer metrics")
    if any(not math.isclose(idle[key], value, rel_tol=1e-9, abs_tol=1e-9)
           for key, value in computed.items()):
        return bad("idle aggregate mismatch")

    reads = contract["geometry_reads"]
    if not isinstance(reads, list):
        return evidence_state("malformed", "invalid geometry reads")
    if len(reads) != 20:
        return bad("expected twenty geometry reads")
    counters = ("handoffs", "exits", "hides")
    handoff = states["handoff"]
    previous = s["measure_end"]
    for read in reads:
        if not isinstance(read, dict) or not evidence_number(read.get("t")) or any(
                not evidence_uint(read.get(key)) for key in counters):
            return evidence_state("malformed", "invalid geometry read")
        if not previous < read["t"] < s["rest_read"]:
            return bad("geometry polling overlaps measurement or is unordered")
        if read.get("renderer") != "layer" or any(read[key] != handoff[key] for key in counters):
            return bad("geometry polling changed renderer or counters")
        previous = read["t"]

    rested, reload, key = (states[label] for label in ("rested", "after_reload", "after_key"))
    if handoff["renderer"] != "layer" or handoff["handoffs"] == 0 or any(
            contract[label].get("fallback", "missing") is not None for label in states):
        return bad("layer handoff or fallback state unproven")
    if (rested["renderer"] != "layer" or any(rested[k] != handoff[k] for k in counters) or
            contract["rested"].get("phase_on") is not True or
            "next_edge_ms" not in contract["rested"] or contract["rested"]["next_edge_ms"] is not None):
        return bad("rested state inconsistent with timeout")
    if (reload["renderer"] != "layer" or reload["exits"] != rested["exits"] + 1 or
            reload["handoffs"] != rested["handoffs"] + 1 or reload["hides"] != rested["hides"]):
        return bad("reload state did not exit once and hand off again")
    if (key["exits"] <= reload["exits"] or key["hides"] != reload["hides"] or
            key["handoffs"] < reload["handoffs"] or
            (key["renderer"] == "layer" and key["handoffs"] <= reload["handoffs"])):
        return bad("key state did not exit the reloaded layer")

    # started/exited/hidden balance active handoffs; every exit records a frame.
    if any(state["handoffs"] != state["exits"] + state["hides"] +
           int(state["renderer"] == "layer") or state["exit_frame_us"]["count"] != state["exits"]
           for state in states.values()):
        return bad("cursor state counters or exit history inconsistent")

    thresholds = dict(peak_mib=max_footprint_mib, wakeups_per_s=max_wakeups,
                      cpu_percent=max_cpu_percent)
    if any(not evidence_number(value) for value in thresholds.values()):
        return evidence_state("malformed", "invalid native layer acceptance thresholds")
    checks = {name: computed[name] <= limit for name, limit in thresholds.items()}
    median = statistics.median(sample["footprint_mib"] for sample in samples)
    if not evidence_number(median):
        return evidence_state("malformed", "nonfinite native layer metrics")
    details.update(interval_peak_mib=computed["peak_mib"], interval_median_mib=median,
                   geometry_polling_valid=True, samples=len(samples), span_s=span,
                   wakeups_per_s=computed["wakeups_per_s"], cpu_percent=computed["cpu_percent"],
                   covered_interval=[first["t"], last["t"]],
                   allowed_interval=[interval_start, timeout_bound],
                   sample_period_s=period, max_gap_s=max(gaps), gap_limit_s=gap_limit,
                   acceptance=dict(verdict="pass" if all(checks.values()) else "fail",
                                   thresholds=thresholds, checks=checks))
    return evidence_state("supported", "complete raw native layer evidence", **details)


def renderer_trace_evidence(text: str) -> dict:
    """No private echo trace format is present in the row-shaping producer.

    Reject proposed JSONL and arbitrary logs alike. A made-up wire agreement
    cannot attribute prepare/emit/upload, join echoes, or convert clock domains.
    """
    return evidence_state("unavailable", "producer echo trace wire format not supplied",
                          capability=None, echo_durations=None, acceptance=None)


def diagnostic_json(path: Path) -> dict:
    # Bound inputs, reject duplicate JSON members and nonfinite numbers.
    data, _ = config_file_bytes(path, 16 * 1024 * 1024, follow=True)
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate diagnostic JSON member")
            result[key] = value
        return result
    def nonfinite(value):
        raise ValueError("nonfinite diagnostic JSON number")
    result = json.loads(data, object_pairs_hook=unique, parse_constant=nonfinite)
    if not isinstance(result, dict):
        raise ValueError("diagnostic input must be an object")
    return result


def run_evidence_postprocessing(args) -> int:
    """Separate CLI exit path, before builds, platform checks or app launch."""
    reports = []
    native_log = getattr(args, "startup_native_log", None)
    child_file = getattr(args, "startup_child_observation", None)
    try:
        if bool(native_log) != bool(child_file) or (native_log and len(args.startup_input or []) != 1):
            raise ValueError("native collection needs one row input and both files")
        for path in args.startup_input or []:
            data = diagnostic_json(Path(path))
            workloads = data.get("workloads")
            if not isinstance(workloads, dict) or not isinstance(workloads.get("startup"), dict):
                raise ValueError("startup input needs workloads.startup")
            if native_log and sum(len(rows) if isinstance(rows, list) else 2 for rows in workloads["startup"].values()) != 1:
                raise ValueError("native collection needs exactly one launch row")
            for name, rows in workloads["startup"].items():
                if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
                    raise ValueError("invalid startup rows")
                # Export no terminal names, paths, identities or private stderr.
                if name not in ("kettle", "kettle-a", "kettle-b"):
                    if native_log:
                        raise ValueError("native collection requires a Kettle launch row")
                    continue
                if native_log:
                    if not rows:
                        # A sibling entry (kettle-a beside kettle-b) the
                        # harness created but never launched.
                        continue
                    rows = [collect_native_pty(rows[0],
                        private_native_text(Path(native_log), 1024 * 1024),
                        private_native_text(Path(child_file), NATIVE_RECORD_LIMIT), enabled=True)]
                reports.append({"kind": "startup", "rows": [
                    startup_grid_evidence(row, args.startup_grid_policy) for row in rows
                    if not row.get("warmup")]})
        phase_rows = []
        for path in args.startup_phase_input or []:
            text = config_file_bytes(Path(path), 16 * 1024 * 1024, follow=True)[0].decode("utf-8")
            phase_rows.append(startup_phase_evidence(text, args.startup_started_ns))
        if phase_rows:
            reports.append({"kind": "startup-phases", "rows": phase_rows,
                            "summary": startup_duration_summary(phase_rows)})
        for path in args.native_layer_input or []:
            reports.append({"kind": "native-layer", "evidence": cursor_layer_evidence(
                diagnostic_json(Path(path)),
                max_footprint_mib=getattr(args, "native_layer_max_footprint_mib", 80.0),
                max_wakeups=getattr(args, "native_layer_max_wakeups", 0.5),
                max_cpu_percent=getattr(args, "native_layer_max_cpu_percent", 0.02))})
        for path in args.trace_input or []:
            text = config_file_bytes(Path(path), 16 * 1024 * 1024, follow=True)[0].decode("utf-8")
            reports.append({"kind": "renderer-trace", "evidence": renderer_trace_evidence(text)})
    except (OSError, ValueError, RuntimeError) as error:
        # Raw exceptions can contain owner-local paths or fragments of input.
        print("diagnostic input refused: invalid or unreadable evidence", file=sys.stderr)
        return 1
    print(json.dumps({"diagnostic_only": True, "countable": False, "reports": reports}, indent=2, allow_nan=False))
    return 0


def read_keyblock_log(path: Path) -> Dict[int, Tuple[int, int, int]]:
    """keyblock's records, {seq: (bytes read, t_read, t_written)}. A torn last
    record (the payload killed mid-write) is dropped."""
    try:
        data = path.read_bytes()
    except OSError:
        return {}
    usable = len(data) - len(data) % KEYBLOCK_RECORD.size
    return {seq: (nbytes, t_read, t_written)
            for seq, nbytes, t_read, t_written in KEYBLOCK_RECORD.iter_unpack(data[:usable])}


def lead_out_of_range(leads: List[float], probe: dict) -> int:
    """Keys whose display time falls before their frame's arrival, or more
    than LATENCY_LEAD_PERIODS refresh periods after it."""
    period_ns = (probe.get("vsync") or {}).get("period_ns") or 0
    refresh = (probe.get("display") or {}).get("refresh_hz") or 60
    period_ms = period_ns / 1e6 if period_ns else 1000 / refresh
    return sum(1 for lead in leads if not 0 <= lead <= LATENCY_LEAD_PERIODS * period_ms)


def latency_row(probe: dict, records: Dict[int, Tuple[int, int, int]], censor_ms: float) -> dict:
    """One latency round from the probe's samples and the payload's log.

    A key's latency runs from the probe's post to the display time of the
    first frame showing the flip. The payload's record for the same sequence
    number splits it into an input half (post to the payload's read) and an
    output half (the payload's write to that display time). Every key must
    have reached the payload as exactly one byte; anything else is counted
    as a sequence mismatch.
    """
    samples = probe.get("samples") or []
    measured = [s for s in samples if not s.get("warmup")]
    latencies: List[float] = []
    inputs: List[float] = []
    outputs: List[float] = []
    leads: List[float] = []
    censored = mixed = reverted = 0
    for s in measured:
        record = records.get(s["seq"])
        if s.get("censored") or s.get("display") is None:
            censored += 1
            continue
        latencies.append((s["display"] - s["t_post"]) / 1e6)
        mixed += int(s.get("mixed") or 0)
        reverted += bool(s.get("reverted"))
        if s.get("arrival") is not None:
            leads.append((s["display"] - s["arrival"]) / 1e6)
        if record is not None and record[0] == 1:
            inputs.append((record[1] - s["t_post"]) / 1e6)
            outputs.append((s["display"] - record[2]) / 1e6)
    posted = LATENCY_CALIBRATION_KEYS + len(samples)
    # A posted key that never arrived as exactly one byte, or a read nobody
    # posted.
    mismatched = (sum(1 for seq in range(1, posted + 1) if records.get(seq, (0,))[0] != 1)
                  + sum(1 for seq in records if seq > posted))
    row: dict = {"samples_ms": latencies, "keys": len(measured), "censored": censored,
                 "mixed_frames": mixed, "reverted": reverted, "seq_mismatch": mismatched,
                 "inputs_ms": inputs, "outputs_ms": outputs,
                 "input_ms": statistics.median(inputs) if inputs else None,
                 "output_ms": statistics.median(outputs) if outputs else None,
                 # ScreenCaptureKit delivers a frame before the time it is
                 # displayed; every key's gap is kept and checked.
                 "leads_ms": leads, "lead_out_of_range": lead_out_of_range(leads, probe),
                 "activation": probe.get("activation"), "vsync": probe.get("vsync"),
                 "refresh_hz": (probe.get("display") or {}).get("refresh_hz")}
    # A censored key counts at the bound here too, as in every statistic.
    keys = latencies + [float(censor_ms)] * censored
    if keys:
        row.update({"mean_ms": statistics.mean(keys), "median_ms": statistics.median(keys),
                    "p95_ms": percentile(keys, 0.95), "p99_ms": percentile(keys, 0.99)})
    return row


def read_stamp(path: Path, timeout: float) -> Optional[int]:
    """The time a `stamp` file records, once it is fully written: the file
    appears when stamp opens it, before its buffered line lands."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            fields = path.read_text().split()
            if len(fields) == 3:
                return int(fields[0])
        except (OSError, ValueError):
            pass
        if time.monotonic() >= deadline:
            return None
        time.sleep(0.01)


def idle_row(first: dict, second: dict, window: float, checks: List[bool]) -> dict:
    """A round counts only if the window was frontmost at settle start, midway
    through the sampling window and at its end: blinking cursors run only in
    a focused window."""
    return {
        "cpu_percent": (second["cpu_ns"] - first["cpu_ns"]) / (window * 1e9) * 100,
        "wakeups_per_second": (second["wakeups"] - first["wakeups"]) / window,
        "footprint_mib": second["footprint"] / 2**20,
        "rss_mib": second["rss"] / 2**20,
        "frontmost": all(checks),
        "frontmost_checks": checks,
    }


def flood_row(samples: List[tuple], done_ns: Optional[int], offsets: Sequence[float]) -> dict:
    """Memory columns from a (t_ns, footprint, lifetime max) timeline.

    peak: the highest footprint seen, including the kernel's lifetime maximum.
    doneN: the first sample at or after N seconds past the flood's end; done+20
    falls after Kettle's 10 s and kitty's 15 s blink timeouts plus the driver's
    roughly 1 s release. release_s: when the footprint first came within one
    8 MiB driver chunk of the last column. A flood that never finished, or a
    timeline that ends early, is an error rather than a silent sample.
    """
    if done_ns is None:
        return {"error": "the flood never finished"}
    if not samples:
        return {"error": "no memory samples"}
    row: dict = {"done_ok": True,
                 "peak_mib": max(max(fp, peak) for _, fp, peak in samples) / 2**20}
    for offset in offsets:
        target = done_ns + int(offset * 1e9)
        after = [fp for t, fp, _ in samples if t >= target]
        if not after:
            return {"error": f"no sample at done+{offset:g} s"}
        row[f"done{offset:g}_mib"] = after[0] / 2**20
    settled = row[f"done{max(offsets):g}_mib"]
    release = next((t for t, fp, _ in samples if t >= done_ns and fp / 2**20 <= settled + 8), None)
    row["release_s"] = (release - done_ns) / 1e9 if release is not None else None
    row["timeline"] = [[round((t - done_ns) / 1e6), round(fp / 2**20, 2)] for t, fp, _ in samples]
    return row


def footprint_graphics(data: dict) -> Dict[str, dict]:
    """The (graphics) categories of `footprint -j` output, which hold the GPU
    driver's pools. Their names carry no paths."""
    categories = data.get("processes", [{}])[0].get("categories", {})
    return {name: {"dirty_mib": value.get("dirty", 0) / 2**20, "regions": value.get("regions", 0)}
            for name, value in categories.items() if name.endswith("(graphics)")}


def footprint_detail_at(pid: int, work: Path) -> Dict[str, dict]:
    """Diagnostic only: walking the address space can perturb the process."""
    out = work / "footprint.json"
    out.unlink(missing_ok=True)
    subprocess.run(["footprint", "-v", "-w", "-j", str(out), str(pid)], capture_output=True)
    try:
        return footprint_graphics(json.loads(out.read_text()))
    except (OSError, json.JSONDecodeError):
        return {"error": "footprint produced no data"}


# === Parsing and statistics ==========================================


def parse_dat_samples(text: str, unit: str = "ms") -> Dict[str, List[float]]:
    """Every sample per benchmark in a vtebench DAT file, in milliseconds."""
    per_ms = {"ms": 1, "us": 1000}[unit]
    lines = [line.split() for line in text.splitlines() if line.strip()]
    if not lines:
        return {}
    header, rows = lines[0], lines[1:]
    columns: Dict[str, List[float]] = {name: [] for name in header}
    for row in rows:
        for name, value in zip(header, row):
            if value != "_":
                columns[name].append(float(value) / per_ms)
    return {name: values for name, values in columns.items() if values}


def parse_dat(text: str) -> Dict[str, float]:
    """Median milliseconds per sample for each benchmark in a vtebench DAT file."""
    return {name: statistics.median(values) for name, values in parse_dat_samples(text).items()}


def vtebench_row(text: str, unit: str) -> dict:
    """One round of one terminal: each benchmark's mean and median sample."""
    samples = parse_dat_samples(text, unit)
    return {
        "unit": unit,
        "means_ms": {name: statistics.mean(values) for name, values in samples.items()},
        "medians_ms": {name: statistics.median(values) for name, values in samples.items()},
    }


def vtebench_means(row: dict) -> Dict[str, Optional[float]]:
    """Benchmark means of a round. A schema-1 row holds medians, so its means
    come from its .dat file (see load_session)."""
    values = row["means_ms"] if "means_ms" in row else row
    return {key: value if is_number(value) else None for key, value in values.items() if key != "error"}


def vtebench_aggregate(run: dict, benchmarks: Sequence[str]) -> Optional[float]:
    """Keep the session's benchmark set fixed; an invalid member loses a round."""
    means = vtebench_means(run)
    if ("error" in run or not benchmarks or set(means) != set(benchmarks)
            or any(not is_number(means[bench]) or means[bench] <= 0 for bench in benchmarks)):
        return None
    return geometric_mean(means[bench] for bench in benchmarks)


def geometric_mean(values: Iterable[float]) -> float:
    return math.exp(statistics.mean(math.log(value) for value in values))


def percentile(values: Sequence[float], q: float) -> float:
    """Linear interpolation between closest ranks, inclusive of the ends."""
    ordered = sorted(values)
    position = (len(ordered) - 1) * q
    low, high = ordered[math.floor(position)], ordered[math.ceil(position)]
    # Equal ends (including two infinities, whose difference is NaN) need no
    # interpolation.
    if low == high:
        return high
    return low + (high - low) * (position - math.floor(position))


def finite(value):
    """JSON has no infinity or NaN; write them as strings instead."""
    if isinstance(value, float) and not math.isfinite(value):
        return "nan" if math.isnan(value) else ("inf" if value > 0 else "-inf")
    if isinstance(value, dict):
        return {key: finite(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [finite(item) for item in value]
    return value


def dumps(value) -> str:
    return json.dumps(finite(value), indent=1, allow_nan=False)


def _beta_continued_fraction(a: float, b: float, x: float) -> float:
    """Lentz's continued fraction for the regularized incomplete beta."""
    tiny = 1e-300
    c, d = 1.0, 1.0 - (a + b) * x / (a + 1.0)
    d = 1.0 / (d if abs(d) > tiny else tiny)
    h = d
    for m in range(1, 400):
        m2 = 2 * m
        numerator = m * (b - m) * x / ((a + m2 - 1.0) * (a + m2))
        d = 1.0 + numerator * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + numerator / c if abs(c) > tiny else tiny
        h *= d * c
        numerator = -(a + m) * (a + b + m) * x / ((a + m2) * (a + m2 + 1.0))
        d = 1.0 + numerator * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + numerator / c if abs(c) > tiny else tiny
        step = d * c
        h *= step
        if abs(step - 1.0) < 1e-15:
            break
    return h


def regularized_beta(a: float, b: float, x: float) -> float:
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    front = math.exp(math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b)
                     + a * math.log(x) + b * math.log1p(-x))
    if x < (a + 1.0) / (a + b + 2.0):
        return front * _beta_continued_fraction(a, b, x) / a
    return 1.0 - front * _beta_continued_fraction(b, a, 1.0 - x) / b


def t_cdf(t: float, df: int) -> float:
    tail = 0.5 * regularized_beta(df / 2.0, 0.5, df / (df + t * t))
    return 1.0 - tail if t >= 0 else tail


def t_quantile(p: float, df: int) -> float:
    """The p-quantile of Student's t with `df` degrees of freedom, for p in
    (0.5, 1), by bisection on the CDF."""
    low, high = 0.0, 1.0
    while t_cdf(high, df) < p:
        high *= 2.0
    for _ in range(200):
        mid = (low + high) / 2.0
        if t_cdf(mid, df) < p:
            low = mid
        else:
            high = mid
    return (low + high) / 2.0


def t_interval(values: Sequence[float], level: float = LEVEL) -> Tuple[float, float, float]:
    """The mean with a two-sided Student-t interval. It keeps its coverage at
    the 5-10 rounds a session has, where a percentile bootstrap over rounds
    runs narrow (about 0.85-0.93 at a nominal 0.95). One value has no
    spread, so its interval is unbounded."""
    mean = statistics.mean(values)
    if len(values) < 2:
        return mean, -math.inf, math.inf
    half = (t_quantile(1.0 - (1.0 - level) / 2.0, len(values) - 1)
            * statistics.stdev(values) / math.sqrt(len(values)))
    return mean, mean - half, mean + half


def median_interval(values: Sequence[float], level: float = LEVEL) -> Tuple[float, float, float]:
    """The median with the distribution-free order-statistic (sign-test)
    interval: the narrowest symmetric pair of order statistics whose
    Binomial(n, 1/2) coverage is at least `level`. With fewer rounds than
    that needs (6 at 95 %), no pair reaches it and the interval is unbounded."""
    ordered = sorted(values)
    n = len(ordered)
    # r is the largest rank with P(Binomial(n, 1/2) <= r - 1) <= (1 - level) / 2;
    # the interval is [x_(r), x_(n+1-r)] in 1-based order statistics.
    within, cumulative = 0, 0.0
    for j in range(n):
        cumulative += math.comb(n, j) / 2.0 ** n
        if cumulative > (1.0 - level) / 2.0:
            break
        within += 1
    if within == 0:
        return statistics.median(ordered), -math.inf, math.inf
    k = min(within - 1, (n - 1) // 2)
    return statistics.median(ordered), ordered[k], ordered[n - 1 - k]


def ratio(base: float, test: float) -> float:
    """test/base, with a zero base kept rather than dropped: equal zeros tie,
    and a positive value over zero is infinitely worse."""
    if base == 0:
        return 1.0 if test == 0 else math.inf
    return test / base


def exp_bound(value: float) -> float:
    """exp for an interval bound: past the largest finite float the bound is
    unbounded, not an overflow."""
    return math.inf if value > 709.0 else math.exp(value)


def family_level(family: int) -> float:
    """The Bonferroni level for one of `family` rows tested together."""
    return 1.0 - (1.0 - LEVEL) / max(1, family)


def paired(a: List[Optional[float]], b: List[Optional[float]], family: int = 1) -> dict:
    """b against a, rounds paired by index: the geometric mean of the b/a
    ratios with a Student-t 95 % interval on their logs, and the rounds in
    which b was lower. With `family` > 1, `family_low`/`family_high` hold the
    interval at the Bonferroni level for rows tested together, which an A/A
    judges vtebench's benchmarks by.

    A zero in a pair (a round with no wakeups) has no log ratio. The estimate
    is then the median ratio and the interval the whole range of ratios,
    which can only be wider."""
    pairs = [(x, y) for x, y in zip(a, b) if x is not None and y is not None]
    if not pairs:
        return {}
    ratios = [ratio(x, y) for x, y in pairs]
    levels = [LEVEL] + ([family_level(family)] if family > 1 else [])
    if all(0.0 < r < math.inf for r in ratios):
        logs = [math.log(r) for r in ratios]
        bounds = [t_interval(logs, level) for level in levels]
        estimate = exp_bound(bounds[0][0])
        bounds = [(exp_bound(low), exp_bound(high)) for _, low, high in bounds]
    else:
        estimate = statistics.median(ratios)
        bounds = [(min(ratios), max(ratios))] * len(levels)
    stats = {"ratio": estimate, "low": bounds[0][0], "high": bounds[0][1],
             "wins": sum(1 for x, y in pairs if y < x), "n": len(ratios)}
    if family > 1:
        stats.update({"family": family, "family_low": bounds[1][0], "family_high": bounds[1][1]})
    return stats


def paired_difference(a: List[Optional[float]], b: List[Optional[float]],
                      level: float = LEVEL) -> dict:
    """The mean of b - a round pairs, in the metric's own unit, with a
    two-sided Student-t interval at `level` (95 % unless an equivalence test
    asks for its own): the absolute gain a ratio hides."""
    diffs = [y - x for x, y in zip(a, b) if x is not None and y is not None]
    if not diffs:
        return {}
    mean, low, high = t_interval(diffs, level)
    return {"diff": mean, "low": low, "high": high, "n": len(diffs)}


def latency_keys(run: dict, censor_ms: float) -> Optional[List[float]]:
    """A latency round's keys, with each censored key at the censor bound,
    which can only make a terminal look slower; None if the round failed."""
    if ("error" in run or run.get("warmup") or run.get("seq_mismatch")
            or not isinstance(run.get("samples_ms"), list)):
        return None
    censored = run.get("censored") or 0
    if (not isinstance(censored, int) or isinstance(censored, bool) or censored < 0
            or not is_number(censor_ms) or censor_ms <= 0
            or any(not is_number(v) or v < 0 for v in run["samples_ms"])):
        return None
    keys = list(run["samples_ms"]) + [float(censor_ms)] * censored
    return keys or None


def pooled_mean(rounds: Sequence[Sequence[float]]) -> float:
    return sum(sum(r) for r in rounds) / sum(len(r) for r in rounds)


def cluster_mean_ci(rounds: List[Optional[List[float]]]) -> dict:
    """Mean latency: the mean of the round (launch) means, with a Student-t
    95 % interval over rounds. Keys of one launch share a window, a GPU state
    and a compositor path, so they are not independent; the launch is the
    unit. Every round has the same key count, so this is the pooled mean."""
    present = [r for r in rounds if r]
    if not present:
        return {}
    mean, low, high = t_interval([statistics.mean(r) for r in present])
    return {"mean": mean, "low": low, "high": high, "n": len(present)}


def cluster_compare(base: List[Optional[List[float]]], test: List[Optional[List[float]]]) -> dict:
    """test against base on mean latency, rounds paired by index, from the
    round (launch) means. The difference in ms has a Student-t 95 % interval
    on the per-round differences; it sets latency's A/B gate. The ratio is the
    geometric mean of the per-round ratios with a t interval on their logs,
    as for every other row; being its own test, its interval can disagree
    with the difference's at the margin. A round is won when test's round
    mean is lower."""
    pairs = [(statistics.mean(b), statistics.mean(t)) for b, t in zip(base, test) if b and t]
    if not pairs:
        return {}
    diff, diff_low, diff_high = t_interval([t - b for b, t in pairs])
    stats = paired([b for b, _ in pairs], [t for _, t in pairs])
    return {"ratio": stats["ratio"], "low": stats["low"], "high": stats["high"], "diff": diff,
            "diff_low": diff_low, "diff_high": diff_high, "wins": stats["wins"], "n": stats["n"]}


def latency_standing(runs: List[dict], censor_ms: float, planned: int) -> dict:
    """Whether one entry's latency row may be ranked in its session: not
    measured if it lost LATENCY_NOT_MEASURED_SHARE of its rounds, and
    unranked if more than LATENCY_CENSOR_SHARE of its keys were censored."""
    counted = [run for run in runs if not run.get("warmup")]
    failed = sum(1 for run in counted if latency_keys(run, censor_ms) is None)
    good = [run for run in counted if latency_keys(run, censor_ms) is not None]
    keys = sum(int(run.get("keys") or 0) for run in good)
    censored = sum(int(run.get("censored") or 0) for run in good)
    out_of_range = sum(int(run.get("lead_out_of_range") or 0) for run in good)
    measured = failed < LATENCY_NOT_MEASURED_SHARE * max(planned, len(counted))
    return {"measured": measured, "failed": failed, "censored": censored, "keys": keys,
            "lead_out_of_range": out_of_range,
            "ranked": (measured and keys > 0 and censored <= LATENCY_CENSOR_SHARE * keys
                       and out_of_range <= LATENCY_CENSOR_SHARE * keys)}


def median_ci(values: Sequence[float]) -> dict:
    median, low, high = median_interval(values)
    return {"median": median, "low": low, "high": high, "n": len(values)}


def mean_ci(values: Sequence[float]) -> dict:
    mean, low, high = t_interval(values)
    return {"mean": mean, "low": low, "high": high, "n": len(values)}


def distribution(samples_ms: Sequence[float], censored: int = 0) -> dict:
    """Summary of one set of timing samples, such as keystroke latencies."""
    return {"mean": statistics.mean(samples_ms), "median": statistics.median(samples_ms),
            "p95": percentile(samples_ms, 0.95), "p99": percentile(samples_ms, 0.99),
            "n": len(samples_ms), "censored": censored}


def ordinal(n: int) -> str:
    suffix = "th" if 10 <= n % 100 <= 20 else {1: "st", 2: "nd", 3: "rd"}.get(n % 10, "th")
    return f"{n}{suffix}"


def first_per_date(sessions: List[dict], limit: int) -> List[dict]:
    """The earliest countable session on each date, for the first `limit`
    dates. Sessions after those never change a label: no reruns."""
    def instant(s: dict) -> datetime.datetime:
        moment = datetime.datetime.fromisoformat(s.get("started") or s["date"])
        return moment if moment.tzinfo else moment.replace(tzinfo=datetime.timezone.utc)

    ordered = sorted((s for s in sessions if s.get("countable") and "ratio" in s), key=instant)
    chosen: List[dict] = []
    for s in ordered:
        if all(s["date"] != c["date"] for c in chosen):
            chosen.append(s)
    return chosen[:limit]


def claim(sessions: List[dict]) -> dict:
    """The publication label for one row, from Kettle against the best other
    terminal in each session.

    "1st" needs 3 countable sessions on 3 dates, each with the ratio's
    interval below 1 and Kettle lower in at least 80 % of rounds. "tied 1st"
    needs no session in which Kettle was clearly behind. Anything else is a
    rank of at least 2nd, marked "(varies)" when sessions disagree.
    """
    countable = first_per_date(sessions, CLAIM_SESSIONS)
    if len(countable) < CLAIM_SESSIONS:
        return {"label": "insufficient sessions", "sessions": len(countable)}
    if all(s["high"] < 1 and s["wins"] >= math.ceil(CLAIM_WIN_SHARE * s["n"]) for s in countable):
        return {"label": "1st", "sessions": len(countable)}
    if not any(s["low"] > 1 for s in countable):
        return {"label": "tied 1st", "sessions": len(countable)}
    ranks = [s["rank"] for s in countable]
    rank = max(2, math.ceil(statistics.median(ranks)))
    varies = len(set(ranks)) > 1
    return {"label": ordinal(rank) + (" (varies)" if varies else ""), "sessions": len(countable)}


def ab_verdict(sessions: List[dict], gate: Optional[float] = None) -> dict:
    """An A/B change counts when the first 2 countable sessions, on different
    dates, both exclude 1 on the same side and the smaller change clears the
    A/A gate."""
    countable = first_per_date(sessions, 2)
    if len(countable) < 2:
        return {"verdict": "insufficient sessions"}
    headline = min((s["ratio"] for s in countable), key=lambda r: abs(r - 1))
    clears = gate is None or abs(headline - 1) >= gate
    if all(s["high"] < 1 for s in countable) and clears:
        verdict = "lower"
    elif all(s["low"] > 1 for s in countable) and clears:
        verdict = "higher"
    else:
        verdict = "no change"
    return {"verdict": verdict, "headline": headline, "gate": gate}


def aa_gate(stats: dict) -> dict:
    """An A/A interval must contain 1; its 95 % half-width sets that metric's
    gate. A row tested with others (a vtebench benchmark) is judged by its
    Bonferroni interval, so one A/A of 12 benchmarks is not 12 chances to
    fail."""
    half = (stats["high"] - stats["low"]) / 2
    low, high = stats.get("family_low", stats["low"]), stats.get("family_high", stats["high"])
    gate = {"contains_one": low <= 1 <= high, "half_width": half, "gate": max(0.03, 2 * half)}
    if "family" in stats:
        gate["family"] = stats["family"]
    return gate


def latency_aa_gate(stats: dict) -> dict:
    """A latency A/A's difference interval must contain 0, and it sets the A/B
    gate: an improvement of at least max(1 ms, 2 x the A/A's |difference|)."""
    return {"contains_one": stats["diff_low"] <= 0 <= stats["diff_high"], "aa_diff_ms": stats["diff"],
            "gate_ms": max(1.0, 2 * abs(stats["diff"]))}


def latency_ab_verdict(sessions: List[dict], gate_ms: Optional[float] = None) -> dict:
    """A latency change counts when the first 2 countable sessions, on
    different dates, both exclude 0 on the same side and the smaller
    difference clears the gate in ms. `no_regression` is the check for
    changes that do not aim at latency: every difference interval tops out
    at +1 ms or less."""
    countable = first_per_date(sessions, 2)
    # The no-regression check needs one session: a PR's own A/B.
    no_regression = all(s["diff_high"] <= 1.0 for s in countable) if countable else None
    if len(countable) < 2:
        return {"verdict": "insufficient sessions", "no_regression": no_regression}
    headline = min((s["diff"] for s in countable), key=abs)
    clears = gate_ms is None or abs(headline) >= gate_ms
    if all(s["diff_high"] < 0 for s in countable) and clears:
        verdict = "lower"
    elif all(s["diff_low"] > 0 for s in countable) and clears:
        verdict = "higher"
    else:
        verdict = "no change"
    return {"verdict": verdict, "headline_ms": headline, "gate_ms": gate_ms, "no_regression": no_regression}


# === Analysis ========================================================


class Metric:
    """Analysis contract. Reserved fields do not imply collector support."""
    def __init__(self, id: str, unit: str, kind: str = "scalar", extract: str = "field",
                 eligibility: str = "legacy", estimate: str = "median with distribution-free sign-test interval",
                 comparison: str = "geometric mean of paired ratios with log Student-t interval", aa_kind: str = "ratio",
                 claim_kind: str = "ratio", publication_role: str = "standing", direction: str = "lower"):
        self.id, self.unit, self.kind = id, unit, kind
        self.direction, self.analysis_kind = direction, kind
        self.extract, self.eligibility, self.estimate = extract, eligibility, estimate
        self.comparison, self.aa_kind = comparison, aa_kind
        self.claim_kind, self.publication_role = claim_kind, publication_role

    def record(self) -> dict:
        return dict(vars(self))


def scalar_metric(workload: str, field: str, unit: str, optional: bool = False,
                  signed: bool = False, diagnostic: bool = False) -> Metric:
    return Metric(f"{workload}.{field}", unit, kind="signed" if signed else "scalar",
                  eligibility="metric-validity" if optional else "legacy",
                  comparison="mean of paired differences with Student-t interval" if signed else "geometric mean of paired ratios with log Student-t interval",
                  aa_kind="none" if diagnostic or signed else "ratio",
                  claim_kind="none" if diagnostic or signed else "ratio",
                  publication_role="diagnostic" if diagnostic or signed else "standing",
                  direction="none" if signed else "lower")


METRIC_REGISTRY = {
    m.id: m for m in [
        *(scalar_metric("startup", f, "ms") for f in METRICS["startup"]),
        scalar_metric("idle", "cpu_percent", "percentage points"),
        scalar_metric("idle", "wakeups_per_second", "/s"),
        scalar_metric("idle", "footprint_mib", "MiB"),
        scalar_metric("startup", "first_output_ms", "ms", optional=True),
        *(scalar_metric("startup", f, "ms", optional=True, signed=True) for f in
          ("fonts_ready_to_resumed_ms", "fonts_ready_after_config_ms")),
        *(scalar_metric("startup", f, "ms", optional=True, diagnostic=True) for f in
          ("fonts_join_wait_ms", "event_loop_build_ms", "gpu_init_ms")),
        scalar_metric("latency", "typing_footprint_mib", "MiB", optional=True),
        *(scalar_metric("latency", f, "MiB", optional=True, diagnostic=True)
          for f in ("typing_observed_peak_mib", "typing_max_footprint_mib")),
        *(scalar_metric("output-memory", f, "MiB", optional=True, diagnostic=f != "printing_mib")
          for f in ("printing_mib", "printing_max_mib")),
        scalar_metric("blink-window", "footprint_mib", "MiB", optional=True),
        scalar_metric("blink-window", "cpu_percent", "percentage points", optional=True),
        scalar_metric("blink-window", "wakeups_per_second", "/s", optional=True),
        *(scalar_metric("blink-window", f, "MiB", optional=True, diagnostic=True)
          for f in ("blink_median_footprint_mib", "blink_peak_mib")),
        Metric("latency.mean_ms", "ms", "latency", "censored-keys", "latency-standing",
               "arithmetic mean of launch means with Student-t interval", "mean paired launch difference with Student-t interval and geometric mean paired ratio with log Student-t interval", "latency-difference", "latency"),
        Metric("latency-cursor.mean_ms", "ms", "latency", "censored-keys", "latency-standing",
               "arithmetic mean of launch means with Student-t interval", "mean paired launch difference with Student-t interval and geometric mean paired ratio with log Student-t interval", "latency-difference", "latency",
               "diagnostic"),
        *(Metric(f"{w}.{f}", "ms", "distribution", "pooled-keys", "latency-standing",
                 "pooled key quantile", "none", "none", "none", "descriptive") for f in
          ("median_ms", "p95_ms", "p99_ms", "input_ms", "output_ms") for w in ("latency", "latency-cursor")),
    ]
}


def metric_descriptor(workload: str, field: str) -> Metric:
    key = f"{workload}.{field}"
    if key in METRIC_REGISTRY:
        return METRIC_REGISTRY[key]
    if workload == "flood-memory" and field in flood_metrics(DEFAULT_FLOOD_OFFSETS):
        return scalar_metric(workload, field, "MiB")
    if workload == "flood-memory" and re.fullmatch(r"done(?:[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:e[+-]?[0-9]+)?|[+-]?inf|nan)_mib", field):
        return scalar_metric(workload, field, "MiB")
    if workload == "vtebench":
        return Metric(key, "ms", "benchmark", "benchmark-means", "legacy", "arithmetic mean of round means with Student-t interval")
    if workload == "startup" and field.startswith("phase_") and field.endswith("_ms"):
        # Keep the cumulative phase comparisons available to old readers.
        return scalar_metric(workload, field, "ms")
    raise ValueError(f"no metric contract for {key}")


def typing_memory_present(run: dict) -> bool:
    """Whether a latency row carries typing-memory evidence. Any typing value
    needs the provenance checks, so removing the marker cannot skip them."""
    return "typing_memory_valid" in run or any(
        key.startswith("typing_") and value is not None for key, value in run.items())


def metric_reason(descriptor: Metric, workload: str, run: dict) -> Optional[str]:
    if run.get("warmup"):
        return "warmup"
    if "error" in run or (descriptor.eligibility == "metric-validity" and run.get("killed")):
        return "round failed"
    if workload == "idle" and not run.get("frontmost"):
        return "not frontmost"
    field = descriptor.id.split(".", 1)[1]
    if workload == "blink-window" and descriptor.publication_role != "diagnostic" and run.get("blink_activity") != "verified":
        return "active blink " + run.get("blink_activity", "unproven")
    if workload == "latency" and field.startswith("typing_") and typing_memory_present(run):
        if latency_keys(run, 500) is None:
            return "latency guards invalid"
        artifact = run.get("tool_artifact") or {}
        if artifact != run.get("typing_tool_artifact") or not hc.typing_artifact_valid(artifact):
            return "verified typing probe artifact mismatch"
    if descriptor.eligibility == "metric-validity":
        validity = (run.get("metric_validity") or {}).get(field)
        if not isinstance(validity, dict):
            return "metric evidence unavailable"
        if validity.get("valid") is not True:
            return validity.get("reason") or "metric evidence invalid"
        if not validity.get("capability_version"):
            return "metric capability unavailable"
        expected, observed = validity.get("expected"), validity.get("observed")
        if not is_number(expected) or not is_number(observed) or expected <= 0 or observed < expected:
            return "metric evidence incomplete"
    return None


def metric_value(descriptor: Metric, workload: str, run: dict) -> Optional[float]:
    if metric_reason(descriptor, workload, run):
        return None
    value = row_value(workload, run, descriptor.id.split(".", 1)[1])
    if (descriptor.eligibility == "metric-validity" and value is not None and value < 0
            and descriptor.kind != "signed"):
        return None
    return value


def scalar_entry(descriptor: Metric, per_name: dict, names: List[str], ab: bool, unranked: set,
                 family: int = 1) -> dict:
    mean = descriptor.kind == "benchmark"
    terminals = {}
    for name in names:
        present = [v for v in per_name.get(name, []) if v is not None]
        if present:
            ci = mean_ci(present) if mean else median_ci(present)
            terminals[name] = {"estimate": ci["mean" if mean else "median"], "low": ci["low"],
                               "high": ci["high"], "n": ci["n"]}
    entry = {"kind": "mean" if mean else "median", "descriptor": descriptor.record(), "terminals": terminals,
             "values": {name: per_name.get(name, []) for name in names}}
    def compare(base: str, test: str, label: str) -> None:
        a, b = per_name.get(base, []), per_name.get(test, [])
        if descriptor.kind != "signed":
            entry[label] = paired(a, b, family if ab else 1)
        entry[label + "_diff"] = paired_difference(a, b)
    if ab and len(names) == 2:
        compare(names[0], names[1], "ab")
    elif names[0] in terminals and descriptor.claim_kind != "none":
        ranked = {name: t for name, t in terminals.items() if name not in unranked}
        others = {name: t for name, t in ranked.items() if name != names[0]}
        if others:
            best = min(others, key=lambda name: others[name]["estimate"])
            entry["best_other"] = best
            compare(best, names[0], "vs_best")
            entry["rank"] = sorted(ranked, key=lambda name: ranked[name]["estimate"]).index(names[0]) + 1
    if not ab and descriptor.claim_kind != "none":
        ranked = sorted((name for name in terminals if name not in unranked),
                        key=lambda name: (terminals[name]["estimate"], name))
        comparisons = []
        for i, base in enumerate(ranked):
            for test in ranked[i + 1:]:
                comparisons.append({"base": base, "test": test,
                                    "current": paired(per_name[base], per_name[test])})
        entry["pairwise"] = comparisons
        entry["adjacent"] = [{"base": base, "test": test,
                              "order": "ordered" if comparison["current"].get("low", 0) > 1 else "tied"}
                             for base, test in zip(ranked, ranked[1:])
                             for comparison in comparisons if comparison["base"] == base and comparison["test"] == test]
    entry["statistics"] = {"authoritative": "current", "current": {
        "estimate": descriptor.estimate, "comparison": descriptor.comparison,
        "difference_estimator": "mean of paired differences/Student-t",
        "terminals": {name: dict(t) for name, t in terminals.items()},
        **{k: entry[k] for k in ("ab", "ab_diff", "vs_best", "vs_best_diff") if k in entry}}}
    return entry


def is_number(value) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def row_value(workload: str, row: dict, metric: str) -> Optional[float]:
    """A round's value, or None when the round does not count."""
    if "error" in row or row.get("warmup"):
        return None
    if workload == "idle" and not row.get("frontmost"):
        return None
    value = row.get(metric)
    return float(value) if is_number(value) else None


def flood_metrics(offsets: Sequence[float]) -> tuple:
    """The flood columns for a session's offsets: the peak, then one per offset."""
    return ("peak_mib",) + tuple(f"done{offset:g}_mib" for offset in offsets)


def metrics_for(workload: str, meta: dict) -> tuple:
    if workload == "flood-memory":
        return flood_metrics(meta.get("flood_offsets") or DEFAULT_FLOOD_OFFSETS)
    return METRICS.get(workload, ())


LATENCY_METRICS = ("mean_ms", "median_ms", "p95_ms", "p99_ms", "input_ms", "output_ms")


def workload_metrics(workload: str, rows: Dict[str, List[dict]],
                     meta: Optional[dict] = None) -> Dict[str, Dict[str, List[Optional[float]]]]:
    """{metric: {terminal: [value per round]}} with rounds aligned by index."""
    if workload in ("latency", "latency-cursor"):
        return {metric: {name: [row_value(workload, run, metric) for run in runs] for name, runs in rows.items()}
                for metric in LATENCY_METRICS}
    if workload == "vtebench":
        benches = sorted({bench for runs in rows.values() for run in runs if "error" not in run
                          for bench in vtebench_means(run)})
        values: Dict[str, Dict[str, List[Optional[float]]]] = {bench: {} for bench in benches}
        values["geometric mean"] = {}
        for name, runs in rows.items():
            for bench in benches:
                values[bench][name] = [None if "error" in run else vtebench_means(run).get(bench) for run in runs]
            values["geometric mean"][name] = [
                vtebench_aggregate(run, benches)
                for run in runs
            ]
        return values
    metrics = [key for key in metrics_for(workload, meta or {})
               if any(is_number(run.get(key)) for runs in rows.values() for run in runs)]
    if workload == "startup":
        # Kettle's phase stamps, when --startup-phases recorded them, in the
        # order they happen: by median time, then by name.
        phases: Dict[str, List[float]] = {}
        for runs in rows.values():
            for run in runs:
                for key, value in run.items():
                    if key.startswith("phase_") and is_number(value):
                        phases.setdefault(key, []).append(value)
        metrics += sorted(phases, key=lambda key: (statistics.median(phases[key]), key))
    metrics += [m.id.split(".", 1)[1] for m in METRIC_REGISTRY.values()
                if m.id.split(".", 1)[0] == workload and m.eligibility == "metric-validity"
                and m.id.split(".", 1)[1] not in metrics
                and any(m.id.split(".", 1)[1] in run for runs in rows.values() for run in runs)]
    return {metric: {name: [metric_value(metric_descriptor(workload, metric), workload, run) for run in runs]
                           for name, runs in rows.items()} for metric in metrics}


def analyze(results: dict, names: List[str], ab: bool) -> dict:
    """Per-workload estimates, intervals and comparisons for one session."""
    if results.get("meta", {}).get("kind") == "observer-pilot":
        raise ValueError("observer pilot (diagnostic); use --observer-control")
    analysis: Dict[str, dict] = {}
    kettle = names[0]
    unranked = set(results.get("unranked", []))
    for workload, rows in results["workloads"].items():
        if workload in ("latency", "latency-cursor"):
            analysis[workload] = analyze_latency(results, rows, [n for n in names if n in rows], ab, workload)
            continue
        mean_based = workload == "vtebench"
        metrics: Dict[str, dict] = {}
        per_metric = workload_metrics(workload, rows, results.get("meta"))
        benchmarks = sum(1 for metric in per_metric if metric != "geometric mean")
        for metric, per_name in per_metric.items():
            descriptor = metric_descriptor(workload, metric)
            family = benchmarks if mean_based and metric != "geometric mean" else 1
            entry = scalar_entry(descriptor, per_name, names, ab, unranked, family)
            metrics[metric] = entry
        info: dict = {"metrics": metrics}
        if workload == "idle":
            info["frontmost"] = {name: [sum(1 for run in rows.get(name, []) if run.get("frontmost")),
                                        len(rows.get(name, []))] for name in names}
        grids = {name: sorted({(run["cols"], run["rows"]) for run in rows.get(name, []) if "cols" in run})
                 for name in names}
        if any(grids.values()):
            info["grids"] = grids
        analysis[workload] = info
    counts = workload_countable(results, results.get("meta") or {})
    for workload, info in analysis.items():
        for metric, entry in info["metrics"].items():
            entry["metric_countable"] = metric_countability(results, workload, metric, entry, counts.get(workload, False))
    return analysis


def latency_censor_ms(results: dict, workload: str = "latency") -> float:
    return float(((results.get("meta") or {}).get(workload) or {}).get("censor_ms", 500))


def analyze_latency(results: dict, rows: Dict[str, List[dict]], names: List[str], ab: bool, workload: str = "latency") -> dict:
    """The latency workload: every entry that ran (the terminals, then the
    unranked opaque variant and the floors), with mean latency over rounds
    (launches). Kettle is compared with the fastest other ranked terminal, or
    B with A, on the round means of every paired round."""
    censor_ms = latency_censor_ms(results, workload)
    planned = ((results.get("meta") or {}).get("rounds") or {}).get(workload, 0)
    entries = [name for name in names if name in rows] + [name for name in rows if name not in names]
    unranked = set(results.get("unranked", [])) | (set(rows) if workload == "latency-cursor" else set())
    keys = {name: [latency_keys(run, censor_ms) for run in rows[name]] for name in entries}
    standing = {name: latency_standing(rows[name], censor_ms, planned) for name in entries}
    metrics: Dict[str, dict] = {}
    # Median, percentiles and halves come from every counted key of the
    # session, never from per-round summaries.
    pooled = {name: [k for r in keys[name] if r for k in r] for name in entries}
    halves = {half: {name: [v for run in rows[name] if latency_keys(run, censor_ms) is not None
                            for v in run.get(half) or [] if is_number(v)] for name in entries}
              for half in ("inputs_ms", "outputs_ms")}
    for metric, per_name in workload_metrics(workload, rows).items():
        terminals = {}
        for name in entries:
            if not standing[name]["measured"]:
                # Not measured in this session: nothing of it is published.
                continue
            if metric == "mean_ms":
                ci = cluster_mean_ci(keys[name])
                if ci:
                    terminals[name] = {"estimate": ci["mean"], "low": ci["low"], "high": ci["high"], "n": ci["n"]}
                continue
            if metric in ("input_ms", "output_ms"):
                values = halves["inputs_ms" if metric == "input_ms" else "outputs_ms"][name]
                estimate = statistics.median(values) if values else None
            elif pooled[name]:
                estimate = {"median_ms": statistics.median, "p95_ms": lambda v: percentile(v, 0.95),
                            "p99_ms": lambda v: percentile(v, 0.99)}[metric](pooled[name])
            else:
                estimate = None
            if estimate is not None:
                terminals[name] = {"estimate": estimate, "n": sum(1 for r in keys[name] if r)}
        entry: dict = {"kind": "mean" if metric == "mean_ms" else "median", "terminals": terminals,
                       "values": {name: per_name.get(name, []) for name in entries}}
        if metric == "mean_ms":
            if ab and len(names) == 2:
                if standing[names[0]]["ranked"] and standing[names[1]]["ranked"]:
                    entry["ab"] = cluster_compare(keys[names[0]], keys[names[1]])
            elif names[0] in terminals and standing[names[0]]["ranked"]:
                ranked = {name: terminals[name] for name in terminals
                          if name not in unranked and standing[name]["ranked"]}
                others = {name: t for name, t in ranked.items() if name != names[0]}
                if others:
                    best = min(others, key=lambda name: others[name]["estimate"])
                    entry["best_other"] = best
                    entry["vs_best"] = cluster_compare(keys[best], keys[names[0]])
                    order = sorted(ranked, key=lambda name: ranked[name]["estimate"])
                    entry["rank"] = order.index(names[0]) + 1
        descriptor = metric_descriptor(workload, metric)
        entry["descriptor"] = descriptor.record()
        if metric == "mean_ms":
            ranked = sorted((name for name in terminals if name not in unranked and standing[name]["ranked"]),
                            key=lambda name: (terminals[name]["estimate"], name))
            comparisons = []
            for i, base in enumerate(ranked):
                for test in ranked[i + 1:]:
                    stats = cluster_compare(keys[base], keys[test])
                    comparisons.append({"base": base, "test": test, "current": stats})
            entry["pairwise"] = comparisons
            entry["adjacent"] = [{"base": base, "test": test,
                                  "order": "ordered" if comparison["current"].get("diff_low", 0) > 0 else "tied"}
                                 for base, test in zip(ranked, ranked[1:])
                                 for comparison in comparisons if comparison["base"] == base and comparison["test"] == test]
        entry["statistics"] = {"authoritative": "current", "current": {
            "estimate": descriptor.estimate, "comparison": descriptor.comparison,
            "terminals": {name: dict(t) for name, t in terminals.items()},
            **{k: entry[k] for k in ("ab", "vs_best") if k in entry}}}
        metrics[metric] = entry
    for descriptor in METRIC_REGISTRY.values():
        field = descriptor.id.split(".", 1)[1]
        if (descriptor.id.split(".", 1)[0] == workload and descriptor.kind == "scalar"
                and any(field in run for runs in rows.values() for run in runs)):
            values = {name: [metric_value(descriptor, "latency", run) for run in rows[name]] for name in entries}
            metrics[field] = scalar_entry(descriptor, values, entries, ab, unranked)
    result = {"metrics": metrics, "entries": entries, "standing": standing}
    if workload == "latency-cursor":
        result["exit_frames"] = {name: cursor.pooled(rows[name], percentile)
            if cursor.complete_exits(rows[name], planned) else None for name in entries}
        result["diagnostic"] = True
    return result


def latency_markdown(info: dict, ab: bool, countable: Optional[bool] = None) -> List[str]:
    metrics = info["metrics"]
    out = ["| entry | mean (95% CI) | median | p95 | p99 | input half | output half | censored | rounds |",
           "|---|---|---:|---:|---:|---:|---:|---:|---:|"]

    def cell(metric: str, name: str) -> str:
        terminal = metrics[metric]["terminals"].get(name)
        return f"{terminal['estimate']:.1f}" if terminal else "-"

    for name in info["entries"]:
        mean = metrics["mean_ms"]["terminals"].get(name)
        standing = info["standing"][name]
        mean_cell = f"{mean['estimate']:.1f} ({mean['low']:.1f}-{mean['high']:.1f})" if mean else "-"
        if not standing["measured"]:
            mean_cell += " not measured"
        elif not standing["ranked"] or info.get("diagnostic"):
            mean_cell += " unranked"
        out.append(f"| {name} | {mean_cell} | " + " | ".join(cell(m, name) for m in LATENCY_METRICS[1:])
                   + f" | {standing['censored']}/{standing['keys']} | {mean['n'] if mean else 0} |")
    if info.get("diagnostic"):
        out.append("Cursor latency is diagnostic and unranked; block latency remains the standing row.")
        for name, frames in info["exit_frames"].items():
            out.append(f"{name} total exit frame: " + ("unavailable" if frames is None else
                f"p50 {frames['p50_us']:.0f} us, p95 {frames['p95_us']:.0f} us, max {frames['max_us']} us, "
                f"n {frames['count']}; p95 <= 4000: {'yes' if frames['p95_le_4000'] else 'NO'}"))
    entry = metrics["mean_ms"]
    stats = entry.get("ab") if ab else entry.get("vs_best")
    if stats:
        out.append("")
        who = "B/A" if ab else f"Kettle/{entry['best_other']}"
        out.append(f"mean_ms: {who} {stats['ratio']:.3f} (95% CI {stats['low']:.3f}-{stats['high']:.3f}), "
                   f"difference {stats['diff']:+.2f} ms ({stats['diff_low']:+.2f} to {stats['diff_high']:+.2f}), "
                   f"lower in {stats['wins']}/{stats['n']} rounds" + ("" if ab else f", rank {entry['rank']}"))
        if ab:
            # A verdict only from a session that counts for latency.
            out.append("no regression (difference interval tops out at +1 ms or less): "
                       + (("yes" if stats["diff_high"] <= 1.0 else "NO") if countable
                          else "not decided, since this session does not count for latency"))
    elif ab:
        out.append("")
        out.append("mean_ms: no comparison (a side is not measured or not ranked)")
    for field, optional in metrics.items():
        if optional["descriptor"]["kind"] == "scalar":
            out.extend(["", f"{field} ({optional['descriptor']['unit']}): " + ", ".join(
                f"{name} {value['estimate']:.2f}" for name, value in optional["terminals"].items())])
            if optional.get("ab_diff"):
                out.append(f"B-A {optional['ab_diff']['diff']:+.2f} {optional['descriptor']['unit']} "
                           "(mean paired difference); ratio gate.")
    return out


def extended_report(results: dict) -> bool:
    """Only schema 3 or a new metric opts a session into the added report fields."""
    return results.get("schema", 1) >= 3 or any(
        descriptor.eligibility == "metric-validity"
        and any(descriptor.id.split(".", 1)[1] in run for runs in results["workloads"].get(
            descriptor.id.split(".", 1)[0], {}).values() for run in runs)
        for descriptor in METRIC_REGISTRY.values())


def summarize(results: dict, names: List[str], ab: bool, analysis: Optional[dict] = None) -> str:
    analysis = analyze(results, names, ab) if analysis is None else analysis
    out = ["# macOS standing", "", results["context"], ""]
    for workload, info in analysis.items():
        out.append(f"## {workload}")
        out.append("")
        if workload in ("latency", "latency-cursor"):
            countable = ((results.get("meta") or {}).get("workload_countable") or {}).get(workload)
            out.extend(latency_markdown(info, ab, countable))
            out.append("")
            continue
        metrics = info["metrics"]
        mean_based = workload == "vtebench"
        if workload == "blink-window":
            for name, runs in results["workloads"][workload].items():
                states = sorted({run.get("blink_activity", "unproven") for run in runs})
                out.append(f"{name}: shipped cursor activity {', '.join(states)}. Separate validation supports the setup; it does not observe every counted round.")
                quiet = [run["footprint_mib"] for run in runs if run.get("blink_window_valid") and is_number(run.get("footprint_mib"))]
                if states != ["verified"] and quiet:
                    out.append(f"Quiet-window descriptive endpoint footprint: {statistics.median(quiet):.2f} MiB. Active blink is not measured.")
            out.append("")
        if "grids" in info:
            out.append("Grid: " + ", ".join(
                f"{name} {'/'.join(f'{c}x{r}' for c, r in grid)}" for name, grid in info["grids"].items() if grid))
            out.append("")
        label = "benchmark" if mean_based else "metric"
        out.append(f"| {label} | " + " | ".join(names) + " |")
        out.append("|---|" + "---:|" * len(names))
        ordered = [m for m in metrics if m != "geometric mean"]
        for metric in ordered + (["geometric mean"] if "geometric mean" in metrics else []):
            cells = []
            for name in names:
                terminal = metrics[metric]["terminals"].get(name)
                cells.append("-" if not terminal else
                             f"{terminal['estimate']:.1f}" if mean_based else f"{terminal['estimate']:.2f}")
            title = "**geometric mean**" if metric == "geometric mean" else metric
            out.append(f"| {title} | " + " | ".join(cells) + " |")
        if "frontmost" in info:
            out.append("| frontmost rounds | " + " | ".join(
                f"{info['frontmost'][name][0]}/{info['frontmost'][name][1]}" for name in names) + " |")
        if ab:
            out.append("")
            for metric in ordered + (["geometric mean"] if "geometric mean" in metrics else []):
                stats = metrics[metric].get("ab")
                if stats:
                    diff = metrics[metric].get("ab_diff") if workload == "startup" else None
                    delta = (f"; B-A {diff['diff']:+.1f} {metrics[metric]['descriptor']['unit']}, 95% CI {diff['low']:+.1f} to {diff['high']:+.1f}"
                             if diff else "")
                    out.append(
                        f"{metric}: B/A {stats['ratio']:.3f} "
                        f"(95% CI {stats['low']:.3f}-{stats['high']:.3f}, n={stats['n']}, "
                        f"B lower in {stats['wins']}/{stats['n']}{delta})"
                    )
            for metric, entry in metrics.items():
                if entry["descriptor"]["kind"] == "signed" and entry.get("ab_diff"):
                    diff = entry["ab_diff"]
                    out.append(f"{metric}: B-A {diff['diff']:+.2f} {entry['descriptor']['unit']}, "
                               f"95% CI {diff['low']:+.2f} to {diff['high']:+.2f}; diagnostic only")
        else:
            compared = [m for m in ordered + (["geometric mean"] if "geometric mean" in metrics else [])
                        if metrics[m].get("vs_best")]
            if compared:
                out.append("")
                with_diff = workload == "startup"
                out.append(f"| {label} | best other | Kettle/other | 95% CI | Kettle lower in |"
                           + (" Kettle-other (95% CI) |" if with_diff else ""))
                out.append("|---|---|---:|---|---:|" + ("---|" if with_diff else ""))
                for metric in compared:
                    stats = metrics[metric]["vs_best"]
                    row = (f"| {metric} | {metrics[metric]['best_other']} | {stats['ratio']:.3f} | "
                           f"{stats['low']:.3f}-{stats['high']:.3f} | {stats['wins']}/{stats['n']} |")
                    diff = metrics[metric].get("vs_best_diff")
                    if with_diff:
                        row += (f" {diff['diff']:+.1f} ms ({diff['low']:+.1f} to {diff['high']:+.1f}) |" if diff
                                else " - |")
                    out.append(row)
        out.append("")
    if extended_report(results):
        out.extend(statistics_markdown(analysis))
    text = "\n".join(out)
    return publication.public(text) if publication.modern(results) else text


def statistics_markdown(analysis: dict) -> List[str]:
    out = ["## Statistical contracts", "", "statistics.current is authoritative.", "",
           "| metric | unit | direction | analysis kind | estimate / comparison | mean difference (Student-t CI) |",
           "|---|---|---|---|---|---|"]
    labels = []
    has_new_metrics = any(entry["descriptor"]["eligibility"] == "metric-validity"
                          for info in analysis.values() for entry in info["metrics"].values())
    for info in analysis.values():
        for entry in info["metrics"].values():
            descriptor = entry["descriptor"]
            diff = entry.get("ab_diff") or entry.get("vs_best_diff") or {}
            comparison = entry.get("ab") or entry.get("vs_best") or {}
            if not diff and "diff" in comparison:
                diff = {"diff": comparison["diff"], "low": comparison["diff_low"], "high": comparison["diff_high"]}
            difference = (f"{diff['diff']:+.3f} ({diff['low']:+.3f} to {diff['high']:+.3f})"
                          if diff else "not available")
            out.append(f"| {descriptor['id']} | {descriptor['unit']} | {descriptor['direction']} | "
                       f"{descriptor['analysis_kind']} | {descriptor['estimate']} / {descriptor['comparison']} | {difference} |")
            if has_new_metrics:
                for pair in entry.get("adjacent", []):
                    labels.append(f"Adjacent {descriptor['id']}: {pair['base']} / {pair['test']}: {pair['order']}.")
    out.append("")
    out.extend(labels)
    out.append("")
    return out


# === Sessions and combining ==========================================


def session_countable(meta: dict) -> bool:
    """A session counts only if its preflight was clean, Kettle ran from an
    app bundle, every requested round finished, and it was not a diagnostic
    that changes what the terminals do: walking their memory mid-measurement
    (--footprint-detail) or turning on Kettle's startup log (--startup-phases)."""
    return (not meta.get("refusals") and not meta.get("bare") and not meta.get("footprint_detail")
            and not meta.get("startup_phases") and meta.get("kind", "ordinary") == "ordinary"
            and meta.get("complete") is True)


def round_ok(workload: str, run: dict, meta: Optional[dict] = None) -> bool:
    if run.get("warmup"):
        return True
    if "error" in run or run.get("killed"):
        return False
    if workload == "vtebench":
        return bool(run.get("means_ms"))
    if workload in ("latency", "latency-cursor"):
        # A key read with another, or a read nobody posted, shifts the join
        # of samples to records: the round is not trusted.
        return (isinstance(run.get("samples_ms"), list) and not run.get("killed")
                and not run.get("seq_mismatch"))
    if workload in ("output-memory", "blink-window"):
        return run.get("printing_valid" if workload == "output-memory" else "blink_window_valid") is True
    required = metrics_for(workload, meta or {}) if workload == "flood-memory" else REQUIRED.get(workload, ())
    return all(is_number(run.get(key)) for key in required)


def workload_complete(results: dict, meta: dict, workload: str) -> bool:
    """Every requested round of every entry is present, has no error and
    carries its workload's values. For latency every round must have run and
    at one refresh rate, but a lost round only counts against its entry: a
    notification or a stray window can end a round without saying anything
    about the terminal."""
    rounds = meta.get("rounds") or {}
    expected = rounds.get(workload, 0) + (meta.get("warmup", 0) if workload == "startup" else 0)
    for runs in results["workloads"].get(workload, {}).values():
        if len(runs) != expected:
            return False
        # A latency entry that lost rounds is judged on its own (see
        # latency_standing); the other entries still count.
        if workload not in ("latency", "latency-cursor") and not all(round_ok(workload, run, meta) for run in runs):
            return False
    if workload in ("latency", "latency-cursor"):
        # A refresh rate that changed mid-session moves every sample.
        rates = {run.get("refresh_hz") for runs in results["workloads"][workload].values() for run in runs
                 if round_ok(workload, run, meta)}
        return len(rates) <= 1
    return True


def rounds_complete(results: dict, meta: dict) -> bool:
    """Every workload but latency is complete (see workload_complete);
    latency counts on its own, so a lost latency round never costs a
    session its other rows."""
    return all(workload_complete(results, meta, workload)
               for workload in results["workloads"] if workload not in OPT_IN_WORKLOADS)


def workload_countable(results: dict, meta: dict) -> Dict[str, bool]:
    """Whether each workload's rows count. The default workloads count
    together, as a complete session; latency counts on its own
    completeness, so a lost latency round never costs the other rows, nor a
    lost idle round the latency row."""
    base = session_countable(meta)
    defaults = base and rounds_complete(results, meta)
    return {workload: (base and workload_complete(results, meta, workload)) if workload in OPT_IN_WORKLOADS
            else defaults for workload in results["workloads"]}


def metric_countability(results: dict, workload: str, metric: str, entry: dict, counts: bool) -> dict:
    meta = results.get("meta") or {}
    descriptor = metric_descriptor(workload, metric)
    planned = ((results.get("meta") or {}).get("rounds") or {}).get(workload, 0)
    rows = results["workloads"].get(workload, {})
    report = {}
    for name, values in entry["values"].items():
        runs = rows.get(name, [])
        n = (entry["terminals"].get(name, {}).get("n", 0) if descriptor.kind in ("latency", "distribution")
             else sum(v is not None for v in values))
        reasons = []
        if not counts:
            reasons.append("session/workload not countable")
        if workload == "latency" and descriptor.id.startswith("latency.typing_") and any(
                typing_memory_present(r) for r in runs):
            method = ((meta.get("latency") or {}).get("typing_memory"))
            artifact = (meta.get("tool_artifacts") or {}).get("latency-probe")
            if method != hc.typing_method() or not hc.typing_artifact_valid(artifact):
                reasons.append("typing memory method/artifact mismatch")
            elif ((meta.get("tool_hashes") or {}).get("latency-probe") != artifact["bundle_sha256"]
                  or any(r.get("typing_tool_artifact") != artifact for r in runs)):
                reasons.append("typing memory bundle identity mismatch")
        if descriptor.eligibility == "metric-validity":
            for run in runs:
                reason = metric_reason(descriptor, workload, run)
                if reason and reason != "warmup":
                    reasons.append(reason)
            if n != planned or not planned:
                reasons.append("incomplete metric rounds")
        elif descriptor.kind == "latency":
            status = latency_standing(runs, latency_censor_ms(results, workload), planned)
            if not status["measured"]:
                reasons.append("latency not measured")
        if descriptor.kind == "benchmark" and planned and n != planned:
            reasons.append("incomplete benchmark rounds")
        if not n:
            reasons.append("metric unavailable")
        report[name] = {"countable": not reasons, "n": n, "planned": planned,
                        "failed": sum(not r.get("warmup") and (metric_value(descriptor, workload, r) is None)
                                      for r in runs) if descriptor.kind not in ("benchmark", "latency", "distribution")
                                  else sum(v is None for r, v in zip(runs, values) if not r.get("warmup"))
                                  if descriptor.kind == "benchmark" else sum("error" in r for r in runs),
                        "reasons": sorted(set(reasons))}
    return report


def load_session(folder: Path) -> dict:
    """A session directory's results with the fields --combine needs.

    Schema 1 (4.7.0 and earlier) has no metadata or preflight, so it never
    counts; its vtebench rows hold medians, so their means are rebuilt from
    the round's .dat file.
    """
    results = publication.strict_json(folder / "results.json", legacy=True)
    schema = results.get("schema", 1)
    if schema not in (1, 2, 3):
        raise ValueError(f"unsupported results schema {schema}")
    names = results["terminals"]
    if schema == 1:
        date = datetime.date.fromtimestamp((folder / "results.json").stat().st_mtime).isoformat()
        meta = {"date": date, "countable": False, "label": folder.name,
                "mode": "ab" if names[:1] == ["kettle-a"] else "standing"}
        vtebench = results["workloads"].get("vtebench")
        if vtebench:
            for name, runs in vtebench.items():
                for index, run in enumerate(runs):
                    dat = folder / f"{name}-r{index}.dat"
                    if "error" in run:
                        continue
                    # The row itself holds whole-ms medians; without the
                    # samples the round cannot give a mean.
                    runs[index] = vtebench_row(dat.read_text(), "ms") if dat.exists() else {
                        "error": f"missing {dat.name}"}
        countable = False
    else:
        meta = results["meta"]
        countable = session_countable(meta) and rounds_complete(results, meta)
    if meta.get("statistics_policy", "current") != "current":
        raise ValueError("unsupported statistics_policy; only current is supported")
    per_workload = workload_countable(results, meta)
    setup = {key: meta.get(key) for key in SESSION_KEYS}
    setup["identity"] = {name: {k: v for k, v in (ident or {}).items() if k in ("sha256", "cdhash")}
                         for name, ident in (meta.get("identity") or {}).items()}
    return {"dir": folder.name, "schema": schema, "names": names, "results": results, "date": meta["date"],
            "started": meta.get("started") or meta["date"], "countable": countable,
            "workload_countable": per_workload, "label": meta.get("label", folder.name), "ab": meta.get("mode") == "ab",
            "rounds": meta.get("rounds") or {}, "setup": setup, "configs": meta.get("configs") or {}}


def _combine_current(folders: List[Path], aa: Optional[Path] = None) -> dict:
    """Merge sessions into published values and labels (see claim, ab_verdict)."""
    sessions = [load_session(Path(folder)) for folder in folders]
    if len({s["ab"] for s in sessions}) > 1:
        raise SystemExit("--combine takes standing sessions or A/B sessions, not both")
    ab = sessions[0]["ab"]
    # Every session any row counts from must share one setup; latency rows
    # can count from a session whose other rows do not.
    counted = [s for s in sessions if s["countable"] or any(s["workload_countable"].values())]
    for s in counted[1:]:
        differs = sorted(key for key in s["setup"] if s["setup"][key] != counted[0]["setup"][key])
        if differs:
            raise SystemExit(f"{s['label']} and {counted[0]['label']} differ in {', '.join(differs)}; "
                             "a changed setup starts a new session set")
    gates: Dict[str, dict] = {}
    if aa:
        control = load_session(Path(aa))
        if not (control["countable"] and control["ab"] and control["names"] == ["kettle-a", "kettle-b"]):
            raise SystemExit(f"--aa {Path(aa).name} is not a countable, complete A/A session")
        identity = control["setup"]["identity"]
        if (identity.get("kettle-a") != identity.get("kettle-b")
                or control["configs"].get("kettle-a", "") != control["configs"].get("kettle-b", "")
                or (control["setup"].get("config_closures") or {}).get("kettle-a")
                   != (control["setup"].get("config_closures") or {}).get("kettle-b")):
            raise SystemExit(f"--aa {Path(aa).name} does not run the same build and config on both sides")
        # The A/A calibrates the harness and the machine, not a build, so every
        # setting but the binaries and configs under test must match. Its round
        # counts may differ (fewer rounds only widen its gates), and only the
        # tools both ran are compared.
        reference = counted[0] if counted else sessions[0]
        differs = sorted(key for key in control["setup"]
                         if key not in ("identity", "configs", "rounds", "tool_hashes", "latency", "latency-cursor", "config_closures")
                         and control["setup"][key] != reference["setup"][key])
        # Latency's knobs must match when both ran it; its entries differ by
        # design (a standing adds floors).
        knobs = ("keys", "warmup", "censor_ms", "inject", "signed", "typing_memory", "gap_ms", "first_gap_ms", "payload", "exit_logs", "exit_contract")
        for mode in ("latency", "latency-cursor"):
            latency_a, latency_b = control["setup"].get(mode), reference["setup"].get(mode)
            if latency_a and latency_b and any(
                    latency_a.get(k, {"gap_ms": [100, 300], "first_gap_ms": 0, "payload": "block", "exit_logs": False}.get(k)) !=
                    latency_b.get(k, {"gap_ms": [100, 300], "first_gap_ms": 0, "payload": "block", "exit_logs": False}.get(k)) for k in knobs):
                differs.append(mode)
        tools_a, tools_b = control["setup"].get("tool_hashes") or {}, reference["setup"].get("tool_hashes") or {}
        if any(tools_a[name] != tools_b[name] for name in tools_a.keys() & tools_b.keys()):
            differs.append("tool_hashes")
        if differs:
            raise SystemExit(f"--aa {Path(aa).name} and {reference['label']} differ in {', '.join(differs)}")
        # Only what the A/B tests may differ: the B side's build or config.
        if (control["configs"].get("kettle-a", "") != reference["configs"].get("kettle-a", "")
                or not config_closure_match(control["setup"].get("config_closures"),
                                            reference["setup"].get("config_closures"))):
            raise SystemExit(f"--aa {Path(aa).name} and {reference['label']} differ in the baseline config")
        for workload, info in analyze(control["results"], control["names"], True).items():
            if not control["workload_countable"].get(workload, control["countable"]):
                continue
            planned = control["rounds"].get(workload)
            for metric, entry in info["metrics"].items():
                stats = entry.get("ab")
                if stats and (not planned or stats["n"] >= math.ceil(MIN_PAIRED_SHARE * planned)):
                    descriptor = metric_descriptor(workload, metric)
                    local = metric_countability(control["results"], workload, metric, entry, True)
                    if descriptor.eligibility == "metric-validity" and not all(v["countable"] for v in local.values()):
                        continue
                    if descriptor.aa_kind == "latency-difference":
                        gates[descriptor.id] = latency_aa_gate(stats)
                    elif descriptor.aa_kind == "ratio":
                        gates[descriptor.id] = aa_gate(stats)
    analyses = [analyze(s["results"], s["names"], s["ab"]) for s in sessions]
    rows: Dict[str, dict] = {}
    for session, analysis in zip(sessions, analyses):
        for workload, info in analysis.items():
            counts = session["workload_countable"].get(workload, session["countable"])
            for metric, entry in info["metrics"].items():
                descriptor = metric_descriptor(workload, metric)
                row = rows.setdefault(descriptor.id, {"descriptor": descriptor.record(), "terminals": {},
                                                       "sessions": [], "per_session": []})
                local = metric_countability(session["results"], workload, metric, entry, counts)
                estimates = {name: terminal["estimate"] for name, terminal in entry["terminals"].items()}
                row["per_session"].append({"label": session["label"], "countable": counts,
                                           "estimates": estimates, "metric_countable": local,
                                           "statistics": entry["statistics"],
                                           **{k: entry[k] for k in ("ab_diff", "vs_best_diff", "pairwise", "adjacent") if k in entry}})
                if counts:
                    for name, value in estimates.items():
                        if local[name]["countable"]:
                            row["terminals"].setdefault(name, {"estimates": []})["estimates"].append(value)
                # A comparison with too few paired rounds (idle rounds that lost
                # focus) does not stand for the session.
                planned = session["rounds"].get(workload)
                stats = entry.get("ab") if ab else entry.get("vs_best")
                covered = not planned or (stats or {}).get("n", 0) >= math.ceil(MIN_PAIRED_SHARE * planned)
                base = {"label": session["label"], "date": session["date"], "started": session["started"],
                        "countable": counts and covered and all(local.get(name, {}).get("countable", False)
                                                                               for name in (session["names"] if ab else
                                                                                             [session["names"][0], entry.get("best_other")]))}
                if ab and entry.get("ab"):
                    row["sessions"].append({**base, **entry["ab"], "statistics": entry["statistics"],
                                            **({"difference": entry["ab_diff"]} if "ab_diff" in entry else {})})
                elif entry.get("vs_best"):
                    row["sessions"].append({**base, **entry["vs_best"], "peer": entry["best_other"],
                                            "rank": entry["rank"], "statistics": entry["statistics"],
                                            **({"difference": entry["vs_best_diff"]} if "vs_best_diff" in entry else {})})
    for key, row in rows.items():
        for terminal in row["terminals"].values():
            estimates = terminal["estimates"]
            terminal.update({"published": statistics.median(estimates), "min": min(estimates),
                             "max": max(estimates)})
        if ab:
            if row["descriptor"]["claim_kind"] == "none":
                # Retain old distribution rows' read behavior. They never
                # supplied a comparison or a calibrated gain gate.
                row["verdict"] = ({"verdict": "A/A missing"} if aa else latency_ab_verdict([])) \
                    if row["descriptor"]["kind"] == "distribution" else {"verdict": "diagnostic only"}
                continue
            gate = gates.get(key)
            if aa and not gate:
                row["verdict"] = {"verdict": "A/A missing"}
            elif gate and not gate["contains_one"]:
                # The same build differed from itself: that A/A cannot
                # calibrate anything.
                row["verdict"] = {"verdict": "A/A failed"}
            elif row["descriptor"]["aa_kind"] == "latency-difference":
                row["verdict"] = latency_ab_verdict(row["sessions"], gate["gate_ms"] if gate else None)
            else:
                row["verdict"] = ab_verdict(row["sessions"], gate["gate"] if gate else None)
            if gate:
                row["aa"] = gate
        elif row["sessions"]:
            row["claim"] = claim(row["sessions"])
    combined = {"schema": SCHEMA, "statistics_policy": "current", "sessions": [{k: s[k] for k in ("dir", "label", "date", "countable", "schema")} for s in sessions],
                "rows": rows}
    if not any(extended_report(s["results"]) for s in sessions) and not (aa and extended_report(control["results"])):
        # Preserve key insertion order as well as values in the legacy JSON.
        combined.pop("schema")
        combined.pop("statistics_policy")
        for row in rows.values():
            row.pop("descriptor")
            for per in row["per_session"]:
                for key in ("metric_countable", "statistics", "ab_diff", "vs_best_diff", "pairwise", "adjacent"):
                    per.pop(key, None)
            for per in row["sessions"]:
                per.pop("statistics", None)
                per.pop("difference", None)
    if any("latency-cursor" in session["results"]["workloads"] for session in sessions):
        cursor_runs = {}
        cursor_complete = {}
        for session in sessions:
            for name, runs in session["results"]["workloads"].get("latency-cursor", {}).items():
                cursor_runs.setdefault(name, []).extend(runs)
                cursor_complete[name] = cursor_complete.get(name, True) and (
                    session["workload_countable"].get("latency-cursor", False) and cursor.complete_exits(
                        runs, session["rounds"].get("latency-cursor", 0)))
        combined["cursor_exit_frames"] = {name: cursor.pooled(runs, percentile)
            if cursor_complete[name] else None for name, runs in cursor_runs.items()}
    combined["markdown"] = combined_markdown(combined, ab)
    return combined


def combine(folders: List[Path], aa: Optional[Path] = None) -> dict:
    return publication.combine(sys.modules[__name__] if __name__ in sys.modules else _publication_host(),
                               folders, aa, _combine_current)


def _publication_host():
    # importlib callers need not register the module in sys.modules.
    return argparse.Namespace(**globals())


def combined_markdown(combined: dict, ab: bool) -> str:
    out = ["# Combined macOS sessions", ""]
    out.append("| session | date | countable |")
    out.append("|---|---|---|")
    for s in combined["sessions"]:
        out.append(f"| {s['label']} | {s['date']} | {'yes' if s['countable'] else 'no'} |")
    out.append("")
    if "cursor_exit_frames" in combined:
        for name, frames in combined["cursor_exit_frames"].items():
            out.append(f"{name} pooled complete exit frame: " + ("unavailable" if frames is None else
                f"p95 {frames['p95_us']:.0f} us, n {frames['count']}; p95 <= 4000: "
                + ("yes" if frames['p95_le_4000'] else "NO")))
        out.append("")
    if ab:
        out.append("| row | A | B | sessions (B/A, 95% CI) | verdict |")
        out.append("|---|---:|---:|---|---|")
        for key, row in combined["rows"].items():
            a = row["terminals"].get("kettle-a", {}).get("published")
            b = row["terminals"].get("kettle-b", {}).get("published")
            per = "; ".join(f"{s['ratio']:.3f} ({s['low']:.3f}-{s['high']:.3f})" for s in row["sessions"] if "ratio" in s)
            verdict = row.get("verdict", {}).get("verdict", "-")
            if row.get("verdict", {}).get("no_regression") is not None:
                verdict += "; no regression" if row["verdict"]["no_regression"] else "; REGRESSION over +1 ms"
            out.append(f"| {key.replace('.', ' ', 1)} | {a if a is None else f'{a:.2f}'} | "
                       f"{b if b is None else f'{b:.2f}'} | {per} | {verdict} |")
        if "schema" in combined:
            out.extend(combined_statistics_markdown(combined))
        return "\n".join(out) + "\n"
    out.append("| row | Kettle | label | field (published = median of sessions) |")
    out.append("|---|---:|---|---|")
    for key, row in combined["rows"].items():
        kettle = row["terminals"].get("kettle")
        field = ", ".join(f"{name} {t['published']:.2f}" for name, t in
                          sorted(row["terminals"].items(), key=lambda item: item[1]["published"]))
        label = row.get("claim", {}).get("label", "-")
        value = f"{kettle['published']:.2f}" if kettle else "-"
        out.append(f"| {key.replace('.', ' ', 1)} | {value} | {label} | {field} |")
    out.append("")
    out.append("| row | session | counts | best other | Kettle/other | 95% CI | Kettle lower in | rank |")
    out.append("|---|---|---|---|---:|---|---:|---:|")
    for key, row in combined["rows"].items():
        for s in row["sessions"]:
            out.append(f"| {key.replace('.', ' ', 1)} | {s['label']} | {'yes' if s['countable'] else 'no'} | "
                       f"{s['peer']} | {s['ratio']:.3f} | {s['low']:.3f}-{s['high']:.3f} | {s['wins']}/{s['n']} | "
                       f"{s['rank']} |")
    if "schema" in combined:
        out.extend(combined_statistics_markdown(combined))
    return "\n".join(out) + "\n"



def combined_statistics_markdown(combined: dict) -> List[str]:
    out = ["", "## Absolute differences", "",
           "Differences use the metric's unit and the current Student-t interval.", "",
           "| metric | unit | session | mean difference (Student-t CI) |", "|---|---|---|---|"]
    def cell(report: dict) -> str:
        if not report:
            return "not available"
        return f"{report['diff']:+.3f} ({report['low']:+.3f} to {report['high']:+.3f})"
    labels = []
    has_new_metrics = any(row["descriptor"]["eligibility"] == "metric-validity"
                          for row in combined["rows"].values())
    for key, row in combined["rows"].items():
        for session in row["per_session"]:
            current = session["statistics"]["current"]
            diff = current.get("ab_diff") or current.get("vs_best_diff") or {}
            comparison = current.get("ab") or current.get("vs_best") or {}
            if not diff and "diff" in comparison:
                diff = {"diff": comparison["diff"], "low": comparison["diff_low"], "high": comparison["diff_high"]}
            out.append(f"| {key} | {row['descriptor']['unit']} | {session['label']} | {cell(diff)} |")
            if has_new_metrics:
                for pair in session.get("adjacent", []):
                    labels.append(f"Adjacent {key}: {pair['base']} / {pair['test']}: {pair['order']}.")
    out.append("")
    out.extend(labels)
    return out


# === Preflight, safety and metadata ==================================


def needs_build(kettle: str, kettle_b: Optional[str], no_build: bool) -> bool:
    """Build only the default target, so a run pointed at an installed app
    never builds the checkout and then measures something else."""
    return not no_build and not kettle_b and Path(kettle).resolve() == DEFAULT_KETTLE.resolve()


def in_app_bundle(path: Path) -> bool:
    return path.parent.name == "MacOS" and path.parent.parent.name == "Contents" and path.parents[2].suffix == ".app"


def require_bundles(kettle: Dict[str, str], allow_bare: bool) -> None:
    """A bare binary misses AppKit's bundle-only work (persistent UI, idle
    costs), so it does not measure what users run."""
    bare = [path for path in kettle.values() if not in_app_bundle(Path(path))]
    if bare and not allow_bare:
        raise SystemExit(
            "not inside an .app bundle: " + ", ".join(bare) + ". Copy /Applications/kettle.app, replace "
            "Contents/MacOS/kettle, run `codesign --force --deep -s -` on the copy and pass its binary, or "
            "pass --allow-bare for a diagnostic run."
        )


def bundle_kettle(binary: Path, dest: Path, template: Optional[Path]) -> Path:
    """Put `binary` inside an ad-hoc signed app bundle at `dest`: a copy of
    `template` (the installed app, so resources match what users run) when
    given, otherwise a bundle made from packaging/macos/Info.plist. Returns
    the bundled binary."""
    if dest.is_symlink() or (dest.exists() and not dest.is_dir()):
        raise SystemExit(f"{dest} is not a bundle directory the harness can replace")
    dest = dest.parent.resolve() / dest.name
    for source in [binary.resolve()] + ([template.resolve()] if template else []):
        if source == dest or dest in source.parents or source in dest.parents:
            raise SystemExit(f"{dest} overlaps {source}; bundle somewhere else")
    # Build in a scratch directory of this run's own beside the destination,
    # and swap it in only once it is signed: a failure never leaves a half-made
    # bundle, loses the previous one, or touches another run's files.
    dest.parent.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix=f".{dest.name}.", dir=dest.parent))
    try:
        partial = scratch / dest.name
        if template:
            subprocess.run(["ditto", str(template), str(partial)], check=True)
        else:
            (partial / "Contents" / "MacOS").mkdir(parents=True)
            shutil.copy2(REPO / "packaging" / "macos" / "Info.plist", partial / "Contents" / "Info.plist")
        shutil.copy2(binary, partial / "Contents" / "MacOS" / "kettle")
        subprocess.run(["codesign", "--force", "--deep", "-s", "-", str(partial)], check=True, capture_output=True)
        previous = scratch / "previous"
        if dest.exists():
            dest.rename(previous)
        try:
            partial.rename(dest)
        except BaseException:
            if previous.exists():
                previous.rename(dest)
            raise
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    return dest / "Contents" / "MacOS" / "kettle"


def installed_template() -> Optional[Path]:
    app = Path(INSTALLED_KETTLE).parents[2]
    return app if app.exists() else None


def parse_pmset_batt(text: str) -> dict:
    source = "AC" if "'AC Power'" in text else "Battery" if "'Battery Power'" in text else "unknown"
    percent = re.search(r"(\d+)%", text)
    return {"source": source, "percent": int(percent.group(1)) if percent else None}


def power_mode(section: str) -> Optional[bool]:
    """True for Low Power Mode (`powermode 1`, or the older `lowpowermode 1`),
    None when the section does not say."""
    found = re.search(r"^\s*(?:low)?powermode\s+(\d+)\b", section, re.M)
    return found.group(1) == "1" if found else None


def parse_low_power(text: str) -> Optional[bool]:
    """Low Power Mode in the settings `pmset -g` lists as currently in use."""
    if "Currently in use:" not in text:
        return None
    current = text.split("Currently in use:", 1)[1]
    return power_mode(re.split(r"^\S.*:\s*$", current, maxsplit=1, flags=re.M)[0])


def parse_low_power_custom(text: str, source: str) -> Optional[bool]:
    """Low Power Mode from `pmset -g custom`, in the section for the current
    power source."""
    heading = {"AC": "AC Power:", "Battery": "Battery Power:"}.get(source)
    if not heading or heading not in text:
        return None
    section = text.split(heading, 1)[1]
    return power_mode(re.split(r"^\S.*:\s*$", section, maxsplit=1, flags=re.M)[0])


def parse_ioreg_locked(text: str) -> Optional[bool]:
    """Whether the console session is locked; None when ioreg shows no console
    session to judge."""
    if "IOConsoleUsers" not in text:
        return None
    return '"CGSSessionScreenIsLocked"=Yes' in text


def parse_tmutil_running(text: str) -> Optional[bool]:
    found = re.search(r"\bRunning\s*=\s*(\d+)\s*;", text)
    return found.group(1) != "0" if found else None


def parse_ps(text: str) -> List[dict]:
    """`ps -axo pid=,ppid=,%cpu=,comm=` lines."""
    procs = []
    for line in text.splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4:
            procs.append({"pid": int(fields[0]), "ppid": int(fields[1]), "cpu": float(fields[2]),
                          "path": fields[3].strip()})
    return procs


def tools_running(procs: List[dict]) -> List[dict]:
    """Every build or review tool running, by name and CPU share only."""
    return [{"name": os.path.basename(proc["path"]), "cpu": proc["cpu"]} for proc in procs
            if os.path.basename(proc["path"]) in BUSY_TOOLS]


def busy_processes(procs: List[dict]) -> List[dict]:
    return [tool for tool in tools_running(procs) if tool["cpu"] >= BUSY_CPU_PERCENT]


def ancestors(procs: List[dict], pid: int) -> List[int]:
    parent = {proc["pid"]: proc["ppid"] for proc in procs}
    chain = []
    current = parent.get(pid)
    while current and current != 1 and current not in chain:
        chain.append(current)
        current = parent.get(current)
    return chain


def host_terminal_of(procs: List[dict], field: Dict[str, str], host_pid: Optional[int]) -> Optional[dict]:
    """The --host-pid terminal as recorded in the results: pid and name only."""
    if host_pid is None:
        return None
    name = next((field[p["path"]] for p in procs if p["pid"] == host_pid and p["path"] in field), None)
    return {"pid": host_pid, "name": name}


def preflight_refusals(state: dict) -> List[str]:
    """Why a session would not count, from collected state (pure)."""
    refusals = []
    # Anything that could not be read refuses: a check that fails open would
    # let a noisy session count.
    source = state["power"]["source"]
    if source == "Battery":
        refusals.append("on battery power")
    elif source != "AC":
        refusals.append("power source unknown")
    if state["low_power"] is None:
        refusals.append("Low Power Mode unknown")
    elif state["low_power"]:
        refusals.append("Low Power Mode is on")
    if state["locked"] is None:
        refusals.append("screen lock state unknown")
    elif state["locked"]:
        refusals.append("the screen is locked")
    if state["time_machine"] is None:
        refusals.append("Time Machine state unknown")
    elif state["time_machine"]:
        refusals.append("a Time Machine backup running")
    if state["load"][0] >= LOAD_LIMIT:
        refusals.append(f"load {state['load'][0]:.2f} (limit {LOAD_LIMIT:.1f})")
    if state.get("display") is None:
        refusals.append("display mode unknown")
    if state["harness_dirty"] is None:
        refusals.append("harness state unknown (git failed)")
    elif state["harness_dirty"]:
        refusals.append("the harness has local changes")
    if state["procs"] is None:
        refusals.append("could not list processes")
        return refusals
    busy = busy_processes(state["procs"])
    if busy:
        refusals.append(", ".join(sorted({proc["name"] for proc in busy})) + " running")
    running = [(proc["pid"], state["field"][proc["path"]]) for proc in state["procs"] if proc["path"] in state["field"]]
    host = state.get("host_pid")
    if host is not None:
        if host not in [pid for pid, _ in running]:
            refusals.append(f"--host-pid {host} is not a running measured terminal")
        elif host not in ancestors(state["procs"], state["self_pid"]):
            refusals.append(f"--host-pid {host} is not an ancestor of this harness")
    for pid, name in running:
        if pid != host:
            refusals.append(f"field terminal already running: {name} (pid {pid})")
    return refusals


def command(argv: List[str]) -> str:
    return subprocess.run(argv, capture_output=True, text=True).stdout


def checked(argv: List[str]) -> Optional[str]:
    """A command's output, or None if it failed or printed nothing."""
    try:
        done = subprocess.run(argv, capture_output=True, text=True)
    except OSError:
        return None
    return done.stdout if done.returncode == 0 and done.stdout.strip() else None


# Files under scripts/perf that no session runs or reads: the docs, the
# self-tests, their fixtures and the other perf tools. Everything else there,
# whatever its name or kind (a module Python could load before the harness's
# first line, bytecode, a symlink, a submodule), is part of the harness
# version, so a merge that changes only these keeps a session set open, and a
# file nobody listed counts.
INERT = frozenset({
    "scripts/perf/README.md",
    "scripts/perf/kettle-live-probes.py",
    "scripts/perf/linux-compare.sh",
    "scripts/perf/macos-compare-score-self-test.py",
    "scripts/perf/macos-compare.sh",
    "scripts/perf/macos-standing-self-test.py",
})


def runs_in_a_session(path: str) -> bool:
    """Whether a path under scripts/perf is part of the harness version.
    Python's bytecode cache is not: it loads a file from __pycache__ only
    for a source beside it, which counts. Nor is Finder's .DS_Store."""
    if "/__pycache__/" in path or path.endswith("/__pycache__") or path.rpartition("/")[2] == ".DS_Store":
        return False
    return not (path in INERT or (path.startswith("scripts/perf/macos-standing/") and path.endswith(".fixture")))


def git_blob_id(data: bytes, algorithm: str = "sha1") -> str:
    """The object id Git gives a file with these bytes, in a repository
    whose object format is `algorithm` (sha1 or sha256)."""
    return hashlib.new(algorithm, b"blob %d\0" % len(data) + data).hexdigest()


def git_file_id(path: str, expected: os.stat_result, algorithm: str = "sha1") -> Tuple[str, str]:
    """(mode, object id) of the regular file at `path`, which lstat showed
    as `expected`, hashed in bounded chunks. Opening neither follows a link
    nor waits on a writer, and a file replaced or resized while it is read
    raises OSError."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (expected.st_dev, expected.st_ino):
            raise OSError(f"{path} changed while it was read")
        digest = hashlib.new(algorithm, b"blob %d\0" % opened.st_size)
        remaining = opened.st_size
        while True:
            chunk = os.read(descriptor, 1 << 20)
            if not chunk:
                break
            digest.update(chunk)
            remaining -= len(chunk)
        if remaining != 0:
            raise OSError(f"{path} changed while it was read")
    finally:
        os.close(descriptor)
    return ("100755" if opened.st_mode & 0o100 else "100644"), digest.hexdigest()


def _raise(error: OSError) -> None:
    raise error


def working_entries(repo: Path, algorithm: str = "sha1") -> Optional[Dict[str, Tuple[str, str]]]:
    """Every file a session runs, as it is on disk: path to (mode, object id)
    in Git's terms, a symlink by its target. It reads the directory itself,
    so no Git index flag (assume-unchanged, skip-worktree), ignore rule or
    sparse checkout can hide one. A FIFO, socket or device, which Git cannot
    hold, is listed as such and so differs from any commit. None when any
    directory or file cannot be read."""
    entries: Dict[str, Tuple[str, str]] = {}
    try:
        for directory, subdirectories, files in os.walk(repo / "scripts" / "perf", onerror=_raise,
                                                       followlinks=False):
            # A symlinked directory is an entry of its own.
            for name in list(subdirectories):
                if os.path.islink(os.path.join(directory, name)):
                    subdirectories.remove(name)
                    files.append(name)
            for name in files:
                full = os.path.join(directory, name)
                path = Path(full).relative_to(repo).as_posix()
                if not runs_in_a_session(path):
                    continue
                info = os.lstat(full)
                if stat.S_ISLNK(info.st_mode):
                    entries[path] = ("120000", git_blob_id(os.fsencode(os.readlink(full)), algorithm))
                elif stat.S_ISREG(info.st_mode):
                    entries[path] = git_file_id(full, info, algorithm)
                else:
                    entries[path] = ("special", stat.filemode(info.st_mode))
    except OSError:
        return None
    return entries


def harness_revision(repo: Path = REPO) -> dict:
    """A hash of what a session runs: every file under scripts/perf but the
    INERT ones, by path, mode and object id, taken from the files on disk.
    And whether that differs from HEAD, which a session refuses: an edit, an
    untracked or ignored file, an index flag hiding a change."""
    try:
        listed = subprocess.run(["git", "-C", str(repo), "ls-tree", "-r", "-z", "HEAD", "--", "scripts/perf"],
                                capture_output=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        listed = None
    committed: Optional[Dict[str, Tuple[str, str]]] = None
    if listed is not None and listed.returncode == 0:
        committed = {}
        for record in listed.stdout.split(b"\0"):
            if not record:
                continue
            meta, _, raw_path = record.partition(b"\t")
            path = raw_path.decode("utf-8", "surrogateescape")
            mode, _, oid = meta.decode().split(" ")
            if runs_in_a_session(path):
                committed[path] = (mode, oid)
    # The ids HEAD lists tell the repository's object format.
    algorithm = "sha256" if committed and any(len(oid) == 64 for _, oid in committed.values()) else "sha1"
    entries = working_entries(repo, algorithm)
    if not entries:
        return {"harness_tree": None, "harness_dirty": None}
    tree = hashlib.sha256("\0".join(f"{mode} {oid}\t{path}" for path, (mode, oid) in sorted(entries.items()))
                          .encode("utf-8", "surrogateescape")).hexdigest()
    # Without HEAD's list the state is unknown, which refuses.
    return {"harness_tree": tree, "harness_dirty": None if committed is None else committed != entries}


def collect_preflight(field: Dict[str, str], host_pid: Optional[int], wait_quiet: float) -> dict:
    """Gather preflight state, waiting up to `wait_quiet` minutes for load to
    fall under the limit and busy tools to go quiet first."""
    deadline = time.monotonic() + wait_quiet * 60

    def noisy() -> bool:
        listing = checked(["ps", "-axo", "pid=,ppid=,%cpu=,comm="])
        return os.getloadavg()[0] >= LOAD_LIMIT or bool(listing and busy_processes(parse_ps(listing)))

    while time.monotonic() < deadline and noisy():
        time.sleep(30)
    ps = checked(["ps", "-axo", "pid=,ppid=,%cpu=,comm="])
    power = parse_pmset_batt(checked(["pmset", "-g", "batt"]) or "")
    low_power = parse_low_power(checked(["pmset", "-g"]) or "")
    if low_power is None:
        low_power = parse_low_power_custom(checked(["pmset", "-g", "custom"]) or "", power["source"])
    ioreg = checked(["ioreg", "-n", "Root", "-d1"])
    tmutil = checked(["tmutil", "status"])
    return {
        "procs": parse_ps(ps) if ps else None,
        "self_pid": os.getpid(), "host_pid": host_pid, "field": field,
        "power": power, "low_power": low_power,
        "locked": parse_ioreg_locked(ioreg) if ioreg else None,
        "time_machine": parse_tmutil_running(tmutil) if tmutil else None,
        "load": list(os.getloadavg()),
        "harness_dirty": harness_revision()["harness_dirty"],
        "display": display_mode(),
    }


def terminal_identity(path: str) -> tuple:
    """(public, local) identity of a measured binary. The public half (version,
    sha256, CDHash) tells whether a terminal changed within a session set; the
    path and signing team stay in a local-only manifest."""
    binary = Path(path)
    identity: dict = {}
    if in_app_bundle(binary):
        try:
            with (binary.parents[1] / "Info.plist").open("rb") as plist:
                info = plistlib.load(plist)
            identity["version"] = info.get("CFBundleShortVersionString")
            identity["build"] = info.get("CFBundleVersion")
        except (OSError, plistlib.InvalidFileException):
            pass
    digest = hashlib.sha256()
    with binary.open("rb") as data:
        for block in iter(lambda: data.read(1 << 20), b""):
            digest.update(block)
    identity["sha256"] = digest.hexdigest()
    # codesign prints CDHash only from the third level of verbosity.
    signature = parse_codesign(
        subprocess.run(["codesign", "-dvvv", str(binary)], capture_output=True, text=True).stderr)
    identity["cdhash"] = signature["cdhash"]
    return identity, {"path": path, "teamidentifier": signature["teamidentifier"]}


def parse_codesign(text: str) -> dict:
    """CDHash and TeamIdentifier from `codesign -dvvv`; an unset team is None."""
    fields = {}
    for key in ("CDHash", "TeamIdentifier"):
        found = re.search(rf"^{key}=(.+)$", text, re.M)
        value = found.group(1).strip() if found else None
        fields[key.lower()] = None if value == "not set" else value
    return fields


def parse_display(data: dict) -> Optional[str]:
    """The main display's pixels, its size in points (which gives the scale)
    and its refresh rate from `system_profiler SPDisplaysDataType -json`.
    Nothing that identifies the panel."""
    for gpu in data.get("SPDisplaysDataType", []):
        for display in gpu.get("spdisplays_ndrvs", []):
            if display.get("spdisplays_main") != "spdisplays_yes":
                continue
            pixels = display.get("_spdisplays_pixels")
            mode = display.get("_spdisplays_resolution") or display.get("spdisplays_resolution") or ""
            points, _, refresh = mode.partition("@")
            if pixels and points.strip() and refresh.strip():
                return f"{pixels} px, {points.strip()} pt @ {refresh.strip()}"
    return None


def display_mode() -> Optional[str]:
    try:
        return parse_display(json.loads(checked(["system_profiler", "SPDisplaysDataType", "-json"]) or "{}"))
    except json.JSONDecodeError:
        return None


class Recorder:
    """Keeps results.json current: an aborted session keeps every row so far."""

    def __init__(self, path: Path, results: dict):
        self.path = path
        self.results = results

    def write(self) -> None:
        partial = self.path.with_name(self.path.name + ".partial")
        partial.write_text(dumps(self.results))
        os.replace(partial, self.path)


def config_record(configs: Dict[str, str]) -> tuple:
    """(public, local) records of each Kettle entry's extra config: a digest
    for results.json, the text itself only in the local manifest, since a
    config line can carry paths or other private values."""
    public = {name: "sha256:" + hashlib.sha256(text.encode()).hexdigest() if text else ""
              for name, text in configs.items()}
    return public, dict(configs)


def is_ab(kettle: Dict[str, str]) -> bool:
    """A run is an A/B whenever it has a B side, even one that differs from
    A only by config."""
    return "kettle-a" in kettle and "kettle-b" in kettle and "kettle" not in kettle


def variant_name(label: str) -> str:
    """The entry name for --kettle-variant LABEL; the A/B sides' names are
    reserved so a variant can never pass for one."""
    if (not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,63}", label) or label.lower() in ("a", "b")):
        raise ValueError("--kettle-variant requires a nonreserved ASCII role name")
    return f"kettle-{label}"


def default_out_dir(root: Path) -> Path:
    return root / datetime.datetime.now().strftime("%Y-%m-%d-%H%M%S")


def claim_out_dir(path: Path) -> Path:
    """A session directory of its own: never one that already holds results.

    Returns the canonical path, so a parent symlink retargeted later cannot
    send recording, the sealed config closure or a launch to another
    directory than the one the closure checks."""
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        path.mkdir()
    except FileExistsError:
        raise SystemExit(f"{path} already exists; each session needs a new --out-dir") from None
    resolved = path.resolve(strict=True)
    if not resolved.is_dir() or resolved.is_symlink() or any(resolved.iterdir()):
        raise SystemExit(f"{path} changed while it was claimed")
    return resolved


def select_latency_workloads(workloads: List[str], payload: str, exit_logs: bool) -> List[str]:
    selected = ["latency-cursor" if w == "latency" and payload == "cursor" else w for w in workloads]
    if len(selected) != len(set(selected)):
        raise ValueError("duplicate workload selection")
    if exit_logs and "latency-cursor" not in selected:
        raise ValueError("--cursor-exit-logs requires latency-cursor")
    return selected


def resolve_rounds(args: argparse.Namespace) -> Dict[str, int]:
    if args.rounds:
        selected = [w for w in ("output-memory", "blink-window", "latency-cursor") if w in getattr(args, "workloads", "") or (w == "blink-window" and getattr(args, "blink_validate_only", False))]
        return {workload: args.rounds for workload in WORKLOADS + ("latency",) + tuple(selected)}
    return {"startup": args.startup_rounds, "idle": args.idle_rounds, "flood-memory": args.flood_rounds,
            "vtebench": args.vtebench_rounds, "latency": args.latency_rounds,
            **({"latency-cursor": args.cursor_rounds} if "latency-cursor" in getattr(args, "workloads", "") else {}),
            **({"output-memory": args.output_memory_rounds} if "output-memory" in getattr(args, "workloads", "") else {}),
            **({"blink-window": args.blink_rounds} if "blink-window" in getattr(args, "workloads", "") or getattr(args, "blink_validate_only", False) else {})}


def workload_entries(workload: str, names: List[str], latency_names: List[str], cursor_names: List[str]) -> List[str]:
    return cursor_names if workload == "latency-cursor" else latency_names if workload == "latency" else names


def latency_entries(names: List[str], ab: bool, opaque: bool, floors: List[str], payload: str = "block") -> List[str]:
    """The latency rotation: the session's terminals, then, in a standing
    session, Kettle's opaque variant and the floors, which are published
    beside them and never ranked. An A/B compares its two builds only."""
    if payload == "cursor":
        return [name for name in names if name == "kettle" or name in ("kettle-a", "kettle-b")]
    if ab:
        return list(names)
    return names + (["kettle-opaque"] if opaque else []) + [f"floor-{mode}" for mode in floors]


def run_latency_check(tools: Path, identity: Optional[str], rebuild: bool = False) -> int:
    """--latency-check: build the probe, ask macOS for its grants, report."""
    with probe_lock(tools):
        probes = build_probes(tools, latency=True, sign_identity=identity, rebuild_latency=rebuild)
        if rebuild:
            print("latency probe prepared and verified; ad-hoc grants may need renewal")
        with tempfile.TemporaryDirectory(prefix="kettle-latency-check-") as tmp:
            grants = latency_grants(probes["latency-probe"], Path(tmp), request=True)
            grants = latency_grants(probes["latency-probe"], Path(tmp))
        for grant, held in grants.items():
            print(f"{grant.replace('_', ' ')}: {'granted' if held else 'missing'}")
        if not all(grants.values()):
            print("grant both to KettleLatencyProbe in System Settings > Privacy & Security, "
                  "then run --latency-check again", file=sys.stderr)
            return 3
        return latency_probe_self_test(probes["latency-probe"])


def run_combine(args: argparse.Namespace) -> int:
    combined = combine([Path(folder) for folder in args.combine], Path(args.aa) if args.aa else None)
    out_dir = claim_out_dir(Path(args.out_dir) if args.out_dir else
                            default_out_dir(REPO / "target" / "perf-results" / "combined"))
    markdown = combined.pop("markdown")
    (out_dir / "combined.json").write_text(dumps(combined))
    (out_dir / "combined.md").write_text(markdown)
    if combined.get("publication_contract"):
        (out_dir / "aa-coverage.json").write_text(publication.canonical(combined["aa_coverage"]))
        (out_dir / "publication-values.json").write_text(publication.canonical(publication.publication_values(combined)))
    print(markdown)
    return 0


def start_caffeinate(cleanup: contextlib.ExitStack) -> subprocess.Popen:
    process = subprocess.Popen(["caffeinate", "-dimsu", "-w", str(os.getpid())])
    try:
        cleanup.callback(reap_owned_child, process, 0)
    except BaseException:
        reap_owned_child(process, 0)
        raise
    return process


def configure_observer_pilot(parser, args, argv):
    explicit = {item.split("=", 1)[0] for item in argv if item.startswith("--")}
    if args.observer_pairs < 2:
        parser.error("--observer-pairs must be at least 2")
    if not args.observer_pilot:
        if "--observer-pairs" in explicit:
            parser.error("--observer-pairs requires --observer-pilot")
        return
    workload = {"typing": "latency", "printing": "output-memory", "blink": "blink-window"}[args.observer_pilot]
    if (args.kettle_b or args.kettle_b_config or args.kettle_variant or args.latency_payload != "block"
            or args.combine or args.aa or args.observer_control or args.make_bundle or args.latency_check
            or args.preflight_only or args.rebuild_latency_probe or args.blink_validate_only
            or args.startup_phases or args.cursor_exit_logs
            or any((args.startup_input, args.startup_phase_input, args.native_layer_input, args.trace_input))):
        parser.error("observer pilot is a standalone peer diagnostic with block payload")
    if "--workloads" in explicit and args.workloads != workload:
        parser.error("observer pilot requires workload " + workload)
    round_flags = {"--latency-rounds": args.latency_rounds,
                   "--output-memory-rounds": args.output_memory_rounds, "--blink-rounds": args.blink_rounds,
                   "--startup-rounds": args.startup_rounds, "--idle-rounds": args.idle_rounds,
                   "--flood-rounds": args.flood_rounds, "--vtebench-rounds": args.vtebench_rounds,
                   "--cursor-rounds": args.cursor_rounds}
    if args.rounds is not None and args.rounds != args.observer_pairs * 2:
        parser.error("observer pilot --rounds must equal twice --observer-pairs")
    if any(flag in explicit for flag in round_flags):
        parser.error("observer pilot uses --observer-pairs, not per-workload rounds")
    args.workloads, args.rounds = workload, args.observer_pairs * 2
    args.latency_floors, args.latency_kettle_opaque = "", False


def observer_plan(entries, pairs):
    for pair in range(pairs):
        for name in rotated(entries, pair):
            for order, arm in enumerate(("on", "off") if pair % 2 == 0 else ("off", "on")):
                yield name, arm, pair, order, SEED * 1000 + pair


def run_observer_pilot(results, entries, pairs, collect, closure, recorder, work, out_dir):
    workload = next(iter(results["meta"]["rounds"]))
    rows = results["workloads"][workload] = {name: [] for name in entries}
    for name, arm, pair, order, seed in observer_plan(entries, pairs):
        timeline = work / ("typing-memory.jsonl" if workload == "latency" else "hc-timeline.jsonl")
        for stale in (work / "stamp", work / "grid", timeline, Path(str(timeline) + ".self.json")):
            stale.unlink(missing_ok=True)
        cancelled = None
        def attempt():
            nonlocal cancelled
            try:
                row = collect(name, arm, pair, seed)
            except Exception:
                row = {"error": "observer pilot collection failed"}
                # The cause stays private beside the session, never in a row.
                try:
                    with (out_dir / f"{workload}-{name}-p{pair}-{arm}.error.txt").open("x") as trace:
                        traceback.print_exc(file=trace)
                except OSError:
                    pass
            except (KeyboardInterrupt, SystemExit) as error:
                cancelled = error
                row = {"error": "observer pilot collection cancelled"}
            if (workload == "latency" and arm == "on" and row.get("typing_memory_reason") in
                    ("typing observer readiness missing", "typing timeline unavailable or invalid",
                     "typing launch context unavailable or invalid")):
                row["error"] = "observer pilot on arm unavailable"
            grid, initial = round_grid(work)
            row = check_grid(row, grid, initial)
            if grid is None and "error" not in row:
                row["error"] = "launch/window/grid readiness missing"
            row.update(observer_arm=arm, observer_pair=pair, observer_order=order,
                       at=datetime.datetime.now().astimezone().isoformat(timespec="seconds"))
            timeline = work / ("typing-memory.jsonl" if workload == "latency" else "hc-timeline.jsonl")
            row["observer_cost"] = hc.observer_cost(timeline, arm == "off" and workload == "latency")
            cost = row["observer_cost"]
            fields = ("cpu_ns", "wakeups", "query_count")
            if not (workload == "latency" and arm == "off") and all(cost[k] is not None for k in fields):
                try:
                    private_json(out_dir / f"{workload}-{name}-p{pair}-{arm}.self.json", {k: cost[k] for k in fields})
                except OSError:
                    pass  # The counters remain in the retained row.
            return row
        row = config_campaign_row(closure, results, recorder, attempt)
        rows[name].append(row)
        recorder.write()
        print(f"observer pilot pair {pair} {name} {arm}", flush=True)
        if cancelled is not None:
            raise cancelled
        # The same pause the ordinary loop takes between launches.
        time.sleep(1.0)


def main() -> int:
    with contextlib.ExitStack() as cleanup:
        try:
            return standing_main(cleanup)
        except RuntimeError as error:
            if not str(error).startswith("latency probe:"):
                raise
            print(str(error), file=sys.stderr)
            return 1


def standing_main(cleanup: contextlib.ExitStack) -> int:
    if len(sys.argv) == 5 and sys.argv[1] == KEEP_DEFAULT_ARG:
        return keep_default(sys.argv[2], sys.argv[3], sys.argv[4])
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--kettle", default=str(DEFAULT_KETTLE))
    parser.add_argument("--kettle-b", help="compare this Kettle build against --kettle instead of peers")
    parser.add_argument("--peers", default=",".join(APPS), help="comma list of peer terminals")
    parser.add_argument("--workloads", default=",".join(WORKLOADS))
    parser.add_argument("--rounds", type=int, help="rounds for every workload, overriding the per-workload counts")
    parser.add_argument("--startup-rounds", type=int, default=ROUNDS["startup"])
    parser.add_argument("--idle-rounds", type=int, default=ROUNDS["idle"])
    parser.add_argument("--flood-rounds", type=int, default=ROUNDS["flood-memory"])
    parser.add_argument("--vtebench-rounds", type=int, default=ROUNDS["vtebench"])
    parser.add_argument("--latency-rounds", type=int, default=ROUNDS["latency"])
    parser.add_argument("--output-memory-rounds", type=int, default=10,
                        help="opt-in paced printing rounds, 80 lines over eight seconds")
    parser.add_argument("--memory-sample-ms", type=int, default=100,
                        help="optional printing/blink cadence, 50..1000 ms; nondefault is diagnostic")
    parser.add_argument("--blink-rounds", type=int, default=10, help="opt-in launch blink-window rounds")
    parser.add_argument("--blink-settle", type=float, default=2.5, help="blink boundary seconds from launch")
    parser.add_argument("--blink-window", type=float, default=6.0, help="blink observation seconds")
    parser.add_argument("--blink-validate-only", action="store_true",
                        help="separate cursor-area capture, no keys or performance session")
    parser.add_argument("--blink-validation", action="append", metavar="FILE",
                        help="same binary/config/display validation JSON; repeat once per A/B side")
    parser.add_argument("--blink-validation-before", action="append", metavar="FILE",
                        help="prior validation JSON to link a post-set check; repeat once per A/B side")
    parser.add_argument("--blink-cursor-rect", help="validation crop x,y,width,height in window points")
    parser.add_argument("--blink-shape", help="shipped cursor shape recorded by the owner pilot")
    parser.add_argument("--blink-timeout", type=float, help="shipped blink timeout seconds, 0 for none")
    parser.add_argument("--blink-disabled-default", default="", help="comma list of peers with shipped blink disabled")
    parser.add_argument("--cursor-rounds", type=int, default=10)
    parser.add_argument("--latency-payload", choices=("block", "cursor"), default="block",
                        help="map selected latency to the Kettle-only cursor diagnostic")
    parser.add_argument("--latency-gap-ms", help="MIN:MAX; block 100:300, cursor 2000:2400")
    parser.add_argument("--latency-first-gap-ms", type=int, help="initial stream delay; block 0, cursor 2000")
    parser.add_argument("--cursor-exit-logs", action="store_true", help="capture optional C2 complete exit evidence")
    parser.add_argument("--latency-keys", type=int, default=100, help="measured keys per latency round")
    parser.add_argument("--latency-warmup", type=int, default=20, help="discarded keys before them")
    parser.add_argument("--latency-censor-ms", type=int, default=500,
                        help="a key with no flipped frame by then counts at this bound")
    parser.add_argument("--latency-floors", default="ca,metal-sync,metal-nosync",
                        help=f"bare-window floors to report, of {','.join(LATENCY_FLOORS)} (empty for none)")
    parser.add_argument("--latency-kettle-opaque", action=argparse.BooleanOptionalAction, default=True,
                        help="also measure Kettle opaque and unblurred, unranked (standing sessions only)")
    parser.add_argument("--latency-inject", choices=("hid", "pid"), default="hid",
                        help="post keys at the HID tap (default) or to the terminal's pid (pilot comparison)")
    parser.add_argument("--latency-sign-identity",
                        help="codesign identity for KettleLatencyProbe.app, so its grants survive rebuilds")
    parser.add_argument("--latency-check", action="store_true",
                        help="verify the latency probe, ask for its grants, report them and exit")
    parser.add_argument("--rebuild-latency-probe", action="store_true",
                        help="with --latency-check only: explicitly prepare a fresh verified probe")
    parser.add_argument("--vtebench-seconds", type=int, default=10, help="seconds per benchmark (upstream's default)")
    parser.add_argument("--idle-settle", type=float, default=20.0)
    parser.add_argument("--idle-window", type=float, default=30.0)
    parser.add_argument("--fd-limit", type=int, default=256,
                        help="soft RLIMIT_NOFILE the terminals inherit; 256 is what the Dock gives apps, 0 inherits")
    parser.add_argument("--out-dir", help="a new directory for this session's results "
                        "(default: target/perf-results/macos-standing/<date-time>)")
    parser.add_argument("--label", help="session label recorded in results.json")
    parser.add_argument("--no-build", action="store_true", help="use --kettle as built")
    parser.add_argument("--allow-bare", action="store_true", help="measure a Kettle binary outside an .app bundle")
    parser.add_argument("--allow-noisy", action="store_true",
                        help="run despite preflight refusals; the session is marked not countable")
    parser.add_argument("--preflight-only", action="store_true", help="run the preflight and exit")
    parser.add_argument("--host-pid", type=int,
                        help="the measured terminal hosting this shell, allowed to stay open (must be an ancestor)")
    parser.add_argument("--wait-quiet", type=float, default=30.0,
                        help="minutes to wait for load under the limit before the preflight decides")
    parser.add_argument("--startup-phases", nargs="?", const="all", choices=("all", "b"),
                        help="record Kettle's startup phase stamps in startup rounds, for every Kettle entry "
                             "or only the A/B's B side (a diagnostic: the session never counts)")
    parser.add_argument("--warmup", type=int, default=1,
                        help="discarded launches per terminal before the startup rounds")
    parser.add_argument("--flood-offsets", default="3,20",
                        help="seconds after the flood ends at which memory columns are read")
    parser.add_argument("--no-activate", action="store_true",
                        help="leave idle and flood windows unfocused instead of bringing each to the front")
    parser.add_argument("--footprint-detail", action="store_true",
                        help="record `footprint` graphics categories at each flood offset (diagnostic sessions only)")
    parser.add_argument("--kettle-b-config", action="append", default=[], metavar="LINE",
                        help="config line for kettle-b only; without --kettle-b, B is the same binary as A")
    parser.add_argument("--kettle-variant", action="append", default=[], metavar="NAME=LINES",
                        help="an unranked Kettle entry kettle-NAME with extra config lines (separate with ';')")
    parser.add_argument("--make-bundle", nargs=2, metavar=("BINARY", "APP"),
                        help="put a Kettle binary in an ad-hoc signed copy of the installed app and exit")
    parser.add_argument("--combine", nargs="+", metavar="DIR", help="merge session directories and exit")
    parser.add_argument("--aa", metavar="DIR", help="with --combine: ordinary shared A/A, checked metric by metric")
    parser.add_argument("--observer-pilot", choices=("typing", "printing", "blink"),
                        help="diagnostic paired observer on/off launches; never standings or A/A")
    parser.add_argument("--observer-pairs", type=int, default=10,
                        help="observer pilot pairs per terminal (default 10, minimum 2)")
    parser.add_argument("--observer-control", metavar="DIR",
                        help="analyze an observer pilot or complete 30-pair stamp diagnostic; launch nothing")
    parser.add_argument("--startup-input", nargs="+", metavar="FILE",
                        help="postprocess retained startup results JSON; launch no app")
    parser.add_argument("--startup-grid-policy", choices=("settled", "child", "native"),
                        default="settled", help="policy for --startup-input only")
    parser.add_argument("--startup-phase-input", nargs="+", metavar="FILE",
                        help="postprocess retained S1/S2 startup stderr, one file per round")
    parser.add_argument("--startup-started-ns", type=int,
                        help="optional launch clock origin for a single --startup-phase-input")
    parser.add_argument("--startup-native-log", metavar="FILE",
                        help="join one private native_pty log to a single retained startup row")
    parser.add_argument("--startup-child-observation", metavar="FILE",
                        help="private shell-wrapper JSON for --startup-native-log")
    parser.add_argument("--native-layer-input", nargs="+", metavar="FILE",
                        help="certify retained native cursor layer analysis.json raw evidence")
    parser.add_argument("--native-layer-max-footprint-mib", type=float, default=80.0,
                        help="native layer peak threshold in MiB (default: 80)")
    parser.add_argument("--native-layer-max-wakeups", type=float, default=0.5,
                        help="native layer wakeups/s threshold (default: 0.5)")
    parser.add_argument("--native-layer-max-cpu-percent", type=float, default=0.02,
                        help="native layer CPU percent threshold (default: 0.02)")
    parser.add_argument("--trace-input", nargs="+", metavar="FILE",
                        help="report retained private echo trace capability; launch no app")
    args = parser.parse_args()
    configure_observer_pilot(parser, args, sys.argv[1:])
    if args.observer_control:
        if args.combine or args.aa or args.make_bundle or args.latency_check or args.preflight_only or args.rebuild_latency_probe or any((args.startup_input, args.startup_phase_input, args.native_layer_input, args.trace_input)):
            parser.error("--observer-control is standalone postprocessing")
        try:
            report = publication.observer_control(_publication_host(), Path(args.observer_control))
        except (ValueError, OSError) as error:
            parser.error(str(error))
        out = claim_out_dir(Path(args.out_dir) if args.out_dir else default_out_dir(REPO / "target" / "perf-results" / "observer-control"))
        (out / "observer-equivalence.json").write_text(publication.canonical(report))
        return 0 if report.get("equivalent", report.get("phase_attribution_allowed", False)) else 1
    if args.aa and not args.combine:
        parser.error("--aa requires --combine")
    diagnostic = any((args.startup_input, args.startup_phase_input,
                      args.native_layer_input, args.trace_input, args.startup_native_log, args.startup_child_observation))
    if diagnostic:
        if args.combine or args.make_bundle or args.latency_check or args.preflight_only or args.rebuild_latency_probe:
            parser.error("evidence postprocessing cannot be combined with execution modes")
        if args.startup_started_ns is not None and (not args.startup_phase_input or len(args.startup_phase_input) != 1 or not evidence_uint(args.startup_started_ns)):
            parser.error("--startup-started-ns needs one phase input and an unsigned clock value")
        return run_evidence_postprocessing(args)
    if args.startup_grid_policy != "settled" or args.startup_started_ns is not None:
        parser.error("startup evidence options require a postprocessing input")
    # Practical bounds, checked before anything runs: a run stays finite, and
    # the probe's nanosecond arithmetic cannot overflow.
    for flag, value, least, most in (("--latency-keys", args.latency_keys, 1, 1000),
                                     ("--latency-rounds", args.latency_rounds, 1, 100),
                                     ("--cursor-rounds", args.cursor_rounds, 1, 100),
                                     ("--latency-censor-ms", args.latency_censor_ms, 1, 5000),
                                     ("--latency-warmup", args.latency_warmup, 0, 200),
                                     ("--rounds", args.rounds, 1, 1000)):
        if value is None or (flag == "--rounds" and args.observer_pilot):
            continue
        if value < least:
            parser.error(f"{flag} must be at least {least}")
        if value > most:
            parser.error(f"{flag} must be at most {most}")

    if args.rebuild_latency_probe and (not args.latency_check or args.combine or args.make_bundle or args.preflight_only):
        parser.error("--rebuild-latency-probe requires --latency-check alone")
    for flag, value, low, high in (("--memory-sample-ms", args.memory_sample_ms, 50, 1000),
                                  ("--output-memory-rounds", args.output_memory_rounds, 1, 100),
                                  ("--blink-rounds", args.blink_rounds, 1, 100),
                                  ("--blink-settle", args.blink_settle, .1, 10),
                                  ("--blink-window", args.blink_window, .1, 20)):
        if not math.isfinite(value) or not low <= value <= high:
            parser.error(f"{flag} must be finite and in {low}..{high}")
    if args.blink_timeout is not None and (not math.isfinite(args.blink_timeout) or args.blink_timeout < 0):
        parser.error("--blink-timeout must be finite and nonnegative")
    if args.blink_cursor_rect:
        try:
            rect = [float(v) for v in args.blink_cursor_rect.split(",")]
            if len(rect) != 4 or not all(math.isfinite(v) for v in rect) or min(rect[:2]) < 0 or min(rect[2:]) <= 0 or max(rect[2:]) > 256:
                raise ValueError()
        except ValueError:
            parser.error("--blink-cursor-rect requires finite x,y,width,height")
    if args.blink_validate_only and (not args.blink_cursor_rect or not args.blink_shape or args.blink_timeout is None):
        parser.error("--blink-validate-only requires --blink-cursor-rect, --blink-shape and --blink-timeout")
    if args.blink_validation_before and not args.blink_validate_only:
        parser.error("--blink-validation-before requires --blink-validate-only")
    # A mistyped name would otherwise leave its side quietly unproven.
    for flag, paths in (("--blink-validation", args.blink_validation),
                        ("--blink-validation-before", args.blink_validation_before)):
        for path in paths or []:
            try:
                info = Path(path).stat()
                if stat.S_ISREG(info.st_mode) and info.st_size <= 1024 * 1024:
                    with open(path, "rb") as stream:
                        stream.read(1024 * 1024 + 1)
            except OSError:
                parser.error(f"{flag} file is unreadable")
            if not stat.S_ISREG(info.st_mode) or info.st_size > 1024 * 1024:
                parser.error(f"{flag} must name a regular file of at most 1 MiB")
    if args.blink_validate_only and (args.combine or args.latency_check or args.make_bundle):
        parser.error("blink validation is a separate preparation invocation")
    if args.combine:
        return run_combine(args)
    if args.make_bundle:
        print(bundle_kettle(Path(args.make_bundle[0]), Path(args.make_bundle[1]), installed_template()))
        return 0
    if sys.platform != "darwin":
        print("macos-standing.py: this benchmark requires macOS", file=sys.stderr)
        return 1
    if args.latency_check:
        return run_latency_check(REPO / "target" / "perf-tools" / "macos-standing", args.latency_sign_identity, args.rebuild_latency_probe)
    workloads = [w for w in args.workloads.split(",") if w]
    if args.blink_validate_only:
        workloads = ["blink-window"]
    unknown = set(workloads) - set(WORKLOADS) - set(OPT_IN_WORKLOADS)
    if unknown:
        parser.error(f"unknown workloads: {', '.join(sorted(unknown))}")
    try:
        workloads = select_latency_workloads(workloads, args.latency_payload, args.cursor_exit_logs)
        methods = {mode: cursor.method("cursor" if mode == "latency-cursor" else "block",
                     args.latency_gap_ms, args.latency_first_gap_ms)
                   for mode in workloads if mode in ("latency", "latency-cursor")}
    except ValueError as error:
        parser.error(str(error))
    args.workloads = ",".join(workloads)
    floors = [f for f in args.latency_floors.split(",") if f]
    if set(floors) - set(LATENCY_FLOORS):
        parser.error(f"unknown latency floors: {', '.join(sorted(set(floors) - set(LATENCY_FLOORS)))}")
    rounds = resolve_rounds(args)
    if args.observer_pilot:
        rounds = {workloads[0]: args.observer_pairs * 2}
    offsets = [float(value) for value in args.flood_offsets.split(",") if value]
    unranked: List[str] = []

    if args.kettle_b or args.kettle_b_config:
        kettle = {"kettle-a": args.kettle, "kettle-b": args.kettle_b or args.kettle}
        names = ["kettle-a", "kettle-b"]
        kettle_configs = {"kettle-a": "", "kettle-b": "\n".join(args.kettle_b_config)}
        skipped = {}
    else:
        kettle = {"kettle": args.kettle}
        names = ["kettle"]
        kettle_configs = {"kettle": ""}
        for variant in args.kettle_variant:
            label, _, lines = variant.partition("=")
            if not lines:
                parser.error("--kettle-variant needs NAME=LINES")
            try:
                name = variant_name(label)
            except ValueError as error:
                parser.error(str(error))
            kettle[name] = args.kettle
            kettle_configs[name] = "\n".join(line.strip() for line in lines.split(";"))
            names.append(name)
            unranked.append(name)
        skipped = {}
        for peer in [p for p in args.peers.split(",") if p]:
            if peer not in APPS:
                parser.error(f"unknown peer: {peer}")
            if Path(APPS[peer]).exists():
                names.append(peer)
            else:
                skipped[peer] = f"not installed at {APPS[peer]}"
    try:
        stamped = stamped_entries(args.startup_phases, kettle)
    except ValueError as error:
        parser.error(str(error))
    latency_names = latency_entries(names, is_ab(kettle), args.latency_kettle_opaque, floors)
    cursor_names = latency_entries(names, is_ab(kettle), False, [], "cursor")
    if "latency" in workloads and "kettle-opaque" in latency_names:
        kettle["kettle-opaque"] = kettle["kettle"]
        kettle_configs["kettle-opaque"] = KETTLE_OPAQUE
    try:
        for extra in kettle_configs.values():
            config_entries(extra)
    except ConfigClosureError as error:
        parser.error(str(error))
    if "latency" in workloads:
        unranked.extend(name for name in latency_names if name not in names)
    for workload in workloads:
        if rounds[workload] % len(names):
            print(f"note: {rounds[workload]} {workload} rounds do not balance a {len(names)}-entry rotation",
                  file=sys.stderr)

    def field_terminals() -> Dict[str, str]:
        field = {path: name for name, path in APPS.items()}
        field[INSTALLED_KETTLE] = "kettle"
        for name, path in kettle.items():
            field[str(Path(path).resolve())] = name
            field[path] = name
        return field

    if args.preflight_only:
        refusals = preflight_refusals(collect_preflight(field_terminals(), args.host_pid, 0))
        for reason in refusals:
            print(f"preflight: {reason}", file=sys.stderr)
        print("preflight: " + ("refused" if refusals else "clear"))
        return 1 if refusals else 0

    tools = REPO / "target" / "perf-tools" / "macos-standing"
    if set(workloads) & {"latency", "latency-cursor"} or args.blink_validate_only:
        cleanup.enter_context(probe_lock(tools))
        validate_latency_probe(tools / "KettleLatencyProbe.app", args.latency_sign_identity)
    if needs_build(args.kettle, args.kettle_b, args.no_build):
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "kettle"], cwd=REPO, check=True)
    if not args.allow_bare and Path(args.kettle).resolve() == DEFAULT_KETTLE.resolve() and DEFAULT_KETTLE.exists():
        # The local build is measured the way users run it: inside an app.
        local = str(bundle_kettle(DEFAULT_KETTLE, tools / "kettle-local.app", installed_template()))
        for name, path in list(kettle.items()):
            if path == args.kettle:
                kettle[name] = local
    require_bundles(kettle, args.allow_bare)
    bare = [name for name, path in kettle.items() if not in_app_bundle(Path(path))]

    # Build every tool first: compiling right before measuring adds load and
    # heat, so the preflight that decides the session runs after it.
    probes = build_probes(tools, latency=bool(set(workloads) & {"latency", "latency-cursor"}) or args.blink_validate_only, sign_identity=args.latency_sign_identity)
    if set(workloads) & {"latency", "latency-cursor", "output-memory", "blink-window"}:
        probes.update(hc.build_helpers(PROBES, tools))
    vtebench = build_vtebench(tools) if "vtebench" in workloads else None
    tool_hashes, tool_artifacts = probe_tool_identity(probes, args.latency_sign_identity)
    if set(workloads) & {"latency", "latency-cursor"}:
        with tempfile.TemporaryDirectory(prefix="kettle-latency-grants-") as tmp:
            grants = latency_grants(probes["latency-probe"], Path(tmp))
        if not all(grants.values()):
            print("latency: KettleLatencyProbe lacks " + " and ".join(
                g.replace("_", " ") for g, held in grants.items() if not held) + "; run --latency-check",
                file=sys.stderr)
            return 3
    if vtebench:
        tool_hashes["vtebench"] = file_sha256(vtebench)
    field = field_terminals()
    state = collect_preflight(field, args.host_pid, args.wait_quiet)
    refusals = preflight_refusals(state)
    for reason in refusals:
        print(f"preflight: {reason}", file=sys.stderr)
    if refusals and not args.allow_noisy:
        print("preflight refused; --allow-noisy runs anyway and marks the session not countable", file=sys.stderr)
        return 1

    if args.fd_limit:
        _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(resource.RLIMIT_NOFILE, (args.fd_limit, hard))
    # Keep the display and the machine awake for as long as this process runs.
    start_caffeinate(cleanup)

    out_dir = claim_out_dir(Path(args.out_dir) if args.out_dir else
                            default_out_dir(REPO / "target" / "perf-results" / "macos-standing"))

    host = command(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
    release = command(["sw_vers", "-productVersion"]).strip()
    load = os.getloadavg()[0]
    started = datetime.datetime.now().astimezone()
    host_terminal = host_terminal_of(state["procs"] or [], field, args.host_pid)
    results = {
        "schema": SCHEMA, "evidence_contract": EVIDENCE_CONTRACT,
        "context": f"{host}, macOS {release}, load {load:.2f} at start, rounds "
                   + ", ".join(f"{w} {rounds[w]}" for w in workloads)
                   + f", {COLS}x{ROWS} grid, default configs, fd soft limit "
                   f"{resource.getrlimit(resource.RLIMIT_NOFILE)[0]}",
        "terminals": names, "skipped": skipped, "unranked": unranked,
        "meta": {
            "statistics_policy": "current",
            "kind": "observer-pilot" if args.observer_pilot else "observer-control" if args.startup_phases == "b" and workloads == ["startup"] and is_ab(kettle) else "ordinary",
            "label": args.label or out_dir.name, "mode": "ab" if is_ab(kettle) else "standing",
            "started": started.isoformat(timespec="seconds"), "date": started.date().isoformat(),
            # countable is final only once every round has run (see
            # session_countable); an interrupted session never counts.
            "complete": False, "refusals": refusals, "bare": bool(bare), "countable": False,
            **harness_revision(), "tool_hashes": tool_hashes,
            **({"tool_artifacts": tool_artifacts} if tool_artifacts else {}),
            "hw_model": command(["sysctl", "-n", "hw.model"]).strip(), "cpu": host, "macos": release,
            "macos_build": command(["sw_vers", "-buildVersion"]).strip(), "display": state["display"],
            "power": state["power"], "low_power": state["low_power"], "load_start": state["load"],
            "tools": tools_running(state["procs"] or []), "host_terminal": host_terminal,
            "fd_limit": resource.getrlimit(resource.RLIMIT_NOFILE)[0], "rounds": rounds,
            "vtebench_seconds": args.vtebench_seconds, "vtebench_unit": "us",
            "idle_settle": args.idle_settle, "idle_window": args.idle_window, "warmup": args.warmup,
            "flood_offsets": offsets, "activate": not args.no_activate,
            "footprint_detail": args.footprint_detail, "configs": config_record(kettle_configs)[0],
            "startup_phases": args.startup_phases,
            "latency": {"keys": args.latency_keys, "warmup": args.latency_warmup,
                        "censor_ms": args.latency_censor_ms, "inject": args.latency_inject,
                        "typing_memory": hc.typing_method(args.memory_sample_ms),
                        "entries": latency_names, "signed": "identity" if args.latency_sign_identity else "ad hoc",
                        } if "latency" in workloads else None,
            "identity": {},
        },
        "workloads": {},
    }
    if "latency-cursor" in workloads:
        results["meta"]["latency-cursor"] = {"keys": args.latency_keys, "warmup": args.latency_warmup,
            "censor_ms": args.latency_censor_ms, "inject": args.latency_inject,
            "entries": cursor_names, "signed": "identity" if args.latency_sign_identity else "ad hoc",
            **methods["latency-cursor"], "exit_logs": args.cursor_exit_logs, "exit_contract": cursor.CONTRACT}
    if "latency" in workloads and (args.latency_gap_ms is not None or args.latency_first_gap_ms is not None):
        results["meta"]["latency"].update(methods["latency"])
    if args.observer_pilot:
        results["meta"]["observer_pilot"] = {"kind": args.observer_pilot, "pairs": args.observer_pairs,
                                            "bounds": publication.PILOT_BOUNDS[args.observer_pilot]}
        results["meta"]["refusals"].append("observer pilot (diagnostic)")
    local_manifest = {"configs": config_record(kettle_configs)[1]}
    if tool_artifacts:
        local_manifest["latency_probe"] = json.loads((tools / "KettleLatencyProbe.build.json").read_text())
    for name in names:
        public, local = terminal_identity(kettle.get(name) or APPS[name])
        results["meta"]["identity"][name] = public
        local_manifest[name] = local
    # Paths and signing teams identify this machine and its owner; they stay
    # beside the results and never go into anything published.
    private_json(out_dir / "local-manifest.json", local_manifest)
    recorder = Recorder(out_dir / "results.json", results)
    recorder.write()

    with config_work_directory(out_dir) as work:
        write_configs(work, kettle_configs)
        try:
            closure = ConfigClosure(work, kettle_configs, Path.cwd(), measured=names)
        except ConfigClosureError as error:
            results["meta"]["refusals"].append(str(error))
            recorder.write()
            raise SystemExit("refused: " + str(error)) from None
        results["meta"]["config_closures"] = closure.public
        local_manifest["config_closure"] = closure.local
        private_json(out_dir / "local-manifest.json", local_manifest)
        recorder.write()
        runner = Runner(probes, work, kettle)
        runner.config_closure = closure
        runner.observer_pilot = bool(args.observer_pilot)
        runner.phases = stamped
        optional_options = {"activate": not args.no_activate, "settle": args.blink_settle,
                            "window": args.blink_window, "validation": args.blink_validation,
                            "validate_only": args.blink_validate_only, "rect": args.blink_cursor_rect,
                            "before_path": args.blink_validation_before, "sample_ms": args.memory_sample_ms}
        if set(workloads) & {"output-memory", "blink-window"}:
            results["meta"]["output_blink"] = {"contract": hc.CONTRACT, "sample_ms": args.memory_sample_ms,
                "settle_s": args.blink_settle, "window_s": args.blink_window,
                "validation_only": args.blink_validate_only,
                "printing_payload_sha256": hc.PRINT_SHA256}
            if args.memory_sample_ms != 100 or args.blink_settle != 2.5 or args.blink_window != 6.0 or args.blink_validate_only:
                results["meta"]["refusals"].append("diagnostic blink interval or validation-only capture")
        if "latency" in workloads and args.memory_sample_ms != 100:
            results["meta"]["refusals"].append("diagnostic typing memory interval")
        flood = work / "flood.txt"
        if "flood-memory" in workloads:
            write_flood(flood)
        if vtebench:
            benchmarks = prepare_benchmarks(vtebench.parents[2] / "benchmarks", work / "benchmarks")
        latency_options = {"keys": args.latency_keys, "warmup": args.latency_warmup,
                           "censor_ms": args.latency_censor_ms, "inject": args.latency_inject,
                           "sample_ms": args.memory_sample_ms}
        cursor_options = {**latency_options, **methods.get("latency-cursor", {}), "exit_logs": args.cursor_exit_logs}
        if "latency" in methods and (args.latency_gap_ms is not None or args.latency_first_gap_ms is not None):
            latency_options.update(methods["latency"])
        measured = set(names) | set(latency_names if "latency" in workloads else ())
        frame = (GhosttyFrame(recovery=out_dir / "ghostty-frame-restore.txt") if "ghostty" in measured
                 else contextlib.nullcontext())
        # The terminals whose first launch showed its grid.
        launched: set = set()
        with frame:
            runner.ghostty_frame = frame if isinstance(frame, GhosttyFrame) else None
            try:
                if args.observer_pilot:
                    workload = workloads[0]
                    def pilot_collect(name, arm, pair, seed):
                        options = {"observer_arm": arm}
                        if workload == "latency":
                            return runner.latency(name, {**latency_options, **options}, seed,
                                out_dir / f"{workload}-{name}-p{pair}-{arm}.json")
                        setup = {"binary_sha256": file_sha256(Path(kettle.get(name) or APPS[name])),
                            "config_sha256": hashlib.sha256(dumps(closure.public[name]).encode()).hexdigest(),
                            "display": state["display"], "settle_s": args.blink_settle,
                            "window_s": args.blink_window, "cursor_rect": args.blink_cursor_rect,
                            "shape": args.blink_shape, "timeout_s": args.blink_timeout}
                        return hc.collect(runner, name, workload,
                            {**optional_options, **options, "disabled": name in args.blink_disabled_default.split(",")},
                            out_dir / f"{workload}-{name}-p{pair}-{arm}", setup)
                    run_observer_pilot(results, names, args.observer_pairs, pilot_collect, closure,
                                       recorder, work, out_dir)
                for workload in ([] if args.observer_pilot else workloads):
                    entries = workload_entries(workload, names, latency_names, cursor_names)
                    rows: Dict[str, List[dict]] = {name: [] for name in entries}
                    results["workloads"][workload] = rows
                    # A whole rotation of failed latency rounds in a row points at the
                    # machine (an alert over the windows, lost grants), not at a
                    # terminal: the rest of the workload is recorded as not run.
                    failures_in_a_row = 0
                    # Warm-up launches run first for every terminal, are flagged, and
                    # never enter a statistic; they absorb first-launch costs such as
                    # the payload script's one-time assessment.
                    warmups = args.warmup if workload == "startup" else 0
                    for round_index in range(1 if args.blink_validate_only else warmups + rounds[workload]):
                        for name in rotated(entries, round_index):
                            # A round that never launches (latency's "not run")
                            # must not read the previous launch's grid.
                            for stale in (work / "stamp", work / "grid"):
                                stale.unlink(missing_ok=True)
                            def collect_row():
                                nonlocal failures_in_a_row
                                if workload == "startup":
                                    row = runner.startup(name)
                                elif workload == "idle":
                                    row = runner.idle(name, args.idle_settle, args.idle_window, not args.no_activate)
                                elif workload == "flood-memory":
                                    row = runner.flood_memory(name, flood, offsets, not args.no_activate, args.footprint_detail)
                                elif workload in ("output-memory", "blink-window"):
                                    config_bytes = dumps(closure.public[name]).encode()
                                    setup = {"binary_sha256": file_sha256(Path(kettle.get(name) or APPS[name])),
                                             "config_sha256": hashlib.sha256(config_bytes).hexdigest(), "display": display_mode(),
                                             "settle_s": args.blink_settle, "window_s": args.blink_window,
                                             "cursor_rect": args.blink_cursor_rect, "shape": args.blink_shape,
                                             "timeout_s": args.blink_timeout}
                                    row = hc.collect(runner, name, workload,
                                        {**optional_options, "disabled": name in args.blink_disabled_default.split(",")},
                                        out_dir / f"{workload}-{name}-r{round_index}", setup)
                                elif workload in ("latency", "latency-cursor"):
                                    if failures_in_a_row >= len(entries):
                                        row = {"error": f"not run: {len(entries)} latency rounds in a row failed"}
                                    else:
                                        # A new gap sequence every round, the same for
                                        # every entry in it.
                                        row = runner.latency(name, cursor_options if workload == "latency-cursor" else latency_options, SEED * 1000 + round_index,
                                                             out_dir / f"{workload}-{name}-r{round_index}.json")
                                        failures_in_a_row = failures_in_a_row + 1 if "error" in row else 0
                                else:
                                    row = runner.vtebench(name, vtebench, benchmarks,
                                                          out_dir / f"{name}-r{round_index}.dat", args.vtebench_seconds)
                                return row
                            row = config_campaign_row(closure, results, recorder, collect_row)
                            if not name.startswith("floor-"):
                                row = check_grid(row, *round_grid(work))
                                reason = first_launch_refusal(name, row, launched)
                                if reason:
                                    results["meta"]["refusals"].append(reason)
                                    rows[name].append(row)
                                    recorder.write()
                                    raise SystemExit(f"refused: {reason}")
                            if round_index < warmups:
                                row["warmup"] = True
                            row["at"] = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
                            row["load"] = list(os.getloadavg()[:2])
                            rows[name].append(row)
                            recorder.write()
                            print(f"{workload} round {round_index} {name}: {json.dumps(row)}", flush=True)
                            time.sleep(1.0)
            finally:
                # A round still in flight (an interrupt, a crash) is stopped
                # before the frame goes back: a measured Ghostty closing later
                # would write its own frame over the restored one. The keeper
                # waits for it anyway (wait_for_round); this keeps that short.
                runner.stop_current()
                local_manifest["config_source_changes"] = closure.source_changes()
                private_json(out_dir / "local-manifest.json", local_manifest)

    if tool_artifacts:
        validate_latency_probe(probes["latency-probe"], args.latency_sign_identity)
    if args.blink_validate_only:
        print("blink validation artifacts written; no performance session")
        return 0 if all(not row.get("error") for runs in results["workloads"]["blink-window"].values() for row in runs) else 1
    results["meta"]["complete"] = True
    results["meta"]["countable"] = session_countable(results["meta"]) and rounds_complete(results, results["meta"])
    if args.observer_pilot:
        recorder.write()
        report = publication.observer_control(_publication_host(), out_dir)
        (out_dir / "observer-equivalence.json").write_text(publication.canonical(report))
        summary = "Observer pilot (diagnostic). Equivalent: " + str(report["equivalent"]).lower()
        (out_dir / "summary.md").write_text(summary + "\n")
        print(summary)
        return 0
    results["meta"]["workload_countable"] = workload_countable(results, results["meta"])
    analysis = analyze(results, names, is_ab(kettle))
    results["meta"]["metric_countable"] = {entry["descriptor"]["id"]: entry["metric_countable"]
                                          for info in analysis.values() for entry in info["metrics"].values()}
    recorder.write()
    (out_dir / "analysis.json").write_text(dumps({"schema": SCHEMA, "statistics_policy": "current", "workloads": analysis}))
    results["meta"]["contracts"] = publication.contracts(results["meta"], results["workloads"])
    recorder.write()
    summary = summarize(results, names, is_ab(kettle), analysis)
    (out_dir / "summary.md").write_text(summary + "\n")
    print(summary)
    return 0


def rotated(names: List[str], round_index: int) -> List[str]:
    shift = round_index % len(names)
    return names[shift:] + names[:shift]


if __name__ == "__main__":
    sys.exit(main())
