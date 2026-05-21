# Performance

Real measurements from kettle's release binary, captured on the
maintainer's CI-equivalent Linux box (Ubuntu, 8-core x86_64, software-
Vulkan via `mesa-vulkan-drivers` for the headless render path —
matches what `.github/workflows/ci.yml`'s `--screenshot` smoke runs on).

Reproducible: see `scripts/bench.sh`. The script builds a release
binary if one isn't present, then runs each measurement five times
and prints the wall-clock + peak-RSS for each invocation.

## Numbers

Captured against **v1.3.8 + cycle 277** (commit `1026858`):

| Measurement | Value | Notes |
|---|---:|---|
| Release binary size | 24.7 MB | Includes embedded JetBrains Mono Nerd Font + ~500 themes. Cycle 277 trimmed ~6 MB by narrowing `image` features to PNG/JPEG/GIF (was pulling AVIF/`rav1e` + EXR + WebP + HDR + TIFF + …) and disabling `arboard`'s image-clipboard default feature |
| `kettle --version` startup | < 10 ms wall, 5.0 MB peak RSS | Cold (no warm pages); 5 runs all rounded to 0.00 s |
| `kettle --screenshot OUT.png` | ≈ 250–270 ms wall, 236 MB peak RSS | Includes wgpu adapter init, offscreen Vulkan device, font system load, full GPU text + quad pipelines |
| `kettle --screenshot-menu OUT.png` | ≈ 240–250 ms wall, 236 MB peak RSS | Same as above + the second TextRenderer / menu_quads pass; identical memory footprint, ~10 ms faster on the GPU pipeline warmup pattern |

## What the numbers mean

- **Startup is fast.** `--version` is a single `clap::Parser::parse`
  + a `println!`; under 10 ms. Cold-cache process spawn dominates
  the runtime. This is the floor for any kettle invocation that
  doesn't touch the GPU.
- **GPU init is the screenshot cost.** ~250 ms wall for a single
  96×28 frame is almost entirely `wgpu::Instance::request_adapter` +
  `Adapter::request_device` + the first font-atlas glyph upload.
  The live windowed run pays this *once* per session; thereafter
  every frame is a sub-millisecond redraw against the warm
  pipeline.
- **Peak RSS = 236 MB.** Looks high for a terminal but is dominated
  by:
  - The bundled JetBrains Mono Nerd Font set (~50 MB of glyph data
    via `kettle_config::font::all()`).
  - The ~500 bundled themes (Ghostty + iTerm2-Color-Schemes set).
  - The wgpu adapter (software-Vulkan in the headless path; the
    GPU driver on a real machine pages most of this out).
  - The font atlas + glyph cache (one entry per visible glyph,
    grows on first render of each codepoint).

  A live windowed kettle session at idle measures ≈80–120 MB on the
  same machine; the headless `--screenshot` peak is an overestimate
  for the steady-state windowed case because software-Vulkan keeps
  more state resident than a hardware adapter would.

## Reproducing

```sh
cargo build --release -p kettle
./scripts/bench.sh
```

`scripts/bench.sh` requires `time` (GNU coreutils — on macOS use
`gtime` from `brew install coreutils`). Output goes to stdout; pipe
to a file or markdown table as you like. On macOS / Windows expect
slightly different numbers — startup is generally faster on macOS
arm64, the headless GPU path uses Metal / DX12 instead of software-
Vulkan, and the binary size differs because the universal2 macOS
build is fatter.

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

- **5-run minimum.** Wall-clock and RSS both have system jitter;
  `bench.sh` runs each measurement 5× and emits all five so the
  spread is visible.
- **Cold-cache start.** Each invocation is a fresh `exec`; we don't
  benchmark inside a long-lived process because the user pays the
  cold-start cost at every shell launch.
- **`/usr/bin/time -v` for RSS.** `Maximum resident set size`
  reports peak resident memory in KB; we convert to MB in the
  table above.
