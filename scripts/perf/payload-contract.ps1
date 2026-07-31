# Canonical byte identity for every Windows throughput workload. All producers,
# runners, score gates, and synthetic fixtures dot-source this file so changing
# a payload requires one deliberate contract update.
$KettlePerfPayloadContracts = [ordered]@{
    ascii = [ordered]@{
        file = 'ascii.txt'
        bytes = 16768000
        sha256 = 'C651D96082179865874D6D6FCA573093C974C92FBE2B04B6F18B565786A5745D'
    }
    sgr = [ordered]@{
        file = 'sgr.txt'
        bytes = 6384000
        sha256 = 'C421906516453BA997D79610B5F4A11E307D215629FACCE95B2B11578C988F0B'
    }
    unicode = [ordered]@{
        file = 'unicode.txt'
        bytes = 4500000
        sha256 = '9DE3A50A416819285E43CB33A1D32FD4474000262575EAD8714897C2E3971863'
    }
}

function Test-KettlePerfPayloadFile {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)]
        [ValidateSet('ascii', 'sgr', 'unicode')]
        [string]$Name
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $false
    }
    $contract = $KettlePerfPayloadContracts[$Name]
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -ne $contract.bytes
    ) {
        return $false
    }
    return [StringComparer]::OrdinalIgnoreCase.Equals(
        (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash,
        $contract.sha256
    )
}

function Open-KettlePerfPayloadSet {
    param(
        [Parameter(Mandatory)]
        [string]$PayloadDirectory,
        [Collections.IDictionary]$Contracts =
            $KettlePerfPayloadContracts
    )

    $expectedNames = [string[]]@('ascii', 'sgr', 'unicode')
    if ($null -eq $Contracts) {
        throw 'Throughput payload contracts are required'
    }
    $unexpectedNames = @(
        $Contracts.Keys | Where-Object {
            [string]$_ -cnotin $expectedNames
        }
    )
    if (
        $Contracts.Count -ne $expectedNames.Count -or
        $unexpectedNames.Count -ne 0
    ) {
        throw 'Throughput payload contracts must define ascii, sgr, and unicode'
    }

    [long]$maximumTotalBytes = 0
    foreach ($name in $expectedNames) {
        $contract = $Contracts[$name]
        $file = [string]$contract.file
        [long]$bytes = $contract.bytes
        $sha256 = [string]$contract.sha256
        if (
            $file -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$' -or
            [IO.Path]::GetFileName($file) -cne $file -or
            $bytes -lt 1 -or
            $sha256 -cnotmatch '^[0-9A-Fa-f]{64}$' -or
            $maximumTotalBytes -gt ([int]::MaxValue - $bytes)
        ) {
            throw "Throughput payload contract is invalid: $name"
        }
        $maximumTotalBytes += $bytes
    }

    $snapshot = Open-KettlePerfEvidenceSnapshot `
        -Directory $PayloadDirectory `
        -MaximumFiles $expectedNames.Count `
        -MaximumTotalBytes $maximumTotalBytes
    return [pscustomobject]@{
        schema = 'kettle-throughput-payload-set-v1'
        snapshot = $snapshot
        contracts = $Contracts
        entries = [ordered]@{}
        closed = $false
    }
}

function Read-KettlePerfPayloadEntry {
    param(
        [Parameter(Mandatory)]
        $PayloadSet,
        [Parameter(Mandatory)]
        [ValidateSet('ascii', 'sgr', 'unicode')]
        [string]$Name
    )

    if (
        $null -eq $PayloadSet -or
        $PayloadSet.schema -cne 'kettle-throughput-payload-set-v1' -or
        $PayloadSet.closed -ne $false -or
        $null -eq $PayloadSet.snapshot -or
        $null -eq $PayloadSet.contracts
    ) {
        throw 'Throughput payload set is missing, invalid, or closed'
    }
    if ($PayloadSet.entries.Contains($Name)) {
        return $PayloadSet.entries[$Name]
    }
    $contract = $PayloadSet.contracts[$Name]
    $entry = Read-KettlePerfEvidenceText `
        -Snapshot $PayloadSet.snapshot `
        -LeafName ([string]$contract.file) `
        -MaximumBytes ([long]$contract.bytes) -Required
    if (
        [long]$entry.bytes -ne [long]$contract.bytes -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$entry.sha256,
            [string]$contract.sha256
        )
    ) {
        throw (
            'Throughput payload does not match its byte/hash ' +
            "contract: $($entry.path)"
        )
    }
    $PayloadSet.entries[$Name] = $entry
    return $entry
}

function Release-KettlePerfPayloadEntry {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Only releases one retained read handle and buffer.'
    )]
    param(
        [Parameter(Mandatory)]
        $PayloadSet,
        [Parameter(Mandatory)]
        [ValidateSet('ascii', 'sgr', 'unicode')]
        [string]$Name
    )

    if (
        $null -eq $PayloadSet -or
        $PayloadSet.schema -cne 'kettle-throughput-payload-set-v1' -or
        $PayloadSet.closed -ne $false -or
        -not $PayloadSet.entries.Contains($Name)
    ) {
        return
    }
    $entry = $PayloadSet.entries[$Name]
    if ($null -ne $entry.native) {
        $entry.native.Dispose()
    }
    $entry.text = $null
    [void]$PayloadSet.entries.Remove($Name)
}

function Close-KettlePerfPayloadSet {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Only releases retained read handles and memory.'
    )]
    param(
        $PayloadSet
    )

    if (
        $null -eq $PayloadSet -or
        $PayloadSet.schema -cne 'kettle-throughput-payload-set-v1' -or
        $PayloadSet.closed -eq $true
    ) {
        return
    }
    try {
        Close-KettlePerfEvidenceSnapshot -Snapshot $PayloadSet.snapshot
    } finally {
        $PayloadSet.entries.Clear()
        $PayloadSet.closed = $true
    }
}
