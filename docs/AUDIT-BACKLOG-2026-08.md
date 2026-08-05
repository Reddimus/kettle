# Full-repo audit backlog — 2026-08-03

Four independent adversarial audits at max reasoning effort covered the eight
crates outside the Terminator-parity surface: `kettle-core` + `kettle-vt`,
`kettle-render`, `kettle-ctl` + `kettle-state` + `kettle-remote`, and `kettle` +
`kettle-update`. Together they returned **91 findings**, and a follow-up review of the fixes
returned 8 more.

Seven were fixed immediately — the ones that corrupt output, hang a pane, break a
documented guarantee, or defeat a safety promise — and a review of those fixes
then found four of them incomplete, which is recorded below rather than
smoothed over. Everything else is listed here rather than silently dropped.

"Fixed" below means the stated trigger no longer reproduces and a test covers
it. Where a fix is partial, it says so and names what is left.

A finding being listed here is not a claim that it is minor. It is a claim that
it was **verified as real and consciously deferred**, usually because the fix is
a rewrite that needs its own change and its own measurement.

## Fixed in this pass

| Area | Defect |
|---|---|
| `kettle/exec.rs` | ANSI stripping ate UTF-8 continuation bytes — the six C1 controls it honored (`0x90`, `0x98`, `0x9b`, `0x9d`, `0x9e`, `0x9f`) all sit in the `0x80..=0xbf` continuation range, so `Û`, `‘`, and `▐` were corrupted. MCP `kettle_run` strips by default, so this was the output agents read. |
| `kettle/exec.rs` | Windows exit codes saturated at `i32::MAX`, destroying `STATUS_ACCESS_VIOLATION` and every other NTSTATUS. A test pinned the wrong behaviour. |
| `kettle/main.rs` | `--write-default-config` had a symlink TOCTOU: `exists()` follows links and the answer is stale by the time of the write. |
| `kettle-render/color.rs` | `minimum-contrast` chose its endpoint by thresholding luminance at 0.5, but the WCAG crossover is ~0.1791. Mid-tone backgrounds got the *worse* endpoint and the guarantee silently failed. |
| `kettle-render/{quad,imgpipe}.rs` | Shaders returned premultiplied color while the blend state was straight-alpha, so alpha was applied twice — every translucent surface rendered at roughly α². |
| `kettle-vt/extract.rs` | CAN/SUB did not cancel a control string, so one stray `0x18` inside an OSC swallowed the rest of the stream and the pane appeared frozen. |
| `kettle-update/install.rs` | The Linux updater replaced provenance-covered files without regenerating `install-files.json`, so the record held the OLD hashes for the NEW files and the next verification reported the installation **unmanaged** — stranding it permanently after one official update. It also never installed `install-unix.py`, which verification requires. The `cfg(test)` duplicate `apply_staged_update` carried the complete correct logic, which is why every test stayed green. Verified on Linux. |

### Residual, on fixes above

A review of the fixes found several of them **partial**, and they were then
completed in the same branch. Recording the shape here because it is the
recurring failure mode: a fix that closes the case you tested and leaves the
neighbouring one open.

- ANSI stripping tracked UTF-8 only in ground state, so `0x9c` inside an OSC
  still read as ST — `ESC ] 0 ; ✳ title BEL` leaked its tail. Now tracked in
  every state where text can appear.
- A malformed lead byte blindly shielded the next N bytes, hiding real C1
  controls. A byte that is not `0x80..=0xbf` now ends the shield at once.
- CAN/SUB cancellation was bypassed when `st_pending` was set, so `ESC CAN`
  reopened the freeze with one byte of disguise.
- `cancel_seq` used `Vec::clear`, retaining up to the 16 MiB capacity while
  releasing the budget reservation that accounted for it.
- Choosing the contrast endpoint by maximum ratio reversed near-compliant
  foregrounds: `#fdfdfd` on `#767676` is 4.465:1, and both ends clear 4.5, so
  maximizing flipped near-white text to near-black. It now prefers the endpoint
  the foreground is already nearest and crosses over only when that side cannot
  reach the target.
- `--write-default-config` returned an error for an existing DIRECTORY, because
  Windows reports that as a permission failure rather than `AlreadyExists`.

A second review, of that second round, found four more. Those are now fixed too,
and the pattern is the same one again:

- Regenerating the Linux provenance record from the archive alone **disowned**
  files the previous release installed and this one no longer ships. They stayed
  on disk with nothing recording them, and uninstall deletes only what
  provenance lists. `scripts/install-unix.py` seeds the new record from the old
  one; both writers now share one function, so the drift that hid the original
  bug cannot recur.
- Directory ownership was sampled BEFORE the writes, so a transaction that
  created a directory and rolled back left it unowned — the retry saw it as
  pre-existing and never claimed it. The transaction reports what it created and
  removes those directories on rollback.
- The `exec` stripper's UTF-8 shield asked the CURRENT state whether to emit a
  continuation byte. The forced 64-KiB resynchronization can land on a lead
  byte, swallowing it and moving to ground — so the continuations were emitted
  with no lead in front of them and stdout became invalid UTF-8 from there on. A
  character now follows its lead.
- `--check-config` deduplicated inert keys on the spelling in the file, so
  `use-system-font` and `use_system_font` — one key to the parser — were
  reported as two separate inert settings.

A third review, of the fixes to the fixes, found nine more — and the pattern
held a third time. The instructive ones:

- The reported-cwd guard refused a path leading with TWO separators. The NT
  object-manager prefix has ONE, and `\??\UNC\host\share` reaches the same
  redirector; `\??\GLOBALROOT\Device\...` reaches the rest of the NT namespace.
  Measured: 380 ms against a share, 0.2 ms against a local path — that gap is
  the network round-trip and the credential handshake. Enumerating dangerous
  prefixes cannot work, so it is an allowlist now. The denylist had also been
  refusing the WSL plan-9 shares, which is exactly what Microsoft's documented
  OSC 9;9 WSL integration emits.
- The `exec` stripper fixed ONE of three bounded resynchronizations. `Csi` and
  `EscapeIntermediate` have the same 64-KiB bound and the same hole; `ESC`
  followed by a lead byte reaches it with no 64 KiB at all.
- The MCP shutdown budget discarded the result of any tool call outliving 30
  seconds, while printing a diagnostic blaming stdout. A wall clock cannot tell
  a stalled peer from busy work; the writer reports whether it is parked inside
  a write now. The hang was also still reachable below 28 queued responses,
  where nothing ever latched — and the test's 52-message shape could not see it.
- Carrying provenance records forward silently defanged the test written to
  catch the original provenance bug: dropping a file from the applier's map
  passed 43 of 43, because the carried record still matched the untouched file
  on disk. The test compares archive bytes to disk bytes now.
- The marker-version check refused `unknown`, which both installers write, and
  so reported those installations as unmanaged.

A fourth pass closed the first of the four items below and turned the middle
two into recorded decisions.

- ~~The premultiplied blend fix is correct locally, but `lib.rs`'s surface
  clear still writes straight RGB under
  `CompositeAlphaMode::PreMultiplied`.~~ **FIXED.** `surface_clear_color` owns
  the multiply, in linear space, and only for `PreMultiplied` — `Opaque`
  discards alpha at composite time and `PostMultiplied` divides it back out.
  Chasing it found the same chain running into both capture paths, which is
  the worse half: PNG stores straight alpha and neither the offscreen
  `--screenshot` nor the live-surface `ctl screenshot` converted, so every
  translucent capture was wrong in both directions at once. The GPU fixture
  renders the scene twice, once per clear convention, and asserts they
  disagree before asserting which is right — a translucent clear with nothing
  drawn over it reads back identically either way, so a clear-only fixture
  could not have failed.

Still open on those same fixes:

- Linux provenance now records the uid that PUBLISHED the files
  (`geteuid()`, matching `install-unix.py`) rather than the prefix owner. The
  two agree in every reachable case, so no test can distinguish them without
  root and an ACL-writable root-owned prefix; the assertion pins intent only.

### Open — the AltGr substitute types the wrong character

Pre-existing, and deliberately left alone by the `Ctrl+Alt` fix above rather
than blind-fixed on a layout this machine cannot produce.

winit neutralizes AltGr only for the RIGHT Alt
(`has_alt_graph && key_pressed(VK_RMENU)`), but Windows also documents
left-Ctrl + left-Alt as an AltGr substitute, and that arrives as plain
`CONTROL|ALT`. On a German layout that chord is how `@` is typed. The
`Key::Character` arm then emits `s`, the *logical* key, so kettle sends `ESC q`
instead of the `@` the platform committed.

The `Ctrl+Alt` fix is careful not to make this worse — a press that committed a
printable character no longer claims the C0 table, so `@` is not turned into
DC1/XON — but the character kettle finally emits is still the logical key. The
real fix is for the `Character` arm to prefer `text` over the logical key when
the platform committed a printable character, which is the same correction the
uncombined-dead-key finding needs. It wants a real AltGr layout to test
against; there is none here.

### Verified, then consciously not fixed

Both of these were traced to a working fix and then rejected on cost, which is
a review outcome and is recorded so the next pass does not rebuild them.

- **`--write-default-config` follows a parent junction.** Only the final
  component is atomic: `create_new` refuses to follow a symlink at the leaf,
  but every ancestor is still traversed by name, so an attacker who can replace
  a writable parent directory redirects the creation. `--config PATH` accepts
  an arbitrary path, so the parent is not always one kettle owns.

  The machinery to close it already exists — `kettle-state`'s
  `create_private_file_new` holds every ancestor open with
  `FILE_FLAG_OPEN_REPARSE_POINT` / `O_NOFOLLOW`, creates relative to that
  anchored parent, and re-verifies afterwards. Routing the config write through
  it would also apply `require_trusted_directory_security`, which **refuses a
  broadly writable parent** — and that would reject legitimate uses like a
  config on a shared volume or in a synced folder, turning a working command
  into a failing one. It would also make the config owner-only, a visible
  permissions change to a file that is not a secret.

  Weighed against what the primitive actually buys an attacker: creation only
  (never clobbering, because `O_EXCL`/`CREATE_NEW` holds regardless of the
  parent), of a fixed, shipped, fully commented-out template that contains
  nothing private. That is an arbitrary-file-create with benign content at the
  invoking user's privilege. The regression risk is larger than the finding.

- **A directory created by a killed update stays behind unowned.** Rollback
  removes the directories a transaction created only when the process that
  created them is the one rolling back. A killed process leaves recovery to
  `recover_transaction`, which rebuilds the transaction from the journal, and
  the journal does not record directory creations.

  A sidecar file keyed to the transaction id would work — `Journal` and
  `JournalEntry` both carry `#[serde(deny_unknown_fields)]`, so widening the
  schema really would make a new journal unreadable to an older release, but a
  separate file next to it is invisible to that reader and costs no
  compatibility. What it costs instead is a new write, a new read, and new
  stale-file handling in the crash-recovery path of a **self-updater** — the
  one code path whose failure mode is an unbootable installation, and the file
  in this repository where three consecutive review rounds each found a fresh
  defect in the previous round's fix.

  The residue being cleaned up is an empty directory inside the install
  prefix. It does not strand the installation, corrupt the record, or leak
  anything; `uninstall` and the next transaction both tolerate it. Cleaning it
  is not worth new risk in that path.

## Deferred — correctness

- **`kettle-vt`**: 8-bit C1 introducers (`0x90`/`0x9d`/`0x9f`) bypass extraction,
  so raw-C1 Sixel/Kitty images do not render and raw-C1 OSC 7 does not update
  cwd. **Closed as WON'T FIX** after building it: see `docs/ARCHITECTURE.md`.
  In a UTF-8 stream a raw C1 byte cannot be told from mojibake, and guessing
  wrong costs the rest of the line — `0x9d` is `¥` in CP437, so a legacy-codepage
  console printing `¥100 units` produced exactly an OSC introducer followed by a
  plausible body, and the implementation measured 32 bytes of that line reaching
  the grid as 7. Recognising the values also costs a second scan pass over every
  byte of every PTY read (~1.9× on plain ASCII), paid by everyone for a form
  nothing in ordinary use emits. Two adversarial reviews each found a fresh
  quadratic in the recognition scan (594×, then 3,021× after the first was
  "fixed"). If a real emitter turns up, the honest shape is an opt-in mode
  (S8C1T) rather than a heuristic over untrusted bytes.
- **`kettle-core/term.rs`**: `log_strip_ansi` recognises only CSI/OSC, so
  DCS/APC image bodies are written into session logs as plain text. (Its CAN/SUB
  and UTF-8 gaps are fixed.)
- **`kettle-vt`**: untrusted output could install a UNC cwd, which a downstream
  Windows existence check turns into an SMB/WebDAV lookup that hands over the
  machine's credentials during the handshake. **Fixed** — both report channels
  (OSC 7 and OSC 9;9) go through one guard that refuses a path leading with two
  separators in any mix, one carrying a control character, and one longer than
  any real path. Still open: OSC 9;9 trims quotes and whitespace and OSC 7
  converts lossily, so a valid POSIX path containing them does not round-trip.
- ~~**`kettle-vt/image.rs`**: straight-alpha source-over ignores destination
  alpha, darkening Kitty animation composites.~~ **Fixed**, and verified
  bit-identical for an opaque destination so nothing that rendered correctly
  before moves.
- **`kettle-render`**: combining marks consume a grid column each, shifting the
  rest of the row; snapshots keep only four zero-width marks; the block cursor
  redraws only the base scalar, so an accent vanishes under the cursor.
- **`kettle-render`**: all three **fixed**.
  `cursor-fg-color`'s branch was unreachable; the decision is
  `color::cursor_glyph_color` now and a test drives it.
  `background-darkness` was not inverted in the CODE — it matches Terminator,
  which assigns the value straight to the background colour's alpha, so `0.0` is
  see-through and `1.0` is covered. Both `docs/CONFIG.md` and the field's own
  doc comment described that backwards, sending anyone who followed the
  documentation to the wrong end of the scale. The prose is corrected and the
  direction is pinned by a test.
  `minimum-contrast` really was defeated by `bold-is-bright`: the lift ran
  first, and the remap then replaced the foreground outright with a palette
  entry. The order is inverted and both steps live in `attributed_foreground`.
  The fixture that catches it needs a base colour that is ALREADY compliant —
  with a non-compliant one the lift moves the colour off the palette entry, the
  remap finds nothing to match, and the bug hides.
- **`kettle-render`**: `extra-styling`, `title-font`, `title-use-system-font`,
  `use-system-font`, `use-theme-colors` parse, validate, and have no consumer.
- **`kettle-remote`**: SSH options are parsed and discarded, so
  `ssh -p 2222 -J bastion host` reconnects as plain `ssh host` — potentially a
  different service. Container context (`docker --context`, `kubectl -n`) is
  likewise dropped, and reconnect strings assume a POSIX shell. Cloning an
  interactive pane re-executes command-bearing argv, repeating side effects and
  copying any credentials in it into a new process command line.
- **`kettle-remote`**: detection walks descendants rather than the foreground
  process group, so a backgrounded `ssh -N` tunnel marks a pane remote; and an
  attached tmux client's panes belong to a separate server process, so detection
  usually disappears inside tmux.
- **`kettle-ctl`**: activation is at-least-once with no idempotency key, so a
  slow cold start can open two windows; a `Client` stays reusable after a
  timeout without draining, so a late response can be read as the next call's;
  liveness is PID-only, so PID reuse resurrects stale records; and Unix casts
  `u32` PIDs straight to `pid_t`, making `u32::MAX` signal *everything*.
- **`kettle-state`**: the Windows private-parent ACL check omitted creation
  rights — **fixed, but not the way the finding described**. Adding
  `FILE_ADD_FILE` / `FILE_ADD_SUBDIRECTORY` / `GENERIC_WRITE` to the chain-wide
  set rejects every path on a stock Windows machine, because `C:\` grants
  Authenticated Users "create folders / append data" (14 of this crate's own
  tests went red). Creation rights on an ANCESTOR are ordinary and reach nothing
  of kettle's; on the directory kettle enumerates they let an untrusted
  principal plant a session, layout, or registry entry it reads back. The check
  now splits: path-redirecting rights on every component, creation rights on the
  target directory only. Still open: on Windows every ordinary directory is
  treated as private with no owner/DACL validation of its own, and the "durable"
  replace contract is not met (parent sync is a no-op, the rename is not
  write-through).
- **`kettle-update`**: the startup sweep finding is **overstated and closed**.
  Re-read against the code: the deletion is not "any file matching the pattern".
  The name must carry a valid transaction id, the path is resolved through
  `cleanup_anchored_destination`, and `open_windows_held_file` opens with
  `FILE_FLAG_OPEN_REPARSE_POINT` and refuses anything that is not a
  single-hardlink, non-reparse, ordinary file. The symlink and hardlink
  redirections that would make an unowned delete into an arbitrary-delete
  primitive are all already closed; what remains is kettle deleting a file an
  attacker planted under kettle's own scratch name, which harms only them. An
  ownership check would add nothing an attacker who can write to the install
  prefix has not already defeated by replacing `kettle.exe`.
  `InstallMarker.version` written-but-never-validated was real, and is **fixed**:
  every other field of that record was checked, and `install.json` is what
  support instructions and packaging scripts read to answer "what is installed
  here".
  (The Linux provenance break that was listed here as the highest-severity
  deferred item has since been **fixed** — see the table above.)
- **`kettle/exec.rs`**: process-tree termination misses double-forked/`setsid()`
  descendants; terminal replies outrank piped stdin with no timeout, so a child
  that queries in a loop can starve stdin indefinitely; `--cwd` rejects
  non-UTF-8 paths that work for an ordinary launch.
- ~~**`kettle/mcp.rs`**: stdout backpressure can deadlock the server — the
  writer blocks, `responses.send` blocks behind it, and EOF joins workers before
  closing the response senders.~~ **Fixed.** Every wait on the peer is bounded,
  and the first send that proves the peer is not reading latches a flag the rest
  check — otherwise bounding each send individually would have made sixty queued
  responses cost sixty times the limit. `run_mcp` is now a thin wrapper over
  `run_mcp_with`, which takes its transport, because the failure only appears
  when the peer stops reading and the process's real stdout cannot be made to do
  that from inside a test.

## Performance

Three of these turned out to be a wrong algorithm rather than a rewrite, and are
**fixed**. Each was measured before and after, on this machine, comparing the
two implementations directly:

- **Selection drawing walked every selected history line** before discarding the
  offscreen ones, so `Ctrl+A` over a million-line scrollback cost a million
  iterations on every repaint — every blink, every keystroke — to draw at most
  `screen_lines` quads. The row range is clamped to the viewport first
  (`visible_selection_rows`), making the work proportional to what is drawn.
- **The `kettle exec` / MCP capture sink trimmed to exactly 1 MiB on every
  write**, so once full a 4-KiB chunk shifted the whole buffer down by 4 KiB.
  Compaction is amortized over a full cap of slack now. Retaining the last 1 MiB
  of 64 MiB of output in 4-KiB chunks: **391 ms → 8.1 ms, 48×**, on the thread
  draining the PTY.
- **Glyph-cache eviction fully sorted all 131,072 entries** on the render
  thread, inside the frame that overflowed the cache, to keep a prefix.
  `select_nth_unstable_by_key` answers the same question in average linear time:
  **217 ms → 43 ms over 50 rounds, 5×**.

A fourth pass took the starfield:

- ~~Starfield is `O(surface pixels × 55 stars)` with trig/pow/exp in the
  fragment loop — roughly 456M star-iterations per 4K frame.~~ **FIXED.** The
  work was never per-pixel work: the hash, angle, radial ease, colour lookup
  and sRGB decode are all pixel-independent and were simply being recomputed.
  The model is resolved on the CPU now — once at startup for what never
  changes, once per frame for what tracks time and resolution — and the shader
  keeps only the distance to each star and its two falloff terms. Per star per
  pixel that is one `exp` where there were about ten transcendentals, and the
  loop bound shrinks because stars below the visibility threshold are dropped
  before upload rather than `continue`-ing on every pixel.

  It also closed one of the "tests that cannot fail" below: the brightness
  curve was a hand-copied Rust transcription in the test module, so the shader
  it protected could drift away underneath it. The curve is production code
  now and the tests drive it.

Still deferred — these are rewrites that need their own change and their own
measurement:

- Animated backgrounds re-upload the whole texture every frame (~32 MiB per
  frame at 4K) and repeat it each loop.
- ~~**Widening a window permanently destroys scrollback.**~~ **FIXED**, and it
  was worse than first characterised: not one gesture but four, all ordinary —
  a window drag (each intermediate width applying its own cap), decrease-font,
  closing a sibling split (−2013 lines), and un-zooming. The cap is monotonic
  per pane now.

  A survey of the field settled the design question. xterm, Windows Terminal,
  alacritty, WezTerm, kitty and VTE all bound scrollback in LINES, and none of
  them evicts on widening. The one terminal with a real byte budget, Ghostty,
  trims from the oldest end *as new output arrives* — a property of growth, not
  of geometry. So enforcing a memory budget through a resize was the mistake,
  not the budget itself.

  Still open, and worth doing in the same release: enforce `scrollback-bytes`
  against ACTUAL retained size on the growth path rather than as a
  width-derived ceiling, so a widened pane converges back into budget as output
  arrives instead of overshooting until it narrows again. Longer term, storing
  history rows at their occupied length rather than padded to the column count
  removes the trade-off entirely — a widen would then cost nothing and the
  budget would measure real occupancy — but that is a change to the vendored
  grid and it makes reflow harder.

- **Superseded, kept for the measurements:** `kettle-core`'s
  `effective_scrollback_lines` turns the `scrollback-bytes` budget into a LINE
  cap by dividing it by a worst-case per-row cost at the *current* column
  count, and `try_resize_geometry` recomputes that on any grid change —
  including a width-only one. Wider pane, smaller cap, oldest rows evicted at
  once and unrecoverably. Reproduced against a live pane with
  `scrollback = 10000` and 30 000 emitted lines: history went 5202 → 3210 →
  2134 → 1681 as the window was dragged from 77 to 241 columns, and narrowing
  again did not bring any of it back. Every value matches
  `(10_000_000 − (24·cols+64)·rows) / (24·cols+64)` exactly, so this is the
  byte-budget recompute rather than reflow. Dragging a window wider once during
  a long Claude Code or Codex session throws away most of the transcript.

  Related, same arithmetic: the shipped `scrollback-bytes` default of 10 MB
  binds before the documented `scrollback = 10000` at any ordinary width
  (10 000 × (24·80+64) ≈ 19.8 MB), so the documented default is unreachable and
  `--check-config` reports a number the terminal will not honour.

  Not fixed here because there is no way to hold a hard byte ceiling *and*
  never evict on widening — retained bytes genuinely grow with width. Choosing
  between them is a design decision about what `scrollback` and
  `scrollback-bytes` each promise, it changes a documented memory guarantee,
  and it belongs in `kettle-core`'s grid with its own measurement rather than
  bolted onto an unrelated batch.
- Per-cell quad vectors and the GPU instance buffer keep their high-water
  capacity and are not charged against `GraphicsBudget`.

## Load-sensitive fixtures

`ctl_server::tests::eight_idle_peers_expire_and_a_fresh_request_is_served`
fails as `connection N expired before all peers were admitted` when the machine
is saturated — observed with nine kettle instances running concurrently — and
passes in isolation. It asserts that eight peers are all admitted before the
idle-expiry timer retires any of them, which is a race the fixture wins only
when scheduling is prompt. CI runners are shared, so this will flake there too.
The fix is to make admission observable rather than assumed, not to lengthen
the timeout. Joins the two `kettle/tests/exec.rs` ConPTY timing fixtures, which
have the same shape.

## Open — an empty channel is mistaken for a fully-read PTY

**This is a product race, not a fixture problem, and it appears to be the root
cause of two separate long-standing macOS intermittents.**

`crates/kettle/src/exec.rs:1069` decides the child is finished and it is safe to
wrap up when

```rust
gone.elapsed() >= SETTLE && orx.is_empty()
```

`SETTLE` is 60 ms and `orx` is the raw PTY output channel, filled by the reader
thread. **An empty channel is not evidence that the PTY has been read to EOF.**
It is equally consistent with the reader thread not having been scheduled yet.
For a child that writes a little and exits at once — `echo` — the exit status
can be observed, 60 ms can elapse, and the output can still be in flight. The
loop then calls `recorder.begin_finish()` and closes `output`, and the bytes
arrive with nowhere to go.

Two symptoms, one cause, because `drain_output_slice` feeds the recorder and
stdout from that same channel behind that same gate:

- `exec_record_writes_replayable_asciicast` fails with a recording containing
  only its asciicast header. Observed on macOS CI 2026-08-05. The file looks
  structurally valid, which is what makes this data loss rather than an error:
  `kettle exec --record` can silently produce a trace missing the command's
  entire output.
- `exec_streams_stdout_and_exits_zero` returning exit 0 with empty stdout —
  the older macOS intermittent, whose description matches this mechanism
  exactly.

**The shape of the fix.** A *disconnected* channel is conclusive where an empty
one is not: the reader drops the sender only after the PTY reaches EOF, so
`try_recv() == Err(Disconnected)` is positive evidence. Prefer it, and keep a
time bound as the fallback for platforms where the reader outlives the child
(Windows ConPTY), so the loop still cannot hang. Lengthening `SETTLE` is not a
fix — it moves the race rather than removing it.

**Why it is not fixed here.** `exec`'s lifecycle loop is the highest-risk code
in the repository, this machine has no macOS loop to verify against (see the
project notes), and the failure only manifests under scheduling pressure on the
platform that cannot be reproduced locally. A change made blind here is how the
ConPTY saturation regression happened. It needs its own change, with the fix
verified red against a fixture that starves the reader deliberately rather than
waiting for CI to lose the race again.

## Deferred — tests that cannot fail

The audits found **24** of these, on top of the ones already fixed during the
parity work. The recurring shapes:

- `include_str!` source guards that match their own assertion literal, or slice
  to end-of-file so deleting the production call still passes.
- GPU tests that convert adapter-resolution failure into `Ok(false)` and report
  "skipped", so a resolver regression keeps the pipeline test green.
- ~~Starfield tests that exercise a hand-copied Rust transcription of the
  brightness formula rather than the WGSL that actually runs.~~ **CLOSED** by
  the starfield rewrite above: the curve is production code the shader consumes,
  and the tests drive it.
- **A source guard that searches its own file matches its own assertion.** Any
  `src.contains("<literal>")` where `src` came from `include_str!` of the same
  file passes whether or not the production code exists, because the needle is
  sitting one line above the search. `kettle-ui/src/app.rs` had **28** of these
  across 11 tests and `mux.rs` had 4.

  `split_divider_drag_is_wired` is the one that had already gone wrong: the
  multi-window refactor threaded `ws` through `split_drag_at` and
  `split_seam_hover_icon`, leaving the guard with zero production matches, and
  it stayed green with the entire press-to-start-drag block deleted — rustc
  reported both functions as dead code while their dedicated drift guard passed.
  Its own comment shows the author knew the failure mode and applied the
  reasoning to two of six assertions; those two were vacuous as well, because a
  distinctive comment at the production site does not help when the test
  repeats it as a string literal.

  **CLOSED.** Both files now have a `production_source()` that REMOVES every
  `#[cfg(test)]` item before any guard searches, and all 50 self-searching
  guards were moved onto it — a scan for whole-file `contains` needles reports
  zero in each file, against 22 and 4 before.

  Removing rather than truncating matters: `app.rs` interleaves seven
  `#[cfg(test)]` items with production code across 30,000 lines, so a first
  attempt that sliced at the first one threw most of the production away and
  failed all 48 guards for the opposite reason. The helper now asserts three
  postconditions — that it excludes itself, that it still contains a known
  production symbol, and that it kept more than half the file — so an
  over-broad or under-broad cut fails loudly instead of quietly making every
  guard meaningless in either direction.

  Turning the guards on immediately found a second rotted one:
  `recorder_output_flushed_before_reap_and_on_close` keyed on three comment
  sentences, and one had been reworded when `close_window_now` grew its
  `DropPanes` ordering explanation. The wiring was intact; the guard had been
  matching its own stale copy of the sentence. It keys on the three call sites
  and their enclosing functions now, plus an exact count, because a comment is
  not a contract.
- Tests that assert a *duplicated* expression instead of driving the production
  entry point (`update_cli` confirmation, shell-integration mapping,
  `--print-default-config` dispatch, man-page keybindings).
- Assertions loose enough to accept the failure they exist to catch — the MCP
  self-test accepts any error text containing `PTY`.
- `install.rs` tests that call a `cfg(test)` duplicate containing the logic the
  production path is missing, so both stay green while the bug ships.

Five of these were closed rather than deferred, each by moving the decision into
a named function the test drives exactly as production does:

- `--write-default-config` — the test restated `create_new` and the error
  predicate locally; deleting the production branch left it green. The branch is
  now `write_default_config`, and the test also checks the bytes written are the
  config kettle ships.
- The cursor glyph colour — the test demonstrated that `resolve_query(258, ..)`
  and `term_colors[258]` differ, but never referenced the renderer's condition,
  so restoring the bug at the call site left it green. It is
  `color::cursor_glyph_color` now.
- The profile cycle order — `list_profiles` can only read the real config
  directory, so its documented ordering was untested. The rule is
  `sort_profile_names`.
- `--accent` and `--working-directory` had no test at the CLI surface at all.
  Both are `flag_value_problem` now, driven from a parsed `Cli`.
- The alpha convention detector recognised exactly two token spellings, so
  multiplying through `let extra = in.color.a` read as a single multiply. It
  resolves aliases now — but the real answer is
  `gpu_tests::a_half_opaque_quad_blends_at_half_not_a_quarter`, which renders a
  translucent quad and reads the pixel back. It returns 137 instead of 188 with
  the original bug restored.

See `docs/TESTING.md` for the checks that catch each shape.
