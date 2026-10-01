# Standing method and freeze protocol

Keep startup, idle, flood-memory and vtebench as the default workloads.
Optional methods are block latency with typing memory, cursor latency, paced
printing and the launch blink window. A plain invocation posts no keys. No
recipe or CI step starts the shared A/A automatically.

## Workloads and identities

Startup reuses its content-addressed payload and excludes one warmup launch.
Child time is payload start, not shell-prompt appearance. Window appearance
is separate from displayed output. First-output capability remains unavailable;
early reveal needs a separately validated displayed-output method before a
ranking claim. Strict child/native grid checks are separate from peer standings.

Idle keeps 20 seconds of settling and a 30-second counter interval, with three
focus checks and endpoint current footprint. Flood keeps the pinned 32 MiB
payload and 100 ms timeline; done+3 and done+20 select the first sample at or
after the deadline. Current footprint and process lifetime maximum differ.
Vtebench uses per-round sample means in milliseconds and the benchmark geomean.

Block latency keeps its hidden steady block, six calibration flips, 20 warmup
keys, 100 measured keys, HID posting, seeded 100..300 ms gaps and 500 ms censor.
Censored keys count at that bound. Typing memory is the median current
footprint of 100 ms queries wholly inside the measured key epoch. Calibration,
warmup and shutdown samples are excluded. At least five samples, 80 percent
coverage and no gap over 250 ms are required. A memory failure does not discard
valid response latency. Floors have no terminal typing-memory row; opaque is
unranked. Ten valid 100-key launches are needed for an n=1000 statement.

Cursor latency enables a visible blinking cursor after hidden calibration,
then waits 2000 ms before its first key and 2000..2400 ms between keys. Its
Kettle-only rotation and method identity are independent of block latency.
It collects no typing-memory row. Complete `cursor_exit_v1` records join launch,
pane, window, sequence and clocks. Every key sequence appears exactly once:
1..6 as calibration inputs, then warmup and measured keys as exits. Retained
`cursor_exit_records` contain exits only. Warmup exits stay raw; measured total-frame
values are pooled before p50/p95/max. A capable partial stream fails; a legacy
empty stream is unavailable. External cursor-response A/A does not calibrate
producer logging or prove the <=4000 us exit-frame predicate.

Printing emits 80 numbered lines on absolute 100 ms deadlines over eight
seconds. Its current footprint uses the designated first whole query at or
after six seconds, before completion, within 250 ms lateness. The same query's
lifetime maximum is descriptive. Focus before/after the query, focus changes,
window identity and all payload writes must be valid. No search for a later
lower sample is allowed.

Blink observes launch+2.5 through launch+8.5 seconds, with readiness before the
first boundary. CPU and wakeups use cumulative deltas divided by actual span.
Current endpoint memory and descriptive interval peak/median stay distinct.
Separate before/after validation supports the unchanged setup, not continuous
phase coverage in every measured interval. Disabled-default and unproven
activity cannot rank as active blink.

Record effective config closures and captured asset digests per side and peer.
Private paths, raw configs/logs and signing details stay in the local manifest.
Countable configs are restricted to the supported declarative resolver.
Revalidate captured inputs and tool artifacts outside measured intervals. The
verified receipt binds the latency bundle that runs, not merely its source.
A damaged cache requires explicit preparation before grants or measurement.
Never rebuild or re-sign during a campaign.

## Freeze and owner measurement window

Merge the prerequisites and all harness changes before any measured D1/D2 set
opens. Do not merge runtime harness changes during an open set. Keep the
on-disk path/mode/content hash with the explicit inert exclusions. Complete
portable and native CI, required repository gates and an independent read-only
review of the fixed candidate. Agree versioned native producer formats.

Prepare and verify the dedicated probe once, record its source and bundle,
then obtain owner grants for that artifact. Run excluded functional, focus,
timestamp/floor and observer-cost pilots. Predeclare observer equivalence:
block latency interval within +/-1 ms, printing memory within +/-0.5 MiB,
blink CPU within +/-.01 percentage point and wakeups within +/-.1 per second.
An interval merely containing zero is insufficient. Failed equivalence blocks
freeze until the method is repaired and pilots repeated.

The owner starts from the declared host terminal with field terminals quit,
AC power, no Low Power Mode, unlocked display, load below 2, no backup, builds
or review activity, and hands off. Preserve environment and identity checks at
workload boundaries. Known drift stops the affected measurement. These checks
do not prove unchanged statistical variance across dates.

After freeze, make two verified copies of the same notarized 4.8.0 app at equal
path depth. Run this ONE ordinary control invocation in the owner's scheduled
window. Explicit local bundle paths replace A.app and B.app.

```sh
python3 scripts/perf/macos-standing.py \
  --no-build --kettle A.app/Contents/MacOS/kettle \
  --kettle-b B.app/Contents/MacOS/kettle \
  --workloads startup,idle,flood-memory,vtebench,latency,latency-cursor,output-memory,blink-window \
  --startup-rounds 30 --idle-rounds 8 --flood-rounds 6 \
  --vtebench-rounds 10 --latency-rounds 10 --cursor-rounds 10 \
  --output-memory-rounds 10 --blink-rounds 10 \
  --warmup 1 --idle-settle 20 --idle-window 30 \
  --flood-offsets 3,20 --vtebench-seconds 10 --fd-limit 256 \
  --latency-keys 100 --latency-warmup 20 --memory-sample-ms 100 \
  --blink-validation BLINK-BEFORE.json \
  --label aa-480-hc --out-dir CONTROL
```

BLINK-BEFORE.json must be the linked pre-set validation for both control
entries. Retain and inspect the post-set validation before using the control.
An absent or mismatched validation leaves active blink unproven.

Do not use `--rounds`; it overrides the distinct counts. Floors and opaque do
not join this two-entry control. Retain their artifact identities and prior
floor pilot evidence. An optional higher display refresh needs its own control
and must never be pooled with 60 Hz.

The control requires all declared paired rows, matching methods and identities.
Scalar ratios must contain 1; benchmark family intervals govern the benchmark
A/A pass. Their ordinary 95 percent half-width h still sets max(.03,2h).
Block and cursor launch-mean differences must contain zero and have absolute
mean shift <=1 ms. Improvement is at least max(1 ms,2*abs(control delta));
external no-regression stays +1 ms. Product limits are additional constraints.
Unbounded/all-zero controls remain uncalibrated. One whole-control rerun is
allowed, with the failed run retained. A second failure stops gating and
publication. No per-metric favorable selection is allowed.

## Stamp observer control

In the same scheduled campaign, run 30 startup pairs of the same verified
binary with stamps off on A and on for B. Use the binary that actually supplies
the startup instrumentation. This is a diagnostic leg, separate from CONTROL.

```sh
python3 scripts/perf/macos-standing.py \
  --no-build --kettle STAMP-A.app/Contents/MacOS/kettle \
  --kettle-b STAMP-B.app/Contents/MacOS/kettle \
  --workloads startup --startup-rounds 30 --warmup 1 \
  --startup-phases b --fd-limit 256 --label stamp-observer \
  --out-dir STAMP-CONTROL
python3 scripts/perf/macos-standing.py \
  --observer-control STAMP-CONTROL --out-dir STAMP-ANALYSIS
```

The collector labels this `kind=observer-control`. It is never countable as a
standing or ordinary A/A. Analysis launches nothing, requires complete
on/off stamp evidence and both child/window difference intervals inside
[-1,+1] ms and ratio intervals inside [.97,1.03]. Failure blocks phase
attribution. Later producer logging needs its own perturbation evidence;
freezing a parser does not calibrate logging that does not yet exist.

## Reports, publication and owner choices

The PR #409 estimators and intervals are the only statistical policy.

Schema 3 results use `evidence_contract=hc-v1`; metadata includes workload
contracts, actual tool artifacts, config closures, capabilities and metric
countability. Older schema-1/2 whole-file outputs remain compatible. Missing
new fields are unavailable and cannot suppress old sections or become zero.

Combined rows retain descriptors, units, comparisons, differences, current
statistics, source sessions and local failures. Publication cells
in `publication-values.json` have unique IDs, units, source metric/sessions,
current estimator, unrounded value, display, claim/rank, n, actual keys and
caveat status. Values use the first three standing dates or two A/B dates.
A/B headlines use the session nearer no change; days are never pooled into a
new experiment. Diagnostic distributions may use the matching launch-mean
control but never acquire their own no-regression verdict. Native exit cells
remain diagnostic until producer perturbation is independently validated.

Before measurement, record these owner choices in the ledger:

- Preserve the existing timestamp rule until the native pilot resolves the
  older conflicting arrival/display inequality.
- Use the shared frozen method control with environmental drift monitoring.
  A changed method or machine requires a new complete control.
- Reject all-zero ratio calibration unless an absolute rule with units is
  declared before capture. No unlimited regression gate.
- Agree producer wire layouts, complete-frame exit endpoints and S2 wait
  interpretation. Recommend every required font wait <=3 ms with complete
  thread/path evidence; native absence cannot pass a gate.
- Use separate blink validation with its bounded claim. Simultaneous capture
  would change observer cost and require calibration before freeze.
- Keep early reveal deferred. Use the generic latency improvement suite for
  FLAT until an explicit separate target is supplied.
- Use the frozen verified ad-hoc probe and 60 Hz primary display; decide any
  certificate signing or high-refresh control before grants/measurement.
- Decide D1 latency inclusion before its first countable session. If included,
  typing memory is required. D2 includes the already frozen latency method.
- Treat M2 as explicit opt-in. Decide printing publication before capture;
  collection support does not approve mapped buffers or a numerical claim.

D1 uses the notarized 4.8.0 set and two 4.7-to-4.8 A/B dates; D2 follows the
4.9.0 tag and uses its separate released-app set and two 4.8-to-4.9 A/B dates.
Final numerical documents wait for passing control, complete data, factcheck,
anchor/provenance checks and owner acceptance of any conflicting prior claim.
