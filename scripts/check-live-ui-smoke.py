#!/usr/bin/env python3
"""Cross-platform live UI diagnostics for Kettle.

The shell scripts remain the Unix-friendly entrypoints. This script exists so
Windows `just` recipes can run the same live tab/underline checks without Bash.
It intentionally uses only Python stdlib plus `kettle ctl`.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import queue
import re
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple


def run(
    argv: List[str], *, timeout: Optional[float] = None, capture: bool = True
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
    )


def require_cmd(cmd: str) -> None:
    if shutil.which(cmd) is None:
        raise SystemExit(f"live-ui smoke: skipped ({cmd} not found)")


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


def shell_quote(text: str) -> str:
    if platform.system() == "Windows":
        return "'" + text.replace("'", "''") + "'"
    return "'" + text.replace("'", "'\"'\"'") + "'"


class LiveKettle:
    def __init__(self, kettle: str, cfg: Path, log: Path, extra_args: Optional[List[str]] = None):
        self.kettle = kettle
        self.cfg = cfg
        self.log = log
        self.extra_args = extra_args or []
        self.proc: Optional[subprocess.Popen] = None

    def __enter__(self) -> "LiveKettle":
        log_f = self.log.open("wb")
        self.proc = subprocess.Popen(
            [self.kettle, "--config", str(self.cfg), "--agent-server", "full", *self.extra_args],
            stdout=log_f,
            stderr=subprocess.STDOUT,
        )
        log_f.close()
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(
                    "live-ui smoke: kettle exited before control server came up\n"
                    + self.log.read_text(errors="replace")
                )
            probe = self.ctl("list_panes", raw=True, allow_fail=True)
            if probe.returncode == 0:
                return self
            time.sleep(0.1)
        raise SystemExit("live-ui smoke: timed out waiting for control server")

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
        for line in self.proc.stdout:
            line = line.strip()
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

        before = live.json_ctl("ui_geometry")
        (out / "geometry-before-press.json").write_text(json.dumps(before, indent=2) + "\n")
        live.screenshot(out / "before-press.png")
        segments = before["tab_bar"]["segments"]  # type: ignore[index]
        widths = [float(seg["rect"]["width"]) for seg in segments]  # type: ignore[index]
        if len(widths) >= 2 and not all(abs(w - widths[0]) < 1.0 for w in widths[1:]):
            raise SystemExit(
                "tabbar smoke: homogeneous segments not equal: "
                f"widths={[round(w, 1) for w in widths]}"
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


def capture_live_state(live: LiveKettle, out: Path, label: str) -> Dict[str, object]:
    cells = live.read_cells()
    (out / f"{label}.cells.json").write_text(json.dumps(cells, indent=2) + "\n")
    screen = live.json_ctl("read_screen")
    (out / f"{label}.screen.json").write_text(json.dumps(screen, indent=2) + "\n")
    shot = out / f"{label}.png"
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


def live_shell_command(live: LiveKettle, command: str, marker: str, timeout_ms: int = 10000) -> None:
    live.ctl("send_text", params={"text": command})
    live.ctl("send_keys", params={"keys": ["enter"]})
    live.wait_for_text(marker, timeout_ms=timeout_ms, quiet_ms=250)


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


def command_with_marker(command: str, marker: str) -> str:
    split = max(1, len(marker) // 2)
    left = marker[:split]
    right = marker[split:]
    if platform.system() == "Windows":
        return f"{command}; Write-Output ({shell_quote(left)} + {shell_quote(right)})"
    return f"{command}; printf '%s\\n' {shell_quote(left)}{shell_quote(right)}"


def first_lines_command(command: str, lines: int = 22) -> str:
    if platform.system() == "Windows":
        return f"{command} | Select-Object -First {lines}"
    return f"{command} | sed -n '1,{lines}p'"


def prompt_marker_command(marker: str) -> str:
    split = max(1, len(marker) // 2)
    left = marker[:split]
    right = marker[split:]
    if platform.system() == "Windows":
        return (
            "$arrow=[char]0x279c; "
            f"Write-Output ($arrow + '  ~ ' + ({shell_quote(left)} + {shell_quote(right)}))"
        )
    return f"printf '\\342\\236\\234  ~ %s\\n' {shell_quote(left)}{shell_quote(right)}"


def codex_cursor_fixture_command(*, queued_input: bool) -> Tuple[str, int, int]:
    row = 6
    text = "queued work" if queued_input else "Explain this codebase"
    col = 3 + len(text) if queued_input else 3
    if platform.system() == "Windows":
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
    if platform.system() == "Windows":
        return (
            "$esc=[char]27; $bel=[char]7; "
            f"[Console]::Write($esc + ']777;notify;' + {shell_quote(title)} + ';' + {shell_quote(body)} + $bel); "
            f"Write-Output {shell_quote(marker)}"
        )
    return (
        "printf '\\033]777;notify;%s;%s\\007' "
        f"{shell_quote(title)} {shell_quote(body)}; "
        f"printf '%s\\n' {shell_quote(marker)}"
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
    if use_windows:
        return (
            f"Set-Location {shell_quote(expected_path)}; "
            "$esc=[char]27; $bel=[char]7; "
            "[Console]::Write($esc + ']9;9;\"' + $PWD.Path + '\"' + $bel + $esc + ']2;' + "
            f"{shell_quote(title)} + $bel); "
            f"Write-Output {shell_quote(marker)}; "
            f"Start-Sleep -Seconds {sleep_seconds}"
        )
    return (
        f"cd {shell_quote(expected_path)}; "
        f"printf '\\033]7;file://localhost%s\\007\\033]2;{title}\\007"
        f"{marker}\\n' \"$PWD\"; sleep {sleep_seconds}"
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
        ps_argv = " ".join(shell_quote(part) for part in argv)
        done = shell_quote(done_marker)
        return (
            "$tmp=[System.IO.Path]::GetTempFileName(); $rc=125; "
            "try { "
            "$LASTEXITCODE=$null; "
            f"& {ps_argv} *> $tmp; "
            "$rc=if ($null -eq $LASTEXITCODE) { 125 } else { [int]$LASTEXITCODE }; "
            "} catch { "
            "$rc=125; $_ | Out-File -LiteralPath $tmp -Append -Encoding utf8; "
            "}; "
            f"Write-Output ({shell_quote(output_left)} + {shell_quote(output_right)}); "
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


def live_helper_selftest() -> None:
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
    assert f"Write-Output {shell_quote(cwd_marker)}" in win_cwd_command
    assert "Start-Sleep -Seconds 5" in win_cwd_command
    # POSIX: unchanged `cd` + `printf` OSC 7 shape.
    assert "printf" in posix_cwd_command
    assert "file://localhost" in posix_cwd_command
    assert "Set-Location" not in posix_cwd_command
    assert "[Console]::Write" not in posix_cwd_command
    assert f"{cwd_marker}\\n" in posix_cwd_command
    assert "sleep 5" in posix_cwd_command


def nvim_marker_command(marker: str, configured: bool) -> str:
    base = "nvim -n" if configured else "nvim --clean -n"
    return (
        f'{base} "+set termguicolors" '
        f'"+call setline(1, {shell_quote(marker)})" '
        '"+normal! gg"'
    )


def nvim_split_command(left_marker: str, right_marker: str, configured: bool) -> str:
    base = "nvim -n" if configured else "nvim --clean -n"
    return (
        f'{base} "+set termguicolors cursorline laststatus=2" '
        f'"+call setline(1, [{shell_quote(left_marker)}, {shell_quote(left_marker + "_LINE_2")}])" '
        '"+vsplit" '
        '"+wincmd l" '
        '"+enew" '
        f'"+call setline(1, [{shell_quote(right_marker)}, {shell_quote(right_marker + "_LINE_2")}])" '
        '"+wincmd h"'
    )


def exit_nvim(live: LiveKettle) -> None:
    live.ctl(
        "send_keys",
        params={"keys": ["escape", ":", "q", "a", "l", "l", "!", "enter"]},
        timeout=8,
    )


def run_agent_tui(kettle: str, root: Path) -> Path:
    out = root / f"agent-tui-{time.strftime('%Y%m%d-%H%M%S')}"
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
                "cursor-blink = false",
                "window-width = 120",
                "window-height = 36",
            ]
        )
        + "\n"
    )
    extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"] if platform.system() == "Windows" else []
    states: List[Dict[str, object]] = []
    probes: List[Dict[str, object]] = []
    run_auth_smoke = env_flag("KETTLE_AGENT_AUTH_SMOKE")
    require_auth_smoke = env_strict("KETTLE_AGENT_AUTH_SMOKE")
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        marker = "KETTLE_AGENT_TUI_SHELL_SMOKE"
        live_shell_command(live, command_with_marker("printf 'shell-live-ok\\n'" if platform.system() != "Windows" else "Write-Output shell-live-ok", marker), marker)
        states.append(capture_live_state(live, out, "shell"))
        probes.append({"name": "shell", "status": "ok"})

        prompt_marker = "KETTLE_AGENT_TUI_PROMPT_SHAPE"
        live_shell_command(live, prompt_marker_command(prompt_marker), prompt_marker)
        prompt_screen = live.json_ctl("read_screen")
        prompt_text = screen_text(prompt_screen)
        if platform.system() == "Windows":
            prompt_visible = prompt_marker in prompt_text
        else:
            prompt_visible = f"\u279c  ~ {prompt_marker}" in prompt_text
        if not prompt_visible:
            raise SystemExit("agent-tui smoke: prompt-shaped marker is not visible")
        states.append(capture_live_state(live, out, "prompt-shape"))
        probes.append({"name": "prompt-shape", "status": "ok"})

        if platform.system() == "Windows":
            for queued_input, label in (
                (False, "codex-active-placeholder-cursor"),
                (True, "codex-active-queued-input-cursor"),
            ):
                command, cursor_row, cursor_col = codex_cursor_fixture_command(
                    queued_input=queued_input
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

        for tool in ("codex", "claude"):
            if shutil.which(tool) is None:
                probes.append({"name": tool, "status": "skipped", "reason": "not on PATH"})
                continue
            marker = f"KETTLE_AGENT_TUI_{tool.upper()}_SMOKE"
            live_shell_command(live, command_with_marker(f"{tool} --version", marker), marker, timeout_ms=12000)
            states.append(capture_live_state(live, out, tool))
            probes.append({"name": tool, "status": "ok"})
            if tool == "codex":
                help_label = "codex-exec-help"
                help_marker = "KETTLE_AGENT_TUI_CODEX_EXEC_HELP"
                expected = "Run Codex non-interactively"
                help_command = first_lines_command("codex exec --help")
            else:
                help_label = "claude-print-help"
                help_marker = "KETTLE_AGENT_TUI_CLAUDE_PRINT_HELP"
                expected = "non-interactive output"
                help_command = first_lines_command("claude --print --help")
            live_shell_command(
                live,
                command_with_marker(help_command, help_marker),
                help_marker,
                timeout_ms=12000,
            )
            help_screen = live.json_ctl("read_screen")
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
                            tool, auth_marker, output_marker, done_marker
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

        if platform.system() == "Windows" or shutil.which("tmux") is None:
            probes.append({"name": "tmux", "status": "skipped", "reason": "not on PATH"})
        else:
            tmux_socket = f"kettle-smoke-{live.pid}"
            tmux_marker = "KETTLE_AGENT_TUI_TMUX_SMOKE"
            tmux_left_marker = "KETTLE_AGENT_TUI_TMUX_SPLIT_LEFT"
            tmux_right_marker = "KETTLE_AGENT_TUI_TMUX_SPLIT_RIGHT"
            live.ctl(
                "send_text",
                params={"text": f"tmux -L {tmux_socket} -f /dev/null new-session -A -s kettle_smoke"},
            )
            live.ctl("send_keys", params={"keys": ["enter"]})
            time.sleep(1.0)
            live.ctl("send_text", params={"text": f"printf '%s\\n' {shell_quote(tmux_marker)}"})
            live.ctl("send_keys", params={"keys": ["enter"]})
            live.wait_for_text(tmux_marker, timeout_ms=12000, quiet_ms=500)
            states.append(capture_live_state(live, out, "tmux"))
            tmux_cmds = [
                ["tmux", "-L", tmux_socket, "split-window", "-h", "-t", "kettle_smoke:0.0"],
                ["tmux", "-L", tmux_socket, "send-keys", "-t", "kettle_smoke:0.0", f"printf '%s\\n' {shell_quote(tmux_left_marker)}", "C-m"],
                ["tmux", "-L", tmux_socket, "send-keys", "-t", "kettle_smoke:0.1", f"printf '%s\\n' {shell_quote(tmux_right_marker)}", "C-m"],
            ]
            for cmd in tmux_cmds:
                cp = run(cmd, timeout=5, capture=True)
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
            run(["tmux", "-L", tmux_socket, "kill-server"], timeout=3, capture=True)
            tmux_exit_marker = "KETTLE_AGENT_TUI_TMUX_EXITED"
            time.sleep(0.5)
            live_shell_command(
                live,
                command_with_marker("printf 'tmux-exited\\n'", tmux_exit_marker),
                tmux_exit_marker,
                timeout_ms=12000,
            )
            probes.append({"name": "tmux", "status": "ok"})
            probes.append({"name": "tmux-split", "status": "ok"})

        if shutil.which("nvim") is None:
            probes.append({"name": "nvim-clean", "status": "skipped", "reason": "not on PATH"})
            probes.append({"name": "nvim-configured", "status": "skipped", "reason": "not on PATH"})
            probes.append({"name": "nvim-split-clean", "status": "skipped", "reason": "not on PATH"})
            probes.append({"name": "nvim-split-configured", "status": "skipped", "reason": "not on PATH"})
        else:
            for label, configured in (("nvim-clean", False), ("nvim-configured", True)):
                marker = f"KETTLE_AGENT_TUI_{label.replace('-', '_').upper()}_SMOKE"
                live.ctl("send_text", params={"text": nvim_marker_command(marker, configured)})
                live.ctl("send_keys", params={"keys": ["enter"]})
                live.wait_for_text(marker, timeout_ms=18000, quiet_ms=500)
                states.append(capture_live_state(live, out, label))
                exit_nvim(live)
                time.sleep(0.6)
                shell_marker = f"{marker}_EXITED"
                live_shell_command(live, command_with_marker("printf 'nvim-exited\\n'" if platform.system() != "Windows" else "Write-Output nvim-exited", shell_marker), shell_marker)
                probes.append({"name": label, "status": "ok"})
            for label, configured in (
                ("nvim-split-clean", False),
                ("nvim-split-configured", True),
            ):
                base = label.replace("-", "_").upper()
                left_marker = f"KETTLE_AGENT_TUI_{base}_LEFT"
                right_marker = f"KETTLE_AGENT_TUI_{base}_RIGHT"
                live.ctl(
                    "send_text",
                    params={"text": nvim_split_command(left_marker, right_marker, configured)},
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                live.wait_for_text(left_marker, timeout_ms=30000, quiet_ms=500)
                live.wait_for_text(right_marker, timeout_ms=30000, quiet_ms=500)
                split_screen = live.json_ctl("read_screen")
                split_text = screen_text(split_screen)
                if left_marker not in split_text or right_marker not in split_text:
                    raise SystemExit(f"agent-tui smoke: {label} split markers are not both visible")
                states.append(capture_live_state(live, out, label))
                exit_nvim(live)
                time.sleep(0.6)
                shell_marker = f"KETTLE_AGENT_TUI_{base}_EXITED"
                live_shell_command(
                    live,
                    command_with_marker(
                        "printf 'nvim-split-exited\\n'"
                        if platform.system() != "Windows"
                        else "Write-Output nvim-split-exited",
                        shell_marker,
                    ),
                    shell_marker,
                )
                probes.append({"name": label, "status": "ok"})

    ok = [p for p in probes if p.get("status") == "ok"]
    if not ok:
        raise SystemExit("agent-tui smoke: no probes ran")
    (out / "analysis.json").write_text(
        json.dumps({"probes": probes, "states": states}, indent=2) + "\n"
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
            paste_text = "Write-Output PASTE_LINE_ONE; Write-Output PASTE_LINE_TWO; Write-Output " + shell_quote(paste_marker)
        else:
            paste_text = "printf '%s\\n' PASTE_LINE_ONE PASTE_LINE_TWO " + shell_quote(paste_marker)
        live.ctl("send_text", params={"text": paste_text})
        live.ctl("send_keys", params={"keys": ["enter"]})
        live.wait_for_text(paste_marker, timeout_ms=10000, quiet_ms=250)
        paste_screen = live.json_ctl("read_screen")
        if "PASTE_LINE_ONE" not in screen_text(paste_screen) or "PASTE_LINE_TWO" not in screen_text(paste_screen):
            raise SystemExit("interaction smoke: multiline paste/send_text marker was not visible")
        states.append(capture_live_state(live, out, "paste"))

        scroll_marker = "KETTLE_INTERACTION_SCROLL_100"
        if platform.system() == "Windows":
            scroll_cmd = (
                "$esc=[char]27; [Console]::Write($esc + '[2J' + $esc + '[3J' + $esc + '[H'); "
                "1..140 | ForEach-Object { 'KETTLE_INTERACTION_SCROLL_{0:D3}' -f $_ }; "
                "Write-Output KETTLE_INTERACTION_SCROLL_DONE"
            )
        else:
            scroll_cmd = "printf '\\033[2J\\033[3J\\033[H'; for i in $(seq 1 140); do printf 'KETTLE_INTERACTION_SCROLL_%03d\\n' \"$i\"; done; printf 'KETTLE_INTERACTION_SCROLL_DONE\\n'"
        live_shell_command(live, scroll_cmd, "KETTLE_INTERACTION_SCROLL_DONE", timeout_ms=12000)
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
        last_marker = "KETTLE_INTERACTION_SCROLL_DONE"
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
        live.screenshot(out / "search-open.png")
        if not modal_open(search_geo, "search"):
            raise SystemExit("interaction smoke: perform_action start_search did not open search")
        if modal_open(search_geo, "palette"):
            raise SystemExit("interaction smoke: search action did not close the command palette")
        search_changes = len(changed_pixels(out / "palette-open.png", out / "search-open.png", 0.0, float(palette_geo["surface"]["height"])))  # type: ignore[index]
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
            modal_shot = out / f"{label}.png"
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


def run_split_titlebar(kettle: str, root: Path) -> Path:
    out = root / f"split-titlebar-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    cfg = out / "config"
    cfg.write_text(
        "\n".join(
            [
                "agent-server = full",
                "tab-bar = always",
                "tab-bar-position = top",
                "status-bar = off",
                "show-titlebar = true",
                "title-hide-sizetext = true",
                "restore-session = false",
                "update-check = false",
                "background = #101010",
                "foreground = #f4f4f4",
                "window-width = 240",
                "window-height = 60",
            ]
        )
        + "\n"
    )
    # v2.38.2: `tempfile.gettempdir()` rather than a hardcoded POSIX `/tmp` —
    # the latter is not a valid anchor on Windows (pathlib treats it as
    # drive-relative to whatever the current drive happens to be), which is
    # why this fixture used to be written Windows-out instead of
    # Windows-supported.
    nested = (
        Path(tempfile.gettempdir())
        / f"kettle-split-titlebar-{os.getpid()}"
        / "Repos"
        / "SPI-1"
        / "flight-event-line-server-go"
    )
    nested.mkdir(parents=True, exist_ok=True)
    expected_path = str(nested)
    marker = "KETTLE_SPLIT_TITLEBAR_READY"
    truncated_title = "..PI-1/flight-event-line-server-go"
    command = cwd_title_command(expected_path, truncated_title, marker)
    extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"] if platform.system() == "Windows" else []

    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live.ctl("send_text", params={"text": command + "\n"})
        for _ in range(50):
            initial = live.json_ctl("list_panes")
            pane_rows = initial.get("panes", [])
            focused = [p for p in pane_rows if isinstance(p, dict) and p.get("focused")]
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
                "split-titlebar smoke: pane title/cwd did not settle; "
                f"panes={initial} screen={screen.get('text')!r}"
            )

        live.json_ctl("perform_action", params={"action": "split_right"})
        panes: Dict[str, object] = {}
        geo: Dict[str, object] = {}
        for _ in range(40):
            panes = live.json_ctl("list_panes")
            pane_rows = panes.get("panes", [])
            geo = live.json_ctl("ui_geometry")
            titlebars = geo.get("pane_titlebars", [])
            if (
                isinstance(pane_rows, list)
                and len(pane_rows) >= 2
                and isinstance(titlebars, list)
                and len(titlebars) >= 2
            ):
                break
            time.sleep(0.1)

        live.screenshot(out / "split-titlebar.png")
        (out / "panes.json").write_text(json.dumps(panes, indent=2) + "\n")
        (out / "geometry.json").write_text(json.dumps(geo, indent=2) + "\n")

    pane_rows = panes.get("panes", [])
    if not isinstance(pane_rows, list) or len(pane_rows) < 2:
        raise SystemExit(f"split-titlebar smoke: split did not create two panes: {panes}")
    if not any(isinstance(p, dict) and p.get("title") == truncated_title for p in pane_rows):
        raise SystemExit(f"split-titlebar smoke: raw truncated title was not preserved: {panes}")

    titlebars = geo.get("pane_titlebars", [])
    if not isinstance(titlebars, list) or len(titlebars) < 2:
        raise SystemExit(f"split-titlebar smoke: missing pane titlebar diagnostics: {geo}")
    for titlebar in titlebars:
        if not isinstance(titlebar, dict):
            raise SystemExit(f"split-titlebar smoke: malformed titlebar: {titlebar}")
        if titlebar.get("path") != expected_path:
            raise SystemExit(f"split-titlebar smoke: titlebar path did not track cwd: {titlebar}")
        fitted = titlebar.get("fitted_title")
        if not isinstance(fitted, str) or fitted.strip() != expected_path:
            raise SystemExit(
                "split-titlebar smoke: wide split titlebar did not recover full cwd: "
                f"{titlebar}"
            )
        if fitted.strip().startswith(".."):
            raise SystemExit(f"split-titlebar smoke: rendered truncated shell title: {titlebar}")

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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "case",
        choices=[
            "tabbar",
            "tab-title",
            "split-titlebar",
            "zoom-keybind",
            "underline",
            "agent-tui",
            "search-history",
            "interaction",
            "self-test",
            "all",
        ],
    )
    parser.add_argument("--kettle", default=os.environ.get("KETTLE_BIN", "kettle"))
    parser.add_argument("--out-dir", default=os.environ.get("KETTLE_DIAG_DIR", "target/diagnostics"))
    args = parser.parse_args()

    if args.case == "self-test":
        live_helper_selftest()
        print("live-ui helper self-test: OK")
        return 0

    if platform.system() != "Windows" and not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        print("live-ui smoke: skipped (no DISPLAY or WAYLAND_DISPLAY)", file=sys.stderr)
        return 0

    root = Path(args.out_dir).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if args.case in ("tabbar", "all"):
        out = run_tabbar(args.kettle, root)
        print(f"tabbar-click smoke: OK artifacts={out}")
    if args.case in ("tab-title", "all"):
        out = run_tab_title(args.kettle, root)
        print(f"tab-title smoke: OK artifacts={out}")
    if args.case in ("split-titlebar", "all"):
        out = run_split_titlebar(args.kettle, root)
        print(f"split-titlebar smoke: OK artifacts={out}")
    if args.case in ("zoom-keybind", "all"):
        out = run_zoom_keybind(args.kettle, root)
        print(f"zoom-keybind smoke: OK artifacts={out}")
    if args.case in ("underline", "all"):
        missing = missing_commands("git", "delta", "less")
        if missing:
            print(
                f"underline-scroll smoke: skipped ({', '.join(missing)} not found)",
                file=sys.stderr,
            )
        else:
            out = run_underline(args.kettle, root)
            print(f"underline-scroll smoke: OK artifacts={out}")
    if args.case in ("agent-tui", "all"):
        out = run_agent_tui(args.kettle, root)
        print(f"agent-tui smoke: OK artifacts={out}")
    if args.case in ("search-history", "all"):
        out = run_search_history(args.kettle, root)
        print(f"search-history smoke: OK artifacts={out}")
    if args.case in ("interaction", "all"):
        out = run_interaction(args.kettle, root)
        print(f"interaction smoke: OK artifacts={out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
