# Terminal Client Compatibility

Kettle transports terminal input and output; Codex CLI and Claude Code own image
decoding and attachment. Kettle does not add a proprietary image protocol.

## Clipboard image paste

| Client location | Chord in Kettle | What Kettle sends |
|---|---|---|
| Codex CLI on native Windows or Linux | `Ctrl+V` | `C-v` (`0x16`) |
| Codex CLI under WSL | `Ctrl+Alt+V` | `M-C-v` (`ESC`, `0x16`) |
| Claude Code on native Windows | `Alt+V` | `M-v` (`ESC v`) |
| Claude Code on WSL or native Linux | `Ctrl+V` | `C-v` (`0x16`) |

`Ctrl+Shift+V` remains Kettle's normal text-paste command. Bare `Ctrl+V` and
`Alt+V` and `Ctrl+Alt+V` are not default Kettle bindings, so the client receives
them unchanged. Current Codex WSL builds can read image data through a Windows
PowerShell clipboard fallback when Linux clipboard APIs are unavailable. The
client still controls accepted formats, size limits, prompts, and attachment UI.

To test, place a PNG or screenshot in the Windows clipboard, focus the client's
prompt, and press its chord from the table. A text-only paste result means the
client did not detect image data; Kettle cannot convert clipboard text back into
an image.

## Focus and cursor state

When a Kettle window is unfocused, Kettle suppresses the rendered terminal
cursor. It does not replace the cursor with a hollow block and does not send a
DEC cursor command to the child. On refocus, the exact client-selected DEC shape,
visibility, and blink state is rendered again. This avoids the hollow bottom-left
caret that interactive clients can leave visible while another window is active.

## Keyboard encoding

Kettle currently emits its legacy/xterm-compatible key encodings, including
application cursor/keypad modes and the existing modifier forms. It does not
emit kitty CSI-u keyboard events. Although the underlying terminal engine can
parse kitty keyboard mode requests, Kettle deliberately leaves that mode
disabled so Neovim and other clients do not negotiate CSI-u and then receive
legacy bytes. A future CSI-u encoder must land before that capability can be
advertised; v2.35 does not claim it.

## tmux and full-screen clients

- Kettle forwards application cursor/keypad modes, focus reports, SGR mouse
  events, bracketed paste, alternate-scroll behavior, and resize events through
  the PTY. tmux can therefore negotiate these features normally; this does not
  imply kitty CSI-u/extended-key support.
- Hold `Shift` while using the wheel to scroll Kettle's own scrollback when a
  mouse-aware tmux/TUI pane would otherwise consume the wheel.
- Client-owned `Ctrl+V`, `Alt+V`, and `Ctrl+Alt+V` chords remain PTY input inside
  and outside tmux. A tmux binding using the same chord takes precedence by tmux
  design.
- Kettle's Codex-specific cursor compatibility policy is limited to the known
  transient native-Windows ConPTY sequence. The global unfocused-window rule is
  client-independent and does not identify processes by name.
