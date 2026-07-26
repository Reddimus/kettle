# Strict release statistics over raw, paired terminal observations.
#
# The caller must only name peers whose isolated configuration evidence has
# already been verified. This layer deliberately accepts no aggregate values:
# every comparison, drift diagnostic, and throughput round gate is derived from
# the raw observations supplied here.

. "$PSScriptRoot\statistics.ps1"

function Test-KettlePerfReleaseNumericType {
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

function Get-KettlePerfReleaseIdentifier {
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

function Get-KettlePerfReleaseSequence {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    if (-not (Test-KettlePerfReleaseNumericType -Value $Value)) {
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

function Get-KettlePerfReleaseValue {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    if (-not (Test-KettlePerfReleaseNumericType -Value $Value)) {
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

function ConvertTo-KettlePerfReleaseDataSet {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The raw contract contains multiple observations.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations,
        [string]$CandidateTerminal = 'kettle',
        [Parameter(Mandatory = $true)]
        [string[]]$IsolatedPeers
    )

    $candidate = Get-KettlePerfReleaseIdentifier `
        -Value $CandidateTerminal -FieldName 'CandidateTerminal' `
        -MaximumLength 128
    if ($IsolatedPeers.Count -lt 3 -or $IsolatedPeers.Count -gt 19) {
        throw 'IsolatedPeers must contain between 3 and 19 terminals'
    }

    $requestedNames = [Collections.Generic.List[string]]::new()
    $requestedNames.Add($candidate)
    $nameSet = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    if (-not $nameSet.Add($candidate)) {
        throw 'CandidateTerminal is duplicated'
    }
    foreach ($rawPeer in $IsolatedPeers) {
        $peer = Get-KettlePerfReleaseIdentifier `
            -Value $rawPeer -FieldName 'IsolatedPeers' -MaximumLength 128
        if (-not $nameSet.Add($peer)) {
            throw "terminal name '$peer' is duplicated"
        }
        $requestedNames.Add($peer)
    }
    if ($requestedNames.Count -lt 4 -or $requestedNames.Count -gt 20) {
        throw 'release statistics require between 4 and 20 terminals'
    }

    $byTerminal = [Collections.Generic.Dictionary[
        string,
        Collections.Generic.List[object]
    ]]::new([StringComparer]::Ordinal)
    foreach ($name in $requestedNames) {
        $byTerminal[$name] = [Collections.Generic.List[object]]::new()
    }

    $expectedProperties = [string[]]@(
        'terminal',
        'cluster_id',
        'sequence',
        'value',
        'status'
    )
    foreach ($observation in $Observations) {
        if ($null -eq $observation) {
            throw 'observation cannot be null'
        }
        $actualProperties = [string[]]@(
            $observation.PSObject.Properties |
                ForEach-Object { $_.Name }
        )
        if ($actualProperties.Count -ne $expectedProperties.Count) {
            throw (
                'observation must contain exactly terminal, cluster_id, ' +
                'sequence, value, and status'
            )
        }
        foreach ($propertyName in $expectedProperties) {
            if (-not ($actualProperties -ccontains $propertyName)) {
                throw "observation is missing exact property '$propertyName'"
            }
        }

        $terminal = Get-KettlePerfReleaseIdentifier `
            -Value $observation.PSObject.Properties['terminal'].Value `
            -FieldName 'terminal' -MaximumLength 128
        if (-not $byTerminal.ContainsKey($terminal)) {
            throw "observation terminal '$terminal' was not requested"
        }
        $clusterId = Get-KettlePerfReleaseIdentifier `
            -Value $observation.PSObject.Properties['cluster_id'].Value `
            -FieldName 'cluster_id' -MaximumLength 256
        $sequence = Get-KettlePerfReleaseSequence `
            -Value $observation.PSObject.Properties['sequence'].Value
        $value = Get-KettlePerfReleaseValue `
            -Value $observation.PSObject.Properties['value'].Value
        $statusValue = $observation.PSObject.Properties['status'].Value
        if ($statusValue -isnot [string] -or [string]$statusValue -cne 'ok') {
            throw 'release observations must have status exactly ok'
        }

        $byTerminal[$terminal].Add(
            [pscustomobject][ordered]@{
                terminal = $terminal
                cluster_id = $clusterId
                sequence = $sequence
                value = $value
                status = 'ok'
            }
        )
    }

    $clusterMaps = [Collections.Generic.Dictionary[
        string,
        Collections.Generic.Dictionary[string, object]
    ]]::new([StringComparer]::Ordinal)
    foreach ($name in $requestedNames) {
        $terminalObservations = $byTerminal[$name]
        if (
            $terminalObservations.Count -lt 6 -or
            $terminalObservations.Count -gt 1000
        ) {
            throw (
                "terminal '$name' must contain between 6 and 1000 " +
                'observations'
            )
        }
        $clusterMap = [Collections.Generic.Dictionary[string, object]]::new(
            [StringComparer]::Ordinal
        )
        $sequenceSet = [Collections.Generic.HashSet[int64]]::new()
        foreach ($observation in $terminalObservations) {
            $clusterId = [string]$observation.cluster_id
            if ($clusterMap.ContainsKey($clusterId)) {
                throw (
                    "terminal '$name' contains duplicate cluster id " +
                    "'$($observation.cluster_id)'"
                )
            }
            $clusterMap.Add($clusterId, $observation)
            if (-not $sequenceSet.Add([int64]$observation.sequence)) {
                throw (
                    "terminal '$name' contains duplicate sequence " +
                    "'$($observation.sequence)'"
                )
            }
        }
        $clusterMaps[$name] = $clusterMap
    }

    $candidateClusters = $clusterMaps[$candidate]
    foreach ($peer in $requestedNames | Select-Object -Skip 1) {
        $peerClusters = $clusterMaps[$peer]
        if ($peerClusters.Count -ne $candidateClusters.Count) {
            throw (
                "candidate and peer '$peer' do not have the same matched " +
                'cluster count'
            )
        }
        foreach ($clusterId in $candidateClusters.Keys) {
            if (-not $peerClusters.ContainsKey($clusterId)) {
                throw (
                    "peer '$peer' is missing matched cluster '$clusterId'"
                )
            }
        }
    }

    return [pscustomobject][ordered]@{
        candidate = $candidate
        peers = [string[]]@($requestedNames | Select-Object -Skip 1)
        terminal_names = [string[]]$requestedNames.ToArray()
        by_terminal = $byTerminal
        cluster_maps = $clusterMaps
        observation_count = $Observations.Count
        observations_per_terminal = $candidateClusters.Count
    }
}

function Get-KettlePerfReleaseDriftFromDataSet {
    param(
        [Parameter(Mandatory = $true)]
        $DataSet
    )

    $terminalResults = [Collections.Generic.List[object]]::new()
    foreach ($terminal in $DataSet.terminal_names) {
        $ordered = [object[]]@(
            $DataSet.by_terminal[$terminal] |
                Sort-Object -Property sequence
        )
        $values = [double[]]@($ordered | ForEach-Object { $_.value })
        $sequences = [int64[]]@($ordered | ForEach-Object { $_.sequence })
        $theilSen = Get-KettlePerfTheilSenDrift `
            -Values $values -XValues $sequences `
            -MaxAbsoluteDriftPct 10.0 -ZeroFloor 0.000001
        $median = [double](Get-KettlePerfMedian -Values $values)
        $measure = $values | Measure-Object -Minimum -Maximum
        $minimum = [double]$measure.Minimum
        $maximum = [double]$measure.Maximum
        $peakDenominator = [Math]::Max([Math]::Abs($median), 0.000001)
        $peakToPeakPct = (($maximum - $minimum) / $peakDenominator) * 100.0
        $peakPassed = $peakToPeakPct -le 20.0
        $passed = [bool]$theilSen.passed -and $peakPassed
        $terminalResults.Add(
            [pscustomobject][ordered]@{
                terminal = $terminal
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
                trend_passed = [bool]$theilSen.passed
                peak_to_peak_passed = $peakPassed
                passed = $passed
            }
        )
    }
    $failed = [object[]]@(
        $terminalResults | Where-Object { -not $_.passed }
    )
    return [pscustomobject][ordered]@{
        algorithm = 'theil-sen-and-normalized-range-v1'
        trend_limit_pct = 10.0
        peak_to_peak_limit_pct = 20.0
        zero_floor = 0.000001
        terminal_count = $terminalResults.Count
        failed_terminal_count = $failed.Count
        passed = $failed.Count -eq 0
        terminals = [object[]]$terminalResults.ToArray()
    }
}

function Get-KettlePerfReleaseDriftDiagnostic {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations,
        [string]$CandidateTerminal = 'kettle',
        [Parameter(Mandatory = $true)]
        [string[]]$IsolatedPeers
    )

    $dataSet = ConvertTo-KettlePerfReleaseDataSet `
        -Observations $Observations `
        -CandidateTerminal $CandidateTerminal `
        -IsolatedPeers $IsolatedPeers
    return Get-KettlePerfReleaseDriftFromDataSet -DataSet $dataSet
}

function Test-KettlePerfReleasePolicy {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Comparisons
    )

    if ($Comparisons.Count -lt 3 -or $Comparisons.Count -gt 19) {
        throw 'release policy requires between 3 and 19 peer comparisons'
    }
    $peers = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $wins = 0
    $losses = 0
    $uncertain = 0
    foreach ($comparison in $Comparisons) {
        if ($null -eq $comparison) {
            throw 'release policy comparison cannot be null'
        }
        $propertyNames = [string[]]@(
            $comparison.PSObject.Properties |
                ForEach-Object { $_.Name }
        )
        foreach ($propertyName in @('peer', 'classification')) {
            if (-not ($propertyNames -ccontains $propertyName)) {
                throw "release policy comparison lacks '$propertyName'"
            }
        }
        $peer = Get-KettlePerfReleaseIdentifier `
            -Value $comparison.peer -FieldName 'comparison peer' `
            -MaximumLength 128
        if (-not $peers.Add($peer)) {
            throw "release policy contains duplicate peer '$peer'"
        }
        switch -CaseSensitive ([string]$comparison.classification) {
            'confirmed-win' { $wins++; break }
            'confirmed-loss' { $losses++; break }
            'uncertain' { $uncertain++; break }
            default {
                throw (
                    "comparison for '$peer' has invalid classification " +
                    "'$($comparison.classification)'"
                )
            }
        }
    }
    return [pscustomobject][ordered]@{
        algorithm = 'confirmed-pair-count-v1'
        comparison_count = $Comparisons.Count
        required_confirmed_wins = 3
        maximum_confirmed_losses = 1
        confirmed_wins = $wins
        confirmed_losses = $losses
        uncertain = $uncertain
        uncertainty_counts_as_win = $false
        passed = $wins -ge 3 -and $losses -le 1
    }
}

function Get-KettlePerfReleaseComparison {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations,
        [string]$CandidateTerminal = 'kettle',
        [Parameter(Mandatory = $true)]
        [string[]]$IsolatedPeers,
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

    if (
        [string]::IsNullOrWhiteSpace($Seed) -or
        $Seed.Length -gt 4096
    ) {
        throw 'Seed must be a non-empty string of at most 4096 characters'
    }
    if ($Direction -cnotin @('higher', 'lower')) {
        throw 'Direction must use the exact value higher or lower'
    }
    if (
        [double]::IsNaN($AbsoluteMargin) -or
        [double]::IsInfinity($AbsoluteMargin) -or
        [double]::IsNaN($RelativeMargin) -or
        [double]::IsInfinity($RelativeMargin)
    ) {
        throw 'release margins must be finite'
    }
    $dataSet = ConvertTo-KettlePerfReleaseDataSet `
        -Observations $Observations `
        -CandidateTerminal $CandidateTerminal `
        -IsolatedPeers $IsolatedPeers
    $higherIsBetter = $Direction -ceq 'higher'
    $comparisons = [Collections.Generic.List[object]]::new()
    $candidateClusters = $dataSet.cluster_maps[$dataSet.candidate]
    $clusterIds = [string[]]@($candidateClusters.Keys)
    [Array]::Sort($clusterIds, [StringComparer]::Ordinal)

    foreach ($peer in $dataSet.peers) {
        $peerClusters = $dataSet.cluster_maps[$peer]
        $adjustedPairs = [Collections.Generic.List[object]]::new()
        $adjustedValues = [double[]]::new($clusterIds.Count)
        $zeroReference = [double[]]::new($clusterIds.Count)
        $positive = 0
        $zero = 0
        $negative = 0
        for ($index = 0; $index -lt $clusterIds.Count; $index++) {
            $clusterId = $clusterIds[$index]
            $candidateObservation = $candidateClusters[$clusterId]
            $peerObservation = $peerClusters[$clusterId]
            $candidateValue = [double]$candidateObservation.value
            $peerValue = [double]$peerObservation.value
            $margin = [Math]::Max(
                $AbsoluteMargin,
                $RelativeMargin * [Math]::Abs($peerValue)
            )
            $rawFavorable = if ($higherIsBetter) {
                $candidateValue - $peerValue
            } else {
                $peerValue - $candidateValue
            }
            $adjusted = $rawFavorable - $margin
            $adjustedValues[$index] = $adjusted
            if ($adjusted -gt 0.0) {
                $positive++
            } elseif ($adjusted -lt 0.0) {
                $negative++
            } else {
                $zero++
            }
            $adjustedPairs.Add(
                [pscustomobject][ordered]@{
                    cluster_id = $clusterId
                    candidate_sequence = [int64]$candidateObservation.sequence
                    peer_sequence = [int64]$peerObservation.sequence
                    candidate_value = $candidateValue
                    peer_value = $peerValue
                    raw_favorable_difference = $rawFavorable
                    practical_margin = $margin
                    favorable_difference_after_margin = $adjusted
                }
            )
        }

        $peerSeed = 'seed:{0}:{1}|peer:{2}:{3}' -f (
            $Seed.Length,
            $Seed,
            $peer.Length,
            $peer
        )
        $interval = Get-KettlePerfPairedClusterBootstrapInterval `
            -Candidate $adjustedValues -Reference $zeroReference `
            -ClusterIds $clusterIds -HigherIsBetter `
            -Iterations $BootstrapIterations -ConfidenceLevel 0.90 `
            -Seed $peerSeed -Statistic median
        $classification = if ([double]$interval.lower -gt 0.0) {
            'confirmed-win'
        } elseif ([double]$interval.upper -lt 0.0) {
            'confirmed-loss'
        } else {
            'uncertain'
        }
        $comparisons.Add(
            [pscustomobject][ordered]@{
                candidate = $dataSet.candidate
                peer = $peer
                classification = $classification
                direction = $Direction
                absolute_margin = $AbsoluteMargin
                relative_margin = $RelativeMargin
                interval = $interval
                counts = [pscustomobject][ordered]@{
                    candidate_observations = $clusterIds.Count
                    peer_observations = $clusterIds.Count
                    matched_pairs = $clusterIds.Count
                    favorable_after_margin = $positive
                    exactly_at_margin = $zero
                    unfavorable_after_margin = $negative
                }
                adjusted_pairs = [object[]]$adjustedPairs.ToArray()
            }
        )
    }

    $comparisonArray = [object[]]$comparisons.ToArray()
    $policy = Test-KettlePerfReleasePolicy -Comparisons $comparisonArray
    $drift = Get-KettlePerfReleaseDriftFromDataSet -DataSet $dataSet
    $terminalCounts = [Collections.Generic.List[object]]::new()
    foreach ($terminal in $dataSet.terminal_names) {
        $terminalCounts.Add(
            [pscustomobject][ordered]@{
                terminal = $terminal
                observations = $dataSet.by_terminal[$terminal].Count
            }
        )
    }
    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'paired-practical-cluster-bootstrap-v1'
        candidate = $dataSet.candidate
        isolated_peers = [string[]]$dataSet.peers
        isolation_basis = 'caller-verified'
        direction = $Direction
        absolute_margin = $AbsoluteMargin
        relative_margin = $RelativeMargin
        confidence_level = 0.90
        bootstrap_iterations = $BootstrapIterations
        statistic = 'median'
        seed_sha256 = Get-KettlePerfSha256Hex -Text $Seed
        terminal_count = $dataSet.terminal_names.Count
        observation_count = $dataSet.observation_count
        terminal_observation_counts = [object[]]$terminalCounts.ToArray()
        comparisons = $comparisonArray
        policy = $policy
        drift = $drift
        passed = [bool]$policy.passed -and [bool]$drift.passed
    }
}

function Get-KettlePerfThroughputRoundGate {
    param(
        [Parameter(Mandatory = $true)]
        $Comparison
    )

    $requiredProperties = @(
        'algorithm',
        'direction',
        'absolute_margin',
        'relative_margin',
        'comparisons'
    )
    $propertyNames = [string[]]@(
        $Comparison.PSObject.Properties |
            ForEach-Object { $_.Name }
    )
    foreach ($propertyName in $requiredProperties) {
        if (-not ($propertyNames -ccontains $propertyName)) {
            throw "throughput comparison lacks '$propertyName'"
        }
    }
    if (
        $Comparison.algorithm -cne
            'paired-practical-cluster-bootstrap-v1' -or
        $Comparison.direction -cne 'higher' -or
        [double]$Comparison.absolute_margin -ne 0.0 -or
        [double]$Comparison.relative_margin -ne 0.05
    ) {
        throw (
            'throughput round gate requires a higher-is-better comparison ' +
            'with exactly a five-percent relative margin'
        )
    }
    $comparisons = [object[]]@($Comparison.comparisons)
    if ($comparisons.Count -lt 3 -or $comparisons.Count -gt 19) {
        throw 'throughput round gate requires between 3 and 19 peers'
    }

    $peerResults = [Collections.Generic.List[object]]::new()
    $peerNames = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $failedPairs = 0
    foreach ($peerComparison in $comparisons) {
        $peer = Get-KettlePerfReleaseIdentifier `
            -Value $peerComparison.peer -FieldName 'throughput peer' `
            -MaximumLength 128
        if (-not $peerNames.Add($peer)) {
            throw "throughput round gate contains duplicate peer '$peer'"
        }
        $pairs = [object[]]@($peerComparison.adjusted_pairs)
        if (
            $pairs.Count -lt 6 -or
            $pairs.Count -gt 1000 -or
            $pairs.Count -ne [int]$peerComparison.counts.matched_pairs
        ) {
            throw "throughput peer '$peer' has invalid matched pair counts"
        }
        $clusterIds = [Collections.Generic.HashSet[string]]::new(
            [StringComparer]::Ordinal
        )
        $failedClusterIds = [Collections.Generic.List[string]]::new()
        foreach ($pair in $pairs) {
            $clusterId = Get-KettlePerfReleaseIdentifier `
                -Value $pair.cluster_id -FieldName 'throughput cluster_id' `
                -MaximumLength 256
            if (-not $clusterIds.Add($clusterId)) {
                throw (
                    "throughput peer '$peer' contains duplicate cluster " +
                    "'$clusterId'"
                )
            }
            $adjusted = $pair.favorable_difference_after_margin
            if (
                -not (Test-KettlePerfReleaseNumericType -Value $adjusted) -or
                [double]::IsNaN([double]$adjusted) -or
                [double]::IsInfinity([double]$adjusted)
            ) {
                throw "throughput peer '$peer' has a non-finite adjusted pair"
            }
            if ([double]$adjusted -le 0.0) {
                $failedClusterIds.Add($clusterId)
            }
        }
        $failedPairs += $failedClusterIds.Count
        $peerResults.Add(
            [pscustomobject][ordered]@{
                peer = $peer
                matched_round_composites = $pairs.Count
                non_positive_round_composites = $failedClusterIds.Count
                failed_cluster_ids = [string[]]$failedClusterIds.ToArray()
                passed = $failedClusterIds.Count -eq 0
            }
        )
    }
    return [pscustomobject][ordered]@{
        algorithm = 'all-matched-rounds-positive-after-five-percent-v1'
        relative_margin = 0.05
        strict_positive_required = $true
        peer_count = $peerResults.Count
        failed_round_composites = $failedPairs
        peers = [object[]]$peerResults.ToArray()
        passed = $failedPairs -eq 0
    }
}

function Get-KettlePerfThroughputReleaseComparison {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Observations,
        [string]$CandidateTerminal = 'kettle',
        [Parameter(Mandatory = $true)]
        [string[]]$IsolatedPeers,
        [ValidateRange(1000, 100000)]
        [int]$BootstrapIterations = 10000,
        [Parameter(Mandatory = $true)]
        [string]$Seed
    )

    $comparisonSet = Get-KettlePerfReleaseComparison `
        -Observations $Observations `
        -CandidateTerminal $CandidateTerminal `
        -IsolatedPeers $IsolatedPeers `
        -Direction higher -AbsoluteMargin 0.0 -RelativeMargin 0.05 `
        -BootstrapIterations $BootstrapIterations -Seed $Seed
    $roundGate = Get-KettlePerfThroughputRoundGate `
        -Comparison $comparisonSet
    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'throughput-release-gate-v1'
        comparison = $comparisonSet
        round_gate = $roundGate
        passed = [bool]$comparisonSet.passed -and [bool]$roundGate.passed
    }
}
