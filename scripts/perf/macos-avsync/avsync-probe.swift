// KettleAVSync: when a lane's picture changes and when its sound is heard,
// on one clock, for measuring a playing video's A/V sync.
//
//   avsync-probe --check        exit 0 if Screen Recording is granted, 3 if
//                               not; never prompts
//   avsync-probe --request      ask macOS for the grant
//   avsync-probe --self-test    the pure helpers on synthetic input; no TCC
//   avsync-probe --latency      print the default output device's latency
//   avsync-probe --rect X,Y,W,H --seconds S --out DIR [--click X,Y --click-after T]
//                [--key CODE --key-at T1,T2,...]
//
// Run mode streams one rectangle of the display under it (global points, top
// left origin) through ScreenCaptureKit, with the system's audio, for S
// seconds (at most 600), and writes four files to DIR, which must exist and be
// empty:
//
//   frames.json  per complete frame: its displayTime (ns, host clock) and the
//                rectangle's mean luma, 0 to 1
//   audio.f32    every captured sample, one channel (the louder channel's
//                absolute value), little-endian float32
//   audio.json   per audio buffer: its presentation time (ns, host clock) and
//                sample count; the sample rate
//   run.json     the default output device's presentation latency as Core
//                Audio reports it, the clock checks and what was captured
//
// With --click, it posts one left click at that global point T seconds into
// the capture (a lane's play control: a control client may not start a
// video, so the press is a native one) and records when, on the same clock.
// With --key, it presses that virtual key code at each listed time (a lane
// that holds the keyboard steps its video five seconds on an arrow; a
// control client's step is refused) and records each, on the same clock.
//
// Both legs are on the host clock. A frame's displayTime is mach time, when
// the window server displayed it, not when light left the panel. An audio
// buffer's presentation time is CMClockGetHostTimeClock time, converted to
// mach time. The run fails if either leg strays more than a second from the
// host clock at arrival. It captures only the rectangle and keeps no pixels;
// the audio is every app's, so run it with nothing else playing.
//
// Pairing flashes with clicks and judging the result is the driver's
// (macos-avsync.py), so this binary, which Screen Recording binds to, stays
// unchanged while the analysis changes.

import AppKit
import CoreAudio
import CoreMedia
import Foundation
import ScreenCaptureKit

let timebase: mach_timebase_info_data_t = {
    var info = mach_timebase_info_data_t()
    mach_timebase_info(&info)
    return info
}()

func machToNs(_ t: UInt64) -> UInt64 {
    t / UInt64(timebase.denom) * UInt64(timebase.numer)
        + t % UInt64(timebase.denom) * UInt64(timebase.numer) / UInt64(timebase.denom)
}

func nowNs() -> UInt64 { machToNs(mach_absolute_time()) }

/// A host-clock CMTime in nanoseconds, or nil for an invalid time.
func hostNs(_ time: CMTime) -> UInt64? {
    guard time.isValid, time.isNumeric else { return nil }
    return machToNs(CMClockConvertHostTimeToSystemUnits(time))
}

struct Failure: Error, CustomStringConvertible {
    let description: String
}

/// The mean luma (Rec. 709 weights on the stored sRGB values) of a BGRA
/// image, from every `step`th pixel each way.
func meanLuma(_ base: UnsafeRawPointer, width: Int, height: Int, bytesPerRow: Int,
              step: Int = 2) -> Double {
    var sum = 0.0
    var count = 0
    var y = 0
    while y < height {
        let row = base.advanced(by: y * bytesPerRow).assumingMemoryBound(to: UInt8.self)
        var x = 0
        while x < width {
            let b = Double(row[x * 4]), g = Double(row[x * 4 + 1]), r = Double(row[x * 4 + 2])
            sum += 0.2126 * r + 0.7152 * g + 0.0722 * b
            count += 1
            x += step
        }
        y += step
    }
    return count == 0 ? 0 : sum / Double(count) / 255
}

/// One channel from several: per frame, the largest absolute value.
func louder(_ channels: [UnsafeBufferPointer<Float>], frames: Int) -> [Float] {
    var out = [Float](repeating: 0, count: frames)
    for channel in channels {
        for i in 0..<min(frames, channel.count) {
            out[i] = max(out[i], abs(channel[i]))
        }
    }
    return out
}

/// X,Y in points; both finite.
func parsePoint(_ text: String) -> CGPoint? {
    let parts = text.split(separator: ",").compactMap { Double($0) }
    guard parts.count == 2, parts.allSatisfy(\.isFinite) else { return nil }
    return CGPoint(x: parts[0], y: parts[1])
}

/// One left click at a global point, in points.
func click(at point: CGPoint) {
    for kind in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource: nil, mouseType: kind, mouseCursorPosition: point,
                mouseButton: .left)?.post(tap: .cghidEventTap)
        usleep(30_000)
    }
}

/// One press and release of a virtual key code, to the frontmost app.
func press(key: CGKeyCode) {
    for down in [true, false] {
        CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: down)?.post(tap: .cghidEventTap)
        usleep(30_000)
    }
}

/// Times in seconds, each finite, nonnegative and after the one before.
func parseTimes(_ text: String) -> [Double]? {
    let parts = text.split(separator: ",").map { Double($0) }
    guard !parts.isEmpty, parts.allSatisfy({ $0 != nil }) else { return nil }
    let times = parts.compactMap { $0 }
    guard times.allSatisfy({ $0.isFinite && $0 >= 0 }), zip(times, times.dropFirst()).allSatisfy({ $0 < $1 }) else {
        return nil
    }
    return times
}

/// X,Y,W,H in points; every part finite, the size positive.
func parseRect(_ text: String) -> CGRect? {
    let parts = text.split(separator: ",").compactMap { Double($0) }
    guard parts.count == 4, parts.allSatisfy(\.isFinite), parts[2] >= 1, parts[3] >= 1 else {
        return nil
    }
    return CGRect(x: parts[0], y: parts[1], width: parts[2], height: parts[3])
}

// MARK: Core Audio

/// An audio object's property of exactly `T`'s size. Callers name `T`
/// (`u32`, `f64`): inferred from an `Any` context it would be `Any`.
func property<T>(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector,
                 _ scope: AudioObjectPropertyScope, as _: T.Type, empty: T) -> T? {
    var address = AudioObjectPropertyAddress(mSelector: selector, mScope: scope,
                                             mElement: kAudioObjectPropertyElementMain)
    var value = empty
    var size = UInt32(MemoryLayout<T>.size)
    let status = withUnsafeMutablePointer(to: &value) {
        AudioObjectGetPropertyData(object, &address, 0, nil, &size, $0)
    }
    return status == noErr && size == UInt32(MemoryLayout<T>.size) ? value : nil
}

func u32(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector,
         _ scope: AudioObjectPropertyScope) -> UInt32? {
    property(object, selector, scope, as: UInt32.self, empty: 0)
}

func f64(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector,
         _ scope: AudioObjectPropertyScope) -> Float64? {
    property(object, selector, scope, as: Float64.self, empty: 0)
}

/// The default output device's presentation latency, in the parts Kettle's
/// audio output adds (kettle-media-native's audio_out::latency), in frames
/// at its nominal rate.
func outputLatency() -> [String: Any] {
    let system = AudioObjectID(kAudioObjectSystemObject)
    guard let device = u32(system, kAudioHardwarePropertyDefaultOutputDevice,
                           kAudioObjectPropertyScopeGlobal),
          device != kAudioObjectUnknown else {
        return ["error": "no default output device"]
    }
    let output = kAudioObjectPropertyScopeOutput, global = kAudioObjectPropertyScopeGlobal
    var out: [String: Any] = [:]
    out["device_latency_frames"] = u32(device, kAudioDevicePropertyLatency, output)
    out["safety_offset_frames"] = u32(device, kAudioDevicePropertySafetyOffset, output)
    out["buffer_frames"] = u32(device, kAudioDevicePropertyBufferFrameSize, global)
    // A device of one output stream or more says the first's latency.
    if let stream = u32(device, kAudioDevicePropertyStreams, output) {
        out["stream_latency_frames"] = u32(stream, kAudioStreamPropertyLatency, global)
    }
    out["nominal_rate"] = f64(device, kAudioDevicePropertyNominalSampleRate, global)
    out["transport"] = u32(device, kAudioDevicePropertyTransportType, global)
    return out
}

// MARK: Capture

final class Capture: NSObject, SCStreamOutput, @unchecked Sendable {
    private let lock = NSLock()
    private var frames: [[String: Any]] = []
    private var buffers: [[String: Any]] = []
    private var samples: [Float] = []
    private var sampleRate: Double?
    private var worstVideoSkewNs: Int64 = 0
    private var worstAudioSkewNs: Int64 = 0
    private var badAudio: String?

    init(seconds: Double) {
        samples.reserveCapacity(Int(seconds * 48_000) + 48_000)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer,
                of type: SCStreamOutputType) {
        let arrival = nowNs()
        switch type {
        case .screen: screen(sample, arrival: arrival)
        case .audio: audio(sample, arrival: arrival)
        default: break
        }
    }

    private func screen(_ sample: CMSampleBuffer, arrival: UInt64) {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let raw = info[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let display = info[.displayTime] as? UInt64,
              let buffer = CMSampleBufferGetImageBuffer(sample) else { return }
        CVPixelBufferLockBaseAddress(buffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(buffer) else { return }
        let luma = meanLuma(base, width: CVPixelBufferGetWidth(buffer),
                            height: CVPixelBufferGetHeight(buffer),
                            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer))
        let displayNs = machToNs(display)
        let skew = Int64(bitPattern: arrival &- displayNs)
        lock.withLock {
            worstVideoSkewNs = max(worstVideoSkewNs, abs(skew))
            frames.append(["display_ns": displayNs, "luma": luma])
        }
    }

    private func audio(_ sample: CMSampleBuffer, arrival: UInt64) {
        guard let format = CMSampleBufferGetFormatDescription(sample),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(format)?.pointee,
              asbd.mFormatID == kAudioFormatLinearPCM,
              asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0, asbd.mBitsPerChannel == 32 else {
            lock.withLock { badAudio = badAudio ?? "audio is not float32 linear PCM" }
            return
        }
        guard let pts = hostNs(CMSampleBufferGetPresentationTimeStamp(sample)) else {
            lock.withLock { badAudio = badAudio ?? "audio buffer without a time" }
            return
        }
        let count = CMSampleBufferGetNumSamples(sample)
        let interleaved = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0
        var mono: [Float] = []
        do {
            try sample.withAudioBufferList { list, _ in
                if interleaved {
                    let channels = Int(asbd.mChannelsPerFrame)
                    guard let data = list.first?.mData, channels > 0 else { return }
                    let all = UnsafeBufferPointer(start: data.assumingMemoryBound(to: Float.self),
                                                  count: count * channels)
                    mono = (0..<count).map { frame in
                        (0..<channels).map { abs(all[frame * channels + $0]) }.max() ?? 0
                    }
                } else {
                    let channels = list.map {
                        UnsafeBufferPointer(start: $0.mData?.assumingMemoryBound(to: Float.self),
                                            count: Int($0.mDataByteSize) / 4)
                    }
                    mono = louder(channels, frames: count)
                }
            }
        } catch {
            lock.withLock { badAudio = badAudio ?? "audio buffer list: \(error)" }
            return
        }
        let skew = Int64(bitPattern: arrival &- pts)
        lock.withLock {
            if let rate = sampleRate, rate != asbd.mSampleRate {
                badAudio = badAudio ?? "the sample rate changed"
            }
            sampleRate = asbd.mSampleRate
            worstAudioSkewNs = max(worstAudioSkewNs, abs(skew))
            buffers.append(["pts_ns": pts, "samples": mono.count, "offset": samples.count])
            samples.append(contentsOf: mono)
        }
    }

    func finish(out: URL, seconds: Double, rect: CGRect, started: UInt64, clicked: UInt64?,
                keys: [UInt64], latency: [String: Any]) throws {
        let (frames, buffers, samples, rate, videoSkew, audioSkew, bad) = lock.withLock {
            (self.frames, self.buffers, self.samples, self.sampleRate, worstVideoSkewNs,
             worstAudioSkewNs, badAudio)
        }
        if let bad { throw Failure(description: bad) }
        guard let rate, !buffers.isEmpty else {
            throw Failure(description: "no audio arrived (nothing playing, or no grant)")
        }
        guard !frames.isEmpty else { throw Failure(description: "no frames arrived") }
        let second: Int64 = 1_000_000_000
        guard videoSkew < second, audioSkew < second else {
            throw Failure(description: "a leg is not on the host clock (video \(videoSkew) ns, audio \(audioSkew) ns)")
        }
        let json = { (value: Any, name: String) in
            let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
            try data.write(to: out.appendingPathComponent(name), options: .withoutOverwriting)
        }
        try json(["frames": frames], "frames.json")
        try json(["sample_rate": rate, "buffers": buffers], "audio.json")
        let bytes = samples.withUnsafeBufferPointer { Data(buffer: $0) }
        try bytes.write(to: out.appendingPathComponent("audio.f32"), options: .withoutOverwriting)
        try json([
            "version": 1,
            "rect": [rect.minX, rect.minY, rect.width, rect.height],
            "seconds": seconds,
            "started_ns": started,
            "click_ns": clicked.map { $0 as Any } ?? NSNull(),
            "keys_ns": keys,
            "frames": frames.count,
            "audio_buffers": buffers.count,
            "audio_samples": samples.count,
            "worst_video_skew_ns": videoSkew,
            "worst_audio_skew_ns": audioSkew,
            "output_latency": latency,
        ], "run.json")
    }
}

func run(rect: CGRect, seconds: Double, out: URL, press button: (CGPoint, Double)?,
         keys: (CGKeyCode, [Double])?) async throws {
    guard CGPreflightScreenCaptureAccess() else {
        throw Failure(description: "Screen Recording is not granted")
    }
    let entries = try FileManager.default.contentsOfDirectory(atPath: out.path)
    guard entries.isEmpty else { throw Failure(description: "the output directory is not empty") }
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    let center = CGPoint(x: rect.midX, y: rect.midY)
    guard let display = content.displays.first(where: { $0.frame.contains(center) }) else {
        throw Failure(description: "no display under the rectangle")
    }
    guard display.frame.contains(rect) else {
        throw Failure(description: "the rectangle crosses a display edge")
    }
    let config = SCStreamConfiguration()
    config.sourceRect = rect.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
    // Luma needs no detail: a quarter of the points each way.
    config.width = max(8, Int(rect.width / 4))
    config.height = max(8, Int(rect.height / 4))
    config.minimumFrameInterval = .zero
    config.queueDepth = 8
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.showsCursor = false
    config.colorSpaceName = CGColorSpace.sRGB
    config.capturesAudio = true
    config.sampleRate = 48_000
    config.channelCount = 2
    config.excludesCurrentProcessAudio = true
    let filter = SCContentFilter(display: display, excludingWindows: [])
    let stream = SCStream(filter: filter, configuration: config, delegate: nil)
    let capture = Capture(seconds: seconds)
    try stream.addStreamOutput(capture, type: .screen,
                               sampleHandlerQueue: DispatchQueue(label: "avsync.screen"))
    try stream.addStreamOutput(capture, type: .audio,
                               sampleHandlerQueue: DispatchQueue(label: "avsync.audio"))
    let latencyBefore = outputLatency()
    let started = nowNs()
    try await stream.startCapture()
    // Each event at its time from the start, in order.
    var events: [(Double, Bool)] = []
    if let (_, after) = button { events.append((after, true)) }
    for at in keys?.1 ?? [] { events.append((at, false)) }
    events.sort { $0.0 < $1.0 }
    var clicked: UInt64?
    var pressed: [UInt64] = []
    func sleep(until offset: Double) async throws {
        let wait = Double(started) + offset * 1e9 - Double(nowNs())
        if wait > 0 { try await Task.sleep(nanoseconds: UInt64(wait)) }
    }
    for (at, isClick) in events {
        try await sleep(until: at)
        if isClick, let (point, _) = button {
            clicked = nowNs()
            click(at: point)
        } else if let (code, _) = keys {
            pressed.append(nowNs())
            press(key: code)
        }
    }
    try await sleep(until: seconds)
    try await stream.stopCapture()
    var latency = latencyBefore
    let after = outputLatency()
    if !NSDictionary(dictionary: latencyBefore).isEqual(to: after) {
        latency["changed_to"] = after
    }
    try capture.finish(out: out, seconds: seconds, rect: rect, started: started, clicked: clicked,
                       keys: pressed, latency: latency)
}

// MARK: Self-test

func selfTest() -> Int32 {
    var failures = 0
    func check(_ ok: Bool, _ what: String) {
        if !ok { failures += 1; FileHandle.standardError.write("self-test: \(what)\n".data(using: .utf8)!) }
    }
    // A 4x2 BGRA image: white on the left half, black on the right.
    var pixels = [UInt8](repeating: 0, count: 4 * 2 * 4)
    for y in 0..<2 { for x in 0..<2 { for c in 0..<3 { pixels[(y * 4 + x) * 4 + c] = 255 } } }
    let half = pixels.withUnsafeBytes { meanLuma($0.baseAddress!, width: 4, height: 2, bytesPerRow: 16, step: 1) }
    check(abs(half - 0.5) < 1e-9, "half white is 0.5 luma, got \(half)")
    let green = [UInt8](arrayLiteral: 0, 255, 0, 255)
    let g = green.withUnsafeBytes { meanLuma($0.baseAddress!, width: 1, height: 1, bytesPerRow: 4, step: 1) }
    check(abs(g - 0.7152) < 1e-9, "green weighs 0.7152, got \(g)")
    let left: [Float] = [0.1, -0.5, 0.2], right: [Float] = [-0.3, 0.4, 0.1]
    let mono = left.withUnsafeBufferPointer { l in right.withUnsafeBufferPointer { r in louder([l, r], frames: 3) } }
    check(mono == [0.3, 0.5, 0.2], "the louder channel per frame, got \(mono)")
    check(parsePoint("10.5,-3") == CGPoint(x: 10.5, y: -3), "a point parses")
    for bad in ["10", "1,2,3", "x,1", "inf,1"] { check(parsePoint(bad) == nil, "\(bad) is not a point") }
    check(parseTimes("1.5,10,16.25") == [1.5, 10, 16.25], "times parse")
    for bad in ["", "1,1", "2,1", "-1", "1,x", "inf"] { check(parseTimes(bad) == nil, "\(bad) is not a list of times") }
    check(parseRect("10,20,300,200") == CGRect(x: 10, y: 20, width: 300, height: 200), "a rectangle parses")
    for bad in ["10,20,300", "10,20,0,5", "a,b,c,d", "10,20,nan,5", "1,2,3,4,5"] {
        check(parseRect(bad) == nil, "\(bad) is refused")
    }
    // mach time converts without overflow for months of uptime.
    let month = UInt64(30 * 86_400) * 1_000_000_000
    let ticks = month / UInt64(timebase.numer) * UInt64(timebase.denom)
    let back = machToNs(ticks)
    check(back <= month && month - back < 1_000, "mach time round trip, got \(back)")
    check(hostNs(.invalid) == nil, "an invalid time has no host time")
    if failures == 0 { print("self-test: ok") }
    return failures == 0 ? 0 : 1
}

// MARK: Main

let args = CommandLine.arguments
func value(_ name: String) -> String? {
    guard let at = args.firstIndex(of: name), at + 1 < args.count else { return nil }
    return args[at + 1]
}

if args.contains("--self-test") {
    exit(selfTest())
}
if args.contains("--latency") {
    let data = try! JSONSerialization.data(withJSONObject: outputLatency(), options: [.sortedKeys])
    print(String(decoding: data, as: UTF8.self))
    exit(0)
}
if args.contains("--check") || args.contains("--request") {
    var granted = CGPreflightScreenCaptureAccess()
    if args.contains("--request") && !granted { granted = CGRequestScreenCaptureAccess() }
    print(granted ? "screen recording: granted" : "screen recording: missing")
    exit(granted ? 0 : 3)
}
guard let rectText = value("--rect"), let rect = parseRect(rectText),
      let secondsText = value("--seconds"), let seconds = Double(secondsText),
      seconds.isFinite, seconds >= 1, seconds <= 600,
      let outText = value("--out") else {
    FileHandle.standardError.write("usage: avsync-probe --check | --request | --self-test | --latency | --rect X,Y,W,H --seconds S --out DIR [--click X,Y --click-after T] [--key CODE --key-at T1,T2,...]\n".data(using: .utf8)!)
    exit(2)
}
var press: (CGPoint, Double)?
if let pointText = value("--click") {
    guard let point = parsePoint(pointText), let afterText = value("--click-after"),
          let after = Double(afterText), after.isFinite, after >= 0, after < seconds else {
        FileHandle.standardError.write("--click needs X,Y and --click-after T inside the capture\n".data(using: .utf8)!)
        exit(2)
    }
    press = (point, after)
}
var keys: (CGKeyCode, [Double])?
if let codeText = value("--key") {
    guard let code = UInt16(codeText), code < 128, let timesText = value("--key-at"),
          let times = parseTimes(timesText), times.last! < seconds else {
        FileHandle.standardError.write("--key needs a virtual key code and --key-at T1,T2,... inside the capture\n".data(using: .utf8)!)
        exit(2)
    }
    keys = (CGKeyCode(code), times)
}
let out = URL(fileURLWithPath: outText, isDirectory: true)
let done = DispatchSemaphore(value: 0)
var status: Int32 = 0
Task {
    do {
        try await run(rect: rect, seconds: seconds, out: out, press: press, keys: keys)
        print("avsync: captured")
    } catch {
        FileHandle.standardError.write("avsync: \(error)\n".data(using: .utf8)!)
        try? JSONSerialization.data(withJSONObject: ["error": "\(error)"])
            .write(to: out.appendingPathComponent("error.json"), options: .withoutOverwriting)
        status = 1
    }
    done.signal()
}
done.wait()
exit(status)
