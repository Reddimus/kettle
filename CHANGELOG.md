# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

## [6.0.0] — 2026-11-08

### Added

- Settings › Appearance has a Minimum contrast row after Theme, for
  `minimum-contrast`: Off, 3, 4.5 or 7. A ratio set by hand in the
  config, such as 2.5, shows as it is until you step it.

- The theme picker shows more under its list. A sample card draws the
  highlighted theme as the terminal would: a prompt, an `ls -l` listing, a
  diff and all 16 palette colours, with your colour overrides and
  `minimum-contrast` applied. Its last line gives the contrast of the
  theme's text and of its least readable colour, with that colour's swatch,
  and marks a colour below 3:1. A status line says whether the theme
  follows the system, a schedule, a forced side or is fixed, names the dark
  and the light theme and marks the side a pick saves to, and has a
  Contrast button that steps `minimum-contrast` through Off, 3:1, 4.5:1
  and 7:1. The list now shows up to 14 rows when the window has room; on a
  short window the card goes first.

- The theme picker can change the theme for the appearance you are not in.
  Click the other side's name on its status line, such as "Light:" while
  the system is dark: the list moves to that theme, the window previews your
  picks, and Enter (the hint reads "save for light appearance") writes only
  `light-theme` and puts the window back on its dark theme. Screen readers
  offer both sides as radio buttons. Tab or Shift+Tab switches to the other
  side too (the hint reads "Tab: light theme"), while the arrows keep moving
  rows; with a fixed theme, which has no sides, Tab moves down a row and
  Shift+Tab now moves up one.

- A searchable list of every keyboard shortcut. Choose Help > Keyboard
  Shortcuts on macOS, Keyboard shortcuts in the new-tab ▾ menu, or "Show
  keyboard shortcuts" in the command palette: the palette opens listing only
  the commands that have a shortcut, each with its shortcut, and Enter runs
  the selected one. Its empty field reads "Search keyboard shortcuts…". The
  action is `show_keybinds` (alias `keyboard_shortcuts`) for a `keybind` line;
  it has no default shortcut.

- macOS has a Help menu, last in the menu bar, with Kettle Help (the README,
  as the palette's help command opens) and Keyboard Shortcuts. As in other
  Mac apps, its search field finds any menu command by name. The new-tab ▾
  menu on every platform gains Keyboard shortcuts and Help rows.

- The command palette's empty search field says "Type a command…" in grey
  until you type, in the UI language. In a window too narrow for it beside the
  key hint, it is left out rather than cut off.

- Broadcast input has its own menu. The macOS Shell menu and the
  right-click menu have a Broadcast Input submenu (Enviar la entrada a varios
  paneles) with Off, Panes in This Tab, Panes in This Window and Panes in
  This Group. The current scope is checked, choosing the checked row stops
  broadcasting, and the broadcast chord's shortcut shows on the row it turns
  on. Panes in This Group is unavailable while the focused pane has no
  group. The checkmark also follows a group scope that ends because its last
  pane closed.

- On macOS, the title bar shows a Settings gear before the new-tab `▾ +`
  buttons, so Settings is one click away in every window. Clicking it opens
  Settings, as Cmd+, does; its tooltip names whichever key opens Settings,
  and screen readers call it Settings (Configuración in Spanish). The gear
  sits apart from `▾ +`, which do not move, and it hides only when the
  window is too narrow to keep a tab beside it. A lone tab stays centred
  on the window beside it, narrowing in a narrow window. Windows and Linux keep
  Settings in their `▾` and `≡` menus.

- Making the font larger or smaller, or resetting it, from the keyboard,
  the menus or Ctrl+scroll shows the new size and the grid it leaves, such
  as "Font size 14 · 80×24", in the middle of the window for a moment, the
  way a window resize shows its size. Screen readers announce the same
  text. `resize-overlay = never` turns it off along with the resize chip.

- Vi mode can select whole lines and blocks from the keyboard, as Vim does:
  `V` starts a line selection and `Ctrl+v` a block (rectangular) selection
  at the vi cursor, beside `v` for characters. Pressing another of the three
  turns the current selection into that kind, and the same key again ends
  it. `y` copies whole lines or the selected columns of each row.

- After a session restore on macOS and Linux, a shell that runs `clear` at
  startup no longer hides the restored output. The restored rows and the
  "Restored" divider stay on screen with the prompt below them, whichever
  order `clear` sends its sequences in. Only that first clear is affected;
  later clears work as before. A startup program that clears and then draws
  at fixed positions outside the alternate screen can draw over restored
  rows; `restore-scrollback-lines = 0` turns restored output off. Windows is
  unchanged.

- Renaming a tab and moving it to a new window no longer need the right-click
  menu or a tab drag. The command palette lists "Rename tab…" beside its
  existing "Move tab to new window", and on macOS the menu bar's Window menu
  has Rename Tab…, Move Tab Left, Move Tab Right and Move Tab to New Window.
  The move rows are unavailable while the window has one tab, and Move Tab to
  New Window also while `detachable-tabs = false`. No new default shortcuts.

- Each row of the theme picker shows a small strip of that theme's colours:
  its background, its text colour and six accent colours. In a narrow window
  the strip is dropped before a theme's name is shortened.

- When a program that uses the mouse, such as Vim, htop or tmux, takes a
  right-click, a short notice on its pane says to hold Shift to select text
  or open the menu. It shows once each time a program turns the mouse on, and
  at most once every 30 seconds in a window, and not at all with
  `putty-paste-style` on, where Shift+right-click pastes. The menu that
  Shift+right-click opens over such a program starts with the same sentence,
  wrapped to the menu's width, and a screen reader reads it as one item.

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

- The context menu's Read only row and the Preferences check boxes and
  choices reach screen readers as check box and radio menu items that carry
  whether they are on. On Linux and Windows their names no longer start with
  the check mark or dot drawn before them; on macOS the names keep the mark,
  because it is still how VoiceOver hears the state. The menu looks the same,
  and a row's shortcut is still given as its description.

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
- `kettle ctl ui_geometry` reports `typed_echo`, counts for comparing builds
  while typing: keystroke echoes, echoes the shell wrote in two parts (as
  zsh-autosuggestions does when a key changes its suggestion), frames
  presented per keystroke, `output_frames` (those that painted the keystroke's
  echo), `intermediate_frames` (frames that showed the half-written state of a
  split echo), `late_superseded_frames` (frames replaced within one refresh
  rather than within 3 ms), `settled_echoes` (waits that ran to the 3 ms
  deadline) and `settled_early` (waits that ended when Kettle handled the
  wake of the echo's second write). They count only typed characters and
  backspaces sent to a pane with nothing left unpainted, and only that pane's
  output: Enter, Tab, arrows, control keys, pastes, other panes' output and
  type-ahead while a pane prints are left out. Counts only, never text.

### Changed

- The macOS menu bar reads like a Mac app's. View has one Enter Full Screen
  item, on Control-Command-F, that reads Exit Full Screen while the window
  is full screen; before, View listed Kettle's item and the system's both.
  F11 and the green button still toggle the same full screen. Shell lists
  Split Left and Split Up beside Split Right and Split Down. Window starts
  with the system's Minimize (Command-M) and Zoom and ends with Bring All to
  Front. Full screen, Minimize and Zoom work on a borderless window too, and
  Minimize and Zoom are off while the window is full screen, as in other
  apps. English titles are in title case: SSH Launcher…, Quick-Select Hints
  and Toggle Quick Terminal. A chord you bind yourself still wins over the
  menu's. When Command-M and Control-Command-F are not bound in Kettle, macOS
  takes them for the menu before Kettle sees them, so programs running in
  Kettle and keybind capture in Settings don't receive them.
- The key hint beside a picker's search field and its "No matching command"
  style messages are easier to read: they use the same clearer grey as menu
  shortcuts, at least 7:1 against the panel where the theme's own text
  reaches that, instead of about 4.5:1. Unavailable rows in right-click menus
  stay dimmed.
- The command palette and the theme, layout and SSH pickers show eight full
  rows starting right under the search field when the list is longer than
  that, instead of six rows under an empty strip, and a thin scrollbar thumb
  at the right edge shows where in the list you are. The panel ends at its
  last row in a short window. The wheel and the arrow keys scroll as before;
  the strips that scrolled when hovered are gone. Right-click menus are
  unchanged.
- The command palette ranks a command by the best way the typed text fits
  its name, and leaves out names the text only lies scattered through while
  a closer match exists. "font" now lists the font-size commands and no
  longer puts "Focus next pane" first, "split" no longer lists the broadcast
  and close-confirmation rows, and "theme" marks the word in "Next theme"
  instead of the "t" of "Next". The SSH launcher and theme picker rank by the
  same better fit. A text that only fits scattered still lists everything it
  fits.

- On macOS, Kettle waits up to 3 ms before painting a typed echo from a pane
  whose echoes have been arriving in two writes, so both writes can land in
  one frame. The second write wakes Kettle, and Kettle paints when it handles
  that wake; only an echo that comes in one write waits the full 3 ms, and so
  does a key typed before the previous key's echo has arrived.
  zsh-autosuggestions redraws each key that changes its suggestion in two
  writes about a millisecond apart, first without the gray suggestion and
  then with the new one, and Kettle could present the first on its own for a
  whole refresh. A pane starts waiting when the echo of a typed character or
  backspace arrives in two writes within 3 ms, and stops 5 seconds after the
  last such echo. Only typed characters
  and backspaces wait: Enter, Tab, arrows and other control keys are painted
  at once, even in a pane that waits, and so is type-ahead while the pane is
  still printing. A paste of any length never starts a wait or teaches a pane
  to wait. Shells that echo each typed key in one write are painted at once,
  as before. `ui_geometry.typed_echo.settled_echoes` counts the waits that ran
  to their deadline, and `settled_early` those that ended at the second write.
  In a live comparison on macOS with zsh-autosuggestions, a build that always
  waited the full 3 ms, before the wait could end early, cut the frames that
  showed a half-written echo from about one per split echo to one in twenty.
  Ending the wait at the second write has not been measured live.

- The command palette names the broadcast chord's row after the scope it
  turns on, so with `broadcast-default = all` it reads "Broadcast input to
  every pane in window" instead of claiming the tab. Each scope is listed
  once in both languages. A new `broadcast_panes_in_tab` action always
  broadcasts to the focused tab, whatever `broadcast-default` says; it has no
  default chord. `broadcast-default = off` still keeps the chord from turning
  broadcast on.

- The profile menu is now called New Window with Profile (Nueva ventana con
  perfil), since each profile has always opened in a new window. In the
  macOS Shell menu it sits right below New Window.

- The size chip a window resize shows (`120×40`) is now as wide as its drawn
  text, measured from the same text it paints, rather than from a count of
  grid cells. That lets it fit the font size caption, and on any platform the
  plain `cols×rows` chip may be a pixel wider or narrower than before.

- When a window loses focus its selection turns a neutral grey, as a
  selection in an inactive macOS window does, instead of a dimmer shade of
  the same colour, so it changes colour as well as strength. The grey stays
  at least 2:1 from the theme's background and at least 1.3:1 quieter than
  the focused colour. With the window's background see-through (the macOS
  and Windows default), selection colours are now chosen against the
  background as it shows over the desktop, not against the theme's solid
  colour. There the unfocused grey reaches about 2:1 where the focused
  colour leaves room for it (TokyoNight Night at 86% opacity: 1.85:1, from
  1.31:1 before), and the focused colour moves a little further from the
  background, on the side it already sits, while selected text keeps at
  least 4.5:1. Where the background as shown would pass the selection
  itself, as TokyoNight Night's does over a white page below about 65%
  opacity, both colours stay those of an opaque window. Linux, at 99%
  opacity, changes by about 1%. An opaque window, and one under macOS
  Reduce Transparency, keeps the focused colours it had.

- Background tabs in a top or bottom tab bar mark their state with a shape
  as well as a colour: a filled dot for new output, a ring when the output
  has stopped, and a triangle when the bell rang. The marks are larger,
  scale with the display instead of shrinking on Retina screens, and keep at
  least 3:1 contrast against the tab behind them, hovered or not. Vertical
  tabs keep the old dots until 6.1. Screen readers read the state after the
  tab's name, for example "build, bell rang".

- The active tab's bar is thicker (3 pixels, scaled) and keeps at least 3:1
  contrast against both the tab and the strip, in broadcast mode too. Over
  a wallpaper, some strip colours leave no colour at 3:1 against both (41
  bundled themes under the default `chrome-background = theme`), and the
  bar then takes the strongest contrast it can.
  Hovering a tab or a strip button now shades it more lightly, so the active
  tab stays the strongest mark in the strip.

- When closing a tab, window or pane would stop a program running in a
  single pane, the question names it: "The program sleep is still running."
  Only the program's name is shown, never its arguments. Shells, programs
  Kettle cannot read, and Windows keep the general wording.

- Kettle's own menus look more like the system's. The right-click menu and
  its submenus are rounded panels on a soft shadow, as the command palette,
  theme and other pickers now are too, and in all of them the highlighted row
  is a rounded, inset highlight instead of a full-width tint with an accent
  bar. An open submenu covers its parent's highlight and labels, shadow
  included, where the two overlap, and an open menu covers a notification
  beneath it. Fonts, row heights and where each row responds to the mouse are
  unchanged.
- Closing a single pane, or a tab or window holding only one, asks first when a
  program is running in it, as Terminal.app and iTerm2 do: a job such as
  `sleep 600` or an editor started from the shell, or the pane's own program
  when it was started as something other than a shell (`vim`, `ssh` or a script
  as its command). That includes the titlebar close button and Alt+F4 on a
  one-pane window; a window holding several panes still closes from the
  titlebar at once unless the setting is `always`. An idle shell still closes
  at once.
  This is the new default, `ask-before-closing = busy-or-multiple`, which also
  appears in the Preferences menu and the command palette. The menu's `always`
  row now reads "Confirm close: always", since the titlebar is no longer
  special to it alone. A config that sets
  `multiple-terminals` keeps asking only when more than one pane would close.
  Windows keeps the old behaviour for a single pane.
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
  and appearance commands into submenus, so it no longer scrolls. Each
  submenu row is described to screen readers as a submenu. On macOS,
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
  Narrow headers keep complete key-hint segments ending with Esc when they
  fit beside the full query; hints disappear when the query needs the space.
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

- On macOS, title-bar buttons no longer drag the window. AppKit moved the
  window with any press in the title bar, so a press on the Settings gear,
  `▾`, `+` or a tab's close button that was dragged off came back over the
  button with the window, and a release there opened Settings. Dragging a
  tab in a window with several tabs moved the window too, instead of
  reordering the tab or tearing it off. Empty title-bar space and a lone
  tab still move the window, and a double-click on empty space still zooms

- The outlines of Settings' fields, choices and switches, the search
  field, the category arrows and an off switch's knob were about 1.5:1. They
  now keep 3:1 against the panel (3.0:1 in TokyoNight Night). On the
  highlighted row they are brighter, keeping 3:1 against the highlight too
  (6.2:1 against the panel there). The panel's own outline and divider stay
  quiet.

- On a translucent window, text on the active tab was checked against the
  theme's background rather than what the window shows over the desktop.
  Over a light desktop the Cmd+N hint fell to about 3.2:1 at macOS's default
  86%. In a focused window the title, hint and close glyph now keep 4.5:1
  against the tab as it shows over a black or a white backdrop. On macOS,
  which shows a translucent window a few levels brighter at both ends, the
  backdrop is lifted by the amount one capture measured. Where no text colour
  can reach 4.5:1, the active tab is drawn up to 10% more opaque over a solid
  background; at 86% that is Blue Dolphin, Grass and Hot Dog Stand, and on
  macOS also Fairyfloss and Ocean. It is never raised over an image,
  starfield or transparent background, where it would show as a seam, and
  past that limit the text is as clear as it can be. Linux's default 99%
  window is translucent too, so its tab hints move by up to 7 levels.

- In an unfocused window, tab titles and Cmd+N hints were dimmed to 3:1, as
  low as 2.5:1 on a translucent window. Titles are still dimmed, but keep
  4.5:1, because they name the tabs. The hints, which are for the keyboard,
  are no longer drawn until the window is focused again; their space stays,
  so nothing moves. The +, ▾ and × glyphs keep their 3:1.

- The search bar's "No matches" fell to 4.45:1 on a translucent window,
  because the terminal showed through the bar. Over a translucent terminal
  the bar is now drawn opaque, in the colour it always had over the theme's
  background.

- The "↓ N lines" button's label reached 4.5:1 only at full ink, passing by
  a rounding step (4.503:1 in TokyoNight Night). It now keeps 4.5:1 at the
  edges of its strokes too. In a few themes the button's fill moves slightly
  toward the terminal to make room.

- On macOS, quitting with ⌘Q, the Dock's or the app menu's Quit, or by
  logging out or shutting down now saves the session on the way out, as
  closing the last window already did. Before, the restored session could
  miss whatever changed since the last background save (a directory, a
  split, a tab title), and a save still being written could be lost. The
  same quit now also deletes the private copies of pasted screenshots and of
  files opened in the image viewer, document viewer or video player, which
  were left behind. A slow disk or a hung network home holds a quit up for
  at most about 2 seconds; a session write that has not finished by then
  leaves the previous session file intact.

- A link no longer draws its hover underline across text you have selected.
  While a selection is being dragged, the links it passes over show no
  underline, "click to open" hint or hand cursor. Once the button is
  released, a selected link keeps its hint and hand cursor, and its underline
  comes back when the selection is cleared. ⌘-click and Ctrl+click still open
  it.

- Starting a recording into a directory could fail with "resource
  temporarily unavailable" when a new pane opened at the same moment as
  recording retention removed an old file. Kettle now takes the next file
  name instead.

- In an unfocused window, the active tab's bar was the same colour as a
  hovered tab and reached only 1.5:1. It is now grey at 3:1 or more, with
  the same wallpaper exception as the bar itself, and vertical tabs follow
  window focus too.

- In a top or bottom tab bar, tab activity marks in several light themes,
  and the Silent mark in TokyoNight Night, fell below 3:1 contrast.
  Vertical tabs keep the old dots until 6.1.

- When the config file is saved while the theme picker previews a theme,
  such as by the picker's Contrast button or an edit in another app, the
  reload puts the preview back at once, so the window no longer briefly
  shows the saved theme under the picker.

- A scrolling picker (the command palette, theme, layout and SSH pickers) no
  longer leaves a blank part-row under its last result. It used to paint six
  whole rows in room for eight; it now paints eight, and the last row it
  paints takes a click.

- On the highlighted row of the command palette and other pickers, the
  characters matching the query were drawn in the accent colour over the
  accent-tinted highlight, at about 3.5:1. They now keep at least 4.5:1 on
  every row in every bundled theme.

- On macOS, Kettle turns off AppKit's automatic window tabbing at launch.
  Kettle's tabs are its own, drawn inside one window, but AppKit's tabbing
  was left on, so under the "Prefer tabs when opening documents" system
  setting (by default, while in full screen) a new Kettle window, such as a
  tab moved to a new window, could open as an AppKit tab of another Kettle
  window, and AppKit could add its own tab items, a second Move Tab to New
  Window among them, to the Window menu.

- A screen of characters that no installed font covers no longer freezes the
  window for seconds, and terminal queries such as cursor-position reports
  are answered promptly while it draws. Finding no font for such a character
  used to shape it with every installed font, and again each time a row
  holding it was redrawn. Kettle now skips fonts whose character map shows
  they cannot supply it, and remembers the clusters (a character with any
  marks on it) that no font's map holds. The fonts chosen matched the
  previous behaviour for every character tested, alone and in a range of
  combining, emoji and right-to-left sequences.
- Settings no longer asks you to restart Kettle or open a new window after
  you change the background opacity of a window that already shows it, such
  as the 99% default on Linux. The note now appears only when the window was
  opened opaque and is turning translucent, or when blur is turned on in a
  window opened without it.
- The transient terminal-size chip uses an opaque chrome surface with readable
  text and outline in light, dark and custom themes, including translucent
  windows. Its accent follows the window's resolved accent.

- Broadcast keystrokes update each recipient pane's accessibility state only
  after that pane accepts the input, including keys delayed behind a media paste.
- Command-status marks follow scrollback and scroll-region changes instead of
  pointing at unrelated output. Media-paste receipts keep their click targets
  when a notice moves them, instead of returning the pane to live output.
- Settings switch labels fit beside their controls in narrow rows, and tooltip
  bounds update when the configured font changes.
- Profile discovery runs off the input thread for the new-tab menu, command
  palette and native macOS menu bar. A status message distinguishes discovery
  in progress from an empty list; open menus and palettes keep their choices
  stable until reopened. Profile launches also run off the input thread, and
  switching to a missing or unreadable cached profile keeps the current profile.
- Wayland tab tear-off accounts for the painted horizontal tab offset. Closing
  panes also releases their cached accessibility output state.
- Text and emoji variation selectors keep the surrounding grapheme's cell width
  correct across streamed input. Saving and restoring private keyboard modes
  keeps Backarrow negotiation as well as its value.
- macOS comparison checks clean up their owned windows after setup failures,
  and Linux UI checks compare captures with reviewed reference images.

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
  mode: `//`, `->`, `!=`, `===` and the `::` in C++'s `std::cout` no longer
  lose their left half.
- With grapheme clustering (mode 2027) on, a block cursor over a single
  character whose glyph is wider than its cell, such as a Nerd Font icon, no
  longer cuts the icon off at the cell's edge.
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

## Older releases

- [5.x](docs/changelog/CHANGELOG-5.x.md)
- [4.x](docs/changelog/CHANGELOG-4.x.md)
- [3.x](docs/changelog/CHANGELOG-3.x.md)
- [2.x](docs/changelog/CHANGELOG-2.x.md)
- [1.x](docs/changelog/CHANGELOG-1.x.md)
- [0.x](docs/changelog/CHANGELOG-0.x.md)
