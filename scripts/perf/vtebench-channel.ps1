# Authenticated vtebench result transport and bounded WSL stderr framing.
. "$PSScriptRoot\throughput-channel.ps1"
. "$PSScriptRoot\vtebench-dat.ps1"

$script:KettlePerfVtebenchChannelHeaderBytes = 48
$script:KettlePerfVtebenchPrivateHeaderBytes = 16
$script:KettlePerfVtebenchMaximumDatBytes = 1MB
$script:KettlePerfVtebenchChannelSchema = 'kettle-vtebench-channel-v1'

function Read-KettlePerfVtebenchStreamExact {
    param(
        [Parameter(Mandatory)]
        [IO.Stream]$Stream,
        [Parameter(Mandatory)]
        [byte[]]$Buffer,
        [ValidateRange(0, 67108928)]
        [int]$Offset,
        [ValidateRange(1, 67108928)]
        [int]$Count,
        [Parameter(Mandatory)]
        [Diagnostics.Stopwatch]$Timer,
        [ValidateRange(1, 86400000)]
        [int]$TimeoutMs,
        [Parameter(Mandatory)]
        [string]$Operation
    )

    $total = 0
    while ($total -lt $Count) {
        $task = $Stream.ReadAsync(
            $Buffer,
            $Offset + $total,
            $Count - $total
        )
        Wait-KettlePerfThroughputChannelTask `
            -Task $task -Timer $Timer -TimeoutMs $TimeoutMs `
            -Operation $Operation
        $read = [int]$task.Result
        if ($read -le 0) {
            throw [IO.EndOfStreamException]::new(
                "vtebench stream ended during $Operation"
            )
        }
        $total += $read
    }
}

function Get-KettlePerfVtebenchUInt32 {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes,
        [ValidateRange(0, 67108924)]
        [int]$Offset
    )

    [uint32]$value = 0
    for ($index = 0; $index -lt 4; $index++) {
        $value = $value -bor (
            [uint32]$Bytes[$Offset + $index] -shl ($index * 8)
        )
    }
    return $value
}

function Get-KettlePerfVtebenchUInt64 {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes,
        [ValidateRange(0, 67108920)]
        [int]$Offset
    )

    [uint64]$value = 0
    for ($index = 0; $index -lt 8; $index++) {
        $value = $value -bor (
            [uint64]$Bytes[$Offset + $index] -shl ($index * 8)
        )
    }
    return $value
}

function Set-KettlePerfVtebenchUInt32 {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes,
        [ValidateRange(0, 67108924)]
        [int]$Offset,
        [Parameter(Mandatory)]
        [uint32]$Value
    )

    for ($index = 0; $index -lt 4; $index++) {
        $Bytes[$Offset + $index] = [byte](
            ($Value -shr ($index * 8)) -band 0xff
        )
    }
}

function Set-KettlePerfVtebenchUInt64 {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes,
        [ValidateRange(0, 67108920)]
        [int]$Offset,
        [Parameter(Mandatory)]
        [uint64]$Value
    )

    for ($index = 0; $index -lt 8; $index++) {
        $Bytes[$Offset + $index] = [byte](
            ($Value -shr ($index * 8)) -band 0xff
        )
    }
}

function Read-KettlePerfVtebenchPrivateFrame {
    param(
        [Parameter(Mandatory)]
        [IO.Stream]$Stream,
        [ValidateRange(1024, 1048576)]
        [int]$MaximumDatBytes =
            $script:KettlePerfVtebenchMaximumDatBytes,
        [ValidateRange(1, 86400000)]
        [int]$TimeoutMs
    )

    $timer = [Diagnostics.Stopwatch]::StartNew()
    $header = [byte[]]::new(
        $script:KettlePerfVtebenchPrivateHeaderBytes
    )
    $datBytes = $null
    try {
        Read-KettlePerfVtebenchStreamExact `
            -Stream $Stream -Buffer $header -Offset 0 `
            -Count $header.Length -Timer $timer `
            -TimeoutMs $TimeoutMs -Operation 'private frame header'
        if (
            $header[0] -ne [byte][char]'K' -or
            $header[1] -ne [byte][char]'V' -or
            $header[2] -ne [byte][char]'D' -or
            $header[3] -ne [byte][char]'1'
        ) {
            throw 'vtebench private stream has an invalid protocol marker'
        }
        $status = Get-KettlePerfVtebenchUInt32 -Bytes $header -Offset 4
        $length = Get-KettlePerfVtebenchUInt64 -Bytes $header -Offset 8
        if ($status -gt 255) {
            throw 'vtebench private stream has an invalid exit status'
        }
        if (
            $length -eq 0 -or
            $length -gt [uint64]$MaximumDatBytes
        ) {
            throw 'vtebench private DAT length is outside its byte bound'
        }
        $datBytes = [byte[]]::new([int]$length)
        Read-KettlePerfVtebenchStreamExact `
            -Stream $Stream -Buffer $datBytes -Offset 0 `
            -Count $datBytes.Length -Timer $timer `
            -TimeoutMs $TimeoutMs -Operation 'private DAT body'
        $extra = [byte[]]::new(1)
        $extraTask = $Stream.ReadAsync($extra, 0, 1)
        Wait-KettlePerfThroughputChannelTask `
            -Task $extraTask -Timer $timer -TimeoutMs $TimeoutMs `
            -Operation 'private frame termination'
        if ([int]$extraTask.Result -ne 0) {
            throw 'vtebench private stream contains trailing bytes'
        }
        $result = [pscustomobject]@{
            Status = [int]$status
            DatBytes = $datBytes
        }
        $datBytes = $null
        return $result
    } finally {
        [Array]::Clear($header, 0, $header.Length)
        if ($null -ne $datBytes) {
            [Array]::Clear($datBytes, 0, $datBytes.Length)
        }
    }
}

function New-KettlePerfVtebenchChannelDescriptor {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates one owner-only local IPC endpoint.'
    )]
    param(
        [ValidateRange(1024, 1048576)]
        [int]$MaximumDatBytes =
            $script:KettlePerfVtebenchMaximumDatBytes
    )

    return New-KettlePerfThroughputChannelDescriptor `
        -Purpose vtebench -MaximumBytes $MaximumDatBytes
}

function New-KettlePerfVtebenchChannelFrame {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Builds an in-memory byte array only.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Nonce,
        [ValidateRange(0, 255)]
        [int]$Status,
        [Parameter(Mandatory)]
        [byte[]]$DatBytes,
        [uint64]$DeclaredLength = [uint64]::MaxValue
    )

    $Nonce = Assert-KettlePerfThroughputChannelNonce $Nonce
    if ($DeclaredLength -eq [uint64]::MaxValue) {
        $DeclaredLength = [uint64]$DatBytes.Length
    }
    $nonceBytes = ConvertFrom-KettlePerfThroughputChannelHex $Nonce
    $frame = [byte[]]::new(
        $script:KettlePerfVtebenchChannelHeaderBytes +
        $DatBytes.Length
    )
    try {
        $frame[0] = [byte][char]'K'
        $frame[1] = [byte][char]'V'
        $frame[2] = [byte][char]'C'
        $frame[3] = [byte][char]'1'
        [Array]::Copy($nonceBytes, 0, $frame, 4, 32)
        Set-KettlePerfVtebenchUInt32 `
            -Bytes $frame -Offset 36 -Value ([uint32]$Status)
        Set-KettlePerfVtebenchUInt64 `
            -Bytes $frame -Offset 40 -Value $DeclaredLength
        if ($DatBytes.Length -gt 0) {
            [Array]::Copy(
                $DatBytes,
                0,
                $frame,
                $script:KettlePerfVtebenchChannelHeaderBytes,
                $DatBytes.Length
            )
        }
        return $frame
    } finally {
        [Array]::Clear($nonceBytes, 0, $nonceBytes.Length)
    }
}

function Send-KettlePerfVtebenchChannelResult {
    param(
        [Parameter(Mandatory)]
        [string]$PipeName,
        [Parameter(Mandatory)]
        [string]$Nonce,
        [ValidateRange(0, 255)]
        [int]$Status,
        [Parameter(Mandatory)]
        [byte[]]$DatBytes,
        [ValidateRange(1, 60000)]
        [int]$ConnectTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$WriteTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$AckTimeoutMs = 15000
    )

    if (
        $DatBytes.Length -eq 0 -or
        $DatBytes.Length -gt
            $script:KettlePerfVtebenchMaximumDatBytes
    ) {
        throw 'vtebench DAT is outside its channel byte bound'
    }
    $frame = New-KettlePerfVtebenchChannelFrame `
        -Nonce $Nonce -Status $Status -DatBytes $DatBytes
    try {
        Send-KettlePerfThroughputChannelFrame `
            -PipeName $PipeName -Frame $frame `
            -ConnectTimeoutMs $ConnectTimeoutMs `
            -WriteTimeoutMs $WriteTimeoutMs `
            -AckTimeoutMs $AckTimeoutMs
    } finally {
        [Array]::Clear($frame, 0, $frame.Length)
    }
}

function Receive-KettlePerfVtebenchChannelResult {
    param(
        [Parameter(Mandatory)]
        $Descriptor,
        [Parameter(Mandatory)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$ExpectedWorkloadPid,
        [Parameter(Mandatory)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$ExpectedTerminalPid,
        [ValidateRange(1, 10000)]
        [int]$ExpectedColumns,
        [ValidateRange(1, 86400000)]
        [int]$ConnectTimeoutMs,
        [ValidateRange(1, 60000)]
        [int]$ReadTimeoutMs = 15000,
        [ValidateRange(1, 60000)]
        [int]$AckTimeoutMs = 15000
    )

    Initialize-KettlePerfThroughputChannelNative
    if (
        $null -eq $Descriptor -or
        [string]$Descriptor.Schema -cne
            $script:KettlePerfThroughputChannelSchema -or
        [string]$Descriptor.Purpose -cne 'vtebench' -or
        $null -eq $Descriptor.Server -or
        [bool]$Descriptor.ReceiveStarted
    ) {
        throw 'vtebench channel descriptor is invalid or already consumed'
    }
    $Descriptor.ReceiveStarted = $true
    $expectedNonce = ConvertFrom-KettlePerfThroughputChannelHex (
        Assert-KettlePerfThroughputChannelNonce (
            [string]$Descriptor.Nonce
        )
    )
    $header = [byte[]]::new(
        $script:KettlePerfVtebenchChannelHeaderBytes
    )
    $datBytes = $null
    try {
        $connectTimer = [Diagnostics.Stopwatch]::StartNew()
        $connectTask = $Descriptor.Server.WaitForConnectionAsync()
        Wait-KettlePerfThroughputChannelTask `
            -Task $connectTask -Timer $connectTimer `
            -TimeoutMs $ConnectTimeoutMs -Operation 'vtebench connection'
        $clientPid = (
            [KettlePerfThroughputChannel.NativeMethods]::
                GetClientProcessId($Descriptor.Server)
        )
        Assert-KettlePerfThroughputChannelClient `
            -ClientPid $clientPid `
            -ExpectedWorkloadPid $ExpectedWorkloadPid `
            -ExpectedTerminalPid $ExpectedTerminalPid

        $readTimer = [Diagnostics.Stopwatch]::StartNew()
        Read-KettlePerfThroughputChannelExact `
            -Stream $Descriptor.Server -Buffer $header `
            -Offset 0 -Count $header.Length -Timer $readTimer `
            -TimeoutMs $ReadTimeoutMs -Operation 'vtebench header'
        if (
            $header[0] -ne [byte][char]'K' -or
            $header[1] -ne [byte][char]'V' -or
            $header[2] -ne [byte][char]'C' -or
            $header[3] -ne [byte][char]'1'
        ) {
            throw 'vtebench channel has an invalid protocol marker'
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
                throw 'vtebench channel nonce does not match'
            }
        } finally {
            [Array]::Clear($actualNonce, 0, $actualNonce.Length)
        }

        $status = Get-KettlePerfVtebenchUInt32 -Bytes $header -Offset 36
        $length = Get-KettlePerfVtebenchUInt64 -Bytes $header -Offset 40
        if ($status -gt 255) {
            throw 'vtebench channel has an invalid exit status'
        }
        if ($status -ne 0) {
            throw "vtebench workload exited with status $status"
        }
        if (
            $length -eq 0 -or
            $length -gt [uint64]$Descriptor.MaximumBytes
        ) {
            throw 'vtebench channel DAT length is outside its byte bound'
        }
        if ($Descriptor.Server.IsMessageComplete) {
            throw 'vtebench channel message ended before its DAT body'
        }
        $datBytes = [byte[]]::new([int]$length)
        Read-KettlePerfThroughputChannelExact `
            -Stream $Descriptor.Server -Buffer $datBytes `
            -Offset 0 -Count $datBytes.Length -Timer $readTimer `
            -TimeoutMs $ReadTimeoutMs -Operation 'vtebench DAT body'
        if (-not $Descriptor.Server.IsMessageComplete) {
            throw 'vtebench channel message contains trailing bytes'
        }
        $parsed = Read-KettlePerfVtebenchDatBytes `
            -Bytes $datBytes -ExpectedColumns $ExpectedColumns `
            -Source '<authenticated vtebench channel>'

        $ack = [byte[]]@(
            [byte]$script:KettlePerfThroughputChannelAck
        )
        $ackTimer = [Diagnostics.Stopwatch]::StartNew()
        $ackTask = $Descriptor.Server.WriteAsync($ack, 0, 1)
        Wait-KettlePerfThroughputChannelTask `
            -Task $ackTask -Timer $ackTimer `
            -TimeoutMs $AckTimeoutMs `
            -Operation 'vtebench acknowledgement'

        $result = [pscustomobject]@{
            ClientPid = $clientPid
            Status = [int]$status
            DatBytes = $datBytes
            Parsed = $parsed
        }
        $datBytes = $null
        return $result
    } finally {
        [Array]::Clear($expectedNonce, 0, $expectedNonce.Length)
        [Array]::Clear($header, 0, $header.Length)
        if ($null -ne $datBytes) {
            [Array]::Clear($datBytes, 0, $datBytes.Length)
        }
    }
}

function Get-KettlePerfVtebenchBytesSha256 {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes
    )

    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return (
            [BitConverter]::ToString(
                $sha.ComputeHash($Bytes)
            ).Replace('-', '').ToLowerInvariant()
        )
    } finally {
        $sha.Dispose()
    }
}

function Initialize-KettlePerfVtebenchPublicationNative {
    if ('KettlePerfVtebenchPublication.NativeMethods' -as [type]) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfVtebenchPublication {
    public static class NativeMethods {
        private const uint FileListDirectory = 0x00000001;
        private const uint FileWriteData = 0x00000002;
        private const uint FileTraverse = 0x00000020;
        private const uint FileReadAttributes = 0x00000080;
        private const uint Synchronize = 0x00100000;
        private const uint FileShareRead = 0x00000001;
        private const uint FileShareWrite = 0x00000002;
        private const uint OpenExisting = 3;
        private const uint FileCreate = 2;
        private const uint FileAttributeDirectory = 0x00000010;
        private const uint FileAttributeReparsePoint = 0x00000400;
        private const uint FileDirectoryFile = 0x00000001;
        private const uint FileSynchronousIoNonAlert = 0x00000020;
        private const uint FileNonDirectoryFile = 0x00000040;
        private const uint FileFlagBackupSemantics = 0x02000000;
        private const uint FileFlagOpenReparsePoint = 0x00200000;
        private const uint ObjCaseInsensitive = 0x00000040;
        private const uint ObjDontReparse = 0x00001000;

        [StructLayout(LayoutKind.Sequential)]
        private struct UnicodeString {
            internal ushort Length;
            internal ushort MaximumLength;
            internal IntPtr Buffer;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ObjectAttributes {
            internal int Length;
            internal IntPtr RootDirectory;
            internal IntPtr ObjectName;
            internal uint Attributes;
            internal IntPtr SecurityDescriptor;
            internal IntPtr SecurityQualityOfService;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct IoStatusBlock {
            internal IntPtr Status;
            internal UIntPtr Information;
        }

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

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern SafeFileHandle CreateFile(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetFileInformationByHandle(
            SafeFileHandle handle,
            out ByHandleFileInformation information);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern uint GetFinalPathNameByHandle(
            SafeFileHandle handle,
            StringBuilder path,
            uint pathLength,
            uint flags);

        [DllImport("ntdll.dll")]
        private static extern int NtCreateFile(
            out IntPtr fileHandle,
            uint desiredAccess,
            ref ObjectAttributes objectAttributes,
            out IoStatusBlock ioStatusBlock,
            IntPtr allocationSize,
            uint fileAttributes,
            uint shareAccess,
            uint createDisposition,
            uint createOptions,
            IntPtr eaBuffer,
            uint eaLength);

        [DllImport("ntdll.dll")]
        private static extern uint RtlNtStatusToDosError(int status);

        public static SafeFileHandle OpenRoot(string path) {
            var handle = CreateFile(
                path,
                FileListDirectory | FileTraverse |
                    FileReadAttributes | Synchronize,
                FileShareRead | FileShareWrite,
                IntPtr.Zero,
                OpenExisting,
                FileFlagBackupSemantics | FileFlagOpenReparsePoint,
                IntPtr.Zero);
            if (handle.IsInvalid) {
                var error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(
                    error, "Opening the vtebench results root failed");
            }
            try {
                AssertDirectory(handle, path);
                return handle;
            } catch {
                handle.Dispose();
                throw;
            }
        }

        public static SafeFileHandle CreateRelative(
            SafeFileHandle root,
            string leaf) {
            if (string.IsNullOrEmpty(leaf) ||
                Path.GetFileName(leaf) != leaf) {
                throw new ArgumentException(
                    "The vtebench DAT leaf name is invalid", "leaf");
            }
            var nameBytes = Encoding.Unicode.GetBytes(leaf);
            if (nameBytes.Length > ushort.MaxValue - 2) {
                throw new ArgumentOutOfRangeException("leaf");
            }
            var nameBuffer = IntPtr.Zero;
            var unicodeBuffer = IntPtr.Zero;
            try {
                nameBuffer = Marshal.StringToHGlobalUni(leaf);
                var unicode = new UnicodeString();
                unicode.Length = checked((ushort)nameBytes.Length);
                unicode.MaximumLength =
                    checked((ushort)(nameBytes.Length + 2));
                unicode.Buffer = nameBuffer;
                unicodeBuffer = Marshal.AllocHGlobal(
                    Marshal.SizeOf(typeof(UnicodeString)));
                Marshal.StructureToPtr(unicode, unicodeBuffer, false);

                var attributes = new ObjectAttributes();
                attributes.Length =
                    Marshal.SizeOf(typeof(ObjectAttributes));
                attributes.RootDirectory = root.DangerousGetHandle();
                attributes.ObjectName = unicodeBuffer;
                attributes.Attributes =
                    ObjCaseInsensitive | ObjDontReparse;

                IntPtr rawHandle;
                IoStatusBlock statusBlock;
                var status = NtCreateFile(
                    out rawHandle,
                    FileWriteData | FileReadAttributes | Synchronize,
                    ref attributes,
                    out statusBlock,
                    IntPtr.Zero,
                    0,
                    FileShareRead,
                    FileCreate,
                    FileNonDirectoryFile |
                        FileSynchronousIoNonAlert |
                        FileFlagOpenReparsePoint,
                    IntPtr.Zero,
                    0);
                if (status < 0) {
                    throw new Win32Exception(
                        unchecked((int)RtlNtStatusToDosError(status)),
                        "Creating the vtebench DAT relative to its root failed");
                }
                var handle = new SafeFileHandle(rawHandle, true);
                try {
                    AssertRegularFile(handle);
                    return handle;
                } catch {
                    handle.Dispose();
                    throw;
                }
            } finally {
                if (unicodeBuffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(unicodeBuffer);
                }
                if (nameBuffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(nameBuffer);
                }
            }
        }

        public static void AssertDirectory(
            SafeFileHandle handle,
            string expectedPath) {
            var information = GetInformation(handle);
            if ((information.FileAttributes & FileAttributeDirectory) == 0 ||
                (information.FileAttributes &
                    FileAttributeReparsePoint) != 0) {
                throw new InvalidOperationException(
                    "The vtebench results root is not an ordinary directory");
            }
            AssertFinalPath(handle, expectedPath);
        }

        public static void AssertRegularFile(
            SafeFileHandle handle,
            string expectedPath) {
            AssertRegularFile(handle);
            AssertFinalPath(handle, expectedPath);
        }

        private static void AssertRegularFile(SafeFileHandle handle) {
            var information = GetInformation(handle);
            if ((information.FileAttributes & FileAttributeDirectory) != 0 ||
                (information.FileAttributes &
                    FileAttributeReparsePoint) != 0) {
                throw new InvalidOperationException(
                    "The published vtebench DAT is not an ordinary file");
            }
        }

        private static ByHandleFileInformation GetInformation(
            SafeFileHandle handle) {
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Inspecting a vtebench publication handle failed");
            }
            return information;
        }

        private static void AssertFinalPath(
            SafeFileHandle handle,
            string expectedPath) {
            var finalPath = ConvertFinalPath(GetFinalPath(handle));
            if (!string.Equals(
                    Path.GetFullPath(expectedPath).TrimEnd('\\', '/'),
                    Path.GetFullPath(finalPath).TrimEnd('\\', '/'),
                    StringComparison.OrdinalIgnoreCase)) {
                throw new InvalidOperationException(
                    "A vtebench publication handle moved unexpectedly");
            }
        }

        private static string GetFinalPath(SafeFileHandle handle) {
            var capacity = 512;
            while (capacity <= 32768) {
                var result = new StringBuilder(capacity);
                var length = GetFinalPathNameByHandle(
                    handle, result, (uint)result.Capacity, 0);
                if (length == 0) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Resolving a vtebench publication handle failed");
                }
                if (length < result.Capacity) {
                    return result.ToString();
                }
                capacity = checked((int)length + 1);
            }
            throw new InvalidOperationException(
                "A vtebench publication path exceeds its bound");
        }

        private static string ConvertFinalPath(string path) {
            const string uncPrefix = @"\\?\UNC\";
            const string localPrefix = @"\\?\";
            if (path.StartsWith(
                    uncPrefix, StringComparison.OrdinalIgnoreCase)) {
                return @"\\" + path.Substring(uncPrefix.Length);
            }
            if (path.StartsWith(
                    localPrefix, StringComparison.OrdinalIgnoreCase)) {
                return path.Substring(localPrefix.Length);
            }
            return path;
        }
    }
}
'@
}

function Publish-KettlePerfVtebenchDat {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Atomically creates one authenticated raw DAT file.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$ResultsDirectory,
        [Parameter(Mandatory)]
        [byte[]]$Bytes
    )

    $root = [IO.Path]::GetFullPath($ResultsDirectory)
    $full = [IO.Path]::GetFullPath($Path)
    if (
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetDirectoryName($full),
            $root
        ) -or
        [IO.Path]::GetFileName($full) -notmatch
            '^vtebench-(kettle|wt|alacritty|wezterm|rio|tabby)\.dat$' -or
        $Bytes.Length -eq 0 -or
        $Bytes.Length -gt
            $script:KettlePerfVtebenchMaximumDatBytes
    ) {
        throw 'Authenticated vtebench DAT publication target is invalid'
    }
    Initialize-KettlePerfVtebenchPublicationNative
    $rootHandle = $null
    $fileHandle = $null
    $stream = $null
    try {
        $rootHandle = (
            [KettlePerfVtebenchPublication.NativeMethods]::OpenRoot($root)
        )
        $fileHandle = (
            [KettlePerfVtebenchPublication.NativeMethods]::CreateRelative(
                $rootHandle,
                [IO.Path]::GetFileName($full)
            )
        )
        $stream = [IO.FileStream]::new(
            $fileHandle,
            [IO.FileAccess]::Write
        )
        $fileHandle = $null
        $stream.Write($Bytes, 0, $Bytes.Length)
        $stream.Flush($true)
        if ($stream.Length -ne $Bytes.LongLength) {
            throw 'Authenticated vtebench DAT write was incomplete'
        }
        [KettlePerfVtebenchPublication.NativeMethods]::AssertDirectory(
            $rootHandle,
            $root
        )
        [KettlePerfVtebenchPublication.NativeMethods]::AssertRegularFile(
            $stream.SafeFileHandle,
            $full
        )
    } finally {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
        if ($null -ne $fileHandle) {
            $fileHandle.Dispose()
        }
        if ($null -ne $rootHandle) {
            $rootHandle.Dispose()
        }
    }
    return [pscustomobject]@{
        Path = $full
        Bytes = [long]$Bytes.Length
        Sha256 = Get-KettlePerfVtebenchBytesSha256 -Bytes $Bytes
    }
}
