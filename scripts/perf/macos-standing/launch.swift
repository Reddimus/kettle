// launch OUT_JSON STAMP_FILE TIMEOUT_SECONDS -- ARGV...
//
// Spawns a terminal and records, on the CLOCK_UPTIME_RAW clock that `stamp`
// uses: when the window server first lists an on-screen window owned by the
// spawned pid, when the child inside the terminal started (read from
// STAMP_FILE), and when the terminal exited. The terminal's pid is written to
// STAMP_FILE.pid for helpers that sample it while it runs, and removed as soon
// as the terminal is reaped, since the pid may then be reused. SIGTERM to this
// probe stops the terminal: only the probe can signal it safely, because it
// has not reaped it yet. The probe also stops the terminal once the process
// that started it is gone (its parent changes), so a harness killed mid-round
// leaves no terminal running on. A terminal still running at the timeout, or
// 10 s after a stop, gets SIGTERM, then SIGKILL. The result also carries the spawn
// time itself (started_ns), so other stamps on the same clock can be placed
// against it, and the machine's thermal state and Low Power Mode.
import CoreGraphics
import Foundation

func now() -> UInt64 { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }

// Set before the spawn so an early stop is never lost; the child resets it to
// the default on exec.
nonisolated(unsafe) var stopRequested: sig_atomic_t = 0
signal(SIGTERM) { _ in stopRequested = 1 }

let args = CommandLine.arguments
guard args.count > 5, args[4] == "--", let timeout = Double(args[3]) else {
    FileHandle.standardError.write("usage: launch OUT_JSON STAMP_FILE TIMEOUT -- ARGV...\n".data(using: .utf8)!)
    exit(2)
}
let outPath = args[1], stampPath = args[2]
let argv = Array(args[5...])
try? FileManager.default.removeItem(atPath: stampPath)

let parent = getppid()
// Already orphaned: the process that started the probe is gone, and launchd
// adopted it before this line. Nothing may be spawned for it.
guard parent != 1 else { exit(3) }
var pid: pid_t = 0
let cargs = argv.map { strdup($0) } + [nil]
let started = now()
let spawned = posix_spawn(&pid, argv[0], nil, nil, cargs, environ)
guard spawned == 0 else {
    FileHandle.standardError.write("spawn failed: \(spawned)\n".data(using: .utf8)!)
    exit(2)
}
try? "\(pid)".write(toFile: stampPath + ".pid", atomically: true, encoding: .utf8)

let contextPath = ProcessInfo.processInfo.environment["KETTLE_HC_LAUNCH_CONTEXT"]
func context(_ window: UInt32) {
    guard let contextPath else { return }
    let data = try! JSONSerialization.data(withJSONObject: ["pid": pid, "started_ns": started,
                                                           "window_id": window,
        "native_display": ["width_pt": CGDisplayCopyDisplayMode(CGMainDisplayID())?.width ?? 0,
                           "height_pt": CGDisplayCopyDisplayMode(CGMainDisplayID())?.height ?? 0,
                           "pixel_width": CGDisplayCopyDisplayMode(CGMainDisplayID())?.pixelWidth ?? 0,
                           "pixel_height": CGDisplayCopyDisplayMode(CGMainDisplayID())?.pixelHeight ?? 0,
                           "refresh_hz": CGDisplayCopyDisplayMode(CGMainDisplayID())?.refreshRate ?? 0]], options: [.sortedKeys])
    try? data.write(to: URL(fileURLWithPath: contextPath), options: .atomic)
}
// The launch helper owns both processes. Reap the sampler before releasing
// the target PID, including on target exit, parent loss, and cancellation.
var observerPID: pid_t = 0
var observerStarted = false
func observerPath(_ suffix: String) -> String? { contextPath.map { $0 + ".observer-" + suffix } }
func stopObserver() {
    guard observerPID > 0 else { return }
    kill(observerPID, SIGTERM)
    var state: Int32 = 0
    let until = now() + 2_000_000_000
    while now() < until {
        if waitpid(observerPID, &state, WNOHANG) == observerPID { observerPID = 0; break }
        usleep(2_000)
    }
    if observerPID > 0 { kill(observerPID, SIGKILL); waitpid(observerPID, &state, 0); observerPID = 0 }
    if let path = observerPath("reaped") { try? "reaped".write(toFile: path, atomically: true, encoding: .utf8) }
}
func serviceObserver() {
    if observerPID > 0 {
        if let path = observerPath("stop"), FileManager.default.fileExists(atPath: path) { stopObserver(); return }
        var state: Int32 = 0
        if waitpid(observerPID, &state, WNOHANG) == observerPID {
            observerPID = 0
            if let path = observerPath("reaped") { try? "reaped".write(toFile: path, atomically: true, encoding: .utf8) }
        }
    }
    guard !observerStarted, let request = observerPath("request"),
          let data = try? Data(contentsOf: URL(fileURLWithPath: request)),
          let args = try? JSONSerialization.jsonObject(with: data) as? [String], (args.count == 7 || args.count == 8),
          args[1] == String(pid), getppid() == parent, stopRequested == 0 else { return }
    observerStarted = true
    let cargs = args.map { strdup($0) } + [nil]
    let spawned = posix_spawn(&observerPID, args[0], nil, nil, cargs, environ)
    for arg in cargs { free(arg) }
    if spawned != 0 {
        observerPID = 0
        if let path = observerPath("reaped") { try? "spawn-failed".write(toFile: path, atomically: true, encoding: .utf8) }
    } else if let path = observerPath("started") {
        try? "\(observerPID)".write(toFile: path, atomically: true, encoding: .utf8)
    }
}
var windowAt: UInt64?, stampSeenAt: UInt64?, exitedAt: UInt64?
var status: Int32 = 0
var deadline = started + UInt64(timeout * 1e9)
var stopped = false
while now() < deadline {
    serviceObserver()
    if (stopRequested != 0 || getppid() != parent) && !stopped {
        // A terminal that already exited quit on its own: leave that to the
        // exit check below, so the result never calls it stopped.
        var early = siginfo_t()
        if !(waitid(P_PID, id_t(pid), &early, WEXITED | WNOHANG | WNOWAIT) == 0 && early.si_pid == pid) {
            stopped = true
            stopObserver()
            kill(pid, SIGTERM)
            deadline = min(deadline, now() + 10_000_000_000)
        }
    }
    if windowAt == nil,
       let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] {
        for window in windows where (window[kCGWindowOwnerPID as String] as? Int32) == pid
            && (window[kCGWindowLayer as String] as? Int) == 0 {
            // A normal window, not a small dialog such as a config-error panel.
            if let bounds = window[kCGWindowBounds as String] as? [String: Double],
               (bounds["Width"] ?? 0) > 300 {
                windowAt = now()
                context(window[kCGWindowNumber as String] as? UInt32 ?? 0)
                break
            }
        }
    }
    if stampSeenAt == nil, FileManager.default.fileExists(atPath: stampPath) { stampSeenAt = now() }
    var observedExit = false
    if contextPath != nil {
        // WNOWAIT preserves the zombie and its PID while Python drains its
        // observer. Ordinary launches retain their existing waitpid path.
        var childInfo = siginfo_t()
        observedExit = waitid(P_PID, id_t(pid), &childInfo, WEXITED | WNOHANG | WNOWAIT) == 0 && childInfo.si_pid == pid
        if observedExit { stopObserver(); waitpid(pid, &status, 0) }
    } else {
        observedExit = waitpid(pid, &status, WNOHANG) == pid
    }
    if observedExit {
        exitedAt = now()
        try? FileManager.default.removeItem(atPath: stampPath + ".pid")
        break
    }
    usleep(windowAt == nil ? 500 : 2_000)
}
let killed = exitedAt == nil
if killed {
    stopObserver()
    kill(pid, SIGTERM)
    usleep(1_000_000)
    kill(pid, SIGKILL)
    waitpid(pid, &status, 0)
    try? FileManager.default.removeItem(atPath: stampPath + ".pid")
}

var childAt: UInt64?, cols = 0, rows = 0
if let text = try? String(contentsOfFile: stampPath, encoding: .utf8) {
    let fields = text.split(separator: " ").map { String($0).trimmingCharacters(in: .whitespacesAndNewlines) }
    if fields.count == 3 {
        childAt = UInt64(fields[0])
        cols = Int(fields[1]) ?? 0
        rows = Int(fields[2]) ?? 0
    }
}
func milliseconds(_ at: UInt64?) -> Any { at.map { Double($0 - started) / 1e6 } ?? NSNull() }
let result: [String: Any] = [
    "window_ms": milliseconds(windowAt), "child_ms": milliseconds(childAt),
    "exit_ms": milliseconds(exitedAt), "killed": killed, "stopped": stopped, "cols": cols, "rows": rows,
    "started_ns": started, "thermal_state": ProcessInfo.processInfo.thermalState.rawValue,
    "low_power": ProcessInfo.processInfo.isLowPowerModeEnabled,
]
let data = try! JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
FileManager.default.createFile(atPath: outPath, contents: data)
