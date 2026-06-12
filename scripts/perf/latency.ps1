# Input-latency probe: SendInput a printable key to the focused terminal, then
# poll PrintWindow(PW_RENDERFULLCONTENT) captures until the client pixels change
# beyond the cursor-blink noise floor (auto-calibrated per terminal beforehand).
# Resolution is bounded by capture cost (~5-15 ms on a 1280x800 window), so the
# numbers are COMPARATIVE between terminals captured the same way, not absolute
# input-to-photon latency.
param(
    [string[]]$Terminals = @('kettle', 'wt', 'alacritty', 'wezterm'),
    [string]$ResultsDir = "$PSScriptRoot\..\..\target\perf-results",
    [string]$KettleExe = "$PSScriptRoot\..\..\target\release\kettle.exe",
    [string]$AlacrittyExe = 'C:\Users\kevm9\Repos\research\bin\alacritty.exe',
    [string]$WeztermExe = 'C:\Users\kevm9\Repos\research\bin\wezterm\wezterm-gui.exe',
    [int]$Samples = 20,
    [int]$WindowW = 1280,
    [int]$WindowH = 800
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
New-Item -ItemType Directory -Force $ResultsDir | Out-Null

function Get-DiffCount([byte[]]$a, [byte[]]$b) {
    if ($null -eq $a -or $null -eq $b -or $a.Length -ne $b.Length) { return [int]::MaxValue }
    $diff = 0
    # Sample every 16th pixel (64-byte stride) — plenty to separate a glyph from noise.
    for ($i = 0; $i -lt $a.Length; $i += 64) {
        if ($a[$i] -ne $b[$i] -or $a[$i + 1] -ne $b[$i + 1] -or $a[$i + 2] -ne $b[$i + 2]) { $diff++ }
    }
    $diff
}

$all = [ordered]@{}
foreach ($t in $Terminals) {
    switch ($t) {
        'kettle'    { $exe = $KettleExe;    $args = @() }
        'wt'        { $exe = 'wt.exe';      $args = @() }
        'alacritty' { $exe = $AlacrittyExe; $args = @() }
        'wezterm'   { $exe = $WeztermExe;   $args = @('start') }
    }
    if (-not (Get-Command $exe -ErrorAction SilentlyContinue) -and -not (Test-Path $exe)) {
        Write-Warning "$t executable not found — skipping"; continue
    }

    Write-Host ">> $t : spawning for latency probe"
    $before = Get-VisibleWindowSet
    $prePids = Get-PidSet
    if ($args.Count -gt 0) { $proc = Start-Process -FilePath $exe -ArgumentList $args -PassThru }
    else { $proc = Start-Process -FilePath $exe -PassThru }
    $hwnd = Wait-NewWindow -Before $before
    if ($hwnd -eq [IntPtr]::Zero) { Write-Warning "$t window never appeared"; continue }
    Start-Sleep -Milliseconds 600
    Set-WindowSize $hwnd $WindowW $WindowH
    [void][KettlePerf.Native]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 300
    # SendInput types into the FOREGROUND window, whatever that is. A
    # background harness is not allowed to steal foreground on Windows, so
    # if the spawned terminal didn't actually take focus the keystrokes
    # would land in the user's active window — refuse instead. This makes
    # the probe interactive-session-only by design.
    if ([KettlePerf.Native]::GetForegroundWindow() -ne $hwnd) {
        Write-Warning "$t : window did not take foreground (background harness?) — skipping (run from an interactive session)"
        [void](Close-SpawnedTerminal -Hwnd $hwnd -PreexistingPids $prePids)
        continue
    }
    Start-Sleep -Seconds 5   # prompt settles

    # Calibrate blink-noise floor: max sampled-pixel diff across 1.5 s of idle.
    $w = 0; $h = 0
    $base = [KettlePerf.Native]::CaptureWindow($hwnd, [ref]$w, [ref]$h)
    $noise = 0
    $calEnd = (Get-Date).AddMilliseconds(1500)
    while ((Get-Date) -lt $calEnd) {
        $cap = [KettlePerf.Native]::CaptureWindow($hwnd, [ref]$w, [ref]$h)
        $d = Get-DiffCount $base $cap
        if ($d -gt $noise -and $d -ne [int]::MaxValue) { $noise = $d }
    }
    $threshold = [Math]::Max(20, $noise * 3)
    Write-Host ">> $t : noise floor $noise sampled px, threshold $threshold"

    $lat = @()
    for ($i = 0; $i -lt $Samples; $i++) {
        # Re-baseline right before the keypress (absorbs blink phase).
        $base = [KettlePerf.Native]::CaptureWindow($hwnd, [ref]$w, [ref]$h)
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        [KettlePerf.Native]::SendChar([char]('m'))
        $deadline = (Get-Date).AddMilliseconds(800)
        $hit = $false
        while ((Get-Date) -lt $deadline) {
            $cap = [KettlePerf.Native]::CaptureWindow($hwnd, [ref]$w, [ref]$h)
            if ((Get-DiffCount $base $cap) -ge $threshold) { $sw.Stop(); $hit = $true; break }
        }
        if ($hit) { $lat += [Math]::Round($sw.Elapsed.TotalMilliseconds, 1) }
        [KettlePerf.Native]::SendVk(0x08)   # backspace cleanup
        Start-Sleep -Milliseconds 150
    }
    # Never Stop-Process a pid that pre-existed the spawn (wt.exe can route
    # the new window into the USER'S running WindowsTerminal instance —
    # see Close-SpawnedTerminal in lib-win32.ps1).
    [void](Close-SpawnedTerminal -Hwnd $hwnd -PreexistingPids $prePids)

    if ($lat.Count -gt 0) {
        $sorted = $lat | Sort-Object
        $all[$t] = [ordered]@{
            samples = $lat.Count
            latency_ms_all = $lat
            latency_ms_median = $sorted[[int](($sorted.Count - 1) / 2)]
            latency_ms_p90 = $sorted[[int][Math]::Min($sorted.Count - 1, [Math]::Ceiling($sorted.Count * 0.9) - 1)]
            noise_floor = $noise
        }
        Write-Host ("{0,-10} latency median {1,6} ms  p90 {2,6} ms  ({3} samples)" -f
            $t, $all[$t].latency_ms_median, $all[$t].latency_ms_p90, $lat.Count)
    } else {
        Write-Warning "$t : no latency samples registered"
    }
    Start-Sleep -Seconds 1
}

$all | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $ResultsDir 'latency.json')
Write-Host "done — results in $ResultsDir"
