# Throughput runner - executes INSIDE the terminal under test.
# Writes fixed payloads to the live console and measures from the first write
# through the terminal's CSI 5n -> CSI 0n response. That boundary establishes
# writer acceptance plus parser round-trip drain, not compositor presentation;
# writer-only timings remain diagnostic.
# Orchestrated samples use one authenticated local named-pipe message, so the
# orchestrator never has to scrape the screen or reopen a path-racy sample file.
param(
    [Parameter(Mandatory)]
    [ValidateSet('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')]
    [string]$Terminal,
    [Parameter(Mandatory)] [string]$ResultsDir,
    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-fA-F-]{36}$')]
    [string]$RunId,
    [string]$PayloadDir = '',
    [string]$ResultFile = '',
    [ValidatePattern(
        '^kettle-perf-throughput-[0-9a-f]{48}$'
    )]
    [string]$PipeName = '',
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$PipeNonce = '',
    [string]$GoFile = '',
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$GoToken = '',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')]
    [string]$SampleId = 'standalone',
    [ValidateRange(0, 1000)]
    [int]$ScheduleCycle = 0,
    [ValidateRange(0, 1000)]
    [int]$ScheduleRound = 0,
    [ValidateRange(0, 1000)]
    [int]$SchedulePosition = 0,
    [ValidateRange(0, 1000000)]
    [int]$ScheduleSequence = 0,
    [ValidateCount(3, 3)]
    [ValidateSet('ascii', 'sgr', 'unicode')]
    [string[]]$PayloadOrder = @('ascii', 'sgr', 'unicode'),
    [ValidateRange(1, 1000)]
    [int]$Iterations = 5,
    [ValidateRange(1, 1000)]
    [int]$MinimumIterations = 3,
    [ValidateRange(0, 3600)]
    [int]$SettleSeconds = 3,
    [ValidateRange(100, 60000)]
    [int]$DrainTimeoutMs = 10000,
    [ValidateRange(0, 5000)]
    [int]$RenderSettleMs = 100,
    [switch]$SkipDrainProbe
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\evidence-snapshot.ps1"
. "$PSScriptRoot\payload-contract.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\statistics.ps1"
. "$PSScriptRoot\go-signal.ps1"
. "$PSScriptRoot\throughput-channel.ps1"
if (-not $PayloadDir) {
    $PayloadDir = Join-Path $PSScriptRoot '..\..\target\perf-payloads'
}
if ($MinimumIterations -gt $Iterations) {
    throw 'MinimumIterations cannot exceed Iterations'
}
New-Item -ItemType Directory -Force $ResultsDir | Out-Null
$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
if ([bool]$PipeName -ne [bool]$PipeNonce) {
    throw 'PipeName and PipeNonce must be provided together'
}
if ($PipeName) {
    if ($ResultFile) {
        throw 'ResultFile cannot be combined with the throughput pipe'
    }
    $PipeName = Assert-KettlePerfThroughputChannelName $PipeName
    $PipeNonce = Assert-KettlePerfThroughputChannelNonce $PipeNonce
} else {
    if (-not $ResultFile) {
        $ResultFile = Join-Path $ResultsDir "throughput-$Terminal.json"
    }
    $resultParent = [IO.Path]::GetDirectoryName(
        [IO.Path]::GetFullPath($ResultFile)
    )
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $resultParent.TrimEnd([char[]]@('\', '/')),
        $ResultsDir.TrimEnd([char[]]@('\', '/'))
    )) {
        throw 'Throughput result file must be a direct child of ResultsDir'
    }
    $ResultFile = [IO.Path]::GetFullPath($ResultFile)
}
if (@($PayloadOrder | Select-Object -Unique).Count -ne 3) {
    throw 'PayloadOrder must contain each throughput payload exactly once'
}
if ([bool]$GoFile -ne [bool]$GoToken) {
    throw 'GoFile and GoToken must be provided together'
}
if ($GoFile) {
    $GoFile = Assert-KettlePerfGoPath `
        -Path $GoFile -Directory $ResultsDir
}

$currentPowerShell = (Get-Process -Id $PID -ErrorAction Stop).Path
if (-not $currentPowerShell) {
    throw 'Could not identify the throughput workload PowerShell executable'
}
$currentPowerShell = (Resolve-Path -LiteralPath $currentPowerShell).Path
$currentScript = (Resolve-Path -LiteralPath $PSCommandPath).Path
$workloadRunner = [ordered]@{
    schema = 'kettle-throughput-runner-v1'
    powershell = [ordered]@{
        path = $currentPowerShell
        sha256 = (
            Get-FileHash -LiteralPath $currentPowerShell -Algorithm SHA256
        ).Hash
        version = $PSVersionTable.PSVersion.ToString()
    }
    script = [ordered]@{
        path = $currentScript
        sha256 = (
            Get-FileHash -LiteralPath $currentScript -Algorithm SHA256
        ).Hash
    }
}

# Windows PowerShell inherits the active console code page, which may be an OEM
# encoding such as IBM437. That silently replaces CJK and emoji and turns the
# Unicode workload into a different byte stream. Use strict, BOM-free UTF-8 for
# every terminal so the pinned source bytes survive the console write path.
$utf8 = [Text.UTF8Encoding]::new($false, $true)
[Console]::OutputEncoding = $utf8
$out = [Console]::Out
if ($out.Encoding.WebName -ne 'utf-8') {
    throw "Could not configure UTF-8 console output (got $($out.Encoding.WebName))"
}

$payloadSet = Open-KettlePerfPayloadSet -PayloadDirectory $PayloadDir
try {
$goWaitMs = 0.0
if ($GoFile) {
    $goWaitMs = Wait-KettlePerfGoSignal `
        -Path $GoFile -Directory $ResultsDir -Token $GoToken `
        -TimeoutSeconds 30
} elseif ($SettleSeconds -gt 0) {
    # Standalone diagnostic mode has no parent orchestrator.
    Start-Sleep -Seconds $SettleSeconds
}

function Wait-KettlePerfTerminalDrain {
    if ($SkipDrainProbe) {
        return 0.0
    }
    try {
        if ([Console]::KeyAvailable) {
            throw 'console input was pending before the terminal drain probe'
        }
    } catch {
        throw "terminal drain probe cannot read console input: $($_.Exception.Message)"
    }
    $probeTimer = [Diagnostics.Stopwatch]::StartNew()
    $out.Write("`e[5n")
    $out.Flush()
    $response = [Text.StringBuilder]::new()
    while ($probeTimer.ElapsedMilliseconds -lt $DrainTimeoutMs) {
        if ([Console]::KeyAvailable) {
            $character = [Console]::ReadKey($true).KeyChar
            [void]$response.Append($character)
            if ($response.ToString().Contains("`e[0n")) {
                $probeTimer.Stop()
                if ($RenderSettleMs -gt 0) {
                    Start-Sleep -Milliseconds $RenderSettleMs
                }
                return $probeTimer.Elapsed.TotalMilliseconds
            }
            if ($response.Length -gt 128) {
                [void]$response.Remove(0, $response.Length - 16)
            }
        } else {
            Start-Sleep -Milliseconds 1
        }
    }
    return $null
}

$results = [ordered]@{
    run_id = $RunId
    terminal = $Terminal
    timestamp = (Get-Date).ToString('o')
    sample_id = $SampleId
    schedule = [ordered]@{
        cycle = $ScheduleCycle
        round = $ScheduleRound
        position = $SchedulePosition
        sequence = $ScheduleSequence
    }
    cols = $Host.UI.RawUI.WindowSize.Width
    rows = $Host.UI.RawUI.WindowSize.Height
    iterations = $Iterations
    minimum_iterations = $MinimumIterations
    output_encoding = $out.Encoding.WebName
    go_handshake = if ($GoFile) {
        'locked-create-new-token-v1'
    } else {
        'standalone-settle'
    }
    go_wait_ms = $goWaitMs
    drain_probe_required = -not [bool]$SkipDrainProbe
    drain_probe = 'CSI 5 n -> CSI 0 n'
    drain_timeout_ms = $DrainTimeoutMs
    render_settle_ms = $RenderSettleMs
    workload_runner = $workloadRunner
    payloads = [ordered]@{}
}

foreach ($name in $PayloadOrder) {
    $contract = $KettlePerfPayloadContracts[$name]
    $payloadEntry = Read-KettlePerfPayloadEntry `
        -PayloadSet $payloadSet -Name $name
    $bytes = $contract.bytes
    $sha256 = $contract.sha256

    # Pre-split into 32 KiB chunks OUTSIDE the timed region so allocation noise
    # doesn't pollute the measurement; the timed loop is pure console writes.
    $chunkSize = 32768
    $chunks = [System.Collections.Generic.List[string]]::new()
    $text = $null
    try {
        $text = [string]$payloadEntry.text
        for ($off = 0; $off -lt $text.Length; $off += $chunkSize) {
            $len = [Math]::Min($chunkSize, $text.Length - $off)
            $chunks.Add($text.Substring($off, $len))
        }
    } finally {
        $text = $null
        Release-KettlePerfPayloadEntry `
            -PayloadSet $payloadSet -Name $name
    }

    # Warmup (1/8 of the payload) primes glyph atlases and scrollback paths.
    foreach ($c in $chunks[0..([Math]::Max(0, [int]($chunks.Count / 8)))]) { $out.Write($c) }
    $out.Flush()
    $warmupDrainMs = Wait-KettlePerfTerminalDrain
    if ($null -eq $warmupDrainMs) {
        throw "$Terminal did not answer the post-warmup terminal drain probe"
    }

    # The declared repetition count is fixed. Adaptive stopping would give
    # slower terminals fewer observations and make run count depend on an
    # interim result, which invalidates paired/interleaved comparisons.
    $writeTimes = @()
    $endToDrainTimes = @()
    $drainTimes = @()
    $drainMisses = 0
    for ($i = 0; $i -lt $Iterations; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        foreach ($c in $chunks) { $out.Write($c) }
        $out.Flush()
        $sw.Stop()
        $writeSeconds = $sw.Elapsed.TotalSeconds
        $writeTimes += $writeSeconds
        $drainMs = Wait-KettlePerfTerminalDrain
        if ($null -eq $drainMs) {
            $drainMisses++
            throw "$Terminal did not answer the post-payload terminal drain probe"
        }
        $drainTimes += $drainMs
        $endToDrainTimes += $writeSeconds + ($drainMs / 1000.0)
    }
    $sorted = @($endToDrainTimes | Sort-Object)
    $median = Get-KettlePerfMedian $sorted
    $writeMedian = Get-KettlePerfMedian @($writeTimes | Sort-Object)
    $results.payloads[$name] = [ordered]@{
        bytes = $bytes
        sha256 = $sha256
        runs = $endToDrainTimes.Count
        timing_boundary = 'console-write-start-to-DSR-response'
        seconds_all = $endToDrainTimes
        seconds_median = [Math]::Round($median, 3)
        mb_per_s_median = [Math]::Round(($bytes / 1MB) / $median, 2)
        write_seconds_all = $writeTimes
        write_seconds_median = [Math]::Round($writeMedian, 3)
        writer_acceptance_mb_per_s_median = [Math]::Round(
            ($bytes / 1MB) / $writeMedian,
            2
        )
        warmup_drain_ms = $warmupDrainMs
        drain_ms_all = $drainTimes
        drain_misses = $drainMisses
    }
}

if ($PipeName) {
    Send-KettlePerfThroughputChannelJson `
        -PipeName $PipeName -Nonce $PipeNonce `
        -InputObject $results -Depth 6
} else {
    Write-KettlePerfJsonFile `
        -Path $ResultFile -InputObject $results -Depth 6
}
$out.WriteLine("")
$out.WriteLine("DONE $Terminal - results delivered.")
Start-Sleep -Seconds 1
} finally {
    Close-KettlePerfPayloadSet -PayloadSet $payloadSet
}
