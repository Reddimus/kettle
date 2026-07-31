# GUI-free tests for authenticated vtebench transport and private WSL framing.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\vtebench-channel.ps1"

function Assert-KettlePerfVtebenchChannelTest {
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

function Invoke-KettlePerfExpectedVtebenchChannelFailure {
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
        throw "Expected vtebench channel failure was accepted: $Description"
    }
}

function New-KettlePerfVtebenchPrivateTestBytes {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Builds an in-memory test frame only.'
    )]
    param(
        [ValidateRange(0, 1000)]
        [int]$Status = 0,
        [Parameter(Mandatory)]
        [byte[]]$DatBytes,
        [uint64]$DeclaredLength = [uint64]::MaxValue,
        [byte[]]$TrailingBytes = [byte[]]::new(0)
    )
    if ($DeclaredLength -eq [uint64]::MaxValue) {
        $DeclaredLength = [uint64]$DatBytes.Length
    }
    $bytes = [byte[]]::new(
        16 + $DatBytes.Length + $TrailingBytes.Length
    )
    $bytes[0] = [byte][char]'K'
    $bytes[1] = [byte][char]'V'
    $bytes[2] = [byte][char]'D'
    $bytes[3] = [byte][char]'1'
    Set-KettlePerfVtebenchUInt32 `
        -Bytes $bytes -Offset 4 -Value ([uint32]$Status)
    Set-KettlePerfVtebenchUInt64 `
        -Bytes $bytes -Offset 8 -Value $DeclaredLength
    [Array]::Copy($DatBytes, 0, $bytes, 16, $DatBytes.Length)
    if ($TrailingBytes.Length -gt 0) {
        [Array]::Copy(
            $TrailingBytes,
            0,
            $bytes,
            16 + $DatBytes.Length,
            $TrailingBytes.Length
        )
    }
    return $bytes
}

function Start-KettlePerfVtebenchChannelTestClient {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Starts only a bounded GUI-free test subprocess.'
    )]
    param(
        [Parameter(Mandatory)]
        $Descriptor,
        [Parameter(Mandatory)]
        [ValidateSet(
            'positive',
            'wrong-nonce',
            'status',
            'truncated',
            'invalid-utf8'
        )]
        [string]$Mode
    )

    $shell = (Get-Process -Id $PID -ErrorAction Stop).Path
    $helper = [IO.Path]::GetFullPath(
        (Join-Path $PSScriptRoot 'vtebench-channel.ps1')
    ).Replace("'", "''")
    $pipeName = ([string]$Descriptor.PipeName).Replace("'", "''")
    $nonce = ([string]$Descriptor.Nonce).Replace("'", "''")
    $child = @"
`$ErrorActionPreference = 'Stop'
. '$helper'
`$pipeName = '$pipeName'
`$nonce = '$nonce'
`$mode = '$Mode'
`$utf8 = [Text.UTF8Encoding]::new(`$false, `$true)
`$dat = `$utf8.GetBytes("bench`n1`n")
if (`$mode -eq 'wrong-nonce') {
    `$nonce = ('00' * 32)
}
if (`$mode -eq 'invalid-utf8') {
    `$dat = [byte[]]@(0xff)
}
if (`$mode -eq 'truncated') {
    `$frame = New-KettlePerfVtebenchChannelFrame ``
        -Nonce `$nonce -Status 0 -DatBytes `$dat -DeclaredLength 100
    try {
        Send-KettlePerfThroughputChannelFrame ``
            -PipeName `$pipeName -Frame `$frame ``
            -ConnectTimeoutMs 5000 -WriteTimeoutMs 5000 -AckTimeoutMs 5000
    } finally {
        [Array]::Clear(`$frame, 0, `$frame.Length)
    }
} else {
    `$status = if (`$mode -eq 'status') { 7 } else { 0 }
    Send-KettlePerfVtebenchChannelResult ``
        -PipeName `$pipeName -Nonce `$nonce -Status `$status ``
        -DatBytes `$dat -ConnectTimeoutMs 5000 ``
        -WriteTimeoutMs 5000 -AckTimeoutMs 5000
}
[Array]::Clear(`$dat, 0, `$dat.Length)
"@
    $encoded = [Convert]::ToBase64String(
        [Text.Encoding]::Unicode.GetBytes($child)
    )
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $shell
    $startInfo.Arguments = (
        '-NoLogo -NoProfile -NonInteractive -EncodedCommand ' +
        $encoded
    )
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        $process.Dispose()
        throw "Could not start vtebench channel test client: $Mode"
    }
    return $process
}

function Close-KettlePerfVtebenchChannelTestClient {
    param(
        $Process,
        [switch]$RequireSuccess
    )
    if ($null -eq $Process) {
        return
    }
    try {
        if (-not $Process.WaitForExit(7000)) {
            try {
                $Process.Kill()
            } catch {
                Write-Verbose (
                    'vtebench test-client cleanup raced exit: ' +
                    $_.Exception.Message
                )
            }
            throw 'vtebench channel test client did not exit'
        }
        $stdout = $Process.StandardOutput.ReadToEnd()
        $stderr = $Process.StandardError.ReadToEnd()
        if ($RequireSuccess -and $Process.ExitCode -ne 0) {
            throw "vtebench channel test client failed: $stderr$stdout"
        }
    } finally {
        $Process.Dispose()
    }
}

function Invoke-KettlePerfVtebenchChannelCase {
    param(
        [Parameter(Mandatory)]
        [string]$Mode,
        [switch]$ExpectSuccess,
        [switch]$WrongExpectedPid
    )
    $descriptor = $null
    $client = $null
    $received = $null
    $failed = $false
    try {
        $descriptor = New-KettlePerfVtebenchChannelDescriptor `
            -MaximumDatBytes 1024
        $client = Start-KettlePerfVtebenchChannelTestClient `
            -Descriptor $descriptor -Mode $Mode
        $expectedPid = if ($WrongExpectedPid) { $PID } else { $client.Id }
        try {
            $received = Receive-KettlePerfVtebenchChannelResult `
                -Descriptor $descriptor `
                -ExpectedWorkloadPid $expectedPid `
                -ExpectedTerminalPid $PID `
                -ExpectedColumns 1 -ConnectTimeoutMs 5000 `
                -ReadTimeoutMs 3000 -AckTimeoutMs 3000
        } catch {
            $failed = $true
        }
        if ($ExpectSuccess) {
            Assert-KettlePerfVtebenchChannelTest (
                -not $failed -and
                $null -ne $received -and
                $received.ClientPid -eq $client.Id -and
                $received.Parsed.Names[0] -ceq 'bench' -and
                $received.Parsed.Samples.bench[0] -eq 1
            ) "Valid vtebench channel exchange failed: $Mode"
        } else {
            Assert-KettlePerfVtebenchChannelTest (
                $failed
            ) "Hostile vtebench channel exchange was accepted: $Mode"
        }
    } finally {
        Close-KettlePerfThroughputChannel $descriptor
        if ($null -ne $received -and $null -ne $received.DatBytes) {
            [Array]::Clear(
                $received.DatBytes,
                0,
                $received.DatBytes.Length
            )
        }
        Close-KettlePerfVtebenchChannelTestClient `
            -Process $client -RequireSuccess:$ExpectSuccess
    }
}

if (-not $script:KettlePerfThroughputChannelIsWindows) {
    Write-Output 'vtebench-channel self-test: SKIP (Windows required)'
    return
}

Assert-KettlePerfVtebenchChannelTest (
    $script:KettlePerfVtebenchMaximumDatBytes -eq 1MB
) 'vtebench DAT memory bound is not the pinned 1 MiB cap'

$utf8 = [Text.UTF8Encoding]::new($false, $true)
$validDat = $utf8.GetBytes("bench`n1`n")
$privateBytes = New-KettlePerfVtebenchPrivateTestBytes `
    -DatBytes $validDat
$privateStream = [IO.MemoryStream]::new($privateBytes, $false)
try {
    $private = Read-KettlePerfVtebenchPrivateFrame `
        -Stream $privateStream -MaximumDatBytes 1024 -TimeoutMs 1000
    Assert-KettlePerfVtebenchChannelTest (
        $private.Status -eq 0 -and
        [Text.Encoding]::UTF8.GetString($private.DatBytes) -ceq
            "bench`n1`n"
    ) 'Valid private vtebench frame did not round trip'
    [Array]::Clear($private.DatBytes, 0, $private.DatBytes.Length)
} finally {
    $privateStream.Dispose()
    [Array]::Clear($privateBytes, 0, $privateBytes.Length)
}

foreach ($privateCase in @(
    [pscustomobject]@{
        Name = 'oversize'
        Bytes = New-KettlePerfVtebenchPrivateTestBytes `
            -DatBytes $validDat -DeclaredLength 1025
    },
    [pscustomobject]@{
        Name = 'truncated'
        Bytes = New-KettlePerfVtebenchPrivateTestBytes `
            -DatBytes $validDat -DeclaredLength 100
    },
    [pscustomobject]@{
        Name = 'invalid status'
        Bytes = New-KettlePerfVtebenchPrivateTestBytes `
            -DatBytes $validDat -Status 256
    },
    [pscustomobject]@{
        Name = 'trailing bytes'
        Bytes = New-KettlePerfVtebenchPrivateTestBytes `
            -DatBytes $validDat -TrailingBytes ([byte[]]@(1))
    }
)) {
    $caseStream = [IO.MemoryStream]::new($privateCase.Bytes, $false)
    try {
        Invoke-KettlePerfExpectedVtebenchChannelFailure `
            -Description $privateCase.Name `
            -Action {
                Read-KettlePerfVtebenchPrivateFrame `
                    -Stream $caseStream -MaximumDatBytes 1024 `
                    -TimeoutMs 1000
            }
    } finally {
        $caseStream.Dispose()
        [Array]::Clear(
            $privateCase.Bytes,
            0,
            $privateCase.Bytes.Length
        )
    }
}

Invoke-KettlePerfVtebenchChannelCase -Mode positive -ExpectSuccess
Invoke-KettlePerfVtebenchChannelCase -Mode wrong-nonce
Invoke-KettlePerfVtebenchChannelCase -Mode status
Invoke-KettlePerfVtebenchChannelCase -Mode truncated
Invoke-KettlePerfVtebenchChannelCase -Mode invalid-utf8
Invoke-KettlePerfVtebenchChannelCase `
    -Mode positive -WrongExpectedPid

$scratch = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-vtebench-channel-' + [Guid]::NewGuid().ToString('N')
)
[void][IO.Directory]::CreateDirectory($scratch)
try {
    $preplaced = Join-Path $scratch 'vtebench-kettle.dat'
    [IO.File]::WriteAllText($preplaced, 'retain me')
    Invoke-KettlePerfExpectedVtebenchChannelFailure `
        -Description 'preplaced raw DAT' `
        -Action {
            Publish-KettlePerfVtebenchDat `
                -Path $preplaced -ResultsDirectory $scratch `
                -Bytes $validDat
        }
    Assert-KettlePerfVtebenchChannelTest (
        [IO.File]::ReadAllText($preplaced) -ceq 'retain me'
    ) 'Preplaced DAT was modified'

    $publishedPath = Join-Path $scratch 'vtebench-wt.dat'
    $published = Publish-KettlePerfVtebenchDat `
        -Path $publishedPath -ResultsDirectory $scratch `
        -Bytes $validDat
    Assert-KettlePerfVtebenchChannelTest (
        $published.Sha256 -ceq (
            Get-KettlePerfVtebenchBytesSha256 -Bytes $validDat
        ) -and
        [IO.File]::ReadAllBytes($publishedPath).Length -eq
            $validDat.Length
    ) 'Authenticated raw DAT publication is invalid'

    $heldRoot = Join-Path $scratch 'held-root'
    [void][IO.Directory]::CreateDirectory($heldRoot)
    Initialize-KettlePerfVtebenchPublicationNative
    $rootHandle = (
        [KettlePerfVtebenchPublication.NativeMethods]::OpenRoot($heldRoot)
    )
    $relativeHandle = $null
    $relativeStream = $null
    try {
        $renameRejected = $false
        try {
            [IO.Directory]::Move(
                $heldRoot,
                (Join-Path $scratch 'swapped-root')
            )
        } catch {
            $renameRejected = $true
        }
        Assert-KettlePerfVtebenchChannelTest (
            $renameRejected
        ) 'Held publication root allowed a path-swap rename'
        $relativeHandle = (
            [KettlePerfVtebenchPublication.NativeMethods]::CreateRelative(
                $rootHandle,
                'vtebench-rio.dat'
            )
        )
        $relativeStream = [IO.FileStream]::new(
            $relativeHandle,
            [IO.FileAccess]::Write
        )
        $relativeHandle = $null
        $relativeStream.Write($validDat, 0, $validDat.Length)
        $relativeStream.Flush($true)
    } finally {
        if ($null -ne $relativeStream) {
            $relativeStream.Dispose()
        }
        if ($null -ne $relativeHandle) {
            $relativeHandle.Dispose()
        }
        $rootHandle.Dispose()
    }
    Assert-KettlePerfVtebenchChannelTest (
        [IO.File]::Exists(
            (Join-Path $heldRoot 'vtebench-rio.dat')
        )
    ) 'Root-relative publication did not target the retained directory'

    $orchestrator = [IO.File]::ReadAllText(
        (Join-Path $PSScriptRoot 'vtebench-wsl.ps1')
    )
    $wrapper = [IO.File]::ReadAllText(
        (Join-Path $PSScriptRoot 'vtebench-inside.ps1')
    )
    $perfAll = [IO.File]::ReadAllText(
        (Join-Path $PSScriptRoot 'perf-all.ps1')
    )
    $launcher = [IO.File]::ReadAllText(
        (Join-Path $PSScriptRoot 'wsl-launcher.ps1')
    )
    Assert-KettlePerfVtebenchChannelTest (
        $orchestrator -notmatch '\.status' -and
        $orchestrator -notmatch 'resultsWsl' -and
        $orchestrator -match
            'Receive-KettlePerfVtebenchChannelResult' -and
        $wrapper -match
            'RedirectStandardOutput = \$false' -and
        $wrapper -match
            'RedirectStandardError = \$true' -and
        $wrapper -notmatch 'Get-Command\s+wsl' -and
        $wrapper -match 'set -euo pipefail' -and
        $wrapper -match 'exec -a "\$1"' -and
        $wrapper -match '"\$setsid_real" --fork --wait' -and
        $wrapper -match 'SETSID_SHA256' -and
        $wrapper -match 'Stop-KettlePerfWslMarkedProcess' -and
        $orchestrator -notmatch '&\s+wsl\.exe' -and
        $orchestrator.Contains(
            ("'-WslExe', " + '$WslExe')
        ) -and
        $orchestrator.Contains(
            ("'-SetsidWsl', " + '$wslSetsidPath')
        ) -and
        $orchestrator -match 'SCRIPT_SHA256' -and
        $orchestrator -match '\$script_path" -qefc' -and
        $launcher -match 'exec setsid --fork --wait' -and
        $launcher -match 'kill "-\$signal" -- "-\$pgid"' -and
        $orchestrator -match 'wsl_launcher = \[ordered\]' -and
        $perfAll -match
            '-WslResolutionPolicy \$wslLauncherEvidence\.ResolutionPolicy' -and
        $launcher.IndexOf(
            "'WSL\wsl.exe'",
            [StringComparison]::Ordinal
        ) -lt $launcher.IndexOf(
            "'System32\wsl.exe'",
            [StringComparison]::Ordinal
        ) -and
        $launcher -match 'OpenPinnedFile' -and
        $launcher -match 'VersionOutputSha256'
    ) 'vtebench retained a Windows-path WSL handoff'
} finally {
    $scratchFull = [IO.Path]::GetFullPath($scratch)
    $tempFull = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $scratchFull.StartsWith(
        $tempFull,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'Refusing unsafe vtebench channel test cleanup'
    }
    [IO.Directory]::Delete($scratchFull, $true)
    [Array]::Clear($validDat, 0, $validDat.Length)
}

Write-Output (
    'vtebench-channel self-test: PASS ' +
    "($($PSVersionTable.PSVersion))"
)
