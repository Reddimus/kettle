#!/usr/bin/env bash
# scripts/bench.sh — reproduce the numbers in docs/PERFORMANCE.md.
#
# Builds a release binary if one isn't present, then runs three
# measurements 5 times each:
#   - `kettle --version`     (cold-cache startup floor)
#   - `kettle --screenshot`  (full GPU pipeline boot + render)
#   - `kettle --screenshot-menu` (GPU pipeline + cycle-251 menu pass)
#
# Output format per row: `<wall-clock>s, <peak RSS in MB>` so the
# spread across runs is visible at a glance. Pipe to a file or
# /tmp/bench.txt for a snapshot to attach to a PR.
#
# Requires `time` (GNU coreutils — on macOS install via
# `brew install coreutils` and call as `gtime`). Inherits the
# repo-root `target/release/kettle` if present; otherwise builds it
# via `cargo build --release -p kettle`.

set -euo pipefail

cd "$(dirname "$0")/.."

# Pick the right `time` binary. macOS / BSD `time` doesn't support
# the `-v` / `-f` flags we need.
TIME_BIN=""
if command -v /usr/bin/time >/dev/null 2>&1 && /usr/bin/time -f '%e' true >/dev/null 2>&1; then
  TIME_BIN="/usr/bin/time"
elif command -v gtime >/dev/null 2>&1; then
  TIME_BIN="gtime"
else
  echo "bench.sh: need GNU 'time' (Linux /usr/bin/time or macOS 'gtime' from coreutils)." >&2
  exit 1
fi

if [ ! -x target/release/kettle ]; then
  echo "==> building release binary"
  cargo build --release -p kettle
fi

BIN="target/release/kettle"

echo "==> kettle build identity"
"$BIN" --version

echo ""
echo "==> binary size"
# `wc -c` is portable, doesn't trip the shellcheck SC2012 ls warning,
# and produces a single byte count.
size_bytes=$(wc -c < "$BIN")
awk -v b="$size_bytes" 'BEGIN { printf "%.1f MB (%d bytes)\n", b / 1024 / 1024, b }'

run_bench() {
  local label="$1"
  shift
  echo ""
  echo "==> $label × 5"
  for _ in 1 2 3 4 5; do
    # `%e` is wall-clock seconds, `%M` is peak RSS in KB. Divide by
    # 1024 to get MB. -o uses a temp file so the binary's own
    # stdout/stderr don't get mixed with the timing line.
    local tmp
    tmp=$(mktemp)
    "$TIME_BIN" -f '%e %M' -o "$tmp" "$@" >/dev/null
    awk '{ printf "  %s s, %.1f MB peak RSS\n", $1, $2 / 1024 }' "$tmp"
    rm -f "$tmp"
  done
}

run_bench "--version (startup floor)" "$BIN" --version
run_bench "--screenshot (GPU pipeline + render)" "$BIN" --screenshot /tmp/kettle-bench.png
run_bench "--screenshot-menu (with cycle-251 menu pass)" "$BIN" --screenshot-menu /tmp/kettle-bench-menu.png

echo ""
echo "==> done. See docs/PERFORMANCE.md for the published baseline."
