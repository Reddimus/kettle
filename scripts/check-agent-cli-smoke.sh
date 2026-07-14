#!/usr/bin/env bash
set -euo pipefail

# Local agent/TUI smoke for the real CLIs users run inside kettle.
#
# This is intentionally not a CI gate: Codex CLI, Claude Code, and a user's
# Neovim/AstroNvim setup are machine-local tools. The always-available checks
# prove kettle's own non-interactive agent surfaces work; optional CLI checks
# prove the real tools launch through kettle when installed. Missing tools are
# reported as skips, not failures.

KETTLE="${KETTLE_BIN:-kettle}"
TIMEOUT="${KETTLE_AGENT_SMOKE_TIMEOUT:-8}"
ran=0

have() {
  command -v "$1" >/dev/null 2>&1
}

run_and_match() {
  local label="$1"
  local pattern="$2"
  shift 2
  local out

  printf 'agent-cli smoke: %s... ' "$label"
  out="$("$KETTLE" exec --timeout "$TIMEOUT" --strip-ansi -- "$@" 2>&1)"
  if ! grep -Eiq "$pattern" <<<"$out"; then
    printf 'FAIL\n'
    printf '%s\n' "$out"
    return 1
  fi
  printf 'OK\n'
  ran=$((ran + 1))
}

run_kettle_self_check() {
  local label="$1"
  local pattern="$2"
  shift 2
  local out

  printf 'agent-cli smoke: %s... ' "$label"
  out="$("$KETTLE" "$@" 2>&1)"
  if ! grep -Eiq "$pattern" <<<"$out"; then
    printf 'FAIL\n'
    printf '%s\n' "$out"
    return 1
  fi
  printf 'OK\n'
  ran=$((ran + 1))
}

run_kettle_exec_json_match() {
  local label="$1"
  local pattern="$2"
  shift 2
  local out

  printf 'agent-cli smoke: %s... ' "$label"
  out="$("$KETTLE" exec --timeout "$TIMEOUT" --json -- "$@" 2>&1)"
  if ! grep -Eiq "$pattern" <<<"$out"; then
    printf 'FAIL\n'
    printf '%s\n' "$out"
    return 1
  fi
  printf 'OK\n'
  ran=$((ran + 1))
}

if have sh; then
  smoke_shell=(sh -lc)
  # shellcheck disable=SC2016 # The child shell, not this harness, expands these.
  env_probe='printf "TERM=%s COLORTERM=%s\n" "$TERM" "$COLORTERM"'
  json_probe='printf "kettle-agent-json-ok\n"'
elif [ "${OS:-}" = "Windows_NT" ]; then
  smoke_shell=(cmd /c)
  env_probe='echo TERM=%TERM% COLORTERM=%COLORTERM%'
  json_probe='echo kettle-agent-json-ok'
else
  echo "agent-cli smoke: missing sh; cannot run built-in PTY probes" >&2
  exit 1
fi

run_and_match \
  "Kettle exec PTY env" \
  'TERM=xterm-256color.*COLORTERM=truecolor|COLORTERM=truecolor.*TERM=xterm-256color' \
  "${smoke_shell[@]}" "$env_probe"

run_kettle_exec_json_match \
  "Kettle exec JSON output" \
  '"event":"output".*kettle-agent-json-ok|kettle-agent-json-ok.*"event":"output"' \
  "${smoke_shell[@]}" "$json_probe"

run_kettle_self_check \
  "Kettle MCP self-test" \
  'kettle mcp --self-test: OK' \
  mcp --self-test

if have codex; then
  run_and_match "Codex CLI version" 'codex' codex --version
else
  echo "agent-cli smoke: Codex CLI skipped (not on PATH)"
fi

if have claude; then
  run_and_match "Claude Code CLI version" 'claude|claude code' claude --version
else
  echo "agent-cli smoke: Claude Code CLI skipped (not on PATH)"
fi

if have nvim; then
  run_and_match \
    "Neovim TUI truecolor command path" \
    'termguicolors' \
    nvim --clean -n --cmd 'set termguicolors' '+set termguicolors?' '+qall!'
  run_and_match \
    "Neovim configured/AstroNvim command path" \
    'termguicolors' \
    nvim -n --cmd 'set termguicolors' '+set termguicolors?' '+qall!'
else
  echo "agent-cli smoke: Neovim/AstroNvim skipped (nvim not on PATH)"
fi

if [ "$ran" -eq 0 ]; then
  echo "agent-cli smoke: no optional CLIs found"
fi
