# kettle shell integration (OSC 133) — zsh
#
# Source from your ~/.zshrc to enable prompt-mark navigation
# (`Ctrl+Up` / `Ctrl+Down` jump between prompt starts in kettle).
# If your prompt already emits OSC 133, Kettle picks it up automatically and
# this snippet is unnecessary.
#
# One-line install:
#
#     kettle --shell-integration zsh >> ~/.zshrc
#
# Marks emitted:
#   OSC 133;A   prompt start (used for jump targets)
#   OSC 133;B   end of prompt / input start
#   OSC 133;C   command started executing
#   OSC 133;D;N command finished (exit code N)
#   OSC 7       current working directory (v2.20: powers new-tab/split cwd
#               inheritance and "Open folder"; kettle validates the hostname
#               so an ssh session's remote cwd is never adopted locally)

# Percent-encode $PWD byte-by-byte (LC_ALL=C makes ${p[i]} a BYTE, so
# multi-byte UTF-8 path characters encode as their UTF-8 byte sequence).
__kettle_osc7() {
  emulate -L zsh
  local LC_ALL=C p="$PWD" out='' i ch encoded
  for (( i = 1; i <= ${#p}; i++ )); do
    ch="${p[i]}"
    case "$ch" in
      ([A-Za-z0-9/:_.~-]) out+="$ch" ;;
      (*) printf -v encoded '%%%02X' "'$ch"; out+="$encoded" ;;
    esac
  done
  printf '\e]7;file://%s%s\a' "${HOST:-localhost}" "$out"
}

__kettle_completion_encode() {
  emulate -L zsh
  local LC_ALL=C p="$1" out='' ch encoded
  integer i=1 j byte need valid len=${#1} limit=$2
  while (( i <= len && i <= limit )); do
    ch="${p[i]}"
    printf -v byte '%d' "'$ch"
    (( byte &= 0xFF ))
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
    (( i + need - 1 <= len && i + need - 1 <= limit )) || break
    valid=1
    for (( j = 1; j < need; j++ )); do
      printf -v byte '%d' "'${p[i+j]}"
      (( byte &= 0xFF ))
      (( byte >= 0x80 && byte <= 0xBF )) || valid=0
    done
    (( valid )) || break
    for (( j = 0; j < need; j++ )); do
      ch="${p[i+j]}"
      printf -v byte '%d' "'$ch"
      (( byte &= 0xFF ))
      case "$ch" in
        ([A-Za-z0-9_.~-]) out+="$ch" ;;
        (*) printf -v encoded '%%%02X' "$byte"; out+="$encoded" ;;
      esac
    done
    (( i += need ))
  done
  if [[ -n "${3-}" ]]; then
    printf -v "$3" '%s' "$out"
  else
    print -rn -- "$out"
  fi
}

# Cooperative bridge for completion widgets and plugins. Kettle only displays
# the supplied rows; the active ZLE widget keeps insertion and execution.
kettle_completion_show() {
  emulate -L zsh
  local LC_ALL=C kind="${1:-completion}" source="${2:-zsh}" selected="$3"
  shift 3 || return
  local payload body='' label description addition
  integer count=0
  case "$kind" in
    (completion|prediction) ;;
    (*) kind=completion ;;
  esac
  if [[ "$selected" != <-> ]] || (( selected < 0 || selected >= 64 )); then
    selected=''
  fi
  typeset -gi __kettle_completion_generation
  (( __kettle_completion_generation++ ))
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
    if [[ -n "$selected" ]] && (( selected >= count )); then
      selected=''
    fi
    payload="777;kettle-completion;1;show;$__kettle_completion_generation;$kind;$selected;$source$body"
    print -rn -- $'\e]'"$payload"$'\a'
  fi
}

kettle_completion_clear() {
  typeset -gi __kettle_completion_generation
  (( __kettle_completion_generation++ ))
  print -rn -- $'\e]777;kettle-completion;1;clear;'"$__kettle_completion_generation"$'\a'
}
autoload -Uz add-zsh-hook
__kettle_precmd()  { kettle_completion_clear; print -Pn '\e]133;D;%?\a\e]133;A\a'; __kettle_osc7; }
__kettle_preexec() { print -Pn '\e]133;C\a'; }
add-zsh-hook precmd __kettle_precmd
add-zsh-hook preexec __kettle_preexec
if [[ "$PS1" != $'%{\e]133;B\a%}'* ]]; then
  PS1=$'%{\e]133;B\a%}'"$PS1"
fi
