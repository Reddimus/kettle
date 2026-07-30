# Produce a shareable JSON-only evidence bundle without local paths, command
# lines, machine identities, monitor serials, device instance identifiers, or
# artifact directories. The private raw result directory remains authoritative.
[Diagnostics.CodeAnalysis.SuppressMessageAttribute(
    'PSAvoidUsingWriteHost',
    '',
    Justification = 'The command reports its published bundle to an operator.'
)]
param(
    [Parameter(Mandatory)]
    [string]$ResultsDir,
    [Parameter(Mandatory)]
    [string]$OutputDir,
    [scriptblock]$BeforePublishSourceTestAction
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\evidence-snapshot.ps1"

$script:SanitizeMaximumFiles = 100
$script:SanitizeMaximumFileBytes = 64MB
$script:SanitizeMaximumTotalBytes = 128MB
$script:SanitizeMaximumNodes = 250000
$script:SanitizeMaximumDepth = 32
$script:SanitizeNodeCount = 0
$script:IsWindowsPlatform = $env:OS -eq 'Windows_NT'

function Test-KettlePerfSameSanitizePath {
    param(
        [Parameter(Mandatory)]
        [string]$Left,
        [Parameter(Mandatory)]
        [string]$Right
    )

    $comparison = if ($script:IsWindowsPlatform) {
        [StringComparison]::OrdinalIgnoreCase
    } else {
        [StringComparison]::Ordinal
    }
    return [IO.Path]::GetFullPath($Left).TrimEnd(
        [char[]]@('\', '/')
    ).Equals(
        [IO.Path]::GetFullPath($Right).TrimEnd([char[]]@('\', '/')),
        $comparison
    )
}

function Test-KettlePerfSanitizePathWithin {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Root
    )

    if (Test-KettlePerfSameSanitizePath -Left $Path -Right $Root) {
        return $true
    }
    $comparison = if ($script:IsWindowsPlatform) {
        [StringComparison]::OrdinalIgnoreCase
    } else {
        [StringComparison]::Ordinal
    }
    $prefix = [IO.Path]::GetFullPath($Root).TrimEnd(
        [char[]]@('\', '/')
    ) + [IO.Path]::DirectorySeparatorChar
    return [IO.Path]::GetFullPath($Path).StartsWith($prefix, $comparison)
}

function Assert-KettlePerfSafeWindowsPathSyntax {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [switch]$AllowMissingLeaf
    )

    if (-not $script:IsWindowsPlatform) {
        return
    }
    if (
        $Path.StartsWith('\\?\', [StringComparison]::Ordinal) -or
        $Path.StartsWith('\\.\', [StringComparison]::Ordinal)
    ) {
        throw "Device-namespace paths are not accepted: $Path"
    }
    $rawSegments = @($Path -split '[\\/]')
    foreach ($rawSegment in $rawSegments) {
        if (
            $rawSegment -and
            $rawSegment -notmatch '^[A-Za-z]:$' -and
            (
                $rawSegment -in @('.', '..') -or
                $rawSegment.EndsWith('.', [StringComparison]::Ordinal) -or
                $rawSegment.EndsWith(' ', [StringComparison]::Ordinal)
            )
        ) {
            throw "Unsafe or ambiguous Windows path segment: $rawSegment"
        }
    }
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($full)
    $tail = $full.Substring($root.Length)
    $segments = @($tail -split '[\\/]')
    $reserved = '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)'
    foreach ($segment in $segments) {
        if (-not $segment) {
            continue
        }
        if (
            $segment -in @('.', '..') -or
            $segment.EndsWith('.', [StringComparison]::Ordinal) -or
            $segment.EndsWith(' ', [StringComparison]::Ordinal) -or
            $segment.Contains(':') -or
            $segment -match $reserved -or
            $segment -match '~[0-9](?:\.|$)'
        ) {
            throw "Unsafe or ambiguous Windows path segment: $segment"
        }
    }
    if (-not $AllowMissingLeaf -and -not (Test-Path -LiteralPath $full)) {
        throw "Sanitizer path does not exist: $full"
    }
}

function Assert-KettlePerfNoReparseSanitizeAncestors {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function validates every existing path ancestor.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [switch]$LeafMayBeMissing
    )

    $current = [IO.Path]::GetFullPath($Path)
    if ($LeafMayBeMissing -and -not (Test-Path -LiteralPath $current)) {
        $current = [IO.Path]::GetDirectoryName($current)
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        if (
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "Sanitizer path traverses a reparse point: $current"
        }
        $parent = [IO.Path]::GetDirectoryName($current)
        if (
            [string]::IsNullOrEmpty($parent) -or
            (Test-KettlePerfSameSanitizePath -Left $current -Right $parent)
        ) {
            break
        }
        $current = $parent
    }
}

function Initialize-KettlePerfSanitizeNativeMethods {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The initialized type exposes multiple native methods.'
    )]
    param()

    if (
        -not $script:IsWindowsPlatform -or
        ('KettlePerfSanitize.NativeMethods' -as [type])
    ) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfSanitize {
    public static class NativeMethods {
        internal const uint DeleteAccess = 0x00010000;
        internal const uint FileListDirectory = 0x00000001;
        internal const uint FileReadData = 0x00000001;
        internal const uint FileWriteData = 0x00000002;
        internal const uint FileTraverse = 0x00000020;
        internal const uint FileReadAttributes = 0x00000080;
        internal const uint Synchronize = 0x00100000;
        internal const uint FileShareRead = 0x00000001;
        internal const uint FileShareWrite = 0x00000002;
        internal const uint FileShareDelete = 0x00000004;
        internal const uint OpenExisting = 3;
        internal const uint FileAttributeDirectory = 0x00000010;
        internal const uint FileAttributeReparsePoint = 0x00000400;
        internal const uint FileFlagBackupSemantics = 0x02000000;
        internal const uint FileFlagOpenReparsePoint = 0x00200000;
        internal const uint FileCreate = 2;
        internal const uint FileDirectoryFile = 0x00000001;
        internal const uint FileSynchronousIoNonAlert = 0x00000020;
        internal const uint FileNonDirectoryFile = 0x00000040;
        internal const uint ObjCaseInsensitive = 0x00000040;
        internal const uint ObjDontReparse = 0x00001000;
        internal const int FileRenameInfo = 3;
        internal const int FileDispositionInfo = 4;

        [StructLayout(LayoutKind.Sequential)]
        internal struct UnicodeString {
            internal ushort Length;
            internal ushort MaximumLength;
            internal IntPtr Buffer;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct ObjectAttributes {
            internal int Length;
            internal IntPtr RootDirectory;
            internal IntPtr ObjectName;
            internal uint Attributes;
            internal IntPtr SecurityDescriptor;
            internal IntPtr SecurityQualityOfService;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct IoStatusBlock {
            internal IntPtr Status;
            internal UIntPtr Information;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct FileTime {
            internal uint Low;
            internal uint High;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct ByHandleFileInformation {
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
        internal static extern SafeFileHandle CreateFile(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        internal static extern bool GetFileInformationByHandle(
            SafeFileHandle handle,
            out ByHandleFileInformation information);

        [DllImport("kernel32.dll")]
        private static extern IntPtr GetCurrentProcess();

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool DuplicateHandle(
            IntPtr sourceProcess,
            SafeFileHandle sourceHandle,
            IntPtr targetProcess,
            out SafeFileHandle targetHandle,
            uint desiredAccess,
            bool inheritHandle,
            uint options);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern uint GetFinalPathNameByHandle(
            SafeFileHandle handle,
            StringBuilder path,
            uint pathLength,
            uint flags);

        [DllImport("kernel32.dll", SetLastError = true)]
        internal static extern bool SetFileInformationByHandle(
            SafeFileHandle handle,
            int informationClass,
            IntPtr information,
            uint bufferSize);

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

        public static string GetFinalPath(SafeFileHandle handle) {
            var capacity = 512;
            while (capacity <= 32768) {
                var result = new StringBuilder(capacity);
                var length = GetFinalPathNameByHandle(
                    handle, result, (uint)result.Capacity, 0);
                if (length == 0) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "GetFinalPathNameByHandle failed");
                }
                if (length < result.Capacity) {
                    return result.ToString();
                }
                capacity = checked((int)length + 1);
            }
            throw new InvalidOperationException(
                "Final file path exceeds the sanitizer path bound");
        }

        internal static SafeFileHandle OpenDirectory(
            string path,
            uint desiredAccess,
            uint shareMode) {
            var handle = CreateFile(
                path,
                desiredAccess,
                shareMode,
                IntPtr.Zero,
                OpenExisting,
                FileFlagBackupSemantics | FileFlagOpenReparsePoint,
                IntPtr.Zero);
            if (handle.IsInvalid) {
                var error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(
                    error, "Opening sanitizer directory failed");
            }
            return handle;
        }

        internal static SafeFileHandle OpenRegularFile(
            string path,
            uint desiredAccess,
            uint shareMode) {
            var handle = CreateFile(
                path,
                desiredAccess,
                shareMode,
                IntPtr.Zero,
                OpenExisting,
                FileFlagOpenReparsePoint,
                IntPtr.Zero);
            if (handle.IsInvalid) {
                var error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(
                    error, "Opening sanitizer staged file failed");
            }
            return handle;
        }

        internal static ByHandleFileInformation GetInformation(
            SafeFileHandle handle) {
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "GetFileInformationByHandle failed");
            }
            return information;
        }

        internal static SafeFileHandle Duplicate(
            SafeFileHandle handle) {
            SafeFileHandle duplicate;
            var process = GetCurrentProcess();
            if (!DuplicateHandle(
                    process,
                    handle,
                    process,
                    out duplicate,
                    0,
                    false,
                    2)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Duplicating sanitizer file handle failed");
            }
            return duplicate;
        }

        internal static string GetIdentity(
            ByHandleFileInformation information) {
            return information.VolumeSerialNumber.ToString("x8") + ":" +
                information.FileIndexHigh.ToString("x8") +
                information.FileIndexLow.ToString("x8");
        }

        internal static bool SamePath(string left, string right) {
            return string.Equals(
                Path.GetFullPath(left).TrimEnd('\\', '/'),
                Path.GetFullPath(right).TrimEnd('\\', '/'),
                StringComparison.OrdinalIgnoreCase);
        }

        internal static string ConvertFinalPath(string path) {
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

        internal static void AssertDirectoryHandle(
            SafeFileHandle handle,
            string expectedPath,
            string expectedIdentity) {
            var information = GetInformation(handle);
            if ((information.FileAttributes & FileAttributeDirectory) == 0 ||
                (information.FileAttributes & FileAttributeReparsePoint) != 0) {
                throw new InvalidOperationException(
                    "Sanitizer stage root is not a plain directory");
            }
            var identity = GetIdentity(information);
            if (!string.Equals(
                    identity, expectedIdentity, StringComparison.Ordinal)) {
                throw new InvalidOperationException(
                    "Sanitizer stage directory identity changed");
            }
            var handlePath = ConvertFinalPath(GetFinalPath(handle));
            if (!SamePath(handlePath, expectedPath)) {
                throw new InvalidOperationException(
                    "Sanitizer stage directory moved unexpectedly");
            }

            using (var pathHandle = OpenDirectory(
                expectedPath,
                FileReadAttributes,
                FileShareRead | FileShareWrite | FileShareDelete)) {
                var pathInformation = GetInformation(pathHandle);
                if ((pathInformation.FileAttributes &
                        FileAttributeDirectory) == 0 ||
                    (pathInformation.FileAttributes &
                        FileAttributeReparsePoint) != 0 ||
                    !string.Equals(
                        GetIdentity(pathInformation),
                        expectedIdentity,
                        StringComparison.Ordinal)) {
                    throw new InvalidOperationException(
                        "Sanitizer stage path no longer names its held identity");
                }
            }
        }

        internal static SafeFileHandle CreateDirectoryRelative(
            SafeFileHandle parent,
            string leaf) {
            var nameBytes = Encoding.Unicode.GetBytes(leaf);
            if (nameBytes.Length == 0 ||
                nameBytes.Length > ushort.MaxValue - 2) {
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
                attributes.RootDirectory = parent.DangerousGetHandle();
                attributes.ObjectName = unicodeBuffer;
                attributes.Attributes =
                    ObjCaseInsensitive | ObjDontReparse;

                IntPtr rawHandle;
                IoStatusBlock statusBlock;
                var status = NtCreateFile(
                    out rawHandle,
                    DeleteAccess | FileListDirectory | FileTraverse |
                        FileReadAttributes | Synchronize,
                    ref attributes,
                    out statusBlock,
                    IntPtr.Zero,
                    0,
                    FileShareRead | FileShareWrite,
                    FileCreate,
                    FileDirectoryFile | FileSynchronousIoNonAlert,
                    IntPtr.Zero,
                    0);
                if (status < 0) {
                    throw new Win32Exception(
                        unchecked((int)RtlNtStatusToDosError(status)),
                        "Atomically creating sanitizer stage failed");
                }
                return new SafeFileHandle(rawHandle, true);
            } finally {
                if (unicodeBuffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(unicodeBuffer);
                }
                if (nameBuffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(nameBuffer);
                }
            }
        }

        internal static SafeFileHandle CreateRegularFileRelative(
            SafeFileHandle parent,
            string leaf) {
            var nameBytes = Encoding.Unicode.GetBytes(leaf);
            if (nameBytes.Length == 0 ||
                nameBytes.Length > ushort.MaxValue - 2) {
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
                attributes.RootDirectory = parent.DangerousGetHandle();
                attributes.ObjectName = unicodeBuffer;
                attributes.Attributes =
                    ObjCaseInsensitive | ObjDontReparse;

                IntPtr rawHandle;
                IoStatusBlock statusBlock;
                var status = NtCreateFile(
                    out rawHandle,
                    DeleteAccess | FileWriteData |
                        FileReadAttributes | Synchronize,
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
                        "Atomically creating sanitizer stage file failed");
                }
                return new SafeFileHandle(rawHandle, true);
            } finally {
                if (unicodeBuffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(unicodeBuffer);
                }
                if (nameBuffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(nameBuffer);
                }
            }
        }

        internal static void RenameToPath(
            SafeFileHandle handle,
            string destinationPath) {
            var name = Encoding.Unicode.GetBytes(destinationPath);
            var rootOffset = IntPtr.Size == 8 ? 8 : 4;
            var lengthOffset = rootOffset + IntPtr.Size;
            var nameOffset = lengthOffset + 4;
            var bufferSize = checked(nameOffset + name.Length + 2);
            var buffer = Marshal.AllocHGlobal(bufferSize);
            try {
                for (var offset = 0; offset < bufferSize; offset++) {
                    Marshal.WriteByte(buffer, offset, 0);
                }
                Marshal.WriteInt32(buffer, lengthOffset, name.Length);
                Marshal.Copy(name, 0, IntPtr.Add(buffer, nameOffset),
                    name.Length);
                if (!SetFileInformationByHandle(
                        handle,
                        FileRenameInfo,
                        buffer,
                        checked((uint)bufferSize))) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Publishing sanitizer stage by held handle failed");
                }
            } finally {
                Marshal.FreeHGlobal(buffer);
            }
        }

        internal static void DeleteByHandle(SafeFileHandle handle) {
            var buffer = Marshal.AllocHGlobal(1);
            try {
                Marshal.WriteByte(buffer, 0, 1);
                if (!SetFileInformationByHandle(
                        handle, FileDispositionInfo, buffer, 1)) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Deleting sanitizer object by held handle failed");
                }
            } finally {
                if (buffer != IntPtr.Zero) {
                    Marshal.FreeHGlobal(buffer);
                }
            }
        }
    }

    public sealed class HeldFile : IDisposable {
        private SafeFileHandle handle;
        private readonly string identity;

        internal HeldFile(SafeFileHandle value) {
            handle = value;
            identity = NativeMethods.GetIdentity(
                NativeMethods.GetInformation(handle));
        }

        public void VerifyAtPath(string path) {
            if (handle == null || handle.IsClosed) {
                throw new ObjectDisposedException("HeldFile");
            }
            var fullPath = Path.GetFullPath(path);
            var information = NativeMethods.GetInformation(handle);
            if ((information.FileAttributes &
                    NativeMethods.FileAttributeDirectory) != 0 ||
                (information.FileAttributes &
                    NativeMethods.FileAttributeReparsePoint) != 0 ||
                !string.Equals(
                    NativeMethods.GetIdentity(information),
                    identity,
                    StringComparison.Ordinal) ||
                !NativeMethods.SamePath(
                    NativeMethods.ConvertFinalPath(
                        NativeMethods.GetFinalPath(handle)),
                    fullPath)) {
                throw new InvalidOperationException(
                    "Held sanitizer file identity moved unexpectedly");
            }
            using (var pathHandle = NativeMethods.OpenRegularFile(
                fullPath,
                NativeMethods.FileReadAttributes,
                NativeMethods.FileShareRead |
                    NativeMethods.FileShareWrite |
                    NativeMethods.FileShareDelete)) {
                var pathInformation =
                    NativeMethods.GetInformation(pathHandle);
                if ((pathInformation.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (pathInformation.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0 ||
                    !string.Equals(
                        NativeMethods.GetIdentity(pathInformation),
                        identity,
                        StringComparison.Ordinal)) {
                    throw new InvalidOperationException(
                        "Held sanitizer file path changed identity");
                }
            }
        }

        public string GetSha256() {
            if (handle == null || handle.IsClosed) {
                throw new ObjectDisposedException("HeldFile");
            }
            var before = NativeMethods.GetInformation(handle);
            using (var duplicate = NativeMethods.Duplicate(handle))
            using (var stream = new FileStream(
                duplicate, FileAccess.Read, 65536, false))
            using (var sha = SHA256.Create()) {
                var digest = sha.ComputeHash(stream);
                var after = NativeMethods.GetInformation(handle);
                if (!string.Equals(
                        NativeMethods.GetIdentity(before),
                        NativeMethods.GetIdentity(after),
                        StringComparison.Ordinal) ||
                    before.FileSizeHigh != after.FileSizeHigh ||
                    before.FileSizeLow != after.FileSizeLow) {
                    throw new InvalidOperationException(
                        "Held sanitizer file changed while hashing");
                }
                var result = new StringBuilder(digest.Length * 2);
                foreach (var value in digest) {
                    result.Append(value.ToString("x2"));
                }
                return result.ToString();
            }
        }

        public void Dispose() {
            if (handle != null) {
                handle.Dispose();
                handle = null;
            }
        }
    }

    public sealed class StageLease : IDisposable {
        private SafeFileHandle parentHandle;
        private SafeFileHandle stageHandle;
        private readonly string parentPath;
        private readonly string parentIdentity;
        private readonly string identity;
        private string currentPath;

        private StageLease(
            SafeFileHandle parent,
            SafeFileHandle stage,
            string parentFullPath,
            string stageFullPath) {
            parentHandle = parent;
            stageHandle = stage;
            parentPath = parentFullPath;
            currentPath = stageFullPath;
            parentIdentity = NativeMethods.GetIdentity(
                NativeMethods.GetInformation(parentHandle));
            identity =
                NativeMethods.GetIdentity(
                    NativeMethods.GetInformation(stageHandle));
        }

        public string CurrentPath {
            get { return currentPath; }
        }

        public string Identity {
            get { return identity; }
        }

        public static StageLease Create(
            string parentPath,
            string stageLeaf) {
            var parentFullPath = Path.GetFullPath(parentPath);
            var stageFullPath = Path.Combine(
                parentFullPath, stageLeaf);
            SafeFileHandle parent = null;
            SafeFileHandle stage = null;
            try {
                parent = NativeMethods.OpenDirectory(
                    parentFullPath,
                    NativeMethods.FileListDirectory |
                        NativeMethods.FileTraverse |
                        NativeMethods.FileReadAttributes |
                        NativeMethods.Synchronize,
                    NativeMethods.FileShareRead |
                        NativeMethods.FileShareWrite);
                var parentInformation =
                    NativeMethods.GetInformation(parent);
                if ((parentInformation.FileAttributes &
                        NativeMethods.FileAttributeDirectory) == 0 ||
                    (parentInformation.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0 ||
                    !NativeMethods.SamePath(
                        NativeMethods.ConvertFinalPath(
                            NativeMethods.GetFinalPath(parent)),
                        parentFullPath)) {
                    throw new InvalidOperationException(
                        "Sanitizer output parent is not its plain held identity");
                }
                stage = NativeMethods.CreateDirectoryRelative(
                    parent, stageLeaf);
                var lease = new StageLease(
                    parent, stage, parentFullPath, stageFullPath);
                parent = null;
                stage = null;
                try {
                    lease.VerifyCurrentPath();
                    return lease;
                } catch {
                    lease.Dispose();
                    throw;
                }
            } finally {
                if (stage != null) {
                    stage.Dispose();
                }
                if (parent != null) {
                    parent.Dispose();
                }
            }
        }

        private void AssertUsable() {
            if (stageHandle == null || stageHandle.IsClosed) {
                throw new ObjectDisposedException("StageLease");
            }
        }

        public void VerifyCurrentPath() {
            AssertUsable();
            NativeMethods.AssertDirectoryHandle(
                stageHandle, currentPath, identity);
            NativeMethods.AssertDirectoryHandle(
                parentHandle, parentPath, parentIdentity);
        }

        public string WriteNewRegularFile(
            string leaf,
            byte[] bytes) {
            VerifyCurrentPath();
            if (string.IsNullOrEmpty(leaf) ||
                leaf == "." ||
                leaf == ".." ||
                leaf.IndexOf('\\') >= 0 ||
                leaf.IndexOf('/') >= 0 ||
                leaf.IndexOf(':') >= 0 ||
                !string.Equals(
                    Path.GetFileName(leaf),
                    leaf,
                    StringComparison.Ordinal)) {
                throw new ArgumentException(
                    "Sanitizer staged file name must be one plain leaf",
                    "leaf");
            }
            if (bytes == null || bytes.Length == 0) {
                throw new ArgumentException(
                    "Sanitizer staged JSON must not be empty",
                    "bytes");
            }

            var expectedPath = Path.Combine(currentPath, leaf);
            var handle = NativeMethods.CreateRegularFileRelative(
                stageHandle, leaf);
            var completed = false;
            try {
                var before = NativeMethods.GetInformation(handle);
                if ((before.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (before.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0 ||
                    !NativeMethods.SamePath(
                        NativeMethods.ConvertFinalPath(
                            NativeMethods.GetFinalPath(handle)),
                        expectedPath)) {
                    throw new InvalidOperationException(
                        "New sanitizer stage file has an unsafe identity");
                }
                using (var duplicate =
                    NativeMethods.Duplicate(handle))
                using (var stream = new FileStream(
                    duplicate, FileAccess.Write, 65536, false)) {
                    stream.Write(bytes, 0, bytes.Length);
                    stream.Flush(true);
                }
                var after = NativeMethods.GetInformation(handle);
                var length =
                    ((long)after.FileSizeHigh << 32) |
                    after.FileSizeLow;
                if (!string.Equals(
                        NativeMethods.GetIdentity(before),
                        NativeMethods.GetIdentity(after),
                        StringComparison.Ordinal) ||
                    length != bytes.LongLength ||
                    (after.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (after.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0) {
                    throw new InvalidOperationException(
                        "New sanitizer stage file changed while held");
                }
                VerifyCurrentPath();
                using (var sha = SHA256.Create()) {
                    var digest = sha.ComputeHash(bytes);
                    var result =
                        new StringBuilder(digest.Length * 2);
                    foreach (var value in digest) {
                        result.Append(value.ToString("x2"));
                    }
                    completed = true;
                    return result.ToString();
                }
            } finally {
                if (!completed) {
                    try {
                        NativeMethods.DeleteByHandle(handle);
                    } catch {
                        // The exact handle remains the only cleanup target.
                    }
                }
                handle.Dispose();
            }
        }

        public HeldFile HoldRegularFile(string path) {
            VerifyCurrentPath();
            var fullPath = Path.GetFullPath(path);
            if (!NativeMethods.SamePath(
                    Path.GetDirectoryName(fullPath), currentPath)) {
                throw new InvalidOperationException(
                    "Staged file is outside the held sanitizer directory");
            }
            var handle = NativeMethods.OpenRegularFile(
                fullPath,
                NativeMethods.FileReadData |
                    NativeMethods.FileReadAttributes |
                    NativeMethods.Synchronize,
                NativeMethods.FileShareRead);
            try {
                var information =
                    NativeMethods.GetInformation(handle);
                if ((information.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (information.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0 ||
                    !NativeMethods.SamePath(
                        NativeMethods.ConvertFinalPath(
                            NativeMethods.GetFinalPath(handle)),
                        fullPath)) {
                    throw new InvalidOperationException(
                        "Sanitizer staged file is not a plain held file");
                }
                var result = new HeldFile(handle);
                result.VerifyAtPath(fullPath);
                handle = null;
                return result;
            } finally {
                if (handle != null) {
                    handle.Dispose();
                }
            }
        }

        public void DeleteRegularFile(string path) {
            VerifyCurrentPath();
            var fullPath = Path.GetFullPath(path);
            if (!NativeMethods.SamePath(
                    Path.GetDirectoryName(fullPath), currentPath)) {
                throw new InvalidOperationException(
                    "Cleanup file is outside the held sanitizer directory");
            }
            using (var handle = NativeMethods.OpenRegularFile(
                fullPath,
                NativeMethods.DeleteAccess |
                    NativeMethods.FileReadAttributes |
                    NativeMethods.Synchronize,
                NativeMethods.FileShareRead |
                    NativeMethods.FileShareWrite)) {
                var information =
                    NativeMethods.GetInformation(handle);
                if ((information.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (information.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0 ||
                    !NativeMethods.SamePath(
                        NativeMethods.ConvertFinalPath(
                            NativeMethods.GetFinalPath(handle)),
                        fullPath)) {
                    throw new InvalidOperationException(
                        "Sanitizer cleanup refuses a non-regular staged file");
                }
                NativeMethods.DeleteByHandle(handle);
            }
            VerifyCurrentPath();
        }

        public void MoveTo(string destinationPath) {
            VerifyCurrentPath();
            var destinationFullPath =
                Path.GetFullPath(destinationPath);
            if (!NativeMethods.SamePath(
                    Path.GetDirectoryName(destinationFullPath),
                    parentPath)) {
                throw new InvalidOperationException(
                    "Sanitizer publication must remain in its held parent");
            }
            NativeMethods.RenameToPath(
                stageHandle,
                destinationFullPath);
            currentPath = destinationFullPath;
            VerifyCurrentPath();
        }

        public void DeleteEmptyDirectory() {
            VerifyCurrentPath();
            NativeMethods.DeleteByHandle(stageHandle);
            stageHandle.Dispose();
            stageHandle = null;
            if (Directory.Exists(currentPath)) {
                throw new IOException(
                    "Held sanitizer staging directory remained after deletion");
            }
        }

        public void Dispose() {
            if (stageHandle != null) {
                stageHandle.Dispose();
                stageHandle = null;
            }
            if (parentHandle != null) {
                parentHandle.Dispose();
                parentHandle = null;
            }
        }
    }
}
'@
}

function ConvertFrom-KettlePerfFinalWindowsPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        return '\\' + $Path.Substring(8)
    }
    if ($Path.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) {
        return $Path.Substring(4)
    }
    return $Path
}

function Read-KettlePerfSanitizeJson {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [long]$MaximumBytes,
        [ValidateRange(1, 256)]
        [int]$MaximumDepth = $script:SanitizeMaximumDepth,
        [ValidateRange(1, 2000000)]
        [int]$MaximumNodes = $script:SanitizeMaximumNodes
    )

    $full = [IO.Path]::GetFullPath($Path)
    Assert-KettlePerfSafeWindowsPathSyntax -Path $full
    Assert-KettlePerfNoReparseSanitizeAncestors -Path $full
    $before = Get-Item -LiteralPath $full -Force -ErrorAction Stop
    if ($before.PSIsContainer) {
        throw "Expected a JSON file, found a directory: $full"
    }
    if (($before.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Sanitizer refuses a reparse-point JSON file: $full"
    }
    $stream = [IO.FileStream]::new(
        $full,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::None,
        65536,
        [IO.FileOptions]::SequentialScan
    )
    try {
        if ($stream.Length -lt 1 -or $stream.Length -gt $MaximumBytes) {
            throw "JSON size is outside the sanitizer bound: $full"
        }
        if ($script:IsWindowsPlatform) {
            Initialize-KettlePerfSanitizeNativeMethods
            $handlePath = ConvertFrom-KettlePerfFinalWindowsPath -Path (
                [KettlePerfSanitize.NativeMethods]::GetFinalPath(
                    $stream.SafeFileHandle
                )
            )
            if (-not (
                Test-KettlePerfSameSanitizePath -Left $full -Right $handlePath
            )) {
                throw "JSON path aliases a different file identity: $full"
            }
        }
        $during = Get-Item -LiteralPath $full -Force -ErrorAction Stop
        if (
            $during.PSIsContainer -or
            ($during.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            [long]$during.Length -ne [long]$stream.Length
        ) {
            throw "JSON identity changed while held for validation: $full"
        }
        $length = [int]$stream.Length
        $bytes = [byte[]]::new($length)
        $offset = 0
        while ($offset -lt $length) {
            $read = $stream.Read($bytes, $offset, $length - $offset)
            if ($read -eq 0) {
                throw "Unexpected end of JSON file: $full"
            }
            $offset += $read
        }
        if (
            $length -ge 3 -and
            $bytes[0] -eq 0xEF -and
            $bytes[1] -eq 0xBB -and
            $bytes[2] -eq 0xBF
        ) {
            throw "UTF-8 BOM is not accepted in sanitizer input: $full"
        }
        $utf8 = [Text.UTF8Encoding]::new($false, $true)
        try {
            $text = $utf8.GetString($bytes)
        } catch {
            throw "JSON is not strict UTF-8: $full"
        }
        try {
            $shape = Get-KettlePerfEvidenceJsonShape `
                -Text $text -MaximumDepth $MaximumDepth `
                -MaximumNodes $MaximumNodes
        } catch {
            throw "JSON shape is outside the sanitizer bound: $full"
        }
        try {
            $value = $text | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw "JSON could not be parsed: $full"
        }
        return [pscustomobject]@{
            bytes = [long]$length
            value = $value
            json_depth = [int]$shape.maximum_depth
            json_nodes = [int]$shape.nodes
            sha256 = (
                -join @(
                    $sha = [Security.Cryptography.SHA256]::Create()
                    try {
                        $sha.ComputeHash($bytes) |
                            ForEach-Object { $_.ToString('x2') }
                    } finally {
                        $sha.Dispose()
                    }
                )
            )
        }
    } finally {
        $stream.Dispose()
    }
}

function Write-KettlePerfSanitizeStageJson {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'The function creates one exact file in a new private stage.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Stage,
        [Parameter(Mandatory)]
        [string]$FileName,
        [Parameter(Mandatory)]
        $InputObject,
        [ValidateRange(1, 100)]
        [int]$Depth,
        $Lease
    )

    if (
        [IO.Path]::GetFileName($FileName) -ne $FileName -or
        $FileName -in @('.', '..') -or
        $FileName.Contains(':')
    ) {
        throw "Unsafe sanitizer stage file name: $FileName"
    }
    $json = $InputObject | ConvertTo-Json -Depth $Depth
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    $bytes = $utf8.GetBytes($json)
    if (
        $bytes.LongLength -lt 1 -or
        $bytes.LongLength -gt $script:SanitizeMaximumFileBytes
    ) {
        throw "Sanitized JSON size is outside the bundle bound: $FileName"
    }

    $path = Join-Path $Stage $FileName
    if ($null -ne $Lease) {
        $expectedSha = $Lease.WriteNewRegularFile(
            $FileName,
            $bytes
        )
    } else {
        $stream = [IO.FileStream]::new(
            $path,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::Read,
            65536,
            [IO.FileOptions]::SequentialScan
        )
        $completed = $false
        try {
            $stream.Write($bytes, 0, $bytes.Length)
            $stream.Flush($true)
            $completed = $true
        } finally {
            $stream.Dispose()
            if (-not $completed) {
                [IO.File]::Delete($path)
            }
        }
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            $expectedSha = -join @(
                $sha.ComputeHash($bytes) |
                    ForEach-Object { $_.ToString('x2') }
            )
        } finally {
            $sha.Dispose()
        }
    }

    $validated = Read-KettlePerfSanitizeJson `
        -Path $path `
        -MaximumBytes $script:SanitizeMaximumFileBytes
    if (
        $validated.bytes -ne $bytes.LongLength -or
        $validated.sha256 -ne $expectedSha
    ) {
        throw "Sanitizer stage file changed after exact creation: $FileName"
    }
    return $validated
}

function Get-KettlePerfRedactionToken {
    param(
        [Parameter(Mandatory)]
        [string]$Value,
        [Parameter(Mandatory)]
        [ValidateCount(32, 32)]
        [byte[]]$Secret,
        [string]$Kind = 'value'
    )

    $hmac = [Security.Cryptography.HMACSHA256]::new($Secret)
    $bytes = $null
    try {
        $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes(
            "$Kind`0$Value"
        )
        $digest = $hmac.ComputeHash($bytes)
        $hex = -join @($digest | ForEach-Object { $_.ToString('x2') })
    } finally {
        if ($null -ne $bytes) {
            [Array]::Clear($bytes, 0, $bytes.Length)
        }
        $hmac.Dispose()
    }
    return "<redacted-$Kind`:$($hex.Substring(0, 16))>"
}

function Test-KettlePerfSensitiveProperty {
    param([string]$Name)

    if (-not $Name) {
        return $false
    }
    # Credential keys are normalized to one ASCII comparison domain so
    # snake_case, kebab-case, camelCase, PascalCase, and upper-case spellings
    # cannot bypass redaction. This check deliberately precedes the generic
    # hash allowlist: password_hash and api_key_sha256 are still credentials.
    $credentialName = [regex]::Replace(
        $Name,
        '[^A-Za-z0-9]',
        ''
    ).ToLowerInvariant()
    if (
        $credentialName -match (
            'password|passwd|passphrase|secret|credential|' +
            'authorization|bearer|cookie|token|privatekey|signingkey|' +
            'apikey|accesskey|accountkey|subscriptionkey|' +
            'connectionstring'
        )
    ) {
        return $true
    }
    if (
        $Name -match (
            '(?i)(^|_)(' +
            'computer_name|computername|computer|machine_name|host_name|' +
            'hostname|host|fqdn|user_name|username|userid|user_id|user|' +
            'login_name|account_name|owner_name|adapter_luid|source_id|' +
            'target_id|connector_instance|hardware_id|' +
            'registry_edid_sha256' +
            ')$'
        )
    ) {
        return $true
    }
    if ($Name -match '(?i)(sha256|hash|version|algorithm|encoding)$') {
        return $false
    }
    return $Name -match (
        '(?i)(^|_)(' +
        'path|root|directory|launcher|executable|command|artifacts|repo|' +
        'instance_name|serial_number|monitor_device_id|device_name|' +
        'computer_name|computername|computer|machine_name|host_name|' +
        'hostname|host|fqdn|user_name|username|userid|user_id|user|' +
        'login_name|account_name|owner_name' +
        ')$'
    )
}

function Test-KettlePerfPublicEvidenceSourceLeafName {
    param([string]$Name)

    switch -CaseSensitive ($Name) {
        'benchmark-manifest.json' { return $true }
        'startup-idle.json' { return $true }
        'latency.json' { return $true }
        'vtebench-summary.json' { return $true }
        'menu-hover.json' { return $true }
        'native-display-menu-hover.json' { return $true }
        'monitor-transition.json' { return $true }
        'score.json' { return $true }
    }
    return $Name -cmatch (
        '^throughput-(?:kettle|wt|alacritty|wezterm|rio|tabby)\.json$'
    )
}

function ConvertTo-KettlePerfRedactionMaterial {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [AllowEmptyCollection()]
        $Value
    )

    $separator = [char]0
    $invariant = [Globalization.CultureInfo]::InvariantCulture
    if ($Value -is [string]) {
        return 'string' + $separator + $Value
    }
    if ($Value -is [bool]) {
        $text = if ([bool]$Value) { 'true' } else { 'false' }
        return 'boolean' + $separator + $text
    }
    if (
        $Value -is [sbyte] -or
        $Value -is [byte] -or
        $Value -is [int16] -or
        $Value -is [uint16] -or
        $Value -is [int32] -or
        $Value -is [uint32] -or
        $Value -is [int64] -or
        $Value -is [uint64]
    ) {
        return (
            'integer' + $separator +
            ([IFormattable]$Value).ToString($null, $invariant)
        )
    }
    if ($Value -is [single]) {
        return (
            'number' + $separator +
            ([single]$Value).ToString('R', $invariant)
        )
    }
    if ($Value -is [double]) {
        return (
            'number' + $separator +
            ([double]$Value).ToString('R', $invariant)
        )
    }
    if ($Value -is [decimal]) {
        return (
            'number' + $separator +
            ([decimal]$Value).ToString('G29', $invariant)
        )
    }
    if ($Value -is [datetime]) {
        return (
            'datetime' + $separator +
            ([datetime]$Value).ToUniversalTime().ToString('o', $invariant)
        )
    }

    $json = ConvertTo-Json -InputObject $Value -Compress `
        -Depth $script:SanitizeMaximumDepth
    return 'json' + $separator + $json
}

function Protect-KettlePerfPublicString {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string]$Value,
        [Parameter(Mandatory)]
        [ValidateCount(32, 32)]
        [byte[]]$Secret,
        [string]$PropertyName = ''
    )

    if (Test-KettlePerfSensitiveProperty $PropertyName) {
        return Get-KettlePerfRedactionToken `
            -Value $Value -Secret $Secret -Kind 'field'
    }
    $protected = $Value
    $patterns = @(
        (
            '(?i)(?<![A-Za-z0-9_])(?:[A-Z]:[\\/]|\\\\)' +
                '[^"''<>\r\n|]*'
        )
        '(?<![A-Za-z0-9_:/])/(?!/)[^"''<>\r\n|]*'
    )
    foreach ($pattern in $patterns) {
        $protected = [regex]::Replace(
            $protected,
            $pattern,
            {
                param($match)
                if (-not $match.Value -or $match.Value -eq '/') {
                    return $match.Value
                }
                return Get-KettlePerfRedactionToken `
                    -Value $match.Value -Secret $Secret -Kind 'path'
            }
        )
    }
    return $protected
}

function ConvertTo-KettlePerfPublicValue {
    param(
        $Value,
        [Parameter(Mandatory)]
        [ValidateCount(32, 32)]
        [byte[]]$Secret,
        [string]$PropertyName = '',
        [int]$Depth = 0
    )

    if ($Depth -gt $script:SanitizeMaximumDepth) {
        throw 'Performance evidence nesting exceeds the public schema bound'
    }
    $script:SanitizeNodeCount++
    if ($script:SanitizeNodeCount -gt $script:SanitizeMaximumNodes) {
        throw 'Performance evidence node count exceeds the public schema bound'
    }
    if ($null -eq $Value) {
        return $null
    }
    if (Test-KettlePerfSensitiveProperty $PropertyName) {
        $material = ConvertTo-KettlePerfRedactionMaterial -Value $Value
        return Get-KettlePerfRedactionToken `
            -Value $material -Secret $Secret -Kind 'field'
    }
    if ($Value -is [string]) {
        return Protect-KettlePerfPublicString `
            -Value $Value -Secret $Secret -PropertyName $PropertyName
    }
    if (
        $Value -is [bool] -or
        $Value -is [byte] -or
        $Value -is [int16] -or
        $Value -is [int32] -or
        $Value -is [int64] -or
        $Value -is [uint16] -or
        $Value -is [uint32] -or
        $Value -is [uint64] -or
        $Value -is [single] -or
        $Value -is [double] -or
        $Value -is [decimal] -or
        $Value -is [datetime]
    ) {
        return $Value
    }
    if ($Value -is [System.Collections.IDictionary]) {
        $result = [ordered]@{}
        foreach ($entry in $Value.GetEnumerator()) {
            $name = [string]$entry.Key
            $result[$name] = ConvertTo-KettlePerfPublicValue `
                -Value $entry.Value -Secret $Secret -PropertyName $name `
                -Depth ($Depth + 1)
        }
        return $result
    }
    if (
        $Value -is [System.Collections.IEnumerable] -and
        $Value -isnot [pscustomobject]
    ) {
        return [object[]]@(
            foreach ($item in $Value) {
                ConvertTo-KettlePerfPublicValue `
                    -Value $item -Secret $Secret -PropertyName $PropertyName `
                    -Depth ($Depth + 1)
            }
        )
    }
    $objectResult = [ordered]@{}
    foreach ($property in $Value.PSObject.Properties) {
        $objectResult[$property.Name] = ConvertTo-KettlePerfPublicValue `
            -Value $property.Value -Secret $Secret `
            -PropertyName $property.Name -Depth ($Depth + 1)
    }
    return $objectResult
}

function Get-KettlePerfSanitizeFlatItems {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function returns every immediate stage item.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Stage,
        $Lease
    )

    if ($null -ne $Lease) {
        $Lease.VerifyCurrentPath()
    } else {
        Assert-KettlePerfNoReparseSanitizeAncestors -Path $Stage
        $stageItem = Get-Item -LiteralPath $Stage -Force -ErrorAction Stop
        if (
            -not $stageItem.PSIsContainer -or
            ($stageItem.Attributes -band
                [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw 'Sanitizer stage root is not a plain directory'
        }
    }

    $items = @(
        Get-ChildItem -LiteralPath $Stage -Force -ErrorAction Stop
    )
    foreach ($item in $items) {
        if (
            $item.PSIsContainer -or
            ($item.Attributes -band
                [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "Sanitizer staging bundle contains an unsafe item: $($item.Name)"
        }
        if (
            [IO.Path]::GetFileName($item.FullName) -ne $item.Name -or
            [IO.Path]::GetDirectoryName(
                [IO.Path]::GetFullPath($item.FullName)
            ) -ne [IO.Path]::GetFullPath($Stage).TrimEnd(
                [char[]]@('\', '/')
            )
        ) {
            throw "Sanitizer staging bundle contains an unsafe name: $($item.Name)"
        }
        if ($script:IsWindowsPlatform) {
            $streams = @(
                Get-Item -LiteralPath $item.FullName -Stream * `
                    -ErrorAction Stop
            )
            if (
                $streams.Count -ne 1 -or
                $streams[0].Stream -ne ':$DATA'
            ) {
                throw (
                    'Sanitizer staging bundle contains an alternate data ' +
                    "stream: $($item.Name)"
                )
            }
        }
    }
    if ($null -ne $Lease) {
        $Lease.VerifyCurrentPath()
    }
    return [object[]]$items
}

function Assert-KettlePerfSanitizeExactFileSet {
    param(
        [Parameter(Mandatory)]
        [string]$Stage,
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$ExpectedNames,
        $Lease,
        [switch]$AllowMissing
    )

    $items = @(
        Get-KettlePerfSanitizeFlatItems -Stage $Stage -Lease $Lease
    )
    $actualNames = [string[]]@(
        $items | Sort-Object Name | ForEach-Object { $_.Name }
    )
    $expected = [string[]]@($ExpectedNames | Sort-Object)
    $unexpected = @(
        $actualNames | Where-Object { $expected -notcontains $_ }
    )
    if ($unexpected.Count -ne 0) {
        throw (
            'Sanitizer staging bundle contains unexpected files: ' +
            ($unexpected -join ', ')
        )
    }
    if (
        -not $AllowMissing -and
        (
            $actualNames.Count -ne $expected.Count -or
            @(Compare-Object $expected $actualNames).Count -ne 0
        )
    ) {
        throw 'Sanitizer staging bundle contains an unexpected file set'
    }
    return [object[]]$items
}

function Publish-KettlePerfSanitizeStage {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'The function atomically commits one validated stage.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Stage,
        [Parameter(Mandatory)]
        [string]$Output,
        [Parameter(Mandatory)]
        [string[]]$ExpectedNames,
        [Parameter(Mandatory)]
        [Collections.IDictionary]$ExpectedHashes,
        [Parameter(Mandatory)]
        $Lease,
        [scriptblock]$BeforeMoveTestAction,
        [scriptblock]$BeforeRootMoveTestAction
    )

    if (Test-Path -LiteralPath $Output) {
        throw 'Public output directory appeared during sanitization'
    }
    if ($null -ne $BeforeMoveTestAction) {
        & $BeforeMoveTestAction
    }
    if ($null -ne $BeforeRootMoveTestAction) {
        & $BeforeRootMoveTestAction
    }

    $postMoveFiles = [Collections.Generic.List[IDisposable]]::new()
    $movedToOutput = $false
    try {
        $Lease.MoveTo($Output)
        $movedToOutput = $true
        $null = Assert-KettlePerfSanitizeExactFileSet `
            -Stage $Output -ExpectedNames $ExpectedNames `
            -Lease $Lease
        foreach ($fileName in $ExpectedNames) {
            $heldFile = $Lease.HoldRegularFile(
                (Join-Path $Output $fileName)
            )
            $postMoveFiles.Add($heldFile)
            if (
                $heldFile.GetSha256() -ne
                    [string]$ExpectedHashes[$fileName]
            ) {
                throw "Published sanitizer file hash changed: $fileName"
            }
        }
        $null = Assert-KettlePerfSanitizeExactFileSet `
            -Stage $Output -ExpectedNames $ExpectedNames `
            -Lease $Lease
    } catch {
        $publicationFailure = $_
        foreach ($heldFile in $postMoveFiles) {
            $heldFile.Dispose()
        }
        $postMoveFiles.Clear()
        if (
            $movedToOutput -or
            (
                Test-KettlePerfSameSanitizePath `
                    -Left $Lease.CurrentPath -Right $Output
            )
        ) {
            $rollbackStage = Join-Path (
                [IO.Path]::GetDirectoryName(
                    [IO.Path]::GetFullPath($Stage)
                )
            ) (
                '.kettle-sanitize-stage-' +
                [Guid]::NewGuid().ToString('N')
            )
            try {
                $Lease.MoveTo($rollbackStage)
            } catch {
                throw (
                    'Sanitizer publication validation failed and the exact ' +
                    'output identity could not be rolled back: ' +
                    $_.Exception.Message
                )
            }
            if (Test-Path -LiteralPath $Output) {
                throw (
                    'Sanitizer publication validation failed and the output ' +
                    'path remained after exact rollback'
                )
            }
        }
        throw $publicationFailure
    } finally {
        foreach ($heldFile in $postMoveFiles) {
            $heldFile.Dispose()
        }
    }
}

function Remove-KettlePerfSanitizeStage {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Only exact held stage identities and files are removed.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Stage,
        [Parameter(Mandatory)]
        [string]$Parent,
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$ExpectedNames,
        [Parameter(Mandatory)]
        [string]$PublicationPath,
        $Lease
    )

    if (
        -not (Test-KettlePerfSanitizePathWithin -Path $Stage -Root $Parent) -or
        (
            (Split-Path -Leaf $Stage) -notmatch
                '^\.kettle-sanitize-stage-[0-9a-f]{32}$' -and
            -not (
                Test-KettlePerfSameSanitizePath `
                    -Left $Stage -Right $PublicationPath
            )
        )
    ) {
        throw "Refusing unsafe sanitizer stage cleanup: $Stage"
    }

    if ($null -ne $Lease) {
        if (-not (
            Test-KettlePerfSameSanitizePath `
                -Left $Stage -Right $Lease.CurrentPath
        )) {
            throw 'Refusing cleanup outside the held sanitizer stage identity'
        }
        $items = @(
            Assert-KettlePerfSanitizeExactFileSet `
                -Stage $Stage -ExpectedNames $ExpectedNames `
                -Lease $Lease -AllowMissing
        )
        foreach ($item in $items) {
            $Lease.DeleteRegularFile($item.FullName)
        }
        $Lease.DeleteEmptyDirectory()
        return
    }

    if (Test-Path -LiteralPath $Stage) {
        $items = @(
            Assert-KettlePerfSanitizeExactFileSet `
                -Stage $Stage -ExpectedNames $ExpectedNames -AllowMissing
        )
        foreach ($item in $items) {
            [IO.File]::Delete($item.FullName)
        }
        [IO.Directory]::Delete($Stage, $false)
    }
}

Assert-KettlePerfSafeWindowsPathSyntax -Path $ResultsDir
Assert-KettlePerfSafeWindowsPathSyntax -Path $OutputDir -AllowMissingLeaf
$resultsFull = [IO.Path]::GetFullPath($ResultsDir)
$outputFull = [IO.Path]::GetFullPath($OutputDir)
if (-not (Test-Path -LiteralPath $resultsFull -PathType Container)) {
    throw "Performance results directory not found: $resultsFull"
}
Assert-KettlePerfNoReparseSanitizeAncestors -Path $resultsFull
$resultsFull = (Resolve-Path -LiteralPath $resultsFull).Path
if (
    Test-KettlePerfSameSanitizePath `
        -Left $resultsFull -Right ([IO.Path]::GetPathRoot($resultsFull))
) {
    throw 'A filesystem root is not a safe private results directory'
}

$outputParent = [IO.Path]::GetDirectoryName($outputFull)
if (-not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    throw "Public output parent directory does not exist: $outputParent"
}
Assert-KettlePerfNoReparseSanitizeAncestors -Path $outputParent
if (
    Test-KettlePerfSameSanitizePath `
        -Left $outputParent -Right ([IO.Path]::GetPathRoot($outputParent))
) {
    throw 'A filesystem root is not a safe public output parent'
}
if (Test-Path -LiteralPath $outputFull) {
    throw 'Public output directory must not already exist'
}
if (
    (Test-KettlePerfSanitizePathWithin -Path $outputFull -Root $resultsFull) -or
    (Test-KettlePerfSanitizePathWithin -Path $resultsFull -Root $outputFull)
) {
    throw 'Public output and private results directories must not overlap'
}

$stageLeaf = '.kettle-sanitize-stage-' + [Guid]::NewGuid().ToString('N')
$stage = Join-Path $outputParent $stageLeaf
$publishedStage = $false
$stageLease = $null
$sourceSnapshot = $null
$redactionSecret = [byte[]]::new(32)
$stageExpectedNames = [Collections.Generic.List[string]]::new()
$stageExpectedHashes = @{}
$heldStageFiles = [Collections.Generic.List[IDisposable]]::new()
try {
    $sourceDocuments = [ordered]@{}
    $totalSourceBytes = [long]0
    if ($script:IsWindowsPlatform) {
        $sourceSnapshot = Open-KettlePerfEvidenceSnapshot `
            -Directory $resultsFull `
            -MaximumFiles $script:SanitizeMaximumFiles `
            -MaximumTotalBytes $script:SanitizeMaximumTotalBytes
        $resultsFull = $sourceSnapshot.root_path
        $sourceNames = [string[]]@(
            Get-KettlePerfEvidenceLeafNames `
                -Snapshot $sourceSnapshot -Extension '.json' `
                -MaximumNames $script:SanitizeMaximumFiles
        )
        if ($sourceNames.Count -eq 0) {
            throw (
                'Private benchmark JSON file count is outside the ' +
                'public bundle bound'
            )
        }
        foreach ($sourceName in $sourceNames) {
            Assert-KettlePerfSafeWindowsPathSyntax -Path (
                Join-Path $resultsFull $sourceName
            )
        }
    } else {
        $boundedFiles = [Collections.Generic.List[object]]::new()
        foreach (
            $path in [IO.Directory]::EnumerateFileSystemEntries(
                $resultsFull,
                '*',
                [IO.SearchOption]::TopDirectoryOnly
            )
        ) {
            if (
                -not [string]::Equals(
                    [IO.Path]::GetExtension($path),
                    '.json',
                    [StringComparison]::OrdinalIgnoreCase
                )
            ) {
                continue
            }
            $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
            $boundedFiles.Add($item)
            if (
                $boundedFiles.Count -gt
                    $script:SanitizeMaximumFiles
            ) {
                throw (
                    'Private benchmark JSON file count is outside the ' +
                    'public bundle bound'
                )
            }
        }
        if ($boundedFiles.Count -eq 0) {
            throw (
                'Private benchmark JSON file count is outside the ' +
                'public bundle bound'
            )
        }
        $jsonFiles = @($boundedFiles | Sort-Object Name)
        $sourceNames = [string[]]@(
            $jsonFiles | ForEach-Object { $_.Name }
        )
    }
    if ($sourceNames -notcontains 'benchmark-manifest.json') {
        throw 'benchmark-manifest.json is required for public evidence'
    }
    if ($sourceNames -contains 'public-evidence.json') {
        throw 'Private results cannot contain the reserved public evidence name'
    }
    if (@($sourceNames | Where-Object {
        -not (Test-KettlePerfPublicEvidenceSourceLeafName $_)
    }).Count -ne 0) {
        throw (
            'Private results contain a JSON file outside the reviewed ' +
            'public evidence filename contract'
        )
    }

    if ($null -ne $sourceSnapshot) {
        $sourceEntries = [object[]]@(
            Read-KettlePerfEvidenceJsonSet `
                -Snapshot $sourceSnapshot -LeafNames $sourceNames `
                -MaximumBytes $script:SanitizeMaximumFileBytes `
                -MaximumDepth $script:SanitizeMaximumDepth `
                -MaximumTotalNodes $script:SanitizeMaximumNodes
        )
        $afterNames = [string[]]@(
            Get-KettlePerfEvidenceLeafNames `
                -Snapshot $sourceSnapshot -Extension '.json' `
                -MaximumNames $script:SanitizeMaximumFiles
        )
        if (
            $afterNames.Count -ne $sourceNames.Count -or
            @(Compare-Object $sourceNames $afterNames).Count -ne 0
        ) {
            throw 'Private benchmark JSON file set changed during sanitization'
        }
        foreach ($entry in $sourceEntries) {
            $totalSourceBytes += $entry.bytes
            $sourceDocuments[$entry.leaf_name] = $entry.value
        }
    } else {
        foreach ($file in $jsonFiles) {
            Assert-KettlePerfSafeWindowsPathSyntax -Path $file.FullName
            if (
                [IO.Path]::GetFileName($file.FullName) -ne $file.Name -or
                $file.Extension -ne '.json' -or
                $file.PSIsContainer -or
                (
                    $file.Attributes -band
                        [IO.FileAttributes]::ReparsePoint
                ) -ne 0 -or
                [long]$file.Length -lt 1 -or
                [long]$file.Length -gt
                    $script:SanitizeMaximumFileBytes
            ) {
                throw "Unsafe performance JSON file: $($file.Name)"
            }
            if (
                [long]$file.Length -gt
                    (
                        $script:SanitizeMaximumTotalBytes -
                        $totalSourceBytes
                    )
            ) {
                throw (
                    'Private benchmark JSON total exceeds the ' +
                    'public bundle bound'
                )
            }
            $totalSourceBytes += [long]$file.Length
        }
        $totalSourceNodes = 0
        foreach ($file in $jsonFiles) {
            $document = Read-KettlePerfSanitizeJson `
                -Path $file.FullName `
                -MaximumBytes $script:SanitizeMaximumFileBytes `
                -MaximumDepth $script:SanitizeMaximumDepth `
                -MaximumNodes (
                    $script:SanitizeMaximumNodes - $totalSourceNodes
                )
            $totalSourceNodes += $document.json_nodes
            $sourceDocuments[$file.Name] = $document.value
        }
    }

    if ($script:IsWindowsPlatform) {
        Initialize-KettlePerfSanitizeNativeMethods
        $stageLease = [KettlePerfSanitize.StageLease]::Create(
            $outputParent,
            $stageLeaf
        )
    } else {
        [IO.Directory]::CreateDirectory($stage) | Out-Null
        Assert-KettlePerfNoReparseSanitizeAncestors -Path $stage
    }

    $privateManifest = $sourceDocuments['benchmark-manifest.json']
    $runId = [string]$privateManifest.run_id
    $parsedRunId = [Guid]::Empty
    if (-not [Guid]::TryParseExact($runId, 'D', [ref]$parsedRunId)) {
        throw 'Private benchmark manifest has no valid run id'
    }
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $random.GetBytes($redactionSecret)
    } finally {
        $random.Dispose()
    }

    $published = [Collections.Generic.List[object]]::new()
    $totalPublishedBytes = [long]0
    $script:SanitizeNodeCount = 0
    foreach ($fileName in $sourceNames) {
        $publicValue = ConvertTo-KettlePerfPublicValue `
            -Value $sourceDocuments[$fileName] -Secret $redactionSecret
        if ($null -ne $stageLease) {
            $stageLease.VerifyCurrentPath()
        }
        $validated = Write-KettlePerfSanitizeStageJson `
            -Stage $stage -FileName $fileName `
            -InputObject $publicValue `
            -Depth $script:SanitizeMaximumDepth `
            -Lease $stageLease
        $stageExpectedNames.Add($fileName)
        $totalPublishedBytes += $validated.bytes
        if ($totalPublishedBytes -gt $script:SanitizeMaximumTotalBytes) {
            throw 'Sanitized JSON total exceeds the public bundle bound'
        }
        $stageExpectedHashes[$fileName] = $validated.sha256
        $published.Add([ordered]@{
            name = $fileName
            bytes = $validated.bytes
            sha256 = $validated.sha256
        })
    }

    $publicManifest = [ordered]@{
        schema_version = 2
        run_id = $runId
        generated_at = (Get-Date).ToString('o')
        source = 'sanitized-json-only'
        raw_artifacts_included = $false
        redactions = [string[]]@(
            'absolute and local paths',
            'commands and executable locations',
            'machine and user identities',
            'monitor serials and device instance identifiers',
            'display hardware, EDID fingerprints, and connector routing',
            'artifact and configuration directories'
        )
        files = [object[]]$published.ToArray()
    }
    if ($null -ne $stageLease) {
        $stageLease.VerifyCurrentPath()
    }
    $validatedIndex = Write-KettlePerfSanitizeStageJson `
        -Stage $stage -FileName 'public-evidence.json' `
        -InputObject $publicManifest -Depth 6 `
        -Lease $stageLease
    $stageExpectedNames.Add('public-evidence.json')
    $stageExpectedHashes['public-evidence.json'] = $validatedIndex.sha256

    $expectedStageNames = [string[]]@(
        $sourceNames
        'public-evidence.json'
    ) | Sort-Object
    $null = Assert-KettlePerfSanitizeExactFileSet `
        -Stage $stage -ExpectedNames $expectedStageNames `
        -Lease $stageLease
    if ($null -ne $stageLease) {
        foreach ($fileName in $expectedStageNames) {
            $heldFile = $stageLease.HoldRegularFile(
                (Join-Path $stage $fileName)
            )
            $heldStageFiles.Add($heldFile)
            if (
                $heldFile.GetSha256() -ne
                    [string]$stageExpectedHashes[$fileName]
            ) {
                throw "Sanitizer staged file changed before publication: $fileName"
            }
        }
        $null = Assert-KettlePerfSanitizeExactFileSet `
            -Stage $stage -ExpectedNames $expectedStageNames `
            -Lease $stageLease
        foreach ($heldFile in $heldStageFiles) {
            $heldFile.Dispose()
        }
        $heldStageFiles.Clear()
    }
    if ($null -ne $BeforePublishSourceTestAction) {
        if ($null -eq $sourceSnapshot) {
            throw (
                'The source publication test hook requires a retained ' +
                'Windows evidence snapshot'
            )
        }
        & $BeforePublishSourceTestAction
    }
    if ($null -ne $sourceSnapshot) {
        $finalSourceNames = [string[]]@(
            Get-KettlePerfEvidenceLeafNames `
                -Snapshot $sourceSnapshot -Extension '.json' `
                -MaximumNames $script:SanitizeMaximumFiles
        )
        if (
            $finalSourceNames.Count -ne $sourceNames.Count -or
            @(Compare-Object $sourceNames $finalSourceNames).Count -ne 0
        ) {
            throw 'Private benchmark JSON file set changed before publication'
        }
    }
    if (Test-Path -LiteralPath $outputFull) {
        throw 'Public output directory appeared during sanitization'
    }
    if ($null -ne $stageLease) {
        Publish-KettlePerfSanitizeStage `
            -Stage $stage -Output $outputFull `
            -ExpectedNames $expectedStageNames `
            -ExpectedHashes $stageExpectedHashes `
            -Lease $stageLease
        $publishedStage = $true
        $stage = $stageLease.CurrentPath
    } else {
        Assert-KettlePerfNoReparseSanitizeAncestors -Path $stage
        [IO.Directory]::Move($stage, $outputFull)
        $publishedStage = $true
        $stage = $outputFull
    }
} finally {
    foreach ($heldFile in $heldStageFiles) {
        $heldFile.Dispose()
    }
    try {
        if (-not $publishedStage) {
            if ($null -ne $stageLease) {
                Remove-KettlePerfSanitizeStage `
                    -Stage $stageLease.CurrentPath -Parent $outputParent `
                    -ExpectedNames $stageExpectedNames.ToArray() `
                    -PublicationPath $outputFull `
                    -Lease $stageLease
            } elseif (Test-Path -LiteralPath $stage) {
                Remove-KettlePerfSanitizeStage `
                    -Stage $stage -Parent $outputParent `
                    -ExpectedNames $stageExpectedNames.ToArray() `
                    -PublicationPath $outputFull
            }
        }
    } finally {
        try {
            if ($null -ne $stageLease) {
                $stageLease.Dispose()
            }
        } finally {
            try {
                Close-KettlePerfEvidenceSnapshot $sourceSnapshot
            } finally {
                [Array]::Clear(
                    $redactionSecret,
                    0,
                    $redactionSecret.Length
                )
            }
        }
    }
}

Write-Host "sanitized performance evidence written to $outputFull"
