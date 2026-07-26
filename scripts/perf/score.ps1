# Score a perf-all result directory and enforce the release performance gates.
#
# Usage:
#   pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after
#   pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after `
#     -BaselineResultsDir target/perf-results/before -MaxRegressionPct 7.5
param(
    [Parameter(Mandatory = $true)]
    [string]$ResultsDir,
    [string]$BaselineResultsDir = '',
    [ValidateRange(0.0, 1000.0)]
    [double]$MaxRegressionPct = 7.5,
    [ValidateRange(1, 100)]
    [int]$MaxKettleRank = 2,
    [ValidateRange(0, 100)]
    [int]$MinimumPeersBeaten = 3,
    [string[]]$RequiredTerminals = @('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby'),
    [ValidateRange(1, 100)]
    [int]$MinimumMetricsPerTerminal = 5,
    [ValidateRange(0, 100)]
    [int]$MinimumThroughputPeersMeasured = 4,
    [ValidateRange(1, 100)]
    [int]$MaxKettleThroughputRank = 2,
    [ValidateRange(0, 100)]
    [int]$MinimumThroughputPeersBeaten = 3,
    [ValidateRange(1, 1000)]
    [int]$MinimumStartupSamples = 5,
    [ValidateRange(1, 1000)]
    [int]$MinimumThroughputRuns = 3,
    [switch]$RequireLatency,
    [ValidateRange(0, 100)]
    [int]$MinimumLatencyPeersBeaten = 3,
    [ValidateRange(1, 10000)]
    [int]$MinimumLatencySamples = 20,
    [ValidateRange(0.0, 1.0)]
    [double]$MaxLatencyMissRate = 0.10,
    [switch]$RequireMenuHover,
    [switch]$RequireVtebench,
    [switch]$RequireMonitorTransition,
    [ValidateRange(1, 1000)]
    [int]$MinimumMonitorTransitionSamplesPerState = 10,
    [ValidateRange(1.0, 60000.0)]
    [double]$MaxMonitorTransitionP95Ms = 1000.0,
    [ValidateRange(1.0, 60000.0)]
    [double]$MaxMonitorTransitionMaxMs = 2000.0,
    [ValidateRange(0.0, 60000.0)]
    [double]$MonitorTransitionBaselineAbsoluteMarginMs = 100.0,
    [ValidateRange(0.0, 100.0)]
    [double]$MonitorTransitionBaselineRelativeMarginPct = 25.0,
    [ValidateRange(1, 10000)]
    [int]$MinimumMenuHoverSamples = 50,
    [ValidateRange(0.0, 10000.0)]
    [double]$MaxMenuHoverP95Ms = 33.0,
    [ValidateRange(0.0, 10000.0)]
    [double]$MaxMenuHoverP99Ms = 50.0,
    [ValidateRange(0.0, 10000.0)]
    [double]$MenuHoverLongFrameMs = 100.0,
    [ValidateRange(0, 10000)]
    [int]$MaxMenuHoverLongFrames = 1,
    [switch]$AllowDirtyManifest,
    [string]$OutJson = ''
)
$ErrorActionPreference = 'Stop'
if ($MaxMonitorTransitionMaxMs -lt $MaxMonitorTransitionP95Ms) {
    throw (
        'MaxMonitorTransitionMaxMs must be greater than or equal to ' +
        'MaxMonitorTransitionP95Ms'
    )
}
. "$PSScriptRoot\payload-contract.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\statistics.ps1"
. "$PSScriptRoot\vtebench-dat.ps1"
. "$PSScriptRoot\release-statistics.ps1"
. "$PSScriptRoot\baseline-statistics.ps1"
. "$PSScriptRoot\score-statistics.ps1"
. "$PSScriptRoot\schedule.ps1"
. "$PSScriptRoot\harness-provenance.ps1"
. "$PSScriptRoot\evidence-snapshot.ps1"

$script:KettlePerfScoreEvidenceSnapshots = $null
$script:KettlePerfReleaseVtebenchRevision =
    'ead80032e57dee2e75f0b51f2ea67528647d9944'

function Get-KettlePerfScoreEvidenceSnapshot([string]$Directory) {
    if ($null -eq $script:KettlePerfScoreEvidenceSnapshots) {
        throw 'Performance evidence snapshots have not been opened'
    }
    $root = [IO.Path]::GetFullPath($Directory).TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    )
    if (-not $script:KettlePerfScoreEvidenceSnapshots.ContainsKey($root)) {
        throw "Path is outside the held performance evidence roots: $Directory"
    }
    return $script:KettlePerfScoreEvidenceSnapshots[$root]
}

function Read-JsonFile([string]$Path) {
    $fullPath = [IO.Path]::GetFullPath($Path)
    $directory = [IO.Path]::GetDirectoryName($fullPath)
    $leafName = [IO.Path]::GetFileName($fullPath)
    $snapshot = Get-KettlePerfScoreEvidenceSnapshot $directory
    $expectedPath = [IO.Path]::Combine(
        [string]$snapshot.root_path,
        $leafName
    )
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        [IO.Path]::GetFullPath($expectedPath),
        $fullPath
    )) {
        throw "Evidence JSON path is not a direct snapshot leaf: $Path"
    }
    $entry = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName $leafName
    if ($null -eq $entry) {
        return $null
    }
    return $entry.value
}

function Get-PropertyValue($Object, [string]$Name) {
    if ($null -eq $Object) {
        return $null
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($property) {
        return $property.Value
    }
    return $null
}

function As-Double($Value, [switch]$AllowZero) {
    if ($null -eq $Value) { return $null }
    try {
        $d = [double]$Value
        if (
            [double]::IsNaN($d) -or
            [double]::IsInfinity($d) -or
            $d -lt 0.0 -or
            (-not $AllowZero -and $d -eq 0.0)
        ) {
            return $null
        }
        return $d
    } catch {
        return $null
    }
}

function Payload-Mbps($Payloads, [string[]]$Names) {
    if ($null -eq $Payloads) { return $null }
    foreach ($name in $Names) {
        $prop = $Payloads.PSObject.Properties[$name]
        if ($prop) {
            $v = As-Double (Get-PropertyValue $prop.Value 'mb_per_s_median')
            if ($null -ne $v) { return $v }
        }
    }
    return $null
}

function Payload-Runs($Payloads, [string]$Name) {
    if ($null -eq $Payloads) { return $null }
    $property = $Payloads.PSObject.Properties[$Name]
    if (-not $property) { return $null }
    return As-NonnegativeInt (Get-PropertyValue $property.Value 'runs')
}

function Payload-Property($Payloads, [string]$Name, [string]$PropertyName) {
    if ($null -eq $Payloads) { return $null }
    $property = $Payloads.PSObject.Properties[$Name]
    if (-not $property) { return $null }
    return Get-PropertyValue $property.Value $PropertyName
}

function As-NonnegativeInt($Value) {
    if ($null -eq $Value) {
        return $null
    }
    try {
        $double = [double]$Value
        if (
            [double]::IsNaN($double) -or
            [double]::IsInfinity($double) -or
            $double -lt 0.0 -or
            [Math]::Floor($double) -ne $double -or
            $double -gt [int]::MaxValue
        ) {
            return $null
        }
        return [int]$double
    } catch {
        return $null
    }
}

function Test-StartupCoverage($Row, [int]$MinimumSamples) {
    $samples = As-NonnegativeInt $Row.startup_samples
    $requested = As-NonnegativeInt $Row.startup_requested_samples
    $misses = As-NonnegativeInt $Row.startup_misses
    $raw = @($Row.startup_ms_all)
    $validRaw = @($raw | Where-Object { $null -ne (As-Double $_) })
    $sorted = @($validRaw | Sort-Object)
    $reportedMedian = As-Double $Row.startup_ms
    $calculatedMedian = if ($sorted.Count) {
        Get-KettlePerfMedian $sorted
    } else {
        $null
    }
    return (
        $null -ne $samples -and
        $null -ne $requested -and
        $null -ne $misses -and
        $samples -ge $MinimumSamples -and
        $requested -ge $MinimumSamples -and
        $samples + $misses -eq $requested -and
        $raw.Count -eq $samples -and
        $validRaw.Count -eq $samples -and
        $null -ne $reportedMedian -and
        [Math]::Abs($reportedMedian - $calculatedMedian) -le 0.001
    )
}

function Test-LatencyCoverage(
    $Row,
    [double]$AllowedMissRate,
    [int]$MinimumSamples
) {
    $samples = As-NonnegativeInt $Row.latency_samples
    $requested = As-NonnegativeInt $Row.latency_requested_samples
    $misses = As-NonnegativeInt $Row.latency_misses
    $raw = @($Row.latency_ms_all)
    $validRaw = @($raw | Where-Object { $null -ne (As-Double $_) })
    $sorted = @($validRaw | Sort-Object)
    $reportedMedian = As-Double $Row.latency_ms
    $reportedP95 = As-Double $Row.latency_p95_ms
    $calculatedMedian = if ($sorted.Count) {
        Get-KettlePerfMedian $sorted
    } else {
        $null
    }
    $calculatedP95 = if ($sorted.Count) {
        $sorted[[int][Math]::Min(
            $sorted.Count - 1,
            [Math]::Ceiling($sorted.Count * 0.95) - 1
        )]
    } else {
        $null
    }
    if (
        $null -eq $samples -or
        $null -eq $requested -or
        $null -eq $misses -or
        $requested -le 0 -or
        $samples -lt $MinimumSamples -or
        $requested -lt $MinimumSamples -or
        $samples + $misses -ne $requested -or
        $raw.Count -ne $samples -or
        $validRaw.Count -ne $samples -or
        $null -eq $reportedMedian -or
        $null -eq $reportedP95 -or
        [Math]::Abs($reportedMedian - $calculatedMedian) -gt 0.011 -or
        [Math]::Abs($reportedP95 - $calculatedP95) -gt 0.011
    ) {
        return $false
    }
    return ($misses / [double]$requested) -le $AllowedMissRate
}

function Test-ThroughputCoverage($Row, [int]$MinimumRuns) {
    if (
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$Row.throughput_output_encoding,
            'utf-8'
        )
    ) {
        return $false
    }
    if ($Row.throughput_drain_required -ne $true) {
        return $false
    }
    $workloadPid = As-NonnegativeInt $Row.throughput_workload_pid
    $excludedPids = @(
        $Row.postflood_ws_excluded_pids |
            ForEach-Object { As-NonnegativeInt $_ }
    )
    if (
        $Row.postflood_ws_scope -ne
            'terminal-tree-excluding-workload' -or
        $null -eq $workloadPid -or
        $workloadPid -le 0 -or
        $excludedPids -notcontains $workloadPid
    ) {
        return $false
    }
    foreach ($name in @('ascii', 'sgr', 'unicode')) {
        $runs = As-NonnegativeInt $Row["${name}_runs"]
        $seconds = @($Row["${name}_seconds_all"])
        $validSeconds = @($seconds | Where-Object { $null -ne (As-Double $_) })
        $sortedSeconds = @($validSeconds | Sort-Object)
        $calculatedMedian = if ($sortedSeconds.Count) {
            Get-KettlePerfMedian $sortedSeconds
        } else {
            $null
        }
        $reportedMedian = As-Double $Row["${name}_seconds_median"]
        $reportedMbps = As-Double $Row["${name}_mbps"]
        $bytes = As-NonnegativeInt $Row["${name}_bytes"]
        $sha256 = [string]$Row["${name}_sha256"]
        $drainTimes = @($Row["${name}_drain_ms_all"])
        $validDrainTimes = @(
            $drainTimes | Where-Object {
                $null -ne (As-Double $_ -AllowZero)
            }
        )
        $drainMisses = As-NonnegativeInt $Row["${name}_drain_misses"]
        $writeSeconds = @($Row["${name}_write_seconds_all"])
        $validWriteSeconds = @(
            $writeSeconds | Where-Object { $null -ne (As-Double $_) }
        )
        $writeMedian = As-Double $Row["${name}_write_seconds_median"]
        $calculatedWriteMedian = if ($validWriteSeconds.Count) {
            Get-KettlePerfMedian @($validWriteSeconds | Sort-Object)
        } else {
            $null
        }
        $writerMbps = As-Double $Row["${name}_writer_mbps"]
        $calculatedWriterMbps = if ($null -ne $calculatedWriteMedian) {
            [Math]::Round(
                ($KettlePerfPayloadContracts[$name].bytes / 1MB) /
                    $calculatedWriteMedian,
                2
            )
        } else {
            $null
        }
        $expected = $KettlePerfPayloadContracts[$name]
        $calculatedMbps = if ($null -ne $calculatedMedian) {
            [Math]::Round(($expected.bytes / 1MB) / $calculatedMedian, 2)
        } else {
            $null
        }
        if (
            $null -eq $runs -or
            $runs -lt $MinimumRuns -or
            $seconds.Count -ne $runs -or
            $validSeconds.Count -ne $runs -or
            $drainTimes.Count -ne $runs -or
            $validDrainTimes.Count -ne $runs -or
            $writeSeconds.Count -ne $runs -or
            $validWriteSeconds.Count -ne $runs -or
            $drainMisses -ne 0 -or
            $Row["${name}_timing_boundary"] -ne
                'console-write-start-to-DSR-response' -or
            $null -eq $reportedMedian -or
            $null -eq $reportedMbps -or
            $null -eq $writeMedian -or
            $null -eq $writerMbps -or
            [Math]::Abs(
                $reportedMedian - [Math]::Round($calculatedMedian, 3)
            ) -gt 0.001 -or
            [Math]::Abs($reportedMbps - $calculatedMbps) -gt 0.011 -or
            [Math]::Abs(
                $writeMedian - [Math]::Round($calculatedWriteMedian, 3)
            ) -gt 0.001 -or
            [Math]::Abs($writerMbps - $calculatedWriterMbps) -gt 0.011 -or
            $bytes -ne $expected.bytes -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $sha256,
                $expected.sha256
            )
        ) {
            return $false
        }
        for ($i = 0; $i -lt $runs; $i++) {
            $expectedEndToDrain = (
                [double]$validWriteSeconds[$i] +
                ([double]$validDrainTimes[$i] / 1000.0)
            )
            if (
                [Math]::Abs(
                    [double]$validSeconds[$i] - $expectedEndToDrain
                ) -gt 0.000001
            ) {
                return $false
            }
        }
    }
    return (
        $null -ne (As-Double $Row.ascii_mbps) -and
        $null -ne (As-Double $Row.sgr_mbps) -and
        $null -ne (As-Double $Row.unicode_mbps)
    )
}

function Test-MenuHoverCoverage(
    $Menu,
    [int]$MinimumSamples,
    [double]$MaxP95,
    [double]$MaxP99,
    [double]$LongFrameMs,
    [int]$MaxLongFrames,
    [switch]$RequireObservations
) {
    if ($null -eq $Menu) {
        return $false
    }
    $samples = As-NonnegativeInt (Get-PropertyValue $Menu 'samples')
    $requested = As-NonnegativeInt (Get-PropertyValue $Menu 'requested_samples')
    $misses = As-NonnegativeInt (Get-PropertyValue $Menu 'misses')
    $longFrames = As-NonnegativeInt (Get-PropertyValue $Menu 'long_frames')
    $p95 = As-Double (Get-PropertyValue $Menu 'latency_ms_p95')
    $p99 = As-Double (Get-PropertyValue $Menu 'latency_ms_p99')
    $recordedLongFrameMs = As-Double (Get-PropertyValue $Menu 'long_frame_ms')
    $raw = @(Get-PropertyValue $Menu 'latency_ms_all')
    $validRaw = @($raw | Where-Object { $null -ne (As-Double $_) })
    $sorted = @($validRaw | Sort-Object)
    $calculatedP95 = if ($sorted.Count) {
        $sorted[[int][Math]::Min(
            $sorted.Count - 1,
            [Math]::Ceiling($sorted.Count * 0.95) - 1
        )]
    } else {
        $null
    }
    $calculatedP99 = if ($sorted.Count) {
        $sorted[[int][Math]::Min(
            $sorted.Count - 1,
            [Math]::Ceiling($sorted.Count * 0.99) - 1
        )]
    } else {
        $null
    }
    $calculatedLongFrames = @($validRaw | Where-Object {
        (As-Double $_) -gt $LongFrameMs
    }).Count
    $observationsValid = $true
    if ($RequireObservations) {
        $blockSize = As-NonnegativeInt (
            Get-PropertyValue $Menu 'block_size'
        )
        $blockCount = As-NonnegativeInt (
            Get-PropertyValue $Menu 'block_count'
        )
        $observations = @(Get-PropertyValue $Menu 'observations')
        $window = Get-PropertyValue $Menu 'window_pixels'
        $region = Get-PropertyValue $Menu 'capture_region'
        $windowWidth = As-NonnegativeInt (
            Get-PropertyValue $window 'width'
        )
        $windowHeight = As-NonnegativeInt (
            Get-PropertyValue $window 'height'
        )
        $regionX = As-NonnegativeInt (Get-PropertyValue $region 'x')
        $regionY = As-NonnegativeInt (Get-PropertyValue $region 'y')
        $regionWidth = As-NonnegativeInt (
            Get-PropertyValue $region 'width'
        )
        $regionHeight = As-NonnegativeInt (
            Get-PropertyValue $region 'height'
        )
        $observationsValid = (
            $null -ne $blockSize -and
            $blockSize -gt 0 -and
            $null -ne $blockCount -and
            $blockCount * $blockSize -eq $requested -and
            $observations.Count -eq $requested -and
            (Get-PropertyValue $Menu 'capture_scope') -eq
                'context-menu-roi' -and
            (Get-PropertyValue $Menu 'observation_limit') -eq
                'comparative-software-capture-not-input-to-photon' -and
            $null -ne $windowWidth -and
            $null -ne $windowHeight -and
            $null -ne $regionX -and
            $null -ne $regionY -and
            $null -ne $regionWidth -and
            $null -ne $regionHeight -and
            $regionWidth -gt 0 -and
            $regionHeight -gt 0 -and
            $regionX + $regionWidth -le $windowWidth -and
            $regionY + $regionHeight -le $windowHeight
        )
        if ($observationsValid) {
            for ($index = 0; $index -lt $observations.Count; $index++) {
                $observation = $observations[$index]
                $expectedBlock = 1 + [int][Math]::Floor(
                    $index / $blockSize
                )
                if (
                    (Get-PropertyValue $observation 'terminal') -ne
                        'kettle' -or
                    (Get-PropertyValue $observation 'metric') -ne
                        'menu_hover_ms' -or
                    (As-NonnegativeInt (
                        Get-PropertyValue $observation 'sample_id'
                    )) -ne ($index + 1) -or
                    (As-NonnegativeInt (
                        Get-PropertyValue $observation 'sequence'
                    )) -ne ($index + 1) -or
                    (As-NonnegativeInt (
                        Get-PropertyValue $observation 'block_id'
                    )) -ne $expectedBlock -or
                    (Get-PropertyValue $observation 'status') -ne 'ok' -or
                    $null -eq (As-Double (
                        Get-PropertyValue $observation 'value'
                    )) -or
                    [Math]::Abs(
                        (As-Double (
                            Get-PropertyValue $observation 'value'
                        )) -
                        (As-Double $raw[$index])
                    ) -gt 0.011 -or
                    (As-NonnegativeInt (
                        Get-PropertyValue $observation 'poll_count'
                    )) -lt 1 -or
                    $null -eq (As-Double (
                        Get-PropertyValue $observation 'baseline_capture_ms'
                    ) -AllowZero) -or
                    $null -eq (As-Double (
                        Get-PropertyValue $observation 'poll_capture_ms'
                    ) -AllowZero)
                ) {
                    $observationsValid = $false
                    break
                }
            }
        }
    }
    return (
        $null -ne $samples -and
        $null -ne $requested -and
        $null -ne $misses -and
        $null -ne $longFrames -and
        $null -ne $p95 -and
        $null -ne $p99 -and
        $null -ne $recordedLongFrameMs -and
        $samples -ge $MinimumSamples -and
        $requested -ge $MinimumSamples -and
        $samples + $misses -eq $requested -and
        $raw.Count -eq $samples -and
        $validRaw.Count -eq $samples -and
        $misses -eq 0 -and
        [Math]::Abs($p95 - $calculatedP95) -le 0.011 -and
        [Math]::Abs($p99 - $calculatedP99) -le 0.011 -and
        $p95 -le $MaxP95 -and
        $p99 -le $MaxP99 -and
        [Math]::Abs($recordedLongFrameMs - $LongFrameMs) -le 0.001 -and
        $longFrames -eq $calculatedLongFrames -and
        $longFrames -le $MaxLongFrames -and
        $observationsValid -and
        (Get-PropertyValue $Menu 'passed') -eq $true
    )
}

function ConvertTo-KettlePerfMonitorTransitionJson {
    param(
        $Value,
        [int]$Depth = 16
    )

    return ConvertTo-Json -InputObject $Value -Compress -Depth $Depth
}

function ConvertTo-KettlePerfMonitorTransitionInt {
    param($Value)

    if ($null -eq $Value) {
        return $null
    }
    try {
        $number = [double]$Value
        if (
            [double]::IsNaN($number) -or
            [double]::IsInfinity($number) -or
            [Math]::Truncate($number) -ne $number -or
            $number -lt [int]::MinValue -or
            $number -gt [int]::MaxValue
        ) {
            return $null
        }
        return [int]$number
    } catch {
        return $null
    }
}

function Get-KettlePerfMonitorTransitionHardwareId {
    param([string]$DeviceOrInstanceId)

    if ([string]::IsNullOrWhiteSpace($DeviceOrInstanceId)) {
        return $null
    }
    $match = [regex]::Match(
        $DeviceOrInstanceId,
        '^(?:(?:MONITOR|DISPLAY)\\|\\\\\?\\DISPLAY#)' +
            '(?<hardware>[A-Z0-9_-]{1,64})(?:\\|#)',
        [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor
            [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if (-not $match.Success) {
        return $null
    }
    return $match.Groups['hardware'].Value
}

function Get-KettlePerfDisplayIdentityEvidenceIssue {
    param(
        $Topology,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $acquisition = Get-PropertyValue $Topology 'identity_acquisition'
    $screens = @(Get-PropertyValue $Topology 'desktop_screens')
    $monitors = @(Get-PropertyValue $Topology 'active_physical_monitors')
    $connections = @(Get-PropertyValue $Topology 'active_connections')
    $reportedIssues = @(
        Get-PropertyValue $Topology 'identity_issues' |
            Where-Object { $null -ne $_ }
    )
    $desktopCount = As-NonnegativeInt (
        Get-PropertyValue $acquisition 'desktop_screen_count'
    )
    $wmiMonitorCount = As-NonnegativeInt (
        Get-PropertyValue $acquisition 'wmi_active_monitor_count'
    )
    $wmiConnectionCount = As-NonnegativeInt (
        Get-PropertyValue $acquisition 'wmi_active_connection_count'
    )
    $ccdPathCount = As-NonnegativeInt (
        Get-PropertyValue $acquisition 'ccd_active_path_count'
    )
    $resolvedCount = As-NonnegativeInt (
        Get-PropertyValue $acquisition 'resolved_screen_count'
    )
    $ccdStatus = [string](Get-PropertyValue $acquisition 'ccd_status')
    if (
        $null -eq $acquisition -or
        (Get-PropertyValue $acquisition 'schema') -cne
            'kettle-display-identity-acquisition-v1' -or
        (Get-PropertyValue $acquisition 'resolver') -cne
            'wmi-monitor-id-with-ccd-registry-fallback-v1' -or
        (
            $ccdStatus -cne 'available' -and
            $ccdStatus -cne 'unavailable'
        ) -or
        $null -eq $desktopCount -or $desktopCount -ne $screens.Count -or
        $null -eq $wmiMonitorCount -or
        $null -eq $wmiConnectionCount -or
        $null -eq $ccdPathCount -or
        ($ccdStatus -ceq 'unavailable' -and $ccdPathCount -ne 0) -or
        $null -eq $resolvedCount -or $resolvedCount -ne $monitors.Count -or
        $connections.Count -gt $resolvedCount -or
        $reportedIssues.Count -ne 0
    ) {
        $issues.Add("${Prefix}display identity acquisition contract is invalid")
        return $issues
    }

    $identitySources = @(
        $monitors |
            ForEach-Object {
                [string](Get-PropertyValue $_ 'identity_source')
            } |
            Sort-Object -Unique
    )
    $expectedMethod = if ($identitySources.Count -eq 0) {
        'none'
    } elseif (
        $identitySources.Count -eq 1 -and
        $identitySources[0] -in @(
            'wmi-monitor-id-v1',
            'display-config-ccd-registry-edid-v1'
        )
    ) {
        $identitySources[0]
    } elseif (
        $identitySources.Count -eq 2 -and
        $identitySources -contains 'wmi-monitor-id-v1' -and
        $identitySources -contains 'display-config-ccd-registry-edid-v1'
    ) {
        'hybrid-wmi-monitor-id-and-display-config-ccd-v1'
    } else {
        $null
    }
    if (
        $null -eq $expectedMethod -or
        (Get-PropertyValue $acquisition 'method') -cne $expectedMethod
    ) {
        $issues.Add("${Prefix}display identity acquisition method is invalid")
    }

    $monitorInstances = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $wmiResolvedCount = 0
    $ccdResolvedCount = 0
    $monitorEvidenceValid = $true
    foreach ($monitor in $monitors) {
        $source = [string](Get-PropertyValue $monitor 'identity_source')
        $instance = [string](Get-PropertyValue $monitor 'instance_name')
        $hardware = [string](Get-PropertyValue $monitor 'hardware_id')
        if (
            [string]::IsNullOrWhiteSpace($instance) -or
            [string]::IsNullOrWhiteSpace($hardware) -or
            -not $monitorInstances.Add($instance) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                (Get-KettlePerfMonitorTransitionHardwareId $instance),
                $hardware
            )
        ) {
            $monitorEvidenceValid = $false
            continue
        }
        if ($source -ceq 'wmi-monitor-id-v1') {
            $wmiResolvedCount++
            continue
        }
        if ($source -cne 'display-config-ccd-registry-edid-v1') {
            $monitorEvidenceValid = $false
            continue
        }
        $ccdResolvedCount++
        $devicePath = [string](
            Get-PropertyValue $monitor 'monitor_device_path'
        )
        $pathHardware = Get-KettlePerfMonitorTransitionHardwareId $devicePath
        $registryHash = [string](
            Get-PropertyValue $monitor 'registry_edid_sha256'
        )
        $registryBlocks = As-NonnegativeInt (
            Get-PropertyValue $monitor 'registry_edid_block_count'
        )
        if (
            $ccdStatus -cne 'available' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $pathHardware,
                $hardware
            ) -or
            $devicePath -cnotmatch
                '^\\\\\?\\DISPLAY#[A-Z0-9_-]{1,64}#[^#\\]{1,128}#\{' +
                    'e6f07b5f-ee97-4a90-b076-33f57bf4eaa7\}$' -or
            $registryHash -cnotmatch '^[0-9a-f]{64}$' -or
            $null -eq $registryBlocks -or $registryBlocks -lt 1 -or
            $registryBlocks -gt 32
        ) {
            $monitorEvidenceValid = $false
        }
    }
    if (
        -not $monitorEvidenceValid -or
        $wmiResolvedCount -gt $wmiMonitorCount -or
        $ccdResolvedCount -gt $ccdPathCount
    ) {
        $issues.Add("${Prefix}display identity monitor evidence is invalid")
    }

    $resolvedScreenCount = 0
    $screenNames = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $screenEvidenceValid = $true
    foreach ($screen in $screens) {
        $deviceName = [string](Get-PropertyValue $screen 'device_name')
        $edidBacked = Get-PropertyValue $screen 'edid_backed'
        $matchCount = As-NonnegativeInt (
            Get-PropertyValue $screen 'edid_match_count'
        )
        $edid = Get-PropertyValue $screen 'edid_monitor'
        if (
            [string]::IsNullOrWhiteSpace($deviceName) -or
            -not $screenNames.Add($deviceName)
        ) {
            $screenEvidenceValid = $false
        }
        if ($edidBacked -eq $true) {
            $resolvedScreenCount++
            $edidInstance = [string](
                Get-PropertyValue $edid 'instance_name'
            )
            if (
                $matchCount -ne 1 -or
                $null -eq $edid -or
                -not $monitorInstances.Contains($edidInstance)
            ) {
                $screenEvidenceValid = $false
            }
        } elseif ($matchCount -ne 0 -or $null -ne $edid) {
            $screenEvidenceValid = $false
        }
    }
    if (
        -not $screenEvidenceValid -or
        $resolvedScreenCount -ne $resolvedCount
    ) {
        $issues.Add("${Prefix}display identity screen mapping is invalid")
    }
    return $issues
}

function Get-KettlePerfMonitorTransitionTopologySignature {
    param($Topology)

    if ($null -eq $Topology) {
        return $null
    }
    return ConvertTo-KettlePerfMonitorTransitionJson ([ordered]@{
        identity_acquisition = Get-PropertyValue `
            $Topology 'identity_acquisition'
        identity_issues = @(
            Get-PropertyValue $Topology 'identity_issues'
        )
        requested_client = Get-PropertyValue $Topology 'requested_client'
        desktop_screens = @(
            Get-PropertyValue $Topology 'desktop_screens'
        )
        active_physical_monitors = @(
            Get-PropertyValue $Topology 'active_physical_monitors'
        )
        active_connections = @(
            Get-PropertyValue $Topology 'active_connections'
        )
    })
}

function Get-KettlePerfMonitorTransitionEndpoint {
    param($Screen)

    $edid = Get-PropertyValue $Screen 'edid_monitor'
    return [pscustomobject][ordered]@{
        device_name = [string](Get-PropertyValue $Screen 'device_name')
        monitor_device_id = [string](
            Get-PropertyValue $Screen 'monitor_device_id'
        )
        monitor_hardware_id = [string](
            Get-PropertyValue $Screen 'monitor_hardware_id'
        )
        edid_instance_name = if ($null -ne $edid) {
            [string](Get-PropertyValue $edid 'instance_name')
        } else {
            $null
        }
        friendly_name = if ($null -ne $edid) {
            [string](Get-PropertyValue $edid 'friendly_name')
        } else {
            $null
        }
        serial_number = if ($null -ne $edid) {
            [string](Get-PropertyValue $edid 'serial_number')
        } else {
            $null
        }
        effective_dpi = Get-PropertyValue $Screen 'effective_dpi'
        scale_factor = Get-PropertyValue $Screen 'scale_factor'
        refresh_hz = Get-PropertyValue $Screen 'refresh_hz'
        bounds = Get-PropertyValue $Screen 'bounds'
        working_area = Get-PropertyValue $Screen 'working_area'
        requested_client_fits = [bool](
            Get-PropertyValue $Screen 'requested_client_fits'
        )
    }
}

function Get-KettlePerfMonitorTransitionOrderedScreens {
    param([object[]]$Screens)

    $ordered = [Collections.Generic.List[object]]::new()
    foreach ($screen in $Screens) {
        $insertAt = $ordered.Count
        for ($index = 0; $index -lt $ordered.Count; $index++) {
            if (
                [StringComparer]::OrdinalIgnoreCase.Compare(
                    [string](Get-PropertyValue $screen 'device_name'),
                    [string](
                        Get-PropertyValue $ordered[$index] 'device_name'
                    )
                ) -lt 0
            ) {
                $insertAt = $index
                break
            }
        }
        $ordered.Insert($insertAt, $screen)
    }
    return [object[]]$ordered.ToArray()
}

function Test-KettlePerfMonitorTransitionScreenEligible {
    param(
        $Screen,
        $Topology
    )

    $deviceName = [string](Get-PropertyValue $Screen 'device_name')
    $deviceId = [string](Get-PropertyValue $Screen 'monitor_device_id')
    $hardwareId = [string](
        Get-PropertyValue $Screen 'monitor_hardware_id'
    )
    $dpi = Get-PropertyValue $Screen 'effective_dpi'
    $dpiX = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $dpi 'x'
    )
    $dpiY = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $dpi 'y'
    )
    $refresh = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $Screen 'refresh_hz'
    )
    $bounds = Get-PropertyValue $Screen 'bounds'
    $working = Get-PropertyValue $Screen 'working_area'
    $boundsWidth = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $bounds 'width'
    )
    $boundsHeight = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $bounds 'height'
    )
    $workingWidth = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $working 'width'
    )
    $workingHeight = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $working 'height'
    )
    $requested = Get-PropertyValue $Topology 'requested_client'
    $allowance = Get-PropertyValue $requested 'non_client_allowance'
    $requestedWidth = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $requested 'width'
    )
    $requestedHeight = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $requested 'height'
    )
    $allowanceWidth = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $allowance 'width'
    )
    $allowanceHeight = ConvertTo-KettlePerfMonitorTransitionInt (
        Get-PropertyValue $allowance 'height'
    )
    $edid = Get-PropertyValue $Screen 'edid_monitor'
    $edidInstance = [string](
        Get-PropertyValue $edid 'instance_name'
    )
    $deviceHardwareId = Get-KettlePerfMonitorTransitionHardwareId $deviceId
    $physicalMatches = @(
        Get-PropertyValue $Topology 'active_physical_monitors' |
            Where-Object {
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string](Get-PropertyValue $_ 'hardware_id'),
                    $hardwareId
                )
            }
    )
    $scale = As-Double (
        Get-PropertyValue $Screen 'scale_factor'
    )
    $expectedScale = if ($null -ne $dpiX -and $dpiX -gt 0) {
        [Math]::Round(([double]$dpiX / 96.0), 4)
    } else {
        $null
    }
    return (
        -not [string]::IsNullOrWhiteSpace($deviceName) -and
        -not [string]::IsNullOrWhiteSpace($deviceId) -and
        -not [string]::IsNullOrWhiteSpace($hardwareId) -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            $deviceHardwareId,
            $hardwareId
        ) -and
        (Get-PropertyValue $Screen 'edid_backed') -eq $true -and
        (As-NonnegativeInt (
            Get-PropertyValue $Screen 'edid_match_count'
        )) -eq 1 -and
        $null -ne $edid -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $edid 'hardware_id'),
            $hardwareId
        ) -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            (Get-KettlePerfMonitorTransitionHardwareId $edidInstance),
            $hardwareId
        ) -and
        $physicalMatches.Count -eq 1 -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $physicalMatches[0] 'instance_name'),
            $edidInstance
        ) -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            (
                Get-KettlePerfMonitorTransitionHardwareId (
                    [string](Get-PropertyValue `
                        $physicalMatches[0] 'instance_name')
                )
            ),
            $hardwareId
        ) -and
        $null -ne $dpiX -and $dpiX -gt 0 -and
        $null -ne $dpiY -and $dpiY -gt 0 -and
        $null -ne $refresh -and $refresh -gt 0 -and
        $null -ne $boundsWidth -and $boundsWidth -gt 0 -and
        $null -ne $boundsHeight -and $boundsHeight -gt 0 -and
        $null -ne $workingWidth -and $workingWidth -gt 0 -and
        $null -ne $workingHeight -and $workingHeight -gt 0 -and
        $null -ne $requestedWidth -and $requestedWidth -gt 0 -and
        $null -ne $requestedHeight -and $requestedHeight -gt 0 -and
        $null -ne $allowanceWidth -and $allowanceWidth -ge 0 -and
        $null -ne $allowanceHeight -and $allowanceHeight -ge 0 -and
        $workingWidth -ge ($requestedWidth + $allowanceWidth) -and
        $workingHeight -ge ($requestedHeight + $allowanceHeight) -and
        (Get-PropertyValue $Screen 'requested_client_fits') -eq $true -and
        $null -ne $scale -and
        [Math]::Abs($scale - $expectedScale) -le 0.00001
    )
}

function Get-KettlePerfMonitorTransitionPairContrast {
    param(
        $First,
        $Second
    )

    $firstDpi = Get-PropertyValue $First 'effective_dpi'
    $secondDpi = Get-PropertyValue $Second 'effective_dpi'
    $dpiDelta = [Math]::Max(
        [Math]::Abs(
            [int](Get-PropertyValue $firstDpi 'x') -
            [int](Get-PropertyValue $secondDpi 'x')
        ),
        [Math]::Abs(
            [int](Get-PropertyValue $firstDpi 'y') -
            [int](Get-PropertyValue $secondDpi 'y')
        )
    )
    $refreshDelta = [Math]::Abs(
        [int](Get-PropertyValue $First 'refresh_hz') -
        [int](Get-PropertyValue $Second 'refresh_hz')
    )
    $geometryDelta = 0
    foreach ($areaName in @('bounds', 'working_area')) {
        $firstArea = Get-PropertyValue $First $areaName
        $secondArea = Get-PropertyValue $Second $areaName
        foreach ($field in @('width', 'height')) {
            $geometryDelta = [Math]::Max(
                $geometryDelta,
                [Math]::Abs(
                    [int](Get-PropertyValue $firstArea $field) -
                    [int](Get-PropertyValue $secondArea $field)
                )
            )
        }
    }
    $meaningfulDimensions = 0
    if ($dpiDelta -gt 0) {
        $meaningfulDimensions++
    }
    if ($refreshDelta -gt 0) {
        $meaningfulDimensions++
    }
    if ($geometryDelta -gt 0) {
        $meaningfulDimensions++
    }
    $deviceNames = [string[]]@(
        [string](Get-PropertyValue $First 'device_name'),
        [string](Get-PropertyValue $Second 'device_name')
    )
    return [pscustomobject][ordered]@{
        pair_key = $deviceNames -join '|'
        device_names = $deviceNames
        meaningful_dimension_count = $meaningfulDimensions
        dpi_delta = $dpiDelta
        refresh_hz_delta = $refreshDelta
        geometry_delta_pixels = $geometryDelta
    }
}

function Test-KettlePerfMonitorTransitionContrastBetter {
    param(
        $Candidate,
        $Current
    )

    if ($null -eq $Current) {
        return $true
    }
    foreach ($field in @(
        'meaningful_dimension_count',
        'dpi_delta',
        'refresh_hz_delta',
        'geometry_delta_pixels'
    )) {
        $candidateValue = [int](Get-PropertyValue $Candidate $field)
        $currentValue = [int](Get-PropertyValue $Current $field)
        if ($candidateValue -ne $currentValue) {
            return $candidateValue -gt $currentValue
        }
    }
    return (
        [StringComparer]::OrdinalIgnoreCase.Compare(
            [string](Get-PropertyValue $Candidate 'pair_key'),
            [string](Get-PropertyValue $Current 'pair_key')
        ) -lt 0
    )
}

function Get-KettlePerfMonitorTransitionSelectionPolicy {
    param([object[]]$EligibleScreens)

    $candidates = [Collections.Generic.List[object]]::new()
    $selected = $null
    for ($firstIndex = 0; $firstIndex -lt $EligibleScreens.Count; $firstIndex++) {
        for (
            $secondIndex = $firstIndex + 1;
            $secondIndex -lt $EligibleScreens.Count;
            $secondIndex++
        ) {
            $candidate = Get-KettlePerfMonitorTransitionPairContrast `
                $EligibleScreens[$firstIndex] $EligibleScreens[$secondIndex]
            $candidates.Add($candidate)
            if (
                Test-KettlePerfMonitorTransitionContrastBetter `
                    $candidate $selected
            ) {
                $selected = $candidate
            }
        }
    }
    return [pscustomobject][ordered]@{
        algorithm = 'maximum-meaningful-contrast-v1'
        eligible_screen_order = 'device-name-ordinal-ignore-case'
        ranking = [string[]]@(
            'meaningful_dimension_count:descending',
            'dpi_delta:descending',
            'refresh_hz_delta:descending',
            'geometry_delta_pixels:descending',
            'pair_key:ordinal-ignore-case-ascending'
        )
        geometry_definition = (
            'maximum absolute width/height delta across bounds and ' +
            'working-area sizes; desktop coordinates are excluded'
        )
        eligible_screen_device_names = [string[]]@(
            $EligibleScreens | ForEach-Object {
                [string](Get-PropertyValue $_ 'device_name')
            }
        )
        candidate_pair_count = $candidates.Count
        candidate_pairs = [object[]]$candidates.ToArray()
        selected_pair_key = if ($null -ne $selected) {
            [string](Get-PropertyValue $selected 'pair_key')
        } else {
            $null
        }
        selected_device_names = if ($null -ne $selected) {
            [string[]](Get-PropertyValue $selected 'device_names')
        } else {
            [string[]]@()
        }
        selected_contrast = $selected
    }
}

function Get-KettlePerfMonitorTransitionDisplayScreenSignature {
    param($Screen)

    return ConvertTo-KettlePerfMonitorTransitionJson ([ordered]@{
        device_name = Get-PropertyValue $Screen 'device_name'
        monitor_device_id = Get-PropertyValue $Screen 'monitor_device_id'
        primary = Get-PropertyValue $Screen 'primary'
        effective_dpi = Get-PropertyValue $Screen 'effective_dpi'
        refresh_hz = Get-PropertyValue $Screen 'refresh_hz'
        bounds = Get-PropertyValue $Screen 'bounds'
        working_area = Get-PropertyValue $Screen 'working_area'
    })
}

function Get-KettlePerfMonitorTransitionExactIssues {
    param(
        $Transition,
        $BenchmarkManifest,
        [int]$MinimumSamplesPerState,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    if ($null -eq $Transition) {
        return $issues
    }
    try {
        $requested = Get-PropertyValue $Transition 'requested'
        $settings = Get-PropertyValue $BenchmarkManifest 'settings'
        $expectedSamplesPerState = As-NonnegativeInt (
            Get-PropertyValue `
                $settings 'monitor_transition_samples_per_state'
        )
        $samplesPerState = As-NonnegativeInt (
            Get-PropertyValue $requested 'samples_per_state'
        )
        $requestedStates = @(
            Get-PropertyValue $requested 'states'
        )
        $window = Get-PropertyValue $requested 'window_pixels'
        $windowWidth = As-NonnegativeInt (
            Get-PropertyValue $window 'width'
        )
        $windowHeight = As-NonnegativeInt (
            Get-PropertyValue $window 'height'
        )
        $manifestWindow = Get-PropertyValue $settings 'window_pixels'
        $manifestWidth = As-NonnegativeInt (
            Get-PropertyValue $manifestWindow 'width'
        )
        $manifestHeight = As-NonnegativeInt (
            Get-PropertyValue $manifestWindow 'height'
        )
        $recoveryTimeout = As-NonnegativeInt (
            Get-PropertyValue $requested 'recovery_timeout_ms'
        )
        $stableChecks = As-NonnegativeInt (
            Get-PropertyValue $requested 'geometry_stable_checks'
        )
        $pollMs = As-NonnegativeInt (
            Get-PropertyValue $requested 'poll_ms'
        )
        if (
            $null -eq $expectedSamplesPerState -or
            $expectedSamplesPerState -lt $MinimumSamplesPerState -or
            $expectedSamplesPerState -gt 1000 -or
            $samplesPerState -ne $expectedSamplesPerState -or
            $requestedStates.Count -ne 2 -or
            [string]$requestedStates[0] -cne 'menu_closed' -or
            [string]$requestedStates[1] -cne 'context_menu_open' -or
            $null -eq $windowWidth -or
                $windowWidth -lt 320 -or $windowWidth -gt 16384 -or
            $null -eq $windowHeight -or
                $windowHeight -lt 240 -or $windowHeight -gt 16384 -or
            $windowWidth -ne $manifestWidth -or
            $windowHeight -ne $manifestHeight -or
            $null -eq $recoveryTimeout -or
                $recoveryTimeout -lt 100 -or $recoveryTimeout -gt 60000 -or
            $null -eq $stableChecks -or
                $stableChecks -lt 2 -or $stableChecks -gt 10 -or
            $null -eq $pollMs -or
                $pollMs -lt 5 -or $pollMs -gt 1000 -or
            $pollMs -gt $recoveryTimeout
        ) {
            $issues.Add(
                "${Prefix}monitor-transition request contract is invalid"
            )
        }

        $topologyStart = Get-PropertyValue $Transition 'topology_start'
        $topologyEnd = Get-PropertyValue $Transition 'topology_end'
        foreach (
            $identityIssue in Get-KettlePerfDisplayIdentityEvidenceIssue `
                -Topology $topologyStart `
                -Prefix "${Prefix}monitor-transition start "
        ) {
            $issues.Add($identityIssue)
        }
        foreach (
            $identityIssue in Get-KettlePerfDisplayIdentityEvidenceIssue `
                -Topology $topologyEnd `
                -Prefix "${Prefix}monitor-transition end "
        ) {
            $issues.Add($identityIssue)
        }
        $topologyRequested = Get-PropertyValue `
            $topologyStart 'requested_client'
        $topologyAllowance = Get-PropertyValue `
            $topologyRequested 'non_client_allowance'
        if (
            $null -eq $topologyStart -or
            $null -eq $topologyEnd -or
            (Get-KettlePerfMonitorTransitionTopologySignature `
                $topologyStart) -cne (
                    Get-KettlePerfMonitorTransitionTopologySignature `
                        $topologyEnd
                ) -or
            (As-NonnegativeInt (
                Get-PropertyValue $topologyRequested 'width'
            )) -ne $windowWidth -or
            (As-NonnegativeInt (
                Get-PropertyValue $topologyRequested 'height'
            )) -ne $windowHeight -or
            (As-NonnegativeInt (
                Get-PropertyValue $topologyAllowance 'width'
            )) -ne 64 -or
            (As-NonnegativeInt (
                Get-PropertyValue $topologyAllowance 'height'
            )) -ne 96
        ) {
            $issues.Add(
                "${Prefix}monitor-transition topology evidence is inconsistent"
            )
        }

        $topologyScreens = @(
            Get-PropertyValue $topologyStart 'desktop_screens'
        )
        $display = Get-PropertyValue (
            Get-PropertyValue $BenchmarkManifest 'machine'
        ) 'display_topology'
        $manifestScreens = @(
            Get-PropertyValue $display 'desktop_screens'
        )
        $deviceNames = [Collections.Generic.HashSet[string]]::new(
            [StringComparer]::OrdinalIgnoreCase
        )
        $topologyScreensValid = $topologyScreens.Count -ge 2
        foreach ($screen in $topologyScreens) {
            $deviceName = [string](
                Get-PropertyValue $screen 'device_name'
            )
            if (-not $deviceName -or -not $deviceNames.Add($deviceName)) {
                $topologyScreensValid = $false
            }
        }
        $topologyDisplaySignature = ConvertTo-KettlePerfMonitorTransitionJson @(
            Get-KettlePerfMonitorTransitionOrderedScreens $topologyScreens |
                ForEach-Object {
                    Get-KettlePerfMonitorTransitionDisplayScreenSignature $_
                }
        )
        $manifestDisplaySignature = ConvertTo-KettlePerfMonitorTransitionJson @(
            Get-KettlePerfMonitorTransitionOrderedScreens $manifestScreens |
                ForEach-Object {
                    Get-KettlePerfMonitorTransitionDisplayScreenSignature $_
                }
        )
        if (
            $topologyScreens.Count -ne $manifestScreens.Count -or
            $topologyDisplaySignature -cne $manifestDisplaySignature
        ) {
            $issues.Add(
                "${Prefix}monitor-transition desktop screens differ from the run display topology"
            )
        }
        $eligibleScreens = @(
            Get-KettlePerfMonitorTransitionOrderedScreens @(
                $topologyScreens | Where-Object {
                    Test-KettlePerfMonitorTransitionScreenEligible `
                        $_ $topologyStart
                }
            )
        )
        if (-not $topologyScreensValid -or $eligibleScreens.Count -lt 2) {
            $issues.Add(
                "${Prefix}monitor-transition has fewer than two independently eligible screens"
            )
        }

        $expectedPolicy = Get-KettlePerfMonitorTransitionSelectionPolicy `
            $eligibleScreens
        $reportedPolicy = Get-PropertyValue `
            $Transition 'selection_policy'
        if (
            (ConvertTo-KettlePerfMonitorTransitionJson $reportedPolicy) -cne
            (ConvertTo-KettlePerfMonitorTransitionJson $expectedPolicy)
        ) {
            $issues.Add(
                "${Prefix}monitor-transition selection policy or contrast evidence is invalid"
            )
        }

        $selectedScreens = @(
            Get-PropertyValue $Transition 'selected_screens'
        )
        $selectedTopologyScreens = [Collections.Generic.List[object]]::new()
        if ($selectedScreens.Count -eq 2) {
            for ($index = 0; $index -lt 2; $index++) {
                $expectedDevice = [string](
                    $expectedPolicy.selected_device_names[$index]
                )
                $matches = @(
                    $eligibleScreens | Where-Object {
                        [StringComparer]::OrdinalIgnoreCase.Equals(
                            [string](
                                Get-PropertyValue $_ 'device_name'
                            ),
                            $expectedDevice
                        )
                    }
                )
                if (
                    $matches.Count -ne 1 -or
                    (ConvertTo-KettlePerfMonitorTransitionJson `
                        $selectedScreens[$index]) -cne (
                            ConvertTo-KettlePerfMonitorTransitionJson (
                                Get-KettlePerfMonitorTransitionEndpoint `
                                    $matches[0]
                            )
                        )
                ) {
                    $selectedTopologyScreens.Clear()
                    break
                }
                $selectedTopologyScreens.Add($matches[0])
            }
        }
        if ($selectedTopologyScreens.Count -ne 2) {
            $issues.Add(
                "${Prefix}monitor-transition selected pair is not the independently ranked pair"
            )
        } else {
            foreach ($screen in $selectedTopologyScreens) {
                $deviceName = [string](
                    Get-PropertyValue $screen 'device_name'
                )
                $manifestMatches = @(
                    $manifestScreens | Where-Object {
                        [StringComparer]::OrdinalIgnoreCase.Equals(
                            [string](
                                Get-PropertyValue $_ 'device_name'
                            ),
                            $deviceName
                        )
                    }
                )
                if (
                    $manifestMatches.Count -ne 1 -or
                    (Get-KettlePerfMonitorTransitionDisplayScreenSignature `
                        $screen) -cne (
                            Get-KettlePerfMonitorTransitionDisplayScreenSignature `
                                $manifestMatches[0]
                        )
                ) {
                    $issues.Add(
                        "${Prefix}monitor-transition selected pair differs from the run display topology"
                    )
                    break
                }
            }
        }

        $expectedTotal = $expectedSamplesPerState * 2
        $observations = @(
            Get-PropertyValue $Transition 'observations'
        )
        if (
            (As-NonnegativeInt (
                Get-PropertyValue $Transition 'requested_samples'
            )) -ne $expectedTotal -or
            (As-NonnegativeInt (
                Get-PropertyValue $Transition 'samples'
            )) -ne $expectedTotal -or
            (As-NonnegativeInt (
                Get-PropertyValue $Transition 'misses'
            )) -ne 0 -or
            $observations.Count -ne $expectedTotal
        ) {
            $issues.Add(
                "${Prefix}monitor-transition exact sample coverage is invalid"
            )
        }

        $rawValues = [ordered]@{
            menu_closed = @{}
            context_menu_open = @{}
        }
        $keys = [Collections.Generic.HashSet[string]]::new(
            [StringComparer]::Ordinal
        )
        $rawCoverageAndDirectionValid = (
            $selectedTopologyScreens.Count -eq 2 -and
            $null -ne $expectedSamplesPerState
        )
        $rawDpiAndRefreshValid = $rawCoverageAndDirectionValid
        $rawCaptureAndSurfaceValid = $rawCoverageAndDirectionValid
        $rawMenuValid = $rawCoverageAndDirectionValid
        $rawGeometryChecksValid = $rawCoverageAndDirectionValid
        foreach ($observation in $observations) {
            $state = [string](
                Get-PropertyValue $observation 'state'
            )
            $sample = As-NonnegativeInt (
                Get-PropertyValue $observation 'sample'
            )
            $stateIndex = [array]::IndexOf(
                [string[]]@('menu_closed', 'context_menu_open'),
                $state
            )
            $key = "$state/$sample"
            if (
                $stateIndex -lt 0 -or
                $null -eq $sample -or
                $sample -ge $expectedSamplesPerState -or
                -not $keys.Add($key) -or
                $selectedTopologyScreens.Count -ne 2
            ) {
                $rawCoverageAndDirectionValid = $false
                continue
            }
            $globalIndex = ($stateIndex * $expectedSamplesPerState) + $sample
            $sourceIndex = $globalIndex % 2
            $targetIndex = if ($sourceIndex -eq 0) { 1 } else { 0 }
            $expectedSourceScreen = $selectedTopologyScreens[$sourceIndex]
            $expectedTargetScreen = $selectedTopologyScreens[$targetIndex]
            $expectedSource = Get-KettlePerfMonitorTransitionEndpoint `
                $expectedSourceScreen
            $expectedTarget = Get-KettlePerfMonitorTransitionEndpoint `
                $expectedTargetScreen
            $source = Get-PropertyValue $observation 'source'
            $target = Get-PropertyValue $observation 'target'
            $sourceDevice = [string](
                Get-PropertyValue $expectedSource 'device_name'
            )
            $targetDevice = [string](
                Get-PropertyValue $expectedTarget 'device_name'
            )
            $value = As-Double (
                Get-PropertyValue `
                    $observation 'recovery_to_capturable_client_ms'
            )
            $capture = Get-PropertyValue $observation 'capture'
            $surface = Get-PropertyValue `
                $observation 'ui_geometry_surface'
            $observedDpi = Get-PropertyValue `
                $observation 'target_effective_dpi_observed'
            $expectedDpi = Get-PropertyValue `
                $expectedTarget 'effective_dpi'
            $contextMenu = Get-PropertyValue `
                $observation 'context_menu'
            $menuRows = As-NonnegativeInt (
                Get-PropertyValue $contextMenu 'rows'
            )
            $menuRect = Get-PropertyValue $contextMenu 'rect'
            $menuValid = if ($state -ceq 'menu_closed') {
                (Get-PropertyValue $contextMenu 'open') -eq $false -and
                $null -eq $menuRect -and
                $menuRows -eq 0
            } else {
                (Get-PropertyValue $contextMenu 'open') -eq $true -and
                $null -ne $menuRect -and
                $null -ne $menuRows -and
                $menuRows -gt 0 -and
                $null -ne (As-Double (
                    Get-PropertyValue $menuRect 'width'
                )) -and
                $null -ne (As-Double (
                    Get-PropertyValue $menuRect 'height'
                ))
            }
            $expectedBytes = [int64]$windowWidth *
                [int64]$windowHeight * 4
            if (
                (Get-PropertyValue $observation 'status') -cne 'ok' -or
                $null -ne (Get-PropertyValue `
                    $observation 'miss_reason') -or
                $null -eq $value -or
                $value -gt $recoveryTimeout -or
                [string](Get-PropertyValue $observation 'direction') -cne
                    "$sourceDevice->$targetDevice" -or
                (ConvertTo-KettlePerfMonitorTransitionJson $source) -cne
                    (ConvertTo-KettlePerfMonitorTransitionJson `
                        $expectedSource) -or
                (ConvertTo-KettlePerfMonitorTransitionJson $target) -cne
                    (ConvertTo-KettlePerfMonitorTransitionJson `
                        $expectedTarget) -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string](Get-PropertyValue `
                        $observation 'actual_target_device_name'),
                    $targetDevice
                )
            ) {
                $rawCoverageAndDirectionValid = $false
            }
            if (
                (ConvertTo-KettlePerfMonitorTransitionJson $observedDpi) -cne
                    (ConvertTo-KettlePerfMonitorTransitionJson $expectedDpi) -or
                (As-NonnegativeInt (
                    Get-PropertyValue `
                        $observation 'target_refresh_hz_observed'
                )) -ne (As-NonnegativeInt (
                    Get-PropertyValue $expectedTarget 'refresh_hz'
                ))
            ) {
                $rawDpiAndRefreshValid = $false
            }
            if (
                (As-NonnegativeInt (
                    Get-PropertyValue $capture 'width'
                )) -ne $windowWidth -or
                (As-NonnegativeInt (
                    Get-PropertyValue $capture 'height'
                )) -ne $windowHeight -or
                [int64](Get-PropertyValue $capture 'bytes') -ne
                    $expectedBytes -or
                (As-NonnegativeInt (
                    Get-PropertyValue $surface 'width'
                )) -ne $windowWidth -or
                (As-NonnegativeInt (
                    Get-PropertyValue $surface 'height'
                )) -ne $windowHeight
            ) {
                $rawCaptureAndSurfaceValid = $false
            }
            if (-not $menuValid) {
                $rawMenuValid = $false
            }
            if (
                (As-NonnegativeInt (
                    Get-PropertyValue `
                        $observation 'ui_geometry_checks'
                )) -lt $stableChecks
            ) {
                $rawGeometryChecksValid = $false
            }
            $rawValues[$state][$sample] = $value
        }
        foreach ($stateName in @('menu_closed', 'context_menu_open')) {
            for (
                $sample = 0;
                $sample -lt $expectedSamplesPerState;
                $sample++
            ) {
                if (-not $rawValues[$stateName].ContainsKey($sample)) {
                    $rawCoverageAndDirectionValid = $false
                }
            }
        }
        if (-not $rawCoverageAndDirectionValid) {
            $issues.Add(
                "${Prefix}monitor-transition state/sample coverage or direction binding is invalid"
            )
        }
        if (-not $rawDpiAndRefreshValid) {
            $issues.Add(
                "${Prefix}monitor-transition observed DPI or refresh differs from its target"
            )
        }
        if (-not $rawCaptureAndSurfaceValid) {
            $issues.Add(
                "${Prefix}monitor-transition capture or surface geometry is invalid"
            )
        }
        if (-not $rawMenuValid) {
            $issues.Add(
                "${Prefix}monitor-transition menu state is invalid"
            )
        }
        if (-not $rawGeometryChecksValid) {
            $issues.Add(
                "${Prefix}monitor-transition stable geometry check count is invalid"
            )
        }

        $states = Get-PropertyValue $Transition 'states'
        $stateProperties = @(
            if ($null -ne $states) {
                $states.PSObject.Properties | ForEach-Object { $_.Name }
            }
        )
        if (
            $stateProperties.Count -ne 2 -or
            $stateProperties -cnotcontains 'menu_closed' -or
            $stateProperties -cnotcontains 'context_menu_open'
        ) {
            $issues.Add(
                "${Prefix}monitor-transition state summary set is invalid"
            )
        }
        $combinedRaw = [Collections.Generic.List[double]]::new()
        foreach ($stateName in @('menu_closed', 'context_menu_open')) {
            $stateProperty = if ($null -ne $states) {
                $states.PSObject.Properties[$stateName]
            } else {
                $null
            }
            $stateRaw = [Collections.Generic.List[double]]::new()
            for (
                $sample = 0;
                $sample -lt $expectedSamplesPerState;
                $sample++
            ) {
                if ($rawValues[$stateName].ContainsKey($sample)) {
                    $rawValue = As-Double $rawValues[$stateName][$sample]
                    if ($null -ne $rawValue) {
                        $stateRaw.Add($rawValue)
                        $combinedRaw.Add($rawValue)
                    }
                }
            }
            if ($null -eq $stateProperty) {
                continue
            }
            $stateSummary = $stateProperty.Value
            $sorted = [double[]]@(
                $stateRaw.ToArray() | Sort-Object
            )
            $calculatedMedian = if ($sorted.Count) {
                Get-KettlePerfMedian $sorted
            } else {
                $null
            }
            $calculatedP95 = if ($sorted.Count) {
                $sorted[[Math]::Min(
                    $sorted.Count - 1,
                    [Math]::Ceiling($sorted.Count * 0.95) - 1
                )]
            } else {
                $null
            }
            $calculatedMax = if ($sorted.Count) {
                $sorted[$sorted.Count - 1]
            } else {
                $null
            }
            if (
                (As-NonnegativeInt (
                    Get-PropertyValue $stateSummary 'requested_samples'
                )) -ne $expectedSamplesPerState -or
                (As-NonnegativeInt (
                    Get-PropertyValue $stateSummary 'samples'
                )) -ne $expectedSamplesPerState -or
                (As-NonnegativeInt (
                    Get-PropertyValue $stateSummary 'misses'
                )) -ne 0 -or
                -not (Test-KettlePerfNumericArraysEqual `
                    -Left ([object[]]$sorted) `
                    -Right @(
                        Get-PropertyValue $stateSummary `
                            'recovery_to_capturable_client_ms_all'
                    )) -or
                [Math]::Abs(
                    (As-Double (
                        Get-PropertyValue $stateSummary `
                            'recovery_to_capturable_client_ms_median'
                    )) - $calculatedMedian
                ) -gt 0.001 -or
                [Math]::Abs(
                    (As-Double (
                        Get-PropertyValue $stateSummary `
                            'recovery_to_capturable_client_ms_p95'
                    )) - $calculatedP95
                ) -gt 0.001 -or
                [Math]::Abs(
                    (As-Double (
                        Get-PropertyValue $stateSummary `
                            'recovery_to_capturable_client_ms_max'
                    )) - $calculatedMax
                ) -gt 0.001
            ) {
                $issues.Add(
                    "${Prefix}monitor-transition $stateName summary differs from raw observations"
                )
            }
        }
        $combinedSorted = [double[]]@(
            $combinedRaw.ToArray() | Sort-Object
        )
        $combinedMedian = if ($combinedSorted.Count) {
            Get-KettlePerfMedian $combinedSorted
        } else {
            $null
        }
        $combinedP95 = if ($combinedSorted.Count) {
            $combinedSorted[[Math]::Min(
                $combinedSorted.Count - 1,
                [Math]::Ceiling($combinedSorted.Count * 0.95) - 1
            )]
        } else {
            $null
        }
        $combinedMax = if ($combinedSorted.Count) {
            $combinedSorted[$combinedSorted.Count - 1]
        } else {
            $null
        }
        if (
            -not (Test-KettlePerfNumericArraysEqual `
                -Left ([object[]]$combinedSorted) `
                -Right @(
                    Get-PropertyValue $Transition `
                        'recovery_to_capturable_client_ms_all'
                )) -or
            [Math]::Abs(
                (As-Double (
                    Get-PropertyValue $Transition `
                        'recovery_to_capturable_client_ms_median'
                )) - $combinedMedian
            ) -gt 0.001 -or
            [Math]::Abs(
                (As-Double (
                    Get-PropertyValue $Transition `
                        'recovery_to_capturable_client_ms_p95'
                )) - $combinedP95
            ) -gt 0.001 -or
            [Math]::Abs(
                (As-Double (
                    Get-PropertyValue $Transition `
                        'recovery_to_capturable_client_ms_max'
                )) - $combinedMax
            ) -gt 0.001
        ) {
            $issues.Add(
                "${Prefix}monitor-transition combined summary differs from raw observations"
            )
        }
    } catch {
        $issues.Add(
            "${Prefix}monitor-transition exact evidence validation failed: $($_.Exception.Message)"
        )
    }
    return $issues
}

function Get-KettlePerfMonitorTransitionSummary {
    param(
        $Transition,
        [ValidateSet('combined', 'menu_closed', 'context_menu_open')]
        [string]$Scope
    )

    if ($Scope -ceq 'combined') {
        return $Transition
    }
    $states = Get-PropertyValue $Transition 'states'
    $property = if ($null -ne $states) {
        $states.PSObject.Properties[$Scope]
    } else {
        $null
    }
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

function Get-KettlePerfMonitorTransitionPerformanceIssues {
    param(
        $Transition,
        [double]$MaximumP95Ms,
        [double]$MaximumMaxMs,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    foreach ($scope in @(
        'combined',
        'menu_closed',
        'context_menu_open'
    )) {
        $summary = Get-KettlePerfMonitorTransitionSummary `
            $Transition $scope
        $p95 = As-Double (
            Get-PropertyValue $summary `
                'recovery_to_capturable_client_ms_p95'
        )
        $maximum = As-Double (
            Get-PropertyValue $summary `
                'recovery_to_capturable_client_ms_max'
        )
        if ($null -eq $p95 -or $p95 -gt $MaximumP95Ms) {
            $issues.Add(
                "${Prefix}monitor-transition $scope p95 exceeds the configured limit"
            )
        }
        if ($null -eq $maximum -or $maximum -gt $MaximumMaxMs) {
            $issues.Add(
                "${Prefix}monitor-transition $scope max exceeds the configured limit"
            )
        }
    }
    return $issues
}

function Get-KettlePerfMonitorTransitionBaselineNonInferiority {
    param(
        $Current,
        $Baseline,
        [double]$AbsoluteMarginMs,
        [double]$RelativeMargin
    )

    $comparisons = [Collections.Generic.List[object]]::new()
    $passed = $true
    foreach ($scope in @(
        'combined',
        'menu_closed',
        'context_menu_open'
    )) {
        $currentSummary = Get-KettlePerfMonitorTransitionSummary `
            $Current $scope
        $baselineSummary = Get-KettlePerfMonitorTransitionSummary `
            $Baseline $scope
        foreach ($statistic in @('p95', 'max')) {
            $propertyName = (
                'recovery_to_capturable_client_ms_' + $statistic
            )
            $currentValue = As-Double (
                Get-PropertyValue $currentSummary $propertyName
            )
            $baselineValue = As-Double (
                Get-PropertyValue $baselineSummary $propertyName
            )
            $margin = if ($null -ne $baselineValue) {
                [Math]::Max(
                    $AbsoluteMarginMs,
                    $RelativeMargin * $baselineValue
                )
            } else {
                $null
            }
            $allowedCurrent = if ($null -ne $margin) {
                $baselineValue + $margin
            } else {
                $null
            }
            $comparisonPassed = (
                $null -ne $currentValue -and
                $null -ne $allowedCurrent -and
                $currentValue -le ($allowedCurrent + 0.001)
            )
            if (-not $comparisonPassed) {
                $passed = $false
            }
            $comparisons.Add([pscustomobject][ordered]@{
                scope = $scope
                statistic = $statistic
                current_ms = $currentValue
                baseline_ms = $baselineValue
                absolute_margin_ms = $AbsoluteMarginMs
                relative_margin_component_ms = if (
                    $null -ne $baselineValue
                ) {
                    $RelativeMargin * $baselineValue
                } else {
                    $null
                }
                practical_margin_ms = $margin
                maximum_non_inferior_current_ms = $allowedCurrent
                passed = $comparisonPassed
            })
        }
    }
    return [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'monitor-transition-summary-noninferiority-v1'
        direction = 'lower'
        required_scopes = [string[]]@(
            'combined',
            'menu_closed',
            'context_menu_open'
        )
        required_statistics = [string[]]@('p95', 'max')
        absolute_margin_ms = $AbsoluteMarginMs
        relative_margin = $RelativeMargin
        practical_margin_rule = (
            'max(absolute_ms, relative * baseline_ms)'
        )
        uncertainty_is_pass = $false
        comparisons = [object[]]$comparisons.ToArray()
        passed = $passed
    }
}

function Get-MonitorTransitionIssues {
    param(
        $Transition,
        $BenchmarkManifest,
        [int]$MinimumSamplesPerState,
        [string]$Prefix = '',
        [double]$MaximumP95Ms = 1000.0,
        [double]$MaximumMaxMs = 2000.0
    )

    $issues = [Collections.Generic.List[string]]::new()
    if ($null -eq $Transition) {
        $issues.Add("${Prefix}monitor-transition result is missing or invalid")
        return $issues
    }
    foreach (
        $issue in Get-KettlePerfMonitorTransitionExactIssues `
            $Transition $BenchmarkManifest $MinimumSamplesPerState $Prefix
    ) {
        $issues.Add($issue)
    }
    foreach (
        $issue in Get-KettlePerfMonitorTransitionPerformanceIssues `
            $Transition $MaximumP95Ms $MaximumMaxMs $Prefix
    ) {
        $issues.Add($issue)
    }
    if (
        (As-NonnegativeInt (
            Get-PropertyValue $Transition 'schema_version'
        )) -ne 2 -or
        (Get-PropertyValue $Transition 'status') -ne 'passed' -or
        (Get-PropertyValue $Transition 'release_evidence_valid') -ne $true -or
        (Get-PropertyValue $Transition 'topology_stable') -ne $true -or
        (Get-PropertyValue $Transition 'metric_name') -ne
            'recovery_to_capturable_client_ms'
    ) {
        $issues.Add("${Prefix}monitor-transition did not pass its evidence contract")
    }
    $manifestRunId = [string](
        Get-PropertyValue $BenchmarkManifest 'run_id'
    )
    if (
        -not $manifestRunId -or
        [string](Get-PropertyValue $Transition 'run_id') -ne $manifestRunId
    ) {
        $issues.Add("${Prefix}monitor-transition belongs to a different run")
    }
    $selectedScreens = @(
        Get-PropertyValue $Transition 'selected_screens'
    )
    if (
        $selectedScreens.Count -ne 2 -or
        @($selectedScreens | ForEach-Object {
            [string](Get-PropertyValue $_ 'device_name')
        } | Select-Object -Unique).Count -ne 2
    ) {
        $issues.Add("${Prefix}monitor-transition has no unique screen pair")
    }

    $observations = @(
        Get-PropertyValue $Transition 'observations'
    )
    $reportedSamples = As-NonnegativeInt (
        Get-PropertyValue $Transition 'samples'
    )
    $reportedRequested = As-NonnegativeInt (
        Get-PropertyValue $Transition 'requested_samples'
    )
    $reportedMisses = As-NonnegativeInt (
        Get-PropertyValue $Transition 'misses'
    )
    $expectedTotal = $MinimumSamplesPerState * 2
    if (
        $null -eq $reportedSamples -or
        $null -eq $reportedRequested -or
        $reportedRequested -lt $expectedTotal -or
        $reportedSamples -ne $reportedRequested -or
        $reportedMisses -ne 0 -or
        $observations.Count -ne $reportedSamples
    ) {
        $issues.Add("${Prefix}monitor-transition sample coverage is incomplete")
    }
    $observationKeys = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    foreach ($observation in $observations) {
        $state = [string](Get-PropertyValue $observation 'state')
        $sample = As-NonnegativeInt (
            Get-PropertyValue $observation 'sample'
        )
        $source = Get-PropertyValue $observation 'source'
        $target = Get-PropertyValue $observation 'target'
        $sourceDevice = [string](
            Get-PropertyValue $source 'device_name'
        )
        $targetDevice = [string](
            Get-PropertyValue $target 'device_name'
        )
        $actualDevice = [string](
            Get-PropertyValue $observation 'actual_target_device_name'
        )
        $value = As-Double (
            Get-PropertyValue `
                $observation 'recovery_to_capturable_client_ms'
        )
        $key = "$state/$sample"
        if (
            $state -notin @('menu_closed', 'context_menu_open') -or
            $null -eq $sample -or
            -not $observationKeys.Add($key) -or
            (Get-PropertyValue $observation 'status') -ne 'ok' -or
            $null -eq $value -or
            -not $sourceDevice -or
            -not $targetDevice -or
            [StringComparer]::OrdinalIgnoreCase.Equals(
                $sourceDevice,
                $targetDevice
            ) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $targetDevice,
                $actualDevice
            )
        ) {
            $issues.Add(
                "${Prefix}monitor-transition has an invalid raw observation"
            )
            break
        }
    }

    $states = Get-PropertyValue $Transition 'states'
    $combinedValues = @()
    foreach ($stateName in @('menu_closed', 'context_menu_open')) {
        $property = if ($states) {
            $states.PSObject.Properties[$stateName]
        } else {
            $null
        }
        if (-not $property) {
            $issues.Add(
                "${Prefix}monitor-transition has no $stateName summary"
            )
            continue
        }
        $state = $property.Value
        $values = @(
            Get-PropertyValue `
                $state 'recovery_to_capturable_client_ms_all'
        )
        $validValues = @(
            $values | Where-Object { $null -ne (As-Double $_) }
        )
        $sorted = @($validValues | Sort-Object)
        $stateSamples = As-NonnegativeInt (
            Get-PropertyValue $state 'samples'
        )
        $stateRequested = As-NonnegativeInt (
            Get-PropertyValue $state 'requested_samples'
        )
        $stateMisses = As-NonnegativeInt (
            Get-PropertyValue $state 'misses'
        )
        $median = As-Double (
            Get-PropertyValue `
                $state 'recovery_to_capturable_client_ms_median'
        )
        $p95 = As-Double (
            Get-PropertyValue `
                $state 'recovery_to_capturable_client_ms_p95'
        )
        $calculatedMedian = if ($sorted.Count) {
            Get-KettlePerfMedian $sorted
        } else {
            $null
        }
        $calculatedP95 = if ($sorted.Count) {
            $sorted[[Math]::Min(
                $sorted.Count - 1,
                [Math]::Ceiling($sorted.Count * 0.95) - 1
            )]
        } else {
            $null
        }
        if (
            $null -eq $stateSamples -or
            $null -eq $stateRequested -or
            $stateRequested -lt $MinimumSamplesPerState -or
            $stateSamples -ne $stateRequested -or
            $stateMisses -ne 0 -or
            $values.Count -ne $stateSamples -or
            $validValues.Count -ne $stateSamples -or
            $null -eq $median -or
            $null -eq $p95 -or
            [Math]::Abs($median - $calculatedMedian) -gt 0.001 -or
            [Math]::Abs($p95 - $calculatedP95) -gt 0.001
        ) {
            $issues.Add(
                "${Prefix}monitor-transition $stateName summary is invalid"
            )
        }
        $combinedValues += $validValues
    }

    $binary = Get-PropertyValue $Transition 'binary'
    $kettleRecords = @(
        Get-PropertyValue $BenchmarkManifest 'terminals' |
            Where-Object {
                (Get-PropertyValue $_ 'name') -eq 'kettle'
            }
    )
    if ($kettleRecords.Count -ne 1) {
        $issues.Add(
            "${Prefix}monitor-transition has no unique Kettle manifest record"
        )
    } else {
        $record = $kettleRecords[0]
        $configHash = [string](
            Get-PropertyValue $BenchmarkManifest 'kettle_config_sha256'
        )
        $cliHelpers = @(
            Get-PropertyValue $record 'helper_binaries' |
                Where-Object {
                    (Get-PropertyValue $_ 'role') -eq 'kettle-cli'
                }
        )
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $binary 'executable'),
                [string](Get-PropertyValue $record 'executable')
            ) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](
                    Get-PropertyValue $binary 'executable_sha256'
                ),
                [string](
                    Get-PropertyValue $record 'executable_sha256'
                )
            ) -or
            [string](Get-PropertyValue $binary 'product_version') -ne
                [string](Get-PropertyValue $record 'version') -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $binary 'config_sha256'),
                $configHash
            ) -or
            $cliHelpers.Count -ne 1 -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](
                    Get-PropertyValue $binary 'cli_executable_sha256'
                ),
                [string](
                    Get-PropertyValue $cliHelpers[0] 'sha256'
                )
            )
        ) {
            $issues.Add(
                "${Prefix}monitor-transition binary provenance is invalid"
            )
        }
    }
    return $issues
}

function Load-Perf([string]$Dir, [string[]]$TerminalNames) {
    $startup = Read-JsonFile (Join-Path $Dir 'startup-idle.json')
    $latency = Read-JsonFile (Join-Path $Dir 'latency.json')

    $rows = [ordered]@{}
    foreach ($name in @($TerminalNames | Sort-Object)) {
        $tp = Read-JsonFile (Join-Path $Dir "throughput-$name.json")
        $st = if ($startup -and $startup.PSObject.Properties[$name]) {
            $startup.PSObject.Properties[$name].Value
        } else {
            $null
        }
        $lt = if ($latency -and $latency.PSObject.Properties[$name]) {
            $latency.PSObject.Properties[$name].Value
        } else {
            $null
        }
        $payloads = Get-PropertyValue $tp 'payloads'
        $sgrPayload = Get-PropertyValue $payloads 'sgr'

        $rows[$name] = [ordered]@{
            ascii_mbps = Payload-Mbps $payloads @('ascii')
            sgr_mbps = Payload-Mbps $payloads @('sgr', 'sgr-heavy')
            unicode_mbps = Payload-Mbps $payloads @('unicode')
            throughput_output_encoding = Get-PropertyValue $tp 'output_encoding'
            throughput_drain_required = Get-PropertyValue $tp 'drain_probe_required'
            ascii_runs = Payload-Runs $payloads 'ascii'
            sgr_runs = if ($null -ne $sgrPayload) {
                Payload-Runs $payloads 'sgr'
            } else {
                Payload-Runs $payloads 'sgr-heavy'
            }
            unicode_runs = Payload-Runs $payloads 'unicode'
            ascii_seconds_all = @(Payload-Property $payloads 'ascii' 'seconds_all')
            ascii_timing_boundary = Payload-Property `
                $payloads 'ascii' 'timing_boundary'
            ascii_write_seconds_all = @(
                Payload-Property $payloads 'ascii' 'write_seconds_all'
            )
            ascii_write_seconds_median = As-Double (
                Payload-Property $payloads 'ascii' 'write_seconds_median'
            )
            ascii_writer_mbps = As-Double (
                Payload-Property `
                    $payloads 'ascii' 'writer_acceptance_mb_per_s_median'
            )
            ascii_seconds_median = As-Double (
                Payload-Property $payloads 'ascii' 'seconds_median'
            )
            sgr_seconds_all = if ($null -ne $sgrPayload) {
                @(Payload-Property $payloads 'sgr' 'seconds_all')
            } else {
                @(Payload-Property $payloads 'sgr-heavy' 'seconds_all')
            }
            sgr_timing_boundary = if ($null -ne $sgrPayload) {
                Payload-Property $payloads 'sgr' 'timing_boundary'
            } else {
                Payload-Property $payloads 'sgr-heavy' 'timing_boundary'
            }
            sgr_write_seconds_all = if ($null -ne $sgrPayload) {
                @(Payload-Property $payloads 'sgr' 'write_seconds_all')
            } else {
                @(Payload-Property $payloads 'sgr-heavy' 'write_seconds_all')
            }
            sgr_write_seconds_median = if ($null -ne $sgrPayload) {
                As-Double (
                    Payload-Property $payloads 'sgr' 'write_seconds_median'
                )
            } else {
                As-Double (
                    Payload-Property `
                        $payloads 'sgr-heavy' 'write_seconds_median'
                )
            }
            sgr_writer_mbps = if ($null -ne $sgrPayload) {
                As-Double (
                    Payload-Property `
                        $payloads 'sgr' 'writer_acceptance_mb_per_s_median'
                )
            } else {
                As-Double (
                    Payload-Property $payloads 'sgr-heavy' `
                        'writer_acceptance_mb_per_s_median'
                )
            }
            sgr_seconds_median = if ($null -ne $sgrPayload) {
                As-Double (Payload-Property $payloads 'sgr' 'seconds_median')
            } else {
                As-Double (
                    Payload-Property $payloads 'sgr-heavy' 'seconds_median'
                )
            }
            unicode_seconds_all = @(Payload-Property $payloads 'unicode' 'seconds_all')
            unicode_timing_boundary = Payload-Property `
                $payloads 'unicode' 'timing_boundary'
            unicode_write_seconds_all = @(
                Payload-Property $payloads 'unicode' 'write_seconds_all'
            )
            unicode_write_seconds_median = As-Double (
                Payload-Property $payloads 'unicode' 'write_seconds_median'
            )
            unicode_writer_mbps = As-Double (
                Payload-Property `
                    $payloads 'unicode' `
                    'writer_acceptance_mb_per_s_median'
            )
            unicode_seconds_median = As-Double (
                Payload-Property $payloads 'unicode' 'seconds_median'
            )
            ascii_bytes = As-NonnegativeInt (
                Payload-Property $payloads 'ascii' 'bytes'
            )
            ascii_sha256 = Payload-Property $payloads 'ascii' 'sha256'
            sgr_bytes = if ($null -ne $sgrPayload) {
                As-NonnegativeInt (Payload-Property $payloads 'sgr' 'bytes')
            } else {
                As-NonnegativeInt (Payload-Property $payloads 'sgr-heavy' 'bytes')
            }
            sgr_sha256 = if ($null -ne $sgrPayload) {
                Payload-Property $payloads 'sgr' 'sha256'
            } else {
                Payload-Property $payloads 'sgr-heavy' 'sha256'
            }
            unicode_bytes = As-NonnegativeInt (
                Payload-Property $payloads 'unicode' 'bytes'
            )
            unicode_sha256 = Payload-Property $payloads 'unicode' 'sha256'
            ascii_drain_ms_all = @(
                Payload-Property $payloads 'ascii' 'drain_ms_all'
            )
            ascii_drain_misses = As-NonnegativeInt (
                Payload-Property $payloads 'ascii' 'drain_misses'
            )
            sgr_drain_ms_all = if ($null -ne $sgrPayload) {
                @(Payload-Property $payloads 'sgr' 'drain_ms_all')
            } else {
                @(Payload-Property $payloads 'sgr-heavy' 'drain_ms_all')
            }
            sgr_drain_misses = if ($null -ne $sgrPayload) {
                As-NonnegativeInt (
                    Payload-Property $payloads 'sgr' 'drain_misses'
                )
            } else {
                As-NonnegativeInt (
                    Payload-Property $payloads 'sgr-heavy' 'drain_misses'
                )
            }
            unicode_drain_ms_all = @(
                Payload-Property $payloads 'unicode' 'drain_ms_all'
            )
            unicode_drain_misses = As-NonnegativeInt (
                Payload-Property $payloads 'unicode' 'drain_misses'
            )
            postflood_ws_mb = As-Double (Get-PropertyValue $tp 'postflood_ws_mb')
            postflood_ws_scope = Get-PropertyValue `
                $tp 'postflood_ws_scope'
            postflood_ws_excluded_pids = @(
                Get-PropertyValue $tp 'postflood_ws_excluded_pids'
            )
            throughput_workload_pid = As-NonnegativeInt (
                Get-PropertyValue $tp 'workload_pid'
            )
            startup_ms = As-Double (Get-PropertyValue $st 'startup_ms_median')
            startup_ms_all = @(Get-PropertyValue $st 'startup_ms_all')
            startup_samples = As-NonnegativeInt (Get-PropertyValue $st 'startup_samples')
            startup_requested_samples = As-NonnegativeInt (
                Get-PropertyValue $st 'startup_requested_samples'
            )
            startup_misses = As-NonnegativeInt (Get-PropertyValue $st 'startup_misses')
            startup_observations = @(
                Get-PropertyValue $st 'startup_observations'
            )
            fresh_ws_mb = As-Double (Get-PropertyValue $st 'fresh_ws_mb')
            fresh_ws_mb_all = @(Get-PropertyValue $st 'fresh_ws_mb_all')
            idle_cpu_pct = As-Double (Get-PropertyValue $st 'idle_cpu_pct') -AllowZero
            idle_cpu_pct_all = @(Get-PropertyValue $st 'idle_cpu_pct_all')
            idle_observations = @(
                Get-PropertyValue $st 'idle_observations'
            )
            latency_ms = As-Double (Get-PropertyValue $lt 'latency_ms_median')
            latency_p95_ms = As-Double (Get-PropertyValue $lt 'latency_ms_p95')
            latency_ms_all = @(Get-PropertyValue $lt 'latency_ms_all')
            latency_samples = As-NonnegativeInt (Get-PropertyValue $lt 'samples')
            latency_requested_samples = As-NonnegativeInt (Get-PropertyValue $lt 'requested_samples')
            latency_misses = As-NonnegativeInt (Get-PropertyValue $lt 'misses')
            latency_observations = @(
                Get-PropertyValue $lt 'observations'
            )
            throughput_observations = @(
                Get-PropertyValue $tp 'observations'
            )
            throughput_configuration_mode = Get-PropertyValue `
                $tp 'configuration_mode'
            throughput_configuration_evidence = Get-PropertyValue `
                $tp 'configuration_evidence'
            throughput_schedule_algorithm = Get-PropertyValue `
                $tp 'schedule_algorithm'
            throughput_schedule_seed_sha256 = Get-PropertyValue `
                $tp 'schedule_seed_sha256'
            throughput_runner = Get-PropertyValue $tp 'workload_runner'
            throughput_executable = Get-PropertyValue $tp 'executable'
            throughput_executable_sha256 = Get-PropertyValue $tp 'executable_sha256'
            throughput_version = Get-PropertyValue $tp 'product_version'
            throughput_run_id = Get-PropertyValue $tp 'run_id'
            throughput_helper_binaries = @(
                Get-PropertyValue $tp 'helper_binaries' |
                    Where-Object { $null -ne $_ }
            )
            startup_executable = Get-PropertyValue $st 'executable'
            startup_executable_sha256 = Get-PropertyValue $st 'executable_sha256'
            startup_version = Get-PropertyValue $st 'product_version'
            startup_run_id = Get-PropertyValue $st 'run_id'
            startup_configuration_mode = Get-PropertyValue `
                $st 'configuration_mode'
            startup_configuration_evidence = Get-PropertyValue `
                $st 'configuration_evidence'
            startup_schedule_algorithm = Get-PropertyValue `
                $st 'startup_schedule_algorithm'
            startup_schedule_seed_sha256 = Get-PropertyValue `
                $st 'startup_schedule_seed_sha256'
            idle_schedule_algorithm = Get-PropertyValue `
                $st 'idle_schedule_algorithm'
            idle_schedule_seed_sha256 = Get-PropertyValue `
                $st 'idle_schedule_seed_sha256'
            startup_readiness = Get-PropertyValue $st 'readiness'
            latency_executable = Get-PropertyValue $lt 'executable'
            latency_executable_sha256 = Get-PropertyValue $lt 'executable_sha256'
            latency_workload_executable = Get-PropertyValue `
                $lt 'workload_executable'
            latency_workload_executable_sha256 = Get-PropertyValue `
                $lt 'workload_executable_sha256'
            latency_version = Get-PropertyValue $lt 'product_version'
            latency_run_id = Get-PropertyValue $lt 'run_id'
            latency_helper_binaries = @(
                Get-PropertyValue $lt 'helper_binaries' |
                    Where-Object { $null -ne $_ }
            )
            latency_configuration_mode = Get-PropertyValue `
                $lt 'configuration_mode'
            latency_configuration_evidence = Get-PropertyValue `
                $lt 'configuration_evidence'
            latency_schedule_algorithm = Get-PropertyValue `
                $lt 'schedule_algorithm'
            latency_schedule_seed_sha256 = Get-PropertyValue `
                $lt 'schedule_seed_sha256'
        }
    }
    return $rows
}

function Score-Rows($Rows, $MetricDefs, [string[]]$Terminals) {
    $scores = [ordered]@{}
    foreach ($term in $Terminals) {
        $scores[$term] = [ordered]@{
            score = 0.0
            weight = 0.0
            metrics = [ordered]@{}
        }
    }

    foreach ($def in $MetricDefs) {
        $name = $def.name
        $vals = @()
        foreach ($term in $Terminals) {
            $v = As-Double $Rows[$term][$name] -AllowZero:$def.allow_zero
            if ($null -ne $v) {
                $vals += [pscustomobject]@{ term = $term; value = $v }
            }
        }
        if ($vals.Count -lt 2) { continue }

        $best = if ($def.higher) {
            ($vals | Measure-Object -Property value -Maximum).Maximum
        } else {
            ($vals | Measure-Object -Property value -Minimum).Minimum
        }

        foreach ($v in $vals) {
            $component = if ($def.higher) {
                $v.value / $best
            } elseif ($v.value -eq $best) {
                1.0
            } elseif ($best -eq 0.0) {
                # A measured 0% idle-CPU delta is valid, unlike zero
                # throughput/startup/latency. Other values cannot be expressed
                # as a ratio to zero and therefore receive the floor.
                0.0
            } else {
                $best / $v.value
            }
            $component = [Math]::Max(0.0, [Math]::Min(1.0, $component))
            $weighted = $component * [double]$def.weight
            $scores[$v.term].score += $weighted
            $scores[$v.term].weight += [double]$def.weight
            $scores[$v.term].metrics[$name] = [Math]::Round($component, 4)
        }
    }

    foreach ($term in $scores.Keys) {
        if ($scores[$term].weight -gt 0.0) {
            $scores[$term].score = [Math]::Round($scores[$term].score / $scores[$term].weight, 4)
        }
    }
    return $scores
}

function Regression-Report($Now, $Base, [double]$MaxPct) {
    if (-not $Base -or -not $Base.Contains('kettle') -or -not $Now.Contains('kettle')) {
        return @()
    }
    $defs = @(
        @{ name = 'ascii_mbps'; higher = $true; allow_zero = $false },
        @{ name = 'sgr_mbps'; higher = $true; allow_zero = $false },
        @{ name = 'unicode_mbps'; higher = $true; allow_zero = $false },
        @{ name = 'startup_ms'; higher = $false; allow_zero = $false },
        @{ name = 'idle_cpu_pct'; higher = $false; allow_zero = $true },
        @{ name = 'fresh_ws_mb'; higher = $false; allow_zero = $false },
        @{ name = 'postflood_ws_mb'; higher = $false; allow_zero = $false },
        @{ name = 'latency_ms'; higher = $false; allow_zero = $false },
        @{ name = 'latency_p95_ms'; higher = $false; allow_zero = $false }
    )
    $bad = @()
    foreach ($d in $defs) {
        $n = As-Double $Now.kettle[$d.name] -AllowZero:$d.allow_zero
        $b = As-Double $Base.kettle[$d.name] -AllowZero:$d.allow_zero
        if ($null -eq $n -or $null -eq $b) { continue }
        $delta = if ($d.higher) {
            (($b - $n) / $b) * 100.0
        } elseif ($b -eq 0.0) {
            if ($n -eq 0.0) { 0.0 } else { [double]::PositiveInfinity }
        } else {
            (($n - $b) / $b) * 100.0
        }
        if ($delta -gt $MaxPct) {
            $bad += [pscustomobject]@{
                metric = $d.name
                baseline = [Math]::Round($b, 3)
                current = [Math]::Round($n, 3)
                regression_pct = if ([double]::IsPositiveInfinity($delta)) {
                    'unbounded'
                } else {
                    [Math]::Round($delta, 2)
                }
            }
        }
    }
    return $bad
}

function Get-JsonCollectionSignature($Value) {
    return @(
        @($Value) |
            ForEach-Object { $_ | ConvertTo-Json -Compress -Depth 10 } |
            Sort-Object
    ) -join "`n"
}

function Get-OrderedJsonCollectionSignature($Value) {
    return ConvertTo-Json `
        -InputObject ([object[]]@($Value)) -Compress -Depth 10
}

function Get-EnvironmentConfiguration($Configuration) {
    $files = @(
        Get-PropertyValue $Configuration 'files' |
            Where-Object { $null -ne $_ } |
            ForEach-Object {
                [ordered]@{
                    bytes = Get-PropertyValue $_ 'bytes'
                    sha256 = (
                        [string](Get-PropertyValue $_ 'sha256')
                    ).ToLowerInvariant()
                }
            }
    )
    return [ordered]@{
        mode = Get-PropertyValue $Configuration 'mode'
        claim_eligible = Get-PropertyValue $Configuration 'claim_eligible'
        files = [object[]]$files
    }
}

function Get-ManifestEnvironmentSignature($BenchmarkManifest) {
    $machine = Get-PropertyValue $BenchmarkManifest 'machine'
    $display = Get-PropertyValue $machine 'display_topology'
    $settings = Get-PropertyValue $BenchmarkManifest 'settings'
    $os = Get-PropertyValue $BenchmarkManifest 'os'
    $toolchain = Get-PropertyValue $BenchmarkManifest 'toolchain'
    $terminalRecords = @(Get-PropertyValue $BenchmarkManifest 'terminals')
    $peerTerminals = @(
        $terminalRecords |
            Where-Object { (Get-PropertyValue $_ 'name') -ne 'kettle' } |
            ForEach-Object {
                [ordered]@{
                    name = Get-PropertyValue $_ 'name'
                    executable = Get-PropertyValue $_ 'executable'
                    executable_sha256 = Get-PropertyValue $_ 'executable_sha256'
                    version = Get-PropertyValue $_ 'version'
                    command_workloads = Get-PropertyValue $_ 'command_workloads'
                    command_confirmation = Get-PropertyValue $_ 'command_confirmation'
                    helper_binaries = Get-PropertyValue $_ 'helper_binaries'
                    configuration = Get-EnvironmentConfiguration (
                        Get-PropertyValue $_ 'configuration'
                    )
                }
            }
    )
    $measurementSettings = [ordered]@{
        terminals = @(Get-PropertyValue $settings 'terminals')
        benchmark_seed = Get-PropertyValue $settings 'benchmark_seed'
        window_pixels = Get-PropertyValue $settings 'window_pixels'
        native_window_pixels = Get-PropertyValue `
            $settings 'native_window_pixels'
        startup_runs = Get-PropertyValue $settings 'startup_runs'
        idle_samples = Get-PropertyValue $settings 'idle_samples'
        idle_seconds = Get-PropertyValue $settings 'idle_seconds'
        latency_samples = Get-PropertyValue $settings 'latency_samples'
        latency_block_size = Get-PropertyValue `
            $settings 'latency_block_size'
        max_latency_censored = Get-PropertyValue `
            $settings 'max_latency_censored'
        latency_timeout_ms = Get-PropertyValue `
            $settings 'latency_timeout_ms'
        menu_hover_samples = Get-PropertyValue $settings 'menu_hover_samples'
        native_display_enabled = Get-PropertyValue `
            $settings 'native_display_enabled'
        monitor_transition_samples_per_state = Get-PropertyValue `
            $settings 'monitor_transition_samples_per_state'
        throughput_iterations = Get-PropertyValue $settings 'throughput_iterations'
        minimum_throughput_iterations = Get-PropertyValue `
            $settings 'minimum_throughput_iterations'
        terminal_order_offset = Get-PropertyValue `
            $settings 'terminal_order_offset'
        vtebench_terminal_order = Get-PropertyValue `
            $settings 'vtebench_terminal_order'
        schedules = Get-PropertyValue $settings 'schedules'
        vtebench_enabled = Get-PropertyValue $settings 'vtebench_enabled'
        vtebench_revision = Get-PropertyValue $settings 'vtebench_revision'
        monitor_transition_enabled = Get-PropertyValue `
            $settings 'monitor_transition_enabled'
        unidentified_display_allowed = Get-PropertyValue `
            $settings 'unidentified_display_allowed'
        probe_cooldown_seconds = Get-PropertyValue `
            $settings 'probe_cooldown_seconds'
    }

    return [ordered]@{
        os = Get-JsonCollectionSignature @([ordered]@{
            description = Get-PropertyValue $os 'description'
            version = Get-PropertyValue $os 'version'
            architecture = Get-PropertyValue $os 'architecture'
        })
        toolchain = Get-JsonCollectionSignature @($toolchain)
        machine_identity = Get-JsonCollectionSignature @([ordered]@{
            manufacturer = Get-PropertyValue $machine 'manufacturer'
            model = Get-PropertyValue $machine 'model'
            total_memory_bytes = Get-PropertyValue $machine 'total_memory_bytes'
        })
        processors = Get-JsonCollectionSignature (
            Get-PropertyValue $machine 'processors'
        )
        video_controllers = Get-JsonCollectionSignature (
            Get-PropertyValue $machine 'video_controllers'
        )
        desktop_screens = Get-JsonCollectionSignature (
            Get-PropertyValue $display 'desktop_screens'
        )
        physical_monitors = Get-JsonCollectionSignature (
            Get-PropertyValue $display 'active_physical_monitors'
        )
        monitor_connections = Get-JsonCollectionSignature (
            Get-PropertyValue $display 'active_connections'
        )
        display_acquisition = Get-JsonCollectionSignature @([ordered]@{
            schema = Get-PropertyValue $display 'acquisition_schema'
            start_signature_sha256 = Get-PropertyValue `
                $display 'start_signature_sha256'
            end_signature_sha256 = Get-PropertyValue `
                $display 'end_signature_sha256'
            topology_stable = Get-PropertyValue $display 'topology_stable'
        })
        power_scheme = [string](Get-PropertyValue $machine 'active_power_scheme')
        kettle_config_sha256 = [string](
            Get-PropertyValue $BenchmarkManifest 'kettle_config_sha256'
        )
        harness_aggregate = [string](Get-PropertyValue (
            Get-PropertyValue $BenchmarkManifest 'harness_provenance'
        ) 'aggregate_sha256')
        measurement_settings = Get-JsonCollectionSignature @($measurementSettings)
        peer_terminals = Get-JsonCollectionSignature $peerTerminals
    }
}

function Compare-BenchmarkEnvironment($CurrentManifest, $BaselineManifest) {
    $issues = [System.Collections.Generic.List[string]]::new()
    $current = Get-ManifestEnvironmentSignature $CurrentManifest
    $baseline = Get-ManifestEnvironmentSignature $BaselineManifest
    foreach ($name in $current.Keys) {
        if (-not [StringComparer]::Ordinal.Equals(
            [string]$current[$name],
            [string]$baseline[$name]
        )) {
            $issues.Add("baseline environment differs: $name")
        }
    }
    return $issues
}

function Get-KettlePerfHarnessManifestIssues {
    param(
        $BenchmarkManifest,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $provenance = Get-PropertyValue `
        $BenchmarkManifest 'harness_provenance'
    if (
        $null -eq $provenance -or
        (As-NonnegativeInt (
            Get-PropertyValue $provenance 'schema_version'
        )) -ne 1 -or
        (Get-PropertyValue $provenance 'lock_protocol') -ne
            'file-share-read-no-write-delete-v1'
    ) {
        $issues.Add("${Prefix}harness provenance metadata is invalid")
        return $issues
    }
    $expectedNames = [string[]]@(Get-KettlePerfHarnessFileNames)
    $files = @(
        Get-PropertyValue $provenance 'files' |
            Where-Object { $null -ne $_ }
    )
    if ($files.Count -ne $expectedNames.Count) {
        $issues.Add("${Prefix}harness provenance file coverage is incomplete")
        return $issues
    }

    $aggregateText = [Text.StringBuilder]::new()
    foreach ($name in $expectedNames) {
        $matchingFiles = @($files | Where-Object {
            (Get-PropertyValue $_ 'path') -ceq $name
        })
        if ($matchingFiles.Count -ne 1) {
            $issues.Add(
                "${Prefix}harness provenance is not exact and unique for $name"
            )
            continue
        }
        $bytes = As-NonnegativeInt (
            Get-PropertyValue $matchingFiles[0] 'bytes'
        )
        $sha256 = [string](
            Get-PropertyValue $matchingFiles[0] 'sha256'
        )
        if (
            $null -eq $bytes -or
            $bytes -le 0 -or
            $sha256 -cnotmatch '^[0-9a-f]{64}$'
        ) {
            $issues.Add("${Prefix}harness provenance record is invalid for $name")
            continue
        }
        [void]$aggregateText.Append($name)
        [void]$aggregateText.Append([char]0)
        [void]$aggregateText.Append($sha256)
        [void]$aggregateText.Append("`n")
    }
    if ($issues.Count -gt 0) {
        return $issues
    }
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash(
            $utf8.GetBytes($aggregateText.ToString())
        )
    } finally {
        $sha.Dispose()
    }
    $calculated = (
        [BitConverter]::ToString($digest).Replace('-', '').
            ToLowerInvariant()
    )
    $recorded = [string](
        Get-PropertyValue $provenance 'aggregate_sha256'
    )
    if (
        $recorded -cnotmatch '^[0-9a-f]{64}$' -or
        $recorded -cne $calculated
    ) {
        $issues.Add("${Prefix}harness provenance aggregate is invalid")
    }
    return $issues
}

function Get-KettlePerfCandidateManifestIssues {
    param(
        $BenchmarkManifest,
        [ValidateSet('current', 'baseline')]
        [string]$ExpectedCandidate,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $settings = Get-PropertyValue $BenchmarkManifest 'settings'
    $repositoryCommit = [string](
        Get-PropertyValue $BenchmarkManifest 'repository_commit'
    )
    $records = @(
        Get-PropertyValue $BenchmarkManifest 'terminals' |
            Where-Object {
                (Get-PropertyValue $_ 'name') -ceq 'kettle'
            }
    )
    if ($records.Count -ne 1) {
        $issues.Add("${Prefix}Kettle candidate has no unique manifest record")
        return $issues
    }
    $record = $records[0]
    $source = Get-PropertyValue $record 'source'
    $candidate = [string](
        Get-PropertyValue $settings 'kettle_candidate'
    )
    $sourceCandidate = [string](
        Get-PropertyValue $source 'candidate'
    )
    $embeddedCommit = [string](
        Get-PropertyValue $source 'embedded_commit'
    )
    $abbreviation = [string](
        Get-PropertyValue $source 'embedded_commit_abbreviation'
    )
    $actualSha = [string](
        Get-PropertyValue $source 'actual_sha256'
    )
    $recordSha = [string](
        Get-PropertyValue $record 'executable_sha256'
    )
    $cleanEmbeddedIdentity = (
        $embeddedCommit -cmatch '^[0-9a-f]{40}$' -and
        $abbreviation -cmatch '^[0-9a-f]{7,40}$' -and
        $embeddedCommit.StartsWith(
            $abbreviation,
            [StringComparison]::Ordinal
        ) -and
        (Get-PropertyValue $source 'embedded_dirty') -eq $false
    )
    if (
        $candidate -cne $ExpectedCandidate -or
        $sourceCandidate -cne $ExpectedCandidate
    ) {
        $issues.Add(
            "${Prefix}Kettle candidate role differs from $ExpectedCandidate"
        )
    }
    if (
        -not $cleanEmbeddedIdentity -or
        $actualSha -cnotmatch '^[0-9a-f]{64}$' -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            $actualSha,
            $recordSha
        ) -or
        (Get-PropertyValue $source 'commit_object_verified') -ne $true -or
        (Get-PropertyValue $source 'commit_is_ancestor') -ne $true
    ) {
        $issues.Add("${Prefix}Kettle candidate source identity is invalid")
    }

    $expectedCommit = [string](
        Get-PropertyValue $source 'expected_commit'
    )
    $expectedSha = [string](
        Get-PropertyValue $source 'expected_sha256'
    )
    $settingsCommit = [string](
        Get-PropertyValue $settings 'expected_kettle_commit'
    )
    $settingsSha = [string](
        Get-PropertyValue $settings 'expected_kettle_sha256'
    )
    if ($ExpectedCandidate -ceq 'current') {
        if (
            (Get-PropertyValue $source 'acquisition') -cne 'repository' -or
            $embeddedCommit -cne $repositoryCommit.ToLowerInvariant() -or
            (Get-PropertyValue $source 'build_performed') -ne $true -or
            (Get-PropertyValue $source 'release_build_performed') -ne $true -or
            (Get-PropertyValue $source 'skip_build') -ne $false -or
            (Get-PropertyValue $source 'external_executable') -ne $false -or
            $expectedCommit -or
            $expectedSha -or
            $settingsCommit -or
            $settingsSha -or
            (Get-PropertyValue $settings 'kettle_build_skipped') -ne $false
        ) {
            $issues.Add(
                "${Prefix}current Kettle candidate was not built from repository HEAD"
            )
        }
    } elseif (
        (Get-PropertyValue $source 'acquisition') -cne 'pinned-external' -or
        (Get-PropertyValue $source 'build_performed') -ne $false -or
        (Get-PropertyValue $source 'release_build_performed') -ne $false -or
        (Get-PropertyValue $source 'skip_build') -ne $true -or
        (Get-PropertyValue $source 'external_executable') -ne $true -or
        (Get-PropertyValue $settings 'kettle_build_skipped') -ne $true -or
        $expectedCommit -cnotmatch '^[0-9a-f]{40}$' -or
        $expectedSha -cnotmatch '^[0-9a-f]{64}$' -or
        $embeddedCommit -cne $expectedCommit -or
        $actualSha -cne $expectedSha -or
        $settingsCommit -cne $expectedCommit -or
        $settingsSha -cne $expectedSha
    ) {
        $issues.Add(
            "${Prefix}Kettle candidate lacks an exact external pin"
        )
    }
    return $issues
}

function Get-KettlePerfProbeConfigurationIssues {
    param(
        $Rows,
        $BenchmarkManifest,
        [string[]]$Terminals,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $records = @(Get-PropertyValue $BenchmarkManifest 'terminals')
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            continue
        }
        $matchingRecords = @($records | Where-Object {
            (Get-PropertyValue $_ 'name') -ceq $terminal
        })
        if ($matchingRecords.Count -ne 1) {
            continue
        }
        $configuration = Get-PropertyValue `
            $matchingRecords[0] 'configuration'
        $manifestFiles = @(
            Get-PropertyValue $configuration 'files' |
                Where-Object { $null -ne $_ }
        )
        foreach ($probe in @('startup', 'latency', 'throughput')) {
            $mode = [string]$Rows[$terminal]["${probe}_configuration_mode"]
            $evidence = @(
                $Rows[$terminal]["${probe}_configuration_evidence"] |
                    Where-Object { $null -ne $_ }
            )
            if ($terminal -ceq 'wt') {
                if ($mode -cne 'uncontrolled' -or $evidence.Count -ne 0) {
                    $issues.Add(
                        "${Prefix}wt $probe configuration activation is not uncontrolled"
                    )
                }
            } elseif (
                $mode -cne 'benchmark-isolated' -or
                (Get-PropertyValue $configuration 'mode') -cne
                    'benchmark-isolated' -or
                (Get-PropertyValue $configuration 'claim_eligible') -ne $true -or
                (Get-JsonCollectionSignature $evidence) -cne
                    (Get-JsonCollectionSignature $manifestFiles)
            ) {
                $issues.Add(
                    "${Prefix}$terminal $probe configuration activation differs from its manifest"
                )
            }
        }
    }
    return $issues
}

function Get-KettlePerfExpectedReleaseSchedules {
    param(
        $BenchmarkManifest
    )

    $settings = Get-PropertyValue $BenchmarkManifest 'settings'
    $terminals = [string[]]@(Get-PropertyValue $settings 'terminals')
    $terminalCount = $terminals.Count
    $startupRuns = As-NonnegativeInt (
        Get-PropertyValue $settings 'startup_runs'
    )
    $idleSamples = As-NonnegativeInt (
        Get-PropertyValue $settings 'idle_samples'
    )
    $latencySamples = As-NonnegativeInt (
        Get-PropertyValue $settings 'latency_samples'
    )
    $latencyBlockSize = As-NonnegativeInt (
        Get-PropertyValue $settings 'latency_block_size'
    )
    $throughputIterations = As-NonnegativeInt (
        Get-PropertyValue $settings 'throughput_iterations'
    )
    $seed = [string](Get-PropertyValue $settings 'benchmark_seed')
    if (
        $terminalCount -lt 6 -or
        ($terminalCount % 2) -ne 0 -or
        -not $seed -or
        $null -eq $startupRuns -or
        $null -eq $idleSamples -or
        $null -eq $latencySamples -or
        $null -eq $latencyBlockSize -or
        $latencyBlockSize -le 0 -or
        $null -eq $throughputIterations -or
        ($startupRuns % $terminalCount) -ne 0 -or
        ($idleSamples % $terminalCount) -ne 0 -or
        ($latencySamples % $latencyBlockSize) -ne 0 -or
        (([int]($latencySamples / $latencyBlockSize)) % $terminalCount) -ne 0 -or
        ($throughputIterations % $terminalCount) -ne 0
    ) {
        throw 'release schedule settings cannot form complete Williams cycles'
    }
    return [ordered]@{
        startup = New-KettlePerfWilliamsSchedule `
            -Terminals $terminals -Seed "${seed}:startup" `
            -Cycles ([int]($startupRuns / $terminalCount)) `
            -Namespace 'startup'
        idle = New-KettlePerfWilliamsSchedule `
            -Terminals $terminals -Seed "${seed}:idle" `
            -Cycles ([int]($idleSamples / $terminalCount)) `
            -Namespace 'idle'
        latency = New-KettlePerfWilliamsSchedule `
            -Terminals $terminals -Seed "${seed}:latency" `
            -Cycles ([int](
                ($latencySamples / $latencyBlockSize) / $terminalCount
            )) -Namespace 'latency'
        throughput = New-KettlePerfWilliamsSchedule `
            -Terminals $terminals -Seed "${seed}:throughput" `
            -Cycles ([int]($throughputIterations / $terminalCount)) `
            -Namespace 'throughput'
    }
}

function Get-KettlePerfVtebenchOrderIssues {
    param(
        $BenchmarkManifest,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $settings = Get-PropertyValue $BenchmarkManifest 'settings'
    $terminals = [string[]]@(Get-PropertyValue $settings 'terminals')
    $offset = As-NonnegativeInt (
        Get-PropertyValue $settings 'terminal_order_offset'
    )
    $actual = @(
        Get-PropertyValue $settings 'vtebench_terminal_order'
    )
    if ($null -eq $offset -or $terminals.Count -eq 0) {
        $issues.Add("${Prefix}vtebench terminal order metadata is invalid")
        return $issues
    }
    $start = ($offset + 3) % $terminals.Count
    $expected = @(
        for ($index = 0; $index -lt $terminals.Count; $index++) {
            $terminals[($start + $index) % $terminals.Count]
        }
    )
    if (
        (Get-OrderedJsonCollectionSignature $actual) -cne
            (Get-OrderedJsonCollectionSignature $expected)
    ) {
        $issues.Add(
            "${Prefix}vtebench terminal order differs from its pinned rotation"
        )
    }
    return $issues
}

function Get-KettlePerfVisitFieldIssues {
    param(
        $Observation,
        $Visit,
        [string]$Kind,
        [string]$Terminal,
        [switch]$BlockKey,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $expectedCluster = "c$($Visit.cycle)-r$($Visit.round)"
    $checks = [ordered]@{
        terminal = @(
            [string](Get-PropertyValue $Observation 'terminal')
            $Terminal
        )
        sample_id = @(
            (As-NonnegativeInt (Get-PropertyValue $Observation 'sample_id'))
            [int]$Visit.sample_id
        )
        cycle = @(
            (As-NonnegativeInt (Get-PropertyValue $Observation 'cycle'))
            [int]$Visit.cycle
        )
        round = @(
            (As-NonnegativeInt (Get-PropertyValue $Observation 'round'))
            [int]$Visit.round
        )
        round_in_cycle = @(
            (As-NonnegativeInt (
                Get-PropertyValue $Observation 'round_in_cycle'
            ))
            [int]$Visit.round_in_cycle
        )
        position = @(
            (As-NonnegativeInt (Get-PropertyValue $Observation 'position'))
            [int]$Visit.position
        )
        sequence = @(
            (As-NonnegativeInt (Get-PropertyValue $Observation 'sequence'))
            [int]$Visit.sequence
        )
        cluster_id = @(
            [string](Get-PropertyValue $Observation 'cluster_id')
            $expectedCluster
        )
    }
    $keyName = if ($BlockKey) { 'block_id' } else { 'sample_key' }
    $checks[$keyName] = @(
        [string](Get-PropertyValue $Observation $keyName)
        [string]$Visit.sample_key
    )
    foreach ($field in $checks.Keys) {
        if ($checks[$field][0] -cne $checks[$field][1]) {
            $issues.Add(
                "${Prefix}$Kind $Terminal observation $field differs from schedule"
            )
        }
    }
    return $issues
}

function Get-KettlePerfReleaseScheduleIssues {
    param(
        $Rows,
        $BenchmarkManifest,
        [string[]]$Terminals,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    try {
        $expected = Get-KettlePerfExpectedReleaseSchedules `
            -BenchmarkManifest $BenchmarkManifest
    } catch {
        $issues.Add("${Prefix}release schedules are invalid: $($_.Exception.Message)")
        return $issues
    }
    $settings = Get-PropertyValue $BenchmarkManifest 'settings'
    $recorded = Get-PropertyValue $settings 'schedules'
    foreach ($kind in @('startup', 'idle', 'latency', 'throughput')) {
        $actualSchedule = Get-PropertyValue $recorded $kind
        if (
            $null -eq $actualSchedule -or
            (Get-JsonCollectionSignature @($actualSchedule)) -cne
                (Get-JsonCollectionSignature @($expected[$kind]))
        ) {
            $issues.Add(
                "${Prefix}$kind schedule differs from benchmark seed and pinned settings"
            )
        }
    }

    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            continue
        }
        $row = $Rows[$terminal]
        $metadataChecks = [object[]]@(
            [pscustomobject]@{
                kind = 'startup'
                algorithm = $row.startup_schedule_algorithm
                seed = $row.startup_schedule_seed_sha256
            },
            [pscustomobject]@{
                kind = 'idle'
                algorithm = $row.idle_schedule_algorithm
                seed = $row.idle_schedule_seed_sha256
            },
            [pscustomobject]@{
                kind = 'latency'
                algorithm = $row.latency_schedule_algorithm
                seed = $row.latency_schedule_seed_sha256
            },
            [pscustomobject]@{
                kind = 'throughput'
                algorithm = $row.throughput_schedule_algorithm
                seed = $row.throughput_schedule_seed_sha256
            }
        )
        foreach ($metadata in $metadataChecks) {
            if (
                [string]$metadata.algorithm -cne
                    [string]$expected[$metadata.kind].algorithm -or
                [string]$metadata.seed -cne
                    [string]$expected[$metadata.kind].seed_sha256
            ) {
                $issues.Add(
                    "${Prefix}$($metadata.kind) $terminal probe schedule metadata differs"
                )
            }
        }

        foreach ($kind in @('startup', 'idle')) {
            $source = if ($kind -ceq 'startup') {
                [object[]]@($row.startup_observations)
            } else {
                [object[]]@($row.idle_observations)
            }
            $visits = @(
                $expected[$kind].rounds |
                    ForEach-Object { $_.visits } |
                    Where-Object { $_.terminal -ceq $terminal }
            )
            if ($source.Count -ne $visits.Count) {
                $issues.Add(
                    "${Prefix}$kind $terminal observation coverage differs from schedule"
                )
                continue
            }
            foreach ($visit in $visits) {
                $matchingObservations = @($source | Where-Object {
                    (As-NonnegativeInt (
                        Get-PropertyValue $_ 'sample_id'
                    )) -eq [int]$visit.sample_id
                })
                if ($matchingObservations.Count -ne 1) {
                    $issues.Add(
                        "${Prefix}$kind $terminal has no unique scheduled visit"
                    )
                    continue
                }
                foreach (
                    $issue in Get-KettlePerfVisitFieldIssues `
                        -Observation $matchingObservations[0] -Visit $visit `
                        -Kind $kind -Terminal $terminal -Prefix $Prefix
                ) {
                    $issues.Add($issue)
                }
            }
        }

        $latencyVisits = @(
            $expected.latency.rounds |
                ForEach-Object { $_.visits } |
                Where-Object { $_.terminal -ceq $terminal }
        )
        $latencySource = [object[]]@($row.latency_observations)
        $latencyBlockSize = As-NonnegativeInt (
            Get-PropertyValue $settings 'latency_block_size'
        )
        if (
            $latencySource.Count -ne
                ($latencyVisits.Count * $latencyBlockSize)
        ) {
            $issues.Add(
                "${Prefix}latency $terminal observation coverage differs from schedule"
            )
        } else {
            foreach ($visit in $latencyVisits) {
                $block = @($latencySource | Where-Object {
                    (As-NonnegativeInt (
                        Get-PropertyValue $_ 'sample_id'
                    )) -eq [int]$visit.sample_id
                })
                if ($block.Count -ne $latencyBlockSize) {
                    $issues.Add(
                        "${Prefix}latency $terminal scheduled block coverage is invalid"
                    )
                    continue
                }
                $inBlock = @(
                    $block | ForEach-Object {
                        As-NonnegativeInt (
                            Get-PropertyValue $_ 'sample_in_block'
                        )
                    } | Sort-Object
                )
                if (
                    (Get-JsonCollectionSignature $inBlock) -cne
                        (Get-JsonCollectionSignature @(
                            1..$latencyBlockSize
                        ))
                ) {
                    $issues.Add(
                        "${Prefix}latency $terminal sample_in_block coverage is invalid"
                    )
                }
                foreach ($observation in $block) {
                    foreach (
                        $issue in Get-KettlePerfVisitFieldIssues `
                            -Observation $observation -Visit $visit `
                            -Kind 'latency' -Terminal $terminal `
                            -BlockKey -Prefix $Prefix
                    ) {
                        $issues.Add($issue)
                    }
                    $sampleInBlock = As-NonnegativeInt (
                        Get-PropertyValue $observation 'sample_in_block'
                    )
                    $expectedTerminalSample = (
                        (([int]$visit.round - 1) * $latencyBlockSize) +
                        $sampleInBlock
                    )
                    if (
                        (As-NonnegativeInt (
                            Get-PropertyValue `
                                $observation 'terminal_sample'
                        )) -ne $expectedTerminalSample
                    ) {
                        $issues.Add(
                            "${Prefix}latency $terminal terminal_sample differs from schedule"
                        )
                    }
                }
            }
        }

        $throughputVisits = @(
            $expected.throughput.rounds |
                ForEach-Object { $_.visits } |
                Where-Object { $_.terminal -ceq $terminal }
        )
        $throughputSource = [object[]]@($row.throughput_observations)
        if ($throughputSource.Count -ne ($throughputVisits.Count * 3)) {
            $issues.Add(
                "${Prefix}throughput $terminal observation coverage differs from schedule"
            )
        } else {
            foreach ($visit in $throughputVisits) {
                $round = @($throughputSource | Where-Object {
                    (As-NonnegativeInt (
                        Get-PropertyValue $_ 'sample_id'
                    )) -eq [int]$visit.sample_id
                })
                $payloads = @(
                    $round | ForEach-Object {
                        [string](Get-PropertyValue $_ 'payload')
                    } | Sort-Object
                )
                if (
                    $round.Count -ne 3 -or
                    (Get-JsonCollectionSignature $payloads) -cne
                        (Get-JsonCollectionSignature @(
                            'ascii', 'sgr', 'unicode'
                        ))
                ) {
                    $issues.Add(
                        "${Prefix}throughput $terminal scheduled payload coverage is invalid"
                    )
                    continue
                }
                foreach ($observation in $round) {
                    foreach (
                        $issue in Get-KettlePerfVisitFieldIssues `
                            -Observation $observation -Visit $visit `
                            -Kind 'throughput' -Terminal $terminal `
                            -Prefix $Prefix
                    ) {
                        $issues.Add($issue)
                    }
                }
            }
        }
    }
    return $issues
}

function Test-KettlePerfNumericArraysEqual {
    param(
        [object[]]$Left,
        [object[]]$Right,
        [double]$Tolerance = 0.000001
    )

    if ($Left.Count -ne $Right.Count) {
        return $false
    }
    for ($index = 0; $index -lt $Left.Count; $index++) {
        $leftValue = As-Double $Left[$index] -AllowZero
        $rightValue = As-Double $Right[$index] -AllowZero
        if (
            $null -eq $leftValue -or
            $null -eq $rightValue -or
            [Math]::Abs($leftValue - $rightValue) -gt $Tolerance
        ) {
            return $false
        }
    }
    return $true
}

function Get-KettlePerfIdlePidIssues {
    param(
        [object[]]$Observations,
        [string]$Terminal,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $excludedInvalid = $false
    $beforeInvalid = $false
    $afterInvalid = $false
    $setsDiffer = $false
    $includedExcludedOverlap = $false
    foreach ($observation in $Observations) {
        $workloadPid = As-NonnegativeInt (
            Get-PropertyValue $observation 'workload_pid'
        )
        $excluded = @(
            Get-PropertyValue $observation 'excluded_pids'
        )
        $excludedSet = [Collections.Generic.HashSet[int]]::new()
        foreach ($rawPid in $excluded) {
            $pidValue = As-NonnegativeInt $rawPid
            if (
                $null -eq $pidValue -or
                $pidValue -le 0 -or
                -not $excludedSet.Add($pidValue)
            ) {
                $excludedInvalid = $true
            }
        }
        if (
            $null -eq $workloadPid -or
            $workloadPid -le 0 -or
            -not $excludedSet.Contains($workloadPid)
        ) {
            $excludedInvalid = $true
        }

        $before = @(
            Get-PropertyValue $observation 'included_processes_before'
        )
        $after = @(
            Get-PropertyValue $observation 'included_processes_after'
        )
        $beforeSet = [Collections.Generic.HashSet[int]]::new()
        $afterSet = [Collections.Generic.HashSet[int]]::new()
        if ($before.Count -eq 0) {
            $beforeInvalid = $true
        }
        if ($after.Count -eq 0) {
            $afterInvalid = $true
        }
        foreach ($sample in $before) {
            $pidValue = As-NonnegativeInt (
                Get-PropertyValue $sample 'pid'
            )
            if (
                $null -eq $pidValue -or
                $pidValue -le 0 -or
                -not $beforeSet.Add($pidValue)
            ) {
                $beforeInvalid = $true
                continue
            }
            if ($excludedSet.Contains($pidValue)) {
                $includedExcludedOverlap = $true
            }
        }
        foreach ($sample in $after) {
            $pidValue = As-NonnegativeInt (
                Get-PropertyValue $sample 'pid'
            )
            if (
                $null -eq $pidValue -or
                $pidValue -le 0 -or
                -not $afterSet.Add($pidValue)
            ) {
                $afterInvalid = $true
                continue
            }
            if ($excludedSet.Contains($pidValue)) {
                $includedExcludedOverlap = $true
            }
        }
        if (
            $beforeSet.Count -ne $afterSet.Count -or
            @($beforeSet | Where-Object {
                -not $afterSet.Contains($_)
            }).Count -ne 0
        ) {
            $setsDiffer = $true
        }
    }
    if ($excludedInvalid) {
        $issues.Add(
            "${Prefix}idle $Terminal excluded PID evidence is invalid"
        )
    }
    if ($beforeInvalid) {
        $issues.Add(
            "${Prefix}idle $Terminal before process evidence contains invalid or duplicate PIDs"
        )
    }
    if ($afterInvalid) {
        $issues.Add(
            "${Prefix}idle $Terminal after process evidence contains invalid or duplicate PIDs"
        )
    }
    if ($setsDiffer) {
        $issues.Add(
            "${Prefix}idle $Terminal before/after included PID sets differ"
        )
    }
    if ($includedExcludedOverlap) {
        $issues.Add(
            "${Prefix}idle $Terminal includes a PID declared excluded"
        )
    }
    return $issues
}

function Get-KettlePerfRawAggregateIssues {
    param(
        $Rows,
        [string[]]$Terminals,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            continue
        }
        $row = $Rows[$terminal]
        foreach (
            $issue in Get-KettlePerfIdlePidIssues `
                -Observations ([object[]]@($row.idle_observations)) `
                -Terminal $terminal -Prefix $Prefix
        ) {
            $issues.Add($issue)
        }
        $startupRaw = @($row.startup_observations | ForEach-Object {
            Get-PropertyValue $_ 'value'
        })
        if (-not (Test-KettlePerfNumericArraysEqual `
            -Left $startupRaw -Right @($row.startup_ms_all)
        )) {
            $issues.Add(
                "${Prefix}$terminal startup raw observations differ from aggregates"
            )
        }
        $idleCpuRaw = @($row.idle_observations | ForEach-Object {
            Get-PropertyValue $_ 'idle_cpu_pct'
        })
        $freshWsRaw = @($row.idle_observations | ForEach-Object {
            Get-PropertyValue $_ 'fresh_ws_mb'
        })
        if (
            -not (Test-KettlePerfNumericArraysEqual `
                -Left $idleCpuRaw -Right @($row.idle_cpu_pct_all)) -or
            -not (Test-KettlePerfNumericArraysEqual `
                -Left $freshWsRaw -Right @($row.fresh_ws_mb_all))
        ) {
            $issues.Add(
                "${Prefix}$terminal idle raw observations differ from aggregates"
            )
        }
        $latencyRaw = @(
            $row.latency_observations |
                Where-Object {
                    (Get-PropertyValue $_ 'status') -ceq 'ok'
                } |
                ForEach-Object { Get-PropertyValue $_ 'value' }
        )
        if (-not (Test-KettlePerfNumericArraysEqual `
            -Left $latencyRaw -Right @($row.latency_ms_all) `
            -Tolerance 0.000001
        )) {
            $issues.Add(
                "${Prefix}$terminal latency raw observations differ from aggregates"
            )
        }
        foreach ($payload in @('ascii', 'sgr', 'unicode')) {
            $observations = @(
                $row.throughput_observations |
                    Where-Object {
                        (Get-PropertyValue $_ 'payload') -ceq $payload
                    }
            )
            $seconds = @($observations | ForEach-Object {
                Get-PropertyValue $_ 'seconds'
            })
            $writeSeconds = @($observations | ForEach-Object {
                Get-PropertyValue $_ 'write_seconds'
            })
            $drain = @($observations | ForEach-Object {
                Get-PropertyValue $_ 'drain_ms'
            })
            if (
                -not (Test-KettlePerfNumericArraysEqual `
                    -Left $seconds `
                    -Right @($row["${payload}_seconds_all"])) -or
                -not (Test-KettlePerfNumericArraysEqual `
                    -Left $writeSeconds `
                    -Right @($row["${payload}_write_seconds_all"])) -or
                -not (Test-KettlePerfNumericArraysEqual `
                    -Left $drain `
                    -Right @($row["${payload}_drain_ms_all"]))
            ) {
                $issues.Add(
                    "${Prefix}$terminal $payload throughput raw timings differ from aggregates"
                )
            }
        }
    }
    return $issues
}

function Get-KettlePerfHarnessRecord {
    param(
        $BenchmarkManifest,
        [string]$Name
    )

    $files = @(
        Get-PropertyValue (
            Get-PropertyValue $BenchmarkManifest 'harness_provenance'
        ) 'files'
    )
    $matchingFiles = @($files | Where-Object {
        (Get-PropertyValue $_ 'path') -ceq $Name
    })
    if ($matchingFiles.Count -ne 1) {
        return $null
    }
    return $matchingFiles[0]
}

function Get-KettlePerfUtf8Sha256 {
    param(
        [Parameter(Mandatory)]
        [string]$Text
    )

    $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash($bytes)
        return (
            [BitConverter]::ToString($digest).Replace('-', '').
                ToLowerInvariant()
        )
    } finally {
        $sha.Dispose()
        [Array]::Clear($bytes, 0, $bytes.Length)
    }
}

function Get-KettlePerfDisplayTopologySnapshotSignature {
    param(
        $Snapshot
    )

    if ($null -eq $Snapshot) {
        return $null
    }
    $signatureValue = [ordered]@{
        schema = Get-PropertyValue $Snapshot 'schema'
        identity_acquisition = Get-PropertyValue `
            $Snapshot 'identity_acquisition'
        target_screen_device = Get-PropertyValue `
            $Snapshot 'target_screen_device'
        primary_screen_device = Get-PropertyValue `
            $Snapshot 'primary_screen_device'
        target_monitor_hardware_id = Get-PropertyValue `
            $Snapshot 'target_monitor_hardware_id'
        desktop_screens = [object[]]@(
            Get-PropertyValue $Snapshot 'desktop_screens'
        )
        active_physical_monitors = [object[]]@(
            Get-PropertyValue $Snapshot 'active_physical_monitors'
        )
        active_connections = [object[]]@(
            Get-PropertyValue $Snapshot 'active_connections'
        )
        target_edid_monitors = [object[]]@(
            Get-PropertyValue $Snapshot 'target_edid_monitors'
        )
        identity_issues = [object[]]@(
            Get-PropertyValue $Snapshot 'identity_issues'
        )
    }
    $json = ConvertTo-Json -InputObject $signatureValue `
        -Compress -Depth 8
    return Get-KettlePerfUtf8Sha256 -Text $json
}

function Get-KettlePerfDisplayTopologyAcquisitionIssues {
    param(
        $BenchmarkManifest,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $display = Get-PropertyValue (
        Get-PropertyValue $BenchmarkManifest 'machine'
    ) 'display_topology'
    $start = Get-PropertyValue $display 'acquisition_start'
    $end = Get-PropertyValue $display 'acquisition_end'
    $startSignature = [string](
        Get-PropertyValue $display 'start_signature_sha256'
    )
    $endSignature = [string](
        Get-PropertyValue $display 'end_signature_sha256'
    )
    $startEmbeddedSignature = [string](
        Get-PropertyValue $start 'signature_sha256'
    )
    $endEmbeddedSignature = [string](
        Get-PropertyValue $end 'signature_sha256'
    )
    $calculatedStart = Get-KettlePerfDisplayTopologySnapshotSignature $start
    $calculatedEnd = Get-KettlePerfDisplayTopologySnapshotSignature $end
    if (
        (Get-PropertyValue $display 'acquisition_schema') -cne
            'kettle-display-topology-acquisition-v2' -or
        (Get-PropertyValue $start 'schema') -cne
            'kettle-display-topology-snapshot-v2' -or
        (Get-PropertyValue $end 'schema') -cne
            'kettle-display-topology-snapshot-v2' -or
        $startSignature -cnotmatch '^[0-9a-f]{64}$' -or
        $endSignature -cnotmatch '^[0-9a-f]{64}$'
    ) {
        $issues.Add(
            "${Prefix}display topology acquisition contract is invalid"
        )
        return $issues
    }
    foreach (
        $identityIssue in Get-KettlePerfDisplayIdentityEvidenceIssue `
            -Topology $start -Prefix "${Prefix}start "
    ) {
        $issues.Add($identityIssue)
    }
    foreach (
        $identityIssue in Get-KettlePerfDisplayIdentityEvidenceIssue `
            -Topology $end -Prefix "${Prefix}end "
    ) {
        $issues.Add($identityIssue)
    }
    if (
        -not [StringComparer]::Ordinal.Equals(
            $startSignature,
            $startEmbeddedSignature
        ) -or
        -not [StringComparer]::Ordinal.Equals(
            $startSignature,
            [string]$calculatedStart
        )
    ) {
        $issues.Add("${Prefix}display topology start snapshot is invalid")
    }
    if (
        -not [StringComparer]::Ordinal.Equals(
            $endSignature,
            $endEmbeddedSignature
        ) -or
        -not [StringComparer]::Ordinal.Equals(
            $endSignature,
            [string]$calculatedEnd
        )
    ) {
        $issues.Add("${Prefix}display topology end snapshot is invalid")
    }
    if (
        -not [StringComparer]::Ordinal.Equals(
            $startSignature,
            $endSignature
        ) -or
        (Get-PropertyValue $display 'topology_stable') -ne $true
    ) {
        $issues.Add(
            "${Prefix}display topology acquisition signatures do not match"
        )
    }
    if (
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $display 'target_screen_device'),
            [string](Get-PropertyValue $start 'target_screen_device')
        ) -or
        (Get-JsonCollectionSignature (
            Get-PropertyValue $display 'desktop_screens'
        )) -cne (Get-JsonCollectionSignature (
            Get-PropertyValue $start 'desktop_screens'
        )) -or
        (Get-JsonCollectionSignature (
            Get-PropertyValue $display 'active_physical_monitors'
        )) -cne (Get-JsonCollectionSignature (
            Get-PropertyValue $start 'active_physical_monitors'
        )) -or
        (Get-JsonCollectionSignature (
            Get-PropertyValue $display 'active_connections'
        )) -cne (Get-JsonCollectionSignature (
            Get-PropertyValue $start 'active_connections'
        )) -or
        (Get-JsonCollectionSignature (
            Get-PropertyValue $display 'target_edid_monitors'
        )) -cne (Get-JsonCollectionSignature (
            Get-PropertyValue $start 'target_edid_monitors'
        ))
    ) {
        $issues.Add(
            "${Prefix}display topology start aliases differ from acquisition evidence"
        )
    }
    $targetDevice = [string](
        Get-PropertyValue $start 'target_screen_device'
    )
    $startTargetScreens = @(
        Get-PropertyValue $start 'desktop_screens' |
            Where-Object {
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string](Get-PropertyValue $_ 'device_name'),
                    $targetDevice
                )
            }
    )
    if (
        -not $targetDevice -or
        $startTargetScreens.Count -ne 1 -or
        (Get-PropertyValue $startTargetScreens[0] 'primary') -ne $true -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $start 'primary_screen_device'),
            $targetDevice
        ) -or
        @(Get-PropertyValue $start 'target_edid_monitors').Count -ne 1 -or
        @(Get-PropertyValue $end 'target_edid_monitors').Count -ne 1
    ) {
        $issues.Add(
            "${Prefix}display topology target monitor identity is invalid"
        )
    }
    return $issues
}

function Test-KettlePerfWslDistributionIdentity {
    param(
        $Distribution
    )

    $schema = [string](Get-PropertyValue $Distribution 'schema')
    $name = [string](Get-PropertyValue $Distribution 'name')
    $osReleasePath = [string](
        Get-PropertyValue $Distribution 'os_release_path'
    )
    $osReleaseSha = [string](
        Get-PropertyValue $Distribution 'os_release_sha256'
    )
    $osPrettyLine = [string](
        Get-PropertyValue $Distribution 'os_pretty_line'
    )
    $osVersionLine = [string](
        Get-PropertyValue $Distribution 'os_version_line'
    )
    $kernelRelease = [string](
        Get-PropertyValue $Distribution 'kernel_release'
    )
    $kernelVersion = [string](
        Get-PropertyValue $Distribution 'kernel_version'
    )
    $architecture = [string](
        Get-PropertyValue $Distribution 'architecture'
    )
    $userName = [string](
        Get-PropertyValue $Distribution 'user_name'
    )
    $userId = As-NonnegativeInt (
        Get-PropertyValue $Distribution 'user_id'
    )
    foreach ($value in @(
        $name,
        $osReleasePath,
        $osPrettyLine,
        $osVersionLine,
        $kernelRelease,
        $kernelVersion,
        $architecture,
        $userName
    )) {
        if (
            -not $value -or
            $value.Length -gt 4096 -or
            $value.Contains([char]0) -or
            $value.Contains("`r") -or
            $value.Contains("`n")
        ) {
            return $false
        }
    }
    return (
        $schema -ceq 'kettle-wsl-distribution-v1' -and
        $name -cmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$' -and
        $osReleasePath.StartsWith(
            '/',
            [StringComparison]::Ordinal
        ) -and
        $osReleaseSha -cmatch '^[0-9a-f]{64}$' -and
        $osPrettyLine.StartsWith(
            'PRETTY_NAME=',
            [StringComparison]::Ordinal
        ) -and
        $osVersionLine.StartsWith(
            'VERSION_ID=',
            [StringComparison]::Ordinal
        ) -and
        $architecture -cmatch '^[A-Za-z0-9._-]+$' -and
        $userName -cmatch '^[^:]+$' -and
        $null -ne $userId
    )
}

function Test-KettlePerfWslLauncherIdentity {
    param(
        $Launcher,
        [switch]$RequireHash
    )

    $path = [string](Get-PropertyValue $Launcher 'path')
    $sha256 = [string](Get-PropertyValue $Launcher 'sha256')
    $version = [string](Get-PropertyValue $Launcher 'version')
    $fileVersion = [string](
        Get-PropertyValue $Launcher 'file_version'
    )
    $runtimeVersion = [string](
        Get-PropertyValue $Launcher 'runtime_version'
    )
    $versionOutput = [string](
        Get-PropertyValue $Launcher 'version_output'
    )
    $versionOutputSha = [string](
        Get-PropertyValue $Launcher 'version_output_sha256'
    )
    $resolutionPolicy = [string](
        Get-PropertyValue $Launcher 'resolution_policy'
    )
    $validVersion = $false
    $validRuntimeVersion = $false
    try {
        $validVersion = ([version]$version).Major -ge 1
        $validRuntimeVersion = ([version]$runtimeVersion).Major -ge 1
    } catch {
        $validVersion = $false
        $validRuntimeVersion = $false
    }
    if (
        -not [IO.Path]::IsPathRooted($path) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetFileName($path),
            'wsl.exe'
        ) -or
        ($RequireHash -and $sha256 -cnotmatch '^[0-9a-f]{64}$') -or
        -not $validVersion -or
        -not $fileVersion -or
        -not $validRuntimeVersion -or
        -not $versionOutput -or
        $versionOutput.Length -gt 32768 -or
        -not $versionOutput.Contains($runtimeVersion) -or
        $versionOutputSha -cnotmatch '^[0-9a-f]{64}$' -or
        $versionOutputSha -cne (
            Get-KettlePerfUtf8Sha256 -Text $versionOutput
        ) -or
        $resolutionPolicy -notin @(
            'program-files-wsl-then-system32-v1',
            'explicit-override-v1'
        ) -or
        -not (
            Test-KettlePerfWslDistributionIdentity `
                -Distribution (
                    Get-PropertyValue $Launcher 'distribution'
                )
        )
    ) {
        return $false
    }
    return $true
}

function Get-KettlePerfVtebenchSourceStateSignature {
    param(
        $Source
    )

    $fields = [ordered]@{
        cache = Get-PropertyValue $Source 'wsl_cache'
        build_root = Get-PropertyValue $Source 'wsl_build_root'
        binary = Get-PropertyValue $Source 'wsl_binary'
        revision = Get-PropertyValue $Source 'revision'
        benchmark_tree = Get-PropertyValue $Source 'benchmark_tree'
        binary_sha256 = Get-PropertyValue $Source 'wsl_binary_sha256'
        cargo_lock_sha256 = Get-PropertyValue $Source 'cargo_lock_sha256'
        cargo_path = Get-PropertyValue $Source 'cargo_path'
        cargo_sha256 = Get-PropertyValue $Source 'cargo_sha256'
        cargo_version = Get-PropertyValue $Source 'cargo_version'
        rustup_path = Get-PropertyValue $Source 'rustup_path'
        rustup_sha256 = Get-PropertyValue $Source 'rustup_sha256'
        rustup_version = Get-PropertyValue $Source 'rustup_version'
        timeout_path = Get-PropertyValue $Source 'timeout_path'
        timeout_sha256 = Get-PropertyValue $Source 'timeout_sha256'
        timeout_version = Get-PropertyValue $Source 'timeout_version'
        setsid_path = Get-PropertyValue $Source 'setsid_path'
        setsid_sha256 = Get-PropertyValue $Source 'setsid_sha256'
        setsid_version = Get-PropertyValue $Source 'setsid_version'
        script_path = Get-PropertyValue $Source 'script_path'
        script_sha256 = Get-PropertyValue $Source 'script_sha256'
        script_version = Get-PropertyValue $Source 'script_version'
    }
    foreach ($name in $fields.Keys) {
        $value = [string]$fields[$name]
        if (
            -not $value -or
            $value.Length -gt 4096 -or
            $value.Contains([char]0) -or
            $value.Contains("`r") -or
            $value.Contains("`n")
        ) {
            return $null
        }
    }
    foreach ($pathField in @(
        'cache',
        'build_root',
        'binary',
        'cargo_path',
        'rustup_path',
        'timeout_path',
        'setsid_path',
        'script_path'
    )) {
        if (-not ([string]$fields[$pathField]).StartsWith(
            '/',
            [StringComparison]::Ordinal
        )) {
            return $null
        }
    }
    foreach ($hashField in @(
        'binary_sha256',
        'cargo_lock_sha256',
        'cargo_sha256',
        'rustup_sha256',
        'timeout_sha256',
        'setsid_sha256',
        'script_sha256'
    )) {
        if ([string]$fields[$hashField] -cnotmatch '^[0-9a-f]{64}$') {
            return $null
        }
    }
    if (
        [string]$fields.revision -cnotmatch '^[0-9a-f]{40}$' -or
        [string]$fields.benchmark_tree -cnotmatch '^[0-9a-f]{40}$'
    ) {
        return $null
    }
    $text = [Text.StringBuilder]::new()
    foreach ($name in $fields.Keys) {
        [void]$text.Append($name)
        [void]$text.Append([char]0)
        [void]$text.Append([string]$fields[$name])
        [void]$text.Append("`n")
    }
    return Get-KettlePerfUtf8Sha256 -Text $text.ToString()
}

function Get-KettlePerfToolchainEvidenceIssues {
    param(
        $Rows,
        $BenchmarkManifest,
        [string[]]$Terminals,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $toolchain = Get-PropertyValue $BenchmarkManifest 'toolchain'
    foreach ($role in @('orchestrator_powershell', 'throughput_powershell')) {
        $powerShell = Get-PropertyValue $toolchain $role
        if (
            -not [IO.Path]::IsPathRooted(
                [string](Get-PropertyValue $powerShell 'path')
            ) -or
            [string](Get-PropertyValue $powerShell 'sha256') -cnotmatch
                '^[0-9A-Fa-f]{64}$' -or
            -not (Get-PropertyValue $powerShell 'version')
        ) {
            $issues.Add("${Prefix}toolchain $role identity is invalid")
        }
    }
    $latencyWorkload = Get-PropertyValue $toolchain 'latency_workload'
    $latencyWorkloadPath = [string](
        Get-PropertyValue $latencyWorkload 'path'
    )
    if (
        -not [IO.Path]::IsPathRooted($latencyWorkloadPath) -or
        [IO.Path]::GetFileName($latencyWorkloadPath) -cne 'cmd.exe' -or
        [string](Get-PropertyValue $latencyWorkload 'sha256') -cnotmatch
            '^[0-9A-Fa-f]{64}$' -or
        -not (Get-PropertyValue $latencyWorkload 'version')
    ) {
        $issues.Add("${Prefix}toolchain latency_workload identity is invalid")
    }
    if (-not (
        Test-KettlePerfWslLauncherIdentity `
            -Launcher (Get-PropertyValue $toolchain 'vtebench_wsl') `
            -RequireHash
    )) {
        $issues.Add("${Prefix}toolchain vtebench_wsl identity is invalid")
    }
    $throughputPowerShell = Get-PropertyValue `
        $toolchain 'throughput_powershell'
    $startupHelper = Get-KettlePerfHarnessRecord `
        -BenchmarkManifest $BenchmarkManifest -Name 'startup-ready.ps1'
    $runnerHelper = Get-KettlePerfHarnessRecord `
        -BenchmarkManifest $BenchmarkManifest -Name 'run-inside.ps1'
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            continue
        }
        $readiness = $Rows[$terminal].startup_readiness
        if (
            (Get-PropertyValue $readiness 'schema') -cne
                'kettle-startup-ready-v1' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $readiness 'shell'),
                [string](Get-PropertyValue $throughputPowerShell 'path')
            ) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $readiness 'shell_sha256'),
                [string](Get-PropertyValue $throughputPowerShell 'sha256')
            ) -or
            -not [IO.Path]::IsPathRooted(
                [string](Get-PropertyValue $readiness 'helper_script')
            ) -or
            [IO.Path]::GetFileName(
                [string](Get-PropertyValue $readiness 'helper_script')
            ) -cne 'startup-ready.ps1' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](
                    Get-PropertyValue $readiness 'helper_script_sha256'
                ),
                [string](Get-PropertyValue $startupHelper 'sha256')
            )
        ) {
            $issues.Add(
                "${Prefix}$terminal startup readiness toolchain identity is invalid"
            )
        }
        $runner = $Rows[$terminal].throughput_runner
        $runnerPowerShell = Get-PropertyValue $runner 'powershell'
        $runnerScript = Get-PropertyValue $runner 'script'
        if (
            (Get-PropertyValue $runner 'schema') -cne
                'kettle-throughput-runner-v1' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $runnerPowerShell 'path'),
                [string](Get-PropertyValue $throughputPowerShell 'path')
            ) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $runnerPowerShell 'sha256'),
                [string](Get-PropertyValue $throughputPowerShell 'sha256')
            ) -or
            [string](Get-PropertyValue $runnerPowerShell 'version') -cne
                [string](Get-PropertyValue $throughputPowerShell 'version') -or
            -not [IO.Path]::IsPathRooted(
                [string](Get-PropertyValue $runnerScript 'path')
            ) -or
            [IO.Path]::GetFileName(
                [string](Get-PropertyValue $runnerScript 'path')
            ) -cne 'run-inside.ps1' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $runnerScript 'sha256'),
                [string](Get-PropertyValue $runnerHelper 'sha256')
            )
        ) {
            $issues.Add(
                "${Prefix}$terminal throughput runner identity is invalid"
            )
        }
    }
    return $issues
}

function Get-ResultProvenanceIssues {
    param(
        $Rows,
        $BenchmarkManifest,
        [string[]]$Terminals,
        [bool]$LatencyRequired,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    if ($null -eq $BenchmarkManifest) {
        return $issues
    }
    $runId = [string](Get-PropertyValue $BenchmarkManifest 'run_id')
    $manifestSchema = As-NonnegativeInt (
        Get-PropertyValue $BenchmarkManifest 'schema_version'
    )
    $latencyWorkload = Get-PropertyValue (
        Get-PropertyValue $BenchmarkManifest 'toolchain'
    ) 'latency_workload'
    $latencyWorkloadPath = [string](
        Get-PropertyValue $latencyWorkload 'path'
    )
    $latencyWorkloadHash = [string](
        Get-PropertyValue $latencyWorkload 'sha256'
    )
    $manifestTerminals = @(Get-PropertyValue $BenchmarkManifest 'terminals')
    foreach ($terminal in $Terminals) {
        if (-not $Rows.Contains($terminal)) {
            continue
        }
        $record = @($manifestTerminals | Where-Object {
            (Get-PropertyValue $_ 'name') -eq $terminal
        })
        if ($record.Count -ne 1) {
            $issues.Add("${Prefix}${terminal} has no unique manifest record")
            continue
        }
        $manifestExe = [string](Get-PropertyValue $record[0] 'executable')
        $manifestHash = [string](
            Get-PropertyValue $record[0] 'executable_sha256'
        )
        $manifestVersion = [string](Get-PropertyValue $record[0] 'version')
        $manifestHelpers = @(
            Get-PropertyValue $record[0] 'helper_binaries' |
                Where-Object { $null -ne $_ }
        )
        foreach ($source in @('startup', 'latency', 'throughput')) {
            $sourceExe = [string]$Rows[$terminal]["${source}_executable"]
            $sourceHash = [string](
                $Rows[$terminal]["${source}_executable_sha256"]
            )
            $sourceVersion = [string]$Rows[$terminal]["${source}_version"]
            $sourceRunId = [string]$Rows[$terminal]["${source}_run_id"]
            $sourcePresent = (
                $sourceExe -or $sourceHash -or $sourceVersion -or $sourceRunId
            )
            $sourceRequired = (
                $source -eq 'startup' -or
                ($source -eq 'latency' -and $LatencyRequired) -or
                (
                    $source -eq 'throughput' -and (
                        $null -ne (As-Double $Rows[$terminal].ascii_mbps) -or
                        $null -ne (As-Double $Rows[$terminal].sgr_mbps) -or
                        $null -ne (As-Double $Rows[$terminal].unicode_mbps)
                    )
                )
            )
            if (-not $sourcePresent -and -not $sourceRequired) {
                continue
            }
            if (
                -not $sourceExe -or
                -not $sourceVersion -or
                $sourceHash -notmatch '^[0-9a-fA-F]{64}$' -or
                -not $sourceRunId
            ) {
                $issues.Add(
                    "${Prefix}${terminal} $source result lacks run/binary provenance"
                )
                continue
            }
            if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
                $sourceExe,
                $manifestExe
            )) {
                $issues.Add(
                    "${Prefix}${terminal} $source executable differs from the manifest"
                )
            }
            if ($sourceVersion -ne $manifestVersion) {
                $issues.Add(
                    "${Prefix}${terminal} $source version differs from the manifest"
                )
            }
            if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
                $sourceHash,
                $manifestHash
            )) {
                $issues.Add(
                    "${Prefix}${terminal} $source executable hash differs from the manifest"
                )
            }
            if ($sourceRunId -ne $runId) {
                $issues.Add(
                    "${Prefix}${terminal} $source result belongs to a different run"
                )
            }
            if (
                $source -in @('latency', 'throughput') -and
                (
                    $manifestHelpers.Count -gt 0 -or
                    @(
                        $Rows[$terminal]["${source}_helper_binaries"] |
                            Where-Object { $null -ne $_ }
                    ).Count -gt 0
                ) -and
                (Get-JsonCollectionSignature (
                    $Rows[$terminal]["${source}_helper_binaries"]
                )) -ne (Get-JsonCollectionSignature $manifestHelpers)
            ) {
                $issues.Add(
                    "${Prefix}${terminal} $source helper binaries differ from the manifest"
                )
            }
            if (
                $manifestSchema -eq 3 -and
                $LatencyRequired -and
                $source -eq 'latency' -and
                (
                    -not [IO.Path]::IsPathRooted(
                        [string]$Rows[$terminal].latency_workload_executable
                    ) -or
                    -not [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$Rows[$terminal].latency_workload_executable,
                        $latencyWorkloadPath
                    ) -or
                    [string](
                        $Rows[$terminal].
                            latency_workload_executable_sha256
                    ) -cnotmatch '^[0-9A-Fa-f]{64}$' -or
                    -not [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string](
                            $Rows[$terminal].
                                latency_workload_executable_sha256
                        ),
                        $latencyWorkloadHash
                    )
                )
            ) {
                $issues.Add(
                    "${Prefix}${terminal} latency workload identity is invalid"
                )
            }
        }
    }
    return $issues
}

function Get-MenuProvenanceIssues {
    param(
        $Menu,
        $BenchmarkManifest,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    if ($null -eq $Menu -or $null -eq $BenchmarkManifest) {
        return $issues
    }
    $kettleRecord = @(Get-PropertyValue $BenchmarkManifest 'terminals' |
        Where-Object { (Get-PropertyValue $_ 'name') -eq 'kettle' })
    if ($kettleRecord.Count -ne 1) {
        $issues.Add("${Prefix}menu-hover has no unique Kettle manifest record")
        return $issues
    }
    $menuExe = [string](Get-PropertyValue $Menu 'executable')
    $menuHash = [string](Get-PropertyValue $Menu 'executable_sha256')
    $menuVersion = [string](Get-PropertyValue $Menu 'kettle_version')
    $menuRunId = [string](Get-PropertyValue $Menu 'run_id')
    $menuConfig = [string](Get-PropertyValue $Menu 'config')
    $menuConfigHash = [string](Get-PropertyValue $Menu 'config_sha256')
    $menuHelpers = @(
        Get-PropertyValue $Menu 'helper_binaries' |
            Where-Object { $null -ne $_ }
    )
    $manifestHelpers = @(
        Get-PropertyValue $kettleRecord[0] 'helper_binaries' |
            Where-Object { $null -ne $_ }
    )
    if (
        -not $menuExe -or
        -not $menuVersion -or
        $menuHash -notmatch '^[0-9a-fA-F]{64}$' -or
        -not $menuRunId
    ) {
        $issues.Add("${Prefix}menu-hover result lacks run/binary provenance")
        return $issues
    }
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $menuExe,
        [string](Get-PropertyValue $kettleRecord[0] 'executable')
    )) {
        $issues.Add("${Prefix}menu-hover executable differs from the manifest")
    }
    if ($menuVersion -ne [string](Get-PropertyValue $kettleRecord[0] 'version')) {
        $issues.Add("${Prefix}menu-hover version differs from the manifest")
    }
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $menuHash,
        [string](Get-PropertyValue $kettleRecord[0] 'executable_sha256')
    )) {
        $issues.Add("${Prefix}menu-hover executable hash differs from the manifest")
    }
    if ($menuRunId -ne [string](Get-PropertyValue $BenchmarkManifest 'run_id')) {
        $issues.Add("${Prefix}menu-hover result belongs to a different run")
    }
    if (
        (Get-JsonCollectionSignature $menuHelpers) -ne
            (Get-JsonCollectionSignature $manifestHelpers)
    ) {
        $issues.Add("${Prefix}menu-hover helper binaries differ from the manifest")
    }
    $manifestSchema = As-NonnegativeInt (
        Get-PropertyValue $BenchmarkManifest 'schema_version'
    )
    if ($manifestSchema -in @(2, 3)) {
        $manifestConfigHash = [string](
            Get-PropertyValue $BenchmarkManifest 'kettle_config_sha256'
        )
        $configuration = Get-PropertyValue $kettleRecord[0] 'configuration'
        $configurationFiles = @(
            Get-PropertyValue $configuration 'files'
        )
        $matchingFiles = @($configurationFiles | Where-Object {
            [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $_ 'sha256'),
                $menuConfigHash
            ) -and
            [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $_ 'path'),
                $menuConfig
            )
        })
        if (
            -not $menuConfig -or
            $menuConfigHash -notmatch '^[0-9a-fA-F]{64}$' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $menuConfigHash,
                $manifestConfigHash
            ) -or
            $matchingFiles.Count -ne 1
        ) {
            $issues.Add(
                "${Prefix}menu-hover configuration differs from the manifest"
            )
        }
        $displayTopology = Get-PropertyValue (
            Get-PropertyValue $BenchmarkManifest 'machine'
        ) 'display_topology'
        $targetScreen = [string](
            Get-PropertyValue $displayTopology 'target_screen_device'
        )
        if (
            -not $targetScreen -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $Menu 'target_screen_device'),
                $targetScreen
            )
        ) {
            $issues.Add(
                "${Prefix}menu-hover target screen differs from the manifest"
            )
        }
    }
    return $issues
}

function Get-VtebenchIssues {
    param(
        [string]$Directory,
        $BenchmarkManifest,
        [string[]]$Terminals,
        [string]$Prefix = ''
    )

    $issues = [Collections.Generic.List[string]]::new()
    $settings = Get-PropertyValue $BenchmarkManifest 'settings'
    if ((Get-PropertyValue $settings 'vtebench_enabled') -ne $true) {
        return $issues
    }
    $summary = Read-JsonFile (
        Join-Path $Directory 'vtebench-summary.json'
    )
    if ($null -eq $summary) {
        $issues.Add("${Prefix}vtebench summary is missing or invalid")
        return $issues
    }
    if ((As-NonnegativeInt (Get-PropertyValue $summary 'schema_version')) -ne 2) {
        $issues.Add("${Prefix}vtebench summary schema_version is not 2")
    }
    $manifestRunId = [string](Get-PropertyValue $BenchmarkManifest 'run_id')
    if ([string](Get-PropertyValue $summary 'run_id') -ne $manifestRunId) {
        $issues.Add("${Prefix}vtebench summary belongs to a different run")
    }
    $vtebenchRunner = Get-PropertyValue $summary 'workload_runner'
    $runnerPowerShell = Get-PropertyValue $vtebenchRunner 'powershell'
    $runnerScript = Get-PropertyValue $vtebenchRunner 'script'
    $toolchainPowerShell = Get-PropertyValue (
        Get-PropertyValue $BenchmarkManifest 'toolchain'
    ) 'throughput_powershell'
    $runnerHelper = Get-KettlePerfHarnessRecord `
        -BenchmarkManifest $BenchmarkManifest `
        -Name 'vtebench-inside.ps1'
    if (
        [string](Get-PropertyValue $summary 'transport_schema') -cne
            'kettle-vtebench-channel-v1' -or
        [string](Get-PropertyValue $vtebenchRunner 'schema') -cne
            'kettle-vtebench-runner-v1' -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $runnerPowerShell 'path'),
            [string](Get-PropertyValue $toolchainPowerShell 'path')
        ) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $runnerPowerShell 'sha256'),
            [string](Get-PropertyValue $toolchainPowerShell 'sha256')
        ) -or
        [string](Get-PropertyValue $runnerPowerShell 'version') -cne
            [string](Get-PropertyValue $toolchainPowerShell 'version') -or
        -not [IO.Path]::IsPathRooted(
            [string](Get-PropertyValue $runnerScript 'path')
        ) -or
        [IO.Path]::GetFileName(
            [string](Get-PropertyValue $runnerScript 'path')
        ) -cne 'vtebench-inside.ps1' -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $runnerScript 'sha256'),
            [string](Get-PropertyValue $runnerHelper 'sha256')
        )
    ) {
        $issues.Add("${Prefix}vtebench workload runner identity is invalid")
    }
    $source = Get-PropertyValue $summary 'source'
    $summaryWsl = Get-PropertyValue $source 'wsl_launcher'
    $manifestWsl = Get-PropertyValue (
        Get-PropertyValue $BenchmarkManifest 'toolchain'
    ) 'vtebench_wsl'
    $summaryDistribution = Get-PropertyValue `
        $summaryWsl 'distribution'
    $manifestDistribution = Get-PropertyValue `
        $manifestWsl 'distribution'
    $distributionMatches = $true
    foreach ($field in @(
        'schema',
        'name',
        'os_release_path',
        'os_release_sha256',
        'os_pretty_line',
        'os_version_line',
        'kernel_release',
        'kernel_version',
        'architecture',
        'user_name',
        'user_id'
    )) {
        if (
            [string](Get-PropertyValue $summaryDistribution $field) -cne
            [string](Get-PropertyValue $manifestDistribution $field)
        ) {
            $distributionMatches = $false
        }
    }
    if (
        -not (
            Test-KettlePerfWslLauncherIdentity `
                -Launcher $summaryWsl -RequireHash
        ) -or
        -not (
            Test-KettlePerfWslLauncherIdentity `
                -Launcher $manifestWsl -RequireHash
        ) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $summaryWsl 'path'),
            [string](Get-PropertyValue $manifestWsl 'path')
        ) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string](Get-PropertyValue $summaryWsl 'sha256'),
            [string](Get-PropertyValue $manifestWsl 'sha256')
        ) -or
        [string](Get-PropertyValue $summaryWsl 'version') -cne
            [string](Get-PropertyValue $manifestWsl 'version') -or
        [string](Get-PropertyValue $summaryWsl 'file_version') -cne
            [string](Get-PropertyValue $manifestWsl 'file_version') -or
        [string](Get-PropertyValue $summaryWsl 'runtime_version') -cne
            [string](Get-PropertyValue $manifestWsl 'runtime_version') -or
        [string](Get-PropertyValue $summaryWsl 'version_output') -cne
            [string](Get-PropertyValue $manifestWsl 'version_output') -or
        [string](
            Get-PropertyValue $summaryWsl 'version_output_sha256'
        ) -cne [string](
            Get-PropertyValue $manifestWsl 'version_output_sha256'
        ) -or
        [string](Get-PropertyValue $summaryWsl 'resolution_policy') -cne
            [string](Get-PropertyValue $manifestWsl 'resolution_policy') -or
        -not $distributionMatches
    ) {
        $issues.Add("${Prefix}vtebench WSL launcher identity is invalid")
    }
    $expectedCount = As-NonnegativeInt (
        Get-PropertyValue $source 'expected_benchmark_count'
    )
    $expectedRevision = [string](
        Get-PropertyValue $settings 'vtebench_revision'
    )
    $manifestSchema = As-NonnegativeInt (
        Get-PropertyValue $BenchmarkManifest 'schema_version'
    )
    if (
        $manifestSchema -eq 3 -and
        $expectedRevision -cne
            $script:KettlePerfReleaseVtebenchRevision
    ) {
        $issues.Add(
            "${Prefix}release vtebench revision is not the documented pin"
        )
    }
    $calculatedStateSignature =
        Get-KettlePerfVtebenchSourceStateSignature -Source $source
    $recordedStateSignature = [string](
        Get-PropertyValue $source 'source_state_sha256'
    )
    $deadlines = Get-PropertyValue $source 'deadlines_seconds'
    $deadlinesValid = (
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'setup'
        )) -eq 1800 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'generator'
        )) -eq 30 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'cargo_fetch'
        )) -eq 600 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'cargo_build'
        )) -eq 1200 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'preflight'
        )) -eq 120 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'source_validation'
        )) -eq 30 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'workload'
        )) -eq 900 -and
        (As-NonnegativeInt (
            Get-PropertyValue $deadlines 'cleanup'
        )) -eq 30
    )
    if (
        $null -eq $expectedCount -or
        $expectedCount -lt 1 -or
        [string](Get-PropertyValue $source 'revision') -ne $expectedRevision -or
        [string](Get-PropertyValue $source 'benchmark_tree') -notmatch
            '^[0-9a-fA-F]{40}$' -or
        [string](Get-PropertyValue $source 'wsl_binary_sha256') -notmatch
            '^[0-9a-fA-F]{64}$' -or
        [string](Get-PropertyValue $source 'cargo_lock_sha256') -notmatch
            '^[0-9a-fA-F]{64}$' -or
        -not ([string](Get-PropertyValue $source 'cargo_path')).StartsWith(
            '/'
        ) -or
        [string](Get-PropertyValue $source 'cargo_sha256') -notmatch
            '^[0-9a-fA-F]{64}$' -or
        -not (Get-PropertyValue $source 'cargo_version') -or
        (
            $manifestSchema -eq 3 -and
            (
                -not (
                    [string](
                        Get-PropertyValue $source 'rustup_path'
                    )
                ).StartsWith(
                    '/',
                    [StringComparison]::Ordinal
                ) -or
                [string](
                    Get-PropertyValue $source 'rustup_sha256'
                ) -cnotmatch '^[0-9a-f]{64}$' -or
                -not (Get-PropertyValue $source 'rustup_version')
            )
        ) -or
        (
            $manifestSchema -eq 3 -and
            (
                [string](
                    Get-PropertyValue $source 'timeout_sha256'
                ) -cnotmatch '^[0-9a-f]{64}$' -or
                -not (Get-PropertyValue $source 'timeout_version') -or
                -not (
                    [string](
                        Get-PropertyValue $source 'setsid_path'
                    )
                ).StartsWith(
                    '/',
                    [StringComparison]::Ordinal
                ) -or
                [string](
                    Get-PropertyValue $source 'setsid_sha256'
                ) -cnotmatch '^[0-9a-f]{64}$' -or
                -not (Get-PropertyValue $source 'setsid_version') -or
                -not (
                    [string](
                        Get-PropertyValue $source 'script_path'
                    )
                ).StartsWith(
                    '/',
                    [StringComparison]::Ordinal
                ) -or
                [string](
                    Get-PropertyValue $source 'script_sha256'
                ) -cnotmatch '^[0-9a-f]{64}$' -or
                -not (Get-PropertyValue $source 'script_version') -or
                [string](
                    Get-PropertyValue $source 'source_state_schema'
                ) -cne 'kettle-vtebench-source-state-v1' -or
                $null -eq $calculatedStateSignature -or
                $recordedStateSignature -cnotmatch
                    '^[0-9a-f]{64}$' -or
                $recordedStateSignature -cne
                    $calculatedStateSignature -or
                -not $deadlinesValid
            )
        )
    ) {
        $issues.Add("${Prefix}vtebench source provenance is invalid")
        return $issues
    }

    $summaryTerminals = Get-PropertyValue $summary 'terminals'
    $manifestTerminals = @(Get-PropertyValue $BenchmarkManifest 'terminals')
    foreach ($terminal in $Terminals) {
        $terminalProperty = if ($summaryTerminals) {
            $summaryTerminals.PSObject.Properties[$terminal]
        } else {
            $null
        }
        if (-not $terminalProperty) {
            $issues.Add("${Prefix}vtebench has no result for $terminal")
            continue
        }
        $result = $terminalProperty.Value
        $manifestRecord = @($manifestTerminals | Where-Object {
            (Get-PropertyValue $_ 'name') -eq $terminal
        })
        if ($manifestRecord.Count -ne 1) {
            $issues.Add(
                "${Prefix}vtebench has no unique manifest record for $terminal"
            )
            continue
        }
        $resultHash = [string](
            Get-PropertyValue $result 'executable_sha256'
        )
        if (
            [string](Get-PropertyValue $result 'run_id') -ne $manifestRunId -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string](Get-PropertyValue $result 'executable'),
                [string](Get-PropertyValue $manifestRecord[0] 'executable')
            ) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $resultHash,
                [string](
                    Get-PropertyValue $manifestRecord[0] 'executable_sha256'
                )
            ) -or
            [string](Get-PropertyValue $result 'product_version') -ne
                [string](Get-PropertyValue $manifestRecord[0] 'version')
        ) {
            $issues.Add(
                "${Prefix}vtebench $terminal run/binary provenance is invalid"
            )
        }
        if (
            $manifestSchema -eq 3 -and
            (
            [string](
                Get-PropertyValue $result 'source_state_before_sha256'
            ) -cne $recordedStateSignature -or
            [string](
                Get-PropertyValue $result 'source_state_after_sha256'
            ) -cne $recordedStateSignature
            )
        ) {
            $issues.Add(
                "${Prefix}vtebench $terminal source state changed during its leg"
            )
        }
        $benchmarkCount = As-NonnegativeInt (
            Get-PropertyValue $result 'benchmark_count'
        )
        $benchmarks = Get-PropertyValue $result 'benchmarks'
        $benchmarkProperties = if ($benchmarks) {
            @($benchmarks.PSObject.Properties)
        } else {
            @()
        }
        if (
            $benchmarkCount -ne $expectedCount -or
            $benchmarkProperties.Count -ne $expectedCount
        ) {
            $issues.Add(
                "${Prefix}vtebench $terminal benchmark coverage is incomplete"
            )
            continue
        }
        foreach ($benchmark in $benchmarkProperties) {
            $samples = @(
                Get-PropertyValue $benchmark.Value 'samples_ms'
            )
            $sampleCount = As-NonnegativeInt (
                Get-PropertyValue $benchmark.Value 'sample_count'
            )
            $validSamples = @($samples | Where-Object {
                $null -ne (As-Double $_ -AllowZero)
            })
            $reportedMedian = As-Double (
                Get-PropertyValue $benchmark.Value 'median_ms'
            ) -AllowZero
            $calculatedMedian = if ($validSamples.Count) {
                Get-KettlePerfMedian $validSamples
            } else {
                $null
            }
            if (
                $null -eq $sampleCount -or
                $sampleCount -lt 1 -or
                $samples.Count -ne $sampleCount -or
                $validSamples.Count -ne $sampleCount -or
                $null -eq $reportedMedian -or
                [Math]::Abs($reportedMedian - $calculatedMedian) -gt 0.001
            ) {
                $issues.Add(
                    "${Prefix}vtebench $terminal/$($benchmark.Name) samples are invalid"
                )
            }
        }
        $datLeaf = "vtebench-$terminal.dat"
        $datEntry = Read-KettlePerfEvidenceText `
            -Snapshot (Get-KettlePerfScoreEvidenceSnapshot $Directory) `
            -LeafName $datLeaf
        $recordedDatHash = [string](
            Get-PropertyValue $result 'dat_sha256'
        )
        if (
            $null -eq $datEntry -or
            $recordedDatHash -notmatch '^[0-9a-fA-F]{64}$'
        ) {
            $issues.Add("${Prefix}vtebench $terminal DAT evidence is missing")
            continue
        }
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            $recordedDatHash,
            [string]$datEntry.sha256
        )) {
            $issues.Add("${Prefix}vtebench $terminal DAT hash differs")
            continue
        }
        try {
            $parsedDat = Read-KettlePerfVtebenchDatText `
                -Text $datEntry.text -ExpectedColumns $expectedCount `
                -Source $datEntry.path
            if (
                (Get-JsonCollectionSignature $parsedDat.Names) -ne
                    (Get-JsonCollectionSignature $benchmarkProperties.Name)
            ) {
                throw 'DAT benchmark names differ from the summary'
            }
            foreach ($benchmark in $benchmarkProperties) {
                $summarySamples = @(
                    Get-PropertyValue $benchmark.Value 'samples_ms'
                )
                $datSamples = @($parsedDat.Samples[$benchmark.Name])
                if ($summarySamples.Count -ne $datSamples.Count) {
                    throw (
                        "DAT sample count differs for $($benchmark.Name)"
                    )
                }
                for (
                    $sampleIndex = 0;
                    $sampleIndex -lt $datSamples.Count;
                    $sampleIndex++
                ) {
                    if (
                        [double]$summarySamples[$sampleIndex] -ne
                            [double]$datSamples[$sampleIndex]
                    ) {
                        throw "DAT samples differ for $($benchmark.Name)"
                    }
                }
            }
        } catch {
            $issues.Add(
                "${Prefix}vtebench $terminal DAT is invalid: " +
                $_.Exception.Message
            )
        }
    }
    return $issues
}

function Get-VtebenchSourceSignature([string]$Directory) {
    $summary = Read-JsonFile (
        Join-Path $Directory 'vtebench-summary.json'
    )
    $source = Get-PropertyValue $summary 'source'
    if ($null -eq $source) {
        return ''
    }
    return Get-JsonCollectionSignature @([ordered]@{
        revision = Get-PropertyValue $source 'revision'
        benchmark_tree = Get-PropertyValue $source 'benchmark_tree'
        expected_benchmark_count = Get-PropertyValue `
            $source 'expected_benchmark_count'
        wsl_binary_sha256 = Get-PropertyValue $source 'wsl_binary_sha256'
        cargo_lock_sha256 = Get-PropertyValue $source 'cargo_lock_sha256'
        cargo_path = Get-PropertyValue $source 'cargo_path'
        cargo_sha256 = Get-PropertyValue $source 'cargo_sha256'
        cargo_version = Get-PropertyValue $source 'cargo_version'
    })
}

if (-not (Test-Path -LiteralPath $ResultsDir -PathType Container)) {
    throw "Results directory not found: $ResultsDir"
}
if ($BaselineResultsDir -and -not (Test-Path -LiteralPath $BaselineResultsDir -PathType Container)) {
    throw "Baseline results directory not found: $BaselineResultsDir"
}
if (@($RequiredTerminals | Select-Object -Unique).Count -ne $RequiredTerminals.Count) {
    throw 'RequiredTerminals contains duplicates'
}

$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
if ($BaselineResultsDir) {
    $BaselineResultsDir = (
        Resolve-Path -LiteralPath $BaselineResultsDir
    ).Path
}
$script:KettlePerfScoreEvidenceSnapshots = (
    [Collections.Generic.Dictionary[string, object]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
)
$currentEvidenceSnapshot = $null
$baselineEvidenceSnapshot = $null
$scoreExitCode = 0
try {
$currentEvidenceSnapshot = Open-KettlePerfEvidenceSnapshot `
    -Directory $ResultsDir
$script:KettlePerfScoreEvidenceSnapshots.Add(
    [string]$currentEvidenceSnapshot.root_path,
    $currentEvidenceSnapshot
)
if ($BaselineResultsDir) {
    if ([StringComparer]::OrdinalIgnoreCase.Equals(
        [string]$currentEvidenceSnapshot.root_path,
        $BaselineResultsDir
    )) {
        $baselineEvidenceSnapshot = $currentEvidenceSnapshot
    } else {
        $baselineEvidenceSnapshot = Open-KettlePerfEvidenceSnapshot `
            -Directory $BaselineResultsDir
        $script:KettlePerfScoreEvidenceSnapshots.Add(
            [string]$baselineEvidenceSnapshot.root_path,
            $baselineEvidenceSnapshot
        )
    }
}

$rows = Load-Perf $ResultsDir $RequiredTerminals
if (-not $rows.Contains('kettle')) {
    throw "No kettle results found in $ResultsDir"
}
$missingRequiredTerminals = @(
    $RequiredTerminals | Where-Object { -not $rows.Contains($_) }
)

$manifest = Read-JsonFile (Join-Path $ResultsDir 'benchmark-manifest.json')
$manifestIssues = [System.Collections.Generic.List[string]]::new()
$manifestSchema = $null
$benchmarkMode = ''
$manifestRunId = ''
$settings = $null
if ($null -eq $manifest) {
    $manifestIssues.Add('benchmark-manifest.json is missing or invalid')
} else {
    $manifestSchema = As-NonnegativeInt (
        Get-PropertyValue $manifest 'schema_version'
    )
    if ($manifestSchema -notin @(1, 2, 3)) {
        $manifestIssues.Add('benchmark manifest schema_version is unsupported')
    }
    $manifestRunId = [string](Get-PropertyValue $manifest 'run_id')
    if (
        $manifestRunId -notmatch (
            '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-' +
            '[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
        )
    ) {
        $manifestIssues.Add('benchmark manifest has no valid run id')
    }
    $repositoryCommit = [string](Get-PropertyValue $manifest 'repository_commit')
    if ($repositoryCommit -notmatch '^[0-9a-fA-F]{7,40}$') {
        $manifestIssues.Add('benchmark manifest has no valid repository commit')
    }
    $configSha256 = [string](Get-PropertyValue $manifest 'kettle_config_sha256')
    if ($configSha256 -notmatch '^[0-9a-fA-F]{64}$') {
        $manifestIssues.Add('benchmark manifest has no valid Kettle config hash')
    }
    $toolchain = Get-PropertyValue $manifest 'toolchain'
    foreach ($powerShellRole in @(
        'orchestrator_powershell',
        'throughput_powershell'
    )) {
        $powerShell = Get-PropertyValue $toolchain $powerShellRole
        $powerShellPath = [string](Get-PropertyValue $powerShell 'path')
        $powerShellHash = [string](
            Get-PropertyValue $powerShell 'sha256'
        )
        $powerShellEdition = [string](Get-PropertyValue $powerShell 'edition')
        $powerShellVersion = [string](Get-PropertyValue $powerShell 'version')
        $validPowerShellVersion = $false
        try {
            $validPowerShellVersion = ([version]$powerShellVersion).Major -ge 7
        } catch {
            $validPowerShellVersion = $false
        }
        if (
            -not $powerShellPath -or
            (
                $manifestSchema -eq 3 -and
                $powerShellHash -notmatch '^[0-9a-fA-F]{64}$'
            ) -or
            $powerShellEdition -ne 'Core' -or
            -not $validPowerShellVersion
        ) {
            $manifestIssues.Add(
                "benchmark manifest has no valid PowerShell 7 $powerShellRole"
            )
        }
    }
    if (
        $manifestSchema -eq 3 -and
        -not (
            Test-KettlePerfWslLauncherIdentity `
                -Launcher (Get-PropertyValue $toolchain 'vtebench_wsl') `
                -RequireHash
        )
    ) {
        $manifestIssues.Add(
            'benchmark manifest has no valid pinned WSL launcher'
        )
    }
    $dirty = Get-PropertyValue $manifest 'repository_dirty'
    if (-not $AllowDirtyManifest -and $dirty -ne $false) {
        $manifestIssues.Add('benchmark manifest does not identify a clean repository commit')
    }
    $machine = Get-PropertyValue $manifest 'machine'
    $displayTopology = Get-PropertyValue $machine 'display_topology'
    if ((Get-PropertyValue $displayTopology 'release_evidence_valid') -ne $true) {
        $manifestIssues.Add('benchmark display topology is not valid release evidence')
    }
    if ((Get-PropertyValue $displayTopology 'topology_stable') -ne $true) {
        $manifestIssues.Add('benchmark display topology was not stable for the full run')
    }
    if ($manifestSchema -eq 3) {
        foreach (
            $issue in Get-KettlePerfDisplayTopologyAcquisitionIssues `
                -BenchmarkManifest $manifest
        ) {
            $manifestIssues.Add($issue)
        }
    }
    $settings = Get-PropertyValue $manifest 'settings'
    $benchmarkMode = [string](Get-PropertyValue $settings 'mode')
    if ($benchmarkMode -eq 'release' -and $manifestSchema -ne 3) {
        $manifestIssues.Add('release scoring requires benchmark manifest schema 3')
    }
    if ($benchmarkMode -eq 'release') {
        $releaseTerminals = [string[]]@(
            'kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby'
        )
        if (
            $RequiredTerminals.Count -ne $releaseTerminals.Count -or
            @(
                $RequiredTerminals |
                    Where-Object { $_ -notin $releaseTerminals }
            ).Count -gt 0 -or
            @($RequiredTerminals | Select-Object -Unique).Count -ne
                $RequiredTerminals.Count
        ) {
            $manifestIssues.Add(
                'release scoring requires the complete six-terminal set'
            )
        }
        if (
            -not $RequireLatency -or
            -not $RequireMenuHover -or
            -not $RequireVtebench -or
            -not $RequireMonitorTransition
        ) {
            $manifestIssues.Add(
                'release scoring requires latency, menu, vtebench, and monitor gates'
            )
        }
        $releaseSettings = [ordered]@{
            startup_runs = 12
            idle_samples = 6
            idle_seconds = 10
            latency_samples = 60
            latency_block_size = 10
            max_latency_censored = 3
            latency_timeout_ms = 800
            menu_hover_samples = 200
            throughput_iterations = 6
            minimum_throughput_iterations = 6
        }
        foreach ($setting in $releaseSettings.GetEnumerator()) {
            if (
                (As-NonnegativeInt (
                    Get-PropertyValue $settings ([string]$setting.Key)
                )) -ne [int]$setting.Value
            ) {
                $manifestIssues.Add(
                    "release benchmark setting differs: $($setting.Key)"
                )
            }
        }
        if (
            (Get-PropertyValue $settings 'native_display_enabled') -ne $true
        ) {
            $manifestIssues.Add(
                'release scoring requires native-display Kettle evidence'
            )
        }
        if (
            (Get-PropertyValue $settings 'kettle_build_skipped') -ne $false
        ) {
            $manifestIssues.Add(
                'release scoring requires a Kettle build from this checkout'
            )
        }
    }
    if (
        $RequireVtebench -and
        (Get-PropertyValue $settings 'vtebench_enabled') -ne $true
    ) {
        $manifestIssues.Add('release scoring requires vtebench evidence')
    }
    if (
        $RequireMonitorTransition -and
        (Get-PropertyValue $settings 'monitor_transition_enabled') -ne $true
    ) {
        $manifestIssues.Add(
            'release scoring requires monitor-transition evidence'
        )
    }
    if ((Get-PropertyValue $settings 'unidentified_display_allowed') -eq $true) {
        $manifestIssues.Add('benchmark allowed an unidentified display')
    }
    $manifestTerminals = @(Get-PropertyValue $manifest 'terminals')
    foreach ($terminal in $RequiredTerminals) {
        $record = @($manifestTerminals | Where-Object {
            (Get-PropertyValue $_ 'name') -eq $terminal
        })
        if ($record.Count -ne 1) {
            $manifestIssues.Add("manifest has no unique terminal record for $terminal")
            continue
        }
        $record = $record[0]
        $manifestExe = [string](Get-PropertyValue $record 'executable')
        $manifestExeHash = [string](
            Get-PropertyValue $record 'executable_sha256'
        )
        $manifestVersion = [string](Get-PropertyValue $record 'version')
        if ((Get-PropertyValue $record 'available') -ne $true -or -not $manifestExe) {
            $manifestIssues.Add("manifest marks $terminal unavailable")
        }
        if (-not $manifestVersion) {
            $manifestIssues.Add("manifest has no product version for $terminal")
        }
        if ($manifestExeHash -notmatch '^[0-9a-fA-F]{64}$') {
            $manifestIssues.Add("manifest has no executable hash for $terminal")
        }
        if ($terminal -eq 'kettle' -and $manifestSchema -lt 3) {
            $source = Get-PropertyValue $record 'source'
            $embeddedCommit = [string](
                Get-PropertyValue $source 'embedded_commit'
            )
            $embeddedDirty = Get-PropertyValue $source 'embedded_dirty'
            $releaseBuildPerformed = Get-PropertyValue `
                $source 'release_build_performed'
            if (
                $embeddedCommit -notmatch '^[0-9a-fA-F]{40}$' -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    $embeddedCommit,
                    $repositoryCommit
                ) -or
                $embeddedDirty -ne $false -or
                $releaseBuildPerformed -ne $true
            ) {
                $manifestIssues.Add(
                    'Kettle binary source identity differs from the repository commit'
                )
            }
        }
        $helperBinaries = @(
            Get-PropertyValue $record 'helper_binaries' |
                Where-Object { $null -ne $_ }
        )
        $expectedHelperCount = if ($terminal -eq 'tabby') {
            2
        } elseif ($terminal -eq 'kettle') {
            1
        } else {
            0
        }
        if ($helperBinaries.Count -ne $expectedHelperCount) {
            $manifestIssues.Add(
                "manifest has unexpected helper-binary coverage for $terminal"
            )
        }
        foreach ($helper in $helperBinaries) {
            if (
                -not (Get-PropertyValue $helper 'role') -or
                -not [IO.Path]::IsPathRooted(
                    [string](Get-PropertyValue $helper 'path')
                ) -or
                [string](Get-PropertyValue $helper 'sha256') -notmatch
                    '^[0-9a-fA-F]{64}$'
            ) {
                $manifestIssues.Add(
                    "manifest has invalid helper-binary evidence for $terminal"
                )
            }
        }
        $configuration = Get-PropertyValue $record 'configuration'
        $configurationMode = [string](
            Get-PropertyValue $configuration 'mode'
        )
        $allowedConfigurationModes = if ($manifestSchema -in @(2, 3)) {
            if ($terminal -eq 'wt') {
                @('advisory-user-config', 'advisory-built-in-default')
            } else {
                @('benchmark-isolated', 'explicit')
            }
        } elseif ($terminal -eq 'kettle') {
            @('benchmark-isolated', 'explicit')
        } else {
            @('built-in-default', 'detected-user-config')
        }
        if ($configurationMode -notin $allowedConfigurationModes) {
            $manifestIssues.Add(
                "manifest has no valid configuration mode for $terminal"
            )
        }
        $claimEligible = Get-PropertyValue $configuration 'claim_eligible'
        if (
            $benchmarkMode -eq 'release' -and (
                (
                    $terminal -eq 'wt' -and
                    $claimEligible -ne $false
                ) -or
                (
                    $terminal -ne 'wt' -and (
                        $configurationMode -ne 'benchmark-isolated' -or
                        $claimEligible -ne $true
                    )
                )
            )
        ) {
            $manifestIssues.Add(
                "release configuration eligibility is invalid for $terminal"
            )
        }
        $configurationFiles = @(
            Get-PropertyValue $configuration 'files'
        )
        if (
            $configurationMode -in @(
                'benchmark-isolated',
                'explicit',
                'detected-user-config',
                'advisory-user-config'
            ) -and
            $configurationFiles.Count -eq 0
        ) {
            $manifestIssues.Add(
                "manifest has no configuration file evidence for $terminal"
            )
        }
        foreach ($configurationFile in $configurationFiles) {
            $configurationPath = [string](
                Get-PropertyValue $configurationFile 'path'
            )
            $configurationBytes = As-NonnegativeInt (
                Get-PropertyValue $configurationFile 'bytes'
            )
            $configurationHash = [string](
                Get-PropertyValue $configurationFile 'sha256'
            )
            if (
                -not $configurationPath -or
                $null -eq $configurationBytes -or
                $configurationHash -notmatch '^[0-9a-fA-F]{64}$'
            ) {
                $manifestIssues.Add(
                    "manifest has invalid configuration evidence for $terminal"
                )
            }
        }
        if (
            $terminal -eq 'kettle' -and
            @($configurationFiles | Where-Object {
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string](Get-PropertyValue $_ 'sha256'),
                    $configSha256
                )
            }).Count -ne 1
        ) {
            $manifestIssues.Add(
                'Kettle terminal configuration differs from the manifest config hash'
            )
        }
    }
    foreach (
        $issue in Get-ResultProvenanceIssues `
            $rows $manifest $RequiredTerminals ([bool]$RequireLatency)
    ) {
        $manifestIssues.Add($issue)
    }
    if ($manifestSchema -eq 3) {
        foreach (
            $issue in Get-KettlePerfCandidateManifestIssues `
                -BenchmarkManifest $manifest -ExpectedCandidate current
        ) {
            $manifestIssues.Add($issue)
        }
        foreach (
            $issue in Get-KettlePerfHarnessManifestIssues `
                -BenchmarkManifest $manifest
        ) {
            $manifestIssues.Add($issue)
        }
        foreach (
            $issue in Get-KettlePerfProbeConfigurationIssues `
                -Rows $rows -BenchmarkManifest $manifest `
                -Terminals $RequiredTerminals
        ) {
            $manifestIssues.Add($issue)
        }
        foreach (
            $issue in Get-KettlePerfReleaseScheduleIssues `
                -Rows $rows -BenchmarkManifest $manifest `
                -Terminals $RequiredTerminals
        ) {
            $manifestIssues.Add($issue)
        }
        foreach (
            $issue in Get-KettlePerfRawAggregateIssues `
                -Rows $rows -Terminals $RequiredTerminals
        ) {
            $manifestIssues.Add($issue)
        }
        foreach (
            $issue in Get-KettlePerfToolchainEvidenceIssues `
                -Rows $rows -BenchmarkManifest $manifest `
                -Terminals $RequiredTerminals
        ) {
            $manifestIssues.Add($issue)
        }
        foreach (
            $issue in Get-KettlePerfVtebenchOrderIssues `
                -BenchmarkManifest $manifest
        ) {
            $manifestIssues.Add($issue)
        }
    }
}

$primaryMetricDefs = @(
    @{ name = 'startup_ms'; higher = $false; weight = 1.25; allow_zero = $false },
    @{ name = 'idle_cpu_pct'; higher = $false; weight = 0.5; allow_zero = $true },
    @{ name = 'fresh_ws_mb'; higher = $false; weight = 0.5; allow_zero = $false }
)
if ($RequireLatency) {
    $primaryMetricDefs += @(
        @{ name = 'latency_ms'; higher = $false; weight = 1.5; allow_zero = $false },
        @{ name = 'latency_p95_ms'; higher = $false; weight = 1.5; allow_zero = $false }
    )
}
$allMetricNames = @(
    'ascii_mbps', 'sgr_mbps', 'unicode_mbps', 'postflood_ws_mb',
    'startup_ms', 'idle_cpu_pct', 'fresh_ws_mb',
    'latency_ms', 'latency_p95_ms'
)
$primaryTerms = @(
    $RequiredTerminals | Where-Object {
        if (-not $rows.Contains($_)) {
            return $false
        }
        $row = $rows[$_]
        foreach ($definition in $primaryMetricDefs) {
            if (
                $null -eq (
                    As-Double $row[$definition.name] -AllowZero:$definition.allow_zero
                )
            ) {
                return $false
            }
        }
        return $true
    }
)
if ('kettle' -notin $primaryTerms) {
    throw 'Kettle lacks complete startup/interactive data for primary ranking'
}

$scores = Score-Rows $rows $primaryMetricDefs $primaryTerms
$ranked = @($scores.Keys |
    Sort-Object @{ Expression = { $scores[$_].score }; Descending = $true }, @{ Expression = { $_ }; Ascending = $true } |
    ForEach-Object {
        [pscustomobject]@{
            terminal = $_
            score = $scores[$_].score
            metrics = $rows[$_]
            metric_scores = $scores[$_].metrics
        }
    })

$kettleRank = 1 + @($ranked | Where-Object { $_.score -gt $scores.kettle.score }).Count
$topHalfCutoff = [Math]::Ceiling($ranked.Count / 2.0)
$beaten = @($ranked | Where-Object { $_.score -lt $scores.kettle.score }).Count
$kettleLatency = As-Double $rows.kettle.latency_ms
$kettleLatencyP95 = As-Double $rows.kettle.latency_p95_ms
$kettleLatencyCoverage = Test-LatencyCoverage `
    $rows.kettle $MaxLatencyMissRate $MinimumLatencySamples
$latencyPeers = @(
    $RequiredTerminals | Where-Object {
        $_ -ne 'kettle' -and
        $rows.Contains($_) -and
        (Test-LatencyCoverage $rows[$_] $MaxLatencyMissRate $MinimumLatencySamples) -and
        $null -ne (As-Double $rows[$_].latency_ms) -and
        $null -ne (As-Double $rows[$_].latency_p95_ms)
    }
)
$latencyBeaten = if ($null -ne $kettleLatency -and $null -ne $kettleLatencyP95) {
    @($latencyPeers | Where-Object {
        (As-Double $rows[$_].latency_ms) -gt $kettleLatency -and
        (As-Double $rows[$_].latency_p95_ms) -gt $kettleLatencyP95
    }).Count
} else {
    0
}
$latencyPassed = (
    -not $RequireLatency -or (
        $null -ne $kettleLatency -and
        $null -ne $kettleLatencyP95 -and
        $kettleLatencyCoverage -and
        $latencyPeers.Count -ge $MinimumLatencyPeersBeaten -and
        $latencyBeaten -ge $MinimumLatencyPeersBeaten
    )
)
$menuHover = Read-JsonFile (Join-Path $ResultsDir 'menu-hover.json')
$requireMenuObservations = $manifestSchema -in @(2, 3)
$menuHoverDataValid = Test-MenuHoverCoverage `
    $menuHover $MinimumMenuHoverSamples `
    $MaxMenuHoverP95Ms $MaxMenuHoverP99Ms `
    $MenuHoverLongFrameMs $MaxMenuHoverLongFrames `
    -RequireObservations:$requireMenuObservations
if (
    $manifestSchema -in @(2, 3) -and
    (Get-PropertyValue $menuHover 'variant') -ne 'fixed-comparator'
) {
    $menuHoverDataValid = $false
    $manifestIssues.Add('fixed-size menu-hover variant is invalid')
}
$menuHoverPassed = -not $RequireMenuHover -or $menuHoverDataValid
if ($RequireMenuHover -and $manifest) {
    foreach ($issue in Get-MenuProvenanceIssues $menuHover $manifest) {
        $manifestIssues.Add($issue)
    }
}
$nativeMenuHover = Read-JsonFile (
    Join-Path $ResultsDir 'native-display-menu-hover.json'
)
$nativeMenuHoverRequired = $benchmarkMode -eq 'release'
$nativeMenuHoverIssues = [Collections.Generic.List[string]]::new()
$nativeMenuHoverDataValid = $false
if ($nativeMenuHoverRequired) {
    $nativeMenuHoverDataValid = Test-MenuHoverCoverage `
        $nativeMenuHover $MinimumMenuHoverSamples `
        $MaxMenuHoverP95Ms $MaxMenuHoverP99Ms `
        $MenuHoverLongFrameMs $MaxMenuHoverLongFrames `
        -RequireObservations
    if (
        (Get-PropertyValue $nativeMenuHover 'variant') -ne 'native-display'
    ) {
        $nativeMenuHoverDataValid = $false
        $nativeMenuHoverIssues.Add(
            'native-display menu-hover variant is invalid'
        )
    }
    $expectedNativeWindow = Get-PropertyValue `
        $settings 'native_window_pixels'
    $actualNativeWindow = Get-PropertyValue `
        $nativeMenuHover 'window_pixels'
    if (
        $null -eq $expectedNativeWindow -or
        (As-NonnegativeInt (
            Get-PropertyValue $actualNativeWindow 'width'
        )) -ne (As-NonnegativeInt (
            Get-PropertyValue $expectedNativeWindow 'width'
        )) -or
        (As-NonnegativeInt (
            Get-PropertyValue $actualNativeWindow 'height'
        )) -ne (As-NonnegativeInt (
            Get-PropertyValue $expectedNativeWindow 'height'
        ))
    ) {
        $nativeMenuHoverDataValid = $false
        $nativeMenuHoverIssues.Add(
            'native-display menu-hover window differs from the manifest'
        )
    }
    if ($manifest) {
        foreach (
            $issue in Get-MenuProvenanceIssues `
                $nativeMenuHover $manifest 'native-display '
        ) {
            $nativeMenuHoverIssues.Add($issue)
        }
    }
    if (-not $nativeMenuHoverDataValid) {
        $nativeMenuHoverIssues.Add(
            'native-display menu-hover observations are invalid'
        )
    }
    foreach ($issue in $nativeMenuHoverIssues) {
        $manifestIssues.Add($issue)
    }
}
$nativeMenuHoverPassed = (
    -not $nativeMenuHoverRequired -or
    (
        $nativeMenuHoverDataValid -and
        $nativeMenuHoverIssues.Count -eq 0
    )
)
$monitorTransition = Read-JsonFile (
    Join-Path $ResultsDir 'monitor-transition.json'
)
$monitorTransitionIssues = @()
if ($RequireMonitorTransition) {
    if ($manifest) {
        $monitorTransitionIssues = @(
            Get-MonitorTransitionIssues `
                $monitorTransition $manifest `
                $MinimumMonitorTransitionSamplesPerState `
                -MaximumP95Ms $MaxMonitorTransitionP95Ms `
                -MaximumMaxMs $MaxMonitorTransitionMaxMs
        )
        foreach ($issue in $monitorTransitionIssues) {
            $manifestIssues.Add($issue)
        }
    } else {
        $monitorTransitionIssues = @(
            'monitor-transition cannot be validated without a manifest'
        )
    }
}
$monitorTransitionPassed = (
    -not $RequireMonitorTransition -or
    $monitorTransitionIssues.Count -eq 0
)
$monitorTransitionPerformancePassed = (
    -not $RequireMonitorTransition -or
    @(
        $monitorTransitionIssues | Where-Object {
            $_ -like '*exceeds the configured limit'
        }
    ).Count -eq 0
)
if ($manifest) {
    foreach (
        $issue in Get-VtebenchIssues `
            $ResultsDir $manifest $RequiredTerminals
    ) {
        $manifestIssues.Add($issue)
    }
}

$coverageFailures = @(
    $RequiredTerminals | Where-Object {
        $terminal = $_
        if (-not $rows.Contains($terminal)) {
            return $false
        }
        $row = $rows[$terminal]
        $measured = @($allMetricNames | Where-Object {
            $metric = $_
            $allowZero = $metric -eq 'idle_cpu_pct'
            $null -ne (As-Double $row[$metric] -AllowZero:$allowZero)
        }).Count
        $startupValid = Test-StartupCoverage $row $MinimumStartupSamples
        $latencyValid = (
            -not $RequireLatency -or
            (Test-LatencyCoverage $row $MaxLatencyMissRate $MinimumLatencySamples)
        )
        (
            $terminal -notin $primaryTerms -or
            -not $startupValid -or
            -not $latencyValid -or
            $measured -lt $MinimumMetricsPerTerminal
        )
    } | ForEach-Object {
        $terminal = $_
        $row = $rows[$terminal]
        $measured = @($allMetricNames | Where-Object {
            $metric = $_
            $allowZero = $metric -eq 'idle_cpu_pct'
            $null -ne (As-Double $row[$metric] -AllowZero:$allowZero)
        }).Count
        [pscustomobject]@{
            terminal = $terminal
            measured_metrics = $measured
            startup_samples_valid = Test-StartupCoverage $row $MinimumStartupSamples
            latency_samples_valid = (
                -not $RequireLatency -or
                (Test-LatencyCoverage $row $MaxLatencyMissRate $MinimumLatencySamples)
            )
            primary_metrics_valid = $terminal -in $primaryTerms
        }
    }
)
$throughputTerms = @(
    $RequiredTerminals | Where-Object {
        $rows.Contains($_) -and
        (Test-ThroughputCoverage $rows[$_] $MinimumThroughputRuns)
    }
)
$throughputPeers = @($throughputTerms | Where-Object { $_ -ne 'kettle' })
$throughputMetricDefs = @(
    @{ name = 'ascii_mbps'; higher = $true; weight = 1.0; allow_zero = $false },
    @{ name = 'sgr_mbps'; higher = $true; weight = 1.0; allow_zero = $false },
    @{ name = 'unicode_mbps'; higher = $true; weight = 1.0; allow_zero = $false }
)
$throughputScores = if ('kettle' -in $throughputTerms) {
    Score-Rows $rows $throughputMetricDefs $throughputTerms
} else {
    [ordered]@{}
}
$throughputRanked = @($throughputScores.Keys |
    Sort-Object @{ Expression = { $throughputScores[$_].score }; Descending = $true },
        @{ Expression = { $_ }; Ascending = $true } |
    ForEach-Object {
        [pscustomobject]@{
            terminal = $_
            score = $throughputScores[$_].score
            metric_scores = $throughputScores[$_].metrics
        }
    })
$kettleThroughputRank = if ($throughputScores.Contains('kettle')) {
    1 + @($throughputRanked | Where-Object {
        $_.score -gt $throughputScores.kettle.score
    }).Count
} else {
    $null
}
$throughputBeaten = if ($throughputScores.Contains('kettle')) {
    @($throughputRanked | Where-Object {
        $_.score -lt $throughputScores.kettle.score
    }).Count
} else {
    0
}
$throughputPassed = (
    $null -ne $kettleThroughputRank -and
    $throughputPeers.Count -ge $MinimumThroughputPeersMeasured -and
    $kettleThroughputRank -le $MaxKettleThroughputRank -and
    $throughputBeaten -ge $MinimumThroughputPeersBeaten
)
$releaseStatisticsRequired = $benchmarkMode -eq 'release'
$releaseStatistics = $null
$releaseStatisticsPassed = -not $releaseStatisticsRequired
if ($releaseStatisticsRequired) {
    try {
        $benchmarkSeed = [string](
            Get-PropertyValue $settings 'benchmark_seed'
        )
        if (-not $benchmarkSeed) {
            throw 'manifest has no benchmark seed'
        }
        $releaseWindow = Get-PropertyValue $settings 'window_pixels'
        $releaseWindowWidth = As-NonnegativeInt (
            Get-PropertyValue $releaseWindow 'width'
        )
        $releaseWindowHeight = As-NonnegativeInt (
            Get-PropertyValue $releaseWindow 'height'
        )
        if (
            $null -eq $releaseWindowWidth -or
            $null -eq $releaseWindowHeight
        ) {
            throw 'manifest has no valid release window geometry'
        }
        $releaseStatistics = Get-KettlePerfReleaseStatisticalGate `
            -Rows $rows `
            -Seed "run:$manifestRunId|benchmark:$benchmarkSeed" `
            -StartupSamples 12 -IdleSamples 6 `
            -LatencySamples 60 -LatencyBlockSize 10 `
            -MaximumLatencyCensored 3 -LatencyTimeoutMs 800 `
            -ThroughputRounds 6 `
            -ExpectedWindowWidth $releaseWindowWidth `
            -ExpectedWindowHeight $releaseWindowHeight
        $releaseStatisticsPassed = [bool]$releaseStatistics.passed
        if (-not $releaseStatisticsPassed) {
            $manifestIssues.Add(
                'confirmed isolated-peer release statistics did not pass'
            )
        }
    } catch {
        $releaseStatisticsPassed = $false
        $manifestIssues.Add(
            "release statistical evidence is invalid: $($_.Exception.Message)"
        )
    }
}
$coveragePassed = (
    $missingRequiredTerminals.Count -eq 0 -and
    $coverageFailures.Count -eq 0 -and
    $manifestIssues.Count -eq 0
)

$baselineIssues = [System.Collections.Generic.List[string]]::new()
$baselineRows = $null
$baselineManifest = $null
$baselineMenu = $null
$baselineNativeMenu = $null
$baselineTransition = $null
$monitorTransitionBaselineApplied = (
    [bool]$BaselineResultsDir -and [bool]$RequireMonitorTransition
)
$monitorTransitionBaselineRequired = (
    $benchmarkMode -eq 'release' -and [bool]$RequireMonitorTransition
)
$monitorTransitionBaselineNonInferiority = $null
$monitorTransitionBaselineNonInferiorityPassed = -not (
    $monitorTransitionBaselineApplied -or
    $monitorTransitionBaselineRequired
)
if ($BaselineResultsDir) {
    $baselineRows = Load-Perf $BaselineResultsDir $RequiredTerminals
    if (-not $baselineRows.Contains('kettle')) {
        $baselineIssues.Add('baseline has no Kettle results')
    } else {
        if (
            -not (
                Test-StartupCoverage `
                    $baselineRows.kettle $MinimumStartupSamples
            )
        ) {
            $baselineIssues.Add('baseline Kettle startup samples are invalid')
        }
        if (
            -not (
                Test-ThroughputCoverage `
                    $baselineRows.kettle $MinimumThroughputRuns
            )
        ) {
            $baselineIssues.Add('baseline Kettle throughput samples are invalid')
        }
        if (
            $RequireLatency -and
            -not (
                Test-LatencyCoverage `
                    $baselineRows.kettle `
                    $MaxLatencyMissRate `
                    $MinimumLatencySamples
            )
        ) {
            $baselineIssues.Add('baseline Kettle latency samples are invalid')
        }
        foreach ($definition in $primaryMetricDefs) {
            if (
                $null -eq (
                    As-Double `
                        $baselineRows.kettle[$definition.name] `
                        -AllowZero:$definition.allow_zero
                )
            ) {
                $baselineIssues.Add(
                    "baseline Kettle metric is invalid: $($definition.name)"
                )
            }
        }
    }
    $baselineManifest = Read-JsonFile (
        Join-Path $BaselineResultsDir 'benchmark-manifest.json'
    )
    if ($null -eq $baselineManifest) {
        $baselineIssues.Add('baseline benchmark-manifest.json is missing or invalid')
    } else {
        $baselineManifestSchema = As-NonnegativeInt (
            Get-PropertyValue $baselineManifest 'schema_version'
        )
        if ($baselineManifestSchema -notin @(1, 2, 3)) {
            $baselineIssues.Add(
                'baseline manifest schema_version is unsupported'
            )
        }
        $baselineCommit = [string](
            Get-PropertyValue $baselineManifest 'repository_commit'
        )
        if ($baselineCommit -notmatch '^[0-9a-fA-F]{7,40}$') {
            $baselineIssues.Add('baseline has no valid repository commit')
        }
        if (
            -not $AllowDirtyManifest -and
            (Get-PropertyValue $baselineManifest 'repository_dirty') -ne $false
        ) {
            $baselineIssues.Add('baseline does not identify a clean repository commit')
        }
        $baselineMachine = Get-PropertyValue $baselineManifest 'machine'
        $baselineDisplay = Get-PropertyValue `
            $baselineMachine 'display_topology'
        if (
            (Get-PropertyValue $baselineDisplay 'release_evidence_valid') -ne
                $true
        ) {
            $baselineIssues.Add('baseline display topology is not valid release evidence')
        }
        if ((Get-PropertyValue $baselineDisplay 'topology_stable') -ne $true) {
            $baselineIssues.Add(
                'baseline display topology was not stable for the full run'
            )
        }
        if ($baselineManifestSchema -eq 3) {
            foreach (
                $issue in Get-KettlePerfDisplayTopologyAcquisitionIssues `
                    -BenchmarkManifest $baselineManifest -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
        }
        $baselineSettings = Get-PropertyValue $baselineManifest 'settings'
        if ($benchmarkMode -eq 'release') {
            if (
                $baselineManifestSchema -ne 3 -or
                (Get-PropertyValue $baselineSettings 'mode') -ne 'release'
            ) {
                $baselineIssues.Add(
                    'release baseline must use a schema 3 release manifest'
                )
            }
            $baselineReleaseSettings = [ordered]@{
                startup_runs = 12
                idle_samples = 6
                idle_seconds = 10
                latency_samples = 60
                latency_block_size = 10
                max_latency_censored = 3
                latency_timeout_ms = 800
                menu_hover_samples = 200
                throughput_iterations = 6
                minimum_throughput_iterations = 6
            }
            foreach ($setting in $baselineReleaseSettings.GetEnumerator()) {
                if (
                    (As-NonnegativeInt (
                        Get-PropertyValue `
                            $baselineSettings ([string]$setting.Key)
                    )) -ne [int]$setting.Value
                ) {
                    $baselineIssues.Add(
                        "release baseline setting differs: $($setting.Key)"
                    )
                }
            }
            if (
                (Get-PropertyValue `
                    $baselineSettings 'native_display_enabled') -ne $true
            ) {
                $baselineIssues.Add(
                    'release baseline lacks native-display Kettle evidence'
                )
            }
        }
        if (
            $RequireVtebench -and
            (Get-PropertyValue $baselineSettings 'vtebench_enabled') -ne $true
        ) {
            $baselineIssues.Add('baseline has no required vtebench evidence')
        }
        if (
            $RequireMonitorTransition -and
            (Get-PropertyValue `
                $baselineSettings 'monitor_transition_enabled') -ne $true
        ) {
            $baselineIssues.Add(
                'baseline has no required monitor-transition evidence'
            )
        }
        if (
            (Get-PropertyValue `
                $baselineSettings 'unidentified_display_allowed') -eq $true
        ) {
            $baselineIssues.Add('baseline allowed an unidentified display')
        }
        $baselineConfigSha = [string](
            Get-PropertyValue $baselineManifest 'kettle_config_sha256'
        )
        if ($baselineConfigSha -notmatch '^[0-9a-fA-F]{64}$') {
            $baselineIssues.Add('baseline has no valid Kettle config hash')
        }
        foreach (
            $issue in Get-ResultProvenanceIssues `
                $baselineRows $baselineManifest @('kettle') `
                ([bool]$RequireLatency) 'baseline '
        ) {
            $baselineIssues.Add($issue)
        }
        foreach (
            $issue in Get-VtebenchIssues `
                $BaselineResultsDir $baselineManifest `
                $RequiredTerminals 'baseline '
        ) {
            $baselineIssues.Add($issue)
        }
        if ($baselineManifestSchema -eq 3) {
            foreach (
                $issue in Get-KettlePerfCandidateManifestIssues `
                    -BenchmarkManifest $baselineManifest `
                    -ExpectedCandidate baseline -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
            foreach (
                $issue in Get-KettlePerfHarnessManifestIssues `
                    -BenchmarkManifest $baselineManifest -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
            foreach (
                $issue in Get-KettlePerfProbeConfigurationIssues `
                    -Rows $baselineRows `
                    -BenchmarkManifest $baselineManifest `
                    -Terminals $RequiredTerminals -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
            foreach (
                $issue in Get-KettlePerfReleaseScheduleIssues `
                    -Rows $baselineRows `
                    -BenchmarkManifest $baselineManifest `
                    -Terminals $RequiredTerminals -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
            foreach (
                $issue in Get-KettlePerfRawAggregateIssues `
                    -Rows $baselineRows -Terminals $RequiredTerminals `
                    -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
            foreach (
                $issue in Get-KettlePerfToolchainEvidenceIssues `
                    -Rows $baselineRows `
                    -BenchmarkManifest $baselineManifest `
                    -Terminals $RequiredTerminals -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
            foreach (
                $issue in Get-KettlePerfVtebenchOrderIssues `
                    -BenchmarkManifest $baselineManifest `
                    -Prefix 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
        }
        if ($manifest) {
            foreach (
                $issue in Compare-BenchmarkEnvironment $manifest $baselineManifest
            ) {
                $baselineIssues.Add($issue)
            }
            if (
                (Get-PropertyValue $baselineSettings 'vtebench_enabled') -eq
                    $true -and
                (Get-VtebenchSourceSignature $ResultsDir) -ne
                    (Get-VtebenchSourceSignature $BaselineResultsDir)
            ) {
                $baselineIssues.Add(
                    'baseline environment differs: vtebench_source'
                )
            }
        }
    }
    if ($RequireMenuHover) {
        $baselineMenu = Read-JsonFile (
            Join-Path $BaselineResultsDir 'menu-hover.json'
        )
        if (
            -not (
                Test-MenuHoverCoverage `
                    $baselineMenu `
                    $MinimumMenuHoverSamples `
                    $MaxMenuHoverP95Ms `
                    $MaxMenuHoverP99Ms `
                    $MenuHoverLongFrameMs `
                    $MaxMenuHoverLongFrames `
                    -RequireObservations:($baselineManifestSchema -in @(2, 3))
            )
        ) {
            $baselineIssues.Add('baseline menu-hover samples are invalid')
        }
        if (
            $baselineManifestSchema -in @(2, 3) -and
            (Get-PropertyValue $baselineMenu 'variant') -ne 'fixed-comparator'
        ) {
            $baselineIssues.Add('baseline fixed-size menu-hover variant is invalid')
        }
        if ($baselineManifest) {
            foreach (
                $issue in Get-MenuProvenanceIssues `
                    $baselineMenu $baselineManifest 'baseline '
            ) {
                $baselineIssues.Add($issue)
            }
        }
    }
    if ($benchmarkMode -eq 'release') {
        $baselineNativeMenu = Read-JsonFile (
            Join-Path $BaselineResultsDir 'native-display-menu-hover.json'
        )
        if (
            -not (
                Test-MenuHoverCoverage `
                    $baselineNativeMenu `
                    $MinimumMenuHoverSamples `
                    $MaxMenuHoverP95Ms `
                    $MaxMenuHoverP99Ms `
                    $MenuHoverLongFrameMs `
                    $MaxMenuHoverLongFrames `
                    -RequireObservations
            ) -or
            (Get-PropertyValue $baselineNativeMenu 'variant') -ne
                'native-display'
        ) {
            $baselineIssues.Add(
                'baseline native-display menu-hover samples are invalid'
            )
        }
        $expectedBaselineNativeWindow = Get-PropertyValue `
            $baselineSettings 'native_window_pixels'
        $actualBaselineNativeWindow = Get-PropertyValue `
            $baselineNativeMenu 'window_pixels'
        if (
            $null -eq $expectedBaselineNativeWindow -or
            (As-NonnegativeInt (
                Get-PropertyValue $actualBaselineNativeWindow 'width'
            )) -ne (As-NonnegativeInt (
                Get-PropertyValue $expectedBaselineNativeWindow 'width'
            )) -or
            (As-NonnegativeInt (
                Get-PropertyValue $actualBaselineNativeWindow 'height'
            )) -ne (As-NonnegativeInt (
                Get-PropertyValue $expectedBaselineNativeWindow 'height'
            ))
        ) {
            $baselineIssues.Add(
                'baseline native-display window differs from its manifest'
            )
        }
        if ($baselineManifest) {
            foreach (
                $issue in Get-MenuProvenanceIssues `
                    $baselineNativeMenu $baselineManifest `
                    'baseline native-display '
            ) {
                $baselineIssues.Add($issue)
            }
        }
    }
    if ($RequireMonitorTransition) {
        $baselineTransition = Read-JsonFile (
            Join-Path $BaselineResultsDir 'monitor-transition.json'
        )
        if ($baselineManifest) {
            foreach (
                $issue in Get-MonitorTransitionIssues `
                    $baselineTransition $baselineManifest `
                    $MinimumMonitorTransitionSamplesPerState 'baseline ' `
                    -MaximumP95Ms $MaxMonitorTransitionP95Ms `
                    -MaximumMaxMs $MaxMonitorTransitionMaxMs
            ) {
                $baselineIssues.Add($issue)
            }
        } else {
            $baselineIssues.Add(
                'baseline monitor-transition cannot be validated without a manifest'
            )
        }
        if (
            $null -ne $monitorTransition -and
            $null -ne $baselineTransition
        ) {
            try {
                $monitorTransitionBaselineNonInferiority = (
                    Get-KettlePerfMonitorTransitionBaselineNonInferiority `
                        -Current $monitorTransition `
                        -Baseline $baselineTransition `
                        -AbsoluteMarginMs (
                            $MonitorTransitionBaselineAbsoluteMarginMs
                        ) `
                        -RelativeMargin (
                            $MonitorTransitionBaselineRelativeMarginPct /
                                100.0
                        )
                )
                $monitorTransitionBaselineNonInferiorityPassed = [bool](
                    $monitorTransitionBaselineNonInferiority.passed
                )
            } catch {
                $monitorTransitionBaselineNonInferiorityPassed = $false
                $baselineIssues.Add(
                    (
                        'monitor-transition baseline non-inferiority ' +
                        "evidence is invalid: $($_.Exception.Message)"
                    )
                )
            }
        }
    }
}
$monitorTransitionBaselineIssue = (
    'monitor-transition p95/max baseline non-inferiority did not pass'
)
if (
    (
        $monitorTransitionBaselineApplied -or
        $monitorTransitionBaselineRequired
    ) -and
    -not $monitorTransitionBaselineNonInferiorityPassed -and
    -not $baselineIssues.Contains($monitorTransitionBaselineIssue)
) {
    $baselineIssues.Add($monitorTransitionBaselineIssue)
}
$baselineStatisticsRequired = (
    $benchmarkMode -eq 'release'
)
$baselineStatistics = $null
$baselineStatisticsPassed = -not $baselineStatisticsRequired
if ($baselineStatisticsRequired) {
    if (
        $null -eq $baselineRows -or
        $null -eq $baselineManifest
    ) {
        $baselineStatisticsPassed = $false
        $baselineIssues.Add(
            'release baseline statistics require complete baseline evidence'
        )
    } else {
        try {
            $baselineRunId = [string](
                Get-PropertyValue $baselineManifest 'run_id'
            )
            $baselineStatistics = Get-KettlePerfBaselineStatisticalGate `
                -CurrentRows $rows -BaselineRows $baselineRows `
                -Seed "current:$manifestRunId|baseline:$baselineRunId" `
                -StartupSamples 12 -IdleSamples 6 `
                -LatencySamples 60 -LatencyBlockSize 10 `
                -MaximumLatencyCensored 3 -LatencyTimeoutMs 800 `
                -ThroughputRounds 6 `
                -ExpectedWindowWidth $releaseWindowWidth `
                -ExpectedWindowHeight $releaseWindowHeight
            $baselineStatisticsPassed = [bool]$baselineStatistics.passed
            if (-not $baselineStatisticsPassed) {
                $baselineIssues.Add(
                    'paired baseline non-inferiority statistics did not pass'
                )
            }
        } catch {
            $baselineStatisticsPassed = $false
            $baselineIssues.Add(
                "baseline statistical evidence is invalid: $($_.Exception.Message)"
            )
        }
    }
}
$regressions = @(Regression-Report $rows $baselineRows $MaxRegressionPct)
$legacyPointGatesApplied = $benchmarkMode -ne 'release'
$legacyPointGatesPassed = (
    $kettleRank -le $MaxKettleRank -and
    $beaten -ge $MinimumPeersBeaten -and
    $throughputPassed -and
    $latencyPassed
)
$legacyRegressionGateApplied = (
    $benchmarkMode -ne 'release' -and
    [bool]$BaselineResultsDir
)
$legacyRegressionGatePassed = (
    -not $legacyRegressionGateApplied -or
    @($regressions).Count -eq 0
)
$overallPassed = if ($benchmarkMode -eq 'release') {
    (
        $coveragePassed -and
        $releaseStatisticsPassed -and
        $menuHoverPassed -and
        $nativeMenuHoverPassed -and
        $monitorTransitionPassed -and
        $baselineIssues.Count -eq 0 -and
        $baselineStatisticsPassed
    )
} else {
    (
        $legacyPointGatesPassed -and
        $coveragePassed -and
        $menuHoverPassed -and
        $monitorTransitionPassed -and
        $baselineIssues.Count -eq 0 -and
        $legacyRegressionGatePassed
    )
}

$result = [ordered]@{
    schema_version = 3
    timestamp = (Get-Date).ToString('o')
    results_dir = (Resolve-Path $ResultsDir).Path
    baseline_results_dir = if ($BaselineResultsDir) { (Resolve-Path $BaselineResultsDir).Path } else { $null }
    terminals = $ranked
    kettle_rank = $kettleRank
    max_kettle_rank = $MaxKettleRank
    top_half_cutoff = $topHalfCutoff
    terminals_beaten_by_kettle = $beaten
    minimum_peers_beaten = $MinimumPeersBeaten
    required_terminals = $RequiredTerminals
    missing_required_terminals = $missingRequiredTerminals
    minimum_metrics_per_terminal = $MinimumMetricsPerTerminal
    minimum_startup_samples = $MinimumStartupSamples
    dirty_manifest_allowed = [bool]$AllowDirtyManifest
    benchmark_mode = $benchmarkMode
    point_rank_is_advisory = $benchmarkMode -eq 'release'
    legacy_point_gates_applied = $legacyPointGatesApplied
    legacy_point_gates_passed = $legacyPointGatesPassed
    baseline_compatible = (
        $baselineIssues.Count -eq 0 -and
        $baselineStatisticsPassed
    )
    baseline_issues = $baselineIssues
    baseline_statistics_required = $baselineStatisticsRequired
    baseline_statistics_passed = $baselineStatisticsPassed
    baseline_statistics = $baselineStatistics
    legacy_regression_gate_applied = $legacyRegressionGateApplied
    legacy_regression_gate_passed = $legacyRegressionGatePassed
    coverage_failures = $coverageFailures
    manifest_issues = $manifestIssues
    throughput_peers_measured = $throughputPeers.Count
    minimum_throughput_peers_measured = $MinimumThroughputPeersMeasured
    throughput_terminals = $throughputRanked
    kettle_throughput_rank = $kettleThroughputRank
    max_kettle_throughput_rank = $MaxKettleThroughputRank
    throughput_peers_beaten_by_kettle = $throughputBeaten
    minimum_throughput_peers_beaten = $MinimumThroughputPeersBeaten
    minimum_throughput_runs = $MinimumThroughputRuns
    throughput_passed = $throughputPassed
    release_statistics_required = $releaseStatisticsRequired
    release_statistics_passed = $releaseStatisticsPassed
    release_statistics = $releaseStatistics
    coverage_passed = $coveragePassed
    latency_required = [bool]$RequireLatency
    max_latency_miss_rate = $MaxLatencyMissRate
    minimum_latency_samples = $MinimumLatencySamples
    kettle_latency_data_valid = $kettleLatencyCoverage
    latency_peers_measured = $latencyPeers.Count
    latency_peers_beaten_by_kettle = $latencyBeaten
    minimum_latency_peers_beaten = $MinimumLatencyPeersBeaten
    latency_passed = $latencyPassed
    menu_hover_required = [bool]$RequireMenuHover
    vtebench_required = [bool]$RequireVtebench
    minimum_menu_hover_samples = $MinimumMenuHoverSamples
    max_menu_hover_p95_ms = $MaxMenuHoverP95Ms
    max_menu_hover_p99_ms = $MaxMenuHoverP99Ms
    menu_hover_long_frame_ms = $MenuHoverLongFrameMs
    max_menu_hover_long_frames = $MaxMenuHoverLongFrames
    menu_hover_data_valid = $menuHoverDataValid
    menu_hover = $menuHover
    menu_hover_passed = $menuHoverPassed
    native_menu_hover_required = $nativeMenuHoverRequired
    native_menu_hover_data_valid = $nativeMenuHoverDataValid
    native_menu_hover = $nativeMenuHover
    native_menu_hover_issues = $nativeMenuHoverIssues
    native_menu_hover_passed = $nativeMenuHoverPassed
    monitor_transition_required = [bool]$RequireMonitorTransition
    minimum_monitor_transition_samples_per_state = (
        $MinimumMonitorTransitionSamplesPerState
    )
    monitor_transition_performance_limits = [ordered]@{
        p95_ms = $MaxMonitorTransitionP95Ms
        max_ms = $MaxMonitorTransitionMaxMs
        scopes = [string[]]@(
            'combined',
            'menu_closed',
            'context_menu_open'
        )
    }
    monitor_transition_performance_passed = (
        $monitorTransitionPerformancePassed
    )
    monitor_transition = $monitorTransition
    monitor_transition_issues = $monitorTransitionIssues
    monitor_transition_passed = $monitorTransitionPassed
    monitor_transition_baseline_non_inferiority_required = (
        $monitorTransitionBaselineRequired
    )
    monitor_transition_baseline_non_inferiority_applied = (
        $monitorTransitionBaselineApplied
    )
    monitor_transition_baseline_non_inferiority_passed = (
        $monitorTransitionBaselineNonInferiorityPassed
    )
    monitor_transition_baseline_non_inferiority = (
        $monitorTransitionBaselineNonInferiority
    )
    max_regression_pct = $MaxRegressionPct
    regressions = $regressions
    passed = $overallPassed
}

Write-Host "terminal        score"
Write-Host "----------------------"
foreach ($r in $ranked) {
    Write-Host ("{0,-14} {1,6:N4}" -f $r.terminal, $r.score)
}
Write-Host ""
Write-Host (
    "kettle rank: {0} of {1} (required <= {2}); peers beaten: {3} (required >= {4})" -f
    $kettleRank, $ranked.Count, $MaxKettleRank, $beaten, $MinimumPeersBeaten
)
Write-Host (
    "coverage: required terminals missing={0}; invalid primary data={1}; manifest issues={2}" -f
    $missingRequiredTerminals.Count, $coverageFailures.Count,
    $manifestIssues.Count
)
Write-Host (
    "throughput: kettle rank={0} (required <= {1}); peers measured={2}; beaten={3} (required >= {4})" -f
    $kettleThroughputRank, $MaxKettleThroughputRank,
    $throughputPeers.Count, $throughputBeaten, $MinimumThroughputPeersBeaten
)
if ($RequireLatency) {
    Write-Host (
        "latency peers: measured={0}; beaten={1} (required >= {2})" -f
        $latencyPeers.Count, $latencyBeaten, $MinimumLatencyPeersBeaten
    )
}
if ($RequireMenuHover) {
    Write-Host "menu-hover gate: $menuHoverPassed"
}
if ($nativeMenuHoverRequired) {
    Write-Host "native-display menu-hover gate: $nativeMenuHoverPassed"
}
if ($RequireMonitorTransition) {
    Write-Host "monitor-transition gate: $monitorTransitionPassed"
    Write-Host (
        'monitor-transition p95/max gate: ' +
        $monitorTransitionPerformancePassed
    )
}
if (
    $monitorTransitionBaselineApplied -or
    $monitorTransitionBaselineRequired
) {
    Write-Host (
        'monitor-transition baseline p95/max non-inferiority gate: ' +
        $monitorTransitionBaselineNonInferiorityPassed
    )
}
if ($releaseStatisticsRequired) {
    Write-Host (
        (
            'confirmed isolated-peer statistical gate: {0} ' +
            '(Windows Terminal is advisory)'
        ) -f $releaseStatisticsPassed
    )
}
if ($baselineStatisticsRequired) {
    Write-Host (
        "paired baseline non-inferiority gate: $baselineStatisticsPassed"
    )
}
if ($baselineIssues.Count -gt 0) {
    Write-Host "baseline compatibility issues:"
    $baselineIssues | ForEach-Object { Write-Host "  - $_" }
}
if (@($regressions).Count -gt 0) {
    Write-Host "regressions over $MaxRegressionPct%:"
    $regressions | Format-Table -AutoSize | Out-String | Write-Host
}

if (-not $OutJson) {
    $OutJson = Join-Path $ResultsDir 'score.json'
}
Write-KettlePerfJsonFile -Path $OutJson -InputObject $result -Depth 8

if (-not $result.passed) {
    $scoreExitCode = 1
}
} finally {
    if (
        $null -ne $baselineEvidenceSnapshot -and
        -not [object]::ReferenceEquals(
            $baselineEvidenceSnapshot,
            $currentEvidenceSnapshot
        )
    ) {
        Close-KettlePerfEvidenceSnapshot $baselineEvidenceSnapshot
    }
    if ($null -ne $currentEvidenceSnapshot) {
        Close-KettlePerfEvidenceSnapshot $currentEvidenceSnapshot
    }
    $script:KettlePerfScoreEvidenceSnapshots = $null
}
if ($scoreExitCode -ne 0) {
    exit $scoreExitCode
}
