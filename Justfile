# kettle — task runner for common dev workflows.
#
# Install `just` from https://just.systems (Rust ecosystem standard):
#
#   cargo install just         # or `brew install just`, `apt install just`
#
# Run `just` (no args) to see the recipe list; `just <recipe>` to run.
# Recipes intentionally mirror the CI gate so a green `just gauntlet`
# locally is the same gate `.github/workflows/ci.yml` runs on every PR.

# Default recipe: show the list when `just` is invoked with no args.
default:
    @just --list

# === Daily dev loop ================================================

# Format every crate's source in place (matches `cargo fmt` defaults
# enforced by .editorconfig + the CI fmt --check step).
fmt:
    cargo fmt --all

# `clippy -D warnings` across the whole workspace, all targets
# (lib + bins + tests + benches + examples). Same flag set the CI
# `ci.yml` `cargo clippy` step uses.
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# `cargo test --workspace` — runs all 261+ unit + integration tests
# including the cycle-251 visual-regression menu_visual.rs.
test:
    cargo test --workspace

# `cargo doc` with `-D warnings` — rustdoc has its own warning class
# (broken intra-doc-links, missing docs on public items) that
# `clippy -D warnings` doesn't catch. CI runs this on Linux only
# (rustdoc is platform-agnostic); same here.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# === Builds ========================================================

# Dev build (fast incremental) — what `cargo build` would do anyway,
# scoped to the workspace.
build:
    cargo build --workspace --all-targets

# Release build of just the binary crate — what gets shipped in the
# release tarballs. Use this to test the same artifact CI would build.
release:
    cargo build --release -p kettle

# === Verification gauntlet =========================================

# The full CI-equivalent gate. Run this before every PR — a green
# `just gauntlet` is what the GitHub Actions matrix runs on every
# OS. Use this if you're about to push a feature; trust this if
# it's green.
gauntlet:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo build --workspace --all-targets
    cargo test --workspace
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    @echo ""
    @echo "GAUNTLET PASSED — same gate CI runs on every PR. Safe to push."

# === End-to-end smoke ==============================================

# Render the canonical "kettle in a terminal" screenshot — exercises
# the full GPU pipeline (wgpu adapter + offscreen Vulkan device +
# glyphon text + quad + image pipelines + image::save PNG encode).
# Output at /tmp/kettle.png; pass `OUT=path` to override.
screenshot OUT="/tmp/kettle.png":
    cargo build --release -p kettle
    ./target/release/kettle --screenshot {{OUT}}
    @echo "wrote {{OUT}}"

# Render the synthetic right-click context menu — useful for visually
# verifying menu rendering changes. Pixel-level CI smoke
# (tests/menu_visual.rs) covers the regression class; this gives you
# a PNG to eyeball when tweaking colors / padding / etc.
menu OUT="/tmp/kettle-menu.png":
    cargo build --release -p kettle
    ./target/release/kettle --screenshot-menu {{OUT}}
    @echo "wrote {{OUT}}"

# Reproduce the docs/PERFORMANCE.md baseline (cycle 260). Runs each
# measurement 5× via /usr/bin/time + scripts/bench.sh.
bench:
    ./scripts/bench.sh

# === Install / uninstall ===========================================

# Drop a build under ~/.local/ (the cycle-0 install script — Linux
# only). Same path the cycle-253 `install-online.sh` curl|sh wrapper
# uses for online installs.
install:
    ./scripts/install.sh

# Remove everything `just install` placed.
uninstall:
    ./scripts/install.sh --uninstall

# === Misc ==========================================================

# Run kettle in a real window (Linux: needs X11 / Wayland). Useful
# when verifying interactive behavior the offscreen `--screenshot*`
# paths can't reach.
run:
    cargo run --release -p kettle

# Clean every build artifact — `cargo clean` plus the bench /
# screenshot output PNGs that may have leaked into /tmp.
clean:
    cargo clean
    rm -f /tmp/kettle.png /tmp/kettle-menu.png /tmp/kettle-bench.png /tmp/kettle-bench-menu.png
