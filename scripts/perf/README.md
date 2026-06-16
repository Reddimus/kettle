# Cross-terminal performance harness

The pinned methodology behind the comparative numbers in `docs/PERFORMANCE.md`.
The Windows harness runs the SAME way against every terminal (kettle, Windows
Terminal, Alacritty, WezTerm), windows normalized to the same pixel size,
medians of repeated runs. The Linux desktop harness below covers the Ubuntu
Terminator/Ghostty smoke target with Hyperfine and explicit pass/fail rules.

| Script | Measures | How |
| --- | --- | --- |
| `linux-compare.sh` | Linux startup + ASCII flood | Hyperfine: launch terminal, run `/bin/true`, close; launch terminal, print ~4 MiB ASCII, close. Requires Terminator + Ghostty; includes Alacritty when installed; fails if kettle does not beat Terminator or is more than 10% slower than Ghostty |
| `gen-payloads.ps1` | — | Deterministic payloads: ~16 MB plain ASCII, ~6.1 MB SGR-heavy, ~4.3 MB CJK/emoji |
| `run-inside.ps1` | throughput | Runs INSIDE the terminal; times chunked console writes of each payload (termbench principle: the terminal's consumption backpressures the writer); writes JSON, no screen scraping |
| `throughput.ps1` | throughput + post-flood memory | Orchestrates run-inside per terminal; samples process-tree working set right after the flood |
| `startup-idle.ps1` | startup, fresh memory, idle CPU | Spawn→first-visible-window (median of 5); process-tree WS after settle; tree CPU-seconds over 60 s focused at a blinking prompt |
| `latency.ps1` | input latency (comparative) | SendInput a key, poll PrintWindow(PW_RENDERFULLCONTENT) until pixels change beyond an auto-calibrated blink-noise floor; capture-poll resolution ~5-15 ms, so treat results as relative |
| `vtebench-wsl.ps1` | PTY read speed (WSL) | alacritty/vtebench full suite inside each terminal's WSL session; gnuplot .dat + median summary |
| `perf-all.ps1` | everything | One label = one results directory under `target/perf-results/<label>/` |
| `score.ps1` | release gate | Scores `startup-idle.json` + `throughput-*.json`; fails unless kettle ranks in the top half, beats at least two peers, and stays within the allowed regression threshold vs an optional baseline |

## Prerequisites

- Linux desktop smoke: `hyperfine`, `terminator`, `ghostty`; optional
  `alacritty`; a graphical X11/Wayland session. `linux-compare.sh` builds
  `target/release/kettle` by default and uses a temporary config with
  `text-renderer = grid`, `gpu-power-preference = auto`, no restore, and no
  update check.
- Terminals: `wt` on PATH; Alacritty portable at `C:\Users\kevm9\Repos\research\bin\alacritty.exe`; WezTerm portable at `...\bin\wezterm\wezterm-gui.exe`; kettle from `target\release\kettle.exe` (build first). Paths are overridable parameters on every script.
- vtebench built in WSL: `CARGO_TARGET_DIR=$HOME/vtebench-target cargo build --release` from a clone of `alacritty/vtebench`.
- A quiet machine: close other GPU/CPU-heavy apps; plugged in, high-performance power profile; do not move the mouse during the latency probe.

## Score gate

For the Ubuntu Terminator/Ghostty target:

```sh
just linux-perf
# or:
scripts/perf/linux-compare.sh --runs 7 --out-dir target/perf-results/linux-v2.25.1
```

That writes `linux-startup.json`, `linux-ascii-flood.json`, and
`linux-score.json`. The gate is deliberately narrow and repeatable: Kettle must
beat Terminator and remain within 10% of Ghostty on both probes.

Run the full harness, then score the result directory:

```pwsh
pwsh -File scripts/perf/perf-all.ps1 -Label after
pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after
```

For same-machine before/after work, add a baseline directory:

```pwsh
pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after `
  -BaselineResultsDir target/perf-results/before -MaxRegressionPct 7.5
```

The score normalizes each terminal against the best observed result per metric:
higher is better for throughput; lower is better for startup, idle CPU, and
memory. The gate is intentionally comparative so a faster workstation does not
hide a regression that only appears relative to the peer terminals measured in
the same session.

## Caveats (read before quoting numbers)

- All Windows terminals consume output through ConPTY, so the throughput test
  measures the full real-world pipeline (ConPTY + parser + renderer
  backpressure), not the renderer alone.
- The latency probe is bounded by PrintWindow capture cost; numbers are
  comparative between terminals captured identically, not input-to-photon.
- GDI screen captures cannot see flip-model (DXGI) windows — that is why the
  probe uses PrintWindow with PW_RENDERFULLCONTENT.
- Run-to-run variance on laptop hardware is real: medians of N≥5, and prefer
  same-session comparisons (same thermal state) over absolute values.
