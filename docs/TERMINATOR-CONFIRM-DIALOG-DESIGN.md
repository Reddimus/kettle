# Terminator `ask_before_closing` — confirm-dialog primitive design

> Status: shipped as the close-confirmation primitive. The config key +
> `AskBeforeClosing` enum are wired, CloseWindow / CloseTab / ClosePane route
> through the prompt, keyboard navigation is supported, and mouse hit-testing
> for the visible bottom-bar buttons is wired up. A centered modal panel
> remains optional renderer polish; the functional `ask_before_closing` path is
> complete.

## What it is

Terminator's `ask_before_closing` config has three values:

  - `never`              — close without prompt
  - `multiple_terminals` — prompt only when >1 pane exists (default)
  - `always`             — always prompt

End-state UX in kettle:

- A user has 4 panes open (1 tab, 4-way split). They press
  `Ctrl+Shift+Q` (CloseWindow).
- With `ask-before-closing = multiple_terminals` (default), a modal
  overlay appears asking "Close 4 panes?" with two buttons:
  `[Cancel]` / `[Close]`. Default focus is Cancel (safe default).
- Click Close or focus Close and press Enter → window closes.
- Click Cancel, focus Cancel and press Enter, or press Escape → modal closes,
  no action.

The primitive is reusable:
- "Killing a running process — proceed?" (when a pane has
  a non-shell foreground process)
- "Unsaved layout — discard?" (when reload would lose
  uncommitted layout edits)
- "Reset config to defaults?" (`--reset-config` future flag)

## Why multiple phases

Three concerns:

1. **New overlay state + render path**. Existing overlays
   (search, palette, hint, title-edit, context-menu) each carry
   their own state struct + render path. A confirm dialog is its
   own state shape: `prompt: String`, `buttons: Vec<ConfirmButton>`,
   `default_focus: usize`, `on_confirm: ConfirmAction`. The render
   path is a centered modal with backdrop dimming (same as the
   command palette's overlay shape).

2. **Action vs overlay dispatch separation**. Today the
   `Action::CloseWindow` dispatch arm directly calls
   `event_loop.exit()`. We need to:
   - On Action::CloseWindow: check `cfg.ask_before_closing`.
   - If prompt needed, open the confirm modal with action =
     `ActionAfterConfirm::CloseWindow`. Don't exit yet.
   - On modal Confirm: dispatch the stored action.
   - On modal Cancel: just close the modal.

3. **Generic + extensible**. The first user is
   `ask_before_closing`. Phase 5 also wires `ClosePane` to use
   it; phase 6 wires `CloseTab`. Future work adds
   "kill running process" + "unsaved layout discard" as additional
   `ConfirmAction` variants.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::app                                                       │
│                                                                      │
│  pub struct ConfirmDialogState {                                     │
│      prompt: String,                                                 │
│      buttons: Vec<ConfirmButton>,                                    │
│      focus_idx: usize,                                               │
│      on_confirm: ConfirmAction,                                      │
│  }                                                                   │
│                                                                      │
│  pub enum ConfirmButton {                                            │
│      Cancel,                                                         │
│      Confirm { label: String, destructive: bool },                   │
│  }                                                                   │
│                                                                      │
│  pub enum ConfirmAction {                                            │
│      CloseWindow,                                                    │
│      CloseTab(usize),                                                │
│      ClosePane(PaneId),                                              │
│      // Future: KillProcess(pid), DiscardLayout, ResetConfig, ...    │
│  }                                                                   │
│                                                                      │
│  App carries: pub confirm_dialog: Option<ConfirmDialogState>         │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Dispatch wrapping                                                    │
│                                                                      │
│  fn maybe_confirm_then(&mut self, action: Action):                   │
│      let scope = match action {                                      │
│          Action::CloseWindow => panel_count_window(),                │
│          Action::CloseTab    => panel_count_tab(),                   │
│          Action::ClosePane   => 1,                                   │
│      };                                                              │
│      if should_prompt(self.cfg.ask_before_closing, scope) {          │
│          self.confirm_dialog = Some(ConfirmDialogState {             │
│              prompt: format!("Close {scope} pane(s)?"),              │
│              buttons: vec![                                          │
│                  ConfirmButton::Cancel,                              │
│                  ConfirmButton::Confirm {                            │
│                      label: "Close".into(), destructive: true,       │
│                  },                                                  │
│              ],                                                      │
│              focus_idx: 0,  // safe default                          │
│              on_confirm: ConfirmAction::CloseWindow,                 │
│          });                                                         │
│      } else {                                                        │
│          self.dispatch(action);  // bypass                           │
│      }                                                               │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_render: paint_confirm_dialog (NEW)                            │
│                                                                      │
│  Centered modal panel ~360x140 px on top of a dimming backdrop.      │
│  - Top: prompt text                                                  │
│  - Middle: button row (Cancel + Confirm; destructive Confirm gets    │
│            a red accent)                                             │
│  - Focused button has a 2 px accent border.                          │
└──────────────────────────────────────────────────────────────────────┘
```

`should_prompt`:

```rust
fn should_prompt(mode: AskBeforeClosing, scope_count: usize) -> bool {
    match mode {
        AskBeforeClosing::Never              => false,
        AskBeforeClosing::Always             => true,
        AskBeforeClosing::MultipleTerminals  => scope_count > 1,
    }
}
```

Pure — testable in isolation.

## Phase roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `should_prompt` pure helper + drift guard on all 3×{0..N} input shapes | Pure drift guard |
| 2 | `ConfirmDialogState` + `ConfirmAction` enum + App field | Compiles + existing tests pass |
| 3 | Renderer bottom-bar projection; centered modal + dimming backdrop remains polish | Snapshot / visual smoke on the layout |
| 4 | Keyboard nav: `Tab` cycles focus; `Enter` confirms; `Esc` cancels | Drift guard on key dispatch |
| 5 | Wire `Action::CloseWindow` through `maybe_confirm_then` | Drift guard + manual e2e |
| 6 | Wire `Action::CloseTab` + `Action::ClosePane` similarly | Drift guard |
| 7 | Mouse: click button hit-test + cursor-icon flip on hover | Pure geometry drift guard + manual e2e |
| 8 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +6-8 (the pure `should_prompt` covers a
matrix; the dispatch wrapping needs 3 drift guards).

## What WON'T ship in v1

- **Per-pane confirmation toggle**. Terminator has a per-profile
  `ask_before_closing`. kettle ships a single global cfg key.
  Per-pane would compound with the named-groups design; defer.
- **Confirm-with-input** ("Type the pane title to close" — git-rebase
  style protection). Overkill for terminals; the existing modal
  with focus-on-Cancel is adequate.
- **Toast-instead-of-modal**. Some users prefer a transient
  "Closing in 3..2..1 (Esc to cancel)" instead of a modal. v1
  ships the modal; toast is a follow-up on the same
  primitive.

## Acceptance test

```
# In ~/.config/kettle/config:
ask-before-closing = multiple_terminals

$ kettle
# split 4 ways with Ctrl+Shift+E
# press Ctrl+Shift+Q (CloseWindow)
# verify: modal opens with "Close 4 panes?" + Cancel/Close buttons
# verify: Cancel has focus (the safe default)
# press Esc → modal closes, window stays open
# press Ctrl+Shift+Q again, Tab to focus Close, press Enter
# verify: window closes
# repeat and click the visible [Cancel] / [Close] buttons in the bottom bar
# verify: the pointer cursor appears over buttons and clicks dispatch correctly

# In config: ask-before-closing = always
# now even single-pane CloseWindow prompts
# verify: yes

# In config: ask-before-closing = never
# 4-pane CloseWindow closes immediately, no modal
# verify: yes
```

## Risks + mitigations

- **Risk:** modal blocks the entire window — what if the user has
  a long-running command they want to keep typing into? **Mitigation:**
  modals only fire on the Close family of actions. Other input flows
  through. The close action is already user-triggered, so
  blocking is acceptable.
- **Risk:** double-press of CloseWindow (`Q-Q`) accidentally
  bypasses the modal. **Mitigation:** the modal absorbs keystrokes
  while open; a second `Q` doesn't re-dispatch CloseWindow — it
  goes to the modal as an unhandled key.
- **Risk:** the command palette has its own overlay z-order;
  confirm modal needs to render above it. **Mitigation:** explicit
  overlay-z-order list documented in `kettle_render`; confirm modal
  sits above palette + below the global cursor.
- **Risk:** keyboard-only users can't see the focused button.
  **Mitigation:** focused button has a 2 px accent border. Same
  visual treatment as the command palette's selected row.
