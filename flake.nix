{
  description = "kettle — a fast, cross-platform, GPU-accelerated terminal emulator written in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pin to the workspace MSRV (1.89, cycle 250) so a `nix build`
        # uses exactly the toolchain CI verifies on every PR. Drift-
        # proofs the Nix path against a nixpkgs Rust version bump.
        rustToolchain = pkgs.rust-bin.stable."1.89.0".default;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Runtime libraries the wgpu / winit / glyphon / portable-pty
        # stack dlopens at runtime — same set the release.yml
        # ubuntu-latest runner installs.
        runtimeLibs = with pkgs; [
          fontconfig
          freetype
          libGL
          libxkbcommon
          vulkan-loader
          wayland
          xorg.libX11
          xorg.libxcb
          xorg.libxkbfile
        ];
      in {
        packages.default = rustPlatform.buildRustPackage {
          pname = "kettle";
          # Keep in lockstep with `Cargo.toml`'s workspace `version`.
          # Bump in the same PR that bumps the release tag.
          version = "2.3.1";
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            # `makeWrapper` provides `wrapProgram` used in postFixup
            # below to inject the runtime library search path.
            makeWrapper
          ];

          buildInputs = runtimeLibs;

          # wgpu picks the Vulkan / GL / Wayland icd at runtime via
          # dlopen — Nix needs the runtime search path patched in
          # explicitly, otherwise `nix run` fails with a confusing
          # "failed to create wgpu instance: NoAdapterFound" on
          # systems where the loader isn't otherwise on
          # LD_LIBRARY_PATH.
          postFixup = ''
            patchelf \
              --set-rpath "${pkgs.lib.makeLibraryPath runtimeLibs}" \
              $out/bin/kettle
          '';

          # Skip the offscreen GPU self-test during `nix build` —
          # the Nix sandbox has no Vulkan-capable GPU, the cycle-205
          # offscreen pipeline would fail. CI on a real runner still
          # exercises it.
          checkFlags = [
            "--skip=gpu_tests::gpu_pipelines_compile_and_render_offscreen"
            "--skip=context_menu_renders_visibly_with_text"
          ];

          meta = with pkgs.lib; {
            description = "Fast cross-platform GPU-accelerated terminal emulator";
            homepage = "https://github.com/Reddimus/kettle";
            license = licenses.mit;
            mainProgram = "kettle";
            platforms = platforms.unix;
          };
        };

        # `nix develop` drops you into a shell with everything needed
        # to build kettle from source — same toolchain + libs the
        # release uses. Useful for contributors who already have nix
        # but not Rust, or want a hermetic build env.
        devShells.default = pkgs.mkShell {
          buildInputs = runtimeLibs ++ [ rustToolchain pkgs.pkg-config ];
          # Plumb the runtime libs onto LD_LIBRARY_PATH so `cargo run`
          # inside the dev shell works the same as `nix run` does on
          # the built package.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
        };

        # Standard `nix flake check` entrypoint — runs `cargo test`
        # against the workspace MSRV. Excludes the GPU + visual
        # regression tests for the same reason `checkFlags` above
        # does. Add other lints (clippy, fmt) here when adopting
        # `nix-fast-build` style CI.
        checks.cargo-test = self.packages.${system}.default.overrideAttrs (old: {
          doCheck = true;
          checkFlags = old.checkFlags or [];
        });
      });
}
