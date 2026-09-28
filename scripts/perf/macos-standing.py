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
  vtebench      Alacritty's vtebench at a pinned revision, per-benchmark
                medians and their geometric mean

Rounds rotate the terminal order so no terminal always runs first. With
--kettle-b the run compares two Kettle builds only and reports the median
paired ratio with a bootstrap 95% interval.

Idle numbers depend on focus: Kettle, Ghostty, and kitty blink the cursor only
in a focused window, and macOS gives a launched window focus only while the
desktop is unlocked and nothing else holds it. Each idle sample records
whether the terminal was frontmost.

Results go to --out-dir as results.json and summary.md.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import random
import resource
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Dict, List, Optional

REPO = Path(__file__).resolve().parents[2]
PROBES = Path(__file__).resolve().parent / "macos-standing"
VTEBENCH_URL = "https://github.com/alacritty/vtebench"
VTEBENCH_REV = "ead80032e57dee2e75f0b51f2ea67528647d9944"
COLS, ROWS = 120, 36
FLOOD_BYTES = 32 * 1024 * 1024

APPS = {
    "alacritty": "/Applications/Alacritty.app/Contents/MacOS/alacritty",
    "kitty": "/Applications/kitty.app/Contents/MacOS/kitty",
    "wezterm": "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
    "ghostty": "/Applications/Ghostty.app/Contents/MacOS/ghostty",
}
WORKLOADS = ("startup", "idle", "flood-memory", "vtebench")
# Reported once per terminal rather than as metrics.
GRID_KEYS = ("cols", "rows")


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


def build_vtebench(tools: Path) -> Path:
    """Clone and build vtebench at the pinned revision into `tools`.

    The checkout sits inside Kettle's workspace root, so the root `Cargo.toml`
    lists it under `workspace.exclude`; the self-test keeps the two in step.
    """
    checkout = tools / "vtebench"
    binary = checkout / "target" / "release" / "vtebench"
    if checkout_vtebench(checkout) or not binary.exists():
        subprocess.run(["cargo", "build", "--release", "--locked", "--quiet"], cwd=checkout, check=True)
    return binary


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
        return subprocess.Popen(
            [str(self.probes["launch"]), str(self.work / "launch.json"), str(stamp), str(timeout), "--", *argv],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )

    def finish(self, process: subprocess.Popen, timeout: float) -> dict:
        returncode = process.wait(timeout=timeout + 15)
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

    def stop(self, process: subprocess.Popen, timeout: float) -> None:
        """Signal the terminal, not the launch probe: the probe owns the
        terminal and records its exit, so stopping the probe would orphan it."""
        pid_file = Path(str(self.work / "stamp") + ".pid")
        if pid_file.exists():
            subprocess.run(["/bin/kill", pid_file.read_text().strip()], capture_output=True)
        try:
            self.finish(process, timeout)
        except subprocess.TimeoutExpired:
            process.kill()

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
        self.wait_for(self.work / "stamp", 20)
        time.sleep(settle)
        first = self.sample()
        time.sleep(window)
        second = self.sample()
        pid_file = Path(str(self.work / "stamp") + ".pid")
        focused = pid_file.exists() and self.frontmost_pid() == int(pid_file.read_text())
        self.stop(process, 30)
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
        self.wait_for(done, 100)
        time.sleep(3)
        sample = self.sample()
        self.stop(process, 60)
        if not sample:
            return {"error": "terminal exited before sampling"}
        return {"footprint_mib": sample["footprint"] / 2**20,
                "max_footprint_mib": sample["max_footprint"] / 2**20,
                "rss_mib": sample["rss"] / 2**20}

    def vtebench(self, name: str, vtebench: Path, dat: Path, seconds: int) -> dict:
        benchmarks = vtebench.parents[2] / "benchmarks"
        process = self.launch(
            name, f'exec "{vtebench}" -s -b "{benchmarks}" --dat "{dat}" --max-secs {seconds}', 900
        )
        self.finish(process, 900)
        return parse_dat(dat.read_text()) if dat.exists() else {"error": "no vtebench output"}


def parse_dat(text: str) -> Dict[str, float]:
    """Median milliseconds per sample for each benchmark in a vtebench DAT file."""
    lines = [line.split() for line in text.splitlines() if line.strip()]
    if not lines:
        return {}
    header, rows = lines[0], lines[1:]
    columns: Dict[str, List[float]] = {name: [] for name in header}
    for row in rows:
        for name, value in zip(header, row):
            if value != "_":
                columns[name].append(float(value))
    return {name: statistics.median(values) for name, values in columns.items() if values}


def geometric_mean(values: List[float]) -> float:
    return math.exp(statistics.mean(math.log(value) for value in values))


def paired(a: List[float], b: List[float], seed: int = 7) -> dict:
    """Median of b/a pairs with a 10,000-resample bootstrap 95% interval."""
    ratios = [y / x for x, y in zip(a, b) if x and y]
    if not ratios:
        return {}
    rng = random.Random(seed)
    boots = sorted(statistics.median(rng.choices(ratios, k=len(ratios))) for _ in range(10_000))
    return {"ratio": statistics.median(ratios), "low": boots[250], "high": boots[9_749], "n": len(ratios)}


def rotated(names: List[str], round_index: int) -> List[str]:
    shift = round_index % len(names)
    return names[shift:] + names[:shift]


def summarize(results: dict, names: List[str], ab: bool) -> str:
    out = ["# macOS standing", "", results["context"], ""]
    for workload, rows in results["workloads"].items():
        out.append(f"## {workload}")
        out.append("")
        if workload == "vtebench":
            benches = sorted({bench for runs in rows.values() for run in runs for bench in run if bench != "error"})
            out.append("| benchmark | " + " | ".join(names) + " |")
            out.append("|---|" + "---:|" * len(names))
            medians = {
                name: {bench: statistics.median([run[bench] for run in rows[name] if bench in run])
                       for bench in benches if any(bench in run for run in rows[name])}
                for name in names
            }
            for bench in benches:
                cells = [f"{medians[name][bench]:.1f}" if bench in medians[name] else "-" for name in names]
                out.append(f"| {bench} | " + " | ".join(cells) + " |")
            geo = [f"{geometric_mean(list(medians[name].values())):.1f}" if medians[name] else "-" for name in names]
            out.append("| **geometric mean** | " + " | ".join(geo) + " |")
        else:
            metrics = sorted({key for runs in rows.values() for run in runs for key, value in run.items()
                              if isinstance(value, (int, float)) and not isinstance(value, bool)
                              and key not in GRID_KEYS})
            grids = {name: sorted({(run["cols"], run["rows"]) for run in rows[name] if "cols" in run})
                     for name in names}
            if any(grids.values()):
                out.append("Grid: " + ", ".join(
                    f"{name} {'/'.join(f'{c}x{r}' for c, r in grid)}" for name, grid in grids.items() if grid))
                out.append("")
            out.append("| metric | " + " | ".join(names) + " |")
            out.append("|---|" + "---:|" * len(names))
            for metric in metrics:
                cells = []
                for name in names:
                    values = [run[metric] for run in rows[name] if isinstance(run.get(metric), (int, float))]
                    cells.append(f"{statistics.median(values):.2f}" if values else "-")
                out.append(f"| {metric} | " + " | ".join(cells) + " |")
            if ab:
                for metric in metrics:
                    a = [run.get(metric) for run in rows[names[0]]]
                    b = [run.get(metric) for run in rows[names[1]]]
                    pairs = [(x, y) for x, y in zip(a, b) if isinstance(x, (int, float)) and isinstance(y, (int, float))]
                    stats = paired([x for x, _ in pairs], [y for _, y in pairs])
                    if stats:
                        out.append(
                            f"\n{metric}: B/A {stats['ratio']:.3f} "
                            f"(95% CI {stats['low']:.3f}-{stats['high']:.3f}, n={stats['n']})"
                        )
        out.append("")
    return "\n".join(out)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--kettle", default=str(REPO / "target" / "release" / "kettle"))
    parser.add_argument("--kettle-b", help="compare this Kettle build against --kettle instead of peers")
    parser.add_argument("--peers", default=",".join(APPS), help="comma list of peer terminals")
    parser.add_argument("--workloads", default=",".join(WORKLOADS))
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--vtebench-rounds", type=int, default=2)
    parser.add_argument("--vtebench-seconds", type=int, default=3)
    parser.add_argument("--idle-settle", type=float, default=20.0)
    parser.add_argument("--idle-window", type=float, default=10.0)
    parser.add_argument("--fd-limit", type=int, help="soft RLIMIT_NOFILE the terminals inherit")
    parser.add_argument("--out-dir", default=str(REPO / "target" / "perf-results" / "macos-standing"))
    parser.add_argument("--no-build", action="store_true", help="use --kettle as built")
    args = parser.parse_args()

    if sys.platform != "darwin":
        print("macos-standing.py: this benchmark requires macOS", file=sys.stderr)
        return 1
    workloads = [w for w in args.workloads.split(",") if w]
    unknown = set(workloads) - set(WORKLOADS)
    if unknown:
        parser.error(f"unknown workloads: {', '.join(sorted(unknown))}")
    if not args.no_build and not args.kettle_b:
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "kettle"], cwd=REPO, check=True)
    if args.fd_limit:
        _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(resource.RLIMIT_NOFILE, (args.fd_limit, hard))

    tools = REPO / "target" / "perf-tools" / "macos-standing"
    probes = build_probes(tools)
    vtebench = build_vtebench(tools) if "vtebench" in workloads else None
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

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

    host = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
    release = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
    load = os.getloadavg()[0]
    results = {
        "context": f"{host}, macOS {release}, load {load:.2f} at start, {args.rounds} rounds, "
                   f"{COLS}x{ROWS} grid, default configs, fd soft limit "
                   f"{resource.getrlimit(resource.RLIMIT_NOFILE)[0]}",
        "terminals": names, "skipped": skipped, "workloads": {},
    }

    with tempfile.TemporaryDirectory(prefix="kettle-standing-") as tmp:
        work = Path(tmp)
        write_configs(work)
        runner = Runner(probes, work, kettle)
        flood = work / "flood.txt"
        if "flood-memory" in workloads:
            write_flood(flood)
        for workload in workloads:
            rows: Dict[str, List[dict]] = {name: [] for name in names}
            rounds = args.vtebench_rounds if workload == "vtebench" else args.rounds
            for round_index in range(rounds):
                for name in rotated(names, round_index):
                    if workload == "startup":
                        row = runner.startup(name)
                    elif workload == "idle":
                        row = runner.idle(name, args.idle_settle, args.idle_window)
                    elif workload == "flood-memory":
                        row = runner.flood_memory(name, flood)
                    else:
                        row = runner.vtebench(name, vtebench, out_dir / f"{name}-r{round_index}.dat",
                                              args.vtebench_seconds)
                    rows[name].append(row)
                    print(f"{workload} round {round_index} {name}: {json.dumps(row)}", flush=True)
                    time.sleep(1.0)
            results["workloads"][workload] = rows

    (out_dir / "results.json").write_text(json.dumps(results, indent=1))
    summary = summarize(results, names, bool(args.kettle_b))
    (out_dir / "summary.md").write_text(summary + "\n")
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
