// latency-floor MODE LOG
//
// A reference window for the keystroke-to-screen workload: the least work a
// macOS app can do to show a key, with no terminal, PTY or text. It opens a
// 960x600 window with a 128x64 block in the middle and toggles the block on
// every key press, logging the same 32-byte records as keyblock (sequence,
// 1, time of the key event, time the frame was handed to the system). Modes:
//
//   ca            the block is a CALayer whose color changes inside a
//                 CATransaction with actions disabled
//   metal-sync    a CAMetalLayer with displaySyncEnabled, 3 drawables
//   metal-nosync  the same without display sync
//   metal-sync-2  display sync with 2 drawables
//
// Floors are reported beside the terminals and never ranked.
import AppKit
import Metal
import QuartzCore

let off = (r: 0.10, g: 0.11, b: 0.15)
let on = (r: 0.85, g: 0.86, b: 0.90)

final class Log {
    let handle: FileHandle
    var seq: UInt64 = 0

    init?(path: String) {
        guard FileManager.default.createFile(atPath: path, contents: nil),
              let handle = FileHandle(forWritingAtPath: path) else { return nil }
        self.handle = handle
    }

    func record(read: UInt64, written: UInt64) {
        seq += 1
        var fields = [seq, 1, read, written]
        handle.write(Data(bytes: &fields, count: 32))
    }
}

let shader = """
#include <metal_stdlib>
using namespace metal;
vertex float4 vmain(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * 2.0 - 1.0, 0.0, 1.0);
}
fragment float4 fmain(constant float4 &color [[buffer(0)]]) { return color; }
"""

final class FloorView: NSView {
    let mode: String
    let log: Log
    var lit = false
    let block = CALayer()
    var device: MTLDevice?
    var queue: MTLCommandQueue?
    var pipeline: MTLRenderPipelineState?

    init(frame: NSRect, mode: String, log: Log) {
        self.mode = mode
        self.log = log
        super.init(frame: frame)
        wantsLayer = true
        if mode == "ca" {
            layer?.backgroundColor = CGColor(red: off.r, green: off.g, blue: off.b, alpha: 1)
            block.frame = CGRect(x: frame.midX - 64, y: frame.midY - 32, width: 128, height: 64)
            block.backgroundColor = layer?.backgroundColor
            layer?.addSublayer(block)
        }
    }

    required init?(coder: NSCoder) { nil }

    override var acceptsFirstResponder: Bool { true }

    override func makeBackingLayer() -> CALayer {
        guard mode.hasPrefix("metal"), let device = MTLCreateSystemDefaultDevice() else { return CALayer() }
        let metal = CAMetalLayer()
        metal.device = device
        metal.pixelFormat = .bgra8Unorm_srgb
        metal.displaySyncEnabled = mode != "metal-nosync"
        metal.maximumDrawableCount = mode == "metal-sync-2" ? 2 : 3
        self.device = device
        queue = device.makeCommandQueue()
        let library = try! device.makeLibrary(source: shader, options: nil)
        let description = MTLRenderPipelineDescriptor()
        description.vertexFunction = library.makeFunction(name: "vmain")
        description.fragmentFunction = library.makeFunction(name: "fmain")
        description.colorAttachments[0].pixelFormat = .bgra8Unorm_srgb
        pipeline = try! device.makeRenderPipelineState(descriptor: description)
        return metal
    }

    func draw() {
        guard let metal = layer as? CAMetalLayer, let queue, let pipeline,
              let drawable = metal.nextDrawable(), let buffer = queue.makeCommandBuffer() else { return }
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = drawable.texture
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColor(red: off.r, green: off.g, blue: off.b, alpha: 1)
        pass.colorAttachments[0].storeAction = .store
        guard let encoder = buffer.makeRenderCommandEncoder(descriptor: pass) else { return }
        if lit {
            let w = drawable.texture.width, h = drawable.texture.height
            let scale = Int(metal.contentsScale)
            encoder.setScissorRect(MTLScissorRect(x: w / 2 - 64 * scale, y: h / 2 - 32 * scale,
                                                  width: 128 * scale, height: 64 * scale))
            var color = SIMD4<Float>(Float(on.r), Float(on.g), Float(on.b), 1)
            encoder.setRenderPipelineState(pipeline)
            encoder.setFragmentBytes(&color, length: MemoryLayout<SIMD4<Float>>.size, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
        }
        encoder.endEncoding()
        buffer.present(drawable)
        buffer.commit()
    }

    override func keyDown(with event: NSEvent) {
        let read = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
        lit.toggle()
        if mode == "ca" {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            let c = lit ? on : off
            block.backgroundColor = CGColor(red: c.r, green: c.g, blue: c.b, alpha: 1)
            CATransaction.commit()
            CATransaction.flush()
        } else {
            draw()
        }
        log.record(read: read, written: clock_gettime_nsec_np(CLOCK_UPTIME_RAW))
    }
}

let args = CommandLine.arguments
let modes = ["ca", "metal-sync", "metal-nosync", "metal-sync-2"]
guard args.count == 3, modes.contains(args[1]), let log = Log(path: args[2]) else {
    FileHandle.standardError.write("usage: latency-floor ca|metal-sync|metal-nosync|metal-sync-2 LOG\n".data(using: .utf8)!)
    exit(2)
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 960, height: 600),
                      styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "latency floor: \(args[1])"
let view = FloorView(frame: NSRect(x: 0, y: 0, width: 960, height: 600), mode: args[1], log: log)
window.contentView = view
window.center()
window.makeKeyAndOrderFront(nil)
window.makeFirstResponder(view)
if args[1] != "ca" { DispatchQueue.main.async { view.draw() } }
app.activate()
signal(SIGTERM) { _ in exit(0) }
app.run()
