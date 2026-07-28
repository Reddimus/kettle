# Immutable source provenance for the Windows performance capture/scoring
# harness. The caller retains the returned streams for the complete run.

$script:KettlePerfHarnessFiles = [string[]]@(
    'baseline-statistics.ps1',
    'comparator-campaign.ps1',
    'display-identity-contract.ps1',
    'display-stability.ps1',
    'evidence-snapshot.ps1',
    'gen-payloads.ps1',
    'go-signal.ps1',
    'harness-provenance.ps1',
    'isolated-configs.ps1',
    'json-io.ps1',
    'latency.ps1',
    'lib-win32.ps1',
    'menu-hover.ps1',
    'monitor-transition.ps1',
    'payload-contract.ps1',
    'perf-all.ps1',
    'process-capture.ps1',
    'release-contract.ps1',
    'release-statistics.ps1',
    'run-inside.ps1',
    'sanitize-results.ps1',
    'schedule.ps1',
    'score-statistics.ps1',
    'score.ps1',
    'setup-comparator-campaign.ps1',
    'startup-idle.ps1',
    'startup-ready.ps1',
    'statistics.ps1',
    'terminal-specs.ps1',
    'throughput.ps1',
    'throughput-channel.ps1',
    'vtebench-dat.ps1',
    'vtebench-channel.ps1',
    'vtebench-inside.ps1',
    'vtebench-wsl.ps1',
    'wsl-launcher.ps1'
)

function Get-KettlePerfHarnessFileNames {
    return [string[]]@($script:KettlePerfHarnessFiles)
}

function Assert-KettlePerfOrdinaryFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$ExpectedParent
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($fullPath)
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $parent,
        $ExpectedParent
    )) {
        throw "Harness file escapes its owning directory: $Path"
    }
    $item = Get-Item -LiteralPath $fullPath -Force -ErrorAction Stop
    if (
        -not ($item -is [IO.FileInfo]) -or
        (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)
    ) {
        throw "Harness source must be an ordinary file: $fullPath"
    }
    return $item
}

function Open-KettlePerfHarnessLocks {
    param(
        [string]$ScriptDirectory = $PSScriptRoot
    )

    $directory = [IO.Path]::GetFullPath($ScriptDirectory)
    $directoryItem = Get-Item -LiteralPath $directory -Force -ErrorAction Stop
    if (
        -not ($directoryItem -is [IO.DirectoryInfo]) -or
        (($directoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)
    ) {
        throw "Harness directory must be an ordinary directory: $directory"
    }

    $locks = [System.Collections.Generic.List[object]]::new()
    try {
        foreach ($name in $script:KettlePerfHarnessFiles) {
            if (
                $name -notmatch '^[a-z0-9][a-z0-9-]*\.ps1$' -or
                [IO.Path]::GetFileName($name) -ne $name
            ) {
                throw "Invalid pinned harness filename: $name"
            }
            $item = Assert-KettlePerfOrdinaryFile `
                -Path (Join-Path $directory $name) `
                -ExpectedParent $directory
            $stream = [IO.File]::Open(
                $item.FullName,
                [IO.FileMode]::Open,
                [IO.FileAccess]::Read,
                [IO.FileShare]::Read
            )
            $locks.Add([pscustomobject]@{
                name = $name
                path = $item.FullName
                stream = $stream
            })
        }
        return [object[]]$locks.ToArray()
    } catch {
        foreach ($lock in $locks) {
            $lock.stream.Dispose()
        }
        throw
    }
}

function Close-KettlePerfHarnessLocks {
    param(
        [object[]]$Locks
    )

    foreach ($lock in @($Locks)) {
        if ($null -ne $lock -and $null -ne $lock.stream) {
            $lock.stream.Dispose()
        }
    }
}

function Get-KettlePerfHarnessProvenance {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Locks
    )

    if ($Locks.Count -ne $script:KettlePerfHarnessFiles.Count) {
        throw 'Harness lock coverage does not match the pinned file set.'
    }

    $records = [System.Collections.Generic.List[object]]::new()
    foreach ($name in $script:KettlePerfHarnessFiles) {
        $matchingLocks = @($Locks | Where-Object { $_.name -ceq $name })
        if ($matchingLocks.Count -ne 1) {
            throw "Harness lock coverage is not unique for $name"
        }
        $stream = $matchingLocks[0].stream
        if ($null -eq $stream -or -not $stream.CanRead) {
            throw "Harness source lock is no longer readable: $name"
        }
        $stream.Position = 0
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            $digest = $sha.ComputeHash($stream)
        } finally {
            $sha.Dispose()
            $stream.Position = 0
        }
        $records.Add([ordered]@{
            path = $name
            bytes = [long]$stream.Length
            sha256 = (
                [BitConverter]::ToString($digest).Replace('-', '').
                    ToLowerInvariant()
            )
        })
    }

    $aggregateText = [Text.StringBuilder]::new()
    foreach ($record in $records) {
        [void]$aggregateText.Append([string]$record.path)
        [void]$aggregateText.Append([char]0)
        [void]$aggregateText.Append([string]$record.sha256)
        [void]$aggregateText.Append("`n")
    }
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    $aggregateSha = [Security.Cryptography.SHA256]::Create()
    try {
        $aggregateDigest = $aggregateSha.ComputeHash(
            $utf8.GetBytes($aggregateText.ToString())
        )
    } finally {
        $aggregateSha.Dispose()
    }

    return [ordered]@{
        schema_version = 1
        lock_protocol = 'file-share-read-no-write-delete-v1'
        files = [object[]]$records.ToArray()
        aggregate_sha256 = (
            [BitConverter]::ToString($aggregateDigest).Replace('-', '').
                ToLowerInvariant()
        )
    }
}
