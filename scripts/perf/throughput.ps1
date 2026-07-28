# Interleaved cross-terminal throughput orchestrator.
# Each Williams-balanced visit launches one isolated terminal and runs every
# pinned payload exactly once. The timed boundary is console-write start through
# the terminal's CSI 5n -> CSI 0n response: parser round-trip drain, not display
# presentation. Raw paired rounds are retained for bootstrap release gates.
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
    [string]$ScheduleSeed = 'kettle-throughput-release-v1',
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800,
    [ValidateRange(1, 86400)]
    [int]$TimeoutSec = 600,
    [ValidateRange(6, 1000)]
    [int]$Iterations = 6,
    [ValidateRange(0, 600)]
    [int]$VisitCooldownSeconds = 2
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\schedule.ps1"
. "$PSScriptRoot\go-signal.ps1"
. "$PSScriptRoot\throughput-channel.ps1"

function Assert-KettlePerfThroughputRunnerEvidence {
    param(
        [Parameter(Mandatory = $true)]
        $Actual,
        [Parameter(Mandatory = $true)]
        $Expected,
        [Parameter(Mandatory = $true)]
        [string]$Terminal
    )

    if (
        $null -eq $Actual -or
        [string]$Actual.schema -cne [string]$Expected.schema -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$Actual.powershell.path,
            [string]$Expected.powershell.path
        ) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$Actual.powershell.sha256,
            [string]$Expected.powershell.sha256
        ) -or
        [string]$Actual.powershell.version -cne
            [string]$Expected.powershell.version -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$Actual.script.path,
            [string]$Expected.script.path
        ) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$Actual.script.sha256,
            [string]$Expected.script.sha256
        )
    ) {
        throw "$Terminal throughput workload runner provenance differs"
    }
}

if (
    $Terminals.Count -lt 6 -or
    ($Terminals.Count % 2) -ne 0 -or
    ($Iterations % $Terminals.Count) -ne 0
) {
    throw (
        'Throughput interleaving requires an even set of at least six ' +
        'terminals and Iterations divisible by the terminal count'
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
if (-not (Test-Path -LiteralPath $PowerShellExe -PathType Leaf)) {
    throw "PowerShell 7 workload runner not found: $PowerShellExe"
}
$PowerShellExe = (Resolve-Path -LiteralPath $PowerShellExe).Path

New-Item -ItemType Directory -Force $ResultsDir | Out-Null
$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
$resultsRoot = Open-KettlePerfPersistenceRoot -Directory $ResultsDir
& "$PSScriptRoot\gen-payloads.ps1" | Out-Null

$runner = (
    Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'run-inside.ps1')
).Path

$powerShellLock = $null
$runnerLock = $null
try {
    $powerShellLock = [IO.File]::Open(
        $PowerShellExe,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $runnerLock = [IO.File]::Open(
        $runner,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $powerShellVersion = (
        & $PowerShellExe -NoLogo -NoProfile -NonInteractive -Command `
            '[Console]::Out.Write($PSVersionTable.PSVersion.ToString())'
    ) -join ''
    if ($LASTEXITCODE -ne 0 -or -not $powerShellVersion) {
        throw 'Could not identify the throughput workload PowerShell version'
    }
    $workloadRunner = [ordered]@{
        schema = 'kettle-throughput-runner-v1'
        powershell = [ordered]@{
            path = $PowerShellExe
            sha256 = (
                Get-FileHash -LiteralPath $PowerShellExe -Algorithm SHA256
            ).Hash
            version = $powerShellVersion
        }
        script = [ordered]@{
            path = $runner
            sha256 = (
                Get-FileHash -LiteralPath $runner -Algorithm SHA256
            ).Hash
        }
    }

$cycles = [int]($Iterations / $Terminals.Count)
$schedule = New-KettlePerfWilliamsSchedule -Terminals $Terminals `
    -Seed $ScheduleSeed -Cycles $cycles -Namespace 'throughput'

$specs = [ordered]@{}
$aggregates = [ordered]@{}
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
    $payloads = [ordered]@{}
    foreach ($payload in @('ascii', 'sgr', 'unicode')) {
        $payloads[$payload] = [ordered]@{
            bytes = $null
            sha256 = $null
            runs = 0
            timing_boundary = 'console-write-start-to-DSR-response'
            seconds_all = [Collections.Generic.List[double]]::new()
            write_seconds_all = [Collections.Generic.List[double]]::new()
            drain_ms_all = [Collections.Generic.List[double]]::new()
            drain_misses = 0
            warmup_drain_ms_all = [Collections.Generic.List[double]]::new()
        }
    }
    $aggregates[$terminal] = [ordered]@{
        run_id = $RunId
        terminal = $terminal
        launcher = $spec.Exe
        executable = $spec.BenchmarkExe
        executable_sha256 = $spec.BenchmarkExeSha256
        product_version = Get-KettlePerfVersion $spec
        configuration_mode = $spec.ConfigurationMode
        configuration_evidence = $spec.ConfigurationEvidence
        output_encoding = 'utf-8'
        drain_probe_required = $true
        drain_probe = 'CSI 5 n -> CSI 0 n'
        timing_boundary = 'console-write-start-to-DSR-response'
        requested_samples = $Iterations
        completed_samples = 0
        missed_samples = 0
        schedule_algorithm = $schedule.algorithm
        schedule_seed_sha256 = $schedule.seed_sha256
        workload_runner = $workloadRunner
        observations = [Collections.Generic.List[object]]::new()
        payloads = $payloads
        postflood_ws_mb_all = [Collections.Generic.List[double]]::new()
        workload_pids = [Collections.Generic.List[int]]::new()
        postflood_ws_excluded_pids_all = [Collections.Generic.List[object]]::new()
        helper_binaries = [object[]]@($spec.HelperBinaries)
    }
}

$payloadOrders = @(
    [string[]]@('ascii', 'sgr', 'unicode'),
    [string[]]@('sgr', 'unicode', 'ascii'),
    [string[]]@('unicode', 'ascii', 'sgr')
)

foreach ($round in $schedule.rounds) {
    $payloadOrder = $payloadOrders[
        ([int]$round.round_in_cycle - 1) % $payloadOrders.Count
    ]
    foreach ($visit in $round.visits) {
        $terminal = [string]$visit.terminal
        $spec = $specs[$terminal]
        $goDescriptor = New-KettlePerfGoDescriptor `
            -Directory $ResultsDir

        Write-Host (
            '>> {0} throughput sample {1}/{2} (round {3}, position {4})' -f
            $terminal,
            $visit.sample_id,
            $schedule.sample_count,
            $visit.round,
            $visit.position
        )
        $before = Get-VisibleWindowSet
        $prePids = Get-PidSet
        $launched = $null
        $visitResult = $null
        $memoryAfter = $null
        $goLock = $null
        $channelDescriptor = $null
        try {
            $channelDescriptor =
                New-KettlePerfThroughputChannelDescriptor
            $inner = @(
                $PowerShellExe,
                '-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass',
                '-File', $runner,
                '-Terminal', $terminal,
                '-ResultsDir', $ResultsDir,
                '-PipeName', $channelDescriptor.PipeName,
                '-PipeNonce', $channelDescriptor.Nonce,
                '-GoFile', $goDescriptor.Path,
                '-GoToken', $goDescriptor.Token,
                '-RunId', $RunId,
                '-SampleId', [string]$visit.sample_key,
                '-ScheduleCycle', [string]$visit.cycle,
                '-ScheduleRound', [string]$visit.round,
                '-SchedulePosition', [string]$visit.position,
                '-ScheduleSequence', [string]$visit.sequence,
                '-Iterations', '1',
                '-MinimumIterations', '1',
                '-SettleSeconds', '0',
                '-PayloadOrder'
            ) + [string[]]$payloadOrder
            $launched = Start-KettlePerfCommandWindow -Spec $spec `
                -Command $inner -BeforeWindows $before `
                -PreexistingPids $prePids `
                -CommandWrapperDirectory $ResultsDir
            Set-WindowSize $launched.Hwnd $WindowW $WindowH `
                $TargetScreenDevice
            if (-not (
                Wait-KettlePerfWindowReady `
                    -Hwnd $launched.Hwnd -Width $WindowW -Height $WindowH `
                    -TargetScreenDevice $TargetScreenDevice
            )) {
                throw "$terminal throughput window never reached exact placement"
            }
            [void][KettlePerf.Native]::SetForegroundWindow($launched.Hwnd)
            if (
                [KettlePerf.Native]::GetForegroundWindow() -ne
                    $launched.Hwnd
            ) {
                throw "$terminal throughput window did not retain foreground"
            }
            $goLock = Publish-KettlePerfGoSignal $goDescriptor

            $channelResult = Receive-KettlePerfThroughputChannelJson `
                -Descriptor $channelDescriptor `
                -ExpectedWorkloadPid ([int]$launched.TargetPid) `
                -ExpectedTerminalPid ([int]$launched.WindowPid) `
                -ConnectTimeoutMs ([int]($TimeoutSec * 1000))
            $visitResult = $channelResult.Value
            if (-not $prePids.Contains($launched.WindowPid)) {
                $memoryAfter = Get-ProcessTreeStats `
                    -RootPid $launched.WindowPid `
                    -ExcludeRootPids @($launched.TargetPid)
            }
            if (
                $visitResult.run_id -ne $RunId -or
                $visitResult.terminal -ne $terminal -or
                $visitResult.sample_id -ne $visit.sample_key -or
                [int]$visitResult.schedule.cycle -ne [int]$visit.cycle -or
                [int]$visitResult.schedule.round -ne [int]$visit.round -or
                [int]$visitResult.schedule.position -ne [int]$visit.position -or
                [int]$visitResult.schedule.sequence -ne [int]$visit.sequence -or
                $visitResult.go_handshake -ne
                    'locked-create-new-token-v1'
            ) {
                throw "$terminal throughput sample provenance does not match its schedule"
            }
            Assert-KettlePerfThroughputRunnerEvidence `
                -Actual $visitResult.workload_runner `
                -Expected $workloadRunner -Terminal $terminal
            $consoleCols = 0
            $consoleRows = 0
            if (
                -not [int]::TryParse(
                    [string]$visitResult.cols,
                    [ref]$consoleCols
                ) -or
                -not [int]::TryParse(
                    [string]$visitResult.rows,
                    [ref]$consoleRows
                ) -or
                $consoleCols -le 0 -or
                $consoleRows -le 0 -or
                $consoleCols -gt 10000 -or
                $consoleRows -gt 10000
            ) {
                throw "$terminal reported invalid throughput console geometry"
            }
            if (-not $memoryAfter) {
                throw "$terminal throughput process tree was not attributable"
            }
            if (@($memoryAfter.ExcludedPids) -notcontains $launched.TargetPid) {
                throw "$terminal workload was not excluded from post-flood memory"
            }

            $aggregate = $aggregates[$terminal]
            $aggregate.completed_samples++
            $aggregate.workload_pids.Add([int]$launched.TargetPid)
            $aggregate.postflood_ws_mb_all.Add(
                [double]$memoryAfter.WorkingSetMB
            )
            $aggregate.postflood_ws_excluded_pids_all.Add(
                [int[]]@($memoryAfter.ExcludedPids)
            )
            foreach ($payload in @('ascii', 'sgr', 'unicode')) {
                $samplePayload = $visitResult.payloads.$payload
                if (
                    $null -eq $samplePayload -or
                    [int]$samplePayload.runs -ne 1 -or
                    [int]$samplePayload.drain_misses -ne 0 -or
                    @($samplePayload.seconds_all).Count -ne 1 -or
                    @($samplePayload.write_seconds_all).Count -ne 1 -or
                    @($samplePayload.drain_ms_all).Count -ne 1
                ) {
                    throw "$terminal $payload throughput sample is incomplete"
                }
                $target = $aggregate.payloads[$payload]
                if ($null -eq $target.bytes) {
                    $target.bytes = [int64]$samplePayload.bytes
                    $target.sha256 = [string]$samplePayload.sha256
                } elseif (
                    [int64]$target.bytes -ne [int64]$samplePayload.bytes -or
                    -not [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$target.sha256,
                        [string]$samplePayload.sha256
                    )
                ) {
                    throw "$terminal $payload payload contract changed between rounds"
                }
                $seconds = [double]$samplePayload.seconds_all[0]
                $writeSeconds = [double]$samplePayload.write_seconds_all[0]
                $drainMs = [double]$samplePayload.drain_ms_all[0]
                $target.seconds_all.Add($seconds)
                $target.write_seconds_all.Add($writeSeconds)
                $target.drain_ms_all.Add($drainMs)
                $target.warmup_drain_ms_all.Add(
                    [double]$samplePayload.warmup_drain_ms
                )
                $target.runs++
                $aggregate.observations.Add([pscustomobject][ordered]@{
                    terminal = $terminal
                    payload = $payload
                    metric = 'throughput_mb_per_s'
                    cluster_id = "c$($visit.cycle)-r$($visit.round)"
                    sample_id = [int]$visit.sample_id
                    sample_key = [string]$visit.sample_key
                    cycle = [int]$visit.cycle
                    round = [int]$visit.round
                    round_in_cycle = [int]$visit.round_in_cycle
                    position = [int]$visit.position
                    sequence = [int]$visit.sequence
                    payload_order = [string[]]$payloadOrder
                    client_pixels = [ordered]@{
                        width = $WindowW
                        height = $WindowH
                    }
                    console_cells = [ordered]@{
                        columns = $consoleCols
                        rows = $consoleRows
                    }
                    go_handshake = [string]$visitResult.go_handshake
                    go_wait_ms = [double]$visitResult.go_wait_ms
                    seconds = $seconds
                    write_seconds = $writeSeconds
                    drain_ms = $drainMs
                    value = ([double]$samplePayload.bytes / 1MB) / $seconds
                    postflood_ws_mb = [double]$memoryAfter.WorkingSetMB
                    workload_pid = [int]$launched.TargetPid
                    excluded_pids = [int[]]@($memoryAfter.ExcludedPids)
                    status = 'ok'
                })
            }
        } catch {
            $aggregates[$terminal].missed_samples++
            throw
        } finally {
            Close-KettlePerfGoSignal `
                -Descriptor $goDescriptor -Lock $goLock
            Close-KettlePerfThroughputChannel $channelDescriptor
            if ($null -ne $launched) {
                [void](Close-SpawnedTerminal -Hwnd $launched.Hwnd `
                    -ExpectedPid $launched.WindowPid `
                    -PreexistingPids $prePids)
                try {
                    if (-not $launched.Process.HasExited) {
                        Stop-Process -Id $launched.Process.Id -Force
                    }
                } catch {
                    Write-Verbose (
                        'throughput launcher cleanup raced process exit: ' +
                        $_.Exception.Message
                    )
                }
                if ($null -ne $launched.CommandWrapper) {
                    Close-KettlePerfCommandWrapper $launched.CommandWrapper
                }
                Close-KettlePerfExecutableLease $launched.ExecutableLease
            }
        }
        if ($VisitCooldownSeconds -gt 0) {
            Start-Sleep -Seconds $VisitCooldownSeconds
        }
    }
}

foreach ($terminal in $Terminals) {
    $aggregate = $aggregates[$terminal]
    if (
        $aggregate.completed_samples -ne $Iterations -or
        $aggregate.missed_samples -ne 0
    ) {
        throw "$terminal does not have complete throughput schedule coverage"
    }
    foreach ($payload in @('ascii', 'sgr', 'unicode')) {
        $result = $aggregate.payloads[$payload]
        $secondsMedian = Get-KettlePerfMedian @(
            $result.seconds_all | Sort-Object
        )
        $writeMedian = Get-KettlePerfMedian @(
            $result.write_seconds_all | Sort-Object
        )
        $result.seconds_all = [double[]]$result.seconds_all.ToArray()
        $result.write_seconds_all = [double[]](
            $result.write_seconds_all.ToArray()
        )
        $result.drain_ms_all = [double[]]$result.drain_ms_all.ToArray()
        $result.warmup_drain_ms_all = [double[]](
            $result.warmup_drain_ms_all.ToArray()
        )
        $result.seconds_median = [Math]::Round($secondsMedian, 6)
        $result.mb_per_s_median = [Math]::Round(
            ([double]$result.bytes / 1MB) / $secondsMedian,
            4
        )
        $result.write_seconds_median = [Math]::Round($writeMedian, 6)
        $result.writer_acceptance_mb_per_s_median = [Math]::Round(
            ([double]$result.bytes / 1MB) / $writeMedian,
            4
        )
    }
    $aggregate.postflood_ws_mb = Get-KettlePerfMedian @(
        $aggregate.postflood_ws_mb_all | Sort-Object
    )
    $aggregate.postflood_ws_scope = 'terminal-tree-excluding-workload'
    $aggregate.workload_pid = [int]$aggregate.workload_pids[0]
    $aggregate.postflood_ws_excluded_pids = [int[]]@(
        $aggregate.postflood_ws_excluded_pids_all[0]
    )
    $aggregate.observations = [object[]]$aggregate.observations.ToArray()
    $aggregate.postflood_ws_mb_all = [double[]](
        $aggregate.postflood_ws_mb_all.ToArray()
    )
    $aggregate.workload_pids = [int[]]$aggregate.workload_pids.ToArray()
    $aggregate.postflood_ws_excluded_pids_all = [object[]](
        $aggregate.postflood_ws_excluded_pids_all.ToArray()
    )
    Write-KettlePerfJsonFile `
        -Path (Join-Path $ResultsDir "throughput-$terminal.json") `
        -InputObject $aggregate -Depth 9 -Root $resultsRoot
    Write-Host (
        '{0,-10} throughput samples {1}; ASCII {2:N2}, SGR {3:N2}, Unicode {4:N2} MB/s' -f
        $terminal,
        $aggregate.completed_samples,
        $aggregate.payloads.ascii.mb_per_s_median,
        $aggregate.payloads.sgr.mb_per_s_median,
        $aggregate.payloads.unicode.mb_per_s_median
    )
}

Write-Host "done - results in $ResultsDir"
} finally {
    if ($null -ne $runnerLock) {
        $runnerLock.Dispose()
    }
    if ($null -ne $powerShellLock) {
        $powerShellLock.Dispose()
    }
    Close-KettlePerfPersistenceRoot $resultsRoot
}
