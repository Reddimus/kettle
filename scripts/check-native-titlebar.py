#!/usr/bin/env python3
"""Check a light native caption against its traffic-light cluster on macOS."""

import argparse
import importlib.util
import json
from pathlib import Path
import signal
import sys
import time

spec = importlib.util.spec_from_file_location(
    "titlebar_modal", Path(__file__).with_name("check-modal-shortcuts.py")
)
modal = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = modal
spec.loader.exec_module(modal)


def caption_ink(path, bounds, scale):
    width, height, rows = modal.live_ui.read_rgba_png(path)
    assert scale in (1, 2), f"unsupported native backing scale: {scale}"
    scale = int(scale)
    left = round((width - bounds["Width"] * scale) / 2)
    top = next(y for y in range(height) if rows[y][(width // 2) * 4 + 3] > 250)
    overlap = title = 0
    for y in range(top + 7 * scale, top + 25 * scale):
        for x in range(left + 3 * scale, width - left - 3 * scale):
            red, green, blue, alpha = rows[y][x * 4:x * 4 + 4]
            if (alpha > 250 and max(red, green, blue) < 140
                    and max(red, green, blue) - min(red, green, blue) < 20):
                if x < left + 76 * scale:
                    # Hover glyphs inside native buttons are legitimate ink.
                    inside_button = any(
                        left + a * scale <= x <= left + b * scale
                        for a, b in ((8, 24), (31, 47), (54, 70))
                    ) and top + 8 * scale <= y <= top + 24 * scale
                    if not inside_button:
                        overlap += 1
                else:
                    title += 1
    return overlap, title


def capture(live, out, label, hover_caption=False):
    modal.focus(live)
    code = (
        'import AppKit; import Foundation; '
        'let rows=CGWindowListCopyWindowInfo([.optionOnScreenOnly],kCGNullWindowID)! as! [[String:Any]]; '
        'let row=rows.first { ($0[kCGWindowOwnerPID as String] as? Int)==' + str(live.pid) +
        ' && ($0[kCGWindowName as String] as? String)=="Kettle caption fixture" }!; '
        'let data=try! JSONSerialization.data(withJSONObject:row); '
        'print(String(data:data,encoding:.utf8)!)'
    )
    result = modal.live_ui.run(["swift", "-e", code], timeout=15)
    result.check_returncode()
    row = json.loads(result.stdout)
    bounds = row["kCGWindowBounds"]
    if hover_caption:
        if modal.frontmost_pid() != live.pid:
            raise RuntimeError("fixture Kettle lost focus before native pointer input")
        modal.live_ui.macos_mouse_event(5, bounds["X"] + 40, bounds["Y"] + 16)
        time.sleep(0.2)
    path = out / (label + ".png")
    result = modal.live_ui.run([
        "screencapture", "-x", "-l", str(row["kCGWindowNumber"]), str(path)
    ], timeout=15)
    result.check_returncode()
    scale = live.json_ctl("ui_geometry")["scale_factor"]
    count, title = caption_ink(path, bounds, scale)
    metric = {"traffic_light_caption_ink": count, "title_ink": title,
              "scale_factor": scale, "native_window": row}
    (out / (label + ".json")).write_text(json.dumps(metric, indent=2) + "\n")
    assert count == 0, f"native caption overlaps traffic lights: {count} pixels"
    assert title > 0, "native caption text is missing or the capture is blank"
    return metric


def interrupted(_signum, _frame):
    raise InterruptedError("owned titlebar check interrupted")


def check(kettle, out):
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    config = out / "config"
    config.write_text(
        "agent-server = full\nrestore-session = false\nupdate-policy = off\n"
        "login-shell = false\nshell-integration = false\nshell = /bin/sh\n"
        "window-title-format = Kettle caption fixture\ntab-format = {n}: demo\n"
        "theme = Catppuccin Latte\nbackground-opacity = 0.86\nwindow-blur = true\n"
        "window-width = 72\nwindow-height = 24\ncursor-blink = false\n"
    )
    with modal.RecordedLive(kettle, config, out / "kettle.log") as live:
        modal.focus(live)
        time.sleep(0.25)
        start = capture(live, out, "startup")
        scale = live.json_ctl("ui_geometry")["scale_factor"]
        wanted = {"width": round(740 * scale), "height": round(480 * scale)}
        live.json_ctl("resize_window", wanted)
        deadline = time.monotonic() + 10
        while live.json_ctl("ui_geometry")["surface"] != wanted:
            assert time.monotonic() < deadline, "resize did not settle"
            time.sleep(0.05)
        resized = capture(live, out, "resized")
        hovered = capture(live, out, "hovered", hover_caption=True)
    (out / "results.json").write_text(json.dumps({"startup": start, "resized": resized, "hovered": hovered}, indent=2) + "\n")
    print("native-titlebar smoke: caption clear of traffic lights", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", required=True)
    parser.add_argument("--out-dir", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise SystemExit("native-titlebar smoke requires macOS")
    signal.signal(signal.SIGTERM, interrupted)
    check(str(Path(args.kettle).resolve()), args.out_dir.resolve())
