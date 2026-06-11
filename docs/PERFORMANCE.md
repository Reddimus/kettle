# Performance

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
  - The bundled JetBrains Mono Nerd Font set (~50 MB of glyph data
    via `kettle_config::font::all()`).
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
