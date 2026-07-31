# Root-relative, UTF-8, atomic publication for benchmark evidence. A retained
# ordinary-directory handle prevents root swaps, and NtCreateFile plus
# SetFileInformationByHandle prevent a preplaced reparse leaf from redirecting
# writes outside that root.

Set-StrictMode -Version Latest

function Initialize-KettlePerfPersistenceTypes {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The initialized assembly exposes one related type family.'
    )]
    param()

    if ('KettlePerfPersistence.OutputRoot' -as [type]) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfPersistence {
internal static class NativeMethods {
    internal const uint FileListDirectory = 0x00000001;
    internal const uint FileWriteData = 0x00000002;
    internal const uint FileTraverse = 0x00000020;
    internal const uint FileReadAttributes = 0x00000080;
    internal const uint Delete = 0x00010000;
    internal const uint Synchronize = 0x00100000;
    internal const uint FileShareRead = 0x00000001;
    internal const uint FileShareWrite = 0x00000002;
    internal const uint OpenExisting = 3;
    internal const uint FileCreate = 2;
    internal const uint FileAttributeDirectory = 0x00000010;
    internal const uint FileAttributeReparsePoint = 0x00000400;
    internal const uint FileDirectoryFile = 0x00000001;
    internal const uint FileSynchronousIoNonAlert = 0x00000020;
    internal const uint FileNonDirectoryFile = 0x00000040;
    internal const uint FileOpenReparsePoint = 0x00200000;
    internal const uint FileFlagBackupSemantics = 0x02000000;
    internal const uint FileFlagOpenReparsePoint = 0x00200000;
    internal const uint ObjCaseInsensitive = 0x00000040;
    internal const uint ObjDontReparse = 0x00001000;
    internal const int FileRenameInfo = 3;
    internal const int FileDispositionInfo = 4;
    internal const int FileRenameInformation = 10;

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

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetFileInformationByHandle(
        SafeFileHandle file,
        int informationClass,
        IntPtr information,
        uint informationLength);

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
    private static extern int NtSetInformationFile(
        SafeFileHandle fileHandle,
        out IoStatusBlock ioStatusBlock,
        IntPtr fileInformation,
        uint length,
        int fileInformationClass);

    [DllImport("ntdll.dll")]
    private static extern uint RtlNtStatusToDosError(int status);

    internal static SafeFileHandle OpenRoot(string path) {
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
                error, "Opening the performance output root failed");
        }
        return handle;
    }

    internal static SafeFileHandle CreateRelative(
        SafeFileHandle root,
        string leaf,
        bool directory) {
        ValidateLeaf(leaf);
        IntPtr nameBuffer = IntPtr.Zero;
        IntPtr unicodeBuffer = IntPtr.Zero;
        try {
            var nameBytes = Encoding.Unicode.GetBytes(leaf);
            if (nameBytes.Length > ushort.MaxValue - 2) {
                throw new ArgumentOutOfRangeException("leaf");
            }
            nameBuffer = Marshal.StringToHGlobalUni(leaf);
            var unicode = new UnicodeString {
                Length = checked((ushort)nameBytes.Length),
                MaximumLength = checked((ushort)(nameBytes.Length + 2)),
                Buffer = nameBuffer
            };
            unicodeBuffer = Marshal.AllocHGlobal(
                Marshal.SizeOf(typeof(UnicodeString)));
            Marshal.StructureToPtr(unicode, unicodeBuffer, false);
            var attributes = new ObjectAttributes {
                Length = Marshal.SizeOf(typeof(ObjectAttributes)),
                RootDirectory = root.DangerousGetHandle(),
                ObjectName = unicodeBuffer,
                Attributes = ObjCaseInsensitive | ObjDontReparse
            };
            IntPtr rawHandle;
            IoStatusBlock statusBlock;
            uint access = FileReadAttributes | Synchronize;
            uint options = FileSynchronousIoNonAlert | FileOpenReparsePoint;
            if (directory) {
                access |= FileListDirectory | FileWriteData | FileTraverse;
                options |= FileDirectoryFile;
            } else {
                access |= FileWriteData | Delete;
                options |= FileNonDirectoryFile;
            }
            int status = NtCreateFile(
                out rawHandle,
                access,
                ref attributes,
                out statusBlock,
                IntPtr.Zero,
                directory ? FileAttributeDirectory : 0,
                FileShareRead,
                FileCreate,
                options,
                IntPtr.Zero,
                0);
            if (status < 0) {
                throw new Win32Exception(
                    unchecked((int)RtlNtStatusToDosError(status)),
                    "Creating an output entry relative to its root failed");
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

    internal static void RenameRelative(
        SafeFileHandle file,
        SafeFileHandle root,
        string leaf) {
        ValidateLeaf(leaf);
        byte[] name = Encoding.Unicode.GetBytes(leaf);
        int rootOffset = IntPtr.Size == 8 ? 8 : 4;
        int lengthOffset = checked(rootOffset + IntPtr.Size);
        int nameOffset = checked(lengthOffset + sizeof(uint));
        // FILE_RENAME_INFO declares FileName[1]; include that terminating
        // WCHAR even though FileNameLength excludes it.
        int size = checked(nameOffset + name.Length + sizeof(char));
        IntPtr buffer = Marshal.AllocHGlobal(size);
        try {
            for (int index = 0; index < size; index++) {
                Marshal.WriteByte(buffer, index, 0);
            }
            // Classic FILE_RENAME_INFO starts with ReplaceIfExists. Writing
            // the whole union word also initializes its alignment padding.
            Marshal.WriteInt32(buffer, 0, 1);
            Marshal.WriteIntPtr(
                buffer, rootOffset, root.DangerousGetHandle());
            Marshal.WriteInt32(buffer, lengthOffset, name.Length);
            Marshal.Copy(name, 0, IntPtr.Add(buffer, nameOffset), name.Length);
            IoStatusBlock statusBlock;
            int status = NtSetInformationFile(
                file,
                out statusBlock,
                buffer,
                (uint)size,
                FileRenameInformation);
            if (status < 0) {
                throw new Win32Exception(
                    unchecked((int)RtlNtStatusToDosError(status)),
                    "Atomically publishing the output entry failed");
            }
        } finally {
            Marshal.FreeHGlobal(buffer);
        }
    }

    internal static void DeleteOnClose(SafeFileHandle file) {
        IntPtr buffer = Marshal.AllocHGlobal(1);
        try {
            Marshal.WriteByte(buffer, 0, 1);
            SetFileInformationByHandle(
                file, FileDispositionInfo, buffer, 1);
        } finally {
            Marshal.FreeHGlobal(buffer);
        }
    }

    internal static ByHandleFileInformation GetInformation(
        SafeFileHandle handle) {
        ByHandleFileInformation information;
        if (!GetFileInformationByHandle(handle, out information)) {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "Inspecting a performance output handle failed");
        }
        return information;
    }

    internal static string GetFinalPath(SafeFileHandle handle) {
        int capacity = 512;
        while (capacity <= 32768) {
            var result = new StringBuilder(capacity);
            uint length = GetFinalPathNameByHandle(
                handle, result, (uint)result.Capacity, 0);
            if (length == 0) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Resolving a performance output handle failed");
            }
            if (length < result.Capacity) return result.ToString();
            capacity = checked((int)length + 1);
        }
        throw new InvalidDataException(
            "A performance output path exceeds the path bound");
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

    internal static string Identity(ByHandleFileInformation information) {
        return information.VolumeSerialNumber.ToString("x8") + ":" +
            information.FileIndexHigh.ToString("x8") +
            information.FileIndexLow.ToString("x8");
    }

    internal static bool SamePath(string left, string right) {
        return String.Equals(
            Path.GetFullPath(left).TrimEnd('\\', '/'),
            Path.GetFullPath(right).TrimEnd('\\', '/'),
            StringComparison.OrdinalIgnoreCase);
    }

    internal static void ValidateLeaf(string leaf) {
        if (
            String.IsNullOrEmpty(leaf) ||
            Path.GetFileName(leaf) != leaf ||
            leaf.IndexOfAny(new char[] {
                '\\', '/', ':', '\0'
            }) >= 0
        ) {
            throw new ArgumentException(
                "The performance output leaf name is invalid", "leaf");
        }
    }
}

public sealed class OutputRoot : IDisposable {
    private static readonly Regex PublicLeafPattern = new Regex(
        @"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$",
        RegexOptions.CultureInvariant);
    private SafeFileHandle handle;
    private readonly string rootPath;
    private readonly string identity;

    private OutputRoot(SafeFileHandle root, string path) {
        handle = root;
        rootPath = Path.GetFullPath(path).TrimEnd('\\', '/');
        var information = NativeMethods.GetInformation(handle);
        if (
            (information.FileAttributes &
                NativeMethods.FileAttributeDirectory) == 0 ||
            (information.FileAttributes &
                NativeMethods.FileAttributeReparsePoint) != 0
        ) {
            throw new InvalidDataException(
                "The performance output root is not an ordinary directory");
        }
        var finalPath = NativeMethods.ConvertFinalPath(
            NativeMethods.GetFinalPath(handle));
        if (!NativeMethods.SamePath(finalPath, rootPath)) {
            throw new InvalidDataException(
                "The performance output root aliases another directory");
        }
        identity = NativeMethods.Identity(information);
    }

    public string RootPath {
        get { return rootPath; }
    }

    public static OutputRoot Open(string path) {
        if (String.IsNullOrWhiteSpace(path)) {
            throw new ArgumentException("An output root is required", "path");
        }
        string fullPath = Path.GetFullPath(path);
        SafeFileHandle root = null;
        try {
            root = NativeMethods.OpenRoot(fullPath);
            var result = new OutputRoot(root, fullPath);
            root = null;
            return result;
        } finally {
            if (root != null) root.Dispose();
        }
    }

    public static OutputRoot CreateChild(
        string parentDirectory,
        string leaf) {
        ValidatePublicLeaf(leaf);
        using (var parent = Open(parentDirectory)) {
            SafeFileHandle child = null;
            try {
                child = NativeMethods.CreateRelative(
                    parent.handle, leaf, true);
                string path = Path.Combine(parent.rootPath, leaf);
                using (var created = new OutputRoot(child, path)) {
                    child = null;
                    // Reopen with the normal output-root access contract while
                    // the create handle still denies child replacement.
                    var result = Open(path);
                    created.Verify();
                    parent.Verify();
                    return result;
                }
            } finally {
                if (child != null) child.Dispose();
            }
        }
    }

    public void PublishUtf8(
        string leaf,
        string text,
        int maximumBytes) {
        AssertOpen();
        ValidatePublicLeaf(leaf);
        if (text == null) throw new ArgumentNullException("text");
        if (maximumBytes < 1 || maximumBytes > 256 * 1024 * 1024) {
            throw new ArgumentOutOfRangeException("maximumBytes");
        }
        byte[] bytes = new UTF8Encoding(false, true).GetBytes(text);
        try {
            if (bytes.Length > maximumBytes) {
                throw new InvalidDataException(
                    "The performance output exceeds its byte bound");
            }
            PublishBytes(leaf, bytes);
        } finally {
            Array.Clear(bytes, 0, bytes.Length);
        }
    }

    private void PublishBytes(string leaf, byte[] bytes) {
        Verify();
        string stageLeaf =
            "." + leaf + "." +
            Process.GetCurrentProcess().Id.ToString() + "." +
            Guid.NewGuid().ToString("N") + ".tmp";
        SafeFileHandle stage = null;
        FileStream stream = null;
        bool renamed = false;
        try {
            stage = NativeMethods.CreateRelative(handle, stageLeaf, false);
            stream = new FileStream(stage, FileAccess.Write, 65536, false);
            stage = null;
            stream.Write(bytes, 0, bytes.Length);
            stream.Flush(true);
            if (stream.Length != bytes.LongLength) {
                throw new IOException(
                    "The performance output write was incomplete");
            }
            Verify();
            NativeMethods.RenameRelative(
                stream.SafeFileHandle, handle, leaf);
            Verify();
            AssertPublishedFile(stream.SafeFileHandle, leaf);
            renamed = true;
        } finally {
            if (!renamed) {
                if (stream != null) {
                    NativeMethods.DeleteOnClose(stream.SafeFileHandle);
                } else if (stage != null) {
                    NativeMethods.DeleteOnClose(stage);
                }
            }
            if (stream != null) stream.Dispose();
            if (stage != null) stage.Dispose();
        }
    }

    private void AssertPublishedFile(
        SafeFileHandle file,
        string leaf) {
        var information = NativeMethods.GetInformation(file);
        if (
            (information.FileAttributes &
                NativeMethods.FileAttributeDirectory) != 0 ||
            (information.FileAttributes &
                NativeMethods.FileAttributeReparsePoint) != 0
        ) {
            throw new InvalidDataException(
                "The published output is not an ordinary file");
        }
        string expected = Path.Combine(rootPath, leaf);
        string finalPath = NativeMethods.ConvertFinalPath(
            NativeMethods.GetFinalPath(file));
        if (!NativeMethods.SamePath(finalPath, expected)) {
            throw new InvalidDataException(
                "The published output escaped its retained root");
        }
    }

    private static void ValidatePublicLeaf(string leaf) {
        NativeMethods.ValidateLeaf(leaf);
        if (!PublicLeafPattern.IsMatch(leaf)) {
            throw new ArgumentException(
                "The performance output leaf name is outside policy",
                "leaf");
        }
    }

    private void Verify() {
        AssertOpen();
        var information = NativeMethods.GetInformation(handle);
        if (
            (information.FileAttributes &
                NativeMethods.FileAttributeDirectory) == 0 ||
            (information.FileAttributes &
                NativeMethods.FileAttributeReparsePoint) != 0 ||
            !String.Equals(
                NativeMethods.Identity(information),
                identity,
                StringComparison.Ordinal)
        ) {
            throw new InvalidDataException(
                "The performance output root identity changed");
        }
        string finalPath = NativeMethods.ConvertFinalPath(
            NativeMethods.GetFinalPath(handle));
        if (!NativeMethods.SamePath(finalPath, rootPath)) {
            throw new InvalidDataException(
                "The performance output root moved unexpectedly");
        }
    }

    private void AssertOpen() {
        if (handle == null || handle.IsClosed || handle.IsInvalid) {
            throw new ObjectDisposedException("OutputRoot");
        }
    }

    public void Dispose() {
        if (handle != null) {
            handle.Dispose();
            handle = null;
        }
    }
}
}
'@
}

function Open-KettlePerfPersistenceRoot {
    param(
        [Parameter(Mandatory)]
        [string]$Directory
    )

    Initialize-KettlePerfPersistenceTypes
    return [KettlePerfPersistence.OutputRoot]::Open($Directory)
}

function New-KettlePerfPersistenceRoot {
    param(
        [Parameter(Mandatory)]
        [string]$ParentDirectory,
        [Parameter(Mandatory)]
        [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')]
        [string]$LeafName
    )

    Initialize-KettlePerfPersistenceTypes
    return [KettlePerfPersistence.OutputRoot]::CreateChild(
        $ParentDirectory,
        $LeafName
    )
}

function Close-KettlePerfPersistenceRoot {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This only closes a retained directory handle.'
    )]
    param(
        $Root
    )

    if ($null -ne $Root) {
        $Root.Dispose()
    }
}

function Write-KettlePerfUtf8File {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This is the explicit bounded persistence primitive.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string]$Text,
        [ValidateRange(1, 268435456)]
        [int]$MaximumBytes = 67108864,
        $Root = $null
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $directory = [IO.Path]::GetDirectoryName($fullPath)
    $leaf = [IO.Path]::GetFileName($fullPath)
    $ownedRoot = $null
    try {
        if ($null -eq $Root) {
            $ownedRoot = Open-KettlePerfPersistenceRoot `
                -Directory $directory
            $Root = $ownedRoot
        } elseif (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$Root.RootPath,
            $directory.TrimEnd([char[]]@('\', '/'))
        )) {
            throw 'Performance output is not a direct child of its retained root'
        }
        $Root.PublishUtf8($leaf, $Text, $MaximumBytes)
    } finally {
        Close-KettlePerfPersistenceRoot $ownedRoot
    }
}

function Write-KettlePerfJsonFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        $InputObject,
        [ValidateRange(1, 100)]
        [int]$Depth = 8,
        $Root = $null
    )

    $json = $InputObject | ConvertTo-Json -Depth $Depth
    Write-KettlePerfUtf8File -Path $Path -Text $json `
        -MaximumBytes 67108864 -Root $Root
}
