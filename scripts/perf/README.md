# Cross-terminal performance checks

Kettle keeps native comparison checks for the two supported desktop platforms.
Both commands write machine-readable results under `target/perf-results/`.

## Linux

`linux-compare.sh` compares Kettle with Terminator and Ghostty. Alacritty joins
the run when installed. It measures startup and ASCII/SGR floods, then records
advisory Kettle-only resize and scroll evidence.

```sh
just linux-perf
# or
scripts/perf/linux-compare.sh --runs 7 \
  --out-dir target/perf-results/linux-release-candidate
```

The run requires `hyperfine`, Terminator, Ghostty, and a graphical X11 or
Wayland session. Kettle must beat Terminator and remain within 10 percent of
Ghostty on each comparison timing.

## macOS

`macos-compare.sh` compares Kettle with the installed macOS terminal set. It
records Hyperfine startup results, native maximum-RSS and quiet-CPU samples,
and applies the top-half rank gate. `macos-compare-score-self-test.py` covers
the scorer without launching applications.

```sh
just macos-perf
just macos-compare-score-self-test
```

## macOS standing

`macos-standing.py` measures what the comparator above cannot resolve. It
times each launch on one monotonic clock instead of polling, so startup
differences below 100 ms are visible. It reports memory as `phys_footprint`,
the Activity Monitor figure, which includes GPU driver memory that resident
set size leaves out. Workloads:

- `startup`: spawn to the first on-screen window, and to the child's first
  instruction
- `idle`: CPU share, wakeups per second, and memory of a window left alone
- `flood-memory`: memory while and after printing 32 MiB of seeded text that
  is the same on every run, in lines narrower than the grid. A 100 ms
  timeline runs from launch until 20 s after the text ends, and every
  terminal gets the same columns: the peak, memory 3 s after the end, and
  memory 20 s after it. 20 s falls after Kettle's 10 s and kitty's 15 s
  blink timeouts plus the roughly 1 s the GPU driver takes to release its
  pools, so it shows memory once blinking has stopped. A flood that never
  finishes is an error.
- `vtebench`: Alacritty's vtebench at a pinned revision. On macOS its
  scripts cannot read the window size, so they run from a copy with
  upstream's unmerged fix
  ([alacritty/vtebench#46](https://github.com/alacritty/vtebench/pull/46)).
  vtebench itself stores whole milliseconds per sample, which let two
  terminals 7 % apart share a median, so the harness builds a copy
  (`vtebench-us`) whose one sample line records microseconds. The patch
  must match exactly once or the build stops. A run stops if any benchmark
  comes back without samples.

Every terminal runs with its default configuration on a 120x36 grid. Helpers
under `macos-standing/` are compiled into `target/perf-tools/` on first use,
which needs the Xcode command line tools, and vtebench is cloned and built
there too.

Each workload runs one script, reused for every launch; values that change
per launch go in a file the script sources. macOS assesses a script the first
time it runs, which cost every terminal about 120 ms of shell time when each
launch had its own script, so shell times are not comparable with runs before
this change. One discarded warm-up launch per terminal (`--warmup`) comes
before the startup rounds. Idle and flood windows are brought to the front by
pid before sampling, as a click would, and an idle round counts only if its
window was frontmost when settling began, midway through sampling, and at the
end. Each launch records the machine's thermal state and Low Power Mode.

`--startup-phases` runs Kettle's startup rounds with
`RUST_LOG=warn,kettle::startup=info` and records each phase Kettle stamps (from
`main` through the built event loop, `Resumed`, the first pane's spawn, the
window and the GPU to the first frame) as `phase_<name>_ms` since the launch
probe spawned it, plus the pane's startup path. The format is pinned by
`macos-standing/startup-phases.fixture`, which Kettle's own tests share. Other
terminals launch unchanged, and only startup rounds are stamped.
`--startup-phases b` stamps only the B side of an A/B, so one build on both
sides measures what the stamps themselves cost. It is a diagnostic: the
session never counts, so `--combine` never publishes from it. Every terminal
launches without the harness's own `RUST_LOG`, so a filter set in the shell
never changes what a terminal logs.

Startup rows also report the paired difference in milliseconds, B-A in an A/B
and Kettle minus the best other terminal in a standing, since a ratio alone
hides the absolute gain.

`--kettle-b-config LINE` gives the B side of an A/B extra config lines, with
the same binary unless `--kettle-b` is also given. `--kettle-variant
NAME=LINES` adds an unranked Kettle entry, for example
`opaque=background-opacity = 1;window-blur = false`. `--footprint-detail`
records the `footprint` tool's graphics categories at each flood offset; it
walks the address space and can perturb the process, so a session that uses
it never counts. Extra config lines are recorded in `results.json` only as a
digest, since a line can carry a path or other private value; the text goes in
the local manifest. `--flood-offsets` sets the flood columns (default `3,20`:
done+3 and done+20), and sessions with different offsets never combine.

### Keystroke to screen

`--workloads latency` adds a keystroke-to-screen workload. It is never in the
default list: it posts key presses, and it needs the machine to itself and two
macOS grants for its probe. Run `--latency-check` once to build the probe, ask
for the grants and check them. Pass `--latency-sign-identity` (for example
`"Apple Development"`) to sign the probe with a certificate: macOS keys the
grants to the probe's signature, so an ad hoc probe needs new grants after
every rebuild. The probe rebuilds only when its source or identity changes. A
rebuilt probe can also make macOS ask once more, on screen, to let it bypass
the private window picker; the probe posts nothing while any window covers the
measured block.

- **The payload.** Every terminal runs the same `keyblock`, reading
  `/dev/tty` in raw mode with the cursor hidden and steady. Each byte it
  reads toggles a 16x4-cell reverse-video block in one `write()`, and a
  32-byte record logs when it read and when it wrote.
- **The probe.** `KettleLatencyProbe.app` (`latency-probe.swift`) runs through
  `open` as an agent app, so it holds its own grants and never takes focus.
  It calibrates the block from 6 guarded toggles, then streams just that
  rectangle of the display through ScreenCaptureKit, so compositing,
  translucency and blur are inside the measurement. It posts `j` at the HID
  tap (`--latency-inject pid` posts to the terminal instead, for comparison).
  A key's latency runs from its post to the display time of the first frame
  with at least 95 % of the block flipped, censored at `--latency-censor-ms`
  (500).
- **Guards.** Before every key the measured window itself must be the
  frontmost window (not just its app), with no window in front of its block,
  and no other input may have arrived since the last key; if focus changes
  during a key, the round stops. The key's time is taken after these checks,
  just before it is posted. The probe posts nothing after a deadline the
  harness sets and exits there, so it never outlives its round. A round that
  fails is recorded with its reason, and so is one whose keys did not reach
  the payload one byte each.
- **Halves.** The payload's record for the same key splits it into an input
  half (post to read) and an output half (write to display).
- **What else runs.** A standing adds Kettle opaque and unblurred
  (`kettle-opaque`, off with `--no-latency-kettle-opaque`) and bare-window
  floors (`--latency-floors`, default `ca,metal-sync,metal-nosync`) that show
  the pipeline's cost without a terminal. Both are reported beside the
  terminals and never ranked. An A/B measures its two builds only.

Defaults are 10 rounds, 20 discarded and 100 measured keys per round
(`--latency-rounds`, `--latency-warmup`, `--latency-keys`). The row reports
the mean with a two-stage bootstrap interval: rounds (launches), then keys
within each drawn round, since one launch's keys share a window and a GPU
state. Median, p95, p99 and the two halves come from every counted key of the
session. Kettle is ranked on the mean against the fastest other terminal, with
the ratio and the difference in ms from one bootstrap, and the claim rule
applies as for every other row. A censored key counts at the censor bound, in
every figure. A row with more than 1 % of its keys censored is unranked, and
an entry that lost 3 or more of 10 rounds is not measured; the other entries
still are. An A/B needs both sides ranked, and its gate is in ms: a change
counts when both sessions' difference intervals exclude 0 on the same side
and the smaller difference is at least the larger of 1 ms and twice the
latency A/A's. A change not aimed at latency passes when every difference
interval tops out at +1 ms or less.

Latency counts on its own: a lost latency round never costs the session its
other rows, nor a lost idle round the latency rows, and a refresh rate that
changed mid-session voids latency. When a
whole rotation of latency rounds fails in a row (an alert over the windows,
lost grants), the rest of the workload is recorded as not run. Each round also
records how long after ScreenCaptureKit delivered the frame it was displayed:
about 13 ms on a 60 Hz display, since a frame is handed over before its
scheduled display time.

### Statistics

Rounds rotate the terminal order. Startup (time to window and to shell), idle
(CPU, wakeups, memory) and flood (peak, done+3, done+20) report medians,
because launches have cold outliers; other numbers a round records are kept
but never compared. vtebench reports each benchmark's mean sample per round,
then the mean over rounds, and a geometric mean per round. Kettle is compared
with the best other terminal round by round: the median (or, for vtebench, the
mean) of the per-round ratios, a 10,000-resample bootstrap 95 % interval over
rounds, and the number of rounds Kettle won. Idle rows count only rounds in
which the terminal was frontmost, since blinking cursors run only in a focused
window.

Publication defaults are 30 startup, 5 idle, 5 flood, 5 vtebench and 10
latency rounds, 10 s per vtebench benchmark (upstream's default), a 30 s idle
window and the 256-descriptor limit the Dock gives apps. `--rounds` overrides
every count.

### Sessions, preflight and labels

A run first checks the machine and refuses battery power, Low Power Mode, a
locked screen, a load of 2.0 or more (after waiting up to `--wait-quiet`
minutes), a Time Machine backup, local changes under `scripts/perf`, a build
or review tool (including `codex` and `claude`) using 10 % CPU or more, or a
measured terminal that is already open. It waits for load and busy tools to
settle first. Every such tool found is recorded. A
state it cannot read (a failed `ps`, an unknown power mode) refuses too.
`--preflight-only` runs just this check. `--allow-noisy` runs anyway and marks
the session not countable. When the shell running the harness lives inside
one of the measured terminals, pass that terminal's pid as `--host-pid`: it
must be an ancestor of the harness, only it may stay open, and it is recorded
in the results.

The harness never signals a process itself. It asks the launch probe that
started a terminal to stop it: the probe has not yet reaped that terminal, so
its pid cannot belong to anything else, and it deletes the pid file the moment
it does reap it. The self-test bans name-based signals under `scripts/perf`.

Kettle must be inside an `.app` bundle, because a bare binary skips AppKit's
bundle-only work and idles differently from what users run. Left at its
default, `--kettle` builds the checkout and measures that build inside an
ad-hoc signed copy of the installed app. `--make-bundle BINARY APP` does the
same for any build, for A/B comparisons. `--allow-bare` measures a bare
binary for diagnostics, and such a session never counts.

A session counts only if its preflight was clean, Kettle ran from a bundle,
and every requested round finished without an error and with its values. A
terminal that does not stop when asked, or that its launch probe had to kill,
fails its round; a probe that stops responding is killed with its process
group. Each run writes a new directory and refuses one that already holds a
session. `results.json` is rewritten after every
row, so an interrupted session keeps its data but not its standing. It records
the harness version
(the tree hash of `scripts/perf`), each terminal's version, hash and
signature, the display mode, power state and per-row load.
Each terminal's path and signing team go in `local-manifest.json` beside the
results, never into `results.json` or anything combined from it.
`--combine DIR...` merges sessions into `combined.md` and `combined.json`:

- every countable session must share the same setup: harness version,
  terminal binaries, machine, macOS build, display, descriptor limit, round
  counts, durations and configs. A change starts a new session set;
- the published value for each terminal is the median of its estimates in
  countable sessions, with the range; every session's own estimates are kept
  alongside for diagnosis;
- a row's comparison in a session counts only when at least 80 % of its
  rounds are paired (idle rounds that lost focus have no pair);
- labels read only the first countable session on each of the first 3
  dates, so later sessions never change them. Kettle's label is "1st" only
  when all 3 have the ratio's interval below 1 and Kettle lower in at least
  80 % of rounds; "tied 1st" when none has Kettle clearly behind; otherwise a
  rank of 2nd or lower, marked "(varies)" when sessions disagree;
- for A/B sessions, a change counts when the first 2, on different dates, both
  exclude 1 on the same side. `--aa DIR` adds each metric's gate from an A/A
  session: the larger of 3 % and twice the A/A interval's half-width. The A/A
  must itself be a countable session of one build and config against itself,
  with the same setup and baseline config as the A/B sessions (its round
  counts may differ, since fewer rounds only widen its gates); an A/A whose
  interval excludes 1 invalidates that metric's verdict, and a row the A/A did
  not measure gets no verdict.
- a round with no value is dropped from its pair, but a zero is a value: two
  zeros tie, and anything over a zero is infinitely worse.

`--combine` also reads 4.7.0's results layout, rebuilding vtebench means from
its `.dat` files.

```sh
just macos-standing-session 480-s1 --host-pid <pid>
just macos-standing --kettle /path/to/A.app/Contents/MacOS/kettle \
  --kettle-b /path/to/B.app/Contents/MacOS/kettle --no-build --workloads startup
just macos-standing --combine target/perf-results/sessions/*-480-s*
```

`just macos-standing-self-test` checks the parsing, the statistics, the claim
rule, the preflight parsers and the vtebench patches without a desktop.

## Shared probe

`kettle-live-probes.py` owns the bounded Kettle resize and scroll probes used by
the platform scripts. Those timings are useful regression evidence but are not
cross-terminal claims unless the peer tools measure the same boundary.

The former Windows PowerShell acquisition and scoring suite was removed when
Kettle stopped distributing Windows builds in 4.0.0. Historical Windows
measurements remain in `docs/PERFORMANCE.md` with their original scope.
