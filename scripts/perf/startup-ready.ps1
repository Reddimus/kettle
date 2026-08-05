# Controlled, GUI-independent readiness protocol for terminal startup probes.
# The nonce and marker identifiers prevent accidental cross-run matches; they
# are deliberately non-secret and are not an authentication mechanism.
[CmdletBinding()]
param(
    [string]$ChildPayloadBase64 = ''
)

$script:KettlePerfStartupReadySchema = 'kettle-startup-ready-v1'
$script:KettlePerfStartupReadyMaxMarkerBytes = 1024
$script:KettlePerfStartupReadyMaxPayloadBytes = 4096
$script:KettlePerfStartupReadyMaxPaintBytes = 4096

function Get-KettlePerfStartupReadyProperty {
    param(
        [Parameter(Mandatory = $true)]
        $Object,
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        throw "Startup-readiness descriptor is missing $Name"
    }
    return $property.Value
}

function Get-KettlePerfStartupReadyFullPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not $Path -or $Path.Length -gt 4096 -or $Path.Contains([char]0)) {
        throw 'Startup-readiness path is empty or exceeds its size boundary'
    }
    $fullPath = [IO.Path]::GetFullPath($Path)
    $pathRoot = [IO.Path]::GetPathRoot($fullPath)
    while (
        $fullPath.Length -gt $pathRoot.Length -and
        (
            $fullPath.EndsWith(
                [string][IO.Path]::DirectorySeparatorChar,
                [StringComparison]::Ordinal
            ) -or
            $fullPath.EndsWith(
                [string][IO.Path]::AltDirectorySeparatorChar,
                [StringComparison]::Ordinal
            )
        )
    ) {
        $fullPath = $fullPath.Substring(0, $fullPath.Length - 1)
    }
    return $fullPath
}

function Test-KettlePerfStartupReadyPathEqual {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Left,
        [Parameter(Mandatory = $true)]
        [string]$Right
    )

    $comparison = if ([IO.Path]::DirectorySeparatorChar -eq '\') {
        [StringComparison]::OrdinalIgnoreCase
    } else {
        [StringComparison]::Ordinal
    }
    return [string]::Equals($Left, $Right, $comparison)
}

function New-KettlePerfStartupReadyRandomHex {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Returns random text and does not change external state.'
    )]
    param(
        [ValidateRange(16, 64)]
        [int]$ByteCount = 16
    )

    $bytes = [byte[]]::new($ByteCount)
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $random.GetBytes($bytes)
    } finally {
        $random.Dispose()
    }
    return -join @(
        $bytes | ForEach-Object { $_.ToString('x2') }
    )
}

function ConvertTo-KettlePerfStartupReadyRunId {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RunId
    )

    try {
        $parsed = [Guid]::ParseExact($RunId, 'D')
    } catch {
        throw 'Startup-readiness run id must be a D-format GUID'
    }
    return $parsed.ToString('D')
}

function Resolve-KettlePerfStartupReadyPowerShell {
    param(
        [string]$PowerShellExecutable = ''
    )

    $candidate = $PowerShellExecutable
    if (-not $candidate) {
        $candidate = Get-Command pwsh -CommandType Application `
            -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    } elseif (-not [IO.Path]::IsPathRooted($candidate)) {
        $candidate = Get-Command $candidate -CommandType Application `
            -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    }
    $resolved = (
        Resolve-Path -LiteralPath $candidate -ErrorAction Stop
    ).Path
    if (
        -not (Test-Path -LiteralPath $resolved -PathType Leaf) -or
        [IO.Path]::GetFileName($resolved) -notmatch '^pwsh(?:\.exe)?$'
    ) {
        throw 'Startup readiness requires a verified PowerShell 7 executable'
    }
    return $resolved
}

function Assert-KettlePerfStartupReadyDescriptor {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor,
        [switch]$RequireScratchRoot
    )

    $schema = [string](
        Get-KettlePerfStartupReadyProperty $Descriptor 'Schema'
    )
    $runId = [string](
        Get-KettlePerfStartupReadyProperty $Descriptor 'RunId'
    )
    $sampleId = [string](
        Get-KettlePerfStartupReadyProperty $Descriptor 'SampleId'
    )
    $launchId = [string](
        Get-KettlePerfStartupReadyProperty $Descriptor 'LaunchId'
    )
    $nonce = [string](
        Get-KettlePerfStartupReadyProperty $Descriptor 'Nonce'
    )
    if ($schema -cne $script:KettlePerfStartupReadySchema) {
        throw 'Startup-readiness schema is invalid'
    }
    if (
        (ConvertTo-KettlePerfStartupReadyRunId $runId) -cne
            $runId.ToLowerInvariant()
    ) {
        throw 'Startup-readiness run id is not canonical'
    }
    if ($sampleId -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$') {
        throw 'Startup-readiness sample id is invalid'
    }
    if ($launchId -notmatch '^[0-9a-f]{32}$') {
        throw 'Startup-readiness launch id is invalid'
    }
    if ($nonce -notmatch '^[0-9a-f]{64}$') {
        throw 'Startup-readiness nonce is invalid'
    }

    $markerRed = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerRed'
    )
    $markerGreen = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerGreen'
    )
    $markerBlue = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerBlue'
    )
    $markerTop = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerTop'
    )
    $markerLeft = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerLeft'
    )
    $markerColumns = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerColumns'
    )
    $markerRows = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'MarkerRows'
    )
    $holdSeconds = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'HoldSeconds'
    )
    $goTimeoutSeconds = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'GoTimeoutSeconds'
    )
    $dsrTimeoutMs = [int](
        Get-KettlePerfStartupReadyProperty $Descriptor 'DsrTimeoutMs'
    )
    if (
        $markerRed -lt 0 -or $markerRed -gt 255 -or
        $markerGreen -lt 0 -or $markerGreen -gt 255 -or
        $markerBlue -lt 0 -or $markerBlue -gt 255 -or
        $markerTop -lt 1 -or $markerTop -gt 10 -or
        $markerLeft -lt 1 -or $markerLeft -gt 10 -or
        $markerColumns -lt 48 -or $markerColumns -gt 96 -or
        $markerRows -lt 3 -or $markerRows -gt 12 -or
        $holdSeconds -lt 1 -or $holdSeconds -gt 86400 -or
        $goTimeoutSeconds -lt 1 -or $goTimeoutSeconds -gt 300 -or
        $dsrTimeoutMs -lt 100 -or $dsrTimeoutMs -gt 30000
    ) {
        throw 'Startup-readiness geometry or timing boundary is invalid'
    }

    $scratchParent = Get-KettlePerfStartupReadyFullPath (
        [string](
            Get-KettlePerfStartupReadyProperty $Descriptor 'ScratchParent'
        )
    )
    $scratchRoot = Get-KettlePerfStartupReadyFullPath (
        [string](
            Get-KettlePerfStartupReadyProperty $Descriptor 'ScratchRoot'
        )
    )
    if (
        -not (Test-KettlePerfStartupReadyPathEqual `
            ([IO.Path]::GetDirectoryName($scratchRoot)) `
            $scratchParent) -or
        [IO.Path]::GetFileName($scratchRoot) -notmatch
            '^kettle-startup-ready-[0-9a-f]{32}$'
    ) {
        throw 'Startup-readiness scratch root escaped its exact parent'
    }
    if (
        -not (Test-Path -LiteralPath $scratchParent -PathType Container)
    ) {
        throw 'Startup-readiness scratch parent does not exist'
    }
    if ($RequireScratchRoot) {
        if (-not (Test-Path -LiteralPath $scratchRoot -PathType Container)) {
            throw 'Startup-readiness scratch root does not exist'
        }
        $rootItem = Get-Item -LiteralPath $scratchRoot -Force
        if (
            ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne
            0
        ) {
            throw 'Startup-readiness scratch root cannot be a reparse point'
        }
    }

    $pathContracts = @(
        @('GoPath', '^go-[0-9a-f]{32}\.marker$'),
        @('GoStagePath', '^\.go-[0-9a-f]{32}\.[0-9a-f]{32}\.tmp$'),
        @('ReadyPath', '^ready-[0-9a-f]{32}\.marker$'),
        @(
            'ReadyStagePath',
            '^\.ready-[0-9a-f]{32}\.[0-9a-f]{32}\.tmp$'
        )
    )
    $knownPaths = [Collections.Generic.HashSet[string]]::new(
        $(if ([IO.Path]::DirectorySeparatorChar -eq '\') {
            [StringComparer]::OrdinalIgnoreCase
        } else {
            [StringComparer]::Ordinal
        })
    )
    foreach ($contract in $pathContracts) {
        $path = Get-KettlePerfStartupReadyFullPath (
            [string](
                Get-KettlePerfStartupReadyProperty $Descriptor $contract[0]
            )
        )
        if (
            -not (Test-KettlePerfStartupReadyPathEqual `
                ([IO.Path]::GetDirectoryName($path)) `
                $scratchRoot) -or
            [IO.Path]::GetFileName($path) -notmatch $contract[1] -or
            -not $knownPaths.Add($path)
        ) {
            throw "Startup-readiness $($contract[0]) is outside its contract"
        }
    }
    return $true
}

function Get-KettlePerfStartupReadyMarkerText {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor,
        [Parameter(Mandatory = $true)]
        [ValidateSet('go', 'ready')]
        [string]$Kind
    )

    [void](Assert-KettlePerfStartupReadyDescriptor $Descriptor)
    $lines = [Collections.Generic.List[string]]::new()
    if ($Kind -eq 'go') {
        $lines.Add('KETTLE_PERF_STARTUP_GO_V1')
    } else {
        $lines.Add('KETTLE_PERF_STARTUP_READY_V1')
    }
    $lines.Add("run_id=$($Descriptor.RunId)")
    $lines.Add("sample_id=$($Descriptor.SampleId)")
    $lines.Add("launch_id=$($Descriptor.LaunchId)")
    $lines.Add("nonce=$($Descriptor.Nonce)")
    if ($Kind -eq 'ready') {
        $lines.Add(
            "rgb=$($Descriptor.MarkerRed),$($Descriptor.MarkerGreen)," +
            "$($Descriptor.MarkerBlue)"
        )
        $lines.Add(
            "geometry=$($Descriptor.MarkerTop),$($Descriptor.MarkerLeft)," +
            "$($Descriptor.MarkerColumns),$($Descriptor.MarkerRows)"
        )
        $lines.Add("hold_seconds=$($Descriptor.HoldSeconds)")
        $lines.Add('dsr=CSI-0n')
    }
    return (($lines.ToArray() -join "`n") + "`n")
}

function ConvertTo-KettlePerfStartupReadyUtf8 {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Text,
        [ValidateRange(1, 4096)]
        [int]$MaximumBytes = 1024
    )

    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $bytes = $encoding.GetBytes($Text)
    if ($bytes.Length -gt $MaximumBytes) {
        throw 'Startup-readiness UTF-8 payload exceeds its size boundary'
    }
    return $bytes
}

function Write-KettlePerfStartupReadyAtomicFile {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor,
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$StagePath,
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes
    )

    [void](Assert-KettlePerfStartupReadyDescriptor `
        -Descriptor $Descriptor -RequireScratchRoot)
    $isGoPair = (
        (Test-KettlePerfStartupReadyPathEqual `
            (Get-KettlePerfStartupReadyFullPath $Path) `
            (Get-KettlePerfStartupReadyFullPath $Descriptor.GoPath)) -and
        (Test-KettlePerfStartupReadyPathEqual `
            (Get-KettlePerfStartupReadyFullPath $StagePath) `
            (Get-KettlePerfStartupReadyFullPath $Descriptor.GoStagePath))
    )
    $isReadyPair = (
        (Test-KettlePerfStartupReadyPathEqual `
            (Get-KettlePerfStartupReadyFullPath $Path) `
            (Get-KettlePerfStartupReadyFullPath $Descriptor.ReadyPath)) -and
        (Test-KettlePerfStartupReadyPathEqual `
            (Get-KettlePerfStartupReadyFullPath $StagePath) `
            (Get-KettlePerfStartupReadyFullPath $Descriptor.ReadyStagePath))
    )
    if (-not $isGoPair -and -not $isReadyPair) {
        throw 'Startup-readiness atomic publication escaped its exact paths'
    }
    if (
        $Bytes.Length -gt $script:KettlePerfStartupReadyMaxMarkerBytes
    ) {
        throw 'Startup-readiness marker exceeds its size boundary'
    }
    if (
        (Test-Path -LiteralPath $Path) -or
        (Test-Path -LiteralPath $StagePath)
    ) {
        throw 'Startup-readiness marker or stage path already exists'
    }

    $stream = $null
    try {
        $stream = [IO.FileStream]::new(
            $StagePath,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        $stream.Write($Bytes, 0, $Bytes.Length)
        $stream.Flush($true)
        $stream.Dispose()
        $stream = $null
        [IO.File]::Move($StagePath, $Path)
    } finally {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
        if (Test-Path -LiteralPath $StagePath -PathType Leaf) {
            [IO.File]::Delete($StagePath)
        }
    }
}

function Publish-KettlePerfStartupReadyGo {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseApprovedVerbs',
        '',
        Justification = 'Publish describes the parent-issued atomic handshake.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor
    )

    $text = Get-KettlePerfStartupReadyMarkerText `
        -Descriptor $Descriptor -Kind go
    $bytes = ConvertTo-KettlePerfStartupReadyUtf8 `
        -Text $text `
        -MaximumBytes $script:KettlePerfStartupReadyMaxMarkerBytes
    Write-KettlePerfStartupReadyAtomicFile `
        -Descriptor $Descriptor -Path $Descriptor.GoPath `
        -StagePath $Descriptor.GoStagePath -Bytes $bytes
    return $Descriptor.GoPath
}

function Test-KettlePerfStartupReadyMarkerContent {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor,
        [Parameter(Mandatory = $true)]
        [ValidateSet('go', 'ready')]
        [string]$Kind,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [byte[]]$Bytes
    )

    try {
        [void](Assert-KettlePerfStartupReadyDescriptor $Descriptor)
        if (
            $Bytes.Length -eq 0 -or
            $Bytes.Length -gt $script:KettlePerfStartupReadyMaxMarkerBytes
        ) {
            return $false
        }
        $encoding = [Text.UTF8Encoding]::new($false, $true)
        $text = $encoding.GetString($Bytes)
        $expected = Get-KettlePerfStartupReadyMarkerText `
            -Descriptor $Descriptor -Kind $Kind
        if (-not [string]::Equals(
            $text,
            $expected,
            [StringComparison]::Ordinal
        )) {
            return $false
        }
        $roundTrip = $encoding.GetBytes($text)
        if ($roundTrip.Length -ne $Bytes.Length) {
            return $false
        }
        for ($index = 0; $index -lt $Bytes.Length; $index++) {
            if ($roundTrip[$index] -ne $Bytes[$index]) {
                return $false
            }
        }
        return $true
    } catch {
        return $false
    }
}

function Test-KettlePerfStartupReadyMarkerFile {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor,
        [Parameter(Mandatory = $true)]
        [ValidateSet('go', 'ready')]
        [string]$Kind
    )

    try {
        [void](Assert-KettlePerfStartupReadyDescriptor `
            -Descriptor $Descriptor -RequireScratchRoot)
        $path = if ($Kind -eq 'go') {
            $Descriptor.GoPath
        } else {
            $Descriptor.ReadyPath
        }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            return $false
        }
        $item = Get-Item -LiteralPath $path -Force
        if (
            $item.Length -le 0 -or
            $item.Length -gt $script:KettlePerfStartupReadyMaxMarkerBytes -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            return $false
        }
        $bytes = [IO.File]::ReadAllBytes($path)
        return Test-KettlePerfStartupReadyMarkerContent `
            -Descriptor $Descriptor -Kind $Kind -Bytes $bytes
    } catch {
        return $false
    }
}

function Test-KettlePerfStartupReadyGo {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor
    )

    return Test-KettlePerfStartupReadyMarkerFile `
        -Descriptor $Descriptor -Kind go
}

function Test-KettlePerfStartupReadyMarker {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor
    )

    return Test-KettlePerfStartupReadyMarkerFile `
        -Descriptor $Descriptor -Kind ready
}

function Wait-KettlePerfStartupReadyMarker {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor,
        [ValidateRange(1, 300000)]
        [int]$TimeoutMs = 10000,
        [ValidateRange(1, 1000)]
        [int]$PollMilliseconds = 10
    )

    [void](Assert-KettlePerfStartupReadyDescriptor `
        -Descriptor $Descriptor -RequireScratchRoot)
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt $TimeoutMs) {
        if (Test-Path -LiteralPath $Descriptor.ReadyPath -PathType Leaf) {
            if (Test-KettlePerfStartupReadyMarker $Descriptor) {
                return $true
            }
            throw 'Startup-readiness ready marker exists but is invalid'
        }
        Start-Sleep -Milliseconds $PollMilliseconds
    }
    return $false
}

function Get-KettlePerfStartupReadyPaintText {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor
    )

    [void](Assert-KettlePerfStartupReadyDescriptor $Descriptor)
    $escape = [string][char]27
    $luminance = (
        $Descriptor.MarkerRed +
        $Descriptor.MarkerGreen +
        $Descriptor.MarkerBlue
    )
    $foreground = if ($luminance -ge 384) {
        @(0, 0, 0)
    } else {
        @(255, 255, 255)
    }
    $sgr = (
        "$escape[48;2;$($Descriptor.MarkerRed);" +
        "$($Descriptor.MarkerGreen);$($Descriptor.MarkerBlue);" +
        "38;2;$($foreground[0]);$($foreground[1]);" +
        "$($foreground[2])m"
    )
    $label = " KETTLE READY $($Descriptor.LaunchId) "
    $builder = [Text.StringBuilder]::new()
    [void]$builder.Append("$escape[?25l")
    for ($row = 0; $row -lt $Descriptor.MarkerRows; $row++) {
        $screenRow = $Descriptor.MarkerTop + $row
        [void]$builder.Append(
            "$escape[$screenRow;$($Descriptor.MarkerLeft)H"
        )
        [void]$builder.Append($sgr)
        $content = if ($row -eq 0) {
            $label.PadRight($Descriptor.MarkerColumns)
        } else {
            ([string]' ').PadRight($Descriptor.MarkerColumns)
        }
        [void]$builder.Append($content)
        [void]$builder.Append("$escape[0m")
    }
    $nextRow = $Descriptor.MarkerTop + $Descriptor.MarkerRows
    [void]$builder.Append("$escape[$nextRow;1H$escape[?25h")
    $text = $builder.ToString()
    $bytes = ConvertTo-KettlePerfStartupReadyUtf8 `
        -Text $text -MaximumBytes $script:KettlePerfStartupReadyMaxPaintBytes
    if ($bytes.Length -gt $script:KettlePerfStartupReadyMaxPaintBytes) {
        throw 'Startup-readiness paint sequence exceeds its size boundary'
    }
    return $text
}

function Test-KettlePerfPaintedMarkerCapture {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [byte[]]$BgraBytes,
        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 16384)]
        [int]$Width,
        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 16384)]
        [int]$Height,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 255)]
        [int]$ExpectedRed,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 255)]
        [int]$ExpectedGreen,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 255)]
        [int]$ExpectedBlue,
        [ValidateRange(1, 10000000)]
        [int]$MinimumPixelCount = 256,
        [ValidateRange(0.000001, 1.0)]
        [double]$MinimumFrameFraction = 0.0005,
        [ValidateRange(0, 24)]
        [int]$ChannelTolerance = 16
    )

    $pixelCount = [int64]$Width * [int64]$Height
    if ($pixelCount -gt 33554432) {
        throw 'Startup-readiness capture exceeds 33,554,432 pixels'
    }
    $expectedBytes = $pixelCount * 4
    if ($BgraBytes.LongLength -ne $expectedBytes) {
        throw 'Startup-readiness capture byte count does not match BGRA geometry'
    }
    $required = [Math]::Max(
        [int64]$MinimumPixelCount,
        [int64][Math]::Ceiling(
            $pixelCount * $MinimumFrameFraction
        )
    )
    if ($required -gt $pixelCount) {
        return $false
    }
    # Match within a small per-channel tolerance rather than exactly.
    #
    # A terminal is not obliged to hand back the bytes it was given. Rio
    # renders this marker's `107,95,66` as `105,95,69` -- a colour-management
    # difference of 2-3 per channel -- so an exact comparison failed its
    # startup readiness for 30s and aborted the whole comparator run, for a
    # reason that has nothing to do with performance. Alacritty happens to be
    # byte-exact, which is why this went unnoticed.
    #
    # The deviation is not a constant offset -- Rio renders `48,89,94` as
    # `59,89,94`, eleven levels of red, while `107,95,66` comes back two low.
    # That is the shape of a linear-space blend, worst in the dark range, so the
    # tolerance has to cover the widest case rather than the first one measured.
    #
    # 16 is bounded by arithmetic, not taste. The marker is `48 + n % 176` per
    # channel, so it lives in 48..223. The pinned isolated-config background is
    # 16,16,16 -- at least 32 from any possible marker, twice the tolerance --
    # and the marker's own foreground text is 244, at least 21 above the
    # brightest marker. A 16-level band therefore cannot be satisfied by either
    # the background or the text, which are the only two colours guaranteed to
    # be on screen before the marker paints.
    #
    # The check stays specific: the colour is nonce-derived per launch, and a
    # near miss still has to cover the same fraction of the frame. What the
    # tolerance removes is the assumption that every renderer round-trips sRGB
    # byte-for-byte.
    $matchingPixels = 0L
    for ($offset = 0L; $offset -lt $BgraBytes.LongLength; $offset += 4) {
        if (
            [Math]::Abs([int]$BgraBytes[$offset] - $ExpectedBlue) -le $ChannelTolerance -and
            [Math]::Abs([int]$BgraBytes[$offset + 1] - $ExpectedGreen) -le $ChannelTolerance -and
            [Math]::Abs([int]$BgraBytes[$offset + 2] - $ExpectedRed) -le $ChannelTolerance
        ) {
            $matchingPixels++
            if ($matchingPixels -ge $required) {
                return $true
            }
        }
    }
    return $false
}

function Remove-KettlePerfStartupReadyScratch {
    [CmdletBinding(SupportsShouldProcess = $true)]
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor
    )

    [void](Assert-KettlePerfStartupReadyDescriptor $Descriptor)
    $scratchRoot = Get-KettlePerfStartupReadyFullPath $Descriptor.ScratchRoot
    if (-not (Test-Path -LiteralPath $scratchRoot)) {
        return $true
    }
    [void](Assert-KettlePerfStartupReadyDescriptor `
        -Descriptor $Descriptor -RequireScratchRoot)
    $allowed = [Collections.Generic.HashSet[string]]::new(
        $(if ([IO.Path]::DirectorySeparatorChar -eq '\') {
            [StringComparer]::OrdinalIgnoreCase
        } else {
            [StringComparer]::Ordinal
        })
    )
    foreach ($path in @(
        $Descriptor.GoPath,
        $Descriptor.GoStagePath,
        $Descriptor.ReadyPath,
        $Descriptor.ReadyStagePath
    )) {
        [void]$allowed.Add(
            (Get-KettlePerfStartupReadyFullPath $path)
        )
    }
    foreach ($entry in Get-ChildItem -LiteralPath $scratchRoot -Force) {
        $entryPath = Get-KettlePerfStartupReadyFullPath $entry.FullName
        if ($entry.PSIsContainer -or -not $allowed.Contains($entryPath)) {
            throw (
                'Startup-readiness cleanup found an unexpected scratch entry: ' +
                $entry.Name
            )
        }
    }
    if ($PSCmdlet.ShouldProcess(
        $scratchRoot,
        'Remove exact startup-readiness scratch files and empty root'
    )) {
        foreach ($path in $allowed) {
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                [IO.File]::Delete($path)
            }
        }
        [IO.Directory]::Delete($scratchRoot, $false)
    }
    return $true
}

function New-KettlePerfStartupReadyDescriptor {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates a bounded per-launch scratch descriptor.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string]$RunId,
        [Parameter(Mandatory = $true)]
        [string]$ScratchParent,
        [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
        [string]$SampleId = 'sample',
        [string]$PowerShellExecutable = '',
        [ValidateRange(1, 86400)]
        [int]$HoldSeconds = 86400,
        [ValidateRange(1, 300)]
        [int]$GoTimeoutSeconds = 30,
        [ValidateRange(100, 30000)]
        [int]$DsrTimeoutMs = 5000,
        [ValidateRange(48, 96)]
        [int]$MarkerColumns = 64,
        [ValidateRange(3, 12)]
        [int]$MarkerRows = 6
    )

    $canonicalRunId = ConvertTo-KettlePerfStartupReadyRunId $RunId
    $parent = Get-KettlePerfStartupReadyFullPath $ScratchParent
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        throw 'Startup-readiness scratch parent does not exist'
    }
    $powerShell = Resolve-KettlePerfStartupReadyPowerShell `
        $PowerShellExecutable
    $scriptPath = (
        Resolve-Path -LiteralPath (
            Join-Path $PSScriptRoot 'startup-ready.ps1'
        ) -ErrorAction Stop
    ).Path
    $scratchRoot = $null
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        $candidate = Join-Path $parent (
            'kettle-startup-ready-' +
            (New-KettlePerfStartupReadyRandomHex 16)
        )
        if (Test-Path -LiteralPath $candidate) {
            continue
        }
        try {
            $scratchRoot = (
                New-Item -ItemType Directory -Path $candidate `
                    -ErrorAction Stop
            ).FullName
            break
        } catch {
            if ($attempt -eq 7) {
                throw
            }
        }
    }
    if (-not $scratchRoot) {
        throw 'Could not create an unpredictable startup-readiness scratch root'
    }

    try {
        $launchId = New-KettlePerfStartupReadyRandomHex 16
        $nonce = New-KettlePerfStartupReadyRandomHex 32
        $red = 48 + (
            [Convert]::ToByte($nonce.Substring(0, 2), 16) % 176
        )
        $green = 48 + (
            [Convert]::ToByte($nonce.Substring(2, 2), 16) % 176
        )
        $blue = 48 + (
            [Convert]::ToByte($nonce.Substring(4, 2), 16) % 176
        )
        $goName = 'go-' + (New-KettlePerfStartupReadyRandomHex 16)
        $readyName = 'ready-' + (New-KettlePerfStartupReadyRandomHex 16)
        $descriptor = [pscustomobject][ordered]@{
            Schema = $script:KettlePerfStartupReadySchema
            RunId = $canonicalRunId
            SampleId = $SampleId
            LaunchId = $launchId
            Nonce = $nonce
            ScratchParent = $parent
            ScratchRoot = $scratchRoot
            GoPath = Join-Path $scratchRoot "$goName.marker"
            GoStagePath = Join-Path $scratchRoot (
                ".$goName.$(New-KettlePerfStartupReadyRandomHex 16).tmp"
            )
            ReadyPath = Join-Path $scratchRoot "$readyName.marker"
            ReadyStagePath = Join-Path $scratchRoot (
                ".$readyName.$(New-KettlePerfStartupReadyRandomHex 16).tmp"
            )
            MarkerRed = $red
            MarkerGreen = $green
            MarkerBlue = $blue
            MarkerTop = 1
            MarkerLeft = 1
            MarkerColumns = $MarkerColumns
            MarkerRows = $MarkerRows
            HoldSeconds = $HoldSeconds
            GoTimeoutSeconds = $GoTimeoutSeconds
            DsrTimeoutMs = $DsrTimeoutMs
            PowerShellExecutable = $powerShell
            ScriptPath = $scriptPath
            Arguments = [string[]]@()
            Command = [string[]]@()
        }
        [void](Assert-KettlePerfStartupReadyDescriptor `
            -Descriptor $descriptor -RequireScratchRoot)
        $payload = [ordered]@{
            schema = $descriptor.Schema
            run_id = $descriptor.RunId
            sample_id = $descriptor.SampleId
            launch_id = $descriptor.LaunchId
            nonce = $descriptor.Nonce
            scratch_parent = $descriptor.ScratchParent
            scratch_root = $descriptor.ScratchRoot
            go_path = $descriptor.GoPath
            go_stage_path = $descriptor.GoStagePath
            ready_path = $descriptor.ReadyPath
            ready_stage_path = $descriptor.ReadyStagePath
            marker_red = $descriptor.MarkerRed
            marker_green = $descriptor.MarkerGreen
            marker_blue = $descriptor.MarkerBlue
            marker_top = $descriptor.MarkerTop
            marker_left = $descriptor.MarkerLeft
            marker_columns = $descriptor.MarkerColumns
            marker_rows = $descriptor.MarkerRows
            hold_seconds = $descriptor.HoldSeconds
            go_timeout_seconds = $descriptor.GoTimeoutSeconds
            dsr_timeout_ms = $descriptor.DsrTimeoutMs
        }
        $payloadJson = $payload | ConvertTo-Json -Compress -Depth 3
        $payloadBytes = ConvertTo-KettlePerfStartupReadyUtf8 `
            -Text $payloadJson `
            -MaximumBytes $script:KettlePerfStartupReadyMaxPayloadBytes
        $payloadBase64 = [Convert]::ToBase64String($payloadBytes)
        $arguments = [string[]]@(
            '-NoLogo',
            '-NoProfile',
            '-NonInteractive',
            '-ExecutionPolicy',
            'Bypass',
            '-File',
            $scriptPath,
            '-ChildPayloadBase64',
            $payloadBase64
        )
        $descriptor.Arguments = $arguments
        $descriptor.Command = [string[]]@($powerShell) + $arguments
        return $descriptor
    } catch {
        if (
            $scratchRoot -and
            (Test-Path -LiteralPath $scratchRoot -PathType Container) -and
            @(Get-ChildItem -LiteralPath $scratchRoot -Force).Count -eq 0
        ) {
            [IO.Directory]::Delete($scratchRoot, $false)
        }
        throw
    }
}

function ConvertFrom-KettlePerfStartupReadyChildPayload {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PayloadBase64
    )

    if (
        $PayloadBase64.Length -eq 0 -or
        $PayloadBase64.Length -gt 8192 -or
        $PayloadBase64 -notmatch '^[A-Za-z0-9+/]+={0,2}$'
    ) {
        throw 'Startup-readiness child payload envelope is invalid'
    }
    try {
        $bytes = [Convert]::FromBase64String($PayloadBase64)
    } catch {
        throw 'Startup-readiness child payload is not valid base64'
    }
    if (
        $bytes.Length -eq 0 -or
        $bytes.Length -gt $script:KettlePerfStartupReadyMaxPayloadBytes
    ) {
        throw 'Startup-readiness child payload exceeds its size boundary'
    }
    if (
        $bytes.Length -ge 3 -and
        $bytes[0] -eq 0xef -and
        $bytes[1] -eq 0xbb -and
        $bytes[2] -eq 0xbf
    ) {
        throw 'Startup-readiness child payload must be UTF-8 without a BOM'
    }
    try {
        $json = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
        $payload = $json | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw 'Startup-readiness child payload is not strict UTF-8 JSON'
    }
    $expectedNames = @(
        'dsr_timeout_ms',
        'go_path',
        'go_stage_path',
        'go_timeout_seconds',
        'hold_seconds',
        'launch_id',
        'marker_blue',
        'marker_columns',
        'marker_green',
        'marker_left',
        'marker_red',
        'marker_rows',
        'marker_top',
        'nonce',
        'ready_path',
        'ready_stage_path',
        'run_id',
        'sample_id',
        'schema',
        'scratch_parent',
        'scratch_root'
    )
    $actualNames = @($payload.PSObject.Properties.Name | Sort-Object)
    if (($actualNames -join "`0") -cne ($expectedNames -join "`0")) {
        throw 'Startup-readiness child payload fields are invalid'
    }
    $descriptor = [pscustomobject][ordered]@{
        Schema = [string]$payload.schema
        RunId = [string]$payload.run_id
        SampleId = [string]$payload.sample_id
        LaunchId = [string]$payload.launch_id
        Nonce = [string]$payload.nonce
        ScratchParent = [string]$payload.scratch_parent
        ScratchRoot = [string]$payload.scratch_root
        GoPath = [string]$payload.go_path
        GoStagePath = [string]$payload.go_stage_path
        ReadyPath = [string]$payload.ready_path
        ReadyStagePath = [string]$payload.ready_stage_path
        MarkerRed = [int]$payload.marker_red
        MarkerGreen = [int]$payload.marker_green
        MarkerBlue = [int]$payload.marker_blue
        MarkerTop = [int]$payload.marker_top
        MarkerLeft = [int]$payload.marker_left
        MarkerColumns = [int]$payload.marker_columns
        MarkerRows = [int]$payload.marker_rows
        HoldSeconds = [int]$payload.hold_seconds
        GoTimeoutSeconds = [int]$payload.go_timeout_seconds
        DsrTimeoutMs = [int]$payload.dsr_timeout_ms
    }
    [void](Assert-KettlePerfStartupReadyDescriptor `
        -Descriptor $descriptor -RequireScratchRoot)
    return $descriptor
}

function Wait-KettlePerfStartupReadyGo {
    param(
        [Parameter(Mandatory = $true)]
        $Descriptor
    )

    $timer = [Diagnostics.Stopwatch]::StartNew()
    $timeoutMs = [int64]$Descriptor.GoTimeoutSeconds * 1000
    while ($timer.ElapsedMilliseconds -lt $timeoutMs) {
        if (Test-Path -LiteralPath $Descriptor.GoPath -PathType Leaf) {
            if (Test-KettlePerfStartupReadyGo $Descriptor) {
                return $true
            }
            throw 'Startup-readiness GO marker exists but is invalid'
        }
        Start-Sleep -Milliseconds 10
    }
    throw 'Startup-readiness child timed out waiting for GO'
}

function Wait-KettlePerfStartupReadyDsrResponse {
    param(
        [ValidateRange(100, 30000)]
        [int]$TimeoutMs = 5000
    )

    $expected = [int[]]@(27, 91, 48, 110)
    $matched = 0
    $seen = 0
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $redirected = [Console]::IsInputRedirected
    $inputStream = if ($redirected) {
        [Console]::OpenStandardInput()
    } else {
        $null
    }
    $buffer = [byte[]]::new(1)
    $readTask = $null
    while ($timer.ElapsedMilliseconds -lt $TimeoutMs) {
        $character = $null
        if ($redirected) {
            if ($null -eq $readTask) {
                $readTask = $inputStream.ReadAsync($buffer, 0, 1)
            }
            if (-not $readTask.Wait(10)) {
                continue
            }
            if ($readTask.Result -eq 0) {
                throw 'Startup-readiness DSR input closed before CSI 0n'
            }
            $character = [int]$buffer[0]
            $readTask = $null
        } else {
            if (-not [Console]::KeyAvailable) {
                Start-Sleep -Milliseconds 5
                continue
            }
            $character = [int][Console]::ReadKey($true).KeyChar
        }
        $seen++
        if ($seen -gt 64) {
            throw 'Startup-readiness DSR response exceeded 64 characters'
        }
        if ($character -eq $expected[$matched]) {
            $matched++
            if ($matched -eq $expected.Length) {
                return $true
            }
        } elseif ($character -eq $expected[0]) {
            $matched = 1
        } else {
            $matched = 0
        }
    }
    throw 'Startup-readiness terminal did not answer CSI 5n with CSI 0n'
}

function Invoke-KettlePerfStartupReadyChild {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PayloadBase64
    )

    $ErrorActionPreference = 'Stop'
    if ($PSVersionTable.PSVersion.Major -lt 7) {
        throw 'Startup-readiness child requires PowerShell 7 or newer'
    }
    $descriptor = ConvertFrom-KettlePerfStartupReadyChildPayload `
        $PayloadBase64
    [void](Wait-KettlePerfStartupReadyGo $descriptor)

    $output = [Console]::OpenStandardOutput()
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $paintText = Get-KettlePerfStartupReadyPaintText $descriptor
    $paintBytes = $encoding.GetBytes($paintText)
    if ($paintBytes.Length -gt $script:KettlePerfStartupReadyMaxPaintBytes) {
        throw 'Startup-readiness paint sequence exceeds its size boundary'
    }
    $output.Write($paintBytes, 0, $paintBytes.Length)
    $output.Flush()

    $dsrBytes = [byte[]]@(27, 91, 53, 110)
    $output.Write($dsrBytes, 0, $dsrBytes.Length)
    $output.Flush()
    [void](Wait-KettlePerfStartupReadyDsrResponse `
        -TimeoutMs $descriptor.DsrTimeoutMs)

    $readyText = Get-KettlePerfStartupReadyMarkerText `
        -Descriptor $descriptor -Kind ready
    $readyBytes = ConvertTo-KettlePerfStartupReadyUtf8 `
        -Text $readyText `
        -MaximumBytes $script:KettlePerfStartupReadyMaxMarkerBytes
    Write-KettlePerfStartupReadyAtomicFile `
        -Descriptor $descriptor -Path $descriptor.ReadyPath `
        -StagePath $descriptor.ReadyStagePath -Bytes $readyBytes

    [Threading.Thread]::Sleep($descriptor.HoldSeconds * 1000)
}

if ($ChildPayloadBase64) {
    try {
        Invoke-KettlePerfStartupReadyChild `
            -PayloadBase64 $ChildPayloadBase64
        exit 0
    } catch {
        [Console]::Error.WriteLine(
            'startup-readiness child failed: ' + $_.Exception.Message
        )
        exit 70
    }
}
