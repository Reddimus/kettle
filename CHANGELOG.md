# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Added

- Each row of the theme picker shows a small strip of that theme's colours:
  its background, its text colour and six accent colours. In a narrow window
  the strip is dropped before a theme's name is shortened.

- Kettle draws box drawing, block elements, Powerline separators, the
  triangles ◢◣◤◥ and braille itself, to fit each cell, as Ghostty, WezTerm
  and kitty do. Lines are whole pixels sized from the font's underline, so
  boxes, tables and tree views join across cells and rows without soft edges
  or brighter dots at the joins, at any scale. Rounded corners, diagonals
  and Powerline curves are smooth, shades are even tones instead of the
  font's dither, and Powerline separators fill the row with no seam against
  the next segment. `builtin-box-drawing = false`, or the new Text /
  Rendering switch in Settings, keeps the font's glyphs.

- Hebrew, Arabic and other right-to-left text reads right to left. Each
  normal-screen line is shown in Unicode bidi display order, as in VTE and
  Konsole: brackets inside right-to-left text are mirrored and Arabic letters
  join. Selection, links and mouse clicks follow the text as drawn, while
  copies, search and the program see it in its original order. Full-screen
  programs keep logical order, and `bidi = false` turns it off.

- Screen readers announce new command output (politely, the last lines of each
  batch, or a count for a flood), never the command just typed, and stay quiet
  for full-screen programs. Checked with Orca; macOS uses the system's
  announcement notification.

- Screen readers find each pane's place in its scrollback as a vertical scroll
  bar, and can move it a line at a time or straight to a line.

- A zoomed pane keeps its titlebar and shows how many panes the zoom hides and
  the key that restores them, in English and Spanish.

- Config error notices explain ignored settings, suggest nearby setting names,
  and show expected types or ranges without exposing values. Open Config opens
  the affected file, with keyboard and screen-reader access. Clipboard notices
  no longer show an empty button.

- GNOME/Wayland windows now show their title in a Kettle header, even with one
  tab, with minimize/maximize/close buttons, tooltips and screen-reader actions.
  Drag empty header space to move, double-click to maximize, right-click for
  the window menu, or drag an edge to resize. KDE server frames and X11 frames
  stay native; `linux-titlebar-style = native` restores the previous header.

- Quieter tab strips follow the terminal background, with readable separators
  and visibly dimmed inactive windows. Larger, separated New Tab and options
  buttons show translated tooltips and shortcut hints, stay reachable in narrow
  windows, and no longer clip tab shortcuts with narrow terminal cells.

- Discover profiles, save and open named layouts, and launch SSH from the new-tab
  menu, macOS Shell menu and command palette. Profiles open separate windows;
  empty lists explain how to create a profile or save a layout.

- Risky paste confirmations now explain line and control counts, preview controls
  visibly, and offer Paste Without Controls while keeping line breaks. Cancel
  remains the default, with keyboard and screen-reader access to every choice.
  Program clipboard copies show a per-pane notice (disable with
  `osc52-notify = false`); blocked OSC 52 reads and writes explain the policy
  once per pane. The README now documents paste choices and accessibility.

- Links now highlight under the pointer and show their destination and opening
  gesture in a status pill. Quick-select hints include on-screen help, shorter
  labels that use nearby blank space, and entries in the Edit and terminal
  menus. Progress counters such as `158/158` no longer look like file links.

- Session restore now brings back recent terminal output with its colors and
  text styling, followed by a dated divider and a fresh shell prompt. Sessions
  settings control how much output survives; private storage is bounded to
  8 MiB, and remote output requires opting in.

- Follow the system light/dark appearance by default with Kettle Day and
  TokyoNight Night, including live Linux portal changes. Existing fixed-theme
  configs keep their look. Settings adds an Appearance choice; theme picks
  customize the active side while following the system. macOS material
  matches the active palette.

- Kettle Day, the default light theme: TokyoNight Day with darker text. Body
  text reaches 7:1 contrast with the background (TokyoNight Day's is 4.5:1),
  and the red, green, yellow, blue, magenta, cyan and white that prompts use
  reach at least 4.5:1 (some were 3:1), as does the accent drawn from its
  blue. Each colour keeps its hue; the background, selection background and
  the two background-tone slots (0 and 8) are TokyoNight Day's. Settings'
  Theme row lists it. `TokyoNight Day` itself keeps upstream's colours, so a
  config or picker choice that names it is unchanged.

- A quick terminal: `kettle quick-terminal`, a palette/menu/keybind action, or
  `quick-terminal-hotkey` (registered natively on macOS and X11) slides a
  dedicated window in from a screen edge. Its tabs and splits persist while
  hidden; it returns focus when toggled away, hides when another app is
  focused, respects Reduce Motion, and has its own Settings group.
- Failed commands keep their pane and final output with an exit-code banner.
  Restart with Enter or its button; close with Escape, the Close button, or
  the close-pane shortcut. `exit-behavior` also supports closing or holding
  every exit, and restart keeps the original command, directory and
  environment.
- Config typos and invalid values show a dismissible in-window notice with
  the file, line and key at startup and reload, while Kettle keeps running.
- Screen readers can select and close tabs and activate New tab and the New
  tab menu in horizontal and vertical tab strips.
- Bash, Zsh and Fish panes now load shell integration on first launch, without
  editing startup files. Set `shell-integration = off` to disable it.
- Completed commands leave success or failure ticks by the scrollbar, with
  their exit codes available to screen readers.
- `paste-protection` (`auto` by default) asks before a paste that contains
  a newline or control characters when the program has not turned on
  bracketed paste, with Paste, Paste as one line and Cancel; `always`
  confirms every multi-line paste and `off` never asks. In bracketed mode,
  escape bytes in pasted text are neutralized so the paste cannot end it
  early.
- SGR 53 overline (and SGR 55 to clear it) is drawn like underline at the
  top of the cell.
- On macOS, a complete menu bar: Kettle (with Settings…), Shell, Edit, View
  and Window menus list every common action with the shortcut it is bound to
  in your config, in English and Spanish, and macOS lists Kettle's windows in
  the Window menu. Shortcuts chosen from the keyboard still go through
  Kettle's own key handling, so a program that owns the keyboard keeps its
  chords.

### Changed

- Tabs show their go-to-tab shortcut on every platform, taken from the key
  bindings: Alt+1 to Alt+9 on Linux and Windows, ⌘1 to ⌘9 on macOS, or whatever
  key you bound instead. Linux tabs showed none before, and macOS showed a fixed
  Cmd+N.
- The terminal's right-click menu offers Find… and Clear Scrollback, next to
  Copy, Paste and Select All. Before, they were only in the palette and the key
  bindings.
- Tab strips and menus are easier to read. Inactive tab titles and resting
  tab close buttons in a top or bottom tab bar, and menu shortcut hints, reach
  7:1 contrast where the theme's own text does (they stopped at 4.5:1); tab
  text still stays at least 3:1 in an unfocused window. A disabled menu row's
  hint stays dim. The tab close × (✕ in a side tab bar, still inside its
  chip), the new-tab +, the options ▾, GNOME's header ≡ and its window buttons
  are drawn larger, inside the same click targets.

- Text is larger by default on Linux, the BSDs and Windows: `font-size` is
  14 px there (was 13), close to GNOME Terminal's 11 pt and Windows
  Terminal's 12 pt. macOS keeps 13 px. A fresh window grows with the font to
  keep about 164x44 cells on a 1080p monitor. A maximized window on a 1366 px
  wide laptop fits 159 columns, past the 144 at which Claude Code's diff
  panel opens; a fresh window there stays within 90 % of the screen and fits
  143. A 1280-logical-pixel display gets 149 columns maximized. Reset font size
  returns to the platform default, or to the configured `font-size`. Settings
  and `--check-config` label font sizes in logical px.

- Inactive-pane text reserves contrast for antialiased glyph coverage, so
  bundled-font stems remain readable instead of falling just below 4.5:1.

- The terminal is inset 12 px from the window edge by default (was 8).
  `window-padding-x` and `window-padding-y` still set it. A fresh window
  grows by the extra padding to keep its grid where the monitor has room:
  about 164 columns on a 1080p monitor and, at macOS's 13 px font, 154 on a
  1366 px wide laptop.

- Detected URLs and paths are underlined only while hovered, as in most
  terminals, so a screen of links no longer looks busy, competes with a
  selection or hides which link is under the pointer. The new `link-underline`
  key's `always` value brings back the thin, dim resting line. OSC 8 links
  keep their dotted resting underline; hover, the destination pill,
  Cmd/Ctrl-click and quick-select hints are unchanged.

- Background opacity now works on GNOME, X11 and other Linux sessions that
  cannot blur windows. With window blur on, Kettle used to raise the opacity to
  99% there, so the Settings slider seemed to do nothing; the window now keeps
  the configured opacity, as it does with blur off. The Linux default stays
  99%. Wherever the session cannot blur (those Linux sessions, KDE Plasma on
  X11 included, and Windows without its system backdrop, such as Windows 10),
  Settings says "Not available in this session; opacity still applies." under
  the Window blur switch, in English and Spanish, and the footer of the
  Window blur and Background opacity rows drops its hint about what blur
  needs.

- In Kettle's GNOME header bar the new-tab options button is drawn and named as
  the window's primary menu (≡, "Main menu"), as GNOME apps place it.

- An unfocused window's selection keeps a 2:1 contrast with the background
  (was 1.5:1), so it stays visible while still quieter than the focused one.

- A theme selection colour too faint for 3:1 against the background is now
  strengthened as a deeper or lighter tint of its own hue, not greyed toward
  white or black: the default TokyoNight Night selection is a clear blue
  (`#4d64a6`) instead of slate (`#5d6781`). A selection across a status line or
  prompt keeps that one fill over every program-set background it stands out
  from. A cell whose background it does not stand out from, such as one that
  matches it, gets a fill of its own, so the selection no longer vanishes
  there. The ratios are measured against the opaque background colour: in a
  translucent window, the macOS and Windows default, the desktop behind the
  window changes the contrast you see. Link underlines inside a selection or
  search match, hovered or not, are drawn at their 1 px resting weight in the
  colour of the text above them, not link blue or the hover accent.

- The paste confirmation puts its counts on their own line, keeps each count
  with its noun when wrapping, lists Paste without controls first after Cancel
  when the text holds controls, and shows a raw Paste in the warning colour.

- Settings switches show a knob (right when on, left when off) with a label
  chosen for contrast against the accent. Delete or Backspace resets the
  highlighted setting to its default, and the footer says so.

- The focused pane's title is bold as well as highlighted, so focus does not
  rest on colour alone.

- On Linux, `restore-session = auto` (the default) now reopens the previous
  session, as Ptyxis, GNOME's terminal, does. Set `restore-session = false` to
  open fresh windows as before.

- Large output is processed faster: plain ASCII skips the grapheme tables, and
  the session-log stripper keeps only its state while no log is open.

- Quick-select labels that do not fit in the blank space before a match go in
  the blank space after it instead of covering the match's first letters.

- The terminal's right-click menu shows Copy greyed out until something is
  selected, puts a divider between a link's actions and the clipboard's, and
  right-aligns submenu arrows.

- Selected and search-highlighted text, links and decorations stay readable;
  inactive windows use a subdued selection and a steady hollow cursor. Settings
  can hide or retain the unfocused cursor shape, and cursor shape/blink edits
  now apply to running panes.
- `copy-trim-whitespace` (on by default) drops trailing whitespace at each
  copied line's end, for blocks as well as stream selections; `false` copies
  the selection exactly as displayed, including block widths.
  README and Settings now explain Option/Alt block selection and Shift-click
  extension.

- `restore-session` now defaults to `auto`: on macOS it follows "Close windows
  when quitting an application" and restores when unchecked. Windows
  still opens fresh; Linux restores the previous session. `true`, `false` and
  legacy boolean spellings remain valid, and Settings offers all three choices.

- Automatic selection and search text colours meet 4.5:1 contrast against
  their highlight, including dim text and underlined links, while preserving
  readable cell colours and explicit highlight foregrounds.
- Horizontal tab strips draw larger + and ▾ controls and use at least 4.5:1
  contrast for control glyphs, shortcut hints and inactive titles. A single tab hides
  its shortcut hint and close button until hovered, including in the macOS
  titlebar; both controls return with multiple tabs.
- Joined emoji, skin tones, flags and VS16 emoji now share grapheme cells by
  default. Set `grapheme-clustering = false` in config or turn off Grapheme
  clustering in Settings > Text / Rendering for the previous scalar layout
  in new terminals. Applications can still toggle mode 2027; terminal resets
  restore each terminal's configured startup default.
- On macOS, top tabs share the titlebar by default, with a centred title for a
  single tab, native window dragging, and the system double-click action on
  empty strip space. `macos-titlebar-style = native` restores the separate
  caption; bottom, side and hidden tabs and borderless windows keep their
  previous chrome. Tab controls have at least 20 pt hit targets and 4.5:1
  glyph contrast.
- The tab strip appears only with more than one tab by default. Set
  `tab-bar = always` to keep it in a single-tab window.
- Search shows the current match and total (3/122) next to the query, has a
  Regex toggle beside Case (Case cycles on click), and draws other matches
  with stronger contrast.
- Selection and search highlights keep at least 3:1 contrast against the
  background on every theme.
- A pane scrolled into history shows a ↓ N lines button that returns to the
  live bottom, and the scrollbar's hover target is wider.
- `scrollback-bytes` defaults to 64 MB, so the default 10,000-line history
  fits at common window widths.
- The context menu puts the actions for what you clicked first (Copy, Paste,
  Select All, and Open or Copy Link on a link) and groups pane, tab, title
  and appearance commands into submenus, so it no longer scrolls. On macOS,
  menus and the command palette write shortcuts as ⌘V, ⇧⌘D and ⌃⇧A.
- With grapheme clusters on (mode 2027), emoji ZWJ sequences and emoji with
  skin-tone modifiers draw as one glyph across the cluster's width, and emoji
  presentation fills the cell height like other emoji.
- The tab strip draws the active tab as a raised surface in the terminal's
  background with an accent edge, inactive tabs as muted labels without a
  fill, close buttons only on the active and hovered tabs, activity as a dot
  before the title, and on macOS each tab's `Cmd+N` shortcut when it fits.
  The default `tab-format` is now `{title}`; set `tab-format = {n}: {title}`
  for the previous numbered labels.
- Settings opens as a centred two-pane window: categories in a sidebar,
  a search field that finds settings by name, description or config key,
  a one-line description under every setting, and values drawn as controls
  (On/Off pills, ‹ choice ›). Narrow windows fall back to a single pane.
- The command palette and the theme, layout and SSH pickers open as one
  centred card near the top of the window, with the query field above its
  results, and command labels highlight the characters that match the query.
- On macOS, `Cmd+D` splits beside and `Shift+Cmd+D` splits below, as in
  iTerm2, Ghostty and Warp. `Shift+Cmd+D` no longer closes a pane; panes
  close with `Ctrl+Shift+W`, and `keybind = cmd+shift+d=close_pane` restores
  the old chord.
- Reopening Search selects the remembered query, so typing replaces it
  instead of appending to it.
- A tab with a single pane no longer draws a frame around it; pane borders
  appear once a tab is split, and while broadcast input is on.

### Security

- A paste carrying a bracketed-paste end marker (`ESC [201~`) now asks first
  even where bracketed paste is on, as such text was made to escape it. Kettle
  still removes the marker whichever choice is made.
- Update pulldown-cmark to 0.13.6, fixing quadratic parsing of nested image
  wikilinks (RUSTSEC-2026-0433, #763), and async-executor to 1.14.1, replacing
  a yanked release.
- Keep an atomically replaced file's group, clear macOS extended ACLs when
  repairing an owned private directory, and reject parent-directory
  components before repairing modes.
- Resolve trusted symlinks and Windows reparse points one component at a
  time, like realpath(3), following a link only when the directory holding
  it is trusted (at most 40 links and 4096 components). Relative dotfile
  links such as `../../dotfiles/kettle/config` keep loading.
- Leave a visible export in place when its creation check fails instead of
  unlinking a name in an untrusted parent, and charge queued directory
  entries against the shared removal budget as they are listed.

### Fixed

- A screen of characters that no installed font covers no longer freezes the
  window for seconds, and terminal queries such as cursor-position reports
  are answered promptly while it draws. Finding no font for such a character
  used to shape it with every installed font, and again each time a row
  holding it was redrawn. Kettle now skips fonts whose character map cannot
  supply it and remembers characters no font has; the font chosen for every
  other character is unchanged.

- Screen readers read terminal text word by word. Orca's word echo repeated
  the whole row, prompt included, after every typed word, because Kettle's
  accessible text marked no word boundaries; terminal rows and text fields now
  do.
- Combining marks that their font does not position sit on their letter. In
  `Z̤̈`, which macOS draws in San Francisco because the bundled JetBrains Mono
  lacks the diaeresis below, the upper diaeresis no longer overlaps the
  capital and neither mark hangs off its right edge. Such marks stack above
  or below the letter by combining class, as HarfBuzz places marks for fonts
  without anchors; a stack too tall for the line stops a quarter line past
  its row, and a block cursor on the letter inverts the marks where they now
  sit. Marks the font places itself draw exactly as before.

- Screen readers echo typed words in the terminal again. A typed space used to
  reach them only with the next character, so Orca read fragments such as
  " e" aloud instead of the word. The cursor's row now keeps the blanks
  before the cursor, so each typed character is its own text insertion.

- A config notice no longer takes Enter and Escape from the terminal under
  it: a command typed while the notice is up runs, and Escape reaches the
  program in the pane. The notice answers those keys once a screen reader has
  moved focus to it; its buttons, the mouse and Edit Config work as before.

- On GNOME Wayland a window keeps repainting after it enters or leaves
  fullscreen. Mesa's vsync path could hold a frame with no word from the
  compositor, so the window (and its size chip) froze until something else
  redrew the screen. Wayland windows now present in mailbox mode, which the
  compositor still shows without tearing, once per refresh.

- A failed process's exit banner sits along the bottom of its pane instead of
  covering the first lines, where a short session's output and its error are.

- Emoji draw in colour: those written with U+FE0F, such as ❤️, ☺️ and keycaps,
  and on Linux default-emoji characters such as 😀 and ⌚, which a monochrome
  symbol font could claim first.

- Copy and paste of text work in Wayland sessions without XWayland, including
  GNOME, OSC 52 copies included. Middle-click PRIMARY and image or file paste
  still use XWayland where it is available.

- With grapheme clustering on, Indic spacing marks and conjuncts and Thai/Lao
  spacing vowels now reserve two cells, VS15 text variants of default emoji
  narrow to one, and standalone skin-tone modifiers occupy their own two
  cells. Cursor movement, wrapping and insertion follow the corrected widths.

- Programming ligatures and URLs draw every character again in grapheme
  mode: `//`, `->`, `!=` and `===` no longer lose their left half.
- A new window whose shell clears the screen at startup no longer gains a
  blank line above the prompt when the window grows.
- Arch and Homebrew packages ship the renderer license texts from the
  release archive under `share/doc/kettle`, and the manual page describes
  the macOS self-update path and its Homebrew refusal.
- The test-support source scanner no longer lets a test-only enum variant
  hide the production variant after it, and scratch directories are removed
  only when their lifetime lock is free.
- Terminal images: encoded images are limited to PNG, JPEG and GIF before a
  decoder is built, Extended XMP segments and WebP payloads are bounded,
  replacing a PNG retires the old image state, and running out of graphics
  memory no longer drops ordinary control strings or reports the image as
  corrupt.
- Terminal state: a clipped URL query no longer turns into a local file link,
  widening keeps the right prompt row, clearing navigation marks keeps known
  shell state, `VDISCARD` is ordinary input on Linux, output backpressure no
  longer counts toward the end-of-file deadline, and recording retention
  scans are serialized across processes.
- Rendering: image culling uses the rendered extent, failed animation frames
  and decodes retry, atlas failures are reported once per frame, buffer
  growth checks the device limit, and titlebars, completion counts, compact
  scrubbers and settings categories keep stable, correctly measured widths.
- CLI and control: headless clipboard queries get their empty reply, a full
  semantic queue fails the run instead of losing replies, slow readers are no
  longer called stalled, stdin read failures exit non-zero, and the control
  endpoint checks directory ancestors, process ownership and registry bounds.
- Config, remote and update: `force-no-bell` no longer hides saved bell
  changes, keybind edits are batched, SSH reconnect is suppressed when
  `IdentitiesOnly` cannot be kept, and a macOS update verifies the bundle
  through the held staging directory and refuses to delete an unexpected
  displaced bundle.
- Media: memory-probe failures survive cleanup, playback cannot starve
  worker commands, delayed video tracks keep their start time, still-image
  rotation comes from the right stream, and structural limits apply before
  native measurement.
- Tabs and windows: vertical tabs stay clear of the status bar and search
  bar, a hidden vertical tab bar no longer takes terminal width, the docking
  marker follows the last visible tab, generated tab groups no longer collide
  across windows, and closing a scaled-zoom tab restores the window font.
- Input: Kitty keyboard mode gets release events for `send_keys`, keypad 5
  with Num Lock off and Windows AltGr; pending releases are dropped when
  reporting resets; and the mouse wheel runs focus cleanup when it switches
  tabs.
- Links and openers: a clicked link is checked against the pane it came from
  before opening, custom URL handlers are bounded while their children run,
  and the file manager's executable identity is verified.
- Sessions: session files are checked for trust before restoring commands,
  sessions that restore would reject are not autosaved, and repeated restore
  failures no longer turn off saving.
- Lua: finalizers stay under the instruction watchdog and sandboxed `io.read`
  can no longer block the UI.
- Shells: fish paths keep their backslashes, the real default shell is
  detected on Windows instead of assuming PowerShell, and WSL working
  directories apply to ordinary new tabs.
- Settings: failed edits are no longer reported as applied, and closing a
  window restores an uncommitted theme preview. The reconnect refusal notice
  is translated.
- Terminal grid: widening keeps the history origin, height resizes no
  longer scan all scrollback, VS16 widening is limited to emoji-capable
  sequences, and erasing one half of a wide glyph marks its counterpart drawn.
- Shell integration: command-end and prompt-start markers are emitted before
  the prompt renders, PowerShell on Unix reports a hostname with its working
  directory, and the symbolic Shift-Tab binding needs Fish 4 or newer.
- Build and tooling: `just install` no longer installs stale binaries,
  `just screenshot` keeps output paths with spaces intact, the pre-commit
  doc exemption no longer skips files that builds or tests embed, and
  release verification accepts reruns of an already-public release.
- Docs: corrected drift in the supported DECRQSS list, the newline-mode
  Enter table, OSC 52 ordering, the GPU diagnostics cache override, and
  parser versions.
- Linux self-update refuses to overwrite a file the install never recorded,
  instead of backing it up and deleting it after the update, and counts
  generated files toward the recovery limit so a completed update can always
  be rolled back.
- Agent launch treats a primary group shared with another account as not
  private, Windows control directories are checked and created with a
  private ACL, Windows lock files are retired while still locked, and the
  Linux media worker can change only its own scheduling priority.
- Paste, PRIMARY paste and OSC 52 clipboard reads are bounded in size and
  time before their contents are collected, so a misbehaving clipboard owner
  can no longer block the UI or grow memory without limit.
- Opening Settings no longer waits for GPU detection, media tips are claimed
  off the UI thread, a trigger never reruns for a match it already handled,
  sessions save at most the 16 windows restore accepts, and screen readers
  get the same character boundaries as the caret.
- Kitty relative-placement chains deeper than eight are refused when
  defined, command output captures are reported unavailable after a column
  reflow instead of copying the wrong rows, offscreen screenshots no longer
  leave a truncated file when they fail, GPU setup keeps trying other
  backends when one device fails to open, and macOS audio decoding moved off
  the sound-control thread.

## [5.9.0] — 2026-11-03

### Added

- Explain missing hook cards in MCP shelf receipts with fixed `card_reason`
  words and guidance, without exposing card text or unknown reason words
  (#760, #764).
- Document `agent-setup --launch-codex` and `--launch-claude` as stable
  wrapper interfaces, visible in help, preserving arguments, environment
  and the agent's exit status (#760).
- Add `agent-setup --print --form wrapper` for user startup files and
  `--form entry` for programs that start an agent themselves. The ccx entry
  recipe is verified on Ubuntu 26.04.1 with ccx 2.6.26 and Claude Code
  2.1.317, including a real inline SVG card. Entry scripts refuse to run when
  named `claude` or `codex` (#760).

### Changed

- Report a user's own `claude` or `codex` function in
  `agent-setup --status`, and avoid claiming Codex cards when that command
  runs first. One-shot zsh and fish startup hooks record `KETTLE_OWN_*`
  hints bound to the defining shell's PID for status only; aliases are not
  checked, fish abbreviations do not suppress Kettle's function, and inherited
  hints in child shells read as not checked. They never affect admission (#760).
- Extend Linux cardless-launch notices to Claude Code version, executable
  and display-server failures, and Codex native card-preparation failures.
  Codex's earlier version warning and some silent fallbacks remain (#760).

### Fixed

- Keep new Linux windows under their original taskbar identity after an update
  replaces the running executable, including renamed builds.
- Include current Settings values in macOS accessibility row names, so
  assistive tools can read the theme, font size, and other selected values.
- Match symbolic shortcuts such as imported Terminator `Ctrl+Plus` when
  Shift produces the symbol, while preserving explicit Shift bindings and
  the active layout's character.
- Report why a file's hook card is unavailable when its pickup is lost while
  Kettle waits for file metadata.
- Preserve existing aliases when sourcing generated bash and zsh agent
  functions or wrappers, instead of failing to parse the function declaration.
- Keep Fish command correlation enabled when the integration's source path
  contains `cx` or `ctrl-x`; source-location comments are not key bindings.
- Reconnect SSH sessions using the final `-F` configuration and preserve login
  names containing `@`; suppress cloning when source bindings or Match tags
  cannot be reproduced.
- Refuse to reconnect container sessions with relative client configuration
  paths, which would otherwise resolve against a different working directory.
- Recognize PowerShell's `-ec` encoded-command alias as a one-shot command,
  while keeping `-NoExit` sessions interactive.
- Give cards to interactive Claude Code launches on Linux using
  `--dangerously-skip-permissions`, `--add-dir`, `--mcp-config`, `--ide`,
  `--debug`, and the other supported interactive flags instead of silently
  falling back to the media shelf (#759, #761). `--strict-mcp-config` stays
  refused: it hides the plugin's MCP server, so `kettle_show` would not exist.
- Say when an unknown option, `--bare`, `--safe-mode` or `--strict-mcp-config`
  leaves an interactive Claude Code launch without Kettle cards. The stderr
  notice never includes option values or prompts; images and diagrams still
  reach the media shelf (#759, #761).

### Maintenance

- Exit real-Codex job-control probes with the client's `/quit` command instead
  of assuming two Ctrl+C presses terminate an idle session.
- Keep Lua pattern regressions reliable under CPU contention with a stalled-call
  watchdog and required completion records, while preserving their work-limit checks.
- Verify pane-drag geometry across split rounding, wait for the Linux fork
  fixture to start before testing timeout cleanup, and recognize macOS's
  closed-peer response during control-connection stress checks.
- Point failed client probes to their captured output without assuming an
  authentication problem, and align the vendor soft-reset notes with DECSTR.
- Preserve captured macOS clipboard items and formats during the raw-paste
  smoke, and require a disposable desktop for other clipboard-replacing
  cases. Reap a clipboard provider when writing its fixture fails, and verify
  pasted command output independently of shell echo.
- Verify both pasted video filenames even when long paths wrap across
  terminal rows.

## [5.8.2] — 2026-11-02

### Maintenance

- Restore the restrictive-umask test fixture before process exit so coverage
  profiles remain readable and line-coverage reports merge successfully.

## [5.8.1] — 2026-11-02

### Fixed

- Update the coordinated Mermaid and SVG renderer families, including inherited
  baselines in nested SVG text. Keep renderer version pairs checked together.
- Keep the notarized macOS archive within the existing updater's entry limit
  as bundled license notices grow, while preserving every notice and the seal.

## [5.8.0] — 2026-11-01

### Added

- Opt-in Unicode 18 extended-grapheme cells with DEC mode 2027, off by
  default. Joined emoji, regional-indicator pairs, combining sequences and
  Indic conjuncts retain one leading cell through rendering, copying, search
  and reflow. Late VS16 can widen a cluster; documented resource and glyph
  fallback limits still apply.
- DEC left/right margins, column editing, horizontal index, reverse
  wraparound, protected cells and selective erase, rectangular copy/fill/erase
  and attribute operations, mode save/restore, special colours and screen
  reverse. Status reports describe supported modes without inventing device
  capabilities; Kettle identity and window/title/checksum policies remain.
- Software keyboard lock (KAM) and send/receive mode (SRM). Accepted local
  echo has its own parser state and reaches the terminal before the matching
  PTY write; rejected input is never echoed. Recovery actions and protocol
  replies remain available while keyboard input is locked.
- Versioned command-begin/end correlation in bundled shell integrations.
  Control clients can require correlation before submitting input; older
  integrations retain their existing fallback behavior.

### Fixed

- Present update notices in a compact capsule with rounded Details and Close
  controls. Longer messages use a rounded card with room for translated text
  and large fonts; transparent corners leave terminal input available.
- Keep video moving through interior audio timestamp gaps by decoding audio
  and video independently from identity-verified input handles. Silence is
  generated lazily from audio timestamps within the existing aggregate
  resource bounds.


### Maintenance

- Require the six Linux live UI checks after each passed 300 repetitions
  across three fresh runner allocations. Retain case artifacts on failure and
  keep physical-GPU and desktop-focus validation separate.

## [5.7.1] — 2026-10-31

### Fixed

- Stop the cursor at the scroll margins for CUU, CUD, CPL and CNL whether or
  not origin mode is on, as xterm and DEC terminals do. Kettle 5.7.0 stopped
  it only under origin mode.
- Save and restore origin mode with the cursor on DECSC/DECRC, `CSI s`/`CSI u`
  and the alternate screen's mode 1049, keeping a restored origin-mode cursor
  inside the margins.
- Reset the margins and home the cursor on DECALN (`ESC # 8`).
- Ignore a scroll region (DECSTBM) that lies entirely below the screen, as
  xterm does, instead of storing an empty one.
- Honour linefeed/newline mode (LNM, mode 20): LF, VT and FF return to the
  first column, and Return sends CR LF while the mode is set.
- Save and restore the cursor with xterm's private mode 1048, which Kettle
  ignored.
- Keep `Esc cancel` visible in the key hints of the pickers and title editors
  in narrow windows and splits: hints that do not fit now drop their middle
  entries first instead of cutting off the way out.
- Announce the About panel to screen readers as "About Kettle" rather than
  as the right-click menu.
- Let Linux screen readers read each Settings value and the text, caret and
  selection of the title editor and picker fields, and let any screen reader
  move that caret or change that text: Orca announced only the setting's name
  and an empty "entry".

### Maintenance

- Wait for the private D-Bus test daemon to accept connections before starting
  Linux portal tests, avoiding a startup race under CI load.

## [5.7.0] — 2026-10-30

### Added

- Edit every modal text field like the search bar: titles, the command
  palette, SSH, the layout and theme pickers and Settings paths now have a
  caret, selection, Home/End, word movement and deletion, cut/copy/paste,
  mouse placement and drag selection, horizontal scrolling, and IME
  composition anywhere in the text.

### Changed

- Check workflows, standalone shell scripts, Python scripts, spelling and
  local documentation links on every pull request, and find unused dev and
  build dependencies as well as normal ones.
- Build release packages, and the tests that admit them, without restoring
  build caches written by ordinary CI runs.
- Run weekly advisory coverage, mutation-testing and AddressSanitizer jobs.
- Never let an older release become latest once a newer one is published,
  whatever order their release runs finish in.
- Make secondary text and icons readable in every bundled theme: shortcut
  hints, About and update details, empty states, theme labels, tab titles,
  tab and toolbar buttons, search labels and completion details now meet
  WCAG AA contrast (4.5:1 for text, 3:1 for icons) against what they are
  drawn on.
- Keep dimmed text in inactive panes readable, without making text a
  program drew faintly any clearer than in the active pane.
- Show scroll arrows on a menu that is too tall for its window.
- Spanish: "Editar grupo de paneles:", the shorter "Atajos" Settings tab
  where the full label cannot fit, and translated names for the built-in
  themes.
- Share one streaming control-sequence stripper between `kettle exec`
  (including MCP `kettle_run`) and session logs. Session logs now also drop
  8-bit C1 sequences and a stray string terminator, which exec already did.
  Exec now counts UTF-8 continuation bytes and escape-prefix bytes toward its
  64 KiB recovery bound, and a fresh ESC at the bound starts a new sequence
  instead of showing its payload as text.
- Update dependencies within their existing version requirements, including
  wgpu 30.0.1 and notify 8.2.1.
- Decode kitty and iTerm2 images in stages, cutting the peak memory of a
  maximum-size transmission from about 392 MiB to 168 MiB, and hold each
  terminal to a 192 MiB transient decode ceiling so one pane's images
  cannot make another pane's fail.
- Dismiss pickers, editors, hints and the Settings panel consistently when
  clicking outside them, without the click also acting on what is under it;
  confirmation dialogs stay open. Shorten long field labels with an ellipsis
  instead of clipping them.
- List "Paste primary selection" in the command palette only on Linux.

### Fixed

- Refuse a symlinked update backup directory during recovery instead of
  reading and cleaning up whatever it points to.
- Stop enumerating update and backup trees as soon as they exceed their
  entry limits, and reject a stray backup file before collecting the tree.
- Let Windows update cleanup finish when the payload is exactly at its size
  limit.
- Keep two macOS updaters started with different state directories from both
  replacing the app.
- Show Sixel images' HLS palette colors correctly, and refuse a Sixel image
  whose declared size cannot be reserved instead of drawing a smaller one.
- Keep trailing spaces in `text:` key bindings.
- Saving a theme or a finite scrollback in Settings now sticks: a
  `palette = NAME` or `scrollback-infinite` line no longer overrides it on the
  next load.
- Import Terminator punctuation shortcuts and `[keybindings]` full-screen
  accelerators as key bindings.
- Follow the sun correctly near the poles, and apply a forced light or dark
  theme even when the solar schedule lacks coordinates.
- Say how to enable shell integration when an agent's `run_command` times out
  without it.
- Keep titles, prompt marks, clipboard writes and notifications working in
  every pane while other panes hold all image memory.
- Stop a large image or control string that is refused for lack of memory from
  printing the rest of its payload as text.
- On macOS, remove update leftovers through descriptors held since their
  ownership check, so a tree renamed into their place in the shared
  Applications folder is never deleted.
- Repair three documentation links to testing sections that had moved.
- On Linux, stop descendants that left the session with `setsid()` when
  `kettle exec` times out or is cancelled, instead of leaving them running.
- Deliver end-of-file from `kettle exec` to a program reading a canonical
  line even when a terminal reply arrives between its input and the EOF.
- Report a stdout write failure during `kettle exec`'s final output grace as a
  delivery failure when the run times out or is cancelled, instead of losing
  it.
- Refuse `kettle exec` geometry over 4,096 columns or rows, or 262,144 cells,
  before allocating the terminal, and read Linux process names as bytes
  during teardown.
- Rebuild, and change the build's source id, when a vendored crate changes.
- Show an artifact from the legacy agy edit tools only when its sidecar
  metadata says it is user-facing.
- Launch `kettle-console` without waiting when grouped short options include
  one that takes a value, such as `-mTwork`.
- Report a config file that cannot be looked up as a validation error in
  `kettle check-config` instead of treating it as missing, and write the
  first-run config privately and atomically.
- Bound the SSH host table's name padding.
- In the MCP server, stop and report failure when writing to stdout fails,
  cancelling running tools and letting them stop their processes first,
  return a smaller-limit error instead of truncated control results, and
  accept integer arguments written as `5.0` or `5e0`.
- Check every parent directory of the media worker before trusting it, and
  on macOS keep refusing signature checks while any earlier check is stuck
  past its kill, not only the most recent one.
- Trust a sticky temporary directory for video frame export only when you or
  root own it, and keep staging names short whatever the output name.
- Notice a shell's exit even while it keeps output readable, and kill and
  reap shell probes that time out, with their output capped.
- Keep images anchored when a top-anchored scroll region moves them into
  history over several batches.
- Read legacy Escape-prefixed keys as one key, and keep Alt-Tab from
  consuming a completion request.
- Delete kitty images by relative z-index correctly, and allow replacing a
  placement whose chain passes through the placement it retires.
- Keep trailing dot components in file links, strip `(line)` and
  `(line,col)` suffixes, and recognize expanded Windows home directories.
- Revalidate a recording path after taking its lock.
- Bound the regex work in each forward-search step and keep its progress
  across steps.
- Recognize grouped short options containing `-f`, and `<` or `>` right after
  them, in the zsh startup guard.
- Keep `ESC \` ending a message after a repeated ESC, and report working
  directories verbatim on Unix.
- Grow kitty upload buffers geometrically within their bound and charge the
  memory they actually allocate.
- Hold the staging lock until a state file is published, so cleanup cannot
  remove a file mid-publication.
- Refuse a Kettle state directory component that is another user's directory
  or symlink, while following the user's own symlinks.
- Run container reconnects with `/bin/sh` inside the container, and offer no
  reconnect when it cannot reproduce the session (Docker TLS options,
  relative or drive-relative SSH identity and config paths, `-S` and
  `ControlPath`).
- Stop a crash when a pane closes after a font change and before the next
  frame.
- Apply PNG and APNG background limits before reading their metadata, and
  keep 32-bit Linux from overflowing on a 1080p wallpaper.
- Draw the cursor on its row's shaped baseline, draw no underline or
  strikeout on concealed cells, and keep glyph shaping offsets.
- Place the search caret, pointer and selection correctly inside ligatures.
- Size and place toasts and remote receipts from their measured text, and
  wrap long failure messages.
- Keep a confirmation dialog's focused button fully visible when no whole-line
  scroll shows it.
- Keep up to 30 combining marks per cell in snapshots, and the mask
  foreground on non-sRGB surfaces.
- Build the Linux release archive in a private temporary file with its size
  limit enforced, and never overwrite an existing archive.
- Keep files that survived an interrupted Unix install in its provenance.
- Reject tracked paths under a symlinked directory in the tracked-file audit.
- Fix races, quoting and cleanup in the smoke, live-UI, performance and
  Linux check scripts, including the standing self-test's intermittent
  keyblock failure.
- Keep multi-line command buffers whole in Fish completion, and emit no
  command-start mark for an incomplete PowerShell command.
- Keep the PowerShell integration working when a profile turns on strict
  mode.
- Depend on Arch's `wayland` package, which provides `libwayland-client`.
- Bring the security, agent, recording, protocol and architecture documents
  in line with the code, and buffer partial output lines per pane in the
  example `init.lua`.
- Give each screenshot its own destination and report a failed capture.
- Keep a zoomed pane's font when duplicating it.
- Run custom URL handlers and the system opener off the event loop.
- Suppress startup titles only on Windows, where ConPTY causes them.
- Hit-test the status bar where it is drawn.
- Return focus to a restarted pane, and keep its directory for WSL launches.
- Redraw after a broadcast that scrolls a pane.
- Tie IME composition to its pane, end it on focus changes, and accept new
  input after a cancelled composition.
- Start recording before restored sibling windows open.
- Keep the new-tab button and tab overflow arrows in step with focus changes.
- Open a second theme picker from the first one's baseline, and keep
  configured theme overrides in previews.
- Run command deadlines before GPU recovery, and wake for the earliest of the
  animation, background and cursor-blink timers.
- Give a screen reader that attaches late a current accessibility tree.
- Keep search on the focused pane after a remote new tab or a tear-off.
- Keep wide and combining characters in trigger matches.
- Hide a vertical tab's close button when it cannot fit, and hit-test tabs,
  tear-off anchors and docking against the tab band as drawn, including the
  status bar.
- Keep an update notice dismissed in this window when an older update state
  arrives later.
- Keep live config reload working when re-registering its watcher fails.
- Stop reporting mouse motion to the terminal while a menu or dialog owns the
  pointer.
- Resize splits by dragging a divider through the control API.
- Submit `run_command` with exactly one carriage return whatever line ending
  it has.
- Keep a short window's menu at least one full row tall; offer Ungroup when
  any pane in the tab is grouped; accept Space typed as a character in
  Settings, toasts and dialogs.
- Reap layouts opened from the layout picker.
- Deliver keys typed behind a queued paste to the panes they were typed for
  when the broadcast scope changes.
- Hide sibling preview lanes behind the theater and fullscreen viewers.
- Bound a tab handoff to one deadline however it is fragmented.
- Tell Lua plugins `pane_close` for every pane that ends: with its tab, its
  window, or after its shell exits.
- Keep a torn-off tab's font zoom in its new window.
- Keep a client's control response when it half-closes its connection after
  sending a request.
- Wait to write shared session state until a scan of other running builds has
  completed, and clean up their stale sockets in every temporary fallback.
- Keep a replacement control alias when a stale one is pruned at the same
  time, and prune aliases whose server registration is gone.
- Refuse a control socket directory that other users can write unless it is
  sticky, saying how to fix it, and remove only a socket you own when binding.
- Stop advertising a control server whose process has exited but not yet
  been reaped.
- Never enqueue an audio buffer twice or write one already queued, and keep
  playback from seeking past the last frame.
- Refuse GIF frames larger than the image's screen before decoding, keep
  fractional frame delays, and apply image orientation before fitting.
- Render SVG text kerning, filters, stylesheets and trailing whitespace as
  browsers do, within the memory budget.
- Keep a video's presentation clock honest while frames are pending, and end
  the audio-tail allowance when the sound ends.
- Bound the Linux audio relay's queue before admitting blocks.
- Keep the Linux media sandbox from reading or changing other processes'
  resource limits or aiming I/O signals at them.
- Keep runtime incident logs within their size cap after a write failure, and
  record exit incidents without raw error text.
- Send application-keypad sequences for Num Lock-off numpad navigation, use
  the layout's character for shifted keys, and keep wheel scrolling smooth
  in both directions.
- Name new SSH tabs from their destination, quote curly single quotes for
  PowerShell, and reopen closed WSL tabs in their directory.
- Repair read-only owned directories during cleanup through held
  descriptors.
- Keep every selected cell in split preview lanes.
- Keep the caret on character boundaries after edits, and collapse a
  selection with plain Left or Right.
- Refuse a session save larger than the limit instead of replacing the file.
- Keep Settings numbers within what the config accepts, and show pinned
  devices consistently.
- Clear a failed update's installing state and refresh update status when an
  install finishes; count artifact download errors as request errors.
- Dismiss a paste receipt and a pending preview independently.
- Stop a Lua plugin's runaway `string.find`, `match`, `gmatch` or `gsub` in
  the safe and restricted sandboxes, which could freeze the window; the
  instruction limit now covers pattern matching too.
- Refuse `io.tmpfile` in the safe and restricted sandboxes, as `io.open`
  already is.
- Give each URL handler callback its own instruction budget.
- Make `kettle-console` reject invalid options and show help or errors
  before returning, exactly as `kettle` does.
- Include config-file recording settings when deciding whether a launch can
  reuse a running window, so a launch never joins a window recording
  differently.
- On Unix, export video frames through the output directory checked at the
  start, so a directory swapped meanwhile cannot redirect the file.
- Reconnect a remote pane by running its client with literal arguments
  instead of a shell command line, so no shell parses host names or values.
- Refuse a JPEG image whose decode would exceed the graphics budget before
  decoding it, using a bounded header scan.
- Start kitty animation playback from the selected frame, and restart it
  when the same frame is selected again.
- Keep every cell of an image stretched across more Unicode placeholder
  cells than it has pixels.
- Fail the build, naming the file, when a bundled theme cannot be read,
  instead of leaving it out.
- Bound title templates to 4 KiB.
- Reset modes, character sets and the saved cursor on a soft terminal reset
  (DECSTR), and report terminfo capabilities with real escape bytes, so
  programs that probe for undercurl get it.
- Keep scrollback anchors when the window grows.
- Stop a clipboard owner that keeps sending refused data from holding a paste
  on X11.
- Remove a pasted image whose handoff fails after it was saved.
- Report an external viewer that cannot start instead of leaving its action
  waiting.
- Stop PDF verification promptly when it is cancelled or runs out of time.
- Fit wide and right-to-left file names on file cards, and keep text aligned
  after a wide character clipped at the left edge.
- Keep desktop notifications working after one fails on Linux.
- Return a cancelled Wayland tab drag to its own window.
- Check a WebP wallpaper's chunk sizes, frame dimensions and memory budget
  before decoding it.
- Try other GPUs of the same model during device-loss recovery instead of
  excluding every one of them.
- Draw IME composition text on its own opaque layer above terminal text and
  inline cards, keeping the cursor and composed characters visible.
- Keep the paste receipt clear of the update toast.
- Restore the whole clipboard after the modal-shortcut smoke test, or skip
  the test, instead of overwriting the user's clipboard.
- Make an interrupted Unix uninstall resumable through a private journal of
  verified files.
- Reject audio that arrives a full second late in the A/V sync check.
- Check a process's start time before `macos-compare` signals it, so a
  reused process ID is never signalled.
- Answer OSC 52 clipboard reads in order with the writes before them.
- Show a "Detecting shells…" row in the shell menu instead of waiting for
  detection, then run a held New Tab With Shell action when it finishes.
- Deliver focus reports that arrive while terminal input is backed up,
  once and in order.
- Let the control API drag panes the same way the pointer does.
- Remove only staging directories that an update recorded next to the macOS
  app, never other directories whose names match the pattern.
- Bound the control server's inbox, each connection's request queue and the
  work each UI frame takes from them, so a flood of requests or a busy client
  cannot stall the window.
- Release keys held in a terminal when its window loses focus, and never
  drop a key release owed to it when the paste queue is full.
- Read file-card metadata in the media worker instead of on the UI thread.
- Translate a pasted path for another WSL distribution only through a
  verified mount of the same file.
- Stop Wayland blur discovery from blocking the UI.
- Keep split layouts within the depth that saved sessions can load.
- Keep a concurrently saved session when a backup is recovered.
- Finish a file card whose metadata cannot be read instead of leaving it
  checking, and keep size and explanation in `kettle show` replies for files.
- Leave a saved layout unchanged when another process holds its lock at
  startup, instead of replacing it on the next save.
- Refresh links and search highlights when terminal output lands while a
  frame is being prepared, instead of keeping stale ones until the next
  output.
- Align the control-message buffers of the legacy tab handoff and bound the
  Windows access-control entry checks to each entry's size.
- Finish a `kettle exec` recording before returning from a timeout or
  cancellation, instead of reporting a healthy recording as incomplete and
  leaving the file locked.
- Keep a media render whose worker answered and exited cleanly when the
  waiting thread resumes late on a loaded machine, instead of reporting
  the worker as unavailable.

### Security

- Stop PTY children from inheriting open descriptors numbered above a
  lowered `RLIMIT_NOFILE` soft limit, and fail the spawn rather than leave
  any descriptor unchecked.
- On Linux, run each MCP `kettle_run` in its own supervising
  `kettle exec` process, so descendants that leave the terminal session are
  stopped with the command on timeout or cancellation, and one command's
  cancellation never touches another's. `kettle exec` now treats SIGTERM and
  SIGINT as a bounded cancellation (exit 130).

### Maintenance

- Make the recorder stop, media cancellation and session handoff tests
  independent of thread and ConPTY scheduling.

## [5.6.0] — 2026-10-28

### Added

- Open focused media previews' context menus with the mouse or keyboard,
  including in narrow splits.

### Changed

- Announce updates once per release and state in brief top-center toasts.
  Background windows wait; About retains the status after the toast disappears.

### Fixed

- Keep media-preview header expansion available in short and narrow panes.
- Route control-API mouse clicks to the centered media viewer and its menus.
- Deliver update-toast replies to the requesting window after focus changes.
- Keep default journey and treemap labels readable over colored fills.
- Reopen the installed macOS build from the Dock without closing old terminals
  or adding a window on each repeated reopen.
- Start Linux video promptly when its audio track begins later; keep video
  usable if audio probing stalls.
- End audio at its last decoded sample after a seek.
- Fix Linux accessibility cache signals when panes or tabs change.
- Keep Venn set labels dark on the light checkerboard canvas.
- Use a light Mermaid palette over the checkerboard canvas.
- Keep slow system URL, Help and config launchers off the UI thread.
- Wait for final shell argv before checking macOS process-scan limits in tests.
- Keep installer test fixtures private regardless of the invoking shell's umask.
- Restore the parent menu's selected row and scroll position after leaving a submenu.
- Fix dark-theme contrast in event-model headings and event-model/Cynefin cards.
- Apply custom window title templates at filesystem roots and when cwd is unknown.
- Keep Linux process scans updating other panes when one argv exceeds its cap.
- Preserve Bash single-dash long options when detecting shells for splits.
- Reject shared sticky directories when trusting their contents, while retaining
  safe private-file creation there.
- Handle clean worker exit while playback tests drain buffered frames.
- Suppress SSH Reconnect when a configuration tag cannot be reproduced, so
  reconnecting cannot silently select another host.
- Make offscreen demo meters proportional to their labels and show empty capacity.

### Maintenance

- Keep Linux archives within existing updaters' entry limit while retaining all
  license files; verify each Linux tarball before upload.
- Make native process-deadline tests independent of shell helper timing.
- Refresh reviewed Rust dependencies and pinned workflow actions.

- Upgrade the diagram renderer family together and retire the released lexer backport.
- Require the live stress test to reach its documented tab and pane counts.
- Check media stress results against rendered pixels or metadata-only file cards.
- Use current macOS clipboard bindings and validate their native pixel round trip.
- Fail font FIFO tests promptly when the loader exits or stalls before opening.
- Reuse source-identity builds when inputs are unchanged; retain worker/GUI
  identity checks across source and symlink changes.
- Bound audio CI package downloads and allow cold audio/nightly builds to finish.
- Replace the yanked Windows clipboard dependency in both audited lockfiles.
- Keep test-only conditional attributes and same-line documentation out of
  source guards, preventing tests from satisfying their own assertions.

## [5.5.1] — 2026-10-21

### Fixed

- Large videos no longer stop after a few frames when FFmpeg's automatic
  raw-video encoder threading exhausts the playback sandbox's address space.
  Output encoding now uses one thread while retaining the existing resource
  limits, playback controls, and audio behavior.

## [5.5.0] — 2026-10-20

### Added

- Codex launches supply relevant visual guidance through native prompt hooks.
  A turn-end fallback displays finished visuals from a private session inbox
  when the agent has not already shown them. The inbox is bounded and never
  scans the repository. New hooks require Codex's own trust review.
- Optional agy previews through an explicitly installed native plugin. Visual
  guidance encourages display calls; on macOS, explicit native visual artifact
  writes and edits also reach the media shelf. agy's existing Mermaid rendering
  remains available. Its native hooks do not support Kettle inline cards.

### Fixed

- Refresh the updater's TLS certificate verification and trust roots for
  RUSTSEC-2026-0381 and RUSTSEC-2026-0382, using the current HTTP client.
- Remove Codex's private visual inbox when its MCP server receives SIGTERM,
  SIGINT or SIGHUP, including native CLI exit before stdio closes.
- Let programs run inside the signed macOS app request Apple Events permission,
  restoring editor/PDF synchronization. Keep media helpers unentitled.
- Accept agy's native artifact write/edit hook payloads, including the explicit
  user-facing flag, so current CLI artifacts qualify for automatic previews.
- Recognize agy's foreground TUI after it redirects stdout and stderr to its
  log. Automatic previews still exclude print sessions and unknown launch modes.
- Keep RustSec warnings enforcement while scoping three informational D-Bus
  maintenance notices to exact reviewed versions, sources and reverse dependencies.
- Prevent parallel test processes from inheriting a crash-report pipe reader,
  which could make a closed-stderr test fail intermittently.

### Maintenance

- Refresh reviewed Rust dependencies and pinned workflow actions.

- Update the pinned font parser, compatible dependency patches and workflow
  actions. Keep the Mermaid renderer family on its audited versions and group
  future proposals for coordinated review.

## [5.4.0] — 2026-10-19

### Added

- Confirmations appear in a centered dialog with a clear heading, consequence
  text, and rounded buttons. Cancel keeps initial focus for close prompts.
  Buttons activate on release, short windows support scrolling, and Settings
  stays visible beneath the dimmed backdrop. Screen readers receive only the
  pending question and its controls.
- Video cards open paused within 90% of their split, with padding on every
  side and the picture fitted as large as possible without cropping. A central
  Play button and compact white controls on a dark strip keep playback within
  reach. A thin timeline in the theme accent spans the player; hover or drag
  reveals its seek handle. Narrow players put the time label on its own line.
  Click the picture or press K to play or pause, T for a larger Theater view,
  and F for fullscreen. Esc leaves fullscreen before closing; closing or
  pinning restores fullscreen entered by the viewer.

### Fixed

- Keep agent media cards enabled after routine client updates. Codex 0.162
  and newer in the 0.x line and Claude Code 2.1.280 and newer in the 2.x line
  use inline cards, subject to the existing launch and hook checks. A new
  major version needs a compatibility review before it is supported.

## [5.3.0] — 2026-10-17

### Added

- `kettle show --inline` prints a card under the command, for any program
  in a Kettle pane: an image, SVG, Mermaid or Markdown file, a PDF page, a
  video, a file Kettle does not draw, or bytes on stdin, which opens in the
  viewer when clicked. It needs the
  command to write straight to its pane's own terminal from the foreground;
  through a pipe, a file or tmux the item goes to the shelf alone and a note
  says why.
- HEIC and AVIF photos show, drawn in the confined media worker at the size
  the lane needs and turned as they are meant to be shown: by Apple's own
  decoder on macOS (AVIF before macOS 13 by ffmpeg 7.1 or later), and on
  Linux by ffmpeg 7.1 or later, a camera's tiled photo whole. With an older
  ffmpeg, or none, they show as file cards.
- PDFs show: `kettle show report.pdf`, an agent's `kettle_show` or a preview
  link puts the first page on the shelf, drawn on white at the size the lane
  needs, and the preview pages through the rest with `‹ ›`, PageUp and
  PageDown. `kettle show --page N`, the agent tool's `page` and the control
  API's `page_index` open another page; Home and End go to the first and
  last. The card's menu and the lane's `↗` open a private copy of the PDF in
  Preview on macOS, or Papers (else Evince) on Linux. The renderer is hayro,
  run only in the confined media worker, with a fix for a crafted file whose
  object streams nest, which overflowed its parser's stack.
- A file Kettle does not draw, such as a document, an archive, audio, or a
  video this system has no decoder for, shows as a file card instead of an
  error: its name and size, a page for a picture, and in the lane why
  nothing was drawn. Show in Finder (macOS) or Show in Folder (Linux) opens
  its folder with it selected. `kettle show` and the agent tools report it
  as `kind: "file"` with its size and the reason.
- TIFF images show like the other formats: the first image of the file, 8 or
  16 bits a channel. A floating-point TIFF is not drawn, since its values
  have no range Kettle could map to colors faithfully.
- `kettle ctl schema` prints the control protocol's JSON Schema (draft
  2020-12), with no running Kettle needed: each public method's params and
  result, the error codes and the event messages, so a tool driving
  `kettle ctl METHOD --json … --raw` can check what it sends and what it gets
  back. Kettle's tests keep the schema true to the code and to AGENT.md's
  method table.

### Fixed

- `list_panes` reports a Markdown file's diagram gallery as `markdown`, the
  word its `show` result and card use, and the viewer names it "Markdown
  diagram". Both said Mermaid.
- An agent's inline card shows once in its pane. Printing its text again, by
  replaying a saved transcript, used to paint the picture a second time.
- A file shown with `kettle show`, an agent's `kettle_show` or a preview link
  is classified by what it is even when its first letters spell an image
  format's short signature: a Markdown file starting `BMI results` or
  `P1 tasks` shows its diagrams instead of being refused as a broken image.
  A Graphviz DOT file is reported as a format Kettle does not draw instead of
  rendering as a garbled Mermaid flowchart.

## [5.2.0] — 2026-10-17

### Changed

- After an update, launching Kettle opens a window in the new version instead
  of the old process; windows already open keep theirs until they close
  (#575). While the old version still runs, the new one leaves the restorable
  session, pending remote commands and the old agent files to it. On macOS a
  Dock click still returns to the running app.
- Update notices are a small chip at the end of the tab bar instead of a
  full-width banner, so no pane is resized when one appears or goes (#576).
  It reads "Update v5.2.0", "Restart for v5.2.0" or "Update failed", and
  either click opens a menu with the facts, the release notes and Dismiss.
  `open_update` and `dismiss_update` work from the command palette while a
  notice is active, About lists what is true about updates, and a screen
  reader can reach the chip without it taking focus from the terminal.
  Opening the release notes no longer dismisses the notice.
- Drop the unmaintained `ttf-parser` (RUSTSEC-2026-0192). The vendored
  cosmic-text backports upstream's move to fontdb 0.24, which reads font
  tables itself, and `deny.toml` now bans the crate.

### Fixed

- Keep a pane open, and Kettle running, when a program prints binary data,
  floods bells or titles, or stops reading the replies to its queries.
- Stop a rectangular selection dragged past a split's right edge from
  aborting Kettle, and a runaway Lua callback from escaping its budget.
- Render Mermaid sequence diagrams with a bare `%%` comment instead of hanging
  the media worker until it is killed.
- On X11, paste text of 1 MiB or more that `xclip` put on the clipboard.
  Kettle froze for about 16 seconds and pasted nothing.
- Delete, erase and scroll only the cells and history a program asked for,
  clear both halves of a cut wide character, and copy wrapped wide characters
  from their own row.
- Move the cursor correctly in origin mode and inside scroll margins.
- Replace a reused Kitty image or named placement correctly, refuse cyclic
  placement updates, and share Kitty upload limits across both screens.
- Refuse OSC 8 link targets over 4 KiB cheaply, keep remote `user@host:/path`
  text from linking to a local file, and open a clicked link only inside the
  focused terminal.
- Keep reverse regex searches responsive on long lines, finish searches that
  need several passes, and keep F3 on its match after output moves it.
- Hold typing behind a screenshot paste until its path arrives, so Enter
  cannot run the command first; keep input-method commits with their pane.
- Type Windows AltGr and dead-key characters correctly, keep a Shift-drag
  selection after Shift is released, and count repeated clicks per pane.
- Retry missing glyphs once image memory frees up, keep glyph caches bounded
  in long sessions, and keep large scenes within the GPU's buffer limits.
- Draw the block cursor's character in its own face and height, keep
  confirmation buttons visible with `cell-width`, and scale transparent images
  without dark fringes.
- Finish an X11 tab tear-off when the window manager swallows the release,
  move Search with a docked tab, and drop a tab where it is shown.
- Restore later windows when the first one's command no longer exists, open
  restored windows at the size their budget counted, and save sessions off
  the UI thread.
- Keep every broken-session backup, even several in one second.
- Refuse negative window padding, save a rebind whole at the config size
  limit, and keep the current config when a reload cannot read its folder.
- Reload symlinked configs and profiles, and apply forced or scheduled themes
  at startup and on reload.
- Control API: `wait_for` matches text past the first page, `run_command`
  refuses while the shell is busy, and long output keeps its exit status.
- Time a Bash command line from its start, so long-command notifications
  fire and `run_command` reports the real duration. The Bash integration
  marked every simple command, which restarted the clock; it now marks each
  line once, and Kettle counts one command per line even for snippets an
  earlier version added to `~/.bashrc`.
- `kettle exec`: reap the process tree when its input writer cannot start,
  and keep control characters inside escapes with `--strip-ansi`.
- Keep agent integrations that another Kettle of the same version installed,
  and never wait for the install lock on the UI thread.
- Hold GIF frames and SVG, Mermaid and Markdown renders to their limits,
  play MP4 files with unusual display transforms, and refuse video durations
  that overflow.
- Bound decompressed Linux update archives, extension records included.
- Install the binaries just built when `CARGO_TARGET_DIR` is set, and suggest
  `kettle --write-default-config` on first run.
- Write crash logs even when stderr is closed, and report a startup stall
  when a hook blocks.
- Tabs in a side tab bar (`tab-position = left` or `right`) reorder by
  dragging, as they do in a top or bottom bar.
- A left or right tab bar with more tabs than fit the window's height now
  scrolls with `⌃ ⌄` buttons and the mouse wheel and keeps the active tab in
  view. The active tab used to be drawn below the window, with `+` on top of
  another tab.
- Editing a tab title with a left or right tab bar no longer draws the first
  tab's name through the editor; the tabs move down a row while it is open.
- In a window too narrow for one tab between the tab bar's scroll arrows, the
  active tab narrows to fit instead of covering the arrows and `+`.
- Update notifications: each release is announced, installed or reported as
  failed with at most one desktop notification across every Kettle process,
  and none of them requests the user's attention any more. A failed automatic
  install is reported, in your language, only after it failed on three
  checks; the raw error goes to the log.
- Update checks no longer stop when the system clock was ahead at the last
  check, or when `update-check.json` cannot be written.
- Dismissing an update notice in one window is no longer undone by a check
  running in another.
- A window left open across an update no longer announces the version that
  update installed, and no longer reports "update failed" for it on macOS
  (#574). Dismissing an update notice no longer stops `update-policy = auto`
  from installing that release.
- A Linux update never installs a release that is not newer than the installed
  version. A window left open across an update could otherwise download the
  release it was replaced by and install it again, or put an older one back.
- Keep remote and working-directory labels current on a busy machine. A
  process scan that runs out of time is now retried with a longer budget,
  instead of publishing nothing until the next poll.
- `kettle exec --timeout` on Linux: tearing down a session of several hundred
  processes no longer runs out of file descriptors and reports "teardown was
  not verified" (exit 125) instead of a timeout. The cleanup warning now names
  the error that stopped it.

### Security

- Refuse a dropped or pasted file name that a shell could run as a command or
  read as another file: control characters, non-Unicode bytes, or `%` and `!`
  for `cmd`. A notice says why.
- Refuse config lines that contain a NUL byte; `--check-config` reports them.
- Keep session files owner-only even when another tool loosened them, and
  create private files on macOS without inherited ACL grants.
- Refuse Linux agent executables and npm files that another user may write
  through an ACL.
- Keep export cleanup inside its private staging folder, so another writer
  cannot swap in a file for it to delete.

## [5.1.0] — 2026-10-15

### Added

- Tear off and rejoin live tabs on supported Wayland desktops, including
  lone-tab windows, with cancellation and destination focus.
- Drop local files from Wayland file managers through the shared paste path.
- Open image copies in Ubuntu's Loupe image viewer, with Eye of GNOME as a
  fallback.
- Move a live tab to another window from the command palette, including on
  Wayland; keep its running programs, splits and scrollback.
- Play Linux video sound through a PulseAudio-compatible service, with pause,
  per-video mute, synchronized frames, and a visible video-only fallback.
- Show inline agent cards on Linux for Codex 0.162 and Claude Code started
  from a Kettle pane.
- Follow the desktop's reduced-motion preference on Linux, with live portal
  updates and explicit `reduce-motion` overrides.
- Center manual media previews within their split without resizing the
  terminal. Pin the same preview beside the terminal, with keyboard navigation
  and focus return; `media-open-mode = lane` keeps the previous layout.

### Changed

- Paste multi-line text straight into a shell, without a confirmation.
  `clipboard-paste-protection` is retired and ignored.
- Open file links printed behind tmux, screen or zellij directly, as in a
  local pane. A path that is not on this computer gets a notice.
- Let Claude Code run `kettle_show` without a permission prompt, as Codex
  already does; it only shows you a file on this computer.
- Show macOS shortcuts with Cmd and Opt, as macOS keyboards name them, in
  menus, the command palette and Settings.
- Keep a pane's lines on screen when a split narrows it. Lines that wrap take
  the empty rows below the prompt instead of pushing the top into scrollback.
- Print `kettle ctl: listening for events` on stderr once `kettle ctl events`
  is subscribed, so a script can wait for it.

### Fixed

- Make the title editor, command palette, theme, layout and SSH pickers
  readable in every bundled theme: they now share the update banner's neutral
  fill, which the theme's text reads on at WCAG AA, with each bar's accent
  color on its edges. Renaming a pane in Dracula drew white on yellow at 1.08:1.
- Keep the saved workspace when a restored second window fails to open, and
  pass a Settings click to its confirmation dialog.
- Resize sibling windows when the update banner changes, and refresh what a
  screen reader reads after scrolling.
- Type the character an AltGr chord composes, and keep a closed tab's
  snapshot when reopening it fails.
- Keep WSL panes in the right directory when cloned or restored.
- Validate a saved session's size and encoding before overwriting it, and keep
  a dismissed update banner dismissed.
- Bound the update-check cache read, keep window padding from producing a
  negative terminal size, and keep display titles within 256 characters.
- Keep a control-API command's output once scrollback is full, write each
  pane's log to its own file, and reap trigger and URL-handler processes.
- Keep pinned previews open while searching, show recording indicators beside
  a custom window title, and translate the software-rendering title notice.
- Refuse a control-API screenshot of a pane in a background tab instead of
  capturing the whole window.
- Return to the tab you were viewing after confirming a pane's close, and stop
  `Ctrl`+wheel down from enlarging a size-5 font.
- Read a pane's command line with its empty arguments, so container options
  are not shifted, and reconnect a duplicated kubectl or Podman session with
  its credential selectors and storage root.
- Recognize an agent's composer only when it is the one program under a
  nested shell, not a forked helper beside it.
- Save state files whose names are long enough to overflow the temporary
  name.
- Play a video whose sound cannot be decoded without its sound, instead of
  failing, and keep playing a 30 fps video's sound after its last frame.
- Report that a video ended only after its worker exited cleanly.
- Bound SVG text and CSS selector work by what each element actually holds,
  and reuse video frame buffers between frames.
- Keep text after decomposed accents, common in macOS file names, in its own
  column; the block cursor now draws those accents too.
- Update a preview's play and pause control as soon as it changes, and show a
  wallpaper as soon as it finishes decoding.
- Draw terminal text again once images free GPU memory, instead of leaving it
  blank.
- Sample cropped kitty images at their pixel centers, and brighten bold text
  from the palette colors a program set at runtime.
- Keep wallpaper GIF frames and still images within the 64 MiB decode limit,
  and keep dark colors in a blurred wallpaper from turning black.
- Refuse a screenshot larger than the GPU texture limit with a clear message.
- Show the whole "Kettle" label in Codex's narrower card gutter, and blank
  cells instead of boxes for a card partly scrolled out of a full-screen
  program.
- Report each zsh command's real exit status to Kettle, so failed commands
  are marked and notified again; zsh's prompt hook always reported 0.
- Build the media worker in `just install-local`, `just install-recording` and
  the source-install steps, so a local build can show media.
- Keep the Homebrew-installed macOS app's code signature intact when the
  formula copies its documentation.
- Resize the terminal grid before telling the program, so a program that
  redraws at once is never parsed against the old size.
- Finish a pane's exit on time when a background process keeps writing to its
  terminal after the shell has gone.
- Play kitty animations in Unicode-placeholder and relative placements, and
  stop redrawing once a finite animation has played.
- Keep a pane's `scrollback-bytes` budget after it is widened.
- Keep `log-strip-ansi` logs free of the text inside a control sequence that
  follows a very long one, and recover from an unterminated sequence after
  64 KiB of multi-byte text as promised.
- Open the whole file a bare path names, such as `out/report#1.pdf`, rather
  than the part before `#`.
- Report the full match when a regex search spans a very long wrapped line.
- Save Settings changes even when a config has an older spelling of the same
  key further down, such as `copy-on-selection` or `padding-x`.
- Save settings into the part of a Terminator-format config Kettle reads, not
  into a trailing `[layouts]` or other ignored section.
- Refuse a settings save that would push the config past its 1 MiB limit,
  rather than writing a file Kettle then ignores.
- Give up on a config lock after half a second when another Kettle holds it,
  instead of freezing the window.
- Apply `keybind = ctrl+shift+1 = …` and its unbind to the `Ctrl+Shift+!`
  spelling winit reports for the same chord.
- List only profile names whose file opens under that name.
- Preserve Wayland window dimensions at startup and when tearing off tabs.
- Open pasted screenshots in Kettle without changing the pasted command line.
- Keep Linux desktop notifications visible after sending them on GNOME.
- Paste copied Nautilus files without adding a carriage return to the filename.
- Define Kettle's `codex` and `claude` functions in zsh on Debian, Ubuntu and
  Fedora, whose stock `zshenv` mentions `NO_RCS` in a comment.
- Keep `kettle_show` available in Claude Code, which skipped the tool because
  its schema used a top-level `oneOf`; inline cards and shelf delivery from
  Claude Code work again.
- Update Rustls to 0.23.46 for upstream TLS fixes (RUSTSEC-2026-0346 to
  RUSTSEC-2026-0349).
- Paste a large screenshot without freezing the window; pastes made while it
  is being saved land after it, in order.
- Load Lua modules with `require` in `safe` and `restricted` mode only from
  trusted absolute paths, never from the directory Kettle was started in.
- Keep an older Kettle that is still running from replacing a newer macOS app
  during an update.
- Report `kettle exec`'s stop code on macOS when the stop lands just after the
  command ends, instead of an internal error.
- Refuse `kettle_run` sizes over 512 columns or rows before starting it.
- Retry or uninstall a Linux install that a full disk interrupted, and keep
  `KETTLE_VERSION=latest` from installing a release older than signed
  manifests.
- Keep a trailing space in a directory name a shell reports with OSC 7.
- Open no second window for a bare launch that already opened its own after
  the running Kettle was slow to answer.

## [5.0.1] — 2026-10-13

### Fixed

- Keep light-themed macOS titles clear of the traffic lights while retaining
  native blur, including on macOS 27.
- Leave native fullscreen on the first toggle, including after using the
  macOS green window button, and restore the last windowed size and position.

- Keep paths containing wide characters whole during double-click selection.
- Avoid signaling an already reaped Unix PTY child during pane teardown.
- Keep Command shortcuts from selecting hints, changing vi selection or
  activating menu mnemonics; prevent control-plane typing into a field
  covered by a menu or vi mode.
- Use the actual UTC calendar day for solar theme scheduling, including leap years.
- Recognize working-directory, prompt and completion OSCs after repeated
  escapes or intervening controls; preserve ordinary recorded output and
  strip those escape prefixes correctly in session logs and `exec --strip-ansi`.
- Coordinate Linux installation and removal with the updater, and remove its
  idle lock during uninstall without allowing stale lock holders to proceed.
- Make complexity, deadline and private video-output fixtures independent of
  runner scheduling and umask while retaining the production bounds.
- Accept absolute cache directories in the macOS release update smoke.
- Create missing private data directories with owner-only modes, so agent
  plugins can install under a group-writable default umask.
- Stop video-session test heartbeats after the final frame, avoiding a write
  racing the worker's normal exit; read queued failures after closed input
  and check still timestamps against the decoder's bounded keyframe fallback.
- Copy a path in the native hint fixture, keeping browser activation from
  interfering with modal shortcut checks.
- Publish FFmpeg test scripts after their writers exit, avoiding Linux
  `Text file busy` errors during parallel tests.

## [5.0.0] — 2026-10-11

### Added

- Inline media cards for Claude Code: `kettle mcp --display`, launched by
  Kettle's Claude Code plugin, asks for a card under the `kettle_show` call,
  and Kettle registers one only for Claude Code under Anthropic's signature
  running in the pane the item lands in. Kettle builds the card's cells and
  caption; the plugin's hook collects them once and prints them under the
  call, where Kettle paints the image. The model is told only that the media
  shows below the call.
- `kettle agent-setup --print` prints a `codex` shell function to review and
  add to your shell's startup file. In a Kettle pane, with Codex CLI 0.159
  or a later 0.x, an interactive Codex session started through it (fresh,
  `codex resume` or `codex fork`) gets this Kettle's display server for that
  launch only, so `kettle_show` sends media to the pane's shelf; such a
  session runs without Codex's shared background server, so it does not
  appear in `codex agents`. Every other Codex command, and any other version,
  runs unchanged, and Kettle edits neither the shell's startup files nor
  Codex's configuration.
  `--status` reports whether the shell reaches its Kettle with agent previews
  on, what a launch would get, and Claude Code's plugin and policy state for
  the pane, and `--uninstall` prints how
  to remove the function. It is for bash, zsh and fish, on macOS and Linux.
- The Codex launch function approves `kettle_show` by its own name for the
  launch, never the whole server, so media shows under any approval policy:
  with `approval_policy = "never"`, Codex refused every call that would ask.
- Codex shows a card under the call for media it sends with `kettle_show`,
  like Claude Code. On macOS with Codex CLI 0.162, the launch function adds a
  `PostToolUse` hook that collects the card Kettle registers for the call, and
  Codex prints it under the call, where Kettle paints the image. Codex asks
  you once to trust the hook in its own hook review; until then, and with
  other Codex versions, media goes to the pane's shelf. A card's provenance
  tab names the harness that printed it.
- "Copy agent setup commands" in the palette (`copy_agent_setup`) puts the
  `codex` function for your shell on the clipboard, ready to paste into its
  startup file. In English and Spanish.
- A new pane on macOS or Linux whose `PATH` has no `kettle` gets the running
  Kettle's folder appended, after your own commands, so agents and setup
  commands there find it (`add-kettle-to-path`, Settings → Agents; `off`
  leaves the `PATH` as configured).
- Codex previews (`agent-display-codex`, Settings → Agents → Codex previews;
  off by default, needs agent previews): the zsh or fish a new pane starts
  defines `codex` itself, as the function `kettle agent-setup --print`
  prints, so there is nothing to paste. zsh reads a Kettle-owned, read-only
  `.zshenv` through a borrowed `ZDOTDIR` it puts back exactly; fish runs
  Kettle's code after your configuration (`fish -C`). Your startup files run
  as before, and a `codex` you define or can autoload still wins. `kettle
  agent-setup --status` says whether your saved settings turn it on.
- Claude Code previews (`agent-display-claude-code`, Settings → Agents →
  Claude Code previews; off by default, needs agent previews): Claude Code
  started in a new pane gets Kettle's own plugin, the display server and the
  hook that prints inline cards, through `CLAUDE_CODE_PLUGIN_DIRS`. Kettle
  writes the plugin read-only into its own data directory and checks it
  before every new pane. It changes none of Claude Code's settings, and new
  panes get nothing while Claude Code's managed policy forbids plugins from
  the environment, nor a plugin an outer Kettle passed down. Settings says
  why when a pane gets no plugin.
- `kettle ctl get_state` reports `caller`: whether the connecting process runs
  in one of this Kettle's panes, from the process tree rather than anything
  the caller says. A client's first request claims its own pid and start time;
  Kettle believes it only when the kernel agrees, then walks at most 64
  parents looking for a pane's own child process. Every pane now starts with
  `KETTLE_PANE_ID` and `KETTLE_PID`, set after your `env` entries; they are
  reported as hints and never count as proof.
- `kettle ctl` and `kettle mcp` now pick the Kettle they run inside, matched
  by process ancestry, before a newer one; then the one `KETTLE_PID` names;
  then the newest, as before. A Kettle chosen by `--pid` or ancestry is the
  only one tried. Before sending anything, a client checks that the server it
  reached is the process its registry entry names. A server also leaves a
  pointer to its entry where the OS keeps the registry for your account, so a
  client started with a stripped environment still finds it.

- Agent previews (`agent-display`, Settings → Agents → Agent previews, or
  `--agent-display on|off`) let agents show media in Kettle without reading
  the screen or typing. On its own it starts the control server for display
  requests only, which refuses every read with `display_only` and every
  mutation with `read_only`. Turning it on applies at once, also for agents
  already connected; turning it off applies when Kettle restarts.
  `--agent-server off` alone also turns previews off. `kettle ctl get_state`
  reports the policy in force as `policy: {server, display}`.

- Inline media-card rendering foundation: bounded marker capture and registered
  card recognition, owned fallback glyphs, clipped posters, caller labels and
  selection tint in Grid and Legacy text modes. Preview accounts are
  separate from terminal images, and unavailable preview capacity leaves text
  windows usable.

- Kitty graphics capability queries decode the supplied image and echo its
  image id in an immediate success or error reply. They preserve stored images
  and placements, support chunked direct transfers and quiet replies, and
  answer during synchronized output before a following device-attributes reply.

- Video paste receipts identify container content before asking the native
  poster provider. Text or still-image containers named as videos no longer
  show a video card; movies renamed to another supported video suffix still
  work. Header inspection reads at most 64 KiB on the background worker.
- `kettle show PATH` (or stdin, with no path or `-`; `--mermaid` renders
  either as a Mermaid diagram) and the `show` control method send an image,
  SVG or Mermaid diagram to the media shelf of the pane they run in, in the
  Kettle they run inside. They need only Agent previews, never full control.
  Media too large for one request is refused, never cut short, with the
  advice to pass a file path. Media lands
  in the caller's own pane by its process ancestry; a sender Kettle cannot
  place is labeled unverified with the program the system names for it (for
  `kettle show` and `kettle mcp`, the program that ran them) and, on macOS,
  who signed that program's code once macOS validates it.
  Each pane keeps up to eight items: a full shelf lets go of the one viewed
  least recently, never the one its lane shows; `list_panes` reports them.
  Showing the same file again replaces its item in place, and `--key` (an
  agent's `key`) names the item to replace. A push never opens anything on
  screen. Media previews run on macOS and Linux; elsewhere the worker says
  `unsupported_platform`.
- `kettle_show` takes inline Mermaid source (`mermaid`) as well as a file
  `path`, and the full MCP server (`kettle mcp`) offers it too. Its
  instructions follow where the server runs: inside a Kettle pane they say
  when to show media and that showing is not seeing, in tmux what happens to
  the pane, and elsewhere not to call it.
- `kettle mcp --display` offers agents one tool, `kettle_show`, which sends
  an image, SVG or Mermaid diagram file to the media shelf of the pane the agent runs in. It
  needs only Agent previews and cannot read the screen, type or run
  anything.
- Inline cards for the keyboard and screen readers: quick select labels each
  card in the focused pane and opens it when picked, and a screen reader
  hears each card as a button named for its item's title, kind, size and
  sender, which opens it in the pane's preview lane. In English and Spanish.
- Right-click an inline card for its menu: Open, and Open in Preview on
  macOS or Open in Image Viewer on Linux with Eye of GNOME. The viewer gets
  a PNG copy of exactly what Kettle shows, never the bytes a program sent,
  and never through the default app for the file type: Kettle checks it is
  Apple's own Preview, or a root-owned `/usr/bin/eog`, first. Copies are
  private to you, marked as downloaded on macOS, kept to the newest 32 and
  128 MiB (each for at least a minute, so its viewer can read it), and
  deleted when Kettle quits. The preview lane offers the same from a `↗`
  button in its header.
- The first inline card you see with its image shown says "Click to open"
  along its foot for ten seconds, or until you open a card, and then never
  again: Kettle records that it showed in `ui-tips.json` beside the config
  file, so of two Kettles running at once only one shows it. It waits until
  nothing covers the card.
- The pointer turns into a hand over an inline card a click would open, and
  the card gets an accent outline; one that just appeared gets them once it
  has stayed put half a second, even if the pointer does not move.
- Clicking an inline card opens its item in its pane's preview lane. The card takes
  both the press and its release, so the program behind it sees neither. A
  Shift-click selects text as it does anywhere else, and the wheel, like a
  press on a card that came on screen under the pointer less than half a
  second ago, goes where it would without the card.
- Preview lanes: `open_media_shelf` (palette: "Open media shelf";
  `Ctrl+Shift+Cmd+I` on macOS) opens a lane along the bottom of the focused
  pane, about 40% of it, on the item published last, with its title, kind,
  size and sender, and closes it again. The terminal shrinks to make room,
  so nothing is covered, and its program sees the new size; keys still go
  to it. The lane's header browses the shelf (`‹`/`›`), opens the item
  outside (`↗`), collapses the lane to a one-row strip (`▾`) or closes it
  (`×`); a lane with no room for its rows shows as that strip, along the
  pane's bottom whichever side it opened on, and a header too narrow for
  every control keeps close and collapse first. The terminal always keeps
  at least 20 columns and 5 rows; opening a lane in a pane too small even
  for the strip says so instead. Each pane has its own lane; lanes are
  never saved, and leave with their pane or a torn-off tab.
  `preview-lane-side = right` opens lanes beside the terminal instead. A
  screen reader hears each lane, its item's place on the shelf and each of
  its buttons. A pane titlebar marks unopened items as `▣N`, and the window
  title counts them.
- A preview lane can show the source an SVG or Mermaid diagram was rendered
  from (`≡`), scrolled with the wheel; put the picture on another
  background (`◐`), drawing a diagram again for it; and copy the picture, or
  the source exactly as it was read (`⧉`); and read an item's file again
  (`↻`), replacing it in place. Also as `preview_source`, `preview_canvas`,
  `preview_copy` and `preview_reload`. In English and Spanish.
- Zoom and pan a preview: `−` and `+` in the lane's header, a pinch, or
  Cmd+wheel (Ctrl+wheel off macOS) at the pointer zoom the picture, the
  wheel or a drag pans it, and `⤢` fits it again. Also as
  `preview_zoom_in`, `preview_zoom_out` and `preview_fit`.
- An agent's Mermaid diagram card shows the new picture when its preview
  lane draws it again on another background.
- Preview a diagram an agent printed: `/copy` it in Claude Code or Codex and
  use `render_clipboard_as_diagram`, or select the reply and use
  `render_selection_as_diagram` (also on right-click), and the lane renders
  it, with the agent's labels, notices and wrapping taken back off.
- Markdown diagram galleries: `kettle show notes.md`, an agent's
  `kettle_show`, a link or quick select opens a Markdown file's Mermaid
  diagrams (up to 32, each up to 64 KiB, in a file up to 1 MiB) as one
  gallery, as do several diagrams copied or selected together. The lane's
  detail line pages it with `‹` and `›`, as do Page Up and Page Down in a
  focused lane and `preview_previous_diagram` and `preview_next_diagram`;
  every page comes from the file as it was read, and `↻` reads it again at
  the diagram shown. The `show` control method's `markdown_index` opens a
  file's gallery at a page. In English and Spanish.
- `focus_preview` ("Focus preview") gives a preview lane the keyboard until
  Esc: arrows, `+`, `-`, `0` and `c` move, zoom, fit and copy, and nothing
  else typed reaches the program in the pane.
- Drag a preview lane's edge to resize it; the terminal keeps at least 20
  columns and 5 rows, and its program sees the new size as you drag.
- A zoomed preview sharpens a moment after it stops moving: the part in view
  is drawn again at the size shown, from its file only when you moved it
  yourself and not once the file changed.
- Preview a file without an agent: `preview_link` (palette: "Preview a file
  in Kettle") labels the image, SVG and Mermaid files in the focused pane,
  and typing a label renders that file in its pane's lane; right-clicking a
  link to one offers "Preview in Kettle". A link from another machine's pane is
  refused and one from behind tmux, screen or zellij is asked about first,
  as opening it is. Your preview goes ahead of anything agents are
  showing, and a notification says why when one cannot open. In English
  and Spanish.
- Preview a copied file: `preview_clipboard_path` (palette: "Preview the
  copied file in Kettle") opens the image, SVG or Mermaid file you copied
  in a file manager, or whose path or `file://` link you copied, in the
  focused pane's lane. Shift+right-click offers it as "Preview Copied File".
  `preview_next`, `preview_previous` and `close_preview` browse and close
  the focused pane's lane from the keyboard or the palette.
- Shift+right-click on selected text opens the menu instead of extending
  the selection; off the selection it still extends it. With nothing
  selected it opens the menu too, mouse reporting or not: a plain click
  used to leave an empty selection that Shift+right-click silently
  extended.
- The media worker can classify a file by its content, raster or SVG,
  whatever it is called, and says what it rendered. Kettle and its worker now
  speak media protocol 7; a worker from an older build is refused until
  Kettle restarts, as before.
- `kettle-media`, a new crate, defines the bounded media protocol for the
  agent visuals: jobs and results, caps, source authorization, the
  build handshake and the binary frames between Kettle and a media worker.
- `kettle ctl get_state` reports `media`: whether media previews are
  available, and a fixed reason when not. Kettle looks for its media worker
  only beside its own executable, never in `PATH` or the working directory,
  and checks the file and, on macOS, its code signature before trusting it.
  Missing workers read `worker_missing`; installed workers that pass their
  checks read `available`. Kettle and its workers
  now share one build identity, a hash of the source they were built from.
- `kettle-media-worker`, the media worker executable, is built with the
  workspace. Before reading anything it closes inherited descriptors, turns
  off core dumps and lowers its resource limits, and a watchdog ends it if
  its parent stalls. Unix packages now ship it beside the terminal. The media
  client can run a job in a fresh worker under startup and job deadlines and
  a 768 MiB limit on the memory the worker and everything it started hold,
  killing the worker's whole process group before reaping it.
- The media worker renders raster images through `kettle-media-render`, a new
  crate in safe code that writes nothing. It reads a file once through one
  open descriptor, refusing anything but a regular file and a file that
  changes while it is read; it takes the format from the content, not the
  name, and decodes PNG, JPEG, WebP, BMP and a GIF's first frame only after
  checking the image's size against the decoded caps. The image is scaled
  down to fit the requested box, never enlarged past its own size, keeping its
  aspect ratio, without transparent pixels bleeding color.
- The media worker renders Mermaid diagrams, every family merman 0.8.0
  pins, from flowcharts and sequences to gantt charts, mind maps and
  timelines: `kettle show diagram.mmd`, a small one on stdin, or an agent's
  `kettle_show` puts the rendered diagram on the shelf, in the pane's colors.
  Labels are measured and drawn with the same fonts, bundled Fira Sans for
  text, and the diagram's SVG goes through the same checks as any other SVG.
- The media worker renders SVG with resvg. The document is parsed with no
  DTD, scripts and foreign content are dropped, its style sheets (simple
  selectors, only from retained subtrees) are applied and written as
  attributes, and every reference
  outside the document (files, network, data URLs) is removed before resvg
  sees it;
  resvg's own resolvers load nothing either. A document whose references
  would expand past a million units, nest deeper than 256, or make resvg
  allocate more than four million pixels of layers, filter results, masks,
  clips or pattern tiles is refused before any of it is allocated. Filter
  input-name copies count toward expanded work, and every merge input and
  both inputs of blend, composite and displacement filters count toward the
  layer budget. SVG
  results are at most 1024 pixels a side and a million pixels, and text is
  drawn with the bundled JetBrains Mono face by default. A job can explicitly
  supply up to eight regular outline font files, 32 MiB each and 128 MiB total,
  preserving a selected TTC face index. Each job has its own font database;
  no host fonts are discovered, and embedded SVG, color and bitmap glyph
  formats are refused. Results report actual font use and missing scripts
  through `FontFallback`, `MissingGlyphs` and `uncovered_scripts`.
- Unix packaging builds `kettle-media-worker` separately with the unwinding
  `media-worker` profile. Linux installers and package-manager templates install
  both executables; updates journal the pair. The first restart after a Linux
  4.9-to-5.0 update installs the worker from packaged compatibility data through
  the same recovery journal. macOS updates exchange the whole paired bundle.
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
  language of Kettle's own text: Settings, menus, the macOS menu bar's
  application menu and the Dock menu, prompts, notifications and
  screen-reader names. The Linux desktop entry has Spanish too. The CLI's
  output stays English, byte for byte, whatever the language. `auto`, the default, follows the
  operating system's locale. A change applies when Kettle restarts. The
  Spanish is neutral Latin American, reviewed in context on the rendered
  Spanish build.
- A theme picker: type to filter the 500+ bundled themes by name, or move
  through them with the arrows, and the window previews the selected theme.
  Enter keeps it and Esc puts back the theme you started with. With nothing
  typed it lists the current theme's appearance first, each theme followed by
  the most similar one, and tags each row dark or light. Open it from the
  Settings Theme row (Enter or a click), right-click → Theme…, the command
  palette's "Choose theme…", or the bindable `open_theme_picker` action.
- `kettle video-frames clip.webm -o sheet.jpg` and the full MCP server's
  `kettle_video_frames` return a contact sheet of a video's frames for a
  model to read, each labeled with its time, with an index of the times.
  Kettle reads GIF, APNG and animated WebP itself, and on macOS MP4 and
  QuickTime through AVFoundation; any other video needs your own ffmpeg and
  ffprobe, which Kettle finds only in fixed places, trusts only when no one
  else could have changed them, and runs contained, with a deadline. Kettle
  never installs them. The display server never offers the tool. `--start`
  and `--end` narrow the window, `--at` takes one frame, `--count` takes 1
  to 16 (9 by default), and `--max-edge` sets the sheet's longer edge, up to
  2560 pixels (1568 by default).
- Agents and `kettle show` can show a video: it lands as its poster, the
  middle frame, with a play glyph and its length on its card, and the lane,
  `kettle show` and `kettle_show`'s result say its size, codec, length and
  sound. It plays in its lane when you play it, and the model has still not
  seen it. Quick select and the menus offer common video files for
  a preview.
  Without a decoder for it, the notice says to install ffmpeg.
- The media worker confines itself before it reads a job: to the job's own
  files, a trusted video decoder's own programs, and nothing to write or
  connect to (Landlock and seccomp on Linux, a Seatbelt profile on macOS).
  Kettle decodes video only inside it; on a Linux kernel without Landlock,
  videos are not shown or read while images and diagrams still are.
- A video card's menu, and the lane's open button, open the video in
  QuickTime Player on macOS or mpv on Linux (`/usr/bin/mpv` only), never your
  default application, and only in a format that player plays. The player
  gets a private copy of the file Kettle showed, checked to still be that
  file: a clone where the disk allows, otherwise a copy that leaves 2 GiB
  free, owner-only, quarantined on macOS, up to 4 GiB, and deleted when
  Kettle exits. It opens paused.
- A video plays in its lane when you press its `▶` (Space while the lane has
  the keyboard, or the `video_play` action): every frame at its time, up to
  1080p and 30 frames a second, decoded in a sandboxed worker of its own.
  On macOS, AVFoundation plays MP4 and QuickTime with their sound, from that
  worker's own output, each frame kept to the sound as you hear it (the
  output device's delay, Bluetooth's included, allowed for); `♪` mutes it
  (M while the lane has the keyboard, or `video_mute`), across seeks, until
  you unmute it. Otherwise your ffmpeg plays the picture only. The footer
  says when a video plays muted, or why it plays without sound. `‖` pauses it; the bar under the picture shows where it is and
  moves it there when you press or drag along it; Left and Right step it
  five seconds while the lane has the keyboard; screen readers get the bar
  as a slider. It pauses when its lane is hidden, until you play it again,
  holds its last frame at the end, and at most two lanes play at once (the
    one played least recently waits on its frame). It never starts on its
  own, for a control client or a Lua script, or for a file that changed; a
  control client or a Lua script may pause or mute it, never play, move or
  unmute it.
- The `preview_play` action plays a video's silent preview instead: eight
  frames from across the video, shown in turn, each for an eighth of its
  length, at most twelve a second and at least two, until `■` stops it. The sandboxed worker makes them within two seconds, or the
  poster stays and the lane says why. With `video-preview-hover = on`, the
  pointer resting on the picture loops a preview you played for up to four
  seconds, unless `reduce-motion` says otherwise: `auto`, the default,
  follows macOS's Reduce Motion and holds the hover preview back on Linux,
  where the system does not say; `off` allows it there. It never starts on its own, never for a control client or a Lua
  script, and never makes a sound.
- `kettle ctl get_state` says which video decoders a worker uses, under
  `media.video`: AVFoundation then your ffmpeg on macOS, your ffmpeg on
  Linux, and that no GStreamer is used.

### Changed

- A pasted or dropped video's poster is made by the sandboxed media worker on
  macOS and Linux, the same way a shown video's is, from the exact file the
  receipt checked; Kettle itself decodes nothing. macOS no longer asks Quick
  Look, so WebM and other videos AVFoundation cannot read get a poster when
  ffmpeg is installed, and a generic receipt otherwise. On Linux the poster
  now comes from your ffmpeg when it is installed; without one, or for a
  video it cannot read, the thumbnail your file manager cached is still used
  when it matches the file, read by the worker as any image is (so also
  without the sandbox) rather than by Kettle.

- JSON objects in Kettle's control, MCP and other JSON replies keep their
  keys in the order Kettle writes them, in every build, rather than
  alphabetically. A JSON reader sees no difference; a script that compared
  reply text byte for byte may. `kettle exec --json` events keep their bytes.
- The control server checks each request's permission on its connection
  thread before anything else, so a refused request does no work and never
  reaches the window. A refusal now comes before parameter errors, a
  `subscribe` that fails no longer switches the connection to the event
  stream, and a request a `read-only` server refuses reads "This connection
  cannot perform control mutations." instead of naming a config value.

- Each media render attempt encodes its job without first copying the
  source, which can be a 32 MiB image, so a render no longer holds two copies
  of it while starting the worker.

- Building Kettle from source now needs Rust 1.95 or newer (was 1.89). The
  `msrv` CI job, the Nix toolchain and the build docs follow the
  `rust-version` in `Cargo.toml`. `sysinfo` moves to 0.39, whose releases
  need 1.95; the process walk behind tab titles and remote detection asks it
  only for parents, command lines and working directories, never thread
  lists.
- Image rendering reuses retired textures for same-size replacements without
  reserving a second texture, preserves every image needed later in the frame,
  and releases decoded CPU pixels independently of the GPU cache.
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
  each Spanish message's review status is recorded by key. Every Spanish
  message has been reviewed in context on the rendered Spanish build and
  accepted.
- The tab bar's numbered fallback label (`tab 2`) and an untitled pane's
  screen-reader name follow the UI language. `kettle ctl` tab titles keep the
  English `tab 2`.
- The right-click menu's Theme submenu, a list of every bundled theme, is now
  a Theme… row that opens the theme picker.
- Screen readers read a picker row's hint after its name, such as a
  command's shortcut or a theme's appearance.
- Stepping through themes (`next_theme`, `prev_theme`, and ←/→ on the Settings
  Theme row) stays with the current theme's appearance and follows an order
  of likeness, judged by background, foreground and accent; the Settings row
  steps through the popular themes in that order. It used to follow the
  themes' names or the popular list's order, flipping between light and dark
  palettes. `toggle_light_dark` still switches appearance.
- Close confirmations name the tab or window and count in words that match
  the number: "Close tab with 1 pane?", "Close window with 3 panes?" rather
  than "Close 3 panes?". The paste prompt says "1 line" or "2 lines". They
  used to say "pane(s)" and "lines" whatever the count.

### Fixed

- `login-shell = true` with no `command` no longer stops Kettle at launch on
  macOS and Linux. It added `-l` to your default shell, which takes no
  arguments there, and the pane's spawn panicked. Your default shell already
  starts as a login shell; `login-shell` now adds `-l` only to a shell a
  `command` names.
- `kettle mcp` lists its tools to clients on MCP 2026-07-28, which include
  current Claude Code. Its tool list left out the `ttlMs` and `cacheScope`
  fields that revision requires on every list result, so those clients
  rejected the list and showed Kettle's server with no tools.
- `kettle ctl send_mouse` with `button` `back` or `forward` reaches a
  mouse-tracking program as the physical Back and Forward buttons do. It used
  to send xterm's button numbers 8 and 9 as report codes, which read as a
  left or middle press with Meta held.

- A mouse Back or Forward release reaches a mouse-tracking program only after
  a press it received. A press that dismissed a context menu or that a dialog
  took used to leave the program a release with no press, and one it received
  lost its release when a dialog opened while the button was held.

- Every mouse press a program receives gets exactly one release, in its own
  pane. A button held while focus moves to another pane or tab keeps its drag
  and release with the program that got the press; the release used to go to
  the newly focused pane, leaving the first program a press with no release
  and handing the other a release with no press. Two buttons held at once
  each keep their own, and Back and Forward follow the same rule. When Kettle
  ends a drag itself (the window loses focus, search or a menu opens, the
  pane turns read-only or is reset, or the tab moves to another window), the
  program gets its release at the last cell it saw instead of waiting
  forever. Pressing Shift partway through no longer drops the release, a
  press the program never got (its input was full) leaves no release behind,
  and a release always gets through: each pane's input keeps a little room
  that only mouse releases may use. The newest press owns the drag, so a
  Shift-drag selection started while a program holds Back still selects.

- `kettle ctl`, `kettle mcp` and other control clients keep finding running
  servers after many Kettle sessions. Each exit used to leave its control
  socket behind; once more than about a thousand piled up, discovery stopped
  before reaching live servers' entries and reported that no server was
  running. Servers now unlink their socket on exit, pruning a dead server
  removes its socket, and a starting server clears leftovers in the registry
  directory, including those from earlier releases. Leftover sockets no
  longer count toward the 1,024 entries a discovery reads, within a walk of at
  most 8,192 directory entries.

- A paste receipt and a pending video preview move with their tab when it is
  torn off, moved to a new window or docked into another window, keeping
  their pixels and remaining time. They used to stay in the old window, and a
  video poster that finished after the move was dropped. A failed move puts
  them back. Docking replaces only the part that arrived, so a receipt or a
  pending preview that belongs to another tab in the target window stays,
  and a preview that fails no longer removes another tab's receipt.

- Dragging a tab onto another window docks where that window's tab bar is
  painted. A bottom bar's docking band ignored the search bar and a bottom
  status bar, and a side strip's band ran under the search bar. The split
  preview while dragging a pane now matches the pane the split creates,
  rounded the same way.

- In Spanish, the pasted-path receipt titles said "pasted image" and "pasted
  video" where the card shows a pasted path; they now say "Ruta de imagen"
  and "Ruta de video". "Ocultar el puntero al escribir" replaces "ratón",
  since the setting also hides the trackpad pointer.

- Automatic window accents follow a palette change that keeps the theme's
  name, such as a config reload that edits one palette color. Accents were
  re-resolved only when the theme name changed, so windows kept their old
  colors and a tab torn off afterwards could take its source window's color.
  A window whose accent changed while its renderer was being rebuilt after a
  GPU failure also keeps the new accent once the rebuild finishes, instead of
  going back to the old one.

- An opaque context menu, Settings panel or paste receipt over the cursor
  cell now hides the whole cursor. The inverted glyph of a focused block
  cursor was drawn after every overlay and showed through them.

- On macOS, a window move reported while Kettle is handling a key or other
  non-mouse event no longer aborts it. The caption-drag check read mouse-only
  event fields, which AppKit refuses with an exception for those events.

- Kitty transmission and placement commands return completion replies after
  actual image storage or placement admission, including synchronized output.
  Chunked replies retain the original ids and quiet settings. Missing images,
  missing parent placements, invalid data, and refused storage return errors.
  Anonymous images remain silent. Refused relative replacements keep the previous
  placement, accepted relative definitions survive reflow during admission, and
  combined uploads preserve their relative destination. Zero identifiers are
  treated as absent, unrecognized actions cannot become uploads, and a delete
  refused for conflicting identifiers leaves partial uploads intact.

- Kitty animation frame uploads and composition return completion replies after
  refreshing animation state. Uploads report the actual one-based frame number.
  Missing frames and invalid composition rectangles leave existing pixels intact;
  zero composition dimensions use the full source canvas. Partial appended frames
  use a transparent canvas unless a background color or existing frame is selected.
  Animation controls require an existing root and preserve the current frame
  when given an invalid selector.

- Torn-off windows reserve automatic accent colors in process even when the
  optional presence registry is unavailable. Closing a window frees its color
  for reuse. When the palette is full, a torn window avoids the original
  window's color if another hue exists; reuse favors the least-used hue.
  Pinned colors retain their configured behavior.
- On macOS, dragging a released single-tab window by its tab uses manual
  follow and keeps the original press point under the pointer. A fast drag
  processes its first movement immediately, allowing that same gesture to
  latch a sibling window's insertion target before release.
- Native macOS caption drags of a single-tab window use the same rejoin path
  when its automatic tab bar is hidden, and holding Escape before release
  cancels the rejoin. Desktop-to-client docking coordinates account for each
  target window's display scale.

- New Kitty image ids remain addressable at image-count and retained-byte limits
  by reclaiming eligible old images, with unplaced images first. Pixel snapshots
  keep their charge, and active count pressure preserves the other screen's roots.

- Kitty relative placements select the exact parent named by `Q=`, including
  chained and virtual placements. Rendering and spatial deletion use the same
  origins; a missing explicit parent cannot fall back to another placement.

- Kitty image-id retransmission retires old placements and animation data with
  the first accepted chunk, releasing their pixel leases before replacement
  decoding. Unrelated uploads and matching ids on the other screen survive.
  Self-composition releases its temporary source handle before editing pixels.

- Kitty frame composition uses the correct source and destination offsets.
  Newly appended animation frames default to 40 ms; edits with omitted or zero
  delay preserve the existing timing.

- Kitty root-frame edits refresh existing physical, virtual, and relative
  placements, including synchronized output, while preserving placement
  geometry, animation timing, and independent primary/alternate screen images.

- Kitty animation edits work when the old GPU cache still has a weak reference
  to the pixels. They transfer the buffer without copying it; retained pixel
  snapshots keep their memory charge until their last handle is released.

- Partial Kitty animation frames keep the full image canvas size. If the canvas
  cannot fit in the current memory quota, the frame is refused without changing
  the image; retrying after memory is released works normally.

- Small encoded terminal images can load when the remaining image quota holds
  their decoded pixels. They no longer need room for the maximum image size.
  An already-RGBA8 decode also avoids a redundant copy of its pixels.

- macOS release packaging works with the system Bash when assembling command
  arguments for ordinary binaries and the media worker. It signs nested binaries
  before the app bundle so the new helper does not block packaging.

- On macOS, a video preview helper that exits before reading its request can
  no longer close Kettle. A write to a pipe nobody reads raises SIGPIPE on the
  whole process there, so blocking it on the writing thread did not help; the
  pipe is now marked to fail the write quietly instead.
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

## Older releases

- [4.x](docs/changelog/CHANGELOG-4.x.md)
- [3.x](docs/changelog/CHANGELOG-3.x.md)
- [2.x](docs/changelog/CHANGELOG-2.x.md)
- [1.x](docs/changelog/CHANGELOG-1.x.md)
- [0.x](docs/changelog/CHANGELOG-0.x.md)
