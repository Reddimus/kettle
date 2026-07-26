# GUI-free positive and hostile-input tests for the throughput named pipe.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\throughput-channel.ps1"

function Assert-KettlePerfThroughputChannelTest {
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

function Invoke-KettlePerfExpectedChannelFailure {
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
        throw "Expected throughput channel failure was accepted: $Description"
    }
}

function Start-KettlePerfThroughputChannelTestClient {
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
            'oversize',
            'invalid-utf8',
            'truncated',
            'trailing',
            'bom',
            'invalid-json',
            'mismatched-json',
            'deep-json',
            'wide-json',
            'duplicate-json'
        )]
        [string]$Mode
    )

    $shell = (Get-Process -Id $PID -ErrorAction Stop).Path
    $helper = [IO.Path]::GetFullPath(
        (Join-Path $PSScriptRoot 'throughput-channel.ps1')
    ).Replace("'", "''")
    $pipeName = ([string]$Descriptor.PipeName).Replace("'", "''")
    $nonce = ([string]$Descriptor.Nonce).Replace("'", "''")
    $child = @"
`$ErrorActionPreference = 'Stop'
. '$helper'
`$pipeName = '$pipeName'
`$nonce = '$nonce'
`$mode = '$Mode'
if (`$mode -eq 'positive') {
    Send-KettlePerfThroughputChannelJson ``
        -PipeName `$pipeName -Nonce `$nonce ``
        -InputObject ([ordered]@{
            ok = `$true
            text = 'strict UTF-8'
        }) -MaximumBytes 1024 ``
        -ConnectTimeoutMs 5000 -WriteTimeoutMs 5000 -AckTimeoutMs 5000
} else {
    `$utf8 = [Text.UTF8Encoding]::new(`$false, `$true)
    `$declared = [uint32]::MaxValue
    switch (`$mode) {
        'wrong-nonce' {
            `$nonce = ('00' * 32)
            `$body = `$utf8.GetBytes('{}')
        }
        'oversize' {
            `$body = [byte[]]::new(0)
            `$declared = [uint32]1025
        }
        'invalid-utf8' {
            `$body = [byte[]]@(0xff)
        }
        'truncated' {
            `$body = `$utf8.GetBytes('{}')
            `$declared = [uint32]10
        }
        'trailing' {
            `$body = `$utf8.GetBytes('{}x')
            `$declared = [uint32]2
        }
        'bom' {
            `$body = [byte[]]@(0xef, 0xbb, 0xbf, 0x7b, 0x7d)
        }
        'invalid-json' {
            `$body = `$utf8.GetBytes('{broken')
        }
        'mismatched-json' {
            `$body = `$utf8.GetBytes('{"items":[1,2}')
        }
        'deep-json' {
            `$bodyText = ('{"nested":' * 33) + '0' + ('}' * 33)
            `$body = `$utf8.GetBytes(`$bodyText)
        }
        'wide-json' {
            `$bodyText = (
                '{"items":[' + ('0,' * 10000) + '0]}'
            )
            `$body = `$utf8.GetBytes(`$bodyText)
        }
        'duplicate-json' {
            `$body = `$utf8.GetBytes('{"sample":1,"Sample":2}')
        }
    }
    `$frame = New-KettlePerfThroughputChannelFrame ``
        -Nonce `$nonce -JsonBytes `$body -DeclaredLength `$declared
    try {
        Send-KettlePerfThroughputChannelFrame ``
            -PipeName `$pipeName -Frame `$frame ``
            -ConnectTimeoutMs 5000 -WriteTimeoutMs 5000 -AckTimeoutMs 5000
    } finally {
        [Array]::Clear(`$frame, 0, `$frame.Length)
        if (`$null -ne `$body) {
            [Array]::Clear(`$body, 0, `$body.Length)
        }
    }
}
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
        throw "Could not start throughput channel test client: $Mode"
    }
    return $process
}

function Close-KettlePerfThroughputChannelTestClient {
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
                    'Throughput channel client cleanup raced exit: ' +
                    $_.Exception.Message
                )
            }
            throw 'Throughput channel test client did not exit'
        }
        $stdout = $Process.StandardOutput.ReadToEnd()
        $stderr = $Process.StandardError.ReadToEnd()
        if ($RequireSuccess -and $Process.ExitCode -ne 0) {
            throw (
                "Throughput channel test client failed: $stderr$stdout"
            )
        }
    } finally {
        $Process.Dispose()
    }
}

function Invoke-KettlePerfThroughputChannelCase {
    param(
        [Parameter(Mandatory)]
        [string]$Mode,
        [switch]$ExpectSuccess,
        [switch]$WrongExpectedPid,
        [int]$ExpectedTerminalPid = 0,
        [string]$ExpectedErrorPattern = ''
    )

    $descriptor = $null
    $client = $null
    $received = $null
    $receiveFailed = $false
    $receiveError = ''
    try {
        $maximumBytes = if ($Mode -eq 'wide-json') {
            64KB
        } else {
            1024
        }
        $descriptor = New-KettlePerfThroughputChannelDescriptor `
            -MaximumBytes $maximumBytes
        $client = Start-KettlePerfThroughputChannelTestClient `
            -Descriptor $descriptor -Mode $Mode
        $expectedPid = if ($WrongExpectedPid) {
            $PID
        } else {
            $client.Id
        }
        $terminalPid = if ($ExpectedTerminalPid -gt 0) {
            $ExpectedTerminalPid
        } else {
            $PID
        }
        try {
            $received = Receive-KettlePerfThroughputChannelJson `
                -Descriptor $descriptor `
                -ExpectedWorkloadPid $expectedPid `
                -ExpectedTerminalPid $terminalPid `
                -ConnectTimeoutMs 5000 `
                -ReadTimeoutMs 3000 `
                -AckTimeoutMs 3000
        } catch {
            $receiveFailed = $true
            $receiveError = $_.Exception.Message
        }
        if ($ExpectSuccess) {
            Assert-KettlePerfThroughputChannelTest (
                -not $receiveFailed -and
                $null -ne $received -and
                $received.ClientPid -eq $client.Id -and
                $received.Value.ok -eq $true -and
                $received.Value.text -ceq 'strict UTF-8'
            ) "Valid throughput channel exchange failed: $Mode"
        } else {
            Assert-KettlePerfThroughputChannelTest (
                $receiveFailed -and
                (
                    -not $ExpectedErrorPattern -or
                    $receiveError -match $ExpectedErrorPattern
                )
            ) (
                "Hostile throughput channel exchange was accepted or " +
                "failed for the wrong reason: $Mode ($receiveError)"
            )
        }
    } finally {
        Close-KettlePerfThroughputChannel $descriptor
        Close-KettlePerfThroughputChannelTestClient `
            -Process $client -RequireSuccess:$ExpectSuccess
    }
}

if (-not $script:KettlePerfThroughputChannelIsWindows) {
    Write-Output 'throughput-channel self-test: SKIP (Windows required)'
    return
}

$first = New-KettlePerfThroughputChannelDescriptor -MaximumBytes 1024
$second = New-KettlePerfThroughputChannelDescriptor -MaximumBytes 1024
try {
    Assert-KettlePerfThroughputChannelTest (
        $first.PipeName -cne $second.PipeName -and
        $first.Nonce -cne $second.Nonce -and
        $first.PipeName -cmatch
            '^kettle-perf-throughput-[0-9a-f]{48}$' -and
        $first.Nonce -cmatch '^[0-9a-f]{64}$' -and
        $script:KettlePerfThroughputChannelAckFrame.Length -eq 1 -and
        $script:KettlePerfThroughputChannelAckFrame[0] -eq 0xa5 -and
        $first.SecurityMode -in @(
            'current-user-only-first-instance',
            'explicit-owner-only-acl'
        )
    ) 'Throughput channel names, nonces, or owner restriction are invalid'
} finally {
    Close-KettlePerfThroughputChannel $second
    Close-KettlePerfThroughputChannel $first
}

$snapshotTimer = [Diagnostics.Stopwatch]::StartNew()
$processParents = Get-KettlePerfThroughputChannelProcessSnapshot `
    -Timer $snapshotTimer -TimeoutMs 3000
Assert-KettlePerfThroughputChannelTest (
    $snapshotTimer.ElapsedMilliseconds -lt 3000 -and
    $processParents.Count -le
        $script:KettlePerfThroughputChannelMaximumProcesses -and
    $processParents.ContainsKey($PID)
) 'Native process enumeration was incomplete or exceeded its deadline'
Invoke-KettlePerfExpectedChannelFailure `
    -Description 'native process snapshot record bound' `
    -Action {
        $boundedSnapshotTimer = [Diagnostics.Stopwatch]::StartNew()
        Get-KettlePerfThroughputChannelProcessSnapshot `
            -Timer $boundedSnapshotTimer -TimeoutMs 3000 `
            -MaximumProcesses 1
    }
$expiredSnapshotTimer = [Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Milliseconds 20
$expiredSnapshotReturn = [Diagnostics.Stopwatch]::StartNew()
Invoke-KettlePerfExpectedChannelFailure `
    -Description 'expired process snapshot deadline' `
    -Action {
        Get-KettlePerfThroughputChannelProcessSnapshot `
            -Timer $expiredSnapshotTimer -TimeoutMs 1
    }
$expiredSnapshotReturn.Stop()
Assert-KettlePerfThroughputChannelTest (
    $expiredSnapshotReturn.ElapsedMilliseconds -lt 1000
) 'Expired process-snapshot deadline did not fail immediately'

$syntheticParents = [Collections.Generic.Dictionary[int, int]]::new()
$syntheticParents.Add(30, 20)
$syntheticParents.Add(20, 10)
$syntheticParents.Add(40, 41)
$syntheticParents.Add(41, 40)
Assert-KettlePerfThroughputChannelTest (
    (
        Test-KettlePerfThroughputChannelProcessRelated `
            -CandidatePid 30 -RootPid 10 -Parents $syntheticParents
    ) -and
    -not (
        Test-KettlePerfThroughputChannelProcessRelated `
            -CandidatePid 30 -RootPid 99 -Parents $syntheticParents
    ) -and
    -not (
        Test-KettlePerfThroughputChannelProcessRelated `
            -CandidatePid 40 -RootPid 10 -Parents $syntheticParents
    )
) 'Ancestry validation accepted an unrelated or cyclic process chain'

Invoke-KettlePerfExpectedChannelFailure `
    -Description 'pipe-name command injection' `
    -Action {
        Assert-KettlePerfThroughputChannelName `
            'kettle-perf-throughput-bad;whoami'
    }
Invoke-KettlePerfExpectedChannelFailure `
    -Description 'nonce command injection' `
    -Action {
        Assert-KettlePerfThroughputChannelNonce ('00' * 31 + ';x')
    }

Invoke-KettlePerfThroughputChannelCase `
    -Mode positive -ExpectSuccess
Invoke-KettlePerfThroughputChannelCase -Mode wrong-nonce
Invoke-KettlePerfThroughputChannelCase -Mode oversize
Invoke-KettlePerfThroughputChannelCase -Mode invalid-utf8
Invoke-KettlePerfThroughputChannelCase -Mode truncated
Invoke-KettlePerfThroughputChannelCase -Mode trailing
Invoke-KettlePerfThroughputChannelCase -Mode bom
Invoke-KettlePerfThroughputChannelCase -Mode invalid-json
Invoke-KettlePerfThroughputChannelCase `
    -Mode mismatched-json -ExpectedErrorPattern 'mismatched delimiters'
Invoke-KettlePerfThroughputChannelCase `
    -Mode deep-json -ExpectedErrorPattern 'depth bound'
Invoke-KettlePerfThroughputChannelCase `
    -Mode wide-json -ExpectedErrorPattern 'token bound'
Invoke-KettlePerfThroughputChannelCase `
    -Mode duplicate-json -ExpectedErrorPattern 'duplicate property'
Invoke-KettlePerfThroughputChannelCase `
    -Mode positive -WrongExpectedPid

$decoyCommand = [Convert]::ToBase64String(
    [Text.Encoding]::Unicode.GetBytes('Start-Sleep -Seconds 30')
)
$decoyInfo = [Diagnostics.ProcessStartInfo]::new()
$decoyInfo.FileName = (Get-Process -Id $PID -ErrorAction Stop).Path
$decoyInfo.Arguments = (
    '-NoLogo -NoProfile -NonInteractive -EncodedCommand ' +
    $decoyCommand
)
$decoyInfo.UseShellExecute = $false
$decoyInfo.CreateNoWindow = $true
$decoy = [Diagnostics.Process]::new()
$decoy.StartInfo = $decoyInfo
$decoyStarted = $false
try {
    if (-not $decoy.Start()) {
        throw 'Could not start the unrelated ancestry test process'
    }
    $decoyStarted = $true
    Invoke-KettlePerfThroughputChannelCase `
        -Mode positive -ExpectedTerminalPid $decoy.Id `
        -ExpectedErrorPattern 'outside.*process ancestry'
} finally {
    try {
        if ($decoyStarted -and -not $decoy.HasExited) {
            $decoy.Kill()
            [void]$decoy.WaitForExit(3000)
        }
    } catch {
        Write-Verbose (
            'Throughput ancestry decoy cleanup raced exit: ' +
            $_.Exception.Message
        )
    }
    $decoy.Dispose()
}

$timeoutDescriptor = New-KettlePerfThroughputChannelDescriptor `
    -MaximumBytes 1024
$timeoutTimer = [Diagnostics.Stopwatch]::StartNew()
try {
    Invoke-KettlePerfExpectedChannelFailure `
        -Description 'finite connection timeout' `
        -Action {
            Receive-KettlePerfThroughputChannelJson `
                -Descriptor $timeoutDescriptor `
                -ExpectedWorkloadPid $PID `
                -ExpectedTerminalPid $PID `
                -ConnectTimeoutMs 150 `
                -ReadTimeoutMs 500 `
                -AckTimeoutMs 500
        }
} finally {
    $timeoutTimer.Stop()
    Close-KettlePerfThroughputChannel $timeoutDescriptor
}
Assert-KettlePerfThroughputChannelTest (
    $timeoutTimer.ElapsedMilliseconds -lt 3000
) 'Throughput channel connection timeout was not finite'

$orchestratorText = [IO.File]::ReadAllText(
    (Join-Path $PSScriptRoot 'throughput.ps1')
)
Assert-KettlePerfThroughputChannelTest (
    $orchestratorText -notmatch 'throughput-sample-' -and
    $orchestratorText -match
        'Receive-KettlePerfThroughputChannelJson'
) 'Throughput orchestrator retained a filesystem sample handoff'

Write-Output (
    'throughput-channel self-test: PASS ' +
    "($($PSVersionTable.PSVersion))"
)
