#!/usr/bin/env python3
"""Exercise modifier filtering through live modal handlers on macOS."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time


spec = importlib.util.spec_from_file_location(
    "kettle_live_ui", Path(__file__).with_name("check-live-ui-smoke.py")
)
live_ui = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = live_ui
spec.loader.exec_module(live_ui)


class RecordedLive(live_ui.LiveKettle):
    def _finish_startup(self):
        print(json.dumps({"pid": self.pid, "pgid": os.getpgid(self.pid)}), flush=True)
        return super()._finish_startup()


def frontmost_pid():
    result = live_ui.run([
        "osascript", "-e",
        'tell application "System Events" to get unix id of first process whose frontmost is true',
    ], timeout=10)
    result.check_returncode()
    return int(result.stdout.strip())


def focus(live):
    live.json_ctl("perform_action", {"action": "focus_window"})
    result = live_ui.run([
        "osascript", "-e",
        'tell application "System Events" to set frontmost of '
        f'(first process whose unix id is {live.pid}) to true',
    ], timeout=10)
    result.check_returncode()
    deadline = time.monotonic() + 5
    while True:
        current = frontmost_pid()
        focused = live.json_ctl("ui_geometry").get("window_focused")
        if current == live.pid and focused is True:
            return
        if time.monotonic() >= deadline:
            (live.log.parent / "focus-failure.json").write_text(json.dumps({
                "expected_pid": live.pid, "frontmost_pid": current,
                "window_focused": focused,
            }, indent=2) + "\n")
            raise RuntimeError("fixture Kettle did not become frontmost")
        time.sleep(0.05)


def key(live, code, flags):
    focus(live)
    if frontmost_pid() != live.pid:
        raise RuntimeError("fixture Kettle lost focus before native input")
    live_ui.macos_chord(code, flags, "modal-shortcuts smoke")
    time.sleep(0.2)


def screenshot(live, path):
    focus(live)
    live.screenshot(path)


def check(kettle, out):
    out.mkdir(parents=True, exist_ok=False)
    config = out / "config"
    config.write_text(
        "agent-server = full\nrestore-session = false\nupdate-policy = off\n"
        "login-shell = false\nshell-integration = false\nshell = /bin/sh\n"
        "status-bar = off\ncursor-blink = false\nbackground-opacity = 1.0\n"
        "window-blur = false\nvim-menu-nav = true\n"
    )
    results = {}
    hint_path = "/tmp/kettle-modal-selection-fixture"
    command = 1 << 20
    try:
        with RecordedLive(kettle, config, out / "kettle.log", extra_env={"SHELL": "/bin/sh"}) as live:
            focus(live)
            live_ui.live_shell_command(
                live,
                f"printf '{hint_path}\nMODAL_READY\n'",
                "MODAL_READY",
            )
            live.json_ctl("perform_action", {"action": "hint_mode"})
            assert live_ui.modal_open(live.json_ctl("ui_geometry"), "hint_mode")
            live.json_ctl("dispatch_ui_key", {"keys": ["super+a"]})
            results["hint_command_keeps_mode"] = live_ui.modal_open(
                live.json_ctl("ui_geometry"), "hint_mode"
            )
            if not results["hint_command_keeps_mode"]:
                live.json_ctl("perform_action", {"action": "hint_mode"})
            subprocess.run(["/usr/bin/pbcopy"], input=b"", timeout=10, check=True)
            live.json_ctl("dispatch_ui_key", {"keys": ["a"]})
            copied = live_ui.run(["/usr/bin/pbpaste"], timeout=10)
            copied.check_returncode()
            results["ordinary_hint_still_selects"] = (
                not live_ui.modal_open(live.json_ctl("ui_geometry"), "hint_mode")
                and copied.stdout == hint_path
            )

            focus(live)
            live.json_ctl("perform_action", {"action": "toggle_vi_mode"})
            screenshot(live, out / "vi-before.png")
            hidden = live_ui.run([
                "osascript", "-e",
                'tell application "System Events" to set visible of '
                f'(first process whose unix id is {live.pid}) to false',
            ], timeout=10)
            hidden.check_returncode()
            key(live, 9, command)  # V
            screenshot(live, out / "vi-command.png")
            results["vi_command_keeps_pixels"] = (
                (out / "vi-before.png").read_bytes() == (out / "vi-command.png").read_bytes()
            )
            key(live, 9, 0)
            screenshot(live, out / "vi-ordinary.png")
            results["ordinary_vi_still_selects"] = (
                (out / "vi-command.png").read_bytes() != (out / "vi-ordinary.png").read_bytes()
            )
            live.json_ctl("perform_action", {"action": "toggle_vi_mode"})

            focus(live)
            geometry = live.json_ctl("ui_geometry")
            x, y = live_ui.rect_center(geometry["tab_bar"]["new_tab_menu"])
            open_menu = {"event": "click", "x": x, "y": y, "button": "left"}
            live.json_ctl("send_mouse", open_menu)
            before = live.json_ctl("ui_geometry")["context_menu"]
            assert before
            key(live, 1, command)  # S, the sh mnemonic.
            after = live.json_ctl("ui_geometry").get("context_menu")
            results["menu_command_keeps_state"] = before == after
            if not after:
                live.json_ctl("send_mouse", open_menu)
            key(live, 1, 0)
            results["ordinary_menu_still_dispatches"] = (
                live.json_ctl("ui_geometry").get("context_menu") is None
            )
        print(json.dumps({"results": results, "exit": live.proc.returncode}), flush=True)
        (out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        if not all(results.values()):
            raise SystemExit("modal-shortcuts smoke: regression detected")
    finally:
        subprocess.run(["/usr/bin/pbcopy"], input=b"", timeout=10, check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--out-dir", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise SystemExit("modal-shortcuts smoke requires a native macOS desktop")
    check(str(Path(args.kettle).resolve()), args.out_dir.resolve())
