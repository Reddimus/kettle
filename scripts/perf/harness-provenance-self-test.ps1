# GUI-free contract tests for immutable harness-source provenance.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\harness-provenance.ps1"

function Assert-HarnessProvenance {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

$expectedProductionFiles = @(
    Get-KettlePerfHarnessFileNames | Sort-Object
)
$actualProductionFiles = @(
    Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*.ps1' -File |
        Where-Object {
            $_.Name -notlike '*-self-test.ps1' -and
            $_.Name -ne 'self-test.ps1'
        } |
        ForEach-Object { $_.Name } |
        Sort-Object
)
$coverageDiff = @(Compare-Object $expectedProductionFiles $actualProductionFiles)
Assert-HarnessProvenance ($coverageDiff.Count -eq 0) `
    'pinned harness provenance does not cover every production PowerShell file'

$testRoot = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-harness-provenance-' + [Guid]::NewGuid().ToString('N')
)
[void][IO.Directory]::CreateDirectory($testRoot)
$locks = @()
try {
    foreach ($name in Get-KettlePerfHarnessFileNames) {
        $bytes = [Text.Encoding]::ASCII.GetBytes("fixture:$name`n")
        [IO.File]::WriteAllBytes((Join-Path $testRoot $name), $bytes)
    }

    $locks = @(Open-KettlePerfHarnessLocks -ScriptDirectory $testRoot)
    $first = Get-KettlePerfHarnessProvenance -Locks $locks
    $second = Get-KettlePerfHarnessProvenance -Locks $locks
    Assert-HarnessProvenance `
        ($first.aggregate_sha256 -ceq $second.aggregate_sha256) `
        'locked harness provenance was not deterministic'
    Assert-HarnessProvenance `
        ($first.files.Count -eq (Get-KettlePerfHarnessFileNames).Count) `
        'harness provenance omitted a pinned source file'

    $protectedPath = Join-Path $testRoot 'perf-all.ps1'
    $writeRejected = $false
    try {
        [IO.File]::WriteAllText($protectedPath, 'tampered')
    } catch [IO.IOException] {
        $writeRejected = $true
    }
    Assert-HarnessProvenance $writeRejected `
        'retained harness lock allowed source overwrite'

    $deleteRejected = $false
    try {
        [IO.File]::Delete($protectedPath)
    } catch [IO.IOException] {
        $deleteRejected = $true
    }
    Assert-HarnessProvenance $deleteRejected `
        'retained harness lock allowed source deletion'

    Close-KettlePerfHarnessLocks -Locks $locks
    $locks = @()
    [IO.File]::WriteAllText($protectedPath, 'tampered')
    $changedLocks = @(Open-KettlePerfHarnessLocks -ScriptDirectory $testRoot)
    try {
        $changed = Get-KettlePerfHarnessProvenance -Locks $changedLocks
        Assert-HarnessProvenance `
            ($changed.aggregate_sha256 -cne $first.aggregate_sha256) `
            'harness provenance did not detect source tampering'
    } finally {
        Close-KettlePerfHarnessLocks -Locks $changedLocks
    }
} finally {
    Close-KettlePerfHarnessLocks -Locks $locks
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}

Write-Output 'harness-provenance self-test: PASS'
