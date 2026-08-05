# Controlled cold-start, fresh working-set, and idle-CPU probe.
# Startup stops only after the requested client is on the target screen, a
# common PowerShell 7 child has painted a unique truecolor marker, and the
# terminal has answered CSI 5n with CSI 0n. Williams-balanced rounds control
# position, predecessor, and thermal drift across terminals.
param(
    [string[]]$Terminals = @(
        'kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby'
    ),
    [string]$ResultsDir = '',
    [string]$KettleExe = '',
    [string]$KettleConfig = '',
    [string]$WindowsTerminalExe = '',
    [string]$AlacrittyExe = '',
    [string]$WeztermExe = '',
    [string]$RioExe = '',
    [string]$TabbyExe = '',
    [hashtable]$TerminalVersions = @{},
    $IsolatedProfile = $null,
    [string]$PowerShellExe = '',
    [string]$TargetScreenDevice = '',
    [ValidatePattern('^[0-9a-fA-F-]{36}$')]
    [string]$RunId = '',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$')]
    [string]$StartupScheduleSeed = 'kettle-startup-release-v1',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$')]
    [string]$IdleScheduleSeed = 'kettle-idle-release-v1',
    [ValidateRange(6, 1000)]
    [int]$StartupRuns = 12,
    [ValidateRange(6, 1000)]
    [int]$IdleSamples = 6,
    [ValidateRange(1, 86400)]
    [int]$IdleSeconds = 10,
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800,
    [ValidateRange(0, 600)]
    [int]$SampleCooldownSeconds = 2,
    # Smoke only: measure whatever comparators the machine can actually offer,
    # with a position-balanced rotation instead of a Williams square. Release
    # mode rejects this -- see perf-all.ps1.
    [switch]$AllowUnbalanced,
    # Smoke only: measure even though another instance of a measured terminal
    # is already running. Safe only for launches that force a new process --
    # `Start-KettlePerfCommandWindow` checks that and records the tolerated
    # PIDs, because those processes still contend for CPU and GPU.
    [switch]$AllowForeignInstances
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\schedule.ps1"
. "$PSScriptRoot\startup-ready.ps1"

if (
    (-not $AllowUnbalanced -and (
        $Terminals.Count -lt 6 -or ($Terminals.Count % 2) -ne 0
    )) -or
    $Terminals.Count -lt 2 -or
    ($StartupRuns % $Terminals.Count) -ne 0 -or
    ($IdleSamples % $Terminals.Count) -ne 0
) {
    throw (
        'Startup/idle interleaving requires an even set of at least six ' +
        'terminals and sample counts divisible by the terminal count'
    )
}
if (-not $ResultsDir) {
    $ResultsDir = Join-Path $PSScriptRoot '..\..\target\perf-results'
}
if (-not $KettleExe) {
    $KettleExe = Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'
}
if (-not $PowerShellExe) {
    $PowerShellExe = Get-Command pwsh.exe -CommandType Application `
        -ErrorAction Stop |
        Select-Object -First 1 -ExpandProperty Source
}
if (-not $RunId) {
    $RunId = [Guid]::NewGuid().ToString('D')
}
$PowerShellExe = Resolve-KettlePerfStartupReadyPowerShell $PowerShellExe
New-Item -ItemType Directory -Force $ResultsDir | Out-Null
$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
$resultsRoot = Open-KettlePerfPersistenceRoot -Directory $ResultsDir
$scratchParent = Join-Path $ResultsDir 'startup-ready-scratch'
if (Test-Path -LiteralPath $scratchParent) {
    if (
        -not (Test-Path -LiteralPath $scratchParent -PathType Container) -or
        @(Get-ChildItem -LiteralPath $scratchParent -Force).Count -gt 0
    ) {
        throw 'Startup-readiness scratch parent must be absent or empty'
    }
} else {
    New-Item -ItemType Directory -Path $scratchParent | Out-Null
}
$scratchParent = (Resolve-Path -LiteralPath $scratchParent).Path

$scheduleFor = if ($AllowUnbalanced -and (
        $Terminals.Count -lt 6 -or ($Terminals.Count % 2) -ne 0
    )) { 'New-KettlePerfRotationSchedule' } else { 'New-KettlePerfWilliamsSchedule' }
$startupSchedule = & $scheduleFor `
    -Terminals $Terminals -Seed $StartupScheduleSeed `
    -Cycles ([int]($StartupRuns / $Terminals.Count)) `
    -Namespace 'startup'
$idleSchedule = & $scheduleFor `
    -Terminals $Terminals -Seed $IdleScheduleSeed `
    -Cycles ([int]($IdleSamples / $Terminals.Count)) `
    -Namespace 'idle'

$specs = [ordered]@{}
$all = [ordered]@{}
foreach ($terminal in $Terminals) {
    $isolatedConfig = Get-KettlePerfIsolatedConfigEntry `
        -ConfigProfile $IsolatedProfile -Name $terminal
    if ($terminal -eq 'kettle' -and $KettleConfig) {
        $isolatedConfig = $null
    }
    $spec = Resolve-KettlePerfTerminal -Name $terminal `
        -KettleExe $KettleExe -KettleConfig $KettleConfig `
        -WindowsTerminalExe $WindowsTerminalExe `
        -AlacrittyExe $AlacrittyExe -WeztermExe $WeztermExe `
        -RioExe $RioExe -TabbyExe $TabbyExe `
        -VersionOverride ([string]$TerminalVersions[$terminal]) `
        -IsolatedConfig $isolatedConfig
    if (-not $spec.Available -or -not $spec.SupportsCommand) {
        throw "$terminal does not provide an available command-launch contract"
    }
    $specs[$terminal] = $spec
    $all[$terminal] = [ordered]@{
        run_id = $RunId
        launcher = $spec.Exe
        executable = $spec.BenchmarkExe
        executable_sha256 = $spec.BenchmarkExeSha256
        product_version = Get-KettlePerfVersion $spec
        configuration_mode = $spec.ConfigurationMode
        configuration_evidence = $spec.ConfigurationEvidence
        helper_binaries = [object[]]@($spec.HelperBinaries)
        startup_schedule_algorithm = $startupSchedule.algorithm
        startup_schedule_seed_sha256 = $startupSchedule.seed_sha256
        idle_schedule_algorithm = $idleSchedule.algorithm
        idle_schedule_seed_sha256 = $idleSchedule.seed_sha256
        startup_requested_samples = $StartupRuns
        startup_samples = 0
        startup_misses = 0
        startup_ms_all = [Collections.Generic.List[double]]::new()
        startup_observations = [Collections.Generic.List[object]]::new()
        idle_requested_samples = $IdleSamples
        idle_samples = 0
        idle_misses = 0
        idle_seconds = $IdleSeconds
        idle_cpu_pct_all = [Collections.Generic.List[double]]::new()
        fresh_ws_mb_all = [Collections.Generic.List[double]]::new()
        idle_observations = [Collections.Generic.List[object]]::new()
        readiness_workload_pids = [Collections.Generic.List[int]]::new()
        readiness = [ordered]@{
            schema = 'kettle-startup-ready-v1'
            shell = $PowerShellExe
            shell_sha256 = Get-KettlePerfExecutableSha256 $PowerShellExe
            helper_script = (Resolve-Path -LiteralPath (
                Join-Path $PSScriptRoot 'startup-ready.ps1'
            )).Path
            helper_script_sha256 = (
                Get-FileHash -LiteralPath (
                    Join-Path $PSScriptRoot 'startup-ready.ps1'
                ) -Algorithm SHA256
            ).Hash
            boundary = (
                'process-spawn-through-exact-client-placement-' +
                'truecolor-paint-and-CSI-5n-CSI-0n'
            )
            target_attribution = (
                'validated-after-painted-endpoint-and-excluded-from-startup-ms'
            )
            milestones_recorded = [string[]]@(
                'window_discovered_ms',
                'sized_focused_ms',
                'go_published_ms',
                'go_to_ready_ms',
                'post_endpoint_attribution_ms'
            )
            capture_scope = 'top-left-client-roi'
            presentation_limit = (
                'terminal-parser-and-capturable-client-not-display-photon'
            )
        }
        acclimation_runs = 0
        window_pixels = [ordered]@{
            width = $WindowW
            height = $WindowH
        }
    }
}

function Close-KettlePerfReadyLaunch {
    param(
        $Context
    )

    if ($null -eq $Context) {
        return
    }
    if ($null -ne $Context.Launched) {
        [void](Close-SpawnedTerminal -Hwnd $Context.Launched.Hwnd `
            -ExpectedPid $Context.Launched.WindowPid `
            -PreexistingPids $Context.PreexistingPids)
        try {
            if (-not $Context.Launched.Process.HasExited) {
                Stop-Process -Id $Context.Launched.Process.Id -Force
            }
        } catch {
            Write-Verbose (
                'startup/idle launcher cleanup raced process exit: ' +
                $_.Exception.Message
            )
        }
        if ($null -ne $Context.Launched.CommandWrapper) {
            Close-KettlePerfCommandWrapper `
                $Context.Launched.CommandWrapper
        }
        Close-KettlePerfExecutableLease `
            $Context.Launched.ExecutableLease
    }
    if ($null -ne $Context.Descriptor) {
        [void](Remove-KettlePerfStartupReadyScratch `
            -Descriptor $Context.Descriptor -Confirm:$false)
    }
}

function Start-KettlePerfReadyLaunch {
    param(
        [Parameter(Mandatory)]
        [string]$Terminal,
        [Parameter(Mandatory)]
        $Spec,
        [Parameter(Mandatory)]
        [string]$SampleKey
    )

    $descriptor = New-KettlePerfStartupReadyDescriptor `
        -RunId $RunId -ScratchParent $scratchParent `
        -SampleId $SampleKey -PowerShellExecutable $PowerShellExe
    $before = Get-VisibleWindowSet
    $preexistingPids = Get-PidSet
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $launched = $null
    try {
        $launched = Start-KettlePerfCommandWindow -Spec $Spec `
            -Command $descriptor.Command -BeforeWindows $before `
            -PreexistingPids $preexistingPids `
            -CommandWrapperDirectory $ResultsDir `
            -AllowForeignInstances:$AllowForeignInstances `
            -DeferTargetAttribution
        $windowDiscoveredMs = $timer.Elapsed.TotalMilliseconds
        Set-WindowSize $launched.Hwnd $WindowW $WindowH `
            $TargetScreenDevice
        if (-not (Confirm-KettlePerfForegroundWindow -Hwnd $launched.Hwnd)) {
            throw "$Terminal startup-readiness window did not take foreground"
        }
        $sizedFocusedMs = $timer.Elapsed.TotalMilliseconds
        [void](Publish-KettlePerfStartupReadyGo $descriptor)
        $goPublishedMs = $timer.Elapsed.TotalMilliseconds
        # Clamp the readiness region to the client the window ACTUALLY has, not
        # to the size that was requested.
        #
        # `CaptureWindowRegion` refuses a region that does not fit inside the
        # client rect -- correctly, it would read outside the bitmap -- and
        # returns null. The poll below only evaluates the painted marker when
        # the capture is non-null, so a region even one pixel too wide makes
        # `paintReady` unreachable: the loop spins for the full 30s and reports
        # `paint=False`, which reads as "the terminal never painted" when what
        # actually happened is "the harness never looked". A terminal whose
        # client quantizes to whole character cells can land narrower than the
        # requested width, so this is reachable without anything being wrong
        # with the terminal.
        #
        # The timeout below reports a null capture as "no pixels were read at
        # all" rather than naming a cause. The region not fitting is only one of
        # several ways `CaptureWindowRegion` returns null -- `PrintWindow`
        # refusing, or a device-context or bitmap allocation failing, are others
        # -- and a diagnostic that asserts one cause sends the reader down one
        # path. The geometry printed beside it is what tells them apart.
        # `CaptureWindow` reports the client it actually measured, which is the
        # same rect `CaptureWindowRegion` validates against. A failed
        # measurement is NOT fatal: `Set-WindowSize` above already proved the
        # client is exactly the requested size, so falling back to that is both
        # correct and the status quo. This clamp exists for the case where the
        # window later disagrees, not to add a new way to fail.
        $clientWidth = 0
        $clientHeight = 0
        [void][KettlePerf.Native]::CaptureWindow(
            $launched.Hwnd,
            [ref]$clientWidth,
            [ref]$clientHeight
        )
        if ($clientWidth -le 0) { $clientWidth = $WindowW }
        if ($clientHeight -le 0) { $clientHeight = $WindowH }
        $roiWidth = [Math]::Min(1024, [Math]::Min($WindowW, $clientWidth))
        $roiHeight = [Math]::Min(384, [Math]::Min($WindowH, $clientHeight))
        $readyDeadline = (Get-Date).AddSeconds(30)
        $markerReady = $false
        $paintReady = $false
        $captureAttempts = 0
        $captureMs = [Collections.Generic.List[double]]::new()
        while ((Get-Date) -lt $readyDeadline) {
            if (-not $markerReady -and (
                Test-Path -LiteralPath $descriptor.ReadyPath -PathType Leaf
            )) {
                if (-not (Test-KettlePerfStartupReadyMarker $descriptor)) {
                    throw "$Terminal published an invalid startup-ready marker"
                }
                $markerReady = $true
            }
            $captureTimer = [Diagnostics.Stopwatch]::StartNew()
            $capture = [KettlePerf.Native]::CaptureWindowRegion(
                $launched.Hwnd,
                0,
                0,
                $roiWidth,
                $roiHeight
            )
            $captureTimer.Stop()
            $captureAttempts++
            $captureMs.Add($captureTimer.Elapsed.TotalMilliseconds)
            if ($null -ne $capture) {
                $paintReady = Test-KettlePerfPaintedMarkerCapture `
                    -BgraBytes $capture -Width $roiWidth -Height $roiHeight `
                    -ExpectedRed $descriptor.MarkerRed `
                    -ExpectedGreen $descriptor.MarkerGreen `
                    -ExpectedBlue $descriptor.MarkerBlue
            }
            if ($markerReady -and $paintReady) {
                $timer.Stop()
                $attributionTimer = [Diagnostics.Stopwatch]::StartNew()
                $launched = Complete-KettlePerfTargetAttribution `
                    -Launch $launched `
                    -PreexistingPids $preexistingPids `
                    -TerminalName $Terminal
                $attributionTimer.Stop()
                return [pscustomobject]@{
                    Terminal = $Terminal
                    Descriptor = $descriptor
                    Launched = $launched
                    PreexistingPids = $preexistingPids
                    ElapsedMs = $timer.Elapsed.TotalMilliseconds
                    WindowDiscoveredMs = $windowDiscoveredMs
                    SizedFocusedMs = $sizedFocusedMs
                    GoPublishedMs = $goPublishedMs
                    GoToReadyMs = (
                        $timer.Elapsed.TotalMilliseconds - $goPublishedMs
                    )
                    PostEndpointAttributionMs = (
                        $attributionTimer.Elapsed.TotalMilliseconds
                    )
                    CaptureAttempts = $captureAttempts
                    CaptureMs = [double[]]$captureMs.ToArray()
                    CaptureRegion = [ordered]@{
                        x = 0
                        y = 0
                        width = $roiWidth
                        height = $roiHeight
                    }
                }
            }
            Start-Sleep -Milliseconds 10
        }
        # Report how long the polling itself cost, not just how many times it
        # ran. A low attempt count means one of two very different things --
        # the terminal was slow, or the poll was -- and without the per-attempt
        # cost the message reads as an accusation against the terminal either
        # way. That is not hypothetical: an interpreted pixel walk once made
        # each miss cost ~2.6s, so a 30s deadline bought eight looks and
        # reported a slow-painting terminal as one that never painted.
        $slowestCaptureMs = if ($captureMs.Count -gt 0) {
            [Math]::Round(($captureMs | Measure-Object -Maximum).Maximum, 1)
        } else {
            0
        }
        $totalCaptureMs = if ($captureMs.Count -gt 0) {
            [Math]::Round(($captureMs | Measure-Object -Sum).Sum, 1)
        } else {
            0
        }
        throw (
            "$Terminal startup readiness timed out; " +
            "marker=$markerReady paint=$paintReady " +
            "(client ${clientWidth}x${clientHeight}, roi ${roiWidth}x${roiHeight}, " +
            "captures=$captureAttempts, slowest ${slowestCaptureMs}ms, " +
            "${totalCaptureMs}ms of the deadline spent capturing, last capture " +
            "$(if ($null -eq $capture) { 'NULL -- no pixels were read at all' } `
              else { 'ok, the marker pixels were not in it' }))"
        )
    } catch {
        $timer.Stop()
        Close-KettlePerfReadyLaunch ([pscustomobject]@{
            Descriptor = $descriptor
            Launched = $launched
            PreexistingPids = $preexistingPids
        })
        throw
    }
}

function Get-KettlePerfStableCpuDelta {
    param(
        [Parameter(Mandatory)]
        $Before,
        [Parameter(Mandatory)]
        $After,
        [Parameter(Mandatory)]
        [string]$Terminal
    )

    if (
        @($Before.SamplingMisses).Count -ne 0 -or
        @($After.SamplingMisses).Count -ne 0
    ) {
        throw "$Terminal process tree changed during CPU sampling"
    }
    $beforeSamples = [object[]]@($Before.ProcessSamples)
    $afterSamples = [object[]]@($After.ProcessSamples)
    if (
        $beforeSamples.Count -eq 0 -or
        $beforeSamples.Count -ne $afterSamples.Count
    ) {
        throw "$Terminal idle process-tree cardinality changed"
    }
    $afterByPid = @{}
    foreach ($sample in $afterSamples) {
        $pidValue = [int]$sample.pid
        if ($afterByPid.ContainsKey($pidValue)) {
            throw "$Terminal idle process tree contains a duplicate PID"
        }
        $afterByPid[$pidValue] = $sample
    }
    $delta = 0.0
    foreach ($beforeSample in $beforeSamples) {
        $pidValue = [int]$beforeSample.pid
        if (-not $afterByPid.ContainsKey($pidValue)) {
            throw "$Terminal idle process $pidValue exited or was reparented"
        }
        $afterSample = $afterByPid[$pidValue]
        if (
            [int64]$afterSample.start_time_utc_ticks -ne
                [int64]$beforeSample.start_time_utc_ticks -or
            [string]$afterSample.process_name -cne
                [string]$beforeSample.process_name
        ) {
            throw "$Terminal idle process $pidValue changed identity"
        }
        $processDelta = (
            [double]$afterSample.cpu_seconds -
            [double]$beforeSample.cpu_seconds
        )
        if ($processDelta -lt -0.000001) {
            throw "$Terminal idle process $pidValue CPU time decreased"
        }
        $delta += [Math]::Max(0.0, $processDelta)
    }
    return $delta
}

try {
    # Exactly one unmeasured controlled launch per terminal primes driver and
    # font caches before the balanced measured schedule begins.
    foreach ($terminal in $startupSchedule.rounds[0].terminals) {
        Write-Host ">> $terminal startup acclimation"
        $context = $null
        try {
            $context = Start-KettlePerfReadyLaunch `
                -Terminal $terminal -Spec $specs[$terminal] `
                -SampleKey "acclimation-$terminal"
            $all[$terminal].acclimation_runs++
        } finally {
            Close-KettlePerfReadyLaunch $context
        }
        if ($SampleCooldownSeconds -gt 0) {
            Start-Sleep -Seconds $SampleCooldownSeconds
        }
    }

    foreach ($round in $startupSchedule.rounds) {
        foreach ($visit in $round.visits) {
            $terminal = [string]$visit.terminal
            Write-Host (
                '>> {0} startup sample {1}/{2} (round {3}, position {4})' -f
                $terminal,
                $visit.sample_id,
                $startupSchedule.sample_count,
                $visit.round,
                $visit.position
            )
            $context = $null
            try {
                $context = Start-KettlePerfReadyLaunch `
                    -Terminal $terminal -Spec $specs[$terminal] `
                    -SampleKey $visit.sample_key
                $value = [Math]::Round($context.ElapsedMs, 3)
                $all[$terminal].startup_samples++
                $all[$terminal].startup_ms_all.Add($value)
                $all[$terminal].readiness_workload_pids.Add(
                    [int]$context.Launched.TargetPid
                )
                $all[$terminal].startup_observations.Add(
                    [pscustomobject][ordered]@{
                        terminal = $terminal
                        metric = 'startup_ms'
                        cluster_id = "c$($visit.cycle)-r$($visit.round)"
                        sample_id = [int]$visit.sample_id
                        sample_key = [string]$visit.sample_key
                        cycle = [int]$visit.cycle
                        round = [int]$visit.round
                        round_in_cycle = [int]$visit.round_in_cycle
                        position = [int]$visit.position
                        sequence = [int]$visit.sequence
                        value = $value
                        status = 'ok'
                        capture_attempts = $context.CaptureAttempts
                        capture_ms_all = [double[]]$context.CaptureMs
                        capture_region = $context.CaptureRegion
                        window_discovered_ms = [Math]::Round(
                            $context.WindowDiscoveredMs,
                            3
                        )
                        sized_focused_ms = [Math]::Round(
                            $context.SizedFocusedMs,
                            3
                        )
                        go_published_ms = [Math]::Round(
                            $context.GoPublishedMs,
                            3
                        )
                        go_to_ready_ms = [Math]::Round(
                            $context.GoToReadyMs,
                            3
                        )
                        post_endpoint_attribution_ms = [Math]::Round(
                            $context.PostEndpointAttributionMs,
                            3
                        )
                        workload_pid = [int]$context.Launched.TargetPid
                    }
                )
            } catch {
                $all[$terminal].startup_misses++
                throw
            } finally {
                Close-KettlePerfReadyLaunch $context
            }
            if ($SampleCooldownSeconds -gt 0) {
                Start-Sleep -Seconds $SampleCooldownSeconds
            }
        }
    }

    foreach ($round in $idleSchedule.rounds) {
        foreach ($visit in $round.visits) {
            $terminal = [string]$visit.terminal
            Write-Host (
                '>> {0} idle sample {1}/{2} (round {3}, position {4})' -f
                $terminal,
                $visit.sample_id,
                $idleSchedule.sample_count,
                $visit.round,
                $visit.position
            )
            $context = $null
            try {
                $context = Start-KettlePerfReadyLaunch `
                    -Terminal $terminal -Spec $specs[$terminal] `
                    -SampleKey $visit.sample_key
                $hwnd = $context.Launched.Hwnd
                # Re-ACQUIRE rather than merely assert. The launch already took
                # the foreground, but readiness then polls for up to 30s, and
                # anything on the machine can take it during that window -- so a
                # bare assertion here fails for reasons that have nothing to do
                # with the terminal being measured.
                #
                # This does not weaken the property that matters. Whether the
                # terminal SURRENDERS the foreground is asserted by the in-loop
                # check below, which runs throughout the measurement and is left
                # exactly as it was; this line only establishes the precondition
                # the measurement needs before it starts.
                if (-not (Confirm-KettlePerfForegroundWindow -Hwnd $hwnd)) {
                    throw "$terminal lost foreground before idle measurement"
                }
                $beforeStats = Get-ProcessTreeStats `
                    -RootPid $context.Launched.WindowPid `
                    -ExcludeRootPids @($context.Launched.TargetPid)
                if (
                    @($beforeStats.ExcludedPids) -notcontains
                        $context.Launched.TargetPid
                ) {
                    throw "$terminal readiness workload was not excluded"
                }
                $idleTimer = [Diagnostics.Stopwatch]::StartNew()
                while (
                    $idleTimer.Elapsed.TotalSeconds -lt $IdleSeconds
                ) {
                    Start-Sleep -Milliseconds 250
                    if (
                        [KettlePerf.Native]::GetForegroundWindow() -ne
                            $hwnd
                    ) {
                        throw "$terminal lost foreground during idle measurement"
                    }
                }
                $idleTimer.Stop()
                $afterStats = Get-ProcessTreeStats `
                    -RootPid $context.Launched.WindowPid `
                    -ExcludeRootPids @($context.Launched.TargetPid)
                $cpuDelta = Get-KettlePerfStableCpuDelta `
                    -Before $beforeStats -After $afterStats `
                    -Terminal $terminal
                $idleCpu = (
                    ($cpuDelta / $idleTimer.Elapsed.TotalSeconds) * 100.0
                )
                $idleCpu = [Math]::Round($idleCpu, 6)
                $freshWs = [Math]::Round(
                    [double]$beforeStats.WorkingSetMB,
                    3
                )
                $all[$terminal].idle_samples++
                $all[$terminal].idle_cpu_pct_all.Add($idleCpu)
                $all[$terminal].fresh_ws_mb_all.Add($freshWs)
                $all[$terminal].readiness_workload_pids.Add(
                    [int]$context.Launched.TargetPid
                )
                $all[$terminal].idle_observations.Add(
                    [pscustomobject][ordered]@{
                        terminal = $terminal
                        cluster_id = "c$($visit.cycle)-r$($visit.round)"
                        sample_id = [int]$visit.sample_id
                        sample_key = [string]$visit.sample_key
                        cycle = [int]$visit.cycle
                        round = [int]$visit.round
                        round_in_cycle = [int]$visit.round_in_cycle
                        position = [int]$visit.position
                        sequence = [int]$visit.sequence
                        status = 'ok'
                        idle_cpu_pct = $idleCpu
                        fresh_ws_mb = $freshWs
                        measured_seconds = $idleTimer.Elapsed.TotalSeconds
                        workload_pid = [int]$context.Launched.TargetPid
                        excluded_pids = [int[]]@(
                            $beforeStats.ExcludedPids
                        )
                        included_processes_before = [object[]]@(
                            $beforeStats.ProcessSamples
                        )
                        included_processes_after = [object[]]@(
                            $afterStats.ProcessSamples
                        )
                        cpu_seconds_delta = $cpuDelta
                    }
                )
            } catch {
                $all[$terminal].idle_misses++
                throw
            } finally {
                Close-KettlePerfReadyLaunch $context
            }
            if ($SampleCooldownSeconds -gt 0) {
                Start-Sleep -Seconds $SampleCooldownSeconds
            }
        }
    }

    foreach ($terminal in $Terminals) {
        $result = $all[$terminal]
        if (
            $result.acclimation_runs -ne 1 -or
            $result.startup_samples -ne $StartupRuns -or
            $result.startup_misses -ne 0 -or
            $result.idle_samples -ne $IdleSamples -or
            $result.idle_misses -ne 0
        ) {
            throw "$terminal startup/idle schedule coverage is incomplete"
        }
        $result.startup_ms_median = Get-KettlePerfMedian @(
            $result.startup_ms_all | Sort-Object
        )
        $result.idle_cpu_pct = Get-KettlePerfMedian @(
            $result.idle_cpu_pct_all | Sort-Object
        )
        $result.fresh_ws_mb = Get-KettlePerfMedian @(
            $result.fresh_ws_mb_all | Sort-Object
        )
        $result.startup_ms_all = [double[]](
            $result.startup_ms_all.ToArray()
        )
        $result.idle_cpu_pct_all = [double[]](
            $result.idle_cpu_pct_all.ToArray()
        )
        $result.fresh_ws_mb_all = [double[]](
            $result.fresh_ws_mb_all.ToArray()
        )
        $result.startup_observations = [object[]](
            $result.startup_observations.ToArray()
        )
        $result.idle_observations = [object[]](
            $result.idle_observations.ToArray()
        )
        $result.readiness_workload_pids = [int[]](
            $result.readiness_workload_pids.ToArray()
        )
        Write-Host (
            '{0,-10} startup {1,8:N2} ms; fresh WS {2,8:N2} MB; idle CPU {3,8:N4}%' -f
            $terminal,
            $result.startup_ms_median,
            $result.fresh_ws_mb,
            $result.idle_cpu_pct
        )
    }

    Write-KettlePerfJsonFile `
        -Path (Join-Path $ResultsDir 'startup-idle.json') `
        -InputObject $all -Depth 10 -Root $resultsRoot
    Write-Host "done - results in $ResultsDir"
} finally {
    if (
        (Test-Path -LiteralPath $scratchParent -PathType Container) -and
        @(Get-ChildItem -LiteralPath $scratchParent -Force).Count -eq 0
    ) {
        [IO.Directory]::Delete($scratchParent, $false)
    }
    Close-KettlePerfPersistenceRoot $resultsRoot
}
