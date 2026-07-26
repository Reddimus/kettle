# Convert raw benchmark records into the exact paired-observation contracts
# consumed by release-statistics.ps1 and baseline-statistics.ps1.
#
# This file deliberately does not read result files. The score loader owns file
# and provenance validation; these helpers only validate and normalize the raw
# observations retained in those results.

function Get-KettlePerfScoreSourceProperty {
    param(
        [Parameter(Mandatory = $true)]
        $InputObject,
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    if ($null -eq $InputObject) {
        throw "raw observation is null while reading '$Name'"
    }
    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) {
        throw "raw observation lacks '$Name'"
    }
    return $property.Value
}

function New-KettlePerfScoreObservation {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [Parameter(Mandatory = $true)]
        [string]$ClusterId,
        [Parameter(Mandatory = $true)]
        [int64]$Sequence,
        [Parameter(Mandatory = $true)]
        [double]$Value
    )

    return [pscustomobject][ordered]@{
        terminal = $Terminal
        cluster_id = $ClusterId
        sequence = $Sequence
        value = $Value
        status = 'ok'
    }
}

function Assert-KettlePerfScoreSourceTerminal {
    param(
        [Parameter(Mandatory = $true)]
        $Observation,
        [Parameter(Mandatory = $true)]
        [string]$Terminal
    )

    $sourceTerminal = Get-KettlePerfReleaseIdentifier `
        -Value (Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'terminal') `
        -FieldName 'raw terminal' -MaximumLength 128
    if ($sourceTerminal -cne $Terminal) {
        throw "raw observation terminal '$sourceTerminal' differs from '$Terminal'"
    }
}

function Assert-KettlePerfScoreIdleAccounting {
    param(
        [Parameter(Mandatory = $true)]
        $Observation,
        [Parameter(Mandatory = $true)]
        [double]$ReportedIdleCpu,
        [Parameter(Mandatory = $true)]
        [double]$ReportedFreshWs
    )

    $workloadPid = Get-KettlePerfReleaseSequence `
        -Value (Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'workload_pid')
    $excluded = [int64[]]@(
        Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'excluded_pids' |
            ForEach-Object { Get-KettlePerfReleaseSequence -Value $_ }
    )
    if ($excluded -notcontains $workloadPid) {
        throw 'idle accounting did not exclude its controlled workload'
    }
    $before = [object[]]@(
        Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'included_processes_before'
    )
    $after = [object[]]@(
        Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'included_processes_after'
    )
    if ($before.Count -eq 0 -or $before.Count -ne $after.Count) {
        throw 'idle accounting process-tree cardinality changed'
    }
    $afterByPid = @{}
    foreach ($sample in $after) {
        $pidValue = Get-KettlePerfReleaseSequence `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $sample -Name 'pid')
        if ($afterByPid.ContainsKey($pidValue)) {
            throw 'idle accounting contains a duplicate after PID'
        }
        $afterByPid[$pidValue] = $sample
    }
    $calculatedDelta = 0.0
    $calculatedWorkingSetBytes = 0.0
    foreach ($beforeSample in $before) {
        $pidValue = Get-KettlePerfReleaseSequence `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $beforeSample -Name 'pid')
        if (-not $afterByPid.ContainsKey($pidValue)) {
            throw 'idle accounting process tree changed'
        }
        $afterSample = $afterByPid[$pidValue]
        $beforeStart = Get-KettlePerfReleaseSequence `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $beforeSample -Name 'start_time_utc_ticks')
        $afterStart = Get-KettlePerfReleaseSequence `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $afterSample -Name 'start_time_utc_ticks')
        $beforeName = Get-KettlePerfReleaseIdentifier `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $beforeSample -Name 'process_name') `
            -FieldName 'idle process name' -MaximumLength 256
        $afterName = Get-KettlePerfReleaseIdentifier `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $afterSample -Name 'process_name') `
            -FieldName 'idle process name' -MaximumLength 256
        if ($beforeStart -ne $afterStart -or $beforeName -cne $afterName) {
            throw 'idle accounting process identity changed'
        }
        $beforeCpu = Get-KettlePerfReleaseValue `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $beforeSample -Name 'cpu_seconds')
        $beforeWorkingSet = Get-KettlePerfReleaseValue `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $beforeSample -Name 'working_set_bytes')
        $afterCpu = Get-KettlePerfReleaseValue `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $afterSample -Name 'cpu_seconds')
        $afterWorkingSet = Get-KettlePerfReleaseValue `
            -Value (Get-KettlePerfScoreSourceProperty `
                -InputObject $afterSample -Name 'working_set_bytes')
        if ($afterCpu + 0.000001 -lt $beforeCpu) {
            throw 'idle accounting process CPU decreased'
        }
        foreach ($workingSet in @($beforeWorkingSet, $afterWorkingSet)) {
            if (
                $workingSet -lt 0.0 -or
                $workingSet -gt [int64]::MaxValue -or
                [Math]::Truncate($workingSet) -ne $workingSet
            ) {
                throw 'idle accounting working-set bytes are invalid'
            }
        }
        $calculatedWorkingSetBytes += $beforeWorkingSet
        $calculatedDelta += [Math]::Max(0.0, $afterCpu - $beforeCpu)
    }
    $recordedDelta = Get-KettlePerfReleaseValue `
        -Value (Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'cpu_seconds_delta')
    $measuredSeconds = Get-KettlePerfReleaseValue `
        -Value (Get-KettlePerfScoreSourceProperty `
            -InputObject $Observation -Name 'measured_seconds')
    if ($measuredSeconds -le 0.0) {
        throw 'idle accounting measured_seconds must be positive'
    }
    $calculatedIdle = ($calculatedDelta / $measuredSeconds) * 100.0
    $calculatedFreshWs = [Math]::Round(
        $calculatedWorkingSetBytes / 1MB,
        1
    )
    if (
        [Math]::Abs($recordedDelta - $calculatedDelta) -gt 0.00001 -or
        [Math]::Abs($ReportedIdleCpu - $calculatedIdle) -gt 0.000011 -or
        [Math]::Abs($ReportedFreshWs - $calculatedFreshWs) -gt 0.000001
    ) {
        throw 'idle accounting aggregates differ from per-process evidence'
    }
}

function ConvertTo-KettlePerfScoreSimpleMetric {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The result is an observation collection.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        $Rows,
        [Parameter(Mandatory = $true)]
        [string[]]$Terminals,
        [Parameter(Mandatory = $true)]
        [ValidateSet('startup_ms', 'idle_cpu_pct', 'fresh_ws_mb')]
        [string]$Metric,
        [Parameter(Mandatory = $true)]
        [ValidateRange(6, 1000)]
        [int]$ExpectedSamples
    )

    $normalized = [Collections.Generic.List[object]]::new()
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            throw "release metric '$Metric' lacks terminal '$terminal'"
        }
        $row = $Rows[$terminal]
        $sourceName = if ($Metric -ceq 'startup_ms') {
            'startup_observations'
        } else {
            'idle_observations'
        }
        $source = [object[]]@($row[$sourceName])
        if ($source.Count -ne $ExpectedSamples) {
            throw (
                "release metric '$Metric' terminal '$terminal' has " +
                "$($source.Count) raw observations; expected $ExpectedSamples"
            )
        }
        $sampleIds = [Collections.Generic.HashSet[int64]]::new()
        foreach ($observation in $source) {
            Assert-KettlePerfScoreSourceTerminal `
                -Observation $observation -Terminal $terminal
            $status = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'status'
            if ($status -isnot [string] -or [string]$status -cne 'ok') {
                throw "release metric '$Metric' requires status exactly ok"
            }
            if ($Metric -ceq 'startup_ms') {
                $sourceMetric = Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'metric'
                if (
                    $sourceMetric -isnot [string] -or
                    [string]$sourceMetric -cne 'startup_ms'
                ) {
                    throw 'startup observation has an invalid metric'
                }
            }
            $sampleId = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'sample_id')
            if (-not $sampleIds.Add($sampleId)) {
                throw (
                    "release metric '$Metric' terminal '$terminal' has an " +
                    'invalid or duplicate sample_id'
                )
            }
            $clusterId = Get-KettlePerfReleaseIdentifier `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'cluster_id') `
                -FieldName 'raw cluster_id' -MaximumLength 256
            $sequence = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'sequence')
            $sourceValue = if ($Metric -ceq 'startup_ms') {
                Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'value'
            } else {
                Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name $Metric
            }
            $value = Get-KettlePerfReleaseValue -Value $sourceValue
            if ($Metric -ceq 'startup_ms' -and $value -le 0.0) {
                throw 'startup observations must be positive'
            }
            if ($Metric -ceq 'startup_ms') {
                $windowDiscovered = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation `
                        -Name 'window_discovered_ms')
                $sizedFocused = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'sized_focused_ms')
                $goPublished = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'go_published_ms')
                $goToReady = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'go_to_ready_ms')
                [void](Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation `
                        -Name 'post_endpoint_attribution_ms'))
                if (
                    $windowDiscovered -gt $sizedFocused -or
                    $sizedFocused -gt $goPublished -or
                    $goPublished -gt $value -or
                    [Math]::Abs(($value - $goPublished) - $goToReady) -gt
                        0.011
                ) {
                    throw 'startup timing milestones are inconsistent'
                }
            } else {
                $reportedIdleCpu = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'idle_cpu_pct')
                $reportedFreshWs = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'fresh_ws_mb')
                Assert-KettlePerfScoreIdleAccounting `
                    -Observation $observation `
                    -ReportedIdleCpu $reportedIdleCpu `
                    -ReportedFreshWs $reportedFreshWs
            }
            $normalized.Add(
                (New-KettlePerfScoreObservation `
                    -Terminal $terminal -ClusterId $clusterId `
                    -Sequence $sequence -Value $value)
            )
        }
    }
    return [object[]]$normalized.ToArray()
}

function ConvertTo-KettlePerfScoreLatencyMetric {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The result is an observation collection.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        $Rows,
        [Parameter(Mandatory = $true)]
        [string[]]$Terminals,
        [ValidateRange(6, 10000)]
        [int]$ExpectedSamples = 60,
        [ValidateRange(1, 1000)]
        [int]$BlockSize = 10,
        [ValidateRange(0, 1000)]
        [int]$MaximumCensored = 3,
        [ValidateRange(100, 10000)]
        [int]$TimeoutMs = 800
    )

    if (($ExpectedSamples % $BlockSize) -ne 0) {
        throw 'latency ExpectedSamples must be divisible by BlockSize'
    }
    $expectedBlocks = [int]($ExpectedSamples / $BlockSize)
    $normalized = [Collections.Generic.List[object]]::new()
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            throw "release latency lacks terminal '$terminal'"
        }
        $source = [object[]]@($Rows[$terminal].latency_observations)
        if ($source.Count -ne $ExpectedSamples) {
            throw (
                "release latency terminal '$terminal' has $($source.Count) " +
                "raw observations; expected $ExpectedSamples"
            )
        }
        $groups = [Collections.Generic.Dictionary[
            string,
            Collections.Generic.List[object]
        ]]::new([StringComparer]::Ordinal)
        $terminalSamples = [Collections.Generic.HashSet[int64]]::new()
        $censored = 0
        foreach ($observation in $source) {
            Assert-KettlePerfScoreSourceTerminal `
                -Observation $observation -Terminal $terminal
            $sourceMetric = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'metric'
            if (
                $sourceMetric -isnot [string] -or
                [string]$sourceMetric -cne 'latency_ms'
            ) {
                throw 'latency observation has an invalid metric'
            }
            $clusterId = Get-KettlePerfReleaseIdentifier `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'cluster_id') `
                -FieldName 'raw cluster_id' -MaximumLength 256
            $terminalSample = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'terminal_sample')
            if (
                $terminalSample -gt $ExpectedSamples -or
                -not $terminalSamples.Add($terminalSample)
            ) {
                throw (
                    "release latency terminal '$terminal' has an invalid or " +
                    'duplicate terminal_sample'
                )
            }
            $status = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'status'
            $recordedTimeout = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'timeout_ms')
            if ($recordedTimeout -ne $TimeoutMs) {
                throw "release latency timeout differs from $TimeoutMs ms"
            }
            $rawValue = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'value'
            if ($status -is [string] -and [string]$status -ceq 'ok') {
                $latency = Get-KettlePerfReleaseValue -Value $rawValue
                if ($latency -le 0.0 -or $latency -gt $TimeoutMs) {
                    throw 'successful latency observation is outside its timeout'
                }
            } elseif (
                $status -is [string] -and
                [string]$status -ceq 'censored-timeout'
            ) {
                if ($null -ne $rawValue) {
                    throw 'censored latency observation must have a null value'
                }
                $latency = [double]$TimeoutMs
                $censored++
            } else {
                throw 'latency status must be exactly ok or censored-timeout'
            }
            if (-not $groups.ContainsKey($clusterId)) {
                $groups.Add(
                    $clusterId,
                    [Collections.Generic.List[object]]::new()
                )
            }
            $groups[$clusterId].Add(
                [pscustomobject]@{
                    observation = $observation
                    value = $latency
                }
            )
        }
        if ($censored -gt $MaximumCensored) {
            throw (
                "release latency terminal '$terminal' has $censored censored " +
                "samples; maximum is $MaximumCensored"
            )
        }
        if ($groups.Count -ne $expectedBlocks) {
            throw (
                "release latency terminal '$terminal' has $($groups.Count) " +
                "blocks; expected $expectedBlocks"
            )
        }
        foreach ($clusterId in $groups.Keys) {
            $block = $groups[$clusterId]
            if ($block.Count -ne $BlockSize) {
                throw (
                    "release latency block '$clusterId' has $($block.Count) " +
                    "samples; expected $BlockSize"
                )
            }
            $inBlock = [Collections.Generic.HashSet[int64]]::new()
            $blockSequence = $null
            $values = [Collections.Generic.List[double]]::new()
            foreach ($entry in $block) {
                $observation = $entry.observation
                $sampleInBlock = Get-KettlePerfReleaseSequence `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'sample_in_block')
                if (
                    $sampleInBlock -gt $BlockSize -or
                    -not $inBlock.Add($sampleInBlock)
                ) {
                    throw (
                        "release latency block '$clusterId' has an invalid or " +
                        'duplicate sample_in_block'
                    )
                }
                $sequence = Get-KettlePerfReleaseSequence `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'sequence')
                if ($null -eq $blockSequence) {
                    $blockSequence = $sequence
                } elseif ($sequence -ne $blockSequence) {
                    throw "release latency block '$clusterId' spans sequences"
                }
                $values.Add([double]$entry.value)
            }
            $median = [double](Get-KettlePerfMedian -Values @(
                $values | Sort-Object
            ))
            $normalized.Add(
                (New-KettlePerfScoreObservation `
                    -Terminal $terminal -ClusterId $clusterId `
                    -Sequence $blockSequence -Value $median)
            )
        }
    }
    return [object[]]$normalized.ToArray()
}

function ConvertTo-KettlePerfScoreThroughputMetric {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The result is an observation collection.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        $Rows,
        [Parameter(Mandatory = $true)]
        [string[]]$Terminals,
        [ValidateRange(6, 1000)]
        [int]$ExpectedRounds = 6,
        [ValidateRange(320, 16384)]
        [int]$ExpectedWindowWidth = 1280,
        [ValidateRange(240, 16384)]
        [int]$ExpectedWindowHeight = 800
    )

    $normalized = [Collections.Generic.List[object]]::new()
    $expectedPayloads = [string[]]@('ascii', 'sgr', 'unicode')
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            throw "release throughput lacks terminal '$terminal'"
        }
        $source = [object[]]@($Rows[$terminal].throughput_observations)
        if ($source.Count -ne ($ExpectedRounds * $expectedPayloads.Count)) {
            throw (
                "release throughput terminal '$terminal' has $($source.Count) " +
                "raw observations; expected $($ExpectedRounds * 3)"
            )
        }
        $groups = [Collections.Generic.Dictionary[
            string,
            Collections.Generic.List[object]
        ]]::new([StringComparer]::Ordinal)
        $terminalColumns = $null
        $terminalRows = $null
        foreach ($observation in $source) {
            Assert-KettlePerfScoreSourceTerminal `
                -Observation $observation -Terminal $terminal
            $sourceMetric = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'metric'
            $status = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'status'
            if (
                $sourceMetric -isnot [string] -or
                [string]$sourceMetric -cne 'throughput_mb_per_s' -or
                $status -isnot [string] -or
                [string]$status -cne 'ok' -or
                (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'go_handshake') -cne
                    'locked-create-new-token-v1'
            ) {
                throw (
                    'throughput observation has an invalid metric, status, ' +
                    'or GO handshake'
                )
            }
            $clientPixels = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'client_pixels'
            $consoleCells = Get-KettlePerfScoreSourceProperty `
                -InputObject $observation -Name 'console_cells'
            $clientWidth = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $clientPixels -Name 'width')
            $clientHeight = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $clientPixels -Name 'height')
            $consoleColumns = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $consoleCells -Name 'columns')
            $consoleRows = Get-KettlePerfReleaseSequence `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $consoleCells -Name 'rows')
            if (
                $clientWidth -ne $ExpectedWindowWidth -or
                $clientHeight -ne $ExpectedWindowHeight -or
                $consoleColumns -gt 10000 -or
                $consoleRows -gt 10000
            ) {
                throw 'throughput observation has invalid client or cell geometry'
            }
            if ($null -eq $terminalColumns) {
                $terminalColumns = $consoleColumns
                $terminalRows = $consoleRows
            } elseif (
                $consoleColumns -ne $terminalColumns -or
                $consoleRows -ne $terminalRows
            ) {
                throw 'throughput console geometry changed between observations'
            }
            $goWaitMs = Get-KettlePerfReleaseValue `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'go_wait_ms')
            if ($goWaitMs -lt 0.0) {
                throw 'throughput GO wait duration must be nonnegative'
            }
            $clusterId = Get-KettlePerfReleaseIdentifier `
                -Value (Get-KettlePerfScoreSourceProperty `
                    -InputObject $observation -Name 'cluster_id') `
                -FieldName 'raw cluster_id' -MaximumLength 256
            if (-not $groups.ContainsKey($clusterId)) {
                $groups.Add(
                    $clusterId,
                    [Collections.Generic.List[object]]::new()
                )
            }
            $groups[$clusterId].Add($observation)
        }
        if ($groups.Count -ne $ExpectedRounds) {
            throw (
                "release throughput terminal '$terminal' has $($groups.Count) " +
                "rounds; expected $ExpectedRounds"
            )
        }
        foreach ($clusterId in $groups.Keys) {
            $round = $groups[$clusterId]
            if ($round.Count -ne $expectedPayloads.Count) {
                throw "throughput round '$clusterId' lacks exactly three payloads"
            }
            $payloadSet = [Collections.Generic.HashSet[string]]::new(
                [StringComparer]::Ordinal
            )
            $roundSequence = $null
            $logSum = 0.0
            foreach ($observation in $round) {
                $payload = Get-KettlePerfReleaseIdentifier `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'payload') `
                    -FieldName 'throughput payload' -MaximumLength 32
                if (
                    $payload -cnotin $expectedPayloads -or
                    -not $payloadSet.Add($payload)
                ) {
                    throw "throughput round '$clusterId' has invalid payload coverage"
                }
                $sequence = Get-KettlePerfReleaseSequence `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'sequence')
                if ($null -eq $roundSequence) {
                    $roundSequence = $sequence
                } elseif ($sequence -ne $roundSequence) {
                    throw "throughput round '$clusterId' spans sequences"
                }
                $value = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'value')
                if ($value -le 0.0) {
                    throw 'throughput observations must be positive'
                }
                $seconds = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'seconds')
                $writeSeconds = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'write_seconds')
                $drainMs = Get-KettlePerfReleaseValue `
                    -Value (Get-KettlePerfScoreSourceProperty `
                        -InputObject $observation -Name 'drain_ms')
                if (
                    $seconds -le 0.0 -or
                    $writeSeconds -le 0.0 -or
                    $drainMs -lt 0.0
                ) {
                    throw 'throughput timing observations must be positive'
                }
                $expectedSeconds = $writeSeconds + ($drainMs / 1000.0)
                $secondsTolerance = [Math]::Max(
                    0.000000001,
                    [Math]::Abs($expectedSeconds) * 0.000000001
                )
                if (
                    [Math]::Abs($seconds - $expectedSeconds) -gt
                        $secondsTolerance
                ) {
                    throw (
                        'throughput end-to-drain seconds differ from writer ' +
                        'and drain evidence'
                    )
                }
                $payloadContract = $KettlePerfPayloadContracts[$payload]
                if ($null -eq $payloadContract) {
                    throw "throughput payload '$payload' has no byte contract"
                }
                $derivedValue = (
                    ([double]$payloadContract.bytes / 1MB) / $seconds
                )
                $valueTolerance = [Math]::Max(
                    0.000000001,
                    [Math]::Abs($derivedValue) * 0.000000001
                )
                if ([Math]::Abs($value - $derivedValue) -gt $valueTolerance) {
                    throw 'throughput MB/s differs from pinned bytes and seconds'
                }
                $logSum += [Math]::Log($derivedValue)
            }
            $geometricMean = [Math]::Exp(
                $logSum / [double]$expectedPayloads.Count
            )
            $normalized.Add(
                (New-KettlePerfScoreObservation `
                    -Terminal $terminal -ClusterId $clusterId `
                    -Sequence $roundSequence -Value $geometricMean)
            )
        }
    }
    return [object[]]$normalized.ToArray()
}

function Get-KettlePerfReleaseStatisticalGate {
    param(
        [Parameter(Mandatory = $true)]
        $Rows,
        [string]$CandidateTerminal = 'kettle',
        [string[]]$IsolatedPeers = @(
            'alacritty', 'wezterm', 'rio', 'tabby'
        ),
        [Parameter(Mandatory = $true)]
        [string]$Seed,
        [ValidateRange(6, 1000)]
        [int]$StartupSamples = 12,
        [ValidateRange(6, 1000)]
        [int]$IdleSamples = 6,
        [ValidateRange(6, 10000)]
        [int]$LatencySamples = 60,
        [ValidateRange(1, 1000)]
        [int]$LatencyBlockSize = 10,
        [ValidateRange(0, 1000)]
        [int]$MaximumLatencyCensored = 3,
        [ValidateRange(100, 10000)]
        [int]$LatencyTimeoutMs = 800,
        [ValidateRange(6, 1000)]
        [int]$ThroughputRounds = 6,
        [ValidateRange(320, 16384)]
        [int]$ExpectedWindowWidth = 1280,
        [ValidateRange(240, 16384)]
        [int]$ExpectedWindowHeight = 800,
        [ValidateRange(1000, 100000)]
        [int]$BootstrapIterations = 10000
    )

    $terminals = [string[]]@($CandidateTerminal) + [string[]]$IsolatedPeers
    $definitions = [object[]]@(
        [pscustomobject]@{
            name = 'startup_ms'
            absolute_margin = 25.0
            relative_margin = 0.05
        },
        [pscustomobject]@{
            name = 'idle_cpu_pct'
            absolute_margin = 0.10
            relative_margin = 0.20
        },
        [pscustomobject]@{
            name = 'fresh_ws_mb'
            absolute_margin = 8.0
            relative_margin = 0.05
        },
        [pscustomobject]@{
            name = 'latency_ms'
            absolute_margin = 5.0
            relative_margin = 0.10
        }
    )
    $metricResults = [Collections.Generic.List[object]]::new()
    foreach ($definition in $definitions) {
        $observations = switch -CaseSensitive ($definition.name) {
            'startup_ms' {
                ConvertTo-KettlePerfScoreSimpleMetric `
                    -Rows $Rows -Terminals $terminals `
                    -Metric startup_ms -ExpectedSamples $StartupSamples
                break
            }
            'idle_cpu_pct' {
                ConvertTo-KettlePerfScoreSimpleMetric `
                    -Rows $Rows -Terminals $terminals `
                    -Metric idle_cpu_pct -ExpectedSamples $IdleSamples
                break
            }
            'fresh_ws_mb' {
                ConvertTo-KettlePerfScoreSimpleMetric `
                    -Rows $Rows -Terminals $terminals `
                    -Metric fresh_ws_mb -ExpectedSamples $IdleSamples
                break
            }
            'latency_ms' {
                ConvertTo-KettlePerfScoreLatencyMetric `
                    -Rows $Rows -Terminals $terminals `
                    -ExpectedSamples $LatencySamples `
                    -BlockSize $LatencyBlockSize `
                    -MaximumCensored $MaximumLatencyCensored `
                    -TimeoutMs $LatencyTimeoutMs
                break
            }
        }
        $comparison = Get-KettlePerfReleaseComparison `
            -Observations $observations `
            -CandidateTerminal $CandidateTerminal `
            -IsolatedPeers $IsolatedPeers -Direction lower `
            -AbsoluteMargin ([double]$definition.absolute_margin) `
            -RelativeMargin ([double]$definition.relative_margin) `
            -BootstrapIterations $BootstrapIterations `
            -Seed "$Seed|metric:$($definition.name)"
        $metricResults.Add(
            [pscustomobject][ordered]@{
                metric = $definition.name
                comparison = $comparison
            }
        )
    }

    $peerResults = [Collections.Generic.List[object]]::new()
    foreach ($peer in $IsolatedPeers) {
        $classifications = [Collections.Generic.List[object]]::new()
        foreach ($metricResult in $metricResults) {
            $peerComparison = @(
                $metricResult.comparison.comparisons |
                    Where-Object { $_.peer -ceq $peer }
            )
            if ($peerComparison.Count -ne 1) {
                throw (
                    "release metric '$($metricResult.metric)' has no unique " +
                    "comparison for '$peer'"
                )
            }
            $classifications.Add(
                [pscustomobject][ordered]@{
                    metric = $metricResult.metric
                    classification = $peerComparison[0].classification
                }
            )
        }
        $wins = @(
            $classifications |
                Where-Object { $_.classification -ceq 'confirmed-win' }
        ).Count
        $losses = @(
            $classifications |
                Where-Object { $_.classification -ceq 'confirmed-loss' }
        ).Count
        $uncertain = $classifications.Count - $wins - $losses
        $classification = if ($wins -ge 3 -and $losses -le 1) {
            'confirmed-win'
        } elseif ($losses -ge 3 -and $wins -le 1) {
            'confirmed-loss'
        } else {
            'uncertain'
        }
        $peerResults.Add(
            [pscustomobject][ordered]@{
                peer = $peer
                classification = $classification
                confirmed_metric_wins = $wins
                confirmed_metric_losses = $losses
                uncertain_metrics = $uncertain
                required_metric_wins = 3
                maximum_metric_losses = 1
                metrics = [object[]]$classifications.ToArray()
            }
        )
    }
    $primaryPolicy = Test-KettlePerfReleasePolicy `
        -Comparisons ([object[]]$peerResults.ToArray())
    $primaryDriftPassed = @(
        $metricResults | Where-Object {
            -not [bool]$_.comparison.drift.passed
        }
    ).Count -eq 0
    $throughputObservations = ConvertTo-KettlePerfScoreThroughputMetric `
        -Rows $Rows -Terminals $terminals `
        -ExpectedRounds $ThroughputRounds `
        -ExpectedWindowWidth $ExpectedWindowWidth `
        -ExpectedWindowHeight $ExpectedWindowHeight
    $throughput = Get-KettlePerfThroughputReleaseComparison `
        -Observations $throughputObservations `
        -CandidateTerminal $CandidateTerminal `
        -IsolatedPeers $IsolatedPeers `
        -BootstrapIterations $BootstrapIterations `
        -Seed "$Seed|metric:throughput_geomean_mbps"

    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'kettle-release-superiority-v1'
        candidate = $CandidateTerminal
        isolated_peers = [string[]]$IsolatedPeers
        advisory_terminals = [string[]]@('wt')
        confidence_level = 0.90
        bootstrap_iterations = $BootstrapIterations
        primary_metrics = [object[]]$metricResults.ToArray()
        peer_primary_classifications = [object[]]$peerResults.ToArray()
        primary_policy = $primaryPolicy
        primary_drift_passed = $primaryDriftPassed
        throughput = $throughput
        passed = (
            [bool]$primaryPolicy.passed -and
            $primaryDriftPassed -and
            [bool]$throughput.passed
        )
    }
}

function Get-KettlePerfBaselineStatisticalGate {
    param(
        [Parameter(Mandatory = $true)]
        $CurrentRows,
        [Parameter(Mandatory = $true)]
        $BaselineRows,
        [Parameter(Mandatory = $true)]
        [string]$Seed,
        [string]$CandidateTerminal = 'kettle',
        [ValidateRange(6, 1000)]
        [int]$StartupSamples = 12,
        [ValidateRange(6, 1000)]
        [int]$IdleSamples = 6,
        [ValidateRange(6, 10000)]
        [int]$LatencySamples = 60,
        [ValidateRange(1, 1000)]
        [int]$LatencyBlockSize = 10,
        [ValidateRange(0, 1000)]
        [int]$MaximumLatencyCensored = 3,
        [ValidateRange(100, 10000)]
        [int]$LatencyTimeoutMs = 800,
        [ValidateRange(6, 1000)]
        [int]$ThroughputRounds = 6,
        [ValidateRange(320, 16384)]
        [int]$ExpectedWindowWidth = 1280,
        [ValidateRange(240, 16384)]
        [int]$ExpectedWindowHeight = 800,
        [ValidateRange(1000, 100000)]
        [int]$BootstrapIterations = 10000
    )

    $terminals = [string[]]@($CandidateTerminal)
    $definitions = [object[]]@(
        [pscustomobject]@{
            name = 'startup_ms'
            direction = 'lower'
            absolute_margin = 25.0
            relative_margin = 0.05
        },
        [pscustomobject]@{
            name = 'idle_cpu_pct'
            direction = 'lower'
            absolute_margin = 0.10
            relative_margin = 0.20
        },
        [pscustomobject]@{
            name = 'fresh_ws_mb'
            direction = 'lower'
            absolute_margin = 8.0
            relative_margin = 0.05
        },
        [pscustomobject]@{
            name = 'latency_ms'
            direction = 'lower'
            absolute_margin = 5.0
            relative_margin = 0.10
        },
        [pscustomobject]@{
            name = 'throughput_geomean_mbps'
            direction = 'higher'
            absolute_margin = 0.0
            relative_margin = 0.05
        }
    )
    $metricResults = [Collections.Generic.List[object]]::new()
    foreach ($definition in $definitions) {
        if ($definition.name -ceq 'throughput_geomean_mbps') {
            $current = ConvertTo-KettlePerfScoreThroughputMetric `
                -Rows $CurrentRows -Terminals $terminals `
                -ExpectedRounds $ThroughputRounds `
                -ExpectedWindowWidth $ExpectedWindowWidth `
                -ExpectedWindowHeight $ExpectedWindowHeight
            $baseline = ConvertTo-KettlePerfScoreThroughputMetric `
                -Rows $BaselineRows -Terminals $terminals `
                -ExpectedRounds $ThroughputRounds `
                -ExpectedWindowWidth $ExpectedWindowWidth `
                -ExpectedWindowHeight $ExpectedWindowHeight
        } elseif ($definition.name -ceq 'latency_ms') {
            $current = ConvertTo-KettlePerfScoreLatencyMetric `
                -Rows $CurrentRows -Terminals $terminals `
                -ExpectedSamples $LatencySamples `
                -BlockSize $LatencyBlockSize `
                -MaximumCensored $MaximumLatencyCensored `
                -TimeoutMs $LatencyTimeoutMs
            $baseline = ConvertTo-KettlePerfScoreLatencyMetric `
                -Rows $BaselineRows -Terminals $terminals `
                -ExpectedSamples $LatencySamples `
                -BlockSize $LatencyBlockSize `
                -MaximumCensored $MaximumLatencyCensored `
                -TimeoutMs $LatencyTimeoutMs
        } else {
            $sampleCount = if ($definition.name -ceq 'startup_ms') {
                $StartupSamples
            } else {
                $IdleSamples
            }
            $current = ConvertTo-KettlePerfScoreSimpleMetric `
                -Rows $CurrentRows -Terminals $terminals `
                -Metric $definition.name -ExpectedSamples $sampleCount
            $baseline = ConvertTo-KettlePerfScoreSimpleMetric `
                -Rows $BaselineRows -Terminals $terminals `
                -Metric $definition.name -ExpectedSamples $sampleCount
        }
        $metricResults.Add(
            (Get-KettlePerfBaselineNonInferiority `
                -CurrentObservations $current `
                -BaselineObservations $baseline `
                -Metric $definition.name `
                -Direction $definition.direction `
                -AbsoluteMargin ([double]$definition.absolute_margin) `
                -RelativeMargin ([double]$definition.relative_margin) `
                -BootstrapIterations $BootstrapIterations `
                -Seed "$Seed|baseline-metric:$($definition.name)")
        )
    }
    $required = [string[]]@($definitions | ForEach-Object { $_.name })
    $policy = Get-KettlePerfBaselinePolicy `
        -MetricResults ([object[]]$metricResults.ToArray()) `
        -RequiredMetrics $required
    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'kettle-baseline-noninferiority-gate-v1'
        candidate = $CandidateTerminal
        confidence_level = 0.90
        bootstrap_iterations = $BootstrapIterations
        required_metrics = $required
        metrics = [object[]]$metricResults.ToArray()
        policy = $policy
        passed = [bool]$policy.passed
    }
}
