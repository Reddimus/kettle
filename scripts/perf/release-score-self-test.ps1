# Deterministic, GUI-free integration coverage for schema-4 release scoring.
#
# The positive case invokes score.ps1 with its production 10,000-iteration
# bootstrap defaults. Negative cases clone and mutate complete result trees so
# each fail-closed assertion exercises the serialized score boundary.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\payload-contract.ps1"
. "$PSScriptRoot\schedule.ps1"
. "$PSScriptRoot\harness-provenance.ps1"
. "$PSScriptRoot\release-contract.ps1"
. "$PSScriptRoot\comparator-campaign.ps1"

$script:scoreScript = Join-Path $PSScriptRoot 'score.ps1'
$script:shell = (Get-Process -Id $PID).Path
$script:terminals = [string[]]@(
    'kettle',
    'wt',
    'alacritty',
    'wezterm',
    'rio',
    'tabby'
)
$script:repositoryCommit = '0123456789abcdef0123456789abcdef01234567'
$script:baselineCommit = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
$script:vtebenchRevision = 'ead80032e57dee2e75f0b51f2ea67528647d9944'
$script:targetScreen = '\\.\DISPLAY1'
$script:secondScreen = '\\.\DISPLAY2'
$script:thirdScreen = '\\.\DISPLAY3'
$script:fixedWidth = 1280
$script:fixedHeight = 800
$script:nativeWidth = 1920
$script:nativeHeight = 1080
$script:terminalHash = (
    Get-FileHash -LiteralPath $script:shell -Algorithm SHA256
).Hash
$script:latencyWorkload = Join-Path $env:SystemRoot 'System32\cmd.exe'
$script:latencyWorkloadHash = $script:terminalHash
$script:configurationEvidence = [ordered]@{}
$script:benchmarkSeed = 'kettle-windows-release-v1'
$script:timings = [Collections.Generic.List[object]]::new()
$script:releaseContract = Get-KettlePerfReleaseAcquisitionContract
$script:campaignRoot = Join-Path $PSScriptRoot 'campaigns'
$script:campaignPath = Join-Path `
    $script:campaignRoot `
    $script:releaseContract.comparator_campaign.relative_path
$script:campaign = Read-KettlePerfComparatorCampaign `
    -Path $script:campaignPath -ExpectedCampaignRoot $script:campaignRoot
$script:campaignEvidence = Get-KettlePerfComparatorCampaignEvidence `
    -Campaign $script:campaign

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$testRoot = Join-Path $tempRoot (
    'kettle-release-score-selftest-' + [guid]::NewGuid().ToString('N')
)

function Assert-ReleaseScore {
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

function Get-FixtureComparatorEntry {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal
    )

    if ($Terminal -ceq 'kettle') {
        return $null
    }
    return Get-KettlePerfComparatorCampaignEntry `
        -Campaign $script:campaign -Name $Terminal
}

function Get-FixtureTerminalPath {
    param([Parameter(Mandatory = $true)][string]$Terminal)

    if ($Terminal -ceq 'kettle') {
        return $script:shell
    }
    $entry = Get-FixtureComparatorEntry $Terminal
    if ($Terminal -ceq 'wt') {
        return Join-Path (
            'C:\Program Files\WindowsApps\' +
            "Microsoft.WindowsTerminal_$($entry.version)_x64__8wekyb3d8bbwe"
        ) $entry.executable.leaf
    }
    return Join-Path 'C:\KettleFixture\comparators' `
        $entry.executable.leaf
}

function Get-FixtureTerminalHash {
    param([Parameter(Mandatory = $true)][string]$Terminal)

    if ($Terminal -ceq 'kettle') {
        return $script:terminalHash
    }
    return [string](Get-FixtureComparatorEntry $Terminal).executable.sha256
}

function Get-FixtureTerminalVersion {
    param([Parameter(Mandatory = $true)][string]$Terminal)

    if ($Terminal -ceq 'kettle') {
        return 'fixture-kettle-1.0'
    }
    return [string](Get-FixtureComparatorEntry $Terminal).version
}

function Get-FixtureUtf8Sha256 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Text
    )

    $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return (
            [BitConverter]::ToString(
                $sha.ComputeHash($bytes)
            ).Replace('-', '').ToLowerInvariant()
        )
    } finally {
        $sha.Dispose()
        [Array]::Clear($bytes, 0, $bytes.Length)
    }
}

function New-FixtureWslLauncher {
    $versionOutput = 'WSL version: 2.7.3.0'
    return [pscustomobject][ordered]@{
        path = 'C:\Program Files\WSL\wsl.exe'
        sha256 = $script:terminalHash.ToLowerInvariant()
        version = '2.7.3.0'
        file_version = '2.7.3.0'
        runtime_version = '2.7.3.0'
        version_output = $versionOutput
        version_output_sha256 = Get-FixtureUtf8Sha256 $versionOutput
        resolution_policy = 'program-files-wsl-then-system32-v1'
        distribution = [pscustomobject][ordered]@{
            schema = 'kettle-wsl-distribution-v1'
            name = 'Ubuntu'
            os_release_path = '/usr/lib/os-release'
            os_release_sha256 = ('34' * 32)
            os_pretty_line = 'PRETTY_NAME="Ubuntu 24.04.2 LTS"'
            os_version_line = 'VERSION_ID="24.04"'
            kernel_release = '6.6.87.2-microsoft-standard-WSL2'
            kernel_version = '#1 SMP PREEMPT_DYNAMIC fixture'
            architecture = 'x86_64'
            user_name = 'fixture'
            user_id = 1000
        }
    }
}

function Get-FixtureVtebenchSourceStateSignature {
    param(
        [Parameter(Mandatory = $true)]
        $Source
    )

    $fields = [ordered]@{
        cache = $Source.wsl_cache
        build_root = $Source.wsl_build_root
        binary = $Source.wsl_binary
        revision = $Source.revision
        benchmark_tree = $Source.benchmark_tree
        binary_sha256 = $Source.wsl_binary_sha256
        cargo_lock_sha256 = $Source.cargo_lock_sha256
        cargo_path = $Source.cargo_path
        cargo_sha256 = $Source.cargo_sha256
        cargo_version = $Source.cargo_version
        rustup_path = $Source.rustup_path
        rustup_sha256 = $Source.rustup_sha256
        rustup_version = $Source.rustup_version
        timeout_path = $Source.timeout_path
        timeout_sha256 = $Source.timeout_sha256
        timeout_version = $Source.timeout_version
        setsid_path = $Source.setsid_path
        setsid_sha256 = $Source.setsid_sha256
        setsid_version = $Source.setsid_version
        script_path = $Source.script_path
        script_sha256 = $Source.script_sha256
        script_version = $Source.script_version
    }
    $text = [Text.StringBuilder]::new()
    foreach ($name in $fields.Keys) {
        [void]$text.Append($name)
        [void]$text.Append([char]0)
        [void]$text.Append([string]$fields[$name])
        [void]$text.Append("`n")
    }
    return Get-FixtureUtf8Sha256 $text.ToString()
}

function Get-FixtureDisplaySnapshotSignature {
    param(
        [Parameter(Mandatory = $true)]
        $Snapshot
    )

    $signatureValue = [pscustomobject][ordered]@{
        schema = $Snapshot.schema
        identity_acquisition = $Snapshot.identity_acquisition
        target_screen_device = $Snapshot.target_screen_device
        primary_screen_device = $Snapshot.primary_screen_device
        target_monitor_hardware_id = (
            $Snapshot.target_monitor_hardware_id
        )
        desktop_screens = $Snapshot.desktop_screens
        active_physical_monitors = $Snapshot.active_physical_monitors
        active_connections = $Snapshot.active_connections
        target_edid_monitors = $Snapshot.target_edid_monitors
        identity_issues = $Snapshot.identity_issues
    }
    return Get-FixtureUtf8Sha256 (
        ConvertTo-Json -InputObject $signatureValue -Compress -Depth 8
    )
}

function New-FixtureDisplaySnapshot {
    $targetMonitor = [pscustomobject][ordered]@{
        identity_source = 'wmi-monitor-id-v1'
        instance_name = 'DISPLAY\FIXTURE1\1'
        hardware_id = 'FIXTURE1'
        manufacturer_code = 'KTL'
        product_code = '0001'
        friendly_name = 'Kettle External Fixture'
        serial_number = 'FIXTURE-1'
        manufacture_week = 1
        manufacture_year = 2026
    }
    $secondMonitor = [pscustomobject][ordered]@{
        identity_source = 'wmi-monitor-id-v1'
        instance_name = 'DISPLAY\FIXTURE2\2'
        hardware_id = 'FIXTURE2'
        manufacturer_code = 'KTL'
        product_code = '0002'
        friendly_name = 'Kettle Internal Fixture'
        serial_number = 'FIXTURE-2'
        manufacture_week = 1
        manufacture_year = 2026
    }
    $thirdMonitor = [pscustomobject][ordered]@{
        identity_source = 'wmi-monitor-id-v1'
        instance_name = 'DISPLAY\FIXTURE3\3'
        hardware_id = 'FIXTURE3'
        manufacturer_code = 'KTL'
        product_code = '0003'
        friendly_name = 'Kettle High Contrast Fixture'
        serial_number = 'FIXTURE-3'
        manufacture_week = 1
        manufacture_year = 2026
    }
    $targetConnection = [pscustomobject][ordered]@{
        identity_source = 'wmi-monitor-connection-v1'
        instance_name = 'DISPLAY\FIXTURE1\1'
        hardware_id = 'FIXTURE1'
        video_output_technology = 10
    }
    $secondConnection = [pscustomobject][ordered]@{
        identity_source = 'wmi-monitor-connection-v1'
        instance_name = 'DISPLAY\FIXTURE2\2'
        hardware_id = 'FIXTURE2'
        video_output_technology = 0
    }
    $thirdConnection = [pscustomobject][ordered]@{
        identity_source = 'wmi-monitor-connection-v1'
        instance_name = 'DISPLAY\FIXTURE3\3'
        hardware_id = 'FIXTURE3'
        video_output_technology = 5
    }
    $snapshot = [pscustomobject][ordered]@{
        schema = 'kettle-display-topology-snapshot-v2'
        captured_at = '2026-07-26T12:00:00.0000000-07:00'
        identity_acquisition = [pscustomobject][ordered]@{
            schema = 'kettle-display-identity-acquisition-v2'
            resolver = 'wmi-monitor-id-with-ccd-registry-fallback-v2'
            method = 'wmi-monitor-id-v1'
            ccd_status = 'unavailable'
            desktop_screen_count = 3
            wmi_active_monitor_count = 3
            wmi_active_connection_count = 3
            ccd_active_path_count = 0
            resolved_screen_count = 3
        }
        target_screen_device = $script:targetScreen
        primary_screen_device = $script:targetScreen
        target_monitor_hardware_id = 'FIXTURE1'
        desktop_screens = [object[]]@(
            [pscustomobject][ordered]@{
                device_name = $script:targetScreen
                monitor_device_id = 'MONITOR\FIXTURE1\1'
                monitor_hardware_id = 'FIXTURE1'
                primary = $true
                edid_backed = $true
                edid_match_count = 1
                edid_monitor = $targetMonitor
                connection = $targetConnection
                effective_dpi = [pscustomobject][ordered]@{
                    x = 192
                    y = 192
                }
                scale_factor = 2.0
                refresh_hz = 60
                bounds = [pscustomobject][ordered]@{
                    x = 0
                    y = 0
                    width = $script:nativeWidth
                    height = $script:nativeHeight
                }
                working_area = [pscustomobject][ordered]@{
                    x = 0
                    y = 0
                    width = $script:nativeWidth
                    height = 1040
                }
                requested_client_fits = $true
            },
            [pscustomobject][ordered]@{
                device_name = $script:secondScreen
                monitor_device_id = 'MONITOR\FIXTURE2\2'
                monitor_hardware_id = 'FIXTURE2'
                primary = $false
                edid_backed = $true
                edid_match_count = 1
                edid_monitor = $secondMonitor
                connection = $secondConnection
                effective_dpi = [pscustomobject][ordered]@{
                    x = 144
                    y = 144
                }
                scale_factor = 1.5
                refresh_hz = 60
                bounds = [pscustomobject][ordered]@{
                    x = $script:nativeWidth
                    y = 0
                    width = 2560
                    height = 1440
                }
                working_area = [pscustomobject][ordered]@{
                    x = $script:nativeWidth
                    y = 0
                    width = 2560
                    height = 1400
                }
                requested_client_fits = $true
            },
            [pscustomobject][ordered]@{
                device_name = $script:thirdScreen
                monitor_device_id = 'MONITOR\FIXTURE3\3'
                monitor_hardware_id = 'FIXTURE3'
                primary = $false
                edid_backed = $true
                edid_match_count = 1
                edid_monitor = $thirdMonitor
                connection = $thirdConnection
                effective_dpi = [pscustomobject][ordered]@{
                    x = 96
                    y = 96
                }
                scale_factor = 1.0
                refresh_hz = 144
                bounds = [pscustomobject][ordered]@{
                    x = 4480
                    y = 0
                    width = 3840
                    height = 2160
                }
                working_area = [pscustomobject][ordered]@{
                    x = 4480
                    y = 0
                    width = 3840
                    height = 2120
                }
                requested_client_fits = $true
            }
        )
        active_physical_monitors = [object[]]@(
            $targetMonitor,
            $secondMonitor,
            $thirdMonitor
        )
        active_connections = [object[]]@(
            $targetConnection,
            $secondConnection,
            $thirdConnection
        )
        target_edid_monitors = [object[]]@($targetMonitor)
        identity_issues = [object[]]@()
    }
    Add-Member -InputObject $snapshot -NotePropertyName signature_sha256 `
        -NotePropertyValue (
            Get-FixtureDisplaySnapshotSignature -Snapshot $snapshot
        )
    return $snapshot
}

function New-FixtureDisplayTopology {
    $start = New-FixtureDisplaySnapshot
    $end = New-FixtureDisplaySnapshot
    $end.captured_at = '2026-07-26T12:30:00.0000000-07:00'
    $stabilityMonitoring = [pscustomobject][ordered]@{
        schema = 'kettle-display-stability-evidence-v1'
        provider = 'Microsoft.Win32.SystemEvents.DisplaySettingsChanged'
        monitoring_active_for_run = $true
        registration_error_type = $null
        display_change_events = [object[]]@()
        checkpoints = [object[]]@(
            [pscustomobject][ordered]@{
                phase = 'start'
                snapshot = $start
            },
            [pscustomobject][ordered]@{
                phase = 'end'
                snapshot = $end
            }
        )
        invalid_checkpoint_phases = [string[]]@()
        stable = $true
    }
    return [pscustomobject][ordered]@{
        acquisition_schema = 'kettle-display-topology-acquisition-v2'
        acquisition_start = $start
        acquisition_end = $end
        start_signature_sha256 = $start.signature_sha256
        end_signature_sha256 = $end.signature_sha256
        topology_stable = $true
        desktop_screens = [object[]]$start.desktop_screens
        target_screen_device = $start.target_screen_device
        active_physical_monitors = [object[]](
            $start.active_physical_monitors
        )
        active_connections = [object[]]$start.active_connections
        target_edid_monitors = [object[]]$start.target_edid_monitors
        requested_client_fits = $true
        native_client = [pscustomobject][ordered]@{
            width = $script:nativeWidth
            height = $script:nativeHeight
            fits = $true
        }
        start_evidence_valid = $true
        release_evidence_valid = $true
        stability_monitoring = $stabilityMonitoring
        issues = [object[]]@()
    }
}

function Set-FixtureNonPhysicalConnectionEvidence {
    param(
        [Parameter(Mandatory = $true)]
        $Topology
    )

    $technologies = @(15, 16, 17)
    for ($index = 0; $index -lt $technologies.Count; $index++) {
        $technology = $technologies[$index]
        $connection = $Topology.active_connections[$index]
        $connection.video_output_technology = $technology
        $screen = $Topology.desktop_screens[$index]
        $screen.connection.video_output_technology = $technology
        foreach ($monitor in @(
            $Topology.active_physical_monitors[$index],
            $screen.edid_monitor
        )) {
            if ($monitor.PSObject.Properties['output_technology']) {
                $monitor.output_technology = $technology
            } else {
                Add-Member -InputObject $monitor `
                    -NotePropertyName output_technology `
                    -NotePropertyValue $technology
            }
        }
        $targetMonitors = if (
            $Topology.PSObject.Properties['target_edid_monitors']
        ) {
            @($Topology.target_edid_monitors)
        } else {
            @()
        }
        foreach (
            $targetMonitor in $targetMonitors | Where-Object {
                [string]$_.instance_name -ceq
                    [string]$screen.edid_monitor.instance_name
            }
        ) {
            if ($targetMonitor.PSObject.Properties['output_technology']) {
                $targetMonitor.output_technology = $technology
            } else {
                Add-Member -InputObject $targetMonitor `
                    -NotePropertyName output_technology `
                    -NotePropertyValue $technology
            }
        }
    }
}

function Read-FixtureJson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

function Write-FixtureJson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        $Value
    )

    Write-KettlePerfJsonFile -Path $Path -InputObject $Value -Depth 16
}

function Set-FixtureMonitorTransitionUniformRecovery {
    param(
        [Parameter(Mandatory = $true)]
        $Transition,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0.001, 60000.0)]
        [double]$Value
    )

    $samplesPerState = [int]$Transition.requested.samples_per_state
    foreach ($observation in $Transition.observations) {
        $observation.recovery_to_capturable_client_ms = $Value
    }
    foreach ($stateName in @('menu_closed', 'context_menu_open')) {
        $summary = $Transition.states.PSObject.Properties[
            $stateName
        ].Value
        $summary.recovery_to_capturable_client_ms_all = [double[]]@(
            1..$samplesPerState | ForEach-Object { $Value }
        )
        $summary.recovery_to_capturable_client_ms_median = $Value
        $summary.recovery_to_capturable_client_ms_p95 = $Value
        $summary.recovery_to_capturable_client_ms_max = $Value
    }
    $Transition.recovery_to_capturable_client_ms_all = [double[]]@(
        1..($samplesPerState * 2) | ForEach-Object { $Value }
    )
    $Transition.recovery_to_capturable_client_ms_median = $Value
    $Transition.recovery_to_capturable_client_ms_p95 = $Value
    $Transition.recovery_to_capturable_client_ms_max = $Value
}

function Set-FixtureConfigurationEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory
    )

    $configurationDirectory = Join-Path $Directory 'isolated-configs'
    New-Item -ItemType Directory -Path $configurationDirectory | Out-Null
    $script:configurationEvidence = [ordered]@{}
    foreach ($terminal in $script:terminals) {
        $configurationPath = Join-Path `
            $configurationDirectory "$terminal.fixture-config"
        [IO.File]::WriteAllText(
            $configurationPath,
            "terminal=$terminal`nfont=Cascadia Mono`nfont_size=13`n",
            [Text.UTF8Encoding]::new($false)
        )
        $item = Get-Item -LiteralPath $configurationPath
        $script:configurationEvidence[$terminal] = (
            [pscustomobject][ordered]@{
                path = $item.FullName
                bytes = [int64]$item.Length
                sha256 = (
                    Get-FileHash -LiteralPath $item.FullName `
                        -Algorithm SHA256
                ).Hash
            }
        )
    }
}

function New-FixtureSchedules {
    return [ordered]@{
        startup = New-KettlePerfWilliamsSchedule `
            -Terminals $script:terminals `
            -Seed "$($script:benchmarkSeed):startup" `
            -Cycles 2 -Namespace 'startup'
        idle = New-KettlePerfWilliamsSchedule `
            -Terminals $script:terminals `
            -Seed "$($script:benchmarkSeed):idle" `
            -Cycles 1 -Namespace 'idle'
        latency = New-KettlePerfWilliamsSchedule `
            -Terminals $script:terminals `
            -Seed "$($script:benchmarkSeed):latency" `
            -Cycles 1 -Namespace 'latency'
        throughput = New-KettlePerfWilliamsSchedule `
            -Terminals $script:terminals `
            -Seed "$($script:benchmarkSeed):throughput" `
            -Cycles 1 -Namespace 'throughput'
    }
}

function Get-FixtureVisits {
    param(
        [Parameter(Mandatory = $true)]
        $Schedule,
        [Parameter(Mandatory = $true)]
        [string]$Terminal
    )

    return [object[]]@(
        $Schedule.rounds |
            ForEach-Object { $_.visits } |
            Where-Object { $_.terminal -ceq $Terminal }
    )
}

function New-FixtureHarnessProvenance {
    $records = [Collections.Generic.List[object]]::new()
    foreach ($name in Get-KettlePerfHarnessFileNames) {
        $path = Join-Path $PSScriptRoot $name
        $item = Get-Item -LiteralPath $path
        $records.Add([pscustomobject][ordered]@{
            path = $name
            bytes = [int64]$item.Length
            sha256 = (
                Get-FileHash -LiteralPath $path -Algorithm SHA256
            ).Hash.ToLowerInvariant()
        })
    }
    $aggregateText = [Text.StringBuilder]::new()
    foreach ($record in $records) {
        [void]$aggregateText.Append([string]$record.path)
        [void]$aggregateText.Append([char]0)
        [void]$aggregateText.Append([string]$record.sha256)
        [void]$aggregateText.Append("`n")
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash(
            [Text.UTF8Encoding]::new($false, $true).GetBytes(
                $aggregateText.ToString()
            )
        )
    } finally {
        $sha.Dispose()
    }
    return [pscustomobject][ordered]@{
        schema_version = 1
        lock_protocol = 'file-share-read-no-write-delete-v1'
        files = [object[]]$records
        aggregate_sha256 = (
            [BitConverter]::ToString($digest).Replace('-', '').
                ToLowerInvariant()
        )
    }
}

function New-FixtureHelpers {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal
    )

    if ($Terminal -ceq 'kettle') {
        return [object[]]@(
            [pscustomobject][ordered]@{
                role = 'kettle-cli'
                path = $script:shell
                sha256 = $script:terminalHash
            }
        )
    }
    if ($Terminal -ceq 'tabby') {
        return [object[]]@(
            [pscustomobject][ordered]@{
                role = 'command-shell'
                path = $script:shell
                sha256 = $script:terminalHash
            },
            [pscustomobject][ordered]@{
                role = 'command-launcher'
                path = $script:shell
                sha256 = $script:terminalHash
            }
        )
    }
    return [object[]]@()
}

function Get-FixturePerformance {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [ValidateSet('current', 'regressed-baseline')]
        [string]$KettlePerformance = 'current'
    )

    if ($Terminal -ceq 'kettle') {
        if ($KettlePerformance -ceq 'regressed-baseline') {
            return [pscustomobject][ordered]@{
                startup_ms = 10.0
                idle_cpu_pct = 0.01
                fresh_ws_mb = 10.0
                latency_ms = 1.0
                throughput_mbps = 200.0
                postflood_ws_mb = 30.0
            }
        }
        return [pscustomobject][ordered]@{
            startup_ms = 50.0
            idle_cpu_pct = 0.20
            fresh_ws_mb = 50.0
            latency_ms = 8.0
            throughput_mbps = 100.0
            postflood_ws_mb = 60.0
        }
    }

    if ($Terminal -ceq 'wt') {
        return [pscustomobject][ordered]@{
            startup_ms = 90.0
            idle_cpu_pct = 0.70
            fresh_ws_mb = 90.0
            latency_ms = 22.0
            throughput_mbps = 70.0
            postflood_ws_mb = 100.0
        }
    }

    $peerOffset = [array]::IndexOf(
        [string[]]@('alacritty', 'wezterm', 'rio', 'tabby'),
        $Terminal
    )
    if ($peerOffset -lt 0) {
        throw "unknown fixture terminal '$Terminal'"
    }
    return [pscustomobject][ordered]@{
        startup_ms = 115.0 + $peerOffset
        idle_cpu_pct = 1.00 + ($peerOffset * 0.05)
        fresh_ws_mb = 110.0 + $peerOffset
        latency_ms = 30.0 + $peerOffset
        throughput_mbps = 50.0 - $peerOffset
        postflood_ws_mb = 120.0 + $peerOffset
    }
}

function New-StartupObservation {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [Parameter(Mandatory = $true)]
        $Visit,
        [Parameter(Mandatory = $true)]
        [double]$Value
    )

    $windowDiscovered = $Value * 0.20
    $sizedFocused = $Value * 0.40
    $goPublished = $Value * 0.60
    return [pscustomobject][ordered]@{
        terminal = $Terminal
        metric = 'startup_ms'
        cluster_id = "c$($Visit.cycle)-r$($Visit.round)"
        sample_id = [int]$Visit.sample_id
        sample_key = [string]$Visit.sample_key
        cycle = [int]$Visit.cycle
        round = [int]$Visit.round
        round_in_cycle = [int]$Visit.round_in_cycle
        position = [int]$Visit.position
        sequence = [int]$Visit.sequence
        value = $Value
        status = 'ok'
        window_discovered_ms = $windowDiscovered
        sized_focused_ms = $sizedFocused
        go_published_ms = $goPublished
        go_to_ready_ms = $Value - $goPublished
        post_endpoint_attribution_ms = 0.5
    }
}

function New-IdleObservation {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [Parameter(Mandatory = $true)]
        $Visit,
        [Parameter(Mandatory = $true)]
        [double]$IdleCpu,
        [Parameter(Mandatory = $true)]
        [double]$FreshWorkingSet,
        [Parameter(Mandatory = $true)]
        [int]$TerminalIndex
    )

    $measuredSeconds = 10.0
    $cpuDelta = ($IdleCpu / 100.0) * $measuredSeconds
    $terminalPid = 10000 + $TerminalIndex
    $workloadPid = 20000 + $TerminalIndex
    $startTicks = 638800000000000000 + $TerminalIndex
    $workingSetBytes = [int64]($FreshWorkingSet * 1MB)
    return [pscustomobject][ordered]@{
        terminal = $Terminal
        cluster_id = "c$($Visit.cycle)-r$($Visit.round)"
        sample_id = [int]$Visit.sample_id
        sample_key = [string]$Visit.sample_key
        cycle = [int]$Visit.cycle
        round = [int]$Visit.round
        round_in_cycle = [int]$Visit.round_in_cycle
        position = [int]$Visit.position
        sequence = [int]$Visit.sequence
        status = 'ok'
        idle_cpu_pct = $IdleCpu
        fresh_ws_mb = $FreshWorkingSet
        workload_pid = $workloadPid
        excluded_pids = [int[]]@($workloadPid)
        cpu_seconds_delta = $cpuDelta
        measured_seconds = $measuredSeconds
        included_processes_before = [object[]]@(
            [pscustomobject][ordered]@{
                pid = $terminalPid
                process_name = "fixture-$Terminal"
                start_time_utc_ticks = $startTicks
                cpu_seconds = 10.0
                working_set_bytes = $workingSetBytes
            }
        )
        included_processes_after = [object[]]@(
            [pscustomobject][ordered]@{
                pid = $terminalPid
                process_name = "fixture-$Terminal"
                start_time_utc_ticks = $startTicks
                cpu_seconds = 10.0 + $cpuDelta
                working_set_bytes = $workingSetBytes
            }
        )
    }
}

function New-LatencyObservation {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [Parameter(Mandatory = $true)]
        $Visit,
        [Parameter(Mandatory = $true)]
        [int]$SampleInBlock,
        [Parameter(Mandatory = $true)]
        [double]$Value
    )

    $terminalSample = (([int]$Visit.round - 1) * 10) + $SampleInBlock
    return [pscustomobject][ordered]@{
        terminal = $Terminal
        metric = 'latency_ms'
        cluster_id = "c$($Visit.cycle)-r$($Visit.round)"
        block_id = [string]$Visit.sample_key
        sample_id = [int]$Visit.sample_id
        sample_in_block = $SampleInBlock
        terminal_sample = $terminalSample
        cycle = [int]$Visit.cycle
        round = [int]$Visit.round
        round_in_cycle = [int]$Visit.round_in_cycle
        position = [int]$Visit.position
        sequence = [int]$Visit.sequence
        value = $Value
        status = 'ok'
        timeout_ms = 800
    }
}

function New-ThroughputObservation {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [Parameter(Mandatory = $true)]
        [string]$Payload,
        [Parameter(Mandatory = $true)]
        $Visit,
        [Parameter(Mandatory = $true)]
        [double]$Value,
        [Parameter(Mandatory = $true)]
        [double]$Seconds,
        [Parameter(Mandatory = $true)]
        [double]$WriteSeconds
    )

    return [pscustomobject][ordered]@{
        terminal = $Terminal
        payload = $Payload
        metric = 'throughput_mb_per_s'
        cluster_id = "c$($Visit.cycle)-r$($Visit.round)"
        sample_id = [int]$Visit.sample_id
        sample_key = [string]$Visit.sample_key
        cycle = [int]$Visit.cycle
        round = [int]$Visit.round
        round_in_cycle = [int]$Visit.round_in_cycle
        position = [int]$Visit.position
        sequence = [int]$Visit.sequence
        payload_order = [string[]]@('ascii', 'sgr', 'unicode')
        client_pixels = [pscustomobject][ordered]@{
            width = $script:fixedWidth
            height = $script:fixedHeight
        }
        console_cells = [pscustomobject][ordered]@{
            columns = 120
            rows = 40
        }
        go_handshake = 'locked-create-new-token-v1'
        go_wait_ms = 2.0
        seconds = $Seconds
        write_seconds = $WriteSeconds
        drain_ms = 1.0
        value = $Value
        status = 'ok'
    }
}

function Write-StartupLatencyThroughputEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory,
        [Parameter(Mandatory = $true)]
        [string]$RunId,
        [ValidateSet('current', 'regressed-baseline')]
        [string]$KettlePerformance = 'current'
    )

    $schedules = New-FixtureSchedules
    $startup = [ordered]@{}
    $latency = [ordered]@{}
    $startupHelperPath = Join-Path $PSScriptRoot 'startup-ready.ps1'
    $runnerPath = Join-Path $PSScriptRoot 'run-inside.ps1'
    $startupHelperHash = (
        Get-FileHash -LiteralPath $startupHelperPath -Algorithm SHA256
    ).Hash
    $runnerHash = (
        Get-FileHash -LiteralPath $runnerPath -Algorithm SHA256
    ).Hash
    $workloadRunner = [pscustomobject][ordered]@{
        schema = 'kettle-throughput-runner-v1'
        powershell = [pscustomobject][ordered]@{
            path = $script:shell
            sha256 = $script:terminalHash
            version = '7.5.0'
        }
        script = [pscustomobject][ordered]@{
            path = $runnerPath
            sha256 = $runnerHash
        }
    }
    for ($terminalIndex = 0; $terminalIndex -lt $script:terminals.Count; $terminalIndex++) {
        $terminal = $script:terminals[$terminalIndex]
        $terminalPath = Get-FixtureTerminalPath $terminal
        $terminalHash = Get-FixtureTerminalHash $terminal
        $version = Get-FixtureTerminalVersion $terminal
        $helpers = [object[]]@(New-FixtureHelpers -Terminal $terminal)
        $performance = Get-FixturePerformance `
            -Terminal $terminal -KettlePerformance $KettlePerformance

        $startupObservations = [Collections.Generic.List[object]]::new()
        $startupVisits = Get-FixtureVisits `
            -Schedule $schedules.startup -Terminal $terminal
        foreach ($visit in $startupVisits) {
            $startupObservations.Add(
                (New-StartupObservation `
                    -Terminal $terminal -Visit $visit `
                    -Value ([double]$performance.startup_ms))
            )
        }
        $idleObservations = [Collections.Generic.List[object]]::new()
        $idleVisits = Get-FixtureVisits `
            -Schedule $schedules.idle -Terminal $terminal
        foreach ($visit in $idleVisits) {
            $idleObservations.Add(
                (New-IdleObservation `
                    -Terminal $terminal -Visit $visit `
                    -IdleCpu ([double]$performance.idle_cpu_pct) `
                    -FreshWorkingSet ([double]$performance.fresh_ws_mb) `
                    -TerminalIndex $terminalIndex)
            )
        }
        $startup[$terminal] = [pscustomobject][ordered]@{
            run_id = $RunId
            executable = $terminalPath
            executable_sha256 = $terminalHash
            product_version = $version
            configuration_mode = if ($terminal -ceq 'wt') {
                'uncontrolled'
            } else {
                'benchmark-isolated'
            }
            configuration_evidence = if ($terminal -ceq 'wt') {
                $null
            } else {
                $script:configurationEvidence[$terminal]
            }
            helper_binaries = $helpers
            startup_schedule_algorithm = $schedules.startup.algorithm
            startup_schedule_seed_sha256 = $schedules.startup.seed_sha256
            idle_schedule_algorithm = $schedules.idle.algorithm
            idle_schedule_seed_sha256 = $schedules.idle.seed_sha256
            startup_ms_all = [double[]]@(
                1..12 | ForEach-Object {
                    [double]$performance.startup_ms
                }
            )
            startup_samples = 12
            startup_requested_samples = 12
            startup_misses = 0
            startup_ms_median = [double]$performance.startup_ms
            startup_observations = [object[]]$startupObservations
            fresh_ws_mb = [double]$performance.fresh_ws_mb
            fresh_ws_mb_all = [double[]]@(
                1..6 | ForEach-Object {
                    [double]$performance.fresh_ws_mb
                }
            )
            idle_cpu_pct = [double]$performance.idle_cpu_pct
            idle_cpu_pct_all = [double[]]@(
                1..6 | ForEach-Object {
                    [double]$performance.idle_cpu_pct
                }
            )
            idle_observations = [object[]]$idleObservations
            readiness = [pscustomobject][ordered]@{
                schema = 'kettle-startup-ready-v1'
                shell = $script:shell
                shell_sha256 = $script:terminalHash
                helper_script = $startupHelperPath
                helper_script_sha256 = $startupHelperHash
            }
        }

        $latencyObservations = [Collections.Generic.List[object]]::new()
        $latencyVisits = Get-FixtureVisits `
            -Schedule $schedules.latency -Terminal $terminal
        foreach ($visit in $latencyVisits) {
            for ($sampleInBlock = 1; $sampleInBlock -le 10; $sampleInBlock++) {
                $latencyObservations.Add(
                    (New-LatencyObservation `
                        -Terminal $terminal -Visit $visit `
                        -SampleInBlock $sampleInBlock `
                        -Value ([double]$performance.latency_ms))
                )
            }
        }
        $latency[$terminal] = [pscustomobject][ordered]@{
            run_id = $RunId
            executable = $terminalPath
            executable_sha256 = $terminalHash
            workload_executable = $script:latencyWorkload
            workload_executable_sha256 = $script:latencyWorkloadHash
            product_version = $version
            helper_binaries = $helpers
            configuration_mode = if ($terminal -ceq 'wt') {
                'uncontrolled'
            } else {
                'benchmark-isolated'
            }
            configuration_evidence = if ($terminal -ceq 'wt') {
                $null
            } else {
                $script:configurationEvidence[$terminal]
            }
            schedule_algorithm = $schedules.latency.algorithm
            schedule_seed_sha256 = $schedules.latency.seed_sha256
            samples = 60
            requested_samples = 60
            misses = 0
            latency_ms_all = [double[]]@(
                1..60 | ForEach-Object {
                    [double]$performance.latency_ms
                }
            )
            latency_ms_median = [double]$performance.latency_ms
            latency_ms_p95 = [double]$performance.latency_ms
            observations = [object[]]$latencyObservations
        }

        $payloadSummaries = [ordered]@{}
        $throughputObservations = [Collections.Generic.List[object]]::new()
        $throughputVisits = Get-FixtureVisits `
            -Schedule $schedules.throughput -Terminal $terminal
        foreach ($payload in @('ascii', 'sgr', 'unicode')) {
            $contract = $KettlePerfPayloadContracts[$payload]
            $mbps = [double]$performance.throughput_mbps
            $seconds = ([double]$contract.bytes / 1MB) / $mbps
            $writeSeconds = $seconds - 0.001
            Assert-ReleaseScore ($writeSeconds -gt 0.0) (
                "fixture throughput write time is invalid for $terminal/$payload"
            )
            $payloadSummaries[$payload] = [pscustomobject][ordered]@{
                mb_per_s_median = $mbps
                bytes = [int]$contract.bytes
                sha256 = [string]$contract.sha256
                runs = 6
                timing_boundary = 'console-write-start-to-DSR-response'
                seconds_all = [double[]]@(1..6 | ForEach-Object {
                    $seconds
                })
                seconds_median = [Math]::Round($seconds, 3)
                write_seconds_all = [double[]]@(1..6 | ForEach-Object {
                    $writeSeconds
                })
                write_seconds_median = [Math]::Round($writeSeconds, 3)
                writer_acceptance_mb_per_s_median = [Math]::Round(
                    ([double]$contract.bytes / 1MB) / $writeSeconds,
                    2
                )
                drain_ms_all = [double[]]@(1..6 | ForEach-Object { 1.0 })
                drain_misses = 0
            }
            foreach ($visit in $throughputVisits) {
                $throughputObservations.Add(
                    (New-ThroughputObservation `
                        -Terminal $terminal -Payload $payload -Visit $visit `
                        -Value $mbps -Seconds $seconds `
                        -WriteSeconds $writeSeconds)
                )
            }
        }
        $workloadPid = 30000 + $terminalIndex
        $throughput = [pscustomobject][ordered]@{
            run_id = $RunId
            executable = $terminalPath
            executable_sha256 = $terminalHash
            product_version = $version
            output_encoding = 'utf-8'
            drain_probe_required = $true
            helper_binaries = $helpers
            configuration_mode = if ($terminal -ceq 'wt') {
                'uncontrolled'
            } else {
                'benchmark-isolated'
            }
            configuration_evidence = if ($terminal -ceq 'wt') {
                $null
            } else {
                $script:configurationEvidence[$terminal]
            }
            schedule_algorithm = $schedules.throughput.algorithm
            schedule_seed_sha256 = $schedules.throughput.seed_sha256
            workload_runner = $workloadRunner
            workload_pid = $workloadPid
            postflood_ws_scope = 'terminal-tree-excluding-workload'
            postflood_ws_excluded_pids = [int[]]@($workloadPid)
            payloads = $payloadSummaries
            postflood_ws_mb = [double]$performance.postflood_ws_mb
            observations = [object[]]$throughputObservations
        }
        Write-FixtureJson `
            -Path (Join-Path $Directory "throughput-$terminal.json") `
            -Value $throughput
    }
    Write-FixtureJson `
        -Path (Join-Path $Directory 'startup-idle.json') -Value $startup
    Write-FixtureJson `
        -Path (Join-Path $Directory 'latency.json') -Value $latency
}

function New-MenuEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RunId,
        [Parameter(Mandatory = $true)]
        [ValidateSet('fixed-comparator', 'native-display')]
        [string]$Variant,
        [Parameter(Mandatory = $true)]
        [int]$WindowWidth,
        [Parameter(Mandatory = $true)]
        [int]$WindowHeight
    )

    $observations = [Collections.Generic.List[object]]::new()
    for ($sample = 1; $sample -le 200; $sample++) {
        $observations.Add([pscustomobject][ordered]@{
            terminal = 'kettle'
            metric = 'menu_hover_ms'
            sample_id = $sample
            sequence = $sample
            block_id = 1 + [int][Math]::Floor(($sample - 1) / 20)
            status = 'ok'
            value = 10.0
            poll_count = 1
            baseline_capture_ms = 0.2
            poll_capture_ms = 0.2
        })
    }
    $kettleConfiguration = $script:configurationEvidence.kettle
    return [pscustomobject][ordered]@{
        schema_version = 2
        run_id = $RunId
        passed = $true
        variant = $Variant
        executable = $script:shell
        executable_sha256 = $script:terminalHash
        helper_binaries = [object[]]@(
            New-FixtureHelpers -Terminal 'kettle'
        )
        kettle_version = 'fixture-kettle-1.0'
        config = [string]$kettleConfiguration.path
        config_sha256 = [string]$kettleConfiguration.sha256
        target_screen_device = $script:targetScreen
        requested_samples = 200
        samples = 200
        misses = 0
        block_size = 20
        block_count = 10
        capture_scope = 'context-menu-roi'
        observation_limit = (
            'comparative-software-capture-not-input-to-photon'
        )
        window_pixels = [pscustomobject][ordered]@{
            width = $WindowWidth
            height = $WindowHeight
        }
        capture_region = [pscustomobject][ordered]@{
            x = 10
            y = 10
            width = 240
            height = 160
        }
        latency_ms_all = [double[]]@(1..200 | ForEach-Object { 10.0 })
        latency_ms_p95 = 10.0
        latency_ms_p99 = 10.0
        long_frame_ms = 100.0
        long_frames = 0
        gates = [pscustomobject][ordered]@{
            max_p95_ms = 33.0
            max_p99_ms = 50.0
            max_long_frames = 1
        }
        observations = [object[]]$observations
    }
}

function New-FixtureMonitorTransitionTopology {
    param(
        [string]$Timestamp = '2026-07-26T12:10:00.0000000-07:00'
    )

    $display = New-FixtureDisplaySnapshot
    $physicalByHardwareId = @{}
    foreach ($physical in $display.active_physical_monitors) {
        $physicalByHardwareId[[string]$physical.hardware_id] = $physical
    }
    $screens = [Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $display.desktop_screens.Count; $index++) {
        $screen = $display.desktop_screens[$index]
        $hardwareId = "FIXTURE$($index + 1)"
        $physical = $physicalByHardwareId[$hardwareId]
        $connection = [pscustomobject][ordered]@{
            identity_source = 'wmi-monitor-connection-v1'
            instance_name = [string]$physical.instance_name
            hardware_id = $hardwareId
            video_output_technology = if ($index -eq 0) {
                10
            } elseif ($index -eq 1) {
                0
            } else {
                5
            }
        }
        $screens.Add([pscustomobject][ordered]@{
            device_name = [string]$screen.device_name
            monitor_device_id = [string]$screen.monitor_device_id
            monitor_hardware_id = $hardwareId
            primary = [bool]$screen.primary
            edid_backed = $true
            edid_match_count = 1
            edid_monitor = $physical
            connection = $connection
            effective_dpi = $screen.effective_dpi
            scale_factor = [Math]::Round(
                [double]$screen.effective_dpi.x / 96.0,
                4
            )
            refresh_hz = [int]$screen.refresh_hz
            bounds = $screen.bounds
            working_area = $screen.working_area
            requested_client_fits = $true
        })
    }
    return [pscustomobject][ordered]@{
        identity_acquisition = $display.identity_acquisition
        identity_issues = [object[]]@()
        timestamp = $Timestamp
        requested_client = [pscustomobject][ordered]@{
            width = $script:fixedWidth
            height = $script:fixedHeight
            non_client_allowance = [pscustomobject][ordered]@{
                width = 64
                height = 96
            }
        }
        desktop_screens = [object[]]$screens.ToArray()
        active_physical_monitors = [object[]](
            $display.active_physical_monitors
        )
        active_connections = [object[]]@(
            for ($index = 0; $index -lt $screens.Count; $index++) {
                $screens[$index].connection
            }
        )
    }
}

function Get-FixtureMonitorTransitionEndpoint {
    param($Screen)

    return [pscustomobject][ordered]@{
        device_name = [string]$Screen.device_name
        monitor_device_id = [string]$Screen.monitor_device_id
        monitor_hardware_id = [string]$Screen.monitor_hardware_id
        edid_instance_name = [string]$Screen.edid_monitor.instance_name
        friendly_name = [string]$Screen.edid_monitor.friendly_name
        serial_number = [string]$Screen.edid_monitor.serial_number
        effective_dpi = $Screen.effective_dpi
        scale_factor = $Screen.scale_factor
        refresh_hz = $Screen.refresh_hz
        bounds = $Screen.bounds
        working_area = $Screen.working_area
        requested_client_fits = $true
    }
}

function Get-FixtureMonitorTransitionPolicy {
    param([object[]]$Screens)

    $candidates = [Collections.Generic.List[object]]::new()
    $selected = $null
    for ($firstIndex = 0; $firstIndex -lt $Screens.Count; $firstIndex++) {
        for (
            $secondIndex = $firstIndex + 1;
            $secondIndex -lt $Screens.Count;
            $secondIndex++
        ) {
            $first = $Screens[$firstIndex]
            $second = $Screens[$secondIndex]
            $dpiDelta = [Math]::Max(
                [Math]::Abs(
                    [int]$first.effective_dpi.x -
                    [int]$second.effective_dpi.x
                ),
                [Math]::Abs(
                    [int]$first.effective_dpi.y -
                    [int]$second.effective_dpi.y
                )
            )
            $refreshDelta = [Math]::Abs(
                [int]$first.refresh_hz - [int]$second.refresh_hz
            )
            $geometryDelta = 0
            foreach ($areaName in @('bounds', 'working_area')) {
                foreach ($field in @('width', 'height')) {
                    $geometryDelta = [Math]::Max(
                        $geometryDelta,
                        [Math]::Abs(
                            [int]$first.$areaName.$field -
                            [int]$second.$areaName.$field
                        )
                    )
                }
            }
            $meaningfulDimensions = 0
            foreach ($delta in @(
                $dpiDelta,
                $refreshDelta,
                $geometryDelta
            )) {
                if ($delta -gt 0) {
                    $meaningfulDimensions++
                }
            }
            $deviceNames = [string[]]@(
                [string]$first.device_name,
                [string]$second.device_name
            )
            $candidate = [pscustomobject][ordered]@{
                pair_key = $deviceNames -join '|'
                device_names = $deviceNames
                meaningful_dimension_count = $meaningfulDimensions
                dpi_delta = $dpiDelta
                refresh_hz_delta = $refreshDelta
                geometry_delta_pixels = $geometryDelta
            }
            $candidates.Add($candidate)
            $better = $null -eq $selected
            if (-not $better) {
                foreach ($field in @(
                    'meaningful_dimension_count',
                    'dpi_delta',
                    'refresh_hz_delta',
                    'geometry_delta_pixels'
                )) {
                    if ([int]$candidate.$field -ne [int]$selected.$field) {
                        $better = [int]$candidate.$field -gt
                            [int]$selected.$field
                        break
                    }
                }
                if (
                    [int]$candidate.meaningful_dimension_count -eq
                        [int]$selected.meaningful_dimension_count -and
                    [int]$candidate.dpi_delta -eq
                        [int]$selected.dpi_delta -and
                    [int]$candidate.refresh_hz_delta -eq
                        [int]$selected.refresh_hz_delta -and
                    [int]$candidate.geometry_delta_pixels -eq
                        [int]$selected.geometry_delta_pixels
                ) {
                    $better = [StringComparer]::OrdinalIgnoreCase.Compare(
                        [string]$candidate.pair_key,
                        [string]$selected.pair_key
                    ) -lt 0
                }
            }
            if ($better) {
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
            $Screens | ForEach-Object { [string]$_.device_name }
        )
        candidate_pair_count = $candidates.Count
        candidate_pairs = [object[]]$candidates.ToArray()
        selected_pair_key = [string]$selected.pair_key
        selected_device_names = [string[]]$selected.device_names
        selected_contrast = $selected
    }
}

function Write-MonitorTransitionEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory,
        [Parameter(Mandatory = $true)]
        [string]$RunId
    )

    $topologyStart = New-FixtureMonitorTransitionTopology
    $topologyEnd = New-FixtureMonitorTransitionTopology `
        -Timestamp '2026-07-26T12:20:00.0000000-07:00'
    $policy = Get-FixtureMonitorTransitionPolicy `
        ([object[]]$topologyStart.desktop_screens)
    $selectedTopologyScreens = [Collections.Generic.List[object]]::new()
    foreach ($deviceName in $policy.selected_device_names) {
        $selectedTopologyScreens.Add(@(
            $topologyStart.desktop_screens |
                Where-Object {
                    [string]$_.device_name -ceq [string]$deviceName
                }
        )[0])
    }
    $menuClosedValues = [double[]]@(
        50.0, 42.0, 58.0, 44.0, 56.0,
        46.0, 54.0, 48.0, 52.0, 40.0
    )
    $contextMenuOpenValues = [double[]]@(
        70.0, 62.0, 78.0, 64.0, 76.0,
        66.0, 74.0, 68.0, 72.0, 60.0
    )
    $recoveryValues = [double[]]@(
        $menuClosedValues + $contextMenuOpenValues
    )
    $observations = [Collections.Generic.List[object]]::new()
    $states = [string[]]@('menu_closed', 'context_menu_open')
    for ($stateIndex = 0; $stateIndex -lt $states.Count; $stateIndex++) {
        $state = $states[$stateIndex]
        for ($sample = 0; $sample -lt 10; $sample++) {
            $globalIndex = ($stateIndex * 10) + $sample
            $sourceIndex = $globalIndex % 2
            $targetIndex = if ($sourceIndex -eq 0) { 1 } else { 0 }
            $source = $selectedTopologyScreens[$sourceIndex]
            $target = $selectedTopologyScreens[$targetIndex]
            $sourceEndpoint = Get-FixtureMonitorTransitionEndpoint $source
            $targetEndpoint = Get-FixtureMonitorTransitionEndpoint $target
            $observations.Add([pscustomobject][ordered]@{
                started_utc = '2026-07-26T12:15:00.0000000-07:00'
                state = $state
                sample = $sample
                direction = (
                    [string]$source.device_name + '->' +
                    [string]$target.device_name
                )
                source = $sourceEndpoint
                target = $targetEndpoint
                status = 'ok'
                miss_reason = $null
                recovery_to_capturable_client_ms = $recoveryValues[
                    $globalIndex
                ]
                actual_target_device_name = [string]$target.device_name
                target_effective_dpi_observed = $target.effective_dpi
                target_refresh_hz_observed = [int]$target.refresh_hz
                capture = [pscustomobject][ordered]@{
                    width = $script:fixedWidth
                    height = $script:fixedHeight
                    bytes = $script:fixedWidth * $script:fixedHeight * 4
                }
                ui_geometry_surface = [pscustomobject][ordered]@{
                    width = $script:fixedWidth
                    height = $script:fixedHeight
                }
                context_menu = if ($state -ceq 'context_menu_open') {
                    [pscustomobject][ordered]@{
                        open = $true
                        rect = [pscustomobject][ordered]@{
                            x = 20
                            y = 20
                            width = 240
                            height = 320
                        }
                        rows = 8
                    }
                } else {
                    [pscustomobject][ordered]@{
                        open = $false
                        rect = $null
                        rows = 0
                    }
                }
                ui_geometry_checks = 3
            })
        }
    }
    $stateSummaries = [pscustomobject][ordered]@{
        menu_closed = [pscustomobject][ordered]@{
            requested_samples = 10
            samples = 10
            misses = 0
            recovery_to_capturable_client_ms_all = [double[]]@(
                $menuClosedValues | Sort-Object
            )
            recovery_to_capturable_client_ms_median = 49.0
            recovery_to_capturable_client_ms_p95 = 58.0
            recovery_to_capturable_client_ms_max = 58.0
        }
        context_menu_open = [pscustomobject][ordered]@{
            requested_samples = 10
            samples = 10
            misses = 0
            recovery_to_capturable_client_ms_all = [double[]]@(
                $contextMenuOpenValues | Sort-Object
            )
            recovery_to_capturable_client_ms_median = 69.0
            recovery_to_capturable_client_ms_p95 = 78.0
            recovery_to_capturable_client_ms_max = 78.0
        }
    }
    $config = $script:configurationEvidence.kettle
    $transition = [pscustomobject][ordered]@{
        schema_version = 2
        run_id = $RunId
        status = 'passed'
        release_evidence_valid = $true
        metric_name = 'recovery_to_capturable_client_ms'
        topology_stable = $true
        selected_screens = [object[]]@(
            Get-FixtureMonitorTransitionEndpoint `
                $selectedTopologyScreens[0]
            Get-FixtureMonitorTransitionEndpoint `
                $selectedTopologyScreens[1]
        )
        selection_policy = $policy
        binary = [pscustomobject][ordered]@{
            executable = $script:shell
            executable_sha256 = $script:terminalHash
            cli_executable = $script:shell
            cli_executable_sha256 = $script:terminalHash
            product_version = 'fixture-kettle-1.0'
            config = [string]$config.path
            config_mode = 'provided'
            config_sha256 = [string]$config.sha256
        }
        requested = [pscustomobject][ordered]@{
            samples_per_state = 10
            states = $states
            window_pixels = [pscustomobject][ordered]@{
                width = $script:fixedWidth
                height = $script:fixedHeight
            }
            recovery_timeout_ms = 5000
            geometry_stable_checks = 3
            poll_ms = 25
        }
        topology_start = $topologyStart
        topology_end = $topologyEnd
        observations = [object[]]$observations
        requested_samples = 20
        samples = 20
        misses = 0
        recovery_to_capturable_client_ms_all = [double[]]@(
            $recoveryValues | Sort-Object
        )
        recovery_to_capturable_client_ms_median = 59.0
        recovery_to_capturable_client_ms_p95 = 76.0
        recovery_to_capturable_client_ms_max = 78.0
        states = $stateSummaries
    }
    Write-FixtureJson `
        -Path (Join-Path $Directory 'monitor-transition.json') `
        -Value $transition
}

function Write-VtebenchEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory,
        [Parameter(Mandatory = $true)]
        [string]$RunId
    )

    $source = [pscustomobject][ordered]@{
        revision = $script:vtebenchRevision
        benchmark_tree = ('de' * 20)
        expected_benchmark_count = 2
        wsl_cache = '/home/fixture/.cache/kettle-perf/vtebench-source'
        wsl_build_root = '/home/fixture/.cache/kettle-perf/vtebench-build'
        wsl_binary = '/home/fixture/.cache/kettle-perf/vtebench-build/vtebench'
        wsl_binary_sha256 = ('ef' * 32)
        cargo_lock_sha256 = ('01' * 32)
        cargo_path = '/home/fixture/.cargo/bin/cargo'
        cargo_sha256 = ('02' * 32)
        cargo_version = 'cargo 1.88.0 (fixture)'
        rustup_path = '/home/fixture/.cargo/bin/rustup'
        rustup_sha256 = ('06' * 32)
        rustup_version = 'rustup 1.28.2 (fixture)'
        timeout_path = '/usr/bin/timeout'
        timeout_sha256 = ('03' * 32)
        timeout_version = 'timeout (GNU coreutils) 9.4'
        setsid_path = '/usr/bin/setsid'
        setsid_sha256 = ('04' * 32)
        setsid_version = 'setsid from util-linux 2.39.3'
        script_path = '/usr/bin/script'
        script_sha256 = ('05' * 32)
        script_version = 'script from util-linux 2.39.3'
        source_state_schema = 'kettle-vtebench-source-state-v1'
        source_state_sha256 = ''
        deadlines_seconds = [pscustomobject][ordered]@{
            setup = 1800
            generator = 30
            cargo_fetch = 600
            cargo_build = 1200
            preflight = 120
            source_validation = 30
            workload = 900
            cleanup = 30
        }
        wsl_launcher = New-FixtureWslLauncher
    }
    $source.source_state_sha256 = (
        Get-FixtureVtebenchSourceStateSignature $source
    )
    $terminalResults = [ordered]@{}
    foreach ($terminal in $script:terminals) {
        $datPath = Join-Path $Directory "vtebench-$terminal.dat"
        [IO.File]::WriteAllText(
            $datPath,
            "one two `n1 3 `n2 2 `n3 _ `n",
            [Text.UTF8Encoding]::new($false)
        )
        $terminalResults[$terminal] = [pscustomobject][ordered]@{
            run_id = $RunId
            executable = Get-FixtureTerminalPath $terminal
            executable_sha256 = Get-FixtureTerminalHash $terminal
            product_version = Get-FixtureTerminalVersion $terminal
            source_state_before_sha256 = $source.source_state_sha256
            source_state_after_sha256 = $source.source_state_sha256
            dat_path = $datPath
            dat_sha256 = (
                Get-FileHash -LiteralPath $datPath -Algorithm SHA256
            ).Hash
            benchmark_count = 2
            sample_rows = 3
            benchmarks = [pscustomobject][ordered]@{
                one = [pscustomobject][ordered]@{
                    samples_ms = [double[]]@(1.0, 2.0, 3.0)
                    sample_count = 3
                    median_ms = 2.0
                }
                two = [pscustomobject][ordered]@{
                    samples_ms = [double[]]@(3.0, 2.0)
                    sample_count = 2
                    median_ms = 2.5
                }
            }
        }
    }
    $vtebenchRunnerPath = (
        Resolve-Path -LiteralPath (
            Join-Path $PSScriptRoot 'vtebench-inside.ps1'
        )
    ).Path
    $summary = [pscustomobject][ordered]@{
        schema_version = 2
        run_id = $RunId
        transport_schema = 'kettle-vtebench-channel-v1'
        workload_runner = [pscustomobject][ordered]@{
            schema = 'kettle-vtebench-runner-v1'
            powershell = [pscustomobject][ordered]@{
                path = $script:shell
                sha256 = $script:terminalHash
                version = '7.5.0'
            }
            script = [pscustomobject][ordered]@{
                path = $vtebenchRunnerPath
                sha256 = (
                    Get-FileHash -LiteralPath $vtebenchRunnerPath `
                        -Algorithm SHA256
                ).Hash
            }
        }
        source = $source
        terminals = $terminalResults
    }
    Write-FixtureJson `
        -Path (Join-Path $Directory 'vtebench-summary.json') `
        -Value $summary
}

function New-TerminalManifestRecord {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Terminal,
        [Parameter(Mandatory = $true)]
        [ValidateSet('current', 'baseline')]
        [string]$Candidate
    )

    $configuration = $script:configurationEvidence[$Terminal]
    $isWindowsTerminal = $Terminal -ceq 'wt'
    $entry = Get-FixtureComparatorEntry $Terminal
    $record = [ordered]@{
        name = $Terminal
        available = $true
        launcher = Get-FixtureTerminalPath $Terminal
        executable = Get-FixtureTerminalPath $Terminal
        executable_sha256 = Get-FixtureTerminalHash $Terminal
        version = Get-FixtureTerminalVersion $Terminal
        command_workloads = $true
        command_confirmation = if ($Terminal -ceq 'tabby') {
            'tabby-run'
        } else {
            'synthetic-command-ready'
        }
        helper_binaries = [object[]]@(
            New-FixtureHelpers -Terminal $Terminal
        )
        source = if ($Terminal -ceq 'kettle') {
            $embeddedCommit = if ($Candidate -ceq 'current') {
                $script:repositoryCommit
            } else {
                $script:baselineCommit
            }
            [pscustomobject][ordered]@{
                candidate = $Candidate
                acquisition = if ($Candidate -ceq 'current') {
                    'repository'
                } else {
                    'pinned-external'
                }
                embedded_commit = $embeddedCommit
                embedded_commit_abbreviation = $embeddedCommit.Substring(0, 12)
                embedded_dirty = $false
                expected_commit = if ($Candidate -ceq 'baseline') {
                    $script:baselineCommit
                } else {
                    $null
                }
                expected_sha256 = if ($Candidate -ceq 'baseline') {
                    $script:terminalHash.ToLowerInvariant()
                } else {
                    $null
                }
                actual_sha256 = $script:terminalHash.ToLowerInvariant()
                commit_object_verified = $true
                commit_is_ancestor = $true
                external_executable = $Candidate -ceq 'baseline'
                skip_build = $Candidate -ceq 'baseline'
                build_performed = $Candidate -ceq 'current'
                release_build_performed = $Candidate -ceq 'current'
            }
        } else {
            New-KettlePerfComparatorTerminalSource -Entry $entry
        }
        configuration = [pscustomobject][ordered]@{
            mode = if ($isWindowsTerminal) {
                'advisory-user-config'
            } else {
                'benchmark-isolated'
            }
            claim_eligible = -not $isWindowsTerminal
            files = [object[]]@($configuration)
        }
    }
    if ($Terminal -cne 'kettle') {
        $record['executable_bytes'] = [long]$entry.executable.bytes
        $record['authenticode_status'] = (
            [string]$entry.executable.authenticode_status
        )
        $record['signer_cert_sha256'] = if (
            $null -eq $entry.executable.signer_cert_sha256
        ) {
            $null
        } else {
            [string]$entry.executable.signer_cert_sha256
        }
        $record['comparator_role'] = [string]$entry.role
    }
    if ($isWindowsTerminal) {
        $record['launch_mode'] = 'installed-appx-direct-host'
        $installLocation = [IO.Path]::GetDirectoryName(
            [string]$record.executable
        )
        $record['installed_package'] = [pscustomobject][ordered]@{
            schema = 'kettle-windows-terminal-appx-v1'
            name = 'Microsoft.WindowsTerminal'
            publisher_id = '8wekyb3d8bbwe'
            package_family_name = 'Microsoft.WindowsTerminal_8wekyb3d8bbwe'
            package_full_name = (
                'Microsoft.WindowsTerminal_' +
                [string]$entry.version +
                '_x64__8wekyb3d8bbwe'
            )
            version = [string]$entry.version
            architecture = 'X64'
            status = 'Ok'
            signature_kind = 'Store'
            is_framework = $false
            non_removable = $false
            install_location = $installLocation
        }
    }
    return [pscustomobject]$record
}

function Write-Manifest {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory,
        [Parameter(Mandatory = $true)]
        [string]$RunId,
        [Parameter(Mandatory = $true)]
        [ValidateSet('current', 'baseline')]
        [string]$Candidate
    )

    $schedules = New-FixtureSchedules
    $terminalRecords = [Collections.Generic.List[object]]::new()
    foreach ($terminal in $script:terminals) {
        $terminalRecords.Add(
            (New-TerminalManifestRecord `
                -Terminal $terminal -Candidate $Candidate)
        )
    }
    $manifest = [pscustomobject][ordered]@{
        schema_version = 4
        run_id = $RunId
        repository_commit = $script:repositoryCommit
        repository_dirty = $false
        kettle_config_sha256 = (
            [string]$script:configurationEvidence.kettle.sha256
        )
        harness_provenance = New-FixtureHarnessProvenance
        comparator_campaign = $script:campaignEvidence
        os = [pscustomobject][ordered]@{
            description = 'Windows 11 fixture'
            version = '10.0.26200'
            architecture = 'x64'
        }
        toolchain = [pscustomobject][ordered]@{
            orchestrator_powershell = [pscustomobject][ordered]@{
                path = $script:shell
                sha256 = $script:terminalHash
                edition = 'Core'
                version = '7.5.0'
            }
            throughput_powershell = [pscustomobject][ordered]@{
                path = $script:shell
                sha256 = $script:terminalHash
                edition = 'Core'
                version = '7.5.0'
            }
            latency_workload = [pscustomobject][ordered]@{
                path = $script:latencyWorkload
                sha256 = $script:latencyWorkloadHash
                version = '10.0.26200.1'
            }
            vtebench_wsl = New-FixtureWslLauncher
        }
        machine = [pscustomobject][ordered]@{
            manufacturer = 'Kettle Test'
            model = 'Release Score Fixture'
            total_memory_bytes = 34359738368
            processors = [object[]]@(
                [pscustomobject][ordered]@{
                    name = 'Fixture CPU'
                    logical_processors = 8
                }
            )
            video_controllers = [object[]]@(
                [pscustomobject][ordered]@{
                    name = 'Fixture GPU'
                    driver_version = '1.0.0'
                }
            )
            active_power_scheme = 'Fixture Balanced'
            display_topology = New-FixtureDisplayTopology
        }
        settings = [pscustomobject][ordered]@{
            mode = 'release'
            terminals = $script:terminals
            benchmark_seed = $script:benchmarkSeed
            comparator_campaign_id = $script:campaign.campaign_id
            kettle_candidate = $Candidate
            expected_kettle_commit = if ($Candidate -ceq 'baseline') {
                $script:baselineCommit
            } else {
                $null
            }
            expected_kettle_sha256 = if ($Candidate -ceq 'baseline') {
                $script:terminalHash.ToLowerInvariant()
            } else {
                $null
            }
            window_pixels = [pscustomobject][ordered]@{
                width = $script:fixedWidth
                height = $script:fixedHeight
            }
            native_window_pixels = [pscustomobject][ordered]@{
                width = $script:nativeWidth
                height = $script:nativeHeight
            }
            startup_runs = 12
            idle_samples = 6
            idle_seconds = 10
            latency_samples = 60
            latency_block_size = 10
            max_latency_censored = 3
            latency_timeout_ms = 800
            menu_hover_samples = 200
            menu_hover_block_size = 20
            throughput_iterations = 6
            minimum_throughput_iterations = 6
            native_display_enabled = $true
            unidentified_display_allowed = $false
            vtebench_enabled = $true
            vtebench_revision = $script:vtebenchRevision
            monitor_transition_enabled = $true
            monitor_transition_samples_per_state = 10
            probe_cooldown_seconds = 15
            terminal_order_offset = 3
            vtebench_terminal_order = [string[]]$script:terminals.Clone()
            schedules = $schedules
            kettle_build_skipped = $Candidate -ceq 'baseline'
        }
        terminals = [object[]]$terminalRecords
    }
    Write-FixtureJson `
        -Path (Join-Path $Directory 'benchmark-manifest.json') `
        -Value $manifest
}

function Write-ReleaseFixture {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory,
        [Parameter(Mandatory = $true)]
        [string]$RunId,
        [Parameter(Mandatory = $true)]
        [ValidateSet('current', 'baseline')]
        [string]$Candidate,
        [ValidateSet('current', 'regressed-baseline')]
        [string]$KettlePerformance = 'current'
    )

    New-Item -ItemType Directory -Path $Directory | Out-Null
    Set-FixtureConfigurationEvidence -Directory $Directory
    Write-StartupLatencyThroughputEvidence `
        -Directory $Directory -RunId $RunId `
        -KettlePerformance $KettlePerformance
    Write-Manifest `
        -Directory $Directory -RunId $RunId -Candidate $Candidate
    Write-FixtureJson `
        -Path (Join-Path $Directory 'menu-hover.json') `
        -Value (New-MenuEvidence `
            -RunId $RunId -Variant 'fixed-comparator' `
            -WindowWidth $script:fixedWidth `
            -WindowHeight $script:fixedHeight)
    Write-FixtureJson `
        -Path (Join-Path $Directory 'native-display-menu-hover.json') `
        -Value (New-MenuEvidence `
            -RunId $RunId -Variant 'native-display' `
            -WindowWidth $script:nativeWidth `
            -WindowHeight $script:nativeHeight)
    Write-MonitorTransitionEvidence -Directory $Directory -RunId $RunId
    Write-VtebenchEvidence -Directory $Directory -RunId $RunId
}

function Copy-ReleaseFixture {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Source,
        [Parameter(Mandatory = $true)]
        [string]$Destination
    )

    Assert-ReleaseScore (
        (Test-Path -LiteralPath $Source -PathType Container)
    ) "fixture source does not exist: $Source"
    Assert-ReleaseScore (
        -not (Test-Path -LiteralPath $Destination)
    ) "fixture destination already exists: $Destination"
    Copy-Item -LiteralPath $Source -Destination $Destination -Recurse
}

function Invoke-ReleaseScore {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CurrentDirectory,
        [string]$BaselineDirectory = '',
        [Parameter(Mandatory = $true)]
        [string]$Label,
        [string]$ScoreScript = $script:scoreScript,
        [string[]]$AdditionalArguments = @()
    )

    $scorePath = Join-Path $CurrentDirectory "$Label-score.json"
    $logPath = Join-Path $CurrentDirectory "$Label-score.log"
    $arguments = @(
        '-NoLogo',
        '-NoProfile',
        '-File',
        $ScoreScript,
        '-ResultsDir',
        $CurrentDirectory,
        '-Mode',
        'release',
        '-RequireLatency',
        '-RequireMenuHover',
        '-RequireVtebench',
        '-RequireMonitorTransition',
        '-OutJson',
        $scorePath
    )
    $arguments += $AdditionalArguments
    if ($BaselineDirectory) {
        $arguments += @('-BaselineResultsDir', $BaselineDirectory)
    }
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    & $script:shell @arguments *> $logPath
    $exitCode = $LASTEXITCODE
    $stopwatch.Stop()
    $score = if (Test-Path -LiteralPath $scorePath -PathType Leaf) {
        Read-FixtureJson -Path $scorePath
    } else {
        $null
    }
    $record = [pscustomobject][ordered]@{
        label = $Label
        exit_code = $exitCode
        elapsed_seconds = [Math]::Round($stopwatch.Elapsed.TotalSeconds, 3)
        score_path = $scorePath
        log_path = $logPath
        score = $score
    }
    $script:timings.Add($record)
    return $record
}

function Assert-ExpectedScoreFailure {
    param(
        [Parameter(Mandatory = $true)]
        $Invocation,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    Assert-ReleaseScore ($Invocation.exit_code -ne 0) (
        "$Message unexpectedly exited successfully"
    )
    Assert-ReleaseScore ($null -ne $Invocation.score) (
        "$Message did not publish a score contract"
    )
    Assert-ReleaseScore (-not [bool]$Invocation.score.passed) (
        "$Message published passed=true"
    )
}

try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $currentDirectory = Join-Path $testRoot 'current'
    $baselineDirectory = Join-Path $testRoot 'baseline'
    Write-ReleaseFixture `
        -Directory $currentDirectory `
        -RunId '11111111-1111-4111-8111-111111111111' `
        -Candidate current
    Write-ReleaseFixture `
        -Directory $baselineDirectory `
        -RunId '22222222-2222-4222-8222-222222222222' `
        -Candidate baseline

    $positive = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'positive-production-10000'
    if ($positive.exit_code -ne 0) {
        $manifestIssues = if ($positive.score) {
            @($positive.score.manifest_issues) -join '; '
        } else {
            'score contract missing'
        }
        $baselineIssues = if ($positive.score) {
            @($positive.score.baseline_issues) -join '; '
        } else {
            'score contract missing'
        }
        $scoreLog = if (
            Test-Path -LiteralPath $positive.log_path -PathType Leaf
        ) {
            [IO.File]::ReadAllText($positive.log_path).Trim()
        } else {
            'score log missing'
        }
        throw (
            'complete schema-4 release fixture did not pass; manifest: ' +
            "$manifestIssues; baseline: $baselineIssues; scorer output: " +
            $scoreLog
        )
    }
    $positiveScore = $positive.score
    Assert-ReleaseScore ([bool]$positiveScore.passed) (
        'positive score did not publish passed=true'
    )
    Assert-ReleaseScore (
        [bool]$positiveScore.coverage_passed -and
        @($positiveScore.manifest_issues).Count -eq 0
    ) 'positive score did not pass complete provenance and coverage'
    Assert-ReleaseScore (
        $positiveScore.scoring_mode -ceq 'release' -and
        $positiveScore.benchmark_mode -ceq 'release' -and
        $positiveScore.release_acquisition_contract -ceq
            'kettle-release-acquisition-contract-v2' -and
        $positiveScore.release_score_contract -ceq
            'kettle-release-score-contract-v1'
    ) 'positive score did not bind the immutable release contracts'
    Assert-ReleaseScore (
        [bool]$positiveScore.release_statistics_required -and
        [bool]$positiveScore.release_statistics_passed -and
        [int]$positiveScore.release_statistics.bootstrap_iterations -eq 10000
    ) 'production release statistics did not pass at 10,000 iterations'
    Assert-ReleaseScore (
        [bool]$positiveScore.release_statistics.primary_policy.passed -and
        [int](
            $positiveScore.release_statistics.primary_policy.confirmed_wins
        ) -eq 4 -and
        [bool]$positiveScore.release_statistics.throughput.passed
    ) 'positive release fixture did not confirm all isolated peers'
    Assert-ReleaseScore (
        @($positiveScore.release_statistics.advisory_terminals).Count -eq 1 -and
        [string]$positiveScore.release_statistics.advisory_terminals[0] -ceq
            'wt'
    ) 'Windows Terminal was not retained as advisory-only evidence'
    Assert-ReleaseScore (
        [bool]$positiveScore.baseline_statistics_required -and
        [bool]$positiveScore.baseline_statistics_passed -and
        [int]$positiveScore.baseline_statistics.bootstrap_iterations -eq 10000
    ) 'paired baseline non-inferiority did not pass at 10,000 iterations'
    Assert-ReleaseScore (
        [bool]$positiveScore.baseline_statistics.policy.passed -and
        [int](
            $positiveScore.baseline_statistics.policy.required_metric_count
        ) -eq 5
    ) 'baseline policy did not pass all five required raw metrics'
    Assert-ReleaseScore (
        [bool]$positiveScore.native_menu_hover_required -and
        [bool]$positiveScore.native_menu_hover_data_valid -and
        [bool]$positiveScore.native_menu_hover_passed -and
        @($positiveScore.native_menu_hover_issues).Count -eq 0
    ) 'native-display ROI menu evidence did not pass'
    Assert-ReleaseScore (
        [bool]$positiveScore.monitor_transition_passed
    ) 'monitor-transition evidence did not pass'
    Assert-ReleaseScore (
        [bool]$positiveScore.monitor_transition_performance_passed -and
        [double](
            $positiveScore.monitor_transition_performance_limits.p95_ms
        ) -eq 1000.0 -and
        [double](
            $positiveScore.monitor_transition_performance_limits.max_ms
        ) -eq 2000.0 -and
        [bool](
            $positiveScore.
                monitor_transition_baseline_non_inferiority_passed
        ) -and
        @(
            $positiveScore.monitor_transition_baseline_non_inferiority.
                comparisons
        ).Count -eq 6
    ) 'monitor-transition performance or baseline gate did not pass'
    $positiveTransition = $positiveScore.monitor_transition
    Assert-ReleaseScore (
        [double](
            $positiveTransition.observations[0].
                recovery_to_capturable_client_ms
        ) -eq 50.0 -and
        [double](
            $positiveTransition.observations[1].
                recovery_to_capturable_client_ms
        ) -eq 42.0 -and
        [double](
            $positiveTransition.
                recovery_to_capturable_client_ms_all[0]
        ) -eq 40.0 -and
        [double](
            $positiveTransition.
                recovery_to_capturable_client_ms_median
        ) -eq 59.0
    ) (
        'monitor-transition summaries were not derived from the unsorted ' +
        'raw sample sequence'
    )
    Assert-ReleaseScore (
        [string](
            $positiveScore.monitor_transition.selection_policy.algorithm
        ) -ceq 'maximum-meaningful-contrast-v1' -and
        [int](
            $positiveScore.monitor_transition.selection_policy.candidate_pair_count
        ) -eq 3 -and
        [string](
            $positiveScore.monitor_transition.selected_screens[0].device_name
        ) -ceq $script:targetScreen -and
        [string](
            $positiveScore.monitor_transition.selected_screens[1].device_name
        ) -ceq $script:thirdScreen
    ) (
        'three-display fixture did not select the deterministic ' +
        'maximum-contrast pair'
    )

    $positiveManifest = Read-FixtureJson -Path (
        Join-Path $currentDirectory 'benchmark-manifest.json'
    )
    $positiveBaselineManifest = Read-FixtureJson -Path (
        Join-Path $baselineDirectory 'benchmark-manifest.json'
    )
    $positiveDisplay = $positiveManifest.machine.display_topology
    Assert-ReleaseScore (
        $positiveDisplay.acquisition_schema -ceq
            'kettle-display-topology-acquisition-v2' -and
        $positiveDisplay.topology_stable -eq $true -and
        $positiveDisplay.release_evidence_valid -eq $true -and
        $positiveDisplay.start_signature_sha256 -ceq
            $positiveDisplay.end_signature_sha256 -and
        $positiveDisplay.start_signature_sha256 -ceq
            $positiveDisplay.acquisition_start.signature_sha256 -and
        $positiveDisplay.end_signature_sha256 -ceq
            $positiveDisplay.acquisition_end.signature_sha256
    ) 'stable start/end display acquisition evidence did not pass'
    $windowsTerminalRecord = @(
        $positiveManifest.terminals | Where-Object { $_.name -ceq 'wt' }
    )
    Assert-ReleaseScore (
        $windowsTerminalRecord.Count -eq 1 -and
        $windowsTerminalRecord[0].configuration.mode -ceq
            'advisory-user-config' -and
        $windowsTerminalRecord[0].configuration.claim_eligible -eq $false
    ) 'positive manifest did not mark Windows Terminal advisory-only'
    $currentKettleRecord = @(
        $positiveManifest.terminals |
            Where-Object { $_.name -ceq 'kettle' }
    )[0]
    $baselineKettleRecord = @(
        $positiveBaselineManifest.terminals |
            Where-Object { $_.name -ceq 'kettle' }
    )[0]
    Assert-ReleaseScore (
        $positiveManifest.schema_version -eq 4 -and
        $positiveManifest.settings.kettle_candidate -ceq 'current' -and
        $currentKettleRecord.source.acquisition -ceq 'repository' -and
        [bool]$currentKettleRecord.source.build_performed -and
        $positiveBaselineManifest.schema_version -eq 4 -and
        $positiveBaselineManifest.settings.kettle_candidate -ceq 'baseline' -and
        $baselineKettleRecord.source.acquisition -ceq 'pinned-external' -and
        -not [bool]$baselineKettleRecord.source.build_performed
    ) 'positive fixture did not preserve current/external-baseline acquisition roles'
    $currentPeerConfiguration = @(
        $positiveManifest.terminals |
            Where-Object { $_.name -ceq 'alacritty' }
    )[0].configuration.files[0]
    $baselinePeerConfiguration = @(
        $positiveBaselineManifest.terminals |
            Where-Object { $_.name -ceq 'alacritty' }
    )[0].configuration.files[0]
    Assert-ReleaseScore (
        $currentPeerConfiguration.path -cne
            $baselinePeerConfiguration.path -and
        [int64]$currentPeerConfiguration.bytes -eq
            [int64]$baselinePeerConfiguration.bytes -and
        [string]$currentPeerConfiguration.sha256 -ceq
            [string]$baselinePeerConfiguration.sha256
    ) (
        'positive baseline did not prove path-independent environment ' +
        'comparison with exact bytes/hash'
    )

    # Keep the positive end-to-end case on the untouched production defaults.
    # Negative cases exercise an exact temporary copy of the same scripts with
    # only the two score-statistics bootstrap defaults reduced from 10,000 to
    # the supported 1,000 minimum. This retains all serialized validation and
    # fail-closed wiring without paying for another positive-confidence run for
    # every provenance mutation.
    $negativeScorerDirectory = Join-Path $testRoot 'negative-scorer'
    New-Item -ItemType Directory -Path $negativeScorerDirectory | Out-Null
    foreach (
        $sourceScript in Get-ChildItem `
            -LiteralPath $PSScriptRoot -Filter '*.ps1'
    ) {
        Copy-Item -LiteralPath $sourceScript.FullName `
            -Destination (Join-Path `
                $negativeScorerDirectory $sourceScript.Name)
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'campaigns') `
        -Destination (Join-Path $negativeScorerDirectory 'campaigns') `
        -Recurse
    $negativeStatisticsPath = Join-Path `
        $negativeScorerDirectory 'score-statistics.ps1'
    $negativeStatisticsText = [IO.File]::ReadAllText(
        $negativeStatisticsPath
    )
    $bootstrapNeedle = '[int]$BootstrapIterations = 10000'
    $bootstrapReplacement = '[int]$BootstrapIterations = 1000'
    $bootstrapMatches = [regex]::Matches(
        $negativeStatisticsText,
        [regex]::Escape($bootstrapNeedle)
    ).Count
    Assert-ReleaseScore ($bootstrapMatches -eq 2) (
        'negative scorer expected exactly two 10,000-iteration defaults'
    )
    [IO.File]::WriteAllText(
        $negativeStatisticsPath,
        $negativeStatisticsText.Replace(
            $bootstrapNeedle,
            $bootstrapReplacement
        ),
        [Text.UTF8Encoding]::new($false)
    )
    $negativeScoreScript = Join-Path `
        $negativeScorerDirectory 'score.ps1'
    Assert-ReleaseScore (
        (Get-FileHash -LiteralPath $negativeScoreScript -Algorithm SHA256).Hash `
            -ceq (
                Get-FileHash -LiteralPath $script:scoreScript -Algorithm SHA256
            ).Hash
    ) 'negative score.ps1 copy differs from the production scorer'

    $policyOverride = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-release-policy-overrides' `
        -ScoreScript $negativeScoreScript `
        -AdditionalArguments @(
            '-MaxRegressionPct', '8.0',
            '-MaxKettleRank', '3',
            '-MinimumPeersBeaten', '2',
            '-MinimumMetricsPerTerminal', '8',
            '-MinimumThroughputPeersMeasured', '3',
            '-MaxKettleThroughputRank', '3',
            '-MinimumThroughputPeersBeaten', '2',
            '-MinimumStartupSamples', '11',
            '-MinimumThroughputRuns', '5',
            '-MinimumLatencyPeersBeaten', '2',
            '-MinimumLatencySamples', '59',
            '-MaxLatencyMissRate', '0.06',
            '-MinimumMonitorTransitionSamplesPerState', '9',
            '-MaxMonitorTransitionP95Ms', '1001',
            '-MaxMonitorTransitionMaxMs', '2001',
            '-MonitorTransitionBaselineAbsoluteMarginMs', '101',
            '-MonitorTransitionBaselineRelativeMarginPct', '26',
            '-MinimumMenuHoverSamples', '199',
            '-MaxMenuHoverP95Ms', '34',
            '-MaxMenuHoverP99Ms', '51',
            '-MenuHoverLongFrameMs', '101',
            '-MaxMenuHoverLongFrames', '2',
            '-AllowDirtyManifest'
        )
    Assert-ExpectedScoreFailure `
        -Invocation $policyOverride `
        -Message 'noncanonical release scorer policy'
    foreach ($policyName in @(
        'max_regression_pct',
        'max_kettle_rank',
        'minimum_peers_beaten',
        'minimum_metrics_per_terminal',
        'minimum_throughput_peers_measured',
        'max_kettle_throughput_rank',
        'minimum_throughput_peers_beaten',
        'minimum_startup_samples',
        'minimum_throughput_runs',
        'minimum_latency_peers_beaten',
        'minimum_latency_samples',
        'max_latency_miss_rate',
        'minimum_monitor_transition_samples_per_state',
        'max_monitor_transition_p95_ms',
        'max_monitor_transition_max_ms',
        'monitor_transition_baseline_absolute_margin_ms',
        'monitor_transition_baseline_relative_margin_pct',
        'minimum_menu_hover_samples',
        'max_menu_hover_p95_ms',
        'max_menu_hover_p99_ms',
        'menu_hover_long_frame_ms',
        'max_menu_hover_long_frames',
        'allow_dirty_manifest'
    )) {
        Assert-ReleaseScore (
            @($policyOverride.score.manifest_issues) -contains
                "release scoring policy differs: $policyName"
        ) "release scorer accepted policy override: $policyName"
    }

    $modeSchemaDirectory = Join-Path `
        $testRoot 'negative-trusted-mode-schema-downgrade'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $modeSchemaDirectory
    $modeSchemaPath = Join-Path `
        $modeSchemaDirectory 'benchmark-manifest.json'
    $modeSchemaManifest = Read-FixtureJson -Path $modeSchemaPath
    $modeSchemaManifest.schema_version = 2
    $modeSchemaManifest.settings.mode = 'smoke'
    Write-FixtureJson `
        -Path $modeSchemaPath -Value $modeSchemaManifest
    $modeSchemaTamper = Invoke-ReleaseScore `
        -CurrentDirectory $modeSchemaDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-trusted-mode-schema-downgrade' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $modeSchemaTamper `
        -Message 'release manifest mode/schema downgrade'
    Assert-ReleaseScore (
        @($modeSchemaTamper.score.manifest_issues) -contains
            'trusted release scoring requires benchmark manifest schema 4' -and
        @($modeSchemaTamper.score.manifest_issues) -contains
            'trusted scoring mode differs from benchmark manifest mode' -and
        [bool]$modeSchemaTamper.score.release_statistics_required -and
        $modeSchemaTamper.score.scoring_mode -ceq 'release'
    ) 'release manifest controlled its own scoring policy'

    $typedCurrentDirectory = Join-Path `
        $testRoot 'negative-release-json-types-current'
    $typedBaselineDirectory = Join-Path `
        $testRoot 'negative-release-json-types-baseline'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $typedCurrentDirectory
    Copy-ReleaseFixture `
        -Source $baselineDirectory -Destination $typedBaselineDirectory
    $typedCurrentPath = Join-Path `
        $typedCurrentDirectory 'benchmark-manifest.json'
    $typedCurrentManifest = Read-FixtureJson -Path $typedCurrentPath
    $typedCurrentManifest.schema_version = '4'
    $typedCurrentManifest.repository_dirty = 'false'
    $typedCurrentManifest.settings.startup_runs = '12'
    $typedCurrentManifest.settings.window_pixels.width = [double]1280.5
    $typedCurrentManifest.settings.native_display_enabled = 'true'
    $typedCurrentManifest.settings.unidentified_display_allowed = 'false'
    $typedCurrentManifest.settings.kettle_build_skipped = 'false'
    Write-FixtureJson `
        -Path $typedCurrentPath -Value $typedCurrentManifest
    $typedBaselinePath = Join-Path `
        $typedBaselineDirectory 'benchmark-manifest.json'
    $typedBaselineManifest = Read-FixtureJson -Path $typedBaselinePath
    $typedBaselineManifest.schema_version = '4'
    $typedBaselineManifest.repository_dirty = 'false'
    $typedBaselineManifest.settings.idle_samples = '6'
    $typedBaselineManifest.settings.vtebench_enabled = 'true'
    Write-FixtureJson `
        -Path $typedBaselinePath -Value $typedBaselineManifest
    $typedManifestTamper = Invoke-ReleaseScore `
        -CurrentDirectory $typedCurrentDirectory `
        -BaselineDirectory $typedBaselineDirectory `
        -Label 'negative-release-json-types' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $typedManifestTamper `
        -Message 'release manifest scalar type substitutions'
    foreach ($expectedIssue in @(
        'trusted release scoring requires benchmark manifest schema 4',
        'release benchmark setting differs: startup_runs',
        'release benchmark setting differs: native_display_enabled',
        'release benchmark setting differs: unidentified_display_allowed',
        'release benchmark window differs from the contract',
        'release scoring requires a clean repository manifest',
        'release scoring requires a Kettle build from this checkout'
    )) {
        Assert-ReleaseScore (
            @($typedManifestTamper.score.manifest_issues) -contains
                $expectedIssue
        ) "release scorer accepted typed current value: $expectedIssue"
    }
    foreach ($expectedIssue in @(
        'trusted release scoring requires baseline manifest schema 4',
        'baseline release benchmark setting differs: idle_samples',
        'baseline release benchmark setting differs: vtebench_enabled',
        'release baseline requires a clean repository manifest'
    )) {
        Assert-ReleaseScore (
            @($typedManifestTamper.score.baseline_issues) -contains
                $expectedIssue
        ) "release scorer accepted typed baseline value: $expectedIssue"
    }

    # Release evidence is a typed JSON contract. PowerShell's ordinary casts
    # accept "12" as 12 and $true as 1, so exercise aggregate and raw-record
    # fields independently to prove the trusted scorer rejects both coercions.
    $evidenceTypeCases = [object[]]@(
        [pscustomobject][ordered]@{
            label = 'aggregate-integer-string'
            file = 'startup-idle.json'
            mutate = {
                param($Evidence)
                $Evidence.kettle.startup_samples = '12'
            }
            expected_manifest_issue = $null
        },
        [pscustomobject][ordered]@{
            label = 'aggregate-integer-boolean'
            file = 'startup-idle.json'
            mutate = {
                param($Evidence)
                $Evidence.kettle.startup_misses = $false
            }
            expected_manifest_issue = $null
        },
        [pscustomobject][ordered]@{
            label = 'raw-integer-string'
            file = 'throughput-kettle.json'
            mutate = {
                param($Evidence)
                $Evidence.observations[0].cycle = '1'
            }
            expected_manifest_issue = (
                'throughput kettle observation cycle differs from schedule'
            )
        },
        [pscustomobject][ordered]@{
            label = 'raw-integer-boolean'
            file = 'throughput-kettle.json'
            mutate = {
                param($Evidence)
                $Evidence.observations[0].round = $true
            }
            expected_manifest_issue = (
                'throughput kettle observation round differs from schedule'
            )
        },
        [pscustomobject][ordered]@{
            label = 'monitor-topology-integer-string'
            file = 'monitor-transition.json'
            mutate = {
                param($Evidence)
                $dpi = $Evidence.topology_start.desktop_screens[0].effective_dpi
                $dpi.x = '192'
            }
            expected_manifest_issue = (
                'monitor-transition topology evidence is inconsistent'
            )
        },
        [pscustomobject][ordered]@{
            label = 'monitor-capture-bytes-boolean'
            file = 'monitor-transition.json'
            mutate = {
                param($Evidence)
                $Evidence.observations[0].capture.bytes = $true
            }
            expected_manifest_issue = (
                'monitor-transition capture or surface geometry is invalid'
            )
        }
    )
    foreach ($typeCase in $evidenceTypeCases) {
        $typedEvidenceDirectory = Join-Path (
            $testRoot
        ) "negative-release-evidence-type-$($typeCase.label)"
        Copy-ReleaseFixture `
            -Source $currentDirectory -Destination $typedEvidenceDirectory
        $typedEvidencePath = Join-Path `
            $typedEvidenceDirectory ([string]$typeCase.file)
        $typedEvidence = Read-FixtureJson -Path $typedEvidencePath
        $mutation = [scriptblock]$typeCase.mutate
        & $mutation $typedEvidence
        Write-FixtureJson `
            -Path $typedEvidencePath -Value $typedEvidence
        $typedEvidenceTamper = Invoke-ReleaseScore `
            -CurrentDirectory $typedEvidenceDirectory `
            -BaselineDirectory $baselineDirectory `
            -Label "negative-release-evidence-type-$($typeCase.label)" `
            -ScoreScript $negativeScoreScript
        Assert-ExpectedScoreFailure `
            -Invocation $typedEvidenceTamper `
            -Message "release evidence type substitution $($typeCase.label)"
        if ($null -eq $typeCase.expected_manifest_issue) {
            $kettleCoverageFailure = @(
                $typedEvidenceTamper.score.coverage_failures |
                    Where-Object { $_.terminal -ceq 'kettle' }
            )
            Assert-ReleaseScore (
                -not [bool]$typedEvidenceTamper.score.coverage_passed -and
                $kettleCoverageFailure.Count -eq 1 -and
                -not [bool]$kettleCoverageFailure[0].startup_samples_valid
            ) (
                'typed aggregate evidence failed outside exact startup ' +
                "coverage validation: $($typeCase.label)"
            )
        } else {
            Assert-ReleaseScore (
                @($typedEvidenceTamper.score.manifest_issues) -contains
                    [string]$typeCase.expected_manifest_issue
            ) (
                'typed raw evidence failed outside exact schedule ' +
                "validation: $($typeCase.label)"
            )
        }
    }

    # Keep schema 4 intact while substituting strings and 0/1 for JSON
    # booleans at independent evidence boundaries. Also rebuild an otherwise
    # internally consistent display snapshot around a 10.0 connector token so
    # the failure proves the release scorer rejects an integral JSON float,
    # rather than merely detecting a stale topology signature.
    $booleanTypeDirectory = Join-Path `
        $testRoot 'negative-release-boolean-and-output-types'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $booleanTypeDirectory
    $booleanTypeManifestPath = Join-Path `
        $booleanTypeDirectory 'benchmark-manifest.json'
    $booleanTypeManifest = Read-FixtureJson -Path $booleanTypeManifestPath
    $booleanTopology = $booleanTypeManifest.machine.display_topology
    $integralJsonFloat = (
        ConvertFrom-Json -InputObject '{"value":10.0}'
    ).value
    $booleanTopology.release_evidence_valid = 'true'
    $booleanTopology.topology_stable = 1
    $booleanTypeManifest.settings.vtebench_enabled = 1
    $booleanTypeManifest.settings.monitor_transition_enabled = 'true'
    $booleanTypeManifest.settings.unidentified_display_allowed = 0
    $booleanKettleRecord = @(
        $booleanTypeManifest.terminals |
            Where-Object { $_.name -ceq 'kettle' }
    )[0]
    $booleanKettleRecord.available = 'true'
    $booleanKettleRecord.configuration.claim_eligible = 1
    foreach ($snapshot in @(
        $booleanTopology.acquisition_start,
        $booleanTopology.acquisition_end
    )) {
        $snapshot.active_connections[0].video_output_technology =
            $integralJsonFloat
        $snapshot.desktop_screens[0].connection.video_output_technology =
            $integralJsonFloat
        $snapshot.signature_sha256 = (
            Get-FixtureDisplaySnapshotSignature -Snapshot $snapshot
        )
    }
    $booleanTopology.start_signature_sha256 = (
        $booleanTopology.acquisition_start.signature_sha256
    )
    $booleanTopology.end_signature_sha256 = (
        $booleanTopology.acquisition_end.signature_sha256
    )
    $booleanTopology.desktop_screens = [object[]](
        $booleanTopology.acquisition_start.desktop_screens
    )
    $booleanTopology.active_connections = [object[]](
        $booleanTopology.acquisition_start.active_connections
    )
    $booleanTopology.active_physical_monitors = [object[]](
        $booleanTopology.acquisition_start.active_physical_monitors
    )
    $booleanTopology.target_edid_monitors = [object[]](
        $booleanTopology.acquisition_start.target_edid_monitors
    )
    $booleanTopology.stability_monitoring.checkpoints[0].snapshot = (
        $booleanTopology.acquisition_start
    )
    $booleanTopology.stability_monitoring.checkpoints[1].snapshot = (
        $booleanTopology.acquisition_end
    )
    Write-FixtureJson `
        -Path $booleanTypeManifestPath -Value $booleanTypeManifest

    $booleanThroughputPath = Join-Path `
        $booleanTypeDirectory 'throughput-kettle.json'
    $booleanThroughput = Read-FixtureJson -Path $booleanThroughputPath
    $booleanThroughput.drain_probe_required = 'true'
    Write-FixtureJson `
        -Path $booleanThroughputPath -Value $booleanThroughput

    $booleanMenuPath = Join-Path $booleanTypeDirectory 'menu-hover.json'
    $booleanMenu = Read-FixtureJson -Path $booleanMenuPath
    $booleanMenu.passed = 1
    Write-FixtureJson -Path $booleanMenuPath -Value $booleanMenu

    $booleanTransitionPath = Join-Path `
        $booleanTypeDirectory 'monitor-transition.json'
    $booleanTransition = Read-FixtureJson -Path $booleanTransitionPath
    $booleanTransition.release_evidence_valid = 'true'
    $booleanTransition.topology_stable = 1
    Write-FixtureJson `
        -Path $booleanTransitionPath -Value $booleanTransition

    $booleanTypeTamper = Invoke-ReleaseScore `
        -CurrentDirectory $booleanTypeDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-release-boolean-and-output-types' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $booleanTypeTamper `
        -Message 'release boolean and output-technology type substitutions'
    foreach ($expectedIssue in @(
        'benchmark display topology is not valid release evidence',
        'benchmark display topology was not stable for the full run',
        'checkpoint start display identity connection evidence is invalid',
        'checkpoint end display identity connection evidence is invalid',
        'start display identity connection evidence is invalid',
        'end display identity connection evidence is invalid',
        'release benchmark setting differs: vtebench_enabled',
        'release benchmark setting differs: monitor_transition_enabled',
        'release benchmark setting differs: unidentified_display_allowed',
        'manifest marks kettle unavailable',
        'release configuration eligibility is invalid for kettle'
    )) {
        Assert-ReleaseScore (
            @($booleanTypeTamper.score.manifest_issues) -contains
                $expectedIssue
        ) "release scorer accepted a typed boolean/output value: $expectedIssue"
    }
    Assert-ReleaseScore (
        -not [bool]$booleanTypeTamper.score.throughput_passed -and
        -not [bool]$booleanTypeTamper.score.menu_hover_data_valid -and
        @($booleanTypeTamper.score.monitor_transition_issues) -contains
            'monitor-transition did not pass its evidence contract'
    ) 'release scorer accepted typed raw boolean evidence'

    $displayMonitorDirectory = Join-Path `
        $testRoot 'negative-display-continuous-monitor-contract'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $displayMonitorDirectory
    $displayMonitorPath = Join-Path `
        $displayMonitorDirectory 'benchmark-manifest.json'
    $displayMonitorManifest = Read-FixtureJson -Path $displayMonitorPath
    $displayMonitorManifest.machine.display_topology.stability_monitoring.
        monitoring_active_for_run = $false
    Write-FixtureJson `
        -Path $displayMonitorPath -Value $displayMonitorManifest
    $displayMonitorTamper = Invoke-ReleaseScore `
        -CurrentDirectory $displayMonitorDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-display-continuous-monitor-contract' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $displayMonitorTamper `
        -Message 'inactive continuous display monitor'
    Assert-ReleaseScore (
        @($displayMonitorTamper.score.manifest_issues) -contains
            'continuous display stability contract is invalid'
    ) 'inactive continuous display monitoring was accepted'

    $displayCheckpointDirectory = Join-Path `
        $testRoot 'negative-display-event-and-middle-checkpoint'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $displayCheckpointDirectory
    $displayCheckpointPath = Join-Path `
        $displayCheckpointDirectory 'benchmark-manifest.json'
    $displayCheckpointManifest = Read-FixtureJson -Path $displayCheckpointPath
    $displayCheckpointTopology = (
        $displayCheckpointManifest.machine.display_topology
    )
    $displayCheckpointStability = (
        $displayCheckpointTopology.stability_monitoring
    )
    $middleSnapshot = (
        $displayCheckpointTopology.acquisition_start |
            ConvertTo-Json -Depth 16 |
            ConvertFrom-Json
    )
    $middleSnapshot.captured_at = '2026-07-26T12:15:00.0000000-07:00'
    $middleSnapshot.desktop_screens[0].refresh_hz = 120
    $middleSnapshot.signature_sha256 = (
        Get-FixtureDisplaySnapshotSignature -Snapshot $middleSnapshot
    )
    $tamperedSignatureSnapshot = (
        $displayCheckpointTopology.acquisition_start |
            ConvertTo-Json -Depth 16 |
            ConvertFrom-Json
    )
    $tamperedSignatureSnapshot.signature_sha256 = ('dd' * 32)
    $displayCheckpointStability.display_change_events = [object[]]@(
        [pscustomobject][ordered]@{
            sequence = 1
            observed_at = '2026-07-26T12:15:00.0000000-07:00'
        }
    )
    $displayCheckpointStability.checkpoints = [object[]]@(
        $displayCheckpointStability.checkpoints[0],
        [pscustomobject][ordered]@{
            phase = 'throughput'
            snapshot = $middleSnapshot
        },
        [pscustomobject][ordered]@{
            phase = 'latency'
            snapshot = $tamperedSignatureSnapshot
        },
        $displayCheckpointStability.checkpoints[1]
    )
    Write-FixtureJson `
        -Path $displayCheckpointPath -Value $displayCheckpointManifest
    $displayCheckpointTamper = Invoke-ReleaseScore `
        -CurrentDirectory $displayCheckpointDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-display-event-and-middle-checkpoint' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $displayCheckpointTamper `
        -Message 'display event and changed intermediate topology'
    foreach ($expectedIssue in @(
        'display-change events occurred during benchmarking',
        'display stability checkpoint is invalid: throughput'
    )) {
        Assert-ReleaseScore (
            @($displayCheckpointTamper.score.manifest_issues) -contains
                $expectedIssue
        ) "continuous display evidence accepted: $expectedIssue"
    }

    $campaignPinDirectory = Join-Path `
        $testRoot 'negative-comparator-campaign-pin-current'
    $baselineCampaignPinDirectory = Join-Path `
        $testRoot 'negative-comparator-campaign-pin-baseline'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $campaignPinDirectory
    Copy-ReleaseFixture `
        -Source $baselineDirectory -Destination $baselineCampaignPinDirectory
    $campaignPinPath = Join-Path `
        $campaignPinDirectory 'benchmark-manifest.json'
    $campaignPinManifest = Read-FixtureJson -Path $campaignPinPath
    $campaignPinManifest.comparator_campaign.campaign_file.sha256 = ('ff' * 32)
    Write-FixtureJson -Path $campaignPinPath -Value $campaignPinManifest
    $baselineCampaignPinPath = Join-Path `
        $baselineCampaignPinDirectory 'benchmark-manifest.json'
    $baselineCampaignPinManifest = Read-FixtureJson `
        -Path $baselineCampaignPinPath
    $baselineCampaignPinManifest.comparator_campaign.campaign_id = (
        'kettle-windows-2026-07-26-tampered'
    )
    Write-FixtureJson `
        -Path $baselineCampaignPinPath -Value $baselineCampaignPinManifest
    $campaignPinTamper = Invoke-ReleaseScore `
        -CurrentDirectory $campaignPinDirectory `
        -BaselineDirectory $baselineCampaignPinDirectory `
        -Label 'negative-comparator-campaign-pins' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $campaignPinTamper `
        -Message 'current and baseline comparator campaign pin tampering'
    Assert-ReleaseScore (
        @($campaignPinTamper.score.manifest_issues) -contains
            'comparator campaign evidence differs from the release pin' -and
        @($campaignPinTamper.score.baseline_issues) -contains
            'baseline comparator campaign evidence differs from the release pin'
    ) 'tampered current or baseline comparator campaign evidence was accepted'

    $peerIdentityDirectory = Join-Path `
        $testRoot 'negative-comparator-peer-identities'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $peerIdentityDirectory
    $peerIdentityPath = Join-Path `
        $peerIdentityDirectory 'benchmark-manifest.json'
    $peerIdentityManifest = Read-FixtureJson -Path $peerIdentityPath
    $peerRecords = [ordered]@{}
    foreach ($record in $peerIdentityManifest.terminals) {
        $peerRecords[[string]$record.name] = $record
    }
    $peerRecords.alacritty.executable_sha256 = ('ee' * 32)
    $peerRecords.wezterm.executable_bytes++
    $peerRecords.rio.version = '0.0.0-tampered'
    $peerRecords.tabby.source.runtime_kind = 'unverified-path'
    $peerRecords.wt.comparator_role = 'isolated-confirmed'
    $peerRecords.wt.installed_package.package_full_name = (
        'Microsoft.WindowsTerminal_tampered_x64__8wekyb3d8bbwe'
    )
    Write-FixtureJson `
        -Path $peerIdentityPath -Value $peerIdentityManifest
    $peerIdentityTamper = Invoke-ReleaseScore `
        -CurrentDirectory $peerIdentityDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-comparator-peer-identities' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $peerIdentityTamper `
        -Message 'comparator executable and package identity tampering'
    foreach ($terminal in @('alacritty', 'wezterm', 'rio', 'tabby', 'wt')) {
        $expectedIssue = "$terminal identity differs from comparator campaign"
        Assert-ReleaseScore (
            @($peerIdentityTamper.score.manifest_issues) -contains
                $expectedIssue
        ) "tampered comparator identity was accepted: $terminal"
    }
    Assert-ReleaseScore (
        @($peerIdentityTamper.score.manifest_issues) -contains
            'Windows Terminal installed Appx identity is invalid'
    ) 'tampered Windows Terminal Appx identity was accepted'

    $windowsTerminalLauncherDirectory = Join-Path `
        $testRoot 'negative-windows-terminal-launcher'
    Copy-ReleaseFixture `
        -Source $currentDirectory `
        -Destination $windowsTerminalLauncherDirectory
    $windowsTerminalLauncherPath = Join-Path `
        $windowsTerminalLauncherDirectory 'benchmark-manifest.json'
    $windowsTerminalLauncherManifest = Read-FixtureJson `
        -Path $windowsTerminalLauncherPath
    $windowsTerminalLauncherRecord = @(
        $windowsTerminalLauncherManifest.terminals |
            Where-Object { $_.name -ceq 'wt' }
    )[0]
    $windowsTerminalLauncherRecord.launcher = 'C:\shadow\wt.exe'
    Write-FixtureJson `
        -Path $windowsTerminalLauncherPath `
        -Value $windowsTerminalLauncherManifest
    $windowsTerminalLauncherTamper = Invoke-ReleaseScore `
        -CurrentDirectory $windowsTerminalLauncherDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-windows-terminal-launcher' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $windowsTerminalLauncherTamper `
        -Message 'Windows Terminal PATH-shadow launcher'
    Assert-ReleaseScore (
        @($windowsTerminalLauncherTamper.score.manifest_issues) -contains
            'wt identity differs from comparator campaign' -and
        @($windowsTerminalLauncherTamper.score.manifest_issues) -contains
            'Windows Terminal release launcher is not the installed Appx host'
    ) 'a Windows Terminal PATH-shadow launcher was accepted'

    $monitorContractDirectory = Join-Path `
        $testRoot 'negative-monitor-transition-contract'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $monitorContractDirectory
    $monitorContractPath = Join-Path `
        $monitorContractDirectory 'monitor-transition.json'
    $monitorContract = Read-FixtureJson -Path $monitorContractPath
    $monitorContract.topology_end.desktop_screens[0].refresh_hz++
    $monitorContract.topology_start.desktop_screens[1].primary = $true
    $monitorContract.topology_end.desktop_screens[1].primary = $true
    $monitorContract.selection_policy.candidate_pairs[0].dpi_delta++
    $monitorContract.observations[0].sample = 1
    $monitorContract.observations[2].capture.width--
    $monitorContract.observations[3].target_effective_dpi_observed.x++
    $monitorContract.observations[4].target_refresh_hz_observed++
    $monitorContract.observations[5].ui_geometry_surface.width--
    $monitorContract.observations[10].context_menu.open = $false
    $monitorContract.observations[11].ui_geometry_checks = 2
    $monitorContract.states.menu_closed.
        recovery_to_capturable_client_ms_all[0] = 49.0
    $monitorContract.recovery_to_capturable_client_ms_all[0] = 49.0
    Write-FixtureJson `
        -Path $monitorContractPath -Value $monitorContract
    $monitorContractTamper = Invoke-ReleaseScore `
        -CurrentDirectory $monitorContractDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-monitor-transition-contract' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $monitorContractTamper `
        -Message 'tampered monitor-transition evidence'
    foreach ($expectedIssue in @(
        'monitor-transition topology evidence is inconsistent',
        (
            'monitor-transition desktop screens differ from the run display ' +
            'topology'
        ),
        (
            'monitor-transition selection policy or contrast evidence ' +
            'is invalid'
        ),
        (
            'monitor-transition state/sample coverage or direction binding ' +
            'is invalid'
        ),
        (
            'monitor-transition observed DPI or refresh differs from its ' +
            'target'
        ),
        (
            'monitor-transition capture or surface geometry is invalid'
        ),
        (
            'monitor-transition menu state is invalid'
        ),
        (
            'monitor-transition stable geometry check count is invalid'
        ),
        (
            'monitor-transition menu_closed summary differs from raw ' +
            'observations'
        ),
        (
            'monitor-transition combined summary differs from raw ' +
            'observations'
        )
    )) {
        Assert-ReleaseScore (
            @($monitorContractTamper.score.monitor_transition_issues) `
                -contains $expectedIssue
        ) "monitor-transition tamper was not detected: $expectedIssue"
    }

    $slowTransitionDirectory = Join-Path `
        $testRoot 'negative-monitor-transition-performance'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $slowTransitionDirectory
    $slowTransitionPath = Join-Path `
        $slowTransitionDirectory 'monitor-transition.json'
    $slowTransition = Read-FixtureJson -Path $slowTransitionPath
    Set-FixtureMonitorTransitionUniformRecovery `
        -Transition $slowTransition -Value 2500.0
    Write-FixtureJson `
        -Path $slowTransitionPath -Value $slowTransition
    $slowTransitionScore = Invoke-ReleaseScore `
        -CurrentDirectory $slowTransitionDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-monitor-transition-performance' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $slowTransitionScore `
        -Message 'slow but internally consistent monitor-transition evidence'
    Assert-ReleaseScore (
        -not [bool](
            $slowTransitionScore.score.
                monitor_transition_performance_passed
        ) -and
        @($slowTransitionScore.score.monitor_transition_issues) -contains
            (
                'monitor-transition combined p95 exceeds the configured ' +
                'limit'
            ) -and
        @($slowTransitionScore.score.monitor_transition_issues) -contains
            (
                'monitor-transition combined max exceeds the configured ' +
                'limit'
            )
    ) 'slow monitor-transition p95/max evidence was not rejected'

    $transitionRegressionDirectory = Join-Path `
        $testRoot 'negative-monitor-transition-baseline'
    Copy-ReleaseFixture `
        -Source $currentDirectory `
        -Destination $transitionRegressionDirectory
    $transitionRegressionPath = Join-Path `
        $transitionRegressionDirectory 'monitor-transition.json'
    $transitionRegression = Read-FixtureJson `
        -Path $transitionRegressionPath
    Set-FixtureMonitorTransitionUniformRecovery `
        -Transition $transitionRegression -Value 200.0
    Write-FixtureJson `
        -Path $transitionRegressionPath -Value $transitionRegression
    $transitionRegressionScore = Invoke-ReleaseScore `
        -CurrentDirectory $transitionRegressionDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-monitor-transition-baseline' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $transitionRegressionScore `
        -Message 'monitor-transition baseline regression'
    Assert-ReleaseScore (
        [bool]$transitionRegressionScore.score.monitor_transition_passed -and
        -not [bool](
            $transitionRegressionScore.score.
                monitor_transition_baseline_non_inferiority_passed
        ) -and
        @($transitionRegressionScore.score.baseline_issues) -contains
            (
                'monitor-transition p95/max baseline non-inferiority did ' +
                'not pass'
            ) -and
        @(
            $transitionRegressionScore.score.
                monitor_transition_baseline_non_inferiority.comparisons |
                Where-Object { -not [bool]$_.passed }
        ).Count -eq 6
    ) 'monitor-transition p95/max baseline regression was not rejected'

    $idlePidDirectory = Join-Path $testRoot 'negative-idle-pid-accounting'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $idlePidDirectory
    $idlePidPath = Join-Path $idlePidDirectory 'startup-idle.json'
    $idlePid = Read-FixtureJson -Path $idlePidPath
    $idlePid.kettle.idle_observations[0].included_processes_before = @(
        $idlePid.kettle.idle_observations[0].included_processes_before
    ) + @(
        $idlePid.kettle.idle_observations[0].included_processes_before[0]
    )
    $idlePid.kettle.idle_observations[1].included_processes_after = @(
        $idlePid.kettle.idle_observations[1].included_processes_after
    ) + @(
        $idlePid.kettle.idle_observations[1].included_processes_after[0]
    )
    $idlePid.kettle.idle_observations[2].excluded_pids = @(
        $idlePid.kettle.idle_observations[2].workload_pid,
        $idlePid.kettle.idle_observations[2].workload_pid
    )
    $includedPid = [int](
        $idlePid.kettle.idle_observations[3].
            included_processes_before[0].pid
    )
    $idlePid.kettle.idle_observations[3].excluded_pids += $includedPid
    $idlePid.kettle.idle_observations[4].
        included_processes_after[0].pid += 10000
    Write-FixtureJson -Path $idlePidPath -Value $idlePid
    $idlePidTamper = Invoke-ReleaseScore `
        -CurrentDirectory $idlePidDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-idle-pid-accounting' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $idlePidTamper `
        -Message 'tampered idle PID accounting'
    foreach ($expectedIssue in @(
        'idle kettle excluded PID evidence is invalid',
        (
            'idle kettle before process evidence contains invalid or ' +
            'duplicate PIDs'
        ),
        (
            'idle kettle after process evidence contains invalid or ' +
            'duplicate PIDs'
        ),
        'idle kettle before/after included PID sets differ',
        'idle kettle includes a PID declared excluded'
    )) {
        Assert-ReleaseScore (
            @($idlePidTamper.score.manifest_issues) -contains $expectedIssue
        ) "idle PID tamper was not detected: $expectedIssue"
    }

    $uncertaintyDirectory = Join-Path `
        $testRoot 'positive-authoritative-uncertainty-policy'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $uncertaintyDirectory
    $uncertaintyStartupPath = Join-Path `
        $uncertaintyDirectory 'startup-idle.json'
    $uncertaintyStartup = Read-FixtureJson -Path $uncertaintyStartupPath
    foreach ($terminal in @('alacritty', 'wezterm')) {
        $row = $uncertaintyStartup.PSObject.Properties[$terminal].Value
        foreach ($observation in $row.startup_observations) {
            $observation.value = 75.0
            $observation.window_discovered_ms = 15.0
            $observation.sized_focused_ms = 30.0
            $observation.go_published_ms = 45.0
            $observation.go_to_ready_ms = 30.0
        }
        $row.startup_ms_all = [double[]]@(1..12 | ForEach-Object {
            75.0
        })
        $row.startup_ms_median = 75.0
    }
    $uncertainPeer = $uncertaintyStartup.alacritty
    for (
        $index = 0;
        $index -lt $uncertainPeer.idle_observations.Count;
        $index++
    ) {
        $observation = $uncertainPeer.idle_observations[$index]
        $idleValue = if (($index % 2) -eq 0) { 0.299 } else { 0.301 }
        $cpuDelta = ($idleValue / 100.0) *
            [double]$observation.measured_seconds
        $observation.idle_cpu_pct = $idleValue
        $observation.fresh_ws_mb = 58.0
        $observation.cpu_seconds_delta = $cpuDelta
        $observation.included_processes_before[0].working_set_bytes = (
            [int64](58 * 1MB)
        )
        $observation.included_processes_after[0].working_set_bytes = (
            [int64](58 * 1MB)
        )
        $observation.included_processes_after[0].cpu_seconds = (
            [double]$observation.included_processes_before[0].cpu_seconds +
            $cpuDelta
        )
    }
    $uncertainPeer.idle_cpu_pct_all = [double[]]@(
        0.299,
        0.301,
        0.299,
        0.301,
        0.299,
        0.301
    )
    $uncertainPeer.idle_cpu_pct = 0.3
    $uncertainPeer.fresh_ws_mb_all = [double[]]@(
        1..6 | ForEach-Object { 58.0 }
    )
    $uncertainPeer.fresh_ws_mb = 58.0
    Write-FixtureJson `
        -Path $uncertaintyStartupPath -Value $uncertaintyStartup
    $uncertaintyLatencyPath = Join-Path `
        $uncertaintyDirectory 'latency.json'
    $uncertaintyLatency = Read-FixtureJson -Path $uncertaintyLatencyPath
    foreach ($observation in $uncertaintyLatency.alacritty.observations) {
        $observation.value = 13.0
    }
    $uncertaintyLatency.alacritty.latency_ms_all = [double[]]@(
        1..60 | ForEach-Object { 13.0 }
    )
    $uncertaintyLatency.alacritty.latency_ms_median = 13.0
    $uncertaintyLatency.alacritty.latency_ms_p95 = 13.0
    Write-FixtureJson `
        -Path $uncertaintyLatencyPath -Value $uncertaintyLatency
    $uncertaintyPolicy = Invoke-ReleaseScore `
        -CurrentDirectory $uncertaintyDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'positive-authoritative-uncertainty-policy' `
        -ScoreScript $negativeScoreScript
    Assert-ReleaseScore ($uncertaintyPolicy.exit_code -eq 0) (
        'authoritative uncertainty-policy fixture did not pass'
    )
    $uncertainPeerResult = @(
        $uncertaintyPolicy.score.release_statistics.
            peer_primary_classifications |
            Where-Object { $_.peer -ceq 'alacritty' }
    )
    $threeOfFourResult = @(
        $uncertaintyPolicy.score.release_statistics.
            peer_primary_classifications |
            Where-Object { $_.peer -ceq 'wezterm' }
    )
    Assert-ReleaseScore (
        $uncertainPeerResult.Count -eq 1 -and
        $uncertainPeerResult[0].classification -ceq 'uncertain' -and
        $threeOfFourResult.Count -eq 1 -and
        $threeOfFourResult[0].classification -ceq 'confirmed-win' -and
        [int]$threeOfFourResult[0].confirmed_metric_wins -eq 3 -and
        [int]$threeOfFourResult[0].uncertain_metrics -eq 1 -and
        [bool](
            $uncertaintyPolicy.score.release_statistics.primary_policy.passed
        ) -and
        [int](
            $uncertaintyPolicy.score.release_statistics.primary_policy.
                confirmed_wins
        ) -eq 3 -and
        [int](
            $uncertaintyPolicy.score.release_statistics.primary_policy.uncertain
        ) -eq 1 -and
        -not [bool](
            $uncertaintyPolicy.score.release_statistics.primary_policy.
                uncertainty_counts_as_win
        )
    ) (
        'uncertainty did not remain non-winning under the authoritative ' +
        '3-of-4 policies'
    )

    $omittedBaselineDirectory = Join-Path $testRoot 'negative-no-baseline'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $omittedBaselineDirectory
    $omittedBaseline = Invoke-ReleaseScore `
        -CurrentDirectory $omittedBaselineDirectory `
        -Label 'negative-no-baseline' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $omittedBaseline `
        -Message 'release score without a baseline'
    Assert-ReleaseScore (
        [bool]$omittedBaseline.score.baseline_statistics_required -and
        -not [bool]$omittedBaseline.score.baseline_statistics_passed -and
        @($omittedBaseline.score.baseline_issues) -contains
            'release baseline statistics require complete baseline evidence'
    ) 'omitted baseline failed outside the release baseline gate'

    $wtClaimDirectory = Join-Path $testRoot 'negative-wt-claim'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $wtClaimDirectory
    $wtClaimManifestPath = Join-Path `
        $wtClaimDirectory 'benchmark-manifest.json'
    $wtClaimManifest = Read-FixtureJson -Path $wtClaimManifestPath
    $wtClaimRecord = @(
        $wtClaimManifest.terminals | Where-Object { $_.name -ceq 'wt' }
    )
    $wtClaimRecord[0].configuration.claim_eligible = $true
    Write-FixtureJson -Path $wtClaimManifestPath -Value $wtClaimManifest
    $wtClaim = Invoke-ReleaseScore `
        -CurrentDirectory $wtClaimDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-wt-claim' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $wtClaim `
        -Message 'claim-eligible Windows Terminal'
    Assert-ReleaseScore (
        @($wtClaim.score.manifest_issues) -contains
            'release configuration eligibility is invalid for wt'
    ) 'claim-eligible Windows Terminal was not rejected by its exact contract'

    $missingNativeDirectory = Join-Path `
        $testRoot 'negative-missing-native'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $missingNativeDirectory
    Remove-Item -LiteralPath (
        Join-Path $missingNativeDirectory 'native-display-menu-hover.json'
    )
    $missingNative = Invoke-ReleaseScore `
        -CurrentDirectory $missingNativeDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-missing-native' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $missingNative `
        -Message 'release score without native-display menu evidence'
    Assert-ReleaseScore (
        -not [bool]$missingNative.score.native_menu_hover_data_valid -and
        -not [bool]$missingNative.score.native_menu_hover_passed -and
        @($missingNative.score.native_menu_hover_issues).Count -gt 0
    ) 'missing native-display file failed outside the native menu gate'

    $geometryDirectory = Join-Path `
        $testRoot 'negative-throughput-geometry'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $geometryDirectory
    $geometryPath = Join-Path $geometryDirectory 'throughput-kettle.json'
    $geometry = Read-FixtureJson -Path $geometryPath
    $geometry.observations[0].client_pixels.width = $script:fixedWidth - 1
    Write-FixtureJson -Path $geometryPath -Value $geometry
    $geometryTamper = Invoke-ReleaseScore `
        -CurrentDirectory $geometryDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-throughput-geometry' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $geometryTamper `
        -Message 'tampered throughput geometry'
    Assert-ReleaseScore (
        -not [bool]$geometryTamper.score.release_statistics_passed -and
        @($geometryTamper.score.manifest_issues | Where-Object {
            $_ -like '*release statistical evidence is invalid:*geometry*'
        }).Count -ge 1
    ) 'throughput geometry tamper failed outside raw release statistics'

    $goDirectory = Join-Path $testRoot 'negative-throughput-go'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $goDirectory
    $goPath = Join-Path $goDirectory 'throughput-kettle.json'
    $go = Read-FixtureJson -Path $goPath
    $go.observations[0].go_handshake = 'unlocked-fixture-tamper'
    Write-FixtureJson -Path $goPath -Value $go
    $goTamper = Invoke-ReleaseScore `
        -CurrentDirectory $goDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-throughput-go' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $goTamper `
        -Message 'tampered throughput GO handshake'
    Assert-ReleaseScore (
        -not [bool]$goTamper.score.release_statistics_passed -and
        @($goTamper.score.manifest_issues | Where-Object {
            $_ -like '*release statistical evidence is invalid:*GO handshake*'
        }).Count -ge 1
    ) 'throughput GO tamper failed outside raw release statistics'

    $candidateRoleDirectory = Join-Path `
        $testRoot 'negative-candidate-role'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $candidateRoleDirectory
    $candidateRolePath = Join-Path `
        $candidateRoleDirectory 'benchmark-manifest.json'
    $candidateRoleManifest = Read-FixtureJson -Path $candidateRolePath
    $candidateRoleManifest.settings.kettle_candidate = 'baseline'
    Write-FixtureJson `
        -Path $candidateRolePath -Value $candidateRoleManifest
    $candidateRole = Invoke-ReleaseScore `
        -CurrentDirectory $candidateRoleDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-candidate-role' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $candidateRole `
        -Message 'tampered current candidate role'
    Assert-ReleaseScore (
        @($candidateRole.score.manifest_issues) -contains
            'Kettle candidate role differs from current'
    ) 'current candidate role tamper failed outside acquisition provenance'

    $pinBaselineDirectory = Join-Path `
        $testRoot 'negative-baseline-pins'
    Copy-ReleaseFixture `
        -Source $baselineDirectory -Destination $pinBaselineDirectory
    $pinBaselinePath = Join-Path `
        $pinBaselineDirectory 'benchmark-manifest.json'
    $pinBaselineManifest = Read-FixtureJson -Path $pinBaselinePath
    $pinKettle = @(
        $pinBaselineManifest.terminals |
            Where-Object { $_.name -ceq 'kettle' }
    )[0]
    $pinBaselineManifest.settings.expected_kettle_commit = ('b' * 40)
    $pinBaselineManifest.settings.expected_kettle_sha256 = ('0' * 64)
    $pinKettle.source.expected_commit = ('b' * 40)
    $pinKettle.source.expected_sha256 = ('0' * 64)
    Write-FixtureJson `
        -Path $pinBaselinePath -Value $pinBaselineManifest
    $pinBaseline = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $pinBaselineDirectory `
        -Label 'negative-baseline-pins' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $pinBaseline `
        -Message 'tampered baseline expected commit/hash'
    Assert-ReleaseScore (
        @($pinBaseline.score.baseline_issues) -contains
            'baseline Kettle candidate lacks an exact external pin'
    ) 'baseline expected commit/hash tamper failed outside acquisition provenance'

    $harnessDirectory = Join-Path `
        $testRoot 'negative-harness-aggregate'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $harnessDirectory
    $harnessPath = Join-Path $harnessDirectory 'benchmark-manifest.json'
    $harnessManifest = Read-FixtureJson -Path $harnessPath
    $harnessManifest.harness_provenance.aggregate_sha256 = ('0' * 64)
    Write-FixtureJson -Path $harnessPath -Value $harnessManifest
    $harnessTamper = Invoke-ReleaseScore `
        -CurrentDirectory $harnessDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-harness-aggregate' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $harnessTamper `
        -Message 'tampered harness aggregate'
    Assert-ReleaseScore (
        @($harnessTamper.score.manifest_issues) -contains
            'harness provenance aggregate is invalid'
    ) 'harness aggregate tamper was not independently recomputed'

    $scheduleDirectory = Join-Path `
        $testRoot 'negative-schedule-seed'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $scheduleDirectory
    $scheduleManifestPath = Join-Path `
        $scheduleDirectory 'benchmark-manifest.json'
    $scheduleManifest = Read-FixtureJson -Path $scheduleManifestPath
    $scheduleManifest.settings.schedules.startup.seed_sha256 = ('0' * 64)
    Write-FixtureJson `
        -Path $scheduleManifestPath -Value $scheduleManifest
    $scheduleStartupPath = Join-Path `
        $scheduleDirectory 'startup-idle.json'
    $scheduleStartup = Read-FixtureJson -Path $scheduleStartupPath
    $scheduleStartup.kettle.startup_schedule_seed_sha256 = ('0' * 64)
    Write-FixtureJson `
        -Path $scheduleStartupPath -Value $scheduleStartup
    $scheduleTamper = Invoke-ReleaseScore `
        -CurrentDirectory $scheduleDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-schedule-seed' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $scheduleTamper `
        -Message 'tampered schedule and producer seed hashes'
    Assert-ReleaseScore (
        @($scheduleTamper.score.manifest_issues) -contains
            'startup schedule differs from benchmark seed and pinned settings' -and
        @($scheduleTamper.score.manifest_issues) -contains
            'startup kettle probe schedule metadata differs'
    ) 'schedule seed/hash tamper failed outside regenerated schedule evidence'

    $benchmarkSeedBaselineDirectory = Join-Path `
        $testRoot 'negative-benchmark-seed'
    Copy-ReleaseFixture `
        -Source $baselineDirectory `
        -Destination $benchmarkSeedBaselineDirectory
    $benchmarkSeedPath = Join-Path `
        $benchmarkSeedBaselineDirectory 'benchmark-manifest.json'
    $benchmarkSeedManifest = Read-FixtureJson -Path $benchmarkSeedPath
    $benchmarkSeedManifest.settings.benchmark_seed = 'tampered-benchmark-seed'
    Write-FixtureJson `
        -Path $benchmarkSeedPath -Value $benchmarkSeedManifest
    $benchmarkSeedTamper = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $benchmarkSeedBaselineDirectory `
        -Label 'negative-benchmark-seed' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $benchmarkSeedTamper `
        -Message 'tampered baseline benchmark seed'
    Assert-ReleaseScore (
        @($benchmarkSeedTamper.score.baseline_issues) -contains
            'baseline startup schedule differs from benchmark seed and pinned settings' -and
        @($benchmarkSeedTamper.score.baseline_issues) -contains
            'baseline environment differs: measurement_settings'
    ) 'benchmark seed tamper failed outside schedule/environment provenance'

    $vtebenchOrderDirectory = Join-Path `
        $testRoot 'negative-vtebench-order'
    Copy-ReleaseFixture `
        -Source $baselineDirectory -Destination $vtebenchOrderDirectory
    $vtebenchOrderPath = Join-Path `
        $vtebenchOrderDirectory 'benchmark-manifest.json'
    $vtebenchOrderManifest = Read-FixtureJson -Path $vtebenchOrderPath
    $firstVtebench = $vtebenchOrderManifest.settings.vtebench_terminal_order[0]
    $vtebenchOrderManifest.settings.vtebench_terminal_order[0] = (
        $vtebenchOrderManifest.settings.vtebench_terminal_order[1]
    )
    $vtebenchOrderManifest.settings.vtebench_terminal_order[1] = $firstVtebench
    Write-FixtureJson `
        -Path $vtebenchOrderPath -Value $vtebenchOrderManifest
    $vtebenchOrderTamper = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $vtebenchOrderDirectory `
        -Label 'negative-vtebench-order' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $vtebenchOrderTamper `
        -Message 'tampered baseline vtebench order'
    Assert-ReleaseScore (
        @($vtebenchOrderTamper.score.baseline_issues) -contains
            'baseline vtebench terminal order differs from its pinned rotation' -and
        @($vtebenchOrderTamper.score.baseline_issues) -contains
            'baseline environment differs: measurement_settings'
    ) 'vtebench order tamper failed outside exact environment provenance'

    $methodologyDirectory = Join-Path `
        $testRoot 'negative-release-methodology-profile'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $methodologyDirectory
    $methodologyPath = Join-Path `
        $methodologyDirectory 'benchmark-manifest.json'
    $methodologyManifest = Read-FixtureJson -Path $methodologyPath
    $methodologySettings = $methodologyManifest.settings
    $methodologySettings.terminals[0] = 'wt'
    $methodologySettings.terminals[1] = 'kettle'
    $methodologySettings.benchmark_seed = 'alternate-release-seed'
    $methodologySettings.startup_runs = 18
    $methodologySettings.idle_samples = 12
    $methodologySettings.idle_seconds = 11
    $methodologySettings.latency_samples = 120
    $methodologySettings.latency_block_size = 20
    $methodologySettings.max_latency_censored = 4
    $methodologySettings.latency_timeout_ms = 801
    $methodologySettings.menu_hover_samples = 201
    $methodologySettings.menu_hover_block_size = 19
    $methodologySettings.monitor_transition_samples_per_state = 11
    $methodologySettings.throughput_iterations = 12
    $methodologySettings.minimum_throughput_iterations = 12
    $methodologySettings.terminal_order_offset = 4
    $methodologySettings.probe_cooldown_seconds = 14
    $methodologySettings.window_pixels.width = 1281
    $methodologySettings.window_pixels.height = 801
    Write-FixtureJson `
        -Path $methodologyPath -Value $methodologyManifest
    $methodologyTamper = Invoke-ReleaseScore `
        -CurrentDirectory $methodologyDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-release-methodology-profile' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $methodologyTamper `
        -Message 'noncanonical release acquisition methodology'
    foreach ($settingName in @(
        'startup_runs',
        'idle_samples',
        'idle_seconds',
        'latency_samples',
        'latency_block_size',
        'max_latency_censored',
        'latency_timeout_ms',
        'menu_hover_samples',
        'menu_hover_block_size',
        'monitor_transition_samples_per_state',
        'throughput_iterations',
        'minimum_throughput_iterations',
        'terminal_order_offset',
        'probe_cooldown_seconds'
    )) {
        Assert-ReleaseScore (
            @($methodologyTamper.score.manifest_issues) -contains
                "release benchmark setting differs: $settingName"
        ) "release scorer accepted methodology override: $settingName"
    }
    foreach ($expectedIssue in @(
        'release terminal sequence differs from the contract',
        'release benchmark seed differs from the contract',
        'release benchmark window differs from the contract'
    )) {
        Assert-ReleaseScore (
            @($methodologyTamper.score.manifest_issues) -contains
                $expectedIssue
        ) "release scorer missed methodology issue: $expectedIssue"
    }

    $visitDirectory = Join-Path $testRoot 'negative-williams-visits'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $visitDirectory
    $visitStartupPath = Join-Path $visitDirectory 'startup-idle.json'
    $visitStartup = Read-FixtureJson -Path $visitStartupPath
    $visitStartup.kettle.startup_observations[0].position += 1
    $visitStartup.kettle.idle_observations[0].sample_key = 'tampered-key'
    Write-FixtureJson -Path $visitStartupPath -Value $visitStartup
    $visitLatencyPath = Join-Path $visitDirectory 'latency.json'
    $visitLatency = Read-FixtureJson -Path $visitLatencyPath
    $visitLatency.kettle.observations[0].cluster_id = 'tampered-cluster'
    Write-FixtureJson -Path $visitLatencyPath -Value $visitLatency
    $visitThroughputPath = Join-Path `
        $visitDirectory 'throughput-kettle.json'
    $visitThroughput = Read-FixtureJson -Path $visitThroughputPath
    $visitThroughput.observations[0].sequence += 1
    Write-FixtureJson `
        -Path $visitThroughputPath -Value $visitThroughput
    $visitTamper = Invoke-ReleaseScore `
        -CurrentDirectory $visitDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-williams-visits' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $visitTamper `
        -Message 'tampered raw Williams visit fields'
    foreach ($expectedIssue in @(
        'startup kettle observation position differs from schedule',
        'idle kettle observation sample_key differs from schedule',
        'latency kettle observation cluster_id differs from schedule',
        'throughput kettle observation sequence differs from schedule'
    )) {
        Assert-ReleaseScore (
            @($visitTamper.score.manifest_issues) -contains $expectedIssue
        ) "Williams visit tamper was not detected: $expectedIssue"
    }

    $configurationDirectory = Join-Path `
        $testRoot 'negative-probe-configuration'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $configurationDirectory
    $configurationPath = Join-Path `
        $configurationDirectory 'throughput-kettle.json'
    $configuration = Read-FixtureJson -Path $configurationPath
    $configuration.configuration_evidence.path += '.tampered'
    Write-FixtureJson -Path $configurationPath -Value $configuration
    $configurationTamper = Invoke-ReleaseScore `
        -CurrentDirectory $configurationDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-probe-configuration' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $configurationTamper `
        -Message 'tampered probe configuration activation'
    Assert-ReleaseScore (
        @($configurationTamper.score.manifest_issues) -contains
            'kettle throughput configuration activation differs from its manifest'
    ) 'probe configuration tamper failed outside activation provenance'

    $runnerDirectory = Join-Path $testRoot 'negative-runner'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $runnerDirectory
    $runnerPath = Join-Path $runnerDirectory 'throughput-kettle.json'
    $runner = Read-FixtureJson -Path $runnerPath
    $runner.workload_runner.script.sha256 = ('0' * 64)
    Write-FixtureJson -Path $runnerPath -Value $runner
    $runnerTamper = Invoke-ReleaseScore `
        -CurrentDirectory $runnerDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-runner' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $runnerTamper `
        -Message 'tampered throughput runner identity'
    Assert-ReleaseScore (
        @($runnerTamper.score.manifest_issues) -contains
            'kettle throughput runner identity is invalid'
    ) 'throughput runner tamper failed outside toolchain provenance'

    $vtebenchRunnerDirectory = Join-Path `
        $testRoot 'negative-vtebench-runner'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $vtebenchRunnerDirectory
    $vtebenchRunnerPath = Join-Path `
        $vtebenchRunnerDirectory 'vtebench-summary.json'
    $vtebenchRunner = Read-FixtureJson -Path $vtebenchRunnerPath
    $vtebenchRunner.workload_runner.script.sha256 = ('0' * 64)
    Write-FixtureJson `
        -Path $vtebenchRunnerPath -Value $vtebenchRunner
    $vtebenchRunnerTamper = Invoke-ReleaseScore `
        -CurrentDirectory $vtebenchRunnerDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-vtebench-runner' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $vtebenchRunnerTamper `
        -Message 'tampered vtebench runner identity'
    Assert-ReleaseScore (
        @($vtebenchRunnerTamper.score.manifest_issues) -contains
            'vtebench workload runner identity is invalid'
    ) 'vtebench runner tamper failed outside toolchain provenance'

    $vtebenchTransportDirectory = Join-Path `
        $testRoot 'negative-vtebench-transport'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $vtebenchTransportDirectory
    $vtebenchTransportPath = Join-Path `
        $vtebenchTransportDirectory 'vtebench-summary.json'
    $vtebenchTransport = Read-FixtureJson -Path $vtebenchTransportPath
    $vtebenchTransport.transport_schema = 'tampered-transport'
    Write-FixtureJson `
        -Path $vtebenchTransportPath -Value $vtebenchTransport
    $vtebenchTransportTamper = Invoke-ReleaseScore `
        -CurrentDirectory $vtebenchTransportDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-vtebench-transport' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $vtebenchTransportTamper `
        -Message 'tampered vtebench transport identity'
    Assert-ReleaseScore (
        @($vtebenchTransportTamper.score.manifest_issues) -contains
            'vtebench workload runner identity is invalid'
    ) 'vtebench transport tamper failed outside transport provenance'

    $vtebenchWslDirectory = Join-Path `
        $testRoot 'negative-vtebench-wsl-launcher'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $vtebenchWslDirectory
    $vtebenchWslPath = Join-Path `
        $vtebenchWslDirectory 'vtebench-summary.json'
    $vtebenchWsl = Read-FixtureJson -Path $vtebenchWslPath
    $vtebenchWsl.source.wsl_launcher.sha256 = ('0' * 64)
    Write-FixtureJson -Path $vtebenchWslPath -Value $vtebenchWsl
    $vtebenchWslTamper = Invoke-ReleaseScore `
        -CurrentDirectory $vtebenchWslDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-vtebench-wsl-launcher' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $vtebenchWslTamper `
        -Message 'tampered vtebench WSL launcher identity'
    Assert-ReleaseScore (
        @($vtebenchWslTamper.score.manifest_issues) -contains
            'vtebench WSL launcher identity is invalid'
    ) 'vtebench WSL launcher tamper was not bound to raw provenance'

    $vtebenchRevisionDirectory = Join-Path `
        $testRoot 'negative-vtebench-revision'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $vtebenchRevisionDirectory
    $vtebenchRevisionManifestPath = Join-Path `
        $vtebenchRevisionDirectory 'benchmark-manifest.json'
    $vtebenchRevisionSummaryPath = Join-Path `
        $vtebenchRevisionDirectory 'vtebench-summary.json'
    $vtebenchRevisionManifest = Read-FixtureJson `
        -Path $vtebenchRevisionManifestPath
    $vtebenchRevisionSummary = Read-FixtureJson `
        -Path $vtebenchRevisionSummaryPath
    $differentRevision = ('ab' * 20)
    $vtebenchRevisionManifest.settings.vtebench_revision = (
        $differentRevision
    )
    $vtebenchRevisionSummary.source.revision = $differentRevision
    $revisedStateSignature = (
        Get-FixtureVtebenchSourceStateSignature `
            $vtebenchRevisionSummary.source
    )
    $vtebenchRevisionSummary.source.source_state_sha256 = (
        $revisedStateSignature
    )
    foreach ($terminal in $script:terminals) {
        $terminalProperty = (
            $vtebenchRevisionSummary.terminals.PSObject.Properties[
                $terminal
            ]
        )
        $terminalResult = $terminalProperty.Value
        $terminalResult.source_state_before_sha256 = (
            $revisedStateSignature
        )
        $terminalResult.source_state_after_sha256 = (
            $revisedStateSignature
        )
    }
    Write-FixtureJson `
        -Path $vtebenchRevisionManifestPath `
        -Value $vtebenchRevisionManifest
    Write-FixtureJson `
        -Path $vtebenchRevisionSummaryPath `
        -Value $vtebenchRevisionSummary
    $vtebenchRevisionTamper = Invoke-ReleaseScore `
        -CurrentDirectory $vtebenchRevisionDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-vtebench-revision' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $vtebenchRevisionTamper `
        -Message 'internally consistent unpinned vtebench revision'
    Assert-ReleaseScore (
        @($vtebenchRevisionTamper.score.manifest_issues) -contains
            'release vtebench revision is not the documented pin'
    ) 'release scorer accepted an alternate vtebench revision'

    $vtebenchSourceDirectory = Join-Path `
        $testRoot 'negative-vtebench-source-state'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $vtebenchSourceDirectory
    $vtebenchSourcePath = Join-Path `
        $vtebenchSourceDirectory 'vtebench-summary.json'
    $vtebenchSource = Read-FixtureJson -Path $vtebenchSourcePath
    $vtebenchSource.terminals.kettle.source_state_after_sha256 = (
        '0' * 64
    )
    Write-FixtureJson `
        -Path $vtebenchSourcePath -Value $vtebenchSource
    $vtebenchSourceTamper = Invoke-ReleaseScore `
        -CurrentDirectory $vtebenchSourceDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-vtebench-source-state' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $vtebenchSourceTamper `
        -Message 'changed post-leg vtebench source state'
    Assert-ReleaseScore (
        @($vtebenchSourceTamper.score.manifest_issues) -contains
            'vtebench kettle source state changed during its leg'
    ) 'vtebench post-leg source mutation was not rejected'

    $displaySwitchDirectory = Join-Path `
        $testRoot 'negative-display-switch-during-run'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $displaySwitchDirectory
    $displaySwitchManifestPath = Join-Path `
        $displaySwitchDirectory 'benchmark-manifest.json'
    $displaySwitchManifest = Read-FixtureJson `
        -Path $displaySwitchManifestPath
    $displaySwitch = $displaySwitchManifest.machine.display_topology
    $displaySwitch.acquisition_end.desktop_screens[0].refresh_hz = 120
    $displaySwitch.acquisition_end.signature_sha256 = (
        Get-FixtureDisplaySnapshotSignature `
            -Snapshot $displaySwitch.acquisition_end
    )
    $displaySwitch.end_signature_sha256 = (
        $displaySwitch.acquisition_end.signature_sha256
    )
    Write-FixtureJson `
        -Path $displaySwitchManifestPath -Value $displaySwitchManifest
    $displaySwitchTamper = Invoke-ReleaseScore `
        -CurrentDirectory $displaySwitchDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-display-switch-during-run' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $displaySwitchTamper `
        -Message 'internally consistent mid-run monitor switch'
    Assert-ReleaseScore (
        @($displaySwitchTamper.score.manifest_issues) -contains
            'display topology acquisition signatures do not match'
    ) 'mid-run monitor switch was not rejected by acquisition provenance'

    $displayMethodDirectory = Join-Path `
        $testRoot 'negative-display-identity-method'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $displayMethodDirectory
    $displayMethodManifestPath = Join-Path `
        $displayMethodDirectory 'benchmark-manifest.json'
    $displayMethodManifest = Read-FixtureJson `
        -Path $displayMethodManifestPath
    $displayMethod = $displayMethodManifest.machine.display_topology
    foreach ($snapshot in @(
        $displayMethod.acquisition_start,
        $displayMethod.acquisition_end
    )) {
        $snapshot.identity_acquisition.method = 'untrusted-registry-scan-v1'
        $snapshot.signature_sha256 = (
            Get-FixtureDisplaySnapshotSignature -Snapshot $snapshot
        )
    }
    $displayMethod.start_signature_sha256 = (
        $displayMethod.acquisition_start.signature_sha256
    )
    $displayMethod.end_signature_sha256 = (
        $displayMethod.acquisition_end.signature_sha256
    )
    Write-FixtureJson `
        -Path $displayMethodManifestPath -Value $displayMethodManifest
    $displayMethodTamper = Invoke-ReleaseScore `
        -CurrentDirectory $displayMethodDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-display-identity-method' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $displayMethodTamper `
        -Message 'signed but unsupported display identity acquisition method'
    Assert-ReleaseScore (
        @($displayMethodTamper.score.manifest_issues) -contains
            'start display identity acquisition method is invalid'
    ) 'unsupported display identity method was not rejected semantically'

    $connectionTypeDirectory = Join-Path `
        $testRoot 'negative-nonphysical-display-connections'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $connectionTypeDirectory
    $connectionTypeManifestPath = Join-Path `
        $connectionTypeDirectory 'benchmark-manifest.json'
    $connectionTypeManifest = Read-FixtureJson `
        -Path $connectionTypeManifestPath
    $connectionTypeDisplay = (
        $connectionTypeManifest.machine.display_topology
    )
    foreach ($snapshot in @(
        $connectionTypeDisplay.acquisition_start,
        $connectionTypeDisplay.acquisition_end
    )) {
        Set-FixtureNonPhysicalConnectionEvidence -Topology $snapshot
        $snapshot.signature_sha256 = (
            Get-FixtureDisplaySnapshotSignature -Snapshot $snapshot
        )
    }
    Set-FixtureNonPhysicalConnectionEvidence `
        -Topology $connectionTypeDisplay
    $connectionTypeDisplay.start_signature_sha256 = (
        $connectionTypeDisplay.acquisition_start.signature_sha256
    )
    $connectionTypeDisplay.end_signature_sha256 = (
        $connectionTypeDisplay.acquisition_end.signature_sha256
    )
    Write-FixtureJson `
        -Path $connectionTypeManifestPath -Value $connectionTypeManifest

    $connectionTypeTransitionPath = Join-Path `
        $connectionTypeDirectory 'monitor-transition.json'
    $connectionTypeTransition = Read-FixtureJson `
        -Path $connectionTypeTransitionPath
    Set-FixtureNonPhysicalConnectionEvidence `
        -Topology $connectionTypeTransition.topology_start
    Set-FixtureNonPhysicalConnectionEvidence `
        -Topology $connectionTypeTransition.topology_end
    Write-FixtureJson `
        -Path $connectionTypeTransitionPath `
        -Value $connectionTypeTransition

    $connectionTypeTamper = Invoke-ReleaseScore `
        -CurrentDirectory $connectionTypeDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-nonphysical-display-connections' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $connectionTypeTamper `
        -Message 'signed Miracast and indirect display connections'
    Assert-ReleaseScore (
        @($connectionTypeTamper.score.manifest_issues) -contains
            'start display identity connection evidence is invalid' -and
        @($connectionTypeTamper.score.manifest_issues) -contains
            'end display identity connection evidence is invalid' -and
        @($connectionTypeTamper.score.monitor_transition_issues) -contains
            (
                'monitor-transition start display identity connection ' +
                'evidence is invalid'
            ) -and
        @($connectionTypeTamper.score.monitor_transition_issues) -contains
            (
                'monitor-transition end display identity connection ' +
                'evidence is invalid'
            )
    ) 'nonphysical display connections were not rejected semantically'

    $missingConnectionDirectory = Join-Path `
        $testRoot 'negative-missing-display-connection'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $missingConnectionDirectory
    $missingConnectionManifestPath = Join-Path `
        $missingConnectionDirectory 'benchmark-manifest.json'
    $missingConnectionManifest = Read-FixtureJson `
        -Path $missingConnectionManifestPath
    $missingConnectionDisplay = (
        $missingConnectionManifest.machine.display_topology
    )
    foreach ($snapshot in @(
        $missingConnectionDisplay.acquisition_start,
        $missingConnectionDisplay.acquisition_end
    )) {
        $snapshot.active_connections = [object[]]@(
            $snapshot.active_connections | Select-Object -Skip 1
        )
        $snapshot.desktop_screens[0].connection = $null
        $snapshot.signature_sha256 = (
            Get-FixtureDisplaySnapshotSignature -Snapshot $snapshot
        )
    }
    $missingConnectionDisplay.desktop_screens = [object[]](
        $missingConnectionDisplay.acquisition_start.desktop_screens
    )
    $missingConnectionDisplay.active_connections = [object[]](
        $missingConnectionDisplay.acquisition_start.active_connections
    )
    $missingConnectionDisplay.start_signature_sha256 = (
        $missingConnectionDisplay.acquisition_start.signature_sha256
    )
    $missingConnectionDisplay.end_signature_sha256 = (
        $missingConnectionDisplay.acquisition_end.signature_sha256
    )
    Write-FixtureJson `
        -Path $missingConnectionManifestPath `
        -Value $missingConnectionManifest

    $missingConnectionTransitionPath = Join-Path `
        $missingConnectionDirectory 'monitor-transition.json'
    $missingConnectionTransition = Read-FixtureJson `
        -Path $missingConnectionTransitionPath
    foreach ($topology in @(
        $missingConnectionTransition.topology_start,
        $missingConnectionTransition.topology_end
    )) {
        $topology.active_connections[0].instance_name = (
            $topology.active_connections[1].instance_name
        )
        $topology.desktop_screens[0].connection.instance_name = (
            $topology.active_connections[1].instance_name
        )
    }
    Write-FixtureJson `
        -Path $missingConnectionTransitionPath `
        -Value $missingConnectionTransition

    $missingConnectionTamper = Invoke-ReleaseScore `
        -CurrentDirectory $missingConnectionDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-missing-display-connection' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $missingConnectionTamper `
        -Message 'signed missing and mismatched display connections'
    Assert-ReleaseScore (
        @($missingConnectionTamper.score.manifest_issues) -contains
            'start display identity connection evidence is invalid' -and
        @($missingConnectionTamper.score.manifest_issues) -contains
            'end display identity connection evidence is invalid' -and
        @($missingConnectionTamper.score.monitor_transition_issues) -contains
            (
                'monitor-transition start display identity connection ' +
                'evidence is invalid'
            ) -and
        @($missingConnectionTamper.score.monitor_transition_issues) -contains
            (
                'monitor-transition end display identity connection ' +
                'evidence is invalid'
            )
    ) 'missing or mismatched display connections were not rejected'

    $latencyWorkloadDirectory = Join-Path `
        $testRoot 'negative-latency-workload'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $latencyWorkloadDirectory
    $latencyWorkloadPath = Join-Path `
        $latencyWorkloadDirectory 'latency.json'
    $latencyWorkload = Read-FixtureJson -Path $latencyWorkloadPath
    $latencyWorkload.kettle.workload_executable_sha256 = ('0' * 64)
    Write-FixtureJson `
        -Path $latencyWorkloadPath -Value $latencyWorkload
    $latencyWorkloadTamper = Invoke-ReleaseScore `
        -CurrentDirectory $latencyWorkloadDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-latency-workload' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $latencyWorkloadTamper `
        -Message 'mismatched latency workload executable'
    Assert-ReleaseScore (
        @($latencyWorkloadTamper.score.manifest_issues) -contains
            'kettle latency workload identity is invalid'
    ) 'latency workload executable provenance tamper was not rejected'

    $baselineWslDirectory = Join-Path `
        $testRoot 'negative-baseline-wsl-launcher'
    Copy-ReleaseFixture `
        -Source $baselineDirectory -Destination $baselineWslDirectory
    $baselineWslManifestPath = Join-Path `
        $baselineWslDirectory 'benchmark-manifest.json'
    $baselineWslSummaryPath = Join-Path `
        $baselineWslDirectory 'vtebench-summary.json'
    $baselineWslManifest = Read-FixtureJson -Path $baselineWslManifestPath
    $baselineWslSummary = Read-FixtureJson -Path $baselineWslSummaryPath
    $differentWslOutput = 'WSL version: 2.7.4.0'
    foreach ($identity in @(
        $baselineWslManifest.toolchain.vtebench_wsl,
        $baselineWslSummary.source.wsl_launcher
    )) {
        $identity.version = '2.7.4.0'
        $identity.file_version = '2.7.4.0'
        $identity.runtime_version = '2.7.4.0'
        $identity.version_output = $differentWslOutput
        $identity.version_output_sha256 = (
            Get-FixtureUtf8Sha256 $differentWslOutput
        )
    }
    Write-FixtureJson `
        -Path $baselineWslManifestPath -Value $baselineWslManifest
    Write-FixtureJson `
        -Path $baselineWslSummaryPath -Value $baselineWslSummary
    $baselineWslTamper = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $baselineWslDirectory `
        -Label 'negative-baseline-wsl-launcher' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $baselineWslTamper `
        -Message 'different baseline WSL launcher identity'
    Assert-ReleaseScore (
        @($baselineWslTamper.score.baseline_issues) -contains
            'baseline environment differs: toolchain'
    ) 'baseline comparison did not bind the WSL launcher toolchain'

    $crossLinkDirectory = Join-Path `
        $testRoot 'negative-raw-aggregate-link'
    Copy-ReleaseFixture `
        -Source $currentDirectory -Destination $crossLinkDirectory
    $crossLinkPath = Join-Path `
        $crossLinkDirectory 'throughput-kettle.json'
    $crossLink = Read-FixtureJson -Path $crossLinkPath
    $crossLink.observations[0].seconds += 0.25
    Write-FixtureJson -Path $crossLinkPath -Value $crossLink
    $crossLinkTamper = Invoke-ReleaseScore `
        -CurrentDirectory $crossLinkDirectory `
        -BaselineDirectory $baselineDirectory `
        -Label 'negative-raw-aggregate-link' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $crossLinkTamper `
        -Message 'tampered raw-to-aggregate throughput timing'
    Assert-ReleaseScore (
        @($crossLinkTamper.score.manifest_issues) -contains
            'kettle ascii throughput raw timings differ from aggregates'
    ) 'raw throughput timing tamper failed outside aggregate cross-link'

    $regressedBaselineDirectory = Join-Path `
        $testRoot 'negative-regressed-baseline'
    Write-ReleaseFixture `
        -Directory $regressedBaselineDirectory `
        -RunId '33333333-3333-4333-8333-333333333333' `
        -Candidate baseline `
        -KettlePerformance 'regressed-baseline'
    $regressedBaseline = Invoke-ReleaseScore `
        -CurrentDirectory $currentDirectory `
        -BaselineDirectory $regressedBaselineDirectory `
        -Label 'negative-regressed-baseline' `
        -ScoreScript $negativeScoreScript
    Assert-ExpectedScoreFailure `
        -Invocation $regressedBaseline `
        -Message 'statistically regressed baseline comparison'
    Assert-ReleaseScore (
        -not [bool]$regressedBaseline.score.baseline_statistics_passed -and
        -not [bool]$regressedBaseline.score.baseline_statistics.passed -and
        @(
            $regressedBaseline.score.baseline_statistics.metrics |
                Where-Object { -not [bool]$_.passed }
        ).Count -ge 1 -and
        @($regressedBaseline.score.baseline_issues) -contains
            'paired baseline non-inferiority statistics did not pass'
    ) 'regressed baseline failed outside paired non-inferiority'

    $totalSeconds = [Math]::Round(
        ($script:timings |
            Measure-Object -Property elapsed_seconds -Sum).Sum,
        3
    )
    foreach ($timing in $script:timings) {
        Write-Host (
            '  {0,-38} exit={1} seconds={2:N3}' -f
            $timing.label,
            $timing.exit_code,
            $timing.elapsed_seconds
        )
    }
    Write-Host (
        'release score self-test: PASS ({0}; score runtime {1:N3}s)' -f
        $PSVersionTable.PSVersion,
        $totalSeconds
    )
} finally {
    $resolvedTestRoot = [IO.Path]::GetFullPath($testRoot)
    $tempPrefix = $tempRoot.TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    ) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolvedTestRoot.StartsWith(
        $tempPrefix,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw (
            'refusing to clean release score self-test directory outside ' +
            "the OS temp root: $resolvedTestRoot"
        )
    }
    if (Test-Path -LiteralPath $resolvedTestRoot -PathType Container) {
        Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
    }
}
