// KettleLatencyProbe: keystroke-to-screen latency for one terminal window.
//
//   latency-probe --check          exit 0 if Screen Recording and event posting
//                                  are granted, 3 if not; never prompts
//   latency-probe --request        ask macOS for both grants (--latency-check)
//   latency-probe --self-test      the pure logic on synthetic frames; no TCC
//   latency-probe --pid P --out F --keys N --warmup W [--gap-ms 100:300]
//                 [--censor-ms 500] [--seed S] [--inject hid|pid]
//
// Run mode measures one window whose payload (keyblock) toggles a 16x4-cell
// reverse-video block on every byte it reads. It calibrates the block from six
// guarded toggles, then streams only that rectangle of the DISPLAY through
// ScreenCaptureKit, so the window server's compositing (translucency, blur) is
// inside the measurement. For each key it records t_post (mach time just
// before CGEventPost) and the displayTime of the first complete frame on
// which at least 95 % of the block's pixels have flipped. displayTime is when
// the window server displayed the frame, not when light left the panel.
//
// Before every key the target must be frontmost (NSWorkspace and the top
// on-screen window both belong to it) and no input may have arrived from
// anyone else since the previous key. If focus changes during a sample the
// whole run aborts: a key must never reach another window. Only the letter j
// is ever posted, never Return or a modifier.
import AppKit
import CoreMedia
import CoreVideo
import CryptoKit
import Foundation
import QuartzCore
import ScreenCaptureKit

// MARK: - Time

let timebase: mach_timebase_info_data_t = {
    var info = mach_timebase_info_data_t()
    mach_timebase_info(&info)
    return info
}()

func machToNs(_ ticks: UInt64) -> UInt64 {
    UInt64((Double(ticks) * Double(timebase.numer) / Double(timebase.denom)).rounded())
}

func nowNs() -> UInt64 { machToNs(mach_absolute_time()) }

// Bracket the Mach epoch in the observer clock without changing timing samples.
func typingClockCheck() -> [String: UInt64] {
    let before = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    let mach = nowNs()
    let after = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    return ["raw_before_ns": before, "mach_ns": mach, "raw_after_ns": after]
}

func typingRawNs(_ mach: UInt64, _ check: [String: UInt64]) -> UInt64 {
    let midpoint = check["raw_before_ns"]! + (check["raw_after_ns"]! - check["raw_before_ns"]!) / 2
    return UInt64(Int64(mach) + Int64(midpoint) - Int64(check["mach_ns"]!))
}

// MARK: - Pure logic (covered by --self-test)

/// Rec. 709 luma of an 8-bit BGRA pixel.
func luma(_ b: UInt8, _ g: UInt8, _ r: UInt8) -> Double {
    0.0722 * Double(b) + 0.7152 * Double(g) + 0.2126 * Double(r)
}

struct Frame {
    var width: Int
    var height: Int
    var bytesPerRow: Int
    var pixels: [UInt8]  // BGRA

    func luma(x: Int, y: Int) -> Double {
        let i = y * bytesPerRow + x * 4
        return probeLuma(pixels[i], pixels[i + 1], pixels[i + 2])
    }
}

func probeLuma(_ b: UInt8, _ g: UInt8, _ r: UInt8) -> Double { luma(b, g, r) }

struct PixelRect: Equatable {
    var x: Int, y: Int, width: Int, height: Int

    func inset(fraction: Double) -> PixelRect {
        let dx = Int(Double(width) * fraction), dy = Int(Double(height) * fraction)
        return PixelRect(x: x + dx, y: y + dy, width: max(1, width - 2 * dx), height: max(1, height - 2 * dy))
    }
}

/// Mean luma of `rect` and the share of its pixels above `threshold`.
func regionStats(_ frame: Frame, _ rect: PixelRect, threshold: Double) -> (mean: Double, above: Double) {
    var sum = 0.0, above = 0
    for y in rect.y..<(rect.y + rect.height) {
        for x in rect.x..<(rect.x + rect.width) {
            let l = frame.luma(x: x, y: y)
            sum += l
            if l > threshold { above += 1 }
        }
    }
    let n = Double(rect.width * rect.height)
    return (sum / n, Double(above) / n)
}

enum CalibrationError: Error, Equatable, CustomStringConvertible {
    case noChange, notOneRectangle, badShape, contrast

    var description: String {
        switch self {
        case .noChange: return "calibration: the block never changed"
        case .notOneRectangle: return "calibration: the change is not one rectangle"
        case .badShape: return "calibration: the change does not have the block's shape"
        case .contrast: return "calibration: contrast"
        }
    }
}

/// The bounding box of pixels whose luma changed by more than 32 between two
/// frames. It must be one filled rectangle with the block's proportions.
func changedBox(_ before: Frame, _ after: Frame) throws -> PixelRect {
    var minX = Int.max, minY = Int.max, maxX = -1, maxY = -1, changed = 0
    for y in 0..<before.height {
        for x in 0..<before.width where abs(before.luma(x: x, y: y) - after.luma(x: x, y: y)) > 32 {
            minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
            changed += 1
        }
    }
    guard maxX >= 0 else { throw CalibrationError.noChange }
    let box = PixelRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1)
    guard Double(changed) >= 0.9 * Double(box.width * box.height) else { throw CalibrationError.notOneRectangle }
    let aspect = Double(box.width) / Double(box.height)
    guard aspect >= 1.2 && aspect <= 3.5 else { throw CalibrationError.badShape }
    return box
}

struct Calibration {
    var on: Double
    var off: Double
    var threshold: Double { (on + off) / 2 }
}

/// The two states' median lumas from several samples of each. The states must
/// differ by at least 40 and each must vary by at most a quarter of that.
func calibrate(on: [Double], off: [Double]) throws -> Calibration {
    func median(_ v: [Double]) -> Double {
        let s = v.sorted()
        return s.count % 2 == 1 ? s[s.count / 2] : (s[s.count / 2 - 1] + s[s.count / 2]) / 2
    }
    let lOn = median(on), lOff = median(off)
    let gap = abs(lOn - lOff)
    let spread = max((on.max() ?? 0) - (on.min() ?? 0), (off.max() ?? 0) - (off.min() ?? 0))
    guard gap >= 40, spread <= gap / 4 else { throw CalibrationError.contrast }
    return Calibration(on: lOn, off: lOff)
}

/// The share of region pixels on the side of the threshold that the target
/// state has.
func flippedShare(above: Double, targetOn: Bool, calibration: Calibration) -> Double {
    (calibration.on > calibration.off) == targetOn ? above : 1 - above
}

struct Observed {
    var displayNs: UInt64
    var arrivalNs: UInt64
    var share: Double  // flipped share toward the target state
}

struct Sample: Equatable {
    var displayNs: UInt64?
    var arrivalNs: UInt64?
    var mixedFrames: Int
    var censored: Bool
}

/// The first frame displayed after the post on which at least 95 % of the
/// block has flipped; frames between 5 % and 95 % count as mixed. Nothing by
/// the censor limit means the sample is censored.
func pickSample(_ frames: [Observed], postNs: UInt64, censorNs: UInt64) -> Sample {
    var mixed = 0
    for frame in frames where frame.displayNs > postNs {
        if frame.displayNs - postNs > censorNs { break }
        if frame.share >= 0.95 {
            return Sample(displayNs: frame.displayNs, arrivalNs: frame.arrivalNs, mixedFrames: mixed, censored: false)
        }
        if frame.share > 0.05 { mixed += 1 }
    }
    return Sample(displayNs: nil, arrivalNs: nil, mixedFrames: mixed, censored: true)
}

/// A key may be posted only when the target owns both the frontmost
/// application and the top on-screen window.
/// A key may be posted only when the target app is frontmost and the topmost
/// ordinary window is the measured window itself: one terminal process can
/// own several windows, and the key must reach the one being captured.
func mayPost(frontmost: pid_t?, topWindow: TopWindow?, target: pid_t, window: CGWindowID) -> Bool {
    frontmost == target && topWindow?.owner == target && topWindow?.number == window
}

struct TopWindow {
    var owner: pid_t
    var number: CGWindowID
}

struct WindowInfo {
    var owner: pid_t
    var layer: Int
    var alpha: Double
    var bounds: CGRect
    var number: CGWindowID = 0
}

/// The first window drawn over `rect` in front of the target's own window, at
/// any layer: a system alert or notification over the block would be measured
/// as the terminal. Windows are in front-to-back order.
/// The first visible window in front of the measured window that overlaps
/// `rect` (front to back, as the window list orders them), even one of the
/// target's own; a layer of -1 means the measured window is not on screen.
func obscuring(_ windows: [WindowInfo], window: CGWindowID, rect: CGRect) -> WindowInfo? {
    for info in windows {
        if info.number == window { return nil }
        if info.alpha > 0 && info.bounds.intersects(rect) { return info }
    }
    return WindowInfo(owner: 0, layer: -1, alpha: 0, bounds: .zero)
}

func coverReason(_ cover: WindowInfo) -> String {
    cover.layer < 0 ? "the measured window is not on screen"
        : "a window (pid \(cover.owner), layer \(cover.layer)) covers the block"
}

/// Input from anyone else since our last post: the most recent key or mouse
/// event is newer than our own post, beyond a small tolerance.
func foreignInput(secondsSinceKey: Double, secondsSinceMouse: Double, secondsSincePost: Double,
                  secondsSinceStart: Double) -> Bool {
    secondsSinceKey + 0.02 < secondsSincePost || secondsSinceMouse + 0.02 < secondsSinceStart
}

/// Spread of detected display times against the vsync grid, in ns: the width
/// of the smallest arc holding every phase.
func phaseSpread(_ times: [UInt64], vsyncNs: UInt64, periodNs: UInt64) -> UInt64 {
    guard periodNs > 0, !times.isEmpty else { return 0 }
    let phases = times.map { t -> UInt64 in
        let d = t >= vsyncNs ? t - vsyncNs : periodNs - ((vsyncNs - t) % periodNs)
        return d % periodNs
    }.sorted()
    var largestGap = phases.first! + periodNs - phases.last!
    for (a, b) in zip(phases, phases.dropFirst()) { largestGap = max(largestGap, b - a) }
    return periodNs - largestGap
}

// MARK: - Self-test

func selfTest() -> Int32 {
    var failures: [String] = []
    func check(_ ok: Bool, _ what: String) { if !ok { failures.append(what) } }

    func solid(_ w: Int, _ h: Int, _ v: UInt8) -> Frame {
        Frame(width: w, height: h, bytesPerRow: w * 4, pixels: [UInt8](repeating: v, count: w * h * 4))
    }
    func paint(_ f: inout Frame, _ r: PixelRect, _ v: UInt8) {
        for y in r.y..<(r.y + r.height) {
            for x in r.x..<(r.x + r.width) {
                let i = y * f.bytesPerRow + x * 4
                f.pixels[i] = v; f.pixels[i + 1] = v; f.pixels[i + 2] = v
            }
        }
    }

    let off = solid(200, 100, 20)
    var on = off
    let block = PixelRect(x: 40, y: 30, width: 128, height: 68)
    paint(&on, PixelRect(x: 40, y: 30, width: 128, height: 68), 230)
    check((try? changedBox(off, on)) == block, "changedBox finds the block")
    check((try? changedBox(off, off)) == nil, "no change is an error")
    var two = off
    paint(&two, PixelRect(x: 0, y: 0, width: 10, height: 10), 230)
    paint(&two, PixelRect(x: 150, y: 80, width: 10, height: 10), 230)
    check((try? changedBox(off, two)) == nil, "two patches are not one rectangle")

    check((try? calibrate(on: [200, 201, 199], off: [20, 21, 20])) != nil, "clear contrast calibrates")
    check((try? calibrate(on: [60, 61], off: [40, 41])) == nil, "low contrast aborts")
    check((try? calibrate(on: [200, 120], off: [20, 21])) == nil, "an unstable state aborts")

    let cal = try! calibrate(on: [200], off: [20])
    let region = block.inset(fraction: 0.25)
    check(abs(regionStats(on, region, threshold: cal.threshold).above - 1) < 1e-9, "on region all above")
    check(regionStats(off, region, threshold: cal.threshold).above == 0, "off region all below")
    var half = off
    paint(&half, PixelRect(x: region.x, y: region.y, width: region.width / 2, height: region.height), 230)
    let share = flippedShare(above: regionStats(half, region, threshold: cal.threshold).above, targetOn: true,
                             calibration: cal)
    check(share > 0.4 && share < 0.6, "a half-drawn frame is mixed")
    check(flippedShare(above: 0, targetOn: false, calibration: cal) == 1, "off target counts pixels below")
    let dark = Calibration(on: 20, off: 200)
    check(flippedShare(above: 0, targetOn: true, calibration: dark) == 1, "light themes invert the sides")

    let frames = [Observed(displayNs: 900, arrivalNs: 950, share: 1),
                  Observed(displayNs: 1_016, arrivalNs: 1_020, share: 0.5),
                  Observed(displayNs: 1_033, arrivalNs: 1_040, share: 0.94),
                  Observed(displayNs: 1_050, arrivalNs: 1_060, share: 0.96)]
    let picked = pickSample(frames, postNs: 1_000, censorNs: 500)
    check(picked == Sample(displayNs: 1_050, arrivalNs: 1_060, mixedFrames: 2, censored: false),
          "the first frame after the post with 95 % flipped wins; earlier ones are ignored")
    check(pickSample(frames, postNs: 1_000, censorNs: 40).censored, "nothing within the limit is censored")

    check(typingRawNs(900, ["raw_before_ns": 100, "raw_after_ns": 300, "mach_ns": 500]) == 600,
          "typing clock conversion retains the measured Mach epoch")
    check(machToNs(0) == 0, "mach zero")
    check(machToNs(UInt64(timebase.denom)) == UInt64(timebase.numer), "mach ticks convert with the timebase")

    let measured = TopWindow(owner: 10, number: 5)
    check(mayPost(frontmost: 10, topWindow: measured, target: 10, window: 5), "post when the measured window is on top")
    check(!mayPost(frontmost: 11, topWindow: measured, target: 10, window: 5), "refuse another frontmost app")
    check(!mayPost(frontmost: 10, topWindow: TopWindow(owner: 12, number: 9), target: 10, window: 5),
          "refuse a window on top of the target")
    check(!mayPost(frontmost: 10, topWindow: TopWindow(owner: 10, number: 6), target: 10, window: 5),
          "refuse the target's other window on top")
    check(!mayPost(frontmost: nil, topWindow: measured, target: 10, window: 5), "refuse an unknown frontmost app")
    check(foreignInput(secondsSinceKey: 0.01, secondsSinceMouse: 99, secondsSincePost: 0.2, secondsSinceStart: 5),
          "a key newer than our post is foreign")
    check(!foreignInput(secondsSinceKey: 0.2, secondsSinceMouse: 99, secondsSincePost: 0.2, secondsSinceStart: 5),
          "our own key is not foreign")
    check(foreignInput(secondsSinceKey: 0.2, secondsSinceMouse: 1, secondsSincePost: 0.2, secondsSinceStart: 5),
          "a mouse move after the start is foreign")

    let blockRect = CGRect(x: 400, y: 300, width: 128, height: 68)
    let target = WindowInfo(owner: 10, layer: 0, alpha: 1, bounds: CGRect(x: 0, y: 0, width: 960, height: 660),
                            number: 5)
    let alert = WindowInfo(owner: 99, layer: 1000, alpha: 1, bounds: CGRect(x: 350, y: 80, width: 260, height: 370))
    let menuBar = WindowInfo(owner: 98, layer: 24, alpha: 1, bounds: CGRect(x: 0, y: 0, width: 1920, height: 24))
    let behind = WindowInfo(owner: 97, layer: 0, alpha: 1, bounds: CGRect(x: 0, y: 0, width: 1920, height: 1080))
    check(obscuring([menuBar, target, behind], window: 5, rect: blockRect) == nil,
          "windows behind the target and away from the block are fine")
    check(obscuring([alert, menuBar, target], window: 5, rect: blockRect)?.owner == 99,
          "an alert over the block obscures it")
    check(obscuring([WindowInfo(owner: 99, layer: 1000, alpha: 0, bounds: alert.bounds), target], window: 5,
                    rect: blockRect) == nil, "an invisible window does not")
    let sibling = WindowInfo(owner: 10, layer: 0, alpha: 1, bounds: target.bounds, number: 6)
    check(obscuring([sibling, target], window: 5, rect: blockRect)?.number == 6,
          "the target's own other window in front obscures it")
    check(obscuring([menuBar], window: 5, rect: blockRect)?.layer == -1,
          "a measured window that left the screen is reported")
    check(obscuring([menuBar, behind], window: 5, rect: blockRect) != nil,
          "without the measured window, whatever shows there blocks the key")
    let titlebar = CGPoint(x: 480, y: 12)
    check(windowAt(titlebar, [menuBar, target, behind])?.number == 0, "the menu bar is topmost at y 12")
    check(windowAt(CGPoint(x: 480, y: 40), [menuBar, target, behind])?.number == 5,
          "the measured window is topmost below the menu bar")
    check(windowAt(CGPoint(x: 480, y: 100), [alert, target])?.owner == 99, "an alert over the titlebar is topmost")

    let lease = NSTemporaryDirectory() + UUID().uuidString + ".probe-lease"
    let leaseFd = open(lease, O_CREAT | O_EXCL | O_RDWR, 0o600)
    check(leaseFd >= 0, "create scratch invocation lease")
    if leaseFd >= 0 {
        flock(leaseFd, LOCK_EX | LOCK_NB)
        check(invocationAlive(lease), "locked lease retains invocation ownership")
        flock(leaseFd, LOCK_UN)
        check(!invocationAlive(lease), "unlocked lease cancels after owner exit")
        close(leaseFd)
        unlink(lease)
        check(!invocationAlive(lease), "removed lease cancels")
    }
    check(invocationAlive(""), "standalone self-test needs no lease")
    check(!invocationAlive("/nonexistent-kettle-probe-lease"), "missing invocation lease cancels")

    check(phaseSpread([100, 16_767, 33_434], vsyncNs: 100, periodNs: 16_667) == 0, "on-grid times have no spread")
    check(phaseSpread([100, 16_867], vsyncNs: 100, periodNs: 16_667) == 100, "spread is the arc width")
    check(phaseSpread([16_600, 16_700], vsyncNs: 0, periodNs: 16_667) == 100, "spread wraps around the period")

    var blinkOptions = Options()
    blinkOptions.blinkCheck = true; blinkOptions.pid = 42; blinkOptions.blinkWindowID = 7
    var posts = 0, captures = 0
    probeMode(blinkOptions, blink: { captures += 1 }, injecting: { posts += 1 })
    check(posts == 0 && captures == 1, "blink dispatch captures without posting")
    blinkOptions.blinkCheck = false
    probeMode(blinkOptions, blink: { captures += 1 }, injecting: { posts += 1 })
    check(posts == 1 && captures == 1, "ordinary dispatch retains injecting mode")
    check(blinkWindowMatches(owner: 42, windowID: 7, layer: 0, width: 500, options: blinkOptions), "exact blink window")
    check(!blinkWindowMatches(owner: 42, windowID: 8, layer: 0, width: 500, options: blinkOptions), "another target window refused")
    check(!blinkWindowMatches(owner: 43, windowID: 7, layer: 0, width: 500, options: blinkOptions), "foreign owner refused")
    check(!blinkWindowMatches(owner: 42, windowID: 7, layer: 1, width: 500, options: blinkOptions), "layer refused")
    check(!blinkWindowMatches(owner: 42, windowID: 7, layer: 0, width: 300, options: blinkOptions), "small window refused")
    let complete = blinkFrame(previous: nil, raw: SCFrameStatus.complete.rawValue, hash: "pixels", arrival: 100)
    check(complete?.0 == 100 && complete?.1 == "pixels" && complete?.2 == "complete", "complete frame updates pixels")
    let idle = blinkFrame(previous: complete, raw: SCFrameStatus.idle.rawValue, hash: nil, arrival: 200)
    check(idle?.0 == 200 && idle?.1 == "pixels" && idle?.2 == "idle", "idle refreshes complete pixels")
    check(blinkFrame(previous: nil, raw: SCFrameStatus.idle.rawValue, hash: nil, arrival: 200) == nil, "idle cannot invent pixels")
    for raw in [SCFrameStatus.blank.rawValue, SCFrameStatus.stopped.rawValue, -1] {
        let rejected = blinkFrame(previous: complete, raw: raw, hash: "partial", arrival: 300)
        check(rejected?.0 == 100 && rejected?.1 == "pixels", "incomplete frame does not refresh pixels")
    }
    check(blinkFrame(previous: nil, raw: SCFrameStatus.complete.rawValue, hash: nil, arrival: 300) == nil, "complete needs image pixels")

    if failures.isEmpty {
        print("latency-probe self-test: ok")
        return 0
    }
    failures.forEach { FileHandle.standardError.write("self-test failed: \($0)\n".data(using: .utf8)!) }
    return 1
}

// A locked private file binds this LaunchServices process to the invoking
// harness lifetime. It requires no pid lookup or signal to a discovered app.
func invocationAlive(_ path: String) -> Bool {
    if path.isEmpty { return true }
    let fd = open(path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK)
    guard fd >= 0 else { return false }
    defer { close(fd) }
    if flock(fd, LOCK_EX | LOCK_NB) == 0 {
        flock(fd, LOCK_UN)
        return false
    }
    return errno == EWOULDBLOCK
}

// MARK: - System state

func frontmostPid() -> pid_t? { NSWorkspace.shared.frontmostApplication?.processIdentifier }

func topWindow() -> TopWindow? {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]
    else { return nil }
    for window in windows where (window[kCGWindowLayer as String] as? Int) == 0 {
        guard let owner = window[kCGWindowOwnerPID as String] as? pid_t,
              let number = window[kCGWindowNumber as String] as? CGWindowID else { return nil }
        return TopWindow(owner: owner, number: number)
    }
    return nil
}

func onScreenWindows() -> [WindowInfo] {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]
    else { return [] }
    return windows.compactMap { window in
        guard let owner = window[kCGWindowOwnerPID as String] as? pid_t,
              let bounds = window[kCGWindowBounds as String] as? [String: Double] else { return nil }
        return WindowInfo(owner: owner, layer: window[kCGWindowLayer as String] as? Int ?? 0,
                          alpha: window[kCGWindowAlpha as String] as? Double ?? 1,
                          bounds: CGRect(x: bounds["X"] ?? 0, y: bounds["Y"] ?? 0, width: bounds["Width"] ?? 0,
                                         height: bounds["Height"] ?? 0),
                          number: window[kCGWindowNumber as String] as? CGWindowID ?? 0)
    }
}

func displayContext() -> [String: Any] {
    let display = CGMainDisplayID()
    let mode = CGDisplayCopyDisplayMode(display)
    let modes = (CGDisplayCopyAllDisplayModes(display, nil) as? [CGDisplayMode]) ?? []
    let rates = Set(modes.filter { $0.width == mode?.width && $0.height == mode?.height }.map { $0.refreshRate })
    return ["refresh_hz": mode?.refreshRate ?? 0, "refresh_modes_hz": rates.sorted(),
            "width_pt": mode?.width ?? 0, "height_pt": mode?.height ?? 0,
            "scale": NSScreen.main?.backingScaleFactor ?? 1]
}

// MARK: - Capture

/// Receives display frames; keeps the latest complete one (for calibration)
/// or a log of flipped shares for the block region (for measuring).
final class Capture: NSObject, SCStreamOutput, @unchecked Sendable {
    private let lock = NSLock()
    private var latest: Frame?
    private var observed: [Observed] = []
    private var region: PixelRect?
    private var targetOn = true
    private var calibration: Calibration?

    func latestFrame() -> Frame? { lock.withLock { latest } }
    func frames() -> [Observed] { lock.withLock { observed } }
    func measure(region: PixelRect, calibration: Calibration) {
        lock.withLock { self.region = region; self.calibration = calibration }
    }
    /// Starts a new sample: clear the log and set the state the block should flip to.
    func expect(on: Bool) { lock.withLock { targetOn = on; observed.removeAll() } }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let arrival = Optional(nowNs()),
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let raw = info[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let display = info[.displayTime] as? UInt64,
              let buffer = CMSampleBufferGetImageBuffer(sample) else { return }
        CVPixelBufferLockBaseAddress(buffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(buffer) else { return }
        let width = CVPixelBufferGetWidth(buffer), height = CVPixelBufferGetHeight(buffer)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        let frame = Frame(width: width, height: height, bytesPerRow: stride,
                          pixels: [UInt8](UnsafeRawBufferPointer(start: base, count: stride * height)))
        lock.withLock {
            if let region, let calibration {
                let above = regionStats(frame, region, threshold: calibration.threshold).above
                observed.append(Observed(displayNs: machToNs(display), arrivalNs: arrival,
                                         share: flippedShare(above: above, targetOn: targetOn,
                                                             calibration: calibration)))
            } else {
                latest = frame
            }
        }
    }
}

/// Vsync timestamps from a display link on the main run loop. CADisplayLink's
/// timestamp is host time in seconds, the base CACurrentMediaTime uses.
final class VsyncLog: NSObject, @unchecked Sendable {
    private let lock = NSLock()
    private var times: [UInt64] = []
    @MainActor private var link: CADisplayLink?

    @objc func tick(_ link: CADisplayLink) {
        lock.withLock { times.append(UInt64(link.timestamp * 1e9)) }
    }

    @MainActor func start() {
        link = NSScreen.main?.displayLink(target: self, selector: #selector(tick(_:)))
        link?.add(to: .main, forMode: .common)
    }

    @MainActor func stop() { link?.invalidate() }

    func snapshot() -> [UInt64] { lock.withLock { times } }
}

// MARK: - Run mode

struct Options {
    var pid: pid_t = 0
    var out = ""
    var keys = 100
    var warmup = 20
    var gapMs = (100, 300)
    var censorMs = 500
    var seed: UInt64 = 7
    var injectPid = false
    /// After this many ms the probe posts nothing more and exits (0: none),
    /// so it can never outlive the harness's wait for it.
    var deadlineMs = 0
    var leaseFile = ""
    var selfTestDispatch = false
    var blinkCheck = false
    var blinkWindowID: UInt32 = 0
    var startedNs: UInt64 = 0
    var blinkSettle = 2.5
    var blinkWindow = 6.0
    var cursorRect: CGRect?
}

struct Failure: Error { let reason: String }

/// SplitMix64, so gaps are reproducible for a seed.
struct Rng {
    var state: UInt64
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
    mutating func uniform(_ low: Int, _ high: Int) -> Int { low + Int(next() % UInt64(high - low + 1)) }
}

/// The key's down and up events, built before the post so building them
/// stays out of the timing. Both or neither: a key-down posted alone would
/// leave `j` held.
func keyEvents() -> [CGEvent]? {
    let source = CGEventSource(stateID: .hidSystemState)
    guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0x26 /* j */, keyDown: true),
          let up = CGEvent(keyboardEventSource: source, virtualKey: 0x26, keyDown: false) else { return nil }
    down.flags = []
    up.flags = []
    return [down, up]
}

func post(_ events: [CGEvent], _ options: Options) {
    for event in events {
        if options.injectPid { event.postToPid(options.pid) } else { event.post(tap: .cghidEventTap) }
    }
}

/// The topmost visible window under `point`, front to back.
func windowAt(_ point: CGPoint, _ windows: [WindowInfo]) -> WindowInfo? {
    windows.first { $0.alpha > 0 && $0.bounds.contains(point) }
}

/// One left click at `point`, for the activation fallback.
func click(at point: CGPoint) {
    for type in [CGEventType.leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: .left)?
            .post(tap: .cghidEventTap)
    }
}

/// Serializes key posts with the probe's end: a post and a finish never
/// interleave, and once a finish takes it, it is never released, so nothing
/// posts after the probe has decided to stop.
let gate = NSLock()

/// The one way the probe ends. The first caller writes the result and exits;
/// a later one (the deadline racing a finished run) waits on the gate until
/// the process is gone, so results never overwrite each other.
func finish(_ object: [String: Any], to path: String, code: Int32) -> Never {
    gate.lock()
    write(object, to: path)
    exit(code)
}

func cancelInvocation() -> Never {
    gate.lock()
    exit(4)
}

func secondsSince(_ type: CGEventType) -> Double {
    CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: type)
}

// Shared campaign loops let synthetic checks use the production ordering.
func calibrateBlock(_ options: Options, guardedPost: () throws -> UInt64,
                    latestFrame: () async throws -> Frame,
                    sleep: (UInt64) async throws -> Void) async throws -> (PixelRect, Calibration) {
    // Six toggles: off->on, on->off, ... The block ends where it started.
    var box: PixelRect?
    var onLumas: [Double] = [], offLumas: [Double] = []
    var before = try await latestFrame()
    for toggle in 0..<6 {
        _ = try guardedPost()
        try await sleep(250_000_000)
        let after = try await latestFrame()
        if box == nil {
            do {
                box = try changedBox(before, after)
            } catch {
                // Keep the pair for diagnosis: the raw BGRA frames beside the result.
                for (name, f) in [("before", before), ("after", after)] {
                    FileManager.default.createFile(atPath: options.out + ".\(name)-\(f.width)x\(f.height)-\(f.bytesPerRow).bgra",
                                                   contents: Data(f.pixels))
                }
                throw error
            }
        }
        let region = box!.inset(fraction: 0.25)
        let nowOn = toggle % 2 == 0
        (nowOn ? { onLumas.append(regionStats(after, region, threshold: 0).mean) }
               : { offLumas.append(regionStats(after, region, threshold: 0).mean) })()
        (nowOn ? { offLumas.append(regionStats(before, region, threshold: 0).mean) }
               : { onLumas.append(regionStats(before, region, threshold: 0).mean) })()
        before = after
    }
    let calibration = try calibrate(on: onLumas, off: offLumas)
    return (box!, calibration)
}

func measureKeys(_ options: Options, guardedPost: () throws -> UInt64, expect: (Bool) -> Void,
                 frames: () -> [Observed], now: () -> UInt64,
                 sleep: (UInt64) async throws -> Void,
                 sampleGuard: () throws -> Void) async throws -> ([[String: Any]], UInt64, UInt64) {
    var rng = Rng(state: options.seed)
    var on = false
    var samples: [[String: Any]] = []
    var typingStartMach: UInt64 = 0, typingEndMach: UInt64 = 0
    let censorNs = UInt64(options.censorMs) * 1_000_000
    for seq in 1...(options.warmup + options.keys) {
        on.toggle()
        expect(on)
        let postNs = try guardedPost()
        if seq == options.warmup + 1 { typingStartMach = postNs }
        // Poll until a flipped frame is in, or the censor limit passes. The
        // sample's time is the frame's display time, so the poll interval
        // does not enter the result.
        var sample = Sample(displayNs: nil, arrivalNs: nil, mixedFrames: 0, censored: true)
        while now() - postNs < censorNs + 50_000_000 {
            try await sleep(2_000_000)
            sample = pickSample(frames(), postNs: postNs, censorNs: censorNs)
            if !sample.censored { break }
        }
        // No revert over the next two frame intervals.
        try await sleep(40_000_000)
        let after = frames()
        let reverted = !sample.censored && after.contains { $0.displayNs > sample.displayNs! && $0.share < 0.5 }
        try sampleGuard()
        if seq == options.warmup + options.keys { typingEndMach = now() }
        samples.append(["seq": seq + 6, "warmup": seq <= options.warmup, "t_post": postNs,
                        "display": sample.displayNs.map { $0 as Any } ?? NSNull(),
                        "arrival": sample.arrivalNs.map { $0 as Any } ?? NSNull(),
                        "mixed": sample.mixedFrames, "censored": sample.censored, "reverted": reverted])
        try await sleep(UInt64(rng.uniform(options.gapMs.0, options.gapMs.1)) * 1_000_000)
    }

    return (samples, typingStartMach, typingEndMach)
}

func run(_ options: Options) async throws -> [String: Any] {
    if options.selfTestDispatch { return ["posts": 1, "captures": 0] }
    guard invocationAlive(options.leaseFile) else { throw Failure(reason: "invocation cancelled") }
    guard CGPreflightScreenCaptureAccess() && CGPreflightPostEventAccess() else { throw Failure(reason: "permission") }
    var result: [String: Any] = ["display": displayContext()]

    // The target's window: layer 0, on screen, wider than 300 pt.
    var window: SCWindow?
    var display: SCDisplay?
    for _ in 0..<100 {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        window = content.windows.first {
            $0.owningApplication?.processID == options.pid && $0.windowLayer == 0 && $0.frame.width > 300
        }
        display = content.displays.first { $0.displayID == CGMainDisplayID() }
        if window != nil { break }
        try await Task.sleep(nanoseconds: 100_000_000)
    }
    guard let window, let display else { throw Failure(reason: "no window") }
    let frame = window.frame
    let measured = window.windowID
    result["window_pt"] = [frame.minX, frame.minY, frame.width, frame.height]

    // Activation: already frontmost; or ask cooperatively once; or, if that
    // is ignored, one click on the window's titlebar, only when the measured
    // window is the topmost visible window at that point.
    var activation = "already"
    if !mayPost(frontmost: frontmostPid(), topWindow: topWindow(), target: options.pid, window: measured) {
        activation = "activate"
        NSRunningApplication(processIdentifier: options.pid)?.activate()
        try await Task.sleep(nanoseconds: 2_000_000_000)
    }
    if !mayPost(frontmost: frontmostPid(), topWindow: topWindow(), target: options.pid, window: measured) {
        let titlebar = CGPoint(x: frame.midX, y: frame.minY + 12)
        guard windowAt(titlebar, onScreenWindows())?.number == measured else {
            throw Failure(reason: "not frontmost, and the titlebar is covered")
        }
        activation = "click"
        try gate.withLock {
            guard invocationAlive(options.leaseFile) else { throw Failure(reason: "invocation cancelled") }
            click(at: titlebar)
        }
        try await Task.sleep(nanoseconds: 1_000_000_000)
    }
    result["activation"] = activation
    guard mayPost(frontmost: frontmostPid(), topWindow: topWindow(), target: options.pid, window: measured) else {
        throw Failure(reason: "not frontmost")
    }

    // Park the pointer outside the window and away from the hot corners.
    let screen = CGDisplayBounds(CGMainDisplayID())
    let parkX = frame.minX > 220 ? frame.minX - 120 : min(frame.maxX + 120, screen.maxX - 120)
    CGWarpMouseCursorPosition(CGPoint(x: parkX, y: screen.midY))
    try await Task.sleep(nanoseconds: 200_000_000)
    let startNs = nowNs()

    // Calibration stream over the whole window.
    let capture = Capture()
    let scale = Int(NSScreen.main?.backingScaleFactor ?? 1)
    let config = SCStreamConfiguration()
    config.sourceRect = CGRect(x: frame.minX - screen.minX, y: frame.minY - screen.minY,
                               width: frame.width, height: frame.height)
    config.width = Int(frame.width) * scale
    config.height = Int(frame.height) * scale
    config.minimumFrameInterval = .zero
    config.queueDepth = 8
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.showsCursor = false
    config.colorSpaceName = CGColorSpace.sRGB
    let filter = SCContentFilter(display: display, excludingWindows: [])
    let stream = SCStream(filter: filter, configuration: config, delegate: nil)
    let queue = DispatchQueue(label: "latency-probe.capture")
    try stream.addStreamOutput(capture, type: .screen, sampleHandlerQueue: queue)
    try await stream.startCapture()
    defer { Task { try? await stream.stopCapture() } }

    func latestFrame() async throws -> Frame {
        for _ in 0..<50 {
            if let f = capture.latestFrame() { return f }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        throw Failure(reason: "no frames")
    }

    var lastPostNs = nowNs()
    var watched = frame  // the window until calibration finds the block
    /// Checks every guard, then posts; returns the time taken just before the
    /// post, so neither the guards nor building the events enters a sample.
    func guardedPost() throws -> UInt64 {
        guard let events = keyEvents() else { throw Failure(reason: "could not create the key events") }
        let front = frontmostPid(), top = topWindow()
        guard mayPost(frontmost: front, topWindow: top, target: options.pid, window: measured) else {
            throw Failure(reason: "focus changed before a key (frontmost \(front.map(String.init) ?? "none"), "
                          + "top window \(top.map { "\($0.owner)/\($0.number)" } ?? "none"))")
        }
        if let cover = obscuring(onScreenWindows(), window: measured, rect: watched) {
            throw Failure(reason: coverReason(cover))
        }
        let since = Double(nowNs() - lastPostNs) / 1e9
        let sinceStart = Double(nowNs() - startNs) / 1e9
        let mouse = min(secondsSince(.mouseMoved), secondsSince(.leftMouseDown))
        if foreignInput(secondsSinceKey: secondsSince(.keyDown), secondsSinceMouse: mouse, secondsSincePost: since,
                        secondsSinceStart: sinceStart) && lastPostNs > startNs {
            throw Failure(reason: "foreign input")
        }
        // The deadline is checked at the post itself, under the gate the
        // deadline's finish takes: a guard query that stalls across the
        // deadline cannot let a key through.
        let postNs: UInt64 = try gate.withLock {
            guard invocationAlive(options.leaseFile) else { throw Failure(reason: "invocation cancelled") }
            if options.deadlineMs > 0 && nowNs() - probeStartNs > UInt64(options.deadlineMs) * 1_000_000 {
                throw Failure(reason: "deadline passed")
            }
            let postNs = nowNs()
            post(events, options)
            return postNs
        }
        lastPostNs = postNs
        return postNs
    }

    let typingClockBefore = typingClockCheck()

    let (calibratedBox, calibration) = try await calibrateBlock(options, guardedPost: guardedPost,
        latestFrame: latestFrame, sleep: { try await Task.sleep(nanoseconds: $0) })
    let box: PixelRect? = calibratedBox
    result["calibration"] = ["on": calibration.on, "off": calibration.off, "box_px": [box!.x, box!.y, box!.width, box!.height]]

    // Measuring stream: just the block and a 4 pt margin.
    let margin = 4 * scale
    let boxPt = CGRect(x: config.sourceRect.minX + CGFloat(box!.x - margin) / CGFloat(scale),
                       y: config.sourceRect.minY + CGFloat(box!.y - margin) / CGFloat(scale),
                       width: CGFloat(box!.width + 2 * margin) / CGFloat(scale),
                       height: CGFloat(box!.height + 2 * margin) / CGFloat(scale))
    config.sourceRect = boxPt
    watched = boxPt.offsetBy(dx: screen.minX, dy: screen.minY)
    config.width = box!.width + 2 * margin
    config.height = box!.height + 2 * margin
    capture.measure(region: PixelRect(x: margin, y: margin, width: box!.width, height: box!.height).inset(fraction: 0.25),
                    calibration: calibration)
    try await stream.updateConfiguration(config)
    try await Task.sleep(nanoseconds: 300_000_000)

    let vsync = VsyncLog()
    await MainActor.run { vsync.start() }

    let (samples, typingStartMach, typingEndMach) = try await measureKeys(options,
        guardedPost: guardedPost, expect: { capture.expect(on: $0) }, frames: { capture.frames() },
        now: nowNs, sleep: { try await Task.sleep(nanoseconds: $0) }, sampleGuard: {
        guard mayPost(frontmost: frontmostPid(), topWindow: topWindow(), target: options.pid, window: measured),
              obscuring(onScreenWindows(), window: measured, rect: watched) == nil else {
            throw Failure(reason: "focus changed or a window covered the block during a sample")
        }
        })

    await MainActor.run { vsync.stop() }
    let ticks = vsync.snapshot()
    let periods = zip(ticks, ticks.dropFirst()).map { $1 - $0 }.sorted()
    let period = periods.isEmpty ? 0 : periods[periods.count / 2]
    let shown = samples.compactMap { $0["display"] as? UInt64 }
    result["vsync"] = ["count": ticks.count, "period_ns": period,
                       "phase_spread_ns": phaseSpread(shown, vsyncNs: ticks.first ?? 0, periodNs: period)]
    result["samples"] = samples
    result["calibration_keys"] = 6
    result["typing_epoch"] = ["contract": "hc-typing-memory-v1", "clock": "CLOCK_UPTIME_RAW",
        "probe_clock": "mach_absolute_time_ns", "clock_before": typingClockBefore,
        "clock_after": typingClockCheck(), "start_mach_ns": typingStartMach, "end_mach_ns": typingEndMach,
        "typing_start_ns": typingRawNs(typingStartMach, typingClockBefore),
        "typing_end_ns": typingRawNs(typingEndMach, typingClockBefore),
        "pid": options.pid, "window_id": measured, "guards_ok": true] as [String: Any]
    return result
}

func parse(_ args: [String]) -> Options? {
    var o = Options()
    var i = 1
    func value() -> String? { i += 1; return i < args.count ? args[i] : nil }
    while i < args.count {
        switch args[i] {
        case "--self-test-dispatch": o.selfTestDispatch = true
        case "--blink-check": o.blinkCheck = true
        case "--window-id": guard let v = value(), let n = UInt32(v), n > 0 else { return nil }; o.blinkWindowID = n
        case "--started-ns": guard let v = value(), let n = UInt64(v), n > 0 else { return nil }; o.startedNs = n
        case "--blink-settle": guard let v = value(), let n = Double(v), n.isFinite, n > 0, n <= 10 else { return nil }; o.blinkSettle = n
        case "--blink-window": guard let v = value(), let n = Double(v), n.isFinite, n > 0, n <= 20 else { return nil }; o.blinkWindow = n
        case "--cursor-rect":
            guard let v = value() else { return nil }
            let r = v.split(separator: ",").compactMap { Double($0) }
            guard r.count == 4, r.allSatisfy({ $0.isFinite }), r[0] >= 0, r[1] >= 0,
                  r[2] > 0, r[3] > 0, r[2] <= 256, r[3] <= 256 else { return nil }
            o.cursorRect = CGRect(x: r[0], y: r[1], width: r[2], height: r[3])
        case "--pid": guard let v = value(), let p = pid_t(v) else { return nil }; o.pid = p
        case "--out": guard let v = value() else { return nil }; o.out = v
        case "--keys": guard let v = value(), let n = Int(v) else { return nil }; o.keys = n
        case "--warmup": guard let v = value(), let n = Int(v) else { return nil }; o.warmup = n
        case "--gap-ms":
            guard let v = value() else { return nil }
            let parts = v.split(separator: ":").compactMap { Int($0) }
            guard parts.count == 2, parts[0] <= parts[1] else { return nil }
            o.gapMs = (parts[0], parts[1])
        case "--censor-ms": guard let v = value(), let n = Int(v) else { return nil }; o.censorMs = n
        case "--seed": guard let v = value(), let n = UInt64(v) else { return nil }; o.seed = n
        case "--inject": guard let v = value(), v == "hid" || v == "pid" else { return nil }; o.injectPid = v == "pid"
        case "--lease-file": guard let v = value() else { return nil }; o.leaseFile = v
        case "--deadline-ms": guard let v = value(), let n = Int(v), n >= 0 else { return nil }; o.deadlineMs = n
        default: return nil
        }
        i += 1
    }
    if o.blinkCheck && (o.startedNs == 0 || o.cursorRect == nil || o.blinkWindowID == 0) { return nil }
    return o.pid > 0 && !o.out.isEmpty && o.keys > 0 ? o : nil
}

/// Written atomically: the harness reads the file as soon as it appears.
func write(_ object: [String: Any], to path: String) {
    let data = try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    try? data.write(to: URL(fileURLWithPath: path), options: .atomic)
}

// GUI-free decisions also used by the native capture and entry dispatch.
func probeMode<T>(_ options: Options, blink: () -> T, injecting: () -> T) -> T {
    options.blinkCheck ? blink() : injecting()
}
func blinkWindowMatches(owner: pid_t?, windowID: UInt32, layer: Int, width: CGFloat,
                        options: Options) -> Bool {
    owner == options.pid && windowID == options.blinkWindowID && layer == 0 && width > 300
}
func blinkFrame(previous: (UInt64, String, String)?, raw: Int, hash: String?,
                arrival: UInt64) -> (UInt64, String, String)? {
    if SCFrameStatus(rawValue: raw) == .idle {
        return previous.map { (arrival, $0.1, "idle") }
    }
    guard SCFrameStatus(rawValue: raw) == .complete, let hash = hash else { return previous }
    return (arrival, hash, "complete")
}

// Noninjecting preparation mode. No calibration, key posting, pointer warp,
// click, or activation belongs to this path. The owner supplies a cursor-only
// crop from an excluded geometry pilot; the counted window uses no capture.
final class BlinkCapture: NSObject, SCStreamOutput, @unchecked Sendable {
    let lock = NSLock()
    var latest: (UInt64, String, String)?
    func snapshot() -> (UInt64, String, String)? { lock.withLock { latest } }
    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer,
                of type: SCStreamOutputType) {
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int else { return }
        let arrival = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
        guard SCFrameStatus(rawValue: raw) == .complete, let image = sample.imageBuffer else {
            lock.withLock { latest = blinkFrame(previous: latest, raw: raw, hash: nil, arrival: arrival) }
            return
        }
        CVPixelBufferLockBaseAddress(image, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(image) else { return }
        // Hash only actual pixels, never allocation padding.
        var bytes = Data()
        let width = CVPixelBufferGetWidth(image), height = CVPixelBufferGetHeight(image)
        for y in 0..<height {
            bytes.append(base.advanced(by: y * CVPixelBufferGetBytesPerRow(image))
                .assumingMemoryBound(to: UInt8.self), count: width * 4)
        }
        let hash = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        lock.withLock { latest = blinkFrame(previous: latest, raw: raw, hash: hash, arrival: arrival) }
    }
}
func blinkCheck(_ options: Options) async throws -> [String: Any] {
    if options.selfTestDispatch { return ["posts": 0, "captures": 1] }
    guard invocationAlive(options.leaseFile) else { throw Failure(reason: "invocation cancelled") }
    guard CGPreflightScreenCaptureAccess() else { throw Failure(reason: "screen recording permission") }
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let window = content.windows.first(where: {
        blinkWindowMatches(owner: $0.owningApplication?.processID, windowID: $0.windowID,
                           layer: $0.windowLayer, width: $0.frame.width, options: options)
    }), let display = content.displays.first(where: { $0.displayID == CGMainDisplayID() }),
          let crop = options.cursorRect,
          CGRect(origin: .zero, size: window.frame.size).contains(crop) else {
        throw Failure(reason: "cursor window or crop unavailable")
    }
    let start = options.startedNs + UInt64(options.blinkSettle * 1e9)
    let end = start + UInt64(options.blinkWindow * 1e9)
    guard clock_gettime_nsec_np(CLOCK_UPTIME_RAW) < start else { throw Failure(reason: "capture readiness missed launch boundary") }
    let config = SCStreamConfiguration()
    let scale = CGFloat(NSScreen.main?.backingScaleFactor ?? 1)
    let screen = CGDisplayBounds(display.displayID)
    config.sourceRect = crop.offsetBy(dx: window.frame.minX - screen.minX, dy: window.frame.minY - screen.minY)
    config.width = Int(crop.width * scale); config.height = Int(crop.height * scale)
    config.minimumFrameInterval = CMTime(value: 1, timescale: 10)
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.showsCursor = false
    config.queueDepth = 3
    let capture = BlinkCapture()
    let stream = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: config, delegate: nil)
    try stream.addStreamOutput(capture, type: .screen, sampleHandlerQueue: DispatchQueue(label: "blink-check.capture"))
    try await stream.startCapture()
    var frames: [[String: Any]] = []
    do {
        for i in 0...Int(options.blinkWindow * 10) {
            let deadline = start + UInt64(i) * 100_000_000
            let now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
            if deadline > now { try await Task.sleep(nanoseconds: deadline - now) }
            guard invocationAlive(options.leaseFile) else { throw Failure(reason: "invocation cancelled") }
            let t = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
            guard let (arrival, hash, status) = capture.snapshot(), t >= arrival, t - arrival <= 250_000_000 else {
                throw Failure(reason: "cursor frame missing or stale")
            }
            let visible = mayPost(frontmost: frontmostPid(), topWindow: topWindow(), target: options.pid, window: window.windowID)
                && obscuring(onScreenWindows(), window: window.windowID,
                             rect: crop.offsetBy(dx: window.frame.minX, dy: window.frame.minY)) == nil
            frames.append(["t_ns": t, "arrival_ns": arrival, "sha256": hash, "visible": visible, "frame_status": status])
        }
        try await stream.stopCapture()
    } catch {
        try? await stream.stopCapture()
        throw error
    }
    let mode = CGDisplayCopyDisplayMode(display.displayID)
    let nativeDisplay: [String: Any] = ["width_pt": mode?.width ?? 0, "height_pt": mode?.height ?? 0,
                                      "pixel_width": mode?.pixelWidth ?? 0, "pixel_height": mode?.pixelHeight ?? 0,
                                      "refresh_hz": mode?.refreshRate ?? 0]
    return ["started_ns": options.startedNs, "native_display": nativeDisplay,
            "start_ns": start, "end_ns": end, "frames": frames, "window_id": window.windowID,
            "display": displayContext(), "cursor_rect": [crop.minX, crop.minY, crop.width, crop.height]]
}

// A virtual clock and frames exercise both loops without AppKit or input.
func typingFixture(failGuard: Bool) async throws -> [String: Any] {
    var options = Options()
    options.keys = 2
    var clock: UInt64 = 1_000_000_000
    var events: [[String: Any]] = []
    var posts = 0
    func record(_ kind: String, _ extra: [String: Any] = [:]) {
        events.append(extra.merging(["kind": kind, "at": clock]) { _, new in new })
    }
    func guardedPost() -> UInt64 {
        posts += 1
        record("post", ["number": posts])
        return clock
    }
    func sleep(_ ns: UInt64) async {
        record("sleep", ["ns": ns]); clock += ns
    }
    func frame() -> Frame {
        var pixels = [UInt8](repeating: 20, count: 200 * 100 * 4)
        if posts % 2 == 1 {
            for y in 30..<98 { for x in 40..<168 {
                let i = (y * 200 + x) * 4
                pixels[i] = 230; pixels[i + 1] = 230; pixels[i + 2] = 230
            } }
        }
        return Frame(width: 200, height: 100, bytesPerRow: 800, pixels: pixels)
    }
    let (box, calibration) = try await calibrateBlock(options, guardedPost: guardedPost,
        latestFrame: { frame() }, sleep: sleep)
    let calibrationPosts = posts
    var currentPost: UInt64 = 0
    let (samples, start, end) = try await measureKeys(options, guardedPost: {
        currentPost = guardedPost(); return currentPost
    }, expect: { record("expect", ["on": $0]) }, frames: {
        // First measured key has a 94% frame then a 96% frame; last censors.
        if posts == calibrationPosts + options.warmup + options.keys { return [] }
        return [Observed(displayNs: currentPost + 1_000_000, arrivalNs: currentPost + 1_500_000, share: 0.94),
                Observed(displayNs: currentPost + 2_000_000, arrivalNs: currentPost + 2_500_000, share: 0.96)]
    }, now: { clock }, sleep: sleep, sampleGuard: {
        record("guard")
        if failGuard && posts == calibrationPosts + options.warmup + options.keys {
            throw Failure(reason: "synthetic focus loss")
        }
    })
    let check: [String: UInt64] = ["raw_before_ns": 100, "raw_after_ns": 300, "mach_ns": 500]
    return ["calibration_posts": calibrationPosts, "box": [box.x, box.y, box.width, box.height],
            "calibration": [calibration.on, calibration.off], "events": events, "samples": samples,
            "start_mach_ns": start, "end_mach_ns": end,
            "typing_start_ns": typingRawNs(start, check), "typing_end_ns": typingRawNs(end, check)]
}

// MARK: - Entry

let probeStartNs = nowNs()
let args = CommandLine.arguments
if args.contains("--self-test") { exit(selfTest()) }
if let typingIndex = args.firstIndex(of: "--self-test-typing"), typingIndex + 1 < args.count {
    let output = args[typingIndex + 1]
    Task.detached {
        do {
            let result = try await typingFixture(failGuard: args.contains("--fail-guard"))
            finish(result, to: output, code: 0)
        } catch {
            finish(["error": "typing fixture failed: \(error)"], to: output, code: 1)
        }
    }
    dispatchMain()
}
let leaseIndex = args.firstIndex(of: "--lease-file")
let invocationLease = leaseIndex.flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } ?? ""
let leaseTimer = DispatchSource.makeTimerSource(queue: DispatchQueue.global())
if !invocationLease.isEmpty {
    guard invocationAlive(invocationLease) else { cancelInvocation() }
    leaseTimer.schedule(deadline: .now(), repeating: .milliseconds(50))
    leaseTimer.setEventHandler {
        if !invocationAlive(invocationLease) {
            cancelInvocation()
        }
    }
    leaseTimer.resume()
}
if args.contains("--check") || args.contains("--request") {
    var screen = CGPreflightScreenCaptureAccess(), events = CGPreflightPostEventAccess()
    if args.contains("--request") {
        screen = screen || CGRequestScreenCaptureAccess()
        events = events || CGRequestPostEventAccess()
    }
    print("screen recording: \(screen ? "granted" : "missing"), post events: \(events ? "granted" : "missing")")
    exit(screen && events ? 0 : 3)
}
guard let options = parse(args) else {
    FileHandle.standardError.write("usage: see the header of latency-probe.swift\n".data(using: .utf8)!)
    exit(2)
}
// Synthetic entry dispatch uses the same parser and task, before AppKit or TCC.
let app = options.selfTestDispatch ? nil : NSApplication.shared
app?.setActivationPolicy(.accessory)
if options.deadlineMs > 0 {
    // guardedPost refuses after the deadline; this also ends a probe stuck
    // in a wait, so none outlives the harness's wait for it.
    DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(options.deadlineMs)) {
        finish(["error": "deadline passed"], to: options.out, code: 4)
    }
}
Task.detached {
    do {
        let operation = probeMode(options, blink: { blinkCheck }, injecting: { run })
        finish(try await operation(options), to: options.out, code: 0)
    } catch let failure as Failure {
        finish(["error": failure.reason], to: options.out, code: failure.reason == "permission" ? 3 : 1)
    } catch {
        finish(["error": "\(error)"], to: options.out, code: 1)
    }
}
if let app { app.run() } else { dispatchMain() }
