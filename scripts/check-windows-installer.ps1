#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $repo

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$prefix = Join-Path $tempRoot "kettle-windows-install-smoke"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $prefix

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

& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix | Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-install.out')

Assert-PathExists (Join-Path $prefix 'kettle.exe') 'kettle.exe'
Assert-PathExists (Join-Path $prefix 'install.ps1') 'saved install.ps1'
Assert-PathExists (Join-Path $prefix '.kettle-install-prefix') 'prefix marker'
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

# Run the saved helper without -Prefix; it should infer $prefix from the marker.
& (Join-Path $prefix 'install.ps1') -Uninstall | Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-uninstall.out')

Assert-PathAbsent (Join-Path $prefix 'kettle.exe') 'kettle.exe'
Assert-PathAbsent (Join-Path $prefix 'install.ps1') 'install helper'
Assert-PathAbsent (Join-Path $prefix '.kettle-install-prefix') 'prefix marker'

Write-Output 'windows-installer check: custom-prefix install/uninstall OK'
