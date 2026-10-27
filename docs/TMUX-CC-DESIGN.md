# tmux control mode - design proposal

> **Not implemented.** The unused parser scaffold was removed in v2.26.0.
> This proposal describes future integration, not Kettle's current API.

Control mode lets a terminal show tmux's windows and panes in its own UI.
The tmux client exchanges commands and notifications instead of drawing the
usual terminal interface. Kettle would need virtual panes backed by remote
tmux panes rather than new local PTYs.

## Protocol essentials

Use the [tmux control-mode documentation](https://github.com/tmux/tmux/wiki/Control-Mode)
and [current manual](https://man.openbsd.org/tmux#CONTROL_MODE) when implementing.

- `-C` starts control mode without changing terminal attributes, useful for
  tests. `-CC` also changes terminal attributes and wraps the protocol in
  `\033P1000p` and a closing `\033\`. The entry sequence can identify that
  mode; `%begin 0 0 1` is not a fixed startup marker.
- A command response starts with `%begin TIME NUMBER FLAGS` and ends with
  matching `%end` or `%error`. Its body can contain ordinary lines without
  a `%` prefix. Time and command number vary.
- `%output %PANE DATA` carries pane output. Bytes below `0x20` and backslashes
  use three-digit octal escapes. Other bytes, including UTF-8, can pass through
  unchanged: decode bytes, without assuming ASCII or valid UTF-8.
- Notifications describe sessions, windows, panes and layout changes. Use their
  stable `$session`, `@window` and `%pane` IDs rather than displayed indexes.
  Preserve unknown notification types for diagnostics and forward compatibility.

## Proposed stages

| Stage | Work |
|---|---|
| Parser | Add a byte-oriented parser in `kettle-vt` for framing, command blocks, notifications and output escapes. |
| Pane mode | Track entry and exit per controlling pane; route control records separately from ordinary terminal output. |
| Virtual panes | Give each remote pane its own terminal state, without creating another local PTY. |
| Layout | Map tmux windows and layouts to Kettle tabs and splits, retaining remote IDs. |
| Input | Serialize tmux commands and encode `send-keys` input correctly, including literal bytes and named keys. |
| Cleanup | Remove remote views on detach or exit and restore the controlling pane's ordinary terminal mode. |

Keep parsing free of I/O. `kettle-core` would feed decoded pane bytes into
terminal state; the UI and mux would own layout and input routing. The current
mux assumes local PTYs, so virtual panes need an explicit interface rather
than dummy PTYs or duplicated read loops.

Each stage needs focused tests. Choose and test record-size limits and recovery
before accepting input; retain partial records across reads and report oversized
records. Track response blocks so their output cannot be mistaken for pane data.
Do not declare support when only the parser exists.

## Acceptance

Run an isolated native tmux server and a Kettle client, then verify:

1. New tmux windows and splits create matching views without extra local shells.
2. `%output` reaches only its named pane, including escaped controls, backslashes
   and raw multibyte text split across reads.
3. Typing reaches the correct remote pane; local shortcuts remain local.
4. Renaming, layout changes and closing a window update the matching views.
5. Failed commands, detach, client exit and connection loss clean up without
   leaking control text into a shell or closing unrelated local panes.

Parser tests also need fragmented framing, empty output, `%error`, unknown
notifications and oversized-record recovery. End-to-end tests must exercise
both directions and teardown; parser-only tests do not establish integration.
