# Locked, unpredictable, same-directory parent/child GO signal.

function Test-KettlePerfGoSamePath {
    param(
        [Parameter(Mandatory)]
        [string]$Left,
        [Parameter(Mandatory)]
        [string]$Right
    )

    $leftFull = [IO.Path]::GetFullPath($Left).TrimEnd([char[]]@('\', '/'))
    $rightFull = [IO.Path]::GetFullPath($Right).TrimEnd([char[]]@('\', '/'))
    return [StringComparer]::OrdinalIgnoreCase.Equals(
        $leftFull,
        $rightFull
    )
}

function Assert-KettlePerfGoDirectory {
    param(
        [Parameter(Mandatory)]
        [string]$Directory
    )

    $full = [IO.Path]::GetFullPath($Directory)
    if (-not (Test-Path -LiteralPath $full -PathType Container)) {
        throw "GO signal directory does not exist: $full"
    }
    $current = $full
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        if (
            $item.PSProvider.Name -ne 'FileSystem' -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "GO signal path traverses a reparse point: $current"
        }
        $parent = [IO.Path]::GetDirectoryName($current)
        if (
            -not $parent -or
            (Test-KettlePerfGoSamePath -Left $current -Right $parent)
        ) {
            break
        }
        $current = $parent
    }
    return $full
}

function Assert-KettlePerfGoPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Directory
    )

    $root = Assert-KettlePerfGoDirectory -Directory $Directory
    $full = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($full)
    $leaf = [IO.Path]::GetFileName($full)
    if (
        -not (Test-KettlePerfGoSamePath -Left $parent -Right $root) -or
        $leaf -notmatch '^throughput-go-[0-9a-f]{32}\.signal$'
    ) {
        throw 'Throughput GO file must be a canonical direct child'
    }
    return $full
}

function New-KettlePerfGoDescriptor {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This only allocates an unpredictable unused path.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Directory
    )

    $Directory = Assert-KettlePerfGoDirectory -Directory $Directory
    $bytes = [byte[]]::new(32)
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $random.GetBytes($bytes)
    } finally {
        $random.Dispose()
    }
    $token = -join @($bytes | ForEach-Object { $_.ToString('x2') })
    for ($attempt = 0; $attempt -lt 16; $attempt++) {
        $leaf = (
            'throughput-go-' + [Guid]::NewGuid().ToString('N') + '.signal'
        )
        $path = Assert-KettlePerfGoPath `
            -Path (Join-Path $Directory $leaf) -Directory $Directory
        if (-not (Test-Path -LiteralPath $path)) {
            return [pscustomobject]@{
                Path = $path
                Directory = $Directory
                Token = $token
            }
        }
    }
    throw 'Could not allocate an unused throughput GO path'
}

function Publish-KettlePerfGoSignal {
    param(
        [Parameter(Mandatory)]
        $Descriptor
    )

    $path = Assert-KettlePerfGoPath `
        -Path ([string]$Descriptor.Path) `
        -Directory ([string]$Descriptor.Directory)
    $token = [string]$Descriptor.Token
    if ($token -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Throughput GO token is invalid'
    }
    $bytes = [Text.Encoding]::ASCII.GetBytes($token)
    $stream = [IO.FileStream]::new(
        $path,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::ReadWrite,
        [IO.FileShare]::Read
    )
    try {
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
        $stream.Position = 0
        return $stream
    } catch {
        $stream.Dispose()
        [IO.File]::Delete($path)
        throw
    }
}

function Wait-KettlePerfGoSignal {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Directory,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$Token,
        [ValidateRange(1, 300)]
        [int]$TimeoutSeconds = 30
    )

    $Path = Assert-KettlePerfGoPath -Path $Path -Directory $Directory
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
            if (
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne
                    0 -or
                $item.Length -ne 64
            ) {
                throw 'Throughput GO file is a reparse point or has invalid size'
            }
            $stream = [IO.FileStream]::new(
                $Path,
                [IO.FileMode]::Open,
                [IO.FileAccess]::Read,
                # The retained publisher has ReadWrite access but only shares
                # reads. This reader must acknowledge that existing write
                # access while requesting no write access of its own.
                [IO.FileShare]::ReadWrite
            )
            try {
                $bytes = [byte[]]::new(64)
                $offset = 0
                while ($offset -lt $bytes.Length) {
                    $count = $stream.Read(
                        $bytes,
                        $offset,
                        $bytes.Length - $offset
                    )
                    if ($count -eq 0) {
                        break
                    }
                    $offset += $count
                }
                if (
                    $offset -ne 64 -or
                    $stream.ReadByte() -ne -1
                ) {
                    throw 'Throughput GO file changed size while reading'
                }
                $text = [Text.UTF8Encoding]::new(
                    $false,
                    $true
                ).GetString($bytes)
                if (-not [StringComparer]::Ordinal.Equals($text, $Token)) {
                    throw 'Throughput GO file token is invalid'
                }
            } finally {
                $stream.Dispose()
            }
            $timer.Stop()
            return $timer.Elapsed.TotalMilliseconds
        }
        Start-Sleep -Milliseconds 10
    }
    throw 'Timed out waiting for the throughput GO file'
}

function Close-KettlePerfGoSignal {
    param(
        [Parameter(Mandatory)]
        $Descriptor,
        $Lock
    )

    $path = Assert-KettlePerfGoPath `
        -Path ([string]$Descriptor.Path) `
        -Directory ([string]$Descriptor.Directory)
    if ($null -ne $Lock) {
        $Lock.Dispose()
    }
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        [IO.File]::Delete($path)
    }
}
