#!/usr/bin/env bash
set -euo pipefail

KETTLE="${KETTLE_BIN:-kettle}"
FRAMES="${KETTLE_UNDERLINE_FRAMES:-8}"
TIMEOUT="${KETTLE_UNDERLINE_TIMEOUT:-25}"
SCROLL_DOWN_KEYS="${KETTLE_UNDERLINE_DOWN_KEYS:-j,j,j,j,j,j}"
SCROLL_UP_KEYS="${KETTLE_UNDERLINE_UP_KEYS:-k,k,k,k,k,k}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

python3 "$SCRIPT_DIR/check-live-ui-smoke.py" session-check
if ! command -v git >/dev/null 2>&1; then
  echo "underline-scroll smoke: cannot run (git not found)" >&2
  exit 1
fi
if ! command -v delta >/dev/null 2>&1; then
  echo "underline-scroll smoke: cannot run (delta not found)" >&2
  exit 1
fi

stamp="$(date +%Y%m%d-%H%M%S)"
out="${KETTLE_DIAG_DIR:-target/diagnostics}/underline-scroll-$stamp"
case "$out" in
  /*) ;;
  *) out="$(pwd)/$out" ;;
esac
repo="$out/repo"
svn_checkout="$out/svn-checkout"
svn_enabled=0
mkdir -p "$repo"
pid=""
cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

(
  cd "$repo"
  git init -q
  git config user.email kettle-smoke@example.invalid
  git config user.name "Kettle Smoke"
  for i in $(seq 1 180); do
    printf 'stable line %03d\n' "$i"
  done > fixture.txt
  git add fixture.txt
  git commit -q -m base
  for i in $(seq 1 180); do
    if [ $((i % 3)) -eq 0 ]; then
      printf 'changed underlined token_%03d and link https://example.invalid/%03d\n' "$i" "$i"
    else
      printf 'stable line %03d\n' "$i"
    fi
  done > fixture.txt
)

if command -v svn >/dev/null 2>&1 && command -v svnadmin >/dev/null 2>&1; then
  svn_repo="$out/svnrepo"
  svnadmin create "$svn_repo"
  svn checkout "file://$svn_repo" "$svn_checkout" >/dev/null
  (
    cd "$svn_checkout"
    for i in $(seq 1 180); do
      printf 'stable svn line %03d\n' "$i"
    done > fixture.txt
    svn add fixture.txt >/dev/null
    svn commit -m base >/dev/null
    for i in $(seq 1 180); do
      if [ $((i % 4)) -eq 0 ]; then
        printf 'svn changed underlined token_%03d and link https://example.invalid/svn/%03d\n' "$i" "$i"
      else
        printf 'stable svn line %03d\n' "$i"
      fi
    done > fixture.txt
  )
  svn_enabled=1
fi

cfg="$out/config"
cat > "$cfg" <<'CFG'
agent-server = full
text-renderer = grid
tab-bar = off
status-bar = off
restore-session = false
update-check = false
background = #080808
foreground = #f8f8f8
minimum-contrast = 0
window-padding-x = 0
window-padding-y = 0
window-width = 100
window-height = 32
CFG

"$KETTLE" --config "$cfg" --agent-server full >"$out/kettle.log" 2>&1 &
pid="$!"

deadline=$((SECONDS + TIMEOUT))
while ! "$KETTLE" ctl --pid "$pid" list_panes --raw >"$out/panes.json" 2>/dev/null; do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "underline-scroll smoke: kettle exited before control server came up" >&2
    cat "$out/kettle.log" >&2 || true
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "underline-scroll smoke: timed out waiting for control server" >&2
    cat "$out/kettle.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done

ctl_screenshot() {
  local path="$1"
  if ! timeout 8 "$KETTLE" ctl --pid "$pid" screenshot --json "{\"full_window\":true,\"path\":\"$path\"}" >/dev/null; then
    echo "underline-scroll smoke: screenshot timed out for $path" >&2
    echo "underline-scroll smoke: artifacts preserved at $out" >&2
    exit 1
  fi
}

svn_marker=""
svn_diff_part=""
if [ "$svn_enabled" -eq 1 ]; then
  svn_marker="printf 'SVN_DELTA_FIXTURE_BEGIN\n';"
  svn_diff_part="( cd '$svn_checkout' && svn diff | delta --paging=never --line-numbers );"
fi
cmd="cd '$repo' && { printf 'GIT_DELTA_FIXTURE_BEGIN\n'; $svn_marker for i in \$(seq 1 120); do if [ \$((i % 2)) -eq 1 ]; then printf '\033[4mUNDERLINE_%s_%03d\033[24m link https://example.invalid/%03d\n' SENTINEL \"\$i\" \"\$i\"; else printf 'PLAIN_%s_%03d link https://example.invalid/%03d\n' SENTINEL \"\$i\" \"\$i\"; fi; done; git diff --color=always | delta --paging=never --line-numbers; $svn_diff_part } | less -R"
"$KETTLE" ctl --pid "$pid" send_text --text "$cmd" >/dev/null
"$KETTLE" ctl --pid "$pid" send_keys --keys enter >/dev/null
"$KETTLE" ctl --pid "$pid" wait_for --text "GIT_DELTA_FIXTURE_BEGIN" --json '{"timeout_ms":8000,"quiet_ms":250}' >/dev/null
if [ "$svn_enabled" -eq 1 ]; then
  "$KETTLE" ctl --pid "$pid" wait_for --text "SVN_DELTA_FIXTURE_BEGIN" --json '{"timeout_ms":8000,"quiet_ms":250}' >/dev/null
fi
"$KETTLE" ctl --pid "$pid" wait_for --text "UNDERLINE_SENTINEL" --json '{"timeout_ms":8000,"quiet_ms":250}' >/dev/null

for i in $(seq 1 "$FRAMES"); do
  "$KETTLE" ctl --pid "$pid" ui_geometry --raw >"$out/geometry-$i.json"
  "$KETTLE" ctl --pid "$pid" read_cells --raw >"$out/cells-$i.json"
  ctl_screenshot "$out/frame-$i.png"
  if [ "$i" -lt "$FRAMES" ]; then
    if [ "$i" -lt $((FRAMES / 2 + 1)) ]; then
      keys="$SCROLL_DOWN_KEYS"
    else
      keys="$SCROLL_UP_KEYS"
    fi
  else
    keys=""
  fi
  if [ -n "$keys" ] && ! timeout 5 "$KETTLE" ctl --pid "$pid" send_keys --keys "$keys" >/dev/null; then
    echo "underline-scroll smoke: send_keys timed out for keys '$keys'" >&2
    echo "underline-scroll smoke: artifacts preserved at $out" >&2
    exit 1
  fi
  sleep 0.08
done

python3 - "$out" "$FRAMES" <<'PY'
import json
import re
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

def bright_at(rgba_rows, x, y):
    if y < 0 or y >= len(rgba_rows) or x < 0:
        return False
    row = rgba_rows[y]
    if x * 4 + 3 >= len(row):
        return False
    off = x * 4
    r, g, b, a = row[off : off + 4]
    return a > 0 and (r * 299 + g * 587 + b * 114) >= 140_000

out = Path(sys.argv[1])
frames = int(sys.argv[2])
underline_frames = 0
top_sentinels = []
analysis = []
for i in range(1, frames + 1):
    data = json.loads((out / f"cells-{i}.json").read_text())
    cells = data.get("cells", [])
    cols = max(1, int(data.get("cols", 1)))
    screen_rows = max(1, int(data.get("rows", 1)))
    rows = {}
    underline_rows = set()
    underline_cols = {}
    for c in cells:
        rows.setdefault(c["row"], []).append((c["col"], c.get("ch", "")))
        if c.get("any_underline"):
            underline_rows.add(c["row"])
            underline_cols.setdefault(c["row"], []).append(c["col"])
    if underline_rows:
        underline_frames += 1
    found = []
    plain_found = []
    for row, row_cells in sorted(rows.items()):
        text = "".join(ch for _, ch in sorted(row_cells))
        match = re.search(r"UNDERLINE_SENTINEL_(\d+)", text)
        if match:
            found.append((row, int(match.group(1))))
        plain_match = re.search(r"PLAIN_SENTINEL_(\d+)", text)
        if plain_match:
            plain_found.append((row, int(plain_match.group(1))))
    if not found:
        raise SystemExit(f"underline-scroll smoke: no sentinel text visible in cells-{i}.json")
    top_sentinels.append(found[0][1])
    png_path = out / f"frame-{i}.png"
    if not png_path.exists():
        raise SystemExit(f"underline-scroll smoke: missing frame-{i}.png")
    width, height, rgba_rows = read_rgba_png(png_path)
    geometry = json.loads((out / f"geometry-{i}.json").read_text())
    content = geometry.get("content", {})
    cell = geometry.get("cell", {})
    origin_x = float(content.get("x", 0.0))
    origin_y = float(content.get("y", 0.0))
    cell_w = float(cell.get("width") or (float(content.get("width", width)) / cols))
    cell_h = float(cell.get("height") or (float(content.get("height", height)) / screen_rows))
    pixel_rows = []
    for row, number in found[:8]:
        cols_for_row = sorted(underline_cols.get(row, []))
        if not cols_for_row:
            raise SystemExit(f"underline-scroll smoke: sentinel row {row} has no underline attrs")
        sample_cols = cols_for_row[: min(22, len(cols_for_row))]
        baseline = int(round(origin_y + row * cell_h + cell_h - 2.0))
        best = 0
        best_y = baseline
        for y in range(baseline - 2, baseline + 3):
            hits = 0
            for col in sample_cols:
                x = int(origin_x + (col + 0.5) * cell_w)
                if bright_at(rgba_rows, x, y):
                    hits += 1
            if hits > best:
                best = hits
                best_y = y
        min_hits = max(8, int(len(sample_cols) * 0.60))
        if best < min_hits:
            raise SystemExit(
                "underline-scroll smoke: rendered underline is not aligned with "
                f"frame={i} row={row} sentinel={number} hits={best}/{len(sample_cols)} "
                f"baseline={baseline}"
            )
        pixel_rows.append({
            "row": row,
            "sentinel": number,
            "underline_pixel_hits": best,
            "sampled_columns": len(sample_cols),
            "pixel_y": best_y,
        })
    plain_pixel_rows = []
    for row, number in plain_found[:8]:
        sample_cols = list(range(0, 18))
        baseline = int(round(origin_y + row * cell_h + cell_h - 2.0))
        best = 0
        best_y = baseline
        for y in range(baseline - 2, baseline + 3):
            hits = 0
            for col in sample_cols:
                x = int(origin_x + (col + 0.5) * cell_w)
                if bright_at(rgba_rows, x, y):
                    hits += 1
            if hits > best:
                best = hits
                best_y = y
        max_plain_hits = max(16, int(len(sample_cols) * 0.90))
        if best > max_plain_hits:
            raise SystemExit(
                "underline-scroll smoke: rendered underline leaked onto plain row: "
                f"frame={i} row={row} sentinel={number} hits={best}/{len(sample_cols)} "
                f"baseline={baseline}"
            )
        plain_pixel_rows.append({
            "row": row,
            "sentinel": number,
            "baseline_pixel_hits": best,
            "sampled_columns": len(sample_cols),
            "pixel_y": best_y,
            "near_solid_threshold": max_plain_hits,
        })
    analysis.append({
        "frame": i,
        "top_sentinel": found[0][1],
        "cell": {"width": cell_w, "height": cell_h},
        "content": content,
        "underline_rows": sorted(underline_rows),
        "sentinels": [{"row": row, "number": number} for row, number in found],
        "plain_sentinels": [{"row": row, "number": number} for row, number in plain_found],
        "pixel_rows": pixel_rows,
        "plain_pixel_rows": plain_pixel_rows,
    })
if underline_frames == 0:
    raise SystemExit("underline-scroll smoke: no underlined cells observed in delta fixture")
(out / "analysis.json").write_text(json.dumps({
    "frames": frames,
    "underline_frames": underline_frames,
    "top_sentinels": top_sentinels,
    "delta_fixtures": {
        "git": True,
        "svn": (out / "svn-checkout").exists(),
    },
    "frames_detail": analysis,
}, indent=2) + "\n")
mid = frames // 2
if frames >= 4:
    if not top_sentinels[0] < top_sentinels[mid]:
        raise SystemExit(
            "underline-scroll smoke: down-scroll did not advance underlined text: "
            f"{top_sentinels}"
        )
    if not top_sentinels[-1] < top_sentinels[mid]:
        raise SystemExit(
            "underline-scroll smoke: up-scroll did not move underlined text back: "
            f"{top_sentinels}"
        )
print(
    "underline-scroll smoke: OK "
    f"frames={frames} underline_frames={underline_frames} "
    f"top_sentinels={top_sentinels} artifacts={out}"
)
PY
