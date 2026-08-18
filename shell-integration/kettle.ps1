# Kettle shell integration for PowerShell 5.1 and 7+. Install with:
#     kettle.exe --shell-integration powershell >> $PROFILE

if (-not $global:__kettle_prompt_installed) {
    $global:__kettle_completion_session = [uint64]0
    $global:__kettle_completion_request = [uint64]0
    $global:__kettle_completion_generation = [uint64]0
    $global:__kettle_completion_counter_max = [uint64]4503599627370495
    $global:__kettle_completion_enabled = $true
    # Capture the body, not FunctionInfo: the latter re-resolves the live
    # `prompt` after this wrapper replaces it and recurses forever.
    $global:__kettle_original_prompt = (Get-Item function:prompt -ErrorAction SilentlyContinue).ScriptBlock

    function global:prompt {
        # Capture status before a dynamically scoped prompt can overwrite it.
        $__kettle_state = @($?, $global:LASTEXITCODE)
        $__kettle_ok = $__kettle_state[0]

        # `$?` covers cmdlets; `$LASTEXITCODE` supplies native numeric status.
        $__kettle_code = if ($__kettle_ok) {
            0
        } elseif ($__kettle_state[1]) {
            $__kettle_state[1]
        } else {
            1
        }

        # `$?` is read-only. Ignore restores failure without polluting `$Error`.
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
        # OSC 133: D is the prior exit; A starts this prompt.
        [Console]::Write("$esc]133;D;$__kettle_code$bel$esc]133;A$bel")
        if ($global:__kettle_completion_enabled -and
            $global:__kettle_completion_session -lt $global:__kettle_completion_counter_max) {
            $global:__kettle_completion_session =
                [uint64]([uint64]$global:__kettle_completion_session + [uint64]1)
            $global:__kettle_completion_request = [uint64]0
            $__kettle_keys = __kettle_completion_key_mask
            [Console]::Write(
                "$esc]777;kettle-completion;4;sync;$global:__kettle_completion_session;$__kettle_keys$bel"
            )
        } else {
            $global:__kettle_completion_enabled = $false
            __kettle_completion_reset_cycle
        }
        # Report only filesystem locations for new-tab/split cwd inheritance.
        # Windows paths use file://HOST/C:/... with encoded segments.
        $loc = $ExecutionContext.SessionState.Path.CurrentLocation
        if ($loc.Provider.Name -eq 'FileSystem') {
            $segs = $loc.ProviderPath -replace '\\', '/' -split '/'
            $enc = ($segs | ForEach-Object { [uri]::EscapeDataString($_) }) -join '/'
            # Drive paths need a leading slash; UNC paths already have one.
            if (-not $enc.StartsWith('/')) { $enc = "/$enc" }
            [Console]::Write("$esc]7;file://$env:COMPUTERNAME$enc$bel")
        }
        # Do not let the prompt renderer replace the user's native exit code.
        $global:LASTEXITCODE = $__kettle_state[1]
        # Return B with the prompt so it lands where input begins.
        return "$rendered$esc]133;B$bel"
    }

    function global:__kettle_completion_field([string]$Value, [int]$Limit) {
        if ($null -eq $Value) { return '' }
        try {
            # Bound UTF-8 without cutting a surrogate pair or grapheme.
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

    function global:__kettle_completion_begin_generation {
        if (-not $global:__kettle_completion_enabled -or
            $global:__kettle_completion_generation -ge $global:__kettle_completion_counter_max) {
            $global:__kettle_completion_enabled = $false
            __kettle_completion_reset_cycle
            return $false
        }
        $global:__kettle_completion_generation =
            [uint64]([uint64]$global:__kettle_completion_generation + [uint64]1)
        return $true
    }

    function global:__kettle_completion_hide {
        if (-not (__kettle_completion_begin_generation)) { return }
        [Console]::Write(
            [char]27 + ']777;kettle-completion;4;clear;' +
            $global:__kettle_completion_session + ';' +
            $global:__kettle_completion_generation + ';' +
            $global:__kettle_completion_request + [char]7
        )
    }

    function global:__kettle_completion_clear {
        __kettle_completion_reset_cycle
        __kettle_completion_hide
    }

    function global:__kettle_completion_emit($Selected, $Operation = 'show') {
        $candidates = $global:__kettle_completion_matches
        if ($null -eq $candidates -or $candidates.Count -eq 0) {
            __kettle_completion_clear
            return
        }
        if (-not (__kettle_completion_begin_generation)) { return }
        $count = $candidates.Count
        if ($count -eq 0) {
            __kettle_completion_clear
        } else {
            $offset = 0
            if ($null -ne $Selected) {
                $offset = [Math]::Floor([int]$Selected / 64) * 64
            }
            # Build against the parser's actual wire cap. Per-field limits make
            # 64 rows fit today, but keeping the aggregate check here prevents a
            # later label-width change from silently dropping the whole reply.
            $token = __kettle_completion_field $global:__kettle_completion_token 128
            $prefix = __kettle_completion_field $global:__kettle_completion_prefix 1024
            $pageAttempts = 0
            while ($pageAttempts -lt 2) {
                $pageAttempts++
                $last = [Math]::Min($offset + 63, $count - 1)
                $page = [Collections.Generic.List[string]]::new()
                $bodyLength = 0
                for ($index = $offset; $index -le $last; $index++) {
                    $row = $global:__kettle_completion_rows[$index]
                    if (256 + $token.Length + $prefix.Length +
                        $bodyLength + $row.Length -gt 65000) { break }
                    $page.Add($row)
                    $bodyLength += $row.Length
                }
                if ($null -eq $Selected -or
                    [int]$Selected -lt $offset + $page.Count) {
                    break
                }
                # Keep the selected row visible if a future wider field makes
                # the original 64-row page exceed the wire budget.
                $offset = [int]$Selected
            }
            if ($page.Count -eq 0) {
                __kettle_completion_clear
                return
            }
            if ($null -ne $Selected -and
                [int]$Selected -ge $offset + $page.Count) {
                __kettle_completion_clear
                return
            }
            $body = $page -join ''
            $selectedField = ''
            if ($null -ne $Selected -and
                [int]$Selected -ge 0 -and
                [int]$Selected -lt $count) {
                $selectedField = [string]([int]$Selected - $offset)
            }
            $payload = '777;kettle-completion;4;' + $Operation + ';' +
                $global:__kettle_completion_session + ';' +
                $global:__kettle_completion_generation +
                ';' + $global:__kettle_completion_request +
                ';completion;' + $selectedField + ';powershell;' +
                $token + ';' + $prefix + ';' + $offset + ';' + $count + $body
            [Console]::Write([char]27 + ']' + $payload + [char]7)
        }
    }

    function global:__kettle_completion_capture_result($Result) {
        $global:__kettle_completion_result = $null
        $global:__kettle_completion_matches = @()
        $global:__kettle_completion_rows = @()
        $global:__kettle_completion_token = $null
        if ($null -eq $Result) { return $true }
        # Keep the indexed collection: `@(...)` would copy every provider
        # result before the 2048-row retained-memory limit can apply.
        $candidates = $Result.CompletionMatches
        $retained = [Collections.Generic.List[object]]::new()
        $rows = [Collections.Generic.List[string]]::new()
        $sourceBytes = 0
        $last = [Math]::Min($candidates.Count, 2048)
        for ($index = 0; $index -lt $last; $index++) {
            $candidate = $candidates[$index]
            $text = [string]$candidate.CompletionText
            $tooltip = [string]$candidate.ToolTip
            # Reject huge provider objects before scanning their UTF-8 bytes.
            if ($text.Length -gt 4096 -or $tooltip.Length -gt 16384) { break }
            $textBytes = [Text.Encoding]::UTF8.GetByteCount($text)
            $tooltipBytes = [Text.Encoding]::UTF8.GetByteCount($tooltip)
            $rowBytes = $textBytes + $tooltipBytes
            if ($textBytes -gt 4096 -or $tooltipBytes -gt 16384 -or
                $rowBytes -gt 16384 -or $sourceBytes + $rowBytes -gt 262144) {
                break
            }
            $label = __kettle_completion_field $text 64
            if ([string]::IsNullOrEmpty($label)) {
                break
            }
            $description = __kettle_completion_field $tooltip 256
            $row = ";$label;$description"
            $retained.Add([pscustomobject]@{
                CompletionText = $text
                ToolTip = $tooltip
                ResultType = $candidate.ResultType
            })
            $rows.Add($row)
            $sourceBytes += $rowBytes
        }
        $global:__kettle_completion_result = [pscustomobject]@{
            ReplacementIndex = [int]$Result.ReplacementIndex
            ReplacementLength = [int]$Result.ReplacementLength
        }
        $global:__kettle_completion_matches = $retained.ToArray()
        $global:__kettle_completion_rows = $rows.ToArray()
        return $true
    }

    function global:__kettle_completion_reset_cycle {
        $global:__kettle_completion_result = $null
        $global:__kettle_completion_matches = @()
        $global:__kettle_completion_rows = @()
        $global:__kettle_completion_token = $null
        $global:__kettle_completion_prefix = $null
        $global:__kettle_completion_index = -1
        $global:__kettle_completion_replacement_index = 0
        $global:__kettle_completion_replacement_length = 0
        $global:__kettle_completion_last_line = $null
        $global:__kettle_completion_last_cursor = -1
    }

    function global:__kettle_completion_begin_request {
        if (-not $global:__kettle_completion_enabled -or
            [uint64]$global:__kettle_completion_request -ge $global:__kettle_completion_counter_max) {
            # Never reuse an id; the next prompt resets the session.
            __kettle_completion_reset_cycle
            return $false
        }
        $global:__kettle_completion_request =
            [uint64]([uint64]$global:__kettle_completion_request + [uint64]1)
        return $true
    }

    function global:__kettle_completion_key_mask {
        try {
            $handlers = Get-PSReadLineKeyHandler -Bound
            $tab = $handlers | Where-Object { $_.Key -eq 'Tab' } | Select-Object -First 1
            $backtab = $handlers | Where-Object { $_.Key -eq 'Shift+Tab' } | Select-Object -First 1
            $mask = 0
            if ($null -ne $tab -and $tab.Function -eq 'KettleCompleteNext') { $mask += 1 }
            if ($null -ne $backtab -and $backtab.Function -eq 'KettleCompletePrevious') { $mask += 2 }
            return $mask
        } catch {
            return 0
        }
    }

    # Narrow editor boundary used by production and the portable fixture.
    function global:__kettle_completion_editor_state {
        $line = $null
        $cursor = 0
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
        return [pscustomobject]@{ Line = $line; Cursor = $cursor }
    }

    function global:__kettle_completion_capture_prefix(
        [string]$Line,
        [int]$Cursor
    ) {
        if ($null -eq $Line -or $Cursor -lt 0 -or $Cursor -gt $Line.Length) {
            return ''
        }
        $prefix = $Line.Substring(0, $Cursor)
        $lastBreak = [Math]::Max($prefix.LastIndexOf("`n"), $prefix.LastIndexOf("`r"))
        if ($lastBreak -ge 0) {
            $prefix = $prefix.Substring($lastBreak + 1)
        }
        # `__kettle_completion_field` performs grapheme-safe encoding. Reject
        # an implausibly large editor line before it allocates one index per
        # text element merely to produce the protocol's 1 KiB prefix hint.
        if ($prefix.Length -gt 16384) { return '' }
        return $prefix
    }

    function global:__kettle_completion_capture_token(
        [string]$Line,
        [int]$Index,
        [int]$Length
    ) {
        if ($null -eq $Line -or $Index -lt 0 -or $Length -lt 0 -or
            $Index -gt $Line.Length -or $Length -gt $Line.Length - $Index -or
            $Length -gt 4096) {
            return ''
        }
        # Bound before the wire encoder indexes every grapheme.
        return $Line.Substring($Index, $Length)
    }

    function global:__kettle_completion_expand([string]$Line, [int]$Cursor) {
        return TabExpansion2 -inputScript $Line -cursorColumn $Cursor
    }

    function global:__kettle_completion_apply_replacement([int]$Index, [int]$Length, [string]$Text) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace($Index, $Length, $Text)
    }

    function global:__kettle_completion_set_cursor([int]$Position) {
        [Microsoft.PowerShell.PSConsoleReadLine]::SetCursorPosition($Position)
    }

    function global:__kettle_completion_replacement($Match) {
        $text = [string]$Match.CompletionText
        $cursorAdjustment = 0
        if ([string]$Match.ResultType -eq 'ProviderContainer') {
            $separator = [string][IO.Path]::DirectorySeparatorChar
            if (-not $text.EndsWith($separator)) {
                if ($text.EndsWith($separator + "'") -or
                    $text.EndsWith($separator + '"')) {
                    $cursorAdjustment = -1
                } elseif ($text.EndsWith("'") -or $text.EndsWith('"')) {
                    $text = $text.Substring(0, $text.Length - 1) +
                        $separator + $text.Substring($text.Length - 1)
                    $cursorAdjustment = -1
                } else {
                    $text += $separator
                }
            }
        }
        return @($text, $cursorAdjustment)
    }

    function global:__kettle_completion_cycle([int]$Direction) {
        if (-not (__kettle_completion_begin_request)) { return $true }
        $editor = __kettle_completion_editor_state
        $line = [string]$editor.Line
        $cursor = [int]$editor.Cursor
        $continues = $null -ne $global:__kettle_completion_result -and
            $line -ceq $global:__kettle_completion_last_line -and
            $cursor -eq $global:__kettle_completion_last_cursor
        if (-not $continues) {
            $global:__kettle_completion_prefix =
                __kettle_completion_capture_prefix $line $cursor
            try {
                $expanded = __kettle_completion_expand $line $cursor
                __kettle_completion_capture_result $expanded | Out-Null
            } catch {
                # Let the handler clear the card without invoking PSReadLine's
                # inline completion UI.
                throw
            }
            $global:__kettle_completion_index = -1
            if ($null -ne $global:__kettle_completion_result) {
                $global:__kettle_completion_replacement_index =
                    [int]$global:__kettle_completion_result.ReplacementIndex
                $global:__kettle_completion_replacement_length =
                    [int]$global:__kettle_completion_result.ReplacementLength
                $tokenIndex = $global:__kettle_completion_replacement_index
                $tokenLength = $global:__kettle_completion_replacement_length
                $global:__kettle_completion_token =
                    __kettle_completion_capture_token $line $tokenIndex $tokenLength
            }
        }

        $result = $global:__kettle_completion_result
        $candidates = $global:__kettle_completion_matches
        if ($null -eq $result -or $candidates.Count -eq 0) {
            __kettle_completion_clear
            return $true
        }
        $operation = if ($continues) { 'update' } else { 'show' }
        if ($global:__kettle_completion_index -lt 0 -and $Direction -lt 0) {
            $global:__kettle_completion_index = $candidates.Count - 1
        } else {
            $global:__kettle_completion_index =
                ($global:__kettle_completion_index + $Direction +
                    $candidates.Count) % $candidates.Count
        }
        $replacement = __kettle_completion_replacement `
            $candidates[$global:__kettle_completion_index]
        __kettle_completion_apply_replacement `
            $global:__kettle_completion_replacement_index `
            $global:__kettle_completion_replacement_length `
            ([string]$replacement[0])
        $global:__kettle_completion_replacement_length =
            ([string]$replacement[0]).Length
        if ([int]$replacement[1] -ne 0) {
            __kettle_completion_set_cursor (
                $global:__kettle_completion_replacement_index +
                $global:__kettle_completion_replacement_length +
                [int]$replacement[1]
            )
        }

        $after = __kettle_completion_editor_state
        $global:__kettle_completion_last_line = [string]$after.Line
        $global:__kettle_completion_last_cursor = [int]$after.Cursor
        __kettle_completion_emit $global:__kettle_completion_index $operation

        # PSReadLine deliberately re-queries a single directory match on the
        # next Tab so completion can continue inside it. Retain a cycle only
        # when there is actually another candidate to visit.
        if ($candidates.Count -eq 1) {
            $global:__kettle_completion_result = $null
        }
        return $true
    }

    function global:__kettle_completion_cycle_next {
        __kettle_completion_cycle 1
    }

    function global:__kettle_completion_cycle_previous {
        __kettle_completion_cycle -1
    }

    function global:__kettle_completion_handle_next {
        try {
            __kettle_completion_cycle_next | Out-Null
        } catch {
            # Never fall back to PSReadLine's inline completion UI.
            __kettle_completion_clear
        }
    }

    function global:__kettle_completion_handle_previous {
        try {
            __kettle_completion_cycle_previous | Out-Null
        } catch {
            __kettle_completion_clear
        }
    }

    __kettle_completion_reset_cycle

    # Emit C from PSReadLine's stock AcceptLine binding. Custom handlers cannot
    # be recovered as ScriptBlocks, so leave them untouched.
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
            $backtabHandler = Get-PSReadLineKeyHandler -Bound |
                Where-Object { $_.Key -eq 'Shift+Tab' } |
                Select-Object -First 1
            if ($env:KETTLE_COMPLETION_OVERLAY -eq '1' -and
                [string]::IsNullOrEmpty($env:TMUX) -and
                [string]::IsNullOrEmpty($env:STY) -and
                $null -ne $tabHandler -and
                $tabHandler.Function -eq 'TabCompleteNext') {
                Set-PSReadLineKeyHandler -Key Tab -BriefDescription KettleCompleteNext -Description 'Kettle detached completion' -ScriptBlock {
                    __kettle_completion_handle_next
                }
                if ($null -ne $backtabHandler -and
                    $backtabHandler.Function -eq 'TabCompletePrevious') {
                    Set-PSReadLineKeyHandler -Chord Shift+Tab -BriefDescription KettleCompletePrevious -Description 'Kettle detached completion' -ScriptBlock {
                        __kettle_completion_handle_previous
                    }
                }
            }
        }
    }

    $global:__kettle_prompt_installed = $true
}
