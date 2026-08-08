#!/usr/bin/env bash
# Terminator parity: drag a terminal to another position inside its tab.
#
# Drives the real gesture through the control plane -- press on a pane's own
# titlebar, move, release -- and asserts on `ui_geometry`, which reports the
# gesture in the same armed/live shape as the tab drag. The pure geometry
# (`pane_drop_zone`, `pane_drop_preview`) is unit-tested in mux.rs; what only a
# live window can show is that the press REACHES that geometry: that the
# titlebar hit-test, the slop threshold, the drop latch, and the release all
# agree about which pane is being carried and where it lands.
set -euo pipefail

KETTLE="${KETTLE_BIN:-kettle}"
TIMEOUT="${KETTLE_PANE_DRAG_TIMEOUT:-20}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

python3 "$SCRIPT_DIR/check-live-ui-smoke.py" session-check

stamp="$(date +%Y%m%d-%H%M%S)"
out="${KETTLE_DIAG_DIR:-target/diagnostics}/pane-drag-$stamp"
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
show-titlebar = true
title-at-bottom = false
tab-bar = never
status-bar = off
restore-session = false
update-check = false
background = #101010
foreground = #f4f4f4
window-width = 200
window-height = 50
CFG

"$KETTLE" --config "$cfg" --agent-server full >"$out/kettle.log" 2>&1 &
pid="$!"

deadline=$((SECONDS + TIMEOUT))
while ! "$KETTLE" ctl --pid "$pid" list_tabs --raw >"$out/tabs-0.json" 2>/dev/null; do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "pane-drag smoke: kettle exited before control server came up" >&2
    cat "$out/kettle.log" >&2 || true
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "pane-drag smoke: timed out waiting for control server" >&2
    cat "$out/kettle.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done

geometry() {
  "$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-$1.json"
}

mouse() {
  "$KETTLE" ctl --pid "$pid" send_mouse --json "$1" >/dev/null
}

# Three panes: a left column split into two rows, so the tree has somewhere to
# collapse when the dragged pane is lifted. A two-pane fixture would make the
# lift trivial (the root collapses to the survivor) and would not exercise the
# lift-before-graft ordering the tree half depends on.
"$KETTLE" ctl --pid "$pid" perform_action --text split_right >/dev/null
sleep 0.3
"$KETTLE" ctl --pid "$pid" perform_action --text go_left >/dev/null
sleep 0.2
"$KETTLE" ctl --pid "$pid" perform_action --text split_down >/dev/null
sleep 0.3
geometry laid-out

python3 - "$out" <<'PY'
import json, sys
from pathlib import Path

out = Path(sys.argv[1])
g = json.loads((out / "geometry-laid-out.json").read_text())
panes = g.get("panes") or []
if len(panes) != 3:
    raise SystemExit(f"pane-drag smoke: expected 3 panes, got {panes}")
# The pane rects must be disjoint, or every point below is ambiguous and the
# whole check proves nothing about which pane the cursor picked.
for i, a in enumerate(panes):
    for b in panes[i + 1:]:
        ra, rb = a["rect"], b["rect"]
        overlap_w = min(ra["x"] + ra["width"], rb["x"] + rb["width"]) - max(ra["x"], rb["x"])
        overlap_h = min(ra["y"] + ra["height"], rb["y"] + rb["height"]) - max(ra["y"], rb["y"])
        if overlap_w > 0.5 and overlap_h > 0.5:
            raise SystemExit(f"pane-drag smoke: pane rects overlap: {ra} {rb}")
(out / "panes.json").write_text(json.dumps(panes))
PY

# Grab the FOCUSED pane by its titlebar. The band sits at the top of the pane
# rect (`title-at-bottom = false`), `cell.height + 6` tall; aim at its middle.
press_json="$(python3 - "$out" <<'PY'
import json, sys
from pathlib import Path

out = Path(sys.argv[1])
g = json.loads((out / "geometry-laid-out.json").read_text())
panes = json.loads((out / "panes.json").read_text())
moving = next(p for p in panes if p["focused"])
bar_h = g["cell"]["height"] + 6.0
r = moving["rect"]
print(json.dumps({
    "event": "press",
    "x": r["x"] + r["width"] / 2,
    "y": r["y"] + bar_h / 2,
    "button": "left",
    "_pane": moving["id"],
}))
PY
)"
moving_pane="$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["_pane"])' "$press_json")"
mouse "$(python3 -c 'import json,sys;d=json.loads(sys.argv[1]);d.pop("_pane");print(json.dumps(d))' "$press_json")"
sleep 0.2
geometry pressed

# Jitter well inside the slop radius: still a click, not yet a drag.
mouse "$(python3 - "$press_json" <<'PY'
import json, sys
d = json.loads(sys.argv[1])
print(json.dumps({"event": "move", "x": d["x"] + 4.0, "y": d["y"] + 2.0}))
PY
)"
sleep 0.2
geometry jittered

# Now move onto the RIGHT column's right half -- past the slop, and squarely in
# one drop zone.
mouse "$(python3 - "$out" "$moving_pane" <<'PY'
import json, sys
from pathlib import Path

out, moving = Path(sys.argv[1]), int(sys.argv[2])
panes = json.loads((out / "panes.json").read_text())
others = [p for p in panes if p["id"] != moving]
# The widest other pane, so the "right quarter" point is unambiguous.
target = max(others, key=lambda p: p["rect"]["width"])
r = target["rect"]
(out / "target.json").write_text(json.dumps(target))
print(json.dumps({
    "event": "move",
    "x": r["x"] + r["width"] * 0.88,
    "y": r["y"] + r["height"] / 2,
}))
PY
)"
sleep 0.2
geometry dragging

mouse "$(python3 - "$out" <<'PY'
import json, sys
from pathlib import Path
g = json.loads((Path(sys.argv[1]) / "geometry-dragging.json").read_text())
x, y = g["cursor"]
print(json.dumps({"event": "release", "x": x, "y": y, "button": "left"}))
PY
)"
sleep 0.3
geometry released

python3 - "$out" "$moving_pane" <<'PY'
import json, sys
from pathlib import Path

out, moving = Path(sys.argv[1]), int(sys.argv[2])
def g(name):
    return json.loads((out / f"geometry-{name}.json").read_text())

pressed, jittered, dragging, released = g("pressed"), g("jittered"), g("dragging"), g("released")
target = json.loads((out / "target.json").read_text())

if not pressed.get("pane_drag_armed"):
    raise SystemExit("pane-drag smoke: press on a pane titlebar did not arm the gesture")
if pressed.get("pane_drag_live"):
    raise SystemExit("pane-drag smoke: a press with no movement became a live drag")
if pressed.get("pane_drag_pane") != moving:
    raise SystemExit(
        f"pane-drag smoke: armed on pane {pressed.get('pane_drag_pane')}, pressed on {moving}"
    )
if not jittered.get("pane_drag_armed") or jittered.get("pane_drag_live"):
    raise SystemExit("pane-drag smoke: jitter inside the slop radius promoted to a drag")
if jittered.get("pane_drag_target") is not None:
    raise SystemExit("pane-drag smoke: a gesture that is not live must offer no drop target")

if not dragging.get("pane_drag_live"):
    raise SystemExit("pane-drag smoke: movement past the slop radius did not promote to a drag")
got = dragging.get("pane_drag_target")
if got is None:
    raise SystemExit("pane-drag smoke: a live drag over another pane latched no drop target")
if got.get("pane") != target["id"] or got.get("edge") != "right":
    raise SystemExit(
        f"pane-drag smoke: expected the right edge of pane {target['id']}, got {got}"
    )

if released.get("pane_drag_armed") or released.get("pane_drag_live"):
    raise SystemExit("pane-drag smoke: the release left the gesture armed")

before = [p["id"] for p in json.loads((out / "panes.json").read_text())]
after = [p["id"] for p in released.get("panes") or []]
if sorted(after) != sorted(before):
    raise SystemExit(f"pane-drag smoke: panes gained or lost by the move: {before} -> {after}")
if after == before:
    raise SystemExit(
        f"pane-drag smoke: the drop did not reorder anything (still {after}) -- "
        "a released drag with a latched target must move the pane"
    )
# Dropped on the target's RIGHT edge, so it must now sit after it.
if after.index(moving) < after.index(target["id"]):
    raise SystemExit(
        f"pane-drag smoke: dropped right of pane {target['id']} but landed before it: {after}"
    )
print("pane-drag smoke: ok")
PY
