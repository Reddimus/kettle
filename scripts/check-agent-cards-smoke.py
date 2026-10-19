#!/usr/bin/env python3
"""Exercise Linux launch scopes and rendered cards with a simulated native agent."""

import argparse
import importlib.util
import json
import os
import platform
import shlex
import shutil
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
        assert receipt["kind"] == {"png": "raster", "jpg": "raster", "webp": "raster"}.get(kind, kind)
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
            native.keys("Escape")
            wait_for(lambda: app.json_ctl("ui_geometry")["preview_overlay"] is None)
        else:
            assert not app.json_ctl("ui_geometry")["inline_cards"]
            assert json.loads(hook["result"]["content"][0]["text"]) == {}
        pids = [int(value) for value in (case / "pids").read_text().split()]
        (case / "quit").write_text("quit\n")
        wait_for(lambda: all(not Path(f"/proc/{pid}").exists() for pid in pids))
        wait_for(lambda: not app.json_ctl("ui_geometry")["inline_cards"])
        return {"case": name, "delivery": delivery, "pids": pids, "processes_closed": True}
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
                "restore-session = false\nupdate-policy = off\nrecord = off\n"
                "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
                "agent-server = full\nagent-display = true\nagent-display-claude-code = true\n"
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
