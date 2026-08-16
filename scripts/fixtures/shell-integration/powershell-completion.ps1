param([Parameter(Mandatory = $true)][string]$IntegrationPath)

$ErrorActionPreference = 'Stop'
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue
Import-Module PSReadLine -ErrorAction Stop
$env:KETTLE_COMPLETION_OVERLAY = '0'
Set-PSReadLineKeyHandler -Key Tab -Function TabCompleteNext
. $IntegrationPath

$tabHandler = Get-PSReadLineKeyHandler -Bound |
    Where-Object { $_.Key -eq 'Tab' } |
    Select-Object -First 1
if ($null -eq $tabHandler -or $tabHandler.Function -ne 'TabCompleteNext') {
    throw 'completion-overlay = off replaced the stock Tab handler'
}

$sample = 'abc' + [char]::ConvertFromUtf32(0x1FAD6) + 'def'
$actual = __kettle_completion_field $sample 7
$expected = 'abc%F0%9F%AB%96'
if ($actual -ne $expected) {
    throw "completion field cut a Unicode boundary: expected $expected, got $actual"
}

Write-Output 'KETTLE_POWERSHELL_COMPLETION_OK'
