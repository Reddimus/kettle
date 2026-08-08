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
    flake-utils.lib.eachSystem [
      "aarch64-darwin"
      # Intel Macs. Omitting this made `nix build` simply unavailable on every
      # x86_64 Mac -- not degraded, absent -- while the release ships a
      # universal macOS binary that explicitly supports them.
      "x86_64-darwin"
      "aarch64-linux"
      "x86_64-linux"
    ] (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pin to the workspace MSRV (1.89) so a `nix build`
        # uses exactly the toolchain CI verifies on every PR. Drift-
        # proofs the Nix path against a nixpkgs Rust version bump.
        rustToolchain = pkgs.rust-bin.stable."1.89.0".default;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Complete Linux runtime closure for libraries the wgpu /
        # winit / glyphon stack loads dynamically. In particular,
        # winit's X11 backend opens Xcursor and XInput2 even when
        # those libraries were not linked directly. Darwin uses
        # system frameworks supplied by its stdenv instead.
        runtimeLibs = pkgs.lib.optionals pkgs.stdenv.isLinux (with pkgs; [
          fontconfig
          freetype
          libGL
          libxkbcommon
          libxkbfile
          libX11
          libxcb
          libxcursor
          libxi
          vulkan-loader
          wayland
        ]);

        # Prove the installed binary can create a visible X11 window
        # using only its patched RUNPATH. This catches missing
        # dynamically-loaded libraries that compile-time tests do not
        # exercise (for example Xcursor and XInput2).
        runtimeSmokeScript = pkgs.writeShellScript "kettle-x11-runtime-smoke" ''
          set -eu

          # `xvfb-run` exports DISPLAY and execs this script, but Xvfb is not
          # necessarily accepting connections yet. Launching straight into that
          # window makes kettle exit 1 with "Failed to open connection to X
          # server", which this check then reports as a kettle failure — an
          # infrastructure race wearing a product bug's clothes. Wait for the
          # server, and fail with a message that names the real culprit.
          attempt=0
          until ${pkgs.xdotool}/bin/xdotool getdisplaygeometry >/dev/null 2>&1; do
            attempt=$((attempt + 1))
            if [ "$attempt" -ge 100 ]; then
              echo "Xvfb did not accept a connection on ''${DISPLAY:-unset} within 10 seconds" >&2
              exit 1
            fi
            sleep 0.1
          done

          log="$TMPDIR/kettle-runtime-smoke.log"
          "${self.packages.${system}.default}/bin/kettle" \
            --config "$KETTLE_SMOKE_CONFIG" \
            --new-process >"$log" 2>&1 &
          kettle_pid=$!

          cleanup() {
            kill "$kettle_pid" 2>/dev/null || true
            wait "$kettle_pid" 2>/dev/null || true
          }
          trap cleanup EXIT HUP INT TERM

          attempt=0
          while [ "$attempt" -lt 200 ]; do
            if ! kill -0 "$kettle_pid" 2>/dev/null; then
              wait "$kettle_pid" || status=$?
              cat "$log" >&2
              echo "kettle exited before creating an X11 window (status ''${status:-0})" >&2
              exit 1
            fi

            if ${pkgs.xdotool}/bin/xdotool search \
              --onlyvisible --pid "$kettle_pid" >/dev/null 2>&1; then
              exit 0
            fi

            sleep 0.1
            attempt=$((attempt + 1))
          done

          cat "$log" >&2
          echo "kettle did not create a visible X11 window within 20 seconds" >&2
          exit 1
        '';
      in {
        packages.default = rustPlatform.buildRustPackage {
          pname = "kettle";
          # Keep in lockstep with `Cargo.toml`'s workspace `version`.
          # Bump in the same PR that bumps the release tag.
          version = "2.54.0";
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = [ pkgs.pkg-config ]
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.patchelf ];

          # Process-tree tests invoke these tools by name from inside
          # a PTY. Nix's check sandbox does not inherit the host PATH,
          # so declare every fixture command explicitly.
          nativeCheckInputs = [ (pkgs.lib.getBin pkgs.bash) pkgs.coreutils ]
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
              (pkgs.lib.getBin pkgs.util-linux)
            ];

          # Nix deliberately supplies a non-existent HOME. portable-pty
          # resolves an unset command cwd through HOME before spawning, so
          # give the tests an existing sandbox directory.
          preCheck = ''
            export HOME="$TMPDIR"
          '';

          buildInputs = runtimeLibs;

          # Keep the Nix Linux package at feature parity with Kettle's other
          # Linux distribution channels. These assets are intentionally
          # omitted from Darwin outputs, where a Linux Desktop Entry and
          # hicolor icon hierarchy would be inert and misleading.
          postInstall = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            install -Dm644 packaging/linux/kettle.desktop \
              "$out/share/applications/kettle.desktop"
            install -Dm644 packaging/linux/kettle.svg \
              "$out/share/icons/hicolor/scalable/apps/kettle.svg"

            for size in 16 24 32 48 64 128 256; do
              install -Dm644 "packaging/linux/kettle-$size.png" \
                "$out/share/icons/hicolor/''${size}x''${size}/apps/kettle.png"
            done

            install -Dm644 packaging/linux/kettle.1 \
              "$out/share/man/man1/kettle.1"

            for shell in bash zsh fish ps1; do
              install -Dm644 "shell-integration/kettle.$shell" \
                "$out/share/kettle/shell-integration/kettle.$shell"
            done
          '';

          # wgpu picks the Vulkan / GL / Wayland icd at runtime via
          # dlopen — Nix needs the runtime search path patched in
          # explicitly, otherwise `nix run` fails with a confusing
          # "failed to create wgpu instance: NoAdapterFound" on
          # systems where the loader isn't otherwise on
          # LD_LIBRARY_PATH.
          postFixup = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            patchelf \
              --add-rpath "${pkgs.lib.makeLibraryPath runtimeLibs}" \
              $out/bin/kettle

            final_rpath="$(patchelf --print-rpath $out/bin/kettle)"
            for required in \
              "${pkgs.stdenv.cc.libc}/lib" \
              "${pkgs.stdenv.cc.cc.lib}/lib"
            do
              case ":$final_rpath:" in
                *":$required:"*) ;;
                *)
                  echo "kettle RUNPATH lost required stdenv path: $required" >&2
                  exit 1
                  ;;
              esac
            done
          '';

          # Linux Nix sandboxes map the filesystem root to overflow uid 65534
          # while running the builder as uid 1000. Kettle deliberately rejects
          # every private path below a root owned by neither uid 0 nor the
          # effective user, so positive private-file tests cannot execute
          # faithfully inside the derivation sandbox. Run only
          # root-identity-independent crates here. The native CI matrix
          # executes the complete workspace on Linux, macOS, and Windows, and
          # the MSRV job repeats it on Rust 1.89. Do not add individual skips:
          # later private-state, recording, IPC, and update tests share the
          # same impossible premise, while negative tests could pass for the
          # wrong early-rejection reason.
          cargoTestFlags = [
            "--package=kettle-vt"
            "--package=kettle-remote"
          ];

          # Skip the offscreen GPU and visual-regression tests during
          # `nix build`: the sandbox has no Vulkan-capable GPU. CI on a real
          # runner still exercises them.
          checkFlags = [
            "--skip=gpu_tests::gpu_pipelines_compile_and_render_offscreen"
            "--skip=context_menu_renders_visibly_with_text"
            "--skip=compact_scrollbar_is_visible_contrasting_and_edge_scoped"
          ];

          meta = with pkgs.lib; {
            description = "Fast cross-platform GPU-accelerated terminal emulator";
            homepage = "https://github.com/Reddimus/kettle";
            license = licenses.mit;
            mainProgram = "kettle";
            platforms = [ system ];
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
        # does. Linux additionally launches the installed binary
        # under Xvfb without LD_LIBRARY_PATH so its RUNPATH is tested.
        checks = {
          cargo-test = self.packages.${system}.default;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          package-contents = pkgs.runCommand "kettle-linux-package-contents" {
            nativeBuildInputs = [
              pkgs.coreutils
              pkgs.diffutils
              pkgs.findutils
              pkgs.gzip
            ];
          } ''
            package="${self.packages.${system}.default}"

            assert_packaged_file() {
              source_file="$1"
              installed_file="$2"

              test -f "$installed_file"
              test ! -L "$installed_file"
              # Nix removes write bits when it registers a store path.
              test "$(stat -c '%a' "$installed_file")" = 444
              cmp "$source_file" "$installed_file"
            }

            assert_packaged_file \
              "${./packaging/linux/kettle.desktop}" \
              "$package/share/applications/kettle.desktop"
            assert_packaged_file \
              "${./packaging/linux/kettle.svg}" \
              "$package/share/icons/hicolor/scalable/apps/kettle.svg"

            for size in 16 24 32 48 64 128 256; do
              assert_packaged_file \
                "${./packaging/linux}/kettle-$size.png" \
                "$package/share/icons/hicolor/''${size}x''${size}/apps/kettle.png"
            done

            # nixpkgs' compressManPages hook gzips installed man pages, so both
            # the packaged name and its bytes differ from the checked-in source
            # and `assert_packaged_file` cannot be used here. Compare the
            # decompressed stream instead of disabling compression: gzipped man
            # pages are the Linux norm and `man` reads them transparently, so
            # turning that off would ship a nonstandard package to suit a check.
            installed_man="$package/share/man/man1/kettle.1.gz"
            test -f "$installed_man"
            test ! -L "$installed_man"
            test "$(stat -c '%a' "$installed_man")" = 444
            gzip -dc "$installed_man" | cmp - "${./packaging/linux/kettle.1}"

            for shell in bash zsh fish ps1; do
              assert_packaged_file \
                "${./shell-integration}/kettle.$shell" \
                "$package/share/kettle/shell-integration/kettle.$shell"
            done

            # The share tree is wholly owned by the declarations above. Count
            # its directories and files as well as comparing every file so
            # additions cannot silently bypass a conscious package-content
            # policy update. Reject links and special nodes explicitly.
            test -z "$(
              find "$package/share" \
                -mindepth 1 ! -type d ! -type f -print -quit
            )"
            test "$(find "$package/share" -mindepth 1 -type d | wc -l)" -eq 23
            test "$(find "$package/share" -type f | wc -l)" -eq 14

            touch "$out"
          '';

          runtime-smoke = pkgs.runCommand "kettle-x11-runtime-smoke" {
            nativeBuildInputs = [ pkgs.coreutils pkgs.xvfb-run ];
          } ''
            export HOME="$TMPDIR/home"
            export XDG_CACHE_HOME="$TMPDIR/cache"
            export XDG_CONFIG_HOME="$TMPDIR/config"
            export XDG_DATA_HOME="$TMPDIR/data"
            export XDG_RUNTIME_DIR="$TMPDIR/runtime"
            export SHELL="${pkgs.bash}/bin/bash"
            export KETTLE_SMOKE_CONFIG="$TMPDIR/kettle.config"
            unset LD_LIBRARY_PATH WAYLAND_DISPLAY

            mkdir -p \
              "$HOME" \
              "$XDG_CACHE_HOME" \
              "$XDG_CONFIG_HOME" \
              "$XDG_DATA_HOME" \
              "$XDG_RUNTIME_DIR"
            chmod 0700 "$XDG_RUNTIME_DIR"

            # Wgpu's GL backend cannot create an EGL surface on every Xvfb
            # build. Use Mesa's CPU Vulkan implementation so this remains a
            # deterministic no-GPU launch while still exercising a real wgpu
            # surface. Nix names the ICD for the package architecture.
            set -- "${pkgs.mesa}"/share/vulkan/icd.d/lvp_icd.*.json
            test "$#" -eq 1
            test -f "$1"
            export VK_DRIVER_FILES="$1"

            printf '%s\n' \
              "shell = ${pkgs.bash}/bin/bash" \
              "gpu-backend = vulkan" \
              "gpu-force-software = true" \
              "restore-session = false" \
              "update-policy = off" \
              "window-title-format = kettle-nix-runtime-smoke" \
              > "$KETTLE_SMOKE_CONFIG"

            ${pkgs.xvfb-run}/bin/xvfb-run \
              -a -s "-screen 0 1280x800x24 -nolisten tcp" \
              ${runtimeSmokeScript}
            touch "$out"
          '';
        };
      });
}
