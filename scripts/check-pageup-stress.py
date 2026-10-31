#!/usr/bin/env python3
"""Bounded native macOS page-up reproduction with retained artifact identities."""

import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time


SPEC = importlib.util.spec_from_file_location(
    "pageup_modal", Path(__file__).with_name("check-modal-shortcuts.py")
)
modal = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = modal
SPEC.loader.exec_module(modal)

CONFIG = (
    "agent-server = full\nrestore-session = false\nupdate-policy = off\n"
    "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
    "window-width = 110\nwindow-height = 34\nbackground-opacity = 1.0\n"
    "window-blur = false\n"
)
VERSION_TIMEOUT_S = 10


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def probe_version(command):
    return subprocess.check_output(command, text=True, timeout=VERSION_TIMEOUT_S).strip()


def wait_for_scrollback(live, marker, timeout=20):
    """An echoed command is not proof that its 1,600 lines were produced."""
    deadline = time.monotonic() + timeout
    while True:
        screen = live.json_ctl("read_screen")
        if (marker in screen.get("text", "").splitlines()
                and screen.get("history_size", 0) > 1500):
            return screen
        if time.monotonic() >= deadline:
            raise RuntimeError("1,600 output lines not observed; fixture is not ready")
        time.sleep(0.05)


def navigate(live, mode, up, native_key):
    action = "scroll_page_up" if up else "scroll_page_down"
    if mode == "control":
        live.json_ctl("perform_action", {"action": action})
    elif mode == "keybind":
        result = live.json_ctl("dispatch_keybind", {
            "logical": "pageup" if up else "pagedown", "mods": "shift",
        })
        expected = "ScrollPageUp" if up else "ScrollPageDown"
        if result.get("action") != expected:
            raise RuntimeError(f"unexpected keybind result: {result}")
    else:
        native_key(live, 116 if up else 121, 1 << 17)


def exercise(live, out, trial, native_key=modal.key):
    marker = f"PAGEUP_OUTPUT_DONE_{trial:02d}"
    command = (
        "i=1; while [ $i -le 1600 ]; do printf 'PAGEUP_LINE_%04d\\n' $i; "
        "i=$((i+1)); done; printf '%s%s\\n' PAGEUP_OUTPUT_ "
        f"DONE_{trial:02d}\r"
    )
    live.ctl("send_text", params={"text": command})
    wait_for_scrollback(live, marker)
    time.sleep(0.3)
    before = live.json_ctl("read_screen")
    write_json(out / "before.json", before)
    live.screenshot(out / "before.png")
    if before.get("display_offset") != 0 or before.get("history_size", 0) <= 1500:
        raise RuntimeError("baseline is not settled at live bottom with scrollback")
    pairs = []
    for mode, count in [("control", 10), ("keybind", 5), ("native", 5)]:
        for cycle in range(count):
            navigate(live, mode, True, native_key)
            up = live.json_ctl("read_screen")
            write_json(out / f"{mode}-{cycle:02d}-up.json", up)
            if up.get("display_offset", 0) <= 0:
                raise RuntimeError(f"{mode} page-up did not enter scrollback")
            if cycle == 0:
                live.screenshot(out / f"{mode}-up.png")
            navigate(live, mode, False, native_key)
            down = live.json_ctl("read_screen")
            write_json(out / f"{mode}-{cycle:02d}-down.json", down)
            if down.get("display_offset") != 0:
                raise RuntimeError(f"{mode} page-down did not restore live bottom")
            pairs.append({"mode": mode, "up_offset": up["display_offset"],
                          "down_offset": down["display_offset"]})
    return pairs


def check(kettle, out, windows):
    if platform.system() != "Darwin":
        raise RuntimeError("native keyboard qualification requires macOS")
    out.mkdir(parents=True, mode=0o700, exist_ok=False)
    write_json(out / "identity.json", {
        "started": datetime.datetime.now(datetime.UTC).isoformat(),
        "version": probe_version([str(kettle), "--version"]),
        "executable": str(kettle), "executable_sha256": digest(kettle),
        "os": platform.platform(),
        "scripts": {name: digest(Path(__file__).with_name(name)) for name in [
            "check-pageup-stress.py", "check-modal-shortcuts.py", "check-live-ui-smoke.py",
        ]},
        "windows": windows, "pairs_per_window": 20,
    })
    results = []
    for trial in range(windows):
        folder = out / f"trial-{trial:02d}"
        folder.mkdir(mode=0o700)
        config = folder / "config"
        config.write_text(CONFIG)
        row = {"trial": trial, "load_before": os.getloadavg()}
        try:
            with modal.RecordedLive(str(kettle), config, folder / "kettle.log",
                                    extra_env={"SHELL": "/bin/sh"}) as live:
                row["pid"] = live.pid
                write_json(folder / "owner.json", {"pid": live.pid})
                try:
                    row["pairs"] = exercise(live, folder, trial)
                except BaseException:
                    row["load_on_failure"] = os.getloadavg()
                    for label, method in [("failure-screen", "read_screen"),
                                          ("failure-geometry", "ui_geometry")]:
                        try:
                            write_json(folder / f"{label}.json", live.json_ctl(method))
                        except Exception:
                            pass
                    try:
                        live.screenshot(folder / "failure.png")
                    except Exception:
                        pass
                    raise
            row["result"] = "PASS"
        except BaseException as error:
            row["result"] = "FAIL"
            row["error"] = repr(error)
            row.setdefault("load_on_failure", os.getloadavg())
            raise
        finally:
            row["load_after"] = os.getloadavg()
            results.append(row)
            write_json(out / "results.json", results)
        print(f"page-up stress: window {trial + 1}/{windows} passed", flush=True)


def window_count(value):
    count = int(value)
    if not 1 <= count <= 30:
        raise argparse.ArgumentTypeError("windows must be between 1 and 30")
    return count


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True, type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument("--windows", type=window_count, default=10)
    args = parser.parse_args()
    check(args.kettle.resolve(strict=True), args.out_dir.resolve(), args.windows)
