#!/usr/bin/env bash
# Compare Kettle against installed macOS terminal peers.
#
# Timing workloads use Hyperfine. Memory is the median maximum RSS from
# `/usr/bin/time -l`; macOS reports "maximum resident set size" in bytes.
# AppleScript-launched apps are skipped for RSS because `time` would measure
# osascript rather than the terminal process. Idle CPU uses the direct
# terminal parent PID, or the owning app PID for AppleScript peers, settles for
# 3 seconds, then takes five `ps -o %cpu= -p PID` samples one second apart and
# reports their median.
# Input latency is intentionally not measured: AppleScript-only terminals do
# not expose a comparable keystroke-to-paint observation path.

set -euo pipefail

cd "$(dirname "$0")/../.."

runs=5
warmup=1
out_dir="target/perf-results/macos-local"
build_release=1

startup_timeout_seconds=30
workload_timeout_seconds=180
applescript_timeout_seconds=10
idle_start_timeout_seconds=20
idle_exit_timeout_seconds=15
idle_settle_seconds=3
idle_sample_count=5
idle_sample_interval_seconds=1

usage() {
  cat <<'EOF'
Usage: scripts/perf/macos-compare.sh [--runs N] [--warmup N] [--out-dir DIR] [--no-build]

Runs macOS desktop probes:
  - startup: launch a terminal, run /bin/true, close
  - ascii-flood: launch a terminal, print ~4 MiB ASCII, close
  - ansi-underline-flood: launch a terminal, print 35k SGR/underline lines, close
  - memory-rss: median max RSS over the ascii-flood lifecycle
  - idle-cpu: median of five 1-second-spaced ps %CPU samples after a 3s settle
  - kettle-live: Kettle control-plane resize + scrollback-navigation probes

Peers: Ghostty, kitty, WezTerm, Terminal.app, optional ALACRITTY_BIN, and
optional iTerm2. Missing or undriveable peers are explicit skips in the score
JSON. AppleScript-launched peers are skipped for RSS because /usr/bin/time -l
cannot attribute their detached GUI process.

For each eligible metric, lower is better and Kettle is top-half when its rank
is <= ceil(N/2). A metric is eligible only when Kettle and at least one real
competitor were both measured. The run passes when Kettle is top-half on at
least 3 eligible metrics. Input latency is deliberately out of scope and is
recorded as not measured in macos-score.json.
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
      echo "macos-compare.sh: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$runs" in
  ''|*[!0-9]*)
    echo "macos-compare.sh: --runs must be a positive integer" >&2
    exit 2
    ;;
esac
case "$warmup" in
  ''|*[!0-9]*)
    echo "macos-compare.sh: --warmup must be a non-negative integer" >&2
    exit 2
    ;;
esac
if [ "$runs" -lt 1 ]; then
  echo "macos-compare.sh: --runs must be at least 1" >&2
  exit 2
fi
if [ "$(uname -s)" != "Darwin" ]; then
  echo "macos-compare.sh: this benchmark requires macOS" >&2
  exit 1
fi

for cmd in hyperfine python3 osascript ps pgrep; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "macos-compare.sh: missing required command: $cmd" >&2
    exit 1
  fi
done
time_bin="${TIME_BIN:-/usr/bin/time}"
if [ ! -x "$time_bin" ]; then
  echo "macos-compare.sh: missing required command: $time_bin" >&2
  exit 1
fi

if [ "$build_release" -eq 1 ]; then
  cargo build --release -p kettle
fi

if [ -n "${KETTLE_BIN:-}" ]; then
  kettle_bin="$KETTLE_BIN"
else
  kettle_bin="$PWD/target/release/kettle"
fi
if [ ! -x "$kettle_bin" ]; then
  echo "macos-compare.sh: Kettle binary is not executable: $kettle_bin" >&2
  echo "Set KETTLE_BIN=/path/to/kettle or omit --no-build." >&2
  exit 1
fi

mkdir -p "$out_dir"
tmp_dir="$(mktemp -d)"
peer_pid_log="$tmp_dir/peer-pids.txt"
idle_stop_log="$tmp_dir/idle-stop-files.txt"
apple_window_log="$tmp_dir/apple-windows.tsv"
: > "$peer_pid_log"
: > "$idle_stop_log"
: > "$apple_window_log"

# Invoked indirectly by the EXIT trap.
# shellcheck disable=SC2329
cleanup() {
  cleanup_status=$?
  trap - EXIT HUP INT TERM

  if [ -f "$idle_stop_log" ]; then
    while IFS= read -r stop_file; do
      if [ -n "$stop_file" ]; then
        : > "$stop_file"
      fi
    done < "$idle_stop_log"
  fi

  if [ -s "$apple_window_log" ]; then
    python3 - "$apple_window_log" <<'PY' || true
import subprocess
import sys
from pathlib import Path

windows = set()
for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    try:
        app, window_id = line.split("\t", 1)
    except ValueError:
        continue
    windows.add((app, window_id))

for app, window_id in windows:
    if app == "terminal" and window_id.isdigit():
        source = f'tell application "Terminal" to close window id {window_id} saving no'
    elif app == "iterm2":
        escaped_id = window_id.replace("\\", "\\\\").replace('"', '\\"')
        source = (
            f'tell application "iTerm2" to close '
            f'(first window whose id is "{escaped_id}")'
        )
    else:
        continue
    try:
        subprocess.run(
            ["osascript", "-e", source],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        )
    except subprocess.TimeoutExpired:
        pass
PY
  fi

  if [ -f "$peer_pid_log" ]; then
    while IFS= read -r peer_pid; do
      case "$peer_pid" in
        ''|*[!0-9]*) continue ;;
      esac
      if kill -0 "$peer_pid" 2>/dev/null; then
        kill -TERM "$peer_pid" 2>/dev/null || true
      fi
    done < "$peer_pid_log"
    sleep 1
    while IFS= read -r peer_pid; do
      case "$peer_pid" in
        ''|*[!0-9]*) continue ;;
      esac
      if kill -0 "$peer_pid" 2>/dev/null; then
        kill -KILL "$peer_pid" 2>/dev/null || true
      fi
    done < "$peer_pid_log"
  fi

  rm -rf "$tmp_dir"
  exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

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

write_wrapper() {
  local wrapper_path="$1"
  local wrapper_body="$2"
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' "$wrapper_body" > "$wrapper_path"
  chmod +x "$wrapper_path"
}

bounded_run="$tmp_dir/bounded-run"
cat > "$bounded_run" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

timeout_seconds="$1"
pid_log="$2"
shift 2

completion_marker="$MARKER_DIR/direct-result-$$"
rm -f "$completion_marker"
export COMPLETION_MARKER="$completion_marker"
"$@" &
child_pid=$!
printf '%s\n' "$child_pid" >> "$pid_log"

terminate_child() {
  if kill -0 "$child_pid" 2>/dev/null; then
    kill -TERM "$child_pid" 2>/dev/null || true
  fi
}
trap terminate_child HUP INT TERM

ticks=0
max_ticks=$((timeout_seconds * 10))
while [ "$ticks" -lt "$max_ticks" ]; do
  if [ -s "$completion_marker" ] || ! kill -0 "$child_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
  ticks=$((ticks + 1))
done

if [ -s "$completion_marker" ]; then
  payload_status="$(tr -dc '0-9' < "$completion_marker")"
  rm -f "$completion_marker"
  case "$payload_status" in
    ''|*[!0-9]*) payload_status=1 ;;
  esac

  # Give a terminal that closes with its child a brief chance to exit itself.
  close_ticks=0
  while kill -0 "$child_pid" 2>/dev/null && [ "$close_ticks" -lt 2 ]; do
    sleep 0.1
    close_ticks=$((close_ticks + 1))
  done
  if kill -0 "$child_pid" 2>/dev/null; then
    kill -TERM "$child_pid" 2>/dev/null || true
  fi
  term_ticks=0
  while kill -0 "$child_pid" 2>/dev/null && [ "$term_ticks" -lt 20 ]; do
    sleep 0.1
    term_ticks=$((term_ticks + 1))
  done
  if kill -0 "$child_pid" 2>/dev/null; then
    kill -KILL "$child_pid" 2>/dev/null || true
  fi
  if wait "$child_pid" 2>/dev/null; then
    :
  fi
  trap - HUP INT TERM
  exit "$payload_status"
fi

if ! kill -0 "$child_pid" 2>/dev/null; then
  if wait "$child_pid"; then
    child_status=0
  else
    child_status=$?
  fi
  trap - HUP INT TERM
  echo "macos-compare.sh: terminal exited before its payload completion marker: $1" >&2
  if [ "$child_status" -eq 0 ]; then
    child_status=1
  fi
  exit "$child_status"
fi

kill -TERM "$child_pid" 2>/dev/null || true
sleep 2
kill -KILL "$child_pid" 2>/dev/null || true
wait "$child_pid" 2>/dev/null || true
trap - HUP INT TERM
rm -f "$completion_marker"
echo "macos-compare.sh: command timed out after ${timeout_seconds}s: $1" >&2
exit 124
EOF
chmod +x "$bounded_run"

apple_run="$tmp_dir/apple-run.py"
cat > "$apple_run" <<'PY'
#!/usr/bin/env python3
"""Launch one AppleScript terminal window, bound it, and close that window."""

from __future__ import annotations

import argparse
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Iterable, List, Set


def process_ids(name: str) -> Set[int]:
    try:
        cp = subprocess.run(
            ["pgrep", "-x", name],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return set()
    result: Set[int] = set()
    for value in cp.stdout.split():
        if value.isdigit():
            result.add(int(value))
    return result


def append_pids(pid_log: Path, pids: Iterable[int]) -> None:
    with pid_log.open("a", encoding="utf-8") as output:
        for pid in sorted(pids):
            output.write(f"{pid}\n")


def apple_string(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')


def osascript(lines: List[str], timeout: float) -> subprocess.CompletedProcess[str]:
    argv = ["osascript"]
    for line in lines:
        argv += ["-e", line]
    return subprocess.run(
        argv,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def close_window(app: str, window_id: str) -> None:
    if not window_id:
        return
    try:
        if app == "terminal" and window_id.isdigit():
            lines = [f'tell application "Terminal" to close window id {window_id} saving no']
        else:
            escaped_id = apple_string(window_id)
            lines = [
                f'tell application "iTerm2" to close (first window whose id is "{escaped_id}")'
            ]
        osascript(lines, 5)
    except subprocess.TimeoutExpired:
        pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", choices=("terminal", "iterm2"), required=True)
    parser.add_argument("--timeout", type=float, required=True)
    parser.add_argument("--launch-timeout", type=float, required=True)
    parser.add_argument("--pid-log", required=True, type=Path)
    parser.add_argument("--marker-dir", required=True, type=Path)
    parser.add_argument("--idle-pid-file", type=Path)
    parser.add_argument(
        "--window-log", type=Path, default=Path(os.environ["APPLE_WINDOW_LOG"])
    )
    parser.add_argument("payload", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.payload[:1] == ["--"]:
        args.payload = args.payload[1:]
    if not args.payload:
        parser.error("missing payload command")

    app_name = "Terminal" if args.app == "terminal" else "iTerm2"
    process_name = app_name
    before_pids = process_ids(process_name)
    marker_fd, marker_name = tempfile.mkstemp(prefix="apple-result-", dir=str(args.marker_dir))
    os.close(marker_fd)
    os.unlink(marker_name)
    marker = Path(marker_name)
    payload = " ".join(shlex.quote(value) for value in args.payload)
    # Terminal.app runs the command in the user's login shell. `status` is a
    # read-only special parameter in zsh, so use a deliberately unique name.
    command = (
        f"{payload}; kettle_perf_status=$?; "
        f"echo $kettle_perf_status > {shlex.quote(str(marker))}; exit"
    )
    escaped_command = apple_string(command)
    if args.app == "terminal":
        lines = [
            f'tell application "Terminal" to do script "{escaped_command}"',
        ]
    else:
        lines = [
            'tell application "iTerm2"',
            f'set launchedWindow to create window with default profile command "{escaped_command}"',
            "return id of launchedWindow",
            "end tell",
        ]

    window_id = ""
    try:
        launch = osascript(lines, args.launch_timeout)
    except subprocess.TimeoutExpired:
        time.sleep(0.2)
        append_pids(args.pid_log, process_ids(process_name) - before_pids)
        print(
            f"{app_name} AppleScript launch timed out after {args.launch_timeout:g}s "
            "before returning a window",
            file=sys.stderr,
        )
        return 124

    after_pids = process_ids(process_name)
    append_pids(args.pid_log, after_pids - before_pids)
    if args.idle_pid_file is not None and after_pids:
        args.idle_pid_file.write_text(f"{min(after_pids)}\n", encoding="utf-8")
    if launch.returncode != 0:
        detail = launch.stderr.strip() or f"osascript exited {launch.returncode}"
        print(f"{app_name} AppleScript launch failed: {detail}", file=sys.stderr)
        return launch.returncode
    launch_result = launch.stdout.strip().splitlines()[-1] if launch.stdout.strip() else ""
    if args.app == "terminal":
        match = re.search(r"window id (\d+)", launch_result)
        window_id = match.group(1) if match else ""
    else:
        window_id = launch_result
    if window_id:
        with args.window_log.open("a", encoding="utf-8") as output:
            output.write(f"{args.app}\t{window_id}\n")

    deadline = time.monotonic() + args.timeout
    try:
        while time.monotonic() < deadline:
            if marker.exists():
                value = marker.read_text(encoding="utf-8", errors="replace").strip()
                if value.isdigit():
                    return int(value)
                print(f"{app_name} produced an invalid completion marker: {value!r}", file=sys.stderr)
                return 1
            time.sleep(0.05)
        print(f"{app_name} payload timed out after {args.timeout:g}s", file=sys.stderr)
        return 124
    finally:
        close_window(args.app, window_id)
        try:
            marker.unlink()
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    sys.exit(main())
PY
chmod +x "$apple_run"

startup_payload="$tmp_dir/startup-payload"
cat > "$startup_payload" <<'EOF'
#!/bin/sh
/usr/bin/true
status=$?
if [ -n "${COMPLETION_MARKER:-}" ]; then
  printf '%s\n' "$status" > "$COMPLETION_MARKER"
fi
exit "$status"
EOF
chmod +x "$startup_payload"

ascii_payload="$tmp_dir/ascii-flood-payload"
cat > "$ascii_payload" <<'EOF'
#!/bin/sh
yes "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" | head -c 4194304
status=$?
if [ -n "${COMPLETION_MARKER:-}" ]; then
  printf '%s\n' "$status" > "$COMPLETION_MARKER"
fi
exit "$status"
EOF
chmod +x "$ascii_payload"

ansi_payload="$tmp_dir/ansi-underline-flood-payload"
cat > "$ansi_payload" <<'EOF'
#!/bin/sh
for i in $(seq 1 35000); do
  printf "\033[4mUNDERLINE_%05d\033[24m plain text https://example.invalid/%05d \033[38;2;180;220;255mcolor\033[0m\n" "$i" "$i"
done
status=$?
if [ -n "${COMPLETION_MARKER:-}" ]; then
  printf '%s\n' "$status" > "$COMPLETION_MARKER"
fi
exit "$status"
EOF
chmod +x "$ansi_payload"

idle_payload="$tmp_dir/idle-payload"
cat > "$idle_payload" <<'EOF'
#!/bin/sh
set -u
pid_file="$1"
stop_file="$2"
process_name="${3:-}"
if [ -n "$process_name" ]; then
  /usr/bin/pgrep -x "$process_name" | head -n 1 > "$pid_file"
else
  printf '%s\n' "$PPID" > "$pid_file"
fi
while [ ! -f "$stop_file" ]; do
  sleep 1
done
status=$?
if [ -n "${COMPLETION_MARKER:-}" ]; then
  printf '%s\n' "$status" > "$COMPLETION_MARKER"
fi
exit "$status"
EOF
chmod +x "$idle_payload"

export KETTLE_BIN="$kettle_bin"
export KETTLE_CONFIG="$kettle_config"
export BOUNDED_RUN="$bounded_run"
export APPLE_RUN="$apple_run"
export PEER_PID_LOG="$peer_pid_log"
export APPLE_WINDOW_LOG="$apple_window_log"
export MARKER_DIR="$tmp_dir"
export STARTUP_PAYLOAD="$startup_payload"
export ASCII_PAYLOAD="$ascii_payload"
export ANSI_PAYLOAD="$ansi_payload"
export IDLE_PAYLOAD="$idle_payload"
export STARTUP_TIMEOUT_SECONDS="$startup_timeout_seconds"
export WORKLOAD_TIMEOUT_SECONDS="$workload_timeout_seconds"
export APPLESCRIPT_TIMEOUT_SECONDS="$applescript_timeout_seconds"

ghostty_bin="/Applications/Ghostty.app/Contents/MacOS/ghostty"
kitty_bin="/Applications/kitty.app/Contents/MacOS/kitty"
wezterm_bin="/Applications/WezTerm.app/Contents/MacOS/wezterm-gui"
iterm_bin="/Applications/iTerm.app/Contents/MacOS/iTerm2"
terminal_bin="/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"
alacritty_bin="${ALACRITTY_BIN:-}"

export GHOSTTY_BIN="$ghostty_bin"
export KITTY_BIN="$kitty_bin"
export WEZTERM_BIN="$wezterm_bin"
export ALACRITTY_BIN="$alacritty_bin"

# The single-quoted wrapper bodies intentionally defer expansion until the
# generated wrapper runs under Hyperfine.
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kettle-startup" 'exec "$BOUNDED_RUN" "$STARTUP_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KETTLE_BIN" --config "$KETTLE_CONFIG" -e sh -lc '\''exec "$STARTUP_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kettle-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KETTLE_BIN" --config "$KETTLE_CONFIG" -e sh -lc '\''exec "$ASCII_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kettle-ansi-underline-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KETTLE_BIN" --config "$KETTLE_CONFIG" -e sh -lc '\''exec "$ANSI_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kettle-idle" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KETTLE_BIN" --config "$KETTLE_CONFIG" -e sh -lc '\''exec "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE"'\'''

# shellcheck disable=SC2016
write_wrapper "$tmp_dir/ghostty-startup" 'exec "$BOUNDED_RUN" "$STARTUP_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$GHOSTTY_BIN" -e sh -lc '\''exec "$STARTUP_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/ghostty-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$GHOSTTY_BIN" -e sh -lc '\''exec "$ASCII_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/ghostty-ansi-underline-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$GHOSTTY_BIN" -e sh -lc '\''exec "$ANSI_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/ghostty-idle" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$GHOSTTY_BIN" -e sh -lc '\''exec "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE"'\'''

# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kitty-startup" 'exec "$BOUNDED_RUN" "$STARTUP_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KITTY_BIN" --single-instance=no sh -lc '\''exec "$STARTUP_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kitty-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KITTY_BIN" --single-instance=no sh -lc '\''exec "$ASCII_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kitty-ansi-underline-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KITTY_BIN" --single-instance=no sh -lc '\''exec "$ANSI_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/kitty-idle" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$KITTY_BIN" --single-instance=no sh -lc '\''exec "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE"'\'''

# shellcheck disable=SC2016
write_wrapper "$tmp_dir/wezterm-startup" 'exec "$BOUNDED_RUN" "$STARTUP_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$WEZTERM_BIN" start --always-new-process -- sh -lc '\''exec "$STARTUP_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/wezterm-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$WEZTERM_BIN" start --always-new-process -- sh -lc '\''exec "$ASCII_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/wezterm-ansi-underline-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$WEZTERM_BIN" start --always-new-process -- sh -lc '\''exec "$ANSI_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/wezterm-idle" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$WEZTERM_BIN" start --always-new-process -- sh -lc '\''exec "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE"'\'''

# shellcheck disable=SC2016
write_wrapper "$tmp_dir/alacritty-startup" 'exec "$BOUNDED_RUN" "$STARTUP_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$ALACRITTY_BIN" -e sh -lc '\''exec "$STARTUP_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/alacritty-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$ALACRITTY_BIN" -e sh -lc '\''exec "$ASCII_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/alacritty-ansi-underline-flood" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$ALACRITTY_BIN" -e sh -lc '\''exec "$ANSI_PAYLOAD"'\'''
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/alacritty-idle" 'exec "$BOUNDED_RUN" "$WORKLOAD_TIMEOUT_SECONDS" "$PEER_PID_LOG" "$ALACRITTY_BIN" -e sh -lc '\''exec "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE"'\'''

# shellcheck disable=SC2016
write_wrapper "$tmp_dir/terminal-startup" 'exec python3 "$APPLE_RUN" --app terminal --timeout "$STARTUP_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" -- "$STARTUP_PAYLOAD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/terminal-flood" 'exec python3 "$APPLE_RUN" --app terminal --timeout "$WORKLOAD_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" -- "$ASCII_PAYLOAD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/terminal-ansi-underline-flood" 'exec python3 "$APPLE_RUN" --app terminal --timeout "$WORKLOAD_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" -- "$ANSI_PAYLOAD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/terminal-idle" 'exec python3 "$APPLE_RUN" --app terminal --timeout "$WORKLOAD_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" --idle-pid-file "$IDLE_PID_FILE" -- "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE" Terminal'

# shellcheck disable=SC2016
write_wrapper "$tmp_dir/iterm2-startup" 'exec python3 "$APPLE_RUN" --app iterm2 --timeout "$STARTUP_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" -- "$STARTUP_PAYLOAD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/iterm2-flood" 'exec python3 "$APPLE_RUN" --app iterm2 --timeout "$WORKLOAD_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" -- "$ASCII_PAYLOAD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/iterm2-ansi-underline-flood" 'exec python3 "$APPLE_RUN" --app iterm2 --timeout "$WORKLOAD_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" -- "$ANSI_PAYLOAD"'
# shellcheck disable=SC2016
write_wrapper "$tmp_dir/iterm2-idle" 'exec python3 "$APPLE_RUN" --app iterm2 --timeout "$WORKLOAD_TIMEOUT_SECONDS" --launch-timeout "$APPLESCRIPT_TIMEOUT_SECONDS" --pid-log "$PEER_PID_LOG" --marker-dir "$MARKER_DIR" --idle-pid-file "$IDLE_PID_FILE" -- "$IDLE_PAYLOAD" "$IDLE_PID_FILE" "$IDLE_STOP_FILE" iTerm2'

peer_status_tsv="$tmp_dir/peer-status.tsv"
metric_skips_tsv="$tmp_dir/metric-skips.tsv"
: > "$peer_status_tsv"
: > "$metric_skips_tsv"
terminal_names=()

clean_reason() {
  printf '%s' "$1" | tr '\t\r\n' '   ' | sed 's/  */ /g; s/^ //; s/ $//'
}

add_active_peer() {
  local peer_name="$1"
  terminal_names+=("$peer_name")
  printf '%s\tactive\t\n' "$peer_name" >> "$peer_status_tsv"
}

add_skipped_peer() {
  local peer_name="$1"
  local reason
  reason="$(clean_reason "$2")"
  printf '%s\tskipped\t%s\n' "$peer_name" "$reason" >> "$peer_status_tsv"
  echo "SKIP $peer_name: $reason"
}

add_metric_skip() {
  local metric="$1"
  local peer_name="$2"
  local reason
  reason="$(clean_reason "$3")"
  printf '%s\t%s\t%s\n' "$metric" "$peer_name" "$reason" >> "$metric_skips_tsv"
  echo "SKIP $metric/$peer_name: $reason"
}

preflight_peer() {
  local peer_name="$1"
  local stderr_file="$tmp_dir/$peer_name-preflight.err"
  local reason
  if "$tmp_dir/$peer_name-startup" >/dev/null 2>"$stderr_file"; then
    add_active_peer "$peer_name"
    return 0
  fi
  reason="$(tail -n 1 "$stderr_file")"
  if [ -z "$reason" ]; then
    reason="startup command failed during bounded driveability probe"
  fi
  add_skipped_peer "$peer_name" "$reason"
  return 1
}

echo "==> peer preflight: bounded launch, /usr/bin/true, close"
if ! preflight_peer kettle; then
  echo "macos-compare.sh: Kettle failed its launch preflight" >&2
  exit 1
fi

if [ -x "$ghostty_bin" ]; then
  preflight_peer ghostty || true
else
  add_skipped_peer ghostty "not executable at $ghostty_bin"
fi
if [ -x "$kitty_bin" ]; then
  preflight_peer kitty || true
else
  add_skipped_peer kitty "not executable at $kitty_bin"
fi
if [ -x "$wezterm_bin" ]; then
  preflight_peer wezterm || true
else
  add_skipped_peer wezterm "not executable at $wezterm_bin"
fi
if [ -n "$alacritty_bin" ] && [ -x "$alacritty_bin" ]; then
  preflight_peer alacritty || true
elif [ -z "$alacritty_bin" ]; then
  add_skipped_peer alacritty "ALACRITTY_BIN is unset"
else
  add_skipped_peer alacritty "ALACRITTY_BIN is not executable: $alacritty_bin"
fi
if [ -x "$terminal_bin" ]; then
  preflight_peer terminal || true
else
  add_skipped_peer terminal "Terminal.app is not executable at $terminal_bin"
fi
if [ -x "$iterm_bin" ]; then
  preflight_peer iterm2 || true
else
  add_skipped_peer iterm2 "iTerm2 is not executable at $iterm_bin"
fi

startup_args=()
flood_args=()
ansi_args=()
for name in "${terminal_names[@]}"; do
  startup_args+=(--command-name "$name" "$tmp_dir/$name-startup")
  flood_args+=(--command-name "$name" "$tmp_dir/$name-flood")
  ansi_args+=(--command-name "$name" "$tmp_dir/$name-ansi-underline-flood")
done

startup_json="$out_dir/macos-startup.json"
flood_json="$out_dir/macos-ascii-flood.json"
ansi_json="$out_dir/macos-ansi-underline-flood.json"
rss_json="$out_dir/macos-rss-flood.json"
idle_json="$out_dir/macos-idle-cpu.json"
live_json="$out_dir/macos-kettle-live.json"
score_json="$out_dir/macos-score.json"
rss_tsv="$tmp_dir/rss-flood.tsv"
idle_tsv="$tmp_dir/idle-cpu.tsv"

echo ""
echo "==> kettle build identity"
"$kettle_bin" --version
echo ""
echo "==> startup: launch terminal, run /bin/true, close"
hyperfine --runs "$runs" --warmup "$warmup" --export-json "$startup_json" "${startup_args[@]}"
echo ""
echo "==> ascii-flood: launch terminal, print ~4 MiB ASCII, close"
hyperfine --runs "$runs" --warmup "$warmup" --export-json "$flood_json" "${flood_args[@]}"
echo ""
echo "==> ansi-underline-flood: launch terminal, print 35k SGR/underline lines, close"
hyperfine --runs "$runs" --warmup "$warmup" --export-json "$ansi_json" "${ansi_args[@]}"

echo ""
echo "==> memory-rss: max RSS while printing ~4 MiB ASCII"
: > "$rss_tsv"
for name in "${terminal_names[@]}"; do
  case "$name" in
    terminal|iterm2)
      add_metric_skip memory_rss "$name" "/usr/bin/time -l would measure osascript, not the detached terminal process"
      continue
      ;;
  esac

  peer_rss_tsv="$tmp_dir/$name-rss.tsv"
  : > "$peer_rss_tsv"
  rss_ok=1
  i=1
  while [ "$i" -le "$runs" ]; do
    rss_stderr="$tmp_dir/$name-rss-$i.err"
    if ! "$time_bin" -l "$tmp_dir/$name-flood" >/dev/null 2>"$rss_stderr"; then
      add_metric_skip memory_rss "$name" "flood run $i failed under /usr/bin/time -l"
      rss_ok=0
      break
    fi
    rss_bytes="$(awk '/maximum resident set size/ { value=$1 } END { print value }' "$rss_stderr")"
    case "$rss_bytes" in
      ''|*[!0-9]*)
        add_metric_skip memory_rss "$name" "could not parse maximum resident set size in bytes for run $i"
        rss_ok=0
        break
        ;;
    esac
    printf '%s\t%s\n' "$name" "$rss_bytes" >> "$peer_rss_tsv"
    i=$((i + 1))
  done
  if [ "$rss_ok" -eq 1 ]; then
    cat "$peer_rss_tsv" >> "$rss_tsv"
  fi
done

wait_for_file() {
  local file="$1"
  local timeout_seconds="$2"
  local elapsed=0
  while [ ! -s "$file" ] && [ "$elapsed" -lt "$timeout_seconds" ]; do
    sleep 1
    elapsed=$((elapsed + 1))
  done
  [ -s "$file" ]
}

wait_for_process_exit() {
  local watched_pid="$1"
  local timeout_seconds="$2"
  local elapsed=0
  while kill -0 "$watched_pid" 2>/dev/null && [ "$elapsed" -lt "$timeout_seconds" ]; do
    sleep 1
    elapsed=$((elapsed + 1))
  done
  ! kill -0 "$watched_pid" 2>/dev/null
}

echo ""
echo "==> idle-cpu: 3s settle, then five ps %CPU samples 1s apart"
: > "$idle_tsv"
for name in "${terminal_names[@]}"; do
  idle_pid_file="$tmp_dir/$name-idle.pid"
  idle_stop_file="$tmp_dir/$name-idle.stop"
  idle_log="$tmp_dir/$name-idle.log"
  peer_idle_tsv="$tmp_dir/$name-idle.tsv"
  rm -f "$idle_pid_file" "$idle_stop_file"
  : > "$peer_idle_tsv"
  printf '%s\n' "$idle_stop_file" >> "$idle_stop_log"
  export IDLE_PID_FILE="$idle_pid_file"
  export IDLE_STOP_FILE="$idle_stop_file"

  "$tmp_dir/$name-idle" >"$idle_log" 2>&1 &
  idle_wrapper_pid=$!
  printf '%s\n' "$idle_wrapper_pid" >> "$peer_pid_log"
  if ! wait_for_file "$idle_pid_file" "$idle_start_timeout_seconds"; then
    : > "$idle_stop_file"
    kill -TERM "$idle_wrapper_pid" 2>/dev/null || true
    wait "$idle_wrapper_pid" 2>/dev/null || true
    reason="$(tail -n 1 "$idle_log")"
    if [ -z "$reason" ]; then
      reason="idle shell did not report a terminal PID within ${idle_start_timeout_seconds}s"
    fi
    add_metric_skip idle_cpu "$name" "$reason"
    continue
  fi

  terminal_pid="$(tr -dc '0-9' < "$idle_pid_file")"
  if [ -z "$terminal_pid" ] || ! kill -0 "$terminal_pid" 2>/dev/null; then
    : > "$idle_stop_file"
    wait "$idle_wrapper_pid" 2>/dev/null || true
    add_metric_skip idle_cpu "$name" "idle shell reported a terminal PID that was not running"
    continue
  fi

  sleep "$idle_settle_seconds"
  idle_ok=1
  i=1
  while [ "$i" -le "$idle_sample_count" ]; do
    if cpu_value="$(ps -o %cpu= -p "$terminal_pid" | awk '{$1=$1; print}')"; then
      :
    else
      cpu_value=""
    fi
    case "$cpu_value" in
      ''|*[!0-9.-]*)
        add_metric_skip idle_cpu "$name" "ps did not return a numeric %CPU sample for PID $terminal_pid"
        idle_ok=0
        break
        ;;
    esac
    printf '%s\t%s\n' "$name" "$cpu_value" >> "$peer_idle_tsv"
    if [ "$i" -lt "$idle_sample_count" ]; then
      sleep "$idle_sample_interval_seconds"
    fi
    i=$((i + 1))
  done

  : > "$idle_stop_file"
  if ! wait_for_process_exit "$idle_wrapper_pid" "$idle_exit_timeout_seconds"; then
    kill -TERM "$idle_wrapper_pid" 2>/dev/null || true
    wait "$idle_wrapper_pid" 2>/dev/null || true
    if [ "$idle_ok" -eq 1 ]; then
      add_metric_skip idle_cpu "$name" "idle terminal did not close within ${idle_exit_timeout_seconds}s"
    fi
    idle_ok=0
  else
    if ! wait "$idle_wrapper_pid"; then
      if [ "$idle_ok" -eq 1 ]; then
        reason="$(tail -n 1 "$idle_log")"
        if [ -z "$reason" ]; then
          reason="idle terminal wrapper exited unsuccessfully"
        fi
        add_metric_skip idle_cpu "$name" "$reason"
      fi
      idle_ok=0
    fi
  fi
  if [ "$idle_ok" -eq 1 ]; then
    cat "$peer_idle_tsv" >> "$idle_tsv"
  fi
done

echo ""
echo "==> kettle-live: resize and scrollback navigation through kettle ctl"
live_error="$tmp_dir/kettle-live.err"
if python3 scripts/perf/kettle-live-probes.py \
  --kettle "$kettle_bin" \
  --config "$kettle_config" \
  --out "$live_json" 2>"$live_error"; then
  :
else
  live_status=$?
  cat "$live_error" >&2
  live_reason="$(tail -n 1 "$live_error")"
  if [ -z "$live_reason" ]; then
    live_reason="kettle-live-probes.py exited with status $live_status"
  fi
  python3 - "$live_json" "$live_status" "$live_reason" <<'PY'
import json
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(
    json.dumps({"error": sys.argv[3], "exit_code": int(sys.argv[2])}, indent=2) + "\n",
    encoding="utf-8",
)
PY
fi

score_status=0
python3 - "$startup_json" "$flood_json" "$ansi_json" "$rss_tsv" "$rss_json" "$idle_tsv" "$idle_json" "$peer_status_tsv" "$metric_skips_tsv" "$live_json" "$score_json" <<'PY' || score_status=$?
import json
import sys
from pathlib import Path

(
    startup_path,
    flood_path,
    ansi_path,
    rss_tsv_path,
    rss_path,
    idle_tsv_path,
    idle_path,
    peer_status_path,
    metric_skips_path,
    live_path,
    score_path,
) = map(Path, sys.argv[1:12])


def load_medians(path):
    with path.open("r", encoding="utf-8") as source:
        doc = json.load(source)
    return {row["command"]: float(row["median"]) for row in doc.get("results", [])}


def median(values):
    ordered = sorted(values)
    count = len(ordered)
    if count == 0:
        raise ValueError("empty median input")
    midpoint = count // 2
    if count % 2:
        return float(ordered[midpoint])
    return (float(ordered[midpoint - 1]) + float(ordered[midpoint])) / 2.0


def load_samples(path, converter):
    samples = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        name, value = line.split("\t", 1)
        samples.setdefault(name, []).append(converter(value))
    return samples, {name: median(values) for name, values in samples.items()}


peer_order = []
peer_status = {}
for line in peer_status_path.read_text(encoding="utf-8").splitlines():
    name, status, reason = line.split("\t", 2)
    peer_order.append(name)
    peer_status[name] = {"status": status, "reason": reason}

specific_skips = {}
for line in metric_skips_path.read_text(encoding="utf-8").splitlines():
    metric, name, reason = line.split("\t", 2)
    specific_skips.setdefault(metric, {})[name] = reason


def skipped_for(metric, values):
    skipped = []
    for name in peer_order:
        if name in values:
            continue
        status = peer_status[name]
        reason = status["reason"] if status["status"] == "skipped" else ""
        reason = specific_skips.get(metric, {}).get(name, reason)
        if not reason:
            reason = "no result was produced"
        skipped.append({"terminal": name, "reason": reason})
    return skipped


def score_metric(metric, unit, values):
    ranking = [
        {
            "rank": 1 + sum(other < value for other in values.values()),
            "terminal": name,
            "value": value,
        }
        for name, value in sorted(values.items(), key=lambda item: (item[1], item[0]))
    ]
    terminal_count = len(values)
    competitor_count = sum(name != "kettle" for name in values)
    cutoff = (terminal_count + 1) // 2
    if "kettle" in values:
        kettle_value = values["kettle"]
        kettle_rank = 1 + sum(value < kettle_value for value in values.values())
    else:
        kettle_rank = None
    eligible = kettle_rank is not None and competitor_count >= 1
    top_half = eligible and kettle_rank <= cutoff
    if kettle_rank is None:
        eligibility_reason = "Kettle was not successfully measured"
    elif competitor_count == 0:
        eligibility_reason = "no real competitor was successfully measured"
    else:
        eligibility_reason = None
    return {
        "unit": unit,
        "values": values,
        "ranking": ranking,
        "kettle_rank": kettle_rank,
        "terminal_count": terminal_count,
        "competitor_count": competitor_count,
        "eligible_for_pass": eligible,
        "ineligible_reason": eligibility_reason,
        "top_half_cutoff": cutoff,
        "kettle_top_half": top_half,
        "skipped": skipped_for(metric, values),
    }


timings = {
    "startup": load_medians(startup_path),
    "ascii_flood": load_medians(flood_path),
    "ansi_underline_flood": load_medians(ansi_path),
}
rss_samples, rss_medians = load_samples(rss_tsv_path, int)
idle_samples, idle_medians = load_samples(idle_tsv_path, float)

metrics = {
    "startup": score_metric("startup", "seconds", timings["startup"]),
    "ascii_flood": score_metric("ascii_flood", "seconds", timings["ascii_flood"]),
    "ansi_underline_flood": score_metric(
        "ansi_underline_flood", "seconds", timings["ansi_underline_flood"]
    ),
    "memory_rss": score_metric("memory_rss", "bytes", rss_medians),
    "idle_cpu": score_metric("idle_cpu", "percent", idle_medians),
}

rss_path.write_text(
    json.dumps(
        {
            "workload": "memory_rss",
            "unit": "bytes",
            "method": "/usr/bin/time -l maximum resident set size over ascii-flood",
            "samples_bytes": rss_samples,
            "median_bytes": rss_medians,
            "skipped": metrics["memory_rss"]["skipped"],
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)
idle_path.write_text(
    json.dumps(
        {
            "workload": "idle_cpu",
            "unit": "percent",
            "method": "direct terminal parent PID or owning AppleScript app PID; 3s settle; five ps -o %cpu= samples 1s apart",
            "samples_percent": idle_samples,
            "median_percent": idle_medians,
            "skipped": metrics["idle_cpu"]["skipped"],
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)

measured = [name for name, data in metrics.items() if data["kettle_rank"] is not None]
eligible = [name for name, data in metrics.items() if data["eligible_for_pass"]]
ineligible = [name for name in measured if not metrics[name]["eligible_for_pass"]]
top_half = [name for name in eligible if metrics[name]["kettle_top_half"]]
passed = len(top_half) >= 3
failures = []
if not passed:
    failures.append(
        f"kettle was top-half on {len(top_half)} of {len(eligible)} eligible metrics; "
        "3 required"
    )
    if ineligible:
        failures.append(
            f"{len(ineligible)} Kettle-measured metric(s) were ineligible because no real "
            "competitor was successfully measured"
        )
warnings = []

try:
    live_doc = json.loads(live_path.read_text(encoding="utf-8"))
    if "error" in live_doc:
        raise ValueError(live_doc["error"])
    live_summary = {
        "resize_median_ms": live_doc["resize"]["median_ms"],
        "resize_p95_ms": live_doc["resize"]["p95_ms"],
        "scroll_page_up_median_ms": live_doc["scrollback_navigation"]["page_up_median_ms"],
        "scroll_page_down_median_ms": live_doc["scrollback_navigation"]["page_down_median_ms"],
        "max_observed_display_offset": live_doc["scrollback_navigation"]["max_observed_display_offset"],
        "advisory": True,
    }
except Exception as exc:
    live_summary = {"error": f"failed to read {live_path}: {exc}", "advisory": True}
    warnings.append(live_summary["error"])

summary = {
    "startup_json": str(startup_path),
    "ascii_flood_json": str(flood_path),
    "ansi_underline_flood_json": str(ansi_path),
    "memory_rss_json": str(rss_path),
    "idle_cpu_json": str(idle_path),
    "kettle_live_json": str(live_path),
    "intended_metric_count": 5,
    "measured_metric_count": len(measured),
    "eligible_metric_count": len(eligible),
    "ineligible_metric_count": len(ineligible),
    "top_half_metric_count": len(top_half),
    "metrics": metrics,
    "kettle_live": live_summary,
    "not_measured": {
        "input_latency": {
            "measured": False,
            "reason": "out of scope: AppleScript-only terminals lack a comparable keystroke-to-paint observation path",
        }
    },
    "rules": {
        "metric_rank": "lower is better; ties share Kettle's rank via count of strictly lower values",
        "eligibility": "Kettle and at least one real competitor must both have a result",
        "top_half": "on eligible metrics, kettle rank <= ceil(N / 2), where N is the number of terminals with a result",
        "pass": "kettle is top-half on at least 3 eligible metrics",
        "kettle_live": "advisory Kettle-only resize/scrollback medians; probe failure is recorded but is not part of the rank gate",
        "input_latency": "deliberately not measured and is not a pass",
    },
    "passed": passed,
    "failures": failures,
    "warnings": warnings,
}
score_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")


def display_value(unit, value):
    if unit == "seconds":
        return f"{value:.3f} s"
    if unit == "bytes":
        return f"{value / (1024.0 * 1024.0):.1f} MiB"
    return f"{value:.2f}%"


print("")
print("macOS terminal ranking (lower is better)")
for metric, data in metrics.items():
    print("")
    print(metric)
    print(
        f"  Measured terminals: {data['terminal_count']}; "
        f"real competitors measured: {data['competitor_count']}"
    )
    print(f"  {'rank':>4}  {'terminal':<12} {'value':>12}")
    for row in data["ranking"]:
        print(
            f"  {row['rank']:>4}  {row['terminal']:<12} "
            f"{display_value(data['unit'], row['value']):>12}"
        )
    for skipped in data["skipped"]:
        print(f"  SKIP  {skipped['terminal']:<12} {skipped['reason']}")
    if data["kettle_rank"] is None:
        print("  Kettle: INELIGIBLE - not measured; this metric does not count toward pass")
    elif not data["eligible_for_pass"]:
        print(
            f"  Kettle: rank {data['kettle_rank']}/{data['terminal_count']}; "
            "INELIGIBLE - 0 real competitors measured; this metric does not count toward pass"
        )
    else:
        result = "YES" if data["kettle_top_half"] else "NO"
        print(
            f"  Kettle: rank {data['kettle_rank']}/{data['terminal_count']}; "
            f"top-half cutoff {data['top_half_cutoff']} -> {result}"
        )

print("")
print(f"Measured {len(measured)} of 5 intended metrics.")
print(
    f"Eligible comparison metrics: {len(eligible)}; "
    f"excluded for having no measured competitor: {len(ineligible)}."
)
print(f"Kettle was top-half on {len(top_half)} of {len(eligible)} eligible metrics.")
print("input_latency: NOT MEASURED (deliberately out of scope; absence is not a pass).")
if warnings:
    print("WARNINGS:")
    for warning in warnings:
        print(f"  - {warning}")
if passed:
    print(f"PASS: wrote {score_path}")
else:
    print("FAILED:")
    for failure in failures:
        print(f"  - {failure}")
    sys.exit(1)
PY

echo ""
echo "results:"
echo "  $startup_json"
echo "  $flood_json"
echo "  $ansi_json"
echo "  $rss_json"
echo "  $idle_json"
echo "  $live_json"
echo "  $score_json"

exit "$score_status"
