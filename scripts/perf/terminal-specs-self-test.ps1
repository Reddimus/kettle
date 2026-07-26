# GUI-free regression tests for terminal discovery contracts.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\terminal-specs.ps1"

function Assert-KettlePerfTerminalSpec {
    param(
        [Parameter(Mandatory = $true)]
        [bool]$Condition,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

$root = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-terminal-specs-' + [Guid]::NewGuid().ToString('N')
)
$programFilesRoot = Join-Path $root 'Program Files'
$localAppDataRoot = Join-Path $root 'LocalAppData'
$rio = Join-Path $programFilesRoot 'Rio\rio.exe'

$savedProgramFiles = $env:ProgramFiles
$savedLocalAppData = $env:LOCALAPPDATA
$savedPath = $env:Path
$savedExplicitRio = $env:KETTLE_PERF_RIO_EXE
try {
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $rio) -Force)
    [IO.File]::WriteAllBytes($rio, [byte[]]@(0x4d, 0x5a))

    $candidates = Get-KettlePerfRioCandidates `
        -ProgramFilesRoot $programFilesRoot `
        -LocalAppDataRoot $localAppDataRoot
    Assert-KettlePerfTerminalSpec `
        -Condition ($candidates.Count -eq 6) `
        -Message 'Rio discovery candidate count drifted'
    Assert-KettlePerfTerminalSpec `
        -Condition ([StringComparer]::OrdinalIgnoreCase.Equals(
            $candidates[1],
            $rio
        )) `
        -Message 'Rio Winget install location is absent from discovery'

    $env:ProgramFiles = $programFilesRoot
    $env:LOCALAPPDATA = $localAppDataRoot
    $env:Path = ''
    $env:KETTLE_PERF_RIO_EXE = $null
    $spec = Resolve-KettlePerfTerminal -Name rio
    Assert-KettlePerfTerminalSpec `
        -Condition (
            $spec.Available -and
            [StringComparer]::OrdinalIgnoreCase.Equals($spec.Exe, $rio) -and
            $spec.BenchmarkExeSha256 -match '^[0-9A-F]{64}$'
        ) `
        -Message 'Rio Winget install layout was not resolved and hashed'
} finally {
    $env:ProgramFiles = $savedProgramFiles
    $env:LOCALAPPDATA = $savedLocalAppData
    $env:Path = $savedPath
    $env:KETTLE_PERF_RIO_EXE = $savedExplicitRio
    if (
        (Test-Path -LiteralPath $root) -and
        [IO.Path]::GetFullPath($root).StartsWith(
            [IO.Path]::GetFullPath([IO.Path]::GetTempPath()),
            [StringComparison]::OrdinalIgnoreCase
        )
    ) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
}

Write-Output 'terminal-specs self-test: PASS'
