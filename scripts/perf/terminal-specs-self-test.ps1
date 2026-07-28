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
$appxRoot = Join-Path $root 'WindowsApps\Microsoft.WindowsTerminal'
$windowsTerminalHost = Join-Path $appxRoot 'WindowsTerminal.exe'
$shadowRoot = Join-Path $root 'shadow'
$shadowWindowsTerminal = Join-Path $shadowRoot 'wt.exe'

$savedProgramFiles = $env:ProgramFiles
$savedLocalAppData = $env:LOCALAPPDATA
$savedPath = $env:Path
$savedExplicitRio = $env:KETTLE_PERF_RIO_EXE
$savedExplicitWindowsTerminal = $env:KETTLE_PERF_WT_EXE
$shadowLease = $null
try {
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $rio) -Force)
    [IO.File]::WriteAllBytes($rio, [byte[]]@(0x4d, 0x5a))
    [void](New-Item -ItemType Directory -Path $appxRoot -Force)
    [void](New-Item -ItemType Directory -Path $shadowRoot -Force)
    [IO.File]::WriteAllBytes(
        $windowsTerminalHost,
        [byte[]]@(0x4d, 0x5a, 0x01)
    )
    [IO.File]::WriteAllBytes(
        $shadowWindowsTerminal,
        [byte[]]@(0x4d, 0x5a, 0x02)
    )

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
    $versionedSpec = Resolve-KettlePerfTerminal `
        -Name rio -VersionOverride '0.4.12'
    Assert-KettlePerfTerminalSpec `
        -Condition (
            $versionedSpec.VersionOverride -ceq '0.4.12' -and
            (Get-KettlePerfVersion $versionedSpec) -ceq '0.4.12'
        ) `
        -Message 'A campaign-pinned terminal version was not preserved'

    function Get-AppxPackage {
        [CmdletBinding()]
        param([string]$Name)

        if ($Name -cne 'Microsoft.WindowsTerminal') {
            return
        }
        return [pscustomobject]@{
            InstallLocation = $appxRoot
            Version = '1.24.11911.0'
        }
    }

    $env:Path = $shadowRoot
    $env:KETTLE_PERF_WT_EXE = $shadowWindowsTerminal
    $advisorySpec = Resolve-KettlePerfTerminal `
        -Name wt -VersionOverride '1.24.11911.0'
    Assert-KettlePerfTerminalSpec `
        -Condition (
            [StringComparer]::OrdinalIgnoreCase.Equals(
                $advisorySpec.Exe,
                $shadowWindowsTerminal
            ) -and
            [StringComparer]::OrdinalIgnoreCase.Equals(
                $advisorySpec.BenchmarkExe,
                $windowsTerminalHost
            ) -and
            $advisorySpec.WindowsTerminalLaunchMode -ceq
                'app-execution-alias-advisory'
        ) `
        -Message 'Windows Terminal smoke discovery did not remain advisory'

    # Deny even shared reads of the PATH/environment shadow while resolving
    # the release-bound spec. The direct Appx-host contract must not consult,
    # hash, version-probe, or execute that mutable launcher.
    $shadowLease = [IO.File]::Open(
        $shadowWindowsTerminal,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::None
    )
    $releaseSpec = Resolve-KettlePerfTerminal `
        -Name wt -WindowsTerminalExe $windowsTerminalHost `
        -VersionOverride '1.24.11911.0'
    Assert-KettlePerfTerminalSpec `
        -Condition (
            [StringComparer]::OrdinalIgnoreCase.Equals(
                $releaseSpec.Exe,
                $windowsTerminalHost
            ) -and
            [StringComparer]::OrdinalIgnoreCase.Equals(
                $releaseSpec.BenchmarkExe,
                $windowsTerminalHost
            ) -and
            $releaseSpec.WindowsTerminalLaunchMode -ceq
                'installed-appx-direct-host' -and
            (Get-KettlePerfVersion $releaseSpec) -ceq '1.24.11911.0'
        ) `
        -Message 'Release acquisition selected or invoked the shadow wt.exe'
} finally {
    if ($null -ne $shadowLease) {
        $shadowLease.Dispose()
    }
    $env:ProgramFiles = $savedProgramFiles
    $env:LOCALAPPDATA = $savedLocalAppData
    $env:Path = $savedPath
    $env:KETTLE_PERF_RIO_EXE = $savedExplicitRio
    $env:KETTLE_PERF_WT_EXE = $savedExplicitWindowsTerminal
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
