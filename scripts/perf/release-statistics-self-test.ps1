# GUI-free, cross-engine contract tests for the strict release statistics gate.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\release-statistics.ps1"

function Assert-KettlePerfReleaseSelfTest {
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

function Test-KettlePerfReleaseThrow {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action
    )

    try {
        & $Action
    } catch {
        return $true
    }
    return $false
}

function Get-KettlePerfReleaseFixture {
    param(
        [Parameter(Mandatory = $true)]
        [double[]]$CandidateValues,
        [Parameter(Mandatory = $true)]
        [string[]]$Peers,
        [Parameter(Mandatory = $true)]
        [hashtable]$PeerValues
    )

    $observations = [Collections.Generic.List[object]]::new()
    $sequence = [int64]1
    $terminals = [string[]]@('kettle') + $Peers
    foreach ($terminal in $terminals) {
        $values = if ($terminal -ceq 'kettle') {
            $CandidateValues
        } else {
            [double[]]$PeerValues[$terminal]
        }
        if ($values.Count -ne $CandidateValues.Count) {
            throw "fixture terminal '$terminal' has the wrong value count"
        }
        for ($index = 0; $index -lt $values.Count; $index++) {
            $observations.Add(
                [pscustomobject][ordered]@{
                    terminal = $terminal
                    cluster_id = 'round-{0:D2}' -f ($index + 1)
                    sequence = $sequence
                    value = [double]$values[$index]
                    status = 'ok'
                }
            )
            $sequence++
        }
    }
    return [object[]]$observations.ToArray()
}

function Copy-KettlePerfReleaseFixture {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations
    )

    return [object[]]@(
        foreach ($observation in $Observations) {
            [pscustomobject][ordered]@{
                terminal = [string]$observation.terminal
                cluster_id = [string]$observation.cluster_id
                sequence = [int64]$observation.sequence
                value = [double]$observation.value
                status = [string]$observation.status
            }
        }
    )
}

$peers = [string[]]@('alacritty', 'wezterm', 'rio')
$constant120 = [double[]]@(120, 120, 120, 120, 120, 120)
$positiveObservations = Get-KettlePerfReleaseFixture `
    -CandidateValues $constant120 -Peers $peers -PeerValues @{
        alacritty = [double[]]@(100, 100, 100, 100, 100, 100)
        wezterm = [double[]]@(80, 80, 80, 80, 80, 80)
        rio = [double[]]@(60, 60, 60, 60, 60, 60)
    }
$positive = Get-KettlePerfReleaseComparison `
    -Observations $positiveObservations -IsolatedPeers $peers `
    -Direction higher -RelativeMargin 0.05 `
    -BootstrapIterations 1000 -Seed 'release-positive-v1'
Assert-KettlePerfReleaseSelfTest -Condition (
    $positive.passed -and
    $positive.policy.confirmed_wins -eq 3 -and
    $positive.policy.confirmed_losses -eq 0 -and
    $positive.policy.uncertain -eq 0 -and
    $positive.drift.passed -and
    @($positive.comparisons | Where-Object {
        $_.classification -ne 'confirmed-win'
    }).Count -eq 0
) -Message 'positive release fixture did not pass with three confirmed wins'

$classificationObservations = Get-KettlePerfReleaseFixture `
    -CandidateValues $constant120 -Peers $peers -PeerValues @{
        alacritty = [double[]]@(100, 100, 100, 100, 100, 100)
        wezterm = [double[]]@(140, 140, 140, 140, 140, 140)
        rio = [double[]]@(100, 140, 100, 140, 100, 140)
    }
$classified = Get-KettlePerfReleaseComparison `
    -Observations $classificationObservations -IsolatedPeers $peers `
    -Direction lower -RelativeMargin 0.05 `
    -BootstrapIterations 1000 -Seed 'release-classification-v1'
$classifications = [string[]]@(
    $classified.comparisons | ForEach-Object { $_.classification }
)
Assert-KettlePerfReleaseSelfTest -Condition (
    $classifications[0] -ceq 'confirmed-loss' -and
    $classifications[1] -ceq 'confirmed-win' -and
    $classifications[2] -ceq 'uncertain' -and
    -not $classified.policy.passed
) -Message 'win, loss, and uncertain classifications were not strict'

$zeroObservations = Get-KettlePerfReleaseFixture `
    -CandidateValues ([double[]]@(0, 0, 0, 0, 0, 0)) `
    -Peers $peers -PeerValues @{
        alacritty = [double[]]@(0, 0, 0, 0, 0, 0)
        wezterm = [double[]]@(0, 0, 0, 0, 0, 0)
        rio = [double[]]@(0, 0, 0, 0, 0, 0)
    }
$zeroResult = Get-KettlePerfReleaseComparison `
    -Observations $zeroObservations -IsolatedPeers $peers `
    -Direction higher -RelativeMargin 0.05 `
    -BootstrapIterations 1000 -Seed 'release-zero-v1'
Assert-KettlePerfReleaseSelfTest -Condition (
    -not $zeroResult.passed -and
    $zeroResult.policy.uncertain -eq 3 -and
    $zeroResult.drift.passed -and
    $zeroResult.comparisons[0].interval.lower -eq 0.0 -and
    $zeroResult.comparisons[0].interval.upper -eq 0.0
) -Message 'zero-valued observations were not handled without discontinuity'

$unmatched = Copy-KettlePerfReleaseFixture -Observations $positiveObservations
$unmatchedPeer = $unmatched | Where-Object {
    $_.terminal -ceq 'alacritty' -and
    $_.cluster_id -ceq 'round-06'
} | Select-Object -First 1
$unmatchedPeer.cluster_id = 'round-unmatched'
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $unmatched -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'unmatched clusters were accepted'

$duplicate = [Collections.Generic.List[object]]::new()
$duplicate.AddRange(
    [object[]](Copy-KettlePerfReleaseFixture -Observations $positiveObservations)
)
$duplicate.Add(
    [pscustomobject][ordered]@{
        terminal = 'alacritty'
        cluster_id = 'round-01'
        sequence = [int64]9999
        value = 100.0
        status = 'ok'
    }
)
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $duplicate.ToArray() -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'duplicate matched cluster was accepted'

$nonFinite = Copy-KettlePerfReleaseFixture -Observations $positiveObservations
$nonFinite[0].value = [double]::NaN
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $nonFinite -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'non-finite observation was accepted'

$badStatus = Copy-KettlePerfReleaseFixture -Observations $positiveObservations
$badStatus[0].status = 'miss'
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $badStatus -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'non-ok observation status was accepted'

$extraProperty = Copy-KettlePerfReleaseFixture `
    -Observations $positiveObservations
$extraProperty[0] | Add-Member -NotePropertyName diagnostic `
    -NotePropertyValue 'not-part-of-schema'
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $extraProperty -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'observation with an extra schema property was accepted'

$duplicateSequence = Copy-KettlePerfReleaseFixture `
    -Observations $positiveObservations
$duplicateSequence[1].sequence = $duplicateSequence[0].sequence
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $duplicateSequence -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'duplicate terminal sequence was accepted'

Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $positiveObservations `
            -IsolatedPeers ([string[]]@('alacritty', 'wezterm')) `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'fewer than four total terminals were accepted'
Assert-KettlePerfReleaseSelfTest -Condition (
    Test-KettlePerfReleaseThrow -Action {
        Get-KettlePerfReleaseComparison `
            -Observations $positiveObservations -IsolatedPeers $peers `
            -Direction higher -BootstrapIterations 999 -Seed 'invalid'
    }
) -Message 'bootstrap iteration lower bound was not enforced'

$driftValues = [double[]]@(100..111)
$driftObservations = Get-KettlePerfReleaseFixture `
    -CandidateValues $driftValues -Peers $peers -PeerValues @{
        alacritty = [double[]]@(100..111)
        wezterm = [double[]]@(100..111)
        rio = [double[]]@(100..111)
    }
$driftResult = Get-KettlePerfReleaseDriftDiagnostic `
    -Observations $driftObservations -IsolatedPeers $peers
Assert-KettlePerfReleaseSelfTest -Condition (
    -not $driftResult.passed -and
    $driftResult.failed_terminal_count -eq 4 -and
    -not $driftResult.terminals[0].trend_passed -and
    $driftResult.terminals[0].peak_to_peak_passed
) -Message 'greater-than-ten-percent fitted first-to-last drift passed'

$peakValues = [double[]]@(80, 120, 80, 120, 80, 120, 80, 120)
$peakObservations = Get-KettlePerfReleaseFixture `
    -CandidateValues $peakValues -Peers $peers -PeerValues @{
        alacritty = [double[]]$peakValues.Clone()
        wezterm = [double[]]$peakValues.Clone()
        rio = [double[]]$peakValues.Clone()
    }
$peakResult = Get-KettlePerfReleaseDriftDiagnostic `
    -Observations $peakObservations -IsolatedPeers $peers
Assert-KettlePerfReleaseSelfTest -Condition (
    -not $peakResult.passed -and
    $peakResult.failed_terminal_count -eq 4 -and
    $peakResult.terminals[0].trend_passed -and
    -not $peakResult.terminals[0].peak_to_peak_passed
) -Message 'greater-than-twenty-percent peak-to-peak drift passed'

$throughputPositive = Get-KettlePerfThroughputRoundGate `
    -Comparison $positive
Assert-KettlePerfReleaseSelfTest -Condition (
    $throughputPositive.passed -and
    $throughputPositive.failed_round_composites -eq 0
) -Message 'all-positive throughput rounds did not pass'

$savedAdjustedPair = (
    $positive.comparisons[0].adjusted_pairs[-1].
        favorable_difference_after_margin
)
$positive.comparisons[0].adjusted_pairs[-1].
    favorable_difference_after_margin = -0.001
$throughputBad = Get-KettlePerfThroughputRoundGate `
    -Comparison $positive
$positive.comparisons[0].adjusted_pairs[-1].
    favorable_difference_after_margin = $savedAdjustedPair
Assert-KettlePerfReleaseSelfTest -Condition (
    -not $throughputBad.passed -and
    $throughputBad.failed_round_composites -eq 1 -and
    $positive.policy.passed
) -Message 'a non-positive five-percent-adjusted throughput round passed'

$culture = [Globalization.CultureInfo]::InvariantCulture
$canonicalFields = [Collections.Generic.List[string]]::new()
$canonicalFields.Add(
    (($positive.comparisons | ForEach-Object {
        $_.classification
    }) -join ',')
)
$canonicalFields.Add(
    $positive.policy.confirmed_wins.ToString($culture)
)
$canonicalFields.Add(
    $positive.drift.failed_terminal_count.ToString($culture)
)
$canonicalFields.Add(
    (($classified.comparisons | ForEach-Object {
        $_.classification
    }) -join ',')
)
$canonicalFields.Add(
    $classified.comparisons[2].interval.lower.ToString('F6', $culture)
)
$canonicalFields.Add(
    $classified.comparisons[2].interval.upper.ToString('F6', $culture)
)
$canonicalFields.Add(
    $zeroResult.policy.uncertain.ToString($culture)
)
$canonicalFields.Add(
    $driftResult.terminals[0].absolute_fitted_first_to_last_pct.ToString(
        'F6',
        $culture
    )
)
$canonicalFields.Add(
    $peakResult.terminals[0].peak_to_peak_normalized_pct.ToString(
        'F6',
        $culture
    )
)
$canonicalFields.Add(
    $throughputPositive.failed_round_composites.ToString($culture)
)
$canonicalFields.Add(
    $throughputBad.failed_round_composites.ToString($culture)
)
$canonicalFields.Add(
    $positive.comparisons[0].interval.seed_sha256
)
$canonical = $canonicalFields.ToArray() -join '|'
$fixtureHash = Get-KettlePerfSha256Hex -Text $canonical
$expectedFixtureHash = (
    '1fe8d113d8472713d7f655fb0bcff73a08f217920a0082b0d122c7cbe3e4eb9b'
)
Assert-KettlePerfReleaseSelfTest -Condition (
    $fixtureHash -ceq $expectedFixtureHash
) -Message "release statistics fixture drifted: $fixtureHash"

Write-Output (
    "release statistics self-test: PASS (fixture=$fixtureHash)"
)
