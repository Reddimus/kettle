#!/usr/bin/env python3
"""Live stress scenarios against a running Kettle (the break-it plan's C6).

Each scenario starts a fresh Kettle with a private config, drives it through
the control API, and must leave it alive with no panic in its log, answering
`get_state` within 2 s and echoing a typed command within 1 s afterwards.

  S1  50 MB of /dev/urandom in a pane
  S2  `yes` in four panes while the window resizes 50 times, down to 1x1
  S3  101 tabs and 151 panes, then close them all
  S4  split, flood and close a pane 50 times
  S5  close a pane whose child ignores SIGHUP; the child must not survive
  S6  a 64 MiB paste, then a paste with an embedded bracketed-paste end
  S7  a 16 MiB title, then 10,000 title changes
  S8  theme and language rewritten every 200 ms during a flood
  S9  hostile media through `kettle show` (corrupt, empty, huge and bomb
      images, SVG, Mermaid and video, a FIFO, a symlink loop); each must be
      refused within its deadline, and three good files must still render
  S10 64 parallel control clients, 20 requests each

S6 uses the system clipboard (pbcopy, wl-copy or xclip) and restores it
afterwards. S9 needs a working media worker, on macOS a signed one (`just
macos-avsync` signs it). Run on macOS or a Linux desktop session:

  scripts/check-stress-live.py --kettle target/debug/kettle --out-dir DIR [S1 S3 ...]

`--self-test` checks the pass criteria without starting Kettle.
"""
import argparse
import importlib.util
import json
import os
import struct
import subprocess
import sys
import threading
import time
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = (
    "agent-server = full\nrestore-session = false\nupdate-policy = off\nshell = /bin/sh\n"
    "login-shell = false\nshell-integration = false\nwindow-width = 100\nwindow-height = 30\n"
    "ask-before-closing = never\n"
)
MAX_GET_STATE_S = 2.0
MAX_ECHO_S = 1.0
MAX_TITLE_CHARS = 512


def failures(row):
    """Why a scenario's result fails the pass criteria; empty when it passes."""
    problems = []
    if row.get("error"):
        problems.append(f"error: {row['error']}")
    if row.get("panic"):
        problems.append("a panic in the log")
    if row.get("alive") is False:
        problems.append("kettle exited")
    if row.get("get_state_s") is not None and row["get_state_s"] > MAX_GET_STATE_S:
        problems.append(f"get_state took {row['get_state_s']} s")
    if "echo_s" in row and (row["echo_s"] is None or row["echo_s"] > MAX_ECHO_S):
        problems.append(f"echo round trip {row['echo_s']}")
    if row.get("tabs_left", 1) != 1 or row.get("panes_left", 1) != 1:
        problems.append("tabs or panes left open")
    if row.get("sleep_1000_left"):
        problems.append(f"orphaned children {row['sleep_1000_left']}")
    if any(length > MAX_TITLE_CHARS for length in row.get("title_lengths", [])):
        problems.append("a title past its cap")
    if row.get("show_timeouts") or row.get("show_unexpected"):
        problems.append(f"show: {row.get('show_timeouts')} timed out, {row.get('show_unexpected')} unexpected")
    if row.get("ctl_errors"):
        problems.append(f"{row['ctl_errors']} control errors")
    return problems


def rss_kib(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout
    return int(out.strip()) if out.strip().isdigit() else -1


def responsive(live):
    start = time.monotonic()
    live.json_ctl("get_state")
    return time.monotonic() - start


def echo_round_trip(live, timeout=10.0):
    token = f"rt{int(time.time() * 1000) % 100000}"
    start = time.monotonic()
    live.ctl("send_text", params={"text": f"echo {token}\r"})
    while time.monotonic() - start < timeout:
        text = live.json_ctl("read_screen").get("text", "")
        if f"\n{token}" in text or text.startswith(token):
            return time.monotonic() - start
        time.sleep(0.05)
    return None


def probe_during(live, seconds, every=0.5):
    """Worst get_state latency while something else runs."""
    worst = 0.0
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        worst = max(worst, responsive(live)); time.sleep(every)
    return round(worst, 3)


def s1_urandom(live):
    live.ctl("send_text", params={"text": "head -c 50000000 /dev/urandom; echo; echo DONE-S1\r"})
    worst = probe_during(live, 20)
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if "DONE-S1" in live.json_ctl("read_screen").get("text", ""):
            break
        time.sleep(0.5)
    live.ctl("send_keys", params={"keys": ["ctrl+c"]}); time.sleep(0.5)
    return {"worst_get_state_during_s": worst}


def s2_resize_storm(live):
    for _ in range(3):
        live.json_ctl("perform_action", {"action": "split_right"})
    time.sleep(0.5)
    panes = live.json_ctl("list_panes").get("panes", [])
    for pane in panes:
        live.ctl("send_text", params={"pane": pane["id"], "text": "yes kettle-stress-yes\r"})
    worst = 0.0
    sizes = [(1200, 800), (300, 200), (2000, 1200), (120, 80), (900, 600), (1, 1), (640, 480)]
    for i in range(50):
        w, h = sizes[i % len(sizes)]
        try:
            live.json_ctl("resize_window", {"width": w, "height": h})
        except BaseException:
            pass
        worst = max(worst, responsive(live))
    for pane in panes:
        live.ctl("send_keys", params={"pane": pane["id"], "keys": ["ctrl+c"]})
    live.json_ctl("resize_window", {"width": 900, "height": 600}); time.sleep(1)
    rss_open = rss_kib(live.proc.pid)
    for _ in range(3):
        live.json_ctl("perform_action", {"action": "close_pane"}); time.sleep(0.3)
    time.sleep(10)
    return {"worst_get_state_during_s": round(worst, 3), "panes": len(panes), "rss_with_panes_kib": rss_open, "rss_10s_after_close_kib": rss_kib(live.proc.pid), "panes_left": len(live.json_ctl("list_panes").get("panes", []))}


def s3_many_tabs(live):
    for _ in range(100):
        live.json_ctl("perform_action", {"action": "new_tab"})
    for _ in range(50):
        live.json_ctl("perform_action", {"action": "split_right"})
    state = live.json_ctl("list_tabs")
    tabs = len(state.get("tabs", []))
    panes = len(live.json_ctl("list_panes").get("panes", []))
    worst = probe_during(live, 3)
    for _ in range(tabs + 60):
        live.json_ctl("perform_action", {"action": "close_tab"})
        if len(live.json_ctl("list_tabs").get("tabs", [])) <= 1:
            break
    return {"tabs_opened": tabs, "panes_opened": panes, "worst_get_state_during_s": worst,
            "tabs_left": len(live.json_ctl("list_tabs").get("tabs", []))}


def s5_sighup_ignorer(live):
    live.json_ctl("perform_action", {"action": "new_tab"}); time.sleep(0.5)
    live.ctl("send_text", params={"text": "trap '' HUP; exec sleep 1000\r"}); time.sleep(1)
    live.json_ctl("perform_action", {"action": "close_tab"}); time.sleep(3)
    left = subprocess.run(["pgrep", "-f", "sleep 1000"], capture_output=True, text=True).stdout.split()
    return {"sleep_1000_left": left}


def s7_titles(live):
    live.ctl("send_text", params={"text": "printf '\\033]0;%s\\007' \"$(head -c 16777216 /dev/zero | tr '\\0' T)\"\r"})
    time.sleep(3)
    live.ctl("send_text", params={"text": "i=0; while [ $i -lt 10000 ]; do printf '\\033]2;title %d\\007' $i; i=$((i+1)); done; echo DONE-S7\r"})
    worst = probe_during(live, 15)
    titles = [t.get("title", "") for t in live.json_ctl("list_tabs").get("tabs", [])]
    return {"worst_get_state_during_s": worst, "title_lengths": [len(t) for t in titles]}


def s10_ctl_storm(live):
    errors = []; ok = [0]
    def loop():
        for _ in range(20):
            try:
                live.json_ctl("get_state"); ok[0] += 1
            except BaseException as error:
                errors.append(str(error)[:120])
    threads = [threading.Thread(target=loop) for _ in range(64)]
    for t in threads: t.start()
    for t in threads: t.join()
    return {"ctl_ok": ok[0], "ctl_errors": len(errors), "first_error": errors[0] if errors else None}


def s4_close_mid_flood(live):
    for _ in range(50):
        live.json_ctl("perform_action", {"action": "split_right"})
        live.ctl("send_text", params={"text": "yes kettle-close-mid-flood\r"})
        time.sleep(0.1)
        live.json_ctl("perform_action", {"action": "close_pane"})
    return {"panes_left": len(live.json_ctl("list_panes").get("panes", [])), "worst_get_state_after_s": probe_during(live, 2)}


def clipboard_commands():
    """The system clipboard's (copy, paste) commands: pbcopy on macOS,
    wl-copy under Wayland, else xclip."""
    if sys.platform == "darwin":
        return ["pbcopy"], ["pbpaste"]
    if os.environ.get("WAYLAND_DISPLAY"):
        return ["wl-copy"], ["wl-paste", "--no-newline"]
    return ["xclip", "-selection", "clipboard"], ["xclip", "-selection", "clipboard", "-o"]


def s6_paste(live):
    copy, paste = clipboard_commands()
    saved = subprocess.run(paste, capture_output=True).stdout
    try:
        live.ctl("send_text", params={"text": "cat > /dev/null\r"}); time.sleep(0.5)
        big = b"kettle paste line\n" * (64 * 1024 * 1024 // 18)
        subprocess.run(copy, input=big, check=True)
        live.json_ctl("perform_action", {"action": "paste"})
        worst = probe_during(live, 8)
        live.ctl("send_keys", params={"keys": ["ctrl+c"]}); time.sleep(0.5)
        live.ctl("send_text", params={"text": "cat -v\r"}); time.sleep(0.5)
        subprocess.run(copy, input=b"before\x1b[201~after-end-marker\n", check=True)
        live.json_ctl("perform_action", {"action": "paste"}); time.sleep(1)
        screen = live.json_ctl("read_screen").get("text", "")
        live.ctl("send_keys", params={"keys": ["ctrl+c"]}); time.sleep(0.3)
        return {"worst_get_state_during_s": worst, "end_marker_echo": [l for l in screen.splitlines() if "after-end-marker" in l][:3]}
    finally:
        subprocess.run(copy, input=saved, check=False)


def s8_theme_language_churn(live):
    live.ctl("send_text", params={"text": "yes kettle-churn | head -c 200000000; echo CHURN-DONE\r"})
    cfg = live.cfg
    base = cfg.read_text()
    worst = 0.0
    try:
        for i in range(50):
            theme = "Catppuccin Mocha" if i % 2 else "Catppuccin Latte"
            language = "es" if i % 3 == 0 else "en"
            cfg.write_text(base + f"theme = {theme}\nlanguage = {language}\n")
            worst = max(worst, responsive(live)); time.sleep(0.2)
    finally:
        cfg.write_text(base)
    live.ctl("send_keys", params={"keys": ["ctrl+c"]}); time.sleep(1)
    return {"worst_get_state_during_s": round(worst, 3)}


def png(width, height, color_type, idat):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    header = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def zero_rows(width, height, channels):
    deflate = zlib.compressobj(9)
    row = b"\x00" * (1 + width * channels)
    return b"".join(deflate.compress(row) for _ in range(height)) + deflate.flush()


def media_cases(root):
    """S9's files: name -> (path, as Mermaid, must render)."""
    root.mkdir(parents=True, exist_ok=True)
    good_png = png(64, 64, 2, zero_rows(64, 64, 3))
    nested_use = ['<g id="u0"><rect width="1" height="1"/></g>'] + [
        f'<g id="u{i}">' + f'<use href="#u{i - 1}"/>' * 10 + "</g>" for i in range(1, 11)]
    entities = "".join(f'<!ENTITY {chr(98 + i)} "' + f"&{chr(97 + i)};" * 10 + '">' for i in range(9))
    files = {
        "ok.png": (good_png, False, True),
        "ok.mmd": (b"graph TD\nA-->B\n", True, True),
        "ok.md": (b"```mermaid\ngraph TD\nA-->B\n```\n", False, True),
        "empty.png": (b"", False, False),
        "corrupt.png": (b"\x89PNG\r\n\x1a\n" + os.urandom(65536), False, False),
        "truncated.png": (good_png[: len(good_png) // 2], False, False),
        "16k.png": (png(16384, 16384, 0, zero_rows(16384, 16384, 1)), False, False),
        "claims-64k.png": (png(65535, 65535, 6, zlib.compress(b"\x00" * 5)), False, False),
        "no-frames.gif": (b"GIF89a" + struct.pack("<HHBBB", 100, 100, 0, 0, 0) + b";", False, False),
        "use-bomb.svg": (('<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><defs>'
                          + "".join(nested_use) + '</defs><use href="#u10"/></svg>').encode(), False, False),
        "entity-bomb.svg": (('<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaaaaaaaa">' + entities
                             + ']><svg xmlns="http://www.w3.org/2000/svg"><text>&j;</text></svg>').encode(), False, False),
        "500-subgraphs.mmd": (("flowchart TD\n" + "".join(f"subgraph s{i}\n" for i in range(500)) + "a-->b\n" + "end\n" * 500).encode(), True, False),
        "500-loops.mmd": (("sequenceDiagram\n" + "loop l\n" * 500 + "A->>B: hi\n" + "end\n" * 500).encode(), True, False),
        "10k-edges.mmd": (("graph TD\n" + "".join(f"a{i}-->a{i + 1}\n" for i in range(10000))).encode(), True, False),
        "1m-label.mmd": (('graph TD\nA["' + "x" * (1 << 20) + '"]-->B\n').encode(), True, False),
        "40-diagrams.md": ("".join(f"```mermaid\ngraph TD\nA{i}-->B{i}\n```\n" for i in range(40)).encode(), False, False),
        "corrupt.mp4": (b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isom" + os.urandom(200000), False, False),
    }
    cases = {}
    for name, (data, mermaid, renders) in files.items():
        (root / name).write_bytes(data)
        cases[name] = (root / name, mermaid, renders)
    fifo, loop = root / "fifo.png", root / "loop.png"
    if not fifo.exists():
        os.mkfifo(fifo)
    if not loop.is_symlink():
        loop.symlink_to(loop)
    cases["fifo.png"] = (fifo, False, False)
    cases["loop.png"] = (loop, False, False)
    return cases


def s9_media(live):
    timeouts, unexpected = [], []
    for name, (path, mermaid, renders) in media_cases(live.cfg.parent / "s9-media").items():
        token = f"S9-{name}"
        flag = "--mermaid " if mermaid else ""
        live.ctl("send_text", params={"text": f"clear; '{live.kettle}' show {flag}'{path}' >/dev/null 2>&1; echo {token} rc=$?\r"})
        deadline = time.monotonic() + 30
        status = None
        while status is None and time.monotonic() < deadline:
            lines = live.json_ctl("read_screen").get("text", "").splitlines()
            status = next((l.split("rc=")[1].strip() for l in lines if l.startswith(f"{token} rc=")), None)
            time.sleep(0.2)
        if status is None:
            timeouts.append(name)
        elif (status == "0") != renders:
            unexpected.append(f"{name} rc={status}")
    return {"show_timeouts": timeouts, "show_unexpected": unexpected}


SCENARIOS = {
    "S1": s1_urandom,
    "S2": s2_resize_storm,
    "S3": s3_many_tabs,
    "S4": s4_close_mid_flood,
    "S5": s5_sighup_ignorer,
    "S6": s6_paste,
    "S7": s7_titles,
    "S8": s8_theme_language_churn,
    "S9": s9_media,
    "S10": s10_ctl_storm,
}


def run(lui, kettle, out, name):
    cfg = out / f"{name}.config"
    cfg.write_text(CONFIG)
    log = out / f"{name}.log"
    row = {"scenario": name}
    started = time.monotonic()
    try:
        with lui.LiveKettle(kettle, cfg, log) as live:
            time.sleep(1.0)
            pid = live.proc.pid
            row["rss_start_kib"] = rss_kib(pid)
            peak = [row["rss_start_kib"]]
            stop = threading.Event()

            def sample():
                while not stop.is_set():
                    peak.append(rss_kib(pid))
                    time.sleep(0.25)

            sampler = threading.Thread(target=sample, daemon=True)
            sampler.start()
            try:
                row.update(SCENARIOS[name](live) or {})
            finally:
                stop.set()
                sampler.join()
            row["rss_peak_kib"] = max(peak)
            row["alive"] = live.proc.poll() is None
            row["get_state_s"] = round(responsive(live), 3)
            echo = echo_round_trip(live)
            row["echo_s"] = None if echo is None else round(echo, 3)
    except BaseException as error:  # noqa: BLE001 - a scenario's failure is its result
        row["error"] = f"{type(error).__name__}: {error}"[:500]
    row["seconds"] = round(time.monotonic() - started, 1)
    row["panic"] = log.exists() and "panicked at" in log.read_text(errors="replace")
    row["failures"] = failures(row)
    return row


def self_test():
    passing = {"alive": True, "panic": False, "get_state_s": 0.01, "echo_s": 0.02}
    assert failures(passing) == []
    for broken, why in [
        ({"alive": False}, "kettle exited"),
        ({"panic": True}, "a panic"),
        ({"echo_s": None}, "echo round trip"),
        ({"echo_s": 1.5}, "echo round trip"),
        ({"get_state_s": 2.5}, "get_state"),
        ({"tabs_left": 101}, "left open"),
        ({"sleep_1000_left": ["123"]}, "orphaned"),
        ({"title_lengths": [513]}, "title"),
        ({"ctl_errors": 3}, "control errors"),
        ({"error": "SystemExit: server closed the connection"}, "error"),
        ({"show_timeouts": ["16k.png"]}, "show"),
        ({"show_unexpected": ["ok.png rc=1"]}, "show"),
    ]:
        result = failures({**passing, **broken})
        assert any(why in item for item in result), (broken, result)
    assert set(SCENARIOS) == {"S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9", "S10"}
    print("check-stress-live self-test: ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--kettle")
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("scenarios", nargs="*", help="scenario names (all when none)")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if not args.kettle or not args.out_dir:
        parser.error("--kettle and --out-dir are required")
    unknown = sorted(set(args.scenarios) - set(SCENARIOS))
    if unknown:
        parser.error(f"unknown scenarios: {' '.join(unknown)}")
    spec = importlib.util.spec_from_file_location("live_ui", ROOT / "scripts/check-live-ui-smoke.py")
    lui = importlib.util.module_from_spec(spec)
    sys.modules["live_ui"] = lui
    spec.loader.exec_module(lui)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    failed = False
    for name in args.scenarios or list(SCENARIOS):
        row = run(lui, str(Path(args.kettle).resolve()), args.out_dir, name)
        print(json.dumps(row), flush=True)
        failed |= bool(row["failures"])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
