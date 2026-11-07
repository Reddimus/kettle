#!/usr/bin/env python3
"""Capture Kettle's UI states as PNGs, and optionally compare them to baselines.

    scripts/capture-ui-states.py [--kettle PATH] [--out DIR] [--states a,b]
                                 [--languages en,es] [--themes dark,light]
                                 [--sizes default,narrow] [--compare DIR]
                                 [--geometry-only]

Each state starts from a fresh, isolated Kettle: a private 0700 HOME and XDG
directories, `record = false`, and a config written only for this run, so the
developer's own config, sessions and recordings are never read or written. The
state is reached through `kettle ctl` (actions and UI keys, never a desktop
input path) and captured with `kettle ctl screenshot`, which renders the
window's own frame: no desktop, other windows or OS decorations.

With --compare, every capture is checked against the PNG of the same name in
DIR. A pixel differs when any channel differs by more than --channel-tolerance;
a state fails when more than --max-changed percent of its pixels differ, or when
the sizes differ. A missing baseline is reported, not created.
Output and baseline directories must be disjoint, including resolved symlink
aliases and ancestor/descendant paths. Output must not be a symlink.
--geometry-only asserts state and layout
from ui_geometry and saves JSON instead of PNGs; it cannot be used with --compare.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

ROOT = Path(__file__).resolve().parent.parent
FIXTURE_TEXT = (
    "printf '\\033[2J\\033[3J\\033[H'; "
    "for i in $(seq 1 40); do printf 'line %02d: error warning info https://example.org/p%d\\n' "
    "\"$i\" \"$i\"; done; printf '%s%s\\n' UI_STATES_ READY"
)
READY = "UI_STATES_READY"


def load_live_smoke():
    spec = importlib.util.spec_from_file_location(
        "kettle_live_ui_smoke", ROOT / "scripts" / "check-live-ui-smoke.py")
    if spec is None or spec.loader is None:
        raise SystemExit("cannot load scripts/check-live-ui-smoke.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def keys(live, *chords: str) -> None:
    live.json_ctl("dispatch_ui_key", {"keys": list(chords)})


def type_text(live, text: str) -> None:
    live.json_ctl("dispatch_ui_key", {"keys": list(text)})


def action(live, name: str) -> None:
    live.json_ctl("perform_action", {"action": name})


# name -> steps run on a fresh Kettle that already shows the fixture text.
STATES: Dict[str, Callable] = {
    "default": lambda live: None,
    "palette-empty": lambda live: action(live, "command_palette"),
    "palette-query": lambda live: (action(live, "command_palette"), type_text(live, "split")),
    "palette-nomatch": lambda live: (action(live, "command_palette"), type_text(live, "qqzzxx")),
    "settings": lambda live: action(live, "open_settings"),
    "search-matches": lambda live: (action(live, "start_search"), type_text(live, "error")),
    "search-nomatch": lambda live: (action(live, "start_search"), type_text(live, "zzzqqq")),
    "tabs": lambda live: (action(live, "new_tab"), action(live, "new_tab")),
    "splits": lambda live: (action(live, "split_right"), action(live, "split_down")),
    "theme-picker": lambda live: action(live, "open_theme_picker"),
}

SIZES: Dict[str, Tuple[int, int]] = {"default": (1280, 820), "narrow": (640, 820)}


def write_config(path: Path, language: str, theme: str, shell: Path) -> None:
    lines = [
        "record = false",
        "restore-session = false",
        "check-for-updates = false",
        f"language = {language}",
        "theme = Kettle Day" if theme == "light" else "theme = TokyoNight Night",
        "cursor-blink = false",
        "window-title-format = {title}",
        # Every tab and split runs the same plain shell (see capture_state).
        f"command = {shell}",
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def private_env(home: Path) -> Dict[str, Optional[str]]:
    env: Dict[str, Optional[str]] = {"HOME": str(home)}
    for name, sub in (("XDG_CONFIG_HOME", ".config"), ("XDG_DATA_HOME", ".local/share"),
                      ("XDG_STATE_HOME", ".local/state"), ("XDG_CACHE_HOME", ".cache"),
                      ("XDG_RUNTIME_DIR", "run")):
        d = home / sub
        d.mkdir(parents=True, exist_ok=True, mode=0o700)
        env[name] = str(d)
    env["KETTLE_CONFIG"] = None
    return env


def assert_geometry(geometry, name: str) -> None:
    """Check the fixed state matrix in physical surface coordinates."""
    surface = dict(x=0, y=0, **geometry["surface"])

    def inside(rect, parent, label):
        assert all(math.isfinite(rect[key]) for key in ("x", "y", "width", "height")), label
        assert rect["width"] > 0 and rect["height"] > 0, f"{label}: empty rectangle"
        for axis, extent in (("x", "width"), ("y", "height")):
            # Layout uses f32 coordinates; allow one physical pixel at shared edges.
            assert rect[axis] >= parent[axis] - 1, f"{label}: outside {axis} origin"
            assert rect[axis] + rect[extent] <= parent[axis] + parent[extent] + 1, (
                f"{label}: outside {extent}"
            )

    inside(surface, surface, "surface")
    scale = geometry["scale_factor"]
    assert math.isfinite(scale) and scale > 0, "invalid backing scale"
    content = geometry["content"]
    inside(content, surface, "content")
    panes = geometry["panes"]
    assert len(panes) == (3 if name == "splits" else 1), "wrong pane count"
    assert sum(pane["focused"] is True for pane in panes) == 1, "pane focus is missing or duplicated"
    for index, pane in enumerate(panes):
        inside(pane["rect"], content, "pane")
        inside(pane["terminal"], pane["rect"], "terminal")
        for other in panes[:index]:
            a, b = pane["rect"], other["rect"]
            assert any(a[axis] + a[extent] <= b[axis] + 1 or
                       b[axis] + b[extent] <= a[axis] + 1
                       for axis, extent in (("x", "width"), ("y", "height"))), "panes overlap"

    bar = geometry["tab_bar"]
    if bar["height"] > 0:
        inside(bar["band"], surface, "tab band")
        for control in ("new_tab", "new_tab_menu"):
            inside(bar[control], bar["band"], control)
        assert len(bar["segments"]) == (3 if name == "tabs" else 1), "wrong tab count"
        assert sum(tab["active"] is True for tab in bar["segments"]) == 1, "active tab is missing"
        for tab in bar["segments"]:
            if tab["rect"]["x"] < -1000:  # the native checker also skips parked overflow tabs
                assert not tab["active"], "active tab is parked offscreen"
                continue
            inside(tab["rect"], bar["band"], "tab")
        if bar["in_titlebar"]:
            assert bar["y"] == bar["band"]["y"] == 0, "titlebar moved from top"
            assert bar["band"]["x"] > 0 or geometry["fullscreen"], "tabs overlap traffic lights"
            assert content["y"] >= bar["height"], "content overlaps titlebar"
            assert bar["height"] >= 20 * scale, "titlebar is too short"
            for control in ("new_tab", "new_tab_menu"):
                assert min(bar[control]["width"], bar[control]["height"]) >= 20 * scale, (
                    f"{control}: titlebar hit target is too small"
                )
    else:
        assert name != "tabs", "three-tab strip is hidden"

    expected = ("palette" if name.startswith("palette-") else
                "search" if name.startswith("search-") else
                {"settings": "settings", "theme-picker": "theme_picker"}.get(name))
    active = {key for key, value in geometry["modals"].items() if value}
    assert active == ({expected} if expected else set()), f"unexpected modals: {active}"
    if expected in ("palette", "theme_picker"):
        picker = geometry["picker"]
        inside(picker["rect"], surface, "picker")
        inside(picker["rows_rect"], picker["rect"], "picker results")
        assert picker["rows"], "picker has no visible rows"
        for row in picker["rows"]:
            inside(row["rect"], picker["rows_rect"], "picker row")
        assert any(row["enabled"] for row in picker["rows"]) == (name != "palette-nomatch"), (
            "picker match state is wrong"
        )
    elif expected == "settings":
        settings = geometry["settings"]
        inside(settings["rect"], surface, "settings")
        for key in ("content_rect", "search_rect"):
            inside(settings[key], settings["rect"], key)
        assert settings["rows"], "settings has no visible rows"
        for row in settings["rows"]:
            inside(row["rect"], settings["content_rect"], "settings row")
    elif expected == "search":
        search = geometry["search"]
        inside(search["rect"], surface, "search")
        assert content["y"] + content["height"] <= search["rect"]["y"] + 1, (
            "terminal content overlaps search"
        )
        assert search["has_match"] is (name == "search-matches"), "search match state is wrong"
        assert {control["name"] for control in search["controls"]} == {
            "editor", "previous", "next", "wrap", "case", "regex", "invert", "close"
        }, "search controls missing"
        for control in search["controls"]:
            inside(control["rect"], search["rect"], control["name"])


def capture_state(smoke, kettle: str, out: Path, name: str, language: str, theme: str,
                  size: str, geometry_only: bool = False) -> Path:
    label = f"{name}.{language}.{theme}.{size}"
    with tempfile.TemporaryDirectory(prefix="kettle-ui-states-") as raw:
        scratch = Path(raw)
        os.chmod(scratch, 0o700)
        home = scratch / "home"
        home.mkdir(mode=0o700)
        # `command` names a program, so the shell's flags live in a wrapper,
        # and a fixed prompt and title live in its rc file: images must not
        # carry the account's shell setup, the host name or the scratch path.
        rc = scratch / "shell.rc"
        rc.write_text("PS1='$ '\nPROMPT_COMMAND='printf \"\\033]0;shell\\007\"'\n",
                      encoding="utf-8")
        shell = scratch / "shell.sh"
        shell.write_text(f"#!/bin/sh\nexec bash --noprofile --rcfile '{rc}' -i\n",
                         encoding="utf-8")
        os.chmod(shell, 0o700)
        cfg = scratch / "kettle.cfg"
        write_config(cfg, language, theme, shell)
        env = private_env(home)
        with smoke.LiveKettle(kettle, cfg, scratch / "kettle.log", extra_env=env) as live:
            width, height = SIZES[size]
            live.json_ctl("resize_window", {"width": width, "height": height})
            smoke.live_shell_command(live, FIXTURE_TEXT, READY, timeout_ms=15000)
            STATES[name](live)
            # Let the state's own frame settle before capturing it.
            time.sleep(0.6)
            if geometry_only:
                target = out / f"{label}.geometry.json"
                geometry = live.json_ctl("ui_geometry")
                target.write_text(json.dumps(geometry, indent=2) + "\n", encoding="utf-8")
                assert_geometry(geometry, name)
                return target
            target = out / f"{label}.png"
            live.screenshot(target)
    return target


def compare(path: Path, baseline: Path, channel_tolerance: int, max_changed: float) -> str:
    from PIL import Image, ImageChops  # optional dependency, compare mode only

    if not baseline.exists():
        return "no baseline"
    a = Image.open(path).convert("RGBA")
    b = Image.open(baseline).convert("RGBA")
    if a.size != b.size:
        return f"size {a.size} != baseline {b.size}"
    diff = ImageChops.difference(a, b)
    over = sum(1 for px in diff.getdata() if max(px) > channel_tolerance)
    share = 100.0 * over / (a.size[0] * a.size[1])
    return "ok" if share <= max_changed else f"{share:.2f}% of pixels changed"


def directory_contains(parent: Path, child: Path) -> bool:
    if parent == child or parent in child.parents:
        return True
    # realpath preserves case on macOS, even on case-insensitive volumes.
    return parent.exists() and any(
        candidate.exists() and parent.samefile(candidate)
        for candidate in (child, *child.parents)
    )


def main(*, smoke_loader=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--kettle", default=str(ROOT / "target" / "release" / "kettle"))
    ap.add_argument("--out", default=str(ROOT / "target" / "ui-states"))
    ap.add_argument("--states", default=",".join(STATES))
    ap.add_argument("--languages", default="en")
    ap.add_argument("--themes", default="dark")
    ap.add_argument("--sizes", default="default")
    ap.add_argument("--compare", default="")
    ap.add_argument("--geometry-only", action="store_true",
                    help="assert UI state geometry and save JSON instead of PNGs (no Pillow)")
    ap.add_argument("--channel-tolerance", type=int, default=24)
    ap.add_argument("--max-changed", type=float, default=0.5)
    args = ap.parse_args()

    unknown = [s for s in args.states.split(",") if s not in STATES]
    if unknown:
        raise SystemExit(f"unknown states: {', '.join(unknown)}; known: {', '.join(STATES)}")
    output_path = Path(args.out)
    out = output_path.resolve()
    baseline = Path(args.compare).resolve() if args.compare else None
    if baseline is not None and (
            directory_contains(out, baseline) or directory_contains(baseline, out)):
        raise SystemExit("--out and --compare directories overlap; use disjoint directories")
    if output_path.is_symlink():
        raise SystemExit("--out must not be a symlink; use a directory directly")
    if args.geometry_only and baseline is not None:
        ap.error("--geometry-only cannot compare PNG baselines")
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True, mode=0o700)
    smoke = (smoke_loader or load_live_smoke)()
    failures: List[str] = []
    results = []
    for language in args.languages.split(","):
        for theme in args.themes.split(","):
            for size in args.sizes.split(","):
                for name in args.states.split(","):
                    try:
                        if args.geometry_only:
                            shot = capture_state(smoke, args.kettle, out, name, language, theme,
                                                 size, geometry_only=True)
                        else:
                            shot = capture_state(smoke, args.kettle, out, name, language, theme, size)
                    except (AssertionError, KeyError, TypeError, ValueError) as error:
                        if not args.geometry_only:
                            raise
                        label = f"{name}.{language}.{theme}.{size}.geometry.json"
                        verdict = f"geometry: {error}"
                        failures.append(f"{label}: {verdict}")
                        results.append({"file": label, "result": verdict})
                        print(f"{label}: {verdict}", flush=True)
                        continue
                    verdict = "geometry ok" if args.geometry_only else "captured"
                    if baseline is not None:
                        verdict = compare(shot, baseline / shot.name,
                                          args.channel_tolerance, args.max_changed)
                        if verdict != "ok":
                            failures.append(f"{shot.name}: {verdict}")
                    results.append({"file": shot.name, "result": verdict})
                    print(f"{shot.name}: {verdict}", flush=True)
    (out / "ui-states.json").write_text(json.dumps(results, indent=1) + "\n", encoding="utf-8")
    if failures:
        print("ui-states: " + "; ".join(failures), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
