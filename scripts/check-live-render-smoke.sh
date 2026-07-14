#!/usr/bin/env bash
set -euo pipefail

# Local live-render smoke for the grid text renderer.
#
# This needs a real desktop session. It starts a temporary kettle window with
# the control server enabled, captures several live screenshots across cursor
# blink phases, decodes the PNGs with Python stdlib only, and fails if the
# prompt-like marker is absent from rendered frames or if blink changes a broad
# region instead of a cursor-sized box.

KETTLE="${KETTLE_BIN:-kettle}"
FRAMES="${KETTLE_LIVE_RENDER_FRAMES:-6}"
SLEEP_SECS="${KETTLE_LIVE_RENDER_SLEEP:-0.20}"
TIMEOUT="${KETTLE_LIVE_RENDER_TIMEOUT:-20}"

if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
  echo "live-render smoke: skipped (no DISPLAY or WAYLAND_DISPLAY)" >&2
  exit 0
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/kettle-live-render.XXXXXX")"
pid=""
cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

cfg="$tmp/config"
cat > "$cfg" <<'CFG'
text-renderer = grid
agent-server = full
cursor-blink = true
cursor-blink-interval = 250
background = #000000
foreground = #ffffff
cursor-color = #ffff00
cursor-fg-color = #000000
minimum-contrast = 0
tab-bar = off
status-bar = off
restore-session = false
update-check = false
CFG

"$KETTLE" --config "$cfg" --agent-server full >"$tmp/kettle.log" 2>&1 &
pid="$!"

deadline=$((SECONDS + TIMEOUT))
while ! "$KETTLE" ctl --pid "$pid" list_panes --raw >"$tmp/panes.json" 2>/dev/null; do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "live-render smoke: kettle exited before control server came up" >&2
    cat "$tmp/kettle.log" >&2 || true
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "live-render smoke: timed out waiting for control server" >&2
    cat "$tmp/kettle.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done

# The control socket can become ready while an interactive shell is still
# running startup hooks. A late prompt/theme redraw may then clear a command
# that was successfully written and briefly observed. Require the marker in a
# final snapshot and retry within the existing launch deadline.
for attempt in 1 2 3; do
  "$KETTLE" ctl --pid "$pid" send_text --text "printf '\342\236\234  ~ KETTLE_LIVE_RENDER_%s' SMOKE" >/dev/null
  "$KETTLE" ctl --pid "$pid" send_keys --keys enter >/dev/null
  "$KETTLE" ctl --pid "$pid" wait_for --text "KETTLE_LIVE_RENDER_SMOKE" \
    --json '{"timeout_ms":5000,"quiet_ms":250}' >/dev/null
  "$KETTLE" ctl --pid "$pid" read_screen --raw >"$tmp/screen.json"
  if python3 - "$tmp/screen.json" <<'PY'
import json
import sys

screen = json.loads(open(sys.argv[1], encoding="utf-8").read())
raise SystemExit("KETTLE_LIVE_RENDER_SMOKE" not in screen.get("text", ""))
PY
  then
    break
  fi
  if [ "$attempt" -lt 3 ]; then
    sleep 0.25
  fi
done

for i in $(seq 1 "$FRAMES"); do
  "$KETTLE" ctl --pid "$pid" screenshot --json "{\"path\":\"$tmp/frame-$i.png\"}" >/dev/null
  sleep "$SLEEP_SECS"
done

python3 - "$tmp" "$FRAMES" "$tmp/screen.json" <<'PY'
import json
import struct
import sys
import zlib
from pathlib import Path

tmp = Path(sys.argv[1])
frame_count = int(sys.argv[2])
screen = json.loads(Path(sys.argv[3]).read_text())
cols = max(1, int(screen.get("cols", 1)))
rows = max(1, int(screen.get("rows", 1)))
screen_text = screen.get("text", "")
if "KETTLE_LIVE_RENDER_SMOKE" not in screen_text:
    raise SystemExit("live-render smoke: marker text is not present on screen")
if "\u279c  ~ KETTLE_LIVE_RENDER_SMOKE" not in screen_text:
    raise SystemExit("live-render smoke: prompt-shaped marker is not present on screen")


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
                raise SystemExit(
                    f"{path}: expected non-interlaced 8-bit RGBA PNG, got "
                    f"bit_depth={bit_depth} color_type={color_type} interlace={interlace}"
                )
        elif typ == b"IDAT":
            raw += chunk
        elif typ == b"IEND":
            break
    if width is None or height is None:
        raise SystemExit(f"{path}: missing IHDR")
    decoded = zlib.decompress(raw)
    bpp = 4
    stride = width * bpp
    rows_out = []
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
        rows_out.append(bytes(recon))
        prev = recon
    return width, height, rows_out


frames = [read_rgba_png(tmp / f"frame-{i}.png") for i in range(1, frame_count + 1)]
width, height, _ = frames[0]
if any((w, h) != (width, height) for w, h, _ in frames):
    raise SystemExit("live-render smoke: screenshot sizes changed across frames")

cell_w = max(1.0, width / cols)
cell_h = max(1.0, height / rows)
max_w = cell_w * 4.0
max_h = cell_h * 3.0
max_changed = cell_w * cell_h * 8.0
min_ink_pixels = max(200, int(cell_w * cell_h * 3.0))
nonzero_pairs = 0
worst = (0, None, 0, 0, 0)
ink_counts = []

for frame_idx, (_, _, rgba_rows) in enumerate(frames, start=1):
    ink = 0
    for row in rgba_rows:
        for x in range(width):
            off = x * 4
            r, g, b, a = row[off : off + 4]
            # The temp config is white text on black bg with a yellow cursor.
            # This rejects blank/mostly-empty frames without depending on OCR.
            if a > 0 and (r * 299 + g * 587 + b * 114) >= 80_000:
                ink += 1
    ink_counts.append(ink)
    if ink < min_ink_pixels:
        raise SystemExit(
            "live-render smoke: rendered frame has too little visible text: "
            f"frame={frame_idx} ink_pixels={ink} min={min_ink_pixels}"
        )

for idx in range(len(frames) - 1):
    _, _, a = frames[idx]
    _, _, b = frames[idx + 1]
    changed = []
    for y, (ra, rb) in enumerate(zip(a, b)):
        for x in range(width):
            off = x * 4
            if ra[off : off + 4] != rb[off : off + 4]:
                changed.append((x, y))
    if not changed:
        continue
    nonzero_pairs += 1
    xs = [x for x, _ in changed]
    ys = [y for _, y in changed]
    bbox = (min(xs), min(ys), max(xs) + 1, max(ys) + 1)
    bw = bbox[2] - bbox[0]
    bh = bbox[3] - bbox[1]
    if len(changed) > worst[0]:
        worst = (len(changed), bbox, bw, bh, idx + 1)
    if bw > max_w or bh > max_h or len(changed) > max_changed:
        raise SystemExit(
            "live-render smoke: blink changed too much outside a cursor-sized area: "
            f"pair={idx + 1}->{idx + 2} changed={len(changed)} bbox={bbox} "
            f"cell=({cell_w:.2f},{cell_h:.2f})"
        )

if nonzero_pairs == 0:
    raise SystemExit("live-render smoke: no blink-phase pixel changes observed")

changed, bbox, bw, bh, pair = worst
print(
    "live-render smoke: OK "
    f"frames={frame_count} worst_pair={pair}->{pair + 1} changed={changed} "
    f"bbox={bbox} bbox_size=({bw}x{bh}) "
    f"ink=min:{min(ink_counts)} max:{max(ink_counts)} image=({width}x{height})"
)
PY
