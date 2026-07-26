# Bounded external-process capture for performance-harness probes. Benchmark
# timing workloads use their dedicated transports; this helper is for version,
# GPU, and local-control evidence where an inherited pipe or noisy process must
# never hang or exhaust the harness.

Set-StrictMode -Version Latest

if (-not ('KettlePerf.BoundedProcess' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace KettlePerf {
public sealed class BoundedProcessResult {
    public int ExitCode { get; internal set; }
    public string StandardOutput { get; internal set; }
    public string StandardError { get; internal set; }
}

public static class BoundedProcess {
    private static readonly UTF8Encoding StrictUtf8 =
        new UTF8Encoding(false, true);
    private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;

    [StructLayout(LayoutKind.Sequential)]
    private struct JobObjectBasicLimitInformation {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize;
        public UIntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct IoCounters {
        public ulong ReadOperationCount;
        public ulong WriteOperationCount;
        public ulong OtherOperationCount;
        public ulong ReadTransferCount;
        public ulong WriteTransferCount;
        public ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JobObjectExtendedLimitInformation {
        public JobObjectBasicLimitInformation BasicLimitInformation;
        public IoCounters IoInfo;
        public UIntPtr ProcessMemoryLimit;
        public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed;
        public UIntPtr PeakJobMemoryUsed;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateJobObject(
        IntPtr securityAttributes,
        string name);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(
        IntPtr job,
        int informationClass,
        ref JobObjectExtendedLimitInformation information,
        uint informationLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(
        IntPtr job,
        IntPtr process);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    public static string JoinArguments(string[] arguments) {
        var commandLine = new StringBuilder();
        foreach (string value in arguments ?? new string[0]) {
            if (commandLine.Length > 0) commandLine.Append(' ');
            commandLine.Append(QuoteArgument(value ?? ""));
        }
        return commandLine.ToString();
    }

    private static string QuoteArgument(string argument) {
        bool needsQuotes = argument.Length == 0;
        foreach (char value in argument) {
            if (Char.IsWhiteSpace(value) || value == '"') {
                needsQuotes = true;
                break;
            }
        }
        if (!needsQuotes) return argument;

        var quoted = new StringBuilder(argument.Length + 2);
        quoted.Append('"');
        int backslashes = 0;
        foreach (char value in argument) {
            if (value == '\\') {
                backslashes++;
                continue;
            }
            if (value == '"') {
                quoted.Append('\\', checked(backslashes * 2 + 1));
                quoted.Append('"');
                backslashes = 0;
                continue;
            }
            quoted.Append('\\', backslashes);
            backslashes = 0;
            quoted.Append(value);
        }
        quoted.Append('\\', checked(backslashes * 2));
        quoted.Append('"');
        return quoted.ToString();
    }

    public static BoundedProcessResult Run(
        string fileName,
        string arguments,
        int timeoutMilliseconds,
        int maximumStandardOutputBytes,
        int maximumStandardErrorBytes) {
        if (String.IsNullOrWhiteSpace(fileName)) {
            throw new ArgumentException("An executable path is required", "fileName");
        }
        if (timeoutMilliseconds < 1) {
            throw new ArgumentOutOfRangeException("timeoutMilliseconds");
        }
        if (maximumStandardOutputBytes < 1) {
            throw new ArgumentOutOfRangeException("maximumStandardOutputBytes");
        }
        if (maximumStandardErrorBytes < 1) {
            throw new ArgumentOutOfRangeException("maximumStandardErrorBytes");
        }

        var startInfo = new ProcessStartInfo {
            FileName = fileName,
            Arguments = arguments ?? "",
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        IntPtr job = CreateKillOnCloseJob();
        try {
        using (var process = new Process { StartInfo = startInfo }) {
            try {
                if (!process.Start()) {
                    throw new InvalidOperationException(
                        "The bounded probe process did not start");
                }
                if (!AssignProcessToJobObject(job, process.Handle)) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Could not isolate the bounded probe in a job object");
                }
            } catch {
                TryTerminate(process);
                throw;
            }
            var stdoutTask = Task.Run(
                () => ReadBounded(
                    process.StandardOutput.BaseStream,
                    maximumStandardOutputBytes,
                    "standard output"));
            var stderrTask = Task.Run(
                () => ReadBounded(
                    process.StandardError.BaseStream,
                    maximumStandardErrorBytes,
                    "standard error"));
            var timer = Stopwatch.StartNew();
            try {
                while (true) {
                    if (stdoutTask.IsFaulted) {
                        throw Unwrap(stdoutTask.Exception);
                    }
                    if (stderrTask.IsFaulted) {
                        throw Unwrap(stderrTask.Exception);
                    }
                    if (
                        process.HasExited &&
                        stdoutTask.IsCompleted &&
                        stderrTask.IsCompleted
                    ) {
                        break;
                    }
                    if (timer.ElapsedMilliseconds >= timeoutMilliseconds) {
                        throw new TimeoutException(
                            "The bounded probe exceeded its timeout");
                    }
                    Thread.Sleep(10);
                }
                process.WaitForExit();
                return new BoundedProcessResult {
                    ExitCode = process.ExitCode,
                    StandardOutput = StrictUtf8.GetString(stdoutTask.Result),
                    StandardError = StrictUtf8.GetString(stderrTask.Result)
                };
            } catch {
                TryTerminate(process);
                TryClose(process.StandardOutput);
                TryClose(process.StandardError);
                throw;
            }
        }
        } finally {
            // The job contains the probe and every child it created after
            // assignment. Closing it guarantees no helper descendants survive
            // either successful capture or exceptional cleanup.
            CloseHandle(job);
        }
    }

    private static IntPtr CreateKillOnCloseJob() {
        IntPtr job = CreateJobObject(IntPtr.Zero, null);
        if (job == IntPtr.Zero) {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "Could not create a bounded-probe job object");
        }
        var information = new JobObjectExtendedLimitInformation();
        information.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(
            job,
            9,
            ref information,
            (uint)Marshal.SizeOf<JobObjectExtendedLimitInformation>())) {
            int error = Marshal.GetLastWin32Error();
            CloseHandle(job);
            throw new Win32Exception(
                error,
                "Could not configure the bounded-probe job object");
        }
        return job;
    }

    private static byte[] ReadBounded(
        Stream stream,
        int maximumBytes,
        string streamName) {
        var buffer = new byte[Math.Min(8192, maximumBytes)];
        using (var output = new MemoryStream(
            Math.Min(maximumBytes, 64 * 1024))) {
            while (true) {
                int read = stream.Read(buffer, 0, buffer.Length);
                if (read == 0) break;
                if (output.Length + read > maximumBytes) {
                    throw new InvalidDataException(
                        "The bounded probe " + streamName +
                        " exceeded " + maximumBytes + " bytes");
                }
                output.Write(buffer, 0, read);
            }
            return output.ToArray();
        }
    }

    private static Exception Unwrap(AggregateException error) {
        var flattened = error.Flatten();
        return flattened.InnerExceptions.Count == 1
            ? flattened.InnerExceptions[0]
            : flattened;
    }

    private static void TryTerminate(Process process) {
        try {
            if (!process.HasExited) process.Kill();
        } catch {
            // Cleanup is best effort; the original bounded-capture error wins.
        }
        try {
            process.WaitForExit(2000);
        } catch {
            // The caller still gets the original timeout or bound violation.
        }
    }

    private static void TryClose(StreamReader reader) {
        try {
            reader.Close();
        } catch {
            // Closing only unblocks a capture task during exceptional cleanup.
        }
    }
}
}
'@
}

function Invoke-KettlePerfBoundedProcess {
    param(
        [Parameter(Mandatory)]
        [string]$FilePath,
        [string[]]$ArgumentList = @(),
        [ValidateRange(1, 300000)]
        [int]$TimeoutMs = 10000,
        [ValidateRange(1, 16777216)]
        [int]$MaxStdoutBytes = 1048576,
        [ValidateRange(1, 16777216)]
        [int]$MaxStderrBytes = 1048576
    )

    if (-not [IO.Path]::IsPathRooted($FilePath)) {
        $FilePath = Get-Command $FilePath -CommandType Application `
            -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    }
    $resolved = (Resolve-Path -LiteralPath $FilePath -ErrorAction Stop).Path
    $item = Get-Item -LiteralPath $resolved -Force -ErrorAction Stop
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw "Bounded probe executable must be an ordinary file: $resolved"
    }
    foreach ($argument in $ArgumentList) {
        if ($null -eq $argument -or $argument.Contains([char]0)) {
            throw 'Bounded probe contains an invalid argument'
        }
    }
    $joined = [KettlePerf.BoundedProcess]::JoinArguments($ArgumentList)
    return [KettlePerf.BoundedProcess]::Run(
        $resolved,
        $joined,
        $TimeoutMs,
        $MaxStdoutBytes,
        $MaxStderrBytes
    )
}

function Assert-KettlePerfJsonShape {
    param(
        [Parameter(Mandatory)]
        [string]$Json,
        [ValidateRange(1, 256)]
        [int]$MaximumDepth = 32,
        [ValidateRange(1, 1000000)]
        [int]$MaximumTokens = 10000
    )

    $stack = [Collections.Generic.Stack[object]]::new()
    $inString = $false
    $escaped = $false
    $stringHadEscape = $false
    $stringValue = [Text.StringBuilder]::new()
    $tokens = 1
    for ($index = 0; $index -lt $Json.Length; $index++) {
        $value = $Json[$index]
        if ($inString) {
            if ($escaped) {
                [void]$stringValue.Append($value)
                $escaped = $false
                continue
            }
            if ($value -eq '\') {
                $escaped = $true
                $stringHadEscape = $true
                continue
            }
            if ($value -eq '"') {
                $inString = $false
                $next = $index + 1
                while (
                    $next -lt $Json.Length -and
                    [char]::IsWhiteSpace($Json[$next])
                ) {
                    $next++
                }
                if ($next -lt $Json.Length -and $Json[$next] -eq ':') {
                    if (
                        $stack.Count -eq 0 -or
                        $stack.Peek().Kind -ne '{'
                    ) {
                        throw 'Probe JSON property appears outside an object'
                    }
                    # Control/evidence property names are ASCII identifiers.
                    # Reject escapes so alternate spellings such as \u0061
                    # cannot evade the case-insensitive duplicate-key check.
                    if ($stringHadEscape) {
                        throw 'Probe JSON property names must not contain escapes'
                    }
                    if (-not $stack.Peek().Keys.Add($stringValue.ToString())) {
                        throw 'Probe JSON contains a duplicate property name'
                    }
                }
                continue
            }
            [void]$stringValue.Append($value)
            continue
        }
        if ($value -eq '"') {
            $inString = $true
            $escaped = $false
            $stringHadEscape = $false
            [void]$stringValue.Clear()
            $tokens++
            if ($tokens -gt $MaximumTokens) {
                throw 'Probe JSON exceeds the requested token bound'
            }
            continue
        }
        if ($value -eq '{' -or $value -eq '[') {
            $frame = [pscustomobject]@{
                Kind = [char]$value
                Keys = $null
            }
            if ($value -eq '{') {
                $frame.Keys = [Collections.Generic.HashSet[string]]::new(
                    [StringComparer]::OrdinalIgnoreCase
                )
            }
            $stack.Push($frame)
            $tokens++
            if ($stack.Count -gt $MaximumDepth) {
                throw 'Probe JSON exceeds the requested depth bound'
            }
        } elseif ($value -eq '}' -or $value -eq ']') {
            if ($stack.Count -eq 0) {
                throw 'Probe JSON has an unmatched closing delimiter'
            }
            $opening = $stack.Pop().Kind
            if (
                ($value -eq '}' -and $opening -ne '{') -or
                ($value -eq ']' -and $opening -ne '[')
            ) {
                throw 'Probe JSON has mismatched delimiters'
            }
        } elseif ($value -eq ',' -or $value -eq ':') {
            $tokens++
        }
        if ($tokens -gt $MaximumTokens) {
            throw 'Probe JSON exceeds the requested token bound'
        }
    }
    if ($inString -or $escaped -or $stack.Count -ne 0) {
        throw 'Probe JSON is structurally incomplete'
    }
}

function ConvertFrom-KettlePerfBoundedJson {
    param(
        [Parameter(Mandatory)]
        [string]$Json,
        [ValidateRange(1, 256)]
        [int]$MaximumDepth = 32,
        [ValidateRange(1, 1000000)]
        [int]$MaximumTokens = 10000
    )

    Assert-KettlePerfJsonShape -Json $Json `
        -MaximumDepth $MaximumDepth -MaximumTokens $MaximumTokens
    return ($Json | ConvertFrom-Json -ErrorAction Stop)
}
