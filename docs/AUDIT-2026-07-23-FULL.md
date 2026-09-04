# Full-repository audit — 2026-07-23 (v2.39.0 cycle)

## Scope and method

A whole-repository correctness / robustness / security / performance / UX /
compatibility / architecture audit, run as a multi-agent workflow: nineteen
parallel finder lanes (one deep-review agent per crate for all ten crates, plus
cross-cutting lanes for terminal-emulator semantics, untrusted-input security,
performance hot paths, the UI/UX state matrix, platform + hosted-TUI
compatibility, architecture / dead-config, documentation accuracy, scripts/CI,
and a re-examination of the deferred backlog). Each finding was deduplicated and
then **adversarially verified** — high-severity findings by two independent
refuters instructed to default to "refuted" — before implementation.

The pass surfaced **59 verified findings** (16 high, 33 medium, 10 low); the
adversarial gate refuted only 4 of 74 verdicts. Implementation ran one owner
agent per file (disjoint files, robust fixes preferred over quick patches, tests
added where behaviourally testable), followed by a compile/clippy/test
reconciliation pass and a second adversarial verification of the
security-critical diffs.

**Result: 52 findings fixed, 7 deferred.** The full workspace gauntlet
(`fmt` · `clippy -D warnings` · `build` · `test` · `doc` · `deny` · `machete` ·
`tracked-audit`) is green; 690 workspace tests pass. (The four GPU
`gpu_tests` / `menu_visual` / `scrollbar_visual` binaries could not run locally
during this cycle — the dev machine's D3D12 runtime was in a crash-on-init
state needing a reboot; they self-skip on CI, which is the authoritative gate.)

## Shipped fixes, by theme

### Security / untrusted input
- **Control-plane peer authentication on Windows was a no-op.**
  `CtlStream::peer_is_same_user()` returned `Ok(true)` unconditionally,
  contradicting its own contract and the "verifies same-user peers" guarantee in
  `ARCHITECTURE.md`. It now performs a real check —
  `GetNamedPipeClientProcessId` → open the client process →
  `GetTokenInformation(TokenUser)` → `EqualSid` against this process's token —
  with RAII handle ownership and fail-closed error handling.
- **Remote-context pane titles bypassed the bidi/control-char sanitizer.** SSH /
  container titles built from scanned process argv reached `pane.title`,
  window titles, and the accessibility tree without the neutralization every
  other title path applies; they now route through `sanitize_title()`.
- **`ctl screenshot` wrote a PNG to an attacker-supplied path with no
  containment**, and screenshots / the remote-command file were created with
  default permissions; the write paths are now contained and permission-
  restricted (owner-only on Unix), matching the recording feature's hardening.
- **Unbounded JSON nesting could crash the MCP server** via parser stack
  overflow; an explicit non-recursive depth guard (limit 64, string/escape
  aware) now rejects over-nested lines with a JSON-RPC error before parsing.
- **The update-archive verify/extract was a TOCTOU** (two separate path opens);
  it now verifies and extracts without a swap window.
- **The `curl | sh` bootstrap installer trusted an unsigned checksum** fetched
  from the same channel as the tarball; it now verifies against the signed
  update manifest.
- Config load bypassed the hardened bounded reader; the ctl discovery registry
  and AF_UNIX path length gained the ownership / length safeguards their Unix
  siblings already had; the legacy scrollback `search` API and the OSC 52
  clipboard-read reply gained the bounds their symmetric paths already enforced.

### Correctness
- **Windows piped stdin was silently discarded.**
  `attach_parent_console_if_needed` unconditionally replaced `STD_INPUT_HANDLE`
  with `CONIN$`, defeating `is_terminal()` and hanging `echo y | kettle update`;
  it now guards stdin the same way it already guarded stdout/stderr.
- The `kettle.com` console launcher was missing `--new-process` in its
  hand-maintained flag classifier, so it blocked the shell instead of returning.
- `atomic_create_new` could leak its staged temp file and report a successful
  create as a failure; bounded/timeout lock acquisition was added so a stuck
  holder no longer wedges every caller forever.
- Cross-chunk ANSI-strip state, zoom/font-size desync, vi-mode selection
  reclamping after reflow, and keybind-rebind collision warnings were fixed.

### Performance
- **Context-menu, settings, and search-family overlay text buffers reshaped
  every frame.** They now carry the same text-equality reshape gate the tab bar
  and (as of v2.38.2) the quick-select hints use — an open overlay no longer
  re-shapes every label on each blink-driven redraw. The glyph-atlas slot cache
  and quad-buffer growth gained eviction / checked-arithmetic bounds.

### Config / architecture / tooling
- **Ten config knobs were parsed but never read.** Dead knobs were removed
  (unknown keys still warn-and-ignore, so existing user configs never error);
  knobs that named real behaviour were wired up.
- `just gauntlet` was aligned to actually mirror the CI gate; CI gained the
  Windows CLI-rendering smokes and the split-titlebar live smoke was enabled on
  Windows; the `bench.ps1` zero-sample race was fixed.

### Documentation
- Six `TERMINATOR-*-DESIGN.md` "design only" headers were corrected to the
  version each feature actually shipped in; the PowerShell shell-integration
  snippet that reproduced an already-fixed infinite-prompt loop, the broadcast
  keybind, the animated-background limits, the workspace test counts, and the
  stale `CONTRIBUTING.md` recipe list were all corrected against the code.

## Deferred (tracked in `AUDIT-DEFERRED.md`)

Seven findings were deferred as genuine multi-session work or cross-crate
plumbing that should not be rushed into one release:

- **`app.rs` (24.7k lines) and the `kettle-render` `impl Renderer` block** —
  god-file splits along the already-implied module seams; large, best done as
  their own refactor with behavioural tests replacing the source-drift guards.
- **OSC 52 selection target** (`p`/`s` vs `c`) — needs the
  `alacritty_terminal::term::ClipboardType` threaded from the event into the
  arboard PRIMARY path.
- **OSC 133 prompt-mark line numbers desync once scrollback wraps** — needs a
  stable anchor that survives the grid's history-ring cap.
- **Global kitty `a=d,d=f` animation clear never reaches the renderer** — needs
  a `Chunk::PtyReply`-style clear signal across the vt→render boundary.
- **State/lock-file `0600` is a no-op on Windows** — needs new `windows-sys`
  `Win32_Security_Authorization` features + a `SetNamedSecurityInfoW` ACL.
- **Command palette / layout / SSH pickers stay single-line** — fold into the
  responsive multi-row layout the search bar already uses.

### Follow-up status (2026-07-27)

This list is retained as the audit's historical seven-finding result. The
subsequent performance/quality campaign resolved four entries without erasing
that record:

- OSC 52 `p`/`s` now uses Linux PRIMARY with no cross-target fallback.
- OSC 133 prompt marks use stable `history_origin`-based document-row ids.
- Global Kitty deletion and `d=f` frame deletion now update renderer-visible
  state; the full spatial/id selector set and same-read delete/replacement
  ordering are covered.
- Windows private state/lock creation now requires a protected current-user
  DACL and fails closed when it cannot establish one.

The Kitty acknowledgement/query path, existing-image-id retransmission cleanup,
and exact `Q=` parent-placement selection remain deferred; completed deletion
and placement geometry must not be read as full Kitty protocol conformance.

## Verification

`just gauntlet-strict` green (GPU binaries excepted, see scope); 690 tests pass;
the security-critical diffs passed a second adversarial verification pass. Live
on-screen verification of the render-path fixes is pending the dev machine's GPU
reboot and folds into the post-release install check.
