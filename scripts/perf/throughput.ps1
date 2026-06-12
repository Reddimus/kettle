# Cross-terminal throughput orchestrator.
# Spawns each terminal running scripts/perf/run-inside.ps1, normalizes the window
# size, waits for the JSON results the runner writes, and samples the process
# tree's working set right after the flood (memory-under-load comes free here).
#
# Usage: pwsh -File throughput.ps1 [-Terminals kettle,wt,alacritty,wezterm] [-ResultsDir <dir>]
param(
    [string[]]$Terminals = @('kettle', 'wt', 'alacritty', 'wezterm'),
    [string]$ResultsDir = "$PSScriptRoot\..\..\target\perf-results",
    [string]$KettleExe = "$PSScriptRoot\..\..\target\release\kettle.exe",
    [string]$AlacrittyExe = 'C:\Users\kevm9\Repos\research\bin\alacritty.exe',
    [string]$WeztermExe = 'C:\Users\kevm9\Repos\research\bin\wezterm\wezterm-gui.exe',
    [int]$WindowW = 1280,
    [int]$WindowH = 800,
    [int]$TimeoutSec = 600
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"

New-Item -ItemType Directory -Force $ResultsDir | Out-Null
& "$PSScriptRoot\gen-payloads.ps1" | Out-Null

$runner = Join-Path $PSScriptRoot 'run-inside.ps1'
$pwshArgs = { param($t) @('-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $runner,
        '-Terminal', $t, '-ResultsDir', $ResultsDir) }

foreach ($t in $Terminals) {
    $resultFile = Join-Path $ResultsDir "throughput-$t.json"
    Remove-Item $resultFile -Force -ErrorAction SilentlyContinue

    $inner = @('pwsh') + (& $pwshArgs $t)
    switch ($t) {
        'kettle'    { $exe = $KettleExe;       $args = @('-e') + $inner }
        'wt'        { $exe = 'wt.exe';         $args = $inner }
        'alacritty' { $exe = $AlacrittyExe;    $args = @('-e') + $inner }
        'wezterm'   { $exe = $WeztermExe;      $args = @('start', '--') + $inner }
        default     { Write-Warning "unknown terminal $t"; continue }
    }
    if (-not (Get-Command $exe -ErrorAction SilentlyContinue) -and -not (Test-Path $exe)) {
        Write-Warning "$t executable not found ($exe) — skipping"; continue
    }

    Write-Host ">> $t : spawning"
    $before = Get-VisibleWindowSet
    $prePids = Get-PidSet
    $proc = Start-Process -FilePath $exe -ArgumentList $args -PassThru
    $hwnd = Wait-NewWindow -Before $before
    if ($hwnd -eq [IntPtr]::Zero) { Write-Warning "$t window never appeared"; continue }
    Start-Sleep -Milliseconds 600
    Set-WindowSize $hwnd $WindowW $WindowH
    $winPid = Get-WindowPid $hwnd
    Write-Host ">> $t : window '$(Get-WindowTitle $hwnd)' pid=$winPid resized to ${WindowW}x${WindowH}"

    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $memAfter = $null
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $resultFile) {
            Start-Sleep -Milliseconds 300   # runner lingers ~1s after writing — sample under load
            $memAfter = Get-ProcessTreeStats -RootPid $winPid
            break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not (Test-Path $resultFile)) {
        Write-Warning "$t : timed out waiting for results"
    } else {
        $r = Get-Content $resultFile | ConvertFrom-Json
        if ($memAfter) {
            $r | Add-Member -NotePropertyName postflood_ws_mb -NotePropertyValue $memAfter.WorkingSetMB
            $r | ConvertTo-Json -Depth 6 | Set-Content $resultFile
        }
        foreach ($p in $r.payloads.PSObject.Properties) {
            Write-Host ("   {0,-8} {1,8} MB/s (median of {2})" -f $p.Name, $p.Value.mb_per_s_median, $r.iterations)
        }
        if ($memAfter) { Write-Host ("   post-flood WS: {0} MB across {1}" -f $memAfter.WorkingSetMB, ($memAfter.Names -join '+')) }
    }

    # Window closes itself when the runner's shell exits; force-kill stragglers
    # — but never a pid that pre-existed the spawn (wt.exe can route the new
    # window into the USER'S running WindowsTerminal instance; see
    # Close-SpawnedTerminal in lib-win32.ps1).
    Start-Sleep -Seconds 3
    [void](Close-SpawnedTerminal -Hwnd $hwnd -PreexistingPids $prePids)
    Start-Sleep -Seconds 1
}
Write-Host "done — results in $ResultsDir"
