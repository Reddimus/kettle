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

precmd()  { print -Pn '\e]133;D;%?\a\e]133;A\a'; }
preexec() { print -Pn '\e]133;C\a'; }
PS1='%{$(print -Pn "\e]133;B\a")%}'"$PS1"
