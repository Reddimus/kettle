# kettle shell integration (OSC 133) — PowerShell
#
# Source from your $PROFILE to enable prompt-mark navigation
# (Ctrl+Up / Ctrl+Down jump between prompt starts in kettle).
# If you already use Starship / oh-my-posh / posh-git, those tools
# emit OSC 133 themselves and this snippet is a no-op (the prompt
# wrapper preserves your existing prompt).
#
# One-line install (PowerShell 5.1+ on Windows, PowerShell Core 7+ on
# any OS):
#
#     kettle.exe --shell-integration powershell >> $PROFILE
#
# Marks emitted:
#   OSC 133;A    prompt start (used for jump targets)
#   OSC 133;B    end of prompt / input start
#   OSC 133;C    command started executing
#   OSC 133;D;N  command finished (exit code N)
#
# Idempotency: re-sourcing $PROFILE (e.g. after a config tweak) is a
# no-op. The $global:__kettle_prompt_installed flag prevents stacking
# multiple prompt wrappers.

if (-not $global:__kettle_prompt_installed) {
    # Stash the user's pre-existing `prompt` function so the kettle
    # wrapper calls into it instead of replacing it. Without this,
    # sourcing this snippet would clobber starship / oh-my-posh /
    # posh-git / etc.
    #
    # CRITICAL: capture the `.ScriptBlock` (a snapshot of the body), NOT the
    # `FunctionInfo` itself. Invoking a captured `FunctionInfo` with `&`
    # RE-RESOLVES the live `prompt` function — which, after we redefine it
    # below, is THIS wrapper, so `& $info` recurses into itself, throws, and
    # PowerShell re-invokes the throwing prompt forever (an infinite prompt
    # loop: the shell shows no prompt and accepts no input). A ScriptBlock is
    # a frozen copy of the original body, so `& $sb` always runs the original.
    $global:__kettle_original_prompt = (Get-Item function:prompt -ErrorAction SilentlyContinue).ScriptBlock

    function global:prompt {
        # Capture BOTH failure indicators before any other statement runs.
        #
        # `$?` reflects only the immediately preceding statement, so it must be
        # read first. `$LASTEXITCODE` must be read first too: the user's prompt
        # (starship, oh-my-posh, posh-git) routinely runs native commands, and
        # each one overwrites it. Reading it *after* rendering therefore
        # reported the prompt's own exit code — a command that failed with 37
        # followed by a prompt that shelled out successfully emitted `D;0`, so
        # command notifications and ctl/MCP `run_command` reported success for
        # a failed command.
        #
        # An array literal evaluates `$?` before the assignment resets it, so a
        # single statement captures both without either clobbering the other.
        # Every local here is `__kettle_`-prefixed on purpose. PowerShell
        # resolves variables dynamically through the call stack, so a plainly
        # named local (`$code`) declared before the user's prompt is invoked
        # would shadow a variable of that name inside their prompt.
        $__kettle_state = @($?, $global:LASTEXITCODE)
        $__kettle_ok = $__kettle_state[0]

        # `$LASTEXITCODE` is written only by NATIVE commands. A failed *cmdlet*
        # leaves it untouched, so reporting it verbatim mislabels both
        # directions: a failed `Get-Item` after a clean native command would
        # report success, and a successful cmdlet after `sh -c 'exit 37'` would
        # report 37. `$?` is the only indicator that tracks cmdlets, so it
        # decides success or failure; the numeric code is consulted only when
        # `$?` already says the command failed, and a failure with no native
        # code of its own reports 1.
        $__kettle_code = if ($__kettle_ok) {
            0
        } elseif ($__kettle_state[1]) {
            $__kettle_state[1]
        } else {
            1
        }

        # Hand the user's prompt the same `$?` an unwrapped prompt would see.
        # `$?` is read-only; failing a statement is the only way to set it
        # False, so this must be the LAST statement before the prompt runs.
        #
        # `-ErrorAction Ignore`, not `SilentlyContinue`: both set `$?`, but
        # `SilentlyContinue` also pushes a record onto `$Error`, so a prompt
        # that inspects `$Error[0]` — posh-git does — would read kettle's
        # synthetic error instead of the user's real one, and a long session
        # would push real errors out of the capped list. `Ignore` records
        # nothing, so there is nothing to clean up afterwards. Either form
        # overrides a profile-wide `$ErrorActionPreference = 'Stop'`, so this
        # cannot become a terminating error and break the prompt.
        if (-not $__kettle_ok) {
            Write-Error 'kettle: propagating command failure' -ErrorAction Ignore
        }
        try {
            $rendered = & $global:__kettle_original_prompt
        } catch {
            $rendered = $null
        }
        if ($null -eq $rendered) {
            $rendered = "PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) "
        }

        $esc = [char]27
        $bel = [char]7
        __kettle_completion_clear
        # D = last command's exit code, A = this prompt's start.
        # Emitted together at the top of the prompt function.
        [Console]::Write("$esc]133;D;$__kettle_code$bel$esc]133;A$bel")
        # OSC 7 cwd report (v2.20): powers new-tab/split cwd inheritance and
        # "Open folder" in kettle. Windows paths travel in URL form
        # (`file://HOST/C:/Users/...`, forward slashes, each segment
        # percent-encoded); kettle normalizes the drive-letter form back.
        # Only filesystem locations report (a registry/cert PSDrive cwd is
        # not a directory another pane could start in).
        $loc = $ExecutionContext.SessionState.Path.CurrentLocation
        if ($loc.Provider.Name -eq 'FileSystem') {
            $segs = $loc.ProviderPath -replace '\\', '/' -split '/'
            $enc = ($segs | ForEach-Object { [uri]::EscapeDataString($_) }) -join '/'
            # Drive paths ("C:/…") need the URL path slash prepended; a UNC
            # path ("//server/share/…") already starts with one — prepending
            # again would yield file://HOST///server/… (host-relative parse
            # breaks and cwd inheritance dies on network shares).
            if (-not $enc.StartsWith('/')) { $enc = "/$enc" }
            [Console]::Write("$esc]7;file://$env:COMPUTERNAME$enc$bel")
        }
        # Restore the exit code the user's own last command left, undoing any
        # native call the rendered prompt made. Without this, `$LASTEXITCODE`
        # typed at the next prompt reports the prompt's internals rather than
        # the command the user actually ran.
        $global:LASTEXITCODE = $__kettle_state[1]
        # B = end of prompt / input start. Emitted after the rendered
        # prompt text so the marker lands right where the user starts
        # typing. Returning it with the prompt is necessary: Console.Write
        # runs before PowerShell displays the function's returned text.
        return "$rendered$esc]133;B$bel"
    }

    function global:__kettle_completion_field([string]$Value, [int]$Limit) {
        if ($null -eq $Value) { return '' }
        try {
            # Bound UTF-8 bytes without cutting a surrogate pair or combining
            # sequence. `Substring(0, $Limit)` can create invalid UTF-16 when a
            # completion crosses the boundary on an emoji.
            $starts = [Globalization.StringInfo]::ParseCombiningCharacters($Value)
            $end = 0
            $bytes = 0
            for ($index = 0; $index -lt $starts.Count; $index++) {
                $next = if ($index + 1 -lt $starts.Count) {
                    $starts[$index + 1]
                } else {
                    $Value.Length
                }
                $part = $Value.Substring($starts[$index], $next - $starts[$index])
                $partBytes = [Text.Encoding]::UTF8.GetByteCount($part)
                if ($bytes + $partBytes -gt $Limit) { break }
                $bytes += $partBytes
                $end = $next
            }
            return [uri]::EscapeDataString($Value.Substring(0, $end))
        } catch {
            return ''
        }
    }

    function global:__kettle_completion_clear {
        $global:__kettle_completion_generation =
            [uint64]$global:__kettle_completion_generation + 1
        [Console]::Write(
            [char]27 + ']777;kettle-completion;1;clear;' +
            $global:__kettle_completion_generation + [char]7
        )
    }

    function global:__kettle_completion_emit($Result, $Selected) {
        $global:__kettle_completion_generation =
            [uint64]$global:__kettle_completion_generation + 1
        if ($null -eq $Result -or $Result.CompletionMatches.Count -eq 0) {
            __kettle_completion_clear
            return
        }
        $selectedField = if ($null -eq $Selected) { '' } else { [string]$Selected }
        $payload = '777;kettle-completion;1;show;' +
            $global:__kettle_completion_generation +
            ';completion;' + $selectedField + ';powershell'
        $count = 0
        foreach ($match in $Result.CompletionMatches) {
            if ($count -ge 64) { break }
            $label = __kettle_completion_field ([string]$match.CompletionText) 64
            if ([string]::IsNullOrEmpty($label)) { continue }
            $description = __kettle_completion_field ([string]$match.ToolTip) 256
            $addition = ";$label;$description"
            if (($payload.Length + $addition.Length) -gt 30000) { break }
            $payload += $addition
            $count++
        }
        if ($count -eq 0) {
            __kettle_completion_clear
        } else {
            [Console]::Write([char]27 + ']' + $payload + [char]7)
        }
    }

    # C = command started executing. PSReadLine (the default in
    # PowerShell 5.1+ since Windows 10 1809; bundled with PS 7) fires
    # AcceptLine when the user hits Enter — hook the stock binding to emit
    # OSC 133;C right before the command runs. PSReadLine reports a custom
    # binding's name but cannot return its ScriptBlock, so replacing one
    # would be irreversible; leave every non-stock Enter binding untouched.
    # Silently skipped if PSReadLine isn't loaded (rare; the user would have
    # disabled it on purpose).
    if (Get-Module -ListAvailable PSReadLine) {
        & {
            $enterHandler = Get-PSReadLineKeyHandler -Bound |
                Where-Object { $_.Key -eq 'Enter' } |
                Select-Object -First 1
            if ($null -ne $enterHandler -and $enterHandler.Function -eq 'AcceptLine') {
                Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
                    __kettle_completion_clear
                    [Console]::Write([char]27 + ']133;C' + [char]7)
                    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
                }
            }

            $tabHandler = Get-PSReadLineKeyHandler -Bound |
                Where-Object { $_.Key -eq 'Tab' } |
                Select-Object -First 1
            if ($env:KETTLE_COMPLETION_OVERLAY -eq '1' -and
                $null -ne $tabHandler -and
                $tabHandler.Function -eq 'TabCompleteNext') {
                Set-PSReadLineKeyHandler -Key Tab -ScriptBlock {
                    $line = $null
                    $cursor = 0
                    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState(
                        [ref]$line,
                        [ref]$cursor
                    )
                    try {
                        $result = TabExpansion2 -inputScript $line -cursorColumn $cursor
                    } catch {
                        $result = $null
                    }
                    [Microsoft.PowerShell.PSConsoleReadLine]::TabCompleteNext()
                    $after = $null
                    $afterCursor = 0
                    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState(
                        [ref]$after,
                        [ref]$afterCursor
                    )
                    $selected = $null
                    if ($null -ne $result) {
                        for ($index = 0; $index -lt $result.CompletionMatches.Count; $index++) {
                            $candidate = $line.Substring(0, $result.ReplacementIndex) +
                                $result.CompletionMatches[$index].CompletionText +
                                $line.Substring($result.ReplacementIndex + $result.ReplacementLength)
                            if ($candidate -eq $after) {
                                $selected = $index
                                break
                            }
                        }
                    }
                    try {
                        __kettle_completion_emit $result $selected
                    } catch {
                        __kettle_completion_clear
                    }
                }
            }
        }
    }

    $global:__kettle_prompt_installed = $true
}
