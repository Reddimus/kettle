# Terminal Client Compatibility

Kettle 4.0 supports Linux and macOS. Windows and WSL paragraphs on this page
record the final Windows-supported 3.3.0 behavior and retained compatibility
logic; they do not describe a current package or support commitment.

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

## Terminal queries

Programs ask the terminal what it supports and adapt to the answers. Kettle
answers these:

| Query | Reply | Who uses it |
|---|---|---|
| DA1 (`CSI c`) | `CSI ? 6 ; 4 ; 52 c`: VT102-compatible, sixel, and OSC 52 clipboard writes while they are allowed | Neovim's OSC 52 clipboard, sixel viewers |
| DA2 (`CSI > c`) | the terminal engine's version | vim's `termresponse` |
| XTVERSION (`CSI > q`) | `DCS > \| kettle(<version>) ST` | Claude Code, tmux |
| DSR (`CSI 5 n`, `CSI 6 n`) and DECXCPR (`CSI ? 6 n`) | status, and the cursor position with or without DEC's `?`, counted from the top margin under origin mode | shells, Claude Code |
| DECRQM (`CSI ? Ps $ p`) | each mode's state, including 47, 1047, 1049 and 2026 | Claude Code |
| DECRQSS (`DCS $ q Pt ST`) | the current SGR (`m`), scroll region (`r`) or cursor shape (` q`); anything else is answered as invalid | Neovim's truecolor and undercurl probes |
| XTGETTCAP (`DCS + q Pt ST`) | `Tc`, `RGB`, `setrgbf`, `setrgbb`, `Smulx`, `Setulc`, `Ss`, `Se`, `Co` and `colors`; one reply per name, in one write, and any other name answered as unknown (kitty's and Ghostty's form; xterm joins the names and stops at the first unknown one) | Neovim, tmux |
| Kitty graphics (`APC G a=q,i=...`) | `APC G i=<same id>;OK ST` after decoding direct RGB/RGBA or supported encoded data; fixed errors for invalid data or unsupported media | Kitty graphics capability probes |
| Kitty keyboard flags (`CSI ? u`) | the active flags | Codex, Claude Code, Neovim |
| Colour scheme (`CSI ? 996 n`), and DEC mode 2031 | `CSI ? 997 ; 1 n` for a dark theme, `CSI ? 997 ; 2 n` for light; with mode 2031 on, the same report whenever the theme's colours change, dark to dark included | Claude Code (mode 2031) |

Kitty graphics queries do not store images or replace existing image data or
placements. Chunked queries reply once on completion, retaining the first
image id and the latest nonzero quiet setting. `q=1` suppresses success and
`q=2` suppresses all replies. File, temporary-file and shared-memory transfer
queries return `ENOTSUP`; direct transfer is supported. Replies reach the PTY
in wire order before a following DA1 reply. A query answers immediately during
DEC 2026 synchronized output; the engine can hold the DA1 reply until the
update ends.

What the answers change:

- Claude Code probes DECRQM 2026 only after XTVERSION answers, and then draws
  with synchronized output, so a redraw arrives as one frame.
- Claude Code turns on mode 2031 and, with its theme set to follow the
  terminal (`/theme`, the automatic option), takes each report as a cue to
  re-read the background with OSC 11 and decide light or dark from that
  colour itself. It therefore switches when Kettle's theme does:
  `theme-mode = auto` following the system appearance or a schedule,
  `toggle_light_dark`, or a theme picked by hand. Kettle reports every change
  of the theme's colours, not only a flip between light and dark, because
  Claude Code's threshold differs from Kettle's.
- Neovim turns `termguicolors` on from the XTGETTCAP or DECRQSS answer when
  `COLORTERM` is unset, as it usually is over ssh, and draws undercurl
  diagnostics as curly lines rather than plain underlines, although
  `TERM=xterm-256color` has no `Smulx`.

No reply echoes what the program sent: an unknown DECRQSS setting gets the
bare invalid reply, and XTGETTCAP repeats a capability name only after
checking that it is hex digits. A request over 1 KiB gets the bare invalid
reply without its contents being kept, names past the 32nd in one XTGETTCAP
request get no reply, and a request that CAN, SUB or another escape sequence
cuts off before its ST gets none either.

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

Kitty transmission (`a=t`, `a=T`) and placement (`a=p`) commands with a
nonzero image id (`i`) or image number (`I`) receive one completion reply.
Transmit-only success means the image was stored; display success follows actual
placement admission. Replies include the image id, the original image number
when supplied, and a nonzero placement id (`p`). Anonymous images remain silent
and do not receive placement ids. Chunked uploads retain the first chunk's
identity and respond only on completion. A later nonzero `q` updates suppression;
`q=0` does not reset it. `q=1` suppresses success and `q=2` suppresses all replies.

Errors distinguish invalid data or geometry (`EINVAL`), unsupported transmission
media (`ENOTSUP`), a missing image (`ENOENT`), a missing parent placement
(`ENOPARENT`), and unavailable image storage (`ENOSPC`). A command cannot specify
both nonzero `i` and `I`; zero means that identifier was not supplied.
Image-number uploads create distinct ids; later number-based commands select
the newest image with that number. A delete refused for conflicting identifiers
leaves partial uploads intact. Unrecognized action codes never become uploads
or return success replies.

Ordinary replies follow the same graphics ordering as placement. During DEC 2026
synchronized output, they follow journal replay at commit or the existing 150 ms
synchronized-output timeout. A child may wait for its reply before ending the
update without indefinitely blocking that handshake. Capability queries retain
the immediate path above. Animation frame and composition commands (`a=f`,
`a=c`) likewise complete after actual admission and the animation refresh,
including synchronized replay. Frame replies include the actual one-based
frame number (`r`); composition replies do not include `r`. This coverage does
not establish complete Kitty response conformance.

Frame 1 is the root. Frame uploads edit an existing nonzero `r`, or append when
`r` is omitted, zero, or beyond the last frame. An appended partial frame starts
transparent unless a background color (`Y`) or existing background frame (`c`)
is specified. Composition requires existing nonzero source (`r`) and destination
(`c`) frames. Omitted or zero composition width/height selects the full source
canvas. Both rectangles must fit; overlapping self-composition is refused.
Missing images/frames return `ENOENT`, invalid geometry returns `EINVAL`, and
unavailable pixel storage returns `ENOSPC`. Refused operations preserve pixels.

Kitty animation frame composition (`a=c`) reads source offsets from `X`/`Y`
and destination offsets from `x`/`y`. Frame data (`a=f`) uses lowercase offsets
and keeps `X=1` as its replacement flag. Newly appended frames default to
40 ms when `z` is omitted or zero; edits preserve the existing delay unless a
nonzero `z` is supplied. Root frames default to zero delay, and negative gaps
remain gapless. These rules follow the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

Direct Kitty retransmission of an existing image id retires its previous data,
placements, and animation at the first accepted chunk. A transmit-only `a=t`
upload remains unplaced until `a=p` or a new display transmission. Synchronized
output keeps the old placements visible until commit, then processes retirement
before replacement decoding. Other partial uploads and the other screen's
matching numeric id remain independent. Relative children follow parent deletion;
a child with an independent placement retains its image data. Actual retained
render snapshots remain charged and can prevent replacement admission.

At retained-byte or active image-count pressure, a completed Kitty transmission
reclaims eligible old images so the incoming id remains addressable. Unplaced
images come first, followed by placed images, each in creation order. The two
screens share the byte allowance, but only an active-screen root releases an
active count slot. External pixel snapshots stay charged until their final handle
is released; if the available victims cannot satisfy admission, existing roots
remain intact. The new image is decoded before eviction and must fit the existing
process-wide staging allowance as well as the final retained allowance.

Kitty relative placements preserve the exact parent `(P, Q)` through physical,
virtual, and relative chains. A nonzero `Q` never falls back to another placement
of the same image. Rendering and spatial deletion use the same origin. With
`Q` omitted or zero, Kettle chooses the smallest concrete placement id, falling
back to the smallest relative placement id only when there is no concrete
parent. A hidden virtual prototype does not shadow that concrete default.
Virtual origins use only cells for the selected registered prototype; missing
explicit parents produce no tile and receive `ENOPARENT`, unless replies are
suppressed. A refused replacement preserves the previously accepted relative
placement. Combined transmit-and-display commands (`a=T`) retain their relative
destination; relative placement does not move the cursor.

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

On macOS, `Cmd+V` uses the same Kettle clipboard paste action. The menu's
Paste action and clipboard fallback for middle-click or `paste_primary` also
use that pipeline. Linux PRIMARY text takes precedence over the clipboard.

Codex CLI 0.155.1 handles bare `Ctrl+V` itself. A successful image attachment
appears as `[Image #1]` in its composer, without Kettle's thumbnail receipt.
Kettle cannot confirm a client-owned clipboard read. Both `Ctrl+Shift+V` and
`Ctrl+V` were verified with a clipboard bitmap in the local Codex composer;
this does not assert that a prompt was submitted or a model read the image.
Copying an existing image file pastes its path and does not create Kettle's
bitmap thumbnail. Preview settings, focus, and later input also affect receipt
visibility. These distinctions prevent a missing thumbnail from being mistaken
for a failed attachment.

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

## Agent display, not model attachment

Two directions are easy to confuse. Attaching an image to a prompt, above,
gives it to the model; Kettle only carries the keys or the path. Agent
previews go the other way: an agent sends media to the user through
`kettle_show` (`kettle mcp --display`, or the full `kettle mcp`) or `kettle
show`, and Kettle renders it for the user alone. The model never receives the
pixels, and every result says so: showing is not seeing.

Delivery depends on the harness and how it was started:

| Harness | How it gets `kettle_show` | What the user sees |
|---|---|---|
| Claude Code in a Kettle pane | Kettle's plugin, offered to new panes with Claude Code previews on | a card under the call, and the pane's media shelf |
| Codex in a Kettle pane | the `codex` function `kettle agent-setup --print` prints, or Codex previews | a card under the call with Codex CLI 0.162 on macOS once its hook is trusted; the shelf otherwise |
| Any MCP client registered by hand | `kettle mcp --display` | the shelf |
| A shell, or an agent's shell tool | `kettle show` | the shelf; Codex's command sandbox blocks Kettle's socket, so it uses MCP |

Outside Kettle, the instructions say not to call `kettle_show`, and a call is
refused with `not_in_kettle`. In tmux inside a Kettle pane, Kettle can place
the session only by the `KETTLE_PID` the tmux server inherited: media lands in
the pane tmux was started from, marked as from an unverified sender, or is
refused. Over SSH, in a container or on another machine, the session is not in
a Kettle the server can reach, and nothing is shown. Without any agent, the
user can still preview a diagram they copied (`render_clipboard_as_diagram`)
or selected (`render_selection_as_diagram`), or a file a pane names
(`preview_link`). See [Automation and MCP](AGENT.md#showing-media).

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

A keybinding is therefore the only way to give a Super chord meaning in a
legacy pane, and macOS ships one: `Cmd+Backspace` is bound to `text:\x15`,
the `^U` that deletes to the start of the line. Kettle uses a binding here
rather than an encoder fallback because a binding fires whatever the client
negotiated. A fallback would defer to the Kitty protocol, so it would go dead
in exactly the TUIs that negotiate it. `keybind = cmd+backspace=unbind` gives
the chord back to the application.

Option is separate from Super, and `macos-option-as-alt` covers only keys that
produce text. The policy exists so `⌥e` can compose `´` instead of sending
`ESC e`. Keys that compose no character (Return, Backspace, Delete, the
arrows, Home/End, Page Up/Down, Insert and the F-keys) always carry Alt to
the encoder, on every setting and from either Option key, because there is no
composition to protect. `⌥⌫` is `ESC DEL` and `⌥←`/`⌥→` are
`CSI 1;3D`/`CSI 1;3C`. `⌥↩` is Alt+Return, encoded as the table below
gives it: `CSI 13;3u` to a program that negotiated the kitty protocol, which
Codex and Claude Code read as "insert a newline", and `ESC CR` where no
keyboard mode applies, a newline in zsh's emacs keymap. bash leaves `ESC CR`
unbound and rings the bell, and a canonical-mode prompt (`read`, a password)
keeps the ESC in its line, as in every terminal that sends Alt as ESC. kitty
draws the same line for the same reason.

What the application does with `ESC DEL` is then its own business, and one
client is worth naming. Neovim leaves `<M-BS>` unmapped, so `⌥⌫` does not
delete a word there — in any terminal, since they all send this same sequence.
Its mapped word delete is `<C-w>` (`i_CTRL-W`). One line closes the gap:

```lua
vim.keymap.set("i", "<M-BS>", "<C-w>")
```

`⌘⌫` needs nothing: `^U` is `i_CTRL-U` in insert mode, which deletes the
entered characters on the line. In normal mode it scrolls instead, which is
the trade the shipped binding makes.

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
| No negotiation; `auto` at a canonical/unknown shell prompt | `0D` | `0D` | `0D` | `ESC 0D` |
| No negotiation; `auto` in a recognized agent composer, or `always` | `0D` | `ESC [ 27;2;13~` | `ESC [ 27;5;13~` | `ESC [ 27;3;13~` |
| Negotiated xterm level 0 | `0D` | `0D` | `0D` | `ESC 0D` |
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

## Keys a program shares with Kettle

Some Kettle default chords are also application keys: Codex answers a queued
question with `Shift+Left` and steps back with `Shift+Right`, Codex and Claude
Code extend selections and change reasoning effort with `Shift+Arrow`, Neovim
moves by word with `Shift+Left/Right`, AstroNvim resizes windows with
`Ctrl+Up/Down`, and `Ctrl+_` is undo in Claude Code and most line editors.

Kettle decides who gets such a chord from what the program has told the
terminal, not from its name. A program owns the keyboard while its screen:

- is the alternate screen, or has mouse reporting on (Neovim, fzf, htop), or
- has negotiated the kitty keyboard protocol or xterm `modifyOtherKeys` while
  shell integration does not show the shell at its prompt (Codex, Claude Code).

A plain shell prompt owns nothing, and neither does a fish prompt that pushes
its own kitty flags, even before its first command. The rule also works over
ssh, because the remote program sets the same modes. While a program owns the
keyboard:

- `Shift+Arrow` goes to the program; Kettle resizes splits with it only when
  no program has the keyboard. Drag the divider meanwhile, or bind resizing to
  a chord of your own (`keybind = ctrl+alt+left=resize_left`).
- Jump to prompt (`Ctrl+Up/Down`, and `Cmd+Up/Down` on macOS), the scroll
  chords, `Shift+Home/End`, tab switching and `Alt+1`-`9` (and `Cmd+1`-`9` on
  macOS) go to the program only when Kettle's action would do nothing: no
  prompt to jump to, nothing to scroll, one tab, no selection to extend. While
  the view is scrolled back into history they stay Kettle's, because a key
  sent to the program would snap the view to the bottom.
- Pane focus (`Ctrl+Shift+N/P`, and `Cmd+Opt+Arrow` and `Ctrl+Cmd+Arrow` on
  macOS) goes to the program when focus has nowhere to go: a zoomed split,
  which shows only its focused pane, a tab with one pane, or no pane on that
  side. It does so only while the program's keyboard protocol sends the chord
  as itself: the kitty protocol for every one of them, or `modifyOtherKeys`
  level 2 for `Ctrl+Shift+N/P`. Without either, `Ctrl+Shift+N` would arrive as
  `Ctrl+N` and a Command chord as nothing, so they stay Kettle's. `Alt+Arrow`
  on Linux and Windows falls through with nowhere to go for any program or
  shell, unless you bind it yourself.
- `Ctrl+Shift+X` (zoom) stays Kettle's: it is how a zoomed split comes back.
  On a tab with one pane it does nothing.
- Copy stays Kettle's: Codex reads `Ctrl+Shift+C` as `Ctrl+C`, which would
  discard its draft.
- Every other chord stays Kettle's, and so do all of them while broadcast
  input is on, since the other panes may not own their keyboards.

A chord you bind yourself is always Kettle's, even when your line restates the
default. `keybind-yield = off` keeps every default chord Kettle's, as in 4.8.

`Ctrl+Shift+-` and `Ctrl+Shift+_` (`Ctrl+_`) are no longer bound to shrinking
the font, whatever `keybind-yield` says; `Ctrl+-` and `Cmd+-` still are. To
shrink with them again, add `keybind = ctrl+shift+minus=decrease_font_size`
and `keybind = ctrl+shift+_=decrease_font_size`.

The pointer works the same way. While a program has mouse reporting on, it
gets your clicks, and Shift keeps a click Kettle's: Shift+drag selects text,
and Shift+right-click opens Kettle's menu, which also offers "Preview Copied
File" when the clipboard names an image, SVG or Mermaid file. With text
selected, Shift+right-click off the selection extends it, as in xterm, and on
the selection opens the menu. With `putty-paste-style` on, a right-click
pastes instead, Shift or not.

## Claude Code diff panel

Claude Code 2.1.260 and newer draws a diff panel beside the conversation
inside its own fullscreen TUI (`"tui": "fullscreen"` in `~/.claude/settings.json`,
or `CLAUDE_CODE_NO_FLICKER=1`), showing the session's or the repository's
uncommitted changes as it edits. It is a client feature; Kettle transports it
like any other alternate-screen application, and nothing terminal-specific is
required (`TERM_PROGRAM=kettle` is fine). What the client does check is the
column count of the pane it runs in:

- `/diff` toggles the panel from **110 columns**; below that the client answers
  "Resize your terminal to at least 110 columns to show the diff panel".
- The panel **auto-opens at 144 columns** once the session has changes in a
  git repository (`session`, `uncommitted`, and `branch` bases are cycled from
  the panel).

Kettle's default fresh window is sized for this (about 152 columns on a 1080p
monitor at the default font; see `window-width` in [CONFIG.md](CONFIG.md)).
A split pane is narrower than the window, so zoom the pane running Claude
(`Ctrl+Shift+X`) when the panel matters; `Alt+Arrow` still reaches the client
while zoomed. `just default-window-size-smoke` proves the fresh-window width,
and the opt-in `KETTLE_AGENT_AUTH_SMOKE=1 just agent-tui-smoke` drives a live
Claude Code session through both answers.

## tmux and full-screen clients

- Kettle forwards application cursor/keypad modes, focus reports, SGR mouse
  events, bracketed paste, alternate-scroll behavior, resize events, Kitty
  keyboard negotiation, OSC 8 links, OSC 52 clipboard writes, styled
  underlines, and synchronized updates through the PTY. OSC 52 target `c`
  addresses the regular clipboard; `p`/`s` addresses Linux PRIMARY without
  falling back to the regular clipboard when a PRIMARY operation fails.
- Focus reports (DEC mode 1004, tmux `focus-events`) follow the pane that holds
  the keyboard, not only the window: a pane that enabled them hears `CSI O`
  when focus moves to another pane or tab (or the window loses OS focus) and
  `CSI I` when it comes back, once per change. Zooming changes nothing.
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
  (`1` means enabled). `just agent-tui-smoke` performs the DA1 check on tmux
  3.4 and newer and cross-checks the format on tmux 3.6 and newer.

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

### Codex suspension and shell editing

On Unix, Ctrl+Z suspends Codex; `fg` resumes the same session. Codex CLI 0.155.1
can re-enable enhanced keyboard reporting before its process actually stops.
The shell then receives CSI-u sequences instead of its normal editing keys.
Visible symptoms include `7;3u` or `7;5u` after Alt+Backspace or Ctrl+Backspace.
The race can happen on the first suspension and does not require `bg`.

[Kettle issue #328](https://github.com/Reddimus/kettle/issues/328) records the
reproduction and protocol evidence. The upstream suspend race is discussed in
[Codex #26564](https://github.com/openai/codex/issues/26564), with the related
background ownership problem in [Codex #37088](https://github.com/openai/codex/issues/37088).
Kettle follows the client's negotiated modes. It does not clear them based on
a process name or assume every foreground shell uses legacy keyboard input.

See [Unix suspend/resume compatibility](TESTING.md#unix-suspendresume-compatibility)
for the offline regression scenario and the explicit real-Codex probe. Until a
fixed client is installed, exiting Codex normally avoids this suspension path.
The fixed interactive TUI remains stopped after `bg`; use `fg` to resume it.

At an empty shell prompt, `printf '\033c'; stty sane` restores terminal modes
and shell editing. This clears the terminal display; the suspended job remains
stopped until `fg`. The recovery was checked against a client fixture that
deliberately leaves enhanced keyboard reporting enabled while suspended.

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
