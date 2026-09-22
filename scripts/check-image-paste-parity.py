#!/usr/bin/env python3
"""Exercise native paste shortcuts and clipboard fallback in an isolated window.

This explicit smoke replaces the desktop clipboard. Use Xvfb on Linux or a
hosted macOS runner; do not run against an unrelated user's active clipboard.
"""

import argparse
import ctypes
import importlib.util
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path


def wait_for(check, label, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise RuntimeError(f"timed out: {label}")


@contextmanager
def failure_evidence(live, out, prefix):
    try:
        yield
    except BaseException:
        for method in ("read_screen", "ui_geometry"):
            try:
                result = live.ctl(method, raw=True, allow_fail=True, timeout=2)
                (out / f"{prefix}-{method}.json").write_text(result.stdout)
            except (OSError, subprocess.TimeoutExpired, SystemExit):
                pass
        raise


def native_keys(live, names, *, middle=False):
    if platform.system() == "Darwin":
        cg = ctypes.CDLL(
            "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"
        )
        cf = ctypes.CDLL(
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
        )
        cg.CGEventCreateKeyboardEvent.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint16,
            ctypes.c_bool,
        ]
        cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
        cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
        cf.CFRelease.argtypes = [ctypes.c_void_p]
        codes = {
            "control": (59, 1 << 18),
            "shift": (56, 1 << 17),
            "command": (55, 1 << 20),
            "v": (9, 0),
        }
        flags = 0
        for down, sequence in ((True, names), (False, list(reversed(names)))):
            for name in sequence:
                code, mask = codes[name]
                flags = flags | mask if down else flags & ~mask
                event = cg.CGEventCreateKeyboardEvent(None, code, down)
                if not event:
                    raise RuntimeError("could not create native key event")
                try:
                    cg.CGEventSetFlags(event, flags)
                    cg.CGEventPost(0, event)
                finally:
                    cf.CFRelease(event)
        return
    x = ctypes.CDLL("libX11.so.6")
    xt = ctypes.CDLL("libXtst.so.6")
    x.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x.XOpenDisplay.restype = ctypes.c_void_p
    x.XStringToKeysym.argtypes = [ctypes.c_char_p]
    x.XStringToKeysym.restype = ctypes.c_ulong
    x.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    x.XKeysymToKeycode.restype = ctypes.c_ubyte
    x.XSetInputFocus.argtypes = [
        ctypes.c_void_p,
        ctypes.c_ulong,
        ctypes.c_int,
        ctypes.c_ulong,
    ]
    x.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    x.XCloseDisplay.argtypes = [ctypes.c_void_p]
    xt.XTestFakeKeyEvent.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint,
        ctypes.c_int,
        ctypes.c_ulong,
    ]
    display = x.XOpenDisplay(None)
    if not display:
        raise RuntimeError("native Linux keys require an X11 display")
    try:
        # Match the PID, not a title that another application could duplicate.
        tree = subprocess.check_output(
            ["xwininfo", "-root", "-tree"], text=True, timeout=3
        )
        windows = re.findall(r"^\s+(0x[0-9a-fA-F]+)", tree, re.MULTILINE)
        owned = []
        for window in windows:
            prop = subprocess.run(
                ["xprop", "-id", window, "_NET_WM_PID"],
                capture_output=True,
                check=False,
                text=True,
                timeout=3,
            )
            if re.search(rf"= {live.pid}\s*$", prop.stdout):
                owned.append(int(window, 16))
        if len(owned) != 1:
            raise RuntimeError(f"expected one owned native window, got {owned}")
        x.XSetInputFocus(display, owned[0], 1, 0)
        if middle:
            x.XWarpPointer.argtypes = [
                ctypes.c_void_p,
                ctypes.c_ulong,
                ctypes.c_ulong,
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_uint,
                ctypes.c_uint,
                ctypes.c_int,
                ctypes.c_int,
            ]
            xt.XTestFakeButtonEvent.argtypes = [
                ctypes.c_void_p,
                ctypes.c_uint,
                ctypes.c_int,
                ctypes.c_ulong,
            ]
            x.XWarpPointer(display, 0, owned[0], 0, 0, 0, 0, 100, 100)
            xt.XTestFakeButtonEvent(display, 2, 1, 0)
            xt.XTestFakeButtonEvent(display, 2, 0, 0)
            x.XSync(display, 0)
            return
        symbols = {"control": "Control_L", "shift": "Shift_L", "v": "v"}
        codes = [
            x.XKeysymToKeycode(display, x.XStringToKeysym(symbols[name].encode()))
            for name in names
        ]
        for down, sequence in ((1, codes), (0, list(reversed(codes)))):
            for code in sequence:
                if not code:
                    raise RuntimeError("native key has no X11 keycode")
                xt.XTestFakeKeyEvent(display, code, down, 0)
        x.XSync(display, 0)
    finally:
        x.XCloseDisplay(display)


def check_codex(helpers, kettle, config, image, out, codex):
    home = out / "codex-home"
    workspace = out / "workspace"
    home.mkdir(mode=0o700)
    workspace.mkdir(mode=0o700)
    # Only this newly created empty directory is trusted. No account, user
    # configuration, MCP server, model request, or prompt submission is needed.
    (home / "config.toml").write_text(
        "[projects."
        + json.dumps(str(workspace.resolve()))
        + ']\ntrust_level = "trusted"\n'
    )
    argv = [
        "-d",
        str(workspace),
        "-e",
        str(Path(codex).resolve()),
        "--sandbox",
        "read-only",
        "--ask-for-approval",
        "never",
        "-c",
        'model_provider="image-test"',
        "-c",
        'model_providers.image-test.name="Image test"',
        "-c",
        'model_providers.image-test.base_url="http://127.0.0.1:9/v1"',
        "-c",
        'model_providers.image-test.wire_api="responses"',
        "-c",
        "model_providers.image-test.requires_openai_auth=false",
        "--model",
        "gpt-6-astra",
    ]
    results = []
    with (
        helpers.LiveKettle(
            kettle,
            config,
            out / "codex-kettle.log",
            extra_args=argv,
            extra_env={
                "CODEX_HOME": str(home.resolve()),
                "OPENAI_API_KEY": None,
                "CODEX_API_KEY": None,
            },
        ) as live,
        failure_evidence(live, out, "codex-failure"),
    ):
        live.wait_for_text("Ask Codex to do anything", timeout_ms=20000)
        if platform.system() == "Darwin":
            helpers.focus_live_kettle_window(live)
        else:
            native_keys(live, [])
        wait_for(
            lambda: live.json_ctl("ui_geometry").get("window_focused"),
            "Codex window focus",
        )
        routes = [
            ("ctrl+shift+v", ["control", "shift", "v"], True),
            ("ctrl+v", ["control", "v"], False),
        ]
        if platform.system() == "Darwin":
            routes.append(("cmd+v", ["command", "v"], True))
        for number, (route, keys, thumbnail) in enumerate(routes, 1):
            owner = helpers.set_bitmap_clipboard(image)
            try:
                native_keys(live, keys)
                marker = f"[Image #{number}]"
                wait_for(
                    lambda marker=marker: (
                        marker in helpers.screen_text(live.json_ctl("read_screen"))
                    ),
                    route + " Codex attachment",
                )
                wait_for(
                    lambda thumbnail=thumbnail: (
                        bool(live.json_ctl("ui_geometry").get("image_paste_receipt"))
                        == thumbnail
                    ),
                    route + " thumbnail policy",
                )
                results.append(
                    {"route": route, "attachment": marker, "thumbnail": thumbnail}
                )
            finally:
                if owner is not None and owner.poll() is None:
                    owner.terminate()
                    owner.wait(timeout=3)
        version = subprocess.check_output(
            [codex, "--version"], text=True, timeout=5
        ).strip()
        (out / "codex-results.json").write_text(
            json.dumps(
                {"version": version, "results": results, "submitted": False}, indent=2
            )
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument(
        "--codex", help="Optional Codex 0.155.1 executable for offline composer checks"
    )
    args = parser.parse_args()
    if platform.system() not in ("Linux", "Darwin"):
        parser.error("this native smoke supports Linux and macOS")
    if platform.system() == "Linux" and os.environ.get("WAYLAND_DISPLAY"):
        parser.error("run in an isolated Xvfb display with WAYLAND_DISPLAY unset")
    out = args.out_dir or Path(tempfile.mkdtemp(prefix="kettle-image-paste-"))
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    spec = importlib.util.spec_from_file_location(
        "kettle_live", Path(__file__).with_name("check-live-ui-smoke.py")
    )
    helpers = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = helpers
    spec.loader.exec_module(helpers)
    config = out / "config"
    config.write_text(
        "agent-server = full\nrecord = off\nrestore-session = false\nupdate-check = false\npaste-images = on\npaste-image-preview = on\nwindow-width = 92\nwindow-height = 28\n"
    )
    image = out / "fixture.png"
    helpers.write_image_receipt_fixture(image)
    state = out / "input.hex"
    client = out / "client.py"
    client.write_text(
        "import os,sys,tty\nfrom pathlib import Path\ntty.setraw(0)\nos.write(1,b'\\x1b[>7u\\x1b[?2004hIMAGE_PASTE_READY')\ndata=b''\nwhile True:\n data+=os.read(0,4096)\n Path(sys.argv[1]).write_text(data.hex())\n"
    )
    results = []
    with (
        helpers.LiveKettle(
            str(Path(args.kettle).resolve()),
            config,
            out / "kettle.log",
            extra_args=["-e", sys.executable, str(client), str(state)],
        ) as live,
        failure_evidence(live, out, "fixture-failure"),
    ):
        live.wait_for_text("IMAGE_PASTE_READY", timeout_ms=12000)
        if platform.system() == "Darwin":
            helpers.focus_live_kettle_window(live)
        else:
            native_keys(live, [])
        wait_for(
            lambda: live.json_ctl("ui_geometry").get("window_focused"),
            "native window focus",
        )

        def read_input():
            return bytes.fromhex(state.read_text()) if state.exists() else b""

        routes = [
            "paste",
            "ctrl+shift+v",
            "context-menu",
            "paste_primary",
            "middle-click",
        ]
        if platform.system() == "Darwin":
            routes.append("cmd+v")
        for route in routes:
            live.ctl("send_text", params={"text": "clear"})
            wait_for(
                lambda: live.json_ctl("ui_geometry").get("image_paste_receipt") is None,
                "receipt dismissed before next paste",
            )
            before = len(read_input())
            owner = helpers.set_bitmap_clipboard(image)
            try:
                if route == "ctrl+shift+v":
                    native_keys(live, ["control", "shift", "v"])
                elif route == "cmd+v":
                    native_keys(live, ["command", "v"])
                elif route == "context-menu":
                    live.ctl(
                        "send_mouse",
                        params={
                            "event": "press",
                            "x": 100,
                            "y": 100,
                            "button": "right",
                        },
                    )
                    live.ctl(
                        "send_mouse",
                        params={
                            "event": "release",
                            "x": 100,
                            "y": 100,
                            "button": "right",
                        },
                    )
                    menu = wait_for(
                        lambda: live.json_ctl("ui_geometry").get("context_menu"),
                        "context menu",
                    )
                    row = next(row for row in menu["rows"] if row["label"] == "Paste")
                    rect = row["rect"]
                    point = {
                        "x": rect["x"] + rect["width"] / 2,
                        "y": rect["y"] + rect["height"] / 2,
                        "button": "left",
                    }
                    live.ctl("send_mouse", params={"event": "press", **point})
                    live.ctl("send_mouse", params={"event": "release", **point})
                elif route == "middle-click":
                    if platform.system() == "Darwin":
                        x, y, _, _ = helpers.macos_window_frame(live.pid)
                        helpers.macos_mouse_event(25, x + 100, y + 100, button=2)
                        helpers.macos_mouse_event(26, x + 100, y + 100, button=2)
                    else:
                        native_keys(live, [], middle=True)
                else:
                    live.ctl("perform_action", params={"action": route})
                receipt = wait_for(
                    lambda: live.json_ctl("ui_geometry").get("image_paste_receipt"),
                    route + " thumbnail",
                )
                if (receipt.get("original_width"), receipt.get("original_height")) != (
                    640,
                    360,
                ):
                    raise RuntimeError(f"{route}: wrong image dimensions")
                wait_for(
                    lambda before=before: b"kettle-paste-" in read_input()[before:],
                    route + " managed path",
                )
                # Key releases must not immediately dismiss the new receipt.
                time.sleep(0.2)
                if live.json_ctl("ui_geometry").get("image_paste_receipt") is None:
                    raise RuntimeError(f"{route}: release dismissed thumbnail")
                results.append(
                    {"route": route, "thumbnail": True, "managed_path": True}
                )
                if route == "paste":
                    live.screenshot(out / "thumbnail.png")
            finally:
                if owner is not None and owner.poll() is None:
                    owner.terminate()
                    owner.wait(timeout=3)

        before = len(read_input())
        owner = helpers.set_file_list_clipboard([image])
        try:
            live.ctl("perform_action", params={"action": "paste_primary"})
            wait_for(
                lambda: str(image).encode() in read_input()[before:],
                "copied-image path fallback",
            )
            if live.json_ctl("ui_geometry").get("image_paste_receipt") is not None:
                raise RuntimeError("arbitrary file path created a bitmap receipt")
            results.append(
                {"route": "paste_primary", "copied_file": True, "thumbnail": False}
            )
        finally:
            if owner is not None and owner.poll() is None:
                owner.terminate()
                owner.wait(timeout=3)

        if platform.system() == "Linux":
            # A genuine PRIMARY selection must win over a bitmap in CLIPBOARD.
            selection = out / "selection.txt"
            selection.write_text("PRIMARY_SENTINEL")
            primary = subprocess.Popen(
                ["xclip", "-quiet", "-selection", "primary", "-in", str(selection)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            owner = helpers.set_bitmap_clipboard(image)
            try:
                before = len(read_input())
                live.ctl("perform_action", params={"action": "paste_primary"})
                wait_for(
                    lambda: b"PRIMARY_SENTINEL" in read_input()[before:],
                    "PRIMARY text priority",
                )
                if b"kettle-paste-" in read_input()[before:]:
                    raise RuntimeError("bitmap fallback replaced selected text")
                results.append(
                    {"route": "paste_primary", "selected_text_precedes_bitmap": True}
                )
            finally:
                primary.terminate()
                primary.wait(timeout=3)
                if owner is not None and owner.poll() is None:
                    owner.terminate()
                    owner.wait(timeout=3)

        before = len(read_input())
        native_keys(live, ["control", "v"])
        wait_for(
            lambda: b"\x1b[118;5u" in read_input()[before:],
            "Ctrl+V reaches the client's image handler",
        )
        wait_for(
            lambda: live.json_ctl("ui_geometry").get("image_paste_receipt") is None,
            "client-owned input dismisses stale receipt",
        )
        results.append({"route": "ctrl+v", "client_key": True, "thumbnail": False})
        geometry = live.json_ctl("ui_geometry")
        (out / "results.json").write_text(
            json.dumps(
                {
                    "platform": platform.platform(),
                    "surface": geometry.get("surface"),
                    "grid": {
                        key: live.json_ctl("read_screen").get(key)
                        for key in ("cols", "rows")
                    },
                    "results": results,
                },
                indent=2,
            )
        )
    if args.codex:
        check_codex(
            helpers, str(Path(args.kettle).resolve()), config, image, out, args.codex
        )
    print(f"image-paste parity: OK artifacts={out}")


if __name__ == "__main__":
    main()
