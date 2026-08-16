# kettle shell integration (OSC 133) — bash
#
# Source from your ~/.bashrc to enable prompt-mark navigation
# (`Ctrl+Up` / `Ctrl+Down` jump between prompt starts in kettle).
# If your prompt already emits OSC 133, Kettle picks it up automatically and
# this snippet is unnecessary.
#
# One-line install:
#
#     kettle --shell-integration bash >> ~/.bashrc
#
# Marks emitted:
#   OSC 133;A   prompt start (used for jump targets)
#   OSC 133;B   end of prompt / input start
#   OSC 133;C   command started executing
#   OSC 133;D;N command finished (exit code N)
#   OSC 7       current working directory (v2.20: powers new-tab/split cwd
#               inheritance and "Open folder"; kettle validates the hostname
#               so an ssh session's remote cwd is never adopted locally)

# Percent-encode $PWD byte-by-byte (LC_ALL=C makes ${p:i:1} a BYTE, so
# multi-byte UTF-8 path characters encode as their UTF-8 byte sequence).
__kettle_osc7() {
  local LC_ALL=C p="$PWD" out='' i ch byte
  for (( i = 0; i < ${#p}; i++ )); do
    ch="${p:i:1}"
    case "$ch" in
      [A-Za-z0-9/:_.~-]) out+="$ch" ;;
      *)
        printf -v byte '%d' "'$ch"
        printf -v ch '%%%02X' "$(( byte & 0xFF ))"
        out+="$ch"
        ;;
    esac
  done
  printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$out"
}

__kettle_completion_encode() {
  local LC_ALL=C
  local p="$1" out='' i=0 j ch byte need valid len=${#1} limit=$2
  while (( i < len && i < limit )); do
    ch="${p:i:1}"
    printf -v byte '%d' "'$ch"
    byte=$(( byte & 0xFF ))
    if (( byte < 0x80 )); then
      need=1
    elif (( byte >= 0xC2 && byte <= 0xDF )); then
      need=2
    elif (( byte >= 0xE0 && byte <= 0xEF )); then
      need=3
    elif (( byte >= 0xF0 && byte <= 0xF4 )); then
      need=4
    else
      break
    fi
    (( i + need <= len && i + need <= limit )) || break
    valid=1
    for (( j = 1; j < need; j++ )); do
      ch="${p:i+j:1}"
      printf -v byte '%d' "'$ch"
      byte=$(( byte & 0xFF ))
      (( byte >= 0x80 && byte <= 0xBF )) || valid=0
    done
    (( valid )) || break
    for (( j = 0; j < need; j++ )); do
      ch="${p:i+j:1}"
      printf -v byte '%d' "'$ch"
      byte=$(( byte & 0xFF ))
      case "$ch" in
        [A-Za-z0-9_.~-]) out+="$ch" ;;
        *)
          printf -v ch '%%%02X' "$byte"
          out+="$ch"
          ;;
      esac
    done
    (( i += need ))
  done
  if [[ -n "${3-}" ]]; then
    printf -v "$3" '%s' "$out"
  else
    printf '%s' "$out"
  fi
}

# Cooperative bridge for Bash completion frameworks. Call with
#   kettle_completion_show KIND SOURCE SELECTED LABEL DESCRIPTION [...]
# where SELECTED is empty or a zero-based row. Kettle presents the list but
# never inserts or executes it.
kettle_completion_show() {
  local LC_ALL=C kind="${1:-completion}" source="${2:-bash}" selected="$3"
  shift 3 || return
  local payload body='' label description addition count=0
  case "$kind" in
    completion|prediction) ;;
    *) kind=completion ;;
  esac
  if [[ ! "$selected" =~ ^[0-9][0-9]?$ ]] || (( 10#$selected >= 64 )); then
    selected=''
  fi
  __kettle_completion_generation=$(( ${__kettle_completion_generation:-0} + 1 ))
  __kettle_completion_encode "$source" 64 source
  while (( $# >= 2 && count < 64 )); do
    __kettle_completion_encode "$1" 64 label
    __kettle_completion_encode "$2" 256 description
    shift 2
    [[ -n "$label" ]] || continue
    addition=";$label;$description"
    (( 128 + ${#source} + ${#body} + ${#addition} <= 30000 )) || break
    body+="$addition"
    (( count++ ))
  done
  if (( count == 0 )); then
    kettle_completion_clear
  else
    if [[ -n "$selected" ]] && (( 10#$selected >= count )); then
      selected=''
    fi
    payload="777;kettle-completion;1;show;$__kettle_completion_generation;$kind;$selected;$source$body"
    printf '\033]%s\007' "$payload"
  fi
}

kettle_completion_clear() {
  __kettle_completion_generation=$(( ${__kettle_completion_generation:-0} + 1 ))
  printf '\033]777;kettle-completion;1;clear;%s\007' "$__kettle_completion_generation"
}
# Capture the command's status FIRST, and hand it back on the way out.
#
# kettle deliberately runs first in PROMPT_COMMAND so its own `$?` read is the
# real one — but it used to end on a successful `printf`, so every segment
# chained after it saw `$?` as 0. Anything that colours a prompt by exit
# status, or appends `[$?]`, silently reported success after a failing command
# purely because kettle was installed. Returning the saved status makes this
# hook transparent to whatever follows it.
__kettle_pc() {
  local __kettle_status=$?
  kettle_completion_clear
  printf '\033]133;D;%s\007\033]133;A\007' "$__kettle_status"
  __kettle_osc7
  return "$__kettle_status"
}
# bash 5.1 allows PROMPT_COMMAND to be an ARRAY. The string form below happens
# to survive that — bash assigns a plain string to index 0 and leaves the later
# elements alone, so they still run — but only by accident, and it rewrites the
# user's first element into a compound string. Prepending in kind says what is
# meant. Verified both ways against a real bash: every segment runs exactly
# once.
#
# `declare -p` is checked rather than `${PROMPT_COMMAND@a}` because that
# transformation is itself 5.1-only and is a parse error on the bash 5.0 Ubuntu
# 20.04 ships and the 3.2 macOS ships. Converting a scalar to an array would be
# worse than doing nothing, since bash below 5.1 ignores an array-valued
# PROMPT_COMMAND entirely.
# shellcheck disable=SC2128,SC2178 # The fallback arm is specifically scalar.
case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
  "declare -a"*) PROMPT_COMMAND=(__kettle_pc "${PROMPT_COMMAND[@]}") ;;
  *) PROMPT_COMMAND="__kettle_pc${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac
PS1='\[\033]133;B\007\]'"$PS1"
trap 'printf "\033]133;C\007"' DEBUG
