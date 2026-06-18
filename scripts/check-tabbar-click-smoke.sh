#!/usr/bin/env bash
set -euo pipefail

KETTLE="${KETTLE_BIN:-kettle}"
TIMEOUT="${KETTLE_TABBAR_TIMEOUT:-20}"

if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
  echo "tabbar-click smoke: skipped (no DISPLAY or WAYLAND_DISPLAY)" >&2
  exit 0
fi

stamp="$(date +%Y%m%d-%H%M%S)"
out="${KETTLE_DIAG_DIR:-target/diagnostics}/tabbar-click-$stamp"
case "$out" in
  /*) ;;
  *) out="$(pwd)/$out" ;;
esac
mkdir -p "$out"
pid=""
cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

cfg="$out/config"
cat > "$cfg" <<'CFG'
agent-server = full
tab-bar = always
tab-bar-pos = top
status-bar = off
restore-session = false
update-check = false
background = #101010
foreground = #f4f4f4
CFG

"$KETTLE" --config "$cfg" --agent-server full >"$out/kettle.log" 2>&1 &
pid="$!"

deadline=$((SECONDS + TIMEOUT))
while ! "$KETTLE" ctl --pid "$pid" list_tabs --raw >"$out/tabs-0.json" 2>/dev/null; do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "tabbar-click smoke: kettle exited before control server came up" >&2
    cat "$out/kettle.log" >&2 || true
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "tabbar-click smoke: timed out waiting for control server" >&2
    cat "$out/kettle.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done

click_rect_center() {
  python3 - "$1" "$2" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
rect = data
for part in sys.argv[2].split("."):
    rect = rect[part]
print(json.dumps({
    "event": "click",
    "x": rect["x"] + rect["width"] / 2,
    "y": rect["y"] + rect["height"] / 2,
    "button": "left",
}))
PY
}

press_segment_center() {
  python3 - "$1" "$2" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
idx = int(sys.argv[2])
seg = data["tab_bar"]["segments"][idx]["rect"]
print(json.dumps({
    "event": "press",
    "x": seg["x"] + seg["width"] / 2,
    "y": seg["y"] + seg["height"] / 2,
    "button": "left",
}))
PY
}

release_at_cursor() {
  python3 - "$1" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
x, y = data["cursor"]
print(json.dumps({"event": "release", "x": x, "y": y, "button": "left"}))
PY
}

move_jitter_from_cursor() {
  python3 - "$1" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
x, y = data["cursor"]
print(json.dumps({"event": "move", "x": float(x) + 6.0, "y": y}))
PY
}

for i in 1 2; do
  "$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-plus-$i.json"
  "$KETTLE" ctl --pid "$pid" send_mouse --json "$(click_rect_center "$out/geometry-plus-$i.json" tab_bar.new_tab)" >/dev/null
  sleep 0.2
done

"$KETTLE" ctl --pid "$pid" list_tabs --raw >"$out/tabs-created.json"
python3 - "$out/tabs-created.json" <<'PY'
import json, sys
tabs = json.load(open(sys.argv[1])).get("tabs", [])
if len(tabs) < 3:
    raise SystemExit(f"tabbar-click smoke: expected at least 3 tabs, got {len(tabs)}")
PY

"$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-before-press.json"
"$KETTLE" ctl --pid "$pid" screenshot --json "{\"full_window\":true,\"path\":\"$out/before-press.png\"}" >/dev/null
"$KETTLE" ctl --pid "$pid" send_mouse --json "$(press_segment_center "$out/geometry-before-press.json" 1)" >/dev/null
sleep 0.1
"$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-pressed.json"
"$KETTLE" ctl --pid "$pid" screenshot --json "{\"full_window\":true,\"path\":\"$out/pressed.png\"}" >/dev/null
"$KETTLE" ctl --pid "$pid" send_mouse --json "$(move_jitter_from_cursor "$out/geometry-pressed.json")" >/dev/null
sleep 0.1
"$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-jittered.json"
"$KETTLE" ctl --pid "$pid" screenshot --json "{\"full_window\":true,\"path\":\"$out/jittered.png\"}" >/dev/null
"$KETTLE" ctl --pid "$pid" send_mouse --json "$(release_at_cursor "$out/geometry-jittered.json")" >/dev/null
sleep 0.1
"$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-released.json"
"$KETTLE" ctl --pid "$pid" screenshot --json "{\"full_window\":true,\"path\":\"$out/released.png\"}" >/dev/null

python3 - "$out" <<'PY'
import json
import struct
import sys
import zlib
from pathlib import Path

def read_rgba_png(path):
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos = 8
    width = height = None
    raw = b""
    while pos < len(data):
        n = struct.unpack(">I", data[pos : pos + 4])[0]
        pos += 4
        typ = data[pos : pos + 4]
        pos += 4
        chunk = data[pos : pos + n]
        pos += n + 4
        if typ == b"IHDR":
            width, height, bit_depth, color_type, _, _, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
            if (bit_depth, color_type, interlace) != (8, 6, 0):
                raise SystemExit(f"{path}: expected non-interlaced 8-bit RGBA PNG")
        elif typ == b"IDAT":
            raw += chunk
        elif typ == b"IEND":
            break
    if width is None or height is None:
        raise SystemExit(f"{path}: missing IHDR")
    decoded = zlib.decompress(raw)
    bpp = 4
    stride = width * bpp
    rows = []
    prev = [0] * stride
    i = 0
    for _ in range(height):
        filt = decoded[i]
        i += 1
        cur = list(decoded[i : i + stride])
        i += stride
        recon = [0] * stride
        for x, value in enumerate(cur):
            left = recon[x - bpp] if x >= bpp else 0
            up = prev[x]
            up_left = prev[x - bpp] if x >= bpp else 0
            if filt == 0:
                out = value
            elif filt == 1:
                out = value + left
            elif filt == 2:
                out = value + up
            elif filt == 3:
                out = value + ((left + up) // 2)
            elif filt == 4:
                p = left + up - up_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                predictor = left if pa <= pb and pa <= pc else (up if pb <= pc else up_left)
                out = value + predictor
            else:
                raise SystemExit(f"{path}: unsupported PNG filter {filt}")
            recon[x] = out & 0xFF
        rows.append(bytes(recon))
        prev = recon
    return width, height, rows

def rect_contains(rect, x, y, margin=3):
    return (
        x >= rect["x"] - margin
        and x < rect["x"] + rect["width"] + margin
        and y >= rect["y"] - margin
        and y < rect["y"] + rect["height"] + margin
    )

def active_rect(geometry):
    active = [s for s in geometry["tab_bar"]["segments"] if s.get("active")]
    if len(active) != 1:
        raise SystemExit(f"tabbar-click smoke: expected one active tab, got {active}")
    return active[0]["rect"], active[0]["index"]

def changed_pixels(a_path, b_path, y0, y1):
    aw, ah, a = read_rgba_png(a_path)
    bw, bh, b = read_rgba_png(b_path)
    if (aw, ah) != (bw, bh):
        raise SystemExit("tabbar-click smoke: screenshot dimensions changed")
    changed = []
    for y in range(max(0, int(y0)), min(ah, int(y1))):
        ra = a[y]
        rb = b[y]
        for x in range(aw):
            off = x * 4
            if ra[off : off + 4] != rb[off : off + 4]:
                changed.append((x, y))
    return changed

out = Path(sys.argv[1])
before = json.loads((out / "geometry-before-press.json").read_text())
pressed = json.loads((out / "geometry-pressed.json").read_text())
jittered = json.loads((out / "geometry-jittered.json").read_text())
released = json.loads((out / "geometry-released.json").read_text())
if not pressed.get("tab_drag_active"):
    raise SystemExit("tabbar-click smoke: press did not arm tab drag state")
if not pressed.get("tab_drag_armed"):
    raise SystemExit("tabbar-click smoke: press should remain click-armed before movement")
if pressed.get("tab_drag_visible"):
    raise SystemExit("tabbar-click smoke: drag ghost became visible during a plain click")
if not jittered.get("tab_drag_active") or not jittered.get("tab_drag_armed"):
    raise SystemExit("tabbar-click smoke: small tab-click jitter promoted to drag")
if jittered.get("tab_drag_visible"):
    raise SystemExit("tabbar-click smoke: drag ghost became visible during small click jitter")
active = [s for s in pressed["tab_bar"]["segments"] if s.get("active")]
if len(active) != 1 or active[0].get("index") != 1:
    raise SystemExit(f"tabbar-click smoke: expected tab 1 active after press, got {active}")
if released.get("tab_drag_active") or released.get("tab_drag_armed") or released.get("tab_drag_visible"):
    raise SystemExit("tabbar-click smoke: release left tab drag state latched")

before_rect, before_idx = active_rect(before)
pressed_rect, pressed_idx = active_rect(pressed)
bar = pressed["tab_bar"]
y0 = bar["y"]
y1 = bar["y"] + bar["height"]
changed = changed_pixels(out / "before-press.png", out / "pressed.png", y0, y1)
outside = [
    (x, y)
    for x, y in changed
    if not (rect_contains(before_rect, x, y) or rect_contains(pressed_rect, x, y))
]
if outside:
    xs = [x for x, _ in outside]
    ys = [y for _, y in outside]
    raise SystemExit(
        "tabbar-click smoke: tab press changed pixels outside old/new active tab rects: "
        f"outside={len(outside)} bbox=({min(xs)},{min(ys)},{max(xs)+1},{max(ys)+1})"
    )

all_chrome_rects = [s["rect"] for s in pressed["tab_bar"]["segments"]]
all_chrome_rects.append(pressed["tab_bar"]["new_tab"])
if pressed["tab_bar"]["new_tab_menu"]["width"] > 0:
    all_chrome_rects.append(pressed["tab_bar"]["new_tab_menu"])
release_changed = changed_pixels(out / "pressed.png", out / "released.png", y0, y1)
release_outside = [
    (x, y)
    for x, y in release_changed
    if not any(rect_contains(rect, x, y) for rect in all_chrome_rects)
]
if release_outside:
    xs = [x for x, _ in release_outside]
    ys = [y for _, y in release_outside]
    raise SystemExit(
        "tabbar-click smoke: release changed pixels outside tab chrome: "
        f"outside={len(release_outside)} bbox=({min(xs)},{min(ys)},{max(xs)+1},{max(ys)+1})"
    )

analysis = {
    "before_active": before_idx,
    "pressed_active": pressed_idx,
    "before_active_rect": before_rect,
    "pressed_active_rect": pressed_rect,
    "tabbar_y": y0,
    "tabbar_height": bar["height"],
    "press_changed_pixels": len(changed),
    "press_outside_allowed_rects": len(outside),
    "release_changed_pixels": len(release_changed),
    "release_outside_chrome_pixels": len(release_outside),
}
(out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
print("tabbar-click smoke: OK artifacts=" + str(out))
PY
