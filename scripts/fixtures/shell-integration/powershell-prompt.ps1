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
    $global:LASTEXITCODE = 0
    return 'USER-PROMPT'
}
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue
. $IntegrationPath

$global:LASTEXITCODE = 37
$capture = [System.IO.StringWriter]::new()
$stdout = [Console]::Out
[Console]::SetOut($capture)
try { $null = prompt } finally { [Console]::SetOut($stdout) }

$emitted = $capture.ToString()
if ($emitted -notmatch '\]133;D;37') {
    throw 'wrapper reported the wrong exit code; expected D;37'
}
# The prompt's own native calls must not leak into the next command's view of
# $LASTEXITCODE either.
if ($global:LASTEXITCODE -ne 37) {
    throw "wrapper left LASTEXITCODE = $($global:LASTEXITCODE), expected 37"
}

Write-Output 'KETTLE_POWERSHELL_PROMPT_OK'
