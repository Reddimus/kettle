# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

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
