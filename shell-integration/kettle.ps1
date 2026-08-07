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
        # Invoke the user's prompt before doing anything that changes `$?`.
        # Starship, oh-my-posh, and other prompts read it as their first
        # operation to distinguish a failed command from a successful one.
        try {
            $rendered = & $global:__kettle_original_prompt
        } catch {
            $rendered = $null
        }
        if ($null -eq $rendered) {
            $rendered = "PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) "
        }

        $code = $LASTEXITCODE
        if ($null -eq $code) { $code = 0 }
        $esc = [char]27
        $bel = [char]7
        # D = last command's exit code, A = this prompt's start.
        # Emitted together at the top of the prompt function.
        [Console]::Write("$esc]133;D;$code$bel$esc]133;A$bel")
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
        # B = end of prompt / input start. Emitted after the rendered
        # prompt text so the marker lands right where the user starts
        # typing. Returning it with the prompt is necessary: Console.Write
        # runs before PowerShell displays the function's returned text.
        return "$rendered$esc]133;B$bel"
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
                    [Console]::Write([char]27 + ']133;C' + [char]7)
                    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
                }
            }
        }
    }

    $global:__kettle_prompt_installed = $true
}
