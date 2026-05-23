#requires -Version 5.1
<#
.SYNOPSIS
    scripts/bench.ps1 — reproduce the docs/PERFORMANCE.md numbers on Windows.

.DESCRIPTION
    Cycle 730: PowerShell-native equivalent of scripts/bench.sh. Uses
    System.Diagnostics.Process for wall-clock and PeakWorkingSet64
    instead of GNU `/usr/bin/time -f`, which doesn't exist on Windows.

    Builds a release binary if one isn't present, then runs three
    measurements 5 times each:
      - `kettle --version`         (cold-cache startup floor)
      - `kettle --screenshot`      (full GPU pipeline boot + render)
      - `kettle --screenshot-menu` (cycle-251 menu pass)

    Output format per row: `<wall-clock>s, <peak working-set in MB>`
    so the spread across runs is visible at a glance. Pipe to a file
    for a snapshot to attach to a PR.

.NOTES
    Requires PowerShell 5.1+ (preinstalled on Windows 10+) or
    PowerShell Core 7+. No external dependencies — uses the .NET
    Diagnostics.Process API.

    To run from the project root:
        powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bench.ps1
    Or via just:
        just bench
#>

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$bin = Join-Path (Get-Location) 'target\release\kettle.exe'
if (-not (Test-Path $bin)) {
    Write-Output "==> building release binary"
    cargo build --release -p kettle
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed (exit $LASTEXITCODE)"
    }
}

Write-Output "==> kettle build identity"
& $bin --version

Write-Output ""
Write-Output "==> binary size"
$size = (Get-Item $bin).Length
"{0:F1} MB ({1} bytes)" -f ($size / 1MB), $size

function Invoke-Bench {
    param(
        [Parameter(Mandatory)] [string] $Label,
        [Parameter(Mandatory)] [string[]] $Arguments
    )
    Write-Output ""
    Write-Output "==> $Label x 5"
    foreach ($i in 1..5) {
        # Direct Process API. The .NET `PeakWorkingSet64` property is
        # documented to return the kernel's peak working set, but in
        # practice (PS 5.1 on Windows 10/11) it returns 0 once the
        # process has exited — the OS releases the metadata before
        # the property can read it via `Refresh()`. Workaround: poll
        # `WorkingSet64` on a tight loop while the process is alive
        # and track the max. 5ms granularity catches all peaks for
        # both fast (--version, ~100ms) and slow (--screenshot, ~2s)
        # invocations on a typical Win11 box.
        $pi = New-Object System.Diagnostics.ProcessStartInfo
        $pi.FileName = $bin
        # Use ArgumentList (PowerShell 7+) or join-and-quote on 5.1.
        # The simple join works because none of kettle's CLI args
        # contain spaces or quotes that need escaping.
        $pi.Arguments = ($Arguments -join ' ')
        $pi.RedirectStandardOutput = $true
        $pi.RedirectStandardError = $true
        $pi.UseShellExecute = $false
        $pi.CreateNoWindow = $true
        $p = [System.Diagnostics.Process]::Start($pi)
        # Poll for peak working set on the running process. The
        # StandardOutput / StandardError pipes are drained
        # asynchronously after exit (small kettle outputs fit in
        # the OS pipe buffer; no risk of blocking the child).
        $maxWs = 0L
        while (-not $p.HasExited) {
            $p.Refresh()
            if ($p.WorkingSet64 -gt $maxWs) { $maxWs = $p.WorkingSet64 }
            Start-Sleep -Milliseconds 5
        }
        $null = $p.StandardOutput.ReadToEnd()
        $null = $p.StandardError.ReadToEnd()
        $wall = ($p.ExitTime - $p.StartTime).TotalSeconds
        $peak = $maxWs / 1MB
        "{0,7:F3} s, {1,7:F1} MB peak working set (exit {2})" -f $wall, $peak, $p.ExitCode
    }
}

Invoke-Bench '--version (startup floor)' @('--version')
Invoke-Bench '--screenshot (GPU pipeline + render)' @('--screenshot', "$env:TEMP\kettle-bench.png")
Invoke-Bench '--screenshot-menu (cycle-251 menu pass)' @('--screenshot-menu', "$env:TEMP\kettle-bench-menu.png")

Write-Output ""
Write-Output "==> done. See docs/PERFORMANCE.md for the published baseline."
