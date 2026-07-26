# GUI-free deterministic schedule/statistics contract tests.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\statistics.ps1"
. "$PSScriptRoot\schedule.ps1"

function Assert-KettlePerfSelfTest {
    param(
        [Parameter(Mandatory = $true)]
        [bool]$Condition,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Get-KettlePerfScheduleCanonicalText {
    param(
        [Parameter(Mandatory = $true)]
        $Schedule
    )

    return @(
        foreach ($round in $Schedule.rounds) {
            foreach ($visit in $round.visits) {
                @(
                    $visit.cycle,
                    $visit.round,
                    $visit.round_in_cycle,
                    $visit.position,
                    $visit.sequence,
                    $visit.williams_sequence,
                    $visit.terminal,
                    $visit.sample_id,
                    $visit.sample_key
                ) -join '|'
            }
        }
    ) -join "`n"
}

$terminals = @('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')
$schedule = New-KettlePerfWilliamsSchedule `
    -Terminals $terminals -Seed 'kettle-statistics-self-test-v1' `
    -Cycles 2 -Namespace 'selftest'
Assert-KettlePerfSelfTest `
    -Condition (Test-KettlePerfWilliamsSchedule -Schedule $schedule) `
    -Message 'generated Williams schedule failed strict validation'
Assert-KettlePerfSelfTest `
    -Condition ($schedule.round_count -eq 12 -and $schedule.sample_count -eq 72) `
    -Message 'generated schedule does not contain two complete six-terminal cycles'

foreach ($cycle in 1..2) {
    $rounds = @($schedule.rounds | Where-Object { $_.cycle -eq $cycle })
    foreach ($terminal in $terminals) {
        foreach ($position in 1..6) {
            $count = @($rounds.visits | Where-Object {
                $_.terminal -eq $terminal -and $_.position -eq $position
            }).Count
            Assert-KettlePerfSelfTest -Condition ($count -eq 1) `
                -Message "cycle $cycle is not position-balanced"
        }
        foreach ($successor in $terminals) {
            if ($terminal -eq $successor) {
                continue
            }
            $count = 0
            foreach ($round in $rounds) {
                for ($position = 1; $position -lt $round.terminals.Count; $position++) {
                    if (
                        $round.terminals[$position - 1] -eq $terminal -and
                        $round.terminals[$position] -eq $successor
                    ) {
                        $count++
                    }
                }
            }
            Assert-KettlePerfSelfTest -Condition ($count -eq 1) `
                -Message "cycle $cycle lacks directed predecessor balance"
        }
    }
}

$canonical = Get-KettlePerfScheduleCanonicalText -Schedule $schedule
$canonicalHash = Get-KettlePerfSha256Hex -Text $canonical
$expectedScheduleHash = (
    'a0f751336449d4f617802302de0500afa23488e05d38a1966317a98d431b4482'
)
Assert-KettlePerfSelfTest `
    -Condition ($canonicalHash -eq $expectedScheduleHash) `
    -Message 'Williams schedule drifted from the cross-engine fixture'
$secondSchedule = New-KettlePerfWilliamsSchedule `
    -Terminals $terminals -Seed 'kettle-statistics-self-test-v1' `
    -Cycles 2 -Namespace 'selftest'
$secondHash = Get-KettlePerfSha256Hex -Text (
    Get-KettlePerfScheduleCanonicalText -Schedule $secondSchedule
)
Assert-KettlePerfSelfTest -Condition ($canonicalHash -eq $secondHash) `
    -Message 'Williams schedule is not reproducible'

$eightTerminalSchedule = New-KettlePerfWilliamsSchedule `
    -Terminals @('t0', 't1', 't2', 't3', 't4', 't5', 't6', 't7') `
    -Seed 'eight-terminal-fixture' -Cycles 1
Assert-KettlePerfSelfTest -Condition (
    (Test-KettlePerfWilliamsSchedule -Schedule $eightTerminalSchedule) -and
    $eightTerminalSchedule.round_count -eq 8 -and
    $eightTerminalSchedule.sample_count -eq 64
) -Message 'eight-terminal Williams schedule is invalid'

$tampered = New-KettlePerfWilliamsSchedule `
    -Terminals $terminals -Seed 'tamper-fixture' -Cycles 1
$tampered.rounds[0].visits[0].position = 2
Assert-KettlePerfSelfTest `
    -Condition (-not (Test-KettlePerfWilliamsSchedule -Schedule $tampered)) `
    -Message 'strict schedule validation accepted a tampered visit'
$tamperedKey = New-KettlePerfWilliamsSchedule `
    -Terminals $terminals -Seed 'tamper-key-fixture' -Cycles 1
$tamperedKey.rounds[0].visits[0].sample_key = 'synthetic-unique-key'
Assert-KettlePerfSelfTest `
    -Condition (-not (Test-KettlePerfWilliamsSchedule -Schedule $tamperedKey)) `
    -Message 'strict schedule validation accepted a tampered sample key'

$randomA = New-KettlePerfPinnedRandom -Seed 'pinned-prng-fixture'
$randomB = New-KettlePerfPinnedRandom -Seed 'pinned-prng-fixture'
$drawsA = @(1..12 | ForEach-Object {
    Get-KettlePerfPinnedRandomUInt32 -Random $randomA
})
$drawsB = @(1..12 | ForEach-Object {
    Get-KettlePerfPinnedRandomUInt32 -Random $randomB
})
Assert-KettlePerfSelfTest `
    -Condition (($drawsA -join ',') -eq ($drawsB -join ',')) `
    -Message 'pinned PRNG is not reproducible'
$expectedDraws = (
    '3124259874,3681958979,1763022777,4247403868,2722984720,' +
    '181701029,2687359838,4023457891,3955302533,3914354140,' +
    '1377910273,4087365163'
)
Assert-KettlePerfSelfTest `
    -Condition (($drawsA -join ',') -eq $expectedDraws) `
    -Message 'pinned PRNG drifted from the cross-engine fixture'

$nearestRank = Get-KettlePerfNearestRankPercentile `
    -Values @(1..10) -Percentile 0.90
Assert-KettlePerfSelfTest -Condition ($nearestRank -eq 9.0) `
    -Message 'nearest-rank percentile returned the wrong observation'
Assert-KettlePerfSelfTest -Condition (
    $null -eq (Get-KettlePerfMedian -Values @()) -and
    $null -eq (
        Get-KettlePerfNearestRankPercentile -Values @() -Percentile 0.50
    )
) -Message 'empty descriptive statistics did not return null'

$equivalent = Get-KettlePerfPracticalComparison `
    -Candidate 100.4 -Reference 100.0 -HigherIsBetter `
    -AbsoluteMargin 1.0 -RelativeMargin 0.0
Assert-KettlePerfSelfTest `
    -Condition ($equivalent.classification -eq 'equivalent') `
    -Message 'epsilon-sized difference was not classified as equivalent'
$clearWin = Get-KettlePerfPracticalComparison `
    -Candidate 105.0 -Reference 100.0 -HigherIsBetter `
    -AbsoluteMargin 1.0 -RelativeMargin 0.0
Assert-KettlePerfSelfTest `
    -Condition ($clearWin.classification -eq 'win') `
    -Message 'clear practical win was not classified as a win'

$idleNearZero = Get-KettlePerfIdleCpuComparison `
    -Candidate 0.0 -Reference 0.05
$idleClear = Get-KettlePerfIdleCpuComparison `
    -Candidate 0.0 -Reference 0.25
Assert-KettlePerfSelfTest `
    -Condition ($idleNearZero.classification -eq 'equivalent') `
    -Message 'idle CPU 0 versus 0.05 hit a zero discontinuity'
Assert-KettlePerfSelfTest `
    -Condition ($idleClear.classification -eq 'win') `
    -Message 'idle CPU 0 versus 0.25 did not produce a practical win'

$candidate = @(90.0, 91.0, 89.0, 90.0, 92.0, 91.0, 88.0, 89.0)
$reference = @(100.0, 101.0, 99.0, 101.0, 102.0, 100.0, 98.0, 100.0)
$clusters = @('r1', 'r1', 'r2', 'r2', 'r3', 'r3', 'r4', 'r4')
$bootstrapA = Get-KettlePerfPairedClusterBootstrapInterval `
    -Candidate $candidate -Reference $reference -ClusterIds $clusters `
    -Iterations 1000 -ConfidenceLevel 0.95 `
    -Seed 'bootstrap-self-test-v1'
$bootstrapB = Get-KettlePerfPairedClusterBootstrapInterval `
    -Candidate $candidate -Reference $reference -ClusterIds $clusters `
    -Iterations 1000 -ConfidenceLevel 0.95 `
    -Seed 'bootstrap-self-test-v1'
Assert-KettlePerfSelfTest -Condition (
    $bootstrapA.lower -eq $bootstrapB.lower -and
    $bootstrapA.upper -eq $bootstrapB.upper -and
    $bootstrapA.point_estimate -eq $bootstrapB.point_estimate -and
    $bootstrapA.seed_sha256 -eq $bootstrapB.seed_sha256
) -Message 'cluster bootstrap interval is not deterministic'
Assert-KettlePerfSelfTest -Condition (
    $bootstrapA.point_estimate -eq 10.0 -and
    $bootstrapA.lower -eq 10.0 -and
    $bootstrapA.upper -eq 10.5
) -Message 'cluster bootstrap drifted from the pinned fixture'
$bootstrapClass = Get-KettlePerfPairClassification `
    -Residual $bootstrapA.point_estimate -Deadband 1.0 `
    -IntervalLower $bootstrapA.lower -IntervalUpper $bootstrapA.upper
Assert-KettlePerfSelfTest `
    -Condition ($bootstrapClass.classification -eq 'win') `
    -Message 'clear paired bootstrap win was not classified as a win'

$driftValues = @(0..11 | ForEach-Object { 100.0 + $_ })
$drift = Get-KettlePerfTheilSenDrift `
    -Values $driftValues -MaxAbsoluteDriftPct 10.0
Assert-KettlePerfSelfTest -Condition (
    -not $drift.passed -and
    [Math]::Abs($drift.absolute_drift_pct - 11.0) -lt 0.000001
) -Message '11 percent Theil-Sen drift was not rejected'

Write-Output (
    'statistics self-test: PASS ({0}; schedule={1}; bootstrap={2:N3}..{3:N3})' -f
    $PSVersionTable.PSVersion,
    $canonicalHash,
    $bootstrapA.lower,
    $bootstrapA.upper
)
