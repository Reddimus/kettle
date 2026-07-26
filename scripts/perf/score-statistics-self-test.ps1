# GUI-free integration tests for raw score normalization and release policy.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\payload-contract.ps1"
. "$PSScriptRoot\statistics.ps1"
. "$PSScriptRoot\release-statistics.ps1"
. "$PSScriptRoot\baseline-statistics.ps1"
. "$PSScriptRoot\score-statistics.ps1"

function Assert-ScoreStatistics {
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

function New-ScoreStatisticsRows {
    param(
        [double]$KettleStartup = 50.0,
        [double]$KettleIdle = 0.10,
        [double]$KettleMemory = 50.0,
        [double]$KettleLatency = 5.0,
        [double]$KettleThroughput = 100.0,
        [switch]$KettleOnly
    )

    $names = if ($KettleOnly) {
        [string[]]@('kettle')
    } else {
        [string[]]@('kettle', 'alacritty', 'wezterm', 'rio', 'tabby')
    }
    $rows = [ordered]@{}
    foreach ($name in $names) {
        $terminalSlot = switch -CaseSensitive ($name) {
            'kettle' { 1; break }
            'alacritty' { 3; break }
            'wezterm' { 4; break }
            'rio' { 5; break }
            'tabby' { 6; break }
            default { throw "unexpected fixture terminal: $name" }
        }
        $isKettle = $name -ceq 'kettle'
        $startupValue = if ($isKettle) { $KettleStartup } else { 100.0 }
        $idleValue = if ($isKettle) { $KettleIdle } else { 0.50 }
        $memoryValue = if ($isKettle) { $KettleMemory } else { 100.0 }
        $latencyValue = if ($isKettle) { $KettleLatency } else { 20.0 }
        $throughputValue = if ($isKettle) {
            $KettleThroughput
        } else {
            50.0
        }
        $startup = [Collections.Generic.List[object]]::new()
        for ($sample = 1; $sample -le 12; $sample++) {
            $startup.Add([pscustomobject][ordered]@{
                terminal = $name
                metric = 'startup_ms'
                cluster_id = "startup-$sample"
                sample_id = (($sample - 1) * 6) + $terminalSlot
                sequence = $sample
                value = $startupValue
                status = 'ok'
                window_discovered_ms = 20.0
                sized_focused_ms = 30.0
                go_published_ms = 35.0
                go_to_ready_ms = $startupValue - 35.0
                post_endpoint_attribution_ms = 5.0
            })
        }
        $idle = [Collections.Generic.List[object]]::new()
        $workingSetBytes = [int64][Math]::Round($memoryValue * 1MB)
        for ($sample = 1; $sample -le 6; $sample++) {
            $idle.Add([pscustomobject][ordered]@{
                terminal = $name
                cluster_id = "idle-$sample"
                sample_id = (($sample - 1) * 6) + $terminalSlot
                sequence = $sample
                status = 'ok'
                idle_cpu_pct = $idleValue
                fresh_ws_mb = $memoryValue
                measured_seconds = 10.0
                workload_pid = 900
                excluded_pids = @(900)
                included_processes_before = @(
                    [pscustomobject]@{
                        pid = 100
                        process_name = 'terminal'
                        start_time_utc_ticks = 1000000
                        cpu_seconds = 1.0
                        working_set_bytes = $workingSetBytes
                    }
                )
                included_processes_after = @(
                    [pscustomobject]@{
                        pid = 100
                        process_name = 'terminal'
                        start_time_utc_ticks = 1000000
                        cpu_seconds = 1.0 + ($idleValue / 10.0)
                        working_set_bytes = $workingSetBytes
                    }
                )
                cpu_seconds_delta = $idleValue / 10.0
            })
        }
        $latency = [Collections.Generic.List[object]]::new()
        for ($block = 1; $block -le 6; $block++) {
            for ($sampleInBlock = 1; $sampleInBlock -le 10; $sampleInBlock++) {
                $terminalSample = (($block - 1) * 10) + $sampleInBlock
                $latency.Add([pscustomobject][ordered]@{
                    terminal = $name
                    metric = 'latency_ms'
                    cluster_id = "latency-$block"
                    block_id = "latency-block-$block"
                    sample_id = $block
                    sample_in_block = $sampleInBlock
                    terminal_sample = $terminalSample
                    sequence = $block
                    value = $latencyValue
                    status = 'ok'
                    timeout_ms = 800
                })
            }
        }
        $throughput = [Collections.Generic.List[object]]::new()
        for ($round = 1; $round -le 6; $round++) {
            foreach ($payload in @('ascii', 'sgr', 'unicode')) {
                $seconds = (
                    ([double]$KettlePerfPayloadContracts[$payload].bytes / 1MB) /
                    $throughputValue
                )
                $drainMs = 1.0
                $writeSeconds = $seconds - ($drainMs / 1000.0)
                $throughput.Add([pscustomobject][ordered]@{
                    terminal = $name
                    payload = $payload
                    metric = 'throughput_mb_per_s'
                    cluster_id = "throughput-$round"
                    sample_id = $round
                    sequence = $round
                    client_pixels = [pscustomobject]@{
                        width = 1280
                        height = 800
                    }
                    console_cells = [pscustomobject]@{
                        columns = 120
                        rows = 40
                    }
                    go_handshake = 'locked-create-new-token-v1'
                    go_wait_ms = 10.0
                    seconds = $seconds
                    write_seconds = $writeSeconds
                    drain_ms = $drainMs
                    value = $throughputValue
                    status = 'ok'
                })
            }
        }
        $rows[$name] = [ordered]@{
            startup_observations = [object[]]$startup.ToArray()
            idle_observations = [object[]]$idle.ToArray()
            latency_observations = [object[]]$latency.ToArray()
            throughput_observations = [object[]]$throughput.ToArray()
        }
    }
    return $rows
}

$rows = New-ScoreStatisticsRows
$release = Get-KettlePerfReleaseStatisticalGate `
    -Rows $rows -Seed 'score-statistics-self-test' `
    -BootstrapIterations 1000
Assert-ScoreStatistics $release.passed 'positive release fixture did not pass'
Assert-ScoreStatistics (
    $release.primary_policy.confirmed_wins -eq 4
) 'positive release fixture did not confirm all isolated peers'
Assert-ScoreStatistics (
    $release.advisory_terminals.Count -eq 1 -and
    $release.advisory_terminals[0] -ceq 'wt'
) 'Windows Terminal was not retained as advisory evidence'
Assert-ScoreStatistics (
    $release.throughput.round_gate.failed_round_composites -eq 0
) 'positive throughput fixture failed a matched round'
Assert-ScoreStatistics (
    $rows.tabby.startup_observations[-1].sample_id -eq 72 -and
    $rows.tabby.idle_observations[-1].sample_id -eq 36
) 'positive fixture did not exercise global Williams visit IDs'

$baselineRows = New-ScoreStatisticsRows -KettleOnly `
    -KettleStartup 49.0 -KettleIdle 0.09 -KettleMemory 49.0 `
    -KettleLatency 4.0 -KettleThroughput 105.0
$baseline = Get-KettlePerfBaselineStatisticalGate `
    -CurrentRows $rows -BaselineRows $baselineRows `
    -Seed 'score-statistics-baseline-self-test' `
    -BootstrapIterations 1000
Assert-ScoreStatistics $baseline.passed (
    'within-margin baseline fixture did not pass strict non-inferiority'
)
Assert-ScoreStatistics (
    $baseline.policy.required_metric_count -eq 5 -and
    $baseline.policy.provided_metric_count -eq 5
) 'baseline policy did not require all five metrics'

$tamperedRows = New-ScoreStatisticsRows
$tamperedRows.kettle.throughput_observations[0].status = 'OK'
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreThroughputMetric `
        -Rows $tamperedRows -Terminals @('kettle') -ExpectedRounds 6)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'non-exact throughput status was accepted'

$tamperedValueRows = New-ScoreStatisticsRows -KettleOnly
$tamperedValueRows.kettle.throughput_observations[0].value += 1.0
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreThroughputMetric `
        -Rows $tamperedValueRows -Terminals @('kettle') -ExpectedRounds 6)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'throughput value detached from bytes was accepted'

$tamperedTimingRows = New-ScoreStatisticsRows -KettleOnly
$tamperedTimingRows.kettle.throughput_observations[0].drain_ms += 1.0
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreThroughputMetric `
        -Rows $tamperedTimingRows -Terminals @('kettle') -ExpectedRounds 6)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'inconsistent throughput drain timing was accepted'

$tamperedGeometryRows = New-ScoreStatisticsRows -KettleOnly
$tamperedGeometryRows.kettle.throughput_observations[0].
    console_cells.columns++
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreThroughputMetric `
        -Rows $tamperedGeometryRows -Terminals @('kettle') -ExpectedRounds 6)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'inconsistent throughput geometry was accepted'

$tamperedMemoryRows = New-ScoreStatisticsRows -KettleOnly
$tamperedMemoryRows.kettle.idle_observations[0].fresh_ws_mb += 0.1
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreSimpleMetric `
        -Rows $tamperedMemoryRows -Terminals @('kettle') `
        -Metric fresh_ws_mb -ExpectedSamples 6)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'fresh working set detached from process evidence was accepted'

$duplicateSampleRows = New-ScoreStatisticsRows -KettleOnly
$duplicateSampleRows.kettle.startup_observations[1].sample_id = (
    $duplicateSampleRows.kettle.startup_observations[0].sample_id
)
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreSimpleMetric `
        -Rows $duplicateSampleRows -Terminals @('kettle') `
        -Metric startup_ms -ExpectedSamples 12)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'duplicate global sample_id was accepted'

$invalidSampleRows = New-ScoreStatisticsRows -KettleOnly
$invalidSampleRows.kettle.idle_observations[0].sample_id = 0
$threw = $false
try {
    [void](ConvertTo-KettlePerfScoreSimpleMetric `
        -Rows $invalidSampleRows -Terminals @('kettle') `
        -Metric idle_cpu_pct -ExpectedSamples 6)
} catch {
    $threw = $true
}
Assert-ScoreStatistics $threw 'nonpositive global sample_id was accepted'

Write-Host "score-statistics self-test: PASS ($($PSVersionTable.PSVersion))"
