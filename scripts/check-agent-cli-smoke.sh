#!/usr/bin/env bash
set -euo pipefail

# Local agent/TUI smoke for the real CLIs users run inside kettle.
#
# macOS CI runs the always-available checks to prove kettle's own
# non-interactive agent surfaces work. Codex CLI, Claude Code, and a user's
# Neovim/AstroNvim setup remain machine-local optional probes: when installed
# they prove the real tools launch through kettle, and otherwise they report
# explicit skips. This script does not drive either client's interactive
# composer, populate a clipboard, inject paste keys, or assert an image
# attachment.

KETTLE="${KETTLE_BIN:-kettle}"
TIMEOUT="${KETTLE_AGENT_SMOKE_TIMEOUT:-8}"
ran=0

have() {
  command -v "$1" >/dev/null 2>&1
}

is_windows_host() {
  [ "${OS:-}" = "Windows_NT" ]
}

agent_cli_available() {
  local tool="$1"

  case "$tool" in
    codex|claude) ;;
    *)
      echo "agent-cli smoke: unsupported agent CLI name: $tool" >&2
      return 2
      ;;
  esac

  if is_windows_host; then
    # Git Bash prefers extensionless npm POSIX shims over their adjacent
    # `.cmd` launchers. Ask cmd.exe whether the fixed command name is runnable
    # instead of treating that non-PE shim as a native executable.
    # The doubled slash preserves cmd.exe switches across MSYS argument
    # conversion; cmd.exe receives the normal `/d /s /c` spellings.
    cmd.exe //d //s //c "where.exe $tool >NUL 2>&1"
  else
    have "$tool"
  fi
}

run_and_match() {
  local label="$1"
  local pattern="$2"
  shift 2
  local out

  printf 'agent-cli smoke: %s... ' "$label"
  out="$("$KETTLE" exec --timeout "$TIMEOUT" --strip-ansi -- "$@" 2>&1)"
  if ! grep -Eiq -- "$pattern" <<<"$out"; then
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
  if ! grep -Eiq -- "$pattern" <<<"$out"; then
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
  if ! grep -Eiq -- "$pattern" <<<"$out"; then
    printf 'FAIL\n'
    printf '%s\n' "$out"
    return 1
  fi
  printf 'OK\n'
  ran=$((ran + 1))
}

run_agent_cli_and_match() {
  local label="$1"
  local pattern="$2"
  local tool="$3"
  local probe="$4"
  local windows_command

  case "$tool:$probe" in
    codex:version)
      windows_command='codex --version'
      set -- codex --version
      ;;
    codex:help)
      windows_command='codex --help'
      set -- codex --help
      ;;
    codex:exec-help)
      windows_command='codex exec --help'
      set -- codex exec --help
      ;;
    claude:version)
      windows_command='claude --version'
      set -- claude --version
      ;;
    claude:print-help)
      windows_command='claude --print --help'
      set -- claude --print --help
      ;;
    *)
      echo "agent-cli smoke: unsupported agent CLI probe: $tool:$probe" >&2
      return 2
      ;;
  esac

  if is_windows_host; then
    # Every command string above is a fixed literal. cmd.exe selects the
    # runnable `.exe`/`.cmd` launcher via PATHEXT without handing CreateProcess
    # Git Bash's extensionless npm shell shim.
    run_and_match "$label" "$pattern" cmd.exe //d //s //c "$windows_command"
  else
    run_and_match "$label" "$pattern" "$@"
  fi
}

agent_cli_self_test() {
  if ! is_windows_host; then
    echo "agent-cli smoke self-test: Windows batch-shim fixture skipped (non-Windows host)"
    return 0
  fi

  local fixture
  fixture="$(mktemp -d "${TMPDIR:-/tmp}/kettle-agent-cli-smoke.XXXXXXXX")"
  local cleanup
  printf -v cleanup 'rm -rf -- %q' "$fixture"
  # shellcheck disable=SC2064 # Capture the function-local, shell-escaped path now.
  trap "$cleanup" EXIT

  # Poison extensionless files reproduce Git Bash's npm resolution order.
  # cmd.exe must ignore them and select the adjacent batch launchers.
  printf '%s\n' 'this is not a Windows executable' >"$fixture/codex"
  printf '%s\n' 'this is not a Windows executable' >"$fixture/claude"
  printf '%s\r\n' '@echo off' 'echo CODEX_BATCH_SHIM_OK' >"$fixture/codex.cmd"
  printf '%s\r\n' '@echo off' 'echo CLAUDE_BATCH_SHIM_OK' >"$fixture/claude.cmd"

  local codex_output claude_output
  codex_output="$(PATH="$fixture:$PATH" cmd.exe //d //s //c "codex --version")"
  claude_output="$(PATH="$fixture:$PATH" cmd.exe //d //s //c "claude --version")"
  grep -Fqx 'CODEX_BATCH_SHIM_OK' <<<"$codex_output"
  grep -Fqx 'CLAUDE_BATCH_SHIM_OK' <<<"$claude_output"
  echo "agent-cli smoke self-test: OK"
}

if [ "${1:-}" = "--self-test" ]; then
  agent_cli_self_test
  exit
fi

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

if agent_cli_available codex; then
  run_agent_cli_and_match "Codex CLI version" 'codex' codex version
  run_agent_cli_and_match \
    "Codex CLI initial-image help" \
    '--image[[:space:]]+<FILE>' \
    codex \
    help
  run_agent_cli_and_match \
    "Codex CLI non-interactive command path" \
    'usage|codex exec' \
    codex \
    exec-help
else
  echo "agent-cli smoke: Codex CLI skipped (not on PATH)"
fi

if agent_cli_available claude; then
  run_agent_cli_and_match \
    "Claude Code CLI version" \
    'claude|claude code' \
    claude \
    version
  run_agent_cli_and_match \
    "Claude Code non-interactive command path" \
    'usage|print' \
    claude \
    print-help
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

if have python3 \
  && [ "${smoke_shell[0]}" = sh ] \
  && python3 -c 'import termios,tty' >/dev/null 2>&1; then
  kitty_probe='import os,select,sys,termios,tty; old=termios.tcgetattr(0); tty.setraw(0); os.write(1,b"\x1b[?u"); ready,_,_=select.select([0],[],[],2); data=os.read(0,64) if ready else b""; termios.tcsetattr(0,termios.TCSADRAIN,old); print("KITTY_QUERY="+data.hex())'
  run_and_match \
    "Kitty keyboard capability query" \
    'KITTY_QUERY=1b5b3f3075' \
    python3 -c "$kitty_probe"
else
  echo "agent-cli smoke: Kitty keyboard PTY query skipped (Unix Python termios unavailable)"
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
