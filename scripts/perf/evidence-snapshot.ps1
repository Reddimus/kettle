# Bounded, immutable reads of Windows performance-evidence files.
#
# A snapshot retains a no-delete handle to an ordinary result directory and a
# no-write/no-delete handle to every direct child it reads. Each leaf is read
# once, decoded as strict BOM-less UTF-8, and cached for the snapshot lifetime.

$script:KettlePerfEvidenceDefaultJsonBytes = 32MB
$script:KettlePerfEvidenceDefaultTextBytes = 64MB
$script:KettlePerfEvidenceDefaultJsonDepth = 64
$script:KettlePerfEvidenceDefaultJsonNodes = 1000000
$script:KettlePerfEvidenceDefaultMaximumFiles = 128
$script:KettlePerfEvidenceDefaultTotalBytes = 256MB
$script:KettlePerfEvidenceIsWindows = (
    [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT
)

function Initialize-KettlePerfEvidenceSnapshotTypes {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The initialized assembly exposes several related types.'
    )]
    param()

    if ('KettlePerfEvidence.EvidenceSnapshot' -as [type]) {
        return
    }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.Win32.SafeHandles;

namespace KettlePerfEvidence {
    internal static class NativeMethods {
        internal const uint FileListDirectory = 0x00000001;
        internal const uint FileReadData = 0x00000001;
        internal const uint FileTraverse = 0x00000020;
        internal const uint FileReadAttributes = 0x00000080;
        internal const uint Synchronize = 0x00100000;
        internal const uint FileShareRead = 0x00000001;
        internal const uint OpenExisting = 3;
        internal const uint FileAttributeDirectory = 0x00000010;
        internal const uint FileAttributeReparsePoint = 0x00000400;
        internal const uint FileFlagBackupSemantics = 0x02000000;
        internal const uint FileFlagOpenReparsePoint = 0x00200000;
        internal const uint FileShareWrite = 0x00000002;

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

        internal static SafeFileHandle OpenDirectory(string path) {
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
                    error, "Opening performance evidence root failed");
            }
            return handle;
        }

        internal static SafeFileHandle TryOpenRegularFile(
            string path,
            out int error) {
            var handle = CreateFile(
                path,
                FileReadData | FileReadAttributes | Synchronize,
                FileShareRead,
                IntPtr.Zero,
                OpenExisting,
                FileFlagOpenReparsePoint,
                IntPtr.Zero);
            if (handle.IsInvalid) {
                error = Marshal.GetLastWin32Error();
                handle.Dispose();
                return null;
            }
            error = 0;
            return handle;
        }

        internal static ByHandleFileInformation GetInformation(
            SafeFileHandle handle) {
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information)) {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "Reading performance evidence identity failed");
            }
            return information;
        }

        internal static string GetFinalPath(SafeFileHandle handle) {
            var capacity = 512;
            while (capacity <= 32768) {
                var result = new StringBuilder(capacity);
                var length = GetFinalPathNameByHandle(
                    handle, result, (uint)result.Capacity, 0);
                if (length == 0) {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Resolving performance evidence identity failed");
                }
                if (length < result.Capacity) {
                    return result.ToString();
                }
                capacity = checked((int)length + 1);
            }
            throw new InvalidDataException(
                "Performance evidence path exceeds the path bound");
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

        internal static bool SamePath(string left, string right) {
            return string.Equals(
                Path.GetFullPath(left).TrimEnd('\\', '/'),
                Path.GetFullPath(right).TrimEnd('\\', '/'),
                StringComparison.OrdinalIgnoreCase);
        }

        internal static string GetIdentity(
            ByHandleFileInformation information) {
            return information.VolumeSerialNumber.ToString("x8") + ":" +
                information.FileIndexHigh.ToString("x8") +
                information.FileIndexLow.ToString("x8");
        }

        internal static long GetLength(
            ByHandleFileInformation information) {
            return checked(
                ((long)information.FileSizeHigh << 32) |
                information.FileSizeLow);
        }
    }

    public sealed class EvidenceFile : IDisposable {
        private FileStream stream;
        private bool jsonScanned;
        private int jsonDepth;
        private int jsonNodes;

        internal EvidenceFile(
            string leafName,
            string fullPath,
            FileStream heldStream,
            byte[] data,
            string text,
            string sha256) {
            LeafName = leafName;
            FullPath = fullPath;
            stream = heldStream;
            Data = data;
            Text = text;
            Sha256 = sha256;
        }

        public string LeafName { get; private set; }
        public string FullPath { get; private set; }
        public byte[] Data { get; private set; }
        public long Length {
            get { return Data == null ? 0 : Data.LongLength; }
        }
        public string Text { get; private set; }
        public string Sha256 { get; private set; }
        public int JsonDepth {
            get { return jsonDepth; }
        }
        public int JsonNodes {
            get { return jsonNodes; }
        }

        public void ValidateJson(int maximumDepth, int maximumNodes) {
            AssertOpen();
            if (maximumDepth < 1 || maximumDepth > 256) {
                throw new ArgumentOutOfRangeException("maximumDepth");
            }
            if (maximumNodes < 1 || maximumNodes > 2000000) {
                throw new ArgumentOutOfRangeException("maximumNodes");
            }
            if (!jsonScanned) {
                var shape = JsonShapeScanner.Scan(
                    Text, maximumDepth, maximumNodes);
                jsonDepth = shape.MaximumDepth;
                jsonNodes = shape.Nodes;
                jsonScanned = true;
                return;
            }
            if (jsonDepth > maximumDepth) {
                throw new InvalidDataException(
                    "JSON exceeds the requested depth bound");
            }
            if (jsonNodes > maximumNodes) {
                throw new InvalidDataException(
                    "JSON exceeds the requested node bound");
            }
        }

        private void AssertOpen() {
            if (stream == null) {
                throw new ObjectDisposedException("EvidenceFile");
            }
        }

        public void Dispose() {
            if (stream != null) {
                stream.Dispose();
                stream = null;
            }
            if (Data != null) {
                Array.Clear(Data, 0, Data.Length);
                Data = null;
            }
            Text = null;
        }
    }

    public sealed class JsonShape {
        internal JsonShape(int maximumDepth, int nodes) {
            MaximumDepth = maximumDepth;
            Nodes = nodes;
        }

        public int MaximumDepth { get; private set; }
        public int Nodes { get; private set; }
    }

    public sealed class JsonShapeScanner {
        private readonly string text;
        private readonly int maximumDepth;
        private readonly int maximumNodes;
        private int index;
        private int nodes;
        private int observedDepth;

        private JsonShapeScanner(
            string value,
            int depthBound,
            int nodeBound) {
            text = value;
            maximumDepth = depthBound;
            maximumNodes = nodeBound;
        }

        public static JsonShape Scan(
            string value,
            int maximumDepth,
            int maximumNodes) {
            if (value == null) {
                throw new ArgumentNullException("value");
            }
            var scanner = new JsonShapeScanner(
                value, maximumDepth, maximumNodes);
            scanner.SkipWhitespace();
            scanner.ParseValue(1);
            scanner.SkipWhitespace();
            if (scanner.index != value.Length) {
                throw scanner.Error("JSON contains trailing data");
            }
            return new JsonShape(
                scanner.observedDepth, scanner.nodes);
        }

        private void ParseValue(int depth) {
            if (depth > maximumDepth) {
                throw Error("JSON exceeds the requested depth bound");
            }
            nodes++;
            if (nodes > maximumNodes) {
                throw Error("JSON exceeds the requested node bound");
            }
            if (depth > observedDepth) {
                observedDepth = depth;
            }
            if (index >= text.Length) {
                throw Error("JSON value is missing");
            }
            var value = text[index];
            if (value == '{') {
                ParseObject(depth);
            } else if (value == '[') {
                ParseArray(depth);
            } else if (value == '"') {
                ParseString();
            } else if (value == 't') {
                ParseLiteral("true");
            } else if (value == 'f') {
                ParseLiteral("false");
            } else if (value == 'n') {
                ParseLiteral("null");
            } else if (value == '-' || IsDigit(value)) {
                ParseNumber();
            } else {
                throw Error("JSON contains an invalid value");
            }
        }

        private void ParseObject(int depth) {
            index++;
            SkipWhitespace();
            if (Consume('}')) {
                return;
            }
            var names = new HashSet<string>(
                StringComparer.OrdinalIgnoreCase);
            while (true) {
                if (index >= text.Length || text[index] != '"') {
                    throw Error("JSON object property name is missing");
                }
                var name = ParseString();
                if (name.Length == 0) {
                    throw Error("JSON object property name is empty");
                }
                if (!names.Add(name)) {
                    throw Error(
                        "JSON contains a duplicate or case-ambiguous property");
                }
                SkipWhitespace();
                Require(':');
                SkipWhitespace();
                ParseValue(depth + 1);
                SkipWhitespace();
                if (Consume('}')) {
                    return;
                }
                Require(',');
                SkipWhitespace();
            }
        }

        private void ParseArray(int depth) {
            index++;
            SkipWhitespace();
            if (Consume(']')) {
                return;
            }
            while (true) {
                ParseValue(depth + 1);
                SkipWhitespace();
                if (Consume(']')) {
                    return;
                }
                Require(',');
                SkipWhitespace();
            }
        }

        private string ParseString() {
            Require('"');
            var result = new StringBuilder();
            while (index < text.Length) {
                var value = text[index++];
                if (value == '"') {
                    return result.ToString();
                }
                if (value == '\\') {
                    if (index >= text.Length) {
                        throw Error("JSON string escape is incomplete");
                    }
                    var escape = text[index++];
                    switch (escape) {
                    case '"':
                    case '\\':
                    case '/':
                        result.Append(escape);
                        break;
                    case 'b':
                        result.Append('\b');
                        break;
                    case 'f':
                        result.Append('\f');
                        break;
                    case 'n':
                        result.Append('\n');
                        break;
                    case 'r':
                        result.Append('\r');
                        break;
                    case 't':
                        result.Append('\t');
                        break;
                    case 'u':
                        AppendEscapedUnicode(result);
                        break;
                    default:
                        throw Error("JSON string contains an invalid escape");
                    }
                    continue;
                }
                if (value < 0x20) {
                    throw Error(
                        "JSON string contains an unescaped control character");
                }
                if (char.IsHighSurrogate(value)) {
                    if (index >= text.Length ||
                        !char.IsLowSurrogate(text[index])) {
                        throw Error(
                            "JSON string contains an unpaired surrogate");
                    }
                    result.Append(value);
                    result.Append(text[index++]);
                } else if (char.IsLowSurrogate(value)) {
                    throw Error("JSON string contains an unpaired surrogate");
                } else {
                    result.Append(value);
                }
            }
            throw Error("JSON string is unterminated");
        }

        private void AppendEscapedUnicode(StringBuilder result) {
            var first = ReadHexQuad();
            if (char.IsHighSurrogate(first)) {
                if (index + 1 >= text.Length ||
                    text[index] != '\\' ||
                    text[index + 1] != 'u') {
                    throw Error(
                        "JSON string contains an unpaired surrogate");
                }
                index += 2;
                var second = ReadHexQuad();
                if (!char.IsLowSurrogate(second)) {
                    throw Error(
                        "JSON string contains an unpaired surrogate");
                }
                result.Append(first);
                result.Append(second);
            } else if (char.IsLowSurrogate(first)) {
                throw Error("JSON string contains an unpaired surrogate");
            } else {
                result.Append(first);
            }
        }

        private char ReadHexQuad() {
            if (index + 4 > text.Length) {
                throw Error("JSON Unicode escape is incomplete");
            }
            var value = 0;
            for (var offset = 0; offset < 4; offset++) {
                var digit = HexValue(text[index++]);
                if (digit < 0) {
                    throw Error("JSON Unicode escape is invalid");
                }
                value = (value << 4) | digit;
            }
            return (char)value;
        }

        private void ParseNumber() {
            if (Consume('-') && index >= text.Length) {
                throw Error("JSON number is incomplete");
            }
            if (Consume('0')) {
                if (index < text.Length && IsDigit(text[index])) {
                    throw Error("JSON number contains a leading zero");
                }
            } else {
                if (index >= text.Length ||
                    text[index] < '1' || text[index] > '9') {
                    throw Error("JSON number integer part is invalid");
                }
                while (index < text.Length &&
                    IsDigit(text[index])) {
                    index++;
                }
            }
            if (Consume('.')) {
                if (index >= text.Length ||
                    !IsDigit(text[index])) {
                    throw Error("JSON number fraction is invalid");
                }
                while (index < text.Length &&
                    IsDigit(text[index])) {
                    index++;
                }
            }
            if (index < text.Length &&
                (text[index] == 'e' || text[index] == 'E')) {
                index++;
                if (index < text.Length &&
                    (text[index] == '+' || text[index] == '-')) {
                    index++;
                }
                if (index >= text.Length ||
                    !IsDigit(text[index])) {
                    throw Error("JSON number exponent is invalid");
                }
                while (index < text.Length &&
                    IsDigit(text[index])) {
                    index++;
                }
            }
        }

        private void ParseLiteral(string expected) {
            if (index + expected.Length > text.Length ||
                !string.Equals(
                    text.Substring(index, expected.Length),
                    expected,
                    StringComparison.Ordinal)) {
                throw Error("JSON literal is invalid");
            }
            index += expected.Length;
        }

        private void SkipWhitespace() {
            while (index < text.Length) {
                var value = text[index];
                if (value == ' ' || value == '\t' ||
                    value == '\r' || value == '\n') {
                    index++;
                } else {
                    return;
                }
            }
        }

        private bool Consume(char expected) {
            if (index < text.Length && text[index] == expected) {
                index++;
                return true;
            }
            return false;
        }

        private void Require(char expected) {
            if (!Consume(expected)) {
                throw Error(
                    "JSON is missing expected '" + expected + "'");
            }
        }

        private static bool IsDigit(char value) {
            return value >= '0' && value <= '9';
        }

        private static int HexValue(char value) {
            if (value >= '0' && value <= '9') {
                return value - '0';
            }
            if (value >= 'a' && value <= 'f') {
                return value - 'a' + 10;
            }
            if (value >= 'A' && value <= 'F') {
                return value - 'A' + 10;
            }
            return -1;
        }

        private InvalidDataException Error(string message) {
            return new InvalidDataException(
                message + " at character " +
                index.ToString(CultureInfo.InvariantCulture));
        }
    }

    internal sealed class PendingEvidenceFile : IDisposable {
        internal PendingEvidenceFile(
            string leafName,
            string fullPath,
            FileStream heldStream,
            NativeMethods.ByHandleFileInformation information,
            long length) {
            LeafName = leafName;
            FullPath = fullPath;
            Stream = heldStream;
            Information = information;
            Length = length;
        }

        internal string LeafName { get; private set; }
        internal string FullPath { get; private set; }
        internal FileStream Stream { get; private set; }
        internal NativeMethods.ByHandleFileInformation Information {
            get;
            private set;
        }
        internal long Length { get; private set; }

        internal FileStream TakeStream() {
            if (Stream == null) {
                throw new ObjectDisposedException("PendingEvidenceFile");
            }
            var result = Stream;
            Stream = null;
            return result;
        }

        public void Dispose() {
            if (Stream != null) {
                Stream.Dispose();
                Stream = null;
            }
        }
    }

    public sealed class EvidenceSnapshot : IDisposable {
        private static readonly Regex LeafPattern = new Regex(
            @"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$",
            RegexOptions.CultureInvariant);

        private SafeFileHandle rootHandle;
        private readonly string rootPath;
        private readonly string rootIdentity;
        private readonly int maximumFiles;
        private readonly long maximumTotalBytes;
        private long totalBytes;
        private readonly Dictionary<string, EvidenceFile> files;
        private readonly HashSet<string> missing;

        private EvidenceSnapshot(
            SafeFileHandle handle,
            string path,
            string identity,
            int fileBound,
            long totalBound) {
            rootHandle = handle;
            rootPath = path;
            rootIdentity = identity;
            maximumFiles = fileBound;
            maximumTotalBytes = totalBound;
            files = new Dictionary<string, EvidenceFile>(
                StringComparer.OrdinalIgnoreCase);
            missing = new HashSet<string>(
                StringComparer.OrdinalIgnoreCase);
        }

        public string RootPath {
            get { return rootPath; }
        }

        public int OpenFileCount {
            get { return files.Count; }
        }

        public long TotalBytes {
            get { return totalBytes; }
        }

        public static EvidenceSnapshot Open(
            string directory,
            int maximumFiles,
            long maximumTotalBytes) {
            if (string.IsNullOrWhiteSpace(directory)) {
                throw new ArgumentException(
                    "Evidence root is required", "directory");
            }
            if (maximumFiles < 1 || maximumFiles > 10000) {
                throw new ArgumentOutOfRangeException("maximumFiles");
            }
            if (maximumTotalBytes < 1 ||
                maximumTotalBytes > int.MaxValue) {
                throw new ArgumentOutOfRangeException(
                    "maximumTotalBytes");
            }
            var fullPath = Path.GetFullPath(directory);
            SafeFileHandle handle = null;
            try {
                handle = NativeMethods.OpenDirectory(fullPath);
                var information =
                    NativeMethods.GetInformation(handle);
                if ((information.FileAttributes &
                        NativeMethods.FileAttributeDirectory) == 0 ||
                    (information.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0) {
                    throw new InvalidDataException(
                        "Evidence root is not an ordinary directory");
                }
                var finalPath = NativeMethods.ConvertFinalPath(
                    NativeMethods.GetFinalPath(handle));
                if (!NativeMethods.SamePath(finalPath, fullPath)) {
                    throw new InvalidDataException(
                        "Evidence root aliases a different directory");
                }
                var result = new EvidenceSnapshot(
                    handle,
                    fullPath.TrimEnd('\\', '/'),
                    NativeMethods.GetIdentity(information),
                    maximumFiles,
                    maximumTotalBytes);
                handle = null;
                result.VerifyRoot();
                return result;
            } finally {
                if (handle != null) {
                    handle.Dispose();
                }
            }
        }

        public string[] EnumerateLeafNames(
            string extension,
            int maximumNames) {
            AssertOpen();
            if (string.IsNullOrEmpty(extension) ||
                extension[0] != '.' ||
                extension.IndexOfAny(
                    new char[] { '\\', '/', ':', '*', '?', '[', ']' }) >= 0) {
                throw new ArgumentException(
                    "Evidence extension must be one exact suffix",
                    "extension");
            }
            if (maximumNames < 1 || maximumNames > maximumFiles) {
                throw new ArgumentOutOfRangeException("maximumNames");
            }

            VerifyRoot();
            var names = new List<string>();
            var unique = new HashSet<string>(
                StringComparer.OrdinalIgnoreCase);
            foreach (var path in Directory.EnumerateFileSystemEntries(
                    rootPath, "*", SearchOption.TopDirectoryOnly)) {
                var leafName = Path.GetFileName(path);
                if (!string.Equals(
                        Path.GetExtension(leafName),
                        extension,
                        StringComparison.OrdinalIgnoreCase)) {
                    continue;
                }
                ValidateLeafName(leafName);
                if (!unique.Add(leafName)) {
                    throw new InvalidDataException(
                        "Evidence root has case-ambiguous leaf names");
                }
                names.Add(leafName);
                if (names.Count > maximumNames) {
                    throw new InvalidDataException(
                        "Evidence root exceeds the enumerated file-count bound");
                }
            }
            VerifyRoot();
            names.Sort(StringComparer.OrdinalIgnoreCase);
            return names.ToArray();
        }

        public EvidenceFile[] CaptureFiles(
            string[] leafNames,
            long maximumBytes) {
            AssertOpen();
            if (leafNames == null || leafNames.Length == 0) {
                throw new ArgumentException(
                    "At least one evidence leaf is required",
                    "leafNames");
            }
            if (maximumBytes < 1 || maximumBytes > int.MaxValue) {
                throw new ArgumentOutOfRangeException("maximumBytes");
            }
            if (leafNames.Length >
                maximumFiles - files.Count - missing.Count) {
                throw new InvalidDataException(
                    "Evidence snapshot exceeds the leaf-count bound");
            }

            var unique = new HashSet<string>(
                StringComparer.OrdinalIgnoreCase);
            foreach (var leafName in leafNames) {
                ValidateLeafName(leafName);
                if (!unique.Add(leafName)) {
                    throw new InvalidDataException(
                        "Bulk evidence capture has duplicate leaf names");
                }
                if (files.ContainsKey(leafName) ||
                    missing.Contains(leafName)) {
                    throw new InvalidOperationException(
                        "Bulk evidence capture requires unobserved leaves");
                }
            }

            var pending = new List<PendingEvidenceFile>();
            var completed = new List<EvidenceFile>();
            long pendingBytes = 0;
            try {
                VerifyRoot();
                foreach (var leafName in leafNames) {
                    var entry = OpenPendingFile(
                        leafName, maximumBytes, pendingBytes);
                    pending.Add(entry);
                    pendingBytes = checked(
                        pendingBytes + entry.Length);
                }
                VerifyRoot();
                foreach (var entry in pending) {
                    completed.Add(CompletePendingFile(entry));
                }
                VerifyRoot();
                foreach (var entry in completed) {
                    files.Add(entry.LeafName, entry);
                    totalBytes = checked(totalBytes + entry.Length);
                }
                var result = completed.ToArray();
                completed.Clear();
                return result;
            } finally {
                foreach (var entry in completed) {
                    entry.Dispose();
                }
                foreach (var entry in pending) {
                    entry.Dispose();
                }
            }
        }

        private PendingEvidenceFile OpenPendingFile(
            string leafName,
            long maximumBytes,
            long pendingBytes) {
            var fullPath = Path.Combine(rootPath, leafName);
            if (!NativeMethods.SamePath(
                    Path.GetDirectoryName(fullPath), rootPath)) {
                throw new InvalidDataException(
                    "Evidence leaf escapes the snapshot root");
            }
            int openError;
            var handle = NativeMethods.TryOpenRegularFile(
                fullPath, out openError);
            if (handle == null) {
                if (openError == 2 || openError == 3) {
                    throw new FileNotFoundException(
                        "Required performance evidence is missing",
                        fullPath);
                }
                throw new Win32Exception(
                    openError, "Opening performance evidence file failed");
            }

            FileStream stream = null;
            try {
                var information =
                    NativeMethods.GetInformation(handle);
                if ((information.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (information.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0) {
                    throw new InvalidDataException(
                        "Evidence leaf is not an ordinary file");
                }
                var finalPath = NativeMethods.ConvertFinalPath(
                    NativeMethods.GetFinalPath(handle));
                if (!NativeMethods.SamePath(finalPath, fullPath)) {
                    throw new InvalidDataException(
                        "Evidence leaf aliases a different file");
                }
                var length = NativeMethods.GetLength(information);
                var remaining = maximumTotalBytes -
                    totalBytes - pendingBytes;
                if (length < 1 || length > maximumBytes ||
                    remaining < 0 || length > remaining) {
                    throw new InvalidDataException(
                        "Evidence file size is outside the snapshot bound");
                }
                stream = new FileStream(
                    handle, FileAccess.Read, 65536, false);
                handle = null;
                var result = new PendingEvidenceFile(
                    leafName,
                    fullPath,
                    stream,
                    information,
                    length);
                stream = null;
                return result;
            } finally {
                if (stream != null) {
                    stream.Dispose();
                }
                if (handle != null) {
                    handle.Dispose();
                }
            }
        }

        private static EvidenceFile CompletePendingFile(
            PendingEvidenceFile pending) {
            var stream = pending.TakeStream();
            byte[] data = null;
            try {
                data = new byte[checked((int)pending.Length)];
                var offset = 0;
                while (offset < data.Length) {
                    var read = stream.Read(
                        data, offset, data.Length - offset);
                    if (read == 0) {
                        throw new EndOfStreamException(
                            "Evidence file ended during its held read");
                    }
                    offset += read;
                }
                var after = NativeMethods.GetInformation(
                    stream.SafeFileHandle);
                if (!string.Equals(
                        NativeMethods.GetIdentity(pending.Information),
                        NativeMethods.GetIdentity(after),
                        StringComparison.Ordinal) ||
                    pending.Length != NativeMethods.GetLength(after) ||
                    !NativeMethods.SamePath(
                        NativeMethods.ConvertFinalPath(
                            NativeMethods.GetFinalPath(
                                stream.SafeFileHandle)),
                        pending.FullPath)) {
                    throw new InvalidDataException(
                        "Evidence file identity changed during its held read");
                }
                if (data.Length >= 3 &&
                    data[0] == 0xef &&
                    data[1] == 0xbb &&
                    data[2] == 0xbf) {
                    throw new InvalidDataException(
                        "UTF-8 BOM is not accepted in performance evidence");
                }
                string text;
                try {
                    text = new UTF8Encoding(false, true).GetString(data);
                } catch (DecoderFallbackException error) {
                    throw new InvalidDataException(
                        "Performance evidence is not strict UTF-8", error);
                }
                string digestText;
                using (var sha = SHA256.Create()) {
                    var digest = sha.ComputeHash(data);
                    var digestBuilder =
                        new StringBuilder(digest.Length * 2);
                    foreach (var value in digest) {
                        digestBuilder.Append(value.ToString("x2"));
                    }
                    digestText = digestBuilder.ToString();
                }
                var result = new EvidenceFile(
                    pending.LeafName,
                    pending.FullPath,
                    stream,
                    data,
                    text,
                    digestText);
                stream = null;
                data = null;
                return result;
            } finally {
                if (data != null) {
                    Array.Clear(data, 0, data.Length);
                }
                if (stream != null) {
                    stream.Dispose();
                }
            }
        }

        public EvidenceFile ReadFile(
            string leafName,
            long maximumBytes,
            bool required) {
            AssertOpen();
            ValidateLeafName(leafName);
            if (maximumBytes < 1 ||
                maximumBytes > int.MaxValue) {
                throw new ArgumentOutOfRangeException("maximumBytes");
            }
            EvidenceFile cached;
            if (files.TryGetValue(leafName, out cached)) {
                if (cached.Length > maximumBytes) {
                    throw new InvalidDataException(
                        "Cached evidence exceeds the requested byte bound");
                }
                return cached;
            }
            if (missing.Contains(leafName)) {
                if (required) {
                    throw new FileNotFoundException(
                        "Required performance evidence is missing",
                        leafName);
                }
                return null;
            }
            if (files.Count + missing.Count >= maximumFiles) {
                throw new InvalidDataException(
                    "Evidence snapshot exceeds the leaf-count bound");
            }

            VerifyRoot();
            var fullPath = Path.Combine(rootPath, leafName);
            if (!NativeMethods.SamePath(
                    Path.GetDirectoryName(fullPath), rootPath)) {
                throw new InvalidDataException(
                    "Evidence leaf escapes the snapshot root");
            }
            int openError;
            var handle = NativeMethods.TryOpenRegularFile(
                fullPath, out openError);
            if (handle == null) {
                if (openError == 2 || openError == 3) {
                    missing.Add(leafName);
                    if (required) {
                        throw new FileNotFoundException(
                            "Required performance evidence is missing",
                            fullPath);
                    }
                    return null;
                }
                throw new Win32Exception(
                    openError, "Opening performance evidence file failed");
            }

            FileStream stream = null;
            try {
                var before = NativeMethods.GetInformation(handle);
                if ((before.FileAttributes &
                        NativeMethods.FileAttributeDirectory) != 0 ||
                    (before.FileAttributes &
                        NativeMethods.FileAttributeReparsePoint) != 0) {
                    throw new InvalidDataException(
                        "Evidence leaf is not an ordinary file");
                }
                var finalPath = NativeMethods.ConvertFinalPath(
                    NativeMethods.GetFinalPath(handle));
                if (!NativeMethods.SamePath(finalPath, fullPath)) {
                    throw new InvalidDataException(
                        "Evidence leaf aliases a different file");
                }
                var length = NativeMethods.GetLength(before);
                if (length < 1 || length > maximumBytes ||
                    totalBytes + length > maximumTotalBytes) {
                    throw new InvalidDataException(
                        "Evidence file size is outside the snapshot bound");
                }
                stream = new FileStream(
                    handle, FileAccess.Read, 65536, false);
                handle = null;
                var data = new byte[checked((int)length)];
                var offset = 0;
                while (offset < data.Length) {
                    var read = stream.Read(
                        data, offset, data.Length - offset);
                    if (read == 0) {
                        throw new EndOfStreamException(
                            "Evidence file ended during its held read");
                    }
                    offset += read;
                }
                var after = NativeMethods.GetInformation(
                    stream.SafeFileHandle);
                if (!string.Equals(
                        NativeMethods.GetIdentity(before),
                        NativeMethods.GetIdentity(after),
                        StringComparison.Ordinal) ||
                    NativeMethods.GetLength(before) !=
                        NativeMethods.GetLength(after)) {
                    throw new InvalidDataException(
                        "Evidence file identity changed during its held read");
                }
                if (data.Length >= 3 &&
                    data[0] == 0xef &&
                    data[1] == 0xbb &&
                    data[2] == 0xbf) {
                    throw new InvalidDataException(
                        "UTF-8 BOM is not accepted in performance evidence");
                }
                string text;
                try {
                    text = new UTF8Encoding(false, true).GetString(data);
                } catch (DecoderFallbackException error) {
                    throw new InvalidDataException(
                        "Performance evidence is not strict UTF-8", error);
                }
                string digestText;
                using (var sha = SHA256.Create()) {
                    var digest = sha.ComputeHash(data);
                    var digestBuilder =
                        new StringBuilder(digest.Length * 2);
                    foreach (var value in digest) {
                        digestBuilder.Append(value.ToString("x2"));
                    }
                    digestText = digestBuilder.ToString();
                }
                var entry = new EvidenceFile(
                    leafName,
                    fullPath,
                    stream,
                    data,
                    text,
                    digestText);
                stream = null;
                files.Add(leafName, entry);
                totalBytes += length;
                VerifyRoot();
                return entry;
            } finally {
                if (stream != null) {
                    stream.Dispose();
                }
                if (handle != null) {
                    handle.Dispose();
                }
            }
        }

        private static void ValidateLeafName(string leafName) {
            if (string.IsNullOrEmpty(leafName) ||
                !LeafPattern.IsMatch(leafName) ||
                !string.Equals(
                    Path.GetFileName(leafName),
                    leafName,
                    StringComparison.Ordinal) ||
                leafName == "." || leafName == "..") {
                throw new ArgumentException(
                    "Evidence leaf must be an exact safe file name",
                    "leafName");
            }
        }

        private void VerifyRoot() {
            AssertOpen();
            var information =
                NativeMethods.GetInformation(rootHandle);
            if ((information.FileAttributes &
                    NativeMethods.FileAttributeDirectory) == 0 ||
                (information.FileAttributes &
                    NativeMethods.FileAttributeReparsePoint) != 0 ||
                !string.Equals(
                    NativeMethods.GetIdentity(information),
                    rootIdentity,
                    StringComparison.Ordinal)) {
                throw new InvalidDataException(
                    "Evidence root identity changed");
            }
            var finalPath = NativeMethods.ConvertFinalPath(
                NativeMethods.GetFinalPath(rootHandle));
            if (!NativeMethods.SamePath(finalPath, rootPath)) {
                throw new InvalidDataException(
                    "Evidence root moved during the snapshot");
            }
        }

        private void AssertOpen() {
            if (rootHandle == null || rootHandle.IsClosed) {
                throw new ObjectDisposedException("EvidenceSnapshot");
            }
        }

        public void Dispose() {
            foreach (var entry in files.Values) {
                entry.Dispose();
            }
            files.Clear();
            missing.Clear();
            if (rootHandle != null) {
                rootHandle.Dispose();
                rootHandle = null;
            }
        }
    }
}
'@
}

function Open-KettlePerfEvidenceSnapshot {
    param(
        [Parameter(Mandatory)]
        [string]$Directory,
        [ValidateRange(1, 10000)]
        [int]$MaximumFiles = $script:KettlePerfEvidenceDefaultMaximumFiles,
        [ValidateRange(1, 2147483647)]
        [long]$MaximumTotalBytes = (
            $script:KettlePerfEvidenceDefaultTotalBytes
        )
    )

    if (-not $script:KettlePerfEvidenceIsWindows) {
        throw [PlatformNotSupportedException]::new(
            'Kettle performance evidence snapshots require Windows'
        )
    }
    Initialize-KettlePerfEvidenceSnapshotTypes
    $native = [KettlePerfEvidence.EvidenceSnapshot]::Open(
        $Directory,
        $MaximumFiles,
        $MaximumTotalBytes
    )
    return [pscustomobject]@{
        schema = 'kettle-evidence-snapshot-v1'
        root_path = $native.RootPath
        native = $native
        entries = [Collections.Generic.Dictionary[string, object]]::new(
            [StringComparer]::OrdinalIgnoreCase
        )
        closed = $false
    }
}

function Assert-KettlePerfEvidenceSnapshotOpen {
    param(
        [Parameter(Mandatory)]
        $Snapshot
    )

    if (
        $null -eq $Snapshot -or
        $Snapshot.schema -cne 'kettle-evidence-snapshot-v1' -or
        $Snapshot.closed -ne $false -or
        $null -eq $Snapshot.native -or
        $null -eq $Snapshot.entries
    ) {
        throw 'Performance evidence snapshot is missing, invalid, or closed'
    }
}

function Get-KettlePerfEvidenceJsonShape {
    param(
        [Parameter(Mandatory)]
        [string]$Text,
        [ValidateRange(1, 256)]
        [int]$MaximumDepth = $script:KettlePerfEvidenceDefaultJsonDepth,
        [ValidateRange(1, 2000000)]
        [int]$MaximumNodes = $script:KettlePerfEvidenceDefaultJsonNodes
    )

    Initialize-KettlePerfEvidenceSnapshotTypes
    $shape = [KettlePerfEvidence.JsonShapeScanner]::Scan(
        $Text,
        $MaximumDepth,
        $MaximumNodes
    )
    return [pscustomobject]@{
        maximum_depth = [int]$shape.MaximumDepth
        nodes = [int]$shape.Nodes
    }
}

function Get-KettlePerfEvidenceLeafNames {
    [OutputType([string[]])]
    param(
        [Parameter(Mandatory)]
        $Snapshot,
        [Parameter(Mandatory)]
        [ValidatePattern('^\.[A-Za-z0-9]{1,16}$')]
        [string]$Extension,
        [ValidateRange(1, 10000)]
        [int]$MaximumNames = $script:KettlePerfEvidenceDefaultMaximumFiles
    )

    Assert-KettlePerfEvidenceSnapshotOpen -Snapshot $Snapshot
    return [string[]]$Snapshot.native.EnumerateLeafNames(
        $Extension,
        $MaximumNames
    )
}

function Read-KettlePerfEvidenceJsonSet {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function atomically captures a complete file set.'
    )]
    [OutputType([object[]])]
    param(
        [Parameter(Mandatory)]
        $Snapshot,
        [Parameter(Mandatory)]
        [ValidateNotNullOrEmpty()]
        [string[]]$LeafNames,
        [ValidateRange(1, 2147483647)]
        [long]$MaximumBytes = $script:KettlePerfEvidenceDefaultJsonBytes,
        [ValidateRange(1, 256)]
        [int]$MaximumDepth = $script:KettlePerfEvidenceDefaultJsonDepth,
        [ValidateRange(1, 2000000)]
        [int]$MaximumTotalNodes = (
            $script:KettlePerfEvidenceDefaultJsonNodes
        )
    )

    Assert-KettlePerfEvidenceSnapshotOpen -Snapshot $Snapshot
    $nativeEntries = [object[]]$Snapshot.native.CaptureFiles(
        $LeafNames,
        $MaximumBytes
    )
    $entries = [Collections.Generic.List[object]]::new()
    foreach ($nativeEntry in $nativeEntries) {
        $entry = [pscustomobject]@{
            leaf_name = $nativeEntry.LeafName
            path = $nativeEntry.FullPath
            bytes = [long]$nativeEntry.Length
            sha256 = $nativeEntry.Sha256
            text = $nativeEntry.Text
            value = $null
            json_depth = $null
            json_nodes = $null
            json_parsed = $false
            native = $nativeEntry
        }
        $Snapshot.entries.Add($entry.leaf_name, $entry)
        $entries.Add($entry)
    }

    $totalNodes = 0
    foreach ($entry in $entries) {
        $entry.native.ValidateJson($MaximumDepth, $MaximumTotalNodes)
        $entry.json_depth = [int]$entry.native.JsonDepth
        $entry.json_nodes = [int]$entry.native.JsonNodes
        if (
            $entry.json_nodes -gt
                ($MaximumTotalNodes - $totalNodes)
        ) {
            throw 'Performance evidence JSON exceeds the total node bound'
        }
        $totalNodes += $entry.json_nodes
    }
    foreach ($entry in $entries) {
        try {
            $entry.value = $entry.text |
                ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw (
                'Performance evidence JSON could not be parsed: ' +
                $entry.leaf_name
            )
        }
        $entry.json_parsed = $true
    }
    return [object[]]$entries.ToArray()
}

function Read-KettlePerfEvidenceText {
    param(
        [Parameter(Mandatory)]
        $Snapshot,
        [Parameter(Mandatory)]
        [string]$LeafName,
        [ValidateRange(1, 2147483647)]
        [long]$MaximumBytes = $script:KettlePerfEvidenceDefaultTextBytes,
        [switch]$Required
    )

    Assert-KettlePerfEvidenceSnapshotOpen -Snapshot $Snapshot
    $nativeEntry = $Snapshot.native.ReadFile(
        $LeafName,
        $MaximumBytes,
        [bool]$Required
    )
    if ($null -eq $nativeEntry) {
        return $null
    }
    if ($Snapshot.entries.ContainsKey($LeafName)) {
        return $Snapshot.entries[$LeafName]
    }
    $entry = [pscustomobject]@{
        leaf_name = $nativeEntry.LeafName
        path = $nativeEntry.FullPath
        bytes = [long]$nativeEntry.Length
        sha256 = $nativeEntry.Sha256
        text = $nativeEntry.Text
        value = $null
        json_depth = $null
        json_nodes = $null
        json_parsed = $false
        native = $nativeEntry
    }
    $Snapshot.entries.Add($LeafName, $entry)
    return $entry
}

function Read-KettlePerfEvidenceJson {
    param(
        [Parameter(Mandatory)]
        $Snapshot,
        [Parameter(Mandatory)]
        [string]$LeafName,
        [ValidateRange(1, 2147483647)]
        [long]$MaximumBytes = $script:KettlePerfEvidenceDefaultJsonBytes,
        [ValidateRange(1, 256)]
        [int]$MaximumDepth = $script:KettlePerfEvidenceDefaultJsonDepth,
        [ValidateRange(1, 2000000)]
        [int]$MaximumNodes = $script:KettlePerfEvidenceDefaultJsonNodes,
        [switch]$Required
    )

    $entry = Read-KettlePerfEvidenceText `
        -Snapshot $Snapshot -LeafName $LeafName `
        -MaximumBytes $MaximumBytes -Required:$Required
    if ($null -eq $entry) {
        return $null
    }
    $entry.native.ValidateJson($MaximumDepth, $MaximumNodes)
    if ($entry.json_parsed -ne $true) {
        try {
            $entry.value = $entry.text |
                ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw "Performance evidence JSON could not be parsed: $LeafName"
        }
        $entry.json_depth = [int]$entry.native.JsonDepth
        $entry.json_nodes = [int]$entry.native.JsonNodes
        $entry.json_parsed = $true
    }
    return $entry
}

function Close-KettlePerfEvidenceSnapshot {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This only closes held read handles and clears caches.'
    )]
    param(
        $Snapshot
    )

    if (
        $null -eq $Snapshot -or
        $Snapshot.schema -cne 'kettle-evidence-snapshot-v1' -or
        $Snapshot.closed -eq $true
    ) {
        return
    }
    try {
        $Snapshot.native.Dispose()
    } finally {
        $Snapshot.entries.Clear()
        $Snapshot.closed = $true
    }
}
