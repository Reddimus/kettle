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

Write-Output 'KETTLE_POWERSHELL_PROMPT_OK'
