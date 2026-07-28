# Shared Win32 helpers for the perf harness (dot-source this).
# PrintWindow with PW_CLIENTONLY | PW_RENDERFULLCONTENT is used for captures
# because GDI screen copies cannot see flip-model (DXGI) swapchains like
# kettle's; PrintWindow renders at PHYSICAL pixels regardless of the caller's
# DPI virtualization.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\statistics.ps1"
. "$PSScriptRoot\display-identity-contract.ps1"

if (-not ('KettlePerf.Native' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace KettlePerf {
public static class Native {
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr dpiContext);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr hWnd, EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder path, ref uint size);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr hWnd, IntPtr hdc);
    [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromPoint(POINT point, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr hWnd, uint flags);
    [DllImport("shcore.dll")] public static extern int GetDpiForMonitor(IntPtr monitor, uint dpiType, out uint dpiX, out uint dpiY);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern bool EnumDisplaySettings(string deviceName, int modeNum, ref DEVMODE mode);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern bool GetMonitorInfo(IntPtr monitor, ref MONITORINFOEX info);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern bool EnumDisplayDevices(string device, uint index, ref DISPLAY_DEVICE displayDevice, uint flags);
    [DllImport("user32.dll")] private static extern int GetDisplayConfigBufferSizes(uint flags, out uint pathCount, out uint modeCount);
    [DllImport("user32.dll")] private static extern int QueryDisplayConfig(
        uint flags,
        ref uint pathCount,
        [Out] DISPLAYCONFIG_PATH_INFO[] paths,
        ref uint modeCount,
        [Out] DISPLAYCONFIG_MODE_INFO[] modes,
        IntPtr topologyId);
    [DllImport("user32.dll", EntryPoint = "DisplayConfigGetDeviceInfo", CharSet = CharSet.Unicode)]
    private static extern int DisplayConfigGetSourceName(ref DISPLAYCONFIG_SOURCE_DEVICE_NAME packet);
    [DllImport("user32.dll", EntryPoint = "DisplayConfigGetDeviceInfo", CharSet = CharSet.Unicode)]
    private static extern int DisplayConfigGetTargetName(ref DISPLAYCONFIG_TARGET_DEVICE_NAME packet);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr hdc);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr hdc, int w, int h);
    [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);
    [DllImport("gdi32.dll")] public static extern bool SetViewportOrgEx(IntPtr hdc, int x, int y, out POINT oldPoint);
    [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr obj);
    [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr hdc);
    [DllImport("gdi32.dll")] public static extern int GetDIBits(IntPtr hdc, IntPtr bmp, uint start, uint lines, byte[] bits, ref BITMAPINFO bi, uint usage);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DEVMODE {
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
        public ushort dmSpecVersion, dmDriverVersion, dmSize, dmDriverExtra;
        public uint dmFields;
        public int dmPositionX, dmPositionY;
        public uint dmDisplayOrientation, dmDisplayFixedOutput;
        public short dmColor, dmDuplex, dmYResolution, dmTTOption, dmCollate;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName;
        public ushort dmLogPixels;
        public uint dmBitsPerPel, dmPelsWidth, dmPelsHeight, dmDisplayFlags, dmDisplayFrequency;
        public uint dmICMMethod, dmICMIntent, dmMediaType, dmDitherType;
        public uint dmReserved1, dmReserved2, dmPanningWidth, dmPanningHeight;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct MONITORINFOEX {
        public uint cbSize;
        public RECT monitor, work;
        public uint flags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string device;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DISPLAY_DEVICE {
        public uint cb;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceString;
        public uint StateFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceID;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceKey;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct LUID {
        public uint LowPart;
        public int HighPart;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct DISPLAYCONFIG_RATIONAL {
        public uint Numerator;
        public uint Denominator;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct DISPLAYCONFIG_PATH_SOURCE_INFO {
        public LUID AdapterId;
        public uint Id;
        public uint ModeInfoIdx;
        public uint StatusFlags;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct DISPLAYCONFIG_PATH_TARGET_INFO {
        public LUID AdapterId;
        public uint Id;
        public uint ModeInfoIdx;
        public int OutputTechnology;
        public uint Rotation;
        public uint Scaling;
        public DISPLAYCONFIG_RATIONAL RefreshRate;
        public uint ScanLineOrdering;
        [MarshalAs(UnmanagedType.Bool)] public bool TargetAvailable;
        public uint StatusFlags;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct DISPLAYCONFIG_PATH_INFO {
        public DISPLAYCONFIG_PATH_SOURCE_INFO SourceInfo;
        public DISPLAYCONFIG_PATH_TARGET_INFO TargetInfo;
        public uint Flags;
    }
    [StructLayout(LayoutKind.Explicit, Size = 48)]
    public struct DISPLAYCONFIG_MODE_INFO_UNION {
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct DISPLAYCONFIG_MODE_INFO {
        public uint InfoType;
        public uint Id;
        public LUID AdapterId;
        public DISPLAYCONFIG_MODE_INFO_UNION Info;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct DISPLAYCONFIG_DEVICE_INFO_HEADER {
        public uint Type;
        public uint Size;
        public LUID AdapterId;
        public uint Id;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        public DISPLAYCONFIG_DEVICE_INFO_HEADER Header;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
        public string ViewGdiDeviceName;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DISPLAYCONFIG_TARGET_DEVICE_NAME {
        public DISPLAYCONFIG_DEVICE_INFO_HEADER Header;
        public uint Flags;
        public int OutputTechnology;
        public ushort EdidManufactureId;
        public ushort EdidProductCodeId;
        public uint ConnectorInstance;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)]
        public string MonitorFriendlyDeviceName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
        public string MonitorDevicePath;
    }
    public sealed class DisplayPathIdentity {
        public string SourceDeviceName;
        public string MonitorDevicePath;
        public string FriendlyName;
        public string AdapterLuid;
        public uint SourceId;
        public uint TargetId;
        public uint ConnectorInstance;
        public int OutputTechnology;
        public ushort EdidManufactureId;
        public ushort EdidProductCodeId;
        public bool FriendlyNameFromEdid;
        public bool FriendlyNameForced;
        public bool EdidIdsValid;
        public bool TargetAvailable;
        public bool SourceInUse;
        public bool TargetInUse;
    }
    [StructLayout(LayoutKind.Sequential)] public struct BITMAPINFOHEADER {
        public uint biSize; public int biWidth, biHeight; public ushort biPlanes, biBitCount;
        public uint biCompression, biSizeImage; public int biXPelsPerMeter, biYPelsPerMeter;
        public uint biClrUsed, biClrImportant;
    }
    [StructLayout(LayoutKind.Sequential)] public struct BITMAPINFO { public BITMAPINFOHEADER bmiHeader; public uint colors; }
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
    [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public MOUSEINPUT mi; }
    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }

    public const uint SWP_NOZORDER = 0x0004, SWP_NOACTIVATE = 0x0010;
    public const uint PW_CLIENTONLY = 0x1, PW_RENDERFULLCONTENT = 0x2;
    public const uint KEYEVENTF_KEYUP = 0x2, KEYEVENTF_UNICODE = 0x4;
    public const uint MOUSEEVENTF_RIGHTDOWN = 0x0008, MOUSEEVENTF_RIGHTUP = 0x0010;
    public const uint WM_CLOSE = 0x0010, BM_CLICK = 0x00F5;
    public const int ENUM_CURRENT_SETTINGS = -1;
    private const uint EDD_GET_DEVICE_INTERFACE_NAME = 0x00000001;
    private const uint QDC_ONLY_ACTIVE_PATHS = 0x00000002;
    private const uint QDC_VIRTUAL_MODE_AWARE = 0x00000010;
    private const int ERROR_SUCCESS = 0;
    private const int ERROR_INSUFFICIENT_BUFFER = 122;
    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 is the documented pseudo
    // handle -4. Keeping the harness thread in physical pixels makes Kettle's
    // ui_geometry, pointer input, window sizing, and PrintWindow buffers share
    // one coordinate system on scaled/high-resolution displays.
    public static readonly IntPtr PerMonitorAwareV2 = new IntPtr(-4);

    public static string JoinArguments(IEnumerable<string> arguments) {
        var commandLine = new StringBuilder();
        foreach (string argument in arguments) {
            if (commandLine.Length > 0) commandLine.Append(' ');
            commandLine.Append(QuoteArgument(argument ?? ""));
        }
        return commandLine.ToString();
    }

    // Encode one argument according to the CommandLineToArgvW/MSVC rules.
    // Start-Process joins ArgumentList elements with spaces and does not quote
    // them itself, so passing a pre-encoded scalar is the only path that also
    // works in Windows PowerShell 5.1.
    public static string QuoteArgument(string argument) {
        bool needsQuotes = argument.Length == 0;
        foreach (char c in argument) {
            if (char.IsWhiteSpace(c) || c == '"') {
                needsQuotes = true;
                break;
            }
        }
        if (!needsQuotes) return argument;

        var quoted = new StringBuilder(argument.Length + 2);
        quoted.Append('"');
        int backslashes = 0;
        foreach (char c in argument) {
            if (c == '\\') {
                backslashes++;
                continue;
            }
            if (c == '"') {
                quoted.Append('\\', backslashes * 2 + 1);
                quoted.Append('"');
                backslashes = 0;
                continue;
            }
            quoted.Append('\\', backslashes);
            backslashes = 0;
            quoted.Append(c);
        }
        quoted.Append('\\', backslashes * 2);
        quoted.Append('"');
        return quoted.ToString();
    }

    public static List<IntPtr> VisibleTopWindows() {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            if (IsWindowVisible(h) && GetWindowTextLength(h) > 0) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    public static List<IntPtr> EnabledChildWindowsByClassAndTitle(
        IntPtr parent,
        string className,
        string title
    ) {
        var list = new List<IntPtr>();
        EnumChildWindows(parent, (h, l) => {
            if (!IsWindowEnabled(h)) return true;
            var cls = new StringBuilder(256);
            var text = new StringBuilder(512);
            GetClassName(h, cls, cls.Capacity);
            GetWindowText(h, text, text.Capacity);
            if (
                StringComparer.OrdinalIgnoreCase.Equals(cls.ToString(), className)
                && StringComparer.Ordinal.Equals(text.ToString(), title)
            ) {
                list.Add(h);
            }
            return true;
        }, IntPtr.Zero);
        return list;
    }

    public static bool ClickButton(IntPtr button) {
        return SendMessage(button, BM_CLICK, IntPtr.Zero, IntPtr.Zero) == IntPtr.Zero;
    }

    public static uint[] EffectiveDpiAt(int x, int y) {
        IntPtr monitor = MonitorFromPoint(new POINT { X = x, Y = y }, 2);
        uint dpiX, dpiY;
        if (monitor == IntPtr.Zero || GetDpiForMonitor(monitor, 0, out dpiX, out dpiY) != 0) {
            return null;
        }
        return new uint[] { dpiX, dpiY };
    }

    public static int CurrentRefreshRate(string deviceName) {
        var mode = new DEVMODE();
        mode.dmSize = (ushort)Marshal.SizeOf<DEVMODE>();
        return EnumDisplaySettings(deviceName, ENUM_CURRENT_SETTINGS, ref mode)
            ? (int)mode.dmDisplayFrequency
            : 0;
    }

    public static string ProcessExecutablePath(int processId) {
        if (processId <= 0) return null;
        const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
        IntPtr process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            (uint)processId);
        if (process == IntPtr.Zero) return null;
        try {
            uint length = 32768;
            var path = new StringBuilder((int)length);
            return QueryFullProcessImageName(process, 0, path, ref length)
                ? path.ToString()
                : null;
        } finally {
            CloseHandle(process);
        }
    }

    public static string MonitorDeviceForWindow(IntPtr hWnd) {
        IntPtr monitor = MonitorFromWindow(hWnd, 2);
        var info = new MONITORINFOEX();
        info.cbSize = (uint)Marshal.SizeOf<MONITORINFOEX>();
        return monitor != IntPtr.Zero && GetMonitorInfo(monitor, ref info)
            ? info.device
            : null;
    }

    public static string MonitorDeviceIdForDisplay(string deviceName) {
        var device = new DISPLAY_DEVICE();
        device.cb = (uint)Marshal.SizeOf<DISPLAY_DEVICE>();
        if (
            EnumDisplayDevices(
                deviceName,
                0,
                ref device,
                0)
            && !String.IsNullOrWhiteSpace(device.DeviceID)
        ) {
            return device.DeviceID;
        }
        device = new DISPLAY_DEVICE();
        device.cb = (uint)Marshal.SizeOf<DISPLAY_DEVICE>();
        return EnumDisplayDevices(
            deviceName,
            0,
            ref device,
            EDD_GET_DEVICE_INTERFACE_NAME)
            ? device.DeviceID
            : null;
    }

    private static string FormatLuid(LUID value) {
        return String.Format(
            System.Globalization.CultureInfo.InvariantCulture,
            "{0:x8}:{1:x8}",
            unchecked((uint)value.HighPart),
            value.LowPart);
    }

    // Enumerate only paths active in the caller's interactive console session.
    // The source GDI name gives an exact Screen.DeviceName mapping; the target
    // packet proves whether its manufacturer/product IDs came from EDID.
    public static DisplayPathIdentity[] ActiveDisplayPaths() {
        const uint flags = QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE;
        for (int attempt = 0; attempt < 8; attempt++) {
            uint pathCount;
            uint modeCount;
            int result = GetDisplayConfigBufferSizes(
                flags,
                out pathCount,
                out modeCount);
            if (result != ERROR_SUCCESS) {
                throw new InvalidOperationException(
                    "GetDisplayConfigBufferSizes failed with Win32 status " + result);
            }
            if (pathCount > 256 || modeCount > 1024) {
                throw new InvalidOperationException(
                    "DisplayConfig returned an unreasonable topology size");
            }
            var paths = new DISPLAYCONFIG_PATH_INFO[pathCount];
            var modes = new DISPLAYCONFIG_MODE_INFO[modeCount];
            result = QueryDisplayConfig(
                flags,
                ref pathCount,
                paths,
                ref modeCount,
                modes,
                IntPtr.Zero);
            if (result == ERROR_INSUFFICIENT_BUFFER) {
                continue;
            }
            if (result != ERROR_SUCCESS) {
                throw new InvalidOperationException(
                    "QueryDisplayConfig failed with Win32 status " + result);
            }

            var identities = new List<DisplayPathIdentity>((int)pathCount);
            for (int index = 0; index < pathCount; index++) {
                DISPLAYCONFIG_PATH_INFO path = paths[index];
                var source = new DISPLAYCONFIG_SOURCE_DEVICE_NAME();
                source.Header.Type = 1;
                source.Header.Size =
                    (uint)Marshal.SizeOf<DISPLAYCONFIG_SOURCE_DEVICE_NAME>();
                source.Header.AdapterId = path.SourceInfo.AdapterId;
                source.Header.Id = path.SourceInfo.Id;
                result = DisplayConfigGetSourceName(ref source);
                if (result != ERROR_SUCCESS) {
                    throw new InvalidOperationException(
                        "DisplayConfig source-name query failed with Win32 status "
                        + result);
                }

                var target = new DISPLAYCONFIG_TARGET_DEVICE_NAME();
                target.Header.Type = 2;
                target.Header.Size =
                    (uint)Marshal.SizeOf<DISPLAYCONFIG_TARGET_DEVICE_NAME>();
                target.Header.AdapterId = path.TargetInfo.AdapterId;
                target.Header.Id = path.TargetInfo.Id;
                result = DisplayConfigGetTargetName(ref target);
                if (result != ERROR_SUCCESS) {
                    throw new InvalidOperationException(
                        "DisplayConfig target-name query failed with Win32 status "
                        + result);
                }

                identities.Add(new DisplayPathIdentity {
                    SourceDeviceName = source.ViewGdiDeviceName,
                    MonitorDevicePath = target.MonitorDevicePath,
                    FriendlyName = target.MonitorFriendlyDeviceName,
                    AdapterLuid = FormatLuid(path.TargetInfo.AdapterId),
                    SourceId = path.SourceInfo.Id,
                    TargetId = path.TargetInfo.Id,
                    ConnectorInstance = target.ConnectorInstance,
                    OutputTechnology = target.OutputTechnology,
                    EdidManufactureId = target.EdidManufactureId,
                    EdidProductCodeId = target.EdidProductCodeId,
                    FriendlyNameFromEdid = (target.Flags & 0x1) != 0,
                    FriendlyNameForced = (target.Flags & 0x2) != 0,
                    EdidIdsValid = (target.Flags & 0x4) != 0,
                    TargetAvailable = path.TargetInfo.TargetAvailable,
                    SourceInUse = (path.SourceInfo.StatusFlags & 0x1) != 0,
                    TargetInUse = (path.TargetInfo.StatusFlags & 0x1) != 0,
                });
            }
            return identities.ToArray();
        }
        throw new InvalidOperationException(
            "DisplayConfig topology changed during every bounded query attempt");
    }

    public static bool SendChar(char c) {
        var inputs = new INPUT[2];
        inputs[0].type = 1; inputs[0].U.ki = new KEYBDINPUT { wScan = c, dwFlags = KEYEVENTF_UNICODE };
        inputs[1].type = 1; inputs[1].U.ki = new KEYBDINPUT { wScan = c, dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP };
        return SendInput(2, inputs, Marshal.SizeOf<INPUT>()) == 2;
    }

    public static bool SendVk(ushort vk) {
        var inputs = new INPUT[2];
        inputs[0].type = 1; inputs[0].U.ki = new KEYBDINPUT { wVk = vk };
        inputs[1].type = 1; inputs[1].U.ki = new KEYBDINPUT { wVk = vk, dwFlags = KEYEVENTF_KEYUP };
        return SendInput(2, inputs, Marshal.SizeOf<INPUT>()) == 2;
    }

    public static bool SetClientCursorPos(IntPtr hWnd, int x, int y) {
        var point = new POINT { X = x, Y = y };
        return ClientToScreen(hWnd, ref point) && SetCursorPos(point.X, point.Y);
    }

    public static bool SendRightClick() {
        var inputs = new INPUT[2];
        inputs[0].type = 0;
        inputs[0].U.mi = new MOUSEINPUT { dwFlags = MOUSEEVENTF_RIGHTDOWN };
        inputs[1].type = 0;
        inputs[1].U.mi = new MOUSEINPUT { dwFlags = MOUSEEVENTF_RIGHTUP };
        return SendInput(2, inputs, Marshal.SizeOf<INPUT>()) == 2;
    }

    public static bool SetClientSize(
        IntPtr hWnd,
        int targetWidth,
        int targetHeight,
        int x,
        int y,
        int workingRight,
        int workingBottom
    ) {
        if (targetWidth <= 0 || targetHeight <= 0) return false;
        // Electron and WinUI can apply their non-client frame asynchronously
        // after the first show. Re-measure for up to one second instead of
        // accepting a differently sized client or racing that transition.
        for (int attempt = 0; attempt < 20; attempt++) {
            RECT client, window;
            if (!GetClientRect(hWnd, out client) || !GetWindowRect(hWnd, out window)) return false;
            int clientWidth = client.Right - client.Left;
            int clientHeight = client.Bottom - client.Top;
            int frameWidth = (window.Right - window.Left) - clientWidth;
            int frameHeight = (window.Bottom - window.Top) - clientHeight;
            int outerWidth = targetWidth + frameWidth;
            int outerHeight = targetHeight + frameHeight;
            if (x + outerWidth > workingRight || y + outerHeight > workingBottom) return false;
            if (
                clientWidth == targetWidth
                && clientHeight == targetHeight
                && window.Left == x
                && window.Top == y
            ) return true;
            if (!SetWindowPos(
                hWnd,
                IntPtr.Zero,
                x,
                y,
                outerWidth,
                outerHeight,
                SWP_NOZORDER | SWP_NOACTIVATE
            )) return false;
            System.Threading.Thread.Sleep(50);
        }
        RECT finalClient, finalWindow;
        return GetClientRect(hWnd, out finalClient)
            && GetWindowRect(hWnd, out finalWindow)
            && finalClient.Right - finalClient.Left == targetWidth
            && finalClient.Bottom - finalClient.Top == targetHeight
            && finalWindow.Left == x
            && finalWindow.Top == y
            && finalWindow.Right <= workingRight
            && finalWindow.Bottom <= workingBottom;
    }

    // Captures the window's client-area pixels (BGRA bottom-up) via PrintWindow.
    public static byte[] CaptureWindow(IntPtr hWnd, out int w, out int h) {
        RECT rc;
        if (!GetClientRect(hWnd, out rc)) {
            w = 0; h = 0;
            return null;
        }
        w = rc.Right - rc.Left; h = rc.Bottom - rc.Top;
        if (w <= 0 || h <= 0) return null;
        long pixels = checked((long)w * h);
        // 16M BGRA pixels = 64 MiB. This admits 5120x2880 capture while
        // rejecting parameter-driven GiB allocations.
        if (pixels > 16L * 1024L * 1024L) return null;
        int byteCount = checked((int)(pixels * 4L));
        IntPtr screen = IntPtr.Zero;
        IntPtr mem = IntPtr.Zero;
        IntPtr bmp = IntPtr.Zero;
        IntPtr old = IntPtr.Zero;
        try {
            screen = GetDC(IntPtr.Zero);
            if (screen == IntPtr.Zero) return null;
            mem = CreateCompatibleDC(screen);
            if (mem == IntPtr.Zero) return null;
            bmp = CreateCompatibleBitmap(screen, w, h);
            if (bmp == IntPtr.Zero) return null;
            old = SelectObject(mem, bmp);
            if (old == IntPtr.Zero) return null;
            // The bitmap is deliberately sized to GetClientRect. Without
            // PW_CLIENTONLY, PrintWindow starts with the non-client frame and
            // truncates the bottom/right of the client.
            if (!PrintWindow(
                hWnd,
                mem,
                PW_CLIENTONLY | PW_RENDERFULLCONTENT
            )) return null;
            var bi = new BITMAPINFO();
            bi.bmiHeader.biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>();
            bi.bmiHeader.biWidth = w; bi.bmiHeader.biHeight = h;
            bi.bmiHeader.biPlanes = 1; bi.bmiHeader.biBitCount = 32;
            var bits = new byte[byteCount];
            return GetDIBits(mem, bmp, 0, (uint)h, bits, ref bi, 0) == h
                ? bits
                : null;
        } finally {
            if (old != IntPtr.Zero && mem != IntPtr.Zero) {
                SelectObject(mem, old);
            }
            if (bmp != IntPtr.Zero) DeleteObject(bmp);
            if (mem != IntPtr.Zero) DeleteDC(mem);
            if (screen != IntPtr.Zero) ReleaseDC(IntPtr.Zero, screen);
        }
    }

    // Capture only a bounded client-area region. The viewport offset clips
    // PrintWindow into the small destination bitmap, avoiding full-frame BGRA
    // allocation and transfer during high-resolution polling.
    public static byte[] CaptureWindowRegion(
        IntPtr hWnd,
        int x,
        int y,
        int w,
        int h
    ) {
        RECT rc;
        if (
            !GetClientRect(hWnd, out rc)
            || x < 0
            || y < 0
            || w <= 0
            || h <= 0
            || x > rc.Right - rc.Left - w
            || y > rc.Bottom - rc.Top - h
        ) return null;
        long pixels = checked((long)w * h);
        if (pixels > 2L * 1024L * 1024L) return null;
        int byteCount = checked((int)(pixels * 4L));
        IntPtr screen = IntPtr.Zero;
        IntPtr mem = IntPtr.Zero;
        IntPtr bmp = IntPtr.Zero;
        IntPtr old = IntPtr.Zero;
        POINT oldViewport = new POINT();
        bool viewportChanged = false;
        try {
            screen = GetDC(IntPtr.Zero);
            if (screen == IntPtr.Zero) return null;
            mem = CreateCompatibleDC(screen);
            if (mem == IntPtr.Zero) return null;
            bmp = CreateCompatibleBitmap(screen, w, h);
            if (bmp == IntPtr.Zero) return null;
            old = SelectObject(mem, bmp);
            if (old == IntPtr.Zero) return null;
            viewportChanged = SetViewportOrgEx(mem, -x, -y, out oldViewport);
            if (!viewportChanged) return null;
            if (!PrintWindow(
                hWnd,
                mem,
                PW_CLIENTONLY | PW_RENDERFULLCONTENT
            )) return null;
            POINT ignored;
            SetViewportOrgEx(mem, oldViewport.X, oldViewport.Y, out ignored);
            viewportChanged = false;
            var bi = new BITMAPINFO();
            bi.bmiHeader.biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>();
            bi.bmiHeader.biWidth = w; bi.bmiHeader.biHeight = h;
            bi.bmiHeader.biPlanes = 1; bi.bmiHeader.biBitCount = 32;
            var bits = new byte[byteCount];
            return GetDIBits(mem, bmp, 0, (uint)h, bits, ref bi, 0) == h
                ? bits
                : null;
        } finally {
            if (viewportChanged && mem != IntPtr.Zero) {
                POINT ignored;
                SetViewportOrgEx(
                    mem,
                    oldViewport.X,
                    oldViewport.Y,
                    out ignored
                );
            }
            if (old != IntPtr.Zero && mem != IntPtr.Zero) {
                SelectObject(mem, old);
            }
            if (bmp != IntPtr.Zero) DeleteObject(bmp);
            if (mem != IntPtr.Zero) DeleteDC(mem);
            if (screen != IntPtr.Zero) ReleaseDC(IntPtr.Zero, screen);
        }
    }
}
}
'@
}

# PowerShell itself can be DPI-unaware. Without a thread override Windows
# virtualizes GetClientRect/ClientToScreen coordinates while Kettle reports
# physical pixels, moving a nominal menu-row hover far outside the row on the
# 175-200% displays this harness is specifically intended to investigate.
$dpiContext = [KettlePerf.Native]::SetThreadDpiAwarenessContext(
    [KettlePerf.Native]::PerMonitorAwareV2
)
if ($dpiContext -eq [IntPtr]::Zero) {
    throw 'Could not enable per-monitor-v2 DPI awareness for the performance harness'
}

function Convert-KettlePerfMonitorIdText {
    param($Characters)

    if ($null -eq $Characters) {
        return $null
    }
    $text = [Text.StringBuilder]::new()
    foreach ($value in @($Characters)) {
        $code = [int]$value
        if ($code -eq 0) {
            continue
        }
        if (
            $code -lt 0x20 -or
            $code -eq 0x7f -or
            $code -gt [char]::MaxValue -or
            $text.Length -ge 128
        ) {
            return $null
        }
        [void]$text.Append([char]$code)
    }
    if ($text.Length -eq 0) {
        return $null
    }
    return $text.ToString()
}

function Get-KettlePerfMonitorHardwareId {
    param([AllowEmptyString()][string]$DeviceOrInstanceId = '')

    if (-not $DeviceOrInstanceId) {
        return $null
    }
    $match = [regex]::Match(
        $DeviceOrInstanceId,
        '^(?:(?:MONITOR|DISPLAY)\\|\\\\\?\\DISPLAY#)' +
            '(?<hardware>[A-Z0-9_-]{1,64})(?:\\|#)',
        [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor
            [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if (-not $match.Success) {
        return $null
    }
    return $match.Groups['hardware'].Value
}

function Convert-KettlePerfEdidManufacturerCode {
    param([ValidateRange(0, 65535)][int]$Value)

    $first = ($Value -shr 10) -band 0x1f
    $second = ($Value -shr 5) -band 0x1f
    $third = $Value -band 0x1f
    if (
        $first -lt 1 -or $first -gt 26 -or
        $second -lt 1 -or $second -gt 26 -or
        $third -lt 1 -or $third -gt 26
    ) {
        return $null
    }
    return -join @(
        [char](64 + $first),
        [char](64 + $second),
        [char](64 + $third)
    )
}

function Convert-KettlePerfEdidDescriptorText {
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,
        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 4095)]
        [int]$Offset
    )

    if ($Offset -gt $Bytes.Length - 13) {
        return $null
    }
    $text = [Text.StringBuilder]::new()
    for ($index = 0; $index -lt 13; $index++) {
        $value = [int]$Bytes[$Offset + $index]
        if ($value -eq 0 -or $value -eq 0x0a -or $value -eq 0x0d) {
            break
        }
        if ($value -lt 0x20 -or $value -gt 0x7e) {
            return $null
        }
        [void]$text.Append([char]$value)
    }
    $result = $text.ToString().Trim()
    if (-not $result) {
        return $null
    }
    return $result
}

function ConvertTo-KettlePerfEdidEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,
        [Parameter(Mandatory = $true)]
        [ValidatePattern('^[A-Z]{3}[0-9A-F]{4}$')]
        [string]$ExpectedHardwareId
    )

    if (
        $Bytes.Length -lt 128 -or
        $Bytes.Length -gt 4096 -or
        ($Bytes.Length % 128) -ne 0
    ) {
        throw 'the exact monitor registry EDID has an invalid byte length'
    }
    $header = [byte[]]@(0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00)
    for ($index = 0; $index -lt $header.Length; $index++) {
        if ($Bytes[$index] -ne $header[$index]) {
            throw 'the exact monitor registry EDID has an invalid header'
        }
    }
    $declaredBlocks = 1 + [int]$Bytes[126]
    if ($Bytes.Length -ne (128 * $declaredBlocks)) {
        throw 'the exact monitor registry EDID block count is inconsistent'
    }
    for ($block = 0; $block -lt $declaredBlocks; $block++) {
        $sum = 0
        for ($index = 0; $index -lt 128; $index++) {
            $sum = ($sum + [int]$Bytes[($block * 128) + $index]) -band 0xff
        }
        if ($sum -ne 0) {
            throw "the exact monitor registry EDID block $block has a bad checksum"
        }
    }

    $manufacturerValue = (
        ([int]$Bytes[8] -shl 8) -bor [int]$Bytes[9]
    )
    $manufacturer = Convert-KettlePerfEdidManufacturerCode $manufacturerValue
    $product = '{0:X4}' -f (
        ([int]$Bytes[11] -shl 8) -bor [int]$Bytes[10]
    )
    if (
        -not $manufacturer -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            "$manufacturer$product",
            $ExpectedHardwareId
        )
    ) {
        throw 'the exact monitor registry EDID does not match its device interface'
    }

    $friendlyName = $null
    $serialNumber = $null
    foreach ($offset in @(54, 72, 90, 108)) {
        if (
            $Bytes[$offset] -ne 0 -or
            $Bytes[$offset + 1] -ne 0 -or
            $Bytes[$offset + 2] -ne 0
        ) {
            continue
        }
        $descriptorType = [int]$Bytes[$offset + 3]
        if ($descriptorType -eq 0xfc) {
            $friendlyName = Convert-KettlePerfEdidDescriptorText `
                -Bytes $Bytes -Offset ($offset + 5)
        } elseif ($descriptorType -eq 0xff) {
            $serialNumber = Convert-KettlePerfEdidDescriptorText `
                -Bytes $Bytes -Offset ($offset + 5)
        }
    }
    if (-not $serialNumber) {
        $binarySerial = (
            [uint32]$Bytes[12] -bor
            ([uint32]$Bytes[13] -shl 8) -bor
            ([uint32]$Bytes[14] -shl 16) -bor
            ([uint32]$Bytes[15] -shl 24)
        )
        if ($binarySerial -ne 0 -and $binarySerial -ne [uint32]::MaxValue) {
            $serialNumber = '{0:X8}' -f $binarySerial
        }
    }
    $manufactureWeek = if (
        $Bytes[16] -ne 0 -and $Bytes[16] -ne 0xff
    ) {
        [int]$Bytes[16]
    } else {
        $null
    }
    $manufactureYear = if (
        $Bytes[17] -ne 0 -and $Bytes[17] -ne 0xff
    ) {
        1990 + [int]$Bytes[17]
    } else {
        $null
    }
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $hash = [BitConverter]::ToString(
            $algorithm.ComputeHash($Bytes)
        ).Replace('-', '').ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
    return [pscustomobject][ordered]@{
        manufacturer_code = $manufacturer
        product_code = $product
        friendly_name = $friendlyName
        serial_number = $serialNumber
        manufacture_week = $manufactureWeek
        manufacture_year = $manufactureYear
        block_count = $declaredBlocks
        sha256 = $hash
    }
}

function Get-KettlePerfMonitorDevicePathPart {
    param(
        [Parameter(Mandatory = $true)]
        [string]$MonitorDevicePath
    )

    $pattern = (
        '^\\\\\?\\DISPLAY#(?<hardware>[A-Z]{3}[0-9A-F]{4})#' +
        '(?<instance>[^#\\]{1,128})#\{' +
        'e6f07b5f-ee97-4a90-b076-33f57bf4eaa7\}$'
    )
    $match = [regex]::Match(
        $MonitorDevicePath,
        $pattern,
        [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor
            [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if (-not $match.Success) {
        return $null
    }
    return [pscustomobject][ordered]@{
        hardware_id = $match.Groups['hardware'].Value.ToUpperInvariant()
        instance_id = $match.Groups['instance'].Value
    }
}

function Read-KettlePerfExactMonitorEdid {
    param(
        [Parameter(Mandatory = $true)]
        [string]$MonitorDevicePath
    )

    $parts = Get-KettlePerfMonitorDevicePathPart $MonitorDevicePath
    if ($null -eq $parts) {
        throw 'DisplayConfig returned an invalid monitor device-interface path'
    }
    $subkey = (
        'SYSTEM\CurrentControlSet\Enum\DISPLAY\' +
        $parts.hardware_id + '\' + $parts.instance_id +
        '\Device Parameters'
    )
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::LocalMachine,
        [Microsoft.Win32.RegistryView]::Default
    )
    try {
        $key = $base.OpenSubKey($subkey, $false)
        if ($null -eq $key) {
            throw 'the exact active monitor registry key is unavailable'
        }
        try {
            $value = $key.GetValue(
                'EDID',
                $null,
                [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
            )
        } finally {
            $key.Dispose()
        }
    } finally {
        $base.Dispose()
    }
    if ($null -eq $value -or -not ($value -is [byte[]])) {
        throw 'the exact active monitor registry key has no binary EDID'
    }
    return [byte[]]$value
}

function Convert-KettlePerfCcdPathEvidence {
    param(
        [Parameter(Mandatory = $true)]
        $Path,
        [hashtable]$RegistryEdidByPath
    )

    $devicePath = [string]$Path.MonitorDevicePath
    $parts = Get-KettlePerfMonitorDevicePathPart $devicePath
    if (
        $null -eq $parts -or
        -not [bool]$Path.TargetAvailable -or
        -not [bool]$Path.SourceInUse -or
        -not [bool]$Path.TargetInUse -or
        -not [bool]$Path.EdidIdsValid -or
        [bool]$Path.FriendlyNameForced -or
        -not (Test-KettlePerfPhysicalOutputTechnology (
            $Path.OutputTechnology
        ))
    ) {
        throw 'DisplayConfig path is not active physical EDID evidence'
    }

    $edidBytes = if (
        $null -ne $RegistryEdidByPath -and
        $RegistryEdidByPath.ContainsKey($devicePath)
    ) {
        [byte[]]$RegistryEdidByPath[$devicePath]
    } else {
        Read-KettlePerfExactMonitorEdid $devicePath
    }
    $edid = ConvertTo-KettlePerfEdidEvidence `
        -Bytes $edidBytes -ExpectedHardwareId $parts.hardware_id

    $manufacturerValues = @(
        [int]$Path.EdidManufactureId,
        (
            (([int]$Path.EdidManufactureId -band 0xff) -shl 8) -bor
            (([int]$Path.EdidManufactureId -shr 8) -band 0xff)
        )
    )
    $manufacturerMatches = @(
        $manufacturerValues |
            ForEach-Object { Convert-KettlePerfEdidManufacturerCode $_ } |
            Where-Object {
                $_ -and [StringComparer]::OrdinalIgnoreCase.Equals(
                    $_,
                    $edid.manufacturer_code
                )
            } |
            Select-Object -Unique
    )
    $productValues = @(
        [int]$Path.EdidProductCodeId,
        (
            (([int]$Path.EdidProductCodeId -band 0xff) -shl 8) -bor
            (([int]$Path.EdidProductCodeId -shr 8) -band 0xff)
        )
    )
    $productMatches = @(
        $productValues |
            ForEach-Object { '{0:X4}' -f $_ } |
            Where-Object {
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    $_,
                    $edid.product_code
                )
            } |
            Select-Object -Unique
    )
    if ($manufacturerMatches.Count -ne 1 -or $productMatches.Count -ne 1) {
        throw 'DisplayConfig EDID identifiers disagree with the exact registry EDID'
    }
    $ccdFriendlyName = if (
        [bool]$Path.FriendlyNameFromEdid -and
        -not [string]::IsNullOrWhiteSpace([string]$Path.FriendlyName)
    ) {
        ([string]$Path.FriendlyName).Trim()
    } else {
        $null
    }
    if (
        $ccdFriendlyName -and
        $edid.friendly_name -and
        -not [StringComparer]::Ordinal.Equals(
            $ccdFriendlyName,
            [string]$edid.friendly_name
        )
    ) {
        throw 'DisplayConfig friendly name disagrees with the exact registry EDID'
    }
    $instanceName = (
        'DISPLAY\' + $parts.hardware_id + '\' + $parts.instance_id
    )
    $monitor = [pscustomobject][ordered]@{
        identity_source = 'display-config-ccd-registry-edid-v1'
        instance_name = $instanceName
        hardware_id = $parts.hardware_id
        manufacturer_code = $edid.manufacturer_code
        product_code = $edid.product_code
        friendly_name = if ($edid.friendly_name) {
            $edid.friendly_name
        } else {
            $ccdFriendlyName
        }
        serial_number = $edid.serial_number
        manufacture_week = $edid.manufacture_week
        manufacture_year = $edid.manufacture_year
        monitor_device_path = $devicePath
        registry_edid_sha256 = $edid.sha256
        registry_edid_block_count = $edid.block_count
        adapter_luid = [string]$Path.AdapterLuid
        source_id = [uint32]$Path.SourceId
        target_id = [uint32]$Path.TargetId
        connector_instance = [uint32]$Path.ConnectorInstance
        output_technology = [int]$Path.OutputTechnology
    }
    $connection = [pscustomobject][ordered]@{
        identity_source = 'display-config-ccd-registry-edid-v1'
        instance_name = $instanceName
        hardware_id = $parts.hardware_id
        video_output_technology = [int]$Path.OutputTechnology
        adapter_luid = [string]$Path.AdapterLuid
        source_id = [uint32]$Path.SourceId
        target_id = [uint32]$Path.TargetId
        connector_instance = [uint32]$Path.ConnectorInstance
    }
    return [pscustomobject][ordered]@{
        source_device_name = [string]$Path.SourceDeviceName
        monitor = $monitor
        connection = $connection
    }
}

function Resolve-KettlePerfDisplayIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$DesktopScreens,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$WmiMonitors,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$WmiConnections,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$CcdPaths,
        [Parameter(Mandatory = $true)]
        [ValidateSet('available', 'unavailable')]
        [string]$CcdStatus,
        [hashtable]$RegistryEdidByPath
    )

    $issues = [Collections.Generic.List[string]]::new()
    $ccdEvidence = [Collections.Generic.List[object]]::new()
    $invalidCcdSources = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    if ($CcdStatus -eq 'available') {
        foreach ($path in $CcdPaths) {
            $source = [string]$path.SourceDeviceName
            if (-not $source) {
                [void]$invalidCcdSources.Add('')
                continue
            }
            try {
                $ccdEvidence.Add((
                    Convert-KettlePerfCcdPathEvidence `
                        -Path $path -RegistryEdidByPath $RegistryEdidByPath
                ))
            } catch {
                [void]$invalidCcdSources.Add($source)
            }
        }
    }

    $resolvedScreens = [Collections.Generic.List[object]]::new()
    foreach ($screen in $DesktopScreens) {
        $deviceName = [string]$screen.device_name
        $screenHardware = Get-KettlePerfMonitorHardwareId (
            [string]$screen.monitor_device_id
        )
        $wmiMatches = @(
            if ($screenHardware) {
                $WmiMonitors | Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$_.hardware_id,
                        $screenHardware
                    )
                }
            }
        )
        $rawCcdMatches = @(
            if ($CcdStatus -eq 'available') {
                $CcdPaths | Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$_.SourceDeviceName,
                        $deviceName
                    )
                }
            }
        )
        $validCcdMatches = @(
            $ccdEvidence | Where-Object {
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$_.source_device_name,
                    $deviceName
                )
            }
        )
        $strictCcdEvidence = if (
            $CcdStatus -eq 'available' -and
            $rawCcdMatches.Count -eq 1 -and
            $validCcdMatches.Count -eq 1 -and
            -not $invalidCcdSources.Contains($deviceName) -and
            (
                -not $screenHardware -or
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$validCcdMatches[0].monitor.hardware_id,
                    $screenHardware
                )
            )
        ) {
            $validCcdMatches[0]
        } else {
            $null
        }
        $chosenMonitor = $null
        $chosenConnection = $null
        if ($wmiMatches.Count -eq 1) {
            $wmi = $wmiMatches[0]
            $wmiInstanceConnectionMatches = @(
                $WmiConnections | Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$_.instance_name,
                        [string]$wmi.instance_name
                    )
                }
            )
            $wmiHardwareConnectionMatches = @(
                $WmiConnections | Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$_.hardware_id,
                        $screenHardware
                    )
                }
            )
            $wmiConnection = if (
                $wmiInstanceConnectionMatches.Count -eq 1 -and
                [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$wmiInstanceConnectionMatches[0].hardware_id,
                    $screenHardware
                ) -and
                (Test-KettlePerfPhysicalOutputTechnology (
                    $wmiInstanceConnectionMatches[0].video_output_technology
                ))
            ) {
                $wmiInstanceConnectionMatches[0]
            } else {
                $null
            }
            if ($null -ne $wmiConnection) {
                $ccdCorroboration = $strictCcdEvidence
                $chosenMonitor = [pscustomobject][ordered]@{
                    identity_source = 'wmi-monitor-id-v1'
                    instance_name = [string]$wmi.instance_name
                    hardware_id = [string]$wmi.hardware_id
                    manufacturer_code = $wmi.manufacturer_code
                    product_code = $wmi.product_code
                    friendly_name = $wmi.friendly_name
                    serial_number = $wmi.serial_number
                    manufacture_week = $wmi.manufacture_week
                    manufacture_year = $wmi.manufacture_year
                    monitor_device_path = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.monitor_device_path
                    } else {
                        $null
                    }
                    registry_edid_sha256 = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.registry_edid_sha256
                    } else {
                        $null
                    }
                    registry_edid_block_count = if (
                        $null -ne $ccdCorroboration
                    ) {
                        $ccdCorroboration.monitor.registry_edid_block_count
                    } else {
                        $null
                    }
                    adapter_luid = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.adapter_luid
                    } else {
                        $null
                    }
                    source_id = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.source_id
                    } else {
                        $null
                    }
                    target_id = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.target_id
                    } else {
                        $null
                    }
                    connector_instance = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.connector_instance
                    } else {
                        $null
                    }
                    output_technology = if ($null -ne $ccdCorroboration) {
                        $ccdCorroboration.monitor.output_technology
                    } else {
                        $null
                    }
                }
                $chosenConnection = [pscustomobject][ordered]@{
                    identity_source = 'wmi-monitor-connection-v1'
                    instance_name = [string]$wmiConnection.instance_name
                    hardware_id = [string]$wmiConnection.hardware_id
                    video_output_technology = (
                        $wmiConnection.video_output_technology
                    )
                    adapter_luid = $null
                    source_id = $null
                    target_id = $null
                    connector_instance = $null
                }
            } elseif (
                $wmiInstanceConnectionMatches.Count -eq 0 -and
                $wmiHardwareConnectionMatches.Count -eq 0 -and
                $null -ne $strictCcdEvidence
            ) {
                $chosenMonitor = $strictCcdEvidence.monitor
                $chosenConnection = $strictCcdEvidence.connection
                $screenHardware = [string]$chosenMonitor.hardware_id
            }
        } elseif ($null -ne $strictCcdEvidence) {
            $chosenMonitor = $strictCcdEvidence.monitor
            $chosenConnection = $strictCcdEvidence.connection
            $screenHardware = [string]$chosenMonitor.hardware_id
        }

        if ($null -eq $chosenMonitor) {
            [void]$issues.Add(
                "screen $deviceName has no unique fail-closed physical EDID identity"
            )
        }
        $resolvedScreens.Add([pscustomobject][ordered]@{
            device_name = $deviceName
            monitor_device_id = $screen.monitor_device_id
            monitor_hardware_id = $screenHardware
            primary = [bool]$screen.primary
            edid_backed = $null -ne $chosenMonitor
            edid_match_count = if ($null -ne $chosenMonitor) { 1 } else { 0 }
            edid_monitor = $chosenMonitor
            connection = $chosenConnection
            effective_dpi = $screen.effective_dpi
            scale_factor = $screen.scale_factor
            refresh_hz = $screen.refresh_hz
            bounds = $screen.bounds
            working_area = $screen.working_area
            requested_client_fits = [bool]$screen.requested_client_fits
        })
    }

    $identityCounts = [Collections.Generic.Dictionary[string, int]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($screen in $resolvedScreens) {
        if ($null -eq $screen.edid_monitor) {
            continue
        }
        $identity = [string]$screen.edid_monitor.instance_name
        if ($identityCounts.ContainsKey($identity)) {
            $identityCounts[$identity]++
        } else {
            $identityCounts[$identity] = 1
        }
    }
    foreach ($screen in $resolvedScreens) {
        if ($null -eq $screen.edid_monitor) {
            continue
        }
        $identity = [string]$screen.edid_monitor.instance_name
        if ($identityCounts[$identity] -gt 1) {
            [void]$issues.Add(
                "physical monitor identity $identity maps to multiple desktop screens"
            )
            $screen.edid_backed = $false
            $screen.edid_match_count = $identityCounts[$identity]
            $screen.edid_monitor = $null
            $screen.connection = $null
        }
    }

    $activeMonitors = @(
        $resolvedScreens |
            Where-Object { $null -ne $_.edid_monitor } |
            ForEach-Object { $_.edid_monitor } |
            Sort-Object -Property instance_name
    )
    $activeConnections = @(
        $resolvedScreens |
            Where-Object { $null -ne $_.connection } |
            ForEach-Object { $_.connection } |
            Sort-Object -Property instance_name
    )
    $identitySources = @(
        $activeMonitors |
            ForEach-Object { [string]$_.identity_source } |
            Sort-Object -Unique
    )
    $method = if ($identitySources.Count -eq 0) {
        'none'
    } elseif ($identitySources.Count -eq 1) {
        $identitySources[0]
    } else {
        'hybrid-wmi-monitor-id-and-display-config-ccd-v1'
    }
    return [pscustomobject][ordered]@{
        identity_acquisition = [pscustomobject][ordered]@{
            schema = 'kettle-display-identity-acquisition-v2'
            resolver = 'wmi-monitor-id-with-ccd-registry-fallback-v2'
            method = $method
            ccd_status = $CcdStatus
            desktop_screen_count = $DesktopScreens.Count
            wmi_active_monitor_count = $WmiMonitors.Count
            wmi_active_connection_count = $WmiConnections.Count
            ccd_active_path_count = $CcdPaths.Count
            resolved_screen_count = $activeMonitors.Count
        }
        desktop_screens = [object[]]$resolvedScreens.ToArray()
        active_physical_monitors = [object[]]$activeMonitors
        active_connections = [object[]]$activeConnections
        issues = [object[]]$issues.ToArray()
    }
}

function Get-KettlePerfDisplayIdentityTopology {
    param(
        [AllowEmptyString()]
        [string]$TargetScreenDevice = '',
        [ValidateRange(0, 16384)]
        [int]$ClientWidth = 0,
        [ValidateRange(0, 16384)]
        [int]$ClientHeight = 0,
        [ValidateRange(0, 1024)]
        [int]$NonClientWidthAllowance = 0,
        [ValidateRange(0, 1024)]
        [int]$NonClientHeightAllowance = 0
    )

    Add-Type -AssemblyName System.Windows.Forms
    $desktopScreens = @(
        [Windows.Forms.Screen]::AllScreens |
            ForEach-Object {
                $centerX = $_.Bounds.X + [int]($_.Bounds.Width / 2)
                $centerY = $_.Bounds.Y + [int]($_.Bounds.Height / 2)
                $dpi = [KettlePerf.Native]::EffectiveDpiAt($centerX, $centerY)
                $fits = (
                    $ClientWidth -eq 0 -or
                    (
                        $_.WorkingArea.Width -ge (
                            $ClientWidth + $NonClientWidthAllowance
                        ) -and
                        $_.WorkingArea.Height -ge (
                            $ClientHeight + $NonClientHeightAllowance
                        )
                    )
                )
                [pscustomobject][ordered]@{
                    device_name = $_.DeviceName
                    monitor_device_id = (
                        [KettlePerf.Native]::MonitorDeviceIdForDisplay(
                            $_.DeviceName
                        )
                    )
                    primary = $_.Primary
                    effective_dpi = if ($null -ne $dpi) {
                        [pscustomobject][ordered]@{
                            x = [int]$dpi[0]
                            y = [int]$dpi[1]
                        }
                    } else {
                        $null
                    }
                    scale_factor = if ($null -ne $dpi) {
                        [Math]::Round(([double]$dpi[0] / 96.0), 4)
                    } else {
                        $null
                    }
                    refresh_hz = (
                        [KettlePerf.Native]::CurrentRefreshRate($_.DeviceName)
                    )
                    bounds = [pscustomobject][ordered]@{
                        x = $_.Bounds.X
                        y = $_.Bounds.Y
                        width = $_.Bounds.Width
                        height = $_.Bounds.Height
                    }
                    working_area = [pscustomobject][ordered]@{
                        x = $_.WorkingArea.X
                        y = $_.WorkingArea.Y
                        width = $_.WorkingArea.Width
                        height = $_.WorkingArea.Height
                    }
                    requested_client_fits = $fits
                }
            } |
            Sort-Object -Property device_name, monitor_device_id
    )
    $wmiMonitors = @(
        Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID `
            -ErrorAction SilentlyContinue |
            Where-Object { $_.Active } |
            ForEach-Object {
                [pscustomobject][ordered]@{
                    instance_name = [string]$_.InstanceName
                    hardware_id = Get-KettlePerfMonitorHardwareId (
                        [string]$_.InstanceName
                    )
                    manufacturer_code = (
                        Convert-KettlePerfMonitorIdText $_.ManufacturerName
                    )
                    product_code = (
                        Convert-KettlePerfMonitorIdText $_.ProductCodeID
                    )
                    friendly_name = (
                        Convert-KettlePerfMonitorIdText $_.UserFriendlyName
                    )
                    serial_number = (
                        Convert-KettlePerfMonitorIdText $_.SerialNumberID
                    )
                    manufacture_week = $_.WeekOfManufacture
                    manufacture_year = $_.YearOfManufacture
                }
            } |
            Sort-Object -Property instance_name
    )
    $wmiConnections = @(
        Get-CimInstance -Namespace root\wmi `
            -ClassName WmiMonitorConnectionParams `
            -ErrorAction SilentlyContinue |
            Where-Object { $_.Active } |
            ForEach-Object {
                [pscustomobject][ordered]@{
                    instance_name = [string]$_.InstanceName
                    hardware_id = Get-KettlePerfMonitorHardwareId (
                        [string]$_.InstanceName
                    )
                    video_output_technology = $_.VideoOutputTechnology
                }
            } |
            Sort-Object -Property instance_name
    )
    $ccdStatus = 'available'
    $ccdPaths = @()
    try {
        $ccdPaths = @([KettlePerf.Native]::ActiveDisplayPaths())
    } catch {
        $ccdStatus = 'unavailable'
    }
    $resolved = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens $desktopScreens `
        -WmiMonitors $wmiMonitors `
        -WmiConnections $wmiConnections `
        -CcdPaths $ccdPaths `
        -CcdStatus $ccdStatus
    $primary = @(
        $resolved.desktop_screens | Where-Object { $_.primary }
    ) | Select-Object -First 1
    if (-not $TargetScreenDevice -and $null -ne $primary) {
        $TargetScreenDevice = [string]$primary.device_name
    }
    $targetScreens = @(
        $resolved.desktop_screens | Where-Object {
            [StringComparer]::OrdinalIgnoreCase.Equals(
                [string]$_.device_name,
                $TargetScreenDevice
            )
        }
    )
    $targetMonitors = @(
        $targetScreens |
            Where-Object { $null -ne $_.edid_monitor } |
            ForEach-Object { $_.edid_monitor }
    )
    return [pscustomobject][ordered]@{
        identity_acquisition = $resolved.identity_acquisition
        target_screen_device = $TargetScreenDevice
        primary_screen_device = if ($null -ne $primary) {
            [string]$primary.device_name
        } else {
            $null
        }
        target_monitor_hardware_id = if ($targetMonitors.Count -eq 1) {
            [string]$targetMonitors[0].hardware_id
        } else {
            $null
        }
        desktop_screens = [object[]]$resolved.desktop_screens
        active_physical_monitors = [object[]]$resolved.active_physical_monitors
        active_connections = [object[]]$resolved.active_connections
        target_edid_monitors = [object[]]$targetMonitors
        issues = [object[]]$resolved.issues
    }
}

function Get-VisibleWindowSet {
    $set = [System.Collections.Generic.HashSet[IntPtr]]::new()
    foreach ($h in [KettlePerf.Native]::VisibleTopWindows()) { [void]$set.Add($h) }
    , $set
}

function Get-KettlePerfProcessSnapshot {
    $parents = @{}
    foreach (
        $process in Get-CimInstance Win32_Process |
            Select-Object ProcessId, ParentProcessId
    ) {
        $parents[[int]$process.ProcessId] = [int]$process.ParentProcessId
    }
    return $parents
}

function Test-KettlePerfProcessRelated {
    param(
        [Parameter(Mandatory)] [int]$CandidatePid,
        [Parameter(Mandatory)] [int]$RootPid,
        [Parameter(Mandatory)] [hashtable]$Parents
    )

    if ($CandidatePid -eq $RootPid) {
        return $true
    }
    $ancestor = $CandidatePid
    $visited = [Collections.Generic.HashSet[int]]::new()
    while ($Parents.ContainsKey($ancestor) -and $visited.Add($ancestor)) {
        $ancestor = [int]$Parents[$ancestor]
        if ($ancestor -eq $RootPid) {
            return $true
        }
    }
    return $false
}

# Returns one stable NEW visible top-level window in the launched process tree.
# Requiring ancestry, an exact owner image, and a non-preexisting owner prevents
# a concurrent terminal launch, execution alias, or shared Windows Terminal
# server from becoming benchmark evidence for a different executable.
function Wait-NewWindow {
    param(
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[IntPtr]]$Before,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[int]]$PreexistingPids,
        [Parameter(Mandatory)] [int]$RootPid,
        [string[]]$ProcessNames = @(),
        [string]$ExpectedExecutable = '',
        [string[]]$ExcludedClassNames = @(),
        [int]$TimeoutMs = 20000,
        [ValidateRange(0, 5000)]
        [int]$StabilityMs = 100
    )
    $acceptedNames = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($name in $ProcessNames) {
        if ($name) {
            [void]$acceptedNames.Add($name)
        }
    }
    $excludedClasses = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($name in $ExcludedClassNames) {
        if ($name) {
            [void]$excludedClasses.Add($name)
        }
    }
    $expectedPath = if ($ExpectedExecutable) {
        (Resolve-Path -LiteralPath $ExpectedExecutable -ErrorAction Stop).Path
    } else {
        ''
    }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $stableWindow = [IntPtr]::Zero
    $stableSince = 0L
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $candidates = @()
        foreach ($h in [KettlePerf.Native]::VisibleTopWindows()) {
            if ($Before.Contains($h)) {
                continue
            }
            if ($excludedClasses.Count -gt 0 -and $excludedClasses.Contains((Get-WindowClass $h))) {
                continue
            }
            $windowPid = Get-WindowPid $h
            if (-not $windowPid -or $PreexistingPids.Contains($windowPid)) {
                continue
            }
            if ($acceptedNames.Count -gt 0) {
                try {
                    $processName = (Get-Process -Id $windowPid -ErrorAction Stop).ProcessName
                } catch {
                    continue
                }
                if (-not $acceptedNames.Contains($processName)) {
                    continue
                }
            }
            $ownerPath = [KettlePerf.Native]::ProcessExecutablePath($windowPid)
            if (
                $expectedPath -and
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    $ownerPath,
                    $expectedPath
                )
            ) {
                continue
            }
            $candidates += [pscustomobject]@{
                Hwnd = $h
                Pid = $windowPid
                Executable = $ownerPath
            }
        }
        $related = @()
        if ($candidates.Count -gt 0) {
            $parents = Get-KettlePerfProcessSnapshot
            $related = @(
                $candidates | Where-Object {
                    Test-KettlePerfProcessRelated `
                        -CandidatePid $_.Pid -RootPid $RootPid -Parents $parents
                }
            )
        }
        if ($related.Count -gt 1) {
            throw "ambiguous benchmark launch: $($related.Count) related terminal windows appeared"
        }
        if ($related.Count -eq 1) {
            if ($stableWindow -ne $related[0].Hwnd) {
                $stableWindow = $related[0].Hwnd
                $stableSince = $sw.ElapsedMilliseconds
            } elseif (
                $sw.ElapsedMilliseconds - $stableSince -ge $StabilityMs
            ) {
                return $stableWindow
            }
        } else {
            $stableWindow = [IntPtr]::Zero
            $stableSince = 0L
        }
        Start-Sleep -Milliseconds 15
    }
    return [IntPtr]::Zero
}

function Get-WindowTitle([IntPtr]$h) {
    $sb = [System.Text.StringBuilder]::new(512)
    [void][KettlePerf.Native]::GetWindowText($h, $sb, 512)
    $sb.ToString()
}

function Get-WindowClass([IntPtr]$h) {
    $sb = [System.Text.StringBuilder]::new(256)
    [void][KettlePerf.Native]::GetClassName($h, $sb, 256)
    $sb.ToString()
}

function Get-WindowPid([IntPtr]$h) {
    $procId = 0u
    [void][KettlePerf.Native]::GetWindowThreadProcessId($h, [ref]$procId)
    [int]$procId
}

function Confirm-KettlePerfCommand {
    param(
        [Parameter(Mandatory)] $Spec,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[IntPtr]]$BeforeWindows,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[int]]$PreexistingPids,
        [Parameter(Mandatory)] [int]$RootPid,
        [int]$TimeoutMs = 30000
    )

    if (-not $Spec.CommandConfirmation) {
        return
    }
    if ($Spec.CommandConfirmation -ne 'tabby-run') {
        throw "Unsupported command-confirmation contract: $($Spec.CommandConfirmation)"
    }

    $dialog = [IntPtr]::Zero
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs -and $dialog -eq [IntPtr]::Zero) {
        foreach ($candidate in [KettlePerf.Native]::VisibleTopWindows()) {
            if (
                $BeforeWindows.Contains($candidate) -or
                (Get-WindowClass $candidate) -ne '#32770' -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    (Get-WindowTitle $candidate),
                    'tabby'
                )
            ) {
                continue
            }
            $candidatePid = Get-WindowPid $candidate
            if (-not $candidatePid -or $PreexistingPids.Contains($candidatePid)) {
                continue
            }
            try {
                $processName = (Get-Process -Id $candidatePid -ErrorAction Stop).ProcessName
            } catch {
                continue
            }
            if ($processName -notin $Spec.WindowProcessNames) {
                continue
            }
            $parents = Get-KettlePerfProcessSnapshot
            if (-not (
                Test-KettlePerfProcessRelated `
                    -CandidatePid $candidatePid -RootPid $RootPid -Parents $parents
            )) {
                continue
            }
            $dialog = $candidate
            break
        }
        if ($dialog -eq [IntPtr]::Zero) {
            Start-Sleep -Milliseconds 15
        }
    }
    if ($dialog -eq [IntPtr]::Zero) {
        throw 'Tabby command confirmation did not appear in a newly spawned process'
    }

    $runButtons = @(
        [KettlePerf.Native]::EnabledChildWindowsByClassAndTitle(
            $dialog,
            'Button',
            'Run'
        )
    )
    if ($runButtons.Count -ne 1) {
        throw "Tabby confirmation has $($runButtons.Count) enabled Run buttons; refusing automation"
    }
    if (-not [KettlePerf.Native]::ClickButton($runButtons[0])) {
        throw 'Could not click the verified Tabby Run button'
    }
}

function New-KettlePerfCommandWrapper {
    param(
        [Parameter(Mandatory)] [string]$OutputDirectory,
        [Parameter(Mandatory)] [string[]]$Command,
        [string]$PowerShellExecutable = ''
    )

    if ($Command.Count -eq 0) {
        throw 'Cannot compile an empty performance command'
    }
    if (-not $PowerShellExecutable) {
        $PowerShellExecutable = Join-Path $env:SystemRoot (
            'System32\WindowsPowerShell\v1.0\powershell.exe'
        )
    }
    if (-not [IO.Path]::IsPathRooted($PowerShellExecutable)) {
        throw 'Tabby command launcher must be an absolute path'
    }
    $PowerShellExecutable = (
        Resolve-Path -LiteralPath $PowerShellExecutable -ErrorAction Stop
    ).Path
    $executable = $Command[0]
    if (-not [IO.Path]::IsPathRooted($executable)) {
        $executable = Get-Command $executable -CommandType Application -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    }
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Performance command executable not found: $executable"
    }
    $executable = (Resolve-Path -LiteralPath $executable).Path
    $arguments = Join-KettlePerfArguments @($Command | Select-Object -Skip 1)
    if ($executable.Length -gt 32767 -or $arguments.Length -gt 32767) {
        throw 'Performance command exceeds the Windows process-argument limit'
    }
    if ($executable.Contains([char]0) -or $arguments.Contains([char]0)) {
        throw 'Performance command contains a NUL character'
    }

    $executableBase64 = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($executable)
    )
    $argumentsBase64 = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($arguments)
    )
    $launcher = @'
$ErrorActionPreference = 'Stop'
$executable = [Text.Encoding]::UTF8.GetString(
    [Convert]::FromBase64String('__KETTLE_PERF_EXECUTABLE__')
)
$arguments = [Text.Encoding]::UTF8.GetString(
    [Convert]::FromBase64String('__KETTLE_PERF_ARGUMENTS__')
)
$startInfo = New-Object Diagnostics.ProcessStartInfo
$startInfo.FileName = $executable
$startInfo.Arguments = $arguments
$startInfo.UseShellExecute = $false
$process = [Diagnostics.Process]::Start($startInfo)
if ($null -eq $process) {
    exit 67
}
$process.WaitForExit()
exit $process.ExitCode
'@
    $launcher = $launcher.Replace(
        '__KETTLE_PERF_EXECUTABLE__',
        $executableBase64
    )
    $launcher = $launcher.Replace(
        '__KETTLE_PERF_ARGUMENTS__',
        $argumentsBase64
    )
    $encodedLauncher = [Convert]::ToBase64String(
        [Text.Encoding]::Unicode.GetBytes($launcher)
    )
    $wrapperLine = (
        '@' + [KettlePerf.Native]::QuoteArgument($PowerShellExecutable) + ' ' +
        '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass ' +
        "-EncodedCommand $encodedLauncher`r`n"
    )
    $wrapperBytes = [Text.Encoding]::ASCII.GetBytes($wrapperLine)
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    $OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
    $wrapperPath = Join-Path $OutputDirectory (
        "kettle-perf-command-$PID-$([Guid]::NewGuid().ToString('N')).cmd"
    )
    $writer = $null
    $lock = $null
    try {
        # CreateNew defeats pre-creation. Reopen read-only after the durable
        # write so cmd.exe can read it while the retained handle denies later
        # replacement, mutation, or deletion for the benchmark lifetime.
        $writer = [IO.FileStream]::new(
            $wrapperPath,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::Read
        )
        $writer.Write($wrapperBytes, 0, $wrapperBytes.Length)
        $writer.Flush($true)
        $writer.Dispose()
        $writer = $null
        $lock = [IO.FileStream]::new(
            $wrapperPath,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        $readBack = [byte[]]::new($wrapperBytes.Length)
        $offset = 0
        while ($offset -lt $readBack.Length) {
            $read = $lock.Read($readBack, $offset, $readBack.Length - $offset)
            if ($read -eq 0) {
                break
            }
            $offset += $read
        }
        if (
            $offset -ne $wrapperBytes.Length -or
            [Convert]::ToBase64String($readBack) -ne
                [Convert]::ToBase64String($wrapperBytes)
        ) {
            throw 'Tabby benchmark command wrapper changed while it was locked'
        }
        $lock.Position = 0
        return [pscustomobject]@{
            Path = $wrapperPath
            Lock = $lock
        }
    } catch {
        if ($null -ne $writer) {
            $writer.Dispose()
        }
        if ($null -ne $lock) {
            $lock.Dispose()
        }
        Remove-Item -LiteralPath $wrapperPath -Force -ErrorAction SilentlyContinue
        throw
    }
}

function Close-KettlePerfCommandWrapper {
    param(
        [Parameter(Mandatory)] $Wrapper
    )

    if ($null -ne $Wrapper.Lock) {
        $Wrapper.Lock.Dispose()
    }
    if ($Wrapper.Path) {
        Remove-Item -LiteralPath $Wrapper.Path -Force -ErrorAction SilentlyContinue
    }
}

function Wait-KettlePerfDescendant {
    param(
        [Parameter(Mandatory)] [int]$RootPid,
        [Parameter(Mandatory)] [string]$Executable,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[int]]$PreexistingPids,
        [int]$TimeoutMs = 30000
    )

    if (-not [IO.Path]::IsPathRooted($Executable)) {
        $Executable = Get-Command $Executable -CommandType Application `
            -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    }
    $expectedPath = (Resolve-Path -LiteralPath $Executable).Path
    $targetName = [IO.Path]::GetFileName($expectedPath)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $processes = @(Get-CimInstance Win32_Process |
            Select-Object ProcessId, ParentProcessId, Name, ExecutablePath)
        $parents = @{}
        foreach ($process in $processes) {
            $parents[[int]$process.ProcessId] = [int]$process.ParentProcessId
        }
        foreach ($process in $processes) {
            $candidatePid = [int]$process.ProcessId
            if (
                $PreexistingPids.Contains($candidatePid) -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$process.Name,
                    $targetName
                ) -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$process.ExecutablePath,
                    $expectedPath
                )
            ) {
                continue
            }
            $ancestor = $candidatePid
            $visited = [Collections.Generic.HashSet[int]]::new()
            while ($parents.ContainsKey($ancestor) -and $visited.Add($ancestor)) {
                $ancestor = $parents[$ancestor]
                if ($ancestor -eq $RootPid) {
                    return $candidatePid
                }
            }
        }
        Start-Sleep -Milliseconds 50
    }
    return $null
}

function Start-KettlePerfCommandWindow {
    param(
        [Parameter(Mandatory)] $Spec,
        [Parameter(Mandatory)] [string[]]$Command,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[IntPtr]]$BeforeWindows,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[int]]$PreexistingPids,
        [string]$CommandWrapperDirectory = '',
        [switch]$DeferTargetAttribution
    )

    if (-not $Spec.Available -or -not $Spec.SupportsCommand) {
        throw "$($Spec.Name) does not provide an available command-launch contract"
    }
    if ($Command.Count -eq 0) {
        throw "$($Spec.Name) benchmark command is empty"
    }
    $expectedHashProperty = $Spec.PSObject.Properties['BenchmarkExeSha256']
    if (
        $null -eq $expectedHashProperty -or
        -not [string]$expectedHashProperty.Value
    ) {
        throw "$($Spec.Name) is missing its benchmark executable hash"
    }
    $launchHash = [string]$expectedHashProperty.Value
    $preexistingOwners = @(
        Get-Process -Name $Spec.WindowProcessNames -ErrorAction SilentlyContinue |
            Where-Object { $PreexistingPids.Contains($_.Id) }
    )
    if ($preexistingOwners.Count -gt 0) {
        throw (
            "$($Spec.Name) is already running in pid(s) " +
            "$($preexistingOwners.Id -join ', '); close it before benchmarking"
        )
    }
    $effectiveCommand = $Command
    $commandWrapper = $null
    if ($Spec.CommandConfirmation) {
        if (-not $CommandWrapperDirectory) {
            throw "$($Spec.Name) requires a benchmark command-wrapper directory"
        }
        # Tabby's yargs parser consumes dash-prefixed child arguments instead of
        # retaining them in `run [command...]`. Its supported cmd.exe profile
        # calls a one-use ASCII wrapper whose encoded launcher starts the real
        # executable directly. The open read lock prevents wrapper mutation.
        $commandWrapper = New-KettlePerfCommandWrapper `
            -OutputDirectory $CommandWrapperDirectory -Command $Command `
            -PowerShellExecutable $Spec.CommandPowerShell
        if (
            -not $Spec.CommandShell -or
            -not [IO.Path]::IsPathRooted($Spec.CommandShell) -or
            -not (Test-Path -LiteralPath $Spec.CommandShell -PathType Leaf)
        ) {
            throw "$($Spec.Name) command shell is not a verified absolute executable"
        }
        $effectiveCommand = @(
            $Spec.CommandShell, '/d', '/q', '/v:off', '/s', '/c', 'call',
            $commandWrapper.Path
        )
    }
    $launchArgs = @($Spec.CommandPrefix) + $effectiveCommand
    $window = [IntPtr]::Zero
    $windowPid = 0
    $executableLease = $null
    $launchEnvironment = @{}
    $environmentProperty = $Spec.PSObject.Properties['Environment']
    if ($environmentProperty -and $null -ne $environmentProperty.Value) {
        if ($environmentProperty.Value -isnot [System.Collections.IDictionary]) {
            throw "$($Spec.Name) benchmark environment is not a dictionary"
        }
        $launchEnvironment = $environmentProperty.Value
    }
    try {
        $executableLease = Open-KettlePerfExecutableLease `
            -Executable $Spec.BenchmarkExe -ExpectedSha256 $launchHash
        $process = Start-KettlePerfProcess -FilePath $Spec.Exe `
            -ArgumentList $launchArgs -Environment $launchEnvironment
    } catch {
        Close-KettlePerfExecutableLease $executableLease
        if ($null -ne $commandWrapper) {
            Close-KettlePerfCommandWrapper $commandWrapper
        }
        throw
    }
    try {
        Confirm-KettlePerfCommand -Spec $Spec `
            -BeforeWindows $BeforeWindows -PreexistingPids $PreexistingPids `
            -RootPid $process.Id
        $excludedClasses = if ($Spec.CommandConfirmation) { @('#32770') } else { @() }
        $window = Wait-NewWindow -Before $BeforeWindows `
            -PreexistingPids $PreexistingPids -RootPid $process.Id `
            -ProcessNames $Spec.WindowProcessNames `
            -ExpectedExecutable $Spec.BenchmarkExe `
            -ExcludedClassNames $excludedClasses
        if ($window -eq [IntPtr]::Zero) {
            throw "$($Spec.Name) command window never appeared"
        }
        $windowPid = Get-WindowPid $window
        $windowExecutable = [KettlePerf.Native]::ProcessExecutablePath($windowPid)
        $windowHash = Get-KettlePerfExecutableSha256 $windowExecutable
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            $windowHash,
            $launchHash
        )) {
            throw "$($Spec.Name) window owner executable changed during launch"
        }
        $targetPid = 0
        $targetExecutable = ''
        if (-not $DeferTargetAttribution) {
            $targetPid = Wait-KettlePerfDescendant `
                -RootPid $windowPid `
                -Executable $Command[0] -PreexistingPids $PreexistingPids
            if ($null -eq $targetPid) {
                throw "$($Spec.Name) benchmark command did not start in its process tree"
            }
            $targetExecutable = (
                Get-CimInstance Win32_Process -Filter "ProcessId = $targetPid"
            ).ExecutablePath
        }
        return [pscustomobject]@{
            Process = $process
            Hwnd = $window
            WindowPid = $windowPid
            WindowExecutable = $windowExecutable
            WindowExecutableSha256 = $windowHash
            ExecutableLease = $executableLease
            CommandWrapper = $commandWrapper
            HelperBinaries = @($Spec.HelperBinaries)
            TargetPid = $targetPid
            TargetExecutable = $targetExecutable
            TargetAttributionDeferred = [bool]$DeferTargetAttribution
            ExpectedTargetExecutable = [string]$Command[0]
        }
    } catch {
        if ($window -and $window -ne [IntPtr]::Zero) {
            [void](Close-SpawnedTerminal `
                -Hwnd $window -ExpectedPid $windowPid `
                -PreexistingPids $PreexistingPids)
        }
        try {
            if (-not $process.HasExited) {
                Stop-Process -Id $process.Id -Force
            }
        } catch {
            Write-Verbose "failed command-launch cleanup raced process exit: $($_.Exception.Message)"
        }
        if ($null -ne $commandWrapper) {
            Close-KettlePerfCommandWrapper $commandWrapper
        }
        Close-KettlePerfExecutableLease $executableLease
        throw
    }
}

function Complete-KettlePerfTargetAttribution {
    param(
        [Parameter(Mandatory)]
        $Launch,
        [Parameter(Mandatory)]
        [System.Collections.Generic.HashSet[int]]$PreexistingPids,
        [Parameter(Mandatory)]
        [string]$TerminalName
    )

    if (-not [bool]$Launch.TargetAttributionDeferred) {
        if (
            [int]$Launch.TargetPid -le 0 -or
            -not [string]$Launch.TargetExecutable
        ) {
            throw "$TerminalName launch has incomplete target attribution"
        }
        return $Launch
    }
    $targetPid = Wait-KettlePerfDescendant `
        -RootPid ([int]$Launch.WindowPid) `
        -Executable ([string]$Launch.ExpectedTargetExecutable) `
        -PreexistingPids $PreexistingPids
    if ($null -eq $targetPid) {
        throw "$TerminalName benchmark command did not start in its process tree"
    }
    $targetExecutable = (
        Get-CimInstance Win32_Process -Filter "ProcessId = $targetPid"
    ).ExecutablePath
    if (-not $targetExecutable) {
        throw "$TerminalName benchmark command executable could not be verified"
    }
    $Launch.TargetPid = [int]$targetPid
    $Launch.TargetExecutable = [string]$targetExecutable
    $Launch.TargetAttributionDeferred = $false
    return $Launch
}

function Get-KettlePerfTargetScreen {
    param([string]$DeviceName = '')

    Add-Type -AssemblyName System.Windows.Forms
    $screen = if ($DeviceName) {
        @(
            [Windows.Forms.Screen]::AllScreens |
                Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        $_.DeviceName,
                        $DeviceName
                    )
                }
        ) | Select-Object -First 1
    } else {
        [Windows.Forms.Screen]::PrimaryScreen
    }
    if ($null -eq $screen) {
        throw "Windows reports no requested desktop screen: $DeviceName"
    }
    return $screen
}

function Set-WindowSize(
    [IntPtr]$h,
    [int]$Width,
    [int]$Height,
    [string]$TargetScreenDevice = ''
) {
    $screen = Get-KettlePerfTargetScreen $TargetScreenDevice
    $working = $screen.WorkingArea
    $margin = 16
    if (-not [KettlePerf.Native]::SetClientSize(
        $h,
        $Width,
        $Height,
        $working.X + $margin,
        $working.Y + $margin,
        $working.Right,
        $working.Bottom
    )) {
        throw "Could not set terminal client area to ${Width}x${Height} physical pixels"
    }
    $actualDevice = [KettlePerf.Native]::MonitorDeviceForWindow($h)
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $actualDevice,
        $screen.DeviceName
    )) {
        throw (
            "terminal landed on $actualDevice instead of target " +
            $screen.DeviceName
        )
    }
}

function Wait-KettlePerfWindowReady {
    param(
        [Parameter(Mandatory)] [IntPtr]$Hwnd,
        [Parameter(Mandatory)] [int]$Width,
        [Parameter(Mandatory)] [int]$Height,
        [string]$TargetScreenDevice = '',
        [int]$TimeoutMs = 5000
    )

    Set-WindowSize $Hwnd $Width $Height $TargetScreenDevice
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt $TimeoutMs) {
        $captureWidth = 0
        $captureHeight = 0
        $capture = [KettlePerf.Native]::CaptureWindow(
            $Hwnd,
            [ref]$captureWidth,
            [ref]$captureHeight
        )
        if (
            $null -ne $capture -and
            $captureWidth -eq $Width -and
            $captureHeight -eq $Height
        ) {
            return $true
        }
        Start-Sleep -Milliseconds 15
    }
    return $false
}

function Join-KettlePerfArguments {
    param([string[]]$Arguments)

    return [KettlePerf.Native]::JoinArguments($Arguments)
}

function Start-KettlePerfProcess {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This is the harness process-launch primitive.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$FilePath,
        [string[]]$ArgumentList = @(),
        [System.Collections.IDictionary]$Environment = @{}
    )

    if (-not [IO.Path]::IsPathRooted($FilePath)) {
        $FilePath = Get-Command $FilePath -CommandType Application `
            -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    }
    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
        throw "Performance executable not found: $FilePath"
    }
    $resolvedFile = (Resolve-Path -LiteralPath $FilePath).Path
    $arguments = Join-KettlePerfArguments $ArgumentList
    if (
        $resolvedFile.Length -gt 32767 -or
        $arguments.Length -gt 32767 -or
        $resolvedFile.Contains([char]0) -or
        $arguments.Contains([char]0)
    ) {
        throw 'Performance process path or arguments exceed the Windows process contract'
    }

    $startInfo = New-Object Diagnostics.ProcessStartInfo
    $startInfo.FileName = $resolvedFile
    $startInfo.Arguments = $arguments
    $startInfo.UseShellExecute = $false
    foreach ($entry in $Environment.GetEnumerator()) {
        $name = [string]$entry.Key
        $value = [string]$entry.Value
        if (
            $name -notmatch '^[A-Za-z_][A-Za-z0-9_]{0,127}$' -or
            $value.Length -gt 32767 -or
            $value.Contains([char]0)
        ) {
            throw "Invalid isolated benchmark environment entry: $name"
        }
        $startInfo.EnvironmentVariables[$name] = $value
    }
    $process = [Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw "Windows did not start the performance executable: $resolvedFile"
    }
    return $process
}

# Close a benchmark-spawned terminal WITHOUT risking someone else's session.
# Windows Terminal can route a `wt.exe` spawn into an ALREADY-RUNNING
# WindowsTerminal.exe (windowingBehavior = useExisting) - the new window then
# belongs to a pre-existing pid, and `Stop-Process` on it would take down the
# user's live terminal (possibly the very session driving this harness). Rule:
# only kill pids born AFTER the spawn; for a shared pid, WM_CLOSE the single
# window we created and leave the process alone.
function Close-SpawnedTerminal {
    param(
        [Parameter(Mandatory)] [IntPtr]$Hwnd,
        [Parameter(Mandatory)] [int]$ExpectedPid,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[int]]$PreexistingPids
    )
    $winPid = Get-WindowPid $Hwnd
    if (-not $winPid) { return $true }   # window already gone - nothing to close
    if ($winPid -ne $ExpectedPid) {
        Write-Warning (
            "window owner changed from expected pid $ExpectedPid to $winPid; " +
            'refusing benchmark cleanup'
        )
        return $false
    }
    if (-not $PreexistingPids.Contains($winPid)) {
        [void][KettlePerf.Native]::PostMessage(
            $Hwnd,
            [KettlePerf.Native]::WM_CLOSE,
            [IntPtr]::Zero,
            [IntPtr]::Zero
        )
        $deadline = (Get-Date).AddSeconds(2)
        while (
            (Get-Date) -lt $deadline -and
            (Get-WindowPid $Hwnd) -eq $ExpectedPid
        ) {
            Start-Sleep -Milliseconds 50
        }
        if ((Get-WindowPid $Hwnd) -ne $ExpectedPid) {
            return $true
        }
        try {
            Stop-Process -Id $winPid -Force
        } catch {
            Write-Verbose "benchmark process $winPid already exited: $($_.Exception.Message)"
        }
        return $true   # process was ours; tree stats were valid
    }
    Write-Warning "window pid $winPid pre-existed the spawn (shared-instance terminal) - closing the window only"
    [void][KettlePerf.Native]::PostMessage($Hwnd, [KettlePerf.Native]::WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    return $false      # shared process; per-process stats are NOT attributable
}

function Get-PidSet {
    $set = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($p in Get-Process) { [void]$set.Add($p.Id) }
    , $set
}

function Get-ProcessTreeStats {
    param(
        [Parameter(Mandatory = $true)]
        [int]$RootPid,
        [int[]]$ExcludeRootPids = @()
    )

    # Sum CPU seconds + working set across the root process and its descendants.
    # A controlled workload can be excluded by PID while retaining the
    # terminal, ConPTY host, and their other descendants.
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
    $children = @{}
    foreach ($p in $all) {
        if (-not $children.ContainsKey([int]$p.ParentProcessId)) { $children[[int]$p.ParentProcessId] = @() }
        $children[[int]$p.ParentProcessId] += [int]$p.ProcessId
    }
    $tree = [System.Collections.Generic.List[int]]::new()
    $queue = [System.Collections.Generic.Queue[int]]::new()
    $queue.Enqueue($RootPid)
    while ($queue.Count -gt 0) {
        $procId = $queue.Dequeue()
        $tree.Add($procId)
        if ($children.ContainsKey($procId)) { foreach ($c in $children[$procId]) { $queue.Enqueue($c) } }
    }
    $excluded = [Collections.Generic.HashSet[int]]::new()
    $excludeQueue = [Collections.Generic.Queue[int]]::new()
    foreach ($excludeRootPid in $ExcludeRootPids) {
        if ($excludeRootPid -gt 0) {
            $excludeQueue.Enqueue($excludeRootPid)
        }
    }
    while ($excludeQueue.Count -gt 0) {
        $excludedPid = $excludeQueue.Dequeue()
        if (-not $excluded.Add($excludedPid)) {
            continue
        }
        if ($children.ContainsKey($excludedPid)) {
            foreach ($childPid in $children[$excludedPid]) {
                $excludeQueue.Enqueue($childPid)
            }
        }
    }

    $cpu = 0.0
    $ws = 0L
    $names = @()
    $includedPids = [Collections.Generic.List[int]]::new()
    $processSamples = [Collections.Generic.List[object]]::new()
    $samplingMisses = [Collections.Generic.List[int]]::new()
    foreach ($procId in $tree) {
        if ($excluded.Contains($procId)) {
            continue
        }
        try {
            $p = Get-Process -Id $procId -ErrorAction Stop
            $cpu += $p.CPU
            $ws += $p.WorkingSet64
            $names += $p.ProcessName
            $includedPids.Add($procId)
            $processSamples.Add([pscustomobject][ordered]@{
                pid = $procId
                process_name = [string]$p.ProcessName
                start_time_utc_ticks = (
                    $p.StartTime.ToUniversalTime().Ticks
                )
                cpu_seconds = [double]$p.CPU
                working_set_bytes = [int64]$p.WorkingSet64
            })
        } catch {
            $samplingMisses.Add($procId)
            Write-Verbose "process $procId exited during tree sampling: $($_.Exception.Message)"
        }
    }
    [pscustomobject]@{
        CpuSeconds = $cpu
        WorkingSetMB = [Math]::Round($ws / 1MB, 1)
        Pids = $includedPids
        Names = $names
        ExcludedPids = @($excluded)
        ProcessSamples = [object[]]@(
            $processSamples |
                Sort-Object -Property pid
        )
        SamplingMisses = [int[]]$samplingMisses.ToArray()
    }
}
