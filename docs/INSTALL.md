# Install

For the default Linux installer and macOS download, start with
[README.md](../README.md#install). This guide covers other install options,
requirements, updates and removal.

## Supported platforms

| Platform | Architecture | Requirements and support |
|---|---|---|
| Linux | x86_64 | Tier 1; prebuilt glibc 2.35+ archive |
| Linux | aarch64 | Tier 1 distribution; cross-built glibc 2.35+ archive; native runtime checks are manual |
| macOS | Intel and Apple Silicon | Tier 1; signed, notarized universal app; macOS 11+ |
| Windows | Any | Unsupported since 4.0; final supported release: 3.3.0 |
| Other Linux targets | armv7l, i686, riscv64 | Experimental source builds; no prebuilt archives or guaranteed GPU support |

Prebuilt Linux archives work on Ubuntu 22.04+, Debian 12 and distributions
with a compatible glibc. They do not run on stock musl-based Alpine. On older
systems, use Nix or build from source. See [Testing](TESTING.md#release-pipeline-releaseyml)
for the distinction between package validation and native runtime coverage.

## Linux

The online installer needs curl with `--max-filesize`, GNU tar, OpenSSL 3.0+
with Ed25519 support, and Python 3. It installs under `~/.local` without sudo.

Pin the version for a reproducible install:

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \
  | KETTLE_VERSION=v5.7.1 sh
```

For a custom prefix, use `KETTLE_PREFIX`. The invoking user needs write access:

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \
  | KETTLE_PREFIX=/usr/local sh
```

You can combine `KETTLE_PREFIX` and `KETTLE_VERSION`. The installer verifies
current releases against the signed manifest before extraction. It refuses
unsafe paths, changed managed files and package-manager collisions. Review and
move conflicting files aside before retrying; do not overwrite an unknown tree.
See [Updates](UPDATES.md) for the verification and recovery rules.

### From a downloaded release tarball

Download the archive for your architecture and verify it as described
[below](#verifying-a-download-sha-256). For x86_64:

```sh
tar -xzf kettle-linux-x86_64.tar.gz
cd kettle
./install.sh
```

The installer places both `kettle` and `kettle-media-worker` in `~/.local/bin`,
the launcher in `~/.local/share/applications`, and icons under
`~/.local/share/icons/hicolor`. It uses an absolute PNG icon path so the user
launcher does not depend on an icon cache. Add `~/.local/bin` to `PATH`. If the
launcher does not appear, log out and back in.

### Uninstalling

Hit the **Super key** and type **"kettle"** to launch. To remove everything later:

```sh
~/.local/share/kettle/install.sh --uninstall
```

For a custom prefix, run its `share/kettle/install.sh --uninstall` instead.
Removal checks the installation record and leaves unrelated prefix files alone.
If installation was interrupted, rerun the same installer to finish it before
retrying removal. A changed managed file or untrusted path can block recovery;
inspect the reported conflict first.

## macOS

Unzip `kettle-macos-universal.zip` and move `kettle.app` to `~/Applications`
(create it if needed), or to `/Applications` if writable. Official apps are
Developer ID signed, notarized and stapled. Local and PR preview bundles are
unsigned development builds.

Run CLI commands in a Kettle pane. From another terminal, use the bundle path:

```sh
~/Applications/kettle.app/Contents/MacOS/kettle --version
```

To uninstall, quit Kettle and move the app to Trash. This leaves your config,
recordings and other user data in place. Use `kettle --config-path` before
removing the app if you also want to review its configuration.

## Package managers and Nix

Each release includes a `PKGBUILD` and a Homebrew formula. This snapshot has
no published `kettle-bin` AUR package or `reddimus/kettle` Homebrew tap. Use the
release app/archive, or follow the local packaging instructions in
[Arch](../packaging/arch/README.md) and
[Homebrew](../packaging/homebrew/README.md).

With Nix and flakes enabled, you can run or install directly from the repository:

```sh
nix run github:reddimus/kettle
nix profile install github:reddimus/kettle
nix profile remove kettle
```

The flake defines x86_64/aarch64 Linux and Apple Silicon macOS outputs. Intel
macOS needs the universal release app. Only x86_64 Linux has a repository Nix
build/runtime CI check. Linux packages include the media worker; Darwin Nix
packages provide the text terminal, with media unavailable under the worker
signature policy. Nix supplies libraries, but the host still needs a usable GPU
driver. See [Nix packaging](../packaging/nix/README.md) for pinned tags,
home-manager, system configuration, driver setup and `nix develop`.

## Archived Windows installs

Kettle 4.0 and later publish no Windows package. For recovery or removal, use
[the archived 3.3.0 release](https://github.com/Reddimus/kettle/releases/tag/v3.3.0)
and its bundled `install.ps1`. Remove an installed copy through Add/Remove
Programs, or run `install.ps1 -Uninstall` from that installation. The archived
[source instructions](https://github.com/Reddimus/kettle/blob/v3.3.0/docs/INSTALL.md)
cover custom prefixes, execution policy and legacy permission migration.

## Updating

Official Linux installer layouts and the macOS app can update themselves.
For a manual update:

```sh
kettle update
# Non-interactive automation:
kettle update --yes
```

Kettle verifies the signed manifest, size and SHA-256 before replacing files.
It never requests elevation or restarts open windows. The macOS updater replaces
the whole bundle. Package-manager, Cargo, Nix, manually copied and `local-dev`
installs must use their original install method. Rebuild source installs and
rerun the installer. See [Updates](UPDATES.md) for automatic update policy and
recovery. Updating from macOS 3.1.1 requires a manual app replacement once.

## Verifying a download (SHA-256)

Every release from **v1.3.4** onward ships a `.sha256` sidecar (current latest: v5.7.1)

Download the matching archive and sidecar before extracting. For Linux x86_64:

```sh
curl -fLO https://github.com/Reddimus/kettle/releases/download/v5.7.1/kettle-linux-x86_64.tar.gz
curl -fLO https://github.com/Reddimus/kettle/releases/download/v5.7.1/kettle-linux-x86_64.tar.gz.sha256
sha256sum -c kettle-linux-x86_64.tar.gz.sha256
```

On macOS, download the universal ZIP and its sidecar, then run:

```sh
shasum -a 256 -c kettle-macos-universal.zip.sha256
```

These checks detect corruption. The online installer and self-updater also
require the independent Ed25519 signature for current releases; missing or
failed authentication stops installation.

## From source

Requires Rust ≥ 1.95 (the workspace MSRV).
Linux also needs a C toolchain and these Debian/Ubuntu development packages:

```sh
sudo apt-get install -y build-essential pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev

git clone https://github.com/Reddimus/kettle
cd kettle
cargo build --locked --release -p kettle
cargo build --locked --profile media-worker -p kettle-media-worker
cp target/media-worker/kettle-media-worker target/release/kettle-media-worker
./target/release/kettle
```

Install the equivalent development packages on other Linux distributions.
Runtime rendering also needs a Vulkan loader and a hardware or software driver
(`libvulkan1` and `mesa-vulkan-drivers` on Debian/Ubuntu). macOS needs Rust and
Apple's command-line developer tools. Official universal app packaging has
additional [Xcode and signing requirements](../packaging/macos/README.md).

Build the worker separately: its `media-worker` profile retains unwinding and
keeps its image codecs separate from the terminal's clipboard TIFF feature.
The commands above use Cargo's default target directory. A custom target
location must also place the worker beside the terminal. `cargo install --path
crates/kettle` installs only the terminal; media reports `worker_missing` until
the sibling worker is installed. Unsigned local macOS workers are unavailable
under the release signature policy.

On Linux, build and install both binaries with:

```sh
./scripts/install.sh
```

Use `./scripts/install.sh --skip-build` only after building both outputs.
Source installs are marked `local-dev`, so stable self-updates cannot replace
them. To record each desktop-launcher session from a source install:

```sh
just install-recording
just install-recording "$HOME/Kettle recordings"
```

This recipe needs [just](https://github.com/casey/just). Stable release installs
should use `record = on` and `record-dir` in config because updates replace the
launcher. See [Recording](RECORDING.md) for limits and retention, and
[Testing](TESTING.md) for build validation.

## Optional media dependencies

GIF, APNG and animated WebP need no external decoder. macOS also reads MP4 and
QuickTime through AVFoundation, and HEIF and AVIF stills through ImageIO. Other
video formats, HEIF and AVIF stills on Linux, and AVIF before macOS 13 need
FFmpeg and ffprobe (7.1 or later for stills):

```sh
brew install ffmpeg          # macOS
sudo apt install ffmpeg      # Debian / Ubuntu
sudo dnf install ffmpeg-free # Fedora; codec availability varies
```

Kettle searches only `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`,
`/run/current-system/sw/bin` and `~/.nix-profile/bin`, with trusted ownership
and permissions. A temporary addition to `PATH` does not make a decoder
available. Kettle uses no GStreamer.

Linux video, PDF, HEIF and AVIF decoding need Landlock (kernel 5.13+ with
Landlock enabled). Containers must permit its three syscalls in their seccomp
policy. Without it, other images, SVG and Mermaid remain available, but
videos, PDFs and HEIF and AVIF stills are refused.
Linux sound also needs PulseAudio or a compatible PipeWire server and
`libpulse.so.0` (`libpulse0` on Debian/Ubuntu).

Opening video in an external player uses QuickTime Player on macOS or
`/usr/bin/mpv` on Linux. Install the latter with `sudo apt install mpv` or
`sudo dnf install mpv`; without it, the external-player row is absent.
Opening a PDF outside Kettle uses Preview on macOS, and on Linux
`/usr/bin/papers` (GNOME 47 and later), or `/usr/bin/evince` where Papers is
missing (`sudo apt install evince`); without either, that row is absent too.
See [Agent media](AGENT.md) for playback, previews and decoder diagnostics.
