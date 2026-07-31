# Deterministic Williams-balanced round schedules for cross-terminal probes.

if (-not (Get-Command New-KettlePerfPinnedRandom -ErrorAction SilentlyContinue)) {
    . "$PSScriptRoot\statistics.ps1"
}

function Invoke-KettlePerfDeterministicShuffle {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Values,
        [Parameter(Mandatory = $true)]
        $Random
    )

    # Explicit allocation avoids PowerShell 5.1 reusing the input object[]
    # while PowerShell 7 materializes a copy for the same cast expression.
    $copy = [object[]]::new($Values.Count)
    [Array]::Copy($Values, $copy, $Values.Count)
    for ($index = $copy.Count - 1; $index -gt 0; $index--) {
        $swapIndex = Get-KettlePerfPinnedRandomInt `
            -Random $Random -ExclusiveMaximum ($index + 1)
        $temporary = $copy[$index]
        $copy[$index] = $copy[$swapIndex]
        $copy[$swapIndex] = $temporary
    }
    return $copy
}

function Get-KettlePerfWilliamsBaseSequence {
    param(
        [Parameter(Mandatory = $true)]
        [ValidateRange(6, 100)]
        [int]$Count
    )

    if (($Count % 2) -ne 0) {
        throw 'Williams-balanced schedules require an even terminal count'
    }
    $sequence = [Collections.Generic.List[int]]::new()
    $sequence.Add(0)
    for ($offset = 1; $offset -lt ($Count / 2); $offset++) {
        $sequence.Add($offset)
        $sequence.Add($Count - $offset)
    }
    $sequence.Add([int]($Count / 2))
    return $sequence.ToArray()
}

function New-KettlePerfWilliamsSchedule {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This only constructs an in-memory schedule object.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Terminals,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Seed,
        [ValidateRange(1, 10000)]
        [int]$Cycles = 1,
        [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
        [string]$Namespace = 'perf'
    )

    if (
        $Terminals.Count -lt 6 -or
        $Terminals.Count -gt 100 -or
        ($Terminals.Count % 2) -ne 0
    ) {
        throw 'Williams-balanced schedules require 6 to 100 even terminals'
    }
    $sampleCount = (
        [int64]$Cycles *
        [int64]$Terminals.Count *
        [int64]$Terminals.Count
    )
    if ($sampleCount -gt 1000000) {
        throw 'Williams-balanced schedules are capped at 1,000,000 samples'
    }
    $terminalSet = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    foreach ($terminal in $Terminals) {
        if (
            -not $terminal -or
            $terminal.Length -gt 128 -or
            $terminal.Contains([char]0) -or
            -not $terminalSet.Add($terminal)
        ) {
            throw 'Schedule terminal names must be unique, non-empty, and bounded'
        }
    }

    $random = New-KettlePerfPinnedRandom -Seed $Seed
    $baseSequence = Get-KettlePerfWilliamsBaseSequence `
        -Count $Terminals.Count
    $rounds = [Collections.Generic.List[object]]::new()
    $sampleId = 0
    $roundId = 0
    $sequenceId = 0
    for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
        $cycleTerminals = @(
            Invoke-KettlePerfDeterministicShuffle `
                -Values $Terminals -Random $random
        )
        $williamsRows = [object[]]@(
            0..($Terminals.Count - 1) |
                ForEach-Object { [int]$_ }
        )
        $williamsRows = @(
            Invoke-KettlePerfDeterministicShuffle `
                -Values $williamsRows -Random $random
        )
        $reverse = (
            Get-KettlePerfPinnedRandomInt -Random $random -ExclusiveMaximum 2
        ) -eq 1
        $roundInCycle = 0
        foreach ($row in $williamsRows) {
            $roundId++
            $roundInCycle++
            $sequenceId++
            $sequenceIndexes = @(
                foreach ($baseIndex in $baseSequence) {
                    ([int]$baseIndex + [int]$row) % $Terminals.Count
                }
            )
            if ($reverse) {
                [Array]::Reverse($sequenceIndexes)
            }
            $sequence = @(
                $sequenceIndexes |
                    ForEach-Object { $cycleTerminals[$_] }
            )
            $visits = [Collections.Generic.List[object]]::new()
            for ($position = 1; $position -le $sequence.Count; $position++) {
                $sampleId++
                $visits.Add([pscustomobject][ordered]@{
                    sample_id = $sampleId
                    sample_key = (
                        '{0}-c{1:d4}-r{2:d6}-p{3:d3}-s{4:d8}' -f
                        $Namespace, $cycle, $roundId, $position, $sampleId
                    )
                    cycle = $cycle
                    round = $roundId
                    round_in_cycle = $roundInCycle
                    position = $position
                    sequence = $sequenceId
                    williams_sequence = [int]$row + 1
                    terminal = $sequence[$position - 1]
                })
            }
            $rounds.Add([pscustomobject][ordered]@{
                cycle = $cycle
                round = $roundId
                round_in_cycle = $roundInCycle
                sequence = $sequenceId
                williams_sequence = [int]$row + 1
                terminals = [string[]]$sequence
                visits = [object[]]$visits.ToArray()
            })
        }
    }
    $schedule = [pscustomobject][ordered]@{
        schema_version = 1
        algorithm = 'williams-even-sha256-v1'
        namespace = $Namespace
        seed_sha256 = $random.seed_sha256
        terminals = [string[]]$Terminals
        terminal_count = $Terminals.Count
        cycles = $Cycles
        rounds_per_cycle = $Terminals.Count
        round_count = $rounds.Count
        sample_count = $sampleId
        rounds = [object[]]$rounds.ToArray()
    }
    [void](Assert-KettlePerfWilliamsSchedule -Schedule $schedule)
    return $schedule
}

function Get-KettlePerfScheduleProperty {
    param(
        $Object,
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    if ($null -eq $Object) {
        return $null
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($property) {
        return $property.Value
    }
    return $null
}

function Get-KettlePerfWilliamsScheduleIssues {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function returns every schedule validation issue.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        $Schedule
    )

    $issues = [Collections.Generic.List[string]]::new()
    $addIssue = {
        param([string]$Message)
        if ($issues.Count -lt 100) {
            $issues.Add($Message)
        }
    }
    $terminalCount = [int](
        Get-KettlePerfScheduleProperty $Schedule 'terminal_count'
    )
    $cycles = [int](Get-KettlePerfScheduleProperty $Schedule 'cycles')
    $namespace = [string](
        Get-KettlePerfScheduleProperty $Schedule 'namespace'
    )
    $terminals = @(
        Get-KettlePerfScheduleProperty $Schedule 'terminals'
    )
    $rounds = @(Get-KettlePerfScheduleProperty $Schedule 'rounds')
    if (
        (Get-KettlePerfScheduleProperty $Schedule 'schema_version') -ne 1 -or
        (Get-KettlePerfScheduleProperty $Schedule 'algorithm') -ne
            'williams-even-sha256-v1'
    ) {
        & $addIssue 'schedule metadata is invalid'
    }
    $validatedTerminals = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    $terminalCoverageValid = (
        $terminalCount -ge 6 -and
        $terminalCount -le 100 -and
        ($terminalCount % 2) -eq 0 -and
        $terminals.Count -eq $terminalCount
    )
    foreach ($terminal in $terminals) {
        $terminalName = [string]$terminal
        if (
            -not $terminalName -or
            $terminalName.Length -gt 128 -or
            $terminalName.Contains([char]0) -or
            -not $validatedTerminals.Add($terminalName)
        ) {
            $terminalCoverageValid = $false
        }
    }
    if (-not $terminalCoverageValid) {
        & $addIssue 'terminal coverage is invalid'
        return $issues
    }
    if ($namespace -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$') {
        & $addIssue 'schedule namespace is invalid'
    }
    $cycleCoverageValid = (
        $cycles -ge 1 -and
        $cycles -le 10000 -and
        ([int64]$cycles * $terminalCount * $terminalCount) -le 1000000 -and
        $rounds.Count -eq ($cycles * $terminalCount) -and
        (Get-KettlePerfScheduleProperty $Schedule 'round_count') -eq
            $rounds.Count -and
        (Get-KettlePerfScheduleProperty $Schedule 'rounds_per_cycle') -eq
            $terminalCount
    )
    if (-not $cycleCoverageValid) {
        & $addIssue 'cycle or round coverage is invalid'
        return $issues
    }
    if (
        [string](Get-KettlePerfScheduleProperty $Schedule 'seed_sha256') -notmatch
            '^[0-9a-f]{64}$'
    ) {
        & $addIssue 'schedule seed provenance is invalid'
    }

    $terminalIndexes = [Collections.Generic.Dictionary[string, int]]::new(
        [StringComparer]::Ordinal
    )
    for ($index = 0; $index -lt $terminals.Count; $index++) {
        $terminalIndexes[[string]$terminals[$index]] = $index
    }
    $sampleIds = [Collections.Generic.HashSet[int]]::new()
    $sampleKeys = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    $expectedSampleId = 0
    $expectedRoundId = 0
    for ($cycle = 1; $cycle -le $cycles; $cycle++) {
        $cycleRounds = @($rounds | Where-Object {
            (Get-KettlePerfScheduleProperty $_ 'cycle') -eq $cycle
        })
        if ($cycleRounds.Count -ne $terminalCount) {
            & $addIssue "cycle $cycle does not contain a full Williams cycle"
            continue
        }
        $positionCounts = @{}
        $predecessorCounts = @{}
        $williamsRows = [Collections.Generic.HashSet[int]]::new()
        $expectedRoundInCycle = 0
        foreach ($round in $cycleRounds) {
            $expectedRoundId++
            $expectedRoundInCycle++
            $roundId = [int](Get-KettlePerfScheduleProperty $round 'round')
            $roundInCycle = [int](
                Get-KettlePerfScheduleProperty $round 'round_in_cycle'
            )
            $sequenceId = [int](
                Get-KettlePerfScheduleProperty $round 'sequence'
            )
            $williamsSequence = [int](
                Get-KettlePerfScheduleProperty $round 'williams_sequence'
            )
            $sequence = @(
                Get-KettlePerfScheduleProperty $round 'terminals'
            )
            $visits = @(Get-KettlePerfScheduleProperty $round 'visits')
            if (
                $roundId -ne $expectedRoundId -or
                $roundInCycle -ne $expectedRoundInCycle -or
                $sequenceId -ne $roundId
            ) {
                & $addIssue "round identifiers are invalid at round $roundId"
            }
            if (
                $williamsSequence -lt 1 -or
                $williamsSequence -gt $terminalCount -or
                -not $williamsRows.Add($williamsSequence)
            ) {
                & $addIssue "Williams sequence id is invalid at round $roundId"
            }
            if (
                $sequence.Count -ne $terminalCount -or
                @($sequence | Select-Object -Unique).Count -ne $terminalCount -or
                $visits.Count -ne $terminalCount
            ) {
                & $addIssue "round $roundId has invalid terminal coverage"
                continue
            }
            for ($position = 1; $position -le $terminalCount; $position++) {
                $terminal = [string]$sequence[$position - 1]
                if (-not $terminalIndexes.ContainsKey($terminal)) {
                    & $addIssue "round $roundId contains an unknown terminal"
                    continue
                }
                $positionKey = (
                    "$($terminalIndexes[$terminal])|$position"
                )
                $positionCounts[$positionKey] = (
                    [int]$positionCounts[$positionKey] + 1
                )
                $visit = $visits[$position - 1]
                $expectedSampleId++
                $sampleId = [int](
                    Get-KettlePerfScheduleProperty $visit 'sample_id'
                )
                $sampleKey = [string](
                    Get-KettlePerfScheduleProperty $visit 'sample_key'
                )
                $expectedSampleKey = (
                    '{0}-c{1:d4}-r{2:d6}-p{3:d3}-s{4:d8}' -f
                    $namespace,
                    $cycle,
                    $roundId,
                    $position,
                    $expectedSampleId
                )
                if (
                    $sampleId -ne $expectedSampleId -or
                    -not $sampleIds.Add($sampleId) -or
                    -not $sampleKey -or
                    -not $sampleKeys.Add($sampleKey) -or
                    -not [StringComparer]::Ordinal.Equals(
                        $sampleKey,
                        $expectedSampleKey
                    ) -or
                    (Get-KettlePerfScheduleProperty $visit 'cycle') -ne
                        $cycle -or
                    (Get-KettlePerfScheduleProperty $visit 'round') -ne
                        $roundId -or
                    (Get-KettlePerfScheduleProperty $visit 'round_in_cycle') -ne
                        $roundInCycle -or
                    (Get-KettlePerfScheduleProperty $visit 'position') -ne
                        $position -or
                    (Get-KettlePerfScheduleProperty $visit 'sequence') -ne
                        $sequenceId -or
                    (Get-KettlePerfScheduleProperty $visit 'williams_sequence') -ne
                        $williamsSequence -or
                    -not [StringComparer]::Ordinal.Equals(
                        [string](
                            Get-KettlePerfScheduleProperty $visit 'terminal'
                        ),
                        $terminal
                    )
                ) {
                    & $addIssue "visit integrity is invalid at sample $sampleId"
                }
                if ($position -gt 1) {
                    $predecessor = [string]$sequence[$position - 2]
                    $pairKey = (
                        "$($terminalIndexes[$predecessor])>" +
                        "$($terminalIndexes[$terminal])"
                    )
                    $predecessorCounts[$pairKey] = (
                        [int]$predecessorCounts[$pairKey] + 1
                    )
                }
            }
        }
        foreach ($terminal in $terminals) {
            $terminalIndex = $terminalIndexes[[string]$terminal]
            for ($position = 1; $position -le $terminalCount; $position++) {
                if (
                    [int]$positionCounts["$terminalIndex|$position"] -ne 1
                ) {
                    & $addIssue (
                        "cycle $cycle is not position-balanced for $terminal"
                    )
                }
            }
            foreach ($successor in $terminals) {
                $successorIndex = $terminalIndexes[[string]$successor]
                if (
                    $terminalIndex -ne $successorIndex -and
                    [int]$predecessorCounts[
                        "$terminalIndex>$successorIndex"
                    ] -ne 1
                ) {
                    & $addIssue (
                        "cycle $cycle lacks directed predecessor balance"
                    )
                }
            }
        }
    }
    if (
        $expectedSampleId -ne
            (Get-KettlePerfScheduleProperty $Schedule 'sample_count') -or
        $expectedSampleId -ne ($cycles * $terminalCount * $terminalCount)
    ) {
        & $addIssue 'sample coverage is invalid'
    }
    return $issues
}

function Test-KettlePerfWilliamsSchedule {
    param(
        [Parameter(Mandatory = $true)]
        $Schedule
    )

    return @(
        Get-KettlePerfWilliamsScheduleIssues -Schedule $Schedule
    ).Count -eq 0
}

function Assert-KettlePerfWilliamsSchedule {
    param(
        [Parameter(Mandatory = $true)]
        $Schedule
    )

    $issues = @(
        Get-KettlePerfWilliamsScheduleIssues -Schedule $Schedule
    )
    if ($issues.Count -gt 0) {
        throw (
            'Williams schedule validation failed: ' +
            ($issues -join '; ')
        )
    }
    return $true
}
