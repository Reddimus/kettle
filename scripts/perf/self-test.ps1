# Runs every GUI-free performance-harness self-test in a fresh copy of the
# current PowerShell engine. CI invokes this once with PowerShell 7 and once
# with Windows PowerShell 5.1.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# A Windows PowerShell child inherits PSModulePath from its parent. When that
# parent is pwsh, the PowerShell 7 module directory can precede the Desktop
# edition's built-ins and make commands such as Get-FileHash undiscoverable.
# Rebuild the no-profile test environment from native machine roots and load
# Utility by its engine-owned manifest before spawning the isolated tests.
if ($PSVersionTable.PSEdition -eq 'Desktop') {
    $psHomeModules = Join-Path $PSHOME 'Modules'
    $machineModulePath = [Environment]::GetEnvironmentVariable(
        'PSModulePath',
        'Machine'
    )
    if ([string]::IsNullOrWhiteSpace($machineModulePath)) {
        throw 'Windows PowerShell machine PSModulePath is unavailable.'
    }

    $nativeModulePaths = @($psHomeModules)
    foreach ($path in @(
        $machineModulePath -split [regex]::Escape(
            [string][IO.Path]::PathSeparator
        )
    )) {
        $expanded = [Environment]::ExpandEnvironmentVariables($path).Trim()
        if (
            $expanded -and
            -not @(
                $nativeModulePaths |
                    Where-Object {
                        [StringComparer]::OrdinalIgnoreCase.Equals(
                            $_,
                            $expanded
                        )
                    }
            ).Count
        ) {
            $nativeModulePaths += $expanded
        }
    }
    $env:PSModulePath = $nativeModulePaths -join [IO.Path]::PathSeparator

    $utilityManifest = Join-Path $psHomeModules (
        'Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1'
    )
    if (-not [IO.File]::Exists($utilityManifest)) {
        throw "Windows PowerShell Utility manifest is missing: $utilityManifest"
    }
    Import-Module -Name $utilityManifest -Force -ErrorAction Stop
}

$requiredTests = [string[]]@(
    'statistics-self-test.ps1',
    'comparator-campaign-self-test.ps1',
    'setup-comparator-campaign-self-test.ps1',
    'display-identity-self-test.ps1',
    'display-stability-self-test.ps1',
    'documentation-contract-self-test.ps1',
    'terminal-specs-self-test.ps1',
    'evidence-snapshot-self-test.ps1',
    'harness-provenance-self-test.ps1',
    'isolated-configs-self-test.ps1',
    'json-io-self-test.ps1',
    'payload-contract-self-test.ps1',
    'process-capture-self-test.ps1',
    'startup-ready-self-test.ps1',
    'go-signal-self-test.ps1',
    'release-contract-self-test.ps1',
    'release-statistics-self-test.ps1',
    'baseline-statistics-self-test.ps1',
    'score-statistics-self-test.ps1',
    'throughput-channel-self-test.ps1',
    'vtebench-channel-self-test.ps1',
    'sanitize-results-self-test.ps1',
    'score-self-test.ps1',
    'release-score-self-test.ps1',
    # latency.ps1 was the one module with no self-test, and it shipped a defect
    # that made a latency run impossible to complete.
    'latency-self-test.ps1',
    # Runs first in spirit but last in the list: it is the only test whose
    # subject is the other files' bytes rather than their behaviour.
    'ascii-contract-self-test.ps1'
)

$shell = (Get-Process -Id $PID).Path
if (-not $shell -or -not [IO.File]::Exists($shell)) {
    throw 'Could not resolve the current PowerShell executable.'
}

$tests = @(
    Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*-self-test.ps1' -File |
        Sort-Object -Property Name
)
$testNames = @($tests | ForEach-Object { $_.Name })
$missing = @($requiredTests | Where-Object { $_ -notin $testNames })
if ($missing.Count -ne 0) {
    throw "Required performance self-tests are missing: $($missing -join ', ')"
}

$suiteTimer = [Diagnostics.Stopwatch]::StartNew()
foreach ($test in $tests) {
    Write-Output "==> $($test.Name)"
    $testTimer = [Diagnostics.Stopwatch]::StartNew()
    & $shell -NoLogo -NoProfile -NonInteractive -File $test.FullName
    $exitCode = $LASTEXITCODE
    $testTimer.Stop()
    if ($exitCode -ne 0) {
        throw "$($test.Name) failed with exit code $exitCode."
    }
    Write-Output (
        '<== {0} passed in {1:N1}s' -f $test.Name, $testTimer.Elapsed.TotalSeconds
    )
}
$suiteTimer.Stop()

Write-Output (
    'Performance harness self-tests passed: {0} tests in {1:N1}s ({2}).' -f
        $tests.Count,
        $suiteTimer.Elapsed.TotalSeconds,
        $PSVersionTable.PSVersion
)
