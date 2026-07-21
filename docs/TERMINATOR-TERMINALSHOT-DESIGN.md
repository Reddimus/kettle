# Terminator `terminalshot.py` port — design

> Status: design only. The runtime live-window surface
> readback machinery spans more than one phase of work, so this doc
> lays out the architecture + phase roadmap. Same shape as
> [`TERMINATOR-REMOTE-DESIGN.md`](TERMINATOR-REMOTE-DESIGN.md),
> [`TERMINATOR-DETACHABLE-TABS-DESIGN.md`](TERMINATOR-DETACHABLE-TABS-DESIGN.md),
> [`TERMINATOR-PANE-TITLEBAR-DESIGN.md`](TERMINATOR-PANE-TITLEBAR-DESIGN.md),
> [`TERMINATOR-BG-IMAGE-DESIGN.md`](TERMINATOR-BG-IMAGE-DESIGN.md).

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

1. **wgpu surface readback**. kettle's renderer paints into the wgpu
   swap-chain texture and presents. To read it back we have to either:
   - Render into an intermediate texture + copy to a readable buffer
     + map-async + write PNG (most general, but adds a full extra
     render pass each screenshot).
   - Or hook into the existing `render_frame` path with an optional
     readback flag that fires once per screenshot trigger (lower
     overhead, but requires a state machine: queue screenshot request →
     next render copies the surface in its normal submission → a bounded
     worker waits, maps, and writes the PNG).
   - Decision: the second approach. One frame's worth of latency is
     fine for a user-triggered screenshot. The hot path stays unchanged.

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
│    copy the acquired surface into a capped staging buffer            │
│    queue.submit(draw + copy)                                         │
│    hand {device, submission, staging, req} to one lazy worker        │
│    call Window::pre_present_notify, then present                      │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Screenshot worker (never the winit event-loop thread)                │
│  finite GPU wait → map → validate rows → BGRA/RGBA → crop → PNG      │
│  log result + answer optional control-plane completion sender         │
└──────────────────────────────────────────────────────────────────────┘
```

The queue is a single `Option<ScreenshotRequest>` owned on the event-loop
thread. The worker uses a capacity-one synchronous channel and atomic busy
latch. Readback is capped at 256 MiB and the GPU wait at five seconds.

The PNG encoder is the same crate used by the existing `capture_png`
function — `image` is already a transitive dep via
kettle-render. No new deps.

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
  **Mitigation:** one bounded worker owns a finite five-second wait; the winit
  event loop submits and presents without waiting.
- **Risk:** repeated capture requests grow staging memory or overwrite a
  completion channel. **Mitigation:** exactly one request may be pending or in
  flight; later callers receive a busy result.
- **Risk:** mapped-buffer cleanup when the user closes kettle mid-encode.
  **Mitigation:** the worker owns the job and staging buffer to completion; a
  process exit lets wgpu/the OS release them, with no detached borrowed state.
- **Risk:** image crate version skew with the headless `capture_png`
  path. **Mitigation:** share the encoder helper between the two
  paths — single image-crate import location.
