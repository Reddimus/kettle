# GUI-free contract tests for the controlled startup-readiness protocol.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\startup-ready.ps1"

function Assert-KettlePerfStartupReadySelfTest {
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

function Assert-KettlePerfStartupReadyFailure {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    $threw = $false
    try {
        & $Action
    } catch {
        $threw = $true
    }
    if (-not $threw) {
        throw $Message
    }
}

$testParent = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-startup-ready-self-test-' +
    [Guid]::NewGuid().ToString('N')
)
$descriptor = $null
$defaultDescriptor = $null
$roguePath = $null
[void][IO.Directory]::CreateDirectory($testParent)
$sentinelPath = Join-Path $testParent 'parent-sentinel.txt'
[IO.File]::WriteAllText(
    $sentinelPath,
    'outside the per-launch scratch root',
    [Text.UTF8Encoding]::new($false)
)

try {
    $runId = [Guid]::NewGuid().ToString('D')
    $descriptor = New-KettlePerfStartupReadyDescriptor `
        -RunId $runId -ScratchParent $testParent `
        -SampleId 'self-test.sample-1' -HoldSeconds 1 `
        -GoTimeoutSeconds 5 -DsrTimeoutMs 2000
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $descriptor.Schema -eq 'kettle-startup-ready-v1' -and
        $descriptor.RunId -eq $runId.ToLowerInvariant() -and
        $descriptor.Command.Count -eq ($descriptor.Arguments.Count + 1) -and
        $descriptor.Command[0] -eq $descriptor.PowerShellExecutable -and
        $descriptor.Arguments[0] -eq '-NoLogo' -and
        $descriptor.Arguments -contains '-ChildPayloadBase64' -and
        $descriptor.MarkerColumns -eq 64 -and
        $descriptor.MarkerRows -eq 6
    ) -Message 'startup-readiness descriptor contract is invalid'
    Assert-KettlePerfStartupReadySelfTest -Condition (
        (Test-Path -LiteralPath $descriptor.ScratchRoot -PathType Container) -and
        -not (Test-Path -LiteralPath $descriptor.GoPath) -and
        -not (Test-Path -LiteralPath $descriptor.ReadyPath) -and
        [IO.Path]::GetDirectoryName($descriptor.ScratchRoot) -eq $testParent
    ) -Message 'startup-readiness scratch layout is invalid'
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (-not (Test-KettlePerfStartupReadyGo $descriptor)) `
        -Message 'GO marker unexpectedly existed before parent publication'

    $publishedGo = Publish-KettlePerfStartupReadyGo $descriptor
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $publishedGo -eq $descriptor.GoPath -and
        (Test-KettlePerfStartupReadyGo $descriptor)
    ) -Message 'atomic GO publication failed validation'
    $goBytes = [IO.File]::ReadAllBytes($descriptor.GoPath)
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $goBytes.Length -gt 0 -and
        $goBytes.Length -le 1024 -and
        -not (
            $goBytes.Length -ge 3 -and
            $goBytes[0] -eq 0xef -and
            $goBytes[1] -eq 0xbb -and
            $goBytes[2] -eq 0xbf
        )
    ) -Message 'GO marker is not bounded BOM-free UTF-8'
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](Publish-KettlePerfStartupReadyGo $descriptor)
    } -Message 'GO publication overwrote an existing marker'
    $outsideMarker = Join-Path $testParent 'escaped.marker'
    Assert-KettlePerfStartupReadyFailure -Action {
        Write-KettlePerfStartupReadyAtomicFile `
            -Descriptor $descriptor -Path $outsideMarker `
            -StagePath $descriptor.ReadyStagePath -Bytes $goBytes
    } -Message 'atomic publication accepted a path outside the scratch root'
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (-not (Test-Path -LiteralPath $outsideMarker)) `
        -Message 'rejected atomic publication wrote outside the scratch root'

    $payloadArgumentIndex = [Array]::IndexOf(
        $descriptor.Arguments,
        '-ChildPayloadBase64'
    ) + 1
    $payloadBytes = [Convert]::FromBase64String(
        $descriptor.Arguments[$payloadArgumentIndex]
    )
    $bomPayload = [byte[]]@([byte]0xef, [byte]0xbb, [byte]0xbf) +
        $payloadBytes
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](ConvertFrom-KettlePerfStartupReadyChildPayload `
            ([Convert]::ToBase64String($bomPayload)))
    } -Message 'child payload accepted a UTF-8 BOM'

    $childArguments = [string[]]$descriptor.Arguments
    $dsrResponse = ([string][char]27) + '[0n'
    $childTimer = [Diagnostics.Stopwatch]::StartNew()
    $childOutput = @(
        $dsrResponse |
            & $descriptor.PowerShellExecutable @childArguments 2>&1
    )
    $childTimer.Stop()
    $childExitCode = $LASTEXITCODE
    Assert-KettlePerfStartupReadySelfTest `
        -Condition ($childExitCode -eq 0) `
        -Message (
            'startup-readiness child failed: ' +
            (($childOutput | ForEach-Object { [string]$_ }) -join ' | ')
        )
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (Test-KettlePerfStartupReadyMarker $descriptor) `
        -Message 'ready marker failed strict validation'
    $readyItem = Get-Item -LiteralPath $descriptor.ReadyPath
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $childTimer.ElapsedMilliseconds -ge 800 -and
        (([DateTime]::UtcNow - $readyItem.LastWriteTimeUtc).TotalMilliseconds) `
            -ge 700
    ) -Message 'readiness child did not hold after publishing its marker'

    $outputText = (
        $childOutput | ForEach-Object { [string]$_ }
    ) -join "`n"
    $escape = [string][char]27
    $paintPrefix = (
        "$escape[48;2;$($descriptor.MarkerRed);" +
        "$($descriptor.MarkerGreen);$($descriptor.MarkerBlue);38;2;"
    )
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $outputText.Contains($paintPrefix) -and
        $outputText.Contains("KETTLE READY $($descriptor.LaunchId)") -and
        $outputText.Contains("$escape[5n")
    ) -Message 'child output lacks the unique truecolor marker or CSI 5n'

    $readyBytes = [IO.File]::ReadAllBytes($descriptor.ReadyPath)
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $readyBytes.Length -gt 0 -and
        $readyBytes.Length -le 1024 -and
        -not (
            $readyBytes.Length -ge 3 -and
            $readyBytes[0] -eq 0xef -and
            $readyBytes[1] -eq 0xbb -and
            $readyBytes[2] -eq 0xbf
        )
    ) -Message 'ready marker is not bounded BOM-free UTF-8'

    $wrongRun = $descriptor | Select-Object *
    $wrongRun.RunId = [Guid]::NewGuid().ToString('D')
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (-not (
            Test-KettlePerfStartupReadyMarkerContent `
                -Descriptor $wrongRun -Kind ready -Bytes $readyBytes
        )) `
        -Message 'ready marker accepted the wrong run id'
    $bomReady = [byte[]]@([byte]0xef, [byte]0xbb, [byte]0xbf) +
        $readyBytes
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (-not (
            Test-KettlePerfStartupReadyMarkerContent `
                -Descriptor $descriptor -Kind ready -Bytes $bomReady
        )) `
        -Message 'ready marker accepted a UTF-8 BOM'
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (-not (
            Test-KettlePerfStartupReadyMarkerContent `
                -Descriptor $descriptor -Kind ready `
                -Bytes ([byte[]]@(0xff))
        )) `
        -Message 'ready marker accepted invalid UTF-8'
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (-not (
            Test-KettlePerfStartupReadyMarkerContent `
                -Descriptor $descriptor -Kind ready `
                -Bytes ([byte[]]::new(1025))
        )) `
        -Message 'ready marker accepted an oversized payload'

    $width = 60
    $height = 40
    $frame = [byte[]]::new($width * $height * 4)
    for ($pixel = 0; $pixel -lt 300; $pixel++) {
        $offset = $pixel * 4
        $frame[$offset] = [byte]$descriptor.MarkerBlue
        $frame[$offset + 1] = [byte]$descriptor.MarkerGreen
        $frame[$offset + 2] = [byte]$descriptor.MarkerRed
        $frame[$offset + 3] = 255
    }
    Assert-KettlePerfStartupReadySelfTest -Condition (
        Test-KettlePerfPaintedMarkerCapture `
            -BgraBytes $frame -Width $width -Height $height `
            -ExpectedRed $descriptor.MarkerRed `
            -ExpectedGreen $descriptor.MarkerGreen `
            -ExpectedBlue $descriptor.MarkerBlue `
            -MinimumPixelCount 200 -MinimumFrameFraction 0.05
    ) -Message 'synthetic painted marker was not detected'
    # A renderer that is a few levels off must still be recognised -- Rio
    # paints this marker's red channel two levels low and its blue three high,
    # and an exact comparison aborted the whole comparator run over it.
    Assert-KettlePerfStartupReadySelfTest -Condition (
        Test-KettlePerfPaintedMarkerCapture `
            -BgraBytes $frame -Width $width -Height $height `
            -ExpectedRed ([Math]::Min(255, $descriptor.MarkerRed + 2)) `
            -ExpectedGreen $descriptor.MarkerGreen `
            -ExpectedBlue ([Math]::Max(0, $descriptor.MarkerBlue - 3)) `
            -MinimumPixelCount 200 -MinimumFrameFraction 0.05
    ) -Message 'capture rejected a marker a real renderer would produce'
    # A DIFFERENT colour must still be rejected. The tolerance is a few levels,
    # not a licence to match anything -- without this the check above could be
    # satisfied by widening the band until every frame passes.
    Assert-KettlePerfStartupReadySelfTest -Condition (
        -not (
            Test-KettlePerfPaintedMarkerCapture `
                -BgraBytes $frame -Width $width -Height $height `
                -ExpectedRed ([int]($descriptor.MarkerRed -bxor 0x80)) `
                -ExpectedGreen $descriptor.MarkerGreen `
                -ExpectedBlue $descriptor.MarkerBlue `
                -MinimumPixelCount 200 -MinimumFrameFraction 0.05
        )
    ) -Message 'capture accepted the wrong marker RGB'
    # The tolerance is a bound, not an opening. Drive it explicitly: at
    # tolerance 4, a channel 4 levels off still matches and one 5 levels off
    # does not. Without this pair the acceptance above could be satisfied by
    # widening the band until every frame passes, which is the failure mode a
    # tolerance invites.
    Assert-KettlePerfStartupReadySelfTest -Condition (
        Test-KettlePerfPaintedMarkerCapture `
            -BgraBytes $frame -Width $width -Height $height `
            -ExpectedRed ([Math]::Min(255, $descriptor.MarkerRed + 4)) `
            -ExpectedGreen $descriptor.MarkerGreen `
            -ExpectedBlue $descriptor.MarkerBlue `
            -MinimumPixelCount 200 -MinimumFrameFraction 0.05 `
            -ChannelTolerance 4
    ) -Message 'a channel exactly at the tolerance must match'
    Assert-KettlePerfStartupReadySelfTest -Condition (
        -not (
            Test-KettlePerfPaintedMarkerCapture `
                -BgraBytes $frame -Width $width -Height $height `
                -ExpectedRed ([Math]::Min(255, $descriptor.MarkerRed + 5)) `
                -ExpectedGreen $descriptor.MarkerGreen `
                -ExpectedBlue $descriptor.MarkerBlue `
                -MinimumPixelCount 200 -MinimumFrameFraction 0.05 `
                -ChannelTolerance 4
        )
    ) -Message 'a channel one level past the tolerance must not match'
    Assert-KettlePerfStartupReadySelfTest -Condition (
        -not (
            Test-KettlePerfPaintedMarkerCapture `
                -BgraBytes $frame -Width $width -Height $height `
                -ExpectedRed $descriptor.MarkerRed `
                -ExpectedGreen $descriptor.MarkerGreen `
                -ExpectedBlue $descriptor.MarkerBlue `
                -MinimumPixelCount 400 -MinimumFrameFraction 0.05
        )
    ) -Message 'capture accepted too few marker pixels'
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](Test-KettlePerfPaintedMarkerCapture `
            -BgraBytes ([byte[]]::new(3)) -Width 1 -Height 1 `
            -ExpectedRed 1 -ExpectedGreen 2 -ExpectedBlue 3)
    } -Message 'capture accepted an invalid BGRA byte count'
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](Test-KettlePerfPaintedMarkerCapture `
            -BgraBytes ([byte[]]@()) -Width 16384 -Height 16384 `
            -ExpectedRed 1 -ExpectedGreen 2 -ExpectedBlue 3)
    } -Message 'capture accepted an unbounded pixel count'

    Assert-KettlePerfStartupReadyFailure -Action {
        [void](New-KettlePerfStartupReadyDescriptor `
            -RunId $runId -ScratchParent $testParent -HoldSeconds 0)
    } -Message 'descriptor accepted a zero hold'
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](New-KettlePerfStartupReadyDescriptor `
            -RunId $runId -ScratchParent $testParent -HoldSeconds 86401)
    } -Message 'descriptor accepted an excessive hold'
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](New-KettlePerfStartupReadyDescriptor `
            -RunId $runId -ScratchParent $testParent `
            -SampleId "unsafe';exit 0")
    } -Message 'descriptor accepted an injectable sample id'

    $defaultDescriptor = New-KettlePerfStartupReadyDescriptor `
        -RunId $runId -ScratchParent $testParent -SampleId 'defaults'
    Assert-KettlePerfStartupReadySelfTest -Condition (
        $defaultDescriptor.HoldSeconds -eq 86400 -and
        $defaultDescriptor.GoTimeoutSeconds -eq 30 -and
        $defaultDescriptor.DsrTimeoutMs -eq 5000 -and
        $defaultDescriptor.ScratchRoot -ne $descriptor.ScratchRoot -and
        $defaultDescriptor.GoPath -ne $descriptor.GoPath -and
        $defaultDescriptor.ReadyPath -ne $descriptor.ReadyPath
    ) -Message 'default bounds or unpredictable names drifted'
    [void](Remove-KettlePerfStartupReadyScratch $defaultDescriptor)
    $defaultDescriptor = $null

    $roguePath = Join-Path $descriptor.ScratchRoot 'unexpected.txt'
    [IO.File]::WriteAllText(
        $roguePath,
        'cleanup must refuse this',
        [Text.UTF8Encoding]::new($false)
    )
    Assert-KettlePerfStartupReadyFailure -Action {
        [void](Remove-KettlePerfStartupReadyScratch $descriptor)
    } -Message 'cleanup accepted an unexpected scratch entry'
    Assert-KettlePerfStartupReadySelfTest -Condition (
        (Test-Path -LiteralPath $roguePath -PathType Leaf) -and
        (Test-Path -LiteralPath $sentinelPath -PathType Leaf)
    ) -Message 'refused cleanup removed data'
    [IO.File]::Delete($roguePath)
    $roguePath = $null
    [void](Remove-KettlePerfStartupReadyScratch $descriptor)
    $descriptor = $null
    Assert-KettlePerfStartupReadySelfTest `
        -Condition (Test-Path -LiteralPath $sentinelPath -PathType Leaf) `
        -Message 'scratch cleanup escaped into its parent'

    Write-Output (
        'startup-ready self-test: PASS ({0})' -f
        $PSVersionTable.PSVersion
    )
} finally {
    if ($roguePath -and (Test-Path -LiteralPath $roguePath -PathType Leaf)) {
        [IO.File]::Delete($roguePath)
    }
    foreach ($candidate in @($defaultDescriptor, $descriptor)) {
        if ($null -eq $candidate) {
            continue
        }
        foreach ($path in @(
            $candidate.GoPath,
            $candidate.GoStagePath,
            $candidate.ReadyPath,
            $candidate.ReadyStagePath
        )) {
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                [IO.File]::Delete($path)
            }
        }
        if (
            Test-Path -LiteralPath $candidate.ScratchRoot -PathType Container
        ) {
            [IO.Directory]::Delete($candidate.ScratchRoot, $false)
        }
    }
    if (Test-Path -LiteralPath $sentinelPath -PathType Leaf) {
        [IO.File]::Delete($sentinelPath)
    }
    if (Test-Path -LiteralPath $testParent -PathType Container) {
        [IO.Directory]::Delete($testParent, $false)
    }
}
