# vtebench (alacritty/vtebench) PTY-read benchmark, run inside each terminal's
# WSL session. vtebench self-times and writes a gnuplot .dat with per-benchmark
# samples; we additionally summarize medians into vtebench-summary.json.
# Prereq: WSL build at ~/vtebench-target/release/vtebench (cargo build --release
# with CARGO_TARGET_DIR=$HOME/vtebench-target from the vtebench clone).
param(
    [string[]]$Terminals = @('kettle', 'wt', 'alacritty', 'wezterm'),
    [string]$ResultsDir = "$PSScriptRoot\..\..\target\perf-results",
    [string]$VtebenchRepo = 'C:\Users\kevm9\Repos\research\vtebench',
    [string]$KettleExe = "$PSScriptRoot\..\..\target\release\kettle.exe",
    [string]$AlacrittyExe = 'C:\Users\kevm9\Repos\research\bin\alacritty.exe',
    [string]$WeztermExe = 'C:\Users\kevm9\Repos\research\bin\wezterm\wezterm-gui.exe',
    [int]$WindowW = 1280,
    [int]$WindowH = 800,
    [int]$TimeoutSec = 900
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
New-Item -ItemType Directory -Force $ResultsDir | Out-Null

$repoWsl = '/mnt/' + $VtebenchRepo.Substring(0, 1).ToLower() + ($VtebenchRepo.Substring(2) -replace '\\', '/')
$resWsl = '/mnt/' + $ResultsDir.Substring(0, 1).ToLower() + (((Resolve-Path $ResultsDir).Path).Substring(2) -replace '\\', '/')

foreach ($t in $Terminals) {
    $dat = Join-Path $ResultsDir "vtebench-$t.dat"
    Remove-Item $dat -Force -ErrorAction SilentlyContinue
    $bash = "cd $repoWsl && `$HOME/vtebench-target/release/vtebench --dat $resWsl/vtebench-$t.dat; sleep 1"
    $inner = @('wsl.exe', 'bash', '-lc', $bash)
    switch ($t) {
        'kettle'    { $exe = $KettleExe;    $args = @('-e') + $inner }
        'wt'        { $exe = 'wt.exe';      $args = $inner }
        'alacritty' { $exe = $AlacrittyExe; $args = @('-e') + $inner }
        'wezterm'   { $exe = $WeztermExe;   $args = @('start', '--') + $inner }
        default     { Write-Warning "unknown terminal $t"; continue }
    }
    if (-not (Get-Command $exe -ErrorAction SilentlyContinue) -and -not (Test-Path $exe)) {
        Write-Warning "$t executable not found — skipping"; continue
    }

    Write-Host ">> $t : running vtebench (full suite, ~2-5 min)"
    $before = Get-VisibleWindowSet
    $prePids = Get-PidSet
    $proc = Start-Process -FilePath $exe -ArgumentList $args -PassThru
    $hwnd = Wait-NewWindow -Before $before
    if ($hwnd -eq [IntPtr]::Zero) { Write-Warning "$t window never appeared"; continue }
    Start-Sleep -Milliseconds 600
    Set-WindowSize $hwnd $WindowW $WindowH
    $winPid = Get-WindowPid $hwnd

    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        # The .dat is written at the very end; window then closes via the trailing sleep.
        if ((Test-Path $dat) -and (Get-Item $dat).Length -gt 0) { Start-Sleep -Seconds 2; break }
        Start-Sleep -Seconds 2
    }
    if (-not (Test-Path $dat)) { Write-Warning "$t : vtebench timed out / produced no .dat" }
    else { Write-Host ">> $t : .dat written ($([int]((Get-Item $dat).Length / 1KB)) KB)" }
    # Never Stop-Process a pid that pre-existed the spawn (wt.exe can route
    # the new window into the USER'S running WindowsTerminal instance).
    [void](Close-SpawnedTerminal -Hwnd $hwnd -PreexistingPids $prePids)
    Start-Sleep -Seconds 1
}

# Summarize: .dat format = "# benchname" header then one sample (ms) per line.
$summary = [ordered]@{}
foreach ($t in $Terminals) {
    $dat = Join-Path $ResultsDir "vtebench-$t.dat"
    if (-not (Test-Path $dat)) { continue }
    $bench = $null; $vals = @{}
    foreach ($line in Get-Content $dat) {
        if ($line -match '^#\s*(.+)$') { $bench = $Matches[1].Trim(); $vals[$bench] = @() }
        elseif ($line.Trim() -match '^[\d.]+$' -and $bench) { $vals[$bench] += [double]$line.Trim() }
    }
    $summary[$t] = [ordered]@{}
    foreach ($b in ($vals.Keys | Sort-Object)) {
        $sorted = $vals[$b] | Sort-Object
        if ($sorted.Count -gt 0) {
            $summary[$t][$b] = [Math]::Round($sorted[[int](($sorted.Count - 1) / 2)], 2)
        }
    }
}
$summary | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $ResultsDir 'vtebench-summary.json')
Write-Host "done — medians (ms) in vtebench-summary.json"
