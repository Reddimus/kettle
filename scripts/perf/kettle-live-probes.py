#!/usr/bin/env python3
"""Kettle-only live performance probes driven through `kettle ctl`.

These probes complement the cross-terminal Hyperfine timings in
`linux-compare.sh`. They intentionally measure real interactive window paths
that peer terminals cannot be driven through Kettle's control plane:

  - resize_window: OS window resize request -> Kettle resize handling observed
    through ui_geometry
  - scrollback navigation: generated scrollback -> perform_action page
    movement -> read_screen display_offset observation

The timings include control-plane round-trip overhead. That is acceptable for
regression tracking because every run uses the same Kettle binary and same
control path.
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, List, Optional


def run(argv: List[str], *, timeout: float, capture: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        timeout=timeout,
        check=False,
    )


def median(values: List[float]) -> float:
    if not values:
        raise SystemExit("kettle-live-probes: internal error: empty timing sample")
    return float(statistics.median(values))


class LiveKettle:
    def __init__(self, kettle: Path, config: Path, log: Path) -> None:
        self.kettle = kettle
        self.config = config
        self.log = log
        self.proc: Optional[subprocess.Popen[bytes]] = None

    def __enter__(self) -> "LiveKettle":
        log_f = self.log.open("wb")
        self.proc = subprocess.Popen(
            [
                str(self.kettle),
                "--config",
                str(self.config),
                "--agent-server",
                "full",
            ],
            stdout=log_f,
            stderr=subprocess.STDOUT,
        )
        log_f.close()
        deadline = time.monotonic() + 25.0
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(
                    "kettle-live-probes: kettle exited before control server was ready\n"
                    + self.log.read_text(errors="replace")
                )
            probe = self.ctl("list_panes", raw=True, allow_fail=True, timeout=2.0)
            if probe.returncode == 0:
                return self
            time.sleep(0.1)
        raise SystemExit("kettle-live-probes: timed out waiting for control server")

    def __exit__(self, *_exc: object) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)

    @property
    def pid(self) -> int:
        assert self.proc is not None
        return int(self.proc.pid)

    def ctl(
        self,
        method: str,
        *,
        params: Optional[Dict[str, Any]] = None,
        raw: bool = False,
        allow_fail: bool = False,
        timeout: float = 10.0,
    ) -> subprocess.CompletedProcess[str]:
        argv = [str(self.kettle), "ctl", "--pid", str(self.pid), method]
        if params is not None:
            argv += ["--json", json.dumps(params, separators=(",", ":"))]
        if raw:
            argv.append("--raw")
        cp = run(argv, timeout=timeout)
        if cp.returncode != 0 and not allow_fail:
            raise SystemExit(
                f"kettle-live-probes: kettle ctl {method} failed\nSTDERR:\n{cp.stderr}\nSTDOUT:\n{cp.stdout}"
            )
        return cp

    def json_ctl(
        self, method: str, params: Optional[Dict[str, Any]] = None, *, timeout: float = 10.0
    ) -> Dict[str, Any]:
        return json.loads(self.ctl(method, params=params, raw=True, timeout=timeout).stdout)


def wait_for_surface(live: LiveKettle, width: int, height: int) -> Dict[str, Any]:
    deadline = time.monotonic() + 5.0
    last: Dict[str, Any] = {}
    while time.monotonic() < deadline:
        last = live.json_ctl("ui_geometry", timeout=3.0)
        surface = last.get("surface", {})
        if int(surface.get("width", -1)) == width and int(surface.get("height", -1)) == height:
            return last
        time.sleep(0.03)
    raise SystemExit(
        "kettle-live-probes: resize_window did not reach "
        f"{width}x{height}; last geometry={json.dumps(last, sort_keys=True)}"
    )


def resize_probe(live: LiveKettle, cycles: int) -> Dict[str, Any]:
    sizes = [(900, 560), (1120, 700), (820, 540), (1040, 640)]
    samples_ms: List[float] = []
    observations: List[Dict[str, Any]] = []
    for i in range(cycles):
        width, height = sizes[i % len(sizes)]
        t0 = time.perf_counter()
        live.ctl("resize_window", params={"width": width, "height": height}, timeout=5.0)
        geo = wait_for_surface(live, width, height)
        samples_ms.append((time.perf_counter() - t0) * 1000.0)
        if i < len(sizes):
            observations.append(
                {
                    "width": width,
                    "height": height,
                    "content": geo.get("content"),
                    "resize_overlay": geo.get("resize_overlay"),
                }
            )
    return {
        "workload": "kettle_live_resize",
        "unit": "ms",
        "cycles": cycles,
        "samples_ms": samples_ms,
        "median_ms": median(samples_ms),
        "p95_ms": sorted(samples_ms)[max(0, int(len(samples_ms) * 0.95) - 1)],
        "observations": observations,
    }


def make_scrollback(live: LiveKettle) -> None:
    command = (
        "for i in $(seq 1 1600); do "
        "printf '\\033[4mKETTLE_PERF_SCROLL_%04d\\033[24m underlined-link https://example.invalid/%04d\\n' \"$i\" \"$i\"; "
        "done; printf 'KETTLE_PERF_SCROLL_DONE\\n'"
    )
    live.ctl("send_text", params={"text": command}, timeout=5.0)
    live.ctl("send_keys", params={"keys": ["enter"]}, timeout=5.0)
    result = live.json_ctl(
        "wait_for",
        params={"text": "KETTLE_PERF_SCROLL_DONE", "timeout_ms": 25000, "quiet_ms": 250},
        timeout=30.0,
    )
    if not result.get("matched"):
        raise SystemExit(f"kettle-live-probes: scrollback generator timed out: {result}")


def scrollback_probe(live: LiveKettle, cycles: int) -> Dict[str, Any]:
    make_scrollback(live)
    top_samples_ms: List[float] = []
    bottom_samples_ms: List[float] = []
    offsets: List[int] = []

    for _ in range(cycles):
        t0 = time.perf_counter()
        live.ctl("perform_action", params={"action": "scroll_page_up"}, timeout=5.0)
        screen = live.json_ctl("read_screen", timeout=5.0)
        top_samples_ms.append((time.perf_counter() - t0) * 1000.0)
        offset = int(screen.get("display_offset", 0))
        offsets.append(offset)
        if offset <= 0:
            raise SystemExit("kettle-live-probes: scroll_page_up did not enter scrollback")

        t1 = time.perf_counter()
        live.ctl("perform_action", params={"action": "scroll_page_down"}, timeout=5.0)
        live.json_ctl("read_screen", timeout=5.0)
        bottom_samples_ms.append((time.perf_counter() - t1) * 1000.0)

    live.ctl("perform_action", params={"action": "scroll_to_bottom"}, timeout=5.0)
    final = live.json_ctl("read_screen", timeout=5.0)
    return {
        "workload": "kettle_live_scrollback_navigation",
        "unit": "ms",
        "cycles": cycles,
        "page_up_samples_ms": top_samples_ms,
        "page_down_samples_ms": bottom_samples_ms,
        "page_up_median_ms": median(top_samples_ms),
        "page_down_median_ms": median(bottom_samples_ms),
        "max_observed_display_offset": max(offsets),
        "final_display_offset": int(final.get("display_offset", -1)),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kettle", required=True, type=Path)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--resize-cycles", type=int, default=16)
    parser.add_argument("--scroll-cycles", type=int, default=12)
    args = parser.parse_args()

    if args.resize_cycles < 1 or args.scroll_cycles < 1:
        raise SystemExit("kettle-live-probes: cycle counts must be positive")
    if not args.kettle.exists():
        raise SystemExit(f"kettle-live-probes: missing kettle binary: {args.kettle}")
    if not args.config.exists():
        raise SystemExit(f"kettle-live-probes: missing config: {args.config}")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="kettle-live-probes-") as tmp:
        log = Path(tmp) / "kettle.log"
        with LiveKettle(args.kettle, args.config, log) as live:
            doc = {
                "kettle": str(args.kettle),
                "config": str(args.config),
                "resize": resize_probe(live, args.resize_cycles),
                "scrollback_navigation": scrollback_probe(live, args.scroll_cycles),
                "rules": {
                    "state": "probe fails if resize geometry does not settle or scroll actions do not move the viewport",
                    "timing": "timings are advisory control-plane medians until peer-terminal automation is available",
                },
            }
    args.out.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    print(f"kettle live probes: wrote {args.out}")
    print(
        "resize median={:.1f} ms, scroll page-up median={:.1f} ms, page-down median={:.1f} ms".format(
            doc["resize"]["median_ms"],
            doc["scrollback_navigation"]["page_up_median_ms"],
            doc["scrollback_navigation"]["page_down_median_ms"],
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
