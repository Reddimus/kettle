# Install

## Linux — easy desktop install (Ubuntu / Fedora / Arch / GNOME / KDE)

### One-line installer (recommended)

Downloads the latest prebuilt binary + XDG launcher + icons and drops
everything into `~/.local/`. No `sudo`, no Rust toolchain:

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
```

Pin a specific version (recommended for reproducible installs):

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \
  | KETTLE_VERSION=v2.1.0 sh
```

System-wide install (writes to a custom prefix; needs the
corresponding permissions):

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \
  | KETTLE_PREFIX=/usr/local sh
# binary at /usr/local/bin/kettle, launcher under /usr/local/share/applications
```

`KETTLE_VERSION` and `KETTLE_PREFIX` compose — pin both at once.

The script verifies the gzip magic bytes on the downloaded tarball,
checks the SHA-256 against the published sidecar (every release ships one), and runs
everything in a `mktemp -d` cleaned up on exit. Uninstall later via
`~/.local/share/kettle/install.sh --uninstall`.

### From source (cloned repo)

```sh
# Build deps (Debian / Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev

git clone https://github.com/Reddimus/kettle
cd kettle
./scripts/install.sh
```

Requires Rust ≥ 1.89 (the workspace MSRV).

### From a downloaded release tarball

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
  path above, or copy the files manually. Arch / Manjaro / EndeavourOS
  users: a ready-to-use AUR `PKGBUILD` lives at
  [`packaging/arch/PKGBUILD`](../packaging/arch/PKGBUILD); see
  [`packaging/arch/README.md`](../packaging/arch/README.md) for the
  one-time AUR submission walkthrough that lets users install via
  `yay -S kettle-bin`. NixOS / nix-flake users:
  `nix run github:reddimus/kettle` runs without installing; see
  [`packaging/nix/README.md`](../packaging/nix/README.md) for
  `nix profile install` + dev-shell + home-manager usage.
- **macOS** — `kettle-macos-universal.zip` containing `kettle.app`. Unzip and
  drag `kettle.app` to `/Applications`. First launch: right-click → Open
  (unsigned build). A ready-to-use Homebrew formula lives at
  [`packaging/homebrew/kettle.rb`](../packaging/homebrew/kettle.rb);
  see [`packaging/homebrew/README.md`](../packaging/homebrew/README.md)
  for the one-time tap-repo setup that lets users install with
  `brew tap reddimus/kettle && brew install kettle`.
- **Windows 11** — `kettle-windows-x86_64.zip` containing `kettle.exe` +
  `install.ps1`. Unzip anywhere, then run the bundled installer for
  Start menu + PATH integration:

  ```powershell
  # From the extracted folder:
  .\install.ps1
  ```

  The installer copies kettle into `%LOCALAPPDATA%\Programs\kettle`,
  creates a Start menu shortcut (so **Win-key → type "kettle"** finds
  it), adds it to your user PATH, and registers an Add/Remove
  Programs entry. No admin / UAC prompt — everything is per-user.
  Uses ConPTY + your default shell (PowerShell/cmd) at runtime.

  Or if you'd rather skip the installer and keep it portable: just
  run `.\kettle.exe` from the extracted folder. Pass `-Prefix
  "D:\PortableApps\kettle"` to `install.ps1` for a portable install
  to a custom location (skips PATH + registry + Start menu — pure
  copy).

  Uninstall later via Add/Remove Programs (`appwiz.cpl`), or
  `.\install.ps1 -Uninstall` from the install dir.

## First run

After install, launch `kettle` from any shell. A few one-liners worth
trying first:

```sh
kettle --list-themes      # browse the ~512 bundled themes
kettle --config-path      # show where your config file lives
kettle --list-keybinds    # see every default keybind (with overrides applied)
kettle --check-config     # validate the config; flags unknown keys
kettle --gpu-info         # print the wgpu adapter / backend / texture limits
```

To bootstrap a commented starter config in the right spot for your OS:

```sh
# Linux / WSL    — ~/.config/kettle/config (or $XDG_CONFIG_HOME/kettle/config)
# macOS          — ~/Library/Application Support/kettle/config
# Windows        — %APPDATA%\kettle\config
kettle --print-default-config > "$(kettle --config-path)"
```

(On Windows PowerShell: `kettle --print-default-config | Out-File -Encoding utf8 (kettle --config-path)`.)

Inside kettle: **right-click anywhere in a pane** for the context menu —
Copy / Paste / Split / Close, plus **Theme ▸** (cycle through ~512 bundled
themes), **Profile ▸**, and **Preferences ▸** with one-click toggles for
cursor blink, scrollbar mode, bell, copy-on-select, mouse-hide, and font
size. Reload config with `Ctrl+Shift+M`; cycle themes from the command
palette (`Ctrl+Shift+K`, type "Next theme"); jump between prompts with
`Ctrl+Up` / `Ctrl+Down` after enabling [shell integration](SHELL-INTEGRATION.md)
(bash / zsh / fish / **PowerShell**).

### Verifying a download (SHA-256)

Every release from **v1.3.4** onward ships a `.sha256` sidecar (current latest: v2.1.0)
generated on the same CI runner as the artifact. Verify a tarball
before extracting it:

```sh
# Linux / WSL
curl -fLO https://github.com/Reddimus/kettle/releases/download/v2.1.0/kettle-linux-x86_64.tar.gz
curl -fLO https://github.com/Reddimus/kettle/releases/download/v2.1.0/kettle-linux-x86_64.tar.gz.sha256
sha256sum -c kettle-linux-x86_64.tar.gz.sha256
# → kettle-linux-x86_64.tar.gz: OK
```

```sh
# macOS (shasum is preinstalled)
shasum -a 256 -c kettle-macos-universal.zip.sha256
```

```powershell
# Windows (PowerShell)
$expected = (Get-Content kettle-windows-x86_64.zip.sha256).Split()[0]
$actual   = (Get-FileHash kettle-windows-x86_64.zip).Hash.ToLower()
if ($expected -eq $actual) { "OK" } else { "MISMATCH" }
```

The one-line installer
([`scripts/install-online.sh`](../scripts/install-online.sh))
performs this check automatically. A failed verification aborts the
install with a clear error.

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
system packages. Minimum supported Rust version is **1.89** (Cargo.toml
`rust-version`); `rustup update stable` will always satisfy it.

## Verifying your build

```sh
cargo test --workspace      # 319+ tests incl. an offscreen GPU pipeline check
cargo run -p kettle -- --list-themes | wc -l   # 512
```

The GPU self-test (`kettle_render::offscreen_selftest`) compiles the WGSL
shaders on the platform backend (Vulkan/Metal/DX12) and runs an offscreen
render pass — it executes in CI on Linux, macOS and Windows.
