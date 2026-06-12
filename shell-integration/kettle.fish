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
    __kettle_osc7
end
function __kettle_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
function __kettle_postexec --on-event fish_postexec
    printf '\e]133;D;%d\a' $status
end

# OSC 7 cwd report (v2.20): powers new-tab/split cwd inheritance and
# "Open folder" in kettle; the hostname is validated terminal-side so an
# ssh session's remote cwd is never adopted locally. Segments are
# percent-encoded individually so the `/` separators stay literal.
function __kettle_osc7
    set -l enc
    for s in (string split '/' -- $PWD)
        set -a enc (string escape --style=url -- $s)
    end
    printf '\e]7;file://%s%s\a' (hostname) (string join '/' -- $enc)
end
