# Shared Win32 helpers for the perf harness (dot-source this).
# PrintWindow with PW_RENDERFULLCONTENT is used for captures because GDI screen
# copies cannot see flip-model (DXGI) swapchains like kettle's; PrintWindow
# renders at PHYSICAL pixels regardless of the caller's DPI virtualization.
$ErrorActionPreference = 'Stop'

if (-not ('KettlePerf.Native' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace KettlePerf {
public static class Native {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr hWnd, IntPtr hdc);
    [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr hdc);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr hdc, int w, int h);
    [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);
    [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr obj);
    [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr hdc);
    [DllImport("gdi32.dll")] public static extern int GetDIBits(IntPtr hdc, IntPtr bmp, uint start, uint lines, byte[] bits, ref BITMAPINFO bi, uint usage);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
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
    public const uint PW_RENDERFULLCONTENT = 0x2;
    public const uint KEYEVENTF_KEYUP = 0x2, KEYEVENTF_UNICODE = 0x4;
    public const uint WM_CLOSE = 0x0010;

    public static List<IntPtr> VisibleTopWindows() {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            if (IsWindowVisible(h) && GetWindowTextLength(h) > 0) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    public static void SendChar(char c) {
        var inputs = new INPUT[2];
        inputs[0].type = 1; inputs[0].U.ki = new KEYBDINPUT { wScan = c, dwFlags = KEYEVENTF_UNICODE };
        inputs[1].type = 1; inputs[1].U.ki = new KEYBDINPUT { wScan = c, dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP };
        SendInput(2, inputs, Marshal.SizeOf<INPUT>());
    }

    public static void SendVk(ushort vk) {
        var inputs = new INPUT[2];
        inputs[0].type = 1; inputs[0].U.ki = new KEYBDINPUT { wVk = vk };
        inputs[1].type = 1; inputs[1].U.ki = new KEYBDINPUT { wVk = vk, dwFlags = KEYEVENTF_KEYUP };
        SendInput(2, inputs, Marshal.SizeOf<INPUT>());
    }

    // Captures the window's client-area pixels (BGRA bottom-up) via PrintWindow.
    public static byte[] CaptureWindow(IntPtr hWnd, out int w, out int h) {
        RECT rc; GetClientRect(hWnd, out rc);
        w = rc.Right - rc.Left; h = rc.Bottom - rc.Top;
        if (w <= 0 || h <= 0) return null;
        IntPtr screen = GetDC(IntPtr.Zero);
        IntPtr mem = CreateCompatibleDC(screen);
        IntPtr bmp = CreateCompatibleBitmap(screen, w, h);
        IntPtr old = SelectObject(mem, bmp);
        bool ok = PrintWindow(hWnd, mem, PW_RENDERFULLCONTENT);
        byte[] bits = null;
        if (ok) {
            var bi = new BITMAPINFO();
            bi.bmiHeader.biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>();
            bi.bmiHeader.biWidth = w; bi.bmiHeader.biHeight = h;
            bi.bmiHeader.biPlanes = 1; bi.bmiHeader.biBitCount = 32;
            bits = new byte[w * h * 4];
            GetDIBits(mem, bmp, 0, (uint)h, bits, ref bi, 0);
        }
        SelectObject(mem, old); DeleteObject(bmp); DeleteDC(mem); ReleaseDC(IntPtr.Zero, screen);
        return bits;
    }
}
}
'@
}

function Get-VisibleWindowSet {
    $set = [System.Collections.Generic.HashSet[IntPtr]]::new()
    foreach ($h in [KettlePerf.Native]::VisibleTopWindows()) { [void]$set.Add($h) }
    , $set
}

# Spawns a process and returns the first NEW visible top-level window that appears
# (handles launcher indirection like wt.exe handing off to WindowsTerminal.exe).
function Wait-NewWindow {
    param(
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[IntPtr]]$Before,
        [int]$TimeoutMs = 20000
    )
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        foreach ($h in [KettlePerf.Native]::VisibleTopWindows()) {
            if (-not $Before.Contains($h)) { return $h }
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

function Get-WindowPid([IntPtr]$h) {
    $procId = 0u
    [void][KettlePerf.Native]::GetWindowThreadProcessId($h, [ref]$procId)
    [int]$procId
}

function Set-WindowSize([IntPtr]$h, [int]$Width, [int]$Height) {
    [void][KettlePerf.Native]::SetWindowPos($h, [IntPtr]::Zero, 40, 40, $Width, $Height,
        [KettlePerf.Native]::SWP_NOZORDER -bor [KettlePerf.Native]::SWP_NOACTIVATE)
}

# Close a benchmark-spawned terminal WITHOUT risking someone else's session.
# Windows Terminal can route a `wt.exe` spawn into an ALREADY-RUNNING
# WindowsTerminal.exe (windowingBehavior = useExisting) — the new window then
# belongs to a pre-existing pid, and `Stop-Process` on it would take down the
# user's live terminal (possibly the very session driving this harness). Rule:
# only kill pids born AFTER the spawn; for a shared pid, WM_CLOSE the single
# window we created and leave the process alone.
function Close-SpawnedTerminal {
    param(
        [Parameter(Mandatory)] [IntPtr]$Hwnd,
        [Parameter(Mandatory)] [System.Collections.Generic.HashSet[int]]$PreexistingPids
    )
    $winPid = Get-WindowPid $Hwnd
    if (-not $winPid) { return $true }   # window already gone — nothing to close
    if (-not $PreexistingPids.Contains($winPid)) {
        try { Stop-Process -Id $winPid -Force } catch {}
        return $true   # process was ours; tree stats were valid
    }
    Write-Warning "window pid $winPid pre-existed the spawn (shared-instance terminal) — closing the window only"
    [void][KettlePerf.Native]::PostMessage($Hwnd, [KettlePerf.Native]::WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    return $false      # shared process; per-process stats are NOT attributable
}

function Get-PidSet {
    $set = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($p in Get-Process) { [void]$set.Add($p.Id) }
    , $set
}

function Get-ProcessTreeStats([int]$RootPid) {
    # Sum CPU seconds + working set across the root process and its descendants
    # (terminals host ConPTY/shell children that belong in the measurement).
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
    $cpu = 0.0; $ws = 0L; $names = @()
    foreach ($procId in $tree) {
        try {
            $p = Get-Process -Id $procId -ErrorAction Stop
            $cpu += $p.CPU; $ws += $p.WorkingSet64; $names += $p.ProcessName
        } catch {}
    }
    [pscustomobject]@{ CpuSeconds = $cpu; WorkingSetMB = [Math]::Round($ws / 1MB, 1); Pids = $tree; Names = $names }
}
