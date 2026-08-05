# GUI-free contract test for latency.ps1's summarisation.
#
# latency.ps1 was the one harness module with no self-test, and it shipped a
# defect that made it impossible to complete a single run: it called
# `Get-KettlePerfNearestRankPercentile` with 90 / 95 / 99 while that function
# declares `[ValidateRange(0.0, 1.0)]`, so PowerShell rejected the call at
# parameter binding -- before the body ran -- with "The 90 argument is greater
# than the maximum allowed range of 1". Every other caller in the harness
# already passed a fraction.
#
# This pins the calling convention at its source rather than restating the
# arithmetic: it reads latency.ps1's own percentile calls and drives each
# extracted argument through the real function.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\statistics.ps1"

function Assert-KettlePerfSelfTest {
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

$latencyPath = Join-Path $PSScriptRoot 'latency.ps1'
Assert-KettlePerfSelfTest -Condition (Test-Path -LiteralPath $latencyPath) `
    -Message 'latency.ps1 is missing'
$latencySource = Get-Content -LiteralPath $latencyPath -Raw

# Every `-Percentile <literal>` latency.ps1 passes, in source order.
$matches = [regex]::Matches(
    $latencySource, '-Percentile\s+(?<value>[0-9]+(?:\.[0-9]+)?)')
Assert-KettlePerfSelfTest -Condition ($matches.Count -ge 4) `
    -Message ("latency.ps1 should pass at least four percentiles; found " +
        "$($matches.Count) -- if the summarisation moved, point this test at it")

foreach ($match in $matches) {
    $value = [double]$match.Groups['value'].Value
    Assert-KettlePerfSelfTest -Condition ($value -ge 0.0 -and $value -le 1.0) `
        -Message ("latency.ps1 passes -Percentile $value, but the function " +
            'takes a FRACTION in [0.0, 1.0] and rejects anything else at ' +
            'parameter binding -- no latency run could complete')

    # Drive the real function with the real argument: the range check above is
    # necessary but not sufficient, and this is what actually proves the call
    # binds.
    $observed = Get-KettlePerfNearestRankPercentile `
        -Values @(1..100) -Percentile $value
    Assert-KettlePerfSelfTest -Condition ($null -ne $observed) `
        -Message "percentile $value did not produce an observation"
}

# The fixture must be able to tell a percentage from a fraction, or the loop
# above would pass against the broken values it exists to catch.
$rejected = $false
try {
    $null = Get-KettlePerfNearestRankPercentile -Values @(1..100) -Percentile 95
} catch {
    $rejected = $true
}
Assert-KettlePerfSelfTest -Condition $rejected `
    -Message ('the percentile function accepted 95, so this test cannot ' +
        'detect the percentage-for-fraction mistake it exists for')

# And the fractions latency.ps1 uses select the observations they name.
Assert-KettlePerfSelfTest -Condition (
    (Get-KettlePerfNearestRankPercentile -Values @(1..100) -Percentile 0.90) -eq 90.0 -and
    (Get-KettlePerfNearestRankPercentile -Values @(1..100) -Percentile 0.95) -eq 95.0 -and
    (Get-KettlePerfNearestRankPercentile -Values @(1..100) -Percentile 0.99) -eq 99.0
) -Message 'nearest-rank percentiles did not land on the expected observations'

Write-Output 'latency self-test: OK'
