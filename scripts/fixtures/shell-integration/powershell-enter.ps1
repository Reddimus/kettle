param(
    [Parameter(Mandatory = $true)]
    [string]$IntegrationPath
)

$ErrorActionPreference = 'Stop'
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue

Import-Module PSReadLine -ErrorAction Stop
Set-PSReadLineKeyHandler -Key Enter -Function ValidateAndAcceptLine
. $IntegrationPath

$enterHandler = Get-PSReadLineKeyHandler -Bound |
    Where-Object { $_.Key -eq 'Enter' } |
    Select-Object -First 1
if ($null -eq $enterHandler -or $enterHandler.Function -ne 'ValidateAndAcceptLine') {
    throw 'kettle replaced the existing Enter key handler'
}

Write-Output 'KETTLE_POWERSHELL_ENTER_OK'
