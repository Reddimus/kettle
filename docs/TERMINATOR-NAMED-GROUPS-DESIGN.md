# Terminator named broadcast groups — design

> Status: design only. Named broadcast groups (Terminator
> `create_group` / `group_tab` / `group_win` / `ungroup_*`) require
> per-pane group-name state + a group-scoped broadcast policy. This
> doc lays out the architecture + phase roadmap. Same shape as
> the existing Bucket D design docs.

## What it is

Terminator lets a user create named *broadcast groups*: any pane can
be assigned to a group, and keyboard input fed to a pane in group `G`
is mirrored to every other pane in group `G`. Three scopes:

  - `group_tab`     — all panes in the active tab share a group
  - `group_win`     — all panes in the active window share a group
  - `create_group`  — prompt for a name, assign to that group

Plus the inverses (`ungroup_tab`, `ungroup_win`, `ungroup_all`).

End-state UX in kettle:

- A user splits into 4 panes, runs `ssh box1`, `ssh box2`, `ssh box3`,
  `ssh box4` in each. They want to run `apt update` on all four.
- They focus pane 1, press `Ctrl+Shift+G` → prompt "Group name?"
  → they type "fleet" + Enter. Pane 1's titlebar shows a `[fleet]`
  pill. Repeat for panes 2-4 (or use `group_tab` to one-shot all
  panes in the tab).
- They type `apt update`. The keystrokes mirror to all 4 panes.
- Done: focus another tab, press `Ctrl+Shift+Alt+G` → all groupings
  cleared.

kettle currently has *per-tab broadcast* (`BroadcastScope::Tab`) and
*broadcast-all* (`BroadcastScope::All`) but no *named groups*. Per-tab is "broadcast within this tab
only"; broadcast-all is "every pane in every tab." Named groups are
finer-grained: "every pane I tagged with `fleet`, even across tabs."

## Why this needs multiple changes

Three cross-cutting changes:

1. **Per-pane group state**. New `pub group: Option<String>` field on
   `Pane` (kettle-ui/mux). The `pane.group_name` field
   already exists as part of the title-edit overlay — this design
   *promotes* that field from "display-only" to "broadcast-scoping."
   Renames `group_name` → `group` to match Terminator vocabulary.

2. **Broadcast scope generalization**. Current scope:
   `BroadcastScope { Off, Tab, All }`. Extend to:
   `BroadcastScope { Off, Tab, All, Group(String) }`. The
   `Group(name)` variant scopes broadcast to all panes with
   `pane.group == Some(name)`.

3. **Group-management overlay**. The title-edit overlay
   (`TitleEditState`) already has a `Group` scope. Extend it to
   support:
   - Empty input → clear the group (ungroup_this_pane)
   - Non-empty input → set the group
   - A new "list all groups in use" hint at the bottom of the overlay
     so users can pick an existing name.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_config::Action  (existing variants, new actions added)        │
│                                                                      │
│  Action::GroupTab          — assign focused-tab panes to a group     │
│  Action::GroupWindow       — assign focused-window panes to a group  │
│  Action::CreateGroup       — open group-edit overlay (prompt)        │
│  Action::UngroupTab        — clear group on focused-tab panes        │
│  Action::UngroupWindow     — clear group on focused-window panes     │
│  Action::UngroupAll        — already exists                          │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::mux::Pane                                                 │
│                                                                      │
│  pub group: Option<String>     ← renamed from group_name             │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::mux::BroadcastScope                                       │
│                                                                      │
│  enum BroadcastScope {                                               │
│      Off, Tab, All,                                                  │
│      Group(String),  ← NEW                                           │
│  }                                                                   │
│                                                                      │
│  fn broadcast_targets(&self, focused: PaneId) -> Vec<PaneId>:        │
│      match self {                                                    │
│          Off => vec![focused],                                       │
│          Tab => (panes in active tab),                               │
│          All => (panes in every tab),                                │
│          Group(name) => (panes where pane.group == Some(name)),      │
│      }                                                               │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Renderer: titlebar pill for grouped panes                            │
│                                                                      │
│  When pane.group is Some(name), the titlebar shows `[name]` pill     │
│  in a color hash-derived from the name (so all "fleet" pills look    │
│  the same across the window).                                        │
└──────────────────────────────────────────────────────────────────────┘
```

## Phase roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | Rename `Pane::group_name` → `Pane::group` (mechanical) | existing tests pass |
| 2 | `BroadcastScope::Group(String)` variant + `broadcast_targets` impl | pure unit tests on synthetic Pane vecs |
| 3 | `Action::CreateGroup` dispatch — reuses the title-edit overlay with `TitleEditScope::Group`. Apply on Enter: writes input → focused-pane `group`. | drift guard on the apply path |
| 4 | `Action::GroupTab` / `GroupWindow` — prompt for a name then bulk-assign | drift guard |
| 5 | `Action::UngroupTab` / `UngroupWindow` — bulk-clear | drift guard |
| 6 | Renderer titlebar group-pill (deterministic color hash) | snapshot test on the pill placement |
| 7 | Right-click context menu items: "Set group...", "Clear group" | manual e2e |
| 8 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +8-10 (broadcast_targets edge cases + apply paths).

## What WON'T ship in v1

- **Cross-window groups**. Terminator has a single process so all
  windows share the group registry. kettle is single-window per
  process (kettle-ctl's IPC transport already connects multiple instances). v1 ships
  in-window groups only; cross-window grouping via IPC is a follow-up.
- **Persistent groups in session**. Restored sessions don't carry
  group assignments in v1 — the session.json format would
  need a `group` field per pane. A follow-up.
- **Group-scoped color**. Terminator's title pill takes a color from
  a fixed palette indexed by group hash. v1 ships the same hash-derived
  color; users wanting palette overrides get a follow-up.

## Acceptance test

```
$ kettle
# pane layout: 4 horizontal splits, ssh to box1..4 in each
# focus pane 1, Ctrl+Shift+G → type "fleet" → Enter
# verify: pane 1's titlebar shows [fleet] pill
# focus pane 2, Ctrl+Shift+G → type "fleet" → Enter
# focus pane 3, Ctrl+Shift+G → type "fleet" → Enter
# focus pane 4, Ctrl+Shift+G → type "fleet" → Enter
# now focus pane 1
# enable broadcast to group: Ctrl+Shift+E (or whatever the binding is)
# type "echo hello"
# verify: each of panes 2-4 also shows "echo hello"
# press Enter → all 4 ssh sessions run echo hello

# Ctrl+Shift+Alt+G (ungroup_all)
# verify: all 4 pills disappear; broadcast scope reverts to Off
```

## Risks + mitigations

- **Risk:** group names with special chars (newline, BEL) break
  rendering. **Mitigation:** sanitize on Apply — strip control chars
  + clamp to 32 chars (typical pill width).
- **Risk:** broadcast amplification — a user mass-groups 100 panes
  and types fast, each keystroke fans out 100x to the PTY layer.
  **Mitigation:** the existing per-pane PTY write queue already
  bounds backpressure; broadcast just iterates. Pre-existing
  protection from the broadcast-all path (`BroadcastScope::All`).
- **Risk:** group-name collisions with reserved scopes (`tab`, `all`,
  `off`). **Mitigation:** the typed name is data, not a config-key
  parse; collisions are impossible at the type level (`Group(String)`
  is distinct from `Tab` variant).
- **Risk:** title-edit overlay key bindings overlap with group-edit.
  **Mitigation:** `TitleEditScope::Group` already exists — same
  overlay, scoped variant. Reused, not duplicated.
