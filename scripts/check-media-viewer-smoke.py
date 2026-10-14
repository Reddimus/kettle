#!/usr/bin/env python3
"""Exercise the centered viewer with native input in an isolated Linux X server."""

import argparse
import importlib.util
import json
import os
import shlex
import sys
import tempfile
import time
from pathlib import Path

from native_x11 import X11Input


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


SCRIPTS = Path(__file__).resolve().parent
live = load("viewer_live", SCRIPTS / "check-live-ui-smoke.py")
media = load("viewer_media", SCRIPTS / "check-lane-media-smoke.py")


def wait_for(probe, timeout=15):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        last = probe()
        if last:
            return last
        time.sleep(0.05)
    raise AssertionError(f"viewer condition timed out: {last!r}")


def grid(screen):
    return screen["cols"], screen["rows"], screen["text"], screen["cursor"]


def check(kettle, worker, out, case):
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    result = {"case": case, "pass": False, "checks": []}
    try:
        with tempfile.TemporaryDirectory(prefix="kettle-viewer-") as scratch:
            root = Path(scratch)
            binary = media.stage(Path(kettle), Path(worker), root / "bin")
            config = root / "kettle.conf"
            config.write_text(
                "restore-session = false\nupdate-policy = off\nrecord = off\n"
                "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
                "agent-server = full\nagent-display = true\ncursor-blink = false\n"
                "window-width = 120\nwindow-height = 40\nstatus-bar = off\n"
            )
            environment = {"SHELL": "/bin/sh", "ENV": None, "BASH_ENV": None}
            for key in ("CONFIG", "STATE", "DATA", "CACHE", "RUNTIME"):
                path = root / key.lower()
                path.mkdir(mode=0o700)
                environment[f"XDG_{key}_HOME" if key != "RUNTIME" else "XDG_RUNTIME_DIR"] = str(path)
            svg = root / "graph.svg"
            svg.write_text(
                '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="500">'
                '<rect width="800" height="500" fill="#286c58"/></svg>'
            )
            with (
                live.LiveKettle(binary, config, out / "kettle.log", extra_env=environment) as app,
                X11Input() as native,
            ):
                geometry = lambda: app.json_ctl("ui_geometry")
                screen = lambda: app.json_ctl("read_screen")
                wait_for(lambda: app.json_ctl("get_state")["media"]["availability"] == "available")
                window = wait_for(lambda: native.windows(app.pid))[-1]
                native.focus(window)
                # The real pane shell runs show, preserving process provenance.
                app.json_ctl("send_text", {"text": shlex.join([binary, "show", str(svg)]) + "\n"})
                wait_for(lambda: "Sent to the Kettle media shelf" in screen()["text"])
                settled = app.json_ctl("wait_for", {
                    "text": "Sent to the Kettle media shelf", "quiet_ms": 150, "timeout_ms": 5000,
                })
                assert settled["matched"], "pane output did not settle before the viewer opened"
                before = screen()
                pane = geometry()["panes"][0]["terminal"]
                app.json_ctl("perform_action", {"action": "open_media_shelf"})
                overlay = wait_for(lambda: geometry()["preview_overlay"])
                rect = overlay["rect"]
                for axis, size in (("x", "width"), ("y", "height")):
                    assert abs(rect[size] / pane[size] - 0.9) < 0.0001
                    assert abs(rect[axis] - pane[axis] - pane[size] * 0.05) < 0.01
                assert grid(screen()) == grid(before), "opening resized or changed the terminal"
                result["checks"].append("split-relative 90 percent frame; terminal grid unchanged")
                if case in ("all", "focus"):
                    native.focus(0)
                    wait_for(lambda: not geometry()["window_focused"])
                    native.focus(window)
                    wait_for(lambda: geometry()["window_focused"])
                    native.keys("x")
                    after = screen()
                    assert geometry()["preview_overlay"]["keyboard"] == "preview", (
                        "focus loss released viewer keyboard ownership"
                    )
                    assert grid(after) == grid(before), "input leaked into the terminal after focus returned"
                    result["checks"].append("focus round trip retains modal keyboard and terminal contents")
                if case in ("all", "pan"):
                    native.keys("equal")
                    native.keys("equal")
                    zoomed = wait_for(lambda: (lambda v: v if v and v["zoom"] > 1.4 else None)(
                        geometry()["preview_overlay"]))
                    content = zoomed["content"]
                    x = int(content["x"] + content["width"] / 2)
                    y = int(content["y"] + content["height"] / 2)
                    native.move(window, x, y)
                    native.button(1, True)
                    try:
                        for step in range(1, 9):
                            native.move(window, x + step * 10, y + step * 5)
                            time.sleep(0.03)
                    finally:
                        native.button(1, False)
                    image = zoomed["image"]
                    expected = {
                        axis: min(image[axis] + distance, content[axis])
                        if image[size] > content[size] else image[axis]
                        for axis, size, distance in (("x", "width", 80), ("y", "height", 40))
                    }
                    until = time.monotonic() + 2
                    while True:
                        panned = geometry()["preview_overlay"]
                        complete = all(abs(panned["image"][axis] - position) <= 2
                                       for axis, position in expected.items())
                        if complete or time.monotonic() >= until:
                            break
                        time.sleep(0.05)
                    result["pan"] = {"before": image, "after": panned["image"], "expected": expected}
                    assert complete, "ordinary event-loop turns cancelled the viewer pan"
                    assert grid(screen()) == grid(before), "panning changed the terminal"
                    result["checks"].append("zoomed viewer pans across event-loop turns")
                app.screenshot(out / "viewer.png")
                expected_zoom = geometry()["preview_overlay"]["zoom"]
                native.keys("p")
                pinned = wait_for(lambda: (lambda g:
                    g if g["preview_overlay"] is None and g["preview_lanes"] else None)(geometry()))
                assert pinned["preview_lanes"][0]["zoom"] == expected_zoom
                assert screen()["rows"] < before["rows"], "pinning did not make room beside the terminal"
                result["checks"].append("pin retains preview and resizes the grid only at pin")
                result["pass"] = True
    finally:
        (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--worker", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--case", choices=("all", "focus", "pan"), default="all")
    args = parser.parse_args()
    if sys.platform != "linux" or not os.environ.get("DISPLAY"):
        parser.error("run in an isolated Linux X server, such as xvfb-run")
    os.umask(0o077)
    check(args.kettle, args.worker, Path(args.out), args.case)


if __name__ == "__main__":
    main()
