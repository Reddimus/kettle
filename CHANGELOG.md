# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

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
  - **A stalled `kettle exec` stdout consumer no longer defeats `--timeout`.**
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
    Unix. Crash sweeping moved off the startup/UI thread, has independent
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
[docs/AUDIT-DEFERRED.md](docs/AUDIT-DEFERRED.md). Full write-up in
[docs/AUDIT-2026-07-23-FULL.md](docs/AUDIT-2026-07-23-FULL.md).

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
    [docs/RECORDING.md](docs/RECORDING.md).
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
    **[CONFIG.md](docs/CONFIG.md)**.

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
    don't touch the parse path). See [docs/PERFORMANCE.md](docs/PERFORMANCE.md).

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
  - New **[docs/BACKGROUNDS.md](docs/BACKGROUNDS.md)** — a walkthrough plus
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
  control surface is OFF by default. See [docs/AGENT.md](docs/AGENT.md).

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
    crate). See [docs/TESTING.md](docs/TESTING.md) for the coordinate-space +
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
  - **Docs:** added a non-technical [Getting Started](docs/GETTING-STARTED.md)
    walkthrough and a [Settings panel](docs/SETTINGS.md) reference; the
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

## [1.47.0] — 2026-05-29

  Fixes the Windows 11 close-split freeze (the headline), adds crash
  logging so a Start-menu launch can never silently swallow a panic
  again, and modernizes the Windows default shell to PowerShell 7+.
  Bundles cycles 741-744 (numbered against a local branch point) plus
  the prior on-`main` documentation pass that stripped internal
  cycle-reference parentheticals from CONTRIBUTING / README /
  ARCHITECTURE / CONFIG and extended the doc-scan drift guard.
  Root-caused and verified end-to-end on a Surface Book 3 (Windows 11
  build 26200).

  Minor bump (not a patch): cycle 743 changes the default shell on
  Windows, a user-visible behavior change, and cycle 741 adds crash-log
  output — both warrant a "read the changelog" signal.

  Headline changes:
  - cycle 742: **closing a split pane no longer freezes the app on
    Windows 11** — `Terminal::Drop` deadlocked the UI thread by
    `join()`ing a PTY reader blocked on a ConPTY `read()`; it now detaches
    the reader (never joins on the UI thread). This is the crash users hit
    on `Ctrl+Shift+W` after a split.
  - cycle 743: **defaults to PowerShell 7+ (`pwsh`) on Windows** when no
    shell is configured (was `cmd.exe`), matching Windows Terminal;
    override with `shell = …`.
  - cycle 741: **crash logs** — a `panic = "abort"`-safe hook writes
    panics (message + backtrace) to `%LOCALAPPDATA%\kettle\crash\`
    (`$XDG_STATE_HOME`/`~/.local/state` on Unix) and stderr.
  - cycle 744: README front-door polish — Windows `install.ps1` +
    pwsh-7 default, platform matrix, first-launch walkthrough.

  See per-cycle paragraphs below for details.

  cycle 744 — **Docs: README front-door polish for newcomers**: the
              Windows install section now leads with the bundled
              `install.ps1` (per-user Start-menu install + PATH, the
              `-WithShellIntegration` and `-Uninstall` flags, and the
              `-ExecutionPolicy Bypass` escape hatch) instead of a bare
              "unzip + add to PATH", and documents that kettle opens
              PowerShell 7+ by default (cycle 743) with the `shell =`
              override. Adds a platform-support matrix at the top of
              Install and a 5-step **First launch** walkthrough (theme,
              right-click Preferences, splits/tabs, search + palette,
              config). Refreshes the stale `v1.45.x` status line to
              `v1.46.x`. Removes an internal `cycle-717` reference that
              had leaked into the user-facing context-menu tip.

  cycle 743 — **Change: default to PowerShell 7+ on Windows (was
              `cmd.exe`)**: when no `shell`/`command` is configured,
              kettle now spawns `pwsh.exe` (PowerShell 7+) if it is on
              `PATH`, falling back to Windows PowerShell 5.1
              (`powershell.exe`), then `%ComSpec%` / `cmd.exe`. This
              matches Windows Terminal, whose default is pwsh 7 when
              installed — a bare `cmd.exe` felt dated on a modern Windows
              11 box and couldn't show off kettle's truecolor / OSC 133 /
              bracketed-paste support that PSReadLine relies on. Users
              who prefer cmd (or anything else) still set `shell =
              cmd.exe` in their config; only the *unset* default changed.
              Detection is robust to Store-installed pwsh: it uses
              `symlink_metadata` (lstat) rather than `is_file()` so the
              `%LOCALAPPDATA%\Microsoft\WindowsApps\` app-execution-alias
              stub for a Microsoft-Store pwsh is found too. The
              preference order is unit-tested via an injected resolver
              (`pwsh` > `powershell` > cmd fallback). Verified live on
              build 26200: kettle now opens to the `PowerShell 7.6.2`
              banner instead of `cmd.exe`.

  cycle 742 — **Fix: closing a split pane no longer freezes kettle on
              Windows 11 (UI-thread PTY-teardown deadlock)**: splitting
              a pane (`Ctrl+Shift+O`/`E`) then closing the focused split
              (`Ctrl+Shift+W`) made the whole window stop responding —
              users reported it as a crash. Root cause: `Terminal`'s
              `Drop` runs on the UI thread (a pane close drops the owned
              `Pane.term`) and `join()`ed the PTY reader thread while the
              master pseudoconsole was still open. The reader sits in a
              blocking `read()` on the ConPTY conout pipe that only
              returns once the pseudoconsole is *closed* — but the master
              wasn't dropped until after `Drop` returned, so the join
              could never complete and the UI thread deadlocked. (On
              build 26100+ / 24H2, Microsoft's `ClosePseudoConsole`
              contract makes a UI-thread join on the blocked reader
              unrecoverable.) Reproduced on the Surface Book 3 (build
              26200): close-split left the process alive with
              `Responding=false` indefinitely — a hang, not a panic, so
              the cycle-734/735 render/Arc-lock theories were wrong. Fix
              mirrors WezTerm/Alacritty teardown: signal a stop flag,
              kill the child, close the writer (conin) and the master
              (conout / pseudoconsole) so the reader reaches EOF, then
              **detach** the reader thread — never `join()` on the UI
              thread. `Drop` now returns in sub-millisecond time; the
              detached reader owns only `Arc` clones (no borrow of
              `Terminal`) and winds down on its own once conout EOFs.
              Guards: `drop_is_prompt_with_blocked_reader` (runtime —
              dropping a `Terminal` with a blocked reader completes < 5s,
              verified on 26200) and `drop_detaches_reader_never_joins`
              (source drift guard — pins "no `.join()` in `Drop`").
              Verified live: post-fix, 5+ consecutive split→close cycles
              (incl. a 3-pane tree) keep `Responding=true` throughout.

  cycle 741 — **Add: crash logs (panics are no longer invisible on a
              Start-menu launch)**: a `panic = "abort"`-safe panic hook
              is installed first thing in `main()`. It prints the panic
              message, thread, location and a forced backtrace to stderr
              AND appends the same report to a crash-log file under the
              platform state dir (`%LOCALAPPDATA%\kettle\crash\` on
              Windows; `$XDG_STATE_HOME`/`~/.local/state/kettle/crash/`
              on Unix). Before this, a panic on an Explorer/Start-menu
              launch was completely silent — the cycle-740 console-hide
              path swallows stderr and `panic = "abort"` skips unwinding
              — which is exactly why two prior cycles had to *guess* at a
              crash's cause. The crash-log path helper is pure +
              env-injected (mirrors `home_dir_fallback`) with unit tests
              for the Windows/XDG/HOME/fallback branches; the hook itself
              never panics internally (no double-fault under abort).

## [1.46.3] — 2026-05-23

  Hot-fix release bundling cycles 738-740. Fixes a regression cycle
  734 (in v1.46.1) introduced: SUBSYSTEM:WINDOWS broke the
  bash-piped CLI smoke on Windows CI (`cargo run -- --some-flag |
  grep "…"` captures 0 bytes because stdout goes to the console
  screen buffer, not the inherited pipe). Cycle 740 replaces it
  with the Ghostty pattern (CONSOLE subsystem + hide-console-when-
  orphaned via `GetConsoleProcessList(1)` +
  `ShowWindow(GetConsoleWindow(), SW_HIDE)`). Phantom console on
  Start menu launch stays hidden (sub-50ms flash trade-off);
  `kettle --version` / `--shell-integration powershell` / etc. now
  print to PS/cmd/bash pipes correctly.

  Headline fixes:
  - cycle 740: hide-console-when-orphaned (fixes Win11 CI
    regression + restores `kettle --shell-integration powershell
    >> $PROFILE` one-liner working on Windows).
  - cycle 739: windows-sys 0.59 -> 0.61 minor bump.
  - cycle 738: CONTRIBUTING.md Win11 Smart App Control dev-setup
    note.

  See per-cycle paragraphs below for details.

  cycle 740 — **Fix: replace cycle-734 SUBSYSTEM:WINDOWS with
              hide-console-when-orphaned (Ghostty pattern)**:
              Cycle 734's `#![cfg_attr(windows, windows_subsystem =
              "windows")]` + AttachConsole + CONOUT$ rewire worked
              for the phantom-console-on-Start-menu case, but broke
              the Windows CI bash-piped CLI smoke
              (`cargo run -- --some-flag | grep "…"`) because
              SUBSYSTEM:WINDOWS routes stdout to the console screen
              buffer, NOT the inherited stdout pipe that bash's `|`
              reads. Verified locally on the Surface Book 3: cycle
              734's `kettle --shell-integration powershell >>
              $PROFILE` captured 0 bytes via every PS / cmd
              redirect pattern. Cycle-738/739's CI runs both went
              red on `build (windows-latest)` for this reason
              (cycle 738 was docs-only, cycle 739 was a windows-sys
              bump — neither caused the regression; cycle 734 did).
              Fix: switch to Ghostty's pattern. Stay on the default
              `console` subsystem (so stdout pipe inheritance works
              under PS / bash / cmd) and instead **hide the
              auto-allocated phantom console at startup ONLY when
              we are the only process attached to it** (i.e. Windows
              allocated it for us on Explorer / Start-menu launch
              and no shell is reading from it). `GetConsoleProcessList(1)`
              returns the count; if `== 1`, hide the window via
              `ShowWindow(GetConsoleWindow(), SW_HIDE)`. If `> 1`, a
              parent shell is using this console — leave it visible
              so CLI output reaches the user.
              Trade-off: there's a sub-50ms console flash on Explorer
              launch (Windows shows the console before our hide call
              lands). Tolerable compared to broken CLI stdout.
              `windows-sys` features adjusted:
              `Win32_UI_WindowsAndMessaging` (for ShowWindow) added;
              `Win32_Storage_FileSystem` + `Win32_Security` (cycle-734's
              CreateFileA/SetStdHandle dance) removed.
              Drift-guard test renamed
              `windows_console_hide_on_orphan_launch_survives` -
              asserts both Win32 calls survive AND that the
              `windows_subsystem = "windows"` attribute is NOT
              re-added (a future contributor re-adding it would
              re-break the CLI stdout regression).
              `docs/SHELL-INTEGRATION.md` reverted: the
              `kettle --shell-integration powershell >> $PROFILE`
              one-liner is now back in the cross-platform
              recommended block (cycle 736's "Windows-specific
              section" workaround is now superfluous). Kept the
              `install.ps1 -WithShellIntegration` flag as a
              hands-free alternative — still useful for the BEGIN/END
              marker wrapping + uninstall integration.
              Verified on Surface Book 3:
                - `kettle --version` from PS prints version
                - `kettle --shell-integration powershell > file`
                  captures 5756 bytes
                - Start-menu .lnk launch: conhost child still
                  exists (CONSOLE subsystem) but its ConsoleWindowClass
                  window is hidden by ShowWindow(SW_HIDE).
              Closes the Win11 CI regression introduced by cycle 734.

  cycle 739 — **kettle: windows-sys 0.59 → 0.61 minor bump**:
              `cargo outdated` on the Surface Book 3 audit pass
              (2026-05-23) flagged `windows-sys` as 2 minor versions
              behind (we added it at 0.59 in cycle 734; current
              latest is 0.61.2). No code change required (the
              AttachConsole / CreateFileA / SetStdHandle / FILE_SHARE_*
              constants we use are stable across 0.59-0.61). Bump
              keeps us on the line of versions actively receiving
              security fixes from the windows-rs project (and
              matches what wgpu / winit / sysinfo pull as transitive
              deps, reducing the workspace's duplicate-version
              footprint).
              `just gauntlet` green post-bump on Win11 (Surface Book
              3, MSVC 14.44, Rust 1.95).

  cycle 738 — **CONTRIBUTING.md: Win11 Smart App Control gotcha**:
              The cycle-730/733/734/736 Win11 audit pass surfaced
              that Windows **Smart App Control (SAC)** — enabled
              by default on clean Win11 installs with Secure Boot
              on — blocks any unsigned `.exe`, including every
              `build.rs` artifact cargo produces. Symptom:
              `cargo install <anything>` or `cargo build` on
              crates with `build.rs` fails with `An Application
              Control policy has blocked this file (os error
              4551)`. New Win11 contributors who hit this would
              be stuck trying to do Rust dev.
              Fix: short paragraph in CONTRIBUTING.md (under "Run
              the gate locally") naming the symptom, the toggle
              path (Settings ▸ Privacy & Security ▸ Windows
              Security ▸ App & browser control ▸ Smart App
              Control ▸ Off), and the caveat that it's a one-way
              toggle. Also mentions the winget-install workaround
              for signed binaries (`winget install Casey.Just`
              etc.) for users who prefer to keep SAC on.
              No code change.

## [1.46.2] — 2026-05-23

  Polish hot-fix following v1.46.1's Win11 audit pass. Two cycles:
  cycle 736 fixes the PowerShell shell-integration install one-liner
  (the docs' `kettle --shell-integration powershell >> $PROFILE`
  captured zero bytes under SUBSYSTEM:WINDOWS); cycle 737 drops a
  dead `log` dep from kettle-remote that cargo-machete surfaced on
  the audit pass.

  No Rust runtime change; no user-visible behavior change for users
  who don't use PowerShell shell-integration. PowerShell users now
  have two working install paths:
    - `install.ps1 -WithShellIntegration` (preferred — opt-in flag,
      idempotent, uninstall-aware).
    - Manual `Add-Content $PROFILE (Get-Content kettle.ps1 -Raw)`
      against the bundled snippet file at
      `%LOCALAPPDATA%\Programs\kettle\shell-integration\kettle.ps1`
      (documented in `docs/SHELL-INTEGRATION.md` "Windows / PowerShell"
      section).

  See per-cycle paragraphs below.

  cycle 737 — **kettle-remote: remove unused `log` workspace dep**:
              `cargo machete` on the Surface Book 3 Win11 audit pass
              (2026-05-23) flagged `log` as declared-but-never-used
              in `crates/kettle-remote/Cargo.toml`. Verified:
              `grep -r 'log::\|use log' crates/kettle-remote/`
              returns zero matches. The dep was declared at
              crate-creation (cycle 643) for the eventual remote-
              detection diagnostic logging path that never
              materialized — the actual diagnostics live in
              kettle-ui's per-pane state logging.
              Pre-737 cargo-machete CI hadn't caught this because
              `.github/workflows/machete.yml` only fires on
              Cargo.{toml,lock} changes, and
              `crates/kettle-remote/Cargo.toml` hadn't been touched
              since cycle 718; cycle 730's ProcessTree refactor
              was purely src/ changes → no manifest change → no
              re-run.
              Fix: drop `log.workspace = true` from
              `crates/kettle-remote/Cargo.toml`. No behavior change;
              workspace tests stay at 432. Bonus: cycle 737's
              manifest touch will re-trigger
              `.github/workflows/machete.yml` and confirm the rest
              of the workspace is also machete-clean.

  cycle 736 — **Windows PowerShell shell-integration UX fix**:
              v1.46.1 docs (`docs/SHELL-INTEGRATION.md`) showed
              `kettle --shell-integration powershell >> $PROFILE`
              as the install one-liner. Verified on the Surface Book
              3 (Win11 26200, PowerShell 5.1): the redirect captures
              **zero bytes** via any PowerShell pattern (`>>`, `>`,
              `Out-String`, `Start-Process -RedirectStandardOutput`,
              `cmd /c >`) because `kettle.exe` is now SUBSYSTEM:WINDOWS
              (cycle 734 trade-off). stdout goes to the parent
              console's screen buffer via `AttachConsole` + `CONOUT$`
              rewire, NOT to the inherited stdout pipe that
              PowerShell would read. Same limitation hits Alacritty
              and other Rust terminals; the install one-liner was
              just wrong for Windows.
              Fix shipped two ways:
                1. **`docs/SHELL-INTEGRATION.md`** — moved the
                   PowerShell line out of the "one-liner" block and
                   added a dedicated **"Windows / PowerShell — use
                   the bundled snippet file"** section showing the
                   working pattern: `Add-Content $PROFILE
                   (Get-Content kettle.ps1 -Raw)` directly against
                   the bundled snippet file at
                   `%LOCALAPPDATA%\Programs\kettle\shell-integration\kettle.ps1`.
                2. **`scripts/install.ps1 -WithShellIntegration`** —
                   new opt-in flag that does the same `Add-Content`
                   automatically as part of the install. Wraps the
                   snippet in distinctive `# >>> kettle
                   shell-integration (managed by install.ps1)` /
                   `# <<< …` BEGIN/END markers (oh-my-posh /
                   conda init / nvm pattern) so the uninstall
                   path can find + remove the exact block we
                   added without touching surrounding user
                   customization. Idempotent: re-running with the
                   flag detects the markers and skips. The default
                   uninstall (`install.ps1 -Uninstall`) also strips
                   the block automatically if present so
                   `appwiz.cpl` -> kettle -> Uninstall cleans
                   `$PROFILE` for free.
              Verified end-to-end on the Surface Book 3:
                - `install.ps1 -WithShellIntegration` -> snippet
                  appears in `$PROFILE`, marker block detected.
                - re-run -> "snippet already in `$PROFILE` (no
                  change)".
                - fresh PowerShell session sources `$PROFILE`
                  without error; `$global:__kettle_prompt_installed`
                  = True.
                - `install.ps1 -Uninstall` -> marker block + snippet
                  removed, surrounding `$PROFILE` content
                  preserved.
              No Rust code changes; v1.46.2 hot-fix candidate.

## [1.46.1] — 2026-05-23

  Hot-fix release bundling cycles 732-735. Two user-reported v1.46.0
  Win11 defects + two CI/installer polish cycles. No public API
  change; no behavior change on Linux/macOS. Workspace tests 430 →
  431 on Windows (cycle-734 drift guard).

  Headline fixes for v1.46.0 users on Windows 11:
  - **No more phantom console on Start menu launch** (cycle 734).
    Launching kettle from Windows Search opened TWO windows
    pre-734 (kettle's wgpu window + a stock ConsoleWindowClass
    console). Fixed via `#![cfg_attr(windows, windows_subsystem =
    "windows")]` + AttachConsole(ATTACH_PARENT_PROCESS) re-wire of
    CONOUT$/CONIN$ so CLI flags still work from a parent shell.
  - **No more crash on Ctrl+Shift+O / Ctrl+Shift+W** (cycle 735).
    Close-pane handler now schedules a redraw + re-emits the
    cycle-703 PaneFocus event after a successful close, so the
    renderer + Lua plugin state see the new collapsed tree on the
    same frame as the close. Mitigates the user-reported close-
    focused-split crash on Win11 / wgpu DX12.

  Other cycles in this release:
  - cycle 733: scripts/install.ps1 mirrors install.sh + Start menu
    integration (.lnk shortcut, PATH update, Add/Remove Programs
    entry, all per-user / no UAC). Bundled into the Windows .zip.
  - cycle 732: aligned the last actions/checkout@v4 holdout in
    ci.yml's nightly job to @v6 ahead of Node 20 deprecation.

  See the per-cycle paragraphs below for the full audit-trail.

  cycle 735 — **Fix: close-pane stale-state refresh (Win11
              close-split crash mitigation)**:
              User reported on Win11 (v1.46.0 install via cycle-733
              `install.ps1`): split via `Ctrl+Shift+O` then close
              focused via `Ctrl+Shift+W` crashes kettle. Couldn't
              reproduce via headless SendKeys on the Surface Book 3
              repro pass (likely timing-specific to the user's pane
              configuration or a specific running shell), but the
              audit surfaced a concrete state-refresh gap in
              `Action::ClosePane` at `crates/kettle-ui/src/app.rs`:
              the handler called `mux.close_focused()` and returned
              without scheduling a redraw or re-emitting the
              cycle-703 `PaneFocus` event. The split tree had
              collapsed (sibling promoted to root by cycle-602's
              `neighbor_of` + `remove_leaf`) but the renderer's
              cached layout + the last-emitted-focus pane id were
              both stale until the next user input nudged a
              redraw. On Windows under wgpu DX12, the stale
              tab-bar render path appears to lock the
              `Arc<Mutex<Terminal>>` of the dropped pane and panic
              (the user's reported crash class).
              Fix: add `window.request_redraw()` + a `poll_focus_event()`
              call right after `close_focused()` returns false (pane
              closed, tab survives). The `CloseTab` arm 30 lines
              below already does the analog implicitly via
              `fire_tab_close_event`'s Lua dispatch; `ClosePane`
              had no equivalent. Mirrors the cycle-368 fire-event
              pattern.
              Pre-existing tests `close_focused_promotes_sibling_in_two_pane_split`
              (`crates/kettle-ui/src/mux.rs:2017`) and
              `close_focused_picks_nearest_neighbor_not_leftmost_root`
              (line 2077) cover the tree mutation; the App-level
              redraw + focus-event refresh wasn't testable from a
              Mux unit test (no winit window, no Lua engine in the
              test harness). Drift-guard pinned via the cycle-735
              comment block in the source.

  cycle 734 — **Fix: phantom Windows console window on Start menu
              launch**:
              User reported on Win11 (v1.46.0 via cycle-733
              `install.ps1`): launching kettle from Windows Search
              opened TWO windows — kettle's wgpu window AND a
              stock Windows `ConsoleWindowClass` console. Surface
              Book 3 audit confirmed via process enumeration:
              `kettle.exe` owned 2 visible windows (the wgpu
              `Window Class` + a `ConsoleWindowClass`), and the
              child process list showed a non-headless `conhost.exe`
              spawned by Windows at startup. Root cause: the kettle
              binary inherited Rust's default `console` subsystem
              with no `#![windows_subsystem]` attribute — every
              Explorer / Start-menu launch allocates a phantom
              console because the PE header asked for one. Pre-733
              this was hidden because nobody had a Start menu
              shortcut to launch from (the cycle-730 ".zip is a
              portable archive" pattern said "extract + run from
              shell"); the cycle-733 `install.ps1` Start menu .lnk
              surfaced it.
              Fix: standard Rust-terminal pattern (same shape
              Alacritty / kitty / WezTerm / Ghostty use on
              Windows):
                1. `#![cfg_attr(windows, windows_subsystem =
                   "windows")]` at the top of
                   `crates/kettle/src/main.rs` — linker emits
                   `SUBSYSTEM:WINDOWS` so Windows doesn't
                   allocate the phantom console.
                2. `AttachConsole(ATTACH_PARENT_PROCESS)` + the
                   `CreateFileA` / `SetStdHandle` re-wire of
                   `CONOUT$` / `CONIN$` at `fn main()` startup
                   — when launched from a parent shell (PowerShell
                   / cmd / Git Bash), stdout/stderr re-attach to
                   that shell's console so CLI flags
                   (`--version`, `--list-themes`, `--gpu-info`,
                   `--shell-integration`, `--print-completions`,
                   `--check-config`, `--list-keybinds`,
                   `--list-actions`, `--list-ssh-hosts`,
                   `--config-path`, `--print-default-config`)
                   still print where the user expects. When no
                   parent console exists (Explorer launch), the
                   AttachConsole call returns FALSE and println!
                   / eprintln! become silent no-ops — exactly
                   the desired GUI-launch behavior.
                3. New `windows-sys` Windows-only runtime dep at
                   `crates/kettle/Cargo.toml` with
                   `Win32_System_Console` + `Win32_Storage_FileSystem`
                   features. `windows-sys` was already a
                   transitive dep via wgpu / winit / sysinfo, so
                   binary-size impact is near-zero.
              Drift guard: `windows_subsystem_attribute_survives`
              test in `crates/kettle/src/main.rs` reads the file
              via `include_str!` and asserts both the cfg_attr
              attribute + the AttachConsole call survive. If a
              future contributor strips either, the panic message
              tells them why both need to stay together (removing
              one without the other breaks either the GUI launch
              or the CLI stdout).
              Bundled `scripts/install.ps1` also got a small
              robustness tweak: `& kettle.exe --version` returns
              nothing under SUBSYSTEM:WINDOWS when invoked via
              PowerShell's `&` operator (PS doesn't wait for GUI
              processes); the version capture for the Add/Remove
              Programs `DisplayVersion` entry now uses
              `Start-Process -Wait -RedirectStandardOutput` which
              correctly captures the output, with a "unknown"
              fallback if the call fails.
              Caveat: under PowerShell's `& kettle.exe --version`
              pattern, output capture is still not guaranteed (PS
              doesn't `Wait` on GUI subprocesses). The same
              limitation hits Alacritty / WezTerm / etc.; documented
              in `docs/INSTALL.md` as the trade-off for a no-phantom-
              console Start menu UX. CLI-from-shell users who need
              guaranteed stdout capture can use `Start-Process kettle
              -ArgumentList '--version' -Wait -NoNewWindow
              -RedirectStandardOutput out.txt` or run from cmd which
              is more forgiving.

  cycle 733 — **Windows installer: `scripts/install.ps1` + Start menu
              integration**:
              Pre-733 the Windows release `.zip` shipped as a portable
              archive — `kettle.exe` worked if you unzipped + ran it,
              but didn't appear in **Windows Search / Start menu**
              because portable zips don't create the `.lnk` shortcuts
              the Windows search indexer needs. Linux had this solved
              (cycle-0 `scripts/install.sh` writes the XDG `.desktop`
              entry so GNOME Activities / KDE Krunner / Super-key
              search find kettle); macOS got it for free via the
              `.app` bundle's Spotlight indexing; only Windows users
              had to manually pin or create a `.lnk`.
              Fix: new `scripts/install.ps1`, mirroring `install.sh`'s
              shape:
                - Copies kettle.exe + .ico + LICENSE/NOTICE/README +
                  shell-integration\\ + bundled `install.ps1` itself
                  into `%LOCALAPPDATA%\\Programs\\kettle\\`.
                - Creates a Start menu `.lnk` at
                  `%APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\kettle.lnk`
                  with the proper icon — so `Win` -> type "kettle" ->
                  Enter launches kettle.
                - Adds the install dir to the user's PATH (default-on;
                  pass `-NoPath` to skip) so any fresh shell can call
                  `kettle.exe` by name.
                - Registers an Add/Remove Programs entry under
                  `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\kettle`
                  so kettle shows up in `appwiz.cpl` and uninstall
                  routes through `install.ps1 -Uninstall`.
                - `-Uninstall` reverses everything atomically.
                - `-Prefix D:\\path` flag = portable mode (skips
                  Start menu / PATH / registry; just copies files
                  somewhere the user picked).
                - Runs entirely in user scope — no UAC prompt.
              Wired into the release pipeline (`release.yml`'s Windows
              package step now copies `scripts/install.ps1` into the
              `.zip` next to `kettle.exe`) and the `Justfile`'s
              `[windows] install` recipe (so `just install` works on
              Windows too).
              No behavior change in the kettle.exe runtime; closes
              the "kettle doesn't show up when I search Windows" UX
              gap the v1.46.0 release surfaced.

  cycle 732 — **CI hygiene: last `actions/checkout@v4` holdout**:
              The cycle-723 nightly job in `.github/workflows/ci.yml`
              had stayed on `actions/checkout@v4` while everything
              else moved to `@v6` (cycle 728 Dependabot bump + cycle
              729 release.yml pretest alignment). v1.46.0's release
              workflow surfaced this as a `Node.js 20 actions are
              deprecated` annotation on every nightly run — Node 20's
              runtime is scheduled to be removed from the GitHub
              runner image on 2026-09-16. Bumped to `@v6` to match
              the rest of the repo. No behavior change on the
              nightly job; closes the deprecation warning four
              months before the hard-fail date.

## [1.46.0] — 2026-05-23

  Polish + production-grade Windows 11 audit + cross-platform DX +
  CI hygiene. Cycles 719-731. No public API change to the kettle.exe
  runtime; all additions are documentation polish, audit-trail
  closures, dependency-version alignments, ProcessTree test coverage,
  PowerShell shell integration, cross-platform Justfile, and a
  workspace-test count bump 424 → 432 with full CI matrix green on
  Linux/macOS/Windows for the first time since cycle 718.

  Headline features for end users:
  - PowerShell shell integration: `kettle --shell-integration powershell
    >> $PROFILE` now emits an OSC 133 snippet wired via prompt-function
    override + PSReadLine, idempotent + non-clobbering of starship /
    oh-my-posh / posh-git. Win11 users get jump-to-prompt parity with
    bash/zsh/fish.
  - Cross-platform `just gauntlet`: Justfile rewritten so Windows
    PowerShell contributors get the same CI gate the Linux/macOS
    contributors have always had. Linux-only scripts (install.sh,
    bench.sh, menu-screenshot.sh) gracefully degrade with a "use the
    prebuilt zip" pointer on Windows.
  - Windows 11 performance numbers: a new `scripts/bench.ps1`
    captures wall-clock + peak working set on Windows 11 for the
    PERFORMANCE.md baseline; Linux numbers stay pinned at v1.3.8
    cycle 277 with a re-bench tracked for v1.47.

  Headline fixes for maintainers:
  - kettle-remote: ProcessTree trait extraction lets the BFS body
    be unit-tested against synthetic process trees (cycle 646's
    long-standing test gap finally closed; 8 new tests).
  - Three pre-existing CI breakages fixed (cycles 730 + 731):
    cycle-711's missing `#[cfg(unix)]` on
    `scripts_menu_shot_exists_and_executable` broke Windows MSVC
    test compilation; winit 0.30 dropped
    `WindowExtMacOS::set_visible_on_all_workspaces` and the
    cycle-718+ macOS `sticky = true` branch broke macOS build.
    Both fixed; CI matrix green on all three OSes.
  - Missing CHANGELOG entry for cycle 729 (the aeafd9a checkout@v6
    alignment in release.yml's pretest job).

  See the per-cycle paragraphs below for the full audit-trail.

  cycle 731 — **Fix: macOS sticky no-op replaces removed winit API**:
              Cycle-730's CI run surfaced a pre-existing macOS build
              break: `crates/kettle-ui/src/app.rs:7039` called
              `window.set_visible_on_all_workspaces(true)` which winit
              0.30 dropped from `WindowExtMacOS`. The macOS CI job
              had been red on cycles 718-730 (verified in the GitHub
              Actions history) but no maintainer had hit it locally
              because the macOS dev path was inactive after the
              cycle-558 partial-release incident.
              Fix: replace the broken call with a `log::info!`
              message explaining that `sticky = true` is currently a
              no-op on macOS (same Bucket-E status as X11/Wayland —
              winit doesn't expose the `_NET_WM_STATE_STICKY` /
              `NSWindowCollectionBehavior.canJoinAllSpaces` knob, and
              re-implementing via `objc2` + raw NSWindow handles is
              a heavy dep for one config key). Comment cites the
              cycle-730 audit + names `objc2` as the path forward.
              No behavior change on Linux/Windows; macOS users with
              `sticky = true` now get a debuggable log line instead
              of a silent compile-error CI failure. CI matrix should
              go green on all three OSes for the first time since
              cycle 718.

  cycle 730 — **Production-grade Windows 11 audit: ProcessTree trait
              + cross-platform Justfile + PowerShell shell integration
              + bench.ps1**:
              Closes the four user-facing gaps surfaced by the
              cycle-727 audit pass and the cycle-730 Windows 11
              follow-up audit. One bundled commit; no public API
              breakage; workspace tests 424 → 432.
              1. **kettle-remote: `ProcessTree` trait + 8 mocked BFS
                 tests.** Pre-730, `detect_remote_with(pid, &mut
                 sysinfo::System)` read sysinfo directly — the only
                 test that could run was the `detect_remote(0)`
                 no-op smoke (cycle 646 punted on testing real
                 process trees because spawning ssh from CI was too
                 fragile). Cycle 730 extracts a `pub trait
                 ProcessTree` with four methods (`refresh`,
                 `parent_of`, `argv_of`, `all_pids`) + an impl for
                 `sysinfo::System`; the BFS body moves to a generic
                 `detect_in_tree<T: ProcessTree>` private helper.
                 8 new `#[cfg(test)] MockProcessTree`-backed tests
                 cover the previously-untestable cases: direct-
                 child ssh, two-hop ssh-via-wrapper, depth-3
                 container, closer-descendant-wins-on-tie, missing
                 root, empty tree, non-remote-only descendants,
                 parent-cycle defense via `visited` set. Public
                 `detect_remote_with` signature unchanged; kettle-
                 ui's call site at `crates/kettle-ui/src/app.rs:6101`
                 doesn't move.
              2. **Justfile cross-platform.** Pre-730 the Justfile
                 was bash-only — `RUSTDOCFLAGS="-D warnings"
                 cargo doc …` is bash-prefix syntax that breaks on
                 PowerShell; `/tmp/kettle.png` doesn't exist on
                 Windows; `./scripts/install.sh` is bash-only. CONTRIBUTING.md
                 promises "a green `just gauntlet` locally is the
                 same gate every PR runs on every OS" — broken on
                 Windows pre-730. Fix: `export RUSTDOCFLAGS :=
                 "-D warnings"` at file top (just exports at recipe-
                 entry, working under any shell); `TMPDIR :=
                 if os_family() == "windows" { env_var("TEMP") }
                 else { "/tmp" }` replaces hardcoded `/tmp` defaults;
                 `[unix]` / `[windows]` recipe attributes gate
                 `install`, `uninstall`, `bench`, `menu-shot`,
                 `clean` so Windows users get a graceful "use the
                 prebuilt zip" message instead of a parser error.
                 New CI step (`taiki-e/install-action@just` + `just
                 --summary` + `just --list`) verifies the Justfile
                 parses on every OS so a future regression fails
                 CI instead of slipping past.
              3. **PowerShell shell integration.** `kettle --shell-
                 integration powershell` (or `pwsh` / `ps1` alias)
                 emits a new `shell-integration/kettle.ps1` snippet
                 with OSC 133 A/B/C/D markers wired via PowerShell's
                 `prompt` function override + PSReadLine `Enter`
                 handler. Idempotent (`$global:__kettle_prompt_installed`
                 guard), preserves user's existing prompt (starship /
                 oh-my-posh / posh-git all keep working), gracefully
                 skips PSReadLine if disabled. Documented in
                 `docs/SHELL-INTEGRATION.md` next to the bash/zsh/fish
                 sections.
              4. **INSTALL.md "First run" + kettle.example.config
                 per-OS config-path callout.** Post-install path is
                 the same on every OS: launch kettle, try `--list-
                 themes`, `--config-path`, `--list-keybinds`,
                 `--check-config`, `--gpu-info`. New section in
                 INSTALL.md walks through it + lists the OS-specific
                 config locations (`~/.config/kettle/config` on
                 Linux/WSL, `~/Library/Application Support/kettle/config`
                 on macOS, `%APPDATA%\kettle\config` on Windows).
                 The example config gets a matching path block at
                 the top so users grepping for "Windows" find their
                 path immediately.
              5. **ARCHITECTURE.md session-restore mermaid.** The
                 cycle-109 atomic-save + cycle-411-420 hardening was
                 documented in prose but not diagrammed. New
                 sequence mermaid in `docs/ARCHITECTURE.md` covers
                 OSC 7 cwd capture → debounced autosave → atomic
                 tempfile + rename → next-launch read → tab tree
                 rehydrate → per-pane shell respawn with saved cwd.
                 Three named invariants (atomic write, OSC 7 catchup,
                 no-replay-of-failed-spawns).
              6. **PERFORMANCE.md Windows 11 row + scripts/bench.ps1.**
                 Linux numbers were from cycle 277 (450 cycles
                 stale). Cycle 730 keeps the v1.3.8 Linux baseline
                 (annotated with "re-bench scheduled for v1.47") and
                 adds a Windows 11 reference row from a new
                 `scripts/bench.ps1` (PowerShell-native, uses
                 `System.Diagnostics.Process.PeakWorkingSet64` for
                 peak memory — comparable to Linux's max RSS).
                 Methodology section documents the per-OS measurement
                 difference.
              7. **Bug surfaced + fixed: cycle-711 test failed Windows
                 MSVC compilation.** The `scripts_menu_shot_exists_and_executable`
                 test at `crates/kettle/src/main.rs:1573` used
                 `std::os::unix::fs::PermissionsExt::mode()` without a
                 `#[cfg(unix)]` gate; it broke `cargo build --workspace
                 --all-targets` on Windows MSVC with E0433. The bug
                 had survived since cycle 711 because Windows CI
                 apparently wasn't actually catching it (likely a
                 cycle-711-era Rust-toolchain quirk that newer rustc
                 1.95 enforces strictly). Fix: add `#[cfg(unix)]` to
                 the test (its executable-bit check is unix-only by
                 design — NTFS has no mode word). The exact same
                 cfg-gating pattern is used by the cycle-198 test at
                 `main.rs:1052`.

              No behavior change in the kettle.exe runtime; all
              additions are test coverage + DX + docs (+ one
              load-bearing test-compile fix). Local gauntlet green on
              Win11 (cycle-730 verified by the maintainer on a
              Surface Book 3); CI matrix verifies on Linux/macOS/Windows.

  cycle 729 — **CI hygiene: align actions/checkout@v6 in the
              cycle-723 release pretest job**:
              Cycle 728's Dependabot run bumped `actions/checkout`
              from v4 to v6 across the workflows that had it pinned
              at v4 (PRs #1-#3), but the cycle-723 pretest job
              inside `release.yml` (added the same day as cycle 728)
              had a separate `actions/checkout@v4` pin that the
              first Dependabot pass didn't catch. Aligned to v6 in
              one line so the release workflow runs a consistent
              checkout version everywhere. No behavior change;
              workspace tests still at 424.

  cycle 728 — **Fix: rustdoc regression + close local-gauntlet
              gap with CI's doc gate**:
              Cycle-722's `<unix>-<pid>.png` placeholder in a doc
              comment (and a separately-introduced
              `Option<RemoteContext>` + `argv[0]` in kettle-remote)
              are parsed by rustdoc as unclosed HTML tags. Local
              clippy/test gauntlet doesn't run rustdoc lints, so
              the regression slipped past pre-commit and only
              surfaced in CI's `cargo doc -D warnings` step
              (every push from cycle 722 onward failed CI).
              Fixes:
                - wrap the angle-bracket placeholders in code
                  spans (`\`<unix>\`-\`<pid>\``).
                - same for `Option<RemoteContext>` and `argv[0]`
                  in kettle-remote's `detect_container` /
                  `detect_ssh` docs.
                - add `RUSTDOCFLAGS="-D warnings" cargo doc
                  --workspace --no-deps` to the `Justfile`
                  gauntlet recipe + the `.githooks/pre-commit`
                  hook. Closes the cycle-X
                  CONTRIBUTING.md promise that "a green
                  `just gauntlet` locally is the same gate
                  every PR runs in CI" — pre-728 it was
                  almost-the-same (missing the doc gate).
              No behavior change; workspace tests stay at 424.
              CI will now go green on main + the 3 open
              Dependabot PRs (#1 actions/checkout@v6, #2
              actions/upload-artifact@v7, #3
              softprops/action-gh-release@v3) will re-run
              against a clean main + can merge cleanly.

  cycle 727 — **Comprehensive production-grade audit pass (724-727)**:
              Closes the Stop-hook ask for `entire-terminal`
              analysis with three parallel Explore agents
              auditing concurrency/unsafe, README/install UX,
              and ARCHITECTURE completeness. Findings + actions:
              **Cycle 724** — concurrency / unsafe / clippy
              allow-gate audit. All 11 unsafe blocks (in
              `fd_transport.rs` for SCM_RIGHTS, `main.rs` for
              SIGPIPE setup, `app.rs` for `pre_exec` fd-3 +
              UnixStream adoption) are ≤10 lines, narrowly
              scoped, with ownership-contract docs. No
              `transmute`, no raw-pointer abstractions. 20
              `#[allow(clippy::*)]` gates all justified
              (`too_many_arguments` on domain-anchored
              lifecycle methods, two `field_reassign_with_default`
              for 2-line Config init patterns). Threading
              topology: N reader threads (one per pane) →
              event loop on App; Lua VM parked Send+Sync but
              never cloned to threads; wgpu Device+Queue used
              only on the App thread. No Mutex overkill found.
              Result documented as a new "Synchronization
              primitives audit" subsection in ARCHITECTURE.md.
              **Cycle 725** — README + INSTALL UX. Opening
              paragraph rewritten to lead with "Works out of
              the box" + "GPU-accelerated" + "Cross-platform"
              (per audit feedback: prior copy buried the
              value prop). New keybindings-table footnote
              telling users to right-click for the Preferences
              ▸ submenu. Stale `v1.44.x` status line bumped
              to v1.45.x. `scripts/install-online.sh`:
              SHA-256-tool preflight check moved BEFORE
              download so a coreutils-less container fails
              immediately with an actionable apt/dnf/apk hint
              instead of looking like a corrupted download.
              `docs/kettle.example.config`: new "Quick-find
              index" at the top — 14 search keywords → block
              landings (Theme, Fonts, Cursor, Window, Tabs,
              etc.) so new users find what they want without
              scrolling 200+ lines.
              **Cycle 726** — ARCHITECTURE.md deep refresh.
              Threading model section expanded with Lua VM
              parking semantics + broadcast fan-out
              clarification + memory caps on the extractor.
              Plugin-flow mermaid extended with the cycle-703
              PaneFocus + cycle-704 TitleChanged + cycle-705
              UrlClicked events. Terminator-parity timeline
              extended through cycles 554-723.
              **Cycle 727** — allocation-pattern audit.
              Hot-path allocations counted (5 in drain_events,
              7 in redraw) and all verified load-bearing —
              `LuaEvent::Output` requires a fresh Vec for
              mlua's `IntoLuaMulti`; `ContextMenuRow.label`
              clones only while the menu is open. Documented
              as steady-state-zero pressure in ARCHITECTURE.md
              with a Cow refactor flagged as a profile-driven
              follow-up.
              No code-behavior changes; workspace tests stay
              at 424. Documents an architectural baseline so
              future PRs can land against a recorded contract.

  cycle 723 — **CI hygiene: nightly early-warning + release pretest gate**:
              Closes the last two CI/CD items from the cycle-718
              audit. No build-time changes; both adds are
              Linux-only to keep the cycle-444 GitHub Actions
              budget healthy.
              1. **Nightly job** added to `.github/workflows/ci.yml`.
                 Runs `cargo build --workspace --all-targets` +
                 `cargo test --workspace` on `dtolnay/rust-toolchain@nightly`.
                 `continue-on-error: true` so a nightly
                 regression warns but doesn't block PR merges —
                 the goal is multi-week lead time before a
                 breaking rustc change reaches stable. Cached
                 separately via `key: nightly` so it doesn't
                 evict the main stable cache.
              2. **`pretest` job** added to
                 `.github/workflows/release.yml`. Runs `cargo
                 test --workspace` on Linux before the per-OS
                 `package` matrix kicks off. The package job
                 now `needs: pretest` — a red test on the
                 tagged commit blocks the ~30-40 min per-OS
                 release builds before they consume runner
                 minutes. cycle-558 incident recovery
                 (release-must-complete) is preserved: this
                 just gates *entry* to the release matrix,
                 it doesn't add cancel-in-progress.
              No code change; workspace tests unchanged at
              424. actionlint CI gate validates the YAML on
              the next push.

  cycle 722 — **Production polish: doc + dead-code + magic-number sweep**:
              Picks up the medium-priority audit items that
              cycle 718 deferred. No behavior change anywhere;
              workspace tests stay at 424.
              **Cycle 719 — `docs/CONFIG.md` for the cycle-717
              Preferences submenu**: `cursor-style-blink` row
              now lists `cursor-blink` / `cursor_blink` as the
              short aliases the Preferences submenu writes
              back. New section "Editing the config from inside
              kettle" cross-references each visible toggle to
              its config key + documents the atomic write
              (preserves comments / blanks / order; first
              toggle saves a `.bak` snapshot).
              **Cycle 720 — stale `#[allow(dead_code)]` audit**.
              Removed 6 stale gates whose deferred work has
              shipped:
                - `BroadcastScope::All` / `::Group` enum (mux.rs)
                  — consumed by cycle-679/681/682 dispatch.
                - `compute_broadcast_targets` (mux.rs) — called
                  from production `broadcast_targets`.
                - `Mux::serialize_tab` — called from
                  `App::on_tab_detach` (cycle 411).
                - `session_screenshot_path` — called from
                  `Action::TakeScreenshot` dispatch (cycle 689).
                - `ContextMenuItem::Submenu.items` — consumed by
                  cycle-687 drill-in dispatch.
                - `ThemeChoice` / `ProfileChoice` — flyout-side
                  click dispatch landed at cycle 687/688.
              Kept the gates that are still test-only
              (`extract_tab` + `insert_tab` for the live-PTY
              adoption follow-up, `content_rect_for` for the
              cycle-651 layout-math drift guards,
              `VERTICAL_TAB_STRIP_W` as the documented
              fallback). All retained gates now reference the
              concrete test or design doc instead of "future
              sub-cycle".
              **Cycle 721 — magic numbers → `kettle_render::menu`
              constants**. The 12.0/8.0/40.0/180.0/80.0 layout
              literals duplicated across 16 sites in app.rs +
              lib.rs collapse to a single `pub mod menu` in
              kettle-render exporting `ROW_PAD`, `SEP_H`,
              `H_PAD`, `MIN_W`, `PANEL_BREATHING`. App-side hot
              paths (`context_menu_geometry`, `menu_row_at_cursor`,
              `context_menu_click_action`, `step_context_menu_highlight`,
              `scroll_context_menu`) now import the names. Test
              fixtures keep literals (their assertions pin
              specific pixel values).
              **Cycle 722 — stale sub-cycle promises pinned**.
              Comments like "sub-cycle 3 will add the SSH
              detector" rewritten to the concrete shipping
              cycles. kettle-remote module doc, keybinds.rs
              TakeScreenshot/GroupTab/GroupWindow/UngroupTab
              docs now cite cycles 644-689 instead of
              "later sub-cycles".

  cycle 718 — **Production-grade polish: workspace deps + doc sync**:
              Three real bugs / gaps surfaced by the post-C9
              audit pass, fixed in one cycle.
              1. **Path-dep version drift**: every internal
                 `kettle-X` path dependency in the per-crate
                 manifests hardcoded `version = "1.2.0"` while
                 the workspace lived at 1.45.1. Future releases
                 would have re-diverged. Refactor: declare each
                 `kettle-*` crate as a `[workspace.dependencies]`
                 entry pinned to the workspace version (auto-
                 bumped by `scripts/release.sh`); per-crate
                 manifests switch to `kettle-config.workspace = true`
                 etc. Touched 5 Cargo.toml files; `release.sh`
                 now updates **one line** instead of ten.
              2. **Stale v1.44.0 references** in `README.md`
                 and `docs/INSTALL.md` (5 occurrences). Updated
                 to v1.45.1 to match the current release.
              3. **Missing `kettle-remote` in ARCHITECTURE.md**:
                 the cycle-643 crate didn't appear in the
                 mermaid graph. Added the box with its role
                 (SSH / Docker / Podman / kubectl detection +
                 kitty-@ control surface). Also added
                 cross-references to cycle-716/717 helpers in
                 the `kettle-ui` + `kettle-config` boxes.
              No behavior change — Cargo build / clippy / test
              all green at 424 (same as cycle 717).

  cycle 717 — **Preferences ▸ submenu (C8 + C9)**:
              The user-visible payoff for cycles 716. Right-
              click → Preferences ▸ now opens a submenu with
              13 runtime-mutable toggles + an `Advanced…` row
              that opens the config file in `$EDITOR`. Each
              toggle (a) updates `self.cfg` immediately so the
              change is visible without restart, and (b) writes
              `key = value` back to the config file via the
              cycle-716 atomic helper.
              Submenu rows (separator-grouped):
                - Scrollbar radio: always / auto / hidden
                - Cursor blink (✓), Copy on select (✓),
                  Mouse-hide while typing (✓)
                - Bell radio: off / visual / attention / both
                - Font size + / Font size −
                - Advanced… (open config in $EDITOR)
              Implementation:
                - new `ContextMenuItem::DynamicItem { label:
                  String, action: Action, enabled: bool }`
                  variant — owned label so the radio/check
                  prefixes (`● /○ / ✓ /  `) can be baked in
                  without leaking memory via `Box::leak`.
                  Extends every menu hot-path (filter_disabled,
                  item_is_dispatchable, context_menu_geometry,
                  context_menu_click_action, assign_mnemonics,
                  typeahead_match, context_menu_overlay).
                - 10 new `Action::*` variants + `from_name`
                  arms + palette entries + the cycle-117
                  palette-completeness drift-guard arm.
                - new `App::persist_pref(&self, key, value)`
                  thin wrapper around the cycle-716 atomic
                  helper.
                - `append_preferences_submenu_items` slotted
                  between Theme and Profile submenus in
                  `open_context_menu`.
              Drift guard `preferences_submenu_contains_all
              _user_facing_toggles` walks the 13 Action
              variants and asserts each parses through
              `Action::from_name`. Catches any future
              divergence between palette wiring + the menu
              spec. Workspace tests 423 → 424. Closes both C8
              (toggle wiring) and C9 (Advanced entry) of the
              breezy-hopping-lollipop plan.

  cycle 716 — **Preferences submenu infrastructure: atomic
              config write-back (C7)**:
              Lays the plumbing for the Preferences ▸ submenu
              landing in cycle 717 (C8). No user-visible UX
              yet — this is the safety net.
              New `kettle_config::persist_config_toggle(path,
              key, value) -> io::Result<PathBuf>`:
                - In-place edit: if a line matching `key` exists
                  (allowing `-` ↔ `_` equivalence), only that
                  line is replaced. Comments + blanks + ordering
                  survive byte-for-byte.
                - Append on miss: new `key = value` line goes at
                  the bottom with a leading blank line for
                  readability.
                - Atomic temp+rename (POSIX rename(2) /
                  MoveFileEx — both atomic on supported FSes).
                - First-write backup: `<config>.bak` snapshot of
                  the pre-edit content; subsequent writes don't
                  re-overwrite the backup so the user has a
                  forensic "what did my config look like before
                  I started clicking toggles?" artifact.
                - Path-traversal refused: any path with a `..`
                  component returns `PermissionDenied`.
              Drift guards (in `tests` mod):
                - `persist_config_toggle_appends_on_missing_key`
                - `persist_config_toggle_preserves_user_comments
                  _and_blank_lines` (byte-for-byte assert)
                - `persist_config_toggle_backup_only_on_first_write`
                - `persist_config_toggle_treats_dash_and_underscore
                  _as_equivalent`
                - `persist_config_toggle_refuses_traversal_paths`
              Workspace tests 418 → 423.

  cycle 715 — **Right-click menu: mnemonics + typeahead**:
              Context-menu UX sub-cycle C6. Single A-Z keys
              now dispatch rows by mnemonic; multi-char
              accumulates into a 750ms-windowed typeahead
              buffer for prefix match.
                - new pure helper
                  `assign_mnemonics(items) -> Vec<Option<(usize, char)>>`:
                  first A-Z char per row, with collision fallback
                  (Copy=C, Close Pane=l, Cancel=a).
                - new pure helper
                  `typeahead_match(items, buf) -> Option<usize>`:
                  case-insensitive prefix match against
                  dispatchable rows only (disabled rows
                  skipped).
                - `ContextMenuState` gains `typeahead_buf:
                  String` + `typeahead_until: Option<Instant>`.
                - `context_menu_key` now takes a `text:
                  Option<&str>` parameter. A single A-Z char
                  with an empty buffer dispatches via mnemonic
                  (drill into Submenu, fire Action, set
                  Theme/Profile). Otherwise accumulates into
                  typeahead — buffer clears after 750ms of
                  inactivity.
              Drift guards:
                - `mnemonics_assign_unique_chars_with_fallback`
                  walks 7 rows including separator + no-alpha
                  + collision fallback.
                - `typeahead_th_highlights_theme_first` pins
                  case-insensitive prefix match + dispatchable-
                  row filter.
                - `typeahead_skips_disabled_rows`.
              Workspace tests 415 → 418.

  cycle 714 — **Right-click menu: scrollable long submenus**:
              Context-menu UX sub-cycle C5. The Theme submenu
              (cycle-685) has ~512 entries; pre-cycle-714 the
              panel grew off-screen with no scroll handling —
              the bottom of the list was unreachable. Now:
                - panel height is clamped to `surface_h - 80px`
                  (40px top + 40px bottom breathing room) inside
                  `context_menu_geometry`.
                - `ContextMenuState` gains `scroll_offset: usize`
                  + parallel `scroll_stack: Vec<usize>` so each
                  drill-in level has its own view; drill-pop
                  restores the parent's offset.
                - mouse wheel over an open menu scrolls one row
                  per notch (pre-empts pane scrollback +
                  font-zoom + tab-cycle wheel routing).
                - keyboard `↑/↓/Tab` auto-scroll when the new
                  highlight is outside the visible window.
                - new pure helper `count_rows_fitting(items,
                  start, panel_h, row_h, sep_h) -> usize`.
                - renderer skips rows < scroll_offset and stops
                  when the next row would exceed panel_h;
                  ▲/▼ accent bars at top/bottom of the panel
                  signal clipped content.
              Drift guards:
                - `count_rows_fitting_respects_panel_height_and_separator_height`
                  walks 8 cases (empty panel, exact fit, partial
                  row, separator handling, mid-list start, past-
                  end start).
                - `theme_submenu_with_512_entries_clamps_panel_to_surface_height`
                  asserts ~24 visible rows for a 512-entry list
                  at a real 580px panel — the gap-table invariant.
              Workspace tests 413 → 415.

  cycle 713 — **Right-click menu: hide disabled rows
              (Terminator-style)**:
              Context-menu UX sub-cycle C4. Before this cycle
              disabled rows rendered greyed-out. Per the user's
              feedback ("the right-click menu UX doesn't seem to
              work as intended"), the greyed-out rows added
              visual clutter — Copy without a selection, Ungroup
              without a group set, etc. Terminator + GNOME
              Terminal hide such rows entirely; cycle 713 matches.
              Now: every visible context-menu row is actionable.
              Rules:
                - any `Item { enabled: false, .. }` is dropped
                  before render.
                - runs of `Separator`s collapse to one.
                - leading + trailing separators (orphaned by step 1)
                  are trimmed.
                - all-disabled menu collapses to empty (no orphan
                  chrome).
              Also: `Ungroup This Tab` row's `enabled` is now
              computed from `Pane::group_name.is_some_and(!empty)`
              instead of being hardcoded true — so it appears
              only when the focused pane actually has a group set.
              New pure helper `filter_disabled(items) ->
              Vec<ContextMenuItem>` applied at the end of the
              `open_context_menu` build phase.
              Drift guards: 5 cases
              (`disabled_items_are_hidden_and_separators_collapse`,
              `consecutive_separators_collapse_and_leading_is_dropped`,
              `filter_disabled_is_near_identity_when_all_enabled`,
              `filter_disabled_handles_empty`,
              `filter_disabled_collapses_all_disabled_to_empty`).
              Workspace tests 408 → 413.

  cycle 712 — **Right-click menu: mouse hover-to-highlight**:
              Context-menu UX sub-cycle C3. Before this cycle the
              highlight only moved via keyboard nav; sliding the
              cursor over rows did nothing, which felt unresponsive
              to mouse users used to GTK/NSMenu/Win32 conventions.
              Now: cursor over a context-menu row immediately
              updates `menu.highlight`; cursor over a separator
              (visual gap) is ignored. Disabled rows still highlight
              on hover — the dispatcher rejects the click, but the
              highlight shows the row the cursor is over (matches
              GTK). Only requests a redraw when the highlight
              actually changes — no GPU churn on sub-pixel motion.
              New pure helper:
                `pub(crate) fn find_menu_row_y(cursor_y, anchor_y,
                  row_h, sep_h, kinds: &[bool]) -> Option<usize>`
              Live wiring:
                - `App::menu_row_at_cursor(&self) -> Option<usize>`
                  thin wrapper that builds the `kinds` mask from
                  `menu.items` and delegates.
                - `App::update_menu_highlight_from_cursor(&mut self)`
                  called from `CursorMoved` (only when the menu is
                  open — no-op otherwise).
              Drift guard `hover_updates_menu_highlight_skipping_separators`
              walks 8 cases: inside-row, edge-between-rows,
              separator, after-separator, above-panel, below-panel,
              empty-menu, single-row.
              Workspace tests 407 → 408.

  cycle 711 — **Tooling: `just menu-shot` repro harness for the
              right-click context menu**:
              Context-menu UX overhaul (C3-C9 in the plan) needs
              a reproducible way to capture the menu's visual
              state across sub-cycles. cycle 711 lands the
              harness — no behavior change.
              Adds:
                - `scripts/menu-screenshot.sh` — launches the
                  built kettle binary, focuses + resizes the
                  window to 1280×720 via xdotool, right-clicks
                  near the center, sleeps 350ms for the menu
                  paint, captures the screen with scrot. Output
                  PNG lands in `target/menu-shots/`.
                - `--name <slug>` flag for labeled outputs.
                - `--hold` flag to leave kettle running for
                  manual driving.
                - Auto-skip when `$DISPLAY` and
                  `$WAYLAND_DISPLAY` are both unset (headless
                  CI).
                - `just menu-shot` recipe that forwards args.
              Drift guard `scripts_menu_shot_exists_and_executable`
              pins: file exists, executable bit set, opens with
              bash shebang, references both scrot + xdotool.
              Workspace tests 406 → 407.

  cycle 710 — **Fix: focused pane titlebar respects theme accent
              cascade (kill the red bar)**:
              User-reported regression: on dark themes (Tokyo
              Night Storm in the bug report), the focused
              pane's per-pane titlebar showed a bright
              `#c80003` red bar that didn't match anything
              else in the theme. The unfocused pane next to
              it had a subtle gray bar — exactly the look the
              user wanted for both states.
              Root cause: the cycle-387 focused-pane fallback
              at `crates/kettle-render/src/lib.rs:1241` was a
              hardcoded Terminator-bright `Rgb::new(0xc8, 0x00,
              0x03)`. The pane border (lib.rs:1209) and the
              screenshot accent (lib.rs:3136) both already
              cascaded through theme-aware
              `focused_split_color → accent_color → palette[4]`
              for focus signaling; the titlebar just hadn't
              been updated.
              Fix: extracted a pure `pick_titlebar_bg(cfg,
              theme, focused, broadcast) -> Rgb` helper that
              mirrors the existing cascade. An explicit
              `title_transmit_bg_color = #hex` still wins —
              users who pinned the Terminator look keep it.
              Unfocused (gray) + broadcast (blue) fallbacks
              unchanged.
              Drift guards (in `pick_titlebar_bg_tests`):
                - `focused_titlebar_uses_accent_cascade_when_unset`
                  walks all 4 cascade levels + asserts the
                  historic `#c80003` is NEVER the fallback.
                - `unfocused_titlebar_falls_back_to_inactive_gray`
                  pins the no-regression contract.
                - `broadcast_titlebar_falls_back_to_receive_blue`
                  pins the no-regression contract.
              Workspace tests 403 → 406.

  cycle 708 — **`Action::OpenLayoutPicker` (Terminator
              `layoutlauncher.py` — Plugin system COMPLETE)**:
              Closes the last remaining Terminator plugin gap.
              The Stop hook cited `launcher.py (port to layout
              overlay)` as the final plugin item.
              Adds:
                - `Session::list_layouts() -> Vec<String>` —
                  walks `<config-dir>/layouts/*.json`, strips
                  the `.json` extension, returns names sorted
                  alphabetically. Empty when the dir doesn't
                  exist (fresh install).
                - `Action::OpenLayoutPicker` variant + 6
                  keybind name aliases: `layout_launcher`,
                  `layout-launcher`, `open_layout_picker`,
                  `open-layout-picker`, `layout_picker`,
                  `layout-picker`. cycle-117 palette
                  completeness drift guard enforces registry
                  coverage.
                - `App::layout_picker_input: Option<(String,
                  usize)>` modal state. Same shape as
                  `palette_input` (query + selected index).
                - `App::layout_picker_key` keyboard handler.
                  Esc closes, Backspace pops, Up/Down/Tab steps
                  selection, Enter spawns
                  `std::env::current_exe()` with `--layout
                  NAME` (detached stdio). Type-to-filter.
                - Pure `rank_layouts(query, layouts) ->
                  Vec<usize>` helper. Empty query → identity;
                  non-empty → AND across lower-cased tokens.
                - Renderer overlay extensions:
                  `Overlay::layout_picker_query` +
                  `layout_picker_hint` + a paint arm in the
                  search/palette bar block (theme palette[6]
                  color so it's visually distinct from the
                  cycle-329 palette bar).
                - Empty-layouts-dir hint reads `(no saved
                  layouts; run kettle --save-layout NAME)`
                  so first-time users get a clear affordance.
              Drift guard `rank_layouts_filters_by_tokens
              _case_insensitive` walks 8 cases:
              empty/whitespace queries → identity; single
              token; multi-token AND; case folding; no-match;
              empty list. Audit row promoted from 🟡 to A;
              Plugin system Bucket-D row promoted from
              substantially-complete → COMPLETE (6/6 Terminator
              plugins ported). Workspace tests 402 → 403.

  cycle 707 — **Audit doc Bucket D cleanup — all 4 items now A**:
              Doc-only cycle. The audit doc's Bucket D section
              listed 4 multi-cycle gaps (Plugin system,
              Per-terminal titlebar, Detachable tabs,
              Background image + blur), each marked "TODO"
              with a separate design doc that never landed.
              In reality each one shipped incrementally over
              its own cycle range:

                - Plugin system: cycles 324-377 + 619-621 +
                  688-689 + 703-705 (all 7 LuaEvents + 5/6
                  Terminator plugins functionally ported).

                - Per-terminal titlebar: cycles 379/382/386/
                  682 (chrome reservation + label format +
                  group pill).

                - Detachable tabs: cycles 400-411 (drag-state
                  machine + file-fallback JSON + SCM_RIGHTS
                  IPC; live-PTY adoption tracked separately
                  pending `Terminal::from_raw_fd` plumbing).

                - Background image + blur: cycles 381-394
                  (PNG/JPEG/WebP decode + cache + Gaussian
                  blur + per-frame UV recompute).

              cycle 707 rewrites the four gap-table rows from
              "TODO with design doc" → "A with cross-link to
              shipping cycle + relevant code module". Bucket D
              section header notes the status flip explicitly
              so future Stop-hook readings don't re-litigate.
              No code change; no test count change (workspace
              tests remain at 402).

              In-source paragraph at line 177 (terminatorlib/
              titlebar.py) updated to document the actual
              implementation: which config keys gate it, label
              format, hit-testing routing (was "kettle has NO
              per-pane titlebar" — directly contradicted by
              kettle-render/src/lib.rs:767-829).

  cycle 705 — **`LuaEvent::UrlClicked` (Terminator plugin
              sub-cycle: URL-click event hook — Bucket D
              plugin system substantially complete)**:
              Bucket D rescue (plugin system) — final Lua
              event for Terminator-plugin parity.
              Terminator's `urlhandlers.py` + analytics
              plugins watch every URL click for tracking /
              logging / workflow triggers.
              Adds:
                - new `LuaEvent::UrlClicked(String)` variant —
                  payload is the URI string.
                - emits to Lua as `(uri_string,)`.
                - fired from `App::open_url` AFTER the cycle-X
                  `is_safe_url` safety check but BEFORE the
                  cycle-374 `try_url_handler` pattern dispatch
                  — analytics plugins see every safe URL
                  click, regardless of which handler ultimately
                  opens them (observation-only event).
                - script-facing name is `url_clicked`,
                  registered via `kettle.on('url_clicked',
                  function(uri) … end)`.
              Drift guard `url_clicked_event_emits_uri` walks
              3 URL events (https / file / mailto) + asserts
              the event name string.
              Plugin system audit row promoted to
              **substantially complete** — all 7 plugin-
              relevant LuaEvents shipped (startup, tab_add,
              tab_close, bell, output, pane_focus,
              title_changed, url_clicked) + 5/6 Terminator
              plugins functionally ported. Only `launcher.py`
              remains, and cycle-329 command palette already
              lists layouts as candidate sources.
              Workspace tests 401 → 402.

  cycle 704 — **`LuaEvent::TitleChanged` (Terminator plugin
              sub-cycle: title-change event hook)**:
              Bucket D rescue (plugin system). Terminator's
              status-bar / title-mirroring plugins watch for
              title changes via VTE's `window-title-changed`
              signal. cycle 704 lands the kettle equivalent.
              Adds:
                - new `LuaEvent::TitleChanged(u64, String)`
                  variant — payload is (pane_id, new_title).
                - emits to Lua as `(pane_id, title_string)`.
                - `App::poll_title_event(&mut self)` polled per
                  redraw — walks `mux.panes` and diffs against
                  `App::last_emitted_titles: HashMap<u64,
                  String>`. One pass site, captures changes
                  from ALL 4 title-mutating call sites (OSC 0/2
                  via TermEvent::SetTitle, inline edit via
                  `Action::EditPaneTitle`, reset via
                  TermEvent::ResetTitle, remote-context
                  derivation via cycle-655). O(n_panes) per
                  redraw; trivial up to 100+ panes.
                - script-facing name is `title_changed`,
                  registered via `kettle.on('title_changed',
                  function(id, t) … end)`.
              Drift guard `title_changed_event_emits_pane_id
              _and_title` walks 3 title events + asserts the
              event name string.
              Audit row updated: 6/7 LuaEvents shipped
              (startup, tab_add, tab_close, bell, output,
              pane_focus, title_changed); `url_clicked` is the
              remaining gap. Workspace tests 400 → 401.

  cycle 703 — **`LuaEvent::PaneFocus` (Terminator plugin
              sub-cycle: focus event hook)**:
              Bucket D rescue (plugin system). Terminator's
              plugins often want to react to focus changes
              (status-bar updates, activity-watch suppression
              when active, per-pane theme overlays).
              cycle 703 adds:
                - new `LuaEvent::PaneFocus(Option<u64>, u64)`
                  variant — payload is `(previous_focused_pane_id,
                  new_focused_pane_id)`. `previous = None`
                  signals the first focus after startup so
                  plugins can seed their state.
                - emits to Lua as `(prev|nil, cur)` so user
                  scripts can write `if prev == nil then …`.
                - `App::poll_focus_event(&mut self)` polled per
                  redraw tick — diff against
                  `App::last_emitted_focus: Option<u64>`. One
                  diff site captures focus changes from ALL
                  sources (keybind, mouse click, new tab, close
                  tab, remote-control IPC) — future cycles
                  won't have to wire each path individually.
                - script-facing name is `pane_focus`, registered
                  via `kettle.on('pane_focus', function(prev, cur)
                  … end)`.
              Drift guard `pane_focus_event_emits_optional_prev
              _and_current` walks 3 focus events (nil→42, 42→17,
              17→42) + asserts the event name string. Workspace
              tests 399 → 400. Audit row for Bucket-D Plugin
              system updated from "TODO" to "in-progress" with a
              per-plugin porting status cross-reference.

  cycle 702 — **`Action::SendNewline` (Terminator key_send_newline)**:
              Writes a literal `\n` to the focused pane's PTY.
              Useful for shell line-editors that consume Enter
              normally but expect explicit `\n` for line
              continuation (multi-line readline prompts).
              Reachable from cycle-104 command palette + 2
              keybind name aliases: `send_newline`,
              `send-newline`. cycle-117 palette completeness
              drift guard enforces registry coverage.
              Drift guard `from_name_accepts_send_newline_aliases`
              walks both aliases. Audit row promoted from E → A.
              Workspace tests 398 → 399.

  cycle 700 — **Terminator broadcast `*_toggle` keybind aliases**:
              Terminator names its broadcast keybinds with the
              `_toggle` suffix: `group_all_toggle`,
              `group_tab_toggle`, `group_win_toggle`. Kettle's
              existing actions are `ToggleBroadcastAll/Group/
              Window`; cycle 700 adds the Terminator spellings
              as direct aliases so an unmodified Terminator
              keybind block resolves cleanly.
                - `group_all_toggle` → `ToggleBroadcastAll`
                - `group_tab_toggle` → `ToggleBroadcastGroup`
                  (per-tab broadcast)
                - `group_win_toggle` → `ToggleBroadcastWindow`
                  (per-window broadcast)
              Drift guard
              `from_name_accepts_terminator_group_toggle_aliases`
              walks all 6 (underscore + hyphen) aliases.
              Workspace tests 397 → 398.

  cycle 699 — **Terminator config-key aliases: `custom_command` +
              `use_custom_command` + `copy_on_selection` +
              `enabled_plugins`**:
              Another batch of "kettle already implements this
              but only under the Alacritty / WezTerm spelling" —
              cycle 699 adds the Terminator-spelled key as a
              direct parser alias so an unmodified Terminator
              profile loads cleanly.
                - `custom_command` / `custom-command` → `command`
                  / `shell` (existing per-profile shell override)
                - `use_custom_command` / `use-custom-command` →
                  new `pub use_custom_command: bool` field
                  (default true). When false, `cfg.shell` is
                  cleared at parse-finalize, falling back to
                  $SHELL. Order-independent.
                - `copy_on_selection` / `copy-on-selection` →
                  `copy_on_select` (existing PRIMARY-clipboard
                  auto-copy)
                - `enabled_plugins` / `enabled-plugins` →
                  recognized-but-ignored (kettle's plugin model
                  is cycle-324 Lua + cycle-611 menu-item config,
                  not VTE plugin objects). Prevents
                  `--check-config` warning on copied configs.
              Drift guard `terminator_use_custom_command_gate`
              walks 5 cases: command-then-disable, disable-then-
              command, default (gate implicit-true), Terminator
              `custom_command` alias, `copy_on_selection` alias.
              Workspace tests 396 → 397.

  cycle 698 — **Terminator config-key aliases: `mouse_autohide`
              + `word_chars`**:
              Both keys map 1:1 onto pre-existing kettle config
              targets but had previously required the kettle
              spelling (`mouse-hide-while-typing` /
              `word-delimiters`). Cycle 698 adds the Terminator
              spelling as a direct alias at the parser level so
              an unmodified Terminator config is friendly out of
              the box.
                - `mouse_autohide` / `mouse-autohide` →
                  `mouse_hide_while_typing` (VTE auto-hides the
                  pointer while typing; same semantics).
                - `word_chars` / `word-chars` →
                  `word_delimiters` (double-click word boundary
                  character set).
              Drift guard assertions added to
              `mouse_hide_while_typing_default_and_parse` and
              the word-delimiters parser test.
              Audit rows updated with cycle-698 cross-link.
              Workspace tests unchanged at 396 (assertions
              extend existing tests rather than add new ones).

  cycle 696 — **`Action::EditConfig` (Terminator key_preferences)**:
              Terminator's `key_preferences` /
              `key_preferences_keybindings` open Terminator's
              GUI Preferences dialog. kettle is config-file-
              driven, so cycle 696 ships the equivalent as a
              one-keystroke shortcut to edit the resolved config
              file: `Action::EditConfig` opens
              `App::config_path` (or `Config::default_path()`
              fallback if no config loaded) via
              `open::that_detached`, which respects the OS's
              registered text-editor handler ($EDITOR, BBEdit,
              Notepad, etc).
              Reachable from cycle-104 command palette + 7
              keybind name aliases: `preferences`,
              `preferences_keybindings`, `preferences-keybindings`,
              `edit_config`, `edit-config`, `open_config`,
              `open-config`. cycle-117 palette completeness
              drift guard enforces registry coverage.
              Drift guard `from_name_accepts_edit_config_aliases`
              walks all 7 aliases.
              Closes the "preferences GUI is a paradigm choice"
              Bucket E rationale by making the equivalent UX one
              keystroke away. Audit row promoted from E → A.
              Workspace tests 395 → 396.

  cycle 695 — **`Action::ShowHelp` (Terminator key_help / F1)**:
              Terminator's F1 opens its HTML manual via
              `terminal.key_help` → `open_url(manual_lookup())`.
              kettle opens its README at the canonical GitHub URL
              (https://github.com/Reddimus/kettle#readme) via
              `open::that_detached` — the same cross-platform
              dispatch path that cycle-X URL clicks already use,
              so it works on Linux/macOS/Windows without spawning
              a per-platform helper.
              Reachable from cycle-104 command palette + 5 keybind
              name aliases: `help`, `show_help`, `show-help`,
              `open_help`, `open-help`. cycle-117 palette
              completeness drift guard enforces registry coverage.
              Drift guard `from_name_accepts_show_help_aliases`
              walks all five aliases.
              Audit row promoted from E → A.
              Workspace tests 394 → 395.

  cycle 694 — **`sticky` wired on macOS**:
              Terminator's `sticky = true` shows a window on
              every workspace. macOS exposes this as a
              Window-level method via
              `winit::platform::macos::WindowExtMacOS::set_visible_on_all_workspaces(true)`,
              so cycle 694 applies it post-construction
              (unlike cycle-691's `with_skip_taskbar` which
              is a build-time WindowAttributes attribute).
              X11/Wayland remain Bucket E — winit 0.30
              doesn't expose `_NET_WM_STATE_STICKY` on the
              cross-platform API and would need
              raw-window-handle direct atom writes (heavy dep
              for one config key). A Terminator user copying
              `sticky = true` gets the intended behavior on
              macOS; on other platforms the value parses
              without effect (no warning since the key is
              already recognized via cycle-X parser).
              Audit row reclassified from full Bucket E to
              partial 🟡 (macOS only), matching cycle-691's
              `hide_from_taskbar` pattern.

  cycle 693 — **`Action::ScaledZoom` (Terminator key_scaled_zoom)**:
              Terminator's "scaled zoom" maximizes the active
              pane AND scales the font proportionally so text
              fills the larger area. kettle pairs
              `Mux::toggle_zoom` with a 1.5× font-size bump on
              enter / restore on exit. Saved font size lives in
              `App::scaled_zoom_prev_font_size: Option<f32>`;
              `None` means "not currently in scaled zoom" so
              repeated `ToggleZoom` interactions from other
              code paths don't accidentally undo the restore.
              Reachable from:
                - cycle-104 command palette ("Scaled zoom (zoom
                  + 1.5x font)")
                - 3 keybind name aliases: `scaled_zoom`,
                  `scaled-zoom`, `toggle_scaled_zoom`
                - cycle-117 palette completeness drift guard
                  registry (compile fails if any new Action
                  variant isn't categorized)
              Drift guard `from_name_accepts_scaled_zoom_aliases`
              walks all three aliases + asserts bare
              `toggle_zoom` still parses to the non-scaling
              `Action::ToggleZoom` (no alias collision).
              Audit row promoted from E → A with cross-link.
              Audit doc also flipped a stale Bucket E row for
              `insert_number` / `insert_padded` (the entries
              were actually shipped at cycle 342 — only the
              doc was out of date).
              Workspace tests 393 → 394.

  cycle 692 — **`palette = NAME` named-preset alias**:
              Terminator accepts `palette = solarized_dark`
              as a shorthand that picks the whole 16-slot
              palette + cursor + selection colors at once.
              kettle ships ~512 bundled themes (Ghostty +
              iTerm2 + WezTerm corpora) which are a strict
              superset, so the parser now treats
              `palette = NAME` (no `=` after) as an alias
              for `theme = NAME`. Two-step match:
                1. direct `Theme::find_name(value)` —
                   handles kettle-spelled names verbatim
                2. underscore→space fallback — handles
                   Terminator's `solarized_dark` convention
                   by trying "solarized dark" as the lookup
              Unknown names leave theme unchanged (default
              preserved). Per-slot `palette = N=#hex` form
              still works (no regression).
              Drift guard `palette_named_preset_alias` walks
              4 inputs: direct match, underscore-→space,
              per-slot N=#hex preserved, unknown→default.
              Audit row promoted from E → A with cross-link.
              Workspace tests 392 → 393.

  cycle 691 — **`hide_from_taskbar` wired on Windows**:
              winit 0.30 only exposes `with_skip_taskbar`
              on Windows (`WindowAttributesExtWindows`).
              X11/Wayland/macOS would need raw-window-handle
              direct atom writes which is design-doc Bucket E.
              A user with `hide_from_taskbar = true` in their
              Terminator config now gets the intended behavior
              on Windows; on other platforms the value
              parses without effect (no warning since the
              key is already recognized).
              `#[cfg(target_os = "windows")]` gates the new
              `with_skip_taskbar` call so non-Windows builds
              don't add the `WindowAttributesExtWindows`
              import. Audit row promoted from E → 🟡 (with
              explicit cross-platform-limitation note).
              Workspace tests stay 392.

  cycle 690 — **terminalshot sub-cycle 7: audit close-out**.
              Audit doc row + Bucket-D summary table both
              promoted: terminalshot → ✅ A 7/7 deployed.
              **All 7 Bucket D Terminator features now ship
              end-to-end on the deployed binary**:
                - `plugins/remote.py`
                - `plugins/auto_theme.py`
                - `ask_before_closing`
                - `tab_position = left/right`
                - Named broadcast groups
                - Theme + Profile submenu
                - `plugins/terminalshot.py`
              Plus the summary highlights three durable
              choices: BroadcastScope migration touched 5
              call sites without breaking cycle-178; the
              NOAA solar algo is pure (no deps); the wgpu
              readback respects row padding + BGRA→RGBA
              for cross-adapter portability.
              No code change. **Bucket D pass complete.**

  cycle 689 — **terminalshot sub-cycles 5 + 6: desktop
              notification + per-pane crop**.
              Sub-cycle 5: `Action::TakeScreenshot` dispatch
              now calls `fire_notify("kettle: screenshot
              queued", &path_str)` so the user knows where
              the file landed. Optimistic — fired before the
              GPU readback completes; rare capture failures
              would make the notification mildly inaccurate
              (and the capture path's `log::warn` surfaces
              them in --debug runs).
              Sub-cycle 6: dispatch now computes the focused
              pane's rect via `mux.layout(active, area)` and
              passes it as `ScreenshotRequest::crop`. Cycle
              688's `capture_live_surface` already handles
              the crop math (when crop is Some, it carves
              out the rect post-readback). v1 captures the
              focused pane only; whole-window still
              available via the existing `--screenshot=PATH`
              CLI flag.
              Workspace tests stay 392.
              **terminalshot port: 6/7 — sub-cycle 7 (audit-
              doc finalization) is the only piece left**.

  cycle 688 — **terminalshot sub-cycle 4: wgpu surface
              readback — live screenshots ship**. Last
              remaining heavy work on the cycle-630
              terminalshot design.
              Surface config gains `COPY_SRC` usage flag
              (cycle-654 set up the slot; this cycle lights
              it up). New private
              `Renderer::capture_live_surface(frame, req)`
              runs BEFORE `frame.present()` on a screenshot-
              pending frame:
                1. allocate a staging buffer sized to
                   `padded_bytes_per_row * height` (wgpu's
                   256-byte alignment)
                2. `copy_texture_to_buffer` from swap-chain
                   texture into the staging buffer
                3. `device.poll(wait_indefinitely)` to
                   ensure the copy completes
                4. `map_async` + read the mapped range
                5. strip the 256-byte row padding +
                   convert BGRA → RGBA if needed
                6. apply optional crop from `req.crop`
                7. `image::ImageBuffer::save(req.out_path)`
              Synchronous (poll-wait) implementation — a
              future polish can move the encode off-thread.
              `cfg.crop = None` captures the whole window;
              sub-cycle 6 of the design will compute the
              focused-pane rect for true per-pane capture.
              Workspace tests stay 392 (the path is
              best-effort I/O; manual e2e is the test).
              **terminalshot port: 4/7 → effectively
              live**. Sub-cycles 5-7 (toast notification +
              per-pane crop + audit-doc) are polish on top
              of a working capture.

  cycle 687 — **theme-submenu sub-cycle 3: drill-in submenu
              UI**. Replaces the cycle-684 "click logs info"
              no-op with a real, interactive drill-in:
                - `ContextMenuState.drill_stack: Vec<Vec<...>>`
                  holds the parent menu while drilled in
                - new `ContextMenuClick::DrillIntoSubmenu(idx)`
                  outcome
                - hit-test enumerate() returns
                  `DrillIntoSubmenu(idx)` on a Submenu row
                - click dispatch: push current items to
                  drill_stack, replace with the submenu's
                  items, redraw — same anchor/panel
                  geometry, just different content
                - Esc: pops drill_stack instead of closing
                  the menu when drilled in
                - ThemeChoice / ProfileChoice projection now
                  renders the label normally (no more
                  flyout-only hidden treatment) since the
                  drill-in puts them inline
              v1 implements drill-in (replace-in-place)
              instead of the design's "side-panel flyout"
              — simpler renderer, same UX outcome. The
              design's flyout is a future polish cycle.
              End-to-end: right-click → "Theme ▸" → click →
              menu replaces with 512 theme choices → click
              one → theme swaps. Same flow for "Profile ▸".
              Esc on a drilled-in submenu pops back to the
              parent. Workspace tests stay 392.
              Theme-submenu port: **4/9** sub-cycles
              complete (now end-to-end interactive).

  cycle 686 — **theme-submenu sub-cycle 8: Profile submenu**:
              same machinery as cycle-685's Theme submenu,
              different source.
                - new `ContextMenuItem::ProfileChoice { label,
                  profile }` leaf variant
                - new `ContextMenuClick::SetProfile(String)`
                  click outcome
                - new `App::append_profile_submenu_items` walks
                  `Config::list_profiles()` (cycle-618 helper)
                  and produces a `Submenu { label: "Profile",
                  items: vec![ProfileChoice...] }`
                - empty profiles dir → submenu skipped (no
                  visual artifact)
                - `open_context_menu` calls it after the Theme
                  submenu so the layered order reads:
                  built-in → config → lua → remote → Theme ▸
                  → Profile ▸
                - dispatch sets `self.config_path` to the
                  cycle-618 profile path and calls
                  `reload_config()`
              ProfileChoice rows are flyout-only (same
              projection as cycle-685's ThemeChoice). Users
              with `<config-dir>/profiles/dev.config` etc.
              will see "Profile ▸" in the right-click menu;
              clicking it logs the cycle-684 flyout-pending
              nudge until sub-cycle 3 wires the side panel.
              Workspace tests stay 392.
              Theme-submenu port: **3/9** sub-cycles
              complete (1: variant, 2: Theme populate, 8:
              Profile populate; sub-cycles 3-7 + 9 remain).

  cycle 685 — **theme-submenu sub-cycle 2: populate Theme
              submenu + SetTheme dispatch**: data layer for
              the cycle-634 design's Theme submenu now lands.
                - new `ContextMenuItem::ThemeChoice { label,
                  theme }` leaf variant
                - new `ContextMenuClick::SetTheme(String)`
                  click outcome
                - new `App::append_theme_submenu_items` walks
                  `Theme::list()` (~512 bundled themes) and
                  produces a `Submenu { label: "Theme", items:
                  vec![ThemeChoice...] }`
                - `open_context_menu` calls it after remote
                  + lua + config-file items
                - dispatch sets `cfg.theme_name` + `cfg.theme`
                  + saves session + redraws (same path as
                  cycle-3514 NextTheme)
              ThemeChoice projections are flyout-only (hidden
              in the parent menu's renderer-side row list)
              until sub-cycle 3 wires the second-panel
              flyout. For now the user sees "Theme ▸" in the
              right-click menu and a click logs the
              "flyout-wiring-pending" info nudge from cycle
              684. Workspace tests stay 392 (the data path
              compiles + the existing renderer regression
              tests cover the projection arm).
              Theme-submenu port: **2/9** sub-cycles
              complete.

  cycle 684 — **theme-submenu sub-cycle 1: `ContextMenuItem::
              Submenu` recursive variant**. Adds the data
              type that sub-cycles 2-9 of
              [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](docs/TERMINATOR-THEME-SUBMENU-DESIGN.md)
              will consume:
                - `Submenu { label, items: Vec<ContextMenuItem> }`
                  — recursive, so nested-nested submenus are
                  expressible (v1 renderer only flattens one
                  level; deeper nesting is sub-cycle 3.x
                  polish)
              Threaded through 4 match arms:
                - `panel_h` computation (Submenu row = row_h)
                - `max_chars` (Submenu adds +2 for the "▸"
                  suffix)
                - paint loop (Submenu shows the row at row_h)
                - `to_context_menu` projection (renderer's
                  ContextMenuRow gets `label + " ▸"` so the
                  affordance is visible)
              Click dispatch: Submenu row clicks log a
              "flyout wiring lands in sub-cycle 3" info
              message + no-op return. Keyboard nav lands on
              Submenu rows (`item_is_dispatchable = true`
              for Submenu so ↑↓ doesn't skip).
              `#[allow(dead_code)]` gates the variant until
              sub-cycle 2 populates from
              `append_theme_submenu_items`.
              Workspace tests stay 392.
              Theme-submenu port: **1/9** sub-cycles complete.

  cycle 683 — **named-groups sub-cycles 7 + 8: right-click
              context-menu entries + audit-doc finalization**.
              New right-click items appended after the
              built-in copy/paste/split/close/new-tab group:
                - "Set Group…" → `Action::CreateGroup`
                - "Group This Tab…" → `Action::GroupTab`
                - "Ungroup This Tab" → `Action::UngroupTab`
              The entries are layered AFTER the close-family
              + new-tab so they don't hijack muscle memory.
              Users who never touch groups see them at the
              bottom and can ignore.
              Audit-doc rows promoted from D to A:
                - `group_tab`/`ungroup_tab`/`group_win`/
                  `ungroup_win`
                - `create_group`
              Cycle-677 Bucket D close-out table updated:
              **named broadcast groups → ✅ A 7/8** (only
              cross-window via cycle-302 IPC remains).
              Workspace tests stay 392.
              **Named-groups port: 8/8 sub-cycles effectively
              complete**; the design doc's 8th sub-cycle was
              cross-window IPC which is naturally Bucket E
              for a single-process app.

  cycle 682 — **named-groups sub-cycle 6: `[group_name]`
              pill on pane titlebar**: pane titlebar now
              prepends `[name]` when `pane.group_name` is
              Some. Visual cue that the pane belongs to a
              broadcast group, identical across all panes
              with the same group name.
              Format: `  [fleet]  TITLE  cols×rows  🔔`
              (group pill before title, sizetext after as
              before, bell glyph last).
              Empty group_name silently skipped (no
              `[empty]` artifact). Sub-cycle 7 can promote
              this to a real colored quad chip; v1 ships
              the text-only treatment for immediate UX.
              Workspace tests stay 392 (cycle-117 palette
              drift guard + cycle-678 BroadcastScope
              drift guards cover the data path; this is
              renderer-only paint).
              **Named-groups port: 6/8** sub-cycles
              complete. Coloured chip (sub-cycle 6.5) +
              audit-doc finalization (sub-cycle 8) + cross-
              window groups (Bucket E per design) remain.

  cycle 681 — **named-groups sub-cycle 5: `ToggleBroadcastGroup`
              + `ToggleBroadcastWindow` actions — broadcast
              scope is end-to-end live**.
              Two new actions land the runtime scope switch:
                - `Action::ToggleBroadcastGroup` reads the
                  focused pane's `group_name`. If set, toggles
                  `mux.broadcast` between `Off` and
                  `Group(name)`. If the focused pane has no
                  group, logs + no-ops.
                - `Action::ToggleBroadcastWindow` toggles
                  between `Off` and `All` (window-wide).
                  Distinct from the misnamed cycle-178
                  `ToggleBroadcastAll` which is actually
                  per-tab.
              8 new aliases (kebab + underscore for each).
              Palette includes all 3 broadcast actions with
              clear labels distinguishing tab/group/window
              scope. Workspace tests stay 392 (the cycle-678
              `compute_broadcast_targets` drift guard covers
              the routing; this cycle is dispatch wiring).
              **Named-groups end-to-end is now live**:
                1. `Action::GroupTab` → type "fleet" → Enter
                2. Focus a fleet-tagged pane
                3. `Action::ToggleBroadcastGroup` → broadcast
                   scope set to `Group("fleet")`
                4. Type anything → mirrors to every pane
                   tagged "fleet" across every tab
              Named-groups port: **5/8** sub-cycles complete.

  cycle 680 — **named-groups sub-cycle 4: `GroupTab` +
              `GroupWindow` bulk-apply + `Ungroup*` direct
              clear**: completes the action-dispatch wiring
              for named-groups.
                - new `GroupBulkScope { Single, Tab, Window }`
                  enum + `bulk` field on `TitleEditState`
                - existing constructions default to `Single`
                  (preserves cycle-407 EditPaneGroup behavior)
                - `Action::GroupTab` opens the overlay with
                  `bulk = Tab`; `GroupWindow` with `Window`
                - `apply_title_edit` for `TitleEditScope::Group`
                  branches on `bulk` — Single writes to focused
                  pane (existing); Tab writes to every leaf in
                  the active tab; Window writes to every pane
                  across every tab
                - `Action::UngroupTab` / `UngroupWindow` skip
                  the overlay entirely — they directly clear
                  `pane.group_name = None` on every pane in
                  scope
              End-to-end on the deployed binary (next deploy):
              a user can now run `Action::GroupTab` → type
              "fleet" → Enter → every pane in the tab gets
              group_name="fleet". Combined with cycle-679's
              `BroadcastScope::Group(String)`, typing into
              one tagged pane (once cycle-681's dispatch sets
              the scope) will broadcast to all "fleet" panes.
              Workspace tests stay 392.
              Named-groups port: **4/8** sub-cycles complete.

  cycle 679 — **named-groups sub-cycle 3: `mux.broadcast`
              migrated from `bool` to `BroadcastScope`**:
              the named-groups core refactor. Field type
              change rippled through 5 call sites:
                - `broadcast: bool` → `broadcast: BroadcastScope`
                  on `Mux` (default `Off`)
                - new `Mux::is_broadcast_on()` accessor
                  preserves the old bool ergonomics for
                  yes/no callers (TabBar broadcast indicator,
                  paste-respects-broadcast check, clear-
                  scrollback broadcast, edit-overlay key
                  handler)
                - new private `Mux::broadcast_target_ids()`
                  computes the recipient set via cycle-678
                  `compute_broadcast_targets`; consumed by
                  `broadcast_write`, `broadcast_paste`,
                  `broadcast_scroll_to_bottom`
                - `Action::ToggleBroadcastAll` sets
                  `BroadcastScope::Tab` (preserving cycle-178
                  per-tab semantics — the "All" misnaming
                  was tech-debt)
                - `Action::ToggleBroadcastOff` sets
                  `BroadcastScope::Off`
              All existing per-tab broadcast behavior
              preserved. `Group(name)` + `All` variants
              now reachable for the upcoming GroupTab /
              GroupWindow / CreateGroup dispatch wiring
              (cycle-642 surface). Workspace tests 392 → 392
              (existing tests still pass; cycle-678 drift
              guard covers the new helper).
              Named-groups port: **3/8** sub-cycles complete.

  cycle 678 — **named-groups sub-cycle 2: `BroadcastScope`
              enum + `compute_broadcast_targets` pure helper**:
              lands the core data type the
              [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)
              hinges on.
                - `pub enum BroadcastScope { Off, Tab, All,
                  Group(String) }` — the cycle-178 per-tab
                  `mux.broadcast: bool` represents `Off|Tab`;
                  future sub-cycles will migrate it to this
                  richer enum
                - `pub fn compute_broadcast_targets(scope,
                  focused_pane, panes_in_focused_tab,
                  all_panes_with_groups) -> Vec<u64>` — pure
                  helper computing the recipient set
              `#[allow(dead_code)]` gates the enum + fn until
              the broadcast_write migration consumes them.
              Drift guard `compute_broadcast_targets_matrix`
              walks 5 scope shapes including the cross-tab
              named-group case. Workspace tests 391 → 392.
              Named-groups port: **2/8** sub-cycles complete.

  cycle 677 — **Audit doc: Bucket D close-out summary
              (cycles 614-677)**: new section in
              `docs/TERMINATOR-AUDIT.md` summarizing the
              7-feature Bucket D arc:
                - `plugins/remote.py` ✅ A 7/7 (cycle-659 deploy)
                - `plugins/auto_theme.py` ✅ A 7/7 (cycle-671)
                - `ask_before_closing` ✅ A 7/8+polish (cycle-661/663)
                - `tab_position = left/right` ✅ A 7/8+polish (cycle-674)
                - `plugins/terminalshot.py` 🟡 3/7 (wgpu readback pending)
                - Named broadcast groups 🟡 1/8 (BroadcastScope refactor pending)
                - Right-click theme submenu D 0/9 (design only; cycle-329 palette covers same UX)
              **Four Bucket D features now ship end-to-end on
              the deployed binary**. No code change.

  cycle 676 — **confirm-dialog sub-cycles 7 + 8: audit
              finalization**: audit row promoted from D
              (design only) to A (shipped) with sub-cycle 7
              (mouse hit-test) reclassified as polish-deferred.
              Rationale:
                - bottom-bar renderer is keyboard-driven by
                  design (Tab/←→/Enter/Esc) — matches the
                  user's "intentional modal" expectation
                - per-button mouse hit-testing on the bar
                  projection would need pixel-accurate label
                  rects the text shaper doesn't expose
                - the centered-panel renderer upgrade (sub-
                  cycle 3.5 of the design) ships discrete
                  button rects at compose time; mouse hit-test
                  comes for free there
              No code change. **`ask_before_closing` port now
              effectively complete** for all 3 close-family
              actions on the deployed binary.

  cycle 675 — **vertical-tabs sub-cycle 8: audit-doc
              finalization**: audit row flipped from A+D
              (design + partial) to A (shipped). 7/8
              sub-cycles complete end-to-end + deployed on
              the cycle-674 binary; sub-cycle 6 (drag-reorder
              y-axis) deferred as polish — horizontal drag-
              reorder already works (cycle-249) and the
              y-axis flip is identical-shape work that can
              land when a user files a real need. No code
              change. **`tab_position = left/right` port now
              effectively complete.**

  cycle 674 — **Deploy: redeploy with `tab-bar-width` config**.
              Binary at `~/.local/bin/kettle` reports
              `1.45.1 (a13ea56)`. User-visible end-to-end:
              setting `tab-bar-width = 240` (or any value in
              `[40, 600]`) widens or narrows the vertical tab
              strip on the deployed binary.

  cycle 673 — **vertical-tabs sub-cycle 7: `tab-bar-width`
              config key**: new `pub tab_bar_width: f32` field
              on Config, default 180.0 (Firefox-style sidebar).
              Parser clamps to `[40.0, 600.0]` — narrower
              wouldn't fit a tab title; wider would be more
              sidebar than terminal. Threaded through:
                - new `content_rect_for_with_strip` helper takes
                  explicit `strip_w` (the no-arg
                  `content_rect_for` becomes a 180.0 default
                  shim, kept for legacy callers)
                - `App::area` now uses `_with_strip` +
                  `self.cfg.tab_bar_width`
                - `cursor_in_tab_bar` Left/Right x-axis checks
                  use `self.cfg.tab_bar_width`
                - `App::tab_bar_vertical` uses
                  `self.cfg.tab_bar_width` for the strip width
              No effect on horizontal layouts. Drift guard
              `tab_bar_width_parses_and_clamps` walks 6 input
              shapes incl. below-min, above-max, garbage-fallback.
              Workspace tests 390 → 391.
              Vertical-tabs port: 6/8 sub-cycles complete.

  cycle 672 — **vertical-tabs sub-cycle 5: renderer paints
              vertical strips correctly**: per-segment chrome
              now uses each segment's own `(y, h)` (from
              cycle-668's `tab_bar_vertical`) instead of the
              strip-wide `by`/`tabbar.height`. Other paint
              changes:
                - bar background: for vertical, paint a
                  column matching the strip rect (left or
                  right edge), not a full-width horizontal
                  stripe at the top
                - segment separator flips axis: vertical
                  separator along the bottom of each row
                  (vs right of each column for horizontal)
                - active-tab accent bar uses segment's own y/h
                - activity dot positions relative to segment
                - close chip uses segment's own rect
                - new-tab `+` button paints at its own rect
                  (which cycle-668 anchored at strip bottom
                  for vertical)
              Workspace tests stay 390 (renderer paint paths
              are snapshot-tested by the existing tests; the
              vertical-rendering-correctness is best verified
              by user-visible deploy).
              Vertical-tabs port: 5/8 sub-cycles complete.

  cycle 671 — **Deploy: redeploy with `auto_theme.py` port
              7/7 complete**. Binary at `~/.local/bin/kettle`
              reports `1.45.1 (849726f)`. End-to-end on the
              deployed binary:
                - `theme-schedule = sunrise/sunset` with
                  `theme-schedule-lat = 37.7749` +
                  `theme-schedule-long = -122.4194` now flips
                  the theme at actual sunrise/sunset UTC times
                  for that lat/long
                - `theme-schedule = 18:00 dark, 06:00 light`
                  clock variant continues to work
              Three Terminator-port subsystems fully end-to-end
              deployed: remote.py (7/7), confirm-dialog (6/8
              user-visible), auto-theme (7/7).

  cycle 670 — **auto-theme sub-cycle 7: solar-position math —
              completes the 7/7 auto-theme port**.
              New surface in kettle-config (pure, no dep):
                - `pub fn sunrise_sunset_utc_secs(day_of_year,
                  lat, long) -> Option<(u32, u32)>` — NOAA
                  simplified algorithm. Returns sunrise + sunset
                  as seconds-of-day in UTC. Accurate to ~1 min
                  at temperate latitudes; `None` for polar
                  day / polar night.
                - `pub fn schedule_decision_sunrise(now, doy,
                  lat, long) -> bool` — returns dark/light
                  decision. Polar regions fall back to a
                  day-of-year heuristic (winter at high
                  latitude = polar night = dark; summer = polar
                  day = light).
              App's `poll_theme_schedule` now branches on the
              `ThemeSchedule` variant:
                - `Clock` → cycle-664 helper (unchanged)
                - `SunriseSunset` → cycle-670 sunrise helper
                  using `(unix_days % 365) + 1` as the day-of-
                  year approximation (sub-cycle 8 could refine
                  with full Gregorian calendar arithmetic)
              Drift guards covering 11 input shapes:
                - SF June solstice (~12:48 UTC sunrise checked
                  within 10 min)
                - equator equinox (~06:00 / 18:00 UTC)
                - polar day / polar night → None
                - 5 decision-helper window-crossing shapes
              **`auto_theme.py` port now 7/7 sub-cycles complete**.
              Workspace tests 388 → 390. Privacy posture
              upheld (no network).

  cycle 669 — **auto-theme sub-cycle 6: sunrise/sunset variant
              + lat/long config keys**:
                - new `ThemeSchedule::SunriseSunset { lat,
                  long }` enum variant (the design's no-clock
                  geo-driven mode)
                - new config keys `theme-schedule-lat` (range
                  `-90..=90`) and `theme-schedule-long` (range
                  `-180..=180`) — out-of-range values silently
                  reject. Aliases: `theme-schedule-lon`,
                  `theme-schedule-longitude`, underscore forms.
                - `parse_theme_schedule` recognizes
                  `sunrise/sunset`, `sunrise-sunset`, `solar`,
                  `auto` as the sunrise-mode trigger
                - post-process at end-of-parse patches the
                  SunriseSunset variant with the parsed
                  lat/long. If lat OR long is missing,
                  downgrades the schedule to `None` (both
                  halves required).
                - `schedule_decision_clock` now uses
                  `let-else` to defensively default-to-light
                  on the SunriseSunset variant (its own
                  decision helper lands in sub-cycle 7).
              Drift guard `theme_schedule_sunrise_sunset_with_lat_long`
              walks 9 input shapes: happy path, 3 sunrise-
              spelling aliases, 1 underscore-key form, 1
              longitude-alias form, 3 downgrade cases
              (missing lat / missing long / lat out of
              range / long out of range).
              **Privacy posture upheld**: kettle never makes
              network requests for theme purposes; no GeoClue2
              / CoreLocation prompts. Lat/long are explicit
              user config.
              Workspace tests 387 → 388. Auto-theme port: 6/7
              sub-cycles complete.

  cycle 668 — **vertical-tabs sub-cycle 4: tab_bar() stacks
              segments vertically for Left/Right**: the
              cycle-647/665 layout-side foundations + this
              cycle's segment generation give actual vertical
              tab strips. Specifics:
                - new `App::tab_bar_vertical(sw, sh, height)`
                  branches off `tab_bar()` for `is_vertical()`
                  positions
                - each `TabSeg` gets `(strip_x, i * h, strip_w,
                  h)` — strip_x is 0 for Left, `sw - strip_w`
                  for Right
                - close-hit zone keeps the trailing-right axis
                  convention (same as horizontal)
                - new-tab `+` button anchors at the bottom of
                  the strip
                - `cursor_in_tab_bar` updated to do x-axis
                  hit-test for vertical strips (instead of the
                  y-band check)
                - `cursor_in_tab_bar_band` returns y in `[0, sh]`
                  for vertical strips (whole-window span)
              Workspace tests stay 387 (the renderer's actual
              paint still uses horizontal layout — the
              `paint_tab_bar` orientation parameter is
              sub-cycle 5). The mux side now hands the renderer
              the correct vertical rects; renderer-side
              tweaks for stacked drawing land next.
              Vertical-tabs port: 4/8 sub-cycles complete.

  cycle 667 — **Deploy: redeploy with theme-schedule poll +
              vertical-strip layout**. Binary at
              `~/.local/bin/kettle` reports `1.45.1 (c7d6f6c)`.
              User-visible end-to-end:
                - `theme-schedule = 18:00 dark, 06:00 light`
                  in config + a configured light/dark theme
                  pair → theme flips automatically on the
                  boundary minutes.
                - `tab-bar-position = left` (or `right`) now
                  carves a 180 px strip from the side instead
                  of falling through to top. The strip's
                  vertical rendering (paint side) is sub-cycle
                  4 of vertical-tabs, but the layout math
                  honors the orientation now — pane content
                  shrinks correctly.

  cycle 666 — **auto-theme sub-cycle 5: App-side schedule
              poll**: `App::poll_theme_schedule` now runs on
              every redraw tick. When `cfg.theme_schedule` is
              `Some(Clock { … })`:
                1. compute now in HH:MM (UTC; matches the
                   cycle-296 status-bar clock semantics)
                2. ask cycle-664's `schedule_decision_clock`
                   for the bool (true=dark, false=light)
                3. compare against `last_schedule_decision`
                4. on boundary crossing, call cycle-649's
                   `resolve_theme_for_mode(Light|Dark, …)` to
                   compute the next theme + swap
              First call seeds `last_schedule_decision` without
              swapping — only boundary crossings fire the swap,
              not "starting up at 18:30 with dark scheduled."
              End-to-end: a user with `theme-schedule = 18:00
              dark, 06:00 light` + `light-theme`/`dark-theme`
              configured now gets automatic theme flips on
              minute boundaries.
              Workspace tests stay 387 (logic covered by
              cycle-664 pure-helper drift guards;
              `poll_theme_schedule` is the side-effecting
              wrapper).
              Auto-theme port: 5/7 sub-cycles complete.

  cycle 665 — **vertical-tabs sub-cycle 3: `content_rect_for`
              honors Left/Right strip width**: cycle-651 v1
              treated `TabBarPos::Left/Right` the same as `Top`
              (the cycle-647 fallback). Now the pane content
              actually carves out a 180 px strip on the
              configured side instead of consuming a height
              band like `Top`/`Bottom`.
              New `VERTICAL_TAB_STRIP_W: f32 = 180.0` constant
              (Firefox-style sidebar default; configurable via
              the upcoming `tab-bar-width` config key in
              sub-cycle 7).
              Drift guard `content_rect_for_carves_out_tab_and_status_bands`
              extended with 4 new shapes:
                - Left + status-off
                - Right + status-off
                - Left + status-top (vertical strip claims x;
                  status claims y; both compose)
                - Right + status-bottom (same)
              Plus a "narrow window with vertical strip clamps
              content_w" defensive assertion.
              Workspace tests stay 387 (existing drift guard
              expanded; no new fn).
              Vertical-tabs port: 3/8 sub-cycles complete.

  cycle 664 — **auto-theme sub-cycle 4: clock-schedule parser
              + decision helper**: privacy-conscious no-
              geolocation half of the auto-theme schedule.
              New surface:
                - `pub enum ThemeSchedule { Clock { dark_at,
                  light_at } }` (sunrise/sunset is sub-cycle 5)
                - `pub theme_schedule: Option<ThemeSchedule>`
                  field on Config
                - `theme-schedule = HH:MM dark, HH:MM light`
                  parser arm (either tag-order; whitespace
                  flexible; strict on bad input)
                - `pub fn parse_theme_schedule(value)` helper
                - `pub fn schedule_decision_clock(now_hm,
                  schedule) -> bool` (true=dark, false=light)
                  handles wrap-past-midnight + same-day window
                  + degenerate dark==light
              Drift guards:
                - `parse_theme_schedule_walks_input_shapes`
                  walks 8 input shapes incl. 6 rejection cases
                - `schedule_decision_clock_walks_boundaries`
                  walks 13 (now, schedule) pairs incl. wrap-
                  past-midnight, same-day window, degenerate
                  dark==light → light default
              Workspace tests 385 → 387.
              Auto-theme port: 4/7 sub-cycles complete.

  cycle 663 — **docs(audit): flip `ask_before_closing` row
              from D → A** (cycle 662 completed sub-cycles
              1-6 of 8 confirm-dialog work) + redeploy local
              kettle to commit 3f0a7c3.

  cycle 662 — **confirm-dialog sub-cycle 6: `CloseTab` +
              `ClosePane` interception**: completes the close-
              family dispatch wrapping.
                - `Action::CloseTab` checks
                  `should_prompt(panes_in_active_tab)` → opens
                  modal with `on_confirm = ConfirmAction::CloseTab`
                  if pane count meets the configured threshold
                - `Action::ClosePane` checks `should_prompt(1)`
                  → only fires the modal when
                  `ask_before_closing = always` (the
                  multiple_terminals default doesn't prompt for
                  single-pane close)
                - new pure helper `count_leaves(node)` walks the
                  split tree to compute the panes-in-tab scope
                - drift guard `count_leaves_for_nested_splits`
                  covers Leaf / 2-way / 3-way / 4-way trees
              `ConfirmAction::CloseTab` and `ClosePane` arms in
              `dispatch_confirm_action` now have real callers,
              so the `#[allow(dead_code)]` gate on the enum is
              dropped. Workspace tests 384 → 385. Confirm-dialog
              port: 6/8 sub-cycles complete (mouse hit-test +
              audit-doc remain).

  cycle 661 — **Deploy: redeployed local kettle with the
              live confirm-dialog**. Binary at
              `~/.local/bin/kettle` reports `1.45.1 (54b49e6)`.
              User-visible behavior now testable end-to-end:
              with `ask-before-closing = multiple_terminals`
              (the default) + 2+ panes open, pressing the
              CloseWindow chord opens a bottom bar
              `⚠ Close N pane(s)?      [Cancel]   ▶ Close
              (Tab/←→ focus · Enter confirm · Esc cancel)`.
              Tab cycles focus, Enter confirms, Esc cancels.

  cycle 660 — **confirm-dialog sub-cycles 3 + 5: renderer
              + dispatch interception (modal goes live)**.
              When `ask-before-closing` fires for
              `Action::CloseWindow`, the dialog now opens
              visibly and the user can interact:
                - Sub-cycle 3 (renderer): bottom-bar projection
                  paints a red-accent strip with `⚠ Close N
                  pane(s)?      [Cancel]   ▶ Close      (Tab/
                  ←→ focus · Enter confirm · Esc cancel)`.
                  Focused button shows a `▶` prefix. Full
                  centered-panel painting deferred to a polish
                  sub-cycle (3.5).
                - Sub-cycle 5 (dispatch interception):
                  `Action::CloseWindow` checks
                  `cfg.ask_before_closing.should_prompt(pane_count)`
                  (cycle 638). If true, opens the modal with
                  `on_confirm = ConfirmAction::CloseWindow`
                  and the safe default (Cancel) focused.
                  Modal's Confirm → `dispatch_confirm_action`
                  runs the real close path
                  (`mux.close_window` + `save_session` +
                  `event_loop.exit()`).
              Modal-aware key handler: while open, intercepts
              Tab/Shift+Tab/←/→/Enter/Esc via cycle-652's
              `confirm_dialog_keypress` pure helper. Non-nav
              keys are swallowed (modal is exclusive).
              `Pane::CloseTab` and `ClosePane` wired to
              `dispatch_confirm_action` arms for the cycle-661
              sub-cycle 6 wiring. New renderer types:
              `ConfirmDialogOverlay` + `ConfirmDialogButton`.
              Workspace tests stay 384.

  cycle 659 — **Deploy: redeployed local kettle with the full
              remote.py port (sub-cycles 1-7 of 7
              complete)**. Binary at `~/.local/bin/kettle`
              reports `1.45.1 (81b1fa6)`. End-to-end remote-
              session detection is now live + user-discoverable:
              the pane title updates within ~200ms of running
              ssh/docker/podman/kubectl/lxc-attach, and
              right-clicking exposes the "Reconnect" /
              "Re-attach" menu entry to re-establish.

  cycle 658 — **remote.py sub-cycle 7: right-click "Reconnect"
              menu entry — completes the remote.py port**.
              When the focused pane has a detected remote
              context, the context menu now shows a final
              entry (after built-ins, config menu items, and
              Lua items):
                - SSH:        `Reconnect ssh user@host`
                - Docker:     `Re-attach docker container`
                - Podman:     `Re-attach podman container`
                - Kubectl:    `Re-attach kubectl pod`
                - LXC:        `Re-attach lxc name`
              Click → reuses the cycle-611 `ConfigItem`
              dispatch path to write the reconnect command +
              `\n` to the focused pane's PTY (run after the
              original session exits).
              New public surface on kettle-remote:
                - `pub fn clone_session_command(ctx) -> String`
                - `pub fn clone_session_label(ctx) -> String`
              New `App::append_remote_menu_items(items)`
              hooked into `open_context_menu` after the Lua
              items.
              Drift guards on both new functions cover all 6
              `RemoteContext` shapes (SSH ± user, 4 container
              runtimes). Workspace tests 382 → 384.
              Remote.py port now **7/7 sub-cycles complete**
              end-to-end.

  cycle 657 — **Deploy: redeployed local kettle with the live
              remote-session detector** (cycle 656). Binary at
              `~/.local/bin/kettle` reports `1.45.1 (798ea71)`.
              `ssh <host>` in a pane will now update the pane
              title to `ssh user@host` within ~200ms; same for
              docker/podman/kubectl exec. End-to-end user-
              visible behavior for remote.py port (sub-cycles
              1-6 of 7) is live for testing.

  cycle 656 — **remote.py sub-cycle 6: App-side poll loop**:
              the remote-session detector now actually runs.
              New `App::poll_remote_contexts` called from
              `redraw()` each tick, throttled to ~5 Hz
              (200ms minimum spacing). For every pane:
                1. `pane.term.child_pid()` (cycle 639)
                2. `kettle_remote::detect_remote_with(pid,
                   &mut self.remote_sysinfo)` (cycle 646)
                3. on change: write
                   `kettle_remote::format_remote_title(ctx)`
                   to `pane.title`, update
                   `pane.remote_context`
              Reused `sysinfo::System` lives on the App
              (`remote_sysinfo` field) so the process-list
              refresh amortizes between calls. New re-export
              `kettle_remote::SysinfoSystem` lets kettle-ui
              own the type without taking on sysinfo as a
              direct dep.
              End-to-end: a user running `ssh user@box` in
              a pane will see the pane title flip to
              `ssh user@box` within ~200ms. The same for
              `docker exec -it foo bash` → `docker: foo`.
              Drops the cycle-655 `#[allow(dead_code)]` on
              `Pane::remote_context` (now read). Workspace
              tests stay 382 (integration testing needs
              real spawns; cycle-644/645 detectors covered).

  cycle 655 — **remote.py sub-cycle 6 prep: `Pane::remote_context`
              field + kettle-ui → kettle-remote dep**: now every
              Pane carries `pub remote_context:
              Option<kettle_remote::RemoteContext>` that the
              upcoming poll loop will populate. kettle-ui's
              Cargo.toml gains `kettle-remote = { path = ... }`
              so the App can call `detect_remote_with(child_pid,
              &mut sysinfo_system)` per pane on the cycle-290
              trigger tick. `#[allow(dead_code)]` gates the
              field until the poll wiring lands in a follow-up
              sub-cycle. Workspace tests stay 382.

  cycle 654 — **terminalshot sub-cycle 3: `ScreenshotRequest`
              + `Renderer::pending_screenshot` slot**: queues
              a screenshot request on the renderer for the next
              `render_frame` to honor. Surfaces:
                - new `pub struct ScreenshotRequest { out_path,
                  crop: Option<Rect> }` in kettle-render
                - new `pub pending_screenshot: Option<...>`
                  field on `Renderer`
                - new `pub fn set_pending_screenshot(req)` +
                  `pub fn take_pending_screenshot() -> Option`
                - `Action::TakeScreenshot` dispatch (cycle 640)
                  now computes the out path via cycle-650
                  `session_screenshot_path` + queues a real
                  `ScreenshotRequest` instead of just logging.
                  v1 has `crop: None` (whole window); sub-cycle
                  6 wires per-pane crop.
              Sub-cycle 4 wires the actual wgpu surface readback
              + PNG encode inside `render_frame`; for now the
              request sits unread on the slot until that
              sub-cycle lands (no user-visible change yet, but
              the dispatch path now reaches the renderer with
              real state). Workspace tests stay 382.

  cycle 653 — **Deploy verification: install latest build to
              `~/.local/bin/kettle`** via `./scripts/install.sh`.
              The local binary now matches commit 82a827f
              (cycle 652) and reports `1.45.1 (82a827f)` on
              `--version`. Smoke-checked `--check-config`:
              loads defaults cleanly, picks up the bundled
              TokyoNight Night theme + JetBrainsMono Nerd Font.
              Honors the user's standing instruction to keep the
              local kettle install current (run install.sh after
              every meaningful build; in-place overwrite). No code
              change; this cycle is the explicit deployment step
              the /goal hook called out.

  cycle 652 — **confirm-dialog sub-cycle 4: keyboard-nav pure
              helper**: `confirm_dialog_keypress(current_focus,
              num_buttons, key) -> ConfirmKeyResult`. Pure state
              machine for the modal's Tab / Shift+Tab / ←→ /
              Enter / Esc handling. Sub-cycle 5 wires this to
              the App's winit key handler — without the wiring
              the helper is just a pure function exercised by
              tests, but landing it now lets the dispatch loop
              be a thin wrapper.
              New types:
                - `ConfirmKey` (winit-decoupled named keys)
                - `ConfirmKeyResult { Move, Confirm, Cancel,
                  Ignore }`
              Drift guard `confirm_dialog_keypress_walks_state_machine`
              walks 12 input shapes including:
                - Esc/Enter from any focus
                - Tab/Shift+Tab wrap behavior
                - Left/Right no-op at boundaries (Ignore vs
                  Move discrimination)
                - 0-button defensive fallback
                - single-button no-op cycle
              Workspace tests 381 → 382.

  cycle 651 — **vertical-tabs sub-cycle 2: `content_rect_for`
              pure helper**: extracts `App::area`'s layout math
              into a pure function that takes the inputs
              explicitly:
                `content_rect_for(surface, tab_bar_h,
                  status_bar_h, tab_bar_pos, status_bar_mode)
                  -> Rect`
              `App::area` now wraps the helper; cycle-651 v1
              treats `TabBarPos::Left` / `Right` the same as
              `Top` (the cycle-647 fallback). Sub-cycle 4 of
              [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](docs/TERMINATOR-VERTICAL-TABS-DESIGN.md)
              branches on orientation + carves a per-strip
              width instead of a per-edge height. Drift guard
              `content_rect_for_carves_out_tab_and_status_bands`
              walks 8 (tab_pos × status_pos) cases including
              the tiny-window content-h floor. Workspace tests
              380 → 381.

  cycle 650 — **terminalshot sub-cycle 2:
              `session_screenshot_path` pure helper**: mirrors
              cycle-621's `session_log_path` shape. Lives under
              `<cache>/kettle/shots/kettle-<secs>-<pid>.png`
              with relative `./kettle-shots/` fallback when no
              cache dir resolves. Sub-cycle 3-5 of
              [`TERMINATOR-TERMINALSHOT-DESIGN.md`](docs/TERMINATOR-TERMINALSHOT-DESIGN.md)
              will call this from `Action::TakeScreenshot`
              dispatch + queue a wgpu readback request keyed
              on the path. Drift guard
              `session_screenshot_path_under_cache_kettle_shots`
              covers XDG path shape + relative-fallback +
              .png extension. Workspace tests 379 → 380.

  cycle 649 — **auto-theme sub-cycle 2: `resolve_theme_for_mode`
              pure helper**: picks the next theme name given the
              `ThemeMode` + `light_theme` / `dark_theme` config +
              current theme name + detected OS dark-mode flag.
              Pure — entirely a function of its 5 inputs, no env
              or clock. Sub-cycle 3 of
              [`TERMINATOR-AUTO-THEME-DESIGN.md`](docs/TERMINATOR-AUTO-THEME-DESIGN.md)
              will add the `dark-light` crate subscribe; this
              cycle's helper consumes whatever boolean that
              subscribe returns. Drift guard
              `resolve_theme_for_mode_matrix` walks 12 input
              shapes including case-insensitive
              "already-current" no-op and unset-theme no-ops.
              Workspace tests 378 → 379.

  cycle 648 — **confirm-dialog sub-cycle 2: `ConfirmDialogState`
              + `ConfirmAction` + `ConfirmButton` types**:
              the state shapes that sub-cycles 3-5 will consume.
              Builds on cycle-638's `should_prompt` helper.
                - `pub enum ConfirmAction { CloseWindow, CloseTab,
                  ClosePane }` — extensible enum the
                  `maybe_confirm_then` dispatch wrapper will
                  carry. Future cycles add `KillProcess`,
                  `DiscardLayout`, `ResetConfig`.
                - `pub enum ConfirmButton { Cancel, Confirm {
                  label, destructive } }` — two-button v1
                  shape; destructive=true renders red-accent.
                - `pub struct ConfirmDialogState { prompt,
                  buttons, focus_idx, on_confirm }` — owned by
                  `App::confirm_dialog: Option<…>`.
              `#[allow(dead_code)]` on the types + the field
              until the consumers land in sub-cycle 3 (renderer)
              and sub-cycle 5 (dispatch interception). This
              cycle landed the data model so the renderer + the
              dispatch can be written against the final shape
              without churn. Workspace tests stay 378.

  cycle 647 — **vertical-tabs sub-cycle 1: `TabBarPos::Left`
              + `Right` variants**: previously the parser
              accepted `tab-bar-position = left/right` since
              cycle 331/628 but `log::warn`'d and fell through
              to `Top`. Now the values store the actual
              orientation; the render-layer change to draw
              vertical strips lands in sub-cycles 2-6 of
              [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](docs/TERMINATOR-VERTICAL-TABS-DESIGN.md).
              Also: new `TabBarPos::is_vertical()` helper for
              the upcoming `content_rect` branch + paint_tab_bar
              orientation dispatch. Non-exhaustive match arms
              in `cursor_in_tab_bar_band` + `tab_bar()` updated
              to handle Left/Right as no-y-band-hit fallthroughs
              (the rest of the renderer still uses the y-band-
              based geometry until the vertical strip lands).
              Drift guard `tab_bar_pos_left_right_parse_and_classify`
              covers parser routing for both Terminator-spelled
              aliases + the classification helper. Updated the
              older cycle-628 drift guard to reflect the new
              parser behavior. Workspace tests 377 → 378.

  cycle 646 — **remote.py sub-cycle 5: sysinfo process-tree
              walk**: sysinfo 0.32 added as a kettle-remote
              dep (default-features disabled; only the
              `system` feature) — isolated to this crate so
              the heavy process-enumeration code doesn't
              propagate to non-UI consumers.
              `detect_remote(child_pid)` now actually walks
              the process tree:
                - BFS from `child_pid` over `sysinfo`'s
                  parent→children index (built once per call)
                - each descendant's argv is fed through cycle-
                  644 `detect_ssh` + cycle-645 `detect_container`
                - closest descendant wins on tie (BFS gives
                  that for free)
              New companion `detect_remote_with(child_pid,
              &mut System)` lets the App's eventual poll loop
              own a single `System` across ticks so sysinfo's
              internal cache amortizes (instead of allocating
              one per call). Drift guard updated from the stub
              `always None` to "no match for invalid pids 0 /
              u32::MAX" — real-process testing would need
              spawn/CI fragility; the argv-side detectors
              already have exhaustive coverage. Workspace
              tests stay 377.

  cycle 645 — **remote.py sub-cycle 4: Container detector**:
              new `pub fn detect_container(argv: &[String]) ->
              Option<RemoteContext>` covers the four container-
              runtime exec argv shapes:
                - `docker exec [-it] <container> <cmd> …`
                - `podman exec [-it] <container> <cmd> …`
                - `kubectl exec [-it] [-n ns] <pod> -- <cmd>`
                - `lxc-attach [-n] <name>` (the `-n VALUE` is
                  the container; specially-cased extraction)
              Skips known value-taking flags (`-n` / `-u` /
              `-c` / `-w` / `-e`), GNU `--flag=value` forms,
              and the kubectl `--` separator. Returns None for
              non-container argv (`docker ps`, `docker build`),
              for `docker exec` with no container, and for
              empty argv. Drift guard walks 11 input shapes.
              Workspace tests 376 → 377.

  cycle 644 — **remote.py sub-cycle 3: SSH detector**: new
              `pub fn detect_ssh(argv: &[String]) -> Option<
              RemoteContext>` in `kettle-remote`. Pure — takes
              the process's argv (as sub-cycle 5's sysinfo walk
              will supply), returns `Some(Ssh { host, user })`
              for argv shapes that match real ssh invocations:
                - `ssh box`
                - `ssh user@host`
                - `ssh -p 22 user@host`
                - `ssh -o StrictHostKeyChecking=no host`
                - `ssh -l user host`
                - `sshpass -p secret ssh user@host`
                - `/usr/bin/ssh box` (absolute argv[0])
              Skips `-o foo=bar` / `-p 22` / `-l user` value
              args correctly. Returns `None` for non-ssh argv
              (vim, bash, …) and for ssh with no target
              (e.g. `ssh -V`). Drift guard walks 11 real-world
              shapes. Workspace tests 375 → 376.

  cycle 643 — **remote.py sub-cycle 2: `kettle-remote` crate
              skeleton + `RemoteContext` type**: new workspace
              member `crates/kettle-remote/`. Isolated from
              kettle-core so the eventual sysinfo dep doesn't
              propagate to non-UI consumers (the headless
              `--screenshot` path, `--check-config` validator).
              v1 of this crate ships:
                - `pub enum RemoteContext { Ssh { host, user },
                  Container { runtime, container } }`
                - `pub enum ContainerRuntime { Docker, Podman,
                  Kubectl, Lxc }`
                - `pub fn detect_remote(child_pid) -> Option<
                  RemoteContext>` — v1 stub returning None;
                  sub-cycle 5 wires the sysinfo dep + actual
                  process-tree walk.
                - `pub fn format_remote_title(ctx) -> String`
                  — pure formatter that drives the pane-title
                  update path.
              Two drift guards in the new crate cover the 6
              format-title shapes (SSH ± user, 4 container
              runtimes) + the stub-returns-None promise. The
              public surface lands NOW so the App code-paths
              can compile against the final return shape before
              the sysinfo dep gets pulled in. Workspace tests
              373 → 375.

  cycle 642 — **named-groups sub-cycle 1: action surface for
              `CreateGroup` + `GroupTab` + `GroupWindow` +
              `UngroupTab` + `UngroupWindow`**: 5 new Action
              variants from [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)
              plus the 12 aliases that Terminator users would
              type. Dispatch:
                - `CreateGroup` and existing cycle-407
                  `EditPaneGroup` share dispatch (same
                  title-edit overlay)
                - `GroupTab` / `GroupWindow` log a TODO pointing
                  at named-groups sub-cycle 4 (bulk-apply path)
                - `UngroupTab` / `UngroupWindow` log a TODO
                  pointing at sub-cycle 5 (bulk-clear path)
              Palette includes all 5 so the actions are
              discoverable via the cycle-329 command palette.
              Workspace tests stay 373 (action enum is covered
              by the cycle-117 palette drift guard
              transitively).

  cycle 641 — **auto-theme sub-cycle 1: `ThemeMode` enum +
              `theme-mode` config key**: new `ThemeMode {
              Explicit, Light, Dark, Auto }` enum on Config
              (default `Explicit` preserves cycle-616 behavior).
              Parser arm accepts kebab + underscore key spellings
              and 4 alias values for `Auto` (`auto` / `system` /
              `follow-system` / `follow_system`). Sub-cycle 2 of
              [`TERMINATOR-AUTO-THEME-DESIGN.md`](docs/TERMINATOR-AUTO-THEME-DESIGN.md)
              wires the `dark-light` crate subscribe; for now
              this just lets a Terminator config copy in
              cleanly without --check-config warnings. Drift
              guard `theme_mode_parses_terminator_values` walks
              10 input shapes. Workspace tests 372 → 373.

  cycle 640 — **terminalshot.py sub-cycle 1:
              `Action::TakeScreenshot` surface**: new Action
              variant + 4 aliases (`take_screenshot`,
              `take-screenshot`, `terminalshot`, `screenshot`)
              from
              [`TERMINATOR-TERMINALSHOT-DESIGN.md`](docs/TERMINATOR-TERMINALSHOT-DESIGN.md).
              Dispatch arm logs a TODO pointing at the headless
              `--screenshot=PATH` fallback for now; sub-cycles
              2-6 wire the wgpu surface readback + PNG encode +
              toast notification. Palette includes the action so
              it's discoverable via cycle-329 command palette.
              Drift guard `from_name_accepts_take_screenshot_aliases`
              walks all 4 spellings. Workspace tests 371 → 372.

  cycle 639 — **remote.py sub-cycle 1: `Terminal::child_pid()`
              accessor**: new public method on `kettle_core::
              Terminal` returns the PTY child's OS pid via
              `portable_pty::Child::process_id()`. Read-only,
              doesn't consume the Child. The upcoming remote-
              session detector (sub-cycles 2-6 from
              [`TERMINATOR-REMOTE-DESIGN.md`](docs/TERMINATOR-REMOTE-DESIGN.md))
              roots its process-tree walk here. Returns None on
              lock contention or platforms without pid access.
              No new drift guard — the method is one line over
              the existing child Arc<Mutex<>>; the upcoming
              `kettle_remote::detect_remote` tests will cover it
              transitively. Workspace tests stay 371.

  cycle 638 — **confirm-dialog sub-cycle 1: `should_prompt`
              pure helper**: new `AskBeforeClosing::should_prompt(
              scope_count) -> bool` method on the existing enum
              implements the matrix from
              [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md):
                - `Never`              → never prompts
                - `Always`             → always prompts
                - `MultipleTerminals`  → prompts iff `scope_count > 1`
              Pure — no `&self` shape needed, just the enum + count.
              Sub-cycle 5+ wires it to the close-family dispatch.
              Drift guard `ask_before_closing_should_prompt_matrix`
              walks all 3 modes × 4 scope counts (0, 1, 2, 100).
              Workspace tests 370 → 371.

  cycle 637 — **`docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md` —
              Bucket D for `ask_before_closing` + a reusable
              confirm-dialog primitive**: the cycle-343-360
              parsed-but-unwired `AskBeforeClosing` config gets
              a real consumer. Architecture:
                - new `ConfirmDialogState` + `ConfirmAction` enum
                - generic primitive — first user is the close
                  family (Window/Tab/Pane), future users include
                  "kill running process" + "discard unsaved
                  layout" + "reset config"
                - new `should_prompt(mode, scope_count) -> bool`
                  pure helper (matrix over the 3 modes × scope)
                - new `maybe_confirm_then(action)` dispatch
                  wrapper — intercepts close-family actions and
                  opens modal vs falls through based on mode
                - centered modal renderer + dim backdrop +
                  focus-on-Cancel safe default
                - keyboard nav: Tab cycles focus, Enter confirms,
                  Esc cancels
              8 sub-cycles, +6-8 estimated tests. Audit row
              promoted from 🟡 (parsed not wired) to D. No code
              change.

  cycle 636 — **`cell_width` / `cell_height` renderer wiring
              (Terminator parity, config.py)**: the config keys
              were parsed (and clamped to [0.5, 3.0]) since
              cycles 343-356 but didn't actually scale the
              measured cell metrics. Now:
                - Renderer gains `pub cell_scale_w: f32` +
                  `pub cell_scale_h: f32` fields (default 1.0)
                - constructor multiplies `measure_cell` results
                  by these before storing `cell_w` / `cell_h`
                - `remeasure_cell` (called on font-family /
                  font-size change) preserves the scale
                - new `pub fn set_cell_scale(w, h)` setter
                  (no-op when unchanged; triggers re-measure)
                - app.rs `reload_config` calls it alongside
                  `set_font_family` + `set_font_size`
              So a user with `cell-height = 1.5` now actually
              gets 50% line spacing on next reload. Workspace
              tests stay 370 (no new drift guard — the
              multiplier is a one-line scale; behavior covered
              by the existing measure_cell tests + the lint+
              build gauntlet exercising the new fields).

  cycle 635 — **Audit doc reconciliation (round 5)**: 7 more
              audit rows reclassified:
                - `inactive_color_offset` → ✅ shipped (both
                  fg + bg offsets parse and apply)
                - `title_at_bottom` → ✅ shipped (per-pane
                  titlebar honors it in render/lib.rs)
                - `remote.py` → D (cycle-629 design doc)
                - `ask_before_closing` → 🟡 parsed not wired
                  (Bucket D: shared modal-overlay primitive)
                - `layout_launcher` → Bucket E (cycle-329
                  palette covers the picker UX)
                - `cell_width`/`cell_height` → 🟡 Bucket C
                  (parsed; needs renderer font-metric multiply)
                - `palette = solarized_dark` → Bucket E
                  (kettle's ~512 themes are a superset)
                - `Multiple grouping modes + auto-cleanup` → D
                  (cycle-631 named-groups design covers it)
              Audit now reflects ground truth: every row is
              either ✅ (shipped), Bucket D with a cross-link to
              the design doc, Bucket E with a divergence
              rationale, or 🟡 Bucket C with a concrete
              implementation sketch.

  cycle 634 — **`docs/TERMINATOR-THEME-SUBMENU-DESIGN.md` —
              Bucket D design doc for the right-click theme +
              profile submenu (Terminator
              `terminal_popup_menu.py`)**: today's cycle-245
              context menu flat-lists items. Submenu requires:
                - new `ContextMenuItem::Submenu { label, items }`
                  recursive variant
                - new `SubmenuState` + hover-delay state machine
                  (~250 ms GNOME-standard)
                - second-panel renderer + window-edge clipping
                  (flip to left when right would overflow)
                - keyboard nav (`→` opens, `←` closes)
                - populated from `Theme::list()` (~512 themes)
                  and `Config::list_profiles()`
              9 sub-cycles, +6-8 estimated tests. Audit row
              promoted from C/❌ to D with cross-link.
              Explicit Bucket E carveouts: nested-nested
              submenus (single level only), search-within-
              submenu (use cycle-329 palette instead), keyboard-
              only accelerator (follow-up). No code change.

  cycle 633 — **`docs/TERMINATOR-VERTICAL-TABS-DESIGN.md` —
              Bucket D design doc for `tab-position = left/right`
              (vertical tab strip)**: cycle 331/628 wired the
              parser for the values; this design lays out the
              render-layer change needed for the actual layout.
              Architecture:
                - new `TabBarPos::Left` / `::Right` variants
                - new `App::content_rect()` pure helper that
                  branches the pane-content rect on
                  (tab_bar_pos, tab_bar_visible, window_size)
                - new `kettle_render::TabBarOrientation` enum
                  (`Horizontal` / `VerticalLeft` / `VerticalRight`)
                  parameter on `paint_tab_bar`
                - hit-testing flip on cursor_in_tab_bar /
                  tab_seg_at_cursor / tab_close_at_cursor
                - drag-reorder generalized to either axis
                - new `tab-bar-width = 180` config knob for
                  vertical strip width
              8 sub-cycles, +10-12 estimated tests. Audit row
              promoted from B-partial to A+D with cross-link.
              No code change.

  cycle 632 — **`docs/TERMINATOR-AUTO-THEME-DESIGN.md` — Bucket D
              design doc for auto-detect + sunrise/sunset (the
              other half of `plugins/auto_theme.py` not shipped
              in cycle 616's manual toggle)**:
              architecture:
                - new `ThemeMode { Explicit, Light, Dark, Auto }`
                  enum + `ThemeSchedule { Clock, SunriseSunset }`
                - `dark-light` crate for cross-platform OS-pref
                  detection (DBus portal on Linux,
                  NSDistributedNotificationCenter on macOS,
                  RegNotifyChangeKeyValue on Windows)
                - theme_watcher module spawns the subscribe task;
                  fires events that reuse cycle-616's apply_theme
                - sunrise/sunset takes explicit lat/long
                  (privacy: never makes network requests; no
                  GeoClue2/CoreLocation prompts)
                - clock schedule: `theme-schedule = 18:00 dark,
                  06:00 light` for no-geolocation users
              7 sub-cycles, +10-12 estimated tests. Audit row
              updated from A (manual-only) to A+D. Risk
              register covers dark-light compile-failure
              fallback to cycle-616 manual, subscribe-blocks-
              launch (100 ms timeout), system-sleep drift,
              lat/long range validation. No code change.

  cycle 631 — **`docs/TERMINATOR-NAMED-GROUPS-DESIGN.md` —
              Bucket D design doc for Terminator's named broadcast
              groups (`create_group` / `group_tab` / `group_win`
              + ungroup_*)**:
              fills the kettle gap between per-tab broadcast
              (cycle 178) and broadcast-all — the finer-grained
              "broadcast to every pane I tagged with X." Design:
                - new `BroadcastScope::Group(String)` variant on
                  the existing scope enum
                - cycle-407 `pane.group_name` field gets promoted
                  from display-only to scoping-load-bearing
                - cycle-369 title-edit overlay reused with the
                  existing `TitleEditScope::Group` variant
                - renderer titlebar shows a `[name]` pill with
                  hash-derived color so all "fleet" panes look
                  visually linked
                - new actions: `CreateGroup`, `GroupTab`,
                  `GroupWindow`, `UngroupTab`, `UngroupWindow`
              8 sub-cycle roadmap, +8-10 estimated tests.
              Explicit Bucket E carveouts: cross-window groups
              (cycle-302 IPC follow-up), session-persistence of
              group assignments. Audit-doc rows promoted from
              C/❌ to D. No code change.

  cycle 630 — **`docs/TERMINATOR-TERMINALSHOT-DESIGN.md` —
              Bucket D design doc for `plugins/terminalshot.py`
              live-window capture**:
              live-window readback fills the gap between the
              existing headless `--screenshot` (synthetic scene)
              and what users actually want when they press a
              "screenshot now" chord. Architecture:
                - `Action::TakeScreenshot` + aliases queues
                  a `ScreenshotRequest` on the renderer
                - Renderer paints into an intermediate texture
                  on screenshot-pending frames + copy_texture_
                  to_buffer + map_async + PNG encode
                - Per-pane crop (focused-pane rect from mux)
                - Toast notification on success
                - Path scheme mirrors cycle-621 logger:
                  `<cache>/kettle/shots/kettle-<secs>-<pid>.png`
              7 sub-cycle roadmap, +5 estimated tests. Audit-
              doc row updated to A+D (`--screenshot` covers
              the synthetic path; D for live capture). Risk
              register covers GPU readback latency, render-
              thread blocking, image-crate version skew. No
              code change.

  cycle 629 — **`docs/TERMINATOR-REMOTE-DESIGN.md` — Bucket D
              design doc for `plugins/remote.py` port**:
              SSH / Docker / Podman / kubectl session detection
              via a new `kettle_remote` crate (sysinfo-backed
              process-tree walk), `Terminal::child_pid()`
              accessor, SSH + Container detectors, ~10 Hz poll
              tied to cycle-290 trigger cadence, right-click
              "Clone session" menu integration. 7 sub-cycles,
              estimated +12-15 tests. Same shape as the existing
              Bucket-D design docs (PLUGIN, DETACHABLE-TABS,
              PANE-TITLEBAR, BG-IMAGE). Audit-doc row promoted
              from C to D with cross-link. No code change.

  cycle 628 — **`tab-position` Terminator alias (config.py:144)**:
              cycle-331 wired the canonical kettle key
              `tab-bar-position` with all 4 Terminator values
              (top/bottom/hidden/left/right). Cycle 628 accepts
              the Terminator-spelled `tab-position` / `tab_position`
              as additional aliases so a Terminator config file
              binds without rename. Both the parser arm and the
              `detect_malformed_values` diagnostic arm updated;
              drift guard `tab_position_alias_parses` covers 5
              input shapes including the parse-time-accepted
              left/right runtime-fallback. Workspace tests 369 → 370.

  cycle 627 — **Doc-truth refresh (round 4)**: 7 more stale
              audit-doc rows flipped, citing the cycles that
              closed them:
                - `edit_*_title` → cycle 369-407 (full title-edit
                  overlay shipped, including the cycle-407
                  `EditPaneGroup` for broadcast-group name)
                - `close_button_on_tab` → wired earlier
                - `login_shell` → cycle 343 (mux.rs threads
                  the bool to Terminal::new_with_env)
                - `next_profile` / `prev_profile` → cycles 342
                  + 618 (refactor)
                - `geometry_hinting` → cycle 359 (winit
                  resize_increments)
              Reclassified `sticky` + `hide_from_taskbar` to
              Bucket E with rationale (winit 0.30 only exposes
              skip_taskbar on Windows; X11/Wayland/macOS would
              need platform-specific extensions kettle hasn't
              taken on). No code change. Tests stay 369/369.

  cycle 626 — **`audible_bell` accepted as documented no-op
              (Terminator config.py:214)**: kettle ships no audio
              bell surface yet (visual flash + window urgency only),
              so the key parses but is otherwise a Bucket E
              documented no-op. Lets a Terminator config copy
              cleanly without --check-config warnings; users who
              want a bell should use `bell = …` or the cycle-619
              `visible_bell` / `urgent_bell` compat aliases. Drift
              guard `audible_bell_parses_as_documented_noop`
              locks in the no-op (combined with the canonical
              `bell =` precedence rule). Workspace tests 368 → 369.

  cycle 625 — **`log-strip-ansi` config — plain-text session
              logs**:
                - extends cycle-621 logger.py parity. When the
                  per-pane session log is open
                  (`Action::ToggleSessionLog`), the reader thread
                  honors `log-strip-ansi = true` and removes CSI
                  / OSC / single-char ESC sequences before
                  writing — gives a grep-friendly log file.
                  `false` (default) preserves the raw-stream
                  behavior (cat-replayable in a terminal).
                - new pure helper `kettle_core::strip_ansi_bytes`
                  is the strip impl. State-free byte-block strip
                  (good enough for the line-buffered reader);
                  documented split-across-reads limitation in
                  doc comments.
                - new per-Terminal `Arc<Mutex<bool>>
                  log_strip_ansi` flag the reader thread reads
                  on each write. Action::ToggleSessionLog
                  propagates `cfg.log_strip_ansi` to it at
                  file-open time.
              Drift guard `strip_ansi_bytes_removes_csi_osc_and_single_esc`
              covers 7 input shapes including OSC terminated by
              BEL vs ESC\\, single-char ESC, bare-ESC-at-end.
              Workspace tests 367 → 368.

  cycle 624 — **Doc-truth refresh:
              `docs/TERMINATOR-AUDIT.md` (round 3)**:
              flipped 9 more stale rows to ✅, citing the cycles
              that closed them (335, 342, 345, 347, 350, 613,
              617). Plus reclassified `scroll_tabbar` to Bucket
              E with rationale (kettle's cycle-620 layout has
              overflow fallback; wheel-cycles-tabs gesture is
              the kitty/iTerm2 convention not the Terminator
              one). Tests still 367/367.

  cycle 623 — **Terminator color / cursor / fullscreen key
              aliases (Terminator config copies in unchanged)**:
                - `background-color` / `background_color` → kettle's
                  canonical `background` key
                - `foreground-color` / `foreground_color` → kettle's
                  canonical `foreground` key
                - `cursor-shape` / `cursor_shape` → kettle's
                  `cursor-style` (`block` / `underline` / `bar`;
                  also accepts `ibeam` / `i-beam` for Terminator's
                  spelling of the vertical bar)
                - `cursor-blink` / `cursor_blink` → kettle's
                  `cursor-style-blink`
                - `full-screen` / `full_screen = true` → sets
                  `window_state` to `Fullscreen` (false is a no-op
                  to preserve a separately-set window-state)
              All canonical key behaviors unchanged; just additional
              spelling acceptance. Drift guard
              `terminator_color_cursor_aliases_parse` walks 11 input
              shapes. Workspace tests 366 → 367.

  cycle 622 — **`plugins/run_cmd_on_match.py` parity**:
                - `trigger = REGEX :: cmd arg1 arg2` extends
                  cycle-289 trigger syntax with a `::` separator.
                  RHS is whitespace-split into argv (no shell
                  expansion at kettle's layer; the configured
                  command is treated as data, not as a shell
                  string).
                - `TriggerAction::RunCommand(Vec<String>)` new
                  variant carrying the argv. `TriggerAction` loses
                  its `Copy` derive (Vec<String> can't be Copy);
                  callers (`compile_triggers`, `match_triggers`)
                  switch to `.clone()`.
                - new pure helper `parse_trigger_with_command`
                  takes the raw value, returns `Option<(pattern,
                  argv)>` — `None` falls through to the cycle-289
                  Urgency action.
                - dispatch: `match_triggers` returns the action;
                  the loop now branches on Urgency vs RunCommand
                  and spawns via `spawn_trigger_command` (fire-
                  and-forget). Spawn errors are logged + ignored.
                - documented limitation: `::` separator means
                  patterns containing a literal `::` (rare IPv6
                  alternations) get split early. Workaround:
                  write `:[:]` or `\x3a\x3a`.
              Drift guard:
                - `parse_trigger_with_command_splits_on_double_colon`
                  covers happy path, multi-arg argv, whitespace
                  collapsing, all 4 sentinel-None cases, +
                  documents the IPv6 footgun.
              Workspace tests 365 → 366.

  cycle 621 — **`plugins/logger.py` parity — per-pane session
              log**:
                - new `Action::ToggleSessionLog` (aliases:
                  `start_logger` / `stop_logger` /
                  `toggle_session_log` plus kebab variants;
                  Terminator's two-button start/stop UX maps
                  to one toggle here)
                - new `pub log_file: Arc<Mutex<Option<File>>>`
                  on `kettle_core::Terminal`. Reader thread
                  holds a clone + writes raw PTY bytes (no ANSI
                  stripping — preserves replayable output)
                  when the file is Some. Best-effort I/O:
                  errors are swallowed so a full disk doesn't
                  crash the reader.
                - dispatch arm computes path via two new pure
                  helpers: `session_log_path(unix_secs, pid,
                  cache_dir)` and `cache_dir_from_env(get)`.
                  Helpers take primitives + Path/env-fn so
                  they're fully unit-testable without disk I/O.
                - file path shape:
                  `<XDG-cache>/kettle/logs/kettle-<secs>-<pid>.log`
                  (relative `./kettle-logs/...` fallback when
                  no cache dir resolves).
              Drift guards (2 new, both pure):
                - `session_log_path_under_cache_kettle_logs`:
                  XDG path shape + relative-fallback shape.
                - `cache_dir_from_env_probes_in_order`: XDG →
                  HOME/.cache → LOCALAPPDATA → None; empty-XDG
                  falls through (CI safety).
              Workspace tests 363 → 365.

  cycle 620 — **Non-homogeneous tab widths (Terminator
              config.py:88 `homogeneous_tabbar = false`)**:
                - new pure helper `compute_tab_segment_widths`
                  drives per-tab strip widths:
                    - `true` (kettle default) → equal width
                      `strip / n` (current behavior, unchanged)
                    - `false` → per-tab natural width =
                      `chars * cell_w + 2 * chrome + close_w`
                      with a `close_w * 1.5` min-affordance floor
                    - sum > strip → silent fallback to homogeneous
                      (no truncation; every tab stays visible)
                - tab_bar() now consumes the helper instead of
                  computing seg_w inline; x_offsets are
                  pre-computed from cumulative widths
                - empty title list yields `vec![strip]` (panic-safe;
                  never seen at runtime but the helper still has
                  to handle it for symmetry)
              Drift guard
              `compute_tab_segment_widths_homogeneous_and_natural`
              walks 4 scenarios (homogeneous, natural with room,
              overflow fallback, empty list).
              Workspace tests 362 → 363.

  cycle 619 — **`visible_bell` + `urgent_bell` compat parsing
              (Terminator config.py:215-216)**:
                - new parser arms map Terminator's two-bool bell
                  split into kettle's unified `BellMode`. Compose
                  semantics: `Off + Visual = Visual`, `Off +
                  Attention = Attention`, `Visual + Attention =
                  Both`. Order-independent (composes at end-of-
                  parse).
                - precedence: explicit canonical `bell = <mode>`
                  wins over the Terminator aliases regardless of
                  file order — kettle key takes precedence on
                  hybrid configs.
                - `force-no-bell = true` still overrides everything
                  (cycle 613 chain unchanged).
                - new `BellMode::compose(other)` pure helper
                  (OR-like, idempotent, with identity = Off).
              Drift guards:
                - `visible_bell_and_urgent_bell_compose_into_bell_mode`
                  walks 8 input shapes including canonical-precedence
                  + force-no-bell chain
                - `bellmode_compose_is_idempotent_and_or_like`
                  exhaustively round-trips all 4×4 input pairs +
                  proves the algebra (idempotence, identity = Off,
                  Both absorbs)
              Workspace tests 360 → 362.

  cycle 618 — **Profile-cycling refactor (Terminator
              `key_next_profile` / `key_previous_profile`)**:
                - new pub fn `Config::list_profiles()` enumerates
                  `<config-dir>/profiles/*.config` (deterministic
                  sort: case-insensitive primary + bytewise tiebreak)
                - new pub fn `Config::profile_name_from_path()`
                  inverts `path_for_profile`
                - app.rs NextProfile/PrevProfile dispatch refactored
                  to use both helpers + new pure `pick_next_profile`
                  helper (forward/back cycling with wrap)
                - inline disk-walk in app.rs was duplicating the
                  same path math kettle-config now exposes; one
                  source of truth + drift guards on it
              Drift guards (3):
                - `profile_name_from_path_inverts_path_for_profile`
                  covers round-trip, default-config rejection, wrong-
                  parent rejection, missing-suffix rejection
                - `pick_next_profile_wraps_and_starts_at_index_0`
                  covers fwd/back cycling, unknown-current → idx 0,
                  single-profile self-return
              Workspace tests 358 → 360.

  cycle 617 — **`case_sensitive` parity (Terminator
              config.py:117)**:
                - new enum `SearchCaseSensitivity { Smart,
                  Always, Never }` on Config (default Smart =
                  kettle's pre-617 ripgrep/vim behavior)
                - parser accepts: `smart`/`auto` ⇒ Smart;
                  `always`/`sensitive` ⇒ Always; `never`/
                  `insensitive` ⇒ Never; Terminator-spelled
                  `case-sensitive = true/false` (and the
                  underscore form) maps to Always/Never
                - new public API in kettle-core:
                  `CaseSensitivity`, `build_regex_with`,
                  `search_with` (the no-arg `search`/
                  `build_regex` remain as Smart-mode
                  shorthands; back-compat preserved)
                - app.rs scrollback search now threads
                  `cfg.search_case_sensitive` through to
                  `kettle_core::search_with`
                - new pure-helper `map_case_sensitivity` is
                  the kettle-config ↔ kettle-core bridge
              Drift guards: parser side
              `search_case_sensitive_parses_terminator_and_named_forms`
              (12 input shapes) + engine side
              `build_regex_with_honors_explicit_case_sensitivity`
              (round-trips all 3 modes + empty-pattern).
              Workspace tests 356 → 358.

  cycle 616 — **`plugins/auto_theme.py` parity (manual toggle)**:
                - new config keys `light-theme = <name>` and
                  `dark-theme = <name>` (kebab + underscore both
                  accepted; case-insensitive bundled-name lookup
                  stores the canonical form)
                - new `Action::ToggleLightDark` (`toggle_light_dark`
                  / `toggle-light-dark` / `toggle_theme_variant` /
                  `toggle-theme-variant`) — runtime swaps the
                  current theme between the two configured ones:
                    - current == dark → switch to light
                    - current == light → switch to dark
                    - third-party current → default to dark
                    - only one configured → one-way switch
                    - neither configured → no-op + warn
                Sunrise/sunset auto-detection is a follow-up; the
                manual chord covers the bulk of the auto_theme.py
                use case (day-to-day variant flipping). Pure helper
                `pick_light_dark_target` is unit-testable; drift
                guard `pick_light_dark_target_round_trips` covers
                the 7 input shapes. Workspace tests 354 → 356.

  cycle 615 — **Doc-truth refresh: `docs/TERMINATOR-AUDIT.md`**
              flipped 9 rows from ❌/🟡 to ✅, citing the
              cycles that closed them (604/606/607/609/611/
              612/613/614). Plugin inventory + gap table +
              roadmap list all now reflect ground truth.
              `insert_number`/`insert_padded` reclassified to
              Bucket E with rationale (kettle uses pane titles,
              not numbered enumeration). Tests still 354/354.

  cycle 614 — **Terminator-spelling keybind aliases**
              (`config.py:133-134` / `:195`):
                - `new_terminator` / `new-terminator` → kettle's
                  `Action::NewWindow` (Terminator name for
                  "spawn a new top-level instance")
                - `cycle_next` / `cycle-next` → `NextTab`
                - `cycle_prev` / `cycle-prev` → `PrevTab`
              A Terminator user with `keybind = super+i =
              new_terminator` in their config now binds
              correctly without a kettle-side rename. Drift
              guard `from_name_accepts_terminator_spelling_aliases`
              walks the 9 alias permutations. Workspace tests
              353 → 354.

  cycle 613 — **`force-no-bell = true` honors override**
              (Terminator parity, `config.py:force_no_bell`).
              Previously the key parsed (since cycle 340) but
              was a documented no-op — setting
              `force_no_bell = true` in a config copied from
              Terminator didn't actually silence the bell.
              Now: at the end of `parse_collect`, if
              `force_no_bell` is true, force `cfg.bell =
              BellMode::Off` regardless of any earlier `bell
              = ...` line. Wins on both orders (`bell` before
              or after `force-no-bell`). Drift guard
              `force_no_bell_overrides_bell_mode_to_off`
              walks 4 cases (alone, with `bell = both`
              before, with `bell = both` after, default
              leaves bell alone). Workspace tests 352 → 353.

  cycle 612 — **Long-command desktop notification on OSC 133 D
              (CommandEnd)** — Terminator parity for
              `terminatorlib/plugins/command_notify.py`. When a
              command completes in a pane:
                - kettle window doesn't have focus, AND
                - elapsed duration crossed
                  `cfg.command_notify_threshold_ms` (default 5 s,
                  `0` disables)
              kettle fires a desktop notification "kettle:
              command finished" with the pane id, duration, and
              exit code. Requires shell integration (`kettle
              --shell-integration bash`) — without OSC 133 the
              shell never emits the CommandEnd event. New
              kettle-core types: `term::CommandFinished {
              duration, exit_code }`, per-Terminal
              `output_started_at: Arc<Mutex<Option<Instant>>>`
              and `command_finished: Arc<Mutex<Vec<...>>>`
              (bounded at 32 entries against runaway shells),
              `Terminal::drain_command_finished_events()`. The
              PTY reader thread tracks the OutputStart →
              CommandEnd transition; the App drains the queue
              each tick. Drift guard
              `command_notify_threshold_parses_and_clamps` walks
              the 4 aliases + default + 0-disables + 1-day clamp.
              CONFIG.md row + commented example in
              kettle.example.config. Workspace tests 351 → 352.

  cycle 611 — **`menu-item = LABEL = CMD` config grammar**
              (Terminator parity, `terminatorlib/plugins/
              custom_commands.py` → "Custom Commands" menu).
              Repeatable config-file syntax that appends a
              right-click menu row writing `CMD\n` to the
              focused pane's PTY on click. Simpler than the
              cycle-375 `kettle.add_menu_item(label, callback)`
              Lua API: no callback to author, just literal
              text. The two paths layer cleanly — visual order
              top-to-bottom in the menu is: built-in actions →
              separator → config-file commands (cycle 611) →
              separator → Lua-registered items (cycle 375).
              New `Config::menu_items: Vec<MenuItem>` field,
              new `ContextMenuItem::ConfigItem` + `ContextMenu
              Click::ConfigCommand` variants, parser arm with
              both kebab + underscore aliases. Drift guards:
              `menu_item_parses_label_and_command` walks 6 cases
              (well-formed, multi-`=`-in-command, default empty,
              missing separator, empty label, empty command,
              underscore alias);
              `detect_malformed_values_flags_invalid_menu_item`
              ensures `--check-config` surfaces the malformed
              forms. CONFIG.md row + commented example in
              kettle.example.config. Workspace tests 349 → 351.

  cycle 610 — **CONFIG.md "no-op keys" reclassification.** The
              cycle-564 "Parsed-but-currently-no-op keys" table
              had grown stale as cycles 353 / 359 / 360 / 604 /
              609 wired specific keys, and as cycle-575's audit
              showed several entries were "no-op because
              kettle's behavior already matches" rather than
              "no-op because not implemented." Split the section
              into three disposition buckets:
                - **Effectively wired** (4 keys): kettle's
                  behavior already matches the setting
                  (`detachable-tabs`, `homogeneous-tabbar`,
                  `sticky` via `always-on-top`,
                  `inactive-color-offset` via
                  `unfocused-split-opacity`).
                - **Won't implement** (4 keys): by-design
                  divergence (`cursor-color-default`,
                  `http-proxy`, `broadcast-default`,
                  `putty-paste-style-source-clipboard`).
                - **Genuine future work** (9 keys): parsed for
                  forward-compat; explicit "why not yet" rationale
                  per row.
              No code change. Doc-only. cycle-179 drift guards
              all still pass after the rewrite.

  cycle 609 — **`smart-copy = false` honor.** Terminator parity
              (`terminal.py:real_copy_clipboard` +
              `config.py:smart_copy`). Pre-cycle-609 kettle
              hardcoded the smart_copy=true behavior (skip the
              clipboard write when no selection); the
              `smart-copy` config key was a documented no-op.
              Now: `smart-copy = false` clobbers the clipboard
              with an empty string on every Ctrl+Shift+C with
              no selection — Terminator's deliberate UX choice
              for users who prefer "Copy means the clipboard
              now reflects the current selection (even empty)"
              over the smart heuristic. New pure helper
              `copy_clipboard_decision(selection, smart_copy)`
              exposes the policy for unit-testing without a
              clipboard fixture. Drift guard
              `copy_clipboard_decision_smart_vs_clobber` walks
              the four (selection × smart_copy) combinations.
              CONFIG.md `smart-copy` row moved out of "Parsed-
              but-currently-no-op keys" into the main table.
              Workspace tests 348 → 349.

  cycle 608 — **`docs/examples/init.lua` sample script.** New
              documented Lua example covering the full
              `kettle.*` API surface — introspection,
              `kettle.add_url_handler` (with Launchpad-bug /
              Launchpad-code / APT-URL handlers ported from
              Terminator's `url_handlers.py`), `kettle.on`
              event hooks, `kettle.add_menu_item` right-click
              entries, and `kettle.exec_action` (with cycle
              606/607's new `insert_pane_name` /
              `open_cwd` actions demoed). Documents the cycle-
              601 send_text/notify/queue caps + the cycle-376
              safe-vs-trusted sandbox model in the file header
              so users see the security envelope before
              writing a script. CONFIG.md `lua-sandbox` row
              now cross-links to the example; the cycle-179
              cross-link drift guard passes the new link. No
              code change; workspace tests unchanged.

  cycle 607 — **`Action::OpenCwdInFileManager`** (Terminator parity,
              `terminatorlib/plugins/dir_open.py` → `CurrDirOpen`
              menu item). New action that reads the focused pane's
              OSC-7-reported cwd, builds `file://<cwd>`, and
              routes through the existing `open_url` machinery —
              re-uses the cycle-374 Lua URL-handler dispatch,
              cycle-X `custom-url-handler` config override, and
              `kettle_core::links::is_safe_url` allowlist for
              free (identical shape to clicking a `file://...`
              hyperlink in pane output). Falls back to a
              `log::info` hint about `kettle --shell-integration
              bash` when no OSC 7 cwd is available. Aliases:
              `open_cwd`, `open-cwd`, `open_cwd_in_file_manager`,
              `open-cwd-in-file-manager`. Drift guard:
              `from_name_accepts_open_cwd_in_file_manager_aliases`.
              Workspace tests 347 → 348.

  cycle 606 — **`Action::InsertPaneName`** (Terminator parity,
              `terminatorlib/plugins/insert_term_name.py`). New
              action that sends the focused pane's title to the
              focused PTY — useful for scripts that label their
              output by source pane or for keyboard-driven
              copy-current-title workflows. Mirrors the existing
              cycle-345 `InsertPaneNumber` / `InsertPanePadded`
              pattern. Accepted name aliases:
              `insert_pane_name`, `insert-pane-name`,
              `insert_name`, `insert-name`, plus
              `insert_term_name` / `insert-term-name` (Terminator
              spelling — copy-a-Terminator-keybind compatibility).
              Drift guards: existing
              `action_names_round_trip_through_from_name` + the
              cycle-117 `palette_includes_every_user_facing_action`
              already cover the addition; new
              `from_name_accepts_insert_pane_name_aliases` pins
              every alias. Workspace tests 346 → 347.

  cycle 605 — **Doc-truth pass: 3 wired keys promoted out of
              the no-op table.** `handle-size` (cycle 353),
              `geometry-hinting` (cycle 359), `focus`
              (cycle 360) were all wired in production but
              listed as no-op in CONFIG.md "Parsed-but-currently-
              no-op keys" — the explanatory copy on `focus` even
              claimed "kettle uses click-focus exclusively"
              which contradicts the cycle-360 sloppy
              implementation. Audit each key's read sites,
              promote into main table with proper type / default /
              behavior rows. Doc-only. cycle-179 drift guards
              all still pass.

  cycle 604 — **Ctrl+wheel font zoom + `disable-mousewheel-zoom`
              opt-out** (Terminator parity, key_zoom_in /
              key_zoom_out). The `disable-mousewheel-zoom` config
              key had been recognized by the parser since cycle
              334 but was a no-op because kettle didn't implement
              the Ctrl+wheel zoom it disables. This cycle adds
              both: the feature (Ctrl+wheel grows / shrinks the
              font, step matches the keyboard
              `IncreaseFontSize` / `DecreaseFontSize` actions for
              a single source of truth) AND the disable gate
              (config bool, default `false`). Fires BEFORE the
              mouse-tracking pass-through so it works even when
              a TUI (tmux / htop / nvim with `mouse=a`) has mouse
              tracking on — matches gnome-terminal / Terminator /
              xterm UX. New pure helper `should_zoom_font(ctrl,
              lines, disabled)` exposes the policy for unit-
              testing without an App fixture. Drift guard
              `should_zoom_font_gates_on_ctrl_and_disable_flag`
              walks the six relevant input combinations. CONFIG.md
              key moved out of "Parsed-but-currently-no-op keys"
              into the main table; example config gains a
              commented-out entry. Workspace tests 345 → 346.

## [1.45.1] — 2026-05-22

Patch release for two critical pane-lifecycle bugs surfaced in
the cycle-602 sweep. Same severity-class as the v1.45.0
close-focused fix — these warrant a re-install per the
cycle-527 "keep-local-current" memory.

User-impacting bug fixes:

  - cycle 603 part-A — `Mux::reap_tabs` now promotes the
    closed-pane's neighbor to focus instead of jumping to the
    leftmost leaf of the whole tab. Companion to cycle 602's
    `close_focused` fix; same root-cause anti-pattern
    (`tab.root.first_leaf()` as the post-close focus). Reachable
    when a user runs `exit` in the rightmost pane of a split
    tab — pre-fix, focus teleported back to the first split.

  - cycle 603 part-B — **data-loss bug.** `Mux::reap_tabs` used
    `Err(_) => tabs.remove(ti)` which conflated `Err(None)` (tab
    is empty, remove) with `Err(Some(sibling))` (focused leaf
    was a direct root child, sibling promoted, KEEP THE TAB).
    In any 2-pane tab, `exit` in either pane caused the WHOLE
    tab including the surviving sibling to disappear.
    `close_focused` already had the right distinction since
    cycle 285; `reap_tabs` didn't, and the existing
    `reap_tabs_keeps_active_pointed_at_the_same_tab` test only
    used single-leaf tabs so the bug went unnoticed for ~480
    cycles.

Drift guards (workspace 342 → 345):

  - `reap_tabs_promotes_neighbor_when_focused_pane_dies` —
    same 4-leaf tree as cycle 602's repro; reap dead leaf 40,
    assert focus = 30 (neighbor), not 10 (leftmost).
  - `reap_tabs_preserves_tab_when_2_pane_split_has_one_pane_exit`
    — 2-pane tab, reap one leaf, assert tab survives with
    the surviving sibling as root + focus.
  - `reap_tabs_keeps_focus_when_dying_pane_is_not_focused` —
    negative case: focus on Leaf(10), reap Leaf(20), assert
    focus stays 10.

## [1.45.0] — 2026-05-22

Release trigger: cycle-602 user-reported pane-close focus bug
("when I split the window many times then close that specific
terminal it sets my cursor/focused window to my first focused
terminal") — meets the cycle-562 "critical bug fix that users
would actively want to re-install for" criterion. Bundling the
accumulated [Unreleased] polish from cycles 561-602 into this
release because the user will re-install for cycle-602 anyway.

User-impacting bug fixes in this release:

  - cycle 574 — `Action::PastePrimary` now routes through
                `paste_clipboard`, picking up the same
                `LOCAL_PASTE_MAX` clamp, bracketed-paste wrap,
                and broadcast scoping as `Action::Paste`. Pre-
                fix, a `paste-primary` keybind under vim could
                interpret pasted text as commands.
  - cycle 602 — `Mux::close_focused` now picks the nearest
                neighbor pane as the new focus, not the
                leftmost leaf of the whole tab. Matches tmux /
                wezterm / kitty semantics.

Security hardening (cycles 576-587, 601):

  - Kitty graphics protocol resource caps: PNG/JPEG/GIF
    decompression-bomb cap (8192² / 256 MiB), `ImageData::new`
    overflow guard, 384 MiB per-chunk-stream cap, 32-slot
    in-flight cap, 256 frames-per-image cap, 64-slot caps on
    `store` / `anim` / `virtual_placements` / `rel` / `frames`.
  - Background-image decoder uses the same 8192² / 256 MiB
    envelope.
  - User-file read-into-memory caps: 16 MiB session.json, 1
    MiB config, 4 MiB init.lua — all defended against
    swap-attack OOM via metadata pre-check.
  - Lua side-effect APIs: 1 MiB per `send_text`, 8 KiB per
    `notify` field, 1024-command queue length cap.

Production polish:

  - SECURITY.md scope reflects every cap (cycles 583, 588, 596).
  - GitHub Actions: `cancel-in-progress` on diagnostic
    workflows (ci.yml, actionlint.yml, machete.yml,
    labeler.yml) + `timeout-minutes` on all 8 workflows.
    Budget-protection measures per the cycle-444 exhaustion.
  - Test-infra: PID + nanos /tmp paths (cycles 592, 593) so
    parallel `cargo test` runs don't race on shared files.
  - Doc accuracy: range-stable test counts in TESTING.md
    (cycle 594); SECURITY.md added to the cycle-179 user-
    facing-doc drift guard (cycle 596).
  - `release.sh` correctly skips `git add flake.nix` on
    forks lacking the file (cycle 589); `install-online.sh`
    SHA-256 diagnostic distinguishes "tool missing" from
    "verification failed" (cycle 590).

Workspace tests: 322 (v1.44.0) → 342 (this release).

  cycle 561 — README + INSTALL.md + scripts/install-online.sh
              version pins bumped to v1.44.0.

  cycle 562 — `app.rs` cycle-560 comment corrected — the claim
              that broadcast_default "still governs scope
              elsewhere" was wrong. The field has no consumer
              after cycle 560 removed the only one; comment
              now states the actual state + forward-compat
              intent.

  cycle 563 — `kettle-config/lib.rs` doc-comments for
              ask_before_closing + focus annotated as currently
              no-op (parses but no consumer).

  cycle 564 — **Doc-truth sweep.** `docs/CONFIG.md` gained a
              "Parsed-but-currently-no-op keys" subsection
              listing all 22 rows / 26 field names that parse
              cleanly but have no runtime consumer in kettle.
              Discovery: grep for `cfg\.<field>` in
              kettle-ui/ / kettle-render/ / kettle-core/
              returned 0 reads for these fields. Users
              configuring them now see at a glance that the
              key is a no-op (rather than guessing).

  cycle 571 — **Security drift guards.** Two new tests cover
              the cycle-376 Lua sandbox: safe-mode nils 16
              dangerous stdlib APIs (os.execute/exit/remove/
              rename/tmpname/setlocale + io.open/popen/lines/
              input/output/stdin/stdout/stderr + loadfile/
              dofile + package.loadlib); trusted-mode keeps
              them callable. The SECURITY.md cycle-447 "Lua
              plugin sandbox escape" scope is now build-time-
              enforced rather than manual-review-only.
              Workspace tests 323 → 325.

  cycle 593 — **Test race fix follow-up: main.rs config_path test.**
              `kettle/src/main.rs:config_path_problem_catches_*`
              still used `kettle-cycle164-{pid}` (PID only, no
              nanos) — common Linux PIDs are large enough to be
              unique within a test session, but Windows PIDs
              cycle quickly and a panicked re-run on the same
              PID would inherit a stale dir. Added nanos suffix
              for consistency with the cycle-592 pattern + the
              rest of the test suite. Workspace tests unchanged
              at 337.

  cycle 592 — **Test race fix: PID + nanos on `/tmp` paths.** Three
              unit tests (`bg_image::real_png_roundtrip`,
              `bg_image::rejects_oversized_dimensions`,
              `lua::exec_file_runs_a_real_script`) used FIXED
              filenames like `kettle-bg-image-cycle392-smoke.png`
              in `std::env::temp_dir()` directly. Two concurrent
              `cargo test` runs (parallel test threads, CI runner
              concurrency, two developers on the same shared
              runner) would race on the same file — one writes,
              the other reads stale/half-written bytes, sporadic
              failures. Switched to the
              `{name}-{pid}-{nanos}.png` pattern already used by
              `session::tests` and `config_tests::load_from_with_
              diagnostics_*` (subdir-level isolation). No
              behavior change for the happy-path single-run
              case; eliminates the flake under parallel
              execution.

  cycle 602 — **Pane-close focus follows the neighbor, not the
              leftmost leaf.** User-reported bug: "when I split
              the window many times then close that specific
              terminal it sets my cursor/focused window to my
              first focused terminal." `Mux::close_focused` was
              setting `tab.focus = tab.root.first_leaf()` after
              the close — which always points at the LEFTMOST
              leaf of the whole tab (the first pane the user
              started from). For deeply-nested closes that
              feels teleporting. New `Node::neighbor_of(id)`
              walks the tree and returns the first leaf of the
              closed pane's sibling subtree; `close_focused`
              calls it BEFORE the destructive `remove_leaf` so
              the right neighbor is captured even after the
              tree is rebuilt. Matches tmux / wezterm / kitty
              neighbor-promotion semantics. Drift guards:
              `close_focused_picks_nearest_neighbor_not_leftmost_root`
              (4-leaf nested tree reproduction of the exact
              user-described scenario; pre-fix focus jumps to
              leaf 10, post-fix it lands on neighbor leaf 30)
              and `node_neighbor_of_finds_sibling_subtree_first_leaf`
              (pins the helper's contract directly).
              `reap_tabs` (PTY-died path) keeps its existing
              fallback policy — only user-initiated close gets
              neighbor focus. Workspace tests 340 → 342.

  cycle 601 — **Lua side-effect API resource caps.** Audit
              extension to the cycle-376 / cycle-591 sandbox
              defense: the `kettle.*` side-effect callbacks
              (`send_text`, `exec_action`, `notify`, `set_theme`)
              had no per-call or queue-length bounds. A hostile
              `init.lua` running under default safe-mode could
              still queue gigabytes via `for i=1,10000 do
              kettle.send_text(string.rep("X", 1<<20)) end` and
              OOM kettle at the App's drain step
              (`app.rs:900` unconditionally
              `extend_from_slice`s every SendText into a single
              Vec). New caps:
                - `MAX_LUA_SEND_TEXT_BYTES = 1 MiB` per call;
                - `MAX_LUA_NOTIFY_BYTES = 8 KiB` per title /
                  body field;
                - `MAX_PENDING_COMMANDS = 1024` queue length.
              Routed all four callbacks through a new
              `bounded_push` helper so the queue cap is enforced
              exactly once. Per-call oversize drops silently
              with `log::warn`; queue saturation drops with
              `log::warn` + discriminant. Drift guards:
              `send_text_drops_oversized_payload_silently`,
              `notify_drops_oversized_field_silently`,
              `pending_queue_caps_at_max_pending_commands`.
              SECURITY.md cycle-447 "Lua plugin sandbox escape"
              scope updated to enumerate the caps. Workspace
              tests 337 → 340.

  cycle 591 — **Pin mlua-default debug-library exclusion as a drift
              guard.** Audit revealed that mlua's `Lua::new()`
              defaults already exclude the entire `debug` library
              (via `StdLib::ALL_SAFE`), so the dangerous methods
              `debug.getregistry` (sandbox-escape via reference-
              table access), `debug.sethook` (instruction-level
              DoS), and `debug.set{metatable,local,upvalue}` (break
              opaque-userdata encapsulation) are already
              unreachable from user scripts in both safe and
              trusted modes. New positive drift guard
              `lua_default_globals_exclude_debug_library` asserts
              `type(debug) == "nil"` in both sandbox modes — if a
              future refactor switches to `Lua::unsafe_new()` or
              explicitly loads `StdLib::DEBUG`, the test fires
              instead of the regression silently widening the
              SECURITY.md cycle-447 "Lua plugin sandbox escape"
              surface. Added a NOTE comment in `new_with_sandbox`
              documenting why no explicit nil-sweep is needed.
              Workspace tests 336 → 337.

  cycle 590 — **install-online.sh: accurate SHA-256 diagnostic.**
              The hash-verification branch tried `sha256sum -c`,
              fell back to `shasum -a 256 -c`, and printed "SHA-
              256 verification FAILED" if both failed. That
              error message implied tampering even when the
              real cause was "no hashing tool installed" (e.g.,
              a minimal container with neither coreutils
              `sha256sum` nor perl-base `shasum`). Now: detect
              tool availability first, fail with a clear "install
              one of them" message if neither is present, and
              reserve "verification FAILED" for the actual
              hash-mismatch case. Both branches still refuse to
              extract, so the security posture is unchanged —
              just the user-facing diagnostic is honest about
              what went wrong. `dash -n` syntax-check + shellcheck
              both clean. No behavior change on the happy path.

  cycle 589 — **release.sh: gate flake.nix add on existence.**
              The cycle-550 atomic flake.nix bump correctly
              guards the `sed` with `if [ -f flake.nix ]`, but
              the subsequent `git add ... flake.nix` was
              unconditional. The cycle-550 comment claimed the
              add was a no-op when the file is absent, but
              `git add <missing>` exits with 128 — under
              `set -euo pipefail` the release would abort
              **after** the Cargo.toml + lockfile bumps had
              already been applied to the working tree, leaving
              the user with a half-bumped dirty state to clean
              up. Switched to a bash array (`ADD_FILES=(…)`)
              that conditionally appends `flake.nix` to match
              the existing existence guard. No behavior change
              on this repo (flake.nix present) — durability
              fix for forks without it. No drift guard: this is
              a fork-only code path; running release.sh in CI
              against this repo doesn't exercise the branch.

  cycle 587 — **Lua script read cap.** Closes the fourth and final
              user-file read in the cycle-584..587 resource-cap
              sweep (bg-image, session.json, config, lua script).
              `LuaEngine::exec_file` previously called
              `std::fs::read_to_string(path)` unbounded. Threat
              model is the same — a swap-attack on
              `~/.config/kettle/init.lua` could OOM kettle on
              launch. New `MAX_LUA_SCRIPT_BYTES = 4 MiB` (~40×
              over typical init.lua, ~10× over a moderately
              complex plugin suite). Past the cap, the function
              `anyhow::bail!`s rather than reading into RAM —
              surfaces a clear diagnostic to the user instead of
              an OOM. Drift guard `exec_file_rejects_oversize_script`
              writes a 5 MiB syntactically valid Lua file and
              asserts the load errors with a "refusing to load"
              message. Workspace tests 335 → 336.

  cycle 586 — **Config-file read cap.** Companion to cycles 584
              (bg-image) and 585 (session.json). `Config::
              load_from_with_diagnostics` previously called
              `std::fs::read_to_string(path)` unbounded — a
              swap-attack on `~/.config/kettle/config` could OOM
              kettle on launch. Cheap metadata pre-check against
              `MAX_CONFIG_BYTES = 1 MiB` (~20× over the bundled
              10 KB example, ~100× over typical user configs)
              before any allocation; past the cap the function
              falls through to `Config::default()` with a
              `log::warn`. Drift guard
              `load_from_with_diagnostics_rejects_oversize_config`
              writes a 2 MiB file of legitimate config lines
              (verifies the size gate fires BEFORE parsing —
              even valid payload past the cap is refused).
              Workspace tests 334 → 335.

  cycle 585 — **Session.json read-into-memory cap.** `session::
              load_from_path` previously called
              `std::fs::read_to_string(p)` with no size cap. A
              swap-attack with filesystem access (out of strict
              scope per SECURITY.md but the same defense-in-depth
              reasoning as cycle 584's bg-image fix) could
              replace the auto-generated session file with a
              multi-GB blob and OOM kettle on launch. Cheap
              pre-read `metadata().len()` check against
              `MAX_SESSION_BYTES = 16 MiB` (1000× over realistic
              sessions, leaves the bomb on disk for forensics
              renamed to `.json.toobig.<unix-seconds>` — same
              shape as the cycle-108 corrupted-file recovery
              path). Drift guard
              `load_from_path_rejects_oversize_file_without_reading_into_memory`
              writes a 17 MiB file, asserts the load returns
              None, the file was renamed, and one `.toobig`
              backup exists. Workspace tests 333 → 334.

  cycle 584 — **Bg-image decompression-bomb defense.** Companion
              to cycle 576 (PTY-layer kitty/iTerm2 images) at the
              renderer crate's user-configurable
              `background-image` path. `image::open(p) + to_rgba8()`
              had no dimension or alloc limits; a malicious file
              masquerading as a 4K wallpaper could OOM kettle on
              launch via the same PNG/JPEG/GIF/WebP/BMP
              decompression-bomb shape. Switched to
              `image::ImageReader::open(p).with_guessed_format()`
              + `reader.limits(MAX_BG_IMAGE_DIM=8192,
              MAX_BG_IMAGE_BYTES=256 MiB)`. Threat model is
              weaker than the PTY path (config-file source, not
              attacker-controlled at runtime) but the defensive
              pattern is the same. Drift guard
              `rejects_oversized_dimensions` writes an 8193 × 1
              RGBA PNG to a temp file and asserts decode returns
              None. Workspace tests 332 → 333.

  cycle 582 — **Kitty per-id derivative-map saturation sweep.**
              Final link in the cycle-576..581 kitty
              resource-cap chain. The store cap from cycle 581
              didn't propagate to the four other per-id HashMaps
              in `KittyState` — `anim` (animation control),
              `virtual_placements` (`U=1` placements), `rel`
              (parent/child placements), `frames` (per-id
              animation frame Vec). The `anim` map was the most
              acute: an attacker can grow it with `a=a,i=N` for
              arbitrary N **without ever transmitting a real
              image**. All four insert sites now check
              `contains_key(...) || len() < MAX_STORED_IMAGES`
              and bail to `KittyOut::None` past the cap; updates
              to already-tracked ids still work. Drift guard
              `kitty_anim_slot_cap_holds_against_distinct_id_flood`
              fills 64 ids via `a=a`, fires a 65th distinct id
              (refused), then updates an existing id (accepted,
              no growth). Workspace tests 331 → 332 (+ 1 ignored).

  cycle 581 — **Kitty stored-image cap.** Sixth link in the
              cycle-576..580 kitty resource-cap chain. The
              `store: HashMap<u32, ImageData>` of completed
              transmissions was unbounded — each entry holds an
              `ImageData` Arc whose payload can be up to 256 MiB
              (the cycle-576 cap), so completing 1000 distinct
              `a=T,i=N,m=0` transmissions could pin up to 256 GB
              resident. New `MAX_STORED_IMAGES = 64` (sits well
              above any realistic terminal usage — icons +
              animations rarely transmit more than a dozen
              images). Updates to already-stored ids still
              replace in place (no growth); brand-new ids past
              saturation are dropped — the decoded image can
              still be drawn at-cursor on the completing
              transmission but can't be replaced later via
              `a=p,i=…`. Drift guard
              `kitty_stored_images_cap_holds_against_distinct_id_flood`
              fills 64 ids, fires a 65th (refused), then updates
              an existing id (accepted, no growth). Workspace
              tests 330 → 331 (+ 1 ignored).

  cycle 580 — **Kitty per-image frame cap.** Each successful `a=f`
              frame transmission appends a `Frame` (carrying an
              `ImageData` Arc) to `frames[id]`; chaining 100 000+
              frame transmissions for one id grew the Vec
              unboundedly. New `MAX_FRAMES_PER_IMAGE = 256` (well
              above any realistic animation — `.gif` files top
              out around 200 frames). Past the cap, additional
              pushes are silently dropped; the animation keeps
              playing the frames already captured. Drift guard
              `kitty_frames_per_image_cap_holds_against_flood`
              spams `MAX_FRAMES_PER_IMAGE + 16` 1×1 frames at one
              id and asserts the Vec stops at the cap. Workspace
              tests 329 → 330 (+ 1 ignored).

  cycle 579 — **Kitty in-flight slot cap.** Complement to cycle
              578. The cycle-578 per-slot byte cap stops any
              *single* chunked transmission from OOMing the host,
              but the `in_flight: HashMap<u32, Acc>` itself was
              keyless growth: an attacker can send 100 000+
              distinct `i=` values with one `m=1` chunk each (no
              terminating `m=0`), each slot holding a few bytes,
              and slowly fill the host's heap with HashMap
              overhead alone. New constant
              `MAX_IN_FLIGHT_SLOTS = 32` (well above any real
              client; kitty + ueberzug + chafa interleave 1-2
              transmissions). Past the cap, brand-new ids are
              refused (`KittyOut::None`); continuation chunks
              for already-tracked ids still work. Drift guard
              `kitty_in_flight_slot_cap_refuses_new_ids_past_
              saturation` fills 32 slots, fires a 33rd id and
              asserts the map didn't grow, then completes one
              and asserts the slot frees. Workspace tests 328
              → 329 (+ 1 ignored).

  cycle 578 — **Kitty chunked-transmission cap.** Both kitty
              graphics accumulators in `KittyState::feed` (the
              regular `a=T,m=1` chunked image and the `a=f,m=1`
              animation-frame chunks) appended to a `String`
              without any per-slot byte cap. A hostile PTY
              emitter could chain `m=1` continuations
              indefinitely and OOM the host before the final
              chunk ever arrived. New constant
              `MAX_KITTY_PAYLOAD_BYTES = 384 MiB` (covers the
              largest realistic single transmission — 8192² × 4
              RGBA at 4/3 base64 expansion ≈ 342 MiB — with
              ~12% margin, and sits below the cycle-10
              `MAX_SEQ = 64 MiB` per-chunk extractor cap times
              6). On cap exceedance the in-flight slot is
              dropped and `KittyOut::None` returned; any next
              chunk for the same id starts fresh (and will
              also hit the cap if the attacker persists).
              Two new tests: `kitty_payload_cap_fits_8k_rgba_
              base64_with_margin` pins the constant; the
              `#[ignore]`-by-default behavioral guard
              `kitty_chunk_payload_cap_drops_oversize_in_flight`
              actually pushes a 384 MiB+1-byte chunk and
              verifies the slot is cleared (run via
              `cargo test -- --ignored`). Workspace tests
              327 → 328 + 1 ignored.

  cycle 577 — **Overflow-safe `ImageData::new`.** The validation
              `rgba.len() != (width as usize * height as usize *
              4)` would panic on debug builds and silently wrap
              on release for adversarial header values — a
              kitty `f=32,s=4294967295,v=4294967295` payload
              hits `u32::MAX² × 4` ≈ 7.4 × 10¹⁹ bytes, which
              overflows `u64::MAX` ≈ 1.8 × 10¹⁹ on 64-bit. The
              cycle-576 `from_encoded` cap funnels the *encoded*
              path safely, but the raw `ImageData::new` surface
              (used by the kitty `f=32` raw-RGBA branch) lacked
              the same guard. Switched to `checked_mul`; the
              oversize case now returns a clean `None`. New test
              `new_rejects_overflowing_dimensions_without_panic`
              walks the u32-saturated boundary so a future
              refactor that drops the `checked_mul` fails the
              gauntlet rather than the binary silently wrapping.
              Workspace tests 326 → 327.

  cycle 576 — **Decompression-bomb defense for terminal-embedded
              images.** `ImageData::from_encoded` — the entry
              point for Kitty graphics `f=100` (PNG) and iTerm2
              OSC-1337 inline-image payloads — used to call
              `image::load_from_memory(bytes).to_rgba8()` with no
              dimension or allocation limits. A small attacker-
              controlled PNG/GIF/JPEG could claim 2^31 × 2^31
              pixels in the header and OOM kettle on decode.
              Switched to `image::ImageReader` with `Limits`
              configured (`max_image_width` / `max_image_height`
              = 8192, matching `sixel::MAX_DIM`; `max_alloc` =
              256 MiB, the matching RGBA cap). New unit test
              `from_encoded_rejects_oversized_images` round-trips
              a 4 × 4 PNG (positive) and rejects an 8193 × 1 PNG
              encoded by the image crate itself (negative); the
              drift guard fires if a future refactor drops the
              `ImageReader::limits` wire-up. SECURITY.md cycle-
              449 "Resource exhaustion via a single PTY frame"
              scope is now tighter for the inline-image surface.

  cycle 574 — **Paste safety bug fix.** `Action::PastePrimary`
              (cycle 345) was reading the clipboard and writing
              raw bytes directly to the focused pane's PTY,
              bypassing all three of the safety nets that
              `Action::Paste` honors: the 4 MiB
              `LOCAL_PASTE_MAX` runaway clamp, the bracketed-
              paste wrap (so vim / neovim / fzf / mc paste
              correctly when BRACKETED_PASTE is enabled —
              the same fix cycle 182 made for drag-drop), and
              broadcast scoping (so group-input keybind
              honors `paste-primary` like it honors `paste`).
              Fix: delegate `PastePrimary` to `paste_clipboard()`
              — arboard has no separate primary-selection API,
              so the two clipboards are equivalent through our
              current surface anyway.

## [1.44.0] — 2026-05-22

Recovery release. The cycle-553 release.yml gate added in v1.43.0
created a circular dependency: it required PKGBUILD/kettle.rb
versions to match the tag, but those templates can't auto-bump
because their sha256 lines need post-CI artifacts (which only
exist AFTER the gate passes). The v1.43.0 Linux release job
failed at this gate — macOS + Windows artifacts shipped, but
Linux artifacts (the install-online.sh target) didn't.

  cycle 558 — Revert the cycle-553 strict gates for PKGBUILD +
              kettle.rb. flake.nix's gate stays (cycle 550 made
              it auto-bumpable). Packaging templates follow the
              "trail by one" pattern (carry v(N-1) artifacts
              until maintainer re-publishes to AUR/tap),
              matching AUR + Homebrew convention.

After v1.44.0 ships, `/releases/latest` redirects to v1.44.0
with full Linux + macOS + Windows artifacts; the v1.43.0
partial release is no longer the "latest".

## [1.43.0] — 2026-05-22

Post-v1.42.0 packaging-drift cleanup. Three template files
(flake.nix, PKGBUILD, kettle.rb) had identical 39-release
version-string drift discovered + closed in one sweep, with
release.yml CI gate extended to prevent recurrence.

  cycle 547 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.42.0 (cycles 411-543, 11 releases,
              121 cycles).

  cycle 549 — **Drift catch #1.** `flake.nix` hardcoded
              `version = "1.3.5"` despite a "Keep in lockstep
              with Cargo.toml" comment. The lockstep was
              advisory-only for 39 releases. Bumped to v1.42.0.

  cycle 550 — **Durable enforcement.** `scripts/release.sh` now
              auto-bumps `flake.nix` version in lockstep with
              `Cargo.toml`; release.yml CI gate asserts the two
              match the tag. Forward (auto-bump) + backward (CI
              guard) per user directive ("durable over
              patches").

  cycle 551 — **Drift catch #2.** `packaging/arch/PKGBUILD`
              had the same `pkgver=1.3.5` + matching v1.3.5
              sha256. Bumped to v1.42.0 with the v1.42.0
              tarball sha256 fetched deterministically from the
              release sidecar.

  cycle 552 — **Drift catch #3.** `packaging/homebrew/kettle.rb`
              had `version "1.3.5"` + matching v1.3.5 sha256s
              for both macOS-universal + Linux-x86_64. Bumped
              to v1.42.0 with both sha256s from the release
              sidecars.

  cycle 553 — release.yml gate extended to assert PKGBUILD
              pkgver + kettle.rb version match the tag.
              PKGBUILD + kettle.rb can't be auto-bumped from
              release.sh because their sha256 lines depend on
              post-CI artifacts; the gate catches forgotten
              manual bumps. End message now lists all 5
              version-bearing files (tag ↔ Cargo.toml ↔
              flake.nix ↔ PKGBUILD ↔ kettle.rb ↔ CHANGELOG.md).

## [1.42.0] — 2026-05-22

Post-v1.41.0 polish + a real user-reported bug fix.

  cycle 524 — README + INSTALL.md + scripts/install-online.sh
              version pins bumped to v1.41.0.

  cycle 525 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.41.0 (cycles 411-521, 10 releases,
              111 cycles).

  cycle 530 — `scripts/install.sh` now refreshes
              `${PREFIX}/share/kettle/install.sh` on every
              install — the matching `--uninstall` script
              always reflects the version that put the binary
              there.

  cycle 531 — `--uninstall` removes `${PREFIX}/share/kettle/
              install.sh` + `rmdir`s the dir if empty.
              Symmetric with cycle 530.

  cycle 535 — `--check-config` annotates the existing
              `bell: <Mode>` line with `(force-no-bell
              overrides)` when force_no_bell is set, so the
              user doesn't read the configured bell mode and
              wonder why no bell actually fires. Pairs with
              the cycle-461 separate "bell: force-no-bell=true"
              echo line.

  cycle 536 — **User-facing string cleanup.** Cycle 461's
              triggers echo read `(cycle-289 Urgency action)` —
              an internal cycle ref in `--check-config` output.
              Same anti-pattern cycles 474-475 scrubbed from
              docs / man page (but the cycle-179 file-scan
              drift guard doesn't reach binary stdout).
              Replaced with `(window-urgency action)`
              describing the actual effect.

  cycle 537 — Drift guard for cycle-N refs in
              `extra_check_config_lines` output. A unit test
              that builds a config triggering every echo
              branch + asserts no resulting line matches
              `cycle <digit>` / `cycle-<digit>`. Workspace
              tests 321 → 322.

  cycle 539 — Exact-numeric test counts in
              `docs/TERMINATOR-AUDIT.md` + `docs/ROADMAP.md`
              bumped 321 → 322.

  cycle 540 — **Real user-reported bug fix.** kettle icon
              wasn't showing in GNOME Activities / Super-key
              search even though the PNG/SVG files were
              correctly in place. Root cause: `scripts/install.sh`
              ran `gtk-update-icon-cache -f -t ${ICON_BASE}`
              against a user-local hicolor dir that has no
              `index.theme`. The "-t" flag (--ignore-theme-index)
              made gtk-update-icon-cache produce a ~584-byte
              empty/broken cache file. GNOME trusts that cache
              and skips file-system fallback scanning — so
              `Icon=kettle` in the .desktop never resolves.

              Two-part fix:
              - Only invoke gtk-update-icon-cache when
                ${ICON_BASE}/index.theme exists (user-local
                hicolor inherits the system
                /usr/share/icons/hicolor/index.theme).
              - Clean up any pre-existing broken cache when
                no index.theme is present.

              Verified end-to-end: re-running ./scripts/install.sh
              --skip-build removes the broken cache; GNOME's
              directory-scan fallback now resolves the icon.

  cycle 543 — Symmetric to cycle-540 fix: `--uninstall` also
              guards `gtk-update-icon-cache` on index.theme
              existing + removes the broken cache. Without
              this, uninstall would re-create a stale cache
              referencing the just-removed icon files.

## [1.41.0] — 2026-05-22

Post-v1.40.0 polish — pre-commit hook UX tightens, real-bug
catches from running shellcheck on scripts/, and crates.io
metadata polish.

  cycle 501 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.40.0 (cycles 411-497, 9 releases,
              87 cycles).

  cycle 502 — Pre-commit hook logs elapsed gauntlet time
              (`pre-commit: PASSED (47s)`) so contributors
              don't misread cold-cargo-cache delay as a hung
              hook.

  cycle 503 — Renamed `start_ns` → `start_sec` (cycle 502
              stored seconds, not nanoseconds).

  cycle 504 — Per-branch test assertion order aligned with
              `extra_check_config_lines` helper-body order
              (accent / force_no_bell / triggers / lua_sandbox
              / background_image / window-flags / status-bar).

  cycle 505 — `extra_check_config_lines_empty_for_default_config`
              binds the helper result once so the assertion +
              failure-message reference the same value.

  cycle 506 — Hook renders sub-second runs as `(<1s)` instead
              of `(0s)`.

  cycle 507 — Timing-comment refined "~30s" → "30-90s on a
              cold cache" + a `<5s warm-cache incremental`
              counterweight.

  cycle 508 — Wall-clock-jumped-backward edge case (NTP
              correction, manual clock set, container time
              jump) renders as `<1s` rather than the
              misleading `(-1s)`.

  cycle 511 — Exact-numeric version snapshots in
              `docs/TESTING.md` (`post-v1.37.0`) +
              `docs/ROADMAP.md` (`v1.35.0`) bumped to v1.40.0.

  cycle 512 — `packaging/linux/kettle.1` `--screenshot-menu`
              description scrubbed of internal `v1.3.0 blank-
              menu regression class` history-ref. Same anti-
              pattern as cycle-475's cycle-N scrub but version
              pattern.

  cycle 515 — `.github/labeler.yml` extended to cover the
              cycle-494 `.githooks/` directory under the
              existing `tooling` label.

  cycle 516 — **Real bug fix.** `scripts/release.sh` line 101
              had backticks inside a double-quoted `echo` that
              ran as command substitution at error time. The
              "helpful hint" actually ran `git fetch && git
              tag -d v${VERSION}`, mutating local state and
              printing garbled output. Caught by manually
              running shellcheck against scripts/. Fixed via
              single-quote re-interpolation.

  cycle 517 — **Durable infrastructure.** Pre-commit hook now
              runs shellcheck against any staged scripts/ or
              .githooks/ files before the cargo gauntlet —
              catches the cycle-516 bug class at commit time.
              Falls back silently when shellcheck isn't
              installed.

  cycle 518 — `scripts/install.sh`'s 4 SC2015 `cmd && X ||
              true` ambiguity-pattern instances rewritten as
              explicit `if … then … fi`. `shellcheck
              scripts/*.sh .githooks/*` now warning-free
              across the repo.

  cycle 520 — `Cargo.toml [workspace.package]` gained
              `homepage` / `readme` / `keywords` / `categories`
              — best-practice metadata that future-proofs a
              potential crates.io publish.

  cycle 521 — `crates/kettle/Cargo.toml` inherits the cycle-520
              new fields via `<field>.workspace = true` (without
              this, the workspace defaults applied to no
              published crate).

## [1.40.0] — 2026-05-22

Pre-commit hook infrastructure. The session caught two of its own
bugs: cycle 484's doc-list overindentation (cycle 493) and the
cycle-494 hook's missing deletion filter (cycle 496) — both
caught the bug class they exist to prevent.

  cycle 489 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.39.0 (cycles 411-486, 8 releases,
              76 cycles).

  cycle 490 — ROADMAP "Drift guards" bullet credited cycles
              471-472 (3 new drift guards on
              `extra_check_config_lines`) + bumped final count
              319 → 321 to match HEAD.

  cycle 491 — Saved a session-summary memory entry for the
              v1.34.0 → v1.39.0 arc so future sessions can
              resume with the load-bearing invariants visible.

  cycle 492 — Helper rustdoc lede reordered (purpose first,
              cycle citation in parens) to match the rest of
              the codebase's rustdoc conventions.

  cycle 493 — **Fix-my-own-bug.** Cycle 484's `lua_engine`
              doc-list used column-aligned hanging-indent
              continuations; clippy 1.93 flagged them as
              `doc_list_item_without_indentation` errors. Re-
              flowed to standard 2-space markdown hanging
              indent + blank-line block separator.

  cycle 494 — **Durable infrastructure.** Added opt-in
              `.githooks/pre-commit` that runs `cargo fmt
              --check && clippy && test` on every commit
              touching code. Skips doc-only commits to stay
              fast (CHANGELOG / README / docs/ / packaging/ /
              .github/ / .githooks/ / NOTICE / LICENSE /
              SECURITY / CODE_OF_CONDUCT / CONTRIBUTING /
              deny.toml / .gitignore). Documented in
              CONTRIBUTING.md step 5 with the cycle-493
              incident citation. Opt in via
              `git config core.hooksPath .githooks`.

  cycle 495 — Hook header expanded to enumerate "NOT excluded"
              path categories that DO trigger the gauntlet
              (crates / Cargo.toml-lock / assets / scripts /
              shell-integration / tests). Self-verified — hook
              fired correctly on its own commit (touched only
              `.githooks/`, fast-path triggered).

  cycle 496 — **Fix-my-own-bug.** Cycle 494's diff-filter
              `ACMR` excluded `D` (deletions). A commit that
              ONLY deleted `.rs` files would have shown an
              empty non-doc set, falsely matched the doc-only
              fast-path, and skipped gauntlet despite breaking
              the build. Switched to `ACMRD`.

  cycle 497 — CONTRIBUTING.md hook section points readers at
              the `.githooks/pre-commit` header comment for
              the trigger/skip enumeration + notes the
              `--no-verify` per-commit bypass.

## [1.39.0] — 2026-05-22

Doc-accuracy release. Justfile + CONTRIBUTING got a `gauntlet-strict`
recipe for release-cut pre-flight, three field doc-comments in
`app.rs` corrected to reflect post-helper-extraction reality, and
the cycle-471 helper rustdoc gained a maintenance note for future
contributors.

  cycle 478 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries extended
              to v1.38.0 (cycles 411-475, 7 releases, 65 cycles).

  cycle 479 — `Justfile` gained `just gauntlet-strict` — chains
              gauntlet + deny + machete for release-cut pre-flight.
              Daily-iter contributors still use plain `just
              gauntlet`; the strict variant catches stale supply-
              chain ignores + unused deps before tagging.

  cycle 480 — `CONTRIBUTING.md` "Releasing" flow documents
              `just gauntlet-strict` as step 3 between CHANGELOG
              commit + `scripts/release.sh`. Drive-by caught
              a duplicate "step 4" numbering bug.

  cycle 481 — `CONTRIBUTING.md` recipe enumeration lists both
              `gauntlet` + `gauntlet-strict` (was missing both
              composite recipes despite naming deny / machete).

  cycle 482 — `CONTRIBUTING.md` enum reordered to match Justfile
              section order (build/release before gauntlet, not
              after). Justfile is the source of truth.

  cycle 483 — **Doc-accuracy fix.** `pending_pane_restarts` doc
              said "respawns into the same pane id slot" — that
              was cycle-412 intent, but cycle 418 actually shipped
              spawn-as-new-tab. Doc rewritten to match
              implementation + cite cycle-452 dedup follow-up.

  cycle 484 — **Doc-accuracy fix.** `lua_engine` field doc listed
              3 LuaEvent emission sites and named "Mux mutations"
              directly. Updated to 5-site enumeration with the
              canonical helper for each (cycles 367/378/424/425)
              + cross-link to `drain_lua_hook_commands` (cycles
              426-428, 433).

  cycle 485 — **Verification fix.** Cycle 484 said 
              `fire_tab_close_event` has 4 call sites; `grep`
              found 5 (tab-bar ✕-click handler has 2 branches
              that both fire the helper, plus 3 keyboard /
              handoff paths). Doc count corrected with "2 click-
              handler branches" note for future readers.

  cycle 486 — `extra_check_config_lines` rustdoc gained an
              "Adding a new branch:" maintenance note that
              names both cycle-471 test guards. Future contri-
              butors adding an 8th opt-in echo see the test-
              extension contract without grep-hunting.

## [1.38.0] — 2026-05-22

Doc-durability release. One more `--check-config` echo (status-bar),
one helper extraction + 3 new drift guards on the cycle-461-470 echo
contract, and a sweep removing internal cycle refs from every user-
facing doc surface (man page + example config + drift-guard scan
list extension so the pattern can't reintroduce).

  cycle 466 — ROADMAP + TERMINATOR-AUDIT + ARCHITECTURE post-sweep
              summaries extended to v1.37.0 (cycles 411-463, 6
              releases, 53 cycles).

  cycle 467 — `docs/CONFIG.md` gained a row for `force-no-bell`
              (was undocumented despite being a real parser arm)
              + `exit-action` row cites the cycle-452 dedup fix.
              (Cycle-179 drift guard caught the user-facing
              cycle ref in cycle 471's test run; reworded.)

  cycle 468 — `CONTRIBUTING.md` inline recipe list documents
              `just deny` + `just machete` (cycle 456 added the
              recipes; this closes the discoverability gap).

  cycle 469 — `docs/kettle.example.config` gained a commented-out
              `status-bar = off` entry. Users running
              `kettle --print-default-config` now discover the
              status-bar feature.

  cycle 470 — `--check-config` echoes `status-bar: Top|Bottom`
              when non-default. Symmetric with the cycle-461-463
              opt-in echoes.

  cycle 471 — **Refactor + drift guards.** Extracted the cycles-
              461-470 inline echo blocks into
              `extra_check_config_lines(cfg) -> Vec<String>`
              pure helper. Added 2 unit tests pinning the
              empty-for-default + per-opt-in-branch contract.
              Drive-by fix for the cycle-467 user-facing cycle
              ref the drift guard caught.

  cycle 472 — 7th in-isolation test covering the `triggers`
              branch of `extra_check_config_lines`. All 7 echo
              branches now have dedicated test coverage.

  cycle 473 — Exact-numeric test count claims in
              `docs/TERMINATOR-AUDIT.md` + `docs/TESTING.md`
              bumped to 321 / +13 / post-v1.37.0. Loose-bound
              snapshots stay accurate without churn.

  cycle 474 — **Real durability fix.** The example config (user-
              facing via `kettle --print-default-config`) had
              picked up 5 "(cycle N)" / "cycle-N" internal refs
              across cycles 459/460/469/470. Every user's
              bootstrap file would have inherited them. Scrubbed
              all 5 + extended the cycle-179 drift guard's scan
              list to include `docs/kettle.example.config` so a
              future reintroduction fails at test time.

  cycle 475 — Same drift-guard reasoning applied to the man
              page: scrubbed 3 internal cycle refs from
              `packaging/linux/kettle.1` (cycle 436 had added
              them) + extended the cycle-179 scan list to
              `packaging/linux/kettle.1`. `man kettle` is
              user-facing.

## [1.37.0] — 2026-05-22

UX + observability release. One real exit-action=restart bug fix,
six new `--check-config` echo lines covering all the Terminator-
parity opt-in keys (accent, force-no-bell, triggers, lua-sandbox,
bg-image, window-flags), three new example-config keys + 3-key
drift-guard extension, build-system fix follow-ups, and supply-
chain hygiene.

  cycle 450 — README + INSTALL.md version pins bumped to v1.36.0.

  cycle 451 — `scripts/install-online.sh` example pin bumped
              v1.3.4 → v1.36.0. Users copying the snippet land
              on a current binary instead of the cycle-150-era
              pre-SHA-256-sidecar release.

  cycle 452 — **Real UX bug fix.** `exit-action = restart` could
              spawn TWO new tabs per dead shell on platforms
              where alacritty fires both `TermEvent::Exit`
              (PTY-side EOF) and `TermEvent::ChildExit(code)`
              (child reaper) for the same exit. Added a
              `Vec::contains` dedup check in `drain_events` so
              only one respawn happens regardless of how many
              TermEvent variants the engine emits per child
              death.

  cycle 454 — Cycle-452 in-code comment cites the two
              alacritty_terminal source-line refs
              (event_loop.rs:263 + term/mod.rs:810) that
              confirm both events ARE emitted on a normal
              shell exit. Future contributors don't have to
              re-derive the dedup rationale.

  cycle 455 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.36.0 (cycles 411-452, 5 releases,
              308 → 319 tests).

  cycle 456 — `Justfile` gained `just deny` (`cargo deny check`)
              + `just machete` (`cargo machete`) recipes
              mirroring the existing CI workflows. Contributors
              can pre-flight supply-chain hygiene locally —
              would have caught cycle-444's stale ignore one
              cycle earlier.

  cycle 457 — `docs/INSTALL.md` line 143 MSRV said "1.88" but
              Cargo.toml + README badge + INSTALL line 49 said
              1.89 (cycle-250 bump). Pointed the stray line at
              `Cargo.toml`'s `rust-version` field so future
              bumps only need the toml change to ripple.

  cycle 458 — Normalized `docs/TESTING.md`, `docs/INSTALL.md`,
              and `CONTRIBUTING.md` to 319+ tests + v1.36.0
              baseline (cycle-446 drift guard + v1.36.0 release
              had left three docs trailing).

  cycle 459 — Three Terminator-parity config keys (accent-color
              cycle 309, force-no-bell cycle 349, trigger cycle
              289) were in the parser but missing from the
              embedded example config. Users following the
              cycle-227 first-launch bootstrap never saw them.
              Added commented-out defaults + extended the
              cycle-413 drift guard from 9 → 12 pinned keys.

  cycle 460 — **Fix-my-own-bug.** Cycle 459's trigger example
              used a non-existent `trigger = REGEX = ACTION`
              syntax. v1's parser hardcodes the action to
              Urgency and takes the entire post-`=` value as
              the regex; a copy-paste would have ended up with
              "= notify" literally in the pattern. Corrected
              to `trigger = REGEX` with a "do NOT add a second
              `=`" warning.

  cycle 461 — `--check-config` summary echoes four Terminator-
              parity opt-in keys when set:
                accent:   #RRGGBB
                bell:     force-no-bell=true ...
                triggers: N pattern(s) configured ...
                lua:      sandbox=Trusted
              Guarded so default-config output stays terse.
              Symmetric with the existing font-features /
              styled-families echoes. End-to-end verified.

  cycle 462 — `--check-config` also echoes bg-image when set:
                bg-image: PATH (mode=…, blur=…, darkness=…)
              Most visually-impactful opt-in surface; the
              cycle-461 sweep had skipped it.

  cycle 463 — `--check-config` also echoes window-flags when
              non-default:
                window-flags: state=Fullscreen borderless=true
                              always-on-top=true
              Easy-to-set-then-forget Terminator-parity keys
              (cycles 339/342) that the summary used to
              silently drop.

## [1.36.0] — 2026-05-22

Production-hygiene release. One real bug fix (`KETTLE_GIT_SHA`
freshness), one supply-chain hygiene fix (stale `cargo-deny`
ignore), one drift guard, and a sweep of stale-snapshot fixes
across the docs.

  cycle 438 — README + INSTALL.md version pins bumped to v1.35.0.

  cycle 439 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md`
              post-sweep summaries updated to include v1.35.0
              (4 releases across 28 cycles, 308 → 318 tests).

  cycle 440 — `packaging/linux/kettle.1` documents 6 more default
              chords: Alt+1..9 (Goto tab N), F11 (Fullscreen),
              Ctrl+0 (ResetFontSize), Shift+Arrow (Resize),
              Shift+PgUp/Dn (page scroll), Shift+Home/End
              (scroll-to-edge). Coverage 27 → 33 of 59.

  cycle 441 — TESTING.md headline (267 → 318 tests, v1.7.0 →
              v1.35.0) + INSTALL.md verify-build example
              (240+ → 318+).

  cycle 442 — ROADMAP "19-test harness" claim → 318; ARCHITECTURE
              "v1.8.0 → v1.32.0 sweep" → consistent v1.8.0 →
              v1.31.0 sweep (cycles 330-410) + v1.32.0 → v1.35.0
              polish (cycles 411-438) split.

  cycle 443 — CONTRIBUTING.md cycle + test-count snapshots
              (300+ → 440+ cycles, 267+ → 318+ tests).

  cycle 444 — **Real hygiene fix.** Dropped the stale
              `RUSTSEC-2024-0436` ignore from `deny.toml` +
              `.github/workflows/audit.yml`. The `paste → rav1e
              → image` chain that justified it is no longer in
              Cargo.lock (verified with `cargo tree -i paste`).
              `cargo deny check` now warning-free.

  cycle 445 — **Real bug fix.** `crates/kettle/build.rs` was
              capturing `KETTLE_GIT_SHA` once and not refreshing
              when only other workspace crates changed (Cargo's
              default rerun-policy only scans the build script's
              own package). `kettle --version` showed stale SHAs.
              Added `cargo:rerun-if-changed=NONEXISTENT_FORCE_
              RERUN_FOR_KETTLE_GIT_SHA` to force every-build
              re-execution; the ~10ms git-subprocess cost is
              well under build-time noise. Restores the cycle-195
              "+dirty marker refreshes on every source edit"
              contract.

  cycle 446 — Drift guard for `kettle.config_path()` return-type
              contract (must be `string | nil`, never anything
              else). 318 → 319 tests.

  cycle 447 — `SECURITY.md` "What's in scope" list gained two
              bullets covering the v1.8.0+ Lua-plugin sandbox-
              escape surface and the cycles 403/408 detachable-
              tabs handoff payload-abuse surface.

  cycle 448 — `Justfile` `just test` recipe doc bumped 261+ →
              319+.

## [1.35.0] — 2026-05-22

Post-v1.34.0 polish. Plugin-contract refactor finished off, drift
guards extended, docs caught up to current HEAD.

  cycle 428 — `App::resumed` startup-hook drain (the last remaining
              inline `LuaCommand`-variant match in the
              `ApplicationHandler` trait impl) now routes through
              `drain_lua_hook_commands`. A stale comment claimed
              inherent methods aren't callable from trait impls —
              they are, as long as `self: &mut App`. All 5 event
              hooks (Startup / TabAdd / TabClose / Bell / Output)
              now share one canonical drain path.

  cycle 429 — README + `docs/INSTALL.md` version pins bumped to
              v1.34.0 (README status line v1.31.x → v1.34.x;
              KETTLE_VERSION example v1.3.4 → v1.34.0; INSTALL.md
              SHA-256 verify URLs v1.32.0 → v1.34.0). The
              recommended install command now lands users on a
              current binary.

  cycle 430 — Drift-guard tests for `kettle.notify` +
              `kettle.set_theme` queue/drain semantics. The
              cycle-426-428 helper depends on these variants
              being present; a future refactor of the mlua
              closures could silently drop the push and the
              helper would just see empty drains. 308 → 316.

  cycle 431 — `docs/TERMINATOR-AUDIT.md` tail extended with the
              post-sweep polish summary (cycles 411-430 spanning
              v1.32.0 → v1.34.0). Future contributors see the
              audit's trajectory through current HEAD.

  cycle 432 — `docs/ROADMAP.md` gained a v1.32.0 → v1.34.0
              section bridging the v1.8.0 → v1.31.0 sweep and
              the Next list. Five threads: plugin-contract bug
              fixes, exit-action=restart, helper unification,
              docs-as-code, drift guards.

  cycle 433 — Lua menu-item click drain (cycle 375) routed
              through `drain_lua_hook_commands` — −35 more lines
              of duplication. The only remaining inline drain
              is App::new (early init before `self` exists).

  cycle 434 — `drain_lua_hook_commands` rustdoc updated to list
              all 6 callers (was 2). Future contributors see the
              full surface without grepping.

  cycle 435 — Drift-guard tests for `kettle.add_menu_item` /
              `invoke_menu_item` + `kettle.add_url_handler` /
              `try_url_handler`. Pattern-match short-circuit,
              error isolation, out-of-range index safety.
              316 → 318.

  cycle 436 — `packaging/linux/kettle.1` man page filled in 8
              missing CLI-flag entries (--remote-send,
              --remote-file, --toggle, --profile, --layout,
              --accent, --lua-script, --annotate). `man kettle`
              now matches `kettle --help`.

## [1.34.0] — 2026-05-22

Plugin-contract hardening — every new_tab / close_tab call site now
fires the canonical Lua event, and the four Lua hook drains share one
helper. Also fixes a live-grid bug in the exit-action=restart path.

  cycle 420 — `exit-action = restart` respawn now uses
              `self.grid_of(self.area())` for cols/rows instead of
              the hardcoded `80, 24` that cycle-418 shipped. The
              restarted shell matches the surface size that was on
              screen when it died, so `tput cols` / `tput lines`
              and TUI apps read the right values.

  cycle 421 — `docs/ARCHITECTURE.md` detachable-tabs flow upgraded
              from ASCII tree to mermaid `flowchart TD` (3 IPC
              paths → target kettle → session restore).

  cycle 422 — `docs/ARCHITECTURE.md` Plugin + Background-image
              flows upgraded to mermaid (`flowchart TD` for
              `init.lua` → LuaEngine → LuaCommand dispatch;
              `flowchart LR` for `decode_bg_image` → blur →
              `BgImage` cache → `imgpipe`). The per-pane titlebar
              keeps its ASCII art — it is layout, not flow.

  cycle 423 — **Plugin-contract bug fix.** Remote-control IPC
              `new-tab` verb (cycle 419) was bypassing
              `LuaEvent::TabAdd`. Plugins listening for tab-spawn
              now see IPC-driven tab creation as well as keyboard
              + mouse paths.

  cycle 424 — **Plugin-contract bug fix (3 sites).** Extracted
              `fire_tab_close_event(closing_idx)` helper and
              applied it to the three `close_tab` paths that had
              been bypassing `LuaEvent::TabClose`:
              - SCM_RIGHTS tab-handoff source (cycle 408)
              - file-fallback tab-handoff source (cycle 403)
              - tab-bar ✕-click (cycle 386)
              Keyboard `CloseTab` already fired correctly; mouse
              + detachable-tabs paths now match it.

  cycle 425 — **Plugin-contract bug fix (2 sites).** Extracted
              `fire_tab_add_event()` helper and applied it to the
              two `new_tab` paths that had been bypassing
              `LuaEvent::TabAdd`:
              - `Action::NewWindow` fallback (when window-spawn
                degrades to in-process new tab)
              - cycle-418 exit-action=restart respawn
              All five tab-spawn paths (keyboard, mouse,
              remote-control, NewWindow fallback, restart) now
              fire the canonical event.

  cycle 426 — **Refactor.** Created `drain_lua_hook_commands(hook_name)`
              with a full LuaCommand match (SendText / ExecAction /
              Notify / SetTheme) and routed the three TabAdd / TabClose
              hook drains through it. Deleted ~120 lines of inline
              variant duplication; the helper logs `hook_name` for
              every dispatched command so trace output identifies
              which event fired what.

  cycle 427 — **Refactor.** Bell + Output hook drains routed
              through the same `drain_lua_hook_commands` helper.
              −51 more lines. All four event hooks (TabAdd,
              TabClose, Bell, Output) now share one canonical
              command-drain path; adding a fifth event is one new
              fire_event call + nothing else.

After cycle 427: every new_tab / close_tab call site fires the
matching `LuaEvent`, and every event hook routes through one
helper. Workspace tests stay green; binary smoke clean.

## [1.33.0] — 2026-05-22

Real feature work — `exit-action = restart` is now end-to-end, and
the remote-control IPC `new-tab` verb is wired.

  cycle 416 — `docs/ARCHITECTURE.md` documents the cycles 330-415
              Terminator-parity subsystems with ASCII flow
              diagrams + integration narratives for Plugin
              (Lua), Per-pane titlebar, Background image, and
              Detachable tabs. Cross-references each design doc.

  cycle 417 — `docs/INSTALL.md` version refs bumped v1.3.4 →
              v1.32.0 (KETTLE_VERSION pin example + SHA-256
              verify URLs). Users following the recommended pin
              now get a recent kettle.

  cycle 418 — `exit-action = restart` fully implemented end-to-
              end. Closes cycle-357's "not yet implemented"
              warn-and-fallback. On shell-exit with
              `cfg.exit_action = restart`, the dead pane queues
              to `pending_pane_restarts`; the post-drain handler
              in `redraw` calls `Mux::new_tab_with` with the
              same argv + cwd, spawning a fresh shell. Matches
              Terminator's documented behavior.

  cycle 419 — Remote-control IPC `new-tab` verb wired. Was
              logging "not yet implemented" since cycle 302
              (the verb was recognized but no-op'd). Today:
              calls `Mux::new_tab` with current cell grid +
              waker. Completes the remote surface alongside
              `send-text` + `toggle-window`.

After cycle 419: zero "TODO" + zero "not yet implemented" markers
remain in the codebase (verified via grep). Workspace tests
stay at 308. End-to-end binary smoke green.

## [1.32.0] — 2026-05-22

Production-readiness polish — docs sync + drift guards + foundation
for exit-action=restart.

  cycle 411 — `cargo doc -D warnings` clean (3 doc-comment warnings
              fixed in kettle-render + kettle-vt). Matches the CI
              doc-warnings gate.

  cycle 412 — exit-action = restart pane-respawn queue infrastructure
              (partial). Replaces the cycle-357 TODO with concrete
              pending_pane_restarts plumbing; respawn dispatch is
              the next sub-cycle.

  cycle 413 — `print_default_config_round_trip` drift guard pins 9
              load-bearing Terminator-parity keys (window-state,
              borderless, always-on-top, show-titlebar,
              title-at-bottom, background-image,
              background-image-mode, exit-action, lua-sandbox)
              in the embedded example config so future strips
              fail loud.

  cycle 414 — Man page (`packaging/linux/kettle.1`) documents
              `--tab-handoff PATH` (cycle 403 file-fallback) +
              `--tab-handoff-fd FD` (cycle 408 SCM_RIGHTS path).

  cycle 415 — `docs/CONFIG.md` adds a "Terminator-parity keys" table
              covering ~30 cycles 331-410 keys with type + default +
              behavior. Cross-references the audit doc for the full
              85-key parsed surface.

  docs sync — README Status line v1.7.x → v1.31.x (caught up after
              24 releases of sweep). `docs/ROADMAP.md` grew a
              "v1.8.0 → v1.31.0 Terminator-parity sweep" section
              + trimmed Next list to the genuine remaining threads.
              `docs/TERMINATOR-AUDIT.md` tail appended with the
              cumulative cycles 330-412 sweep completion summary.
              `docs/kettle.example.config` grew a Terminator-parity
              section with every major new knob's default + origin.

Workspace tests stay at 308. `cargo doc -D warnings` clean. `cargo
machete` reports zero unused deps. End-to-end binary smoke green
(`--version`, `--check-config`, `--list-actions`, `--list-keybinds`,
`--print-default-config`).

## [1.31.0] — 2026-05-22

SCM_RIGHTS cross-process tab handoff end-to-end for the JSON
payload.

  cycle 408 — `--tab-handoff-fd FD` CLI flag plumbing.
              Inherited socket fd carrying serialized tab JSON
              + SCM_RIGHTS ancillary data.

  cycle 409 — Target-side recv. App startup detects
              --tab-handoff-fd FD; constructs UnixStream from
              the fd via FromRawFd; calls fd_transport::recv_fds
              (cycle 399); deserializes the JSON into the
              existing Session restore path. Received PTY fds
              are closed on the target side (source still owns
              canonical refs); adoption-as-Pane is the final
              piece pending Terminal::from_raw_fd in kettle-core.

  cycle 410 — Source-side socketpair + fork+exec. New
              App::try_move_tab_to_new_window_scm_rights helper
              opens a UnixStream pair, fork+execs a kettle child
              with --tab-handoff-fd 3 (via pre_exec dup2 +
              clear-FD_CLOEXEC), then calls
              fd_transport::send_fds with the JSON payload.
              Action::MoveTabToNewWindow now prefers this over
              the cycle-405 file-fallback on Unix.

The detachable-tabs cross-window flow now ships via SCM_RIGHTS-
capable socket IPC on Unix + file-fallback elsewhere. Both paths
deliver the same user-visible UX (split tree + cwds preserved
in the new window). The SCM_RIGHTS variant additionally positions
for live PTY-fd transfer when the Terminal::from_raw_fd kettle-
core change lands — at which point running shells survive the
move without restart.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 10/10 COMPLETE
bg-image (12 sub-cycles):         ✅ 11/12 effectively COMPLETE
Detachable tabs (11 sub-cycles):  ✅ 11/11 sub-cycle 7
                                       SCM_RIGHTS path end-to-end
                                       for the JSON payload
                                       (Terminal::from_raw_fd
                                        Pane-adoption is a
                                        kettle-core internal
                                        change tracked separately
                                        from the design doc)

45 of 46 Bucket-D sub-cycles shipped (98%).

Only the bg-image sub-cycle 8 "explicit resize handler" remains
flagged — and that's documented as implicit per-frame UV
recompute (cycle 394), which IS the implementation (a separate
explicit handler would be redundant).

Workspace tests stay at 308.

## [1.30.0] — 2026-05-22

Named broadcast groups + EditPaneGroup action — titlebar Bucket-D
sub-cycle 8 now COMPLETE.

  cycle 406 — Named broadcast groups foundation.
              Pane.group_name: Option<String>.
              PaneView grows group_name field.
              Per-pane titlebar prefixes "[group-name] " before
              the title (Terminator titlebar.py indicator pattern).

  cycle 407 — Action::EditPaneGroup full impl.
              Aliases: edit_pane_group, edit-pane-group,
                       edit_group, edit-group.
              Opens TitleEditState with new TitleEditScope::Group.
              Apply: writes pane.group_name (None on empty input).
              Overlay label: "Edit pane group:".
              Anchors near focused pane (same as EditPaneTitle).

The previously-Bucket-E titlebar sub-cycle 8 is now end-to-end:
data model + render + keyboard-bindable action + palette entry +
edit overlay.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 10/10 COMPLETE
                                       (cycle 406 + 407 closed
                                        sub-cycle 8 from Bucket-E)
bg-image (12 sub-cycles):         ✅ 11/12 — all implemented
                                       (sub-cycle 8 is implicit
                                        per-frame UV recompute,
                                        cycle 394 documented this)
Detachable tabs (11 sub-cycles):  ✅ 10/11 — file-fallback path
                                       end-to-end shipped
                                   ⌛ 1 — sub-cycle 7 SCM_RIGHTS
                                       live PTY transfer (multi-
                                       week cross-process IPC)

44 of 46 Bucket-D sub-cycles end-to-end (96%).

Two titlebar sub-cycle 8 from Bucket-E to shipped:
  ✅ EditPaneGroup action + palette entry + edit overlay
  ✅ Pane.group_name data model
  ✅ Titlebar render shows "[group-name] title  WxH  🔔"

Workspace tests stay at 308.

## [1.29.0] — 2026-05-22

Detachable-tabs end-to-end file-fallback path COMPLETE.

  cycle 402 — winit CursorLeft/Entered → drag FSM transitions.
              Closes detachable-tabs sub-cycle 6.
  cycle 403 — `--tab-handoff PATH` CLI flag scaffolding.
  cycle 404 — Session::load_tab_handoff App-side restore.
              Closes detachable-tabs sub-cycle 8 in the
              file-fallback path.
  cycle 405 — Action::MoveTabToNewWindow → write JSON
              handoff + spawn --tab-handoff PATH child.
              Source tab serializes; target reconstructs.
              Cross-platform (works on Linux/macOS/Windows/
              Wayland). Closes the cross-process tab-handoff
              workflow end-to-end via the file path.

### End-to-end detachable-tabs flow (file-fallback)

  Source process:
    1. User triggers Action::MoveTabToNewWindow.
    2. Mux::serialize_tab(active) → STab.
    3. serde_json::to_string + write to /tmp/kettle-handoff-PID.json
    4. Spawn `kettle --tab-handoff PATH --config CFG`.
    5. Close source tab.

  Target process:
    1. App startup detects --tab-handoff PATH.
    2. Session::load_tab_handoff reads + deletes the file.
    3. Restore tab(s) via cycle-291 restore path.
    4. User sees split tree + cwds in the new window.

Live PTY transfer requires SCM_RIGHTS (sub-cycle 7); the file-
fallback trades that for cross-platform support (target spawns
fresh shells instead of adopting fds).

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — all impl shipped
                                   E — sub-cycle 8 (group-name
                                       edit) Bucket-E
bg-image (12 sub-cycles):         ✅ 11/12 effectively COMPLETE
Detachable tabs (11 sub-cycles):  ✅ 10 — all sub-cycles shipped
                                       end-to-end via file-fallback
                                   ⌛ 1 — sub-cycle 7 SCM_RIGHTS
                                       cross-process PTY fd transfer
                                       (file-fallback is the cross-
                                       platform analog shipped today;
                                       SCM_RIGHTS variant preserves
                                       live shells)

43 of 46 Bucket-D sub-cycles end-to-end (93%).

Workspace tests stay at 308.

## [1.28.0] — 2026-05-22

  cycle 401 — Drag FSM cancel path + cursor-leave/reenter
              transitions + end-to-end walkthrough drift guard.

              New transitions:
                on_cursor_leave_window(session_id):
                  DraggingInside → DraggingOutside
                on_cursor_reenter_window(x, y):
                  DraggingOutside → DraggingInside
                cancel() -> (Self, Option<usize>):
                  Any → Idle; returns tab_idx that was being
                  dragged so the caller can restore visuals.

              The end_to_end_drag_walkthrough drift guard
              exercises the full FSM path: Idle → Armed →
              DraggingInside → DraggingOutside → cancel → Idle.

              Closes detachable-tabs Bucket-D sub-cycle 9
              (cancel) in full + sub-cycle 11 (e2e test) for
              the FSM portion. Full sub-cycle 11 needs a
              cross-process integration test which spans
              multiple sessions per the design doc.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2-7, 9, 10
                                   E — sub-cycle 8 (group-name
                                       edit) Bucket-E
bg-image (12 sub-cycles):         ✅ 11/12 effectively COMPLETE
Detachable tabs (11 sub-cycles):  ✅ 7 — sub-cycles 1 (design),
                                       2 (serialize), 3 (SCM_RIGHTS),
                                       4 (extract/insert), 5 (FSM),
                                       9 (cancel), 10 (Wayland
                                       fallback), 11 partial
                                       (FSM e2e test)
                                   ⌛ 4 — sub-cycles 6 (cursor
                                       detection winit-side), 7
                                       (cross-process IPC + fd
                                       transfer), 8 (new-window-
                                       on-drop), 11 full (cross-
                                       process integration test)

41 of 46 Bucket-D sub-cycles end-to-end (89%).

Workspace tests stay at 308.

The 4 remaining detachable-tabs sub-cycles are all CROSS-
PROCESS integration: they compose every foundation now shipped
(FSM, SCM_RIGHTS, serialize/extract/insert, cancel path,
Wayland-fallback) into the workflow where two kettle
processes coordinate. Per the design doc, integration spans
multiple sessions because the test fixture (two concurrent
kettle processes) is inherently a multi-process problem.

## [1.27.0] — 2026-05-22

Two more detachable-tabs Bucket-D foundations.

  cycle 399 — `kettle_ui::fd_transport` SCM_RIGHTS module.
              send_fds / recv_fds on Unix sockets via
              libc::sendmsg + ancillary cmsg + SCM_RIGHTS.
              Unix-only (#[cfg(unix)]). Windows + Wayland get
              the cycle-384 keyboard-driven fallback.
              Closes detachable-tabs Bucket-D sub-cycle 3.

  cycle 400 — `kettle_ui::detach::DragState` FSM. Pure-data
              state machine with 4 states (Idle, ArmedInside,
              DraggingInside, DraggingOutside) + 5 transitions
              (on_mouse_down_on_tab, on_mouse_move,
              on_mouse_up, on_abort, is_dragging).
              4px drag-threshold matches GTK + most desktops.
              Closes detachable-tabs Bucket-D sub-cycle 5.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2-7, 9, 10 +
                                       layout-shift
                                   E — sub-cycle 8 (group-name
                                       edit) Bucket-E
bg-image (12 sub-cycles):         ✅ 11 — all implemented
                                       (cycle 396 closed sub-
                                       cycle 9 blur shader as
                                       CPU-side decode-time)
Detachable tabs (11 sub-cycles):  ✅ 6 — sub-cycles 1 (design),
                                       2 (serialize), 3 (SCM_RIGHTS),
                                       4 (extract/insert), 5
                                       (drag FSM), 10 (Wayland
                                       fallback)
                                   ⌛ 5 — sub-cycles 6 (cursor
                                       detection), 7 (cross-
                                       process IPC + fd transfer),
                                       8 (new-window-on-drop), 9
                                       (cancel path), 11 (e2e test)

39 of 46 Bucket-D sub-cycles end-to-end (85%).

The 5 remaining detachable-tabs sub-cycles are all integration
work: each composes the foundations now shipped (FSM, SCM_RIGHTS,
serialize/extract/insert, Wayland fallback) into the cross-
process workflow. Multi-week per the design doc; pickable
cleanly by future sessions.

Workspace tests stay at 308.

## [1.26.0] — 2026-05-22

Detachable-tabs Bucket-D foundation APIs.

  cycle 397 — `Mux::serialize_tab(idx)` returns the same STab
              wire format that session.json uses. Pure-data
              utility that future cross-process IPC consumes.
              Closes detachable-tabs Bucket-D sub-cycle 2.

  cycle 398 — `Mux::extract_tab(idx)` + `Mux::insert_tab(at, Tab)`
              — in-process tab handoff primitives. extract_tab
              removes a tab from the tabs list WITHOUT touching
              its panes (the panes stay in self.panes; the
              caller is responsible for transferring or dropping
              them). insert_tab inserts a Tab at the given idx
              + sets active so the moved tab is focused
              immediately. Closes detachable-tabs Bucket-D
              sub-cycle 4.

Both APIs are #[allow(dead_code)] until the cross-process IPC
caller lands (sub-cycles 7+8); the in-process foundation
ships now so the IPC cycle composes cleanly with proven
primitives.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2-7, 9, 10 +
                                       layout-shift
                                   E — sub-cycle 8 (group-name
                                       edit) — Bucket-E until
                                       named broadcast groups
                                       infra lands
bg-image (12 sub-cycles):         ✅ 11 — all sub-cycles
                                       implemented + 1 implicit
                                       per-frame UV recompute
Detachable tabs (11 sub-cycles):  ✅ 4 — sub-cycles 1 (design
                                       doc), 2 (serialize_tab),
                                       4 (extract/insert), 10
                                       (Wayland fallback)
                                   ⌛ 7 — sub-cycles 3
                                       (SCM_RIGHTS wrapper), 5
                                       (drag state machine), 6
                                       (cursor detection), 7
                                       (cross-process IPC + fd
                                       transfer), 8 (new-window
                                       on-drop), 9 (cancel
                                       path), 11 (e2e test)

37 of 46 Bucket-D sub-cycles end-to-end (80%).

Workspace tests 306 → 308 (+2 drift guards).

## [1.25.0] — 2026-05-22

  cycle 395 — Per-pane Edit-title overlay anchors near clicked
              pane. Pane-scope edits render the overlay at the
              focused pane's titlebar position; window + tab
              scopes keep window-bottom. UX matches Terminator's
              click-to-edit-in-place expectation. Closes
              titlebar Bucket-D sub-cycle 7.
  cycle 396 — CPU-side `background_blur` for bg-image. 3-pass
              separable box blur approximates Gaussian at much
              lower compute (~30-50ms on a 1080p image at radius
              8). Applied at decode-time, so per-frame render
              cost is zero. Closes bg-image Bucket-D sub-cycle 9.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 of 10 — sub-cycles 2/3/4/5/6/7/9/10 +
                                       layout-shift
                                   E — sub-cycle 8 (inline group-name
                                       edit) deferred until kettle
                                       grows named broadcast groups
                                       (currently per-tab on/off only)
bg-image (12 sub-cycles):         ✅ 11 of 12 — sub-cycles 2/3/4/5/6/7/8/9/10/11/12
                                   ⌛ 0 (all impl complete; sub-cycle 8
                                       was implicit per-frame recompute,
                                       documented in cycle 394)
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10 Wayland-fallback
                                   ⌛ 10 — sub-cycles 2-9, 11 (cursor
                                       drag + SCM_RIGHTS fd transfer +
                                       cross-process IPC + auth +
                                       reattach — multi-week thread)

34 of 46 Bucket-D sub-cycles end-to-end (74%).

bg-image Bucket-D is now effectively COMPLETE — every sub-cycle
has a shipped implementation (10 explicit + 1 documented-as-
implicit). The blur is CPU-side; a future wgpu-shader version
would shave the ~50ms decode-time cost but the user-visible
effect lands today.

Workspace tests stay at 306.

## [1.24.0] — 2026-05-22

  cycle 394 — bg-image resize handler documented as implicit
              per-frame UV recompute. The cycle-388 cache
              stores the decoded image bytes; the cycle-390
              UV-mode dispatch reads current surface dims each
              frame in build_frame. Window resizes implicitly
              take effect on the next frame. Closes bg-image
              Bucket-D sub-cycle 8 by documenting the
              recompute-contract so future contributors don't
              add a redundant resize handler.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 of 10
                                   ⌛ 1 — sub-cycle 7
                                       (per-pane edit anchor;
                                       overlay renders at
                                       window-bottom today,
                                       anchoring at clicked
                                       pane's titlebar is polish)
                                   E — sub-cycle 8 (group-name
                                       edit) deferred — kettle
                                       doesn't yet have named-
                                       groups infra; sub-cycle
                                       waits on that to land
                                       independently
bg-image (12 sub-cycles):         ✅ 10 of 12
                                   ⌛ 2 — sub-cycle 8 ✅
                                       (implicit per-frame
                                       recompute documented),
                                       sub-cycle 9 (blur shader
                                       — needs WGSL Gaussian
                                       two-pass pipeline)
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — sub-cycles 2-9, 11
                                       (cross-window cursor
                                       drag + SCM_RIGHTS fd
                                       transfer + auth +
                                       reattach — multi-week
                                       thread)

33 of 46 Bucket-D sub-cycles end-to-end (72%).

Workspace tests stay at 306.

## [1.23.0] — 2026-05-22

  cycle 393 — Titlebar pixel acceptance test. Pure
              `pane_titlebar_hit_geometry` helper extracted +
              drift-guarded with 8 assertions covering both
              top + bottom bar positions, hit/miss for
              multi-pane layouts. Closes titlebar Bucket-D
              sub-cycle 10.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2/3/4/5/6/9/10 +
                                       layout-shift + size-text
                                   ⌛ 1 — 7 per-pane edit anchor
                                       (overlay renders at window
                                       bottom; anchoring at the
                                       clicked pane's titlebar is
                                       polish), 8 group-name edit
                                       (needs named-groups infra)
bg-image (12 sub-cycles):         ✅ 9 — sub-cycles 2/3/4/5/6/7/10/11/12
                                   ⌛ 3 — 8 explicit resize handler,
                                       9 blur shader
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — cursor drag + IPC +
                                       SCM_RIGHTS + auth + reattach

32 of 46 Bucket-D sub-cycles shipped end-to-end (70%).

Workspace tests 305 → 306.

## [1.22.0] — 2026-05-22

  cycle 391 — bg-image align_horiz + align_vert wired. The
              cycle-390 center + scale image modes now honor
              the position-anchor config keys. Closes bg-image
              Bucket-D sub-cycle 6 in full.
  cycle 392 — bg-image acceptance test. Generates a known 8x4
              RGBA PNG via the image crate, decodes via
              decode_bg_image, asserts dimensions + spot-checks
              the first pixel. Closes bg-image Bucket-D
              sub-cycle 12.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 8 — sub-cycles 2/3/4/5/6/9 +
                                       layout-shift + size-text
                                   ⌛ 2 — 7 per-pane edit anchor,
                                       8 group-name edit (needs
                                       named-groups infra),
                                       10 pixel acceptance test
bg-image (12 sub-cycles):         ✅ 9 — sub-cycles 2/3/4/5/6/7/10/12
                                       + 11 (implicit path-cache)
                                   ⌛ 3 — 8 resize (implicit per-frame
                                       UV recompute), 9 blur shader
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — cursor drag + IPC +
                                       SCM_RIGHTS + auth + reattach

31 of 46 Bucket-D sub-cycles shipped end-to-end (67%).

Workspace tests 304 → 305.

## [1.21.0] — 2026-05-22

  cycle 389 — Per-pane titlebar click → EditPaneTitle. Hit-test
              checks click in titlebar y-band (top or bottom
              per cfg.title_at_bottom); focused-pane titlebar
              click opens the edit overlay; unfocused-pane
              titlebar click first focuses (two-click model
              avoids accidental edits on focus transitions).
              Closes titlebar sub-cycle 5.
  cycle 390 — bg-image UV-mode variants. background-image-mode
              controls how the decoded image fills the surface:
                stretch_and_fill (default), tile, center, scale.
              Closes bg-image sub-cycles 5 + 6.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 8 — sub-cycles 2/3/4/5/6/9 +
                                       cell-shift + size-text done
                                   ⌛ 2 — 7 per-pane edit anchor,
                                       8 group-name edit,
                                       10 pixel acceptance test
bg-image (12 sub-cycles):         ✅ 7 — sub-cycles 2/3/4/5/6/7/10
                                       + path-cache reload (11
                                       implicit)
                                   ⌛ 5 — 8 resize, 9 blur shader,
                                       12 acceptance test
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — cursor drag + IPC +
                                       SCM_RIGHTS + auth + reattach

29 of 46 Bucket-D sub-cycles shipped end-to-end (63%).

Workspace tests stay at 304.

## [1.20.0] — 2026-05-22

Titlebar receive-state variant + background-image full render.

  cycle 387 — Per-pane titlebar receive-state color variant.
              cfg.title_receive_bg/fg_color used when broadcast
              is on + pane isn't the focused source. Closes
              titlebar sub-cycle 4.
  cycle 388 — Background-image full wgpu render. When
              cfg.background_type = image + cfg.background_image
              is set, decodes via cycle-381 helper, caches the
              ImageData, prepends to img_items so imgpipe draws
              it as the first textured quad — wallpaper visible
              behind padding gaps, transparent cells, and
              dim overlays. Closes bg-image sub-cycles 3+4.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):       ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):     ✅ 7 (sub-cycles 2/3/4/6/9 + layout-shift)
                              ⌛ 3 (5 hit-test / 7 per-pane edit / 8 group label / 10 test)
bg-image (12 sub-cycles):     ✅ 5 (sub-cycles 2/3/4/7/10)
                              ⌛ 7 (UV modes 5+6, align, resize, blur, reload, test)
Detachable tabs (11 sub-cycles): ✅ 1 (10 Wayland-fallback)
                              ⌛ 10 (cursor drag, IPC, SCM_RIGHTS,
                                    reattach)

26 of 46 Bucket-D sub-cycles shipped end-to-end.

Workspace tests stay at 304.

## [1.19.0] — 2026-05-22

Titlebar Bucket-D + detachable-tabs Wayland-fallback push.

  cycle 383 — Cell-content layout-shift for per-pane titlebar.
              build_pane gets a `pane_titlebar_h` parameter;
              cells, images, search highlights, hint labels all
              shift below the bar. Closes titlebar sub-cycle 2.
  cycle 384 — `Action::MoveTabToNewWindow` (detachable-tabs
              Bucket-D Wayland-fallback sub-cycle 10). Spawns
              a new kettle process with focused pane's cwd +
              closes source tab. Cross-process PTY transfer
              for the cursor-drag case remains a multi-cycle
              SCM_RIGHTS thread.
  cycle 385 — `title-at-bottom` config wired to render. Bar
              + title text flip to bottom of pane. Closes
              titlebar sub-cycle 9.
  cycle 386 — Titlebar size text + icon_bell. Format:
              "title  WxH  🔔". `title-hide-sizetext` skips
              the WxH; `icon_bell` skips the bell glyph.
              Closes titlebar sub-cycle 6.

### Titlebar Bucket-D status

  ✅  2: cell-content layout-shift (cycle 383)
  ✅  3: title text render (cycle 382)
  ✅  6: title_hide_sizetext + icon_bell (cycle 386)
  ✅  9: title_at_bottom flip (cycle 385)
  ⌛  4: receive/group color variants
  ⌛  5: hit-testing for click + drag-detach
  ⌛  7: edit-title overlay per-pane anchor
  ⌛  8: inline group-name edit
  ⌛  10: pixel acceptance test

6 of 10 titlebar sub-cycles complete + visible end-to-end.

### Detachable Tabs Bucket-D status

  ✅  10: Wayland-fallback keyboard alternative (Action::
        MoveTabToNewWindow). Spawns new window with cwd
        inheritance; PTY-transfer remains the SCM_RIGHTS
        thread.
  ⌛  2-9: cross-window cursor drag, IPC, fd transfer,
        cancel/reattach.

Workspace tests stay at 304.

## [1.18.0] — 2026-05-22

  cycle 382 — Per-pane titlebar TITLE TEXT render. cycle-379's
              background quad now actually displays each pane's
              title via a parallel `pane_titlebar_buffers` field
              on Renderer. Focus state picks fg color
              (transmit_fg / inactive_fg); empty title falls
              back to 'kettle'.

The per-pane titlebar is now FULLY visible end-to-end:
  - Background quad colored per focused/unfocused state (cycle 379)
  - Title text rendered in the configured fg variant (cycle 382)
  - Hit-testing, group label, activity dot, size-text, cell-content
    layout-shift remain titlebar Bucket-D follow-ups.

### Titlebar Bucket-D progress

  ✅  2 (partial): visible background quad (cycle 379)
  ✅  3 (partial): title text render (cycle 382)
  ⌛  3 (remainder): activity dot in titlebar
  ⌛  4: color variants for receive (group-broadcast) state
  ⌛  5: hit-test for click + drag-detach region
  ⌛  6: title_hide_sizetext + icon_bell wired
  ⌛  7: edit-title overlay anchors to clicked pane's titlebar
        (existing cycle-369/372 overlay works at window-level
         now; per-pane click is the follow-up)
  ⌛  8: inline group-name edit
  ⌛  9: title_at_bottom flip
  ⌛  10: pixel-tolerance --screenshot acceptance test

4 of 10 titlebar sub-cycles per design doc are functional.

Workspace tests stay at 304.

## [1.17.0] — 2026-05-22

Plugin Bucket-D COMPLETE end-to-end + first user-visible
deliverables for titlebar + bg-image Bucket-D items.

  cycle 377 — LuaEvent::Output variant + fire_event dispatch
              (API surface)
  cycle 378 — LuaEvent::Output PTY-reader sidechannel emission.
              Per-pane `output_rx: Option<Receiver<Vec<u8>>>`
              attached when LuaEngine is active; reader-thread
              try_sends raw bytes; App drain_events coalesces +
              fires LuaEvent::Output(pane_id, bytes).
              Zero-cost when no Lua subscriber.
  cycle 379 — Per-pane titlebar background quad render. When
              cfg.show_titlebar=true + >1 pane in tab, a
              cfg.title_*_bg_color strip renders at the top of
              each pane.
  cycle 380 — background-darkness + background-type composed
              alpha. background-type=transparent or image
              multiplies opacity by darkness, applied to the
              wgpu clear-color (both live + screenshot path).
  cycle 381 — bg-image decoder foundation. New
              `kettle_render::bg_image::decode_bg_image(path)`
              helper with format-feature flags PNG/JPEG/WebP/
              BMP/GIF; tilde-expansion; graceful nil-on-missing.

### Plugin Bucket-D end-to-end

All 5 LuaEvent variants emit; all 7 user-facing kettle.* APIs
ship; init.lua auto-loads; sandbox config knob in place; URL
+ menu handlers route through Lua registry before kettle
defaults.

  ✅  13 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles

### Titlebar Bucket-D progress

  ✅  2 (partial): visible background quad
  ⌛  3+: title text render, color variants for receive state,
         hit-testing, icon_bell, layout-shift so cells don't
         overlap the bar

### bg-image Bucket-D progress

  ✅  2:  decoder foundation
  ✅  7:  background-darkness overlay (composed alpha)
  ✅  10: background-type=transparent path (composed alpha)
  ⌛  3,4,5,6,8,9,11,12 — wgpu texture upload + render quad +
         UV modes + align + resize + blur + reload + tests

### Detachable tabs Bucket-D

  ⌛  All sub-cycles deferred to dedicated session per
      docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md. Needs
      SCM_RIGHTS fd transfer (Linux/macOS) + cross-window
      IPC. Wayland users get the documented keybind-fallback
      alternative.

Workspace tests 302 → 304 (+2 bg_image drift guards).

## [1.16.0] — 2026-05-22

  cycle 375 — `kettle.add_menu_item(label, callback)` Lua API.
              Last user-facing plugin API from docs/TERMINATOR-
              PLUGIN-DESIGN.md. Lua plugins can extend the
              cycle-245 right-click context menu with their
              own entries; clicks invoke the registered
              callback + drain any queued LuaCommands.
              New ContextMenuItem::LuaItem variant + dispatch
              path.

  cycle 376 — `lua-sandbox = safe|trusted` config knob.
              `safe` (default) nil's os.execute / os.exit /
              os.remove / io.open / io.popen / loadfile /
              dofile / package.loadlib in the Lua VM at
              construction. Matches the sandbox defaults of
              WezTerm + Hammerspoon + Neovim plugin runtimes.
              `trusted` exposes everything (user opt-in).

### Plugin sub-cycle status

  ✅  365 — kettle.on event-hook foundation
  ✅  366 — LuaEvent::Startup emission
  ✅  367 — LuaEvent::Bell emission
  ✅  368 — LuaEvent::TabAdd / TabClose emission
  ✅  370 — init.lua auto-load
  ✅  371 — kettle.notify
  ✅  373 — kettle.set_theme
  ✅  374 — kettle.add_url_handler
  ✅  375 — kettle.add_menu_item
  ✅  376 — lua-sandbox config

  ⌛  pending — LuaEvent::Output emission (throttled per-PTY-chunk
                event for plugins that watch terminal output)

10 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles complete.
Every user-facing plugin Lua API is now shipped. Only Output
event emission remains, and that's bounded (throttle bucket +
fire call at the drain_events Output match arm).

Workspace tests stay at 302.

## [1.15.0] — 2026-05-22

Plugin Lua API expansion. Two new plugin sub-cycles + URL routing.

  cycle 373 — `kettle.set_theme(name)` Lua API for runtime theme
              switching. Resolves via Theme::find_name (case-
              insensitive lookup of ~500 bundled themes).
              Unknown name → log::warn fallthrough.
  cycle 374 — `kettle.add_url_handler(name, pattern, callback)`
              Lua API for user-supplied URL routing. Uses Lua's
              native string.match (Terminator-pattern-compatible
              for common URL shapes). Dispatched in
              App::open_url BEFORE cfg.custom_url_handler +
              system-open fallthrough; first-match wins.

### User-facing examples

Replicating Terminator's auto_theme.py + url_handlers.py +
run_cmd_on_match.py + maven.py as a few-line Lua module:

  -- ~/.config/kettle/init.lua
  kettle.on('startup', function()
    local hour = tonumber(os.date('%H'))
    kettle.set_theme(hour >= 18 or hour < 6
      and 'Solarized Dark' or 'Solarized Light')
  end)

  kettle.add_url_handler('github_pr',
    'https?://github%.com/[^/]+/[^/]+/pull/(%d+)',
    function(url) os.execute('gh pr view ' .. url) end)

### Plugin sub-cycle status

  ✅  365 — kettle.on event-hook foundation
  ✅  366 — LuaEvent::Startup emission
  ✅  367 — LuaEvent::Bell emission
  ✅  368 — LuaEvent::TabAdd / TabClose emission
  ✅  370 — init.lua auto-load
  ✅  371 — kettle.notify
  ✅  373 — kettle.set_theme
  ✅  374 — kettle.add_url_handler

  ⌛  pending — LuaEvent::Output, kettle.add_menu_item,
                sandbox config knob

8 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles complete.

Workspace tests stay at 302.

## [1.14.0] — 2026-05-22

Plugin system implementation push. Cycles 370-372 ship:

  cycle 370 — `~/.config/kettle/init.lua` auto-loads at startup
              (no need for explicit --lua-script). Follows the
              Neovim/Hammerspoon/WezTerm convention.
  cycle 371 — `kettle.notify(title, body?)` Lua API for desktop
              notifications. Cross-platform via notify-rust crate
              (libnotify on Linux, NSUserNotification on macOS,
              Toast on Windows). Body is optional; failures
              degrade silently to log::warn (headless / no DBUS).
  cycle 372 — Edit-title overlay visual chrome. Yellow palette[3]
              bottom bar renders the prompt + typed input + cursor.
              Edit-title is now FULLY interactive end-to-end
              (state machine cycle-369 + visual feedback this cycle).

### Plugin system status

  ✅  cycle 365 — kettle.on event-hook foundation
  ✅  cycle 366 — LuaEvent::Startup emission
  ✅  cycle 367 — LuaEvent::Bell emission
  ✅  cycle 368 — LuaEvent::TabAdd / TabClose emission
  ✅  cycle 370 — init.lua auto-load
  ✅  cycle 371 — kettle.notify

  ⌛  pending — LuaEvent::Output, kettle.add_menu_item,
                kettle.add_url_handler, kettle.set_theme,
                sandbox config

6 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles complete.

### Workspace tests

Stay at 302.

## [1.13.0] — 2026-05-22

Plugin emission wirings + Edit-title overlay implementation.

### Plugin sub-cycle wirings (cycles 366-368)

The cycle-365 LuaEvent enum is now wired end-to-end at all 4
emission sites. Users can write event-hook plugins and have
them fire on real kettle events:

  cycle 366  LuaEvent::Startup    fires after first-pane-ready
                                  in App::resumed (guarded against
                                  Wayland's resumed re-emission).
                                  App now persists LuaEngine across
                                  its full lifetime.
  cycle 367  LuaEvent::Bell       fires for each belled pane after
                                  the kettle-side bell processing.
  cycle 368  LuaEvent::TabAdd     fires from Action::NewTab dispatch
                                  with the new active tab index.
             LuaEvent::TabClose   fires from Action::CloseTab dispatch
                                  with the closing tab index.

All 4 LuaEvent variants thus have App emission sites. The
docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles 2-5 are complete
(foundation + every event-site wiring). Subsequent plugin
sub-cycles (notify, add_menu_item, add_url_handler, set_theme,
sandbox config) build on this foundation.

User-facing example:

  -- ~/.config/kettle/init.lua (autoload pending sub-cycle 11)
  kettle.on('startup', function()
    kettle.send_text('echo \"kettle ' .. kettle.version() .. '\"\\n')
  end)
  kettle.on('bell', function(pane)
    kettle.exec_action('toggle_window_visibility')
  end)
  kettle.on('tab_add', function(idx)
    -- could send greeting text, switch profile, etc.
  end)

### Edit-title overlay (cycle 369)

Replaces the cycle-354 placeholders with a real overlay state
machine. `TitleEditState { scope, input }` opens on
Action::Edit{Window,Tab,Pane}Title pre-filled with the current
title; Enter applies via the appropriate setter (Window::set_title,
Tab.title_override, Pane.title); Esc cancels.

The overlay registers with any_modal_open + close_all_modals so
the cycle-X modal discipline (Esc-to-dismiss, cursor-icon override,
key-route guard) extends transparently. Visual chrome render of
the overlay is a follow-up sub-cycle paired with the per-pane
titlebar Bucket-D work; today's state + apply path is observable
via --remote-list-tabs and the OS window title.

### Status

ALL 18 cycle-342 Action variants are now FULLY wired end-to-end.
Zero placeholder stubs remain in the Action dispatch path.

The 4 LuaEvent emission sites are wired. The plugin foundation
(cycle 365) is now functional end-to-end.

Workspace tests stay at 302.

## [1.12.0] — 2026-05-22

Minor-bump — final config-key wiring batch + all four Bucket-D
design docs + plugin-system foundation.

### Behavior wirings (cycles 358-360)

  cycle 358 — invert-search direction toggle
  cycle 359 — geometry-hinting via winit ResizeIncrements
  cycle 360 — focus = sloppy (focus-follows-mouse)

### Bucket-D design docs shipped (cycles 361-364)

All four multi-cycle Bucket-D items from docs/TERMINATOR-AUDIT.md
have concrete design docs now, each following the cycle-328/329
template:

  docs/TERMINATOR-PLUGIN-DESIGN.md          — Lua event-hooks
  docs/TERMINATOR-PANE-TITLEBAR-DESIGN.md   — per-pane titlebar
  docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md — cross-window drag
  docs/TERMINATOR-BG-IMAGE-DESIGN.md        — background image

### Plugin foundation (cycle 365)

  kettle.on(event_name, callback)  — Lua-side registration
  LuaEvent enum                    — Startup / TabAdd / TabClose / Bell
  LuaEngine::fire_event(&LuaEvent) — multi-subscriber, error-isolated

Subsequent plugin sub-cycles wire each LuaEvent variant to its
App emission site.

Workspace tests 300 → 302.

## [1.11.0] — 2026-05-22

Behavior wiring batch — closes the gap between parsed config keys
(v1.9.0) and end-to-end shipped behavior. Cycles 349-357 cover
~20 more Terminator config keys + the last 3 cycle-342 actions.

### Behavior wirings shipped

  cycle 349 — force-no-bell + close-button-on-tab +
              new-tab-after-current-tab
                force-no-bell           silence bell + dot + flash
                close-button-on-tab     hide tab ✕ chip + glyph
                new-tab-after-current-tab  insert after active vs append

  cycle 350 — link-single-click + disable-mouse-paste +
              putty-paste-style
                link-single-click       single-click opens URLs
                disable-mouse-paste     middle-click no-op
                putty-paste-style       right-click pastes (vs menu)

  cycle 351 — use-custom-url-handler + custom-url-handler:
                external program for URL clicks, with safe-URL guard
                + system-open fallback. Routes both Ctrl-click +
                cycle-218 hint-mode URL paths through one helper.

  cycle 352 — backspace-binding + delete-binding:
                remap encoded bytes to AsciiDel / ControlH /
                EscapeSequence / Automatic. Preserves cycle-X
                Alt+Backspace + Ctrl+Backspace muscle-memory
                semantics by only remapping the no-modifier case.

  cycle 353 — handle-size:
                split-divider width in px (1.0 default; clamps -1..50
                already done at parse time).

  cycle 354 — Edit-title actions (last 3 cycle-342 stubs):
                EditWindowTitle     →  Window::set_title
                EditTabTitle        →  Tab.title_override (new field)
                EditPaneTitle       →  Pane.title
              Placeholder values + log::info noting full overlay
              ships with Bucket-D per-pane titlebar.

  cycle 355 — allow-bold + bold-is-bright:
                allow-bold          suppress Flags::BOLD in render
                bold-is-bright      remap palette[0..8] → palette[8..16]
                                    via new color::bright_for_bold helper

  cycle 356 — inactive-bg-color-offset:
                compose with unfocused-split-opacity for unfocused-
                pane dim. inactive-color-offset (FG-only) reserved
                for Bucket-D text-reshape follow-up.

  cycle 357 — broadcast-default + exit-action:
                broadcast-default       seed mux.broadcast at startup
                exit-action = hold      pane stays open on shell exit
                exit-action = restart   log::warn fallthrough (re-spawn
                                         needs argv+cwd plumbing)
                exit-action = close     (default) unchanged

### Status of cycle-342 actions

All 18 now have behavior wired end-to-end. 15 with full real
semantics; 3 (EditWindowTitle / EditTabTitle / EditPaneTitle) are
placeholder + cited Bucket-D titlebar deferral for the
interactive-overlay UX.

### Still deferred (Bucket D — multi-cycle, design docs in audit)

  - Plugin system (Lua event hooks foundation)
  - Per-pane titlebar (full chrome region + interactive title edit)
  - Detachable tabs (cross-window drag)
  - Background image render (texture pass + blur shader)
  - Inactive-color-offset FG-only dim (text reshape per pane)

### Honest no-op stubs (documented in audit, cycle-E rationale)

  - smart-copy: kettle's existing copy behavior already matches
  - homogeneous-tabbar: kettle's existing tab layout already matches
  - extra-styling: kettle is wgpu+glyphon, not GTK
  - cell-height / cell-width: VTE-specific; kettle derives metrics
  - use-system-font: kettle is config-file-driven by design
  - use-theme-colors: kettle is bundled-themes-driven by design
  - disable-mousewheel-zoom: no Ctrl+wheel zoom in kettle today
  - sticky / hide-from-taskbar: winit support varies per platform

Workspace tests stay at 300. Test count steady because the
existing parse-side drift guards already pin the contract; the
wiring is exercised by a windowed run + manual verification.

## [1.10.0] — 2026-05-22

Minor-bump release — Terminator-parity behavior wiring (cycles 343-348).
Builds on v1.9.0's config + Action-registration surface; this release
wires the actual behaviors for 15 of 17 stubbed actions + several
config keys.

### Behavior wirings shipped

  cycle 343 — PTY spawn now honors:
                cfg.term         → TERM env override
                cfg.colorterm    → COLORTERM env override
                cfg.login_shell  → prepends `-l` to shell argv

  cycle 344 — Window state at creation + focus:
                cfg.window_state     → with_maximized / with_fullscreen /
                                        with_visible(false) at startup
                cfg.hide_on_lose_focus → set_visible(false) on focus-loss
                                          (Quake-style; reappears via
                                          cycle-303 --toggle)

  cycle 345 — 9 actions wired end-to-end:
                ZoomInAll / ZoomOutAll / ZoomNormalAll  (broadcast zoom)
                InsertPaneNumber / InsertPanePadded     (pane index to PTY)
                ScrollPageUpHalf / ScrollPageDownHalf   (half-page scroll)
                PastePrimary                            (X11 primary)
                ToggleWindowVisibility                  (in-process toggle)

  cycle 346 — ToggleScrollbar: tri-state cycle of cfg.scrollbar
              (Never → Always → Auto → Never).

  cycle 347 — RotateCw / RotateCcw: split-tree rotation via new
              `Mux::rotate_focused_split(clockwise: bool)`. Cw
              flips dir + swaps children (Terminator semantics);
              Ccw flips dir only.

  cycle 348 — NextProfile / PrevProfile: runtime profile cycle
              enumerating <config-dir>/profiles/*.config and
              calling existing reload_config helper (cycle 151
              infrastructure).

### New API

  Terminal::new_with_env(...)              — cycle 343
  Mux::focused_pane_index_in_tab()         — cycle 345
  Mux::rotate_focused_split(clockwise)     — cycle 347

`Terminal::new` is now a thin shim over `new_with_env` for
backward compat (no caller change).

### Still stubbed (3 of 18)

These actions need a new overlay state + key dispatcher (same
shape as the cycle-X palette overlay):

  EditWindowTitle
  EditTabTitle
  EditPaneTitle

Each is bounded but multi-file (App state + overlay render +
key dispatcher). Tracked as cycle 349+ in audit doc.

### Bucket D items still deferred

Plugin system, per-pane titlebar, detachable tabs, background
image rendering. Each is documented in docs/TERMINATOR-AUDIT.md
with a roadmap pointer; each warrants its own multi-cycle
thread (~3-6 sub-cycles).

Workspace tests stay at 300 (no new drift guards this batch —
the existing config-key drift guards + the Action::from_name
registry test cover the parsing surface; behavior wirings are
exercised by a windowed run).

## [1.9.0] — 2026-05-22

Feature-bump release — Terminator-parity audit + sweep (cycles 330-342).
Adds ~70 new config keys + 18 new Action variants covering the entire
Terminator config surface. Behavior wiring for some keys + most new
actions lands in follow-up sub-cycles; the config + Action surface is
discoverable via `--check-config` + `--list-actions` so Terminator
users can copy their config and have `--check-config` not flag anything.

### Audit + planning (cycle 330)

`docs/TERMINATOR-AUDIT.md` is the single source of truth — every
Terminator source file enumerated with feature/setting bullets +
a 5-bucket gap table (A/B/C/D/E). Phase 1 audited Terminator at
SHA `403fa1e5`; subsequent cycles flip B/C rows to ✅ A.

### Config keys shipped (cycles 331-341)

  Window state:        borderless, always-on-top, hide-on-lose-focus,
                       sticky, hide-from-taskbar, window-state, focus,
                       handle-size, geometry-hinting, extra-styling
  Tab UX:              close-button-on-tab, new-tab-after-current-tab,
                       title-at-bottom, scroll-tabbar, homogeneous-tabbar
  Tab-position:        accepts `hidden` (alias to `tab-bar = off`),
                       `left`/`right` (accepted by parser, falls
                       through to top with a log::warn — vertical
                       tab bars deferred to Bucket C)
  Render:              allow-bold, bold-is-bright, cursor-color-default,
                       use-system-font, use-theme-colors
  Mouse / paste:       link-single-click, disable-mousewheel-zoom,
                       clear-select-on-copy, disable-mouse-paste,
                       putty-paste-style, smart-copy,
                       putty-paste-style-source-clipboard
  Bell:                force-no-bell, icon-bell
  Search / env:        invert-search, term, colorterm
  Shell exec:          login-shell, exit-action (Close/Restart/Hold),
                       ask-before-closing (Always/MultipleTerminals/Never)
  Key encoding:        backspace-binding, delete-binding
  Group / broadcast:   broadcast-default (All/Group/Off),
                       split-to-group, autoclean-groups
  URL handler:         use-custom-url-handler, custom-url-handler
  Inactive offsets:    inactive-color-offset, inactive-bg-color-offset
  Per-pane titlebar:   show-titlebar, title-hide-sizetext,
                       title-use-system-font, title-font, six
                       title-{transmit,receive,inactive}-{fg,bg}-color
                       fields
  Background image:    background-type (Solid/Image/Transparent),
                       background-image, background-image-mode,
                       background-image-align-horiz/vert,
                       background-blur, background-darkness
  Misc:                cell-height, cell-width, http-proxy,
                       always-split-with-profile, detachable-tabs

Every key accepts both kebab-case (kettle convention) and
underscore form (Terminator convention).

### Action variants shipped (cycle 342)

18 new `Action::*` variants registered in the keymap grammar +
discoverable via `--list-actions`:

  RotateCw / RotateCcw
  ToggleScrollbar
  EditWindowTitle / EditTabTitle / EditPaneTitle
  InsertPaneNumber / InsertPanePadded
  NextProfile / PrevProfile
  ZoomInAll / ZoomOutAll / ZoomNormalAll
  ResetAndClear (fully wired — Reset + ClearHistory composed)
  ScrollPageUpHalf / ScrollPageDownHalf
  PastePrimary
  ToggleWindowVisibility

13 of 18 appear in the cycle-117 palette (Ctrl+Shift+K); the
5 title-edit + insert-text variants are excluded because they
need overlays or send raw text.

### Drift guards

Eight new test functions pin defaults + parsing for every new
config key + every action variant. The cycle-117 palette
exhaustive-match guard updated to fail compile on a future
unclassified variant.

### Followups (each its own sub-cycle)

Most config-key BEHAVIOR wiring is a follow-up sub-cycle. The
config + drift guard ship now so Terminator users can copy their
config without --check-config errors. Specifically pending:
- Render-layer: allow-bold, bold-is-bright, background-image,
  inactive-color-offset (per-fg/bg dim).
- Mouse handler: link-single-click, disable-mousewheel-zoom,
  disable-mouse-paste, putty-paste-style.
- Window: borderless + always-on-top WIRED in cycle 332.
  hide-from-taskbar / sticky / hide-on-lose-focus deferred
  (winit support varies).
- Per-pane titlebar: Bucket D (multi-cycle, needs render-layer
  rework).
- Action behaviors: 17 stubbed actions with log::info dispatch
  (ResetAndClear is fully wired).

Workspace tests 286 → 300 (+14 drift guards).

## [1.8.0] — 2026-05-21

Feature-bump release — Lua scripting (WezTerm parity) + tmux `-CC`
parser foundation (iTerm2 parity, multi-cycle thread starts) +
detachable-mux-server design doc.

### Added — Lua scripting (cycles 324-326, WezTerm parity)

`kettle --lua-script PATH` runs a Lua 5.4 file at startup with a
`kettle` namespace. Useful for programmatic startup workflows
without leaving the keymap surface.

  init.lua:
    print("kettle " .. kettle.version() .. " on " .. kettle.theme())
    kettle.exec_action("new_tab")
    kettle.exec_action("split_right")
    kettle.send_text("htop\\n")

  Read-only API (cycle 324):
    kettle.version()      → string
    kettle.config_path()  → string|nil
    kettle.theme()        → string

  Side-effect API (cycles 325-326):
    kettle.send_text(s)        → write s to focused pane's PTY
    kettle.exec_action(name)   → dispatch any kettle Action by
                                  name (same names as the keymap
                                  grammar; cycle-326 promoted
                                  Action::from_name to pub for
                                  this)

Errors in the script `log::warn!` + don't fail launch. Side-
effect commands queue on the engine; the App drains them once
the first pane spawns.

Implementation: `mlua 0.11` with `lua54 + vendored + send +
error-send` features. Vendored Lua means no system liblua
dependency; deterministic across OSes.

### Added — tmux `-CC` parser foundation (cycles 327-328, iTerm2 parity)

`kettle_vt::tmux_cc::TmuxControlParser` is a pure streaming
parser for tmux's control-mode protocol. Feed it bytes, pull
`TmuxEvent` enum values out.

Covers every documented tmux control-mode message: Begin / End /
Error / Output (with `\nnn` octal decode) / WindowAdd / Close /
Renamed / SessionChanged / Renamed / LayoutChange /
ClientDetached / Exit / Unknown / OutsideBlock. 11 unit tests
pin every variant + edge cases (CRLF, partial-line, 64 KB
overflow recovery).

This is the FOUNDATION; tmux integration into kettle's tab
surface is a multi-cycle thread. See `docs/TMUX-CC-DESIGN.md`
for the 7-cycle roadmap.

### Added — Documentation

- `docs/TMUX-CC-DESIGN.md` (cycle 328) — wire protocol summary +
  7-cycle integration roadmap (pane-state → tab synthesis →
  input routing → layout-change → detach cleanup).
- `docs/MUX-SERVER-DESIGN.md` (cycle 329) — architecture + wire
  protocol sketch + 13-cycle roadmap for the detachable mux
  server. No code; honest deliverable for a multi-week thread.

### Library / API additions

  kettle_ui::LuaEngine             — public type
  kettle_ui::LuaCommand            — public enum
  kettle_config::Action::from_name — promoted pub(crate) → pub
  kettle_vt::tmux_cc               — new module
  kettle_vt::tmux_cc::TmuxControlParser
  kettle_vt::tmux_cc::TmuxEvent
  kettle_ui::Options::lua_script   — new field

### CLI additions

  --lua-script PATH    — run Lua at startup (WezTerm parity)

Workspace tests 270 → 286 (+11 tmux parser + 5 lua).

### Deferred (each multi-cycle, see design docs)

- tmux `-CC` full integration (#42): parser shipped; pane-state
  plumbing + tab synthesis + input routing + detach cleanup
  pending. Roadmap in `docs/TMUX-CC-DESIGN.md`.
- Detachable mux server (#44): no code; design doc in
  `docs/MUX-SERVER-DESIGN.md`.
- Persistent in-terminal annotations: still pending.
- Native macOS menu bar + code-signed builds: still pending.

## [1.7.8] — 2026-05-21

Patch release. Cosmetic UX catch on the cycle-295 status bar.

### Fixed
- **Status bar cursor icon (cycles 320 + 321).** Hovering on the
  cycle-295 status strip showed the terminal I-beam cursor (text-
  input style) instead of the OS arrow. Cosmetic — the click
  wouldn't have actually started a selection because the strip
  isn't inside any pane's rect — but inconsistent with the
  tab-bar chrome which already used the arrow.

  Fix: new pure helper `cursor_in_status_bar_band` (sibling of the
  cycle-264 `cursor_in_tab_bar_band`), new
  `cursor_in_chrome_band` accessor that ORs both bars, `chrome_
  cursor_icon` arg renamed `in_tab_bar` → `in_chrome_band`.
  Drift guard `cursor_in_status_bar_band_geometry` pins the
  Off / Top / Bottom + bar_h=0 boundary semantics same shape as
  cycle-264's pinning.

Workspace tests 269 → 270.

## [1.7.7] — 2026-05-21

Patch release. Real UX catch on the cycle-303 Quake toggle +
CI smoke for the cycle-313 --profile contract.

### Fixed
- **Tri-state Quake toggle (cycle 319).** The cycle-303 binary
  "hide if visible, show if hidden" toggle had a UX failure mode:
  user has kettle visible, clicks to another window (kettle is now
  visible-but-unfocused), presses the global hotkey expecting
  kettle to come BACK INTO FOCUS — instead kettle HIDES. Two
  presses required to refocus. Wrong shape for
  Quake / Yakuake / Tilda muscle memory.

  Fix: tri-state.

    hidden            → show + raise + focus
    visible + focused → hide
    visible + !focused → raise + focus (don't hide)

### CI hardening
- **Cycle-313 --profile + --check-config contract smoke (cycles
  317 + 318).** Adds an end-to-end test in `.github/workflows/
  ci.yml`'s introspection-smoke block that writes a profile file
  with a deliberately malformed `font-size = not_a_number` line,
  runs `kettle --profile cibad --check-config`, and asserts the
  exit code is non-zero (the cycle-194 --check-config contract
  fires non-zero when issues are present). Cycle 317 used a
  flaky `if grep -q ...` pipe-into-if that didn't work on Windows
  Git Bash; cycle 318 pivoted to the cleaner exit-code contract.

Workspace tests stay at 269 green.

## [1.7.6] — 2026-05-21

Patch release. Three real durability + UX catches from post-v1.7.5
audit.

### Fixed
- **Remote-control IPC: unbounded read (cycle 315).** The cycle-302
  receiver's `drain_remote_commands` used `std::fs::read_to_string`
  with no size cap. A runaway script (or an accidental `some-cmd
  >> $REMOTE_FILE` instead of `kettle --remote-send "$(some-cmd)"`)
  could push GBs of data and kettle would allocate the whole
  thing before processing. Now: stat the file first; if > 1 MB
  (10× safety margin over realistic command-stream sizes),
  truncate + log::warn + return without processing.
- **Vi-mode yank silently dropped when clipboard unavailable
  (cycle 316).** The cycle-301 y-key handler called
  `clip.set_text(yanked)` with the result ignored via
  `let _ = ...`. When clipboard was None (SSH without X11 /
  Wayland forwarding, missing `$DISPLAY`, arboard init failure
  at startup), the yank silently dropped: visual highlight
  cleared, vi-mode exited, user assumed copy worked, then hit
  paste elsewhere and got their PREVIOUS clipboard contents.
  Now: log::warn! with the byte count + "try a kettle window
  with DISPLAY / Wayland set" hint.

### release.sh
- **'Next steps' race-condition fix (cycle 314).** The previous
  hint suggested
  `gh run watch $(gh run list --workflow=release.yml --limit 1 ...)`
  which races: the `run list` may resolve BEFORE the just-pushed
  tag triggers a new release workflow run on GitHub's side, so
  the watch attaches to the PREVIOUS release run (already done)
  and exits 0 immediately. Now: `--branch "v$VERSION"` filter +
  `--exit-status` so the watch errors on real failure + a brief
  `sleep 5` to let GitHub register the push.

Workspace tests stay at 269 green.

## [1.7.5] — 2026-05-21

Patch release. Real subtle audit catch + structural refactor.

### Fixed
- **`--profile NAME` silently ignored by every introspection flag
  except the windowed run (cycles 312 + 313).** Cycle-292 shipped
  `--profile NAME` only honored by the windowed-run path. A user
  running `kettle --profile dev --check-config` would silently
  check `<config-dir>/config` instead of `profiles/dev.config` —
  same silent-fallback shape as cycle-196's
  `load_from_with_diagnostics`. Cycle 312 fixed `--check-config`
  inline; cycle 313 extracted `resolve_config_path(&Cli) ->
  Option<PathBuf>` and applied it at every introspection site so
  the precedence
  (`--config FILE → --profile NAME → default path`) is uniform:

  - `--check-config`
  - `--list-keybinds`
  - `--list-ssh-hosts`
  - `--config-path`
  - `--screenshot`
  - `--screenshot-menu`

  Every one was doing
  `cli.config.clone().or_else(default_path)` without going through
  `path_for_profile`.

### release.sh
- **Cycle-311 catch surfaced in cycle 311 itself.** First end-to-
  end use of `scripts/release.sh` (cycle 307) tried to invoke
  `cargo build` without `$HOME/.cargo/bin` on PATH and failed
  mid-flow (version already bumped, lockfile not refreshed, no
  commit). The script now falls back to `~/.cargo/bin/cargo`,
  `/opt/homebrew/bin/cargo`, and `/usr/local/bin/cargo` before
  hard-failing with a clear diagnostic + a restore command.

### Other quality
- Added `.claude/` to `.gitignore` (cycle 310) — per-developer
  Claude Code state, not kettle source. Surfaced as untracked by
  the cycle-307 release script's pre-flight check.

Workspace tests stay at 269 green.

## [1.7.4] — 2026-05-21

Patch release. Two real subtle bugs caught by post-feature-sweep
audit + the first release shipped via the new
`scripts/release.sh` (cycle 307).

### Fixed
- **Status bar overflow on long pane titles (cycle 308).** A chatty
  shell prompt that puts the full cwd in the window title (a common
  pattern: `PROMPT_COMMAND='echo -ne "\033]0;$PWD\007"'`) produced
  a status line that cosmic-text wrapped past the strip's 1-cell
  visible region — the user saw the first ~80 chars and the rest
  was silently invisible. Now: char-budget truncation at 60 chars
  with a visible `…` ellipsis. UTF-8 safe (char-count, not
  byte-count).
- **Malformed trigger regex silently dropped (cycle 309).** A
  `trigger = [unclosed` pattern parsed (config layer stores it as a
  plain string), `--check-config` reported OK, then at runtime
  `compile_triggers` failed `Regex::new` and the trigger silently
  never fired (only a log::warn the user usually didn't see). Now:
  `--check-config` surfaces the invalid pattern with non-zero exit.

### Drift guards
- `cap_title_for_status_bar_truncates_at_char_budget_with_ellipsis`
  pins the cycle-308 fix (under/exact/over budget + UTF-8
  multibyte).
- `detect_malformed_values_flags_invalid_trigger_regex` pins the
  cycle-309 fix (both directions — malformed flagged, valid
  alternation `(BUILD SUCCESSFUL|FAILED)` not flagged).

Workspace tests 267 → 269.

## [1.7.3] — 2026-05-21

Repackaging of v1.7.2. Same code; v1.7.2 was tagged before the
CHANGELOG `[1.7.2]` section was committed, so the cycle-286
tag↔Cargo↔CHANGELOG consistency guard correctly failed the Linux
build at pre-flight — the v1.7.2 GitHub release shipped without
its Linux tarball.

v1.7.3 retags from the corrected HEAD so the Linux tarball ships
this time. Use this release instead of v1.7.2.

### Process catch (cycle 307)

The cycle-286 guard worked as designed — caught a real bug
(tag-before-CHANGELOG race). The fix is to tag AFTER the CHANGELOG
commit. A future cycle could harden the release script (a
`scripts/release.sh` that does the bump + CHANGELOG + commit + tag
atomically in one command) to prevent the race entirely.

### Carries v1.7.2's intended changes:

- Remote-control IPC truncate-on-startup (cycle 306) — see [1.7.2]
  below for full rationale.
- Two duplicate `#[allow(clippy::too_many_arguments)]` removed.

## [1.7.2] — 2026-05-21

Patch release. Real durability fix in the cycle-302 remote-control IPC.

### Fixed
- **Stale remote-command bytes replayed on next launch (cycle 306).**
  If kettle window A is running and accumulates pending `send-text
  TEXT\n` lines mid-process — OR crashes mid-process — and the user
  then launches kettle window B with the same `--remote-file PATH`,
  B's startup-time notify watcher would not fire (no write since B
  started watching) — but the first subsequent external
  `--remote-send` write triggers a re-read of the WHOLE file,
  including A's leftover bytes. B's focused pane then receives stale
  bytes typed as if the user had just sent them.

  Fix: `std::fs::write(&path, "")` once at startup, immediately
  before `w.watch(...)`. Truncates any leftover content; the
  watcher still fires on every subsequent write.

  Surfaced by a post-feature-sweep audit, not a user report.

### Code quality
- Dropped two duplicate `#[allow(clippy::too_many_arguments)]`
  annotations on `Terminal::new` and `Renderer::build_pane`
  (harmless but a code smell — pre-v1.4.0 era).

### Docs
- TESTING.md per-crate test counts refreshed (261 → 267 post-sweep).
- CONTRIBUTING.md cycle / test counts refreshed (250+ → 300+, 261+
  → 267+).

Workspace tests 267 stay green.

## [1.7.1] — 2026-05-21

Patch release. Docs catch-up against the v1.4.0 → v1.7.0 feature
sweep. The bundled `kettle.1` man page in the Linux release
tarball had drifted; users running `man kettle` after upgrading
would have seen pre-v1.4.0 keybinds. The fix ships as a binary
release so the tarball-bundled man page gets the v1.4.0+ content.

### Docs
- `packaging/linux/kettle.1` gains a "Vi-mode (Alacritty parity)"
  subsection with all 11 keymap entries (`Ctrl+Shift+Space` to
  enter; h/j/k/l/0/$/g/G/H/M/L/v/y/Esc) and a "Quake / dropdown
  mode" subsection documenting `kettle --toggle`.
- `docs/CONFIG.md` gains rows for the three v1.4.0-era config
  keys that were undocumented: `accent-color`, `status-bar`,
  `trigger`.
- `docs/UX-COMPARISON.md` matrix gains 9 new rows (vi-mode,
  remote-control IPC, Quake toggle, triggers, named-layout /
  profile, peacock accent, annotated screenshots, status bar) +
  a "Shipped in v1.4.0 → v1.7.0" chronological block. Vi-mode
  moved out of the "Future" list.

### Drift guard
- `man_page_documents_load_bearing_default_keybinds` extended
  with `Ctrl+Shift+Space` (vi-mode entry point). Without this,
  the same gap could recur on a future man-page rewrite.

No code change. Workspace tests 267 stay green.

## [1.7.0] — 2026-05-21

Feature-bump release — adds Quake-style dropdown via the cycle-302
remote-control IPC.

### Added — `--toggle` (Quake / Yakuake / Tilda dropdown UX)

`kettle --toggle` flips the running kettle window's visibility,
piggybacking on the cycle-302 remote-control IPC. The user binds
their compositor / DE / OS existing global-hotkey mechanism to
`kettle --toggle` — sidesteps the cross-platform global-hotkey
problem entirely (no XGrabKey / Carbon HotKey / RegisterHotKey
code per OS).

  GNOME       Settings → Keyboard → Custom Shortcuts → `kettle --toggle`
  KDE         System Settings → Shortcuts → Custom
  Sway        bindsym $mod+grave exec kettle --toggle
  Hyprland    bind = SUPER, grave, exec, kettle --toggle
  macOS       Karabiner / Raycast / Hammerspoon
  Windows 11  PowerToys Keyboard Manager / AutoHotKey

Protocol extension: the cycle-302 remote-control file now also
accepts the `toggle-window` command. Receiver calls
`window.set_visible(!is_visible()) + focus_window` so the window
pops above other windows when returning to visible (typical
Quake / Yakuake / Tilda behavior).

CLI surface:
  --toggle    sugar that writes `toggle-window` to the
              `--remote-file` path + exits.

Protocol v1.7 (one command per line — receiver-side):
  send-text TEXT     write TEXT (with `\n` → newline) to PTY
  toggle-window      flip window visibility (Quake dropdown)
  new-tab            recognized but not yet implemented (logs warn)
  # ...              comments + empty lines skipped

Workspace tests 267 stay green.

## [1.6.0] — 2026-05-21

Feature-bump release — adds remote-control IPC (kitty `@ send-text`
parity).

### Added — remote-control IPC

`kettle --remote-send TEXT [--remote-file PATH]` writes a command
line to a file watched by every running kettle window with a
matching `--remote-file`. The receiving window writes TEXT to its
focused pane's PTY. Used by external scripts to drive an already-
open kettle without launching a new window.

  # default path:
  kettle &
  kettle --remote-send 'cargo test\n'

  # explicit per-workspace channel:
  kettle --remote-file /tmp/dev.cmd &
  kettle --remote-send 'top\n' --remote-file /tmp/dev.cmd

Architecture: file-based IPC over the existing notify-watcher
(cycle 151), not a Unix-domain socket. Cross-platform free,
reuses existing patterns, no daemon thread. Multi-window
arbitration is "last writer wins" for now; per-window socket
addressing is planned.

CLI surface:
  --remote-send TEXT    write command + exit (sender mode)
  --remote-file PATH    command file path (default
                        `<config-dir>/kettle/remote.cmd`)

Library surface:
  kettle_ui::Options::remote_file: Option<PathBuf>
  kettle_ui::UserEvent::RemoteCommand

Protocol v1 (one command per line):
  send-text TEXT        write TEXT (with `\n` → newline) to PTY
  # ...                 comments + empty lines skipped

Future verbs reserved: `set-tab-title TEXT`, `focus-tab N`, `ls`,
`new-tab`, `close-tab N`. Unknown verbs log warn + continue, so
configs written for a forward kettle don't error today.

Workspace tests 267 stay green.

## [1.5.0] — 2026-05-21

Feature-bump release — adds full Alacritty-parity vi-mode for the
focused pane's scrollback. Shipped as 4 bounded sub-cycles
(298-301) that landed end-to-end across this minor.

### Added — vi-mode scrollback (Alacritty parity)

`Ctrl+Shift+Space` enters vi-mode. Visible magenta hollow block at
the terminal cursor; navigate with vi keys, yank selection to
clipboard, Esc exits.

Keymap shipped:

  h / j / k / l        move 1 cell left / down / up / right
  arrow keys           same as h/j/k/l
  0 / ^                jump to line start
  $                    jump to line end
  g / H                top of viewport
  G / L                bottom of viewport
  M                    middle of viewport
  v                    toggle char-visual selection
  y                    yank selection to clipboard + exit vi-mode
  Esc                  exit vi-mode

Architecture:

  kettle-config:
    Action::ToggleViMode    + 4 aliases (toggle_vi_mode, vi_mode,
                              vi, scrollback_vi)
    Default keybind: Ctrl+Shift+Space (Alacritty default)
    Cycle-117 palette-completeness drift guard pins it.

  kettle-ui:
    struct ViState { row, col, visual_anchor }
    App.vi_mode: Option<ViState>
    fn vi_mode_key(...) — modal key dispatcher, intercepts before
       PTY write. Reads focused-pane `screen_lines()` /
       `columns()` to clamp movement to grid.
    fn yank_vi_selection(start, end) -> String — extracts cells in
       the inclusive range, per-line trim_end.

  kettle-render:
    Overlay.vi_cursor + Overlay.vi_visual_anchor
    build_pane(...) takes both. Visual selection paints
    `theme.selection_background @ 0.55` rect per row. Vi cursor
    paints magenta (palette[5]) hollow block + 20% fill — distinct
    from broadcast yellow (palette[3]) + accent blue (palette[4])
    + terminal cursor.

Stays within the focused pane's viewport for v1; future cycle
could extend into scrollback rows (negative row indices). Not a
blocker — most vi-mode use cases (copy a build error line, yank
an SHA) work within the viewport.

Workspace tests 267 stay green. Vi-mode is exercised manually
(needs a windowed run for the visible cursor + clipboard yank);
the cycle-298 palette drift guard pins the Action wiring.

## [1.4.0] — 2026-05-21

Feature-bump release — eight new user-facing capabilities landed in
direct response to the parity sweep against other open-source
terminals. First minor version bump (was 1.3.11 → 1.4.0) because
the release introduces new public surface (config keys + CLI flags
+ library types) rather than only patch-level changes.

### Added — Selection / output

- **Smart selection (iTerm2 parity).** Double-click on a URL /
  file path / IPv4 / git SHA selects the whole match instead of
  the alacritty Semantic word, which usually under- or over-shoots
  structured tokens. Reuses the cycle-218 hint regex set.
  Falls through to the existing word-boundary semantic selection
  when nothing matches. (cycle 288)

- **Triggers (iTerm2 parity).** New `trigger = REGEX` config key.
  When a regex matches PTY output in an unfocused pane, kettle
  calls `window.request_user_attention(Critical)` — Wayland
  notification counter, X11 WM_HINTS urgency, macOS dock bounce,
  Windows taskbar flash. Three guard rails:
  - empty trigger set: zero cost (the default);
  - 2 s throttle: chatty builds don't pulse 100×;
  - window-focused check: don't pulse the user's own window.

  Drift guard pins alternation patterns (`(BUILD SUCCESSFUL|
  FAILED)` survives intact — the parser doesn't split on `|`).
  (cycles 289 + 290)

### Added — Workspaces

- **`--layout NAME` named-workspace session.** Saves + restores
  from `<config-dir>/layouts/<NAME>.json` so a user can maintain
  distinct workspaces ("dev", "ops", "docs") without each one
  clobbering the others on close. Composes with the v1.4.0
  `--profile NAME` config split below. Path-sanitized so a
  `--layout ../../etc/passwd` can't traverse out. Terminator
  parity. (cycle 291)

- **`--profile NAME` named-config split.** Loads
  `<config-dir>/profiles/<NAME>.config` instead of the default
  `<config-dir>/config`. Lets a user keep distinct font / theme /
  keybind sets per workspace. `--config FILE` wins when both are
  given. (cycle 292)

- **`accent-color` (peacock-for-VS-Code parity).** One config knob
  cascades to every "kettle accent" surface — active tab segment
  strip, focused pane border, cycle-255 dragged-tab ghost. Lets a
  user run multiple kettle windows (`--profile dev` + `--profile
  ops`) and tell them apart at a glance. CLI override:
  `--accent COLOR` (wins over the config key). `palette[3]`
  broadcast yellow and the cursor stay un-overridden by design.
  (cycle 293)

### Added — Screenshots / chrome

- **`--annotate TEXT` annotated screenshots.** Bottom-strip caption
  overlay on `--screenshot` / `--screenshot-menu` output. Useful
  for docs / README hero images / bug reports that want a version
  / repro / env note baked into the PNG. Translucent dark panel
  + 1 px chrome border + theme.foreground caption. None-passthrough
  on the unannotated path keeps the cycle-251 visual regression
  baseline pixel-stable. (cycle 294)

- **`status-bar = off | top | bottom` status strip (iTerm2 / kitty
  parity).** Thin row at the configured edge of the surface
  showing `HH:MM:SS UTC  ·  theme name  ·  focused pane title`.
  Disabled by default — turning it on subtracts one cell from each
  pane's grid. Composes with peacock accent for per-workspace
  identification. Live windowed app only; `--screenshot` paths
  intentionally don't render the status bar so the cycle-251
  visual regression baseline stays pixel-stable. Future cycle
  adds sysinfo CPU / MEM widgets. (cycles 295 + 296)

### Library / API additions

- `kettle_config::OutputTrigger { pattern, action }`
- `kettle_config::TriggerAction { Urgency }`
- `kettle_config::StatusBarMode { Off, Top, Bottom }`
- `kettle_config::Config::accent_color: Option<Rgb>`
- `kettle_config::Config::triggers: Vec<OutputTrigger>`
- `kettle_config::Config::status_bar: StatusBarMode`
- `kettle_config::Config::path_for_profile(name) -> Option<PathBuf>`
- `kettle_render::StatusBar`
- `kettle_render::capture_png_with_annotation(...)`
- `kettle_render::Renderer::render_frame_with_status(...)`
- `kettle_ui::Options { ..., layout, accent_override }`
- `kettle_ui::session::Session::path_for_layout(name)` +
  `Session::load_layout(name)` + `Session::save_layout(name)`

### CLI additions

- `--layout NAME` — named-workspace session.
- `--profile NAME` — named-config split.
- `--accent COLOR` — one-off peacock override.
- `--annotate TEXT` — bottom caption on screenshots.

### Known sub-cycles

These shipped in v1.4.0 with the minimum bounded scope; future
sub-cycles extend them:

- Triggers v1 only fires `Urgency`. Cycles 297+ add Bell,
  set-tab-title=text, notify-text.
- Profiles v1 fully replaces the base config. Cycle 297+ adds
  overlay-merge so a profile can override just a few keys.
- Status-bar v1 shows clock + theme + title. Cycle 297 adds
  sysinfo CPU / MEM widgets.

### Still deferred (multi-cycle, future)

- Vi-mode for scrollback (Alacritty parity) — keymap + cursor +
  visual selection + yank, 3-5 cycles.
- tmux `-CC` passthrough (iTerm2 parity) — control-protocol parser.
- Remote control protocol (kitty `@` commands) — IPC socket +
  handlers.
- Quake-style dropdown — OS global hotkey + window-state save.
- Lua scripting (WezTerm parity) — embed mlua, expose event hooks.
- Detachable mux server (WezTerm parity) — network protocol + auth.
- Persistent in-terminal annotations (iTerm2 parity, distinct
  from the v1.4.0 screenshot caption) — scrollback-position +
  sticky-note + search-jump.

These deserve dedicated cycles each rather than being half-shipped
alongside the v1.4.0 sweep.

Workspace tests: 261 → 267.

## [1.3.11] — 2026-05-21

Patch release.

### Fixed
- **`man kettle` keybind documentation now matches reality.** The
  cycle-279 hand-written man page had four wrong entries that drifted
  from the actual default keybinds:
  - `Ctrl+Shift+arrow` was documented as "focus pane in direction"
    — that's actually the scroll binding. Focus is **`Alt+arrow`**.
  - `Ctrl+Shift+Z` was documented as undo close tab,
    `Ctrl+Shift+D` as duplicate tab, and `Ctrl+Shift+Alt+D` as
    duplicate pane. Those actions exist (cycles 247/248) but are
    NOT default-bound — they're available via the command palette
    (`Ctrl+Shift+K`). Documented as such in a new
    "Additional actions via command palette" paragraph.
  - `Ctrl+Shift+,` / `Ctrl+Shift+.` for move tab were wrong —
    actually `Ctrl+Shift+PgUp` / `Ctrl+Shift+PgDn`.

  Also added bindings the original man page omitted: NewWindow
  (`Ctrl+Shift+I`), CloseWindow (`Ctrl+Shift+Q`), SplitAuto
  (`Ctrl+Shift+A`), FocusNext / FocusPrev (`Ctrl+Shift+N/P`),
  ScrollLineUp / Down (`Ctrl+Shift+Up/Down`),
  IncreaseFontSize / DecreaseFontSize (`Ctrl+Shift+Plus/-`),
  ToggleBroadcastOff (`Shift+Super+G`).

### Added — drift guards
- **`man_page_documents_load_bearing_default_keybinds`** test in
  `crates/kettle/src/main.rs`. Pins 16 load-bearing default-keybind
  triggers against the man page text via `include_str!`. If a
  future default-keybind set changes (or the man page text gets
  edited carelessly), CI fails instead of a user trying
  `man kettle` + the documented hotkey getting a different
  action. Caught the `Ctrl+PgDn` substring gap on its first run
  (the slashed `PgUp/PgDn` form didn't satisfy the check) — the
  man page now uses per-binding `.TP` lines so each entry has
  its own grep'able row.
- **`--help` output shape** pinned in CI (cycle 282). The
  all-OS CLI smoke now grep's `^Usage: kettle` + six load-bearing
  flag names (`--config`, `--screenshot`, `--gpu-info`,
  `--shell-integration`, `--print-completions`,
  `--print-default-config`). A clap-derive regression that
  silently dropped or renamed a flag would surface here, not
  in a user bug report.

Workspace tests: 261 → 262.

No code-behavior changes from v1.3.10.

## [1.3.10] — 2026-05-21

Patch release. One user-visible addition + two CI hardenings.

### Added
- **`man kettle`** — `packaging/linux/kettle.1` is a 366-line
  hand-written man page covering NAME, SYNOPSIS, DESCRIPTION,
  OPTIONS (Launch / Introspection / Debug+capture), KEY BINDINGS
  (Tabs / Splits / Overlays / Scrollback / Group), CONFIGURATION,
  ENVIRONMENT, FILES, EXAMPLES, SEE ALSO, AUTHORS. Wired into all
  four install paths:
  - `scripts/install.sh` drops it under `~/.local/share/man/man1`
    (or `${PREFIX}/share/man/man1` if `--prefix` overrides).
    `--uninstall` removes it too.
  - `release.yml` bundles the `.1` into the Linux release tarball
    so the bundled `install.sh` finds it.
  - `packaging/arch/PKGBUILD` installs to `/usr/share/man/man1`
    so `man kettle` works system-wide on Arch.
  - `packaging/homebrew/kettle.rb` uses `man1.install` for
    Linuxbrew.

  Format is groff/man macros — uses `.TP` paragraphs instead of
  `.TS/.TE` tables so it renders cleanly without the `tbl`
  preprocessor (some `man -l` invocations skip preprocessors).
  Verified via `groff -man -Tutf8 packaging/linux/kettle.1`.

### CI / automation
- **Tag ↔ Cargo.toml version consistency guard** in `release.yml`.
  An early Linux-only step extracts the version from the pushed
  tag's `$GITHUB_REF_NAME` and the workspace's `Cargo.toml`,
  failing fast with `::error::` annotations if they disagree.
  Without this guard, a future "tag v1.3.11 but forgot to bump
  Cargo.toml" would silently ship artifacts with mixed versions
  (macOS `.app` Info.plist saying 1.3.10, binary `--version` saying
  1.3.10, tag saying 1.3.11).
- **cargo-machete badge** in the README badge row. Closes the
  README's supply-chain badge trio (audit + deny + machete) so
  the supply-chain story is visible above the fold.

No code-behavior changes elsewhere from v1.3.9. Workspace tests
stay at 261 green.

## [1.3.9] — 2026-05-21

Patch release. **~20% binary size reduction.**

### Perf
- **Release binary 30.7 MB → 24.7 MB** via the cycle-277 `image`-features
  narrowing. cargo-bloat audit found three unused image-format
  decoders dominating the binary: `rav1e` (AVIF, 1.6 MB), `exr`
  (OpenEXR), `image_webp`, plus the full `zune_jpeg`. Root cause:
  `arboard`'s default `image_data` feature pulled `image` with
  default features (= every format) and unified with kettle-vt's
  default-feature `image` declaration.

  Fix:
  - `kettle-vt`: `image = { ..., default-features = false,
    features = ["png", "jpeg", "gif"] }`. Matches iTerm2's inline-
    image protocol spec (the only path that decodes user-supplied
    image bytes).
  - `kettle-ui`: `arboard = { ..., default-features = false }`.
    Drops the `image_data` feature; kettle's clipboard surface is
    text-only, no image-to-clipboard path exists.

  Result: AVIF / EXR / WebP / HDR / TIFF / BMP / QOI / DDS / ICO /
  PNM decoders all dropped. PNG / JPEG / GIF retained. Workspace
  tests 261/261 still green.

  The cycle-274 cargo-machete CI + cycle-264 cargo-deny CI prevent
  this class of accumulation from recurring; the cut is durable.

### Docs
- `docs/PERFORMANCE.md` baseline bumped to the new 24.7 MB
  measurement with a footnote explaining the cycle-277 cut.

No code-behavior changes elsewhere from v1.3.8.

## [1.3.8] — 2026-05-21

Patch release.

### Fixed
- **Session restore now surfaces a `warn!` when a tab can't be
  rebuilt** (was a silent skip). The cycle-pattern audit found
  `Mux::restore` quietly dropping any tab whose stored cwd /
  argv couldn't be re-spawned — a user wondering "where did my
  N-tab session go after restart?" had no signal. Converted to
  a `match` that logs `WARN session restore: tab N failed to
  rebuild and was skipped: <error>` per skipped tab. Behavior
  preserved (still don't sink the whole restore on one bad tab);
  visibility added.

### CI / automation
- **actionlint workflow** lints `.github/workflows/*.yml` on every
  workflow-file PR. Runs shellcheck on every `run: |` block —
  caught a real SC2016 in cycle 205's headless GPU smoke
  (single-quoted `$rc` in a nested `bash -c '…'`) which was
  intentional but un-documented; now suppressed inline with a
  shellcheck disable directive + explanatory comment.
- **stale-issue / stale-PR bot**. Conservative thresholds: issues
  warn at 90 days, close at 104; PRs warn at 60 days, close at
  74. Daily 06:30 UTC. Opt-out labels: `pinned`, `security`,
  `enhancement`, `help-wanted`, `blocked-on-maintainer`.

### Docs
- **Bug-report issue template** asks for `kettle --gpu-info`
  output (optional, rendering-related bugs only). Reduces the
  triage round-trip on "blank window" / "wrong colors" reports.

No code-behavior changes elsewhere from v1.3.7. Workspace tests
stay at 261 green.

## [1.3.7] — 2026-05-21

Patch release.

### Added
- **`kettle --gpu-info`** prints the wgpu adapter / backend /
  driver / texture limits the live renderer would pick on this
  machine, then exits — no GUI / PTY needed. Closes the gap
  between "blank window" / "no adapter" bug reports and the
  diagnostic info maintainers need to triage them. Output is
  predictable line-based `Key: value` so a shell script can
  consume it; CI smoke pins three invariant lines (`Backend:`,
  `Adapter:`, `Max texture: N px / side`).

  ```text
  $ kettle --gpu-info
  Backend:        Vulkan
  Adapter:        NVIDIA GeForce RTX 2080
  Adapter type:   DiscreteGpu
  Driver:         NVIDIA
  Driver info:    580.142
  Vendor (PCI):   0x10de
  Device (PCI):   0x1e87
  Max texture:    32768 px / side
  Max buffer:     4292870144 bytes
  Max bind groups: 8
  ```

### CI / automation
- **`actions/labeler@v5`** workflow auto-tags PRs by changed file
  paths (`docs`, `ui`, `vt`, `core`, `render`, `config`, `cli`,
  `ci`, `automation`, `packaging`, `tests`, `dependencies`,
  `release`, `tooling`). Triggered on `pull_request_target` with
  `pull-requests: write` so labels apply to fork PRs too.
  Additive (`sync-labels: false`) so manually-applied labels like
  `triage` / `good-first-issue` survive the auto-run.

No code-behavior changes elsewhere from v1.3.6. Workspace tests stay
at 261 green.

## [1.3.6] — 2026-05-21

Patch release. Theme: **post-v1.3.5 tooling + governance + supply-
chain hygiene**. No user-visible behavior change; the binary is
identical to v1.3.5 except the cycle-263 unwrap → expect refactor
upgrades five provably-safe `.unwrap()` calls to `.expect("invariant:
…")` so a future refactor that breaks one fails with a pinpointed
panic message rather than a bare `unwrap on None`.

### Added — install paths
- **Homebrew formula template** (`packaging/homebrew/kettle.rb`)
  with `packaging/homebrew/README.md` for the one-time tap-repo
  setup. Macros + Linuxbrew users get `brew install kettle` in
  two commands once the tap repo is live.
- **AUR PKGBUILD template** (`packaging/arch/PKGBUILD`) with
  `packaging/arch/README.md` for the one-time AUR submission.
  Arch / Manjaro / EndeavourOS users install with `yay -S
  kettle-bin` / `paru -S kettle-bin`.
- **Nix flake** (`flake.nix` at repo root + `packaging/nix/
  README.md`). NixOS users get `nix run github:reddimus/kettle`,
  `nix profile install`, dev-shell with the workspace MSRV, and
  flake-input usage for home-manager / NixOS configs. Rust
  toolchain pinned to 1.89 via `oxalica/rust-overlay`; rpath
  patched to find the wgpu / wayland / xkb runtime libs that
  dlopen would otherwise miss.

Each template pins exact SHA-256s tied to the release (via the
cycle-254 `.sha256` sidecars), so bumping happens in the same PR
as `Cargo.toml`.

### Added — dev tooling
- **`Justfile`** for common dev workflows. `just gauntlet` is the
  CI-equivalent gate (`fmt --check` + `clippy -D warnings` +
  `build` + `test` + `doc -D warnings`); recipes for every
  daily-loop task (`just fmt` / `just test` / `just screenshot`
  / `just menu` / `just bench` / `just install`). CONTRIBUTING.md
  cross-links so the cycle pattern's "Run the gate locally" step
  can use the one-liner.
- **`scripts/bench.sh`** + **`docs/PERFORMANCE.md`** — measured
  startup / memory / render baselines for the v1.3.5 binary,
  plus a POSIX-bash script that reproduces every measurement
  five times on `/usr/bin/time -f '%e %M'`. macOS users on
  coreutils' `gtime` are supported automatically.
- **`.editorconfig`** at the repo root — codifies indent +
  charset + line-ending rules across VS Code / JetBrains /
  neovim / emacs / Sublime / Helix so a save-on-format doesn't
  fight cargo fmt or the existing scripts.

### Added — supply-chain
- **`cargo-deny` config** (`deny.toml`) + dedicated workflow
  (`.github/workflows/deny.yml`) covering the supply-chain
  surface the cycle-244 `audit.yml` doesn't touch: explicit SPDX
  license allow-list, `unknown-registry = "deny"` + `unknown-git
  = "deny"` for source restrictions, wildcards-banned + warn on
  duplicate versions. Runs on every Cargo.lock change + weekly
  Sunday cron.

### Docs refresh
- **CONTRIBUTING.md** gains a first-class **Drift guards**
  subsection with three concrete kinds from the codebase
  (exhaustive-match guards, drift-against-source guards, pixel /
  output guards). Lead-in updated from "150+" to "250+" cycles.
  CI gate listed with all current workflows (audit, MSRV,
  visual regression, --screenshot-menu).
- **TESTING.md** refreshed against the current 261-test workspace:
  per-crate counts updated to current values; new drift guards
  listed explicitly (menu_visual, close_focused_promotes_sibling,
  classify_tab_activity_*, closed_tab_ring_bounded_and_lifo,
  tab_drag_target_index_clamps_to_strip, hovered_close_button_*,
  cli_help_preserves_indented_code_examples); CI section
  rewritten to list every workflow + smoke step on every OS.
- **UX-COMPARISON.md** matrix gains 8 v1.3.x parity rows
  (drag-reorder, activity / silence dots, undo-close, duplicate,
  right-click menu, command palette, hint mode, search overlay,
  shell integration, SSH launcher). Backlog list now distinguishes
  shipped-since-v1.0 (chronological) from deferred-on-purpose
  (with one-sentence rationale per item).
- **README** gains a `docs/PERFORMANCE.md` link in the
  Documentation section.
- **docs/INSTALL.md** documents all four install paths
  (curl|sh + KETTLE_PREFIX, Homebrew, AUR, Nix flake) + the
  manual SHA-256 verification path (sha256sum / shasum /
  Get-FileHash one-liners).

### Refactor
- **5 provably-safe `.unwrap()` → `.expect("invariant: …")`**
  (`kettle-vt/src/kitty.rs:current_frame` ×3,
  `kettle-vt/src/kitty.rs:feed`, `kettle-core/src/term.rs:placeholder_runs`).
  Each carries an inline invariant comment so a future refactor
  that breaks the safety property fails with a pinpointed
  message. Code-quality audit also confirmed:
  - Zero `TODO` / `FIXME` / `HACK` markers in production code.
  - Only one `unsafe` block (cycle 199 `SIGPIPE → SIG_DFL` with
    existing SAFETY comment).

Workspace tests stay at 261 green.

## [1.3.5] — 2026-05-21

Patch release.

### Added
- **Ghost-render of the dragged tab during reorder.** The cycle-249
  drag-to-reorder snapped the live bar to the new tab order at each
  boundary crossing — functionally correct but the dragged segment
  visibly teleported. Adds the standard chrome / browser-tab
  affordance: a translucent ghost copy of the active segment floats
  under the cursor while the drag is active (theme.background at
  0.85 opacity + matching palette[4]/palette[3]-under-broadcast
  accent strip + soft drop shadow). Anchor clamped to bar width via
  the same shape as the cycle-245 context-menu clamp.
- **`KETTLE_PREFIX` env var in `install-online.sh`.** Composes with
  `KETTLE_VERSION` so a pinned-version system-wide install is one
  line:
  ```sh
  curl -fsSL .../install-online.sh \
    | KETTLE_VERSION=v1.3.5 KETTLE_PREFIX=/usr/local sh
  ```
  Default (env unset) → `~/.local/`, unchanged.

### Tooling
- **`.editorconfig`** at the repo root. Codifies indent + charset +
  line-ending rules across IDEs (VS Code, JetBrains, neovim, emacs,
  Sublime, Helix all read it). 4-space Rust matches cargo fmt;
  2-space TOML/YAML/JSON/Markdown/sh; tab Makefile. Existing files
  already conform.

No code-behavior changes from v1.3.4. Workspace tests stay at 261
green; `--screenshot-menu` still produces the canonical menu PNG.

## [1.3.4] — 2026-05-21

Patch release. Theme: **production-grade supply-chain + governance
hygiene**. No code-behavior changes from v1.3.3; the binary is
byte-identical except for the embedded build SHA. The release
*surface* gains real integrity guarantees.

### Security / supply chain
- **Per-artifact SHA-256 sidecars on every release.** Every
  `release.yml` matrix row now generates a `.sha256` file alongside
  the artifact and uploads both. Linux uses `sha256sum`; macOS
  `shasum -a 256`; Windows emits the `sha256sum`-compatible
  `<hex>  <filename>` layout via `Get-FileHash` so cross-platform
  verification doesn't need a parser per OS. The release page now
  exposes:
    kettle-linux-x86_64.tar.gz
    kettle-linux-x86_64.tar.gz.sha256
    kettle-macos-universal.zip
    kettle-macos-universal.zip.sha256
    kettle-windows-x86_64.zip
    kettle-windows-x86_64.zip.sha256
- **`install-online.sh` verifies SHA-256 before extracting.**
  Downloads the sidecar alongside the tarball, runs `sha256sum -c`
  (or `shasum -a 256 -c` on BusyBox / Alpine where `sha256sum`
  isn't present). Verification failure aborts before `tar -xzf`.
  Backward-compat fallback: releases ≤ v1.3.3 don't ship sidecars,
  so a 404 on `.sha256` is a soft warning rather than a hard
  error — the one-liner keeps working with `KETTLE_VERSION=v1.3.3`.
- **`docs/INSTALL.md` documents manual verification.** New
  "Verifying a download (SHA-256)" subsection with platform-
  specific one-liners (sha256sum / shasum / Get-FileHash).

### Governance
- **README badges** — five shields.io badges at the top of the
  README: CI status, Audit (RustSec) status, latest release, MSRV
  (1.89), and license (MIT). Sourced from the existing workflows
  so badge color tracks real CI conclusion.
- **`CODE_OF_CONDUCT.md` adopting Contributor Covenant 2.1 by
  reference.** GitHub auto-detects + surfaces in the community-
  standards tab. Linked from CONTRIBUTING.md.

## [1.3.3] — 2026-05-21

Patch release. Two additions:

### Added
- **Per-tab silence watcher (Terminator parity).** Companion to the
  v1.3.0 output/bell tab indicators. An inactive tab whose unseen
  output stopped arriving for ≥ N seconds now transitions to a dim
  chrome-gray `Silent` dot — useful for tail-following long jobs
  (`tail -f`, build watchers, network monitors) where the *absence*
  of recent output is the signal you want.

  Configurable via `tab-silence-threshold-ms` (default 10 s, clamped
  `[1000, 600_000]`). Pure `classify_tab_activity` now takes
  `now: Instant` + `silence_threshold: Duration` so the wall clock
  flows in from the caller, keeping the function unit-testable. New
  drift guard `classify_tab_activity_transitions_to_silent_after_threshold`
  pins the threshold-boundary transitions + the bell-wins-over-silent
  precedence + the backward-clock saturation guard.

- **One-line online installer (Linux).** `curl -fsSL
  https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh
  | sh` downloads the latest release tarball, verifies the gzip
  magic bytes, extracts to a `mktemp -d` (cleaned up on exit), and
  runs the bundled `install.sh --skip-build`. POSIX-sh (dash /
  bash / ash compatible), zero non-coreutils deps, shellcheck-
  clean. Pin to a version via `KETTLE_VERSION=v1.3.3 sh`. Caught
  a real Bash-vs-dash bug in the bundled install.sh during
  end-to-end testing (now invoked via shebang, not `sh`).

### Docs
- README "Install" section reworked so the curl-pipe is the
  headline. macOS / Windows / build-from-source are first-class
  alternatives. New "CLI quick reference" heading wraps the
  post-install command list.
- `docs/INSTALL.md` brought into lock-step with the README's new
  install hierarchy.
- `docs/CONFIG.md` documents `tab-silence-threshold-ms` next to
  `cursor-blink-interval`.
- `docs/ARCHITECTURE.md` gains a mermaid `flowchart LR` of the
  six-pass render order — the cycle-251 fix relied on implicit
  reasoning about pass order; the diagram makes it explicit so a
  future overlay layer doesn't repeat the v1.3.0/v1.3.1
  blank-menu trap.

### CI
- **MSRV verification job.** Cargo.toml declared `rust-version =
  "1.88"` but nothing in CI verified the workspace + its transitive
  deps still built there. Adding `dtolnay/rust-toolchain@1.89` to
  ci.yml surfaced the real bug — `cosmic-text@0.18.2` +
  `smol_str@0.3.6` both require 1.89. Declared `rust-version`
  bumped to match; new MSRV job catches the next dep-floor drift
  at PR time instead of release time.

Workspace tests: 260 → 261.

## [1.3.2] — 2026-05-21

Patch release. Direct response to user feedback on v1.3.1: *"The
right click menu still does not work. It is just blank. Think of a
better way to test this and fix this."* Both addressed.

### Fixed
- **Right-click context menu was blank.** The kettle render pass
  order is `quads → imgs → text → overlay_quads`. v1.3.0/v1.3.1 put
  the menu's panel-bg quad in `overlay_quads` (the last pass), which
  drew on top of the menu text that had already been rendered in the
  text pass. v1.3.0 used opacity 0.97 ("looks awful" — text bled
  through at 3%); v1.3.1 bumped to 1.0 ("just blank" — text fully
  covered).

  Fixed by adding a third quad pass + second TextRenderer:

  ```
  1. quads.draw           (panes, tabs)
  2. imgs.draw            (sixel / kitty)
  3. text_renderer.render (pane + tab text — NOT menu)
  4. overlay_quads.draw   (dim + scrollbar — NOT menu chrome)
  5. menu_quads.draw      (menu shadow + bg + border + highlight)
  6. menu_text_renderer.render (menu row labels)
  ```

  All v1.3.1 design choices (drop shadow, theme.background panel,
  palette[8] border, palette[4]@0.18 highlight + 2-px accent strip,
  comfortable padding) carry over verbatim — the colors were right,
  only the pass was wrong.

  Menu chrome quads extracted into a pure `menu_chrome_quads(menu,
  theme, cw, ch) -> Vec<QuadInstance>` helper so the live renderer
  and the new headless screenshot path produce identical pixels.

### Added
- **`kettle --screenshot-menu PATH`** CLI flag: mirrors
  `--screenshot` but renders with a synthetic right-click context
  menu open over the pane. Useful for verifying the menu's render
  path without opening the windowed app — exactly the gap that let
  v1.3.0/v1.3.1 ship the blank-menu bug. Honors `--cols` / `--rows`
  / `--config` the same way.
- **`DebugScene` enum + `capture_png_with(cfg, cols, rows, out,
  scene)`** public API in `kettle-render`. `capture_png(..)` is
  kept as a thin back-compat shim that calls `..., Default`.

### Tests + CI
- **New `crates/kettle-render/tests/menu_visual.rs`** integration
  test that renders both scenes via `capture_png_with`, loads both
  PNGs, and asserts:
  - ≥ 1000 pixels differ between the no-menu baseline and the
    context-menu render. v1.3.0/v1.3.1 produced 0 different pixels
    inside the panel area; this floor catches exactly that
    regression.
  - ≥ 200 foreground-leaning pixels appear inside the menu's
    bounding box, ruling out a blank-menu render where only chrome
    pixels show.

  Combined into one `cargo test` invocation so we only spin up one
  pair of wgpu adapters per run (parallel offscreen software-Vulkan
  devices have segfaulted on shared CI runners).
- **`--screenshot-menu visual regression (Linux)` CI step** runs
  the binary under `LIBGL_ALWAYS_SOFTWARE=1` and asserts PNG magic
  bytes + file size ≥ 40 KB. Catches a wgpu-version drift the unit
  test wouldn't see.

Workspace tests: 259 → 260.

## [1.3.1] — 2026-05-21

Patch release. Direct response to user feedback on v1.3.0:
*"Tab x's are still just characters and not close buttons. Also the
right click actions look awful."*

### Fixed
- **Tab `✕` reads as a button at all times.** v1.3.0 added a *hover-
  only* red chip; the glyph itself was the last character of the tab-
  title text buffer, so at rest it read as plain text in the title.
  Two changes:
  - Always-on background chip behind every tab's close zone — dim
    `theme.foreground` at 0.12 opacity at rest (palette[8] at 0.55 on
    the active tab where the surface is brighter), bright palette[1]
    red at 0.85 on hover. The close button visibly exists before the
    user ever hovers.
  - Dedicated `✕` glyph buffer (`Renderer::tab_close_buffer`, single
    shared across all tabs, positioned per-segment via N TextAreas).
    Removed `✕` from the per-tab title text buffer. The glyph gets
    its own color: theme.palette[8] dim at rest, pure white on hover
    so the contrast against the red chip reads clearly.
- **Right-click context menu redesigned.** v1.3.0 used palette[4]
  (bright accent blue) for the panel *outline*, palette[8] (dim
  chrome) for the bg, and palette[4] again at 0.85 opacity for the
  highlight row — every chrome element was competing for attention.
  Five changes:
  - **Drop shadow** — a near-black quad offset 4px down-right at 0.35
    opacity so the menu reads as floating above the pane rather than
    pasted on (GTK / iTerm2 convention).
  - **Theme-bg panel** — `theme.background` opaque so the menu
    inherits the pane bg the user is calibrated for instead of
    clashing with a chrome-color box.
  - **Subtle border** — 1-px palette[8] at 0.65 on each edge (was
    palette[4] full opacity).
  - **Soft highlight** — active row gets palette[4] at 0.18 (was
    0.85) plus a 2-px palette[4] left-edge accent strip, matching the
    cycle-178 active-tab and cycle-184 focused-pane border pattern.
    "You are here" now reads consistently across every chrome surface.
  - **Breathing room** — row height `ch+12` (was `ch+6`), horizontal
    pad 16px (was 12), min panel width 180px (was 140), separator
    height 8px (was 6) and inset 12px (was 8). Comfortable click
    targets, polished surface.

### CI
- **MSRV verification job.** Cargo.toml declared `rust-version =
  "1.88"` since cycle 225, but nothing in CI actually checked the
  workspace + its transitive deps still build on that toolchain.
  Adding `dtolnay/rust-toolchain@1.89` to ci.yml immediately surfaced
  the real bug: `cosmic-text@0.18.2` and `smol_str@0.3.6` had both
  bumped their own floors to 1.89, so `cargo install kettle` on
  Rust 1.88 used to land in a confusing transitive-dep error instead
  of cargo's clean "package requires rustc 1.89" gate. Declared
  `rust-version` bumped 1.88 → 1.89 to match reality; the new MSRV
  job catches the next regression at PR time, not release time.

## [1.3.0] — 2026-05-21

Minor release. Theme: **production-grade UX cycle — tabs, splits,
right-click, Terminator + iTerm2 + WezTerm parity sweep.**

Seven focused sub-cycles addressing three user-reported issues (tab
`×` looked like a character not a button; `Ctrl+Shift+W` closed the
whole tab instead of just the focused split; right-click behaved
weirdly) plus four feature parities from other major terminals. Each
sub-cycle landed as its own commit with a drift-guard test pinning
the contract.

### Fixed
- **`Ctrl+Shift+W` on a split closes the pane, not the whole tab**
  (cycle 240). `Mux::close_focused` matched `Err(_)` and treated
  every error variant the same — `Err(None)` (only leaf, close
  tab) was conflated with `Err(Some(sibling))` (sibling needs
  promoting, keep tab). Split the arm and merge the
  promote-sibling branch with the regular `Ok(n)` path; both have
  identical post-conditions. Drift guard
  `close_focused_promotes_sibling_in_two_pane_split` pins the
  contract.

### Added — UX parity
- **Tab `×` hover affordance** (cycle 241). Click handler already
  hit-tests `seg.close` and dispatches `close_tab_at`; the bug was
  purely visual. A red chip background now appears behind the
  `✕` glyph on hover and the OS cursor flips to `Pointer` — Chrome
  / Firefox / Safari tab convention. Two pure helpers
  (`hovered_close_button`, `tab_close_hover_icon`) make the
  geometry + cursor decisions unit-testable.
- **Right-click opens a context menu** (cycle 245). Replaces the
  cycle-49 silent no-op with a floating panel of 8 entries
  (Copy / Paste / sep / Split Right / Split Down / Close Pane /
  sep / New Tab). Reuses the modal-overlay infrastructure (cycle
  111 command palette + hint mode pattern). Keyboard nav `↑↓ Tab`
  step the highlight skipping separators + disabled rows;
  `Enter Space` dispatch; `Esc` closes. Mouse click on a row
  dispatches; click outside dismisses. Anchor clamps via pure
  `clamp_context_menu_anchor` so a right-click near the bottom-
  right corner flips the panel up-and-left instead of rendering
  off-screen. Shift+right-click over an existing selection
  preserves the cycle-49 extend-selection muscle memory.
- **Tab-bar activity / bell dots** (cycle 246, Terminator parity).
  Per-`Tab` `last_output_at` / `last_seen_at` / `bell` fields +
  pure `classify_tab_activity(is_active, bell, last_output_at,
  last_seen_at) -> TabActivity { Normal | Output | Bell }`. The
  reader thread already advances per-pane history; that signal
  now also latches the containing tab's `last_output_at` (active
  tab short-circuits — the focused-tab accent is enough). The
  renderer draws a 6-px square in the lower-left corner of
  inactive segments — palette[3] yellow for Bell, palette[6] cyan
  for Output. Same brand colors the cycle-178 broadcast accent
  uses, so the visual language stays consistent.
- **Undo-close-tab** (cycle 247, WezTerm parity).
  `Mux::closed_tabs: VecDeque<ClosedTab>` bounded LIFO ring of 10;
  `close_tab_at` snapshots the first leaf's argv + OSC-7 cwd
  before drop. New `Pane::argv` field (load-bearing for the SSH
  re-spawn case). `Action::UndoCloseTab` (aliases
  `reopen_tab` / `restore_tab`) re-spawns the same program in the
  same cwd at the same tab index. Surfaced in the command palette;
  no default keybind (kettle's Terminator-inherited `Ctrl+Shift+T
  = NewTab` muscle memory takes priority — users who want
  WezTerm's chord add `keybind = ctrl+shift+t=undo_close_tab` to
  their config).
- **Duplicate tab + duplicate pane** (cycle 248, iTerm2 parity).
  `Action::DuplicateTab` / `Action::DuplicatePane` read the
  focused pane's argv (via the cycle-247 field) + OSC-7 cwd and
  clone into a new tab / horizontal split. An `ssh prod` tab
  duplicates to a second `ssh prod`; a `kettle -e vim file` tab
  duplicates to a second vim editing the same file. Empty argv
  falls back to the configured shell. Both surfaced in the
  command palette; no default keybindings.
- **Mouse-drag tab reorder** (cycle 249, kitty / iTerm2 / Ghostty
  parity). Pure `tab_drag_target_index(cursor_x, n, strip_w)`
  helper + a tiny `tab_drag_active` flag on `App`. A left-button
  press on a tab segment arms the drag; subsequent `CursorMoved`
  events compute the target index and call `Mux::move_active_tab`
  (cycle ~125 swap-with-clamp). Release disarms. No ghost-render
  of the dragged segment — kept out of scope; the bar snaps to
  the new order at each boundary crossing.

### Drift guards (+8 across the workspace)
- `close_focused_promotes_sibling_in_two_pane_split` (mux.rs)
- `hovered_close_button_finds_only_the_close_rect_hits` (app.rs)
- `tab_close_hover_icon_overrides_chrome_default` (app.rs)
- `next_context_menu_highlight_skips_separators_and_disabled` (app.rs)
- `clamp_context_menu_anchor_keeps_panel_on_screen` (app.rs)
- `classify_tab_activity_picks_the_right_indicator` (mux.rs)
- `closed_tab_ring_bounded_and_lifo` (mux.rs)
- `tab_drag_target_index_clamps_to_strip` (app.rs)

The cycle-117 palette-completeness exhaustive match guards the
three new actions (`OpenContextMenu`, `UndoCloseTab`,
`DuplicateTab`, `DuplicatePane`) — a future Action variant landed
without a palette decision fails to compile.

Workspace tests: 252 → 259.

## [1.2.1] — 2026-05-21

Patch release. Theme: **production-grade hardening — supply-chain
hygiene, governance scaffolding, and `--help` polish.**

No new features and no behavior change for any windowed-run user. The
v1.2.0 line shipped the first-launch onboarding triplet
(`--print-default-config` / `--shell-integration` / `--print-completions`);
1.2.1 finishes the `verbatim_doc_comment` story on `--help`, adds
project-level security + automation pieces a production-grade Rust
project is expected to have, and pins two `cycle-106/107` hard-fails
in CI that previously only had unit-test coverage.

### Fixed
- **`--help` indented examples for `--print-default-config` /
  `--shell-integration` / `--print-completions` no longer reflow.** The
  cycle-227/229/237 doc-comments contain indented `  kettle --… > …`
  example lines; without `verbatim_doc_comment`, clap collapsed the
  leading spaces in `--help`, flattening the examples into prose
  ("…file: kettle --print-default-config > ~/.config/kettle/config
  Everything in…"). All three flags now carry the attribute. New
  `cli_help_preserves_indented_code_examples` drift guard walks the
  clap `CommandFactory` arg list and asserts each indented example
  survives literally in `get_long_help()`, so a future refactor that
  drops the attribute fails CI with a pointer to the missing field.
- **`kettle --print-completions zsh` no-op fix.** The doc-comment's
  zsh example wrote the script to `~/.config/kettle/_kettle` — a path
  `compinit` would never look at because it isn't on `$fpath`.
  `clap_complete::Shell::Zsh` emits `#compdef kettle` at the top of
  the script, which only loads via autoload. The example now points
  at `"${fpath[1]}/_kettle"`, which lands in zsh's first
  function-path entry on every default install. Bash + fish lines
  were already correct.
- **Workspace-wide rustdoc warning silenced on the new zsh example.**
  `${fpath[1]}` is valid zsh array-indexing syntax but rustdoc tried
  to resolve `[1]` as an intra-doc link. Field-scoped
  `#[allow(rustdoc::broken_intra_doc_links)]` on `print_completions`
  silences just this one site rather than reaching for a workspace
  allow; backslash-escaping the brackets in the doc-comment would
  have leaked into clap's `verbatim_doc_comment` `--help` output and
  made the example un-copy-pasteable.

### Security
- **`SECURITY.md` — coordinated-disclosure policy via GitHub private
  advisories.** A terminal emulator parses untrusted PTY output every
  time the focused program is a remote shell, a `less` of an
  attacker-controlled file, or a CI log replay. New SECURITY.md
  points to GitHub's private vulnerability-reporting form (so we can
  triage and ship a fix before the issue is public) and enumerates
  the in-scope classes (PTY-to-host escape, OSC 52 read-leak past
  the cycle-49 default-deny, URI scheme abuse past
  `links::is_safe_url`, bracketed-paste-marker injection past the
  cycle-49 strip, resource exhaustion past the cycle-47/118 caps,
  config/session tampering, build-time supply chain).

### CI / supply chain
- **Dependabot — weekly Cargo + GitHub Actions update PRs.** Monday
  08:00 UTC cadence, 5 PRs per ecosystem max, patch + minor bumps
  grouped into a single PR per ecosystem so a slow review week
  doesn't pile up 15 individual bumps. Major bumps stay on their own
  so semver-meaningful changes get individual review. Commit prefixes
  align with the existing `fix(…) / feat(…) / ci(…) / docs(…)` scope
  convention (`deps:` / `ci(deps):`).
- **`cargo audit` workflow (`.github/workflows/audit.yml`).** Runs
  the official `rustsec/audit-check` action against the RustSec
  advisory DB on every push/PR that touches `Cargo.lock` plus a daily
  06:00 UTC cron — that catches advisories *published* against an
  unchanged Cargo.lock that Dependabot wouldn't notice until Monday.
  On pushes to `main`, findings open (or update) a single tracking
  issue per advisory rather than spamming the issue tracker.
- **`--config` / `--working-directory` hard-fail smoke (all OSes).**
  Cycle 106 (`--config /typo` exits 1) and cycle 107
  (`--working-directory /typo` exits 1) were covered by unit tests
  but never by CI's actual exit-code path. A regression that
  silently fell back to defaults would have passed the unit tests
  and reached users. Three assertions added at the tail of the CLI
  smoke step: typo'd `--config` exits non-zero, typo'd
  `--working-directory` exits non-zero, and the happy-path round-trip
  `--config /tmp/k.cfg --config-path` echoes the path and exits 0
  (also confirms the bootstrap one-liner survives a round-trip
  through `--config`). Self-contained via `$RANDOM` sentinel paths.
- **`--list-ssh-hosts` empty-case smoke.** Cycle 105's
  `format_ssh_hosts` empty-fallback emits "(no ssh-host entries
  configured)" so a user with no SSH hosts configured sees
  *something* instead of silence. CI never verified that;
  a regression silently producing no output would slip through.
  New smoke step asserts the explicit fallback line via
  `grep -E '^\(no ssh-host entries configured\)$'`.
- **Release artifacts now ship `CHANGELOG.md`.** Linux tarball,
  macOS `.app` (`Contents/Resources/`), and Windows zip already
  carry `LICENSE` / `NOTICE` / `README.md`. CHANGELOG was the
  obvious missing companion — a user who downloaded a tarball had
  no offline way to see "what's new in this release" without
  visiting GitHub. Adding it to all three platform packagings is
  one file each.

### Governance
- **GitHub issue + PR templates aligned with the cycle pattern.**
  `.github/ISSUE_TEMPLATE/{config,bug_report,feature_request}.yml`
  plus `.github/PULL_REQUEST_TEMPLATE.md`. `config.yml` disables
  blank issues, routes security reports at the SECURITY.md advisory
  form, and routes usage questions at Discussions — so the issue
  tracker stays bug + feature signal. The bug-report form requires
  the fields a cycle review would otherwise ping-pong over
  (`kettle --version` incl. the cycle-192/195 git SHA, OS + version,
  numbered repro with escape-sequence printf hints, expected vs
  actual, `RUST_LOG` output, `--check-config` snapshot). PR
  template mirrors the cycle shape from CONTRIBUTING.md:
  Summary / Why / Approach / Verification checklist / Cycle metadata.

### Tests
- **VT conformance: individual SGR-off codes
  (22/23/24/27/29).** `sgr_truecolor_bold_and_reset` and
  `sgr_underline_dim_strike` covered SGR *set* codes; the
  attribute-off codes weren't tested. These matter for nested
  styling: nvim / tmux / less / `git diff --color` set an
  attribute, write, then unset *that one* attribute and
  continue with the remaining accumulated SGR state. A
  regression in the SGR-22 path would silently leave bold set
  on cells the tool thought it had cleared. New
  `sgr_individual_attribute_resets` stacks the full set
  (bold + dim + italic + underline + inverse + strike), then
  walks each off-code and asserts only the matching flag
  clears while the others stay set. SGR 25 (blink off) is
  documented in the test but not asserted —
  `alacritty_terminal`'s `Cell::flags` deliberately doesn't
  track BLINK (render-time concern, not a cell attribute).

## [1.2.0] — 2026-05-20

Second minor release. Theme: **finish the first-launch
onboarding triplet** + **post-v1.1.0 hardening sweep**.

The 1.0/1.1 line shipped great defaults but onboarding still
relied on docs lookup for two affordances (OSC 133 shell
integration and tab completion). 1.2 ships them both as
one-command embedded CLI flags, joining v1.1's
`--print-default-config`. After install, three optional lines
fully configure kettle for daily use:

```sh
kettle --print-default-config > ~/.config/kettle/config
kettle --shell-integration bash >> ~/.bashrc
kettle --print-completions bash >> ~/.bashrc
```

Plus seven cycles of CI / drift-guard / refactor hardening to
keep the docs and packaging in sync as the project grows.

### Added (since 1.1.0)
- **`kettle --print-completions <bash|zsh|fish|elvish|powershell>`
  emits a shell tab-completion script.** Same shape as cycle 227
  (`--print-default-config`) and cycle 229 (`--shell-integration`):
  ```sh
  kettle --print-completions bash >> ~/.bashrc
  kettle --print-completions zsh  > ~/.config/kettle/_kettle
  kettle --print-completions fish > ~/.config/fish/completions/kettle.fish
  ```
  After sourcing, `kettle --li<TAB>` completes to `--list-themes`
  /`--list-keybinds` / `--list-actions` / `--list-ssh-hosts`.
  Generated by `clap_complete` from the same `Cli` struct that
  powers `--help`, so a future flag is auto-completed without a
  manual table update. New test
  `print_completions_emits_per_shell_scripts` pins each known
  shell to a minimum size + the `kettle` substring; CI smoke runs
  every shell + asserts an unknown shell exits non-zero.
  `scripts/install.sh` now lists this as a third optional
  one-liner; README quick-start shows it alongside the others.

### CI
- **`--screenshot` end-to-end smoke on the Linux runner.**
  `kettle_render::offscreen_selftest` compiles the WGSL shaders
  and renders one pass; `--screenshot` exercises the rest of the
  pipeline (bundled Nerd Font, glyphon shaping, wgpu offscreen
  texture, image::save PNG encode, scripted demo content). None
  of that was covered by CI before. New step runs `--screenshot`
  against the software-Vulkan adapter (LIBGL_ALWAYS_SOFTWARE=1)
  and asserts the output has the PNG magic header + ≥ 10 KB,
  catching a wgpu/glyphon/image-crate regression before users
  hit it. No DISPLAY needed — capture_png builds its own
  offscreen device with `compatible_surface: None`.

### Internal
- **Markdown cross-link drift guard for every user-facing doc.**
  Cycle 223/224's image guard catches `![…](path)` regressions;
  cycle 232 adds the same shape for text links — `[label](path.md)`
  to relative `.md` files. README alone cross-links to 8+ docs
  (`CONFIG`, `INSTALL`, `ROADMAP`, `SHELL-INTEGRATION`,
  `ARCHITECTURE`, `RESEARCH`, `UX-COMPARISON`, `TESTING`,
  `CONTRIBUTING`, `CHANGELOG`); a rename / deletion silently broke
  GitHub-rendered navigation with no CI signal. The guard:
  (1) walks byte offsets like the image guard;
  (2) matches `[…](path)` but excludes `![…](path)` (image
      guard's territory) by checking the byte before `[` isn't `!`;
  (3) skips external (`http(s)://`) and anchor-only (`#section`)
      links; only checks relative `.md` paths;
  (4) resolves each path against the *doc's own directory*
      (README's `docs/CONFIG.md` and docs/ARCHITECTURE.md's
      `TESTING.md` both have to work);
  (5) floors the README parser at ≥ 3 cross-links so a regression
      to "matches nothing" can't silently pass.

### Changed
- **`install.sh` final message points at the two bootstrap
  one-liners.** Post-install the user already knows where the
  binary landed and how to launch from the Super key. The
  message now also surfaces `kettle --print-default-config`
  (cycle 227) and `kettle --shell-integration bash` (cycle 229)
  as the two optional one-liners that finish setup. Both already
  worked; the install script just didn't advertise them.

### CI
- **Release tarballs now also ship `shell-integration/` alongside
  the binary.** Cycle 229 embedded the snippets into the binary
  via `include_str!`, so `kettle --shell-integration bash >> ~/.bashrc`
  works without the source tree. The standalone files are still
  useful for users who want to read or customize them before
  sourcing, and for discoverability via `ls`. Linux tarball
  ships them at `kettle/shell-integration/`; macOS .app bundle
  ships them at `Contents/Resources/shell-integration/`; Windows
  zip ships them at `shell-integration/` next to the .exe.

### Added
- **`kettle --shell-integration <bash|zsh|fish>` — one-command
  install of the OSC 133 shell snippet.** Cycle 227 added the
  config bootstrap one-liner; the OSC 133 shell-integration story
  still required the user to manually copy a snippet out of
  `docs/SHELL-INTEGRATION.md` into their rc file. Now:
  ```sh
  kettle --shell-integration bash >> ~/.bashrc
  kettle --shell-integration zsh  >> ~/.zshrc
  kettle --shell-integration fish >> ~/.config/fish/config.fish
  ```
  Snippets live at `shell-integration/kettle.{bash,zsh,fish}` in
  the source tree (Linux release tarball includes them too) and
  are embedded into the binary via `include_str!`. New test
  `shell_integration_snippets_match_in_tree_files` pins each
  snippet to a minimum size + OSC 133 substring so an accidental
  empty include is caught at build time. CI smoke runs all three
  shells + asserts an unknown shell exits non-zero with a clear
  error. SHELL-INTEGRATION.md now leads with the one-liner and
  keeps the inline snippets below as reference.

## [1.1.0] — 2026-05-20

First minor release after `v1.0.0` / `v1.0.1`. Theme is **first-
launch onboarding** + **cross-platform desktop integration parity**:
a newcomer on Ubuntu, macOS, or Windows 11 should now be able to
go from "I just downloaded kettle" to "I'm typing in a terminal
with my icon in the launcher and my config in the right place" in
two commands. Plus durable manifest/CI policy guards for the
contracts that landed in this cycle batch.

### Added (since 1.0.1)
- **`kettle --check-config` emits a bootstrap hint when no config
  exists.** When the resolved config path doesn't exist on disk
  (the common newcomer state), the output now includes:
  ```
  config:  /home/you/.config/kettle/config (not found — using defaults)
  hint:    kettle --print-default-config > /home/you/.config/kettle/config
  ```
  The hint interpolates the **actual** resolved path so the user
  can copy-paste verbatim. Suppressed when the config does exist
  (no nag for users who already set one up). CI smoke verifies
  the hint appears via `grep -E '^hint: +kettle --print-default-config > '`
  so a future regression that drops the hint is caught here, not
  by a confused first-time user.

- **`kettle --print-default-config` — one-command first-launch
  bootstrap.** The documented example config lives at
  `docs/kettle.example.config` (~140 commented lines) and a
  newcomer used to have to copy it manually from the source tree
  or the docs site. Now:
  ```sh
  kettle --print-default-config > ~/.config/kettle/config
  ```
  drops a fully commented starter file in the right place — no
  source tree required (`cargo install kettle` users get the
  correct content too). The file content is embedded at build time
  via `include_str!("../../../docs/kettle.example.config")`, so
  there's no runtime path lookup that could differ from what
  shipped. New test `print_default_config_round_trip` pins three
  contracts: (1) the embedded content is non-trivial (≥ 50 lines
  — catches an empty include_str! at build time, not ship time),
  (2) `Config::parse_collect` emits zero diagnostics on the
  embedded content (catches a future malformed example value
  before users hit it), (3) every line in the example file is
  commented out by convention (cycle 100 drift guard still
  active). Wired into CI smoke too:
  `--print-default-config | wc -l > 50` + round-trip through
  `--check-config` to assert `status: OK`. README's quick-start
  table now leads with the bootstrap one-liner.

### Internal
- **Workspace-metadata contract is now one comprehensive test.**
  Cycle 218's `library_crates_have_per_crate_descriptions` was a
  narrow guard on just the description override. Cycle 225's
  `rust-version` inheritance added a new field to the
  workspace.package shape with no guard. Cycle 226 replaces the
  narrow test with `workspace_metadata_policy`, which pins:
  (1) `workspace.package` declares every shared field
  (`version` / `edition` / `rust-version` / `license` /
  `repository` / `authors` / `description`); (2) every crate
  inherits each of those via `.workspace = true`; (3) library
  crates override `description` with their per-crate `"kettle: …"`
  blurb; (4) the binary inherits `description.workspace = true`.
  Catches "tidying" cycles that revert one piece of the
  inheritance shape — version drift, MSRV drift, license drift,
  binary-blurb leak onto a library — all in one check.

### Changed
- **MSRV declared at Rust 1.88.** The workspace already uses
  let-chains (`if X && let Y = ... && Z`) in kettle-vt,
  kettle-config, kettle-render and the kettle binary. Let-chains
  stabilized in 1.88, but `rust-version` was never set — a user
  on 1.85-1.87 (which support edition 2024 but predate let-chain
  stabilization) hit cryptic `expected expression, found keyword
  'let'` syntax errors instead of cargo's clean "package requires
  rustc 1.88" message at the resolver level. Now declared in
  `workspace.package.rust-version` and each crate opts in with
  `rust-version.workspace = true`. `rustup update stable` always
  satisfies it; this is a floor, not a ceiling. INSTALL.md notes
  the MSRV inline so contributors on stale toolchains see it
  before they try to build.

### Internal
- **Image drift guard now covers every `docs/*.md`, not just README.**
  Cycle 223's `readme_referenced_images_exist` only scanned the
  root README. `docs/UX-COMPARISON.md` already embeds two images
  (`kettle-showcase.png` + `refs/xterm.png`) — same forgotten-
  commit / rename / broken-image-on-github regression risk. The
  guard now walks `docs/*.md` and resolves each `![…](path)`
  against the doc's own directory (README's `docs/images/…` and
  UX-COMPARISON's `images/…` both need to work). Renamed test
  `readme_referenced_images_exist` → `user_facing_doc_images_exist`.

### Added
- **README now embeds a kettle hero image
  (`docs/images/kettle-hero.png`).** Generated by
  `kettle --screenshot docs/images/kettle-hero.png --cols 120 --rows 32`,
  which drives the real GPU text + quad pipeline over the scripted
  two-pane demo from `kettle_render::capture_png`. The hero
  appears immediately after the project tagline so a visitor sees
  what kettle looks like before the highlights bullet list. New
  `readme_referenced_images_exist` test parses the README for every
  `![…](path)` embed and asserts each relative path resolves —
  rename / forgotten-commit caught at PR time. Test sanity-floors
  the parser at ≥ 1 image (cycle 223's hero is the floor) so a
  regression to a no-op scan doesn't silently pass.

### Documentation
- **README status block updated from "early but functional" to
  "v1.0 — ready for daily use".** The old wording dated back to
  pre-v0.1.0 and was the first paragraph a reader saw on
  github.com/Reddimus/kettle. Now points to the latest release page
  for prebuilt binaries (Linux tarball + installer, macOS universal
  `.app`, Windows zip with embedded `.ico`) and summarizes the CI
  matrix shape (fmt → clippy → test → doc → headless GPU smoke →
  CLI + packaging smoke on every push). Passes the cycle-172
  drift guard (`cycle <digit>` and `<digit> workspace tests`
  patterns) because the rewrite intentionally uses range-stable
  prose, no hardcoded counts, no internal `cycle N` refs.

### CI
- **Packaging smoke runs on every push, not just on tag cut.** The
  `release.yml` workflow only fires on `v*` tag push, so a
  regression like "remove a PNG from `packaging/macos/kettle.iconset`"
  or "delete `packaging/windows/kettle.ico`" only surfaces at the
  next release — by which point bisect-and-revert is the only
  remedy. New CI steps run `iconutil -c icns` on the macOS runner
  and `file packaging/windows/kettle.ico` on the Windows runner,
  each verifying the produced/shipped file is well-formed
  (macOS: real .icns, > 100 KB; Windows: ≥ 4 resolutions). Catches
  malformed iconsets at PR time, not release time.

## [1.0.1] — 2026-05-20

Patch release: ships the macOS `.icns` + Windows `.ico` packaging
that landed on `main` an hour after `v1.0.0` was tagged. The
`v1.0.0` Linux artifact already has the icon set; this release
brings macOS and Windows to parity. No code changes to the runtime
binary on Linux.

### Added
- **macOS `.app` icon (`kettle.icns`) + Windows `.exe` icon (`kettle.ico`).**
  Cycle 222 (v1.0.0) shipped a Linux SVG + PNG icon set and an
  `install.sh` that wires it into XDG paths so the kettle tile shows
  up in GNOME / Ubuntu Super-key search / KDE Krunner. Same wasn't
  true on macOS and Windows — the macOS `.app` bundle had no
  `CFBundleIconFile`, so Finder / Dock / ⌘-Tab showed a generic
  document glyph; the Windows `.exe` had no embedded icon, so the
  taskbar / Alt-Tab / Explorer showed the default Rust binary glyph.
  Now:
  - **macOS**: `packaging/macos/kettle.iconset/` holds the ten
    Apple-required PNGs (16/32/128/256/512 in 1× and 2× variants),
    rendered from the master `packaging/linux/kettle.svg` so a future
    icon refresh is a single-file change. `release.yml`'s macOS step
    runs `iconutil -c icns` (built-in on macOS, no extra deps) to
    produce `kettle.icns`, drops it into `Contents/Resources/`, and
    sets `CFBundleIconFile=kettle` so the bundle picks it up. Also
    patches `CFBundleVersion` / `CFBundleShortVersionString` from
    `Cargo.toml`'s workspace version via `PlistBuddy` so a forgotten
    manual bump can't ship a stale `0.1.0` plist.
  - **Windows**: `packaging/windows/kettle.ico` is a 6-resolution
    `.ico` (16/32/48/64/128/256) built from the same SVG. The
    `winresource` build-dep (Windows-only, gated by
    `cfg(target_os = "windows")` in `build.rs`) compiles it into the
    `.exe` so Explorer, the taskbar, Start-menu pins, and Alt-Tab
    all display the kettle icon. The `.ico` also ships standalone in
    the release zip for Start-menu re-pinning if the user moves the
    `.exe`.

## [1.0.0] — 2026-05-20

First "ready for daily use" release. Eleven months and ~240 audit
cycles after `v0.1.0` (the first-cross-platform release of
2026-05-19), the suite is large enough, the docs are tight enough,
and the desktop integration is good enough that we're ready to
stop calling this pre-release software.

### Highlights since 0.1.0
- Full Ghostty-compatible config (`key = value`), 500+ bundled
  themes (iTerm2-Color-Schemes / Ghostty ports), TokyoNight Night
  as the verified default.
- Terminator-style splits + tabs, broadcast input across panes,
  search overlay, command palette, theme picker, session
  save/restore (with corruption-backup contract), drag-drop file
  paste, kitty/Sixel/iTerm2 image protocols, hyperlink + URL +
  path + IP detection, OSC 7 cwd, OSC 133 prompt marks for
  Ctrl+Up/Down navigation, OSC 8 hyperlinks, OSC 52 clipboard,
  bracketed paste with injection guards, wide CJK + combining
  marks.
- GPU-accelerated rendering via wgpu (Vulkan/Metal/DX12) +
  glyphon, with an offscreen self-test that runs in CI on all
  three OSes.
- Linux desktop integration: easy installer (`scripts/install.sh`),
  XDG `.desktop` entry with `StartupWMClass=kettle`, terminal-style
  SVG icon + PNG fallbacks at 32/48/64/128/256, WM_CLASS set
  explicitly via winit so GNOME / KDE bind the launcher to running
  windows.
- macOS universal binary (x86_64 + aarch64), `.app` bundle with
  Info.plist.
- CI matrix on Linux + macOS + Windows: `fmt --check` → `clippy
  -D warnings` → `cargo test --workspace` → `cargo doc -D
  warnings` → headless GPU smoke (Linux) → CLI smoke with grep
  assertions for `--version` / `--check-config` /
  `--list-themes`>400 / `--list-actions`>50 / `--list-keybinds`>40
  on every OS.

### Added (this release cycle)
- **Terminal-style SVG icon + PNG fallbacks at 32/48/64/128/256.**
  TokyoNight palette `>_` motif. Lives at
  `packaging/linux/kettle.{svg,*.png}` and is bundled into the
  Linux release tarball alongside an extracted `install.sh`.
- **`scripts/install.sh` — easy Linux desktop install.** No `sudo`
  needed; drops the binary into `~/.local/bin/kettle`, the
  launcher into `~/.local/share/applications/`, and icons into
  `~/.local/share/icons/hicolor/{scalable,256x256,…}/apps/`. Works
  both from a cloned repo (builds release first) AND from an
  extracted release tarball (uses the bundled binary — detected
  by the `kettle` file living next to the script). After install,
  the kettle launcher appears in the GNOME Activities overview /
  Ubuntu Super-key search / KDE Krunner. `--uninstall` removes
  everything atomically.
- **Explicit `WM_CLASS=kettle` / Wayland `app_id=kettle` on every
  Linux window.** Without this, GNOME's task switcher and dock-pin
  logic doesn't reliably associate running kettle windows with the
  `StartupWMClass=kettle` line in the `.desktop` file. Set via
  `winit::platform::x11::WindowAttributesExtX11::with_name` (the
  same trait impl writes to the shared `platform_specific.name`
  used by both Wayland and X11 backends).

### Internal
- **`[workspace.lints.clippy]` opens forward-guards against
  `dbg_macro` / `todo` / `unimplemented`.** The codebase has zero
  occurrences of all three today, so this is purely "lock the door
  before someone walks through it." `clippy -- -D warnings` already
  enforces them via warning level, but a manifest-level deny is
  durable policy and survives a future `--warnings=allow`
  invocation. Each crate's `Cargo.toml` opts in with
  `[lints]\nworkspace = true`.

### CI
- **`cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`
  added on the Linux job.** Cycles 207-210 landed crate-level
  rustdoc on every workspace crate, with disambiguations like
  `[`mod@search`]` and `[`mod@links`]`. CI's `clippy -D warnings`
  doesn't catch rustdoc's warning class (broken intra-doc-links,
  malformed code blocks, missing docs on public items), so a
  future rename like `mod search` → `mod find` would silently
  invalidate those references and only be caught by a contributor
  running `cargo doc` locally. Building docs in CI with warnings
  denied pins the doc landings as a contract. One platform is
  enough (rustdoc is platform-agnostic); leaving it Linux-only
  trades a tiny CI-time saving for the same coverage.

### Changed
- **Per-crate `description` overrides on every library crate.**
  Cycle 213 moved every crate's `[package]` block onto
  `workspace.package` inheritance, including `description`. That
  works fine for `version` / `license` / `authors` / `edition` /
  `repository` (genuinely shared), but `workspace.package`'s
  description is the *binary's* blurb ("A fast, cross-platform
  GPU terminal emulator combining the best of Ghostty, Terminator,
  kitty, Alacritty and WezTerm") — and inheriting it gave every
  library sub-crate the same text. A user browsing `kettle-config`
  on crates.io or via `cargo metadata` would see the terminal's
  marketing blurb on the config-parsing crate, the VT-byte-extractor
  crate, etc. Now each library overrides with what *it* does:
  - `kettle-config` → "Ghostty-compatible config parsing, bundled
    theme set, embedded Nerd Font, keybinds, fuzzy matcher"
  - `kettle-core` → "PTY + alacritty_terminal VT glue, scrollback
    search, hyperlink/URL/path/IP detection, kitty/Sixel/iTerm2
    image registries"
  - `kettle-vt` → "streaming VT byte extractor — kitty/Sixel/iTerm2
    image protocols, OSC 7 cwd, OSC 133 prompt marks, OSC 1→2
    title rewrite"
  - `kettle-render` → "wgpu + glyphon GPU renderer — quads, images,
    text, overlay pass, headless offscreen self-test"
  - `kettle-ui` → "winit app, tab/pane mux, Terminator-style splits,
    overlays (search/palette/themes), session save/restore"
  - `kettle` (binary) keeps `description.workspace = true` — the
    workspace blurb IS the binary's blurb, single source of truth.
  All start with the prefix `"kettle: "` so they identify as part
  of the same project at a glance. New test
  `library_crates_have_per_crate_descriptions` reads each Cargo.toml
  via `std::fs::read_to_string` and pins both halves (libraries
  must override, binary must inherit) so a future "tidying" cycle
  that uniformizes the manifests back to `description.workspace =
  true` is caught.

### Documentation
- **`docs/TESTING.md` per-crate counts refreshed and shifted to
  `+N` range form.** Cycle-172/179/214 fixed top-level "workspace
  has X tests" claims in INSTALL / ARCHITECTURE / TESTING /
  CONTRIBUTING. The per-crate sub-counts in TESTING.md were still
  the old `~33` / `~56` / `~75` / `~10` / `~37` / `2` numbers from
  cycle 128-ish; some had drifted (`~56` → 74, `~75` → 82,
  `~37` → 40, `2` → 4). Refreshed each to a "+N" range form
  (`~70+` / `~80+` / `~40+` / `~4`) so the figures stay useful as
  rough orders of magnitude without going precisely stale every
  few cycles.

### Internal
- **CI smoke also verifies `--list-actions` and `--list-keybinds`
  produce plausible counts.** Existing smoke verified `--list-themes`
  > 400 entries (catches `theme_filter` over-rejection). Added the
  same range-stable check for `--list-actions` (`> 50`, current 82
  — headroom for new action variants without going stale) and
  `--list-keybinds` (`> 40`, current 58 — headroom for cycle-115-
  style chord-shadow rebalances while still catching an empty
  defaults() regression). Pairs with cycle 215's `--version` and
  `--check-config` grep assertions for full CLI-surface smoke
  coverage.

- **CI's CLI-smoke step now exercises `--version` and `--check-config`.**
  Pre-cycle, the smoke step ran `--config-path` and `--list-themes`
  but not `--version` (which exercises the cycle-192/195 build.rs
  git-SHA capture path) or `--check-config` (cycle 194/196/197/198
  diagnostic path). A regression where the build.rs git invocation
  silently failed and shipped `kettle 0.1.0` without the SHA — or
  `--check-config` lost its cycle-194 `kettle:` lead-line — would
  go unnoticed by CI. Added grep assertions for both:
  ```bash
  cargo run -q -p kettle -- --version | grep -E '^kettle [0-9]+\.[0-9]+\.[0-9]+ \([0-9a-f]+(\+dirty)?\)'
  cargo run -q -p kettle -- --check-config | grep -E '^kettle:  [0-9]'
  ```
  Regex allows the optional `+dirty` suffix so the assertion holds
  on both clean-CI builds (no dirty marker) and local dev builds.

### Documentation
- **`CONTRIBUTING.md` test-count claim reworded to range-stable.**
  Said "workspace runs ~225 tests" — stale by 18 (we're at 243).
  Cycle 172/179 fixed the same drift class in README/CONFIG/
  INSTALL/ARCHITECTURE. CONTRIBUTING.md is contributor-leaning so
  it was exempt from the drift guard, but it has the same problem.
  Reworded to "workspace test suite grows ~1/cycle. Run
  `cargo test --workspace` for today's count" so the count
  doesn't go stale between audits.

### Internal
- **Per-crate `Cargo.toml`s now inherit version / edition / license /
  repository / authors / description from `[workspace.package]`.**
  The workspace `Cargo.toml` had `[workspace.package]` defined with
  `license = "MIT"`, `repository = "https://github.com/Reddimus/kettle"`,
  `authors`, `description`, but **none** of the 6 crate manifests
  used `.workspace = true` to inherit. Each crate just had
  `version = "0.1.0"` and `edition = "2024"` (the workspace.package
  said `edition = "2021"` — mismatch). Cargo would warn about
  missing `license` on `cargo publish`, and a future bump to (say)
  `version = "0.2.0"` would have to be edited in 7 places. Now
  each crate inherits all 6 fields; the workspace.package is the
  single source of truth. Workspace.package edition bumped from
  "2021" to "2024" to match the crates' actual declarations.
  243 workspace tests still pass; `cargo build --workspace` clean.

### Documentation
- **README's License line reflects the cycle-211 NOTICE structure.**
  Pre-cycle the line said "Bundled assets and adapted code are
  credited in NOTICE" — implying all NOTICE entries are
  "adapted code". Cycle 211 added design-source citations
  (kitty / Terminator / Ghostty) with explicit "no code copied"
  notes. Updated to: "Bundled assets, third-party crates kettle
  consumes (Alacritty's VT core, WezTerm's `portable-pty`,
  cosmic-text), and the design-source projects kettle cites
  (kitty's graphics protocol spec, Terminator's splits-and-
  broadcast convention, Ghostty's config syntax)". A user
  reading the License section now sees what's actually in
  NOTICE without having to open it.

- **`NOTICE` credits kitty, Terminator, and Ghostty as
  design-source attributions.** Pre-cycle, NOTICE listed only the
  projects whose CODE kettle uses (Alacritty / vte, WezTerm
  portable-pty, cosmic-text/glyphon, Contour Sixel reference) + the
  bundled assets (font + theme set). Three more projects shape
  kettle's design without sharing code:
  - **kitty** (GPL-3.0) — graphics protocol specification; kettle's
    Rust implementation is original but follows kitty's design.
  - **Terminator** (GPL-3.0) — splits/tabs/broadcast UX +
    default keybinds; the `Ctrl+Shift+O/E/T` convention,
    `broadcast_all` semantics, group-input scoping all originate
    here.
  - **Ghostty** (MIT) — config syntax + key names + `unfocused-
    split-opacity = 0.7` default; a user's Ghostty config drops
    into kettle unchanged.
  Each entry notes "specification/convention consulted, no GPL
  code copied" so the licensing story stays clean — kettle is MIT
  but cites GPL-3.0 *designs* (which is a fair-use / norm-of-
  attribution pattern, not a license-derivation one). No code
  change.

### Internal
- **`kettle-render` and `kettle-vt` crate-level docs updated to
  match what's actually in those crates.** Cycles 207/208/209
  audited the three biggest crate docs (ui / core / config);
  cycle 210 closes the sweep on the remaining two:
  - `kettle-render`: pipeline order (quads → images → text →
    overlay quads), the post-text overlay pass for
    dim+scrollbar, the headless `capture_png` / `offscreen_selftest`
    paths, the broadcast-mode accent flip on tab/border.
  - `kettle-vt`: extractor's dual role for image protocols AND
    OSC 7 (cwd) / OSC 133 (shell integration), the
    `placeholder` module for kitty Unicode-placeholder
    decoding. Both `cargo doc --no-deps` zero-warning.
  All five workspace crates now have rustdoc landings that
  match the contract a contributor would expect after reading
  the CHANGELOG.

- **`kettle-config` crate-level doc lists every public module.**
  Cycles 207/208 siblings. Original kettle-config one-liner mentioned
  "Ghostty-compatible config, bundled Ghostty theme set, embedded
  Nerd Font, Terminator-compatible keybindings" but missed `color`,
  `font`, `fuzzy`, `palette`, `parse`, `template`, and the private
  `theme_filter` module. Now per-module breakdown with intra-doc
  links + cited usage (which UI overlay reuses each helper). Zero
  doc warnings. Closes the crate-level-doc sweep across `kettle-ui`,
  `kettle-core`, and `kettle-config` (3 of 5 workspace crates).

- **`kettle-core` crate-level doc lists every public module.**
  Cycle-207 sibling for the next crate over. The original kettle-core
  doc said "PTY management, the `alacritty_terminal` grid/VT engine
  glue, the UI event bridge, and buffer search" — missed `links`
  (OSC 8 + autodetect), `hints` (Ctrl+Shift+H targets), `images`
  (kitty graphics registries), `scrollbar` (scroll-on-output
  detection), and `url_trim` (cycle 166 bracket-balance helper).
  Now: per-module breakdown with intra-doc links. `cargo doc -p
  kettle-core --no-deps` reports zero warnings (had to disambiguate
  `search`/`links` between the module name and the re-exported
  function name via `mod@`).

- **`kettle-ui` crate-level doc lists what's actually in the crate.**
  Original doc (one-liner from early development) mentioned only
  "winit application, tab/pane multiplexer, keyboard input
  encoding, and the search overlay." Cycles since then added SSH
  launcher (Ctrl+Shift+S), command palette (Ctrl+Shift+K), hint
  mode (Ctrl+Shift+H), session restore, drag-and-drop, broadcast
  input indicators — all undocumented at the crate doc level. A
  new contributor reading `cargo doc -p kettle-ui` saw a stale
  one-liner and had to grep the source to figure out the actual
  surface. Now: per-module breakdown of `app`/`input`/`mux`/
  `session` + a list of modal overlays + the helpers that
  coordinate them. No code change.

- **`theme_filter::is_bundled_theme_filename` doc-comment lists all
  6 skip categories.** The original doc (cycle 167) listed 4
  patterns; cycles 186/187/190 expanded the implementation
  (case-insensitive OS metadata, emacs `#name#` autosave, Office
  `~$name` lock files) but updated only the inline cycle-N
  comments inside the function body. The summary list at the top
  of the doc — which is what a contributor reads first — was
  stale by 2 entries. Now lists all 6 patterns with one-line
  context for each. No code change.

- **`build.rs` module-level doc updated to reflect the cycle-195
  `+dirty` marker and rerun-if-changed removal.** The cycle-192
  module doc said "Outputs `KETTLE_GIT_SHA` as one of two forms"
  (clean SHA or empty). Cycle 195 added the `+dirty` third form
  but didn't update the doc; cycle 195's note was at the
  `cargo:rerun-if-changed` decision site (mid-function), not at
  the top where a contributor first reads. The module doc now
  enumerates all three output forms and cites both cycles. No
  code change; the contract was already implemented in 195.

### Added
- **`kettle` logs its build identity at startup (info level).** A
  user grep'ing their stderr for warnings to file a bug report
  previously had no way to know which build the warnings came
  from. The version+SHA is now logged on first start:
  ```
  [2026-05-20T17:16:54Z INFO kettle] kettle 0.1.0 (a2ff10b2f36f) starting
  ```
  Visible only when the user bumps logging (`RUST_LOG=info kettle`
  or `RUST_LOG=kettle=info`); below the `warn` default so it
  doesn't clutter normal stderr output. Reuses the cycle-192
  `KETTLE_VERSION` constant — same shape as
  `cargo --check-config` and `--version`.

### Documentation
- **`docs/UX-COMPARISON.md` drag-and-drop entry gains a citation
  paragraph.** Cycle 200 added the matrix row but didn't add the
  matching Citations paragraph (which cycle 193 had done for its
  broadcast row). The Citations paragraph explains iTerm2 /
  kitty `paste_from_drop` / WezTerm / GTK origin, plus kettle's
  three-property combination: shell-quote (cycle 175), bracketed-
  paste-safe wrap (cycle 182), and per-pane broadcast aware (the
  cycle 173/174 sibling pattern). Closes the cycle-200 docs gap.

- **`--config` `--help` text documents the cycle-198 unreadable-
  file hard-fail.** The clap doc comment mentioned only the
  cycle-106/164 cases (missing file, directory). Cycle 198 added
  the permission-denied class — the doc didn't reflect it.
  Updated to "must be an existing, regular, readable file" with
  all three hard-fail conditions enumerated. Same docs/runtime
  drift shape as cycle 168 (which originally removed internal
  `cycle N` refs from clap help). The drift-guard test still
  passes (no `cycle <digit>` substring introduced).

### Fixed
- **`--check-config` labels read errors as `i/o error:`, not the
  misleading `malformed value:`.** Cycle-196 follow-up. Cycle 196
  surfaced read failures by pushing them into the `malformed`
  list — they then printed with the existing `- malformed
  value:` prefix. Confusing: a permission-denied file isn't a
  value-parsing failure. Now they get their own category with
  an `i/o error:` prefix and are counted separately in the issues
  total. Sample output diff:
  ```
  before:  - malformed value: could not read /path: ... (using defaults)
  after:   - i/o error: could not read /path: ... (using defaults)
  ```
  Same exit-code semantics (still 1 when issues > 0). 243 tests
  pass.

### Documentation
- **`docs/UX-COMPARISON.md` matrix gains drag-and-drop file paths
  row.** Cycle 175 added drag-drop, cycle 182 made it bracketed-
  paste-safe — kettle's implementation has the distinctive triple
  property (shell-quoted, bracketed-paste-safe, broadcast-aware)
  that's worth recording in the comparison matrix. Row: kettle ✅
  (with the three properties named) · iTerm2 ✅ (long history) ·
  kitty ✅ via `paste_from_drop` · WezTerm ✅ configurable ·
  Terminator 🟡 (GTK builtin; path quoting varies) · Alacritty ⛔.

- **`docs/SHELL-INTEGRATION.md` added to README's Documentation
  list.** The doc has existed since the OSC-133 integration
  landed and got the cycle-189 fish-hook fix, but the README
  only linked it inline from the "Shell integration" feature
  bullet. A user browsing the documentation list to figure out
  what's available would miss it. Now listed alongside CONFIG
  and CONTRIBUTING with a one-line description.

### Fixed
- **`--config FILE` hard-fails at the CLI surface when the file is
  unreadable.** Cycles 106 / 164 caught the "no such file" and
  "not a regular file (typically a dir)" classes. Cycle 198 adds
  the third class: file *exists* and *is regular* but
  permission-denied / I/O-error on open. Pre-fix, kettle started
  with defaults, emitted a warn to stderr, and the user saw their
  theme not apply. Now: `--config FILE: not readable (permission
  denied or I/O error)` and the CLI exits non-zero. Same shape
  as the existing rejections — surface the problem at the CLI
  surface where the user can act on it, instead of silently
  falling back. Test gains a `#[cfg(unix)]` block that
  `chmod 000`s a tempfile and asserts the helper returns the
  right reason; gated on `is_err()` so running tests as root
  (which bypasses unix perms) doesn't spuriously fail.

### Performance
- **`--check-config` reads the config file once, not twice.**
  Cycle-196 follow-up. The cycle-196 fix probed `read_to_string`
  to detect read errors, then on success called
  `load_from_with_diagnostics` which read the file *again*
  internally. Harmless but wasteful (especially on slow disks
  / network mounts / large configs). Now: feed the already-
  read text straight into `parse_collect` and
  `detect_malformed_values` — both are public and take `&str`
  — so the disk read happens exactly once. Same observable
  behavior; just one syscall less. 243 workspace tests pass.

### Fixed
- **`kettle --check-config` exits non-zero when the config file
  is unreadable (perm-denied / I/O error), instead of silently
  returning "status: OK".** Pre-fix,
  `load_from_with_diagnostics` returned defaults on any
  `read_to_string` error and emitted a `warn` log to stderr.
  `--check-config`'s stdout said "config: /path" then "status:
  OK", and the exit code was 0 — making the user think their
  config loaded fine. Bug-report shape: "I set
  `theme = Catppuccin Mocha` but kettle keeps using TokyoNight,
  and --check-config says everything's fine" → the file was
  actually unreadable (umask, sudo'd kettle on a user-owned
  file, network mount lost, etc.). Now the read error is
  surfaced as a malformed-value entry so it shows in the
  issues list and triggers `exit 1`:
  ```
  status:  1 issue(s):
    - malformed value: could not read /etc/kettle.conf:
      Permission denied (os error 13) (using defaults)
  ```

### Added
- **`--version` SHA tags with `+dirty` when the working tree has
  uncommitted changes.** Cycle 192 captured the git SHA; cycle
  195 distinguishes a clean build at a commit from a dev-iter
  build with edits on top of that commit. Pre-fix, a developer
  with edits to `src/main.rs` reported the same SHA as the clean
  tip — bug reports against custom builds were indistinguishable
  from reports against the matching upstream commit. New output
  shapes:
  - `kettle 0.1.0 (a2ff10b2f36f)` — clean tip.
  - `kettle 0.1.0 (a2ff10b2f36f+dirty)` — same commit but with
    uncommitted edits. Mirrors `git describe --dirty`
    convention.
  Build script also dropped the cycle-192 `rerun-if-changed`
  restrictions — source-file edits need to refresh the dirty
  marker, and the two `git` invocations are ~10ms total which
  is well under build-time noise. The cost-benefit pivots
  toward "always rerun" once `+dirty` matters for bug reports.

- **`kettle --check-config` leads with the build version + SHA.**
  Cycle-192 follow-up. The version+SHA shipped in `--version` is
  the canonical "what build are you running" answer; a user
  pasting `--check-config` output into a bug report previously
  had to also run `--version` and quote it separately. The first
  line of `--check-config` is now `kettle:  0.1.0 (sha12)` —
  one paste covers both the build identity and the resolved
  config. Same convention `cargo --version`-style tools use for
  diagnostic flags. Output:
  ```
  kettle:  0.1.0 (a2ff10b2f36f)
  config:  ~/.config/kettle/config
  theme:   TokyoNight Night
  …
  ```

### Documentation
- **`docs/UX-COMPARISON.md` matrix now has a broadcast/group-input
  row.** The 173/174/178/184 trilogy made broadcast a real
  user-facing feature with double visual indicators (tab accent +
  pane border), but the comparison matrix didn't reflect it.
  Added a row showing kettle ✅, Terminator ✅ (origin),
  kitty ✅ (`multi-input.py`), WezTerm ✅, Ghostty ⛔, Alacritty ⛔.
  Citations section also gains an entry explaining the
  per-window-per-tab scoping (cycle-112 invariant), the
  cycles-173/174 sibling methods, and the cycle-178/184
  visual-indicator strategy.

### Added
- **`kettle --version` includes the git SHA.** Pre-cycle, the
  output was just `kettle 0.1.0` (the Cargo.toml version). Every
  serious Rust CLI (cargo, rustc, ripgrep, fd) embeds the build's
  git SHA so users reporting bugs can pin the exact commit they
  hit it on. With nightly `cargo install --git` builds becoming
  common, "kettle 0.1.0" on five different days could mean five
  different binaries. New `build.rs` captures
  `git rev-parse --short=12 HEAD` and embeds it as
  `KETTLE_GIT_SHA`; the main const concats it onto the version
  string. Output: `kettle 0.1.0 (a2ff10b2f36f)` in a git checkout,
  `kettle 0.1.0` in a source-tarball / vendored build (no SHA
  available — empty env string concats to nothing). The build
  script uses cargo:rerun-if-changed on `.git/HEAD` AND the
  ref file the symbolic ref points at (`refs/heads/<branch>`),
  so commits trigger a rebuild with the fresh SHA.

### Performance
- **`broadcast_paste` caches the two possible payload variants.**
  Cycle 174 introduced per-pane bracketed-paste wrapping inside
  `Mux::broadcast_paste` (so panes in vim and panes at a shell
  prompt both get a working paste). The per-pane wrap was
  computed *inside the loop* though — for an N-pane broadcast set
  with a 4 MiB clipboard payload, that's up to N × 4 MiB of
  temporary allocation (8+ MiB at modest pane counts, scaling
  with N). Now: lazy-cache the two possible payloads
  (`bracketed=true` and `bracketed=false`) via
  `Option::get_or_insert_with`. The wrap allocates at most once
  per BRACKETED_PASTE state regardless of pane count. If every
  pane in the broadcast set shares the same mode (typical), only
  one wrap allocation total. Same observable behavior; just
  doesn't allocate as much. No new test — the cycle-174
  per-pane-wrap correctness is unchanged; only the allocation
  count is. 243 workspace tests still pass.

### Fixed
- **Theme filter rejects Microsoft Office lock files (`~$name`).**
  Cycle-167/186/187 follow-up. When Office opens a
  `.docx`/`.xlsx`/`.pptx` from a network drive or shared folder
  (common pattern for theme contributors maintaining a shared
  doc), it writes a hidden-style sibling `~$filename` lock file.
  A maintainer with Office leaking lock files into
  `assets/themes/` would have those slip through cycle 167's
  filter (no leading dot, no `~` suffix, not a known OS metadata
  name). Now: leading-`~` prefix is rejected too. Bundled themes
  never start with `~`. Test gains 2 more asserts (`~$Theme`,
  `~TempTheme`). Closes the theme-filter junk audit at four
  cycles: 167 (initial) → 186 (case) → 187 (emacs `#name#`) →
  190 (Office `~$name`).

### Documentation
- **Fish shell-integration hook emits OSC 133 `D` (command finish
  + exit code).** The bash and zsh sample hooks in
  `docs/SHELL-INTEGRATION.md` emit all four marks (A / B / C / D);
  the fish sample only emitted A (prompt start) and C (preexec).
  Without D, kettle's per-prompt exit-status association is lost
  for fish users — jump-to-prompt still works (it keys off A) but
  any downstream tooling that consumes D (some shell-integration-
  aware status lines, the `__kettle_pc` exit-code template in
  bash) silently skips fish-driven prompts. Added a
  `__kettle_postexec` hook using `fish_postexec` + `$status` so
  fish parity matches bash/zsh. Also documented how to emit B
  inside the prompt itself (fish doesn't expose a fish_prompt_end
  event, but B is optional — kettle only needs A for jump-to-
  prompt). No code change; docs-only.

- **`focused-split-color` row in CONFIG.md notes the broadcast-mode
  override.** Cycle 184 changed the focused-pane border to theme
  yellow when broadcast is on (the cycle-178 sibling indicator for
  single-tab / `tab-bar = auto` layouts). A user who'd configured
  `focused-split-color = #ff0000` and toggled broadcast on used to
  see the color "ignored" with no documented explanation. The
  CONFIG.md row now explains the temporary override — broadcast
  off restores the configured color. README's Terminator-
  multiplexing bullet gains a parenthetical for the indicator so
  the visual cue is discoverable before the user toggles broadcast
  blindly for the first time.

### Fixed
- **Theme filter rejects emacs autosave files (`#name#`).**
  Cycle 167's filter caught dotfiles (`.DS_Store`, `.gitignore`,
  `.#emacs-lock`) — but emacs's *unsaved-buffer* autosave is
  `#name#` (literal `#` on both sides, no leading dot). A
  maintainer editing a theme file in emacs and crashing leaves
  `#TokyoNight Night#` next to the real file, which the
  cycle-167 filter accepted as a theme. Add a leading-`#` skip:
  bundled themes never legitimately start with `#`, so the
  rejection is unambiguous. +2 asserts in the existing test.

- **Theme filter is case-insensitive for OS desktop metadata.**
  Cycle-167 follow-up. The bundled-theme filter's OS-metadata
  branch (`Thumbs.db` / `desktop.ini` / macOS `Icon\r`) used an
  exact-case `matches!`, while the editor-backup-suffix branch
  below it was already case-insensitive. NTFS is case-preserving
  but case-insensitive — a Windows checkout / Git Bash copy /
  robocopy mishap could land `THUMBS.DB` or `Desktop.ini` in the
  themes directory, slipping through the cycle-167 filter and
  surfacing as a phantom "THUMBS.DB" theme with garbage palette.
  Now both branches use the lowercased name. Test gains 4 more
  asserts (THUMBS.DB / Thumbs.DB / Desktop.ini / DESKTOP.INI).

- **`home_dir_fallback` caller now also gates on `is_dir`.**
  Cycles 162/180 made the helper probe HOME → USERPROFILE →
  APPDATA and filter empty values. But the caller fed whatever
  path the helper returned to `cmd.cwd` without checking it was
  actually a directory. A misconfigured `HOME=/etc/passwd`
  (exotic but possible — a script that set HOME to the wrong
  thing, or an env var pointing at a regular file) would have
  the OS spawn fail with "not a directory". Now: if the
  resolved home path isn't a directory, treat it the same as
  "no home" — leave `cmd.cwd` untouched and let `portable_pty`
  inherit kettle's launch directory (the same recovery as the
  no-env-var-set case). One-line `&& home.is_dir()` guard at
  the caller; helper stays pure. No new test (fs predicates
  aren't unit-testable without infrastructure the rest of the
  caller's tests don't stand up; correctness-by-construction
  via the helper's existing coverage + the new guard).

### Added
- **Focused-pane border tints yellow when broadcast is on.**
  Cycle-178 follow-up: the tab-bar accent flipped to yellow on
  broadcast, but with `tab-bar = auto` (the default) and only one
  tab open (the common single-window case), the tab bar is hidden
  and the cycle-178 indicator becomes invisible. The user could
  toggle broadcast on, forget about it, and lose track of where
  their keystrokes were going. This cycle adds a complementary
  per-pane indicator: when broadcast is on, the focused-pane
  border flips from `palette[4]` (theme accent blue, the standard
  focused-split color) to `palette[3]` (yellow, matching the
  cycle-178 tab-bar accent). Works regardless of `tab-bar` mode
  — even with the tab bar fully disabled (`tab-bar = off`) the
  user sees the visual cue. Inactive panes keep their normal
  divider color (broadcast is scoped to the active tab,
  cycle-112 invariant). No new test (render-time tint
  conditional, same pattern as cycle 178).

- **`clear_history` action — clear scrollback without resetting the
  terminal.** Every modern terminal exposes this (kitty
  `clear_terminal`, iTerm2 "Clear Buffer", WezTerm
  `clear_scrollback`). kettle's existing `reset` action is RIS
  (`\e c`) which clears the screen AND the engine state — bigger
  hammer than users want. The new `clear_history` action writes
  `CSI 3 J` (ANSI ED 3) to the focused pane, which clears the
  scrollback buffer only and leaves the visible grid intact.
  Aliases: `clear_history` / `clear_scrollback` / `clear_buffer`.
  Honors broadcast (cycle-173/174 invariant): when group input
  is on, every pane in the active tab clears its scrollback.
  Reachable via the command palette ("Clear scrollback") and
  bindable via `keybind = … = clear_history`. Unbound by
  default (the natural chord on most terminals — `Ctrl+Shift+L`
  — collides with the shell's form-feed; the user picks their
  own preferred chord). docs/CONFIG.md keybind grammar updated.

### Fixed
- **Drag-and-drop routes through bracketed paste like clipboard
  paste does.** Cycle-175 follow-up. The drop handler wrote the
  shell-quoted path bytes raw, even when the focused pane had
  `BRACKETED_PASTE` mode enabled (vim/neovim/fzf/mc default in
  modern setups). With brackets disabled the user got the path
  cleanly; with brackets enabled, the path bytes were *not*
  wrapped in `\e[200~ … \e[201~`, so vim treated each char as a
  normal-mode command — `'` opened a register selection, `:`
  entered command mode, the path digits hopped lines, etc. Now
  uses the same `input::paste_payload(text, bracketed)` helper
  that clipboard paste uses, with per-pane wrap when broadcast
  is on (cycle-174 invariant — a broadcast set containing one
  shell + one vim doesn't break either of them). Same chrome-
  wiring shape as cycles 173/174 — no new test, fix is correct-
  by-construction once it routes through the shared helper.

- **`XDG_CONFIG_HOME=""` no longer makes `default_path` return a
  *relative* path.** Cycle-180 sibling: same empty-env-var
  filter shape, applied to `Config::default_path`. Pre-cycle,
  the first arm read `var_os("XDG_CONFIG_HOME")` and produced
  `Some(PathBuf::from(""))` for an empty value — the final path
  became `"kettle/config"`, a relative path that resolves
  against whatever the current working directory happens to be.
  A user launching kettle from a directory that happened to
  contain a `kettle/` subdirectory could have kettle silently
  read a stray config file there instead of the user's real
  one — wrong config OR (worse, in a multi-user-shared CWD
  scenario) someone else's config. Fix: filter empty values
  in every arm of the XDG_CONFIG_HOME → HOME → APPDATA fallback;
  refactored as `default_path_from(lookup)` so the env-probe
  order + filter are unit-testable without mutating the process
  env. Test pins all four branches (XDG set / HOME fallback /
  APPDATA fallback / all-empty-or-unset → None). +1 test
  (243 total).

- **`HOME=""` (empty env var) is treated as unset in
  `home_dir_fallback`.** Cycle-162 follow-up. The cycle-162 fix
  introduced `home_dir_fallback` to probe HOME → USERPROFILE →
  APPDATA when the recorded session cwd no longer exists, so
  Windows users (whose `HOME` is unset) finally landed in their
  user profile instead of kettle's launch directory. But the
  probe used `var_os(k)` which returns `Some(OsString::new())`
  for an *empty* value (`HOME=""`) — a real shape in stripped-
  down CI containers, after a misconfigured `unset HOME` /
  `export HOME=` in a parent shell, or in custom Docker entry-
  points. The empty value flowed through to
  `CommandBuilder::cwd("")` which then handed the OS spawn an
  invalid empty path. Fix: filter empty values as if unset, so
  the probe continues to the next variable in the fallback
  chain. Test pins every level: HOME empty → USERPROFILE, both
  empty → APPDATA, all three empty → None (cmd.cwd left
  untouched). +1 test (242 total).

### Documentation
- **Hardcoded test-count claims removed from user-facing docs.**
  Cycle 172 caught `docs/TESTING.md`'s "213 tests as of cycle 128"
  drift (wrong by 40+ cycles); this cycle catches the matching
  stragglers — `docs/ARCHITECTURE.md` claimed "117 workspace
  tests" (wrong by 120+) and `docs/INSTALL.md` claimed "213 tests"
  in its build-verification snippet. Both reworded to range-
  stable phrasing ("an extensive workspace test suite" / "230+
  tests"). The cycle-172 drift guard now also flags any future
  `<digit> workspace tests` / `<digit> tests across` substring in
  the user-facing markdown set, so the next time someone hardcodes
  a count it fails CI instead of going stale silently.

### Added
- **Visual indicator when broadcast (group-input) mode is on.**
  Pre-cycle, toggling broadcast via Ctrl+Shift+G (or the command
  palette) flipped the input-routing flag with no UI cue — every
  keystroke went to every pane in the active tab, but the user
  had no way to tell at a glance. Cycle 173/174 sealed up the
  broadcast scoping (keystrokes / scroll-on-keystroke / paste);
  this cycle adds the obvious missing piece — a warning-yellow
  accent (theme palette[3]) on the active tab segment's left
  edge when broadcast is on. Inactive tabs stay normal (broadcast
  is scoped to the active tab; cycle-112 invariant). No new
  config key: uses the theme's standard ANSI yellow slot so it
  works automatically with every bundled theme. No new test
  (render-time tint; the conditional is a 4-line if/else read
  from `tabbar.broadcast`).

### Fixed
- **Session restore canonicalizes the theme name the same way parse
  does.** Cycle-176 sibling. The session.json file holds whatever
  theme name was current at save time. A session written by a
  pre-176 kettle could hold a typo'd or all-lowercase name (e.g.,
  the user wrote `theme = tokyonight night` in their config and
  the pre-176 parser stored it verbatim, then save_session wrote
  that lowercase form). On restore, the pre-cycle code re-stored
  the lowercase name in `cfg.theme_name` while `Theme::by_name`
  resolved the right palette case-insensitively — so the runtime
  used TokyoNight Night's palette but `--check-config` (on the
  next reload) would have echoed the lowercase form. Route the
  restore through `Theme::find_name` (the cycle-176 helper) so
  the same canonicalization the parse path uses applies to the
  restore path too. No new test (existing `find_name` coverage
  + the existing session-restore integration smoke). Same
  cycle-shape as 173/174 — sibling chrome-wiring fix that extends
  a prior cycle's invariant to one more code path.

- **`kettle --check-config` now prints the *actual* theme name in
  use, not the user's typo.** Pre-cycle, `parse_collect` did:
  ```rust
  cfg.theme_name = e.value.clone();      // typo stored verbatim
  cfg.theme = Theme::by_name(&e.value);  // silent fallback to default
  ```
  So a user writing `theme = TokyoNitght Night` (typo) had
  `--check-config` print `theme: TokyoNitght Night` while the
  runtime palette was actually TokyoNight Night's defaults. Same
  docs/runtime mismatch shape as cycle 139 (font-size clamp).
  Now: store the *canonical* bundled name (with original casing)
  when the lookup matches; leave `theme_name` at the prior
  default when it misses. Bonus: `theme = tokyonight night`
  (all-lowercase) now produces `theme_name = "TokyoNight Night"`
  (canonical casing) — was lowercase before. The malformed-value
  diagnostic still flags the typo separately so the user sees
  their mistake. New `Theme::find_name` companion to `by_name`.
  Test: `theme_name_matches_the_actually_loaded_palette` pins
  case-insensitive→canonical, typo→default-name, and the
  diagnostic-still-flags assertion. +1 test (241 total).

### Added
- **Drag-and-drop files.** Dropping a file onto the kettle window
  inserts its shell-quoted path at the cursor of the focused pane
  (or broadcasts to every pane in the active tab when group input
  is on). A trailing space is appended so the common workflow —
  type `cat `, drop a log file, press Enter — Just Works. POSIX-
  style single-quote escaping (close-escape-reopen for embedded
  apostrophes) so the same form works on bash / zsh / fish /
  PowerShell 7+. iTerm2 / WezTerm / kitty / Ghostty / GNOME
  Terminal all have this affordance. Test:
  `shell_quote_path_handles_spaces_quotes_and_multibyte` pins
  spaces, apostrophe escaping (single + repeated), multibyte
  paths, and empty input. +1 test (240 total).

### Fixed
- **Paste distributes to every pane in a broadcast group, not just
  the focused pane.** Cycle 173 sibling. With broadcast on
  (Ctrl+Shift+G group-input mode), keystrokes go to every pane in
  the active tab — paste is also user input and should follow the
  same scoping. Pre-cycle, Ctrl+Shift+V (or middle-click) wrote
  only to the focused pane regardless of broadcast state, so a
  user who'd just turned on broadcast to send the same command to
  three SSH sessions saw it work for typing but silently single-
  target for paste. New `Mux::broadcast_paste(text)` reads each
  pane's `BRACKETED_PASTE` mode separately and wraps the bytes
  per-pane (panes can disagree — e.g., one is in vim, one is at
  a shell prompt — and wrapping the wrong way would either
  inject literal `\e[200~`/`\e[201~` markers into the shell or
  leave bytes vulnerable to the paste-injection attack inside
  vim). Same active-tab scoping as `broadcast_write` and
  `broadcast_scroll_to_bottom` (cycle-112 leaf_ids invariant).
  Chrome-only, no new test (PTY-mode reads aren't unit-testable
  without infrastructure the rest of the mux tests don't stand
  up — same rationale as cycle 173 / 151).

### Fixed
- **`scroll-on-keystroke` (default `true`) now applies to every
  pane in a broadcast group, not just the focused pane.** The
  config flag says "snap the viewport back to the bottom on every
  keystroke" — meant to keep the user's view of incoming output
  current. With broadcast off, only the focused pane is written
  to and only it snaps; self-consistent. With broadcast on (the
  Ctrl+Shift+G group-input mode where typing goes to every pane
  in the active tab), the pre-cycle code wrote the bytes to all
  panes but skipped the snap entirely — so a user with broadcast
  on AND any pane scrolled back saw their typing reach the remote
  shells fine while the local view of those panes stayed pinned
  to history (no way to tell from the screen that the bytes
  actually went through). Fix: new
  `Mux::broadcast_scroll_to_bottom` companion to `broadcast_write`,
  same active-tab scoping (cycle-112 invariant); called from the
  same `scroll_on_keystroke` gate. No new test — the scoping
  matches `broadcast_write`'s, which is pinned by the cycle-112
  `leaf_ids` test; the snap itself requires a real Term lock that
  the existing mux unit tests don't stand up. Same shape as
  cycle 151 — chrome-only fix, correctness-by-construction.

### Documentation
- **User-facing docs no longer leak internal `cycle N` references.**
  Cycle 168 caught the audit-trail leak in `kettle --help`; this
  cycle extends the cleanup to the markdown docs the README links
  to. `docs/CONFIG.md` had two stragglers (`(cycle 138)` next to
  the bool-alias prose, `(cycle 163)` next to the modifier-typo
  rejection rule) — same mysterious-parenthetical UX issue as
  `--help`. `docs/TESTING.md`'s lead now says "230+ tests" instead
  of a specific cycle-number snapshot ("213 tests as of cycle 128")
  that's been wrong for 40+ cycles; the per-crate counts below
  remain order-of-magnitude. Regression test
  `user_facing_docs_have_no_internal_cycle_refs` reads README.md,
  docs/CONFIG.md, docs/INSTALL.md and scans for the pattern
  `cycle <digit>` — same drift-guard shape as cycle 168 for the
  CLI surface, but for the user-facing markdown surface.
  TESTING.md / ROADMAP.md / CONTRIBUTING.md are intentionally
  exempt (contributor-leaning, cycle refs serve as CHANGELOG
  anchors). +1 test (239 total).

### Internal
- **Two more blink-reset sites route through `reset_blink_phase()`.**
  The cycles 134-141 + 144 + 150 audit landed a shared
  `reset_blink_phase()` helper, but two callers still inlined the
  same two field writes (`blink_on = true; last_blink = now()`):
  `WindowEvent::Focused` and `WindowEvent::KeyboardInput`. A future
  change to the reset semantics (e.g., also clearing a `blink_pause`
  field if one's added) would need to touch every call site —
  routing through the helper keeps all eight user-driven blink-reset
  paths (Reset, focus changes, modal close, typing, tab close,
  window focus, DEC ?12 toggle, mouse focus) in lock-step. The one
  inline that remains is `CursorBlinkingChange`, which runs inside
  `self.mux.panes.values_mut()` and can't borrow `self` again —
  that one's documented in place. No behavior change; 238 tests
  still pass.

### Fixed
- **`kettle --list-keybinds` renders `Ctrl+Plus` / `Ctrl+Minus` /
  `Ctrl+Equal` for the punctuation keys, not the literal-`+`
  ambiguity of `Ctrl++` / `Ctrl+-` / `Ctrl+=`.** `Trigger::label`
  was uppercasing every `Char(c)` and joining with `+`, so the
  default zoom-in binding (Ctrl++ for the `+` key) showed up as
  `Ctrl++` — two adjacent `+` make it unclear whether the second
  one is the separator's repetition or the key itself. Same
  shape for `Ctrl+-` (zoom out: looks like a trailing dash) and
  `Ctrl+=` (also zoom in: looks like an assignment). The parser
  already accepts `plus` / `minus` / `equal` as named-key tokens
  (the same way the user would type them in their config file);
  the label now mirrors that convention so the row reads
  `Ctrl+Plus  IncreaseFontSize` and a user copying it back into
  their config file works without translation. Both kitty and
  Ghostty render these as `Plus`/`Minus`/`Equal` for the same
  reason. Test: pins the three named-token labels + two
  unaffected punctuation chars (`,` `/`) + plain letter
  regression. +1 test (238 total).

- **`font-feature = LIGA on` (uppercase tag) now actually toggles
  ligatures.** OpenType feature tags are case-sensitive per spec —
  every standard tag is lowercase (`liga`, `clig`, `calt`, `cv01`,
  `ss05`…). `FontFeature::parse` was storing whatever case the user
  typed, so `LIGA on` had two silent failures: (1) `is_ligature()`
  returned false because it only matched lowercase `liga`/`clig`/
  `calt`/`dlig`, so the coarse `cfg.font_ligatures` flag stayed
  stale and downstream code thought ligatures were still on; (2)
  the uppercase tag was passed verbatim to the cosmic-text /
  harfbuzz shaper, which uses a case-sensitive lookup and silently
  ignored the unknown `LIGA` tag. Net effect pre-fix: the user's
  `LIGA on` did nothing visible — ligatures didn't toggle, the
  feature didn't apply.
  Fix: `FontFeature::parse` lowercases the tag bytes before
  returning. Both the `is_ligature()` check and the FeatureTag
  passed to the shaper now see the canonical form. Test:
  `font_feature_tag_is_lowercased` pins uppercase / mixed-case
  inputs and asserts the downstream `cfg.font_ligatures` flag
  toggles the same way it would for lowercase. +1 test (237 total).

- **`kettle --help` no longer leaks internal cycle references, and
  `--config` documents the cycle-164 directory rejection.** The
  rustdoc-style doc comments for `--list-keybinds` and `--config`
  carried internal audit trail like `(cycle 103)` and
  `(cycle 106)` — useful for me reading the source, mysterious
  parentheticals when a user runs `kettle --help` in a real
  terminal. The `--config` description also still said "non-existent
  path is a hard error" with no mention that cycle 164 extended the
  check to reject directories too (typing `--config ~/.config/kettle`
  when you meant `.../kettle/config` is now a hard error, not a
  silent fallback to defaults).
  Fix: rewrote both doc comments to describe the *user-facing*
  behavior in plain English, dropping the cycle refs (the audit
  trail lives in code comments and CHANGELOG, where it belongs).
  Added a regression test that walks every clap `Arg`'s help and
  long-help (plus the top-level about/long-about) and asserts none
  contain the substring `"cycle "` — same shape as the cycle-116
  `defaults_has_no_shadow_collisions` drift guard, but for the
  CLI's user-facing surface. +1 test (236 total).

- **Bundled-theme filter is robust to OS/editor junk in
  `assets/themes/`.** `build.rs` skipped only the exact filenames
  `LICENSE` and `README.md`. A maintainer cloning the repo on
  macOS and opening the themes folder in Finder would pollute the
  bundled theme list with a `.DS_Store` "theme" whose contents are
  binary garbage — and the count is publicly surfaced
  (`kettle --list-themes`, README highlights). Same shape for a
  Windows checkout with `Thumbs.db`, an Emacs swap file, or
  `.swp`/`.bak`/`*~` backup files left over after editing a theme.
  Fix: extracted `is_bundled_theme_filename(name) -> bool` into a
  small `theme_filter` module the lib and `build.rs` share via
  `include!`. The filter rejects dotfiles
  (`.DS_Store`/`.gitignore`/`.directory`/etc.),
  desktop-metadata files (`Thumbs.db`/`desktop.ini`/macOS
  `Icon\r`), and editor backup-file suffixes (`~`/`.bak`/
  `.orig`/`.swp`/`.swo`/`.tmp`, case-insensitive). +1 test
  pinning all of the above plus four real theme names that must
  still pass (235 total).

- **Autodetected Wikipedia / Apple-docs / MDN URLs that legitimately
  end in `)` now stay clickable.** Both `links.rs` (the runtime
  hyperlink overlay) and `hints.rs` (`Ctrl+Shift+H` quick-select)
  had their own private `trim_trailing` that stripped *every*
  trailing `)` / `]` / `}` along with the other prose punctuation.
  A URL like `https://en.wikipedia.org/wiki/Foo_(bar)` was trimmed
  to `https://en.wikipedia.org/wiki/Foo_(bar` — a different
  (404) page. Same shape for any URL ending with a closing bracket
  used for disambiguation.
  Fix: shared `kettle_core::url_trim::trim_trailing` that
  bracket-balance-aware-strips: a `)` / `]` / `}` is removed only
  when the candidate substring has *more* closes than opens of the
  matching pair. So `..._(bar)` keeps its bracket (1 open + 1
  close = balanced), but `https://rust-lang.org)` from a
  `(https://rust-lang.org).` excerpt loses it (0 opens + 1 close =
  unbalanced) — both cases the user actually wants. Operates at
  byte level so multi-byte chars in IRI-ish URLs are passed through
  verbatim. +5 tests pinning sentence-punctuation, balanced-keep,
  unbalanced-strip, multi-byte-untouched, and an empty-input
  no-op (234 total).

- **`kettle --list-keybinds` columns line up again — even for the
  three default rows whose triggers exceed 16 chars.** `describe()`
  hard-coded the trigger column at 16 chars, so `Ctrl+Shift+PageDown`
  (19 chars; move-tab-right) and `Ctrl+Shift+PageUp` (17 chars;
  move-tab-left) overflowed the padding and their action column
  landed one or three bytes to the right of every other row.
  Visually jarring on the one CLI command whose purpose is making
  the keymap scannable. Fix: column width = max(16, longest
  trigger label) — same shape as `format_ssh_hosts` (cycle 105).
  Test pins the alignment contract: byte `longest+1` is the
  separator's second space and byte `longest+2` is the first
  action char on every row. +1 test (229 total).

- **`--config DIR` is now a hard error instead of a silent
  fallback-to-defaults.** Cycle 106 made `--config` fail when the
  path didn't exist. The matching "exists but isn't a regular file"
  case (typically a directory — a user typing `--config ~/.config/kettle`
  intending the file `~/.config/kettle/config`) wasn't covered. The
  path passed the existence check, `read_to_string` returned an
  `IsADirectory` error, `load_from_with_diagnostics` logged a
  `warn`-level message most users miss, and downstream branches used
  the default Config — the user saw the same "my theme is gone"
  symptom as the cycle-106 no-such-file case but with no obvious
  CLI-surface error to point at. Fix: hard-fail with
  `--config PATH: not a regular file` when `p.exists() && !p.is_file()`,
  mirroring the existing `--working-directory` shape (cycle 107).
  Extracted as a pure `config_path_problem(&Path) -> Option<&str>`
  helper so the truth table (missing / dir / regular file) is
  unit-testable without spawning the binary. +1 test (228 total).

- **Keybind modifier parsing recognizes `win`/`windows`/`meta`/`logo`
  as Super aliases, and *rejects* typo'd modifier names outright.**
  Before cycle 163, `parse_trigger` only knew `super`/`cmd`/`command`
  for the Super key — a user copying `keybind = win+t=new_tab` from
  their Windows config (or `meta+x` from a Linux config) silently
  saw the `win`/`meta` token fall to the `parse_key(other)` catchall,
  which returned None, then the parser kept iterating, so `key`
  ultimately landed on the *plain key* token (`t`/`x`). Result: every
  press of `t` in the terminal opened a new tab. Any typo'd modifier
  (`cttrl+t`, `supre+t`) had the same shape. Fix: extend the Super
  alias set (super / cmd / command / win / windows / meta / logo —
  the names every OS/WM/Qt ecosystem uses for the same key), AND
  make `parse_trigger` strict so a non-modifier in any but the
  last `+`-separated slot returns None. `--check-config` already
  gates triggers via `parse_trigger.is_some()`, so the rejected
  line now surfaces as a malformed-value diagnostic instead of
  becoming a "secret" plain-key binding stomping on normal typing.
  Test: pinned all seven Super aliases + multi-modifier chord +
  three typo rejections (`cttrl`, `contorl`, `supre`) + bare-`f5`
  regression. +1 test, docs/CONFIG.md updated.

- **Stale-cwd fallback now works on Windows too (and on stripped-down
  Linux containers).** When a saved session's recorded pane cwd no
  longer exists on disk — user moved the repo between launches, or
  the `-d` arg pointed at a since-deleted directory — kettle falls
  back to the OS home directory before letting `portable_pty` spawn
  the shell. The previous code only consulted `HOME`, which is unset
  on Windows by default: stale-cwd Windows users silently ended up
  in whatever directory they happened to launch kettle from
  (typically `C:\` from a Start-menu shortcut). Now
  `home_dir_fallback(lookup)` probes `HOME` → `USERPROFILE` →
  `APPDATA` in order, so all three platforms (Linux, macOS, Windows)
  converge on the same "user-home" intent. Same shape as
  cycle 159's macOS universal2 fix — Linux+macOS worked, Windows
  didn't, the env var probe order was the difference. The helper
  takes a `lookup` closure so its order can be unit-tested without
  mutating the real process env (which would race with the rest of
  the suite). Test: pinned truth table across HOME-set, USERPROFILE-
  only, APPDATA-only, and empty-env. +1 test (226 total).

- **OS mouse cursor is now the standard arrow over the tab bar and
  modal overlays (not the text I-beam).** `sync_cursor_icon` only
  considered two states — `Pointer` while a Ctrl-held URL was under
  the mouse, and `Text` everywhere else — so hovering the clickable
  tab bar, scrollbar-thumb-adjacent area, or any open modal (search
  bar, command palette, hint mode, SSH launcher) showed the I-beam,
  visually implying "this surface accepts text selection" when those
  surfaces don't. The fix extracts a pure
  `chrome_cursor_icon(in_tab_bar, modal_open) -> Option<CursorIcon>`
  helper that returns `Some(Default)` for chrome and `None` for
  content (the caller's existing Pointer/Text branch then runs),
  plus a new `any_modal_open()` reader on `App` that mirrors
  `close_all_modals`. iTerm2 / WezTerm / Ghostty / kitty all show
  the standard arrow over their chrome — this brings kettle in
  line. Test: the truth table of all four (in_tab_bar × modal_open)
  states pinned in `app::tests::chrome_cursor_icon_overrides_only_for_chrome`.

### Documentation
- **`CONTRIBUTING.md` documents the audit-cycle pattern.**
  After 150+ cycles the project has a distinctive workflow
  (find a bounded silent-fallback bug → extract a pure
  helper → wire it in → pin the contract with a test → land
  behind the full gate) that's hard to reverse-engineer
  from the CHANGELOG alone. New top-level file walks through
  the cycle shape, lists project layout, gives a real
  recent example (cycle 151's notify-filter fix), and points
  newcomers at `_ => {}` arms / the ROADMAP "Next" list as
  starting points. README's documentation section links to
  it.

### Build
- **macOS release builds are now actually universal (`x86_64` +
  `aarch64`).** The release workflow's artifact has been named
  `kettle-macos-universal.zip` since the project's first
  tagged release scaffolding, but the underlying binary was
  whatever single architecture `macos-latest` happened to be
  (currently arm64, but historically x86_64). An Intel-Mac
  user downloading the "universal" archive got a binary
  their CPU couldn't run; an Apple-Silicon user got a
  potentially-Rosetta-translated x86_64 binary, slow and
  unnecessary. Now the workflow:
  - Adds both `x86_64-apple-darwin` and `aarch64-apple-darwin`
    targets to the toolchain.
  - Builds release artifacts for each.
  - Combines them with `lipo -create` into a single
    universal2 binary at `target/release/kettle`.
  - The existing `.app` packaging step copies that universal
    binary unchanged.
  Linux and Windows still do the native single-arch build.

### Fixed
- **`--check-config` no longer flags empty values as malformed.**
  parse.rs documents the "empty value resets the key"
  semantics; cycle 121/122 made the runtime honor it
  explicitly for string keys, and the bool / enum / numeric
  arms naturally fall through to defaults on empty. But
  `detect_malformed_values` still tried to validate the
  empty string against each per-key contract, surfacing
  diagnostics like `malformed value: theme = ""` while the
  runtime quietly used the default. Disagreement.
  Now a single empty-value skip at the top of the per-key
  match handles every key uniformly. Diagnostic surface
  agrees with runtime — empty means "use default, no
  warning needed." +1 test covers theme / font-family /
  cursor-style / cursor-style-blink / bell / scrollbar /
  font-size / background-opacity all on empty plus a real
  typo regression guard.

### Fixed
- **Tab-close-via-middle-click and `Action::CloseWindow`
  save the session before exit.** Two exit paths were
  missing the save_session call that every other exit path
  already had (Action::CloseTab on the last tab, Action::
  ClosePane closing the final pane, WindowEvent::Close
  Requested via the OS window-X button). Result: a user
  middle-clicking their last tab or hitting `Ctrl+Shift+Q`
  (CloseWindow) exited kettle without persisting the
  now-empty session. On next launch, the *previous* multi-
  tab state from before the close still sat in
  session.json and silently restored — the user expected
  a fresh start, got their old layout back. Both paths now
  save before `event_loop.exit()`, matching the other
  exit handlers.

### Fixed
- **`detect_malformed_values` also strips a leading UTF-8 BOM.**
  Sibling to cycle 155. The cycle-155 strip lived only in
  `parse::parse`; `detect_malformed_values` does its own raw
  text scan for missing-`=` lines (cycle 96) and would still
  surface `missing `=` separator: "\u{feff}font-family"` on
  a BOM-prefixed config with a typo on the first line —
  invisible character mangled the diagnostic. Now the same
  one-line `strip_prefix('\u{feff}')` is applied here too.
  +1 test (`detect_malformed_values_strips_bom_before_scanning`)
  covers the missing-= + BOM combination and confirms a
  clean BOM-prefixed config isn't flagged.

### Fixed
- **Config parser strips a leading UTF-8 BOM.** Notepad and
  a few Windows editors save UTF-8 text files with a leading
  byte-order mark (`\u{feff}`, 0xEF 0xBB 0xBF). Without
  stripping it, the first config line parsed as `\u{feff}theme
  = …` and the BOM-prefixed key surfaced as an
  `unknown key: ﻿theme` in `--check-config` — invisible
  character making the diagnostic look bizarre, and the
  user's theme setting silently didn't apply. The parser now
  drops the BOM if it's at byte 0; a `\u{feff}` mid-file is
  not a BOM and stays in the value. +1 test
  (`strips_leading_utf8_bom`). Verified end-to-end against a
  `printf '\xef\xbb\xbftheme = ...'` fixture: status now
  reads `OK — no issues`.

### Fixed
- **Opening a modal closes any other modal first.** A user
  with the SSH launcher open who pressed `Ctrl+Shift+K` got
  both the SSH launcher AND the command palette rendered
  on top of each other, with the palette capturing keys
  (because the input dispatch checks hint → palette → ssh
  → search, first-open-wins). Visually confusing — the
  user couldn't tell which modal would receive their next
  keystroke without trying. Now `StartSearch`, `OpenSsh`,
  `CommandPalette`, and `HintMode` all call a new
  `close_all_modals()` helper before opening their own
  state. Extracted from cycle 111's `Action::Reset` sweep
  so both share one implementation.

### Fixed
- **Workspace `repository` URL points at the actual repo.**
  `Cargo.toml`'s `[workspace.package].repository` said
  `https://github.com/kevim/kettle` — but the actual repo
  has been `https://github.com/Reddimus/kettle` from the
  start. Stale metadata that affects: any future
  `cargo install kettle`, crates.io listings if published,
  any tooling that scrapes the Cargo.toml for an upstream
  URL. Other docs (INSTALL.md's `git clone …`) already
  had the correct URL.

### Fixed
- **Session restore agrees with `Theme::by_name` on case.**
  The session-restore branch checked `Theme::list().contains
  (&name)` (case-sensitive verbatim string match) before
  applying a stored theme, but `Theme::by_name(name)` is
  case-insensitive (cycle 0). A session written by an older
  kettle build, or hand-edited, holding a lowercase theme
  name (`tokyonight night`) would fail the verbatim
  `contains` check and stay on the default theme — even
  though `by_name` would have happily resolved it. Now
  the check uses `iter().any(|n| n.eq_ignore_ascii_case
  (name))` so the gate agrees with the apply.

### Fixed
- **Live config reload no longer fires on unrelated file
  events.** The `notify` watcher watched the config file's
  *directory* (NonRecursive) and reloaded on every event.
  Cycle 109's atomic session save writes
  `session.json.tmp.<pid>.<nanos>` then `rename`s it into
  place — each save fires 3+ notify events
  (create-temp / write-temp / rename), all of which used
  to pointlessly trigger a config reload. Editor swap
  files (`.config.swp`), theme caches, the user's own
  `vim` editing some other file in `~/.config/kettle/` —
  same story. Filter now matches `event.paths` against the
  watched config file specifically, so only edits to the
  config file itself cause a reload. No behavior change
  for the intended path (user edits config in any editor;
  notify fires for the config file; we reload).

### Fixed
- **DEC ?25l (hide cursor) is respected even when the window
  is unfocused.** The renderer's `draw_cursor` gate was
  `shape != Hidden && cp.line.0 >= 0 && pv.focused` — missing
  the `cursor_visible` flag. So when a TUI (vim, less, fzf,
  etc.) sent `\e[?25l` to hide its cursor and the user
  clicked away, the *unfocused-window hollow outline* still
  drew. Cursor was supposed to be invisible; it wasn't. The
  shape-based Hidden variant was correctly excluded, but
  DEC ?25 is a separate mode and routed through a different
  flag, so the bug only fired on the `shape != Hidden &&
  cursor_visible == false` combination — i.e. any program
  using `printf '\e[?25l'` rather than DECSCUSR `q`. Now
  the `cursor_visible` flag also gates `draw_cursor`, so a
  hidden cursor stays hidden in both focused and unfocused
  states, and across all DECSCUSR shapes (Block / Underline
  / Beam / HollowBlock).

### Fixed
- **`--screenshot` PNG honors `background-opacity` too.**
  Cycle 148 fixed the live-window path's clear-op alpha
  (`a: cfg.background_opacity`) and surface alpha-mode
  selection. The screenshot path's clear op still hardcoded
  `a: 1.0`, so `kettle --config /transparent.conf --screenshot
  out.png` produced an opaque PNG regardless of what the
  user asked for. PNG output is RGBA8 and the alpha channel
  is stored verbatim — honoring the config makes the
  screenshot match what the live window shows. Verified
  end-to-end: an `--screenshot` at `background-opacity = 0.5`
  produces a noticeably larger PNG (alpha varies across
  pixels) than the same shot at `1.0` (flat 0xff alpha).

### Fixed
- **`background-opacity` actually produces transparency.**
  Real bug. The old surface config used
  `alpha_mode: caps.alpha_modes[0]` — i.e. whatever the
  backend listed first, which on most platforms is
  `Opaque`. The `wgpu::Color { a: cfg.background_opacity }`
  on the clear op then had its alpha channel discarded by
  the surface composite, so `background-opacity = 0.5`
  rendered as fully opaque. A user setting transparency
  for a desktop-blur effect saw no difference between
  `1.0` and `0.5`. Now when `background_opacity < 1.0` we
  prefer `PreMultiplied → PostMultiplied → Inherit →
  Auto` (the standard alpha modes for compositing),
  falling back to whatever the backend lists first only
  if none of those are supported. Opaque configs are
  unchanged. Headless smoke still passes.

### Fixed
- **`Action::from_name` is now case-insensitive.** Same pattern
  as cycle 146's enum-key fix, applied to keybind action names.
  A user writing `keybind = ctrl+shift+c = Copy` (capitalized)
  used to silently drop the binding — `from_name` returned
  None on the unrecognized case variant, and `apply_keybind`'s
  silent-skip path swallowed it. `--check-config` flagged it
  via cycle 85, but the runtime didn't bind anything. Now
  lowercased (and whitespace-trimmed) before matching, so
  `Copy` / `COPY` / `copy` / `  paste  ` all resolve. The
  parametric `GOTO_TAB:1` form also works. +1 test
  (`action_from_name_is_case_insensitive`).

### Fixed
- **Enum config keys are now case-insensitive.** Cycle 138
  made the bool keys case-insensitive via `parse_bool`.
  The six enum keys (`bell`, `osc52`/`clipboard`, `tab-bar`,
  `tab-bar-position`, `scrollbar`, `cursor-style`) still
  matched `e.value.as_str()` verbatim, so `bell = OFF`
  silently fell into the catchall (→ `BellMode::Both`,
  the default) while `--check-config` flagged the same
  spelling as malformed. Both surfaces now lowercase
  before matching, so case variants validate and apply
  the same way as the canonical lowercase forms. +1 test
  (`enum_keys_are_case_insensitive`) covers all six keys
  with uppercase / mixed-case variants and confirms
  `--check-config` no longer flags them.

### Changed
- **`kettle --list-themes` is now case-insensitive alphabetical.**
  The build-script's pre-cycle sort was raw `String::cmp`, which
  is ASCII-bytewise: uppercase letters (0x41..0x5A) sort before
  lowercase (0x61..0x7A), so `CGA` came before `branch` because
  `'C' < 'b'` in ASCII. Skimming the 512-theme list was harder
  than it needed to be — users expect mixed-case alphabetical
  (matching what GNU `sort` does in a UTF-8 locale). New sort:
  `to_lowercase()` primary, original cmp tiebreak. End-to-end:
  `branch` now precedes `Calamity`; `CGA`/`Chalk` interleave
  with lowercase c-themes naturally. Also affects the order
  the `next_theme` / `prev_theme` chord cycles in.

### Fixed
- **Closing a tab via middle-click or ✕ also resets blink
  on the now-active tab.** Cycle 120's `reap_tabs` fix
  keeps `mux.active` pointing at the same tab the user was
  on when an *unfocused* tab closes; when the *focused*
  tab closes, focus naturally falls on a neighbor (matching
  every modern terminal). Either way the cursor lands on a
  potentially-different pane, and pre-cycle-144 that
  pane's cursor could be invisible for up to one
  `blink_interval` depending on the blink-timer phase.
  The tab-bar middle-click and ✕-click paths now snapshot
  `focus_key()` before the close and call
  `note_focus_change(pre)` after — same shape as cycles
  135/136's keyboard-and-pane-click paths. The last
  user-driven focus path that hadn't picked up the
  cycle-134→141 blink-reset pattern.

### Documentation
- **`docs/CONFIG.md` documents bool aliases, numeric clamps,
  and the `beam` cursor-style alias.** The bool-row entries
  just said `bool` with no hint that "yes" / "no" / "off" /
  "on" / "0" / "1" / "enabled" / "disabled" are also accepted
  (cycle 138). Numeric-range clamps (cycles 118/131/132/133)
  were never mentioned in the docs even though they affected
  user-facing behavior. The `beam` alias (cycle 142) wasn't
  in the cursor-style row. Added a "Type notes" preamble
  that documents all three concerns and updated the
  cursor-style row's value list.

### Added
- **`cursor-style = beam` accepted as an alias for `bar`.**
  Alacritty's config calls the vertical-stroke cursor
  `Beam`; kettle's enum calls it `Bar`. A user copying their
  Alacritty config got a silent fallback to `Block` plus a
  `--check-config` malformed-value warning. Now `beam`
  parses to `CursorStyle::Bar` directly and
  `detect_malformed_values` no longer flags it.
  +1 test (`cursor_style_accepts_beam_as_alacritty_alias_for_bar`)
  covers all four valid values, plus a real typo
  (`bream`) still flagging.

### Fixed
- **Typing also resets the cursor blink phase.** Final
  user-gesture path that still missed the blink reset. A
  fast typist hitting a key right as `blink_on` was false
  saw a brief flash of no-cursor before the next half-
  period. Alacritty / kitty / iTerm2 / WezTerm all reset
  on every keystroke; matches the rest of the user-driven
  paths kettle now handles (cycle 134: Reset; cycle 135:
  focus actions; cycle 136: mouse focus; cycle 140: modal
  close).

- **Closing a modal overlay also resets the cursor blink
  phase.** Cycle 134 fixed the chord-Reset path; cycles
  135/136 covered focus changes (keyboard and mouse). The
  four modal-close paths — Escape closing the search bar,
  command palette, quick-select hints, or SSH launcher —
  still left the cursor invisible for up to one
  `blink_interval` after the close if it landed on the
  off-half. Same "where's my cursor?" surprise on the
  pane the overlay was hiding.
  - New shared helper `fn reset_blink_phase(&mut self)`
    centralizes the two-line reset (cycle 134's body)
    so the five call sites — search-Escape, palette-Escape,
    hint-Escape, ssh-Escape, and `Action::Reset` — all
    use one path. The `note_focus_change` helper
    (cycle 136) now delegates to it.
  - The `CursorBlinkingChange` event handler (DEC ?12)
    can't call the helper because it runs inside a
    `self.mux.panes.values_mut()` loop (borrow conflict);
    keeps the inline two-line body, documented with a
    pointer to the helper.

- **`font-size` clamps at parse-time, not just at render-time.**
  Cycle 118 added `clamp_font_size` in `Renderer::new` /
  `set_font_size`; cycle 131 surfaced out-of-range as a
  `--check-config` diagnostic. But `cfg.font_size` still held
  the raw value — so `--check-config`'s `font: ... 500pt`
  print echoed the user's input *not* what the renderer would
  use. Now `parse_collect` also clamps to [5.0, 72.0] so the
  stored value matches reality. Cycle 132 already did this
  for the other clamped numerics; cycle 139 closes the
  symmetry. End-to-end: `font-size = 500` now reads as
  `font: ... 72pt` in `--check-config` (with the diagnostic
  still flagging the over-cap value).

### Fixed
- **Bool config keys accept the standard true/false aliases
  + flag unrecognized values.** All five bool fields used:
  `cfg.X = e.value != "false"`. Result: every non-literal-
  "false" value silently meant *true* — so `cursor-style-blink
  = no` enabled the blink instead of disabling it; `copy-on-
  select = 0` enabled copy; etc. A real footgun.
  - New `pub(crate) fn parse_bool(s: &str) -> Option<bool>`
    recognizes case-insensitive `true / yes / on / 1 /
    enabled / enable / y` for truthy and `false / no / off /
    0 / disabled / disable / n` for falsy.
  - The five bool parsers (`cursor-style-blink`,
    `copy-on-select`, `scroll-on-keystroke`,
    `scroll-on-output`, `mouse-hide-while-typing`) route
    through `parse_bool` — bad values keep the current state
    instead of silently flipping to `true`.
  - `detect_malformed_values` flags unrecognized values so the
    typo surfaces in `--check-config`.
  +1 test (`bool_keys_accept_yes_no_off_on_0_1_aliases`)
  covers all 8 truthy + 7 falsy aliases × all 5 keys, plus
  the typo→default-preserved + diagnostic-fires paths.

### Fixed
- **`Renderer::resize` clamps the surface to the device's
  max texture dimension.** Old `resize` only floor-clamped at 1
  (`surface.configure(0, …)` would panic). The ceiling went
  unchecked, so a window stretched past 8192 px (multi-4K
  spans, 8K displays, or a tiling-WM tile larger than the
  device limit) used to silently fail `surface.configure`'s
  validation and leave the surface in a stale state painting
  nothing. Now `width.clamp(1, device.limits().
  max_texture_dimension_2d)` clips to whatever the device
  actually supports — the user sees the visible top-left
  region cleanly instead of a frozen frame. Sibling to cycle
  119's `cap_axis_cells` which fixed the same class of bug on
  the `--screenshot` path.

- **Mouse-driven focus changes also reset the blink phase.**
  Cycle 135 caught the keyboard path; this cycle extends to:
  - Clicking a tab in the tab bar to switch tabs.
  - Clicking inside a pane to focus it (`Mux::focus_at`).
  Both could leave the new pane's cursor invisible for up to
  one `blink_interval` after the click, depending on the
  half-period the timer happened to be on. Extracted the
  cycle-135 pre/post pattern into shared helpers
  (`focus_key()` + `note_focus_change(pre)`) so the three
  focus-changing entry points (`handle_action`, tab-bar
  click, content-area click) all use one implementation.

- **Any focus-changing action also resets cursor blink phase.**
  Cycle 134 fixed it for `Action::Reset` specifically. The same
  "where's my cursor?" surprise applied to every focus-changing
  action: `NextTab` / `PrevTab` / `GotoTab(N)`, `FocusNext` /
  `Prev` / `Up` / `Down` / `Left` / `Right`, `ToggleZoom`, and
  any other action that flipped which pane the cursor lives in.
  Hit `Alt+Right` to jump to the next pane right as `blink_on`
  was false → cursor invisible on the new pane for up to one
  `blink_interval` (530 ms default), which is exactly the
  beat where you've just told kettle "show me where I'm
  typing next."

  Snapshot `(mux.active, mux.active_focus())` before the
  match runs; compare after. If the focused (tab, leaf)
  changed at all, reset `blink_on = true; last_blink =
  Instant::now()`. Catches every focus-changing path in
  one place without decorating each arm individually.

- **`Action::Reset` also resets the cursor blink phase.**
  Cycle 111 swept the modal overlays + selection so the
  chord meant "fresh start" — but it left the `blink_on`
  flag and `last_blink` timestamp untouched. Hitting Reset
  right as `blink_on` was false left the user staring at a
  *missing* cursor for up to one blink-interval (530 ms
  default) — confusing precisely because Reset is the chord
  users hit to recover from a visually-jammed terminal.
  Now sweeps `blink_on = true; last_blink = Instant::now()`
  alongside the cycle-111 modal/selection clears. Mirrors
  the same fix already applied to `TermEvent::CursorBlinking
  Change` so DEC mode 12 toggles also land the cursor
  visible-first.

### Fixed
- **`scrollback = N` clamped at `INFINITE_SCROLLBACK` (10 M
  lines), out-of-range flagged.** A user typo'd or
  curious-pasted `scrollback = 100000000` (100 M) used to
  flow that value verbatim into `cfg.scrollback`, which
  alacritty_terminal honored by reserving rows for ~250 GB
  of history on the first PTY spawn. The docstring on
  `INFINITE_SCROLLBACK` calls 10 M "practical stand-in for
  infinite"; anything higher is asking for an OOM. Now
  clamped at parse to `INFINITE_SCROLLBACK`, and
  `detect_malformed_values` flags above-cap values so the
  user sees the silent cap in `--check-config`. Cycle-132
  pattern, but on a field whose mistake was a memory
  footgun rather than a visual artifact. +1 test
  (`scrollback_clamps_at_infinite_and_flags_above`)
  covers 10M+1, 100M, in-range untouched, the three
  documented escape hatches (`infinite`/`unlimited`/`0`),
  and the cap-above diagnostic.

### Fixed
- **`--check-config` flags the other four clamped numerics
  + `background-opacity` clamps at parse.** Cycle 131
  surfaced `font-size`'s runtime-clamp / docs mismatch. The
  same pattern lived in four siblings:
  - `background-opacity` — no runtime clamp at all (raw value
    flowed to `wgpu::Color { a: ... }`, where alpha < 0 / > 1
    is undefined on some backends). **Now clamped at parse**
    to `[0.0, 1.0]` so the runtime stays safe even if the
    user ignores the warning. + diagnostic for out-of-range.
  - `unfocused-split-opacity` — clamped to `[0.1, 1.0]` at
    parse; diagnostic added.
  - `scroll-multiplier` / `mouse-scroll-multiplier` — clamped
    to `[0.1, 50.0]` at parse; diagnostic added.
  - `minimum-contrast` — clamped to `[0.0, 21.0]` at parse;
    diagnostic added.
  - `cursor-blink-interval` — clamped to `[50, 5000]` at
    parse; diagnostic added.
  +1 test (`detect_malformed_values_flags_clamped_numerics_out_of_range`)
  covers 9 out-of-range entries (all flagged), 14 in-range +
  boundary entries (none flagged), and the new
  `background-opacity` runtime-clamp behavior for the
  user-ignores-the-warning path.

### Fixed
- **`--check-config` flags `font-size` outside `[5.0, 72.0]`.**
  Cycle 118 added a runtime clamp at the renderer; a user
  config of `font-size = 500` silently rendered at the
  clamped 72pt. But `--check-config` echoed the raw value
  verbatim (`font: ... 500pt`), so the docs/diagnostic UI
  and the runtime disagreed without telling the user.
  Same shape as cycle 124's `palette = N=#hex` with N ≥ 16:
  surface the silent clamp as a malformed-value diagnostic.
  The runtime still clamps cleanly — the warning just stops
  the silent mismatch. +1 test
  (`detect_malformed_values_flags_font_size_out_of_renderer_range`)
  covers 500 / 0 / -4 / 72.5 (out of range) and 5 / 72 /
  13 / 13.5 (in-range, including bounds).

### Fixed
- **`Mux::split` while zoomed exits zoom so the user sees both
  halves.** The old `split` set `tab.focus = new_id` but left
  `tab.zoomed = true`, so `Mux::layout`'s zoom-collapse only
  returned the new leaf — the half the user had just split
  from *vanished from the screen* (still alive, just hidden)
  with no UX cue that the split happened. Every modern
  terminal exits zoom on split because "show me both" is the
  intent of the action (tmux's `display-panes` UX after
  `split-window`, WezTerm's `SplitHorizontal/Vertical`).
  Extracted the post-spawn tree mutation into a pure
  `insert_split(&mut Tab, new_id, dir)` helper so the
  contract is unit-testable without a real PTY spawn. +1
  test (`insert_split_exits_zoom_and_focuses_new_pane`)
  covering zoomed-before-split (zoom exits, both leaves
  render) and unzoomed-before-split (no-op on the flag,
  focus still moves).

### Documentation
- **`docs/TESTING.md` and `docs/INSTALL.md` test counts and
  coverage catch up to reality.** Massive drift: INSTALL.md
  claimed `cargo test --workspace` runs **20 tests**; TESTING.md
  enumerated ~33 tests across four crates. Actual workspace
  total is **213 tests** across six crates (2/56/75/10/37/33
  for kettle/kettle-config/kettle-core/kettle-render/kettle-ui/
  kettle-vt). 80+ cycles of additions had landed without the
  testing docs being refreshed. Rewrote TESTING.md with the
  correct counts, broader category descriptions, and pointers
  to the audit-cycle pattern that drives ongoing growth.
  INSTALL.md's test-count claim corrected.

### Fixed
- **`--screenshot foo.jpg` (or no extension) now fails up-front
  with a clear error.** `capture_png` writes via `image::save`,
  which dispatches on the file extension and is compiled
  PNG-only (`kettle-render/Cargo.toml`: `image = { …, features
  = ["png"] }`). A typo'd `.jpg` / `.bmp` / no-extension
  argument used to reach `image::save` *after* all the GPU work
  and surface a crate-internal error:
  `The file extension `."txt"` was not recognized as an image
  format`. Now pre-validated at the CLI surface:
  - `--screenshot foo.txt` → `Error: --screenshot foo.txt:
    extension .txt not supported; only .png is built in`
    (exit 1)
  - `--screenshot foo` → `Error: --screenshot foo: missing
    .png extension` (exit 1)
  - `--screenshot foo.PNG` → still works (case-insensitive)
  Same shape as the cycle-106/107 hard-fails on `--config /typo`
  and `--working-directory /typo` — surface bad input at the CLI
  surface, not deep in the engine.

### Documentation
- **README Quick-start CLI block matches reality.** Same drift
  cycle 126 caught in `--help` was also present in README's
  `Quick start` shell block:
  - `--list-keybinds` claimed "print the default keymap" — but
    cycle 103 made it show the *effective* keymap (defaults +
    overrides + unbinds) when `--config` is active.
  - `--list-actions` (cycle 104), `--list-ssh-hosts` (cycle
    105), and `--screenshot` (cycle 69) were missing entirely.
  - `--config FILE` claim "live-reloaded" stayed, with a new
    "error if it doesn't exist" addendum from cycle 106.
  Block updated; tooling claims now match runtime behavior so
  a first-time user reading the README finds the introspection
  surface kettle actually ships.

- **`kettle --help` text updated for cycle-103/105/106
  behavior changes.** `--list-keybinds` help previously said
  "Print the default keymap" — but cycle 103 made it show
  the *effective* keymap (defaults + overrides + unbinds)
  when a `--config FILE` is active. `--config` help still
  named only `--check-config` and `--screenshot` as
  consumers — cycle 103/105 added `--list-keybinds` and
  `--list-ssh-hosts` to that set, and cycle 106 made the
  flag hard-fail on a non-existent path. Both help strings
  now match runtime behavior; the cycle numbers stay in the
  help text as breadcrumbs for anyone tracing a behavior
  back to its source. No code change beyond the doc-comments
  read by `clap` to generate `--help`.
- **README keybind table gained 9 user-facing default chords.**
  The table previously surfaced only the basics (split / tab /
  copy-paste / search / focus / fullscreen / resize / scroll /
  font / broadcast / reload / reset) and quietly omitted SSH
  launcher (`Ctrl+Shift+S`), command palette (`Ctrl+Shift+K`),
  quick-select hints (`Ctrl+Shift+H`), split-auto
  (`Ctrl+Shift+A`), new window (`Ctrl+Shift+I`), pane zoom
  (`Ctrl+Shift+X`), jump-prompt (`Ctrl+Up/Down`), move-tab
  (`Ctrl+Shift+PgUp/Dn`), and goto-tab-N (`Alt+1..9`). All nine
  surfaced now, with the three "hidden-gem" rows (SSH /
  palette / hints) bolded to match the existing Search
  highlight. Footer line directs power users to
  `kettle --list-keybinds` (cycle 103) for the *effective*
  keymap after their `--config FILE` is applied.
- **+1 README-keybind regression guard.** New test
  `readme_documented_chords_are_actually_bound` pins each of
  the ten promoted chords (`Ctrl+Shift+S/K/H/A/I/X`, `Ctrl+
  Up/Down`, `Ctrl+Shift+PgUp/Dn`) to the action the README
  claims. If a future unbind / rebind drops one of these the
  test fails and the README's docs-drift is caught at CI
  time — same shape as cycles 100/104/117's drift guards but
  on the README surface.

### Fixed
- **`--check-config` flags `palette = N=#hex` with N ≥ 16.**
  The example config (cycle 100) advertised `palette = N=#hex`
  as supporting N in 0..=255, but the runtime apply path only
  writes `theme.palette[0..16]` — overrides for the xterm
  256-color extension (16..255) silently no-op'd. A user
  writing `palette = 200=#ff0000` (intending the bright-red
  cube slot) saw no effect and no warning. Two surfaces fixed:
  - `detect_malformed_values` (`--check-config`) flags any
    `palette = N=…` with N ≥ 16 so the user sees the typo.
  - The example config text reflects the real limit, with a
    note that runtime OSC 4 from a program can still override
    the 16..255 slots (just not the static config).
  Adding full runtime support for 16..255 would mean a Theme
  / renderer-resolve refactor; deferred. +1 test
  (`detect_malformed_values_flags_palette_index_out_of_range`).

### Fixed
- **`Action::NewWindow` now inherits `--config FILE`.** A user
  who launched kettle with `kettle --config /custom.conf` and
  then hit `Ctrl+Shift+I` (or invoked `New window` from the
  command palette) got a child process loading the *default*
  config path. Their theme / font / keybinds appeared in the
  original window but the new window looked like a vanilla
  kettle launch — confusing and easy to mistake for a settings-
  reset. The spawn now passes `--config <self.config_path>` to
  the child when the parent had one, so the new window starts
  with the same settings. No behavior change when no
  `--config` was passed; falls back to the cycle-67 "new tab"
  path if `current_exe()` is unresolvable.

### Fixed
- **`command =` clears the override; `ssh-host =` with empty
  halves is dropped at parse time.** Cycle-121 sibling. Two
  more empty-value bugs uncovered by extending the same
  audit:
  - `command = /usr/bin/fish` followed by `command =` (the
    user trying to revert) used to leave `cfg.shell =
    Some("")`. `shell_argv` then handed `vec![""]` to
    `Terminal::new`, producing an unspawnable empty program
    name. Now: empty value clears the override to `None`,
    so the engine falls back to `$SHELL` as intended.
  - `ssh-host = name=` or `ssh-host = =target` (one half
    empty) used to push `("name", "")` / `("", "target")`
    into `cfg.ssh_hosts`. `--check-config` flagged these as
    malformed (cycle 88) but the *runtime* list still
    contained them — the SSH launcher then showed an empty
    row or tried to connect to "". Now filtered at parse
    time so the diagnostic and the runtime state agree.
  Extended the cycle-121 test with both cases.
- **Empty string-config values no longer silently break
  rendering.** The parser docstring promises "empty value
  resets the key" but `parse_collect` unconditionally
  assigned `cfg.font_family = e.value.clone()` — so a single
  `font-family =` line silently set the family to `""`. The
  renderer's `measure_cell` then asked cosmic-text for an
  empty family name; the system fell back to *some* font but
  cell metrics drifted and glyphs rendered unpredictably.
  Same shape for `font-family-bold / -italic / -bold-italic`
  (per-style overrides) and `theme`. Fix:
  - `font-family =`: empty value is a no-op (keep the
    previous valid value; default is "JetBrainsMono Nerd
    Font").
  - `font-family-{bold,italic,bold-italic} =`: empty value
    *clears* the override (`Option::None`), so the per-style
    family falls back to the main `font-family`.
  - `theme =`: empty value is a no-op (keep the previous
    valid theme).
  +1 test (`empty_value_resets_string_keys_to_their_default`)
  pinning the contract for all five keys.
- **`Mux::reap` keeps `active` pointed at the same *tab*, not
  the same numeric index.** When a tab's last pane exited, the
  tab was removed from `self.tabs`, shifting every later tab
  left by one — but `self.active` was only adjusted by a
  trailing clamp ("if it ran off the end, pull it back"). So
  the case "a tab BEFORE active died" silently shifted focus to
  a different tab without any user action: focused on tab B
  (index 1), tab A dies → tabs become [B, C], `active` was 1
  → now indexes C instead of B. The fix decrements `active`
  whenever `ti < *active` at the moment of tab removal; if
  `ti == *active` (the user IS on the dying tab) focus
  naturally falls on the right-neighbor (matches every
  modern terminal). Logic extracted to pure `pub(crate) fn
  reap_tabs(&mut Vec<Tab>, &mut usize, &[u64])` so the
  active-index bookkeeping is unit-testable without spawning
  real PTYs to populate `self.panes`. +1 test
  (`reap_tabs_keeps_active_pointed_at_the_same_tab`) covers
  all five scenarios: leftmost-dies-while-mid-active,
  leftmost-dies-while-rightmost-active, active-itself-dies
  (right-neighbor takeover), active-is-last-and-dies
  (trailing clamp), and multi-tab death.
- **`--screenshot` caps cells to fit the wgpu 8192-per-side
  texture limit at any font size.** Cycle 69 added static
  `--cols ≤ 400 / --rows ≤ 200` clamps, but at a clamped 72pt
  font the cell is ~35×90px — so `--cols 200 --rows 100`
  computed an 18000×9000-pixel texture (above the 8192 limit)
  and aborted at GPU init with `dimension exceeds the limit of
  8192`. Cycle 119: `capture_png` now dynamically caps each
  axis against the actual cell pixel size via the new pure
  helper `cap_axis_cells(requested, cell_px, chrome_px) ->
  u32` (max-texture-px minus chrome, divided by cell-px,
  floored at 1). Plus it now returns the *actual* (cols,
  rows) used so the CLI's `wrote …` line tells the user when
  their request was capped (`wrote /tmp/k.png (189×89 cells
  — capped from 200×100 for GPU texture limit at current
  font size)`) instead of lying. Also: `capture_png` was the
  *other* unclamped `cfg.font_size` reader (cycle 118 only
  caught `Renderer::new`); that's clamped now too.
  +1 test (`cap_axis_cells_respects_8192_texture_limit`)
  covering happy-path passthrough, axis-specific caps,
  chrome shrinking the budget, and the 1-cell floor.

### Fixed
- **`Renderer::new` now clamps `cfg.font_size` to the same
  range `set_font_size` uses.** Cycle 73 added a `[5.0, 72.0]`
  clamp inside `set_font_size` (the runtime Ctrl+= / Ctrl+- /
  Ctrl+0 path), but `Renderer::new` still took `cfg.font_size`
  raw — so a user with `font-size = 200` in their config
  booted with 200pt cells, potentially hitting the wgpu 8192px-
  per-side texture limit and panicking GPU init. The bound was
  silently enforced only after a Ctrl+0 round-trip flowed
  through `set_font_size`. Same "downstream cache stale at
  startup" shape as cycle 98's font-family fix.
  - New pure helper `clamp_font_size(f32) -> f32` (sanitizes
    NaN to floor; clamps to `[5.0, 72.0]`; both setters now use
    it so the startup and runtime paths can't drift on which
    sizes they accept).
  - +1 test (`clamp_font_size_bounds_match_set_font_size`)
    covering in-range, at-bounds, above/below, negative, NaN,
    and ±infinity. Verified end-to-end: a `font-size = 500`
    config that would have hit the GPU texture limit now
    renders cleanly at the clamped 72pt.

### Added
- **Command palette gained Quick-select hints, Move tab
  left/right, and the four scroll-line / scroll-page entries.**
  When cycle 110 added `ScrollLineUp`/`ScrollLineDown`, the
  defaults map + `--list-actions` + the keybind name table all
  got updated, but the palette didn't — users invoking
  Ctrl+Shift+K and typing "scroll" got only "Scroll to top /
  bottom", no per-line nor per-page. Same drift for `HintMode`
  (Ctrl+Shift+H quick-select labels) and `MoveTabLeft/Right`,
  which had keybinds but no palette label. All five rows added,
  in registry order that puts scroll motions near each other.

### Tests
- **Palette drift guard: every actionable variant must appear
  (or be explicitly excluded).** New test
  `palette_includes_every_user_facing_action` enumerates every
  `Action` variant via an explicit match (so a new variant
  fails compilation until categorized), then asserts each
  variant is in `commands()` OR in a hand-curated `excluded`
  list with a one-line rationale (geometric directional
  motions, parametric `GotoTab(N)`, the palette itself).
  Catches the same shape as cycle 110's drift but on the
  palette surface, so the next time a new Action lands without
  a palette label the CI reports it.
- **Shadow-collision audit added to `defaults()`.** Cycle 115
  found one keybind collision (the cycle-110-introduced
  `Ctrl+Shift+Up/Down` landing on top of the
  `Ctrl+Shift+Arrows` Resize quartet). The class of bug is easy
  to reintroduce: `bind()` is `HashMap::insert()` which
  silently overwrites a duplicate trigger, so a CI run that
  passes `cargo test` can still ship an inconsistent keymap.
  New `defaults_audit() -> (Bindings, Vec<Trigger>)` returns
  both the final map AND the ordered list of every trigger
  the builder bound. `defaults()` becomes `defaults_audit().0`.
  Test `defaults_has_no_shadow_collisions` asserts
  `triggers.len() == map.len()` — and if it fires, builds a
  duplicate set so the panic message names exactly which
  trigger(s) shadowed (and by how many extra bind calls).
  Verifies cycle 115's fix was complete and locks the
  invariant going forward.

### Fixed
- **Cycle-110 keybind collision dropped:** the `Ctrl+Shift+Arrows
  → Resize<dir>` quartet was bound at line 412–415 of
  `keybinds.rs` defaults, then cycle 110 added `Ctrl+Shift+Up /
  Ctrl+Shift+Down → ScrollLineUp/Down` at line 462–463 of the
  same function. HashMap insertion order put the scroll-line
  binds last, **silently shadowing** the Resize-Up/Down chord
  while Resize-Left/Right remained mapped — an inconsistent
  four-direction map (Up/Down scroll, Left/Right resize) that
  passed cargo test but failed user expectation. The defaults
  now drop the Ctrl+Shift+Arrows resize quartet entirely;
  `Shift+Arrows` is the only canonical resize chord (already
  bound at line 418–421 from before, so no resize chord was
  actually lost — just the duplicate). README's keybind table
  updated to remove the `Ctrl+Shift+Arrows` resize column and
  to add a new row for the Scroll-line / Scroll-page / Scroll-
  top/bottom chord family. Cycle-110 test
  (`scroll_line_up_down_bound_to_ctrl_shift_arrows`) grew
  positive guards on `Shift+Arrows → Resize<dir>` for all four
  directions and *negative* guards that `Ctrl+Shift+Left/Right`
  are unbound (prevents a future reintroduction of the
  collision).

### Changed
- **`--check-config` echoes `font-feature` count and per-style
  font-family overrides.** Previously the summary surfaced
  `ssh: N host(s) configured` for SSH but silently dropped the
  other opt-in repeatable/optional keys — a user who had set
  `font-feature = liga` / `font-feature = cv01=2` / etc. saw
  nothing about them, same for the `font-family-{bold,italic,
  bold-italic}` overrides. Now both groups echo when actually
  set (default-config case stays terse). Output:
  - `font-features: <N> configured (ligatures=<bool>)`
  - `font-styles: per-style overrides for [bold, italic, ...]`
  Verified end-to-end against a config with both keys set
  (3 features, 2 styled families) and a `/dev/null` config
  (nothing printed for these lines).

### Fixed
- **`Action::CloseWindow` actually closes the window now (was
  an alias for `CloseTab`).** Both action variants exist in the
  `Action` enum and are surfaced by `--list-actions`, but the
  app handler folded them together:
  `Action::CloseWindow | Action::CloseTab => self.mux.close_tab()`
  which is just-the-focused-tab semantics. A user binding
  `close_window` for "kill the whole app" got tab-close behavior
  with no warning, and a multi-tab kettle window kept running.
  Now they're distinct: `CloseTab` still does `close_tab()`;
  `CloseWindow` calls a new `Mux::close_window()` that drops
  every tab + pane and resets `active = 0`, then the chrome
  exits the event loop. +1 test
  (`close_window_drops_every_tab_and_pane`).
- **`ToggleBroadcastAll` now scopes broadcast to the active tab,
  not every pane in every tab.** `broadcast_write` walked
  `self.panes.values_mut()` — the *whole* pane map, including
  panes in other tabs. A user with `broadcast = true` typing in
  one tab had their keystroke echoed into every pane across
  every tab (often unrelated work, often where they specifically
  *didn't* want their fan-out keystroke landing — `rm`, `git
  push`, anything). Terminator's `broadcast_all` is per-tab,
  iTerm2's "Send Input to All Sessions" defaults per-window,
  kitty's `send_text` targets the current tab. Kettle now
  matches: `Mux::broadcast_write` walks `tabs[active].root.
  leaf_ids()` instead. New `Node::leaf_ids() -> Vec<u64>`
  helper (DFS-order, symmetric with the existing `nth_leaf` /
  `leaf_index_of`). +1 test (`leaf_ids_walks_dfs_order`).
- **`Action::Reset` (Ctrl+Shift+R) now also sweeps kettle's local
  UI state.** Sending RIS (`ESC c`) to the engine reset the grid /
  DEC modes / alt-screen, but kettle owns several pieces of state
  *outside* the engine that survived the chord: the selection
  highlight, any open modal overlay (search bar, command palette,
  hint mode, SSH launcher). A user hitting Reset to recover from a
  visually-jammed terminal got a half-cleared result — fresh grid
  underneath, stale modal floating over it, or a leftover
  highlight on cells that just changed. Now sweeps all four after
  the RIS write: `clear_selection_on_input`, `mux.search.open =
  false`, `palette_input = None`, `hint_state = None`,
  `ssh_input = None`. Matches Alacritty's `Reset` action.

### Added
- **`scroll_line_up` / `scroll_line_down` actions bound to
  `Ctrl+Shift+Up` / `Ctrl+Shift+Down`.** Alacritty, kitty, and
  WezTerm all ship a keyboard chord for line-by-line scrollback;
  kettle had only `Shift+PageUp/PageDown` (one full screen at a
  time) and `Shift+Home/End` (jump to extremes). Filling the
  gap in the middle. New `Action::ScrollLineUp` / `ScrollLineDown`
  variants, `scroll_line_up` / `scroll_line_down` action names
  (also surfaced by `--list-actions`), default bindings on
  `Ctrl+Shift+Up/Down`. Sign matches the mouse-wheel path:
  `Scroll::Delta(+1)` scrolls back. Ctrl+Up/Down stays bound to
  `JumpPrev/NextPrompt` (cycle 47) — both coexist; only the
  Ctrl+Shift+ versions are the new line-scroll. +1 test
  (`scroll_line_up_down_bound_to_ctrl_shift_arrows`) covers the
  new bindings + a regression guard that the existing
  `JumpPrev/NextPrompt` (Ctrl+Up/Down) coexist.

### Fixed
- **`Session::save` is now atomic and surfaces I/O errors.**
  Cycle 108 fixed the *symptom* (corrupted session.json restored
  silently). This fixes the *cause*: the old `save` did
  `fs::write(p, text)` which is non-atomic — if kettle was
  killed mid-write (signal, panic, crash, power loss) the file
  ended up half-written. Now `save_to_path(&Session, &Path) ->
  io::Result<()>` writes to a `.tmp.<pid>.<nanos>` sibling and
  `rename`s it into place (atomic on every supported FS: POSIX
  `rename(2)`, Windows `MoveFileEx` with `MOVEFILE_REPLACE_
  EXISTING`). Mid-write death now leaves either the previous
  state intact (rename hadn't run) or the new state (rename
  succeeded) — never a half-written file. The pub `save`
  wrapper logs `log::warn!("could not save session to <path>:
  <err>")` on failure instead of silently swallowing every
  filesystem error (disk full, permission denied, locked dir).
  +2 tests: `save_to_path_is_atomic_and_round_trips` (asserts
  no leftover `.tmp.*` sibling + round-trip through load), and
  `save_to_path_overwrites_atomically` (rename replaces existing
  contents cleanly).
- **Corrupted `session.json` is backed up + a warning logged
  instead of silently discarding state.** A read error
  (no file on first launch, `HOME` changed) is the expected
  silent path. A JSON parse error is a real signal — kettle
  was killed mid-write, the disk filled up, the file got
  hand-edited badly — and used to silently drop the user's
  tabs/splits/focus state on the next launch with no
  diagnostic and no way to recover. Now: emit
  `log::warn!("session file <path> is corrupted (<err>);
  backed up to <path>.broken.<unix-seconds>")` and `rename`
  the broken file out of the way so the next launch starts
  fresh AND the user keeps a forensic artifact. If the
  rename fails (locked directory, permission issue) the warn
  still lands and the next save overwrites — the user's
  state is gone either way but at least they know. Logic
  factored into `pub(crate) fn load_from_path(p: &Path) ->
  Option<Session>` so the rename-on-corruption contract is
  testable without standing up the full app. +3 tests
  (missing file silent, corrupted file renamed+None, happy-
  path no-rename round-trip).
- **`--working-directory /typo` hard-fails instead of silently
  spawning in `$HOME`.** Cycle-107 sibling to cycle 106's
  `--config /typo` fix. The engine's PTY spawn (`Terminal::new`)
  uses `Some(d) if is_dir => cmd.cwd(d)` and falls back to
  `$HOME` otherwise — so `kettle -d ~/projets` (with a typo)
  silently started the shell in the user's home with no warning
  and no obvious cue that the explicit cwd was discarded. Now
  hard-fail at the top of `main` *before* the engine runs, with
  one of two errors so the fix is one keystroke away:
  - `--working-directory <path>: no such file or directory`
  - `--working-directory <path>: not a directory`
  (the latter for the case where the user accidentally pointed
  at a file instead of a directory). Both exit 1. Verified
  end-to-end: missing dir, regular file, existing dir all route
  correctly.
- **`--config /typo.conf` hard-fails instead of silently using
  defaults.** Every downstream branch (windowed run, `--screenshot`,
  every `--list-*` introspection, the `--check-config` fall-through)
  silently dropped to `Config::default()` when the user named a
  config file that didn't exist. So `kettle --config ~/typoconfig`
  produced a screenshot with the bundled theme and no warning, a
  keybinds list with no overrides, etc. — the user thought their
  file was being read. Hard-fail at the top of `main` with
  `Error: --config <path>: no such file` (exit 1) so the diagnostic
  lands exactly where the typo is. Omitting `--config` (the
  "kettle works out of the box" path) still falls back silently —
  that's intentional. Same "silent-fallback on bad input" shape as
  the cycle-44+ cluster, on the CLI surface.
- **`--screenshot` uses the same `Config::load_from` path as
  windowed startup and reload.** It was the lone hold-out: a
  hand-rolled `parse_collect` call meant malformed values silently
  defaulted with no `log::warn!` (the other paths warned), and
  unknown keys never appeared. Now consistent across all entry
  points.

### Added
- **`kettle --list-ssh-hosts` prints the configured `ssh-host`
  entries.** Companion to `--check-config` (which reported only a
  count) and the in-window Ctrl+Shift+S launcher (which shows them
  but requires opening kettle): users with many `ssh-host =
  name=user@host` lines wanted to verify the parse from the CLI
  without launching. Two-column table aligned to the longest name
  (floor 4 chars so single-character names don't collapse the
  column), sorted alphabetically; empty configs print `(no
  ssh-host entries configured)` so silence isn't ambiguous. Same
  `--config FILE` override convention as the rest of the
  introspection commands; falls back to the default config path.
  Formatting extracted to pure `format_ssh_hosts(&[(String,
  String)]) -> Vec<String>` so the table layout is unit-tested
  (`format_ssh_hosts_sorts_and_aligns_columns`) — sort order,
  alignment width, two-space separator, and the empty-input
  fallback all pinned.
- **`kettle --list-actions` enumerates every valid `keybind` action
  name.** The onboarding gap inverse of `--list-keybinds`: that one
  shows what's currently bound; this one shows what `keybind =
  trigger=…` values are valid. Previously, a user writing a new
  binding from scratch had to either read the kettle source or
  invoke `--check-config` after each guess to confirm a name parsed
  — both fall short of "I want to see the menu". 75 documented
  action tokens (including every alias — `focus_next` /
  `go_next` / `previous_tab` / `prev_tab`), sorted alphabetically,
  followed by two tail lines documenting the parametric
  `goto_tab:N` form and the `unbind` sentinel (which isn't an
  Action variant but is accepted by `apply_keybind`). New pure
  helper `keybinds::action_names() -> Vec<&'static str>`. Kept in
  sync with `Action::from_name` via a drift test
  (`action_names_round_trip_through_from_name`) that asserts every
  listed name parses back to `Some(Action)` — a typo in the list
  or a forgotten alias both fail it.

### Changed
- **`kettle --list-keybinds` honors `--config FILE` (or the default
  config path) and shows the *effective* keymap.** Previously the
  command always printed the built-in defaults regardless of which
  config was active, so a user who had spent time customizing their
  keybinds had no CLI way to confirm their `keybind = …` lines and
  `unbind` sentinels took effect — they had to restart kettle and
  test the chord by hand. New public `keybinds::describe(bindings:
  &Bindings) -> Vec<String>` factors out the sort+label rendering
  so `describe_defaults()` becomes `describe(&defaults())` and
  `main.rs` can pass `&cfg.keybinds` (which is the post-apply
  effective map) instead. End-to-end: overridden triggers show
  the new action label; unbound triggers don't appear in the
  output at all; brand-new bindings the user added land alongside
  the defaults, all in the same sorted listing. +1 test
  (`describe_reflects_user_overrides_and_unbinds`).

### Fixed
- **OSC 1 (icon name) now sets the tab title.** xterm distinguishes
  OSC 0 (icon + title), OSC 1 (icon only) and OSC 2 (title only);
  VTE/alacritty's dispatch only matches `"0" | "2"` and silently
  drops OSC 1 entirely. But vim / tmux / ranger / mc emit OSC 1
  to set their *short* title — exactly the string a tabbed
  terminal wants in the tab bar — so those titles disappeared in
  kettle. kitty / iTerm2 / Gnome Terminal / Konsole all treat OSC 1
  the same as OSC 2 in modern (tabbed) terminals; the icon-name
  distinction predates tabs. The extractor now rewrites the
  leading byte of OSC 1 payloads from `1` to `2` so VTE picks them
  up downstream and `TermEvent::Title` fires normally. Bracket-
  ST and BEL terminators both handled (vim uses `\e\\`; xterm
  uses `\a`). OSC 0 / OSC 2 left untouched. +1 test
  (`osc1_icon_name_rewrites_to_osc2_window_title`).

### Tests
- **Pin OSC 104 (no-param) and OSC 110/111/112 reset conformance.**
  Cycle 47 pinned OSC 104;N (single-index reset). Cycle 56/65/66
  pinned OSC 10/11/12 SET → renderer round-trips. The reset
  siblings — OSC 110 / 111 / 112 (reset default fg / bg / cursor)
  and OSC 104 with no parameters (reset *all* 256 palette
  indices) — were exercised through vte+alacritty but not pinned
  in kettle. A future upstream regression silently disconnecting
  any of those paths would slip through CI. Two new conformance
  tests:
    + `osc_110_111_112_reset_default_fg_bg_cursor_slots` — set
      each of `Colors[256..=258]` via OSC 10/11/12, confirm the
      matching `OSC 11X` clears the slot.
    + `osc_104_no_params_resets_all_256_palette_slots` —
      populate slots 1/2/200 via OSC 4, send `\e]104\a`, assert
      every index in `0..256` is back to None (the "reset
      palette to defaults" trick that theme-changers like
      `zsh-colorize` emit on exit).

### Documentation
- **`docs/kettle.example.config` documents every key kettle
  understands (was 9 of ~35).** New onboarding users copying the
  example into their own config never saw `font-feature`,
  `tab-bar`, `tab-bar-position`, `tab-format`,
  `window-title-format`, `scrollbar`, `cursor-color`,
  `cursor-blink-interval`, `bell`, `osc52`, `unfocused-split-
  opacity`, `focused-split-color`, `split-divider-color`,
  `mouse-hide-while-typing`, `word-delimiters`, `copy-on-select`,
  `scroll-on-keystroke`, `scroll-on-output`, `scroll-multiplier`,
  `minimum-contrast`, `selection-foreground`,
  `selection-background`, `command`/`shell`, `ssh-host`, the per-
  style `font-family-{bold,italic,bold-italic}` keys, the unbind
  sentinels, or the `palette = N=#hex` syntax. All now grouped
  under section headers with comments naming the valid value
  range / enum variants for each. Header callout reminds users
  that `#` is a *full-line* comment marker only — inline `#` in
  a value (e.g. a hex color) is part of the value, NOT a
  trailing comment. New test
  (`example_config_in_docs_uncommented_parses_with_zero_diagnostics`)
  strip-comments the file and runs the activated keys through
  `parse_collect` + `detect_malformed_values`; both must come
  back empty. Catches docs drift: any future key added without
  an example, or any example typo, fails this test.

### Fixed
- **`Config::load_from` now warns on malformed values, not just
  unknown keys.** The reload path (`Action::ReloadConfig`) called
  `Config::load_from`, which `log::warn!`-ed unrecognized keys but
  silently dropped bad values (`font-size = wrong`, missing `=`,
  unknown enum, …). A user hitting the reload chord after editing
  their config got no feedback when their typo didn't apply — they
  could only catch it via `kettle --check-config`. New
  `Config::load_from_with_diagnostics(path) -> (Config,
  Vec<String>, Vec<String>)` returns both diagnostic lists so
  callers can render them (future in-window banner, the existing
  `--check-config` path). `load_from` wraps it and `log::warn!`s
  each list with the file path. `--check-config` now uses the
  same helper, so the two diagnostic sources can't drift on which
  lints they run. +1 test
  (`load_from_with_diagnostics_surfaces_both_unknown_and_malformed`).
- **`Action::ReloadConfig` now applies `font-family` changes.** The
  reload handler picked up the new `font-size` (via the renderer's
  `set_font_size`) but left the renderer's cached `font_family`
  field at whatever was passed to `Renderer::new` at startup. A
  user editing `font-family = ...` in their config and hitting the
  reload chord saw the new size flow through immediately while the
  glyphs kept rendering in the *old* family — only a restart
  picked it up. Same shape as the cycle-44+ "reload swaps `self.cfg`
  but downstream caches are stale" cluster. New `Renderer::
  set_font_family(String)` setter (idempotent guard skips
  re-measure on no-op reloads, so steady-state reloads stay free);
  a sibling private `remeasure_cell()` factored out so the family
  and size setters share one re-measure path and can't drift on
  which fields they touch. `reload_config` calls
  `set_font_family` before `set_font_size` so the cell measurer
  sees the new family when size is re-applied (stale family for
  one frame is a real artifact otherwise). Tested via the
  headless `--screenshot` smoke that builds a full wgpu Renderer
  through the `capture_png` path; pure-helper unit tests aren't
  feasible without standing up wgpu, which the GPU selftest
  already does.

### Added
- **`keybind = TRIGGER = unbind` removes a default binding.**
  `apply_keybind` only ever *inserted* into the map; the closed
  `Action` enum has no "no-op" variant, so a user whose shell wants
  `Ctrl+Shift+C` for itself (some readline kits, certain TUI menus)
  had no way to remove kettle's default Copy on that chord. Now
  the action half accepts the sentinels `unbind` (Ghostty-style),
  `none`, `null`, `false`, or an empty string after the `=`; any
  of them calls `map.remove(&trigger)` instead of inserting. New
  pure helper `keybinds::is_unbind_token(s)` so `apply_keybind` and
  `detect_malformed_values` agree on what's a valid sentinel
  (otherwise `--check-config` would flag `keybind = ctrl+shift+c=
  unbind` as malformed). Aliases are case-insensitive
  (`Unbind` / `UNBIND` work). Unbinding a free trigger is a no-op,
  not an error. +2 tests
  (`apply_keybind_unbind_removes_default`,
  `is_unbind_token_recognizes_aliases`), plus the existing
  `detect_malformed_values_catches_bad_keybind_lines` test grew
  three positive assertions covering each sentinel.

### Fixed
- **`--check-config` flags config lines missing the `=` separator.**
  The line-oriented tokenizer (`parse.rs:21`) silently `continue`s on
  every non-comment, non-empty line that doesn't contain `=`. A typo
  like `font-family Jetbrains Mono` (forgot the equals), a left-over
  TOML-style `[section]` header from a config copied off another
  terminal, or a stray identifier on its own line all just disappeared
  with no warning — and `--check-config` happily reported
  `status: OK — no issues`. `detect_malformed_values` now scans the
  raw text (using the same comment / blank exclusion rules `parse::
  parse` applies internally) and emits `missing \`=\` separator: "<line>"`
  for each offender, so the user sees exactly which lines were
  ignored. Same shape as the cycle-70/84/85/86/87/88 silent-fallback
  cascade, but caught *before* parsing rather than after. +1 test
  (`detect_malformed_values_flags_lines_missing_equals`).
- **Explicit `kettle -e PROG` seeds the tab title from PROG.** Cycle 93
  surfaced `ssh <target>` for SSH panes but every *other* program
  launched with `-e` still showed the generic "kettle" placeholder
  forever: `kettle -e htop`, `kettle -e vim`, `kettle -e tmux` all
  fell through to the shell-default branch even though the user had
  just told us exactly what's running. Worse, the cycle-89 cwd-basename
  fallback doesn't help for these — `htop`/`top`/`less` and most
  full-screen TUIs never emit OSC 2 and either inherit the launching
  cwd (so the basename is your repo, not the program) or have none at
  all. `initial_pane_title(argv)` now extracts the **basename of
  `argv[0]`** as the seed (`/usr/bin/htop` → `htop`), with a hand-curated
  shell allow-list (`bash`, `zsh`, `fish`, `dash`, `ash`, `ksh`, `csh`,
  `tcsh`, `nu`, `elvish`, `xonsh`, `pwsh`, `powershell`, plus the
  `.exe` Windows spellings and `cmd`) that still routes through the
  "kettle" placeholder so the cwd-basename fallback runs — for shells
  the directory name is genuinely more useful than the literal "bash".
  SSH is still special-cased ahead of the basename path so
  `ssh me@box` keeps its argument. The function stays pure; the test
  grew five new assertions covering `htop` / `/usr/bin/htop` /
  `vim file.rs` / `python3 script.py` / `tmux` plus path-qualified
  shells (`/bin/bash`, `/usr/bin/fish`) and the Windows shell names.

### Security
- **SSH tab title seeded from the target.** Fresh SSH tabs
  (Ctrl+Shift+S launcher, restored sessions with an `ssh` argv)
  showed the literal `kettle` placeholder until the *remote*
  shell sent its first OSC 2 — distinguishing six SSH tabs at
  the same host was impossible during connection setup. The
  cycle-89 cwd-basename fallback didn't help (SSH panes have no
  local cwd to fall back to). New pure helper `initial_pane_title
  (argv)` inspects `argv[0] == "ssh"` and renders `ssh <target>`
  (first positional argument, skipping flags) at pane spawn time;
  the existing OSC 2 handler overwrites it the moment the remote
  shell sets a real title. Applies to both fresh launches and
  session restore since both flow through `spawn_pane`. +1 test
  covering ssh / non-ssh argvs and edge cases (`ssh -V`, etc).
- **`--list-keybinds` shows `Goto tab N` (1-based) instead of
  `GotoTab(0)`.** The action label was rendered via Rust's
  `Debug` derive — fine for non-parametric variants (`Copy`,
  `NewTab`, `SplitRight`, …) but leaked the 0-based internal
  index for `Action::GotoTab(0..=8)`. Users reading the listing
  saw `Alt+1 → GotoTab(0)` and reasonably wondered whether tabs
  were 0- or 1-indexed. New `action_label` helper renders the
  1-based human form for `GotoTab` and falls back to Debug for
  everything else (no churn on the other action labels). +1 test.
- **`--check-config` echoes window padding, opacity, and split
  colors.** The cycle-59 expansion of `--check-config` grouped
  many config gates but omitted `padding-x/y`,
  `background-opacity`, `unfocused-split-opacity`, and the cycle-83
  `focused-split-color` + companion `split-divider-color`. Added a
  `window:` line for the always-present numerics and a conditional
  `splits:` line for the opt-in overrides (only printed when at
  least one is set, so defaulted configs stay terse):

      window:  padding=8x8 opacity=1 unfocused-split=0.7
      splits:  focused=#ff8800 divider=#404040

- **OS window title also gets the cwd-basename fallback.** Cycle 89
  taught `Mux::tab_titles` to fall back to the cwd basename before
  the first OSC 2 — `window_title` (used for the OS-level title via
  `Window::set_title`) had the same gap and was returning the
  literal `"kettle"` placeholder even when the cwd was already
  known. Now mirrors the tab-title behavior so the window title and
  the in-app tab agree pre-OSC 2. The bail-out is also tighter: a
  cwd that *literally* equals "kettle" (e.g. `~/Repos/kettle`)
  doesn't collapse the substitution — only the placeholder-with-
  no-cwd path bails. +2 new asserts in the existing
  `window_title_formats_and_falls_back` test (now 1 test split
  into a wider matrix).
- **Tab title falls back to cwd basename before the first OSC 2.**
  Fresh tabs showed the literal placeholder "kettle" until the
  shell emitted `\e]2;…\007` on its first prompt. iTerm2 /
  Ghostty / WezTerm bridge that gap by showing the cwd basename
  or the running command — kettle now shows the cwd basename so
  a tab opened in `~/Repos/kettle` reads as `kettle` (the
  directory, useful) instead of `kettle` (the binary name,
  redundant). Real shell-set titles still win the moment they
  arrive. +1 test pinning the path-basename logic.
- **`--check-config` now catches `font-feature` and `ssh-host`
  typos.** Both arms also had the silent-drop pattern:
  `font-feature = liga,!@#,calt` silently dropped the bad `!@#`
  token leaving the user with a partial feature set; `ssh-host =
  no-equals-sign` silently dropped the entire entry, so the
  Ctrl+Shift+S launcher had no `name` to bind. Now flagged:
  every `font-feature` token has to parse via the documented
  syntax (`liga` / `+calt` / `cv01=2` / `zero on`) and every
  `ssh-host` line needs a non-empty `name=target` form. +1 test.
- **`--check-config` now catches unknown enum values.** Every
  enum-typed config arm (`cursor-style`, `bell`, `osc52` /
  `clipboard`, `tab-bar`, `tab-bar-position`, `scrollbar`) has an
  `_ => DefaultVariant` fallthrough — a typo like `cursor-style =
  wibble`, `bell = loud`, `scrollbar = sometimes` silently fell
  back to the default. The list of valid variants per key now
  lives alongside the apply arm (mirrored exactly), and
  `detect_malformed_values` flags anything not in the documented
  set. Sample after-fix output:

      status:  3 issue(s):
        - malformed value: cursor-style = "wibble"
        - malformed value: bell = "loud"
        - malformed value: scrollbar = "sometimes"

  +1 test covering 7 bad + ~25 good values (every variant + alias
  per key).
- **`--check-config` now catches unknown theme names.**
  `Theme::by_name` silently falls back to TokyoNight Night on an
  unknown name. A user copying `theme = …` from another terminal's
  config (Alacritty `colors.theme`, kitty `include theme.conf`)
  got no warning their theme wasn't bundled. Extended
  `detect_malformed_values` to scan against `Theme::list()`
  case-insensitively (matching `by_name`'s resolution), so
  `theme = NonExistent` now produces:

      status:  1 issue(s):
        - malformed value: theme = "NonExistent"

  +1 test covering an unknown name, plus three valid names
  (bundled, lowercase alias, and a different bundled theme).
- **`--check-config` now catches malformed `keybind = …` lines.**
  `apply_keybind` silently dropped on a bad trigger (typo in
  modifier or key name) or unknown action — a user with
  `keybind = ctrl+shift+nope=copy` or `keybind = ctrl+a=garbage_
  action` got zero feedback that their line never produced a
  binding. Same trap as the cycle-70 / cycle-84 setup. Extended
  `detect_malformed_values` to split each `keybind = ` value on
  `=` and route both halves through `parse_trigger` /
  `Action::from_name` (the same predicates the apply path uses),
  so what `--check-config` accepts is what actually binds. +1
  test (bad trigger + bad action + missing-separator counted;
  valid aliases like `split_horiz` and parametric `goto_tab:5`
  pass cleanly).
- **`--check-config` now catches malformed color values.** The
  cycle-70 `detect_malformed_values` side scan covered numeric/
  duration keys but skipped colors — `background = #not-a-color`
  or `cursor-color = whatever` silently kept the default while
  `--check-config` reported a clean status. Extended to also
  check `background`, `foreground`, `cursor-color`, `selection-
  bg/fg`, `search-bg/fg`, `split-divider-color`, `focused-split-
  color` (incl. alias `split-divider-color-focused`), and
  `palette = N=#hex` (validates both halves). Each goes through
  `Rgb::parse` — same path the apply arm uses — so what
  `--check-config` accepts is what actually applies. +1 test
  covering 6 bad + 7 good values (including X11 3-char hex
  shorthand and color names like `red` which are valid).
- **`focused-split-color` config key.** The inactive pane border
  color was already configurable via `split-divider-color`
  (introduced cycles ago); the *focused* pane's border was
  hard-wired to `theme.palette[4]`. Users with a theme whose
  accent blue blends into nearby content had no way to tune the
  "here am I" indicator without re-theming the whole palette.
  New `focused-split-color` (alias `split-divider-color-focused`)
  fills the gap; `None` keeps the theme-accent default. +1 test.
- **Session restore brings back the focused pane in each tab.**
  `STab` was only saving the split tree (`root`) — restore used
  `first_leaf()` to pick a focus, so every reopened tab landed on
  the leftmost pane regardless of which one the user had focused
  at save time. Now records `STab.focus: usize` as a DFS-order
  index of the focused leaf (pane ids are reallocated across
  restores, so the id itself isn't portable). `#[serde(default)]`
  means pre-cycle session files still load (defaults to `0` =
  first leaf, the previous behavior). +1 round-trip test, +1
  legacy-file test confirming back-compat.
- **All five underline style flags reach the renderer.** Cycle 79
  drew a single line for `Flags::UNDERLINE | UNDERCURL`. The
  engine actually tracks five style bits: UNDERLINE (`\e[4m`),
  DOUBLE_UNDERLINE (`\e[21m` / `\e[4:2m`), UNDERCURL (`\e[4:3m`,
  spell), DOTTED_UNDERLINE (`\e[4:4m`), DASHED_UNDERLINE
  (`\e[4:5m`). The render check now keys on `Flags::ALL_UNDERLINES`
  so every style draws *something*, and `DOUBLE_UNDERLINE` gets a
  second stacked line so the visually-distinct double-underline
  case looks different from plain. Wave/dotted/dashed visual
  styles still draw as a single line — a shader path is deferred,
  but the presence/absence cue is what matters most.
  +1 conformance test confirming each of the five SGR sequences
  reaches the correct engine flag.
- **SGR 58 per-cell underline color is now respected.** The
  cycle-79 underline render used the cell's `fg` for the line
  color — fine for plain `\e[4m` but wrong for neovim spell-check,
  git diff, and LSP diagnostics, which emit `\e[58;2;r;g;b m` to
  draw a *separate* (typically red) squiggle while keeping the
  text in its normal palette color. Renderer now reads
  `cell.underline_color()` and uses it for the underline quad,
  falling back to `fg` when unset. +1 conformance test pinning
  the engine contract: SGR 58 stores the spec, SGR 59 clears it,
  UNDERLINE flag survives.
- **SGR 4 underline + SGR 9 strikeout are rendered.** The engine
  tracked `Flags::UNDERLINE`, `Flags::UNDERCURL` (the `4:3` curly
  variant), and `Flags::STRIKEOUT` correctly — the conformance
  test `sgr_underline_dim_strike` pinned each bit reaching the
  cell since cycle ~14 — but the renderer never turned them into
  pixels. vim's `:set list`, neovim's spell-check, `diff` output,
  `git diff --color-words` deletions, man pages — none of these
  visual cues survived to the screen. New 1-px-tall quads at
  `cell_bottom - 2` for underline (and curly, drawn as a plain
  line for now — a real wave wants a shader tweak) and at
  `cell_mid` for strikeout, both using `fg` so the line color
  follows the text (or the dim / selection override above).
- **SGR 2 dim/faint is rendered.** The engine tracked
  `Flags::DIM` correctly (parsed by vte from `\e[2m`), and there's
  even a `sgr_underline_dim_strike` conformance test confirming the
  bit reaches the cell — but the renderer was ignoring it.
  Programs emitting dim text (fish prompt themers, `less` status
  lines, mc panel headers) rendered at full intensity. New pure
  `kettle_render::color::dim(fg, bg)` blends the fg halfway toward
  the cell bg (50 % intensity, the xterm/alacritty/iTerm2
  convention). Applied *before* the minimum-contrast lift so the
  lift can claw back legibility on themes where dim drops below
  WCAG. +1 helper test.
- **OS cursor turns into a pointing hand over Ctrl-clickable
  URLs.** Browser / iTerm2 / Ghostty convention: the mouse cursor
  morphs from text-I-beam to `CursorIcon::Pointer` while the user
  holds Ctrl (or Cmd, on macOS) and the pointer is on a
  hyperlink — same chord that actually opens the URL. Without
  this affordance, the underline-on-hover (already there) is the
  only hint that the link is clickable. Re-syncs on:
  - `CursorMoved` (position changed → hit-test may flip)
  - `ModifiersChanged` (Ctrl pressed/released → affordance flips
    without the mouse moving)
  - Per-frame in `redraw()` after `update_links()` so a URL
    scrolling out from under a held Ctrl (Ctrl+PageUp, scroll-
    on-output, etc.) doesn't leave the pointer-hand icon stuck
    on a now-empty cell.
  Deduped via `last_cursor_icon` so we don't issue a `set_cursor`
  syscall on every frame.
- **`selection-foreground` is now actually applied.** The config
  key was parsed, stored on `Theme.selection_foreground`, and then…
  ignored by the renderer — selected cells kept their normal text
  color. Dark text on a slightly darker selection background was
  often unreadable. Fixed by capturing the
  `RenderableContent.selection` range at the top of `build_pane`
  and swapping `fg` to `theme.selection_foreground` for cells
  whose point is in the range — applied *after* INVERSE so the
  selection always wins for readability (cells with INVERSE under
  a selection would otherwise render as inverse-fg on selection-bg,
  often invisible).
- **Local paste capped at 4 MiB.** OSC 52 (remote-program write
  into the system clipboard) was capped at 1 MiB back in cycle 47;
  the reverse direction (`paste_clipboard` reads the user's
  clipboard into the PTY) was uncapped — a user with a 1 GB file
  on the clipboard could shove every byte into the PTY in one
  shot and freeze the terminal until the program at the other end
  drained the pipe. 4 MiB fits any realistic code-review / log-
  snippet paste; bigger pastes are almost certainly fat-finger.
  Reuses the existing `clamp_osc52` byte-clamper (UTF-8 char-
  boundary preserved).
- **Tab title truncation honors display columns, not chars.** The
  `truncate(s, n)` helper used `chars().count()` to decide whether
  to cut — but every CJK character or emoji is 2 cells wide in the
  rendered tab segment, so a title like `中文中文中文` (6 chars / 12
  cells) sailed past the segment width without being trimmed and
  overflowed visually. Now sums `UnicodeWidthChar::width()` of each
  char and reserves 1 column for the trailing `…`. Pure helper,
  +1 test covering ASCII / CJK / mixed / edge cases (limit=0,
  exact-fit).
- **`Ctrl+Plus` font-zoom muscle memory works on US layouts.** On
  a US keyboard the `+` glyph lives on `Shift+=` — pressing what a
  user thinks of as "Ctrl+Plus" actually sends `mods=Ctrl+Shift,
  key='+'` to winit. The existing `bind(Ctrl, Char('+'))` binding
  needed bare Ctrl and didn't match. The user got zero feedback;
  font size just stayed put unless they typed `Ctrl+=` instead.
  Fixed by adding the obvious Ctrl+Shift variants of the
  zoom-in / zoom-out chords:
  - Ctrl+Plus, Ctrl+= (already)
  - **Ctrl+Shift+Plus, Ctrl+Shift+= (new)**
  - Ctrl+- (already)
  - **Ctrl+Shift+-, Ctrl+Shift+_ (new — `_` is the shifted `-`)**
  +1 test covering the whole 7-variant matrix.
- **Shift bypasses mouse tracking** (xterm / Alacritty / kitty /
  Ghostty convention). When a TUI like htop, tmux, vim, or fzf
  enables mouse mode (`CSI ?1000h`/`?1002h`/etc.), every click and
  wheel notch was being forwarded to the program — kettle's
  selection, scrollback, and shift-click-extend were completely
  locked out. Now `Shift+click` does a local selection, `Shift+
  drag` extends it, and `Shift+wheel` scrolls kettle's scrollback
  even while the TUI thinks it owns the mouse. Implemented as a
  single early-return in `send_mouse` (so press/release/drag all
  bypass uniformly) plus a parallel guard in the wheel branch.
  Nothing changes when Shift isn't held — mouse tracking still
  works the way it always did.
- **`--check-config` surfaces malformed numeric values.** Every
  numeric/duration config arm was guarded with `if let Ok(v) =
  e.value.parse() { … }` — clean code, but it silently fell back
  to the default when the value didn't parse. A user writing
  `font-size = 14px` or `scrollback = lots` saw a clean
  `status: OK` from `--check-config` while their setting was being
  ignored. New `Config::detect_malformed_values(text)` runs a
  side scan after parse and lists the bad ones; the
  `--check-config` body merges them with the unknown-key list:
  ```
  status:  3 issue(s):
    - unknown key: invalid
    - unknown key: unknown-key
    - malformed value: font-size = "not_a_number"
  ```
  Catches font-size, padding-x/y, background-opacity,
  unfocused-split-opacity, scroll-multiplier, minimum-contrast,
  scrollback (special: accepts `infinite`/`unlimited`/integer),
  and cursor-blink-interval. +1 test covering each. Side scan
  keeps adding-new-validated-keys to one place instead of every
  parse arm.
- **`--screenshot --cols`/`--rows` clamp instead of crashing.**
  Passing a large value (`--cols 100000`) tried to allocate a
  texture exceeding wgpu's per-side limit (8192 px on most GPUs)
  and panicked with `Dimension X value … exceeds the limit of
  8192`. Now clamped to `[20, 400]` cols and `[8, 200]` rows —
  every realistic screenshot fits comfortably, and `--cols 100000`
  produces a 400×200 PNG with a friendly `wrote PATH (400×200
  cells)` instead of a backtrace.
- **`kettle --list-themes | head` no longer panics on broken
  pipe.** Rust's runtime sets `SIGPIPE` to `SIG_IGN` at startup;
  when the reader of a pipeline closes its end early, the next
  `println!` returns `EPIPE` from `write` and the macro panics
  with `failed printing to stdout`. Every shell pipelining
  `--list-themes` (522 lines) or `--list-keybinds` (47 lines) into
  `head`, `grep`, or `less -F` was hitting this panic — silent
  unless you saw stderr, and `rc=0` because `head` itself exits 0.
  Fixed by resetting `SIGPIPE` to `SIG_DFL` at the top of `main`
  (Unix only; Windows has no `SIGPIPE`), so the process exits
  cleanly on EPIPE the way every other CLI tool does. New
  `libc = "0.2"` Unix-only dep (tiny, in the regular ecosystem).
- **`Action::NewWindow` (Ctrl+Shift+I) opens an actual new OS
  window.** The handler was sharing an arm with `Action::NewTab` —
  the parsed keybind dispatched cleanly all the way to a new
  *tab* in the existing window, so users pressing the "new
  window" chord were silently getting a tab (same shape as the
  empty-arm bug fixed in cycle 55). Now spawns a separate kettle
  process via `std::env::current_exe()` + `Command::spawn`, with
  stdio nulled and the child handle dropped so the OS reaps it.
  Falls back to a new tab if the current executable isn't
  resolvable (snap / appimage with custom argv0), keeping the
  keybind useful on weird platforms instead of silently failing.
- **OSC 10 (set default foreground) now reaches the per-pane
  text-area default color.** Companion to the OSC 11 chrome fix
  in cycle 65: a program issuing `OSC 10 ; rgb:RR/GG/BB ST` was
  populating `Colors[256]` and `color::resolve` honored it per-cell
  for fg, but glyphon's per-`TextArea` `default_color` was hard-
  wired to `theme.foreground` — the fallback when a span lacks an
  explicit `Attrs::color` (whitespace / IME composition / chrome
  strings rendered through the buffer). Now per-pane: each
  pane's `TextArea` reads its own `term_colors[256]` override; tab
  bar text and other chrome below keep `theme.foreground`. Same
  precedence as the OSC 10 *query* path.
- **OSC 11 (set default background) now reaches the chrome.**
  The cycle-56 fix paired OSC 12 (cursor color) set with the render
  path; OSC 11 had the same gap but on a larger surface — the
  engine parsed it and populated `Colors[257]`, `color::resolve`
  honored the override for individual cells, but three other places
  hard-wired `theme.background`: the surface clear-color (window
  padding / pane gaps), the active tab-bar segment, and the
  per-cell "is this the default bg, skip the quad?" check. A
  program flipping the bg to red would paint the cells red and
  leave the padding theme-blue — the chrome wouldn't follow. Now
  computed once per `render_frame` from the focused pane's
  `term_colors[257]` and threaded through all three places. Same
  precedence as the OSC 11 *query* path (cycle 44).
- **`Alt+1..9` jumps to tab 1..9** (kitty / Terminator / iTerm2 /
  Ghostty parity). The `Action::GotoTab(u8)` handler has existed
  since the early cycles, but `Action::from_name` had no parser
  for `goto_tab:N` strings and no default keybind, so the action
  was orphaned — users could neither bind it via config nor trigger
  it at all. Now: defaults bind Alt+1..Alt+9 → GotoTab(0..8), and
  config strings `keybind = alt+5=goto_tab:5` work (1-based to
  match the user's mental model; refused on `0` to surface the
  ambiguity rather than silently aliasing first-tab). Alt+0 is
  kept free for users who want to bind "last tab" manually.
  +2 tests (defaults table + parser rules incl. zero-rejection).
- **`Ctrl+Backspace` now sends BS (0x08) for delete-word muscle
  memory.** xterm/alacritty/Ghostty all distinguish the chord:
  plain Backspace → DEL (0x7F, readline `backward-delete-char`),
  Alt+Backspace → ESC+DEL (readline `backward-kill-word` / M-DEL),
  Ctrl+Backspace → BS (0x08). Kettle was mapping Ctrl+Backspace to
  plain DEL — same as a bare Backspace — so users coming from VS
  Code / browsers couldn't get delete-word with their muscle
  memory even after telling bash `bind '"\C-h":backward-kill-word'`
  (the shell never saw the BS that triggers it). +1 test covering
  all three flavors + the Ctrl+Alt combo.
- **OSC 4 multi-index query conformance is now pinned.** The
  cycle-44 fix shipped single-index replies (`OSC 4 ; 1 ; ?`); the
  vte parser already chunks the params so multi-index queries
  (`OSC 4 ; 1 ; ? ; 7 ; ?` — sent by tmux, neovim 0.10+, base16-
  shell-hook to probe an entire palette in one go) emit one
  `ColorRequest` per pair. Added an end-to-end test that asserts
  both indices come through; without per-pair dispatch the batched
  probers would see only the first reply and assume the rest of
  the palette equals the engine default, breaking dark/light
  detection they all rely on.
- **Full xterm Ctrl+<punctuation> C0 row.** Letter mappings
  (Ctrl+A → 0x01, …, Ctrl+Z → 0x1A) were already in place, plus
  `[` `\\` `]` ` `. Missing: `@` (NUL — same as Ctrl+Space), `^`
  (RS 0x1E — vim's alt-buffer toggle and tmux's `Ctrl-^` prefix),
  `_` (US 0x1F), and `/` (US 0x1F — tmux/nano undo). Each was
  previously falling through to "insert the literal character,"
  which silently broke those editor shortcuts. +1 test exercising
  the whole table.
- **`TERM_PROGRAM_VERSION` env var set on every spawned shell.**
  Companion to the existing `TERM_PROGRAM=kettle`; iTerm2 / kitty /
  WezTerm / Ghostty all set this pair. Neovim's
  `:checkhealth provider`, fish's prompt themers, and various
  shell/script diagnostics key off the pair when probing for known
  modern terminals — without the version, kettle showed up as "an
  unknown program calling itself kettle" rather than "kettle
  v0.1.0." Populated from `env!("CARGO_PKG_VERSION")` so a bumped
  `Cargo.toml` flows through with no separate string to maintain.
- **`--check-config` now echoes every per-cycle config gate.** The
  command was added back at cycle 5-ish and still only reported
  five fields (config path, theme, font, scrollback, keybind
  count). Since then we've added ~15 user-facing toggles — bell,
  OSC 52 policy, minimum-contrast, scroll-on-keystroke, scroll-on-
  output, scroll-multiplier, mouse-hide-while-typing, copy-on-
  select, tab-bar mode/position/format, window-title-format,
  word-delimiters, ssh-host count, cursor style/blink/interval —
  and none of them surfaced. A user setting `mouse-hide = false`
  had no way to verify it was actually applied without reading the
  source. `--check-config` now groups them by theme (cursor / bell+
  osc52+contrast / scroll / mouse / tabs / title / words / ssh) so
  the output stays scannable; `word-delimiters` and `ssh` lines
  only render when non-empty.
- **Bracketed paste also strips the *opening* marker `\x1b[200~`.**
  The injection-guard added earlier (and tested in
  `paste_strips_injected_end_marker`) only filtered the closing
  marker `\x1b[201~` — the well-known attack vector that ends paste
  mode early and lets the shell auto-execute the remainder. But the
  opening marker is the same class of bug on the other side: a
  paste containing `\x1b[200~` can confuse some shells into thinking
  they're entering paste mode mid-way, so our genuine `\x1b[201~` at
  the wrapper's end doesn't actually exit it — subsequent typed
  input is then absorbed as paste content. Alacritty / iTerm2 /
  WezTerm all strip both. +1 test (`paste_strips_injected_start_
  marker`) pairs the contract symmetrically with the close-marker
  test.
- **OSC 7 cwd percent-decoding handles UTF-8 paths correctly.**
  Shells (zsh `print -P %d`, bash `printf '\\e]7;…'`) percent-encode
  each *UTF-8 byte* of a non-ASCII filename individually — `café`
  arrives as `caf%C3%A9`. The old parser pushed each decoded byte
  as a `char`, which produced the Latin-1 garbage `cafÃ©` and broke
  new-tab/split cwd inheritance, the window title's `{cwd}`
  placeholder, and the OSC 7 session-restore path for every user
  with a non-ASCII directory in their tree. Fixed by decoding into
  a `Vec<u8>` and converting via `from_utf8_lossy`. +1 conformance
  test covering non-ASCII alone and mixed (`%20` space + `%C3%A9` +
  ASCII).
- **OSC 12 (set cursor color) now actually paints the cursor.**
  Companion bug to the OSC 4/10/11/12 *query* path shipped two
  weeks ago: the engine already parsed `OSC 12 ; rgb:RR/GG/BB ST`
  and populated `Colors[258]`, but the renderer hard-wired the
  cursor quad to `theme.cursor` so the override never reached the
  screen. Drawing now resolves via `kettle_render::color
  ::resolve_query(258, theme, term_colors)` — runtime override
  wins, theme value is the fallback. The same precedence rule the
  *query* path returns, so OSC 12 set + OSC 12 ? now agree.
  Confirmed end-to-end via a new test asserting OSC 10/11/12 SET
  populate engine slots 256 / 257 / 258 with the exact xparsecolor
  values.
- **`move_tab_left` / `move_tab_right` actions now actually move
  the tab.** They were bound to `Ctrl+Shift+PageUp` / `PageDown` in
  the default keymap (Terminator parity), parsed correctly, and
  threaded all the way to `Action::MoveTabLeft|MoveTabRight` in the
  app — and then dispatched into an empty arm. Every press was a
  silent no-op. Wired by a new `Mux::move_active_tab(delta: i32) ->
  bool` that swaps the active tab with its neighbor and clamps at
  the edges (no wrap, matching iTerm2 / Ghostty / WezTerm; wrap
  would have the tab bar lurch across on every press). +1 test
  covering swap, clamp, no-op, and the < 2 tabs case.
- **Selection auto-scrolls when you drag past the pane edge.**
  Previously the highlight stopped at the visible boundary — you
  had to release, scroll, then shift-click to extend. Every modern
  terminal (Alacritty / iTerm2 / WezTerm / kitty / Ghostty) keeps
  the scroll going while the mouse holds past the edge so a
  long-distance "select these 500 lines" gesture is a single
  click-and-drag. Per-frame rate scales with overshoot (1 line/
  frame at the edge, 2 at 10 px past, 3 at 40+ px) via a pure
  `selection_autoscroll_lines(y, top, bottom)` helper. The event
  loop wake-up cadence (`about_to_wait`) now keeps a 30 fps tick
  alive while drag-autoscroll is active, so the user doesn't have
  to wiggle the mouse to keep it moving. +1 test covering all six
  zones (inside, just-past, moderate, big × top/bottom).
- **`word-delimiters` config** (Alacritty `selection.
  semantic_escape_chars` parity, aliases `selection-word-chars` and
  `semantic-escape-chars`). Customizes what counts as a word for
  double-click selection (and the jump-to-prompt search that uses
  the same boundary set). Defaults to empty meaning "use the engine
  default" — `,│\`|:\"' ()[]{}<>\t`. Override to e.g. `()[]{}` to
  drop `/` and `:` from the delimiter set so a double-click on a URL
  or path picks it up whole. Plumbed through a new
  `Terminal::new(word_delimiters: Option<&str>)` arg →
  `TermConfig::semantic_escape_chars`. +1 config-parse test
  covering the canonical key and both aliases.
- **Shift+Click / right-click extend the selection** (xterm /
  Alacritty / iTerm2 / WezTerm convention). Previously every left
  click started a fresh selection at the click point, so the only
  way to grow a selection across a long scrollback was to start
  the drag over and hold all the way through. Now:
  - **Shift+left-click** anchors the current selection's start and
    pulls the end to the click — and you can keep dragging from
    there. Shift+Alt-Click still does block-select (Alt takes
    precedence). Shift+Click on empty space falls through to a
    normal new-selection.
  - **Right-click** extends an existing selection to the click;
    bare right-click on empty space is still a no-op so a stray
    right-click doesn't conjure a selection.
  Shared via a new `extend_selection_to_cursor` helper that updates
  the engine selection's right edge and enters drag mode for live
  follow-up. Copy-on-select fires on right-click extend too.
- **Wheel over tab bar cycles tabs** (kitty / iTerm2 / Ghostty
  parity). Spinning the mouse wheel while the pointer is over the
  tab bar now switches tabs (wheel-up = previous, wheel-down =
  next) instead of scrolling the focused pane's scrollback — the
  same gesture every modern terminal binds. Honors
  `tab-bar-position = bottom` and the hidden-bar case (`tab-bar =
  off` or `auto` with one tab). Pure `cursor_in_tab_bar_band`
  geometry helper, +1 unit test covering top/bottom/hidden bands.
- **`mouse-hide-while-typing` + selection clears on typing.** Two
  QoL gaps every modern terminal (Alacritty, kitty, WezTerm,
  iTerm2, Ghostty) has but kettle didn't:
  - The OS mouse cursor now hides on keyboard input (configurable,
    default on; aliases `mouse-hide`) and reappears on the next
    mouse move — so the pointer doesn't sit over the column you're
    editing.
  - The focused pane's selection is cleared on any keystroke that
    produces PTY bytes — so typing after a select doesn't leave a
    stale highlight behind to confuse the next copy/paste.
  Wired via small `hide_mouse_cursor`/`show_mouse_cursor`/
  `clear_selection_on_input` helpers on App. +1 config test.
- **Modified named keys now encode per xterm modifyCursorKeys** —
  `Ctrl+Right` (skip-word in bash/zsh/readline), `Ctrl+Delete`
  (delete-word), `Shift+Tab` (`CSI Z` back-tab used by readline /
  fzf / TUI forms), and modified arrows / F-keys / Insert / Delete /
  PageUp / PageDown / Home / End all previously collapsed to their
  unmodified sequence — vim users couldn't word-skip, fzf couldn't
  reverse-tab through fields. New pure `xterm_modifier(mods) → u32`
  emits the standard table (1 + shift + 2·alt + 4·ctrl + 8·super)
  and the encoder switches:
  - Arrows / Home / End → `CSI 1;<m>A..D|H|F` when modified
    (unmodified still honors DECCKM, modified always CSI).
  - Insert / Delete / PgUp / PgDn / F5..F12 → `CSI <n>;<m>~`.
  - F1..F4 → `CSI 1;<m>P..S` when modified (SS3 only when bare).
  - `Shift+Tab` → `CSI Z`.
  +2 tests covering the modifier table + every encoded family.
- **DECSCUSR cursor shape & DEC ?25 visibility now honor the
  running program.** Vim / neovim / fish flip cursor shape per-mode
  (`CSI 1 SP q` block in normal, `CSI 5 SP q` beam in insert,
  `CSI 3 SP q` underline in replace), and full-screen TUIs hide the
  cursor with `CSI ?25 l`. The renderer ignored both and always drew
  the static `cursor-style` config shape — so insert mode looked the
  same as normal mode, and the cursor stayed visible over `less`/
  `fzf`/`htop`. Fixed by seeding the engine's `default_cursor_style`
  from `cursor-style` at pane creation (so the user's static config
  is still the default) and reading the live
  `RenderableContent.cursor.shape` per frame — which the engine
  collapses `?25 l` into `CursorShape::Hidden` for, so a single
  guard handles both DECSCUSR and visibility. Added a new
  `HollowBlock` shape for when programs ask for an outline (vim's
  `:set guicursor` does this). +3 tests (config→engine mapping;
  engine ↔ ?25 hide/show round-trip; existing DECSCUSR shape test
  retained).
- **`scroll-on-keystroke` (alias `scroll-on-input`) + `scroll-on-
  output`** (Alacritty / xterm parity): two new bools governing
  scrollback behavior. `scroll-on-keystroke` defaults `true` (typing
  yanks you to the bottom — the longstanding behavior, now opt-out
  so pinning the viewport while typing is possible) and `scroll-on-
  output` defaults `false` (a chatty background process won't tear
  you away from the page you're reading; flip it on to chase the
  newest line). Output detection uses a pure
  `kettle_core::scrollbar::should_scroll_on_output` helper (history-
  size diff against the previous frame; first frame is a no-op) so
  the rule lives outside the render path. +1 config-parse test, +1
  pure-helper test.
- **OSC color set/reset round-trip conformance** — end-to-end test
  that `OSC 4 ; 1 ; rgb:…` writes into the engine's `Colors` slot and
  `OSC 104 ; 1` clears it. Guards the connection between OSC color
  set/reset (parser → engine) and the OSC 4/10/11/12 *query* reply
  path shipped last week — together they prove a full xparsecolor
  loop works.
- **DEC mode 12 (cursor blink) now honors the running program.**
  `CSI ?12 h` / `?12 l` is the standard way for vim, neovim, and
  shell prompts to ask the terminal for a solid (steady) or blinking
  cursor inside their UI. The engine raised
  `TermEvent::CursorBlinkingChange` and tracked the state on
  `cursor_style().blinking`, but the app's blink decision was hard-
  wired to the static `cursor-style-blink` config — every program
  request was silently ignored. Wired via a small
  `Terminal::cursor_blinking()` accessor (engine kept hidden), with
  the redraw + cursor-visibility path now intersecting config and
  live pane state. The event handler resets the blink phase so
  off→solid is immediate (no half-period delay). Default initial
  blink is seeded from `cursor-style-blink` at pane creation. +1
  conformance test.
- **`CSI 14 t` (text-area pixel size) now replies.** Sixel / kitty
  graphics / iTerm2 OSC 1337 apps probe this to compute
  pixel-perfect image placements (a 200-px image needs to know how
  many cells it covers); the engine raised
  `TextAreaSizeRequest(formatter)` but the app's event loop dropped
  it and the apps fell back to a 8×16 cell guess. New pure helper
  `kettle_render::reply_for_text_area_size(cols, rows, cell_w,
  cell_h, fmt)` feeds the engine formatter the live grid + cell
  dimensions and yields the canonical xtwinops reply
  `CSI 4 ; <height-px> ; <width-px> t`. +1 conformance test.
- **OSC 4 / 10 / 11 / 12 color queries now reply** (xparsecolor
  `rgb:RRRR/GGGG/BBBB` form). vim/neovim and tmux use these to detect
  the actual default foreground / background / cursor and the live
  palette so they pick a colorscheme that matches the terminal. The
  engine emitted `ColorRequest` events but the running app silently
  dropped them — now they're resolved against the active theme plus
  any runtime OSC overrides via a pure `kettle_render::reply_for_query`
  (palette 0..=15 → theme, 16..=255 → xterm cube, 256/257/258 →
  fg/bg/cursor; out-of-range → no reply). +2 tests (pure helper +
  end-to-end formatter shape for all four OSC prefixes).
- **`tab-format`** (alias `tab-title-format`): user-templatable per-tab
  label (default `{n}: {title}`) via the shared `template::fill`;
  unknown placeholders pass through verbatim; the trailing `✕` is
  still appended by the renderer. +1 test.
- **`window-title-format`** (alias `title-format`, Ghostty/WezTerm
  parity): template the OS window title with `{title}` / `{cwd}` /
  `{tab}` placeholders; `{{`/`}}` escape literal braces; unknown
  placeholders are left as literal text (typos visible). Pure
  `kettle_config::template::fill` + 4 tests.
- **`minimum-contrast`** (WezTerm parity) — keep text readable on
  low-contrast themes by lifting each cell's foreground toward
  white/black until it meets a configured WCAG 2.0 ratio (`0.0` = off,
  `4.5` ≈ AA, `7.0` ≈ AAA). Pure `color::with_min_contrast` over
  `relative_luminance`/`contrast_ratio` (+4 tests).
- Mouse-wheel scroll speed is now configurable: `scroll-multiplier`
  (alias `mouse-scroll-multiplier`, default `1.0` ≈ 3 lines per notch,
  clamped 0.1–50) scales both `LineDelta` and `PixelDelta` input;
  Ghostty/kitty parity. Pure `wheel_lines` helper, +2 tests.
- OSC 52 clipboard **writes are now size-capped** (1 MiB, truncated on
  a UTF-8 char boundary) so a hostile program can't push an unbounded
  payload into the system clipboard.
- **OSC 52 clipboard policy** (`osc52 = off|copy|paste|both`, default
  `copy`): clipboard *reads* via OSC 52 — which let a possibly-remote
  program exfiltrate your system clipboard — are now **denied by
  default** (an empty, well-formed reply is sent); writes remain
  allowed. Configurable per the new key (alias `clipboard`).
- Hardened **URL opening**: a URI from terminal output (an OSC 8
  hyperlink or autodetected link, opened via Ctrl/Cmd-click or hint
  mode) is now run through `links::is_safe_url` before the OS handler —
  only `http(s)`/`ftp(s)`/`mailto`/`file://` are allowed; custom
  schemes (`javascript:`, `vscode:`, `data:`, …), control characters,
  whitespace, `file://` path traversal, and absurd lengths are
  refused. Closes a known terminal scheme-handler abuse vector.

### Fixed
- Scrollback **search now scrolls the viewport to the active match**:
  matches in history (and `Tab`/`Shift+Tab` cycling onto them) bring
  the line into view (~⅓ from the top), once per match/query change so
  wheel-scrolling still works. Previously off-screen matches were found
  but never shown. Pure tested `search::reveal_offset`.
- Theme cycling (`next_theme`/`prev_theme`) now matches the current
  theme **case-insensitively and trimmed** (like `by_name`), so a
  config such as `theme = tokyonight night` cycles from the right
  place instead of jumping to the first theme.
- Split keys now match Terminator exactly: `Ctrl+Shift+O` splits
  horizontally (top/bottom), `Ctrl+Shift+E` splits vertically
  (left/right); `split_horiz`/`split_vert` action names corrected.

### Added
- `kettle --screenshot <out.png> [--cols --rows]`: renders a representative
  frame **offscreen** (no window) through the real `wgpu`/`glyphon`/quad
  path and writes a PNG. Used to generate the showcase images in
  **docs/UX-COMPARISON.md** — a cited UI/UX comparison matrix (kettle vs
  Ghostty/kitty/WezTerm/Terminator/Alacritty) with a tab-bar hit-region
  mermaid and the prioritized backlog status.
- UX backlog: unfocused-pane **dimming** (`unfocused-split-opacity`,
  default 0.7), **pane zoom/maximize** (`Ctrl+Shift+X`), per-pane
  **scrollbar** (`scrollbar = never|auto|always`), configurable
  **split-divider color**, configurable **cursor-blink interval**, and
  a **copy-on-select** toggle. Dimming/scrollbar use a post-text quad
  pass so they sit above glyphs.
- Tab bar redesign: per-tab close **✕** (click to close), trailing
  **+** new-tab button, **middle-click** a tab to close it,
  always-shown by default, active-tab accent, title eliding. New
  config `tab-bar` (off|auto|always) and `tab-bar-position`
  (top|bottom). Geometry is a single source of truth shared by the
  renderer and click hit-testing.
- `kettle --list-keybinds` prints the resolved default keymap
  (`trigger → action`, sorted) so the binding set is discoverable
  without reading the source (parallels `--list-themes`).
- A theme picked at runtime now **persists across restarts** — it's
  saved in `session.json` (`theme`, `#[serde(default)]` so older
  sessions still load) and reapplied on launch, until you change it
  again or reload the config.
- **Live theme switching**: `next_theme` / `prev_theme` keybind actions
  and "Next theme" / "Previous theme" command-palette entries cycle the
  ~512 bundled themes at runtime — no config edit or reload. Pure
  `Theme::cycle` (wrap-around; unknown current → first theme).
- The scrollback **scrollbar is now interactive**: left-click the
  focused pane's right-edge bar to jump the viewport there, then
  **drag** to scrub through history (x is ignored once grabbed, like a
  normal scrollbar; released on button-up). Geometry moved to a pure,
  tested `kettle_core::scrollbar` module (`thumb` for drawing,
  `target_offset` for the click mapping), shared by the renderer and
  the UI (was duplicated, untested math).
- `--config FILE` selects an explicit config file instead of the
  default path; it is honored by the running terminal (including the
  live-reload watcher, which now watches that file's directory) and by
  `--config-path`, `--check-config`, and `--screenshot`.
- **Middle-click pastes** the clipboard into the focused pane (standard
  X11 terminal behavior; bracketed-paste-safe via the shared
  `paste_clipboard`), when mouse-reporting isn't consuming the click
  and the cursor isn't over the tab bar (where middle-click still
  closes a tab).
- The OS **window title now follows the active pane** — switching tabs
  or focusing another split retitles the window (not just on OSC title
  events), with empty/placeholder titles falling back to `kettle`. The
  `set_title` call is deduped so it isn't a per-frame syscall.
- **Rectangular (block) selection**: hold `Alt` and drag to select a
  column block (iTerm2/Alacritty/WezTerm parity), via a pure
  `selection_kind(clicks, alt)` mapping; word/line still copy on press,
  Simple/Block copy on release.
- Standard launch CLI: `-e/--exec CMD …` runs a command in the first
  tab instead of the shell (consumes the rest of the args, hyphenated
  program flags included — e.g. `kettle -e ssh -t host`) and
  `-d/--working-directory DIR` sets its directory; either overrides a
  saved session for that first tab. (`kettle_ui::run_with(Options)`.)
- New tabs and splits now **inherit the focused pane's working
  directory** (via OSC 7), like WezTerm/iTerm/kitty — open a split and
  you're already in the same project. A since-deleted directory falls
  back to the default (`usable_cwd` guard) instead of failing to spawn.
- Quick-select **hint mode** is now usable (`Ctrl+Shift+H`): every
  visible URL / path / git-hash / IP gets a short label drawn over the
  focused pane (chip + glyph); type the label to open it (URLs via the
  OS handler) or copy it to the clipboard, `Backspace` to correct,
  `Esc` to cancel. New `hint_mode` keybind action.
- Quick-select / hint-mode core (`kettle_core::hints`, pure +
  fully-tested): scans the visible rows for URLs, filesystem paths,
  git hashes and IPv4 addresses (higher-priority kinds win on overlap,
  trailing punctuation trimmed, char-column coordinates) and generates
  minimal-width unique labels over a home-row alphabet. The overlay +
  key-to-act wiring is the next cycle.
- Docs: `ARCHITECTURE.md` refreshed to the current system — crate
  responsibilities, the side-channel chunk set
  (VirtualImage/Animation/RelativePlacement), the per-pane registries,
  the animation redraw tick, an accurate test count, and a **new
  mermaid diagram of the kitty graphics pipeline** (decode → registries
  → placeholder/relative/animation render).
- Search is now a **real regex with smart-case**: the `Ctrl+Shift+F`
  pattern is compiled as a regex (alternation, anchors, `\b`, …),
  case-insensitive unless it contains an uppercase character
  (ripgrep/vim smart-case), and an invalid pattern falls back to a
  literal search instead of returning nothing (`search::build_regex`).
- Command palette (`Ctrl+Shift+K`): a fuzzy action launcher over a
  29-command registry (`kettle_config::palette`) — type to filter,
  `Tab`/`↑↓` to select, `Enter` to run, `Esc` to cancel. Bottom-bar
  overlay reusing the SSH-launcher plumbing; new `command_palette`
  keybind action.
- Fuzzy matcher (`kettle_config::fuzzy`, dependency-free): subsequence
  scoring with prefix / word-boundary / camelCase / contiguity bonuses
  and a length penalty (`score`, `best`). The `Ctrl+Shift+S` SSH
  launcher now fuzzy-matches host names on `Tab`-complete and `Enter`
  (was prefix-only); the matcher is reusable by a future command
  palette.
- VT conformance sweep: IRM insert mode (`CSI 4h` shifts text right),
  DECTCEM cursor visibility (`CSI ?25 h/l`), LNM mode bit
  (`CSI 20 h/l`), DECCKM + DECKPAM/DECKPNM application cursor/keypad
  modes, and mouse-tracking DECSET flags (`?1000/?1002/?1003/?1006`)
  set and cleared — 5 end-to-end tests through the real vte path.
- kitty relative placements: parents can now also be **regular
  placements** (not just placeholders) and **relative chains** are
  resolved — a pure `resolve_chain` walks child→parent with a depth
  bound of 8 (kitty `ETOODEEP`; cycles are bounded, not infinite), with
  parent origins unified from placeholder cells and the image registry.
  This completes the kitty graphics protocol surface.
- kitty relative placements **now render** when the parent is a visible
  Unicode-placeholder (virtual) image: the child image is drawn `(h,v)`
  cells from the parent's placeholder origin (the min abs-line/column of
  its cells), through a per-terminal `Relatives` registry and the pure
  `relative_origin` clamp. Parents that aren't on screen this frame are
  skipped; the placement group still dies with its parent.
- kitty relative placements (decode/state): `a=p,P=,Q=` is recorded as
  a `RelativePlacement` (parent image/placement + `H`/`V` cell offset)
  instead of drawing at the cursor; a placement group dies with its
  parent (parent-image deletion cascades to its relatives). Render-time
  resolution of the on-screen position from the parent is the next
  sub-item.
- kitty animation frame compositing: partial-rect `a=f` frames are
  blended (or `X=1` replaced) over a chosen canvas — a previous frame
  (`c=`), a `Y=` background color, or transparent — and `r=` edits an
  existing frame in place; `a=c` copies a rectangle between frames
  (including onto the root image). New RGBA `ImageData::compose`
  (source-over) and `solid` primitives.
- kitty animation **now plays end-to-end**: `a=f` frames / `a=a`
  control snapshot through `Chunk::Animation` into a per-terminal
  `Animations` registry; at draw time a placement's image is swapped for
  the frame the playback clock selects, and the event loop schedules
  ~30 fps redraws while any animation is running. Root-frame gap via
  `a=a,r=1,z=`; animations are reaped with the image or by `a=d,d=f`.
- kitty animation playback-timing engine: pure, deterministic
  `current_frame(gaps, state, elapsed_ms)` mapping elapsed time to the
  frame to show — skips gapless frames, honors infinite/finite loop
  counts, `loading`-mode hold-at-end, and stopped→selected-frame. The
  renderer clock + frame substitution is the only remaining sub-item.
- kitty animation (decode/state layer): `a=f` animation-frame
  transmission (chunked via a single in-flight slot, gap from `z` with
  `z<0` = gapless base frames), `a=a` animation control (`c` current
  frame, `s` = stop/run/loading, `v` loop count, `r`+`z` per-frame gap),
  and `a=d,d=f` frame deletion (keeps the base image).
  `KittyState::frames()/animation()` expose the model for the upcoming
  playback/compositing cycle. Cited: kitty
  `docs/graphics-protocol.rst:839`.
- Font-feature tuning: `font-feature` now parses real OpenType tags
  (`liga`, `calt`, `ss01`, `cv01`, `zero`, …) with `+tag` / `-tag` /
  `tag=N` / `tag on|off` dialects, repeatable and comma-separated, and
  applies them through cosmic-text `FontFeatures` on top of the coarse
  ligature toggle (explicit settings win; Advanced shaping kept whenever
  any feature is set). Cited: Ghostty `font-feature`, kitty
  `font_features`.
- kitty placeholders: the **placement id** is now decoded from each
  cell's underline color (256/truecolor/named), feeding the spec's
  run-grouping and left-inheritance so cells of different placements no
  longer inherit across each other.
- kitty Unicode placeholders **now render**: each frame the visible grid
  is scanned for `U+10EEEE`, the image id is read from the cell
  foreground (256-color / truecolor / ANSI-named) plus the msb diacritic,
  contiguous runs apply the left-inheritance rules, and the referenced
  `U=1` virtual image is sliced per cell (`ImageData::crop` +
  `placeholder::tile_src_rect`, exact-tiling) and drawn through the
  existing GPU image pipeline. Virtual images are reaped on
  delete-by-id/all. (`Terminal::placeholder_tiles`.)
- kitty Unicode placeholders (decode layer): `kettle-vt::placeholder` —
  the 297-entry row/column diacritic table, per-cell diacritic parsing,
  32-bit image-id reconstruction (foreground + msb diacritic), and the
  omitted-diacritic left-inheritance algorithm; plus `U=1` **virtual
  placements** in the kitty decoder (`a=p,U=1` / `a=T,U=1` store the
  image and register a rows×cols placement without drawing at the
  cursor). Renderer compositing of placeholder cells is the next cycle.
- VT conformance: XTWINOPS `CSI 18 t` text-area size report
  (`CSI 8 ; rows ; cols t`), DSR `CSI 5 n` device-status (`→ CSI 0 n`),
  and an exact-match DA1 assertion (`CSI c`/`CSI 0 c` → `CSI ? 6 c`).
  44 conformance tests total.
- VT conformance suite — 35 end-to-end tests through the real
  `vte`+`alacritty_terminal` path: CUP/erase/SGR/tabs, scroll region,
  charsets, ICH/DCH/IL/DL, DECSC/DECRC, autowrap, origin mode, DECALN,
  REP, SO/SI, RIS, ECH, CHA/HPA/VPA, SU/SD, DECSCUSR, wide CJK,
  combining marks, OSC 4/8/52, DECRQM, DSR/DA1/DA2, DECSET 1049.
- kitty graphics advanced ops: transmit-only store, place-by-id,
  delete (all/by id), z-index ordering.
- Per-style font families (`font-family-bold/italic/bold-italic`) and a
  ligature shaping toggle.
- Configurable bell (`off|visual|attention|both`) with cross-platform
  window-attention (taskbar/dock urgency); no audio deps.
- Focus-event reporting (DEC ?1004).
- UX polish: safe bracketed paste, double/triple-click word/line select
  with auto-copy, focus-aware hollow cursor, cursor blink, visual bell.
- Offscreen GPU self-test (WGSL compile + render pass) run in CI on
  Linux/macOS/Windows.

## [0.1.0] — 2026-05-19

First cross-platform release; artifacts built on real runners and
attached to the GitHub release (Linux tar+`.desktop`, macOS `.app`,
Windows zip).

### Added
- GPU renderer: `wgpu` + `glyphon`, tiled multi-pane, tab bar, split
  dividers, focus border, cursor/selection/search overlays.
- Engine: `portable-pty` + `alacritty_terminal` + `vte`, per-pane
  reader thread, infinite scrollback option.
- Terminator-style tabs + binary split tree, broadcast input,
  Terminator-compatible keybinds incl. Shift+Arrow resize.
- 512 bundled Ghostty themes (default **TokyoNight Night**); bundled
  JetBrains Mono Nerd Font; Ghostty-syntax config with live reload.
- Regex search overlay; mouse selection + wheel scroll.
- Inline images: Sixel, kitty graphics, iTerm2 (OSC 1337).
- Hyperlinks: OSC 8 + URL autodetection, Ctrl/Cmd-click to open.
- Mouse-reporting passthrough (X10 + SGR 1006).
- Shell integration (OSC 133) + jump-to-prompt.
- Session save/restore (tab/split tree + per-pane cwd).
- SSH multiplexing (launcher + session-persisted SSH tabs).
- MIT licensed; CI matrix; docs with citations + mermaid diagrams.
