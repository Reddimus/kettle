# kettle shell integration (OSC 133) — bash
#
# Source from your ~/.bashrc to enable prompt-mark navigation
# (`Ctrl+Up` / `Ctrl+Down` jump between prompt starts in kettle).
# If you already use Starship / kitty / iTerm2 shell integration,
# you don't need this — those already emit OSC 133 and kettle
# picks them up automatically.
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
  local LC_ALL=C p="$PWD" out='' i ch
  for (( i = 0; i < ${#p}; i++ )); do
    ch="${p:i:1}"
    case "$ch" in
      [A-Za-z0-9/:_.~-]) out+="$ch" ;;
      *) printf -v ch '%%%02X' "'$ch"; out+="$ch" ;;
    esac
  done
  printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$out"
}
__kettle_pc() { printf '\033]133;D;%s\007\033]133;A\007' "$?"; __kettle_osc7; }
PROMPT_COMMAND="__kettle_pc${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
PS1='\[\033]133;B\007\]'"$PS1"
trap 'printf "\033]133;C\007"' DEBUG
