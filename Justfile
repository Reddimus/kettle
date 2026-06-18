# kettle — task runner for common dev workflows.
#
# Install `just` from https://just.systems (Rust ecosystem standard):
#
#   cargo install just         # or `brew install just`, `apt install just`,
#                              # `winget install Casey.Just` on Windows
#
# Run `just` (no args) to see the recipe list; `just <recipe>` to run.
# Recipes intentionally mirror the CI gate so a green `just gauntlet`
# locally is the same gate `.github/workflows/ci.yml` runs on every PR.
#
# Cross-platform: cycle 730 rewrote the Justfile so every recipe runs
# on Windows PowerShell, Linux/macOS bash/zsh, and Git Bash on
# Windows. Two patterns:
#
#   - `export RUSTDOCFLAGS := "-D warnings"` below makes the env
#     var visible to all recipes without a shell-prefix (`FOO=bar
#     cmd` is bash-only; just's `export` is shell-agnostic).
#   - `[unix]` / `[windows]` recipe attributes platform-gate
#     install/uninstall/bench/menu-shot/clean so Windows users
#     get a graceful message + the Linux scripts stay shipped
#     as-is.

# Cycle 730: use Windows PowerShell (preinstalled on every Windows 10+
# machine) as the recipe shell on Windows. Just's default is `sh` which
# requires Git Bash on PATH — not a thing on a fresh Win11 install.
# `-NoLogo -Command` suppresses the PS startup banner and accepts a
# script body. All recipe bodies in this file are cargo / @echo /
# explicit cmdlets — all of which work in PowerShell 5.1+.
#
# Native-command exit codes: just halts the recipe if any single line
# returns non-zero (just's default --shell-arg flag is `-c` which
# preserves native exit codes), so we don't need `$ErrorActionPreference
# = 'Stop'` injection inside every recipe.
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# Cycle 730: surface rustdoc-lint denials to every recipe (doc,
# gauntlet) without a bash-only env-var prefix. Just exports this
# at recipe-entry as a real env var, working under bash, zsh,
# PowerShell, and cmd. Pre-730 the `doc` + `gauntlet` recipes ran
# `RUSTDOCFLAGS="-D warnings" cargo doc …` which broke under
# PowerShell (the inline `FOO=bar cmd` prefix is bash-only).
export RUSTDOCFLAGS := "-D warnings"

# Cycle 730: OS-appropriate temp dir for default screenshot output.
# Just has no `tempdir()` builtin (cache/data/config-directory yes;
# temp deliberately omitted since temp semantics vary across OSes).
# The `if os_family()` ternary picks `%TEMP%` on Windows (always
# set) and `/tmp` on Linux/macOS (always present), so the
# `screenshot` / `menu` recipes can default to a writable location
# everywhere. Pre-730 they defaulted to `/tmp/...` which doesn't
# exist on Windows.
TMPDIR := if os_family() == "windows" { env_var("TEMP") } else { "/tmp" }

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

# `cargo test --workspace` — runs all 432+ unit + integration tests
# (post cycle 730; baseline was 424 + 8 new kettle-remote BFS tests).
test:
    cargo test --workspace

# `cargo doc` with `-D warnings` — rustdoc has its own warning class
# (broken intra-doc-links, missing docs on public items) that
# `clippy -D warnings` doesn't catch. CI runs this on Linux only
# (rustdoc is platform-agnostic); same here. RUSTDOCFLAGS exported
# at the top.
doc:
    cargo doc --workspace --no-deps

# === Supply chain ==================================================

# `cargo deny check` — supply-chain gate covering advisories,
# duplicate-version bans, allowed licenses, and crates.io-only
# sources. CI runs this on every Cargo.lock change via
# `.github/workflows/deny.yml`. Local run mirrors CI exactly so
# a green `just deny` means the workflow won't catch new issues.
# Requires `cargo install cargo-deny` (one-time).
deny:
    cargo deny check

# `cargo machete` — finds unused workspace dependencies. CI runs
# this on every PR via `.github/workflows/machete.yml`. Local
# pre-flight before adding a `Cargo.toml` dep, since a forgotten
# leftover trips CI later. Requires `cargo install cargo-machete`.
machete:
    cargo machete

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
    cargo doc --workspace --no-deps
    @echo ""
    @echo "GAUNTLET PASSED — same gate CI runs on every PR. Safe to push."

# Strict gate: gauntlet + supply-chain hygiene (cycle 444 added the
# `cargo-deny` stale-ignore catch + cycle 274 added `cargo-machete`
# unused-deps catch as separate CI workflows triggered on Cargo.lock
# changes). Run `just gauntlet-strict` before a release-cut so all
# CI gates pass locally first. Requires `cargo install cargo-deny
# cargo-machete` (one-time).
gauntlet-strict: gauntlet deny machete
    @echo ""
    @echo "STRICT GAUNTLET PASSED — every CI workflow green locally."

# === End-to-end smoke ==============================================

# Render the canonical "kettle in a terminal" screenshot — exercises
# the full GPU pipeline (wgpu adapter + offscreen Vulkan device +
# glyphon text + quad + image pipelines + image::save PNG encode).
# Default OUT lands in the platform's temp dir (`/tmp` on Linux,
# `$env:TEMP` on Windows). Pass `OUT=path` to override.
#
# Cycle 730: switched to `cargo run` (cargo handles the `.exe`
# suffix automatically on Windows) and `TMPDIR` (OS-aware temp
# dir, set at the top of this Justfile) instead of hardcoded
# `/tmp/kettle.png`.
screenshot OUT=(TMPDIR / "kettle.png"):
    cargo run --release -p kettle -- --screenshot {{OUT}}
    @echo "wrote {{OUT}}"

# Render the synthetic right-click context menu — useful for visually
# verifying menu rendering changes. Pixel-level CI smoke
# (tests/menu_visual.rs) covers the regression class; this gives you
# a PNG to eyeball when tweaking colors / padding / etc.
menu OUT=(TMPDIR / "kettle-menu.png"):
    cargo run --release -p kettle -- --screenshot-menu {{OUT}}
    @echo "wrote {{OUT}}"

# Reproduce the docs/PERFORMANCE.md baseline. Runs each measurement
# 5× via `/usr/bin/time` (Linux/macOS) or `Measure-Command` (Win11).
#
# Cycle 730: platform-gated. The unix version calls scripts/bench.sh
# (cycle 260's GNU-time based harness); the Windows version calls
# scripts/bench.ps1 (cycle 730's Measure-Command based harness).
# `powershell.exe` is the Windows-PowerShell-5.1 binary preinstalled
# on every Windows 10+ machine; if `pwsh` (PS Core 7+) is also
# present it'd work too, but powershell.exe is the universal default.
[unix]
bench:
    ./scripts/bench.sh

[windows]
bench:
    & ./scripts/bench.ps1

# Compare Kettle against installed Linux peer terminals using Hyperfine:
# Terminator and Ghostty are required, Alacritty is included when present.
# This is desktop-local by design (needs a graphical Linux session) and gates
# the Ubuntu "better than Terminator, close to Ghostty" requirement with
# repeatable JSON output under target/perf-results/linux-local/.
[unix]
linux-perf:
    ./scripts/perf/linux-compare.sh

[windows]
linux-perf:
    @echo "linux-perf is a Linux desktop benchmark."
    @echo "On Windows use: pwsh -File scripts/perf/perf-all.ps1 -Label after"
    @echo "then:          pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after"

# === Install / uninstall ===========================================

# Drop a build under ~/.local/ (the cycle-0 install script — Linux
# only). Same path the cycle-253 `install-online.sh` curl|sh wrapper
# uses for online installs.
#
# Cycle 730 / 733: Linux uses scripts/install.sh (cycle-0 XDG
# installer); Windows now uses scripts/install.ps1 (per-user install
# to %LOCALAPPDATA%\Programs\kettle + Start menu shortcut + PATH
# update + Add/Remove Programs entry, no admin / UAC). Both run with
# no system-wide side effects; the install.ps1 script mirrors
# install.sh's shape and runs on PowerShell 5.1+ (built into Win10+).
[unix]
install:
    ./scripts/install.sh

[windows]
install:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/install.ps1

# Remove everything `just install` placed.
[unix]
uninstall:
    ./scripts/install.sh --uninstall

[windows]
uninstall:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/install.ps1 -Uninstall

# Build the current release binary AND (re)install it in one step, so the
# Start-menu / PATH / Windows-Search "kettle" launches THIS build. `just install`
# alone installs whatever is already in target/release/ (which may be stale or
# absent); this recipe rebuilds first, closing the "built but forgot to reinstall"
# gap. Run after any change you want reflected in the installed app — and after
# every release cut — to keep the installed app synced to the repo.
#
# Unix uses `--skip-build` after the `release` dependency so local deployment
# does not compile the same release binary twice. Windows install.ps1 already
# expects the release artifact built by the dependency.
[unix]
install-local: release
    ./scripts/install.sh --skip-build
    @echo "local install synced to the current release build"

[windows]
install-local: release
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/install.ps1
    @echo "local install synced to the current release build"

# === Misc ==========================================================

# Run kettle in a real window (Linux: needs X11 / Wayland; Windows:
# native; macOS: native). Useful when verifying interactive behavior
# the offscreen `--screenshot*` paths can't reach.
run:
    cargo run --release -p kettle

# Cycle 711: capture a screenshot of the right-click context menu via
# scrot + xdotool. Useful for visual regression of the C3-C9 context-
# menu sub-cycles. Output lands in `target/menu-shots/`. Pass
# `--name <slug>` to label the file; `--hold` to leave kettle running.
#
# Cycle 730: Linux-only by design — uses xdotool / scrot which only
# exist on X11. Windows / macOS can use the offscreen `just menu`
# recipe instead (cycle-251 visual regression pipeline covers the
# same regression class without needing a real desktop).
[unix]
menu-shot *ARGS:
    ./scripts/menu-screenshot.sh {{ARGS}}

[windows]
menu-shot *ARGS:
    @echo "menu-shot requires xdotool + scrot (Linux X11). On Windows,"
    @echo "use 'just menu' instead — it renders the same menu offscreen"
    @echo "via the cycle-251 visual-regression pipeline."

# Start a real kettle window with `text-renderer = grid`, capture live
# screenshots through `kettle ctl screenshot`, and assert cursor blink changes
# only a cursor-sized region. This is Linux desktop-local by design: it needs a
# visible X11/Wayland session and complements the CI offscreen renderer tests.
[unix]
live-render-smoke:
    ./scripts/check-live-render-smoke.sh

[windows]
live-render-smoke:
    @echo "live-render-smoke is currently a Unix desktop helper."
    @echo "Windows coverage comes from CI's windows-latest build/test/CLI smoke;"
    @echo "manual Windows live screenshot smoke can use 'kettle --agent-server full'"
    @echo "plus 'kettle ctl screenshot'."

# Reproduce and guard the multi-tab mouse-click visual state. Captures full
# window PNGs and tab geometry JSON under target/diagnostics/tabbar-click-*.
[unix]
tabbar-click-smoke:
    ./scripts/check-tabbar-click-smoke.sh

[windows]
tabbar-click-smoke:
    python scripts/check-live-ui-smoke.py tabbar

# Reproduce underline scrolling with git diff | delta under repeated j/k input.
# Captures PNG frames and read_cells JSON under target/diagnostics/underline-scroll-*.
[unix]
underline-scroll-smoke:
    ./scripts/check-underline-scroll-smoke.sh

[windows]
underline-scroll-smoke:
    python scripts/check-live-ui-smoke.py underline

# Clean every build artifact — `cargo clean` plus any temp PNGs
# the screenshot / menu / bench recipes may have left in the OS
# temp dir.
#
# Cycle 730: split into [unix] / [windows] because `rm` and `/tmp`
# are bash-only. cargo clean is the cross-platform core; the temp-
# PNG cleanup is OS-specific. The Windows variant delegates the
# delete to powershell.exe (Remove-Item is a cmdlet, not a binary)
# and `-ErrorAction SilentlyContinue` swallows the not-found case
# so re-running `just clean` after the first call doesn't fail.
[unix]
clean:
    cargo clean
    rm -f /tmp/kettle.png /tmp/kettle-menu.png /tmp/kettle-bench.png /tmp/kettle-bench-menu.png

[windows]
clean:
    cargo clean
    Remove-Item -ErrorAction SilentlyContinue $env:TEMP\kettle.png, $env:TEMP\kettle-menu.png, $env:TEMP\kettle-bench.png, $env:TEMP\kettle-bench-menu.png
