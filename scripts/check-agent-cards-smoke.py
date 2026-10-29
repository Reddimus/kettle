#!/usr/bin/env python3
"""Exercise Linux launch scopes and rendered cards with a simulated native agent."""

import argparse
import importlib.util
import json
import os
import platform
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from native_x11 import X11Input

SCRIPTS = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


live = load("card_live", SCRIPTS / "check-live-ui-smoke.py")
media = load("card_media", SCRIPTS / "check-lane-media-smoke.py")


def wait_for(probe, timeout=20):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = probe()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError("card fixture condition timed out")


def npm_fixture(root, agent, node):
    architecture = {"aarch64": "arm64", "x86_64": "x64"}[platform.machine()]
    triple = f"{platform.machine()}-unknown-linux-musl"
    package = root / "npm/node_modules/@openai/codex"
    wrapper = package / "bin/codex.js"
    wrapper.parent.mkdir(parents=True)
    native = package.parent / f"codex-linux-{architecture}"
    program = native / "vendor" / triple / "bin/codex"
    program.parent.mkdir(parents=True)
    shutil.copy2(agent, program)
    # npm installs the native package through an alias, so its own
    # package.json names @openai/codex at the platform version.
    for directory, version in ((package, "0.163.0"), (native, f"0.163.0-linux-{architecture}")):
        metadata = directory / "package.json"
        metadata.write_text(json.dumps({"name": "@openai/codex", "version": version}))
        metadata.chmod(0o600)
    child = f"../../codex-linux-{architecture}/vendor/{triple}/bin/codex"
    wrapper.write_text(
        '#!/usr/bin/env node\nconst {spawnSync}=require("node:child_process");\n'
        'const path=require("node:path");\n'
        f'const child=spawnSync(path.join(__dirname,{json.dumps(child)}),'
        'process.argv.slice(2),{stdio:"inherit"});\nprocess.exit(child.status ?? 1);\n'
    )
    wrapper.chmod(0o700)
    binary = root / "npm/bin"
    binary.mkdir()
    (binary / "codex").symlink_to(wrapper)
    # Hosted toolcache runtimes may belong to the image builder or be group
    # writable. Test the accepted private-install route without changing them.
    private_node = binary / "node"
    shutil.copyfile(node, private_node)
    private_node.chmod(0o700)
    return binary


def check_preview_menu(app, native, destination, kind, asset):
    """Use native input and the private X11 clipboard after opening a real card."""
    geometry = lambda: app.json_ctl("ui_geometry")
    viewer = lambda: geometry().get("preview_overlay")
    window = wait_for(lambda: native.windows(app.pid))[-1]
    terminal = app.json_ctl("read_screen")["text"]
    checked = []

    def click(rect, button=1):
        native.move(window, int(rect["x"] + rect["width"] / 2),
                    int(rect["y"] + rect["height"] / 2))
        native.button(button, True)
        native.button(button, False)

    def open_menu():
        panel = viewer() or geometry()["preview_lanes"][0]
        click(panel["rect"] if panel["state"] == "strip" else
              (panel.get("image") or panel["content"]), button=3)
        menu = wait_for(lambda: geometry().get("context_menu"))
        assert viewer() or geometry()["preview_lanes"], "menu closed its preview"
        return {row["label"] for row in menu["rows"] if row["dispatchable"]}

    def choose(label):
        row = live.visible_context_row(live.reveal_context_row(app, label), label)
        click(row["rect"])
        wait_for(lambda: not geometry().get("context_menu"))

    def copy(label, name, text=None):
        owner = live.set_text_clipboard("KETTLE_PREVIEW_MENU_SENTINEL")
        try:
            open_menu()
            choose(label)

            def read():
                result = subprocess.run(
                    ["xclip", "-selection", "clipboard", "-out", "-target",
                     "UTF8_STRING" if text is not None else "image/png"],
                    capture_output=True, timeout=3, check=False,
                )
                if result.returncode:
                    return None
                if text is not None:
                    return result.stdout if result.stdout == text.encode() else None
                return result.stdout if result.stdout.startswith(b"\x89PNG\r\n\x1a\n") else None

            data = wait_for(read)
            if text is None:
                width, height = struct.unpack(">II", data[16:24])
                assert width > 0 and height > 0
                (destination / name).write_bytes(data)
                if kind == "png":
                    assert (width, height) == (160, 100)
                    _, _, rows = live.read_rgba_png(destination / name)
                    expected = [b"".join(bytes(((x * 4) % 256, (y * 5) % 256, 160, 255))
                                         for x in range(width)) for y in range(height)]
                    assert rows == expected, "image copy changed the fixture's pixels"
            assert viewer() or geometry()["preview_lanes"], "copy closed its preview"
            checked.append(label)
            return data
        finally:
            if owner is not None:
                if owner.poll() is None:
                    owner.terminate()
                owner.wait(timeout=5)

    labels = open_menu()
    (destination / "preview-menu.json").write_text(json.dumps(geometry(), indent=2) + "\n")
    app.screenshot(destination / "preview-menu.png")
    expected = "Copy frame" if kind == "video" else "Copy page as image" if kind == "pdf" else "Copy image"
    if kind == "file":
        assert not any(label.startswith("Copy ") for label in labels), labels
    else:
        assert expected in labels, labels
    native.keys("Escape")
    wait_for(lambda: not geometry().get("context_menu"))
    assert viewer(), "Escape closed the preview instead of its menu"
    native.keys("Shift_L", "F10")
    wait_for(lambda: geometry().get("context_menu"))
    native.keys("Escape")
    wait_for(lambda: not geometry().get("context_menu"))
    assert viewer()
    checked.append("right click; Shift+F10; Escape preserves preview")

    if kind != "file":
        first = copy(expected, "copied.png")
    if kind in ("svg", "mermaid"):
        copy("Copy source", "source.txt", text=asset.read_text())
        assert viewer()["mode"] == "rendered", "Copy source changed the viewing mode"
        open_menu()
        choose("Show source")
        wait_for(lambda: viewer()["mode"] == "source")
        assert "Copy image" not in open_menu()
        native.keys("Escape")
        wait_for(lambda: not geometry().get("context_menu"))
        copy("Copy source", "source.txt", text=asset.read_text())
        open_menu()
        choose("Show rendered")
        wait_for(lambda: viewer()["mode"] == "rendered")
    if kind == "pdf":
        assert "Previous page" not in open_menu()
        choose("Next page")
        def last_page_menu():
            labels = open_menu()
            if "Previous page" in labels and "Next page" not in labels:
                return True
            native.keys("Escape")
            wait_for(lambda: not geometry().get("context_menu"))
            return False

        wait_for(last_page_menu)
        assert "Next page" not in {row["label"] for row in geometry()["context_menu"]["rows"]}
        native.keys("Escape")
        wait_for(lambda: not geometry().get("context_menu"))
        assert first != copy(expected, "copied-page-2.png"), "page copy retained the first page"
        checked.append("document page bounds and distinct page pixels")
    if kind == "video":
        open_menu()
        choose("Play video")
        wait_for(lambda: viewer().get("playback", {}).get("state") == "playing")
        time.sleep(0.4)
        open_menu()
        choose("Pause video")
        wait_for(lambda: viewer().get("playback", {}).get("state") == "paused")
        paused = copy(expected, "copied-paused-frame.png")
        assert paused == copy(expected, "copied-paused-frame-again.png")
        checked.append("menu playback controls and stable paused frame")
    if kind == "png":
        open_menu()
        choose("Pin beside terminal")
        wait_for(lambda: not viewer() and geometry()["preview_lanes"])
        open_menu()
        choose("Collapse preview")
        wait_for(lambda: geometry()["preview_lanes"][0]["state"] == "strip")
        open_menu()
        choose("Expand preview")
        wait_for(lambda: geometry()["preview_lanes"][0]["state"] == "expanded")
        surface = geometry()["surface"]
        for control in ("menu", "button"):
            app.json_ctl("resize_window", {"width": 240, "height": 140})
            wait_for(lambda: geometry()["preview_lanes"][0]["state"] == "strip")
            if control == "menu":
                open_menu()
                choose("Expand preview")
            else:
                click(geometry()["preview_lanes"][0]["controls"]["toggle"])
            app.json_ctl("resize_window", surface)
            try:
                wait_for(lambda: geometry()["preview_lanes"][0]["state"] == "expanded")
            finally:
                (destination / f"expansion-after-resize-{control}.json").write_text(json.dumps(geometry(), indent=2) + "\n")
        assert "Copy image" in open_menu()
        choose("Close preview")
        wait_for(lambda: not geometry()["preview_lanes"])
        checked.append("pinned lane collapse/expand; menu and button expansion after constrained resize; close")
    if kind == "svg":
        open_menu()
        choose("Pin beside terminal")
        wait_for(lambda: not viewer() and geometry()["preview_lanes"])
        surface = geometry()["surface"]
        app.json_ctl("resize_window", {"width": 400, "height": surface["height"]})
        wait_for(lambda: geometry()["surface"]["width"] == 400 and
                 geometry()["preview_lanes"][0]["state"] == "expanded")
        lane = geometry()["preview_lanes"][0]
        assert lane["image"] and "copy" not in lane["controls"], lane
        wanted = {"Copy image", "Copy source", "Show source", "Zoom in", "Zoom out", "Fit preview"}
        wanted.update(label for label in labels if label.startswith("Open in "))
        assert wanted <= open_menu()
        (destination / "narrow-menu.json").write_text(json.dumps(geometry(), indent=2) + "\n")
        app.screenshot(destination / "narrow-menu.png")
        native.keys("Escape")
        wait_for(lambda: not geometry().get("context_menu"))
        copy("Copy image", "narrow-copied.png")
        copy("Copy source", "narrow-source.txt", text=asset.read_text())
        assert geometry()["preview_lanes"][0]["mode"] == "rendered"
        open_menu()
        choose("Zoom in")
        wait_for(lambda: geometry()["preview_lanes"][0]["zoom"] > 1)
        open_menu()
        choose("Fit preview")
        wait_for(lambda: geometry()["preview_lanes"][0]["zoom"] == 1)
        app.json_ctl("resize_window", surface)
        wait_for(lambda: geometry()["surface"] == surface)
        open_menu()
        choose("Close preview")
        wait_for(lambda: not geometry()["preview_lanes"])
        checked.append("narrow preview keeps copy, source, zoom, fit and external-viewer options")
    assert app.json_ctl("read_screen")["text"] == terminal, "preview menu typed into the terminal"
    return checked


def check_case(app, binary, root, out, name, target, scoped, path, asset, native, kind="svg", version=None):
    case = root / name
    case.mkdir(mode=0o700)
    meta = "callId" if target == "codex" else "claudecode/toolUseId"
    request = {
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "kettle_show", "arguments": {"path": str(asset)},
                   "_meta": {meta: "fixture-1"}},
    }
    environment = {
        "FIXTURE_AGENT": target, "FIXTURE_KETTLE": binary,
        "FIXTURE_CASE": str(case), "FIXTURE_SHOW_REQUEST": json.dumps(request),
        "CLAUDE_CODE_ENTRYPOINT": "cli", "PATH": str(path) + ":" + os.environ["PATH"],
    }
    if version:
        environment["FIXTURE_AGENT_VERSION"] = version
    command = ["env", *(key + "=" + value for key, value in environment.items())]
    command += [binary, "agent-setup", "--launch-" + target, "--"] if scoped else [str(root / "agent")]
    script = case / "run.sh"
    script.write_text("#!/bin/sh\nexec " + shlex.join(command) + "\n")
    # Only a short fixture path is echoed, keeping environment and RPC data out of the capture.
    app.json_ctl("send_text", {"text": shlex.join(["/bin/sh", str(script)]) + "\n"})
    try:
        wait_for(lambda: (case / "ready").exists())
        model = json.loads((case / "model.json").read_text())
        hook = json.loads((case / "hook.json").read_text())
        assert "permit" not in json.dumps(model) and "systemMessage" not in json.dumps(model)
        destination = out / name
        destination.mkdir()
        (destination / "model.json").write_text(json.dumps(model, indent=2) + "\n")
        assert model["result"]["structuredContent"]["status"] == "sent", model
        delivery = model["result"]["structuredContent"]["delivery"]
        assert delivery == ("card" if scoped else "shelf"), model
        receipt = model["result"]["structuredContent"]
        assert receipt["model_has_seen"] is False and receipt["verified"] is True
        assert receipt["kind"] == {"png": "raster", "jpg": "raster", "webp": "raster", "gif": "raster"}.get(kind, kind)
        menu_checks = []
        if scoped:
            wait_for(lambda: app.json_ctl("ui_geometry")["inline_cards"])
            wait_for(lambda: all(card["settled"] for card in app.json_ctl("ui_geometry")["inline_cards"]))
            image = destination / "rendered.png"
            app.screenshot(image)
            _, _, rows = live.read_rgba_png(image)
            geometry = app.json_ctl("ui_geometry")
            card = geometry["inline_cards"][-1]
            rect = card["rect"]
            left, top = int(rect["x"] + 12), int(rect["y"] + 12)
            right, bottom = int(rect["x"] + rect["width"] - 12), int(rect["y"] + rect["height"] / 2)
            colored = sum(1 for row in rows[top:bottom] for i in range(left * 4, right * 4, 4)
                          if max(row[i:i + 3]) - min(row[i:i + 3]) > 45 or
                          (kind == "file" and max(row[i:i + 3]) > 140))
            assert colored > 100, f"{kind} card pixels missing: {colored}"
            (destination / "geometry.json").write_text(json.dumps(geometry, indent=2) + "\n")
            window = wait_for(lambda: native.windows(app.pid))[-1]
            native.focus(window)
            native.move(window, int(rect["x"] + rect["width"] / 2), int(rect["y"] + rect["height"] / 2))
            native.button(1, True)
            native.button(1, False)
            wait_for(lambda: app.json_ctl("ui_geometry")["preview_overlay"])
            app.screenshot(destination / "opened.png")
            if name.startswith("native-codex-"):
                menu_checks = check_preview_menu(app, native, destination, kind, asset)
            if app.json_ctl("ui_geometry")["preview_overlay"]:
                native.keys("Escape")
            wait_for(lambda: app.json_ctl("ui_geometry")["preview_overlay"] is None)
        else:
            assert not app.json_ctl("ui_geometry")["inline_cards"]
            assert json.loads(hook["result"]["content"][0]["text"]) == {}
        pids = [int(value) for value in (case / "pids").read_text().split()]
        (case / "quit").write_text("quit\n")
        wait_for(lambda: all(not Path(f"/proc/{pid}").exists() for pid in pids))
        wait_for(lambda: not app.json_ctl("ui_geometry")["inline_cards"])
        return {"case": name, "delivery": delivery, "pids": pids,
                "processes_closed": True, "preview_menu": menu_checks}
    except BaseException:
        (out / "failure-screen.json").write_text(json.dumps(app.json_ctl("read_screen"), indent=2) + "\n")
        raise


def check(kettle, worker, node, out):
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    result = {"node_source_mode": oct(node.stat().st_mode & 0o7777), "node_source_uid": node.stat().st_uid, "pass": False, "scope": "simulated native agents; no official CLI or model acceptance", "cases": []}
    try:
        with tempfile.TemporaryDirectory(prefix="kettle-agent-cards-") as scratch:
            root = Path(scratch)
            binary = media.stage(Path(kettle), Path(worker), root / "kettle-bin")
            agent = root / "agent"
            subprocess.run(["rustc", "--edition=2024", "-O", str(SCRIPTS / "fixtures/agent-cards.rs"),
                            "-o", str(agent)], check=True, timeout=30)
            agent.chmod(0o700)
            direct = root / "bin"
            direct.mkdir(mode=0o700)
            for name in ("codex", "claude"):
                (direct / name).symlink_to(agent)
            npm = npm_fixture(root, agent, node)
            assets = media.card_fixtures(root / "assets")
            config = root / "kettle.conf"
            config.write_text(
                "language = en\nrestore-session = false\nupdate-policy = off\nrecord = off\n"
                "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
                "agent-server = full\nagent-display = true\nagent-display-claude-code = true\n"
                "preview-lane-side = right\n"
                "cursor-blink = false\nwindow-width = 140\nwindow-height = 45\nstatus-bar = off\n"
            )
            environment = {"SHELL": "/bin/sh", "ENV": None, "BASH_ENV": None,
                           "WAYLAND_DISPLAY": None, "WAYLAND_SOCKET": None}
            for key in ("CONFIG", "STATE", "DATA", "CACHE"):
                directory = root / key.lower()
                directory.mkdir(mode=0o700)
                environment[f"XDG_{key}_HOME"] = str(directory)
            if not os.environ.get("XDG_RUNTIME_DIR"):
                runtime = root / "runtime"
                runtime.mkdir(mode=0o700)
                environment["XDG_RUNTIME_DIR"] = str(runtime)
            with (live.LiveKettle(binary, config, out / "kettle.log", extra_env=environment) as app,
                  X11Input() as native):
                wait_for(lambda: app.json_ctl("get_state")["media"]["availability"] == "available")
                for name, target, scoped, path in [
                    ("native-codex", "codex", True, direct),
                    ("npm-codex", "codex", True, npm),
                    ("native-claude", "claude", True, direct),
                    ("unscoped-codex", "codex", False, direct),
                ]:
                    for kind, asset in (assets.items() if scoped and path == direct else [("svg", assets["svg"])]):
                        record = check_case(app, binary, root, out, name + "-" + kind, target,
                                            scoped, path, asset, native, kind)
                        result["cases"].append(record)
                        print(json.dumps(record), flush=True)
                for label, target, version in [
                    ("codex-minimum", "codex", "codex-cli 0.162.0"),
                    ("claude-minimum", "claude", "2.1.280 (Claude Code)"),
                    ("codex-later-minor", "codex", "codex-cli 0.200.0"),
                    ("claude-later-minor", "claude", "2.2.0 (Claude Code)"),
                ]:
                    record = check_case(app, binary, root, out, label, target,
                                        True, direct, assets["svg"], native, version=version)
                    result["cases"].append(record)
                    print(json.dumps(record), flush=True)
        result["pass"] = True
    finally:
        (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--worker", required=True)
    parser.add_argument("--node", default=shutil.which("node"))
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if not args.node:
        parser.error("an existing Node executable is required for the npm route")
    check(args.kettle, args.worker, Path(args.node).resolve(strict=True), args.out)
