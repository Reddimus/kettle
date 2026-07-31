# GUI-free contract and producer-guard tests for release acquisition.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\release-contract.ps1"

function Assert-ReleaseContract {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Assert-ReleaseContractObjectExact {
    param(
        [Parameter(Mandatory = $true)]
        $Actual,
        [Parameter(Mandatory = $true)]
        $Expected,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    $actualNames = [string[]]@($Actual.PSObject.Properties.Name)
    $expectedNames = [string[]]@($Expected.PSObject.Properties.Name)
    Assert-ReleaseContract `
        (Test-KettlePerfOrdinalSequenceEqual `
            -Actual $actualNames -Expected $expectedNames) `
        "$Description property sequence changed"
    foreach ($name in $expectedNames) {
        $actualValue = $Actual.$name
        $expectedValue = $Expected.$name
        Assert-ReleaseContract `
            ($null -ne $actualValue -and $null -ne $expectedValue) `
            "$Description field is unexpectedly null: $name"
        Assert-ReleaseContract `
            ($actualValue.GetType() -eq $expectedValue.GetType()) `
            "$Description field type changed: $name"
        $equal = if ($expectedValue -is [string]) {
            [StringComparer]::Ordinal.Equals(
                [string]$actualValue,
                [string]$expectedValue
            )
        } else {
            $actualValue.Equals($expectedValue)
        }
        Assert-ReleaseContract $equal `
            "$Description field value changed: $name"
    }
}

function Assert-ReleaseProducerRejection {
    param(
        [Parameter(Mandatory = $true)]
        [hashtable]$Arguments,
        [Parameter(Mandatory = $true)]
        [string]$ExpectedMessage,
        [Parameter(Mandatory = $true)]
        [string]$Case
    )

    try {
        & "$PSScriptRoot\perf-all.ps1" @Arguments
    } catch {
        if ($_.Exception.Message -cnotlike "*$ExpectedMessage*") {
            throw (
                "release producer rejected '$Case' for the wrong reason: " +
                $_.Exception.Message
            )
        }
        return
    }
    throw "release producer accepted forbidden '$Case'"
}

$contract = Get-KettlePerfReleaseAcquisitionContract
$second = Get-KettlePerfReleaseAcquisitionContract
Assert-ReleaseContract `
    ($contract.schema -ceq 'kettle-release-acquisition-contract-v2') `
    'release acquisition contract schema changed'
Assert-ReleaseContract `
    (Test-KettlePerfOrdinalSequenceEqual `
        -Actual $contract.terminals `
        -Expected @('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')) `
    'release terminal sequence changed'
Assert-ReleaseContract `
    (
        $contract.comparator_campaign.campaign_id -ceq
            'windows-x86_64-20260727T012800Z-d76cbf4b8173c691' -and
        $contract.comparator_campaign.relative_path -ceq (
            'windows-x86_64-20260727T012800Z-d76cbf4b8173c691/' +
            'campaign.json'
        ) -and
        [long]$contract.comparator_campaign.bytes -eq 6372 -and
        $contract.comparator_campaign.sha256 -ceq (
            'ee3637b8dec6deeb5b824f769e68b941' +
            '3a7777f1d82bccb751edbec33acabc55'
        )
    ) 'release comparator campaign identity changed'
Assert-ReleaseContract `
    (-not (Test-KettlePerfOrdinalSequenceEqual `
        -Actual @('wt', 'kettle') -Expected @('kettle', 'wt'))) `
    'ordinal sequence helper accepted reordered values'
Assert-ReleaseContract `
    (-not (Test-KettlePerfOrdinalSequenceEqual `
        -Actual @('Kettle') -Expected @('kettle'))) `
    'ordinal sequence helper accepted a case-only difference'
Assert-ReleaseContract `
    (-not (Test-KettlePerfOrdinalSequenceEqual `
        -Actual @('kettle') -Expected @('kettle', 'wt'))) `
    'ordinal sequence helper accepted a length difference'
Assert-ReleaseContract `
    (-not (Test-KettlePerfOrdinalSequenceEqual `
        -Actual @($null) -Expected @($null))) `
    'ordinal sequence helper accepted non-string values'
Assert-ReleaseContract `
    ($contract.benchmark_seed -ceq 'kettle-windows-release-v1') `
    'release benchmark seed changed'
Assert-ReleaseContract `
    ($contract.vtebench_revision -ceq (
        'ead80032e57dee2e75f0b51f2ea67528647d9944'
    )) `
    'release vtebench revision changed'
Assert-ReleaseContract `
    (
        $contract.startup_runs -eq 12 -and
        $contract.idle_samples -eq 6 -and
        $contract.idle_seconds -eq 10 -and
        $contract.latency_samples -eq 60 -and
        $contract.latency_block_size -eq 10 -and
        $contract.max_latency_censored -eq 3 -and
        $contract.latency_timeout_ms -eq 800 -and
        $contract.throughput_iterations -eq 6 -and
        $contract.minimum_throughput_iterations -eq 6 -and
        $contract.menu_hover_samples -eq 200 -and
        $contract.menu_hover_block_size -eq 20 -and
        $contract.monitor_transition_samples_per_state -eq 10 -and
        $contract.terminal_order_offset -eq 3 -and
        $contract.probe_cooldown_seconds -eq 15 -and
        $contract.window_pixels.width -eq 1280 -and
        $contract.window_pixels.height -eq 800
    ) `
    'release sample, timing, ordering, or window contract changed'

$scoreContract = Get-KettlePerfReleaseScoreContract
$expectedScoreContract = [pscustomobject][ordered]@{
    schema = [string]'kettle-release-score-contract-v1'
    max_regression_pct = [double]7.5
    max_kettle_rank = [int]2
    minimum_peers_beaten = [int]3
    minimum_metrics_per_terminal = [int]9
    minimum_throughput_peers_measured = [int]4
    max_kettle_throughput_rank = [int]2
    minimum_throughput_peers_beaten = [int]3
    minimum_startup_samples = [int]12
    minimum_throughput_runs = [int]6
    require_latency = [bool]$true
    minimum_latency_peers_beaten = [int]3
    minimum_latency_samples = [int]60
    max_latency_miss_rate = [double]0.05
    require_menu_hover = [bool]$true
    require_vtebench = [bool]$true
    require_monitor_transition = [bool]$true
    minimum_monitor_transition_samples_per_state = [int]10
    max_monitor_transition_p95_ms = [double]1000.0
    max_monitor_transition_max_ms = [double]2000.0
    monitor_transition_baseline_absolute_margin_ms = [double]100.0
    monitor_transition_baseline_relative_margin_pct = [double]25.0
    minimum_menu_hover_samples = [int]200
    max_menu_hover_p95_ms = [double]33.0
    max_menu_hover_p99_ms = [double]50.0
    menu_hover_long_frame_ms = [double]100.0
    max_menu_hover_long_frames = [int]1
    allow_dirty_manifest = [bool]$false
}
Assert-ReleaseContractObjectExact `
    -Actual $scoreContract -Expected $expectedScoreContract `
    -Description 'release score contract'

$contract.terminals[0] = 'mutated'
$contract.comparator_campaign.sha256 = '0' * 64
$contract.window_pixels.width = 1
Assert-ReleaseContract `
    ($second.terminals[0] -ceq 'kettle') `
    'release contract callers shared a mutable terminal array'
Assert-ReleaseContract `
    ($second.window_pixels.width -eq 1280) `
    'release contract callers shared a mutable nested object'
Assert-ReleaseContract `
    ($second.comparator_campaign.sha256 -ceq (
        'ee3637b8dec6deeb5b824f769e68b941' +
        '3a7777f1d82bccb751edbec33acabc55'
    )) 'release contract callers shared mutable comparator campaign state'

$methodologyMessage = 'Release mode requires the canonical acquisition methodology'
$producerCases = @(
    @{ Case = 'terminal order'; Arguments = @{
        Terminals = @('wt', 'kettle', 'alacritty', 'wezterm', 'rio', 'tabby')
    }},
    @{ Case = 'benchmark seed'; Arguments = @{
        BenchmarkSeed = 'noncanonical-seed'
    }},
    @{ Case = 'vtebench revision'; Arguments = @{
        VtebenchRevision = '0000000000000000000000000000000000000000'
    }},
    @{ Case = 'startup runs'; Arguments = @{ StartupRuns = 18 }},
    @{ Case = 'idle samples'; Arguments = @{ IdleSamples = 12 }},
    @{ Case = 'idle seconds'; Arguments = @{ IdleSeconds = 11 }},
    @{ Case = 'latency samples'; Arguments = @{ LatencySamples = 120 }},
    @{ Case = 'latency block size'; Arguments = @{ LatencyBlockSize = 20 }},
    @{ Case = 'maximum censored latency'; Arguments = @{
        MaxLatencyCensored = 4
    }},
    @{ Case = 'latency timeout'; Arguments = @{ LatencyTimeoutMs = 801 }},
    @{ Case = 'throughput iterations'; Arguments = @{
        ThroughputIterations = 12
        MinimumThroughputIterations = 6
    }},
    @{ Case = 'minimum throughput iterations'; Arguments = @{
        ThroughputIterations = 12
        MinimumThroughputIterations = 12
    }},
    @{ Case = 'hover samples'; Arguments = @{ HoverSamples = 201 }},
    @{ Case = 'monitor transition samples'; Arguments = @{
        MonitorTransitionSamples = 11
    }},
    @{ Case = 'terminal order offset'; Arguments = @{
        TerminalOrderOffset = 4
    }},
    @{ Case = 'probe cooldown'; Arguments = @{ ProbeCooldownSeconds = 14 }},
    @{ Case = 'window width'; Arguments = @{ WindowW = 1281 }},
    @{ Case = 'window height'; Arguments = @{ WindowH = 801 }}
)
foreach ($producerCase in $producerCases) {
    Assert-ReleaseProducerRejection `
        -Arguments $producerCase.Arguments `
        -ExpectedMessage $methodologyMessage `
        -Case $producerCase.Case
}

$skipMessage = 'Release mode does not permit manifest-only acquisition'
$skipCases = @(
    @{ Case = 'ManifestOnly'; Arguments = @{ ManifestOnly = $true }},
    @{ Case = 'SkipVtebench'; Arguments = @{ SkipVtebench = $true }},
    @{ Case = 'SkipLatency'; Arguments = @{ SkipLatency = $true }},
    @{ Case = 'SkipMenuHover'; Arguments = @{ SkipMenuHover = $true }},
    @{ Case = 'SkipNativeDisplay'; Arguments = @{ SkipNativeDisplay = $true }},
    @{ Case = 'SkipMonitorTransition'; Arguments = @{
        SkipMonitorTransition = $true
    }},
    @{ Case = 'AllowUnidentifiedDisplay'; Arguments = @{
        AllowUnidentifiedDisplay = $true
    }}
)
foreach ($skipCase in $skipCases) {
    Assert-ReleaseProducerRejection `
        -Arguments $skipCase.Arguments `
        -ExpectedMessage $skipMessage `
        -Case $skipCase.Case
}

Assert-ReleaseProducerRejection `
    -Arguments @{ KettleConfig = 'forbidden.toml' } `
    -ExpectedMessage 'generated isolated Kettle configuration' `
    -Case 'external Kettle configuration'
Assert-ReleaseProducerRejection `
    -Arguments @{ KettleExe = 'forbidden.exe' } `
    -ExpectedMessage 'current release candidate must be built' `
    -Case 'external current-candidate executable'
Assert-ReleaseProducerRejection `
    -Arguments @{ SkipKettleBuild = $true } `
    -ExpectedMessage 'current release candidate must be built' `
    -Case 'skipped current-candidate build'
Assert-ReleaseProducerRejection `
    -Arguments @{ KettleCandidate = 'baseline' } `
    -ExpectedMessage 'baseline release candidate requires' `
    -Case 'unpinned baseline candidate'
Assert-ReleaseProducerRejection `
    -Arguments @{ Mode = 'smoke'; KettleCandidate = 'baseline' } `
    -ExpectedMessage 'expected pins are release-only' `
    -Case 'smoke baseline candidate'

Write-Output 'release-contract self-test: PASS'
