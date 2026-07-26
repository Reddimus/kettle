# Runs every GUI-free performance-harness self-test in a fresh copy of the
# current PowerShell engine. CI invokes this once with PowerShell 7 and once
# with Windows PowerShell 5.1.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$requiredTests = [string[]]@(
    'statistics-self-test.ps1',
    'display-identity-self-test.ps1',
    'terminal-specs-self-test.ps1',
    'evidence-snapshot-self-test.ps1',
    'harness-provenance-self-test.ps1',
    'isolated-configs-self-test.ps1',
    'json-io-self-test.ps1',
    'payload-contract-self-test.ps1',
    'process-capture-self-test.ps1',
    'startup-ready-self-test.ps1',
    'go-signal-self-test.ps1',
    'release-statistics-self-test.ps1',
    'baseline-statistics-self-test.ps1',
    'score-statistics-self-test.ps1',
    'throughput-channel-self-test.ps1',
    'vtebench-channel-self-test.ps1',
    'sanitize-results-self-test.ps1',
    'score-self-test.ps1',
    'release-score-self-test.ps1'
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
