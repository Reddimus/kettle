param(
    [Parameter(Mandatory = $true)][string]$IntegrationPath,
    [ValidateSet('Arm', 'Enter', 'Vi')][string]$ReplaceBinding = 'Arm'
)
$ErrorActionPreference = 'Stop'
Import-Module PSReadLine
Set-PSReadLineOption -EditMode Emacs
Set-PSReadLineKeyHandler -Key Enter -Function AcceptLine
Remove-PSReadLineKeyHandler -Chord Ctrl+x -ErrorAction SilentlyContinue
. $IntegrationPath
if (-not $global:__kettle_correlation_enabled) { throw 'correlation was not installed' }
$token = '0123456789abcdef0123456789abcdef'
__kettle_correlation_begin
foreach ($digit in ($token + 'a').ToCharArray()) { __kettle_correlation_nibble $digit }
__kettle_correlation_arm
if ($global:__kettle_command_token) { throw 'an overlong token was accepted' }
__kettle_correlation_begin
foreach ($digit in $token.ToCharArray()) { __kettle_correlation_nibble $digit }
__kettle_correlation_arm
if ($global:__kettle_command_token -cne $token) { throw 'the exact token was lost' }
__kettle_command_begin
if ($global:__kettle_command_token) { throw 'the pending token was reused' }
if ($global:__kettle_active_token -cne $token) { throw 'the active token was lost' }
$global:LASTEXITCODE = 7
Write-Error 'fixture failure' -ErrorAction Ignore
$null = prompt
if ($global:__kettle_active_token) { throw 'the finished token was reused' }
if ($global:LASTEXITCODE -ne 7) { throw 'native status was changed' }
switch ($ReplaceBinding) {
    'Arm' { Set-PSReadLineKeyHandler -Chord 'Ctrl+x,h' -Function BackwardChar }
    'Enter' { Set-PSReadLineKeyHandler -Key Enter -Function AcceptLine }
    'Vi' { Set-PSReadLineOption -EditMode Vi }
}
$null = prompt
if ($global:__kettle_correlation_enabled) { throw 'replaced handler still advertised correlation' }
Write-Output 'KETTLE_POWERSHELL_CORRELATION_OK'
