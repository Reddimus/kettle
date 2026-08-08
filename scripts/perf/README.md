# Cross-terminal performance harness

This directory contains the pinned Windows methodology behind comparative
claims in `docs/PERFORMANCE.md`. The release suite measures Kettle, Windows
Terminal, Alacritty, WezTerm, Rio, and Tabby in one session. The Linux desktop
suite remains a separate Terminator/Ghostty smoke gate.

## Windows modes

`perf-all.ps1` defaults to `-Mode release`.

- `release` is the publication gate. It requires PowerShell 7, all six
  terminals in the canonical order, every probe, the pinned seed, sample
  counts, 15-second cooldown, 1280×800 comparator client, isolated comparator
  configurations, one reviewed comparator campaign, stable display provenance,
  and no skipped or censored evidence beyond the stated latency allowance. A
  `current` candidate is built from the clean checkout. A `baseline` candidate
  must be the exact GUI binary extracted from a previously verified signed
  release archive and is pinned by its full source commit and SHA-256. The
  scorer requires both compatible release-mode runs and rejects noncanonical
  caller thresholds independently of the manifest. Release mode does not
  accept `-ManifestOnly`, `-KettleConfig`, comparator executable/environment
  overrides, `-AllowUnidentifiedDisplay`, or probe-skipping switches.
- `smoke` is for discovery, parser checks, and short local experiments. It
  permits custom counts, explicit Kettle configuration, skipped probes, and
  `-AllowUnidentifiedDisplay`. Smoke output is never release evidence.
  `-ManifestOnly` writes discovery, configuration, schedule, toolchain, and
  display provenance without opening terminal windows.

Examples:

```pwsh
# Read-only discovery/configuration smoke. This is not release evidence.
pwsh -NoLogo -NoProfile -File scripts/perf/perf-all.ps1 `
  -Mode smoke -ManifestOnly -AllowUnidentifiedDisplay `
  -Label ("topology-" + (Get-Date -Format 'yyyyMMdd-HHmmss'))

# Extract the previous signed release archive, then pin its exact GUI binary.
$baselineExe = (Resolve-Path 'C:\path\to\previous-release\kettle.exe').Path
$baselineTag = (git describe --tags --abbrev=0).Trim()
$baselineCommit = (git rev-parse "$baselineTag^{commit}").Trim()
$baselineSha = (Get-FileHash -LiteralPath $baselineExe -Algorithm SHA256).Hash
$baselineLabel = "baseline-$baselineTag"
$baselineDir = Join-Path 'target/perf-results' $baselineLabel

# Run the prior binary through today's locked harness on today's machine.
pwsh -NoLogo -NoProfile -File scripts/perf/perf-all.ps1 `
  -Mode release -KettleCandidate baseline -KettleExe $baselineExe `
  -SkipKettleBuild -ExpectedKettleCommit $baselineCommit `
  -ExpectedKettleSha256 $baselineSha -Label $baselineLabel

# After the candidate changes are committed, capture a separate live run.
# Every label must be new; even an existing empty directory is rejected.
pwsh -NoLogo -NoProfile -File scripts/perf/perf-all.ps1 `
  -Mode release -KettleCandidate current -Label release-candidate

pwsh -NoLogo -NoProfile -File scripts/perf/score.ps1 `
  -Mode release `
  -ResultsDir target/perf-results/release-candidate `
  -BaselineResultsDir $baselineDir `
  -RequireLatency -RequireMenuHover -RequireVtebench `
  -RequireMonitorTransition
```

Verify the prior archive's signed update manifest and archive checksum before
extracting it; do not treat a similarly named local executable as a baseline.
The full expected commit must resolve to a commit object in this repository and
be an ancestor of the current checkout. The harness verifies that commit, the
exact GUI SHA-256, and the colocated CLI's embedded clean source identity. It
records the acquisition as an external pinned release rather than a local
build.

## What each script owns

| Script | Responsibility |
| --- | --- |
| `perf-all.ps1` | Creates one new retained-handle result directory, builds the release candidate, records machine/GPU/display/power/git/toolchain provenance, locks run-local configs, runs all probes, and invalidates the evidence on any continuous display-change event or probe-boundary topology mismatch |
| `display-stability.ps1` | Subscribes to Windows display-setting changes and publishes ordered change events; `perf-all.ps1` captures and signs the full topology snapshots at probe boundaries, including switch-away-and-back cases |
| `comparator-campaign.ps1` | Strictly parses the reviewed campaign, validates exact peer source/version/tree/executable/signature identities, and retains a read lease for every staged file during measurement |
| `setup-comparator-campaign.ps1` | Downloads only the campaign's HTTPS allowlist before measurement, safely expands bounded archives into a new append-only local campaign, and fully revalidates offline reuse |
| `isolated-configs.ps1` | Creates validated, run-local Kettle, Alacritty, WezTerm, Rio, and Tabby configs |
| `schedule.ps1` | Produces deterministic seeded Williams-balanced orders for startup, idle, latency blocks, and throughput rounds |
| `release-contract.ps1` | Defines the immutable release acquisition and scoring profiles, including terminal order, seed, sample counts, geometry, and absolute gates |
| `display-identity-contract.ps1` | Normalizes signed/unsigned Windows output technologies and applies the explicit physical-connector allowlist |
| `startup-ready.ps1` | Implements the bounded parent/child GO, paint, DSR, and READY protocol used by startup and idle measurements |
| `startup-idle.ps1` | Measures controlled startup, fresh terminal-tree working set, and attributable idle CPU |
| `go-signal.ps1` | Publishes the locked, unpredictable GO capability used to start throughput only after exact window placement |
| `throughput-channel.ps1` | Transfers one bounded workload result over an authenticated current-user named pipe without a path-based sample handoff |
| `latency.ps1` | Measures comparative typed-echo latency in six balanced blocks and binds every raw row to the exact `cmd.exe` path and SHA-256; censored samples remain visible and bounded |
| `menu-hover.ps1` | Measures Kettle context-menu hover transitions for the fixed comparator window and the native-display window |
| `payload-contract.ps1` / `gen-payloads.ps1` | Pin and generate the exact ASCII, SGR-heavy, and Unicode UTF-8 payloads |
| `run-inside.ps1` / `throughput.ps1` | Measure console-write start through terminal parser-drain response and post-flood terminal-tree memory |
| `vtebench-wsl.ps1` / `vtebench-inside.ps1` / `vtebench-dat.ps1` | Build the pinned vtebench revision in a fresh WSL target, bind the exact Linux source/toolchain state before and after every terminal leg, transport its bounded result privately, and validate raw `.dat` evidence |
| `wsl-launcher.ps1` | Pins, locks, hashes, versions, and invokes the exact Windows WSL engine and distribution used by vtebench, with bounded output and exact process-group cleanup |
| `monitor-transition.ps1` | Alternates a live Kettle window between two physical screens and measures recovery with the menu closed and open |
| `statistics.ps1` | Deterministic medians, percentiles, Theil-Sen drift, and paired-cluster bootstrap primitives, with a pinned in-memory C# kernel for the 10,000-resample hot loop |
| `release-statistics.ps1` | Confirmed peer wins/losses, throughput round gates, and release drift policy |
| `baseline-statistics.ps1` | Matched current-versus-baseline non-inferiority policy |
| `score-statistics.ps1` | Converts validated raw probe records into matched metric clusters and applies the release/baseline statistical gates |
| `evidence-snapshot.ps1` | Opens a bounded, no-follow, strict-UTF-8 snapshot of scorer inputs and retains identity locks while scoring |
| `harness-provenance.ps1` | Hashes and read-locks every production harness script for the complete live run |
| `score.ps1` | Verifies raw evidence and provenance, applies the release and baseline policies, and writes `score.json` |
| `sanitize-results.ps1` | Creates a new JSON-only public bundle while preserving the private raw result directory |
| `*-self-test.ps1` | GUI-free positive, negative, tamper, and cross-PowerShell fixtures |
| `self-test.ps1` | Runs the complete discovered self-test set in a fresh copy of the current PowerShell engine |

`linux-compare.sh` and `kettle-live-probes.py` are independent Linux desktop
smoke tools. `just linux-perf` measures startup and ASCII/SGR floods against
Terminator and Ghostty and records advisory Kettle-only resize/scroll evidence.
Those results do not substitute for the Windows release suite.

## Prerequisites

- Windows release: PowerShell 7, the Rust toolchain, an interactive foreground
  desktop, Kettle from this checkout, the campaign's exact installed Windows
  Terminal Appx version, and a fully staged reviewed comparator campaign under
  `%LOCALAPPDATA%\KettleBench\campaigns`. Acquire or revalidate the pinned
  assets before the run with:

  ```pwsh
  pwsh -NoLogo -NoProfile -File `
    scripts/perf/setup-comparator-campaign.ps1
  ```

  Release acquisition immediately re-enters that setup in offline mode and
  rejects ambient discovery plus explicit `KETTLE_PERF_*_EXE`/parameter
  overrides. Smoke mode may still use normal installed discovery and explicit
  executable overrides.
- WSL vtebench: one explicitly selected WSL distribution with Rustup/Cargo,
  GNU `timeout`, and util-linux `setsid` and `script`, plus a clone of
  `alacritty/vtebench`. The harness verifies and builds the pinned
  `ead80032e57dee2e75f0b51f2ea67528647d9944` revision in a fresh target.
  It launches every command with the exact registered distribution name and
  records that distribution's OS, kernel, architecture, and user identity.
- Hardware: a quiet, plugged-in machine and two eligible EDID-backed physical
  screens for a complete release. Close unrelated GPU/CPU-heavy applications
  and do not type or move the mouse during foreground input probes.
- Linux desktop smoke: `hyperfine`, `terminator`, `ghostty`, optional
  `alacritty`, and a graphical X11/Wayland session. The script builds
  `target/release/kettle` by default and uses a temporary Kettle config with
  the grid renderer, automatic GPU policy, no restore, and no update check.

Run the independent Linux smoke gate with:

```sh
just linux-perf
# or:
scripts/perf/linux-compare.sh --runs 7 \
  --out-dir target/perf-results/linux-release-candidate
```

It writes Hyperfine startup, ASCII-flood, and SGR/underline-flood JSON,
advisory RSS and Kettle-live JSON, and `linux-score.json`. Kettle must beat
Terminator and remain within 10% of Ghostty on each cross-terminal timing
probe. The Kettle-live probe fails if resize geometry does not settle or
scrolling does not move the viewport, but its timings remain Kettle-only
advisory evidence until equivalent peer automation exists.

## Comparator isolation

Release schema 4 binds both candidates to the campaign named by
`release-contract.ps1`. The tracked campaign file records each official
release URL, asset bytes/hash, expanded file count/bytes/tree hash, terminal
version and role, executable bytes/hash, and Authenticode status/certificate.
Setup publishes a campaign only by an atomic append-only directory move.
During a run, Kettle opens every file in each confirmed peer's staged tree and
keeps those handles readable; the scorer requires the same complete campaign
projection and terminal identities in current and baseline evidence. Windows
Terminal remains an installed Appx because its supported launch/configuration
boundary differs from the portable peers; its exact package family, full name,
version, architecture, store signature, install location, executable, and
campaign hash are rechecked at both ends of acquisition. Release acquisition
executes the exact installed `WindowsTerminal.exe` Appx host directly and keeps
that hash-validated file under a read lease for the entire run. It never
launches Windows Terminal through `PATH`, `KETTLE_PERF_WT_EXE`, or the mutable
App Execution Alias. Standalone/smoke probes may still use ambient `wt.exe`
discovery, and their manifest labels that launch mode as advisory.

The generated benchmark profile gives Kettle, Alacritty, WezTerm, Rio, and
Tabby the same Cascadia Mono 13 pt font, opaque background, fixed palette,
10,000-line scrollback, zero padding, steady block cursor, and disabled
ligatures, blur, animation, bells, update checks, telemetry, restore, and
recording where each terminal exposes the setting. Config files are written as
BOM-free UTF-8 with LF endings, hashed into the manifest, passed through each
terminal's supported per-launch config mechanism, and held read-locked for the
live suite.

The installed Windows Terminal does not expose a per-launch settings-file
switch. Its configuration and hash are recorded, but its results are
`advisory-user-config` or `advisory-built-in-default`. Windows Terminal remains
visible in descriptive tables; it is excluded from confirmed release wins,
losses, and claims. The confirmed comparator set is Alacritty, WezTerm, Rio,
and Tabby.

Every command workload uses the same deterministic `cmd.exe /d /q /k` child.
Tabby's supported `run` command opens a confirmation dialog, so the harness
accepts only a new native dialog owned by the launched Tabby process. A
byte-verified one-use wrapper stays read-locked until that process exits.

## Display and capture contract

A release run needs a visible interactive Windows desktop. The selected target
screen must map to exactly one active EDID-backed physical monitor and must fit
the requested 1280×800 physical-pixel client. Because monitor-transition
evidence is mandatory, the complete release suite also needs two eligible
EDID-backed physical screens that can each fit that client.

Display identity, bounds, working area, DPI, refresh rate, EDID identity,
connection type, and primary mapping are captured before and after the suite.
The dedicated transition probe deliberately moves Kettle between the two
pinned screens. Outside that probe, changing, disconnecting, or switching
monitors invalidates all release evidence. A virtual/default 1024×768 fallback
display is suitable only for manifest/synthetic smoke checks.

The versioned identity resolver accepts a WMI monitor only with exactly one
same-instance physical `WmiMonitorConnectionParams` record. Miracast (15),
indirect wired (16), indirect virtual (17), undefined, and unknown output
technologies are not physical release evidence. If the same-instance WMI
connection is absent, the resolver may instead accept exactly one active
physical CCD path for that desktop source; monitor and connection then both
come from CCD rather than forming a mixed-source identity. The CCD monitor
device-interface name must have the strict expected shape and exact
`GUID_DEVINTERFACE_MONITOR` class; the harness derives one
`HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\...\Device Parameters` key from it
and validates the binary EDID's header, declared blocks, checksums,
manufacturer, and product against CCD. It never scans registry instances for a
matching model. The scorer reconstructs the one-monitor/one-connection mapping
from serialized evidence and re-applies the same physical allowlist. Missing,
duplicate, malformed, synthetic, or inconsistent evidence leaves the screen
unidentified and fails closed in release mode.

Schema-4 scoring also enforces the JSON token type at these trust boundaries.
Flags must be literal JSON booleans, and Windows output-technology values must
be literal JSON integers. PowerShell-coercible substitutes such as `"true"`,
`1`, `0`, or `10.0` fail even when their apparent value matches.

The fixed Kettle hover leg uses the common 1280×800 client. A second
`native-display` leg sizes Kettle to a large client derived from the
selected monitor's working area. Both run 200 samples in 20-sample blocks and
capture only the context-menu region. Startup readiness similarly captures only
the top-left client ROI, capped at 1024×384. Region capture avoids repeatedly
allocating and transferring a full 4K/5K frame while retaining the pixels
needed for the asserted state. Each hover leg requires zero misses, p95 at most
33 ms, p99 at most 50 ms, and at most one sample over 100 ms.

The hover and typed-input probes use real foreground input and refuse to inject
it into the wrong window. Their PrintWindow polling includes input dispatch,
redraw scheduling, GPU submission, composition, and capture cost. It is useful
comparative and regression evidence, not photodiode-grade input-to-photon
measurement.

## Controlled startup readiness

Startup timing begins immediately before the terminal command is spawned. The
harness then:

1. creates and sizes a fresh terminal window on the target screen;
2. gives that exact window foreground focus and atomically publishes a unique
   GO marker;
3. has the common PowerShell 7 child paint and flush a nonce-derived truecolor
   rectangle;
4. requires the terminal to answer `CSI 5 n` with `CSI 0 n`;
5. requires the child to atomically publish the matching READY marker; and
6. stops only when the validated marker and painted ROI are both observable.

This boundary is process spawn through exact client placement, truecolor paint,
and parser round trip. Expensive descendant/process-tree attribution runs only
after that endpoint and is recorded separately, so CIM latency cannot inflate
`startup_ms`. The manifest retains the discovered, sized/focused, GO-published,
GO-to-ready, and post-endpoint-attribution milestones. It is not a
present-to-photon measure. Strict UTF-8,
bounded payloads, unpredictable per-launch scratch names, exact-path
publication, reparse-point rejection, and exact cleanup keep the protocol from
accepting stale or cross-run markers. The held child PID is excluded from
fresh-memory and idle-CPU attribution. Release scoring requires a unique
excluded-PID set containing that workload, unique and nonempty included PID
sets before and after the idle interval, identical before/after membership, and
no overlap between included and excluded PIDs.

## Scheduling and throughput

Startup (12 samples per terminal), idle CPU/fresh memory (6 samples held for 10
seconds each), latency (60 samples as six blocks of 10, with at most 3 censored
at the 800 ms timeout), and throughput (6 visits) use deterministic,
seeded Williams-balanced schedules. Complete cycles balance position and
predecessor effects instead of running every Kettle sample before every peer
sample. The schedule and seed hash are part of the manifest and baseline
compatibility contract. Throughput also rotates the ASCII/SGR/Unicode order
inside each visit.

The square is defined for an even number of treatments and needs at least six
to be worth running, which is a hard floor on a machine that cannot offer six
comparators. `-AllowUnbalanced` (smoke only; `-Mode release` refuses it) drops
to `rotation-position-only-v1`: the order rotates by one slot each round, so
across a complete set every terminal starts in every position exactly once.
That controls position and warm-up order and nothing else — each terminal
follows the same neighbour every round, so a carry-over effect cannot be
separated from a terminal difference. The generator name and the sentence
`position-balanced; predecessors NOT balanced (smoke only)` travel with the
results, so a reader is never left to infer which schedule produced them. Pass
the terminal list as a real array (`-Terminals @('kettle','alacritty')` under
`pwsh -Command`); `pwsh -File` does not parse `a,b,c` and hands it over as one
name, which the parameter's `ValidateSet` now rejects outright.

A launch is refused outright when another instance of that terminal is already
running, because a terminal that joins a running instance opens no new window
and the launch would otherwise fail as an unexplained timeout.
`-AllowForeignTerminalInstances` (smoke only; `-Mode release` refuses it) lifts
that for terminals whose pinned launch arguments force a NEW process — read off
those arguments, so a spec that stops forcing one stops being tolerated.
Attribution is unaffected either way: the new window is found by diffing the
window set and skipping pre-existing PIDs, its owner's SHA-256 must equal the
launched executable's, the benchmark command must appear as a descendant of that
PID, and CPU/memory walk that tree. What a foreign instance *does* affect is
contention, so the manifest records `foreign_terminal_instances` (terminal names
and PIDs, no titles or command lines) whether or not the switch is set, and
those samples must not be read as quiet-machine numbers.

Throughput is not writer-acceptance time. The workload cannot begin until the
parent has attributed, sized, focused, and verified the exact terminal window,
then publishes a read-only locked GO capability whose unpredictable token the
child must acknowledge. Each payload starts at the first console write and
ends only after the terminal responds to `CSI 5 n` with `CSI 0 n`. This
end-to-drain boundary includes ConPTY backpressure and terminal parsing; the
separate writer timing remains diagnostic. Every terminal must complete every
payload in every paired round with strict UTF-8, the pinned byte
count/SHA-256, the exact client pixels, and the recorded console-cell
diagnostic. Post-flood memory covers the terminal process tree while excluding
the workload child. The child returns its single result through a random,
nonce-authenticated, current-user-only named pipe with bounded framing,
strict UTF-8, a finite timeout, and client-process ancestry validation; no
predictable result file is used.

Vtebench keeps its transient `.dat` file on the WSL filesystem. Its live
benchmark output remains attached to the terminal, while a private binary
stderr frame carries at most 1 MiB of DAT bytes to the locked PowerShell
wrapper. The wrapper forwards one nonce-authenticated named-pipe message; the
parent verifies the exact wrapper PID and terminal ancestry, parses the DAT
before acknowledgement, and publishes the compatibility `.dat` with a
create-new operation relative to a retained ordinary-directory handle. The WSL
workload uses a nonce-derived Linux process marker in a dedicated `setsid
--fork --wait` process group. Wrapper or parent timeout cleanup sends TERM and
then KILL only to that exact group, without terminating the entire WSL
distribution.

The Windows WSL engine is never resolved through `PATH`. By default the harness
prefers the installed `C:\Program Files\WSL\wsl.exe` engine and falls back to
the explicit System32 launcher only when that file is absent. The selected
ordinary file remains read-locked for the run. Its absolute path, SHA-256,
file/product version, normalized `wsl.exe --version` output and hash, runtime
version, and resolution policy are bound into both the manifest toolchain and
the vtebench summary. The launcher also resolves one registered distribution,
passes its exact name with `-d` on every invocation, and records its
distribution GUID/default identity, `/etc/os-release` hash and version lines,
kernel release, architecture, and numeric user ID.

The WSL source contract binds the clean benchmark tree, `Cargo.lock`, built
binary, Rustup, the actual Cargo executable resolved by Rustup, GNU `timeout`,
util-linux `setsid`, and util-linux `script` by canonical path, SHA-256, and
version. Generator and preflight checks run inside a fixed 120×40 pseudo-TTY so
TTY-dependent workloads are validated without inheriting the harness input
stream. Setup, each generator, Cargo fetch, Cargo build, preflight, source
validation, terminal workload, and cleanup have recorded finite deadlines.
The complete source-state signature is re-derived before and after every
terminal leg. Baseline scoring requires the launcher, distribution, deadlines,
and source/tool identities to match exactly.

## Statistical release gate

Release decisions are derived from raw matched observations, not aggregate
rank alone. The isolated peer comparisons use a deterministic 10,000-resample,
90% paired-cluster bootstrap over round/block medians with practical margins:

| Primary metric (lower is better) | Practical margin |
| --- | ---: |
| startup | max(25 ms, 5%) |
| idle CPU | max(0.10 percentage points, 20%) |
| fresh working set | max(8 MiB, 5%) |
| typed-echo latency | max(5 ms, 10%) |

For one peer, Kettle is a confirmed primary win only when at least three of the
four metric intervals are confirmed wins and at most one is a confirmed loss.
Across Alacritty, WezTerm, Rio, and Tabby, release requires at least three
confirmed peer wins and at most one confirmed loss. An interval spanning zero
is uncertain and never establishes a metric or peer win. The authoritative
3-of-4 rule still confirms a peer with three metric wins and one uncertain
metric, and the peer-count rule still passes with three confirmed peer wins and
one uncertain peer. Baseline non-inferiority is an all-metrics rule, so any
uncertain baseline metric fails closed.

Throughput reduces each round's three payloads to a geometric-mean composite.
It uses the same paired bootstrap with a 5% relative margin and additionally
requires every one of the six matched round composites to remain strictly
positive after that margin.

Every data set must pass drift diagnostics: absolute Theil-Sen fitted
first-to-last drift is at most 10%, and normalized peak-to-peak spread is at
most 20%. A mandatory compatible before/after baseline uses matched Kettle clusters and
the same 10,000-resample, 90% paired bootstrap. Every required metric must be
non-inferior after its practical margin; drift failure, missing data, or an
uncertain interval fails the baseline gate.

The fixed and native-display menu-hover files, vtebench evidence, stable
monitor-transition evidence (exactly 10 moves with the menu closed and 10
open), exact sample counts, executable/config hashes, clean source/build
identity, and unchanged start/end display topology are also mandatory in
release mode. With more than two eligible displays, the transition pair is
selected deterministically by the most differing DPI/refresh/geometry
dimensions, then largest DPI, refresh, and screen/working-area size deltas,
with an ordinal device-pair tie-break. The artifact records every candidate;
the scorer reconstructs the eligible set and ranking from raw topology. The
combined, menu-closed, and menu-open recovery summaries must each keep p95 at
or below 1000 ms and maximum at or below 2000 ms. Against the mandatory
same-machine baseline, all six p95/max summaries must remain within
`max(100 ms, 25% of baseline)`; this deterministic guard is additional to the
raw-evidence contract and fails closed.

## Private and public evidence

The raw directory under `target/perf-results/<label>/` is the authoritative
audit record. A run creates `target`, `perf-results`, and the label directory
through retained ordinary-directory handles as needed; the label itself must
not already exist, even if it is empty. It may contain local paths, commands,
monitor identifiers, and raw helper artifacts and should not be published
directly. Before scoring, the scorer opens a bounded no-follow snapshot,
rejects byte/depth/node-limit excesses,
invalid or BOM-prefixed UTF-8, duplicate or case-equivalent JSON keys, reparse
leaves, and mutations, and holds the input identities locked through atomic
score publication. Release manifests also carry the deterministic aggregate
hash and per-file hashes of the production harness; current and baseline must
use identical harness bytes.

Create a separate shareable bundle with:

```pwsh
pwsh -NoLogo -NoProfile -File scripts/perf/sanitize-results.ps1 `
  -ResultsDir target/perf-results/release-candidate `
  -OutputDir target/perf-public-release-candidate
```

The sanitizer refuses an existing destination, copies a bounded flat set of
JSON files only, replaces local paths, commands, serials, device identifiers,
hardware IDs, EDID fingerprints, adapter LUIDs, source/target IDs, and connector
instances with type-separated bundle-secret HMAC tokens. Credential-like keys
are normalized across snake, kebab, camel, Pascal, and upper-case spellings
before nested scalar, object, or array values are tokenized. The public bundle
accepts only the reviewed harness filenames (`benchmark-manifest.json`, the
fixed probe/score names, and the six fixed `throughput-*.json` names); custom
JSON evidence must first receive an explicit contract and sanitizer review.
This strict allowlist prevents a secret or user identity in a source filename
from becoming public metadata.

The sanitizer retains safe metrics and non-device hashes and writes schema-2
`public-evidence.json`. Numeric and boolean identifiers are tokenized before
scalar passthrough, so routing data cannot bypass string redaction. It retains
exact directory and child handles, rejects reparse points and alternate path
syntaxes, checks the exact flat set, alternate data streams, and file hashes
again after the handle-relative stage rename, and rolls the same object back on
any publication failure. It never recursively cleans an untrusted path and
never copies raw `.dat`, logs, screenshots, or artifact directories.

## Validation

The harness helpers have GUI-free fixtures that must pass in both PowerShell 7
and Windows PowerShell 5.1:

```pwsh
pwsh.exe -NoLogo -NoProfile -File scripts/perf/self-test.ps1
powershell.exe -NoLogo -NoProfile -File scripts/perf/self-test.ps1
```

The macOS comparator's embedded scorer has a separate portable regression gate:

```sh
just macos-compare-score-self-test
```

It rejects Kettle-only measurements as ineligible evidence and runs on the
required macOS CI leg without launching terminal applications.

The Windows PowerShell 5.1 entry point reconstructs `PSModulePath` from that
engine's native machine roots and imports its engine-owned Utility manifest.
This keeps the documented command deterministic even when it is launched from
PowerShell 7 and would otherwise inherit incompatible PowerShell 7 modules.

Run the repository gates before release:

```pwsh
cargo fmt --all --check
just gauntlet
just gauntlet-strict
```

`gauntlet-strict` also requires locally installed `cargo-deny` and
`cargo-machete`. These commands and the self-tests validate code and synthetic
fixtures, including exact-path CCD/EDID identity fallback and hostile identity
inputs. They do not prove native GPU behavior, live registry acquisition,
physical-monitor transitions, or live input/capture timing. Record those as
passed only after the full release suite actually runs on the stated hardware.

## Caveats

- All measured Windows output traverses ConPTY, so end-to-drain throughput is a
  full pipeline measurement rather than a renderer microbenchmark.
- Windows Terminal's shared process can make per-window memory/CPU
  unattributable; the harness records unavailable values rather than charging
  unrelated tabs to the benchmark.
- Capture polling is bounded by PrintWindow and display/compositor behavior.
  Per-monitor-v2 DPI awareness keeps Kettle geometry, pointer targets, and
  capture pixels in one physical coordinate space, but it does not turn
  software capture into an optical measurement.
- GDI screen capture cannot observe flip-model swapchains directly; the probes
  use client-only PrintWindow with `PW_RENDERFULLCONTENT`.
- Laptop thermal and power state still matter. Use one quiet, plugged-in
  session, keep the benchmark foreground, and do not type or move the mouse.
- The Linux live resize/scroll medians include `kettle ctl` round-trip time and
  remain Kettle-only advisory evidence.
