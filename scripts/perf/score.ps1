# Score a perf-all result directory and fail when kettle is not in the top half.
#
# Usage:
#   pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after
#   pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after `
#     -BaselineResultsDir target/perf-results/before -MaxRegressionPct 7.5
param(
    [Parameter(Mandatory = $true)]
    [string]$ResultsDir,
    [string]$BaselineResultsDir = '',
    [double]$MaxRegressionPct = 7.5,
    [string]$OutJson = ''
)
$ErrorActionPreference = 'Stop'

function Read-JsonFile([string]$Path) {
    if (-not (Test-Path $Path)) { return $null }
    return Get-Content -Raw $Path | ConvertFrom-Json
}

function As-Double($Value) {
    if ($null -eq $Value) { return $null }
    try {
        $d = [double]$Value
        if ([double]::IsNaN($d) -or [double]::IsInfinity($d) -or $d -le 0.0) {
            return $null
        }
        return $d
    } catch {
        return $null
    }
}

function Payload-Mbps($Payloads, [string[]]$Names) {
    if ($null -eq $Payloads) { return $null }
    foreach ($name in $Names) {
        $prop = $Payloads.PSObject.Properties[$name]
        if ($prop) {
            $v = As-Double $prop.Value.mb_per_s_median
            if ($null -ne $v) { return $v }
        }
    }
    return $null
}

function Load-Perf([string]$Dir) {
    $startup = Read-JsonFile (Join-Path $Dir 'startup-idle.json')
    $names = New-Object 'System.Collections.Generic.HashSet[string]'

    if ($startup) {
        foreach ($p in $startup.PSObject.Properties) {
            [void]$names.Add($p.Name)
        }
    }
    foreach ($f in Get-ChildItem -Path $Dir -Filter 'throughput-*.json' -ErrorAction SilentlyContinue) {
        $n = [IO.Path]::GetFileNameWithoutExtension($f.Name).Substring('throughput-'.Length)
        [void]$names.Add($n)
    }

    $rows = [ordered]@{}
    foreach ($name in ($names | Sort-Object)) {
        $tp = Read-JsonFile (Join-Path $Dir "throughput-$name.json")
        $st = if ($startup -and $startup.PSObject.Properties[$name]) {
            $startup.PSObject.Properties[$name].Value
        } else {
            $null
        }

        $rows[$name] = [ordered]@{
            ascii_mbps = Payload-Mbps $tp.payloads @('ascii')
            sgr_mbps = Payload-Mbps $tp.payloads @('sgr', 'sgr-heavy')
            unicode_mbps = Payload-Mbps $tp.payloads @('unicode')
            postflood_ws_mb = As-Double $tp.postflood_ws_mb
            startup_ms = As-Double $st.startup_ms_median
            fresh_ws_mb = As-Double $st.fresh_ws_mb
            idle_cpu_pct = As-Double $st.idle_cpu_pct
        }
    }
    return $rows
}

function Score-Rows($Rows) {
    $metricDefs = @(
        @{ name = 'ascii_mbps'; higher = $true; weight = 1.0 },
        @{ name = 'sgr_mbps'; higher = $true; weight = 1.0 },
        @{ name = 'unicode_mbps'; higher = $true; weight = 1.0 },
        @{ name = 'startup_ms'; higher = $false; weight = 1.0 },
        @{ name = 'idle_cpu_pct'; higher = $false; weight = 1.0 },
        @{ name = 'fresh_ws_mb'; higher = $false; weight = 0.75 },
        @{ name = 'postflood_ws_mb'; higher = $false; weight = 0.75 }
    )

    $scores = [ordered]@{}
    foreach ($term in $Rows.Keys) {
        $scores[$term] = [ordered]@{
            score = 0.0
            weight = 0.0
            metrics = [ordered]@{}
        }
    }

    foreach ($def in $metricDefs) {
        $name = $def.name
        $vals = @()
        foreach ($term in $Rows.Keys) {
            $v = As-Double $Rows[$term][$name]
            if ($null -ne $v) {
                $vals += [pscustomobject]@{ term = $term; value = $v }
            }
        }
        if ($vals.Count -lt 2) { continue }

        $best = if ($def.higher) {
            ($vals | Measure-Object -Property value -Maximum).Maximum
        } else {
            ($vals | Measure-Object -Property value -Minimum).Minimum
        }

        foreach ($v in $vals) {
            $component = if ($def.higher) { $v.value / $best } else { $best / $v.value }
            $component = [Math]::Max(0.0, [Math]::Min(1.0, $component))
            $weighted = $component * [double]$def.weight
            $scores[$v.term].score += $weighted
            $scores[$v.term].weight += [double]$def.weight
            $scores[$v.term].metrics[$name] = [Math]::Round($component, 4)
        }
    }

    foreach ($term in $scores.Keys) {
        if ($scores[$term].weight -gt 0.0) {
            $scores[$term].score = [Math]::Round($scores[$term].score / $scores[$term].weight, 4)
        }
    }
    return $scores
}

function Regression-Report($Now, $Base, [double]$MaxPct) {
    if (-not $Base -or -not $Base.Contains('kettle') -or -not $Now.Contains('kettle')) {
        return @()
    }
    $defs = @(
        @{ name = 'ascii_mbps'; higher = $true },
        @{ name = 'sgr_mbps'; higher = $true },
        @{ name = 'unicode_mbps'; higher = $true },
        @{ name = 'startup_ms'; higher = $false },
        @{ name = 'idle_cpu_pct'; higher = $false },
        @{ name = 'fresh_ws_mb'; higher = $false },
        @{ name = 'postflood_ws_mb'; higher = $false }
    )
    $bad = @()
    foreach ($d in $defs) {
        $n = As-Double $Now.kettle[$d.name]
        $b = As-Double $Base.kettle[$d.name]
        if ($null -eq $n -or $null -eq $b) { continue }
        $delta = if ($d.higher) { (($b - $n) / $b) * 100.0 } else { (($n - $b) / $b) * 100.0 }
        if ($delta -gt $MaxPct) {
            $bad += [pscustomobject]@{
                metric = $d.name
                baseline = [Math]::Round($b, 3)
                current = [Math]::Round($n, 3)
                regression_pct = [Math]::Round($delta, 2)
            }
        }
    }
    return $bad
}

$rows = Load-Perf $ResultsDir
if (-not $rows.Contains('kettle')) {
    throw "No kettle results found in $ResultsDir"
}
$scores = Score-Rows $rows
$ranked = $scores.Keys |
    Sort-Object @{ Expression = { $scores[$_].score }; Descending = $true }, @{ Expression = { $_ }; Ascending = $true } |
    ForEach-Object {
        [pscustomobject]@{
            terminal = $_
            score = $scores[$_].score
            metrics = $rows[$_]
            metric_scores = $scores[$_].metrics
        }
    }

$kettleRank = 1 + [array]::IndexOf(@($ranked.terminal), 'kettle')
$topHalfCutoff = [Math]::Ceiling($ranked.Count / 2.0)
$beaten = @($ranked | Where-Object { $_.score -lt $scores.kettle.score }).Count

$baselineRows = if ($BaselineResultsDir) { Load-Perf $BaselineResultsDir } else { $null }
$regressions = @(Regression-Report $rows $baselineRows $MaxRegressionPct)

$result = [ordered]@{
    results_dir = (Resolve-Path $ResultsDir).Path
    baseline_results_dir = if ($BaselineResultsDir) { (Resolve-Path $BaselineResultsDir).Path } else { $null }
    terminals = $ranked
    kettle_rank = $kettleRank
    top_half_cutoff = $topHalfCutoff
    terminals_beaten_by_kettle = $beaten
    max_regression_pct = $MaxRegressionPct
    regressions = $regressions
    passed = ($kettleRank -le $topHalfCutoff -and $beaten -ge 2 -and @($regressions).Count -eq 0)
}

Write-Host "terminal        score"
Write-Host "----------------------"
foreach ($r in $ranked) {
    Write-Host ("{0,-14} {1,6:N4}" -f $r.terminal, $r.score)
}
Write-Host ""
Write-Host "kettle rank: $kettleRank of $($ranked.Count); top-half cutoff: $topHalfCutoff; beaten: $beaten"
if (@($regressions).Count -gt 0) {
    Write-Host "regressions over $MaxRegressionPct%:"
    $regressions | Format-Table -AutoSize | Out-String | Write-Host
}

if ($OutJson) {
    $result | ConvertTo-Json -Depth 8 | Set-Content $OutJson
}

if (-not $result.passed) {
    exit 1
}
