## [2.56.0] — 2026-08-09

### Fixed

- **Implicit configuration could be replaced by another local principal.** The
  bounded reader rejected symlink leaves and special files, but it did not keep
  a trusted directory chain held through the open. Default and named-profile
  configs now validate the requested and resolved target directories plus the
  leaf's mutation permissions, ownership and link count on Unix and Windows. A
  dotfile-manager symlink is followed only after the link object itself is
  proven user/root-owned and single-linked, so narrowing an old `0775` config
  directory cannot bless a link another group member planted before repair;
  macOS extended ACLs and Windows generic access masks are included rather than
  trusting mode/specific-right bits alone. Live reload uses the same policy and
  retains the last known-good settings on refusal. Automatically discovered
  `init.lua` now carries the same provenance because even safe-mode Lua can type
  commands into the shell; both Lua paths also read once through a held handle
  under the 4 MiB cap. `--config FILE` and `--lua-script FILE` remain explicit
  trust grants for project/shared files. Parent guards retain one immediate
  directory capability and revalidate ancestor identities on demand, avoiding
  path-depth-scaled descriptor pressure. (#188)

- **Live config reload could be silently off for a whole session.** A watcher
  whose registration failed was stored anyway, so kettle held a handle that
  watched nothing and reported nothing — indistinguishable from a config nobody
  was editing. The handle is now stored only on a successful subscription, and
  the two ways it can fail both say so: a rejected registration, and a watcher
  the platform declines to construct at all (an exhausted inotify budget), which
  was dropped by an `if let Ok` with no `else`. The remote-command watcher had
  the same silent constructor and got the same line. (#187)
- **Crash diagnostics were refused on machines with a group-writable cache
  directory.** `<cache>/kettle` left at 0775 by an earlier run under a 002 umask
  was never narrowed — missing ancestors are created at 0700, but an existing
  directory is not touched — and the private-path verifier then refused every
  write beneath it, including the incident report. The cache root is now a
  recognized base and the diagnostic writer asks for the repair. Found by
  sweeping a real machine, where `~/.cache/kettle` sat at 0775 over 0600
  content. (#187)
- **The activation tests leaked a scratch directory per run.** The helper built
  a path from the pid and deleted whatever the *previous* run with that pid had
  left, never its own; a sweep of a Windows machine found 148 `kettle*` entries
  in `%TEMP%`. The directory is now owned by a guard that removes it on drop.
  On Windows the guard is not enough on its own and the first version of this
  entry overstated the fix: the activation server thread owns the election lock
  for the process lifetime by design and opens it without delete sharing, so
  `remove_dir_all` is refused and `TempDir::drop` discards the error — the leak
  would have moved to `%LOCALAPPDATA%` rather than stopped. The test server now
  has a stop/wake/join guard that closes the listener and election lock before
  the scratch guard drops on every platform; the age-gated sweep remains only
  for abrupt test-process termination and historical leftovers. Test-only
  either way — no shipped code path is affected. (#187, #188)

## [2.55.0] — 2026-08-09

### Fixed

- **The confirm bar clipped its own rightmost button.** Every close-confirm,
  quit-confirm and keybind-reassign prompt rendered `[  Clos…` instead of
  `[  Close]`, at every window size on every platform. The bar composed itself to
  one column wider than the budget it was then fitted to, so the overflow was
  unconditional and the fitter dropped two columns for an ellipsis. The click
  target had the matching error, leaving the last painted column dead and the
  column past the bar live. (#164)

- **Title editing showed no input under a vertical tab bar.** With
  `tab-bar-position = left` or `right`, editing a pane, tab or group title
  rendered zero input columns — no text, no caret — while Enter still committed
  the invisible buffer. For a broadcast group that buffer names the group, so it
  decided which panes received later keystrokes. The overlay had been handed the
  tab *segment's* rect (`tab-bar-width`, 180px ≈ 21 columns) while the line it
  composes ends in a 30-column hint. (#165)

- **Opening or closing a title edit left the PTYs at the wrong size.** The chrome
  strip that appears for the modal changes the content rectangle, and nothing
  resized the panes across either transition, so the grid the child believed it
  had stayed wrong until an unrelated event corrected it. Resizes are now
  coalesced to one per frame, so a queued pair of actions no longer sends two
  `SIGWINCH`s for a net change of zero. (#168, #170)

- **Icon rasters, release documentation, and Intel-Mac Nix.** The 18 tracked
  PNG/ICO rasters had no gate at all and could drift from the generator silently;
  they are now compared by decoded pixels and metadata on every push.
  `docs/RELEASING.md` documents the two-PR flow and the tag-signature requirement
  that fails a release closed. (#167)

- **A confirmed paste, and a committed title edit, went wherever focus had
  drifted to.** Both prompts recorded only *that* they were open, not what they
  were opened over, so a click, a Lua hook or a remote command that moved focus
  while the prompt was up sent the paste — or renamed the pane, tab or broadcast
  group — somewhere the user never pointed at. `ClosePane` had pinned its target
  since it was written; these two now do the same, and a target that dies while
  the prompt is open dismisses it instead of applying to whatever inherited the
  focus. (#172, #175)

- **kettle created the directory its own security check then rejected.** The
  private-directory paths ran `create_dir_all` on their parent chain, which takes
  the ambient umask, and named `0700` only on the leaf. On a `002` umask —
  Debian and Ubuntu's per-user-group default — `$XDG_RUNTIME_DIR/kettle` and
  `~/.config/kettle` landed at `0775`, and because the private-path verifier
  walks *ancestors*, every private path beneath them was refused. On Ubuntu 24.04
  that silently disabled single-instance activation, the remote-command watcher
  and the update-check throttle, each reporting only a warning in a log. Every
  kettle-named directory now gets an explicit `0700`, an existing one this user
  owns is repaired through an `O_NOFOLLOW` descriptor rather than a path, and
  the refusal message names the `chmod` that restores it. Eight sites in all,
  and the ones that mattered were not in the obvious list: the config-reload
  watcher creates the config directory *first*, so it decided the mode every
  later check was judged against, and the control socket binds before the
  registry is repaired, so an agent-enabled launch could bind under a
  group-writable parent. (#180)

- **kettle could narrow a source checkout to `0700`.** A directory counted as
  kettle's own if its *name* matched, so `~/Repos/kettle` — anybody's checkout —
  qualified, and `kettle --config ~/Repos/kettle/dev.config` set the whole tree
  to `0700`. It now also has to sit where kettle actually puts its namespace: an
  XDG base, `~/.config`, `~/.local/state`, or the temp root. The same mistake
  made the refusal message advise `chmod 700` on directories kettle does not
  own, including a user's entire `~/Repos`; that advice is now offered only for
  kettle's own directories, while the refusal and its diagnostic are unchanged.
  (#181)

- **`--list-actions` hid 27 aliases the parser accepts, while the documentation
  promised it could not.** `page_up`, `move_pane_left`, `bell_off` and 24 more
  worked in a config but were absent from the list users are told is complete,
  and `switch_to_tab_N` appeared in no output at all. The drift guard behind that
  promise pinned two names by hand; it now derives the whole set from the
  parser's own arms. (#177)

### Performance

- **Every visible pane walked its entire grid, every frame, to discover it had
  no kitty images.** `placeholder_tiles` scanned the grid under the terminal lock
  before checking whether any virtual placement existed — 127 µs per pane per
  frame on a 300x80 grid, held against the mutex the PTY reader blocks on. It now
  checks first and scans only when there is something to draw. (#176)

### Added

- **`kettle mcp` answers modern MCP clients, not just handshake-based ones.**
  MCP 2026-07-28 removed the `initialize` handshake from the protocol *core*,
  so a client on that revision sends none and carries its version and
  capabilities in every request. Against a handshake-only server that does not
  degrade — the specification's own compatibility matrix scores it *Fails*, and
  in practice every call returned "server is not initialized". kettle now
  serves both eras on the same stdio process, implements `server/discover`, and
  refuses a version it does not speak by naming the ones it does. (#183)

### Changed

- Four CI gates that existed but were dispatched by nothing now run on every
  push: the tracked-file audit (the only enforcement of encoding, CRLF, PNG-CRC
  and Markdown-link integrity across 925 files), the agent-CLI smoke, the macOS
  comparator scorer self-test, and an executing `kettle.fish` fixture. (#166)

- Removed `scripts/check-underline-scroll-smoke.sh`, superseded by the
  cross-platform live-UI driver its recipe already dispatched. (#171)

- Every `mermaid` diagram in tracked Markdown is compiled in CI. One in
  `docs/ARCHITECTURE.md` had never rendered — a node label with escaped quotes,
  showing as a red error panel on GitHub — and nothing caught it because
  nothing looked. Four diagrams were added where prose was carrying more
  structure than prose can: the release flow, the update verification chain,
  the update transaction journal, and the OSC 133 command lifecycle. (#182)

- `docs/AUDIT-DEFERRED.md` records why the eleven live-UI scenarios still run in
  no automated gate, with the evidence that the recorded reason — a GPU and
  interactive desktop — is wrong for Linux: one scenario was run headlessly under
  Xvfb with software Vulkan, and the bug it found is the umask fix above. (#179)

## [2.54.0] — 2026-08-08

### Fixed

Fifty-eight findings from a full-repo audit. Five adversarial audits covered
every crate; a separate six-surface pass covered the scripts, CI, packaging,
shell-integration, documentation, and UI/UX-design surfaces those crate audits do
not reach; and three successive reviews of the integrated change set found
further defects — including three in the fixes themselves, which is why the
review loop ran until a pass came back clean rather than stopping at the first
one. Each finding was adversarially re-verified before it earned a fix, and each
fix carries a regression test that was confirmed to fail without it. Full ledger
in `docs/AUDIT-2026-08-07-FULL.md`.

- **A killed Linux update could not be recovered by its own recovery code.**
  Provenance was verified before any path could recover a transaction, so a crash
  between publishing a file and writing the record left the journal and backups
  intact but unreachable — the installation was stranded until manual repair.
  Structural discovery is now separated from provenance verification. Backups
  also became visible before their journal entries, which recovery correctly
  refused to act on; a durable intent record now precedes the backup.

- **Shell-integration snippets corrupted the prompts they hook.** The zsh snippet
  built `PS1` with `$(...)`, which requires `PROMPT_SUBST` — off by default — so
  on stock zsh the prompt rendered 25 literal characters and never emitted OSC
  133;B, with ZLE column arithmetic wrong for the whole line. It also defined
  `precmd`/`preexec` outright, destroying hand-rolled hooks. The PowerShell
  snippet read `$LASTEXITCODE` and wrote to the console before invoking the
  user's prompt, destroying `$?` — under starship every failed command rendered a
  success prompt — and replaced any existing Enter binding. The bash OSC 7
  encoder emitted 16-hex-digit escapes on bash 3.2, which is what macOS ships.

- **PowerShell reported a successful exit code for failed commands.** Fixing the
  `$?` defect above moved the `$LASTEXITCODE` read *after* the user's prompt
  rendered — and starship, oh-my-posh and posh-git all shell out while rendering,
  with every native call overwriting it. A command that failed with 37 followed
  by a prompt that ran anything successfully emitted `OSC 133;D;0`, so command
  notifications, `command_finished`, and ctl/MCP `run_command` reported success
  for a failed command. Both indicators are now captured in one statement, before
  anything else runs, and `$LASTEXITCODE` is restored afterwards so the prompt's
  own native calls do not leak into the next command's view of it.

- **Drift guards across ten files could not fail.** Tests asserting on
  `include_str!` of their own file also searched their own assertions, so the
  needle was always present and the guard passed whether or not the production
  code it named still existed. Reintroducing the exact defects two of them
  describe left both passing. Every such guard now slices the test module off
  first, reusing the `production_source()` helper already written for `mux.rs`
  and `app.rs`; no file in the workspace reads its own source unsliced any more.

  Doing so exposed seven stale expectations in five render guards that had been
  masking real refactors — none needed a production change, and each was
  re-checked against current source rather than relaxed to match. One guard's
  name was corrected too: it asserted the tab title lane while calling itself
  `..._full_segment_rect_budget`.

  The sweep took three passes, and the reasons are worth recording because each
  one hid behind the previous fix. The first scan only recognised
  `src.contains("literal")`, missing guards that put their needles in an array
  and pass them as a variable. The second left three hand-rolled strippers in
  place, two of which were unsound — one halted at a `}` inside a multiline
  string and copied the test module back into its own "production" slice; another
  missed an indented `#[cfg(test)]`. Both passed their self-check, because that
  check only looked for the helper's own name.

  There is now **one** lexer-based implementation in `kettle-test-support`. It
  brace-matches while tracking line comments, nested block comments, escaped and
  raw strings, byte strings and char literals, and it declines to strip a `cfg`
  whose "test" is part of a longer word. Sixteen unit tests cover the hazards
  individually, and every wrapper now asserts its slice contains no `#[test]` and
  no `#[cfg(test)]`, not merely that it lost the helper.

  Negative guards (`!src.contains(...)`) get particular care: they fail *open*
  when the slice loses production code, so the result was checked by probing real
  production lines rather than by trusting the stripper.

- **Pastes could be corrupted by terminal replies.** Priority replies were
  selected at every 8 KiB chunk boundary, so a mouse report could land between
  the bracketed-paste markers and be read as pasted content. PTY writes are now
  message-atomic.

- **Broadcast mangled keys and lost panes.** Keys were encoded once for the
  focused pane's mode and the bytes fanned out, so a legacy pane sharing a group
  with a Kitty pane received `ESC [ 97 u` instead of `a`; releases never crossed
  windows, leaving remote TUIs holding a stuck key; dropped files reached only
  the source window; and paths were formatted for the source pane's shell, so a
  WSL pane received a Windows path.

- **macOS translucency composited twice.** Metal's `PostMultiplied` surface
  received a premultiplied scene, so the compositor multiplied it again and
  darkened every translucent edge. Alpha-mode selection also ignored
  `background-darkness` and never recomputed on reload. `background-darkness` had
  no effect over a wallpaper, per-pane OSC 11 backdrops compounded window
  opacity, and `minimum-contrast` measured against the wrong background — white
  text on a white selection satisfied a 4.5 requirement at an actual 1:1.

- **Glyphs could go permanently blank.** Atlas-full misses were cached as
  whitespace and eviction never reclaimed pixels, so once the atlas filled the
  affected glyphs stayed blank until a font change or restart.

- **Three parsers disagreed about where a control sequence ends.** The exec
  stripper ignored CAN/SUB outside one state, so `\033[31\030hello` printed
  `ello`, and MCP capture reads that same path. The session-log scrubber lacked
  escape-intermediate and ESC-from-CSI transitions, writing OSC payloads — which
  can carry a window title — into a supposedly plain-text log, and leaked parser
  state across logging sessions. Bounded VT recovery could split a UTF-8 scalar
  and emit orphaned continuation bytes.

- **Kettle reversed OpenSSH's option precedence.** Real `ssh` keeps the first
  value, so `ssh -l bob alice@h` connects as `bob`; Kettle's title and Reconnect
  used `alice`, authenticating as a different user. Verified against `ssh -G`.

- **Three CI gates could not turn red.** The mutable-action-reference
  supply-chain check grepped with a tool absent from the runner image and had
  never executed; the daily audit cron filed an issue instead of failing; and a
  failing `cargo test -p portable-pty` was masked by a trailing command in the
  same step. Each is now demonstrated to fail on a real violation.

- **Settings could silently discard a configured value.** Number rows snapped an
  out-of-range value into the row's own range on the first arrow press. A rebind
  that could not be written reported nothing.

- **`run_command` reported complete output that was not.** A 10,000-line cap
  dropped the head of the capture while `output_truncated` stayed `false`, so
  agents read partial output believing it whole.

- Tests across the workspace created "private" scratch directories without making
  them private, so the suite failed on any host with a permissive umask
  (`umask 002`, a common Ubuntu default) — 23 failures in one crate alone.
  Verified under both `umask 002` and `umask 022`.

### Added

- **macOS performance comparator** (`scripts/perf/macos-compare.sh`,
  `just macos-perf`), the leg Windows and Linux already had. Ranks Kettle against
  installed macOS terminals on startup, ASCII flood, ANSI/underline flood, max
  RSS, and idle CPU. A metric counts only when Kettle and at least one real
  competitor were both measured, so the harness cannot certify a standing it did
  not measure; undriveable peers and unmeasured metrics are explicit skips.

- **Shell-integration fixtures that run the real interpreters**
  (`just shell-integration-check`), because the defects above all survived tests
  that only inspected snippet text. zsh runs under `zsh -f` with no options set,
  and the bash encoder is exercised under macOS `/bin/bash` 3.2 specifically — a
  bash 5 test passes while the shipped path is broken.

## [2.53.0] — 2026-08-07

### Added

- **Native macOS keyboard defaults and configurable Option behavior.** Cmd
  shortcuts now supplement the existing portable Ctrl+Shift map for copy,
  paste, tabs, windows, search, clearing the scrollback, the command palette,
  Settings, font size, prompt jumps, and tab selection. `Cmd+W` closes the tab
  and `Shift+Cmd+D` the split, matching Apple Terminal rather than kettle's
  portable meaning, and `Cmd+K` clears the scrollback as it does in Terminal
  and iTerm2. Bare Option+arrows now reach the PTY so word-wise motion works,
  with directional pane focus on Ctrl+Cmd+arrows.

  `macos-option-as-alt = none|left|right|both` chooses which Option side, if
  any, produces terminal Alt/Meta. The default `none` matches macOS itself.
  Previously Option was unconditionally Meta: winit reports `ALT` whether or
  not Option is acting as Alt — it only swaps the event's characters — so every
  Option chord emitted `ESC` followed by the composed character, and typing
  `å`, `é` or `—` was impossible. `Alt+1..9` never selected a tab either, since
  Option+1 is `¡`.

  `Cmd+G` and `Cmd+Shift+G` are now free for the system's Find Next/Previous.
  They previously toggled broadcast-to-all-panes, where mistaking one for Find
  duplicated every later keystroke into every pane; broadcast moved to
  Ctrl+Cmd+B.

### Fixed

- **Package extraction accepts macOS temporary paths.** `/var/folders/...` is
  the per-user `$TMPDIR` on macOS and was rejected by the extraction-root
  check, and a pre-planted extraction root is now refused outright. Shipped in
  `a4aab83` with no changelog entry of its own.
- `event-listener` moved past RUSTSEC-2026-0221 (the advisory affects 5.4.1;
  the lock is on 5.4.2). The only path into the graph was Linux-only, through
  `accesskit_unix` and `notify-rust` → `zbus`.

- **Control discovery validates its completed Unix-socket fallback.** A long
  `TMPDIR` could turn the supposedly short fallback into another path beyond
  the BSD/macOS `sun_path` limit. The check is also applied after the lossy
  UTF-8 conversion the endpoint goes through, since each invalid byte expands
  to a three-byte replacement character.
- **Control sockets refuse a directory this user does not own.** The endpoint
  directory is predictable from the uid, so another local user could pre-create
  it and then remove or replace the socket inside. Creation now verifies a real
  directory — not a symlink — that is either owned by the effective uid or is a
  root-owned sticky shared root such as `/tmp`, where the sticky bit already
  prevents one user unlinking another's entries. Anything else fails closed.
  The mode is tightened to `0700` best-effort rather than required, because a
  legitimate shared temp root cannot always be chmod'd: macOS's per-user
  `$TMPDIR` returns `EPERM`.
- **Large high-DPI windows keep their real dimensions.** The renderer requests
  the adapter's full 2D texture limit instead of wgpu's default 8192, so a
  window spanning several 4K displays no longer has its right or bottom edge
  left without a matching surface. It is still clamped to what the device can
  actually present: configuring past that limit fails validation outright and
  leaves a surface that paints nothing, which is worse than clipping.

## [2.52.0] — 2026-08-06

  ### Changed
  - **Kettle reaches the screen first now.** It used to be last: a window
    appeared after ~1068 ms against Alacritty's 502 ms and WezTerm's 696 ms, and
    the reason was not extra work. Renderer init is ~700 ms of that and the
    window was HIDDEN for all of it — because a window shown before its first
    painted frame shows the window class's stock WHITE brush, and most of a
    second of white is a worse greeting than a late window.

    The window is now revealed immediately, but only where the pre-paint frame
    cannot be told from what follows. Setting the class background brush to the
    terminal's own colour was tried first and does NOT decide what appears —
    with a light background configured the pre-paint window still sampled
    `0,0,0`, because winit owns `WM_ERASEBKGND`. Trusting it would have handed
    light-theme users a black flash instead of a white one.

    So the reveal rests on arithmetic instead: an unpainted window is black, so
    reveal early only when the configured background is within 24 levels of
    black on every channel. `#101010` against black is four levels on one
    channel. Anything with visible colour, light or dark, keeps the previous
    hide-until-painted behaviour exactly, and Windows only — that black-window
    premise was measured on Windows and is not portable.

    Measured, warmed, quiet machine, time until the window is visible:

    | | Alacritty | WezTerm | Kettle |
    |---|---|---|---|
    | before | 502 ms | 696 ms | 1068 ms |
    | after | 370-408 ms | 507-525 ms | **205-227 ms** |

  ### Added
  - **Renderer and startup phase timings** under `RUST_LOG=info`: adapter,
    device, everything-after-device, `FontSystem::new()` and the bundled-font
    load separately, plus window create, accessibility, GPU and the reveal. Two
    findings recorded so they are not re-derived: `FontSystem::new()` is ~46 ms
    and not the problem even with `font-family = Cascadia Mono` forcing a
    system-font lookup, and the first launch after a driver shader-cache reset
    measures the post-device phase at 4218 ms against 165 ms warm — always
    discard it.

## [2.51.0] — 2026-08-05

  ### Added
  - **Drag a terminal to another position in its tab, with the mouse.** v2.50.0
    landed the tree surgery and said plainly that the gesture was missing; this
    finishes it. Press a pane's own titlebar and move past a slop radius and the
    pane is picked up; the pane under the cursor shows, washed in accent, the
    half of itself the drop would take; releasing puts it there. Esc abandons the
    drag, and so does anything that takes the pointer away — a modal opening, the
    window losing focus.

    Which half is decided by splitting the target pane along its diagonals, so
    every point inside it means exactly one edge. The obvious alternative — a
    band along each edge — leaves a dead middle where a drop means nothing, and
    on a narrow pane the bands overlap and the middle disappears entirely.

    The press had to become ambiguous for this to work. A titlebar click already
    meant "focus this pane", and clicking the focused pane meant "rename it", so
    the press now only focuses and arms; the *release* picks between renaming and
    moving. Opening the title editor on press, as it did before, put a text field
    over the pane the user had just picked up.

  ### Fixed
  - **The benchmark harness refused every terminal list spelled on the command
    line.** `pwsh -File perf-all.ps1 -Terminals a,b,c` hands the whole
    comma-joined text over as ONE literal argument — `-File` does not parse array
    syntax — and the resulting one-element list failed several hundred lines
    later, inside the schedule generator, complaining that a schedule needs at
    least two terminals. Nothing in that message points at how the list was
    spelled. The parameter now takes the same `ValidateSet` the terminal
    resolver uses, so an unusable name is rejected at the boundary.
  - **Two benchmark probes asserted a window reached the foreground on the very
    next instruction.** `SetForegroundWindow` is documented to be refusable, and
    even when granted the switch is not observable immediately — so a single
    call followed by an instant `GetForegroundWindow` comparison is a race, and
    it loses whenever another application happens to own the foreground. The
    startup and throughput probes did exactly that and aborted the whole run.
    That two sibling probes had already met this and bolted on flat 3 s and
    500 ms sleeps is what says it is a race rather than a real refusal. Those
    two now use a bounded acquire-and-confirm helper that re-issues the call and
    polls, so the usual case costs one poll rather than a fixed wait; a window
    that genuinely never reaches the foreground still fails the sample. The
    latency and menu-hover sleeps stay as they are — they are not waiting to
    acquire the foreground but letting the window settle before input is
    injected into it, which the helper does not replace.
  - **Startup readiness spent its deadline scanning pixels in PowerShell, then
    blamed the terminal.** The readiness poll walks the captured region looking
    for the marker colour. A match stops early; a MISS cannot — and a miss is
    exactly what every poll before the window paints is. Interpreted, that walk
    over a 1024x384 region costs **2,585 ms** measured on this machine, so a 30 s
    deadline bought about eight looks rather than hundreds, and a terminal that
    painted a little late was reported as one that never painted at all. The walk
    now runs in the harness's compiled helper (`CountPixelsNearColor`): **89.8 ms**
    for the same worst case, a 29x cut, with identical tolerance semantics. The
    deadline now measures the terminal instead of the harness. The timeout
    message also reports the slowest capture and how much of the deadline went
    on capturing, so "the terminal was slow" and "the poll was slow" stop looking
    the same from the outside.
  - **A smoke run could name a schedule nobody walked.** The probes choose
    between the Williams square and the position-only rotation from one
    predicate; the manifest preview that records the choice still called Williams
    unconditionally. A five-terminal smoke run would therefore have written
    `williams-*` into the manifest while every probe walked a rotation — the
    reader could not tell. Both now apply the same predicate.

  ### Changed
  - **The benchmark can now run on a machine somebody else is using.** A launch
    is refused when another instance of that terminal is already up, which is
    right for a terminal that joins a running instance — no new window appears
    and the launch would otherwise die as an unexplained timeout. It was applied
    by process NAME to every terminal, including the ones whose pinned launch
    arguments force a fresh process, so an unrelated session's window made the
    whole suite unrunnable. `-AllowForeignTerminalInstances` (smoke only;
    release refuses it) lifts the refusal for exactly those, decided by reading
    the pinned arguments rather than a hardcoded list. Attribution never
    depended on the name — the measured window is found by diffing the window
    set, its owner's SHA-256 must match the launched executable, and CPU and
    memory walk that process tree — but contention is real, so the manifest
    records `foreign_terminal_instances` (names and PIDs, never titles or
    command lines) whether or not the switch is set.
  - **`-AllowUnbalanced` runs the comparators a machine can actually offer.**
    Release evidence needs a Williams square over an even set of at least six
    terminals, and a machine that cannot spare six could previously measure
    nothing at all. The switch drops to a rotation that balances *position* —
    each terminal starts in each slot once — and states in the results that
    predecessors are not balanced, which is the property the square adds and the
    reason `-Mode release` still refuses the switch outright.

## [2.50.0] — 2026-08-05

  ### Added
  - **Move a pane to another position in its tab.** Terminator rearranges panes
    by dragging a terminal onto another one; kettle could only rotate or
    equalize a split tree, never rearrange it. `move_split:{up,down,left,right}`
    lifts the focused pane out — collapsing whatever split it leaves behind,
    exactly as closing it would — and puts it beside its neighbour in that
    direction, on that side. The neighbour is found by the same search
    `goto_split` navigates with, so the pane lands where you were looking rather
    than by a second, subtly different rule.

    The mouse gesture itself is not implemented: it needs drop-target
    hit-testing and a drag preview, which is interaction design rather than tree
    surgery, and `docs/TERMINATOR-AUDIT.md` says so rather than implying the row
    is closed.

  ### Fixed
  - **The benchmark harness could not run a comparator session, and it was not
    the terminal's fault.** A run aborted at Rio's startup readiness reporting
    `paint=False`, which reads as "the terminal never painted". Rio paints
    perfectly well — it renders a truecolor background a few levels off the
    colour it is handed (`48,89,94` comes back as `59,89,94`), and the readiness
    check demanded a byte-exact match. Alacritty happens to be exact, which is
    why this went unnoticed. The deviation is worst in the dark range, the shape
    of a linear-space blend, so the check now allows a bounded per-channel
    tolerance instead of assuming every renderer round-trips sRGB.

    Behind it sat a second, unrelated defect: the run-local WezTerm config set a
    field that WezTerm 20240203 rejects, and an unknown field makes WezTerm open
    a configuration-error window *alongside* the terminal — so the launcher saw
    two windows and refused the whole run as an ambiguous launch.

    The readiness timeout now reports the client and region geometry, how many
    captures it took, and whether the last one came back empty or merely lacked
    the marker. That message is what identified the real cause after three wrong
    theories, so it is the part most worth keeping.

## [2.49.0] — 2026-08-05

  ### Fixed
  - **A broadcast group stopped at the window edge.** `group_all` already put
    panes in *every* window into one group, matching Terminator's process-wide
    terminal collection — but the broadcast itself only reached the window you
    were typing in. So grouping panes across two windows and then typing hit
    half of them, with nothing on screen to explain it: the other window's panes
    still wore the group name in their titlebars.

    A named group now reaches every window in the process, for typing and for
    pasting alike — a paste is user input under the same scope as a keystroke,
    and fixing only the typing path would have left a broadcast that types to
    the whole group and pastes to half of it. The per-pane bracketed-paste
    decision is shared between the two paths rather than duplicated, so they
    cannot disagree about how to wrap the same payload in different windows.

    Scopes defined by something window-local do not travel: `tab` is a focused
    tab, which exists in exactly one window, and `all` remains kettle's own
    window-wide scope. A second kettle *process* is still its own broadcast
    domain, which is not a divergence — Terminator is single-process.

    The paste-protection prompt widened with it. It fires when a multi-line
    paste can reach a pane with no bracketed-paste mode, because a newline runs
    the line there — and it now asks every window the broadcast reaches, not
    just the focused one. A group member sitting at a shell prompt in a second
    window is exactly the case that prompt exists for.

  - **`kettle exec` could lose the command's output entirely.** The lifecycle
    loop treated an empty raw PTY channel as proof the output had all been
    read. It is not: the reader thread owns the only sender and drops it after
    EOF, so a *disconnected* channel is proof, while an empty one is equally
    consistent with the reader not having been scheduled yet. For a command
    that writes a little and exits at once, the exit could be observed and the
    settle window elapse while the bytes were still in flight; the recorder was
    then finished and stdout closed, and they arrived with nowhere to go.

    One bug behind two failures that looked unrelated, because a single gate
    feeds both stdout and the recorder: a command reporting success with no
    output, and `--record` writing a structurally valid trace containing only
    its header. The second is the worse one — a recording missing everything it
    was asked to capture gives no sign that anything went wrong.

## [2.48.0] — 2026-08-05

  Findings from an audit of `kettle-ui` — the largest crate, and the one
  holding the UI/UX states and the AstroNvim / tmux / agent-CLI input surface —
  the last of the render residuals from the 2.47.0 review, and a sweep through
  the Terminator features that were present in name only: an action that did
  something other than what it was called, keys that parsed and were never
  read, a documented sandbox level that did not hold, and two settings that
  changed nothing until you restarted.

  ### Added
  - **`lua-sandbox = restricted`**, a third trust level below `safe`:
    `kettle.send_text` and `kettle.exec_action` refuse, so a plugin can observe,
    notify and restyle but cannot type into your shell or dispatch actions. See
    the `Fixed` entry below for why `safe` was never that level.

  ### Performance
  - **The starfield evaluated its whole model once per star per pixel.** The
    hash, the angle, the radial ease, the colour lookup and the sRGB decode all
    lived inside the fragment shader's per-star loop, and none of them depend on
    the pixel being shaded — at 4K that was roughly 456 million star-iterations
    a frame, about ten transcendentals apiece, recomputing 55 stars' worth of
    values over and over.

    Everything pixel-independent is hoisted out. The angle, phase and colour are
    fixed for the life of the field and are computed once at startup; the radial
    position, radii and brightness change with time and resolution and are
    computed once per frame for 55 stars, then uploaded. What is left in the
    shader is the part that genuinely varies per pixel: the distance to each
    star and the two falloff terms, one `exp` instead of ten transcendentals.
    Stars too dim to see are dropped before upload, so they no longer cost a
    `continue` on every pixel.

    The brightness curve is production code now rather than a copy kept in the
    test module, so the tests that pin the visual contract drive the function
    that actually runs. A GPU test renders a real frame and checks the stars
    land where the CPU placed them, which is the only thing that can catch a
    layout disagreement between the Rust uniform struct and the hand-written
    WGSL — verified by swapping the struct's two members and watching it go red
    while every CPU test stayed green.

  ### Fixed
  - **`equalize-splits` stopped equalizing once a tab held more than twenty
    panes.** Every ratio in the split tree was clamped into a fixed
    `[0.05, 0.95]` band at each of the six places a ratio was read or written.
    A balanced chain of N panes on one axis needs ratios of `1/N`, so past
    twenty the value it wanted could not be represented: 23 panes came out
    7,7,7,6,6,…, and 28 panes ran 95px down to 63px — the widest pane half
    again as wide as the narrowest, after asking for them to be equal.

    The same band made keyboard resize run backwards on a crowded tab. A pane
    already below the floor that was asked to get *smaller* landed under 0.05
    and was clamped back up, so it grew. And it silently rewrote layouts on
    restore: the session file's ratios went through the same clamp, so an
    equalized many-pane workspace came back unequal.

    The band is gone. What keeps a pane usable is now a floor measured in
    pixels against the space actually available, applied in the one place that
    turns a ratio into geometry — so it binds only when a pane would really
    become too small to read or to grab by its divider, and a divider dragged
    to the edge now stops the same physical distance from it whatever the
    window size, instead of reserving 95px on a wide monitor and almost
    nothing on a narrow one.

  - **Two tests could fail without anything being wrong.** The MCP stall
    detector's "a busy writer is not a stalled peer" fixture published progress
    from a helper thread every 10 ms against a 50 ms budget — five missed
    wake-ups of headroom, which a loaded CI runner spends routinely, so the
    test failed on macOS having proven nothing about the code. Progress now
    comes from inside the detector's own poll, which tests the same property
    without asking the scheduler for a favour.

    And the performance harness's Windows PowerShell 5.1 run died on a parse
    error, because a file with no BOM is read as the ANSI code page there: an
    em dash decodes to three characters, the last of which PowerShell accepts
    as a closing quote, so a dash inside a string ended it and the parser
    failed somewhere else entirely. Every harness script is ASCII now, and a
    new self-test refuses any byte above `0x7F` — including a check that the
    check itself still fires.

  - **A session file replaced underneath kettle could be mistaken for its
    own.** The write-skip that keeps a keybound action from costing tens of
    milliseconds of durability syscalls remembered what this process last wrote
    and confirmed it against the file's *size* — so a session replaced with
    different contents of the same length read as unchanged and was never
    corrected. Two kettle windows share `session.json` and a hand-edited file is
    a supported thing to do, so that is a state you can actually be in. The
    check now compares the file's bytes, which costs microseconds against the
    write it decides whether to spend, and needs no remembered state at all.

  - **The keybind-conflict question could not be answered.** Rebinding a key
    onto a chord that is already taken raises a confirmation — from inside the
    Settings overlay, which is where the keybind editor lives. But the Settings
    arms claimed the keyboard first in both key routers, so every key, `y` and
    `n` included, went to the panel and none reached the dialog. The panel's
    dim backdrop covered the bar as well, which is what made it look like a
    stacking bug rather than a dead one.

    A modal question now outranks every overlay, including the one that asked
    it, and the backdrop stops above the bar instead of greying out the thing
    being asked.

  - **A Lua callback stuck in a loop froze the terminal on every event, not
    once.** The instruction-budget watchdog aborted a runaway, which is the
    right first move, but then left the callback registered. The budget is
    deliberately enormous — around 128 million instructions — so exhausting it
    costs real wall time, and an `output` callback that never finishes is
    re-entered on every chunk of PTY output. Each chunk paid the stall again.
    That is not a terminal with a broken plugin in it; it is a terminal that
    has stopped working.

    A callback that burns the whole budget is now retired for the session and
    the user is told, once, through the ordinary notification path — a plugin
    silently ceasing to work is its own kind of bug. Wrapping the runaway in a
    `pcall` does not buy it a reprieve: the watchdog records the abort somewhere
    Lua cannot reach, so swallowing the error changes nothing. Each callback
    also gets its own budget rather than sharing one per event, so a heavy but
    honest callback can no longer starve the ones registered after it, or take
    the blame for them.

  - **Drag-to-reorder was dead on a vertical tab bar.** With
    `tab-position = left` or `right` the segments stack down a shared column,
    and the drag handler tested the cursor's **x** — which every segment
    contains. The first one always matched, so dragging any tab moved tab 0, or
    did nothing at all when tab 0 was the one being dragged. The ghost had the
    same problem from the other side: it was pinned to the top of the strip and
    slid sideways out of the bar as the cursor moved.

    Both now follow the axis the strip is actually stacked on, read back off
    the rendered segments so the drag cannot disagree with what is on screen.
    The control plane reports `drag_cursor_y` alongside the existing
    `drag_cursor_x`, so a live-UI check can see the vertical ghost.

  - **Changing the scrollback budget did nothing until you restarted.** Both
    `scrollback` and `scrollback-bytes` were read once, when a pane spawned.
    Editing them — in the config file, or through the Settings overlay's two
    scrollback rows — wrote the value, reloaded the config, and left every open
    pane on its old cap, so the setting looked broken rather than deferred.

    A reload now carries the budget into the panes that are already running. A
    decrease is honoured, which is the opposite of what a *resize* is allowed
    to do: a resize must never lower the cap, because nothing about dragging a
    window wider means you want less history, whereas typing a smaller number
    into the setting means precisely that.

  - **`lua-sandbox = safe` did not mean what it read as, and there was no
    level that did.** Safe mode nils `os.execute`, `io.popen`, `io.open` and
    the rest, which reads as "a safe-mode plugin cannot run programs". It can:
    `kettle.send_text` types into the focused shell and a newline submits the
    line — that is the documented plugin API, and the shipped example clears
    the screen exactly that way. So the two statements in the documentation
    contradicted each other, and a user picking `safe` before running someone
    else's plugin was misled about what they were getting.

    Rather than break the API, kettle now says so plainly and adds the level
    that was missing. `lua-sandbox = restricted` refuses `send_text` and
    `exec_action` — the two calls that drive the terminal — while hooks,
    queries, notifications and theme changes keep working, so a plugin you
    have not read can still be useful without being able to type for you.
    `safe` stays the default and stays honest about being a guard against a
    careless plugin rather than a container for a hostile one; `SECURITY.md`
    now scopes reports accordingly.

  - **Four Terminator config keys parsed and did nothing.** `broadcast-default`,
    `split-to-group`, `autoclean-groups` and `always-split-with-profile` were
    accepted, validated, stored — and never read. Setting them changed nothing,
    and `--check-config` reported them as fine. They are wired now:

    - `broadcast-default` picks the scope the broadcast chord turns on:
      `group` (the default) is the active tab, exactly what the chord has
      always done; `all` is the whole window; `off` means the chord cannot
      enable broadcast at all. Terminator stores this as the *initial* mode
      instead — kettle waits to be asked, because a window that started in
      `all` would mirror every keystroke into every pane before you touched
      anything, which kettle shipped once by accident and had reported as a
      bug. Terminator's own default behaves identically either way.
    - `split-to-group` puts a new split in the broadcast group of the pane it
      came from, instead of silently dropping out of it.
    - `autoclean-groups` drops a broadcast still aimed at a group whose last
      pane has closed. Terminator prunes a group registry; kettle's groups are
      just the names its panes carry, so the thing that outlives its members is
      the scope — which kept the titlebar claiming a group nobody was in, and
      would have swept up the next pane given that name.
    - `always-split-with-profile` makes a split repeat the parent's launch
      command rather than falling back to a shell. It only affects direct
      launches (`kettle -e vim`, an agent CLI); an ordinary shell was always
      cloned.

    The same chord is a toggle now. It used to *set* per-tab broadcast, so the
    key that turned broadcasting on could not turn it off again and you had to
    know a second one. Terminator has that pair too — `group_all` /
    `ungroup_all` — plus a `group_all_toggle` that ships unbound; this is that
    toggle, and the explicit off chord still works.

  - **`rotate_cw` / `rotate_ccw` turned one split, and the two directions did
    not undo each other.** Terminator's rotate turns the visible tab's whole
    layout a quarter turn (`paned.py:rotate_recursive`); kettle flipped the axis
    of the focused pane's parent split alone, swapped its children only when
    rotating clockwise, and never mirrored the ratio. So a rotation moved one
    pair of panes rather than the picture, an uneven split changed its
    proportions on the way round, and clockwise-then-counter-clockwise did not
    come back — it left the two panes swapped.

    Rotation is now what the word means: every split turns, children swap with a
    mirrored ratio exactly where the rectangles demand it, the two directions
    are inverses, and four turns are the identity. The test asserts the pane
    rectangles land where turning the screen would put them, which a
    shape-only check could not have caught. Zoom is dropped first, as Terminator
    does, so the result is visible; and because rotating changes every pane's
    size, the PTYs are now resized and the new arrangement saved — previously
    the layout was redrawn but every child process still believed its old
    geometry.

  - **`move_tab_left` / `move_tab_right` stopped dead at the ends of the tab
    bar.** Terminator's `move_tab` wraps: left from the first tab sends it to
    the end, right from the last brings it back to the front. kettle clamped, so
    the keys silently did nothing on the tab most likely to be moved. They wrap
    now, and reordering by keyboard is saved. The mouse path deliberately still
    clamps — a drag that wrapped would fling the tab across the bar as soon as
    the cursor overshot the last segment.

  - **A workspace could come back with directories the user left an hour ago.**
    `session.json` records each pane's working directory, each split's ratio and
    each tab's title, but only a handful of gestures ever wrote it. A shell
    `cd`, a dragged divider and a renamed tab all changed what the file should
    say without saving it, so what came back depended on whether some *later*
    gesture happened to save.

    A sweep now writes the session when it has fallen behind. It costs nothing
    at rest: it rides turns the event loop was already taking rather than arming
    a timer — waking twice a second would have shown up in the idle-CPU figure
    the perf suite publishes — and the write is skipped entirely when the
    serialized text already matches what is on disk. A window with no tabs is
    never swept, whatever the clock says: a window is empty exactly when it has
    not opened yet or is on its way out, and writing that snapshot would put an
    empty session over the one about to be restored.

  - **`split-auto` always split downward.** The dispatch arm read
    `Action::SplitDown | Action::SplitAuto`, so "auto" was literally "down" —
    on a pane wider than it is tall it stacked instead of splitting side by
    side. Terminator splits along the pane's longer axis, and every
    user-facing description in kettle already said the same: `docs/CONFIG.md`'s
    "pick by aspect ratio", the palette entry, the context-menu row, and the
    default `Ctrl+Shift+A` binding. Only the implementation disagreed. It now
    reads the focused pane's rect and cuts the longer axis, ties going to a
    vertical cut so a square pane behaves as it did before.

  - **Closing a window erased the named layout it was launched from.**
    `close_window` deliberately empties the mux before saving, so the session
    it writes is the empty one — "this window is finished, do not bring it
    back". Routed at a named layout, that intent destroyed the workspace
    instead: a layout measured at 2043 bytes came back as 65
    (`{"tabs":[],"windows":[]}`) after one close, and the next
    `--layout NAME` opened a single default pane. Terminator, whose layouts
    this mirrors, only ever writes one from an explicit Add/Refresh —
    launching a layout never modifies it.

    An empty snapshot can no longer overwrite a named layout. The deliberate
    clear still reaches `session.json`, which is where that intent belongs.
    The routing is one function now with the whole truth table as its test.

  - **Widening a pane permanently destroyed its scrollback.** The
    `scrollback-bytes` budget was turned into a line cap by dividing it by a
    worst-case per-row cost at the pane's *current* width, and any grid change
    — including a width-only one — reassigned it. A wider pane therefore
    produced a smaller cap, and the grid enforces a lowered cap by discarding
    the oldest rows, immediately and with no way back.

    Four ordinary gestures reached it: dragging a window wider, where every
    intermediate width applied its own cap; decrease-font, which fits more
    columns in the same pixels; closing a sibling split so the survivor doubles
    in width; and un-zooming. Measured with the shipped defaults at 28 rows: 77
    columns held 5202 lines, 126 held 3210, 241 held 1681 — and dragging back
    to 77 restored none of them. One font-decrease step cost 997 lines; closing
    one split cost 2013. A long agent-CLI transcript lost most of itself to a
    mouse drag.

    The cap is monotonic for the life of a pane now: it can rise, never fall.
    Nothing about a resize means the user wants less history, so a resize is
    not how a memory budget gets enforced — every other terminal bounds
    scrollback in lines and none of them evicts on widening. The worst case
    becomes the budget measured at the width the history was accumulated at,
    which is bounded and is paid only by someone who actually widened.

  - **Modes 1000 and 1003 both reported the wrong motion.** The gate asked
    "not 1003, and no button held", which let 1000 — defined as press and
    release only — emit drag reports whenever a button happened to be down;
    `vim` with `ttymouse=xterm` enables 1000 alone and hit exactly that. In the
    other direction, both motion call sites only ran with a button held, so
    1003 never delivered the button-less `CSI < 35 ; x ; y M` that is its
    entire purpose, while DECRQM still answered that the mode was set. Hover
    handling in Neovim's `mousemoveevent`, lazygit, btop and fzf was silently
    dead. The rule is stated per mode now, in one function the tests drive.

  - **Every modified Delete lost its modifier, under the shipped default.**
    `delete-binding` defaults to `escape-sequence`, and the remap that
    implements it had no modifier guard, so it rewrote `Ctrl+Delete`,
    `Shift+Delete` and `Alt+Delete` back to the plain `CSI 3 ~`. `Ctrl+Delete`
    was byte-identical to `Delete`: readline's `kill-word` deleted one
    character, and no `<C-Del>` / `<S-Del>` / `<M-Del>` mapping in an editor
    could ever fire. It hit both input planes — real keystrokes and agent
    `send_keys` — and the remap had no test of any kind.

    The binding now replaces only the *unmodified* encoding, and decides that
    by comparing the encoded bytes rather than inspecting modifier state.
    Reasoning from modifiers is what went wrong in the first place: Backspace
    guarded on control and alt, correct for its C0 forms, while Delete's
    `CSI 3 ~` carries a parameter for shift, alt, control and super alike.
    Comparing against the plain form cannot drift from the encoder, and it also
    leaves `modifyOtherKeys` and kitty-protocol encodings alone — an
    application that negotiated a precise encoding should not have it
    overwritten by a legacy remap.

  - **`Ctrl+Alt+<char>` dropped Control, and four of them wrote a bare escape
    introducer into the terminal.** The Meta+Control form was special-cased for
    `C-M-v` alone; every other chord fell through to the printable-Meta path,
    so `C-M-f` ran `forward-word` instead of `forward-sexp` in Emacs and no
    `\e\C-h` readline binding fired. Worse, that path emitted the character
    verbatim after `ESC`, so `Ctrl+Alt+[`, `]`, `_` and `Shift+P` sent a CSI,
    OSC, APC or DCS opener to the shell and the terminal then consumed
    whatever was typed next as sequence parameters.

    The whole C0 table takes the Meta+Control form now. The scoping had been
    justified by AltGr on international layouts, but that hazard cannot reach
    the branch: winit clears CONTROL *and* ALT whenever the layout has AltGr
    and the right Alt is down, and on X11/Wayland AltGr is Mod5, never ALT. A
    character outside the ASCII control table is unaffected either way.

  - **Answering a close confirmation left the surviving panes' PTYs at their
    pre-close size.** Every keybind and menu action ends with a resize; the
    confirm dispatch is a separate entry point and did not. With
    `ask-before-closing` on — which the ✕ and Alt+F4 prompts require — a
    confirmed pane or tab close collapsed the layout and repainted it while the
    shells kept their old rows and columns, so a tmux, vim or agent CLI drew
    into part of its pane with dead space around it. Typing did not heal it;
    only some later unrelated action did. The resize is now the dispatch's
    tail, in one place, so a new arm cannot forget it.

  - **The latency benchmark could never complete a single run.** `latency.ps1`
    called the harness's nearest-rank percentile helper with `90`, `95` and
    `99` while that function declares `[ValidateRange(0.0, 1.0)]`, so
    PowerShell rejected every call at parameter binding — before the body ran
    — with "The 90 argument is greater than the maximum allowed range of 1".
    Every other caller in the harness already passed a fraction; this file was
    the outlier, and it was also the only harness module with no self-test,
    which is why it shipped. It has one now, and the self-test reads
    `latency.ps1`'s own calls and drives each argument through the real
    function rather than restating the arithmetic.

  - **The display-topology probe leaked the monitor's device instance path
    into published results.** A duplicate-identity warning interpolated the
    value — something like `DISPLAY\DEL41A8\5&2b41c7ee&0&UID4353` — into a
    free-text `issues` string, which lands in `benchmark-manifest.json`. The
    sanitizer tokenizes that value everywhere it appears under its own
    `instance_name` property, but `issues` is free text and never matched a
    sensitive property name, so it was published verbatim. The message carries
    the count now, which is the part that makes it actionable, and the
    sanitizer's self-test refuses any issue message that interpolates a
    machine-identifying value.

  - **`kettle.bash` zeroed `$?` for everything chained after it.** The hook
    deliberately runs first in `PROMPT_COMMAND` so its own exit-status read is
    the real one, but it ended on a successful `printf` and never restored the
    status — so any segment after it saw `0`. Anything colouring a prompt by
    exit status, or appending `[$?]`, reported success after a failing command
    purely because kettle's integration was installed. Verified against a real
    bash: `false` now leaves `$?=1` for the next segment where it used to leave
    `0`.

  - **The online installer's Ed25519 verification had no test that could fail.**
    Every signed-path test ran against a stub whose `openssl pkeyutl -verify`
    returned success unconditionally, and no test ever made it fail — so the
    entire verification block could have been deleted, or made to accept a
    forged signature, without anything going red. The manifest is where the
    archive's hash comes from, so that check is the only thing between a user
    and an attacker-supplied hash. There is now a test for the refusal, and it
    was confirmed by disabling the verification and watching the installer
    happily install from an unauthenticated manifest.

  - **Fifty source guards could not fail, and one of them had already rotted.**
    A guard that reads its own file with `include_str!` also reads its own
    assertions, so `src.contains("literal")` matches the needle written one
    line above it and passes whether or not the production code exists. Both
    `app.rs` and `mux.rs` now strip every `#[cfg(test)]` item before searching,
    and every guard was moved onto that. Turning them on immediately caught
    `recorder_output_flushed_before_reap_and_on_close`, which keyed on three
    comment sentences and had been matching its own stale copy of one that was
    reworded — the wiring was fine, the guard was not. It keys on the call
    sites and their enclosing functions now.

  - **Minimizing a window persisted it as a 160×120 stub.** The session
    snapshot read the window's position and size with no minimized check, and
    Win32 answers a minimized window with the `(-32000, -32000)` sentinel and a
    0×0 client rect, which winit passes through verbatim. The restore clamp
    could only rescue that as far as its floor, so the window came back
    unusable. A window that cannot say where it will return to now saves no
    geometry and restores at the default size.

  - **Dragging a tab reordered the other tabs.** `move_active_tab` swapped,
    which is only the same as moving when the distance is exactly one. The drag
    handler passes `target_index - active`, Windows coalesces mouse motion so a
    single event can cross several narrow segments, and an overshoot past the
    edge clamps to the last one — so the tab at the destination teleported back
    to the dragged tab's original slot. It relocates now, sliding whatever it
    passes. The shipped test never asserted the order of the tabs it did not
    drag, so it stayed green under both meanings; it compares the whole bar now.

  - **The session file was rewritten, durably, on every keybound action.**
    `handle_action`'s unconditional tail saves the session, and the save is an
    atomic replace that stages the bytes, `sync_all()`s them, applies the
    Windows DACL, renames, and fsyncs the parent directory — 30–100 ms,
    synchronously, on the event-loop thread. Since most of the ~200 action arms
    fall through to that tail, holding `Ctrl+Shift+Down` to scroll asked for
    tens of blocking disk writes a second and the window stopped responding.

    Serializing costs about a millisecond; it is the durability syscalls that
    cost, and almost none of those actions change the session at all. An
    unchanged session skips the write now. A real change is still written
    immediately and synchronously, so no guarantee moved, and the memo is
    re-checked against the file's size so a session deleted out from under the
    process is rewritten rather than assumed.

  - **Translucent backgrounds composited too bright, and translucent
    screenshots were wrong in both directions.** Closing the last open item
    from the 2.47.0 premultiplied-alpha fix. Every pipeline that draws over the
    frame's clear treats the destination as premultiplied — `quad` and
    `imgpipe` through `PREMULTIPLIED_ALPHA_BLENDING`, `glyphpipe` through
    `ALPHA_BLENDING`, whose `OneMinusSrcAlpha` destination factor is the
    premultiplied "over" operator. The clear is the one write in the frame that
    does not pass through a blend, so it is the only one that has to
    premultiply itself, and it wrote straight colour instead. Measured on the
    GPU: a 50%-alpha black quad over a 50%-alpha white background reads back at
    188 with the straight clear and 156 with the correct one.

    The multiply belongs in linear space, because an sRGB attachment is decoded
    before blending and re-encoded on write. Only `PreMultiplied` surfaces get
    it: `Opaque` discards alpha at composite time, so scaling would only darken
    the surface toward black, and `PostMultiplied` divides it back out.

    The same chain ran into the capture paths, where it was worse: PNG stores
    straight alpha, and both the offscreen `--screenshot` and the live-surface
    `ctl screenshot` saved premultiplied pixels unconverted. Both now convert,
    in the space matching the attachment's format — a plain `Unorm` surface
    blends on the stored bytes, an sRGB one in linear, and applying either
    reciprocal in the other's space leaves the capture visibly off.

## [2.47.0] — 2026-08-03

  Follow-through on the review of the 2.46.0 fixes: the defects that review
  found in them, plus the tests it judged unable to fail.

  Then a review of *those* fixes, which found nine more and is why several
  entries below describe a fix being corrected rather than made. The ones worth
  knowing about: the UNC guard did not close the hole it claimed to (`\??\UNC\`
  has one leading separator, not two, and reaches the same redirector — measured
  at 380 ms against a share versus 0.2 ms locally); the MCP shutdown budget was
  a regression worse than the hang it replaced, discarding the result of any
  tool call outliving 30 seconds; and the marker-version check refused
  `unknown`, which both installers write, reporting those installations as
  unmanaged. Every finding was reproduced before being fixed, and every fix was
  re-checked against the reviewer's own trigger.

  ### Fixed
  - **A Linux self-update disowned files the previous release installed.** The
    provenance record was regenerated from the archive alone, so a file an
    earlier release shipped and this one no longer does stayed on disk with
    nothing recording it — uninstall deletes only what provenance lists, so it
    was installed permanently. `scripts/install-unix.py` seeds the new record
    from the old one; the two writers now go through one function and cannot
    drift apart again.
  - **A rolled-back update left directories behind, unowned.** Directory
    ownership was decided by sampling the filesystem BEFORE the writes. A
    transaction that created a directory and then failed restored the files but
    not the directory, so the retry saw it as pre-existing, left it out of
    provenance, and uninstall could never remove it. The transaction now reports
    the directories it actually created and removes them when it rolls back.
  - **`kettle exec` could emit invalid UTF-8 after a very long control string.**
    The stripper shields UTF-8 continuation bytes so a `0x9c` inside a character
    is not mistaken for the 8-bit string terminator. When the 64-KiB
    resynchronization bound fell on a multi-byte lead byte, the lead was
    swallowed as the string's last byte while its continuations were emitted
    into ordinary output with no lead in front of them — anything decoding
    stdout saw invalid UTF-8 from that point on. A character now goes wherever
    its lead went.
  - **Named colors were nine.** `--accent`'s own `--help` gives
    `kettle --accent teal` as its example, and `teal` was not one of them: before
    the flag validated its value that silently fell back to the configured
    accent, and after, it became a hard error on kettle's own documented
    example. `orange`, `purple`, `pink`, `navy` and the rest failed the same
    way. All 148 CSS/X11 named colors resolve now; the original nine keep the
    values configs were written against.
  - **`--check-config` reported one inert setting as several.** It
    deduplicated on the spelling in the file, so `use-system-font` and
    `use_system_font` — which the parser folds to the same key — were listed
    separately, reading as two problems to fix.
- **Reconnect went to a different endpoint than the session it cloned.** The
  right-click "Reconnect" / "Re-attach" entry rebuilt its command from the host
  or container name alone. Every option that decides which machine that name
  reaches was parsed past and discarded: `ssh -p 2222 -J bastion -i key box`
  came back as plain `ssh box` — another port, no bastion, another key — and
  `docker --context remote exec web` came back as a local `docker exec web`,
  attaching to whatever container happened to share the name on this machine.
  `kubectl -n prod exec api` reconnected in the default namespace, and a pod's
  `-c sidecar` was lost. The endpoint-selecting options now travel with the
  name and are re-emitted, each single-quoted; where one cannot be reproduced
  faithfully (`-o ProxyCommand=…`, `-W`, `podman --remote`, a bearer token that
  must never be echoed back into a command line) the menu entry is dropped
  instead, because no Reconnect beats one that lands somewhere else.
- **An option's value could be reported as the host or container.** Short
  options are now read the way getopt and Go's pflag actually parse them, so a
  boolean in front of a value-taking letter no longer hides the value:
  `ssh -vp 2222 box` reported the PORT as the host, `ssh -luser box` dropped the
  login name, and `kubectl exec -itc sidecar pod` reported the sidecar as the
  pod. The long-option tables gained the entries whose absence had the same
  effect — `kubectl exec --container sidecar pod` named the container as the
  pod and `docker exec --env-file vars web` named the env file as the
  container — and `lxc-attach --name web` / `--name=web` / `-nweb` are detected
  at all now, where only the separated `-n web` form used to be.
- **`--` before the container made the COMMAND the container.** `kubectl exec -f
  pod.yaml -- sh` takes the pod from the manifest and runs `sh` inside it; the
  walk skipped the `--`, titled the pane `kubectl: sh`, and offered to re-attach
  to a container by that name. `--` is now read the way each CLI reads it:
  docker and podman use it only to end flag parsing, so `docker exec -- web sh`
  still names `web`, while kubectl — and `podman exec --latest`/`-l` — take
  everything after it as the command, which leaves no name in the argv and
  therefore no menu entry rather than a wrong one. In the same pass: `podman -r`
  (the documented short form of `--remote`) was not gated, so a session on the
  remote service offered a reconnect to the LOCAL socket; the kubeconfig
  `--user`, `--as`, and the Docker/kubectl TLS and client-certificate flags were
  consumed and forgotten, so the rebuilt command authenticated as a different
  account; `lxc-attach --uid`/`--gid`/`-g`/`-o` had their VALUES read as the
  container name; and `ssh -o IdentityFile=…` / `CertificateFile=…` /
  `IdentityAgent=…` / `CanonicalizeHostname=…` were not treated as
  endpoint-selecting, while a leading space (`-o " ProxyJump=bastion"`, which
  OpenSSH honours) evaded the keyword gate outright.
- **A Windows install path removed the Reconnect entry.** `ssh -i "C:\Program
  Files (x86)\OpenSSH\key" box` fell outside the reproducible path charset, so
  the menu entry disappeared entirely. Parentheses, brackets, braces, commas and
  apostrophes are all literal inside the POSIX single quotes the value is
  emitted in, so they are accepted; argv-derived values are additionally
  length-bounded, and a second, different `ssh -i` (OpenSSH tries every identity
  in order) suppresses the entry rather than reproducing only the last key.
  - **A control client kept using a connection whose response was still in
    flight.** After a request timed out, was cancelled, breached the buffered
    event bound, or hit malformed data, `kettle-ctl`'s client stayed reusable
    without draining or retiring the stream — so the abandoned request's
    response was read as the NEXT call's, failing correlation, and a second
    (possibly mutating) request went onto a stream nobody could correlate. Any
    such outcome now retires the connection: further use fails with a distinct
    error naming the abandoned request, and the caller reconnects. Retiring
    closes the transport and releases what the abandoned exchange had buffered,
    so a caller holding a retired client no longer holds one of the server's
    connection slots with it. A structured server error is a real response and
    still leaves the client usable, and so does a request whose deadline
    expired before any of it reached the wire — the server never saw that one.
    A timeout or a cancellation now also says that the server may have carried
    the request out anyway, since that is what decides whether retrying it is
    safe, and `kettle ctl` and the MCP bridge show the agent nothing else.
  - **One launcher click could open two windows.** Bare-launch activation is
    at-least-once — the primary opens the window before writing its response —
    but the request carried nothing that identified the launch, so a response
    lost to a slow cold start made the secondary re-send an identical request
    the primary could not tell apart from a second click. Every launch now
    carries an idempotency key; the primary remembers what it did for that key
    and answers a retry from the record, and a retry that arrives while the
    first attempt is still opening the window waits for that attempt's outcome
    instead of opening a second one. That wait is bounded well inside the
    deadline the requester is reading under, so a duplicate always leaves with
    an answer rather than waiting out its own request for nothing.
  - **A reused process id resurrected dead registry and presence records.**
    Liveness identified an owner by its pid alone, so once the system handed
    that number to an unrelated program, a dead control server stayed
    advertised (every client wasting a connect attempt on it) and a closed
    window's accent claim stayed active, keeping that color out of the pool
    forever. Records now name their owner by pid *and* by that process's
    OS-reported start time; a record whose pid is alive but whose instance is
    gone is pruned. A record without the token (an older build, or an OS that
    cannot report one) keeps the previous bare-pid answer rather than being
    pruned on suspicion. Pruning is equally careful in the other direction: a
    record is named on disk by its owner's pid, so the delete re-reads the file
    and does nothing unless it is still the record that was judged — otherwise
    the new kettle that inherited the pid, and registered at that same name,
    would be the one erased.
  - **Kitty images composited over transparency came out darkened.** The blend
    ignored the destination's own alpha and never divided back out of
    premultiplied space, so colour drawn onto a transparent pixel was pulled
    toward black in proportion to the transparency under it. A kitty animation
    frame canvas starts out fully transparent, which makes that the common case
    rather than a corner. Verified bit-identical for an opaque destination, so
    nothing that rendered correctly before moves.
  - **Images silently stopped appearing past the 256th.** A transmission whose
    id the saturated store refused was handed back with that id anyway, so a
    later `a=p,i=<id>` found nothing and `a=d,i=<id>` freed nothing. It now
    draws while advertising no id — exactly like an `i=0` transmission — since
    `icat`, `timg` and `chafa` all send fresh ids and never delete. The `U=1`
    virtual form is declined outright, because a virtual placement is resolved
    by id later and has no id-less fallback.
  - **A client that stopped reading `kettle mcp`'s stdout stranded the server.**
    The writer thread blocks in `write`, the bounded response channel fills
    behind it, and every tool worker blocks mid-send; shutdown then joined those
    workers, which never return. The process stayed alive holding a terminal,
    answering nothing, until something killed it. Worse, the reader loop blocked
    on the same channel — so the server stopped reading stdin, which is exactly
    where the `notifications/cancelled` that would free it arrives. Every wait
    on the peer is bounded now, and the first send that proves the peer is not
    reading short-circuits the rest.
  - **The installed-version record was never checked.** Every other field of
    `install.json` was validated on read; `version` — the one a person actually
    reads, and what support instructions and packaging scripts consult for
    "what is installed here" — was written and trusted. A marker carrying a
    version no kettle installer would write is now refused like any other
    mismatch.
  - **`minimum-contrast` did nothing for bold text under `bold-is-bright`.** The
    lift ran first and the bright remap then replaced the foreground outright
    with a palette entry, discarding it. Since the bright variant is the lighter
    one, the case it threw away is exactly the one that needed it: pale bold text
    on a pale background. The two steps now run in the order that makes the
    guarantee hold.
  - **`background-darkness` was documented backwards.** The code matches
    Terminator — the value is the background colour's alpha, so `0.0` is
    see-through and `1.0` is fully covered — but both `docs/CONFIG.md` and the
    setting's own reference described the opposite ("1.0 = no tint, 0.0 = fully
    dark"), so anyone configuring it from the documentation reached for the
    wrong end. The prose is corrected and the direction is pinned by a test.
  - **Untrusted output could point a pane's working directory off the machine.**
    A reported cwd (OSC 7 or OSC 9;9) is a claim by whatever is writing to the
    pane, and kettle acts on it — an existence check, a new tab's directory,
    "open in file manager". One line of output could set it to a UNC server
    path, and on Windows the very next existence check reaches out over SMB or
    WebDAV and hands over the machine's credentials during the handshake, before
    anything is opened. `cat`ting a hostile file was enough to send it. Both
    channels now refuse a path that leads with two separators, carries a control
    character, or is longer than any real path.
  - **A directory anyone could add files to counted as private (Windows).** The
    trust check covered removal and re-permissioning but not creation, so an
    ACE granting `FILE_ADD_FILE`, `FILE_ADD_SUBDIRECTORY`, or `GENERIC_WRITE`
    let an untrusted principal plant a session, layout, or control-server
    registry entry where kettle enumerates and reads them back. Creation rights
    are refused on that directory now — and deliberately still allowed on its
    ancestors, because `C:\` grants Authenticated Users "create folders" on
    stock Windows and a directory created there reaches nothing of kettle's.
  - **`--working-directory` had no test at the CLI surface**, and neither did
    `--accent`. Both are validated by one function now, driven by the tests
    exactly as the CLI drives it.

  ### Changed
  - The alpha-blending convention is verified by rendering a half-opaque quad
    and reading the pixel back, rather than only by reading the shader source.
    A source-level check can be worked around; the source check remains as a
    cross-check for the pipelines the GPU test cannot cheaply stand up, and now
    resolves local aliases so multiplying through a renamed variable is still
    counted.
  - Three decisions moved out of long functions into named ones so their tests
    exercise what production runs rather than a restatement of it: the cursor
    glyph colour, `--write-default-config`, and the profile cycle order.

## [2.46.0] — 2026-08-03

  Terminator-parity pass. Everything here is a setting, gesture, or documented
  promise that already existed and did not do what it said — found by auditing
  kettle against a clone of GNOME Terminator, then by four rounds of
  adversarial review over the fixes themselves.

  ### Fixed
  - **`ask-before-closing` was bypassed by four close gestures.** Only the three
    close *actions* asked. The titlebar ✕, Alt+F4 (both arrive as
    `WindowEvent::CloseRequested`), the tab bar's ✕ button, and middle-clicking
    a tab all closed immediately — so a window of running work vanished on one
    stray click under every setting, `always` included. Every close now routes
    through one gate, and a drift guard fails the build if a close site is added
    that skips it.
  - **A confirmed close could act on something the user never selected.** The ✕
    and middle-click can target a tab that is not the focused one, and the
    prompt can sit unanswered for as long as the user takes to reply — during
    which an exiting shell renumbers the tabs, or the target pane's own shell
    exits and promotes a sibling into focus. Confirmations now carry their
    target: a tab by the panes it holds, a pane by its id. A target that has
    since vanished closes nothing rather than falling back to whatever is
    focused now.
  - **The recording tail depended on which gesture closed the window.** The
    flush walks the pane map and reads each pane's output sidechannel, and two
    of the three window-close paths cleared that map first — handing the flush
    nothing to drain. The same clearing decided what `restore-session` saved, so
    whether a session came back depended on whether the confirm prompt happened
    to appear. Both are now decided by the requester's intent, not by timing.
  - **A hand-set pane title died three ways.** Shells emit OSC 0/2 on every
    prompt, so naming a pane lasted under a second. Gating the OSC *set* alone
    was not enough — a title RESET wiped it too, and shells emit those at
    prompts as well. Separately, applying a title inside a remote context
    cleared the saved pre-remote title *and* demoted the origin, so leaving ssh
    restored nothing: a pane named `db-prod` came back still calling itself the
    remote host. The remote shell's own title still shows while connected.
  - **`icon_bell` could not draw under any configuration.** The renderer gates
    the per-pane titlebar bell on `cfg.icon_bell && pane.bell`, and the frame
    builder passed a literal `false` for every pane — the pane had no bell state
    at all, only the tab did. Panes now latch their own, cleared when the user
    looks at them.
  - **Every Lua URL handler kettle ships was dead.** `docs/examples/init.lua`
    documents the contract as *return a string and kettle opens that URL*, and
    every example in it is that shape (`LP: #12345` → Launchpad, `lp:branch`,
    `apt://`). The returned string was discarded while the handler still claimed
    the URL, so copying the documented file gave you links that matched, ran,
    computed the right URL, and opened nothing. A handler that raises or
    declines now falls through to kettle's own open instead of killing the link,
    a malformed Lua pattern no longer disables every handler after it, and a
    rewritten URL is re-checked against the same allowlist as the original.
  - **`background_image_mode = Tile` was accepted and ignored.**
    `--check-config` validates the three background-image placement enums
    case-insensitively while the renderer matches them case-sensitively, so an
    accepted uppercase value fell through to the default arm. Accepting a value
    is a promise it does something.
  - **`inactive_color_offset` dimmed nothing.** Its foreground offset was read
    nowhere, so Terminator's own default pair produced exactly zero visible
    dimming.
  - **A hidden tab close button was still clickable.** `close-button-on-tab =
    false` hid only the paint; the hit rect stayed full-size, so clicking the
    trailing square of a tab closed it — and every pane in it — with no visible
    button.
  - **Three right-click rows Terminator has were missing.** Set Window Title,
    Split Auto, and Zoom/Restore all existed as bindable actions; only the menu
    rows were absent, making them keyboard-only by accident.

  ### Fixed — Terminator config import
  - **A real Terminator config is sectioned INI, and it was read flat.** Every
    line applied regardless of section, so the LAST profile in the file won and
    a user's `[[default]]` colours were silently replaced by whichever profile
    was written last. `[layouts]` internals leaked in as config keys, and
    `--check-config` reported every section header as a line missing its `=` — a
    wall of errors on a well-formed file. kettle now reads `[global_config]`,
    `[keybindings]`, and the default profile. A file with no sections is
    kettle's own format and is untouched.
  - **The whole `[keybindings]` section imported as nothing.** The tokenizer
    folds `_` to `-` so Terminator's key spellings reach kettle's hyphenated
    arms — and that fold also rewrote the ACTION names, which are spelled with
    underscores. `new_tab` arrived as `new-tab`, matched no arm, and became an
    unknown key. Verified before the fix: `new_tab = <Control><Shift>y` left the
    keybind count unmoved.
  - **Imported bindings were additive.** Terminator's grammar is `action =
    accelerator` — one accelerator per action. Treating it as additive left
    kettle's stock chord live alongside the imported one, so rebinding `new_tab`
    precisely *because* Ctrl+Shift+T collides with tmux, AstroNvim, or an agent
    CLI did not resolve the collision. kettle's own `keybind =` stays additive.
  - **Grouping reaches every window** — Terminator's scope is its whole
    terminal collection — and `group_all_toggle` asks the pane you invoked it
    from rather than testing whether everything is already grouped, which
    inverted the answer whenever one pane had been ungrouped by hand.
    `group_tab_toggle` and `group_win_toggle` are their own actions now; when
    they shared one with the non-toggling names, importing both had the second
    silently unbind the first.
  - **`group_all` armed broadcasting instead of grouping.** Grouping is not
    broadcasting — in Terminator you group terminals and then choose to
    broadcast to the group. All three `*_toggle` names were mapped onto
    broadcast actions, so one press after importing sent everything typed to
    every pane at once.
  - **A malformed accelerator bound an ordinary letter.** An empty modifier
    group was dropped, so `<>t` narrowed to the bare key `t` — from then on
    typing that letter fired the action instead of reaching the shell. Malformed
    accelerators now bind nothing. GTK's `<Ctl>` abbreviation is accepted.
  - **Pango style options were folded into the family name.** `font = DejaVu
    Sans Mono Bold 13` asks for the bold face of `DejaVu Sans Mono`; keeping the
    whole string requested a family no system has, so the font silently fell
    back to something else entirely.
  - **`scrollback_lines`** — Terminator's own key name — had no arm, so the
    scrollback size out of a copied config did nothing. Added, with
    `scrollback_infinite`.
  - **A working colour palette was reported broken.** Terminator writes its
    palette as one colon-separated list, which applies correctly at runtime
    while `--check-config` called it malformed — sending users to fix a line
    that was already right.
  - **Quoted values.** Terminator's manual writes `background_color =
    "#1a1b26"`; the quote became part of the value and the line was silently
    discarded. One matched pair is now stripped, documented in `CONFIG.md`.
  - **A malformed section header kept the previous section in force.** A typo'd
    `[[work]` fell through while the parser still believed it was inside
    `[[default]]`, so one profile's settings quietly became the user's
    defaults — a failure with no warning and no symptom except wrong colours.
    An unreadable header now means the parser has lost its place, and nothing
    applies until the next one it can read. A skipped nesting level
    (`[profiles]` then `[[[default]]]`) no longer collapses into the
    default-profile path either.
  - **`scrollback_infinite` raced `scrollback_lines`.** Whichever line came
    second won; Terminator treats the boolean as an override of the count.
  - **An empty accelerator now disables a shortcut**, which is what it means in
    Terminator — its own defaults ship several, and its preferences UI writes
    one when a binding is cleared. Ignoring the line left kettle's chord live,
    so a config that deliberately freed a chord for tmux, AstroNvim, or an
    agent CLI did not free it.
  - **Pango: `Regular` and family lists.** `Regular` was missing from the style
    table, so the most common description GTK writes
    (`DejaVu Sans Mono Regular 13`) requested a family no system has. A comma
    is Pango's family-LIST separator, so `Arial Black, 12` resolved to the
    family `Arial`.

  ### Fixed — CLI
  - **`--profile typo --list-profiles` refused to run**, which is the command
    that shows the valid profile names. Same for `--list-themes`,
    `--list-actions`, `--print-completions`, `--shell-integration`, and
    `--check-update`, none of which read a profile. `--list-keybinds`,
    `--list-layouts`, `--list-ssh-hosts` and `--check-config` still refuse:
    those resolve the profile, so a typo means the output is quietly wrong
    rather than merely blocked — and any of them appearing in a mixed
    invocation disqualifies the whole command line, since several modes can be
    set at once and only the first runs.
  - **Named layouts were written back over.** A `--layout NAME` launched with a
    command or cwd override saved the overridden state back to the named layout
    file, destroying it.
  - **Infinite scrollback displayed as `0`** in the Settings panel.

  ### Documentation
  - `docs/TERMINATOR-AUDIT.md`: corrected five claims that no source supported —
    `hide_titlebar`, `tab_max_width`, and `use_login_shell` are not Terminator
    options at any SHA; `maybe_confirm_then` exists only in this feature's design
    pseudocode and changelog entry, never in a Rust source; and
    `ask_before_closing` was described as "complete end-to-end" while four
    gestures bypassed it. The Method section records how these were found, so
    the next pass can re-run the check instead of re-reading prose.
  - `docs/CONFIG.md`: new Syntax section covering the `=` split, comments,
    `-`/`_` equivalence, and the quote rule.

## [2.45.0] — 2026-08-01

  ### Fixed
  - **Recording retention could delete a file Kettle never wrote.** Any name in
    the recording directory that merely started `kettle-session-` and ended
    `.cast` was a deletion candidate, which is far looser than what Kettle
    generates and contradicts the documented promise that unrecognized files are
    left alone. A file a user named `kettle-session-notes.cast` and left there
    was deleted if it was the oldest and the budgets demanded a deletion.
    Retention now matches the generated grammar exactly —
    `kettle-session-<seconds>-<pid>-<counter>.cast`, all three fields non-empty
    decimal digits. `docs/RECORDING.md` states plainly that this narrows
    ownership without proving it, since a file matching the shape exactly is
    still eligible.
  - **Retention allocated in proportion to the whole recording directory.** It
    collected every matching entry, with its path and metadata, then sorted the
    lot — O(n) memory and O(n log n) time to enforce a 50-file target, at
    recorder startup. A directory left to accumulate made that cost arbitrarily
    large on a path that must stay responsive. The scan now holds only the
    oldest batch of candidates in a bounded heap and walks forward through the
    directory, so memory is bounded by the batch rather than by the directory.
    Coverage is unchanged: a batch whose entries are all locked advances past
    them instead of stopping, so an active file can never shield a newer
    deletable one.
  - **`cargo build -p kettle-core` failed on its own.** Per-pane session logging
    creates its log through `kettle-state`, but that dependency was optional and
    enabled only by the `asciicast` feature, so building the crate standalone
    did not compile. Cargo feature unification hid it completely: other
    workspace members enable `asciicast`, so every `--workspace` build — and
    therefore all of CI — turned the dependency on. The dependency is now
    required, and the gauntlet lints `kettle-core` by name so a check exists
    that a workspace build structurally cannot substitute for.
  - **A `PATHEXT` ending in `;` crashed Kettle while resolving a program.**
    Resolving a command against `PATH` sliced each `PATHEXT` entry to drop its
    leading `.`, which panics on an empty entry — and a trailing separator
    produces exactly one, because installers append extensions without checking
    whether a separator is already there. The same slice assumed that leading
    `.` occupied a single byte, so an entry starting with a multi-byte character
    panicked on a char boundary, and the entry was additionally required to be
    UTF-8, which a Windows environment variable is never obliged to be. Any of
    the three aborted the terminal at the moment it tried to spawn a pane.
  - **`PATHEXT` replaced part of the requested program name instead of
    extending it.** Resolving `foo.bar` searched for `foo.EXE`, because the
    lookup substituted the extension rather than appending one. Windows appends
    — `cmd` and `CreateProcess` look for `foo.bar.EXE` — so Kettle could run a
    *different* program that merely shared the requested name's stem. Extensions
    are now appended, matching the operating system.
  - **The Unix passwd lookup raced with itself and could dereference NULL.**
    Resolving the login shell and home directory used `getpwuid`, which returns
    a pointer into a buffer shared by the whole process — so two panes opening
    at the same moment raced, and the second lookup could overwrite the entry
    while the first was still reading through it, leaving neither the shell nor
    the home path trustworthy. Both fields were also read with
    `CStr::from_ptr` and no NULL check, though a passwd entry is not obliged to
    supply either; that is a crash rather than a missing value. The lookup now
    uses the reentrant `getpwuid_r` into a caller-owned buffer, checks both
    fields, and treats an empty `pw_shell` as POSIX specifies — the default
    shell — rather than as a shell named "".
  - **The Windows environment block was read through a misaligned pointer.**
    Expanding a `REG_EXPAND_SZ` value reinterpreted the registry's `Vec<u8>` as
    `*const u16` and built a slice from it, which is undefined behaviour:
    `slice::from_raw_parts` requires alignment for its element type, and a byte
    vector guarantees none. It also fed that buffer to
    `ExpandEnvironmentStringsW` without a terminator, so a registry value that
    was not NUL-terminated — or whose length was odd, since the trailing byte
    was dropped — was read past its end, and an expansion failure silently
    replaced the variable's value with an empty string. Values are now decoded
    pairwise, terminated explicitly, and fall back to the unexpanded text when
    expansion fails.
  - **A command killed by a signal is no longer indistinguishable from one that
    failed.** On Unix, `kettle exec` reported a generic `1` for every signal
    death, so `kill -TERM` on a child looked exactly like the child running
    `exit 1`. Automation driving `kettle exec` therefore could not tell
    termination from ordinary failure. Signal deaths now report the shell's
    `128 + signal` — `143` for SIGTERM, `137` for SIGKILL, `130` for SIGINT —
    and the numeric signal is retained alongside its name for callers that need
    to act on it directly. Ordinary exit codes are unchanged.
  - **Windows child termination reported its outcome backwards.**
    `TerminateProcess` returns nonzero on success, but the vendored PTY layer
    treated nonzero as an error and zero as success. Every successful kill was
    reported as a failure, and — the damaging direction — every genuine failure
    was reported as success, so Kettle could wait indefinitely on a process it
    had never terminated. A failed exit-status query was likewise reported as
    "still running", making an unreadable process indistinguishable from a live
    one, a failed wait was ignored entirely, and handle exhaustion aborted the
    terminal instead of returning an error. `kettle exec` now says so on stderr
    when a child could not be terminated, rather than exiting quietly while the
    process survives.
  - **A pane that failed to open left its shell running.** Opening a pane
    spawns the child first and then finishes wiring the terminal around it —
    taking the pseudoterminal's polling descriptor, taking the non-blocking
    writer, cloning the reader, starting the reader thread. Any of those can
    fail, and dropping the child handle does not terminate the process it
    represents, so the failure was reported while a live shell stayed behind
    with no owner, no reaper, and no handle for Kettle to reach it by. On a
    machine where one of those steps fails reliably, every attempt to open a
    pane leaked another process, each holding its end of a pseudoconsole. The
    child is now terminated if construction does not complete.

## [2.44.0] — 2026-08-01

  ### Security
  - **A bare-name `conpty.dll` load was a DLL preloading vector.** The vendored
    ConPTY loader probed `conpty.dll` as a relative name, which reaches
    `LoadLibraryW` and walks the full search order — application directory,
    working directory, then `PATH`. The file is in neither `System32` nor
    `SysWOW64` on a stock Windows 11 host and Kettle ships none, so the probe
    always missed and the search always ran. A terminal is routinely launched
    with its working directory set to a project the user has just cloned, so a
    planted `conpty.dll` would have run its `DllMain` inside Kettle, at Kettle's
    privilege level, on the first pane open — before any missing-export check
    could reject it. Kettle neither ships nor supports a sideloaded
    OpenConsole, so only the system kernel32 exports are resolved now. The same
    pattern was removed from vendored `alacritty_terminal`'s Windows TTY
    backend, which Kettle does not use but which compiles into the binary
    regardless.
  - **Closing a Unix pane executed whatever the user had half-typed.** The
    vendored PTY writer's destructor wrote `\n` followed by `VEOF` into the
    terminal, so closing a pane supplied an Enter nobody pressed and the shell
    ran the pending line. The justifying comment was wrong on two counts:
    canonical `VEOF` makes pending input available immediately and needs no
    preceding newline, and the disabled sentinel is `_POSIX_VDISABLE` rather
    than `0`. A blocking write inside `Drop`, reached before the child reaper is
    spawned, could also stall teardown on a full input queue. Destructors now
    only close the descriptor they own; deliberate EOF remains Kettle's own
    path, which reads live termios and honours `_PC_VDISABLE`.
  - **The updater could be made to install bytes its signed-release boundary
    never approved.** The Windows pending-update record carried only mutable
    stage and helper hashes plus a target version, and the helper rebuilt an
    unsigned update without revalidating the release signature, the signed
    manifest, the asset digest, or the installed version. A same-SID process
    could copy the running Kettle as a correctly named helper, create the
    allowed stage files, compute matching hashes, write the pending record, and
    have Kettle install arbitrary — or older — bytes. A merely stale record
    could downgrade a newer manual install. Pending updates now use an
    authenticated capsule carrying the signed manifest, its Ed25519 signature,
    the selected asset, the archive digest, and the package manifest; startup
    and the helper reverify all of it against the compiled key, read the
    actually installed version, and refuse equal-version and downgrade
    transitions. Verification and application no longer communicate through
    pathnames — what was verified is provably what is applied.
  - **The control plane bounded neither peer lifetime nor peer identity.** A
    client that connected and never read, or fed one byte at a time, held its
    slot indefinitely, and liveness probing could not reclaim it while a
    connection thread was blocked in a write. Requests now expire after 30
    seconds of inactivity, frame assembly carries an absolute five-second
    budget measured from when the server begins waiting, responses and events
    carry write deadlines, and subscribers are kept honest with keepalives.
    Neither end authenticated the other: Unix now compares effective peer UIDs
    and Windows clients verify the pipe object's owner. Server-accepted and
    client pipe handles are now overlapped on Windows so the existing
    cancellation path covers server writes, which previously ignored their
    deadline — masked only because the sole caller capped frames below
    `PIPE_BUF`. Request framing is no longer quadratic in the accumulated
    buffer.
  - **`base64`'s new default-on `simd-unsafe` feature put unused `unsafe` on
    the untrusted-decode and signature-verification paths.** It is
    hand-written `core::arch` AVX2/NEON covering both encode and decode.
    Kettle only ever uses the scalar engine, so the feature was inert — but
    those call sites decode kitty and iTerm image payloads (untrusted terminal
    output) and the Ed25519 release signature, and unused `unsafe` has no
    place on either. Disabling it also makes `base64` enforce
    `#![forbid(unsafe_code)]`.

  ### Fixed
  - Xterm `modifyOtherKeys` negotiation now has real per-terminal state.
    It starts at level zero, reports only the application-selected level, and
    applies the modifier-aware level-one/two matrix to Return, Tab, Backspace,
    Escape, Space, and ASCII keys without gating cursor, function, or keypad
    keys. Omitted XTMODKEYS values and both RIS and DECSTR restore the initial
    level. The new `modify-other-keys = enter|off` setting controls only
    Kettle's pre-negotiation modified-Enter fallback; `enter` is the default so
    existing CLI multiline chords survive without pretending level two is on.
  - A `kettle exec` run whose consumer closed the pipe could die from `SIGPIPE`
    instead of reporting the exit code it had already chosen. The stdout worker
    blocks `SIGPIPE` for itself, so a broken pipe correctly surfaced as `EPIPE`,
    was reported on stderr, and became exit 74 — but the worker wrote through
    the process-global `std::io::stdout()`, and bytes the failed write left in
    that shared buffer were retried by the runtime's exit-time flush on the main
    thread, where `SIGPIPE` is back at its default fatal disposition. The signal
    then killed Kettle and discarded the code, so callers saw a signal death
    with no status at all. Whether bytes remained buffered was a timing race, so
    the failure was intermittent. The child stream now goes to a descriptor of
    exec's own and never enters that shared buffer; a broken stdout
    additionally makes the chosen exit code final. Streaming also stops paying
    for a process-wide lock and a second copy of every byte.
  - Diagnostics no longer contaminate `kettle exec`'s output.
    `tracing_subscriber`'s default writer is stdout, so a single warning could
    splice a log line into byte-exact child output, or between the NDJSON
    records agent callers parse. Logging now writes to stderr, which the
    adjacent ANSI-detection had already assumed.
  - `kettle exec --timeout` now keeps its deadline and MCP cancellation
    enforceable while draining output after the child exits. If stdout is still
    stalled at the deadline, Kettle abandons output the downstream consumer
    cannot accept and returns the child's collected exit code, or 124 if no
    status was available; cancellation always wins with 130. Ordinary
    completion remains lossless, but stdout-worker acknowledgements and final
    flush/join are polled from the lifecycle loop instead of blocking it.
    Abandoning output now says so on stderr. Because the exit status in that
    case is the child's own, a caller reading only the status could otherwise
    not tell a fully delivered run from one whose tail was dropped because its
    own reader had stalled.
  - The release workflow's asset verification never ran, and the release never
    published itself. It read the release through
    `GET /releases/tags/{tag}`, which only finds *published* releases, so the
    draft it had just created returned 404 — and under `set -e` the step died
    there, before `verify-release-assets.py` could compare uploaded sizes and
    SHA-256 records against the local set. v2.43.0's assets were therefore
    uploaded, left unverified by the gate, and stranded as a draft until
    published by hand. Every lookup now goes through the list endpoint, which is
    the only one that sees drafts, and the release id it returns addresses the
    verification read and the final publish. A re-run also recognizes its own
    leftover draft instead of failing to recreate it.

## [2.43.0] — 2026-07-28

  ### Fixed
  - Updated Wayland protocol code generation to `wayland-scanner` 0.31.11 and
    `quick-xml` 0.41.0, removing both current RustSec denial-of-service
    advisories for `quick-xml` instead of retaining the former build-time
    exception.
  - `gpu-backend` now applies without requiring a physical GPU pin, explicit
    low/high power preference wins across backend ranks, detected software
    adapters remain valid device pins, and unavailable portable backend
    settings fall back observably instead of preventing startup. Stale pins
    preserve the platform-preferred physical adapter when falling back to Auto.
  - Surface timeouts, occlusion, and swapchain reconfiguration no longer count
    as painted frames. On the normal render path, Kettle now consumes PTY output
    generations, advances paint timestamps, and updates flood pacing only after
    wgpu presents the frame. (Visible startup windows are still revealed after
    renderer initialization, before their first redraw; the device-loss guard
    still snapshots generations without presentation to quiesce output while
    recovery is in progress.)
  - Surface acquisition and renderer failures can no longer strand terminal
    damage or enter an immediate redraw loop. Timeout/outdated retries use a
    capped per-window deadline backoff, invisible windows keep repairs armed
    without GPU wakeups, `Lost` recreates only the affected wgpu surface on the
    healthy shared device, and non-device render errors rebuild that renderer's
    retained resources on a separate capped backoff. Process-wide device
    recovery now preserves every window's live font/cell scale, resolved
    accent, and queued screenshot completion across failed adapter attempts,
    then reflows each rebuilt surface at its current monitor size and DPI.
    Paint wakeups also honor the complete occluded/minimized/invisible
    predicate, retaining damage without futile redraw loops until restore.
    Output transport remains wakeable only when an opt-in recorder/Lua
    sidechannel needs event-loop service.
  - PTY output, inline images, animation updates, progress, and notifications
    now publish through one generation-ordered per-pane wake gate. The gate
    remains closed for the complete deferred-frame interval, stale queued wakes
    rearm and resample without losing racing output, and the paint state machine
    cannot enter its presenting phase until every visibility, recovery, and
    renderer-availability guard has passed. Hidden and recovering windows keep
    paint wakeups quiescent while still servicing opt-in recorder/Lua output
    sidechannels, so their bounded queues cannot stall PTY parsing.
  - DA1 capability code `52` now follows the live OSC 52 write policy and
    platform clipboard availability. Kettle continues to advertise its sixel
    decoder, but no longer tells Neovim, tmux, or other probers that clipboard
    writes are available when `osc52 = off`/`paste` or the host has no clipboard;
    live config reloads update existing panes without restarting their PTYs.
  - OSC 52 target `p`/`s` now reads and writes Linux PRIMARY instead of silently
    using the regular clipboard. Failed PRIMARY queries return an empty
    protocol reply rather than falling back across clipboard targets; Windows
    and macOS retain their single clipboard channel.
  - Atomic replacement now applies and syncs the final Unix mode and preserved
    permissions on the staged inode before publication. A power loss between
    publishing an updated executable and checkpointing its journal can no
    longer strand the installed binary at the private staging mode `0600`.
    Exact same-destination staging files orphaned by a hard-killed writer are
    reclaimed only when their canonical creator PID is definitively dead and
    their opened object proves owner-private, ordinary, single-link identity;
    cleanup is bounded and preserves live, malformed, linked, or nonregular
    lookalikes. Its asynchronous scheduler now distinguishes in-flight work
    from a bounded, expiring completion cache, retries after saturation or
    worker failure, and evicts old keys instead of permanently disabling
    cleanup after 256 distinct destinations.
  - Linux updater destination snapshots now retain their anchored parent handle
    until the descriptor-relative leaf is open. Existing files are no longer
    misclassified as absent through a dangling `/proc/self/fd/...` path, so
    rollback reliably backs up and restores the prior executable and mode.
  - Failure to create a pane's blocking PTY pump thread is now logged and closes
    the pane through its normal exit event. Thread exhaustion can no longer
    leave a parser waiting forever on a channel whose sender was never created.
    Pane teardown also moves child kill/reap and native master destruction onto
    that detached lifecycle path: Windows `ClosePseudoConsole` can wait for
    conout drainage, so destroying it on the UI thread could still freeze pane
    close even without joining the reader. Before the detached close starts,
    the blocking pump now switches to direct discard/drain mode and can escape
    a full bounded parser handoff; the reader stop is published only after the
    native close returns. This prevents pre-Windows 11 24H2
    `ClosePseudoConsole` from stranding the reaper behind undrained output while
    retaining prompt UI-side Drop. Teardown-worker exhaustion fails open by
    logging and retaining the handles rather than entering an unbounded
    platform close on the event thread.
  - Mixed-DPI monitor moves no longer reflow the grid twice. Kettle applies the
    new glyph scale immediately but coalesces `ScaleFactorChanged` with the
    following nonzero physical resize before resizing PTYs; a one-shot
    event-loop fallback covers backends without that resize and remains pending
    while minimized or while the renderer/GPU is recovering.
  - PTY size changes now carry one versioned transaction containing both the
    grid and the exact text-area pixel extent derived from fractional cell
    metrics. Spawn, restore, tab, split, duplicate, SSH, undo, and live resize
    paths share it; failed native resizes remain retryable, and Windows clamps
    ConPTY's signed 16-bit grid without making synchronous calls for pixel-only
    changes.
  - **Agent `send_keys` now matches negotiated Kitty keyboard input.**
    Synthetic `Shift` chords retain their modifier in CSI-u (so
    `ctrl+shift+c` is no longer reported as `ctrl+c`), and synthetic
    Control chords no longer invent associated text for values such as
    `ctrl+space`. Bare uppercase tokens now infer that same Shift modifier, so
    `G` reports Kitty's unshifted `g` primary and shifted `G` alternate instead
    of an unmodified Caps-Lock-style key.
  - **Agent/TUI compatibility smoke isolation and shell parity.** Unix native
    runs now use deterministic non-rc Bash instead of feeding POSIX commands to
    an arbitrary default shell. WSL removes `/mnt/<drive>` entries from the
    target `PATH` and rejects canonical tool paths that still resolve to the
    Windows host. Neovim snapshots use unpredictable private directories,
    dereference config links through a regular-file-only traversal bounded to
    100,000 entries, 64 directory levels, 256 MiB per file, and 2 GiB total,
    and redirect `HOME` plus all XDG state away from live configuration.
    Neovim/AstroNvim expected markers are assembled only inside Vimscript, so
    an echoed launch command cannot satisfy a live-state wait. tmux smokes use
    a cryptographically random private socket, resolve Bash inside the selected
    target, and register checked cleanup for success and failure paths.
    Windows Git Bash probes resolve npm clients to adjacent `.cmd` launchers
    through `cmd.exe`, and their self-test rejects unusable extensionless
    shadows. Live Windows diagnostics and copied Neovim state now use
    unpredictable `%LOCALAPPDATA%\kettle` trees with protected current-user and
    SYSTEM-only DACLs and no reparse-point ancestry. The tab-bar visual probe
    accepts the intentional last-tab width reserved for the new-tab/menu
    buttons and waits for title geometry to stabilize before pixel comparison.
  - **Unix `kettle exec` keeps terminal replies alive after piped stdin EOF.**
    The stdin pump no longer closes the bidirectional PTY master and silently
    discards later DA, DSR, Kitty-keyboard, color, or clipboard replies.
    Canonical children receive their live configured VEOF sequence; raw-mode,
    disabled-VEOF, and uninspectable states preserve the reply channel and fail
    explicitly instead of injecting a guessed byte. Stdin-pump thread
    exhaustion now fails the command before continuing, and a parent-stdin read
    error ends forwarding explicitly without presenting partial input as EOF.
    Record-boundary planning follows live `IGNCR`/`ICRNL`/`INLCR`, VEOF, VEOL,
    VEOL2, host-specific VWERASE, and `EXTPROC` settings while bounding retained
    canonical-record state at 64 KiB; oversized records conservatively receive
    two VEOF characters, while `EXTPROC` refuses guessed EOF injection. A
    dedicated bounded writer arbiter now handles terminal replies whether or
    not stdin is forwarded, so a child that floods queries without reading
    their replies cannot block timeout/cancellation. VEOF injection advances
    one byte per lowest-priority arbiter step. Reply admission and the final
    reply recheck plus one nonblocking VEOF attempt share a short ordering gate,
    so an admitted protocol reply cannot be overtaken by an EOF write based on
    a stale empty-channel observation; the gate is released before any yield or
    retry. This intentionally makes no impossible claim that a future query can
    overtake a VEOF byte already accepted by the kernel. The parser's
    semantic-event channel is bounded at 1024 and fails the command explicitly
    on overflow. Native PTY regressions cover empty,
    line-terminated, and unterminated input,
    read-until-EOF, ordered DSR/DA1/Kitty replies, raw/`EXTPROC` no-injection,
    Linux N_TTY VWERASE, query floods with and without piped stdin, and normal
    child exit.
    Unix also permits only one live nonblocking stdin-arbiter lease per PTY.
    Duplicated descriptors share their open-file status flags, so overlapping
    handles can no longer restore `O_NONBLOCK` beneath each other; setup failure
    releases the reservation, exact restoration makes it reusable, and a
    restoration failure latches future acquisition closed.
  - **Windows ConPTY input backpressure no longer traps the PTY writer.**
    Kettle opts only its caller-side anonymous-pipe writer into `PIPE_NOWAIT`
    and advances it in bounded 1 KiB steps; the synchronous handle required by
    `CreatePseudoConsole` is unchanged. A child that stops reading can no
    longer park protocol replies, timeout, cancellation, or pane shutdown
    behind one blocking user-input write. Two native tests split that claim:
    one drives an anonymous pipe to real zero progress at the Windows boundary,
    and one requires a finite `kettle exec` timeout and close after a child
    query while the writer path is loaded. Neither forces the ConPTY input
    queue itself to refuse a write — ConPTY buffers input without a bound a
    test can exhaust, so the zero-progress guarantee is proven at the pipe
    boundary rather than end to end. Windows zero-byte pipe progress is normalized
    to `WouldBlock`, and complete-message callers retain partial progress
    through bounded retries instead of silently dropping the unwritten suffix.
  - **A stalled `kettle exec` stdout consumer no longer defeats `--timeout`
    while the child is still running.**
    Output was written synchronously on the lifecycle thread, so the slice
    limits bounded how many bytes were drained per turn but not how long one
    `write` could block. An automation client that opened the pipe and stopped
    reading held the lifecycle loop before its timeout check and suppressed
    timeout, cancellation, and child reaping indefinitely — measured still
    running 4.59 s into a `--timeout 1` run, and 5.008 s with 65 424 bytes
    buffered under the regression. Stdout now belongs to a bounded worker; when
    its queue fills, the lifecycle loop stops draining PTY output and lets
    backpressure reach the child instead of parking. Normal completion drains
    and joins losslessly, while timeout and cancellation abandon unaccepted
    output rather than block teardown. The same run now exits 124 in 1.106 s.
    This did not cover a child that had already exited while its trailing output
    remained stuck; that narrower lifecycle gap is corrected under
    `[Unreleased]`.
  - Headless `kettle exec` now omits DA1 extension `52`, matching its deliberate
    lack of a clipboard-write sink instead of advertising OSC 52 writes that
    would be discarded.
  - Pasted screenshots now enforce the advertised 256 MiB live aggregate
    against the final encoded PNG bytes, not only the total observed before an
    encode. Source RGBA is capped too, and an over-budget or failed encode
    removes its partial file through the creating handle. Successful PNGs keep
    a path-pinned identity handle through shutdown. An empty bootstrap
    establishes the held private directory before any screenshot bytes are
    encoded; Unix creates every real PNG with `openat` beneath that descriptor,
    while Windows pins the directory name. Cleanup never recursively follows a
    replaced directory: Unix child operations resolve from the held descriptor,
    while Windows identity-transitions to a DELETE handle; final empty-directory
    removal compares volume/file IDs on Windows and owner/mode/device/inode on
    Unix. Those Unix child operations are genuinely descriptor-relative on every
    platform — `openat`, `fdopendir`, `fstatat`, and `unlinkat` — rather than
    reached by joining a child name onto `/proc/self/fd/<n>`. Only Linux
    resolves such a path: Darwin's `/dev/fd/<n>` names the open file
    description itself and cannot be traversed into, so the shortcut would have
    returned `ENOENT` for every save, reopen, and removal on macOS.
    Crash sweeping moved off the startup/UI thread, has independent
    time/attempt/removal/entry caps, and requires an exact creator/session name,
    a `0001.png` through `0064.png` child name, more than 24 hours of age, a
    definitively dead PID, and only verified private, non-reparse, single-link
    regular children. Long-running sibling sessions, PID reuse, malformed
    names, hard links, and unknown entries fail closed.
  - **Private files are now fail-closed on Windows.** State, lock, recording,
    remote-command, terminal-log, diagnostic, pasted-image, screenshot, and
    crash files receive a protected current-user DACL in the creating
    `CreateFileW` call, before content exists. Existing reparse-point leaves
    and reparse-point parents are rejected. Elevated tokens no longer trust
    the group-valued default-owner SID as file provenance: the user SID is
    selected as owner explicitly, and every owner/DACL postcondition is
    verified before the handle is returned. File operations resolve from the
    held parent handle instead of repeating a mutable DOS-path lookup, and
    Win32 trailing-dot/space aliases plus NTFS alternate-data-stream names are
    rejected.
  - Private atomic replacement on Windows now moves the already-secured staged
    file into place instead of publishing under a legacy destination DACL and
    tightening it afterward. A privacy failure therefore cannot leave newly
    written state visible despite returning an error.
  - The Windows installer/uninstaller now accepts only a dedicated `kettle`
    prefix, validates bounded product and prefix markers, rejects protected
    roots, Win32 device aliases, invalid/control characters, wildcards,
    alternate streams, trailing aliases, and reparse points. Upgrades publish
    ordinary files atomically beneath retained no-delete directory handles;
    uninstall accepts one exact bounded managed tree and removes only named
    leaves plus empty directories, never recursively traversing an install
    path. Ownership JSON requires exact keys, scalar types, and no duplicates.
    A moved or edited helper can no longer redirect uninstall toward an
    unrelated directory.
  - Windows package installation now preflights and journals the complete
    payload, stages and backs up every managed file before publication, rolls
    back every completed publication on failure, and recovers an interrupted
    transaction on the next run. Stable zip installs require an exact package
    manifest, saved helpers preserve their existing stable/local-development
    channel, and prefixes must remain on one fixed physical volume rather than
    a network, removable, or `SUBST` mapping. PowerShell profile integration
    retains the original file identity while publishing and preserves supported
    DACLs, attributes, timestamps, encoding, BOM, and newline form; alternate
    streams, special storage attributes, hard links, and ambiguous managed
    markers fail before package mutation. Installed-version discovery no longer
    uses a predictable temporary path or unbounded redirected output: stdout
    and stderr share a 4 KiB limit and the child has a 15-second deadline.
  - Windows pending updates now bind the copied helper as well as every staged
    file by size and SHA-256 and retain the complete root-down object handle
    chain through launch and consumption. Cleanup marks only revalidated held
    objects for deletion, legacy-journal recovery requires an exact journaled
    tree, backup capacity is checked in aggregate before destination mutation,
    and copies use bounded streaming buffers. A five-minute grace and bounded
    handoff-timeout counter prevent a permanently held run lock from trapping
    every future launch; nonregular pending paths are never adopted as
    quarantine evidence. Self-update also now rejects a v2.36.0-or-newer
    release archive that omits its mandatory inner package manifest instead of
    silently accepting it under the pre-v2.36 compatibility path.
    The saved PowerShell uninstaller now recognizes that same schema-2 pending
    record, validates its exact typed and bounded helper/file identities, and
    tolerates a named artifact already disappearing during controlled removal
    or crash recovery. A legitimate staged update therefore no longer blocks
    uninstall, while schema-1, unknown-field, and unmanaged-path records still
    fail closed.
  - The Windows remote-command watcher now creates, reads, bounds-checks, and
    truncates its command file through verified no-reparse handles, so a path
    swap cannot redirect command data to another file.
  - Pane input now crosses a bounded, two-lane worker instead of writing from
    the window thread. User input and terminal protocol replies have separate
    message/byte budgets; replies preempt bulk user data at bounded write
    boundaries. Every enqueue reports `queued`, `read_only`, `backpressured`,
    `oversize`, or `failed`: control RPCs map those outcomes to distinct
    protocol errors, while local input surfaces a throttled notification and a
    failed transport closes the pane. Local clipboard/PRIMARY/drop pastes over
    4 MiB are rejected visibly rather than silently truncated.
  - Lua side effects now retain one process-wide ordered FIFO across startup
    and every event hook. The queue is capped at 1,024 commands and 8 MiB of
    pending `send_text` data, processes at most 16 commands/1 MiB per event-loop
    turn, and retries an unchanged backpressured head on a 10–250 ms deadline.
    A send latches its pane on first attempt so a focus change cannot reroute
    delayed text. Lua side-effect calls now return whether the first bounded
    queue admitted them; the 1 MiB per-send and aggregate limits remain
    fail-soft with warnings, and admission is not a promise of PTY delivery.
    Lua registries now expose the same boolean admission contract, accept only
    the nine events Kettle can emit, and bound callbacks, menu items, URL
    handlers, labels, handler names, and patterns before copying Lua strings
    into Rust-owned storage.
  - Legacy `remote.cmd` transport now uses one shared advisory lock for sender
    append and receiver claim, with a 1 MiB aggregate cap and deadline retries
    for lock contention or pane backpressure. Claimed batches are capped at
    1,024 operations and rejected whole on overflow; unknown-line diagnostics
    are coalesced. New `--remote-send` writers use one reversible
    `send-text-json <JSON_STRING>` line, preserving literal backslash+n, LF,
    CR, NUL, and command-looking text exactly; malformed frames are counted
    without logging payloads, and the legacy lossy `send-text` receiver remains
    compatible with direct writers. A claimed batch is truncated before
    ordered in-memory dispatch, making the file transport explicitly
    at-most-once; callers that need an acknowledged result should use
    `kettle ctl`, whose response distinguishes read-only, busy, bad-parameter,
    and failed-transport outcomes.
  - Scrollback vi mode now delegates cursor motion, viewport following,
    selection, reflow, and history eviction to the native
    `alacritty_terminal` vi state. The UI owns only pane/modal routing and
    visual-mode intent, preventing a second cursor/selection model from
    drifting from the terminal grid.
  - OSC 133 prompt marks now use monotonic grid row identities built from the
    vendored grid's `history_origin`. Ring eviction, scrollback-limit changes,
    reset, and reflow can no longer retarget a saved prompt mark to unrelated
    text; prompt navigation prunes only genuinely evicted rows and ignores the
    alternate screen.
  - Inline and placeholder image placements now use that same monotonic
    document-row domain instead of reusable capped-history coordinates.
    Evicted half-open placement spans are pruned after normal parsing,
    synchronized-update timeout flushes, and history-changing resize; renderer
    snapshots carry the origin; and scrolled placeholder cells no longer apply
    `display_offset` twice. Native vi selections are now removed from internal
    state once their complete range leaves retained history, so menus and the
    control API cannot report an unmaterializable stale selection.
  - Sixel, Kitty, and iTerm2 graphics now follow the active terminal screen
    buffer. Mode 47 preserves alternate graphics on entry and exit; mode 1047
    preserves them on entry and clears them on exit; mode 1049 saves/restores
    the cursor, clears alternate graphics on entry, and preserves them on exit.
    Kitty image ids, virtual placements, animations, relatives, and partial
    uploads are isolated between buffers. ED 2 clears only the active buffer
    and RIS clears both. An authoritative engine-owned, ordered 256-event
    journal reports these committed mutations and page-region scrolls; adjacent
    compatible scrolls coalesce without losing their full document-row delta,
    and overflow clears both graphics buffers before resynchronizing to the
    engine's active screen.
  - DEC 2026 synchronized updates now keep text and inline graphics in exact
    wire order. A bounded, out-of-band VTE marker defers Sixel, Kitty, and
    iTerm2 controls before their decoders can mutate state, then replays each
    control against the cursor and screen buffer active at that byte offset.
    Plain terminal bytes and concurrent row, pixel, or DPI resizes now share
    the graphics ordering gate, so resize cannot split a committed text scroll
    from its corresponding graphics-journal mutation.
    Natural close, timeout, EOF, and nested synchronized regions publish one
    atomic result; no graphics mutation or redraw becomes visible mid-region.
    Marker overflow or an inconsistent marker sequence fails closed by clearing
    both graphics buffers before resynchronizing to the engine's active screen.
  - Inline graphics now follow partial DECSTBM scrolling. Placements wholly
    inside the page margins move with text and permanently crop both their
    destination and source range when the move crosses a margin; placements
    already crossing a margin stay fixed. Full-screen/top-anchored scrolling
    retains exact document ids even when many engine scrolls coalesce. Cropped
    Kitty fragments retain their original placement parameters and composed
    source range, so natural-size and one-axis-auto geometry can follow a later
    monitor/DPI change without restoring discarded pixels or moving the
    post-scroll anchor.
  - Column reflow now fails safe for inline graphics: regular and relative
    placements are cleared instead of being attached to guessed replacement
    rows, while Kitty Unicode-placeholder prototypes and animations survive
    because their locations remain owned by reflowed grid cells.
  - Inline image instances are now CPU-clipped to the intersection of the pane
    interior and exact terminal grid. Destination and source UV rectangles are
    cropped by the same fractions, so negative or oversized placements cannot
    bleed into padding, pane titlebars, borders, sibling panes, or window
    chrome without squashing the visible pixels. Fully outside, degenerate, or
    non-finite placements are skipped, including every placement in a zero-line
    viewport. Wallpaper remains intentionally unclipped, and zero-sized skipped
    slots preserve indexed same-texture batching.
  - Bottom-positioned pane titlebars no longer leave the terminal grid shifted
    down by one title strip or clip the final rows early. Grid and legacy text,
    cursor/selection, links, search hints/highlights, inline images, IME paint,
    pointer hit testing, mouse reporting, and the native IME anchor now share
    one title-position-aware grid origin.
  - Session restore now preflights the complete workspace before creating
    native windows, renderers, or PTYs. Restore is limited to 16 non-empty
    windows, 256 panes, 32 Mi pixels per surface, and 64 Mi pixels in
    aggregate; saved geometry is clamped to the live monitor layout and applied
    before the first restored window is revealed.
  - Kitty graphics deletion now implements visible, image/placement, cursor,
    cell, cell-plus-z, id-range, column, row, z-index, and frame selectors with
    lowercase retain-data versus uppercase free-data semantics. A delete is
    applied before later APCs from the same PTY read, so delete-and-replace is
    ordered. Placement rendering now honors source crop (`x/y/w/h`),
    destination cells (`c/r`), pixel offsets (`X/Y`), aspect-preserving
    one-axis sizing, and `C=1` cursor suppression across DPI changes.
  - Managed-recording retention now removes the exact locked private file
    instead of resolving its pathname again. Windows marks the verified kernel
    object for deletion through a reopened handle; Unix unlinks the verified
    leaf relative to its held parent directory.
  - Linux self-update verification and extraction now consume one bounded
    in-memory archive buffer, closing the remaining same-inode overwrite gap.
    Unix control connections also keep a stable nonblocking mode and serialize
    cloned deadline-aware writers without exposing transient `O_NONBLOCK`
    changes to readers.
  - Linux release binaries now build on Ubuntu 22.04 and a release-workflow
    `readelf` gate rejects requirements newer than glibc 2.35. The documented
    one-line installer floor therefore matches the published binary ABI instead
    of silently inheriting glibc 2.39 from the packaging runner.
  - Windows performance evidence now requires one exact physical connection
    per resolved monitor. Miracast and indirect outputs are rejected, WMI and
    CCD identities cannot be mixed, same-model monitors are distinguished by
    instance identity, and the scorer independently reconstructs the signed
    monitor/connection mapping.
  - Public performance evidence now tokenizes numeric and Boolean
    display-routing identifiers, hardware IDs, and EDID fingerprints before
    scalar passthrough. Tokens use an unpublished cryptographically random
    per-bundle HMAC key, so low-entropy connector identifiers cannot be
    recovered by enumerating values against the published run id. The public
    evidence index is schema 2. Credential-like property names are normalized
    across casing and separator variants and redact scalar, object, and array
    values before generic hash handling. Publication accepts only the reviewed
    fixed benchmark filenames, preventing an unexpected JSON leaf from
    disclosing credential or user text through its name.

  ### Changed
  - GPU backend selection is deterministic and shared by live rendering,
    screenshots, `--gpu-info`, tests, and recovery. Default unpinned Windows
    startup probes DX12 once and does not initialize Vulkan unless DX12 is
    unavailable; macOS prefers Metal and Linux prefers Vulkan. Live instances
    retain winit's event-loop-owned display handle required for the Linux
    Wayland GLES fallback without retaining a closed window.
  - `just split-titlebar-smoke` now exercises real top- and bottom-title
    windows, checks title-position-aware grid edges, and verifies exact
    configured focused/transmit, receiving, and inactive colors in PNG
    interiors selected outside text, icon, and pane-accent pixels.
  - `just agent-tui-smoke` and Windows `just agent-tui-wsl-smoke` now build and
    run the exact current-checkout release executable reported by Cargo,
    honoring `CARGO_TARGET_DIR`, configured target triples, and the platform
    executable suffix instead of assuming `target/release` or exercising a
    stale `kettle` from `PATH`.
  - Agent-client compatibility documentation now distinguishes terminal input
    transport from client-owned image attachment. Local smokes cover
    version/help and terminal primitives, not interactive composer shortcuts;
    Kettle's temporary-PNG path paste and Codex's help-verified `--image` /
    `-i` initial-attachment option are the documented stable fallbacks.
  - tmux SIXEL guidance and live coverage now require tmux 3.4 or newer plus a
    build that actually advertises DA1 feature code `4`; tmux 3.6's
    `#{sixel_support}` value is cross-checked when available. The live smoke
    declares the outer `sixel` feature only after that check, verifies runtime
    pixel cell geometry, and distinguishes a rendered 24x12 image from tmux's
    zero-geometry `SIXEL IMAGE (WxH)` fallback without reporting skips as
    passes.

  ### Performance
  - Context-menu highlight redraws now reuse the last validated pane snapshots
    and skip terminal event maintenance, terminal mutex acquisition, viewport
    copies, and glyphon text preparation. The event-loop blink scheduler reads
    the same validated snapshot, and opening a menu now ends any pointer gesture
    that could mutate selection or scrollback behind it. Reuse is one-shot and
    fails closed when pane order, output generation, columns, or rows differ;
    menu text is re-prepared when content, enabled color, theme color, anchor,
    or scroll window changes. A highlighted-row change still rebuilds the
    frame's quad batches, but reuses all retained text vertices. Stable
    block-cursor glyph vertices are retained too and are refreshed on glyph,
    geometry, style, font, or shared-atlas damage.
  - Full UI-only frames no longer lock every pane to compare scrollback depth.
    Tab activity and `scroll-on-output` use the PTY reader's lock-free output
    generation, which also detects in-place and alternate-screen output that
    the former history-growth proxy missed. The grid lock is now taken only
    when `scroll-on-output` is enabled and output actually changed.
  - Remote-context polling now coalesces all panes/windows through one bounded
    background scanner; process enumeration never blocks the event loop and
    idle windows receive explicit title/redraw delivery. Linux follows every
    bounded task's child edges, reuses BFS scratch, reads cwd only for the
    selected local foreground pid, and enforces byte/node/task/argv/time caps.
    Partial scans do not replace the last complete state; direct/nested remote
    clients suppress host cwd, and Split/Duplicate uses the cached foreground
    shell rather than scanning on the input path.
  - Context-menu width now uses Unicode display columns and truncates only at
    grapheme boundaries. Rendering, pointer hit-testing, and agent geometry
    reporting share the same clamped panel dimensions, and partially clipped
    bottom rows remain non-interactive. Scroll indicators now disappear when
    the visible suffix actually reaches the final row.
  - Context-menu pointer hit-testing now streams row kinds without allocating a
    temporary vector, and wheel-scroll clamping finds the final fitting suffix
    in one reverse pass. Both paths remain linear even for the 512-entry theme
    submenu.

  ### Added
  - Expanded the Windows performance harness from four terminals to Kettle,
    Windows Terminal, Alacritty, WezTerm, Rio, and Tabby. Executable discovery
    is centralized; every probe records binary, version, configuration, run,
    schedule, and source/build provenance.
  - Added explicit `release` and `smoke` harness modes. Release mode pins all
    probes and sample counts and rejects skips, unidentified displays, custom
    Kettle config, or non-release toolchains. Manifest-only smoke records
    discovery and topology without presenting it as live benchmark evidence.
  - Added shared immutable release acquisition and scoring contracts. Release
    evidence now pins terminal order, seed, menu block size, cooldown, window
    geometry, transition count, and every score threshold; producer and scorer
    reject caller deviations independently.
  - Added an append-only Windows comparator campaign. Official asset URLs,
    archive and expanded-tree identities, versions, executable
    bytes/hashes/signatures, and confirmed/advisory roles are reviewed in one
    tracked manifest. Setup is networked only before measurement; release runs
    revalidate it offline, retain every confirmed-tree file handle, reject
    executable overrides, and bind both schema-4 manifests to the same
    campaign. Windows Terminal additionally requires the exact installed Appx
    identity. Release acquisition resolves and read-leases that package's real
    `WindowsTerminal.exe`; the ambient `wt.exe` app-execution alias is accepted
    only in smoke mode, so a shadow launcher or unrelated running host cannot
    satisfy release evidence.
  - Added run-local isolated configs for Kettle, Alacritty, WezTerm, Rio, and
    Tabby with a common font, palette, scrollback, padding, opacity, cursor, and
    disabled effects/update/telemetry settings. Windows Terminal is recorded as
    advisory because it has no supported per-launch settings-file switch.
  - Added deterministic seeded Williams-balanced schedules for startup,
    idle/fresh-memory, latency blocks, and throughput rounds.
  - Added a controlled startup-ready protocol: an atomic GO marker, flushed
    truecolor paint, `CSI 5 n` → `CSI 0 n` parser round trip, atomic READY
    marker, and validated painted ROI now define the startup boundary.
    Process-tree attribution is deferred until after that endpoint and recorded
    separately so CIM latency cannot inflate startup.
  - Added a locked, unpredictable throughput GO capability. Workloads cannot
    begin before the exact attributed window is sized and focused, and each
    observation retains client-pixel, console-cell, and handshake evidence.
  - Added high-DPI context-menu latency probes for both the common 1280×800
    comparator window and a native-display window derived from the active
    monitor. Both capture only the context-menu ROI over 200 blocked samples.
  - Added a two-screen physical-monitor transition probe that measures
    capturable `ui_geometry` recovery with Kettle's context menu closed and
    open and invalidates results if topology changes.
  - Added continuous Windows `DisplaySettingsChanged` monitoring plus signed
    topology snapshots at every probe boundary. A switch-away-and-back is now
    retained as an invalidating event even when start and end displays match.
  - Added raw paired release statistics: deterministic 10,000-resample 90%
    bootstrap intervals, practical margins, Theil-Sen/peak-to-peak drift gates,
    confirmed isolated-peer policy, strict per-round throughput margins, and
    mandatory matched current-versus-baseline non-inferiority. The deterministic
    bootstrap hot loop uses a pinned in-memory C# kernel with cross-engine
    fixtures that preserve the prior algorithm byte-for-byte.
  - Added JSON-only evidence sanitization. Public bundles replace local paths,
    commands, monitor serials, and device identifiers with bundle-secret HMAC
    tokens and never copy raw logs, `.dat`, screenshots, or artifact
    directories.
    Flat bounded staging, retained identity checks, reparse rejection, exact
    cleanup, and atomic publication prevent path-swap escape.
  - Added complete GUI-free performance-harness integration fixtures under
    PowerShell 7 and Windows PowerShell 5.1, including positive schema-4
    release evidence and tampered baseline/provenance/geometry cases.
  - Added bounded no-follow evidence snapshots with strict BOM-free UTF-8,
    JSON depth/node/byte limits, duplicate-key rejection, retained identity
    locks, and deterministic hashes. Release runs also hash and read-lock every
    production harness script and require current/baseline harness identity.
  - Replaced the predictable throughput sample-file handoff with a bounded,
    nonce-authenticated, current-user named-pipe protocol with finite timeouts
    and client-process ancestry validation.
  - Added a committed Nix flake lock and always-on PR/main CI that checks the
    locked flake and builds the x86_64 Linux package.
  - Added a locked validation-only workspace for every patched vendored crate.
    Local strict validation and native CI now run their retained unit targets,
    doctests, warnings-denied clippy, RustSec audit, and cargo-deny policy
    directly; Dependabot excludes immutable vendored snapshots so updates
    cannot silently discard reviewed local fixes.

  ### Benchmark, packaging, and release hardening
  - The Windows installer smoke no longer aborts before reaching any product
    logic when it is started from a PowerShell 7 session. Windows PowerShell
    cannot load PowerShell 7's .NET-Core build of the modules whose names it
    shares, and the inherited `PSModulePath` put those roots ahead of the system
    one, so autoloading `Microsoft.PowerShell.Security` for `Get-Acl` failed
    outright. The check now keeps only module roots its own edition can load.
  - The Windows `.ico` packaging smoke now runs. Its recipe body was inline
    PowerShell, and a plain `just` recipe evaluates each line in a separate
    shell, so the variable holding the icon path was already gone by the line
    that read it — the check failed on every Windows invocation and took the
    full local gate down with it. The ICONDIR parsing moved to
    `scripts/check-windows-ico.ps1`, matching how every other Windows recipe
    here calls a script, with the resolution floor as a parameter.
  - Windows performance runs now retain a unique active `WmiMonitorID` as the
    preferred physical-display identity and use a versioned, fail-closed CCD
    fallback when WMI is absent or ambiguous. The fallback reads only the exact
    registry EDID named by the active device-interface path, validates every
    block plus CCD manufacturer/product agreement, rejects ambiguity and
    tampering, and never searches stale monitor instances by model.
  - Homebrew's macOS install now writes bundled documentation through a
    platform-independent share path instead of a Linux-only local variable.
  - Nix outputs no longer apply Linux-only libraries and `patchelf` fixups on
    Darwin, advertise the x86_64 Darwin platform dropped by nixpkgs unstable,
    or omit the shell/process tools required by sandboxed PTY tests.
  - Nix Linux packages now include winit's dynamically loaded Xcursor and
    XInput2 libraries without replacing Nix's glibc/libgcc RUNPATH. A
    clean-environment Xvfb and Mesa software-Vulkan launch check verifies the
    installed binary creates a visible rendered window.
  - Nix Linux packages now install the Desktop Entry, scalable and raster
    hicolor icons, man page, and all shell-integration snippets beneath
    `$out/share`; Darwin outputs remain free of Linux desktop assets. A
    derivation-level content gate byte-compares the complete installed share
    tree with its checked-in sources.
  - Windows comparator discovery now recognizes Rio's current Winget MSI
    layout (`Program Files\Rio\rio.exe`) as well as its legacy `bin` layout.
  - Performance runs now force a separate Kettle process and stop attributing
    unrelated Windows Terminal tabs to one benchmark window's memory/idle CPU.
    Workload child processes are also excluded from fresh and post-flood
    terminal-tree memory/CPU accounting.
  - Windows benchmark input/capture now opts into per-monitor-v2 DPI awareness,
    keeping physical Kettle geometry, pointer targets, and PrintWindow pixels
    aligned on scaled 4K/5K displays. Startup and menu polling use bounded ROIs
    rather than transferring a full high-resolution frame on every poll.
  - Throughput now measures console-write start through the terminal's DSR
    parser-drain response. Writer acceptance remains diagnostic and cannot make
    a backlogged terminal look faster.
  - Release scoring now fails closed on missing/inconsistent raw samples,
    censored evidence beyond policy, uncertain bootstrap intervals, drift,
    altered payloads, non-UTF-8 output, dirty/unknown source identity, changed
    configs, unstable display topology, or incomplete native-display and
    monitor-transition evidence. Tabby's command probes retain their bounded
    native confirmation and byte-verified, read-locked one-use wrapper.
  - Public-evidence publication now retains the exact staging object through
    its handle-relative rename, revalidates the exact flat set, alternate data
    streams, and hashes after the move, and rolls the same object back if any
    post-publication invariant fails.
  - Release signing now rejects a secret whose Ed25519 public key differs from
    the checked-in production trust root and verifies the manifest with that
    pinned key, preventing a misconfigured or rotated secret from publishing
    updates that every shipped client would reject.
  - Release archives are now reopened and validated through one bounded,
    manifest-aware extractor in the protected finalizer. It rejects link,
    device, sparse, PAX override, traversal, alias, prefix-conflict,
    permission, count, and expanded-size hazards without a raw `tar` or
    `Expand-Archive` pass, and the manifest generator shares the updater's
    256 MiB artifact ceiling.
  - The Linux one-line installer now bounds every response, requires
    Ed25519 verification for modern releases without a checksum downgrade,
    binds the canonical manifest target/name/size/hash tuple, and preflights
    the authenticated archive before extracting only regular files and
    directories into a private root. Legacy releases without an exact
    same-origin checksum are refused rather than executed unauthenticated.
  - Release builds now use pinned runner images, an exact Rust toolchain, and
    `--locked` Cargo commands. The protected signer is read-only and hands its
    exact finalized set to a separate publisher that has no signing secret.
    Before publication that job re-verifies the signature, canonical
    sidecars, archives, package metadata, draft identity/state, and the exact
    14 local byte lengths and streamed SHA-256 digests reported by GitHub,
    closing the prior shared-credential and name-and-size-only gaps.

## [2.42.0] — 2026-07-24

  ### Added
  - **Paste a screenshot into a pane.** Copying a *file* populates `CF_HDROP` /
    `text/uri-list`, which v2.38.0's `paste-files` already turns into a
    shell-quoted path. Capturing a *screenshot* (Win+Shift+S, Snipping Tool,
    macOS Cmd+Shift+4, GNOME Screenshot) does something different: it puts a raw
    bitmap on the clipboard with no file and no text behind it, so neither the
    file branch nor the text fallback could see it and the paste did nothing at
    all. Kettle now materializes the bitmap as a PNG and pastes its path through
    the same `format_paths_for_paste` pipeline — shell-aware quoting and WSL
    `C:\` → `/mnt/c` translation included. Handing a CLI agent a path also avoids
    depending on the agent's own clipboard-bitmap decoding, which is unreliable
    on native Windows. New `paste-images` key (on by default, `paste-image`
    alias); a copied file still wins when both are present.

    Temp files use owner-only permissions inside a per-process scratch
    directory, are bounded in count and total bytes so a paste loop cannot fill
    the disk, reject malformed clipboard geometry before allocating, and are
    deleted when kettle exits — a directory orphaned by a crash is reclaimed on
    the next launch. Windows uses per-user Local App Data rather than a process
    temp directory that may grant sandbox principals delete-child access; other
    platforms use the OS temp directory. Because the files are session-scoped,
    a path captured in an old transcript will not resolve after kettle closes.

  ### Changed
  - `arboard` is now built with its `image-data` feature. The comment that
    disabled it claimed the feature "transitively pulls `image` with default
    features (= every format incl. avif/rav1e)" — that was wrong: arboard pins
    `default-features = false` on `image` for every target and requests only
    `png`+`bmp` on Windows, `png` on Linux, `tiff` on macOS. Since kettle-render
    already builds `image` with png/jpeg/webp/bmp/gif, the Windows and Linux
    dependency graphs gain **no new crate**; macOS adds `tiff` + `fax`. Verified
    with `cargo tree --target x86_64-pc-windows-msvc`. Kettle still never writes
    an image *to* the clipboard.

## [2.41.0] — 2026-07-24

Touchpad scrolling fix. Reported from a Windows 11 Surface Book 3: two-finger
scroll gestures did nothing at all in kettle. The cause was in kettle, not the
driver — and the wheel path around it turned out to have several more defects,
all fixed here and now covered by unit and live end-to-end guards.

  ### Fixed
  - **Precision-touchpad and high-resolution-wheel scrolling did nothing.**
    Windows delivers touchpad scrolling as `WM_MOUSEWHEEL` with deltas far
    smaller than `WHEEL_DELTA` (120); MSDN requires applications to accumulate
    them. winit divides by 120 and on Windows always reports `LineDelta` (never
    `PixelDelta`), so one gesture arrives as a stream of ~0.07–0.3 notch
    events. `wheel_lines` did `y.round() * 3.0` per event and the handler
    returned early on zero, so **every event rounded to nothing and the residue
    was discarded** — touchpad scrolling was not slow, it was completely dead.
    Only a violent flick packing ≥ 60/120 into a single message survived.
    Replaced with `WheelAccum`, which carries the fraction across events.
    Whole-notch feel is unchanged (3 lines per notch, scaled by
    `scroll-multiplier`); sub-notch motion now accumulates instead of vanishing.
  - **`scroll-multiplier = 0.1` disabled the mouse wheel outright.** A legal,
    documented, clamp-passing value: one notch became `1.0 × 3.0 × 0.1 = 0.3`,
    which rounded to zero. Fixed by the same accumulator.
  - **Slow macOS/Wayland trackpad scrolling was dropped.** Those backends emit
    small `PixelDelta`s; anything under `10/multiplier` px rounded away.
  - **Horizontal scrolling was discarded entirely.** The handler matched
    `LineDelta(_, y)` and ignored `PixelDelta.x`, so two-finger sideways swipes
    and tilt-wheels did nothing. Horizontal motion now reports as xterm buttons
    6/7 (encoded 66/67) to mouse-tracking applications, and cycles tabs when the
    pointer is over the tab bar.
  - **A read-only pane running a mouse-tracking TUI could not be scrolled.**
    `send_mouse` returns `false` for a read-only pane specifically so callers
    fall through to local handling, but the wheel handler discarded that return
    — consuming the event, reporting nothing, and leaving the pane frozen. It
    now falls through to local scrollback, matching the documented contract that
    selection and scrollback keep working when input is disabled.
  - **Alternate scroll ignored DEC private mode 1007.** `alternate_scroll_key`
    gated on `ALT_SCREEN` alone, so an application that turned alternate scroll
    off with `CSI ?1007 l` still received synthesized arrow keys. Now gated on
    `ALTERNATE_SCROLL` as well, matching xterm and upstream Alacritty. The flag
    is set by default, so default behavior is unchanged.
  - **The ctl/agent wheel path had drifted from the real one.** `ctl_mouse_wheel`
    was a partial copy of the dispatch ladder, missing the context-menu,
    settings, modal-swallow and Ctrl+zoom stages — automation exercised a
    different terminal than a human did. Both paths now share one
    `dispatch_wheel`.

  ### Added
  - `send_mouse` accepts `wheel_delta`: a signed **raw** wheel-detent count with
    fractions allowed (e.g. `0.08` per event), alongside the existing
    pre-quantized integer `wheel_lines`. This is the only synthetic path that
    runs the real accumulator, which is why the touchpad defect had no coverage
    before — `wheel_lines` enters downstream of the conversion that was broken.
  - `just touchpad-scroll-smoke`: a live end-to-end scenario driving 60
    sub-detent events through ctl and asserting the viewport actually moves,
    returns to the bottom on the mirrored gesture, and still scrolls exactly 3
    lines for one whole detent. Wired into `gauntlet-full`.
  - `KETTLE_SMOKE_EXTRA_CONFIG`: extra config appended to every generated live
    smoke config. Each scenario writes a minimal config and so inherits none of
    the developer's real settings — including a pinned GPU. On a dual-GPU laptop
    that silently drops the harness onto the integrated GPU, where a driver
    fault can abort startup before the control server comes up. Unset in CI.

  ### Changed
  - Discrete wheel actions (tab cycling, Ctrl+wheel font zoom, context-menu and
    settings rows) now advance once per **physical detent** rather than per
    scaled line. Previously they were driven by the multiplier-scaled line
    count; with sub-detent accumulation that would have moved three tabs per
    detent. Ctrl+wheel with zoom enabled also swallows mid-gesture events, so a
    partial detent can no longer scroll the pane instead of zooming it.

## [2.40.0] — 2026-07-23

Tab tear-off UX overhaul, driven by a recorded frame-by-frame session of the
real gesture on Ubuntu X11/GNOME (xdotool-driven drags, ffmpeg capture, every
attempt analyzed). The session caught two functional breaks on X11 and a set
of missing affordances; all are fixed here, with the gesture now covered by a
deterministic two-tier live smoke.

  ### Fixed
  - **X11: torn windows could freeze mid-air.** When the WM silently ignored
    the `_NET_WM_MOVERESIZE` handoff (a race against the just-created,
    not-yet-mapped window), the manual-follow fallback only tracked while the
    pointer stayed inside the capture-holding source window — leaving the
    torn window stranded the moment the pointer crossed its border. A new
    `about_to_wait` rescue tick polls the real pointer (x11rb
    `QueryPointer`, the X11 counterpart of the Windows `GetCursorPos` drift
    correction), demotes a silent handoff on travel-without-`Moved` evidence,
    and carries the torn window itself.
  - **X11: re-docking a torn window rarely latched.** The dock hit-test
    approximated the pointer as `frame + grab`, but the WM anchors its move
    grab at the button-press position while `grab` is computed at tear time —
    measured 55-86px apart under Mutter, more than the whole tab band. The
    hit-test now runs from the live cursor wherever a query source exists
    (Windows/X11), and commit-time revalidation distinguishes an Esc-cancel
    from a real drop by physical button state (the X11 `QueryPointer` button
    mask — the same tell the Windows release path reads), since Esc moves
    the frame but never the pointer.
  - `KETTLE_TEAR_MANUAL_FOLLOW=1` (diagnostics) forces the manual-follow
    path, making the otherwise race-dependent fallback testable on demand.

  ### Changed
  - **Cross-platform dock-target highlight.** The latched target strip now
    paints an accent wash plus a pane-edge border (with the insertion marker
    widened to 3px and given square end-caps), so a Linux/macOS drop target
    is no longer signalled by a bare 2px line — previously the only
    non-Windows cue, and invisible in the recorded session. The Windows
    torn-window translucency remains, additively.
  - **Pre-tear ghost escalation.** The drag ghost's shadow grows and its body
    fades as the cursor approaches the tear threshold (`TabBar::tear_lift`,
    0→1 over the band-to-threshold distance), telegraphing "release will
    tear" instead of springing a new window unannounced.
  - **Grab/Grabbing cursor** through the whole tab-drag gesture, first in the
    cursor-priority chain so transiting another tab's close-✕ or a split seam
    can't flicker the icon mid-drag.
  - Tab-drag paint constants consolidated into `kettle_render::tab_drag`
    (ghost opacities/offsets, marker width/caps, highlight alphas), the same
    single-home pattern as `kettle_render::menu`; `ui_geometry` now reports
    `tear_lift`, `dock_highlighted`, and the band rect for smoke assertions.

  ### Added
  - `just tearoff-smoke`: a ctl tier (mouseless `move_tab_to_new_window`
    tear + `tab_moved` event + diagnostics keys) plus an X11 live tier
    driving real xdotool input through tear → follow → re-dock merge → Esc
    cancel, once per carry path (native handoff, forced manual-follow).
  - `docs/ARCHITECTURE.md` detachable-tabs section updated for the live-
    pointer tracking, rescue tick, and highlight; `CONTRIBUTING.md`'s
    "Releasing" section rewritten to match the actual two-script,
    PR-mediated flow (`release.sh` on a branch → PR → `tag-release.sh`).

## [2.39.0] — 2026-07-23

A whole-repository audit (multi-agent: per-crate plus cross-cutting security,
performance, terminal-semantics, UX, compatibility, architecture, and docs
lanes; every finding adversarially re-verified, and the security-critical
fixes verified a second time against their diffs). 59 findings confirmed, 52
shipped here; the remainder — two god-file splits and five cross-crate
follow-ups — are tracked in
[docs/AUDIT-DEFERRED.md](../AUDIT-DEFERRED.md). Full write-up in
[docs/AUDIT-2026-07-23-FULL.md](../AUDIT-2026-07-23-FULL.md).

  ### Fixed
  - **Windows control-plane peer authentication was a no-op.**
    `CtlStream::peer_is_same_user()` returned `Ok(true)` unconditionally on
    Windows, contradicting its own contract and the "verifies same-user peers"
    guarantee. It now checks the named-pipe client via
    `GetNamedPipeClientProcessId` → open process → `GetTokenInformation` →
    `EqualSid` against this process's token, with RAII handle ownership and
    fail-closed error handling.
  - **Remote-context pane titles bypassed the bidi/control-char sanitizer.**
    SSH/container titles built from scanned process argv reached the pane
    title, OS window title, and accessibility tree unfiltered; they now route
    through `sanitize_title()` like every other title path.
  - **`ctl screenshot` could be steered into an arbitrary-file overwrite.** The
    live screenshot now opens its output with `create_new` (`O_EXCL` /
    `CREATE_NEW`), so a symlink or file planted at the destination during the
    capture window makes the write fail atomically instead of being followed;
    screenshots and the remote-command file are also created owner-only.
  - **A malformed MCP line could crash the server** via unbounded JSON nesting;
    an explicit, string/escape-aware depth guard (limit 64) now rejects
    over-nested input before the parser runs.
  - **The `curl | sh` bootstrap installer could be silently downgraded.**
    Verification against the Ed25519-signed release manifest no longer falls
    back to the forgeable same-origin checksum when a capable openssl simply
    can't fetch the manifest for a ≥ v2.35.0 release — that now fails closed;
    only a genuinely older release or an openssl without Ed25519 takes the
    weaker path. The update-archive verify/extract TOCTOU is closed outright on
    Windows and narrowed on Linux (verify and extract share one handle).
  - **Windows piped stdin was silently discarded.**
    `attach_parent_console_if_needed` unconditionally replaced
    `STD_INPUT_HANDLE` with `CONIN$`, defeating `is_terminal()` and hanging
    `echo y | kettle update`; stdin is now guarded exactly as stdout/stderr
    already were. The `kettle.com` launcher also learned `--new-process`, so it
    no longer blocks the shell for that flag.
  - `atomic_create_new` could leak its staged temp file and report success as
    failure; state/lock acquisition gained bounded/timeout variants so a stuck
    holder no longer wedges every caller. The ctl discovery registry and
    AF_UNIX path length gained the ownership / length guards their Unix
    siblings already had; config load, the legacy scrollback `search` API, and
    the OSC 52 clipboard-read reply gained the bounds their symmetric paths
    enforce. Cross-chunk ANSI-strip state, zoom/font-size desync, and vi-mode
    selection reclamping after reflow were corrected.
  - **A symlinked config file is read on startup again** (regression caught in
    review before release): the hardened `O_NOFOLLOW` config reader is now
    reached through the same symlink resolution the editor uses, so a
    `~/.config/kettle/config` managed by GNU Stow / chezmoi / a manual `ln -s`
    loads instead of silently falling back to defaults.

  ### Changed
  - **Context-menu, settings, and search-family overlay text buffers stopped
    reshaping every frame.** They now carry the same text-equality reshape gate
    the tab bar and quick-select hints use, so an open overlay no longer
    re-shapes every label on each blink-driven redraw. The glyph-atlas slot
    cache and quad-buffer growth gained eviction / checked-arithmetic bounds.
  - **Ten config knobs that were parsed but never read** were removed (unknown
    keys still warn-and-ignore, so existing configs never error) or wired up to
    the behaviour they name.
  - `just gauntlet` now actually mirrors the CI gate; CI gained the Windows
    CLI-rendering smokes and the split-titlebar live smoke runs on Windows; the
    `bench.ps1` zero-sample race was fixed. Six `TERMINATOR-*-DESIGN.md` "design
    only" headers now state the version each feature shipped in, and the
    PowerShell shell-integration snippet that reproduced an already-fixed
    infinite-prompt loop, the broadcast keybind, the animated-background limits,
    and the workspace test counts were corrected against the code.

## [2.38.2] — 2026-07-23

  ### Fixed
  - **Split-pane titlebars no longer render a tofu box for emoji or symbol
    glyphs on any platform.** The per-pane titlebar shaped its label with
    cosmic-text's no-fallback Basic mode, so any codepoint outside the bundled
    JetBrainsMono Nerd Font — the leading status glyph agents such as Claude
    Code put in OSC 0/2 titles, an emoji `agent-badge`, or the `icon-bell`
    `U+1F514` indicator — rasterized the font's `.notdef` box while the tab
    bar rendered the identical string correctly. The titlebar, the resize
    overlay chip, and quick-select hint labels now shape with Advanced mode
    like every other chrome surface, walking the platform font-fallback
    cascade (Segoe UI Emoji/Symbol on Windows, Noto/DejaVu via fontconfig on
    Linux). Regression coverage: a cross-platform shaping-superset invariant,
    a Windows-gated end-to-end glyph-resolution test, and a source guard that
    keeps no-fallback shaping out of the render crate. Quick-select hint
    labels also gained the same text-equality reshape gate the other chrome
    buffers use, so an open overlay no longer re-shapes every label on each
    blink-driven redraw.
  - **Disabling ligatures no longer silently disables font fallback for
    terminal text.** Turning the ligature flag off (any `liga`/`clig`/`calt`/
    `dlig` toggle set to 0 via `font-feature`, with no other features active)
    dropped the whole pane to the same no-fallback shaping mode, tofu-boxing
    CJK/emoji/symbol fallback glyphs for those users. The ligature toggle was
    already fully expressed as OpenType features (`liga/clig/calt/dlig = 0`),
    so shaping now always uses the fallback-aware path; per-line shaping
    caches keep the cost bounded to rows whose content changed.

## [2.38.1] — 2026-07-22

  ### Fixed
  - Linux stable self-updates now preserve the source installer's absolute
    256x256 PNG desktop icon instead of switching the launcher to the SVG.
    This keeps GNOME Activities/Super-key icon rendering consistent across an
    authenticated update and avoids user-local SVG loader/theme variance.

## [2.38.0] — 2026-07-22

  ### Added
  - **Search now has a responsive Terminator-style bottom bar.**
    `Ctrl+Shift+F` exposes Previous, Next, Wrap, case mode
    (**Smart / Match / Ignore**), Invert, and Close controls around a
    grapheme-aware editor. Wrap/case/invert persist through both the bar and the
    new Settings → Search page; queries are remembered per pane within each
    window. Enter/Shift+Enter follow/opposite the configured direction, while
    F3/Shift+F3 remain literal Next/Previous.
  - **Search is deterministically agent-testable without corrupting a running
    TUI.** The full control server and MCP bridge add `dispatch_ui_key`, a
    bounded modal-only key path that never writes PTY bytes. `ui_geometry` adds
    query-free Search control rectangles, status, target, modes, and
    match/truncation metadata.

  ### Changed
  - Scrollback patterns now use `regex-automata`'s meta engine with strict Rust
    regex semantics and a 4096-byte UTF-8 cap; invalid syntax is reported
    instead of silently falling back to a literal. Valid expressions that
    exceed the 512 KiB NFA ceiling report **Pattern too complex**. Search keeps
    only the implicit whole-match capture, with 256 KiB one-pass and hybrid
    cache ceilings and a 40 KiB DFA ceiling. Zero-width results are suppressed
    in the engine's single leftmost-first pass, so a winning empty alternative
    can shadow a later consuming alternative at the same position. The bar
    reports status rather than an eager global match count.
  - Typing targets a nearby 1000-line range immediately and continues through
    nominal 1000-line ranges after 500 ms idle. Each event-loop turn runs at
    most one synchronous bounded work slice, which processes at most 64 KiB of
    UTF-8 and inspects at most 262,144 cells and 256 complete logical-line
    haystacks. Exact work-budget
    yields resume on a later event-loop turn without becoming Results limited.
    One logical haystack is additionally capped at 256 physical rows, 64 KiB,
    and 262,144 inspected cells; hitting a capacity boundary inside it is an
    immediate **Results limited** barrier that search never skips. Nearby
    projection is capped at 65,536 match spans.
  - Continuous output no longer starves chunk progress. A non-navigation scan
    verifies drift after 500 ms quiet; an explicit Previous/Next interrupted by
    output remains **Results limited** until the user retries that navigation.

  ### Fixed
  - **Historical and soft-wrapped search matches are highlighted and navigated
    correctly.** Search coordinates now preserve negative scrollback lines and
    account for the viewport display offset; bounded logical-line matching maps
    combining marks, variation selectors, ZWJ sequences, wide cells, and
    soft-wrap boundaries back to exact grid spans. Closing Search keeps the
    selected result anchored instead of snapping the viewport.
  - The byte-by-byte iTerm2 image extractor regression test now uses an
    isolated graphics budget, closing the remaining parallel-test race without
    changing production process-wide resource accounting.

## [2.37.0] — 2026-07-22

  ### Added
  - **Session recording is now a runtime toggle in every build.** Recording a
    Kettle session to an asciicast trace no longer needs a special
    `--features dev-record` build — the `--record PATH` / `--record-dir DIR` /
    `--record-raw-input` flags and the `KETTLE_RECORD*` env vars ship in every
    binary, plus new persistent config keys `record = off|on`,
    `record-dir = <path>` (default `<config-dir>/recordings`), and
    `record-raw-input = off|on`. Off by default. Keystrokes stay redacted to
    tokens; raw-keystroke capture remains a separate explicit opt-in and now
    shows a distinct **`[REC RAW]`** window-title indicator so it is never
    silent. Every existing privacy guardrail is preserved (verbatim-output
    caveat, `0600`/`0700` permissions, symlink refusal, 512 MiB / 50-file /
    5 GiB bounds, `[REC]` title, never-uploaded). See
    [docs/RECORDING.md](../RECORDING.md).
  - **A recording-enabled install now self-updates normally.** Because recording
    is a runtime toggle rather than a compile-time feature, `kettle update` (and
    automatic updates) work on any official `stable` install with recording on
    via the `record = on` config key (which survives updates) — the old
    `local-dev-record` install channel that blocked updates is retired (legacy
    markers are still recognized and refused). The Linux `--record-dir` launcher
    wiring remains a source-install convenience (it's refused on a self-updating
    release install, which would drop it on the next update).
  - **Configurable update cadence** via `update-check-interval-hours` (default
    24 = daily; floored at 1), and an hourly in-session re-check so a window left
    open for days stays current without a restart.

  ### Changed
  - **Automatic updates are on by default (`update-policy = auto`).** Kettle now
    keeps itself current in the background, oh-my-zsh style: a daily check
    installs newer signed releases (applied in place on Linux, staged on Windows
    until every window closes). The first automatic install shows a one-time
    notification explaining how to opt out (`update-policy = off`); the previous
    `notify` (banner-only) and `off` modes remain available. The first launch
    still performs no network request, and `KETTLE_PACKAGED` builds still opt out
    entirely for downstream distributions.
  - The GUI settings overlay gains an **Update check (hours)** field; recording
    stays config/CLI-driven (not a one-click toggle) to avoid accidentally
    enabling verbatim capture.
  - Retired the compile-time `dev-record` machinery that existed only to keep the
    recorder out of shipped builds: the `dev-record` Cargo feature, its separate
    CI clippy/test leg, the `install.sh --record-dir` feature sniff, and the
    `just install-local-dev-record` recipe (now `just install-recording`, a
    normal build). Net reduction in special-case code.

  ### Fixed
  - **Flaky `kettle-vt` iTerm2 image-decode test.** `iterm::tests` decoded
    through the public `decode()`, which draws on the process-shared graphics
    budget, so a concurrent test's transient-CPU reservation could intermittently
    starve a well-formed decode to `None`. The tests now use an isolated
    per-call budget, removing the contention (no runtime behavior change).

  ### Dependencies
  - Routine Dependabot bumps (GitHub Actions, cargo patch/minor group, and
    `pollster` 0.4→1.0) merged since 2.36.6.

## [2.36.6] — 2026-07-22

  ### Fixed
  - **With 2 tabs, the tab boundary now lines up with a centered vertical
    split.** The tab strip was packed flush-left into the width left of the
    trailing `▾`/`+` buttons, so the tab1/tab2 boundary sat one tab-bar-height
    left of the window centre while a 50/50 split's divider sits at the centre —
    the split line didn't continue the tab boundary. Tabs now divide the FULL
    bar width (so their boundaries fall on the same grid as pane splits for any
    tab count); only the last tab is shortened to yield the button reservation.
    The re-dock slot hit-test and the synthetic README hero/showcase scene use
    the same centre, and `docs/images/kettle-hero.png` was regenerated to match.

## [2.36.5] — 2026-07-21

  ### Added
  - **Copying a file in the OS file manager and pasting it into a pane now
    inserts the file's path**, so a video, PDF, or any non-image file can be
    handed to a CLI agent (Claude Code, Codex) — which reads the path, or drives
    `ffmpeg`/`ffprobe` for a video — instead of the paste doing nothing. When the
    clipboard holds a file list (`CF_HDROP` / `text/uri-list`) rather than text,
    Kettle pastes the path(s), shell-quoted for the focused pane (POSIX,
    PowerShell, or `cmd`) and translated to `/mnt/…` (or an in-distro `/home/…`
    for a `\\wsl.localhost\…` share) when the pane runs WSL. Multiple selected
    files paste as space-separated quoted paths. Drag-and-drop now shares the
    same shell-aware quoting and WSL translation. Controlled by the new
    `paste-files` key (on by default; `paste-files = off` restores the previous
    behavior). Explicit drag-and-drop always pastes a path regardless.

  ### Fixed
  - **A typo saved into a live-reloaded config no longer reverts silently.**
    `--check-config` already flagged malformed values (e.g. `cursor-style =
    beem`) at the CLI, but editing one into the running config just dropped the
    line with no feedback. Kettle now fires a desktop notification listing the
    ignored lines on reload. It is edge-triggered — an unchanged malformed set
    (as produced by Kettle's own settings-persistence writes) does not re-notify,
    and fixing the config resets the latch.
  - **The stray `C` and the intermittently solid autofill text that appeared
    only in Kettle while running Claude Code are fixed at the parser.** The VT
    pre-parser treated a raw `0x9c` byte inside an OSC/DCS/APC control string as
    an 8-bit ST terminator with no UTF-8 awareness, so a window title carrying a
    U+2700-block glyph (Claude Code's spinner, e.g. `✳` = `E2 9C B3`) was cut at
    the continuation byte and its tail leaked to the grid as text — printing a
    stray `C` and, when it wrapped at the last cell, scrolling the grid one row
    out of step with ConPTY so later partial repaints left stale solid text
    where dim ghost input belonged. The stop-byte scan is now UTF-8-aware (a
    `0x9c` that completes a multi-byte scalar is payload; a standalone `0x9c`
    still terminates), matching vte/xterm/Windows Terminal, for both the OSC and
    the DCS/APC arms. Separately, the bounded-discard recovery counter's
    side-effecting calls were moved out of `debug_assert_eq!` so release builds
    keep the same resynchronization behavior as debug builds.

  ### Changed
  - **Internal cycle-numbered dev-log references removed from all living
    sources.** Roughly 3,500 numbered audit-cycle bookkeeping references in
    code comments, docs, scripts, and build files were reworded into timeless
    prose that preserves each rationale. Historical changelog entries and git
    subjects are unchanged and remain the provenance record. A new repo-wide
    drift guard (`no_internal_cycle_refs_anywhere`) keeps the pattern from
    returning, and the mechanical scrub commits are listed in
    `.git-blame-ignore-revs` so `git blame` skips them.

## [2.36.4] — 2026-07-16

  ### Fixed
  - **Focused Wayland windows with developer recording enabled now return to
    idle instead of redrawing at compositor rate.** Repeated empty IME preedit
    notifications are state-deduplicated, cursor-blink deadlines advance before
    a redraw is queued, and Linux remote-session detection walks only Kettle's
    bounded pane-descendant `/proc` trees instead of refreshing every process
    and thread on each eligible frame.
  - **Linux installer CI no longer deadlocks during the tag-to-assets release
    window.** The published-release smoke waits for the platform asset as well
    as its signed tag, while retaining fatal handling for non-404 probe errors.

## [2.36.3] — 2026-07-16

  ### Fixed
  - **Tearing a tab into a second window no longer disconnects Kettle on
    Wayland.** Linux file notifications report config reads as access events;
    treating those opens as edits caused live reload to reread the config, and
    the old per-window fan-out doubled that feedback after a tear-off created a
    second window. Kettle now accepts only real file changes, coalesces watcher
    bursts with a bounded non-blocking latch, loads process-wide config once,
    and applies it to every window while preserving session-local font zoom.

## [2.36.2] — 2026-07-16

  ### Fixed
  - **Window presence records are now private, bounded, and strictly
    validated.** Kettle writes each local discovery record through an atomic
    owner-only `0600` replacement inside an owner-only `0700` directory,
    instead of allowing the process umask to leave metadata group-readable.
    Readers cap entries at 4 KiB; require the supported schema, PID/filename
    agreement, and exact RGB syntax; and reject links, reparse points,
    non-regular files, foreign ownership, or permissive modes before pruning
    invalid state. These checks keep multi-window discovery fail-closed at its
    local trust boundary without changing normal launcher behavior.

## [2.36.1] — 2026-07-15

  ### Fixed
  - **Opening Kettle a second time no longer creates an unnecessary second GUI
    process.** An argument-free desktop or Super-key launch now asks the
    existing primary process to create a fresh OS window through private,
    same-user, size- and deadline-bounded local IPC. The secondary exits only
    after the winit thread confirms that the renderer, shell, and window are
    live; incompatible, busy, malformed, or failed handoffs open a separate
    process so a launcher click is never lost. Explicit CLI launches remain
    isolated, and `--new-process` forces isolation for a default launch.
  - **Wayland frame presentation follows winit's required lifecycle.** Every
    submitted surface frame now calls the pre-present notification immediately
    before presentation, avoiding compositor timing/protocol failures during
    multi-window activity. Live screenshots no longer wait indefinitely for
    GPU readback on the UI thread: a single bounded worker performs a finite
    wait, mapping, crop, and PNG write without stalling terminal input or
    compositor dispatch.
  - **Wayland disconnects and event-loop stalls retain actionable diagnostics.**
    Kettle bridges winit/tracing events into its configured log filter and
    writes privacy-safe, bounded runtime phase incidents plus exit context to a
    private rotating state directory. Diagnostics exclude terminal contents,
    command lines, environment values, and working paths.

  ### Changed
  - **Developer recording remains policy-consistent across launcher
    activation.** The handshake compares a bounded destination fingerprint and
    raw-input policy without transmitting the recording path. Compatible
    windows share the live recorder and emit a `kettle:activation_window`
    marker; a recorder mismatch or stopped recorder falls back to an isolated
    process instead of silently changing capture or redaction behavior.

## [2.36.0] — 2026-07-14

  ### Added
  - **Progressive Kitty keyboard input is implemented end to end.** Kettle now
    answers capability queries and emits the negotiated CSI-u press, repeat,
    and release forms for text, navigation, keypad, function, modifier, media,
    and volume keys. Alternate key codes and associated text are supported,
    while applications that do not negotiate the protocol retain the existing
    xterm-compatible input path. Keyboard flags and the bounded mode stack are
    independent across primary and alternate screens.
  - **Native input methods and accessibility bridges cover the terminal UI.**
    IME composition is positioned and rendered at the focused terminal cursor,
    committed text follows the same read-only, broadcast, selection, blink, and
    scroll rules as keyboard input, and text-entry overlays accept IME commits.
    AccessKit exposes the application and live pane tree, pane titles, visible
    terminal text, focus, read-only state, and geometry through each platform's
    native accessibility API. Semantic-change detection and a 10 Hz publication
    ceiling keep high-output TUIs from overwhelming the UI thread when a native
    accessibility client is active.
  - **Release packages carry a second, content-level integrity manifest.**
    Linux and Windows archives bind their exact product, version, target, file
    set, sizes, SHA-256 hashes, and Unix modes where applicable. Release CI
    verifies the generated manifest before compression and again after
    extracting the final archive.

  ### Fixed
  - **Control requests cannot hang indefinitely while writing to a stalled
    local peer.** Unix uses nonblocking sends with deadline-aware polling;
    Windows uses operation-specific overlapped I/O, cancellation, and mandatory
    kernel completion draining. Timeouts and MCP cancellation retain their
    public error semantics.
  - **Headless Windows command cancellation reaches descendant processes.**
    `kettle exec` assigns its ConPTY child to a private Job Object configured to
    terminate the tree on cancellation, timeout, or handle closure, with the
    portable PTY kill retained as a fallback.
  - **Consumed UI shortcuts no longer leak unmatched Kitty key releases.**
    Physical presses owned by Kettle overlays or keybindings suppress only
    their corresponding release and are cleared safely on focus loss.
  - **Bottom input bars remain one line at narrow widths.** Search, command and
    layout palettes, SSH/title prompts, confirmations, and update notices are
    width-fitted with Unicode-aware truncation instead of wrapping into clipped
    rows.
  - **Kitty mode-stack overflow no longer corrupts terminal title state.** A
    narrowly vendored `alacritty_terminal` patch evicts the oldest keyboard
    mode, tracks direct flag changes, and preserves screen-local query state;
    public parser regressions cover the patched behavior until upstream ships
    an equivalent release.

  ### Changed
  - **Agent/TUI compatibility checks now exercise the real local toolchain.**
    The smoke validates Kettle's PTY environment and Kitty query round trip,
    Codex CLI, Claude Code CLI, clean and configured Neovim/AstroNvim, and tmux
    with additive terminal features and progressive extended keys. Terminal
    compatibility documentation now records the supported tmux configuration
    and protocol behavior.
  - **Release commits are signed.** The release helper now requires `git
    commit -S`, matching the already signed release-tag path and GitHub's
    verified-signature publication gate.

## [2.35.0] — 2026-07-14

  ### Fixed
  - **Session recording is bounded, private, and collision-safe.** GUI
    development recordings stop on a complete event boundary at 512 MiB and
    keep their stopped state visible as `[REC LIMIT]` or `[REC ERROR]`.
    Managed recording directories use private, unique, locked files and prune
    only the recognized `kettle-session-*.cast` namespace toward 50 files / 5
    GiB, without deleting active, linked, legacy, or unrelated files. Explicit
    paths are locked before truncation, symbolic links are refused, and
    `kettle exec --record` secures its trace before spawning the child or exits
    125. The lossless PTY-output fan-out also prevents close/redraw drains from
    skipping recorded output.
  - **Terminal graphics have one enforced allocation envelope.** OSC/APC/DCS
    parsing, kitty transmissions, decoded images, animations, placements,
    retained CPU memory, and GPU textures now have checked per-resource and
    process limits with RAII accounting. Oversized unterminated control strings
    recover after a bounded discard window instead of retaining an unbounded
    parser state. Unicode-placeholder tiles share source images and select
    source rectangles with per-instance UVs rather than cloning cropped image
    data; wallpaper tiling keeps its independent 4096-instance budget.
  - **The local control plane fails closed on malformed or ambiguous traffic.**
    Protocol v1 now requires exact versions and response ids, caps requests,
    responses, events, connections, and interleaved client event queues, and
    pages large live reads with snapshot tokens. Explicit pane/window targets
    must be typed live ids and never fall back to focus. Unix discovery/socket
    state is private and peer-uid checked; Windows named pipes reject remote
    clients and use a current-user DACL. Client calls have method-aware
    deadlines and cancellable polling.
  - **MCP initialization and tool execution are bounded and cancellable.** The
    stdio bridge negotiates MCP `2025-11-25` or `2025-06-18`, accepts `ping`
    during initialization, requires the exact initialized notification, and
    distinguishes invalid JSON-RPC/tool envelopes from known-tool execution
    errors. Four workers sit behind a 16-request queue; request lines, encoded
    responses, and tool text are capped, including after JSON escaping.
    Cancellation drops queued work and stops running headless commands or
    control waits.
  - **Config, session, and updater state survive interruption without following
    unsafe paths.** The new `kettle-state` crate provides durable atomic
    replacement and advisory locks. Config UI edits preserve BOM/UTF-16
    encoding, line endings, existing permissions, comments, and first-write
    backups; retain dotfile-manager symlinks by editing their regular target;
    and refuse oversized/non-regular files, newly malformed edits, or external
    changes observed by the final pre-stage comparison. Session snapshots are
    durably replaced as private files and reject link destinations.
  - **Self-update extraction and recovery are strict and restartable.** Archive
    download, entry-count, and expanded-size caps are enforced while reading;
    traversal, aliases, links, special files, unsafe Windows names, declared
    size mismatches, and path conflicts are rejected. Schema-2 journals bind
    every old/new file to size and SHA-256 and checkpoint rollback so recovery
    itself can resume after interruption. Windows stages verified files for an
    out-of-process helper, holds shared run locks until old processes unmap,
    checkpoints each of at most three attempts before fallible work, and
    attempts to quarantine invalid/exhausted pending state so the intact old
    build can start; recovery notifications are best-effort.
  - **Linux source/dev-record installs cannot masquerade as stable packages.**
    Ownership markers are atomically replaced, record directories and binary
    feature support are verified, symbolic-link directories are refused, and
    Desktop Entry values now survive the full two-layer escaping required for
    spaces, backslashes, percent signs, dollar signs, quotes, and backticks.
    The stable updater refuses `local-dev` and `local-dev-record` ownership.
  - **Unfocused windows no longer show a hollow terminal cursor.** Kettle now
    suppresses cursor rendering while the OS window lacks focus without
    mutating the child's DEC cursor shape, visibility, or blink state. This
    removes the bottom-left hollow caret seen in Codex and Claude Code and
    restores the exact client-selected state on refocus.
  - **Scrollbar interaction no longer jumps at drag start or shrink on high-DPI
    displays.** The compact overlay uses logical-pixel scaling, a larger
    invisible hit strip, a DPI-scaled minimum thumb, theme-derived contrast,
    and preserves the pointer's offset inside the thumb while dragging.
  - **Windows Search launches survive upgrades from older custom shortcuts.**
    The installer now replaces its managed Start-menu shortcut and explicitly
    clears arguments instead of allowing WScript.Shell to retain stale
    PowerShell recorder flags that make `kettle.exe` exit before opening.
    Custom-prefix uninstalls also leave the default installation's shortcut,
    registry entry, PATH, and PowerShell profile integration untouched.
  - **Authenticated Codex/Claude live smokes no longer accept shell echo as an
    agent response.** Child output is explicitly framed, stale or absent native
    exit codes fail closed, and Windows uses a version-independent .NET temp
    path. A headless CI self-test covers the original command-failure false
    positive.
  - **Package metadata can no longer pair a new version with old checksums.**
    Release CI now renders Homebrew and AUR files from the exact verified
    archives, publishes both as release assets, and checks their values against
    the archive sidecars. Source-controlled `.in` files contain no stale
    artifact hashes.

  ### Changed
  - **Release and dependency automation is reproducible by source identity.**
    GitHub Actions are pinned to full commit SHAs, the locally downloaded
    `actionlint` binary is checksum-verified, dependency update policy is
    deduplicated, and the `open` dependency is current.
  - **Stable releases now publish atomically from one signing job.** Required
    Windows, macOS, Linux x86_64, and Linux aarch64 packages finish before CI
    creates an Ed25519-signed update manifest. CI verifies the annotated tag,
    sidecars, exact draft asset names, and sizes before making the release
    public; protected-main release preparation and tag creation are separate.
  - **The renderer now uses one coherent wgpu 30 stack.** `glyphon` 0.12 and
    `cosmic-text` 0.19 replace the incompatible wgpu 29 text-rendering graph.
    Kettle presents usable suboptimal surface frames before reconfiguring and
    propagates fallible GPU readback mappings with context instead of assuming
    that a mapped range is always available.

  ### Added
  - **Official Windows and Linux installs can update with `kettle update`.** A
    shared updater verifies a domain-separated Ed25519 manifest signature,
    target-specific archive size and SHA-256, bounded path-safe extraction,
    installer ownership, and a prefix lock before journaled atomic replacement.
    `--yes` supports automation, `--update` is an interactive alias, automatic
    policy is `off|notify|auto`, and interrupted transactions roll back. Windows
    installs include a console launcher so bare `kettle` commands wait for
    prompts and propagate exit codes without adding a console to Start launches.
  - **Unbound application shortcuts remain available to PTY clients.** Kettle
    keeps `Ctrl+Shift+V` for its own paste action; other `V` modifier
    combinations are encoded through the active terminal keyboard mode instead
    of being intercepted by default bindings. This guarantees input transport,
    not a particular client's version- or platform-specific attachment action.

## [2.34.4] — 2026-07-09

  ### Fixed
  - **DEC 2026 deadlines now take priority over queued PTY data and EOF.** A
    ready chunk at the 150 ms boundary can no longer starve synchronized-output
    flushing, and a child exit flushes its final buffered update immediately.
    Timeout recovery also consumes a poisoned terminal lock instead of spinning
    on a zero-duration receive loop.

## [2.34.3] — 2026-07-09

  ### Fixed
  - **Package-template CI no longer fails a fresh release race.** The automatic
    check still validates published hashes when the sidecars are available, but
    it now skips the remote hash comparison while a newly pushed tag's release
    assets are still uploading. `--require-release` remains strict.
  - **Codex CLI's active placeholder no longer leaves a blinking block at the
    bottom-left on native Windows.** Kettle now recognizes the active Codex
    footer and suppresses only transient status cursors and the cursor over a
    DIM empty placeholder. A real queued-input caret remains visible, and the
    parsed DEC cursor state is unchanged.
  - **Kettle no longer panics when launched during the first minute after
    Windows boots.** Trigger and remote-poll throttles now represent "never
    fired" explicitly instead of subtracting 60 seconds from a monotonic
    `Instant` whose uptime origin may be newer than that.
  - **Shift+Home/End selections survive action dispatch.** The common action
    tail used to issue a no-op terminal resize after every action; Alacritty
    clears selection state on resize, so the new keyboard selection was erased
    immediately. Logical-grid and pixel-only resize changes are now separated,
    avoiding redundant ConPTY resize work and preserving selection on no-ops.
  - **PTY output handoff memory is bounded.** The blocking PTY pump now uses a
    four-slot synchronous queue and recycled 64 KiB buffers instead of an
    unbounded allocation-per-read channel. DEC 2026 timeout handling remains
    independent and now has behavioral omitted/split-terminator tests.
    `kettle exec` also uses a four-slot lossless output queue, so a slow stdout
    consumer backpressures the child instead of growing process memory.
  - **`kettle --gpu-info` now reflects the configured adapter policy.** It
    honors `--config` / `--profile`, GPU pins, power preference, and
    `gpu-force-software` instead of always querying default hardware.
  - **Native Windows PTYs preserve session-local environment overrides.**
    Kettle now overlays the actual parent environment after `portable-pty`'s
    registry refresh and merges registry-only PATH entries behind it, keeping
    virtualenvs and temporary package-manager/tool paths available to children.
  - **Relative file links work with Windows working directories.** Drive-letter
    cwd values are normalized into local `file:///C:/...` URIs, so relative
    compiler paths are clickable and underlined like absolute Windows paths.
  - **Closed CLI pipes no longer create false crash reports.** Crate-local
    stdout/stderr writers suppress only broken/closed-pipe errors (including
    Windows errors 109 and 232); other output failures remain fatal.

  ### Added
  - **Fault-only GPU recovery diagnostics.** Fatal wgpu errors latch a bounded
    in-memory reason and the event-loop thread writes versioned JSONL under
    `<cache>/kettle/diagnostics/`. Files contain adapter/recovery metadata but
    never terminal text, commands, environment variables, or working
    directories; each incident is capped at 256 KiB and only the newest ten are
    retained.
  - `kettle ctl read_screen` now reports `selection_present` and
    `selection_range`; `include_selection: true` adds text capped at 256 KiB.
    `send_mouse` accepts an optional synthetic `mods` value. These additive
    protocol-v1 fields power an exact live
    Shift+Home/Shift+End/Shift+click regression workflow.
  - `just tracked-audit` verifies every Git-tracked path and emits a JSON
    integrity ledger covering path/case collisions, object hashes, UTF-8/LF
    hygiene, manifests, local Markdown links, and font/image bounds.

## [2.34.2] — 2026-07-05

  ### Fixed
  - **Alternate-screen mouse wheel scrolling now works when the running TUI has
    not enabled mouse tracking.** Wheel notches in `less`, `man`, vim, and
    similar alternate-screen programs are translated to cursor-key input
    (honoring application-cursor mode) instead of trying to scroll Kettle
    scrollback. Shift+wheel remains the explicit local scrollback override, and
    mouse-tracking apps still receive real wheel reports. The same behavior is
    wired through `kettle ctl send_mouse` wheel events.
  - **Context menus now consume middle-clicks.** A middle-click while a context
    menu is open dismisses the menu instead of leaking through to paste PRIMARY
    into the pane or close a tab behind the menu. Right-click still relocates the
    menu.
  - **Windows Codex CLI status-row cursor artifacts are suppressed in Kettle's
    renderer.** Native Windows ConPTY/Codex sessions can report a visible cursor
    parked on Codex's model/status row; Kettle now suppresses that renderer-only
    artifact on native Windows while preserving terminal cursor state and leaving
    Ubuntu/WSL behavior unchanged.

## [2.34.1] — 2026-07-03

  ### Fixed
  - **The live window no longer crashes over RDP (and other
    RENDER_ATTACHMENT-only display adapters).** The wgpu surface was configured
    with `RENDER_ATTACHMENT | COPY_SRC` unconditionally (COPY_SRC added in cycle
    688 for in-window screenshot readback). The Microsoft Remote Display adapter
    injected into a Windows RDP session advertises only `RENDER_ATTACHMENT`, so
    `Surface::configure` failed validation, the surface stayed unconfigured, and
    the next `get_current_texture` panicked ("Surface is not configured for
    presentation") — crashing the live window over RDP. `COPY_SRC` is now gated
    on the surface's advertised capabilities, and the in-window screenshot
    readback degrades gracefully when it is absent (a clear error instead of a
    validation panic). Offscreen `--screenshot` / `--gpu-info` are unaffected
    (they build their own COPY_SRC textures), and the resize / GPU-recovery
    reconfigure paths reuse the gated config so they inherit the fix.
  - Install docs (`docs/INSTALL.md`, `README.md`) referenced a stale `v2.31.0`
    for the "current latest" line, download URLs, and `KETTLE_VERSION=` pin
    example; they now track the current release, and `scripts/release.sh` rewrites
    those release-reference strings by pattern so they cannot silently strand
    again when a release is missed (the cycle-790 bump only matched the
    immediately-previous version).

## [2.34.0] — 2026-07-02

  ### Fixed
  - **GNOME Wayland titlebar decorations are Adwaita-styled again.** v2.33.1's
    RustSec scoping turned off winit's default features, which silently dropped
    the Adwaita client-side-decoration frame — on GNOME (whose Mutter offers no
    server-side decorations) the minimize/maximize/close buttons regressed to
    smithay-client-toolkit's fallback frame: a flat gray strip with a
    filled-square close button and no hover polish. Kettle now enables winit's
    `wayland-csd-adwaita-notitle` feature: proper Adwaita-style buttons return
    without reintroducing the scoped `ttf-parser` advisory path (the notitle
    variant carries no text renderer, so `ab_glyph`/`owned_ttf_parser` stay out
    of the graph — `scripts/check-ttf-parser-scope.sh` still proves it, and a
    new drift-guard test pins the winit feature list). The CSD bar shows
    buttons but no title text; the window title still reaches the taskbar,
    Alt-Tab, and dock via the usual title channel.
  - Config-file persistence no longer overwrites unreadable/non-UTF-8 existing
    configs with an empty file or empty `.bak`; menu toggles and keybind edits
    now fail loudly before touching the file.
  - `Ctrl+Shift+Space` now round-trips through the keybind grammar as
    `Ctrl+Shift+Space`, so the default vi-mode toggle is listable and
    re-bindable from config.
  - `padding_x`/`padding_y` and `window_padding_x`/`window_padding_y` now share
    the same malformed-value validation as the dash-form padding aliases.
  - Failed `custom-url-handler` launches now really fall back to the system URL
    opener instead of logging that fallback would happen and then returning.
  - Agent `run_command` orphan detection now checks panes across all windows,
    so a pending command in another window is not mistaken for a closed pane.
  - The Windows notification dependency chain now uses
    `tauri-winrt-notification` 0.7.3, removing the runtime `quick-xml` advisory
    path. The remaining `quick-xml` RustSec ignore is scoped to build-time
    Wayland protocol code generation until `wayland-scanner` ships a fixed
    dependency.

  ### Added
  - **Native titlebar follows the active theme.** The OS titlebar (Windows DWM
    caption dark/light mode; the new Wayland Adwaita frame) now matches the
    darkness of the active kettle theme instead of always tracking the OS-wide
    setting: a forced dark theme on a light desktop gets a dark titlebar (and
    vice versa). `theme-mode = auto` without a schedule keeps deferring to the
    OS so the palette auto-switcher continues to work. Decided by WCAG relative
    background luminance (`Theme::is_dark`, contrast-crossover threshold) and
    re-synced lazily on redraw, so every theme-change path — actions, context
    menu, Lua, settings, schedule, live reload — is covered by one chokepoint.

## [2.33.1] — 2026-07-02

  ### Added
  - **Scoped `ttf-parser` dependency guard.** CI now verifies that the temporary
    RustSec exception remains limited to the upstream-bound
    `glyphon`/`cosmic-text`/`fontdb` path, and tells maintainers to remove the
    ignore once `ttf-parser` disappears.
  - **GPU device-loss recovery.** Kettle now attempts to rebuild the renderer
    after a GPU reset/device loss without restarting panes or PTYs. Recovery
    retries the configured GPU, then another hardware adapter, then software
    rendering if no hardware path is usable.
  - **Signed release-tag path.** `scripts/release.sh` now creates signed
    annotated tags by default and keeps `docs/VERSION-HISTORY.md` in version
    lockstep during release bumps.
  - **Version-history reference.** Added a compact version-history guide that
    summarizes the release eras and points maintainers at the authoritative tag,
    release, changelog, and packaging lockstep sources.

  ### Fixed
  - **DEC 2026 synchronized updates flush on timeout.** A quiet or split
    synchronized update can no longer leave stale terminal text buffered
    indefinitely; the PTY reader now wakes at the vte sync deadline and flushes
    the pending grid update.
  - **RustSec `ttf-parser` advisory is scoped and documented.** The avoidable
    `winit` Adwaita-CSD path was removed; the remaining `glyphon`/`fontdb`
    path is guarded and tracked as an upstream-bound exception until a
    compatible replacement stack is available.

  ### Changed
  - CI workflows now use `actions/checkout@v7`.
  - Refreshed patch/minor Cargo dependencies: `log`, `env_logger`,
    `notify-rust`, `open`, and `clap_complete`.

## [2.33.0] — 2026-06-29

  ### Added
  - **Keyboard text selection — Shift+Home/End, and Select All.** Selecting
    scrollback text no longer needs the mouse (the AskUbuntu "select all in
    terminator" gestures):
    - **Shift+Home** extends the selection up to the first line / top of the
      buffer (scrollback included); **Shift+End** extends it down to the last
      cell. Both scroll the viewport to reveal the new extent.
    - **Shift+click** on a character still extends the current selection to that
      point (unchanged).
    - **Select All** (`select_all`) selects the entire scrollback + screen — in
      the command palette and bindable as a keybind (no default chord to avoid
      conflicts).
    - Scroll-to-extremes moved off Shift+Home/End to **Ctrl+Home / Ctrl+End** so
      both behaviors stay reachable. New bindable actions: `select_all`,
      `select_to_top`, `select_to_bottom`.

  ### Fixed
  - **Dev-record title bars avoid missing Ubuntu glyphs.** The native OS title
    indicator now uses ASCII `[REC]` instead of `● REC`, so Linux desktop title
    fonts that lack the symbol still show the recording state clearly. The
    control-plane `get_state` result now also reports the computed
    `window_title` for diagnostics and live smoke tests.

## [2.32.3] — 2026-06-23

  ### Fixed
  - **Wide tabs recover full cwd labels from shell-truncated titles.** If a
    shell or prompt sets a left-truncated title such as `..ine-server-go` while
    the pane cwd is `flight-event-line-server-go`, Kettle now resolves the tab
    back to the full directory leaf and keeps the width-aware path metadata.
  - **Split-pane titlebars use cwd-aware title fitting.** Pane titlebars now use
    OSC 7 cwd metadata to recover the full directory path when the shell title
    is only a truncated cwd suffix, matching the fixed tab title behavior.
  - **Cwd title recovery now handles parent-directory suffixes.** Oh My Zsh-style
    titles such as `..PI-1/platform` are recognized as truncated cwd renderings
    when OSC 7 reports the full cwd, so wide tabs and the Ubuntu window title can
    show the full cwd context instead of preserving the shell's stale truncation.
  - **Zoom keybinds survive Ubuntu/Wayland key reporting.** App keybind matching
    now falls back from winit's logical key to the physical plus/minus/reset key
    codes, so Ctrl+Plus / Ctrl+Minus / Ctrl+0 keep changing the font size even
    when the compositor reports a truncated or layout-dependent logical key.

## [2.32.2] — 2026-06-23

  ### Fixed
  - **Linux desktop launchers display as `Kettle`.** Packaged and user-local
    `.desktop` entries now use the user-facing app name expected by Ubuntu /
    GNOME Super-key search.

## [2.32.1] — 2026-06-23

  ### Fixed
  - **Title-edit input no longer covers terminal output on Ubuntu.** Window,
    tab, pane, and group rename now render in tab/chrome space with matching
    geometry for the background and text instead of placing the text at the
    bottom of the terminal viewport.
  - **Directory tab labels use the available tab width before ellipsizing.** A
    tab titled from `flight-event-line-server-go` now shows the full leaf when
    it fits, including when a shell title exactly matches the current directory
    name.
  - **Tab labels center inside their usable lane.** Two-tab windows with split
    panes no longer look visually pulled off-center by the trailing close button
    or the new-tab dropdown controls.

  ### Changed
  - Added a Linux maintainer install path for local dev-record launches:
    `just install-local-dev-record` builds with `--features dev-record`, syncs
    the user-local desktop launcher, and records Super-key launches under the
    configured record directory.

## [2.32.0] — 2026-06-21

  A correctness, robustness, and security hardening release driven by an
  exhaustive multi-agent audit of every crate plus seven cross-cutting
  dimensions (Rust, terminal-emulator correctness, docs, UI/UX, architecture,
  security, performance). Every fix below was adversarially re-verified against
  the code before landing. Larger structural items (in-process GPU
  auto-recovery, the `app.rs` split, kitty `a=q` replies, vertical-list pickers)
  are tracked as a follow-up in `docs/AUDIT-DEFERRED.md`.

  ### Fixed — terminal correctness
  - **Bracketed paste no longer corrupts newlines.** The CR-normalization that
    guards against paste auto-run was applied unconditionally, rewriting `\n`→`\r`
    even *inside* the `ESC[200~`/`201~` markers — garbling multi-line pastes into
    vim / IPython / node. It now applies only on the non-bracketed path; bracketed
    pastes preserve `\n` (and still strip embedded end-markers).
  - **OSC 9 ConEmu subcommands no longer fire bogus desktop notifications.** Every
    `OSC 9;<n>` (progress `9;1`, `9;2`, …) was treated as an iTerm2 notification
    title; the parser now forwards the structured ConEmu shape downstream and only
    notifies on genuine free-text `OSC 9` titles.
  - **Legacy (non-SGR) mouse mode reports button release.** A release re-encoded
    the original button instead of the release sentinel (code 3); pagers/editors
    using X10/normal mouse mode now see button-up.
  - **Combining (zero-width) marks are preserved** on screen and in search /
    links / agent-scrape, routed through one shared `grid_text` helper + a
    `SnapCell.zerowidth` field (a decomposed `e`+U+0301 renders as `é`).
  - **iTerm2 OSC 1337 file transfers with `inline=0`** are no longer rendered as
    inline images; `bail()` on an over-long control string now appends a synthetic
    terminator so the downstream VT parser can't desync; OSC 7 percent-decoding
    rejects sign-prefixed escapes.
  - **Vi-mode cursor + visual selection now render** with the search bar closed
    (the normal case) — the overlay early-return had omitted the vi fields.

  ### Fixed — robustness & multi-window
  - **A lost GPU device no longer spins the event loop.** Building on v2.31.0, the
    redraw guard now clears the coalescing flag and snapshots per-pane output
    generations, `about_to_wait` short-circuits animation wakes, and a resize can't
    reconfigure a dead surface — so streaming panes during a driver TDR quiesce
    instead of waking at 30–60 Hz.
  - **OS focus changes route correctly.** `focused_seq` was never updated on a
    window-manager focus change, so `--remote`/ctl "focused-window" operations, the
    update banner, and the agent `focused_window`/`to_window` JSON targeted a stale
    window.
  - **Close-confirmation is no longer skipped for a busy multi-pane scope.** A tab
    or window with several panes but only one busy pane bypassed the
    "MultipleTerminals" prompt and closed without asking; the busy count is now only
    the all-idle skip, while the prompt decision uses the full scope size.
  - **Broadcast input is never black-holed.** An emptied named broadcast group
    self-heals to the focused pane (was: all keystrokes silently dropped with the
    indicator still lit), and a group broadcast always includes the on-screen pane.
  - **Session save/restore round-trips** tab title overrides, pane broadcast-group
    membership, and zoom state (additively, so old session files still load).
  - **ctl control-plane:** `discover()` now prunes a registry entry only when the
    owning process is genuinely dead (a transient client-side hiccup no longer makes
    a live server permanently undiscoverable); the Windows connector fast-fails a
    missing pipe instead of a ~1 s retry spin; `run_command` probes for a
    disconnected client so a Ctrl+C'd run frees its slot + badge promptly; one
    connection-cap counter is the single source of truth.
  - A directly-launched `ssh`/`docker` pane (no shell parent) is now detected as a
    remote context; `foreground_cwd` only follows a *linear* process chain, so a
    background job can't mislabel the pane's directory; `attach_tab` drops a
    displaced pane instead of leaking its PTY + child.

  ### Security
  - **OSC window titles are sanitized** before reaching the OS titlebar / tab /
    status bar: control characters and Unicode bidirectional-override format
    characters (the U+202E Alt-Tab / titlebar spoofing vector) become spaces, and
    the length is capped.
  - **dev-record redacts modifier+printable keystrokes** unless raw-input recording
    is explicitly on — AltGr is reported as Ctrl+Alt on Windows, so non-US-layout
    symbols / accented letters would otherwise have landed in the always-on trace in
    cleartext.
  - **Remote reconnect commands are shell-safe.** Host/user/container fields parsed
    from a descendant's argv are now charset-validated at parse time and
    POSIX-single-quoted at build time; a value with a control char yields no
    Reconnect menu item rather than an unsafe auto-executed line. `ssh -l <user>`
    now reconnects as the right user.

  ### Fixed — config & UX
  - `accent_color` (snake_case) now applies instead of validating-but-silently-
    ignoring; `background-image-mode` / `-align-*` enum typos are flagged by
    `--check-config`; an explicit color override (`background = …`) survives a later
    `theme =` line; a valid `trigger = REGEX :: cmd` no longer false-positives.
  - The default broadcast chord is `Ctrl+Shift+G` on Windows (Win+G is the OS Game
    Bar); `Ctrl+Shift+D` half-page menu scroll now goes *down*.
  - `--screenshot` honors the default **Grid** (cell-locked) text renderer, so the
    README hero/showcase imagery matches the live product; block (Alt+drag)
    selection draws the column rectangle it actually copies.

  ### Internal / docs
  - `resolve_run` column inheritance saturates instead of risking a `u16` overflow
    abort. Documentation sweep: README / INSTALL version pins, the default theme
    name, the `group_tab` description, accent-color defaults, and a new
    auto-shell-integration section in `docs/SHELL-INTEGRATION.md`. The dev-record
    feature build is now exercised in CI.

## [2.31.0] — 2026-06-20

  ### Fixed
  - **kettle no longer hangs/crashes when the GPU device is lost** (a driver
    TDR/reset under memory pressure — the user-reported crash while running many
    tabs + windows + WSL). Diagnosis from the Windows event log: a GPU display-
    watchdog reset, no kettle panic (the crash log was empty) → kettle's
    surface-error arm spun reconfiguring a dead device forever (a permanent
    freeze). Now: a wgpu **uncaptured-error handler** + **device-lost callback**
    turn a GPU fault into a logged, observable event instead of wgpu's default
    panic (which `panic = "abort"` had turned into a hard crash); a shared
    `gpu_lost` flag (also tripped by a sustained surface-failure streak as a
    backend-independent fallback) makes the App stop painting the dead device and
    show a "GPU device lost — please reopen kettle" state, keeping the event loop
    alive instead of spinning. The handlers are careful NOT to false-trip: a
    recoverable `Validation` error is logged but does not flag device-loss, and the
    benign transient `Occluded`/`Timeout` acquire states (e.g. a minimized macOS
    window) are skipped, never counted toward the device-lost streak.
  - **README hero/showcase screenshots: tabs now fill the bar.** The synthetic
    `--screenshot` scene still drew the pre-v2.28.0 compact, label-width tabs; it
    now renders the current style — each tab takes an equal share of the strip with
    the title left-aligned, the `✕` at the tab's right edge, and the active-tab
    accent — so the README matches the real product.

  ### Changed
  - **dev-record: `KETTLE_RECORD` may now be a DIRECTORY.** When it (or `--record`)
    points at a directory, kettle drops a fresh `session-<unix>.cast` inside it —
    so a *persistent* `KETTLE_RECORD=<dir>` records **every** launch (taskbar /
    direct / reopen), not just the Start-menu VBS launch that passes an explicit
    `--record <file>`. This is what makes a future crash reliably captured.

## [2.30.1] — 2026-06-20

  ### Fixed
  - **Critical: the v2.30.0 auto shell-integration broke the PowerShell prompt
    (blank screen, couldn't type).** `kettle.ps1` stashed the existing prompt with
    `Get-Item function:prompt` (a `FunctionInfo`) and called it with `&` — which
    re-resolves the *live* `prompt`, i.e. the new kettle wrapper, so it recursed
    into itself, threw, and PowerShell re-fired the (throwing) prompt forever: an
    infinite prompt loop with no visible prompt and no input. Now it captures the
    original prompt's `.ScriptBlock` (a frozen snapshot) and invokes that, and the
    wrapper renders the original prompt inside a `try/catch` so a failure can never
    re-create a loop. cwd tracking (OSC 7) is unaffected and still works. The
    ConPTY integration test now also asserts a typed command *executes* (the shell
    stays interactive), not just that OSC 7 fires.

## [2.30.0] — 2026-06-19

  ### Added
  - **Auto shell-integration — the tab now tracks `cd` for a stock PowerShell
    with ZERO setup.** v2.29.0 could not make this work: PowerShell's
    `Set-Location` does not update the OS process working directory, so kettle
    can't read the cwd from outside the process (this is why even Windows Terminal
    needs shell integration for PowerShell). kettle now launches its **default**
    shell already wired to report its cwd — pwsh/powershell via `-NoExit
    -EncodedCommand <kettle.ps1>` (your `$PROFILE` still loads first; kettle only
    wraps the resulting prompt, preserving oh-my-posh / posh-git / starship). The
    prompt then emits OSC 7 on every `cd`, so the tab/window/pane label tracks
    your directory. New config **`shell-integration`** (`auto` default / `off`);
    cmd.exe is left untouched (its process cwd already tracks `cd`, read by the
    native poll); only the default shell is affected, not an explicit `command =`.

  ### Fixed
  - **Native cwd poll no longer shadowed by the launch directory.** The OSC-7 cwd
    cell is pre-seeded with the shell's launch dir; v2.29.0's
    `current_dir_or_native` always preferred it, so a stock shell's tab froze at
    the launch dir. kettle now treats the seed as authoritative only once the
    shell actually *reports* a cwd (OSC 7/9;9, tracked by a new `osc_cwd_seen`
    flag); until then it uses the live native poll. This makes `cmd.exe` cwd
    tracking work and lets the OSC 7 from the new auto-integration take over.

## [2.29.0] — 2026-06-19

  ### Fixed
  - **Tab / window / pane titles now track the working directory for a stock
    Windows shell — with zero setup.** A bare `pwsh`/`cmd` launched in kettle used
    to show the full `…\pwsh.exe` path forever, because (1) ConPTY injects that
    executable path as the startup window title and (2) the shell never reports
    its directory unless shell-integration is sourced from `$PROFILE`. kettle now
    (a) ignores that bogus injected exe-path title so the cwd fallback engages,
    and (b) reads the foreground process's working directory natively from the OS
    process table (reusing the existing 5 Hz process poll — no new dependency), so
    the tab labels its directory and updates on `cd`, matching Windows Terminal.
    OSC 7 stays authoritative whenever a shell *does* report a cwd (including WSL,
    where the native read is meaningless and is skipped). The split-pane titlebar —
    which had no cwd fallback at all — now also shows the cwd leaf for a
    placeholder-titled pane.

  ### Added
  - **OSC 9;9 (ConEmu “set working directory”) is now honored** as a cwd source
    alongside OSC 7. Any oh-my-posh / starship / custom prompt already emitting it
    for Windows Terminal reports its directory to kettle for free (and it is the
    correct cwd source for an in-distro WSL prompt).

  ### Changed
  - **App icon recolored to the TokyoNight Night palette** (kettle's default theme
    since v2.28.0): a deep-navy `#1a1b26` window with a signature blue `#7aa2f7`
    border + caret and a `#c0caf5` prompt chevron — the same blue kettle paints the
    active-pane border in. Every artifact regenerated from the SVG (Linux hicolor
    PNGs, the macOS `.iconset`, the 7-resolution Windows `.ico`, and the embedded
    winit window icon).
  - **Docs + showcase images aligned to the TokyoNight Night default.** The v2.28.0
    default-theme switch left README / CONFIG / UX-COMPARISON / TESTING and the
    architecture diagram still describing Catppuccin Mocha; these now read
    TokyoNight Night, and the hero/showcase screenshots are re-rendered in it.

## [2.28.0] — 2026-06-19

  ### Fixed
  - **Tabs now fill the bar width.** Removed the erroneous `tab-max-width` cap
    (introduced in v2.26.0) that left dead space with few tabs — tabs divide the
    bar evenly and **maximize width** (2 tabs each take half), so the tiered label
    can show the full path in a wide tab. The `tab-min-width` floor + `scroll-tabbar`
    overflow scrolling are unchanged; the `tab-max-width` config key is removed.
  - **Settings: rebinding a key replaces the old chord.** Capturing a new chord
    for an action now drops the action's previous binding (live + persisted via an
    `unbind` line), so the old chord no longer fires and the Keybinds row no longer
    shows a stale chord (it previously read a non-deterministic HashMap).

  ### Changed
  - **Default theme is now TokyoNight Night** (was Catppuccin Mocha). Affects a
    fresh config only; an explicit `theme =` line in your config is unchanged.

  ### Added
  - **Settings overlay: a Tabs page** exposing `tab-bar`, `tab-bar-position`
    (top/bottom), `tab-min-width`, `scroll-tabbar`, `close-button-on-tab`, and
    `detachable-tabs`; plus **`scrollbar-width`** on the Behavior page — all
    previously config-file-only.

## [2.27.0] — 2026-06-19

  ### Fixed
  - **X11 PRIMARY selection is now written on copy.** Copying a selection (and
    copy-on-select) also sets the X11 PRIMARY selection on Linux, completing the
    canonical select → middle-click-paste loop (`paste_primary` already read
    PRIMARY, but copy only ever set the CLIPBOARD). No PRIMARY on
    Wayland/macOS/Windows — those stay clipboard-only. (Audit finding.)
  - **`kettle exec --json` no longer drops a trailing partial UTF-8 sequence.** A
    stream that ends mid-codepoint now flushes the carry lossily in a final
    output event before `exit`, instead of silently dropping those bytes.
  - **Control-plane client no longer prunes a live server on a transient connect
    failure (Unix).** `transport::connect` retries briefly on `ConnectionRefused`
    (server mid-accept / socket swap), matching the Windows named-pipe retry,
    while still bailing immediately on `NotFound` so a truly-dead entry is pruned
    promptly.

## [2.26.0] — 2026-06-19

  ### Added
  - **Pronounced, overlay-style scrollbar.** The per-pane scrollback scrollbar is
    now a configurable-width bar (`scrollbar-width`, default `14` px, up from the
    old 3 px hairline) with a faint track gutter behind the thumb. It is **dim at
    rest and brighter** while the view is scrolled back or while the pointer
    hovers / drags it (a two-state step, so no fade timer and zero idle cost). The
    `auto` mode now shows the bar whenever a pane has scrollback history (not only
    while scrolled), and the click/drag grab zone matches the painted width
    (floored at 10 px) so it's easy to grab with the mouse — Terminator-like.
  - **Tiered tab labels (full path → directory name → tail).** A tab whose label
    comes from the working directory now shows, by available width: the whole
    (home-abbreviated) path, else the current directory name, else the tail of the
    name with a leading `…`. Explicit / shell-set (OSC 2) titles are unchanged.
  - **Tab-bar width caps + overflow scrolling.** New `tab-min-width` /
    `tab-max-width` keep tabs readable: a 2-tab window no longer gives each tab
    half the screen, and many tabs stop shrinking at the minimum. Past that the
    bar overflows and scrolls — `scroll-tabbar` (now wired and default-on) shows
    `‹ ›` arrow buttons and lets the mouse wheel reach hidden tabs, keeping the
    active tab in view.

  ### Fixed
  - **Wide-character search / link / agent-scrape corruption.** Reconstructing a
    grid row's text injected a literal space after every wide (CJK / emoji) glyph
    (the cell spacer), so `世界` became `世 界` and search, autodetected links, and
    the agent screen-scrape never matched across wide text. A single spacer-aware
    `kettle_core::grid_text` helper now backs all those sites so the fix can't
    drift.
  - **Agent `send_mouse` could close the wrong window.** Closing the last tab of a
    non-focused window via the control plane consumed an app-global close flag
    against the *focused* window, destroying it and orphaning the emptied target.
    The target window is now closed locally.
  - **`rotate_focused_split` rotated the wrong split in nested layouts.** It now
    rotates the split that is the focused pane's *immediate* parent, not any
    ancestor that merely has a leaf child (extracted to a unit-tested `rotate_node`).
  - **kitty `a=d` delete aborted unrelated in-flight image transmissions.** A
    targeted delete interleaved between another image's chunks no longer discards
    that image's accumulator — only a delete-all clears every in-flight transmit.
  - **`+` new-tab button now fires the `TabAdd` event** (Lua / dev-record) like
    every other new-tab path, on both the GUI and the agent control paths.

  ### Changed
  - **`word_chars` / `word-chars` config keys are no longer accepted.** They are
    VTE/Terminator's *inverse* of kettle's `word-delimiters` (word constituents vs.
    word separators); aliasing them produced exactly-inverted double-click
    selection, so they now surface as unknown keys instead of silently doing the
    opposite. Use `word-delimiters` / `selection-word-chars`.

  ### Removed
  - **tmux control-mode (`-CC`) parser scaffold.** The `kettle-vt` `tmux_cc`
    module was an unwired parser foundation, dead across many releases; removed
    rather than left as latent surface. (Design notes retained as a proposal.)

  ### Audit
  - This release also lands the high-severity findings of a workspace-wide
    multi-agent audit (per-subsystem review → adversarial verification → feature
    debate). Lower-severity findings and larger feature redesigns are tracked for
    follow-up releases.

## [2.25.1] — 2026-06-15

  ### Added
  - **Pane environment overrides.** Config now accepts repeatable
    `env = KEY=VALUE` entries for every spawned GUI pane, with portable
    variable-name validation, empty-value support, deterministic last-writer
    process-env behavior, and Windows → WSL forwarding via Kettle's existing
    `WSLENV` propagation path.
  - **Fresh-window startup geometry.** Config now accepts `window-width` /
    `window-height` in terminal cells and `window-position-x` /
    `window-position-y` in physical pixels for deterministic startup placement.
    Restored session geometry and explicit new-window placement still take
    precedence.
  - **Device Attributes advertise shipped terminal features.** Primary DA now
    replies `CSI ? 6 ; 4 ; 52 c`, so capability probers can discover Kettle's
    existing sixel decoder and OSC 52 clipboard support.
  - **Protocol desktop notifications.** PTY programs can now request desktop
    notifications with `OSC 9 ; message` or
    `OSC 777 ; notify ; title ; body`; Kettle validates, caps, and dispatches
    them through the existing notification helper.

  ### Fixed
  - **Grid renderer cursor-blink regression.** The v2.25.0 cell-locked
    `text-renderer = grid` path now keeps pane glyph uploads on their own damage
    gate: pane text, style, cell metrics and geometry refresh grid glyph
    instances, while cursor blink updates only cursor quads / the cursor-glyph
    pass. A new offscreen GPU regression renders a prompt-like `➜  ~` line
    across cursor blink phases and asserts every non-cursor prompt pixel remains
    unchanged.
  - **Live grid text no longer blanks after glyph-cache clears.** Clearing the
    cell-locked glyph pipeline now forces the next pane-glyph upload even when
    terminal contents are unchanged, so font/scale/cache invalidation cannot
    leave cursor-only frames presenting an empty text buffer.
  - **Renderer cache invalidation now includes pane layout damage.** Grid glyph
    instances and glyphon text areas refresh when pane rects, surface size, cell
    metrics, renderer mode, padding or text shaping inputs change, even if row
    contents themselves did not reshape.
  - **GPU preference default is now `auto`.** Kettle no longer labels the default
    policy as discrete/high-performance on machines that only expose integrated
    graphics. Explicit `high` and `low` remain available, and a pinned GPU still
    wins over the policy.
  - **`theme-mode = auto` now follows OS appearance changes.** The
    `auto` / `system` / `follow-system` modes apply winit's current window theme
    at startup when available and handle live `ThemeChanged` events. An explicit
    `theme-schedule` remains the owner when configured so time-based switching
    and OS switching do not fight each other.
  - **Mouse paste now uses the right selection source.** Middle-click paste uses
    X11 PRIMARY on Linux with clipboard fallback elsewhere, and PuTTY-style
    right-click paste now honors `putty-paste-style-source-clipboard` instead of
    always reading the regular clipboard.
  - **`detachable-tabs = false` now disables cross-window detach.** Mouse
    tear-off, the Wayland release-only fallback, and the
    `move_tab_to_new_window` keyboard/palette action now honor the setting while
    keeping normal tab switching and in-window reordering available.
  - **Agent CLI smoke covers Kettle's own non-interactive surfaces.** The local
    `scripts/check-agent-cli-smoke.sh` now always checks `kettle exec` PTY
    environment, `kettle exec --json` output, and `kettle mcp --self-test`
    before the optional Codex CLI / Claude Code / Neovim / AstroNvim probes.
  - **Close-confirmation buttons are clickable.** The `ask-before-closing`
    prompt now right-aligns visible `[Cancel]` / `[Close]` buttons in its
    bottom bar, switches the pointer cursor over those buttons, and dispatches
    mouse clicks through the same safe cancel/confirm path as keyboard input.
  - **Agent/editor file paths are clickable links.** Local absolute paths,
    Windows drive paths, and pane-cwd-relative paths such as
    `crates/kettle-core/src/links.rs:12:3` now resolve to local `file://` links
    through the same hover, context-menu, and `Ctrl`/`Cmd`+click path as URLs.
    The existing local-only `file://` safety gate still rejects traversal,
    remote authorities, UNC-like paths, and unsafe schemes.
  - **Multi-line raw pastes now ask before sending.** `clipboard-paste-protection`
    defaults on and confirms clipboard/PRIMARY/PuTTY-style pastes only when a
    writable target would receive raw, non-bracketed multi-line text. Bracketed
    paste targets such as editors and agent CLIs continue immediately, while
    shells that could execute embedded newlines get a safe cancel-first prompt.

  ### Documentation / packaging
  - Corrected the config reference for shipped Terminator-parity settings:
    `ask-before-closing` and `cell-width` / `cell-height` now live in the main
    key table instead of the future-work table, matching the runtime wiring.
    Added doc drift guards for stale future-work rows and duplicate config rows.
  - Refreshed stale install and package-template version references so the
    v2.25.1 release bump can keep README / INSTALL / Homebrew / AUR / Nix
    surfaces in lockstep.

## [2.25.0] — 2026-06-14

  **Cell-locked text rendering + sub-cell selection accuracy.** Fixes two
  long-standing, related glitches: text that was "every now and then misaligned"
  and a mouse selection that felt "off by one letter".

  ### Fixed
  - **Glyphs no longer drift off the cell grid.** Pane text is now rendered the
    way Alacritty / kitty / WezTerm / Ghostty do — a new cell-locked instanced
    glyph pipeline pins every glyph to its terminal cell (`col × cell_w`).
    Previously each row was laid out as one continuous shaped run, so any glyph
    whose advance differed from the monospace cell width — fallback-font glyphs
    (CJK, color emoji, some symbols), ligature clusters, a bold/italic face of a
    different width — shifted **every following glyph** off the grid that the
    selection highlight, the block cursor, link underlines and mouse hit-testing
    all assume. For ordinary primary-font ASCII the position is unchanged (its
    advance already equals the cell width), so this is a fix purely where drift
    occurred. Rasterization, antialiasing, gamma and theme colors are identical
    to before — only the X position changes. Verified live (a row of CJK that
    drifted several cells in the old path now lands exactly on the grid).
  - **Mouse selection respects the sub-cell pointer position.** Dragging to
    select now computes which HALF of a cell the pointer is in (matching
    xterm / Alacritty / iTerm2) instead of always anchoring on a fixed side, so
    the boundary cell is included only once you cross its midpoint — no more
    "off by one letter". Copy (`selection_to_string`) follows automatically.
    Word / line / double-click selection is unchanged (it snaps to token
    boundaries, which ignore the sub-cell side).

  ### Added
  - **`text-renderer` config key** (`grid` | `legacy`, default `grid`). `grid` is
    the new cell-locked path above; `legacy` restores the previous continuous
    layout as a rollback escape hatch. See
    **[CONFIG.md](../CONFIG.md)**.

## [2.24.1] — 2026-06-14

  ### Changed
  - **The starfield is now a fixed built-in example, not config-driven.** The
    `starfield-speed` / `starfield-density` / `starfield-glow` knobs introduced in
    2.24.0 are removed; the look is baked into the shader. Baked changes: a
    **much slower drift** (speed `0.06 → 0.009`) and a **much more dramatic "fade in as
    we get closer"** — stars now emerge at the center **completely invisible** (no
    brightness floor) and brighten sharply (cubic) as they approach, so the middle
    stays dark and stars bloom into view as they near. Turning it on
    (`background-type = starfield`) and the general
    `background-animation` / `chrome-background` controls are unchanged. (A stale
    `starfield-speed` line in an old config is now just an ignored unknown key.)

## [2.24.0] — 2026-06-14

  **A procedural starfield background, always-on by default, plus title-overflow
  and mouse-driven settings polish.**

  ### Added
  - **Procedural GPU starfield background** (`background-type = starfield`). A
    slow forward-flight field of soft-glowing, subtly-colored stars rendered
    entirely in a WGSL fragment shader — true-color (no GIF banding), a perfect
    loop, crisp at any resolution/aspect, and **~zero memory** (no decoded
    frames, vs the ~253 MB a 1080p animated GIF decoded to). Stars **fade in as
    they get closer** over a pure-black sky. Zero-config: pick it and it plays.
    Tunable via `starfield-speed`, `starfield-density`, `starfield-glow`.
  - **Live theme preview on hover.** Right-click → Theme → hovering (or arrowing
    over) a theme applies it instantly as a preview; moving off, Esc, or clicking
    away reverts to your current theme, and clicking commits it. No config write
    happens until you commit.
  - **Mouse control for the Settings overlay.** Left-click a row to cycle its
    value forward, right-click to cycle back, scroll-wheel to adjust; click a
    category tab to switch pages; click outside to close. Keybind rows start
    capture on click; the image-path row opens an inline text prompt.
  - **In-settings background setup.** A dedicated **Background** page: choose the
    type (solid / image / starfield / transparent), set the image **path inline**
    (no more hand-editing the config), pick the animation mode + chrome bar color.
    Options that don't apply to the current type are dimmed and skipped; the page
    dims its backdrop so the **live** wallpaper previews around the panel.

  ### Changed
  - **Animated backgrounds now play by default even when the window is
    unfocused** (`background-animation` default `when-focused → always`). A
    wallpaper that only moved while focused felt broken. It still **freezes when
    the window is minimized or fully occluded** (it can't be seen), so a hidden
    window costs zero idle — the safety refinement that keeps always-on cheap.
    Set `background-animation = when-focused` for the old battery-friendly mode.
  - **Long pane/tab titles now shorten gracefully.** A narrow split used to hard-
    cut a long title (e.g. PowerShell's full exe path) to `C:\Program` with no
    ellipsis. Titles now shed the size text, then the group tag, then
    middle-ellipsize keeping the program/leaf name (`C:\Pr…\pwsh.exe`); tab
    labels switched from head-priority to the same middle-ellipsis so the program
    name stays visible.

## [2.23.2] — 2026-06-14

  ### Fixed
  - **Animated background burned CPU at idle.** A focused animated
    `background-image` repainted at a fixed 30 fps regardless of the GIF's own
    frame rate, and `request_redraw` was issued every event-loop iteration
    (level-triggered), so winit redrew continuously — **~55–60 % of a core**
    while an animated wallpaper was visible. The bg redraw is now
    **edge-triggered** (requested only when the displayed frame index changes,
    like the cursor blink) and the loop wakes at the GIF's own frame boundary
    (`bg_next_frame_ms`). Measured **20.9 %** for the same 8 fps loop (~2.7×
    less); a still/solid background stays at the ~3.8 % present-bound idle.
    Re-ran the cross-terminal suite: throughput is unchanged and still beats
    Alacritty and WezTerm on all payloads (the wallpaper pass + GPU default
    don't touch the parse path). See [docs/PERFORMANCE.md](../PERFORMANCE.md).

## [2.23.1] — 2026-06-14

  ### Fixed
  - **Overlay lingered after closing.** Closing the settings panel (or any text
    overlay — command palette, search, context menu) left it drawn on screen
    until the next keystroke. The v2.21.0 damage gate skipped the glyphon
    `prepare` on the close frame (overlay already gone + nothing else changed),
    so the just-closed overlay's cached text vertices kept rendering. The gate
    now tracks the previous overlay-open state and forces one clearing prepare on
    the open→closed transition.

  ### Changed
  - **The sample background is now a slow forward-flight starfield.** Replaces
    the previous look with stars that emerge near the center and drift gently
    outward as you move forward ("warp at low speed"), then fade as they pass —
    slow, sparse, and dark so text stays readable, and a uniform radial field so
    it looks right at any aspect ratio. `scripts/gen-starfield.py` +
    `docs/examples/space-starfield.gif` updated; off by default as always.

## [2.23.0] — 2026-06-14

  **Animated-background polish + a cross-platform GPU picker.**

  ### Background images
  - **Opaque chrome over the wallpaper (bleed-through fix).** The background
    image now draws in its own pipeline at the very back, so cell backgrounds,
    the tab bar / status bar / per-pane titlebars, and pane borders composite
    *opaquely on top* of it — the standard kitty/WezTerm/Alacritty layering. The
    animation no longer shows through the tab bar, and colored cell backgrounds
    (selections, syntax panels, TUI apps) are no longer hidden under an opaque
    wallpaper. (Pre-2.23.0 the wallpaper shared the inline-image pass and drew
    *after* the quads.)
  - New **`chrome-background`** config: the opaque chrome color over a wallpaper
    — `theme` (default) · `auto` (the wallpaper's average color, auto-adjusted to
    keep the tab text readable) · `black` · `white`.
  - **Settings → Appearance** now surfaces **Background** (`background-type`) and
    **Background animation** (`background-animation`) so the focus behavior is
    discoverable (the image *path* stays a config line).
  - New **[docs/BACKGROUNDS.md](../BACKGROUNDS.md)** — a walkthrough plus
    curated, clearly-licensed wallpaper sources (NASA SVS public-domain loops,
    OpenGameArt CC0). Nothing is bundled; backgrounds stay **off by default**.

  ### GPU selection (Settings → Graphics)
  - **Default GPU is now the discrete / dedicated adapter** (`gpu-power-preference
    = high`). kettle renders on the dedicated GPU out of the box for more headroom
    (animated backgrounds, large/high-DPI windows, many panes). On a dual-GPU
    laptop this wakes the discrete GPU (~1.5 s of extra cold start on the
    reference Surface Book 3); set `gpu-power-preference = low` for the fastest
    cold start. Single-GPU machines are unaffected.
  - **Pin a specific GPU** — new `gpu-vendor-id` / `gpu-device-id` / `gpu-name`
    keys and a **Settings → Graphics** picker that lists the GPUs detected on this
    machine. The resolver matches `(vendor,device,backend) → (vendor,device) →
    name` among surface-capable adapters and falls back to the power-preference
    policy if the pinned GPU is gone — a stale pin never fails startup. kettle's
    cross-platform answer to the OS GPU picker, and it persists per-app.
  - New `gpu-backend` (`auto`/`dx12`/`vulkan`/`metal`/`gl`) and
    `gpu-force-software` (debugging) keys.
  - GPU changes apply on the **next launch** (the device/surface graph can't
    hot-swap, and every window shares one adapter); the panel shows the **active**
    GPU and a "⚠ restart to apply" hint after a change.

  Render/config only — Claude Code CLI / AstroNvim / Tmux behavior, MSRV (1.89),
  and the cross-platform builds (Windows / Ubuntu / macOS / aarch64) are
  unchanged. Throughput, idle CPU, and the damage gate are untouched (the new
  wallpaper pass is one quad; the GPU picker is startup-only).

## [2.22.0] — 2026-06-13

  **Animated (GIF / APNG / animated WebP) backgrounds — a "video"/space-loop
  background, done natively and performantly.**

  - `background-image` now plays **animated** GIF / APNG / animated WebP as a
    moving background (it was first-frame-only before). Point it at a slow
    space/nebula loop for the "video background" look people set up on Ghostty —
    except kettle does it **natively** (no transparency + external wallpaper
    player, no GLSL shader). No mainstream terminal decodes a video *file* as its
    background; an animated GIF/WebP is the lean, cross-platform equivalent, and
    kettle already had the multi-format decoder + a per-frame animation clock to
    reuse.
  - New `background-animation` config: `when-focused` (default — animate only
    while focused, freeze at **zero idle cost** otherwise), `always`, or `off`
    (freeze on the first frame). Frames advance on the media's own timestamps,
    capped by the existing ~30 fps render tick — deliberately *unlike* Ghostty's
    custom shaders, which pin the GPU to a high frame rate even when idle.
  - Performance + safety: frames decode once (not per render); the imgpipe
    texture cache (keyed by `Arc::as_ptr`) re-uploads only when the displayed
    frame index changes; total decoded RGBA is bounded (`MAX_BG_ANIM_BYTES` +
    `MAX_BG_FRAMES`), degrading gracefully to first-frame-static on an oversized
    animation rather than OOMing; an unfocused window drops to `ControlFlow::Wait`
    (no busy-loop). Respects `background-opacity` / `background-blur` /
    `background-image-mode` per frame.
  - Reminder (unchanged): the *other* way to get a real video background — a
    transparent window over an external desktop video wallpaper — already works
    via `background-opacity < 1.0`.

## [2.21.1] — 2026-06-13

  **Flood throughput 2.0–2.4× — kettle now beats Alacritty and WezTerm on all
  three payloads (and Windows Terminal on ascii).** Measured on the reference
  Surface Book 3 (Intel Iris Plus, Win11), release build, medians of 5, same
  harness as the table below.

  - **Adaptive output-paint budget under sustained flood.** Under a flood,
    kettle's main thread was grabbing each pane's `Term` mutex ~60×/s to take an
    O(cells) `PaneSnapshot` — the *same* mutex the PTY reader thread needs to run
    `Processor::advance` — so the parser was starved on a CPU-contended machine.
    The output-paint budget now grows (60 → 30 → 20 fps) the longer a flood is
    sustained (`effective_output_budget`), handing the lock and the cores back to
    the reader. On-screen flood content is unreadable scrolling anyway; a brief
    burst (< ~4 coalesced frames) never throttles, keystroke echo still paints
    immediately at 60 fps, and the counter resets the instant output drops below
    the budget so the settled post-flood frame paints within one budget. Every
    fast terminal coalesces paints under flood — kettle was simply
    under-throttling.

  | payload | v2.21.0 | **v2.21.1** | Windows Terminal | Alacritty 0.17 | WezTerm |
  |---|---:|---:|---:|---:|---:|
  | ascii | 1.90 | **4.57 MB/s** | 4.33 | 3.59 | 2.56 |
  | sgr-heavy | 1.63 | **3.70 MB/s** | 4.12 | 3.06 | 2.67 |
  | unicode/CJK | 3.48 | **7.00 MB/s** | 9.04 | 5.79 | 5.03 |

  kettle is now **#1 on ascii** and **#2 on sgr/unicode** (behind only Windows
  Terminal, which runs in a shared `windowingBehavior = useExisting` process).
  Startup (~999 ms), idle CPU (~3.8%) and the rendered output are unchanged —
  the throttle only affects how often a *flood* repaints. Post-flood working set
  is a watch-item (faster consumption accumulates scrollback sooner).

## [2.21.0] — 2026-06-13

  **Startup 2.2× faster, damage-aware idle rendering, perf gate, dependency
  hygiene.** Measured on the reference Surface Book 3 (Intel Iris Plus, Win11),
  release build, medians of 5.

  - **Integrated-GPU adapter by default (the big startup win).** The live-window
    renderer requested its wgpu adapter with `PowerPreference::HighPerformance`,
    which on a dual-GPU laptop wakes the **discrete** GPU from its low-power
    state — ~1.5 s of pure cold-start cost for zero rendering benefit on a text
    workload. It now defaults to the low-power (integrated) adapter, cutting
    spawn → first-visible-window from **2202 ms to ~1000 ms (2.2×)**. New
    `gpu-power-preference` config key (`low` (default) | `high` | `auto`) lets a
    desktop user with an always-resident discrete card opt back in. (Trade-off:
    the integrated adapter keeps its buffers in system RAM, so the measured
    working set now *includes* GPU memory that the discrete path hid in VRAM.)
  - **Damage-aware idle rendering.** An idle repaint (cursor blink, bell-flash
    decay, focus dim) no longer re-runs the whole-viewport glyphon `prepare`,
    which re-encodes every visible glyph's vertices. `build_pane` now reports
    whether it actually reshaped a row; `render_frame` skips `prepare` (and the
    paired `atlas.trim`) when no pane row reshaped, no chrome label changed and
    no text overlay is open, re-rendering the cached vertex buffers instead.
  - **Block cursor decoupled from the pane buffer.** The inverted glyph under a
    focused solid block cursor is now drawn in a dedicated 1-glyph renderer ON
    TOP of the block, instead of being recolored INTO the pane text buffer
    (which dirtied the cursor row's shaping cache every blink). The pane buffer
    now stays byte-identical across a blink, so the prepare-skip above applies
    to a blinking block too. (Idle CPU on this hardware is now `present()`-bound
    at ~3.8 % with a blinking cursor — down from 28 % — the 2 vsync presents/sec
    a blink requires; the deadline-scheduled blink below is the dominant lever.)
  - Added `scripts/perf/score.ps1`, a cross-terminal perf gate that scores
    `perf-all.ps1` output across throughput, startup, idle CPU and memory, then
    fails unless kettle ranks in the top half, beats at least two peer
    terminals and stays within the configured regression threshold when a
    baseline result directory is supplied.
  - Live renderer startup now loads only the bundled Regular font face up front;
    Bold / Italic / Bold Italic are loaded lazily on the first frame that
    contains styled terminal text, with pane/chrome text caches invalidated so
    shaping sees the complete family.
  - Per-pane renderer caches are now keyed by process-global pane id, preserving
    text/titlebar buffers across split reorders and tab/window moves instead of
    cold-starting caches by visible index.
  - Startup visibility now reveals visible-state OS windows as soon as renderer
    init has configured the surface, then paints immediately, instead of
    waiting for the first full terminal frame before the window appears.
    `window_state = hidden` still remains hidden.
  - Idle cursor blinking now schedules the next redraw at the configured
    half-period deadline instead of polling every 120 ms and producing mostly
    unchanged frames between visible cursor toggles.
  - **Dependency / CI hygiene.** Bumped `actions/labeler` v5→v6 and
    `actions/stale` v9→v10; bumped the `cargo-machete` action to v0.9.2. Added a
    dependabot `ignore` for `dtolnay/rust-toolchain` (its `@1.89` ref is the
    MSRV pin, not an action release — dependabot mis-bumped it to a non-existent
    `@1.100`) and for `sysinfo` (0.39.x requires rustc 1.95, six releases past
    the declared MSRV 1.89, for a freshness-only bump with no advisory;
    `cargo audit` remains the security backstop).
  - Removed accidentally tracked local `kettle-target` cargo artifacts and
    ignored future `kettle-target` directories.

## [2.20.0] — 2026-06-12

  **Performance overhaul, measured.** A new committed cross-terminal
  benchmark harness (`scripts/perf/` — throughput / startup / idle CPU /
  memory / input latency, medians of 5, identical 1280×800 windows)
  established the v2.19.0 baseline: kettle parsed 0.42–0.8 MB/s under
  output flood vs 2.6–9.0 MB/s across Windows Terminal, Alacritty and
  WezTerm (WT, the fastest, 4.1–9.0) — 5–10× behind — with idle CPU
  near 56% of a core. Root causes
  were structural, and each fix below is the durable refactor, not a
  band-aid:

  - **Lock-free rendering (P2)** — `redraw` held every visible pane's
    terminal mutex across the whole GPU frame (text shaping +
    `get_current_texture`, which can block a full vsync + submit +
    present), so the PTY reader thread starved on `term.lock()` under
    flood. The renderer now works from a pooled **`PaneSnapshot`** — a
    µs-scale raw copy (cells verbatim from `display_iter`, cursor,
    selection, color table, grid dims) captured under the lock and
    rendered after it is released. The per-cell SGR/selection/cursor
    pipeline is byte-identical; the parser just never waits on a frame
    again.
  - **Per-line shaping cache (P1)** — every painted frame re-shaped the
    entire viewport of every pane through cosmic-text (`set_rich_text`
    resets all lines unconditionally), including idle cursor-blink
    frames. Pane text now keeps one `BufferLine` per grid row with a
    content key (run text + fg + bold/italic); only rows whose key
    changed are re-set, with `BufferLine::set_text`'s equality check as
    the second guard and a per-pane style key (font stack, ligatures,
    font-features, shaping mode) forcing full `reset_new` invalidation
    when shaping inputs change. An idle blink frame now re-shapes zero
    rows; a cursor move re-shapes one. Titlebar / tab / status-bar /
    glyph-button labels got the same equality gate (P1b).
  - **SIMD extractor fast path (P3)** — the image-protocol extractor
    walked the PTY stream byte-by-byte. It now `memchr`-scans to the
    next ESC / ST / BEL and bulk-copies plain runs (also collapsing the
    per-chunk realloc ladder into one exact reserve). First criterion
    benches in the repo (`cargo bench -p kettle-vt`: plain flood /
    SGR-heavy / OSC-spam) pin it.
  - **Wakeup dedup (P4)** — a flood enqueued one event-loop wakeup per
    64KiB PTY read, each fanning out across every window. An atomic
    latch now allows exactly one queued wakeup per paint window, with
    the latch reopened before generations are read so no output is ever
    missed.
  - **Recorder batching (P5)** — `dev-record` flushed to disk per event
    on the UI thread (the installed build records every session); it
    now batches through the BufWriter with a 250ms interval flush.
    Close-path flush is unchanged — clean exits still produce complete,
    replayable traces.
  - **Link-scan debounce (P6)** — the viewport URL regex pass re-ran on
    every painted frame during streaming (the scan key includes the
    output timestamp); output-only changes are now debounced to 150ms
    while focus/scroll changes still rescan immediately.
  - **Hot-path trims (P7)** — the per-read session-log mutex is skipped
    via an atomic flag when logging is off (new `Terminal::set_log_file`
    keeps flag and slot in sync).

  **Vim menu navigation (`vim-menu-nav`, default ON).** Every kettle
  menu and overlay is now traversable from the home row: context menu /
  new-tab ▾ dropdown and the Settings panel take `j`/`k` (wrapping),
  `g`/`G` first/last, `Ctrl+d`/`Ctrl+u` half-page; in the context menu
  and new-tab dropdown `h` goes back/pops a submenu and `l`
  drills-in/activates, while in the Settings panel `h`/`l` step the
  highlighted row's value (same as `←`/`→`); confirm dialogs answer to
  `y`/`n` (`y`
  confirms the question regardless of which button is focused); the
  command palette and layout picker move their selection with
  `Ctrl+j`/`Ctrl+k` (or telescope/fzf-style `Ctrl+n`/`Ctrl+p`) so
  letters keep typing; search steps next/previous match with the same
  chords — its first keyboard nav beyond Enter. Menu mnemonics
  auto-assignment skips the five nav letters while enabled (no row
  silently loses its hotkey; typeahead still works for everything
  else), and footer hints advertise the keys. `vim-menu-nav = false`
  restores the previous arrow-only behavior exactly.

  **Agents can now drive interactive apps.** The control plane (and its
  MCP tools) gained the two primitives that make vim / htop / fzf / tmux
  scriptable from Claude Code:

  - **`send_keys`** (full mode) — press named keys and chords:
    `["escape", "ctrl+c", "down", "G", "f5", "shift+tab", …]`.
    `send_text` could only type literal characters; this is how an agent
    presses Escape. Tokens use the keybind-trigger grammar plus the named
    keys it lacked (escape, backspace, delete, insert, space), parse
    entirely before any byte is written, and encode through the SAME
    `input::encode` path as human keystrokes against the pane's live
    terminal modes — DECCKM application-cursor arrows come out right in
    vim automatically. CLI: `kettle ctl send_keys --keys
    "escape,:,w,q,enter"`; MCP: `kettle_send_keys`.
  - **`wait_for`** (read-only) — block until a pane's screen contains a
    substring, matches a regex, and/or has been *quiet* (unchanged) for N
    ms; bounded by `timeout_ms`. Replaces sleep-and-pray agent scripting.
    Implemented on the ctl connection thread as a ≥50ms poll over cheap
    screen snapshots — the UI thread is never blocked. A timeout returns
    `matched: false` rather than an error. CLI: `kettle ctl wait_for
    --text "INSERT"`; MCP: `kettle_wait_for`.
  - **`read_screen`** now also reports `cursor_visible` (DEC ?25), so an
    agent knows when the reported cursor position is meaningless (vim's
    command line, fzf and less hide the cursor).

  **Terminator + Ghostty deep-dive → six integrations.** An 11-agent
  source-level analysis of both trees inventoried 130 features (42
  claims adversarially cross-checked against kettle source); the full
  ranked matrix — 39 now / 54 backlog / 37 reject, with next-cycle
  headliners like the kitty keyboard protocol and the terminal-reply
  layer — landed in docs/UX-COMPARISON.md. Shipped from the "now" tier:

  - **Resize overlay** (Ghostty `resize-overlay`) — a transient centered
    `cols×rows` chip while the window is resized. `always | never |
    after-first` (default `after-first`: every resize except the initial
    placement).
  - **OSC 7 cwd reporting in kettle's own shell integration** — the
    bash/zsh/fish/PowerShell snippets now report the working directory
    every prompt (percent-encoded, hostname-tagged), feeding new-tab/
    split cwd inheritance and "Open folder". The PowerShell snippet
    makes kettle one of the few terminals with first-class cwd tracking
    on stock Windows.
  - **`kitty-shell-cwd://` + hostname validation** — OSC 7 accepts
    kitty's raw-path scheme, normalizes URL-form Windows drive paths
    (`/C:/…`), and **rejects another machine's report**: an ssh
    session's shell integration reports the remote host's cwd, and
    adopting it locally broke cwd inheritance (Ghostty applies the same
    check).
  - **Prompt-aware close confirmation** (Ghostty `confirm-close-surface`)
    — panes sitting idle at an integrated-shell prompt (OSC 133 marks
    seen, no command running) no longer count toward `ask-before-closing`;
    a shell with no integration counts as busy, so behavior there is
    unchanged.
  - **Trigger capture groups** — `trigger = REGEX :: cmd {1}` now
    substitutes the match's capture groups into the spawned argv
    (`{0}` whole match), completing Terminator `run_cmd_on_match`
    parity. Substitution is value-only: argv stays argv, no shell.
  - **`equalize_splits` action** (bindable + in the palette) — rebalance
    the active tab's split tree to equal pane areas.

  **Adversarially reviewed before shipping.** A 52-agent, 7-dimension
  review of the full diff (render correctness, concurrency, security,
  agent-plane semantics, vim-nav UX, the six integrations, docs drift)
  raised 45 findings; 36 survived adversarial verification and every one
  was fixed in this release — among them: `wait_for` now detects a
  vanished client instead of pinning a connection slot for its full
  timeout (and pins its target pane against mid-wait focus changes, and
  no longer repaints idle windows or floods dev-record traces with its
  probes), the OSC 7 hostname check asks the OS for the real hostname
  (the env-var form silently failed open on Linux), session-restored
  panes ride the deduplicated wakeup path, `send_keys` normalizes
  `shift+letter` and honors `backspace-binding`/`delete-binding` like
  real keystrokes, trigger capture-group substitution is single-pass
  (matched output can no longer re-expand placeholders) and trims grid
  padding, OSC 133;B un-sticks command tracking under user
  `PROMPT_COMMAND`s, an idle verdict now also requires a working
  OutputStart source, CapsLock no longer disables the vim-nav layer,
  `G` in a 500-row theme list scrolls to a full last page, and the
  resize chip survives DPI changes, font reloads and restore-time
  placement storms. The 9 refuted claims are retained in the review
  artifacts with their refutations.

## [2.19.0] — 2026-06-12

  **Chromium-grade live tab tear-off + re-docking.** v2.18.0's tear-off
  created the window only at release, at a default size, with nothing
  visible mid-drag. v2.19.0 replaces that UX with the model Chrome uses —
  which Windows Terminal itself has not shipped (WT shows a ghost tab
  header and creates the window at drop; the live-window drag is its
  open wish, blocked on WinUI 3):

  - **Tear at the strip, not at release** — drag a tab ~1.5 bar-heights
    past the tab band (pure-distance hysteresis, uniform in every
    direction: dragging along the strip still reorders) and the tab
    tears off **instantly into a live window**. It inherits the source
    window's size (60% approximation from a maximized/fullscreen
    source), appears positioned so the pointer keeps holding the tab,
    and is immediately handed to the **OS-native move loop**
    (`drag_window()`: ReleaseCapture + `WM_NCLBUTTONDOWN`/`HTCAPTION` on
    Windows — the exact Chromium handoff — `_NET_WM_MOVERESIZE` on X11,
    `performWindowDragWithEvent` on macOS). **Snap Layouts / FancyZones
    / aero-snap work mid-drag** (verified live: drag-to-top maximizes);
    terminal output keeps streaming while the window rides the pointer.
    A manual-follow fallback covers platforms where the handoff is
    rejected. The torn window is re-anchored from the live cursor right
    before the handoff — the pointer slides during the ~100ms window
    creation, and the modal loop anchors at the current position
    (measured ~97px of drift without this).
  - **Re-docking** — drop a torn window onto any kettle window's tab
    band to merge it there. While hovering a band: the dragged window
    turns **translucent** (Windows, `WS_EX_LAYERED` alpha — verified
    against the wgpu flip-model swapchain) so the target strip stays
    readable beneath it, a **2px accent insertion line** marks the
    landing slot, and a hidden single-tab `tab-bar = auto` strip
    **materializes** so the target is visible before the drop. The merge
    attaches at the marked slot (insertion between segments, n+1 slots),
    focuses the docked tab, and closes the emptied window through the
    existing close funnel. Dock targets are resolved against the real
    z-order on Windows (cloaked-window aware), so a covered band can't
    false-match.
  - **Lone-tab windows drag whole** — grabbing the tab of a single-tab
    window drags the entire window (Chromium semantics; tearing would
    just re-create the same window), with full dock tracking attached:
    this is how a previously torn-off window merges back.
  - **Drop detection without polling** — winit synthesizes the
    left-release when the Windows modal move loop exits
    (`WM_EXITSIZEMOVE` → `WM_LBUTTONUP`, verified in the vendored
    0.30.13 source); X11/macOS commit on the first client pointer event
    after the WM's pointer grab ends (clients receive none during the
    move). A 30s failsafe abandons tracking whose drop signal was lost.
  - **Wayland** keeps the v2.18.0 tear-at-release path (compositors
    forbid client-side positioning and validate move serials against
    the press; `xdg_toplevel_drag_v1`, the real protocol for this, is
    not exposed by winit 0.30 — tracked follow-up). `Esc` before the
    tear still cancels everywhere; a within-slop release never tears.
  - Agent surface: tear-off and re-dock both emit the existing
    `tab_moved` event (`from_window`/`to_window`/`tab`).
  - Internals: `open_window` returns the new window's seq and takes an
    inner-size override; `WindowEvent::Moved` drives the dock hit-test
    (flows during the modal loop via `WM_WINDOWPOSCHANGED`); `TabBar`
    gains `insert_marker`; per-window `dock_preview` materializes auto
    bars; no new `event_loop.exit()` sites (allowlist unchanged at 6).

  **Hardened by a 35-agent adversarial review** (6 dimensions × verify;
  29 raw → 27 confirmed → 12 distinct fixes, 1 high):

  - **Esc-cancel of the OS move loop no longer commits the merge** (the
    HIGH): winit synthesizes the same left-release for a cancelled and a
    completed modal loop, and the latch survives the snap-back — the
    drop now checks the PHYSICAL primary-button state
    (`GetAsyncKeyState`, swapped-button aware): still held = Esc-cancel
    = abandon. X11/macOS commits additionally REVALIDATE the latch
    against the torn window's final resting position (a WM-cancelled
    move snapped it off the band). Verified live: Esc while latched
    snaps back with no merge; real drops still commit.
  - Manual-follow is **carrier-gated** (only the capture holder's cursor
    stream drives it — stale tracking can no longer hijack every
    window's mouse-motion features), other-button presses mid-drag are
    swallowed instead of killing the gesture, and a torn/carrier window
    dying mid-drag abandons its tracking eagerly (no leaked insertion
    marker / permanently materialized auto bar; every finalize early
    return now clears the latched preview too).
  - **Native→manual demotion**: a WM that accepts `drag_window()` but
    never actually moves the window (no `Moved` within 400ms while the
    capture holder still streams motion) demotes to manual-follow
    instead of leaving the torn window frozen mid-air; a 300ms
    post-handoff blackout absorbs stray pointer events racing the WM
    grab; the stale-tracking failsafe widened to 120s (a motionless X11
    hover is indistinguishable from staleness — patience is cheaper
    than aborting a live drag).
  - **Grab math corrected per `tab-bar-pos`**: the pointer now holds the
    torn window at its strip — inside the client (caption delta
    measured from the source), at the bottom/right edge for
    Bottom/Right bars (it used to hang the window off the wrong side of
    the pointer) — and macOS routes the torn-window handoff through
    manual-follow (`performWindowDragWithEvent` would consume a
    foreign window's NSEvent; the lone-tab whole-window drag keeps the
    native path since it owns the event).
  - Dock-preview **materialization is render-only** (no more PTY
    SIGWINCH spam when hovering across a single-tab auto window's band
    edge; the one real resize happens at the actual merge), the
    hidden-bar insertion slot now uses the geometry the bar will have
    once materialized (the marker can't flip slots as the bar appears),
    the x-only drag-reorder is gated off vertical bars (it shuffled
    tabs bogusly from cursor.x), and both merge-close paths save the
    session like every other window-close path.

  Verified live on Windows 11 with scripted SendInput drags: tear →
  instant window at source size under the cursor → rides the OS loop →
  aero-snap mid-drag → re-dock onto a sibling strip at the marked slot
  with `ping -t` streaming uninterrupted through tear AND merge; the
  insertion marker pixel-verified (`#cba6f7` accent line at the slot
  boundary); Esc before the tear, Esc while latched, and within-slop
  releases all leave the tabs intact. The X11 threshold tear verified
  live under WSLg/XWayland (2 windows mid-drag, live PTY move); Wayland
  keeps the at-release path (WSLg's Wayland EGL is broken in the test
  env — failure precedes any v2.19.0 code). macOS uses the verified
  manual-follow path; the native handoff is a tracked follow-up
  pending real hardware.

## [2.18.0] — 2026-06-11

  **In-process multi-window + live tab tear-off (Windows Terminal parity).**
  One kettle process now hosts any number of OS windows:

  - **Drag a tab out of the window and it becomes a new window at the drop
    point** — the tab's panes (PTYs, scrollback, running programs) move
    LIVE; nothing respawns, pane ids and child processes are stable.
    `Esc` mid-drag (or focus loss) cancels. Verified with `ping -t`
    streaming uninterrupted across the move.
  - **`new_window` (Ctrl+Shift+I) opens in-process** (was: a separate kettle
    process). Closing one window leaves the rest running; the process exits
    with the last window. The GPU device is shared across windows (~17-25 MB
    swapchain + 4-16 MB glyph atlas of VRAM per additional window).
  - **`move_tab_to_new_window` is the same live move** by keyboard (the only
    route on Wayland). The old serialize-and-respawn handoff senders
    (SCM_RIGHTS socketpair / one-shot JSON file) are retired — they never
    transferred live PTYs; the `--tab-handoff` receive parsing stays one
    release for an upgrade-in-flight old sender, deprecated.
  - **Session v2**: `session.json` records EVERY window (tabs + geometry);
    restore reopens each window at its saved position, clamped to the live
    monitor layout (an unplugged monitor can't strand a window off-screen).
    Old session files load unchanged; new files mirror window 1 into the
    legacy fields so an older kettle still restores something sensible.
  - **Agent API (additive)**: `get_state` gains `windows` +
    `focused_window`; `list_tabs` / `list_panes` enumerate every window with
    a per-entry `window` field; an explicit `--pane N` resolves across all
    windows (pane ids are process-global now); new `tab_moved` event.

  **Peacock per-window accents — ON by default.** Every kettle window claims
  a distinct hue from the theme's accent pool (the user-visible chrome that
  already follows the accent: focused-pane border, active-tab strip, pane
  titlebars, drag ghost, menu/settings highlights). Same project → same
  starting hue; two live windows never share one while the pool has a free
  hue — coordinated across processes by a tiny presence registry
  (`<runtime>/kettle/instances`, dead entries auto-pruned, best-effort by
  design). A theme switch keeps each window's pool slot. Opt out with
  `accent-color = theme` (or `off`/`none`); a hex / `--accent` pins every
  window and skips the dedupe.

  **Windows Terminal dropdown parity.** The new-tab `▾` menu now lists, in
  WT's order: PowerShell (relabeled from "PowerShell 7"), Windows
  PowerShell, Command Prompt, the WSL distros, **Developer Command Prompt /
  Developer PowerShell for VS 2022** (auto-detected via `vswhere`), and
  **Git Bash** (registry → well-known dirs → `git.exe` on PATH) — then
  WT's bottom section: **Settings… / Command palette / About kettle**.
  **Ctrl+Shift+1..9 opens the Nth dropdown entry** (`new_tab_shell_N`; both
  the digit and its US-shifted symbol are bound, the font-zoom precedent).
  Menus show **right-aligned dimmed shortcut hints** computed from the LIVE
  keybind map — a rebind shows your actual chord; the right-click menu rows
  get hints for free. The dropdown is now **always visible on every
  platform** (supersedes the cycle-917 single-shell gating: the bottom rows
  mean it's never a one-item menu). New **About panel** (`about` action,
  also in the palette): version + git hash (exactly `--version`'s output),
  update status, copy-version / open-GitHub / open-release rows.

  **Held-key stutter fixed** (user report: holding a character stuttered in
  kettle but not Terminator). Typing echo only ever painted via the
  PTY-output coalescer's `WaitUntil` deadline, whose ~16 ms Windows timer
  granularity made the repeat cadence irregular. Echo arriving within 150 ms
  of a keystroke now paints immediately — `request_redraw` is
  vsync-coalesced, so this can't outpace the display — while non-input
  bursts (build logs, streaming) still coalesce to one paint per frame.

  **App icon cleanup.** The macOS-style titlebar strip + traffic-light dots
  are gone — a clean rounded window with the centered `>_` prompt, signature
  mauve border unchanged. `scripts/gen-icons.py` (new, committed) is a
  Pillow reproduction of the SVG that regenerates every artifact (Linux
  hicolor PNGs, the macOS iconset, the 7-resolution Windows `.ico`) on any
  host — the rsvg path in `gen-icons.sh` remains for Linux.

  **Fixed (latent, found while wiring the above):**

  - `--layout`, `--restore`, and `--tab-handoff` startup loads were dead
    since ~v2.15: `resumed()` consumed the whole CLI-options struct with a
    wholesale `mem::take` before the load gates read it (verified: a 2-tab
    `--layout` opened 1 tab). Live config reloads also lost the
    `-m/-f/-b/-H/-T` launch overrides through the same hole. Only the
    consumed-once `-e`/`-d` fields are taken now; regression-guarded.
  - Context-menu geometry measured rows with the integer PTY cell height
    while the renderer lays them out with the fractional cell size — at 9+
    rows the ~0.5 px/row drift clipped the last row and drew a phantom
    "more rows" scroll marker. Menu geometry now uses the renderer's
    fractional metrics.

## [2.17.0] — 2026-06-10

  **Terminator-parity sweep (full re-audit of the Terminator codebase against
  kettle; cycles 938-941).** A file-by-file review of GNOME Terminator
  (keybindings, config DEFAULTS, popup menu, CLI, plugins) found kettle at
  ~95% parity with a finite gap list — now closed:

  - **CLI window-state flags** (Terminator's option set): `-m/--maximise`
    (alias `--maximize`), `-f/--fullscreen`, `-b/--borderless`, `-H/--hidden`,
    and `-T/--title <t>` (pins the window title). Plus `--list-layouts` and
    `--list-profiles` to enumerate saved layouts/profiles from scripts.
  - **`cursor-fg-color` / `cursor-bg-color`** (Terminator's cursor color
    split): the block cursor is now a solid box that recolors the glyph under
    it (the standard inverted-cursor model every mainstream terminal uses —
    previously a 0.55-alpha translucent block), and the two keys control the
    recolored-glyph / box colors independently (`cursor-color` stays as the
    bg alias).
  - **Keybind-name aliases** so a Terminator config drops in unchanged:
    `page_up`/`page-up` (+ `_down`, `line` variants), `switch_to_tab_N`,
    `toggle_read_only` / `read_only`. **`search-wrap`** (default `true`)
    matches Terminator's wrap-around search toggle.
  - **Per-pane read-only** (Terminator's right-click "Read only"): a
    check item in the context menu, a `toggle_read_only` keybind action, and
    a command-palette row. While on, the pane drops *user* input — keystrokes,
    paste, drag-drop, broadcast, Lua `send_text`, `remote.cmd`, and agent
    `send_text`/`run_command` (those reply with a `read_only` error) — through
    a single `Pane::feed_input` gate (VTE `input-enabled` semantics: protocol
    replies like focus/mouse reports keep flowing), while the running
    program's output keeps rendering. The titlebar shows `[RO]`.
  - **URL-aware context menu** (Terminator's "Open link" / "Copy address"):
    right-clicking a detected hyperlink now leads the menu with **Open
    Link** / **Copy Link Address**. The URL is captured at menu-open time so
    fresh output scrolling the grid can't retarget the click; Open routes
    through the same guarded chain as Ctrl+click (Lua URL handlers →
    `custom-url-handler` → system open).

  **Adversarial review of the parity sweep (75 agents, cycle 942) — 14
  distinct confirmed findings fixed before release.** Highlights: the
  `-T/--title` (and `-m/-f/-b/-H`) launch overrides now survive a live config
  reload (any reload — including kettle's own theme/settings persistence —
  silently reverted them); `-H` now wins over `-m`/`-f` instead of being
  dropped (Terminator applies hidden last); launching fullscreen no longer
  desyncs `ToggleFullscreen` (one press exits); a wide (CJK/emoji) glyph
  under the solid block cursor gets a two-cell block (its right half used to
  recolor to an invisible color), and an OSC 12 runtime cursor color flips
  the under-glyph to reverse-video; the read-only gate now also covers
  Reset / Clear-scrollback / mouse-tracking reports (VTE input-enabled
  parity) and read-only panes keep their scroll position under broadcast;
  the URL menu rows no longer steal the menu's stable mnemonics ('p' fired
  Copy over a link) and only appear for clicks inside the focused pane;
  `search-wrap` is validated by `--check-config`; a config-level
  `palette = 4=#hex` now carries a derived theme accent along; `accent-color
  = auto` dedups its candidate hues and canonicalizes the cwd before
  hashing; agents see a pane's `read_only` state in `list_panes`.

  **Theme-aware UI accent + Peacock `accent-color = auto`.** The UI chrome
  accent (active-tab strip, focused-pane border, per-pane titlebars, settings/
  menu highlights) now derives from a new theme-level **`accent`**:
  Catppuccin Mocha sets it to its signature **mauve `#cba6f7`** — the same color
  as the app icon — so the whole window reads as one accent instead of the icon
  being mauve while the chrome stayed ANSI-blue. Themes that don't declare an
  `accent` keep their `palette[4]` (blue), so nothing changes for them. A new
  **`accent-color = auto`** is VS Code Peacock parity: it varies the accent by
  *working directory*, so a kettle window opened in a different project is a
  visibly different (but per-project stable) color — handy for telling several
  windows apart. An explicit `accent-color = <hex>` or `--accent` still wins.

## [2.16.0] — 2026-06-09

  **Agent-first kettle — AI agents work great with kettle both interactively
  and programmatically.** Three new non-GUI entry points, all opt-in; the
  control surface is OFF by default. See [docs/AGENT.md](../AGENT.md).

  - **`kettle exec -- <argv…>`** runs a command headlessly under a real PTY (full
    VT emulation, no GPU/window) and streams its output to real stdout,
    propagating the child's exit code (124 on `--timeout`, 125 internal). Output
    modes: raw, `--strip-ansi` (plain text), `--json` (NDJSON events); optional
    `--record <path.cast>`. The headless counterpart to the GUI. (The session
    recorder was promoted into kettle-core behind an `asciicast` feature so the
    GUI's `--record` and `kettle exec --record` share one implementation.)
  - **Control server + `kettle ctl`.** `agent-server = off | read-only | full`
    (config or `--agent-server`, default **off**) starts a local-IPC server (Unix
    socket `0600` / Windows named pipe, current-user; no TCP) that `kettle ctl`,
    `kettle mcp`, or an AI agent can drive: `get_state`, `list_tabs`,
    `list_panes`, `read_screen`, `subscribe` (read-only) and `send_text`,
    `run_command` (full). `run_command` correlates the shell's OSC 133 command-end
    to report the exit code, with a `timed_out` + shell-integration hint fallback.
  - **`kettle mcp`** is a Model Context Protocol server (stdio) exposing kettle as
    native agent tools — `kettle_run` (headless one-shot), plus
    `kettle_list_panes` / `kettle_read_screen` / `kettle_send_text` /
    `kettle_run_command` against a running kettle. Register with Claude Code:
    `claude mcp add kettle -- kettle mcp`. `kettle mcp --self-test` is a CI guard.
  - A pane an agent has attached shows a configurable titlebar badge
    (`agent-badge`, default `"[agent] "`). Every connection + mutating method is
    logged and annotated in the dev-record trace.

  **Whole-codebase production review (76 agents, adversarially verified — 40
  confirmed findings fixed)** hardened the new agent surface and caught latent
  bugs in mature code. Highlights: `run_command` now returns the real captured
  output (it sliced the wrong grid region — empty for any command whose output
  fit on screen — and now submits with CR, the Enter key under ConPTY); the MCP
  `kettle_run` tool always bounds the child with a timeout (a non-exiting child
  can't wedge the server); the Windows control pipe uses
  `FILE_FLAG_FIRST_PIPE_INSTANCE` (refuses to bind onto a squatted name); the
  NDJSON readers enforce the 1 MiB line cap incrementally; the recorder stitches
  multibyte UTF-8 split across PTY reads; plus pre-existing fixes
  (`hints::labels` hung on a single-char alphabet; `wsl ~` / `--distribution-id`
  shell-classification; `extract_tab` active-index; scaled-zoom stale font).

  **Cycle 920 — UI chrome colors derive from the theme.** A focused
  adversarially-verified audit of every UI/UX color (render passes + config
  color defaults) found that while most chrome already cascades through the
  theme, a few elements were baked to legacy literals that clashed with the
  Catppuccin Mocha default (and every other theme). Now theme-derived so they
  match whatever theme is set; explicit `*-color` config overrides still win:

  - **Search-match + quick-select highlight** no longer use a hardcoded
    TokyoNight amber (`#e0af68`) / bg (`#1a1b26`). `search-foreground` /
    `search-background` became `Option<Rgb>` defaulting to the theme (the
    background falls back to `theme.palette[3]` — the theme's yellow; the
    foreground to `theme.background`), so the *active* highlight now matches its
    theme-derived *inactive* sibling instead of clashing beside it.
  - **Per-pane titlebars (non-focused states)** dropped their Terminator/legacy
    literals: the broadcast bar mirrors the focused accent cascade
    (`→ theme.palette[4]`), the inactive bar uses the theme surface
    (`theme.palette[8]`), and the title/close-glyph text derives from
    `theme.cursor_text` / `theme.foreground` so it stays readable on the
    now-theme-colored bars (fixes black-on-dark + white-on-light contrast).

## [2.15.1] — 2026-06-09

  Post-v2.15.0 patch: a whole-codebase audit batch (cycle 919) + the cycle-918
  tail. Fixes the v2.15.0 session-restore data-loss path, ships the Catppuccin
  signature mauve icon, and syncs the docs.

  **Cycle 919 — whole-codebase production-grade audit** (8 dimensions —
  Rust safety, complexity/memory, testing, UI/UX states, cross-platform incl.
  macOS code, docs, CI/CD+install, architecture — each finding adversarially
  verified; 20 confirmed, 0 high/critical, false positives filtered):

  - **Fresh window no longer clobbers the saved session (the one real bug).**
    v2.15.0 made session *load* opt-in but left *save* unconditional, so a fresh
    (non-opted-in) window overwrote the very `session.json` that `--restore` /
    `restore-session = true` exists to recover. Both gates now route through one
    `should_restore_session` predicate (load and save agree) + a truth-table test
    + a source-order drift guard pinning that `--layout`/tab-handoff outrank a
    stale session.
  - **Cache dir split-brain fixed** to match the config dir: on Windows
    `cache_dir_from_env` now uses `%LOCALAPPDATA%` and ignores a stray `HOME`, so
    screenshots/crash logs don't land in `~/.cache` on a shell launch.
  - **`pwsh -EncodedCommand` / `-e` classified one-shot** (tools spawn
    `pwsh -e <base64>`), so splitting such a foreground pane falls back to the
    pane's shell instead of cloning a dead pane; `-ExecutionPolicy` stays
    interactive.
  - **Background-image self-heal (throttled).** A failed decode now retries at
    most every ~3 s, so a transient read error / in-place file fix recovers —
    without the per-frame re-decode of a broken path.
  - **Runtime theme/setting persistence failures now notify** instead of being
    silent (the change is live this session but would be lost on restart).
  - Tests + drift guards added for the cycle-917 `insert_split` stale-focus retry
    + caller orphan-reap, and a `Theme::default()` == bundled-Catppuccin-Mocha
    fingerprint guard.
  - Docs synced for the cycle-918 behavior changes (CONFIG.md theme default +
    `restore-session` key, ARCHITECTURE.md restore-is-opt-in + diagram, README /
    man page / example-config), and the release.yml version-consistency gate
    moved into the single-Linux `pretest` job (fails fast, blocks all platform
    builds on a version mismatch instead of letting macOS/Windows publish first).

  **Cycle 918 tail:**

  - **App icon → Catppuccin signature mauve.** The icon was already the Mocha
    palette but used Mocha's ANSI blue `#89b4fa` for the window border/caret,
    which reads almost identically to the old TokyoNight blue — so it looked
    unchanged. Recolored the border + caret to Catppuccin's flagship mauve
    `#cba6f7` (a core Mocha color) so the theming is unmistakable; regenerated
    all 19 rasters + the multi-resolution `.ico` from the SVG.
  - **`install.ps1` refreshes the Windows icon cache** (`ie4uinit -show`) after
    writing `kettle.ico`, so a re-install with a changed icon shows immediately
    instead of Explorer serving the stale cached bitmap (the "icon didn't update"
    symptom). The repo/installed/embedded assets were already correct — this is
    the cache-invalidation that was missing.
  - **CI `--profile` smoke pins `XDG_CONFIG_HOME`.** The v2.15.0 Windows
    config-dir change (use `%APPDATA%`, ignore a stray `HOME`) meant the smoke's
    `~/.config` test profile wasn't found by the Windows binary; it now sets the
    cross-platform `XDG_CONFIG_HOME` override so the test is OS-independent.
  - **Hero/showcase screenshots regenerated** for the Catppuccin Mocha default
    (they render the default theme via `--screenshot`); captions updated.

## [2.15.0] — 2026-06-09

  Session/theme UX overhaul + a small audit batch, all from live use on the
  maintainer's machines. Cycle 918. The headline: kettle now opens **fresh
  windows by default** (like every mainstream terminal) and the **theme is
  config-governed**, fixing a class of "my setting didn't take" surprises.

  - **Session restore is opt-in (fresh windows by default).** Previously every
    launch — including a second concurrent instance — restored the full split
    tree + working dirs from `session.json`, so opening a new window cloned your
    layout and two windows raced to write the session. That is non-standard:
    GNOME Terminal, Windows Terminal, kitty, Alacritty, WezTerm, and iTerm2 all
    open a fresh window (single pane, default cwd). kettle now matches them. Opt
    in to "continue where you left off" with `restore-session = true` (config) or
    `--restore` (one-shot). The session is still saved on exit, and `--layout
    NAME` / tab-handoff remain explicit restore paths.
  - **Theme is config-governed; a stale session no longer overrides it.** The
    theme was persisted in `session.json` and *overrode* the config/compile-time
    default on restore — so after the default changed (or for a user with any
    prior session) the old theme stuck (the "theme didn't update to Catppuccin"
    report). The theme now lives in the config `theme =` line (with the
    compile-time default as fallback); every runtime theme change — the Settings
    picker, the right-click Theme submenu, Next/PrevTheme, light/dark toggle —
    persists there. The session no longer stores or applies a theme (the field is
    kept only so older `session.json` files still parse).
  - **Windows config-dir split-brain fixed.** `Config::default_path` fell back
    `XDG_CONFIG_HOME → HOME/.config → APPDATA`. On Windows a stray `HOME` (git-bash
    / MSYS / WSL-interop all export one) sent a shell-launched kettle to
    `~/.config` while a Start-menu launch used `%APPDATA%` — two different config
    + session files, so settings (and the theme) appeared to randomly not apply.
    Windows now uses `%APPDATA%\kettle` for the non-XDG fallback (ignoring `HOME`,
    a Unix idiom); `XDG_CONFIG_HOME` is still honored everywhere as the explicit
    override. Unix behavior is unchanged.
  - **`--list-actions` no longer hides bindable actions.** `insert_pane_name` and
    `open_cwd_in_file_manager` had `from_name` aliases (and tests) but were absent
    from the hand-maintained discovery list. Added, plus a reverse-coverage drift
    guard so a future omission fails CI.
  - **Background-image cache: clear on decode failure.** When the bg-image path
    changed to one that fails to decode, the renderer kept showing the *previous*
    wallpaper and re-attempted the failing decode every frame. It now caches the
    failed (path, blur) key so a stale image never renders and the broken decode
    isn't retried per-frame.
  - **`pwsh -NoExit -Command …` is treated as interactive.** The cycle-917
    one-shot-shell guard flagged any `-Command`/`-File` pwsh as non-interactive;
    `-NoExit` keeps the session open, so such an invocation is now correctly
    interactive (a benign false-positive that fell back to the pane's own shell).

## [2.14.0] — 2026-06-09

  Three user-reported Ubuntu bugs fixed (each reproduced with a failing test
  first, then fixed), plus two requested changes: the new-tab shell dropdown is
  hidden when only one shell exists, and **Catppuccin Mocha is now the default
  theme + icon**. Cycle 917.

  - **Directional pane navigation now matches Terminator/tmux (#1).**
    `Mux::focus_dir` ranked candidate panes by Euclidean distance between pane
    *centers*, so in a nested layout focus would jump to a *diagonal* pane and
    skip the one directly adjacent (reproduced from the user's screenshot: from
    the bottom-right pane, Left landed on the diagonal mid-left pane). Rewritten
    to the standard edge-adjacency rule — only panes that border the focused
    pane on the pressed side **and** overlap it on the perpendicular axis are
    candidates; the smallest primary-axis gap wins, tie-broken by perpendicular
    proximity. A corner-touching diagonal pane is never a neighbor. Five new
    tests, including the exact screenshot tree with the old algorithm inlined to
    pin the bug.
  - **A split always yields a working interactive shell (#2).** Splitting a pane
    running Claude Code (a `node` process) could spawn a new pane whose terminal
    "never loaded": the clone-foreground-shell feature did a deepest-descendant
    walk and would clone a transient non-interactive `sh -c "…"` helper that
    `node`/`nvim` spawn for tools, which runs its command and exits instantly.
    Detection now rejects one-shot invocations (`-c`/`-lc`/`-ic`,
    `pwsh -Command`/`-File`, `cmd /c`, `wsl … <command>`) across shell families,
    and the split boundary re-checks the contract — a non-interactive argv falls
    back to cloning the pane's own launch shell, so a split can never produce a
    dead pane. Hardened the split tree too: `insert_split` now repairs a stale
    focus and reaps the just-spawned pane (instead of leaking it and reporting
    success) if the graft fails, and `close_focused` gained the `contains(focus)`
    guard `reap_tabs` already had. (A foreground-process-group `tcgetpgrp` check
    is noted as a future refinement.)
  - **Hyperlink underlines no longer ghost when scrolled (#3).**
    `kettle_core::links()` scanned the active screen (`grid[Line(row)]`),
    ignoring `display_offset`, so scrolling Claude Code up painted the active
    screen's link underlines over the scrolled-back history ("leftover/ghost
    underlines"). It now reads the visible viewport (`Line(row − display_offset)`)
    — the sibling the cycle-912 decoration fix missed. Also fixes click-to-open
    landing on a link while scrolled. Harness regression test.
  - **New-tab `▾` dropdown hides when there's only one shell (#4).** On a stock
    Ubuntu with just `bash`, the shell-picker arrow opened a pointless one-item
    menu; it's now hidden (zero-width → dropped from the render pass and the
    click hit-test) when `detect_shells()` finds ≤ 1 choice. Windows always has
    multiple launch targets (cmd / pwsh / WSL) so the arrow stays there; the Unix
    count is a cheap PATH probe cached per process.
  - **Catppuccin Mocha is the default theme + icon (#5).** The shipped default
    moves from TokyoNight Night to Catppuccin Mocha (the darkest Catppuccin
    flavor) — `Theme::default()`, `Config::default()`, and the bundled-name
    resolution. The window/launcher icon is recolored to match: the source SVG
    and every raster (Linux hicolor PNGs, the macOS `.iconset`, the multi-res
    Windows `.ico`, and the embedded winit window icon) regenerated in Catppuccin
    Mocha. A user can still pick any of the 500+ bundled themes via `theme =`,
    the Theme submenu, or `NextTheme`/`PrevTheme`.

## [2.13.0] — 2026-06-08

  Post-v2.12.0 batch: the cycle-912 audit tail (cycles 913–915) plus a literal
  whole-codebase file-by-file review (cycle 916) that found defects the
  8-dimension audit missed — including a bracketed-paste injection-guard bypass,
  a third missed `display_offset` site (the cursor block over scrollback), and a
  cycle-904 divider-drag regression that left child PTYs unresized.

  - **Whole-codebase file-by-file review (cycle 916).** A literal pass over every
    one of the 49 source files (82 reviewer/verifier agents, 40 adversarially
    confirmed findings, 0 release-blocking high) — distinct from the cycle-912
    8-dimension audit and it earned its keep, catching real defects in
    recently-touched code that the dimension lens missed. 18 fixed this cycle:
    - **Bracketed-paste injection guard** (`input.rs`) — the single-pass
      `.replace` strip was defeatable by *overlap reconstruction* (a crafted
      clipboard like `\x1b[20\x1b[201~1~` re-forms an intact closer across the
      splice seam, ending paste early so the tail auto-runs). Now a fixpoint loop;
      the only finding with a real external attacker. + overlap regression test.
    - **Cursor `display_offset`** (`render lib.rs`) — a THIRD missed cycle-912
      site: the cursor block painted at its grid-absolute line, so when scrolled
      back a phantom cursor rendered over scrollback. Now viewport-converted
      (`cvrow = line + display_off`) with on-screen visibility folded into the gate.
    - **Divider-drag PTY resize** (`app.rs`) — cycle-904 regression: dragging a
      split divider changed the layout ratio but never `resize_all`'d, so child
      TUIs kept stale cols/rows (clipped) until an unrelated event. Now resizes.
    - **Mouse-tracking clamp** (`app.rs cursor_cell`) used the zero-inset grid, so
      a bottom-edge click in a titlebar'd split reported one row past the PTY's
      last (cycle-817 class) — now clamps against the inset grid.
    - **Cross-platform / untrusted-PTY hardening:** bg-image `~/` now expands via
      USERPROFILE on Windows (was HOME-only → silently never loaded); Sixel HLS
      components clamped like RGB (cycle-860 parity); `FILE://` accepted
      case-insensitively; iterm + kitty base64 strip embedded whitespace
      (line-wrapped payloads); DCS Sixel detection requires a numeric `params q`
      prefix so DECRQSS (`$q`) / XTGETTCAP (`+q`) aren't eaten as spurious images;
      a kitty animation with all-zero gaps no longer pins a 30 fps redraw forever;
      a Lua 256 MiB heap cap (a native `string.rep` can't OOM-abort kettle now).
    - **Correctness/UX:** keybind `insert-pane-number` / `insert-pane-padded`
      kebab aliases accepted; `--check-config` no longer flags the valid
      `palette = NAME` named-preset form; remote process-tree tie-breaks sorted by
      PID (deterministic split-clone / pane-title target); hints IP regex clamps
      octets to 0–255; the byte→column map no longer over-selects a token ending
      in a multi-byte char; `KETTLE_RECORD_RAW_INPUT` is bool-parsed (was
      any-value-enables — a password-capture footgun; dev-record builds only).
    - Deferred/tracked lows: MAX_SEQ vs image caps, place_image>256 cells,
      persist_config duplicate-line edge, `--config` introspection-flag gate,
      `--list-actions` rows, dev_record UTF-8-split, bg cache stale-on-fail,
      unfocused-dim OSC-11, session WSL `--cd` restore, `images::prune` dead code.

  - **Config/CLI (cycle 913, cycle-912 audit tail).** Three small correctness/
    robustness fixes deferred from the audit: `--check-config` no longer
    false-positives on a `font-feature` trailing comma / empty token (`liga,`,
    `liga, , calt`) — it now skips empties like the apply path already does;
    `--screenshot` and `--screenshot-menu` are mutually exclusive (clap
    `conflicts_with`) instead of silently dropping one; and a whitespace-only
    `-e`/`--exec` program name (`kettle -e ""`) fails loudly at the CLI surface.
    Test: `cli_screenshot_flags_are_mutually_exclusive`.

  - **Config (cycle 914, audit tail).** `append_keybind` (the interactive keybind
    editor's persistence) now de-dups SEMANTICALLY via `parse_trigger` and splits
    the value on the LAST `=` like `apply_keybind` — so re-binding a chord written
    in a different case (`ctrl+alt+r` vs `Ctrl+Alt+R`) or a literal `=` chord
    (`ctrl+==action`) overwrites the old line instead of stacking a stale
    duplicate (the old first-`=` string compare missed both). Test:
    `append_keybind_dedupes_by_semantic_trigger`; plus a stale `BOOL_KEYS`
    doc-reference corrected. Remaining tracked low-severity audit items —
    per-frame Vec pooling (bounded/low-impact perf), `images::prune` wiring
    (memory already bounded by the 512-placement cap), and the app.rs god-object
    extraction (a multi-cycle incremental refactor) — deferred for a future cycle.

## [2.12.0] — 2026-06-08

  Whole-codebase audit batch (cycle 912): a 45-agent, 8-dimension production-grade
  audit with adversarial per-finding verification surfaced 26 confirmed issues
  (0 release-blocking high). The dominant theme: the v2.11.0 R1 `display_offset`
  fix was INCOMPLETE — it corrected the copy coordinate but the same
  grid-absolute-vs-viewport bug recurred at four more sites. All mediums + the
  security/quick-win lows are fixed here; a handful of genuinely-low items
  (per-frame Vec pooling, `append_keybind` `=`-hygiene, `images::prune` wiring,
  a few CLI-validation niceties, the `font-feature` diagnostic asymmetry, the
  app.rs god-object extraction) are tracked for a follow-up cycle.

  - **Selection/copy/render (cycle 912) — finish the R1 `display_offset` fix
    across all 5 sites.** v2.11.0 converted the mouse-selection coordinate but
    missed: (1) **vi-mode visual yank** (`yank_vi_selection`) and (2) **URL/
    quick-select hint detection** (`collect_hints`) both indexed the grid with a
    raw viewport row — so while scrolled back they read the active screen, not
    the visible history (silent wrong/empty yank, hint over the wrong URL); and,
    in the renderer, (3) the **per-cell bg / underline / strikethrough quads** and
    (4) the **selection-background highlight** positioned by the raw grid-absolute
    line (`display_iter` and `content.selection` are grid-absolute, negative in
    history) — so cell backgrounds/decorations detached from the text and a
    scrolled-back selection's highlight was DROPPED (the `r < 0` guard). All four
    now convert with `viewport_row = grid_line + display_offset` (alacritty's
    `point_to_viewport`); the cursor path was verified already viewport-relative.
    No-op at the bottom (`display_offset == 0`), so no regression. New kettle-core
    contract test `display_iter_is_grid_absolute_so_render_adds_display_offset`
    locks the render invariant.

  - **Pane lifecycle (cycle 912) — `exit-action = hold` was silently broken.**
    `reap()` removed any pane whose child had exited regardless of intent, so the
    documented Hold behavior (keep the dead shell on screen) acted exactly like
    Close — the pane vanished on the next event-loop turn. Added a `held` flag
    (set on the Hold arm), a pure `is_reapable(closed, held, child_exited)`
    predicate (`closed || (!held && child_exited)`), and a unit test. A held pane
    stays until explicitly closed (which sets `closed`).

  - **WSL (cycle 912) — new-tab ▾ dropdown dropped the inherited cwd.**
    `open_tab_with_argv` called `new_tab_with` (a raw spawn) directly, bypassing
    the `launch_cwd` WSL `--cd` translation that splits/duplicates use — so a WSL
    entry's Linux cwd failed the Windows `is_dir` gate and the new tab fell back
    to `~` (the cycle-887 regression class). Added `Mux::new_tab_with_launch`
    (mirrors `split_with`) and routed the dropdown through it.

  - **CI/CD (cycle 912).** The `dev-record` CI leg filtered tests by the
    `dev_record::` module path, so the cycle-908 completeness guard (in
    `app::tests::`) compiled but never RAN — now runs all `--features dev-record`
    tests. And `release.yml` passed `generate_release_notes: true` on all four
    matrix legs, duplicating the release body 4× (v2.6.0–v2.10.0) — now generated
    on exactly one leg (gated on the artifact name, since `runner.os == Linux`
    matches both ubuntu legs); the others attach assets only.

  - **Untrusted-PTY hardening (cycle 912, kitty images).** Capped the kitty APC
    control-string half at 4 KiB (`KittyState::feed`) so a multi-MB control prefix
    with no `;` can't amplify into a huge transient HashMap in `parse_control`;
    and the kitty `f=24` RGB arm now validates the payload length against the
    declared dimensions BEFORE the 4/3 RGBA expansion (a mismatched 1×1 claim with
    a large payload no longer wastes a payload-sized alloc + copy). Both
    drift-guarded.

  - **Rendering (cycle 912).** Added a source-level drift guard pinning the
    output-coalescer's flush-before-wait-clamp ordering in `about_to_wait`
    (anti-busy-spin invariant). Doc corrections: `docs/TESTING.md` (the no-PTY
    harness was mislabeled "PTY-driven"; release asset count six → up to eight),
    `.github/workflows/ci.yml` (stale 1.88 MSRV comment → 1.89), `scripts/
    release.sh` ("Next steps" now watch-THEN-verify the run conclusion, not the
    unreliable `gh run watch` exit code), `docs/ARCHITECTURE.md` (removed an
    unfilled "cycle X" placeholder).

## [2.11.0] — 2026-06-08

  Claude-Code-CLI GUI batch: fixes the two GUI bugs that only showed up running
  the Claude Code CLI inside kettle — copying earlier output returned the
  wrong/truncated/empty text, and the cursor flashed above the prompt under load
  — plus a deterministic end-to-end harness so both stay fixed. Root causes were
  pinned against a real recorded session (`?2026`=0, `?1049`=0, `?25l/h`≈1750:
  Claude Code repaints non-atomically on the MAIN screen, without synchronized
  output).

  - **Selection & copy (cycle 909) — translate clicks by `display_offset` so
    copying while scrolled reads the right rows.** Every selection-creation site
    (`begin_selection`, `update_selection`, `extend_selection_to_cursor`,
    `apply_smart_selection`) and the smart-select grid-row read built the
    alacritty `Selection` / indexed the grid from a *raw viewport line*, but
    alacritty's `Selection` / `selection_to_string` / `to_range` expect
    GRID-ABSOLUTE points (its own frontend calls `viewport_to_point` first:
    `absolute = viewport − display_offset`). At the bottom (offset 0) the two
    coincide so it worked; scrolled back by N it stored `Line(v)` instead of
    `Line(v − N)`, so `selection_to_string` read the wrong/empty rows (the
    wrong/truncated/empty copy) and the highlight slipped down by the scroll
    amount — most visible in Claude Code, where you constantly scroll up to
    select earlier output. Fixed with one shared `viewport_point_to_grid`
    converter (re-exporting alacritty's `viewport_to_point` from kettle-core)
    routed through every selection + grid-row-read site; viewport-relative paths
    (mouse-event reporting to vim/tmux, chrome hit-testing) are untouched. Tests:
    pure `viewport_point_to_grid_applies_display_offset` (kettle-ui) +
    `selection_while_scrolled_reads_visible_row_not_active_screen` and
    `simple_drag_selection_while_scrolled_copies_visible_rows` (kettle-core,
    which also show the buggy raw-viewport path reading the wrong row).

  - **Rendering (cycle 910) — coalesce PTY-output paints so non-2026 apps don't
    tear.** Apps that repaint without DEC 2026 synchronized output (Claude Code
    toggles cursor hide/show ~1750×/session and never opens 2026) could be
    snapshot mid-repaint when a burst of 64 KB reads each triggered an immediate
    `request_redraw` — the transient "cursor above the prompt" under load. kettle
    now caps output-driven paints to one per `OUTPUT_FRAME_BUDGET` (~16 ms — a
    60 fps cap, the standard display-refresh target): a same-budget wakeup sets
    `coalescing_paint` and `about_to_wait` flushes the settled frame at the
    deadline, so a multi-read repaint lands as one frame. Beyond reducing tearing,
    the cap roughly halves the paint-side CPU a continuously re-rendering TUI
    (Claude Code's spinner / progress output) would otherwise burn vs an uncapped
    repaint. Input and cursor paints bypass the cap (they call
    `request_redraw` directly), so typing and the cursor stay immediate, and the
    existing 2026 honoring is unchanged (it already makes well-behaved TUIs
    atomic). It is a reduction, not a guarantee — an app that never uses 2026 can
    still tear a hair under extreme load. Pure test
    `output_paint_coalesces_within_frame_budget`.

  - **Testing (cycle 911) — deterministic end-to-end harness + `.cast` replay.**
    The no-PTY conformance harness (`harness()` + `feed_ex()`, the real
    Extractor→Processor→grid path) now covers the selection/copy bug classes
    above across `display_offset`, and a new
    `replays_asciicast_v2_output_into_grid` parses an asciicast v2 trace — the
    format `docs/DEV-RECORD.md`'s recorder writes — and feeds its output events
    through the pipeline, so a scrubbed real Claude Code / Codex / tmux session
    can be committed as a regression fixture and replayed with no PTY or auth.
    `serde_json` added as a kettle-core dev-dependency (not in the shipped
    crate). See [docs/TESTING.md](../TESTING.md) for the coordinate-space +
    pipeline mermaid diagrams.

  - **dev-record (cycle 908) — capture the session's full output, head and
    tail.** The recorder (a `dev-record` feature build, compiled out of releases)
    is fed PTY output ONLY by `drain_events()` on redraw, and it started *after*
    the first redraw and was never drained on close — so a session's opening
    output and its final in-flight chunk could be dropped (a fast `-e cmd`'s
    line, or bytes still queued when the user clicks X). A 16-agent adversarial
    audit + live close-path tests (graceful shell-exit, window WM_CLOSE, hard
    kill) confirmed: recording always STOPS cleanly on close and the trace is
    always valid/replayable (every event is flushed per-event; asciicast v2
    needs no footer) — the only gap was the completeness of the tail. Fixed by
    (a) starting the recorder before the first `redraw()`/drain, and (b) draining
    each pane's output sidechannel into the trace before reap and on close, with
    a brief *bounded* settle (only while a just-closed pane is present, so steady
    frames pay nothing) that lets the PTY reader push a just-exited shell's final
    bytes before its channel is dropped. Verified: real commands captured 5/5,
    all traces valid, clean stop. (A command that exits in <~50 ms — e.g.
    `cmd /c echo x` — can still have its output collapsed by Windows ConPTY's
    screen-differ and never emitted to kettle at all; that affects display too
    and is not a recorder issue.) Feature-gated drift guard
    `recorder_output_flushed_before_reap_and_on_close`; no effect on shipped
    (non-feature) builds.

## [2.10.0] — 2026-06-07

  Post-v2.9.0 self-review batch (cycles 878–884): an adversarial multi-agent
  review of the v2.9.0 changes surfaced 10 confirmed low/medium findings (no
  high-severity, none a user-facing functional regression) — all fixed here,
  toward the next release.

  - **Windows (cycle 878) — `is_inherited` clears last-error before GetFileType.**
    The `FILE_TYPE_UNKNOWN` arm tested `GetLastError() == 0` without the mandatory
    preceding `SetLastError(0)`, so a stale error code could misclassify an exotic
    UNKNOWN std handle. Now follows the documented Win32 pattern.
  - **Windows (cycle 879) — the update-available banner raises taskbar attention
    only when unfocused**, matching the bell path, so `attention_active` keeps
    meaning "a flash is actually outstanding" (it was latched even while focused,
    where `FlashWindowEx` is a no-op).
  - **Themes (cycle 880) — a theme picked in Settings now survives an unclean
    exit.** The Settings handler persisted to config + reloaded but never synced
    the session file, so a crash/kill/reboot before the next save let the stale
    session theme revert the pick on restart. It now calls `save_session()` like
    the NextTheme / context-menu / ToggleLightDark paths.
  - **dev-record (cycle 881) — the recorder uses a lossless (unbounded) output
    channel** instead of the lossy `bounded(64)` Lua sidechannel, so a fast output
    burst can't silently drop chunks and leave holes in the "verbatim" asciicast
    (same rationale as the already-unbounded event channel). Lua plugins keep
    their drop-on-full channel.
  - **Tests (cycle 882) — the synchronized-output (DEC 2026) test feeds through
    the real Extractor → Processor pipeline**, not the Processor directly, so a
    future Extractor change that swallowed the `?2026` toggles would actually fail
    it — the property its docstring promised.
  - **Docs (cycle 883) — theme count is now range-stable.** README / INSTALL /
    SETTINGS / CONTRIBUTING and code comments said "~512" after the bundle grew to
    532; they now say "500+" (and the INSTALL verify step shows `# 500+`), and a
    new floor-guard test asserts the bundle stays ≥ 500 so the docs can't silently
    drift again. The intra-section CHANGELOG "~512 vs 532" wording was reconciled.
  - **dev-record docs (cycle 884) — output-privacy caveat + discoverability.**
    docs/DEV-RECORD.md (and the `record_output` doc) now state plainly that the
    `o` channel captures on-screen output VERBATIM and cannot be redacted (review/
    scrub a `.cast` before sharing — the largest exposure surface); the doc is now
    linked from TESTING.md, which also documents the `dev-record` CI step.

  - **CI (cycle 885) — fix the red `build (windows-latest)` job + normalize line
    endings.** The Windows CI runner checks out with `autocrlf`, turning the
    LF-committed source into CRLF, which made one drift test's multi-line `\n`
    source-scan (`side_button_forward_is_modal_gated`) match 0 and fail — the
    Windows build had been red since v2.8.0 (masked because earlier releases
    watched `release.yml`, which builds the artifacts separately and was green).
    A new `.gitattributes` (`* text=auto eol=lf`) makes every checkout LF so all
    `include_str!` drift guards are deterministic cross-platform, and the test
    now strips `\r` before scanning (belt-and-suspenders). Restores green
    all-OS CI.

  - **Split now reproduces the shell you're actually in — including WSL (cycles
    886–888).** Splitting (or duplicating) a pane now clones the focused pane's
    shell + working directory instead of always opening the configured shell:
    - It clones the pane's launch command + cwd, so a pane opened as WSL / ssh /
      a specific shell (the `▾` new-tab dropdown, `-e`, or a `command =` config)
      splits into the same in the same dir. Default-shell panes are unchanged
      (their argv *is* the configured shell).
    - It also walks the focused pane's process tree (reusing the SSH/Docker
      scanner in `kettle-remote`): if you've **entered** a shell the pane
      launched — e.g. opened PowerShell, then typed `wsl` — the split clones
      THAT shell, in the same directory. WSL reports a Linux cwd a Windows shell
      can't `cd` into, so the dir is carried via `wsl --cd` using the pane's
      OSC 7 directory. Limited to known shells (wsl/bash/zsh/fish/pwsh/cmd/…),
      never an arbitrary foreground program like `vim`. Verified live: pwsh →
      `wsl` → split → WSL in the same repo dir.
    - Unit-tested: `launch_cwd` WSL `--cd` routing + non-WSL dir inheritance,
      `find_foreground_shell` (deepest-shell-wins, non-shell ignored, plain pane
      → none), and `argv_is_wsl` basename detection.

  Comprehensive whole-codebase audit batch (cycles 889–906): a 13-dimension
  multi-agent audit of every crate, feature, UI/UX state, and platform path
  produced 34 reviewer-confirmed findings (0 high after adversarial
  verification; 9 medium, 25 low) — all addressed here, each a durable fix with
  a unit / drift test and `just gauntlet`-green.

  - **Context menu (cycles 889–890) — mouse drill-in + keyboard parity.** Mouse-
    clicking a submenu row dismissed the menu instead of drilling in (the menu
    was nulled *before* the dispatch match, making the drill arm dead code); and
    keyboard Enter/Space + mnemonics only fired plain `Item` rows, so the
    new-tab `▾` dropdown, Lua items, config-commands, and theme/profile choices
    were keyboard dead-ends. All three input paths now route through one shared
    row→click mapper + dispatcher, and theme/profile leaves are keyboard-
    navigable. (medium ×2)
  - **Render — unfocused-pane OSC 11 backdrop (cycle 891).** An unfocused pane
    running a program that set its own background (OSC 11) painted no quad on
    its default-bg cells, so they leaked the focused pane's clear color. Each
    pane now paints a backdrop over its interior when its default bg differs
    from the surface clear color (border/titlebar-aware geometry, unit-tested).
    (medium)
  - **Render — background-image cache (cycle 892).** The decoded wallpaper (up
    to ~256 MiB) is now freed when the config leaves `background-type = image`,
    and the cache key includes the blur radius so toggling `background-blur`
    reloads even when the path is unchanged. (low ×2)
  - **Session restore — no leaked PTYs on partial failure (cycle 893).** When a
    split-tree's later pane failed to spawn, the panes already created for the
    same tree were orphaned in the mux (a leaked PTY + child process each).
    `build_node` now tracks spawned ids and reaps them on the restore error
    path. (medium)
  - **WSL split `--cd` ordering (cycle 894).** `--cd <dir>` was appended at the
    END of argv, so a launcher carrying a command (`wsl -d Ubuntu -- bash -l`)
    passed `--cd` to the *command*, not WSL — the dir was ignored. It's now
    inserted in WSL's option section (right after the launcher). (medium)
  - **Config validator parity sweep (cycle 895).** `--check-config`'s diagnostic
    now covers every alias the parser accepts, with bounds mirroring the apply
    clamps: `cursor-shape`/`cursor_shape` + `ibeam`/`i-beam`, `scrollback-limit`,
    `tab-silence-threshold-ms` / `command-notify-threshold-ms`, the
    `background-color`/`foreground-color` (`_color`) spellings, and padding now
    rejects `inf`/`nan` (which the runtime rejects). (medium + low ×5)
  - **`persist_config_toggle` rollback (cycle 896).** Contract point 5 — a
    post-write re-validation that rolls back a malformed write — was documented
    but never implemented. It now re-scans the written file and restores the
    previous content (returning an error) if the edit introduced a malformed
    value. (low)
  - **App event-state gates (cycle 897).** A file dropped behind an open modal
    no longer injects its path into the PTY; latched drag flags
    (`selecting`/`dragging_scrollbar`/`tab_drag_active`/`mouse_btn`) are cleared
    on focus loss (a swallowed button-up used to leave them stuck); and a
    side-button press/release with a lone context menu open dismisses the menu
    instead of leaking SGR. (low ×3)
  - **Confirm-close honors close returns (cycle 898).** The confirm dialog's
    CloseTab/ClosePane dispatch ignored the "that was the last tab/pane" return,
    deferring exit a tick and painting an empty frame; it now exits immediately,
    matching the keybind paths. (low)
  - **`fd_transport` short-write fix (cycle 899, Unix).** The no-fds send used
    `write` (ignoring short writes) and the SCM_RIGHTS path didn't flush a
    partial `sendmsg`, so a partial send silently lost the tail of a transferred
    tab (the caller closes the source tab on "success"). Both paths now deliver
    the whole payload via `write_all` (fds sent once, remainder flushed). (low)
  - **Lua instruction budget (cycle 900).** Lua scripts/callbacks had no CPU
    budget — a `while true do end` (incl. in the `output` callback) wedged the
    UI thread forever. An mlua instruction-count hook now aborts a runaway after
    a per-invocation budget; user Lua can't disable it. (low)
  - **Update-checker overall timeout (cycle 901).** The fetch set only per-phase
    timeouts, so a trickling server (resetting the per-byte read timeout) could
    keep the thread / synchronous `--check-update` alive indefinitely. Added an
    overall request deadline. (low)
  - **OSC 133 prompt-mark ring (cycle 902).** Switched the prompt ring from a
    `Vec` with O(n) `drain(0..d)` (on the hot reader path once full) to a
    `VecDeque` with O(1) `pop_front`. (low)
  - **`term.rs` duplicate `#[allow]` (cycle 903).** Removed two dead duplicate
    `#[allow(clippy::too_many_arguments)]` attributes. (low)
  - **Mouse drag-to-resize split dividers (cycle 904).** Dragging a split
    divider now resizes the panes (with a hover resize-cursor) — previously
    keyboard-only. Pure, unit-tested geometry (seam hit-test, position→ratio,
    path-addressed ratio set); drag ends on button-up and focus loss. (low)
  - **Docs sweep + privacy correction (cycle 905).** Documented the previously-
    undocumented runtime config keys (auto light/dark `theme-mode`/
    `theme-schedule`/`-lat`/`-long` + `light-theme`/`dark-theme`, `allow-bold`,
    `bold-is-bright`, `clear-select-on-copy`, `invert-search`,
    `backspace-binding`/`delete-binding`, `login-shell`, `term`, `colorterm`)
    and the vertical tab strips (`tab-bar-position = left|right|hidden` +
    `tab-bar-width`); back-filled the keybind action reference (with a pointer to
    `kettle --list-actions`) and noted digits/punctuation + key aliases. Fixed
    the ARCHITECTURE.md crate-graph (removed a nonexistent `core → cfg` edge) and
    the range-stable theme count (`~512` → `500+`) in ARCHITECTURE/PERFORMANCE/
    example-config, and the man page (`--shell-integration` lists `powershell`;
    icon sizes `16`–`256`). **Corrected the update-check privacy claim**: the
    prebuilt release binaries (and the Homebrew/AUR packages that *repackage*
    them) are not built with `KETTLE_PACKAGED`, so they DO auto-check — only a
    from-source build that sets that flag compiles the check out; the runtime
    `update-check = false` opt-out applies regardless. (medium ×3 + low)
  - **CI — Windows release build + smoke (cycle 906).** The Windows CI leg now
    builds the release binary and smokes a piped `kettle --version` under
    PowerShell 7, so a release-profile Windows regression (incl. the
    GUI-subsystem console-attach path) surfaces on every PR, not only at tag
    time in `release.yml`. (low)

  Deferred (tracked): the app.rs god-object refactor (audit `architecture-rust`)
  is intentionally staged — the punch-list calls for incremental extraction,
  each landing green with a drift test, which is a multi-cycle effort unsuited to
  a pre-release blind change.

## [2.9.0] — 2026-06-06

  Windows 11 polish + new capabilities (cycles 868–877): a flash-free
  GUI-subsystem launch, a taskbar notification that actually clears on focus,
  a Settings **Theme** picker with live preview (532 bundled themes — incl. all
  Terminator palettes), a cursor / synchronized-output rendering pass, and an
  opt-in, developer-only session recorder. Every change is drift-/unit-tested
  and `just gauntlet`-green.

  - **Windows (cycle 868) — no more phantom console window/flash on launch.**
    kettle now builds as a Windows GUI-subsystem app, so Explorer / Start-menu
    launches never auto-allocate a console (the brief console flash is gone).
    When launched from a terminal it attaches the parent console so CLI
    subcommands (`--version`, `--check-update`, `--print-completions`,
    `--shell-integration`, …) still print — and, unlike the abandoned cycle-734
    attempt, it reopens `CONOUT$` ONLY for std handles that aren't already
    inherited (detected via `GetFileType`), so piped/redirected output
    (`kettle --flag | grep`, `… >> $PROFILE`) is never clobbered. The drift test
    now guards the GUI-subsystem attribute + the conditional-attach guard.

  - **Windows (cycle 869) — the taskbar notification now clears when you focus
    kettle.** An attention request (a bell when `bell` includes `attention`, a
    `urgency` trigger match, or the available-update banner) flashes the taskbar
    button while kettle is unfocused — but winit's `request_user_attention(None)`
    alone does not reliably stop the Windows 11 flash on focus-gain, so it kept
    pulsing after you clicked back in. kettle now tracks the outstanding request
    and, on focus, clears it directly via `FlashWindowEx(FLASHW_STOP)` (alongside
    the cross-platform winit clear). Unit-tested (FLASHWINFO `cbSize` + flag).

  - **Rendering (cycle 870) — cursor/text vertical alignment under a non-default
    `cell-height`, plus a synchronized-output guard.** Two related fixes after
    investigating a transient "cursor one row above the prompt" report:
    - The pane text buffer now lays out lines at the grid's actual row height
      `cell_h` (which folds in the `cfg.cell_height` multiplier, cycle 636), not
      the unscaled font `line_height`. Previously the cursor and selection/vi
      quads stepped by `cell_h` per row while glyphon flowed text by
      `font_size × 1.25`, so with `cell-height != 1.0` the cursor drifted a
      fraction of a row per line — a full row off near the bottom of a tall
      window. Unit-tested.
    - The reported transient glitch itself was *not* a kettle geometry bug (at
      the default cell height the cursor and text share an origin and per-row
      step): it was a host TUI repainting non-atomically under heavy load.
      kettle already renders apps that use synchronized output (DEC private mode
      2026) as atomic frames — the engine buffers a sync block so the renderer
      only ever locks a consistent grid — now pinned by a regression test so a
      future change to the byte-extraction path can't silently break it.

  - **Themes (cycle 872) — pick a theme right in Settings.** Settings →
    Appearance gains a **Theme** row listing the most popular themes (Catppuccin
    Mocha/Macchiato/Frappé/Latte, Tokyo Night Night/Storm/Moon/Day, Dracula,
    Gruvbox Dark/Light/Material, Nord/Nord Light, Solarized Dark/Light, Rosé Pine
    Main/Moon/Dawn, Everforest Dark/Light, Kanagawa Wave/Lotus, One Half
    Dark/Light, Ayu Mirage/Light, Monokai Pro, Night Owl). ←/→ cycles them and
    **live-previews each instantly**, persisting the choice to the config file.
    The full bundled theme set stays reachable via the right-click Theme
    submenu, `NextTheme`/`PrevTheme`, or a `theme =` config line. The curated
    list is unit-tested to contain only real bundled theme names.

  - **Themes (cycle 873) — bundle the Terminator app's own built-in palettes.**
    kettle already ships the full iTerm2-Color-Schemes collection (the same set
    Terminator theme repos draw from), but Terminator's four *app-built-in*
    palettes — `linux`, `xterm`, `rxvt`, and Ubuntu's `ambience` (aubergine
    `#300a24`) — aren't in that collection. They're now hand-ported into the
    bundle as **Terminator Linux / XTerm / Rxvt / Ambience**, so "all Terminator
    themes" is literally complete. Unit-tested (bundled + palette parses).

  - **Themes (cycle 874) — refreshed the bundle with new upstream schemes.**
    Additively synced the 16 themes added to the upstream iTerm2-Color-Schemes
    collection since kettle last pulled it (Aardvark Ink, Electron Highlighter
    Day, Neon Purple, and the London / Sequoia / Serendipity families). Only the
    *new* names were copied in — every existing theme's palette is left exactly
    as-is, so no one's current theme changes — bringing the bundle to 532 themes.

  - **Dev tooling (cycle 875) — opt-in developer-only session recorder
    (`--features dev-record`).** A new compile-time Cargo feature adds a `kettle
    --record <path>` flag (also honored via `KETTLE_RECORD`) that writes an
    asciicast v2-compatible trace of the session — terminal output + resizes
    now, keystroke tokens + UI/UX markers next (cycle 876) — replayable with
    `asciinema play`. It is compiled OUT of every released / packaged binary
    (zero code, zero overhead for normal users), never on by default, and writes
    a local-only file (`0600` on Unix). Output capture reuses the existing
    per-pane output sidechannel; writes are best-effort (a full disk disables
    the recorder, it never crashes the terminal). Header/event formatting and
    the end-to-end file write are unit-tested (under the feature).

  - **Dev tooling (cycle 876) — the recorder now captures keystrokes + UI/UX
    state, privacy-first.** Building on cycle 875: `i` events record keystroke
    *tokens* (named keys / chords like `Enter` / `Ctrl+c`; bare printables
    redacted to a class glyph, so a typed password is recorded as length +
    timing — never the characters; `--record-raw-input` / `KETTLE_RECORD_RAW_INPUT`
    opts into literal capture). Pasted content is never recorded — only a
    `kettle:paste len=N` marker. New `m` markers capture kettle's own UI
    transitions the PTY stream can't show (`kettle:tab_add` / `tab_close` /
    `focus_in` / `focus_out`), spanning interactive and non-interactive states.
    A native-title `● REC` indicator appears while recording. CI now builds +
    lints + tests the `dev-record` feature so the gated code can't
    bit-rot. Documented in `docs/DEV-RECORD.md` (with a data-flow diagram); the
    redaction is unit-tested.

## [2.8.0] — 2026-06-06

  Substantive fixes from a third whole-codebase systematic sweep (cycles
  829–859) **plus a fresh 8-agent, file-by-file review of every crate + docs /
  CI / packaging (cycles 860–867)** — the latter found two HIGH bugs the
  findings-list sweep had missed: a Sixel RGB-palette integer overflow that
  could abort the process from an untrusted PTY sequence, and a confirm-dialog
  Enter key that fired the destructive close action even with the safe "Cancel"
  button focused. Every fix below is drift-/unit-tested and `just gauntlet`-green:

  - **Security/stability (cycle 860, audit) — Sixel RGB palette no longer aborts
    on a huge component.** A `#Pc;2;Pr;Pg;Pb` palette entry scaled each component
    `comp * 255 / 100`, but `read_num` saturates a long digit run to `i64::MAX`,
    so `i64::MAX * 255` overflowed — a process abort under `panic = "abort"` with
    overflow checks (debug/test) and a garbage color in release, reachable from
    any untrusted Sixel DCS in the PTY stream. Each component is now clamped to
    its spec-valid `0..=100` percentage before scaling. Regression-tested.

  - **VT (cycle 867, audit) — tmux `%output` rejects a non-ASCII literal byte.**
    The literal (non-octal) branch of the tmux control-mode output decoder used
    `c as u8`, silently truncating any char `> 0xFF` (e.g. a `U+FFFD` that an
    upstream `from_utf8_lossy` introduced from corrupt input) to a wrong byte. It
    now rejects the event via `u8::try_from`, matching the strict octal-branch
    handling. Unit-tested.

  - **CI (cycle 866, audit) — PowerShell shell-integration is now smoked.** The
    CLI smoke loops covered `bash`/`zsh`/`fish` but not `powershell` — the one
    shell-integration target Windows users actually use (`install.ps1
    -WithShellIntegration`). Both `--shell-integration powershell` and
    `--print-completions powershell` are now exercised end-to-end in CI (verified
    locally: 66-line OSC-133 snippet + a 10 KB completion script).

  - **Rendering/search (cycle 865, audit) — two small correctness fixes.** The
    cell-measurement probe sized its layout box at a fixed 1000×100px, but at a
    large font on a high-DPI display the 10-glyph probe (~1300px at 72pt×3)
    wrapped against it — so `cell_w` came out too narrow and mis-gridded the
    terminal; the box is now sized relative to the metrics so it never wraps.
    Search now skips zero-width regex matches (`a*`, `^`, `\b`) that otherwise
    painted a spurious one-cell highlight per position.

  - **Docs/packaging (cycle 864, audit) — accuracy fixes from the fresh review.**
    The man page listed `~/Library/Application Support` as the macOS config path,
    but the resolver is `XDG_CONFIG_HOME` → `$HOME/.config` → `%APPDATA%` with no
    `~/Library` branch — so macOS actually reads `~/.config/kettle/config` (a
    macOS user following `man kettle` was editing a file kettle never reads).
    Vi-mode (a shipped, default-bound feature) is now documented in CONFIG.md's
    action list. The INSTALL.md support table no longer overstates Linux aarch64
    as flat "Tier 1" — its CI build is `continue-on-error`, so it's labelled
    Tier 1.5 (best-effort prebuilt). The Homebrew formula and AUR PKGBUILD were
    bumped from a 2-major-stale `v1.42.0` to the current `v2.7.1` with refreshed
    SHA-256s. The README's "Eight CI workflows" headline (it then listed nine
    checks, several of them stages within `ci.yml`) was reworded to drop the
    inaccurate count.

  - **Hardening (cycle 863, audit) — six small robustness fixes from the fresh
    review.** (1) The SSH-host input now filters control characters like its
    sibling handlers (a cycle-857 comment had wrongly claimed it already did).
    (2) `Mux::restore` bounds the PTY fan-out at 256 panes, so a crafted-but-tiny
    `session.json` of minimal leaves can't fork hundreds of thousands of shells
    on launch. (3) `--tab-handoff-fd` rejects a descriptor `< 3` before it
    reaches `from_raw_fd` (a negative fd is UB; 0/1/2 would adopt+close stdio).
    (4) The panic hook writes via `writeln!` instead of `eprintln!`, so a broken
    stderr pipe can't double-fault the hook and lose the crash-log write.
    (5) `recv_fds` guards its `cmsg_len` subtraction against underflow on a
    malformed control message. (6) Removed a duplicated
    `#[allow(clippy::too_many_arguments)]`. Unit-/drift-tested.

  - **Diagnostics (cycle 862, audit) — three more `--check-config` gaps closed.**
    A malformed `theme-schedule` (bad `HH:MM` / mode word) and a typo'd
    `ask-before-closing` both silently fell back to a default without being
    flagged (the `theme-schedule-lat/long` sub-keys *were* flagged, making the
    omissions inconsistent) — both now validate. The bare `padding-x`/`-y`
    spellings, which the diagnostic already accepted, are now honored by the
    parser too (they previously passed the malformed scan yet did nothing *and*
    warned "unknown key" — a contradictory diagnostic). The `theme` diagnostic
    arm dropped its per-name `to_ascii_lowercase` alloc for `find_name`
    (`eq_ignore_ascii_case`), matching its sibling arms. Unit-tested.

  - **UX/data-loss (cycle 861, audit) — confirm-dialog Enter respects button
    focus.** The close-pane/tab/window confirmation dialogs open focused on (and
    visually highlight) the **Cancel** button as the safe default, but Enter
    fired **Confirm** regardless of focus — so the reflexive "highlighted button
    + Enter" destroyed the tab/pane/window instead of cancelling. Enter now
    activates the focused button (Cancel cancels, Confirm confirms), matching the
    highlight. The drift test that had enshrined the old behavior was corrected.

  - **Docs (cycle 859) — README hero / showcase screenshots refreshed + demo
    scene fixed.** The committed screenshots were stale (baked `v2.3.1`, old test
    count) and had two demo-scene bugs: the block cursor was hardcoded at column
    22 — stranded mid-path on the prompt's `~/Repos/kettle` instead of at the
    line end — and the tab chrome used fixed 240px segments ~2× the label width,
    so the inactive tab's text floated inside the active tab's highlight (and was
    grey-on-grey-invisible once its background was sized correctly). The cursor
    now anchors to the prompt's true end column, each tab's chrome is sized to
    its label (shared label constants keep glyphs + chrome in sync), the inactive
    tab gets a readable muted background, and the demo command is kept short
    enough never to wrap in the narrow showcase split (which would otherwise
    desync the cursor row). Both PNGs regenerated; version + test count now
    current.

  - **Correctness/cleanup (cycle 858, audit) — four small fixes.** (1) Config
    float values now reject `NaN`/`inf` before clamping (`clamp(NaN)` returns
    `NaN`, which would have poisoned opacity/cell-size/darkness etc.); a
    non-finite value keeps the finite default. (2) The tmux control-mode `%output`
    octal decoder rejects values `400`–`777` (256–511) instead of truncating
    them to a wrong byte. (3) Removed a dead, non-gamma-correct `Rgb::to_array_f32`
    helper (misuse risk). (4) Removed a dead duplicate match arm in the
    placeholder column-inheritance logic. Unit-tested.

  - **Input correctness (cycle 857, audit) — three small fixes.** (1) The search
    overlay now filters control characters before appending to the query (like
    the title / SSH-input handlers already did), so a stray control byte can't
    corrupt a search. (2) The fuzzy matcher folds the pattern one char per
    position, matching the candidate side — a character whose lowercase expands
    to multiple codepoints (e.g. `İ`) used to never match. (3) `parse_key` now
    rejects `F0` and `F13`+ (the winit→key bridge only maps `F1`–`F12`, so those
    bound to nothing); a typo'd F-key surfaces instead of silently dead. All
    unit-/drift-tested.

  - **Settings (cycle 856, audit) — "Window padding" sets both axes.** The
    single Settings "Window padding" control persisted only `window-padding-x`,
    leaving `window-padding-y` at its default — so nudging it produced visibly
    asymmetric padding (wider left/right than top/bottom). It now mirrors the
    value to both axes for uniform padding. Drift-guarded.

  - **Diagnostics (cycle 855, audit) — `--check-config` now flags out-of-range
    clamped numerics.** Cycle 837 closed the enum half of this gap; this closes
    the numeric half. `handle-size`, `tab-bar-width`, `background-darkness`,
    `cell-height`/`cell-width`, `inactive-color-offset`/`inactive-bg-color-offset`,
    and `theme-schedule-lat`/`-long` are clamped (or, for lat/long, silently
    discarded) by the parser, so an out-of-range value used to report OK while
    the runtime used something else — and the `theme-schedule-lat` doc even
    *promised* a diagnostic that didn't exist. `detect_malformed_values` now
    range-checks each against the same bounds the parser clamps to. Unit-tested
    (valid passes, out-of-range flags once).

  - **Performance (cycle 853, audit) — the per-frame quad list is pooled.**
    `render_frame_with_status` allocated a fresh `Vec<QuadInstance>` sized
    `panes*16 + 256` every frame for all the cell-background / cursor / UI quads.
    It now reuses a `quad_scratch` buffer on the renderer (taken + cleared at the
    top of the frame, returned after the GPU upload) so the steady-state 60fps
    path keeps the allocation across frames — same high-water pooling as the
    `span_scratch` / `pane_buffers` pools. Drift-guarded.

  - **Performance (cycle 852, audit) — the renderer borrows per-pane frame data
    instead of double-cloning it.** `redraw()` already builds a `guards`
    collection owning each visible pane's image `Vec`, title `String`, and
    group-name for the whole frame, then mapped it into `PaneView`s that *cloned*
    all three again — so every frame deep-copied every pane's placements + title.
    `PaneView` now borrows (`images: &'a [Placement]`, `title: &'a str`,
    `group_name: Option<&'a str>`) from `guards`, exactly as `term` already
    borrowed the `MutexGuard`. Zero per-pane clones on the render hot path;
    drift-guarded.

  - **Performance (cycle 851, audit) — remote-context polling refreshes the
    process snapshot once per tick, not once per pane.** `detect_remote_with`
    refreshes the OS-wide process list *and* rebuilds the parent→children index
    on every call, and the poll loop called it once per pane — so an N-pane
    window did N full process walks + N map builds every 200 ms. A new
    `RemoteScanner` splits the work: `refresh()` does the single OS walk + index
    build per tick, and `detect_root()` answers each pane from the shared index
    (a cheap BFS + cache-hit argv reads). One-shot `detect_remote`/
    `detect_remote_with` are preserved. The shared-index path is unit-tested to
    match the one-shot result.

  - **Performance (cycle 850, audit) — background-image blur is O(W·H) with one
    scratch buffer.** The startup Gaussian-approximation blur allocated a fresh
    full-image `Vec` in each of its six sub-passes (up to 6 × 256 MB transient at
    `MAX_BG_IMAGE_DIM`) and summed `2r+1` samples *per pixel* (O(W·H·R)). It now
    reuses a single scratch buffer (swapped each pass) and a telescoping
    sliding-window running sum (O(W·H) regardless of radius). The constant
    `2r+1` divisor is preserved, so output is byte-identical — guarded by a test
    that diffs the new blur against the old brute force across odd/even/degenerate
    dimensions and a radius larger than the image.

  - **Performance (cycle 849, audit) — animated-image frame selection stops
    allocating per paint.** `current_frame` (kitty animation playback) collected
    a `Vec` of the displayable frames on every call — and it's invoked from
    `Terminal::placements()` on every paint while an animation plays, so a
    running GIF churned a heap `Vec` per frame. It now does two cheap passes over
    the gap slice (accumulate total dwell + last displayable index, then a
    direct filtered modulo walk) with zero allocation. A trailing-gapless freeze
    case was added to the timing test.

  - **Portability/robustness (cycle 848, audit) — fd-passing compiles on every
    Unix and delivers fds close-on-exec.** The `SCM_RIGHTS` tab-handoff transport
    hard-coded the cmsg constant behind a `cfg` that only covered
    Linux/macOS/FreeBSD, so the `#![cfg(unix)]` module *failed to compile* on
    NetBSD/OpenBSD/DragonFly/illumos/Android; it now uses `libc::SCM_RIGHTS`
    (correct on every target). `recv_fds` additionally (1) receives fds
    close-on-exec — atomic `MSG_CMSG_CLOEXEC` on Linux/Android, an `fcntl`
    fallback elsewhere — so a handed-off PTY master can't leak into a
    later-spawned shell, and (2) checks `MSG_CTRUNC`: a truncated control buffer
    now closes the partial fd set and errors instead of adopting the wrong PTYs.
    Round-trip + CLOEXEC unit test added.

  - **Robustness (cycle 847, audit) — Lua callback registries are now bounded.**
    The side-effect command queue was already capped against a hostile
    `init.lua` (`MAX_PENDING_COMMANDS`), but `kettle.on`, `add_menu_item`, and
    `add_url_handler` appended with no length check — a runaway
    `for i=1,1e9 do kettle.on('output', f) end` grew the registry without bound
    *and* made every event fire walk a giant list. Each registry now caps at 256
    (far above any real plugin); past the cap, registration is a no-op with a
    single `log::warn`. Unit-tested.

  - **Zoom (cycle 846, audit) — `ScaledZoom` no longer discards a prior manual
    font zoom.** `Action::IncreaseFontSize`/`DecreaseFontSize` step the renderer
    size but never write `cfg.font_size`, so `ScaledZoom` — which saved
    `cfg.font_size` as its restore baseline and scaled `cfg.font_size * 1.5` —
    ignored any manual zoom: it scaled from the original config size and, on
    exit, *restored* that, throwing away the user's manual change. It now
    baselines off the live `r.font_size()` for both the 1.5× scale and the
    restore. Drift-guarded.

  - **Performance (cycle 845, audit) — the renderer stops heap-allocating the
    font family every frame.** `render_frame_with_status` cloned
    `self.font_family` (a `String`) once per frame — a heap alloc + memcpy at
    60fps — purely to hold an owned handle while `&mut self.font_system` is
    borrowed across ~20 `Family::Name(&family)` reads. The field is now
    `Arc<str>`, so the per-frame clone (and the one in `remeasure_cell`) is a
    refcount bump. `Arc<str>` derefs to `str`, so every `Family::Name` site is
    unchanged. Drift-guarded against a revert to `String`.

  - **Performance (cycle 844, audit) — image-escape extraction stops allocating
    on every non-image OSC/APC.** The OSC branch of the image extractor built a
    full owned `String` from every sequence — titles, colors, OSC 8 hyperlinks,
    OSC 52, OSC 104 — only to test a 10-byte `1337;File=` prefix and discard it.
    The prefix is now matched on the raw bytes; the `String` is built only on an
    actual iTerm image. The sibling kitty-APC path drops its `.into_owned()`,
    borrowing when the (almost always ASCII) payload is valid UTF-8.

  - **Performance (cycle 843, audit) — theme lookups stop allocating per bundled
    name.** `Theme::by_name`/`find_name`/`cycle` lower-cased every one of the
    ~513 bundled theme names in-loop (a heap `String` each) on every theme
    keypress, `ToggleLightDark`, and session restore. They now compare with
    `eq_ignore_ascii_case` against the trimmed query — zero per-element allocs,
    identical case-insensitive semantics — and `cycle` drops its intermediate
    name `Vec`. Drift-guarded.

  - **Mouse tracking (cycle 842, audit) — coordinates clamp to the grid and
    drags coalesce to cell crossings.** Two fixes to the SGR mouse reports sent
    to TUIs (htop, vim, tmux). (1) A click in the right/bottom padding rounded
    the reported cell up to `cols`/`rows` — one past the last valid cell — which
    a tracking app mis-renders; the reported `(row, col)` now clamps to the
    pane's grid. (2) A fast drag inside one cell emitted one SGR motion report
    per pixel of travel; cell-motion modes (1002/1003) now report only when the
    pointer crosses into a new cell, matching xterm. Press/release always
    report. The coalescing rule is a pure, unit-tested helper.

  - **Cross-platform (cycle 841, audit) — minimizing no longer reflows every
    PTY to 1×1.** On Windows, minimizing delivers `Resized(0, 0)`; kettle
    reconfigured the surface and ran `resize_all` to a 0×0 area, collapsing every
    pane's PTY to a 1×1 grid (a SIGWINCH storm that reflowed every TUI) — then
    reflowed them all back on restore. A degenerate `Resized` is now ignored, so
    panes keep their real size; the genuine restore reflows once. Drift-guarded.

  - **Cross-platform (cycle 840, audit) — no POSIX `-l` for explicit Windows
    shells either.** Cycle 822 stopped `login-shell = true` from injecting `-l`
    into the *default* Windows shell, but the explicit-`command` arm still added
    it for any non-`wsl.exe` prog — so `command = pwsh` + `login-shell = true`
    fed `pwsh -l` (rejected). A shared `prog_accepts_login_flag` now excludes
    `pwsh`/`powershell`/`cmd` (and `wsl.exe`) in both arms. Unit-tested.

  - **Docs (cycle 839, audit) — man page + doc drift corrected.** The man page
    (`kettle.1`) was missing **nine** `--<flag>` entries (incl. `--check-update`
    and `--write-default-config`, the recommended Windows bootstrap),
    mis-described `--annotate` as a repeatable `ROW:COL:TEXT` (it's a single
    bottom caption), and leaked internal `cycle N` refs — all because the only
    man-page guard checked keybinds, not flags. A new drift test now walks the
    clap CLI and asserts every flag is documented with no cycle refs. Also
    refreshed user-facing doc drift: the "330+ tests" headline (actually 500+)
    and two 2–4× off per-crate figures, the Settings keybind-rebinder category
    hidden from GETTING-STARTED, the false "this release" markers in
    PERFORMANCE.md, and the Windows config-path in CONFIG.md (the resolver prefers
    `~/.config` when `HOME` is set, not `%APPDATA%`).

  - **Build/CI (cycle 838, audit) — smaller release binaries + per-leg release
    cache.** The release profile now sets `strip = "symbols"`, so the shipped
    ELF/Mach-O binaries drop their (unused, `panic=abort`) symbol tables and
    shrink several MB (MSVC keeps debuginfo in a separate unshipped `.pdb`, so
    it's a no-op there). The two `ubuntu-latest` release legs (x86_64 + aarch64)
    now get a per-artifact `shared-key` so they stop thrashing one shared cache.
    Also corrected the stale `crates/kettle/Cargo.toml` comment that described
    the abandoned `AttachConsole`/`windows_subsystem` console approach instead of
    the cycle-740 `GetConsoleProcessList`/`ShowWindow` one the code uses.

  - **Error-handling (cycle 837, audit) — `--check-config` flags enum + color
    typos too.** The cycle-826 diagnostic sweep covered bool keys but left enum
    keys (`status-bar`, `exit-action`, `backspace-/delete-binding`,
    `broadcast-default`, `theme-mode`, `background-type`, `lua-sandbox`), the
    color keys (`accent-color`, the six `title-*-color`), and the theme-role keys
    (`light-/dark-theme`) silently defaulting on a typo. They're now validated
    against their variant sets / `Rgb::parse` / `Theme::find_name`, so
    `exit-action = clse` or `accent-color = nope` is caught. Drift-tested.

  - **Fix (cycle 836, audit) — remote-session detection drives the right
    reconnect command.** Three arg-parsing bugs each produced a wrong title and a
    wrong command written to the PTY: `ssh -J jump host` took the bastion as the
    target (the value-taking flag set omitted `-J` and others — now the full
    OpenSSH set); `docker exec --privileged c sh` skipped the container (bare
    `--flag` was assumed to take a value — now valueless by default with a small
    allowlist); and global flags before `exec` (`kubectl -n ns exec pod`) made
    detection silently fail (now it scans for `exec` past leading options).

  - **Fix (cycle 835, audit) — a stray key in the keybind-capture overlay can't
    soft-brick typing.** Settings → keybind capture bound whatever key was
    pressed with no guard, so a modifier-less mis-press (a bare `a`) inserted
    `Trigger { mods: empty, key: 'a' }` into the config — and the global key path
    matched it before text encoding, so every future `a` fired the action across
    all panes, persisted, with no in-overlay unbind. Capture now refuses a
    modifier-less chord for text/essential keys (only F-keys may be unmodified)
    and shows a "hold a modifier" hint instead of binding.

  - **Fix (cycle 834, audit) — the new-tab `▾` shell dropdown can't freeze the
    window.** Opening it ran `detect_shells()` synchronously on the UI thread,
    including a `wsl.exe -l -q` subprocess with no timeout — so a cold or wedged
    `LxssManager` (the same `Wsl/Service/E_UNEXPECTED` state that hangs `wsl.exe`)
    froze the whole window ("not responding"), and the detection re-ran on every
    open. The `wsl.exe` call is now bounded by a worker-thread `recv_timeout`
    (≤2 s, then no distros), and the result is cached per session.

  - **Fix (cycle 833, audit) — closed panes don't leak zombie processes.** On
    pane close / quit, `Terminal::Drop` SIGKILL'd the PTY child but never
    `wait()`'d it (`std::process::Child::drop` doesn't reap, and the live-pane
    `try_wait` path doesn't run for a dropped pane), so a long open/close session
    accumulated `<defunct>` processes consuming PID slots on Unix/macOS. Drop now
    reaps the killed child in a short detached thread — staying non-blocking (no
    UI freeze) while leaving no zombie. Windows is unaffected (handle-based).

  - **Fix (cycle 832, audit) — the `=` key can be rebound.** `keybind`
    parsing split the trigger/action on the *first* `=`, but the `=` key is a
    shipped default trigger, so `keybind = ctrl+==increase_font_size` parsed as
    trigger `ctrl+` (rejected) and was silently dropped — and `--check-config`
    flagged the reasonable line as malformed. Action names never contain `=`, so
    it now splits on the *last* `=` (binder + validator agree). Regression-tested.

  - **Fix (cycle 831, audit) — side mouse buttons no longer leak behind a
    modal.** The cycle-810 Back/Forward forwarding ran *above* the modal-input
    gate, so with any dialog open (search/palette/settings/ssh/…) over a
    mouse-tracking TUI, a side-button press/release injected SGR bytes into the
    app behind the dialog — the exact leak cycle 786 closed for the other
    buttons. The forward is now gated by `modal_swallows_pointer` (a lone context
    menu still passes). Source drift guard added.

  - **Fix (cycle 830, audit) — Sixel numeric parsing can't overflow-abort.**
    `read_num` accumulated `v * 10 + d` over an attacker-controlled digit run (up
    to a 64 MiB DCS body) with no overflow guard — a ~20-digit count/dimension
    aborted the process under debug/test (`panic=abort`) and silently wrapped in
    release. It's now saturating (total over any input); a 25-digit run decodes
    to a clean `None` via the existing dimension caps.

  - **Fix (cycle 829, audit) — custom tab titles actually show now.** A title set
    via `EditTabTitle` was stored in `title_override` (and re-read into the edit
    dialog, so it "stuck" there) but `tab_titles()` — the source of truth for
    both the horizontal and vertical bars — never consulted it, so it had zero
    visible effect and the shell's next OSC 2 title silently won. The override
    now takes precedence (via a pure, drift-tested `resolve_tab_title`).

## [2.7.1] — 2026-06-05

  Post-v2.7.0 hardening from a fresh multi-agent production audit (each finding
  3-lens adversarially verified; cycles 813–828):

  - **Security/panic-safety (cycle 813) — kitty graphics can't abort kettle via
    an oversized texture.** The kitty `f=32`/`f=24` raw-pixel branches built an
    image straight from the untrusted `s=`/`v=` dimensions; `f=32,s=10000,v=1`
    (40 KB) produced a 10000×1 image that hit wgpu `create_texture` above the
    8192 limit — a validation error that panics (= whole-process abort under
    panic=abort), a remote DoS from any PTY writer. Now `ImageData::new` caps
    per-axis dims (the one chokepoint), `solid` guards its pre-alloc, and
    `ensure_texture` skips any image past the device limit (last-line defense for
    constructions that bypass `new`).
  - **Security (cycle 814) — kitty zlib (`o=z`) decompression is bounded.** An
    unbounded `read_to_end` let a few hundred KB of `o=z` payload inflate to
    multiple GiB (zlib ~1000:1 on zero runs) → OOM/abort. The inflate is now
    capped to the 256 MiB envelope a legal 8192² image occupies; an over-cap
    stream is rejected.
  - **Security (cycle 815) — remote-authority `file://` URLs are rejected.**
    `is_safe_url` blocked traversal but not a remote authority, so
    `file://evil.example.com/share` (a Windows UNC `\\host\path` → SMB/NTLM-hash
    leak / SSRF) opened from untrusted PTY output. `file://` is now local-only
    (empty or loopback authority; no backslash / `file:////` / encoded
    traversal).
  - **Security (cycle 816) — `OpenCwdInFileManager` refuses a non-local OSC 7
    cwd** before building a `file://` URL from it (defense-in-depth on cycle
    815; a pure string check, since `Path::is_dir` on a `//host` path would
    itself route over SMB).
  - **Correctness (cycle 828) — application-keypad mode (DECKPAM) is honored.**
    `TermMode::APP_KEYPAD` was set/cleared by `DECKPAM`/`DECKPNM` but the key
    encoder only consulted `APP_CURSOR`, so the numpad always sent plain ASCII
    even when an app requested keypad mode — a silent xterm divergence affecting
    curses apps, gnuplot, BBS/serial clients, and TUI calculators. Unmodified
    numpad keys now emit the SS3 keypad sequences (`ESC O p`..`y` for 0–9,
    `ESC O M` for keypad-Enter, etc.) when the mode is set, using the key event's
    numpad location.

  - **Perf (cycle 827) — pane text spans reuse their buffers across frames.**
    `build_pane` allocated the style-run `Vec` and a fresh `String` per run from
    empty on every frame, so a busy colored pane (`ls --color`, a TUI) churned
    dozens–hundreds of `String` allocations on the 60 fps hot path even when
    nothing changed. The run scratch (Vec + per-run `String` buffers) is now
    pooled on the renderer and reused by index, like the per-pane text buffers.

  - **Error-handling (cycle 826) — `--check-config` catches bool/enum typos.**
    The malformed-value diagnostic validated only 8 of ~100 boolean keys (and
    skipped several enum keys), so `borderless = treu`, `login-shell = yse`,
    `focus = sloopy`, `window-state = maximze` passed `--check-config` cleanly
    then silently kept the default at runtime. It now validates the whole
    `BOOL_KEYS` set plus `focus` / `window-state` / `case-sensitive`, round-trip
    drift-tested so the list can't fall behind `parse_collect`. This also
    surfaced that `docs/kettle.example.config`'s Terminator-parity section used
    inline `# comments` after values (which kettle parses as part of the value),
    so those were converted to copy-pasteable full-line-comment form.

  - **Perf (cycle 825) — a tiny `tile` background no longer hangs the renderer.**
    The `background-image-mode = tile` path emitted one CPU quad (+ `Arc` clone)
    per tile every frame straight from the source image's pixel size with no
    floor — a 1×1 source on a 4K surface is ~8.3M quads/frame, freezing the
    window. Tile counts above a cap (~4096; ~60-px tiles on 4K still tile) now
    fall back to a single stretched quad.

  - **Perf/DoS (cycle 824) — Sixel decode is O(W·H), not O(W²·H).** A spec-legal
    sixel that omits the raster-attribute size hint grows its width one pixel at
    a time, and the decoder reallocated to the exact new size and full-copied
    every existing row on each growth — O(W) reallocations of O(W·H) each, i.e.
    seconds-to-minutes of single-threaded work blocking the render/PTY loop on
    one small escape. The decoder now decouples allocated capacity from the
    logical extent and grows capacity geometrically (capped at `MAX_DIM`, so peak
    memory is unchanged), compacting once at the end — amortized O(W·H).

  - **Cross-platform (cycle 823) — remote-session detection works on Windows.**
    The SSH/container detectors derived `argv[0]`'s basename with `split('/')`
    and an exact-name compare, so on Windows (`C:\…\OpenSSH\ssh.exe`,
    `…\docker.exe`) nothing matched and the entire Terminator-parity remote
    feature (pane-title `ssh box`, right-click Reconnect/Re-attach) was silently
    dead. A shared `argv0_basename` now splits on `/` **and** `\`, strips a
    case-insensitive `.exe`, and compares lowercased.

  - **Cross-platform (cycle 822) — no POSIX `-l` on the Windows default shell.**
    With `login-shell = true` and no explicit `command`, kettle appended `-l` to
    the default shell on every platform — but on Windows that's pwsh/powershell/
    cmd, which reject it (a broken/empty pane). The default-shell arm now only
    injects `-l` off Windows (the explicit-argv arm already guarded `wsl.exe`).

  - **UX (cycle 821) — tab drag-to-reorder reaches the last slot again.** The
    cycle-805 `▾` dropdown widened the trailing button area to a `▾ +` pair, but
    the drag handler still subtracted only the `+` width, so its strip was one
    button too wide and the reorder target lagged the cursor near the right edge.
    The drag and `tab_bar()`'s segment layout now share one
    `tab_segment_strip_width` helper.

  - **Correctness (cycle 820) — vi-mode visual selection highlights the full
    pane width.** Intermediate rows of a multi-row visual selection were
    highlighted only to a hardcoded column 256, so on a pane wider than 256
    columns (4K/ultrawide with a small font) the right portion stayed
    un-highlighted while the selection still yanked the full rows. It now extends
    to the pane's real last column.

  - **Correctness (cycle 819) — `rgb:` config colors scale by digit width.**
    The X11/xterm `rgb:<r>/<g>/<b>` parser sliced the first two hex digits of
    each component instead of scaling by digit count, so `rgb:f/8/0` (full red in
    X11) parsed as near-black `(15, 8, 0)` and 3-digit forms dropped a nibble. It
    now scales 1–4 digit components correctly (`f`→`0xff`, `fff`→`0xff`,
    `ffff`→`0xff`), keeping the existing multibyte-safety.

  - **Correctness (cycle 818) — Ctrl+Space sends NUL (0x00), not a space.** The
    space bar arrives as `NamedKey::Space`, which returned a literal space before
    any modifier was checked, so Ctrl+Space silently inserted 0x20 — breaking
    emacs/readline set-mark and tmux/vim `C-SPC` bindings. It now emits NUL for
    Ctrl+Space and ESC+space for Alt+Space (xterm parity).

  - **Correctness (cycle 817) — split-pane mouse + PTY sizing account for the
    per-pane titlebar.** In the default config, splitting a pane left every
    pointer ~1 row too high (selection, link targeting, and the mouse-tracking
    row reported to vim/tmux/htop) and sized the PTY one row too tall (bottom row
    clipped under the chrome). The hit-test and per-pane PTY sizing now apply the
    same titlebar inset the renderer draws with.

## [2.7.0] — 2026-06-05

  - **Fix (cycle 812, audit) — GPU init can't hang kettle on an invisible
    window forever.** Startup block_on's wgpu's adapter+device requests on the
    event-loop thread; a wedged graphics driver or GPU reset can make those
    never return, and since the window stays hidden until the first paint
    (cycle 785) the user just sees nothing — indistinguishable from a crash. A
    watchdog thread (which only touches an `AtomicBool`, so there's no
    Send/thread-affinity hazard with the GPU objects) now bounds init to 30s
    and, on a true hang, logs an actionable diagnostic (update the driver; try
    `LIBGL_ALWAYS_SOFTWARE=1` / `WGPU_BACKEND=gl`) and exits cleanly instead of
    hanging. The 30s budget is far above real init (~1.5s), and the watchdog
    stands down the instant init returns — success or a quick failure — so a
    working GPU is never affected. Timeout logic is unit-tested.

  - **Docs (cycle 811, audit) — document the update checker + fix accuracy
    nits.** The on-by-default update checker (a feature that contacts GitHub
    once a day) had **no user-facing docs**; the README and
    `docs/kettle.example.config` now describe `--check-update`, the
    `update-check` opt-out, and the `KETTLE_PACKAGED` build-time suppression, so
    the privacy control is discoverable (guarded by a new drift test). Also:
    corrected the stale "~500 themes" in the README to ~512 (matching the rest
    of the doc and `--list-themes`); noted Back/Forward mouse support; added the
    `-ExecutionPolicy Bypass` fallback to the Windows install steps in
    `docs/INSTALL.md`; and refreshed the macOS Gatekeeper guidance for macOS 15
    (System Settings → Open Anyway / `xattr -dr com.apple.quarantine`, since
    right-click → Open no longer bypasses Gatekeeper there).

  - **Fix (cycle 810, audit) — forward the side mouse buttons (Back/Forward) to
    mouse-tracking apps.** The press/release handlers dropped every button past
    right-click (`_ => return`), so a 5-button mouse's Back / Forward never
    reached a TUI that maps them (tmux/vim bindings, pagers). They're now
    encoded as SGR buttons 128 / 129 (xterm's 8–11 range; winit `Back` =
    XBUTTON1, `Forward` = XBUTTON2) and forwarded when mouse tracking is on —
    no-op otherwise, since they have no local UI meaning. The mapping and SGR
    encoding are unit-tested.

  - **UX (cycle 809, audit) — keyboard access to the update banner + honest
    docs.** The cycle-794 "update available" banner was mouse-only, yet an
    internal doc claimed it opened "on Enter" — and grabbing a bare Enter/Esc
    would wrongly steal those keys from the terminal (the banner is non-modal).
    Added two bindable actions, **`open_update`** (open the release page +
    dismiss) and **`dismiss_update`** (dismiss only), each a no-op (debug-
    logged) when no banner is showing, so a bound chord is harmless the rest of
    the time. Both are unbound by default and documented in
    `docs/kettle.example.config`; the mouse handler and the actions now share
    one `act_on_update_banner` path. The stale "on Enter" doc is corrected.

  - **Fix (cycle 808, audit) — update banner no longer covers or steals clicks
    from a bottom tab/status bar.** The passive "update available" banner always
    painted flush at the window bottom, and its click handler treated the whole
    bottom band as the banner. With `tab-bar-pos = bottom` or `status-bar =
    bottom` that meant the banner sat *on top of* the bar and swallowed its
    clicks — you couldn't switch tabs (or click the status bar) while an update
    was pending. The banner now **stacks above** any bottom-anchored chrome, and
    the renderer's draw + the App's hit-test share one pure `update_banner_top`
    helper so they line up to the pixel. Top/right/left/off layouts (the
    defaults) are unaffected. Drift-guarded by a unit test.

  - **Fix (cycle 807, audit) — image cache no longer mis-binds a recycled
    buffer address.** The GPU texture cache for Sixel/kitty/iTerm2 images keyed
    entries by the rgba `Arc`'s raw pointer but didn't keep that `Arc` alive, so
    an ABA hazard was possible: image A caches at heap address `P`, A is dropped
    (freeing `P`), and a *different* image B's pixel buffer reallocates at `P`
    before the per-frame `gc` evicts A — making the next lookup hit A's stale
    entry and draw A's pixels for B. The cache now holds an `Arc` clone
    alongside each texture, pinning the keyed address for as long as the entry
    lives (a refcount bump only — the buffer is already shared with the VT
    layer; `gc` releases it the first frame the image isn't drawn). Guarded by a
    source-level invariant test plus a pure `Arc`-semantics test.

  - **Perf (cycle 806, audit) — no per-frame `String` churn in the text hot
    path.** Building a pane's rich-text runs cloned every style run's text into
    an owned `String` (and allocated a fresh `"\n"` per wrapped row) on **every
    frame** — a busy 120×50 pane of colored output minted dozens–hundreds of
    throwaway `String`s per frame, then immediately re-borrowed them as `&str`.
    The runs now **borrow** the span text (`Vec<(&str, Attrs)>`) and the vec is
    handed to `set_rich_text` by value, which also drops the per-run `Attrs`
    clone the old `.iter().map(…)` adapter made. The `&str` element type makes a
    future re-introduction of `text.clone()` a compile error, and a pointer-
    identity drift test pins the zero-copy property. No visual change.

  - **Feature (cycle 805) — new-tab `▾` dropdown to pick a shell (Windows 11
    Terminal-style).** A small `▾` arrow now sits to the **left** of the tab-bar
    `+`. Clicking `+` opens the default tab as before; clicking `▾` opens a menu
    of **auto-detected shells** and opens the chosen one in a new tab that
    **inherits the focused tab's working directory**. Detection (cheap, only on
    the `▾` click): Windows lists Command Prompt, Windows PowerShell, and
    PowerShell 7 (each only if found on `PATH`) plus one entry per installed WSL
    distro (`wsl -l -q`, UTF-16-decoded); other platforms list `$SHELL` then
    bash/zsh/fish found on `PATH` (de-duped). The menu reuses the existing
    context-menu chrome/dispatch (mouse + keyboard + typeahead). Horizontal tab
    bars only (vertical bars keep a plain `+`). Detection, the WSL parser, and
    the arrow/`+` hit-rect split are unit-tested.

  - **UX (cycle 804) — tab titles show in full when there's room.** The
    per-tab title was capped at **24 characters regardless of how wide the tab
    was**, so a single wide tab still showed `1: C:\Program Files\Window…`
    with most of the bar empty. The budget now tracks the actual segment width
    (reserving room for the `"{n}: "` prefix so the whole label fits), showing
    the full title and ellipsizing only on genuine overflow. With many tabs
    each still ellipsizes inside its own narrower segment. Guarded by
    `truncate_honors_budgets_beyond_24_columns`.

  - **Per-frame work elimination (cycle 803) — search + link re-scans now
    cached.** While the search overlay is open, `update_search` re-ran a full
    scrollback regex scan (`kettle_core::search_with`) on **every frame**
    (~60×/s) even when nothing changed; likewise `update_links` re-ran the
    viewport URL autodetect (`kettle_core::links`) every frame. Both now cache
    by a cheap key — search by `(query, focused pane, that tab's last-output
    instant)`, links by `(focus, last-output, scroll offset)` — and re-scan
    only when an input that affects the result actually changes. Match
    navigation (n/N) and the follow-to-match scroll still run every call, so
    only the expensive scan is skipped on an idle frame. O(history)/O(viewport)
    per-frame → O(1) when idle.

  - **UI feedback fixes (cycle 802).** Two confirmed audit findings where an
    action gave no visible result:
    - **PTY-spawn failures are no longer swallowed.** `Action::NewTab`,
      `SplitRight`/`SplitDown`, the new-window in-process fallback, and the
      tab-bar `+` button each discarded the spawn `Result` with `let _ =`,
      while the `-e` launch path logged it — so a failed shell spawn (bad
      `command`, exhausted PTYs/FDs) made the keybind/click read as "does
      nothing." They now log the error; `NewTab` additionally only fires the
      `TabAdd` plugin event when a tab was actually created (it previously
      announced a tab that failed to open).
    - **Focus-follows-mouse repaints immediately.** With `focus = sloppy`, a
      pane gaining focus under the cursor didn't request a redraw, so the
      focused-pane border and cursor solid/hollow state stayed stale until an
      unrelated event triggered a repaint. `note_focus_change` now requests a
      redraw on every real focus change (centralizing the fix for all
      focus-change paths).

  - **Config bootstrap robustness (cycle 801) — Windows/PowerShell config no
    longer silently ignored.** Two fixes for the documented
    `kettle --print-default-config > config` setup flow:
    - **UTF-16 config files now load.** PowerShell **5.1**'s `>` redirect
      writes UTF-16-LE-with-BOM. kettle read the config with `read_to_string`,
      which hard-fails on non-UTF-8 — so the file was silently dropped and the
      user's settings just "didn't apply" with no visible reason. kettle now
      reads bytes and decodes by BOM (UTF-16 LE/BE → decoded; UTF-8 with/without
      BOM → lossy, so one stray byte can't drop the whole file). Unit-tested.
    - **New `--write-default-config`.** The `>` one-liner also fails on a fresh
      install because the shell can't redirect into a not-yet-existing config
      directory — a confusing dead end for a non-technical user. The new flag
      resolves the config path (honoring `--config`/`--profile`), creates the
      parent directory, and writes the embedded default, **refusing to
      overwrite** an existing config.

  - **WSL fix (cycle 799) — `COLORTERM` now propagates into WSL.** kettle sets
    `COLORTERM=truecolor` / `TERM_PROGRAM=kettle` on the child process, but
    WSL only forwards Windows env vars listed in `WSLENV` — so inside an
    Ubuntu/WSL shell `$COLORTERM` was **empty**, and a program that decides
    truecolor support from it (rather than force-enabling `termguicolors`)
    fell back to 256-color and rendered mis-mapped colors. kettle now appends
    `COLORTERM`/`TERM_PROGRAM`/`TERM_PROGRAM_VERSION` to `WSLENV` with the
    `/u` (Windows→WSL) flag, preserving any `WSLENV` the user already set and
    not duplicating entries (pure `augment_wslenv`, unit-tested). Harmless for
    non-WSL children.

  - **Test coverage (cycle 800) — update-check persistence.** Added a serde
    round-trip + partial-file test for the cycle-794 `UpdateCache` (on-disk
    throttle + anti-nag state): it confirms the struct round-trips and that an
    older/partial `update-check.json` (missing fields) still loads via serde
    defaults rather than failing — so a future schema field can't brick the
    stored throttle/dismissal state.

  - **Audit-hardening batch (cycle 798).** Several confirmed findings from
    the multi-agent production audit:
    - **kitty keyboard protocol no longer falsely advertised (critical).**
      `kettle-core` set `kitty_keyboard: true`, so the engine replied to the
      `CSI ? u` progressive-enhancement query and honored `CSI > flags u` —
      telling programs kettle encodes keys in the kitty CSI-u format. But the
      key encoder (`kettle-ui/src/input.rs`) only implements the legacy xterm
      encoding and never emits CSI-u, so an app that enabled the protocol
      (e.g. Neovim's kitty keyboard mode) would mis-read the legacy bytes it
      actually got — broken/ambiguous key input. Until a real CSI-u encoder
      lands, kettle no longer advertises the protocol; programs fall back to
      the legacy encoding, which is correct and unambiguous.
    - **GPU surface `Lost` now recovers instead of freezing.** The
      `get_current_texture` match reconfigured the surface only on `Outdated`;
      `Lost` (GPU device reset, laptop sleep/wake, monitor hot-swap, driver
      TDR) fell into a bare `return Ok(())`, so the surface was never
      recovered and every later frame returned `Lost` again — the window
      froze permanently. The catch-all arm now reconfigures, the standard
      wgpu recovery, so the next redraw paints on a fresh surface.
    - **Doc drift fixes:** `docs/CONFIG.md` listed `icon-bell`'s default as
      `false` (code + test pin it `true`); `CONTRIBUTING.md` claimed kettle
      had "yet to make" a major release (v2.0.0 shipped); `docs/SETTINGS.md`
      Behavior table omitted the `update-check` ("Check for updates") toggle
      that the cycle-794 settings overlay exposes.

  - **Render fix (cycle 797) — sRGB gamma double-encode on solid-color
    quads (washed-out backgrounds).** Every solid rectangle the renderer
    draws — cell backgrounds, the cursor, selection/search highlights, the
    unfocused-pane dim, and all tab-bar/menu/banner chrome — went through a
    quad shader that wrote its color straight to the **sRGB** surface
    (`Bgra8UnormSrgb` live / `Rgba8UnormSrgb` offscreen). The hardware then
    sRGB-*encodes* on store, so a color that was already sRGB got encoded a
    second time and lifted: a dark editor background `#1a1b23` surfaced as a
    washed-out grey `#5a5f68` (verified exactly: sRGB-encode(26/255 as
    linear) = 90 = 0x5a). The render-pass *clear* color was already
    linearized via `srgb()`, so it was correct — only the quad path wasn't.
    This was **invisible in shells** (cells mostly use the default bg, which
    is the clear, not a quad) but hit **every full-screen TUI that sets an
    explicit background on every cell** — AstroNvim/Neovim, which is why
    "the whole screen looks lighter" only showed up there. It also collapsed
    the active-vs-inactive **tab-bar** contrast (active tab is the dark
    `default_bg`, inactive tabs are the lighter `palette[8]`), making tab
    switches hard to perceive. The fix decodes sRGB→linear in the quad
    fragment shader (same math as the CPU-side clear), so a quad lands on
    its intended color after the surface's encode. Guarded by a new GPU
    pixel-readback test (`quad_pipeline_does_not_gamma_lift_on_srgb_target`)
    that draws `#1a1b23` and asserts it reads back ≈ `#1a1b23`, not the
    lifted grey.

  - **Panic fix (cycle 796) — two parsers panicked on a multibyte UTF-8
    byte after a `%`/component prefix.** `parse_osc7` (OSC 7 cwd report,
    `kettle-vt`) percent-decoded by slicing the `&str` (`&path[i+1..i+3]`),
    and `Rgb::parse`'s `rgb:rr/gg/bb` form (`kettle-config`) sliced
    `&h[..2]` — both on indices that can land inside a multibyte char,
    panicking on a non-char-boundary. Under `panic = "abort"` that is a
    hard crash, reachable from the live PTY (a program emitting
    `ESC ] 7 ; …/%€ …`) or from theme/OSC color parsing (`rgb:€/00/00`).
    Both now slice the *bytes* and validate via `from_utf8`, so a mid-char
    pair safely yields a literal/`None` instead of crashing. New drift
    tests in both crates, plus a first `color.rs` test module covering the
    full `#rgb` / `rrggbb` / `0x` / `rgb:` / X11-name parser surface.

  - **Tooling (cycle 795) — `just install-local` for one-command local
    re-sync.** `just install` installs whatever is already in
    `target/release/` (which can be stale or absent); the new
    `install-local` recipe depends on `release` then `install`, so it
    rebuilds the binary *then* reinstalls in one step — closing the
    "built but forgot to reinstall" gap that let the Start-menu /
    Windows-Search "kettle" launcher drift behind the repo (it was found
    pinned at v2.0.0 while the repo was v2.6.0, because the install is a
    *copy* in `%LOCALAPPDATA%\Programs\kettle`, not a live link). The
    installer already refreshes the binary, icon, docs, shell-integration,
    and the Add/Remove-Programs `DisplayVersion`, so `just install-local`
    fully syncs the installed app to the current build.

## [2.6.0] — 2026-06-04

  - **Feature (cycle 794) — in-app update checker (notify-only).** kettle now
    checks GitHub at most **once/24h** for a newer release and shows a
    dismissable bottom-bar banner (`⬆ kettle vX.Y.Z available — <url>`) plus one
    desktop toast. Click the banner to open the release page (right-click to
    dismiss); a dismissed version never re-nags, only a newer one does. It runs
    on a background thread (`ureq` + pure-Rust `rustls`, no tokio) so it never
    blocks startup, and **fails silent** on offline / timeout / rate-limit /
    parse errors. Privacy guardrails: **opt-out** via `update-check = false`
    (also a toggle in the Settings overlay), it **never checks on the first
    launch** (only stamps the throttle), the 24h cache (shared across windows)
    keeps well under GitHub's 60-req/hr/IP limit, and **packaged builds**
    (distro / Homebrew / winget / source — they have their own update channel)
    compile the auto-check out via `KETTLE_PACKAGED`. **Notify-only — kettle
    never downloads or installs** (that would own a signed-artifact / elevation
    security boundary; the OS package manager / release page is the installer,
    matching WezTerm/kitty). `kettle --check-update` does a deliberate one-shot
    check (bypassing the throttle). Version-compare + throttle are pure,
    drift-guarded functions. (`webpki-roots`' Mozilla-CA-bundle data license
    `CDLA-Permissive-2.0` added to the cargo-deny allow-list, in keeping with
    the existing "data file licenses" allowance.)

## [2.5.0] — 2026-06-03

  Multi-agent production audit batch (cycles 785–793): modal input-routing
  fixes, an unbounded-channel invariant, overlay memory-leak fixes, new
  drift-guard tests (incl. a `--layout` path-traversal security guard), doc +
  release-tooling drift fixes, a render hot-path allocation removal, and
  install ease-of-use. IME/CJK input (audit finding A3) is deferred to a
  verified follow-up. All cycles gauntlet-green and live-verified on Win11 +
  WSLg (startup paint, PowerShell, WSL Ubuntu, context-menu overlay, AstroNvim).

  - **Install/docs (cycle 793) — clearer paths for unsupported arches +
    Windows package managers.** The `install-online.sh` unsupported-arch error
    was a dead end ("build from source" with no hint whether that's even
    viable); it now names the tier-1 arches (x86_64 / aarch64), flags 32-bit
    (armv7l / i686) as source-only + experimental (wgpu/glyphon have no tier-1
    support there), points at a new **Supported platforms** tier matrix in
    `docs/INSTALL.md`, and offers `nix run github:Reddimus/kettle` as a
    zero-build sandbox (F1). And `docs/INSTALL.md` now documents that there's no
    `winget` / `scoop` recipe yet, with the `packaging/homebrew` + `packaging/arch`
    templates pointed out for would-be maintainers (F2). (Audit finding A3 — IME
    / CJK input — is the one item deferred to a verified follow-up: doing it
    right needs preedit rendering + a CJK-IME test environment, and enabling it
    blind would risk the input path; see the design note in the repo memory.)

  - **Performance (cycle 791) — no per-frame allocation for image-free panes.**
    The render loop collected each pane's image placements into a fresh `Vec`
    and `sort_by_key`'d it *every frame for every pane*, even the overwhelmingly
    common 0-or-1-image case where ordering is meaningless — an allocation in
    the hot render path for nothing. It now fast-paths `len <= 1` (iterate the
    slice directly, no alloc/sort) and only collects+sorts when 2+ placements
    genuinely need z-ordering, via one closure so the draw body stays single-
    sourced. Output is identical (higher-z images still land on top).

  - **Docs (cycle 790) — version + config-key drift fixed, and the version
    drift automated away.** The audit found the README status banner stuck at
    `v2.3` and `docs/INSTALL.md` claiming `current latest: v2.3.2` (+ stale
    example `KETTLE_VERSION=` / download URLs) while Cargo.toml was already
    2.4.1 — drift that had recurred every release because the bump was manual.
    Corrected to v2.4.1, and `scripts/release.sh` now bumps every `vX.Y.Z`
    string in `README.md` + `docs/INSTALL.md` in the same atomic step as
    Cargo.toml / flake.nix, so it can't re-stale. Also (E3) the settings overlay
    catalogue and `docs/SETTINGS.md` listed the cursor key as the back-compat
    alias `cursor-shape`; both now use the canonical `cursor-style` (matching
    the authoritative `docs/CONFIG.md`), so the overlay persists the canonical
    spelling and all three agree.

  - **Tests (cycle 789) — drift guards for three previously-untested
    regression-prone units.** The audit flagged critical/correctness logic with
    no unit coverage: **(D1, security)** layout-name sanitization — the only
    barrier between an untrusted `kettle --layout <NAME>` and the filesystem —
    is now extracted to the pure, tested `sanitize_layout_name` (proving
    `../../etc/passwd` collapses to the in-`layouts/` `.._.._etc_passwd`, with no
    separator surviving; a regression dropping the filter would fail the build,
    not silently allow arbitrary-file reads); **(D2)** the `leaf_index_of` ↔
    `nth_leaf` inverse that session restore uses to re-focus the right pane
    across id reallocation (an off-by-one would silently focus the wrong pane on
    relaunch); **(D3)** `keybind_action`, the settings-overlay accessor that
    routes Enter into chord-capture. No behavior change beyond the D1 extraction.

  - **Memory (cycle 788) — overlay text-buffer pools no longer grow unbounded.**
    The audit found that `tab_buffers`, `context_menu_buffers`, and
    `hint_buffers` were grown each frame with `while len < N` but, unlike the
    `pane_buffers` / `settings_buffers` pools, never truncated — so each ratcheted
    to its session high-water mark, retaining idle shaped-glyph `TextBuffer`s
    (GPU + host font state): open 50 tabs then close to 5 and 50 stayed; a
    50-row Lua context menu then a 5-row one kept 50; quick-select with varying
    link densities pinned the peak. Each now `truncate`s to the current count
    right after its grow loop (matching `settings_buffers`), and the drift guard
    is extended to pin all five truncate calls at the source level.

  - **Internals (cycle 787 — investigated, no change) — the per-pane `TermEvent`
    channel stays `unbounded` by design.** The audit flagged it as an OOM risk
    and suggested a bounded channel, but both bounded variants are unsafe here:
    `TermEvent` is `alacritty_terminal::event::Event`, which carries one-shot
    events that must never be dropped (`Exit` → pane close; `PtyWrite` → protocol
    replies to the PTY), ruling out `try_send`-drop; and the sender runs inside
    `processor.advance(..)` *while the reader holds `term.lock()`*, so a bounded
    blocking `send` would block the reader with the lock held and deadlock the UI
    thread (which locks the same `term` to render). The channel is drained every
    UI iteration via `try_recv`, so it does not grow in normal operation. The
    invariant is now documented at the creation site to prevent a future "fix"
    from reintroducing the deadlock.

  - **UI/UX (cycle 786) — modals now consume mouse input instead of leaking it
    to the terminal behind them.** A multi-agent production audit found that the
    `MouseInput`, `MouseWheel`, and `CursorMoved` handlers only gated the
    context menu, so with **search / palette / ssh / settings / layout-picker /
    hint / confirm-dialog / inline title-edit / vi copy-mode** open: a left
    click switched tabs or focused a pane, a right click opened a context menu,
    any click injected mouse-tracking events into the TUI behind the dialog
    (**A1, critical** — the dialog was effectively invisible to the mouse);
    Ctrl+wheel still zoomed the font and plain/Shift wheel still scrolled the
    pane or cycled tabs (**A2**); and with `focus = sloppy`, cursor drift while
    typing into a dialog silently reassigned pane focus (**A4**). All three now
    gate on `any_modal_open()`: pointer presses/wheel are swallowed by any open
    modal *except* a lone context menu (which owns its own click/scroll paths
    and is relocated by a right-click), via the pure, drift-guarded
    `modal_swallows_pointer`; sloppy-focus skips while any modal is open.

  - **UI/UX (cycle 785) — no unpainted-window flash on Windows-11 startup.**
    `App::resumed` shows the OS window and *then* blocks the event loop for
    ~1.5s in `pollster::block_on(Renderer::new(...))` (the wgpu adapter+device
    init — measured 1.48s on an Intel-iGPU/Vulkan box), so the user saw a blank
    / "(Not Responding)" rectangle for that whole stall before the first frame
    painted. The window is now **created hidden** (`with_visible(false)`) and
    revealed with `set_visible(true)` only once the first frame is on the
    surface (in `redraw`, after the render attempt — even on a render error, so
    it can never get stuck invisible), so it appears already-painted (the
    Ghostty / modern-terminal pattern). The first frame is painted by calling
    `redraw` **directly at the end of `resumed`** rather than relying on the
    `request_redraw` event: live testing on Win11 caught that Windows does NOT
    deliver `RedrawRequested` to a window that has never been shown, so a purely
    redraw-event-driven reveal left the window stuck invisible (the OS reported
    the `'kettle'` window `visible=False` indefinitely). A configured
    `window_state = hidden` stays hidden. NOTE: this is a *perceived-quality*
    fix, not a wall-clock
    speed-up — investigation (31-agent workflow) confirmed startup is
    machine/environment-bound (the ~1.5s GPU/Vulkan init + cold-WSL boot when
    the default shell is `wsl.exe`); the empty grid already paints right after
    GPU init, independent of
    the shell (which `portable_pty` spawns async). Reveal decision extracted to
    the pure, drift-guarded `should_reveal_after_first_frame`. Verified live on
    Win11 (winapp MCP) + WSLg Linux; surfaced by the user's startup-latency
    question.

## [2.4.1] — 2026-06-03

  **Patch: live UI/UX verification sweep — one cosmetic fix.** A live,
  interactive, screenshot-driven UI/UX verification of kettle on real
  **Windows 11** (driven via the winapp MCP) and **native WSLg Linux** —
  exercising every overlay/modal/menu and transition, all three Windows shells
  (cmd, PowerShell 7, WSL-Ubuntu), and **tmux + AstroNvim** inside kettle.
  Everything rendered correctly (truecolor, splits/resize-reflow, tabs, settings
  overlay, context menu, cursor-shape changes, treesitter highlighting; Linux
  software-Vulkan/llvmpipe fallback renders cleanly under WSLg) — the sweep
  surfaced exactly one cosmetic defect, fixed below. No features, no breaking
  changes; Windows gauntlet + all-three-OS CI green.

  - **UI/UX (cycle 784) — Settings overlay no longer clips its footer or the
    keybind-capture prompt.** The settings panel hard-coded its width at 44
    character cells, but two display lines are wider: the footer hint
    `↑↓ field  ←→ change  Tab category  Esc close` (~50 cells, so the live
    Windows-11 sweep saw it rendered as "Esc clo") and the in-capture
    `‹press a chord — Esc to cancel›` value, which with its 26-col label is ~59
    cells and overflowed onto the next row. The panel width is now derived from
    the widest display line (new `settings_panel_cols`, with a 44-col floor) —
    the same content-fit approach every other overlay already uses — so the
    panel grows to fit the footer always and the chord prompt during capture.
    Both render passes (buffer-text + quad/highlight) compute it off the same
    `settings_display_lines` output, keeping them in lockstep. Surfaced by the
    live interactive UI/UX verification sweep on Windows 11 (winapp MCP) +
    native WSLg Linux; drift-guarded by `settings_panel_fits_footer_and_capture_prompt`.

## [2.4.0] — 2026-06-01

  **Minor: post-v2.3.2 hardening — the exhaustive-review bundle.** One grouped
  release of the entire post-v2.3.2 follow-up (cycles 778-783) rather than a
  string of small patches. Driven by a per-dimension **production-readiness
  audit** (all 11 named dimensions — Rust, testing, docs, mermaid, install/setup,
  memory, time/space complexity, UI/UX, CI/CD, releases, architecture — assessed
  by a multi-agent workflow with adversarial verification) that rated kettle
  **production-grade across the board** and surfaced exactly three gaps, all fixed
  here: a link-detection O(n²) on URL-dense viewports, a missing aarch64
  supply-chain target in `deny.toml`, and stale install-doc version examples.
  Also bundled: a real config-persistence de-duplication bug fix, two corrected
  stale doc-comments (focus-follows-mouse and detachable-tabs were wrongly marked
  "no-op"), and a new Settings-overlay architecture diagram. No new runtime
  features or breaking changes — bundled as a minor to mark the consolidated
  hardening pass. Windows gauntlet + all-three-OS CI green throughout.

  - **Performance (cycle 781) — URL link detection no longer scans all-rows
    links per match.** In `kettle-core::links`, the autodetect overlap check
    (skip an autodetected URL already covered by an OSC 8 hyperlink) walked the
    entire accumulated `out` Vec for every regex match — O(total_links) per match,
    so O(n²) on a link-dense viewport (e.g. a log full of URLs), risking frame
    stutter. Since a row's OSC 8 links sit contiguously in `out` and regex matches
    are non-overlapping, the check now scans only `out[osc8_start..osc8_end]`
    (this row's OSC 8 links), bounding it by the per-row OSC 8 count. Surfaced by
    the per-dimension production-readiness audit (time/space-complexity).

  - **CI (cycle 782) — `deny.toml` now covers the aarch64-linux target.** The
    supply-chain `[graph].targets` list (whose comment promises "match what
    release.yml + ci.yml build for") omitted `aarch64-unknown-linux-gnu`, even
    though release.yml builds it (cycle 767) and ci.yml checks it (cycle 758) — so
    a Linux/ARM-only advisory or banned-dep pull-in could have slipped past
    `cargo deny`. Added the triple.

  - **Docs (cycle 783) — bump install version examples to v2.3.2.** The
    `KETTLE_VERSION=` pin examples and the SHA-256 download URLs in
    `docs/INSTALL.md` + `README.md` still referenced v2.3.1. The installers
    resolve `/releases/latest` dynamically so they worked regardless, but the
    copy-paste examples now match the current release.

  - **Fixed (cycle 779) — `persist_config_toggle` de-duplicates repeated keys.**
    When a config file already had two lines for the same key (or a key somehow
    got written twice), each toggle rewrote *every* matching line, so identical
    lines accumulated indefinitely. The parser is last-wins so behavior was always
    correct, but the on-disk file bloated. Now only the first match is rewritten
    and later duplicates are dropped — collapsing to a single line, matching
    `append_keybind`'s drop-old semantics. Drift guard
    `persist_config_toggle_collapses_duplicate_keys_to_one`. Found by the
    config-validation audit dimension (re-run after a prior tooling glitch).

  - **Docs (cycle 780) — correct two stale config field doc-comments.** The
    config-validation audit flagged `focus` and `detachable_tabs` as "no-op",
    but adversarial verification against the code showed both were *stale
    comments*, not real no-ops: `focus = sloppy` (focus-follows-mouse) **is**
    wired (cycle 360, `app.rs` cursor-move handler) — the comment claiming it
    "isn't wired yet" predated that impl; and the detachable-tabs **feature**
    landed in cycles 397-410 (the comment said "No-op until Bucket-D lands").
    Comments corrected to match reality (only the `detachable_tabs` on/off
    *toggle field* remains unconsumed — the action is always available). No code
    change; prevents a future maintainer from mistaking working features for dead
    code. (The audit's other items were verified false/low-value: `cell-width`
    is a no-op stub so surfacing its clamp would add noise; the no-op compat
    stubs are intentional, documented Terminator-config acceptance.)

  - **Docs (cycle 778) — add a Settings-overlay state diagram to ARCHITECTURE.md.**
    The in-app Settings overlay + interactive keybind editor (cycles 756/766) — a
    headline UI subsystem — was referenced in the crate and render-pass diagrams
    but had no dedicated diagram of its own. Added a `stateDiagram-v2` mapping the
    full UI/UX transitions (Closed → Browsing → EditValue/Capturing, with the
    persist→reload and chord-capture→append_keybind flows) plus a prose subsection,
    and listed the overlay under "most recent additions". Verified the other 8
    ARCHITECTURE.md mermaid diagrams still match the current architecture.

## [2.3.2] — 2026-06-01

  **Patch: post-v2.3.1 hardening — audit fixes + macOS render verification + a
  self-updating screenshot.** A nine-cycle grouped release driven by two
  exhaustive multi-agent audits (an image/screenshot audit and a double-verified
  8-dimension codebase audit). Headline: a **data-loss fix** in the Unix
  detachable-tab handoff (a swallowed `send_fds` error could destroy the source
  tab), consistent clipboard error logging, and a corrected config default. Plus
  the macOS **render pipeline is now verified on real Metal hardware in CI**, the
  week-long-red `actionlint` gate is fixed, the README hero/showcase screenshot is
  refreshed and made **self-updating** (version tracks the crate), the macOS
  `.iconset` is normalized to 8-bit RGBA, and two release/CI robustness gaps are
  closed. No runtime API change beyond the bug fixes. Windows gauntlet + all-three-OS
  CI green.

  - **Fixed (cycle 776) — cross-process tab handoff no longer silently loses a
    tab on a socket error.** In the Unix SCM_RIGHTS detachable-tab path
    (`try_move_tab_to_new_window_scm_rights`), the `fd_transport::send_fds` result
    was discarded with `let _`, then the source tab was closed unconditionally. If
    the send failed (`ENOBUFS` / `EMSGSIZE` / any socket error) the target window
    never received the tab, yet the source had already destroyed it — **data
    loss** with no log trace. The send result is now checked: on failure it logs
    `log::error!` and returns `false` **without** closing the source tab, so the
    caller falls through to the file-fallback and the user keeps their session.
    Found by an exhaustive multi-agent audit (double-verified). *(Unix only.)*

  - **Fixed (cycle 777) — clipboard copy failures are now logged consistently.**
    Four `clipboard.set_text(...)` sites — selection copy, OSC 52 write, the copy
    action, and hint-click copy — swallowed errors with `let _`, while the vi-mode
    yank path already logged via `log::warn!`. A user whose clipboard wasn't
    working had no way to diagnose it for the majority of copy paths. All four now
    log a `log::warn!` with a distinct site label, matching the yank precedent.

  - **Docs (cycle 775) — correct the `background-darkness` default in CONFIG.md.**
    The config reference listed the default as `1.0` (no tint), but the code
    defaults to `0.5` (test-pinned at `lib.rs:4193`), matching Terminator's
    upstream `background_darkness`. A user enabling a background image got 50%
    darkening the docs said wouldn't happen. Fixed the docs (the code is the
    Terminator-correct source of truth), not the code.

  - **Packaging (cycle 772) — normalize the macOS `.iconset` to 8-bit RGBA and
    generate it from the SVG.** The 10 `packaging/macos/kettle.iconset/*.png`
    files had been committed as **16-bit**/color RGBA — the same depth that
    caused the v2.1.1 GNOME "Super-key" blank-icon bug on Linux. `iconutil`/Finder
    consume 16-bit fine so the macOS `.icns` build never broke, but it was
    inconsistent with the repo's documented 8-bit policy and ~3× larger on disk.
    All 10 are re-exported to 8-bit RGBA (249 KB → 89 KB, **64% smaller**, pixels
    visually identical), and `scripts/gen-icons.sh` — previously Linux-only — now
    also rasterizes the full iconset from the single-source `kettle.svg` via
    `rsvg-convert` (which emits 8-bit), so the depth can't drift back.

  - **CI (cycle 773) — fail the release loudly on a missing icon raster.**
    `release.yml` copied the Linux PNG icons with a trailing `|| true`, which
    silently swallowed a `cp` failure — a missing or renamed raster would have
    shipped an **iconless tarball** instead of failing the release. Dropped the
    `|| true` so the glob mismatch surfaces at release time (the SVG copy on the
    line above already had no such guard).

  - **CI (cycle 774) — give the `cargo-machete` badge a refresh path.**
    `machete.yml`'s push/PR triggers are path-filtered to Cargo manifests, so a
    long run of non-manifest commits could leave the README badge showing a stale
    state (or blank on a fresh branch) — unlike `audit.yml` (daily) and `deny.yml`
    (weekly), which already cron. Added a weekly `schedule` mirroring `deny.yml`'s
    Sunday 06:00 UTC slot so the badge always reflects a recent run.

  - **Docs (cycle 771) — refresh the README hero / UX showcase screenshot, and
    make it self-updating.** The `--screenshot` hero (`docs/images/kettle-hero.png`,
    embedded at the top of the README) and the UX showcase
    (`docs/images/kettle-showcase.png`) are both rendered from the hardcoded
    `DebugScene::Default` scene in `kettle-render`. That scene's demo content had
    been frozen since the v0.1.0 era: it baked a literal `Compiling kettle v0.1.0`
    and `74 passed` into the rendered pixels and listed a pre-v2.x feature set — so
    by v2.3.x the hero image looked years out of date even though the PNG still
    matched the (equally frozen) scene. The scene now (a) sources its version label
    from the crate version via `SCREENSHOT_DEMO_VERSION = env!("CARGO_PKG_VERSION")`
    so it renders the real `kettle v2.3.1` and auto-refreshes on every release bump
    with zero code churn; (b) shows the current `481`-test workspace count; and
    (c) advertises the current headline features (`splits · tabs · search ·
    settings` / `keybinds · sixel · kitty · OSC 8`) in place of the old
    `ligatures` / `OSC 8`-only list. Both PNGs were regenerated with the documented
    commands (`--cols 120 --rows 32` hero, `--cols 100 --rows 30` showcase) and the
    hero's reproduction command — previously undocumented — is now recorded in the
    README. A `kettle-render` drift guard
    (`screenshot_demo_version_tracks_crate_version`) asserts the demo version tracks
    the crate version and is never the legacy `0.x`, so the screenshot can't
    silently re-stale.

  - **CI (cycle 770) — fix the long-red `actionlint` workflow-lint gate.** The
    `actionlint` check had failed on *every* push since 2026-05-25 (through the
    entire v2.2.x/v2.3.x series) over a single `shellcheck` SC2015 finding: the
    `--profile cibad` CLI smoke used the `A && B || C` idiom, where the error
    branch `C` would also fire if the success `echo B` ever failed. Rewritten as
    a proper `if/then/else`, which is both correct and SC2015-clean — turning the
    workflow-lint gate green for the first time in a week.

  - **CI (cycle 769) — macOS render-behavior verification on real Metal
    hardware.** The `--gpu-info`, `--screenshot`, and `--screenshot-menu` smokes
    — previously gated to the Linux job's software-Vulkan adapter — now also run
    on the `macos-latest` runner, which has a real Metal GPU. This is the first
    automated coverage of kettle's *actual* macOS render path: adapter
    resolution, the full text + quad + image draw, GPU readback, and the
    `image::save` PNG encode all execute on macOS hardware (the existing
    `offscreen_selftest` unit test only compiles the WGSL shaders). The
    `cargo build --release` that feeds these smokes was hoisted out of the
    Linux-only headless step into a shared non-Windows step, and the size
    assertions switched from GNU `stat -c%s` to the BSD/macOS-portable
    `wc -c <`. The interactive/windowed UI (Spaces stickiness, menu clicks,
    Retina/dock behavior) still requires a human-driven Mac; this closes the
    *render-pipeline* half of macOS verification without one. Windows stays
    excluded — its runtime smokes are bash-only.

## [2.3.1] — 2026-06-01

  **Patch: macOS sticky + bundle polish.** Implements the macOS `sticky`
  window behavior (all-Spaces) that had been a no-op since winit dropped the
  API, and rounds out the `.app` Info.plist. macOS build verified green on CI.

  - **macOS:** `sticky = true` is now implemented — the window joins all Spaces
    (Mission Control workspaces) via `NSWindowCollectionBehavior` through objc2,
    replacing the no-op stub left when winit 0.30 dropped the native method.
    The `.app` Info.plist also gains `LSApplicationCategoryType`
    (developer-tools) and `CFBundleInfoDictionaryVersion`, and a latent
    invalid-XML comment (a literal `--`) was fixed.

## [2.3.0] — 2026-06-01

  **Minor: the audit-hardening + keybind-editor release.** An exhaustive
  multi-agent audit of every crate, UI/UX state, and CI surface drove a sweep
  of overflow/panic-safety and DoS-cap fixes, render + search hot-path
  allocation cuts, and a real per-pane title-map leak fix; the Settings overlay
  gains an **interactive keybind editor**; and releases now ship an ARM64-Linux
  artifact. Windows gauntlet green; Linux logic tests green.

  - **Hardened (overflow/panic safety):** the PTY is now opened with
    `clamp_pty_dim` so a very wide or HiDPI grid can't overflow the u16
    pixel dimensions (the resize path already did this); the Unicode-
    placeholder reader path uses checked access instead of `expect()` so it
    can't panic the reader thread; and image composition computes byte offsets
    in `u64` to avoid wraparound on very large frames. Found by an exhaustive
    multi-agent audit of every crate.
  - **Performance (render):** the per-frame quad / image / menu vectors and the
    per-pane rich-text span vector are now pre-sized, eliminating repeated
    reallocations on the 60fps render hot path; titlebar text-clip bounds are
    clamped to ≥0.
  - **Performance (search / links):** scrollback regex search and viewport URL
    detection reuse their per-line scratch buffers instead of allocating a
    fresh `String` + `Vec` for every line/row (≈2 allocations total instead of
    one pair per line — meaningful on a large scrollback).
  - **Fixed (leak + UX):** the per-pane title-tracking map (used by the
    `title_changed` plugin hook) now drops entries for closed panes instead of
    growing unbounded over a long open/close session; the text cursor stays
    steady while the title-edit bar, confirm dialog, or settings overlay is
    open; and the settings overlay reads choice labels with bounds-checked
    access.
  - **Hardened (DoS):** kitty graphics transmissions now enforce a **global**
    in-flight memory cap (1 GiB across all slots) in addition to the existing
    per-slot 384 MiB cap, so a hostile PTY stream chaining many concurrent
    large partial image/animation transmissions can't accumulate ~12 GiB.
  - **Added:** the Settings overlay now has an **interactive Keybinds editor** —
    a Keybinds category lists common actions with their current chord; press
    Enter on a row, then press the chord you want, and it's bound immediately
    and saved to your config (Esc cancels). Covers split/close/tab-nav/search/
    palette/settings/zoom/copy/paste.
  - **Added (ARM64 Linux):** releases now ship a `kettle-linux-aarch64.tar.gz`
    artifact (Raspberry Pi 4/5, ARM servers/VPS, ARM laptops on Linux),
    cross-compiled on CI; the one-line installer auto-detects aarch64/arm64.
  - **Docs/CI:** corrected the documented macOS config path (kettle uses
    `~/.config`, not `~/Library/Application Support`); the release workflow now
    enforces the full `## [X.Y.Z] — YYYY-MM-DD` CHANGELOG format (matching
    release.sh); and `scripts/release.sh` rolls back the version bump if the
    pre-tag build fails, so a failed release leaves a clean tree.

## [2.2.0] — 2026-05-31

  **Minor: the production-hardening + Settings release.** Adds an in-app
  **Settings overlay** (Ctrl+,) so common options are editable without
  touching a config file, makes Linux/WSL/headless startup robust with a
  software-GPU fallback, fixes X11 middle-click PRIMARY-selection paste, adds
  a non-blocking ARM64-Linux CI check, and ships a non-technical Getting
  Started guide. Verified on Win11 (live) and WSLg Ubuntu 24.04 (build + full
  offscreen render).

  - **Fixed (Linux/headless/WSL):** kettle now falls back to software GPU
    rendering (Mesa llvmpipe / lavapipe, or WARP on Windows) when no hardware
    adapter is available, instead of hard-erroring with "no suitable GPU
    adapter". This lets kettle run under WSLg, headless VMs, minimal Linux
    installs, and GPU-less CI. Hardware is still preferred; the software path
    only engages on failure and logs a warning.
  - **Fixed:** the confirm dialog ("Close pane?", "Quit?") is now treated as a
    modal — opening search/palette/a menu over it dismisses it instead of
    stacking two overlays, and mouse/scroll no longer fall through to the
    terminal behind it.
  - **Changed:** `hide_from_taskbar` / `sticky` now log an informational note on
    platforms where winit can't yet apply them (X11/Wayland), and kettle warns
    at startup when the clipboard is unavailable (headless/SSH/sandbox), so
    these degrade visibly instead of silently.
  - **Fixed (X11):** middle-click / `paste_primary` now pastes the **PRIMARY
    selection** (the last mouse-highlighted text) on X11, matching standard
    terminal behavior, instead of the regular clipboard. Falls back to the
    clipboard on Wayland/macOS/Windows (no separate PRIMARY there). Paste
    safety (size clamp, bracketed-paste, broadcast scoping) is now shared
    across all paste channels.
  - **Changed (Linux):** the X11 `WM_CLASS` is derived from the binary name
    (default `kettle`) instead of hardcoded, so renamed/forked binaries still
    group correctly in GNOME/KDE task switchers and match their own `.desktop`.
  - **Added:** an in-app **Settings overlay** — a keyboard-navigable panel of
    the most-used options (font size, opacity, padding, cursor, scrollbar,
    bell, scrollback, copy-on-select, focus mode, …) grouped into categories.
    Open it with **Ctrl+,** or **right-click → Settings…**; ↑↓ moves between
    fields, ←→ changes a value (Tab switches category, Esc closes). Changes
    apply live and persist to your config file — no hand-editing needed. An
    "Advanced" path to the raw config remains for the long tail.
  - **Docs:** added a non-technical [Getting Started](../GETTING-STARTED.md)
    walkthrough and a [Settings panel](../SETTINGS.md) reference; the
    architecture diagrams now include the settings overlay.
  - **CI:** added a non-blocking aarch64-linux cross-compile check so ARM64
    Linux (Raspberry Pi, ARM servers) build regressions are caught early.

## [2.1.2] — 2026-05-30

  **Patch: completes the Super-key icon fix.** v2.1.1 converted the icons
  to 8-bit (necessary) but the launcher tile was still blank in the Ubuntu
  Super-key / GNOME Activities search — so this finishes the job.

  - **Fixed:** the user-install launcher icon now actually renders in the
    GNOME / Ubuntu Super-key search. Root cause (isolated on GNOME Shell 46
    / Wayland): gnome-shell's `StIconTheme` does **not** resolve a *themed*
    icon name (`Icon=kettle`) from a user-local
    `~/.local/share/icons/hicolor` that has no `icon-theme.cache` — even
    though GTK's own `IconTheme` does (which masked the bug:
    `Gtk.IconTheme` resolves and loads `kettle`, but gnome-shell renders
    nothing). The cycle-540 logic that skipped `gtk-update-icon-cache`
    assumed GNOME would directory-scan the icon by name; it doesn't.
    `scripts/install.sh` now rewrites `Icon=` to the **absolute installed
    PNG path** for the no-sudo user install, which sidesteps icon-theme
    resolution entirely — the icon renders regardless of cache state, and
    (unlike generating a user-level cache) it can't go stale and hide other
    apps' icons. The shipped `packaging/linux/kettle.desktop` keeps the
    themed `Icon=kettle` for distro packages, whose post-install hooks
    maintain the system hicolor cache.

  Note: an existing GNOME session caches its icon theme, so after the first
  install you may still need to log out / back in once (or toggle the icon
  theme) for the running shell to pick up the new launcher entry.

## [2.1.1] — 2026-05-30

  **Patch: the Linux icon + hardening release.** The headline fix is the
  Ubuntu Super-key launcher icon, which showed blank even though the icon
  files installed correctly. Plus a batch of robustness fixes shaken out
  by a full-repo sweep, each landed with a regression test.

  - **Fixed:** the kettle icon now appears in the GNOME / Ubuntu Super-key
    (Activities) search. The shipped `packaging/linux/kettle-*.png` icons
    were encoded at **16-bit/color** depth, which GNOME Shell's icon loader
    silently fails on, so the launcher tile rendered blank while an 8-bit
    icon in the same folder showed fine. The icons are now rasterized from
    `kettle.svg` as standard **8-bit/color RGBA**, and the set gained 16px
    and 24px sizes for the panel / search-result list.
  - **Added:** `scripts/gen-icons.sh` — rasterizes the SVG to every PNG
    size as 8-bit (needs `rsvg-convert`), making the icon set reproducible
    from one vector source. `install.sh` now installs the 16/24px sizes too
    (see docs/INSTALL.md → "Regenerating the app icons").
  - **Fixed:** a PTY resize on a very wide HiDPI grid could overflow the
    `u16` pixel-extent math (`cell_w * cols`), panicking in debug and
    wrapping in release; the product is now computed in `u32` and clamped.
  - **Fixed:** a malformed kitty animation with zero frames panicked at
    render time (`imgs[0]` index out of bounds) — an out-of-bounds index
    reachable from untrusted PTY output. The frame lookup now returns
    `Option` and the renderer keeps the existing image instead of crashing.
  - **Fixed:** broadcast input no longer derives a phantom pane id `0` when
    the active tab has no panes; it now anchors on the real focused pane and
    short-circuits when there is none.
  - **Changed:** `release.sh` now escapes the previous-version string
    before splicing it into its `sed` version-bump patterns, so a
    pre-release tag with regex metacharacters can't mis-match.
  - **Internal:** added unit tests for previously-untested modules
    (`iterm`, `sixel`, `extract`, core `event`) covering parse boundaries
    and the failure paths most likely to regress.

## [2.1.0] — 2026-05-30

  **Minor: the HiDPI + WSL release.** kettle now renders text at the
  correct size on high-DPI Windows 11 displays (the headline fix — text
  was tiny at >100% scaling), supports launching WSL distributions as the
  shell, and gains a `pane_close` plugin event. All verified end-to-end on
  a Surface Book 3 at 200% scale: readable text, an interactive Ubuntu WSL
  shell, and 6 split→close cycles with zero crashes.

  - **Fixed:** text now renders at the correct size on HiDPI displays.
    On Windows 11 at >100% display scaling (and Retina / fractional-scale
    monitors), the font appeared tiny because the renderer stored the
    window's scale factor but never applied it to the glyph metrics —
    a 13pt font drew at ~6.5px on a 200% display. Font metrics and cell
    sizing now multiply the logical font size by the device-pixel scale,
    and kettle rescales live when the window moves between monitors of
    different DPI (the `ScaleFactorChanged` event is now honored).
  - **Added:** documented launching WSL (e.g. Ubuntu) as your shell on
    Windows via `command = wsl.exe -d Ubuntu` (see docs/CONFIG.md). The
    `login-shell` option no longer mis-injects `-l` for `wsl.exe` — `-l`
    means "list distributions" to wsl and would have exited instead of
    opening a shell.
  - **Changed (internal):** the renderer now releases per-pane text
    buffers when splits close, instead of holding them at the session's
    peak pane count.
  - **Added (plugins):** a `pane_close` event hook —
    `kettle.on('pane_close', function(pane_id) … end)` fires with the
    pane id whenever a split is closed, completing tab/pane close-event
    parity for plugins (status bars, per-pane overlays, activity
    watchers).
  - **Fixed (Windows/Linux):** the running window now shows kettle's icon
    in the title bar (the system-menu glyph beside the window controls),
    the taskbar button, and the Alt-Tab switcher. winit leaves the window
    icon unset by default, so the title bar showed the generic placeholder
    even though the `.exe` already embeds the icon as a resource (which
    only covers Explorer / a pinned shortcut). The icon is now set at
    window creation via `with_window_icon`.
  - **Docs:** install instructions now reference the current release
    version instead of stale `v1.45.1` / `v1.46.3` examples.

## [2.0.0] — 2026-05-30

  **Major: the Windows 11 / PowerShell 7 release.** kettle 2.0 makes
  PowerShell 7+ the default shell on Windows (a default-behavior change
  from the old `cmd.exe` — the reason this is a major bump), adds OSC 9;4
  taskbar progress so pwsh `Write-Progress` / `winget` drive the taskbar
  button exactly like Windows Terminal, and folds in the v1.47.0 line —
  the close-split UI-thread deadlock fix and crash logging. Net result:
  kettle can now surface everything a Windows 11 PowerShell 7 session
  drives in a modern terminal. Verified end-to-end on a Surface Book 3
  (Windows 11 build 26200).

  New since v1.47.0:
  - cycle 745 — OSC 9;4 taskbar progress via `ITaskbarList3` (detail below).

  Carried from v1.47.0 (now part of the 2.0 line):
  - **PowerShell 7+ as the Windows default shell** (cycle 743) — the
    default-behavior change behind the major bump; override with `shell =`.
  - **Close-split deadlock fix** — `Ctrl+Shift+W` after a split no longer
    freezes the window into "not responding" (cycle 742).
  - **Crash logs** — a `panic = "abort"`-safe hook writes panics to
    `%LOCALAPPDATA%\kettle\crash\` / `$XDG_STATE_HOME` (cycle 741).

  See the [1.47.0] section below for the carried-in per-cycle detail.

  cycle 745 — **Add: OSC 9;4 taskbar progress (PowerShell 7 / Windows
              Terminal parity)**: kettle now parses the ConEmu/Windows
              Terminal `ESC ] 9 ; 4 ; <state> ; <pct> ST` progress
              sequence — pwsh 7 `Write-Progress` (with
              `$PSStyle.Progress.UseOSCIndicator = $true`) and `winget`
              emit it — and drives the Windows taskbar button via
              `ITaskbarList3` to reflect the FOCUSED pane's progress
              (normal / error / indeterminate / paused, 0–100%), exactly
              like Windows Terminal. The extractor surfaces a
              `Chunk::Progress`; the reader records the latest value per
              pane; the App polls the focused pane each frame and updates
              the taskbar (dedup'd, so an unchanged value costs nothing).
              Cross-platform by construction — a no-op off Windows (a
              macOS dock badge can follow via objc2). Parsing is
              unit-tested (all five states + pct clamp + non-9;4 OSC 9
              left untouched); the COM path is verified live on Windows
              11 build 26200 (each state logged "applied via
              ITaskbarList3"). Closes the last identified pwsh-7 terminal
              gap, so kettle can now surface everything a Windows 11
              PowerShell 7 session drives in a modern terminal.
