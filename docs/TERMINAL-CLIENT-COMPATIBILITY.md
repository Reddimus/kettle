# Terminal Client Compatibility

Kettle transports terminal input and output; Codex CLI and Claude Code own image
decoding and attachment. Kettle does not add a proprietary image protocol.

On Unix and WSL, piped stdin EOF does not close Kettle's bidirectional PTY
master: doing so would also discard DA, DSR, Kitty-keyboard, and other replies
that interactive clients issue after startup. Kettle signals canonical EOF
with the live PTY's configured VEOF character. A client already in raw mode
must use its own delimiter or a `kettle exec --timeout`; Kettle reports that
case explicitly and preserves the terminal-reply path without injecting a
guessed control byte. The headless path also omits DA1 extension `52` because
it has no clipboard sink; a live GUI pane advertises that extension only when
its current OSC 52 write policy and platform clipboard permit it.

Native Windows ConPTY forwards piped bytes but has no safe portable EOF
half-close. Delimiter- or length-driven consumers work; an EOF-waiting child
must use its own protocol delimiter or `--timeout`. Kettle leaves conin open so
terminal queries and normal child lifetime are not converted into a forced
`STATUS_CONTROL_C_EXIT`.

## Terminal-rendered inline graphics

Sixel, Kitty graphics, and iTerm2 OSC 1337 are terminal output protocols,
separate from the Codex/Claude attachment boundary below. Their registries
follow the active screen: mode 47 preserves alternate graphics on entry and
exit; mode 1047 preserves them on entry and clears them on exit; mode 1049
saves/restores the cursor, clears alternate graphics on entry, and preserves
them on exit. ED 2 clears only the active graphics buffer and RIS clears both.

Each rendered placement is clipped to the intersection of its pane interior
and exact terminal grid. Kettle crops destination geometry and source UVs by
the same fractions, so negative or oversized placements do not bleed into
padding, titlebars, borders, sibling panes, or window chrome. Full-screen
scrolling and eviction use stable document rows. Inside a partial DECSTBM
region, images wholly contained by the page margins move with text and crop
their destination/source range at an edge; images already crossing a margin
stay fixed, matching the Kitty graphics protocol.

## Image attachment boundaries

Kettle does not promise a Codex CLI or Claude Code clipboard-attachment chord.
Those clients own their interactive composers, accepted formats, attachment UI,
and platform/version-specific shortcuts. Kettle's default keymap reserves
`Ctrl+Shift+V` for its own paste action and does not bind bare `Ctrl+V`,
`Alt+V`, or `Ctrl+Alt+V`. An unbound key reaches the PTY through the active
legacy-xterm or negotiated Kitty keyboard encoding, but that proves input
transport only; it does not prove that a client attached clipboard image data.

For Kettle's stable, client-independent path, focus the agent pane and press
`Ctrl+Shift+V`. When the clipboard contains a bitmap and `paste-images` is on
(the default), Kettle writes a bounded owner-only temporary PNG and pastes its
shell-quoted path. The running agent can read that path without needing native
clipboard-bitmap support. A short-lived thumbnail confirms the image dimensions
and says the path is on the command line without claiming the client attached
or opened it. Hover expands the receipt, clicking its body opens the retained
PNG, and `×` dismisses it. A two-minute hard limit removes even a hovered
receipt. A newer media paste replaces it, and the next keyboard, paste, or
control input dismisses it because the command line may have changed. Set
`paste-image-preview = off` to avoid creating or retaining preview pixels
without changing image paste.

Current local `codex --help` also exposes `-i, --image <FILE>...` for images
attached to an initial prompt. This is the durable Codex fallback when starting
a session:

```sh
codex --image ./screenshot.png "Inspect this image"
# short form:
codex -i ./screenshot.png "Inspect this image"
```

Under WSL, pass a path visible inside the distro, such as
`/mnt/c/Users/me/Pictures/screenshot.png`. No equivalent Claude Code local-image
flag is claimed here; consult the installed client's current help for its
supported attachment flows.

## File paste (paths)

Kettle's path-paste channel also works for a video, PDF, or arbitrary binary:
the agent reads the **file path pasted as text** (`Read`, or `ffmpeg`/`ffprobe`
via a shell for a video) rather than receiving bytes over an escape sequence.

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
  image to a running agent, not a durable store. This avoids depending on the
  client's platform- and version-specific clipboard-bitmap support. The
  optional receipt previews only these Kettle-created files, never arbitrary
  paths printed or pasted in the terminal.
- **Drag and drop** a file onto the window — always pastes the path.

Multiple selected files paste as space-separated quoted paths. Paths are quoted
for the focused pane's shell (POSIX single-quote, PowerShell `''`, or `cmd`
double-quote), and when the pane runs **WSL** a Windows path is translated to
its `/mnt/c/…` (or in-distro `/home/…` for a `\\wsl.localhost\…` share) form so
the Linux-side agent can open it. There is no video decoder in either client;
the path lets the agent drive `ffmpeg` itself.

An explicit copied or dropped video also gets a short-lived receipt when
`paste-video-preview` is on. After bounded background validation it uses a
native thumbnail when one is available and a generic poster otherwise. The
receipt never means the client attached or opened the video. macOS uses Quick
Look, Windows uses the Shell thumbnail provider, and Linux accepts only a
matching owner-controlled cache entry that other principals cannot modify. The
video card has no open action because native launch APIs cannot bind a path to
Kettle's validated handle; clicking the card or `×` dismisses it. Kettle does
not scan hovered or pasted path text.

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

With no negotiated Kitty flags, Kettle retains the xterm-compatible bytes for
unmodified keys, DECCKM application cursor mode, DECKPAM application keypad
mode, modified navigation keys, and the usual control codes. Cursor, function,
editing, and keypad keys keep their own xterm encodings; `modifyOtherKeys` does
not gate those branches.

The legacy modifier parameter carries Shift, Alt, and Control only — it is
`1 + shift + 2*alt + 4*ctrl`, so it never leaves the range `1..=8`. Bit 8 in
that parameter is xterm's **Meta**, a distinct X11 modifier Kettle has no key
for; it is not macOS Command and not the Windows or Linux Super key. A chord
holding Super therefore has no legacy representation, and Kettle writes **no
PTY bytes** for one that no keybinding claims, on every platform. Super reaches
applications only through the Kitty keyboard protocol, which defines a real
super bit: with `CSI > 1 u` negotiated, `Cmd+Option+Up` is `CSI 1;11A`,
while the same chord in a legacy pane sends nothing. The agent control plane
follows the same rule — `send_keys` reports an error for a Super chord the
target pane cannot encode rather than silently dropping the modifier.

The negotiated `modifyOtherKeys` resource always starts at level zero. An
application can select levels zero, one, or two with `CSI > 4 ; Pv m`, and
`CSI ? 4 m` reports only that state as `CSI > 4 ; Pv m`. Omitting `Pv` restores
resource 4 to its initial zero value; parameterless `CSI > m` restores every
tracked modifier resource. RIS and DECSTR also restore the initial state. A
query counts as negotiation, so after Kettle reports level zero it cannot
contradict that reply by using its pre-negotiation Enter fallback.

For Return, Tab, Backspace, Escape, Space, and ASCII characters, Kettle follows
the [xterm modified-key matrix](https://invisible-island.net/xterm/modified-keys-us-pc105.html):

- Level zero keeps the legacy encoding.
- Level one is modifier-aware. It keeps Shift+Return as Return, Ctrl+I as Tab,
  Shift+Tab as `CSI Z`, Backspace chords in their legacy forms, and established
  control aliases. Alt-bearing combinations use `CSI 27 ; modifier ; code ~`;
  Control-only ASCII combinations use it only outside `[64,127]` and when they
  are not a known control alias. Alt+Return, Shift+Alt+Return, Ctrl+Return, and
  Ctrl+Tab are encoded by that rule.
- Level two uses that `CSI 27` form for modified covered keys. The exact
  Ctrl+Backspace alias remains `BS`, and Shift+Tab remains the separate edit-key
  sequence `CSI Z`.

Plain keys are unchanged at every level. In particular, plain Enter is always
`CR`:

| Keyboard mode | Enter | Shift+Enter | Ctrl+Enter | Alt+Enter |
|---|---|---|---|---|
| No negotiation; `auto` at a canonical/unknown shell prompt | `0D` | `0D` | `0D` | `0D` |
| No negotiation; `auto` in a recognized agent composer, or `always` | `0D` | `ESC [ 27;2;13~` | `ESC [ 27;5;13~` | `ESC [ 27;3;13~` |
| Negotiated xterm level 0 | `0D` | `0D` | `0D` | `0D` |
| Negotiated xterm level 1 | `0D` | `0D` | `ESC [ 27;5;13~` | `ESC [ 27;3;13~` |
| Negotiated xterm level 2 | `0D` | `ESC [ 27;2;13~` | `ESC [ 27;5;13~` | `ESC [ 27;3;13~` |
| Kitty disambiguation negotiated | `0D` | `ESC [ 13;2u` | `ESC [ 13;5u` | `ESC [ 13;3u` |

`modify-other-keys = auto` is the default and controls only the first two rows.
It recognizes Codex, Claude Code, Gemini, and OpenCode rather than assuming that
every raw-mode application accepts Kettle's legacy xterm fallback. On
Unix/macOS Kettle reads the live PTY line discipline and foreground process
group immediately before each modified Enter, then matches that pid to the
direct launch identity or the bounded background process snapshot. Both
noncanonical input and a recognized foreground composer are required. This is
deliberately narrower than "foreground job": zsh ZLE, nested shells, Python,
psql, gdb, and other readline/libedit clients can also use noncanonical mode. A
stale, missing, or ambiguous snapshot gets plain `CR`.

On Windows, an observed recognized composer must coincide with OSC 133's
running-command state. The shell must have one unambiguous direct child branch;
helper forks below the composer are allowed, while multiple shell-child branches
fail closed because ConPTY cannot identify foreground versus background. A
directly launched recognized composer is also accepted because no shell prompt
can inherit the pane. Idle and unknown shells receive plain Enter. SSH/WSL
transports and session, privilege, namespace, sandbox, and container wrappers
are intentionally not unwrapped: Kettle cannot prove which inner client owns
their input. Use `always` for such a client or for an unrecognized composer
(`enter` remains its compatibility alias), and use `off` to remove the fallback.
GUI typing, control-plane `send_keys`, and each broadcast target make this
decision from that target pane's live state.

None of these settings blocks an application's xterm request or Kitty CSI-u,
either of which can still distinguish Enter chords and takes precedence. This
separation matters because assuming level two globally would also stop Ctrl+I
from acting as Tab for every legacy client. It also prevents an unsolicited
`ESC [ 27;2;13~` from reaching an ordinary line editor, where `ESC [ 27` can be
consumed as a function-key prefix and the remainder appears literally as
`;2;13~`.

This progressive behavior matters for shells and older TUIs: enabling support
does not force CSI-u on applications that never request it. A key press consumed
by Kettle UI or a Kettle keybinding also suppresses its matching physical
release, so a Kitty-aware child never receives a release for a press it did not
see.

## tmux and full-screen clients

- Kettle forwards application cursor/keypad modes, focus reports, SGR mouse
  events, bracketed paste, alternate-scroll behavior, resize events, Kitty
  keyboard negotiation, OSC 8 links, OSC 52 clipboard writes, styled
  underlines, and synchronized updates through the PTY. OSC 52 target `c`
  addresses the regular clipboard; `p`/`s` addresses Linux PRIMARY without
  falling back to the regular clipboard when a PRIMARY operation fails.
- Keep Kettle's outer `TERM=xterm-256color`. Inside tmux, keep tmux's
  `default-terminal` at `tmux-256color`; do not globally force either value over
  the other. `COLORTERM=truecolor`, `TERM_PROGRAM=kettle`, and
  `TERM_PROGRAM_VERSION` are already exported by Kettle.
- tmux does not yet auto-detect Kettle's feature set. For tmux 3.4 or newer,
  add this to `~/.tmux.conf`:

  ```tmux
  set -as terminal-features ',xterm-256color:RGB:clipboard:cstyle:extkeys:focus:hyperlinks:mouse:osc7:overline:strikethrough:sync:usstyle'
  set -s extended-keys on
  set -g allow-passthrough on
  ```

  `extended-keys` plus the `extkeys` outer-terminal feature preserve modified
  keys such as Shift+Enter when an inner application requests them.
  `allow-passthrough` is also required by
  [Claude Code's documented tmux setup](https://code.claude.com/docs/en/terminal-config)
  for terminal notifications and progress. tmux 3.5 or newer is
  preferred because its extended-key handling was revised to request and
  preserve xterm mode 2. The options and feature names are defined in the
  [tmux manual](https://man.openbsd.org/tmux#extended-keys) and
  [tmux changelog](https://github.com/tmux/tmux/blob/master/CHANGES).
  On tmux 3.3 or older, use only the feature names documented by that
  installed version instead of copying this newer list; for example, OSC 7 and
  OSC 8 terminal-feature support arrived in later tmux releases.
- SIXEL through tmux has a separate **version and build-capability gate**.
  Kettle supports SIXEL directly, but tmux supports it only in tmux 3.4 or
  newer when tmux itself was configured with `--enable-sixel`. Do not infer
  that compile-time option from `tmux -V`. A capable tmux advertises DA1
  feature code `4` to an application inside its pane; tmux 3.6 or newer also
  exposes the direct check `tmux display-message -p '#{sixel_support}'`
  (`1` means enabled). `just agent-tui-smoke` and
  `just agent-tui-wsl-smoke` perform the DA1 check on tmux 3.4 and newer and
  cross-check the format on tmux 3.6 and newer.

  Only after both gates are confirmed, add `sixel` for Kettle's outer terminal
  type (or append `:sixel` to the existing feature entry):

  ```tmux
  # tmux >= 3.4 AND a tmux build configured with --enable-sixel only
  set -as terminal-features ',xterm-256color:sixel'
  ```

  The live smoke then starts its private tmux client with that feature. Rendering
  also requires tmux to know the outer terminal's nonzero pixel cell size. This
  is commonly missing from `TIOCGWINSZ` across WSL/ConPTY; tmux 3.5a and newer
  can query the outer terminal when the ioctl values are zero, so that version
  or newer is preferred for SIXEL in a Kettle WSL pane. The smoke queries
  `CSI 16 t`: with nonzero geometry it requires a generated 24x12 SIXEL to
  reach Kettle's renderer; with zero geometry it requires and records tmux's
  `SIXEL IMAGE (WxH)` text fallback instead of claiming an image pass.

  If tmux is older, was built without SIXEL, or cannot be verified, leave
  `sixel` out. Run the image command outside tmux, or select the application's
  text/block fallback (for example, `chafa -f symbols`) instead; Kettle cannot
  restore an image sequence an intervening tmux did not preserve.
- Hold `Shift` while using the wheel to scroll Kettle's own scrollback when a
  mouse-aware tmux/TUI pane would otherwise consume the wheel.
- Keys not bound by Kettle remain PTY input inside and outside tmux. A tmux
  binding using the same key takes precedence by tmux design. This transport
  guarantee does not assert that an inner client interprets any particular key
  as an image attachment.
- Kettle's Codex-specific cursor compatibility policy is limited to the known
  transient native-Windows ConPTY sequence. The global unfocused-window rule is
  client-independent and does not identify processes by name.

Run `scripts/check-agent-cli-smoke.sh` from a Kettle checkout to verify the
installed Codex CLI, Claude Code CLI, tmux, clean Neovim, and configured
Neovim/AstroNvim against the current Kettle binary. The smoke also performs a
real `CSI ? u` PTY round trip when Unix Python with `termios` is available,
checks Codex's documented `--image` help entry, and validates tmux's additive
feature entry when tmux is available. It does not populate a clipboard, inject
keys into either client's interactive composer, or assert that an image
attachment appeared. Under Windows Git Bash, npm-installed clients are launched
through their `.cmd` entry points and `cmd.exe`, rather than passing an
extensionless POSIX shim to `CreateProcessW`; run the script with `--self-test`
to exercise that resolver without installed clients.
The live `agent-tui` variants add the build-gated tmux SIXEL render check; an
older, disabled, or unverified tmux build is recorded as a skip, not a pass.
