# Authenticated, bounded local named-pipe transport for one throughput sample.
# The parent creates the unpredictable pipe before launching the terminal. The
# child sends one message-mode frame and remains connected for an ACK so the
# parent can attribute the live client PID before accepting benchmark evidence.

$script:KettlePerfThroughputChannelSchema = 'kettle-throughput-channel-v1'
$script:KettlePerfThroughputChannelMaximumBytes = 4MB
$script:KettlePerfAuthenticatedChannelAbsoluteMaximumBytes = 64MB
$script:KettlePerfThroughputChannelHeaderBytes = 40
$script:KettlePerfThroughputChannelAck = 0xa5
$script:KettlePerfThroughputChannelAckFrame = [byte[]]@(
    $script:KettlePerfThroughputChannelAck
)
$script:KettlePerfThroughputChannelMaximumJsonDepth = 32
$script:KettlePerfThroughputChannelMaximumJsonNodes = 10000
$script:KettlePerfThroughputChannelMaximumProcesses = 65536
$script:KettlePerfThroughputChannelScriptRoot = $PSScriptRoot
$script:KettlePerfThroughputChannelIsWindows = (
    [IO.Path]::DirectorySeparatorChar -eq '\'
)

function Initialize-KettlePerfThroughputChannelNative {
    if (
        -not $script:KettlePerfThroughputChannelIsWindows -or
        ('KettlePerfThroughputChannel.NativeMethods' -as [type])
    ) {
        return
    }

    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfThroughputChannel {
    public static class NativeMethods {
        private const uint Th32csSnapProcess = 0x00000002;
        private const int ErrorNoMoreFiles = 18;
        private static readonly IntPtr InvalidHandleValue =
            new IntPtr(-1);

        [StructLayout(
            LayoutKind.Sequential,
            CharSet = CharSet.Unicode)]
        private struct ProcessEntry32 {
            internal uint Size;
            internal uint Usage;
            internal uint ProcessId;
            internal UIntPtr DefaultHeapId;
            internal uint ModuleId;
            internal uint Threads;
            internal uint ParentProcessId;
            internal int BasePriority;
            internal uint Flags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
            internal string ExecutableFile;
        }

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetNamedPipeClientProcessId(
            SafePipeHandle pipe,
            out uint clientProcessId);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr CreateToolhelp32Snapshot(
            uint flags,
            uint processId);

        [DllImport(
            "kernel32.dll",
            CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern bool Process32First(
            IntPtr snapshot,
            ref ProcessEntry32 entry);

        [DllImport(
            "kernel32.dll",
            CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern bool Process32Next(
            IntPtr snapshot,
            ref ProcessEntry32 entry);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        public static int GetClientProcessId(
            NamedPipeServerStream server) {
            if (server == null) {
                throw new ArgumentNullException("server");
            }
            if (!server.IsConnected) {
                throw new InvalidOperationException(
                    "Throughput channel has no connected client");
            }
            uint processId;
            if (!GetNamedPipeClientProcessId(
                    server.SafePipeHandle, out processId)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "GetNamedPipeClientProcessId failed");
            }
            if (processId == 0 || processId > Int32.MaxValue) {
                throw new InvalidOperationException(
                    "Throughput channel returned an invalid client PID");
            }
            return checked((int)processId);
        }

        public static Dictionary<int, int> GetProcessParents(
            int timeoutMilliseconds,
            int maximumProcesses) {
            if (timeoutMilliseconds < 1) {
                throw new ArgumentOutOfRangeException(
                    "timeoutMilliseconds");
            }
            if (maximumProcesses < 1 ||
                maximumProcesses > 131072) {
                throw new ArgumentOutOfRangeException(
                    "maximumProcesses");
            }

            var timer = Stopwatch.StartNew();
            var snapshot = CreateToolhelp32Snapshot(
                Th32csSnapProcess, 0);
            if (snapshot == InvalidHandleValue) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Creating the throughput process snapshot failed");
            }
            try {
                AssertBeforeDeadline(timer, timeoutMilliseconds);
                var entry = new ProcessEntry32();
                entry.Size = checked(
                    (uint)Marshal.SizeOf(typeof(ProcessEntry32)));
                if (!Process32First(snapshot, ref entry)) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Reading the throughput process snapshot failed");
                }
                var parents = new Dictionary<int, int>();
                while (true) {
                    AssertBeforeDeadline(timer, timeoutMilliseconds);
                    if (entry.ProcessId > Int32.MaxValue ||
                        entry.ParentProcessId > Int32.MaxValue) {
                        throw new InvalidDataException(
                            "Process snapshot contains an invalid PID");
                    }
                    if (parents.Count >= maximumProcesses) {
                        throw new InvalidDataException(
                            "Process snapshot exceeds its record bound");
                    }
                    var processId = checked((int)entry.ProcessId);
                    if (parents.ContainsKey(processId)) {
                        throw new InvalidDataException(
                            "Process snapshot contains an invalid " +
                            "or duplicate PID");
                    }
                    parents.Add(
                        processId,
                        checked((int)entry.ParentProcessId));

                    entry.Size = checked(
                        (uint)Marshal.SizeOf(typeof(ProcessEntry32)));
                    if (Process32Next(snapshot, ref entry)) {
                        AssertBeforeDeadline(
                            timer, timeoutMilliseconds);
                        continue;
                    }
                    var error = Marshal.GetLastWin32Error();
                    if (error != ErrorNoMoreFiles) {
                        throw new Win32Exception(
                            error,
                            "Advancing the throughput process " +
                            "snapshot failed");
                    }
                    AssertBeforeDeadline(
                        timer, timeoutMilliseconds);
                    return parents;
                }
            } finally {
                CloseHandle(snapshot);
            }
        }

        private static void AssertBeforeDeadline(
            Stopwatch timer,
            int timeoutMilliseconds) {
            if (timer.ElapsedMilliseconds >= timeoutMilliseconds) {
                throw new TimeoutException(
                    "Throughput process snapshot exceeded its deadline");
            }
        }

        public static bool FixedEquals(byte[] left, byte[] right) {
            if (left == null || right == null ||
                left.Length != right.Length) {
                return false;
            }
            var difference = 0;
            for (var index = 0; index < left.Length; index++) {
                difference |= left[index] ^ right[index];
            }
            return difference == 0;
        }
    }
}
'@
}

function New-KettlePerfThroughputChannelRandomHex {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Returns random text and does not change external state.'
    )]
    param(
        [ValidateRange(16, 64)]
        [int]$ByteCount = 32
    )

    $bytes = [byte[]]::new($ByteCount)
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $random.GetBytes($bytes)
        return -join @(
            $bytes | ForEach-Object { $_.ToString('x2') }
        )
    } finally {
        [Array]::Clear($bytes, 0, $bytes.Length)
        $random.Dispose()
    }
}

function Assert-KettlePerfThroughputChannelName {
    param(
        [Parameter(Mandatory)]
        [string]$PipeName
    )

    if (
        $PipeName -cnotmatch
            '^kettle-perf-(throughput|vtebench)-[0-9a-f]{48}$' -or
        $PipeName.Length -gt 96
    ) {
        throw 'Throughput channel pipe name is invalid'
    }
    return $PipeName
}

function Assert-KettlePerfThroughputChannelNonce {
    param(
        [Parameter(Mandatory)]
        [string]$Nonce
    )

    if ($Nonce -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Throughput channel nonce is invalid'
    }
    return $Nonce
}

function ConvertFrom-KettlePerfThroughputChannelHex {
    param(
        [Parameter(Mandatory)]
        [string]$Hex
    )

    if (($Hex.Length % 2) -ne 0 -or $Hex -cnotmatch '^[0-9a-f]+$') {
        throw 'Throughput channel hexadecimal value is invalid'
    }
    $bytes = [byte[]]::new([int]($Hex.Length / 2))
    for ($index = 0; $index -lt $bytes.Length; $index++) {
        $bytes[$index] = [Convert]::ToByte(
            $Hex.Substring($index * 2, 2),
            16
        )
    }
    return $bytes
}

function New-KettlePerfThroughputChannelServer {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates one private local IPC endpoint.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$PipeName
    )

    if (-not $script:KettlePerfThroughputChannelIsWindows) {
        throw 'The throughput channel requires Windows named pipes'
    }
    $PipeName = Assert-KettlePerfThroughputChannelName $PipeName

    # FILE_FLAG_FIRST_PIPE_INSTANCE prevents a same-name server from winning a
    # pre-creation race. CurrentUserOnly exists on modern .NET; Windows
    # PowerShell 5.1 receives an equivalent protected ACL explicitly.
    $firstPipeInstance = 0x00080000
    $optionBits = [int][IO.Pipes.PipeOptions]::Asynchronous
    $optionNames = [enum]::GetNames([IO.Pipes.PipeOptions])
    $supportsCurrentUserOnly = (
        $optionNames -contains 'CurrentUserOnly'
    )
    if ($supportsCurrentUserOnly) {
        $optionBits = (
            $optionBits -bor
            $firstPipeInstance -bor
            0x20000000
        )
    }
    $options = [IO.Pipes.PipeOptions]$optionBits

    if ($supportsCurrentUserOnly) {
        return [IO.Pipes.NamedPipeServerStream]::new(
            $PipeName,
            [IO.Pipes.PipeDirection]::InOut,
            1,
            [IO.Pipes.PipeTransmissionMode]::Message,
            $options,
            4096,
            4096
        )
    }

    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    if ($null -eq $identity -or $null -eq $identity.User) {
        throw 'Could not identify the throughput channel owner'
    }
    $security = [IO.Pipes.PipeSecurity]::new()
    $security.SetAccessRuleProtection($true, $false)
    $security.SetOwner($identity.User)
    $security.AddAccessRule(
        [IO.Pipes.PipeAccessRule]::new(
            $identity.User,
            [IO.Pipes.PipeAccessRights]::FullControl,
            [Security.AccessControl.AccessControlType]::Allow
        )
    )
    $parameterTypes = [type[]]@(
        [string],
        [IO.Pipes.PipeDirection],
        [int],
        [IO.Pipes.PipeTransmissionMode],
        [IO.Pipes.PipeOptions],
        [int],
        [int],
        [IO.Pipes.PipeSecurity]
    )
    $constructor = [IO.Pipes.NamedPipeServerStream].GetConstructor(
        $parameterTypes
    )
    if ($null -eq $constructor) {
        throw 'This PowerShell cannot create an owner-only named pipe'
    }
    return $constructor.Invoke(
        [object[]]@(
            $PipeName,
            [IO.Pipes.PipeDirection]::InOut,
            1,
            [IO.Pipes.PipeTransmissionMode]::Message,
            $options,
            4096,
            4096,
            $security
        )
    )
}

function New-KettlePerfThroughputChannelDescriptor {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates one private local IPC endpoint.'
    )]
    param(
        [ValidateSet('throughput', 'vtebench')]
        [string]$Purpose = 'throughput',
        [ValidateRange(1024, 67108864)]
        [int]$MaximumBytes =
            $script:KettlePerfThroughputChannelMaximumBytes
    )

    Initialize-KettlePerfThroughputChannelNative
    $nonce = New-KettlePerfThroughputChannelRandomHex -ByteCount 32
    for ($attempt = 0; $attempt -lt 16; $attempt++) {
        $name = (
            "kettle-perf-$Purpose-" +
            (New-KettlePerfThroughputChannelRandomHex -ByteCount 24)
        )
        try {
            $server = New-KettlePerfThroughputChannelServer `
                -PipeName $name
            return [pscustomobject]@{
                Schema = $script:KettlePerfThroughputChannelSchema
                PipeName = $name
                Nonce = $nonce
                Purpose = $Purpose
                MaximumBytes = $MaximumBytes
                SecurityMode = if (
                    [enum]::GetNames(
                        [IO.Pipes.PipeOptions]
                    ) -contains 'CurrentUserOnly'
                ) {
                    'current-user-only-first-instance'
                } else {
                    'explicit-owner-only-acl'
                }
                Server = $server
                ReceiveStarted = $false
            }
        } catch [IO.IOException] {
            if ($attempt -eq 15) {
                throw
            }
        }
    }
    throw 'Could not create an unpredictable throughput channel'
}

function Close-KettlePerfThroughputChannel {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Closes one local IPC endpoint.'
    )]
    param(
        $Descriptor
    )

    if ($null -eq $Descriptor) {
        return
    }
    if ($null -ne $Descriptor.Server) {
        $Descriptor.Server.Dispose()
        $Descriptor.Server = $null
    }
    $Descriptor.Nonce = ''
}

function Wait-KettlePerfThroughputChannelTask {
    param(
        [Parameter(Mandatory)]
        [Threading.Tasks.Task]$Task,
        [Parameter(Mandatory)]
        [Diagnostics.Stopwatch]$Timer,
        [ValidateRange(1, 86400000)]
        [int]$TimeoutMs,
        [Parameter(Mandatory)]
        [string]$Operation
    )

    $remaining = [long]$TimeoutMs - $Timer.ElapsedMilliseconds
    if ($remaining -le 0) {
        throw [TimeoutException]::new(
            "Throughput channel timed out during $Operation"
        )
    }
    try {
        if (-not $Task.Wait([int]$remaining)) {
            throw [TimeoutException]::new(
                "Throughput channel timed out during $Operation"
            )
        }
    } catch [AggregateException] {
        $inner = $_.Exception.GetBaseException()
        throw [IO.IOException]::new(
            "Throughput channel failed during ${Operation}: " +
                $inner.Message,
            $inner
        )
    }
}

function Read-KettlePerfThroughputChannelExact {
    param(
        [Parameter(Mandatory)]
        [IO.Pipes.NamedPipeServerStream]$Stream,
        [Parameter(Mandatory)]
        [byte[]]$Buffer,
        [ValidateRange(0, 16777256)]
        [int]$Offset,
        [ValidateRange(1, 16777256)]
        [int]$Count,
        [Parameter(Mandatory)]
        [Diagnostics.Stopwatch]$Timer,
        [ValidateRange(1, 86400000)]
        [int]$TimeoutMs,
        [Parameter(Mandatory)]
        [string]$Operation
    )

    $readTotal = 0
    while ($readTotal -lt $Count) {
        $task = $Stream.ReadAsync(
            $Buffer,
            $Offset + $readTotal,
            $Count - $readTotal
        )
        Wait-KettlePerfThroughputChannelTask `
            -Task $task -Timer $Timer -TimeoutMs $TimeoutMs `
            -Operation $Operation
        $read = [int]$task.Result
        if ($read -le 0) {
            throw [IO.EndOfStreamException]::new(
                "Throughput channel ended during $Operation"
            )
        }
        $readTotal += $read
        if (
            $readTotal -lt $Count -and
            $Stream.IsMessageComplete
        ) {
            throw [IO.EndOfStreamException]::new(
                "Throughput channel message was truncated during $Operation"
            )
        }
    }
}

function Read-KettlePerfThroughputChannelClientExact {
    param(
        [Parameter(Mandatory)]
        [IO.Pipes.NamedPipeClientStream]$Stream,
        [Parameter(Mandatory)]
        [byte[]]$Buffer,
        [ValidateRange(1, 4096)]
        [int]$Count,
        [Parameter(Mandatory)]
        [Diagnostics.Stopwatch]$Timer,
        [ValidateRange(1, 60000)]
        [int]$TimeoutMs,
        [Parameter(Mandatory)]
        [string]$Operation
    )

    $readTotal = 0
    while ($readTotal -lt $Count) {
        $task = $Stream.ReadAsync(
            $Buffer,
            $readTotal,
            $Count - $readTotal
        )
        Wait-KettlePerfThroughputChannelTask `
            -Task $task -Timer $Timer -TimeoutMs $TimeoutMs `
            -Operation $Operation
        $read = [int]$task.Result
        if ($read -le 0) {
            throw [IO.EndOfStreamException]::new(
                "Throughput channel ended during $Operation"
            )
        }
        $readTotal += $read
        if (
            $readTotal -lt $Count -and
            $Stream.IsMessageComplete
        ) {
            throw [IO.EndOfStreamException]::new(
                "Throughput channel message was truncated during $Operation"
            )
        }
    }
}

function Get-KettlePerfThroughputChannelProcessSnapshot {
    param(
        [Parameter(Mandatory)]
        [Diagnostics.Stopwatch]$Timer,
        [ValidateRange(1, 60000)]
        [int]$TimeoutMs,
        [ValidateRange(1, 131072)]
        [int]$MaximumProcesses = (
            $script:KettlePerfThroughputChannelMaximumProcesses
        )
    )

    $remaining = [long]$TimeoutMs - $Timer.ElapsedMilliseconds
    if ($remaining -le 0) {
        throw [TimeoutException]::new(
            'Throughput channel timed out during process ancestry validation'
        )
    }
    try {
        $parents = (
            [KettlePerfThroughputChannel.NativeMethods]::GetProcessParents(
                [int]$remaining,
                $MaximumProcesses
            )
        )
    } catch [TimeoutException] {
        throw [TimeoutException]::new(
            'Throughput channel timed out during process ancestry validation',
            $_.Exception
        )
    }
    if ($Timer.ElapsedMilliseconds -ge $TimeoutMs) {
        throw [TimeoutException]::new(
            'Throughput channel timed out during process ancestry validation'
        )
    }
    return $parents
}

function Test-KettlePerfThroughputChannelProcessRelated {
    param(
        [Parameter(Mandatory)]
        [int]$CandidatePid,
        [Parameter(Mandatory)]
        [int]$RootPid,
        [Parameter(Mandatory)]
        [Collections.Generic.IDictionary[int, int]]$Parents
    )

    if ($CandidatePid -eq $RootPid) {
        return $true
    }
    $current = $CandidatePid
    $visited = [Collections.Generic.HashSet[int]]::new()
    while ($Parents.ContainsKey($current) -and $visited.Add($current)) {
        $current = [int]$Parents[$current]
        if ($current -eq $RootPid) {
            return $true
        }
    }
    return $false
}

function Assert-KettlePerfThroughputChannelClient {
    param(
        [Parameter(Mandatory)]
        [int]$ClientPid,
        [Parameter(Mandatory)]
        [int]$ExpectedWorkloadPid,
        [Parameter(Mandatory)]
        [int]$ExpectedTerminalPid,
        [Diagnostics.Stopwatch]$Timer,
        [ValidateRange(1, 60000)]
        [int]$TimeoutMs = 15000
    )

    if ($ClientPid -ne $ExpectedWorkloadPid) {
        throw (
            'Throughput channel client PID does not match the launched ' +
            'workload process'
        )
    }
    if ($null -eq $Timer) {
        $Timer = [Diagnostics.Stopwatch]::StartNew()
    }
    $parents = Get-KettlePerfThroughputChannelProcessSnapshot `
        -Timer $Timer -TimeoutMs $TimeoutMs
    if (
        -not $parents.ContainsKey($ClientPid) -or
        -not (
            Test-KettlePerfThroughputChannelProcessRelated `
                -CandidatePid $ClientPid `
                -RootPid $ExpectedTerminalPid `
                -Parents $parents
        )
    ) {
        throw (
            'Throughput channel client is outside the launched terminal ' +
            'process ancestry'
        )
    }
}

function New-KettlePerfThroughputChannelFrame {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Builds an in-memory byte array only.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Nonce,
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [byte[]]$JsonBytes,
        [uint32]$DeclaredLength = [uint32]::MaxValue
    )

    $Nonce = Assert-KettlePerfThroughputChannelNonce $Nonce
    if ($DeclaredLength -eq [uint32]::MaxValue) {
        $DeclaredLength = [uint32]$JsonBytes.Length
    }
    $nonceBytes = ConvertFrom-KettlePerfThroughputChannelHex $Nonce
    $frame = [byte[]]::new(
        $script:KettlePerfThroughputChannelHeaderBytes +
        $JsonBytes.Length
    )
    try {
        $frame[0] = [byte][char]'K'
        $frame[1] = [byte][char]'T'
        $frame[2] = [byte][char]'C'
        $frame[3] = [byte][char]'1'
        [Array]::Copy($nonceBytes, 0, $frame, 4, 32)
        for ($offset = 0; $offset -lt 4; $offset++) {
            $frame[36 + $offset] = [byte](
                ($DeclaredLength -shr ($offset * 8)) -band 0xff
            )
        }
        if ($JsonBytes.Length -gt 0) {
            [Array]::Copy(
                $JsonBytes,
                0,
                $frame,
                $script:KettlePerfThroughputChannelHeaderBytes,
                $JsonBytes.Length
            )
        }
        return $frame
    } finally {
        [Array]::Clear($nonceBytes, 0, $nonceBytes.Length)
    }
}

function Send-KettlePerfThroughputChannelFrame {
    param(
        [Parameter(Mandatory)]
        [string]$PipeName,
        [Parameter(Mandatory)]
        [byte[]]$Frame,
        [ValidateRange(1, 60000)]
        [int]$ConnectTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$WriteTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$AckTimeoutMs = 15000
    )

    $PipeName = Assert-KettlePerfThroughputChannelName $PipeName
    if (
        $Frame.Length -lt $script:KettlePerfThroughputChannelHeaderBytes -or
        $Frame.Length -gt
            ($script:KettlePerfAuthenticatedChannelAbsoluteMaximumBytes + 64)
    ) {
        throw 'Throughput channel frame is outside its byte bound'
    }

    $client = [IO.Pipes.NamedPipeClientStream]::new(
        '.',
        $PipeName,
        [IO.Pipes.PipeDirection]::InOut,
        [IO.Pipes.PipeOptions]::Asynchronous
    )
    try {
        $client.Connect($ConnectTimeoutMs)
        $client.ReadMode = [IO.Pipes.PipeTransmissionMode]::Message
        $writeTimer = [Diagnostics.Stopwatch]::StartNew()
        $writeTask = $client.WriteAsync($Frame, 0, $Frame.Length)
        Wait-KettlePerfThroughputChannelTask `
            -Task $writeTask -Timer $writeTimer `
            -TimeoutMs $WriteTimeoutMs -Operation 'message write'

        $ack = [byte[]]::new(
            $script:KettlePerfThroughputChannelAckFrame.Length
        )
        $ackTimer = [Diagnostics.Stopwatch]::StartNew()
        Read-KettlePerfThroughputChannelClientExact `
            -Stream $client -Buffer $ack -Count $ack.Length `
            -Timer $ackTimer -TimeoutMs $AckTimeoutMs `
            -Operation 'server acknowledgement'
        $ackDifference = 0
        for ($index = 0; $index -lt $ack.Length; $index++) {
            $ackDifference = $ackDifference -bor (
                $ack[$index] -bxor
                $script:KettlePerfThroughputChannelAckFrame[$index]
            )
        }
        if ($ackDifference -ne 0) {
            throw 'Throughput channel returned an invalid acknowledgement'
        }
    } finally {
        $client.Dispose()
    }
}

function Send-KettlePerfThroughputChannelJson {
    param(
        [Parameter(Mandatory)]
        [string]$PipeName,
        [Parameter(Mandatory)]
        [string]$Nonce,
        [Parameter(Mandatory)]
        $InputObject,
        [ValidateRange(1, 100)]
        [int]$Depth = 10,
        [ValidateRange(1024, 16777216)]
        [int]$MaximumBytes =
            $script:KettlePerfThroughputChannelMaximumBytes,
        [ValidateRange(1, 60000)]
        [int]$ConnectTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$WriteTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$AckTimeoutMs = 15000
    )

    $PipeName = Assert-KettlePerfThroughputChannelName $PipeName
    $Nonce = Assert-KettlePerfThroughputChannelNonce $Nonce
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    $jsonBytes = $null
    $frame = $null
    try {
        $json = $InputObject | ConvertTo-Json -Depth $Depth -Compress
        $jsonBytes = $utf8.GetBytes($json)
        if (
            $jsonBytes.Length -eq 0 -or
            $jsonBytes.Length -gt $MaximumBytes
        ) {
            throw 'Throughput channel JSON is outside its byte bound'
        }
        $frame = New-KettlePerfThroughputChannelFrame `
            -Nonce $Nonce -JsonBytes $jsonBytes
        Send-KettlePerfThroughputChannelFrame `
            -PipeName $PipeName -Frame $frame `
            -ConnectTimeoutMs $ConnectTimeoutMs `
            -WriteTimeoutMs $WriteTimeoutMs `
            -AckTimeoutMs $AckTimeoutMs
    } finally {
        if ($null -ne $frame) {
            [Array]::Clear($frame, 0, $frame.Length)
        }
        if ($null -ne $jsonBytes) {
            [Array]::Clear($jsonBytes, 0, $jsonBytes.Length)
        }
    }
}

function Receive-KettlePerfThroughputChannelJson {
    param(
        [Parameter(Mandatory)]
        $Descriptor,
        [Parameter(Mandatory)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$ExpectedWorkloadPid,
        [Parameter(Mandatory)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$ExpectedTerminalPid,
        [ValidateRange(1, 86400000)]
        [int]$ConnectTimeoutMs,
        [ValidateRange(1, 60000)]
        [int]$ReadTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$AckTimeoutMs = 15000
    )

    . (
        Join-Path $script:KettlePerfThroughputChannelScriptRoot `
            'process-capture.ps1'
    )
    Initialize-KettlePerfThroughputChannelNative
    if (
        $null -eq $Descriptor -or
        [string]$Descriptor.Schema -cne
            $script:KettlePerfThroughputChannelSchema -or
        $null -eq $Descriptor.Server -or
        [bool]$Descriptor.ReceiveStarted
    ) {
        throw 'Throughput channel descriptor is invalid or already consumed'
    }
    $Descriptor.ReceiveStarted = $true
    $expectedNonce = ConvertFrom-KettlePerfThroughputChannelHex (
        Assert-KettlePerfThroughputChannelNonce (
            [string]$Descriptor.Nonce
        )
    )
    $header = [byte[]]::new(
        $script:KettlePerfThroughputChannelHeaderBytes
    )
    $jsonBytes = $null
    try {
        $connectTimer = [Diagnostics.Stopwatch]::StartNew()
        $connectTask = $Descriptor.Server.WaitForConnectionAsync()
        Wait-KettlePerfThroughputChannelTask `
            -Task $connectTask -Timer $connectTimer `
            -TimeoutMs $ConnectTimeoutMs -Operation 'client connection'

        $readTimer = [Diagnostics.Stopwatch]::StartNew()
        $clientPid = (
            [KettlePerfThroughputChannel.NativeMethods]::
                GetClientProcessId($Descriptor.Server)
        )
        Assert-KettlePerfThroughputChannelClient `
            -ClientPid $clientPid `
            -ExpectedWorkloadPid $ExpectedWorkloadPid `
            -ExpectedTerminalPid $ExpectedTerminalPid `
            -Timer $readTimer -TimeoutMs $ReadTimeoutMs

        Read-KettlePerfThroughputChannelExact `
            -Stream $Descriptor.Server -Buffer $header `
            -Offset 0 -Count $header.Length `
            -Timer $readTimer -TimeoutMs $ReadTimeoutMs `
            -Operation 'message header'
        if (
            $header[0] -ne [byte][char]'K' -or
            $header[1] -ne [byte][char]'T' -or
            $header[2] -ne [byte][char]'C' -or
            $header[3] -ne [byte][char]'1'
        ) {
            throw 'Throughput channel message has an invalid protocol marker'
        }
        $actualNonce = [byte[]]::new(32)
        [Array]::Copy($header, 4, $actualNonce, 0, 32)
        try {
            if (-not (
                [KettlePerfThroughputChannel.NativeMethods]::FixedEquals(
                    $actualNonce,
                    $expectedNonce
                )
            )) {
                throw 'Throughput channel message nonce does not match'
            }
        } finally {
            [Array]::Clear($actualNonce, 0, $actualNonce.Length)
        }

        [uint32]$length = 0
        for ($offset = 0; $offset -lt 4; $offset++) {
            $length = $length -bor (
                [uint32]$header[36 + $offset] -shl ($offset * 8)
            )
        }
        if (
            $length -eq 0 -or
            $length -gt [uint32]$Descriptor.MaximumBytes
        ) {
            throw 'Throughput channel JSON length is outside its byte bound'
        }
        if ($Descriptor.Server.IsMessageComplete) {
            throw 'Throughput channel message ended before its JSON body'
        }

        $jsonBytes = [byte[]]::new([int]$length)
        Read-KettlePerfThroughputChannelExact `
            -Stream $Descriptor.Server -Buffer $jsonBytes `
            -Offset 0 -Count $jsonBytes.Length `
            -Timer $readTimer -TimeoutMs $ReadTimeoutMs `
            -Operation 'JSON body'
        if (-not $Descriptor.Server.IsMessageComplete) {
            throw 'Throughput channel message contains trailing bytes'
        }
        if (
            $jsonBytes.Length -ge 3 -and
            $jsonBytes[0] -eq 0xef -and
            $jsonBytes[1] -eq 0xbb -and
            $jsonBytes[2] -eq 0xbf
        ) {
            throw 'Throughput channel JSON must not contain a UTF-8 BOM'
        }
        $utf8 = [Text.UTF8Encoding]::new($false, $true)
        try {
            $json = $utf8.GetString($jsonBytes)
        } catch [Text.DecoderFallbackException] {
            throw 'Throughput channel JSON is not strict UTF-8'
        }
        try {
            $value = ConvertFrom-KettlePerfBoundedJson `
                -Json $json `
                -MaximumDepth (
                    $script:KettlePerfThroughputChannelMaximumJsonDepth
                ) `
                -MaximumTokens (
                    $script:KettlePerfThroughputChannelMaximumJsonNodes
                )
        } catch {
            throw (
                'Throughput channel payload is not valid JSON: ' +
                $_.Exception.Message
            )
        }
        if (
            $null -eq $value -or
            $value -is [string] -or
            $value -is [ValueType] -or
            $value -is [Collections.IList]
        ) {
            throw 'Throughput channel JSON root must be an object'
        }

        $ack = [byte[]]$script:KettlePerfThroughputChannelAckFrame.Clone()
        $ackTimer = [Diagnostics.Stopwatch]::StartNew()
        $ackTask = $Descriptor.Server.WriteAsync(
            $ack,
            0,
            $ack.Length
        )
        Wait-KettlePerfThroughputChannelTask `
            -Task $ackTask -Timer $ackTimer `
            -TimeoutMs $AckTimeoutMs -Operation 'server acknowledgement'

        return [pscustomobject]@{
            ClientPid = $clientPid
            Bytes = [int]$length
            Value = $value
        }
    } finally {
        [Array]::Clear($expectedNonce, 0, $expectedNonce.Length)
        [Array]::Clear($header, 0, $header.Length)
        if ($null -ne $jsonBytes) {
            [Array]::Clear($jsonBytes, 0, $jsonBytes.Length)
        }
    }
}
