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
  env_probe='printf "TERM=%s COLORTERM=%s TERM_PROGRAM=%s TERM_PROGRAM_VERSION=%s\n" "$TERM" "$COLORTERM" "$TERM_PROGRAM" "$TERM_PROGRAM_VERSION"'
  json_probe='printf "kettle-agent-json-ok\n"'
elif [ "${OS:-}" = "Windows_NT" ]; then
  smoke_shell=(cmd /c)
  env_probe='echo TERM=%TERM% COLORTERM=%COLORTERM% TERM_PROGRAM=%TERM_PROGRAM% TERM_PROGRAM_VERSION=%TERM_PROGRAM_VERSION%'
  json_probe='echo kettle-agent-json-ok'
else
  echo "agent-cli smoke: missing sh; cannot run built-in PTY probes" >&2
  exit 1
fi

run_and_match \
  "Kettle exec PTY env" \
  'TERM=xterm-256color.*COLORTERM=truecolor.*TERM_PROGRAM=kettle.*TERM_PROGRAM_VERSION=[0-9]+\.[0-9]+\.[0-9]+' \
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
  run_and_match "Codex CLI non-interactive command path" 'usage|codex exec' codex exec --help
else
  echo "agent-cli smoke: Codex CLI skipped (not on PATH)"
fi

if have claude; then
  run_and_match "Claude Code CLI version" 'claude|claude code' claude --version
  run_and_match "Claude Code non-interactive command path" 'usage|print' claude --print --help
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

if have python3 && [ "${smoke_shell[0]}" = sh ]; then
  kitty_probe='import os,select,sys,termios,tty; old=termios.tcgetattr(0); tty.setraw(0); os.write(1,b"\x1b[?u"); ready,_,_=select.select([0],[],[],2); data=os.read(0,64) if ready else b""; termios.tcsetattr(0,termios.TCSADRAIN,old); print("KITTY_QUERY="+data.hex())'
  run_and_match \
    "Kitty keyboard capability query" \
    'KITTY_QUERY=1b5b3f3075' \
    python3 -c "$kitty_probe"
else
  echo "agent-cli smoke: Kitty keyboard PTY query skipped (python3/Unix shell unavailable)"
fi

if have tmux && [ "${smoke_shell[0]}" = sh ]; then
  # shellcheck disable=SC2016 # The child shell expands the socket and substitutions.
  tmux_probe='socket="kettle-agent-smoke-$$"; trap '\''tmux -L "$socket" kill-server >/dev/null 2>&1 || true'\'' EXIT; tmux -L "$socket" -f /dev/null new-session -d -s smoke; tmux -L "$socket" set-option -as terminal-features ",xterm-256color:RGB:clipboard:cstyle:extkeys:focus:hyperlinks:mouse:osc7:overline:strikethrough:sync:usstyle"; tmux -L "$socket" set-option -g extended-keys on; printf "TMUX_TERM=%s TMUX_EXTKEYS=%s TMUX_FEATURES=%s\n" "$(tmux -L "$socket" show-option -gv default-terminal)" "$(tmux -L "$socket" show-option -gv extended-keys)" "$(tmux -L "$socket" show-option -gv terminal-features | tr "\n" ",")"'
  run_and_match \
    "Tmux extended terminal feature path" \
    'TMUX_TERM=tmux-256color TMUX_EXTKEYS=on TMUX_FEATURES=.*xterm-256color:RGB:clipboard:cstyle:extkeys' \
    sh -lc "$tmux_probe"
else
  echo "agent-cli smoke: Tmux skipped (tmux/Unix shell unavailable)"
fi

if [ "$ran" -eq 0 ]; then
  echo "agent-cli smoke: no optional CLIs found"
fi
