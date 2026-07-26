# Nix / NixOS install

[`flake.nix`](../../flake.nix) at the repo root makes kettle
installable on x86_64/aarch64 Linux and Apple Silicon macOS with
[Nix](https://nixos.org/download.html) + flakes enabled. Current
nixpkgs unstable no longer supports Intel macOS, so that platform
should use the universal Homebrew/release artifact instead. No
package-registry submission is required — the flake builds directly
from the git tag (or `main`).

## Quick paths

### Run without installing

```sh
nix run github:reddimus/kettle
# or pin to a tag (replace vX.Y.Z):
nix run "github:reddimus/kettle?ref=vX.Y.Z"
```

Builds the binary in a Nix sandbox, runs it, and leaves the result
in the Nix store (gc'd when not referenced). Great for trying
kettle on a NixOS box without committing to an install. Replace
The tag reference requires the release's `v` prefix.

Nix packages can provide the Vulkan/OpenGL loaders but cannot bundle a host's
vendor-specific GPU driver. NixOS users should keep normal graphics support
enabled in the system configuration. On another Linux distribution, a
Nix-installed GUI may need a driver bridge such as `nixGL` when the host driver
is not already visible to the Nix process.

### Install into the user profile

```sh
nix profile install github:reddimus/kettle
```

Persists across nix gc; uninstall with `nix profile remove kettle`
(or remove its profile index/store path). The wrapped binary lives
at `~/.nix-profile/bin/kettle`, on the default `$PATH` for both NixOS
and home-manager users.

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

The flake's `version` field must match `Cargo.toml`'s workspace version.
`scripts/release.sh` updates both in the same release PR and rejects a missing
or stale Nix version before it creates the commit. The committed root
`flake.lock` pins nixpkgs, flake-utils, and rust-overlay; update it deliberately
with `nix flake update`, review the input revisions, then run `nix flake check`
and `nix build`.

The `cargoLock.lockFile` auto-resolves crate sources from the
in-tree `Cargo.lock`, so no separate `cargoSha256` to maintain (the
`cargoHash`/`cargoVendorDir`/`cargoLock` triad confused enough
contributors that picking `cargoLock.lockFile` lets `Cargo.lock`
be the single source of truth for Rust dependencies).

## What the flake does that's kettle-specific

- **Rust toolchain pinned to 1.89** via
  [oxalica/rust-overlay](https://github.com/oxalica/rust-overlay)
  — matches the workspace MSRV declared in `Cargo.toml`.
  Drift-proofs the Nix path against a nixpkgs Rust version bump.
- **On Linux, `postFixup` appends to the existing RUNPATH** with
  `pkgs.lib.makeLibraryPath runtimeLibs` and asserts that Nix's glibc and
  libgcc paths remain present. wgpu / winit / glyphon
  dynamically load Vulkan, Wayland, X11, Xcursor, XInput2, and Xkb
  libraries at runtime; without a complete explicit rpath, `nix run`
  fails with a confusing
  `wgpu::Instance::request_adapter: NoAdapterFound` on systems
  where the loader isn't otherwise on `LD_LIBRARY_PATH`.
- **A Linux runtime check creates a visible X11 window under Xvfb**
  with Mesa's architecture-matched software Vulkan driver and no
  `LD_LIBRARY_PATH`. This verifies
  the installed package's own RUNPATH rather than accidentally borrowing
  libraries from a developer shell or CI runner. CI builds the cargo-test and
  runtime-smoke checks as separate named steps so a renderer failure is not
  hidden behind a generic flake-check failure.
- **GPU + visual regression tests are skipped during `nix build`**
  via `checkFlags`: `gpu_tests::gpu_pipelines_compile_and_render_offscreen`,
  `context_menu_renders_visibly_with_text`, and
  `compact_scrollbar_is_visible_contrasting_and_edge_scoped`. The
  Nix sandbox has no Vulkan-capable GPU; CI on a real GitHub runner
  under software-Vulkan still exercises them.
- **On Linux, `devShells.default`** sets `LD_LIBRARY_PATH` to the
  same runtime libs so `cargo run` inside the dev shell mirrors
  the patched-rpath behavior of the built package. Darwin uses the
  frameworks supplied by the Nix stdenv instead.

## Why this lives in the main repo

Same rationale as `packaging/{homebrew,arch}/` — the flake version
field tracks the release, so it bumps in the same PR as
`Cargo.toml`. The repo serves as the canonical source of *every*
distribution channel, and flakes are uniquely flake-native (no
separate tap / AUR repo to push to).

## Sources

Pattern adapted from:

- [alacritty/alacritty](https://github.com/alacritty/alacritty)
  — the same winit/X11/Wayland dynamic-loading pattern; its Nix
  packaging informed kettle's runtime-closure treatment.
- [helix-editor/helix](https://github.com/helix-editor/helix) —
  rust-overlay pinned toolchain pattern.
- [oxalica/rust-overlay](https://github.com/oxalica/rust-overlay)
  README — `makeRustPlatform` with a pinned toolchain.
