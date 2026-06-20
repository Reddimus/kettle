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
        # Render the user's original prompt (or PowerShell's built-in default
        # if none was set). Guarded: a prompt that THROWS would otherwise make
        # PowerShell re-invoke it endlessly (no prompt, no input), so on any
        # failure fall back to the built-in default rather than loop.
        $default = "PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) "
        $rendered = if ($null -ne $global:__kettle_original_prompt) {
            try { & $global:__kettle_original_prompt } catch { $default }
        } else {
            $default
        }
        # B = end of prompt / input start. Emitted after the rendered
        # prompt text so the marker lands right where the user starts
        # typing.
        [Console]::Write("$esc]133;B$bel")
        return $rendered
    }

    # C = command started executing. PSReadLine (the default in
    # PowerShell 5.1+ since Windows 10 1809; bundled with PS 7) fires
    # AcceptLine when the user hits Enter — hook it to emit OSC 133;C
    # right before the command runs. Silently skipped if PSReadLine
    # isn't loaded (rare; the user would have disabled it on purpose).
    if (Get-Module -ListAvailable PSReadLine) {
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            [Console]::Write([char]27 + ']133;C' + [char]7)
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
        }
    }

    $global:__kettle_prompt_installed = $true
}
