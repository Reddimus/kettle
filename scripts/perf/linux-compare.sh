#!/usr/bin/env bash
# Compare Kettle against installed Linux terminal peers with the same
# Hyperfine methodology used for the Ubuntu numbers in docs/PERFORMANCE.md.

set -euo pipefail

cd "$(dirname "$0")/../.."

runs=5
warmup=1
out_dir="target/perf-results/linux-local"
build_release=1

usage() {
  cat <<'EOF'
Usage: scripts/perf/linux-compare.sh [--runs N] [--warmup N] [--out-dir DIR] [--no-build]

Runs Linux desktop probes:
  - startup: launch a terminal, run /bin/true, close
  - ascii-flood: launch a terminal, print ~4 MiB ASCII, close
  - rss-flood: max RSS while running the same ascii-flood lifecycle

Required peers: terminator, ghostty.
Optional context peer: alacritty.

The score gate fails if Kettle is slower than Terminator or more than 10% slower
than Ghostty on either required timing workload. RSS is recorded as advisory
evidence in OUT_DIR/linux-rss-flood.json and summarized in linux-score.json.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --runs)
      runs="${2:?--runs requires a value}"
      shift 2
      ;;
    --warmup)
      warmup="${2:?--warmup requires a value}"
      shift 2
      ;;
    --out-dir)
      out_dir="${2:?--out-dir requires a value}"
      shift 2
      ;;
    --no-build)
      build_release=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "linux-compare.sh: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$runs" in
  ''|*[!0-9]*)
    echo "linux-compare.sh: --runs must be a positive integer" >&2
    exit 2
    ;;
esac
case "$warmup" in
  ''|*[!0-9]*)
    echo "linux-compare.sh: --warmup must be a non-negative integer" >&2
    exit 2
    ;;
esac
if [ "$runs" -lt 1 ]; then
  echo "linux-compare.sh: --runs must be at least 1" >&2
  exit 2
fi

for cmd in hyperfine python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "linux-compare.sh: missing required command: $cmd" >&2
    exit 1
  fi
done
time_bin="${TIME_BIN:-/usr/bin/time}"
if [ ! -x "$time_bin" ]; then
  echo "linux-compare.sh: missing required command: $time_bin" >&2
  exit 1
fi
for cmd in terminator ghostty; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "linux-compare.sh: missing required peer terminal: $cmd" >&2
    exit 1
  fi
done

if [ "$build_release" -eq 1 ]; then
  cargo build --release -p kettle
fi

if [ -n "${KETTLE_BIN:-}" ]; then
  kettle_bin="$KETTLE_BIN"
else
  kettle_bin="$PWD/target/release/kettle"
fi
if [ ! -x "$kettle_bin" ]; then
  echo "linux-compare.sh: Kettle binary is not executable: $kettle_bin" >&2
  echo "Set KETTLE_BIN=/path/to/kettle or omit --no-build." >&2
  exit 1
fi

mkdir -p "$out_dir"
tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

kettle_config="$tmp_dir/kettle.config"
cat > "$kettle_config" <<'EOF'
text-renderer = grid
gpu-power-preference = auto
agent-server = off
restore-session = false
update-check = false
tab-bar = off
status-bar = off
EOF

export KETTLE_BIN="$kettle_bin"
export KETTLE_CONFIG="$kettle_config"
export ASCII_FLOOD_CMD='yes "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" | head -c 4194304'

write_wrapper() {
  local path="$1"
  local body="$2"
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' "$body" > "$path"
  chmod +x "$path"
}

# The single-quoted wrapper bodies intentionally defer env expansion until the
# generated wrapper runs under hyperfine.
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kettle-startup" 'exec "$KETTLE_BIN" --config "$KETTLE_CONFIG" -e sh -lc true'
write_wrapper "$tmp_dir/terminator-startup" 'exec terminator --no-dbus -x sh -lc true'
write_wrapper "$tmp_dir/ghostty-startup" 'exec ghostty -e sh -lc true'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kettle-flood" 'exec "$KETTLE_BIN" --config "$KETTLE_CONFIG" -e sh -lc "$ASCII_FLOOD_CMD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/terminator-flood" 'exec terminator --no-dbus -x sh -lc "$ASCII_FLOOD_CMD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/ghostty-flood" 'exec ghostty -e sh -lc "$ASCII_FLOOD_CMD"'

startup_args=(
  --command-name kettle "$tmp_dir/kettle-startup"
  --command-name terminator "$tmp_dir/terminator-startup"
  --command-name ghostty "$tmp_dir/ghostty-startup"
)
flood_args=(
  --command-name kettle "$tmp_dir/kettle-flood"
  --command-name terminator "$tmp_dir/terminator-flood"
  --command-name ghostty "$tmp_dir/ghostty-flood"
)
terminal_names=(kettle terminator ghostty)
if command -v alacritty >/dev/null 2>&1; then
  write_wrapper "$tmp_dir/alacritty-startup" 'exec alacritty -e sh -lc true'
  # shellcheck disable=SC2016
  write_wrapper "$tmp_dir/alacritty-flood" 'exec alacritty -e sh -lc "$ASCII_FLOOD_CMD"'
  startup_args+=(--command-name alacritty "$tmp_dir/alacritty-startup")
  flood_args+=(--command-name alacritty "$tmp_dir/alacritty-flood")
  terminal_names+=(alacritty)
fi

startup_json="$out_dir/linux-startup.json"
flood_json="$out_dir/linux-ascii-flood.json"
rss_json="$out_dir/linux-rss-flood.json"
score_json="$out_dir/linux-score.json"
rss_tsv="$tmp_dir/rss-flood.tsv"

echo "==> kettle build identity"
"$kettle_bin" --version
echo ""
echo "==> startup: launch terminal, run /bin/true, close"
hyperfine --runs "$runs" --warmup "$warmup" --export-json "$startup_json" "${startup_args[@]}"
echo ""
echo "==> ascii-flood: launch terminal, print ~4 MiB ASCII, close"
hyperfine --runs "$runs" --warmup "$warmup" --export-json "$flood_json" "${flood_args[@]}"

echo ""
echo "==> rss-flood: max RSS while printing ~4 MiB ASCII"
: > "$rss_tsv"
for name in "${terminal_names[@]}"; do
  for i in $(seq 1 "$runs"); do
    rss_out="$tmp_dir/$name-rss-$i.txt"
    if ! "$time_bin" -f '%M' -o "$rss_out" "$tmp_dir/$name-flood" >/dev/null 2>"$tmp_dir/$name-rss-$i.err"; then
      cat "$tmp_dir/$name-rss-$i.err" >&2
      exit 1
    fi
    rss_kib="$(tr -dc '0-9' < "$rss_out")"
    if [ -z "$rss_kib" ]; then
      echo "linux-compare.sh: empty RSS sample for $name run $i" >&2
      exit 1
    fi
    printf '%s\t%s\n' "$name" "$rss_kib" >> "$rss_tsv"
  done
done

python3 - "$startup_json" "$flood_json" "$rss_tsv" "$rss_json" "$score_json" <<'PY'
import json
import sys
from pathlib import Path

startup_path, flood_path, rss_tsv_path, rss_path, score_path = map(Path, sys.argv[1:6])

def load_medians(path):
    with path.open("r", encoding="utf-8") as f:
        doc = json.load(f)
    return {row["command"]: float(row["median"]) for row in doc.get("results", [])}

def median(values):
    vals = sorted(values)
    n = len(vals)
    if n == 0:
        raise ValueError("empty median input")
    mid = n // 2
    if n % 2:
        return float(vals[mid])
    return (float(vals[mid - 1]) + float(vals[mid])) / 2.0

def load_rss(path):
    samples = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        name, value = line.split("\t", 1)
        samples.setdefault(name, []).append(int(value))
    medians = {name: median(values) for name, values in samples.items()}
    return samples, medians

workloads = {
    "startup": load_medians(startup_path),
    "ascii_flood": load_medians(flood_path),
}
rss_samples, rss_medians = load_rss(rss_tsv_path)
rss_path.write_text(
    json.dumps(
        {
            "workload": "rss_flood",
            "unit": "KiB",
            "samples_kib": rss_samples,
            "median_kib": rss_medians,
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)

required = ("kettle", "terminator", "ghostty")
failures = []
summary = {
    "startup_json": str(startup_path),
    "ascii_flood_json": str(flood_path),
    "rss_flood_json": str(rss_path),
    "workloads": {},
    "memory": {},
    "rules": {
        "beats_terminator": "kettle median <= terminator median",
        "close_to_ghostty": "kettle median <= ghostty median * 1.10",
        "rss_flood": "recorded as advisory max-RSS evidence, not a pass/fail gate",
    },
}

for workload, medians in workloads.items():
    missing = [name for name in required if name not in medians]
    if missing:
        failures.append(f"{workload}: missing results for {', '.join(missing)}")
        continue

    kettle = medians["kettle"]
    terminator = medians["terminator"]
    ghostty = medians["ghostty"]
    beats_terminator = kettle <= terminator
    close_to_ghostty = kettle <= ghostty * 1.10
    if not beats_terminator:
        failures.append(
            f"{workload}: kettle median {kettle:.3f}s is slower than Terminator {terminator:.3f}s"
        )
    if not close_to_ghostty:
        failures.append(
            f"{workload}: kettle median {kettle:.3f}s is more than 10% slower than Ghostty {ghostty:.3f}s"
        )

    summary["workloads"][workload] = {
        "median_seconds": medians,
        "kettle_vs_terminator": round(kettle / terminator, 4),
        "kettle_vs_ghostty": round(kettle / ghostty, 4),
        "passed": beats_terminator and close_to_ghostty,
    }

missing_rss = [name for name in required if name not in rss_medians]
if missing_rss:
    failures.append(f"rss_flood: missing results for {', '.join(missing_rss)}")
else:
    kettle_rss = rss_medians["kettle"]
    terminator_rss = rss_medians["terminator"]
    ghostty_rss = rss_medians["ghostty"]
    summary["memory"]["rss_flood"] = {
        "median_kib": rss_medians,
        "kettle_vs_terminator": round(kettle_rss / terminator_rss, 4),
        "kettle_vs_ghostty": round(kettle_rss / ghostty_rss, 4),
        "advisory": True,
    }

summary["passed"] = not failures
summary["failures"] = failures
score_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

names = []
for medians in workloads.values():
    for name in medians:
        if name not in names:
            names.append(name)

print("")
print("Linux performance medians (seconds)")
print("workload      " + " ".join(f"{name:>11}" for name in names))
print("-" * (13 + 12 * len(names)))
for workload, medians in workloads.items():
    print(f"{workload:<12} " + " ".join(
        f"{medians[name]:11.3f}" if name in medians else f"{'n/a':>11}"
        for name in names
    ))
print("")
for workload, data in summary["workloads"].items():
    print(
        f"{workload}: kettle/terminator={data['kettle_vs_terminator']:.3f}, "
        f"kettle/ghostty={data['kettle_vs_ghostty']:.3f}"
    )
if "rss_flood" in summary["memory"]:
    print("")
    print("Linux rss-flood median max RSS (MiB)")
    for name, kib in summary["memory"]["rss_flood"]["median_kib"].items():
        print(f"{name:<12} {kib / 1024.0:8.1f}")
    print(
        "rss_flood: "
        f"kettle/terminator={summary['memory']['rss_flood']['kettle_vs_terminator']:.3f}, "
        f"kettle/ghostty={summary['memory']['rss_flood']['kettle_vs_ghostty']:.3f} "
        "(advisory)"
    )

if failures:
    print("")
    print("FAILED:")
    for failure in failures:
        print(f"  - {failure}")
    sys.exit(1)

print("")
print(f"PASS: wrote {score_path}")
PY

echo ""
echo "results:"
echo "  $startup_json"
echo "  $flood_json"
echo "  $rss_json"
echo "  $score_json"
