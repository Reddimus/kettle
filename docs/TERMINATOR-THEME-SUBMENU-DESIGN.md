# Terminator right-click theme submenu — design

> Status: design only. The context menu
> currently flat-lists Item + Separator + LuaItem + ConfigItem.
> Adding a submenu for theme picking — a Terminator UX pattern
> (`terminal_popup_menu.py`) — needs a hierarchical menu structure
> + nested-render path + hover-to-open flyout.

## What it is

Terminator's right-click context menu has a "Profiles" submenu listing
every configured profile; clicking one switches the focused pane's
profile. kettle's profiles are a runtime concept but
there's no submenu UX for picking one — currently users either:
  - launch kettle with `--profile NAME`
  - bind `next_profile` / `prev_profile` to a chord
  - use the command palette (full-screen overlay)

A flyout submenu in the context menu would be the most discoverable
UX for casual users: right-click → hover "Theme" → pick from list →
done.

End-state UX in kettle:

- A user right-clicks anywhere in a pane.
- The existing context menu opens. Cursor hovers "Theme ▸".
- After ~250 ms (hover delay), a side flyout appears listing themes
  (the same ~512 from `kettle --list-themes`). Scrollable if longer
  than viewport.
- Cursor moves into the flyout, clicks "Catppuccin Mocha". Theme
  applies immediately, menu closes.

Same pattern for "Profile ▸" (lists `<config-dir>/profiles/*.config`)
and could be reused for Lua-registered nested menus as a
follow-up.

## Why multiple changes

Three cross-cutting changes:

1. **`ContextMenuItem::Submenu` variant**. The current enum has flat
   variants:
   ```rust
   enum ContextMenuItem {
       Item { label, action, enabled },
       Separator,
       LuaItem { label, lua_idx },
       ConfigItem { label, command },
   }
   ```
   Add:
   ```rust
   Submenu { label: &'static str, items: Vec<ContextMenuItem> },
   ```
   The variant carries its own item list, recursive (so a Lua plugin
   could nest further if it wanted).

2. **Renderer: flyout layout + clip**. Today's context menu is one
   anchored panel. The flyout is a *second* anchored panel positioned
   to the right of the parent panel (or left if it'd clip the
   screen edge). Both panels need to render together; only the
   submenu's items respond to clicks while it's open.

3. **State machine: parent vs submenu focus**. The
   `ContextMenuState` has a single `highlight: usize`. Extend to
   `(parent_highlight, submenu_highlight: Option<usize>)`. Hover
   on a `Submenu` row + delay → opens; hover off + delay → closes.
   Keyboard nav: `←` closes the submenu, `→` opens it.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::app::ContextMenuItem (existing enum, new variant)         │
│                                                                      │
│  enum ContextMenuItem {                                              │
│      Item { label, action, enabled },                                │
│      Separator,                                                      │
│      LuaItem { label, lua_idx },                                     │
│      ConfigItem { label, command },                                  │
│      Submenu { label, items: Vec<ContextMenuItem> },  ← NEW          │
│  }                                                                   │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ ContextMenuState (existing, extended)                                │
│                                                                      │
│  anchor: (f32, f32),                                                 │
│  items: Vec<ContextMenuItem>,                                        │
│  highlight: usize,                                                   │
│  submenu_open: Option<SubmenuState>,  ← NEW                          │
│  hover_at: Option<(usize, Instant)>,  ← NEW (for delay)              │
│                                                                      │
│  pub struct SubmenuState {                                           │
│      parent_idx: usize,                                              │
│      anchor: (f32, f32),                                             │
│      items: Vec<ContextMenuItem>,                                    │
│      highlight: usize,                                               │
│  }                                                                   │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Renderer: paint_context_menu (existing) + paint_submenu (NEW)        │
│                                                                      │
│  When state.submenu_open is Some, paint two panels:                  │
│      1. parent panel (existing path, but with `▸` glyph on Submenu)  │
│      2. flyout panel at submenu.anchor                               │
│                                                                      │
│  Flyout anchor = parent_panel.right + 4 px, parent_row.top.          │
│  Clip to window edge: if anchor.x + panel.w > window.w,              │
│      flip to parent_panel.left - panel.w - 4 (open on the left).     │
└──────────────────────────────────────────────────────────────────────┘
```

## Phased roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `ContextMenuItem::Submenu` variant + `item_is_dispatchable` + `max_chars` + `panel_h` updates to handle the new variant (no submenu opening yet — just renders `▸` glyph for now) | Drift guard on item-list traversal |
| 2 | `SubmenuState` + hover delay state machine (~250 ms) | Pure unit test on the state transitions |
| 3 | Renderer `paint_submenu` — second panel, right of parent | Snapshot test on the flyout layout |
| 4 | Click dispatch: when submenu is open, click on a flyout item dispatches that item's action | Drift guard on the routing |
| 5 | Keyboard nav: `→` opens submenu, `←` closes, `↑↓` within active panel | Drift guard on keys |
| 6 | Window-edge clipping (flip to left when right would overflow) | Pure unit test on the flip math |
| 7 | Populate "Theme ▸" submenu from `Theme::list()` | Manual e2e |
| 8 | Populate "Profile ▸" submenu from `Config::list_profiles()` | Manual e2e |
| 9 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +6-8 (state-machine + clip math + dispatch
routing; the renderer paths use snapshot tests).

## What WON'T ship in v1

- **Nested-nested submenus**. `Submenu { items: vec![Submenu { … }] }`
  is technically possible with the recursive enum, but v1 only renders
  one level deep. Phases 1-9 hard-code single-level traversal.
- **Search-within-submenu**. The command palette already
  has fuzzy search across themes/profiles/actions; users who want
  search go there. Submenu is for quick browsing.
- **Keyboard-only operation**. The submenu is mouse-first (hover-
  to-open) with keyboard fallback. A pure-keyboard accelerator like
  Alt+T would be a follow-up.

## Acceptance test

```
$ kettle
# right-click in a pane
# verify: menu shows Copy / Paste / Split Right / ... / Theme ▸ / Profile ▸
# hover the "Theme ▸" row
# wait ~250 ms
# verify: flyout opens to the right listing all themes
# move cursor down the flyout, hover "Catppuccin Mocha"
# click
# verify: theme switches to Catppuccin Mocha
# right-click again, hover "Profile ▸"
# verify: flyout lists every <config-dir>/profiles/*.config name
# click "dev"
# verify: profile reloads (same as Action::NextProfile to "dev")

# Edge clipping:
# right-click near the right edge of the window
# hover "Theme ▸"
# verify: flyout opens to the LEFT instead of the right
```

## Risks + mitigations

- **Risk:** ~512 themes in a flyout overflow the viewport.
  **Mitigation:** the flyout panel has max-height = window.h - 40 px
  with a scrollable inner region (extends the existing
  hint-mode scroll machinery). Themes alphabetical;
  type-ahead jumps to first match.
- **Risk:** hover-delay timing varies across machines / OSes.
  **Mitigation:** 250 ms is the standard GNOME / KDE / macOS
  context-menu submenu delay; pinned in code, no user override
  (over-config is a worse UX than a single sensible default).
- **Risk:** the `Submenu` variant adds an `O(items_in_submenu)` cost
  to context-menu population. **Mitigation:** themes (~512) is the
  largest realistic submenu; `Theme::list()` is O(n) and runs
  once per menu open. Acceptable; not a hot path.
- **Risk:** click-outside-flyout-but-inside-parent-panel ambiguity.
  **Mitigation:** explicit hit-test order: flyout first (if open),
  parent panel second, outside both → close both.
