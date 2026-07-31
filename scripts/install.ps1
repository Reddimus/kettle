#requires -Version 5.1
<#
.SYNOPSIS
    kettle - Windows user-install (no admin / UAC required).

.DESCRIPTION
    Windows equivalent of `scripts/install.sh`. Drops
    everything into per-user paths so kettle shows up in Windows
    Search / Start menu - no system-wide changes, no admin.

      %LOCALAPPDATA%\Programs\kettle\kettle.exe            <- the binary
      %LOCALAPPDATA%\Programs\kettle\kettle.ico            <- icon
      %LOCALAPPDATA%\Programs\kettle\shell-integration\    <- OSC 133 snippets
      %APPDATA%\Microsoft\Windows\Start Menu\Programs\kettle.lnk
          ^ Start menu shortcut (so Win-key -> "kettle" finds it)
      HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle
          ^ Add/Remove Programs entry pointing back at this script

    Two layouts supported:
    - Extracted release .zip: `scripts/install.ps1` lives next to
      `kettle.exe`, ico, LICENSE, README, CHANGELOG, shell-integration/.
    - In-tree repo: this script at `scripts/install.ps1`; binary at
      `target/release/kettle.exe` (built by `cargo build --release -p
      kettle` or `just release`).

    User PATH update is on by default so `kettle.exe` is callable from
    any shell after a restart of that shell. Pass `-NoPath` to skip.

.PARAMETER Uninstall
    Reverse everything this script did. Removes the install dir, the
    Start menu shortcut, the Add/Remove Programs registry entry, and
    the PATH addition (only if the entry is exactly this install dir).

.PARAMETER NoPath
    Skip the user PATH update. kettle.exe will still be launchable
    from the Start menu shortcut, but you'll need the full
    `%LOCALAPPDATA%\Programs\kettle\kettle.exe` path from the shell.

.PARAMETER RefreshIntegration
    Refresh the managed Start menu shortcut and Add/Remove Programs metadata
    without copying files. Used internally after an authenticated self-update.

.PARAMETER Prefix
    Override the install location. Default: `%LOCALAPPDATA%\Programs\kettle`.
    For an isolated managed install on another fixed local volume, pass e.g.
    `-Prefix "D:\PortableApps\kettle"`. Network, removable, `SUBST`, and other
    non-volume DOS-device mappings are rejected. The target must be a dedicated
    directory named `kettle` and must be new/empty or an existing
    Kettle-managed install. The script doesn't write to the registry or PATH
    when Prefix is non-default (the assumption is a custom install means
    "no system traces").

.EXAMPLE
    .\install.ps1
    # Default install. Drops kettle into %LOCALAPPDATA%\Programs\kettle,
    # creates Start menu shortcut, adds to user PATH, registers in
    # Add/Remove Programs.

.EXAMPLE
    .\install.ps1 -Uninstall
    # Reverses everything.

.EXAMPLE
    .\install.ps1 -NoPath
    # Default install minus the PATH addition.

.NOTES
    Runs in user scope (HKCU + %LOCALAPPDATA%); no UAC prompt. The
    Start menu shortcut + Add/Remove Programs entry are per-user too,
    so a different Windows user on the same machine doesn't see your
    kettle install.
#>

[CmdletBinding()]
param(
    [switch] $Uninstall,
    [switch] $NoPath,
    [switch] $WithShellIntegration,
    [switch] $RefreshIntegration,
    [string] $Prefix = (Join-Path $env:LOCALAPPDATA "Programs\kettle"),
    [Parameter(DontShow = $true)]
    [string] $IntegrationTestRoot
)

$ErrorActionPreference = 'Stop'

if (-not ('KettleInstaller.NativeFileSystemV1' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Win32.SafeHandles;

namespace KettleInstaller
{
    public static class NativeFileSystemV1
    {
        private const uint GENERIC_READ = 0x80000000;
        private const uint GENERIC_WRITE = 0x40000000;
        private const uint DELETE = 0x00010000;
        private const uint READ_CONTROL = 0x00020000;
        private const uint WRITE_DAC = 0x00040000;
        private const uint FILE_READ_ATTRIBUTES = 0x00000080;
        private const uint FILE_SHARE_READ = 0x00000001;
        private const uint FILE_SHARE_WRITE = 0x00000002;
        private const uint FILE_SHARE_DELETE = 0x00000004;
        private const uint CREATE_NEW = 1;
        private const uint OPEN_EXISTING = 3;
        private const uint OPEN_ALWAYS = 4;
        private const uint FILE_ATTRIBUTE_NORMAL = 0x00000080;
        private const uint FILE_ATTRIBUTE_DIRECTORY = 0x00000010;
        private const uint FILE_ATTRIBUTE_REPARSE_POINT = 0x00000400;
        private const uint FILE_ATTRIBUTE_SPARSE_FILE = 0x00000200;
        private const uint FILE_ATTRIBUTE_COMPRESSED = 0x00000800;
        private const uint FILE_ATTRIBUTE_OFFLINE = 0x00001000;
        private const uint FILE_ATTRIBUTE_ENCRYPTED = 0x00004000;
        private const uint FILE_ATTRIBUTE_INTEGRITY_STREAM = 0x00008000;
        private const uint FILE_ATTRIBUTE_NO_SCRUB_DATA = 0x00020000;
        private const uint FILE_FLAG_BACKUP_SEMANTICS = 0x02000000;
        private const uint FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000;
        private const uint FILE_FLAG_SEQUENTIAL_SCAN = 0x08000000;
        private const uint MOVEFILE_REPLACE_EXISTING = 0x00000001;
        private const uint MOVEFILE_WRITE_THROUGH = 0x00000008;
        private const uint LOCKFILE_EXCLUSIVE_LOCK = 0x00000002;
        private const int ERROR_FILE_NOT_FOUND = 2;
        private const int ERROR_PATH_NOT_FOUND = 3;
        private const int ERROR_HANDLE_EOF = 38;
        private const int ERROR_INVALID_PARAMETER = 87;
        private const int ERROR_INSUFFICIENT_BUFFER = 122;
        private const int ERROR_ALREADY_EXISTS = 183;
        private const int OWNER_SECURITY_INFORMATION = 0x00000001;
        private const int DACL_SECURITY_INFORMATION = 0x00000004;
        private const int FILE_BASIC_INFORMATION = 0;
        private const int FILE_DISPOSITION_INFORMATION = 4;
        private const int FILE_RENAME_INFORMATION_EX = 22;
        private const int FILE_RENAME_FLAG_REPLACE_IF_EXISTS = 0x00000001;
        private const int FILE_RENAME_FLAG_POSIX_SEMANTICS = 0x00000002;
        private const int FILE_RENAME_FLAG_IGNORE_READONLY_ATTRIBUTE =
            0x00000040;
        private const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x00001000;
        private const uint DRIVE_FIXED = 3;

        [StructLayout(LayoutKind.Sequential)]
        internal struct ByHandleFileInformation
        {
            internal uint FileAttributes;
            internal System.Runtime.InteropServices.ComTypes.FILETIME CreationTime;
            internal System.Runtime.InteropServices.ComTypes.FILETIME LastAccessTime;
            internal System.Runtime.InteropServices.ComTypes.FILETIME LastWriteTime;
            internal uint VolumeSerialNumber;
            internal uint FileSizeHigh;
            internal uint FileSizeLow;
            internal uint NumberOfLinks;
            internal uint FileIndexHigh;
            internal uint FileIndexLow;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct Overlapped
        {
            internal UIntPtr Internal;
            internal UIntPtr InternalHigh;
            internal uint Offset;
            internal uint OffsetHigh;
            internal IntPtr Event;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern SafeFileHandle ReOpenFile(
            SafeFileHandle originalFile,
            uint desiredAccess,
            uint shareMode,
            uint flagsAndAttributes);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetFileInformationByHandle(
            SafeFileHandle file,
            out ByHandleFileInformation information);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool ReadFile(
            SafeFileHandle file,
            byte[] buffer,
            int bytesToRead,
            out int bytesRead,
            IntPtr overlapped);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool WriteFile(
            SafeFileHandle file,
            byte[] buffer,
            int bytesToWrite,
            out int bytesWritten,
            IntPtr overlapped);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool FlushFileBuffers(SafeFileHandle file);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetFilePointerEx(
            SafeFileHandle file,
            long distance,
            out long newPosition,
            uint moveMethod);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool LockFileEx(
            SafeFileHandle file,
            uint flags,
            uint reserved,
            uint bytesLow,
            uint bytesHigh,
            ref Overlapped overlapped);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool UnlockFile(
            SafeFileHandle file,
            uint offsetLow,
            uint offsetHigh,
            uint bytesLow,
            uint bytesHigh);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool DeleteFileW(string fileName);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool RemoveDirectoryW(string pathName);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CreateDirectoryW(
            string pathName,
            IntPtr securityAttributes);

        [DllImport("kernel32.dll")]
        private static extern IntPtr LocalFree(IntPtr memory);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool TerminateProcess(
            IntPtr process,
            uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr OpenProcess(
            uint desiredAccess,
            bool inheritHandle,
            uint processId);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string stringSecurityDescriptor,
            uint stringSecurityDescriptorRevision,
            out IntPtr securityDescriptor,
            out uint securityDescriptorSize);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool MoveFileExW(
            string existingFileName,
            string newFileName,
            uint flags);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetFileInformationByHandle(
            SafeFileHandle file,
            int fileInformationClass,
            IntPtr fileInformation,
            uint bufferSize);

        [DllImport(
            "kernel32.dll",
            EntryPoint = "SetFileInformationByHandle",
            SetLastError = true)]
        private static extern bool SetFileBasicInformationByHandle(
            SafeFileHandle file,
            int fileInformationClass,
            ref FileBasicInformation fileInformation,
            uint bufferSize);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetFileTime(
            SafeFileHandle file,
            ref System.Runtime.InteropServices.ComTypes.FILETIME creation,
            ref System.Runtime.InteropServices.ComTypes.FILETIME access,
            ref System.Runtime.InteropServices.ComTypes.FILETIME write);

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool GetKernelObjectSecurity(
            IntPtr handle,
            int requestedInformation,
            byte[] securityDescriptor,
            uint length,
            out uint needed);

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct Win32FindStreamData
        {
            internal long StreamSize;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 296)]
            internal string StreamName;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct FileRenameInformation
        {
            internal int ReplaceIfExists;
            internal IntPtr RootDirectory;
            internal uint FileNameLength;
            internal char FileName;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct FileBasicInformation
        {
            internal long CreationTime;
            internal long LastAccessTime;
            internal long LastWriteTime;
            internal long ChangeTime;
            internal uint FileAttributes;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct SecurityAttributes
        {
            internal int Length;
            internal IntPtr SecurityDescriptor;
            internal int InheritHandle;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr FindFirstStreamW(
            string fileName,
            int infoLevel,
            out Win32FindStreamData findStreamData,
            uint flags);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool FindNextStreamW(
            IntPtr findStream,
            out Win32FindStreamData findStreamData);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool FindClose(IntPtr findFile);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
        private static extern uint GetDriveTypeW(string rootPathName);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint QueryDosDeviceW(
            string deviceName,
            StringBuilder targetPath,
            int maximum);

        private static SafeFileHandle OpenNoFollow(
            string path,
            uint access,
            uint share,
            uint flags)
        {
            SafeFileHandle handle = CreateFileW(
                path,
                access,
                share,
                IntPtr.Zero,
                OPEN_EXISTING,
                flags | FILE_FLAG_OPEN_REPARSE_POINT,
                IntPtr.Zero);
            if (handle.IsInvalid)
            {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(error, "Cannot safely open " + path);
            }
            return handle;
        }

        private static void TerminateProbe(Process process)
        {
            try
            {
                if (!process.HasExited)
                {
                    process.Kill();
                }
            }
            catch
            {
                // The bounded probe reports its original timeout/output error.
            }
            try
            {
                process.WaitForExit(5000);
            }
            catch
            {
                // Disposal below still closes the retained process handles.
            }
        }

        public static string ProbeExecutableVersion(
            string executable,
            int maximumCharacters,
            int timeoutMilliseconds)
        {
            if (String.IsNullOrWhiteSpace(executable) ||
                maximumCharacters < 1 ||
                maximumCharacters > 65536 ||
                timeoutMilliseconds < 1 ||
                timeoutMilliseconds > 60000)
            {
                throw new ArgumentOutOfRangeException(
                    "Invalid bounded executable-version probe parameters.");
            }

            ProcessStartInfo start = new ProcessStartInfo();
            start.FileName = executable;
            start.Arguments = "--version";
            start.UseShellExecute = false;
            start.CreateNoWindow = true;
            start.RedirectStandardOutput = true;
            start.RedirectStandardError = true;

            using (Process process = new Process())
            {
                process.StartInfo = start;
                if (!process.Start())
                {
                    throw new IOException(
                        "The installed version probe did not start.");
                }

                char[] standardOutput = new char[maximumCharacters + 1];
                char[] standardError = new char[maximumCharacters + 1];
                Task<int> outputRead = process.StandardOutput.ReadBlockAsync(
                    standardOutput,
                    0,
                    standardOutput.Length);
                Task<int> errorRead = process.StandardError.ReadBlockAsync(
                    standardError,
                    0,
                    standardError.Length);
                Stopwatch stopwatch = Stopwatch.StartNew();

                while (!outputRead.IsCompleted || !errorRead.IsCompleted)
                {
                    if (outputRead.IsFaulted || errorRead.IsFaulted)
                    {
                        TerminateProbe(process);
                        throw new IOException(
                            "The installed version probe output could not be read.",
                            outputRead.Exception ?? errorRead.Exception);
                    }
                    if (
                        (outputRead.IsCompleted &&
                            outputRead.GetAwaiter().GetResult() >
                                maximumCharacters) ||
                        (errorRead.IsCompleted &&
                            errorRead.GetAwaiter().GetResult() >
                                maximumCharacters))
                    {
                        TerminateProbe(process);
                        throw new IOException(
                            "The installed version probe exceeded its output limit.");
                    }
                    if (stopwatch.ElapsedMilliseconds >= timeoutMilliseconds)
                    {
                        TerminateProbe(process);
                        throw new TimeoutException(
                            "The installed version probe exceeded its time limit.");
                    }
                    Thread.Sleep(10);
                }

                int outputLength = outputRead.GetAwaiter().GetResult();
                int errorLength = errorRead.GetAwaiter().GetResult();
                if (outputLength + errorLength > maximumCharacters)
                {
                    TerminateProbe(process);
                    throw new IOException(
                        "The installed version probe exceeded its output limit.");
                }

                int remaining = timeoutMilliseconds -
                    checked((int)stopwatch.ElapsedMilliseconds);
                if (remaining <= 0 || !process.WaitForExit(remaining))
                {
                    TerminateProbe(process);
                    throw new TimeoutException(
                        "The installed version probe exceeded its time limit.");
                }
                if (process.ExitCode != 0)
                {
                    throw new IOException(
                        "The installed version probe returned a failure status.");
                }
                return new String(standardOutput, 0, outputLength);
            }
        }

        private static ByHandleFileInformation Information(
            SafeFileHandle handle,
            string path)
        {
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot inspect " + path);
            }
            return information;
        }

        private static string FixedDriveTarget(string path)
        {
            string full = Path.GetFullPath(path);
            string root = Path.GetPathRoot(full);
            if (String.IsNullOrEmpty(root) ||
                root.StartsWith("\\\\", StringComparison.Ordinal) ||
                root.Length < 2 ||
                root[1] != ':' ||
                GetDriveTypeW(root) != DRIVE_FIXED)
            {
                throw new IOException(
                    "Install and profile paths must be on a local fixed drive: " +
                    path);
            }
            string drive = root.Substring(0, 2);
            StringBuilder target = new StringBuilder(32768);
            if (QueryDosDeviceW(drive, target, target.Capacity) == 0)
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot resolve the drive mapping " + drive);
            }
            string value = target.ToString();
            if (!value.StartsWith(
                "\\Device\\HarddiskVolume",
                StringComparison.OrdinalIgnoreCase))
            {
                throw new IOException(
                    "SUBST, remote, and non-volume drive mappings are not supported: " +
                    drive);
            }
            return value;
        }

        public static void ValidateFixedDrive(string path)
        {
            FixedDriveTarget(path);
        }

        public sealed class DirectoryChainGuard : IDisposable
        {
            private SafeFileHandle[] handles;
            private string driveName;
            private string driveTarget;

            internal DirectoryChainGuard(
                SafeFileHandle[] handles,
                string driveName,
                string driveTarget)
            {
                this.handles = handles;
                this.driveName = driveName;
                this.driveTarget = driveTarget;
            }

            public void Verify()
            {
                string current = FixedDriveTarget(driveName);
                if (!String.Equals(
                    current,
                    driveTarget,
                    StringComparison.OrdinalIgnoreCase))
                {
                    throw new IOException(
                        "The drive mapping changed while installer state was retained.");
                }
            }

            public void Dispose()
            {
                SafeFileHandle[] owned = handles;
                handles = null;
                if (owned == null)
                {
                    return;
                }
                for (int index = owned.Length - 1; index >= 0; index--)
                {
                    owned[index].Dispose();
                }
            }
        }

        public sealed class InstallerFileLock : IDisposable
        {
            private SafeFileHandle handle;

            internal InstallerFileLock(SafeFileHandle handle)
            {
                this.handle = handle;
            }

            public void Dispose()
            {
                SafeFileHandle owned = handle;
                handle = null;
                if (owned == null)
                {
                    return;
                }
                try
                {
                    UnlockFile(owned, 0, 0, UInt32.MaxValue, UInt32.MaxValue);
                }
                finally
                {
                    owned.Dispose();
                }
            }
        }

        public sealed class ProfileSnapshot : IDisposable
        {
            internal SafeFileHandle Handle;
            internal ByHandleFileInformation Information;
            internal byte[] SecurityDescriptor;
            internal string Path;

            public byte[] Bytes { get; private set; }
            public bool Exists { get { return Handle != null; } }

            internal ProfileSnapshot(
                string path,
                SafeFileHandle handle,
                ByHandleFileInformation information,
                byte[] bytes,
                byte[] securityDescriptor)
            {
                Path = path;
                Handle = handle;
                Information = information;
                Bytes = bytes;
                SecurityDescriptor = securityDescriptor;
            }

            public void Dispose()
            {
                SafeFileHandle owned = Handle;
                Handle = null;
                if (owned != null)
                {
                    owned.Dispose();
                }
            }
        }

        public static SafeFileHandle LockRealDirectory(string path)
        {
            SafeFileHandle handle = OpenNoFollow(
                path,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_FLAG_BACKUP_SEMANTICS);
            try
            {
                ByHandleFileInformation information = Information(handle, path);
                if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 ||
                    (information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    throw new IOException(
                        "Install path component is not a real directory or is " +
                        "a reparse point: " + path);
                }
                return handle;
            }
            catch
            {
                handle.Dispose();
                throw;
            }
        }

        private static SecurityIdentifier CurrentUserSid()
        {
            using (WindowsIdentity identity = WindowsIdentity.GetCurrent())
            {
                if (identity.User == null)
                {
                    throw new IOException(
                        "The installer process has no Windows user SID.");
                }
                return identity.User;
            }
        }

        private static SecurityIdentifier CurrentTokenOwnerSid()
        {
            using (WindowsIdentity identity = WindowsIdentity.GetCurrent())
            {
                if (identity.Owner == null)
                {
                    throw new IOException(
                        "The installer process has no Windows token owner SID.");
                }
                return identity.Owner;
            }
        }

        private static SecurityIdentifier[] PrivateDirectoryTrustees()
        {
            SecurityIdentifier current = CurrentUserSid();
            SecurityIdentifier system = new SecurityIdentifier(
                WellKnownSidType.LocalSystemSid,
                null);
            SecurityIdentifier administrators = new SecurityIdentifier(
                WellKnownSidType.BuiltinAdministratorsSid,
                null);
            System.Collections.Generic.List<SecurityIdentifier> result =
                new System.Collections.Generic.List<SecurityIdentifier>();
            foreach (SecurityIdentifier candidate in new SecurityIdentifier[] {
                current,
                system,
                administrators
            })
            {
                bool duplicate = false;
                foreach (SecurityIdentifier existing in result)
                {
                    if (existing.Equals(candidate))
                    {
                        duplicate = true;
                        break;
                    }
                }
                if (!duplicate)
                {
                    result.Add(candidate);
                }
            }
            return result.ToArray();
        }

        private static byte[] ReadSecurityDescriptor(
            SafeFileHandle handle,
            int requestedInformation,
            string path)
        {
            uint needed;
            GetKernelObjectSecurity(
                handle.DangerousGetHandle(),
                requestedInformation,
                null,
                0,
                out needed);
            int error = Marshal.GetLastWin32Error();
            if (needed == 0 || error != ERROR_INSUFFICIENT_BUFFER)
            {
                throw new Win32Exception(
                    error,
                    "Cannot size security information for " + path);
            }
            byte[] descriptor = new byte[needed];
            if (!GetKernelObjectSecurity(
                handle.DangerousGetHandle(),
                requestedInformation,
                descriptor,
                needed,
                out needed))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot read security information for " + path);
            }
            return descriptor;
        }

        private static void RequirePrivateDirectorySecurity(
            SafeFileHandle handle,
            string path)
        {
            RawSecurityDescriptor descriptor = new RawSecurityDescriptor(
                ReadSecurityDescriptor(
                    handle,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    path),
                0);
            SecurityIdentifier current = CurrentUserSid();
            if (descriptor.Owner == null ||
                !descriptor.Owner.Equals(current) ||
                (descriptor.ControlFlags &
                    ControlFlags.DiscretionaryAclPresent) == 0 ||
                (descriptor.ControlFlags &
                    ControlFlags.DiscretionaryAclProtected) == 0 ||
                descriptor.DiscretionaryAcl == null)
            {
                throw new UnauthorizedAccessException(
                    "The private installer transaction directory has an " +
                    "untrusted owner or access-control list: " + path);
            }

            System.Collections.Generic.List<SecurityIdentifier> remaining =
                new System.Collections.Generic.List<SecurityIdentifier>(
                    PrivateDirectoryTrustees());
            const int fullControl = 0x001F01FF;
            RawAcl dacl = descriptor.DiscretionaryAcl;
            for (int index = 0; index < dacl.Count; index++)
            {
                CommonAce ace = dacl[index] as CommonAce;
                if (ace == null ||
                    ace.IsCallback ||
                    ace.AceQualifier != AceQualifier.AccessAllowed ||
                    ace.AccessMask != fullControl ||
                    ace.AceFlags != (
                        AceFlags.ContainerInherit |
                        AceFlags.ObjectInherit))
                {
                    throw new UnauthorizedAccessException(
                        "The private installer transaction directory has " +
                        "an unexpected access-control entry: " + path);
                }
                int trustee = -1;
                for (int expected = 0; expected < remaining.Count; expected++)
                {
                    if (remaining[expected].Equals(ace.SecurityIdentifier))
                    {
                        trustee = expected;
                        break;
                    }
                }
                if (trustee < 0)
                {
                    throw new UnauthorizedAccessException(
                        "The private installer transaction directory grants " +
                        "access to an unexpected principal: " + path);
                }
                remaining.RemoveAt(trustee);
            }
            if (remaining.Count != 0)
            {
                throw new UnauthorizedAccessException(
                    "The private installer transaction directory is missing " +
                    "a required access-control entry: " + path);
            }
        }

        private static void RequireCurrentTokenOwner(
            SafeFileHandle handle,
            string path)
        {
            RawSecurityDescriptor descriptor = new RawSecurityDescriptor(
                ReadSecurityDescriptor(
                    handle,
                    OWNER_SECURITY_INFORMATION,
                    path),
                0);
            if (descriptor.Owner == null ||
                !descriptor.Owner.Equals(CurrentTokenOwnerSid()))
            {
                throw new UnauthorizedAccessException(
                    "Installer temporary file has an untrusted owner: " +
                    path);
            }
        }

        public static SafeFileHandle LockPrivateDirectory(string path)
        {
            SafeFileHandle handle = OpenNoFollow(
                path,
                FILE_READ_ATTRIBUTES | READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_FLAG_BACKUP_SEMANTICS);
            try
            {
                ByHandleFileInformation information = Information(handle, path);
                if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 ||
                    (information.FileAttributes &
                        FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    throw new IOException(
                        "Private installer transaction path is not a real " +
                        "directory: " + path);
                }
                RequirePrivateDirectorySecurity(handle, path);
                return handle;
            }
            catch
            {
                handle.Dispose();
                throw;
            }
        }

        public static void CreatePrivateDirectory(string path)
        {
            SecurityIdentifier current = CurrentUserSid();
            StringBuilder sddl = new StringBuilder();
            sddl.Append("O:");
            sddl.Append(current.Value);
            sddl.Append("D:P");
            foreach (SecurityIdentifier trustee in PrivateDirectoryTrustees())
            {
                sddl.Append("(A;OICI;FA;;;");
                sddl.Append(trustee.Value);
                sddl.Append(")");
            }

            IntPtr descriptor = IntPtr.Zero;
            IntPtr securityAttributes = IntPtr.Zero;
            try
            {
                uint descriptorSize;
                if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.ToString(),
                    1,
                    out descriptor,
                    out descriptorSize))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot create the private installer security descriptor");
                }
                SecurityAttributes attributes = new SecurityAttributes();
                attributes.Length = Marshal.SizeOf(
                    typeof(SecurityAttributes));
                attributes.SecurityDescriptor = descriptor;
                attributes.InheritHandle = 0;
                securityAttributes = Marshal.AllocHGlobal(attributes.Length);
                Marshal.StructureToPtr(
                    attributes,
                    securityAttributes,
                    false);
                if (!CreateDirectoryW(path, securityAttributes))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot create the private installer transaction directory");
                }
            }
            finally
            {
                if (securityAttributes != IntPtr.Zero)
                {
                    Marshal.FreeHGlobal(securityAttributes);
                }
                if (descriptor != IntPtr.Zero)
                {
                    LocalFree(descriptor);
                }
            }
        }

        public static DirectoryChainGuard LockRealDirectoryChain(string path)
        {
            string full = Path.GetFullPath(path);
            string root = Path.GetPathRoot(full);
            if (String.IsNullOrEmpty(root))
            {
                throw new IOException(
                    "Install directory chain has no filesystem root: " + path);
            }
            if (full.Length > root.Length)
            {
                full = full.TrimEnd(
                    Path.DirectorySeparatorChar,
                    Path.AltDirectorySeparatorChar);
            }
            string[] relative = full.Substring(root.Length).Split(
                new char[] {
                    Path.DirectorySeparatorChar,
                    Path.AltDirectorySeparatorChar
                },
                StringSplitOptions.RemoveEmptyEntries);
            SafeFileHandle[] handles =
                new SafeFileHandle[relative.Length + 1];
            int opened = 0;
            try
            {
                string driveName = root.Substring(0, 2);
                string driveTarget = FixedDriveTarget(full);
                string current = root;
                handles[opened++] = LockRealDirectory(current);
                foreach (string component in relative)
                {
                    current = Path.Combine(current, component);
                    handles[opened++] = LockRealDirectory(current);
                }
                DirectoryChainGuard guard = new DirectoryChainGuard(
                    handles,
                    driveName,
                    driveTarget);
                guard.Verify();
                return guard;
            }
            catch
            {
                for (int index = opened - 1; index >= 0; index--)
                {
                    handles[index].Dispose();
                }
                throw;
            }
        }

        public static InstallerFileLock AcquireExclusiveFileLock(string path)
        {
            SafeFileHandle handle = CreateFileW(
                path,
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                IntPtr.Zero,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                IntPtr.Zero);
            if (handle.IsInvalid)
            {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(error, "Cannot open lock file " + path);
            }
            try
            {
                ByHandleFileInformation information = Information(handle, path);
                if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 ||
                    (information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
                    information.NumberOfLinks != 1)
                {
                    throw new IOException(
                        "Installer lock path is not a single-link ordinary file: " +
                        path);
                }
                Overlapped overlapped = new Overlapped();
                if (!LockFileEx(
                    handle,
                    LOCKFILE_EXCLUSIVE_LOCK,
                    0,
                    UInt32.MaxValue,
                    UInt32.MaxValue,
                    ref overlapped))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot acquire installer lock " + path);
                }
                return new InstallerFileLock(handle);
            }
            catch
            {
                handle.Dispose();
                throw;
            }
        }

        public static byte[] ReadBoundedRegularFile(string path, int maximumBytes)
        {
            SafeFileHandle handle = OpenNoFollow(
                path,
                GENERIC_READ,
                FILE_SHARE_READ,
                FILE_FLAG_SEQUENTIAL_SCAN);
            try
            {
                ByHandleFileInformation information = Information(handle, path);
                if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 ||
                    (information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    throw new IOException("Refusing to read a non-regular file: " + path);
                }
                ulong length =
                    ((ulong)information.FileSizeHigh << 32) | information.FileSizeLow;
                if (length > (ulong)maximumBytes)
                {
                    throw new IOException(
                        "File exceeds its safety limit: " + path);
                }
                using (FileStream input = new FileStream(handle, FileAccess.Read))
                {
                    handle = null;
                    byte[] result = new byte[(int)length];
                    int offset = 0;
                    while (offset < result.Length)
                    {
                        int read = input.Read(result, offset, result.Length - offset);
                        if (read == 0)
                        {
                            throw new EndOfStreamException(
                                "File changed while it was being read: " + path);
                        }
                        offset += read;
                    }
                    if (input.ReadByte() != -1)
                    {
                        throw new IOException(
                            "File grew while it was being read: " + path);
                    }
                    return result;
                }
            }
            finally
            {
                if (handle != null)
                {
                    handle.Dispose();
                }
            }
        }

        private static void RejectAlternateStreams(string path)
        {
            Win32FindStreamData data;
            IntPtr search = FindFirstStreamW(path, 0, out data, 0);
            if (search == new IntPtr(-1))
            {
                int error = Marshal.GetLastWin32Error();
                if (error == ERROR_HANDLE_EOF ||
                    error == ERROR_FILE_NOT_FOUND ||
                    error == ERROR_PATH_NOT_FOUND)
                {
                    return;
                }
                throw new Win32Exception(
                    error,
                    "Cannot enumerate profile streams " + path);
            }
            try
            {
                do
                {
                    if (!String.Equals(
                        data.StreamName,
                        "::$DATA",
                        StringComparison.Ordinal))
                    {
                        throw new IOException(
                            "PowerShell profile has an unsupported alternate stream: " +
                            data.StreamName);
                    }
                }
                while (FindNextStreamW(search, out data));
                int error = Marshal.GetLastWin32Error();
                if (error != ERROR_HANDLE_EOF)
                {
                    throw new Win32Exception(
                        error,
                        "Cannot finish enumerating profile streams " + path);
                }
            }
            finally
            {
                FindClose(search);
            }
        }

        private static byte[] ReadDacl(SafeFileHandle handle, string path)
        {
            uint needed;
            GetKernelObjectSecurity(
                handle.DangerousGetHandle(),
                DACL_SECURITY_INFORMATION,
                null,
                0,
                out needed);
            int error = Marshal.GetLastWin32Error();
            if (needed == 0 || error != ERROR_INSUFFICIENT_BUFFER)
            {
                throw new Win32Exception(
                    error,
                    "Cannot size the PowerShell profile DACL " + path);
            }
            byte[] descriptor = new byte[needed];
            if (!GetKernelObjectSecurity(
                handle.DangerousGetHandle(),
                DACL_SECURITY_INFORMATION,
                descriptor,
                needed,
                out needed))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot read the PowerShell profile DACL " + path);
            }
            return descriptor;
        }

        private static byte[] ReadHeldBytes(
            SafeFileHandle handle,
            ByHandleFileInformation information,
            int maximumBytes,
            string path)
        {
            ulong length =
                ((ulong)information.FileSizeHigh << 32) |
                information.FileSizeLow;
            if (length > (ulong)maximumBytes || length > Int32.MaxValue)
            {
                throw new IOException(
                    "PowerShell profile exceeds its safety limit: " + path);
            }
            long position;
            if (!SetFilePointerEx(handle, 0, out position, 0))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot rewind the PowerShell profile " + path);
            }
            byte[] bytes = new byte[(int)length];
            int offset = 0;
            while (offset < bytes.Length)
            {
                byte[] chunk = new byte[Math.Min(65536, bytes.Length - offset)];
                int read;
                if (!ReadFile(
                    handle,
                    chunk,
                    chunk.Length,
                    out read,
                    IntPtr.Zero))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot read the retained PowerShell profile " + path);
                }
                if (read == 0)
                {
                    throw new EndOfStreamException(
                        "PowerShell profile changed while retained: " + path);
                }
                Buffer.BlockCopy(chunk, 0, bytes, offset, read);
                offset += read;
            }
            byte[] probe = new byte[1];
            int extra;
            if (!ReadFile(handle, probe, 1, out extra, IntPtr.Zero))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot finish reading the PowerShell profile " + path);
            }
            if (extra != 0)
            {
                throw new IOException(
                    "PowerShell profile grew while retained: " + path);
            }
            return bytes;
        }

        public static ProfileSnapshot CaptureProfile(
            string path,
            int maximumBytes)
        {
            SafeFileHandle handle = CreateFileW(
                path,
                GENERIC_READ | READ_CONTROL,
                FILE_SHARE_READ,
                IntPtr.Zero,
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN,
                IntPtr.Zero);
            if (handle.IsInvalid)
            {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                if (error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND)
                {
                    return new ProfileSnapshot(
                        path,
                        null,
                        new ByHandleFileInformation(),
                        new byte[0],
                        null);
                }
                throw new Win32Exception(
                    error,
                    "Cannot retain the PowerShell profile " + path);
            }
            try
            {
                ByHandleFileInformation information = Information(handle, path);
                const uint unsupported =
                    FILE_ATTRIBUTE_DIRECTORY |
                    FILE_ATTRIBUTE_REPARSE_POINT |
                    FILE_ATTRIBUTE_SPARSE_FILE |
                    FILE_ATTRIBUTE_COMPRESSED |
                    FILE_ATTRIBUTE_OFFLINE |
                    FILE_ATTRIBUTE_ENCRYPTED |
                    FILE_ATTRIBUTE_INTEGRITY_STREAM |
                    FILE_ATTRIBUTE_NO_SCRUB_DATA |
                    0x00000100 | // temporary
                    0x00010000;  // virtual
                if ((information.FileAttributes & unsupported) != 0 ||
                    information.NumberOfLinks != 1)
                {
                    throw new IOException(
                        "PowerShell profile uses EFS, links, reparse points, " +
                        "or unsupported file attributes: " + path);
                }
                ulong length =
                    ((ulong)information.FileSizeHigh << 32) |
                    information.FileSizeLow;
                if (length > (ulong)maximumBytes)
                {
                    throw new IOException(
                        "PowerShell profile exceeds its safety limit: " + path);
                }
                RejectAlternateStreams(path);
                byte[] bytes = ReadHeldBytes(
                    handle,
                    information,
                    maximumBytes,
                    path);
                byte[] descriptor = ReadDacl(handle, path);
                return new ProfileSnapshot(
                    path,
                    handle,
                    information,
                    bytes,
                    descriptor);
            }
            catch
            {
                handle.Dispose();
                throw;
            }
        }

        private static bool RenameHeldFileAtomic(
            SafeFileHandle handle,
            string destination,
            bool replace,
            out int error)
        {
            byte[] name = System.Text.Encoding.Unicode.GetBytes(destination);
            int nameOffset = (int)Marshal.OffsetOf(
                typeof(FileRenameInformation),
                "FileName");
            int total = checked(
                Marshal.SizeOf(typeof(FileRenameInformation)) + name.Length);
            IntPtr buffer = Marshal.AllocHGlobal(total);
            try
            {
                for (int index = 0; index < total; index++)
                {
                    Marshal.WriteByte(buffer, index, 0);
                }
                int flags = replace
                    ? FILE_RENAME_FLAG_REPLACE_IF_EXISTS |
                        FILE_RENAME_FLAG_POSIX_SEMANTICS |
                        FILE_RENAME_FLAG_IGNORE_READONLY_ATTRIBUTE
                    : 0;
                Marshal.WriteInt32(buffer, 0, flags);
                Marshal.WriteIntPtr(
                    buffer,
                    (int)Marshal.OffsetOf(
                        typeof(FileRenameInformation),
                        "RootDirectory"),
                    IntPtr.Zero);
                Marshal.WriteInt32(
                    buffer,
                    (int)Marshal.OffsetOf(
                        typeof(FileRenameInformation),
                        "FileNameLength"),
                    name.Length);
                Marshal.Copy(name, 0, new IntPtr(buffer.ToInt64() + nameOffset), name.Length);
                if (!SetFileInformationByHandle(
                    handle,
                    FILE_RENAME_INFORMATION_EX,
                    buffer,
                    (uint)total))
                {
                    error = Marshal.GetLastWin32Error();
                    return false;
                }
                error = 0;
                return true;
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static void PrepareProfileReplacementHold(
            ProfileSnapshot snapshot)
        {
            SafeFileHandle replacementHold = ReOpenFile(
                snapshot.Handle,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_FLAG_OPEN_REPARSE_POINT);
            if (replacementHold.IsInvalid)
            {
                int error = Marshal.GetLastWin32Error();
                replacementHold.Dispose();
                throw new Win32Exception(
                    error,
                    "Cannot prepare the retained PowerShell profile for atomic replacement");
            }
            SafeFileHandle original = snapshot.Handle;
            snapshot.Handle = replacementHold;
            original.Dispose();
        }

        private static void WriteHeldBytes(
            SafeFileHandle handle,
            byte[] bytes,
            string path)
        {
            int offset = 0;
            while (offset < bytes.Length)
            {
                int length = Math.Min(65536, bytes.Length - offset);
                byte[] chunk = new byte[length];
                Buffer.BlockCopy(bytes, offset, chunk, 0, length);
                int written;
                if (!WriteFile(
                    handle,
                    chunk,
                    chunk.Length,
                    out written,
                    IntPtr.Zero))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot write the staged PowerShell profile " + path);
                }
                if (written < 1 || written > chunk.Length)
                {
                    throw new EndOfStreamException(
                        "The staged PowerShell profile write made no progress: " +
                        path);
                }
                offset += written;
            }
            if (!FlushFileBuffers(handle))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot flush the staged PowerShell profile " + path);
            }
        }

        private static void SetHeldFileAttributes(
            SafeFileHandle handle,
            uint attributes,
            string path)
        {
            FileBasicInformation information = new FileBasicInformation();
            information.FileAttributes = attributes;
            if (!SetFileBasicInformationByHandle(
                handle,
                FILE_BASIC_INFORMATION,
                ref information,
                (uint)Marshal.SizeOf(typeof(FileBasicInformation))))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Cannot preserve PowerShell profile attributes " + path);
            }
        }

        public static void HardTerminateInstallerForTesting()
        {
            using (Process process = Process.GetCurrentProcess())
            {
                if (!TerminateProcess(process.Handle, 197))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot trigger the installer hard-kill test checkpoint");
                }
            }
            Thread.Sleep(Timeout.Infinite);
        }

        public static bool PublishProfile(
            ProfileSnapshot snapshot,
            byte[] bytes,
            bool failBeforeAtomicReplace)
        {
            if (snapshot == null)
            {
                throw new ArgumentNullException("snapshot");
            }
            string destination = snapshot.Path;
            string parent = Path.GetDirectoryName(destination);
            string temporary = Path.Combine(
                parent,
                ".kettle-install-tmp-" + Guid.NewGuid().ToString("N"));
            SafeFileHandle temporaryHandle = null;
            GCHandle descriptorPin = new GCHandle();
            IntPtr securityAttributes = IntPtr.Zero;
            try
            {
                if (snapshot.Exists)
                {
                    descriptorPin = GCHandle.Alloc(
                        snapshot.SecurityDescriptor,
                        GCHandleType.Pinned);
                    SecurityAttributes attributes = new SecurityAttributes();
                    attributes.Length = Marshal.SizeOf(
                        typeof(SecurityAttributes));
                    attributes.SecurityDescriptor =
                        descriptorPin.AddrOfPinnedObject();
                    attributes.InheritHandle = 0;
                    securityAttributes = Marshal.AllocHGlobal(
                        attributes.Length);
                    Marshal.StructureToPtr(
                        attributes,
                        securityAttributes,
                        false);
                }
                temporaryHandle = CreateFileW(
                    temporary,
                    GENERIC_WRITE | DELETE | WRITE_DAC,
                    FILE_SHARE_READ,
                    securityAttributes,
                    CREATE_NEW,
                    snapshot.Exists
                        ? snapshot.Information.FileAttributes
                        : FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero);
                if (temporaryHandle.IsInvalid)
                {
                    int error = Marshal.GetLastWin32Error();
                    temporaryHandle.Dispose();
                    temporaryHandle = null;
                    throw new Win32Exception(
                        error,
                        "Cannot create the profile temporary file");
                }
                WriteHeldBytes(temporaryHandle, bytes, temporary);
                if (snapshot.Exists)
                {
                    ByHandleFileInformation information =
                        snapshot.Information;
                    if (!SetFileTime(
                        temporaryHandle,
                        ref information.CreationTime,
                        ref information.LastAccessTime,
                        ref information.LastWriteTime))
                    {
                        throw new Win32Exception(
                            Marshal.GetLastWin32Error(),
                            "Cannot preserve PowerShell profile timestamps");
                    }
                }
                if (!FlushFileBuffers(temporaryHandle))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot flush preserved PowerShell profile metadata");
                }
                if (snapshot.Exists)
                {
                    // Data flushes set the archive bit. Restore the exact
                    // captured attributes only after the last write/flush.
                    SetHeldFileAttributes(
                        temporaryHandle,
                        snapshot.Information.FileAttributes,
                        temporary);
                }
                if (failBeforeAtomicReplace)
                {
                    throw new IOException(
                        "Injected profile publication failure before atomic replacement.");
                }
                if (snapshot.Exists)
                {
                    // The read handle blocks all writers while bytes and
                    // metadata are captured and staged. ReOpenFile then keeps
                    // the same file object pinned without delete sharing, but
                    // with the share mode required by POSIX replacement.
                    PrepareProfileReplacementHold(snapshot);
                }
                int renameError;
                // The retained destination handle denies delete sharing.
                // FileRenameInfoEx POSIX replacement is the single namespace
                // operation that can replace that held name without first
                // making the user's profile path disappear.
                if (!RenameHeldFileAtomic(
                    temporaryHandle,
                    destination,
                    snapshot.Exists,
                    out renameError))
                {
                    if (!snapshot.Exists &&
                        (renameError == ERROR_ALREADY_EXISTS ||
                            renameError == 80))
                    {
                        return false;
                    }
                    throw new Win32Exception(
                        renameError,
                        "Cannot atomically publish the PowerShell profile");
                }
                temporary = null;
                if (snapshot.Exists)
                {
                    SetHeldFileAttributes(
                        temporaryHandle,
                        snapshot.Information.FileAttributes,
                        destination);
                }
                return true;
            }
            finally
            {
                if (securityAttributes != IntPtr.Zero)
                {
                    Marshal.FreeHGlobal(securityAttributes);
                }
                if (descriptorPin.IsAllocated)
                {
                    descriptorPin.Free();
                }
                if (temporaryHandle != null)
                {
                    temporaryHandle.Dispose();
                }
                if (temporary != null)
                {
                    DeleteFileW(temporary);
                }
            }
        }

        public static void CopyRegularFileAtomic(
            string source,
            string destination,
            long maximumBytes)
        {
            CopyRegularFileAtomic(
                source,
                destination,
                maximumBytes,
                false);
        }

        public static void CopyRegularFileAtomic(
            string source,
            string destination,
            long maximumBytes,
            bool terminateAfterTemporaryFlush)
        {
            string parent = Path.GetDirectoryName(destination);
            string temporary = Path.Combine(
                parent,
                ".kettle-install-tmp-" + Guid.NewGuid().ToString("N"));
            SafeFileHandle sourceHandle = null;
            SafeFileHandle temporaryHandle = null;
            try
            {
                sourceHandle = OpenNoFollow(
                    source,
                    GENERIC_READ,
                    FILE_SHARE_READ,
                    FILE_FLAG_SEQUENTIAL_SCAN);
                ByHandleFileInformation information =
                    Information(sourceHandle, source);
                if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 ||
                    (information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    throw new IOException(
                        "Refusing to copy a non-regular source file: " + source);
                }
                ulong length =
                    ((ulong)information.FileSizeHigh << 32) | information.FileSizeLow;
                if (length > (ulong)maximumBytes)
                {
                    throw new IOException(
                        "Install source file exceeds its safety limit: " + source);
                }

                temporaryHandle = CreateFileW(
                    temporary,
                    GENERIC_WRITE,
                    FILE_SHARE_READ,
                    IntPtr.Zero,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero);
                if (temporaryHandle.IsInvalid)
                {
                    int error = Marshal.GetLastWin32Error();
                    temporaryHandle.Dispose();
                    temporaryHandle = null;
                    throw new Win32Exception(
                        error,
                        "Cannot create the installer temporary file");
                }

                using (FileStream input =
                    new FileStream(sourceHandle, FileAccess.Read))
                using (FileStream output =
                    new FileStream(temporaryHandle, FileAccess.Write))
                {
                    sourceHandle = null;
                    temporaryHandle = null;
                    input.CopyTo(output, 1024 * 1024);
                    output.Flush(true);
                }

                if (terminateAfterTemporaryFlush)
                {
                    HardTerminateInstallerForTesting();
                }
                if (!MoveFileExW(
                    temporary,
                    destination,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot atomically publish " + destination);
                }
                temporary = null;
            }
            finally
            {
                if (sourceHandle != null)
                {
                    sourceHandle.Dispose();
                }
                if (temporaryHandle != null)
                {
                    temporaryHandle.Dispose();
                }
                if (temporary != null)
                {
                    DeleteFileW(temporary);
                }
            }
        }

        public static void WriteBytesAtomic(string destination, byte[] bytes)
        {
            WriteBytesAtomic(destination, bytes, false);
        }

        public static void WriteBytesAtomic(
            string destination,
            byte[] bytes,
            bool terminateAfterTemporaryFlush)
        {
            string parent = Path.GetDirectoryName(destination);
            string temporary = Path.Combine(
                parent,
                ".kettle-install-tmp-" + Guid.NewGuid().ToString("N"));
            SafeFileHandle temporaryHandle = null;
            try
            {
                temporaryHandle = CreateFileW(
                    temporary,
                    GENERIC_WRITE,
                    FILE_SHARE_READ,
                    IntPtr.Zero,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero);
                if (temporaryHandle.IsInvalid)
                {
                    int error = Marshal.GetLastWin32Error();
                    temporaryHandle.Dispose();
                    temporaryHandle = null;
                    throw new Win32Exception(
                        error,
                        "Cannot create the installer temporary file");
                }
                using (FileStream output =
                    new FileStream(temporaryHandle, FileAccess.Write))
                {
                    temporaryHandle = null;
                    output.Write(bytes, 0, bytes.Length);
                    output.Flush(true);
                }
                if (terminateAfterTemporaryFlush)
                {
                    HardTerminateInstallerForTesting();
                }
                if (!MoveFileExW(
                    temporary,
                    destination,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Cannot atomically publish " + destination);
                }
                temporary = null;
            }
            finally
            {
                if (temporaryHandle != null)
                {
                    temporaryHandle.Dispose();
                }
                if (temporary != null)
                {
                    DeleteFileW(temporary);
                }
            }
        }

        public static void DeleteOrdinaryLeaf(string path)
        {
            if (DeleteFileW(path))
            {
                return;
            }
            int error = Marshal.GetLastWin32Error();
            if (error != ERROR_FILE_NOT_FOUND && error != ERROR_PATH_NOT_FOUND)
            {
                throw new Win32Exception(error, "Cannot delete " + path);
            }
        }

        private static void DeleteValidatedTemporaryLeaf(string path)
        {
            SafeFileHandle handle = OpenNoFollow(
                path,
                FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                0);
            try
            {
                ByHandleFileInformation information = Information(handle, path);
                ulong length =
                    ((ulong)information.FileSizeHigh << 32) |
                    information.FileSizeLow;
                if ((information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 ||
                    (information.FileAttributes &
                        FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
                    information.NumberOfLinks != 1 ||
                    length > 536870912)
                {
                    throw new IOException(
                        "Installer temporary path is not a bounded, " +
                        "single-link ordinary file: " + path);
                }
                RequireCurrentTokenOwner(handle, path);
                IntPtr disposition = Marshal.AllocHGlobal(4);
                try
                {
                    Marshal.WriteInt32(disposition, 1);
                    if (!SetFileInformationByHandle(
                        handle,
                        FILE_DISPOSITION_INFORMATION,
                        disposition,
                        4))
                    {
                        throw new Win32Exception(
                            Marshal.GetLastWin32Error(),
                            "Cannot delete installer temporary file " + path);
                    }
                }
                finally
                {
                    Marshal.FreeHGlobal(disposition);
                }
            }
            finally
            {
                handle.Dispose();
            }
        }

        public static void DeleteInstallerTemporaryLeaf(string path)
        {
            string name = Path.GetFileName(path);
            if (!System.Text.RegularExpressions.Regex.IsMatch(
                name,
                @"^\.kettle-install-tmp-[0-9a-f]{32}$",
                System.Text.RegularExpressions.RegexOptions.CultureInvariant))
            {
                throw new IOException(
                    "Refusing to delete a noncanonical installer temporary file: " +
                    path);
            }
            DeleteValidatedTemporaryLeaf(path);
        }

        public static void DeleteRustAtomicTemporaryLeaf(
            string path,
            string destinationName)
        {
            if (String.IsNullOrEmpty(destinationName) ||
                !String.Equals(
                    Path.GetFileName(destinationName),
                    destinationName,
                    StringComparison.Ordinal))
            {
                throw new IOException(
                    "Rust atomic temporary destination name is invalid.");
            }
            string name = Path.GetFileName(path);
            string pattern =
                @"^\." +
                System.Text.RegularExpressions.Regex.Escape(destinationName) +
                @"\.tmp\.([1-9][0-9]{0,9})\." +
                @"(0|[1-9][0-9]{0,38})\." +
                @"(0|[1-9][0-9]{0,19})$";
            System.Text.RegularExpressions.Match match =
                System.Text.RegularExpressions.Regex.Match(
                    name,
                    pattern,
                    System.Text.RegularExpressions.RegexOptions.CultureInvariant);
            uint processId;
            ulong sequence;
            const string maximumUInt128 =
                "340282366920938463463374607431768211455";
            string epochNanoseconds =
                match.Success ? match.Groups[2].Value : String.Empty;
            if (!match.Success ||
                !UInt32.TryParse(match.Groups[1].Value, out processId) ||
                !UInt64.TryParse(match.Groups[3].Value, out sequence) ||
                epochNanoseconds.Length > maximumUInt128.Length ||
                (epochNanoseconds.Length == maximumUInt128.Length &&
                    String.CompareOrdinal(
                        epochNanoseconds,
                        maximumUInt128) > 0))
            {
                throw new IOException(
                    "Refusing to delete a noncanonical Rust atomic temporary " +
                    "file: " + path);
            }
            IntPtr process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                processId);
            if (process != IntPtr.Zero)
            {
                CloseHandle(process);
                throw new IOException(
                    "Rust atomic temporary file still belongs to a live " +
                    "process: " + path);
            }
            int error = Marshal.GetLastWin32Error();
            if (error != ERROR_INVALID_PARAMETER)
            {
                throw new Win32Exception(
                    error,
                    "Cannot prove the Rust atomic temporary owner process is dead");
            }
            DeleteValidatedTemporaryLeaf(path);
        }

        public static void RemoveEmptyDirectory(string path)
        {
            if (RemoveDirectoryW(path))
            {
                return;
            }
            int error = Marshal.GetLastWin32Error();
            if (error != ERROR_FILE_NOT_FOUND && error != ERROR_PATH_NOT_FOUND)
            {
                throw new Win32Exception(
                    error,
                    "Cannot remove the nonempty or busy directory " + path);
            }
        }
    }
}
'@
}

function ConvertTo-KettleInstallPath {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,
        [Parameter(Mandatory = $true)]
        [string] $Context
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        throw "$Context is empty."
    }
    if ($Path -cnotmatch '^[A-Za-z]:[\\/](?![\\/])') {
        throw "$Context must be an absolute local drive path."
    }
    $rawParts = @($Path.Substring(3).Split([char[]]@('\', '/')))
    if ($rawParts.Count -eq 0) {
        throw "$Context must not be a filesystem root."
    }
    $reservedName = '^(?:CON|PRN|AUX|NUL|CLOCK\$|CONIN\$|CONOUT\$|COM[1-9\u00B9\u00B2\u00B3]|LPT[1-9\u00B9\u00B2\u00B3])$'
    foreach ($part in $rawParts) {
        $hasControl = $false
        foreach ($character in $part.ToCharArray()) {
            if ([char]::IsControl($character)) {
                $hasControl = $true
                break
            }
        }
        $deviceStem = $part.Split('.')[0].TrimEnd(' ')
        if (
            [string]::IsNullOrEmpty($part) -or
            $part -eq '.' -or
            $part -eq '..' -or
            $part.EndsWith('.') -or
            $part.EndsWith(' ') -or
            $part.IndexOfAny([char[]]@('<', '>', ':', '"', '|', '?', '*')) -ge 0 -or
            $hasControl -or
            $deviceStem -imatch $reservedName
        ) {
            throw "$Context contains an unsafe Win32 path component."
        }
    }
    try {
        $full = [System.IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    } catch {
        throw "$Context is not a valid absolute Windows path."
    }
    $root = [System.IO.Path]::GetPathRoot($full)
    if (
        [string]::IsNullOrWhiteSpace($root) -or
        $full.Length -le $root.TrimEnd('\', '/').Length
    ) {
        throw "$Context must not be a filesystem root."
    }
    return $full
}

function Test-KettleInstallPathEqual {
    param([string] $Left, [string] $Right)
    return [System.StringComparer]::OrdinalIgnoreCase.Equals($Left, $Right)
}

function Assert-KettleInstallPathChain {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $root = [System.IO.Path]::GetPathRoot($Path)
    $current = $root.TrimEnd('\', '/')
    foreach ($part in $Path.Substring($root.Length).Split('\')) {
        $current = Join-Path $current $part
        if (-not (Test-Path -LiteralPath $current)) {
            continue
        }
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        if (
            ($item.Attributes -band
                [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            -not $item.PSIsContainer
        ) {
            throw "Install prefix traverses a non-directory or reparse point: $current"
        }
    }
}

function Read-KettleStrictUtf8File {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,
        [Parameter(Mandatory = $true)]
        [int] $MaximumBytes,
        [Parameter(Mandatory = $true)]
        [string] $Label
    )

    try {
        $bytes = [KettleInstaller.NativeFileSystemV1]::ReadBoundedRegularFile(
            $Path,
            $MaximumBytes
        )
        if (
            $bytes.Length -ge 3 -and
            $bytes[0] -eq 0xEF -and
            $bytes[1] -eq 0xBB -and
            $bytes[2] -eq 0xBF
        ) {
            throw "$Label must be BOM-free UTF-8."
        }
        $strictUtf8 = New-Object System.Text.UTF8Encoding($false, $true)
        return $strictUtf8.GetString($bytes)
    } catch {
        throw "${Label} cannot be read as a bounded ordinary UTF-8 file: $($_.Exception.Message)"
    }
}

$script:KettleProfileBeginMarker =
    '# >>> kettle shell-integration (managed by install.ps1)'
$script:KettleProfileEndMarker =
    '# <<< kettle shell-integration (managed by install.ps1)'
$script:KettleProfileLeadingNewlineMarker =
    '# kettle installer owns the preceding newline'

function Get-KettleProfileDocument {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,
        [AllowEmptyCollection()]
        [byte[]] $Bytes
    )

    [byte[]] $profileBytes = @()
    if ($PSBoundParameters.ContainsKey('Bytes')) {
        $profileBytes = $Bytes
    } elseif (Test-Path -LiteralPath $Path) {
        $profileBytes = [KettleInstaller.NativeFileSystemV1]::ReadBoundedRegularFile(
            $Path,
            4194304
        )
    }
    $offset = 0
    $preamble = New-Object byte[] 0
    if (
        $profileBytes.Length -ge 3 -and
        $profileBytes[0] -eq 0xEF -and
        $profileBytes[1] -eq 0xBB -and
        $profileBytes[2] -eq 0xBF
    ) {
        $encoding = New-Object System.Text.UTF8Encoding($true, $true)
        $offset = 3
        $preamble = [byte[]]@(0xEF, 0xBB, 0xBF)
    } elseif (
        $profileBytes.Length -ge 2 -and
        $profileBytes[0] -eq 0xFF -and
        $profileBytes[1] -eq 0xFE
    ) {
        $encoding = New-Object System.Text.UnicodeEncoding(
            $false,
            $true,
            $true
        )
        $offset = 2
        $preamble = [byte[]]@(0xFF, 0xFE)
    } elseif (
        $profileBytes.Length -ge 2 -and
        $profileBytes[0] -eq 0xFE -and
        $profileBytes[1] -eq 0xFF
    ) {
        $encoding = New-Object System.Text.UnicodeEncoding(
            $true,
            $true,
            $true
        )
        $offset = 2
        $preamble = [byte[]]@(0xFE, 0xFF)
    } else {
        $encoding = New-Object System.Text.UTF8Encoding($false, $true)
    }
    try {
        $text = $encoding.GetString(
            $profileBytes,
            $offset,
            $profileBytes.Length - $offset
        )
    } catch {
        throw "PowerShell profile has an invalid or unsupported encoding: $Path"
    }
    if ($text.IndexOf([char]0) -ge 0) {
        throw "PowerShell profile contains an embedded NUL: $Path"
    }
    $newlineMatch = [regex]::Match($text, "`r`n|`n|`r")
    $newline = if ($newlineMatch.Success) {
        $newlineMatch.Value
    } else {
        "`r`n"
    }
    return [pscustomobject]@{
        Text = $text
        Encoding = $encoding
        Preamble = $preamble
        Newline = $newline
    }
}

function Get-KettleManagedProfileBlock {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $Text
    )

    $begin = $script:KettleProfileBeginMarker
    $end = $script:KettleProfileEndMarker
    $leadingNewline = $script:KettleProfileLeadingNewlineMarker
    $beginOccurrences = ([regex]::Matches(
        $Text,
        [regex]::Escape($begin)
    )).Count
    $endOccurrences = ([regex]::Matches(
        $Text,
        [regex]::Escape($end)
    )).Count
    $beginLines = New-Object 'System.Collections.Generic.List[object]'
    $endLines = New-Object 'System.Collections.Generic.List[object]'
    $leadingNewlineLines =
        New-Object 'System.Collections.Generic.List[object]'
    $leadingNewlineOccurrences = ([regex]::Matches(
        $Text,
        [regex]::Escape($leadingNewline)
    )).Count
    foreach ($line in [regex]::Matches($Text, '.*?(?:\r\n|\n|\r|$)')) {
        if ($line.Length -eq 0) {
            continue
        }
        $lineText = $line.Value.TrimEnd("`r", "`n")
        if ($lineText -ceq $begin) {
            $beginLines.Add($line)
        } elseif ($lineText -ceq $end) {
            $endLines.Add($line)
        } elseif ($lineText -ceq $leadingNewline) {
            $leadingNewlineLines.Add($line)
        }
    }
    if (
        $beginOccurrences -ne $beginLines.Count -or
        $endOccurrences -ne $endLines.Count -or
        $leadingNewlineOccurrences -ne $leadingNewlineLines.Count
    ) {
        throw 'PowerShell profile contains a managed marker outside an exact standalone line.'
    }
    if ($beginLines.Count -eq 0 -and $endLines.Count -eq 0) {
        if ($leadingNewlineLines.Count -ne 0) {
            throw 'PowerShell profile contains orphaned Kettle managed-block metadata.'
        }
        return [pscustomobject]@{
            Present = $false
            Start = 0
            Length = 0
            OwnedLeadingNewlineLength = 0
        }
    }
    if (
        $beginLines.Count -ne 1 -or
        $endLines.Count -ne 1 -or
        $beginLines[0].Index -ge $endLines[0].Index -or
        $leadingNewlineLines.Count -gt 1
    ) {
        throw 'PowerShell profile has ambiguous or unbalanced Kettle managed markers.'
    }
    $blockStart = $beginLines[0].Index
    $blockLength = (
        $endLines[0].Index +
        $endLines[0].Length -
        $blockStart
    )
    $ownedLength = 0
    if ($leadingNewlineLines.Count -eq 1) {
        if (
            $leadingNewlineLines[0].Index -ne (
                $beginLines[0].Index + $beginLines[0].Length
            )
        ) {
            throw 'PowerShell profile has misplaced Kettle managed-block metadata.'
        }
        $prefix = $Text.Substring(0, $blockStart)
        $ownedLength = if ($prefix.EndsWith("`r`n")) {
            2
        } elseif (
            $prefix.EndsWith("`r") -or
            $prefix.EndsWith("`n")
        ) {
            1
        } else {
            0
        }
        if ($ownedLength -eq 0) {
            throw 'PowerShell profile managed block claims a missing leading newline.'
        }
        $blockStart -= $ownedLength
        $blockLength += $ownedLength
    }
    return [pscustomobject]@{
        Present = $true
        Start = $blockStart
        Length = $blockLength
        OwnedLeadingNewlineLength = $ownedLength
    }
}

function ConvertTo-KettleProfileByteArray {
    param(
        [Parameter(Mandatory = $true)]
        [object] $Document,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $Text
    )

    $body = $Document.Encoding.GetBytes($Text)
    $bytes = New-Object byte[] ($Document.Preamble.Length + $body.Length)
    [Array]::Copy(
        $Document.Preamble,
        0,
        $bytes,
        0,
        $Document.Preamble.Length
    )
    [Array]::Copy(
        $body,
        0,
        $bytes,
        $Document.Preamble.Length,
        $body.Length
    )
    return ,$bytes
}

function Invoke-KettleProfileIntegration {
    param(
        [Parameter(Mandatory = $true)]
        [string] $ProfilePath,
        [switch] $Remove,
        [AllowEmptyString()]
        [string] $Snippet = ''
    )

    $profileDir = Split-Path $ProfilePath -Parent
    if (-not (Test-Path -LiteralPath $profileDir)) {
        if ($Remove) {
            return $false
        }
        [void][System.IO.Directory]::CreateDirectory($profileDir)
    }
    $chain = [KettleInstaller.NativeFileSystemV1]::LockRealDirectoryChain(
        $profileDir
    )
    try {
        for ($attempt = 0; $attempt -lt 4; $attempt++) {
            $chain.Verify()
            $snapshot = [KettleInstaller.NativeFileSystemV1]::CaptureProfile(
                $ProfilePath,
                4194304
            )
            try {
                $document = Get-KettleProfileDocument `
                    -Path $ProfilePath -Bytes $snapshot.Bytes
                $block = Get-KettleManagedProfileBlock -Text $document.Text
                if ($Remove) {
                    if (-not $block.Present) {
                        return $false
                    }
                    $before = $document.Text.Substring(0, $block.Start)
                    $after = $document.Text.Substring(
                        $block.Start + $block.Length
                    )
                    $separator = ''
                    if (
                        $block.OwnedLeadingNewlineLength -ne 0 -and
                        $before.Length -ne 0 -and
                        $after.Length -ne 0 -and
                        -not $before.EndsWith("`r") -and
                        -not $before.EndsWith("`n") -and
                        -not $after.StartsWith("`r") -and
                        -not $after.StartsWith("`n")
                    ) {
                        # The managed block owned the only separator after a
                        # profile that originally had no final newline.
                        $separator = $document.Newline
                    }
                    $newText = $before + $separator + $after
                } else {
                    if ($block.Present) {
                        return $false
                    }
                    $newline = $document.Newline
                    $normalizedSnippet = (
                        $Snippet -replace "`r`n|`n|`r", $newline
                    ).TrimEnd("`r", "`n")
                    $separator = if ($document.Text.Length -eq 0) {
                        ''
                    } elseif (
                        $document.Text.EndsWith("`r") -or
                        $document.Text.EndsWith("`n")
                    ) {
                        ''
                    } else {
                        $newline
                    }
                    $leadingNewlineMetadata = if ($separator.Length -ne 0) {
                        $script:KettleProfileLeadingNewlineMarker + $newline
                    } else {
                        ''
                    }
                    $newText = (
                        $document.Text +
                        $separator +
                        $script:KettleProfileBeginMarker +
                        $newline +
                        $leadingNewlineMetadata +
                        $normalizedSnippet +
                        $newline +
                        $script:KettleProfileEndMarker +
                        $newline
                    )
                }
                $bytes = ConvertTo-KettleProfileByteArray `
                    -Document $document -Text $newText
                $failBeforeAtomicReplace = (
                    -not [string]::IsNullOrWhiteSpace(
                        $IntegrationTestRoot
                    ) -and
                    $env:KETTLE_INSTALLER_TEST_PROFILE_FAIL_BEFORE_REPLACE `
                        -ceq '1'
                )
                $published =
                    [KettleInstaller.NativeFileSystemV1]::PublishProfile(
                        $snapshot,
                        $bytes,
                        $failBeforeAtomicReplace
                    )
                $chain.Verify()
                if ($published) {
                    return $true
                }
            } finally {
                $snapshot.Dispose()
            }
        }
        throw 'PowerShell profile changed repeatedly during publication.'
    } finally {
        $chain.Dispose()
    }
}

if (
    -not [string]::IsNullOrWhiteSpace($IntegrationTestRoot) -and
    -not [string]::IsNullOrWhiteSpace(
        $env:KETTLE_INSTALLER_TEST_PROFILE_ONLY
    )
) {
    $profileTestRoot = [System.IO.Path]::GetFullPath(
        $IntegrationTestRoot
    ).TrimEnd('\', '/')
    $profileTestPath = [System.IO.Path]::GetFullPath(
        $env:KETTLE_INSTALLER_TEST_PROFILE_ONLY
    )
    if (-not $profileTestPath.StartsWith(
        $profileTestRoot + '\',
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'The profile-only test path escaped its integration root.'
    }
    $profileTestRemove = (
        $env:KETTLE_INSTALLER_TEST_PROFILE_REMOVE -ceq '1'
    )
    [void](Invoke-KettleProfileIntegration `
        -ProfilePath $profileTestPath `
        -Snippet "Write-Output 'kettle profile test'" `
        -Remove:$profileTestRemove)
    return
}

function Assert-KettleSafeInstallPrefix {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,
        [AllowEmptyString()]
        [string] $TestRoot = ''
    )

    $full = ConvertTo-KettleInstallPath -Path $Path -Context 'Install prefix'
    if (-not [System.StringComparer]::OrdinalIgnoreCase.Equals(
        [System.IO.Path]::GetFileName($full),
        'kettle'
    )) {
        throw 'Install prefix must be a dedicated directory named kettle.'
    }

    $protectedPaths = @(
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::UserProfile
        ),
        $env:LOCALAPPDATA,
        $env:APPDATA,
        $env:TEMP,
        $env:TMP,
        $env:SystemRoot,
        $env:ProgramFiles,
        [Environment]::GetEnvironmentVariable('ProgramFiles(x86)'),
        $TestRoot
    )
    foreach ($protectedPath in $protectedPaths) {
        if ([string]::IsNullOrWhiteSpace($protectedPath)) {
            continue
        }
        $protectedFull = try {
            ConvertTo-KettleInstallPath `
                -Path $protectedPath -Context 'Protected path'
        } catch {
            [System.IO.Path]::GetFullPath($protectedPath).TrimEnd('\', '/')
        }
        if (Test-KettleInstallPathEqual $full $protectedFull) {
            throw "Install prefix is a protected broad directory: $full"
        }
    }
    Assert-KettleInstallPathChain -Path $full
    [KettleInstaller.NativeFileSystemV1]::ValidateFixedDrive($full)
    return $full
}

function Read-KettleInstallPrefixMarker {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $raw = Read-KettleStrictUtf8File `
        -Path $Path -MaximumBytes 32768 -Label 'Installed prefix marker'
    if (
        $raw.Length -eq 0 -or
        $raw.IndexOf([char]0) -ge 0 -or
        $raw.Contains("`r") -or
        $raw.Contains("`n")
    ) {
        throw 'The installed prefix marker has an invalid encoding or shape.'
    }
    return ConvertTo-KettleInstallPath `
        -Path $raw -Context 'Installed prefix marker'
}

function Assert-KettleOrdinaryInstallFile {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,
        [Parameter(Mandatory = $true)]
        [string] $Label
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing."
    }
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band
            [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw "$Label is not an ordinary file."
    }
    return $item
}

function Assert-KettleInstallOwnership {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    [void](Assert-KettleOrdinaryInstallFile `
        -Path (Join-Path $Path 'kettle.exe') -Label 'Installed kettle.exe')
    [void](Assert-KettleOrdinaryInstallFile `
        -Path (Join-Path $Path 'install.ps1') -Label 'Installed helper')
    [void](Assert-KettleOrdinaryInstallFile `
        -Path (Join-Path $Path 'kettle.com') -Label 'Installed console launcher')
    $prefixFile = Join-Path $Path '.kettle-install-prefix'
    $markerPrefix = Read-KettleInstallPrefixMarker -Path $prefixFile
    if (-not (Test-KettleInstallPathEqual $markerPrefix $Path)) {
        throw 'The installed prefix marker does not name its helper directory.'
    }

    $ownershipPath = Join-Path $Path '.kettle-install.json'
    $ownershipItem = Assert-KettleOrdinaryInstallFile `
        -Path $ownershipPath -Label 'Install ownership marker'
    if ($ownershipItem.Length -le 0 -or $ownershipItem.Length -gt 4096) {
        throw 'The install ownership marker is not bounded.'
    }
    try {
        $ownershipRaw = Read-KettleStrictUtf8File `
            -Path $ownershipItem.FullName -MaximumBytes 4096 `
            -Label 'Install ownership marker'
        $keyMatches = @(
            [regex]::Matches($ownershipRaw, '"([^"\\]*)"\s*:')
        )
        $expectedKeys = @(
            'schema',
            'product',
            'managed_by',
            'channel',
            'target',
            'version'
        )
        if ($keyMatches.Count -ne $expectedKeys.Count) {
            throw 'unexpected or duplicate JSON keys'
        }
        $seenKeys = New-Object 'System.Collections.Generic.HashSet[string]' (
            [System.StringComparer]::Ordinal
        )
        foreach ($keyMatch in $keyMatches) {
            $keyName = $keyMatch.Groups[1].Value
            if (
                $expectedKeys -cnotcontains $keyName -or
                -not $seenKeys.Add($keyName)
            ) {
                throw 'unexpected, escaped, or duplicate JSON keys'
            }
        }
        $ownership = $ownershipRaw | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw 'The install ownership marker is not valid strict JSON.'
    }
    $actualKeys = @($ownership.PSObject.Properties.Name)
    if (
        $ownership -isnot [System.Management.Automation.PSCustomObject] -or
        $actualKeys.Count -ne 6 -or
        (
            $ownership.schema -isnot [int] -and
            $ownership.schema -isnot [long]
        ) -or
        $ownership.schema -ne 1 -or
        $ownership.product -isnot [string] -or
        $ownership.product -cne 'kettle' -or
        $ownership.managed_by -isnot [string] -or
        $ownership.managed_by -cne 'kettle-installer' -or
        $ownership.channel -isnot [string] -or
        $ownership.channel -notin @('stable', 'local-dev') -or
        $ownership.target -isnot [string] -or
        $ownership.target -cne 'x86_64-pc-windows-msvc' -or
        $ownership.version -isnot [string] -or
        $ownership.version -cnotmatch (
            '^(?:unknown|[0-9]+\.[0-9]+\.[0-9]+)$'
        )
    ) {
        throw 'The install ownership marker has an invalid product identity.'
    }
}

function Test-KettleUpdateTransactionId {
    param([Parameter(Mandatory = $true)][string] $Value)

    if (
        $Value -cnotmatch
            '^(0|[1-9][0-9]{0,9})-(0|[1-9][0-9]{0,38})$'
    ) {
        return $false
    }
    $parsedPid = [uint32]0
    if (
        -not [uint32]::TryParse(
            $Matches[1],
            [System.Globalization.NumberStyles]::None,
            [System.Globalization.CultureInfo]::InvariantCulture,
            [ref]$parsedPid
        )
    ) {
        return $false
    }
    $maximumEpochNanoseconds =
        '340282366920938463463374607431768211455'
    return (
        $Matches[2].Length -lt $maximumEpochNanoseconds.Length -or
        (
            $Matches[2].Length -eq $maximumEpochNanoseconds.Length -and
            [String]::CompareOrdinal(
                $Matches[2],
                $maximumEpochNanoseconds
            ) -le 0
        )
    )
}

function Test-KettleManagedRootFileName {
    param([Parameter(Mandatory = $true)][string] $Name)

    $fixed = @(
        'kettle.exe',
        'kettle.com',
        'install.ps1',
        'kettle.ico',
        'LICENSE',
        'NOTICE',
        'README.md',
        'CHANGELOG.md',
        'kettle-package-manifest.json',
        '.kettle-install-prefix',
        '.kettle-install.json',
        '.kettle-running.lock',
        '.kettle-update.lock',
        '.kettle-update-pending.json',
        '.kettle-update-journal.json'
    )
    if ($fixed -ccontains $Name) {
        return $true
    }
    if ($Name -cmatch '^\.kettle-install-tmp-[0-9a-f]{32}$') {
        return $true
    }
    if (
        $Name -cmatch '^\.kettle-update-helper-(.+)\.exe$' -and
        (Test-KettleUpdateTransactionId -Value $Matches[1])
    ) {
        return $true
    }
    if (
        $Name -cmatch '^\.kettle-update-archive-(.+)\.zip$' -and
        (Test-KettleUpdateTransactionId -Value $Matches[1])
    ) {
        return $true
    }
    if (
        $Name -cmatch '^\.kettle-update-failed-(.+)\.(?:json|txt)$' -and
        (Test-KettleUpdateTransactionId -Value $Matches[1])
    ) {
        return $true
    }
    return (
        $Name -cmatch (
            '^kettle\.(?:exe|com)\.bak-(?:' +
            '[0-9]{1,4}\.[0-9]{1,4}\.[0-9]{1,4}-[0-9]{8}|' +
            '[0-9]{4}-[0-9]{2}-[0-9]{2}|' +
            '[0-9]{1,4}-[0-9]{1,4}' +
            ')$'
        )
    )
}

function Test-KettleUpdateArtifactDirectoryName {
    param([Parameter(Mandatory = $true)][string] $Name)

    foreach ($prefix in @(
        '.kettle-update-stage-',
        '.kettle-update-backup-'
    )) {
        if ($Name.StartsWith($prefix, [StringComparison]::Ordinal)) {
            return Test-KettleUpdateTransactionId -Value (
                $Name.Substring($prefix.Length)
            )
        }
    }
    return $false
}

function Test-KettleWindowsPayloadRelativePath {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Relative,
        [switch] $AllowInstallMarker
    )

    $parts = @($Relative.Replace('\', '/').Split('/'))
    $rootFiles = @(
        'kettle.exe',
        'kettle.com',
        'install.ps1',
        'kettle.ico',
        'LICENSE',
        'NOTICE',
        'README.md',
        'CHANGELOG.md',
        'kettle-package-manifest.json'
    )
    if ($AllowInstallMarker) {
        $rootFiles += @(
            '.kettle-install-prefix',
            '.kettle-install.json'
        )
    }
    if ($parts.Count -eq 1) {
        return $rootFiles -ccontains $parts[0]
    }
    return (
        $parts.Count -eq 2 -and
        $parts[0] -ceq 'shell-integration' -and
        @(
            'kettle.bash',
            'kettle.fish',
            'kettle.ps1',
            'kettle.zsh'
        ) -ccontains $parts[1]
    )
}

function Remove-KettleRustAtomicTemporarySet {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Deletes only validated orphan leaves during recovery.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string] $Directory,
        [Parameter(Mandatory = $true)]
        [ValidateSet('root', 'artifact', 'shell')]
        [string] $Context,
        [int] $MaximumLeaves = 8
    )

    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
        return 0
    }
    $directoryLock =
        [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($Directory)
    try {
        $entries = @(
            Get-ChildItem -LiteralPath $Directory -Force -ErrorAction Stop
        )
        if ($entries.Count -gt 256) {
            throw 'Rust atomic temporary recovery exceeded its entry limit.'
        }
        $temporary = @()
        foreach ($item in $entries) {
            if (
                $item.PSIsContainer -or
                $item.Name -cnotmatch (
                    '^\.(.+)\.tmp\.([1-9][0-9]{0,9})\.' +
                    '(?:0|[1-9][0-9]{0,38})\.' +
                    '(?:0|[1-9][0-9]{0,19})$'
                )
            ) {
                continue
            }
            $destinationName = $Matches[1]
            $allowed = switch ($Context) {
                'shell' {
                    @(
                        'kettle.bash',
                        'kettle.fish',
                        'kettle.ps1',
                        'kettle.zsh'
                    ) -ccontains $destinationName
                }
                'artifact' {
                    $destinationName -ceq '.kettle-update-backup.json' -or
                    (Test-KettleWindowsPayloadRelativePath `
                        -Relative $destinationName -AllowInstallMarker)
                }
                'root' {
                    (
                        @(
                            'kettle.exe',
                            'kettle.com',
                            'install.ps1',
                            'kettle.ico',
                            'LICENSE',
                            'NOTICE',
                            'README.md',
                            'CHANGELOG.md',
                            'kettle-package-manifest.json',
                            '.kettle-install-prefix',
                            '.kettle-install.json',
                            '.kettle-update-pending.json',
                            '.kettle-update-journal.json'
                        ) -ccontains $destinationName
                    ) -or (
                        $destinationName -cmatch
                            '^\.kettle-update-helper-(.+)\.exe$' -and
                        (Test-KettleUpdateTransactionId -Value $Matches[1])
                    ) -or (
                        $destinationName -cmatch
                            '^\.kettle-update-archive-(.+)\.zip$' -and
                        (Test-KettleUpdateTransactionId -Value $Matches[1])
                    ) -or (
                        $destinationName -cmatch
                            '^\.kettle-update-failed-(.+)\.(?:json|txt)$' -and
                        (Test-KettleUpdateTransactionId -Value $Matches[1])
                    )
                }
            }
            if ($allowed) {
                $temporary += [pscustomobject]@{
                    Item = $item
                    DestinationName = $destinationName
                }
            }
        }
        if ($temporary.Count -gt $MaximumLeaves) {
            throw 'Rust atomic temporary recovery exceeded its leaf limit.'
        }
        foreach ($candidate in $temporary) {
            [KettleInstaller.NativeFileSystemV1]::DeleteRustAtomicTemporaryLeaf(
                $candidate.Item.FullName,
                $candidate.DestinationName
            )
        }
        return $temporary.Count
    } finally {
        $directoryLock.Dispose()
    }
}

function Remove-KettleManagedRustAtomicTemporarySet {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Runs the bounded internal orphan-recovery scans.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string] $Prefix
    )

    [void](Remove-KettleRustAtomicTemporarySet `
        -Directory $Prefix -Context root)
    $shell = Join-Path $Prefix 'shell-integration'
    if (Test-Path -LiteralPath $shell -PathType Container) {
        [void](Remove-KettleRustAtomicTemporarySet `
            -Directory $shell -Context shell)
    }
    foreach ($artifact in @(
        Get-ChildItem -LiteralPath $Prefix -Directory -Force `
            -ErrorAction Stop | Where-Object {
                Test-KettleUpdateArtifactDirectoryName -Name $_.Name
            }
    )) {
        [void](Remove-KettleRustAtomicTemporarySet `
            -Directory $artifact.FullName -Context artifact)
        $artifactShell = Join-Path $artifact.FullName 'shell-integration'
        if (Test-Path -LiteralPath $artifactShell -PathType Container) {
            [void](Remove-KettleRustAtomicTemporarySet `
                -Directory $artifactShell -Context shell)
        }
    }
}

function Read-KettleVerifiedReleaseManifest {
    param(
        [Parameter(Mandatory = $true)]
        [string] $PackageRoot
    )

    $manifestPath = Join-Path $PackageRoot 'kettle-package-manifest.json'
    $raw = Read-KettleStrictUtf8File `
        -Path $manifestPath -MaximumBytes 262144 `
        -Label 'Release package manifest'
    try {
        $manifest = $raw | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw 'The release package manifest is not valid bounded JSON.'
    }
    $topKeys = @($manifest.PSObject.Properties.Name)
    if (
        $manifest -isnot [System.Management.Automation.PSCustomObject] -or
        $topKeys.Count -ne 5 -or
        @($topKeys | Where-Object {
            @('schema', 'product', 'target', 'version', 'files') -cnotcontains $_
        }).Count -ne 0 -or
        (
            $manifest.schema -isnot [int] -and
            $manifest.schema -isnot [long]
        ) -or
        $manifest.schema -ne 1 -or
        $manifest.product -isnot [string] -or
        $manifest.product -cne 'kettle' -or
        $manifest.target -isnot [string] -or
        $manifest.target -cne 'x86_64-pc-windows-msvc' -or
        $manifest.version -isnot [string] -or
        $manifest.version -cnotmatch (
            '^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.' +
            '(?:0|[1-9][0-9]*)$'
        ) -or
        $manifest.files -isnot [System.Array] -or
        $manifest.files.Count -lt 1 -or
        $manifest.files.Count -gt 127
    ) {
        throw 'The release package manifest has an invalid identity or schema.'
    }

    $declared = New-Object 'System.Collections.Generic.Dictionary[string,object]' (
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $lastPath = $null
    [uint64]$total = 0
    foreach ($record in $manifest.files) {
        $recordKeys = @($record.PSObject.Properties.Name)
        if (
            $record -isnot [System.Management.Automation.PSCustomObject] -or
            $recordKeys.Count -ne 4 -or
            @($recordKeys | Where-Object {
                @('path', 'size', 'sha256', 'mode') -cnotcontains $_
            }).Count -ne 0 -or
            $record.path -isnot [string] -or
            $record.path -ceq 'kettle-package-manifest.json' -or
            -not (Test-KettleWindowsPayloadRelativePath -Relative $record.path) -or
            $record.path.Contains('\') -or
            (
                $record.size -isnot [int] -and
                $record.size -isnot [long]
            ) -or
            [int64]$record.size -lt 0 -or
            [uint64]$record.size -gt 536870912 -or
            $record.sha256 -isnot [string] -or
            $record.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
            $null -ne $record.mode -or
            $declared.ContainsKey($record.path) -or
            (
                $null -ne $lastPath -and
                [StringComparer]::Ordinal.Compare($lastPath, $record.path) -ge 0
            )
        ) {
            throw 'The release package manifest contains an invalid file record.'
        }
        $total += [uint64]$record.size
        if ($total -gt 536870912) {
            throw 'The release package manifest exceeds the aggregate size limit.'
        }
        $declared.Add($record.path, $record)
        $lastPath = $record.path
    }

    $actual = New-Object 'System.Collections.Generic.HashSet[string]' (
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($item in Get-ChildItem -LiteralPath $PackageRoot -Force -Recurse) {
        if (
            ($item.Attributes -band
                [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "The release package contains a reparse point: $($item.FullName)"
        }
        if ($item.PSIsContainer) {
            if (
                -not (Test-KettleInstallPathEqual `
                    $item.FullName (Join-Path $PackageRoot 'shell-integration'))
            ) {
                throw "The release package contains an unexpected directory: $($item.FullName)"
            }
            continue
        }
        $relative = $item.FullName.Substring($PackageRoot.Length).TrimStart(
            '\',
            '/'
        ).Replace('\', '/')
        if ($relative -ceq 'kettle-package-manifest.json') {
            continue
        }
        $record = $null
        if (
            -not $declared.TryGetValue($relative, [ref]$record) -or
            $record.path -cne $relative -or
            -not $actual.Add($relative) -or
            [uint64]$item.Length -ne [uint64]$record.size
        ) {
            throw "The release package file set does not match its manifest: $relative"
        }
        $digest = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash
        if ($digest.ToLowerInvariant() -cne $record.sha256) {
            throw "The release package hash does not match its manifest: $relative"
        }
    }
    if ($actual.Count -ne $declared.Count) {
        throw 'The release package is missing one or more manifest files.'
    }
    $manifestBytes = (New-Object System.Text.UTF8Encoding(
        $false,
        $true
    )).GetBytes($raw)
    $manifestHash = [System.Security.Cryptography.SHA256]::Create()
    try {
        $manifestDigest = (
            [BitConverter]::ToString(
                $manifestHash.ComputeHash($manifestBytes)
            ).Replace('-', '').ToLowerInvariant()
        )
    } finally {
        $manifestHash.Dispose()
    }
    $manifest | Add-Member -NotePropertyName '_manifest_size' `
        -NotePropertyValue ([uint64]$manifestBytes.Length)
    $manifest | Add-Member -NotePropertyName '_manifest_sha256' `
        -NotePropertyValue $manifestDigest
    return $manifest
}

function Remove-KettleInstallerTemporarySet {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Deletes only validated transaction temporary leaves.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string] $Directory,
        [int] $MaximumLeaves = 4
    )

    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
        return 0
    }
    $directoryLock =
        [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($Directory)
    try {
        $entries = @(
            Get-ChildItem -LiteralPath $Directory -Force -ErrorAction Stop
        )
        if ($entries.Count -gt 256) {
            throw 'Installer temporary-file recovery exceeded its entry limit.'
        }
        $temporary = @(
            $entries | Where-Object {
                -not $_.PSIsContainer -and
                $_.Name -cmatch '^\.kettle-install-tmp-[0-9a-f]{32}$'
            }
        )
        if ($temporary.Count -gt $MaximumLeaves) {
            throw 'Installer temporary-file recovery exceeded its leaf limit.'
        }
        foreach ($item in $temporary) {
            [KettleInstaller.NativeFileSystemV1]::DeleteInstallerTemporaryLeaf(
                $item.FullName
            )
        }
        return $temporary.Count
    } finally {
        $directoryLock.Dispose()
    }
}

function Write-KettlePackageJournal {
    param(
        [Parameter(Mandatory = $true)]
        [string] $TransactionRoot,
        [Parameter(Mandatory = $true)]
        [object] $Journal,
        [switch] $HardKillAfterTemporaryFlush
    )

    $encoding = New-Object System.Text.UTF8Encoding($false)
    $bytes = $encoding.GetBytes(
        (($Journal | ConvertTo-Json -Depth 5) + "`n")
    )
    [KettleInstaller.NativeFileSystemV1]::WriteBytesAtomic(
        (Join-Path $TransactionRoot 'journal.json'),
        $bytes,
        [bool]$HardKillAfterTemporaryFlush
    )
}

function Read-KettlePackageJournal {
    param(
        [Parameter(Mandatory = $true)]
        [string] $TransactionRoot,
        [Parameter(Mandatory = $true)]
        [string] $Prefix
    )

    $raw = Read-KettleStrictUtf8File `
        -Path (Join-Path $TransactionRoot 'journal.json') `
        -MaximumBytes 1048576 -Label 'Installer package journal'
    try {
        $journal = $raw | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw 'The installer package journal is not valid bounded JSON.'
    }
    $records = @($journal.files)
    if (
        (
            $journal.schema -isnot [int] -and
            $journal.schema -isnot [long]
        ) -or
        $journal.schema -ne 2 -or
        $journal.product -isnot [string] -or
        $journal.product -cne 'kettle-installer' -or
        $journal.PSObject.Properties.Name -cnotcontains
            'created_directories' -or
        $journal.prefix -isnot [string] -or
        -not (Test-KettleInstallPathEqual $journal.prefix $Prefix) -or
        (
            $journal.published -isnot [int] -and
            $journal.published -isnot [long]
        ) -or
        $journal.published -lt 0 -or
        $journal.published -gt $records.Count -or
        $records.Count -lt 1 -or
        $records.Count -gt 127 -or
        @($journal.created_directories).Count -gt 1 -or
        (
            @($journal.created_directories).Count -eq 1 -and
            @($journal.created_directories)[0] -cne 'shell-integration'
        )
    ) {
        throw 'The installer package journal has an invalid identity.'
    }
    $seen = New-Object 'System.Collections.Generic.HashSet[string]' (
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($record in $records) {
        if (
            $record.relative -isnot [string] -or
            -not (Test-KettleWindowsPayloadRelativePath `
                -Relative $record.relative -AllowInstallMarker) -or
            $record.relative.Contains('\') -or
            $record.existed -isnot [bool] -or
            -not $seen.Add($record.relative)
        ) {
            throw 'The installer package journal contains an invalid file record.'
        }
    }
    return $journal
}

function Repair-KettlePackageTransactionRoot {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Prefix,
        [Parameter(Mandatory = $true)]
        [string] $TransactionRoot
    )

    $transactionLock =
        [KettleInstaller.NativeFileSystemV1]::LockPrivateDirectory(
            $TransactionRoot
        )
    $removeRoot = $false
    try {
        [void](Remove-KettleInstallerTemporarySet `
            -Directory $TransactionRoot)
        $journalPath = Join-Path $TransactionRoot 'journal.json'
        if (Test-Path -LiteralPath $journalPath -PathType Leaf) {
            [void](Read-KettlePackageJournal `
                -TransactionRoot $TransactionRoot -Prefix $Prefix)
            return $true
        }

        # Publication cannot begin before the first journal is durable. A
        # journal-free private root may therefore contain only the empty
        # scaffold and an interrupted journal temporary file (removed above).
        $rootEntries = @(
            Get-ChildItem -LiteralPath $TransactionRoot -Force `
                -ErrorAction Stop
        )
        if ($rootEntries.Count -gt 2) {
            throw 'Uninitialized installer transaction has too many entries.'
        }
        foreach ($item in $rootEntries) {
            if (
                -not $item.PSIsContainer -or
                ($item.Attributes -band
                    [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
                $item.Name -cnotin @('stage', 'backup')
            ) {
                throw (
                    'Uninitialized installer transaction contains an ' +
                    "unexpected entry: $($item.Name)"
                )
            }
        }

        foreach ($treeName in @('stage', 'backup')) {
            $tree = Join-Path $TransactionRoot $treeName
            if (-not (Test-Path -LiteralPath $tree -PathType Container)) {
                continue
            }
            $treeLock =
                [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($tree)
            try {
                [void](Remove-KettleInstallerTemporarySet -Directory $tree)
                $shell = Join-Path $tree 'shell-integration'
                if (Test-Path -LiteralPath $shell -PathType Container) {
                    $shellLock =
                        [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                            $shell
                        )
                    try {
                        [void](Remove-KettleInstallerTemporarySet `
                            -Directory $shell)
                        if (@(
                            Get-ChildItem -LiteralPath $shell -Force `
                                -ErrorAction Stop
                        ).Count -ne 0) {
                            throw (
                                'Uninitialized installer transaction has a ' +
                                'nonempty shell scaffold.'
                            )
                        }
                    } finally {
                        $shellLock.Dispose()
                    }
                    [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
                        $shell
                    )
                }
                if (@(
                    Get-ChildItem -LiteralPath $tree -Force `
                        -ErrorAction Stop
                ).Count -ne 0) {
                    throw (
                        'Uninitialized installer transaction has a nonempty ' +
                        "$treeName scaffold."
                    )
                }
            } finally {
                $treeLock.Dispose()
            }
            [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($tree)
        }
        if (@(
            Get-ChildItem -LiteralPath $TransactionRoot -Force `
                -ErrorAction Stop
        ).Count -ne 0) {
            throw 'Uninitialized installer transaction cleanup was incomplete.'
        }
        $removeRoot = $true
    } finally {
        $transactionLock.Dispose()
    }
    if ($removeRoot) {
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
            $TransactionRoot
        )
    }
    return $false
}

function Invoke-KettlePackageTransactionCleanup {
    param(
        [Parameter(Mandatory = $true)]
        [string] $TransactionRoot,
        [Parameter(Mandatory = $true)]
        [object] $Journal
    )

    $records = @($Journal.files)
    foreach ($treeName in @('stage', 'backup')) {
        $tree = Join-Path $TransactionRoot $treeName
        if (-not (Test-Path -LiteralPath $tree)) {
            continue
        }
        $treeLock = [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($tree)
        $shell = Join-Path $tree 'shell-integration'
        $transactionShellLock = $null
        if (Test-Path -LiteralPath $shell) {
            $transactionShellLock =
                [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($shell)
        }
        try {
            [void](Remove-KettleInstallerTemporarySet -Directory $tree)
            if ($null -ne $transactionShellLock) {
                [void](Remove-KettleInstallerTemporarySet `
                    -Directory $shell)
            }
            $expected = New-Object 'System.Collections.Generic.HashSet[string]' (
                [System.StringComparer]::OrdinalIgnoreCase
            )
            foreach ($record in $records) {
                if ($treeName -ceq 'stage' -or $record.existed) {
                    [void]$expected.Add($record.relative)
                }
            }
            foreach ($item in Get-ChildItem -LiteralPath $tree -Force -Recurse) {
                if (
                    ($item.Attributes -band
                        [System.IO.FileAttributes]::ReparsePoint) -ne 0
                ) {
                    throw 'Installer transaction cleanup found a reparse point.'
                }
                $relative = $item.FullName.Substring($tree.Length).TrimStart(
                    '\',
                    '/'
                ).Replace('\', '/')
                if ($item.PSIsContainer) {
                    if ($relative -cne 'shell-integration') {
                        throw 'Installer transaction cleanup found an unexpected directory.'
                    }
                } elseif (-not $expected.Remove($relative)) {
                    throw "Installer transaction cleanup found an unmanaged file: $relative"
                }
            }
            foreach ($record in $records) {
                if ($treeName -ceq 'stage' -or $record.existed) {
                    [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                        (Join-Path $tree $record.relative.Replace('/', '\'))
                    )
                }
            }
        } finally {
            if ($null -ne $transactionShellLock) {
                $transactionShellLock.Dispose()
            }
            $treeLock.Dispose()
        }
        if (Test-Path -LiteralPath $shell) {
            [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($shell)
        }
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($tree)
    }
    [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
        (Join-Path $TransactionRoot 'journal.json')
    )
}

function Restore-KettlePackageTransaction {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Prefix,
        [Parameter(Mandatory = $true)]
        [string] $TransactionRoot
    )

    $transactionLock =
        [KettleInstaller.NativeFileSystemV1]::LockPrivateDirectory(
            $TransactionRoot
        )
    try {
        [void](Remove-KettleInstallerTemporarySet `
            -Directory $TransactionRoot)
        $journal = Read-KettlePackageJournal `
            -TransactionRoot $TransactionRoot -Prefix $Prefix
        $records = @($journal.files)
        for ($index = [int]$journal.published - 1; $index -ge 0; $index--) {
            $record = $records[$index]
            $destination = Join-Path $Prefix $record.relative.Replace('/', '\')
            if ($record.existed) {
                [KettleInstaller.NativeFileSystemV1]::CopyRegularFileAtomic(
                    (Join-Path $TransactionRoot (
                        'backup\' + $record.relative.Replace('/', '\')
                    )),
                    $destination,
                    536870912
                )
            } else {
                [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                    $destination
                )
            }
            $journal.published = $index
            Write-KettlePackageJournal `
                -TransactionRoot $TransactionRoot -Journal $journal
        }
        [void](Remove-KettleInstallerTemporarySet -Directory $Prefix)
        $prefixShell = Join-Path $Prefix 'shell-integration'
        if (Test-Path -LiteralPath $prefixShell -PathType Container) {
            [void](Remove-KettleInstallerTemporarySet `
                -Directory $prefixShell)
        }
        foreach ($relativeDirectory in @($journal.created_directories)) {
            $createdDirectory = Join-Path $Prefix $relativeDirectory
            if (-not (Test-Path -LiteralPath $createdDirectory)) {
                continue
            }
            $createdDirectoryLock =
                [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                    $createdDirectory
                )
            try {
                if (@(
                    Get-ChildItem -LiteralPath $createdDirectory -Force `
                        -ErrorAction Stop
                ).Count -ne 0) {
                    throw (
                        'Installer rollback could not remove a nonempty ' +
                        "created directory: $relativeDirectory"
                    )
                }
            } finally {
                $createdDirectoryLock.Dispose()
            }
            [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
                $createdDirectory
            )
        }
        Invoke-KettlePackageTransactionCleanup `
            -TransactionRoot $TransactionRoot -Journal $journal
    } finally {
        $transactionLock.Dispose()
    }
    [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($TransactionRoot)
}

function Invoke-KettlePackageTransaction {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Prefix,
        [Parameter(Mandatory = $true)]
        [object[]] $Plan,
        [AllowEmptyString()]
        [string] $TestRoot = ''
    )

    if ($Plan.Count -lt 1 -or $Plan.Count -gt 127) {
        throw 'The installer package plan exceeds its entry limit.'
    }
    $hardKillPhase = ''
    if (
        -not [string]::IsNullOrWhiteSpace($TestRoot) -and
        -not [string]::IsNullOrWhiteSpace(
            $env:KETTLE_INSTALLER_HARD_KILL_PHASE
        )
    ) {
        $hardKillPhase = $env:KETTLE_INSTALLER_HARD_KILL_PHASE
        if ($hardKillPhase -cnotin @(
            'initial-journal',
            'shell-directory',
            'stage',
            'publication-journal',
            'destination',
            'prefix-marker',
            'ownership-marker',
            'after-package-commit'
        )) {
            throw 'Invalid installer hard-kill test phase.'
        }
    }
    $transactionRoot = $Prefix + '.install-transaction'
    if (Test-Path -LiteralPath $transactionRoot) {
        if (Repair-KettlePackageTransactionRoot `
            -Prefix $Prefix -TransactionRoot $transactionRoot) {
            Restore-KettlePackageTransaction `
                -Prefix $Prefix -TransactionRoot $transactionRoot
        }
    }
    [KettleInstaller.NativeFileSystemV1]::CreatePrivateDirectory(
        $transactionRoot
    )
    $transactionLock =
        [KettleInstaller.NativeFileSystemV1]::LockPrivateDirectory(
            $transactionRoot
        )
    $journal = $null
    try {
        $stage = Join-Path $transactionRoot 'stage'
        $backup = Join-Path $transactionRoot 'backup'
        [void][System.IO.Directory]::CreateDirectory($stage)
        [void][System.IO.Directory]::CreateDirectory($backup)
        $stageLock =
            [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($stage)
        $backupLock =
            [KettleInstaller.NativeFileSystemV1]::LockRealDirectory($backup)
        $stageShellLock = $null
        $backupShellLock = $null
        $destinationShellLock = $null
        if (@($Plan | Where-Object {
            $_.Relative -like 'shell-integration/*'
        }).Count -ne 0) {
            $stageShell = Join-Path $stage 'shell-integration'
            $backupShell = Join-Path $backup 'shell-integration'
            [void][System.IO.Directory]::CreateDirectory($stageShell)
            [void][System.IO.Directory]::CreateDirectory($backupShell)
            $stageShellLock =
                [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                    $stageShell
                )
            $backupShellLock =
                [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                    $backupShell
                )
        }
        try {
            $seen = New-Object 'System.Collections.Generic.HashSet[string]' (
                [System.StringComparer]::OrdinalIgnoreCase
            )
            $records = @()
            [uint64]$existingBytes = 0
            [uint64]$sourceBytes = 0
            foreach ($entry in $Plan) {
                $hasSource = (
                    $entry.PSObject.Properties.Name -ccontains 'Source' -and
                    $entry.Source -is [string]
                )
                $hasBytes = (
                    $entry.PSObject.Properties.Name -ccontains 'Bytes' -and
                    $entry.Bytes -is [byte[]]
                )
                if (
                    $hasSource -eq $hasBytes -or
                    $entry.Relative -isnot [string] -or
                    (
                        $entry.Limit -isnot [int] -and
                        $entry.Limit -isnot [long]
                    ) -or
                    [int64]$entry.Limit -lt 0 -or
                    [int64]$entry.Limit -gt 536870912 -or
                    -not (Test-KettleWindowsPayloadRelativePath `
                        -Relative $entry.Relative -AllowInstallMarker) -or
                    -not $seen.Add($entry.Relative)
                ) {
                    throw 'The installer package plan contains an invalid entry.'
                }
                $sourceLength = if ($hasSource) {
                    $sourceItem = Assert-KettleOrdinaryInstallFile `
                        -Path $entry.Source -Label 'Installer package source'
                    [uint64]$sourceItem.Length
                } else {
                    [uint64]$entry.Bytes.LongLength
                }
                $hasExpectedSize = (
                    $entry.PSObject.Properties.Name -ccontains 'ExpectedSize'
                )
                $hasExpectedHash = (
                    $entry.PSObject.Properties.Name -ccontains 'ExpectedSha256'
                )
                if (
                    $hasExpectedSize -ne $hasExpectedHash -or
                    ($hasBytes -and $hasExpectedSize) -or
                    (
                        $hasExpectedSize -and (
                            (
                                $entry.ExpectedSize -isnot [int] -and
                                $entry.ExpectedSize -isnot [long] -and
                                $entry.ExpectedSize -isnot [uint64]
                            ) -or
                            $sourceLength -ne [uint64]$entry.ExpectedSize -or
                            $entry.ExpectedSha256 -isnot [string] -or
                            $entry.ExpectedSha256 -cnotmatch '^[0-9a-f]{64}$'
                        )
                    )
                ) {
                    throw 'A release package source changed after manifest verification.'
                }
                $sourceBytes += $sourceLength
                if (
                    $sourceLength -gt [uint64]$entry.Limit -or
                    $sourceBytes -gt 536870912
                ) {
                    throw 'Installer package sources exceed their safety limits.'
                }
                $destination = Join-Path $Prefix (
                    $entry.Relative.Replace('/', '\')
                )
                $existed = Test-Path -LiteralPath $destination -PathType Leaf
                if (Test-Path -LiteralPath $destination) {
                    $existing = Assert-KettleOrdinaryInstallFile `
                        -Path $destination -Label 'Managed install destination'
                    $existingBytes += [uint64]$existing.Length
                    if ($existingBytes -gt 536870912) {
                        throw 'Existing package backups exceed the aggregate size limit.'
                    }
                }
                $records += [ordered]@{
                    relative = $entry.Relative
                    existed = [bool]$existed
                }
            }
            $hasDestinationShell = @(
                $records | Where-Object {
                    $_.relative -like 'shell-integration/*'
                }
            ).Count -ne 0
            $destinationShell = Join-Path $Prefix 'shell-integration'
            $destinationShellExisted = (
                $hasDestinationShell -and
                (Test-Path -LiteralPath $destinationShell -PathType Container)
            )
            if (
                $hasDestinationShell -and
                (Test-Path -LiteralPath $destinationShell) -and
                -not $destinationShellExisted
            ) {
                throw 'The managed shell-integration path is not a real directory.'
            }
            $journal = [ordered]@{
                schema = 2
                product = 'kettle-installer'
                prefix = $Prefix
                published = 0
                created_directories = @(
                    if ($hasDestinationShell -and -not $destinationShellExisted) {
                        'shell-integration'
                    }
                )
                files = $records
            }
            Write-KettlePackageJournal `
                -TransactionRoot $transactionRoot -Journal $journal `
                -HardKillAfterTemporaryFlush:(
                    $hardKillPhase -ceq 'initial-journal'
                )
            if ($hasDestinationShell) {
                if (-not (Test-Path -LiteralPath $destinationShell)) {
                    [void][System.IO.Directory]::CreateDirectory(
                        $destinationShell
                    )
                }
                $destinationShellLock =
                    [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                        $destinationShell
                    )
                if ($hardKillPhase -ceq 'shell-directory') {
                    [KettleInstaller.NativeFileSystemV1]::HardTerminateInstallerForTesting()
                }
            }
            $stageIndex = 0
            foreach ($entry in $Plan) {
                $stageDestination = Join-Path $stage (
                    $entry.Relative.Replace('/', '\')
                )
                $stageParent = Split-Path $stageDestination -Parent
                if (-not (Test-Path -LiteralPath $stageParent)) {
                    [void][System.IO.Directory]::CreateDirectory($stageParent)
                }
                $terminateStage = (
                    $hardKillPhase -ceq 'stage' -and
                    $stageIndex -eq 0
                )
                if (
                    $entry.PSObject.Properties.Name -ccontains 'Bytes'
                ) {
                    [KettleInstaller.NativeFileSystemV1]::WriteBytesAtomic(
                        $stageDestination,
                        $entry.Bytes,
                        $terminateStage
                    )
                } else {
                    [KettleInstaller.NativeFileSystemV1]::CopyRegularFileAtomic(
                        $entry.Source,
                        $stageDestination,
                        [long]$entry.Limit,
                        $terminateStage
                    )
                }
                $stageIndex++
                if (
                    $entry.PSObject.Properties.Name -ccontains
                        'ExpectedSha256'
                ) {
                    $stagedDigest = (
                        Get-FileHash -LiteralPath $stageDestination `
                            -Algorithm SHA256
                    ).Hash.ToLowerInvariant()
                    if ($stagedDigest -cne $entry.ExpectedSha256) {
                        throw "Release package source changed while staging: $($entry.Relative)"
                    }
                }
            }
            foreach ($record in $records) {
                if (-not $record.existed) {
                    continue
                }
                $backupDestination = Join-Path $backup (
                    $record.relative.Replace('/', '\')
                )
                $backupParent = Split-Path $backupDestination -Parent
                if (-not (Test-Path -LiteralPath $backupParent)) {
                    [void][System.IO.Directory]::CreateDirectory($backupParent)
                }
                [KettleInstaller.NativeFileSystemV1]::CopyRegularFileAtomic(
                    (Join-Path $Prefix $record.relative.Replace('/', '\')),
                    $backupDestination,
                    536870912
                )
            }
            $faultAfter = 0
            $faultAfterJournal = 0
            if (
                -not [string]::IsNullOrWhiteSpace($TestRoot) -and
                -not [string]::IsNullOrWhiteSpace(
                    $env:KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS
                )
            ) {
                if (-not [int]::TryParse(
                    $env:KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS,
                    [ref]$faultAfter
                )) {
                    throw 'Invalid installer publication fault checkpoint.'
                }
            }
            if (
                -not [string]::IsNullOrWhiteSpace($TestRoot) -and
                -not [string]::IsNullOrWhiteSpace(
                    $env:KETTLE_INSTALLER_FAULT_AFTER_JOURNAL
                )
            ) {
                if (-not [int]::TryParse(
                    $env:KETTLE_INSTALLER_FAULT_AFTER_JOURNAL,
                    [ref]$faultAfterJournal
                )) {
                    throw 'Invalid installer journal fault checkpoint.'
                }
            }
            for ($index = 0; $index -lt $records.Count; $index++) {
                $record = $records[$index]
                $destination = Join-Path $Prefix (
                    $record.relative.Replace('/', '\')
                )
                $destinationParent = Split-Path $destination -Parent
                if (-not (Test-Path -LiteralPath $destinationParent)) {
                    [void][System.IO.Directory]::CreateDirectory($destinationParent)
                }
                # Record rollback coverage durably before the destination
                # mutation. Recovery may safely restore/delete an unchanged
                # entry if the process stops before the copy begins.
                $journal.published = $index + 1
                Write-KettlePackageJournal `
                    -TransactionRoot $transactionRoot -Journal $journal `
                    -HardKillAfterTemporaryFlush:(
                        $hardKillPhase -ceq 'publication-journal' -and
                        $index -eq 0
                    )
                if ($faultAfterJournal -eq $journal.published) {
                    throw (
                        'Injected installer journal failure before ' +
                        "publication $faultAfterJournal."
                    )
                }
                [KettleInstaller.NativeFileSystemV1]::CopyRegularFileAtomic(
                    (Join-Path $stage $record.relative.Replace('/', '\')),
                    $destination,
                    536870912,
                    (
                        $hardKillPhase -ceq 'destination' -and
                        $index -eq 0
                    )
                )
                if (
                    (
                        $hardKillPhase -ceq 'prefix-marker' -and
                        $record.relative -ceq '.kettle-install-prefix'
                    ) -or (
                        $hardKillPhase -ceq 'ownership-marker' -and
                        $record.relative -ceq '.kettle-install.json'
                    )
                ) {
                    # Unlike the generic destination seam, marker seams fire
                    # only after MoveFileExW has atomically published the
                    # selected marker.
                    [KettleInstaller.NativeFileSystemV1]::HardTerminateInstallerForTesting()
                }
                if ($faultAfter -eq $journal.published) {
                    throw "Injected installer publication failure after $faultAfter files."
                }
            }
        } finally {
            if ($null -ne $destinationShellLock) {
                $destinationShellLock.Dispose()
            }
            if ($null -ne $backupShellLock) { $backupShellLock.Dispose() }
            if ($null -ne $stageShellLock) { $stageShellLock.Dispose() }
            if ($null -ne $backupLock) { $backupLock.Dispose() }
            if ($null -ne $stageLock) { $stageLock.Dispose() }
        }
        Invoke-KettlePackageTransactionCleanup `
            -TransactionRoot $transactionRoot -Journal $journal
        $transactionLock.Dispose()
        $transactionLock = $null
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
            $transactionRoot
        )
    } catch {
        $publicationError = $_
        if (
            -not [string]::IsNullOrWhiteSpace($TestRoot) -and
            $env:KETTLE_INSTALLER_TEST_LEAVE_TRANSACTION -ceq '1'
        ) {
            $transactionLock.Dispose()
            $transactionLock = $null
            throw $publicationError
        }
        if (
            $null -ne $journal -and
            (Test-Path -LiteralPath (Join-Path $transactionRoot 'journal.json'))
        ) {
            try {
                $transactionLock.Dispose()
                $transactionLock = $null
                Restore-KettlePackageTransaction `
                    -Prefix $Prefix -TransactionRoot $transactionRoot
            } catch {
                throw (
                    $publicationError.Exception.Message +
                    '; package rollback also failed: ' +
                    $_.Exception.Message
                )
            }
        }
        throw $publicationError
    } finally {
        if ($null -ne $transactionLock) {
            $transactionLock.Dispose()
        }
    }
}

function Get-KettleUpdateJournal {
    param(
        [Parameter(Mandatory = $true)]
        [string] $InstallRoot
    )

    $journalPath = Join-Path $InstallRoot '.kettle-update-journal.json'
    if (-not (Test-Path -LiteralPath $journalPath -PathType Leaf)) {
        throw 'An updater backup directory exists without its transaction journal.'
    }
    $raw = Read-KettleStrictUtf8File `
        -Path $journalPath -MaximumBytes 1048576 `
        -Label 'Update transaction journal'
    try {
        $journal = $raw | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw 'The update transaction journal is not valid bounded JSON.'
    }
    $entries = @($journal.entries)
    if (
        (
            $journal.schema -isnot [int] -and
            $journal.schema -isnot [long]
        ) -or
        $journal.schema -ne 2 -or
        $journal.transaction_id -isnot [string] -or
        -not (Test-KettleUpdateTransactionId `
            -Value $journal.transaction_id) -or
        $journal.backup_dir -isnot [string] -or
        $journal.backup_dir -cne (
            '.kettle-update-backup-' + $journal.transaction_id
        ) -or
        $entries.Count -gt 128
    ) {
        throw 'The update transaction journal has an invalid artifact identity.'
    }
    return $journal
}

function Assert-KettleUpdateArtifactDirectory {
    param(
        [Parameter(Mandatory = $true)]
        [string] $InstallRoot,
        [Parameter(Mandatory = $true)]
        [System.IO.FileSystemInfo] $Directory,
        [Parameter(Mandatory = $true)]
        [ref] $Seen,
        [Parameter(Mandatory = $true)]
        [ref] $TotalBytes
    )

    $isBackup = $Directory.Name.StartsWith(
        '.kettle-update-backup-',
        [StringComparison]::Ordinal
    )
    $transactionId = $Directory.Name.Substring(
        $Directory.Name.LastIndexOf('-') + 1
    )
    # LastIndexOf only returns the nanoseconds component. Recover the exact
    # `PID-nanoseconds` suffix from the known fixed prefix instead.
    $artifactPrefix = if ($isBackup) {
        '.kettle-update-backup-'
    } else {
        '.kettle-update-stage-'
    }
    $transactionId = $Directory.Name.Substring($artifactPrefix.Length)
    $expectedBackup = $null
    $backupEntries = @{}
    $orphanBackup = $false
    if ($isBackup) {
        $expectedBackup =
            New-Object 'System.Collections.Generic.HashSet[string]' (
                [System.StringComparer]::OrdinalIgnoreCase
            )
        [void]$expectedBackup.Add('.kettle-update-backup.json')
        $journalPath = Join-Path $InstallRoot `
            '.kettle-update-journal.json'
        if (Test-Path -LiteralPath $journalPath -PathType Leaf) {
            $journal = Get-KettleUpdateJournal -InstallRoot $InstallRoot
            if ($journal.transaction_id -cne $transactionId) {
                throw 'Updater backup directory does not match its transaction journal.'
            }
            foreach ($entry in @($journal.entries)) {
                if (
                    $entry.existed -isnot [bool] -or
                    $entry.relative -isnot [string]
                ) {
                    throw 'The update journal contains an invalid backup entry.'
                }
                if ($entry.existed) {
                    if (-not (Test-KettleWindowsPayloadRelativePath `
                        -Relative $entry.relative -AllowInstallMarker)) {
                        throw "The update journal names an unmanaged backup: $($entry.relative)"
                    }
                    if (-not $expectedBackup.Add(
                        $entry.relative.Replace('\', '/')
                    )) {
                        throw 'The update journal contains duplicate backup paths.'
                    }
                    if (
                        $entry.previous_size -isnot [int] -and
                        $entry.previous_size -isnot [long]
                    ) {
                        throw 'The update journal contains an invalid backup size.'
                    }
                    if (
                        $entry.previous_sha256 -isnot [string] -or
                        $entry.previous_sha256 -cnotmatch '^[0-9a-fA-F]{64}$'
                    ) {
                        throw 'The update journal contains an invalid backup hash.'
                    }
                    $backupEntries[
                        $entry.relative.Replace('\', '/').ToLowerInvariant()
                    ] = $entry
                }
            }
        } else {
            $orphanBackup = $true
        }
        $markerPath = Join-Path $Directory.FullName `
            '.kettle-update-backup.json'
        $markerRaw = Read-KettleStrictUtf8File `
            -Path $markerPath -MaximumBytes 4096 `
            -Label 'Update backup marker'
        try {
            $marker = $markerRaw | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw 'The update backup marker is not valid bounded JSON.'
        }
        if (
            (
                $marker.schema -isnot [int] -and
                $marker.schema -isnot [long]
            ) -or
            $marker.schema -ne 2 -or
            $marker.product -isnot [string] -or
            $marker.product -cne 'kettle' -or
            $marker.transaction_id -isnot [string] -or
            $marker.transaction_id -cne $transactionId
        ) {
            throw 'The update backup marker does not match its transaction.'
        }
    }

    $directoryLock =
        [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
            $Directory.FullName
        )
    try {
        $files = @(
            Get-ChildItem -LiteralPath $Directory.FullName -Recurse -Force `
                -ErrorAction Stop
        )
        foreach ($item in $files) {
            $Seen.Value++
            if ($Seen.Value -gt 128) {
                throw 'The managed install tree exceeds its 128-entry limit.'
            }
            if (
                ($item.Attributes -band
                    [System.IO.FileAttributes]::ReparsePoint) -ne 0
            ) {
                throw "Updater artifact contains a reparse point: $($item.FullName)"
            }
            $relative = $item.FullName.Substring(
                $Directory.FullName.Length
            ).TrimStart('\', '/').Replace('\', '/')
            if ($item.PSIsContainer) {
                if (
                    $relative -cne 'shell-integration' -or
                    $item.Parent.FullName -cne $Directory.FullName
                ) {
                    throw "Updater artifact contains an unmanaged directory: $relative"
                }
                continue
            }
            $TotalBytes.Value += [long]$item.Length
            if ($TotalBytes.Value -gt 536870912) {
                throw 'The managed install tree exceeds 512 MiB.'
            }
            if ($isBackup) {
                if ($orphanBackup) {
                    if (
                        $relative -cne '.kettle-update-backup.json' -and
                        -not (Test-KettleWindowsPayloadRelativePath `
                            -Relative $relative -AllowInstallMarker)
                    ) {
                        throw "Orphan updater backup contains an unmanaged file: $relative"
                    }
                } elseif (-not $expectedBackup.Remove($relative)) {
                    throw "Updater backup contains an unjournaled file: $relative"
                } elseif ($relative -cne '.kettle-update-backup.json') {
                    $entry = $backupEntries[$relative.ToLowerInvariant()]
                    if (
                        [long]$entry.previous_size -ne [long]$item.Length -or
                        (Get-FileHash -LiteralPath $item.FullName `
                            -Algorithm SHA256).Hash -cne (
                                $entry.previous_sha256.ToUpperInvariant()
                            )
                    ) {
                        throw "Updater backup failed its journal hash: $relative"
                    }
                }
            } elseif (
                -not (Test-KettleWindowsPayloadRelativePath `
                    -Relative $relative)
            ) {
                throw "Updater staging contains an unmanaged file: $relative"
            }
        }
        if (
            $isBackup -and
            -not $orphanBackup -and
            $expectedBackup.Count -ne 0
        ) {
            throw 'Updater backup does not exactly cover its journal.'
        }
    } finally {
        $directoryLock.Dispose()
    }
}

function Assert-KettleManagedInstallTree {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $rootItems = @(
        Get-ChildItem -LiteralPath $Path -Force -ErrorAction Stop
    )
    if ($rootItems.Count -gt 128) {
        throw 'The install tree exceeds its 128-entry root limit.'
    }
    $seen = 0
    $totalBytes = [long]0
    $backupDirectories = 0
    $stageDirectories = 0
    $failedTransactions =
        New-Object 'System.Collections.Generic.HashSet[string]' (
            [System.StringComparer]::Ordinal
        )
    foreach ($item in $rootItems) {
        $seen++
        if (
            ($item.Attributes -band
                [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "The install tree contains a reparse point: $($item.FullName)"
        }
        if ($item.PSIsContainer) {
            if ($item.Name -ceq 'shell-integration') {
                $shellItems = @(
                    Get-ChildItem -LiteralPath $item.FullName -Force `
                        -ErrorAction Stop
                )
                if ($shellItems.Count -gt 4) {
                    throw 'The managed shell-integration directory exceeds four entries.'
                }
                foreach ($shellItem in $shellItems) {
                    $seen++
                    if (
                        $shellItem.PSIsContainer -or
                        ($shellItem.Attributes -band
                            [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
                        @(
                            'kettle.bash',
                            'kettle.fish',
                            'kettle.ps1',
                            'kettle.zsh'
                        ) -cnotcontains $shellItem.Name
                    ) {
                        throw "The shell-integration tree contains an unmanaged entry: $($shellItem.Name)"
                    }
                    $totalBytes += [long]$shellItem.Length
                }
            } elseif (
                Test-KettleUpdateArtifactDirectoryName -Name $item.Name
            ) {
                if ($item.Name.StartsWith(
                    '.kettle-update-backup-',
                    [StringComparison]::Ordinal
                )) {
                    $backupDirectories++
                } else {
                    $stageDirectories++
                }
                Assert-KettleUpdateArtifactDirectory `
                    -InstallRoot $Path -Directory $item `
                    -Seen ([ref]$seen) -TotalBytes ([ref]$totalBytes)
            } else {
                throw "The install tree contains an unmanaged directory: $($item.Name)"
            }
        } else {
            if (-not (Test-KettleManagedRootFileName -Name $item.Name)) {
                throw "The install tree contains an unmanaged file: $($item.Name)"
            }
            if (
                $item.Name -cmatch
                    '^\.kettle-update-failed-(.+)\.(?:json|txt)$'
            ) {
                $failedTransactionId = $Matches[1]
                if (-not (Test-KettleUpdateTransactionId `
                    -Value $failedTransactionId)) {
                    throw "Failed-update evidence has an invalid transaction id: $($item.Name)"
                }
                [void]$failedTransactions.Add($failedTransactionId)
                if ($item.Length -gt 1048576) {
                    throw "Failed-update evidence exceeds 1 MiB: $($item.Name)"
                }
            }
            $totalBytes += [long]$item.Length
        }
    }
    $pendingPath = Join-Path $Path '.kettle-update-pending.json'
    if (Test-Path -LiteralPath $pendingPath -PathType Leaf) {
        $pendingRaw = Read-KettleStrictUtf8File `
            -Path $pendingPath -MaximumBytes 1048576 `
            -Label 'Pending update record'
        try {
            $pending = $pendingRaw | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw 'The pending update record is not valid bounded JSON.'
        }
        $pendingKeys = @($pending.PSObject.Properties.Name)
        $expectedPendingKeys = @(
            'schema',
            'product',
            'target',
            'transaction_id',
            'target_version',
            'archive',
            'archive_size',
            'archive_sha256',
            'release_manifest',
            'release_signature',
            'asset',
            'package_manifest',
            'helper',
            'helper_size',
            'helper_sha256',
            'attempts',
            'handoff_timeouts',
            'last_error'
        )
        if (
            $pending -isnot [System.Management.Automation.PSCustomObject] -or
            $pendingKeys.Count -ne $expectedPendingKeys.Count -or
            @($pendingKeys | Where-Object {
                $expectedPendingKeys -cnotcontains $_
            }).Count -ne 0 -or
            (
                $pending.schema -isnot [int] -and
                $pending.schema -isnot [long]
            ) -or
            $pending.schema -ne 3 -or
            $pending.product -isnot [string] -or
            $pending.product -cne 'kettle' -or
            $pending.target -isnot [string] -or
            $pending.target -cne 'x86_64-pc-windows-msvc' -or
            $pending.transaction_id -isnot [string] -or
            -not (Test-KettleUpdateTransactionId `
                -Value $pending.transaction_id) -or
            $pending.target_version -isnot [string] -or
            $pending.target_version.Length -gt 256 -or
            $pending.target_version -cnotmatch (
                '^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.' +
                '(?:0|[1-9][0-9]*)' +
                '(?:-(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-]' +
                '[0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|' +
                '[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?' +
                '(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
            ) -or
            $pending.archive -isnot [string] -or
            $pending.archive -cne (
                '.kettle-update-archive-' +
                $pending.transaction_id +
                '.zip'
            ) -or
            (
                $pending.archive_size -isnot [int] -and
                $pending.archive_size -isnot [long]
            ) -or
            $pending.archive_size -lt 1 -or
            $pending.archive_size -gt 268435456 -or
            $pending.archive_sha256 -isnot [string] -or
            $pending.archive_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
            $pending.release_manifest -isnot [string] -or
            $pending.release_manifest.Length -lt 1 -or
            $pending.release_manifest.Length -gt 131072 -or
            $pending.release_manifest.IndexOf([char]0) -ge 0 -or
            $pending.release_signature -isnot [string] -or
            $pending.release_signature -cnotmatch '^[A-Za-z0-9+/]{86}==$' -or
            $pending.asset -isnot [System.Management.Automation.PSCustomObject] -or
            $pending.package_manifest -isnot [string] -or
            $pending.package_manifest.Length -lt 1 -or
            $pending.package_manifest.Length -gt 262144 -or
            $pending.package_manifest.IndexOf([char]0) -ge 0 -or
            $pending.helper -isnot [string] -or
            $pending.helper -cne (
                '.kettle-update-helper-' +
                $pending.transaction_id +
                '.exe'
            ) -or
            (
                $pending.helper_size -isnot [int] -and
                $pending.helper_size -isnot [long]
            ) -or
            $pending.helper_size -lt 1 -or
            $pending.helper_size -gt 536870912 -or
            $pending.helper_sha256 -isnot [string] -or
            $pending.helper_sha256 -cnotmatch '^[0-9a-fA-F]{64}$' -or
            (
                $pending.attempts -isnot [int] -and
                $pending.attempts -isnot [long]
            ) -or
            $pending.attempts -lt 0 -or
            $pending.attempts -gt [uint32]::MaxValue -or
            (
                $pending.handoff_timeouts -isnot [int] -and
                $pending.handoff_timeouts -isnot [long]
            ) -or
            $pending.handoff_timeouts -lt 0 -or
            $pending.handoff_timeouts -gt [uint32]::MaxValue -or
            (
                $null -ne $pending.last_error -and
                $pending.last_error -isnot [string]
            )
        ) {
            throw 'The pending update record has an invalid artifact identity.'
        }
        $assetKeys = @($pending.asset.PSObject.Properties.Name)
        if (
            $assetKeys.Count -ne 4 -or
            @($assetKeys | Where-Object {
                @('target', 'name', 'size', 'sha256') -cnotcontains $_
            }).Count -ne 0 -or
            $pending.asset.target -isnot [string] -or
            $pending.asset.target -cne $pending.target -or
            $pending.asset.name -isnot [string] -or
            $pending.asset.name -cnotmatch (
                '^[A-Za-z0-9][A-Za-z0-9._-]{0,250}\.zip$'
            ) -or
            (
                $pending.asset.size -isnot [int] -and
                $pending.asset.size -isnot [long]
            ) -or
            $pending.asset.size -ne $pending.archive_size -or
            $pending.asset.sha256 -isnot [string] -or
            $pending.asset.sha256 -cne $pending.archive_sha256
        ) {
            throw 'The pending update record contains an invalid signed asset identity.'
        }
        try {
            $pendingPackage =
                $pending.package_manifest | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw 'The pending update record contains an invalid package manifest.'
        }
        $packageKeys = @($pendingPackage.PSObject.Properties.Name)
        if (
            $pendingPackage -isnot [System.Management.Automation.PSCustomObject] -or
            $packageKeys.Count -ne 5 -or
            @($packageKeys | Where-Object {
                @('schema', 'product', 'target', 'version', 'files') `
                    -cnotcontains $_
            }).Count -ne 0 -or
            (
                $pendingPackage.schema -isnot [int] -and
                $pendingPackage.schema -isnot [long]
            ) -or
            $pendingPackage.schema -ne 1 -or
            $pendingPackage.product -isnot [string] -or
            $pendingPackage.product -cne 'kettle' -or
            $pendingPackage.target -isnot [string] -or
            $pendingPackage.target -cne $pending.target -or
            $pendingPackage.version -isnot [string] -or
            $pendingPackage.version -cne $pending.target_version -or
            $pendingPackage.files -isnot [System.Array] -or
            $pendingPackage.files.Count -lt 1 -or
            $pendingPackage.files.Count -gt 127
        ) {
            throw 'The pending update record contains an invalid package identity.'
        }
        $pendingPaths =
            New-Object 'System.Collections.Generic.HashSet[string]' (
                [System.StringComparer]::OrdinalIgnoreCase
            )
        $lastPendingPath = $null
        [uint64]$pendingBytes = 0
        foreach ($record in $pendingPackage.files) {
            $recordKeys = @($record.PSObject.Properties.Name)
            if (
                $record -isnot [System.Management.Automation.PSCustomObject] -or
                $recordKeys.Count -ne 4 -or
                @($recordKeys | Where-Object {
                    @('path', 'size', 'sha256', 'mode') -cnotcontains $_
                }).Count -ne 0 -or
                $record.path -isnot [string] -or
                $record.path -ceq 'kettle-package-manifest.json' -or
                $record.path.Contains('\') -or
                -not (Test-KettleWindowsPayloadRelativePath `
                    -Relative $record.path) -or
                -not $pendingPaths.Add($record.path) -or
                (
                    $record.size -isnot [int] -and
                    $record.size -isnot [long]
                ) -or
                $record.size -lt 0 -or
                $record.size -gt 536870912 -or
                $record.sha256 -isnot [string] -or
                $record.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
                $null -ne $record.mode -or
                (
                    $null -ne $lastPendingPath -and
                    [StringComparer]::Ordinal.Compare(
                        $lastPendingPath,
                        $record.path
                    ) -ge 0
                )
            ) {
                throw 'The pending update record contains an invalid file identity.'
            }
            $pendingBytes += [uint64]$record.size
            if ($pendingBytes -gt 536870912) {
                throw 'The pending update record exceeds its package-byte limit.'
            }
            $lastPendingPath = $record.path
        }
        # Do not require the named leaves to exist here. The managed remover
        # deliberately revalidates the root after deleting each updater
        # directory, and a crash can likewise leave a structurally valid
        # pending record after one artifact has already disappeared. Every
        # extant root entry is independently constrained above; retaining the
        # identity checks while tolerating absent leaves lets uninstall finish
        # without broadening what it may delete.
    }
    if (
        $failedTransactions.Count -gt 8 -or
        $seen -gt 128 -or
        $totalBytes -gt 536870912
    ) {
        throw 'The managed install tree exceeds its bounded size.'
    }
}

function Invoke-KettleManagedInstallTreeRemoval {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    Assert-KettleManagedInstallTree -Path $Path
    foreach ($artifact in @(
        Get-ChildItem -LiteralPath $Path -Directory -Force -ErrorAction Stop |
            Where-Object {
                Test-KettleUpdateArtifactDirectoryName -Name $_.Name
            }
    )) {
        $artifactLock =
            [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                $artifact.FullName
            )
        try {
            Assert-KettleManagedInstallTree -Path $Path
            $nestedShell = Join-Path $artifact.FullName 'shell-integration'
            if (Test-Path -LiteralPath $nestedShell -PathType Container) {
                $nestedLock =
                    [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
                        $nestedShell
                    )
                try {
                    foreach ($nestedFile in @(
                        Get-ChildItem -LiteralPath $nestedShell -Force `
                            -ErrorAction Stop
                    )) {
                        if (
                            $nestedFile.PSIsContainer -or
                            @(
                                'kettle.bash',
                                'kettle.fish',
                                'kettle.ps1',
                                'kettle.zsh'
                            ) -cnotcontains $nestedFile.Name
                        ) {
                            throw "Refusing to delete unmanaged updater entry: $($nestedFile.FullName)"
                        }
                        [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                            $nestedFile.FullName
                        )
                    }
                } finally {
                    $nestedLock.Dispose()
                }
                [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
                    $nestedShell
                )
            }
            foreach ($artifactFile in @(
                Get-ChildItem -LiteralPath $artifact.FullName -Force `
                    -ErrorAction Stop
            )) {
                if ($artifactFile.PSIsContainer) {
                    throw "Refusing to recursively delete updater directory: $($artifactFile.FullName)"
                }
                [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                    $artifactFile.FullName
                )
            }
        } finally {
            $artifactLock.Dispose()
        }
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
            $artifact.FullName
        )
    }

    $shellPath = Join-Path $Path 'shell-integration'
    if (Test-Path -LiteralPath $shellPath) {
        $shellLock = [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
            $shellPath
        )
        try {
            Assert-KettleManagedInstallTree -Path $Path
            foreach ($name in @(
                'kettle.bash',
                'kettle.fish',
                'kettle.ps1',
                'kettle.zsh'
            )) {
                [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                    (Join-Path $shellPath $name)
                )
            }
        } finally {
            $shellLock.Dispose()
        }
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($shellPath)
    }

    $remaining = @(
        Get-ChildItem -LiteralPath $Path -Force -ErrorAction Stop
    )
    if ($remaining.Count -gt 128) {
        throw 'The install root changed beyond its bounded entry limit.'
    }
    foreach ($item in $remaining) {
        if ($item.PSIsContainer) {
            throw "Refusing to recursively delete unexpected directory: $($item.Name)"
        }
        if (-not (Test-KettleManagedRootFileName -Name $item.Name)) {
            throw "Refusing to delete unmanaged file: $($item.Name)"
        }
        if (
            $item.Name -ceq '.kettle-update.lock' -or
            $item.Name -ceq '.kettle-running.lock'
        ) {
            continue
        }
        [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf($item.FullName)
    }
}

# Detect the layout: extracted-zip mode keeps `kettle.exe` next to
# this script; in-repo mode has it under `target/release/`.
$scriptDir = [System.IO.Path]::GetFullPath(
    (Split-Path -Parent $MyInvocation.MyCommand.Definition)
).TrimEnd('\', '/')
$prefixMarker = Join-Path $scriptDir ".kettle-install-prefix"
$prefixFromMarker = $false
if (
    -not $PSBoundParameters.ContainsKey('Prefix') -and
    (Test-Path -LiteralPath $prefixMarker)
) {
    $Prefix = Read-KettleInstallPrefixMarker -Path $prefixMarker
    $prefixFromMarker = $true
}
$zipModeExe = Join-Path $scriptDir "kettle.exe"
$repoModeExe = Join-Path (Split-Path -Parent $scriptDir) "target\release\kettle.exe"
$installedSourceChannel = $null
$releaseManifest = $null

if (Test-Path -LiteralPath $prefixMarker -PathType Leaf) {
    if (-not (Test-Path -LiteralPath (Join-Path $scriptDir '.kettle-install.json') -PathType Leaf)) {
        throw 'The installed helper has an incomplete ownership marker set.'
    }
    $installedPrefix = Read-KettleInstallPrefixMarker -Path $prefixMarker
    if (-not (Test-KettleInstallPathEqual $installedPrefix $scriptDir)) {
        throw 'The installed prefix marker does not name its helper directory.'
    }
    Assert-KettleInstallOwnership -Path $scriptDir
    $installedOwnership = (
        Read-KettleStrictUtf8File `
            -Path (Join-Path $scriptDir '.kettle-install.json') `
            -MaximumBytes 4096 -Label 'Install ownership marker'
    ) | ConvertFrom-Json -ErrorAction Stop
    $installedSourceChannel = $installedOwnership.channel
    $sourceMode = 'installed'
    $sourceDir = $scriptDir
    $sourceExe = $zipModeExe
} elseif (Test-Path $zipModeExe) {
    $releaseManifest = Read-KettleVerifiedReleaseManifest `
        -PackageRoot $scriptDir
    $sourceMode = 'zip'
    $sourceDir = $scriptDir
    $sourceExe = $zipModeExe
} elseif (Test-Path $repoModeExe) {
    $sourceMode = 'repo'
    $sourceDir = Split-Path -Parent $scriptDir   # repo root
    $sourceExe = $repoModeExe
} else {
    $sourceMode = $null
}

$integrationTest = -not [string]::IsNullOrWhiteSpace($IntegrationTestRoot)
if ($integrationTest) {
    # The Windows installer smoke uses isolated filesystem and registry roots to
    # exercise the real default-install path without touching the developer's
    # installed app, Start menu, PATH, or Add/Remove Programs entry.
    $IntegrationTestRoot = ConvertTo-KettleInstallPath `
        -Path $IntegrationTestRoot -Context 'Integration test root'
    $testDefaultPrefix = Join-Path $IntegrationTestRoot "Programs\kettle"
    if (-not $PSBoundParameters.ContainsKey('Prefix') -and -not $prefixFromMarker) {
        $Prefix = $testDefaultPrefix
    }
    $startMenuDir = Join-Path $IntegrationTestRoot "Start Menu\Programs"
    $uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle-installer-smoke-$PID"
    $profilePath = Join-Path $IntegrationTestRoot "WindowsPowerShell\profile.ps1"
    $NoPath = $true
} else {
    $startMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    $uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle"
    $profilePath = $PROFILE
}
$Prefix = Assert-KettleSafeInstallPrefix `
    -Path $Prefix -TestRoot $IntegrationTestRoot
if (
    $prefixFromMarker -and
    -not (Test-KettleInstallPathEqual $scriptDir $Prefix)
) {
    throw 'The installed prefix marker does not name its helper directory.'
}
$defaultPrefix = if ($integrationTest) {
    [System.IO.Path]::GetFullPath($testDefaultPrefix).TrimEnd('\', '/')
} else {
    [System.IO.Path]::GetFullPath(
        (Join-Path $env:LOCALAPPDATA "Programs\kettle")
    ).TrimEnd('\', '/')
}
$portable = -not (Test-KettleInstallPathEqual $Prefix $defaultPrefix)
$shortcutPath = Join-Path $startMenuDir "kettle.lnk"

function Invoke-KettleUserPathEdit {
    param([string] $Dir, [switch] $Remove)
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($null -eq $current) { $current = '' }
    # Split + filter exact-match (case-insensitive) so we don't strip a
    # superstring entry by accident.
    $parts = $current -split ';' | Where-Object { $_ -ne '' }
    $without = $parts | Where-Object { $_ -ne $Dir }
    if ($Remove) {
        if ($without.Count -eq $parts.Count) { return $false }  # nothing to remove
        $new = ($without -join ';')
    } else {
        if ($without.Count -ne $parts.Count) { return $false }  # already present
        $new = (@($without) + $Dir) -join ';'
    }
    [Environment]::SetEnvironmentVariable("Path", $new, "User")
    return $true
}

if ($Uninstall) {
    if (
        -not $portable -and
        (Test-Path -LiteralPath $profilePath -PathType Leaf)
    ) {
        $profilePreflightChain =
            [KettleInstaller.NativeFileSystemV1]::LockRealDirectoryChain(
                (Split-Path $profilePath -Parent)
            )
        try {
            $profileSnapshot =
                [KettleInstaller.NativeFileSystemV1]::CaptureProfile(
                    $profilePath,
                    4194304
                )
            try {
                $profilePreflight = Get-KettleProfileDocument `
                    -Path $profilePath -Bytes $profileSnapshot.Bytes
                [void](Get-KettleManagedProfileBlock -Text $profilePreflight.Text)
            } finally {
                $profileSnapshot.Dispose()
            }
        } finally {
            $profilePreflightChain.Dispose()
        }
    }
    Write-Output "Removing kettle..."
    if (Test-Path -LiteralPath $Prefix -PathType Container) {
        $prefixChain =
            [KettleInstaller.NativeFileSystemV1]::LockRealDirectoryChain(
            $Prefix
        )
        try {
            $prefixChain.Verify()
            Assert-KettleInstallOwnership -Path $Prefix
            $updateLock =
                [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
                    (Join-Path $Prefix '.kettle-update.lock')
                )
            try {
                $runningLock =
                    [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
                        (Join-Path $Prefix '.kettle-running.lock')
                )
                try {
                    Assert-KettleInstallOwnership -Path $Prefix
                    [void](Remove-KettleInstallerTemporarySet `
                        -Directory $Prefix)
                    $uninstallShell = Join-Path $Prefix 'shell-integration'
                    if (
                        Test-Path -LiteralPath $uninstallShell `
                            -PathType Container
                    ) {
                        [void](Remove-KettleInstallerTemporarySet `
                            -Directory $uninstallShell)
                    }
                    Remove-KettleManagedRustAtomicTemporarySet -Prefix $Prefix
                    Assert-KettleManagedInstallTree -Path $Prefix
                    Invoke-KettleManagedInstallTreeRemoval -Path $Prefix
                } finally {
                    $runningLock.Dispose()
                }
            } finally {
                $updateLock.Dispose()
            }
            [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                (Join-Path $Prefix '.kettle-running.lock')
            )
            [KettleInstaller.NativeFileSystemV1]::DeleteOrdinaryLeaf(
                (Join-Path $Prefix '.kettle-update.lock')
            )
            $prefixChain.Verify()
        } finally {
            $prefixChain.Dispose()
        }
        # RemoveDirectoryW never recursively follows a replacement. If anything
        # appeared after validation, the nonempty-directory failure preserves it.
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($Prefix)
        Write-Output "  removed $Prefix"
    } elseif (Test-Path -LiteralPath $Prefix) {
        throw "The install prefix is not a real directory: $Prefix"
    } else {
        Write-Output "  install dir already absent: $Prefix"
    }
    if (-not $portable) {
        if (Test-Path $shortcutPath) {
            Remove-Item -Force $shortcutPath
            Write-Output "  removed Start menu shortcut"
        }
        if (Test-Path $uninstallKey) {
            Remove-Item -Recurse -Force $uninstallKey
            Write-Output "  removed Add/Remove Programs entry"
        }
        if (Invoke-KettleUserPathEdit -Dir $Prefix -Remove) {
            Write-Output "  removed $Prefix from user PATH"
        }
        # Also strip any -WithShellIntegration block we
        # appended to $PROFILE. Portable installs never add this block, so their
        # uninstall path must not remove integration owned by a default install.
        if (
            (Test-Path -LiteralPath $profilePath) -and
            (Invoke-KettleProfileIntegration `
                -ProfilePath $profilePath -Remove)
        ) {
            Write-Output "  removed kettle.ps1 snippet from `$PROFILE"
        }
    }
    Write-Output ""
    Write-Output "Uninstall complete. (Restart any open shells for PATH changes to take effect.)"
    return
}

if ($null -eq $sourceMode) {
    Write-Error @"
Could not find kettle.exe to install.

Looked for:
  $zipModeExe   (extracted release .zip layout)
  $repoModeExe  (in-tree repo layout - run `cargo build --release -p kettle` first)

If you grabbed the release zip, make sure you extracted it AND ran
install.ps1 from inside the extracted folder. If you cloned the repo,
build the release binary first:

    cargo build --release -p kettle
    .\scripts\install.ps1
"@
    exit 1
}

$consoleLauncher = if ($sourceMode -ne 'repo') {
    Join-Path $sourceDir "kettle.com"
} else {
    Join-Path $sourceDir "target\release\kettle-console.exe"
}
if (-not (Test-Path -LiteralPath $consoleLauncher -PathType Leaf)) {
    Write-Error "Could not find the required kettle console launcher: $consoleLauncher"
    exit 1
}

if (
    $WithShellIntegration -and
    -not $portable -and
    (Test-Path -LiteralPath $profilePath -PathType Leaf)
) {
    $profilePreflightChain =
        [KettleInstaller.NativeFileSystemV1]::LockRealDirectoryChain(
            (Split-Path $profilePath -Parent)
        )
    try {
        $profileSnapshot =
            [KettleInstaller.NativeFileSystemV1]::CaptureProfile(
                $profilePath,
                4194304
            )
        try {
            $profilePreflight = Get-KettleProfileDocument `
                -Path $profilePath -Bytes $profileSnapshot.Bytes
            [void](Get-KettleManagedProfileBlock -Text $profilePreflight.Text)
        } finally {
            $profileSnapshot.Dispose()
        }
    } finally {
        $profilePreflightChain.Dispose()
    }
}

if ($RefreshIntegration) {
    Assert-KettleInstallOwnership -Path $Prefix
    $refreshPrefixChain =
        [KettleInstaller.NativeFileSystemV1]::LockRealDirectoryChain(
        $Prefix
    )
    try {
        $refreshPrefixChain.Verify()
        $refreshUpdateLock =
            [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
                (Join-Path $Prefix '.kettle-update.lock')
            )
        try {
            $refreshRunningLock =
                [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
                    (Join-Path $Prefix '.kettle-running.lock')
            )
            try {
                Assert-KettleInstallOwnership -Path $Prefix
                [void](Remove-KettleInstallerTemporarySet `
                    -Directory $Prefix)
                $refreshShell = Join-Path $Prefix 'shell-integration'
                if (
                    Test-Path -LiteralPath $refreshShell -PathType Container
                ) {
                    [void](Remove-KettleInstallerTemporarySet `
                        -Directory $refreshShell)
                }
                Remove-KettleManagedRustAtomicTemporarySet -Prefix $Prefix
                Assert-KettleManagedInstallTree -Path $Prefix
                if ($portable) {
                    Write-Output "Portable install: no Windows integration to refresh."
                    return
                }
                $installedExe = Join-Path $Prefix "kettle.exe"
                $installedIcon = Join-Path $Prefix "kettle.ico"
                $installMarker = Join-Path $Prefix ".kettle-install.json"
                $ownershipRaw = Read-KettleStrictUtf8File `
                    -Path $installMarker -MaximumBytes 4096 `
                    -Label 'Install ownership marker'
                $marker = $ownershipRaw | ConvertFrom-Json -ErrorAction Stop
                $installedVersion = if (
                    $marker.version -is [string] -and
                    $marker.version -cmatch '^[0-9]+\.[0-9]+\.[0-9]+$'
                ) {
                    $marker.version
                } else {
                    'unknown'
                }

                New-Item -ItemType Directory -Force -Path $startMenuDir |
                    Out-Null
                if (Test-Path -LiteralPath $shortcutPath) {
                    Remove-Item -LiteralPath $shortcutPath -Force
                }
                $ws = New-Object -ComObject WScript.Shell
                $lnk = $ws.CreateShortcut($shortcutPath)
                $lnk.TargetPath = $installedExe
                $lnk.Arguments = ''
                $lnk.WorkingDirectory = $Prefix
                $lnk.IconLocation = $installedIcon
                $lnk.Description = "Fast, GPU-accelerated terminal emulator"
                $lnk.Save()

                New-Item -Path $uninstallKey -Force | Out-Null
                Set-ItemProperty -Path $uninstallKey `
                    -Name "DisplayName" -Value "kettle"
                Set-ItemProperty -Path $uninstallKey `
                    -Name "DisplayVersion" -Value $installedVersion
                Set-ItemProperty -Path $uninstallKey `
                    -Name "Publisher" -Value "kettle contributors"
                Set-ItemProperty -Path $uninstallKey `
                    -Name "InstallLocation" -Value $Prefix
                Set-ItemProperty -Path $uninstallKey `
                    -Name "DisplayIcon" -Value $installedIcon
                Set-ItemProperty -Path $uninstallKey `
                    -Name "URLInfoAbout" `
                    -Value "https://github.com/Reddimus/kettle"
                Set-ItemProperty -Path $uninstallKey `
                    -Name "NoModify" -Value 1 -Type DWord
                Set-ItemProperty -Path $uninstallKey `
                    -Name "NoRepair" -Value 1 -Type DWord
                $uninstallCmd = (
                    "powershell.exe -NoProfile -ExecutionPolicy Bypass " +
                    "-File `"$(Join-Path $Prefix 'install.ps1')`" -Uninstall"
                )
                Set-ItemProperty -Path $uninstallKey `
                    -Name "UninstallString" -Value $uninstallCmd
                if (-not $integrationTest) {
                    try {
                        & (Join-Path $env:SystemRoot 'System32\ie4uinit.exe') `
                            -show 2>$null
                    } catch {
                        Write-Verbose "Windows icon refresh failed: $($_.Exception.Message)"
                    }
                }
                Write-Output "Refreshed kettle Windows integration for version $installedVersion."
                $refreshPrefixChain.Verify()
                return
            } finally {
                $refreshRunningLock.Dispose()
            }
        } finally {
            $refreshUpdateLock.Dispose()
        }
    } finally {
        $refreshPrefixChain.Dispose()
    }
}

Write-Output "Installing kettle (source: $sourceMode mode, from $sourceDir)"
Write-Output ""

if (Test-Path -LiteralPath $Prefix) {
    $existingPrefix = Get-Item -LiteralPath $Prefix -Force -ErrorAction Stop
    if (
        -not $existingPrefix.PSIsContainer -or
        ($existingPrefix.Attributes -band
            [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'The install prefix is not a real directory.'
    }
} else {
    [void][System.IO.Directory]::CreateDirectory($Prefix)
}
Assert-KettleInstallPathChain -Path $Prefix
$installPrefixChain =
    [KettleInstaller.NativeFileSystemV1]::LockRealDirectoryChain(
    $Prefix
)
$installUpdateLock = $null
$installRunningLock = $null
try {
$recoveryRoot = $Prefix + '.install-transaction'
if (Test-Path -LiteralPath $recoveryRoot) {
    # Validate the private sibling before reading its journal. A hard kill
    # during the first journal write leaves only an empty scaffold and one
    # validated temporary leaf, which repair removes without touching Prefix.
    if (Repair-KettlePackageTransactionRoot `
        -Prefix $Prefix -TransactionRoot $recoveryRoot) {
        $installUpdateLock =
            [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
                (Join-Path $Prefix '.kettle-update.lock')
            )
        $installRunningLock =
            [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
                (Join-Path $Prefix '.kettle-running.lock')
            )
        Restore-KettlePackageTransaction `
            -Prefix $Prefix -TransactionRoot $recoveryRoot
    }
}
if (
    -not [string]::IsNullOrWhiteSpace($IntegrationTestRoot) -and
    $env:KETTLE_INSTALLER_TEST_RECOVER_ONLY -ceq '1'
) {
    # The subprocess hard-kill suite uses this gated seam to observe the exact
    # rollback result before a subsequent installation starts a new
    # transaction. Coordination lock leaves are deliberately retained until
    # the enclosing finally block releases their handles.
    return
}
$preLockChildren = @(
    Get-ChildItem -LiteralPath $Prefix -Force -ErrorAction Stop
)
$upgradingManagedInstall = @(
    $preLockChildren | Where-Object {
        $_.Name -cnotin @(
            '.kettle-update.lock',
            '.kettle-running.lock'
        )
    }
).Count -gt 0
if ($upgradingManagedInstall) {
    # Do not create coordination files in an unowned nonempty directory.
    Assert-KettleInstallOwnership -Path $Prefix
}
if ($null -eq $installUpdateLock) {
    $installUpdateLock =
        [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
            (Join-Path $Prefix '.kettle-update.lock')
        )
}
if ($null -eq $installRunningLock) {
    $installRunningLock =
        [KettleInstaller.NativeFileSystemV1]::AcquireExclusiveFileLock(
            (Join-Path $Prefix '.kettle-running.lock')
        )
}
$installPrefixChain.Verify()
$existingChildren = @(
    Get-ChildItem -LiteralPath $Prefix -Force -ErrorAction Stop
)
if ($upgradingManagedInstall) {
    Assert-KettleInstallOwnership -Path $Prefix
    [void](Remove-KettleInstallerTemporarySet -Directory $Prefix)
    $upgradeShell = Join-Path $Prefix 'shell-integration'
    if (Test-Path -LiteralPath $upgradeShell -PathType Container) {
        [void](Remove-KettleInstallerTemporarySet `
            -Directory $upgradeShell)
    }
    Remove-KettleManagedRustAtomicTemporarySet -Prefix $Prefix
    Assert-KettleManagedInstallTree -Path $Prefix
} else {
    $unexpectedNewChildren = @(
        $existingChildren | Where-Object {
            $_.Name -cnotin @(
                '.kettle-update.lock',
                '.kettle-running.lock'
            )
        }
    )
    if ($unexpectedNewChildren.Count -ne 0) {
        throw 'The new install prefix changed before installer locking completed.'
    }
}
$packagePlan = @(
    [pscustomobject]@{
        Source = $sourceExe
        Relative = 'kettle.exe'
        Limit = 536870912
    },
    [pscustomobject]@{
        Source = $consoleLauncher
        Relative = 'kettle.com'
        Limit = 536870912
    }
)

# Icon: zip mode ships kettle.ico next to the .exe; repo mode pulls
# from packaging/windows/.
$icoSrc = if ($sourceMode -ne 'repo') {
    Join-Path $sourceDir "kettle.ico"
} else {
    Join-Path $sourceDir "packaging\windows\kettle.ico"
}
if (Test-Path $icoSrc) {
    $packagePlan += [pscustomobject]@{
        Source = $icoSrc
        Relative = 'kettle.ico'
        Limit = 16777216
    }
}

# Bundle the supporting files so the install dir is self-contained.
foreach ($extra in @(
    'LICENSE',
    'NOTICE',
    'README.md',
    'CHANGELOG.md',
    'kettle-package-manifest.json'
)) {
    $src = Join-Path $sourceDir $extra
    if (Test-Path $src) {
        $packagePlan += [pscustomobject]@{
            Source = $src
            Relative = $extra
            Limit = 16777216
        }
    }
}

# Shell-integration snippets: both layouts have them at
# `shell-integration/kettle.{bash,zsh,fish,ps1}` relative to the
# source root.
$shellIntegrationSrc = Join-Path $sourceDir "shell-integration"
$shellLock = $null
$shellDestinationCreated = $false
if (Test-Path $shellIntegrationSrc) {
    $shellIntegrationDst = Join-Path $Prefix "shell-integration"
    if (Test-Path -LiteralPath $shellIntegrationDst) {
        $shellItem = Get-Item -LiteralPath $shellIntegrationDst -Force
        if (
            -not $shellItem.PSIsContainer -or
            ($shellItem.Attributes -band
                [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw 'The managed shell-integration path is not a real directory.'
        }
    } else {
        $shellDestinationCreated = $true
    }
    if (Test-Path -LiteralPath $shellIntegrationDst -PathType Container) {
        $shellLock = [KettleInstaller.NativeFileSystemV1]::LockRealDirectory(
            $shellIntegrationDst
        )
    }
    foreach ($shellName in @(
        'kettle.bash',
        'kettle.fish',
        'kettle.ps1',
        'kettle.zsh'
    )) {
        $shellSource = Join-Path $shellIntegrationSrc $shellName
        if (-not (Test-Path -LiteralPath $shellSource -PathType Leaf)) {
            throw "Required shell integration source is missing: $shellSource"
        }
        $packagePlan += [pscustomobject]@{
            Source = $shellSource
            Relative = ('shell-integration/' + $shellName)
            Limit = 1048576
        }
    }
}

$packagePlan += [pscustomobject]@{
    Source = $MyInvocation.MyCommand.Definition
    Relative = 'install.ps1'
    Limit = 4194304
}
if ($sourceMode -eq 'zip') {
    $manifestRecords = @{}
    foreach ($record in @($releaseManifest.files)) {
        $manifestRecords[$record.path] = $record
    }
    foreach ($entry in $packagePlan) {
        if ($entry.Relative -ceq 'kettle-package-manifest.json') {
            $expectedSize = $releaseManifest._manifest_size
            $expectedSha256 = $releaseManifest._manifest_sha256
        } else {
            $manifestRecord = $manifestRecords[$entry.Relative]
            if ($null -eq $manifestRecord) {
                throw "The verified release manifest omitted $($entry.Relative)."
            }
            $expectedSize = $manifestRecord.size
            $expectedSha256 = $manifestRecord.sha256
        }
        $entry | Add-Member -NotePropertyName ExpectedSize `
            -NotePropertyValue ([uint64]$expectedSize)
        $entry | Add-Member -NotePropertyName ExpectedSha256 `
            -NotePropertyValue $expectedSha256
    }
}

# Generate ownership records before publication so they participate in the
# same write-ahead rollback transaction as every payload file. A hard kill can
# never leave a nonempty first-install prefix without its ownership markers.
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$versionOutput = ''
try {
    $versionOutput =
        [KettleInstaller.NativeFileSystemV1]::ProbeExecutableVersion(
            $sourceExe,
            4096,
            15000
        )
} catch {
    Write-Verbose "Source version probe failed: $($_.Exception.Message)"
}
$kettleVersion = if ($versionOutput) {
    $m = [regex]::Match(
        $versionOutput,
        (
            '^kettle ([0-9]+\.[0-9]+\.[0-9]+)' +
            '(?: \([0-9a-f]{12}(?:\+dirty)?\))?(?:\r?\n)?$'
        )
    )
    if ($m -and $m.Success) { $m.Groups[1].Value } else { 'unknown' }
} else {
    'unknown'
}
$installChannel = if ($sourceMode -eq 'zip') {
    'stable'
} elseif ($sourceMode -eq 'installed') {
    $installedSourceChannel
} else {
    'local-dev'
}
$installMarker = [ordered]@{
    schema = 1
    product = 'kettle'
    managed_by = 'kettle-installer'
    channel = $installChannel
    target = 'x86_64-pc-windows-msvc'
    version = $kettleVersion
} | ConvertTo-Json
$packagePlan += [pscustomobject]@{
    Bytes = $utf8NoBom.GetBytes($Prefix)
    Relative = '.kettle-install-prefix'
    Limit = 32768
}
$packagePlan += [pscustomobject]@{
    Bytes = $utf8NoBom.GetBytes($installMarker + "`n")
    Relative = '.kettle-install.json'
    Limit = 4096
}

$packageSucceeded = $false
try {
    Invoke-KettlePackageTransaction `
        -Prefix $Prefix -Plan $packagePlan -TestRoot $IntegrationTestRoot
    if (
        -not [string]::IsNullOrWhiteSpace($IntegrationTestRoot) -and
        $env:KETTLE_INSTALLER_HARD_KILL_PHASE -ceq
            'after-package-commit'
    ) {
        [KettleInstaller.NativeFileSystemV1]::HardTerminateInstallerForTesting()
    }
    $packageSucceeded = $true
} finally {
    if ($null -ne $shellLock) {
        $shellLock.Dispose()
    }
    if (-not $packageSucceeded -and $shellDestinationCreated) {
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
            $shellIntegrationDst
        )
    }
}
Write-Output "  installed kettle.exe -> $Prefix"
Write-Output "  installed kettle.com console launcher"
if (Test-Path $icoSrc) {
    Write-Output "  installed kettle.ico"
}
if (Test-Path $shellIntegrationSrc) {
    Write-Output "  installed shell-integration\ (bash, zsh, fish, ps1)"
}

Write-Output "  wrote authenticated-update ownership marker ($installChannel)"
Assert-KettleInstallOwnership -Path $Prefix
Assert-KettleManagedInstallTree -Path $Prefix
$unpublishedInstallerFiles = @(
    Get-ChildItem -LiteralPath $Prefix -Force -ErrorAction Stop |
        Where-Object {
            $_.Name -cmatch '^\.kettle-install-tmp-[0-9a-f]{32}$'
        }
)
if ($unpublishedInstallerFiles.Count -ne 0) {
    throw 'An installer temporary file remained after atomic publication.'
}

# Portable mode short-circuits the system-touching steps.
if ($portable) {
    Write-Output ""
    Write-Output "Portable install complete at $Prefix"
    Write-Output "  - no Start menu shortcut (portable mode)"
    Write-Output "  - no PATH update (portable mode)"
    Write-Output "  - no Add/Remove Programs entry (portable mode)"
    Write-Output "Launch with: $Prefix\kettle.exe"
    return
}

# Start menu shortcut - via WScript.Shell COM (built into Windows;
# no external dependency). The shortcut lives under %APPDATA% so
# Windows Search indexes it without admin.
New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
$ws = New-Object -ComObject WScript.Shell
# CreateShortcut opens an existing .lnk and preserves every property the caller
# does not overwrite. Replace our managed shortcut so an older launcher's
# arguments (for example, a PowerShell dev-record wrapper) cannot survive an
# upgrade and be passed to kettle.exe.
if (Test-Path -LiteralPath $shortcutPath) {
    Remove-Item -LiteralPath $shortcutPath -Force
}
$lnk = $ws.CreateShortcut($shortcutPath)
$lnk.TargetPath = Join-Path $Prefix "kettle.exe"
$lnk.Arguments = ''
$lnk.WorkingDirectory = $Prefix
$lnk.IconLocation = Join-Path $Prefix "kettle.ico"
$lnk.Description = "Fast, GPU-accelerated terminal emulator"
$lnk.Save()
Write-Output "  created Start menu shortcut: $shortcutPath"

# Refresh the Windows icon cache. Explorer caches launcher icons by
# path, and an in-place `kettle.ico` overwrite raises no change notification, so
# a re-install with a CHANGED icon (e.g. the Catppuccin Mocha re-theme) would
# otherwise keep showing the stale bitmap in Start / search / taskbar until the
# cache rebuilds on its own. `ie4uinit -show` is the light, non-admin refresh;
# wrapped so a failure (older/newer Windows flag differences) never aborts the
# install. A full rebuild (clear %LOCALAPPDATA%\IconCache.db + restart Explorer)
# is only needed in the rare case this doesn't take.
if (-not $integrationTest) {
    try {
        & (Join-Path $env:SystemRoot 'System32\ie4uinit.exe') -show 2>$null
    } catch {
        Write-Verbose "Windows icon refresh failed: $($_.Exception.Message)"
    }
}

# Add/Remove Programs entry. Per-user (HKCU); no admin required.
New-Item -Path $uninstallKey -Force | Out-Null
Set-ItemProperty -Path $uninstallKey -Name "DisplayName" -Value "kettle"
Set-ItemProperty -Path $uninstallKey -Name "DisplayVersion" -Value $kettleVersion
Set-ItemProperty -Path $uninstallKey -Name "Publisher" -Value "kettle contributors"
Set-ItemProperty -Path $uninstallKey -Name "InstallLocation" -Value $Prefix
Set-ItemProperty -Path $uninstallKey -Name "DisplayIcon" -Value (Join-Path $Prefix "kettle.ico")
Set-ItemProperty -Path $uninstallKey -Name "URLInfoAbout" -Value "https://github.com/Reddimus/kettle"
Set-ItemProperty -Path $uninstallKey -Name "NoModify" -Value 1 -Type DWord
Set-ItemProperty -Path $uninstallKey -Name "NoRepair" -Value 1 -Type DWord
$uninstallCmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $Prefix 'install.ps1')`" -Uninstall"
Set-ItemProperty -Path $uninstallKey -Name "UninstallString" -Value $uninstallCmd
Write-Output "  registered in Add/Remove Programs (HKCU)"

# User PATH addition. Default-on; -NoPath to skip.
if (-not $NoPath) {
    if (Invoke-KettleUserPathEdit -Dir $Prefix) {
        Write-Output "  added $Prefix to user PATH"
        Write-Output "    (open a fresh shell to pick it up - already-running shells keep their snapshot)"
    } else {
        Write-Output "  $Prefix already on user PATH (no change)"
    }
}

# Optional opt-in install of the PowerShell shell
# integration snippet (kettle.ps1) into $PROFILE. The recommended
# install path (vs. the bash/zsh/fish "kettle --shell-integration
# powershell >> $PROFILE" one-liner) because that one-liner does NOT
# work under SUBSYSTEM:WINDOWS (a known trade-off: PS doesn't
# read stdout from GUI processes). Idempotent: the snippet itself
# has an internal $global:__kettle_prompt_installed guard, AND we
# skip the Add-Content if the snippet's signature line is already
# in $PROFILE so re-running install.ps1 -WithShellIntegration is
# a no-op.
if ($WithShellIntegration) {
    $snippetSrc = Join-Path $Prefix "shell-integration\kettle.ps1"
    if (-not (Test-Path -LiteralPath $snippetSrc -PathType Leaf)) {
        Write-Output "  -WithShellIntegration: snippet not found at $snippetSrc (skipping)"
    } else {
        $snippet = Read-KettleStrictUtf8File `
            -Path $snippetSrc -MaximumBytes 1048576 `
            -Label 'PowerShell integration snippet'
        if (
            Invoke-KettleProfileIntegration `
                -ProfilePath $profilePath -Snippet $snippet
        ) {
            Write-Output "  -WithShellIntegration: appended kettle.ps1 to `$PROFILE ($profilePath)"
            Write-Output "    (open a fresh PowerShell session to pick up the prompt marks)"
        } else {
            Write-Output "  -WithShellIntegration: snippet already in `$PROFILE (no change)"
        }
    }
}

Write-Output ""
Write-Output "Install complete."
Write-Output ""
Write-Output "Try:"
Write-Output "  - Press Win, type 'kettle', hit Enter."
Write-Output "  - Or from a fresh shell: kettle --version"
if (-not $WithShellIntegration) {
    Write-Output ""
    Write-Output "Tip: re-run with -WithShellIntegration to enable OSC 133"
    Write-Output "  prompt marks in PowerShell (Ctrl+Up / Ctrl+Down to jump"
    Write-Output "  between prompts inside kettle)."
}
Write-Output ""
Write-Output "To uninstall later: appwiz.cpl (Add/Remove Programs) or"
Write-Output "  powershell -File `"$Prefix\install.ps1`" -Uninstall"
$installPrefixChain.Verify()
} finally {
    if ($null -ne $installRunningLock) {
        $installRunningLock.Dispose()
    }
    if ($null -ne $installUpdateLock) {
        $installUpdateLock.Dispose()
    }
    $installPrefixChain.Dispose()
}
