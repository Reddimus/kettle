#!/usr/bin/env bash
set -euo pipefail

KETTLE="${KETTLE_BIN:-kettle}"
FRAMES="${KETTLE_UNDERLINE_FRAMES:-2}"
TIMEOUT="${KETTLE_UNDERLINE_TIMEOUT:-25}"
SCROLL_KEYS="${KETTLE_UNDERLINE_KEYS:-j,j,j,j,j,j}"

if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
  echo "underline-scroll smoke: skipped (no DISPLAY or WAYLAND_DISPLAY)" >&2
  exit 0
fi
if ! command -v git >/dev/null 2>&1; then
  echo "underline-scroll smoke: skipped (git not found)" >&2
  exit 0
fi
if ! command -v delta >/dev/null 2>&1; then
  echo "underline-scroll smoke: skipped (delta not found)" >&2
  exit 0
fi

stamp="$(date +%Y%m%d-%H%M%S)"
out="${KETTLE_DIAG_DIR:-target/diagnostics}/underline-scroll-$stamp"
case "$out" in
  /*) ;;
  *) out="$(pwd)/$out" ;;
esac
repo="$out/repo"
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

cmd="cd '$repo' && { for i in \$(seq 1 120); do printf '\033[4mUNDERLINE_SENTINEL_%03d\033[24m link https://example.invalid/%03d\n' \"\$i\" \"\$i\"; done; git diff --color=always | delta --paging=never --line-numbers; } | less -R"
"$KETTLE" ctl --pid "$pid" send_text --text "$cmd" >/dev/null
"$KETTLE" ctl --pid "$pid" send_keys --keys enter >/dev/null
"$KETTLE" ctl --pid "$pid" wait_for --text "UNDERLINE_SENTINEL" --json '{"timeout_ms":8000,"quiet_ms":250}' >/dev/null

for i in $(seq 1 "$FRAMES"); do
  "$KETTLE" ctl --pid "$pid" read_cells --raw >"$out/cells-$i.json"
  ctl_screenshot "$out/frame-$i.png"
  if ! timeout 5 "$KETTLE" ctl --pid "$pid" send_keys --keys "$SCROLL_KEYS" >/dev/null; then
    echo "underline-scroll smoke: send_keys timed out for keys '$SCROLL_KEYS'" >&2
    echo "underline-scroll smoke: artifacts preserved at $out" >&2
    exit 1
  fi
  sleep 0.08
done

python3 - "$out" "$FRAMES" <<'PY'
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])
frames = int(sys.argv[2])
underline_frames = 0
row_sets = []
for i in range(1, frames + 1):
    data = json.loads((out / f"cells-{i}.json").read_text())
    cells = data.get("cells", [])
    rows = sorted({c["row"] for c in cells if c.get("any_underline")})
    if rows:
        underline_frames += 1
    row_sets.append(rows)
    if not (out / f"frame-{i}.png").exists():
        raise SystemExit(f"underline-scroll smoke: missing frame-{i}.png")
if underline_frames == 0:
    raise SystemExit("underline-scroll smoke: no underlined cells observed in delta fixture")
print(f"underline-scroll smoke: OK frames={frames} underline_frames={underline_frames} artifacts={out}")
PY
