#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$binary = Join-Path $repo 'target\release\kettle.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "missing release binary: $binary"
}
if ($env:RUNNER_TEMP) {
    $scratchRoot = $env:RUNNER_TEMP
} else {
    $scratchRoot = [IO.Path]::GetTempPath()
}
$outputRoot = Join-Path $scratchRoot 'kettle-gpu-render-smoke'
[void][IO.Directory]::CreateDirectory($outputRoot)

function Assert-Png {
    param([string] $Path, [long] $MinimumSize, [string] $Label)
    $bytes = [IO.File]::ReadAllBytes($Path)
    if (
        $bytes.Length -le $MinimumSize -or
        ($bytes[0..3] -join ',') -ne (@(0x89, 0x50, 0x4E, 0x47) -join ',')
    ) {
        throw "$Label is not a valid, nontrivial PNG ($($bytes.Length) bytes)"
    }
    Write-Output "$Label OK ($($bytes.Length) bytes)"
}

$gpuInfo = (& $binary --gpu-info | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw "--gpu-info exited $LASTEXITCODE"
}
Write-Output $gpuInfo
foreach ($pattern in @(
    '(?m)^Backend:\s+\S',
    '(?m)^Adapter:\s+\S',
    '(?m)^Max texture:\s+\d+ px / side\s*$'
)) {
    if ($gpuInfo -notmatch $pattern) {
        throw "--gpu-info output did not match $pattern"
    }
}

$menuPng = Join-Path $outputRoot 'kettle-menu.png'
$process = Start-Process -FilePath $binary `
    -ArgumentList '--screenshot-menu', $menuPng `
    -Wait -PassThru -NoNewWindow
if ($process.ExitCode -ne 0) {
    throw "--screenshot-menu exited $($process.ExitCode)"
}
Assert-Png -Path $menuPng -MinimumSize 40000 -Label 'screenshot-menu'

$shotPng = Join-Path $outputRoot 'kettle-ci.png'
$process = Start-Process -FilePath $binary `
    -ArgumentList '--screenshot', $shotPng `
    -Wait -PassThru -NoNewWindow
if ($process.ExitCode -ne 0) {
    throw "--screenshot exited $($process.ExitCode)"
}
Assert-Png -Path $shotPng -MinimumSize 10000 -Label 'screenshot'
Write-Output 'gpu-render-smoke PASSED (gpu-info + screenshot-menu + screenshot)'
