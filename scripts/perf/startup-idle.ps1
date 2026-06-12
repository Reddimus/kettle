# Startup time + fresh working set + idle CPU for each terminal.
# Startup = spawn -> first visible top-level window (covers launcher indirection
# like wt.exe handing off to WindowsTerminal.exe — that overhead is real UX).
# Idle CPU = process-tree CPU-seconds delta over a fixed window with the terminal
# focused at an interactive prompt (cursor blinking).
param(
    [string[]]$Terminals = @('kettle', 'wt', 'alacritty', 'wezterm'),
    [string]$ResultsDir = "$PSScriptRoot\..\..\target\perf-results",
    [string]$KettleExe = "$PSScriptRoot\..\..\target\release\kettle.exe",
    [string]$AlacrittyExe = 'C:\Users\kevm9\Repos\research\bin\alacritty.exe',
    [string]$WeztermExe = 'C:\Users\kevm9\Repos\research\bin\wezterm\wezterm-gui.exe',
    [int]$StartupRuns = 5,
    [int]$IdleSeconds = 60
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
New-Item -ItemType Directory -Force $ResultsDir | Out-Null

function Resolve-Spawn([string]$t) {
    switch ($t) {
        'kettle'    { @{ exe = $KettleExe;    args = @() } }
        'wt'        { @{ exe = 'wt.exe';      args = @() } }
        'alacritty' { @{ exe = $AlacrittyExe; args = @() } }
        'wezterm'   { @{ exe = $WeztermExe;   args = @('start') } }
    }
}

$all = [ordered]@{}
foreach ($t in $Terminals) {
    $s = Resolve-Spawn $t
    if (-not (Get-Command $s.exe -ErrorAction SilentlyContinue) -and -not (Test-Path $s.exe)) {
        Write-Warning "$t executable not found — skipping"; continue
    }

    # --- startup: N cold-ish spawns, time to first visible window ---
    $startupMs = @()
    for ($i = 0; $i -lt $StartupRuns; $i++) {
        $before = Get-VisibleWindowSet
        $prePids = Get-PidSet
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        if ($s.args.Count -gt 0) { $proc = Start-Process -FilePath $s.exe -ArgumentList $s.args -PassThru }
        else { $proc = Start-Process -FilePath $s.exe -PassThru }
        $hwnd = Wait-NewWindow -Before $before
        $sw.Stop()
        if ($hwnd -eq [IntPtr]::Zero) { Write-Warning "$t run $i window never appeared"; continue }
        $startupMs += $sw.ElapsedMilliseconds
        Start-Sleep -Milliseconds 800
        # Never Stop-Process a pid that pre-existed the spawn (wt.exe can route
        # the new window into the USER'S running WindowsTerminal instance).
        [void](Close-SpawnedTerminal -Hwnd $hwnd -PreexistingPids $prePids)
        try { if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force } } catch {}
        Start-Sleep -Milliseconds 700
    }

    # --- fresh WS + idle CPU: one long-lived window ---
    $before = Get-VisibleWindowSet
    $prePids = Get-PidSet
    if ($s.args.Count -gt 0) { $proc = Start-Process -FilePath $s.exe -ArgumentList $s.args -PassThru }
    else { $proc = Start-Process -FilePath $s.exe -PassThru }
    $hwnd = Wait-NewWindow -Before $before
    $idle = $null; $freshWs = $null
    if ($hwnd -ne [IntPtr]::Zero) {
        $winPid = Get-WindowPid $hwnd
        if ($prePids.Contains($winPid)) {
            # Shared-instance terminal (e.g. wt windowingBehavior=useExisting):
            # process-tree WS/CPU would blend in the user's live session — not
            # attributable. Close just our window and report nulls.
            Write-Warning "$t window landed in pre-existing pid $winPid — skipping WS/idle (not attributable)"
            [void](Close-SpawnedTerminal -Hwnd $hwnd -PreexistingPids $prePids)
        } else {
            [void][KettlePerf.Native]::SetForegroundWindow($hwnd)
            Start-Sleep -Seconds 5   # shell prompt settles
            $s0 = Get-ProcessTreeStats -RootPid $winPid
            $freshWs = $s0.WorkingSetMB
            Start-Sleep -Seconds $IdleSeconds
            $s1 = Get-ProcessTreeStats -RootPid $winPid
            $idle = [Math]::Round((($s1.CpuSeconds - $s0.CpuSeconds) / $IdleSeconds) * 100, 2)
            try { Stop-Process -Id $winPid -Force } catch {}
        }
    }

    $sorted = $startupMs | Sort-Object
    $all[$t] = [ordered]@{
        startup_ms_all = $startupMs
        startup_ms_median = if ($sorted.Count) { $sorted[[int](($sorted.Count - 1) / 2)] } else { $null }
        fresh_ws_mb = $freshWs
        idle_cpu_pct = $idle
        idle_seconds = $IdleSeconds
    }
    Write-Host ("{0,-10} startup(med) {1,5} ms  fresh WS {2,7} MB  idle CPU {3,5}%" -f
        $t, $all[$t].startup_ms_median, $freshWs, $idle)
}

$all | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $ResultsDir 'startup-idle.json')
Write-Host "done — results in $ResultsDir"
