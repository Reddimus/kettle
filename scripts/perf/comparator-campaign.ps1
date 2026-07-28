# Strict provenance contract for the five Windows comparator terminals used by
# release benchmarking. A campaign is immutable evidence: its id is bound to
# its UTC selection time and to an exact root-relative path, while every peer
# is pinned to an official package, a stable release tag, and a byte identity.

Set-StrictMode -Version Latest

. "$PSScriptRoot\evidence-snapshot.ps1"

function Get-KettlePerfComparatorCampaignOfficialEntry {
    param(
        [Parameter(Mandatory)]
        [string]$Name
    )

    switch -CaseSensitive ($Name) {
        'alacritty' {
            return [pscustomobject][ordered]@{
                name = 'alacritty'
                role = 'confirmed'
                origin = 'https://github.com/alacritty/alacritty'
                package = 'Alacritty.Alacritty'
                executable_leaf = 'alacritty.exe'
                version_kind = 'semver'
                tag_prefix = 'v'
            }
        }
        'wezterm' {
            return [pscustomobject][ordered]@{
                name = 'wezterm'
                role = 'confirmed'
                origin = 'https://github.com/wezterm/wezterm'
                package = 'wez.wezterm'
                executable_leaf = 'wezterm-gui.exe'
                version_kind = 'wezterm-stable'
                tag_prefix = ''
            }
        }
        'rio' {
            return [pscustomobject][ordered]@{
                name = 'rio'
                role = 'confirmed'
                origin = 'https://github.com/raphamorim/rio'
                package = 'raphamorim.rio'
                executable_leaf = 'rio.exe'
                version_kind = 'semver'
                tag_prefix = 'v'
            }
        }
        'tabby' {
            return [pscustomobject][ordered]@{
                name = 'tabby'
                role = 'confirmed'
                origin = 'https://github.com/Eugeny/tabby'
                package = 'Eugeny.Tabby'
                executable_leaf = 'Tabby.exe'
                version_kind = 'semver'
                tag_prefix = 'v'
            }
        }
        'wt' {
            return [pscustomobject][ordered]@{
                name = 'wt'
                role = 'advisory'
                origin = 'https://github.com/microsoft/terminal'
                package = 'Microsoft.WindowsTerminal'
                executable_leaf = 'WindowsTerminal.exe'
                version_kind = 'windows-terminal'
                tag_prefix = 'v'
            }
        }
        default {
            throw "Comparator campaign terminal is not allowed: $Name"
        }
    }
}

function Get-KettlePerfComparatorCampaignExpectedAsset {
    param(
        [Parameter(Mandatory)]
        [string]$Name,
        [Parameter(Mandatory)]
        [string]$Version,
        [Parameter(Mandatory)]
        [string]$ReleaseTag
    )

    $origin = (Get-KettlePerfComparatorCampaignOfficialEntry $Name).origin
    $assetKind = $null
    $assetName = $null
    $executableEntry = $null
    switch -CaseSensitive ($Name) {
        'alacritty' {
            $assetKind = 'direct-executable'
            $assetName = "Alacritty-v$Version-portable.exe"
            $executableEntry = $assetName
            break
        }
        'wezterm' {
            $assetKind = 'zip'
            $assetBase = "WezTerm-windows-$Version"
            $assetName = "$assetBase.zip"
            $executableEntry = "$assetBase/wezterm-gui.exe"
            break
        }
        'rio' {
            $assetKind = 'direct-executable'
            $assetName = 'rio-portable-x86_64.exe'
            $executableEntry = $assetName
            break
        }
        'tabby' {
            $assetKind = 'zip'
            $assetName = "tabby-$Version-portable-x64.zip"
            $executableEntry = 'Tabby.exe'
            break
        }
        'wt' {
            $assetKind = 'zip'
            $assetName = "Microsoft.WindowsTerminal_$($Version)_x64.zip"
            $executableEntry = (
                "terminal-$Version/WindowsTerminal.exe"
            )
            break
        }
        default {
            throw "Comparator campaign terminal is not allowed: $Name"
        }
    }
    return [pscustomobject][ordered]@{
        kind = $assetKind
        name = $assetName
        url = "$origin/releases/download/$ReleaseTag/$assetName"
        executable_entry = $executableEntry
    }
}

function Get-KettlePerfComparatorObjectProperty {
    param(
        [AllowNull()]
        $Object,
        [Parameter(Mandatory)]
        [string]$Name
    )

    if ($null -eq $Object) {
        return [pscustomobject]@{
            found = $false
            value = $null
        }
    }
    if ($Object -is [Collections.IDictionary]) {
        foreach ($key in $Object.Keys) {
            if (
                $key -is [string] -and
                [StringComparer]::Ordinal.Equals([string]$key, $Name)
            ) {
                return [pscustomobject]@{
                    found = $true
                    value = $Object[$key]
                }
            }
        }
        return [pscustomobject]@{
            found = $false
            value = $null
        }
    }
    foreach ($property in $Object.PSObject.Properties) {
        if ([StringComparer]::Ordinal.Equals($property.Name, $Name)) {
            return [pscustomobject]@{
                found = $true
                value = $property.Value
            }
        }
    }
    return [pscustomobject]@{
        found = $false
        value = $null
    }
}

function Get-KettlePerfComparatorRequiredProperty {
    param(
        [AllowNull()]
        $Object,
        [Parameter(Mandatory)]
        [string]$Name,
        [Parameter(Mandatory)]
        [string]$Context
    )

    $property = Get-KettlePerfComparatorObjectProperty `
        -Object $Object -Name $Name
    if (-not [bool]$property.found) {
        throw "$Context is missing required property '$Name'"
    }
    return $property.value
}

function Assert-KettlePerfComparatorExactObject {
    param(
        [AllowNull()]
        $Value,
        [Parameter(Mandatory)]
        [string[]]$Properties,
        [Parameter(Mandatory)]
        [string]$Context
    )

    if (
        $null -eq $Value -or
        $Value -is [string] -or
        $Value -is [Array] -or
        $Value -is [Collections.IDictionary]
    ) {
        throw "$Context must be one JSON object"
    }
    $actual = @($Value.PSObject.Properties | ForEach-Object { $_.Name })
    if ($actual.Count -ne $Properties.Count) {
        throw "$Context has missing or unknown properties"
    }
    $actualNames = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    foreach ($name in $actual) {
        if (-not $actualNames.Add([string]$name)) {
            throw "$Context has duplicate properties"
        }
    }
    foreach ($name in $Properties) {
        if (-not $actualNames.Contains($name)) {
            throw "$Context has missing or unknown properties"
        }
    }
}

function Assert-KettlePerfComparatorString {
    param(
        [AllowNull()]
        $Value,
        [Parameter(Mandatory)]
        [string]$Context,
        [ValidateRange(1, 2048)]
        [int]$MaximumLength = 256
    )

    if (
        $Value -isnot [string] -or
        $Value.Length -lt 1 -or
        $Value.Length -gt $MaximumLength -or
        $Value.IndexOf([char]0) -ge 0
    ) {
        $actualType = if ($null -eq $Value) {
            'null'
        } else {
            $Value.GetType().FullName
        }
        throw (
            "$Context must be a non-empty bounded string " +
            "(actual type: $actualType)"
        )
    }
}

function Test-KettlePerfComparatorInteger {
    param(
        [AllowNull()]
        $Value
    )

    if ($null -eq $Value) {
        return $false
    }
    return $Value.GetType() -in @(
        [byte],
        [sbyte],
        [int16],
        [uint16],
        [int32],
        [uint32],
        [int64],
        [uint64]
    )
}

function ConvertFrom-KettlePerfComparatorTimestamp {
    param(
        [AllowNull()]
        $Value,
        [Parameter(Mandatory)]
        [string]$Context
    )

    Assert-KettlePerfComparatorString -Value $Value -Context $Context `
        -MaximumLength 20
    if ($Value -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$') {
        throw "$Context must use canonical whole-second UTC"
    }
    $parsed = [DateTimeOffset]::MinValue
    if (-not [DateTimeOffset]::TryParseExact(
        $Value,
        "yyyy-MM-dd'T'HH:mm:ss'Z'",
        [Globalization.CultureInfo]::InvariantCulture,
        (
            [Globalization.DateTimeStyles]::AssumeUniversal -bor
            [Globalization.DateTimeStyles]::AdjustToUniversal
        ),
        [ref]$parsed
    )) {
        throw "$Context is not a valid UTC timestamp"
    }
    if ($parsed.Year -lt 2020 -or $parsed.Year -gt 9998) {
        throw "$Context is outside the supported campaign range"
    }
    return $parsed
}

function Get-KettlePerfComparatorRawTimestamp {
    param(
        [Parameter(Mandatory)]
        [string]$Json,
        [Parameter(Mandatory)]
        [ValidateSet('started_at_utc', 'completed_at_utc')]
        [string]$Property,
        [Parameter(Mandatory)]
        [string]$Context
    )

    # PowerShell 7 converts ISO JSON strings to DateTime while Windows
    # PowerShell 5.1 preserves strings. Inspect the already shape-validated raw
    # token so both engines enforce the same spelling, including no escapes,
    # fractional seconds, or numeric offsets.
    $pattern = (
        '"' + [regex]::Escape($Property) +
        '"\s*:\s*"(?<value>(?:[^"\\\x00-\x1f]|' +
        '\\["\\/bfnrt]|\\u[0-9A-Fa-f]{4})*)"'
    )
    $rawTimestampMatches = [regex]::Matches(
        $Json,
        $pattern,
        [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if ($rawTimestampMatches.Count -ne 1) {
        throw "$Context must have one exact JSON string token"
    }
    $value = $rawTimestampMatches[0].Groups['value'].Value
    if ($value.IndexOf('\') -ge 0) {
        throw "$Context must not use JSON escapes"
    }
    [void](ConvertFrom-KettlePerfComparatorTimestamp $value $Context)
    return $value
}

function Test-KettlePerfComparatorStableVersion {
    param(
        [Parameter(Mandatory)]
        [string]$Version,
        [Parameter(Mandatory)]
        [string]$Kind
    )

    switch -CaseSensitive ($Kind) {
        'semver' {
            return $Version -cmatch (
                '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.' +
                '(0|[1-9][0-9]*)$'
            )
        }
        'windows-terminal' {
            return $Version -cmatch (
                '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.' +
                '(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
            )
        }
        'wezterm-stable' {
            if (
                $Version -cnotmatch
                    '^([0-9]{8})-([0-9]{6})-([0-9a-f]{8})$'
            ) {
                return $false
            }
            $releaseTime = [datetime]::MinValue
            return [datetime]::TryParseExact(
                "$($Matches[1])-$($Matches[2])",
                'yyyyMMdd-HHmmss',
                [Globalization.CultureInfo]::InvariantCulture,
                [Globalization.DateTimeStyles]::None,
                [ref]$releaseTime
            )
        }
        default {
            return $false
        }
    }
}

function Test-KettlePerfComparatorSafeStagingPath {
    param(
        [AllowNull()]
        $Value,
        [Parameter(Mandatory)]
        [string]$Name,
        [Parameter(Mandatory)]
        [string]$Version,
        [Parameter(Mandatory)]
        [string]$Leaf
    )

    if (
        $Value -isnot [string] -or
        $Value.Length -lt 1 -or
        $Value.Length -gt 240 -or
        $Value.IndexOf('\') -ge 0 -or
        $Value.IndexOf(':') -ge 0 -or
        $Value.IndexOfAny([char[]]"`0`r`n`t*?`"<>|") -ge 0 -or
        $Value.StartsWith('/', [StringComparison]::Ordinal) -or
        $Value.EndsWith('/', [StringComparison]::Ordinal)
    ) {
        return $false
    }
    $parts = [string[]]$Value.Split(
        [char[]]@('/'),
        [StringSplitOptions]::None
    )
    if (
        $parts.Count -ne 4 -or
        -not [StringComparer]::Ordinal.Equals($parts[0], 'staging') -or
        -not [StringComparer]::Ordinal.Equals($parts[1], $Name) -or
        -not [StringComparer]::Ordinal.Equals($parts[2], $Version) -or
        -not [StringComparer]::Ordinal.Equals($parts[3], $Leaf)
    ) {
        return $false
    }
    foreach ($part in $parts) {
        if (
            $part.Length -lt 1 -or
            $part.Length -gt 128 -or
            $part -in @('.', '..') -or
            $part.EndsWith('.', [StringComparison]::Ordinal) -or
            $part.EndsWith(' ', [StringComparison]::Ordinal) -or
            $part -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$'
        ) {
            return $false
        }
        $deviceBase = ($part -split '\.', 2)[0]
        if (
            $deviceBase -cmatch '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$'
        ) {
            return $false
        }
    }
    return $true
}

function Test-KettlePerfComparatorPathHasNoAlternateStream {
    param(
        [Parameter(Mandatory)]
        [string]$FullPath
    )

    $pathRoot = [IO.Path]::GetPathRoot($FullPath)
    if (-not $pathRoot -or $FullPath.Length -lt $pathRoot.Length) {
        return $false
    }
    return $FullPath.Substring($pathRoot.Length).IndexOf(':') -lt 0
}

function Copy-KettlePerfComparatorCampaignEntry {
    param(
        [Parameter(Mandatory)]
        $Entry
    )

    return [pscustomobject][ordered]@{
        campaign_id = [string]$Entry.campaign_id
        campaign_sha256 = [string]$Entry.campaign_sha256
        selection_policy = [string]$Entry.selection_policy
        name = [string]$Entry.name
        role = [string]$Entry.role
        version = [string]$Entry.version
        source = [pscustomobject][ordered]@{
            origin = [string]$Entry.source.origin
            package = [string]$Entry.source.package
            release_tag = [string]$Entry.source.release_tag
            asset = [pscustomobject][ordered]@{
                kind = [string]$Entry.source.asset.kind
                name = [string]$Entry.source.asset.name
                url = [string]$Entry.source.asset.url
                bytes = [long]$Entry.source.asset.bytes
                sha256 = [string]$Entry.source.asset.sha256
                executable_entry = (
                    [string]$Entry.source.asset.executable_entry
                )
                staged_file_count = (
                    [int]$Entry.source.asset.staged_file_count
                )
                staged_total_bytes = (
                    [long]$Entry.source.asset.staged_total_bytes
                )
                staged_tree_sha256 = (
                    [string]$Entry.source.asset.staged_tree_sha256
                )
            }
        }
        executable = [pscustomobject][ordered]@{
            leaf = [string]$Entry.executable.leaf
            staging_path = [string]$Entry.executable.staging_path
            bytes = [long]$Entry.executable.bytes
            sha256 = [string]$Entry.executable.sha256
            authenticode_status = (
                [string]$Entry.executable.authenticode_status
            )
            signer_cert_sha256 = $Entry.executable.signer_cert_sha256
        }
    }
}

function Read-KettlePerfComparatorCampaign {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$ExpectedCampaignRoot
    )

    $requestedRootPath = [IO.Path]::GetFullPath($ExpectedCampaignRoot)
    if (-not (
        Test-KettlePerfComparatorPathHasNoAlternateStream $requestedRootPath
    )) {
        throw 'Comparator campaign root must not name an alternate data stream'
    }
    $rootItem = Get-Item -LiteralPath $ExpectedCampaignRoot -Force `
        -ErrorAction Stop
    if (
        -not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'Comparator campaign root must be an ordinary directory'
    }
    $rootFullPath = [IO.Path]::GetFullPath($rootItem.FullName)
    $fileSystemRoot = [IO.Path]::GetPathRoot($rootFullPath)
    if ([StringComparer]::OrdinalIgnoreCase.Equals(
        $rootFullPath.TrimEnd('\', '/'),
        $fileSystemRoot.TrimEnd('\', '/')
    )) {
        throw 'Comparator campaign root must not be a filesystem root'
    }
    $rootPath = $rootFullPath.TrimEnd('\', '/')
    if (-not (Test-KettlePerfComparatorPathHasNoAlternateStream $rootPath)) {
        throw 'Comparator campaign root must not name an alternate data stream'
    }

    $requestedManifestPath = [IO.Path]::GetFullPath($Path)
    if (-not (
        Test-KettlePerfComparatorPathHasNoAlternateStream `
            $requestedManifestPath
    )) {
        throw 'Comparator campaign manifest must not name an alternate data stream'
    }
    $pathItem = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (
        $pathItem.PSIsContainer -or
        ($pathItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'Comparator campaign manifest must be an ordinary file'
    }
    $fullPath = [IO.Path]::GetFullPath($pathItem.FullName)
    if (-not (Test-KettlePerfComparatorPathHasNoAlternateStream $fullPath)) {
        throw 'Comparator campaign manifest must not name an alternate data stream'
    }
    if (
        -not [StringComparer]::Ordinal.Equals(
            [IO.Path]::GetFileName($fullPath),
            'campaign.json'
        )
    ) {
        throw 'Comparator campaign manifest leaf must be campaign.json'
    }
    $campaignDirectory = [IO.Path]::GetDirectoryName($fullPath)
    $campaignDirectoryItem = Get-Item -LiteralPath $campaignDirectory `
        -Force -ErrorAction Stop
    if (
        -not $campaignDirectoryItem.PSIsContainer -or
        ($campaignDirectoryItem.Attributes -band
            [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'Comparator campaign directory must be ordinary'
    }
    $campaignDirectory = [IO.Path]::GetFullPath(
        $campaignDirectoryItem.FullName
    ).TrimEnd('\', '/')
    $campaignParent = [IO.Directory]::GetParent($campaignDirectory)
    if (
        $null -eq $campaignParent -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            $campaignParent.FullName.TrimEnd('\', '/'),
            $rootPath
        )
    ) {
        throw 'Comparator campaign must be one direct append-only root child'
    }
    $pathCampaignId = [IO.Path]::GetFileName($campaignDirectory)
    if (
        $pathCampaignId -cnotmatch (
            '^windows-x86_64-[0-9]{8}T[0-9]{6}Z-' +
            '[0-9a-f]{16}$'
        )
    ) {
        throw 'Comparator campaign directory has an invalid campaign id'
    }

    $snapshot = $null
    try {
        $snapshot = Open-KettlePerfEvidenceSnapshot `
            -Directory $campaignDirectory -MaximumFiles 1 `
            -MaximumTotalBytes 65536
        $file = Read-KettlePerfEvidenceJson `
            -Snapshot $snapshot -LeafName 'campaign.json' `
            -MaximumBytes 65536 -MaximumDepth 8 -MaximumNodes 512 -Required
        $raw = $file.value

        Assert-KettlePerfComparatorExactObject -Value $raw -Properties @(
            'schema',
            'campaign_id',
            'platform',
            'selection',
            'terminals'
        ) -Context 'Comparator campaign'
        $schema = Get-KettlePerfComparatorRequiredProperty `
            $raw 'schema' 'Comparator campaign'
        Assert-KettlePerfComparatorString $schema 'Comparator campaign schema'
        if ($schema -cne 'kettle-windows-comparator-campaign-v1') {
            throw 'Comparator campaign schema is unsupported'
        }
        $campaignId = Get-KettlePerfComparatorRequiredProperty `
            $raw 'campaign_id' 'Comparator campaign'
        Assert-KettlePerfComparatorString `
            $campaignId 'Comparator campaign id' 96
        if (
            $campaignId -cnotmatch (
                '^windows-x86_64-([0-9]{8}T[0-9]{6}Z)-' +
                '([0-9a-f]{16})$'
            ) -or
            $Matches[2] -ceq '0000000000000000' -or
            -not [StringComparer]::Ordinal.Equals(
                $campaignId,
                $pathCampaignId
            )
        ) {
            throw 'Comparator campaign id does not match its append-only path'
        }
        $campaignTimestamp = $Matches[1]

        $platform = Get-KettlePerfComparatorRequiredProperty `
            $raw 'platform' 'Comparator campaign'
        Assert-KettlePerfComparatorExactObject -Value $platform `
            -Properties @('os', 'architecture') `
            -Context 'Comparator campaign platform'
        $platformOs = Get-KettlePerfComparatorRequiredProperty `
            $platform 'os' 'Comparator campaign platform'
        $platformArchitecture = Get-KettlePerfComparatorRequiredProperty `
            $platform 'architecture' 'Comparator campaign platform'
        if (
            $platformOs -isnot [string] -or
            $platformArchitecture -isnot [string] -or
            $platformOs -cne 'windows' -or
            $platformArchitecture -cne 'x86_64'
        ) {
            throw 'Comparator campaign platform must be Windows x86_64'
        }

        $selection = Get-KettlePerfComparatorRequiredProperty `
            $raw 'selection' 'Comparator campaign'
        Assert-KettlePerfComparatorExactObject -Value $selection `
            -Properties @('policy', 'started_at_utc', 'completed_at_utc') `
            -Context 'Comparator campaign selection'
        $selectionPolicy = Get-KettlePerfComparatorRequiredProperty `
            $selection 'policy' 'Comparator campaign selection'
        if (
            $selectionPolicy -isnot [string] -or
            $selectionPolicy -cne 'official-stable-pinned-assets-v1'
        ) {
            throw 'Comparator campaign selection policy is unsupported'
        }
        [void](Get-KettlePerfComparatorRequiredProperty `
            $selection 'started_at_utc' 'Comparator campaign selection'
        )
        [void](Get-KettlePerfComparatorRequiredProperty `
            $selection 'completed_at_utc' 'Comparator campaign selection'
        )
        $startedText = Get-KettlePerfComparatorRawTimestamp `
            -Json $file.text -Property 'started_at_utc' `
            -Context 'Comparator campaign start timestamp'
        $completedText = Get-KettlePerfComparatorRawTimestamp `
            -Json $file.text -Property 'completed_at_utc' `
            -Context 'Comparator campaign completion timestamp'
        $started = ConvertFrom-KettlePerfComparatorTimestamp `
            $startedText 'Comparator campaign start timestamp'
        $completed = ConvertFrom-KettlePerfComparatorTimestamp `
            $completedText 'Comparator campaign completion timestamp'
        if ($completed -lt $started) {
            throw 'Comparator campaign completed before it started'
        }
        $expectedIdTimestamp = $started.UtcDateTime.ToString(
            'yyyyMMddTHHmmssZ',
            [Globalization.CultureInfo]::InvariantCulture
        )
        if ($campaignTimestamp -cne $expectedIdTimestamp) {
            throw 'Comparator campaign id timestamp does not match selection'
        }

        $rawTerminals = Get-KettlePerfComparatorRequiredProperty `
            $raw 'terminals' 'Comparator campaign'
        if ($rawTerminals -isnot [Array] -or $rawTerminals.Count -ne 5) {
            throw 'Comparator campaign must contain exactly five terminals'
        }
        $expectedNames = [string[]]@(
            'alacritty',
            'wezterm',
            'rio',
            'tabby',
            'wt'
        )
        $normalizedTerminals = [Collections.Generic.List[object]]::new()
        $stagingPaths = [Collections.Generic.HashSet[string]]::new(
            [StringComparer]::OrdinalIgnoreCase
        )
        for ($index = 0; $index -lt $expectedNames.Count; $index++) {
            $context = "Comparator campaign terminal $index"
            $rawTerminal = $rawTerminals[$index]
            Assert-KettlePerfComparatorExactObject -Value $rawTerminal `
                -Properties @(
                    'name',
                    'role',
                    'version',
                    'source',
                    'executable'
                ) -Context $context
            $name = Get-KettlePerfComparatorRequiredProperty `
                $rawTerminal 'name' $context
            if (
                $name -isnot [string] -or
                $name -cne $expectedNames[$index]
            ) {
                throw 'Comparator campaign terminal order or name is invalid'
            }
            $official = Get-KettlePerfComparatorCampaignOfficialEntry $name
            $role = Get-KettlePerfComparatorRequiredProperty `
                $rawTerminal 'role' $context
            if ($role -isnot [string] -or $role -cne $official.role) {
                throw "Comparator campaign role is invalid for $name"
            }
            $version = Get-KettlePerfComparatorRequiredProperty `
                $rawTerminal 'version' $context
            Assert-KettlePerfComparatorString `
                $version "Comparator campaign version for $name" 64
            if (-not (Test-KettlePerfComparatorStableVersion `
                -Version $version -Kind $official.version_kind
            )) {
                throw "Comparator campaign version is not stable for $name"
            }

            $source = Get-KettlePerfComparatorRequiredProperty `
                $rawTerminal 'source' $context
            Assert-KettlePerfComparatorExactObject -Value $source `
                -Properties @('origin', 'package', 'release_tag', 'asset') `
                -Context "Comparator campaign source for $name"
            $origin = Get-KettlePerfComparatorRequiredProperty `
                $source 'origin' "Comparator campaign source for $name"
            $package = Get-KettlePerfComparatorRequiredProperty `
                $source 'package' "Comparator campaign source for $name"
            $tag = Get-KettlePerfComparatorRequiredProperty `
                $source 'release_tag' "Comparator campaign source for $name"
            if (
                $origin -isnot [string] -or
                $package -isnot [string] -or
                $tag -isnot [string] -or
                $origin -cne $official.origin -or
                $package -cne $official.package -or
                $tag -cne "$($official.tag_prefix)$version"
            ) {
                throw "Comparator campaign source is not official for $name"
            }
            $asset = Get-KettlePerfComparatorRequiredProperty `
                $source 'asset' "Comparator campaign source for $name"
            Assert-KettlePerfComparatorExactObject -Value $asset `
                -Properties @(
                    'kind',
                    'name',
                    'url',
                    'bytes',
                    'sha256',
                    'executable_entry',
                    'staged_file_count',
                    'staged_total_bytes',
                    'staged_tree_sha256'
                ) -Context "Comparator campaign asset for $name"
            $assetKind = Get-KettlePerfComparatorRequiredProperty `
                $asset 'kind' "Comparator campaign asset for $name"
            $assetName = Get-KettlePerfComparatorRequiredProperty `
                $asset 'name' "Comparator campaign asset for $name"
            $assetUrl = Get-KettlePerfComparatorRequiredProperty `
                $asset 'url' "Comparator campaign asset for $name"
            $assetBytes = Get-KettlePerfComparatorRequiredProperty `
                $asset 'bytes' "Comparator campaign asset for $name"
            $assetSha256 = Get-KettlePerfComparatorRequiredProperty `
                $asset 'sha256' "Comparator campaign asset for $name"
            $assetExecutableEntry = Get-KettlePerfComparatorRequiredProperty `
                $asset 'executable_entry' `
                "Comparator campaign asset for $name"
            $stagedFileCount = Get-KettlePerfComparatorRequiredProperty `
                $asset 'staged_file_count' `
                "Comparator campaign asset for $name"
            $stagedTotalBytes = Get-KettlePerfComparatorRequiredProperty `
                $asset 'staged_total_bytes' `
                "Comparator campaign asset for $name"
            $stagedTreeSha256 = Get-KettlePerfComparatorRequiredProperty `
                $asset 'staged_tree_sha256' `
                "Comparator campaign asset for $name"
            $expectedAsset = Get-KettlePerfComparatorCampaignExpectedAsset `
                -Name $name -Version $version -ReleaseTag $tag
            if (
                $assetKind -isnot [string] -or
                $assetName -isnot [string] -or
                $assetUrl -isnot [string] -or
                $assetExecutableEntry -isnot [string] -or
                $assetKind -cne $expectedAsset.kind -or
                $assetName -cne $expectedAsset.name -or
                $assetUrl -cne $expectedAsset.url -or
                $assetExecutableEntry -cne
                    $expectedAsset.executable_entry -or
                -not (Test-KettlePerfComparatorInteger $assetBytes) -or
                [decimal]$assetBytes -lt 1 -or
                [decimal]$assetBytes -gt 4294967296 -or
                $assetSha256 -isnot [string] -or
                $assetSha256 -cnotmatch '^[0-9a-f]{64}$' -or
                -not (Test-KettlePerfComparatorInteger $stagedFileCount) -or
                [decimal]$stagedFileCount -lt 1 -or
                [decimal]$stagedFileCount -gt 4096 -or
                -not (Test-KettlePerfComparatorInteger $stagedTotalBytes) -or
                [decimal]$stagedTotalBytes -lt 1 -or
                [decimal]$stagedTotalBytes -gt 8589934592 -or
                $stagedTreeSha256 -isnot [string] -or
                $stagedTreeSha256 -cnotmatch '^[0-9a-f]{64}$'
            ) {
                throw "Comparator campaign asset is invalid for $name"
            }

            $executable = Get-KettlePerfComparatorRequiredProperty `
                $rawTerminal 'executable' $context
            Assert-KettlePerfComparatorExactObject -Value $executable `
                -Properties @(
                    'leaf',
                    'staging_path',
                    'bytes',
                    'sha256',
                    'authenticode_status',
                    'signer_cert_sha256'
                ) `
                -Context "Comparator campaign executable for $name"
            $leaf = Get-KettlePerfComparatorRequiredProperty `
                $executable 'leaf' "Comparator campaign executable for $name"
            $stagingPath = Get-KettlePerfComparatorRequiredProperty `
                $executable 'staging_path' `
                "Comparator campaign executable for $name"
            $bytes = Get-KettlePerfComparatorRequiredProperty `
                $executable 'bytes' "Comparator campaign executable for $name"
            $sha256 = Get-KettlePerfComparatorRequiredProperty `
                $executable 'sha256' "Comparator campaign executable for $name"
            $authenticodeStatus = Get-KettlePerfComparatorRequiredProperty `
                $executable 'authenticode_status' `
                "Comparator campaign executable for $name"
            $signerCertSha256 = Get-KettlePerfComparatorRequiredProperty `
                $executable 'signer_cert_sha256' `
                "Comparator campaign executable for $name"
            if (
                $leaf -isnot [string] -or
                $leaf -cne $official.executable_leaf
            ) {
                throw "Comparator campaign executable leaf is invalid for $name"
            }
            if (-not (Test-KettlePerfComparatorSafeStagingPath `
                -Value $stagingPath -Name $name -Version $version -Leaf $leaf
            )) {
                throw "Comparator campaign staging path is unsafe for $name"
            }
            if (-not $stagingPaths.Add($stagingPath)) {
                throw 'Comparator campaign staging paths must be unique'
            }
            if (
                -not (Test-KettlePerfComparatorInteger $bytes) -or
                [decimal]$bytes -lt 1 -or
                [decimal]$bytes -gt 4294967296
            ) {
                throw "Comparator campaign executable size is invalid for $name"
            }
            if (
                $sha256 -isnot [string] -or
                $sha256 -cnotmatch '^[0-9a-f]{64}$'
            ) {
                throw "Comparator campaign executable hash is invalid for $name"
            }
            if ([decimal]$stagedTotalBytes -lt [decimal]$bytes) {
                throw (
                    'Comparator campaign staged tree is smaller than its ' +
                    "executable for $name"
                )
            }
            $isValidSignature = (
                $authenticodeStatus -is [string] -and
                $authenticodeStatus -ceq 'Valid'
            )
            $isUnsigned = (
                $authenticodeStatus -is [string] -and
                $authenticodeStatus -ceq 'NotSigned'
            )
            if (
                (-not $isValidSignature -and -not $isUnsigned) -or
                (
                    $isValidSignature -and
                    (
                        $signerCertSha256 -isnot [string] -or
                        $signerCertSha256 -cnotmatch '^[0-9a-f]{64}$'
                    )
                ) -or
                (
                    $isUnsigned -and
                    $null -ne $signerCertSha256
                )
            ) {
                throw (
                    'Comparator campaign executable signature policy is ' +
                    "invalid for $name"
                )
            }

            $normalizedTerminals.Add([pscustomobject][ordered]@{
                campaign_id = [string]$campaignId
                campaign_sha256 = [string]$file.sha256
                selection_policy = [string]$selectionPolicy
                name = [string]$name
                role = [string]$role
                version = [string]$version
                source = [pscustomobject][ordered]@{
                    origin = [string]$origin
                    package = [string]$package
                    release_tag = [string]$tag
                    asset = [pscustomobject][ordered]@{
                        kind = [string]$assetKind
                        name = [string]$assetName
                        url = [string]$assetUrl
                        bytes = [long]$assetBytes
                        sha256 = [string]$assetSha256
                        executable_entry = (
                            [string]$assetExecutableEntry
                        )
                        staged_file_count = [int]$stagedFileCount
                        staged_total_bytes = [long]$stagedTotalBytes
                        staged_tree_sha256 = [string]$stagedTreeSha256
                    }
                }
                executable = [pscustomobject][ordered]@{
                    leaf = [string]$leaf
                    staging_path = [string]$stagingPath
                    bytes = [long]$bytes
                    sha256 = [string]$sha256
                    authenticode_status = [string]$authenticodeStatus
                    signer_cert_sha256 = if (
                        $null -eq $signerCertSha256
                    ) {
                        $null
                    } else {
                        [string]$signerCertSha256
                    }
                }
            })
        }

        return [pscustomobject][ordered]@{
            schema = 'kettle-windows-comparator-campaign-v1'
            campaign_id = [string]$campaignId
            platform = [pscustomobject][ordered]@{
                os = 'windows'
                architecture = 'x86_64'
            }
            selection = [pscustomobject][ordered]@{
                policy = [string]$selectionPolicy
                started_at_utc = [string]$startedText
                completed_at_utc = [string]$completedText
            }
            terminals = [object[]]$normalizedTerminals.ToArray()
            campaign_file = [pscustomobject][ordered]@{
                path = [string]$file.path
                relative_path = (
                    "$campaignId/campaign.json"
                )
                bytes = [long]$file.bytes
                sha256 = [string]$file.sha256
            }
        }
    } finally {
        Close-KettlePerfEvidenceSnapshot -Snapshot $snapshot
    }
}

function Get-KettlePerfComparatorCampaignEntry {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        $Campaign,
        [Parameter(Mandatory)]
        [string]$Name
    )

    if (
        $null -eq $Campaign -or
        $Campaign.schema -cne 'kettle-windows-comparator-campaign-v1' -or
        $Campaign.terminals -isnot [Array]
    ) {
        throw 'Comparator campaign is missing or invalid'
    }
    $entryMatches = @(
        $Campaign.terminals | Where-Object {
            $_.name -is [string] -and
            [StringComparer]::Ordinal.Equals($_.name, $Name)
        }
    )
    if ($entryMatches.Count -ne 1) {
        throw "Comparator campaign has no unique terminal entry: $Name"
    }
    return Copy-KettlePerfComparatorCampaignEntry -Entry $entryMatches[0]
}

function Get-KettlePerfComparatorCertificateSha256 {
    param(
        [Parameter(Mandatory)]
        [Security.Cryptography.X509Certificates.X509Certificate2]$Certificate
    )

    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return (
            [BitConverter]::ToString(
                $algorithm.ComputeHash($Certificate.RawData)
            ).Replace('-', '').ToLowerInvariant()
        )
    } finally {
        $algorithm.Dispose()
    }
}

function Initialize-KettlePerfComparatorCampaignNativeTypes {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'Initializes the related native file-identity types.'
    )]
    param()

    if ('KettlePerfComparatorCampaign.NativeFile' -as [type]) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfComparatorCampaign {
    public sealed class FileIdentity {
        internal FileIdentity(
            string identity,
            uint numberOfLinks,
            long length) {
            Identity = identity;
            NumberOfLinks = numberOfLinks;
            Length = length;
        }

        public string Identity { get; private set; }
        public uint NumberOfLinks { get; private set; }
        public long Length { get; private set; }
    }

    public static class NativeFile {
        [StructLayout(LayoutKind.Sequential)]
        private struct FileTime {
            internal uint Low;
            internal uint High;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ByHandleFileInformation {
            internal uint FileAttributes;
            internal FileTime CreationTime;
            internal FileTime LastAccessTime;
            internal FileTime LastWriteTime;
            internal uint VolumeSerialNumber;
            internal uint FileSizeHigh;
            internal uint FileSizeLow;
            internal uint NumberOfLinks;
            internal uint FileIndexHigh;
            internal uint FileIndexLow;
        }

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetFileInformationByHandle(
            SafeFileHandle handle,
            out ByHandleFileInformation information);

        public static FileIdentity GetIdentity(SafeFileHandle handle) {
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Reading comparator staged-file identity failed");
            }
            var identity =
                information.VolumeSerialNumber.ToString("x8") + ":" +
                information.FileIndexHigh.ToString("x8") +
                information.FileIndexLow.ToString("x8");
            var length = checked(
                ((long)information.FileSizeHigh << 32) |
                information.FileSizeLow);
            return new FileIdentity(
                identity,
                information.NumberOfLinks,
                length);
        }
    }
}
'@
}

function Test-KettlePerfComparatorSafeTreeRelativePath {
    param(
        [AllowNull()]
        $Value
    )

    if (
        $Value -isnot [string] -or
        $Value.Length -lt 1 -or
        $Value.Length -gt 512 -or
        -not $Value.IsNormalized([Text.NormalizationForm]::FormC) -or
        $Value.IndexOf('\') -ge 0 -or
        $Value.StartsWith('/', [StringComparison]::Ordinal) -or
        $Value.EndsWith('/', [StringComparison]::Ordinal)
    ) {
        return $false
    }
    $parts = [string[]]$Value.Split(
        [char[]]@('/'),
        [StringSplitOptions]::None
    )
    if ($parts.Count -gt 32) {
        return $false
    }
    $invalidCharacters = [IO.Path]::GetInvalidFileNameChars()
    foreach ($part in $parts) {
        if (
            $part.Length -lt 1 -or
            $part.Length -gt 255 -or
            $part -in @('.', '..') -or
            $part.EndsWith('.', [StringComparison]::Ordinal) -or
            $part.EndsWith(' ', [StringComparison]::Ordinal) -or
            $part.IndexOfAny($invalidCharacters) -ge 0
        ) {
            return $false
        }
        $deviceBase = ($part -split '\.', 2)[0]
        if (
            $deviceBase -cmatch '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$'
        ) {
            return $false
        }
    }
    return $true
}

function Get-KettlePerfComparatorStagedTreeFiles {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'Returns the complete bounded staged file set.'
    )]
    [OutputType([object[]])]
    param(
        [Parameter(Mandatory)]
        [string]$Root,
        [ValidateRange(1, 4096)]
        [int]$MaximumFiles = 4096
    )

    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    if (
        -not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'Comparator staged tree root must be an ordinary directory'
    }
    $rootPath = [IO.Path]::GetFullPath($rootItem.FullName).TrimEnd('\', '/')
    $rootPrefix = "$rootPath$([IO.Path]::DirectorySeparatorChar)"
    $directories = [Collections.Generic.Stack[string]]::new()
    $directories.Push($rootPath)
    $files = [Collections.Generic.Dictionary[string, object]]::new(
        [StringComparer]::Ordinal
    )
    $caseInsensitivePaths = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $observedDirectories = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $directoryCount = 0
    while ($directories.Count -gt 0) {
        $directory = $directories.Pop()
        $directoryCount++
        if ($directoryCount -gt 4096) {
            throw 'Comparator staged tree exceeds its directory bound'
        }
        foreach ($candidate in [IO.Directory]::EnumerateFileSystemEntries(
            $directory,
            '*',
            [IO.SearchOption]::TopDirectoryOnly
        )) {
            $item = Get-Item -LiteralPath $candidate -Force -ErrorAction Stop
            $fullPath = [IO.Path]::GetFullPath($item.FullName)
            if (
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    $fullPath,
                    [IO.Path]::GetFullPath($candidate)
                ) -or
                ($item.Attributes -band
                    [IO.FileAttributes]::ReparsePoint) -ne 0 -or
                -not $fullPath.StartsWith(
                    $rootPrefix,
                    [StringComparison]::OrdinalIgnoreCase
                )
            ) {
                throw 'Comparator staged tree aliases or contains a reparse point'
            }
            $relativePath = $fullPath.Substring(
                $rootPrefix.Length
            ).Replace('\', '/')
            if (-not (
                Test-KettlePerfComparatorSafeTreeRelativePath $relativePath
            )) {
                throw 'Comparator staged tree contains an unsafe relative path'
            }
            if ($item.PSIsContainer) {
                if (-not $observedDirectories.Add($relativePath)) {
                    throw 'Comparator staged tree has ambiguous directories'
                }
                $directories.Push($fullPath)
                continue
            }
            if (-not $caseInsensitivePaths.Add($relativePath)) {
                throw 'Comparator staged tree has case-ambiguous paths'
            }
            if ($files.Count -ge $MaximumFiles) {
                throw 'Comparator staged tree exceeds its file-count bound'
            }
            $files.Add($relativePath, [pscustomobject]@{
                relative_path = $relativePath
                path = $fullPath
            })
        }
    }
    if ($files.Count -eq 0) {
        throw 'Comparator staged tree contains no files'
    }
    $expectedDirectories = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($relativePath in $files.Keys) {
        $parts = [string[]]$relativePath.Split(
            [char[]]@('/'),
            [StringSplitOptions]::None
        )
        for ($index = 1; $index -lt $parts.Count; $index++) {
            [void]$expectedDirectories.Add(
                [string]::Join('/', $parts[0..($index - 1)])
            )
        }
    }
    if ($observedDirectories.Count -ne $expectedDirectories.Count) {
        throw 'Comparator staged tree contains an unbound directory'
    }
    foreach ($directory in $observedDirectories) {
        if (-not $expectedDirectories.Contains($directory)) {
            throw 'Comparator staged tree contains an unbound directory'
        }
    }
    [string[]]$names = @($files.Keys)
    [Array]::Sort($names, [StringComparer]::Ordinal)
    return [object[]]@(
        foreach ($name in $names) {
            $files[$name]
        }
    )
}

function Get-KettlePerfComparatorStagedTreeSha256 {
    param(
        [Parameter(Mandatory)]
        [object[]]$Files
    )

    $builder = [Text.StringBuilder]::new(
        'kettle-comparator-staged-tree-v1'.Length + 1
    )
    [void]$builder.Append("kettle-comparator-staged-tree-v1`n")
    $previous = $null
    foreach ($file in $Files) {
        $relativePath = [string]$file.relative_path
        [long]$bytes = $file.bytes
        $sha256 = [string]$file.sha256
        if (
            -not (Test-KettlePerfComparatorSafeTreeRelativePath `
                $relativePath) -or
            $bytes -lt 0 -or
            $sha256 -cnotmatch '^[0-9a-f]{64}$' -or
            (
                $null -ne $previous -and
                [StringComparer]::Ordinal.Compare(
                    $previous,
                    $relativePath
                ) -ge 0
            )
        ) {
            throw 'Comparator staged tree digest input is invalid or unsorted'
        }
        [void]$builder.Append($relativePath)
        [void]$builder.Append([char]0)
        [void]$builder.Append(
            $bytes.ToString([Globalization.CultureInfo]::InvariantCulture)
        )
        [void]$builder.Append([char]0)
        [void]$builder.Append($sha256)
        [void]$builder.Append("`n")
        $previous = $relativePath
    }
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $data = $encoding.GetBytes($builder.ToString())
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return (
            [BitConverter]::ToString(
                $algorithm.ComputeHash($data)
            ).Replace('-', '').ToLowerInvariant()
        )
    } finally {
        $algorithm.Dispose()
        [Array]::Clear($data, 0, $data.Length)
    }
}

function Open-KettlePerfComparatorStagedTreeLease {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [string]$Root,
        [Parameter(Mandatory)]
        [ValidateRange(1, 4096)]
        [int]$ExpectedFileCount,
        [Parameter(Mandatory)]
        [ValidateRange(1, 8589934592)]
        [long]$ExpectedTotalBytes,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$ExpectedTreeSha256
    )

    Initialize-KettlePerfComparatorCampaignNativeTypes
    $discovered = @(
        Get-KettlePerfComparatorStagedTreeFiles `
            -Root $Root -MaximumFiles $ExpectedFileCount
    )
    if ($discovered.Count -ne $ExpectedFileCount) {
        throw 'Comparator staged tree file count differs from campaign'
    }
    $leasedFiles = [Collections.Generic.List[object]]::new()
    $fileIdentities = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    [long]$totalBytes = 0
    try {
        foreach ($file in $discovered) {
            $item = Get-Item -LiteralPath $file.path -Force -ErrorAction Stop
            if (
                $item.PSIsContainer -or
                ($item.Attributes -band
                    [IO.FileAttributes]::ReparsePoint) -ne 0
            ) {
                throw 'Comparator staged tree file is not ordinary'
            }
            $stream = [IO.FileStream]::new(
                $file.path,
                [IO.FileMode]::Open,
                [IO.FileAccess]::Read,
                [IO.FileShare]::Read
            )
            $algorithm = $null
            try {
                $identity = (
                    [KettlePerfComparatorCampaign.NativeFile]::GetIdentity(
                        $stream.SafeFileHandle
                    )
                )
                if (
                    $identity.NumberOfLinks -ne 1 -or
                    -not $fileIdentities.Add($identity.Identity) -or
                    [long]$identity.Length -ne [long]$stream.Length
                ) {
                    throw 'Comparator staged tree contains a hard-linked file'
                }
                if (
                    [long]$stream.Length -gt
                        ($ExpectedTotalBytes - $totalBytes)
                ) {
                    throw 'Comparator staged tree exceeds its byte contract'
                }
                $totalBytes += [long]$stream.Length
                $algorithm = [Security.Cryptography.SHA256]::Create()
                $sha256 = (
                    [BitConverter]::ToString(
                        $algorithm.ComputeHash($stream)
                    ).Replace('-', '').ToLowerInvariant()
                )
                $stream.Position = 0
                $leasedFiles.Add([pscustomobject][ordered]@{
                    relative_path = [string]$file.relative_path
                    path = [string]$file.path
                    bytes = [long]$stream.Length
                    sha256 = [string]$sha256
                    stream = $stream
                })
                $stream = $null
            } finally {
                if ($null -ne $algorithm) {
                    $algorithm.Dispose()
                }
                if ($null -ne $stream) {
                    $stream.Dispose()
                }
            }
        }
        $after = @(
            Get-KettlePerfComparatorStagedTreeFiles `
                -Root $Root -MaximumFiles $ExpectedFileCount
        )
        if ($after.Count -ne $leasedFiles.Count) {
            throw 'Comparator staged tree changed while leases were acquired'
        }
        for ($index = 0; $index -lt $after.Count; $index++) {
            if (-not [StringComparer]::Ordinal.Equals(
                $after[$index].relative_path,
                $leasedFiles[$index].relative_path
            )) {
                throw 'Comparator staged tree changed while leases were acquired'
            }
        }
        $treeSha256 = Get-KettlePerfComparatorStagedTreeSha256 `
            -Files $leasedFiles.ToArray()
        if (
            $totalBytes -ne $ExpectedTotalBytes -or
            $treeSha256 -cne $ExpectedTreeSha256
        ) {
            throw 'Comparator staged tree aggregate differs from campaign'
        }
        $result = [pscustomobject][ordered]@{
            schema = 'kettle-comparator-staged-tree-lease-v1'
            root = [IO.Path]::GetFullPath($Root)
            file_count = [int]$leasedFiles.Count
            total_bytes = [long]$totalBytes
            tree_sha256 = [string]$treeSha256
            files = [object[]]$leasedFiles.ToArray()
            closed = $false
        }
        $leasedFiles.Clear()
        return $result
    } finally {
        foreach ($file in $leasedFiles) {
            if ($null -ne $file.stream) {
                $file.stream.Dispose()
            }
        }
        $leasedFiles.Clear()
    }
}

function Close-KettlePerfComparatorStagedTreeLease {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Only releases retained staged-tree read handles.'
    )]
    param(
        [AllowNull()]
        $Lease
    )

    if (
        $null -eq $Lease -or
        $Lease.schema -cne 'kettle-comparator-staged-tree-lease-v1' -or
        $Lease.closed -eq $true
    ) {
        return
    }
    try {
        foreach ($file in $Lease.files) {
            if ($null -ne $file.stream) {
                $file.stream.Dispose()
                $file.stream = $null
            }
        }
    } finally {
        $Lease.closed = $true
    }
}

function Get-KettlePerfComparatorVerifiedTreeExecutable {
    param(
        [Parameter(Mandatory)]
        $TreeLease,
        [Parameter(Mandatory)]
        $Entry,
        [Parameter(Mandatory)]
        [string]$ExpectedPath
    )

    $executableMatches = @(
        $TreeLease.files | Where-Object {
            [StringComparer]::Ordinal.Equals(
                $_.relative_path,
                $Entry.executable.leaf
            )
        }
    )
    if (
        $executableMatches.Count -ne 1 -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetFullPath($executableMatches[0].path),
            [IO.Path]::GetFullPath($ExpectedPath)
        ) -or
        [long]$executableMatches[0].bytes -ne
            [long]$Entry.executable.bytes -or
        $executableMatches[0].sha256 -cne $Entry.executable.sha256
    ) {
        throw 'Comparator staged-tree executable differs from campaign'
    }
    $signature = Get-AuthenticodeSignature `
        -LiteralPath $executableMatches[0].path `
        -ErrorAction Stop
    $actualStatus = [string]$signature.Status
    $actualCertificateSha = if (
        $null -eq $signature.SignerCertificate
    ) {
        $null
    } else {
        Get-KettlePerfComparatorCertificateSha256 `
            -Certificate $signature.SignerCertificate
    }
    if (
        $actualStatus -cne $Entry.executable.authenticode_status -or
        (
            $null -eq $Entry.executable.signer_cert_sha256 -and
            $null -ne $actualCertificateSha
        ) -or
        (
            $null -ne $Entry.executable.signer_cert_sha256 -and
            (
                $null -eq $actualCertificateSha -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    $actualCertificateSha,
                    $Entry.executable.signer_cert_sha256
                )
            )
        )
    ) {
        throw 'Comparator staged-tree executable signature differs from campaign'
    }
    return [pscustomobject][ordered]@{
        file = $executableMatches[0]
        authenticode_status = $actualStatus
        signer_cert_sha256 = $actualCertificateSha
    }
}

function Get-KettlePerfComparatorCampaignRuntimeContext {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        $Campaign,
        [Parameter(Mandatory)]
        $Entry,
        [Parameter(Mandatory)]
        [string]$CampaignRoot,
        [Parameter(Mandatory)]
        [string]$StagingRoot
    )

    if ($null -eq $Campaign -or $null -eq $Entry) {
        throw 'Comparator campaign and entry are required'
    }
    $campaignFile = Get-KettlePerfComparatorRequiredProperty `
        $Campaign 'campaign_file' 'Comparator campaign'
    $campaignPath = Get-KettlePerfComparatorRequiredProperty `
        $campaignFile 'path' 'Comparator campaign file'
    $entryName = Get-KettlePerfComparatorRequiredProperty `
        $Entry 'name' 'Comparator campaign entry'
    if ($campaignPath -isnot [string] -or $entryName -isnot [string]) {
        throw 'Comparator campaign runtime identity is invalid'
    }

    # Re-read the retained campaign path before deriving any executable path.
    # The fresh campaign, not a caller-mutated object, is authoritative.
    $freshCampaign = Read-KettlePerfComparatorCampaign `
        -Path $campaignPath -ExpectedCampaignRoot $CampaignRoot
    $freshEntry = Get-KettlePerfComparatorCampaignEntry `
        -Campaign $freshCampaign -Name $entryName
    $expectedEntryJson = $freshEntry |
        ConvertTo-Json -Depth 8 -Compress
    $providedEntryJson = (
        Copy-KettlePerfComparatorCampaignEntry -Entry $Entry
    ) |
        ConvertTo-Json -Depth 8 -Compress
    if ($providedEntryJson -cne $expectedEntryJson) {
        throw 'Comparator campaign entry differs from the fresh campaign'
    }
    $campaignId = Get-KettlePerfComparatorRequiredProperty `
        $Campaign 'campaign_id' 'Comparator campaign'
    $campaignSha = Get-KettlePerfComparatorRequiredProperty `
        $campaignFile 'sha256' 'Comparator campaign file'
    if (
        $campaignId -isnot [string] -or
        $campaignSha -isnot [string] -or
        $campaignId -cne $freshCampaign.campaign_id -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            $campaignSha,
            $freshCampaign.campaign_file.sha256
        )
    ) {
        throw 'Comparator campaign object differs from its fresh file identity'
    }

    $rootItem = Get-Item -LiteralPath $StagingRoot -Force `
        -ErrorAction Stop
    if (
        -not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'Comparator staging root must be an ordinary directory'
    }
    $rootFullPath = [IO.Path]::GetFullPath($rootItem.FullName)
    $fileSystemRoot = [IO.Path]::GetPathRoot($rootFullPath)
    if ([StringComparer]::OrdinalIgnoreCase.Equals(
        $rootFullPath.TrimEnd('\', '/'),
        $fileSystemRoot.TrimEnd('\', '/')
    )) {
        throw 'Comparator staging root must not be a filesystem root'
    }
    $rootPath = $rootFullPath.TrimEnd('\', '/')
    if (-not (Test-KettlePerfComparatorPathHasNoAlternateStream $rootPath)) {
        throw 'Comparator staging root must not name an alternate data stream'
    }

    $relativeParts = [Collections.Generic.List[string]]::new()
    $relativeParts.Add($freshCampaign.campaign_id)
    foreach ($part in $freshEntry.executable.staging_path.Split(
        [char[]]@('/'),
        [StringSplitOptions]::None
    )) {
        $relativeParts.Add($part)
    }
    $current = $rootPath
    for ($index = 0; $index -lt $relativeParts.Count; $index++) {
        $current = Join-Path $current $relativeParts[$index]
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        $actualPath = [IO.Path]::GetFullPath($item.FullName)
        $expectedPath = [IO.Path]::GetFullPath($current)
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $actualPath,
                $expectedPath
            ) -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw 'Comparator staged path aliases or traverses a reparse point'
        }
        $isLast = $index -eq ($relativeParts.Count - 1)
        if (
            ($isLast -and $item.PSIsContainer) -or
            (-not $isLast -and -not $item.PSIsContainer)
        ) {
            throw 'Comparator staged path has an unexpected file type'
        }
    }
    $fullPath = [IO.Path]::GetFullPath($current)
    if (-not (Test-KettlePerfComparatorPathHasNoAlternateStream $fullPath)) {
        throw 'Comparator staged executable must not use an alternate stream'
    }

    return [pscustomobject][ordered]@{
        campaign = $freshCampaign
        entry = $freshEntry
        path = $fullPath
    }
}

function Resolve-KettlePerfComparatorCampaignExecutable {
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        $Campaign,
        [Parameter(Mandatory)]
        $Entry,
        [Parameter(Mandatory)]
        [string]$CampaignRoot,
        [Parameter(Mandatory)]
        [string]$StagingRoot
    )

    $runtime = Get-KettlePerfComparatorCampaignRuntimeContext `
        -Campaign $Campaign -Entry $Entry -CampaignRoot $CampaignRoot `
        -StagingRoot $StagingRoot
    $treeLease = $null
    try {
        $treeLease = Open-KettlePerfComparatorStagedTreeLease `
            -Root ([IO.Path]::GetDirectoryName($runtime.path)) `
            -ExpectedFileCount (
                $runtime.entry.source.asset.staged_file_count
            ) -ExpectedTotalBytes (
                $runtime.entry.source.asset.staged_total_bytes
            ) -ExpectedTreeSha256 (
                $runtime.entry.source.asset.staged_tree_sha256
            )
        [void](Get-KettlePerfComparatorVerifiedTreeExecutable `
            -TreeLease $treeLease -Entry $runtime.entry `
            -ExpectedPath $runtime.path)
        return [string]$runtime.path
    } finally {
        Close-KettlePerfComparatorStagedTreeLease $treeLease
    }
}

function Open-KettlePerfComparatorCampaignExecutableLease {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        $Campaign,
        [Parameter(Mandatory)]
        $Entry,
        [Parameter(Mandatory)]
        [string]$CampaignRoot,
        [Parameter(Mandatory)]
        [string]$StagingRoot
    )

    $runtime = Get-KettlePerfComparatorCampaignRuntimeContext `
        -Campaign $Campaign -Entry $Entry -CampaignRoot $CampaignRoot `
        -StagingRoot $StagingRoot
    $path = [string]$runtime.path
    $treeLease = $null
    try {
        $treeLease = Open-KettlePerfComparatorStagedTreeLease `
            -Root ([IO.Path]::GetDirectoryName($path)) `
            -ExpectedFileCount (
                $runtime.entry.source.asset.staged_file_count
            ) -ExpectedTotalBytes (
                $runtime.entry.source.asset.staged_total_bytes
            ) -ExpectedTreeSha256 (
                $runtime.entry.source.asset.staged_tree_sha256
            )
        $verified = Get-KettlePerfComparatorVerifiedTreeExecutable `
            -TreeLease $treeLease -Entry $runtime.entry -ExpectedPath $path
        $lease = [pscustomobject][ordered]@{
            schema = 'kettle-comparator-executable-lease-v1'
            path = [string]$path
            bytes = [long]$verified.file.bytes
            sha256 = [string]$verified.file.sha256
            authenticode_status = [string]$verified.authenticode_status
            signer_cert_sha256 = $verified.signer_cert_sha256
            staged_file_count = [int]$treeLease.file_count
            staged_total_bytes = [long]$treeLease.total_bytes
            staged_tree_sha256 = [string]$treeLease.tree_sha256
            files = [object[]]$treeLease.files
            stream = $verified.file.stream
            tree_lease = $treeLease
            closed = $false
        }
        $treeLease = $null
        return $lease
    } finally {
        Close-KettlePerfComparatorStagedTreeLease $treeLease
    }
}

function Close-KettlePerfComparatorCampaignExecutableLease {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Only releases one retained read handle.'
    )]
    param(
        [AllowNull()]
        $Lease
    )

    if (
        $null -eq $Lease -or
        $Lease.schema -cne 'kettle-comparator-executable-lease-v1' -or
        $Lease.closed -eq $true
    ) {
        return
    }
    try {
        Close-KettlePerfComparatorStagedTreeLease $Lease.tree_lease
    } finally {
        $Lease.stream = $null
        $Lease.closed = $true
    }
}

function Get-KettlePerfComparatorCampaignEvidence {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        $Campaign
    )

    if (
        $null -eq $Campaign -or
        $Campaign.schema -cne 'kettle-windows-comparator-campaign-v1' -or
        $Campaign.campaign_id -isnot [string] -or
        $Campaign.terminals -isnot [Array] -or
        $null -eq $Campaign.campaign_file
    ) {
        throw 'Comparator campaign is missing or invalid'
    }
    return [pscustomobject][ordered]@{
        schema = 'kettle-comparator-campaign-evidence-v1'
        campaign_schema = [string]$Campaign.schema
        campaign_id = [string]$Campaign.campaign_id
        platform = [pscustomobject][ordered]@{
            os = [string]$Campaign.platform.os
            architecture = [string]$Campaign.platform.architecture
        }
        selection = [pscustomobject][ordered]@{
            policy = [string]$Campaign.selection.policy
            started_at_utc = [string]$Campaign.selection.started_at_utc
            completed_at_utc = [string]$Campaign.selection.completed_at_utc
        }
        campaign_file = [pscustomobject][ordered]@{
            relative_path = [string]$Campaign.campaign_file.relative_path
            bytes = [long]$Campaign.campaign_file.bytes
            sha256 = [string]$Campaign.campaign_file.sha256
        }
        terminals = [object[]]@(
            $Campaign.terminals |
                ForEach-Object {
                    Copy-KettlePerfComparatorCampaignEntry -Entry $_
                }
        )
    }
}

function Test-KettlePerfComparatorCampaignEvidence {
    [OutputType([bool])]
    param(
        [Parameter(Mandatory)]
        $Campaign,
        [Parameter(Mandatory)]
        [AllowNull()]
        $Evidence
    )

    try {
        if ($null -eq $Evidence) {
            return $false
        }
        $expected = Get-KettlePerfComparatorCampaignEvidence `
            -Campaign $Campaign
        $expectedJson = ConvertTo-Json -InputObject $expected `
            -Depth 10 -Compress
        $actualJson = ConvertTo-Json -InputObject $Evidence `
            -Depth 10 -Compress
        return [StringComparer]::Ordinal.Equals(
            $actualJson,
            $expectedJson
        )
    } catch {
        return $false
    }
}

function New-KettlePerfComparatorTerminalSource {
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        $Entry
    )

    $runtimeKind = if ($Entry.name -ceq 'wt') {
        'installed-appx-direct-host-advisory'
    } else {
        'verified-staged-tree'
    }
    return [pscustomobject][ordered]@{
        kind = 'validated-comparator-campaign-v1'
        campaign_id = [string]$Entry.campaign_id
        campaign_sha256 = [string]$Entry.campaign_sha256
        origin = [string]$Entry.source.origin
        package = [string]$Entry.source.package
        release_tag = [string]$Entry.source.release_tag
        staging_path = [string]$Entry.executable.staging_path
        runtime_kind = $runtimeKind
        asset = [pscustomobject][ordered]@{
            kind = [string]$Entry.source.asset.kind
            name = [string]$Entry.source.asset.name
            url = [string]$Entry.source.asset.url
            bytes = [long]$Entry.source.asset.bytes
            sha256 = [string]$Entry.source.asset.sha256
            executable_entry = (
                [string]$Entry.source.asset.executable_entry
            )
            staged_file_count = [int](
                $Entry.source.asset.staged_file_count
            )
            staged_total_bytes = [long](
                $Entry.source.asset.staged_total_bytes
            )
            staged_tree_sha256 = [string](
                $Entry.source.asset.staged_tree_sha256
            )
        }
    }
}

function Test-KettlePerfComparatorCampaignTerminalIdentity {
    [OutputType([bool])]
    param(
        [Parameter(Mandatory)]
        [AllowNull()]
        $Entry,
        [Parameter(Mandatory)]
        [AllowNull()]
        $TerminalRecord
    )

    try {
        if ($null -eq $Entry -or $null -eq $TerminalRecord) {
            return $false
        }
        $name = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'name' 'Terminal record'
        $version = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'version' 'Terminal record'
        $executablePath = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'executable' 'Terminal record'
        $executableBytes = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'executable_bytes' 'Terminal record'
        $executableSha = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'executable_sha256' 'Terminal record'
        $authenticodeStatus = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'authenticode_status' 'Terminal record'
        $signerCertSha256 = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'signer_cert_sha256' 'Terminal record'
        $role = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'comparator_role' 'Terminal record'
        $source = Get-KettlePerfComparatorRequiredProperty `
            $TerminalRecord 'source' 'Terminal record'
        if (
            $name -isnot [string] -or
            $version -isnot [string] -or
            $executablePath -isnot [string] -or
            $executableSha -isnot [string] -or
            $authenticodeStatus -isnot [string] -or
            $role -isnot [string] -or
            -not (Test-KettlePerfComparatorInteger $executableBytes) -or
            -not [StringComparer]::Ordinal.Equals($name, $Entry.name) -or
            -not [StringComparer]::Ordinal.Equals(
                $version,
                $Entry.version
            ) -or
            -not [StringComparer]::Ordinal.Equals(
                [IO.Path]::GetFileName($executablePath),
                $Entry.executable.leaf
            ) -or
            [long]$executableBytes -ne [long]$Entry.executable.bytes -or
            $executableSha -cnotmatch '^[0-9A-Fa-f]{64}$' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $executableSha,
                $Entry.executable.sha256
            ) -or
            -not [StringComparer]::Ordinal.Equals(
                $authenticodeStatus,
                $Entry.executable.authenticode_status
            ) -or
            -not [StringComparer]::Ordinal.Equals($role, $Entry.role)
        ) {
            return $false
        }
        if ($Entry.name -ceq 'wt') {
            $launcher = Get-KettlePerfComparatorRequiredProperty `
                $TerminalRecord 'launcher' 'Terminal record'
            $launchMode = Get-KettlePerfComparatorRequiredProperty `
                $TerminalRecord 'launch_mode' 'Terminal record'
            if (
                $launcher -isnot [string] -or
                $launchMode -isnot [string] -or
                $launchMode -cne 'installed-appx-direct-host' -or
                -not [IO.Path]::IsPathRooted($launcher) -or
                -not [IO.Path]::IsPathRooted($executablePath) -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    [IO.Path]::GetFullPath($launcher),
                    [IO.Path]::GetFullPath($executablePath)
                )
            ) {
                return $false
            }
        }
        if ($null -eq $Entry.executable.signer_cert_sha256) {
            if ($null -ne $signerCertSha256) {
                return $false
            }
        } elseif (
            $signerCertSha256 -isnot [string] -or
            $signerCertSha256 -cnotmatch '^[0-9A-Fa-f]{64}$' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $signerCertSha256,
                $Entry.executable.signer_cert_sha256
            )
        ) {
            return $false
        }

        $expectedSource = [ordered]@{
            kind = 'validated-comparator-campaign-v1'
            campaign_id = [string]$Entry.campaign_id
            campaign_sha256 = [string]$Entry.campaign_sha256
            origin = [string]$Entry.source.origin
            package = [string]$Entry.source.package
            release_tag = [string]$Entry.source.release_tag
            staging_path = [string]$Entry.executable.staging_path
            runtime_kind = if ($Entry.name -ceq 'wt') {
                'installed-appx-direct-host-advisory'
            } else {
                'verified-staged-tree'
            }
        }
        foreach ($propertyName in $expectedSource.Keys) {
            $actual = Get-KettlePerfComparatorRequiredProperty `
                $source $propertyName 'Terminal record source'
            $sourceMatches = if (
                $propertyName -ceq 'campaign_sha256'
            ) {
                $actual -is [string] -and
                $actual -cmatch '^[0-9A-Fa-f]{64}$' -and
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    $actual,
                    $expectedSource[$propertyName]
                )
            } else {
                $actual -is [string] -and
                [StringComparer]::Ordinal.Equals(
                    $actual,
                    $expectedSource[$propertyName]
                )
            }
            if (-not $sourceMatches) {
                return $false
            }
        }
        $sourceAsset = Get-KettlePerfComparatorRequiredProperty `
            $source 'asset' 'Terminal record source'
        $assetStringFields = [string[]]@(
            'kind',
            'name',
            'url',
            'executable_entry'
        )
        foreach ($propertyName in $assetStringFields) {
            $actual = Get-KettlePerfComparatorRequiredProperty `
                $sourceAsset $propertyName 'Terminal record source asset'
            if (
                $actual -isnot [string] -or
                -not [StringComparer]::Ordinal.Equals(
                    $actual,
                    $Entry.source.asset.$propertyName
                )
            ) {
                return $false
            }
        }
        $actualAssetBytes = Get-KettlePerfComparatorRequiredProperty `
            $sourceAsset 'bytes' 'Terminal record source asset'
        $actualAssetSha = Get-KettlePerfComparatorRequiredProperty `
            $sourceAsset 'sha256' 'Terminal record source asset'
        $actualStagedFileCount = Get-KettlePerfComparatorRequiredProperty `
            $sourceAsset 'staged_file_count' `
            'Terminal record source asset'
        $actualStagedTotalBytes = Get-KettlePerfComparatorRequiredProperty `
            $sourceAsset 'staged_total_bytes' `
            'Terminal record source asset'
        $actualStagedTreeSha = Get-KettlePerfComparatorRequiredProperty `
            $sourceAsset 'staged_tree_sha256' `
            'Terminal record source asset'
        if (
            -not (Test-KettlePerfComparatorInteger $actualAssetBytes) -or
            [long]$actualAssetBytes -ne [long]$Entry.source.asset.bytes -or
            -not (Test-KettlePerfComparatorInteger `
                $actualStagedFileCount) -or
            [int]$actualStagedFileCount -ne
                [int]$Entry.source.asset.staged_file_count -or
            -not (Test-KettlePerfComparatorInteger `
                $actualStagedTotalBytes) -or
            [long]$actualStagedTotalBytes -ne
                [long]$Entry.source.asset.staged_total_bytes -or
            $actualAssetSha -isnot [string] -or
            $actualAssetSha -cnotmatch '^[0-9A-Fa-f]{64}$' -or
            $actualStagedTreeSha -isnot [string] -or
            $actualStagedTreeSha -cnotmatch '^[0-9A-Fa-f]{64}$' -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $actualAssetSha,
                $Entry.source.asset.sha256
            ) -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $actualStagedTreeSha,
                $Entry.source.asset.staged_tree_sha256
            )
        ) {
            return $false
        }
        return $true
    } catch {
        return $false
    }
}
