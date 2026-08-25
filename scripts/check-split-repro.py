#!/usr/bin/env python3
"""Hunt for the split that never loads.

A split sometimes produces a pane that never becomes usable. Two mechanisms
can do that and they look identical from the outside:

  1. `split_geometry` / `split_with_geometry` returned an error. No pane is
     grafted at all, and until `report_split_failure` landed the only trace was
     a `warn` line nobody sees at the default level.
  2. The split cloned the source pane's *foreground* shell, and what the
     background process scan had latched was a transient helper. The clone runs
     its command, exits, and the pane is reaped. It flashes and is gone.

Polling fast is what separates them: mechanism 1 never reaches two panes,
mechanism 2 reaches two and falls back to one. A single check a second later
would report both as "the split did not work".

This is a hunt, not a contract, so it is not a `check-live-ui-smoke.py` case
and it is in no gate. Run it on demand:

    just split-repro                 # free shell-churn fixture
    just split-repro --claude        # a real Claude Code pane

Exit codes: 0 clean, 2 reproduced (the capture directory is printed), 1 the
harness itself could not run.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional

REPRODUCED = 2
HARNESS_ERROR = 1

# The shell-churn fixture: a foreground process that keeps spawning short-lived
# shell children whose script is deleted underneath them. That is the shape the
# scan can latch and the split can clone, without any model call.
CHURN = (
    "while :; do "
    "s=$(mktemp); printf 'sleep 0.4\\n' > \"$s\"; "
    "bash \"$s\" & rm -f \"$s\"; "
    "sleep 0.2; "
    "done"
)


def ctl(kettle: str, pid: int, method: str, *, params: Optional[Dict] = None,
        timeout: float = 10.0) -> Dict:
    argv = [kettle, "ctl", "--pid", str(pid), method, "--raw"]
    if params is not None:
        argv += ["--json", json.dumps(params)]
    cp = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    if cp.returncode != 0:
        raise RuntimeError(f"kettle ctl {method} failed: {cp.stderr.strip()}")
    return json.loads(cp.stdout) if cp.stdout.strip() else {}


def panes(kettle: str, pid: int) -> List[Dict]:
    listed = ctl(kettle, pid, "list_panes").get("panes")
    return [p for p in listed if isinstance(p, dict)] if isinstance(listed, list) else []


def process_tree(root_pid: object) -> str:
    if not isinstance(root_pid, int):
        return "no child pid reported\n"
    cp = subprocess.run(
        ["ps", "-axo", "pid,ppid,pgid,stat,command"],
        capture_output=True, text=True,
    )
    rows = cp.stdout.splitlines()
    keep, frontier = [rows[0]] if rows else [], {root_pid}
    # Two passes are enough for claude -> helper shell -> command; a third
    # catches anything that re-execs. Cheap either way.
    for _ in range(3):
        for row in rows[1:]:
            parts = row.split(None, 4)
            if len(parts) < 5:
                continue
            pid, ppid = int(parts[0]), int(parts[1])
            if (pid in frontier or ppid in frontier) and row not in keep:
                keep.append(row)
                frontier.add(pid)
    return "\n".join(keep) + "\n"


def capture(out: Path, cycle: int, kettle: str, pid: int, base: Dict,
            observed: List[Dict], reason: str, log: Path) -> Path:
    bundle = out / f"cycle-{cycle:03d}"
    bundle.mkdir(parents=True, exist_ok=True)
    (bundle / "reason.txt").write_text(reason + "\n")
    (bundle / "panes.json").write_text(json.dumps(observed, indent=2) + "\n")
    try:
        (bundle / "ui_geometry.json").write_text(
            json.dumps(ctl(kettle, pid, "ui_geometry"), indent=2) + "\n")
    except Exception as error:                                # noqa: BLE001
        (bundle / "ui_geometry.json").write_text(f"unavailable: {error}\n")
    for pane in observed:
        if pane.get("id") == base.get("id"):
            continue
        try:
            (bundle / f"screen-pane-{pane.get('id')}.txt").write_text(
                str(ctl(kettle, pid, "read_screen",
                        params={"pane": pane.get("id")}).get("text", "")))
        except Exception:                                     # noqa: BLE001, S110
            pass
    (bundle / "ps-tree.txt").write_text(process_tree(base.get("child_pid")))
    if log.exists():
        shutil.copy2(log, bundle / "kettle.log")
        hits = [line for line in log.read_text(errors="replace").splitlines()
                if "could not split pane" in line]
        (bundle / "split-errors.txt").write_text("\n".join(hits) + "\n")
    return bundle


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kettle", default=os.environ.get("KETTLE_BIN", "kettle"))
    parser.add_argument("--cycles", type=int,
                        default=int(os.environ.get("KETTLE_SPLIT_REPRO_CYCLES", "40")))
    parser.add_argument("--wall-clock", type=float,
                        default=float(os.environ.get("KETTLE_SPLIT_REPRO_SECONDS", "300")))
    parser.add_argument("--claude", action="store_true",
                        help="drive a real Claude Code pane instead of the fixture")
    parser.add_argument("--out-dir", default=os.environ.get("KETTLE_DIAG_DIR", "target/diagnostics"))
    args = parser.parse_args()

    here = Path(__file__).resolve().parent
    check = subprocess.run(
        [sys.executable, str(here / "check-live-ui-smoke.py"), "session-check"],
        capture_output=True, text=True)
    if check.returncode != 0:
        print(check.stderr.strip() or "no graphical session", file=sys.stderr)
        return HARNESS_ERROR
    if args.claude and shutil.which("claude") is None:
        print("split repro: claude not found; skipping", file=sys.stderr)
        return 0

    out = Path(args.out_dir).resolve() / f"split-repro-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text("\n".join([
        "agent-server = full",
        # No confirm dialog on the close half of a cycle, or a cycle can stall
        # on an unanswered modal and look like the bug.
        "ask-before-closing = never",
        "tab-bar = never",
        "status-bar = off",
        "restore-session = false",
        "update-check = false",
        "window-width = 200",
        "window-height = 50",
    ]) + "\n")

    log = out / "kettle.log"
    proc = subprocess.Popen(
        [args.kettle, "--config", str(cfg), "--agent-server", "full"],
        stdout=log.open("wb"), stderr=subprocess.STDOUT)
    pid = proc.pid
    try:
        deadline = time.monotonic() + 20
        while True:
            try:
                if panes(args.kettle, pid):
                    break
            except Exception:                                 # noqa: BLE001
                pass
            if proc.poll() is not None or time.monotonic() > deadline:
                print("split repro: control server never came up", file=sys.stderr)
                return HARNESS_ERROR
            time.sleep(0.1)

        base = panes(args.kettle, pid)[0]
        prime = "claude\r" if args.claude else CHURN + "\r"
        subprocess.run([args.kettle, "ctl", "--pid", str(pid), "send_text",
                        "--json", json.dumps({"text": prime})], check=True,
                       capture_output=True, text=True)
        # Give the foreground-process scan a chance to publish a snapshot that
        # includes whatever the primed process spawned.
        time.sleep(6 if args.claude else 3)

        stop = time.monotonic() + args.wall_clock
        for cycle in range(1, args.cycles + 1):
            if time.monotonic() > stop:
                print(f"split repro: wall-clock cap reached after {cycle - 1} cycles")
                break
            direction = "split_right" if cycle % 2 else "split_down"
            ctl(args.kettle, pid, "perform_action", params={"action": direction})

            reached_two, fell_back, observed = False, False, []
            watch = time.monotonic() + 3.0
            while time.monotonic() < watch:
                observed = panes(args.kettle, pid)
                if len(observed) >= 2:
                    reached_two = True
                elif reached_two:
                    fell_back = True
                    break
                time.sleep(0.05)

            if not reached_two:
                bundle = capture(out, cycle, args.kettle, pid, base, observed,
                                 "no second pane ever appeared: the spawn failed",
                                 log)
                print(f"split repro: REPRODUCED on cycle {cycle}: {bundle}")
                return REPRODUCED
            if fell_back:
                bundle = capture(out, cycle, args.kettle, pid, base, observed,
                                 "the new pane appeared and died: the cloned "
                                 "command exited immediately", log)
                print(f"split repro: REPRODUCED on cycle {cycle}: {bundle}")
                return REPRODUCED

            new = next((p for p in observed if p.get("id") != base.get("id")), None)
            token = f"KETTLE_SPLIT_ALIVE_{cycle}"
            subprocess.run(
                [args.kettle, "ctl", "--pid", str(pid), "send_text", "--json",
                 json.dumps({"pane": new.get("id"), "text": f"echo {token}\r"})],
                check=True, capture_output=True, text=True)
            alive, watch = False, time.monotonic() + 5.0
            while time.monotonic() < watch:
                screen = str(ctl(args.kettle, pid, "read_screen",
                                 params={"pane": new.get("id")}).get("text", ""))
                if screen.count(token) >= 2:   # the echo of the line, then its output
                    alive = True
                    break
                time.sleep(0.1)
            if not alive:
                bundle = capture(out, cycle, args.kettle, pid, base,
                                 panes(args.kettle, pid),
                                 "the new pane exists but never ran a command",
                                 log)
                print(f"split repro: REPRODUCED on cycle {cycle}: {bundle}")
                return REPRODUCED

            ctl(args.kettle, pid, "perform_action", params={"action": "close_pane"})
            watch = time.monotonic() + 5.0
            while time.monotonic() < watch and len(panes(args.kettle, pid)) > 1:
                time.sleep(0.05)
        else:
            print(f"split repro: {args.cycles} cycles clean")
        print(f"split repro: not reproduced. artifacts={out}")
        return 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
