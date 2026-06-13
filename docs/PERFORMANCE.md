# Performance

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
scrollback sooner — the byte-budget-scrollback + atlas-bound follow-ups below
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
   Chrome labels (titlebar/tab/status) gained the same equality gates.
3. **SIMD extractor (P3)** — the image-protocol front stage walked the
   stream byte-by-byte; it now `memchr`-scans to the next ESC/ST/BEL and
   bulk-copies plain runs. `cargo bench -p kettle-vt` pins it (the first
   criterion benches in the repo).
4. **Wakeup dedup (P4)** — floods enqueued one event-loop wakeup per
   64 KiB read; an atomic latch now allows one per paint window.
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
the embedded font set + 500 themes, untouched this cycle and now tracked
with a number against it.

### Input latency

`scripts/perf/latency.ps1` — SendInput a key, poll
`PrintWindow(PW_RENDERFULLCONTENT)` until the client pixels change beyond
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
cargo build --release
pwsh -File scripts/perf/throughput.ps1       # all four terminals
pwsh -File scripts/perf/startup-idle.ps1
pwsh -File scripts/perf/latency.ps1
pwsh -File scripts/perf/vtebench-wsl.ps1     # vtebench inside WSL panes
```

Caveats pinned in `scripts/perf/README.md`: GDI captures cannot see
flip-model swapchains (hence PrintWindow), `wt.exe` may route into an
existing WindowsTerminal process (the harness never kills pre-existing
pids), thermals on a Surface-class device make medians-of-5 the floor for
honest numbers.

### Current performance gate

New performance work should publish a same-machine `perf-all.ps1` result and
run `scripts/perf/score.ps1` on it. The score gate normalizes throughput,
startup, idle CPU, and memory against the best terminal in the run; it fails if
kettle is outside the top half, beats fewer than two peer terminals, or regresses
more than the configured threshold when a baseline directory is supplied.

### v2.21.0 — startup 2.2× faster, damage-aware idle, corrected root causes

v2.21.0 corrected two root-cause attributions this doc previously got wrong, by
*measuring* instead of guessing:

- **Startup was discrete-GPU wake, not "500 themes."** The 500 bundled themes
  cost zero startup time (they are parsed lazily — only the active theme parses
  at boot). The real ~1.5 s cost was `Renderer::new` requesting the wgpu adapter
  with `PowerPreference::HighPerformance`, which on this dual-GPU laptop wakes
  the **discrete NVIDIA** from its low-power state. Defaulting to the low-power
  (integrated) adapter cut **spawn → first-visible-window from 2202 ms to
  ~999 ms (median of 5)**. The new `gpu-power-preference` key (`low` default |
  `high` | `auto`) lets a discrete-only/desktop user opt back in. Trade-off: the
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
- Byte-budget scrollback (`scrollback-bytes`, Ghostty model) to bound
  worst-case memory deterministically (current cap is line-count only).
- Font faces are `include_bytes!`-embedded then `.to_vec()`-copied into the font
  system (resident twice); borrow the `&'static` bytes to reclaim ~10–15 MB.

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
- Windows: `scripts/bench.ps1` (cycle 730: `System.Diagnostics.Process`
  based; uses `PeakWorkingSet64` for peak memory, captured at exit).

Both scripts build a release binary if one isn't present, then run
each measurement five times and print the wall-clock + peak-memory
for each invocation.

## Numbers

### Linux baseline (v1.3.8 + cycle 277, commit `1026858`)

> Captured at v1.3.8 (the Linux box wasn't available for a re-bench at later
> cuts). There's been no major architectural change to the render/startup paths
> since, so these should still be in the same ballpark on the current release
> (v2.18.x) — but treat them as "what we measured then" and run
> `scripts/bench.sh` for a fresh data point on your own machine.

| Measurement | Value | Notes |
|---|---:|---|
| Release binary size | 24.7 MB | Includes embedded JetBrains Mono Nerd Font + ~500 themes. Cycle 277 trimmed ~6 MB by narrowing `image` features to PNG/JPEG/GIF (was pulling AVIF/`rav1e` + EXR + WebP + HDR + TIFF + …) and disabling `arboard`'s image-clipboard default feature |
| `kettle --version` startup | < 10 ms wall, 5.0 MB peak RSS | Cold (no warm pages); 5 runs all rounded to 0.00 s |
| `kettle --screenshot OUT.png` | ≈ 250–270 ms wall, 236 MB peak RSS | Includes wgpu adapter init, offscreen Vulkan device, font system load, full GPU text + quad pipelines |
| `kettle --screenshot-menu OUT.png` | ≈ 240–250 ms wall, 236 MB peak RSS | Same as above + the second TextRenderer / menu_quads pass; identical memory footprint, ~10 ms faster on the GPU pipeline warmup pattern |

### Windows 11 reference (captured at v1.46.0 + cycle 730)

> Captured on a Surface Book 3 (Intel Iris Plus Graphics, x64,
> Windows 11 build 26200) the day the v1.46.0 release was cut (a fixed data
> point — the current release is v2.18.x; re-run `scripts/bench.ps1` for fresh
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
| Release binary size | 21.3 MB (22,370,304 bytes) | `kettle.exe` MSVC release build with embedded Win11 .ico via `winresource`. Slightly smaller than the Linux x86_64 binary (24.7 MB) — likely because MSVC's `panic=abort` codegen + LTO eliminates more unwind tables than gnu-stable did at cycle 277 |
| `kettle --version` startup | ≈ 95-110 ms wall, 4-9 MB peak working set | Cold process spawn floor. Higher than Linux's <10 ms because Windows CreateProcess pays Defender real-time scan + image-load overhead. After warm-cache (Defender has hashed the .exe), drops to ~50-70 ms |
| `kettle --screenshot OUT.png` | ≈ 2.1-3.0 s wall, 377-389 MB peak working set | wgpu Vulkan adapter init + offscreen device + font system load + first font-atlas glyph upload. The first run is the slowest (~3 s — Defender cold-scan); runs 2-5 settle to 2.1-2.2 s |
| `kettle --screenshot-menu OUT.png` | ≈ 2.0-2.1 s wall, 381-389 MB peak working set | Same as above + the cycle-251 menu render pass. Peak WS higher than Linux software-Vulkan (236 MB) because Windows DX12/Vulkan adapter via wgpu keeps more state resident in the process's WS than Mesa software-Vulkan does on Linux. On a real-GPU Linux box with a hardware Vulkan driver, the comparable Windows-vs-Linux number is expected to be much closer |

## What the numbers mean

- **Startup is fast.** `--version` is a single `clap::Parser::parse`
  + a `println!`; under 10 ms on Linux. Windows adds process-spawn
  + Defender real-time scan overhead the first time the .exe is
  invoked from a directory (the cycle-730 install advice "add the
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
  paints are capped at one per ~16 ms frame budget so multi-read
  bursts (build logs, streaming output) settle into single frames.
  Keystroke echo used to ride the same `WaitUntil` deadline, and
  Windows' ~16 ms timer granularity made held-key repeat visibly
  stutter; echo output now requests a redraw immediately
  (`request_redraw` is vsync-coalesced, so it can't outpace the
  display) while non-input bursts still coalesce to one paint per
  frame budget.

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

`scripts/bench.ps1` (cycle 730) needs PowerShell 5.1+ (preinstalled on
Windows 10+) or PowerShell Core 7+. No external dependencies — uses
the .NET `System.Diagnostics.Process` API directly.

On macOS / Windows expect different numbers from the Linux baseline:
startup is generally faster on macOS arm64, the headless GPU path
uses Metal / DX12 instead of software-Vulkan, and the binary size
differs because the universal2 macOS build is fatter.

## Not measured here

These would be valuable but need either a live display (FPS) or
extended runs (steady-state memory, scrollback ingestion). Open
follow-ups:

- Live-window FPS under text-heavy / image-heavy load.
- Steady-state memory after the GPU pipeline + font atlas warm up.
- Scrollback ingestion throughput (`yes | head -10M | kettle`).
- Time to first frame from `kettle -e bash -ic ls` (perceived
  start-to-prompt latency).

Comparative perf vs alacritty / kitty / WezTerm is *not* the goal of
this doc — those projects publish their own numbers and any honest
side-by-side needs a single methodology applied to all of them
(consistent fonts, themes, scrollback, hardware). Worth a future
[`docs/UX-COMPARISON.md`](UX-COMPARISON.md)-style comparison once
that methodology is pinned down.

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
- **`PeakWorkingSet64` for working set** (Windows, cycle 730): the
  .NET `Process.PeakWorkingSet64` property is populated by Win32
  `PSAPI.GetProcessMemoryInfo` and is comparable to Linux's max
  RSS — peak resident pages in physical memory for the lifetime
  of the process.
