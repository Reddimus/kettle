# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Added

- `kettle-media`, a new crate, defines the bounded media protocol for the
  coming agent visuals: jobs and results, caps, source authorization, the
  build handshake and the binary frames between Kettle and a media worker.
  Nothing uses it yet, so no behaviour changes.
- `kettle ctl get_state` reports `media`: whether media previews are
  available, and a fixed reason when not. Kettle looks for its media worker
  only beside its own executable, never in `PATH` or the working directory,
  and checks the file and, on macOS, its code signature before trusting it.
  No worker ships yet, so it reads `worker_missing`. Kettle and its workers
  now share one build identity, a hash of the source they were built from.
- `kettle-media-worker`, the media worker executable, is built with the
  workspace. Before reading anything it closes inherited descriptors, turns
  off core dumps and lowers its resource limits, and a watchdog ends it if
  its parent stalls. It renders nothing yet, and nothing starts or ships it.
- `kettle ctl dispatch_ui_key` drives quick-select hint mode too, in the order
  the keyboard reaches it: right after a confirmation.
- In quick-select hint mode (`Ctrl+Shift+H`), holding Shift while typing a
  label opens a path in the default app and copies a URL. A plain label still
  opens a URL and copies everything else. A path resolves from that pane's
  directory; it is copied instead when it names another machine's file (an
  ssh, mosh or container session), when the label covers only part of a name,
  or when it climbs with `..`, and behind tmux, screen or zellij Kettle asks
  first. On Windows path hints are copied.
- `language` (`auto`, `en`, `es`; Settings → Behavior → Language) chooses the
  language of Kettle's own text: Settings, menus, the Dock menu, prompts,
  notifications and screen-reader names. `auto`, the default, follows the
  operating system's locale. A change applies when Kettle restarts. The
  Spanish text is a first draft awaiting review.
- A theme picker: type to filter the 500+ bundled themes by name, or move
  through them with the arrows, and the window previews the selected theme.
  Enter keeps it and Esc puts back the theme you started with. With nothing
  typed it lists the current theme's appearance first, each theme followed by
  the most similar one, and tags each row dark or light. Open it from the
  Settings Theme row (Enter or a click), right-click → Theme…, the command
  palette's "Choose theme…", or the bindable `open_theme_picker` action.

### Changed

- A clicked file link or path in a pane connected to another machine no
  longer opens the local file of the same name: Kettle says it names a file
  there instead. That covers a detected ssh, mosh or container session and a
  remote client in the pane, also when run through `sh -c`, `env` or `sudo`.
  Behind tmux, screen or zellij, which may or may not be on this computer,
  Kettle asks before opening it. Web and mail links open as before.
- Kettle no longer opens programs or shortcuts from terminal links: a
  clicked `file://` link, OSC 8 link or path to an `.exe`, `.app`, `.lnk`,
  `.desktop`, `.rdp`, script, executable file without a document extension
  (`a.out`), macOS alias or Linux launcher is refused with a notification
  instead of being run. A symlink is judged by what it points at, a name is
  read as Windows reads it (`payload.exe.`), and a configured custom URL
  handler gets a file link only after the check. Documents and folders open
  as before, though a file link's `?query` or `#fragment` is no longer passed
  on, and one with an encoded `/` or `\` (`%2F`, `%5C`) is refused.
- `TRANSLATING.md` explains how Kettle's text is translated and reviewed, and
  each Spanish message's review status is recorded by key. The Spanish
  catalogue is a draft until a fluent reviewer accepts it.
- The tab bar's numbered fallback label (`tab 2`) and an untitled pane's
  screen-reader name follow the UI language. `kettle ctl` tab titles keep the
  English `tab 2`.
- The right-click menu's Theme submenu, a list of every bundled theme, is now
  a Theme… row that opens the theme picker.
- Screen readers read a picker row's hint after its name, such as a
  command's shortcut or a theme's appearance.
- Stepping through themes (`next_theme`, `prev_theme`, and ←/→ on the Settings
  Theme row) stays with the current theme's appearance and moves to the most
  similar theme each time, judged by background, foreground and accent. It used
  to follow the themes' names or the popular list's order, flipping between
  light and dark palettes. `toggle_light_dark` still switches appearance.
- Close confirmations count in words that match the number: "Close tab with 1
  pane?", "Close 3 panes?". The paste prompt says "1 line" or "2 lines". They
  used to say "pane(s)" and "lines" whatever the count.

### Fixed

- On Linux, a video poster that tumbler (Xfce's thumbnailer) cached is used:
  it records the file's modification time with a fraction of a second, which
  Kettle read as stale. A different time or file still does not match.
- A video paste after an update replaced Kettle no longer asks the new build's
  preview helper to read the old build's request. On Linux the helper is the
  running Kettle itself; elsewhere a helper from another build says so, and
  Kettle shows no video card and logs once that previews wait for a restart.
- Opening the current directory in the file manager works for a folder whose
  name holds a space or `#`. The link was refused, or named another folder.
- A `file://` link or a reported working directory (OSC 7) is checked after
  its escapes are decoded: `.%2e`, `%2F%2Fhost`, `%00` and other spellings of
  a climb, a network share or a control character are refused, as are a bad
  escape in a link and an encoded `/` (or `\` on Windows). They could slip
  past checks that only looked at the encoded text. A local WSL share is
  still accepted as a working directory.
- A path in quotes or backticks is one link and one quick-select target,
  spaces included: `"docs/annual report.pdf"`, Python's `File "/my app/x.py"`
  and `` `out/report #1.pdf` `` open the whole file. They used to stop at the
  first space or `#`. Quoted prose such as `"see src/main.rs"` still links
  only the path inside it.
- A URL in Markdown code or emphasis (`` `https://…` ``, `**https://…**`) is
  linked and quick-selected without the trailing backtick or asterisks.
- A URL or path the terminal wrapped onto the next row is one link and one
  quick-select target: clicking either row opens the whole URL, and hovering
  lights both. It used to be two broken links. A program's own line breaks
  are never joined, and a URL running off the visible rows is not linked until
  all of it is in view.
- A path that begins inside a longer token is no longer a link or a
  quick-select target: `foo(1)/bar.png` held a clickable `/bar.png`, which
  opened an unrelated file. Paths after a space, a bracket, `=`, or a list,
  chain or redirect separator (`PATH=/a:/b`, `&&/usr/bin/x`, `>/tmp/out`) are
  unchanged.
- Quick-select hints and double-click selection take a relative path whole:
  `out/diagram.png`, not `/diagram.png`, which named a different file. A
  drive-letter path (`C:/out/diagram.png`) is taken whole too, and an IPv4
  network such as `10.0.0.1/24` stays an address rather than a path.
- A compact paste receipt in a remote pane shows its whole warning, "Remote ·
  local path". It was cut to "Remote · local path o…".
- A key that closes or replaces a modal (the command palette, a picker, a
  title editor, the Settings path prompt) ends an input-method composition in
  progress there, so a late commit can no longer type into the terminal or the
  next modal.
- Dragging a pane onto another shows where it will land with the search bar
  closed. The drop preview appeared only while search was open.
- In the window, tab, pane and group title editors, the input method's
  candidate window opens at the caret. It opened two or three columns to the
  caret's left.
- A Settings keybind row for an action with more than one shortcut shows the
  same shortcut on every launch, the one the command palette and menus show.
  It used to show any of them, and could change between launches.

## [4.9.0] — 2026-10-02

### Added

- `RUST_LOG=warn,kettle::startup=info` prints how long each startup phase
  took, from `main` to the first frame.
- `keybind-yield` (`auto` by default) decides which default chords a program
  in the pane gets while it owns the keyboard; `off` keeps every default chord
  Kettle's, as in 4.8.
- Kettle answers XTVERSION (`kettle(<version>)`), DECXCPR, DECRQSS (SGR,
  scroll region, cursor shape) and XTGETTCAP (truecolor, styled and coloured
  underlines, cursor shapes, palette size), and DECRQM for modes 47 and 1047.
  Claude Code now draws with synchronized output in Kettle, and Neovim finds
  truecolor without `COLORTERM` (over ssh) and draws undercurls as curls.
- Programs can follow Kettle's light or dark theme: `CSI ? 996 n` answers
  the scheme, and DEC mode 2031 reports every change of the theme's colours,
  whether from `theme-mode = auto`, `toggle_light_dark` or a theme picked by
  hand. Claude Code's automatic theme switches with Kettle.
- `macos-cursor-blink-layer` (`true` by default, macOS only): once a window has
  drawn nothing but its cursor blink for half a blink period, the window server
  blinks the cursor and Kettle stops drawing until something changes. `false`
  keeps redrawing the window at every blink. `ui_geometry` reports who draws the
  blink under `cursor_blink`.

### Changed

- On macOS, an eligible first shell starts before AppKit finishes launching,
  sized from the measured font cell to the exact configured grid. Restored
  sessions and later windows keep their existing startup paths.
- Reading `ui_geometry` and closing a control connection that attached no pane
  no longer request a frame. Polling geometry no longer forces a repaint on any
  platform. Agents and resize probes must request an action when they need one.
- Settings use consistent display text and unit spacing, stable label columns,
  separate label/value ellipses in narrow panels, evenly spaced category names,
  and contextual dependency and timing notes.
- Recolouring trailing blank cells, such as a reverse-video block or a
  prompt's padding, no longer reshapes the row's text. Rows shape through
  their last non-blank cell, with face-matched padding to preserve baselines.
- A blinking cursor no longer re-uploads the window's shapes or re-prepares its
  text on each blink; the blink only changes what is drawn.
- A window whose content is not changing writes nothing to the GPU, a
  blinking cursor included. On macOS the driver then releases its blit pool:
  3 s after a burst of output Kettle's memory is 236 MiB instead of 365 MiB
  (10 paired rounds on an Apple M5 Max, B/A 0.647, 95% CI 0.645-0.653). While
  any frame draws, the render pool (about 168 MiB) stays.
- On macOS and Linux GPUs that share memory with the CPU (Apple silicon, and
  integrated or software Vulkan adapters), Kettle writes the quads and glyphs
  that change in a frame straight into memory the GPU reads, instead of
  copying them through a staging upload. Elsewhere uploads work as before.
  Output in the default grid text mode no longer re-prepares the window's
  unchanged chrome text.
- Kettle finds and loads its fonts on a thread while the event loop starts,
  instead of on the main thread before the first window and shell. The first
  window waits only for whatever font work is still unfinished when it opens.
  New windows (`Cmd+N`) load fonts as before.
- A new window builds each distinct quad and image GPU pipeline once, instead
  of once per layer, and on macOS no longer decodes the window icon, which
  macOS ignores. Both were startup work with no effect.
- `Shift+Arrow` reaches a program that owns the keyboard (the alternate
  screen or mouse reporting, or the kitty keyboard protocol or
  `modifyOtherKeys` away from the shell prompt) instead of resizing a split,
  so Codex's `Shift+Left` ("shift+← to answer" a queued question) and
  `Shift+Right`, and Neovim's `Shift+Arrow` motions, work. Jump to prompt,
  scrolling, `Shift+Home/End` and tab switching reach such a program when
  Kettle's action would do nothing and the view is not scrolled back. At a
  shell prompt, with broadcast input on, and for chords you bind yourself,
  nothing changes.
- `Ctrl+Shift+-` and `Ctrl+Shift+_` no longer shrink the font, so `Ctrl+_`
  reaches the program (undo in Claude Code, zsh, bash and emacs). `Ctrl+-` and
  `Cmd+-` still shrink it.
- When focus has nowhere to go (a zoomed split, which shows only its focused
  pane, a tab with one pane, or no pane on that side), the pane focus chords
  `Ctrl+Shift+N/P`, and `Cmd+Opt+Arrow` and `Ctrl+Cmd+Arrow` on macOS, reach a
  program that owns the keyboard, if its keyboard protocol sends the chord as
  itself (the kitty protocol, or `modifyOtherKeys` level 2 for
  `Ctrl+Shift+N/P`). `Alt+Arrow` on Linux and Windows already did.

### Fixed

- A scheduled light/dark theme (`theme-schedule`) now switches at its time even
  while the window draws nothing, such as an idle window, or one whose cursor
  the window server blinks. Before, it waited for the next keystroke, output
  or other repaint.
- A program that asked for focus reports (DEC mode 1004: tmux with
  `focus-events`, Neovim's `FocusGained`/`FocusLost`, Codex and Claude Code)
  hears focus leave its pane when focus moves to another pane or tab, and
  return when it comes back. Kettle used to report only the window gaining or
  losing focus, so a pane in a hidden tab still believed it had the keyboard.
- Underlines under detected paths and URLs stay on their text after a pane
  changes size without new output: zooming a pane, splitting next to it or
  resizing the window. They used to keep the old width's positions, so they
  ran across the wrong words until something was printed.
- A runtime diagnostic incident appears in `<cache>/kettle/diagnostics` only
  once it is complete; a reader could see one empty or half-written.
- A key that closes or replaces one of Kettle's own modals (Enter in the
  command palette, a dialog, the layout picker or the SSH launcher, Escape in
  vi mode or a Settings text field) no longer acts again when held: holding
  Enter to confirm used to send the program behind it an extra Enter per key
  repeat, or launch the first entry of the layout picker the palette had just
  opened, and holding Escape in a Settings text field or a keybind capture
  closed Settings too. The Escape that cancels a pane or tab drag is held
  back the same way. Inside a modal, keys that activate, toggle or pick act
  once per press, so holding Enter no longer runs a context submenu's first
  row, holding Space or Left/Right no longer flips a Settings toggle or cycles
  a choice back and forth (a number still steps), a held hint letter no longer
  completes a doubled label, and holding `v` in vi mode no longer flickers
  visual mode. Moving keys still repeat.
- A cursor position report (`CSI 6 n`) under origin mode counts the row from
  the top margin, as xterm does, so a program that saves the position and
  restores it with CUP lands where it was.
- `Option+Return` on macOS is `Alt+Return`, so it adds a newline in Codex,
  Claude Code and zsh's emacs keymap instead of submitting. Where no keyboard
  mode applies (no kitty protocol, no `modifyOtherKeys` level that encodes it,
  no modified-Enter fallback), `Alt+Return` sends `ESC CR`, as other terminals
  do, instead of a bare CR.
- `Ctrl+Shift+Space` toggles vi mode, in and out; the default never matched a
  real key press, neither did any `space` chord in a config, and vi mode
  took the chord before it could leave.
- Resizing a split while its tab is zoomed no longer moves the hidden panes,
  which then appeared moved on unzoom.
- `Ctrl+Shift+X` on a tab with one pane does nothing. It used to set a hidden
  zoom that the next split silently cleared. Scaled zoom still enlarges the
  font there.
- An `Alt+Arrow` chord you bind yourself stays Kettle's on Linux and Windows
  even with no pane in that direction, as every chord you bind does.
- Decreasing the font at size 5 no longer raises it to 6; the font now
  shrinks to 5, the smallest size the config accepts.
- Holding a toggle chord (vi mode, zoom, fullscreen, broadcast, read-only,
  and the other toggles) toggles once; key repeat used to flip the state back
  and forth for as long as the chord was held.
- A control-socket embedder that reconnects as soon as Kettle closes its
  connection (after the connection sat idle, say) is served. With all eight
  connection places in use, Kettle announced the close before it freed the
  place, and could refuse the reconnect.

## [4.8.0] — 2026-09-28

### Added

- `split_left` and `split_up` (Ghostty's `new_split:left` and
  `new_split:up`) put the new pane left of or above the focused one. They
  have no default keys.
- The right-click menu adds rows from Ghostty's menu: Split Left, Split Up,
  Close Tab, New Window, Close Window, Set Tab Title…, Set Pane Title…, and
  Reset Terminal.
- In the search bar, `Cmd+G` / `Shift+Cmd+G` (`Ctrl+G` / `Ctrl+Shift+G` on
  Linux) step to the next or previous match, and the find shortcut pressed
  again selects the query. Buttons show hover and pressed states.
- The search query has undo and redo (`Cmd+Z` / `Shift+Cmd+Z`, or `Ctrl+Z` /
  `Ctrl+Shift+Z` on Linux). On macOS, `Cmd+Backspace` / `Cmd+Delete` delete to
  the ends of the query, `Ctrl+A` / `E` / `B` / `F` / `D` / `H` work as in any
  macOS text field, and `Ctrl+K` deletes to the end when `vim-menu-nav` is off.

### Changed

- An idle window on macOS uses about 97 % less CPU and wakes about 30 %
  less often. Kettle turns off macOS window restoration ("Resume"), which
  kept saving the app's state while nothing happened. macOS never
  reopened Kettle's windows from that state, since they have no restoration
  class; `restore-session` is the setting that reopens them.

### Fixed

- Text shaping includes an unreleased upstream cosmic-text fix for lines that
  hold several bidi paragraphs, such as right-to-left text after a paragraph
  separator. Kettle carries the fix in a vendored copy until a release has it.
- Kettle's shortcuts work while the search bar is open. A new tab, font zoom,
  tab switching, split focus and the rest did nothing until the bar closed.
  On Linux, `Ctrl+Shift+A` and `Ctrl+Shift+X` split and zoom again instead of
  selecting or cutting the query, and `Alt+1`…`9` switch tabs instead of
  typing a digit.
- The search bar's text is centered in its buttons and editor. Labels sat
  5 px low and flush against the left edge of each button, and the query's
  characters jumped a column whenever the caret moved or focus changed.
- Clicking a search button no longer takes focus from the query, and a
  button now acts on release. Wrap and Invert keep the current match instead
  of jumping back to where search opened, and Case searches again from it.
- CJK and emoji in a search query no longer push the controls after the
  editor out of place, and the caret and selection now sit on the glyphs.
- Space on a focused search button presses it instead of typing a space, and
  a focused button's label is readable on the accent color. Holding Space or
  Enter on a toggle or Close acts once, and the held key no longer types into
  the terminal once the bar closes.
- The search status no longer flashes `Searching…` on every keystroke of a
  query that has no match.
- A search that restarted after output stopped could stay at `Searching…`
  with nothing running when no match was on screen. It now goes on to search
  the rest of the history.
- Reset, Clear Scrollback, and Reset and Clear now act on the terminal.
  They were typing `ESC c` or `CSI 3 J` into the running program, so in zsh
  or bash Reset capitalized a word instead of resetting. Like Terminator's,
  they also work on a read-only pane, since nothing reaches the program.

## [4.7.0] — 2026-09-28

### Changed

- Scrolling inside a scroll region, as `vim`, `less` and status-line TUIs
  do, is up to twice as fast. The region's rows move in one step instead
  of one row at a time.
- An idle window wakes about 0.3 times a second instead of 1.3. The
  event-loop watchdog now sleeps while the loop is idle instead of checking
  every second.
- A confirm bar draws its destructive button, such as Close or Delete, with a
  bold label, so it stands apart from Cancel.
- Output no longer restarts the cursor blink after `cursor-blink-timeout`.
  Only typing, pastes, focus, and settings changes do, as in kitty and
  Alacritty, so a window whose program keeps printing stops redrawing for
  the cursor.
- The first shell starts before the window and GPU are set up, about 50 ms
  sooner. The font is measured first, and the renderer reuses it instead of
  loading the system fonts a second time.
- `window-width` and `window-height` open a window at exactly that grid. A
  120x36 request used to open at 123x35. The default window keeps its size.

### Fixed

- `theme-schedule` clock times and the status-bar clock use local time. Both
  used to run on UTC, so `19:00 dark` switched at 12:00 in California.
- An explicit `font-family` or `font-size` wins over Terminator-style
  `font = Mono 10` wherever the lines sit, and the `font` size is clamped to
  5 to 72 points like `font-size`. `--check-config` now flags an out-of-range
  `font` size.
- A `trigger` pattern's `^` and `$` anchor to each row of output, so a
  pattern like `^Build failed: (.+)$` can match. They used to anchor only to
  the start and end of the whole visible screen.
- `background-animation = off` freezes the starfield. It used to jump forward
  on every repaint from typing or output.
- Quick-select hint labels use the new font after a font-family change. They
  could keep the old font until the labels themselves changed.
- Zooming all panes after a scaled zoom keeps the new size when you leave the
  scaled zoom. It used to snap back to the size from before.
- A mistyped `--profile` is reported with `--gpu-info` and `--config-path`
  even when `--check-update` is also given. The combination used to show the
  default config instead.
- Disabled menu items and shortcut hints are readable: they keep 45% of the
  text color instead of a sixth, about 3:1 contrast on TokyoNight instead of
  1.6:1.
- `scripts/gen-starfield.py` writes a 1280×720 loop that fits Kettle's 128 MiB
  animation cap, so all 32 frames play. At 1920×1080 only 16 loaded and the
  loop jumped halfway. The BACKGROUNDS example `ffmpeg` command fits the cap
  too.
- A Lua plugin refuses to load if Kettle cannot remove the unsafe standard
  functions from its sandbox, as documented. Failures used to be ignored.
- `just install`, `uninstall`, `install-local` and `install-recording` run only
  on Linux. On macOS they ran the Linux installer.

## [4.6.0] — 2026-09-27

### Added

- `cursor-blink-timeout` stops the cursor blink after 10 seconds without
  typing or output, leaving the cursor visible. Set it to `0` to keep the old
  behavior. An idle window then stops redrawing, which on macOS drops its
  memory from about 350 MiB to about 35 MiB and its CPU use to nearly zero.

### Changed

- `kettle exec --json` streams large output 3 to 5 times faster with about 70%
  less CPU, because each event is written to stdout in one call instead of
  about 30.
- On macOS, Kettle reads only each pane's own processes when it checks for
  SSH sessions and shell directories, instead of every process on the
  machine. An idle window with a blinking cursor uses about 80% less CPU.

### Fixed

- `kettle exec` without `--cwd`, and the MCP `kettle_run` tool without a
  `cwd`, now run the command in the current directory as documented. They
  used to run it in your home directory, as did `--cwd DIR` when DIR was
  deleted just as the command started.
- `kettle exec` accepts a working directory whose name is not UTF-8, which
  Linux allows, instead of refusing to start.
- `kettle exec`, and the MCP tools built on it, stream large command output
  about 100 times faster on macOS. 8 MiB used to take 23 seconds.
- `kettle exec --timeout` no longer warns that stdout was not fully delivered
  when every byte was delivered, so the warning now means output was dropped.
  A timed-out `--json` run now always ends with its exit event while stdout
  is being read.
- A command with an enormous argument list in one pane, such as
  `nvim $(git ls-files)`, no longer freezes SSH labels and shell directories
  in every other pane until it exits.
- On macOS, new tabs, splits, and windows no longer pause about 100 ms each
  when Kettle starts from a shell with a high open-file limit, as VS Code and
  other Node-based tools set.

## [4.5.2] — 2026-09-26

### Fixed

- New splits, tabs, and duplicates open where the shell is now, and labels keep
  up. A shell that never reports its directory is read from the OS, which can
  lag a `cd` by about 200 ms.
- Directory reports from local shells are no longer dropped after macOS renames
  the host on a network change.

## [4.5.1] — 2026-09-22

### Fixed

- Clipboard pastes now fall back correctly for empty or non-text Linux selections and preserve images and files on macOS.

## [4.5.0] — 2026-09-20

### Added

- Select, copy, follow links, scroll, and use terminal mouse reporting while
  search stays open. Clicking another split moves the search there. Copy uses
  the query selection when present, otherwise the grid selection.

### Changed

- The visual bell flashes only the ringing pane and fades over 300 ms.
  `bell-flash-intensity` now measures a CIE L\* lightness step instead of blend
  alpha. The default is `0.03`; custom values may need raising. Use
  `0.06`-`0.10` for a stronger flash, `0` to disable, or `1` for solid foreground.
- Fresh windows target a `160x45` cell baseline, fitted within 90% of monitor
  width and 85% of height. Explicit sizes and restored geometry take precedence.
  Set `window-width = 98` and `window-height = 35` for the previous size.

### Fixed

- On Linux, zoomed panes pass `Alt+Arrow` to the terminal. Unzooming restores
  directional focus between visible panes.
- Explicit window sizes now use logical pixels, preserving columns on HiDPI.
- Linux session scans tolerate processes exiting while `/proc` is read.
- Linux timeout cleanup allows delayed stop acknowledgement within its existing
  500 ms budget.
- Update rustls to fix RUSTSEC-2026-0285.

## [4.4.0] — 2026-09-10

### Added

- **Recording retention is configurable.** `record-max-bytes`,
  `record-max-files`, and `record-max-directory-bytes` override the 512 MiB /
  50 file / 5 GiB defaults. Unset keeps the default. `record-max-bytes` has a
  1 KiB floor and `record-max-directory-bytes` a 1 MiB floor, rather than
  accepting any non-zero value: a cast that cannot hold its own header would
  stop the recorder from ever starting, and a bare `500` meant as 500 MB would
  make the next recording delete every completed cast in the directory.

## [4.3.1] — 2026-09-04

### Fixed

- **Linux self-update keeps future changelog archives installable.** The
  updater now installs every manifest-verified
  `docs/changelog/CHANGELOG-<major>.x.md` file instead of stopping at the
  current `3.x` archive. It rejects other names and nested entries before they
  can add an install destination.
- Unsafe terminal URLs no longer echo their untrusted payload into logs.
- Modal overlays now appear in the accessibility tree and move accessibility
  focus to the control that owns keyboard input. Confirmations also stay above
  other bottom-bar overlays.
- Settings scroll to keep the focused row visible in short windows and
  ellipsize long rows in narrow windows.
- Search selection and pointer mapping now account for the painted caret, and
  quick-select preserves paths and URLs containing wide glyphs.
- Large sixel images that fit the configured byte budget are accepted even
  when geometric capacity growth would exceed it; Kitty root-frame edits now
  replace the root instead of appending a duplicate frame.
- Update extraction, activation, config-path, and control-directory edge cases
  now fail safely without unbounded reads, busy loops, relative private paths,
  or divergent ownership checks.

### Changed

- Minimum contrast is cached by transformed color pair for each frame. The
  release diagnostic benchmark reduced a 100,000-cell, four-color workload
  from a 76.93 ms median to 1.21 ms (about 63.7 times faster on the test Mac).
- Glyph-atlas evictions coalesce freed regions in batches, layout-picker
  entries are read once per open, and Kitty deletes skip grid and image-origin
  snapshots when there are no relative placements.

## [4.3.0] — 2026-09-04

### Added

- **macOS directional pane focus now answers `Cmd+Opt+Arrow`.** The chord was
  unbound, so a user arriving from iTerm2 or Ghostty pressed it and got
  nothing: no movement, no error, and no hint that a different chord existed.
  Silent failure is the one kind a user cannot recover from by trying harder.

  A plurality among peer terminals, not a consensus. Ghostty ships
  `super+alt+arrow_left=goto_split:left` and iTerm2 documents the same chord
  for Select Split Pane, but WezTerm uses the portable `Ctrl+Shift+Arrow` and
  kitty ships no directional default at all. Ghostty's and WezTerm's defaults
  were read off the installed binaries; iTerm2's and kitty's come from their
  documentation.

  `Ctrl+Cmd+Arrow` keeps working and is not deprecated. Unbinding it would not
  hand the chord to anything better, because a Cmd-bearing chord has no PTY
  encoding and would simply go dead. `Ctrl+Opt+Arrow` is deliberately left
  alone: Ctrl+Option is the VoiceOver modifier, and VO+Arrow moves the
  VoiceOver cursor. Bare `Option+Arrow` still reaches the shell as word motion.
  Linux and Windows keep Terminator's `Alt+Arrow`.

  iTerm2 and Ghostty also cycle splits with `Cmd+[` / `Cmd+]`, which kettle
  does not adopt. Brackets sit behind Option on the German, French, Italian,
  Spanish and Nordic layouts and macOS reports the modifierless character once
  Command is held, so a character binding is unreachable there; binding the
  physical position instead lands on `+` on German, which is already
  `Cmd++` (increase font size). `Ctrl+Shift+N` / `Ctrl+Shift+P` cycles panes on
  every platform and layout.

  Verified with real keystrokes through the window server, not control-API
  injection: a deterministic 2x2 split, asserting the expected target pane for
  every direction from every pane, 16/16 for the new chord and 16/16 for the
  old one. Every new and extended guard was confirmed to fail against its own
  bug, over six mutations, including one that reproduces a README rewrite
  deleting the pane-focus rows while every test stayed green.

### Changed

- Kitty relative-placement deletion now builds one parent-to-children index
  and walks each stored relation once in both the decoder and terminal
  registries. Deleting the root of a full 256-placement chain no longer
  repeatedly rescans every remaining relation and every removed parent.
- **Windows distribution remains retired while retained code stays in CI.**
  Version 3.3.0 remains the final Windows package. The `windows-latest` leg
  still compiles and tests conditional code, including the CLI, ConPTY,
  PowerShell shell integration, and the retained icon resource, without
  producing an installer or release artifact. Two obsolete ignored `pwsh`
  probes were removed; required native and portable regressions remain.

## [4.2.0] — 2026-09-02

### Added

- **A macOS Dock menu: right-click kettle's Dock icon for New Window and New
  Tab, above the list of open windows.** The menu previously showed only what
  macOS supplies on its own — Options, Show All Windows, Hide, Quit — with no
  way to open a window and not even the window-title list nearly every Mac app
  has.

  Two independent causes. Every row above the system section comes from
  `applicationDockMenu:`, an optional `NSApplicationDelegate` method; there is
  no `NSApplication.dockMenu` property, and the Info.plist route needs a
  compiled nib. winit owns the application delegate, implements exactly two
  methods, and neither is that one — so kettle, which had no delegate code at
  all, contributed nothing. Separately, the open-window list is not free for
  apps with ordinary `NSWindow`s: it is gated on `NSApplication.windowsMenu`
  being non-nil, which winit never sets.

  kettle cannot install a delegate of its own. winit's `ApplicationDelegate::get`
  panics on any other delegate object and runs from the swizzled `sendEvent:`
  and both run-loop observers, so a replacement or forwarding delegate dies
  within milliseconds. The fix builds a runtime subclass of winit's own
  delegate class carrying just `applicationDockMenu:` and isa-swizzles the live
  delegate onto it: a true subclass, no added ivars so the instance size is
  unchanged, nothing overridden, and winit's `isKindOfClass:` check keeps
  passing. Both rows map onto actions kettle already had, so no new action
  exists and the command palette is unaffected. Choosing a row also activates
  the app, without which the new window opened behind whatever was frontmost.

  Drift guards pin the parts that would rot silently: the install call site
  must stay free of `#[cfg]`, both platform arms of the module must exist, the
  AppKit menu features must be declared rather than inherited from winit
  through Cargo feature unification, and the winit version pin is held because
  the subclass depends on a private class name that is not public API. A new
  `just dock-menu-smoke` drives the real Dock through accessibility and clicks
  New Window; the manual right-click is recorded in the appearance gate,
  because the Dock is filtered out of automation screenshots.

## [4.1.0] — 2026-08-28

### Fixed

- **`Opt+Backspace` deletes a word again on macOS, and `Opt+Arrow` moves by
  one.** `macos-option-as-alt` decides whether Option composes text (`⌥e` →
  `´`) or acts as Meta. Kettle applied that decision to every key, so under the
  shipped default (`none`) the Alt bit was stripped before the encoder ran and
  `⌥⌫` arrived as a plain Backspace: one character per press. The `ESC DEL`
  encoding was correct all along and simply never saw the modifier.

  Option composes nothing from Backspace, Delete, an arrow, Home/End, Page
  Up/Down, Insert or an F-key, so the policy no longer masks it for those. They
  carry Alt on every setting and from either Option key, which is the line
  kitty draws too. Keys that do produce text — Enter, Space, Tab, Escape and
  every character key — are untouched, so `⌥e` still composes `´` rather than
  sending `ESC ´`, and nothing gains a stray escape prefix.

  The same mask had also made word editing in Kettle's own search bar
  unreachable: `⌥⌫`, `⌥Delete` and `⌥←`/`⌥→` there now delete and move by word,
  which is what that code was always written to do.

  Verified byte-for-byte against the clients this is used with: zsh, Claude
  Code and Codex CLI all delete the previous word on `ESC DEL` and clear the
  line on `^U`, and tmux passes both through. Neovim is the exception and it is
  not a Kettle one — it leaves `<M-BS>` unmapped, so `⌥⌫` does not word-delete
  there in any terminal; `vim.keymap.set("i", "<M-BS>", "<C-w>")` closes it.
  See [Terminal client compatibility](docs/TERMINAL-CLIENT-COMPATIBILITY.md).

### Added

- **`Cmd+Backspace` deletes to the start of the line on macOS.** Super has no
  legacy terminal encoding at all, so the chord previously reached applications
  as nothing, and no config could fix it: `backspace` was not a bindable
  trigger and no action could send literal bytes. Both now exist. `backspace`
  and `delete` (aliases `bs`, `del`) join the keybind grammar, and the new
  `text:BYTES` action writes a literal byte string to the focused pane as
  though typed — Ghostty's spelling, with `\n` `\r` `\t` `\e` `\a` `\b`
  `\f` `\v` `\0` `\xHH` and `\\` escapes, a 256-byte cap, and `=` written
  `\x3d` because a `keybind` line is split on its last `=`.

  macOS ships `keybind = cmd+backspace = text:\x15`, the `^U` that Ghostty and
  iTerm2's Natural Text Editing preset send. It is a binding rather than a key
  encoding so that it works whatever the client negotiated; in Vim's normal
  mode `^U` scrolls, so `keybind = cmd+backspace=unbind` gives the chord back.
  `Cmd+Left`/`Cmd+Right` are documented as one-line opt-ins rather than
  defaults, because `^A` silently edits the buffer in that same mode.

## [4.0.1] — 2026-08-25

### Fixed

- **The bullet Claude Code prints is a circle again, not a coloured square.**
  `⏺` U+23FA is one cell wide and, per Unicode, renders as text unless a
  variation selector asks otherwise. Kettle drew it from Apple Color Emoji: a
  blue-grey rounded square about two cells wide, covering the space after it.
  Ghostty draws the same character as a plain circle.

  Nothing in the shaping stack consulted `Emoji_Presentation`. cosmic-text takes
  the first family in its cascade whose cmap has the codepoint, and neither the
  bundled JetBrains Mono nor the system text faces have this one, so it reached
  the colour-emoji face by elimination. The width was never wrong; only the face
  was.

  Cells that Unicode renders as text now ask for a monochrome symbol face.
  Emoji that are meant to be colourful are untouched, because they are already
  two cells wide and are excluded by the same rule that selects the text ones. A
  system with no monochrome symbol face installed keeps exactly what it had.

- **Closing a split by typing `exit` now gives its rows back to the pane that
  is left.** Splitting away from a full-screen program, then letting the new
  pane's own shell exit, left the surviving pane's terminal at the size it had
  inside the split. Claude Code kept painting into the top half of a
  full-height pane, still running and still updating, simply convinced the
  terminal was 28 rows instead of 57.

  A pane whose child exits is removed by `Mux::reap`, which prunes it from the
  split tree and promotes its sibling into the whole rectangle. Nothing told
  that sibling's PTY. Because the renderer paints from a live layout, the
  survivor looked correct straight away, which is what made this read as a
  redraw problem rather than a resize one. No `TIOCSWINSZ` went out, so the
  kernel sent no `SIGWINCH`, so the program had no reason to repaint at a new
  size.

  Closing the same split with `Ctrl+Shift+W` always worked, because an explicit
  close runs through the action tail that schedules a resize, and the
  confirm-dialog close had already been fixed for this exact reason once
  before. Reaping was the one close path left without it.

- **Splitting away from an agent no longer produces a pane that vanishes.**
  Splitting clones the focused pane's foreground shell so the new pane lands in
  the same place you were working. A shell was judged interactive by its flags
  alone, so `bash /tmp/hook.sh` counted as one. Agents, git hooks and installers
  spawn helpers in exactly that shape and routinely delete the script straight
  after, so the clone ran a script that was already gone and the pane was reaped
  before it drew. Intermittent, because it depended on what the background
  process scan happened to catch in its last sweep.

  A shell given a script-file operand now counts as running and exiting, the
  same as `-c`. The split falls back to the configured shell, which is somewhere
  to work. The rule applies to the POSIX family, where `sh [options] file` is
  standardized; fish, nu, elvish, xonsh, tcsh and csh keep the flags-only rule
  because their value-taking options would otherwise read as scripts. Within the
  POSIX family, options that take a value are consumed, so
  `bash --rcfile /etc/bashrc` and `zsh -o vi` stay interactive, and `-s` reads
  from stdin so `bash -s worker` does too.

  Confirmed against a live window in both directions. With a `bash <script>`
  helper in the foreground, `list_panes` reported the new pane's argv as exactly
  that script; a 40-cycle split loop reproduced a vanishing pane twice before
  the fix and ran clean after it.

- **A split that fails to start now says so.** Both split actions logged a
  spawn failure at `warn` and carried on. At the default log level that is
  invisible, and since a failed split leaves the layout untouched, all the user
  sees is a keystroke that did nothing. That is also what a pane which spawned
  and immediately died looks like, so the two arrive as the same report. A
  failed split now logs at `error` and raises one desktop notice, matching what
  a failed preference write already did.

## [4.0.0] — 2026-08-24

### Changed

- **Windows distribution support ends with 3.3.0.** Version 3.3.0 is the final
  supported Windows release and keeps its x86_64 archive and installer so the
  end-of-life notice reaches existing clients. Version 4.0.0 removes the
  Windows package, installer, performance harnesses, and signed update target.
  The Windows CI job remains as compile and regression coverage for retained
  conditional code, not as a supported-platform claim.
- **The native Ubuntu ARM test machine now runs under direct QEMU/HVF.** The
  migrated Ubuntu 26.04 aarch64 disk keeps the original OS, user, tools, and
  repository state, with its root filesystem expanded from 128 GiB to 256 GiB.
  Both the Xvfb and real GNOME Wayland `search-history` live-window smokes
  passed on native ARM. Each used Vulkan through Mesa llvmpipe, reported as a
  CPU adapter, so the result proves software rendering and does not claim
  accelerated graphics.

- **Picker matches now render as vertical lists.** The command palette, layout
  picker, and SSH launcher share the scrollable menu panel, keep the selected
  result visible, and no longer flatten matches into a clipped bottom strip.

## Older releases

- [3.x](docs/changelog/CHANGELOG-3.x.md)
- [2.x](docs/changelog/CHANGELOG-2.x.md)
- [1.x](docs/changelog/CHANGELOG-1.x.md)
- [0.x](docs/changelog/CHANGELOG-0.x.md)
