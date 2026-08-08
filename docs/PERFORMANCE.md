# Performance

## Unreleased — macOS comparator, first measured standing

The first macOS comparison Kettle has ever had. Windows and Linux comparator
legs existed; macOS did not, so no claim about Kettle's standing among macOS
terminals could be checked. `scripts/perf/macos-compare.sh` (`just macos-perf`)
closes that gap.

Host: **Apple M5 Max, 18 cores, 48 GB, macOS 26.6.1 (build 25G76, Darwin
25.6.0)**. 7 runs with 2 warmups per timing workload; Kettle built `--release`.
Every peer launched by an empirically verified command form; peers that could
not be driven are recorded as skips with reasons rather than dropped.

Peer builds measured, since a benchmark without them is not reproducible:

| terminal | version |
|---|---|
| Alacritty | 0.17.0 (94e7c88), Homebrew cask |
| kitty | 0.48.2 |
| WezTerm | 20240203-110809-5046fc22 |
| Terminal.app | macOS 26.6.1 system build |
| Ghostty, iTerm2 | not measured — see the skips below |

Measured against the **binary actually being released**, not an earlier commit.
The updater carried a production change after the first measurement, so rather
than argue it could not touch these paths, the comparator was simply re-run.

| metric | kettle | rank | field |
|---|---|---|---|
| startup | 0.336 s | **3 / 5** | wezterm 0.228, alacritty 0.233, **kettle**, kitty 0.551, terminal 1.017 |
| ascii flood | 0.339 s | **3 / 5** | alacritty 0.228, wezterm 0.233, **kettle**, kitty 0.662, terminal 1.102 |
| ansi/underline flood | 0.559 s | **3 / 5** | alacritty 0.450, wezterm 0.450, **kettle**, kitty 0.871, terminal 1.264 |
| max RSS | **105.9 MiB** | **1 / 4** | **kettle**, wezterm 111.9, kitty 122.3, alacritty 127.4 |
| idle CPU | **0.00 %** | **1 / 5** | **kettle**, alacritty, kitty, terminal, wezterm — all 0.00 % |

**Top-half on 5 of 5 eligible metrics**, and outright first on two of them.

Every number above comes from one run of `just macos-perf`, so the table is
reproducible as a whole rather than assembled from several.

### One rank moved when the peer binary changed, and it is worth saying why

An earlier run recorded the ansi/underline flood at **2 / 5**, with Kettle at
0.543 s ahead of Alacritty's 0.548 s. That run could not use Alacritty's
Homebrew cask — it would not open a window — so it measured a build made from
source. The cask now launches (0.17.0) and posts **0.450 s** on the same
workload, which puts Kettle third rather than second.

The honest reading is that the earlier second place was an artifact of the peer
build, not a Kettle regression: Kettle's own figure moved only 0.543 → 0.559 s
across the two runs, well inside the spread of a machine that is not idle. The
table above uses the cask, because that is what a user comparing terminals on
macOS would actually install.

### The memory result overturns what the Windows numbers implied

`docs/PERFORMANCE.md`'s Windows section records Kettle at ~335 MB against
Alacritty's ~148 MB, and attributes ~182 MB of it to the DX12 shader compiler
being loaded and run for the first time — "not an allocation Kettle makes."

That attribution predicted the gap would not transfer to Metal. It did not.
On macOS Kettle is the **lightest** terminal measured, at 105.9 MiB against
WezTerm's 111.9, kitty's 122.3 and Alacritty's 127.4. The same binary, the same
workload, the opposite ranking — because the cost was never Kettle's.

Three separate comparator runs agree on both the value and the ordering: 103.7,
103.9 and 105.9 MiB, Kettle first every time. The spread across runs is smaller
than the gap to second place, so this is not a single lucky sample.

### Idle CPU was the one loss, and finding out why was the point

The first run measured Kettle at **2.60 %** while every competitor measured
**0.00 %** — the only metric it lost, and a regression against its own 0.0000 %
on Windows. The comparator existed to surface exactly that.

The cause was a feedback loop Kettle created against itself, not a busy timer.
Cursor blink — the obvious suspect — was ruled out by measurement: disabling it
still read 2.6 %, while disabling only the remote-command watcher read 0.0 %. A
symbolized sample gave the chain:

    user_event -> poll_pending_remote_commands -> claim_remote_command_batch
      -> open_private_file -> fchmod

The private-file helper called `fchmod(0600)` unconditionally, **including when
the file was already 0600**. macOS FSEvents reports that no-op chmod as both
`INODE_META_MOD` and `ITEM_MODIFIED`, so the watcher woke for the event it had
just caused — hundreds of self-events per second on an idle window.

Hardening now runs only when the mode is not already exactly `0600`; every
ownership, regular-file, hard-link and special-permission check is unchanged.

    before: 2.8, 2.7, 2.6, 2.6, 2.4 %   median 2.6 %
    after:  0.1, 0.0, 0.0, 0.0, 0.0 %   median 0.0 %

A live `--remote-send` is still consumed in 20.3 ms, so responsiveness was not
traded for the idle number. The table above is the re-run after this fix.

### What this measurement does NOT say

- **Input latency was not measured.** Driving keystroke-to-paint across
  AppleScript-only terminals is its own problem and is deliberately out of scope.
  Its absence is not a pass. This matters, because input latency is Kettle's
  strongest published result on Windows and it is unverified here.
- **Ghostty is missing from every metric.** It measured fine during harness
  development, then stopped launching a window on this machine entirely — it
  answers `--version` but `open -a Ghostty` will not start it. That is a peer
  environment fault, not a Kettle or harness fault, but it removes Kettle's
  closest architectural peer from the field and the numbers above are weaker for
  it. Re-run on a machine with a healthy Ghostty before treating the ranking as
  settled.
- **iTerm2 is missing.** Its AppleScript interface times out on this machine
  (AppleEvent −1712) through `create window`, `write text`, and `open -a` alike.
- **Terminal.app has no RSS figure**, because `/usr/bin/time -l` would measure
  `osascript` rather than the detached process it spawns.
- **Alacritty is the Homebrew cask, 0.17.0, and it is ad-hoc-signed.**
  `codesign -dv` reports `flags=0x2(adhoc)`, and Homebrew has deprecated the cask
  for exactly that Gatekeeper failure mode, disabling it on 2026-09-01. During
  harness development it would not open a window at all and had to be built from
  source; it launches now. That instability is the reason the ansi-flood rank
  differs between runs, and it is worth noting that the failure mode Alacritty is
  living with here is precisely the one PR #156 exists to prevent for Kettle.
- The machine was not idle. Apple's `MediaAnalysis` and `replayd` daemons were
  active throughout. Kettle's own run-to-run variance was small (startup stddev
  ~0.003 s), and all peers shared the same conditions, but these are not
  quiet-machine numbers.

A metric counts only when Kettle **and at least one real competitor** were both
measured, so the harness cannot certify a standing it did not actually measure.
Five of five metrics were eligible here; none were excluded for lack of a
competitor.

## Unreleased — context-menu interaction latency

Context-menu row hover used to take the full frame path: every pointer crossing
locked and copied all visible terminal grids, ran terminal maintenance, and
forced both glyphon text renderers to prepare because any open overlay marked
the whole text frame dirty. On a high-DPI 5120x2160 desktop that work sits
directly on the input-to-present critical path.

Menu-only redraws now validate and reuse the pooled pane snapshots by pane id,
output generation, grid dimensions, and order. The fast path performs no
terminal mutex acquisition or viewport copy; a cursor-blink bit captured in the
snapshot avoids hidden locks in both overlay construction and the event-loop
blink scheduler. Opening a menu cancels any pointer gesture that could otherwise
change terminal selection or scrollback without advancing output generation.
Full non-output frames also no longer lock every pane merely to poll scrollback
depth: tab activity and `scroll-on-output` follow the reader's lock-free output
generation. The terminal grid is locked only when that opt-in scroll behavior
must move a pane after real output, and in-place/alternate-screen updates now
count correctly instead of being invisible to a history-growth proxy.
The renderer hashes menu text and layout separately from its highlighted row,
so hover reuses the main, menu, and stable block-cursor text vertices. It still
walks the cached cells and rebuilds/uploads the frame's quad batches; this is a
snapshot-and-text fast path, not a retained scene graph. Output, resize, pane
reorder, or an active gesture fails closed to the full path; menu text, theme,
enabled-state, anchor, and scroll-window changes force text preparation without
recapturing an otherwise-current terminal snapshot. Cross-terminal
frame-latency numbers belong to the machine-local benchmark artifact and are
not claimed by portable unit tests. Menu width is measured in Unicode display
columns and truncated at grapheme boundaries. Drawing, pointer hit-testing, and
agent geometry use one clamped panel height, so a partially clipped row cannot
be activated through otherwise blank bottom pixels. Pointer hit-testing streams
row kinds instead of allocating a temporary vector, and wheel-scroll clamping
computes the final fitting suffix in one reverse pass. Both remain O(items) for
the 512-entry theme submenu.

The remote-context poll submits the newest roots for all panes and windows to
one background worker. A single-slot wake plus replaceable pending/result state
coalesces bursts; process enumeration and procfs I/O never run on the winit
thread, and a completion event repaints affected idle windows. Per-pane
detection reuses scanner-owned BFS queue and visited storage. Linux follows
bounded `task/*/children` edges (including non-leader thread children), charges
all attempted reads to a 4 MiB aggregate ceiling, and caps nodes, task files,
argv count, decoded argv bytes, and scan time (25 ms). Oversized, budget-limited,
or deadline-limited scans never replace the last complete applied state. It
reads `/proc/<pid>/cwd` only for the selected local foreground pid; detected
remotes, direct nonlocal clients, and nested WSL sessions never install a
misleading host cwd. Split/duplicate reads the latest foreground-shell result
from this cache instead of scanning on the input path.

## Unreleased -- comparator smoke, four terminals

Not release evidence, and the reasons are recorded rather than implied: a
position-only rotation instead of a Williams square (`-AllowUnbalanced`), four
terminals instead of six, and another session's Kettle running throughout
(`-AllowForeignTerminalInstances`, PIDs in the manifest). Windows Terminal was
excluded because it is single-instance and a session was open; Rio was excluded
because it fails startup readiness on its SECOND launch of a run, reproducibly,
for reasons not yet understood. Read these as a smoke reading on a busy machine,
not as quiet-machine numbers.

Medians, eight samples per terminal, zero misses. Startup is end-to-end: launch
until a child shell has run inside the terminal and its painted marker is on
screen, so it includes a constant child-startup cost common to all four.

| metric | kettle | alacritty | wezterm | tabby | kettle's rank |
|---|---|---|---|---|---|
| input latency, median | **68.59 ms** | 94.23 | 135.01 | 76.88 | **1 of 4** |
| input latency, p95 | **124.06 ms** | 175.16 | 204.17 | 200.47 | **1 of 4** |
| input latency, p99 | **139.09 ms** | 197.52 | 244.72 | 225.52 | **1 of 4** |
| idle CPU | **0.0000%** | 0.00 / 0.26 | 0.51 / 0.00 | 0.25 / 0.51 | **1 of 4** |
| startup (see below) | 6883 / 5652 ms | 4995 / 3463 | 4637 / 3408 | 15737 / 11827 | 3 of 4 |
| fresh working set | 335.8 / 335.9 MB | 147.7 / 147.5 | 203.3 / 203.4 | 741.2 / 745.0 | 3 of 4 |

Two values mean two independent runs (`20260805-134952`, `20260805-142330`);
latency was measured once, over 40 samples per terminal with nothing censored.

What this says plainly:

- **Input latency is Kettle's win, and it is not marginal.** It leads at every
  percentile, and its p99 (139 ms) is better than every peer's p95. That is the
  number a user meets on each keystroke.
- **Idle CPU: Kettle measured exactly 0.0000% in both runs**, while Alacritty and
  WezTerm each drifted above zero in one of the two.
- **Memory is where Kettle is still behind.** Measured directly, root process
  only, same workload: **Kettle 321.5 MB, Alacritty 121.1 MB, WezTerm 152.8 MB.**

  Sampling the working set every 25 ms and aligning it against the phase log
  attributes it, and the answer is not what a terminal's memory usually implies:

  | phase | delta |
  |---|---|
  | process/runtime init | +21 MB |
  | | +17 MB |
  | adapter selection (DX12) | +21 MB |
  | glyphon `Cache::new` — the FIRST pipeline creation | **+182 MB** |
  | starfield pipeline | +46 MB |
  | glyph pipeline | +21 MB |

  The steps come from one run and the 321.5 MB from another, so they total to
  that run's own final figure (~318 MB) rather than to 321.5 exactly; the spread
  between runs is a few MB. What matters is the SHAPE, and it is stable: about
  312 of those ~318 MB arrive during GPU initialisation, and the terminal itself
  adds only ~7 MB after it. The 182 MB step is the DX12 shader compiler being
  loaded and run for the first time — not an allocation Kettle makes.

  Four plausible causes are ruled out, so nobody re-suspects them: scrollback
  (10000 -> 100 moved the total by ~0; history grows lazily, as in Alacritty),
  config and features (a minimal profile measured within 1 MB of the full one),
  the font system (~25-30 ms and small), and the glyph atlas (glyphon starts at
  256x256 and grows on demand).

  What IS closable: the starfield pipeline costs a measured 46 MB and is built
  unconditionally, though every use of it is already gated on
  `background-type = starfield`. Not compiling shaders a session will never use
  is right on principle; it is tracked, and it does not change the ranking.

  Startup needs care, because the suite's number is an end-to-end readiness
  figure and a reader will mistake it for "time until a window appears". A direct
  window-appearance probe on a quiet machine, three runs each with these same
  configs, gives the narrower truth:

  | | Alacritty | WezTerm | Kettle |
  |---|---|---|---|
  | window visible, median, BEFORE | 502 ms | 696 ms | 1068 ms |
  | window visible, median, AFTER | 407 ms | 524 ms | **215 ms** |

  Kettle was last by ~370-570 ms. It is now first, and the fix was not to make
  renderer init faster -- it was to stop hiding the window through it. Its own phase
  timings (`RUST_LOG=info`, added for exactly this question) account for it:
  pre-main ~40-106 ms, window create + post-create setup ~17 ms, accessibility
  ~5 ms, **GPU init ~700 ms** (adapter ~390, device ~85, and ~172 ms for
  everything after the device -- surface, fonts and pipelines together), first
  paint and reveal ~100 ms. `FontSystem::new()` was the obvious suspect and is
  not the problem at ~46 ms, including with this profile's `font-family =
  Cascadia Mono`, which forces a system-font lookup the bundled default never
  exercises; loading the bundled font on top of it is unmeasurable.

  Those phase boundaries are worth stating precisely, because a review caught
  all three being sloppy: the window figure stopped before post-create setup
  while the GPU figure was derived by subtraction (so that setup was charged to
  the GPU), the post-device figure silently contained the separately-logged font
  time (inviting a double-count), and the "font system" figure covered the
  bundled-font load as well as the constructor the analysis singled out.

  Roughly two thirds of that was GPU initialisation with the window hidden for
  all of it. Kettle hid for a good reason: a window shown before its first
  painted frame shows the class's stock WHITE brush, and most of a second of
  white is a worse greeting than a late window.

  The window is now revealed immediately -- but only when the configured
  background is within 24 levels of black on every channel, because an unpainted
  window is BLACK and that is the only case where the pre-paint frame cannot be
  told from what follows. `#101010` against black is four levels on one channel.
  Anything with visible colour, light or dark, keeps the old hide-until-painted
  path unchanged, and light-theme startup is accordingly still ~839-920 ms.

  Setting the window class background brush to the terminal's own colour was
  tried first and is kept as a best effort, but the reveal deliberately does NOT
  depend on it: with `#f0e8d8` configured the pre-paint window still sampled
  `0,0,0`, because winit owns `WM_ERASEBKGND`. Trusting the brush would have
  handed light-theme users a black flash instead of a white one.

  One caveat for anyone re-measuring: the FIRST launch after a driver shader-cache
  reset measured pipelines+atlas at 4218 ms against 165 ms warm. Discard it.
- **Throughput produced no data.** The channel between the probe and its workload
  child timed out on connection. It is the one comparative metric missing here,
  and it is not being claimed either way.

So on the metrics that were measured, Kettle beats every peer on latency, ties
best on idle CPU, now leads time-to-window (see the startup section below --
these suite figures predate that change and are an end-to-end readiness number,
not time-to-window), and trails on memory.

## Next release — paired six-terminal and physical-display gates

The Windows harness measures Kettle, Windows Terminal, Alacritty, WezTerm, Rio,
and Tabby in one interleaved session. `-Mode release` is the publication gate;
`-Mode smoke` is explicitly non-release evidence and allows shortened/skipped
probes or manifest-only discovery. Release mode pins PowerShell 7, the
release-candidate binary and source identity, every executable, exact raw sample
counts, all workload hashes, and the complete display topology. Schema 4 also
binds both candidates to one reviewed comparator campaign: official release
assets, expanded tree identities, executable bytes/hashes/signatures, versions,
and roles must match the tracked release contract. Confirmed staged trees stay
read-leased for the full run. The installed Windows Terminal package is
revalidated separately, then its exact `WindowsTerminal.exe` Appx host is
hash-validated, read-leased for the full run, and executed directly. Release
evidence rejects `PATH`, `KETTLE_PERF_WT_EXE`, App Execution Alias, or other
launcher indirection; standalone smoke probes retain ambient discovery only as
explicitly advisory evidence.

### Running a comparator session on a working machine

Two harness defects used to make a run impossible here, and both are fixed:

- **The readiness check demanded a byte-exact marker colour.** Rio renders a
  truecolor background a few levels off what it was given -- `48,89,94` comes
  back as `59,89,94` -- so readiness timed out after 30s and aborted the run for
  a reason unrelated to performance. Alacritty is byte-exact, which is why it
  went unnoticed. The check now allows a bounded per-channel tolerance; see
  `Test-KettlePerfPaintedMarkerCapture` for why 16 is the number.
- **The isolated WezTerm config set a field that WezTerm 20240203 rejects.** An
  unknown field makes WezTerm open a configuration-error window alongside the
  terminal, so the launcher saw two windows and refused the run as ambiguous.

Three more were found while getting a session to complete on a shared machine:

- **Readiness spent its deadline scanning pixels in PowerShell.** The poll walks
  the captured region for the marker colour; a match can stop early, a MISS
  cannot -- and a miss is what every poll before the window paints is.
  Interpreted, that walk over 1024x384 measured **2,585 ms** here, so a 30s
  deadline bought about eight looks and a terminal that painted late was reported
  as one that never painted. Compiled (`CountPixelsNearColor`) the same worst
  case is **89.8 ms**, a 29x cut; the deadline now measures the terminal rather
  than the harness. The timeout also reports the slowest capture and how much of
  the deadline went on capturing, so the two failure modes stop looking alike.
- **Two probes asserted a window reached the foreground on the next
  instruction.** `SetForegroundWindow` is refusable and its effect is not
  immediately observable, so that is a race -- and it loses whenever another
  application owns the foreground. Startup and throughput now use a bounded
  acquire-and-confirm helper.
- **`pwsh -File perf-all.ps1 -Terminals a,b,c` passed one literal string.**
  `-File` does not parse array syntax, and the resulting one-element list failed
  hundreds of lines later inside the schedule generator. A `ValidateSet` now
  rejects it at the boundary. Pass a real array under `pwsh -Command`.

The remaining environmental precondition is narrower than it was. The harness
refuses to start when a comparator is already running, because a terminal that
joins a running instance opens no new window and the launch would fail as an
unexplained timeout. That reasoning does not apply to terminals whose pinned
launch arguments force a fresh process, so `-AllowForeignTerminalInstances`
(smoke only) lifts it for exactly those, and the manifest records the tolerated
PIDs -- attribution never depended on the process name, but contention is real
and those samples are not quiet-machine numbers.

Kettle, Alacritty, WezTerm, Rio, and Tabby receive run-local configs with the
same font, scrollback, colors, opacity, padding, cursor, and disabled effects.
The installed Windows Terminal has no per-launch settings-file switch, so its
configuration is recorded but its numbers are advisory. Confirmed release
claims use only the four isolated peers: Alacritty, WezTerm, Rio, and Tabby.

Startup no longer stops at the first HWND. A common PowerShell 7 child waits for
an atomic GO marker, paints and flushes a nonce-derived truecolor rectangle,
requires the terminal's `CSI 5 n` → `CSI 0 n` parser round trip, and atomically
publishes READY. The timer stops only after exact client placement plus both the
validated READY marker and painted pixels. Startup polling captures a top-left
ROI capped at 1024×384 instead of transferring an entire high-resolution frame.
Process-tree/CIM attribution runs after that endpoint and is reported
separately, with each placement and readiness milestone retained for audit.

The hover regression has two Kettle legs: the common 1280×800 comparator client
and a `native-display` client derived from the selected monitor's working area.
Both use real foreground pointer movement and poll only the context-menu ROI
over 200 samples in blocks of 20. This software-capture boundary includes
dispatch, redraw, GPU submission, composition, and PrintWindow cost; it is
comparative evidence, not input-to-photon measurement.

Release evidence requires the target screen to map to one active EDID-backed
physical monitor and fit the requested client. The mandatory transition probe
also requires a second eligible physical screen and measures recovery with the
context menu closed and open. Display bounds, DPI, refresh, EDID, connection,
and primary mapping must remain identical at every probe boundary. A continuous
Windows `DisplaySettingsChanged` subscription also records intervening changes,
so switching away and later returning to the original topology still
invalidates the run. The probe deliberately moves Kettle between the two pinned
screens; any operator-initiated switch or topology change outside it invalidates
the entire result. A virtual/default 1024×768 desktop can run synthetic or
manifest smoke checks but cannot produce release evidence.

The 2026-07-27 inspection after the reported monitor switches is exactly that
blocked case: Windows exposes only default `\\.\DISPLAY1` at 1024×768, and no
physical PnP monitor is present. The desktop cannot fit the required 1280×800
client or supply two EDID-backed screens, so no current six-terminal win/loss
or release claim is recorded. Reconnect a stable two-monitor physical desktop
and start a new run; returning to the old topology cannot rescue samples taken
across a switch.

The source recording is useful only as machine-local diagnostic evidence. Its
626×548 H.264 stream contains 121 frames over 4.095979 seconds at exactly
30 fps; adjacent frame timestamps are 33.333–33.334 ms apart, with none over
40 ms. Cursor motion best predicts the highlight seven frames later
(233.3 ms; 37/56 exact row matches and 0.411-row mean absolute error). Together
with the matching idle PTY trace, that points to input/event-to-paint/present
latency rather than encoder cadence or parser output pressure. It is not
comparator or release evidence.

Physical-monitor identity acquisition is versioned and fail-closed. WMI is
accepted only when one monitor and one physical connection share the exact
instance identity. Miracast and indirect wired/virtual connections remain
ineligible for release evidence even when they expose plausible EDID data. If
the WMI connection is absent, the fallback accepts only one active physical CCD
path for the desktop source, uses both its monitor and connection evidence,
requires the exact `GUID_DEVINTERFACE_MONITOR` class, derives one registry key
from that strict device-interface name, and validates the complete EDID header,
block count, checksums, manufacturer, and product against the CCD identifiers.
It never searches the registry for a matching model or combines sources. The
scorer independently reconstructs that mapping and revalidates connection
technology. Missing, duplicate, malformed, synthetic, or inconsistent evidence
leaves the screen unidentified and therefore cannot pass a release gate.
Trusted schema-4 evidence is type-exact: flags must be JSON booleans and output
technologies must be JSON integers, so PowerShell-coercible strings, 0/1
surrogates, and integral floating-point tokens are rejected.

The vtebench leg pins the Windows WSL engine, one exact registered
distribution, and the Linux Rustup, Cargo, `timeout`, `setsid`, and `script`
executables by canonical path, SHA-256, and version. Its clean source/build
signature is checked before and after every terminal leg, every phase has a
finite deadline, and timed-out descendants are terminated by their exact Linux
process group. Typed-latency rows likewise bind the exact Windows workload
shell path and hash. These identities are baseline compatibility requirements,
not descriptive metadata.

Startup, idle/fresh-memory, typed latency blocks, and throughput rounds use
seeded Williams-balanced schedules so position and predecessor effects are
balanced. Throughput begins only after the exact terminal window acknowledges a
locked, unpredictable GO capability and records its client-pixel and
console-cell geometry. It times console-write start through the terminal's DSR
response, not just writer acceptance. Every visit runs the exact
ASCII/SGR/Unicode payloads once, and the score derives a paired per-round
geometric-mean composite.

The release decision comes from raw paired observations. Deterministic
10,000-resample, 90% paired-cluster bootstrap intervals apply practical margins
to startup, idle CPU, fresh working set, latency, and throughput. An uncertain
interval never establishes a win. The authoritative peer rule confirms a win
from at least three of four confirmed metric wins with at most one confirmed
loss, so its unused fourth metric may be uncertain; the aggregate rule likewise
requires at least three confirmed peer wins and at most one confirmed loss.
Each series must also remain within 10% absolute fitted first-to-last drift and
20% normalized peak-to-peak spread. Every throughput round must remain positive
after its 5% margin. A mandatory same-machine baseline must match the
environment and pass paired non-inferiority for every required metric; because
that is an all-metrics gate, baseline uncertainty fails closed.

Release scoring pairs a `current` candidate built from the clean checkout with
a `baseline` candidate taken from a previously verified signed release
archive. The baseline is an external binary pinned by exact SHA-256 and full
ancestor commit; both candidates run through the current, byte-identical
harness and isolated configurations on the same stable machine session.

The raw result directory is private audit evidence. Use
`sanitize-results.ps1` to publish a separate schema-2 JSON-only bundle with
local paths, commands, monitor serials, device and hardware IDs, EDID
fingerprints, adapter LUIDs, source/target IDs, and connector instances replaced
by type-separated HMAC tokens keyed with a cryptographically random secret that
is discarded instead of published. Tokens correlate equal values only within
one bundle and cannot be brute-forced from the public run id. Source/stage
identities, reparse rejection, bounded flat-file publication, and atomic
directory rename keep that sanitizer fail-closed. Credential-like property
names are normalized across common case and separator variants, and values of
any JSON shape are tokenized. Publication accepts only the reviewed fixed
harness filenames; custom JSON evidence is rejected until its name and schema
receive an explicit sanitizer review, preventing source filenames from leaking
user or credential text.
Exact commands, sample counts, margins, validation steps, and caveats
are in [`scripts/perf/README.md`](../scripts/perf/README.md).

No fresh Windows release comparison is claimed in this section until the full
suite completes on the stated physical displays. GUI-free PowerShell
self-tests and manifest smoke runs validate the harness logic, but they do not
substitute for live GPU, input, or monitor-transition measurements.

## v2.25.1 — grid cursor-blink regression fix

The grid renderer fix keeps cell-locked pane glyph uploads on their own damage
gate. Cursor blink no longer participates in that gate: a blink updates cursor
quads and the separate cursor-glyph pass only, while pane text/style/geometry
damage still refreshes grid glyph instances. This preserves the v2.25.0 idle
intent (no full pane glyph re-upload for a blink) and closes the prompt-glyph
disappear/reappear regression covered by the new offscreen `➜  ~` pixel test.

This release also makes `gpu-power-preference = auto` the default again. That is
the least surprising cross-platform policy: single-GPU machines report their
only adapter without pretending a discrete GPU was selected, while hybrid
laptops can still opt into `high` for dedicated-GPU headroom or `low` for
integrated/battery-friendly startup.

**Ubuntu local desktop smoke, v2.25.1 snapshot**
(`kettle 2.25.1 (5596f3aabbb7)`, `text-renderer = grid`,
`gpu-power-preference = auto`, timing medians over 3 Hyperfine runs with 1
warmup, RSS medians over 3 `/usr/bin/time -f %M` runs, real X11/Wayland
desktop):

| workload | kettle | Terminator | Ghostty | Alacritty |
|---|---:|---:|---:|---:|
| launch terminal, run `/bin/true`, close | 167 ms | 324 ms | 481 ms | 148 ms |
| launch terminal, print ~4 MiB ASCII, close | 282 ms | 394 ms | 562 ms | 257 ms |
| launch terminal, print 35k SGR/underline lines, close | 311 ms | 482 ms | 580 ms | 274 ms |
| max RSS while printing ~4 MiB ASCII | 140.7 MiB | 72.7 MiB | 168.3 MiB | 109.2 MiB |

These are smoke numbers rather than a full latency suite, but they exercise the
current release binary on the adapter the default policy chooses on this
machine. Kettle beats Terminator and Ghostty on startup, plain ASCII flood, and
SGR/underline flood timing probes; it remains close to Alacritty for the flood
paths. The RSS row is advisory evidence: Kettle is below Ghostty for this
lifecycle and above Terminator/Alacritty, so memory work remains open. The JSON
for this run is under `target/perf-results/linux-local-20260618-0025/`.
The same run recorded Kettle-only live control-plane medians of 21.0 ms for
resize settle, 33.0 ms for page-up scrollback navigation, and 33.9 ms for
page-down scrollback navigation.

**Memory follow-up, 2026-06-18 (`13ffdda` → local font-source patch).** The
bundled JetBrains Mono Nerd Font faces are still embedded for out-of-box
AstroNvim/Neovim icon coverage, but `fontdb` now receives the embedded
`&'static [u8]` faces through `Source::Binary(Arc<...>)` instead of cloning each
face into a fresh `Vec<u8>`. On this Ubuntu/Iris Xe machine, the same Kettle-only
ASCII-flood lifecycle dropped grid-mode max-RSS from a pre-change median of
about **138.6 MiB** to **136.5 MiB**. Legacy mode measured the same shape
at **136.1 MiB**, confirming the remaining gap is not grid-renderer-specific.
Forced software Vulkan stayed materially worse at **160.2 MiB**. This closes the
duplicated-font-bytes footgun, but Kettle still remains above Terminator on RSS,
so the newly added byte-budget scrollback cap still needs a refreshed RSS pass;
atlas bounds and GPU buffer residency remain the next memory levers after that.

Reproduce and gate this Ubuntu peer comparison with:

```sh
just linux-perf
# or:
scripts/perf/linux-compare.sh --runs 7 --out-dir target/perf-results/linux-v2.25.1
```

The script writes Hyperfine JSON for startup, ASCII flood, and SGR/underline
flood timing probes, advisory `linux-rss-flood.json`, advisory
`linux-kettle-live.json`, and `linux-score.json`. It fails if Kettle is slower
than Terminator or more than 10% slower than Ghostty on any cross-terminal
timing workload. The Kettle-live probe launches a real grid-renderer window,
times `resize_window` until `ui_geometry` settles, generates underlined
scrollback content, and times `scroll_page_up/down` viewport movement. Those
live medians include `kettle ctl` round-trip overhead and are Kettle-only
regression evidence until a reliable peer-terminal GUI driver is added. The
whole Linux suite is desktop-local by design because it needs installed GUI
terminal peers and a real X11/Wayland session.

## v2.25.0 — cell-locked glyph rendering: no hot-path regression

The cell-locked glyph pipeline (`text-renderer = grid`, the new default) replaces
glyphon's per-pane `Buffer`/`prepare` for pane text with `emit_pane_glyphs` + an
instanced glyph pass. The concern was the render hot path, so it was measured
directly. The pane-shaping cache (`pane_line_keys`) and the lock-free snapshot
pipeline are unchanged, so PTY parsing (the throughput path) is untouched by
design — and the measurements bear that out.

**Throughput — grid vs legacy, same release binary, both orderings** (Surface
Book 3, discrete GPU per the live config; `scripts/perf/throughput.ps1`, MB/s,
median of 5). Run-to-run variance was ~10 % and the *second* run of each pair was
always faster regardless of mode (GPU clock warm-up), so the orderings are
averaged to cancel that bias:

| payload | grid | legacy | Δ |
|---|---|---|---|
| ascii | 1.45 | 1.54 | −6 % |
| sgr-heavy | 1.15 | 1.23 | −6 % |
| unicode/CJK | 2.58 | 2.63 | −2 % |

The deltas sit inside the ±10 % inter-run noise — i.e. **no significant
throughput regression**. (The absolute numbers are well below the v2.21.x
integrated-GPU figures below because the live config pins the discrete NVIDIA
`gpu-power-preference = high`, which trades cold-start + flood throughput for the
discrete path — a config choice, not a code change.)

**Idle CPU** (60 s, blinking cursor, solid background): **4.56 %** in grid mode —
unchanged from the v2.21.0 present-bound floor (~3.8 %), confirming that the grid
path's per-blink re-emit costs essentially nothing (it runs on the same
`need_prepare` damage gate as the glyphon prepare it replaces). **Fresh working
set** 293 MB (≈ the 307 MB documented below). **Startup** is unchanged in
character — discrete-GPU-wake-dominated (~2–3 s on this dual-GPU laptop); the new
pipeline adds only a single shader-module compile + two small atlas textures at
`Renderer::new`.

Net: kettle remains a throughput-and-footprint competitor (beats WezTerm, trades
with Alacritty, behind Windows Terminal's shared-process model), and the
cell-locked renderer is at parity with the path it replaces.

## v2.23.x — re-verification + the animated-background idle fix

Re-ran `scripts/perf/perf-all.ps1` against **Alacritty** and **WezTerm**
(`scripts/perf/score.ps1` is the committed "kettle in the top half" gate).

**Throughput is unchanged and still leads the dedicated competitors.** The
v2.23.x changes (the wallpaper render pass, the GPU-default flip) don't touch the
PTY parse path, and a re-run confirmed it — kettle **beats Alacritty and WezTerm
on all three payloads** (ascii / sgr / unicode), behind only Windows Terminal's
shared-process class. See the v2.21.1 table below for the calm-machine numbers
(the re-run was on a dev box loaded with this very session + a dozen MCP
processes, which depresses every terminal's absolute numbers but preserves the
ranking: kettle 4.32 / 3.67 / 6.99 vs Alacritty 2.48 / 2.57 / 5.24 and WezTerm
2.83 / 2.51 / 4.75 MB/s).

**Animated-background idle fixed (the real find).** An animated `background-image`
was repainting at a fixed 30 fps regardless of the GIF's own frame rate — and,
worse, `request_redraw` was called *level-triggered* every event-loop iteration,
so winit redrew continuously (vsync-bound). Measured **~55–60 % of a core** idle
while a focused animated wallpaper was visible. The fix makes the bg redraw
**edge-triggered** (request a redraw only when the displayed frame index changes)
and wakes the loop at the GIF's own frame boundary (`bg_next_frame_ms`) — so it
repaints at the GIF's fps, not 30 fps. Measured **20.9 %** for the same 8 fps
loop (~2.7× less), and a non-animated background or solid theme stays at the
~3.8 % present-bound idle from v2.21.0. Animating a full-surface wallpaper still
costs one `present()` per frame (inherent to a wgpu flip-model swapchain); the
fix removes the *wasted* repaints, not the necessary ones.

**Honest weak axis: cold start.** kettle's wgpu device + pipeline + font init
makes startup ~1 s on the integrated GPU and ~1.9–2.2 s on the discrete one —
slower than Alacritty/WezTerm/WT (~0.26–0.48 s). The v2.23.0 **default flip to
the discrete GPU** (more render headroom for wallpapers/large windows) widened
that gap by ~1 s at the time; current v2.25.1 builds default back to
`gpu-power-preference = auto`, with `low` available for an explicit integrated
adapter preference and `high` for dedicated-GPU headroom. So on the
equal-weighted `score.ps1` composite (throughput + startup + idle + memory),
kettle leads on throughput and on memory-vs-WT but trails on startup; it is a
**throughput-and-footprint leader, not a cold-start leader**.

## v2.20.0 — the cross-terminal benchmark harness + the perf overhaul

v2.20.0 added the committed harness this doc previously listed as an open
follow-up (`scripts/perf/` — one pinned methodology applied to every
terminal) and a seven-part performance overhaul driven by what it measured.
All numbers below: Surface Book 3 (Intel Iris Plus, Win11 26200), release
builds, identical 1280×800 windows, medians of 5 runs, deterministic
generated payloads written from *inside* each terminal in 32 KiB chunks
(the termbench principle — the terminal's own parse+render path is the
bottleneck being measured, not a pipe).

### Throughput (parse + render under flood)

| payload | v2.19.0 | v2.20.0 | **kettle v2.21.1** | Windows Terminal | Alacritty 0.17 | WezTerm |
|---|---:|---:|---:|---:|---:|---:|
| ascii (16 MB) | 0.55 | 1.90 | **4.57 MB/s** | 4.33 | 3.59 | 2.56 |
| sgr-heavy (6.1 MB) | 0.42 | 1.63 | **3.70 MB/s** | 4.12 | 3.06 | 2.67 |
| unicode/CJK (4.3 MB) | 0.80 | 3.48 | **7.00 MB/s** | 9.04 | 5.79 | 5.03 |
| post-flood working set (terminal+conhost+shell) | 241.5 MB | 485.7 MB | 638.1 MB | 2977.7 MB | 396.6 MB | 411.4 MB |

**v2.21.1 is 2.0–2.4× faster than v2.20.0** and flips kettle from *last* of these
four to **#1 on ascii (4.57 > WT 4.33 > Alacritty 3.59 > WezTerm 2.56)** and **#2
on sgr/unicode** — beating Alacritty and WezTerm on all three payloads, behind
only Windows Terminal (and only on sgr/unicode; WT runs in a shared
`windowingBehavior = useExisting` process, so its "terminal" is a different
measurement class). The win came from the v2.21.1 **adaptive output-paint
budget**: under a sustained flood kettle painted at 60 fps, grabbing each pane's
`Term` mutex ~60×/s for an O(cells) snapshot — the same lock the PTY reader needs
to parse — so on a CPU-contended box the parser was starved. Stretching the paint
budget to 30→20 fps during a flood (content is unreadable scrolling anyway; a
brief burst and all keystroke echo stay at 60 fps) hands the lock and cores back
to the reader. The post-flood working set rose (faster consumption accumulates
scrollback sooner — the byte-budget-scrollback cap and atlas-bound follow-ups below
address it); it still stays ~4.7× leaner than Windows Terminal under the same
flood.

Earlier honest position (now superseded by v2.21.1): at v2.20.0 kettle was last
of the four, ~1.3–2.5× behind, with row-damage tracking listed as the lever.
Adaptive flood-throttling captured most of that gap without the full row-damage
rewrite, which remains the tracked lever for closing the residual WT sgr/unicode
gap and for steady (non-flood) render cost.

What the overhaul changed (each lands with a regression guard):

1. **Lock-free rendering (P2)** — the renderer previously held every
   pane's terminal mutex across the whole GPU frame (shaping +
   `get_current_texture` + present), starving the PTY reader. It now
   works from a pooled `PaneSnapshot` captured under the lock in
   microseconds.
2. **Per-line shaping cache (P1)** — cosmic-text re-shaped 100 % of the
   visible viewport on every painted frame (`set_rich_text` resets all
   lines). Pane text now keeps one `BufferLine` per grid row keyed by its
   content; an idle blink frame re-shapes zero rows, a cursor move one.
   Chrome labels (titlebar/tab/status, and quick-select hints since
   v2.38.2) gained the same equality gates.
3. **SIMD extractor (P3)** — the image-protocol front stage walked the
   stream byte-by-byte; it now `memchr`-scans to the next ESC/ST/BEL and
   bulk-copies plain runs. `cargo bench -p kettle-vt` pins it (the first
   criterion benches in the repo).
4. **Wakeup dedup (P4)** — floods enqueued one event-loop wakeup per
   64 KiB read; a per-pane atomic gate now allows one pending wake while
   renderable, retains paint damage while hidden or recovering, and publishes
   one restore paint wake. An opt-in recorder/Lua output sidechannel keeps
   transport wakes serviceable while hidden so its bounded queue drains, but
   visibility/recovery guards still prohibit presentation. A per-window paint
   state machine keeps failed presents retryable without a zero-delay wake
   loop.
5. **Recorder batching (P5)**, **link-scan debounce (P6)**, **session-log
   lock skip (P7)** — per-frame/per-read costs off the hot paths.

### Startup, idle CPU, memory at rest

| | kettle v2.19.0 | **kettle v2.20.0** | Windows Terminal | Alacritty | WezTerm |
|---|---:|---:|---:|---:|---:|
| spawn → first visible window (median of 5) | 2189 ms | 2202 ms | 268 ms¹ | 277 ms | 506 ms |
| fresh working set (tree) | 306.9 MB | 306.8 MB | —¹ | 201.7 MB | 166.7 MB |
| idle CPU, 60 s, cursor blinking | 55.89 % | **28.28 %** | —¹ | 0.36 % | 0.52 % |

¹ Windows Terminal on this machine runs `windowingBehavior = useExisting`:
`wt.exe` opens a window inside the already-running process, so its
"startup" is not a cold process start and its working set / idle CPU are
not attributable to one window (the harness deliberately refuses to
measure or kill a shared instance).

The idle cost **halved** (the P1 cache removed the full-viewport reshape
that ran on every blink frame) but remains far above Alacritty/WezTerm:
each blink frame still rebuilds the full quad list and glyphon vertex
data. The fix is row-level damage tracking + persistent GPU cell buffers
— the tracked follow-up below. Startup (~2.2 s) is GPU-adapter init +
the embedded font set + 500 themes, untouched by this change and now tracked
with a number against it.

### Input latency

`scripts/perf/latency.ps1` — SendInput a key, poll
client-only `PrintWindow(PW_RENDERFULLCONTENT)` until the pixels change beyond
an auto-calibrated blink-noise floor. Capture cost bounds resolution at
~5–15 ms, so these are **comparative between terminals captured the same
way**, not absolute input-to-photon numbers. The probe requires an
INTERACTIVE session: Windows does not let a background process steal
foreground, and the script refuses to inject keystrokes unless the
spawned terminal verifiably holds focus. In the autonomous v2.20.0 run
only WezTerm took foreground — its guarded 20-sample dataset is in
`target/perf-results/v2.20.0/latency.json` (median ≈116 ms by this
capture method) — while kettle, Windows Terminal and Alacritty failed
the foreground guard, so no cross-terminal comparison is published; that
needs an interactive session.

### Methodology / reproducing

```pwsh
pwsh -NoLogo -NoProfile -File scripts/perf/perf-all.ps1 `
  -Mode release -KettleCandidate current -Label release-candidate
pwsh -NoLogo -NoProfile -File scripts/perf/score.ps1 `
  -Mode release `
  -ResultsDir target/perf-results/release-candidate `
  -BaselineResultsDir target/perf-results/baseline-previous-release `
  -RequireLatency -RequireMenuHover -RequireVtebench `
  -RequireMonitorTransition
```

The prior release baseline must first be acquired and pinned with the
`-KettleCandidate baseline`, `-KettleExe`, `-SkipKettleBuild`,
`-ExpectedKettleCommit`, and `-ExpectedKettleSha256` arguments documented in
[`scripts/perf/README.md`](../scripts/perf/README.md). The orchestrator builds
the current candidate, creates and locks isolated configs, and runs probes in
the pinned order. Do not replace it with direct probe invocations for release
evidence. A manifest-only smoke is:

```pwsh
pwsh -NoLogo -NoProfile -File scripts/perf/perf-all.ps1 `
  -Mode smoke -ManifestOnly -AllowUnidentifiedDisplay `
  -Label ("topology-" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
```

That smoke validates discovery and schema paths only. It does not exercise a
native GPU window or physical-display interaction.

### Current performance gate

New Windows performance work should publish a sanitized bundle derived from a
clean same-machine release run. The confirmed gate excludes advisory Windows
Terminal and uses paired bootstrap intervals against the four isolated peers.
It requires at least three confirmed primary peer wins, at most one confirmed
loss, all throughput rounds positive after the 5% margin, bounded drift, both
context-menu legs, vtebench, two-screen monitor-transition evidence, and stable
start/end display topology. Uncertain evidence never contributes a confirmed
win, but an unused fourth metric or peer may be uncertain under those explicit
3-of-4 and 3-of-4-peer count rules. When a compatible baseline is supplied,
every required Kettle metric must be non-inferior; missing or uncertain
baseline evidence fails closed.

For monitor-transition evidence, every eligible display pair is ranked by
meaningful DPI, refresh, and screen/working-area size contrast, with an ordinal
device-pair tie-break. The scorer reconstructs that ranking from start topology
and validates exact closed/open sample keys, alternating endpoints, captures,
DPI/refresh readings, menu state, stable geometry, and all raw-derived
summaries. Each state and the combined result must keep p95 at or below 1000 ms
and maximum at or below 2000 ms; all six summaries must also remain within
`max(100 ms, 25% of baseline)` against the mandatory same-machine baseline.
Fresh-memory and idle-CPU evidence likewise requires unique,
nonoverlapping included/excluded PID sets and identical included membership
before and after the idle interval.

Linux desktop performance work should also run `just linux-perf` when Terminator
and Ghostty are installed. That gate is intentionally narrower than the Windows
suite, but it directly protects the Ubuntu requirement that Kettle beat
Terminator and stay close to Ghostty on launch, ASCII-flood, and SGR/underline
flood probes. The same run now records Kettle-only live resize and underlined
scrollback-navigation medians; these fail the run if the UI state does not move
correctly, but remain advisory for speed until equivalent Terminator/Ghostty
automation exists.

### v2.21.0 — startup 2.2× faster, damage-aware idle, corrected root causes

v2.21.0 corrected two root-cause attributions this doc previously got wrong, by
*measuring* instead of guessing:

- **Startup was discrete-GPU wake, not "500 themes."** The 500 bundled themes
  cost zero startup time (they are parsed lazily — only the active theme parses
  at boot). The real ~1.5 s cost was `Renderer::new` requesting the wgpu adapter
  with `PowerPreference::HighPerformance`, which on this dual-GPU laptop wakes
  the **discrete NVIDIA** from its low-power state. Defaulting to the low-power
  (integrated) adapter cut **spawn → first-visible-window from 2202 ms to
  ~999 ms (median of 5)**. The then-new `gpu-power-preference` key (`low`
  default at v2.21.0 | `high` | `auto`) let a discrete-only/desktop user opt
  back in; current releases default to `auto`. Trade-off: the
  integrated adapter's buffers live in **system RAM**, so the measured fresh
  working set rose from 306.8 MB (discrete, GPU memory hidden in VRAM) to
  ~471 MB (integrated, GPU memory now counted) — an honest number, comparable to
  how Alacritty/WezTerm are measured.

- **Idle CPU is `present()`-bound, not `prepare`-bound.** With a blinking cursor
  idle CPU is ~3.8 % (down from 28 % via the deadline-scheduled blink). The
  residual is the **two vsync `present()`s per second a blinking cursor
  requires** on the integrated GPU — *not* glyphon `prepare`. v2.21.0 still adds
  damage-aware rendering (an idle frame skips the whole-viewport `prepare`
  + `atlas.trim` when no row reshaped, no chrome label changed, and no overlay
  is open; the block cursor's inverted glyph is drawn in a dedicated 1-glyph
  pass so a blink leaves the pane buffer byte-identical) — this is the right
  damage architecture and pays off on larger grids / faster GPUs / battery, but
  on this small-window, present-bound benchmark it does not move the number.
  Sub-1 % idle would require not presenting per blink (e.g. a cursor-only
  partial update), which a full-surface wgpu swapchain cannot express.

Other v2.21.0 renderer trims: only the Regular font face loads at boot (Bold /
Italic / Bold Italic defer to first styled text); pane text/title caches are
keyed by process-global pane id (preserved across tab moves / split reorders);
visible windows reveal as soon as the surface is configured; cursor blink wakes
at the configured half-period deadline.

### Known follow-ups (tracked)

- Throughput row-level damage + persistent GPU cell buffers (capture/upload only
  changed rows, shrinking the per-frame snapshot lock-hold that contends with
  the PTY parser under flood — the Ghostty architecture; see
  docs/UX-COMPARISON.md). v2.21.0 landed the *idle*-side damage gate (skip
  `prepare` when nothing changed); the *flood*-side capture/upload damage is the
  remaining piece and the lever most likely to close the throughput gap.
- Glyph-atlas growth under sustained unicode flood (the post-flood WS
  delta above) — bound with an LRU / size cap.
- Byte-budget scrollback now has an initial `scrollback-bytes` cap: Kettle keeps
  the legacy `scrollback` line-count key and derives the effective history line
  limit from the byte budget, including the visible screen. Follow-up: rerun the
  Linux peer RSS matrix and Windows/WSL checks with the default 10,000,000-byte
  cap.
- Bundled font duplicate-copy fixed after `13ffdda`: embedded faces are now
  registered with `fontdb::Source::Binary(Arc<&'static [u8]>)` instead of
  `.to_vec()` copies. Keep this as a regression invariant because reverting it
  quietly adds resident heap copies for Regular at startup and Bold / Italic /
  Bold Italic when styled text first appears.

---

## Historical numbers (v1.x — kept for reference)

Real measurements from kettle's release binary, captured across two
reference platforms:

- **Linux baseline** — Ubuntu, 8-core x86_64, software-Vulkan via
  `mesa-vulkan-drivers` (matches what `.github/workflows/ci.yml`'s
  `--screenshot` smoke runs on).
- **Windows 11 reference** — Surface Book 3, x64 + Intel Iris Plus
  Graphics (DX12 / Vulkan adapter via wgpu), Win11 26200.

Reproducible:
- Linux / macOS: `scripts/bench.sh` (GNU `time -f '%e %M'` based).
- Windows: `scripts/bench.ps1` (`System.Diagnostics.Process`
  based; uses `PeakWorkingSet64` for peak memory, captured at exit).

Both scripts build a release binary if one isn't present, then run
each measurement five times and print the wall-clock + peak-memory
for each invocation.

## Numbers

### Linux baseline (v1.3.8, commit `1026858`)

> Captured at v1.3.8 (the Linux box wasn't available for a re-bench at later
> cuts). There's been no major architectural change to the render/startup paths
> since, so these may still be in the same ballpark on a current release —
> whose version may differ — but treat them as "what we measured then" and run
> `scripts/bench.sh` for a fresh data point on your own machine.

| Measurement | Value | Notes |
|---|---:|---|
| Release binary size | 24.7 MB | Includes embedded JetBrains Mono Nerd Font + ~500 themes. Trimmed ~6 MB by narrowing `image` features to PNG/JPEG/GIF (was pulling AVIF/`rav1e` + EXR + WebP + HDR + TIFF + …) and disabling `arboard`'s image-clipboard default feature |
| `kettle --version` startup | < 10 ms wall, 5.0 MB peak RSS | Cold (no warm pages); 5 runs all rounded to 0.00 s |
| `kettle --screenshot OUT.png` | ≈ 250–270 ms wall, 236 MB peak RSS | Includes wgpu adapter init, offscreen Vulkan device, font system load, full GPU text + quad pipelines |
| `kettle --screenshot-menu OUT.png` | ≈ 240–250 ms wall, 236 MB peak RSS | Same as above + the second TextRenderer / menu_quads pass; identical memory footprint, ~10 ms faster on the GPU pipeline warmup pattern |

### Windows 11 reference (captured at v1.46.0)

> Captured on a Surface Book 3 (Intel Iris Plus Graphics, x64,
> Windows 11 build 26200) the day the v1.46.0 release was cut (a fixed data
> point — the current release may differ; re-run `scripts/bench.ps1` for fresh
> numbers). wgpu
> picked the **Vulkan** backend (Intel driver, integrated GPU) — the
> same selection a user with the same hardware would see. Wall-clock
> via .NET `Process.ExitTime - StartTime`; peak working set sampled
> at 5ms granularity via `Process.WorkingSet64` polling (the
> `PeakWorkingSet64` property is documented in .NET but returns 0
> once the process exits on Win11; see the docstring in
> `scripts/bench.ps1` for why we poll instead).

| Measurement | Value | Notes |
|---|---:|---|
| Release binary size | 21.3 MB (22,370,304 bytes) | `kettle.exe` MSVC release build with embedded Win11 .ico via `winresource`. Slightly smaller than the Linux x86_64 binary (24.7 MB) — likely because MSVC's `panic=abort` codegen + LTO eliminates more unwind tables than gnu-stable did in the Linux baseline build above |
| `kettle --version` startup | ≈ 95-110 ms wall, 4-9 MB peak working set | Cold process spawn floor. Higher than Linux's <10 ms because Windows CreateProcess pays Defender real-time scan + image-load overhead. After warm-cache (Defender has hashed the .exe), drops to ~50-70 ms |
| `kettle --screenshot OUT.png` | ≈ 2.1-3.0 s wall, 377-389 MB peak working set | wgpu Vulkan adapter init + offscreen device + font system load + first font-atlas glyph upload. The first run is the slowest (~3 s — Defender cold-scan); runs 2-5 settle to 2.1-2.2 s |
| `kettle --screenshot-menu OUT.png` | ≈ 2.0-2.1 s wall, 381-389 MB peak working set | Same as above + the second TextRenderer / menu_quads pass. Peak WS higher than Linux software-Vulkan (236 MB) because Windows DX12/Vulkan adapter via wgpu keeps more state resident in the process's WS than Mesa software-Vulkan does on Linux. On a real-GPU Linux box with a hardware Vulkan driver, the comparable Windows-vs-Linux number is expected to be much closer |

## What the numbers mean

- **Startup is fast.** `--version` is a single `clap::Parser::parse`
  + a `println!`; under 10 ms on Linux. Windows adds process-spawn
  + Defender real-time scan overhead the first time the .exe is
  invoked from a directory (the install advice to "add the
  unzip folder to PATH" lets Defender hash + cache the binary once,
  after which startup matches the Linux floor).
- **GPU init is the screenshot cost.** ~250 ms wall for a single
  96×28 frame is almost entirely `wgpu::Instance::request_adapter` +
  `Adapter::request_device` + the first font-atlas glyph upload.
  The live windowed run pays this *once* per session; thereafter
  every frame is a sub-millisecond redraw against the warm
  pipeline. On Windows, wgpu's DX12 backend is typically 1.5-2×
  faster than software-Vulkan on the Linux CI runner.
- **Peak RSS / working set ~ 236 MB on Linux.** Looks high for a
  terminal but is dominated by:
  - The bundled JetBrains Mono Nerd Font set (~50 MB of glyph data).
    Newer builds load Regular at renderer startup and defer Bold/Italic/Bold
    Italic until styled terminal text needs them.
  - The 500+ bundled themes (Ghostty + iTerm2-Color-Schemes set).
  - The wgpu adapter (software-Vulkan in the headless path; the
    GPU driver on a real machine pages most of this out).
  - The font atlas + glyph cache (one entry per visible glyph,
    grows on first render of each codepoint).

  A live windowed kettle session at idle measures ≈80–120 MB on the
  same machine; the headless `--screenshot` peak is an overestimate
  for the steady-state windowed case because software-Vulkan keeps
  more state resident than a hardware adapter would. Windows DX12
  pages the wgpu adapter state to GPU-private memory, so the
  Windows working-set number undercounts the "real" footprint by
  comparison.
- **Extra windows cost VRAM, not a second GPU device (v2.18.0).**
  In-process multi-window shares one `wgpu` device/queue across
  every window (the handles are ref-counted), so opening a second
  window does *not* repeat the adapter/device init above. Each
  additional window owns only its surface and text atlas: roughly
  **17–25 MB of swapchain** (resolution-dependent) plus
  **4–16 MB of glyph atlas** per window, in VRAM. The process-side
  costs that dominate the tables — font set, themes, VT state —
  are paid once regardless of window count, and the per-window
  output-generation counter means only windows with new output
  repaint.
- **Typed echo bypasses the output coalescer (v2.18.0).** PTY output
  paints are capped at one per-monitor frame budget so multi-read
  bursts (build logs, streaming output) settle into single frames. When the
  current monitor reports a usable refresh rate, the deadline reserves 250 µs
  of scheduling headroom and remains clamped to 4–33.333 ms; the fallback is
  16.667 ms. This is a scheduling policy, not a claim of lower live latency:
  the two-monitor performance campaign remains the acceptance test.
  Keystroke echo used to ride the same `WaitUntil` deadline, and
  Windows' ~16 ms timer granularity made held-key repeat visibly
  stutter; echo output now requests a redraw immediately
  (`request_redraw` is vsync-coalesced, so it can't outpace the
  display) while non-input bursts still coalesce to one paint per
  frame budget. Text and parser side channels first publish one release-ordered
  output generation and then request a per-pane atomic wake gate. That latch
  stays closed through a deferred interval; redraw reopens it immediately
  before taking the candidate-frame snapshot. If a queued wake was already
  covered by a presented frame, Kettle acknowledges and resamples it so output
  racing the rearm is retained rather than silently closing the gate.

## Reproducing

### Linux / macOS

```sh
cargo build --release -p kettle
./scripts/bench.sh
```

`scripts/bench.sh` requires `time` (GNU coreutils — on macOS use
`gtime` from `brew install coreutils`). Output goes to stdout; pipe
to a file or markdown table as you like.

### Windows 11

```pwsh
cargo build --release -p kettle
.\scripts\bench.ps1
# or via just:
just bench
```

`scripts/bench.ps1` needs PowerShell 5.1+ (preinstalled on
Windows 10+) or PowerShell Core 7+. No external dependencies — uses
the .NET `System.Diagnostics.Process` API directly.

On macOS / Windows expect different numbers from the Linux baseline:
startup is generally faster on macOS arm64, the headless GPU path
uses Metal / DX12 instead of software-Vulkan, and the binary size
differs because the universal2 macOS build is fatter.

## Legacy microbench exclusions

The small `scripts/bench.sh` and `scripts/bench.ps1` microbenchmarks in this
section do not automate live peer windows, display transitions, input latency,
or GPU presentation. The current Windows release harness described above does:
it applies one pinned methodology to Kettle, Windows Terminal, Alacritty,
WezTerm, Rio, and Tabby, and fails closed when configuration, binary, workload,
or physical-display identity is not comparable.

Still outside the automated release evidence are photodiode-grade
input-to-photon latency, long-duration GPU atlas/residency behavior, battery
energy, and thermal throttling. Any result from the legacy scripts is historical
or diagnostic evidence, not a substitute for a valid release-mode comparator
campaign.

## Methodology notes

- **5-run minimum.** Wall-clock and RSS both have system jitter; the
  bench scripts run each measurement 5× and emit all five so the
  spread is visible.
- **Cold-cache start.** Each invocation is a fresh `exec` /
  `CreateProcess`; we don't benchmark inside a long-lived process
  because the user pays the cold-start cost at every shell launch.
- **`/usr/bin/time -v` for RSS** (Linux/macOS): `Maximum resident
  set size` reports peak resident memory in KB; we convert to MB
  in the table above.
- **`PeakWorkingSet64` for working set** (Windows): the
  .NET `Process.PeakWorkingSet64` property is populated by Win32
  `PSAPI.GetProcessMemoryInfo` and is comparable to Linux's max
  RSS — peak resident pages in physical memory for the lifetime
  of the process.
