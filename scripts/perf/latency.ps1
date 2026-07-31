# Comparative input-to-capturable-client latency probe.
# SendInput targets the verified foreground terminal and PrintWindow polls a
# fixed 1280x800-style client until pixels change beyond a per-block blink-noise
# envelope. These are comparative software-observation times, not input-to-
# photon latency. Blocks are Williams-balanced to control thermal/order drift.
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
    [string]$TargetScreenDevice = '',
    [ValidatePattern('^[0-9a-fA-F-]{36}$')]
    [string]$RunId = '',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$')]
    [string]$ScheduleSeed = 'kettle-latency-release-v1',
    [ValidateRange(6, 10000)]
    [int]$Samples = 60,
    [ValidateRange(1, 1000)]
    [int]$BlockSize = 10,
    [ValidateRange(0, 1000)]
    [int]$MaxCensored = 3,
    [ValidateRange(100, 10000)]
    [int]$SampleTimeoutMs = 800,
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800,
    [ValidateRange(0, 600)]
    [int]$BlockCooldownSeconds = 2
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\schedule.ps1"

if (
    $Terminals.Count -lt 6 -or
    ($Terminals.Count % 2) -ne 0 -or
    ($Samples % $BlockSize) -ne 0
) {
    throw (
        'Latency interleaving requires an even set of at least six terminals ' +
        'and Samples divisible by BlockSize'
    )
}
$blocksPerTerminal = [int]($Samples / $BlockSize)
if (($blocksPerTerminal % $Terminals.Count) -ne 0) {
    throw (
        'Latency blocks per terminal must be divisible by the terminal count ' +
        'for complete Williams-balanced cycles'
    )
}
if (-not $ResultsDir) {
    $ResultsDir = Join-Path $PSScriptRoot '..\..\target\perf-results'
}
if (-not $KettleExe) {
    $KettleExe = Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'
}
if (-not $RunId) {
    $RunId = [Guid]::NewGuid().ToString('D')
}
New-Item -ItemType Directory -Force $ResultsDir | Out-Null
$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
$resultsRoot = Open-KettlePerfPersistenceRoot -Directory $ResultsDir

function Get-KettlePerfDiffCount {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Left,
        [Parameter(Mandatory)]
        [byte[]]$Right
    )

    if (
        $Left.Length -ne $Right.Length -or
        $Left.Length -lt 4 -or
        ($Left.Length % 4) -ne 0
    ) {
        throw 'PrintWindow returned an invalid or differently sized BGRA capture'
    }
    $difference = 0
    # Sample every 16th BGRA pixel. This is dense enough to detect one glyph
    # while bounding probe overhead for the fixed comparator client size.
    for ($index = 0; $index -lt $Left.Length; $index += 64) {
        if (
            $Left[$index] -ne $Right[$index] -or
            $Left[$index + 1] -ne $Right[$index + 1] -or
            $Left[$index + 2] -ne $Right[$index + 2]
        ) {
            $difference++
        }
    }
    return $difference
}

function Get-KettlePerfTimedCapture {
    param(
        [Parameter(Mandatory)]
        [IntPtr]$Hwnd
    )

    $width = 0
    $height = 0
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $bytes = [KettlePerf.Native]::CaptureWindow(
        $Hwnd,
        [ref]$width,
        [ref]$height
    )
    $timer.Stop()
    if ($null -eq $bytes -or $width -le 0 -or $height -le 0) {
        throw 'PrintWindow did not return a capturable terminal client'
    }
    return [pscustomobject]@{
        bytes = $bytes
        width = $width
        height = $height
        elapsed_ms = $timer.Elapsed.TotalMilliseconds
    }
}

$latencyShellCommand = Get-Command cmd.exe -CommandType Application `
    -ErrorAction Stop |
    Select-Object -First 1
$latencyShell = (
    Resolve-Path -LiteralPath $latencyShellCommand.Source -ErrorAction Stop
).Path
$latencyCommand = @($latencyShell, '/d', '/q', '/k', 'prompt $G')
$latencyShellSha256 = Get-KettlePerfExecutableSha256 $latencyShell
$cycles = [int]($blocksPerTerminal / $Terminals.Count)
$schedule = New-KettlePerfWilliamsSchedule -Terminals $Terminals `
    -Seed $ScheduleSeed -Cycles $cycles -Namespace 'latency'

$specs = [ordered]@{}
$all = [ordered]@{}
$censorFailures = [Collections.Generic.List[string]]::new()
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
        workload_command = Join-KettlePerfArguments $latencyCommand
        workload_executable = $latencyShell
        workload_executable_sha256 = $latencyShellSha256
        command_confirmation = $spec.CommandConfirmation
        helper_binaries = [object[]]@($spec.HelperBinaries)
        schedule_algorithm = $schedule.algorithm
        schedule_seed_sha256 = $schedule.seed_sha256
        block_size = $BlockSize
        blocks = $blocksPerTerminal
        requested_samples = $Samples
        samples = 0
        misses = 0
        latency_ms_all = [Collections.Generic.List[double]]::new()
        capture_ms_all = [Collections.Generic.List[double]]::new()
        workload_pids = [Collections.Generic.List[int]]::new()
        observations = [Collections.Generic.List[object]]::new()
        window_pixels = [ordered]@{ width = $WindowW; height = $WindowH }
        observation_boundary = 'SendInput-to-PrintWindow-pixel-change'
        observation_limit = 'comparative-software-capture-not-input-to-photon'
        timeout_ms = $SampleTimeoutMs
    }
}

foreach ($round in $schedule.rounds) {
    foreach ($visit in $round.visits) {
        $terminal = [string]$visit.terminal
        $spec = $specs[$terminal]
        Write-Host (
            '>> {0} latency block {1}/{2} (round {3}, position {4})' -f
            $terminal,
            $visit.sample_id,
            $schedule.sample_count,
            $visit.round,
            $visit.position
        )
        $before = Get-VisibleWindowSet
        $prePids = Get-PidSet
        $launched = $null
        try {
            $launched = Start-KettlePerfCommandWindow -Spec $spec `
                -Command $latencyCommand -BeforeWindows $before `
                -PreexistingPids $prePids `
                -CommandWrapperDirectory $ResultsDir
            if (-not (
                Wait-KettlePerfWindowReady `
                    -Hwnd $launched.Hwnd -Width $WindowW -Height $WindowH `
                    -TargetScreenDevice $TargetScreenDevice
            )) {
                throw "$terminal latency block never produced an exact-size capture"
            }
            [void][KettlePerf.Native]::SetForegroundWindow($launched.Hwnd)
            Start-Sleep -Seconds 3
            if ([KettlePerf.Native]::GetForegroundWindow() -ne $launched.Hwnd) {
                throw "$terminal latency window did not retain foreground"
            }
            $all[$terminal].workload_pids.Add([int]$launched.TargetPid)

            $baseline = Get-KettlePerfTimedCapture $launched.Hwnd
            if (
                $baseline.width -ne $WindowW -or
                $baseline.height -ne $WindowH
            ) {
                throw "$terminal latency capture dimensions drifted"
            }
            $noise = 0
            $calibrationCaptureMs = [Collections.Generic.List[double]]::new()
            $calibrationEnd = (Get-Date).AddMilliseconds(1500)
            while ((Get-Date) -lt $calibrationEnd) {
                $capture = Get-KettlePerfTimedCapture $launched.Hwnd
                $calibrationCaptureMs.Add([double]$capture.elapsed_ms)
                $difference = Get-KettlePerfDiffCount `
                    -Left $baseline.bytes -Right $capture.bytes
                if ($difference -gt $noise) {
                    $noise = $difference
                }
            }
            $threshold = $noise + [Math]::Max(
                5,
                [int][Math]::Ceiling($noise * 0.15)
            )

            for ($sampleInBlock = 1; $sampleInBlock -le $BlockSize; $sampleInBlock++) {
                $fresh = Get-KettlePerfTimedCapture $launched.Hwnd
                $all[$terminal].capture_ms_all.Add(
                    [double]$fresh.elapsed_ms
                )
                if ([KettlePerf.Native]::GetForegroundWindow() -ne $launched.Hwnd) {
                    throw "$terminal foreground changed before latency input"
                }
                $timer = [Diagnostics.Stopwatch]::StartNew()
                if (-not [KettlePerf.Native]::SendChar([char]'m')) {
                    throw "$terminal SendInput could not inject the latency key"
                }
                $hit = $false
                $pollCount = 0
                $pollCaptureMs = 0.0
                while ($timer.ElapsedMilliseconds -lt $SampleTimeoutMs) {
                    $capture = Get-KettlePerfTimedCapture $launched.Hwnd
                    $pollCount++
                    $pollCaptureMs += [double]$capture.elapsed_ms
                    $all[$terminal].capture_ms_all.Add(
                        [double]$capture.elapsed_ms
                    )
                    if (
                        (Get-KettlePerfDiffCount `
                            -Left $fresh.bytes -Right $capture.bytes) -ge
                            $threshold
                    ) {
                        $timer.Stop()
                        $hit = $true
                        break
                    }
                }
                if (-not $hit) {
                    $timer.Stop()
                }
                $globalSample = (
                    (([int]$visit.round - 1) * $BlockSize) +
                    $sampleInBlock
                )
                $status = if ($hit) { 'ok' } else { 'censored-timeout' }
                $value = if ($hit) {
                    [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
                } else {
                    $null
                }
                if ($hit) {
                    $all[$terminal].samples++
                    $all[$terminal].latency_ms_all.Add([double]$value)
                } else {
                    $all[$terminal].misses++
                }
                $all[$terminal].observations.Add([pscustomobject][ordered]@{
                    terminal = $terminal
                    metric = 'latency_ms'
                    cluster_id = "c$($visit.cycle)-r$($visit.round)"
                    block_id = [string]$visit.sample_key
                    sample_id = [int]$visit.sample_id
                    sample_in_block = $sampleInBlock
                    terminal_sample = $globalSample
                    cycle = [int]$visit.cycle
                    round = [int]$visit.round
                    round_in_cycle = [int]$visit.round_in_cycle
                    position = [int]$visit.position
                    sequence = [int]$visit.sequence
                    value = $value
                    status = $status
                    timeout_ms = $SampleTimeoutMs
                    noise_floor = $noise
                    threshold = $threshold
                    baseline_capture_ms = [double]$fresh.elapsed_ms
                    poll_count = $pollCount
                    poll_capture_ms = [Math]::Round($pollCaptureMs, 3)
                    calibration_capture_count = $calibrationCaptureMs.Count
                })
                if ([KettlePerf.Native]::GetForegroundWindow() -ne $launched.Hwnd) {
                    throw "$terminal foreground changed before cleanup input"
                }
                if (-not [KettlePerf.Native]::SendVk(0x08)) {
                    throw "$terminal SendInput could not inject cleanup backspace"
                }
                Start-Sleep -Milliseconds 100
            }
        } finally {
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
                        'latency launcher cleanup raced process exit: ' +
                        $_.Exception.Message
                    )
                }
                if ($null -ne $launched.CommandWrapper) {
                    Close-KettlePerfCommandWrapper $launched.CommandWrapper
                }
                Close-KettlePerfExecutableLease $launched.ExecutableLease
            }
        }
        if ($BlockCooldownSeconds -gt 0) {
            Start-Sleep -Seconds $BlockCooldownSeconds
        }
    }
}

foreach ($terminal in $Terminals) {
    $result = $all[$terminal]
    if (
        $result.samples + $result.misses -ne $Samples -or
        $result.observations.Count -ne $Samples
    ) {
        throw "$terminal latency schedule coverage is incomplete"
    }
    $sorted = @($result.latency_ms_all | Sort-Object)
    $captureSorted = @($result.capture_ms_all | Sort-Object)
    $result.latency_ms_all = [double[]]$result.latency_ms_all.ToArray()
    $result.capture_ms_all = [double[]]$result.capture_ms_all.ToArray()
    $result.workload_pids = [int[]]$result.workload_pids.ToArray()
    $result.observations = [object[]]$result.observations.ToArray()
    $result.latency_ms_median = if ($sorted.Count) {
        Get-KettlePerfMedian $sorted
    } else {
        $null
    }
    $result.latency_ms_p90 = if ($sorted.Count) {
        Get-KettlePerfNearestRankPercentile -Values $sorted -Percentile 90
    } else {
        $null
    }
    $result.latency_ms_p95 = if ($sorted.Count) {
        Get-KettlePerfNearestRankPercentile -Values $sorted -Percentile 95
    } else {
        $null
    }
    $result.latency_ms_p99 = if ($sorted.Count) {
        Get-KettlePerfNearestRankPercentile -Values $sorted -Percentile 99
    } else {
        $null
    }
    $result.capture_ms_median = Get-KettlePerfMedian $captureSorted
    $result.capture_ms_p95 = Get-KettlePerfNearestRankPercentile `
        -Values $captureSorted -Percentile 95
    if ($result.misses -gt $MaxCensored) {
        $censorFailures.Add((
            "$terminal produced $($result.misses) censored latency samples; " +
            "the release maximum is $MaxCensored"
        ))
    }
    Write-Host (
        '{0,-10} latency median {1,7:N2} ms; p95 {2,7:N2} ms; censored {3}/{4}' -f
        $terminal,
        $result.latency_ms_median,
        $result.latency_ms_p95,
        $result.misses,
        $Samples
    )
}

Write-KettlePerfJsonFile `
    -Path (Join-Path $ResultsDir 'latency.json') `
    -InputObject $all -Depth 9 -Root $resultsRoot
Close-KettlePerfPersistenceRoot $resultsRoot
if ($censorFailures.Count -gt 0) {
    throw ($censorFailures -join '; ')
}
Write-Host "done - results in $ResultsDir"
