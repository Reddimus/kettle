#!/usr/bin/env python3
"""Cross-platform live UI diagnostics for Kettle.

The shell scripts remain the Unix-friendly entrypoints. This script exists so
Windows `just` recipes can run the same live tab/underline checks without Bash.
It intentionally uses only Python stdlib plus `kettle ctl`.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import platform
import queue
import re
import secrets
import shlex
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Dict, List, Optional, Set, Tuple


NVIM_SNAPSHOT_MAX_ENTRIES = 100_000
NVIM_SNAPSHOT_MAX_BYTES = 2 * 1024 * 1024 * 1024
NVIM_SNAPSHOT_MAX_FILE_BYTES = 256 * 1024 * 1024
NVIM_SNAPSHOT_MAX_DEPTH = 64
NVIM_SNAPSHOT_TAR_OVERHEAD_BYTES = (
    NVIM_SNAPSHOT_MAX_ENTRIES * 1024 + 1024 * 1024
)
COPY_CHUNK_BYTES = 1024 * 1024
SPLIT_TITLEBAR_COLOR_HEX = {
    "transmit": "#1a7f37",
    "receive": "#0969da",
    "inactive": "#6e7781",
    "grid": "#101010",
}


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


def path_is_link(path: Path) -> bool:
    """Recognize symlinks and Windows junctions without following them."""
    is_junction = getattr(path, "is_junction", None)
    return path.is_symlink() or (
        callable(is_junction) and bool(is_junction())
    )


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
            "-printf '%y\\0%s\\0%p\\0' "
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
            'export XDG_RUNTIME_DIR="$KETTLE_SMOKE_ROOT/run"; '
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

    def nvim_sandbox_cleanup_command(self, marker: str) -> str:
        marker_left, marker_right = split_marker(marker)
        if self.powershell:
            return (
                "if ($KettleSmokeRoot -and "
                "(Test-Path -LiteralPath $KettleSmokeRoot -PathType Container)) { "
                "Remove-Item -LiteralPath $KettleSmokeRoot -Recurse -Force; }; "
                "Write-Output ("
                f"{shell_quote(marker_left, windows=True)} + "
                f"{shell_quote(marker_right, windows=True)})"
            )
        return (
            'if [ -n "${KETTLE_SMOKE_ROOT:-}" ] && '
            '[ -d "$KETTLE_SMOKE_ROOT" ]; then rm -rf -- "$KETTLE_SMOKE_ROOT"; fi; '
            "printf '%s%s\\n' "
            f"{shlex.quote(marker_left)} {shlex.quote(marker_right)}"
        )

    @staticmethod
    def validate_wsl_sandbox_path(sandbox_path: str) -> None:
        if not re.fullmatch(
            r"/tmp/kettle-agent-tui-[A-Za-z0-9_.-]+", sandbox_path
        ):
            raise ValueError(f"refusing unsafe WSL sandbox path: {sandbox_path}")

    def terminate_nvim_sandbox_host(self, sandbox_path: str) -> None:
        """Terminate only Neovim processes using this WSL smoke sandbox."""
        if self.mode != "wsl":
            raise ValueError("targeted host-side Neovim termination requires WSL")
        self.validate_wsl_sandbox_path(sandbox_path)
        cp = self.run_command(
            [
                "bash",
                "--noprofile",
                "--norc",
                "-c",
                (
                    'pids=""; '
                    'for envfile in /proc/[0-9]*/environ; do '
                    '[ -r "$envfile" ] || continue; '
                    'pid=${envfile#/proc/}; pid=${pid%/environ}; '
                    '[ "$(cat "/proc/$pid/comm" 2>/dev/null)" = nvim ] '
                    '|| continue; '
                    'if tr "\\0" "\\n" <"$envfile" 2>/dev/null '
                    '| grep -Fqx "XDG_CONFIG_HOME=$1/config"; then '
                    'pids="$pids $pid"; fi; done; '
                    '[ -z "$pids" ] || kill -TERM $pids 2>/dev/null || true; '
                    'sleep 1; '
                    '[ -z "$pids" ] || kill -KILL $pids 2>/dev/null || true'
                ),
                "kettle-nvim-stop",
                sandbox_path,
            ],
            timeout=15,
        )
        if cp.returncode != 0:
            raise RuntimeError(
                f"failed to stop WSL Neovim in {sandbox_path}: {cp.stderr}"
            )

    def cleanup_nvim_sandbox_host(self, sandbox_path: str) -> None:
        """Best-effort cleanup after Kettle exits, including failed smokes."""
        if self.mode == "wsl":
            self.validate_wsl_sandbox_path(sandbox_path)
            cp = self.run_command(
                [
                    "bash",
                    "--noprofile",
                    "--norc",
                    "-c",
                    (
                        'pids=""; '
                        'for envfile in /proc/[0-9]*/environ; do '
                        '[ -r "$envfile" ] || continue; '
                        'if tr "\\0" "\\n" <"$envfile" 2>/dev/null '
                        '| grep -Fqx "XDG_CONFIG_HOME=$1/config"; then '
                        'pid=${envfile#/proc/}; pid=${pid%/environ}; '
                        'pids="$pids $pid"; fi; done; '
                        'if [ -n "$pids" ]; then kill -TERM $pids 2>/dev/null || true; '
                        'sleep 1; kill -KILL $pids 2>/dev/null || true; fi; '
                        'for attempt in 1 2 3 4 5; do '
                        'rm -rf -- "$1" 2>/dev/null || true; '
                        '[ ! -e "$1" ] && exit 0; sleep 1; done; exit 1'
                    ),
                    "kettle-cleanup",
                    sandbox_path,
                ],
                timeout=120,
            )
            if cp.returncode != 0:
                raise RuntimeError(
                    f"failed to remove WSL Neovim sandbox {sandbox_path}: {cp.stderr}"
                )
            return

        root = self.validate_native_sandbox_path(sandbox_path)
        if not root.exists():
            return

        def clear_readonly_and_retry(
            operation: Callable[[str], None],
            name: str,
            _error: Tuple[type, BaseException, object],
        ) -> None:
            os.chmod(name, stat.S_IWRITE | stat.S_IREAD)
            operation(name)

        last_error: Optional[OSError] = None
        for _attempt in range(5):
            root = self.validate_native_sandbox_path(sandbox_path)
            if not root.exists():
                return
            try:
                shutil.rmtree(root, onerror=clear_readonly_and_retry)
                return
            except OSError as error:
                last_error = error
                time.sleep(0.2)
        raise RuntimeError(
            f"failed to remove native Neovim sandbox {root}: {last_error}"
        )


class LiveKettle:
    def __init__(self, kettle: str, cfg: Path, log: Path, extra_args: Optional[List[str]] = None):
        self.kettle = kettle
        self.cfg = cfg
        self.log = log
        self.extra_args = extra_args or []
        self.proc: Optional[subprocess.Popen] = None
        self._post_exit_cleanup: List[Callable[[], None]] = []

    def __enter__(self) -> "LiveKettle":
        # Machine-local escape hatch. Every scenario writes its own minimal
        # config, which means it inherits none of the developer's real settings
        # — including a pinned `gpu-device-id`/`gpu-vendor-id`. On a dual-GPU
        # laptop that silently drops the harness onto the integrated GPU, where
        # a driver fault can abort the process before the control server ever
        # comes up (an 0xC0000005 with an empty log). Appending extra config
        # here lets such a machine run the live smokes without hardcoding one
        # developer's hardware into the repo. Unset in CI, so it is a no-op.
        extra_cfg = os.environ.get("KETTLE_SMOKE_EXTRA_CONFIG", "").strip()
        if extra_cfg:
            with self.cfg.open("a", encoding="utf-8") as fh:
                fh.write("\n" + extra_cfg.replace("\\n", "\n").strip() + "\n")
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

    def __exit__(
        self, exc_type: object, _exc_value: object, _traceback: object
    ) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        cleanup_errors: List[Exception] = []
        for cleanup in reversed(self._post_exit_cleanup):
            try:
                cleanup()
            except Exception as error:
                cleanup_errors.append(error)
                print(
                    f"live-ui smoke: post-exit cleanup failed: {error}",
                    file=sys.stderr,
                )
        if cleanup_errors and exc_type is None:
            raise RuntimeError(
                "live-ui smoke: post-exit cleanup failed: "
                + "; ".join(str(error) for error in cleanup_errors)
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


def capture_live_state(live: LiveKettle, out: Path, label: str) -> Dict[str, object]:
    cells = live.read_cells()
    (out / f"{label}.cells.json").write_text(json.dumps(cells, indent=2) + "\n")
    screen = live.json_ctl("read_screen")
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


def live_helper_selftest() -> None:
    artifact_root = Path("artifacts")
    state_shot = live_state_screenshot_path(artifact_root, "search-open")
    transition_shot = live_transition_screenshot_path(artifact_root, "search-open")
    assert state_shot == artifact_root / "search-open.png"
    assert transition_shot == artifact_root / "search-open-transition.png"
    assert state_shot != transition_shot

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
    assert (
        f"Write-Output {shell_quote(cwd_marker, windows=True)}"
        in win_cwd_command
    )
    assert "Start-Sleep -Seconds 5" in win_cwd_command
    # POSIX: unchanged `cd` + `printf` OSC 7 shape.
    assert "printf" in posix_cwd_command
    assert "file://localhost" in posix_cwd_command
    assert "Set-Location" not in posix_cwd_command
    assert "[Console]::Write" not in posix_cwd_command
    assert f"{cwd_marker}\\n" in posix_cwd_command
    assert "sleep 5" in posix_cwd_command

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
    cleanup_command = wsl_target.nvim_sandbox_cleanup_command(cleanup_marker)
    assert "rm -rf --" in cleanup_command
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
        snapshot_target = AgentShellTarget(
            mode="native",
            astro_config=str(config_source),
            nvim_data=str(data_source),
        )
        native_sandbox_path = snapshot_target.create_nvim_sandbox_host()
        try:
            snapshot_target.prepare_nvim_sandbox_host(native_sandbox_path)
            native_root = Path(native_sandbox_path)
            assert native_root.name.startswith("kettle-agent-tui-")
            assert (native_root / "config" / "nvim" / "init.lua").is_file()
            assert (
                native_root
                / "data"
                / "nvim"
                / "lazy"
                / "fixture"
                / "plugin.lua"
            ).is_file()
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
            if platform.system() == "Windows":
                readonly_cleanup_fixture.chmod(stat.S_IREAD)
        finally:
            snapshot_target.cleanup_nvim_sandbox_host(native_sandbox_path)
        assert not Path(native_sandbox_path).exists()

    # Limits are checked while traversing and before any file body can grow
    # the snapshot without bound.
    with tempfile.TemporaryDirectory(
        prefix="kettle-nvim-limit-fixture-"
    ) as fixture:
        fixture_root = Path(fixture)
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

        if hasattr(os, "mkfifo"):
            special_source = fixture_root / "special-source"
            special_source.mkdir()
            os.mkfifo(special_source / "fifo")
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
    assert nvim_marker not in marker_command
    left_marker = "KETTLE_ASTRO_LEFT_RUNTIME"
    right_marker = "KETTLE_ASTRO_RIGHT_RUNTIME"
    split_command = nvim_split_command(
        left_marker, right_marker, True, windows=False
    )
    assert "nvim -n" in split_command
    assert left_marker not in split_command
    assert right_marker not in split_command

    # LazyVCS leg. These are pure string builders, so the shape they produce is
    # checkable on every platform even though the live probe needs a window.
    lazyvcs_marker = "KETTLE_LAZYVCS_RUNTIME"
    lazyvcs_repo = "/tmp/kettle-smoke-lazyvcs"
    setup_posix = lazyvcs_repo_setup_command(lazyvcs_repo, windows=False)
    assert "git init -q ." in setup_posix
    # An unstaged edit is the whole point: without it the sidebar renders no
    # changed files and no gutter signs, and the probe would assert nothing.
    assert "git commit -q -m base" in setup_posix
    assert setup_posix.count("tracked.txt") >= 3
    setup_windows = lazyvcs_repo_setup_command(lazyvcs_repo, windows=True)
    assert "Set-Content" in setup_windows and "git init -q ." in setup_windows

    sidebar_posix = lazyvcs_sidebar_command(
        lazyvcs_repo, lazyvcs_marker, windows=False
    )
    # `nvim -n`, not `--clean`: the configured runtime is what has LazyVCS.
    assert "nvim -n" in sidebar_posix and "--clean" not in sidebar_posix
    assert "+LazyVCS sidebar open" in sidebar_posix
    # The marker must reach the buffer as an expression, never as a literal, or
    # `wait_for_text` matches the command echo instead of the rendered buffer.
    assert lazyvcs_marker not in sidebar_posix
    # Discovery is asynchronous; without this wait the probe races the render.
    assert "lazyvcs_discovering" in sidebar_posix
    assert "vim.wait(30000" in sidebar_posix

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


def nvim_marker_command(
    marker: str, configured: bool, *, windows: Optional[bool] = None
) -> str:
    base = "nvim -n" if configured else "nvim --clean -n"
    marker_expression = nvim_string_expression(marker, windows=windows)
    return (
        f'{base} "+set termguicolors" '
        f'"+call setline(1, {marker_expression})" '
        '"+normal! gg"'
    )


def lazyvcs_repo_setup_command(repo: str, *, windows: Optional[bool] = None) -> str:
    """Shell command creating a one-file Git repository with an unstaged edit.

    The edit is what makes the probe meaningful: LazyVCS only draws gutter
    signs and a populated sidebar when something has actually changed.
    """
    quoted = shell_quote(repo, windows=windows)
    if windows:
        return (
            f"New-Item -ItemType Directory -Force {quoted} | Out-Null; "
            f"Push-Location {quoted}; "
            "git init -q .; "
            "git config user.name kettle-smoke; "
            "git config user.email kettle-smoke@example.invalid; "
            "Set-Content -Path tracked.txt -Value 'first','second','third'; "
            "git add tracked.txt; git commit -q -m base; "
            "Set-Content -Path tracked.txt -Value 'first','CHANGED','third'; "
            "Pop-Location"
        )
    return (
        f"mkdir -p {quoted} && cd {quoted} && "
        "git init -q . && "
        "git config user.name kettle-smoke && "
        "git config user.email kettle-smoke@example.invalid && "
        "printf 'first\\nsecond\\nthird\\n' > tracked.txt && "
        "git add tracked.txt && git commit -q -m base && "
        "printf 'first\\nCHANGED\\nthird\\n' > tracked.txt"
    )


def lazyvcs_sidebar_command(
    repo: str, marker: str, *, windows: Optional[bool] = None
) -> str:
    """Open a file with LazyVCS loaded, show the sidebar, and print a marker.

    Exercises the parts of LazyVCS that depend on the terminal rather than on
    Neovim: the sidebar's Nerd Font icons, the box-drawing gutter sign glyphs
    (default add/change is U+2503), and inline blame virtual text. Kettle
    bundles JetBrains Mono Nerd Font, so a missing glyph here is a kettle
    rendering defect rather than a font-installation problem on the runner.

    Discovery is asynchronous, so the sidebar's first frame reads
    "Discovering repositories..." -- wait for it to settle before printing the
    marker, or the probe races the very rendering it means to check.
    """
    marker_expression = nvim_string_expression(marker, windows=windows)
    tracked = shell_quote(os.path.join(repo, "tracked.txt"), windows=windows)
    wait = (
        "+lua local s = require('lazyvcs.source_control.native')._state(); "
        "if s then vim.wait(30000, function() "
        "return s.lazyvcs_discovering ~= true and s.lazyvcs_repo_specs ~= nil "
        "end, 25) end"
    )
    return (
        f'nvim -n {tracked} "+set termguicolors" '
        '"+LazyVCS sidebar open" '
        f'"{wait}" '
        f'"+call setline(1, {marker_expression})" '
        '"+normal! gg"'
    )


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
        f'{base} "+set termguicolors cursorline laststatus=2" '
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
            sandbox_path = shell_target.create_nvim_sandbox_host()
            live.add_post_exit_cleanup(
                lambda path=sandbox_path: shell_target.cleanup_nvim_sandbox_host(
                    path
                )
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
                lazyvcs_repo = str(Path(sandbox_path) / "lazyvcs-smoke-repo")
                live.ctl(
                    "send_text",
                    params={
                        "text": lazyvcs_repo_setup_command(
                            lazyvcs_repo, windows=shell_target.powershell
                        )
                    },
                )
                live.ctl("send_keys", params={"keys": ["enter"]})
                setup_marker = "KETTLE_LAZYVCS_REPO_READY"
                ready_probe = (
                    f"Write-Output {setup_marker}"
                    if shell_target.powershell
                    else f"printf '{setup_marker}\\n'"
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
                # Same budget as `nvim-configured`: a copied AstroNvim tree may
                # bootstrap its plugins into the disposable XDG data dir first.
                live.wait_for_text(marker, timeout_ms=120000, quiet_ms=500)
                states.append(
                    capture_live_state(live, out, "nvim-lazyvcs-sidebar")
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
            cleanup_marker = "KETTLE_AGENT_TUI_NVIM_SANDBOX_CLEAN"
            live_shell_command(
                live,
                shell_target.nvim_sandbox_cleanup_command(cleanup_marker),
                cleanup_marker,
                timeout_ms=120000,
            )

    ok = [p for p in probes if p.get("status") == "ok"]
    if not ok:
        raise SystemExit("agent-tui smoke: no probes ran")
    (out / "analysis.json").write_text(
        json.dumps(
            {
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
            fill_cmd = (
                "$esc=[char]27; [Console]::Write($esc + '[2J' + $esc + '[3J' + $esc + '[H'); "
                "1..140 | ForEach-Object { 'KETTLE_TOUCHPAD_SCROLL_{0:D3}' -f $_ }; "
                f"Write-Output {marker}"
            )
        else:
            fill_cmd = (
                "printf '\\033[2J\\033[3J\\033[H'; "
                "for i in $(seq 1 140); do printf 'KETTLE_TOUCHPAD_SCROLL_%03d\\n' \"$i\"; done; "
                f"printf '{marker}\\n'"
            )
        live_shell_command(live, fill_cmd, marker, timeout_ms=12000)
        bottom = live.json_ctl("read_screen")
        (out / "bottom.screen.json").write_text(json.dumps(bottom, indent=2) + "\n")
        if int(bottom.get("display_offset", 0)) != 0:
            raise SystemExit(
                f"touchpad smoke: expected bottom display_offset 0, got {bottom.get('display_offset')}"
            )
        live.screenshot(out / "bottom.png")

        # Scroll back with sub-detent events only. No single one of these can
        # move the viewport on its own; only the accumulated residue can.
        for _ in range(events):
            live.ctl("send_mouse", params={"event": "wheel", "wheel_delta": step})
        time.sleep(0.2)
        scrolled = live.json_ctl("read_screen")
        (out / "scrolled.screen.json").write_text(json.dumps(scrolled, indent=2) + "\n")
        live.screenshot(out / "scrolled.png")
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "case",
        choices=[
            "tabbar",
            "tab-title",
            "tearoff",
            "split-titlebar",
            "zoom-keybind",
            "underline",
            "agent-tui",
            "search-history",
            "interaction",
            "touchpad-scroll",
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

    if platform.system() != "Windows" and not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        print("live-ui smoke: skipped (no DISPLAY or WAYLAND_DISPLAY)", file=sys.stderr)
        return 0

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
        out = run_agent_tui(args.kettle, root, shell_target)
        print(f"agent-tui smoke: OK artifacts={out}")
    if args.case in ("search-history", "all"):
        out = run_search_history(args.kettle, root)
        print(f"search-history smoke: OK artifacts={out}")
    if args.case in ("interaction", "all"):
        out = run_interaction(args.kettle, root)
        print(f"interaction smoke: OK artifacts={out}")
    if args.case in ("touchpad-scroll", "all"):
        out = run_touchpad_scroll(args.kettle, root)
        print(f"touchpad-scroll smoke: OK artifacts={out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
