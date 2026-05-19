# Install

## From a release

Each tagged release ships prebuilt artifacts, built and packaged on real
GitHub runners for every platform:

- **Linux** — `kettle-linux-x86_64.tar.gz` (binary + `kettle.desktop`).
  Extract, then `install -Dm755 kettle ~/.local/bin/kettle` and copy
  `kettle.desktop` to `~/.local/share/applications/`.
- **macOS** — `kettle-macos-universal.zip` containing `kettle.app`. Unzip and
  drag `kettle.app` to `/Applications`. First launch: right-click → Open
  (unsigned build).
- **Windows 11** — `kettle-windows-x86_64.zip` containing `kettle.exe`. Unzip
  anywhere and run; uses ConPTY + your default shell (PowerShell/cmd).

## From source (all platforms)

```sh
# Linux build deps (Debian/Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev

git clone https://github.com/Reddimus/kettle
cd kettle
cargo run --release
```

macOS and Windows need only a stable Rust toolchain (`rustup`) — no extra
system packages.

## Verifying your build

```sh
cargo test --workspace      # 20 tests incl. an offscreen GPU pipeline check
cargo run -p kettle -- --list-themes | wc -l   # 512
```

The GPU self-test (`kettle_render::offscreen_selftest`) compiles the WGSL
shaders on the platform backend (Vulkan/Metal/DX12) and runs an offscreen
render pass — it executes in CI on Linux, macOS and Windows.
