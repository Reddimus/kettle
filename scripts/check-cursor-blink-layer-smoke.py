#!/usr/bin/env python3
"""Live smoke for the macOS Core Animation cursor blink.

Once a focused window has been quiet for a blink half-period, Kettle hands the
blink to a Core Animation layer and stops presenting frames. This drives a real
window and reads `ui_geometry.cursor_blink`, which draws no frame of its own:

  1. the blink moves to the layer within three half-periods of the last frame,
     over the cursor cell;
  2. reading `ui_geometry` does not end it;
  3. from 1.5 s after the hand-off to the blink timeout, the process holds
     under --max-footprint-mib, wakes at most --max-wakeups per second and
     uses at most --max-cpu-percent;
  4. after the timeout the layer rests visible and nothing is scheduled;
  5. a config reload ends the blink with one exit frame, and the layer takes
     over again after a quiet half-period;
  6. a key sent to the pane ends it too.

--pixels also captures the window with `screencapture -l` inside each
blink phase, retrying captures whose timing could cross an edge, for block,
bar and underline cursors in an opaque window and at
0.86 opacity with blur, once with `macos-cursor-blink-layer = true` and once
with it false. Each run's on and off captures must differ at the cursor, and
the two runs must match byte for byte in each phase (translucent runs within
the cursor patch and a two-cell margin). A capture counts only while the
expected renderer drew its phase throughout. The process that runs this
needs Screen Recording; without it the captures come back without the window
and this fails rather than reporting a pass.

Artifacts land under target/diagnostics/cursor-blink-layer-*. Pixel captures
use a private temporary directory and are deleted even when a check fails.
"""

from __future__ import annotations

import argparse
import ctypes
import importlib.util
import json
import tempfile
import platform
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Tuple

INTERVAL_MS = 300
TIMEOUT_S = 12


def load_live_helpers():
    path = Path(__file__).with_name("check-live-ui-smoke.py")
    spec = importlib.util.spec_from_file_location("kettle_cursor_blink_live", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class RusageInfoV4(ctypes.Structure):
    # <sys/resource.h> rusage_info_v4, up to the fields this reads.
    _fields_ = [
        ("ri_uuid", ctypes.c_uint8 * 16),
        ("ri_user_time", ctypes.c_uint64),
        ("ri_system_time", ctypes.c_uint64),
        ("ri_pkg_idle_wkups", ctypes.c_uint64),
        ("ri_interrupt_wkups", ctypes.c_uint64),
        ("ri_pageins", ctypes.c_uint64),
        ("ri_wired_size", ctypes.c_uint64),
        ("ri_resident_size", ctypes.c_uint64),
        ("ri_phys_footprint", ctypes.c_uint64),
        ("ri_rest", ctypes.c_uint64 * 64),
    ]


class TimebaseInfo(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


def usage(pid: int) -> Dict[str, float]:
    """phys_footprint in MiB, CPU time in ns and wakeups, as memsample.c reads them."""
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    info = RusageInfoV4()
    # RUSAGE_INFO_V4 = 4.
    if libproc.proc_pid_rusage(pid, 4, ctypes.byref(info)) != 0:
        raise SystemExit(f"cursor-blink-layer smoke: proc_pid_rusage({pid}) failed")
    base = TimebaseInfo()
    ctypes.CDLL("/usr/lib/libSystem.dylib").mach_timebase_info(ctypes.byref(base))
    ticks = info.ri_user_time + info.ri_system_time
    return {
        "t": time.monotonic(),
        "footprint_mib": info.ri_phys_footprint / (1024 * 1024),
        "cpu_ns": ticks * base.numer / base.denom,
        "wakeups": float(info.ri_pkg_idle_wkups + info.ri_interrupt_wkups),
    }


def blink(live) -> Dict[str, object]:
    value = live.json_ctl("ui_geometry").get("cursor_blink")
    if not isinstance(value, dict):
        raise SystemExit(f"cursor-blink-layer smoke: ui_geometry has no cursor_blink: {value!r}")
    return value


def wait_for_layer(live, label: str, timeout: float) -> Tuple[Dict[str, object], float]:
    """Poll until the layer blinks; returns the state and when it was seen."""
    deadline = time.monotonic() + timeout
    state = blink(live)
    while state.get("renderer") != "layer":
        if time.monotonic() > deadline:
            raise SystemExit(f"cursor-blink-layer smoke: no hand-off {label}: {state}")
        time.sleep(0.05)
        state = blink(live)
    return state, time.monotonic()


def expected_cursor_rect(live) -> Tuple[float, float, float, float]:
    """The focused pane's cursor cell in surface pixels. Call before hand-off."""
    screen = live.json_ctl("read_screen")
    col, row = screen["cursor"][1], screen["cursor"][0]
    geometry = live.json_ctl("ui_geometry")
    pane = next(p for p in geometry["panes"] if p.get("focused"))
    x, y = pane["rect"]["x"], pane["rect"]["y"]
    cell_w, cell_h = geometry["cell"]["width"], geometry["cell"]["height"]
    scale = geometry.get("scale_factor", 1.0)
    pad_x = geometry["padding"]["x"] * scale
    pad_y = geometry["padding"]["y"] * scale
    return (x + pad_x + col * cell_w, y + pad_y + row * cell_h, cell_w, cell_h)


def check_rect(state: Dict[str, object], expected: Tuple[float, float, float, float]) -> None:
    rect = state.get("layer_rect")
    if not isinstance(rect, list) or len(rect) != 4:
        raise SystemExit(f"cursor-blink-layer smoke: no layer_rect: {state}")
    x, y, w, h = rect
    ex, ey, ew, eh = expected
    # The patch is the block quad's pixel box (floor/ceil of its edges).
    if abs(x - ex) > 1 or abs(y - ey) > 1 or not (ew <= w <= ew + 2) or not (eh <= h <= eh + 2):
        raise SystemExit(
            f"cursor-blink-layer smoke: layer_rect {rect} is not the cursor cell {expected}"
        )


def idle_metrics(samples: List[Dict[str, float]]) -> Dict[str, float]:
    first, last = samples[0], samples[-1]
    span = last["t"] - first["t"]
    if span < 3.0:
        raise SystemExit(f"cursor-blink-layer smoke: idle sample too short: {span:.3f} s")
    return {
        "span_s": span,
        "peak_mib": max(sample["footprint_mib"] for sample in samples),
        "wakeups_per_s": (last["wakeups"] - first["wakeups"]) / span,
        "cpu_percent": (last["cpu_ns"] - first["cpu_ns"]) / (span * 1e9) * 100,
    }


def config_text(extra: List[str], timeout_s: int) -> str:
    return "\n".join(
        [
            "agent-server = full",
            "restore-session = false",
            "update-check = false",
            "status-bar = off",
            "tab-bar = off",
            "cursor-blink = true",
            f"cursor-blink-interval = {INTERVAL_MS}",
            f"cursor-blink-timeout = {timeout_s}",
            "window-width = 80",
            "window-height = 24",
            "window-position-x = 120",
            "window-position-y = 120",
        ]
        + extra
    ) + "\n"


def launch(helpers, kettle: str, out: Path, name: str, extra: List[str], timeout_s: int = TIMEOUT_S):
    cfg = out / f"{name}.config"
    cfg.write_text(config_text(extra, timeout_s))
    return cfg, helpers.LiveKettle(
        kettle,
        cfg,
        out / f"{name}.log",
        extra_args=["-e", "/bin/sh"],
        extra_env={"PS1": "$ ", "ENV": None},
    )


def focus(helpers, live) -> None:
    helpers.focus_live_kettle_window(live)
    deadline = time.monotonic() + 10
    while not live.json_ctl("ui_geometry").get("window_focused"):
        if time.monotonic() > deadline:
            raise SystemExit("cursor-blink-layer smoke: the window never got focus")
        time.sleep(0.1)


def run_contract(helpers, kettle: str, out: Path, args) -> Dict[str, object]:
    analysis: Dict[str, object] = {}
    interval = INTERVAL_MS / 1000
    entry_bound = 3 * interval + 1.0
    cfg, live = launch(helpers, kettle, out, "contract", [])
    with live:
        focus(helpers, live)
        time.sleep(1.0)
        expected = expected_cursor_rect(live)
        # read_screen above drew a frame: the hand-off follows within three
        # half-periods (the next edge, then a quiet one, then an off edge).
        started = time.monotonic()
        state, seen = wait_for_layer(live, "after the last frame", entry_bound)
        analysis["handoff_after_s"] = seen - started
        # Raw evidence for later analysis, all on this process's monotonic
        # clock in seconds: phase stamps, every rusage sample and each
        # geometry read, beside the aggregates checked here.
        analysis["clock"] = "python-monotonic-s"
        stamps = {"wait_started": started, "handoff_seen": seen,
                  "interval_ms": INTERVAL_MS, "timeout_s": TIMEOUT_S}
        analysis["stamps"] = stamps
        check_rect(state, expected)
        analysis["handoff"] = state

        # Footprint, wakeups and CPU from hand-off + 1.5 s to the timeout.
        time.sleep(max(0.0, seen + 1.5 - time.monotonic()))
        first = usage(live.pid)
        samples = [first]
        end = first["t"] + 3.5
        while time.monotonic() < end:
            time.sleep(0.5)
            samples.append(usage(live.pid))
        analysis["samples"] = samples
        stamps.update(measure_start=first["t"], measure_end=samples[-1]["t"], sample_period_s=0.5)
        idle = idle_metrics(samples)
        span, peak = idle["span_s"], idle["peak_mib"]
        wakeups, cpu = idle["wakeups_per_s"], idle["cpu_percent"]
        analysis["idle"] = idle
        if peak > args.max_footprint_mib:
            raise SystemExit(f"cursor-blink-layer smoke: {peak:.1f} MiB while the layer blinks")
        if wakeups > args.max_wakeups:
            raise SystemExit(f"cursor-blink-layer smoke: {wakeups:.2f} wakeups/s while the layer blinks")
        if cpu > args.max_cpu_percent:
            raise SystemExit(f"cursor-blink-layer smoke: {cpu:.4f} % CPU while the layer blinks")

        # Reading ui_geometry must not end the blink.
        exits = state["exits"]
        reads = []
        for _ in range(20):
            read_at = time.monotonic()
            again = blink(live)
            reads.append({"t": read_at, **{key: again.get(key) for key in ("renderer", "handoffs", "exits", "hides")}})
            if again["renderer"] != "layer" or again["exits"] != exits:
                raise SystemExit(f"cursor-blink-layer smoke: ui_geometry ended the blink: {again}")
            time.sleep(0.1)
        analysis["geometry_reads"] = reads

        # Wait past the activity timeout and its last visible edge.
        time.sleep(max(0.0, started + TIMEOUT_S + 2 * interval - time.monotonic()))
        stamps["rest_read"] = time.monotonic()
        rested = blink(live)
        if rested["renderer"] != "layer" or not rested["phase_on"] or rested["next_edge_ms"] is not None:
            raise SystemExit(f"cursor-blink-layer smoke: the blink did not rest visible: {rested}")
        analysis["rested"] = rested

        # A config reload is activity: one exit frame, then a new hand-off.
        stamps["reload_written"] = time.monotonic()
        cfg.write_text(cfg.read_text() + "# reload\n")
        deadline = time.monotonic() + 5
        state = blink(live)
        while state["exits"] == rested["exits"]:
            if time.monotonic() > deadline:
                raise SystemExit(f"cursor-blink-layer smoke: a reload did not end the blink: {state}")
            time.sleep(0.05)
            state = blink(live)
        if state["exits"] != rested["exits"] + 1:
            raise SystemExit(f"cursor-blink-layer smoke: a reload ended the blink twice: {state}")
        state, _ = wait_for_layer(live, "after a reload", entry_bound)
        analysis["after_reload"] = state

        # A key is output and activity: it ends the blink too.
        stamps["key_sent"] = time.monotonic()
        live.ctl("send_keys", params={"keys": ["enter"]})
        deadline = time.monotonic() + 5
        while blink(live)["exits"] == state["exits"]:
            if time.monotonic() > deadline:
                raise SystemExit("cursor-blink-layer smoke: a key did not end the blink")
            time.sleep(0.05)
        final = blink(live)
        analysis["after_key"] = final
        if final.get("fallback") is not None:
            raise SystemExit(f"cursor-blink-layer smoke: the window fell back: {final}")
    return analysis


def window_id(helpers, pid: int) -> int:
    ids = helpers.native_visible_window_ids(pid)
    if len(ids) != 1:
        raise SystemExit(f"cursor-blink-layer smoke: expected one window for {pid}, found {ids}")
    return next(iter(ids))


# Counters that change when the blink moves between Kettle and the layer.
BLINK_COUNTERS = ("handoffs", "exits", "hides")


def capture_phases(helpers, live, out: Path, name: str, renderer: str) -> Dict[bool, Path]:
    """Keep captures only when their entire command fits inside one phase that
    `renderer` ("layer" or "gpu") drew from start to end."""
    wid = window_id(helpers, live.pid)
    interval = INTERVAL_MS / 1000
    captures: Dict[bool, Path] = {}
    deadline = time.monotonic() + 10
    margin = 0.020
    while len(captures) < 2:
        if time.monotonic() > deadline:
            raise SystemExit(f"cursor-blink-layer smoke: {name}: could not capture both phases")
        query_start = time.monotonic()
        state = blink(live)
        query_end = time.monotonic()
        wait_ms = state.get("next_edge_ms")
        if wait_ms is None:
            raise SystemExit(f"cursor-blink-layer smoke: {name}: the blink stopped: {state}")
        phase = state["phase_on"]
        if state.get("renderer") != renderer:
            # A frame returned the blink to Kettle. A later edge hands it
            # back; until then a capture would compare the wrong renderer.
            time.sleep(max(0.0, min(0.05, deadline - query_end)))
            continue
        if phase in captures:
            time.sleep(max(0.0, min(wait_ms / 1000 + interval / 2, deadline - query_end)))
            continue
        # The server sampled during the query and truncated next_edge_ms.
        # Its next edge lies between these bounds. Use the latest start and
        # earliest end, leaving a margin for compositor settling at each edge.
        edge_earliest = query_start + wait_ms / 1000
        edge_latest = query_end + (wait_ms + 1) / 1000
        safe_start = edge_latest - interval + margin
        safe_end = edge_earliest - margin
        if wait_ms / 1000 > interval / 2:
            time.sleep(max(0.0, min(wait_ms / 1000 - interval / 2, deadline - query_end)))
            continue  # Read again after sleeping; do not label from old state.
        if query_end < safe_start or query_end >= safe_end:
            time.sleep(max(0.0, min(0.01, deadline - query_end)))
            continue
        path = out / f"{name}-{'on' if phase else 'off'}.png"
        path.touch(mode=0o600, exist_ok=False)
        capture_start = time.monotonic()
        try:
            subprocess.run(["screencapture", f"-l{wid}", "-o", "-x", str(path)], check=True, timeout=max(0.001, deadline - capture_start), umask=0o077)
        except subprocess.TimeoutExpired:
            # The capture outlasted the budget; the deadline check reports it.
            path.unlink(missing_ok=True)
            continue
        capture_end = time.monotonic()
        path.chmod(0o600)
        if not (safe_start <= capture_start <= capture_end <= min(safe_end, deadline)):
            path.unlink()
            continue
        after = blink(live)
        if after["phase_on"] != phase or after.get("renderer") != renderer or any(
            after.get(key) != state.get(key) for key in BLINK_COUNTERS
        ):
            path.unlink()
            continue
        captures.setdefault(phase, path)
    return captures


def run_pixels(helpers, kettle: str, out: Path) -> Dict[str, object]:
    # Translucent captures can contain the desktop. Keep them private and
    # remove them on success or failure, retaining only comparison results.
    with tempfile.TemporaryDirectory(prefix="pixels-", dir=out) as private:
        return compare_pixels(helpers, kettle, Path(private))


def cursor_region_differs(on, off, rect) -> bool:
    """Whether two RGBA captures differ anywhere inside the device-pixel rect."""
    x, y, w, h = rect
    return any(on[2][r][4 * x : 4 * (x + w)] != off[2][r][4 * x : 4 * (x + w)] for r in range(y, min(on[1], y + h)))


def check_captures(label: str, style: str, images, rect, surface) -> Dict[str, bool]:
    """Compare `images[layer][phase]`, each (width, height, rows) RGBA.

    Each run must show the blink: its on and off captures differ inside the
    cursor patch, so a blank, constant or missing window cannot pass. Then the
    layer and GPU runs must match in each phase.
    """
    results: Dict[str, bool] = {}
    for layer, phases in images.items():
        run = "layer" if layer else "GPU"
        for image in phases.values():
            # Borderless windows have no titlebar offset. Refuse an
            # unexpected capture size rather than guessing an origin.
            if image[:2] != (surface["width"], surface["height"]):
                raise SystemExit(f"cursor-blink-layer smoke: {label}: capture does not match the borderless surface")
        if not cursor_region_differs(phases[True], phases[False], rect):
            raise SystemExit(f"cursor-blink-layer smoke: {label}: the {run} captures show no blink at the cursor")
        results[f"{label}-{run.lower()}-blinks"] = True
    for phase in (True, False):
        a = images[True][phase]
        b = images[False][phase]
        if style == "opaque":
            same = a[2] == b[2]
        else:
            x, y, w, h = rect
            margin_x, margin_y = 2 * w, 2 * h
            x0, x1 = max(0, x - margin_x), min(a[0], x + w + margin_x)
            y0, y1 = max(0, y - margin_y), min(a[1], y + h + margin_y)
            same = all(a[2][r][4 * x0 : 4 * x1] == b[2][r][4 * x0 : 4 * x1] for r in range(y0, y1))
        results[f"{label}-{'on' if phase else 'off'}"] = same
        if not same:
            raise SystemExit(
                f"cursor-blink-layer smoke: {label} {'on' if phase else 'off'} phase "
                "differs between the layer and the GPU blink"
            )
    return results


def compare_pixels(helpers, kettle: str, out: Path) -> Dict[str, object]:
    analysis: Dict[str, object] = {}
    for shape in ("block", "bar", "underline"):
        for style, lines in (
            ("opaque", ["background-opacity = 1.0", "background-blur = false"]),
            ("translucent", ["background-opacity = 0.86", "background-blur = true"]),
        ):
            runs: Dict[bool, Dict[bool, Path]] = {}
            rect = None
            for layer in (True, False):
                name = f"{shape}-{style}-{'layer' if layer else 'gpu'}"
                extra = ["borderless = true", f"cursor-style = {shape}", f"macos-cursor-blink-layer = {'true' if layer else 'false'}"] + lines
                # Timeout 0 blinks through every capture.
                _, live = launch(helpers, kettle, out, name, extra, timeout_s=0)
                with live:
                    focus(helpers, live)
                    time.sleep(1.0)
                    surface = live.json_ctl("ui_geometry")["surface"]
                    if layer:
                        state, _ = wait_for_layer(live, name, 3 * INTERVAL_MS / 1000 + 1.0)
                        rect = state["layer_rect"]
                    else:
                        time.sleep(1.0)
                    runs[layer] = capture_phases(helpers, live, out, name, "layer" if layer else "gpu")
            images = {
                layer: {phase: helpers.read_rgba_png(path) for phase, path in phases.items()}
                for layer, phases in runs.items()
            }
            analysis.update(check_captures(f"{shape}-{style}", style, images, rect, surface))
    return analysis


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--kettle", default="./target/release/kettle")
    parser.add_argument("--out-dir", default="target/diagnostics")
    parser.add_argument("--pixels", action="store_true", help="also compare on-screen captures")
    parser.add_argument("--max-footprint-mib", type=float, default=80.0)
    parser.add_argument("--max-wakeups", type=float, default=0.5)
    parser.add_argument("--max-cpu-percent", type=float, default=0.02)
    args = parser.parse_args()
    if platform.system() != "Darwin":
        print("cursor-blink-layer smoke: macOS only; nothing to check here")
        return 0
    helpers = load_live_helpers()
    out = Path(args.out_dir).resolve() / f"cursor-blink-layer-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    out.chmod(0o700)
    analysis = {"contract": run_contract(helpers, args.kettle, out, args)}
    if args.pixels:
        analysis["pixels"] = run_pixels(helpers, args.kettle, out)
    (out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    print(f"cursor-blink-layer smoke: OK artifacts={out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
