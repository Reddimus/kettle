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

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import os
import plistlib
import random
import re
import resource
import shlex
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

REPO = Path(__file__).resolve().parents[2]
PROBES = Path(__file__).resolve().parent / "macos-standing"
DEFAULT_KETTLE = REPO / "target" / "release" / "kettle"
INSTALLED_KETTLE = "/Applications/kettle.app/Contents/MacOS/kettle"
VTEBENCH_URL = "https://github.com/alacritty/vtebench"
VTEBENCH_REV = "ead80032e57dee2e75f0b51f2ea67528647d9944"
COLS, ROWS = 120, 36
FLOOD_BYTES = 32 * 1024 * 1024
SCHEMA = 2

APPS = {
    "alacritty": "/Applications/Alacritty.app/Contents/MacOS/alacritty",
    "kitty": "/Applications/kitty.app/Contents/MacOS/kitty",
    "wezterm": "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
    "ghostty": "/Applications/Ghostty.app/Contents/MacOS/ghostty",
}
WORKLOADS = ("startup", "idle", "flood-memory", "vtebench")
# Never in the default list: latency posts key presses, needs the probe's
# Screen Recording and Accessibility grants, and needs the machine to itself.
OPT_IN_WORKLOADS = ("latency",)
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
                "footprint_detail", "startup_phases", "latency")
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


def build_probes(tools: Path, latency: bool = False, sign_identity: Optional[str] = None) -> Dict[str, Path]:
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
        built["latency-probe"] = build_latency_probe(tools, sign_identity)
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


def build_latency_probe(tools: Path, identity: Optional[str]) -> Path:
    """Build and sign KettleLatencyProbe.app, only when its source or signing
    identity changed: macOS keys the probe's Screen Recording and
    Accessibility grants to its signature. Signed ad hoc, every rebuild needs
    new grants; signed with a certificate (--latency-sign-identity), the
    grants survive rebuilds."""
    app = tools / "KettleLatencyProbe.app"
    contents = app / "Contents"
    source = PROBES / "latency-probe.swift"
    wanted = {"source": file_sha256(source), "identity": identity or "-"}
    # Beside the bundle: a file added inside it after signing breaks the seal.
    record = tools / "KettleLatencyProbe.build.json"
    try:
        if json.loads(record.read_text()) == wanted and (contents / "MacOS" / "latency-probe").exists():
            return app
    except (OSError, json.JSONDecodeError):
        pass
    record.unlink(missing_ok=True)
    if app.exists():
        shutil.rmtree(app)
    (contents / "MacOS").mkdir(parents=True)
    subprocess.run(["swiftc", "-O", "-o", str(contents / "MacOS" / "latency-probe"), str(source)], check=True)
    (contents / "Info.plist").write_bytes(latency_probe_plist())
    subprocess.run(["codesign", "--force", "--sign", identity or "-", "--identifier", LATENCY_PROBE_ID, str(app)],
                   check=True, capture_output=True)
    # Written last: an interrupted build is rebuilt next time.
    record.write_text(json.dumps(wanted))
    return app


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


def run_latency_probe(app: Path, args: List[str], work: Path, timeout: float) -> str:
    """Run the probe as its own app through `open`, so macOS holds the probe,
    not whatever launched this script, responsible for its grants, and it
    never becomes frontmost. Returns its stdout. `open -W` drops the exit
    status, and returns at once when the probe exits before `open` can
    attach to it, so callers wait for the probe's output instead."""
    stdout, stderr = work / "probe.stdout", work / "probe.stderr"
    for path in (stdout, stderr):
        path.write_text("")
    subprocess.run(["open", "-g", "-n", "-W", "--stdout", str(stdout), "--stderr", str(stderr), str(app),
                    "--args", *args], check=False, timeout=timeout)
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


def terminal_argv(name: str, script: Path, work: Path, kettle: Dict[str, str]) -> List[str]:
    """How each terminal runs `script` at the pinned grid with default settings."""
    if name in kettle:
        return [kettle[name], "--config", str(work / f"{name}.config"), "-e", str(script)]
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

    def launch(self, name: str, body: str, timeout: float,
               params: Optional[Dict[str, str]] = None, phases: bool = False,
               argv: Optional[List[str]] = None) -> subprocess.Popen:
        """Start `name` running `body` after the stamp. Values that change per
        launch go in a params file the script sources, so the script stays the
        same file. `argv` replaces the terminal and its payload (the latency
        floors, which are their own windows); nothing then writes the stamp."""
        stamp = self.work / "stamp"
        # A failed launch writes no result, so nothing from the previous round
        # may be left to be read in its place.
        for stale in (stamp, Path(str(stamp) + ".pid"), self.work / "done", self.work / "launch.json"):
            stale.unlink(missing_ok=True)
        params_file = self.work / "params"
        params_file.write_text("".join(f"{key}={shlex.quote(value)}\n" for key, value in (params or {}).items()))
        if argv is None:
            payload = self.script(f'"{self.probes["stamp"]}" "{stamp}"\n. "{params_file}"\n{body}')
            argv = terminal_argv(name, payload, self.work, self.kettle)
        # Every terminal runs with the log filter it ships with, whatever the
        # harness's own shell sets. Kettle's phase stamps go to its stderr,
        # and only a stamped entry's startup rounds get their filter.
        stderr_path = self.work / "terminal.stderr"
        stderr_path.unlink(missing_ok=True)
        stamped = phases and name in self.phases
        env = {key: value for key, value in os.environ.items() if key != "RUST_LOG"}
        if stamped:
            env["RUST_LOG"] = "warn,kettle::startup=info"
        # Its own session, so the probe leads a process group holding only it
        # and what it starts; stop() can clean that group up if it must.
        with (stderr_path.open("w") if stamped else open(os.devnull, "w")) as stderr:
            return subprocess.Popen(
                [str(self.probes["launch"]), str(self.work / "launch.json"), str(stamp), str(timeout), "--", *argv],
                stdout=subprocess.DEVNULL, stderr=stderr, start_new_session=True, env=env,
            )

    def latency(self, name: str, options: dict, seed: int, keep: Optional[Path] = None) -> dict:
        """One keystroke-to-screen round. The terminal runs keyblock (a floor
        is its own window), the probe measures its window, and the payload's
        log splits every key into its input and output halves. The terminal
        is stopped through its launch probe whatever the probe reports."""
        log = self.work / "keyblock.log"
        out = self.work / "latency.json"
        for stale in (log, out):
            stale.unlink(missing_ok=True)
        if name.startswith("floor-"):
            floor = [str(self.probes["latency-floor"]), name[len("floor-"):], str(log)]
            process = self.launch(name, "", 600, argv=floor)
            ready = self.wait_for(Path(str(self.work / "stamp") + ".pid"), 20)
        else:
            process = self.launch(name, f'exec "{self.probes["keyblock"]}" "{log}"', 600)
            ready = self.wait_for(self.work / "stamp", 30)
        pid = self.pid()
        if not ready or pid is None:
            self.stop(process, 30)
            return {"error": "the terminal never ran its payload"}
        time.sleep(1.0)
        # The probe posts nothing after its deadline and exits there, and
        # this waits longer than that, so no probe outlives its round.
        budget = 120 + (LATENCY_CALIBRATION_KEYS + options["warmup"] + options["keys"]) * (
            0.3 + options["censor_ms"] / 1000 + 0.2)
        args = ["--pid", str(pid), "--out", str(out), "--keys", str(options["keys"]),
                "--warmup", str(options["warmup"]), "--censor-ms", str(options["censor_ms"]),
                "--seed", str(seed), "--inject", options["inject"], "--deadline-ms", str(int(budget * 1000))]
        started = time.monotonic()
        try:
            run_latency_probe(self.probes["latency-probe"], args, self.work, budget + 15)
        except subprocess.TimeoutExpired:
            pass
        # The probe writes its result last, even when it fails or its
        # deadline passes, and atomically, so the file appears whole.
        finished = wait_for_text(out, "}", max(1.0, budget + 15 - (time.monotonic() - started)))
        clean = self.stop(process, 30)
        if not finished:
            return {"error": "the latency probe never finished"}
        try:
            probe = json.loads(out.read_text())
        except (OSError, json.JSONDecodeError):
            return {"error": "the latency probe wrote no result"}
        if keep:
            # Every sample, for checks results.json does not carry.
            shutil.copyfile(out, keep)
        if "error" in probe:
            return {"error": f"latency probe: {probe['error']}"}
        row = latency_row(probe, read_keyblock_log(log), options["censor_ms"])
        if not clean:
            row["killed"] = True
        return row

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
            result.update(parse_phases(stderr_path.read_text(errors="replace"), result.get("started_ns")))
        stamp = self.work / "stamp"
        if stamp.exists():
            fields = stamp.read_text().split()
            result["cols"], result["rows"] = int(fields[1]), int(fields[2])
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
        try:
            loop.wait(timeout=5)
        except subprocess.TimeoutExpired:
            loop.kill()
            loop.wait()

    def startup(self, name: str) -> dict:
        # Hold the window for a second so terminals that spawn the child before
        # showing a window still reach the window server.
        process = self.launch(name, "exec /bin/sleep 1", 20, phases=True)
        return self.finish(process, 20)

    def idle(self, name: str, settle: float, window: float, activate: bool) -> dict:
        process = self.launch(name, f"exec /bin/sleep {settle + window + 5}", settle + window + 20)
        if not self.wait_for(self.work / "stamp", 20):
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


def vtebench_means(row: dict) -> Dict[str, float]:
    """Benchmark means of a round. A schema-1 row holds medians, so its means
    come from its .dat file (see load_session)."""
    if "means_ms" in row:
        return row["means_ms"]
    return {key: value for key, value in row.items() if isinstance(value, (int, float)) and key != "error"}


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


def paired_difference(a: List[Optional[float]], b: List[Optional[float]]) -> dict:
    """The mean of b - a round pairs, in the metric's own unit, with a
    Student-t 95 % interval: the absolute gain a ratio hides."""
    diffs = [y - x for x, y in zip(a, b) if x is not None and y is not None]
    if not diffs:
        return {}
    mean, low, high = t_interval(diffs)
    return {"diff": mean, "low": low, "high": high, "n": len(diffs)}


def latency_keys(run: dict, censor_ms: float) -> Optional[List[float]]:
    """A latency round's keys, with each censored key at the censor bound,
    which can only make a terminal look slower; None if the round failed."""
    if ("error" in run or run.get("warmup") or run.get("seq_mismatch")
            or not isinstance(run.get("samples_ms"), list)):
        return None
    keys = list(run["samples_ms"]) + [float(censor_ms)] * int(run.get("censored") or 0)
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


def is_number(value) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


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
    if workload == "latency":
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
                None if "error" in run or not vtebench_means(run) else geometric_mean(vtebench_means(run).values())
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
    return {metric: {name: [row_value(workload, run, metric) for run in runs] for name, runs in rows.items()}
            for metric in metrics}


def analyze(results: dict, names: List[str], ab: bool) -> dict:
    """Per-workload estimates, intervals and comparisons for one session."""
    analysis: Dict[str, dict] = {}
    kettle = names[0]
    unranked = set(results.get("unranked", []))
    for workload, rows in results["workloads"].items():
        if workload == "latency":
            analysis[workload] = analyze_latency(results, rows, names, ab)
            continue
        mean_based = workload == "vtebench"
        metrics: Dict[str, dict] = {}
        per_metric = workload_metrics(workload, rows, results.get("meta"))
        benchmarks = sum(1 for metric in per_metric if metric != "geometric mean")
        for metric, per_name in per_metric.items():
            terminals = {}
            for name in names:
                present = [v for v in per_name.get(name, []) if v is not None]
                if not present:
                    continue
                if mean_based:
                    ci = mean_ci(present)
                    terminals[name] = {"estimate": ci["mean"], "low": ci["low"], "high": ci["high"], "n": ci["n"]}
                else:
                    ci = median_ci(present)
                    terminals[name] = {"estimate": ci["median"], "low": ci["low"], "high": ci["high"],
                                       "n": ci["n"]}
            entry: dict = {"kind": "mean" if mean_based else "median", "terminals": terminals,
                           "values": {name: per_name.get(name, []) for name in names}}
            # vtebench's benchmarks are tested together; the geometric mean
            # is one row.
            family = benchmarks if mean_based and metric != "geometric mean" else 1
            if ab and len(names) == 2:
                entry["ab"] = paired(per_name.get(names[0], []), per_name.get(names[1], []), family)
                if workload == "startup":
                    entry["ab_diff"] = paired_difference(per_name.get(names[0], []), per_name.get(names[1], []))
            elif kettle in terminals:
                ranked = {name: t for name, t in terminals.items() if name not in unranked}
                others = {name: t for name, t in ranked.items() if name != kettle}
                if others:
                    best = min(others, key=lambda name: others[name]["estimate"])
                    entry["best_other"] = best
                    entry["vs_best"] = paired(per_name[best], per_name[kettle])
                    if workload == "startup":
                        entry["vs_best_diff"] = paired_difference(per_name[best], per_name[kettle])
                    order = sorted(ranked, key=lambda name: ranked[name]["estimate"])
                    entry["rank"] = order.index(kettle) + 1
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
    return analysis


def latency_censor_ms(results: dict) -> float:
    return float(((results.get("meta") or {}).get("latency") or {}).get("censor_ms", 500))


def analyze_latency(results: dict, rows: Dict[str, List[dict]], names: List[str], ab: bool) -> dict:
    """The latency workload: every entry that ran (the terminals, then the
    unranked opaque variant and the floors), with mean latency over rounds
    (launches). Kettle is compared with the fastest other ranked terminal, or
    B with A, on the round means of every paired round."""
    censor_ms = latency_censor_ms(results)
    planned = ((results.get("meta") or {}).get("rounds") or {}).get("latency", 0)
    entries = [name for name in names if name in rows] + [name for name in rows if name not in names]
    unranked = set(results.get("unranked", []))
    keys = {name: [latency_keys(run, censor_ms) for run in rows[name]] for name in entries}
    standing = {name: latency_standing(rows[name], censor_ms, planned) for name in entries}
    metrics: Dict[str, dict] = {}
    # Median, percentiles and halves come from every counted key of the
    # session, never from per-round summaries.
    pooled = {name: [k for r in keys[name] if r for k in r] for name in entries}
    halves = {half: {name: [v for run in rows[name] if latency_keys(run, censor_ms) is not None
                            for v in run.get(half) or []] for name in entries}
              for half in ("inputs_ms", "outputs_ms")}
    for metric, per_name in workload_metrics("latency", rows).items():
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
        metrics[metric] = entry
    return {"metrics": metrics, "entries": entries, "standing": standing}


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
        elif not standing["ranked"]:
            mean_cell += " unranked"
        out.append(f"| {name} | {mean_cell} | " + " | ".join(cell(m, name) for m in LATENCY_METRICS[1:])
                   + f" | {standing['censored']}/{standing['keys']} | {mean['n'] if mean else 0} |")
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
    return out


def summarize(results: dict, names: List[str], ab: bool) -> str:
    analysis = analyze(results, names, ab)
    out = ["# macOS standing", "", results["context"], ""]
    for workload, info in analysis.items():
        out.append(f"## {workload}")
        out.append("")
        if workload == "latency":
            countable = ((results.get("meta") or {}).get("workload_countable") or {}).get("latency")
            out.extend(latency_markdown(info, ab, countable))
            out.append("")
            continue
        metrics = info["metrics"]
        mean_based = workload == "vtebench"
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
                    diff = metrics[metric].get("ab_diff")
                    delta = (f"; B-A {diff['diff']:+.1f} ms, 95% CI {diff['low']:+.1f} to {diff['high']:+.1f}"
                             if diff else "")
                    out.append(
                        f"{metric}: B/A {stats['ratio']:.3f} "
                        f"(95% CI {stats['low']:.3f}-{stats['high']:.3f}, n={stats['n']}, "
                        f"B lower in {stats['wins']}/{stats['n']}{delta})"
                    )
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
    return "\n".join(out)


# === Sessions and combining ==========================================


def session_countable(meta: dict) -> bool:
    """A session counts only if its preflight was clean, Kettle ran from an
    app bundle, every requested round finished, and it was not a diagnostic
    that changes what the terminals do: walking their memory mid-measurement
    (--footprint-detail) or turning on Kettle's startup log (--startup-phases)."""
    return (not meta.get("refusals") and not meta.get("bare") and not meta.get("footprint_detail")
            and not meta.get("startup_phases") and meta.get("complete") is True)


def round_ok(workload: str, run: dict, meta: Optional[dict] = None) -> bool:
    if run.get("warmup"):
        return True
    if "error" in run or run.get("killed"):
        return False
    if workload == "vtebench":
        return bool(run.get("means_ms"))
    if workload == "latency":
        # A key read with another, or a read nobody posted, shifts the join
        # of samples to records: the round is not trusted.
        return (isinstance(run.get("samples_ms"), list) and not run.get("killed")
                and not run.get("seq_mismatch"))
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
        if workload != "latency" and not all(round_ok(workload, run, meta) for run in runs):
            return False
    if workload == "latency":
        # A refresh rate that changed mid-session moves every sample.
        rates = {run.get("refresh_hz") for runs in results["workloads"]["latency"].values() for run in runs
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


def load_session(folder: Path) -> dict:
    """A session directory's results with the fields --combine needs.

    Schema 1 (4.7.0 and earlier) has no metadata or preflight, so it never
    counts; its vtebench rows hold medians, so their means are rebuilt from
    the round's .dat file.
    """
    results = json.loads((folder / "results.json").read_text())
    schema = results.get("schema", 1)
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
    per_workload = workload_countable(results, meta)
    setup = {key: meta.get(key) for key in SESSION_KEYS}
    setup["identity"] = {name: {k: v for k, v in (ident or {}).items() if k in ("sha256", "cdhash")}
                         for name, ident in (meta.get("identity") or {}).items()}
    return {"dir": folder.name, "schema": schema, "names": names, "results": results, "date": meta["date"],
            "started": meta.get("started") or meta["date"], "countable": countable,
            "workload_countable": per_workload, "label": meta.get("label", folder.name), "ab": meta.get("mode") == "ab",
            "rounds": meta.get("rounds") or {}, "setup": setup, "configs": meta.get("configs") or {}}


def combine(folders: List[Path], aa: Optional[Path] = None) -> dict:
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
                or control["configs"].get("kettle-a", "") != control["configs"].get("kettle-b", "")):
            raise SystemExit(f"--aa {Path(aa).name} does not run the same build and config on both sides")
        # The A/A calibrates the harness and the machine, not a build, so every
        # setting but the binaries and configs under test must match. Its round
        # counts may differ (fewer rounds only widen its gates), and only the
        # tools both ran are compared.
        reference = counted[0] if counted else sessions[0]
        differs = sorted(key for key in control["setup"]
                         if key not in ("identity", "configs", "rounds", "tool_hashes", "latency")
                         and control["setup"][key] != reference["setup"][key])
        # Latency's knobs must match when both ran it; its entries differ by
        # design (a standing adds floors).
        knobs = ("keys", "warmup", "censor_ms", "inject", "signed")
        latency_a, latency_b = control["setup"].get("latency"), reference["setup"].get("latency")
        if latency_a and latency_b and any(latency_a.get(k) != latency_b.get(k) for k in knobs):
            differs.append("latency")
        tools_a, tools_b = control["setup"].get("tool_hashes") or {}, reference["setup"].get("tool_hashes") or {}
        if any(tools_a[name] != tools_b[name] for name in tools_a.keys() & tools_b.keys()):
            differs.append("tool_hashes")
        if differs:
            raise SystemExit(f"--aa {Path(aa).name} and {reference['label']} differ in {', '.join(differs)}")
        # Only what the A/B tests may differ: the B side's build or config.
        if control["configs"].get("kettle-a", "") != reference["configs"].get("kettle-a", ""):
            raise SystemExit(f"--aa {Path(aa).name} and {reference['label']} differ in the baseline config")
        for workload, info in analyze(control["results"], control["names"], True).items():
            if not control["workload_countable"].get(workload, control["countable"]):
                continue
            planned = control["rounds"].get(workload)
            for metric, entry in info["metrics"].items():
                stats = entry.get("ab")
                if stats and (not planned or stats["n"] >= math.ceil(MIN_PAIRED_SHARE * planned)):
                    gates[f"{workload}.{metric}"] = latency_aa_gate(stats) if workload == "latency" else aa_gate(stats)
    analyses = [analyze(s["results"], s["names"], s["ab"]) for s in sessions]
    rows: Dict[str, dict] = {}
    for session, analysis in zip(sessions, analyses):
        for workload, info in analysis.items():
            counts = session["workload_countable"].get(workload, session["countable"])
            for metric, entry in info["metrics"].items():
                row = rows.setdefault(f"{workload}.{metric}", {"terminals": {}, "sessions": [], "per_session": []})
                estimates = {name: terminal["estimate"] for name, terminal in entry["terminals"].items()}
                row["per_session"].append({"label": session["label"], "countable": counts,
                                           "estimates": estimates})
                if counts:
                    for name, value in estimates.items():
                        row["terminals"].setdefault(name, {"estimates": []})["estimates"].append(value)
                # A comparison with too few paired rounds (idle rounds that lost
                # focus) does not stand for the session.
                planned = session["rounds"].get(workload)
                stats = entry.get("ab") if ab else entry.get("vs_best")
                covered = not planned or (stats or {}).get("n", 0) >= math.ceil(MIN_PAIRED_SHARE * planned)
                base = {"label": session["label"], "date": session["date"], "started": session["started"],
                        "countable": counts and covered}
                if ab and entry.get("ab"):
                    row["sessions"].append({**base, **entry["ab"]})
                elif entry.get("vs_best"):
                    row["sessions"].append({**base, **entry["vs_best"], "peer": entry["best_other"],
                                            "rank": entry["rank"]})
    for key, row in rows.items():
        for terminal in row["terminals"].values():
            estimates = terminal["estimates"]
            terminal.update({"published": statistics.median(estimates), "min": min(estimates),
                             "max": max(estimates)})
        if ab:
            gate = gates.get(key)
            if aa and not gate:
                row["verdict"] = {"verdict": "A/A missing"}
            elif gate and not gate["contains_one"]:
                # The same build differed from itself: that A/A cannot
                # calibrate anything.
                row["verdict"] = {"verdict": "A/A failed"}
            elif key.startswith("latency."):
                row["verdict"] = latency_ab_verdict(row["sessions"], gate["gate_ms"] if gate else None)
            else:
                row["verdict"] = ab_verdict(row["sessions"], gate["gate"] if gate else None)
            if gate:
                row["aa"] = gate
        elif row["sessions"]:
            row["claim"] = claim(row["sessions"])
    combined = {"sessions": [{k: s[k] for k in ("dir", "label", "date", "countable", "schema")} for s in sessions],
                "rows": rows}
    combined["markdown"] = combined_markdown(combined, ab)
    return combined


def combined_markdown(combined: dict, ab: bool) -> str:
    out = ["# Combined macOS sessions", ""]
    out.append("| session | date | countable |")
    out.append("|---|---|---|")
    for s in combined["sessions"]:
        out.append(f"| {s['label']} | {s['date']} | {'yes' if s['countable'] else 'no'} |")
    out.append("")
    if ab:
        out.append("| row | A | B | sessions (B/A, 95% CI) | verdict |")
        out.append("|---|---:|---:|---|---|")
        for key, row in combined["rows"].items():
            a = row["terminals"].get("kettle-a", {}).get("published")
            b = row["terminals"].get("kettle-b", {}).get("published")
            per = "; ".join(f"{s['ratio']:.3f} ({s['low']:.3f}-{s['high']:.3f})" for s in row["sessions"])
            verdict = row.get("verdict", {}).get("verdict", "-")
            if row.get("verdict", {}).get("no_regression") is not None:
                verdict += "; no regression" if row["verdict"]["no_regression"] else "; REGRESSION over +1 ms"
            out.append(f"| {key.replace('.', ' ', 1)} | {a if a is None else f'{a:.2f}'} | "
                       f"{b if b is None else f'{b:.2f}'} | {per} | {verdict} |")
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
    return "\n".join(out) + "\n"


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
        refusals.append("scripts/perf has local changes")
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


def harness_revision(repo: Path = REPO) -> dict:
    """The tree hash of scripts/perf, so merges outside the harness do not
    change it, and whether that tree has local changes."""
    tree = checked(["git", "-C", str(repo), "rev-parse", "HEAD:scripts/perf"])
    status = subprocess.run(["git", "-C", str(repo), "status", "--porcelain", "--", "scripts/perf"],
                            capture_output=True, text=True)
    # Either command failing leaves the state unknown, which refuses.
    dirty = bool(status.stdout.strip()) if tree and status.returncode == 0 else None
    return {"harness_tree": tree.strip() if tree else None, "harness_dirty": dirty}


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
    if not label or label.lower() in ("a", "b"):
        raise ValueError(f"--kettle-variant name {label!r} is reserved or empty")
    return f"kettle-{label}"


def default_out_dir(root: Path) -> Path:
    return root / datetime.datetime.now().strftime("%Y-%m-%d-%H%M%S")


def claim_out_dir(path: Path) -> Path:
    """A session directory of its own: never one that already holds results."""
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        path.mkdir()
    except FileExistsError:
        raise SystemExit(f"{path} already exists; each session needs a new --out-dir") from None
    return path


def resolve_rounds(args: argparse.Namespace) -> Dict[str, int]:
    if args.rounds:
        return {workload: args.rounds for workload in WORKLOADS + OPT_IN_WORKLOADS}
    return {"startup": args.startup_rounds, "idle": args.idle_rounds, "flood-memory": args.flood_rounds,
            "vtebench": args.vtebench_rounds, "latency": args.latency_rounds}


def latency_entries(names: List[str], ab: bool, opaque: bool, floors: List[str]) -> List[str]:
    """The latency rotation: the session's terminals, then, in a standing
    session, Kettle's opaque variant and the floors, which are published
    beside them and never ranked. An A/B compares its two builds only."""
    if ab:
        return list(names)
    return names + (["kettle-opaque"] if opaque else []) + [f"floor-{mode}" for mode in floors]


def run_latency_check(tools: Path, identity: Optional[str]) -> int:
    """--latency-check: build the probe, ask macOS for its grants, report."""
    probes = build_probes(tools, latency=True, sign_identity=identity)
    with tempfile.TemporaryDirectory(prefix="kettle-latency-check-") as tmp:
        grants = latency_grants(probes["latency-probe"], Path(tmp), request=True)
        grants = latency_grants(probes["latency-probe"], Path(tmp))
    for grant, held in grants.items():
        print(f"{grant.replace('_', ' ')}: {'granted' if held else 'missing'}")
    if not all(grants.values()):
        print(f"grant both to {probes['latency-probe']} in System Settings > Privacy & Security, "
              "then run --latency-check again", file=sys.stderr)
        return 3
    self_test = subprocess.run([str(probes["latency-probe"] / "Contents" / "MacOS" / "latency-probe"), "--self-test"])
    return self_test.returncode


def run_combine(args: argparse.Namespace) -> int:
    combined = combine([Path(folder) for folder in args.combine], Path(args.aa) if args.aa else None)
    out_dir = claim_out_dir(Path(args.out_dir) if args.out_dir else
                            default_out_dir(REPO / "target" / "perf-results" / "combined"))
    markdown = combined.pop("markdown")
    (out_dir / "combined.json").write_text(dumps(combined))
    (out_dir / "combined.md").write_text(markdown)
    print(markdown)
    return 0


def main() -> int:
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
                        help="build the latency probe, ask for its grants, report them and exit")
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
    parser.add_argument("--aa", metavar="DIR", help="with --combine: an A/A session whose intervals set the gates")
    args = parser.parse_args()
    # Practical bounds, checked before anything runs: a run stays finite, and
    # the probe's nanosecond arithmetic cannot overflow.
    for flag, value, least, most in (("--latency-keys", args.latency_keys, 1, 1000),
                                     ("--latency-rounds", args.latency_rounds, 1, 100),
                                     ("--latency-censor-ms", args.latency_censor_ms, 1, 5000),
                                     ("--latency-warmup", args.latency_warmup, 0, 200),
                                     ("--rounds", args.rounds, 1, 1000)):
        if value is None:
            continue
        if value < least:
            parser.error(f"{flag} must be at least {least}")
        if value > most:
            parser.error(f"{flag} must be at most {most}")

    if args.combine:
        return run_combine(args)
    if args.make_bundle:
        print(bundle_kettle(Path(args.make_bundle[0]), Path(args.make_bundle[1]), installed_template()))
        return 0
    if sys.platform != "darwin":
        print("macos-standing.py: this benchmark requires macOS", file=sys.stderr)
        return 1
    if args.latency_check:
        return run_latency_check(REPO / "target" / "perf-tools" / "macos-standing", args.latency_sign_identity)
    workloads = [w for w in args.workloads.split(",") if w]
    unknown = set(workloads) - set(WORKLOADS) - set(OPT_IN_WORKLOADS)
    if unknown:
        parser.error(f"unknown workloads: {', '.join(sorted(unknown))}")
    floors = [f for f in args.latency_floors.split(",") if f]
    if set(floors) - set(LATENCY_FLOORS):
        parser.error(f"unknown latency floors: {', '.join(sorted(set(floors) - set(LATENCY_FLOORS)))}")
    rounds = resolve_rounds(args)
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
                parser.error(f"--kettle-variant needs NAME=LINES, got {variant!r}")
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
    if "latency" in workloads and "kettle-opaque" in latency_names:
        kettle["kettle-opaque"] = kettle["kettle"]
        kettle_configs["kettle-opaque"] = KETTLE_OPAQUE
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
    probes = build_probes(tools, latency="latency" in workloads, sign_identity=args.latency_sign_identity)
    vtebench = build_vtebench(tools) if "vtebench" in workloads else None
    # The probe app's signature changes with every signing, so its source
    # stands for it.
    tool_hashes = {name: file_sha256(PROBES / "latency-probe.swift" if name == "latency-probe" else path)
                   for name, path in probes.items()}
    if "latency" in workloads:
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
    subprocess.Popen(["caffeinate", "-dimsu", "-w", str(os.getpid())])

    out_dir = claim_out_dir(Path(args.out_dir) if args.out_dir else
                            default_out_dir(REPO / "target" / "perf-results" / "macos-standing"))

    host = command(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
    release = command(["sw_vers", "-productVersion"]).strip()
    load = os.getloadavg()[0]
    started = datetime.datetime.now().astimezone()
    host_terminal = host_terminal_of(state["procs"] or [], field, args.host_pid)
    results = {
        "schema": SCHEMA,
        "context": f"{host}, macOS {release}, load {load:.2f} at start, rounds "
                   + ", ".join(f"{w} {rounds[w]}" for w in workloads)
                   + f", {COLS}x{ROWS} grid, default configs, fd soft limit "
                   f"{resource.getrlimit(resource.RLIMIT_NOFILE)[0]}",
        "terminals": names, "skipped": skipped, "unranked": unranked,
        "meta": {
            "label": args.label or out_dir.name, "mode": "ab" if is_ab(kettle) else "standing",
            "started": started.isoformat(timespec="seconds"), "date": started.date().isoformat(),
            # countable is final only once every round has run (see
            # session_countable); an interrupted session never counts.
            "complete": False, "refusals": refusals, "bare": bool(bare), "countable": False,
            **harness_revision(), "tool_hashes": tool_hashes,
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
                        "entries": latency_names, "signed": "identity" if args.latency_sign_identity else "ad hoc",
                        } if "latency" in workloads else None,
            "identity": {},
        },
        "workloads": {},
    }
    local_manifest = {"configs": config_record(kettle_configs)[1]}
    for name in names:
        public, local = terminal_identity(kettle.get(name) or APPS[name])
        results["meta"]["identity"][name] = public
        local_manifest[name] = local
    # Paths and signing teams identify this machine and its owner; they stay
    # beside the results and never go into anything published.
    (out_dir / "local-manifest.json").write_text(dumps(local_manifest))
    recorder = Recorder(out_dir / "results.json", results)
    recorder.write()

    with tempfile.TemporaryDirectory(prefix="kettle-standing-") as tmp:
        work = Path(tmp)
        write_configs(work, kettle_configs)
        runner = Runner(probes, work, kettle)
        runner.phases = stamped
        flood = work / "flood.txt"
        if "flood-memory" in workloads:
            write_flood(flood)
        if vtebench:
            benchmarks = prepare_benchmarks(vtebench.parents[2] / "benchmarks", work / "benchmarks")
        latency_options = {"keys": args.latency_keys, "warmup": args.latency_warmup,
                           "censor_ms": args.latency_censor_ms, "inject": args.latency_inject}
        for workload in workloads:
            entries = latency_names if workload == "latency" else names
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
            for round_index in range(warmups + rounds[workload]):
                for name in rotated(entries, round_index):
                    if workload == "startup":
                        row = runner.startup(name)
                    elif workload == "idle":
                        row = runner.idle(name, args.idle_settle, args.idle_window, not args.no_activate)
                    elif workload == "flood-memory":
                        row = runner.flood_memory(name, flood, offsets, not args.no_activate, args.footprint_detail)
                    elif workload == "latency":
                        if failures_in_a_row >= len(entries):
                            row = {"error": f"not run: {len(entries)} latency rounds in a row failed"}
                        else:
                            # A new gap sequence every round, the same for
                            # every entry in it.
                            row = runner.latency(name, latency_options, SEED * 1000 + round_index,
                                                 out_dir / f"latency-{name}-r{round_index}.json")
                            failures_in_a_row = failures_in_a_row + 1 if "error" in row else 0
                    else:
                        row = runner.vtebench(name, vtebench, benchmarks,
                                              out_dir / f"{name}-r{round_index}.dat", args.vtebench_seconds)
                    if round_index < warmups:
                        row["warmup"] = True
                    row["at"] = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
                    row["load"] = list(os.getloadavg()[:2])
                    rows[name].append(row)
                    recorder.write()
                    print(f"{workload} round {round_index} {name}: {json.dumps(row)}", flush=True)
                    time.sleep(1.0)

    results["meta"]["complete"] = True
    results["meta"]["countable"] = session_countable(results["meta"]) and rounds_complete(results, results["meta"])
    results["meta"]["workload_countable"] = workload_countable(results, results["meta"])
    recorder.write()
    summary = summarize(results, names, is_ab(kettle))
    (out_dir / "summary.md").write_text(summary + "\n")
    print(summary)
    return 0


def rotated(names: List[str], round_index: int) -> List[str]:
    shift = round_index % len(names)
    return names[shift:] + names[:shift]


if __name__ == "__main__":
    sys.exit(main())
