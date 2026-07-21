# Terminator `tab-position = left / right` (vertical tabs) — design

> Status: design only. The parser already accepts the
> values, and the runtime already logs a warn-fallback to top for
> them; the render-layout change is a real chunk of work. This doc
> lays out the architecture + phased roadmap.

## What it is

Terminator's `tab_position` accepts `top`, `bottom`, `left`, `right`,
`hidden`. kettle ships top, bottom, and hidden. `left`/`right` move
the tab strip to a vertical layout on the side of the window, with
each tab as a stacked row.

End-state UX in kettle:

- A user sets `tab-position = left` in their config.
- The tab strip moves to the left edge of the window, ~180 px wide.
- Tab "segments" are now horizontal rows stacked top-to-bottom (each
  ~`tab_bar_h` high, same as today's horizontal bar height).
- Tab titles, activity dots, broadcast indicator, close ✕ all
  render the same; just rotated 90° (conceptually — actually
  laid out vertically, not rotated text).
- The pane content area shrinks horizontally by the strip width;
  pane resize / split logic uses the new content rect.
- `tab-position = right` is the mirror.

## Why multi-phase

Three cross-cutting changes:

1. **Layout-rect plumbing**. Every `pane.rect()` / `mux.content_rect()`
   call currently computes from `(0, tab_bar_h)..(window_w, window_h)`.
   For vertical tabs the content rect becomes
   `(tab_strip_w, 0)..(window_w, window_h)` (left mode) or
   `(0, 0)..(window_w - tab_strip_w, window_h)` (right mode).
   Touch points: ~12 call sites across mux + app + renderer.

2. **Tab-bar render rewrite**. The existing `compute_tab_segment_widths`
   helper assumes a horizontal strip + n equal segments. For vertical:
   - segment width = fixed strip width (~180 px)
   - segment height = `tab_bar_h` (constant)
   - layout is stacked vertically, not flowed horizontally
   - close-button + activity-dot positions flip from
     "right edge of segment" to "right edge of strip"
   - drag-reorder uses y-axis instead of x-axis

   The renderer's `paint_tab_bar` would gain a `TabBarOrientation`
   parameter (Horizontal / VerticalLeft / VerticalRight) and branch
   the layout math. Most rendering primitives (rect, text, image)
   are orientation-agnostic; only the layout-loop changes.

3. **Click hit-testing**. `cursor_in_tab_bar`, `tab_seg_at_cursor`,
   `tab_close_at_cursor` all hit-test against horizontal rects.
   Vertical mode flips the math:
   - `cursor_in_tab_bar`: cursor.x in [0, strip_w] (left) or
     [window_w - strip_w, window_w] (right) instead of cursor.y
     in [0, bar_h] / [window_h - bar_h, window_h]
   - `tab_seg_at_cursor`: index from cursor.y / segment_h instead
     of cursor.x / segment_w
   - close-button hit: relative to the row's top-right corner

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_config::TabBarPos (existing enum, expanded)                   │
│                                                                      │
│  pub enum TabBarPos {                                                │
│      Top, Bottom,                                                    │
│      Left,   ← NEW                                                   │
│      Right,  ← NEW                                                   │
│  }                                                                   │
│                                                                      │
│  Parser already accepts the values; the runtime                      │
│  log::warn falls through to Top. This change removes the warn        │
│  + wires the real layout.                                            │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::app::App::content_rect (new helper)                       │
│                                                                      │
│  fn content_rect(&self) -> Rect:                                     │
│      let strip = self.tab_bar_strip();                               │
│      match (self.cfg.tab_bar_pos, self.tab_bar_visible()) {          │
│          (Top,    true)  => (0, strip.h)..(w, h),                    │
│          (Bottom, true)  => (0, 0)..(w, h - strip.h),                │
│          (Left,   true)  => (strip.w, 0)..(w, h),                    │
│          (Right,  true)  => (0, 0)..(w - strip.w, h),                │
│          (_, false)      => (0, 0)..(w, h),                          │
│      }                                                               │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_render::TabBar (existing) + TabBarOrientation (NEW)           │
│                                                                      │
│  pub enum TabBarOrientation { Horizontal, VerticalLeft, VerticalRight }│
│                                                                      │
│  paint_tab_bar(&mut self, bar: &TabBar, orient: TabBarOrientation)   │
│      branches the per-segment layout loop.                           │
└──────────────────────────────────────────────────────────────────────┘
```

## Phased roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `TabBarPos::Left` + `::Right` variants; parser already accepts them. Default `Top` preserved. | Drift guard on round-trip |
| 2 | `App::content_rect()` pure helper that computes the pane-content rect from `(tab_bar_pos, tab_bar_visible, window_size)`. Plumb to all current callers. | Pure unit test on the 8 (4×2) cases |
| 3 | `kettle_render::TabBarOrientation` enum + `paint_tab_bar` orientation parameter. Existing horizontal path becomes `Horizontal`. | Snapshot test on the existing horizontal output (regression) |
| 4 | Vertical layout in `paint_tab_bar` for `VerticalLeft` + `VerticalRight` | Snapshot test on the new vertical output |
| 5 | Hit-testing flip — `cursor_in_tab_bar`, `tab_seg_at_cursor`, `tab_close_at_cursor` branch on orientation. | Pure unit tests on the new hit math |
| 6 | Drag-reorder y-axis support | Drift guard on the drag-axis branch |
| 7 | Strip width config knob: `tab-bar-width = 180` (pixels). Default 180 for vertical, ignored for horizontal. | Drift guard on parser |
| 8 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +10-12 (the layout + hit-test paths are
nicely unit-testable; the renderer paths get snapshot coverage).

## What WON'T ship in v1

- **Per-tab vertical width**. v1 uses fixed strip width
  (`tab-bar-width = 180`). Horizontal tabs always use equal-width segments.
- **Vertical-text titles**. Titles render left-to-right in the
  vertical strip (Firefox-style sidebar, not rotated). Truncation
  with ellipsis kicks in past strip width.
- **Auto-orient on aspect ratio**. v1 honors only the explicit
  config key. Auto-switching to vertical on tall windows (1080x1920
  rotated) is a follow-up if users ask.

## Acceptance test

```
# In ~/.config/kettle/config:
tab-position = left

$ kettle
# verify: tab strip is on the left edge, ~180 px wide
# verify: tab segments are stacked top-to-bottom
# create 3 more tabs (Ctrl+T x3)
# verify: 4 tabs visible, each ~bar_h tall
# click a tab in the strip
# verify: focus switches to that tab
# drag a tab in the strip up/down
# verify: tab reorders by the drag direction
# resize the window narrower
# verify: pane content shrinks; strip stays at 180 px
# focus a pane and split right
# verify: split happens in the content rect (not under the strip)
```

Same flow with `tab-position = right`.

## Risks + mitigations

- **Risk:** existing horizontal-only assumptions leak through tests.
  **Mitigation:** phase 3 is "no behavior change" — just adds
  the orientation enum + threads it. Existing snapshot tests run
  with `Horizontal` and must stay green.
- **Risk:** hit-test math gets the +/- wrong on `Right` mode.
  **Mitigation:** pure unit tests in phase 5 cover both Left
  and Right with synthetic cursors at edge positions.
- **Risk:** vertical strip eats horizontal pixel budget on narrow
  windows (e.g., 80-column-wide). **Mitigation:** strip width is
  configurable (phase 7); user with narrow terminal
  picks `tab-position = top` or smaller `tab-bar-width`.
- **Risk:** Drag-reorder feedback (the drag-cursor-x
  ghost preview) only knows about x-axis. **Mitigation:** phase
  6 generalizes to `drag_cursor_axis` (x for horizontal, y for
  vertical).
