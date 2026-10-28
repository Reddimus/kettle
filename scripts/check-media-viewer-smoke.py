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


def check_control_mouse(app):
    geometry = lambda: app.json_ctl("ui_geometry")
    overlay = geometry()["preview_overlay"]
    terminal = grid(app.json_ctl("read_screen"))
    image = overlay["image"]
    app.json_ctl("send_mouse", {
        "event": "click", "button": "right",
        "x": image["x"] + image["width"] / 2,
        "y": image["y"] + image["height"] / 2,
    })
    menu = wait_for(lambda: geometry().get("context_menu"))
    assert "Copy image" in {row["label"] for row in menu["rows"] if row["dispatchable"]}
    outside = {
        "event": "click", "button": "left",
        "x": overlay["rect"]["x"] - 1,
        "y": overlay["rect"]["y"] + overlay["rect"]["height"] / 2,
    }
    app.json_ctl("send_mouse", outside)
    wait_for(lambda: not geometry().get("context_menu"))
    assert geometry()["preview_overlay"], "dismissing the menu closed its viewer"
    app.json_ctl("send_mouse", outside)
    wait_for(lambda: geometry()["preview_overlay"] is None)
    assert grid(app.json_ctl("read_screen")) == terminal, "control clicks changed the terminal"
    app.json_ctl("perform_action", {"action": "open_media_shelf"})
    wait_for(lambda: geometry()["preview_overlay"])


def check_pages(app, native, binary, assets, out):
    def show(path):
        command = shlex.join([binary, "show", str(path)])
        command += " && printf 'MEDIA_READY_%s\\n' " + shlex.quote(path.name) + "\n"
        app.json_ctl("send_text", {"text": command})
        wait_for(lambda: "MEDIA_READY_" + path.name in app.json_ctl("read_screen")["text"])
        app.json_ctl("perform_action", {"action": "open_media_shelf"})
        wait_for(lambda: app.json_ctl("ui_geometry")["preview_overlay"])

    def color_page(name, blue):
        path = out / name
        path.unlink(missing_ok=True)
        app.screenshot(path)
        _, _, rows = live.read_rgba_png(path)
        if blue:
            count = sum(1 for row in rows for i in range(0, len(row), 4)
                        if row[i + 2] > 170 and row[i] < 85 and row[i + 1] < 120)
        else:
            count = sum(1 for row in rows for i in range(0, len(row), 4)
                        if abs(row[i] - 40) < 6 and abs(row[i + 1] - 108) < 6 and abs(row[i + 2] - 88) < 6)
        return count > 5000

    show(assets["pdf"])
    wait_for(lambda: color_page("pdf-first.png", False))
    native.keys("Next")
    wait_for(lambda: color_page("pdf-second.png", True))
    native.keys("Next")
    assert color_page("pdf-last-boundary.png", True)
    native.keys("Home")
    wait_for(lambda: color_page("pdf-home.png", False))
    native.keys("Prior")
    assert color_page("pdf-first-boundary.png", False)
    native.keys("End")
    wait_for(lambda: color_page("pdf-end.png", True))
    native.keys("Escape")
    wait_for(lambda: app.json_ctl("ui_geometry")["preview_overlay"] is None)
    show(assets["markdown"])
    wait_for(lambda: color_page("gallery-first.png", False))
    native.keys("Next")
    wait_for(lambda: not color_page("gallery-second.png", False) and
             app.json_ctl("ui_geometry")["preview_overlay"]["image"])
    assert (out / "gallery-first.png").read_bytes() != (out / "gallery-second.png").read_bytes()
    native.keys("Escape")
    wait_for(lambda: app.json_ctl("ui_geometry")["preview_overlay"] is None)


def check_video(app, native, binary, path, out):
    command = shlex.join([binary, "show", str(path)]) + " && printf 'VIDEO_%s\\n' READY\n"
    app.json_ctl("send_text", {"text": command})
    wait_for(lambda: "VIDEO_READY" in app.json_ctl("read_screen")["text"])
    app.json_ctl("perform_action", {"action": "open_media_shelf"})
    geometry = lambda: app.json_ctl("ui_geometry")
    viewer = lambda: geometry()["preview_overlay"]
    opened = wait_for(viewer)
    pane = next(p["terminal"] for p in geometry()["panes"] if p["id"] == opened["pane"])
    assert opened["presentation"] == "pane"
    for axis, size in (("x", "width"), ("y", "height")):
        assert abs(opened["rect"][size] / pane[size] - 0.9) < 0.0001
        assert abs(opened["rect"][axis] - pane[axis] - pane[size] * 0.05) < 0.01
    assert opened["image"]["width"] > 480, "video did not expand beyond its source size"
    assert "playback" not in opened, "opening a card started playback"
    assert opened["controls"]["play"]["y"] >= opened["image"]["y"] + opened["image"]["height"]
    assert opened["controls"]["play"]["width"] >= 44 and opened["controls"]["play"]["height"] >= 44
    assert opened["timeline"]["height"] >= 24
    assert opened["time_label"], "poster reserves its timestamp before playback"
    app.screenshot(out / "video-center-play.png")
    terminal = grid(app.json_ctl("read_screen"))
    window = native.windows(app.pid)[-1]

    def picture_click():
        image = viewer()["image"]
        native.move(window, int(image["x"] + image["width"] / 2),
                    int(image["y"] + image["height"] / 2))
        native.button(1, True)
        native.button(1, False)

    def playback(state):
        value = viewer().get("playback", {})
        return value if value.get("state") == state and value.get("frame") else None

    def frame(name):
        image = viewer()["image"]
        file = out / name
        app.screenshot(file)
        _, _, rows = live.read_rgba_png(file)
        return [rows[y][x * 4:x * 4 + 4]
                for y in range(int(image["y"]), int(image["y"] + image["height"]), 16)
                for x in range(int(image["x"]), int(image["x"] + image["width"]), 16)
                if not (abs(x - image["x"] - image["width"] / 2) < 60 and
                        abs(y - image["y"] - image["height"] / 2) < 60)]

    picture_click()
    wait_for(lambda: playback("playing"))
    assert viewer()["image"] == opened["image"], "starting playback moved the picture"
    assert viewer()["transport"] == opened["transport"], "starting playback moved the controls"
    first = frame("video-playing-first.png")
    time.sleep(0.4)
    assert first != frame("video-playing-next.png"), "playing video pixels did not change"
    picture_click()
    paused = wait_for(lambda: playback("paused"))
    time.sleep(0.8)
    still = frame("video-paused-first.png")
    time.sleep(0.4)
    assert still == frame("video-paused-next.png"), "paused video pixels kept changing"
    at = paused["position_ms"]
    native.keys("Right")
    seeked = wait_for(lambda: (lambda p: p if p and p["position_ms"] > at + 4000 else None)(playback("paused")))
    native.keys("Left")
    wait_for(lambda: (lambda p: p if p and p["position_ms"] < seeked["position_ms"] - 4000 else None)(playback("paused")))
    native.keys("k")
    wait_for(lambda: playback("playing"))
    native.keys("k")
    wait_for(lambda: playback("paused"))

    def click_control(name):
        rect = viewer()["controls"][name]
        native.move(window, int(rect["x"] + rect["width"] / 2), int(rect["y"] + rect["height"] / 2))
        native.button(1, True)
        native.button(1, False)

    click_control("play")
    wait_for(lambda: playback("playing"))
    click_control("play")
    wait_for(lambda: playback("paused"))
    track = viewer()["controls"]["scrubber"]
    assert track["width"] >= viewer()["content"]["width"] * 0.8
    assert track["height"] >= 24 and track["y"] + track["height"] <= viewer()["controls"]["play"]["y"]
    native.move(window, int(track["x"] + track["width"] * 0.25), int(track["y"] + track["height"] / 2))
    wait_for(lambda: viewer()["hovered_control"] == "scrubber")
    app.screenshot(out / "video-timeline-hover.png")
    native.button(1, True)
    try:
        wait_for(lambda: viewer()["scrubbing"])
        native.move(window, int(track["x"] + track["width"] * 0.75), int(track["y"] + track["height"] - 2))
    finally:
        native.button(1, False)
    seeked = wait_for(lambda: (lambda p: p if p and abs(p["position_ms"] / p["duration_ms"] - 0.75) < 0.04 else None)(playback("paused")))
    assert not viewer()["scrubbing"]
    app.screenshot(out / "video-timeline-seeked.png")
    native.keys("t")
    larger = wait_for(lambda: (lambda v: v if v["presentation"] == "theater" else None)(viewer()))
    assert larger["rect"]["height"] > opened["rect"]["height"]
    assert larger["rect"]["x"] > 0 and larger["rect"]["y"] > 0
    native.keys("t")
    wait_for(lambda: viewer()["presentation"] == "pane")
    assert viewer()["rect"] == opened["rect"]
    # Verify the viewer's fullscreen request/exit stack even without a WM.
    # Native compositor acceptance separately verifies display fullscreen.
    native.keys("f")
    wait_for(lambda: viewer()["presentation"] == "fullscreen")
    native.keys("Escape")
    wait_for(lambda: viewer() and viewer()["presentation"] == "pane")
    assert grid(app.json_ctl("read_screen")) == terminal, "video controls changed the terminal"
    app.screenshot(out / "video-paused-theater.png")
    (out / "video-controls.json").write_text(json.dumps(viewer(), indent=2) + "\n")
    native.keys("Escape")
    wait_for(lambda: geometry()["preview_overlay"] is None)

    app.json_ctl("perform_action", {"action": "split_vert"})
    wait_for(lambda: len(geometry()["panes"]) == 2)
    app.json_ctl("send_text", {"text": command})
    wait_for(lambda: "VIDEO_READY" in app.json_ctl("read_screen")["text"])
    app.json_ctl("perform_action", {"action": "open_media_shelf"})
    split = wait_for(viewer)
    pane = next(p["terminal"] for p in geometry()["panes"] if p["id"] == split["pane"])
    assert abs(split["rect"]["width"] / pane["width"] - 0.9) < 0.0001
    native.keys("t")
    theater = wait_for(lambda: (lambda v: v if v["presentation"] == "theater" else None)(viewer()))
    assert theater["rect"]["width"] > split["rect"]["width"] * 1.5
    file = out / "video-split-theater.png"
    app.screenshot(file)
    _, _, rows = live.read_rgba_png(file)
    row = rows[int(theater["content"]["y"] - 3)]
    left = int(theater["rect"]["x"] + 2) * 4
    right = int(theater["rect"]["x"] + theater["rect"]["width"] - 3) * 4
    assert row[left:left + 4] == row[right:right + 4], "backdrop shades part of the expanded viewer"
    native.keys("Escape")
    wait_for(lambda: geometry()["preview_overlay"] is None)


def check_large_video(app, native, binary, out):
    path = SCRIPTS.parent / "crates/kettle-media-native/tests/fixtures/video/large-index.webm"
    app.json_ctl("resize_window", {"width": 1920, "height": 1080})
    wait_for(lambda: app.json_ctl("ui_geometry")["surface"] == {"width": 1920, "height": 1080})
    command = shlex.join([binary, "show", str(path)]) + " && printf 'LARGE_VIDEO_%s\\n' READY\n"
    app.json_ctl("send_text", {"text": command})
    wait_for(lambda: "LARGE_VIDEO_READY" in app.json_ctl("read_screen")["text"])
    app.json_ctl("perform_action", {"action": "open_media_shelf"})
    viewer = lambda: app.json_ctl("ui_geometry")["preview_overlay"]
    wait_for(viewer)
    native.keys("t")
    opened = wait_for(lambda: (lambda v: v if v["presentation"] == "theater" else None)(viewer()))
    assert opened["image"]["width"] >= 1200, "large playback was tested in a small viewer"
    native.keys("k")
    wait_for(lambda: viewer().get("playback", {}).get("frame"))
    # No input or control polling while it plays: a short first-frame check
    # missed the encoder exhausting its address space a few frames later.
    time.sleep(2.5)
    ended = wait_for(lambda: (lambda p: p if p["state"] == "stopped" and p["position_ms"] == 1958
                             else None)(viewer()["playback"]))
    app.screenshot(out / "large-video-ended.png")
    (out / "large-video-ended.json").write_text(json.dumps(ended, indent=2) + "\n")
    native.keys("Escape")
    wait_for(lambda: viewer() is None)


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
            environment = {"SHELL": "/bin/sh", "ENV": None, "BASH_ENV": None,
                           "WAYLAND_DISPLAY": None, "WAYLAND_SOCKET": None}
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
                if case == "all":
                    check_control_mouse(app)
                    result["checks"].append("control mouse opens the preview menu and dismisses the viewer without terminal input")
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
                if case == "all":
                    app.json_ctl("perform_action", {"action": "open_media_shelf"})
                    wait_for(lambda: not geometry()["preview_lanes"])
                    assets = media.card_fixtures(root / "assets")
                    check_pages(app, native, binary, assets, out)
                    check_video(app, native, binary, assets["video"], out)
                    check_large_video(app, native, binary, out)
                    result["checks"].append("video fits window; native picture click and K toggle; paused seek; theater toggle; no terminal input")
                    result["checks"].append("1920x1080 video reaches its final frame without further input or polling")
                    result["checks"].append("PDF distinct pages, Home/End and first/last boundaries; Markdown gallery paging")
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
