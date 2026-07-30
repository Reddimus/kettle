# GUI-free regression tests for held throughput payload identity.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. "$PSScriptRoot\evidence-snapshot.ps1"
. "$PSScriptRoot\payload-contract.ps1"

function Assert-KettlePerfPayloadContractTest {
    param(
        [Parameter(Mandatory)]
        [bool]$Condition,
        [Parameter(Mandatory)]
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Get-KettlePerfPayloadContractTestSha256 {
    param(
        [Parameter(Mandatory)]
        [string]$Text
    )

    $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return (
            [BitConverter]::ToString($sha.ComputeHash($bytes)).
                Replace('-', '')
        )
    } finally {
        $sha.Dispose()
        [Array]::Clear($bytes, 0, $bytes.Length)
    }
}

function Invoke-KettlePerfExpectedPayloadContractFailure {
    param(
        [Parameter(Mandatory)]
        [string]$Description,
        [Parameter(Mandatory)]
        [scriptblock]$Action
    )

    $failed = $false
    try {
        & $Action
    } catch {
        $failed = $true
    }
    if (-not $failed) {
        throw "Expected payload-contract failure: $Description"
    }
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-payload-contract-' + [Guid]::NewGuid().ToString('N')
)
$moved = "$scratch-moved"
[void][IO.Directory]::CreateDirectory($scratch)
$utf8 = [Text.UTF8Encoding]::new($false, $true)
$values = [ordered]@{
    ascii = 'plain-ascii'
    sgr = "`e[31mred`e[0m"
    unicode = "snowman $([char]0x2603)"
}
$contracts = [ordered]@{}
foreach ($name in $values.Keys) {
    $leaf = "$name.txt"
    $text = [string]$values[$name]
    [IO.File]::WriteAllText((Join-Path $scratch $leaf), $text, $utf8)
    $contracts[$name] = [ordered]@{
        file = $leaf
        bytes = $utf8.GetByteCount($text)
        sha256 = Get-KettlePerfPayloadContractTestSha256 $text
    }
}

$payloadSet = $null
try {
    $payloadSet = Open-KettlePerfPayloadSet `
        -PayloadDirectory $scratch -Contracts $contracts
    $asciiEntry = Read-KettlePerfPayloadEntry `
        -PayloadSet $payloadSet -Name ascii
    $unicodeEntry = Read-KettlePerfPayloadEntry `
        -PayloadSet $payloadSet -Name unicode
    Assert-KettlePerfPayloadContractTest (
        $payloadSet.schema -ceq 'kettle-throughput-payload-set-v1' -and
        $payloadSet.entries.Count -eq 2 -and
        [string]$asciiEntry.text -ceq $values.ascii -and
        [string]$unicodeEntry.text -ceq $values.unicode
    ) 'Held payload set did not preserve exact validated text'

    Invoke-KettlePerfExpectedPayloadContractFailure `
        -Description 'write while exact payload handle is retained' `
        -Action {
            $writer = [IO.File]::Open(
                (Join-Path $scratch 'ascii.txt'),
                [IO.FileMode]::Open,
                [IO.FileAccess]::Write,
                [IO.FileShare]::None
            )
            $writer.Dispose()
        }
    Invoke-KettlePerfExpectedPayloadContractFailure `
        -Description 'root rename while snapshot handle is retained' `
        -Action {
            [IO.Directory]::Move($scratch, $moved)
        }

    Close-KettlePerfPayloadSet -PayloadSet $payloadSet
    Assert-KettlePerfPayloadContractTest (
        $payloadSet.closed -eq $true -and
        $payloadSet.entries.Count -eq 0
    ) 'Payload-set close did not release and clear retained evidence'
    $payloadSet = $null

    [IO.File]::WriteAllText(
        (Join-Path $scratch 'ascii.txt'),
        'tampered',
        $utf8
    )
    Invoke-KettlePerfExpectedPayloadContractFailure `
        -Description 'tampered same-path payload' `
        -Action {
            $opened = Open-KettlePerfPayloadSet `
                -PayloadDirectory $scratch -Contracts $contracts
            try {
                [void](Read-KettlePerfPayloadEntry `
                    -PayloadSet $opened -Name ascii)
            } finally {
                Close-KettlePerfPayloadSet -PayloadSet $opened
            }
        }
} finally {
    Close-KettlePerfPayloadSet -PayloadSet $payloadSet
    $scratchFull = [IO.Path]::GetFullPath($scratch)
    $movedFull = [IO.Path]::GetFullPath($moved)
    $tempFull = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    foreach ($candidate in @($scratchFull, $movedFull)) {
        if (-not $candidate.StartsWith(
            $tempFull,
            [StringComparison]::OrdinalIgnoreCase
        )) {
            throw 'Refusing unsafe payload-contract test cleanup'
        }
        if ([IO.Directory]::Exists($candidate)) {
            [IO.Directory]::Delete($candidate, $true)
        }
    }
}

Write-Output (
    'payload-contract self-test: PASS ' +
    "($($PSVersionTable.PSVersion))"
)
