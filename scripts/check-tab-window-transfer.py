#!/usr/bin/env python3
"""Check live tab transfers with native input in an isolated Linux X server."""

import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time

from native_x11 import X11Input


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


HERE = Path(__file__).resolve().parent
live = load("transfer_live", HERE / "check-live-ui-smoke.py")
media = load("transfer_media", HERE / "check-lane-media-smoke.py")


def wait_for(probe, label, timeout=15):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        value = probe()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f"timed out: {label}")


def focused_native(native, pid):
    native.x.XGetInputFocus.argtypes = [ctypes.c_void_p,
                                      ctypes.POINTER(ctypes.c_ulong),
                                      ctypes.POINTER(ctypes.c_int)]
    native.x.XGetInputFocus.restype = ctypes.c_int
    window, revert = ctypes.c_ulong(), ctypes.c_int()
    native.x.XGetInputFocus(native.display, ctypes.byref(window), ctypes.byref(revert))
    return window.value if window.value in native.windows(pid) else None


def scenario(binary, out, case, language):
    result = {"case": case, "pass": False}
    with tempfile.TemporaryDirectory(prefix="kettle-window-transfer-") as scratch:
        root = Path(scratch)
        config = root / "kettle.conf"
        config.write_text(
            "restore-session = false\nupdate-policy = off\nrecord = off\n"
            "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
            "window-width = 100\nwindow-height = 32\ncursor-blink = false\n"
            "accent-color = auto\nask-before-closing = never\n"
            f"language = {language}\n"
        )
        env = {"SHELL": "/bin/sh", "ENV": None, "BASH_ENV": None}
        for name in ("CONFIG", "STATE", "DATA", "CACHE", "RUNTIME"):
            path = root / name.lower()
            path.mkdir(mode=0o700)
            env[f"XDG_{name}_HOME" if name != "RUNTIME" else "XDG_RUNTIME_DIR"] = str(path)
        # Keep ctl discovery in the same private runtime as the test process.
        saved = {key: os.environ.get(key) for key in env}
        for key, value in env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        try:
            with live.LiveKettle(binary, config, out / f"{case}.log") as app, X11Input() as native:
                try:
                    tabs = lambda: app.json_ctl("list_tabs")["tabs"]
                    panes = lambda: app.json_ctl("list_panes")["panes"]
                    state = lambda: app.json_ctl("get_state")
                    geometry = lambda: app.json_ctl("ui_geometry")
                    action = lambda name: app.json_ctl("perform_action", {"action": name})
                    original = tabs()[0]["window"]
                    original_pane = tabs()[0]["focused_pane"]
                    owned = wait_for(lambda: native.windows(app.pid), "owned window")
                    assert len(owned) == 1
                    native.focus(owned[0])
                    original_native = wait_for(lambda: focused_native(native, app.pid), "owned focus")
                    if case == "empty":
                        before = app.json_ctl("read_screen")
                        action("move_tab_to_window")
                        menu = wait_for(lambda: geometry().get("context_menu"), "empty chooser")
                        assert menu["rows"] and not any(row["dispatchable"] for row in menu["rows"])
                        app.screenshot(out / f"{case}-{language}.png")
                        native.keys("Escape")
                        wait_for(lambda: geometry().get("context_menu") is None, "dismiss chooser")
                        after = app.json_ctl("read_screen")
                        assert all(before[key] == after[key] for key in ("cols", "rows", "text", "cursor"))
                        result["checks"] = ["empty destinations are explicit", "Escape preserves terminal"]
                    else:
                        action("new_tab")
                        wait_for(lambda: len(tabs()) == 2, "second tab")
                        moving = state()["focused_pane"]
                        app.json_ctl("send_text", {"text": "printf 'WINDOW_TRANSFER_MARKER\\n'\n"})
                        wait_for(lambda: "WINDOW_TRANSFER_MARKER" in app.json_ctl("read_screen")["text"], "marker")
                        before_pids = {p["id"]: p["child_pid"] for p in panes()}
                        action("new_window" if case == "multi" else "move_tab_to_new_window")
                        wait_for(lambda: state()["windows"] == 2, "second window")
                        other = wait_for(lambda: state()["focused_window"] if state()["focused_window"] != original else None, "new window focus")
                        other_native = wait_for(lambda: next((window for window in native.windows(app.pid) if window != original_native), None), "new native window")
                        native.focus(other_native)
                        if case == "multi":
                            native.focus(original_native)
                            wait_for(lambda: state()["focused_window"] == original, "source focus")
                        claims = [json.loads(p.read_text()) for p in root.rglob(f"{app.pid}-w*.json")]
                        colors = {p["win"]: p["rgb"] for p in claims}
                        assert colors.get(original) and colors.get(other) and colors[original] != colors[other]
                        result["accents"] = colors
                        wait_for(lambda: focused_native(native, app.pid), "owned chooser input")
                        action("move_tab_to_window")
                        menu = wait_for(lambda: geometry().get("context_menu"), "destination chooser")
                        row = next(row for row in menu["rows"] if row["dispatchable"])
                        app.screenshot(out / f"{case}-{language}.png")
                        if case == "stale":
                            app.json_ctl("send_text", {"pane": original_pane, "text": "exit\n"})
                            wait_for(lambda: state()["windows"] == 1, "closed destination")
                        if case == "pointer":
                            window = wait_for(lambda: focused_native(native, app.pid), "owned pointer target")
                            rect = row["rect"]
                            native.move(window, round(rect["x"] + rect["width"] / 2),
                                        round(rect["y"] + rect["height"] / 2))
                            native.button(1, True)
                            native.button(1, False)
                        else:
                            native.keys("Return")
                        expected = other if case in ("multi", "stale") else original
                        moved = wait_for(lambda: next((tab for tab in tabs() if moving in tab["panes"] and tab["window"] == expected), None), "original tab kept or moved")
                        if case != "multi":
                            wait_for(lambda: state()["windows"] == 1, "emptied or expired window closed")
                        else:
                            assert state()["windows"] == 2
                        after_pids = {p["id"]: p["child_pid"] for p in panes()}
                        assert after_pids[moving] == before_pids[moving]
                        if case != "stale":
                            assert all(after_pids[p] == child for p, child in before_pids.items())
                        text = app.json_ctl("read_screen", {"pane": moving})["text"]
                        assert "WINDOW_TRANSFER_MARKER" in text
                        result.update(checks=["different window accents", "live PTY and scrollback retained",
                                              "stale destination leaves tab intact" if case == "stale" else "selected destination receives original tab"],
                                      moving_pane=moving, tab=moved, child_pid=after_pids[moving])
                except BaseException:
                    for method in ("ui_geometry", "list_tabs", "list_panes"):
                        try:
                            (out / f"{case}-failure-{method}.json").write_text(json.dumps(app.json_ctl(method), indent=2) + "\n")
                        except (Exception, SystemExit):
                            pass
                    raise
        finally:
            for key, value in saved.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
    result["pass"] = True
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--worker", required=True)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--language", choices=("en", "es"), default="en")
    args = parser.parse_args()
    if sys.platform != "linux" or not os.environ.get("DISPLAY"):
        parser.error("run under an isolated Linux X server")
    os.umask(0o077)
    args.out.mkdir(parents=True, mode=0o700, exist_ok=False)
    result = {"pass": False, "cases": []}
    try:
        with tempfile.TemporaryDirectory(prefix="kettle-transfer-bin-") as stage:
            binary = media.stage(Path(args.kettle), Path(args.worker), Path(stage) / "bin")
            for case in ("empty", "keyboard", "pointer", "multi", "stale"):
                result["cases"].append(scenario(binary, args.out, case, args.language))
        result["pass"] = True
    finally:
        (args.out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
