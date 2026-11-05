param(
    [Parameter(Mandatory = $true)]
    [string]$IntegrationPath
)

$ErrorActionPreference = 'Stop'
Remove-Item Env:COMPUTERNAME -ErrorAction SilentlyContinue
function global:prompt { 'USER-PROMPT' }
. $IntegrationPath
$stdout = [Console]::Out
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
try { $null = prompt } finally { [Console]::SetOut($stdout) }
$report = [regex]::Match($capture.ToString(), '\x1b\]7;file://([^/]*)(/[^\x07]*)\x07')
if (-not $report.Success -or $report.Groups[1].Value -ne [Environment]::MachineName) {
    throw 'OSC 7 must identify the machine when COMPUTERNAME is unset'
}
$expected = $ExecutionContext.SessionState.Path.CurrentLocation.ProviderPath -replace '\\', '/'
if ($expected -notmatch '^/') { $expected = '/' + $expected }
if ([uri]::UnescapeDataString($report.Groups[2].Value) -ne $expected) {
    throw 'OSC 7 did not preserve the filesystem path'
}
Write-Output 'KETTLE_POWERSHELL_CWD_OK'
