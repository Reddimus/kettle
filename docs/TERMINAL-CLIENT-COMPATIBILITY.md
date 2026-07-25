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

## File paste (paths)

Image paste (above) covers images only. For any other file — a video, a PDF, an
arbitrary binary — the portable channel both Claude Code and Codex accept is a
**file path pasted as text**: the client reads the path (`Read`, or `ffmpeg`/
`ffprobe` via a shell for a video) rather than receiving bytes over an escape
sequence.

Kettle supports this three ways, all of which paste a shell-quoted path (never
raw bytes):

- **Copy a file** in Explorer/Finder, then paste (`Ctrl+Shift+V`). When the
  clipboard holds a file list instead of text, Kettle pastes the path(s).
  Controlled by `paste-files` (on by default; `paste-files = off` disables it).
- **Copy a screenshot** (Win+Shift+S, Snipping Tool, macOS Cmd+Shift+4, GNOME
  Screenshot), then paste. A capture puts a raw *bitmap* on the clipboard with
  no file and no text behind it, so Kettle writes it to a temporary PNG and
  pastes that path. Controlled by `paste-images` (on by default). The temp files
  are owner-only, bounded, and deleted when Kettle exits — fine for handing an
  image to a running agent, not a durable store. This also avoids depending on
  the client's own clipboard-bitmap support, which varies by platform: Claude
  Code's documented image-paste chord is `Alt+V` on Windows/WSL and `Ctrl+V`
  elsewhere, and native-Windows bitmap paste is unreliable. Kettle leaves those
  chords unbound (its own paste is `Ctrl+Shift+V`) so they still reach the
  client for anyone who prefers that route.
- **Drag and drop** a file onto the window — always pastes the path.

Multiple selected files paste as space-separated quoted paths. Paths are quoted
for the focused pane's shell (POSIX single-quote, PowerShell `''`, or `cmd`
double-quote), and when the pane runs **WSL** a Windows path is translated to
its `/mnt/c/…` (or in-distro `/home/…` for a `\\wsl.localhost\…` share) form so
the Linux-side agent can open it. There is no video decoder in either client;
the path lets the agent drive `ffmpeg` itself.

## Focus and cursor state

When a Kettle window is unfocused, Kettle suppresses the rendered terminal
cursor. It does not replace the cursor with a hollow block and does not send a
DEC cursor command to the child. On refocus, the exact client-selected DEC shape,
visibility, and blink state is rendered again. This avoids the hollow bottom-left
caret that interactive clients can leave visible while another window is active.

## Keyboard encoding

Kettle supports the progressive [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
end to end. It answers `CSI ? u` capability queries, applies set/push/pop mode
requests, and emits negotiated CSI-u press, repeat, and release events. The
encoder covers alternate key codes, associated text, left/right modifiers,
keypad keys, F13-F35, navigation, media, and volume keys. A bounded 16-entry
mode stack prevents a client from growing terminal state indefinitely.

With no negotiated Kitty flags, Kettle retains its existing xterm-compatible
bytes exactly, including DECCKM application cursor mode, DECKPAM application
keypad mode, xterm modified navigation keys, and the usual control codes. This
progressive behavior matters for shells and older TUIs: enabling support does
not force CSI-u on applications that never request it. A key press consumed by
Kettle UI or a Kettle keybinding also suppresses its matching physical release,
so a Kitty-aware child never receives a release for a press it did not see.

Current Neovim queries CSI-u first and falls back to xterm modifyOtherKeys only
when the terminal does not answer. Kettle's reply lets Neovim distinguish keys
such as `Tab`/`Ctrl+I`, `Enter`/`Ctrl+M`, Escape-related chords, and keypad keys.
See Neovim's [TUI input documentation](https://neovim.io/doc/user/tui/#tui-input).

## tmux and full-screen clients

- Kettle forwards application cursor/keypad modes, focus reports, SGR mouse
  events, bracketed paste, alternate-scroll behavior, resize events, Kitty
  keyboard negotiation, OSC 8 links, OSC 52 clipboard writes, styled
  underlines, and synchronized updates through the PTY.
- Keep Kettle's outer `TERM=xterm-256color`. Inside tmux, keep tmux's
  `default-terminal` at `tmux-256color`; do not globally force either value over
  the other. `COLORTERM=truecolor`, `TERM_PROGRAM=kettle`, and
  `TERM_PROGRAM_VERSION` are already exported by Kettle.
- tmux does not yet auto-detect Kettle's feature set. For tmux 3.2 or newer,
  add this to `~/.tmux.conf`:

  ```tmux
  set -as terminal-features ',xterm-256color:RGB:clipboard:cstyle:extkeys:focus:hyperlinks:mouse:osc7:overline:strikethrough:sync:usstyle'
  set -g extended-keys on
  ```

  `on` preserves progressive negotiation: tmux forwards extended keys to an
  inner application when that application requests them. tmux 3.5 or newer is
  preferred because its extended-key handling was revised to request and
  preserve xterm mode 2. The options and feature names are defined in the
  [tmux manual](https://man.openbsd.org/tmux#extended-keys) and
  [tmux changelog](https://github.com/tmux/tmux/blob/master/CHANGES).
- Hold `Shift` while using the wheel to scroll Kettle's own scrollback when a
  mouse-aware tmux/TUI pane would otherwise consume the wheel.
- Client-owned `Ctrl+V`, `Alt+V`, and `Ctrl+Alt+V` chords remain PTY input inside
  and outside tmux. A tmux binding using the same chord takes precedence by tmux
  design.
- Kettle's Codex-specific cursor compatibility policy is limited to the known
  transient native-Windows ConPTY sequence. The global unfocused-window rule is
  client-independent and does not identify processes by name.

Run `scripts/check-agent-cli-smoke.sh` from a Kettle checkout to verify the
installed Codex CLI, Claude Code CLI, tmux, clean Neovim, and configured
Neovim/AstroNvim against the current Kettle binary. The smoke also performs a
real `CSI ? u` PTY round trip and validates tmux's additive feature entry.
