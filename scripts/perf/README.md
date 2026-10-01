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

Every terminal runs with its default configuration on a 120x36 grid. A
terminal can start its child before its first resize (Ghostty sometimes starts
it at the previous window's size), so every payload waits, up to 5 s, for its
terminal to reach 120x36 before its workload runs; startup rounds, whose child
must start at once, wait after their one-second hold instead. Every round
records the grid it settled at, and the grid it started at when that differs.
A smaller grid does less work, so a round that never reached 120x36 fails, and
a terminal whose first launch in a session does not reach it refuses the
session there. Ghostty 1.3 opens a
new window at the last Ghostty window's frame, which it keeps in the user
default `NSWindowLastPosition`, and ignores `window-width`/`window-height`
while that is set, so a tiled Ghostty window would set the measured Ghostty's
grid. A session that measures Ghostty saves that default, clears it before
every Ghostty launch, and puts the saved value back when the session ends.
A keeper process holds the saved value and makes every change, so no clear
can still be in flight when the value goes back; it also puts the value back
if the run ends any other way, SIGKILL included. It first waits for a
measured Ghostty still running to close, since a closing Ghostty writes its
own frame; the launch probe stops its terminal as soon as the harness is gone,
so that wait is short. Until the value is back, the session folder's
`ghostty-frame-restore.txt` holds the command that restores it by hand. The
user's own Ghostty windows are never touched, but a frame they write during a
session is replaced by the saved one at its end.

Helpers under `macos-standing/` are compiled into `target/perf-tools/` on
first use, which needs the Xcode command line tools, and vtebench is cloned
and built there too.

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
macOS grants for its probe. Prepare it explicitly with
`--latency-check --rebuild-latency-probe` before the grant/source freeze.
This builds a scratch bundle, signs and verifies it, runs its pure self-test,
then publishes it with a private version-2 build receipt outside the seal.
`--rebuild-latency-probe` is valid only with `--latency-check`. Later checks
reuse the verified app. Normal runs never rebuild, re-sign or request grants.
An absent, legacy, malformed, incomplete or changed cache is refused before
check, request, self-test or measurement. A legacy source-only receipt cannot
be promoted from cached bytes. It needs one explicit trusted rebuild.

The receipt binds the source and exact generated plist, compiler and SDK
identities, build command, executable, bundle paths/modes/bytes, identifier,
empty entitlement set, seal and signature requirement. A valid signature on
a replacement app is insufficient. Preparation publishes the receipt last
under a preparation/use lock, so interruption cannot accept a half-built app.
A campaign holds that lock and revalidates outside each probe invocation's
measured epoch. File identities around hashing also detect replacement during
use. A private locked invocation lease lets the probe stop posting and exit
on cancellation or owner exit; the harness reaps its own `open` child.

`tool_hashes.latency-probe` identifies the verified bundle bytes.
`tool_artifacts.latency-probe` and latency rows retain its bundle/executable
hashes, separate source hash, identifier, CDHash and signing mode. Absolute
paths, signing identity, team and certificate details remain in the private
receipt and local manifest. These checks cover harness-owned builds and
accidental or concurrent replacement. They do not attest against a same-user
attacker who can replace both the receipt and the harness.

macOS keys grants to the probe signature. An ad-hoc rebuild may require new
grants, including the private window-picker approval. Freeze the producer
before the owner grants it. `--latency-sign-identity` selects a local
certificate as an explicit owner choice; use the same choice on subsequent
checks and campaigns. Refusal does not repair a cache or prompt. The probe
posts nothing while another window covers the measured block.

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
the mean of the round (launch) means with a Student-t interval over rounds,
since one launch's keys share a window and a GPU state. Median, p95, p99 and
the two halves come from every counted key of the session. Kettle is ranked on
the mean against the fastest other terminal: the difference in ms has a
Student-t interval on the per-round differences, the ratio is the geometric
mean of the per-round ratios with a t interval on their logs, as elsewhere,
and the claim rule applies as for every other row. A censored key counts at the censor bound, in
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
with the best other terminal round by round: the geometric mean of the
per-round ratios, a Student-t 95 % interval on their logs, and the number of
rounds Kettle won. A cold outlier widens that interval rather than moving a
median, so it can only make a claim harder. A terminal's own median carries
the distribution-free order-statistic (sign-test) interval, and its own mean a
Student-t interval. Idle rows count only rounds in which the terminal was
frontmost, since blinking cursors run only in a focused window.

Why not a bootstrap: over the 5-10 rounds a session has, a percentile
bootstrap covered only about 0.85-0.93 of simulated same-binary sessions at a
nominal 0.95; the t interval keeps 0.95 (`macos-standing-self-test.py` checks
both). An A/A also judges vtebench's benchmarks together: each at the
Bonferroni level for the 12 of them (1 - 0.05/12), the geometric mean at 95 %.
Judged each at 95 %, an A/A with no real difference fails most of the time.
Every gate still comes from the 95 % half-width.

Publication defaults are 30 startup, 5 idle, 5 flood, 5 vtebench and 10
latency rounds, 10 s per vtebench benchmark (upstream's default), a 30 s idle
window and the 256-descriptor limit the Dock gives apps. `--rounds` overrides
every count.

### Typed analysis and current statistics

New runs write schema 3 with `evidence_contract = hc-v1` and
`meta.statistics_policy = current`. `analysis.json` records the metric
contracts and statistics for the session. `combined.json` keeps these reports
per session when an input uses schema 3 or contains a new metric. With only
existing schema-1/2 data, `summary.md`, `combined.md`, `combined.json` and
`results.json` retain their original bytes. Reading a session never rewrites
it. Schemas 1 and 2 retain their original measurements and countability rules.
Missing optional fields remain unavailable. Schema 1 still reconstructs
vtebench means from `.dat` files and cannot count toward a publication claim.

Each metric has an ID, unit, direction, analysis kind, extraction and
eligibility rule, estimate, comparison, A/A kind, claim kind and publication
role. Dispatch uses that contract. A memory field under `latency` uses a ratio
gate and differences in MiB. CPU differences use percentage points; wakeup
differences use /s. Signed phase slack uses differences and remains
diagnostic. Reserved typing, printing, blink, cursor and startup fields do not
imply that a collector ran. No new workload is enabled by this schema change.

`statistics.current` contains the existing estimates and intervals described
above and remains authoritative. Scalar comparisons also report the mean of
paired absolute differences with a Student-t interval in their own unit.
Combine retains startup differences and the new memory/rate differences.
Finite zero samples retain the existing A/A gates and verdicts, including
infinite derived ratio intervals. No gate is dropped merely because a ratio
interval is unbounded.

The PR #409 estimators are the only statistical policy. No supplemental
bootstrap reports or estimator selection are emitted.

Analysis adds all pairwise comparisons among ranked entries and adjacent
`ordered`/`tied` labels. Scalar peers use the current geometric mean ratio
and log Student-t interval. vtebench retains its existing Bonferroni family
for A/B comparisons, including A/A controls.
An adjacent order requires a ratio interval entirely above one. Mean latency
peers use the current paired launch difference Student-t interval, and an
adjacent order requires a difference interval entirely above zero. Overlapping
intervals and missing paired data produce ties. The existing best-other
comparison, numeric rank and cross-date claim rule remain unchanged. Markdown
labels appear after the completed tables and only when new metrics are present.

Optional metrics require `metric_validity` evidence with a valid flag,
capability version and complete expected/observed coverage. Their failures
remove only that metric's value, preserving round positions as `null`.
Analysis and combine report metric countability per terminal, reasons and
failure counts. New optional metrics require all planned rounds for that
terminal; defaults retain their existing rules. vtebench aggregates keep the
session's benchmark set fixed. A missing, nonfinite or nonpositive member
makes that round's aggregate unavailable; it cannot change the geometric mean
to a subset of benchmarks. Its failure and incomplete-round countability are
reported. Flood fields accept every `:g` offset spelling, including
`done1e-05_mib`. Distribution-only latency fields have no gain gate. See [the
analysis schema](../../docs/perf-standing-schema.md) for field names and
absence rules.

### Managed config closure and assets

Each new campaign records `meta.config_closures` alongside the existing
extra-text `meta.configs` digests. A closure contains a resolver version,
a template digest, logical asset roles with byte sizes and SHA-256 digests,
and a digest of that record. It covers each Kettle side and variant, the
Ghostty managed file, and all peer config-bypass arguments and grid overrides.
The extra-text digests and default generated config bytes remain unchanged.
Existing schema-1/2/3 result files retain their report bytes when read.

The Kettle registry comes from `kettle-config`'s parser. Its only consumed
file-valued setting is `background-image`, including `background_image` and
case variants. The last assignment wins, including an empty assignment.
The tokenizer strips one matched pair of quotes and trims whitespace. Full-line
comments are ignored; a `#` inside a value is literal. `record-dir` is an
output directory and is refused when nonempty in a countable managed config.
Theme and font settings select bundled themes and system font families; they
are not paths. This closure does not snapshot OS fonts or the terminal app.
Terminal and method identities remain separate.

A leading `~/` resolves using the renderer's HOME, USERPROFILE, APPDATA order.
Other relative references resolve against the pinned app launch cwd, which is
the harness invocation directory. They do not resolve against the generated
config directory. Environment substitutions are refused. The harness follows
asset symlinks only to bounded, readable regular files, checks the opened inode
and read stability, and captures the bytes under their full SHA-256 address.
Dangling links, directories, FIFOs, oversized files and unstable reads refuse
the campaign before any row runs. The v1 capture limit is 64 MiB per asset.
This bound does not certify that an image decoder can render the asset.

For example, with two explicit app bundles and an asset in the invocation
folder:

```sh
python3 scripts/perf/macos-standing.py --no-build \
  --kettle A.app/Contents/MacOS/kettle \
  --kettle-b B.app/Contents/MacOS/kettle \
  --kettle-b-config 'background-image = ./wallpaper.png' \
  --out-dir CAMPAIGN
```

The generated B file points to its private captured asset. Changing the source
file afterward does not change the consumed bytes. Source changes observed at
cleanup are recorded privately. Changing B's captured asset in a later campaign
is an intentional B-only difference. A must still match the A/A baseline.
An A/A must have identical effective closure on both sides. Combined sessions
must share every closure. A control without closure evidence cannot certify a
new captured setup. The public digest normalizes generated asset paths to
logical roles, so an identical setup in another campaign directory compares
equally. Other settings and earlier overridden assignments remain hashed.

Consumed inputs remain in `private-config/` beside raw results. Directories
are private and sealed configs/assets are read-only. Every collection callback
is bracketed by checks outside its measured interval. A changed sealed file or
an added config dependency stops collection, preserves completed raw data and
the invalid collected row, and prevents a countable final result. These checks
cover accidental or concurrent persistent changes at the boundaries; they do
not establish an adversarial same-user trust root.

Countable Kettle extras are section-free declarative settings, bounded at
1 MiB. Kettle has no include syntax, so includes, including recursive cycles,
are refused rather than interpreted by a separate resolver. Nonempty unknown
keys and dynamic settings such as commands, environment assignments, triggers,
keybindings and scripts are refused. Session restoration and automatic profile
splitting are also refused when enabled. New file-reference types require a
parser audit before admission. Arbitrary Lua or dependency-bearing configs
need a separate declared-input contract before they can count.

Every launch pins an isolated XDG config root. This also blocks Kettle's
automatic `init.lua` discovery, which uses its default config directory even
with an explicit `--config`. Ghostty receives only the generated file. Alacritty
uses `/dev/null`, kitty uses `NONE`, and WezTerm uses its config-skip flag.
Peer config-directory/file environment overrides are removed. These generated
peer layouts support no includes or user Lua; added files or changed contents
fail the boundary check. Native peer isolation and background-image rendering
still need an excluded functional pilot on the actual installed apps.

`local-manifest.json` retains original/resolved paths, source identity, rewrite
mappings and generated templates. It is written atomically with mode 0600.
Public JSON and Markdown contain only digests, sizes and logical roles for
these inputs. Closure refusals never print config text or source paths.
The manifest and `private-config/` are private audit artifacts, never public
report input. Review them before sharing a campaign directory.

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
the harness version, each terminal's version, hash and signature, the display
mode, power state and per-row load. The harness version is a hash of what a
session runs: every file under `scripts/perf` as it is on disk, except the
ones no session runs or reads (this README, the two self-tests, the test
fixtures under `macos-standing/`, the other perf tools, Python's
`__pycache__` and `.DS_Store`). A merge that changes only those keeps a set
of sessions comparable, and any file nobody listed counts. A session refuses
to start while those files differ from the commit: an edit, an untracked or
ignored file (a sourceless `.pyc` here would replace a standard module), or
a change a Git index flag hides.
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

### Retained startup and native diagnostics

Evidence postprocessing reads files and prints diagnostic JSON to stdout. It
launches no application, builds no helper and posts no keys. Its output always
has `diagnostic_only: true` and `countable: false`. It does not modify input
rows, session results, ordinary logging or standing metrics.

```sh
python3 scripts/perf/macos-standing.py --startup-input results.json --startup-grid-policy child
python3 scripts/perf/macos-standing.py --startup-phase-input startup.stderr
python3 scripts/perf/macos-standing.py --native-layer-input analysis.json
python3 scripts/perf/macos-standing.py --trace-input echo.log
```

`--startup-input FILE...` reads `workloads.startup` from retained results and
checks measured `kettle`, `kettle-a` and `kettle-b` rows. Warmups are excluded.
Peers keep their existing settled-grid policy. The default diagnostic policy is
`settled`. `child` requires both `cols,rows` and explicit
`start_cols,start_rows` to equal 120x36. Missing child fields are incomplete;
old rows elide a matching initial grid, so a settled grid cannot reconstruct
that observation. `native` also requires a complete native history. These
policies apply only to postprocessing, not to an ordinary launch.

The provisional `native_pty_v1` format has no application producer yet. A row
supplies `launch_id`, `pane_id`, `started_ns`, `child_observed_ns` and its two
explicit grids. Its optional `native_pty` object contains:

- `version: native_pty_v1`, `clock: CLOCK_UPTIME_RAW`, matching `launch_id` and
  `pane_id`, `complete: true`, `dropped: 0`, `overflow: false`.
- `recording_start_ns`, `created_ns`, `initial.t_ns`, `recording_end_ns` and
  `initial_stage: after_create_before_correction`. Recording starts before
  creation, includes the initial observation before the child, spans at least
  two actual seconds and ends no earlier than the child observation.
- `initial` and `final` with positive integer `cols`, `rows`, `pixel_width`,
  `pixel_height` and `t_ns`. The final timestamp equals the recording end.
- `child_observation` with `child_observed_ns`, `start_cols`, `start_rows` and
  `sigwinch_count: 0`, linked to the row.
- `event_count` and `events`. Events have contiguous integer `seq` starting at
  1, increasing `t_ns` inside the recording, matching launch/pane identities,
  `requested` and `observed` geometry, `outcome: ok|error|noop`, `native_error`,
  `reason: initial|window|monitor|config` and boolean `signal_sent`.

Strict acceptance requires an exact initial native grid. Every event must
preserve all four geometry values, report no native error and send no signal.
Failed attempts, missing events or recording endpoints, changed geometry,
wrong identities and overflow cannot prove zero resize. Missing native data
is `unavailable`, never a legacy pass. A supported provisional fixture is not
native runtime evidence. The producer owner must confirm the layout and prove
recording covers every attempt before this can certify S3.

`--startup-phase-input FILE...` accepts one retained stderr file per round.
The S1/S2 lines are `startup phase=NAME t_ns=N since_main_ms=M thread=main|fonts`
and `startup path=resumed_early|after_renderer|unknown`. `since_main_ms` may be
`-`, and the optional thread suffix may be absent in older logs. Other log
lines are ignored. Raw nanosecond stamps, threads, paths, duplicate conflicts
and malformed-line counts remain diagnostics. The first valid stamp wins;
a conflicting or malformed endpoint makes its derived interval unavailable.
Unknown optional stamps stay outside the published phase list.

Font join wait is `fonts_joined - fonts_join_start`. Signed font slack is
`resumed - fonts_ready`, and config overlap is `fonts_ready - config_loaded`.
Font intervals require explicit thread and known path attribution. GPU
initialization is `gpu_ready - window_created`; event loop construction is
`event_loop_built - run_with`. Ordered durations reject reversed endpoints.
The report derives values per round, then reports median, interpolated p95,
maximum, coverage and thread/path counts. It never subtracts endpoint medians.
`--startup-started-ns N` optionally provides the launch origin for one phase
file. Intervals need no launch origin because their endpoints share the
producer clock. Historical `phase_*_ms` and `startup_path` remain unchanged.

S2 emits no monitor agreement or reported font wait, so those fields remain
null. S4 emits no device/pipeline subphase endpoints; only the GPU interval
above is available. `first_output_ms`, its origin and endpoint remain null.
A first-frame present or per-key `output_ms` cannot certify displayed first
output.

`--native-layer-input FILE...` reads the cursor layer smoke's `analysis.json`.
It preserves numeric `contract.handoff_after_s`, `contract.idle` fields
`span_s,peak_mib,wakeups_per_s,cpu_percent`, and the selected `cursor_blink`
states `handoff,rested,after_reload,after_key`. Each state has `renderer`,
`handoffs,exits,hides` and aggregate `exit_frame_us.count,p50,p95,max`.
An empty exit history has null percentiles and maximum 0. These aggregates
are descriptive. The producer does not export raw samples, actual sampling
bounds, the timeout or the twenty geometry-read states/timestamps. Thus the
adapter marks interval peak, geometry polling and acceptance incomplete.
It cannot certify the handoff +1.5 seconds to timeout interval or a per-key
exit join from aggregates or unsequenced `exit_frame_us` log lines.

`--trace-input FILE...` reports private echo trace capability as unavailable.
The row-shaping producer currently contains no emitted private trace format.
The proposed `latency_trace_v1` JSONL layout is not an agreed producer format.
An owner-provided wire sample and explicit event mapping are needed before
parsing skipped prepares, actual `emit_pane_glyphs`, nested intervals or echo
joins. Snapshot, flatten and upload events cannot stand in for glyph emission.
Clock identity and echo/payload bounds must be supplied before any duration or
savings can be reported. This mode never promotes traces to countable results.

Inputs are bounded regular files. Diagnostic JSON rejects duplicate members
and nonfinite constants. Reports exclude source paths, raw stderr, launch/pane
identities and arbitrary payload text. Invalid input produces a generic refusal.

### Paced printing and launch blink window

The four default workloads remain `startup,idle,flood-memory,vtebench`. Select
`output-memory` or `blink-window` explicitly. Both default to ten rounds, with
`--output-memory-rounds` and `--blink-rounds` overrides. These collectors use
an owned native observer at absolute 100 ms deadlines. It writes bounded JSONL
with query start/end times, cumulative counters, process start identity, and
timestamped focus/window checks. It never signals the target. The launch
helper owns and reaps the observer before releasing its target PID, including
when the target exits unexpectedly. Existing idle and flood observers are
unchanged.

Printing emits 80 numbered lines, at absolute deadlines 0 through 7900 ms,
then records completion at or after 8000 ms. Every line is `NN: The quick
brown fox jumps over the lazy dog 0123456789` followed by a newline, NN 01
through 80. The complete payload is byte-pinned. The grid, activation, target
window and first successful observer query must be ready before the output
barrier opens. The first query wholly at or after began+6 s supplies
`printing_mib` and the same query supplies descriptive lifetime
`printing_max_mib`. Its completion must precede done and be no later than
began+6.25 s. Known focus loss anywhere in the output interval invalidates the
result. No later sample substitutes for a designated query that lost focus.
Both focus checks must bracket the query, stay within 250 ms of the
corresponding query boundary, and advance beyond the preceding sample's focus
checks. All lines and the done marker are required; line completion lateness
over 250 ms or a schedule outside 8..8.25 s fails.

Blink observes a quiet payload with shipped cursor behavior. It changes no
cursor escape sequences or defaults. The interval is launch+2.5..+8.5 s;
readiness and activation must finish before the first boundary. It divides
cumulative CPU/wakeup deltas by the actual endpoint query span. Endpoint current
footprint and descriptive current interval median/peak remain separate from
lifetime maximum. Coverage must reach 80%, including endpoint coverage, with
no uncovered gap or query lateness over 250 ms. Every retained query must have
valid exact-window checks. Alternate `--blink-settle` or `--blink-window` values
are diagnostic and cannot count under this method.

`blink_activity` is `verified`, `disabled-default`, or `unproven`. Only verified
setup evidence permits active-blink comparisons. Use `--blink-disabled-default`
to name terminals whose shipped cursor does not blink; their quiet-window
numbers stay descriptive. Missing, malformed or mismatched validation leaves
the setup unproven. Unknown display identity cannot certify a setup.

Run `--blink-validate-only` in a separate owner measurement window, with
explicit bundles, `--no-build`, a fresh `--out-dir`, and these metadata flags:

- `--blink-cursor-rect x,y,width,height`, a cursor-only crop in window points
  determined in an excluded pilot, at most 256 points in each dimension.
- `--blink-shape NAME`, the shipped cursor shape.
- `--blink-timeout SECONDS`, the shipped timeout, or 0 for none.

The dedicated probe's `--blink-check` mode requires only Screen Recording. It
posts no keys, clicks or pointer motion, performs no block calibration, and
requests no grants. It uses the same verified bundle invocation through `open`,
preparation/use lock and private invocation lease as latency. The receipt is
verified before and after capture; lease loss cancels capture.
`--memory-sample-ms` accepts 50..1000 ms. The default is 100; overrides
change the method identity and are diagnostic. Idle and flood ignore this flag.

The probe bundle's grant attribution must pass the live preparation pilot
before freeze. The capture uses the launch helper's exact window and a 100 ms
cursor-area stream. It hashes complete pixels; idle stream notifications
confirm unchanged complete pixels. Partial/stopped frames never refresh them.
Evidence retains frame hashes, arrival/observation timestamps, native display
dimensions/refresh, crop, shape, timeout, and launch-origin boundaries.
Exactly two stable states and repeated transitions near both ends, with no
stopped interior interval, are required. This strict pixel rule can leave
noisy or antialiased captures unproven; inspect the pilot rather than relaxing
the rule after seeing results.

Pass the resulting JSON with `--blink-validation FILE` for a counted
invocation, using the same crop/shape/timeout flags. Binary bytes, sealed
location-independent configuration closure, display, interval and native
display identity must match. A post-set validation uses
`--blink-validation-before FILE` to retain the prior content hash. Both
validation artifacts and their linkage must be inspected before publication.
Separate validation supports the unchanged setup, not continuous phase
observation in every counted round. Counted runs never capture pixels. No A/A,
gate or live pilot has been established by synthetic fixtures.

The printing and observer helpers prepare separately. The blink probe uses
the receipt-v2 verified preparation/use path. Both new workloads launch from
the canonical session directory through the sealed managed configuration
closure, with row checks before and after collection. Printing publication
remains an explicit owner choice.

Imported blink frames require integer observation and arrival timestamps,
nondecreasing arrivals and strictly increasing observations. An arrival must
precede its observation by at most 250 ms. Linkage IDs contain decimal digits;
linkage digests are null or 64 lowercase hexadecimal digits. Public evidence
projects only these validated fields. Trace read failures use logical errors;
trace paths and files remain in the private session directory.
