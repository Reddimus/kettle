# Audit follow-up — deferred items

The v2.32.0 audit (two exhaustive multi-agent passes: per-crate + seven
cross-cutting dimensions, every finding adversarially re-verified) confirmed
~114 findings. v2.32.0 shipped every HIGH-severity correctness / robustness /
security bug plus the cheap high-value mediums and the full docs sweep. The
items below were **deliberately deferred** because they are large, structural,
or need a live GPU / the maintainer's interactive desktop to validate. They are
tracked here so they are not lost.

## Large / structural

- **RUSTSEC-2026-0192 upstream exit for `ttf-parser`.** Issue
  Reddimus/kettle#36 stays open until `ttf-parser` disappears from the dependency
  graph. The only accepted temporary path is `glyphon → cosmic-text → fontdb`;
  `scripts/check-ttf-parser-scope.sh` guards that scope in CI. Upstream tracking:
  RazrFalcon/fontdb#90, pop-os/cosmic-text#352, pop-os/cosmic-text#526, and
  grovesNL/glyphon#123. Rechecked 2026-08-11: crates.io still publishes
  `cosmic-text 0.19.0` and `glyphon 0.12.0`; both projects' `main` manifests
  still select that pair, and cosmic-text still pins `fontdb 0.23`. The focused
  upstream bump to `fontdb 0.24` is open as cosmic-text PR #526, currently
  mergeable but blocked, so Kettle cannot consume the already-published fontdb
  fix without maintaining a fork or unreleased Git dependency. Close this only
  after an upstream release, updating the text-rendering stack, confirming
  `cargo tree -i ttf-parser` reports no matches, and removing
  RUSTSEC-2026-0192 ignores from `deny.toml` and `.github/workflows/audit.yml`.
- **RUSTSEC-2026-0253 upstream exit for `lru`.** Issue Reddimus/kettle#207
  tracks the `lru 0.16.4` unsoundness warning until glyphon accepts a fixed
  release (`lru >=0.18.2`). The reviewed product path is only
  `lru 0.16.4 → glyphon 0.12.0 → kettle-render`: glyphon's cache key is
  `Copy`, so its destructor cannot panic, and glyphon calls `peek_lru`,
  `pop_lru`, and `get`, never the affected `LruCache::pop()` method. A fork or
  vendored text renderer would add more supply-chain surface without making a
  reachable product path safer. `scripts/check-lru-scope.py` therefore reads
  the committed lockfile's all-feature, all-platform metadata graph and pins
  both upstream crates.io sources, versions, and every reverse edge; a source
  replacement, target-specific new consumer, or version fails Linux CI and
  requires a fresh reachability review. Once upstream
  resolves it, remove the audit ignores and the guard in the same change, then
  close #207.
- **`app.rs` god-file split + testability seams.** Extract dispatch / frame /
  modals / ctl-glue into focused subsystems and make per-event handlers return a
  typed `Outcome` command list (pure deciders + a thin applier), replacing the
  source-text drift guards with behavioral unit tests. Large; best done as its own
  multi-session refactor after the small correctness fixes have settled.
- **Vertical-list pickers.** The command-palette / layout / ssh pickers render as a
  one-line bottom strip that clips matches on narrow windows; rework into a
  scrollable vertical list reusing the context-menu panel machinery (also makes
  room for per-row keybind hints).

## Found by the 3.2.0 appearance gate

- **Light themes draw the macOS window title through the traffic lights**
  ([#251](https://github.com/Reddimus/kettle/issues/251)). The title lands at the
  far left of the titlebar, over the red and yellow buttons, with its leading
  characters clipped outside the window. `Alabaster`, `3024 Day` and `Adwaita`
  reproduce it; `TokyoNight` does not. Not a regression — the shipped 3.1.1
  bundle does the same — and not a stale frame, since it survives a full-screen
  re-layout. `apply_macos_window_chrome` only sets
  `with_titlebar_transparent(false)`, so the placement comes out of the
  `NSWindow.appearance` switch that `with_theme` performs rather than out of any
  drawing this repo does, which is why it is filed rather than patched.
  [`APPEARANCE-GATE.md`](APPEARANCE-GATE.md) has the pixel evidence.

## Terminal / protocol

- **Kitty graphics acknowledgement/query response path.** Immediate
  acknowledgements/errors and capability-query replies still need bounded
  `Chunk::PtyReply`-style plumbing, so probers such as `kitten icat` can
  distinguish supported operations from silent failure.
- **Kitty image-id replacement lifecycle.** Retransmitting data for an existing
  id must remove every old physical/virtual/relative placement before retaining
  the new pixels, including for transmit-only `a=t`. The decoder and terminal
  core need an ordered replacement signal rather than inferring this from a
  later placement.
- **Kitty relative `Q=` selection.** Relative-chain origin maps currently
  collapse concrete parents by image id; preserve the full `(image id,
  placement id)` key so a child selects the exact named parent when one image
  has multiple placements.
- ~~**Inline-image scrolling inside partial DECSTBM margins.**~~ Done in the
  next release: the terminal engine now emits bounded, ordered scroll-region
  events with direction, margins, count, and monotonic screen-top ids. Images
  wholly inside the margins move and permanently crop their destination/source
  range at an edge; images already crossing a margin remain fixed. Full-screen
  and top-anchored coalesced scrolls preserve exact history anchors, while
  overflow clears both graphics buffers and resynchronizes fail-safe.
- ~~**OSC 52 selection target** (`p`/`s` vs `c`).~~ Done in the next release:
  `ClipboardType::Selection` now reads and writes Linux PRIMARY through
  arboard, without falling back to CLIPBOARD on a failed query; platforms
  without a separate selection retain their single clipboard channel.
- ~~**Vi-mode over scrollback.**~~ Done in the next release: Kettle now toggles
  `alacritty_terminal`'s native vi mode and dispatches its `ViMotion`, so the
  engine owns viewport following, history bounds, cursor, selection, reflow,
  and eviction. The UI retains only pane ownership and visual-mode intent.

## UX / feedback

- ~~**Surface malformed-config diagnostics in the GUI.**~~ Done in v2.36.5:
  live reload now fires an edge-triggered desktop notification listing the
  ignored malformed lines (`should_notify_malformed` + `load_reloaded_config`).
- ~~**Keybind-capture conflict and Settings persistence feedback.**~~ Fixed:
  rebinding an in-use chord requires confirmation, and failed Settings writes
  are surfaced instead of being silently treated as saved. Search now has a
  grapheme-aware editor with selection, caret/word movement, Home/End, and
  bounded paste.
- **Give the modal text overlays a real editor, not just correct rules.**
  Partially addressed: the title editors, command palette, SSH launcher, layout
  picker, and Settings path prompt now share `kettle-ui/src/modal_input.rs`, so
  they filter Command chords, paste, delete whole grapheme clusters, and stop at
  4 KiB — the four things a live naive-user probe caught them getting wrong.
  What they still lack is the *editor*: no caret, no selection, no Home/End, no
  word movement. Typing a typo in the first character of a tab name still means
  deleting everything after it.

- **Clicking outside a modal dismisses some of them and not others.** Measured
  with the same gesture at the same coordinates against each: the context menu
  closes on an outside click, while the command palette and the layout picker
  stay open — the click is consumed (`handled: true`) and nothing happens. A
  person who opens the palette by accident and clicks away to get rid of it has
  no way out until they find Escape.

  Not fixed alongside the text-entry rules because it is a different concern:
  it needs per-modal rect hit-testing and a decision about whether the
  dismissing click should also pass through to what is underneath (the context
  menu currently swallows it). Both are choices worth making deliberately
  rather than inside a correctness fix.
- **Two more raw-`text` consumers still carry the Command-chord bug.**
  `vi_mode_key` and `context_menu_key` both take `KeyEvent::text` and act on it
  without a modifier check, the same way the five text fields did. Neither
  builds a buffer, so `modal_input::accept_text` is not a drop-in for either;
  they need arm-level guards like the confirm dialog's. Listed with `hint_key`
  below so the remaining set is named rather than partially recorded.
- **`hint_key` has the same Command-chord bug the modal fields just lost.** It
  still takes raw `KeyEvent::text`, so on macOS the `v` that `⌘V` produces is a
  candidate hint selection. It is outside the five append-only text fields this
  round covered — hint mode consumes single characters as labels rather than
  building a buffer, so it needs a different shape of fix, not
  `modal_input::accept_text`. Named here so it is not rediscovered as new.
- **`open_text_modal` cannot see the modals that outrank it.** It resolves the
  six text fields, but a confirm dialog, the context menu, vi-mode, or hint mode
  can own the keyboard *above* one of them. When that happens `dispatch_ui_key`
  types into a field a real keystroke would not reach. This is pre-existing
  rather than new — the old search-only gate had exactly the same hole — and the
  precedence drift guard cannot catch it, because it compares the two orders
  against each other rather than against the modals neither one lists. The fix
  is for the resolver to report "some other modal owns the keyboard" and refuse,
  which needs the non-text modals enumerated in the same place.

  Consolidating them behind `search_input::SearchEditor` is the finish line, and
  it is a bigger change than it looks: `TitleEditOverlay` and its siblings carry
  a bare `String` to the renderer, which measures it and parks the caret at the
  end, so a real cursor means new fields in the `kettle-render` overlay structs,
  caret and horizontal-scroll rendering for each one, and a decision about how
  the IME preedit path (`with_preedit`) composes with a mid-string cursor.
  Estimate: one focused change across `kettle-ui` and `kettle-render`, with
  pixel tests for the caret in each overlay. Worth doing; not worth bolting on
  to a correctness fix.

## Performance (measure first)

- ~~**redraw scroll-on-output lock**~~ Done in the next release: tab activity
  and `scroll-on-output` now use each pane's lock-free `output_generation`
  edge. Idle/UI-only frames acquire no history locks, in-place and alternate
  screen output are no longer missed, and the terminal lock is taken only when
  the user opted into `scroll-on-output` and a generation actually changed.
- ~~**Remote-poll tick**~~ Done in the next release: one bounded process-tree
  worker now coalesces the newest roots across the checked-out window and every
  stored window. The UI consumes only completed snapshots; it never walks the
  OS process table. Linux retains no cwd string per process, reads cwd only for
  the selected foreground pid, and applies byte/node/task/argv bounds.
- ~~**`placeholder_tiles` scanned the grid before checking for any virtuals**~~
  Fixed in the next release. `placeholder_tiles` ran `placeholder_cells` — a
  walk of every visible cell under the `term` lock — and only then locked
  `virtuals` to discover the map was empty, which it is in every pane that has
  never received a kitty virtual placement. It now snapshots `virtuals` first,
  drops that guard, and reads the grid only when there is something to resolve.
  Measured on a 300x80 grid of text, `opt-level = 1`, Apple Silicon:
  **127 µs per call**, paid per visible pane per frame, and paid while holding
  the mutex the PTY reader blocks on. The snapshot-then-read order is
  `relative_tiles`' existing discipline, so the ABBA hazard is unchanged. The
  snapshot lives in `virtuals_snapshot`, whose owned return type makes the lock
  release a compile-time property rather than a review note: the previous source
  guard asserted text order and passed on a flattened body that held the guard
  across the grid read, and a brace-depth rewrite of it still passed on a body
  that moved the guard out of its block.
- **PTY I/O worker consolidation:** each pane currently uses a parser thread
  plus a blocking pump thread so DEC 2026 deadlines remain independent of a
  blocked native read. The channel is bounded and buffers are recycled, so this
  is measured thread-count debt rather than a memory-growth bug. Profile large
  pane counts before considering an async/native-overlapped reader or a shared
  I/O service; any replacement must preserve ConPTY teardown, Unix portability,
  parser deadlines, and per-pane backpressure.
- Per-window `FontSystem` sharing; lazy system-font load on first frame. Both are
  speculative — profile on the maintainer's machine before implementing.

## Deferred from the 2026-08-22 boundary audit

Seven boundary units were audited, each cross-checked with codex and then
handed to a refuter told to default to "not real". 43 findings were raised and
36 survived. One critical and three high were fixed in that pass; what follows
is what was left, with the reason.

Severities below are the refuter's, not the auditor's. Several were lowered on
review, and two the auditor filed as security are robustness.

Four items were fixed for 3.2.0 and no longer appear below: the Sixel
total-work bound, the session-log stripper's resync bound, the OSC 7 / 9;9 /
133 D conversions, and `presence::live_entries` deleting a claim it could not
open. The Sixel cap was measured rather than guessed. A 2 MiB `!8191~$` body
decoded in 13.3 s before the cap and 387 ms after, about 5.4 ns per column
write, and the cap lives in `GraphicsLimits` beside the other graphics caps so
tests can lower it.

### Needs an API change

- **`hints::detect` takes a char index and calls it a column**
  (`kettle-core/src/hints.rs:79`, medium). Callers hand it row text whose wide
  characters occupy two columns, so every hint past a CJK character or emoji is
  offset, and quick-select copies or opens a truncated string. The fix is not
  local: column identity has to travel with the text rather than be inferred,
  which means `detect` taking a byte-to-column map or an iterator of
  `(column, char)` and every caller changing with it. `links.rs:118` has the
  same root cause in a smaller form, under-reporting `end_col` by one cell when
  a link ends in a wide glyph.

- **Graphics transient budget is process-wide with no per-terminal ceiling**
  (`kettle-vt/src/extract.rs:1087`, medium). `reserve_transient_cpu` charges
  only the process counter, so one pane holding retained images plus parked
  in-flight chunks can pin all 512 MiB indefinitely. Other panes then silently
  drop OSC titles and hyperlinks, and control strings over 64 KiB have their
  tail printed as grid text. Fixing it properly means separating "over the
  configured sequence limit" from "transient allocation failure" and giving
  in-flight bytes a per-terminal ceiling, which is a design change to the budget
  rather than a patch.

### Needs measurement or a live platform

- **`output_generation` is published after the terminal mutex is released**
  (`kettle-core/src/term.rs:3888`, low). A reader can pair a mutated grid with
  the pre-mutation generation. The refuter confirmed the ordering and rejected
  two of the auditor's claims about its consequences, including the proposed
  fix, so this needs its own analysis before anything moves.

- **Three Windows update paths** (`kettle-update/src/install.rs:1771`, `:652`,
  `:2821`, all low after review). A wedged lock holder blocks launch with no
  retry budget; a 250 ms sharing-violation window can abort the first launch
  after an update; nothing checks the packaged `kettle.exe`'s PE version against
  the signed release version. All three want a real Windows machine to confirm
  the timing, and the VM cannot currently build the workspace.

### Protocol conformance, kitty graphics

- **A frame transmitted without `z=` is stored with gap 0 and never displayed**
  (`kettle-vt/src/kitty.rs:684`, low), and **`a=f,r=1` appends a frame instead
  of editing the root frame** (`:729`, low). Both are real divergences from the
  kitty specification and neither has a consumer today. They belong with the
  other kitty graphics items already in this file rather than as one-off fixes.

### Bounded, but not yet done

- **`Terminal::drop` sends SIGHUP to a raw PID that `exit-action=hold` may
  already have reaped** (`kettle-core/src/term.rs:9027`, low to medium). The
  window is narrow and needs the PID to be recycled, but the fix is a `try_wait`
  before the `kill`.

### Smaller, individually cheap

Each is a few lines and none is urgent: the Windows named-pipe listener keeps a
closed handle in `pending` on one error path (`kettle-ctl/src/transport.rs:1224`);
one long-argv descendant suppresses the process snapshot for every pane on Linux
(`kettle-ctl/src/lib.rs:522`); `ssh -P tag` is neither reproduced nor marked
unreproducible, so Reconnect can open a shell on a different host (`:1804`); the
activation accept loop retries a failing `accept()` with no backoff
(`kettle-ctl/src/activation.rs:469`); an
overlapped write discards its transferred-byte count on cancellation
(`transport.rs:808`); an interrupted `install-unix.py` wedges both upgrade and
uninstall with no documented recovery (`install-unix.py:700`).

## Testing coverage

- **Ubuntu ARM: the suite runs, a live window does not, 2026-08-22.**

  The test suite passes in the `Ubuntu 26.04` guest: 769 tests across
  `kettle-core`, `kettle-vt`, `kettle-update` and `kettle-config`, zero
  failures, including the Linux-gated startup-lock test and a direct
  reproduction of the one-column crash on aarch64.

  Getting there needed two things that are worth writing down, because both
  wasted a pass. `prlctl exec` runs as root while the guest's `/` is owned by
  uid 1000, so `kettle-state`'s trusted-directory check refuses every
  private-file creation and ~36 tests fail on
  `private path crosses an untrusted directory edge`. That refusal is correct.
  Run the suite as the user who owns `/` instead. Their toolchain is the second
  problem: the only complete one lives under root's home, which on this VM *is*
  `/`, so it is not executable by that user. Widen exactly the two toolchain
  directories, `chmod -R a+rX /.cargo /.rustup`, and nothing else. Root's home
  being `/` is precisely why the paths have to be named: the same command
  without operands, or aimed at `/`, would make the whole filesystem
  world-readable.

  A live window is still not covered. The guest's `/tmp` is a 7.6 GB tmpfs,
  which is where the build has to go because `/` has 1.4 GB free, and the
  target directory fills it before the final link. rustc dies with exit 101 and
  no OOM in `dmesg`, because it is disk rather than memory. Building to the
  shared folder instead works but is slow enough that it was not worth another
  pass for this change.

  So GPU and window behaviour on Linux ARM went unverified this cycle. CI builds
  and tests Linux x86_64 on every pull request and runs an aarch64 early-warning
  job, so everything except live presentation is covered elsewhere.

  Worth flagging for whoever picks this up: `docs/INSTALL.md:15-17` and
  `docs/TESTING.md:180-182` both describe this guest as supplying live-UI and
  Wayland evidence. Neither is wrong about what it can do. It could not do it
  today, and the reason is disk rather than anything about the guest. Freeing
  space in `/tmp`, or linking the binary elsewhere, should restore it. Those
  documents are left alone because a full tmpfs is a passing condition, not a
  change in what the VM is for.

- **Two macOS update cleanup windows are mitigated, not eliminated,
  2026-08-22.** `Staging::discard` and the sweep both delete by pathname,
  because `std::fs::remove_dir_all` takes a path and there is no
  descriptor-taking equivalent in std. Each one checks `(dev, ino)`, renames the
  tree to a name that did not exist a moment earlier, re-checks, and only then
  removes. Someone who can write the enclosing directory and is watching it
  could still substitute in the gap between the second check and the removal.

  Fully closing this means a recursive `unlinkat` walk over an open descriptor,
  roughly fifty lines of `fdopendir`/`readdir` FFI with its own depth and entry
  caps. It is worth doing, and it is not worth doing in the same change that
  introduces the feature.

  Scope, so this is not read as bigger than it is: reaching the window needs
  write access to the bundle's parent, which for `/Applications` means an
  administrator account. The outcome is a deletion, not code execution. What
  gets installed is bound by the Ed25519-signed manifest's SHA-256 and by
  Apple's seal, and neither depends on these paths.

- **The macOS staging emptiness check has no automated coverage, 2026-08-22.**
  `Staging::create` requires the directory to be empty immediately after its ACL
  is cleared, because an inheritable ACE could otherwise let someone create a
  child during the window between `mkdirat` and the clear, and that child would
  keep its own grant. Reaching the branch means winning a microsecond race
  against the process under test, which a test cannot arrange. The check is
  cheap insurance and is documented as untested rather than covered by a test
  that would pass either way.

- **The macOS update sweep's foreign-owner branch has no automated coverage,
  2026-08-22.** `sweep_interrupted_updates` refuses to delete a leftover unless
  it is a directory owned by the effective uid. The ownership half is the part
  that matters: anyone who can write `/Applications` can rename someone else's
  tree to wear the `.kettle-update-previous-` prefix, and a recursive delete
  keyed on the name alone would then destroy it with rights the attacker does
  not have.

  Reaching that branch needs a file owned by a second uid, which a test running
  as one user cannot create. A first attempt covered the symlink case instead
  and was deleted rather than kept: `std::fs::remove_dir_all` already refuses to
  traverse a symlink, so the test passed with the guard removed. A test that
  cannot fail is worse than no test, because it reads like coverage.

  Closing this properly needs either a fixture that runs as a second uid, or
  making the sweep's decision a pure function over `(file_type, uid, euid)` and
  testing that directly. The second is cheap and is the likely fix.


- **Test scratch directories accumulated without bound, 2026-08-09.** A sweep
  of the Windows machine found **148** `kettle*` entries in `%TEMP%`; the Ubuntu
  machine had its own smaller set. Most came from one helper:
  `activation.rs::test_paths` built a path from the pid and deleted whatever a
  *previous* run with the same pid and label had left — never its own. Since the
  pid varies, every run leaked one directory, forever, on every machine that
  ever ran the suite.

  Fixed by switching it to `kettle_test_support::private_tempdir`, whose
  `TempDir` removes the directory when the guard drops. Verified by counting
  before and after a run: unchanged. The entries already on disk are historical
  and safe to delete by hand.

  Worth noting how it was found: no test failed, no gate fired, and reading the
  helper would have looked reasonable — it *does* call `remove_dir_all`. Only
  looking at a machine that had run the suite hundreds of times made it visible.


- **Eleven live-UI scenarios still run in no automated gate.**
  `scripts/check-live-ui-smoke.py` launches a real windowed kettle and drives it
  through twelve scenarios (`tabbar`, `tab-title`, `tearoff`, `split-titlebar`,
  `zoom-keybind`, `underline`, `agent-tui`, `search-history`, `interaction`,
  `hover-wheel`, `window-close-isolation`, `touchpad-scroll`). `search-history`
  is now the first automated scenario; the other eleven remain manual. Its
  other three `case` values do less:
  `all` runs the ten broad scenarios (the interaction walk already includes
  `hover-wheel`), `session-check` only asserts that a graphical session is
  usable and returns without launching kettle, and `self-test` exercises the
  helper's pure functions.

  This closes the narrower gap that mattered: CI previously launched a window
  under Xvfb but never spoke to a running kettle over ctl or asserted rendered
  state. The recorded GPU/interactive-desktop reason was wrong for Linux; CI
  already installs `xvfb` and `mesa-vulkan-drivers`.

  Getting there exposed two real product defects. The first run timed out on the
  control server because the `002`-umask directory bug disabled three features
  with only a log warning (#178/#180). The 2026-08-09 rerun reached ctl and wrote
  real screen/cell dumps, then failed because an explicitly named diagnostic PNG
  under the checkout was treated as private state and therefore required the
  user's whole `~/Repos` tree to be `0700`.

  **Successful rerun, 2026-08-10.** On the same Ubuntu 24.04 host,
  `search-history` ran under `xvfb-run` with
  `LIBGL_ALWAYS_SOFTWARE=1`, reached the control server, exercised bottom/old/
  middle/reverse/new search states, and exited zero. That initial run wrote five
  97–105 KiB PNGs plus screen, cell, geometry, dispatch, log, and analysis
  evidence. Every
  PNG was created `0600` beneath the ordinary diagnostics parent. The fix keeps
  implicit screenshots on the private-state path while giving an explicit ctl/
  MCP `path` a separate new-file-only, owner-only export policy.

  CI now runs the strengthened scenario on Linux and uploads its evidence. In
  addition to the control-plane checks above, it compares controlled captures:
  query/status pixels must change inside an unchanged search rectangle, then a
  focused match and a no-match state must retain the same row count and display
  offset while pixels change inside the exact active match-cell rectangles
  reported by `ui_geometry`. This replaced an initial broad-difference check
  that could be satisfied by viewport reflow rather than the intended paint. A
  Tower rerun measured 1,749 changed query/status pixels and 3,120 changed
  pixels inside the one reported match rectangle (633 required); all seven PNGs
  were `0600`. It is
  deliberately `continue-on-error` for one week while hosted-runner flake rate
  is measured; after that observation window it should become required if the
  evidence supports it, or record and fix the specific flake if not.

  Add cases one at a time rather than enabling `all`; whether each survives Xvfb
  has to be established, not assumed. Note that the pointer-driven tear/re-dock
  gesture is **not** in `all` at all — the Python `tearoff` case is deliberately
  mouseless (`perform_action("move_tab_to_new_window")`), and the real-pointer
  version lives in `scripts/check-tearoff-live-smoke.sh`, driven by `xdotool` and
  dispatched only by the Unix `tearoff-smoke` recipe. macOS and Windows have no
  such gate configured today; whether their runners could host one is untested
  rather than known to be impossible.

## Deferred from the 2026-08-09 umask work

- ~~**The config-reload watcher keeps a handle it may never have registered.**~~
  Fixed in the next release. The decision it was waiting on: a config directory
  that cannot be watched is not fatal — everything else works and reload is a
  convenience — so it warns and continues, but does not pretend the watcher
  exists. Both results are now checked, matching the remote-command block below
  it, and a source guard pins that neither `watch(` call is discarded.

  Original entry:

- **The config-reload watcher keeps a handle it may never have registered.**
  `app.rs` ignores the result of both the directory create and
  `w.watch(&dir, …)`, then stores `Some(w)` regardless. If the watch call fails,
  kettle holds a live watcher that is watching nothing and live config reload is
  silently off for the session — the same failure shape as the umask bug it sits
  next to, which is how it was noticed.

  Not fixed with that work because the fix is a decision, not a line: a config
  directory that cannot be watched is not obviously fatal to startup, so the
  choice is between failing the launch, retrying, and surfacing it in the UI
  rather than a log. The remote-command block below it already fails closed
  through the private-file helpers and logs, which is the shape to copy.

## Deferred from the 2026-08-07 full-repo audit

- **~~`handle_action`'s tail resizes unconditionally~~ — fixed.** Recorded when
  PR #168 shipped, because #168's commit message claimed the child "sees one
  resize to the final geometry, or none at all" and that was true only for the
  paths it changed. `handle_action`'s tail called `resize_all` after *every*
  action, so a Lua hook queueing `EditWindowTitle` then `CommandPalette` before
  a redraw still produced a shrink and a grow for a net-zero frame.

  Deferred at the time on a stated risk: that `save_session`, which runs
  immediately after the tail, might depend on the resize having happened. That
  risk turned out to be unfounded when it was finally checked — `SGeometry` is
  the OS window rect from winit and `STab` is the split tree; neither records
  the PTY grid. The tail now marks the frame dirty instead, and a windowless
  path still resizes eagerly because a deferred resize that never flushes is
  worse than an eager one that fires twice.

  Worth keeping the entry rather than deleting it: the deferral was justified by
  an assumption nobody had verified, and presented as a measured trade-off. That
  is a different and less defensible thing than deferring on measured evidence,
  which is what the other items here rest on.

- **~~The Linux installer smoke's online leg can fail on a transient network
  blip, and it gates every pull request.~~ — fixed.** Observed on PR #162, run
  31255512657:

      kettle install-online.sh: v2.53.0 must ship a bounded Ed25519-signed
      manifest, but .../v2.53.0/kettle-update-manifest.json[.sig] could not be
      fetched. ... Refusing to downgrade to the weaker same-origin checksum.

  The release was **not** broken: all 14 assets are published, and both
  `kettle-update-manifest.json` and its `.sig` return HTTP 200 on direct fetch.
  The manifest simply could not be retrieved during that run.

  Refusing to continue is the correct response — a missing manifest is
  indistinguishable from suppression, and falling back to the same-origin
  checksum would be exactly the downgrade an attacker wants. So the fix is not
  to soften the check. It is to make the *fetch* resilient: a bounded retry with
  backoff, distinguishing a transport failure from a manifest that is genuinely
  absent or oversized, and failing closed only after the retries are exhausted.

  Every bounded fetch now gives curl two retries with its exponential backoff,
  including connection-refused failures. Retry admission closes after 30
  seconds and every started attempt retains the existing 600-second limit, so a
  server-provided `Retry-After` cannot choose an unbounded wait. curl's
  classifier is deliberately retained: timeouts, 408/429 and selected 5xx
  responses are transient; a 404 or `--max-filesize` refusal is permanent and
  gets no second attempt. `-q` is the first argument so user curl configuration
  cannot widen that classifier. Hermetic local-TLS cases drive the installed
  curl through manifest recovery, exhaustion after exactly three total
  attempts, a refused 60-second `Retry-After`, and the single-attempt
  permanent/unknown-length-oversize paths. The oversize case removes curl's
  userspace limit and requires `SIGXFSZ`, so it proves the kernel guard rather
  than a generic failure. Authentication and archive limits are evaluated
  exactly as before after a successful transfer.


- **Two `production_source` forms that leave test-only text in the slice.**
  Neither occurs in this workspace; both are recorded so the contract's limits
  are written down rather than discovered later.

  1. `#[cfg_attr(not(test), cfg(test))]` — an item that exists only in test
     builds, expressed through `cfg_attr`. All `cfg_attr` attributes are ignored,
     so the item survives. This predates the shared helper and is not a
     regression.
  2. `/** needle */ #[cfg(test)] fn f() {}` written on a single line. Doc
     backtracking requires the attribute to start its line, so the doc text
     survives the item's removal. `rustfmt` normalises this form, which is why
     the workspace does not contain it.

  Both leave *test* text behind, which can only make a positive guard pass
  spuriously. Neither deletes production text, which is the failure that makes a
  negative guard pass while protecting nothing — that direction is what the
  helper's tests are weighted toward, and it is why these two are deferred rather
  than fixed under release pressure.


- **~~The `kettle/tests/exec.rs` PTY suite is flaky on macOS — the whole suite, not one test~~ — fixed.**
  The test's own comment already records that it "has failed intermittently on
  macOS CI with empty stdout"; this entry adds the missing part, which is a
  measured rate and a decisive answer to whether the v2.54.0 change set caused
  it. `crates/kettle/tests/exec.rs:346` — the `out.contains("agent-marker-7f3")`
  assertion — fails with empty stdout.

  Measured on an Apple-silicon macOS 15 host, `main` at `f67cce6` versus this
  release branch, same machine, back to back:

  | | full 26-test binary | test alone |
  |---|---|---|
  | `main` (f67cce6) | 1 / 25 | 0 / 30 |
  | `integration/v2.54.0` | 2 / 25 | 1 / 30 |

  Three conclusions, all of which needed the numbers rather than a guess:

  1. **It is pre-existing.** 1/55 on `main` against 3/55 on the branch is not a
     significant difference at these sample sizes. The branch did not introduce
     it — and `git log main..HEAD -- crates/kettle/tests/exec.rs` is empty, so
     the test itself is untouched.
  2. **It is not caused by this release's ANSI-stripper change**, which was the
     obvious suspicion because the test runs `--strip-ansi` and
     `crates/kettle/src/exec.rs` was edited here. `main` predates that change and
     still flakes.
  3. **It is not purely a concurrency artifact.** It reproduces with the test run
     alone, so contention between the binary's 26 PTY tests is not required —
     which rules out the cheapest possible fix.

  The one captured diagnostic instance carried "asciicast capture stopped
  (recording I/O failed or finalization exceeded its bound)" immediately before
  the empty read, which points at recording finalization racing the child's
  final flush rather than at the stripper. Not chased further here because it is
  pre-existing and unrelated to this release's scope; it wants its own change
  with its own measurement, and the rate above is the baseline to beat.

  **Corrected 2026-08-08.** The entry above named a single test. That was too
  narrow. Three different tests in this file have now been observed failing on
  `main`, at 2/30 per full-binary run:

  | test | where seen |
  |---|---|
  | `exec_streams_stdout_and_exits_zero` | the measurement above, and a Codex gauntlet run |
  | `exec_record_writes_replayable_asciicast` | CI on PR #164, and `main` locally |
  | `exec_raw_mode_eof_is_explicit_and_does_not_destroy_terminal_replies` | `main` locally |

  So the flake is a property of the PTY harness these tests share, not of any one
  assertion — which also means "re-run once and investigate if it repeats" was
  the wrong stopping rule: a repeat shows up as a *different* test name and reads
  like an unrelated failure. Any pull request can go red at random on the macOS
  leg, and the correct response is to check whether the failing test is in this
  file before assuming a regression.

  The shared suspect remains recording finalization racing the child's final
  flush; all three tests drive a PTY child to completion and read what it wrote.
  Still deferred, but it should be fixed at the harness rather than per test.

  **It reproduces locally — under `cargo test --workspace`, 2026-08-09.**
  An earlier version of this paragraph said it did not, on the strength of
  thirty runs of the exec binary and forty of one test under CPU load. That
  scoping was wrong and the conclusion it invited — "do not try to reproduce
  this on a developer machine" — would have sent the next investigator away
  from the one recipe that works.

  Running the **whole workspace** reproduces it at roughly **1 in 14** on an
  Apple-silicon macOS host: forty-five `cargo test --workspace` runs, one
  failure. The failing test that time was
  `exec_raw_mode_eof_is_explicit_and_does_not_destroy_terminal_replies` —
  the third name in the table above, and the same family of symptom
  (`missing explicit raw-mode EOF diagnostic: ""`, an empty read where output
  was expected).

  So the missing condition is not CPU load, it is **whole-workspace
  concurrency**: many test binaries and their PTY children running at once.
  That is also what CI does, which is why CI sees it and an exec-only loop
  does not. Reproduce with:

      for i in $(seq 1 30); do cargo test --workspace --no-fail-fast; done

  and expect roughly two failures. That is a tractable loop for whoever
  attacks the harness fix, and it is the thing this entry was missing.

  So the test now diagnoses itself. On failure it re-runs the same command
  **without** `--strip-ansi` and reports both, which answers the one question
  the symptom cannot: an empty stripped result is equally consistent with the
  child producing nothing and with the read or strip path losing it. If the raw
  retry contains the marker, the loss is on this side; if it does not, look at
  the child or the PTY read. At roughly one pull request in three, the next
  occurrence should not be long, and it will arrive with evidence attached
  rather than a symptom every candidate cause shares.

  **Rate revised upward, 2026-08-09.** The 2/30 figure above came from repeated
  runs of one binary on one host. Counting the macOS leg of this cycle's pull
  requests instead gives a worse picture: **4 failures in 13 CI runs** — three
  `exec_streams_stdout_and_exits_zero` (PR #176 once, PR #180 twice) and one
  `exec_record_writes_replayable_asciicast` (PR #181), every one green on an
  unmodified re-run. The fourth landed while this entry was being written and
  is the corrected framing arriving on schedule: a repeat showed up as a
  different test name, which is precisely what makes "re-run once and
  investigate if it repeats" the wrong stopping rule. ~31% per pull request is
  not "occasionally"; it means roughly
  one PR in three goes red on macOS for no reason, and every one of those costs a
  human the judgement call this entry exists to answer. Treat the harness fix as
  higher priority than the 2/30 framing implied, and record the run count when
  updating this figure — a rate measured by re-running one binary and a rate
  measured across CI runs are not the same number.

  **Resolved 2026-08-10.** The recorder was downstream evidence, not the
  cause. `drain_output_slice` latched real EOF only when the raw channel
  disconnected, but the lifecycle then declared the PTY finished after
  `SETTLE + PTY_DRAIN_GRACE` (810 ms) on every platform. That contradicted the
  constant's own comment: the elapsed fallback exists for Windows ConPTY,
  whose output handle can outlive its final repaint. On Unix, a live sender at
  810 ms is still a reader that may deliver bytes. Whole-workspace scheduling
  made that interval reachable, so both stdout and the recorder were finalized
  before the shared bytes arrived.

  The policies are now separate. ConPTY retains a bounded *quiet* interval that
  restarts as soon as the pump reads a chunk and remains closed while that chunk
  is pending in the parser/output pipeline. Quiet now starts an asynchronous
  pseudoconsole close while the reader remains live; it never finishes the
  command. Windows and Unix both report success only after the raw channel
  disconnects and the core reader identifies an orderly EOF; an
  unexpected read error or five seconds without EOF becomes an explicit
  status-125 incomplete-output failure instead of either a hang or false
  success. The bound is applied only when the raw transport is idle, so queued
  bytes and a stdout command already blocked in its OS-facing worker are still
  allowed to drain. A complete
  operation deadline returns 124 after verified teardown rather than laundering
  abandoned bytes through a direct child's collected exit 0 (unverified
  teardown returns 125). Reader status, generation, and pending
  work share one atomic snapshot, so a pump read and parser completion cannot
  manufacture a state that was never real. On Linux, teardown now uses the
  PTY-created session as the ownership boundary and identity-stable pidfds for
  signals. That reaches reparented and descriptor-free session members without
  treating an unrelated process that opened the PTY slave as Kettle's child;
  the bounded procfs scan acknowledges SIGSTOP before declaring its final scan
  stable and reports degraded containment if procfs or pidfds are unavailable.
  Windows containment moved into the PTY backend: the command is created
  suspended, assigned to a kill-on-close Job Object, and resumed only after the
  assignment succeeds, so an immediate descendant cannot escape between spawn
  and attachment. Job accounting also prevents a quiet pseudoconsole close
  while a live descendant can still emit, and handle signalling preserves the
  otherwise ambiguous valid exit code 259.

  Native platform-selection, close/EOF, reader-error, deadline, active-fork,
  Linux reparenting, late-descendant output, exit-259, and real ConPTY Job Object tests fail against the
  respective former behaviors. The macOS exec integration suite and focused
  native Linux and Windows checks pass. The exact
  whole-workspace reproduction loop above completed 30/30 on the pre-fix tree
  during this investigation; that null run is retained honestly rather than
  presented as proof of the fix.

  **Reopened and fixed at the construction boundary, 2026-08-10.** PR #200's
  macOS job reproduced `exec_streams_stdout_and_exits_zero` after the lifecycle
  fix: both the normal invocation and the raw diagnostic retry exited 0 with
  empty stdout. The lifecycle was waiting for a truthful EOF, but construction
  still spawned the child before cloning the master reader and starting its two
  threads. A negative-control build inserted a two-second pause into the former
  post-spawn setup window and reproduced the exact CI result twice in one test:
  exit 0 with empty output, then the same result from the raw retry.

  The first fix prepared the reader and waited for an explicit pump-ready signal
  before `spawn_command`, but adversarial review showed that this only moved the
  race: the pump could be descheduled after sending readiness and before its
  first `read`. A two-second pause in that smaller window reproduced the same
  double-empty failure. The corrected Unix path keeps Kettle's slave descriptor
  alive in the pump until its first bytes are read; if the direct child exits
  silently, Linux waits on master readability plus a pidfd and macOS uses one
  kqueue for master readability plus `NOTE_EXIT`; the portable fallback uses an
  exponentially backed-off poll plus `waitid(WNOWAIT)`. Each releases the
  descriptor so EOF remains observable without stealing the child's wait
  status, without leaving a quiet long-running pane on a periodic wakeup. A
  simultaneous macOS output/exit edge keeps the descriptor through the actual
  read; dropping it at notification time discarded the final bytes. The
  observer remains active after startup and enforces a five-second drain when a
  `setsid()` descendant retains the slave; a real Linux fixture proves the
  reader still emits its ordered exit marker. The UI waits for that marker
  before reaping the pane, so process status cannot win ahead of final output or
  `exit-action`; Hold continues bounded status polling if EOF beats the direct
  child's wait status. Windows interactive panes independently wait on a
  duplicated process handle, publish a lifecycle wake that bypasses paint
  generation, begin ConPTY close at the first bound, and start the second bound
  only after close really starts. A close-worker spawn failure is retried rather
  than retaining a live master under Hold. The same
  post-readiness pause now passes the real integration test because the delayed
  reader still receives the retained output. The ordinary fixed tree completed
  30 whole-workspace runs with no failure, and native Linux, Windows ARM, and
  macOS exec suites cover the platform paths. Tracked as #201.

- **A Unix child can deliberately escape timeout teardown by creating a new
  session.** Kettle owns the PTY-created session and can safely kill every
  member of it, including reparented processes and new process groups. Once a
  descendant successfully calls `setsid`, neither ancestry after reparenting
  nor possession of an open PTY descriptor proves ownership: an unrelated
  same-user process can independently open or receive that descriptor. Killing
  by either signal would make an unrelated process a teardown target. A complete
  guarantee needs an OS-owned containment primitive (a delegated Linux cgroup
  or a supervisor/subreaper designed into process creation), not a procfs
  heuristic; that is a separate architecture change.

- **~~`kettle ctl screenshot` times out on an occluded macOS window~~ — fixed.**
  The earlier non-bundled-window hypothesis was wrong. A direct reproduction
  put Finder in front of a source-built Kettle window: CoreGraphics reported a
  real layer-zero, on-screen Kettle window, but its client surface remained
  blank and the control screenshot timed out. The request had been queued
  correctly; `redraw()` then returned at the general occluded-window power
  guard. The first attempted fix bypassed that guard and was rejected in review:
  wgpu's Metal backend intentionally returns `SurfaceError::Occluded` before
  `nextDrawable`, so no swapchain-based readback can work in this state.

  The capture now renders the same prepared scene into a process-budgeted
  transient target before surface acquisition and reads that texture back. The
  target, staging buffer, and their separate reservations stay alive through
  submission completion or device loss; a finite GPU-wait timeout retains the
  job instead of undercounting it. Thus 6K/8K captures retain the documented
  256 MiB per-allocation bound without weakening the 64 MiB retained-image cap
  or hiding in-flight work. Ordinary occluded output
  stays quiescent, while a shown, visible, non-minimized target may run this one
  explicit offscreen frame through compositor occlusion or transient surface
  recovery. Renderer rebuilds remain blocking. Targets the backend reports as
  hidden/minimized fail before queueing; Wayland cannot report either state and
  therefore retains the bounded timeout. Encoding and staged-inode sync remain
  cancellable; file commit occurs immediately before atomic no-replace
  publication. A finite post-commit grace reports an uncertain destination
  instead of waiting forever, and a wedged GPU worker wakes the event loop into
  recovery, preventing a late write or a stranded `BUSY` request. Native
  verification put Finder in front and completed two consecutive control
  screenshots (816x520 RGBA, non-empty and
  byte-identical). After repairing the separate LazyVCS harness defects below,
  the broader native agent smoke completed all 14 live states, including the
  configured sidebar, changed-row gutter, and fixture-row inline blame.

  The smoke harness now records the exact executable path, SHA-256, version
  output, source commit/dirty state (including untracked regular files), exact
  target Neovim bytes, copied LazyVCS tree hash, canonical loaded module source,
  and harness SHA-256. The basic identity is captured before launch; the bounded,
  no-follow plugin baseline is captured only after configured Neovim finishes
  any first-run bootstrap, then all of it is verified again afterward. This
  retains the files that were actually tested without misclassifying a
  legitimate absent-to-installed bootstrap as a product mutation. Repository
  NUL-delimited porcelain status, diffs, and untracked contents are streamed
  under pathname/record and file/byte budgets, and the complete filesystem pass
  runs in a contained child under one absolute
  parent-enforced launch-and-run deadline. Unix uses a private process group for
  the worker and ordinary descendants and disables configured Git fsmonitor
  processes; a deliberately detached `setsid` descendant is outside POSIX
  process-group containment, so this is cleanup rather than a sandbox.
  Windows uses a kill-on-close Job Object assigned before a handshake lets the
  worker launch Git. A timeout transfers reaping to a background owner rather
  than waiting past the deadline; tests require a successful reap rather than
  only reaper-thread exit, exercise Job close without explicit termination on
  Windows after both tree PIDs are atomically published, kill completed Unix
  worker groups, and route unexpected pipe failures through the same cleanup.
  Ignored trees are pruned and untracked links or special files fail closed
  rather than disappearing from the identity. Its configured-LazyVCS leg also
  fixes three harness defects in the marker itself: Vimscript concatenation
  embedded in Lua, a write to a non-modifiable plugin buffer, and a
  configured-plugin warning whose hit-enter pager covered the otherwise
  successful sidebar. The stable marker gate checks the exact fixture path in
  active and discovered state plus a per-run unique sidebar row. Terminal-grid
  evidence is divided at one cell-proven split column: sidebar tokens must stay
  on the left and the unique gutter/blame rows on the tracked-file side, and
  that exact cell snapshot is retained, so unrelated or independently sampled
  text cannot satisfy the probe. It no longer depends on LazyVCS's private
  caches or extmark namespace names. Native failure cleanup likewise retains
  Linux pidfds or macOS audit tokens for every session member and
  exact-environment daemon; it never carries a reusable numeric PID from a
  check to a signal. Environment matching reads the actual NUL-delimited values
  rather than argv-rendering text. Every acquired handle becomes
  finalizer-owned before any duplicate close or first stop, and individual
  signal/close failures cannot skip later handles. The wrapper preserves both
  normal exit codes and terminating signals while remaining the session anchor
  only for a live background descendant. A reused append-only tracker PID is
  reopened and independently classified in the same pass, and an internal
  identity-query error closes every handle acquired earlier in that scan. On
  native Windows, the PowerShell pane assigns its own handle to an unpredictable
  named Job before configured Neovim starts; cleanup waits for zero active Job
  processes before deletion rather than treating asynchronous termination as a
  completed drain.

- **One shared streaming control-state kernel for the three ANSI parsers.** The
  `kettle exec` stripper, the session-log scrubber, and the VT extractor have now
  drifted from each other **three separate times**, and the 2026-08-07 audit found
  holes in all three simultaneously — each in a *different* state, each already
  fixed in one parser and not the others. The 2026-08-07 pass fixed the holes but
  deliberately did not unify them: two of the parsers were being repaired
  concurrently and a unifying refactor would have collided with that work.

  A shared kernel must cover ground/pass, ESC, escape intermediates, CSI,
  OSC, DCS/SOS/PM/APC, 7-bit and C1 introducers and terminators, OSC BEL, raw ST,
  CAN/SUB, ESC-from-anywhere redispatch, split ESC/ST/UTF-8 input across feed
  boundaries, bounded recovery, and explicit UTF-8 lead ownership. Policy hooks
  decide forwarding versus suppression, protocol extraction, size budgets, and
  session resets, so the three consumers share state semantics without sharing
  output behavior.

  Until this lands, treat any fix to one of the three as incomplete until the
  same input has been checked against the other two. The
  `ansi_stripper_control_events_cover_every_state_cross_product` test added in
  this pass is the shape the shared kernel's conformance matrix should take.

- **`scroll_page_up` intermittently failed to enter scrollback on macOS.** Three
  comparator runs recorded `display_offset == 0` after seeding 1,600 lines.
  Later 2026-08-10 reruns moved the viewport and reported a positive offset, but
  no source change explains the difference and the historical analyses did not
  retain build hashes. Passing reruns therefore do not close a load- or
  state-sensitive defect. Keep this open until either a root cause is fixed or
  a bounded stress run against identified commits/artifacts establishes its
  actual failure rate. The next failure must retain executable and harness
  provenance, `read_screen` before/after payloads, visible PNGs, and system load
  so viewport movement can be distinguished from stale control metadata.

## Search follow-up and platform evidence

The two-track search audit is recorded in
[AUDIT-2026-07-22-SEARCH.md](AUDIT-2026-07-22-SEARCH.md): track A covers every
search-owning file/crate, public contract, cap, and complexity boundary; track B
covers the 88-frame report, Xvfb/XTest reproduction, Terminator comparison, and
live UI states. The implementation addresses the reproduced signed-history and
soft-wrap failures, but the following evidence cannot be inferred from Ubuntu
unit tests and remains explicit release-environment work until recorded there:

- native Windows 11/ConPTY keyboard, IME, DPI, accessibility, and live renderer
  exercise (plus Windows 11 WSL shell/TUI flow);
- macOS native modifier, IME, accessibility, and Metal live-window exercise;
- installed-release desktop-launch verification after the signed release is
  available, including the Ubuntu Super-key launcher and the user's recording
  configuration;
- deeper authenticated Codex CLI / Claude Code CLI and configured AstroNvim
  sessions beyond deterministic fixtures, where installed credentials and
  configuration are external prerequisites.

Moving retained-history traversal to a worker is deliberately deferred pending
profiling. The current event-loop design advances through nominal 1000-line
ranges after a 500 ms idle deadline but performs only one bounded core work
slice per turn, preserves cursor progress under continuous output, and uses a
quiet-period verification only for non-navigation work. Output-interrupted
explicit navigation stays Results limited until the user retries. Query/reflow
changes invalidate work by scan token. One engine
call and one aggregate core slice are each capped at 64 KiB text; the aggregate
slice additionally yields after 262,144 inspected cells or 256 complete logical
haystacks. One logical haystack is capped at 256 rows, 64 KiB, and 262,144
cells, and one projection at 65,536 spans. Any threaded alternative must
snapshot safely without holding the terminal lock across UI work or applying
results after output, resize, or query changes.

These are sourced from the two audit run outputs and the synthesis plans; pick
them up in priority order (robustness/correctness before perf before polish).

## v2.39.0 full-repository audit (2026-07-23)

The whole-repository audit recorded in
[AUDIT-2026-07-23-FULL.md](AUDIT-2026-07-23-FULL.md) confirmed 59 findings and
shipped 52. The seven below were deliberately deferred as multi-session refactors
or cross-crate plumbing that should not be rushed into one release:

- **`app.rs` god-file split.** Extract along the seams the
  `Action` enum already implies — `action_dispatch`, drag/menu modules reusing
  the `detach.rs` pattern, `session_glue` (ctl/MCP/recorder wiring), and
  `window_lifecycle` — keeping `App` as a struct plus small `impl App` blocks
  split across files. Best paired with making per-event handlers return a typed
  `Outcome` list (pure deciders + thin applier) so the source-text drift guards
  become behavioural unit tests. Supersedes the earlier `app.rs` entry above.
- **`kettle-render/src/lib.rs` module split.** The remaining file still
  interleaves the `impl Renderer` frame pipeline with overlay/menu data types,
  GPU adapter selection, the screenshot capture pipeline, and text-fit geometry
  helpers, even though eight sibling submodules were already carved out. Extract
  in isolation order: `gpu.rs` (adapter selection, no `Renderer` state) →
  `screenshot.rs` → `overlays.rs` (data structs) → `text_fit.rs` (pure helpers).
- ~~**OSC 52 selection target (`p`/`s` vs `c`).**~~ Done in the next release;
  see the terminal/protocol entry above.
- ~~**OSC 133 prompt marks desync once scrollback wraps.**~~ Done in the next
  release: the vendored grid maintains a monotonic `history_origin`; prompt
  marks store stable document-row ids, prune on genuine eviction/reset, ignore
  alternate-screen rows, and clear on reflow rather than targeting unrelated
  text.
- **Command palette / layout picker / SSH launcher stay single-line bars.** Fold
  them into the responsive, multi-row layout the search bar gained in v2.38.0
  (also makes room for per-row keybind hints). Extends the vertical-list-pickers
  entry above.

Two related Kitty findings from the same audit are also resolved in the next
release. Global/frame deletion now crosses the extractor/core boundary and
updates physical, virtual, relative, animation, and stored-image state using
the full selector set; same-read delete/replacement order is explicit.
Physical and relative placements now retain and re-resolve source crop,
destination cells, pixel offsets, aspect ratio, and cursor-movement intent.
These fixes do **not** close the acknowledgement/query, existing-id
retransmission lifecycle, or exact `(image id, placement id)` `Q=` parent gaps
listed above.

## Deferred from the #187 review round

- ~~**Config load applies no directory trust check.**~~ Fixed in the next
  release. Implicit default/profile reads hold a verified parent chain through
  the regular-file open and check leaf ownership, mutation rights and link
  count on Unix and Windows; symlinked configs validate both locations. The
  watcher uses the same read-only verifier after best-effort repair, while an
  explicit `--config FILE` is represented as a deliberate trust grant.
- ~~**Activation test servers cannot be stopped.**~~ Fixed. Test servers now
  own a stop/wake/join guard, so the listener and non-delete-shared Windows
  election lock close before the scratch directory guard drops. The stale
  sweep remains only for process aborts and historical leftovers.
- ~~**The watcher source guard is textual and remains bypassable.**~~ Fixed.
  Registration is a generic candidate-plus-closure helper whose behavioral
  test proves success retains the candidate and failure drops it; the brittle
  source parser was removed.
- ~~**The cache-resolver ownership test can self-skip.**~~ Fixed. The real
  resolver runs in re-executed children with conflicting variables cleared for
  each XDG/HOME/LOCALAPPDATA branch, and kettle-state's base-list helper is also
  exercised with controlled absolute values rather than ambient environment.
