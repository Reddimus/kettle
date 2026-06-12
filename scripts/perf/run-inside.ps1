# Throughput runner — executes INSIDE the terminal under test.
# Writes fixed payloads to the live console and times the writes; the terminal's
# ability to consume the stream (ConPTY -> parser -> renderer backpressure) is
# what's being measured, same principle as cmuratori/termbench.
# Results are appended as one JSON object per run to <ResultsDir>\throughput-<Terminal>.json
# so the orchestrator never has to scrape the screen.
param(
    [Parameter(Mandatory)] [string]$Terminal,
    [Parameter(Mandatory)] [string]$ResultsDir,
    [string]$PayloadDir = "$PSScriptRoot\..\..\target\perf-payloads",
    [int]$Iterations = 5,
    [int]$SettleSeconds = 3
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $ResultsDir | Out-Null

# Let the window finish opening / get resized by the orchestrator before measuring.
Start-Sleep -Seconds $SettleSeconds

$out = [Console]::Out
$results = [ordered]@{
    terminal = $Terminal
    timestamp = (Get-Date).ToString('o')
    cols = $Host.UI.RawUI.WindowSize.Width
    rows = $Host.UI.RawUI.WindowSize.Height
    iterations = $Iterations
    payloads = [ordered]@{}
}

foreach ($name in 'ascii', 'sgr', 'unicode') {
    $path = Join-Path $PayloadDir "$name.txt"
    if (-not (Test-Path $path)) { continue }
    $text = [System.IO.File]::ReadAllText($path)
    $bytes = (Get-Item $path).Length

    # Pre-split into 32 KiB chunks OUTSIDE the timed region so allocation noise
    # doesn't pollute the measurement; the timed loop is pure console writes.
    $chunkSize = 32768
    $chunks = [System.Collections.Generic.List[string]]::new()
    for ($off = 0; $off -lt $text.Length; $off += $chunkSize) {
        $len = [Math]::Min($chunkSize, $text.Length - $off)
        $chunks.Add($text.Substring($off, $len))
    }

    # Warmup (1/8 of the payload) primes glyph atlases and scrollback paths.
    foreach ($c in $chunks[0..([Math]::Max(0, [int]($chunks.Count / 8)))]) { $out.Write($c) }
    $out.Flush()
    Start-Sleep -Milliseconds 500

    # Adaptive: aim for $Iterations runs but stop once this payload has consumed
    # ~2 minutes, so a slow terminal still yields a (single-run) measurement
    # instead of timing out the orchestrator.
    $times = @()
    $cum = 0.0
    for ($i = 0; $i -lt $Iterations -and ($i -eq 0 -or $cum -lt 120); $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        foreach ($c in $chunks) { $out.Write($c) }
        $out.Flush()
        $sw.Stop()
        $times += $sw.Elapsed.TotalSeconds
        $cum += $sw.Elapsed.TotalSeconds
        Start-Sleep -Milliseconds 300
    }
    $sorted = $times | Sort-Object
    $median = $sorted[[int](($sorted.Count - 1) / 2)]
    $results.payloads[$name] = [ordered]@{
        bytes = $bytes
        runs = $times.Count
        seconds_all = $times
        seconds_median = [Math]::Round($median, 3)
        mb_per_s_median = [Math]::Round(($bytes / 1MB) / $median, 2)
    }
}

$json = $results | ConvertTo-Json -Depth 6
[System.IO.File]::WriteAllText((Join-Path $ResultsDir "throughput-$Terminal.json"), $json)
$out.WriteLine("")
$out.WriteLine("DONE $Terminal — results written.")
Start-Sleep -Seconds 1
