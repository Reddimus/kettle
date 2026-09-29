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
  flood-memory  phys_footprint after printing 32 MiB of seeded text
  vtebench      Alacritty's vtebench at a pinned revision, built from a copy
                that records microseconds instead of whole milliseconds, with
                its scripts' window-size lookup fixed for macOS

Rounds rotate the terminal order so no terminal always runs first. Startup,
idle and flood rows report medians; vtebench reports the mean of each
benchmark's samples per round, then the mean over rounds. Kettle is compared
with the best other terminal round by round, with a bootstrap 95% interval
over rounds and a count of the rounds Kettle won. With --kettle-b the run
compares two Kettle builds and reports B/A the same way.

Idle numbers depend on focus: Kettle, Ghostty, and kitty blink the cursor only
in a focused window, and macOS gives a launched window focus only while the
desktop is unlocked and nothing else holds it. Only rounds in which the
terminal was frontmost count.

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
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence

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
# Publication defaults. Counts are multiples of five so a five-terminal
# rotation is balanced.
ROUNDS = {"startup": 30, "idle": 5, "flood-memory": 5, "vtebench": 5}
# The metrics each workload reports; a round's other numbers (exit_ms, the
# resident size) are kept in results.json but never compared or published.
METRICS = {"startup": ("window_ms", "child_ms"), "idle": ("cpu_percent", "wakeups_per_second", "footprint_mib"),
           "flood-memory": ("footprint_mib", "max_footprint_mib")}
# The values a round must carry to count toward a complete session.
REQUIRED = {"startup": ("window_ms", "child_ms"), "idle": ("cpu_percent", "wakeups_per_second", "footprint_mib"),
            "flood-memory": ("footprint_mib",)}
# A row's comparison in a session counts only with this share of its rounds
# paired (an idle round that lost focus has no pair).
MIN_PAIRED_SHARE = 0.8
# Settings every session in a combined set must share; any change starts a
# new set.
SESSION_KEYS = ("harness_tree", "tool_hashes", "hw_model", "macos_build", "display", "fd_limit", "rounds", "warmup",
                "vtebench_seconds", "idle_settle", "idle_window", "flood_offsets", "activate", "configs")
# Reported once per terminal rather than as metrics.
GRID_KEYS = ("cols", "rows")

BOOTSTRAP = 10_000
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


def build_probes(tools: Path) -> Dict[str, Path]:
    """Compile the probes once into `tools`; rebuild when a source is newer."""
    tools.mkdir(parents=True, exist_ok=True)
    built = {}
    for name, compiler in (("stamp", "clang"), ("memsample", "clang"), ("launch", "swiftc")):
        source = PROBES / (f"{name}.swift" if compiler == "swiftc" else f"{name}.c")
        binary = tools / name
        if not binary.exists() or binary.stat().st_mtime < source.stat().st_mtime:
            command = [compiler, "-O", "-o", str(binary), str(source)]
            subprocess.run(command, check=True)
        built[name] = binary
    return built


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
        return [kettle[name], "--config", str(work / "kettle.config"), "-e", str(script)]
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


def write_configs(work: Path) -> None:
    (work / "kettle.config").write_text(
        "agent-server = off\nrestore-session = false\nupdate-check = false\n"
        f"window-width = {COLS}\nwindow-height = {ROWS}\n"
    )
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
        self.counter = 0
        # How long past its own timeout a launch probe may take to report.
        self.grace = 15.0

    def script(self, body: str) -> Path:
        self.counter += 1
        path = self.work / f"payload-{self.counter}.sh"
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)
        return path

    def launch(self, name: str, body: str, timeout: float) -> subprocess.Popen:
        stamp = self.work / "stamp"
        # A failed launch writes no result, so nothing from the previous round
        # may be left to be read in its place.
        for stale in (stamp, Path(str(stamp) + ".pid"), self.work / "done", self.work / "launch.json"):
            stale.unlink(missing_ok=True)
        payload = self.script(f'"{self.probes["stamp"]}" "{stamp}"\n{body}')
        argv = terminal_argv(name, payload, self.work, self.kettle)
        # Its own session, so the probe leads a process group holding only it
        # and what it starts; stop() can clean that group up if it must.
        return subprocess.Popen(
            [str(self.probes["launch"]), str(self.work / "launch.json"), str(stamp), str(timeout), "--", *argv],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
        )

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

    def startup(self, name: str) -> dict:
        # Hold the window for a second so terminals that spawn the child before
        # showing a window still reach the window server.
        process = self.launch(name, "exec /bin/sleep 1", 20)
        return self.finish(process, 20)

    def idle(self, name: str, settle: float, window: float) -> dict:
        process = self.launch(name, f"exec /bin/sleep {settle + window + 5}", settle + window + 20)
        if not self.wait_for(self.work / "stamp", 20):
            self.stop(process, 30)
            return {"error": "the terminal never ran its payload"}
        time.sleep(settle)
        first = self.sample()
        time.sleep(window)
        second = self.sample()
        pid_file = Path(str(self.work / "stamp") + ".pid")
        focused = pid_file.exists() and self.frontmost_pid() == int(pid_file.read_text())
        if not self.stop(process, 30):
            return {"error": "the terminal did not stop"}
        if not first or not second:
            return {"error": "terminal exited before sampling"}
        return {
            "cpu_percent": (second["cpu_ns"] - first["cpu_ns"]) / (window * 1e9) * 100,
            "wakeups_per_second": (second["wakeups"] - first["wakeups"]) / window,
            "footprint_mib": second["footprint"] / 2**20,
            "rss_mib": second["rss"] / 2**20,
            "frontmost": focused,
        }

    def flood_memory(self, name: str, flood: Path) -> dict:
        done = self.work / "done"
        process = self.launch(name, f'cat "{flood}"\n: > "{done}"\nexec /bin/sleep 30', 120)
        if not self.wait_for(done, 100):
            self.stop(process, 60)
            return {"error": "the flood never finished"}
        time.sleep(3)
        sample = self.sample()
        if not self.stop(process, 60):
            return {"error": "the terminal did not stop"}
        if not sample:
            return {"error": "terminal exited before sampling"}
        return {"footprint_mib": sample["footprint"] / 2**20,
                "max_footprint_mib": sample["max_footprint"] / 2**20,
                "rss_mib": sample["rss"] / 2**20}

    def vtebench(self, name: str, vtebench: Path, benchmarks: Path, dat: Path, seconds: int) -> dict:
        process = self.launch(
            name, f'exec "{vtebench}" -s -b "{benchmarks}" --dat "{dat}" --max-secs {seconds}', 900
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


def bootstrap(values: Sequence[float], statistic, seed: int = SEED) -> tuple:
    """2.5th and 97.5th percentiles of `statistic` over resamples of `values`."""
    rng = random.Random(seed)
    boots = sorted(statistic(rng.choices(values, k=len(values))) for _ in range(BOOTSTRAP))
    return boots[250], boots[9_749]


def ratio(base: float, test: float) -> float:
    """test/base, with a zero base kept rather than dropped: equal zeros tie,
    and a positive value over zero is infinitely worse."""
    if base == 0:
        return 1.0 if test == 0 else math.inf
    return test / base


def paired(a: List[Optional[float]], b: List[Optional[float]], seed: int = SEED) -> dict:
    """Median of b/a round pairs with a bootstrap 95% interval, and the rounds
    in which b was lower."""
    pairs = [(x, y) for x, y in zip(a, b) if x is not None and y is not None]
    if not pairs:
        return {}
    ratios = [ratio(x, y) for x, y in pairs]
    low, high = bootstrap(ratios, statistics.median, seed)
    return {"ratio": statistics.median(ratios), "low": low, "high": high,
            "wins": sum(1 for x, y in pairs if y < x), "n": len(ratios)}


def ratio_of_rounds(base: List[Optional[float]], test: List[Optional[float]], seed: int = SEED) -> dict:
    """Mean of test/base round pairs with a bootstrap 95% interval over rounds,
    and the rounds in which test was lower. Rounds pair by index."""
    pairs = [(x, y) for x, y in zip(base, test) if x is not None and y is not None]
    if not pairs:
        return {}
    ratios = [ratio(x, y) for x, y in pairs]
    low, high = bootstrap(ratios, statistics.mean, seed)
    return {"ratio": statistics.mean(ratios), "low": low, "high": high,
            "wins": sum(1 for x, y in pairs if y < x), "n": len(ratios)}


def median_ci(values: Sequence[float], seed: int = SEED) -> dict:
    low, high = bootstrap(values, statistics.median, seed)
    return {"median": statistics.median(values), "low": low, "high": high, "n": len(values)}


def mean_ci(values: Sequence[float], seed: int = SEED) -> dict:
    low, high = bootstrap(values, statistics.mean, seed)
    return {"mean": statistics.mean(values), "low": low, "high": high, "n": len(values)}


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
    """An A/A interval must contain 1; its half-width sets that metric's gate."""
    half = (stats["high"] - stats["low"]) / 2
    return {"contains_one": stats["low"] <= 1 <= stats["high"], "half_width": half,
            "gate": max(0.03, 2 * half)}


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


def workload_metrics(workload: str, rows: Dict[str, List[dict]]) -> Dict[str, Dict[str, List[Optional[float]]]]:
    """{metric: {terminal: [value per round]}} with rounds aligned by index."""
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
    metrics = [key for key in METRICS.get(workload, ())
               if any(is_number(run.get(key)) for runs in rows.values() for run in runs)]
    return {metric: {name: [row_value(workload, run, metric) for run in runs] for name, runs in rows.items()}
            for metric in metrics}


def analyze(results: dict, names: List[str], ab: bool) -> dict:
    """Per-workload estimates, intervals and comparisons for one session."""
    analysis: Dict[str, dict] = {}
    kettle = names[0]
    for workload, rows in results["workloads"].items():
        mean_based = workload == "vtebench"
        metrics: Dict[str, dict] = {}
        for metric, per_name in workload_metrics(workload, rows).items():
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
            compare = ratio_of_rounds if mean_based else paired
            if ab and len(names) == 2:
                entry["ab"] = compare(per_name.get(names[0], []), per_name.get(names[1], []))
            elif kettle in terminals:
                others = {name: t for name, t in terminals.items() if name != kettle}
                if others:
                    best = min(others, key=lambda name: others[name]["estimate"])
                    entry["best_other"] = best
                    entry["vs_best"] = compare(per_name[best], per_name[kettle])
                    order = sorted(terminals, key=lambda name: terminals[name]["estimate"])
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


def summarize(results: dict, names: List[str], ab: bool) -> str:
    analysis = analyze(results, names, ab)
    out = ["# macOS standing", "", results["context"], ""]
    for workload, info in analysis.items():
        out.append(f"## {workload}")
        out.append("")
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
                    out.append(
                        f"{metric}: B/A {stats['ratio']:.3f} "
                        f"(95% CI {stats['low']:.3f}-{stats['high']:.3f}, n={stats['n']}, "
                        f"B lower in {stats['wins']}/{stats['n']})"
                    )
        else:
            compared = [m for m in ordered + (["geometric mean"] if "geometric mean" in metrics else [])
                        if metrics[m].get("vs_best")]
            if compared:
                out.append("")
                out.append(f"| {label} | best other | Kettle/other | 95% CI | Kettle lower in |")
                out.append("|---|---|---:|---|---:|")
                for metric in compared:
                    stats = metrics[metric]["vs_best"]
                    out.append(f"| {metric} | {metrics[metric]['best_other']} | {stats['ratio']:.3f} | "
                               f"{stats['low']:.3f}-{stats['high']:.3f} | {stats['wins']}/{stats['n']} |")
        out.append("")
    return "\n".join(out)


# === Sessions and combining ==========================================


def session_countable(meta: dict) -> bool:
    """A session counts only if its preflight was clean, Kettle ran from an
    app bundle, and every requested round finished."""
    return not meta.get("refusals") and not meta.get("bare") and meta.get("complete") is True


def round_ok(workload: str, run: dict) -> bool:
    if run.get("warmup"):
        return True
    if "error" in run or run.get("killed"):
        return False
    if workload == "vtebench":
        return bool(run.get("means_ms"))
    return all(is_number(run.get(key)) for key in REQUIRED.get(workload, ()))


def rounds_complete(results: dict, meta: dict) -> bool:
    """Every requested round of every terminal is present, has no error and
    carries its workload's values."""
    rounds = meta.get("rounds") or {}
    for workload, rows in results["workloads"].items():
        expected = rounds.get(workload, 0) + (meta.get("warmup", 0) if workload == "startup" else 0)
        for runs in rows.values():
            if len(runs) != expected or not all(round_ok(workload, run) for run in runs):
                return False
    return True


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
    setup = {key: meta.get(key) for key in SESSION_KEYS}
    setup["identity"] = {name: {k: v for k, v in (ident or {}).items() if k in ("sha256", "cdhash")}
                         for name, ident in (meta.get("identity") or {}).items()}
    return {"dir": folder.name, "schema": schema, "names": names, "results": results, "date": meta["date"],
            "started": meta.get("started") or meta["date"], "countable": countable,
            "label": meta.get("label", folder.name), "ab": meta.get("mode") == "ab",
            "rounds": meta.get("rounds") or {}, "setup": setup, "configs": meta.get("configs") or {}}


def combine(folders: List[Path], aa: Optional[Path] = None) -> dict:
    """Merge sessions into published values and labels (see claim, ab_verdict)."""
    sessions = [load_session(Path(folder)) for folder in folders]
    if len({s["ab"] for s in sessions}) > 1:
        raise SystemExit("--combine takes standing sessions or A/B sessions, not both")
    ab = sessions[0]["ab"]
    counted = [s for s in sessions if s["countable"]]
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
        # setting but the binaries and configs under test must match.
        reference = counted[0] if counted else sessions[0]
        differs = sorted(key for key in control["setup"] if key not in ("identity", "configs")
                         and control["setup"][key] != reference["setup"][key])
        if differs:
            raise SystemExit(f"--aa {Path(aa).name} and {reference['label']} differ in {', '.join(differs)}")
        # Only what the A/B tests may differ: the B side's build or config.
        if control["configs"].get("kettle-a", "") != reference["configs"].get("kettle-a", ""):
            raise SystemExit(f"--aa {Path(aa).name} and {reference['label']} differ in the baseline config")
        for workload, info in analyze(control["results"], control["names"], True).items():
            planned = control["rounds"].get(workload)
            for metric, entry in info["metrics"].items():
                stats = entry.get("ab")
                if stats and (not planned or stats["n"] >= math.ceil(MIN_PAIRED_SHARE * planned)):
                    gates[f"{workload}.{metric}"] = aa_gate(stats)
    analyses = [analyze(s["results"], s["names"], s["ab"]) for s in sessions]
    rows: Dict[str, dict] = {}
    for session, analysis in zip(sessions, analyses):
        for workload, info in analysis.items():
            for metric, entry in info["metrics"].items():
                row = rows.setdefault(f"{workload}.{metric}", {"terminals": {}, "sessions": [], "per_session": []})
                estimates = {name: terminal["estimate"] for name, terminal in entry["terminals"].items()}
                row["per_session"].append({"label": session["label"], "countable": session["countable"],
                                           "estimates": estimates})
                if session["countable"]:
                    for name, value in estimates.items():
                        row["terminals"].setdefault(name, {"estimates": []})["estimates"].append(value)
                # A comparison with too few paired rounds (idle rounds that lost
                # focus) does not stand for the session.
                planned = session["rounds"].get(workload)
                stats = entry.get("ab") if ab else entry.get("vs_best")
                covered = not planned or (stats or {}).get("n", 0) >= math.ceil(MIN_PAIRED_SHARE * planned)
                base = {"label": session["label"], "date": session["date"], "started": session["started"],
                        "countable": session["countable"] and covered}
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
        return {workload: args.rounds for workload in WORKLOADS}
    return {"startup": args.startup_rounds, "idle": args.idle_rounds, "flood-memory": args.flood_rounds,
            "vtebench": args.vtebench_rounds}


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
    parser.add_argument("--make-bundle", nargs=2, metavar=("BINARY", "APP"),
                        help="put a Kettle binary in an ad-hoc signed copy of the installed app and exit")
    parser.add_argument("--combine", nargs="+", metavar="DIR", help="merge session directories and exit")
    parser.add_argument("--aa", metavar="DIR", help="with --combine: an A/A session whose intervals set the gates")
    args = parser.parse_args()

    if args.combine:
        return run_combine(args)
    if args.make_bundle:
        print(bundle_kettle(Path(args.make_bundle[0]), Path(args.make_bundle[1]), installed_template()))
        return 0
    if sys.platform != "darwin":
        print("macos-standing.py: this benchmark requires macOS", file=sys.stderr)
        return 1
    workloads = [w for w in args.workloads.split(",") if w]
    unknown = set(workloads) - set(WORKLOADS)
    if unknown:
        parser.error(f"unknown workloads: {', '.join(sorted(unknown))}")
    rounds = resolve_rounds(args)

    if args.kettle_b:
        kettle = {"kettle-a": args.kettle, "kettle-b": args.kettle_b}
        names = ["kettle-a", "kettle-b"]
        skipped = {}
    else:
        kettle = {"kettle": args.kettle}
        names = ["kettle"]
        skipped = {}
        for peer in [p for p in args.peers.split(",") if p]:
            if peer not in APPS:
                parser.error(f"unknown peer: {peer}")
            if Path(APPS[peer]).exists():
                names.append(peer)
            else:
                skipped[peer] = f"not installed at {APPS[peer]}"
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
        kettle[names[0]] = str(bundle_kettle(DEFAULT_KETTLE, tools / "kettle-local.app", installed_template()))
    require_bundles(kettle, args.allow_bare)
    bare = [name for name, path in kettle.items() if not in_app_bundle(Path(path))]

    # Build every tool first: compiling right before measuring adds load and
    # heat, so the preflight that decides the session runs after it.
    probes = build_probes(tools)
    vtebench = build_vtebench(tools) if "vtebench" in workloads else None
    tool_hashes = {name: file_sha256(path) for name, path in probes.items()}
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
        "terminals": names, "skipped": skipped,
        "meta": {
            "label": args.label or out_dir.name, "mode": "ab" if args.kettle_b else "standing",
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
            "idle_settle": args.idle_settle, "idle_window": args.idle_window,
            "identity": {},
        },
        "workloads": {},
    }
    local_manifest = {}
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
        write_configs(work)
        runner = Runner(probes, work, kettle)
        flood = work / "flood.txt"
        if "flood-memory" in workloads:
            write_flood(flood)
        if vtebench:
            benchmarks = prepare_benchmarks(vtebench.parents[2] / "benchmarks", work / "benchmarks")
        for workload in workloads:
            rows: Dict[str, List[dict]] = {name: [] for name in names}
            results["workloads"][workload] = rows
            for round_index in range(rounds[workload]):
                for name in rotated(names, round_index):
                    if workload == "startup":
                        row = runner.startup(name)
                    elif workload == "idle":
                        row = runner.idle(name, args.idle_settle, args.idle_window)
                    elif workload == "flood-memory":
                        row = runner.flood_memory(name, flood)
                    else:
                        row = runner.vtebench(name, vtebench, benchmarks,
                                              out_dir / f"{name}-r{round_index}.dat", args.vtebench_seconds)
                    row["at"] = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
                    row["load"] = list(os.getloadavg()[:2])
                    rows[name].append(row)
                    recorder.write()
                    print(f"{workload} round {round_index} {name}: {json.dumps(row)}", flush=True)
                    time.sleep(1.0)

    results["meta"]["complete"] = True
    results["meta"]["countable"] = session_countable(results["meta"]) and rounds_complete(results, results["meta"])
    recorder.write()
    summary = summarize(results, names, bool(args.kettle_b))
    (out_dir / "summary.md").write_text(summary + "\n")
    print(summary)
    return 0


def rotated(names: List[str], round_index: int) -> List[str]:
    shift = round_index % len(names)
    return names[shift:] + names[:shift]


if __name__ == "__main__":
    sys.exit(main())
