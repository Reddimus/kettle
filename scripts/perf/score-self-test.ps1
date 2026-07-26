# Deterministic, GUI-free regression tests for score.ps1's positive and
# fail-closed coverage/latency paths. Intended for Windows CI and local edits to
# the benchmark schema.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$testDir = Join-Path $tempRoot ("kettle-perf-score-selftest-" + [guid]::NewGuid().ToString('N'))
$scoreScript = Join-Path $PSScriptRoot 'score.ps1'
$shell = (Get-Process -Id $PID).Path
. (Join-Path $PSScriptRoot 'terminal-specs.ps1')
. (Join-Path $PSScriptRoot 'lib-win32.ps1')
. (Join-Path $PSScriptRoot 'payload-contract.ps1')
. (Join-Path $PSScriptRoot 'vtebench-dat.ps1')
. (Join-Path $PSScriptRoot 'json-io.ps1')
Assert-KettlePerfTerminalSpecs

function Write-ScoreFixtureJson {
    param(
        [Parameter(Mandatory, ValueFromPipeline)]
        $InputObject,
        [Parameter(Mandatory)]
        [string]$Path,
        [ValidateRange(1, 100)]
        [int]$Depth = 8
    )

    process {
        Write-KettlePerfJsonFile `
            -Path $Path -InputObject $InputObject -Depth $Depth
    }
}

$defaultPathViolations = @()
foreach ($scriptFile in Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*.ps1') {
    $tokens = $null
    $parseErrors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile(
        $scriptFile.FullName,
        [ref]$tokens,
        [ref]$parseErrors
    )
    if ($parseErrors.Count -gt 0) {
        throw "could not parse $($scriptFile.Name) during path-default audit"
    }
    if ($null -eq $ast.ParamBlock) {
        continue
    }
    foreach ($parameter in $ast.ParamBlock.Parameters) {
        if (
            $null -ne $parameter.DefaultValue -and
            $parameter.DefaultValue.Extent.Text -match '\$PSScriptRoot'
        ) {
            $defaultPathViolations += (
                "$($scriptFile.Name):$($parameter.Extent.StartLineNumber)"
            )
        }
    }
}
if ($defaultPathViolations.Count -gt 0) {
    throw (
        '$PSScriptRoot is unavailable while Windows PowerShell 5.1 binds ' +
        'parameter defaults: ' + ($defaultPathViolations -join ', ')
    )
}
$encodedArguments = Join-KettlePerfArguments @(
    'plain', 'with space', 'C:\path with space\', 'quote"inside', ''
)
$expectedArguments = 'plain "with space" "C:\path with space\\" "quote\"inside" ""'
if ($encodedArguments -ne $expectedArguments) {
    throw "Windows argument encoding drifted: $encodedArguments"
}
$missingExplicitWasRejected = $false
try {
    [void](Resolve-KettlePerfTerminal -Name kettle -KettleExe (
        Join-Path $tempRoot ("missing-kettle-" + [guid]::NewGuid().ToString('N') + '.exe')
    ))
} catch {
    $missingExplicitWasRejected = $true
}
if (-not $missingExplicitWasRejected) {
    throw 'an invalid explicit executable did not fail closed'
}

$wrapperDirectory = Join-Path $testDir 'wrapper with spaces'
$wrapper = $null
try {
    $wrapper = New-KettlePerfCommandWrapper `
        -OutputDirectory $wrapperDirectory `
        -Command @($env:ComSpec, '/d', '/q', '/c', 'exit 23')
    $wrapperProcess = Start-Process -FilePath $env:ComSpec `
        -ArgumentList (Join-KettlePerfArguments @(
            '/d', '/q', '/v:off', '/s', '/c', 'call', $wrapper.Path
        )) -Wait -PassThru
    if ($wrapperProcess.ExitCode -ne 23) {
        throw "locked command wrapper returned $($wrapperProcess.ExitCode), expected 23"
    }
} finally {
    if ($null -ne $wrapper) {
        Close-KettlePerfCommandWrapper $wrapper
    }
}

function Get-ScoreFixtureUtf8Sha256 {
    param([Parameter(Mandatory)][string]$Text)

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

function Write-Fixture {
    param([string]$Directory)

    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
    $names = @('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')
    $runId = '01234567-89ab-4cde-8f01-23456789abcd'
    $executableHash = ('cd' * 32)
    $wslVersionOutput = 'WSL version: 2.7.3.0'
    $wslLauncher = [ordered]@{
        path = 'C:\Program Files\WSL\wsl.exe'
        sha256 = $executableHash
        version = '2.7.3.0'
        file_version = '2.7.3.0'
        runtime_version = '2.7.3.0'
        version_output = $wslVersionOutput
        version_output_sha256 = (
            Get-ScoreFixtureUtf8Sha256 $wslVersionOutput
        )
        resolution_policy = 'program-files-wsl-then-system32-v1'
        distribution = [ordered]@{
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
    $monitorPhysical = @(
        [ordered]@{
            identity_source = 'wmi-monitor-id-v1'
            instance_name = 'DISPLAY\FIXTURE1\1'
            hardware_id = 'FIXTURE1'
            friendly_name = 'Fixture Monitor One'
            serial_number = 'FIXTURE-1'
        },
        [ordered]@{
            identity_source = 'wmi-monitor-id-v1'
            instance_name = 'DISPLAY\FIXTURE2\2'
            hardware_id = 'FIXTURE2'
            friendly_name = 'Fixture Monitor Two'
            serial_number = 'FIXTURE-2'
        }
    )
    $monitorScreens = @(
        [ordered]@{
            device_name = '\\.\DISPLAY1'
            monitor_device_id = 'MONITOR\FIXTURE1\1'
            monitor_hardware_id = 'FIXTURE1'
            primary = $true
            edid_backed = $true
            edid_match_count = 1
            edid_monitor = $monitorPhysical[0]
            connection = [ordered]@{
                identity_source = 'wmi-monitor-connection-v1'
                instance_name = 'DISPLAY\FIXTURE1\1'
                hardware_id = 'FIXTURE1'
                video_output_technology = 10
            }
            effective_dpi = [ordered]@{ x = 192; y = 192 }
            scale_factor = 2.0
            refresh_hz = 60
            bounds = [ordered]@{
                x = 0
                y = 0
                width = 1920
                height = 1080
            }
            working_area = [ordered]@{
                x = 0
                y = 0
                width = 1920
                height = 1040
            }
            requested_client_fits = $true
        },
        [ordered]@{
            device_name = '\\.\DISPLAY2'
            monitor_device_id = 'MONITOR\FIXTURE2\2'
            monitor_hardware_id = 'FIXTURE2'
            primary = $false
            edid_backed = $true
            edid_match_count = 1
            edid_monitor = $monitorPhysical[1]
            connection = [ordered]@{
                identity_source = 'wmi-monitor-connection-v1'
                instance_name = 'DISPLAY\FIXTURE2\2'
                hardware_id = 'FIXTURE2'
                video_output_technology = 0
            }
            effective_dpi = [ordered]@{ x = 144; y = 144 }
            scale_factor = 1.5
            refresh_hz = 144
            bounds = [ordered]@{
                x = 1920
                y = 0
                width = 2560
                height = 1440
            }
            working_area = [ordered]@{
                x = 1920
                y = 0
                width = 2560
                height = 1400
            }
            requested_client_fits = $true
        }
    )
    $monitorTopologyStart = [ordered]@{
        identity_acquisition = [ordered]@{
            schema = 'kettle-display-identity-acquisition-v1'
            resolver = 'wmi-monitor-id-with-ccd-registry-fallback-v1'
            method = 'wmi-monitor-id-v1'
            ccd_status = 'unavailable'
            desktop_screen_count = 2
            wmi_active_monitor_count = 2
            wmi_active_connection_count = 2
            ccd_active_path_count = 0
            resolved_screen_count = 2
        }
        identity_issues = @()
        timestamp = '2026-07-26T12:10:00.0000000-07:00'
        requested_client = [ordered]@{
            width = 1280
            height = 800
            non_client_allowance = [ordered]@{
                width = 64
                height = 96
            }
        }
        desktop_screens = $monitorScreens
        active_physical_monitors = $monitorPhysical
        active_connections = @(
            $monitorScreens[0].connection,
            $monitorScreens[1].connection
        )
    }
    $monitorTopologyEnd = (
        $monitorTopologyStart | ConvertTo-Json -Depth 16 |
            ConvertFrom-Json
    )
    $monitorTopologyEnd.timestamp = '2026-07-26T12:20:00.0000000-07:00'
    $monitorEndpoints = @(
        foreach ($screen in $monitorScreens) {
            [ordered]@{
                device_name = $screen.device_name
                monitor_device_id = $screen.monitor_device_id
                monitor_hardware_id = $screen.monitor_hardware_id
                edid_instance_name = $screen.edid_monitor.instance_name
                friendly_name = $screen.edid_monitor.friendly_name
                serial_number = $screen.edid_monitor.serial_number
                effective_dpi = $screen.effective_dpi
                scale_factor = $screen.scale_factor
                refresh_hz = $screen.refresh_hz
                bounds = $screen.bounds
                working_area = $screen.working_area
                requested_client_fits = $true
            }
        }
    )
    $monitorContrast = [ordered]@{
        pair_key = '\\.\DISPLAY1|\\.\DISPLAY2'
        device_names = @('\\.\DISPLAY1', '\\.\DISPLAY2')
        meaningful_dimension_count = 3
        dpi_delta = 48
        refresh_hz_delta = 84
        geometry_delta_pixels = 640
    }
    $monitorPolicy = [ordered]@{
        algorithm = 'maximum-meaningful-contrast-v1'
        eligible_screen_order = 'device-name-ordinal-ignore-case'
        ranking = @(
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
        eligible_screen_device_names = @(
            '\\.\DISPLAY1',
            '\\.\DISPLAY2'
        )
        candidate_pair_count = 1
        candidate_pairs = @($monitorContrast)
        selected_pair_key = $monitorContrast.pair_key
        selected_device_names = $monitorContrast.device_names
        selected_contrast = $monitorContrast
    }
    $startup = [ordered]@{}
    $latency = [ordered]@{}
    $manifestTerminals = @()
    $vtebenchTerminals = [ordered]@{}
    for ($i = 0; $i -lt $names.Count; $i++) {
        $name = $names[$i]
        $version = "test-$name-1.0"
        $helperValues = if ($name -eq 'kettle') {
            @(
                [ordered]@{
                    role = 'kettle-cli'
                    path = $shell
                    sha256 = $executableHash
                }
            )
        } elseif ($name -eq 'tabby') {
            @(
                [ordered]@{
                    role = 'command-shell'
                    path = $shell
                    sha256 = $executableHash
                },
                [ordered]@{
                    role = 'command-launcher'
                    path = $shell
                    sha256 = $executableHash
                }
            )
        } else {
            @()
        }
        $helpers = [object[]]@($helperValues)
        $startupMedian = 100 + ($i * 50)
        $startup[$name] = [ordered]@{
            run_id = $runId
            executable = $shell
            executable_sha256 = $executableHash
            product_version = $version
            startup_ms_all = @($startupMedian) * 5
            startup_samples = 5
            startup_requested_samples = 5
            startup_misses = 0
            startup_ms_median = $startupMedian
            fresh_ws_mb = 100 + ($i * 10)
            idle_cpu_pct = if ($name -eq 'kettle') { 0.0 } else { 0.1 + $i }
        }
        $latencyMedian = 5 + $i
        $latencyP95 = 8 + $i
        $latencySamples = @(@($latencyMedian) * 18) + @($latencyP95, $latencyP95)
        $latency[$name] = [ordered]@{
            run_id = $runId
            executable = $shell
            executable_sha256 = $executableHash
            product_version = $version
            samples = 20
            requested_samples = 20
            misses = 0
            helper_binaries = [object[]]@($helpers)
            latency_ms_all = $latencySamples
            latency_ms_median = $latencyMedian
            latency_ms_p95 = $latencyP95
        }
        $manifestTerminals += [ordered]@{
            name = $name
            available = $true
            executable = $shell
            executable_sha256 = $executableHash
            version = $version
            command_workloads = $true
            command_confirmation = if ($name -eq 'tabby') {
                'tabby-run'
            } else {
                $null
            }
            helper_binaries = [object[]]@($helpers)
            source = if ($name -eq 'kettle') {
                [ordered]@{
                    embedded_commit = (
                        '0123456789abcdef0123456789abcdef01234567'
                    )
                    embedded_dirty = $false
                    release_build_performed = $true
                }
            } else {
                $null
            }
            configuration = if ($name -eq 'kettle') {
                [ordered]@{
                    mode = 'benchmark-isolated'
                    files = @([ordered]@{
                        path = 'C:\kettle-perf-fixture.config'
                        bytes = 1
                        sha256 = (('ab' * 32) -join '')
                    })
                }
            } else {
                [ordered]@{
                    mode = 'built-in-default'
                    files = @()
                }
            }
        }
    }
    $startup | Write-ScoreFixtureJson `
        -Path (Join-Path $Directory 'startup-idle.json') -Depth 5
    $latency | Write-ScoreFixtureJson `
        -Path (Join-Path $Directory 'latency.json') -Depth 5
    for ($i = 0; $i -lt $names.Count; $i++) {
        $name = $names[$i]
        $helpers = [object[]]@($manifestTerminals[$i].helper_binaries)
        $throughput = 20 - $i
        $asciiSeconds = ($KettlePerfPayloadContracts.ascii.bytes / 1MB) /
            $throughput
        $sgrSeconds = ($KettlePerfPayloadContracts.sgr.bytes / 1MB) /
            ($throughput - 1)
        $unicodeSeconds = ($KettlePerfPayloadContracts.unicode.bytes / 1MB) /
            ($throughput - 2)
        [ordered]@{
            run_id = $runId
            executable = $shell
            executable_sha256 = $executableHash
            product_version = "test-$name-1.0"
            output_encoding = 'utf-8'
            drain_probe_required = $true
            helper_binaries = [object[]]@($helpers)
            workload_pid = 4242
            postflood_ws_scope = 'terminal-tree-excluding-workload'
            postflood_ws_excluded_pids = @(4242)
            payloads = [ordered]@{
                ascii = @{
                    mb_per_s_median = $throughput
                    bytes = $KettlePerfPayloadContracts.ascii.bytes
                    sha256 = $KettlePerfPayloadContracts.ascii.sha256
                    runs = 3
                    timing_boundary = (
                        'console-write-start-to-DSR-response'
                    )
                    seconds_all = @($asciiSeconds) * 3
                    seconds_median = [Math]::Round($asciiSeconds, 3)
                    write_seconds_all = @($asciiSeconds - 0.001) * 3
                    write_seconds_median = [Math]::Round(
                        $asciiSeconds - 0.001,
                        3
                    )
                    writer_acceptance_mb_per_s_median = [Math]::Round(
                        ($KettlePerfPayloadContracts.ascii.bytes / 1MB) /
                            ($asciiSeconds - 0.001),
                        2
                    )
                    drain_ms_all = @(1.0) * 3
                    drain_misses = 0
                }
                sgr = @{
                    mb_per_s_median = $throughput - 1
                    bytes = $KettlePerfPayloadContracts.sgr.bytes
                    sha256 = $KettlePerfPayloadContracts.sgr.sha256
                    runs = 3
                    timing_boundary = (
                        'console-write-start-to-DSR-response'
                    )
                    seconds_all = @($sgrSeconds) * 3
                    seconds_median = [Math]::Round($sgrSeconds, 3)
                    write_seconds_all = @($sgrSeconds - 0.001) * 3
                    write_seconds_median = [Math]::Round(
                        $sgrSeconds - 0.001,
                        3
                    )
                    writer_acceptance_mb_per_s_median = [Math]::Round(
                        ($KettlePerfPayloadContracts.sgr.bytes / 1MB) /
                            ($sgrSeconds - 0.001),
                        2
                    )
                    drain_ms_all = @(1.0) * 3
                    drain_misses = 0
                }
                unicode = @{
                    mb_per_s_median = $throughput - 2
                    bytes = $KettlePerfPayloadContracts.unicode.bytes
                    sha256 = $KettlePerfPayloadContracts.unicode.sha256
                    runs = 3
                    timing_boundary = (
                        'console-write-start-to-DSR-response'
                    )
                    seconds_all = @($unicodeSeconds) * 3
                    seconds_median = [Math]::Round($unicodeSeconds, 3)
                    write_seconds_all = @($unicodeSeconds - 0.001) * 3
                    write_seconds_median = [Math]::Round(
                        $unicodeSeconds - 0.001,
                        3
                    )
                    writer_acceptance_mb_per_s_median = [Math]::Round(
                        ($KettlePerfPayloadContracts.unicode.bytes / 1MB) /
                            ($unicodeSeconds - 0.001),
                        2
                    )
                    drain_ms_all = @(1.0) * 3
                    drain_misses = 0
                }
            }
            postflood_ws_mb = 200
        } | Write-ScoreFixtureJson `
            -Path (Join-Path $Directory "throughput-$name.json") -Depth 5
        $datPath = Join-Path $Directory "vtebench-$name.dat"
        [IO.File]::WriteAllText(
            $datPath,
            "one two `n1 3 `n2 2 `n3 _ `n",
            [Text.UTF8Encoding]::new($false)
        )
        $vtebenchTerminals[$name] = [ordered]@{
            run_id = $runId
            executable = $shell
            executable_sha256 = $executableHash
            product_version = "test-$name-1.0"
            dat_path = $datPath
            dat_sha256 = (
                Get-FileHash -LiteralPath $datPath -Algorithm SHA256
            ).Hash
            benchmark_count = 2
            sample_rows = 3
            benchmarks = [ordered]@{
                one = [ordered]@{
                    samples_ms = @(1.0, 2.0, 3.0)
                    sample_count = 3
                    median_ms = 2.0
                }
                two = [ordered]@{
                    samples_ms = @(3.0, 2.0)
                    sample_count = 2
                    median_ms = 2.5
                }
            }
        }
    }
    [ordered]@{
        schema_version = 1
        run_id = $runId
        repository_commit = '0123456789abcdef0123456789abcdef01234567'
        repository_dirty = $false
        kettle_config_sha256 = (('ab' * 32) -join '')
        toolchain = [ordered]@{
            orchestrator_powershell = [ordered]@{
                path = $shell
                edition = 'Core'
                version = '7.0.0'
            }
            throughput_powershell = [ordered]@{
                path = $shell
                edition = 'Core'
                version = '7.0.0'
            }
            vtebench_wsl = $wslLauncher
        }
        machine = [ordered]@{
            manufacturer = 'Kettle Test'
            model = 'Deterministic Fixture'
            display_topology = [ordered]@{
                release_evidence_valid = $true
                topology_stable = $true
                desktop_screens = @(
                    foreach ($screen in $monitorScreens) {
                        [ordered]@{
                            device_name = $screen.device_name
                            monitor_device_id = $screen.monitor_device_id
                            primary = $screen.primary
                            effective_dpi = $screen.effective_dpi
                            refresh_hz = $screen.refresh_hz
                            bounds = $screen.bounds
                            working_area = $screen.working_area
                        }
                    }
                )
            }
        }
        settings = [ordered]@{
            window_pixels = [ordered]@{
                width = 1280
                height = 800
            }
            unidentified_display_allowed = $false
            vtebench_enabled = $true
            monitor_transition_enabled = $true
            monitor_transition_samples_per_state = 10
            probe_cooldown_seconds = 15
            vtebench_revision = (
                'ead80032e57dee2e75f0b51f2ea67528647d9944'
            )
        }
        terminals = $manifestTerminals
    } | Write-ScoreFixtureJson `
        -Path (Join-Path $Directory 'benchmark-manifest.json') -Depth 5
    [ordered]@{
        schema_version = 2
        run_id = $runId
        transport_schema = 'kettle-vtebench-channel-v1'
        workload_runner = [ordered]@{
            schema = 'kettle-vtebench-runner-v1'
            powershell = [ordered]@{
                path = $shell
                version = '7.0.0'
            }
            script = [ordered]@{
                path = (
                    Resolve-Path -LiteralPath (
                        Join-Path $PSScriptRoot 'vtebench-inside.ps1'
                    )
                ).Path
            }
        }
        source = [ordered]@{
            revision = 'ead80032e57dee2e75f0b51f2ea67528647d9944'
            benchmark_tree = ('de' * 20)
            expected_benchmark_count = 2
            wsl_binary_sha256 = ('ef' * 32)
            cargo_lock_sha256 = ('01' * 32)
            cargo_path = '/home/test/.cargo/bin/cargo'
            cargo_sha256 = ('02' * 32)
            cargo_version = 'cargo 1.88.0'
            wsl_launcher = $wslLauncher
        }
        terminals = $vtebenchTerminals
    } | Write-ScoreFixtureJson -Path (
        Join-Path $Directory 'vtebench-summary.json'
    ) -Depth 10
    [ordered]@{
        run_id = $runId
        passed = $true
        executable = $shell
        executable_sha256 = $executableHash
        helper_binaries = @(
            $manifestTerminals[0].helper_binaries
        )
        kettle_version = 'test-kettle-1.0'
        requested_samples = 50
        samples = 50
        misses = 0
        latency_ms_all = @(10.0) * 50
        latency_ms_p95 = 10.0
        latency_ms_p99 = 10.0
        long_frame_ms = 100.0
        long_frames = 0
    } | Write-ScoreFixtureJson `
        -Path (Join-Path $Directory 'menu-hover.json') -Depth 4

    $transitionObservations = @()
    $transitionStates = @('menu_closed', 'context_menu_open')
    for (
        $stateIndex = 0;
        $stateIndex -lt $transitionStates.Count;
        $stateIndex++
    ) {
        $stateName = $transitionStates[$stateIndex]
        for ($sampleIndex = 0; $sampleIndex -lt 10; $sampleIndex++) {
            $globalIndex = ($stateIndex * 10) + $sampleIndex
            $sourceIndex = $globalIndex % 2
            $targetIndex = if ($sourceIndex -eq 0) { 1 } else { 0 }
            $sourceEndpoint = $monitorEndpoints[$sourceIndex]
            $targetEndpoint = $monitorEndpoints[$targetIndex]
            $sourceDevice = [string]$sourceEndpoint.device_name
            $targetDevice = [string]$targetEndpoint.device_name
            $transitionObservations += [ordered]@{
                started_utc = '2026-07-26T12:15:00.0000000-07:00'
                state = $stateName
                sample = $sampleIndex
                direction = "$sourceDevice->$targetDevice"
                status = 'ok'
                miss_reason = $null
                source = $sourceEndpoint
                target = $targetEndpoint
                actual_target_device_name = $targetDevice
                recovery_to_capturable_client_ms = 50.0
                target_effective_dpi_observed = (
                    $targetEndpoint.effective_dpi
                )
                target_refresh_hz_observed = $targetEndpoint.refresh_hz
                capture = [ordered]@{
                    width = 1280
                    height = 800
                    bytes = 4096000
                }
                ui_geometry_surface = [ordered]@{
                    width = 1280
                    height = 800
                }
                context_menu = if (
                    $stateName -ceq 'context_menu_open'
                ) {
                    [ordered]@{
                        open = $true
                        rect = [ordered]@{
                            x = 20
                            y = 20
                            width = 240
                            height = 320
                        }
                        rows = 8
                    }
                } else {
                    [ordered]@{
                        open = $false
                        rect = $null
                        rows = 0
                    }
                }
                ui_geometry_checks = 3
            }
        }
    }
    $transitionState = [ordered]@{
        requested_samples = 10
        samples = 10
        misses = 0
        recovery_to_capturable_client_ms_all = @(50.0) * 10
        recovery_to_capturable_client_ms_median = 50.0
        recovery_to_capturable_client_ms_p95 = 50.0
        recovery_to_capturable_client_ms_max = 50.0
    }
    [ordered]@{
        schema_version = 2
        run_id = $runId
        status = 'passed'
        release_evidence_valid = $true
        metric_name = 'recovery_to_capturable_client_ms'
        topology_stable = $true
        selected_screens = $monitorEndpoints
        selection_policy = $monitorPolicy
        binary = [ordered]@{
            executable = $shell
            executable_sha256 = $executableHash
            cli_executable = $shell
            cli_executable_sha256 = $executableHash
            product_version = 'test-kettle-1.0'
            config = 'C:\kettle-perf-fixture.config'
            config_mode = 'provided'
            config_sha256 = (('ab' * 32) -join '')
        }
        requested = [ordered]@{
            samples_per_state = 10
            states = $transitionStates
            window_pixels = [ordered]@{
                width = 1280
                height = 800
            }
            recovery_timeout_ms = 5000
            geometry_stable_checks = 3
            poll_ms = 25
        }
        topology_start = $monitorTopologyStart
        topology_end = $monitorTopologyEnd
        observations = $transitionObservations
        requested_samples = 20
        samples = 20
        misses = 0
        recovery_to_capturable_client_ms_all = @(50.0) * 20
        recovery_to_capturable_client_ms_median = 50.0
        recovery_to_capturable_client_ms_p95 = 50.0
        recovery_to_capturable_client_ms_max = 50.0
        states = [ordered]@{
            menu_closed = $transitionState
            context_menu_open = $transitionState
        }
    } | Write-ScoreFixtureJson -Path (
        Join-Path $Directory 'monitor-transition.json'
    ) -Depth 16
}

function Test-VtebenchDatParser {
    param([string]$Directory)

    $parserDir = Join-Path $Directory 'vtebench-parser'
    New-Item -ItemType Directory -Force $parserDir | Out-Null
    $utf8NoBom = [Text.UTF8Encoding]::new($false)
    $goodPath = Join-Path $parserDir 'good.dat'
    [IO.File]::WriteAllText(
        $goodPath,
        "one two `n1 3 `n2 2 `n3 _ `n",
        $utf8NoBom
    )
    $parsed = Read-KettlePerfVtebenchDat `
        -Path $goodPath -ExpectedColumns 2
    if (
        $parsed.Names.Count -ne 2 -or
        @($parsed.Samples.one).Count -ne 3 -or
        @($parsed.Samples.two).Count -ne 2
    ) {
        throw 'valid column-oriented vtebench DAT parsed incorrectly'
    }
    $invalidCases = [ordered]@{
        one_byte = "`n"
        duplicate = "one one`n1 2`n"
        missing_column = "one two`n1`n"
        extra_column = "one two`n1 2 3`n"
        invalid_token = "one`nnope`n"
        empty_column = "one two`n1 _`n2 _`n"
    }
    foreach ($case in $invalidCases.GetEnumerator()) {
        $path = Join-Path $parserDir "$($case.Key).dat"
        [IO.File]::WriteAllText($path, $case.Value, $utf8NoBom)
        $rejected = $false
        try {
            [void](Read-KettlePerfVtebenchDat `
                -Path $path `
                -ExpectedColumns $(if ($case.Key -eq 'invalid_token') {
                    1
                } else {
                    2
                }))
        } catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw "invalid vtebench DAT case passed: $($case.Key)"
        }
    }
}

function Invoke-Score {
    param(
        [string]$Directory,
        [string]$BaselineDirectory = ''
    )

    $arguments = @(
        '-NoProfile', '-File', $scoreScript, '-ResultsDir', $Directory,
        '-RequireLatency', '-RequireMenuHover', '-RequireVtebench',
        '-RequireMonitorTransition'
    )
    if ($BaselineDirectory) {
        $arguments += @('-BaselineResultsDir', $BaselineDirectory)
    }
    & $shell @arguments *> $null
    return $LASTEXITCODE
}

try {
    Write-Fixture $testDir
    Test-VtebenchDatParser $testDir

    if ((Invoke-Score $testDir) -ne 0) {
        $failedScore = Get-Content -Raw -LiteralPath (
            Join-Path $testDir 'score.json'
        ) | ConvertFrom-Json
        throw (
            'complete six-terminal fixture did not pass: ' +
            (@($failedScore.manifest_issues) -join '; ')
        )
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $testDir 'score.json') |
        ConvertFrom-Json
    if (
        -not $score.passed -or
        $score.kettle_rank -ne 1 -or
        -not $score.coverage_passed -or
        -not $score.kettle_latency_data_valid
    ) {
        throw 'complete fixture produced the wrong score contract'
    }

    $vteDisabledDir = Join-Path $testDir 'vtebench-disabled'
    Write-Fixture $vteDisabledDir
    $vteDisabledManifestPath = Join-Path $vteDisabledDir (
        'benchmark-manifest.json'
    )
    $vteDisabledManifest = Get-Content -Raw `
        -LiteralPath $vteDisabledManifestPath | ConvertFrom-Json
    $vteDisabledManifest.settings.vtebench_enabled = $false
    $vteDisabledManifest | Write-ScoreFixtureJson `
        -Path $vteDisabledManifestPath -Depth 6
    if ((Invoke-Score $vteDisabledDir) -eq 0) {
        throw 'release fixture without vtebench evidence unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (
        Join-Path $vteDisabledDir 'score.json'
    ) | ConvertFrom-Json
    if (
        @($score.manifest_issues) -notcontains
            'release scoring requires vtebench evidence'
    ) {
        throw 'missing-vtebench fixture failed for the wrong reason'
    }

    $transitionSkippedDir = Join-Path $testDir 'monitor-transition-skipped'
    Write-Fixture $transitionSkippedDir
    $transitionSkippedPath = Join-Path $transitionSkippedDir (
        'monitor-transition.json'
    )
    $transitionSkipped = Get-Content -Raw `
        -LiteralPath $transitionSkippedPath | ConvertFrom-Json
    $transitionSkipped.status = 'skipped'
    $transitionSkipped.release_evidence_valid = $false
    $transitionSkipped | Write-ScoreFixtureJson `
        -Path $transitionSkippedPath -Depth 8
    if ((Invoke-Score $transitionSkippedDir) -eq 0) {
        throw 'skipped monitor-transition evidence unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (
        Join-Path $transitionSkippedDir 'score.json'
    ) | ConvertFrom-Json
    if (
        @($score.monitor_transition_issues).Count -lt 1 -or
        $score.monitor_transition_passed
    ) {
        throw 'skipped monitor-transition fixture failed for the wrong reason'
    }

    $baselineDir = Join-Path $testDir 'compatible-baseline'
    Write-Fixture $baselineDir
    if ((Invoke-Score $testDir $baselineDir) -ne 0) {
        $failedScore = Get-Content -Raw -LiteralPath (
            Join-Path $testDir 'score.json'
        ) | ConvertFrom-Json
        throw (
            'same-environment baseline fixture did not pass: ' +
            (@($failedScore.baseline_issues) -join '; ')
        )
    }

    $badBaselineDir = Join-Path $testDir 'incompatible-baseline'
    Write-Fixture $badBaselineDir
    $badBaselinePath = Join-Path $badBaselineDir 'benchmark-manifest.json'
    $badBaseline = Get-Content -Raw -LiteralPath $badBaselinePath |
        ConvertFrom-Json
    $badBaseline.machine.model = 'Different Machine'
    $badBaseline | Write-ScoreFixtureJson `
        -Path $badBaselinePath -Depth 6
    if ((Invoke-Score $testDir $badBaselineDir) -eq 0) {
        throw 'different-machine baseline unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $testDir 'score.json') |
        ConvertFrom-Json
    if ($score.baseline_compatible -or $score.baseline_issues.Count -lt 1) {
        throw 'different-machine baseline failed for the wrong reason'
    }

    Remove-Item -LiteralPath (Join-Path $testDir 'throughput-rio.json')
    Remove-Item -LiteralPath (Join-Path $testDir 'throughput-tabby.json')
    if ((Invoke-Score $testDir) -eq 0) {
        throw 'fixture with only three throughput peers unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $testDir 'score.json') |
        ConvertFrom-Json
    if (
        -not $score.coverage_passed -or
        $score.throughput_passed -or
        $score.throughput_peers_measured -ne 3
    ) {
        throw 'missing-throughput fixture failed for the wrong reason'
    }

    Write-Fixture (Join-Path $testDir 'latency-negative')
    $latencyDir = Join-Path $testDir 'latency-negative'
    $latencyPath = Join-Path $latencyDir 'latency.json'
    $latency = Get-Content -Raw -LiteralPath $latencyPath | ConvertFrom-Json
    $latency.kettle.samples = 17
    $latency.kettle.misses = 3
    $latency | Write-ScoreFixtureJson -Path $latencyPath -Depth 5
    if ((Invoke-Score $latencyDir) -eq 0) {
        throw '15% latency-miss fixture unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $latencyDir 'score.json') |
        ConvertFrom-Json
    if (
        $score.kettle_latency_data_valid -or
        $score.latency_passed -or
        $score.coverage_passed
    ) {
        throw 'latency-miss fixture failed for the wrong reason'
    }

    $hashDir = Join-Path $testDir 'throughput-hash-negative'
    Write-Fixture $hashDir
    $hashPath = Join-Path $hashDir 'throughput-kettle.json'
    $hashFixture = Get-Content -Raw -LiteralPath $hashPath | ConvertFrom-Json
    $hashFixture.payloads.unicode.sha256 = '00'
    $hashFixture | Write-ScoreFixtureJson -Path $hashPath -Depth 5
    if ((Invoke-Score $hashDir) -eq 0) {
        throw 'corrupt throughput payload hash unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $hashDir 'score.json') |
        ConvertFrom-Json
    if ($score.throughput_passed) {
        throw 'corrupt throughput payload hash failed for the wrong reason'
    }

    $encodingDir = Join-Path $testDir 'throughput-encoding-negative'
    Write-Fixture $encodingDir
    $encodingPath = Join-Path $encodingDir 'throughput-kettle.json'
    $encodingFixture = Get-Content -Raw -LiteralPath $encodingPath |
        ConvertFrom-Json
    $encodingFixture.output_encoding = 'ibm437'
    $encodingFixture | Write-ScoreFixtureJson `
        -Path $encodingPath -Depth 5
    if ((Invoke-Score $encodingDir) -eq 0) {
        throw 'non-UTF-8 throughput result unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $encodingDir 'score.json') |
        ConvertFrom-Json
    if ($score.throughput_passed) {
        throw 'non-UTF-8 throughput result failed for the wrong reason'
    }

    $rawDir = Join-Path $testDir 'throughput-raw-negative'
    Write-Fixture $rawDir
    $rawPath = Join-Path $rawDir 'throughput-kettle.json'
    $rawFixture = Get-Content -Raw -LiteralPath $rawPath | ConvertFrom-Json
    $rawFixture.payloads.ascii.mb_per_s_median += 5
    $rawFixture | Write-ScoreFixtureJson -Path $rawPath -Depth 5
    if ((Invoke-Score $rawDir) -eq 0) {
        throw 'throughput summary inconsistent with raw timings unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $rawDir 'score.json') |
        ConvertFrom-Json
    if ($score.throughput_passed) {
        throw 'inconsistent throughput summary failed for the wrong reason'
    }

    $manifestDir = Join-Path $testDir 'manifest-negative'
    Write-Fixture $manifestDir
    $manifestPath = Join-Path $manifestDir 'benchmark-manifest.json'
    $manifestFixture = Get-Content -Raw -LiteralPath $manifestPath |
        ConvertFrom-Json
    $manifestFixture.repository_commit = ''
    $manifestFixture.machine.display_topology.release_evidence_valid = $false
    $manifestFixture.settings.unidentified_display_allowed = $true
    $manifestFixture | Write-ScoreFixtureJson `
        -Path $manifestPath -Depth 6
    if ((Invoke-Score $manifestDir) -eq 0) {
        throw 'invalid release provenance unexpectedly passed'
    }
    $score = Get-Content -Raw -LiteralPath (Join-Path $manifestDir 'score.json') |
        ConvertFrom-Json
    if ($score.coverage_passed -or $score.manifest_issues.Count -lt 3) {
        throw 'invalid release provenance failed for the wrong reason'
    }

    Write-Host 'performance score self-test: PASS'
} finally {
    $resolved = [IO.Path]::GetFullPath($testDir)
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to clean self-test directory outside the OS temp root: $resolved"
    }
    if (Test-Path -LiteralPath $resolved -PathType Container) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
