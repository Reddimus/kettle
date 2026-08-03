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

Still open on those same fixes:

- The premultiplied blend fix is correct locally, but `lib.rs`'s surface clear
  still writes straight RGB under `CompositeAlphaMode::PreMultiplied`. For a
  translucent clear the two former bugs partly cancelled, so this combination
  needs the clear fixed to be fully right.
- `--write-default-config` still follows a **parent** junction; only the final
  component is atomic. Redirecting creation to an absent destination remains
  possible for an attacker who can replace a writable parent directory.
- Rollback removes the directories the transaction created only when the
  process that created them is the one rolling back. A process killed mid-update
  leaves recovery to `recover_transaction`, which rebuilds the transaction from
  the journal — and the journal does not record directory creations, because
  widening its schema would make a journal unreadable to the release that might
  have to recover it. Such a directory stays behind unowned.
- Linux provenance now records the uid that PUBLISHED the files
  (`geteuid()`, matching `install-unix.py`) rather than the prefix owner. The
  two agree in every reachable case, so no test can distinguish them without
  root and an ACL-writable root-owned prefix; the assertion pins intent only.

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
- **`kettle-vt`**: untrusted output can install a UNC cwd
  (`OSC 9;9;\\attacker\share`), which a downstream Windows existence check could
  turn into an SMB/WebDAV lookup. Also, OSC 9;9 trims quotes/whitespace and
  OSC 7 converts lossily, so valid POSIX paths do not round-trip.
- ~~**`kettle-vt/image.rs`**: straight-alpha source-over ignores destination
  alpha, darkening Kitty animation composites.~~ **Fixed**, and verified
  bit-identical for an opaque destination so nothing that rendered correctly
  before moves.
- **`kettle-render`**: combining marks consume a grid column each, shifting the
  rest of the row; snapshots keep only four zero-width marks; the block cursor
  redraws only the base scalar, so an accent vanishes under the cursor.
- **`kettle-render`**: `cursor-fg-color` has no block-cursor effect (its branch
  is unreachable); `background-darkness` changes clear alpha instead of tinting,
  inverting the documented meaning for `background-type = transparent`;
  min-contrast is applied before `bold-is-bright`, which can undo it.
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
- **`kettle-state`**: the Windows private-parent ACL check omits `FILE_ADD_FILE`,
  `FILE_ADD_SUBDIRECTORY`, and `GENERIC_WRITE`; on Windows every ordinary
  directory is treated as private with no owner/DACL validation; and the
  "durable" replace contract is not met (parent sync is a no-op, the rename is
  not write-through).
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

Still deferred — these are rewrites that need their own change and their own
measurement:

- Starfield is `O(surface pixels × 55 stars)` with trig/pow/exp in the fragment
  loop — roughly 456M star-iterations per 4K frame.
- Animated backgrounds re-upload the whole texture every frame (~32 MiB per
  frame at 4K) and repeat it each loop.
- Per-cell quad vectors and the GPU instance buffer keep their high-water
  capacity and are not charged against `GraphicsBudget`.

## Deferred — tests that cannot fail

The audits found **24** of these, on top of the ones already fixed during the
parity work. The recurring shapes:

- `include_str!` source guards that match their own assertion literal, or slice
  to end-of-file so deleting the production call still passes.
- GPU tests that convert adapter-resolution failure into `Ok(false)` and report
  "skipped", so a resolver regression keeps the pipeline test green.
- Starfield tests that exercise a hand-copied Rust transcription of the
  brightness formula rather than the WGSL that actually runs.
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
