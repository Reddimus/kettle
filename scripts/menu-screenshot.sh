#!/usr/bin/env bash
# Repro harness for the Terminator-style right-click
# context menu.
#
# Launches a real kettle window, drives xdotool to right-click near
# the screen center, waits a beat for the menu to paint, then captures
# the whole screen via scrot. The resulting PNG lands in
# target/menu-shots/ for diffing against prior runs as further
# context-menu work lands.
#
# Why interactive rather than --screenshot:
#   kettle's --screenshot flag captures the surface at a single point
#   in time but doesn't drive UI state — there's no flag to open the
#   context menu before the snapshot fires. xdotool + scrot lets us
#   exercise the actual mouse path the user takes.
#
# Skipped automatically when:
#   - $DISPLAY is unset (CI / headless)
#   - scrot or xdotool aren't on PATH
#   - the kettle binary isn't built yet (gives a hint to run `just build`)
#
# Usage:
#   ./scripts/menu-screenshot.sh                       # captures default
#   ./scripts/menu-screenshot.sh --name baseline       # named output
#   ./scripts/menu-screenshot.sh --hold                # leave kettle running
#                                                       so the operator can
#                                                       drive it manually.

set -euo pipefail

NAME="menu"
HOLD=0
while [ $# -gt 0 ]; do
    case "$1" in
        --name) NAME="$2"; shift 2 ;;
        --hold) HOLD=1; shift ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

# --- Preflight ------------------------------------------------------

if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo "menu-screenshot: no \$DISPLAY (or \$WAYLAND_DISPLAY) — headless env, skipping." >&2
    exit 0
fi

for tool in scrot xdotool; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "menu-screenshot: missing \`$tool\`. Install with: sudo apt install scrot xdotool" >&2
        exit 1
    fi
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KETTLE_BIN="$REPO_ROOT/target/release/kettle"
if [ ! -x "$KETTLE_BIN" ]; then
    if [ -x "$REPO_ROOT/target/debug/kettle" ]; then
        KETTLE_BIN="$REPO_ROOT/target/debug/kettle"
    else
        echo "menu-screenshot: kettle binary not built. Run \`cargo build --release -p kettle\` first." >&2
        exit 1
    fi
fi

OUT_DIR="$REPO_ROOT/target/menu-shots"
mkdir -p "$OUT_DIR"
TS="$(date +%Y%m%d-%H%M%S)"
OUT_PATH="$OUT_DIR/${NAME}-${TS}.png"

# --- Drive ---------------------------------------------------------

echo "menu-screenshot: launching $KETTLE_BIN" >&2
"$KETTLE_BIN" &
KETTLE_PID=$!

cleanup() {
    if [ "$HOLD" -ne 1 ] && kill -0 "$KETTLE_PID" 2>/dev/null; then
        kill "$KETTLE_PID" 2>/dev/null || true
        wait "$KETTLE_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# Give kettle's wgpu surface ~1.2s to initialize + paint the first
# frame. Slower hosts may need a bump.
sleep 1.2

# Find the kettle window id. xdotool's --name uses regex; kettle's
# default title is "kettle" or "~ — kettle".
WID="$(xdotool search --name '\bkettle\b' 2>/dev/null | tail -n1 || true)"
if [ -z "$WID" ]; then
    echo "menu-screenshot: couldn't find kettle window (xdotool search returned nothing)" >&2
    exit 1
fi

# Raise + focus + right-click near the center.
xdotool windowactivate --sync "$WID"
xdotool windowsize --sync "$WID" 1280 720 2>/dev/null || true

# Compute window geometry — right-click 320px in from left, 240px down
# from top so the menu has room to open downward + rightward.
eval "$(xdotool getwindowgeometry --shell "$WID")"
CLICK_X=$((X + 320))
CLICK_Y=$((Y + 240))

xdotool mousemove "$CLICK_X" "$CLICK_Y" click 3

# Wait for the menu to render its first frame.
sleep 0.35

scrot -o -u "$OUT_PATH"
echo "menu-screenshot: wrote $OUT_PATH" >&2

if [ "$HOLD" -eq 1 ]; then
    echo "menu-screenshot: --hold set; kettle PID=$KETTLE_PID still running. Press Enter to terminate." >&2
    read -r _
fi
