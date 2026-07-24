#!/usr/bin/env bash
# v2.40.0 (tear-off UX): live-desktop regression guard for the REAL
# tear-off gesture — the part no ctl-driven smoke can reach, because
# `maybe_tear_off`/re-dock are wired only into native winit pointer events
# (see docs/TESTING.md). Drives xdotool (XTEST) against a real X11 session,
# same precedent as scripts/menu-screenshot.sh, and asserts the failure
# modes a session recording caught on GNOME/Mutter stay fixed:
#   1. the torn window FOLLOWS the pointer (no mid-air freeze when the
#      native handoff silently fails or the pointer leaves the source);
#   2. dropping on a sibling's tab band MERGES the tab back (the dock
#      latch runs from the live cursor, not the drifted frame+grab guess);
#   3. Esc before the threshold cancels without tearing.
# The gesture runs twice, one kettle instance per carry path: the native
# `_NET_WM_MOVERESIZE` handoff, then KETTLE_TEAR_MANUAL_FOLLOW=1 forcing
# the manual-follow/rescue-tick fallback (otherwise only reachable through
# a nondeterministic WM race).
# X11-only by design: Wayland tears at release and takes no synthetic
# XTEST input; macOS/Windows have no xdotool.
set -euo pipefail

KETTLE="${KETTLE_BIN:-kettle}"
TIMEOUT="${KETTLE_TEAROFF_TIMEOUT:-25}"

if [ -z "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
  echo "tearoff live smoke: skipped (needs an X11 session: DISPLAY set, WAYLAND_DISPLAY empty)" >&2
  exit 0
fi
for tool in xdotool xwininfo python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "tearoff live smoke: skipped ($tool not found)" >&2
    exit 0
  fi
done

stamp="$(date +%Y%m%d-%H%M%S)"
out="${KETTLE_DIAG_DIR:-target/diagnostics}/tearoff-live-$stamp"
case "$out" in
  /*) ;;
  *) out="$(pwd)/$out" ;;
esac
mkdir -p "$out"
pid=""
events_pid=""
cleanup() {
  if [ -n "$events_pid" ] && kill -0 "$events_pid" 2>/dev/null; then
    kill "$events_pid" 2>/dev/null || true
  fi
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
tab-bar-position = top
detachable-tabs = true
status-bar = off
restore-session = false
update-check = false
background = #101010
foreground = #f4f4f4
window-width = 120
window-height = 30
CFG

# Client-area absolute origin (xwininfo reports the CLIENT rect; xdotool's
# getwindowgeometry reports the frame and is off by the WM decorations).
client_xy() {
  xwininfo -id "$1" | python3 -c '
import sys
x = y = None
for line in sys.stdin:
    line = line.strip()
    if line.startswith("Absolute upper-left X:"): x = int(line.split(":")[1])
    if line.startswith("Absolute upper-left Y:"): y = int(line.split(":")[1])
print(f"{x} {y}")
'
}

ctl() { "$KETTLE" ctl --pid "$pid" "$@"; }

tab_windows() {
  ctl list_tabs --raw | python3 -c '
import json,sys
tabs = json.load(sys.stdin)["tabs"]
ws = sorted({t["window"] for t in tabs})
print(f"{len(tabs)} {len(ws)}")
'
}

# One full gesture pass against a fresh kettle instance.
#   $1 = label (native | manual-follow)
#   $2 = 1 to also run the Esc-cancel check
#   remaining args = extra env for the kettle launch
gesture_pass() {
  label="$1"; check_cancel="$2"; shift 2

  env "$@" "$KETTLE" --config "$cfg" --agent-server full >"$out/kettle-$label.log" 2>&1 &
  pid="$!"
  deadline=$((SECONDS + TIMEOUT))
  while ! ctl list_tabs --raw >/dev/null 2>&1; do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "tearoff live smoke [$label]: kettle exited before control server came up" >&2
      cat "$out/kettle-$label.log" >&2 || true
      exit 1
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "tearoff live smoke [$label]: timed out waiting for control server" >&2
      exit 1
    fi
    sleep 0.1
  done
  ctl events >"$out/events-$label.ndjson" 2>/dev/null &
  events_pid="$!"

  ctl perform_action --json '{"action":"new_tab"}' >/dev/null
  wid="$(xdotool search --pid "$pid" | head -1)"
  xdotool windowmove --sync "$wid" 60 60
  sleep 0.4
  read -r wx wy <<<"$(client_xy "$wid")"
  bar_h="$(ctl ui_geometry --raw | python3 -c 'import json,sys; print(json.load(sys.stdin)["tab_bar"]["height"])')"
  tear_px="$(python3 -c "print(int(float('$bar_h') * 3))")"   # 2× the 1.5×bar_h threshold
  seg2_cx="$(ctl ui_geometry --raw | python3 -c '
import json,sys
segs = json.load(sys.stdin)["tab_bar"]["segments"]
r = segs[1]["rect"]
print(int(r["x"] + r["width"] / 2))
')"
  press_x=$((wx + seg2_cx))
  press_y=$((wy + ${bar_h%.*} / 2))

  if [ "$check_cancel" = "1" ]; then
    # --- Esc before the threshold cancels: press, jitter, Esc, release. ---
    xdotool mousemove --sync "$press_x" "$press_y"
    xdotool mousedown 1
    sleep 0.15
    xdotool mousemove --sync $((press_x + 6)) "$press_y"
    sleep 0.15
    xdotool key Escape
    sleep 0.15
    xdotool mouseup 1
    sleep 0.3
    read -r ntabs nwins <<<"$(tab_windows)"
    if [ "$ntabs" != "2" ] || [ "$nwins" != "1" ]; then
      echo "tearoff live smoke [$label]: Esc-cancel changed the window/tab set ($ntabs tabs, $nwins windows)" >&2
      exit 1
    fi
  fi

  # --- The tear: press tab 2, drag past the threshold, keep holding. ---
  xdotool mousemove --sync "$press_x" "$press_y"
  xdotool mousedown 1
  sleep 0.15
  xdotool mousemove --sync $((press_x + 7)) "$press_y"
  sleep 0.1
  xdotool mousemove --sync $((press_x + 10)) $((press_y + tear_px))
  sleep 0.6
  torn=""
  deadline=$((SECONDS + 5))
  while [ "$SECONDS" -lt "$deadline" ]; do
    torn="$(xdotool search --pid "$pid" | grep -v "^$wid$" | head -1 || true)"
    [ -n "$torn" ] && break
    sleep 0.1
  done
  if [ -z "$torn" ]; then
    xdotool mouseup 1
    echo "tearoff live smoke [$label]: drag past the threshold never tore a window off" >&2
    exit 1
  fi
  ctl screenshot --json "{\"full_window\":true,\"path\":\"$out/torn-$label.png\"}" >/dev/null 2>&1 || true

  # --- Freeze guard: the torn window must FOLLOW the held pointer. ---
  read -r t0x t0y <<<"$(client_xy "$torn")"
  xdotool mousemove --sync $((press_x + 300)) $((press_y + tear_px + 200))
  sleep 0.8
  read -r t1x t1y <<<"$(client_xy "$torn")"
  moved="$(python3 -c "print(abs($t1x-$t0x) + abs($t1y-$t0y))")"
  if [ "$moved" -lt 150 ]; then
    xdotool mouseup 1
    echo "tearoff live smoke [$label]: torn window froze mid-drag (moved ${moved}px for a 500px pointer travel)" >&2
    exit 1
  fi

  # --- Re-dock: carry onto window 1's band, release, expect a merge. ---
  read -r wx wy <<<"$(client_xy "$wid")"
  band_cx=$((wx + seg2_cx))
  band_cy=$((wy + ${bar_h%.*} / 2))
  xdotool mousemove --sync $((band_cx + 150)) $((band_cy + 200))
  sleep 0.2
  xdotool mousemove --sync "$band_cx" "$band_cy"
  sleep 0.9
  xdotool mouseup 1
  sleep 0.3
  xdotool mousemove --sync $((band_cx + 3)) $((band_cy + 2))
  xdotool mousemove --sync $((band_cx + 6)) $((band_cy + 4))
  merged=""
  deadline=$((SECONDS + 6))
  while [ "$SECONDS" -lt "$deadline" ]; do
    read -r ntabs nwins <<<"$(tab_windows)"
    if [ "$ntabs" = "2" ] && [ "$nwins" = "1" ]; then
      merged=yes
      break
    fi
    sleep 0.2
  done
  if [ -z "$merged" ]; then
    echo "tearoff live smoke [$label]: drop on the tab band did not merge back ($ntabs tabs, $nwins windows)" >&2
    echo "events tail:" >&2
    tail -5 "$out/events-$label.ndjson" >&2 || true
    exit 1
  fi
  ctl screenshot --json "{\"full_window\":true,\"path\":\"$out/merged-$label.png\"}" >/dev/null 2>&1 || true

  if ! grep -q '"tab_moved"' "$out/events-$label.ndjson"; then
    echo "tearoff live smoke [$label]: no tab_moved event was broadcast" >&2
    exit 1
  fi

  kill "$events_pid" 2>/dev/null || true
  events_pid=""
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=""
  echo "tearoff live smoke [$label]: pass"
}

gesture_pass native 1
gesture_pass manual-follow 0 KETTLE_TEAR_MANUAL_FOLLOW=1

python3 - "$out" <<'PY'
import json, sys, pathlib
out = pathlib.Path(sys.argv[1])
analysis = {"passes": {}}
for label in ("native", "manual-follow"):
    events_file = out / f"events-{label}.ndjson"
    events = [json.loads(l) for l in events_file.read_text().splitlines() if l.strip()]
    analysis["passes"][label] = {
        "tab_moved_events": [e for e in events if e.get("event") == "tab_moved"],
        "checks": (["esc-cancel"] if label == "native" else []) + ["tear", "follow", "redock-merge"],
    }
(out / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
PY
echo "tearoff live smoke: OK artifacts=$out"
