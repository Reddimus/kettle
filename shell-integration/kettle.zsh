# kettle shell integration (OSC 133) — zsh
#
# Source from your ~/.zshrc to enable prompt-mark navigation
# (`Ctrl+Up` / `Ctrl+Down` jump between prompt starts in kettle).
# If you already use Starship / kitty / iTerm2 shell integration,
# you don't need this — those already emit OSC 133 and kettle
# picks them up automatically.
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
  local LC_ALL=C p="$PWD" out='' i ch
  for (( i = 1; i <= ${#p}; i++ )); do
    ch="${p[i]}"
    case "$ch" in
      ([A-Za-z0-9/:_.~-]) out+="$ch" ;;
      (*) out+="$(printf '%%%02X' "'$ch")" ;;
    esac
  done
  printf '\e]7;file://%s%s\a' "${HOST:-localhost}" "$out"
}
precmd()  { print -Pn '\e]133;D;%?\a\e]133;A\a'; __kettle_osc7; }
preexec() { print -Pn '\e]133;C\a'; }
PS1='%{$(print -Pn "\e]133;B\a")%}'"$PS1"
