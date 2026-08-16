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
    __kettle_completion_clear
    printf '\e]133;A\a'
    __kettle_osc7
end
function __kettle_preexec --on-event fish_preexec
    __kettle_completion_clear
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

# Kettle's completion card uses Fish's own candidates, quoting, and insertion
# rules. Ambiguous lists stay out of Fish's full-width pager: the first Tab
# completes the common prefix, then Tab / Shift-Tab cycle the compact card.
function __kettle_completion_field --argument value limit
    set -l encoded
    set -l bytes 0
    for character in (string split '' -- $value)
        set -l part (string escape --style=url -- $character)
        # URL escaping leaves unreserved ASCII as one column and represents
        # every other UTF-8 byte as `%HH`; normalize those triplets to count
        # decoded bytes before appending the complete Unicode scalar.
        set -l part_bytes (string length -- (string replace --all --regex '%[0-9A-Fa-f]{2}' x -- $part))
        test (math $bytes + $part_bytes) -le $limit; or break
        set encoded "$encoded$part"
        set bytes (math $bytes + $part_bytes)
    end
    printf '%s' $encoded
end

function __kettle_completion_reset
    set -e __kettle_completion_cycle_rows
    set -e __kettle_completion_cycle_labels
    set -e __kettle_completion_cycle_line
    set -e __kettle_completion_cycle_cursor
    set -g __kettle_completion_cycle_index -1
end

# Fish 3.7 does not autoload a command's completion file for `complete -C`.
# Its stock Tab path does that before querying candidates, so mirror that one
# step when no user or plugin completion is already registered. Fish 4 does
# not need this, but the check makes it a no-op there.
function __kettle_completion_prime
    set -l words (commandline -opc)
    set -l command $words[1]
    if test -z "$command"
        set command (commandline -ct)
    end
    test -n "$command"; or return
    string match --quiet '*/*' -- $command; and return
    test -z (complete -c $command 2>/dev/null | string collect); or return

    set -l completion_path $fish_complete_path
    if test (count $completion_path) -eq 0
        # Fish 3.7 has the component directories but not the combined public
        # path variable. Keep its normal user, system, vendor, bundled order.
        set completion_path \
            "$__fish_config_dir/completions" \
            "$__fish_sysconf_dir/completions" \
            "$__fish_data_dir/vendor_completions.d" \
            "$__fish_data_dir/completions" \
            "$__fish_user_data_dir/generated_completions"
    end
    for directory in $completion_path
        set -l completion "$directory/$command.fish"
        if test -r "$completion"
            source "$completion"
            return
        end
    end
end

function __kettle_completion_rows
    __kettle_completion_prime
    complete -C (commandline -cp | string collect) 2>/dev/null
end

function __kettle_completion_clear
    __kettle_completion_reset
    set -q __kettle_completion_generation; or set -g __kettle_completion_generation 0
    set -g __kettle_completion_generation (math $__kettle_completion_generation + 1)
    printf '\e]777;kettle-completion;1;clear;%d\a' $__kettle_completion_generation
end

function __kettle_completion_emit_rows --argument operation selected
    set -q __kettle_completion_generation; or set -g __kettle_completion_generation 0
    set -g __kettle_completion_generation (math $__kettle_completion_generation + 1)
    set -l rows $argv[3..-1]
    if test (count $rows) -eq 0
        printf '\e]777;kettle-completion;1;clear;%d\a' $__kettle_completion_generation
        return
    end

    set -l payload "777;kettle-completion;1;$operation;$__kettle_completion_generation;completion;$selected;fish"
    set -l count 0
    for row in $rows
        test $count -ge 64; and break
        set -l pair (string split --max 1 \t -- $row)
        set -l label (__kettle_completion_field $pair[1] 64)
        test -n "$label"; or continue
        set -l description
        if test (count $pair) -gt 1
            set description (__kettle_completion_field $pair[2] 256)
        end
        set -l addition ";$label;$description"
        test (string length "$payload$addition") -le 30000; or break
        set payload "$payload$addition"
        set count (math $count + 1)
    end
    if test $count -eq 0
        printf '\e]777;kettle-completion;1;clear;%d\a' $__kettle_completion_generation
    else
        printf '\e]%s\a' $payload
    end
end

# Store a bounded, display-ready copy so cycling never asks Fish for a subtly
# different list after the commandline has changed.
function __kettle_completion_capture
    __kettle_completion_reset
    set -l rows (__kettle_completion_rows)
    set -l count 0
    for row in $rows
        test $count -ge 64; and break
        set -l pair (string split --max 1 \t -- $row)
        set -l label $pair[1]
        test -n "$label"; or continue
        set -l description
        if test (count $pair) -gt 1
            set description $pair[2]
        end
        set -ga __kettle_completion_cycle_labels $label
        set -ga __kettle_completion_cycle_rows (string join \t -- $label $description)
        set count (math $count + 1)
    end
end

function __kettle_completion_emit
    set -l rows (__kettle_completion_rows)
    __kettle_completion_emit_rows show '' $rows
end

function __kettle_completion_cycle --argument direction
    set -l current_line (commandline -b)
    set -l current_cursor (commandline -C)
    set -l count (count $__kettle_completion_cycle_labels)
    if test $count -gt 1; and test "$current_line" = "$__kettle_completion_cycle_line"; and test "$current_cursor" = "$__kettle_completion_cycle_cursor"
        set -l previous $__kettle_completion_cycle_index
        if test $previous -lt 0; and test $direction -lt 0
            set previous $count
        end
        set -l next (math "($previous + $direction + $count) % $count")
        set -g __kettle_completion_cycle_index $next
        set -l candidate $__kettle_completion_cycle_labels[(math $next + 1)]
        commandline -rt -- (string escape -- $candidate)
        set -g __kettle_completion_cycle_line (commandline -b)
        set -g __kettle_completion_cycle_cursor (commandline -C)
        __kettle_completion_emit_rows update $next $__kettle_completion_cycle_rows
        return
    end

    # Shift-Tab only reverses an active card. Otherwise keep Fish's ordinary
    # reverse-completion behavior instead of inventing another one.
    if test $direction -lt 0
        __kettle_completion_clear
        commandline -f complete-and-search
        return
    end

    __kettle_completion_capture
    set count (count $__kettle_completion_cycle_labels)
    if test $count -eq 0
        __kettle_completion_clear
        return
    end
    if test $count -eq 1
        __kettle_completion_clear
        commandline -f complete
        return
    end

    # Fish normally inserts the longest common prefix before opening its pager.
    # Preserve that useful first-Tab step without drawing the pager itself.
    set -l prefix $__kettle_completion_cycle_labels[1]
    for candidate in $__kettle_completion_cycle_labels[2..-1]
        while test -n "$prefix"; and test (string sub --start 1 --length (string length -- $prefix) -- $candidate) != "$prefix"
            set prefix (string sub --start 1 --length (math (string length -- $prefix) - 1) -- $prefix)
        end
    end
    set -l token (string unescape -- (commandline -ct) 2>/dev/null)
    if test (count $token) -eq 0
        set token (commandline -ct)
    end
    if test (string length -- $prefix) -gt (string length -- $token); and test (string sub --start 1 --length (string length -- $token) -- $prefix) = "$token"
        commandline -rt -- (string escape -- $prefix)
    end
    set -g __kettle_completion_cycle_line (commandline -b)
    set -g __kettle_completion_cycle_cursor (commandline -C)
    set -g __kettle_completion_cycle_index -1
    __kettle_completion_emit_rows show '' $__kettle_completion_cycle_rows
end

function __kettle_complete
    __kettle_completion_cycle 1
end

function __kettle_complete_previous
    __kettle_completion_cycle -1
end

# Replace only Fish's stock Tab behavior. A custom user or plugin binding keeps
# ownership and can call `__kettle_completion_emit` cooperatively. The terminal
# sets the capability variable from `completion-overlay`, so `off` leaves Fish
# byte-for-byte stock rather than installing an invisible interaction.
if status is-interactive; and test "$TERM_PROGRAM" = kettle; and test "$KETTLE_COMPLETION_OVERLAY" = 1
    # Emacs edits in `default`; Vi edits in `insert`. Replace the stock
    # completion binding in either map, but leave every user/plugin binding
    # alone. Re-sourcing is idempotent because our own binding is recognized.
    for __kettle_mode in default insert
        # Query the byte sequence, not the named key. Fish 3.7 treats `tab`
        # here as the three literal letters; newer Fish accepts both forms.
        set -l __kettle_user_tab (bind --user -M $__kettle_mode \t 2>/dev/null | string collect)
        set -l __kettle_preset_tab (bind --preset -M $__kettle_mode \t 2>/dev/null | string collect)
        if string match --quiet '* __kettle_complete' "$__kettle_user_tab"; or begin
                test -z "$__kettle_user_tab"; and string match --regex --quiet ' complete$' "$__kettle_preset_tab"
            end
            bind -M $__kettle_mode \t __kettle_complete
            set -l __kettle_user_backtab (bind --user -M $__kettle_mode \e\[Z 2>/dev/null | string collect)
            if test -z "$__kettle_user_backtab"; or string match --quiet '* __kettle_complete_previous' "$__kettle_user_backtab"
                bind -M $__kettle_mode \e\[Z __kettle_complete_previous
            end
        end
    end
end
