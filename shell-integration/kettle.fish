# kettle shell integration (OSC 133) — fish
#
# Source from your ~/.config/fish/config.fish to enable prompt-mark
# navigation (`Ctrl+Up` / `Ctrl+Down` jump between prompt starts in
# kettle). If you already use Starship / kitty / iTerm2 shell
# integration, you don't need this — those already emit OSC 133
# and kettle picks them up automatically.
#
# One-line install:
#
#     kettle --shell-integration fish >> ~/.config/fish/config.fish
#
# Marks emitted:
#   OSC 133;A   prompt start (used for jump targets)
#   OSC 133;C   command started executing
#   OSC 133;D;N command finished (exit code N)
#
# `B` (end-of-prompt) goes inside the prompt itself — fish doesn't
# have a dedicated event for it. Append the marker to your prompt's
# trailing output if you need it; most users only need `A` for
# jump-to-prompt.

function __kettle_prompt --on-event fish_prompt
    printf '\e]133;A\a'
end
function __kettle_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
function __kettle_postexec --on-event fish_postexec
    printf '\e]133;D;%d\a' $status
end
