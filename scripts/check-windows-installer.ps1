#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $repo

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$prefix = Join-Path $tempRoot "kettle-windows-install-smoke"
$integrationRoot = Join-Path $tempRoot "kettle-windows-default-install-smoke"
$startMenuDir = Join-Path $integrationRoot 'Start Menu\Programs'
$shortcutPath = Join-Path $startMenuDir 'kettle.lnk'
$profilePath = Join-Path $integrationRoot 'WindowsPowerShell\profile.ps1'
$testUninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle-installer-smoke-$PID"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $prefix
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $integrationRoot
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $testUninstallKey

function Assert-PathExists {
    param([string] $Path, [string] $Label)
    if (-not (Test-Path $Path)) {
        throw "missing ${Label}: $Path"
    }
}

function Assert-PathAbsent {
    param([string] $Path, [string] $Label)
    if (Test-Path $Path) {
        throw "${Label} survived uninstall: $Path"
    }
}

function Assert-Equal {
    param([string] $Actual, [string] $Expected, [string] $Label)
    if ($Actual -ne $Expected) {
        throw "${Label} mismatch: expected '$Expected', got '$Actual'"
    }
}

# A portable install owns only its prefix. Seed isolated default-install state
# and verify the portable uninstaller cannot remove it.
New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
$ws = New-Object -ComObject WScript.Shell
$sentinel = $ws.CreateShortcut($shortcutPath)
$sentinel.TargetPath = (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')
$sentinel.Arguments = '-NoProfile -File "default-install-sentinel.ps1"'
$sentinel.WorkingDirectory = $tempRoot
$sentinel.Save()
New-Item -Path $testUninstallKey -Force | Out-Null
Set-ItemProperty -Path $testUninstallKey -Name 'Sentinel' -Value 'default-install'
$profileSentinel = @'
# >>> kettle shell-integration (managed by install.ps1)
# default-install sentinel
# <<< kettle shell-integration (managed by install.ps1)
'@
New-Item -ItemType Directory -Force -Path (Split-Path $profilePath -Parent) | Out-Null
Set-Content -Path $profilePath -Value $profileSentinel -NoNewline
$userPathBefore = [Environment]::GetEnvironmentVariable('Path', 'User')

& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix -IntegrationTestRoot $integrationRoot |
    Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-install.out')

Assert-PathExists (Join-Path $prefix 'kettle.exe') 'kettle.exe'
Assert-PathExists (Join-Path $prefix 'kettle.com') 'kettle.com console launcher'
Assert-PathExists (Join-Path $prefix 'install.ps1') 'saved install.ps1'
Assert-PathExists (Join-Path $prefix '.kettle-install-prefix') 'prefix marker'
Assert-PathExists (Join-Path $prefix '.kettle-install.json') 'self-update ownership marker'
$marker = Get-Content -LiteralPath (Join-Path $prefix '.kettle-install.json') -Raw | ConvertFrom-Json
Assert-Equal $marker.channel 'local-dev' 'repo install marker channel'
Assert-PathExists (Join-Path $prefix 'kettle.ico') 'icon'
Assert-PathExists (Join-Path $prefix 'shell-integration\kettle.ps1') 'PowerShell shell integration'

$savedPrefix = Get-Content (Join-Path $prefix '.kettle-install-prefix') -Raw
if ($savedPrefix.Trim() -ne $prefix) {
    throw "prefix marker mismatch: expected $prefix, got $savedPrefix"
}

$versionFile = Join-Path $tempRoot 'kettle-windows-install-version.txt'
Remove-Item -Path $versionFile -ErrorAction SilentlyContinue
Start-Process -FilePath (Join-Path $prefix 'kettle.exe') `
    -ArgumentList '--version' `
    -NoNewWindow -Wait -RedirectStandardOutput $versionFile
$version = (Get-Content $versionFile -Raw).Trim()
if ($version -notmatch '^kettle \d+\.\d+\.\d+') {
    throw "unexpected installed kettle version output: $version"
}
Write-Output "windows-installer check: installed $version"

# A bare command must resolve to the console shim before kettle.exe so
# PowerShell waits and propagates CLI exit codes.
$processPathBefore = $env:Path
try {
    $env:Path = "$prefix;$processPathBefore"
    $resolvedKettle = (Get-Command kettle -CommandType Application | Select-Object -First 1).Source
    Assert-Equal $resolvedKettle (Join-Path $prefix 'kettle.com') 'bare kettle command resolution'
    $shimVersion = (& kettle --version | Out-String).Trim()
    Assert-Equal $LASTEXITCODE 0 'kettle.com version exit code'
    if ($shimVersion -notmatch '^kettle [0-9]+\.[0-9]+\.[0-9]+') {
        throw "unexpected kettle.com --version output: $shimVersion"
    }
} finally {
    $env:Path = $processPathBefore
}

# Run the saved helper without -Prefix; it should infer $prefix from the marker.
& (Join-Path $prefix 'install.ps1') -Uninstall -IntegrationTestRoot $integrationRoot |
    Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-uninstall.out')

Assert-PathAbsent (Join-Path $prefix 'kettle.exe') 'kettle.exe'
Assert-PathAbsent (Join-Path $prefix 'install.ps1') 'install helper'
Assert-PathAbsent (Join-Path $prefix '.kettle-install-prefix') 'prefix marker'
Assert-PathExists $shortcutPath 'default-install shortcut sentinel'
Assert-PathExists $testUninstallKey 'default-install registry sentinel'
$sentinelAfter = $ws.CreateShortcut($shortcutPath)
Assert-Equal $sentinelAfter.Arguments '-NoProfile -File "default-install-sentinel.ps1"' 'shortcut sentinel arguments'
$registrySentinel = (Get-ItemProperty -Path $testUninstallKey -Name 'Sentinel').Sentinel
Assert-Equal $registrySentinel 'default-install' 'registry sentinel'
Assert-Equal (Get-Content $profilePath -Raw) $profileSentinel 'PowerShell profile sentinel'
Assert-Equal ([Environment]::GetEnvironmentVariable('Path', 'User')) $userPathBefore 'user PATH'

Write-Output 'windows-installer check: custom-prefix install/uninstall OK'

# Exercise the real default-install integration path under isolated filesystem
# and registry roots. Seed the exact upgrade hazard seen in production: WScript
# opens an existing shortcut and retains stale PowerShell recorder arguments
# unless the installer replaces or explicitly clears it.
$integrationPrefix = Join-Path $integrationRoot 'Programs\kettle'
Remove-Item -Force -ErrorAction SilentlyContinue $shortcutPath
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $testUninstallKey
Remove-Item -Force -ErrorAction SilentlyContinue $profilePath

try {
    New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
    $ws = New-Object -ComObject WScript.Shell
    $stale = $ws.CreateShortcut($shortcutPath)
    $stale.TargetPath = (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')
    $stale.Arguments = '-NoProfile -WindowStyle Hidden -File "kettle-rec.ps1"'
    $stale.WorkingDirectory = $tempRoot
    $stale.Save()

    & (Join-Path $repo 'scripts\install.ps1') -IntegrationTestRoot $integrationRoot -WithShellIntegration |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-default-install.out')

    Assert-PathExists $shortcutPath 'Start menu shortcut'
    Assert-PathExists $testUninstallKey 'isolated Add/Remove Programs entry'
    Assert-PathExists (Join-Path $integrationPrefix '.kettle-install.json') 'default self-update ownership marker'
    $integrationMarker = Get-Content -LiteralPath (Join-Path $integrationPrefix '.kettle-install.json') -Raw | ConvertFrom-Json
    Assert-Equal $integrationMarker.channel 'local-dev' 'default repo install marker channel'
    $shortcut = $ws.CreateShortcut($shortcutPath)
    Assert-Equal $shortcut.TargetPath (Join-Path $integrationPrefix 'kettle.exe') 'shortcut target'
    Assert-Equal $shortcut.Arguments '' 'shortcut arguments'
    Assert-Equal $shortcut.WorkingDirectory $integrationPrefix 'shortcut working directory'
    # Exercise the updater-only metadata refresh path while the installed
    # executable remains in place. This must not attempt a self-copy.
    & (Join-Path $integrationPrefix 'install.ps1') -RefreshIntegration -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-refresh-integration.out')
    Assert-PathExists $shortcutPath 'refreshed Start menu shortcut'
    Assert-PathExists $testUninstallKey 'refreshed Add/Remove Programs entry'
    if ((Get-Content $profilePath -Raw) -notmatch 'kettle shell-integration \(managed by install\.ps1\)') {
        throw 'default install did not write isolated PowerShell profile integration'
    }

    & (Join-Path $integrationPrefix 'install.ps1') -Uninstall -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-default-uninstall.out')

    Assert-PathAbsent (Join-Path $integrationPrefix 'kettle.exe') 'default-install kettle.exe'
    Assert-PathAbsent $shortcutPath 'Start menu shortcut'
    Assert-PathAbsent $testUninstallKey 'isolated Add/Remove Programs entry'
    if ((Get-Content $profilePath -Raw) -match 'kettle shell-integration \(managed by install\.ps1\)') {
        throw 'default uninstall left isolated PowerShell profile integration behind'
    }
    Write-Output 'windows-installer check: stale-shortcut upgrade repair OK'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $integrationRoot
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $testUninstallKey
}

# Recreate the extracted release-zip layout separately from repo mode. Only
# this layout may opt into the stable self-update channel.
$zipRoot = Join-Path $tempRoot "kettle-windows-zip-fixture"
$zipPrefix = Join-Path $tempRoot "kettle-windows-zip-install"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $zipRoot, $zipPrefix
try {
    New-Item -ItemType Directory -Force -Path $zipRoot | Out-Null
    Copy-Item (Join-Path $repo 'target\release\kettle.exe') (Join-Path $zipRoot 'kettle.exe')
    Copy-Item (Join-Path $repo 'target\release\kettle-console.exe') (Join-Path $zipRoot 'kettle.com')
    Copy-Item (Join-Path $repo 'scripts\install.ps1') (Join-Path $zipRoot 'install.ps1')
    & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-zip-install.out')
    $zipMarker = Get-Content -LiteralPath (Join-Path $zipPrefix '.kettle-install.json') -Raw | ConvertFrom-Json
    Assert-Equal $zipMarker.channel 'stable' 'release zip install marker channel'
    & (Join-Path $zipPrefix 'install.ps1') -Uninstall -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-zip-uninstall.out')
    Assert-PathAbsent (Join-Path $zipPrefix 'kettle.exe') 'release zip kettle.exe'
    Write-Output 'windows-installer check: release zip stable channel OK'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $zipRoot, $zipPrefix
}
