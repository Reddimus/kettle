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
- `flood-memory`: memory after printing 32 MiB of seeded text that is the same
  on every run, in lines narrower than the grid
- `vtebench`: Alacritty's vtebench at a pinned revision

Every terminal runs with its default configuration on a 120x36 grid. Helpers
under `macos-standing/` are compiled into `target/perf-tools/` on first use,
which needs the Xcode command line tools, and vtebench is cloned and built
there too.

```sh
just macos-standing
just macos-standing --workloads idle --rounds 8
just macos-standing --kettle-b /path/to/other/kettle --workloads startup --fd-limit 1048576
```

`--kettle-b` compares two Kettle builds and reports each metric's median
paired ratio with a bootstrap 95% interval. Idle figures depend on focus,
because blinking cursors only run in a focused window, so each idle sample
records whether its terminal was frontmost.
`just macos-standing-self-test` checks the parsing and statistics without a
desktop.

## Shared probe

`kettle-live-probes.py` owns the bounded Kettle resize and scroll probes used by
the platform scripts. Those timings are useful regression evidence but are not
cross-terminal claims unless the peer tools measure the same boundary.

The former Windows PowerShell acquisition and scoring suite was removed when
Kettle stopped distributing Windows builds in 4.0.0. Historical Windows
measurements remain in `docs/PERFORMANCE.md` with their original scope.
