# Terminator `terminalshot.py` port — design

> Status: **Shipped in v1.46.0; readback target revised in v2.56.x.** The live
> renderer screenshot path landed end to end: the
> `ScreenshotWorker`/`ScreenshotJob` pipeline (`crates/kettle-render/src/lib.rs`),
> a `kettle-ctl` `Method::Screenshot`, an MCP `kettle_screenshot` tool
> (`crates/kettle/src/mcp_tools.rs`), and the focused-pane crop path.
> This is in addition to (not a replacement for) the pre-existing
> offscreen `kettle --screenshot=PATH` synthetic-scene renderer used
> for visual regression testing. This doc is kept as the historical
> design record; the phase roadmap below describes what was built. The original
> swapchain copy was later replaced with a one-shot offscreen scene target so
> screenshots work while Metal withholds drawables for occluded windows. It
> also removes the surface-`COPY_SRC` requirement used by the old path; native
> RDP/virtual-adapter completion remains a platform verification item.

## What it is

Terminator's `plugins/terminalshot.py` adds a right-click "Terminal
screenshot" menu item. Clicking opens a file-save dialog; on save it
calls `widget_pixbuf(terminal)` which grabs the GTK widget's current
pixel buffer (already on the host's GPU/CPU memory; cheap on GTK +
X11), scales to half-res, and writes PNG via GdkPixbuf.

End-state UX in kettle:

- A user binds `take_screenshot` to a chord (e.g. `Ctrl+Shift+P`) or
  picks "Take screenshot" from the right-click menu.
- kettle captures the focused pane's current rendered content + saves
  it as a PNG to `<cache>/kettle/shots/kettle-<unix-secs>-<pid>.png`
  (same path scheme as the `session_log_path` helper).
- A transient toast (using the existing notification surface) flashes
  the file path so the user can find it.

The `--screenshot=PATH` / `--screenshot-menu=PATH` CLI flags already
exist. Those render a *synthetic content-free debug scene*
headlessly — useful for visual regression testing but not for
"snapshot what I'm looking at right now." This design fills the live-
window readback gap.

## Why multiple phases

Three cross-cutting changes:

1. **wgpu scene readback**. kettle's renderer paints into the wgpu scene and
   presents it. To read it back we have to either:
   - Render into an intermediate texture + copy to a readable buffer
     + map-async + write PNG (most general, but adds a full extra
     render pass each screenshot).
   - Or hook into the existing `render_frame` path with an optional
     readback flag that fires once per screenshot trigger (lower
     overhead, but requires a state machine: queue screenshot request →
     next render copies the surface in its normal submission → a bounded
     worker waits, maps, and writes the PNG).
   - Original decision: the second approach. That fails when a compositor
     withholds the swapchain drawable (Metal occlusion) or a surface lacks
     `COPY_SRC` (some RDP/virtual adapters). The current path therefore renders
   one requested frame into a dedicated, process-budgeted transient target and
   copies that texture. The target and reservation stay with the worker until
   GPU completion, so a 6K capture can exceed the hostile-image cap without
   undercounting in-flight GPU memory. Ordinary frames keep the hot path
   unchanged.

2. **Per-pane vs full-window capture**. Terminator captures *the focused
   terminal widget*. kettle's renderer paints the whole window in one
   pass (no per-pane texture). Two sub-options:
   - **Per-pane crop**: capture the whole frame + crop to the focused
     pane's rect (which the renderer already computes for pane
     borders). Cheap; lossy at sub-pixel borders.
   - **Per-pane re-render**: render just the focused pane into an
     offscreen texture sized to its pixel rect. Pixel-perfect; more
     work.
   - Decision: per-pane crop. The user's expectation matches whatever
     they see on-screen; sub-pixel border slop is fine.

3. **Toast / notification path**. We already have notify-rust + the
   `fire_notify` desktop notification helper. The interactive action reports
   that the capture was queued and its destination; control-plane callers get
   the worker's exact completion result. A second in-flight request is rejected
   explicitly instead of replacing the first.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::app::App (Action::TakeScreenshot dispatch)                │
│                                                                      │
│  on action:                                                          │
│    renderer.set_pending_screenshot(ScreenshotRequest {               │
│        out_path: session_screenshot_path(unix_secs, pid, cache),     │
│        crop: self.mux.focused_pane_rect(),                           │
│        completion: optional control-plane sender,                    │
│    })?;  // BUSY if one is pending or in flight                      │
│    self.window.request_redraw();  ← forces next paint                │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_render::Renderer::render_frame (extended)                     │
│                                                                      │
│  if let Some(req) = pending_screenshot.take():                       │
│    reserve + create one transient offscreen scene target             │
│    encode scene → target; reserve + copy → capped staging buffer     │
│  acquire surface                                                     │
│    success: encode/present normal frame + capture in one submission  │
│    no drawable: submit the offscreen capture independently           │
│  hand {device, loss flag, submission, target+staging reservations,   │
│        staging, req}                                                  │
│    to one lazy capture worker                                         │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Screenshot worker (never the winit event-loop thread)                │
│  at most two wait slices → completion/loss or reset wedged device     │
│  map → CPU RGBA; drop target, staging buffer, and GPU reservations    │
│  bounded two-worker pool: crop → staged PNG → sync → publish         │
│  log result + answer optional control-plane completion sender         │
└──────────────────────────────────────────────────────────────────────┘
```

The queue is a single `Option<ScreenshotRequest>` owned on the event-loop
thread. The capture worker uses a capacity-one synchronous channel and atomic
busy latch. Each readback allocation is capped at 256 MiB. The worker polls in
five-second slices and retains both reservations until submission completion or
device loss; one slice timeout is not mistaken for resource retirement. Two
consecutive timeouts latch a device fault and destroy the wedged device so the
normal renderer-recovery path can clear admission safely. Once readback reaches
CPU memory, the GPU resources drop and a fixed two-worker persistence pool
performs crop/encode/sync/publication. This bounds retained CPU jobs while
letting a new capture complete when one cancelled filesystem operation stalls.

The PNG encoder is the same crate used by the existing `capture_png`
function — `image` is already a transitive dep via
kettle-render. Secure staging names use the existing lockfile's `getrandom`
package directly rather than adding a new dependency package.

## Phase roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `Action::TakeScreenshot` enum variant + aliases (`take_screenshot` / `terminalshot` / `take-screenshot`) + dispatch arm queues request (no render-side wiring yet) | Unit test on from_name + palette inclusion |
| 2 | `session_screenshot_path(unix_secs, pid, cache_dir)` pure helper (mirrors `session_log_path`) | Drift guard on path shape + relative-fallback |
| 3 | `Renderer::pending_screenshot` slot + getter/setter; render_frame branches to intermediate-texture path when set | Compiles + existing snapshot tests pass |
| 4 | wgpu copy_texture_to_buffer + map_async + PNG encode path | Mock wgpu via existing test infra? (may need manual e2e) |
| 5 | Toast notification on save success/fail | Manual e2e |
| 6 | Right-click context menu entry "Take screenshot" + per-pane crop | Manual e2e |
| 7 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +5 (helpers + dispatch); the wgpu pipeline is
hard to unit-test cleanly — relies on manual screenshot verification.

## What WON'T ship in v1

- **In-place annotation**. The `--annotate` flag adds an
  annotation overlay to the headless screenshot path. Wiring the
  annotation surface into the live-capture path is a follow-up
  (mostly composing existing pieces).
- **Per-tab vs per-window screenshot**. v1 ships the focused-pane
  capture. The whole-window capture is the existing `--screenshot`
  path; if the user wants live-window-whole-frame, they can
  alt-screen + use the OS screenshot tool. (kettle's window is just
  one wgpu surface; the OS tool sees it like any other window.)
- **Mouse-cursor inclusion**. Terminator's pixbuf grab doesn't include
  the OS cursor; kettle's matches. Document this explicitly.

## Acceptance test

```
$ kettle
# bind take_screenshot in config:
# keybind = ctrl+shift+p = take_screenshot

# run some commands in pane 1; split to pane 2; run a different command
# focus pane 1, press Ctrl+Shift+P
# verify: notification "Screenshot saved to ~/.cache/kettle/shots/..."
# open the file:
$ xdg-open ~/.cache/kettle/shots/kettle-*.png
# verify: PNG shows pane 1's content (not pane 2, not the tab bar)
```

## Risks + mitigations

- **Risk:** wgpu readback can be slow or a driver can stop completing work.
  **Mitigation:** one bounded worker owns two finite five-second waits; the
  winit event loop submits and presents without waiting. A second timeout
  resets the shared device rather than dropping in-flight accounting or
  permanently holding screenshot admission.
- **Risk:** repeated capture requests grow staging memory or overwrite a
  completion channel. **Mitigation:** exactly one GPU capture may be pending or
  in flight and at most two persistence jobs may exist; later work receives an
  explicit busy result.
- **Risk:** mapped-buffer cleanup when the user closes kettle mid-encode.
  **Mitigation:** the worker owns the job and staging buffer to completion; a
  process exit lets wgpu/the OS release them, with no detached borrowed state.
- **Risk:** image crate version skew with the headless `capture_png`
  path. **Mitigation:** share the encoder helper between the two
  paths — single image-crate import location.
