#!/usr/bin/env python3
"""Capture Kettle's UI states as PNGs, and optionally compare them to baselines.

    scripts/capture-ui-states.py [--kettle PATH] [--out DIR] [--states a,b]
                                 [--languages en,es] [--themes dark,light]
                                 [--sizes default,narrow] [--compare DIR]

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
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import sys
import tempfile
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


def write_config(path: Path, language: str, theme: str) -> None:
    lines = [
        "record = false",
        "restore-session = false",
        "check-for-updates = false",
        f"language = {language}",
        "theme = TokyoNight Day" if theme == "light" else "theme = TokyoNight Night",
        "cursor-blink = false",
        "window-title-format = {title}",
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


def capture_state(smoke, kettle: str, out: Path, name: str, language: str, theme: str,
                  size: str) -> Path:
    label = f"{name}.{language}.{theme}.{size}"
    with tempfile.TemporaryDirectory(prefix="kettle-ui-states-") as raw:
        scratch = Path(raw)
        os.chmod(scratch, 0o700)
        home = scratch / "home"
        home.mkdir(mode=0o700)
        cfg = scratch / "kettle.cfg"
        write_config(cfg, language, theme)
        with smoke.LiveKettle(kettle, cfg, scratch / "kettle.log",
                              extra_env=private_env(home)) as live:
            width, height = SIZES[size]
            live.json_ctl("resize_window", {"width": width, "height": height})
            smoke.live_shell_command(live, FIXTURE_TEXT, READY, timeout_ms=15000)
            STATES[name](live)
            live.wait_for_text(READY, timeout_ms=3000, quiet_ms=400)
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


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--kettle", default=str(ROOT / "target" / "release" / "kettle"))
    ap.add_argument("--out", default=str(ROOT / "target" / "ui-states"))
    ap.add_argument("--states", default=",".join(STATES))
    ap.add_argument("--languages", default="en")
    ap.add_argument("--themes", default="dark")
    ap.add_argument("--sizes", default="default")
    ap.add_argument("--compare", default="")
    ap.add_argument("--channel-tolerance", type=int, default=24)
    ap.add_argument("--max-changed", type=float, default=0.5)
    args = ap.parse_args()

    unknown = [s for s in args.states.split(",") if s not in STATES]
    if unknown:
        raise SystemExit(f"unknown states: {', '.join(unknown)}; known: {', '.join(STATES)}")
    out = Path(args.out)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True, mode=0o700)
    smoke = load_live_smoke()
    failures: List[str] = []
    results = []
    for language in args.languages.split(","):
        for theme in args.themes.split(","):
            for size in args.sizes.split(","):
                for name in args.states.split(","):
                    shot = capture_state(smoke, args.kettle, out, name, language, theme, size)
                    verdict = "captured"
                    if args.compare:
                        verdict = compare(shot, Path(args.compare) / shot.name,
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
