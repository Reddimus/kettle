#!/usr/bin/env bash
set -euo pipefail

# Local agent/TUI smoke for the real CLIs users run inside kettle.
#
# This is intentionally not a CI gate: Codex CLI, Claude Code, and a user's
# Neovim/AstroNvim setup are machine-local tools. When they are installed, this
# script proves kettle's headless PTY path can launch them, collect output, and
# exit cleanly. Missing tools are reported as skips, not failures.

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
