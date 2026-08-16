param(
    [Parameter(Mandatory = $true)]
    [string]$IntegrationPath
)

$ErrorActionPreference = 'Stop'
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue

function global:prompt {
    $global:__fixture_prompt_status = $?
    return 'USER-PROMPT'
}

# Control: PowerShell passes the failed cmdlet's `$?` into an unwrapped prompt.
Get-Item -LiteralPath '__kettle_missing_control__' -ErrorAction SilentlyContinue
$control = prompt
if ($global:__fixture_prompt_status -ne $false -or $control -ne 'USER-PROMPT') {
    throw 'control prompt did not observe the failed command'
}

. $IntegrationPath

# Regression: Kettle's wrapper must pass the same failed `$?` to the user's
# prompt. Invoke it without capture so the harness can also assert byte order.
Get-Item -LiteralPath '__kettle_missing_wrapped__' -ErrorAction SilentlyContinue
prompt
if ($global:__fixture_prompt_status -ne $false) {
    throw 'wrapped prompt did not observe the failed command'
}

# Regression: the `D;<code>` payload must report the USER's last exit code,
# not whatever the rendered prompt's own work left behind.
#
# This case was missing, and its absence is why the defect shipped: the checks
# above assert `$?` and marker ordering but never read the payload, so a
# wrapper that always emitted `D;0` passed every one of them.
#
# Starship, oh-my-posh and posh-git all shell out while rendering, and every
# native call overwrites $LASTEXITCODE. This prompt reproduces that by setting
# it directly — precisely what such a call does — so the case needs no
# platform-specific binary and runs identically on Windows and Unix.
# Redefine the user prompt and re-install, in that order. `prompt` is the
# kettle wrapper by now, so defining the new one FIRST is what stops the
# re-source from capturing the wrapper as its own "original" and recursing.
function global:prompt {
    $global:__fixture_prompt_status = $?
    $global:__fixture_prompt_err0 =
        if ($Error.Count) { $Error[0].Exception.Message } else { '<none>' }
    $global:LASTEXITCODE = 0
    return 'USER-PROMPT'
}
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue
. $IntegrationPath

# Each case runs its trigger IMMEDIATELY before `prompt`, in this scope. `$?`
# does not survive `& $scriptblock` or a function call, so a helper that wraps
# the trigger would silently test nothing.
$stdout = [Console]::Out

# (a) a failure carrying a native exit code must report that code.
$Error.Clear()
Write-Error 'USERS-REAL-ERROR' -ErrorAction SilentlyContinue
$errorsBefore = $Error.Count
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
$global:LASTEXITCODE = 37
Get-Item -LiteralPath '__kettle_missing_payload__' -ErrorAction SilentlyContinue
try { $null = prompt } finally { [Console]::SetOut($stdout) }
if ($capture.ToString() -notmatch '\]133;D;37') {
    throw 'wrapper reported the wrong exit code; expected D;37'
}
# The prompt's own native calls must not leak into the next command's view of
# $LASTEXITCODE either.
if ($global:LASTEXITCODE -ne 37) {
    throw "wrapper left LASTEXITCODE = $($global:LASTEXITCODE), expected 37"
}
# Re-arming `$?` must not cost the user their error history. The trigger above
# is itself a failing cmdlet, so $Error[0] is legitimately ITS error — what must
# never appear is kettle's synthetic re-arm record, either to the prompt while
# it renders or in the list afterwards.
if ($global:__fixture_prompt_err0 -like '*kettle: propagating*') {
    throw "the prompt observed kettle's synthetic re-arm error at \$Error[0]"
}
foreach ($record in $Error) {
    if ($record.Exception.Message -like '*kettle: propagating*') {
        throw 're-arming $? left a synthetic record in $Error'
    }
}
# The user's earlier error must still be reachable, not pushed out.
if (-not ($Error | Where-Object { $_.Exception.Message -eq 'USERS-REAL-ERROR' })) {
    throw "re-arming \$? displaced the user's own error from \$Error"
}

# (b) `$LASTEXITCODE` is written only by NATIVE commands, so a stale nonzero
# value must not mark a SUCCEEDING cmdlet as failed.
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
$global:LASTEXITCODE = 37
Get-Date | Out-Null
try { $null = prompt } finally { [Console]::SetOut($stdout) }
if ($capture.ToString() -notmatch '\]133;D;0') {
    throw 'a successful command after an earlier native failure must report D;0'
}

# (c) ...and a failed CMDLET, which never touches $LASTEXITCODE, must report a
# failure rather than the stale zero.
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
$global:LASTEXITCODE = 0
Get-Item -LiteralPath '__kettle_missing_cmdlet__' -ErrorAction SilentlyContinue
try { $null = prompt } finally { [Console]::SetOut($stdout) }
if ($capture.ToString() -notmatch '\]133;D;1') {
    throw 'a failed cmdlet with no native exit code must report D;1, not D;0'
}

# A prompt boundary must advance the managed completion session and publish
# the exact v3 sync the terminal will use for subsequent Tab request IDs.
$sessionBefore = [uint64]$global:__kettle_completion_session
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
try { $null = prompt } finally { [Console]::SetOut($stdout) }
$expectedSession = [uint64]($sessionBefore + [uint64]1)
if ([uint64]$global:__kettle_completion_session -ne $expectedSession -or
    -not $capture.ToString().Contains(
        "]777;kettle-completion;3;sync;$expectedSession;0$([char]7)"
    )) {
    throw 'prompt did not advance and publish its v3 completion session'
}

# Session rollover must disable only the side channel, not overflow into an
# imprecise number or break the user's prompt.
$global:__kettle_completion_enabled = $true
$global:__kettle_completion_generation = [uint64]0
$global:__kettle_completion_session = $global:__kettle_completion_counter_max
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
try { $rendered = prompt } finally { [Console]::SetOut($stdout) }
if ($rendered -notmatch 'USER-PROMPT' -or
    $global:__kettle_completion_enabled -or
    [uint64]$global:__kettle_completion_session -ne
        $global:__kettle_completion_counter_max -or
    $capture.ToString() -match 'kettle-completion;3;sync') {
    throw 'session rollover did not fail the completion side channel closed'
}

Write-Output 'KETTLE_POWERSHELL_PROMPT_OK'
