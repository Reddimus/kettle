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

# Parallels Desktop 26.4.1's Windows ARM WDDM driver faults inside a headless
# wgpu device request. Select WARP only for that exact guest: physical Windows
# ARM machines and hosted Windows x64 runners keep hardware-first coverage.
$commonArgs = @()
$manufacturer = ''
try {
    $manufacturer = (Get-ItemProperty `
        -LiteralPath 'HKLM:\HARDWARE\DESCRIPTION\System\BIOS' `
        -Name SystemManufacturer).SystemManufacturer
} catch {
    # An unreadable BIOS key is not evidence that this is the affected VM.
}
$isParallelsArm =
    [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq `
        [Runtime.InteropServices.Architecture]::Arm64 -and
    $manufacturer -like 'Parallels*'
if ($isParallelsArm) {
    $configPath = Join-Path $outputRoot 'parallels-arm-software.toml'
    [IO.File]::WriteAllText(
        $configPath,
        "gpu-force-software = true`n",
        [Text.UTF8Encoding]::new($false)
    )
    $commonArgs = @('--config', $configPath)
    Write-Output 'Parallels Windows ARM detected; GPU smoke is using DX12/WARP.'
}

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

$gpuInfo = (& $binary @commonArgs --gpu-info | Out-String)
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
$menuArgs = @($commonArgs + @('--screenshot-menu', $menuPng))
& $binary @menuArgs
if ($LASTEXITCODE -ne 0) {
    throw "--screenshot-menu exited $LASTEXITCODE"
}
Assert-Png -Path $menuPng -MinimumSize 40000 -Label 'screenshot-menu'

$shotPng = Join-Path $outputRoot 'kettle-ci.png'
$shotArgs = @($commonArgs + @('--screenshot', $shotPng))
& $binary @shotArgs
if ($LASTEXITCODE -ne 0) {
    throw "--screenshot exited $LASTEXITCODE"
}
Assert-Png -Path $shotPng -MinimumSize 10000 -Label 'screenshot'
Write-Output 'gpu-render-smoke PASSED (gpu-info + screenshot-menu + screenshot)'
