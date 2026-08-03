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

Still open on those same fixes:

- The premultiplied blend fix is correct locally, but `lib.rs`'s surface clear
  still writes straight RGB under `CompositeAlphaMode::PreMultiplied`. For a
  translucent clear the two former bugs partly cancelled, so this combination
  needs the clear fixed to be fully right.
- `--write-default-config` still follows a **parent** junction; only the final
  component is atomic. Redirecting creation to an absent destination remains
  possible for an attacker who can replace a writable parent directory.

## Deferred — correctness

- **`kettle-vt`**: 8-bit C1 introducers (`0x90`/`0x9d`/`0x9f`) bypass extraction,
  so raw-C1 Sixel/Kitty images do not render and raw-C1 OSC 7 does not update
  cwd. `kettle-core/term.rs`'s `log_strip_ansi` has the same CAN/SUB gap just
  fixed in the extractor, and recognises only CSI/OSC — so DCS/APC image bodies
  are written into session logs as plain text.
- **`kettle-vt`**: untrusted output can install a UNC cwd
  (`OSC 9;9;\\attacker\share`), which a downstream Windows existence check could
  turn into an SMB/WebDAV lookup. Also, OSC 9;9 trims quotes/whitespace and
  OSC 7 converts lossily, so valid POSIX paths do not round-trip.
- **`kettle-vt/image.rs`**: straight-alpha source-over ignores destination alpha,
  darkening Kitty animation composites.
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
- **`kettle-update`**: Windows startup deletes any file matching the
  helper/archive name pattern without an ownership or age check, and
  `InstallMarker.version` is written but never validated.
  (The Linux provenance break that was listed here as the highest-severity
  deferred item has since been **fixed** — see the table above.)
- **`kettle/exec.rs`**: process-tree termination misses double-forked/`setsid()`
  descendants; terminal replies outrank piped stdin with no timeout, so a child
  that queries in a loop can starve stdin indefinitely; `--cwd` rejects
  non-UTF-8 paths that work for an ordinary launch.
- **`kettle/mcp.rs`**: stdout backpressure can deadlock the server — the writer
  blocks, `responses.send` blocks behind it, and EOF joins workers before
  closing the response senders.

## Deferred — performance

Each of these is a rewrite that needs measurement, not a patch:

- Starfield is `O(surface pixels × 55 stars)` with trig/pow/exp in the fragment
  loop — roughly 456M star-iterations per 4K frame.
- Selection drawing iterates every selected *history* line before discarding
  offscreen rows, so a million-line selection costs a million iterations on
  every repaint.
- Glyph-cache eviction sorts all 131,072 entries on the render thread.
- Animated backgrounds re-upload the whole texture every frame (~32 MiB per
  frame at 4K) and repeat it each loop.
- The MCP 1 MiB tail sink uses front `Vec::drain`, shifting ~1 MiB per small
  chunk — a 128–256× copy amplification on a hot path.
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

See `docs/TESTING.md` for the checks that catch each shape.
