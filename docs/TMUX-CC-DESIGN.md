# tmux `-CC` control-mode integration — design

> Status: parser foundation (cycle 327) shipped. End-to-end integration is a
> multi-cycle thread; this doc is the design + sub-cycle roadmap so a future
> contributor (or future-me) can pick it up without re-deriving the protocol.

## What "tmux control mode" is

`tmux -CC` (or `tmux -CC attach`) puts a tmux client in control mode: instead of
rendering the multiplexer UI itself, tmux emits a structured protocol on its
PTY that the host terminal can parse to discover tmux's window/pane tree, route
output, and forward keystrokes. iTerm2 was the original consumer; the protocol
is documented in tmux's `control.c` source.

Every message is one line, `\n`-terminated, starting with `%`:

- `%begin TIME N FLAGS` ... lines ... `%end TIME N FLAGS` — wraps a multi-line
  response to a command. TIME is a Unix timestamp, N is a monotonically-
  increasing sequence id, FLAGS is currently `0` or `1`.
- `%output %ID DATA` — terminal output from the named tmux pane.
  Non-printable bytes inside DATA are octal-escaped as `\nnn`.
- `%window-add @ID` / `%window-close @ID` / `%window-renamed @ID NAME`
  — windows-tree updates.
- `%session-changed $ID NAME` / `%session-renamed NAME` — session state.
- `%layout-change @ID LAYOUT` — pane layout inside a window changed.
- `%client-detached CLIENT` — client (maybe us) detached.
- `%exit [REASON]` — control session ending.

## kettle's integration plan (sub-cycles)

| # | Cycle | What ships | Status |
|---|------|------|--------|
| 1 | 327 | Pure-parser foundation (`kettle_vt::tmux_cc::TmuxControlParser`). Feed bytes, get `TmuxEvent`s. 11 unit tests pin every variant + edge cases (CRLF, partial lines, overflow). No App integration. | ✅ shipped |
| 2 | 328 | This doc. Roadmap so the remaining sub-cycles aren't lost to context decay. | ✅ shipped |
| 3 | next | `Pane.tmux_control: Option<TmuxControlState>` flag. When set, the pane's PTY reader routes output through the parser before forwarding to alacritty_terminal. Pane gets a method `enter_tmux_control()` that flips the flag. | pending |
| 4 | next+1 | Map tmux windows → kettle tabs. On `%window-add`, kettle synthesizes a new tab in the same window where the controller is running. On `%output`, the controlled-pane's bytes go to the corresponding kettle tab's first pane. On `%window-close`, close the kettle tab. | pending |
| 5 | next+2 | Two-way: when the user types into a kettle pane that maps to a tmux window, kettle sends `send-keys -t %ID <keys>` to the tmux controller pane's PTY. Wires the cycle-298 vi-mode + cycle-302 remote-control similarly. | pending |
| 6 | next+3 | `%layout-change` parser → kettle splits within the tab. tmux's layout format is `<id>,<WxH>,<X>,<Y>,<panes>` recursively. | pending |
| 7 | next+4 | `%client-detached` + `%exit` cleanup. Close synthesized tabs when the control session ends; restore the controller pane to a normal terminal. | pending |
| 8 | follow-up | Auto-detect entry: heuristic that watches PTY output for the `%begin 0 0 1\n` first-message marker tmux always emits and auto-flips `tmux_control`. Optional UX nicety vs explicit toggle. | optional |

Each row is a self-contained cycle: lint/build/test green, drift guard added,
commit, push. The thread closes when the user can run `tmux -CC` in a kettle
pane and see tmux windows surfaced as kettle tabs with two-way input.

## Architecture choices (rationale)

### Parser lives in kettle-vt, not kettle-core

The cycle-327 parser is pure: bytes in, events out, no Rust I/O. That matches
kettle-vt's "vt + image-protocol parsers" charter rather than kettle-core's
"PTY + alacritty_terminal" charter. The kettle-core side will consume the
parser (cycle next+0) but the parser itself doesn't depend on alacritty_terminal.

### `\nnn` octal decoding done in `parse_output`

tmux escapes control bytes (anything outside printable ASCII) in `%output`
DATA as 3-digit octal `\nnn`. Decoding happens at parse time so the consumer
gets raw bytes ready to forward to a terminal. UTF-8 multibyte glyphs in
tmux's terminal state are pre-encoded as bytes inside `\nnn` sequences (not
as UTF-8 codepoints in the wire format), so the parser's char-iteration
loop is correct: every non-`\` char is a single ASCII byte.

### Partial-line buffering + 64 KB overflow cap

A malformed (or hostile) PTY stream that emits a single 100 MB line would
otherwise hold a buffer forever. The 64 KB cap drops the buffer + keeps
scanning so the next normal line still parses. The
`oversize_line_dropped_without_stalling` test pins the recovery behavior.

### `OutsideBlock` for non-`%` lines

tmux's `%begin/%end` blocks contain non-`%` lines (the multi-line response
body). The parser doesn't yet track block state, so it surfaces those as
`OutsideBlock` events; the consumer can attach them to the most-recent
`%begin` it saw. This sidesteps a state machine that would be load-bearing
for `%begin/%output/%end` interleaving — a future cycle can add the state
tracking if a real consumer needs it.

### `Unknown` for forward-compat

tmux occasionally adds new `%verb` messages (recent versions have
`%session-window-changed`, `%paste-buffer-changed`, etc.). The parser
preserves the raw line as `TmuxEvent::Unknown` so kettle can log + skip
without losing data, rather than dropping bytes silently. The consumer
chooses the diagnostic.

## Why this is multi-cycle

The protocol parser is the easy part (200 lines + 11 tests). The hard parts
are:

- **Pane-state plumbing.** Today's `Pane` reads from a PTY and feeds
  `alacritty_terminal`. tmux-control mode needs an in-between layer that
  filters `%output` events through the parser, demuxes by `pane_id`, and
  routes each pane's bytes to a SEPARATE virtual terminal that's surfaced
  as a kettle tab. That's a structural change to the read loop.
- **Mapping tmux windows ↔ kettle tabs.** Today, tabs are 1:1 with `Mux::tabs`.
  When tmux-control enters, kettle needs to synthesize tabs for every
  tmux window WITHOUT spawning new PTYs. The Mux's invariants assume
  tabs back onto real PTYs.
- **Routing input.** Today, user keystrokes go to the focused pane's PTY.
  In tmux-control, they need to be wrapped as `send-keys -t %ID <encoded>`
  and written to the CONTROLLER pane's PTY instead. That's a per-pane
  re-routing decision.
- **Detach cleanup.** When the tmux controller exits (`%exit`), every
  synthesized kettle tab must close cleanly + the controller pane must
  return to a normal terminal session.

Each of those is a multi-day implementation. Shipping them all in a single
session would be rushed; the sub-cycle plan lets each land as a small,
testable contract.

## What kettle SHOULD ship before declaring tmux integration done

End-to-end test: launch kettle, run `tmux -CC new -s demo`, send some
control commands, verify:

1. `%window-add @1` produces a new kettle tab.
2. `%output %1 hello\015\012` makes "hello\r\n" appear in that tab's pane.
3. User typing in the kettle tab arrives at the tmux window via `send-keys`.
4. `%window-close @1` closes the kettle tab.
5. `%exit` cleans up + returns the controller pane to normal mode.

Plus: drift guards on the parser, on the windows→tabs mapping, on the
send-keys encoding, on the cleanup path.

## See also

- tmux source: <https://github.com/tmux/tmux/blob/master/control.c>
- iTerm2's tmux integration docs:
  <https://iterm2.com/documentation-tmux-integration.html>
- WezTerm's analogous design:
  <https://wezterm.org/multiplexing.html#tmux-mode>
