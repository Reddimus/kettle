# Acquire one reviewed Windows comparator campaign before measurement. The
# retained release assets, extracted trees, executable bytes, and Authenticode
# identities are verified on every reuse. Measurement code consumes only the
# completed local campaign and never reaches the network.

[CmdletBinding()]
param(
    [ValidatePattern(
        '^windows-x86_64-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{16}$'
    )]
    [string]$CampaignId = (
        'windows-x86_64-20260727T012800Z-d76cbf4b8173c691'
    ),
    [switch]$Offline,
    [switch]$PassThru
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\comparator-campaign.ps1"

$script:KettlePerfComparatorSetupMaximumEntries = 50000
$script:KettlePerfComparatorSetupMaximumEntryBytes = 1073741824
$script:KettlePerfComparatorSetupMaximumExpandedBytes = 4294967296
$script:KettlePerfComparatorSetupBufferBytes = 65536

function Initialize-KettlePerfComparatorSetupNative {
    if ('KettlePerfComparatorSetup.NativePath' -as [type]) {
        return
    }

    $source = @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfComparatorSetup {
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

    public sealed class NativePath {
        internal NativePath(
            string finalPath,
            uint attributes,
            uint links,
            string identity) {
            FinalPath = finalPath;
            Attributes = attributes;
            LinkCount = links;
            Identity = identity;
        }

        public string FinalPath { get; private set; }
        public uint Attributes { get; private set; }
        public uint LinkCount { get; private set; }
        public string Identity { get; private set; }
    }

    public static class Native {
        private const uint ShareAll = 0x00000007;
        private const uint OpenExisting = 3;
        private const uint FlagBackupSemantics = 0x02000000;
        private const uint FlagOpenReparsePoint = 0x00200000;

        [DllImport(
            "kernel32.dll",
            CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetFileInformationByHandle(
            SafeFileHandle file,
            out ByHandleFileInformation information);

        [DllImport(
            "kernel32.dll",
            CharSet = CharSet.Unicode,
            SetLastError = true)]
        private static extern uint GetFinalPathNameByHandleW(
            SafeFileHandle file,
            StringBuilder path,
            uint pathLength,
            uint flags);

        private static string ToExtendedPath(string path) {
            if (path.StartsWith(
                    @"\\?\", StringComparison.OrdinalIgnoreCase)) {
                return path;
            }
            if (path.StartsWith(
                    @"\\", StringComparison.OrdinalIgnoreCase)) {
                return @"\\?\UNC\" + path.Substring(2);
            }
            return @"\\?\" + path;
        }

        public static NativePath Inspect(string path) {
            if (string.IsNullOrWhiteSpace(path)) {
                throw new ArgumentException("Path is required", "path");
            }
            var fullPath = Path.GetFullPath(path);
            using (var handle = CreateFileW(
                    ToExtendedPath(fullPath),
                    0,
                    ShareAll,
                    IntPtr.Zero,
                    OpenExisting,
                    FlagBackupSemantics | FlagOpenReparsePoint,
                    IntPtr.Zero)) {
                if (handle.IsInvalid) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Opening comparator campaign path failed");
                }
                return InspectHandle(handle);
            }
        }

        public static NativePath InspectHandle(SafeFileHandle handle) {
            if (handle == null || handle.IsInvalid || handle.IsClosed) {
                throw new ArgumentException(
                    "An open file handle is required", "handle");
            }
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Reading comparator campaign file identity failed");
            }

            var capacity = 512;
            string path;
            while (true) {
                var builder = new StringBuilder(capacity);
                var length = GetFinalPathNameByHandleW(
                    handle, builder, (uint)builder.Capacity, 0);
                if (length == 0) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Resolving comparator campaign path failed");
                }
                if (length < builder.Capacity) {
                    path = builder.ToString();
                    break;
                }
                capacity = checked((int)length + 1);
                if (capacity > 32768) {
                    throw new InvalidDataException(
                        "Comparator campaign path is too long");
                }
            }
            if (path.StartsWith(
                    @"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) {
                path = @"\\" + path.Substring(8);
            } else if (path.StartsWith(
                    @"\\?\", StringComparison.OrdinalIgnoreCase)) {
                path = path.Substring(4);
            }
            var identity = string.Format(
                "{0:x8}:{1:x8}{2:x8}",
                information.VolumeSerialNumber,
                information.FileIndexHigh,
                information.FileIndexLow);
            return new NativePath(
                Path.GetFullPath(path),
                information.FileAttributes,
                information.NumberOfLinks,
                identity);
        }
    }
}
'@

    Add-Type -TypeDefinition $source -Language CSharp -ErrorAction Stop
}

function Test-KettlePerfComparatorSetupIsReparse {
    param(
        [Parameter(Mandatory)]
        [uint32]$Attributes
    )

    return ($Attributes -band 0x00000400) -ne 0
}

function Test-KettlePerfComparatorSetupIsDirectory {
    param(
        [Parameter(Mandatory)]
        [uint32]$Attributes
    )

    return ($Attributes -band 0x00000010) -ne 0
}

function Assert-KettlePerfComparatorSetupPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [ValidateSet('File', 'Directory')]
        [string]$Kind,
        [switch]$AllowMultipleLinks
    )

    Initialize-KettlePerfComparatorSetupNative
    $expected = [IO.Path]::GetFullPath($Path)
    if (-not (Test-KettlePerfComparatorPathHasNoAlternateStream $expected)) {
        throw "Comparator setup path names an alternate stream: $expected"
    }
    try {
        $information = (
            [KettlePerfComparatorSetup.Native]::Inspect($expected)
        )
    } catch {
        throw (
            "Comparator setup path could not be inspected: $expected. " +
            $_.Exception.Message
        )
    }
    if (
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            $information.FinalPath,
            $expected
        ) -or
        (Test-KettlePerfComparatorSetupIsReparse $information.Attributes)
    ) {
        throw "Comparator setup path aliases or is a reparse point: $expected"
    }
    $isDirectory = Test-KettlePerfComparatorSetupIsDirectory `
        $information.Attributes
    if (
        ($Kind -ceq 'Directory' -and -not $isDirectory) -or
        ($Kind -ceq 'File' -and $isDirectory)
    ) {
        throw "Comparator setup path has the wrong type: $expected"
    }
    if (
        $Kind -ceq 'File' -and
        -not $AllowMultipleLinks -and
        [uint32]$information.LinkCount -ne 1
    ) {
        throw "Comparator setup file must have exactly one link: $expected"
    }
    return $information
}

function New-KettlePerfComparatorSetupDirectory {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates only a validated direct child directory.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$ExpectedParent
    )

    $parent = [IO.Path]::GetFullPath($ExpectedParent).TrimEnd('\', '/')
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $parent -Kind Directory)
    $fullPath = [IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    $actualParent = [IO.Path]::GetDirectoryName($fullPath).TrimEnd('\', '/')
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $actualParent,
        $parent
    )) {
        throw 'Comparator setup directory is not one direct child'
    }
    if ([IO.Directory]::Exists($fullPath)) {
        [void](Assert-KettlePerfComparatorSetupPath `
            -Path $fullPath -Kind Directory)
        return $fullPath
    }
    if ([IO.File]::Exists($fullPath)) {
        throw "Comparator setup directory collides with a file: $fullPath"
    }
    [void][IO.Directory]::CreateDirectory($fullPath)
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $fullPath -Kind Directory)
    return $fullPath
}

function New-KettlePerfComparatorSetupDirectoryChain {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates only a validated relative directory chain.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Root,
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string]$RelativePath
    )

    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $rootPath -Kind Directory)
    if (-not $RelativePath) {
        return $rootPath
    }
    $current = $rootPath
    foreach ($part in $RelativePath.Split([char[]]@('/'))) {
        $next = Join-Path $current $part
        $current = New-KettlePerfComparatorSetupDirectory `
            -Path $next -ExpectedParent $current
    }
    return $current
}

function Get-KettlePerfComparatorSetupSha256 {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes
    )

    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return [BitConverter]::ToString(
            $algorithm.ComputeHash($Bytes)
        ).Replace('-', '').ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Get-KettlePerfComparatorSetupFileEvidence {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    [void](Assert-KettlePerfComparatorSetupPath -Path $Path -Kind File)
    $stream = $null
    $algorithm = $null
    try {
        $stream = [IO.FileStream]::new(
            [IO.Path]::GetFullPath($Path),
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        $handleInformation = (
            [KettlePerfComparatorSetup.Native]::InspectHandle(
                $stream.SafeFileHandle
            )
        )
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $handleInformation.FinalPath,
                [IO.Path]::GetFullPath($Path)
            ) -or
            (Test-KettlePerfComparatorSetupIsReparse `
                $handleInformation.Attributes) -or
            [uint32]$handleInformation.LinkCount -ne 1
        ) {
            throw 'Comparator setup file identity changed while opening'
        }
        $algorithm = [Security.Cryptography.SHA256]::Create()
        $hash = [BitConverter]::ToString(
            $algorithm.ComputeHash($stream)
        ).Replace('-', '').ToLowerInvariant()
        return [pscustomobject][ordered]@{
            path = [IO.Path]::GetFullPath($Path)
            bytes = [long]$stream.Length
            sha256 = $hash
            identity = [string]$handleInformation.Identity
        }
    } finally {
        if ($null -ne $algorithm) {
            $algorithm.Dispose()
        }
        if ($null -ne $stream) {
            $stream.Dispose()
        }
    }
}

function Assert-KettlePerfComparatorSetupEvidence {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [long]$ExpectedBytes,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$ExpectedSha256
    )

    $evidence = Get-KettlePerfComparatorSetupFileEvidence -Path $Path
    if (
        [long]$evidence.bytes -ne $ExpectedBytes -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            $evidence.sha256,
            $ExpectedSha256
        )
    ) {
        throw "Comparator setup file bytes differ from campaign: $Path"
    }
    return $evidence
}

function Copy-KettlePerfComparatorSetupFile {
    param(
        [Parameter(Mandatory)]
        [string]$Source,
        [Parameter(Mandatory)]
        [string]$Destination
    )

    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $Source -Kind File -AllowMultipleLinks)
    $parent = [IO.Path]::GetDirectoryName(
        [IO.Path]::GetFullPath($Destination)
    )
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $parent -Kind Directory)
    if (
        [IO.File]::Exists($Destination) -or
        [IO.Directory]::Exists($Destination)
    ) {
        throw "Comparator setup destination already exists: $Destination"
    }
    $sourceStream = $null
    $output = $null
    try {
        $sourceStream = [IO.FileStream]::new(
            [IO.Path]::GetFullPath($Source),
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        $sourceInformation = (
            [KettlePerfComparatorSetup.Native]::InspectHandle(
                $sourceStream.SafeFileHandle
            )
        )
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $sourceInformation.FinalPath,
                [IO.Path]::GetFullPath($Source)
            ) -or
            (Test-KettlePerfComparatorSetupIsReparse `
                $sourceInformation.Attributes)
        ) {
            throw 'Comparator setup source changed while opening'
        }
        $output = [IO.FileStream]::new(
            [IO.Path]::GetFullPath($Destination),
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        $sourceStream.CopyTo(
            $output,
            $script:KettlePerfComparatorSetupBufferBytes
        )
        $output.Flush($true)
    } finally {
        if ($null -ne $output) {
            $output.Dispose()
        }
        if ($null -ne $sourceStream) {
            $sourceStream.Dispose()
        }
    }
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $Destination -Kind File)
}

function Assert-KettlePerfComparatorSetupOfficialUri {
    param(
        [Parameter(Mandatory)]
        [string]$Uri,
        [switch]$Redirect
    )

    $parsed = $null
    if (
        -not [Uri]::TryCreate($Uri, [UriKind]::Absolute, [ref]$parsed) -or
        $parsed.Scheme -cne 'https' -or
        -not $parsed.IsDefaultPort -or
        $parsed.UserInfo.Length -ne 0 -or
        $parsed.Fragment.Length -ne 0
    ) {
        throw "Comparator asset URI is not safe HTTPS: $Uri"
    }
    $allowedHosts = if ($Redirect) {
        [string[]]@(
            'github.com',
            'release-assets.githubusercontent.com',
            'objects.githubusercontent.com'
        )
    } else {
        [string[]]@('github.com')
    }
    if ($parsed.DnsSafeHost -cnotin $allowedHosts) {
        throw "Comparator asset URI host is not allowed: $Uri"
    }
    if (-not $Redirect -and ($parsed.Query.Length -ne 0)) {
        throw "Comparator source URI must not contain a query: $Uri"
    }
    return $parsed
}

function Invoke-KettlePerfComparatorSetupHttpsDownload {
    param(
        [Parameter(Mandatory)]
        $Entry,
        [Parameter(Mandatory)]
        [string]$Destination
    )

    $initialUri = Assert-KettlePerfComparatorSetupOfficialUri `
        -Uri ([string]$Entry.source.asset.url)
    $expectedBytes = [long]$Entry.source.asset.bytes
    $parent = [IO.Path]::GetDirectoryName(
        [IO.Path]::GetFullPath($Destination)
    )
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $parent -Kind Directory)
    if (
        [IO.File]::Exists($Destination) -or
        [IO.Directory]::Exists($Destination)
    ) {
        throw 'Comparator download destination already exists'
    }

    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $handler.UseCookies = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromMinutes(15)
    $current = $initialUri
    try {
        for ($redirects = 0; $redirects -le 5; $redirects++) {
            $request = [Net.Http.HttpRequestMessage]::new(
                [Net.Http.HttpMethod]::Get,
                $current
            )
            [void]$request.Headers.TryAddWithoutValidation(
                'User-Agent',
                'KettleComparatorCampaign/1'
            )
            $response = $null
            try {
                $response = $client.SendAsync(
                    $request,
                    [Net.Http.HttpCompletionOption]::ResponseHeadersRead
                ).GetAwaiter().GetResult()
                $status = [int]$response.StatusCode
                if ($status -in @(301, 302, 303, 307, 308)) {
                    if (
                        $redirects -eq 5 -or
                        $null -eq $response.Headers.Location
                    ) {
                        throw 'Comparator asset redirect chain is invalid'
                    }
                    $next = if (
                        $response.Headers.Location.IsAbsoluteUri
                    ) {
                        $response.Headers.Location
                    } else {
                        [Uri]::new($current, $response.Headers.Location)
                    }
                    $current = Assert-KettlePerfComparatorSetupOfficialUri `
                        -Uri $next.AbsoluteUri -Redirect
                    continue
                }
                if ($status -ne 200) {
                    throw "Comparator asset HTTP request failed: $status"
                }
                $contentLength = $response.Content.Headers.ContentLength
                if (
                    $null -ne $contentLength -and
                    [long]$contentLength -ne $expectedBytes
                ) {
                    throw 'Comparator asset Content-Length differs from campaign'
                }

                $sourceStream = $null
                $output = $null
                $algorithm = $null
                try {
                    $sourceStream = $response.Content.ReadAsStreamAsync(
                    ).GetAwaiter().GetResult()
                    $output = [IO.FileStream]::new(
                        [IO.Path]::GetFullPath($Destination),
                        [IO.FileMode]::CreateNew,
                        [IO.FileAccess]::Write,
                        [IO.FileShare]::None
                    )
                    $algorithm = [Security.Cryptography.SHA256]::Create()
                    $buffer = New-Object byte[] (
                        $script:KettlePerfComparatorSetupBufferBytes
                    )
                    [long]$total = 0
                    while (($read = $sourceStream.Read(
                        $buffer, 0, $buffer.Length
                    )) -gt 0) {
                        $total = [long]($total + [long]$read)
                        if ($total -gt $expectedBytes) {
                            throw 'Comparator asset exceeds its pinned size'
                        }
                        [void]$algorithm.TransformBlock(
                            $buffer, 0, $read, $null, 0
                        )
                        $output.Write($buffer, 0, $read)
                    }
                    [void]$algorithm.TransformFinalBlock(
                        [byte[]]@(), 0, 0
                    )
                    $output.Flush($true)
                    if ($total -ne $expectedBytes) {
                        throw 'Comparator asset ended before its pinned size'
                    }
                    $actualHash = [BitConverter]::ToString(
                        $algorithm.Hash
                    ).Replace('-', '').ToLowerInvariant()
                    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
                        $actualHash,
                        [string]$Entry.source.asset.sha256
                    )) {
                        throw 'Comparator asset hash differs from campaign'
                    }
                } finally {
                    if ($null -ne $algorithm) {
                        $algorithm.Dispose()
                    }
                    if ($null -ne $output) {
                        $output.Dispose()
                    }
                    if ($null -ne $sourceStream) {
                        $sourceStream.Dispose()
                    }
                }
                [void](Assert-KettlePerfComparatorSetupPath `
                    -Path $Destination -Kind File)
                return
            } finally {
                if ($null -ne $response) {
                    $response.Dispose()
                }
                $request.Dispose()
            }
        }
        throw 'Comparator asset exceeded its redirect bound'
    } finally {
        $client.Dispose()
        $handler.Dispose()
    }
}

function ConvertTo-KettlePerfComparatorSetupUnsigned32 {
    param(
        [Parameter(Mandatory)]
        [int32]$Value
    )

    return [BitConverter]::ToUInt32(
        [BitConverter]::GetBytes($Value),
        0
    )
}

function Test-KettlePerfComparatorSetupSafePathPart {
    param(
        [Parameter(Mandatory)]
        [string]$Part
    )

    if (
        $Part.Length -lt 1 -or
        $Part.Length -gt 128 -or
        $Part -in @('.', '..') -or
        $Part.IndexOfAny([char[]]"`0`r`n`t<>:`"\/|?*") -ge 0 -or
        $Part.EndsWith('.', [StringComparison]::Ordinal) -or
        $Part.EndsWith(' ', [StringComparison]::Ordinal)
    ) {
        return $false
    }
    foreach ($character in $Part.ToCharArray()) {
        if ([int]$character -lt 32) {
            return $false
        }
    }
    $deviceBase = ($Part -split '\.', 2)[0]
    if (
        $deviceBase -cmatch
            '^(?i:CON|PRN|AUX|NUL|CLOCK\$|COM[1-9]|LPT[1-9])$'
    ) {
        return $false
    }
    return $true
}

function ConvertTo-KettlePerfComparatorSetupZipPath {
    param(
        [Parameter(Mandatory)]
        [string]$EntryName,
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string]$Prefix,
        [Parameter(Mandatory)]
        [bool]$IsDirectory
    )

    if (
        $EntryName.Length -lt 1 -or
        $EntryName.Length -gt 2048 -or
        $EntryName.IndexOf('\') -ge 0 -or
        $EntryName.IndexOf([char]0) -ge 0 -or
        $EntryName.StartsWith('/', [StringComparison]::Ordinal) -or
        [IO.Path]::IsPathRooted($EntryName)
    ) {
        throw "Comparator ZIP entry has an unsafe name: $EntryName"
    }
    if (
        $Prefix -and
        -not $EntryName.StartsWith($Prefix, [StringComparison]::Ordinal)
    ) {
        throw "Comparator ZIP entry is outside the pinned archive root: $EntryName"
    }
    $relative = if ($Prefix) {
        $EntryName.Substring($Prefix.Length)
    } else {
        $EntryName
    }
    if ($IsDirectory) {
        $relative = $relative.TrimEnd('/')
    } elseif ($relative.EndsWith('/', [StringComparison]::Ordinal)) {
        throw "Comparator ZIP file entry ends as a directory: $EntryName"
    }
    if (-not $relative) {
        if ($IsDirectory) {
            return ''
        }
        throw 'Comparator ZIP file entry has an empty relative path'
    }
    if (
        $relative.StartsWith('/', [StringComparison]::Ordinal) -or
        $relative.EndsWith('/', [StringComparison]::Ordinal) -or
        $relative.Contains('//')
    ) {
        throw "Comparator ZIP entry has empty path components: $EntryName"
    }
    $parts = [string[]]$relative.Split([char[]]@('/'))
    foreach ($part in $parts) {
        if (-not (Test-KettlePerfComparatorSetupSafePathPart $part)) {
            throw "Comparator ZIP entry has an unsafe component: $EntryName"
        }
    }
    return $parts -join '/'
}

function Get-KettlePerfComparatorSetupZipPrefix {
    param(
        [Parameter(Mandatory)]
        [string]$ExecutableEntry
    )

    $separator = $ExecutableEntry.LastIndexOf('/')
    if ($separator -lt 0) {
        return ''
    }
    return $ExecutableEntry.Substring(0, $separator + 1)
}

function Get-KettlePerfComparatorSetupZipEntryKind {
    param(
        [Parameter(Mandatory)]
        [IO.Compression.ZipArchiveEntry]$Entry
    )

    $bits = ConvertTo-KettlePerfComparatorSetupUnsigned32 `
        ([int32]$Entry.ExternalAttributes)
    $unixMode = ($bits -shr 16) -band 0xffff
    $unixType = $unixMode -band 0xf000
    $dosAttributes = $bits -band 0xffff
    if (($dosAttributes -band 0x0400) -ne 0) {
        throw "Comparator ZIP entry is a reparse point: $($Entry.FullName)"
    }
    $directoryByName = $Entry.FullName.EndsWith(
        '/',
        [StringComparison]::Ordinal
    )
    if (
        $unixType -notin @(
            [uint32]0x0000,
            [uint32]0x4000,
            [uint32]0x8000
        )
    ) {
        throw "Comparator ZIP entry is a link or special file: $($Entry.FullName)"
    }
    if (
        ($unixType -eq 0x4000 -and -not $directoryByName) -or
        ($unixType -eq 0x8000 -and $directoryByName)
    ) {
        throw "Comparator ZIP entry type conflicts with its name: $($Entry.FullName)"
    }
    if ($directoryByName -or $unixType -eq 0x4000) {
        return 'Directory'
    }
    return 'File'
}

function Add-KettlePerfComparatorSetupExpectedParentPath {
    param(
        [Parameter(Mandatory)]
        [Collections.Generic.Dictionary[string, string]]$Paths,
        [Parameter(Mandatory)]
        [string]$RelativePath
    )

    $parts = [string[]]$RelativePath.Split([char[]]@('/'))
    if ($parts.Count -lt 2) {
        return
    }
    $current = ''
    for ($index = 0; $index -lt ($parts.Count - 1); $index++) {
        $current = if ($current) {
            "$current/$($parts[$index])"
        } else {
            $parts[$index]
        }
        if ($Paths.ContainsKey($current)) {
            if ($Paths[$current] -cne 'Directory') {
                throw "Comparator ZIP path crosses a file: $RelativePath"
            }
        } else {
            $Paths.Add($current, 'Directory')
        }
    }
}

function Add-KettlePerfComparatorSetupExpectedPath {
    param(
        [Parameter(Mandatory)]
        [Collections.Generic.Dictionary[string, string]]$Paths,
        [Parameter(Mandatory)]
        [string]$RelativePath,
        [Parameter(Mandatory)]
        [ValidateSet('File', 'Directory')]
        [string]$Kind
    )

    Add-KettlePerfComparatorSetupExpectedParentPath `
        -Paths $Paths -RelativePath $RelativePath
    if ($Paths.ContainsKey($RelativePath)) {
        if ($Paths[$RelativePath] -cne $Kind) {
            throw "Comparator ZIP path changes type: $RelativePath"
        }
        if ($Kind -ceq 'File') {
            throw "Comparator ZIP has a duplicate file: $RelativePath"
        }
        return
    }
    if ($Kind -ceq 'File') {
        $prefix = "$RelativePath/"
        foreach ($known in $Paths.Keys) {
            if ($known.StartsWith(
                $prefix,
                [StringComparison]::OrdinalIgnoreCase
            )) {
                throw "Comparator ZIP file shadows a directory: $RelativePath"
            }
        }
    }
    $Paths.Add($RelativePath, $Kind)
}

function Open-KettlePerfComparatorSetupZip {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    Add-Type -AssemblyName System.IO.Compression -ErrorAction Stop
    $stream = [IO.FileStream]::new(
        [IO.Path]::GetFullPath($Path),
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    try {
        $information = (
            [KettlePerfComparatorSetup.Native]::InspectHandle(
                $stream.SafeFileHandle
            )
        )
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $information.FinalPath,
                [IO.Path]::GetFullPath($Path)
            ) -or
            (Test-KettlePerfComparatorSetupIsReparse `
                $information.Attributes) -or
            [uint32]$information.LinkCount -ne 1
        ) {
            throw 'Comparator ZIP identity changed while opening'
        }
        $archive = [IO.Compression.ZipArchive]::new(
            $stream,
            [IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        $stream = $null
        return $archive
    } finally {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
    }
}

function Get-KettlePerfComparatorSetupZipPlan {
    param(
        [Parameter(Mandatory)]
        [IO.Compression.ZipArchive]$Archive,
        [Parameter(Mandatory)]
        [string]$ExecutableEntry
    )

    $prefix = Get-KettlePerfComparatorSetupZipPrefix $ExecutableEntry
    $paths = [Collections.Generic.Dictionary[string, string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    [long]$expandedBytes = 0
    [int]$entryCount = 0
    [int]$executableMatches = 0
    $plan = [Collections.Generic.List[object]]::new()
    foreach ($entry in $Archive.Entries) {
        $entryCount++
        if (
            $entryCount -gt
            $script:KettlePerfComparatorSetupMaximumEntries
        ) {
            throw 'Comparator ZIP exceeds the entry-count bound'
        }
        $kind = Get-KettlePerfComparatorSetupZipEntryKind $entry
        $relative = ConvertTo-KettlePerfComparatorSetupZipPath `
            -EntryName $entry.FullName -Prefix $prefix `
            -IsDirectory:($kind -ceq 'Directory')
        if (-not $relative) {
            continue
        }
        if ($kind -ceq 'File') {
            if (
                [long]$entry.Length -lt 0 -or
                [long]$entry.Length -gt
                    $script:KettlePerfComparatorSetupMaximumEntryBytes -or
                [long]$entry.CompressedLength -lt 0
            ) {
                throw "Comparator ZIP entry exceeds its size bound: $relative"
            }
            $expandedBytes = [long](
                $expandedBytes + [long]$entry.Length
            )
            if (
                $expandedBytes -gt
                $script:KettlePerfComparatorSetupMaximumExpandedBytes
            ) {
                throw 'Comparator ZIP exceeds its expanded-size bound'
            }
        }
        Add-KettlePerfComparatorSetupExpectedPath `
            -Paths $paths -RelativePath $relative -Kind $kind
        if (
            [StringComparer]::Ordinal.Equals(
                $entry.FullName,
                $ExecutableEntry
            )
        ) {
            if ($kind -cne 'File') {
                throw 'Comparator ZIP executable entry is not a file'
            }
            $executableMatches++
        }
        $plan.Add([pscustomobject][ordered]@{
            entry = $entry
            full_name = [string]$entry.FullName
            relative_path = $relative
            kind = $kind
            bytes = [long]$entry.Length
        })
    }
    if ($entryCount -lt 1 -or $executableMatches -ne 1) {
        throw 'Comparator ZIP has no unique pinned executable entry'
    }
    return [pscustomobject][ordered]@{
        prefix = $prefix
        paths = $paths
        entries = [object[]]$plan.ToArray()
        expanded_bytes = $expandedBytes
    }
}

function Expand-KettlePerfComparatorSetupZip {
    param(
        [Parameter(Mandatory)]
        [string]$ArchivePath,
        [Parameter(Mandatory)]
        [string]$Destination,
        [Parameter(Mandatory)]
        [string]$ExecutableEntry
    )

    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $Destination -Kind Directory)
    $archive = $null
    try {
        $archive = Open-KettlePerfComparatorSetupZip $ArchivePath
        $plan = Get-KettlePerfComparatorSetupZipPlan `
            -Archive $archive -ExecutableEntry $ExecutableEntry
        foreach ($record in $plan.entries) {
            $relative = [string]$record.relative_path
            if ($record.kind -ceq 'Directory') {
                [void](New-KettlePerfComparatorSetupDirectoryChain `
                    -Root $Destination -RelativePath $relative)
                continue
            }
            $parentRelative = [IO.Path]::GetDirectoryName(
                $relative.Replace('/', '\')
            )
            $parentRelative = if ($parentRelative) {
                $parentRelative.Replace('\', '/')
            } else {
                ''
            }
            $parent = New-KettlePerfComparatorSetupDirectoryChain `
                -Root $Destination -RelativePath $parentRelative
            $leaf = [IO.Path]::GetFileName(
                $relative.Replace('/', '\')
            )
            $target = Join-Path $parent $leaf
            if (
                [IO.File]::Exists($target) -or
                [IO.Directory]::Exists($target)
            ) {
                throw "Comparator ZIP extraction target exists: $relative"
            }
            $sourceStream = $null
            $output = $null
            try {
                $sourceStream = $record.entry.Open()
                $output = [IO.FileStream]::new(
                    $target,
                    [IO.FileMode]::CreateNew,
                    [IO.FileAccess]::Write,
                    [IO.FileShare]::None
                )
                $buffer = New-Object byte[] (
                    $script:KettlePerfComparatorSetupBufferBytes
                )
                [long]$total = 0
                while (($read = $sourceStream.Read(
                    $buffer, 0, $buffer.Length
                )) -gt 0) {
                    $total = [long]($total + [long]$read)
                    if ($total -gt [long]$record.bytes) {
                        throw "Comparator ZIP entry exceeds metadata: $relative"
                    }
                    $output.Write($buffer, 0, $read)
                }
                $output.Flush($true)
                if ($total -ne [long]$record.bytes) {
                    throw "Comparator ZIP entry ended early: $relative"
                }
            } finally {
                if ($null -ne $output) {
                    $output.Dispose()
                }
                if ($null -ne $sourceStream) {
                    $sourceStream.Dispose()
                }
            }
            [void](Assert-KettlePerfComparatorSetupPath `
                -Path $target -Kind File)
        }
    } finally {
        if ($null -ne $archive) {
            $archive.Dispose()
        }
    }
}

function Get-KettlePerfComparatorSetupTree {
    param(
        [Parameter(Mandatory)]
        [string]$Root
    )

    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $rootPath -Kind Directory)
    $result = [Collections.Generic.Dictionary[string, object]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $pending = [Collections.Generic.Stack[object]]::new()
    $pending.Push([pscustomobject]@{
        path = $rootPath
        relative = ''
    })
    while ($pending.Count -gt 0) {
        $current = $pending.Pop()
        foreach ($path in [IO.Directory]::EnumerateFileSystemEntries(
            [string]$current.path
        )) {
            $information = [KettlePerfComparatorSetup.Native]::Inspect($path)
            if (
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    $information.FinalPath,
                    [IO.Path]::GetFullPath($path)
                ) -or
                (Test-KettlePerfComparatorSetupIsReparse `
                    $information.Attributes)
            ) {
                throw 'Comparator staging tree contains an alias or reparse point'
            }
            $leaf = [IO.Path]::GetFileName($path)
            if (-not (Test-KettlePerfComparatorSetupSafePathPart $leaf)) {
                throw "Comparator staging tree has an unsafe leaf: $leaf"
            }
            $relative = if ($current.relative) {
                "$($current.relative)/$leaf"
            } else {
                $leaf
            }
            $isDirectory = Test-KettlePerfComparatorSetupIsDirectory `
                $information.Attributes
            if (-not $isDirectory -and [uint32]$information.LinkCount -ne 1) {
                throw "Comparator staging tree contains a hardlink: $relative"
            }
            if ($result.ContainsKey($relative)) {
                throw "Comparator staging tree has an ambiguous path: $relative"
            }
            $kind = if ($isDirectory) { 'Directory' } else { 'File' }
            $result.Add($relative, [pscustomobject][ordered]@{
                path = [IO.Path]::GetFullPath($path)
                kind = $kind
                identity = [string]$information.Identity
            })
            if ($isDirectory) {
                $pending.Push([pscustomobject]@{
                    path = [IO.Path]::GetFullPath($path)
                    relative = $relative
                })
            }
            if (
                $result.Count -gt
                $script:KettlePerfComparatorSetupMaximumEntries
            ) {
                throw 'Comparator staging tree exceeds the entry-count bound'
            }
        }
    }
    return $result
}

function Get-KettlePerfComparatorSetupStreamEvidence {
    param(
        [Parameter(Mandatory)]
        [IO.Stream]$Stream,
        [Parameter(Mandatory)]
        [long]$ExpectedBytes
    )

    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $buffer = New-Object byte[] (
            $script:KettlePerfComparatorSetupBufferBytes
        )
        [long]$total = 0
        while (($read = $Stream.Read(
            $buffer, 0, $buffer.Length
        )) -gt 0) {
            $total = [long]($total + [long]$read)
            if ($total -gt $ExpectedBytes) {
                throw 'Comparator archive entry exceeds its recorded size'
            }
            [void]$algorithm.TransformBlock(
                $buffer, 0, $read, $null, 0
            )
        }
        [void]$algorithm.TransformFinalBlock([byte[]]@(), 0, 0)
        if ($total -ne $ExpectedBytes) {
            throw 'Comparator archive entry ended before its recorded size'
        }
        return [pscustomobject][ordered]@{
            bytes = $total
            sha256 = [BitConverter]::ToString(
                $algorithm.Hash
            ).Replace('-', '').ToLowerInvariant()
        }
    } finally {
        $algorithm.Dispose()
    }
}

function Assert-KettlePerfComparatorSetupZipTree {
    param(
        [Parameter(Mandatory)]
        [string]$ArchivePath,
        [Parameter(Mandatory)]
        [string]$StagingRoot,
        [Parameter(Mandatory)]
        [string]$ExecutableEntry
    )

    $archive = $null
    try {
        $archive = Open-KettlePerfComparatorSetupZip $ArchivePath
        $plan = Get-KettlePerfComparatorSetupZipPlan `
            -Archive $archive -ExecutableEntry $ExecutableEntry
        $tree = Get-KettlePerfComparatorSetupTree $StagingRoot
        if ($tree.Count -ne $plan.paths.Count) {
            throw 'Comparator staged ZIP tree has missing or extra paths'
        }
        foreach ($path in $plan.paths.Keys) {
            if (
                -not $tree.ContainsKey($path) -or
                $tree[$path].kind -cne $plan.paths[$path]
            ) {
                throw "Comparator staged ZIP path differs: $path"
            }
        }
        foreach ($record in $plan.entries) {
            if ($record.kind -cne 'File') {
                continue
            }
            $staged = Get-KettlePerfComparatorSetupFileEvidence `
                -Path $tree[$record.relative_path].path
            if ([long]$staged.bytes -ne [long]$record.bytes) {
                throw (
                    'Comparator staged ZIP file size differs: ' +
                    $record.relative_path
                )
            }
            $entryStream = $null
            try {
                $entryStream = $record.entry.Open()
                $entryEvidence = (
                    Get-KettlePerfComparatorSetupStreamEvidence `
                        -Stream $entryStream `
                        -ExpectedBytes ([long]$record.bytes)
                )
            } finally {
                if ($null -ne $entryStream) {
                    $entryStream.Dispose()
                }
            }
            if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
                $staged.sha256,
                $entryEvidence.sha256
            )) {
                throw (
                    'Comparator staged ZIP file hash differs: ' +
                    $record.relative_path
                )
            }
        }
    } finally {
        if ($null -ne $archive) {
            $archive.Dispose()
        }
    }
}

function Assert-KettlePerfComparatorSetupDirectTree {
    param(
        [Parameter(Mandatory)]
        [string]$StagingRoot,
        [Parameter(Mandatory)]
        [string]$ExecutableLeaf
    )

    $tree = Get-KettlePerfComparatorSetupTree $StagingRoot
    if (
        $tree.Count -ne 1 -or
        -not $tree.ContainsKey($ExecutableLeaf) -or
        $tree[$ExecutableLeaf].kind -cne 'File'
    ) {
        throw 'Direct comparator staging must contain only its executable'
    }
}

function Get-KettlePerfComparatorSetupAuthenticode {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    [void](Assert-KettlePerfComparatorSetupPath -Path $Path -Kind File)
    $signature = Get-AuthenticodeSignature -LiteralPath $Path `
        -ErrorAction Stop
    $certificateHash = if ($null -eq $signature.SignerCertificate) {
        $null
    } else {
        Get-KettlePerfComparatorCertificateSha256 `
            -Certificate $signature.SignerCertificate
    }
    return [pscustomobject][ordered]@{
        status = [string]$signature.Status
        signer_cert_sha256 = $certificateHash
    }
}

function Assert-KettlePerfComparatorSetupSignature {
    param(
        [Parameter(Mandatory)]
        $Entry,
        [Parameter(Mandatory)]
        [string]$Path,
        [scriptblock]$SignatureProbe
    )

    $actual = if ($null -eq $SignatureProbe) {
        Get-KettlePerfComparatorSetupAuthenticode -Path $Path
    } else {
        & $SignatureProbe $Entry $Path
    }
    if ($null -eq $actual) {
        throw 'Comparator signature probe returned no evidence'
    }
    $actualStatus = $actual.PSObject.Properties['status']
    $actualCertificate = (
        $actual.PSObject.Properties['signer_cert_sha256']
    )
    if (
        $null -eq $actualStatus -or
        $null -eq $actualCertificate -or
        $actualStatus.Value -isnot [string] -or
        [string]$actualStatus.Value -cne
            [string]$Entry.executable.authenticode_status
    ) {
        throw "Comparator executable signature status differs: $($Entry.name)"
    }
    $expectedCertificate = $Entry.executable.signer_cert_sha256
    $actualCertificateValue = $actualCertificate.Value
    if ($null -eq $expectedCertificate) {
        if ($null -ne $actualCertificateValue) {
            throw (
                'Unsigned comparator unexpectedly returned a signer: ' +
                $Entry.name
            )
        }
    } elseif (
        $actualCertificateValue -isnot [string] -or
        $actualCertificateValue -cnotmatch '^[0-9A-Fa-f]{64}$' -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$actualCertificateValue,
            [string]$expectedCertificate
        )
    ) {
        throw "Comparator signer certificate differs: $($Entry.name)"
    }
    return [pscustomobject][ordered]@{
        status = [string]$actualStatus.Value
        signer_cert_sha256 = $actualCertificateValue
    }
}

function Assert-KettlePerfComparatorSetupDirectoryChildSet {
    param(
        [Parameter(Mandatory)]
        [string]$Directory,
        [Parameter(Mandatory)]
        [Collections.IDictionary]$Expected
    )

    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $Directory -Kind Directory)
    $actual = [Collections.Generic.Dictionary[string, string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($path in [IO.Directory]::EnumerateFileSystemEntries($Directory)) {
        $information = [KettlePerfComparatorSetup.Native]::Inspect($path)
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $information.FinalPath,
                [IO.Path]::GetFullPath($path)
            ) -or
            (Test-KettlePerfComparatorSetupIsReparse `
                $information.Attributes)
        ) {
            throw 'Comparator campaign directory contains a reparse point'
        }
        $leaf = [IO.Path]::GetFileName($path)
        $kind = if (Test-KettlePerfComparatorSetupIsDirectory `
            $information.Attributes
        ) {
            'Directory'
        } else {
            if ([uint32]$information.LinkCount -ne 1) {
                throw "Comparator campaign contains a hardlink: $leaf"
            }
            'File'
        }
        if ($actual.ContainsKey($leaf)) {
            throw "Comparator campaign has an ambiguous child: $leaf"
        }
        $actual.Add($leaf, $kind)
    }
    if ($actual.Count -ne $Expected.Count) {
        throw 'Comparator campaign directory has missing or extra children'
    }
    foreach ($leaf in $Expected.Keys) {
        if (
            -not $actual.ContainsKey([string]$leaf) -or
            $actual[[string]$leaf] -cne [string]$Expected[$leaf]
        ) {
            throw "Comparator campaign directory child differs: $leaf"
        }
    }
}

function Get-KettlePerfComparatorSetupAssetPath {
    param(
        [Parameter(Mandatory)]
        [string]$CampaignRoot,
        [Parameter(Mandatory)]
        $Entry
    )

    return Join-Path (
        Join-Path (
            Join-Path $CampaignRoot 'assets'
        ) ([string]$Entry.name)
    ) ([string]$Entry.source.asset.name)
}

function Get-KettlePerfComparatorSetupStagingDirectory {
    param(
        [Parameter(Mandatory)]
        [string]$CampaignRoot,
        [Parameter(Mandatory)]
        $Entry
    )

    return Join-Path (
        Join-Path (
            Join-Path $CampaignRoot 'staging'
        ) ([string]$Entry.name)
    ) ([string]$Entry.version)
}

function Get-KettlePerfComparatorSetupTreeAggregate {
    param(
        [Parameter(Mandatory)]
        [string]$StagingRoot
    )

    $tree = Get-KettlePerfComparatorSetupTree $StagingRoot
    $filePaths = [Collections.Generic.List[string]]::new()
    foreach ($relative in $tree.Keys) {
        if ($tree[$relative].kind -ceq 'File') {
            $filePaths.Add([string]$relative)
        }
    }
    if (
        $filePaths.Count -lt 1 -or
        $filePaths.Count -gt
            $script:KettlePerfComparatorSetupMaximumEntries
    ) {
        throw 'Comparator staged tree file count is outside its bound'
    }
    $orderedPaths = [string[]]$filePaths.ToArray()
    [Array]::Sort($orderedPaths, [StringComparer]::Ordinal)
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    [long]$totalBytes = 0
    try {
        $header = $encoding.GetBytes(
            "kettle-comparator-staged-tree-v1`n"
        )
        [void]$algorithm.TransformBlock(
            $header, 0, $header.Length, $null, 0
        )
        foreach ($relative in $orderedPaths) {
            $evidence = Get-KettlePerfComparatorSetupFileEvidence `
                -Path $tree[$relative].path
            $totalBytes = [long](
                $totalBytes + [long]$evidence.bytes
            )
            if (
                $totalBytes -gt
                $script:KettlePerfComparatorSetupMaximumExpandedBytes
            ) {
                throw 'Comparator staged tree exceeds its byte bound'
            }
            $line = (
                $relative + [char]0 +
                ([long]$evidence.bytes).ToString(
                    [Globalization.CultureInfo]::InvariantCulture
                ) + [char]0 +
                [string]$evidence.sha256 + "`n"
            )
            $lineBytes = $encoding.GetBytes($line)
            [void]$algorithm.TransformBlock(
                $lineBytes, 0, $lineBytes.Length, $null, 0
            )
        }
        [void]$algorithm.TransformFinalBlock([byte[]]@(), 0, 0)
        return [pscustomobject][ordered]@{
            staged_file_count = [int]$orderedPaths.Count
            staged_total_bytes = $totalBytes
            staged_tree_sha256 = [BitConverter]::ToString(
                $algorithm.Hash
            ).Replace('-', '').ToLowerInvariant()
        }
    } finally {
        $algorithm.Dispose()
    }
}

function Assert-KettlePerfComparatorCampaignInstallation {
    param(
        [Parameter(Mandatory)]
        [string]$CampaignRoot,
        [Parameter(Mandatory)]
        [string]$CampaignsRoot,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$ExpectedCampaignSha256,
        [scriptblock]$SignatureProbe
    )

    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $CampaignRoot -Kind Directory)
    Assert-KettlePerfComparatorSetupDirectoryChildSet `
        -Directory $CampaignRoot -Expected ([ordered]@{
            'campaign.json' = 'File'
            'assets' = 'Directory'
            'staging' = 'Directory'
        })
    $campaignPath = Join-Path $CampaignRoot 'campaign.json'
    $campaign = Read-KettlePerfComparatorCampaign `
        -Path $campaignPath -ExpectedCampaignRoot $CampaignsRoot
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        [string]$campaign.campaign_file.sha256,
        $ExpectedCampaignSha256
    )) {
        throw 'Installed comparator campaign manifest differs from source'
    }

    $expectedTerminalDirectories = [ordered]@{}
    foreach ($entry in $campaign.terminals) {
        $expectedTerminalDirectories[[string]$entry.name] = 'Directory'
    }
    $assetsRoot = Join-Path $CampaignRoot 'assets'
    $stagingRoot = Join-Path $CampaignRoot 'staging'
    Assert-KettlePerfComparatorSetupDirectoryChildSet `
        -Directory $assetsRoot -Expected $expectedTerminalDirectories
    Assert-KettlePerfComparatorSetupDirectoryChildSet `
        -Directory $stagingRoot -Expected $expectedTerminalDirectories

    foreach ($entry in $campaign.terminals) {
        $assetDirectory = Join-Path $assetsRoot ([string]$entry.name)
        Assert-KettlePerfComparatorSetupDirectoryChildSet `
            -Directory $assetDirectory -Expected ([ordered]@{
                ([string]$entry.source.asset.name) = 'File'
            })
        $assetPath = Get-KettlePerfComparatorSetupAssetPath `
            -CampaignRoot $CampaignRoot -Entry $entry
        [void](Assert-KettlePerfComparatorSetupEvidence `
            -Path $assetPath `
            -ExpectedBytes ([long]$entry.source.asset.bytes) `
            -ExpectedSha256 ([string]$entry.source.asset.sha256))

        $terminalStaging = Join-Path $stagingRoot ([string]$entry.name)
        Assert-KettlePerfComparatorSetupDirectoryChildSet `
            -Directory $terminalStaging -Expected ([ordered]@{
                ([string]$entry.version) = 'Directory'
            })
        $versionRoot = Get-KettlePerfComparatorSetupStagingDirectory `
            -CampaignRoot $CampaignRoot -Entry $entry
        if ($entry.source.asset.kind -ceq 'zip') {
            Assert-KettlePerfComparatorSetupZipTree `
                -ArchivePath $assetPath -StagingRoot $versionRoot `
                -ExecutableEntry (
                    [string]$entry.source.asset.executable_entry
                )
        } else {
            Assert-KettlePerfComparatorSetupDirectTree `
                -StagingRoot $versionRoot `
                -ExecutableLeaf ([string]$entry.executable.leaf)
        }

        $executablePath = Join-Path $versionRoot (
            [string]$entry.executable.leaf
        )
        [void](Assert-KettlePerfComparatorSetupEvidence `
            -Path $executablePath `
            -ExpectedBytes ([long]$entry.executable.bytes) `
            -ExpectedSha256 ([string]$entry.executable.sha256))
        [void](Assert-KettlePerfComparatorSetupSignature `
            -Entry $entry -Path $executablePath `
            -SignatureProbe $SignatureProbe)
        $aggregate = Get-KettlePerfComparatorSetupTreeAggregate `
            -StagingRoot $versionRoot
        if (
            [int]$aggregate.staged_file_count -ne
                [int]$entry.source.asset.staged_file_count -or
            [long]$aggregate.staged_total_bytes -ne
                [long]$entry.source.asset.staged_total_bytes -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string]$aggregate.staged_tree_sha256,
                [string]$entry.source.asset.staged_tree_sha256
            )
        ) {
            throw "Comparator staged tree aggregate differs: $($entry.name)"
        }
    }
    return $campaign
}

function Remove-KettlePerfComparatorSetupEmptyParent {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Only one validated, empty, invocation-owned directory is removed.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$ExpectedParent,
        [scriptblock]$BeforeDelete
    )

    $fullPath = [IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    $parent = [IO.Path]::GetFullPath($ExpectedParent).TrimEnd('\', '/')
    if (
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetDirectoryName($fullPath).TrimEnd('\', '/'),
            $parent
        ) -or
        [IO.Path]::GetFileName($fullPath) -cnotmatch
            '^\.campaign-setup-[0-9a-f]{32}$'
    ) {
        throw 'Refusing to remove a non-owned comparator setup directory'
    }
    if (-not [IO.Directory]::Exists($fullPath)) {
        if ([IO.File]::Exists($fullPath)) {
            throw 'Comparator setup temporary parent became a file'
        }
        return
    }

    if ($null -ne $BeforeDelete) {
        & $BeforeDelete $fullPath
    }
    # Never enumerate or recurse through an invocation temp tree during
    # cleanup. Directory.Delete(path, false) either removes this exact empty
    # directory entry or fails closed if an attacker or failed acquisition
    # left any child behind.
    [IO.Directory]::Delete($fullPath, $false)
}

function Get-KettlePerfComparatorSetupRoot {
    $localAppData = [Environment]::GetEnvironmentVariable(
        'LOCALAPPDATA',
        [EnvironmentVariableTarget]::Process
    )
    if (-not $localAppData) {
        throw 'LOCALAPPDATA is required for comparator campaign staging'
    }
    $localRoot = [IO.Path]::GetFullPath($localAppData).TrimEnd('\', '/')
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $localRoot -Kind Directory)
    return New-KettlePerfComparatorSetupDirectory `
        -Path (Join-Path $localRoot 'KettleBench') `
        -ExpectedParent $localRoot
}

function Invoke-KettlePerfComparatorCampaignSetupCore {
    param(
        [Parameter(Mandatory)]
        [string]$CampaignPath,
        [Parameter(Mandatory)]
        [string]$CampaignSourceRoot,
        [Parameter(Mandatory)]
        [string]$KettleBenchRoot,
        [switch]$Offline,
        [scriptblock]$FetchAsset,
        [scriptblock]$SignatureProbe,
        [scriptblock]$BeforeTemporaryParentDelete
    )

    if ($env:OS -cne 'Windows_NT') {
        throw [PlatformNotSupportedException]::new(
            'Windows comparator campaigns can only be staged on Windows'
        )
    }
    Initialize-KettlePerfComparatorSetupNative

    $benchRoot = [IO.Path]::GetFullPath(
        $KettleBenchRoot
    ).TrimEnd('\', '/')
    if (
        -not [StringComparer]::Ordinal.Equals(
            [IO.Path]::GetFileName($benchRoot),
            'KettleBench'
        )
    ) {
        throw 'Comparator setup root must be the KettleBench directory'
    }
    $benchParent = [IO.Path]::GetDirectoryName($benchRoot)
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $benchParent -Kind Directory)
    if (-not [IO.Directory]::Exists($benchRoot)) {
        $benchRoot = New-KettlePerfComparatorSetupDirectory `
            -Path $benchRoot -ExpectedParent $benchParent
    } else {
        [void](Assert-KettlePerfComparatorSetupPath `
            -Path $benchRoot -Kind Directory)
    }

    $sourceCampaign = Read-KettlePerfComparatorCampaign `
        -Path $CampaignPath -ExpectedCampaignRoot $CampaignSourceRoot
    $sourceCampaignSha = [string]$sourceCampaign.campaign_file.sha256
    $campaignsRoot = Join-Path $benchRoot 'campaigns'
    if (-not [IO.Directory]::Exists($campaignsRoot)) {
        $campaignsRoot = New-KettlePerfComparatorSetupDirectory `
            -Path $campaignsRoot -ExpectedParent $benchRoot
    } else {
        [void](Assert-KettlePerfComparatorSetupPath `
            -Path $campaignsRoot -Kind Directory)
    }
    $finalCampaignRoot = Join-Path $campaignsRoot (
        [string]$sourceCampaign.campaign_id
    )
    if ([IO.Directory]::Exists($finalCampaignRoot)) {
        try {
            $verified = (
                Assert-KettlePerfComparatorCampaignInstallation `
                    -CampaignRoot $finalCampaignRoot `
                    -CampaignsRoot $campaignsRoot `
                    -ExpectedCampaignSha256 $sourceCampaignSha `
                    -SignatureProbe $SignatureProbe
            )
        } catch {
            throw (
                'Existing append-only comparator campaign failed full ' +
                "verification: $($_.Exception.Message)"
            )
        }
        return [pscustomobject][ordered]@{
            schema = 'kettle-comparator-campaign-setup-v1'
            reused = $true
            campaign_root = $finalCampaignRoot
            campaign_path = Join-Path $finalCampaignRoot 'campaign.json'
            campaigns_root = $campaignsRoot
            campaign = $verified
        }
    }
    if ([IO.File]::Exists($finalCampaignRoot)) {
        throw 'Comparator campaign destination collides with a file'
    }
    if ($Offline) {
        throw 'Offline comparator setup requires a fully verified local campaign'
    }

    $temporaryLeaf = (
        '.campaign-setup-' + [Guid]::NewGuid().ToString('N')
    )
    $temporaryParent = New-KettlePerfComparatorSetupDirectory `
        -Path (Join-Path $benchRoot $temporaryLeaf) `
        -ExpectedParent $benchRoot
    $temporaryCampaignRoot = $null
    $moved = $false
    try {
        $temporaryCampaignRoot = New-KettlePerfComparatorSetupDirectory `
            -Path (Join-Path $temporaryParent (
                [string]$sourceCampaign.campaign_id
            )) -ExpectedParent $temporaryParent
        $assetsRoot = New-KettlePerfComparatorSetupDirectory `
            -Path (Join-Path $temporaryCampaignRoot 'assets') `
            -ExpectedParent $temporaryCampaignRoot
        $stagingRoot = New-KettlePerfComparatorSetupDirectory `
            -Path (Join-Path $temporaryCampaignRoot 'staging') `
            -ExpectedParent $temporaryCampaignRoot

        foreach ($entry in $sourceCampaign.terminals) {
            [void](Assert-KettlePerfComparatorSetupOfficialUri `
                -Uri ([string]$entry.source.asset.url))
            $assetTerminalRoot = New-KettlePerfComparatorSetupDirectory `
                -Path (Join-Path $assetsRoot ([string]$entry.name)) `
                -ExpectedParent $assetsRoot
            $assetPath = Join-Path $assetTerminalRoot (
                [string]$entry.source.asset.name
            )
            if ($null -eq $FetchAsset) {
                Invoke-KettlePerfComparatorSetupHttpsDownload `
                    -Entry $entry -Destination $assetPath
            } else {
                & $FetchAsset $entry $assetPath
            }
            [void](Assert-KettlePerfComparatorSetupEvidence `
                -Path $assetPath `
                -ExpectedBytes ([long]$entry.source.asset.bytes) `
                -ExpectedSha256 ([string]$entry.source.asset.sha256))

            $terminalRoot = New-KettlePerfComparatorSetupDirectory `
                -Path (Join-Path $stagingRoot ([string]$entry.name)) `
                -ExpectedParent $stagingRoot
            $versionRoot = New-KettlePerfComparatorSetupDirectory `
                -Path (Join-Path $terminalRoot ([string]$entry.version)) `
                -ExpectedParent $terminalRoot
            if ($entry.source.asset.kind -ceq 'zip') {
                Expand-KettlePerfComparatorSetupZip `
                    -ArchivePath $assetPath -Destination $versionRoot `
                    -ExecutableEntry (
                        [string]$entry.source.asset.executable_entry
                    )
            } else {
                Copy-KettlePerfComparatorSetupFile `
                    -Source $assetPath `
                    -Destination (Join-Path $versionRoot (
                        [string]$entry.executable.leaf
                    ))
            }
            $executablePath = Join-Path $versionRoot (
                [string]$entry.executable.leaf
            )
            [void](Assert-KettlePerfComparatorSetupEvidence `
                -Path $executablePath `
                -ExpectedBytes ([long]$entry.executable.bytes) `
                -ExpectedSha256 ([string]$entry.executable.sha256))
            [void](Assert-KettlePerfComparatorSetupSignature `
                -Entry $entry -Path $executablePath `
                -SignatureProbe $SignatureProbe)
        }

        Copy-KettlePerfComparatorSetupFile `
            -Source ([string]$sourceCampaign.campaign_file.path) `
            -Destination (Join-Path $temporaryCampaignRoot 'campaign.json')
        [void](Assert-KettlePerfComparatorCampaignInstallation `
            -CampaignRoot $temporaryCampaignRoot `
            -CampaignsRoot $temporaryParent `
            -ExpectedCampaignSha256 $sourceCampaignSha `
            -SignatureProbe $SignatureProbe)

        try {
            [IO.Directory]::Move(
                $temporaryCampaignRoot,
                $finalCampaignRoot
            )
            $moved = $true
        } catch {
            if (-not [IO.Directory]::Exists($finalCampaignRoot)) {
                throw
            }
            $raced = (
                Assert-KettlePerfComparatorCampaignInstallation `
                    -CampaignRoot $finalCampaignRoot `
                    -CampaignsRoot $campaignsRoot `
                    -ExpectedCampaignSha256 $sourceCampaignSha `
                    -SignatureProbe $SignatureProbe
            )
            return [pscustomobject][ordered]@{
                schema = 'kettle-comparator-campaign-setup-v1'
                reused = $true
                campaign_root = $finalCampaignRoot
                campaign_path = Join-Path $finalCampaignRoot 'campaign.json'
                campaigns_root = $campaignsRoot
                campaign = $raced
            }
        }

        $verified = (
            Assert-KettlePerfComparatorCampaignInstallation `
                -CampaignRoot $finalCampaignRoot `
                -CampaignsRoot $campaignsRoot `
                -ExpectedCampaignSha256 $sourceCampaignSha `
                -SignatureProbe $SignatureProbe
        )
        return [pscustomobject][ordered]@{
            schema = 'kettle-comparator-campaign-setup-v1'
            reused = $false
            campaign_root = $finalCampaignRoot
            campaign_path = Join-Path $finalCampaignRoot 'campaign.json'
            campaigns_root = $campaignsRoot
            campaign = $verified
        }
    } finally {
        if ($moved) {
            try {
                Remove-KettlePerfComparatorSetupEmptyParent `
                    -Path $temporaryParent -ExpectedParent $benchRoot `
                    -BeforeDelete $BeforeTemporaryParentDelete
            } catch {
                Write-Warning (
                    'Comparator campaign was published, but its temporary ' +
                    'parent was not empty and was retained without ' +
                    "recursive cleanup: $temporaryParent. Inspect and " +
                    'remove this exact directory manually after verifying ' +
                    "it is not a reparse point. Detail: $($_.Exception.Message)"
                )
            }
        } else {
            Write-Warning (
                'Comparator setup did not publish its invocation-owned ' +
                'temporary tree. It was retained; no recursive cleanup was ' +
                'attempted. Inspect this exact path for retry or manual ' +
                "removal: $temporaryParent"
            )
        }
        if (
            -not $moved -and
            [IO.Directory]::Exists($finalCampaignRoot)
        ) {
            # A competing invocation may have won the atomic move. It is never
            # removed here; the append-only destination is verification-only.
        }
    }
}

function Install-KettlePerfComparatorCampaign {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Stages an immutable campaign and never overwrites one.'
    )]
    param(
        [Parameter(Mandatory)]
        [ValidatePattern(
            '^windows-x86_64-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{16}$'
        )]
        [string]$CampaignId,
        [switch]$Offline
    )

    $campaignSourceRoot = Join-Path $PSScriptRoot 'campaigns'
    [void](Assert-KettlePerfComparatorSetupPath `
        -Path $campaignSourceRoot -Kind Directory)
    $campaignDirectory = Join-Path $campaignSourceRoot $CampaignId
    $campaignPath = Join-Path $campaignDirectory 'campaign.json'
    $benchRoot = Get-KettlePerfComparatorSetupRoot
    $result = Invoke-KettlePerfComparatorCampaignSetupCore `
        -CampaignPath $campaignPath `
        -CampaignSourceRoot $campaignSourceRoot `
        -KettleBenchRoot $benchRoot -Offline:$Offline

    # Re-enter through the runtime API after setup. This independently
    # re-reads the retained manifest and verifies every executable's path,
    # bytes, hash, and Authenticode identity.
    foreach ($entry in $result.campaign.terminals) {
        [void](Resolve-KettlePerfComparatorCampaignExecutable `
            -Campaign $result.campaign -Entry $entry `
            -CampaignRoot $result.campaigns_root `
            -StagingRoot $result.campaigns_root)
    }
    return $result
}

if ($MyInvocation.InvocationName -cne '.') {
    $setupResult = Install-KettlePerfComparatorCampaign `
        -CampaignId $CampaignId -Offline:$Offline
    if ($PassThru) {
        $setupResult
    } else {
        Write-Output (
            'Comparator campaign ready: ' + $setupResult.campaign_path
        )
        Write-Output (
            'Measurement is offline; retained assets and staged trees are ' +
            'fully verified.'
        )
    }
}
