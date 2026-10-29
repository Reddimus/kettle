#!/usr/bin/env python3
"""Exercise confirmation dialogs with native input in an isolated Linux X server."""

import argparse
import importlib.util
import json
import os
import re
import shlex
import sys
import tempfile
import time
from pathlib import Path

from native_x11 import X11Input


def load_live():
    spec = importlib.util.spec_from_file_location(
        "confirmation_live", Path(__file__).with_name("check-live-ui-smoke.py")
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def wait_for(probe, timeout=10):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = probe()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError("confirmation condition timed out")


def assert_visible_dialog(live, path, geometry):
    width, height, rows = live.read_rgba_png(path)
    dialog = geometry["confirmation"]
    panel = dialog["rect"]
    sample_x, sample_y = int(panel["x"] + 4), int(panel["y"] + 4)
    background = rows[sample_y][sample_x * 4:sample_x * 4 + 3]

    def foreground(rect, inset=0, backdrop=background):
        count = 0
        for y in range(max(0, int(rect["y"] + inset)), min(height, int(rect["y"] + rect["height"] - inset))):
            for x in range(max(0, int(rect["x"] + inset)), min(width, int(rect["x"] + rect["width"] - inset))):
                pixel = rows[y][x * 4:x * 4 + 3]
                count += sum(abs(pixel[c] - backdrop[c]) for c in range(3)) >= 96
        return count

    prompt = dict(dialog["heading_rect"])
    prompt["height"] = geometry["cell"]["height"]
    assert foreground(prompt) > 50, "confirmation prompt has no visible text"
    for button in dialog["buttons"]:
        rect = button["rect"]
        bx, by = int(rect["x"] + 8), int(rect["y"] + 8)
        fill = rows[by][bx * 4:bx * 4 + 3]
        assert foreground(rect, inset=8, backdrop=fill) > 20, "confirmation button has no visible label"


def visibility_check_self_test():
    width, height = 300, 160
    rows = [bytearray([16, 16, 16, 255] * width) for _ in range(height)]

    def paint(x, y, w, h, color):
        for row in rows[y:y + h]:
            row[x * 4:(x + w) * 4] = bytes([*color, 255]) * w

    def rect(x, y, w, h):
        return dict(x=x, y=y, width=w, height=h)

    class Frame:
        @staticmethod
        def read_rgba_png(_path):
            return width, height, rows

    geometry = {"cell": {"height": 20}, "confirmation": {
        "rect": rect(0, 0, width, height), "heading_rect": rect(20, 20, 250, 20),
        "buttons": [{"rect": rect(20, 90, 110, 40)}, {"rect": rect(160, 90, 110, 40)}]}}
    paint(30, 25, 80, 2, (240, 240, 240))
    for x in (20, 160):
        paint(x, 90, 110, 40, (120, 120, 120))
        paint(x + 20, 104, 70, 2, (240, 240, 240))
    assert_visible_dialog(Frame, None, geometry)
    paint(160, 90, 110, 40, (120, 120, 120))
    try:
        assert_visible_dialog(Frame, None, geometry)
    except AssertionError as error:
        assert "button has no visible label" in str(error)
    else:
        raise AssertionError("visibility check accepted an unlabeled filled button")


def check(binary, out):
    live = load_live()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    result = {"pass": False, "checks": []}
    try:
        with tempfile.TemporaryDirectory(prefix="kettle-confirmation-") as scratch:
            root = Path(scratch)
            config = root / "kettle.conf"
            config.write_text(
                "language = en\nrestore-session = false\nupdate-policy = off\nrecord = off\n"
                "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
                "agent-server = full\nask-before-closing = always\n"
                "window-width = 100\nwindow-height = 32\ncursor-blink = false\n"
            )
            environment = {"SHELL": "/bin/sh", "ENV": None, "BASH_ENV": None,
                           "WAYLAND_DISPLAY": None, "WAYLAND_SOCKET": None}
            for key in ("CONFIG", "STATE", "DATA", "CACHE", "RUNTIME"):
                path = root / key.lower()
                path.mkdir(mode=0o700)
                environment[f"XDG_{key}_HOME" if key != "RUNTIME" else "XDG_RUNTIME_DIR"] = str(path)
            with (
                live.LiveKettle(binary, config, out / "kettle.log", extra_env=environment) as app,
                X11Input() as native,
            ):
                geometry = lambda: app.json_ctl("ui_geometry")
                panes = lambda: len(app.json_ctl("list_panes")["panes"])
                window = wait_for(lambda: native.windows(app.pid))[-1]
                native.focus(window)
                app.json_ctl("perform_action", {"action": "split_vert"})
                wait_for(lambda: panes() == 2)

                def open_dialog(action="close_window"):
                    app.json_ctl("perform_action", {"action": action})
                    g = wait_for(lambda: (lambda g: g if g.get("confirmation") else None)(geometry()))
                    dialog, surface = g["confirmation"], g["surface"]
                    rect = dialog["rect"]
                    for axis, size in (("x", "width"), ("y", "height")):
                        assert rect[axis] > 0
                        assert abs(rect[axis] + rect[size] / 2 - surface[size] / 2) < 0.1
                    assert dialog["buttons"][0]["focused"]
                    assert not dialog["buttons"][1]["focused"]
                    assert g["accessibility"]["focused"]["label"] == dialog["buttons"][0]["label"]
                    return g

                app.json_ctl("send_text", {"text": r"printf 'modal-tail-probe\n'"})
                wait_for(lambda: "modal-tail-probe" in app.json_ctl("read_screen")["text"])
                opened = open_dialog()
                app.screenshot(out / "window-confirmation.png")
                assert_visible_dialog(live, out / "window-confirmation.png", opened)
                before = app.json_ctl("read_screen")
                native.move(window, 1, 1)
                native.button(1, True)
                native.button(1, False)
                native.keys("x")
                time.sleep(0.1)
                assert geometry()["confirmation"], "outside click dismissed the decision"
                assert app.json_ctl("read_screen")["text"] == before["text"], "modal typing leaked to shell"
                native.key("Return", True)
                try:
                    wait_for(lambda: geometry()["confirmation"] is None)
                    time.sleep(0.75)
                finally:
                    native.key("Return", False)
                assert app.json_ctl("read_screen")["text"] == before["text"], "resolving Enter repeats leaked to the shell"
                assert panes() == 2, "Enter on initial Cancel closed panes"
                result["checks"].append("centered two-pane window confirmation; modal pointer/keyboard isolation; safe Enter")

                open_dialog()
                native.keys("Tab")
                wait_for(lambda: geometry()["confirmation"]["buttons"][1]["focused"])
                native.keys("Shift_L", "Tab")
                wait_for(lambda: geometry()["confirmation"]["buttons"][0]["focused"])
                native.keys("Right")
                wait_for(lambda: geometry()["confirmation"]["buttons"][1]["focused"])
                native.keys("Escape")
                wait_for(lambda: geometry()["confirmation"] is None)
                assert panes() == 2
                result["checks"].append("native Tab, Shift+Tab, Right and Escape match visible focus")

                pressed = open_dialog()
                close = pressed["confirmation"]["buttons"][1]["rect"]
                cancel = pressed["confirmation"]["buttons"][0]["rect"]
                native.move(window, int(close["x"] + close["width"] / 2), int(close["y"] + close["height"] / 2))
                native.button(1, True)
                wait_for(lambda: geometry()["confirmation"]["buttons"][1]["pressed"])
                assert panes() == 2, "button activated on press"
                app.screenshot(out / "close-pressed.png")
                native.move(window, 1, 1)
                native.button(1, False)
                assert geometry()["confirmation"] and panes() == 2, "dragging off activated Close"
                native.move(window, int(cancel["x"] + cancel["width"] / 2), int(cancel["y"] + cancel["height"] / 2))
                native.button(1, True)
                native.move(window, int(close["x"] + close["width"] / 2), int(close["y"] + close["height"] / 2))
                wait_for(lambda: geometry()["confirmation"]["buttons"][1]["hovered"])
                assert geometry()["confirmation"]["buttons"][0]["focused"], "hover stole keyboard focus"
                native.button(1, False)
                assert geometry()["confirmation"] and panes() == 2, "release over a different button activated it"
                native.keys("Escape")
                wait_for(lambda: geometry()["confirmation"] is None)
                result["checks"].append("press feedback, drag-off and cross-button cancellation; hover preserves keyboard focus")

                opened = open_dialog()
                original = opened["surface"]
                app.json_ctl("resize_window", {"width": 400, "height": 160})
                wait_for(lambda: (lambda g: g if g["surface"]["height"] == 160 and g["confirmation"]["max_scroll"] > 0 else None)(geometry()))
                native.keys("Home")
                wait_for(lambda: geometry()["confirmation"]["scroll_offset"] == 0)
                native.keys("End")
                wait_for(lambda: geometry()["confirmation"]["scroll_offset"] > 0)
                native.keys("Home")
                native.key("space", True)
                try:
                    wait_for(lambda: geometry()["confirmation"]["buttons"][0]["pressed"])
                    assert panes() == 2, "Space activated before release"
                finally:
                    native.key("space", False)
                wait_for(lambda: geometry()["confirmation"] is None)
                assert panes() == 2
                app.json_ctl("resize_window", {"width": original["width"], "height": original["height"]})
                wait_for(lambda: geometry()["surface"] == original)
                result["checks"].append("short-window message scrolling and native Space activation on release")

                config_before = config.read_text()
                app.json_ctl("perform_action", {"action": "open_settings"})
                wait_for(lambda: geometry()["modals"]["settings"])
                for _ in range(20):
                    state = geometry()
                    if state["accessibility"]["focused"]["label"] == "Split right":
                        break
                    category = state["settings"]["active_category"]
                    native.keys("Tab")
                    wait_for(lambda: geometry()["settings"]["active_category"] != category)
                else:
                    raise AssertionError("Settings keybind category was not reachable")
                native.keys("Return")
                native.keys("Control_L", "Shift_L", "t")
                settings_dialog = wait_for(lambda: (lambda g: g if g.get("confirmation") else None)(geometry()))
                assert settings_dialog["modals"]["settings"]
                assert settings_dialog["accessibility"]["modal"] == "confirm_dialog"
                assert settings_dialog["confirmation"]["buttons"][0]["focused"]
                app.screenshot(out / "settings-confirmation.png")
                assert_visible_dialog(live, out / "settings-confirmation.png", settings_dialog)
                native.keys("Escape")
                wait_for(lambda: geometry()["confirmation"] is None)
                assert geometry()["modals"]["settings"], "Cancel lost the underlying Settings state"
                assert config.read_text() == config_before, "Cancel persisted a keybind change"
                native.keys("Escape")
                wait_for(lambda: not geometry()["modals"]["settings"])
                result["checks"].append("visible keybind confirmation above Settings; Cancel restores Settings without rebinding")

                opened = open_dialog()
                rect = opened["confirmation"]["buttons"][0]["rect"]
                native.move(window, int(rect["x"] + rect["width"] / 2), int(rect["y"] + rect["height"] / 2))
                native.button(1, True)
                native.button(1, False)
                wait_for(lambda: geometry()["confirmation"] is None)
                assert panes() == 2
                result["checks"].append("native click on visible Cancel preserves both panes")

                recorder = root / "record_keys.py"
                key_log, stop = root / "keys.bin", root / "stop"
                recorder.write_text(r'''
import os, select, sys, termios, time, tty
from pathlib import Path
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
deadline = time.monotonic() + 30
try:
    with open(sys.argv[1], "wb", buffering=0) as log:
        os.write(1, b"\x1b[>11u\x1b[?1000h\x1b[?1006hCONFIRMATION-KEY-RECORDER\r\n")
        total = 0
        while time.monotonic() < deadline and not Path(sys.argv[2]).exists() and total < 4096:
            if select.select([fd], [], [], 0.05)[0]:
                data = os.read(fd, min(64, 4096 - total))
                if not data:
                    break
                log.write(data)
                total += len(data)
finally:
    os.write(1, b"\x1b[?1006l\x1b[?1000l\x1b[<uCONFIRMATION-KEY-RECORDER-DONE\r\n")
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
''')
                native.keys("Control_L", "u")
                command = " ".join(shlex.quote(str(p)) for p in (sys.executable, recorder, key_log, stop))
                app.json_ctl("send_text", {"text": command + "\r"})
                wait_for(lambda: "CONFIRMATION-KEY-RECORDER" in app.json_ctl("read_screen")["text"])
                native.key("space", True)
                try:
                    wait_for(lambda: key_log.exists() and b"\x1b[32u" in key_log.read_bytes())
                    open_dialog()
                finally:
                    native.key("space", False)
                try:
                    wait_for(lambda: b"\x1b[32;1:3u" in key_log.read_bytes())
                    assert geometry()["confirmation"], "terminal-owned Space release resolved the dialog"
                    native.keys("Escape")
                    wait_for(lambda: geometry()["confirmation"] is None)
                    for physical, sgr in ((1, 0), (2, 1), (3, 2), (8, 128), (9, 129)):
                        start = len(key_log.read_bytes())
                        native.move(window, int(original["width"] * 0.75), 80)
                        native.button(physical, True)
                        reports = re.compile(rb"\x1b\[<" + str(sgr).encode() + rb";[0-9]+;[0-9]+([Mm])")
                        try:
                            wait_for(lambda: reports.findall(key_log.read_bytes()[start:]) == [b"M"])
                            open_dialog()
                            wait_for(lambda: reports.findall(key_log.read_bytes()[start:]) == [b"M", b"m"])
                        finally:
                            native.button(physical, False)
                        native.keys("Escape")
                        wait_for(lambda: geometry()["confirmation"] is None)
                        time.sleep(0.05)
                        assert reports.findall(key_log.read_bytes()[start:]) == [b"M", b"m"], "mouse release was duplicated"
                    result["checks"].append("opening a dialog ends all five terminal mouse-button gestures exactly once")
                finally:
                    stop.touch()
                wait_for(lambda: "CONFIRMATION-KEY-RECORDER-DONE" in app.json_ctl("read_screen")["text"])
                result["checks"].append("Space held before the dialog retains its Kitty terminal key release")

                open_dialog("close_pane")
                native.keys("Tab")
                wait_for(lambda: geometry()["confirmation"]["buttons"][1]["focused"])
                native.keys("Return")
                wait_for(lambda: panes() == 1)
                assert geometry()["confirmation"] is None
                result["checks"].append("explicit focused Close closes one pane")
                result["pass"] = True
    finally:
        (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle")
    parser.add_argument("--out")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    visibility_check_self_test()
    if args.self_test:
        print("confirmation visibility checker self-test passed")
        return
    if not args.kettle or not args.out:
        parser.error("--kettle and --out are required for native checks")
    if sys.platform != "linux" or not os.environ.get("DISPLAY"):
        parser.error("run in an isolated Linux X server, such as xvfb-run")
    os.umask(0o077)
    check(str(Path(args.kettle).resolve()), Path(args.out).resolve())


if __name__ == "__main__":
    main()
