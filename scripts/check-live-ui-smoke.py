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
import shutil
import struct
import subprocess
import sys
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
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        timeout=timeout,
        check=False,
    )


def require_cmd(cmd: str) -> None:
    if shutil.which(cmd) is None:
        raise SystemExit(f"live-ui smoke: skipped ({cmd} not found)")


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
        last_cells = live.json_ctl("read_cells")
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
                "tab-bar-pos = top",
                "status-bar = off",
                "restore-session = false",
                "update-check = false",
                "background = #101010",
                "foreground = #f4f4f4",
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
        seg = before["tab_bar"]["segments"][1]["rect"]  # type: ignore[index]
        x, y = rect_center(seg)
        live.ctl("send_mouse", params={"event": "press", "x": x, "y": y, "button": "left"})
        time.sleep(0.1)
        pressed = live.json_ctl("ui_geometry")
        (out / "geometry-pressed.json").write_text(json.dumps(pressed, indent=2) + "\n")
        live.screenshot(out / "pressed.png")
        cx, cy = pressed["cursor"]  # type: ignore[index]
        live.ctl("send_mouse", params={"event": "release", "x": cx, "y": cy, "button": "left"})
        time.sleep(0.1)
        released = live.json_ctl("ui_geometry")
        (out / "geometry-released.json").write_text(json.dumps(released, indent=2) + "\n")
        live.screenshot(out / "released.png")

    if not pressed.get("tab_drag_active") or not pressed.get("tab_drag_armed"):
        raise SystemExit("tabbar smoke: press did not remain click-armed")
    if pressed.get("tab_drag_visible"):
        raise SystemExit("tabbar smoke: drag ghost became visible during a plain click")
    if released.get("tab_drag_active") or released.get("tab_drag_armed") or released.get("tab_drag_visible"):
        raise SystemExit("tabbar smoke: release left drag state latched")

    before_rect, before_idx = active_rect(before)
    pressed_rect, pressed_idx = active_rect(pressed)
    bar = pressed["tab_bar"]  # type: ignore[index]
    y0 = float(bar["y"])
    y1 = y0 + float(bar["height"])
    changed = changed_pixels(out / "before-press.png", out / "pressed.png", y0, y1)
    outside = [
        (x, y)
        for x, y in changed
        if not (rect_contains(before_rect, x, y) or rect_contains(pressed_rect, x, y))
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
        "pressed_active_rect": pressed_rect,
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
    subprocess.run(["git", "add", "fixture.txt"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "base"], cwd=repo, check=True)
    changed = []
    for i in range(1, 181):
        if i % 3 == 0:
            changed.append(f"changed underlined token_{i:03d} and link https://example.invalid/{i:03d}\n")
        else:
            changed.append(f"stable line {i:03d}\n")
    (repo / "fixture.txt").write_text("".join(changed))


def underline_command(repo: Path) -> str:
    if platform.system() == "Windows":
        repo_s = str(repo).replace("'", "''")
        return (
            f"Set-Location -LiteralPath '{repo_s}'; "
            "$esc=[char]27; "
            "& { 1..120 | ForEach-Object { "
            "if ($_ % 2 -eq 1) { '{0}[4mUNDERLINE_{2}_{1:D3}{0}[24m link https://example.invalid/{1:D3}' -f $esc,$_,'SENTINEL' } "
            "else { 'PLAIN_{1}_{0:D3} link https://example.invalid/{0:D3}' -f $_,'SENTINEL' } }; "
            "git diff --color=always | delta --paging=never --line-numbers } | less -R"
        )
    repo_s = str(repo).replace("'", "'\"'\"'")
    return (
        f"cd '{repo_s}' && {{ for i in $(seq 1 120); do "
        "if [ $((i % 2)) -eq 1 ]; then "
        "printf '\\033[4mUNDERLINE_%s_%03d\\033[24m link https://example.invalid/%03d\\n' SENTINEL \"$i\" \"$i\"; "
        "else printf 'PLAIN_%s_%03d link https://example.invalid/%03d\\n' SENTINEL \"$i\" \"$i\"; fi; "
        "done; git diff --color=always | delta --paging=never --line-numbers; } | less -R"
    )


def run_underline(kettle: str, root: Path) -> Path:
    out = root / f"underline-scroll-{time.strftime('%Y%m%d-%H%M%S')}"
    repo = out / "repo"
    make_git_fixture(repo)
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
    extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"] if platform.system() == "Windows" else []
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        live.ctl("send_text", params={"text": underline_command(repo)})
        live.ctl("send_keys", params={"keys": ["enter"]})
        live.ctl("wait_for", params={"text": "UNDERLINE_SENTINEL", "timeout_ms": 8000, "quiet_ms": 250})
        for i in range(1, 9):
            cells = live.json_ctl("read_cells")
            (out / f"cells-{i}.json").write_text(json.dumps(cells))
            live.screenshot(out / f"frame-{i}.png")
            if i < 8:
                keys = ["j"] * 6 if i < 5 else ["k"] * 6
                live.ctl("send_keys", params={"keys": keys}, timeout=6)
            time.sleep(0.08)

    top_sentinels: List[int] = []
    underline_frames = 0
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
        for row, row_cells in sorted(rows.items()):
            text = "".join(ch for _, ch in sorted(row_cells))
            if "UNDERLINE_SENTINEL_" in text:
                num = int(text.split("UNDERLINE_SENTINEL_", 1)[1][:3])
                found.append((row, num))
            if "PLAIN_SENTINEL_" in text:
                num = int(text.split("PLAIN_SENTINEL_", 1)[1][:3])
                plain_found.append((row, num))
        if not found:
            raise SystemExit(f"underline smoke: no sentinel text visible in cells-{i}.json")
        top_sentinels.append(found[0][1])
        width, height, rgba_rows = read_rgba_png(out / f"frame-{i}.png")
        cell_w = width / cols
        cell_h = height / rows_n
        pixel_rows = []
        for row, number in found[:8]:
            sample_cols = sorted(underline_cols.get(row, []))[:22]
            if not sample_cols:
                raise SystemExit(f"underline smoke: row {row} has no underline attrs")
            baseline = int(round((row + 1) * cell_h - 2.0))
            best = 0
            best_y = baseline
            for y in range(baseline - 2, baseline + 3):
                hits = sum(1 for col in sample_cols if bright_at(rgba_rows, int((col + 0.5) * cell_w), y))
                if hits > best:
                    best = hits
                    best_y = y
            if best < max(8, int(len(sample_cols) * 0.60)):
                raise SystemExit(f"underline smoke: rendered underline not aligned on frame {i} row {row}")
            pixel_rows.append({"row": row, "sentinel": number, "underline_pixel_hits": best, "sampled_columns": len(sample_cols), "pixel_y": best_y})
        plain_pixel_rows = []
        for row, number in plain_found[:8]:
            sample_cols = list(range(0, 18))
            baseline = int(round((row + 1) * cell_h - 2.0))
            best = 0
            best_y = baseline
            for y in range(baseline - 2, baseline + 3):
                hits = sum(1 for col in sample_cols if bright_at(rgba_rows, int((col + 0.5) * cell_w), y))
                if hits > best:
                    best = hits
                    best_y = y
            if best > max(6, int(len(sample_cols) * 0.45)):
                raise SystemExit(f"underline smoke: underline leaked onto plain row on frame {i} row {row}")
            plain_pixel_rows.append({"row": row, "sentinel": number, "baseline_pixel_hits": best, "sampled_columns": len(sample_cols), "pixel_y": best_y})
        analysis.append({"frame": i, "top_sentinel": found[0][1], "underline_rows": sorted(underline_rows), "sentinels": [{"row": r, "number": n} for r, n in found], "plain_sentinels": [{"row": r, "number": n} for r, n in plain_found], "pixel_rows": pixel_rows, "plain_pixel_rows": plain_pixel_rows})
    if underline_frames == 0:
        raise SystemExit("underline smoke: no underlined cells observed")
    if not (top_sentinels[0] < top_sentinels[4] and top_sentinels[-1] < top_sentinels[4]):
        raise SystemExit(f"underline smoke: down/up scroll sequence failed: {top_sentinels}")
    (out / "analysis.json").write_text(json.dumps({"frames": 8, "underline_frames": underline_frames, "top_sentinels": top_sentinels, "frames_detail": analysis}, indent=2) + "\n")
    return out


def capture_live_state(live: LiveKettle, out: Path, label: str) -> Dict[str, object]:
    cells = live.json_ctl("read_cells")
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


def command_with_marker(command: str, marker: str) -> str:
    if platform.system() == "Windows":
        return f"{command}; Write-Output {shell_quote(marker)}"
    return f"{command}; printf '%s\\n' {shell_quote(marker)}"


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


def nvim_marker_command(marker: str, configured: bool) -> str:
    base = "nvim -n" if configured else "nvim --clean -n"
    return (
        f'{base} "+set termguicolors" '
        f'"+call setline(1, {shell_quote(marker)})" '
        '"+normal! gg"'
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
                "window-width = 120",
                "window-height = 36",
            ]
        )
        + "\n"
    )
    extra_args = ["-e", "powershell.exe", "-NoLogo", "-NoProfile"] if platform.system() == "Windows" else []
    states: List[Dict[str, object]] = []
    probes: List[Dict[str, object]] = []
    with LiveKettle(kettle, cfg, out / "kettle.log", extra_args=extra_args) as live:
        marker = "KETTLE_AGENT_TUI_SHELL_SMOKE"
        live_shell_command(live, command_with_marker("printf 'shell-live-ok\\n'" if platform.system() != "Windows" else "Write-Output shell-live-ok", marker), marker)
        states.append(capture_live_state(live, out, "shell"))
        probes.append({"name": "shell", "status": "ok"})

        prompt_marker = "KETTLE_AGENT_TUI_PROMPT_SHAPE"
        live_shell_command(live, prompt_marker_command(prompt_marker), prompt_marker)
        prompt_screen = live.json_ctl("read_screen")
        if f"\u279c  ~ {prompt_marker}" not in screen_text(prompt_screen):
            raise SystemExit("agent-tui smoke: prompt-shaped marker is not visible")
        states.append(capture_live_state(live, out, "prompt-shape"))
        probes.append({"name": "prompt-shape", "status": "ok"})

        for tool in ("codex", "claude"):
            if shutil.which(tool) is None:
                probes.append({"name": tool, "status": "skipped", "reason": "not on PATH"})
                continue
            marker = f"KETTLE_AGENT_TUI_{tool.upper()}_SMOKE"
            live_shell_command(live, command_with_marker(f"{tool} --version", marker), marker, timeout_ms=12000)
            states.append(capture_live_state(live, out, tool))
            probes.append({"name": tool, "status": "ok"})

        if platform.system() == "Windows" or shutil.which("tmux") is None:
            probes.append({"name": "tmux", "status": "skipped", "reason": "not on PATH"})
        else:
            tmux_socket = f"kettle-smoke-{live.pid}"
            tmux_marker = "KETTLE_AGENT_TUI_TMUX_SMOKE"
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
            live.ctl("send_text", params={"text": "exit"})
            live.ctl("send_keys", params={"keys": ["enter"]})
            run(["tmux", "-L", tmux_socket, "kill-server"], timeout=3, capture=True)
            probes.append({"name": "tmux", "status": "ok"})

        if shutil.which("nvim") is None:
            probes.append({"name": "nvim-clean", "status": "skipped", "reason": "not on PATH"})
            probes.append({"name": "nvim-configured", "status": "skipped", "reason": "not on PATH"})
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

    ok = [p for p in probes if p.get("status") == "ok"]
    if not ok:
        raise SystemExit("agent-tui smoke: no probes ran")
    (out / "analysis.json").write_text(
        json.dumps({"probes": probes, "states": states}, indent=2) + "\n"
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
                "1..140 | ForEach-Object { 'KETTLE_INTERACTION_SCROLL_{0:D3}' -f $_ }; "
                "Write-Output KETTLE_INTERACTION_SCROLL_DONE"
            )
        else:
            scroll_cmd = "for i in $(seq 1 140); do printf 'KETTLE_INTERACTION_SCROLL_%03d\\n' \"$i\"; done; printf 'KETTLE_INTERACTION_SCROLL_DONE\\n'"
        live_shell_command(live, scroll_cmd, "KETTLE_INTERACTION_SCROLL_DONE", timeout_ms=12000)
        live.screenshot(out / "scroll-bottom.png")
        bottom = live.json_ctl("read_screen")
        (out / "scroll-bottom.screen.json").write_text(json.dumps(bottom, indent=2) + "\n")
        if int(bottom.get("display_offset", 0)) != 0:
            raise SystemExit(f"interaction smoke: expected bottom display_offset 0, got {bottom.get('display_offset')}")
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
        selection_cells = live.json_ctl("read_cells")
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
        split_rows = [
            row for row in menu.get("rows", [])  # type: ignore[union-attr]
            if row.get("label") == "Split Right" and row.get("dispatchable")
        ]
        if len(split_rows) != 1:
            raise SystemExit(f"interaction smoke: expected one Split Right row, got {split_rows}")
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
        before_resize_cells = live.json_ctl("read_cells")
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

    (out / "analysis.json").write_text(
        json.dumps(
            {
                "states": states,
                "menu_changed_pixels": menu_changes,
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
                "menu_split_right_rect": split_rows[0]["rect"],
                "notification_event": notification_event,
            },
            indent=2,
        )
        + "\n"
    )
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("case", choices=["tabbar", "underline", "agent-tui", "interaction", "all"])
    parser.add_argument("--kettle", default=os.environ.get("KETTLE_BIN", "kettle"))
    parser.add_argument("--out-dir", default=os.environ.get("KETTLE_DIAG_DIR", "target/diagnostics"))
    args = parser.parse_args()

    if platform.system() != "Windows" and not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        print("live-ui smoke: skipped (no DISPLAY or WAYLAND_DISPLAY)", file=sys.stderr)
        return 0

    root = Path(args.out_dir).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if args.case in ("tabbar", "all"):
        out = run_tabbar(args.kettle, root)
        print(f"tabbar-click smoke: OK artifacts={out}")
    if args.case in ("underline", "all"):
        out = run_underline(args.kettle, root)
        print(f"underline-scroll smoke: OK artifacts={out}")
    if args.case in ("agent-tui", "all"):
        out = run_agent_tui(args.kettle, root)
        print(f"agent-tui smoke: OK artifacts={out}")
    if args.case in ("interaction", "all"):
        out = run_interaction(args.kettle, root)
        print(f"interaction smoke: OK artifacts={out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
