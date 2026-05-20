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

__kettle_pc() { printf '\033]133;D;%s\007\033]133;A\007' "$?"; }
PROMPT_COMMAND="__kettle_pc${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
PS1='\[\033]133;B\007\]'"$PS1"
trap 'printf "\033]133;C\007"' DEBUG
