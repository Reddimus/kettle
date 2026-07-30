# Measure the exact interaction visible in the 2026-07-25 Screen Sketch
# recording: moving the pointer between rows of Kettle's open context menu.
#
# The probe opens the menu outside the timed region through Kettle's bounded
# control API, then uses real foreground pointer input and polls PrintWindow
# until the highlight changes. It therefore includes hover event dispatch,
# redraw scheduling, GPU submission, composition, and capture-poll cost. As
# with latency.ps1, the result is comparative/regression evidence rather than a
# photodiode-grade input-to-photon measurement.
param(
    [string]$ResultsDir = '',
    [string]$KettleExe = '',
    [string]$ConfigPath = '',
    [string]$ExtraConfig = '',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.json$')]
    [string]$ResultFileName = 'menu-hover.json',
    [ValidateSet('fixed-comparator', 'native-display')]
    [string]$Variant = 'fixed-comparator',
    [string]$TargetScreenDevice = '',
    [ValidatePattern('^[0-9a-fA-F-]{36}$')]
    [string]$RunId = '',
    [ValidateRange(1, 10000)]
    [int]$Samples = 200,
    [ValidateRange(1, 1000)]
    [int]$BlockSize = 20,
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800,
    [ValidateRange(0.0, 10000.0)]
    [double]$MaxP95Ms = 33.0,
    [ValidateRange(0.0, 10000.0)]
    [double]$MaxP99Ms = 50.0,
    [ValidateRange(0.0, 10000.0)]
    [double]$LongFrameMs = 100.0,
    [ValidateRange(0, 10000)]
    [int]$MaxLongFrames = 1,
    [switch]$NoFail
)
$ErrorActionPreference = 'Stop'
if (($Samples % $BlockSize) -ne 0) {
    throw 'Menu-hover Samples must be divisible by BlockSize'
}
. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"
if (-not $ResultsDir) {
    $ResultsDir = Join-Path $PSScriptRoot '..\..\target\perf-results'
}
if (-not $KettleExe) {
    $KettleExe = Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'
}
if (-not $RunId) {
    $RunId = [Guid]::NewGuid().ToString('D')
}

function Invoke-KettleCtlJson {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [Parameter(Mandatory = $true)][int]$TargetProcessId,
        [Parameter(Mandatory = $true)][string]$Method,
        [string]$Text = ''
    )
    $arguments = @(
        'ctl', '--pid', [string]$TargetProcessId, $Method
    )
    if ($Text) {
        $arguments += @('--text', $Text)
    }
    $arguments += '--raw'
    try {
        $capture = Invoke-KettlePerfBoundedProcess -FilePath $Exe `
            -ArgumentList $arguments -TimeoutMs 2000 `
            -MaxStdoutBytes 1048576 -MaxStderrBytes 65536
    } catch {
        return $null
    }
    if ($capture.ExitCode -ne 0) {
        return $null
    }
    try {
        return ConvertFrom-KettlePerfBoundedJson `
            -Json $capture.StandardOutput -MaximumDepth 32 -MaximumTokens 10000
    } catch {
        return $null
    }
}

function Get-MenuPixelDiff {
    param(
        [byte[]]$Before,
        [byte[]]$After,
        [int]$Width,
        [int]$Height,
        $Rect
    )
    if ($null -eq $Before -or $null -eq $After -or $Before.Length -ne $After.Length) {
        throw 'PrintWindow returned an invalid or differently sized menu capture'
    }
    $x0 = [Math]::Max(0, [int][Math]::Floor([double]$Rect.x))
    $x1 = [Math]::Min($Width, [int][Math]::Ceiling([double]$Rect.x + [double]$Rect.width))
    $y0 = [Math]::Max(0, [int][Math]::Floor([double]$Rect.y))
    $y1 = [Math]::Min($Height, [int][Math]::Ceiling([double]$Rect.y + [double]$Rect.height))
    $diff = 0
    # PrintWindow's DIB is bottom-up. Sample every fourth x and every other y;
    # a row highlight changes hundreds of these samples while static text and
    # the hidden OS cursor contribute none.
    for ($y = $y0; $y -lt $y1; $y += 2) {
        $bufferY = $Height - 1 - $y
        for ($x = $x0; $x -lt $x1; $x += 4) {
            $offset = (($bufferY * $Width) + $x) * 4
            if (
                $Before[$offset] -ne $After[$offset] -or
                $Before[$offset + 1] -ne $After[$offset + 1] -or
                $Before[$offset + 2] -ne $After[$offset + 2]
            ) {
                $diff++
            }
        }
    }
    return $diff
}

function Get-Percentile {
    param([double[]]$Sorted, [double]$Percentile)
    if ($Sorted.Count -eq 0) {
        return $null
    }
    if ([Math]::Abs($Percentile - 0.5) -le [double]::Epsilon) {
        return Get-KettlePerfMedian $Sorted
    }
    $index = [Math]::Min(
        $Sorted.Count - 1,
        [Math]::Max(0, [int][Math]::Ceiling($Sorted.Count * $Percentile) - 1)
    )
    return $Sorted[$index]
}

$spec = Resolve-KettlePerfTerminal -Name kettle -KettleExe $KettleExe
if (-not $spec.Available) {
    throw "Kettle executable not found: $KettleExe"
}
if (-not $spec.HasReliableCli) {
    throw (
        'The menu benchmark requires kettle-console.exe or kettle.com beside ' +
        'kettle.exe so control responses and exit codes are reliable'
    )
}
New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
$resultsRoot = Open-KettlePerfPersistenceRoot -Directory $ResultsDir
    $artifactDir = Join-Path $ResultsDir (
        'menu-hover-' +
        $Variant +
        '-' +
        [Guid]::NewGuid().ToString('N')
    )
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null

if ($ConfigPath) {
    if (-not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
        throw "Config file not found: $ConfigPath"
    }
    $effectiveConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
} else {
    $effectiveConfig = Join-Path $artifactDir 'config'
    $configLines = @(
        'agent-server = full'
        'tab-bar = always'
        'status-bar = off'
        'restore-session = false'
        'update-check = false'
        'record = off'
        'font-size = 13'
        'background = #101010'
        'foreground = #f4f4f4'
        'window-width = 100'
        'window-height = 28'
    )
    if ($ExtraConfig) {
        $configLines += $ExtraConfig
    }
    [IO.File]::WriteAllLines(
        $effectiveConfig,
        $configLines,
        [Text.UTF8Encoding]::new($false)
    )
}

$beforeWindows = Get-VisibleWindowSet
$preexistingPids = Get-PidSet
$preexistingKettle = @(
    Get-Process -Name $spec.WindowProcessNames -ErrorAction SilentlyContinue |
        Where-Object { $preexistingPids.Contains($_.Id) }
)
if ($preexistingKettle.Count -gt 0) {
    throw (
        'Kettle is already running; close it before the isolated menu-hover ' +
        'benchmark'
    )
}
$stdoutLog = Join-Path $artifactDir 'kettle.stdout.log'
$stderrLog = Join-Path $artifactDir 'kettle.stderr.log'
$oldCursor = [KettlePerf.Native+POINT]::new()
$oldCursorCaptured = [KettlePerf.Native]::GetCursorPos([ref]$oldCursor)
$proc = $null
$hwnd = [IntPtr]::Zero
$winPid = 0
$guiLease = $null
$cliLease = $null

try {
    $guiLease = Open-KettlePerfExecutableLease `
        -Executable $spec.BenchmarkExe `
        -ExpectedSha256 $spec.BenchmarkExeSha256
    $cliLease = Open-KettlePerfExecutableLease `
        -Executable $spec.CliExe -ExpectedSha256 $spec.CliExeSha256
    $launchArguments = @(
        '--new-process', '--config', $effectiveConfig, '--agent-server', 'full'
    )
    $proc = Start-Process -FilePath $spec.Exe `
        -ArgumentList (Join-KettlePerfArguments $launchArguments) `
        -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -PassThru
    $hwnd = Wait-NewWindow -Before $beforeWindows `
        -PreexistingPids $preexistingPids -RootPid $proc.Id `
        -ProcessNames $spec.WindowProcessNames `
        -ExpectedExecutable $spec.BenchmarkExe -TimeoutMs 30000
    if ($hwnd -eq [IntPtr]::Zero) {
        throw 'Kettle window never appeared'
    }
    $winPid = Get-WindowPid $hwnd
    Set-WindowSize $hwnd $WindowW $WindowH $TargetScreenDevice
    [void][KettlePerf.Native]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 500
    if ([KettlePerf.Native]::GetForegroundWindow() -ne $hwnd) {
        throw 'Kettle did not take foreground; refusing to inject pointer input'
    }

    $ctlDeadline = (Get-Date).AddSeconds(25)
    $geometry = $null
    while ((Get-Date) -lt $ctlDeadline) {
        if ($proc.HasExited) {
            throw "Kettle exited before its control server became ready (code $($proc.ExitCode))"
        }
        $geometry = Invoke-KettleCtlJson -Exe $spec.CliExe -TargetProcessId $proc.Id -Method ui_geometry
        if ($geometry) {
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $geometry) {
        throw 'Timed out waiting for Kettle control server'
    }

    $content = $geometry.content
    $openX = [int]([double]$content.x + [Math]::Min(100.0, [double]$content.width / 2.0))
    $openY = [int]([double]$content.y + [Math]::Min(100.0, [double]$content.height / 2.0))
    if (-not [KettlePerf.Native]::SetClientCursorPos($hwnd, $openX, $openY)) {
        throw 'Could not position the pointer inside Kettle'
    }
    Start-Sleep -Milliseconds 100
    $opened = Invoke-KettleCtlJson -Exe $spec.CliExe -TargetProcessId $proc.Id `
        -Method perform_action -Text open_context_menu
    if (-not $opened) {
        throw 'Kettle control server could not open the context menu'
    }

    $menuDeadline = (Get-Date).AddSeconds(5)
    $menuGeometry = $null
    while ((Get-Date) -lt $menuDeadline) {
        $geometry = Invoke-KettleCtlJson -Exe $spec.CliExe -TargetProcessId $proc.Id -Method ui_geometry
        if ($geometry -and $geometry.context_menu -and $geometry.context_menu.rows.Count -ge 2) {
            $menuGeometry = $geometry.context_menu
            break
        }
        Start-Sleep -Milliseconds 25
    }
    if (-not $menuGeometry) {
        $cursor = [KettlePerf.Native+POINT]::new()
        [void][KettlePerf.Native]::GetCursorPos([ref]$cursor)
        $lastGeometry = if ($geometry) {
            $geometry | ConvertTo-Json -Depth 8 -Compress
        } else {
            '<none>'
        }
        throw (
            "Context menu did not expose at least two rows; hwnd=$hwnd " +
            "foreground=$([KettlePerf.Native]::GetForegroundWindow()) " +
            "screen_cursor=$($cursor.X),$($cursor.Y) geometry=$lastGeometry"
        )
    }

    $menuBottom = [double]$menuGeometry.rect.y + [double]$menuGeometry.rect.height
    $rows = @($menuGeometry.rows | Where-Object {
        $_.dispatchable -and
        [double]$_.rect.height -gt 0.0 -and
        [double]$_.rect.y -ge [double]$menuGeometry.rect.y -and
        ([double]$_.rect.y + [double]$_.rect.height) -le ($menuBottom + 0.5)
    })
    if ($rows.Count -lt 2) {
        throw 'Context menu has fewer than two dispatchable rows'
    }
    # Widely separated rows make the changed highlight unambiguous and avoid
    # boundary rounding at fractional DPI scales.
    $rowA = $rows[0]
    $rowB = $rows[$rows.Count - 1]
    $pointFor = {
        param($row)
        @(
            [int]([double]$row.rect.x + [double]$row.rect.width / 2.0),
            [int]([double]$row.rect.y + [double]$row.rect.height / 2.0)
        )
    }
    $pointA = & $pointFor $rowA
    $pointB = & $pointFor $rowB
    if (-not [KettlePerf.Native]::SetClientCursorPos($hwnd, $pointA[0], $pointA[1])) {
        throw 'Could not position the pointer on the first menu row'
    }
    Start-Sleep -Milliseconds 250

    $captureX = [Math]::Max(
        0,
        [int][Math]::Floor([double]$menuGeometry.rect.x)
    )
    $captureY = [Math]::Max(
        0,
        [int][Math]::Floor([double]$menuGeometry.rect.y)
    )
    $captureRight = [Math]::Min(
        $WindowW,
        [int][Math]::Ceiling(
            [double]$menuGeometry.rect.x +
            [double]$menuGeometry.rect.width
        )
    )
    $captureBottom = [Math]::Min(
        $WindowH,
        [int][Math]::Ceiling(
            [double]$menuGeometry.rect.y +
            [double]$menuGeometry.rect.height
        )
    )
    $captureWidth = $captureRight - $captureX
    $captureHeight = $captureBottom - $captureY
    if ($captureWidth -le 0 -or $captureHeight -le 0) {
        throw 'Context-menu capture region is outside the terminal client'
    }
    $captureRect = [ordered]@{
        x = 0
        y = 0
        width = $captureWidth
        height = $captureHeight
    }
    $baseline = [KettlePerf.Native]::CaptureWindowRegion(
        $hwnd,
        $captureX,
        $captureY,
        $captureWidth,
        $captureHeight
    )
    if ($null -eq $baseline) {
        throw 'PrintWindow could not capture Kettle'
    }

    # Calibrate only inside the opaque menu panel, where cursor blink and PTY
    # output cannot create noise.
    $noise = 0
    $captureCosts = [Collections.Generic.List[double]]::new()
    for ($i = 0; $i -lt 8; $i++) {
        $captureTimer = [System.Diagnostics.Stopwatch]::StartNew()
        $idle = [KettlePerf.Native]::CaptureWindowRegion(
            $hwnd,
            $captureX,
            $captureY,
            $captureWidth,
            $captureHeight
        )
        $captureTimer.Stop()
        $captureCosts.Add($captureTimer.Elapsed.TotalMilliseconds)
        $noise = [Math]::Max(
            $noise,
            (Get-MenuPixelDiff $baseline $idle $captureWidth $captureHeight $captureRect)
        )
        $baseline = $idle
    }
    $threshold = [Math]::Max(12, $noise * 3)

    $latencies = @()
    $observations = [Collections.Generic.List[object]]::new()
    $misses = 0
    $currentIsA = $true
    for ($i = 0; $i -lt $Samples; $i++) {
        if ([KettlePerf.Native]::GetForegroundWindow() -ne $hwnd) {
            throw 'Kettle lost foreground during the pointer benchmark'
        }
        # Capture the fully settled source row immediately before moving the
        # pointer. Reusing the first threshold-crossing frame would let a
        # multi-frame transition from the previous sample masquerade as an
        # artificially fast response in the next sample.
        $baselineTimer = [Diagnostics.Stopwatch]::StartNew()
        $baseline = [KettlePerf.Native]::CaptureWindowRegion(
            $hwnd,
            $captureX,
            $captureY,
            $captureWidth,
            $captureHeight
        )
        $baselineTimer.Stop()
        $captureCosts.Add($baselineTimer.Elapsed.TotalMilliseconds)
        if ($null -eq $baseline) {
            throw 'PrintWindow could not capture the settled menu state'
        }
        $target = if ($currentIsA) { $pointB } else { $pointA }
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        if (-not [KettlePerf.Native]::SetClientCursorPos($hwnd, $target[0], $target[1])) {
            throw 'Could not move the pointer between menu rows'
        }
        $deadline = (Get-Date).AddMilliseconds(500)
        $hit = $false
        $pollCount = 0
        $pollCaptureMs = 0.0
        while ((Get-Date) -lt $deadline) {
            $captureTimer = [Diagnostics.Stopwatch]::StartNew()
            $capture = [KettlePerf.Native]::CaptureWindowRegion(
                $hwnd,
                $captureX,
                $captureY,
                $captureWidth,
                $captureHeight
            )
            $captureTimer.Stop()
            $pollCount++
            $pollCaptureMs += $captureTimer.Elapsed.TotalMilliseconds
            $captureCosts.Add($captureTimer.Elapsed.TotalMilliseconds)
            if (
                (Get-MenuPixelDiff $baseline $capture $captureWidth $captureHeight $captureRect) `
                    -ge $threshold
            ) {
                $timer.Stop()
                $latencies += [Math]::Round($timer.Elapsed.TotalMilliseconds, 2)
                $hit = $true
                break
            }
        }
        if (-not $hit) {
            $timer.Stop()
            $misses++
            # Re-establish the actual displayed state before the next sample.
            Start-Sleep -Milliseconds 100
            $baseline = [KettlePerf.Native]::CaptureWindowRegion(
                $hwnd,
                $captureX,
                $captureY,
                $captureWidth,
                $captureHeight
            )
        }
        $sampleLatency = if ($hit) {
            [double]$latencies[$latencies.Count - 1]
        } else {
            $null
        }
        $observations.Add([pscustomobject][ordered]@{
            terminal = 'kettle'
            metric = 'menu_hover_ms'
            block_id = 1 + [int][Math]::Floor($i / $BlockSize)
            sample_id = $i + 1
            sequence = $i + 1
            source_row = if ($currentIsA) { 'first' } else { 'last' }
            target_row = if ($currentIsA) { 'last' } else { 'first' }
            value = $sampleLatency
            status = if ($hit) { 'ok' } else { 'censored-timeout' }
            timeout_ms = 500
            baseline_capture_ms = $baselineTimer.Elapsed.TotalMilliseconds
            poll_count = $pollCount
            poll_capture_ms = [Math]::Round($pollCaptureMs, 3)
        })
        $currentIsA = -not $currentIsA
        Start-Sleep -Milliseconds 40
    }

    $sorted = @($latencies | Sort-Object)
    if ($sorted.Count -eq 0) {
        throw 'No menu-highlight transitions were observed'
    }
    $p50 = Get-Percentile $sorted 0.50
    $p95 = Get-Percentile $sorted 0.95
    $p99 = Get-Percentile $sorted 0.99
    $longFrames = @($latencies | Where-Object { $_ -gt $LongFrameMs }).Count
    $passed = (
        $misses -eq 0 -and
        $p95 -le $MaxP95Ms -and
        $p99 -le $MaxP99Ms -and
        $longFrames -le $MaxLongFrames
    )
    $version = Get-KettlePerfVersion $spec
    $gpuCapture = Invoke-KettlePerfBoundedProcess -FilePath $spec.CliExe `
        -ArgumentList @('--config', $effectiveConfig, '--gpu-info') `
        -TimeoutMs 10000 -MaxStdoutBytes 1048576 -MaxStderrBytes 1048576
    if ($gpuCapture.ExitCode -ne 0) {
        throw "Kettle --gpu-info failed with exit code $($gpuCapture.ExitCode)"
    }
    $gpuInfo = @(
        $gpuCapture.StandardOutput,
        $gpuCapture.StandardError
    ) -join "`n"
    $refreshRates = @(Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
        ForEach-Object { $_.CurrentRefreshRate } |
        Where-Object { $_ -gt 0 })
    $result = [ordered]@{
        run_id = $RunId
        timestamp = (Get-Date).ToString('o')
        variant = $Variant
        kettle_version = $version
        launcher = $spec.Exe
        executable = $spec.BenchmarkExe
        executable_sha256 = $spec.BenchmarkExeSha256
        helper_binaries = [object[]]@($spec.HelperBinaries)
        config = $effectiveConfig
        config_sha256 = (
            Get-FileHash -LiteralPath $effectiveConfig -Algorithm SHA256
        ).Hash
        target_screen_device = $TargetScreenDevice
        gpu_info = $gpuInfo
        display_refresh_hz = if ($refreshRates.Count) {
            ($refreshRates | Measure-Object -Maximum).Maximum
        } else {
            $null
        }
        window_pixels = @{ width = $WindowW; height = $WindowH }
        capture_region = @{
            x = $captureX
            y = $captureY
            width = $captureWidth
            height = $captureHeight
        }
        capture_scope = 'context-menu-roi'
        requested_samples = $Samples
        block_size = $BlockSize
        block_count = [int]($Samples / $BlockSize)
        samples = $latencies.Count
        misses = $misses
        latency_ms_all = $latencies
        observations = [object[]]$observations.ToArray()
        latency_ms_p50 = $p50
        latency_ms_p95 = $p95
        latency_ms_p99 = $p99
        latency_ms_max = ($sorted | Measure-Object -Maximum).Maximum
        long_frame_ms = $LongFrameMs
        long_frames = $longFrames
        capture_ms_median = Get-Percentile @($captureCosts | Sort-Object) 0.50
        capture_ms_p95 = Get-Percentile @($captureCosts | Sort-Object) 0.95
        observation_boundary = 'pointer-move-to-PrintWindow-menu-ROI-change'
        observation_limit = 'comparative-software-capture-not-input-to-photon'
        noise_floor = $noise
        pixel_threshold = $threshold
        gates = @{
            max_p95_ms = $MaxP95Ms
            max_p99_ms = $MaxP99Ms
            max_long_frames = $MaxLongFrames
        }
        passed = $passed
        artifacts = $artifactDir
    }
    Write-KettlePerfJsonFile `
        -Path (Join-Path $ResultsDir $ResultFileName) `
        -InputObject $result -Depth 8 -Root $resultsRoot
    Write-Host (
        "menu hover: p50={0:N2} ms p95={1:N2} ms p99={2:N2} ms max={3:N2} ms misses={4} long>{5}ms={6}" -f
        $p50, $p95, $p99, $result.latency_ms_max, $misses, $LongFrameMs, $longFrames
    )
    if (-not $passed -and -not $NoFail) {
        exit 1
    }
} finally {
    if ($hwnd -ne [IntPtr]::Zero) {
        [void](Close-SpawnedTerminal -Hwnd $hwnd `
            -ExpectedPid $winPid `
            -PreexistingPids $preexistingPids)
    }
    if ($proc -and -not $proc.HasExited) {
        try {
            Stop-Process -Id $proc.Id -Force
        } catch {
            Write-Verbose "menu benchmark cleanup raced process exit: $($_.Exception.Message)"
        }
    }
    if ($oldCursorCaptured) {
        [void][KettlePerf.Native]::SetCursorPos($oldCursor.X, $oldCursor.Y)
    }
    Close-KettlePerfExecutableLease $cliLease
    Close-KettlePerfExecutableLease $guiLease
    Close-KettlePerfPersistenceRoot $resultsRoot
}
