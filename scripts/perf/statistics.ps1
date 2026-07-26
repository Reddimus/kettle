# Shared deterministic statistics for performance probes and their verifier.
#
# Do not use System.Random here. Its implementation is not a cross-runtime
# contract, while this harness must reproduce evidence under Windows PowerShell
# 5.1 and PowerShell 7. The pinned generator below is SHA-256 counter mode.

function Initialize-KettlePerfStatisticsKernel {
    if ('KettlePerf.StatisticsKernel' -as [type]) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Security.Cryptography;
using System.Text;

namespace KettlePerf
{
    public static class StatisticsKernel
    {
        private sealed class Sha256CounterRandom : IDisposable
        {
            private readonly byte[] seedDigest;
            private readonly SHA256 sha256;
            private ulong counter;
            private byte[] buffer;
            private int offset;

            public Sha256CounterRandom(string seed)
            {
                if (seed == null)
                    throw new ArgumentNullException("seed");
                sha256 = SHA256.Create();
                seedDigest = sha256.ComputeHash(
                    new UTF8Encoding(false, true).GetBytes(seed));
                buffer = new byte[0];
                offset = 0;
                counter = 0;
            }

            private uint NextUInt32()
            {
                if (offset < 0 || offset + 4 > buffer.Length)
                {
                    if (counter == UInt64.MaxValue)
                        throw new InvalidOperationException(
                            "Pinned random counter is exhausted");
                    byte[] input = new byte[40];
                    Buffer.BlockCopy(seedDigest, 0, input, 0, 32);
                    ulong value = counter;
                    for (int index = 0; index < 8; index++)
                        input[32 + index] =
                            (byte)((value >> (8 * index)) & 0xffUL);
                    buffer = sha256.ComputeHash(input);
                    counter++;
                    offset = 0;
                }
                uint result =
                    (uint)buffer[offset] |
                    ((uint)buffer[offset + 1] << 8) |
                    ((uint)buffer[offset + 2] << 16) |
                    ((uint)buffer[offset + 3] << 24);
                offset += 4;
                return result;
            }

            public int NextInt(int exclusiveMaximum)
            {
                if (exclusiveMaximum < 1)
                    throw new ArgumentOutOfRangeException(
                        "exclusiveMaximum");
                const ulong range = 4294967296UL;
                ulong maximum = (ulong)exclusiveMaximum;
                ulong limit = (range / maximum) * maximum;
                ulong value;
                do
                {
                    value = (ulong)NextUInt32();
                }
                while (value >= limit);
                return (int)(value % maximum);
            }

            public void Dispose()
            {
                sha256.Dispose();
            }
        }

        private static double Median(double[] values)
        {
            if (values == null || values.Length == 0)
                throw new ArgumentException(
                    "Median requires at least one value", "values");
            Array.Sort(values);
            int middle = values.Length / 2;
            if ((values.Length & 1) == 1)
                return values[middle];
            return (values[middle - 1] + values[middle]) / 2.0;
        }

        public static double[] ClusteredBootstrap(
            double[][] clusters,
            int iterations,
            string seed,
            bool useMedian)
        {
            if (clusters == null || clusters.Length == 0)
                throw new ArgumentException(
                    "At least one cluster is required", "clusters");
            if (iterations < 1)
                throw new ArgumentOutOfRangeException("iterations");
            int maximumSampleCount = 0;
            for (int index = 0; index < clusters.Length; index++)
            {
                if (clusters[index] == null || clusters[index].Length == 0)
                    throw new ArgumentException(
                        "Clusters cannot be empty", "clusters");
                maximumSampleCount = checked(
                    maximumSampleCount + clusters[index].Length);
            }

            double[] replicates = new double[iterations];
            using (Sha256CounterRandom random =
                new Sha256CounterRandom(seed))
            {
                for (int iteration = 0;
                    iteration < iterations;
                    iteration++)
                {
                    List<double> sample =
                        new List<double>(maximumSampleCount);
                    for (int draw = 0; draw < clusters.Length; draw++)
                    {
                        double[] selected =
                            clusters[random.NextInt(clusters.Length)];
                        sample.AddRange(selected);
                    }
                    double[] values = sample.ToArray();
                    if (useMedian)
                    {
                        replicates[iteration] = Median(values);
                    }
                    else
                    {
                        double sum = 0.0;
                        for (int index = 0;
                            index < values.Length;
                            index++)
                            sum += values[index];
                        replicates[iteration] =
                            sum / (double)values.Length;
                    }
                }
            }
            return replicates;
        }
    }
}
'@
}

function ConvertTo-KettlePerfFiniteDoubles {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function converts a collection of measurements.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Values,
        [string]$Name = 'Values'
    )

    $converted = [Collections.Generic.List[double]]::new()
    foreach ($value in $Values) {
        try {
            $number = [double]$value
        } catch {
            throw "$Name contains a non-numeric value"
        }
        if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
            throw "$Name contains a non-finite value"
        }
        $converted.Add($number)
    }
    return $converted.ToArray()
}

function Get-KettlePerfMedian {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Values
    )

    if ($Values.Count -eq 0) {
        return $null
    }
    $sorted = [double[]](
        ConvertTo-KettlePerfFiniteDoubles -Values $Values
    )
    [Array]::Sort($sorted)
    $middle = [int][Math]::Floor($sorted.Count / 2)
    if (($sorted.Count % 2) -eq 1) {
        return [double]$sorted[$middle]
    }
    return (
        [double]$sorted[$middle - 1] +
        [double]$sorted[$middle]
    ) / 2.0
}

function Get-KettlePerfNearestRankPercentile {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Values,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0.0, 1.0)]
        [double]$Percentile
    )

    if ($Values.Count -eq 0) {
        return $null
    }
    $sorted = [double[]](
        ConvertTo-KettlePerfFiniteDoubles -Values $Values
    )
    [Array]::Sort($sorted)
    $rank = if ($Percentile -le 0.0) {
        1
    } else {
        [int][Math]::Ceiling($Percentile * $sorted.Count)
    }
    return [double]$sorted[[Math]::Min($sorted.Count, $rank) - 1]
}

function Get-KettlePerfSha256Hex {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Text
    )

    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
        return -join @(
            $sha256.ComputeHash($bytes) |
                ForEach-Object { $_.ToString('x2') }
        )
    } finally {
        $sha256.Dispose()
    }
}

function New-KettlePerfPinnedRandom {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This only constructs an in-memory PRNG state object.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Seed
    )

    $seedBytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($Seed)
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $seedDigest = $sha256.ComputeHash($seedBytes)
    } finally {
        $sha256.Dispose()
    }
    return [pscustomobject][ordered]@{
        algorithm = 'sha256-counter-le-v1'
        seed_sha256 = -join @(
            $seedDigest | ForEach-Object { $_.ToString('x2') }
        )
        seed_digest = [byte[]]$seedDigest
        counter = [uint64]0
        buffer = [byte[]]@()
        offset = 0
    }
}

function Get-KettlePerfPinnedRandomUInt32 {
    param(
        [Parameter(Mandatory = $true)]
        $Random
    )

    if (
        $Random.algorithm -ne 'sha256-counter-le-v1' -or
        @($Random.seed_digest).Count -ne 32
    ) {
        throw 'Pinned random state is invalid'
    }
    if (
        $null -eq $Random.buffer -or
        [int]$Random.offset -lt 0 -or
        [int]$Random.offset + 4 -gt @($Random.buffer).Count
    ) {
        if ([uint64]$Random.counter -eq [uint64]::MaxValue) {
            throw 'Pinned random counter is exhausted'
        }
        $counterBytes = [byte[]]::new(8)
        $counter = [uint64]$Random.counter
        for ($index = 0; $index -lt 8; $index++) {
            $counterBytes[$index] = [byte](
                ($counter -shr (8 * $index)) -band 0xff
            )
        }
        $inputBytes = [byte[]]::new(40)
        [Array]::Copy(
            [byte[]]$Random.seed_digest,
            0,
            $inputBytes,
            0,
            32
        )
        [Array]::Copy($counterBytes, 0, $inputBytes, 32, 8)
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try {
            $Random.buffer = [byte[]]$sha256.ComputeHash($inputBytes)
        } finally {
            $sha256.Dispose()
        }
        $Random.counter = [uint64]$Random.counter + [uint64]1
        $Random.offset = 0
    }

    $offset = [int]$Random.offset
    $buffer = [byte[]]$Random.buffer
    $value = (
        [uint64]$buffer[$offset] +
        ([uint64]$buffer[$offset + 1] * [uint64]256) +
        ([uint64]$buffer[$offset + 2] * [uint64]65536) +
        ([uint64]$buffer[$offset + 3] * [uint64]16777216)
    )
    $Random.offset = $offset + 4
    return [uint64]$value
}

function Get-KettlePerfPinnedRandomInt {
    param(
        [Parameter(Mandatory = $true)]
        $Random,
        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 2147483647)]
        [int]$ExclusiveMaximum
    )

    # Rejection sampling avoids modulo bias for bounds that do not divide 2^32.
    $range = [uint64]4294967296
    $limit = [uint64](
        [Math]::Floor(
            [double]$range / [double]$ExclusiveMaximum
        ) * [double]$ExclusiveMaximum
    )
    do {
        $value = Get-KettlePerfPinnedRandomUInt32 -Random $Random
    } while ($value -ge $limit)
    return [int]($value % [uint64]$ExclusiveMaximum)
}

function Get-KettlePerfPairClassification {
    param(
        [Parameter(Mandatory = $true)]
        [double]$Residual,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$Deadband,
        [double]$IntervalLower = [double]::NaN,
        [double]$IntervalUpper = [double]::NaN
    )

    foreach ($number in @($Residual, $Deadband)) {
        if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
            throw 'Pair classification requires finite residual and deadband'
        }
    }
    $hasLower = -not [double]::IsNaN($IntervalLower)
    $hasUpper = -not [double]::IsNaN($IntervalUpper)
    if ($hasLower -ne $hasUpper) {
        throw 'Pair classification requires both interval bounds or neither'
    }
    if ($hasLower) {
        if (
            [double]::IsInfinity($IntervalLower) -or
            [double]::IsInfinity($IntervalUpper) -or
            $IntervalLower -gt $IntervalUpper
        ) {
            throw 'Pair classification interval is invalid'
        }
        $classification = if ($IntervalLower -gt $Deadband) {
            'win'
        } elseif ($IntervalUpper -lt -$Deadband) {
            'loss'
        } elseif (
            $IntervalLower -ge -$Deadband -and
            $IntervalUpper -le $Deadband
        ) {
            'equivalent'
        } else {
            'inconclusive'
        }
    } else {
        $classification = if ($Residual -gt $Deadband) {
            'win'
        } elseif ($Residual -lt -$Deadband) {
            'loss'
        } else {
            'equivalent'
        }
    }
    return [pscustomobject][ordered]@{
        classification = $classification
        residual = $Residual
        deadband = $Deadband
        interval_lower = if ($hasLower) { $IntervalLower } else { $null }
        interval_upper = if ($hasUpper) { $IntervalUpper } else { $null }
    }
}

function Get-KettlePerfPracticalComparison {
    param(
        [Parameter(Mandatory = $true)]
        [double]$Candidate,
        [Parameter(Mandatory = $true)]
        [double]$Reference,
        [switch]$HigherIsBetter,
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$AbsoluteMargin = 0.0,
        [ValidateRange(0.0, 1.0)]
        [double]$RelativeMargin = 0.0,
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$ZeroFloor = 0.0
    )

    foreach ($number in @($Candidate, $Reference)) {
        if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
            throw 'Practical comparison requires finite measurements'
        }
    }
    $midpoint = (
        [Math]::Abs($Candidate) +
        [Math]::Abs($Reference)
    ) / 2.0
    $scale = [Math]::Max($midpoint, $ZeroFloor)
    $deadband = [Math]::Max(
        $AbsoluteMargin,
        $RelativeMargin * $scale
    )
    $residual = if ($HigherIsBetter) {
        $Candidate - $Reference
    } else {
        $Reference - $Candidate
    }
    $classified = Get-KettlePerfPairClassification `
        -Residual $residual -Deadband $deadband
    $practicalResidual = if ([Math]::Abs($residual) -le $deadband) {
        0.0
    } elseif ($residual -gt 0.0) {
        $residual - $deadband
    } else {
        $residual + $deadband
    }
    return [pscustomobject][ordered]@{
        classification = $classified.classification
        candidate = $Candidate
        reference = $Reference
        higher_is_better = [bool]$HigherIsBetter
        residual = $residual
        practical_residual = $practicalResidual
        deadband = $deadband
        absolute_margin = $AbsoluteMargin
        relative_margin = $RelativeMargin
        zero_floor = $ZeroFloor
    }
}

function Get-KettlePerfIdleCpuComparison {
    param(
        [Parameter(Mandatory = $true)]
        [double]$Candidate,
        [Parameter(Mandatory = $true)]
        [double]$Reference,
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$AbsoluteMargin = 0.10,
        [ValidateRange(0.0, 1.0)]
        [double]$RelativeMargin = 0.0,
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$ZeroFloor = 0.10
    )

    return Get-KettlePerfPracticalComparison `
        -Candidate $Candidate -Reference $Reference `
        -AbsoluteMargin $AbsoluteMargin `
        -RelativeMargin $RelativeMargin -ZeroFloor $ZeroFloor
}

function Get-KettlePerfClusteredBootstrapInterval {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Values,
        [Parameter(Mandatory = $true)]
        [object[]]$ClusterIds,
        [ValidateRange(100, 1000000)]
        [int]$Iterations = 5000,
        [ValidateRange(0.50, 0.9999)]
        [double]$ConfidenceLevel = 0.95,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Seed,
        [ValidateSet('median', 'mean')]
        [string]$Statistic = 'median'
    )

    if ($Values.Count -eq 0 -or $Values.Count -ne $ClusterIds.Count) {
        throw 'Bootstrap values and cluster ids must have the same non-zero count'
    }
    $numbers = [double[]]@(
        ConvertTo-KettlePerfFiniteDoubles -Values $Values
    )
    $clusters = [Collections.Generic.Dictionary[
        string,
        Collections.Generic.List[double]
    ]]::new([StringComparer]::Ordinal)
    for ($index = 0; $index -lt $numbers.Count; $index++) {
        if ($null -eq $ClusterIds[$index]) {
            throw 'Bootstrap cluster ids cannot be null'
        }
        $rawClusterId = $ClusterIds[$index]
        $clusterId = if ($rawClusterId -is [IFormattable]) {
            $rawClusterId.ToString(
                $null,
                [Globalization.CultureInfo]::InvariantCulture
            )
        } else {
            [string]$rawClusterId
        }
        if (-not $clusterId) {
            throw 'Bootstrap cluster ids cannot be empty'
        }
        if (-not $clusters.ContainsKey($clusterId)) {
            $clusters[$clusterId] = [Collections.Generic.List[double]]::new()
        }
        $clusters[$clusterId].Add([double]$numbers[$index])
    }
    $clusterKeys = [string[]]@($clusters.Keys)
    [Array]::Sort($clusterKeys, [StringComparer]::Ordinal)
    $random = New-KettlePerfPinnedRandom -Seed $Seed
    Initialize-KettlePerfStatisticsKernel
    $clusterValues = [double[][]]::new($clusterKeys.Count)
    for ($index = 0; $index -lt $clusterKeys.Count; $index++) {
        $clusterValues[$index] = [double[]](
            $clusters[$clusterKeys[$index]].ToArray()
        )
    }
    $replicates = [KettlePerf.StatisticsKernel]::ClusteredBootstrap(
        $clusterValues,
        $Iterations,
        $Seed,
        $Statistic -eq 'median'
    )
    $pointEstimate = if ($Statistic -eq 'median') {
        Get-KettlePerfMedian -Values $numbers
    } else {
        ($numbers | Measure-Object -Average).Average
    }
    $tail = (1.0 - $ConfidenceLevel) / 2.0
    return [pscustomobject][ordered]@{
        algorithm = 'paired-cluster-percentile-sha256-v1'
        statistic = $Statistic
        point_estimate = [double]$pointEstimate
        lower = Get-KettlePerfNearestRankPercentile `
            -Values $replicates -Percentile $tail
        upper = Get-KettlePerfNearestRankPercentile `
            -Values $replicates -Percentile (1.0 - $tail)
        confidence_level = $ConfidenceLevel
        iterations = $Iterations
        observation_count = $numbers.Count
        cluster_count = $clusterKeys.Count
        seed_sha256 = $random.seed_sha256
    }
}

function Get-KettlePerfPairedClusterBootstrapInterval {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Candidate,
        [Parameter(Mandatory = $true)]
        [object[]]$Reference,
        [Parameter(Mandatory = $true)]
        [object[]]$ClusterIds,
        [switch]$HigherIsBetter,
        [ValidateRange(100, 1000000)]
        [int]$Iterations = 5000,
        [ValidateRange(0.50, 0.9999)]
        [double]$ConfidenceLevel = 0.95,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Seed,
        [ValidateSet('median', 'mean')]
        [string]$Statistic = 'median'
    )

    if (
        $Candidate.Count -eq 0 -or
        $Candidate.Count -ne $Reference.Count -or
        $Candidate.Count -ne $ClusterIds.Count
    ) {
        throw 'Paired bootstrap arrays must have the same non-zero count'
    }
    $candidateValues = [double[]]@(
        ConvertTo-KettlePerfFiniteDoubles `
            -Values $Candidate -Name 'Candidate'
    )
    $referenceValues = [double[]]@(
        ConvertTo-KettlePerfFiniteDoubles `
            -Values $Reference -Name 'Reference'
    )
    $residuals = [double[]]::new($candidateValues.Count)
    for ($index = 0; $index -lt $candidateValues.Count; $index++) {
        $residuals[$index] = if ($HigherIsBetter) {
            $candidateValues[$index] - $referenceValues[$index]
        } else {
            $referenceValues[$index] - $candidateValues[$index]
        }
    }
    $result = Get-KettlePerfClusteredBootstrapInterval `
        -Values $residuals -ClusterIds $ClusterIds `
        -Iterations $Iterations -ConfidenceLevel $ConfidenceLevel `
        -Seed $Seed -Statistic $Statistic
    $result | Add-Member -NotePropertyName higher_is_better `
        -NotePropertyValue ([bool]$HigherIsBetter)
    return $result
}

function Get-KettlePerfTheilSenDrift {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Values,
        [object[]]$XValues = @(),
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$MaxAbsoluteDriftPct = 10.0,
        [ValidateRange(0.0, [double]::MaxValue)]
        [double]$ZeroFloor = 0.000001
    )

    if ($Values.Count -lt 2) {
        throw 'Theil-Sen drift requires at least two observations'
    }
    if ($Values.Count -gt 2000) {
        throw 'Exact Theil-Sen drift is capped at 2000 observations'
    }
    $y = [double[]]@(
        ConvertTo-KettlePerfFiniteDoubles -Values $Values
    )
    if ($XValues.Count -eq 0) {
        $x = [double[]]::new($y.Count)
        for ($index = 0; $index -lt $x.Count; $index++) {
            $x[$index] = $index
        }
    } elseif ($XValues.Count -eq $Values.Count) {
        $x = [double[]]@(
            ConvertTo-KettlePerfFiniteDoubles `
                -Values $XValues -Name 'XValues'
        )
    } else {
        throw 'Theil-Sen x values must be empty or match the observation count'
    }
    for ($index = 1; $index -lt $x.Count; $index++) {
        if ($x[$index] -le $x[$index - 1]) {
            throw 'Theil-Sen x values must be strictly increasing'
        }
    }

    $pairCount = [int64](
        ([int64]$y.Count * ($y.Count - 1)) / 2
    )
    $slopes = [Collections.Generic.List[double]]::new(
        [int]$pairCount
    )
    for ($left = 0; $left -lt $y.Count - 1; $left++) {
        for ($right = $left + 1; $right -lt $y.Count; $right++) {
            $slopes.Add(
                ($y[$right] - $y[$left]) /
                ($x[$right] - $x[$left])
            )
        }
    }
    $slope = Get-KettlePerfMedian -Values $slopes.ToArray()
    $intercepts = [double[]]::new($y.Count)
    for ($index = 0; $index -lt $y.Count; $index++) {
        $intercepts[$index] = $y[$index] - ($slope * $x[$index])
    }
    $intercept = Get-KettlePerfMedian -Values $intercepts
    $fittedStart = $intercept + ($slope * $x[0])
    $fittedEnd = $intercept + ($slope * $x[$x.Count - 1])
    $denominator = [Math]::Max([Math]::Abs($fittedStart), $ZeroFloor)
    $driftPct = (($fittedEnd - $fittedStart) / $denominator) * 100.0
    return [pscustomobject][ordered]@{
        algorithm = 'theil-sen-exact-v1'
        observations = $y.Count
        pair_count = $pairCount
        slope = $slope
        intercept = $intercept
        fitted_start = $fittedStart
        fitted_end = $fittedEnd
        drift_pct = $driftPct
        absolute_drift_pct = [Math]::Abs($driftPct)
        max_absolute_drift_pct = $MaxAbsoluteDriftPct
        passed = [Math]::Abs($driftPct) -le $MaxAbsoluteDriftPct
    }
}
