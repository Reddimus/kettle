# Canonical acquisition methodology for publishable Windows performance
# evidence. Construct a new object for every caller so a mutation in one
# producer or scorer cannot alter the contract observed by another caller.

function Get-KettlePerfReleaseAcquisitionContract {
    return [pscustomobject][ordered]@{
        schema = 'kettle-release-acquisition-contract-v2'
        terminals = [string[]]@(
            'kettle',
            'wt',
            'alacritty',
            'wezterm',
            'rio',
            'tabby'
        )
        comparator_campaign = [pscustomobject][ordered]@{
            campaign_id = (
                'windows-x86_64-20260727T012800Z-d76cbf4b8173c691'
            )
            relative_path = (
                'windows-x86_64-20260727T012800Z-d76cbf4b8173c691/' +
                'campaign.json'
            )
            bytes = [long]6372
            sha256 = (
                'ee3637b8dec6deeb5b824f769e68b941' +
                '3a7777f1d82bccb751edbec33acabc55'
            )
        }
        benchmark_seed = 'kettle-windows-release-v1'
        vtebench_revision = (
            'ead80032e57dee2e75f0b51f2ea67528647d9944'
        )
        startup_runs = 12
        idle_samples = 6
        idle_seconds = 10
        latency_samples = 60
        latency_block_size = 10
        max_latency_censored = 3
        latency_timeout_ms = 800
        throughput_iterations = 6
        minimum_throughput_iterations = 6
        menu_hover_samples = 200
        menu_hover_block_size = 20
        monitor_transition_samples_per_state = 10
        terminal_order_offset = 3
        probe_cooldown_seconds = 15
        window_pixels = [pscustomobject][ordered]@{
            width = 1280
            height = 800
        }
    }
}

function Get-KettlePerfReleaseScoreContract {
    return [pscustomobject][ordered]@{
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
}

function Test-KettlePerfOrdinalSequenceEqual {
    param(
        [AllowNull()]
        [object[]]$Actual,
        [AllowNull()]
        [object[]]$Expected
    )

    if (
        $null -eq $Actual -or
        $null -eq $Expected -or
        $Actual.Count -ne $Expected.Count
    ) {
        return $false
    }
    for ($index = 0; $index -lt $Expected.Count; $index++) {
        if (
            $Actual[$index] -isnot [string] -or
            $Expected[$index] -isnot [string] -or
            -not [StringComparer]::Ordinal.Equals(
                [string]$Actual[$index],
                [string]$Expected[$index]
            )
        ) {
            return $false
        }
    }
    return $true
}
