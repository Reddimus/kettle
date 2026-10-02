# Standing analysis schema

New `results.json` files use `schema: 3`, `evidence_contract: hc-v1` and
`meta.statistics_policy: current`. These identify the wire contract, not
completion of every planned collector. `analysis.json` uses schema 3 with
`statistics_policy: current` and a `workloads` object. `combined.json` uses
schema 3 when a session or control uses schema 3 or includes a new metric. It
preserves source session schemas and stores metric rows under
`rows["workload.field"]`.

## Read compatibility

Schema 1 has no trustworthy preflight metadata and remains noncountable.
vtebench means come from each round's `.dat` file, never from the stored
whole-millisecond medians. Missing `.dat` files remain round errors.
Schema 2 keeps its existing workload completeness and measurement semantics.
No migration writes back into a source session. Unknown schemas fail.
With only existing schema-1/2 data, all output files keep their original bytes,
including JSON key order and Markdown whitespace. Combined rows retain their
legacy shape without descriptors, statistics, countability metadata or added
differences. The appended report sections are absent. Typed internal analysis
does not opt an old session into the schema-3 output format.
Missing optional fields, capability declarations or validity evidence cannot
become zero or supported measurements. Finite zero is a value. Nonfinite
inputs and booleans are unavailable numerical inputs.

## Metric descriptors

Analysis entries and extended combined rows carry `descriptor`:

| Field | Meaning |
|---|---|
| `id` | Stable workload and field ID |
| `unit` | `ms`, `MiB`, `percentage points` or `/s` |
| `direction` | `lower` for measured costs, `none` for signed diagnostics |
| `analysis_kind` | Scalar, benchmark, latency, distribution or signed |
| `kind` | Compatibility alias for `analysis_kind` |
| `extract` | Field, benchmark means, censored keys or pooled keys |
| `eligibility` | Legacy workload rule, latency standing rule or metric validity |
| `estimate` | Current estimator and interval name |
| `comparison` | Current paired comparison name, or `none` |
| `aa_kind` | Ratio, latency difference or none |
| `claim_kind` | Ratio, latency or none |
| `publication_role` | Standing, descriptive or diagnostic |

Flood offsets and vtebench benchmark names have generated descriptors. Flood
offset fields accept every `:g` spelling, including scientific notation.
vtebench aggregates use the fixed session benchmark set. A missing, nonfinite
or nonpositive member makes the round's aggregate unavailable. Finite members
remain available in their own benchmark rows. Aggregate failure counts include
unavailable rounds, and incomplete benchmark rounds cannot count for claims.
The registry reserves optional first output, startup durations/slack, typing
memory, printing, blink and cursor latency. It emits rows only for fields
present in input data. Reserved descriptors never enable collection.
Startup phase cumulative timestamps retain their existing comparisons and
remain subject to the startup diagnostic session exclusion.

For an optional scalar, `row.metric_validity[field]` must contain:

```json
{
  "valid": true,
  "reason": "",
  "expected": 5,
  "observed": 5,
  "capability_version": "producer-v1"
}
```

Expected and observed coverage are finite counts. Expected must be positive;
observed must be at least expected. A producer with a threshold such as 80%
coverage expresses its required coverage as expected and must validate all
other method requirements before setting valid. Missing, invalid or incomplete
evidence removes that scalar only. A finite negative value is allowed only
for a signed diagnostic. Round positions remain aligned, including unavailable
and warmup positions. This generic contract does not implement the optional
sampler, timestamp join or producer capability checks from later PRs.

`metric_countable[terminal]` records `countable`, `n`, `planned`, `failed` and
`reasons`. It accompanies each analysis entry and each combined per-session
record. Completed runs copy it to `meta.metric_countable[id]`. Default
workloads retain session-wide completeness. Latency retains its entry-local
ranking exclusions. Optional scalars need all planned metric rounds for a
terminal, without costing another metric its countability. Descriptive
estimates can remain visible even when a metric does not count for a claim.

## Statistical reports

Each metric entry contains:

```text
statistics.authoritative = current
statistics.current.terminals[terminal] = estimate, low, high, n
statistics.current.ab or vs_best = existing ratio/comparison report
statistics.current.ab_diff or vs_best_diff = mean paired difference report
```

Distribution-only fields retain their pooled estimates and have no gain
comparison. Their old empty-comparison verdict behavior stays unchanged.

Current scalar estimates use the median/sign-test interval, except vtebench's
arithmetic mean/Student-t interval. Current scalar ratios use the geometric
mean of paired ratios with a log Student-t interval, retaining zero handling
and vtebench family correction. `ab_diff` and `vs_best_diff` hold `diff`,
`low`, `high`, `n` from the mean of paired differences with a Student-t
interval in the metric's own unit. Signed slack has these differences and
no ratio. Startup's existing absolute differences keep their values.

Current mean latency uses launch means and Student-t intervals. Its comparison
retains `ratio`, `low`, `high`, `diff`, `diff_low`, `diff_high`, `wins`, `n`.
Censored keys count at their configured bound. Distribution cells pool counted
keys or valid halves. Existing defaults and published estimates stay intact.

The PR #409 estimators are the only authoritative policy. Reports contain
`statistics.current` and `statistics.authoritative = current`, with no
supplemental estimator family or bootstrap report.

Ranked scalar and mean latency entries carry `pairwise` records with `base`,
`test`, `current`, and `adjacent` records with `base`, `test`, `order`. Pairs
cover every eligible ranked entry, excluding floors/opaque/unranked entries.
Scalar adjacent order requires the current paired log Student-t ratio interval
to lie entirely above one. vtebench A/B comparisons, including A/A controls,
retain their Bonferroni family. Standing peer comparisons retain the current
unadjusted intervals. Mean latency adjacent order requires the current paired
difference Student-t interval to lie entirely above zero. Pairs with intervals
that overlap no change, or without paired data, are `tied`. These labels do
not form transitive tie groups. Legacy numeric rank and best-other claims
remain unchanged. Adjacent Markdown labels follow the completed tables,
separated by a blank line, and appear only when new metrics are present.

## Combine and gates

Extended combined rows retain descriptors, per-session estimates, statistics and
metric countability. Scalar session comparisons also retain `difference`;
per-session records retain `ab_diff`/`vs_best_diff`. Pairwise/adjacent
reports survive combine. Published values remain the median of countable
session estimates with min/max, and headlines keep the nearer-no-change
session. No pooling across dates or replacement of earlier countable dates
occurs.

Gate dispatch uses `aa_kind`. MiB never goes through a millisecond latency
gate, including a memory field in the latency namespace. Signed diagnostics
have no calibrated ratio gate or standing claim. Finite zero samples preserve
the existing ratio gates and verdicts, including infinite derived intervals.
An unbounded derived interval does not remove an A/A gate. Equal-zero controls
keep their current tie semantics. Unsupported `statistics_policy`
values fail instead of silently choosing a different estimator.

This PR does not add publication fill helpers, probe receipts, asset closure,
new collectors or native instrumentation. Those belong to the remaining
harness-completion PRs.

## Cursor input and exit coverage

The optional `cursor_exit_v1` stderr stream starts with one capability. An
input record has exactly `event: "input"`, `launch_id`, `pane_id` and `key_seq`.
The producer emits one for each counted key that does not latch an exit and
for each cancelled exit ticket. Calibration keys 1..6 must be inputs.
Sequences 7..6+W+N must be exits with `layer_active: true`, in key order.
Every sequence from 1 through the highest seen must occur exactly once across
both record types. Input records can arrive before an earlier exit completes.
Any input at sequence 7 or later, sequence beyond 6+W+N, gap, duplicate,
or pane/launch mismatch invalidates the stream. Raw input records remain in
the stderr artifact; `cursor_exit_records` and statistics contain exits only.
A legacy stream without capability remains unavailable. A capable empty or
partial stream fails.

## Observer pilots

Diagnostic sessions use `meta.kind = "observer-pilot"`, `countable: false` and
`meta.observer_pilot = {kind, pairs, bounds}`. `kind` is typing, printing or
blink. Rows carry `observer_arm` on/off, zero-based `observer_pair`, and
`observer_order` 0/1 within that terminal's pair. Every attempt is retained.
Off printing/blink rows carry `coverage_waived: "observer-pilot off arm"`;
query lateness, focus and boundary checks still apply. Typing off memory is
unavailable with reason "observer off (pilot arm)".

Pilot rows alone carry `observer_cost`: observer `cpu_ns`, `wakeups`,
`query_count`, `query_duration_median_ms`, `query_duration_max_ms`,
`deadline_lateness_max_ms`, `target_cpu_delta_ns` and `target_wakeups_delta`.
Unavailable counters are null; typing off observer counters/count are zero.
The native observer writes a bounded `.self.json` sidecar only in pilot mode.

`observer-equivalence.json` reports per-terminal metric differences on minus
off with paired Student-t 90% intervals (`interval_policy` "TOST at 5% each
side: paired Student-t 90% inside the bounds"), valid-pair counts,
`allowed_invalid_pairs`, invalid-pair counts by public reason, descriptive
per-arm medians and self-cost/query summaries. Bounds are +/-1 ms launch mean for typing, +/-0.5 MiB
`printing_mib`, and both +/-0.01 percentage points `cpu_percent` and +/-0.1/s
`wakeups_per_second` for blink. `allowed_invalid_pairs` is 5% of the declared
pairs, rounded down. An invalid pair for any reason other than focus,
visibility or the probe's cover and foreign-input guards ("probe saw focus,
cover or foreign input") means "invalid pairs not caused by the desktop".
Focus evidence the observer could not read ("focus evidence unavailable"), a
hidden window or activation that names no other process or blames the target
("window hidden, desktop cause unproven"), a desktop-named reason on a row
without `attribution_contract` 1 ("desktop reason without attribution
evidence"), and a probe failure other than foreign input that the round's
own observer did not prove (an off arm has none), or one that ran off the
120x36 grid or did not end by the harness's stop (`shutdown` other than
"stopped"), are not the desktop's. Native focus checks carry `target_owner`,
`top_owner` and `cover_owners` (every window over the measured one),
activations carry `pid`; printing, blink and
typing-memory rows and failed latency rows carry `attribution_contract`, and
failed latency rows carry `target_pid` and `shutdown`. More desktop
failures than allowed means "insufficient valid pairs", and an unfinished
session means "pilot incomplete". Overall equivalence needs every terminal to
pass.
Ordinary analysis, combine and A/A cannot consume these sessions. The startup
stamp observer-control report retains its existing contract.
