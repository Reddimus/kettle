## [3.3.0] — 2026-08-24

### Changed

- **Windows 3.3.0 is the final supported Windows release.** It keeps the
  x86_64 archive, installer, and signed update target so existing clients can
  receive the end-of-life notice before 4.0 stops publishing Windows packages.
  When a signed 4.0-or-newer manifest has no Windows target, Kettle now shows
  the release page instead of failing silently or offering an install.

## [3.2.1] — 2026-08-24

### Fixed

- **macOS: a light theme no longer draws the window title through the traffic
  lights.** With any light theme the title sat at the far left of the titlebar,
  across the red and yellow buttons, its leading characters clipped outside the
  window. Dark themes were unaffected, which is what made it look like a
  drawing bug rather than a layout one.

  It was neither. winit turns a creation-time theme hint into an `NSWindow`
  appearance override and applies it before AppKit has built the titlebar's
  container view. The native material view kettle adds to the frame view
  immediately afterwards then locks that state in: AppKit keeps its
  container-less layout, in which the title is centred over the 72-point
  traffic-light cluster rather than the caption, putting the text field at
  x = -10. Nothing recovers it later — taking the material view back out does
  not bring the caption back, and neither does re-applying the appearance.

  macOS now reaches the window with no appearance override at all and takes
  the hint once the window is on screen and key, which is the path the runtime
  light/dark toggle already used and which leaves the caption intact. The hint
  is tracked per window rather than once per process, so a second window still
  gets it. Every other platform still seeds its caption at creation, where it
  costs nothing and avoids a first-frame flash.

## [3.2.0] — 2026-08-23

### Added

- **`kettle update` works on macOS.** The updater covered Windows and Linux;
  on a Mac it answered "update through the package manager or installer that
  owns this executable", because there was no macOS code path at all. The
  universal archive now ships in the signed update manifest as
  `universal-apple-darwin`, and the app can replace itself.

  It replaces the whole bundle rather than files inside it. A code signature
  seals `kettle.app` as a unit, so a bundle caught part-way through a
  file-by-file update is one Gatekeeper rejects, which on macOS can mean an app
  that will not launch at all rather than one running an older binary. The
  verified archive is extracted into a private directory beside the live bundle,
  checked with `codesign` and `spctl`, and only then exchanged with it in one
  atomic operation. Nothing is ever written into the bundle you are running.

  Extraction and the exchange both resolve names against a descriptor for that
  staging directory, taken when it is created, and every write uses `O_NOFOLLOW`
  and `O_EXCL`. `/Applications` is writable by every administrator on the
  machine, so a pathname is not a stable thing to build on: without this, a
  symlink planted mid-extraction could redirect a write outside the bundle, and
  renaming the staging directory after verification could substitute what got
  installed. Bundle directories are created `0755` rather than inheriting the
  umask, which under `umask 002` would have left an installed bundle writable by
  the user's group.

  The `spctl` check is the load-bearing one. Re-signing a bundle changes its
  cdhash, which orphans the stapled notarization ticket while leaving
  `codesign --verify` perfectly satisfied, so a signature check on its own would
  install a build Gatekeeper then blocks. Both tools ship with macOS; `stapler`
  does not, so it is not used.

  Ownership is proven from the signature rather than an installer marker, since
  macOS has no installer and a marker inside the bundle would break the seal. A
  Homebrew-managed copy, an app translocated read-only out of Downloads, and a
  locally built ad-hoc-signed app are each refused with the specific next step
  rather than a permission error. Existing Windows and Linux clients are
  unaffected by the fourth manifest entry: each one looks up only its own target
  and ignores the rest.
- **`dispatch_ui_key` can drive every modal text field, not just Search.** It
  refused any batch unless the search bar was open, which is why Search was the
  only modal with end-to-end coverage — and why the modal text-entry defects
  listed under Fixed went unnoticed in the fields that had none. It now resolves the command palette,
  the Settings path prompt, the layout picker, the SSH launcher, the title
  editors and Search in the same precedence the real key handler uses, reports
  which modal it typed into, and stops early if that modal closes mid-batch.
- **Screenshots can be cropped before Kettle writes them.** The control and MCP
  screenshot commands accept `crop_x`, `crop_y`, `crop_width`, and
  `crop_height` in window-relative physical pixels. Kettle validates the full
  rectangle and crops the GPU readback before it creates the output file, so
  automation can capture one UI region without first persisting the rest of the
  terminal.
- **Clipboard screenshots now have a visual receipt.** After the initiating
  pane accepts its private temporary PNG path, Kettle shows a bounded thumbnail
  inside that pane without claiming the client attached it. The card contracts
  after four seconds, expires after 30 seconds, and expands while hovered. A
  two-minute hard limit prevents a hovered card from staying over terminal
  cells forever. Click the body to open the retained PNG or `×` to dismiss it.
  The newest media paste replaces the previous card, and later input removes
  it once the command line can no longer be described reliably.
  It avoids completion cards and pane chrome, warns for remote sessions, and
  never previews arbitrary terminal paths. Set
  `paste-image-preview = off` to keep bitmap-to-path paste without allocating
  or retaining thumbnail pixels.
- **Copied and dropped videos now get a safe poster receipt.** The first video
  in an accepted path paste shows a bounded native thumbnail when available;
  batches keep their count and remote panes keep the local-path warning.
  Kettle never decodes video or scans terminal text. Poster work is isolated,
  bounded, requires a trusted file and parent chain, and rechecks a retained
  file handle around extraction. Video cards are informational and cannot open
  the source through a later path lookup; a body or `×` click dismisses the
  card without reaching hidden terminal content. Pending state expires after
  38 seconds if a worker disappears without replying. Set
  `paste-video-preview = off` to keep path paste without the receipt.


### Changed

- **The search bar says what its controls do.** `[x] Wrap` became `Wrap: On`,
  so the state is read rather than decoded. `Case: Smart` gained a `›` to show
  that clicking cycles it, which nothing previously communicated. And `[x]
  Invert` — which never said what it inverts — became `Enter: Next` /
  `Enter: Prev`, naming the direction Enter searches and teaching the
  keybinding at the same time. The query lost its `[ ]` brackets: the field
  already has its own well, and the brackets read as syntax next to the
  checkbox toggles. Groups are now separated by two columns instead of one, so
  the row reads as query, navigation, options, outcome, close rather than one
  undifferentiated strip, in both the one-row and the wrapped layouts. A plain `Match` status is no longer printed — the
  highlight already says it, and suppressing it makes `No match` and
  `Invalid pattern` read as answers instead of as another word in the strip.


### Fixed

- **A session that failed to restore is no longer overwritten seconds later.**
  Nothing on the save side enforced the limits the load side checks, so Kettle
  wrote session files it would later refuse: seventeen open windows, more than
  256 panes, or too much aggregate surface area. The next launch rejected the
  whole file, opened one default tab, and the first save rewrote it from that
  single tab — every window, split ratio, and working directory gone. A tab
  that merely failed to rebuild, from a saved command not yet on `PATH` or a
  fork refused under a process limit, was erased the same way despite the cause
  being transient. Both now move the file aside, the way a parse error and an
  oversize file already did.

  Focus also landed on the wrong tab: `active` indexes the saved list while the
  live one has holes in it, so saved `[A,B,C,D]` with `C` active and `B`
  unbuildable restored `[A,C,D]` focused on `D`.

- **`kettle_run` says why a command never started.** It answered every startup
  failure with `exit code: 125` and nothing else. A command that cannot be
  spawned returns before the output sink exists, so the reply was built from an
  empty capture while the real diagnostic went to the MCP server's own stderr,
  where the caller could not see it.

- **One Sixel image can no longer freeze a pane for minutes.** A `!` repeat run
  stops at the maximum canvas width and `$` returns to column zero without
  growing the canvas, but nothing bounded the two together, so a payload could
  repaint the same band until its bytes ran out. Measured, a 2 MiB body took
  13.3 seconds, which scales to roughly 107 seconds at the sequence limit — one
  `cat` of a hostile file. A total column-write cap brings the same payload to
  387 milliseconds and a refusal, with room for six passes over a
  maximum-size image.

- **A session log no longer stops without saying why.** The log's escape-
  sequence stripper ended a sequence on `CAN`, `SUB` or `ST`, all of which the
  program being logged chooses to send. One that sent none of them held the
  stripper mid-sequence forever and every later line was dropped. It now gives
  up after 64 KiB and resumes, the same bound `kettle exec` uses.

- **A window short of file descriptors no longer releases every other
  window's colour.** Kettle picks a distinct accent per window by reading the
  claims other windows leave in a private directory, and the sweep deleted any
  file it could not turn into a claim — including one it never managed to open.
  One process hitting its descriptor limit therefore erased live windows'
  claims, and their colours went back into the pool for the next window to
  take. Files that read through and are not claims are still pruned.

- **Two shell cwd reports allocated before checking whether they could be
  accepted.** OSC 7 and OSC 9;9 bodies were converted to text and then rejected
  for being over the 8 KiB path limit, and the conversion emits three bytes for
  every byte that is not valid UTF-8. Both now check the raw length first. OSC
  133 D reads its exit code out of the bytes instead of converting the body to
  find one integer.

- **`docs/CONFIG.md` gave the wrong default for `inactive-color-offset`.** It
  said `0.8`; the code has always used `1.0`, so anyone reasoning about
  unfocused-pane dimming from the table was working from the wrong number.

- **A one-column pane no longer takes the whole application down with it.**
  Writing any double-width character to a one-column grid indexed past the end
  of the row and panicked on the PTY reader thread, and because release builds
  abort on panic that killed every pane in every window rather than the pane
  responsible. A CJK filename in `ls` output or an emoji in a prompt was enough.
  The terminal engine declares a two-column minimum and never enforced it;
  kettle now clamps to it in both the geometry constructor and the size handed
  to the engine, so no construction path can produce a grid that cannot hold a
  character. One column was reachable from `kettle exec --cols 1`, from a split
  narrow enough to leave a single cell after padding, and from the fallbacks
  used when a geometry cannot be resolved.

- **The one-line installer no longer lets the release channel choose how much
  it verifies.** With no explicit version, `install-online.sh` takes the version
  from the `releases/latest` redirect and decides from it whether an Ed25519
  signature is required. A redirect naming a release old enough to predate
  signed manifests therefore turned the signature check off and fell back to a
  checksum served by the same party, ending in an unverified archive's
  `install.sh` being executed. The same redirect could also pin every new
  install to an old release indefinitely. A version the channel picks must now
  meet the signed-manifest floor, and the check runs before anything is
  downloaded.
  Naming an old release explicitly still works, because that is a decision the
  person installing made rather than one made for them.

- **A system-wide Linux install no longer refuses to start for everyone but
  root.** Installing with `KETTLE_PREFIX=/usr/local` under sudo leaves the
  prefix owned by root. Every launch by a normal user then failed with
  `Permission denied` and no window, because startup could not create its
  update-recovery lock there and treated that as fatal. From the desktop entry
  the install itself creates there was no message at all. Recovery belongs to
  whoever can write the prefix, so kettle now carries on without it, exactly as
  it already did when another process held the lock.

- **Two synchronized graphics frames in one read no longer print the second one
  on the screen.** The parser handed its own callback a half-opened control
  string, and the callback re-enters the parser to replay deferred graphics, so
  the replay cancelled the string that had just been opened and left the outer
  loop reading a command as text. Anything that wraps kitty, sixel or iTerm2
  graphics in DEC 2026 synchronized updates hit it, which is the ordinary case
  for an animation: the frame was dropped and its payload was painted onto the
  grid.

- **`--check-config` no longer reports "OK" for a file it ignored.** A section
  header with one character wrong makes every line beneath it inert, and because
  each key in it is still spelled correctly there was nothing to call unknown or
  malformed. It now names the section and how many settings went unapplied.
  Sections kettle skips deliberately are reported the same way when they hold
  settings, since a user who put their colours under `[[work]]` instead of
  `[[default]]` has the same problem.

- **GTK's `<Primary>` modifier binds Control instead of nothing.** It is the
  spelling GTK's own documentation recommends, so it turns up in Terminator
  configs. Kettle did not recognize it, and an unrecognized modifier falls
  through to the key parser, so `<Primary>t` quietly became a bare `t` and the
  binding fired on the wrong chord.

- **The Arch and Homebrew packages no longer install an icon into a directory
  no icon theme reads.** Both derived a size from every `kettle-*.png` in the
  release archive, including `kettle-light-256.png`, whose name yields a size of
  `light-256` and a path of `hicolor/light-256xlight-256/apps/`. Both templates
  now match only `kettle-[0-9]*.png`. The one-line installer was never affected:
  it iterates an explicit list of sizes.

- **Searching for text you can see on screen no longer reports an error.** The
  search bar compiles every query as a regex and has no toggle to turn that off,
  so typing `call(x` or a bare `(` — ordinary terminal output — answered
  "invalid regular expression". A query that fails to parse is now retried as a
  literal. Patterns that do parse keep regex meaning, so `a|b` and `^row` are
  unchanged; that also means `call(x)` and `arr[0]` are still valid regex and
  still do not match that literal text, which needs a literal/regex toggle
  rather than a wider fallback.
- **A cold Windows video thumbnail provider could drop a paste receipt.** The
  hidden worker still has a two-second deadline, but Kettle now gives an
  explicit timeout one fresh-worker retry. Other failures remain fail-closed,
  repeated timeouts stop after the second attempt, and a worker that cannot
  reap its child retires instead of starting the next queued job beside it.
  Pending receipt cleanup accounts for the four-second per-job limit.
- **Selection drags no longer stall on the last visible row.** Auto-scroll
  previously started only after the pointer moved beyond a pane. A full-height
  pane has no client area below its bottom edge, so downward selection drags
  could not reach that state. A six-point, DPI-scaled inner drag zone now starts
  the existing one-line-per-frame scroll before the boundary. A two-point,
  DPI-scaled displacement threshold keeps held clicks, duplicate native move
  events, and small pointer jitter inert; farther overshoot keeps the existing faster
  rates. Native macOS and portable control-driver smokes cover both drag
  directions and send a sub-threshold coordinate beyond the client edge.
  Focused tests keep the supplementary native window-leave path behind the
  same gate. Opening a modal or releasing the button that owns the gesture now
  copies selected text first when copy-on-select is enabled, then ends the
  gesture, including Shift+right-drag.
- **The titlebar close button closes the window.** `✕` and `Alt+F4` routed
  through `ask-before-closing` like the close keybinds do, and because a pane
  without OSC 133 shell integration cannot prove it is idle it counted as busy,
  so in practice every window with more than one pane asked first. They are OS
  window-destroy requests rather than Kettle commands, and a terminal owns no
  unsaved-document state to veto one with, so they now close immediately. The
  close **actions** — `Ctrl+Shift+W`, `Ctrl+Shift+Q`, the tab-bar `✕`,
  middle-clicking a tab — still honor the policy in full, and
  `ask-before-closing = always` still asks on the titlebar close for anyone who
  wants it. That policy is now also a radio group under right-click
  **Preferences ▸**, so it can be changed and reversed in one place instead of
  only in the config file.
- **The visual bell flash is dimmer, and now adjustable.** The flash was a
  hard-coded full-surface wash of the theme foreground at 18% peak alpha. The
  most frequent bell in practice is an empty Tab completion — a non-event — so
  the peak now defaults to 10% and is exposed as `bell-flash-intensity`
  (0 to 1). `0` drops the flash while leaving window attention alone, which
  also gives photosensitive users a way to opt out of a full-surface flash
  without silencing the rest of the bell.
- **Command chords no longer leave stray text on the command line.** Kettle
  reported Super in the legacy xterm modifier parameter, but bit 8 there is
  xterm's Meta — a distinct X11 modifier Kettle has no key for, not macOS
  Command. `Cmd+Option+Up` therefore sent `CSI 1;11 A` and `Cmd+Left` sent
  `CSI 1;9 D`, parameters no line editor decodes, so the shell echoed the
  residue as literal `1A` or `1D`. In the other direction the character branch
  dropped Super silently, so an unbound `Cmd+A` typed a bare `a`, and
  `Cmd+Numpad` still emitted an application-keypad sequence. A chord holding
  Super now produces no PTY bytes at all on any platform unless the application
  has negotiated the Kitty keyboard protocol, which defines a real super bit and
  is unchanged. Existing keybindings are unaffected — `Ctrl+Cmd+Arrow` pane
  focus and `Cmd+Up`/`Cmd+Down` prompt jumps are consumed before the encoder.
  `send_keys` now reports which token could not be encoded, and says whether
  the pane's mode was the reason, instead of counting a key it silently
  dropped.
- **Alt chords that dropped their escape prefix now match xterm.** Mapping the
  full modifier matrix while fixing the Command-key encoding turned up four
  more gates that forgot a modifier. `Alt+Escape` sent a bare `ESC`, making it
  indistinguishable from plain Escape; `Alt+Tab` sent a bare tab;
  `Ctrl+Alt+Backspace` sent `BS` with the Alt prefix dropped; and
  `Ctrl+Alt+Space` sent `ESC SP` instead of `ESC NUL`, dropping Control. All
  four now carry both modifiers. `Ctrl+Shift+Tab` and `Alt+Shift+Tab` also
  reach `modifyOtherKeys` instead of collapsing to `CSI Z`, so a TUI can tell
  them apart from plain Shift+Tab — which itself still sends `CSI Z` at every
  level, as before. `Shift+Numpad5` under DECKPAM is deliberately unchanged:
  xterm gates keypad modifier reporting behind resources Kettle does not model,
  so there is no unambiguous encoding to adopt.
- **The close confirmation could not be read, so it could not be answered.**
  The bar paints the theme's red `palette[1]`, but drew its prompt, buttons and
  focus marker in the ordinary theme foreground — a color chosen to contrast
  with the theme *background*. On the shipped TokyoNight Night default that was
  light lavender on light red, about 1.6:1. Kettle now lifts the bar's text to
  WCAG AA against its own background using the same contrast helper the
  completion panel uses, and a test holds the floor across every bundled theme.
  The key help also advertises `y`/`n` when `vim-menu-nav` is on, because those
  answer the question directly while Enter fires the focused button — which is
  the safe `Cancel` on every close prompt.
- **Command chords typed their own letter into every modal text field.**
  `winit` does not filter `KeyEvent::text` by Command, so on macOS `⌘V` arrived
  as the text `"v"` — and the title editors, the command palette, the SSH
  launcher, the layout picker and the Settings path prompt all appended it
  blindly. Pressing paste while renaming a tab produced a tab named `v`; `⌘A`
  produced an `a`. In the command palette it was worse than cosmetic: the stray
  character rewrote the query and re-ranked the list, so the next Enter ran a
  command the user never picked. These fields now share one text-entry rule with
  the confirm dialog, which had always applied it. Option is deliberately still
  allowed through, because on macOS it composes accented characters.
- **AltGr characters were swallowed in the modal fields.** The first cut of the
  shared rule rejected Control outright, but Windows reports AltGr as Ctrl+Alt —
  so `@` on a German layout, `ł` on Polish and `€` on many others silently did
  nothing in exactly the fields where there is no other way to type them.
  Control on its own is still a shortcut modifier; Control **with** Alt is
  treated as composition, and a genuine Ctrl+Alt chord is still not text entry
  because it produces none.
- **An IME commit could be discarded by whatever modifier happened to be held.**
  Committed compositions are not keystrokes — the input method decides when to
  commit, and the modifier latched at that instant did not produce the text.
  Committing a CJK phrase with `⌘Space` dropped the whole phrase. IME text now
  bypasses the modifier rule while keeping the control-character filter.
- **The title editor's IME path had no length bound.** It appended directly to
  the buffer, so an IME commit bypassed the 4 KiB cap the typed path enforces.
  Both paths now share it.
- **A pasted bidi override could reverse the OS titlebar.** Every other route to
  the window title ran `sanitize_title`, which neutralizes the Cf bidi
  characters that `char::is_control` does not catch; the user-set window title
  did not, and the new paste arm made a clipboard payload one keystroke from the
  titlebar and the Alt-Tab switcher. Clipboards are writable by terminal
  programs through OSC 52. The edit buffer keeps what was typed; only what
  reaches the window manager is sanitized.
- **Search had the same bug for every chord it does not claim.** Its catch-all
  filtered control characters but not modifiers, so while `⌘A`/`⌘C`/`⌘X`/`⌘V`
  were handled explicitly, any *other* Command chord typed its letter into the
  query — `⌘Q` added a `q`. It now shares the same rule as its siblings.
- **Paste now works in those fields.** It never had: `⌘V`/`Ctrl+V` was the
  literal-letter bug above, so there was no way to paste a path or a branch name
  into a rename box at all. Every one of them now honours the platform paste
  chord.
- **Backspace no longer breaks emoji and accents apart.** These fields deleted
  with `String::pop`, which removes one `char` rather than one grapheme cluster.
  One press on `👩‍🚀` removed the rocket and left a dangling zero-width joiner in
  the title — `U+200D` is a format character, so no control-character filter
  caught it. Deletion is grapheme-correct now, matching the search bar, which
  always was.
- **Modal text fields are bounded.** They accepted unbounded input; a probe put
  3000 characters into a tab title and the tab bar tried to lay all of them out.
  They now share the search bar's 4 KiB ceiling.

## [3.1.1] — 2026-08-18

### Changed

- **Native window material is now the cross-platform default.** New macOS and
  Windows windows use 86% background opacity with native blur. Linux and other
  targets default to 99% opacity; supported Linux compositors also receive a
  blur hint, while unsupported sessions retain the existing 99% live-opacity
  safety floor for explicit lower values.
  This changes the previous `1.0` opacity and disabled-blur defaults for users
  who have not set either option. Preferences now adjusts opacity in one-point
  steps, so the 99% default remains reachable after another value is selected.
  Set `background-opacity = 1.0` and `window-blur = false` to restore a fully
  opaque window.

## [3.1.0] — 2026-08-17

### Added

- **Shell completions can appear in an IDE style card above the active
  command.** Fish and PowerShell publish their shell completion results
  automatically when Tab has not been customized. Bash and Zsh can use the
  cooperative bridge. Customized Fish and PowerShell bindings stay unchanged
  and do not publish a detached list. The shell supplies matching and quoting;
  the bundled adapters preserve its insertion and cycling semantics.
  Kettle prefers a lane above the active command and flips the card below the
  final wrapped row when the upper lane cannot fit the requested page. Both
  placements stay inside the active pane's grid and outside the command and tab
  bar. The card aligns with the start of editable input rather than the pane
  edge, with a source/count header, roomier rows, selection lookahead, a scroll
  indicator, and path-tail preservation.
  Protocol v4 carries the original bounded Fish or PowerShell replacement
  token for visual emphasis and the bounded current-line prefix needed for
  alignment only; v1 through v3 remain compatible. Fish 4's
  duplicate same-row prompt mark no longer invalidates the managed session
  between `sync` and the first result, which had suppressed Fish's pager and
  then silently rejected Kettle's replacement card.
  Fish's stock Ctrl-L remains untouched, and a redraw that moves the prompt
  without starting a command now preserves the current managed session while
  allowing shells that publish a fresh sync to replace it. Narrow 20-column
  panes remain eligible, and a clamped card no longer paints an orphan divider
  after its description lane disappears. Path-like labels and pane titles keep
  the separator before the leaf when shortened, avoiding forms such as
  `…lib.rs`.
  Automatic bindings stay off inside tmux/screen. Mode changes apply to new
  shells so a live wrapper never loses its visible UI.
- **Application windows can use native background material behind translucent
  terminal content.** Set `background-opacity` below `1.0` and
  `window-blur = true`.
  macOS follows Reduce Transparency changes immediately, Windows requests its
  system backdrop, and Linux enables blur only when Wayland advertises KWin's
  blur protocol. Unsupported Linux sessions clamp the live scene to 99%
  opacity so text remains readable without changing screenshots or saved
  config.

### Fixed

- **Unsupported Linux blur could remain as transparent as the configured
  background.** The fallback clear was 99% opaque, but the pane-base pass used
  replace blending and wrote the configured alpha over it. Live pane bases now
  receive the same floor while screenshots retain the requested alpha.

- **Native titlebars could expose the desktop as a fully clear strip.** macOS
  now keeps material below an opaque AppKit caption that follows the selected
  light or dark appearance. Windows sets a palette-matched DWM caption and
  contrast-checked text color. Renderer and pointer geometry remain below
  native controls.
- **The first PowerShell completion card could stay hidden after a focus
  change.** The next Tab now reconciles an in-flight initial sync with managed
  key counts while rejecting older pre-focus replies. A bare explicit
  `pwsh[.exe]` or `powershell[.exe]` also receives automatic integration;
  commands with arguments and wrappers remain literal. DEC focus reports no
  longer consume the first managed completion request, and a transient startup
  resize now rebases a single-row active prompt instead of discarding the row
  anchor the detached card needs. Prompt-input marks hide the previous card
  without retiring a completion response still crossing the PTY.
- **The application icon had competing rounded shapes and stale theme colors.**
  Every platform now uses the two-stroke `>_` identity. macOS gets adaptive,
  exactly inverted light/dark Icon Composer artwork with a parallel inset;
  Windows and X11 update the live icon when Kettle's theme changes.
- **Completion replies could outlive the command that requested them.** Any
  unrelated input now disarms publication before it reaches the PTY. Malformed
  private metadata has bounded recovery, unsafe rows no longer hide safe
  siblings, and logs/output hooks receive one byte-exact filtered message per
  PTY read instead of one message per parser chunk. Managed Fish and PowerShell
  adapters identify the prompt session and every Tab request on shows, updates,
  and clears. Requests advance atomically with PTY queue admission and retain
  individual key boundaries in remote batches. The active shell keymap declares
  which Tab directions Kettle owns, counters fail closed before losing integer
  precision, and prompt metadata already queued at focus loss stays quarantined.
  Delayed output, custom bindings, focus loss, and backpressure therefore cannot
  desynchronize the shell and terminal. Legacy
  cooperative publishers remain quarantined until the next prompt after that
  boundary.
- **Transparent startup used the wrong window-surface test.** Window creation
  now uses effective composed alpha and wallpaper state. Settings name the
  new-window requirement, and failed config writes no longer claim a restart
  will apply a change that was never saved.
- **Completion cards could show stale or mismatched rows.** Retained text now
  invalidates when labels change. Both adapters retain a count- and byte-bounded
  prefix and page it through detached messages. Fish inserts a captured
  singleton without a second provider query, so a changing result cannot open
  the stock inline pager. Fish's escaped provider form now preserves native
  replacement and quoting semantics, including expandable `~user` results;
  ordinary unique results still receive their trailing space, while native
  no-space continuations remain open. Leading-dash candidates such as `--help`
  are treated as completion data rather than abbreviation or `string` options.
  A page that reaches the private-message byte cap is re-anchored at its
  selected row, and omitted unsafe labels keep their structural position, so
  the card cannot drift from the candidate Fish inserted. An
  ambiguous first Tab does not edit a common prefix into the command line. A
  PowerShell provider error also clears the detached state instead of reviving
  PSReadLine's inline list. PowerShell candidates with multiline tooltips keep
  their safe labels and selection while the unsafe optional description is
  discarded; previously the card could highlight a different command from the
  one PSReadLine inserted.
  Unsafe optional v4 token/prefix hints now degrade without hiding a safe page.
  PowerShell rejects oversized input prefixes and replacement tokens before
  grapheme indexing, so cycling cannot repeatedly scan a pasted multi-megabyte
  editor token merely to produce a bounded visual hint.
  A remote text-and-Tab batch also avoids a stale pre-typing cursor anchor.
  The cursor and prefix that position the card now remain one stable pair when
  Ctrl-L, focus loss, a shell clear, a pointer dismissal, or the grace timeout
  hides the list. Reopening the same completion cycle therefore stays aligned
  to the original editable command instead of jumping to a cursor moved by the
  selected candidate; actual input or a new prefix still replaces the pair.
  Bash preserves complete UTF-8 labels, and structurally valid Kitty Tab forms
  refresh the card. The terminal parser and
  raw-output filter now resume public output at the same bounded recovery
  boundary for malformed or oversized private metadata, including after a
  stray escape.
- **Starfield windows unnecessarily requested transparent surfaces on every
  platform.** `background-type = starfield` now stays opaque regardless of
  `background-opacity` and `background-darkness`, because its shader covers the
  full surface. Other backgrounds still select transparency from their
  effective composed alpha.
- **macOS native material could disagree with the renderer's alpha policy.** It
  now follows the same effective-surface decision and updates when that decision
  changes.
- **Completion cards let pointer presses reach obscured terminal content.** A
  press on the card now dismisses it without selecting hidden text, opening a
  hidden link, or forwarding a mouse report to the foreground program.

- **`Alt+Up` could not reach terminal applications on Linux and Windows.** The
  default chord always ran Kettle's `FocusUp` action, even at the top edge where
  it could not move anywhere, colliding with Codex's previous-message editor
  and similar CLI bindings. The four default `Alt+Arrow` focus chords are now
  adaptive: they move to a real neighboring pane and otherwise pass the
  original key event through to the terminal. A zoom that actually hides
  sibling panes keeps the chord as a no-op; a one-pane tab passes it through
  even if its persisted zoom bit remains set. Custom actions and macOS's
  `Ctrl+Cmd+Arrow` focus map keep their explicit behavior, and key-release
  ownership follows whichever side received the last repeated press.
- **Shift+Enter could print `;2;13~` after leaving an interactive CLI.** Kettle
  sent its compatibility encoding before any keyboard-protocol negotiation even
  at an ordinary shell prompt. Some line editors consumed the escape prefix as
  a function key and inserted the remaining bytes literally. The new default
  `modify-other-keys = auto` enables that fallback only for a known agent
  composer (Codex, Claude Code, Gemini, or OpenCode). Unix/macOS pairs a fresh
  noncanonical PTY sample and foreground process-group id with either the
  direct launch identity or the background process snapshot; Windows requires
  the observed composer to be running, or to have been launched directly.
  Nested shells, readline/libedit programs, SSH/WSL transports, wrappers, and
  snapshots without a recognized composer fail closed to plain Enter. `always`
  (`enter` alias) is the explicit escape hatch for an unrecognized client, and
  negotiated xterm/Kitty modes still take precedence. GUI, control-plane, and
  broadcast input use the same per-pane decision.
- **The app icon used two competing rounded shapes on macOS.** The inner dark
  terminal face and the system-owned outer mask could not stay optically
  parallel at every Dock scale, and a literal teapot replacement became noisy
  at taskbar sizes. Every platform now shares one font-independent `>(_)~`
  terminal-kettle mark at normal sizes. The fixed-size Linux and Windows assets
  plus the retained compatibility iconset use a crisp `>_` optical-size variant
  at 16 px instead of fusing five strokes into two blobs; the native macOS
  vector retains the full mark in every rendition. True Bézier parentheses and
  steam replace the bulbous segmented approximation; the raised square-ended
  underscore avoids
  closing the mark into a horseshoe, and every stroke is opaque so no character
  looks disabled at taskbar size. The generator defines and tests an exact
  two-color inverse for design review; the shipped native macOS document uses
  one shared dark appearance. macOS has no inner face or border, leaving the
  system as the sole owner of the curve; the compiled foreground occupies a
  little over half of the icon so it remains legible without crowding the
  native mask.

## [3.0.1] — 2026-08-13

### Fixed

- **The macOS Dock icon's dark terminal face crowded the system-owned edge.**
  At normal Dock size the blue frame collapsed to a roughly three-pixel
  hairline, and native lighting made the inner and outer corner curves appear
  to converge. The prompt and caret retain their 200% layer scale, while the
  face now uses a wider optical safe area and a matching concentric radius; the
  caret is also narrower, adding right-side breathing room so the wider frame
  does not simply relocate the pinch to the terminal mark. The blue frame stays
  even at both normal and magnified sizes.

## [3.0.0] — 2026-08-13

- **The macOS signing keychain was imported correctly but remained unusable by
  `codesign`.** `--keychain` narrows identity matching; it does not make an
  off-list keychain usable. Packaging now prepends the ephemeral keychain to
  the user search list while preserving existing certificate-chain sources,
  then removes it during cleanup. Native Security.framework APIs preserve
  every existing filename losslessly, and the cleanup fails if the search-list
  entry or private-key keychain survives.

- **The macOS release host could not compile its native AppIcon.** Normal CI
  compiled the Icon Composer package on macOS 26, but the macOS 15 release host
  defaulted to Xcode 16.4: `actool` returned success while emitting neither
  `Assets.car` nor `AppIcon.icns`. Selecting its installed Xcode 26.3 exposed a
  second incompatibility when the Asset Catalog agent crashed against the older
  host frameworks. Packaging now runs on macOS 26, pins the compiler to Xcode
  26.x so a future preview cannot silently change release assets, and exercises
  that exact release-host path in a focused pull-request gate.
- **A Linux PTY lifecycle test could claim its detached fixture was ready before
  the child had started.** The parent reported `$!` after a timed sleep, so a
  delayed `setsid` launch occasionally produced orderly EOF where the test
  required a forced timeout. The detached child now reports its own PID after
  installing its HUP policy, signals readiness to the parent, and proves that
  process still owns the PTY slave through `/proc/<pid>/fd/1` before checking
  the timeout path.
- **The first signed macOS package imported its Developer ID identity, then
  failed to select it.** The workflow maintained the certificate and its
  display name as separate secrets, so the name could drift while the PKCS#12
  remained valid; `codesign` then reported only `no identity found`. Packaging
  now derives the signing hash from the one valid `Developer ID Application`
  identity actually imported into the ephemeral keychain and fails closed on
  zero or multiple matches.

### Added

- **Official macOS packages are now Developer ID signed, notarized, stapled,
  and Gatekeeper-assessed before their release ZIP is created.** The release
  runner imports a Kettle-specific certificate into an ephemeral keychain and
  authenticates notarization with a least-privilege App Store Connect API key;
  an official tag fails closed when any credential is absent, the signature's
  Team ID is wrong, notarization is rejected, or the ticket cannot be stapled.
  Pull requests remain unsigned, and all credential material is removed from
  the runner after packaging.

### Fixed

- **The focused-pane accent was visibly cut off at both lower macOS window
  corners.** Four square border strips reached AppKit's rounded native mask and
  were clipped independently, so the color disappeared through each curve.
  Decorated macOS windows now render a single antialiased pane outline whose
  selected outer corners follow the native radius; split-internal corners,
  fullscreen/borderless windows, Linux, and Windows retain square dividers.
  The trailing new-tab dropdown and `+` also gained independent mouse-hover
  surfaces, pointer affordances, a subtle separator, optically centered glyphs,
  and a persistent two-pixel accent cap on the outside edge of `+`. Vertical
  tab bars now use their full column for pointer dispatch instead of only the
  first row, and each row clips its `+`/close glyph to its own button, so the
  newly visible affordances remain clickable and do not disappear below row
  one.

- **The Dock and Finder could show different Kettle silhouettes on current
  macOS.** The bundle now compiles one native Icon Composer document into both
  `Assets.car` and its macOS 11 fallback instead of replacing AppKit's icon at
  runtime. The blue canvas and outer rounding are owned by macOS exactly once,
  while the terminal face, prompt and caret remain Kettle artwork; this removes
  the clipped-color/double-mask treatment at the rounded edge and keeps Finder,
  the running and closed Dock item, and the app switcher on one asset.

- **A new dependency-unsoundness warning had no fail-closed scope guard.**
  RUSTSEC-2026-0253 affects `LruCache::pop()` in `lru 0.16.4`. Kettle reaches
  that crate only through glyphon 0.12.0, whose `Copy` cache key cannot panic
  on drop and which never calls the affected method, so replacing the renderer
  with a fork would increase risk without removing a reachable bug. CI now
  pins the reviewed crates.io sources, workspace membership, unique package
  identities, upstream versions, and every reverse edge in
  locked, unfiltered all-platform Cargo metadata; even a source replacement, a
  Windows- or macOS-only new consumer, an uncommitted resolution, or an upstream
  version change fails until the reachability decision and temporary audit
  exception are revisited. Issue #207 tracks removal once
  glyphon accepts `lru >=0.18.2`.

- **An explicit live screenshot timed out whenever macOS marked the window
  occluded.** Kettle queued the capture and requested a redraw, but the normal
  background-window power guard discarded that frame; even bypassing the guard
  cannot work because Metal refuses to vend a drawable for an occluded
  `NSWindow`. An explicit capture now renders the completed terminal scene into
  a bounded transient target before swapchain acquisition and reads that target
  back, so the request neither depends on a drawable nor wakes ordinary
  background paints. Both the target and its separately allocated staging
  buffer are reserved before encoding and charged to the process GPU budget
  until submission completion or device loss, preserving the 256 MiB
  per-allocation capture ceiling for 6K/8K
  windows without weakening the 64 MiB untrusted-image cap. Control targets
  known to be hidden or minimized fail promptly; backends such as Wayland that
  cannot report those states retain the bounded timeout. Timeout and final file
  publication now share one atomic decision. PNG bytes stream into a randomly
  named owner-only sibling and the potentially slow inode flush remains
  cancellable. The requested path appears only through an atomic no-replace
  link, with a no-replace rename fallback for filesystems without hard links,
  so neither timeout nor a crash exposes a changing partial leaf. The control
  reply has a finite post-commit grace period and reports an explicitly
  uncertain destination rather than waiting forever if the filesystem stalls;
  post-publication durability, verification, and cleanup failures carry that
  same destination-may-exist state instead of masquerading as safe retries.
  GPU resources and accounting are released as soon as readback reaches CPU
  memory, and one process-wide fixed two-worker persistence pool prevents one cancelled,
  filesystem-blocked save from retaining capture admission indefinitely.
  A submission still pending after two bounded waits resets the wedged GPU,
  while genuine driver-loss callbacks also wake an occluded event loop into
  normal device recovery. Publication and setup failures explicitly
  remove the exact staging object and include any cleanup error in the result.

- **The multi-window close regression smoke mistook winit's internal Win32
  message target for a second Kettle window.** Windows exposes that visible
  16x16 event-loop helper through `EnumWindows`, even though it owns no user
  surface. The native inventory now excludes the helper by its framework class
  name rather than a fragile size or title heuristic, while continuing to count
  every real Kettle top-level window. The scenario also submits its typed
  `exit` through the terminal-mode-aware Enter path, because a literal line feed
  does not execute a PowerShell command under ConPTY.

- **macOS rounded off Kettle's color twice.** On macOS 26+, Kettle now gives
  AppKit an opaque, full-bleed runtime icon so the system owns the only outer
  mask instead of pinching an already-rounded blue border. The static `.icns`
  remains pre-rounded for macOS 11–15, whose Dock does not supply Tahoe's new
  treatment. A decorated Kettle window also ended its terminal theme at an
  opaque titlebar seam. Its native titlebar material is now transparent and the
  underlying NSWindow background tracks the exact Kettle theme color. The
  content view stays below the native controls, so traffic lights, terminal
  geometry, and pointer hit-testing remain native and unobscured.

- **The macOS Agent/TUI live smoke could fail after every product check had
  passed.** Its LazyVCS probe embedded a Vimscript `.` concatenation in Lua,
  then tried to write its success marker into the plugin's non-modifiable
  sidebar buffer; fixing either alone only advanced the failure from E5107 to
  E21. The marker is now assembled with Lua byte strings, inserted under a
  temporary buffer-option toggle, and kept conditional on the disposable
  repository's exact canonical root in active and discovered state, rendered
  sidebar row, and matching file buffer. Gutter and blame are then validated
  from the terminal grid rather than coupled to LazyVCS's private cache and
  extmark namespace internals: the visible evidence requires the gutter beside
  `CHANGED` and `KTLBL` beside the fixture's committed line. The probe dismisses
  Neovim's
  hit-enter pager when an unrelated configured plugin warning covers that
  persistent marker. On native Unix, an isolated, no-site Python shell wrapper records the
  portable-pty session before any payload and before the control server is
  needed, returns the payload's exit code or re-raises its terminating signal
  when the session empties, remains alive only while a same-session background
  job survives, synchronizes the parent-only foreground-process-group handoff
  before the child restores `SIGTTOU`, kills/reaps the child when that handoff
  fails instead of releasing its barrier, distinguishes an inherited tty
  descriptor from a controlling terminal before calling `tcsetpgrp`, and
  accepts only session leaders checked
  as children after a stable handle is retained. Linux retains pidfds and macOS retains audit
  tokens before cleanup freezes each process; neither path carries a reusable
  numeric PID across a check/signal boundary. Every acquired handle enters the
  finalizer-owned set before any duplicate close or first signal, and signal or
  close failures are aggregated only after later handles are processed. An
  exact two-key sandbox-environment sweep reads real NUL-delimited values and
  uses the same stable handles for configured editor daemons that deliberately
  detached from the PTY session; argv decoys cannot be mistaken for them. A
  reported live PTY wrapper whose stable handle cannot be retained now fails
  closed instead of silently looking absent. This covers
  separate foreground groups and background jobs that outlive their shell.
  WSL cleanup records Neovim's PID before config initialization and drains that
  process, then every exact sandbox environment, through pidfds after a
  capability and spawn-during-cleanup self-test. Its narrow PID record is
  opened no-follow and nonblocking, then accepted only as a small, owner-held,
  single-link regular file; FIFO, symlink, and Unix-socket regressions must be
  rejected within a bounded subprocess deadline. Every retained pidfd is
  closed even when one signal fails, unreadable same-user environments fail
  closed, and the sandbox is removed only after a successful quiet scan. The normal
  pane command publishes a release marker without deleting the tree; a
  detached-daemon regression and an explicit ordering guard prove host cleanup
  drains it before removal. On native Windows, the exact PowerShell pane joins
  an unpredictable named kill-on-close Job itself before any sandboxed editor
  starts, avoiding a reusable numeric-PID handoff. The Job now exists before
  the sandbox and cleanup ownership is registered as soon as the tree exists.
  Cleanup terminates the Job,
  waits until Job accounting reports zero active processes, then closes it and
  deletes the tree, so detached editor daemons cannot survive or race removal;
  a native regression proves Windows refuses an actual pre-drain deletion, and
  a junction regression proves cleanup cannot walk out of the sandbox or change
  an external target's permissions.
  Native Linux acquires a child-subreaper scope before the configured editor
  starts, so a detached helper that reparents, hides its environment, or outlives
  Kettle is adopted by the harness instead of disappearing under PID 1. A
  stable-identity baseline distinguishes existing children from adopted ones;
  nested scopes restore process-global state only on the last close, and failed
  restoration remains retryable. Every exact/adopted/descendant handle batch is
  transferred before the first signal, stopped, and walked through a linear
  parent-to-children index under one absolute eight-second deadline. Only
  `ENOENT`/`ESRCH` mean a process disappeared; handle exhaustion and permission
  errors fail closed. Unrelated protected same-user services remain outside the
  adopted tree and no longer disable the smoke. Reparenting, a nondumpable
  descendant, modeled numeric-PID reuse across distinct stable identities, and
  duplicate-close failures are covered. A successful Unix drain hashes and
  removes the snapshot through retained directory descriptors: recursive opens,
  chmods, unlinks, and rmdirs are relative and no-follow, and ancestor-swap
  sabotage cannot redirect them outside the sandbox. The copied
  plugin hash is bounded before it retains a directory's entries, with sentinel
  tests proving iteration stops at the cap; it rejects links and special files,
  hashes typed directory paths so empty-directory changes are visible,
  and starts only after configured Neovim
  finishes any bootstrap, and is tied to the canonical LazyVCS module source
  Neovim actually loaded. Repository provenance now streams the exact
  NUL-delimited status under pathname-byte and record caps, counts indexed
  paths, disables textconv, streams diffs and untracked files under one
  file/byte budget, holds every no-follow directory chain, and rejects traversal
  errors and untracked special files. The complete filesystem pass runs in a
  child process under one parent-enforced 120-second launch-and-run deadline. Unix
  contains ordinary inherited-group workers in a private process group and
  Windows uses a kill-on-close Job Object. Internal Python starts with `-I -S`
  so user site hooks cannot run before assignment; a pre-work handshake then
  prevents children escaping containment, and
  an asynchronous owner reaps a terminated filesystem-stalled worker without
  extending the caller's deadline. A blocked-worker/child regression asserts
  actual reap success rather than only reaper-thread completion. User-configured
  Git fsmonitor processes are disabled. A silent, pipe-free Unix group member
  remains until controller cleanup, so reaping the worker cannot make its PGID
  reusable before every completed result kills that group and removes any
  ordinary outliving helper. This is
  cleanup rather than a POSIX sandbox: a deliberately detached `setsid` process
  is outside that group. A Windows close-only case requires both the
  worker and its child to be recorded before proving the Job limit itself kills
  the tree; unexpected communication errors take the same cleanup path. The
  portable helper self-test compiles both the fully assembled WSL cleanup
  preflight and the generated child it embeds. The visible
  LazyVCS proof uses a per-run fixture token and one cell-proven divider column
  to associate sidebar evidence with the left split and changed/blame rows with
  the tracked-file split; the exact validated cell snapshot is retained.
  Native Windows containment is scoped to the configured-editor phase; Unix
  additionally inventories and freezes every PTY session during whole-window
  failure cleanup. A startup identity uncertainty or interrupt now crosses the
  same cleanup boundary as a timeout. The driver creates and retains the
  owner-only tracker file before launch; wrappers append through a no-follow
  descriptor, then restore the real `SHELL` and remove the tracker-control
  variables before the pane payload starts. Reads stay on that retained file,
  reject path replacement, deduplicate records, and enforce byte, record, PID,
  and absolute-time bounds. A malformed record is reported only after later
  bounded records have been checked and retained, so one corrupt line cannot
  hide a cleanup target. Teardown freezes Kettle's spawning process group,
  drains the tracker again, and independently retains every direct child that
  already escaped into a PTY session, closing the append-after-snapshot race.
  If an append-only tracker PID is reused,
  its replacement is reopened and independently classified in the same pass;
  internal revalidation errors close the complete partial handle batch.
  The complete native macOS run produced and checked all 14 live states,
  including the configured LazyVCS sidebar.

- **Several live-UI smokes could accept a command Kettle had only echoed as if
  the shell had executed it.** Their completion token appeared literally in the
  typed command, so the control-plane wait could return before Enter reached the
  shell; the precision-touchpad gate then inspected an empty history and
  incorrectly reported a product scroll regression. Completion tokens for the
  touchpad, interaction, hovered-pane wheel, notification, cwd/title, LazyVCS
  readiness, and sibling-window checks are now assembled from two shell
  arguments. The literal token exists only in executed output, and portable
  tests reject builders that put it back into command echo.

- **A desktop notification could freeze every Kettle window.** OSC 777, Lua,
  command-completion, and error toasts all called the platform notification
  backend synchronously on winit's UI thread. The stricter interaction smoke
  reproduced a macOS notification call blocking beyond the ten-second event-loop
  watchdog, during which rendering, input, and control replies all stopped. One
  process-wide worker now sends notifications from a bounded 64-message queue.
  Ordering is preserved; a hung OS service fills the queue and drops later
  notifications with one warning instead of hanging the terminal. A portable
  admission test proves full and disconnected queues return immediately. An
  injected backend blocks inside the real worker while admission and bounded
  shutdown remain responsive, and module privacy keeps the OS backend out of
  UI event-loop code. Normal GUI exit
  gives admitted notifications a bounded 250 ms drain; a notification service
  still hung at that boundary may lose queued messages rather than preventing
  Kettle from closing. Update-recovery warnings are queued before bare-launch
  activation and receive that bounded flush before a secondary process exits.

- **A transient release-network failure could make the Linux online installer
  fail even when every signed asset was healthy.** Each bounded fetch now gets
  two curl-classified retries with bounded backoff, including refused
  connections. Kettle suppresses user curl configuration before applying that
  policy, so a local `retry-all-errors` cannot turn a permanent HTTP or
  size-limit refusal into more requests. A modern release still fails closed if
  its signed manifest cannot be authenticated after the third total attempt.

- **Kettle could not create a GPU device on Parallels' otherwise usable
  virtual adapter.** The GLES device advertises graphics and presentation but
  zero compute workgroups; Kettle requested WebGPU's default 65,535 even though
  it has no compute pipelines, so Windows 11 ARM renderer tests and an Ubuntu
  ARM live launch failed before drawing. Every live, screenshot, offscreen, and
  test device request now keeps the adapter's full 2D surface limit while
  clamping all other defaults to its real capabilities. Native Windows ARM64
  source builds are now documented and exercised separately from the shipped
  x86_64 artifact.

- **The `kettle-update` test binary could not run as a standard Windows
  user.** Its generated `kettle_update-<hash>.exe` had no execution manifest,
  so Windows installer detection inferred from the name that it required
  elevation and Cargo stopped the workspace suite with error 740. Windows test
  harnesses for that crate now carry an explicit `asInvoker` manifest; native
  Windows ARM64 development no longer requires elevating the build. Its test
  build exercises the shipped x86_64 package contract while production ARM
  builds truthfully retain an unsupported managed updater, because no ARM
  archive is published.

- **Parallel Windows update tests intermittently lost cleanup races to a file
  scanner.** A newly written journal or backup could be opened briefly without
  delete sharing; Kettle treated the resulting error 32 as permanent and left
  a committed transaction unconfirmed. Windows update-owned opens now retry
  only sharing/lock violations for a bounded 250 ms, then perform the same
  handle-based identity and content validation. Other I/O failures remain
  immediate.

- **Wheel input in a split window followed keyboard focus instead of the
  pointer.** Scrolling over an unfocused pane changed the focused pane's
  scrollback, sent mouse-tracking reports to the wrong TUI, or synthesized
  alternate-screen cursor keys in the wrong shell. Terminal wheel behavior now
  targets the pane under the pointer without changing keyboard focus; chrome
  and gaps retain the focused-pane fallback.

- **Selection drags could freeze at the outer window edge or jump to another
  split.** Some window backends stop sending coordinates after `CursorLeft`, so
  dragging beyond the top or bottom stopped scrollback autoscroll until the
  pointer moved again. The exit edge is now latched and drives the existing
  frame timer. The gesture also pins its starting pane, so ctl/Lua focus changes
  cannot redirect extension, autoscroll, or copy-on-release to a sibling.

- **A close requested for one in-process window could be consumed by another
  window's dispatch epilogue.** The pending close was one app-global Boolean;
  cross-window control dispatch already needed a special case to avoid closing
  the focused window instead of its target. Requests are now keyed by window
  id and consumed only by that window. A focused live regression now terminates
  one detached window's child through the PTY reap path, requires the mapped
  window count to fall, rejects geometry for the removed id, and proves its sibling
  still accepts terminal input. This removes a Kettle-side path by which one
  exiting CLI pane could make separate terminal windows disappear; the child
  program itself (shell, Codex, or another TUI) is not causal.

- **`kettle exec` could report exit 0 before Unix PTY output arrived.** The
  lifecycle correctly distinguished an empty raw-output channel from a
  disconnected one, but then overrode that proof after 810 ms on every
  platform. Under whole-workspace scheduling, the direct child could exit
  before the PTY reader ran; Kettle then closed stdout and the recorder while
  the bytes were still in flight, producing empty stdout or a header-only
  asciicast with the child's successful status. Windows ConPTY, whose output
  handle can legitimately outlive its final repaint, now uses quiet only as
  permission to close the pseudoconsole asynchronously; success still waits for
  the resulting channel disconnection and orderly reader EOF. Unix requires the
  same proof without the close step. Direct-child exit is observed with
  `waitid(WNOWAIT)`, independently of PTY EOF, so an inherited slave starts the
  five-second no-EOF bound instead of hanging forever while Linux retains the
  unreaped identity needed for later teardown. Unexpected read errors are
  latched immediately but bytes already admitted to the parser and stdout
  worker drain before status 125 is returned. Queued output or a backpressured
  stdout worker remains delivery work and is not timed out; if the parser
  itself disappears with a pending count no surviving thread can retire,
  Kettle fails explicitly instead of waiting forever. Reader status,
  source-generation, and pending-chunk count now share one atomic snapshot, so
  a pump read and parser completion cannot manufacture a falsely quiet state
  while bytes are hidden in the bounded pipeline. An explicit
  operation deadline now returns 124 even when the direct child already exited
  0, because that status cannot make abandoned lossless output complete. The
  final completion check resamples source progress after draining every queue;
  because the reader has disconnected by then, nothing can race in afterward.
  At construction, the Unix pump now retains Kettle's slave descriptor through
  the first successful master read. Linux waits on master readability
  plus a pidfd, macOS uses one kqueue for `EVFILT_READ` plus `NOTE_EXIT`, and
  other Unix targets use an exponentially backed-off `waitid(WNOWAIT)` fallback;
  a quiet long-running pane therefore adds no periodic polling on the primary
  platforms. The UI also reaps a pane only after consuming the reader's ordered
  `Exit` event; the earlier `ChildExit(status)` notification is advisory and
  cannot close or restart a pane ahead of final PTY output and `exit-action`
  handling. Held panes continue collecting a direct-child status that was not
  ready at PTY EOF, so Hold cannot leave a zombie until the pane is dismissed.
  The child observer remains active after startup: a daemonized Unix
  descendant retaining the slave now gets a five-second output-drain window
  rather than parking GUI exit policy forever. Windows waits on a duplicated
  child handle only for interactive panes. Its semantic wake bypasses paint
  generation even while hidden; the event loop starts asynchronous ConPTY close
  at that bound, starts the second bound only after the close worker exists, and
  retries worker-start failure instead of applying Hold to a live master.
  Linux teardown tracks the PTY-created session rather than treating possession
  of its slave descriptor as process ownership. It first stops and revalidates
  the original leader through its pidfd; that unreaped identity keeps the
  numeric session and process-group ids reserved while every discovered member
  is frozen and killed through its own pidfd. Numeric targeting is refused when
  that anchor cannot be proven, so PID reuse cannot redirect a signal to an
  unrelated process. Cleanup waits until retained members are actually stopped
  before its final scan and bounds/reports the complete procfs scan. If procfs
  or pidfds are unavailable, it fails closed rather than signaling a numeric
  scope it cannot authenticate; timeout/cancellation reports 125 instead of
  claiming a completed 124/130 teardown. Windows creates the command suspended,
  assigns a shared kill-on-close Job Object, and resumes only after assignment
  succeeds, closing the old post-spawn window in which an immediate descendant
  could escape timeout. Failed assignment/resume now proves rollback completed,
  cloned killers cannot silently lose Job containment, and a timed-out
  asynchronous `ClosePseudoConsole` remains responsible for stopping its reader
  only after the real close returns. Ordinary quiet close also waits until the
  Job Object has no live descendant, so closing ConPTY cannot erase a late
  inherited write while returning the direct parent's success. Windows process
  liveness now comes from the process handle rather than the ambiguous numeric
  `STILL_ACTIVE` value, preserving a legitimate child exit code of 259.
  A later macOS CI run exposed an earlier construction race the lifecycle proof
  could not cover: a short command was spawned before the master reader and its
  pump existed, and both a normal and diagnostic retry exited 0 with empty
  output. A forced two-second delay in that old construction window reproduced
  the same double-empty failure deterministically. Kettle now establishes the
  PTY pump before allowing the child to start. On Unix it also retains its own
  slave descriptor in that pump until the first bytes are read, or until a
  genuinely silent direct child exits; a readiness signal alone still had a
  scheduling gap, and the same forced delay proved that first version could
  lose all output too. (#201)

- **An explicit `kettle ctl screenshot --json '{"path":"PATH"}'` was refused unless every parent
  directory was private.** A user-selected export beneath an ordinary `0755`
  or `0775` project/diagnostics directory was routed through the private-state
  writer, so the live-UI smoke could drive a real window and read its screen but
  could not save the evidence. Explicit screenshot paths now require an
  already-existing parent and create a new owner-only leaf atomically without
  following or overwriting one. The selected parent is pinned and revalidated
  during creation; macOS secures the file in an ACL-free same-filesystem
  staging directory before atomic no-replace publication, and Windows rejects
  alternate streams, trailing-dot/space aliases, embedded NULs, and parent
  replacement while applying a protected current-user DACL. Default screenshot
  locations retain the stricter private-state ancestor policy. An encode or
  flush failure removes the exact partial leaf when it still occupies the
  selected name, reports cleanup failure, and never deletes a replacement.
