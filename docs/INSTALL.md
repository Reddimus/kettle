# Install

## Supported platforms

| Platform | Arch | Support |
|---|---|---|
| Linux | x86_64 | **Tier 1** — glibc 2.35+, prebuilt binary + one-line installer |
| Linux | aarch64 | **Tier 1 distribution** — glibc 2.35+, cross-built prebuilt binary + one-line installer; native ARM UI/runtime is manually verified but not yet in CI |
| macOS | universal (Intel + Apple Silicon) | **Tier 1** — signed and notarized universal `.app` bundle |
| Windows 11 | x86_64 | **Tier 1** — `.zip` + `install.ps1` |
| Windows 11 | aarch64 | **Tier 2** — native source build verified; no prebuilt archive |
| Linux/other | armv7l, i686, riscv64, … | **Tier 2** — source build only, *experimental* (wgpu/glyphon have no tier-1 GPU support on these targets) |

Tier-1 targets are required before a release can publish. Linux aarch64 is
cross-built and package/ABI validated on x86_64 CI; a Parallels Ubuntu ARM
guest supplies additional native build, PTY, software/virtual-GPU, and live-UI
evidence, but is a manual check rather than a release gate. Every archive has a
SHA-256 sidecar; update metadata for every self-updating platform is
additionally signed by a dedicated Ed25519 release key. Tier-2 targets have no prebuilt binary;
`scripts/install-online.sh` points you at a source build (or `nix run
github:Reddimus/kettle` to try it in a sandbox first).

## Updating

Official Windows and Linux installer layouts, and the macOS `kettle.app`, can
update themselves:

```sh
kettle --check-update
kettle update
# Non-interactive automation only:
kettle update --yes
```

`kettle --update` is an interactive convenience alias. Kettle verifies the
signed stable manifest, archive size, and SHA-256 before a transactional
replacement. It never requests elevation and never restarts open windows. On
Windows, the verified release waits in private staged state until all Kettle
windows close; new launches hand off to the helper instead of running the old
binary. Upgrading a pre-v2.35 Windows install requires one rerun of the bundled
`install.ps1` to bootstrap that helper-aware build. A
Windows Kettle executable launched from WSL updates the same Windows install;
a native WSL/Linux Kettle updates its Linux prefix. On macOS the whole bundle is
replaced at once, because the code signature seals it as a unit. Package-manager,
Cargo, Homebrew, Nix, AUR, and manually copied installs are refused so their
owner remains authoritative. See [UPDATES.md](UPDATES.md) for policy and recovery.

## Linux — easy desktop install (Ubuntu / Fedora / Arch / GNOME / KDE)

The prebuilt GNU/Linux archives require glibc 2.35 or newer. That includes
Ubuntu 22.04, Debian 12, and current Fedora/Arch releases. On an older
distribution, build from source or use the Nix package instead.

### One-line installer (recommended)

Downloads the latest prebuilt binary + XDG launcher + icons and drops
everything into `~/.local/`. No `sudo`, no Rust toolchain:

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
```

Pin a specific version (recommended for reproducible installs):

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \
  | KETTLE_VERSION=v3.2.0 sh
```

System-wide install (writes to a custom prefix; needs the
  corresponding permissions and Python 3):

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \
  | KETTLE_PREFIX=/usr/local sh
# binary at /usr/local/bin/kettle, launcher under /usr/local/share/applications
```

`KETTLE_VERSION` and `KETTLE_PREFIX` compose — pin both at once. The installer
requires a current `curl` with `--max-filesize`, GNU `tar`, and OpenSSL 3.0+
with Ed25519 support. The published archives are glibc binaries and do not run
on stock musl-based Alpine; installing GNU tar alone does not make them
compatible. Alpine users must build for their environment or run Kettle in a
supported glibc environment.

The script accepts only an exact `vMAJOR.MINOR.PATCH`, caps the archive at
256 MiB, and caps the signed manifest/signature separately. HTTPS-only
redirects, finite connection/transfer/low-speed deadlines, curl's declared
  size check, a POSIX file-size resource limit, and a final byte count cover both
known-length and chunked responses. For v2.35.0 and newer it verifies the
Ed25519 signature, canonical product/channel/tag/target identity, signed byte
count, and SHA-256 without permitting a checksum-only downgrade. Older
releases must provide their exact same-origin SHA-256 sidecar; checksum-less
releases are refused. Before extraction it permits at most 128 safe regular
files or directories under one `kettle/` root and 512 MiB unpacked, rejecting
links, devices, aliases, unsafe permissions, and path traversal. Archives from
  v2.36.0 onward must also contain the inner package manifest. The current
  installer additionally requires the authenticated archive to contain the
  descriptor-relative `install-unix.py` helper; legacy packages without it are
  refused instead of falling back to path-based writes. All work stays in a
  private `mktemp -d` cleaned on exit. Uninstall later via
  `~/.local/share/kettle/install.sh --uninstall`.

  Installation and removal walk every prefix component with no-follow directory
  descriptors, require trusted ownership and non-group/non-other-writable modes,
  and publish files by atomic descriptor-relative replacement. The installer
  records each managed file's path, mode, size, and SHA-256 plus each directory
  it created in `share/kettle/install-files.json`. Upgrade and uninstall first
  verify the complete recorded tree, then touch only those recorded paths.
  Unrelated files in a shared prefix such as `~/.local` or `/usr/local` are left
  alone. A legacy install, a changed recorded file, a symlink in the path, or a
  package-manager-owned collision has no trustworthy provenance and is refused;
  move the old tree aside after reviewing it and install into collision-free
  paths rather than asking the installer to guess ownership.

### From source (cloned repo)

```sh
# Build deps (Debian / Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev \
  libvulkan1 mesa-vulkan-drivers

git clone https://github.com/Reddimus/kettle
cd kettle
./scripts/install.sh
```

Requires Rust ≥ 1.89 (the workspace MSRV).

An in-tree source install is marked `local-dev`, so `kettle update` will not
replace it with a stable binary. Rebuild and rerun `./scripts/install.sh` to
refresh it. To make every Ubuntu Super-key launch record automatically (the
same effect as `record = on` in the config file, which every build supports):

```sh
just install-recording
# or choose another private directory, including one whose path contains spaces:
just install-recording "$HOME/Kettle recordings"
```

That launcher wires `KETTLE_RECORD_DIR` into the `.desktop` entry; see
[RECORDING.md](RECORDING.md) for privacy, limits, and retention behavior.

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
  (plus 8-bit PNG fallbacks at 16/24/32/48/64/128/256). User-local source,
  release-tarball, and stable self-update installs all point the launcher at
  the absolute 256x256 PNG so GNOME Super-key search does not depend on a
  user-local icon cache.

Make sure `~/.local/bin` is on your `PATH`. Hit the **Super key** and
type **"kettle"** to launch. To remove everything later:

```sh
./scripts/install.sh --uninstall
```

## From a release

Each tagged release ships prebuilt artifacts, built and packaged on real
GitHub runners for every platform:

- **Linux** — `kettle-linux-x86_64.tar.gz` (binary + `kettle.desktop` + icon
  + `install.sh` + the no-follow `install-unix.py` helper). Extract and run
  `./install.sh` for the easy-install
  path above, or copy the files manually. Arch / Manjaro / EndeavourOS
  users: each release includes a ready-to-use `PKGBUILD`, rendered from
  [`packaging/arch/PKGBUILD.in`](../packaging/arch/PKGBUILD.in) after CI knows
  the archive checksum. `kettle-bin` is not currently published in AUR, so
  `yay -S kettle-bin` does not resolve; see
  [`packaging/arch/README.md`](../packaging/arch/README.md) for local
  `makepkg` use and the outstanding publication workflow.
  NixOS / nix-flake users:
  `nix run github:reddimus/kettle` runs without installing; see
  [`packaging/nix/README.md`](../packaging/nix/README.md) for
  `nix profile install` + dev-shell + home-manager usage.
- **macOS** — `kettle-macos-universal.zip` containing `kettle.app`. Official
  release apps are Developer ID signed, notarized, stapled, and assessed by
  Gatekeeper before publication. Unzip and drag `kettle.app` to `/Applications`;
  it should open normally on first launch. Locally built and pull-request preview
  apps are intentionally unsigned and are for development use rather than the
  recommended installation path.

  From 3.2.0 the app updates itself: `kettle update`, or the in-app prompt,
  replaces the whole bundle and verifies the replacement with `codesign` and
  `spctl` before it swaps. Moving up *from* 3.1.1 is a manual drag, once,
  because 3.1.1 predates that code.

  Each release includes a ready-to-use `kettle.rb` formula rendered from
  [`packaging/homebrew/kettle.rb.in`](../packaging/homebrew/kettle.rb.in);
  however, the `Reddimus/homebrew-kettle` tap repository is not currently
  published, so `brew tap reddimus/kettle` does not resolve. See
  [`packaging/homebrew/README.md`](../packaging/homebrew/README.md) for the
  remaining publication workflow. Until then, install the release `.app`
  directly.
- **Windows 11** — `kettle-windows-x86_64.zip` containing `kettle.exe`, the
  `kettle.com` console launcher, and `install.ps1`. Unzip anywhere, then run the bundled installer for
  Start menu + PATH integration:

  ```powershell
  # From the extracted folder:
  .\install.ps1
  ```

  If PowerShell refuses with *"running scripts is disabled on this system"*
  (a Restricted/AllSigned execution policy), run it without changing your
  machine's policy:

  ```powershell
  powershell -ExecutionPolicy Bypass -File .\install.ps1
  ```

  The installer copies kettle into `%LOCALAPPDATA%\Programs\kettle`,
  creates a Start menu shortcut (so **Win-key → type "kettle"** finds
  it), adds it to your user PATH, and registers an Add/Remove
  Programs entry. Re-running the installer replaces its managed shortcut, so
  obsolete launcher targets or arguments cannot survive an upgrade. No admin /
  UAC prompt — everything is per-user.
  Uses ConPTY + your default shell (PowerShell/cmd) at runtime.
  A bare `kettle` command resolves to the console launcher so PowerShell and cmd
  wait for CLI prompts and preserve exit codes; Windows Search continues to
  target `kettle.exe`, which opens without a console flash.

  Or if you'd rather skip the installer and keep it portable: just
  run `.\kettle.exe` from the extracted folder. Pass `-Prefix
  "D:\PortableApps\kettle"` to `install.ps1` for a portable install
  to a custom location (skips PATH + registry + Start menu — pure
  copy). The prefix must be a dedicated directory named `kettle`. Every new
  permanent directory and file receives a protected, non-inheriting ACL granting
  full control only to the initiating Windows identity, SYSTEM, and
  Administrators. Existing roots must already have that exact owner/DACL; the
  parent chain is also rejected if an untrusted principal such as `Users` or
  `Everyone` can replace a component. The path must remain on one local fixed
  physical volume;
  network/UNC, removable, `SUBST`, and other non-volume DOS-device mappings are
  rejected. Prefix components also reject Win32 device names, alternate
  streams, invalid/control characters, traversal spellings, trailing dot/space
  aliases, and reparse points.

  An install made by an older Kettle installer may still inherit a writable
  parent ACL. Review the tree, then migrate it from a trusted extracted release
  or source checkout (never from the helper inside that writable prefix):

  ```powershell
  .\install.ps1 -Prefix "D:\PortableApps\kettle" -MigrateLegacyPermissions
  ```

  Migration is opt-in and fail-closed: it requires a trusted parent chain, the
  current identity to own the root, and the exact bounded Kettle file/directory
  grammar with no reparse points or unrelated entries. A normal reinstall or
  uninstall refuses an old broad-ACL root until this migration succeeds.

  An extracted release is accepted as the stable channel only when its bounded
  package manifest has the exact target/version, sorted file set, sizes,
  SHA-256 digests, and modes. A rerun through the saved helper preserves the
  existing `stable` or `local-dev` channel instead of silently changing update
  ownership. Installation preflights the complete package and the aggregate
  existing backup size (each capped at 512 MiB) before publishing any payload.
  It stages and backs up at most 127 managed files in a sibling transaction
  directory, durably records rollback coverage before every publication, and
  rolls the whole package back on failure. Both ownership markers and creation
  of the `shell-integration` directory participate in that same write-ahead
  transaction, so a stopped first install cannot leave an unowned payload and a
  stopped upgrade restores the complete prior package. A later invocation
  validates and recovers an interrupted transaction before deciding whether the
  prefix is a new or existing install. The transaction directory is created
  with the same protected ACL as the permanent root and payload; recovery
  rejects a different owner, inherited or extra access, and reparse points
  before reading the journal. Resume an interrupted machine-wide install as the
  same Windows identity that started it.

  Abrupt termination can leave an unpublished staging leaf. Recovery removes
  only the exact installer grammar `.kettle-install-tmp-<32 lowercase hex>`
  after validating the containing directory and confirming by handle that the
  leaf is current-user-owned, ordinary, non-reparse, single-link, and bounded.
  The same checks apply to Rust persistence leftovers named
  `.<destination>.tmp.<pid>.<epoch-nanoseconds>.<sequence>`; the destination
  must be one of Kettle's known managed leaves, the numeric fields must fit
  Rust's `u32`/`u128`/`u64` types with canonical decimal spelling, and the PID
  must be provably dead. Live, inaccessible, malformed, linked, or reparse
  lookalikes fail closed.

  The saved uninstaller validates strict product/target JSON and the exact
  bounded managed tree, rejects reparse points, and removes only known leaf
  files and now-empty managed directories. It never recursively follows the
  prefix, leaving unrelated files and any normal Kettle installation untouched.
  Interrupted authenticated-update stage/backup/quarantine artifacts are
  accepted only under their exact bounded transaction grammar. The narrow
  binary-backup names left by older Kettle installers are retained during
  upgrade and removed as ordinary leaves during uninstall; arbitrary `.bak-*`
  files are not adopted. A current schema-3 pending-update record must have the
  exact typed product, target, version, archive, helper, signature capsule,
  selected asset, package manifest, digest, and counter fields written by the
  Rust updater. Its nested asset and package file set are bounded and constrained
  to the Windows release grammar. During uninstall, those identities remain
  valid if a named helper or archive has already disappeared in an earlier
  removal step or after a crash; every object that still exists is validated
  independently before deletion.

  Uninstall later via Add/Remove Programs (`appwiz.cpl`), or
  `.\install.ps1 -Uninstall` from the install dir.

  `-WithShellIntegration` edits the PowerShell profile only when its managed
  BEGIN/END markers are absent or form one unique balanced standalone block.
  Ambiguous or broken markers fail before install/uninstall mutation. Profile
  files are capped at 4 MiB and must be ordinary, single-link, non-reparse
  files without alternate streams, EFS encryption, compression, sparse/offline
  storage, or other special attributes. A retained handle blocks concurrent
  replacement while UTF-8/UTF-16 encoding, BOM, newline spelling, protected
  DACL/ACEs, timestamps, supported attributes, and all text outside the block
  are preserved through one atomic same-volume rename over the retained
  destination. The installer never first moves the original profile to a
  retired name, so interruption cannot leave the profile pathname absent.

  > **No `winget` / `scoop` recipe yet.** `winget install kettle` and
  > `scoop install kettle` don't resolve — kettle isn't in the winget-pkgs
  > repo or a scoop bucket. If you'd like to maintain one, the SHA-256
  > sidecars shipped with every release satisfy both ecosystems' integrity
  > checks, and the generated `kettle.rb` + `PKGBUILD` release assets are
  > ready-made templates for the manifest shape. Until then, use
  > `install.ps1` above (it covers PATH + Start-menu + auto-uninstall, the
  > same integration a package manager would give you).

## First run

After install, launch `kettle` from any shell. A few one-liners worth
trying first:

```sh
kettle --list-themes      # browse the 500+ bundled themes
kettle --config-path      # show where your config file lives
kettle --list-keybinds    # see every default keybind (with overrides applied)
kettle --check-config     # validate the config; flags unknown keys
kettle --gpu-info         # print the wgpu adapter / backend / texture limits
```

To bootstrap a commented starter config in the right spot for your OS:

```sh
# Linux / WSL    — ~/.config/kettle/config (or $XDG_CONFIG_HOME/kettle/config)
# macOS          — ~/.config/kettle/config (kettle uses XDG paths, not ~/Library)
# Windows        — %APPDATA%\kettle\config (a stray HOME is ignored; set XDG_CONFIG_HOME for ~/.config)
# (always run `kettle --config-path` to see the exact resolved location)
# Easiest + cross-platform safe — creates the directory, writes the file,
# won't overwrite an existing config:
kettle --write-default-config
# Or redirect it yourself (note: PowerShell 5.1's `>` writes UTF-16, which
# kettle now reads, but `--write-default-config` avoids the encoding/dir
# pitfalls entirely):
kettle --print-default-config > "$(kettle --config-path)"
```

Inside kettle: **right-click anywhere in a pane** for the context menu —
Copy / Paste / Split / Close, plus **Theme ▸** (cycle through 500+ bundled
themes), **Profile ▸**, and **Preferences ▸** with one-click toggles for
cursor blink, scrollbar mode, bell, copy-on-select, mouse-hide, and font
size. Reload config with `Ctrl+Shift+M`; cycle themes from the command
palette (`Ctrl+Shift+K`, type "Next theme"); jump between prompts with
`Ctrl+Up` / `Ctrl+Down` after enabling [shell integration](SHELL-INTEGRATION.md)
(bash / zsh / fish / **PowerShell**).

For a clipboard screenshot, press `Ctrl+Shift+V`. With `paste-images` enabled
(the default), Kettle writes a bounded private temporary PNG and pastes its
quoted path into the focused pane. Client-native attachment shortcuts vary by
version and platform and are outside Kettle's compatibility contract. To attach
an image to a new Codex session directly, use the option exposed by
`codex --help`:

```sh
codex --image ./screenshot.png "Inspect this image"
# equivalent short option:
codex -i ./screenshot.png "Inspect this image"
```

See [TERMINAL-CLIENT-COMPATIBILITY.md](TERMINAL-CLIENT-COMPATIBILITY.md) for
the exact transport and smoke-test boundaries.

Copying or dropping a video pastes its quoted path through the same channel.
With `paste-video-preview` enabled, Kettle also shows a short-lived poster for
the exact local file. The poster is informational; the program in the pane
still decides whether to read the path.

### AI agents / MCP

kettle ships an opt-in agent surface (`kettle exec` / `kettle ctl` /
`kettle mcp`). To let Claude Code drive it as native tools, register the MCP
server once:

```sh
claude mcp add kettle -- kettle mcp
```

…or, scoped to a single project, drop a `.mcp.json` at the repo root:

```json
{ "mcpServers": { "kettle": { "command": "kettle", "args": ["mcp"] } } }
```

`kettle` must resolve on PATH first. On Windows, `install.ps1` adds kettle to
PATH but already-running shells keep their old snapshot — open a **fresh**
shell before running `claude mcp add`. On Linux, make sure `~/.local/bin` is on
PATH.

See [docs/AGENT.md](AGENT.md) for the full surface (`kettle exec` headless
one-shot, the `kettle ctl` control client, `kettle mcp`) and its threat model.

### Verifying a download (SHA-256)

Every release from **v1.3.4** onward ships a `.sha256` sidecar (current latest: v3.2.0)
generated on the same CI runner as the artifact. Verify a tarball
before extracting it:

```sh
# Linux / WSL
curl -fLO https://github.com/Reddimus/kettle/releases/download/v3.2.0/kettle-linux-x86_64.tar.gz
curl -fLO https://github.com/Reddimus/kettle/releases/download/v3.2.0/kettle-linux-x86_64.tar.gz.sha256
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
([`scripts/install-online.sh`](../scripts/install-online.sh)) uses these
sidecars only for releases older than the signed-manifest channel. Current
releases require the independent Ed25519 trust root and additionally bind the
archive size and platform identity. Any missing or failed required
verification aborts before extraction.

## From source (all platforms)

```sh
# Linux build deps (Debian/Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev

git clone https://github.com/Reddimus/kettle
cd kettle
cargo run --release
```

macOS needs only a stable Rust toolchain (`rustup`). Windows needs the Visual
Studio 2022 Build Tools **Desktop development with C++** workload and a Windows
SDK in addition to Rust. A native Windows ARM64 build also needs the **MSVC
ARM64 build tools** and **C++ Clang tools for Windows** components: `ring` uses
the component's x64-hosted `clang.exe` targeting ARM64, while other native
crates use the ARM64 MSVC linker and libraries. Run Cargo from an ARM64
Developer Command Prompt, or
initialize an ordinary shell first:

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=arm64 -host_arch=arm64
rustup target add aarch64-pc-windows-msvc
cargo build --locked --workspace --all-targets
```

The optional `just` gate runner has a native ARM64 package (`winget install
Casey.Just`). Minimum supported Rust version is **1.89** (Cargo.toml
`rust-version`); `rustup update stable` will always satisfy it.

## Verifying your build

```sh
cargo test --workspace      # an extensive suite incl. an offscreen GPU pipeline
                            # check — see docs/TESTING.md for the per-crate breakdown
cargo run -p kettle -- --list-themes | wc -l   # 500+ (currently 532)
```

The GPU self-test (`kettle_render::offscreen_selftest`) compiles the WGSL
shaders on the platform backend (Vulkan/Metal/DX12) and runs an offscreen
render pass — it executes in CI on Linux, macOS and Windows. It does not need
a visible desktop, but full local coverage does need a usable graphics backend
(the Linux packages above provide the Vulkan loader and Mesa software/hardware
drivers). The workspace also contains native PTY/ConPTY lifecycle tests. See
[TESTING.md](TESTING.md) for their prerequisites and soft-skip semantics.

## Regenerating the app icons (contributors)

`scripts/gen-icons.py` is the single source of truth for the icon geometry. It
emits one custom `>(_)~` terminal-kettle mark for Linux, Windows, and the
foreground of `packaging/macos/AppIcon.icon`. The punctuation is drawn as five
font-independent, fully opaque vector strokes on the default TokyoNight
background at normal sizes. The 16 px fixed-size Linux and Windows assets plus
the retained compatibility iconset use a simplified `>_` optical-size mark
because five punctuation strokes merge at that physical limit. The native
Icon Composer vector retains the full mark in every rendition. Parentheses and
steam in the full mark use true cubic curves; the raised,
square-ended underscore keeps the full-size mark from completing a U-shaped
outline. All five are distinct in the generated 24 px raster. The renderer's
light review variant swaps the dark face and Kettle-blue mark colors exactly
while retaining identical geometry; tests cover it, but the generator does not
write it as a package asset and the native macOS document does not currently
ship it as a separate appearance. There is no inner rounded face or border
whose curve can fight the platform mask. The system owns the only outer mask,
and Xcode generates the previous-release fallback for the macOS 11 deployment
target.
The generator also writes the fixed-size hicolor PNGs (`kettle-16.png` …
`kettle-256.png`), the retained compatibility iconset, and the Windows `.ico`:

```sh
# Cross-platform path (needs Pillow) — regenerates both SVG sources,
# AppIcon.icon, every Linux PNG and compatibility iconset member, and .ico:
python3 scripts/gen-icons.py

# Backward-compatible wrapper around the same canonical generator:
./scripts/gen-icons.sh
```

The scripts emit **8-bit/color RGBA** PNGs. This matters: 16-bit PNGs are
silently rejected by GNOME Shell's icon loader. Edit the geometry constants in
`gen-icons.py`, re-run it, and commit the generated SVGs and rasters together.
Verify with
`file packaging/linux/kettle-*.png` (every line should read `8-bit/color RGBA`).

**Why the user-install `.desktop` uses an absolute `Icon=` path.** GNOME
Shell's `StIconTheme` won't resolve a *themed* icon name (`Icon=kettle`) from
a user-local `~/.local/share/icons/hicolor` that lacks an `icon-theme.cache`
— so the Super-key search tile stays blank. `scripts/install.sh` therefore
rewrites `Icon=` to the absolute installed PNG path, which bypasses icon-theme
resolution and renders regardless of cache state (no shared cache to go stale
and hide other apps' icons). The shipped `packaging/linux/kettle.desktop`
keeps the themed `Icon=kettle`, which is correct for distro packages whose
post-install hooks refresh the system hicolor cache. After the first install,
an already-running GNOME session may need a log-out / log-in (or an icon-theme
toggle) before the launcher entry appears.
