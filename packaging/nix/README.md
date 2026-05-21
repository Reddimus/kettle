# Nix / NixOS install

[`flake.nix`](../../flake.nix) at the repo root makes kettle
installable on any system with [Nix](https://nixos.org/download.html)
+ flakes enabled. No package-registry submission required — the
flake builds directly from the git tag (or `main`).

## Quick paths

### Run without installing

```sh
nix run github:reddimus/kettle
# or pin to a tag:
nix run github:reddimus/kettle/v1.3.5
```

Builds the binary in a Nix sandbox, runs it, and leaves the result
in the Nix store (gc'd when not referenced). Great for trying
kettle on a NixOS box without committing to an install.

### Install into the user profile

```sh
nix profile install github:reddimus/kettle
```

Persists across nix gc; uninstall with
`nix profile remove github:reddimus/kettle`. The wrapped binary
lives at `~/.nix-profile/bin/kettle`, on the default `$PATH` for
both NixOS and home-manager users.

### Add to a flake-based system / home-manager config

Add to your `flake.nix` inputs:

```nix
inputs.kettle.url = "github:reddimus/kettle";
```

Then in the outputs / packages:

```nix
environment.systemPackages = [ kettle.packages.${pkgs.system}.default ];
# or for home-manager:
home.packages = [ inputs.kettle.packages.${pkgs.system}.default ];
```

### Hack on kettle from a `nix develop` shell

```sh
git clone https://github.com/Reddimus/kettle
cd kettle
nix develop
# Drops you into a shell with the workspace MSRV (Rust 1.89) and
# every runtime lib on LD_LIBRARY_PATH. `cargo run`, `cargo test`,
# `cargo clippy` all work without further setup.
```

Useful when you have Nix but no Rust toolchain installed system-
wide — the dev shell is fully hermetic.

## Per-release maintenance

The flake's `version = "1.3.5";` field needs to bump in the same
PR as `Cargo.toml`'s workspace version. The `cargoLock.lockFile`
auto-resolves crate sources from the in-tree `Cargo.lock`, so no
separate `cargoSha256` to maintain (the
`cargoHash`/`cargoVendorDir`/`cargoLock` triad confused enough
contributors that picking `cargoLock.lockFile` lets the lock file
be the single source of truth).

## What the flake does that's kettle-specific

- **Rust toolchain pinned to 1.89** via
  [oxalica/rust-overlay](https://github.com/oxalica/rust-overlay)
  — matches the workspace MSRV declared in `Cargo.toml` (cycle 250).
  Drift-proofs the Nix path against a nixpkgs Rust version bump.
- **`postFixup` patches the rpath** with
  `pkgs.lib.makeLibraryPath runtimeLibs`. wgpu / winit / glyphon
  dlopen Vulkan / Wayland / Xkb at runtime; without an explicit
  rpath, `nix run` fails with a confusing
  `wgpu::Instance::request_adapter: NoAdapterFound` on systems
  where the loader isn't otherwise on `LD_LIBRARY_PATH`.
- **GPU + visual regression tests are skipped during `nix build`**
  via `checkFlags = [ "--skip=gpu_tests::..." "--skip=
  context_menu_renders_visibly_with_text" ]`. The Nix sandbox has
  no Vulkan-capable GPU; CI on a real GitHub runner under
  software-Vulkan still exercises them.
- **`devShells.default`** sets `LD_LIBRARY_PATH` to the same
  runtime libs so `cargo run` inside the dev shell mirrors the
  patched-rpath behavior of the built package.

## Why this lives in the main repo

Same rationale as `packaging/{homebrew,arch}/` — the flake version
field tracks the release, so it bumps in the same PR as
`Cargo.toml`. The repo serves as the canonical source of *every*
distribution channel, and flakes are uniquely flake-native (no
separate tap / AUR repo to push to).

## Sources

Pattern adapted from:

- [alacritty/alacritty](https://github.com/alacritty/alacritty)
  — same wgpu + winit + Wayland dlopen story; their flake's rpath
  patch is what kettle's mirrors.
- [helix-editor/helix](https://github.com/helix-editor/helix) —
  rust-overlay pinned toolchain pattern.
- [oxalica/rust-overlay](https://github.com/oxalica/rust-overlay)
  README — `makeRustPlatform` with a pinned toolchain.
