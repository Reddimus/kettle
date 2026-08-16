param([Parameter(Mandatory = $true)][string]$IntegrationPath)

$ErrorActionPreference = 'Stop'
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue
Import-Module PSReadLine -ErrorAction Stop
$env:KETTLE_COMPLETION_OVERLAY = '1'
$env:TMUX = ''
$env:STY = ''
Set-PSReadLineKeyHandler -Key Tab -Function TabCompleteNext
Set-PSReadLineKeyHandler -Chord Shift+Tab -Function TabCompletePrevious
. $IntegrationPath

$tabHandler = Get-PSReadLineKeyHandler -Bound |
    Where-Object { $_.Key -eq 'Tab' } |
    Select-Object -First 1
if ($null -eq $tabHandler -or $tabHandler.Function -eq 'TabCompleteNext') {
    throw 'completion-overlay = auto did not replace the stock Tab handler'
}
$backtabHandler = Get-PSReadLineKeyHandler -Bound |
    Where-Object { $_.Key -eq 'Shift+Tab' } |
    Select-Object -First 1
if ($null -eq $backtabHandler -or $backtabHandler.Function -eq 'TabCompletePrevious') {
    throw 'completion-overlay = auto did not replace the stock Shift+Tab handler'
}
if ($null -eq (Get-Command __kettle_completion_cycle_next -ErrorAction SilentlyContinue)) {
    throw 'completion cycle helper was not installed'
}

$container = [pscustomobject]@{
    CompletionText = "'folder'"
    ResultType = 'ProviderContainer'
}
$replacement = __kettle_completion_replacement $container
$separator = [string][IO.Path]::DirectorySeparatorChar
if ([string]$replacement[0] -ne "'folder$separator'" -or [int]$replacement[1] -ne -1) {
    throw 'provider-container completion did not preserve the closing quote'
}

Write-Output 'KETTLE_POWERSHELL_COMPLETION_ENABLED_OK'
