# Install

## Linux — easy desktop install (Ubuntu / Fedora / Arch / GNOME / KDE)

The simplest path on Linux is the bundled installer. It builds the
release binary (or unpacks a pre-built one from a release tarball),
drops the launcher entry + icon into the right XDG paths, and the
kettle tile shows up in the GNOME Activities overview / Ubuntu Super-
key search / KDE Krunner.

**From source** (cloned repo):

```sh
# Build deps (Debian / Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev

git clone https://github.com/Reddimus/kettle
cd kettle
./scripts/install.sh
```

**From a release tarball** (no Rust toolchain needed):

```sh
tar -xzf kettle-linux-x86_64.tar.gz
cd kettle
./install.sh
```

Either way, after install:

- Binary at `~/.local/bin/kettle`
- Launcher at `~/.local/share/applications/kettle.desktop`
- Icon at `~/.local/share/icons/hicolor/scalable/apps/kettle.svg`
  (plus PNG fallbacks at 32/48/64/128/256)

Make sure `~/.local/bin` is on your `PATH`. Hit the **Super key** and
type **"kettle"** to launch. To remove everything later:

```sh
./scripts/install.sh --uninstall
```

## From a release

Each tagged release ships prebuilt artifacts, built and packaged on real
GitHub runners for every platform:

- **Linux** — `kettle-linux-x86_64.tar.gz` (binary + `kettle.desktop` + icon
  + `install.sh`). Extract and run `./install.sh` for the easy-install
  path above, or copy the files manually.
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
cargo test --workspace      # 240+ tests incl. an offscreen GPU pipeline check
cargo run -p kettle -- --list-themes | wc -l   # 512
```

The GPU self-test (`kettle_render::offscreen_selftest`) compiles the WGSL
shaders on the platform backend (Vulkan/Metal/DX12) and runs an offscreen
render pass — it executes in CI on Linux, macOS and Windows.
