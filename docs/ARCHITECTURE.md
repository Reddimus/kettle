# kettle architecture

kettle is a Cargo workspace of focused crates. PTY bytes are split by an
**image-protocol extractor** before the VT engine sees them; the engine owns a
shared grid that the GPU renderer reads each frame; side-channels (prompts,
cwd, images, clipboard, title) flow back to the UI.

## Crates

```mermaid
graph TD
    bin["kettle (bin)<br/>CLI · entry"] --> ui
    ui["kettle-ui<br/>winit app · tab/split mux · input<br/>regex search · SSH launcher · command palette · session<br/>context menu · Preferences submenu (cycle 717)"] --> render
    ui --> core
    ui --> cfg
    ui --> remote
    render["kettle-render<br/>wgpu · glyphon text · quad &<br/>image/overlay pipelines · --screenshot · offscreen self-test"] --> core
    render --> cfg
    core["kettle-core<br/>portable-pty · alacritty_terminal+vte · reader thread<br/>regex/smart-case search · links · image/virtual/anim/relative registries"] --> vt
    core --> cfg
    cfg["kettle-config<br/>Ghostty config · ~512 themes · Nerd Font · keybinds<br/>bell · ssh-host · fuzzy matcher · command palette<br/>atomic persist_config_toggle (cycle 716)"]
    vt["kettle-vt<br/>Extractor: Sixel · iTerm2 · OSC 7/133<br/>kitty: store/place/delete/z · Unicode placeholders<br/>animation (frames/control/compositing) · relative placements"]
    remote["kettle-remote (cycle 643)<br/>SSH / Docker / Podman / kubectl / lxc detection<br/>sysinfo process-tree walk · format_remote_title<br/>kitty-@ control protocol surface"]
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
  animation schedules a ~30 fps redraw tick (otherwise idle, no CPU). The
  extractor caps in-flight sequences (64 MiB) so a hostile stream can't hang
  or OOM — the cap is security-relevant: an SSH session into a 256 MiB
  container can otherwise OOM-kill kettle by emitting unbounded image data.
- **Lua VM (cycle 324)** is parked on the App struct (single-threaded
  `LuaEngine`) — `mlua`'s `send` feature makes the handle `Send + Sync`
  but kettle never clones it across threads. Event hooks
  (`LuaEvent::Startup` / `TabAdd` / `TabClose` / `Bell` / `Output` /
  `PaneFocus` / `TitleChanged` / `UrlClicked` — cycles 365-705) fire
  synchronously on the App thread; Lua side-effects (`SendText`,
  `ExecAction`, `Notify`, `SetTheme`) queue onto
  `LuaEngine.pending: Arc<Mutex<Vec<LuaCommand>>>` and are drained
  back to the App's dispatcher each tick. A broken Lua plugin
  `log::warn`s and is skipped — it never aborts the terminal
  (cycle-365 "broken plugin can't take down kettle" contract).
- **Broadcast fan-out** (cycle 678 `BroadcastScope` enum) is App-side
  input dispatch, not a separate thread: on every keystroke the App
  walks `compute_broadcast_targets(scope, focus, in_tab, all)` and
  writes the encoded bytes to each target pane's PTY. The reader
  threads of the receiving panes pick up the echo through their
  normal byte-stream path.
- **Allocation hot-paths (cycle 727 audit)**: `App::drain_events`
  has 5 `.clone()`/`format!()` operations; `App::redraw` has 7.
  Each is load-bearing — `LuaEvent::Output(id, bytes)` copies the
  byte slice into a fresh `Vec<u8>` for the Lua callback (no
  shared ownership because mlua's `IntoLuaMulti` consumes the
  argument); `ContextMenuRow.label` clones the visible row text
  each frame the menu is open (~512 clones/frame in the worst
  case — Theme submenu drilled-in). The menu allocation is bounded
  by user interaction (only allocates while the menu is OPEN) so
  the steady-state allocator pressure is zero. A `Cow<'static, str>`
  refactor of `ContextMenuRow.label` is the natural next step if
  this ever shows up in a profile; today it's not measurable
  against winit's per-frame work.
- **Synchronization primitives audit (cycle 724)**: ~13 `unsafe`
  blocks total, all FFI (libc `sendmsg/recvmsg/SCM_RIGHTS`, signal
  setup, `pre_exec` for fd-3 plumbing, `UnixStream::from_raw_fd`
  adoption). Each is ≤10 lines, narrowly scoped, with a doc comment
  citing the ownership contract. No `transmute`, no raw-pointer
  abstractions, no `Send`/`Sync` impls outside the foreign-fd
  protocols. Per-pane `Arc<Mutex<...>>` are contended only on PTY
  read or App snapshot; lock-hold times are O(bytes) — measured at
  cycle X to stay under 100 µs per drain even on fast scrolling.

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

## Terminator-parity subsystems (cycles 330-410)

The v1.8.0 → v1.31.0 sweep added four major subsystems. Each has its
own design doc; the architectural integration is summarized here.
Cycles 411-553 (v1.32.0 → v1.43.0) hardened the plugin contract,
extended drift guards, surfaced opt-in keys via `--check-config`
echo lines, scrubbed internal cycle refs from every user-facing
doc surface (incl. binary stdout), corrected 3 stale field
doc-comments in `crates/kettle-ui/src/app.rs`, added an opt-in
pre-commit hook that catches clippy / fmt / test / shellcheck
regressions at commit time (`.githooks/pre-commit`), fixed a real
cycle-51-era backticks-as-command-substitution bug in
`scripts/release.sh`, and fixed a real user-reported icon-cache
bug in `scripts/install.sh` (broken stub from
gtk-update-icon-cache against a user-local hicolor dir with no
index.theme stopped GNOME's icon resolution) — see
[`docs/TERMINATOR-AUDIT.md`](TERMINATOR-AUDIT.md)'s post-sweep section
for that polish run.

Cycles 554-723 (v1.44.0 → current) added the cycle-643
`kettle-remote` crate (SSH / Docker / Podman / kubectl / lxc
detection via sysinfo process-tree walk, surfaced as a per-pane
title prefix + right-click "Clone session" entry), the cycle-678
named-broadcast-groups subsystem (`BroadcastScope` enum + per-tab
/ per-window / cross-tab named scopes), the cycle-687 right-click
context-menu drill-in submenu UX (Theme + Profile + cycle-717
Preferences), the cycle-665 vertical tab strips, and the
cycle-688 wgpu surface-readback screenshot path. Cycles 711-717
ran the user-facing right-click menu polish: hover-to-highlight,
disabled-row hiding, scrollable submenus (~512 themes fit
without overflowing), mnemonics + 750ms typeahead, atomic
config write-back via `persist_config_toggle`, and the
Preferences ▸ submenu wiring 13 runtime toggles to the
cycle-716 atomic-write helper. Cycles 718-723 closed out the
post-audit punch list: workspace-deps Cargo.toml refactor,
stale-version + stale-cycle-comment scrubs, magic-number
constants centralized in `kettle_render::menu`, 6 obsolete
`#[allow(dead_code)]` gates removed, CI nightly early-warning
job + release.yml pretest gate.

### Plugin system (cycles 324, 365-378)

```mermaid
flowchart TD
    A["init.lua (auto-load)"]
    A --> B["LuaEngine"]
    B -->|registers| C["kettle.on / notify / set_theme<br/>send_text / exec_action<br/>add_url_handler / add_menu_item"]
    B --> D["App.lua_engine"]
    D -->|fire_event| E["Startup · Bell · TabAdd · TabClose ·<br/>Output(bytes) ·<br/>PaneFocus(prev?, cur) (cycle 703) ·<br/>TitleChanged(pane, str) (cycle 704) ·<br/>UrlClicked(uri) (cycle 705)"]
    D --> F["LuaCommand queue"]
    F -->|drain| G["App dispatch:<br/>SendText · ExecAction ·<br/>Notify · SetTheme"]
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

```mermaid
flowchart LR
    A["cfg.background_image"] --> B["decode_bg_image<br/>(PNG / JPEG / WebP /<br/>BMP / GIF)"]
    B --> C["Optional box blur<br/>(cycle 396, 3-pass<br/>separable)"]
    C --> D["BgImage<br/>(Arc-cached by path)"]
    D --> E["imgpipe"]
    E --> F["Render BEFORE pane<br/>backgrounds with UV-mode<br/>dispatch (stretch /<br/>tile / center / scale)"]
```

Decoded at config-load (one-shot), kept in a path-keyed cache, rendered
via the cell-image pipeline. UV-modes + align-horiz/vert configurable.
See [`docs/TERMINATOR-BG-IMAGE-DESIGN.md`](TERMINATOR-BG-IMAGE-DESIGN.md).

### Detachable tabs (cycles 397-410)

Three paths, all end-to-end:

```mermaid
flowchart TD
    A["Action::MoveTabToNewWindow"]
    A --> B1["Wayland-fallback<br/>(keyboard-only)"]
    A --> B2["Unix SCM_RIGHTS<br/>socketpair + fork+exec<br/>+ send_fds (cycle 399)"]
    A --> B3["File-fallback<br/>/tmp/handoff.json<br/>+ --tab-handoff PATH"]
    B1 --> C["Target kettle"]
    B2 --> C
    B3 --> C
    C --> D["Session restore<br/>(split tree + cwds preserved)"]
```

In-process foundation: `Mux::serialize_tab` (cycle 397) +
`extract_tab`/`insert_tab` (cycle 398); IPC primitive:
`fd_transport::send_fds`/`recv_fds` (cycle 399); drag FSM:
`detach::DragState` (cycle 400). See
[`docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md`](TERMINATOR-DETACHABLE-TABS-DESIGN.md).
