# Exact, immutable Windows launcher selection for WSL-backed benchmarks.
. "$PSScriptRoot\process-capture.ps1"

$script:KettlePerfWslResolutionPolicy =
    'program-files-wsl-then-system32-v1'

function Initialize-KettlePerfWslLauncherNative {
    if ('KettlePerfWslLauncher.NativeMethods' -as [type]) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfWslLauncher {
    public sealed class ProcessResult {
        public int ExitCode { get; internal set; }
        public byte[] StandardOutput { get; internal set; }
        public byte[] StandardError { get; internal set; }
    }

    public static class NativeMethods {
        private const uint FileReadData = 0x00000001;
        private const uint FileReadAttributes = 0x00000080;
        private const uint FileShareRead = 0x00000001;
        private const uint OpenExisting = 3;
        private const uint FileAttributeDirectory = 0x00000010;
        private const uint FileAttributeReparsePoint = 0x00000400;
        private const uint FileFlagOpenReparsePoint = 0x00200000;

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

        public static SafeFileHandle OpenPinnedFile(string path) {
            var handle = CreateFile(
                path,
                FileReadData | FileReadAttributes,
                FileShareRead,
                IntPtr.Zero,
                OpenExisting,
                FileFlagOpenReparsePoint,
                IntPtr.Zero);
            if (handle.IsInvalid) {
                var error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(
                    error, "Opening the pinned WSL launcher failed");
            }

            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information)) {
                var error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(
                    error, "Inspecting the pinned WSL launcher failed");
            }
            if ((information.FileAttributes & FileAttributeDirectory) != 0 ||
                (information.FileAttributes & FileAttributeReparsePoint) != 0) {
                handle.Dispose();
                throw new InvalidOperationException(
                    "The WSL launcher must be an ordinary file");
            }

            var finalPath = ConvertFinalPath(GetFinalPath(handle));
            if (!string.Equals(
                    Path.GetFullPath(path),
                    Path.GetFullPath(finalPath),
                    StringComparison.OrdinalIgnoreCase)) {
                handle.Dispose();
                throw new InvalidOperationException(
                    "The WSL launcher handle resolved to an unexpected path");
            }
            return handle;
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
                        "Resolving the pinned WSL launcher handle failed");
                }
                if (length < result.Capacity) {
                    return result.ToString();
                }
                capacity = checked((int)length + 1);
            }
            throw new InvalidOperationException(
                "The WSL launcher path exceeds its evidence bound");
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

        public static ProcessResult RunBounded(
            string fileName,
            string arguments,
            int timeoutMilliseconds,
            int maximumStandardOutputBytes,
            int maximumStandardErrorBytes,
            bool createNoWindow) {
            if (timeoutMilliseconds < 1 ||
                maximumStandardOutputBytes < 0 ||
                maximumStandardErrorBytes < 0) {
                throw new ArgumentOutOfRangeException();
            }
            var startInfo = new ProcessStartInfo();
            startInfo.FileName = fileName;
            startInfo.Arguments = arguments;
            startInfo.UseShellExecute = false;
            startInfo.CreateNoWindow = createNoWindow;
            startInfo.RedirectStandardOutput =
                maximumStandardOutputBytes > 0;
            startInfo.RedirectStandardError =
                maximumStandardErrorBytes > 0;

            using (var process = new Process()) {
                process.StartInfo = startInfo;
                if (!process.Start()) {
                    throw new InvalidOperationException(
                        "Could not start bounded child process");
                }
                Task<byte[]> stdoutTask = null;
                Task<byte[]> stderrTask = null;
                if (startInfo.RedirectStandardOutput) {
                    stdoutTask = ReadBounded(
                        process.StandardOutput.BaseStream,
                        maximumStandardOutputBytes,
                        "standard output");
                }
                if (startInfo.RedirectStandardError) {
                    stderrTask = ReadBounded(
                        process.StandardError.BaseStream,
                        maximumStandardErrorBytes,
                        "standard error");
                }

                var timer = Stopwatch.StartNew();
                try {
                    while (!process.WaitForExit(50)) {
                        ThrowTaskFailure(stdoutTask);
                        ThrowTaskFailure(stderrTask);
                        if (timer.ElapsedMilliseconds >=
                                timeoutMilliseconds) {
                            throw new TimeoutException(
                                "Bounded child process timed out");
                        }
                    }
                    ThrowTaskFailure(stdoutTask);
                    ThrowTaskFailure(stderrTask);
                    WaitReader(stdoutTask);
                    WaitReader(stderrTask);
                    return new ProcessResult {
                        ExitCode = process.ExitCode,
                        StandardOutput = stdoutTask == null ?
                            new byte[0] : stdoutTask.Result,
                        StandardError = stderrTask == null ?
                            new byte[0] : stderrTask.Result
                    };
                } catch {
                    if (!process.HasExited) {
                        try {
                            process.Kill();
                        } catch (InvalidOperationException) {
                        }
                        process.WaitForExit(5000);
                    }
                    throw;
                }
            }
        }

        private static Task<byte[]> ReadBounded(
            Stream stream,
            int maximumBytes,
            string name) {
            return Task.Factory.StartNew(() => {
                using (var output = new MemoryStream()) {
                    var buffer = new byte[4096];
                    while (true) {
                        var remaining = maximumBytes - (int)output.Length;
                        var requested = Math.Min(
                            buffer.Length,
                            Math.Max(1, remaining + 1));
                        var read = stream.Read(buffer, 0, requested);
                        if (read == 0) {
                            return output.ToArray();
                        }
                        if (read > remaining) {
                            throw new InvalidDataException(
                                "Bounded child " + name +
                                " exceeded its byte limit");
                        }
                        output.Write(buffer, 0, read);
                    }
                }
            }, TaskCreationOptions.LongRunning);
        }

        private static void WaitReader(Task<byte[]> task) {
            if (task == null) {
                return;
            }
            if (!task.Wait(5000)) {
                throw new TimeoutException(
                    "Bounded child stream did not close");
            }
            ThrowTaskFailure(task);
        }

        private static void ThrowTaskFailure(Task<byte[]> task) {
            if (task == null || !task.IsFaulted) {
                return;
            }
            var flattened = task.Exception.Flatten();
            throw flattened.InnerExceptions[0];
        }
    }
}
'@
}

function Invoke-KettlePerfWslBoundedProcessBytes {
    param(
        [Parameter(Mandatory)]
        [string]$FilePath,
        [Parameter(Mandatory)]
        [string]$Arguments,
        [ValidateRange(1, 86400000)]
        [int]$TimeoutMs,
        [ValidateRange(0, 1048576)]
        [int]$MaximumStandardOutputBytes = 32768,
        [ValidateRange(0, 1048576)]
        [int]$MaximumStandardErrorBytes = 32768,
        [switch]$CreateNoWindow
    )

    Initialize-KettlePerfWslLauncherNative
    return [KettlePerfWslLauncher.NativeMethods]::RunBounded(
        $FilePath,
        $Arguments,
        $TimeoutMs,
        $MaximumStandardOutputBytes,
        $MaximumStandardErrorBytes,
        [bool]$CreateNoWindow
    )
}

function ConvertFrom-KettlePerfWslUnicodeBytes {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [byte[]]$Bytes,
        [string]$Source = 'WSL output'
    )

    if (($Bytes.Length % 2) -ne 0) {
        throw "$Source is not complete UTF-16LE"
    }
    try {
        return [Text.UnicodeEncoding]::new(
            $false,
            $false,
            $true
        ).GetString($Bytes)
    } catch {
        throw "$Source is not strict UTF-16LE"
    }
}

function ConvertFrom-KettlePerfWslUtf8Bytes {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [byte[]]$Bytes,
        [string]$Source = 'WSL distribution output'
    )

    try {
        return [Text.UTF8Encoding]::new(
            $false,
            $true
        ).GetString($Bytes)
    } catch {
        throw "$Source is not strict UTF-8"
    }
}

function Resolve-KettlePerfWslLauncherPath {
    param(
        [string]$Path = ''
    )

    $candidate = $Path
    if (-not $candidate) {
        $programFilesLauncher = Join-Path $env:ProgramFiles 'WSL\wsl.exe'
        $systemLauncher = Join-Path $env:SystemRoot 'System32\wsl.exe'
        if (Test-Path -LiteralPath $programFilesLauncher -PathType Leaf) {
            $candidate = $programFilesLauncher
        } else {
            $candidate = $systemLauncher
        }
    }
    if (-not [IO.Path]::IsPathRooted($candidate)) {
        throw 'The WSL launcher path must be absolute'
    }
    $full = [IO.Path]::GetFullPath($candidate)
    if (
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetFileName($full),
            'wsl.exe'
        )
    ) {
        throw 'The WSL launcher must be named wsl.exe'
    }
    $item = Get-Item -LiteralPath $full -Force -ErrorAction Stop
    if (
        -not ($item -is [IO.FileInfo]) -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw 'The WSL launcher must be an ordinary file'
    }
    return $item.FullName
}

function Invoke-KettlePerfWslVersion {
    param(
        [Parameter(Mandatory)]
        [string]$WslExe,
        [ValidateRange(1000, 60000)]
        [int]$TimeoutMs = 10000
    )

    $result = Invoke-KettlePerfWslBoundedProcessBytes `
        -FilePath $WslExe -Arguments '--version' `
        -TimeoutMs $TimeoutMs `
        -MaximumStandardOutputBytes 32768 `
        -MaximumStandardErrorBytes 4096 -CreateNoWindow
    try {
        if ($result.ExitCode -ne 0) {
            throw (
                'Pinned WSL launcher --version failed with exit ' +
                $result.ExitCode
            )
        }
        $stdout = ConvertFrom-KettlePerfWslUnicodeBytes `
            -Bytes $result.StandardOutput -Source 'WSL --version output'
        $stderr = ConvertFrom-KettlePerfWslUnicodeBytes `
            -Bytes $result.StandardError -Source 'WSL --version error'
        $combined = ($stdout + $stderr).Replace("`r`n", "`n").
            Replace("`r", "`n").Trim()
        if (-not $combined -or $combined.Length -gt 32768) {
            throw 'Pinned WSL launcher returned invalid --version output'
        }
        $firstLine = @($combined -split "`n" | Where-Object { $_ })[0]
        $runtimeVersion = if (
            $firstLine -match '([0-9]+(?:\.[0-9]+){1,3})'
        ) {
            $Matches[1]
        } else {
            throw 'Pinned WSL launcher omitted its runtime version identity'
        }
        return [pscustomobject]@{
            RuntimeVersion = $runtimeVersion
            Output = $combined
        }
    } finally {
        [Array]::Clear(
            $result.StandardOutput,
            0,
            $result.StandardOutput.Length
        )
        [Array]::Clear(
            $result.StandardError,
            0,
            $result.StandardError.Length
        )
    }
}

function Open-KettlePerfWslLauncherEvidence {
    param(
        [string]$Path = '',
        [ValidateSet(
            'program-files-wsl-then-system32-v1',
            'explicit-override-v1'
        )]
        [string]$ResolutionPolicy = ''
    )

    $explicit = [bool]$Path
    $resolved = Resolve-KettlePerfWslLauncherPath -Path $Path
    if (-not $ResolutionPolicy) {
        $ResolutionPolicy = if ($explicit) {
            'explicit-override-v1'
        } else {
            $script:KettlePerfWslResolutionPolicy
        }
    }
    Initialize-KettlePerfWslLauncherNative
    $handle = [KettlePerfWslLauncher.NativeMethods]::OpenPinnedFile(
        $resolved
    )
    $stream = $null
    try {
        $stream = [IO.FileStream]::new($handle, [IO.FileAccess]::Read)
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            $digest = $sha.ComputeHash($stream)
        } finally {
            $sha.Dispose()
            $stream.Position = 0
        }
        $item = Get-Item -LiteralPath $resolved -Force -ErrorAction Stop
        $fileVersion = [string]$item.VersionInfo.FileVersion
        $productVersion = [string]$item.VersionInfo.ProductVersion
        if (-not $fileVersion -or -not $productVersion) {
            throw 'Pinned WSL launcher lacks file-version provenance'
        }
        $runtime = Invoke-KettlePerfWslVersion -WslExe $resolved
        $outputBytes = [Text.UTF8Encoding]::new(
            $false,
            $true
        ).GetBytes($runtime.Output)
        $outputSha = [Security.Cryptography.SHA256]::Create()
        try {
            $outputDigest = $outputSha.ComputeHash($outputBytes)
        } finally {
            $outputSha.Dispose()
            [Array]::Clear($outputBytes, 0, $outputBytes.Length)
        }
        return [pscustomobject]@{
            Path = $resolved
            Sha256 = (
                [BitConverter]::ToString($digest).Replace('-', '').
                    ToLowerInvariant()
            )
            Version = $productVersion
            FileVersion = $fileVersion
            RuntimeVersion = $runtime.RuntimeVersion
            VersionOutput = $runtime.Output
            VersionOutputSha256 = (
                [BitConverter]::ToString($outputDigest).Replace('-', '').
                    ToLowerInvariant()
            )
            ResolutionPolicy = $ResolutionPolicy
            Stream = $stream
        }
    } catch {
        if ($null -ne $stream) {
            $stream.Dispose()
        } else {
            $handle.Dispose()
        }
        throw
    }
}

function Assert-KettlePerfWslDistributionName {
    param(
        [Parameter(Mandatory)]
        [string]$Name
    )

    if ($Name -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$') {
        throw (
            'WSL distribution names used for benchmark evidence must use ' +
            'only ASCII letters, digits, dot, underscore, and hyphen'
        )
    }
    return $Name
}

function Get-KettlePerfWslDistributionNames {
    param(
        [Parameter(Mandatory)]
        [string]$WslExe
    )

    $result = Invoke-KettlePerfWslBoundedProcessBytes `
        -FilePath $WslExe -Arguments '--list --quiet' `
        -TimeoutMs 10000 -MaximumStandardOutputBytes 32768 `
        -MaximumStandardErrorBytes 4096 -CreateNoWindow
    try {
        if ($result.ExitCode -ne 0) {
            throw (
                'Pinned WSL launcher could not list distributions; exit ' +
                $result.ExitCode
            )
        }
        $text = ConvertFrom-KettlePerfWslUnicodeBytes `
            -Bytes $result.StandardOutput `
            -Source 'WSL distribution list'
        $names = [Collections.Generic.List[string]]::new()
        foreach ($line in $text.Replace("`r`n", "`n").Split([char]10)) {
            $name = $line.Trim()
            if (-not $name) {
                continue
            }
            $name = Assert-KettlePerfWslDistributionName $name
            if ($names.Contains($name)) {
                throw 'Pinned WSL launcher returned duplicate distributions'
            }
            $names.Add($name)
        }
        if ($names.Count -eq 0) {
            throw 'Pinned WSL launcher has no registered distribution'
        }
        return [string[]]$names.ToArray()
    } finally {
        [Array]::Clear(
            $result.StandardOutput,
            0,
            $result.StandardOutput.Length
        )
        [Array]::Clear(
            $result.StandardError,
            0,
            $result.StandardError.Length
        )
    }
}

function Resolve-KettlePerfWslDistribution {
    param(
        [Parameter(Mandatory)]
        [string]$WslExe,
        [string]$Name = ''
    )

    $candidate = $Name
    if (-not $candidate) {
        $registryRoot = (
            'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
        )
        $defaultId = [string](
            Get-ItemProperty -LiteralPath $registryRoot `
                -Name DefaultDistribution -ErrorAction Stop
        ).DefaultDistribution
        if ($defaultId -cnotmatch '^\{[0-9a-fA-F-]{36}\}$') {
            throw 'WSL default-distribution registry identity is invalid'
        }
        $candidate = [string](
            Get-ItemProperty `
                -LiteralPath (Join-Path $registryRoot $defaultId) `
                -Name DistributionName -ErrorAction Stop
        ).DistributionName
    }
    $candidate = Assert-KettlePerfWslDistributionName $candidate
    $matches = @(
        Get-KettlePerfWslDistributionNames -WslExe $WslExe |
            Where-Object {
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    $_,
                    $candidate
                )
            }
    )
    if ($matches.Count -ne 1) {
        throw "WSL distribution is not uniquely registered: $candidate"
    }
    return [string]$matches[0]
}

function ConvertTo-KettlePerfWslEncodedCommand {
    param(
        [Parameter(Mandatory)]
        [string]$Script,
        [ValidatePattern('^$|^kettle-vtebench-[0-9a-f]{64}$')]
        [string]$Marker = ''
    )

    $encoded = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($Script)
    )
    if ($Marker) {
        return (
            "printf '%s' '$encoded' | base64 --decode | " +
            "exec setsid --fork --wait bash -c " +
            "'exec -a $Marker bash'"
        )
    }
    return "printf '%s' '$encoded' | base64 --decode | bash"
}

function Get-KettlePerfWslBashArguments {
    param(
        [Parameter(Mandatory)]
        [string]$Distribution,
        [Parameter(Mandatory)]
        [string]$EncodedCommand
    )

    $Distribution = Assert-KettlePerfWslDistributionName $Distribution
    if (
        $EncodedCommand.Length -gt 1048576 -or
        $EncodedCommand.Contains("`r") -or
        $EncodedCommand.Contains("`n") -or
        $EncodedCommand.Contains('"')
    ) {
        throw 'Encoded WSL Bash command is outside its argument bound'
    }
    return (
        '-d ' + $Distribution + ' -- bash -lc "' +
        $EncodedCommand + '"'
    )
}

function Invoke-KettlePerfWslBashCapture {
    param(
        [Parameter(Mandatory)]
        [string]$WslExe,
        [Parameter(Mandatory)]
        [string]$Distribution,
        [Parameter(Mandatory)]
        [string]$Script,
        [ValidatePattern('^$|^kettle-vtebench-[0-9a-f]{64}$')]
        [string]$Marker = '',
        [ValidateRange(1, 86400)]
        [int]$TimeoutSec,
        [ValidateRange(1, 1048576)]
        [int]$MaximumOutputBytes = 65536,
        [ValidateRange(0, 1048576)]
        [int]$MaximumErrorBytes = 4096
    )

    $encoded = ConvertTo-KettlePerfWslEncodedCommand `
        -Script $Script -Marker $Marker
    $arguments = Get-KettlePerfWslBashArguments `
        -Distribution $Distribution -EncodedCommand $encoded
    if (
        $MaximumErrorBytes -gt 0 -and
        ($TimeoutSec * 1000) -le 300000
    ) {
        $textResult = Invoke-KettlePerfBoundedProcess `
            -FilePath $WslExe `
            -ArgumentList @(
                '-d',
                $Distribution,
                '--',
                'bash',
                '-lc',
                $encoded
            ) `
            -TimeoutMs ([int]($TimeoutSec * 1000)) `
            -MaxStdoutBytes $MaximumOutputBytes `
            -MaxStderrBytes $MaximumErrorBytes
        if ($textResult.ExitCode -ne 0) {
            throw (
                'Pinned WSL Bash command failed with exit ' +
                $textResult.ExitCode
            )
        }
        return $textResult.StandardOutput
    }
    $result = Invoke-KettlePerfWslBoundedProcessBytes `
        -FilePath $WslExe -Arguments $arguments `
        -TimeoutMs ([int]($TimeoutSec * 1000)) `
        -MaximumStandardOutputBytes $MaximumOutputBytes `
        -MaximumStandardErrorBytes $MaximumErrorBytes
    try {
        if ($result.ExitCode -ne 0) {
            throw (
                'Pinned WSL Bash command failed with exit ' +
                $result.ExitCode
            )
        }
        return ConvertFrom-KettlePerfWslUtf8Bytes `
            -Bytes $result.StandardOutput
    } finally {
        [Array]::Clear(
            $result.StandardOutput,
            0,
            $result.StandardOutput.Length
        )
        [Array]::Clear(
            $result.StandardError,
            0,
            $result.StandardError.Length
        )
    }
}

function Get-KettlePerfWslBase64MarkerValue {
    param(
        [Parameter(Mandatory)]
        [string[]]$Lines,
        [Parameter(Mandatory)]
        [string]$Name
    )

    $prefix = "$Name="
    $matches = @($Lines | Where-Object { $_.StartsWith(
        $prefix,
        [StringComparison]::Ordinal
    ) })
    if ($matches.Count -ne 1) {
        throw "WSL evidence omitted unique $Name"
    }
    $encoded = $matches[0].Substring($prefix.Length)
    if ($encoded -cnotmatch '^[A-Za-z0-9+/]*={0,2}$') {
        throw "WSL evidence returned invalid base64 for $Name"
    }
    try {
        $bytes = [Convert]::FromBase64String($encoded)
        try {
            $value = [Text.UTF8Encoding]::new(
                $false,
                $true
            ).GetString($bytes)
        } finally {
            [Array]::Clear($bytes, 0, $bytes.Length)
        }
    } catch {
        throw "WSL evidence returned non-UTF-8 $Name"
    }
    if (
        -not $value -or
        $value.Length -gt 4096 -or
        $value.Contains([char]0) -or
        $value.Contains("`r") -or
        $value.Contains("`n")
    ) {
        throw "WSL evidence returned an invalid $Name value"
    }
    return $value
}

function Get-KettlePerfWslDistributionEvidence {
    param(
        [Parameter(Mandatory)]
        [string]$WslExe,
        [Parameter(Mandatory)]
        [string]$Distribution
    )

    $Distribution = Assert-KettlePerfWslDistributionName $Distribution
    $scriptText = @'
set -euo pipefail
os_release="$(realpath -e -- /etc/os-release)"
[[ -f "$os_release" ]]
emit() {
    printf '%s=' "$1"
    printf '%s' "$2" | base64 -w0
    printf '\n'
}
emit DISTRO_NAME "${WSL_DISTRO_NAME:?}"
emit OS_RELEASE_PATH "$os_release"
emit OS_RELEASE_SHA256 "$(sha256sum "$os_release" | awk '{print $1}')"
emit OS_PRETTY_LINE "$(grep -m1 '^PRETTY_NAME=' "$os_release")"
emit OS_VERSION_LINE "$(grep -m1 '^VERSION_ID=' "$os_release")"
emit KERNEL_RELEASE "$(uname -r)"
emit KERNEL_VERSION "$(uname -v)"
emit ARCHITECTURE "$(uname -m)"
emit USER_NAME "$(id -un)"
emit USER_ID "$(id -u)"
'@
    $output = Invoke-KettlePerfWslBashCapture `
        -WslExe $WslExe -Distribution $Distribution `
        -Script $scriptText -TimeoutSec 15 `
        -MaximumOutputBytes 32768
    $lines = [string[]]@(
        $output.Replace("`r`n", "`n").Split([char]10) |
            Where-Object { $_ }
    )
    $name = Get-KettlePerfWslBase64MarkerValue $lines 'DISTRO_NAME'
    if ($name -cne $Distribution) {
        throw 'WSL distribution self-identity differs from its launch name'
    }
    $userIdText = Get-KettlePerfWslBase64MarkerValue $lines 'USER_ID'
    [uint32]$userId = 0
    if (-not [uint32]::TryParse($userIdText, [ref]$userId)) {
        throw 'WSL distribution returned an invalid user id'
    }
    $osReleaseSha = Get-KettlePerfWslBase64MarkerValue `
        $lines 'OS_RELEASE_SHA256'
    if ($osReleaseSha -cnotmatch '^[0-9a-f]{64}$') {
        throw 'WSL distribution returned an invalid os-release hash'
    }
    return [pscustomobject]@{
        Schema = 'kettle-wsl-distribution-v1'
        Name = $name
        OsReleasePath = Get-KettlePerfWslBase64MarkerValue `
            $lines 'OS_RELEASE_PATH'
        OsReleaseSha256 = $osReleaseSha
        OsPrettyLine = Get-KettlePerfWslBase64MarkerValue `
            $lines 'OS_PRETTY_LINE'
        OsVersionLine = Get-KettlePerfWslBase64MarkerValue `
            $lines 'OS_VERSION_LINE'
        KernelRelease = Get-KettlePerfWslBase64MarkerValue `
            $lines 'KERNEL_RELEASE'
        KernelVersion = Get-KettlePerfWslBase64MarkerValue `
            $lines 'KERNEL_VERSION'
        Architecture = Get-KettlePerfWslBase64MarkerValue `
            $lines 'ARCHITECTURE'
        UserName = Get-KettlePerfWslBase64MarkerValue `
            $lines 'USER_NAME'
        UserId = $userId
    }
}

function Stop-KettlePerfWslMarkedProcess {
    param(
        [Parameter(Mandatory)]
        [string]$WslExe,
        [Parameter(Mandatory)]
        [string]$Distribution,
        [Parameter(Mandatory)]
        [ValidatePattern('^kettle-vtebench-[0-9a-f]{64}$')]
        [string]$Marker,
        [ValidateRange(1000, 60000)]
        [int]$TimeoutMs = 10000
    )

    $cleanupTemplate = @'
set -uo pipefail
marker=__MARKER__
pgids=()
for proc in /proc/[0-9]*; do
    [[ -r "$proc/cmdline" && -r "$proc/stat" ]] || continue
    argv0=
    IFS= read -r -d '' argv0 < "$proc/cmdline" || true
    [[ "$argv0" == "$marker" ]] || continue
    pgid="$(awk '{print $5}' "$proc/stat")"
    [[ "$pgid" =~ ^[0-9]+$ && "$pgid" -gt 1 ]] || continue
    duplicate=false
    for existing in "${pgids[@]:-}"; do
        [[ "$existing" == "$pgid" ]] && duplicate=true
    done
    $duplicate || pgids+=("$pgid")
done
(( ${#pgids[@]} > 0 )) || exit 0
signal_groups() {
    local signal="$1"
    local pgid
    for pgid in "${pgids[@]}"; do
        kill "-$signal" -- "-$pgid" 2>/dev/null || true
    done
}
groups_exist() {
    local pgid
    for pgid in "${pgids[@]}"; do
        kill -0 -- "-$pgid" 2>/dev/null && return 0
    done
    return 1
}
signal_groups TERM
for _ in {1..10}; do
    groups_exist || exit 0
    sleep 0.1
done
signal_groups KILL
for _ in {1..10}; do
    groups_exist || exit 0
    sleep 0.1
done
exit 71
'@
    $command = $cleanupTemplate.Replace(
        '__MARKER__',
        ("'" + $Marker + "'")
    )
    $encoded = ConvertTo-KettlePerfWslEncodedCommand -Script $command
    $arguments = Get-KettlePerfWslBashArguments `
        -Distribution $Distribution -EncodedCommand $encoded
    $result = Invoke-KettlePerfBoundedProcess `
        -FilePath $WslExe `
        -ArgumentList @(
            '-d',
            $Distribution,
            '--',
            'bash',
            '-lc',
            $encoded
        ) `
        -TimeoutMs $TimeoutMs `
        -MaxStdoutBytes 4096 -MaxStderrBytes 4096
    if ($result.ExitCode -ne 0) {
        throw (
            'Pinned WSL workload cleanup failed with exit ' +
            $result.ExitCode
        )
    }
}
