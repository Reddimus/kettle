# kettle architecture

kettle is a Cargo workspace of focused crates. PTY bytes are split by an
**image-protocol extractor** before the VT engine sees them; the engine owns a
shared grid that the GPU renderer reads each frame; side-channels (prompts,
cwd, images, clipboard, title) flow back to the UI.

## Crates

```mermaid
graph TD
    bin["kettle (bin)<br/>CLI · entry"] --> ui
    ui["kettle-ui<br/>winit app · tab/split mux · input<br/>regex search · SSH launcher · command palette · session"] --> render
    ui --> core
    ui --> cfg
    render["kettle-render<br/>wgpu · glyphon text · quad &<br/>image/overlay pipelines · --screenshot · offscreen self-test"] --> core
    render --> cfg
    core["kettle-core<br/>portable-pty · alacritty_terminal+vte · reader thread<br/>regex/smart-case search · links · image/virtual/anim/relative registries"] --> vt
    core --> cfg
    cfg["kettle-config<br/>Ghostty config · ~512 themes · Nerd Font · keybinds<br/>bell · ssh-host · fuzzy matcher · command palette"]
    vt["kettle-vt<br/>Extractor: Sixel · iTerm2 · OSC 7/133<br/>kitty: store/place/delete/z · Unicode placeholders<br/>animation (frames/control/compositing) · relative placements"]
```

## Per-pane data flow

```mermaid
sequenceDiagram
    participant Shell
    participant PTY as portable-pty
    participant Reader as reader thread
    participant Ext as kettle-vt Extractor
    participant VT as vte + alacritty Term
    participant Side as images/prompts/cwd
    participant Proxy as EventProxy
    participant UI as winit loop
    participant GPU as wgpu/glyphon

    Shell->>PTY: stdout bytes
    PTY->>Reader: read()
    Reader->>Ext: feed(bytes)
    Ext-->>Side: Image/DeleteImages/VirtualImage/Animation/<br/>RelativePlacement/Prompt(OSC133)/Cwd(OSC7)
    Ext->>VT: Pass(bytes) → Processor::advance(&mut Term)
    VT->>Proxy: Title/Bell/Clipboard/ColorRequest/PtyWrite/Wakeup
    Proxy->>UI: EventLoopProxy.send_event(Wakeup)
    UI->>UI: request_redraw() (coalesced)
    UI->>GPU: render_frame(panes, images+placeholder/relative tiles, tabbar, overlay)
    GPU->>UI: present
    UI->>PTY: key / mouse / paste / focus bytes
```

## kitty graphics pipeline

The biggest VT extension. Decoding lives in `kettle-vt::kitty` (pure,
heavily unit-tested); per-terminal registries live on `kettle-core::Terminal`
and are populated by the reader thread; the renderer reads them each frame.

```mermaid
graph LR
    apc["APC G payload"] --> kit["kittyState::feed"]
    kit -->|"a=t/T, a=p"| place["images registry<br/>(at cursor, z-ordered)"]
    kit -->|"a=p,U=1"| virt["virtuals registry<br/>rows×cols box"]
    kit -->|"a=f / a=a / a=c"| anim["anims registry<br/>frames + AnimationState"]
    kit -->|"a=p,P=,Q="| rel["relatives registry<br/>(parent, h, v)"]
    grid["U+10EEEE cells<br/>(fg=id, diacritics=row/col)"] --> ph["placeholder_tiles()<br/>resolve_run + tile crop"]
    virt --> ph
    anim --> clk["current_frame(clock)<br/>swaps Placement.img"]
    rel --> rt["relative_tiles()<br/>resolve_chain(depth≤8)"]
    grid --> rt
    place & ph & rt & clk --> draw["render_frame: image pipeline"]
```

## Render pass order

Each frame the renderer issues six passes against the same wgpu
render-pass encoder, in this order. The order matters: a quad pass
paints over text drawn before it, and text drawn after a quad covers
that quad's pixels.

```mermaid
flowchart LR
    clear["Clear color<br/>(theme bg + opacity)"] --> quads["1. quads.draw<br/>pane bg, tab bar,<br/>chrome quads"]
    quads --> imgs["2. imgs.draw<br/>sixel · kitty · iTerm2<br/>image overlays"]
    imgs --> text["3. text_renderer.render<br/>pane text + tab text<br/>(NOT menu rows)"]
    text --> overlay["4. overlay_quads.draw<br/>pane dimming · scrollbar<br/>(NOT menu chrome)"]
    overlay --> menuq["5. menu_quads.draw<br/>shadow · panel bg ·<br/>border · row highlight"]
    menuq --> menut["6. menu_text_renderer.render<br/>context menu<br/>row labels"]
```

Steps 5–6 own the right-click context menu so its labels land **on
top of** the panel background. Splitting them out fixed the v1.3.0 /
v1.3.1 blank-menu bug — the menu's opaque panel quad used to live in
step 4 (`overlay_quads`), painting over the menu text that had
already been rendered in step 3.

The 5–6 passes are cheap no-ops while the menu is closed (empty
buffer uploads). The two `TextRenderer` instances share the same
`TextAtlas` and `Viewport` — glyphon batches glyphs by atlas, not by
renderer, so the menu pass reuses already-cached glyphs.

## Threading model

- **Main thread** — winit event loop, *all* GPU work, the tab/split tree,
  input encoding, search/SSH overlays, session save/restore, cursor-blink and
  visual-bell timers (scheduled via `ControlFlow::WaitUntil` only while
  something animates, so an idle terminal does no work).
- **One reader thread per pane** — blocking `read()` on the PTY master →
  `Extractor::feed` → image/side-channel chunks recorded, text chunks driven
  into the `alacritty_terminal::Term` (behind a `Mutex` shared with the
  renderer) → wakes the UI.
- Output floods are coalesced by `request_redraw` (≤1 frame/vsync). Per-pane
  registries (`images`, `virtuals`, `anims`, `relatives`, `prompts`, `cwd`)
  are `Arc<Mutex<…>>` snapshotted cheaply for rendering; a running kitty
  animation schedules a ~30 fps redraw tick (otherwise idle). The extractor
  caps in-flight sequences (64 MiB) so a hostile stream can't hang or OOM.

## Why the extractor sits *in front of* the VT engine

`alacritty_terminal` has no image/graphics support and ignores OSC 7/133. The
`Extractor` is a small state machine that pulls Sixel (DCS), kitty (APC `G`)
and iTerm2 (`OSC 1337`) image sequences, plus OSC 7 (cwd) and OSC 133 (shell
integration), out of the byte stream and forwards everything else
**byte-for-byte** (terminator preserved: BEL vs ST) so the engine still sees a
correct, untouched VT stream. This keeps us on a battle-tested engine while
adding modern features it lacks.

## Key design choices

| Concern | Choice | Why |
|---|---|---|
| VT engine | `alacritty_terminal` + `vte` | Battle-tested vs vttest/vim/tmux; avoids re-deriving the xterm long tail. |
| Images/OSC | in-house `Extractor` ahead of the engine | Adds Sixel/kitty/iTerm2 + OSC 7/133 without forking the engine. |
| Text | `glyphon` (cosmic-text) | Pure-Rust shaping + fallback + GPU atlas; ligatures + Nerd glyphs. |
| Window/GPU | `winit` + `wgpu` | One codebase for X11/Wayland/Win32/Cocoa; offscreen self-test in CI. |
| PTY | `portable-pty` | Uniform Unix + Windows ConPTY. |
| Config | Ghostty `key = value` | Ships the Ghostty theme set verbatim; familiar to users. |

Correctness is guarded by an extensive workspace test suite —
end-to-end VT-conformance driving this exact `vte`+`alacritty_terminal`
path, plus pure-unit coverage of the kitty decoder / placeholder /
animation / relative logic, the fuzzy matcher and command palette.
See [TESTING.md](TESTING.md) for the per-crate breakdown
(run `cargo test --workspace` for today's count — it grows ~1/cycle).
Comparative analysis behind these choices (with citations) is in
[RESEARCH.md](RESEARCH.md) and [UX-COMPARISON.md](UX-COMPARISON.md).

## Terminator-parity subsystems (cycles 330-415)

The v1.8.0 → v1.32.0 sweep added four major subsystems. Each has its
own design doc; the architectural integration is summarized here.

### Plugin system (cycles 324, 365-378)

```
init.lua  ──auto-load──▶  LuaEngine ──registers──▶  on/notify/set_theme/
                            │                       send_text/exec_action/
                            │                       add_url_handler/
                            │                       add_menu_item
                            ▼
                       App.lua_engine ──fire_event──▶ Startup, Bell,
                                                      TabAdd, TabClose,
                                                      Output(bytes)
                            │
                            ▼
                     LuaCommand queue ──drain──▶  App dispatch (SendText,
                                                  ExecAction, Notify,
                                                  SetTheme)
```

`lua-sandbox = safe` (default) nils unsafe stdlib APIs (os.execute,
io.open, etc); `trusted` mode opt-in. See
[`docs/TERMINATOR-PLUGIN-DESIGN.md`](TERMINATOR-PLUGIN-DESIGN.md).

### Per-pane titlebar (cycles 379-407)

Renders ONLY when a tab has >1 pane (single-pane tab uses the OS
window title). Layout:

```
┌─────────────────────────────────────────────────┐  ← per-pane bar
│  [group] pane title   80x24   🔔                │     (top OR bottom
├─────────────────────────────────────────────────┤      per cfg)
│                                                 │
│              cell content                       │  ← cell-grid render
│              (shifted by bar height)            │
│                                                 │
└─────────────────────────────────────────────────┘
```

Three color variants based on broadcast state: transmit (focused
source), receive (group member), inactive (idle). Click on the bar
focuses the pane; click again opens `EditPaneTitle`. `EditPaneGroup`
action edits the broadcast-group label. See
[`docs/TERMINATOR-PANE-TITLEBAR-DESIGN.md`](TERMINATOR-PANE-TITLEBAR-DESIGN.md).

### Background image (cycles 380-396)

```
cfg.background_image  ──decode_bg_image──▶  BgImage  ──Arc-cached──▶  imgpipe
   │                       │                  │                          │
   ▼                       ▼                  ▼                          ▼
PNG/JPEG/WebP/      Optional box blur     Render BEFORE pane     Fullscreen quad
BMP/GIF              (cycle 396)           backgrounds            UV-mode dispatch
                                                                  (stretch / tile /
                                                                   center / scale)
```

Decoded at config-load (one-shot), kept in a path-keyed cache, rendered
via the cell-image pipeline. UV-modes + align-horiz/vert configurable.
See [`docs/TERMINATOR-BG-IMAGE-DESIGN.md`](TERMINATOR-BG-IMAGE-DESIGN.md).

### Detachable tabs (cycles 397-410)

Two paths, both end-to-end:

```
                    Action::MoveTabToNewWindow
                              │
            ┌─────────────────┼──────────────────┐
            ▼                 ▼                  ▼
       Wayland-fallback   Unix SCM_RIGHTS    File-fallback
       (keyboard only)    socketpair + fork  /tmp/handoff.json
                          + send_fds         + --tab-handoff PATH
            │                 │                  │
            └────────┬────────┴──────────────────┘
                     ▼
            Target kettle (recv_fds OR load_tab_handoff)
                     ▼
            Session restore → user sees split tree + cwds
```

In-process foundation: `Mux::serialize_tab` (cycle 397) +
`extract_tab`/`insert_tab` (cycle 398); IPC primitive:
`fd_transport::send_fds`/`recv_fds` (cycle 399); drag FSM:
`detach::DragState` (cycle 400). See
[`docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md`](TERMINATOR-DETACHABLE-TABS-DESIGN.md).
