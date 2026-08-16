# kettle shell integration (OSC 133) — fish
#
# Source from your ~/.config/fish/config.fish to enable prompt-mark
# navigation (`Ctrl+Up` / `Ctrl+Down` jump between prompt starts in
# kettle). If your prompt already emits OSC 133, Kettle picks it up
# automatically and this snippet is unnecessary.
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

set -q __kettle_completion_session; or set -g __kettle_completion_session 0
set -q __kettle_completion_request; or set -g __kettle_completion_request 0
set -q __kettle_completion_generation; or set -g __kettle_completion_generation 0
set -g __kettle_completion_counter_max 4503599627370495
set -g __kettle_completion_enabled 1

function __kettle_prompt --on-event fish_prompt
    __kettle_completion_clear
    printf '\e]133;A\a'
    if __kettle_completion_begin_session
        printf '\e]777;kettle-completion;3;sync;%d;%d\a' \
            $__kettle_completion_session (__kettle_completion_key_mask)
    end
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
# opens the compact card without editing the token, then Tab / Shift-Tab cycle.
function __kettle_completion_field --argument value limit
    # Most fields already fit. Encode and count them in two bulk builtin calls;
    # the scalar-by-scalar path below is only for actual truncation. This keeps
    # completion responsive on Fish releases where command substitutions carry
    # noticeably more overhead.
    set -l encoded (string escape --style=url -- $value)
    set -l encoded_bytes (string length -- (string replace --all --regex '%[0-9A-Fa-f]{2}' x -- $encoded))
    if test $encoded_bytes -le $limit
        printf '%s' $encoded
        return
    end

    set encoded ''
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
    set -e __kettle_completion_cycle_insertions
    set -e __kettle_completion_cycle_line
    set -e __kettle_completion_cycle_cursor
    set -g __kettle_completion_cycle_index -1
end

function __kettle_completion_begin_session
    if test $__kettle_completion_enabled -ne 1; or test $__kettle_completion_session -ge $__kettle_completion_counter_max
        set -g __kettle_completion_enabled 0
        __kettle_completion_reset
        return 1
    end
    set -g __kettle_completion_session (math $__kettle_completion_session + 1)
    set -g __kettle_completion_request 0
end

function __kettle_completion_begin_request
    if test $__kettle_completion_enabled -ne 1; or test $__kettle_completion_request -ge $__kettle_completion_counter_max
        # Fish cannot represent the next integer exactly. Keep consuming the
        # binding without publishing or editing until the next prompt resets
        # the session; reusing the last id would admit a delayed old reply.
        __kettle_completion_reset
        return 1
    end
    set -g __kettle_completion_request (math $__kettle_completion_request + 1)
end

function __kettle_completion_begin_generation
    if test $__kettle_completion_enabled -ne 1; or test $__kettle_completion_generation -ge $__kettle_completion_counter_max
        set -g __kettle_completion_enabled 0
        __kettle_completion_reset
        return 1
    end
    set -g __kettle_completion_generation (math $__kettle_completion_generation + 1)
end

function __kettle_completion_source_bytes --argument value
    # Callers apply a character cap first, so the temporary URL-escaped copy is
    # bounded too. Replacing each `%HH` triplet with one byte gives the exact
    # UTF-8 source size while staying compatible with Fish 3.7.
    set -l escaped (string escape --style=url -- $value)
    string length -- (string replace --all --regex '%[0-9A-Fa-f]{2}' x -- $escaped)
end

# Fish 3.7 does not autoload a command's completion file for `complete -C`.
# Its stock Tab path does that before querying candidates, so mirror that one
# step when no user or plugin completion is already registered. Fish 4 does
# not need this, but the check makes it a no-op there.
function __kettle_completion_prime
    set -l major (string split . -- $version)[1]
    if string match --regex --quiet '^[0-9]+$' -- "$major"; and test "$major" -ge 4
        return
    end

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
    # `--escape` returns the exact editor spelling Fish computed with its
    # replacement and quoting flags. Plain `complete -C` discards those flags:
    # re-escaping its text turns expandable `~user` into a quoted literal.
    complete --escape -C (commandline -cp | string collect) 2>/dev/null
end

function __kettle_completion_clear
    __kettle_completion_reset
    __kettle_completion_begin_generation; or return
    printf '\e]777;kettle-completion;3;clear;%d;%d;%d\a' \
        $__kettle_completion_session $__kettle_completion_generation $__kettle_completion_request
end

function __kettle_completion_emit_rows --argument operation selected offset total request
    set -q __kettle_completion_generation; or set -g __kettle_completion_generation 0
    __kettle_completion_begin_generation; or return
    set -l rows $argv[6..-1]
    if test (count $rows) -eq 0
        printf '\e]777;kettle-completion;3;clear;%d;%d;%d\a' \
            $__kettle_completion_session $__kettle_completion_generation $__kettle_completion_request
        return
    end

    set -l body
    set -l count 0
    set -l row_index 0
    set -l normalized_selected ''
    for row in $rows
        test $count -ge 64; and break
        set -l pair (string split --max 1 \t -- $row)
        set -l label (__kettle_completion_field $pair[1] 64)
        if test -z "$label"
            set row_index (math $row_index + 1)
            continue
        end
        set -l description
        if test (count $pair) -gt 1
            set description (__kettle_completion_field $pair[2] 256)
        end
        set -l addition ";$label;$description"
        test (math 192 + (string length "$body$addition")) -le 65000; or break
        if string match --regex --quiet '^[0-9]+$' -- "$selected"; and test "$selected" -eq $row_index
            set normalized_selected $count
        end
        set body "$body$addition"
        set count (math $count + 1)
        set row_index (math $row_index + 1)
    end
    if test $count -eq 0
        printf '\e]777;kettle-completion;3;clear;%d;%d;%d\a' \
            $__kettle_completion_session $__kettle_completion_generation $__kettle_completion_request
    else
        set selected $normalized_selected
        set -l payload
        if string match --regex --quiet '^[0-9]+$' -- "$request"
            set payload "777;kettle-completion;3;$operation;$__kettle_completion_session;$__kettle_completion_generation;$request;completion;$selected;fish;$offset;$total$body"
        else
            set payload "777;kettle-completion;2;$operation;$__kettle_completion_generation;completion;$selected;fish;$offset;$total$body"
        end
        printf '\e]%s\a' $payload
    end
end

# Store one bounded result so cycling never re-queries after the commandline
# changes. Count, per-field, and aggregate source limits prevent a broad or
# hostile completion provider from turning one Tab into unbounded retained
# state. The first bounded candidates stay detached; Kettle never opens Fish's
# inline pager for an overflow result.
function __kettle_completion_capture
    __kettle_completion_reset
    set -l rows
    set -l labels
    set -l insertions
    set -l source_bytes 0
    __kettle_completion_rows | while read -l -n 20482 row
        if test (count $rows) -ge 2048
            break
        end
        string match --regex --quiet '^(\t|$)' -- $row; and continue
        set -l pair (string split --max 1 \t -- $row)
        set -l insertion $pair[1]
        set -l label (string unescape -- $insertion 2>/dev/null | string collect)
        if test (count $label) -eq 0; or string match --regex --quiet '[[:cntrl:]]' -- $label
            # Unix filenames may contain newlines. Keep Fish's escaped spelling
            # as the safe display label; command substitution would otherwise
            # split the decoded value and desynchronize the parallel arrays.
            set label $insertion
        end
        set -l description ''
        if test (count $pair) -gt 1
            set description $pair[2]
        end
        # Bound the temporary URL-escaped value before measuring exact bytes.
        if test (string length -- $label) -gt 4096; or test (string length -- $description) -gt 16384
            break
        end
        set -l row_bytes (math (__kettle_completion_source_bytes $label) + (__kettle_completion_source_bytes $description))
        if test $row_bytes -gt 16384; or test (math $source_bytes + $row_bytes) -gt 262144
            break
        end
        set source_bytes (math $source_bytes + $row_bytes)
        set -a rows (string join \t -- $label $description)
        set -a labels $label
        set -a insertions $insertion
    end
    set -g __kettle_completion_cycle_rows $rows
    set -g __kettle_completion_cycle_labels $labels
    set -g __kettle_completion_cycle_insertions $insertions
end

# The wire message is bounded to 64 candidates. For a larger retained result,
# publish the page containing the selected candidate without opening Fish's
# inline pager.
function __kettle_completion_emit_cycle --argument operation selected
    set -q __kettle_completion_request; or set -g __kettle_completion_request 0
    set -l rows $__kettle_completion_cycle_rows
    set -l total (count $rows)
    if test $total -eq 0
        __kettle_completion_clear
        return
    end
    set -l first 1
    set -l relative ''
    if string match --regex --quiet '^[0-9]+$' -- "$selected"
        set first (math "floor($selected / 64) * 64 + 1")
        set relative (math "$selected - $first + 1")
    end
    set -l last (math "min($first + 63, $total)")
    __kettle_completion_emit_rows $operation $relative (math $first - 1) $total $__kettle_completion_request $rows[$first..$last]
end

# Insert the exact escaped spelling returned by `complete --escape` without
# asking its provider a second time, then reconstruct Fish's hidden NO_SPACE
# decision for the standard completion sources.
function __kettle_completion_insert_captured --argument insertion candidate append_delimiter
    commandline -rt -- $insertion
    test "$append_delimiter" = 1; or return
    # `complete --escape` preserves Fish's exact token spelling but its public
    # output omits NO_SPACE. Mirror every native source of that flag: open
    # paths/options, variables, users, abbreviations, and unclosed brace lists.
    string match --regex --quiet '(^[~$]|[/=@:.,-]$)' -- $candidate; and return
    abbr --query -- $candidate 2>/dev/null; and return
    string match --quiet '*{*' -- (commandline -ct); and not string match --quiet '*}*' -- (commandline -ct); and return

    set -l line (commandline -b)
    set -l cursor (commandline -C)
    set -l following (string sub --start (math $cursor + 1) --length 1 -- $line)
    test "$following" = ' '; or commandline -i ' '
end

function __kettle_completion_cycle --argument direction
    set -q __kettle_completion_request; or set -g __kettle_completion_request 0
    __kettle_completion_begin_request; or return
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
        set -l insertion $__kettle_completion_cycle_insertions[(math $next + 1)]
        commandline -rt -- $insertion
        set -g __kettle_completion_cycle_line (commandline -b)
        set -g __kettle_completion_cycle_cursor (commandline -C)
        __kettle_completion_emit_cycle update $next
        return
    end

    __kettle_completion_capture
    set count (count $__kettle_completion_cycle_labels)
    if test $count -eq 0
        __kettle_completion_clear
        return
    end
    if test $count -eq 1
        # Use the captured candidate instead of handing Tab back to Fish. A
        # second provider query is not guaranteed to return the same singleton;
        # if it grows, Fish can open its inline pager beside the command line.
        # The captured row is already the source used by the detached card, so
        # direct insertion keeps the edit and presentation in lockstep. The
        # helper restores Fish's ordinary unique-result delimiter without
        # reopening the provider.
        set -g __kettle_completion_cycle_index 0
        __kettle_completion_insert_captured $__kettle_completion_cycle_insertions[1] $__kettle_completion_cycle_labels[1] 1
        set -g __kettle_completion_cycle_line (commandline -b)
        set -g __kettle_completion_cycle_cursor (commandline -C)
        __kettle_completion_emit_cycle show 0
        return
    end

    if test $direction -lt 0
        set -g __kettle_completion_cycle_index (math $count - 1)
        commandline -rt -- $__kettle_completion_cycle_insertions[-1]
        set -g __kettle_completion_cycle_line (commandline -b)
        set -g __kettle_completion_cycle_cursor (commandline -C)
        __kettle_completion_emit_cycle show $__kettle_completion_cycle_index
        return
    end

    # Keep the first ambiguous result wholly detached. Editing a common prefix
    # here recreates the inline suggestion the overlay is meant to replace.
    set -g __kettle_completion_cycle_line (commandline -b)
    set -g __kettle_completion_cycle_cursor (commandline -C)
    set -g __kettle_completion_cycle_index -1
    __kettle_completion_emit_cycle show ''
end

function __kettle_complete
    __kettle_completion_cycle 1
end

function __kettle_complete_previous
    __kettle_completion_cycle -1
end

function __kettle_completion_key_mask
    set -l mode default
    set -q fish_bind_mode; and set mode $fish_bind_mode
    set -l tab (printf '\t')
    set -l backtab (printf '\e[Z')
    set -l tab_binding (bind --user -M $mode "$tab" 2>/dev/null | string collect)
    set -l ctrl_i_binding (bind --user -M $mode ctrl-i 2>/dev/null | string collect)
    set -l backtab_binding (bind --user -M $mode "$backtab" 2>/dev/null | string collect)
    set -l shift_tab_binding (bind --user -M $mode shift-tab 2>/dev/null | string collect)
    set -l mask 0
    if string match --quiet '* __kettle_complete' "$tab_binding"; or string match --quiet '* __kettle_complete' "$ctrl_i_binding"
        set mask 1
    end
    if string match --quiet '* __kettle_complete_previous' "$backtab_binding"; or string match --quiet '* __kettle_complete_previous' "$shift_tab_binding"
        set mask (math $mask + 2)
    end
    printf '%d' $mask
end

# A Fish Vi keymap can change without a new prompt. Tell Kettle which raw keys
# still reach these handlers before the next keypress is classified.
function __kettle_completion_keymap_changed --on-variable fish_bind_mode
    test $__kettle_completion_enabled -eq 1; or return
    test $__kettle_completion_session -gt 0; or return
    __kettle_completion_clear
    printf '\e]777;kettle-completion;3;keymap;%d;%d\a' \
        $__kettle_completion_session (__kettle_completion_key_mask)
end

# Replace only Fish's stock Tab behavior. A custom user or plugin binding keeps
# ownership and does not publish detached completion metadata: the terminal
# cannot safely count a request for a handler it does not own. The terminal sets
# the capability variable from `completion-overlay`, so `off` leaves Fish
# byte-for-byte stock rather than installing an invisible interaction.
if status is-interactive; and test "$TERM_PROGRAM" = kettle; and test "$KETTLE_COMPLETION_OVERLAY" = 1; and test -z "$TMUX"; and test -z "$STY"
    # Emacs edits in `default`; Vi edits in `insert`. Replace the stock
    # completion binding in either map, but leave every user/plugin binding
    # alone. Re-sourcing is idempotent because our own binding is recognized.
    # Query Tab by its byte. Fish 3.7 treats the name `tab` as three literal
    # letters, while Fish 4.2 prints the same byte back as the symbolic name.
    set -l __kettle_tab (printf '\t')
    set -l __kettle_backtab (printf '\e[Z')
    set -l __kettle_fish_version (string split . -- $version)
    set -l __kettle_fish_major $__kettle_fish_version[1]
    set -l __kettle_fish_minor $__kettle_fish_version[2]
    for __kettle_mode in default insert
        set -l __kettle_user_tab (bind --user -M $__kettle_mode "$__kettle_tab" 2>/dev/null | string collect)
        set -l __kettle_preset_tab (bind --preset -M $__kettle_mode "$__kettle_tab" 2>/dev/null | string collect)
        set -l __kettle_user_ctrl_i (bind --user -M $__kettle_mode ctrl-i 2>/dev/null | string collect)
        set -l __kettle_preset_ctrl_i (bind --preset -M $__kettle_mode ctrl-i 2>/dev/null | string collect)
        set -l __kettle_owns_tab 0
        if string match --quiet '* __kettle_complete' "$__kettle_user_tab"; or string match --quiet '* __kettle_complete' "$__kettle_user_ctrl_i"
            set __kettle_owns_tab 1
        else if test -z "$__kettle_user_tab"; and test -z "$__kettle_user_ctrl_i"
            # Fish 4.0 through 4.2 expose the same Vi insert-mode byte as both
            # `ctrl-i` and `tab`. A user binding for either leaves the other
            # stock entry ahead of it; binding both adds a one-second sequence
            # delay. Remove only the duplicate stock alias, then install one
            # byte binding. Fish 4.3 fixed the aliasing.
            if test "$__kettle_mode" = insert; and test "$__kettle_fish_major" = 4; and test "$__kettle_fish_minor" -le 2; and string match --regex --quiet ' complete$' "$__kettle_preset_ctrl_i"
                bind --erase --preset -M $__kettle_mode ctrl-i
            end
            if string match --regex --quiet ' complete$' "$__kettle_preset_tab"
                bind -M $__kettle_mode "$__kettle_tab" __kettle_complete
                set __kettle_owns_tab 1
            end
        end

        if test $__kettle_owns_tab = 1

            # Fish 4 resolves CSI Z to the symbolic `shift-tab` binding. Fish
            # 3.7 needs the raw sequence. Install both spellings, but only at a
            # spelling the user has not claimed.
            set -l __kettle_user_backtab (bind --user -M $__kettle_mode "$__kettle_backtab" 2>/dev/null | string collect)
            if test -z "$__kettle_user_backtab"; or string match --quiet '* __kettle_complete_previous' "$__kettle_user_backtab"
                bind -M $__kettle_mode "$__kettle_backtab" __kettle_complete_previous
            end
            set -l __kettle_user_shift_tab (bind --user -M $__kettle_mode shift-tab 2>/dev/null | string collect)
            if test -z "$__kettle_user_shift_tab"; or string match --quiet '* __kettle_complete_previous' "$__kettle_user_shift_tab"
                bind -M $__kettle_mode shift-tab __kettle_complete_previous
            end
        end
    end
end
