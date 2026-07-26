# Performance

## Unreleased — context-menu interaction latency

Context-menu row hover used to take the full frame path: every pointer crossing
locked and copied all visible terminal grids, ran terminal maintenance, and
forced both glyphon text renderers to prepare because any open overlay marked
the whole text frame dirty. On a high-DPI 5120x2160 desktop that work sits
directly on the input-to-present critical path.

Menu-only redraws now validate and reuse the pooled pane snapshots by pane id,
output generation, grid dimensions, and order. The fast path performs no
terminal mutex acquisition or viewport copy; a cursor-blink bit captured in the
snapshot avoids a hidden overlay-builder lock. The renderer hashes menu text
and layout separately from its highlighted row, so hover changes only rebuild
and upload menu quads; the unchanged block-cursor glyph renderer also reuses
its retained vertices unless cursor/font/atlas damage requires a refresh.
Output, resize, reorder, scroll, enabled-state, anchor, or label changes fail
closed to the normal path. Cross-terminal frame-latency numbers belong to the
machine-local benchmark artifact and are not claimed by portable unit tests.

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

**Ubuntu local desktop smoke, current v2.25.1 main**
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
the embedded font set + 500 themes, untouched by this change and now tracked
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

New Windows performance work should publish a same-machine `perf-all.ps1` result
and run `scripts/perf/score.ps1` on it. The score gate normalizes throughput,
startup, idle CPU, and memory against the best terminal in the run; it fails if
kettle is outside the top half, beats fewer than two peer terminals, or regresses
more than the configured threshold when a baseline directory is supplied.

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
> since, so these should still be in the same ballpark on the current release
> (v2.18.x) — but treat them as "what we measured then" and run
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

`scripts/bench.ps1` needs PowerShell 5.1+ (preinstalled on
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
- **`PeakWorkingSet64` for working set** (Windows): the
  .NET `Process.PeakWorkingSet64` property is populated by Win32
  `PSAPI.GetProcessMemoryInfo` and is comparable to Linux's max
  RSS — peak resident pages in physical memory for the lifetime
  of the process.
