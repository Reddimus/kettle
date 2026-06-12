# Full cross-terminal performance suite — the pinned methodology behind
# docs/PERFORMANCE.md's comparative numbers. Runs every probe and drops JSON
# into target/perf-results (label subdirectory per run).
#
# Usage: pwsh -File perf-all.ps1 -Label baseline-v2.19.0
param(
    [string]$Label = (Get-Date -Format 'yyyyMMdd-HHmmss'),
    [string[]]$Terminals = @('kettle', 'wt', 'alacritty', 'wezterm'),
    [switch]$SkipVtebench,
    [switch]$SkipLatency
)
$ErrorActionPreference = 'Stop'
$resultsDir = "$PSScriptRoot\..\..\target\perf-results\$Label"
New-Item -ItemType Directory -Force $resultsDir | Out-Null

Write-Host "=== kettle perf suite — label: $Label ==="
Write-Host "--- throughput (Windows console write path) ---"
& "$PSScriptRoot\throughput.ps1" -Terminals $Terminals -ResultsDir $resultsDir

Write-Host "--- startup / fresh memory / idle CPU ---"
& "$PSScriptRoot\startup-idle.ps1" -Terminals $Terminals -ResultsDir $resultsDir

if (-not $SkipLatency) {
    Write-Host "--- input latency probe ---"
    & "$PSScriptRoot\latency.ps1" -Terminals $Terminals -ResultsDir $resultsDir
}

if (-not $SkipVtebench) {
    Write-Host "--- vtebench (WSL PTY read) ---"
    & "$PSScriptRoot\vtebench-wsl.ps1" -Terminals $Terminals -ResultsDir $resultsDir
}

Write-Host "=== complete — results in $resultsDir ==="
