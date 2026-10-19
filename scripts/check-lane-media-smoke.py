#!/usr/bin/env python3
"""Live smoke of media lanes on Linux, under an X server (xvfb-run).

  check-lane-media-smoke.py --kettle BIN --worker BIN [--out DIR]
  check-lane-media-smoke.py --self-test

Kettle runs only a worker file it trusts, so both binaries are staged into a
private directory first. Under the X server, it launches Kettle with an
isolated configuration and XDG directories, and checks, through ctl and native X input
(X11/XTest), what media lanes do on Linux:

- the media worker is available, and which decoders it reports;
- an image, an SVG, a Mermaid diagram and a video each land on the shelf
  with `kettle show` and render in a lane (the video as its poster);
- the video's play control, pressed natively, plays it (a control client
  may not), its position moving, with audio or a visible output fallback;
- the worker runs with no_new_privs and a seccomp filter;
- a native Left with the lane holding the keyboard steps it back;
- a control client's pause pauses it;
- under `language = es`, a screenshot of the lane, for review.

Results go to OUT/result.json, the screenshots beside it (default
target/diagnostics/lane-media-smoke/<UTC time>). `just lane-media-smoke`
builds both binaries and runs it under xvfb-run. Needs ffmpeg, X11/XTest and x11-utils.
"""
import argparse
import datetime
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

from native_x11 import X11Input

REPO = Path(__file__).resolve().parents[1]
# Set by main(): the staged kettle and where results go.
KETTLE = ""
MODE = "lane"
OUT = Path()
RESULT = {"checks": [], "pass": False}


def check(name, ok, detail=None):
    RESULT["checks"].append({"name": name, "ok": bool(ok), "detail": detail})
    print(("ok   " if ok else "FAIL ") + name + ("" if detail is None else f": {detail}"), flush=True)
    return ok


# The environment of the Kettle under test: ctl finds its server through the
# same state directory.
ENV = dict(os.environ)


def ctl(pid, *args, raw=True):
    command = [KETTLE, "ctl", "--pid", str(pid), *args] + (["--raw"] if raw else [])
    out = subprocess.run(command, capture_output=True, text=True, timeout=30, env=ENV)
    if out.returncode:
        raise RuntimeError(f"ctl {args[0]}: {out.stderr.strip()[:300]}")
    text = out.stdout.strip()
    return json.loads(text) if text.startswith("{") else text


def wait_for(what, probe, timeout=20.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = probe()
            if last:
                return last
        except (RuntimeError, ValueError, KeyError, IndexError) as error:
            last = error
        time.sleep(0.25)
    raise RuntimeError(f"timed out waiting for {what}: {last!r}")


def png(path, width=64, height=48):
    """A small RGB gradient, written without any imaging library."""
    raw = b"".join(b"\x00" + b"".join(bytes([(x * 4) % 256, (y * 5) % 256, 160]) for x in range(width))
                   for y in range(height))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def fixtures(dir_):
    files = {}
    files["image"] = dir_ / "gradient.png"
    png(files["image"])
    files["svg"] = dir_ / "shapes.svg"
    files["svg"].write_text('<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120">'
                            '<rect x="10" y="10" width="100" height="100" fill="#3a7"/>'
                            '<circle cx="180" cy="60" r="50" fill="#a37"/></svg>')
    files["mermaid"] = dir_ / "flow.mmd"
    files["mermaid"].write_text("flowchart LR\n  A[Show] --> B{Lane}\n  B --> C[Rendered]\n")
    files["video"] = dir_ / "flash-click.mov"
    subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-n",
                    "-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=40,format=yuv420p,"
                    "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='between(mod(n\\,30)\\,15\\,16)'",
                    "-f", "lavfi", "-i", "sine=f=1000:d=40:r=48000",
                    "-c:v", "libx264", "-g", "30", "-bf", "0", "-c:a", "pcm_s16le", "-shortest",
                    str(files["video"])], check=True, timeout=120)
    return files


def card_fixtures(directory):
    """Small, local-only examples shared by card acceptance and its demo."""
    directory.mkdir(parents=True, exist_ok=True)
    files = {"png": directory / "gradient.png"}
    png(files["png"], 160, 100)
    for suffix in ("jpg", "webp"):
        files[suffix] = directory / f"gradient.{suffix}"
        subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-n", "-i",
                        str(files["png"]), "-threads", "1", str(files[suffix])], check=True, timeout=20)
    files["svg"] = directory / "chart.svg"
    files["svg"].write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" width="480" height="240">'
        '<rect width="480" height="240" fill="#286c58"/>'
        '<rect x="40" y="100" width="90" height="100" fill="#ffcc66"/>'
        '<rect x="190" y="50" width="90" height="150" fill="#88ccff"/>'
        '<rect x="340" y="25" width="90" height="175" fill="#ff8899"/></svg>'
    )
    diagram = "flowchart LR\n A[Input] --> B[Rendered]\n style A fill:#286c58,color:#fff\n"
    files["mermaid"] = directory / "flow.mmd"
    files["mermaid"].write_text(diagram)
    files["markdown"] = directory / "gallery.md"
    files["markdown"].write_text(f"# Diagram gallery\n\n```mermaid\n{diagram}```\n\n"
                                 "```mermaid\nsequenceDiagram\n A->>B: Second diagram\n```\n")
    files["pdf"] = directory / "pages.pdf"
    objects = [b"<< /Type /Catalog /Pages 2 0 R >>",
               b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>"]
    for index, color in enumerate((b"0.157 0.424 0.345", b"0.2 0.35 0.8")):
        stream = color + b" rg 0 0 480 240 re f\n"
        objects.extend([
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 480 240] /Contents {4 + index * 2} 0 R >>".encode(),
            b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"endstream",
        ])
    data = bytearray(b"%PDF-1.4\n")
    offsets = [0]
    for number, obj in enumerate(objects, 1):
        offsets.append(len(data))
        data.extend(f"{number} 0 obj\n".encode() + obj + b"\nendobj\n")
    start = len(data)
    data.extend(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
    for offset in offsets[1:]:
        data.extend(f"{offset:010d} 00000 n \n".encode())
    data.extend(f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n".encode())
    files["pdf"].write_bytes(data)
    files["video"] = directory / "motion.mp4"
    subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-n", "-f", "lavfi", "-i",
                    "testsrc2=s=480x240:r=15:d=12", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-threads", "1",
                    str(files["video"])], check=True, timeout=30)
    files["file"] = directory / "example.kettle-demo"
    files["file"].write_text("A harmless file-card example.\n")
    return files


def launch(home, language):
    config = home / "config"
    config.write_text("\n".join([
        "restore-session = false", "update-check = false", "record = off",
        "window-width = 110", "window-height = 40", "agent-server = full", "agent-display = true",
        f"media-open-mode = {MODE}", "shell = /bin/sh", "login-shell = false",
        "shell-integration = false",
        f"language = {language}", ""]))
    env = dict(os.environ, LIBGL_ALWAYS_SOFTWARE="1", SHELL="/bin/sh",
               XDG_CONFIG_HOME=str(home / ".config"), XDG_STATE_HOME=str(home / ".state"),
               XDG_DATA_HOME=str(home / ".data"), XDG_CACHE_HOME=str(home / ".cache"),
               XDG_RUNTIME_DIR=str(home / "run"))
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("WAYLAND_SOCKET", None)
    for name in (".config", ".state", ".data", ".cache", "run"):
        (home / name).mkdir(mode=0o700, exist_ok=True)
    for name in ("ENV", "BASH_ENV"):
        env.pop(name, None)
    ENV.clear()
    ENV.update(env)
    log = open(home / "kettle.log", "wb")
    process = subprocess.Popen([KETTLE, "--new-process", "--config", str(config)], env=env,
                               stdin=subprocess.DEVNULL, stdout=log, stderr=log)
    wait_for("the control server", lambda: ctl(process.pid, "ui_geometry"), timeout=60)
    wait_for("the shell's prompt", lambda: screen_text(process.pid).rstrip().endswith(("%", "$", "#")))
    return process


def lane(pid):
    geometry = ctl(pid, "ui_geometry")
    lanes = geometry.get("preview_lanes") or []
    return lanes[0] if lanes else geometry.get("preview_overlay")


def screen_text(pid):
    return ctl(pid, "read_screen", raw=False)


def show(pid, path):
    before = screen_text(pid).count("Sent to the Kettle media shelf")
    ctl(pid, "send_text", "--text", f"{KETTLE} show {path}\n")
    try:
        wait_for(f"{path.name} on the shelf",
                 lambda: screen_text(pid).count("Sent to the Kettle media shelf") > before)
    except RuntimeError:
        print(screen_text(pid)[-1500:], flush=True)
        raise


def native_click(pid, rect):
    with X11Input() as native:
        windows = native.windows(pid)
        if len(windows) != 1:
            raise RuntimeError(f"expected one owned native window, got {windows}")
        native.focus(windows[0])
        native.move(windows[0], round(rect["x"] + rect["width"] / 2),
                    round(rect["y"] + rect["height"] / 2))
        native.button(1, True)
        native.button(1, False)


def native_key(pid, name):
    with X11Input() as native:
        windows = native.windows(pid)
        if len(windows) != 1:
            raise RuntimeError(f"expected one owned native window, got {windows}")
        native.focus(windows[0])
        native.keys(name)


def worker_pids(parent):
    pids = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            status = (entry / "status").read_text()
        except OSError:
            continue
        fields = dict(line.split(":\t", 1) for line in status.splitlines() if ":\t" in line)
        # Only this Kettle's own workers, its direct children.
        if fields.get("Name", "").startswith("kettle-media-wo") and fields.get("PPid", "").strip() == str(parent):
            pids.append((int(entry.name), fields))
    return pids


def stage(kettle, worker, into):
    """Both binaries in a private directory, as Kettle requires of its
    worker file: owned by the user, writable by no one else."""
    into.mkdir(mode=0o700)
    for source, name in ((kettle, "kettle"), (worker, "kettle-media-worker")):
        shutil.copyfile(source, into / name)
        (into / name).chmod(0o755)
    return str(into / "kettle")


def self_test():
    failures = []
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "g.png"
        png(path, 8, 4)
        data = path.read_bytes()
        if not data.startswith(b"\x89PNG\r\n\x1a\n") or struct.unpack(">II", data[16:24]) != (8, 4):
            failures.append("png header")
        staged = stage(path, path, Path(tmp) / "bin")
        mode = Path(staged).stat().st_mode & 0o777
        if mode != 0o755 or (Path(tmp) / "bin").stat().st_mode & 0o777 != 0o700:
            failures.append(f"staged modes {oct(mode)}")
    for failure in failures:
        print("self-test:", failure, file=sys.stderr)
    if not failures:
        print("self-test: ok")
    return 1 if failures else 0


def main():
    global KETTLE, MODE, OUT
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--require-audio", action="store_true", help="fail unless video sound is available")
    parser.add_argument("--mode", choices=("lane", "overlay"), default="lane")
    parser.add_argument("--worker", required=True)
    parser.add_argument("--out")
    args = parser.parse_args()
    MODE = args.mode
    if sys.platform != "linux" or not os.environ.get("DISPLAY"):
        sys.exit("lane-media-smoke: Linux with an X server only (run it under xvfb-run)")
    for tool in ("ffmpeg", "xwininfo", "xprop"):
        if not shutil.which(tool):
            sys.exit(f"lane-media-smoke: needs {tool}")
    # Kettle keeps its control registry only under private directories.
    os.umask(0o077)
    OUT = Path(args.out) if args.out else (
        REPO / "target" / "diagnostics" / "lane-media-smoke"
        / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ"))
    OUT.mkdir(mode=0o700, parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="kettle-lane-smoke-") as private:
        KETTLE = stage(Path(args.kettle), Path(args.worker), Path(private) / "bin")
        try:
            return run_checks(Path(private), args.require_audio)
        finally:
            (OUT / "result.json").write_text(json.dumps(RESULT, indent=2) + "\n")


def run_checks(private, require_audio=False):
    work = private / "work"
    work.mkdir()
    files = fixtures(work)
    home = work / "home-en"
    home.mkdir()
    process = launch(home, "en")
    pid = process.pid
    try:
        media = wait_for("the media worker", lambda: (lambda m: m if m.get("availability") in ("available", "unavailable") else None)(ctl(pid, "get_state").get("media") or {}))
        check("media worker available", media.get("availability") == "available", media)
        for kind in ("image", "svg", "mermaid"):
            show(pid, files[kind])
            ctl(pid, "perform_action", "--text", "open_media_shelf", raw=False)
            got = wait_for(f"the {kind} rendered", lambda: (lambda l: l if l and l.get("image") else None)(lane(pid)))
            check(f"{kind} renders in a lane", got.get("mode") == "rendered" and got["image"]["width"] > 0,
                  {k: got.get(k) for k in ("mode", "image")})
            ctl(pid, "screenshot", "--json", json.dumps({"full_window": True, "path": str(OUT / f"{kind}.png")}))
            ctl(pid, "perform_action", "--text", "open_media_shelf", raw=False)
            wait_for("the lane closed", lambda: lane(pid) is None)
        show(pid, files["video"])
        ctl(pid, "perform_action", "--text", "open_media_shelf", raw=False)
        poster = wait_for("the poster", lambda: (lambda l: l if l and l.get("image") and "play" in (l.get("controls") or {}) else None)(lane(pid)))
        check("video shows its poster with a play control", True, poster.get("image"))
        native_click(pid, poster["controls"]["play"])
        playing = wait_for("playback", lambda: (lambda p: p if p.get("state") == "playing" else None)((lane(pid) or {}).get("playback") or {}), timeout=15)
        first = playing["position_ms"]
        time.sleep(3)
        later = (lane(pid) or {}).get("playback") or {}
        check("a native press plays the video, its position moving",
              later.get("state") == "playing" and later.get("position_ms", 0) - first >= 2000,
              {"first": first, "later": later})
        sound = later.get("sound")
        check("video reports working audio or its output fallback",
              sound == "heard" if require_audio else sound in ("heard", "unavailable"), sound)
        workers = worker_pids(pid)
        check("the worker runs with no_new_privs and a seccomp filter",
              bool(workers) and all(f.get("NoNewPrivs", "").strip() == "1" and f.get("Seccomp", "").strip() == "2"
                                    for _, f in workers),
              [(p, f.get("NoNewPrivs", "").strip(), f.get("Seccomp", "").strip()) for p, f in workers])
        ctl(pid, "screenshot", "--json", json.dumps({"full_window": True, "path": str(OUT / "video-playing.png")}))
        ctl(pid, "perform_action", "--text", "focus_preview", raw=False)
        # Far enough in that five seconds back is not clamped at the start.
        before = wait_for("seven seconds in", lambda: (lambda p: p.get("position_ms", 0) if p.get("position_ms", 0) >= 7000 else None)((lane(pid) or {}).get("playback") or {}))
        stepped = time.monotonic()
        native_key(pid, "Left")
        time.sleep(1.5)
        after = ((lane(pid) or {}).get("playback") or {}).get("position_ms", 0)
        played = (time.monotonic() - stepped) * 1000
        check("a native Left steps it back five seconds", 4000 <= before + played - after <= 6000,
              {"before": before, "after": after, "played_ms": round(played)})
        # A control client's step is refused: the video and its bar stay.
        mid = ((lane(pid) or {}).get("playback") or {}).get("position_ms", 0)
        ctl(pid, "dispatch_ui_key", "--keys", "left", raw=False)
        time.sleep(1.0)
        still = ((lane(pid) or {}).get("playback") or {})
        check("a control client's Left moves nothing", still.get("position_ms", 0) >= mid,
              {"before": mid, "after": still.get("position_ms")})
        ctl(pid, "perform_action", "--text", "video_play", raw=False)
        paused = wait_for("the pause", lambda: (lambda p: p if p.get("state") == "paused" else None)((lane(pid) or {}).get("playback") or {}))
        check("a control client's pause pauses it", True, paused)
        check("audio remains available after seeking",
              paused.get("sound") == "heard" if require_audio else True, paused.get("sound"))
        ctl(pid, "dispatch_ui_key", "--keys", "escape", raw=False)
    finally:
        try:
            (OUT / f"final-{pid}.json").write_text(json.dumps(ctl(pid, "ui_geometry"), indent=2) + "\n")
            shutil.copyfile(home / "kettle.log", OUT / f"kettle-{pid}.log")
            ctl(pid, "screenshot", "--json", json.dumps({"full_window": True, "path": str(OUT / f"final-{pid}.png")}))
        except (OSError, RuntimeError, subprocess.TimeoutExpired):
            pass
        try:
            ctl(pid, "perform_action", "--text", "close_window", raw=False)
            process.wait(timeout=15)
        except Exception:  # noqa: BLE001 - always end the process this script started
            process.terminate()
            process.wait(timeout=10)
    # Spanish: the lane's own text, for review.
    home = work / "home-es"
    home.mkdir()
    process = launch(home, "es")
    pid = process.pid
    try:
        wait_for("the media worker", lambda: (ctl(pid, "get_state").get("media") or {}).get("availability") == "available")
        show(pid, files["video"])
        ctl(pid, "perform_action", "--text", "open_media_shelf", raw=False)
        wait_for("the poster", lambda: (lambda l: l if l and l.get("image") else None)(lane(pid)))
        ctl(pid, "screenshot", "--json", json.dumps({"full_window": True, "path": str(OUT / "video-es.png")}))
        check("Spanish lane captured for review", (OUT / "video-es.png").exists())
    finally:
        try:
            (OUT / f"final-{pid}.json").write_text(json.dumps(ctl(pid, "ui_geometry"), indent=2) + "\n")
            shutil.copyfile(home / "kettle.log", OUT / f"kettle-{pid}.log")
            ctl(pid, "screenshot", "--json", json.dumps({"full_window": True, "path": str(OUT / f"final-{pid}.png")}))
        except (OSError, RuntimeError, subprocess.TimeoutExpired):
            pass
        try:
            ctl(pid, "perform_action", "--text", "close_window", raw=False)
            process.wait(timeout=15)
        except Exception:  # noqa: BLE001
            process.terminate()
            process.wait(timeout=10)
    RESULT["pass"] = bool(RESULT["checks"]) and all(c["ok"] for c in RESULT["checks"])
    (OUT / "result.json").write_text(json.dumps(RESULT, indent=2) + "\n")
    print("PASS" if RESULT["pass"] else "FAIL")
    return 0 if RESULT["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
