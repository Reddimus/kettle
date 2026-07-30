# Deterministic non-inferiority checks against paired Kettle baselines.
#
# This layer accepts raw observations only. Aggregate inputs would make cluster
# matching, practical-margin adjustment, and drift checks unverifiable.

. "$PSScriptRoot\statistics.ps1"

function Test-KettlePerfBaselineNumericType {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    if ($null -eq $Value) {
        return $false
    }
    $typeCode = [Type]::GetTypeCode($Value.GetType())
    return $typeCode -in @(
        [TypeCode]::Byte,
        [TypeCode]::SByte,
        [TypeCode]::Int16,
        [TypeCode]::UInt16,
        [TypeCode]::Int32,
        [TypeCode]::UInt32,
        [TypeCode]::Int64,
        [TypeCode]::UInt64,
        [TypeCode]::Single,
        [TypeCode]::Double,
        [TypeCode]::Decimal
    )
}

function Get-KettlePerfBaselineIdentifier {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Value,
        [Parameter(Mandatory = $true)]
        [string]$FieldName,
        [ValidateRange(1, 4096)]
        [int]$MaximumLength = 256
    )

    if ($Value -isnot [string]) {
        throw "$FieldName must be a string"
    }
    $text = [string]$Value
    if (
        [string]::IsNullOrWhiteSpace($text) -or
        $text.Length -gt $MaximumLength -or
        $text -cne $text.Trim()
    ) {
        throw "$FieldName is empty, padded, or too long"
    }
    foreach ($character in $text.ToCharArray()) {
        if ([char]::IsControl($character)) {
            throw "$FieldName contains a control character"
        }
    }
    return $text
}

function Get-KettlePerfBaselineSequence {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    if (-not (Test-KettlePerfBaselineNumericType -Value $Value)) {
        throw 'sequence must be an integer'
    }
    try {
        $number = [decimal]$Value
    } catch {
        throw 'sequence is outside the supported integer range'
    }
    if (
        $number -lt [decimal]1 -or
        $number -gt [decimal][int64]::MaxValue -or
        $number -ne [Math]::Truncate($number)
    ) {
        throw 'sequence must be a positive 64-bit integer'
    }
    return [int64]$number
}

function Get-KettlePerfBaselineValue {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    if (-not (Test-KettlePerfBaselineNumericType -Value $Value)) {
        throw 'value must be numeric'
    }
    $number = [double]$Value
    if (
        [double]::IsNaN($number) -or
        [double]::IsInfinity($number) -or
        $number -lt 0.0
    ) {
        throw 'value must be finite and non-negative'
    }
    return $number
}

function ConvertTo-KettlePerfBaselineDataSet {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations,
        [Parameter(Mandatory = $true)]
        [ValidateSet('current', 'baseline')]
        [string]$DataSetName
    )

    if ($DataSetName -cnotin @('current', 'baseline')) {
        throw 'DataSetName must use the exact value current or baseline'
    }
    if ($Observations.Count -lt 6 -or $Observations.Count -gt 1000) {
        throw "$DataSetName must contain between 6 and 1000 observations"
    }

    $expectedProperties = [string[]]@(
        'terminal',
        'cluster_id',
        'sequence',
        'value',
        'status'
    )
    $normalized = [Collections.Generic.List[object]]::new()
    $clusterMap = [Collections.Generic.Dictionary[string, object]]::new(
        [StringComparer]::Ordinal
    )
    $sequenceSet = [Collections.Generic.HashSet[int64]]::new()

    foreach ($observation in $Observations) {
        if ($null -eq $observation) {
            throw "$DataSetName observation cannot be null"
        }
        $actualProperties = [string[]]@(
            $observation.PSObject.Properties |
                ForEach-Object { $_.Name }
        )
        if ($actualProperties.Count -ne $expectedProperties.Count) {
            throw (
                "$DataSetName observation must contain exactly terminal, " +
                'cluster_id, sequence, value, and status'
            )
        }
        foreach ($propertyName in $expectedProperties) {
            if (-not ($actualProperties -ccontains $propertyName)) {
                throw (
                    "$DataSetName observation is missing exact property " +
                    "'$propertyName'"
                )
            }
        }

        $terminal = $observation.PSObject.Properties['terminal'].Value
        if ($terminal -isnot [string] -or [string]$terminal -cne 'kettle') {
            throw "$DataSetName terminal must be exactly kettle"
        }
        $clusterId = Get-KettlePerfBaselineIdentifier `
            -Value $observation.PSObject.Properties['cluster_id'].Value `
            -FieldName "$DataSetName cluster_id" -MaximumLength 256
        $sequence = Get-KettlePerfBaselineSequence `
            -Value $observation.PSObject.Properties['sequence'].Value
        $value = Get-KettlePerfBaselineValue `
            -Value $observation.PSObject.Properties['value'].Value
        $status = $observation.PSObject.Properties['status'].Value
        if ($status -isnot [string] -or [string]$status -cne 'ok') {
            throw "$DataSetName observations must have status exactly ok"
        }
        if ($clusterMap.ContainsKey($clusterId)) {
            throw "$DataSetName contains duplicate cluster '$clusterId'"
        }
        if (-not $sequenceSet.Add($sequence)) {
            throw "$DataSetName contains duplicate sequence '$sequence'"
        }

        $item = [pscustomobject][ordered]@{
            terminal = 'kettle'
            cluster_id = $clusterId
            sequence = $sequence
            value = $value
            status = 'ok'
        }
        $normalized.Add($item)
        $clusterMap.Add($clusterId, $item)
    }

    return [pscustomobject][ordered]@{
        name = $DataSetName
        observations = [object[]]$normalized.ToArray()
        cluster_map = $clusterMap
        observation_count = $normalized.Count
    }
}

function Get-KettlePerfBaselineDriftFromDataSet {
    param(
        [Parameter(Mandatory = $true)]
        $DataSet
    )

    $ordered = [object[]]@(
        $DataSet.observations |
            Sort-Object -Property sequence
    )
    $values = [double[]]@(
        $ordered | ForEach-Object { [double]$_.value }
    )
    $sequences = [int64[]]@(
        $ordered | ForEach-Object { [int64]$_.sequence }
    )

    # Sequence values establish order. Ordinal x-values avoid losing adjacent
    # Int64 sequence values when the shared exact estimator converts x to Double.
    $theilSen = Get-KettlePerfTheilSenDrift `
        -Values $values -MaxAbsoluteDriftPct 10.0 -ZeroFloor 0.000001
    $median = [double](Get-KettlePerfMedian -Values $values)
    $measure = $values | Measure-Object -Minimum -Maximum
    $minimum = [double]$measure.Minimum
    $maximum = [double]$measure.Maximum
    $range = $maximum - $minimum
    $denominator = [Math]::Max([Math]::Abs($median), 0.000001)
    $normalizedRange = $range / $denominator
    $peakToPeakPct = if (
        [double]::IsInfinity($normalizedRange) -or
        $normalizedRange -gt ([double]::MaxValue / 100.0)
    ) {
        [double]::MaxValue
    } else {
        $normalizedRange * 100.0
    }
    $peakPassed = $peakToPeakPct -le 20.0
    $passed = [bool]$theilSen.passed -and $peakPassed

    return [pscustomobject][ordered]@{
        algorithm = 'theil-sen-and-normalized-range-v1'
        data_set = [string]$DataSet.name
        observation_count = $values.Count
        first_sequence = $sequences[0]
        last_sequence = $sequences[$sequences.Count - 1]
        theil_sen = $theilSen
        fitted_first_to_last_pct = [double]$theilSen.drift_pct
        absolute_fitted_first_to_last_pct = (
            [double]$theilSen.absolute_drift_pct
        )
        maximum_absolute_fitted_first_to_last_pct = 10.0
        minimum = $minimum
        maximum = $maximum
        median = $median
        peak_to_peak_normalized_pct = $peakToPeakPct
        maximum_peak_to_peak_normalized_pct = 20.0
        zero_floor = 0.000001
        trend_passed = [bool]$theilSen.passed
        peak_to_peak_passed = $peakPassed
        passed = $passed
    }
}

function Get-KettlePerfBaselineDriftDiagnostic {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations,
        [Parameter(Mandatory = $true)]
        [ValidateSet('current', 'baseline')]
        [string]$DataSetName
    )

    $dataSet = ConvertTo-KettlePerfBaselineDataSet `
        -Observations $Observations -DataSetName $DataSetName
    return Get-KettlePerfBaselineDriftFromDataSet -DataSet $dataSet
}

function Get-KettlePerfBaselineNonInferiority {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$CurrentObservations,
        [Parameter(Mandatory = $true)]
        [object[]]$BaselineObservations,
        [Parameter(Mandatory = $true)]
        [string]$Metric,
        [Parameter(Mandatory = $true)]
        [ValidateSet('higher', 'lower')]
        [string]$Direction,
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$AbsoluteMargin = 0.0,
        [ValidateRange(0.0, 1.0)]
        [double]$RelativeMargin = 0.0,
        [ValidateRange(1000, 100000)]
        [int]$BootstrapIterations = 10000,
        [Parameter(Mandatory = $true)]
        [string]$Seed
    )

    $metricName = Get-KettlePerfBaselineIdentifier `
        -Value $Metric -FieldName 'Metric' -MaximumLength 128
    if ($Direction -cnotin @('higher', 'lower')) {
        throw 'Direction must use the exact value higher or lower'
    }
    if (
        [double]::IsNaN($AbsoluteMargin) -or
        [double]::IsInfinity($AbsoluteMargin) -or
        [double]::IsNaN($RelativeMargin) -or
        [double]::IsInfinity($RelativeMargin)
    ) {
        throw 'baseline margins must be finite'
    }
    if (
        [string]::IsNullOrWhiteSpace($Seed) -or
        $Seed.Length -gt 4096
    ) {
        throw 'Seed must be a non-empty string of at most 4096 characters'
    }

    $current = ConvertTo-KettlePerfBaselineDataSet `
        -Observations $CurrentObservations -DataSetName current
    $baseline = ConvertTo-KettlePerfBaselineDataSet `
        -Observations $BaselineObservations -DataSetName baseline
    if ($current.observation_count -ne $baseline.observation_count) {
        throw 'current and baseline must have the same matched cluster count'
    }
    foreach ($clusterId in $current.cluster_map.Keys) {
        if (-not $baseline.cluster_map.ContainsKey($clusterId)) {
            throw "baseline is missing matched cluster '$clusterId'"
        }
    }

    $clusterIds = [string[]]@($current.cluster_map.Keys)
    [Array]::Sort($clusterIds, [StringComparer]::Ordinal)
    $higherIsBetter = $Direction -ceq 'higher'
    $adjustedValues = [double[]]::new($clusterIds.Count)
    $zeroReference = [double[]]::new($clusterIds.Count)
    $adjustedPairs = [Collections.Generic.List[object]]::new()
    $positive = 0
    $zero = 0
    $negative = 0

    for ($index = 0; $index -lt $clusterIds.Count; $index++) {
        $clusterId = $clusterIds[$index]
        $currentObservation = $current.cluster_map[$clusterId]
        $baselineObservation = $baseline.cluster_map[$clusterId]
        $currentValue = [double]$currentObservation.value
        $baselineValue = [double]$baselineObservation.value
        $margin = [Math]::Max(
            $AbsoluteMargin,
            $RelativeMargin * [Math]::Abs($baselineValue)
        )
        $rawFavorableResidual = if ($higherIsBetter) {
            $currentValue - $baselineValue
        } else {
            $baselineValue - $currentValue
        }
        # Non-inferiority tolerates degradation up to the practical margin.
        $adjustedResidual = $rawFavorableResidual + $margin
        if (
            [double]::IsNaN($adjustedResidual) -or
            [double]::IsInfinity($adjustedResidual)
        ) {
            throw 'a practical-margin-adjusted residual is non-finite'
        }
        $adjustedValues[$index] = $adjustedResidual
        if ($adjustedResidual -gt 0.0) {
            $positive++
        } elseif ($adjustedResidual -lt 0.0) {
            $negative++
        } else {
            $zero++
        }
        $adjustedPairs.Add(
            [pscustomobject][ordered]@{
                cluster_id = $clusterId
                current_sequence = [int64]$currentObservation.sequence
                baseline_sequence = [int64]$baselineObservation.sequence
                current_value = $currentValue
                baseline_value = $baselineValue
                raw_favorable_current_vs_baseline_residual = (
                    $rawFavorableResidual
                )
                absolute_margin = $AbsoluteMargin
                relative_margin_component = (
                    $RelativeMargin * [Math]::Abs($baselineValue)
                )
                practical_non_inferiority_margin = $margin
                adjusted_favorable_residual = $adjustedResidual
            }
        )
    }

    $bootstrapSeed = (
        'baseline-noninferiority-v1|seed:{0}:{1}|metric:{2}:{3}|' +
        'direction:{4}'
    ) -f $Seed.Length, $Seed, $metricName.Length, $metricName, $Direction
    $interval = Get-KettlePerfPairedClusterBootstrapInterval `
        -Candidate $adjustedValues -Reference $zeroReference `
        -ClusterIds $clusterIds -HigherIsBetter `
        -Iterations $BootstrapIterations -ConfidenceLevel 0.90 `
        -Seed $bootstrapSeed -Statistic median
    $intervalClassification = if ([double]$interval.lower -gt 0.0) {
        'pass'
    } elseif ([double]$interval.upper -lt 0.0) {
        'fail'
    } else {
        'uncertain'
    }

    $currentDrift = Get-KettlePerfBaselineDriftFromDataSet -DataSet $current
    $baselineDrift = Get-KettlePerfBaselineDriftFromDataSet -DataSet $baseline
    $driftPassed = [bool]$currentDrift.passed -and [bool]$baselineDrift.passed
    $classification = if (-not $driftPassed) {
        'fail'
    } else {
        $intervalClassification
    }
    $nonInferior = $classification -ceq 'pass'

    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'paired-baseline-noninferiority-v1'
        metric = $metricName
        terminal = 'kettle'
        direction = $Direction
        absolute_margin = $AbsoluteMargin
        relative_margin = $RelativeMargin
        practical_margin_rule = (
            'max(absolute, relative * abs(baseline))'
        )
        adjusted_residual_rule = (
            'favorable(current - baseline) + practical margin'
        )
        confidence_level = 0.90
        bootstrap_iterations = $BootstrapIterations
        statistic = 'median'
        seed_sha256 = Get-KettlePerfSha256Hex -Text $Seed
        observation_schema = [string[]]@(
            'terminal',
            'cluster_id',
            'sequence',
            'value',
            'status'
        )
        current_observation_count = $current.observation_count
        baseline_observation_count = $baseline.observation_count
        matched_cluster_count = $clusterIds.Count
        interval = $interval
        counts = [pscustomobject][ordered]@{
            positive_adjusted_residuals = $positive
            zero_adjusted_residuals = $zero
            negative_adjusted_residuals = $negative
        }
        adjusted_pairs = [object[]]$adjustedPairs.ToArray()
        current_drift = $currentDrift
        baseline_drift = $baselineDrift
        drift = [pscustomobject][ordered]@{
            current_passed = [bool]$currentDrift.passed
            baseline_passed = [bool]$baselineDrift.passed
            passed = $driftPassed
        }
        interval_classification = $intervalClassification
        classification = $classification
        uncertainty_is_pass = $false
        non_inferior = $nonInferior
        passed = $nonInferior
    }
}

function Get-KettlePerfBaselinePolicy {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$MetricResults,
        [Parameter(Mandatory = $true)]
        [string[]]$RequiredMetrics
    )

    if ($RequiredMetrics.Count -lt 1 -or $RequiredMetrics.Count -gt 128) {
        throw 'RequiredMetrics must contain between 1 and 128 metric names'
    }
    if ($MetricResults.Count -gt 256) {
        throw 'MetricResults cannot contain more than 256 results'
    }

    $requiredSet = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    $requiredNames = [Collections.Generic.List[string]]::new()
    foreach ($rawMetric in $RequiredMetrics) {
        $metric = Get-KettlePerfBaselineIdentifier `
            -Value $rawMetric -FieldName 'RequiredMetrics' -MaximumLength 128
        if (-not $requiredSet.Add($metric)) {
            throw "RequiredMetrics contains duplicate metric '$metric'"
        }
        $requiredNames.Add($metric)
    }

    $resultMap = [Collections.Generic.Dictionary[string, object]]::new(
        [StringComparer]::Ordinal
    )
    foreach ($result in $MetricResults) {
        if ($null -eq $result) {
            throw 'MetricResults cannot contain null'
        }
        $propertyNames = [string[]]@(
            $result.PSObject.Properties |
                ForEach-Object { $_.Name }
        )
        foreach ($propertyName in @(
            'schema_version',
            'algorithm',
            'metric',
            'interval',
            'current_drift',
            'baseline_drift',
            'interval_classification',
            'classification',
            'non_inferior',
            'passed'
        )) {
            if (-not ($propertyNames -ccontains $propertyName)) {
                throw "baseline metric result lacks '$propertyName'"
            }
        }
        if (
            [int]$result.schema_version -ne 1 -or
            [string]$result.algorithm -cne
                'paired-baseline-noninferiority-v1'
        ) {
            throw 'MetricResults contains a foreign baseline result'
        }
        $metric = Get-KettlePerfBaselineIdentifier `
            -Value $result.metric -FieldName 'result metric' `
            -MaximumLength 128
        if ($resultMap.ContainsKey($metric)) {
            throw "MetricResults contains duplicate metric '$metric'"
        }

        $lower = $result.interval.lower
        $upper = $result.interval.upper
        if (
            -not (Test-KettlePerfBaselineNumericType -Value $lower) -or
            -not (Test-KettlePerfBaselineNumericType -Value $upper) -or
            [double]::IsNaN([double]$lower) -or
            [double]::IsInfinity([double]$lower) -or
            [double]::IsNaN([double]$upper) -or
            [double]::IsInfinity([double]$upper) -or
            [double]$lower -gt [double]$upper
        ) {
            throw "metric '$metric' has an invalid confidence interval"
        }
        foreach ($booleanProperty in @(
            $result.current_drift.passed,
            $result.baseline_drift.passed,
            $result.non_inferior,
            $result.passed
        )) {
            if ($booleanProperty -isnot [bool]) {
                throw "metric '$metric' contains a non-Boolean gate"
            }
        }
        $intervalClassification = if ([double]$lower -gt 0.0) {
            'pass'
        } elseif ([double]$upper -lt 0.0) {
            'fail'
        } else {
            'uncertain'
        }
        $driftPassed = (
            [bool]$result.current_drift.passed -and
            [bool]$result.baseline_drift.passed
        )
        $classification = if ($driftPassed) {
            $intervalClassification
        } else {
            'fail'
        }
        $expectedPass = $classification -ceq 'pass'
        if (
            [string]$result.interval_classification -cne
                $intervalClassification -or
            [string]$result.classification -cne $classification -or
            [bool]$result.non_inferior -ne $expectedPass -or
            [bool]$result.passed -ne $expectedPass
        ) {
            throw "metric '$metric' contains inconsistent gate evidence"
        }
        $resultMap.Add($metric, $result)
    }

    $evaluations = [Collections.Generic.List[object]]::new()
    $missing = [Collections.Generic.List[string]]::new()
    $failed = [Collections.Generic.List[string]]::new()
    $uncertain = [Collections.Generic.List[string]]::new()
    foreach ($metric in $requiredNames) {
        if (-not $resultMap.ContainsKey($metric)) {
            $missing.Add($metric)
            $evaluations.Add(
                [pscustomobject][ordered]@{
                    metric = $metric
                    present = $false
                    classification = 'missing'
                    non_inferior = $false
                }
            )
            continue
        }
        $result = $resultMap[$metric]
        if ([string]$result.classification -ceq 'fail') {
            $failed.Add($metric)
        } elseif ([string]$result.classification -ceq 'uncertain') {
            $uncertain.Add($metric)
        }
        $evaluations.Add(
            [pscustomobject][ordered]@{
                metric = $metric
                present = $true
                classification = [string]$result.classification
                non_inferior = [bool]$result.non_inferior
            }
        )
    }

    $passed = (
        $missing.Count -eq 0 -and
        $failed.Count -eq 0 -and
        $uncertain.Count -eq 0
    )
    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'all-required-baselines-noninferior-v1'
        required_metrics = [string[]]$requiredNames.ToArray()
        provided_metric_count = $resultMap.Count
        required_metric_count = $requiredNames.Count
        evaluations = [object[]]$evaluations.ToArray()
        missing_metrics = [string[]]$missing.ToArray()
        failed_metrics = [string[]]$failed.ToArray()
        uncertain_metrics = [string[]]$uncertain.ToArray()
        uncertainty_is_pass = $false
        all_required_metrics_non_inferior = $passed
        passed = $passed
    }
}
