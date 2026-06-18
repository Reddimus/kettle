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
tab-bar = on
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
"$KETTLE" ctl --pid "$pid" send_mouse --json "$(release_at_cursor "$out/geometry-pressed.json")" >/dev/null
sleep 0.1
"$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-released.json"
"$KETTLE" ctl --pid "$pid" screenshot --json "{\"full_window\":true,\"path\":\"$out/released.png\"}" >/dev/null

python3 - "$out/geometry-pressed.json" "$out/geometry-released.json" <<'PY'
import json, sys
pressed = json.load(open(sys.argv[1]))
released = json.load(open(sys.argv[2]))
if not pressed.get("tab_drag_active"):
    raise SystemExit("tabbar-click smoke: press did not arm tab drag state")
if not pressed.get("tab_drag_armed"):
    raise SystemExit("tabbar-click smoke: press should remain click-armed before movement")
if pressed.get("tab_drag_visible"):
    raise SystemExit("tabbar-click smoke: drag ghost became visible during a plain click")
active = [s for s in pressed["tab_bar"]["segments"] if s.get("active")]
if len(active) != 1 or active[0].get("index") != 1:
    raise SystemExit(f"tabbar-click smoke: expected tab 1 active after press, got {active}")
if released.get("tab_drag_active") or released.get("tab_drag_armed") or released.get("tab_drag_visible"):
    raise SystemExit("tabbar-click smoke: release left tab drag state latched")
print("tabbar-click smoke: OK artifacts=" + sys.argv[1].rsplit("/", 1)[0])
PY
