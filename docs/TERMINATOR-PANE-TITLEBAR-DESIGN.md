# Per-pane titlebar — design

> Status: design only (cycle 362). End-state implementation is multi-cycle
> because it touches layout math, render order, hit-testing, focus +
> group indicators, and the cycle-354 Edit*Title overlay.

## What it is

Terminator's `terminatorlib/titlebar.py` shows a thin per-pane bar above
each terminal containing:

  - The pane title (OSC 0/2 from the shell, or user-set via Edit Title).
  - The current size (`WxH` cells), optionally hidden via `title_hide_sizetext`.
  - An activity/bell icon (per-pane, vs kettle's per-tab).
  - The broadcast-group label (editable inline; click to assign).
  - Custom colors (transmit/receive/inactive variants).

Kettle today shows ONE title bar — the global window title (cycle-X
`window-title-format`) — plus the tab-bar with per-tab activity dots.
Per-pane titlebars surface the same info AT THE PANE level.

## Why it's multi-cycle

  1. **Layout math change.** Today, each pane's content area is the
     full rectangle minus padding. Adding a titlebar above each pane
     means subtracting `titlebar_height` from every pane's content
     area + propagating that to the alacritty_terminal grid sizing.
     The split-tree layout has to know about titlebar height to
     compute correct child rects.

  2. **Render order.** Today: tab-bar (top) → panes → status-bar
     (bottom) → modals. With per-pane titlebars: each pane gets a
     mini-bar drawn ABOVE its content rect. Three new quad
     batches per pane (background, accent, icon) + one text area
     for the title. Multiplies the per-frame render cost by
     `num_panes_per_tab`.

  3. **Hit-testing.** Clicks on the titlebar should NOT pass through
     to the pane's content (cursor positioning would land in
     row 0). The titlebar's clickable regions (group label, close
     ✕ if shown, drag region for tab detach) need their own
     hit-tests in `App::on_mouse_input`.

  4. **Focus + group indicators.** Three color variants
     (`title_transmit_*_color`, `title_receive_*_color`,
     `title_inactive_*_color`) require knowing which panes are in
     which broadcast group + whether they're "transmitting" (focused)
     or "receiving" (group member, focused elsewhere). This is the
     first place broadcast_group identity actually matters in
     the render layer.

  5. **Edit-title overlay.** Cycle 354 ships placeholder direct
     writes; the interactive overlay (text input + cursor + Enter
     to apply) lives here. The titlebar becomes its own input
     widget when the user is editing.

## End-state UX

```
┌────────────────────────────────────────────────────────────────┐
│  [tab1] [tab2 ●] [tab3 *]                                    + │  ← tab bar (cycle-X)
├────────────────────────────────────────────────────────────────┤
│ vim notes.md             80×24       [g1]              ●       │  ← per-pane titlebar
│                                                                │     (top-left: title;
│  # My notes                                                    │      center: size text;
│                                                                │      right of center:
│                                                                │      group label;
│                                                                │      right: activity dot)
├────────────────────────────────────────────────────────────────┤
│ ssh server.example       80×24       [g1]              *       │  ← second pane's titlebar
│                                                                │     bell icon shown,
│  $ tail -f /var/log...                                         │     bg-color = "receive"
│                                                                │     (group member,
│                                                                │      receiving broadcast)
└────────────────────────────────────────────────────────────────┘
```

## Architecture

```mermaid
graph TB
    A[Config: show_titlebar=true] --> B[Pane layout math:<br/>subtract titlebar_h<br/>from content rect]
    B --> C[Renderer.build_pane:<br/>3 new quad batches +<br/>1 text area per pane]
    D[Mux.focused] --> E[Color variant selector:<br/>transmit/receive/inactive]
    E --> C
    F[Broadcast group] --> E
    G[On click in titlebar rect] --> H{Region?}
    H -->|title text| I[Action::EditPaneTitle<br/>cycle 354 wires;<br/>overlay enters edit mode]
    H -->|group label| J[Group-assign overlay]
    H -->|drag region| K[Detachable tabs<br/>cycle DETACHABLE-TABS-DESIGN]
```

### Files affected

  - `crates/kettle-render/src/lib.rs`: new `build_pane_titlebar(...)`
    method; `build_pane` reduces content rect by `titlebar_h`.
  - `crates/kettle-ui/src/mux.rs`: `Pane.broadcast_group: Option<String>`
    field (set by `Action::CreateGroup`; Terminator-parity Bucket-C item
    already promoted to D scope because of this dependency).
  - `crates/kettle-ui/src/app.rs`: `on_mouse_input` adds hit-test for
    the per-pane titlebar region; `handle_action` for `Action::Edit*Title`
    flips a new `editing_title: Option<TitleEditState>` field.
  - `crates/kettle-config/src/lib.rs`: existing title-* config fields
    (cycle 340) get consumed by the render layer.

## Sub-cycle roadmap

| # | Scope | Status |
|---|------|--------|
| 1 | This doc (362). Design + roadmap. No code. | ✅ |
| 2 | `Pane.titlebar_height` field on Pane (computed from cfg.title_font metrics at startup + on config reload). Subtract from pane content rect in layout math; verify grid sizes match. | pending |
| 3 | Renderer: `build_pane_titlebar(pane, focused, group)` emits the 3-quad chrome + 1 title-text area. Wire from `build_pane`. Honors `title_hide_sizetext`. | pending |
| 4 | Color variants: select fg/bg from `cfg.title_{transmit,receive,inactive}_{fg,bg}_color` based on focused + broadcast-group membership. | pending |
| 5 | Hit-testing: clicks on the titlebar rect intercepted in `on_mouse_input` before pane mouse-tracking. New `PaneRegion::Titlebar` discriminator. | pending |
| 6 | Activity dot per-pane (mirrors cycle-246's tab-bar dot). Reuse the existing per-pane `last_output_at` tracking; new dot quad inside the titlebar. | pending |
| 7 | Edit-title overlay: `editing_title: Option<TitleEditState>` field; KeyboardInput handler dispatches to it before normal key encoding when active. Enter applies + clears; Esc clears. | pending |
| 8 | Inline group label edit: same shape as Edit-title but writes to `Pane.broadcast_group`. | pending |
| 9 | `title_at_bottom` config wired: render the titlebar BELOW the pane content rect instead of above. | pending |
| 10 | Bottom-of-document acceptance test: launch kettle with `show-titlebar = true`, capture `--screenshot`, assert N pixel-stripes match the expected color sequence (focused vs group-member vs unfocused). | pending |

## Architecture choices (rationale)

### Why a per-pane field, not a renderer-only concept

Pane titlebar visibility might want to be per-pane in the future
(some panes show, some hide). Modeling `Pane.show_titlebar: Option<bool>`
that overrides `cfg.show_titlebar` keeps the door open. For now,
config is the single source.

### Why icon_bell renders ONLY in the titlebar

Terminator's `icon_bell` toggles whether the bell triggers a titlebar
icon. kettle today maps bell → tab-bar dot (per-tab). With per-pane
titlebars, the bell can render per-pane (the existing per-pane
bell-state in cycle-246 generalizes).

### Why bottom rendering is a config knob, not a separate code path

`title_at_bottom = true` swaps the titlebar's y-offset from "above
content rect" to "below content rect". One render-position
parameter; no code duplication.

### Why drag-to-detach isn't here

Cross-window drag-and-drop (Terminator's `detachable_tabs`) needs
this titlebar's drag-region as the trigger, but the cross-window
state machine + IPC lives in its own Bucket-D thread (see
`docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md`). Sub-cycle 5's
hit-testing reserves the drag region; the actual detach implementation
follows separately.

## Acceptance test

End-to-end ship-criteria:

```bash
# Launch with per-pane titlebars enabled, 2 panes vertically split, both
# in broadcast group "g1", focus on top pane:
kettle --config <(cat <<EOF
show-titlebar = true
title-transmit-bg-color = #c80003
title-receive-bg-color = #0076c9
title-inactive-bg-color = #c0bebf
EOF
) &

# Send a bell to the unfocused pane:
kettle --remote-send "\\a" --pane 2

# Capture + assert:
kettle --screenshot /tmp/out.png
python3 verify_titlebar_colors.py /tmp/out.png  # checks 3 row-stripes
```

`verify_titlebar_colors.py` (a small fixture script) reads pixels at
known y-offsets in the PNG and asserts they match the cfg colors for
each pane's state.

## See also

- Terminator's titlebar.py: <https://github.com/gnome-terminator/terminator/blob/master/terminatorlib/titlebar.py>
- iTerm2 per-pane title bar:
  <https://iterm2.com/documentation-preferences-profiles-general.html>
