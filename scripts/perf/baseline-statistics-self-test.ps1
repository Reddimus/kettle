# GUI-free cross-engine contract tests for Kettle baseline non-inferiority.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\baseline-statistics.ps1"

function Assert-KettlePerfBaselineSelfTest {
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

function Test-KettlePerfBaselineThrow {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action
    )

    try {
        $null = & $Action
    } catch {
        return $true
    }
    return $false
}

function Get-KettlePerfBaselineFixture {
    param(
        [Parameter(Mandatory = $true)]
        [double[]]$Values,
        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 1000000)]
        [int]$SequenceStart
    )

    return [object[]]@(
        for ($index = 0; $index -lt $Values.Count; $index++) {
            [pscustomobject][ordered]@{
                terminal = 'kettle'
                cluster_id = 'cluster-{0:D2}' -f ($index + 1)
                sequence = [int64]($SequenceStart + $index)
                value = [double]$Values[$index]
                status = 'ok'
            }
        }
    )
}

function Copy-KettlePerfBaselineFixture {
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

$baseline100 = Get-KettlePerfBaselineFixture `
    -Values ([double[]]@(100, 100, 100, 100, 100, 100)) `
    -SequenceStart 101

$higherPass = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations (
        Get-KettlePerfBaselineFixture `
            -Values ([double[]]@(98, 98, 98, 98, 98, 98)) `
            -SequenceStart 1
    ) `
    -BaselineObservations $baseline100 -Metric 'throughput' `
    -Direction higher -AbsoluteMargin 3.0 `
    -Seed 'baseline-higher-pass-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    $higherPass.passed -and
    $higherPass.classification -ceq 'pass' -and
    $higherPass.bootstrap_iterations -eq 10000 -and
    $higherPass.interval.iterations -eq 10000 -and
    $higherPass.interval.lower -eq 1.0 -and
    $higherPass.interval.upper -eq 1.0 -and
    $higherPass.adjusted_pairs[0].
        raw_favorable_current_vs_baseline_residual -eq -2.0 -and
    $higherPass.adjusted_pairs[0].
        practical_non_inferiority_margin -eq 3.0 -and
    $higherPass.adjusted_pairs[0].adjusted_favorable_residual -eq 1.0
) -Message 'higher-is-better absolute-margin non-inferiority did not pass'

$higherFail = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations (
        Get-KettlePerfBaselineFixture `
            -Values ([double[]]@(90, 90, 90, 90, 90, 90)) `
            -SequenceStart 1
    ) `
    -BaselineObservations $baseline100 -Metric 'startup-regression' `
    -Direction higher -BootstrapIterations 1000 `
    -Seed 'baseline-higher-fail-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    -not $higherFail.passed -and
    $higherFail.classification -ceq 'fail' -and
    $higherFail.interval.upper -eq -10.0
) -Message 'confirmed inferior higher-is-better data did not fail'

$uncertainValues = [double[]]@(99, 99, 99, 101, 101, 101)
$uncertain = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations (
        Get-KettlePerfBaselineFixture `
            -Values $uncertainValues -SequenceStart 1
    ) `
    -BaselineObservations $baseline100 -Metric 'latency-uncertain' `
    -Direction higher -BootstrapIterations 1000 `
    -Seed 'baseline-uncertain-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    -not $uncertain.passed -and
    $uncertain.classification -ceq 'uncertain' -and
    $uncertain.interval.lower -le 0.0 -and
    $uncertain.interval.upper -ge 0.0
) -Message 'zero-crossing interval was not classified as uncertain'

$lowerPass = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations (
        Get-KettlePerfBaselineFixture `
            -Values ([double[]]@(102, 102, 102, 102, 102, 102)) `
            -SequenceStart 1
    ) `
    -BaselineObservations $baseline100 -Metric 'startup' `
    -Direction lower -AbsoluteMargin 3.0 `
    -BootstrapIterations 1000 -Seed 'baseline-lower-pass-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    $lowerPass.passed -and
    $lowerPass.interval.lower -eq 1.0 -and
    $lowerPass.adjusted_pairs[0].
        raw_favorable_current_vs_baseline_residual -eq -2.0
) -Message 'lower-is-better non-inferiority did not pass'

$relativePass = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations (
        Get-KettlePerfBaselineFixture `
            -Values ([double[]]@(96, 96, 96, 96, 96, 96)) `
            -SequenceStart 1
    ) `
    -BaselineObservations $baseline100 -Metric 'memory' `
    -Direction higher -RelativeMargin 0.05 `
    -BootstrapIterations 1000 -Seed 'baseline-relative-pass-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    $relativePass.passed -and
    $relativePass.adjusted_pairs[0].
        relative_margin_component -eq 5.0 -and
    $relativePass.adjusted_pairs[0].adjusted_favorable_residual -eq 1.0
) -Message 'baseline-relative practical margin was not applied'

$zeroBaseline = Get-KettlePerfBaselineFixture `
    -Values ([double[]]@(0, 0, 0, 0, 0, 0)) -SequenceStart 101
$zeroCurrent = Get-KettlePerfBaselineFixture `
    -Values ([double[]]@(0, 0, 0, 0, 0, 0)) -SequenceStart 1
$zeroRelative = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations $zeroCurrent -BaselineObservations $zeroBaseline `
    -Metric 'idle-zero-relative' -Direction lower -RelativeMargin 0.05 `
    -BootstrapIterations 1000 -Seed 'baseline-zero-relative-v1'
$zeroAbsolute = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations $zeroCurrent -BaselineObservations $zeroBaseline `
    -Metric 'idle-zero-absolute' -Direction lower -AbsoluteMargin 0.10 `
    -BootstrapIterations 1000 -Seed 'baseline-zero-absolute-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    -not $zeroRelative.passed -and
    $zeroRelative.classification -ceq 'uncertain' -and
    $zeroRelative.interval.lower -eq 0.0 -and
    $zeroAbsolute.passed -and
    $zeroAbsolute.interval.lower -eq 0.10
) -Message 'zero baseline did not use absolute and relative margins safely'

$policyPass = Get-KettlePerfBaselinePolicy `
    -MetricResults ([object[]]@($higherPass, $lowerPass, $relativePass)) `
    -RequiredMetrics ([string[]]@('throughput', 'startup', 'memory'))
$policyUncertain = Get-KettlePerfBaselinePolicy `
    -MetricResults ([object[]]@($higherPass, $uncertain)) `
    -RequiredMetrics ([string[]]@('throughput', 'latency-uncertain'))
$policyMissing = Get-KettlePerfBaselinePolicy `
    -MetricResults ([object[]]@($higherPass)) `
    -RequiredMetrics ([string[]]@('throughput', 'startup'))
Assert-KettlePerfBaselineSelfTest -Condition (
    $policyPass.passed -and
    -not $policyUncertain.passed -and
    $policyUncertain.uncertain_metrics.Count -eq 1 -and
    -not $policyMissing.passed -and
    $policyMissing.missing_metrics.Count -eq 1
) -Message 'all-required-metrics policy did not enforce every baseline gate'

$unmatched = Copy-KettlePerfBaselineFixture -Observations (
    Get-KettlePerfBaselineFixture `
        -Values ([double[]]@(98, 98, 98, 98, 98, 98)) -SequenceStart 1
)
$unmatched[-1].cluster_id = 'not-in-baseline'
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $unmatched `
            -BaselineObservations $baseline100 -Metric 'invalid' `
            -Direction higher -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'unmatched clusters were accepted'

$duplicateCluster = Copy-KettlePerfBaselineFixture -Observations $baseline100
$duplicateCluster[-1].cluster_id = $duplicateCluster[0].cluster_id
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $duplicateCluster -Metric 'invalid' `
            -Direction lower -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'duplicate cluster was accepted'

$duplicateSequence = Copy-KettlePerfBaselineFixture -Observations $baseline100
$duplicateSequence[-1].sequence = $duplicateSequence[0].sequence
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $duplicateSequence -Metric 'invalid' `
            -Direction lower -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'duplicate sequence was accepted'

$nonFinite = Copy-KettlePerfBaselineFixture -Observations $baseline100
$nonFinite[0].value = [double]::NaN
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $nonFinite -Metric 'invalid' `
            -Direction lower -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'non-finite value was accepted'

$badStatus = Copy-KettlePerfBaselineFixture -Observations $baseline100
$badStatus[0].status = 'miss'
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $badStatus -Metric 'invalid' `
            -Direction lower -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'non-ok status was accepted'

$extraProperty = Copy-KettlePerfBaselineFixture -Observations $baseline100
$extraProperty[0] | Add-Member -NotePropertyName diagnostic `
    -NotePropertyValue 'not-raw-schema'
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $extraProperty -Metric 'invalid' `
            -Direction lower -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'extra observation property was accepted'

$missingProperty = [object[]]@(
    foreach ($observation in $baseline100) {
        [pscustomobject][ordered]@{
            terminal = $observation.terminal
            cluster_id = $observation.cluster_id
            sequence = $observation.sequence
            value = $observation.value
        }
    }
)
Assert-KettlePerfBaselineSelfTest -Condition (
    Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $missingProperty -Metric 'invalid' `
            -Direction lower -BootstrapIterations 1000 -Seed 'invalid'
    }
) -Message 'missing observation property was accepted'

Assert-KettlePerfBaselineSelfTest -Condition (
    (Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $zeroBaseline -Metric 'invalid' `
            -Direction lower -BootstrapIterations 999 -Seed 'invalid'
    }) -and
    (Test-KettlePerfBaselineThrow -Action {
        Get-KettlePerfBaselineNonInferiority `
            -CurrentObservations $zeroCurrent `
            -BaselineObservations $zeroBaseline -Metric 'invalid' `
            -Direction lower -BootstrapIterations 100001 -Seed 'invalid'
    })
) -Message 'bootstrap iteration bounds were not enforced'

$trendValues = [double[]]@(100..111)
$trendCurrent = Get-KettlePerfBaselineFixture `
    -Values $trendValues -SequenceStart 1
$trendBaseline = Get-KettlePerfBaselineFixture `
    -Values ([double[]]@(100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100)) `
    -SequenceStart 101
$trendResult = Get-KettlePerfBaselineNonInferiority `
    -CurrentObservations $trendCurrent `
    -BaselineObservations $trendBaseline -Metric 'trend-drift' `
    -Direction higher -AbsoluteMargin 1.0 `
    -BootstrapIterations 1000 -Seed 'baseline-trend-drift-v1'
Assert-KettlePerfBaselineSelfTest -Condition (
    $trendResult.interval_classification -ceq 'pass' -and
    $trendResult.classification -ceq 'fail' -and
    -not $trendResult.current_drift.trend_passed -and
    $trendResult.baseline_drift.passed -and
    -not $trendResult.passed
) -Message 'greater-than-ten-percent fitted drift did not fail the gate'

$peakValues = [double[]]@(80, 120, 80, 120, 80, 120, 80, 120)
$peakDiagnostic = Get-KettlePerfBaselineDriftDiagnostic `
    -Observations (
        Get-KettlePerfBaselineFixture -Values $peakValues -SequenceStart 1
    ) -DataSetName current
Assert-KettlePerfBaselineSelfTest -Condition (
    $peakDiagnostic.trend_passed -and
    -not $peakDiagnostic.peak_to_peak_passed -and
    -not $peakDiagnostic.passed
) -Message 'greater-than-twenty-percent normalized range passed drift'

$culture = [Globalization.CultureInfo]::InvariantCulture
$canonicalFields = [Collections.Generic.List[string]]::new()
$canonicalFields.Add($higherPass.classification)
$canonicalFields.Add(
    $higherPass.interval.lower.ToString('F6', $culture)
)
$canonicalFields.Add($higherFail.classification)
$canonicalFields.Add(
    $higherFail.interval.upper.ToString('F6', $culture)
)
$canonicalFields.Add($uncertain.classification)
$canonicalFields.Add(
    $uncertain.interval.lower.ToString('F6', $culture)
)
$canonicalFields.Add(
    $uncertain.interval.upper.ToString('F6', $culture)
)
$canonicalFields.Add($lowerPass.classification)
$canonicalFields.Add($relativePass.classification)
$canonicalFields.Add($zeroRelative.classification)
$canonicalFields.Add($zeroAbsolute.classification)
$canonicalFields.Add(
    $higherPass.adjusted_pairs[0].
        adjusted_favorable_residual.ToString('F6', $culture)
)
$canonicalFields.Add(
    $relativePass.adjusted_pairs[0].
        practical_non_inferiority_margin.ToString('F6', $culture)
)
$canonicalFields.Add(
    $trendResult.current_drift.
        absolute_fitted_first_to_last_pct.ToString('F6', $culture)
)
$canonicalFields.Add(
    $peakDiagnostic.peak_to_peak_normalized_pct.ToString('F6', $culture)
)
$canonicalFields.Add($policyPass.passed.ToString())
$canonicalFields.Add($policyUncertain.passed.ToString())
$canonicalFields.Add($policyMissing.missing_metrics.Count.ToString($culture))
$canonicalFields.Add($higherPass.interval.seed_sha256)
$canonical = $canonicalFields.ToArray() -join '|'
$fixtureHash = Get-KettlePerfSha256Hex -Text $canonical
$expectedFixtureHash = (
    '9551f1476a82020c13c4cab805e0bf9d5d55b2c2d52aa4c27238e4d864040440'
)
Assert-KettlePerfBaselineSelfTest -Condition (
    $fixtureHash -ceq $expectedFixtureHash
) -Message "baseline statistics fixture drifted: $fixtureHash"

Write-Output (
    "baseline statistics self-test: PASS (fixture=$fixtureHash)"
)
