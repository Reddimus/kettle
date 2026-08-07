# kettle — task runner for common dev workflows.
#
# Install `just` from https://just.systems (Rust ecosystem standard):
#
#   cargo install just         # or `brew install just`, `apt install just`,
#                              # `winget install Casey.Just` on Windows
#
# Run `just` (no args) to see the recipe list; `just <recipe>` to run.
# `just gauntlet` mirrors the CI matrix job's *core Rust gate*
# (fmt/clippy/build/test/doc) on every OS — the fast loop to run before
# every commit. It deliberately does NOT cover the packaging/installer/
# update-manifest/GPU-render checks `.github/workflows/ci.yml` also runs
# (those need a release build, platform-specific tooling, or a GPU
# adapter that isn't always available locally). `just gauntlet-full`
# adds every one of those CI-only checks so a green `gauntlet-full` is
# the closest local match to "every ci.yml step passed" — run it before
# a release cut or before touching packaging/*, scripts/install*,
# scripts/*manifest*.{py,ps1}, or the renderer, not just before a
# routine commit.
#
# Cross-platform: every recipe runs on Windows PowerShell, Linux/macOS
# bash/zsh, and Git Bash on Windows. Two patterns:
#
#   - `export RUSTDOCFLAGS := "-D warnings"` below makes the env
#     var visible to all recipes without a shell-prefix (`FOO=bar
#     cmd` is bash-only; just's `export` is shell-agnostic).
#   - `[unix]` / `[windows]` recipe attributes platform-gate
#     install/uninstall/bench/menu-shot/clean/every CI-parity smoke
#     added below, so Windows users get a graceful message on a
#     Linux/macOS-only check (and vice versa) instead of a confusing
#     failure.

# Use Windows PowerShell (preinstalled on every Windows 10+
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

# Surface rustdoc-lint denials to every recipe (doc,
# gauntlet) without a bash-only env-var prefix. Just exports this
# at recipe-entry as a real env var, working under bash, zsh,
# PowerShell, and cmd. Previously the `doc` + `gauntlet` recipes ran
# `RUSTDOCFLAGS="-D warnings" cargo doc …` which broke under
# PowerShell (the inline `FOO=bar cmd` prefix is bash-only).
export RUSTDOCFLAGS := "-D warnings"

# OS-appropriate temp dir for default screenshot output.
# Just has no `tempdir()` builtin (cache/data/config-directory yes;
# temp deliberately omitted since temp semantics vary across OSes).
# The `if os_family()` ternary picks `%TEMP%` on Windows (always
# set) and `/tmp` on Linux/macOS (always present), so the
# `screenshot` / `menu` recipes can default to a writable location
# everywhere. They used to default to `/tmp/...` which doesn't
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

# Build `kettle-core` on its own with DEFAULT features.
#
# A workspace build cannot see this: kettle-ui and the bin crate both enable
# `asciicast`, so any accidental dependence on an optional dependency compiles
# there and fails only for someone building the crate alone. That is exactly
# how session logging came to use `kettle-state` while it was still optional.
# Feature unification makes `--workspace` structurally unable to catch it, so
# the check has to name the crate.
core-default-features-check:
    cargo clippy -p kettle-core --all-targets -- -D warnings

# `cargo test --workspace` — runs the complete workspace unit and integration
# suite. Use the command's summary for the current count.
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

# `cargo deny check` — supply-chain gate covering duplicate-version bans,
# allowed licenses, and crates.io-only sources in both the product and direct
# vendor-test lock graphs. CI runs this on every Cargo.lock change via
# `.github/workflows/deny.yml`. Local runs mirror CI exactly so a green
# `just deny` means the workflow will not catch a new policy issue.
# Requires `cargo install cargo-deny` (one-time).
deny:
    cargo deny check licenses sources bans
    cargo deny --manifest-path vendor/Cargo.toml check licenses sources bans

# RustSec advisory coverage for both committed lock graphs. The product graph
# retains one narrowly guarded unmaintained-crate exception; the vendor graph
# has no product exceptions. Missing cargo-audit is a hard command failure.
audit:
    cargo audit --db target/advisory-db --url https://github.com/RustSec/advisory-db.git --ignore RUSTSEC-2026-0192
    cargo audit --db target/advisory-db --url https://github.com/RustSec/advisory-db.git --file vendor/Cargo.lock

# `cargo machete` — finds unused workspace dependencies. CI runs
# this on every PR via `.github/workflows/machete.yml`. Local
# pre-flight before adding a `Cargo.toml` dep, since a forgotten
# leftover trips CI later. Requires `cargo install cargo-machete`.
machete:
    cargo machete

# Run every retained unit target, doctest, and warnings-denied clippy target
# owned by the patched parser crates. These crates are deliberately outside the
# product workspace, so `cargo test --workspace` cannot cover them. The
# committed vendor validation lock keeps this direct test graph reproducible.
vendor-parser-check:
    cargo test --locked --manifest-path vendor/Cargo.toml --target-dir target/vendor-check -p vte --features ansi
    cargo clippy --locked --manifest-path vendor/Cargo.toml --target-dir target/vendor-check -p vte --all-targets --features ansi -- -D warnings
    cargo test --locked --manifest-path vendor/Cargo.toml --target-dir target/vendor-check -p alacritty_terminal
    cargo clippy --locked --manifest-path vendor/Cargo.toml --target-dir target/vendor-check -p alacritty_terminal --all-targets -- -D warnings

# Exercise the patched portable-pty package on the current native platform.
# Run this on both Unix and Windows: only Windows compiles and executes the
# PIPE_NOWAIT/ConPTY regression, while Linux covers the retained Unix package
# surface. CI runs both native legs.
vendor-pty-check:
    cargo test --locked --manifest-path vendor/Cargo.toml --target-dir target/vendor-check -p portable-pty
    cargo clippy --locked --manifest-path vendor/Cargo.toml --target-dir target/vendor-check -p portable-pty --all-targets -- -D warnings

# Complete direct validation for all patched crates on the current OS.
vendor-check: vendor-parser-check vendor-pty-check

# Audit every Git-tracked path: index/worktree object identity, path/case
# uniqueness, UTF-8/LF hygiene, manifests, Markdown links, and binary font/image
# bounds. The JSON ledger is written under ignored target/diagnostics.
[unix]
tracked-audit:
    python3 scripts/audit-tracked-files.py --output target/diagnostics/tracked-files-audit.json

[windows]
tracked-audit:
    python scripts/audit-tracked-files.py --output target/diagnostics/tracked-files-audit.json

# Guard the temporary RUSTSEC-2026-0192 ignore. This must pass while #36 is
# open, and should print the "remove ignores" instruction once upstream makes
# `ttf-parser` disappear from the tree.
[unix]
ttf-parser-scope:
    python3 scripts/check-ttf-parser-scope.py

[windows]
ttf-parser-scope:
    python scripts/check-ttf-parser-scope.py

# === Packaging & release metadata ==================================
# These four wrap CI checks that used to have NO `just` entry point at
# all — a contributor could only discover them by reading ci.yml, run
# `just gauntlet` clean, and still get an unrelated CI failure on a
# packaging-only change. Folded into `gauntlet-full` above.

# Validate the Homebrew/AUR package templates and their renderer
# (scripts/render-package-templates.py). At an exact clean release tag,
# default `--auto` mode also checks the published assets; feature
# branches validate source rendering without comparing against an older
# tag that happens to share the current Cargo version. Mirrors CI's
# Linux-only "Package template lockstep" step; the script is portable
# Bash 3.2 + Python 3 and also runs unmodified on macOS.
[unix]
package-templates:
    ./scripts/check-package-templates.sh

[windows]
package-templates:
    @echo "package-templates needs bash (scripts/check-package-templates.sh)."
    @echo "Run it under Git Bash/WSL, or trust CI's Linux leg."

# Hermetic unit tests for scripts/make-update-manifest.py (the signed
# update-manifest generator). Mirrors CI's Linux-only "Signed update
# manifest generator" step.
[unix]
update-manifest-test:
    python3 scripts/test-update-manifest.py

[windows]
update-manifest-test:
    python scripts/test-update-manifest.py

# Validate the exact GitHub draft-release shape and local size/SHA-256 binding
# used by the token-only publisher job.
[unix]
release-assets-test:
    python3 scripts/test-verify-release-assets.py

[windows]
release-assets-test:
    python scripts/test-verify-release-assets.py

# Hermetic unit tests for scripts/package-manifest.py (the inner
# release-tarball manifest generator/verifier). Mirrors CI's
# Linux-only "Inner package manifest generator and verifier" step.
[unix]
package-manifest-test:
    python3 scripts/test-package-manifest.py

[windows]
package-manifest-test:
    python scripts/test-package-manifest.py

# Hermetic Linux/POSIX tests for install-online.sh. A private fake curl serves
# authenticated fixtures so the safe archive path, modern no-downgrade policy,
# sidecar parser, and malicious tar rejection run without network access.
[unix]
online-installer-test:
    python3 scripts/test-install-online.py

[windows]
online-installer-test:
    @echo "online-installer-test exercises the Linux/POSIX one-line installer."
    @echo "Run it under WSL, or trust the Linux CI leg."

# Rebuild packaging/macos/kettle.iconset into a .icns via the same
# `iconutil` CI uses, and sanity-check the result isn't a malformed or
# empty container (the same regression class the release.yml iconutil
# step could otherwise only catch at tag-cut time). `iconutil` ships
# with macOS only — there's no Linux/Windows equivalent. The macOS recipe
# fails if the required tool is missing; other OS recipes only classify it as
# inapplicable and are not dependencies of their full gate. Mirrors CI's
# "Packaging smoke — macOS .icns" step. Output lands under
# `{{TMPDIR}}` like the `screenshot`/`menu` recipes.
[macos]
icns-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v iconutil >/dev/null 2>&1 \
      || { echo "error: icns-smoke requires iconutil on macOS" >&2; exit 1; }
    out="{{TMPDIR}}/kettle-icns-smoke.icns"
    iconutil -c icns packaging/macos/kettle.iconset -o "$out"
    file "$out" | grep -q "Mac OS X icon" \
      || { echo "iconutil produced a non-icns file"; file "$out"; exit 1; }
    # Real one is ~300 KB; floor at 100 KB catches an empty container.
    size=$(stat -f%z "$out")
    test "$size" -gt 100000 \
      || { echo "icns too small ($size bytes) — iconutil likely produced an empty container"; exit 1; }
    echo "iconutil OK ($size bytes)"

[linux]
icns-smoke:
    @echo "icns-smoke requires macOS iconutil and is not a Linux gate."

[windows]
icns-smoke:
    @echo "icns-smoke needs iconutil (macOS-only); not applicable on Windows."
    @echo "See 'just ico-smoke' for the Windows .ico equivalent."

# Verify packaging/windows/kettle.ico parses as a well-formed,
# multi-resolution Windows icon. The check parses the ICONDIR header
# natively (no `file`/`xxd` dependency, unlike ci.yml's bash version) so
# it needs nothing beyond PowerShell 5.1+. Mirrors CI's Windows-only
# "Packaging smoke — Windows .ico" step and shares its >= 4 floor.
#
# It lives in scripts/ rather than inline: a plain `just` recipe runs
# each line in a separate shell, so the inline version this replaces
# lost `$path` before the line that read it and could never pass.
[windows]
ico-smoke:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/check-windows-ico.ps1

[unix]
ico-smoke:
    @echo "ico-smoke checks packaging/windows/kettle.ico; not applicable off Windows."
    @echo "See 'just icns-smoke' for the macOS .icns equivalent."

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

# Execute the shipped snippets rather than merely checking that their source
# text is non-empty. macOS supplies the stock zsh configuration and Bash 3.2
# needed for those regressions; Windows runs each installed PowerShell host.
[unix]
shell-integration-check:
    python3 scripts/check-shell-integration.py

[windows]
shell-integration-check:
    python scripts/check-shell-integration.py

# The CI matrix job's core Rust gate (fmt/clippy/build/test/doc) plus
# the live-UI-helper and native shell-integration fixtures. This is the fast
# pre-commit loop; run
# it before every commit. It does NOT cover the packaging/installer/
# update-manifest/GPU-render checks ci.yml also runs — see
# `gauntlet-full` below for those.
gauntlet: live-ui-helper-selftest shell-integration-check
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    # Feature unification hides a crate that leans on an optional dependency,
    # so the workspace lint above structurally cannot catch it.
    cargo clippy --locked -p kettle-core --all-targets -- -D warnings
    cargo build --locked --workspace --all-targets
    cargo test --locked --workspace
    cargo doc --locked --workspace --no-deps
    @echo ""
    @echo "GAUNTLET PASSED — core Rust gate green. Run 'just gauntlet-full' for required current-OS native gates."

# Strict gate: gauntlet + direct patched-crate validation + supply-chain
# hygiene (adds the cargo-deny stale-ignore catch and cargo-machete
# unused-deps catch as separate CI workflows triggered on Cargo.lock
# changes). Run `just gauntlet-strict` before a release cut so all CI gates
# pass locally first. Requires cargo-audit, cargo-deny, and cargo-machete
# (one-time). The current-OS vendor check is supplemented by Linux + Windows
# native vendor legs in CI.
gauntlet-strict: gauntlet vendor-check deny audit ttf-parser-scope machete tracked-audit
    @echo ""
    @echo "STRICT GAUNTLET PASSED — core, patched crates, RustSec product/vendor audits, ttf-parser scope, deny, machete, and tracked-file audit are green."

# The FULL CI-equivalent gate: gauntlet-strict plus every packaging,
# installer, update-manifest, and GPU-render check ci.yml runs that
# `gauntlet`/`gauntlet-strict` don't touch. Every dependency below is
# [unix]/[windows]-gated (see each recipe's own comment for exactly
# what it covers and on which OS it's a real check vs. an informational
# stub), so this either exercises the full ci.yml surface reachable on
# the current OS, or tells you plainly what it couldn't run here. Needs
# a release build (several dependencies exercise target/release/kettle);
# `release` runs once and is shared across every recipe that needs it.
# Run this before a release cut, or before any change to packaging/*,
# scripts/install*, scripts/*manifest*.{py,ps1}, or the renderer —
# `gauntlet`/`gauntlet-strict` alone won't catch a regression there. The
# platform-specific dependency set contains no successful stubs: every listed
# dependency performs a real check, and missing required tooling fails.
gauntlet-full: gauntlet-strict full-native-gates
    @echo ""
    @echo "CURRENT-OS FULL GAUNTLET PASSED — every required native gate listed above is green."
    @echo "This is not a PASS for native legs on other operating systems."

[windows]
full-native-gates: update-manifest-test release-assets-test package-manifest-test ico-smoke windows-installer-smoke gpu-render-smoke cli-smoke touchpad-scroll-smoke perf-self-test
    @echo "NOT APPLICABLE on Windows: Linux installer/online/package-template/headless-Xvfb and macOS iconutil gates."

[linux]
full-native-gates: package-templates update-manifest-test release-assets-test package-manifest-test online-installer-test linux-installer-smoke headless-gpu-smoke gpu-render-smoke cli-smoke touchpad-scroll-smoke
    @echo "NOT APPLICABLE on Linux: Windows installer/ICO/performance-matrix and macOS iconutil gates."

[macos]
full-native-gates: package-templates update-manifest-test release-assets-test package-manifest-test online-installer-test icns-smoke gpu-render-smoke cli-smoke touchpad-scroll-smoke
    @echo "NOT APPLICABLE on macOS: Windows installer/ICO/performance-matrix and Linux installer/Xvfb gates."

# === End-to-end smoke ==============================================

# Headless parser/command-generation guards for the cross-platform live UI
# harness. In particular, this rejects authenticated-agent false positives when
# a shell command fails but an old native exit code and echoed prompt remain.
[unix]
live-ui-helper-selftest:
    python3 scripts/check-live-ui-smoke.py self-test

[windows]
live-ui-helper-selftest:
    python scripts/check-live-ui-smoke.py self-test

# Render the canonical "kettle in a terminal" screenshot — exercises
# the full GPU pipeline (wgpu adapter + offscreen Vulkan device +
# glyphon text + quad + image pipelines + image::save PNG encode).
# Default OUT lands in the platform's temp dir (`/tmp` on Linux,
# `$env:TEMP` on Windows). Pass `OUT=path` to override.
#
# Uses `cargo run` (cargo handles the `.exe`
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

# Run a real windowed kettle under Xvfb for a few seconds and assert it
# neither panics nor exits with an unexpected code. `xvfb-run` is
# Linux/X11-only (no macOS/Windows equivalent), so this self-skips with
# a clear message elsewhere rather than failing on missing tooling.
# Mirrors CI's Linux-only "Headless GPU smoke" step. Needs a release
# binary.
[linux]
headless-gpu-smoke: release
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v xvfb-run >/dev/null 2>&1; then
      echo "error: headless-gpu-smoke requires xvfb-run on Linux" >&2
      exit 1
    fi
    export LIBGL_ALWAYS_SOFTWARE=1
    log="{{TMPDIR}}/kettle-headless-smoke.log"
    # The `bash -c '…$rc…'` invocation is single-quoted on purpose so
    # the inner `$rc` expands inside the *nested* bash, not this one.
    xvfb-run -a bash -c 'timeout 10 ./target/release/kettle >'"$log"' 2>&1; rc=$?; \
      grep -qiE "panic|thread .* panicked" '"$log"' && { echo "panic in run"; cat '"$log"'; exit 1; }; \
      test $rc -eq 124 -o $rc -eq 0 || { echo "bad exit $rc"; cat '"$log"'; exit 1; }; \
      echo "headless run OK (exit $rc)"'

[macos]
headless-gpu-smoke:
    @echo "headless-gpu-smoke requires Linux/Xvfb and is not a macOS gate."

[windows]
headless-gpu-smoke:
    @echo "headless-gpu-smoke needs Xvfb (Linux-only). Windows coverage comes from"
    @echo "CI's windows-latest build/test/CLI smoke; see 'just cli-smoke' locally."

# Bundle of ci.yml's Linux/macOS-only offscreen render smokes:
# `--gpu-info` (adapter resolution + key:value output shape),
# `--screenshot-menu` (the v1.3.0/v1.3.1 blank-menu regression class),
# and `--screenshot` (the full text+quad+image render + PNG encode
# path). Needs a release binary; LIBGL_ALWAYS_SOFTWARE is a harmless
# no-op outside Linux's software-Vulkan path. Mirrors CI's
# "--gpu-info diagnostic smoke", "--screenshot-menu visual regression",
# and "--screenshot end-to-end" steps (all `runner.os != 'Windows'`).
[unix]
gpu-render-smoke: release
    #!/usr/bin/env bash
    set -euo pipefail
    export LIBGL_ALWAYS_SOFTWARE=1
    out_dir="{{TMPDIR}}/kettle-gpu-render-smoke"
    mkdir -p "$out_dir"
    ./target/release/kettle --gpu-info | tee "$out_dir/gpu-info.txt"
    grep -qE '^Backend:[[:space:]]+' "$out_dir/gpu-info.txt"
    grep -qE '^Adapter:[[:space:]]+' "$out_dir/gpu-info.txt"
    grep -qE '^Max texture:[[:space:]]+[0-9]+ px / side$' "$out_dir/gpu-info.txt"
    ./target/release/kettle --screenshot-menu "$out_dir/kettle-menu.png"
    head -c 4 "$out_dir/kettle-menu.png" | xxd | grep -q "8950 4e47"
    # Floor at 40 KB — well above the byte-identical blank-menu
    # regression, well below the typical 55+ KB the real render outputs.
    size=$(wc -c < "$out_dir/kettle-menu.png")
    test "$size" -gt 40000 \
      || { echo "kettle-menu.png is too small ($size bytes) — likely the blank-menu regression"; exit 1; }
    echo "screenshot-menu OK ($size bytes)"
    ./target/release/kettle --screenshot "$out_dir/kettle-ci.png"
    head -c 4 "$out_dir/kettle-ci.png" | xxd | grep -q "8950 4e47"
    size=$(wc -c < "$out_dir/kettle-ci.png")
    test "$size" -gt 10000 \
      || { echo "kettle-ci.png is too small ($size bytes)"; exit 1; }
    echo "screenshot OK ($size bytes)"
    echo "gpu-render-smoke PASSED (gpu-info + screenshot-menu + screenshot)"

[windows]
gpu-render-smoke: release
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/check-gpu-render-smoke.ps1

# Faithful local mirror of ci.yml's "CLI smoke (all OSes)" step: drives
# every introspection flag (--version, --help, --check-config,
# --list-themes/--actions/--keybinds/--ssh-hosts, --print-default-config,
# --profile, --shell-integration, --print-completions, --config-path)
# and asserts both the happy-path output shape and the error-path exit
# codes. Self-builds the debug binary if missing, exactly like CI.
# Artifacts land under {{TMPDIR}}/kettle-cli-smoke instead of a CI
# runner's throwaway workspace. The CI script's `$RUNNER_OS` branch
# collapses to the non-Windows binary path — Git Bash on Windows
# resolves the extensionless name to `kettle.exe` on its own, so this
# also works if invoked under Git Bash, hence [unix] rather than a hard
# OS check.
[unix]
cli-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    out_dir="{{TMPDIR}}/kettle-cli-smoke"
    mkdir -p "$out_dir"
    KETTLE_CI_BIN="./target/debug/kettle"
    if [ ! -x "$KETTLE_CI_BIN" ]; then
      cargo build -q -p kettle
    fi
    # --version exercises the build.rs git-SHA capture.
    "$KETTLE_CI_BIN" --version | tee "$out_dir/kettle-version.txt"
    grep -E '^kettle [0-9]+\.[0-9]+\.[0-9]+ \([0-9a-f]+(\+dirty)?\)' "$out_dir/kettle-version.txt"
    # --help: pin clap's usage prelude plus the load-bearing flags.
    "$KETTLE_CI_BIN" --help > "$out_dir/kettle-help.txt"
    grep -qE '^Usage: kettle' "$out_dir/kettle-help.txt"
    grep -q -- '--config' "$out_dir/kettle-help.txt"
    grep -q -- '--screenshot' "$out_dir/kettle-help.txt"
    grep -q -- '--gpu-info' "$out_dir/kettle-help.txt"
    grep -q -- '--shell-integration' "$out_dir/kettle-help.txt"
    grep -q -- '--print-completions' "$out_dir/kettle-help.txt"
    grep -q -- '--print-default-config' "$out_dir/kettle-help.txt"
    # --check-config falls back to defaults + "status: OK" with no config.
    "$KETTLE_CI_BIN" --check-config | grep -E '^kettle:  [0-9]'
    "$KETTLE_CI_BIN" --check-config \
      | grep -E '^hint: +kettle --print-default-config > '
    "$KETTLE_CI_BIN" --config-path
    # --list-themes should always be 500+ entries (bundled iTerm2-Color-Schemes).
    "$KETTLE_CI_BIN" --list-themes > "$out_dir/themes.txt"
    test "$(wc -l < "$out_dir/themes.txt")" -gt 400
    # --list-actions: every action name + aliases.
    "$KETTLE_CI_BIN" --list-actions > "$out_dir/actions.txt"
    test "$(wc -l < "$out_dir/actions.txt")" -gt 50
    # --list-keybinds: the default Terminator-compatible chord set.
    "$KETTLE_CI_BIN" --list-keybinds > "$out_dir/keybinds.txt"
    test "$(wc -l < "$out_dir/keybinds.txt")" -gt 40
    # --list-ssh-hosts with none configured emits an explicit fallback line.
    "$KETTLE_CI_BIN" --list-ssh-hosts \
      | grep -E '^\(no ssh-host entries configured\)$'
    # --print-default-config emits the embedded example config; round-trip
    # it through --check-config.
    "$KETTLE_CI_BIN" --print-default-config > "$out_dir/k.cfg"
    test "$(wc -l < "$out_dir/k.cfg")" -gt 50
    "$KETTLE_CI_BIN" --config "$out_dir/k.cfg" --check-config \
      | grep -E '^status: +OK'
    # --profile NAME must be honored by every introspection flag, not
    # silently ignored in favor of the default path. Write a profile with
    # a deliberately malformed value under a scratch XDG_CONFIG_HOME
    # (never the user's real config dir) and assert --check-config's
    # exit code goes non-zero.
    export XDG_CONFIG_HOME="$out_dir/xdg-config"
    mkdir -p "$XDG_CONFIG_HOME/kettle/profiles"
    echo 'font-size = not_a_number' > "$XDG_CONFIG_HOME/kettle/profiles/cibad.config"
    out=$("$KETTLE_CI_BIN" --profile cibad --check-config 2>&1) && \
        { echo "--profile cibad --check-config exited 0 (should be non-zero on malformed font-size)"; \
          echo "$out"; \
          exit 1; }
    if echo "$out" | grep -q 'font-size'; then
        echo "--profile cibad --check-config surfaces malformed font-size"
    else
        echo "--profile cibad output missing 'font-size' diagnostic"
        echo "$out"
        exit 1
    fi
    # --shell-integration <shell>: every known shell emits a non-trivial
    # snippet containing the OSC 133 marker; an unknown shell errors.
    for sh in bash zsh fish powershell; do
      "$KETTLE_CI_BIN" --shell-integration "$sh" > "$out_dir/k.${sh}"
      grep -q "OSC 133" "$out_dir/k.${sh}"
      test "$(wc -l < "$out_dir/k.${sh}")" -gt 8
    done
    if "$KETTLE_CI_BIN" --shell-integration tcsh 2>/dev/null; then
      echo "expected --shell-integration tcsh to error"; exit 1
    fi
    # --print-completions <shell>: every known shell mentions `kettle`;
    # an unknown shell errors.
    for sh in bash zsh fish powershell; do
      "$KETTLE_CI_BIN" --print-completions "$sh" > "$out_dir/k.completions.${sh}"
      test "$(wc -c < "$out_dir/k.completions.${sh}")" -gt 200
      grep -q "kettle" "$out_dir/k.completions.${sh}"
    done
    if "$KETTLE_CI_BIN" --print-completions tcsh 2>/dev/null; then
      echo "expected --print-completions tcsh to error"; exit 1
    fi
    # A missing --config / --working-directory path must exit non-zero,
    # not silently fall back to defaults.
    typo="$out_dir/kettle-ci-definitely-no-such-path-$RANDOM"
    rm -rf "$typo"
    if "$KETTLE_CI_BIN" --config "$typo" --config-path 2>/dev/null; then
      echo "expected --config $typo to exit nonzero"; exit 1
    fi
    if "$KETTLE_CI_BIN" --working-directory "$typo" --config-path 2>/dev/null; then
      echo "expected --working-directory $typo to exit nonzero"; exit 1
    fi
    # Happy path: the bootstrap config round-trips through --config-path
    # (basename-only match, since path-separator style varies by OS).
    "$KETTLE_CI_BIN" --config "$out_dir/k.cfg" --config-path \
      | grep -qE 'k\.cfg$'
    rm -rf "$XDG_CONFIG_HOME"
    echo "cli-smoke PASSED"

[windows]
cli-smoke:
    python scripts/check-cli-smoke.py

# Reproduce the docs/PERFORMANCE.md baseline. Runs each measurement
# 5× via `/usr/bin/time` (Linux/macOS) or `Measure-Command` (Win11).
#
# Platform-gated. The unix version calls scripts/bench.sh
# (the GNU-time based harness); the Windows version calls
# scripts/bench.ps1 (the Measure-Command based harness).
# `powershell.exe` is the Windows-PowerShell-5.1 binary preinstalled
# on every Windows 10+ machine; if `pwsh` (PS Core 7+) is also
# present it'd work too, but powershell.exe is the universal default.
[unix]
bench:
    ./scripts/bench.sh

[windows]
bench:
    & ./scripts/bench.ps1

# GUI-free invariants for the Windows performance harness. CI requires both
# PowerShell runtimes, so the Windows full gate does too; a missing runtime is
# a hard failure instead of a successful skip.
[windows]
perf-self-test: perf-self-test-ps7 perf-self-test-ps5

[windows]
perf-self-test-ps7:
    pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/perf/self-test.ps1

[windows]
perf-self-test-ps5:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/perf/self-test.ps1

[unix]
perf-self-test:
    @echo "perf-self-test is the Windows PowerShell 7/5.1 CI matrix and is not a Unix gate."

# Compare Kettle against installed Linux peer terminals using Hyperfine.
# Terminator and Ghostty are required, Alacritty is included when present.
# This is desktop-local by design (needs a graphical Linux session) and gates
# the Ubuntu "better than Terminator, close to Ghostty" requirement across
# startup, ASCII flood, and SGR/underline flood timings, plus advisory RSS
# output under target/perf-results/linux-local/.
[unix]
linux-perf:
    ./scripts/perf/linux-compare.sh

[windows]
linux-perf:
    @echo "linux-perf is a Linux desktop benchmark."
    @echo "On Windows use: pwsh -File scripts/perf/perf-all.ps1 -Label after"
    @echo "then:          pwsh -File scripts/perf/score.ps1 -ResultsDir target/perf-results/after"

# Compare Kettle against installed macOS peer terminals using Hyperfine, native
# max-RSS accounting, quiet-window CPU samples, and a top-half rank gate. This
# is desktop-local by design and writes target/perf-results/macos-local/.
[macos]
macos-perf:
    ./scripts/perf/macos-compare.sh

[linux]
macos-perf:
    @echo "macos-perf is a macOS desktop benchmark."

[windows]
macos-perf:
    @echo "macos-perf is a macOS desktop benchmark."

# === Install / uninstall ===========================================

# Drop a build under ~/.local/ (scripts/install.sh — Linux
# only). Same path the `install-online.sh` curl|sh wrapper
# uses for online installs.
#
# Linux uses scripts/install.sh (the XDG
# installer); Windows uses scripts/install.ps1 (per-user install
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

# Linux install that auto-records each Super-key launch into a local asciicast
# directory (the desktop launcher gets KETTLE_RECORD_DIR wired in). Recording
# now ships in every build, so this is a normal release build — equivalently,
# set `record = on` + `record-dir` in the config file.
[unix]
install-recording RECORD_DIR=(env_var("HOME") / ".cache/kettle/records"):
    cargo build --release -p kettle
    ./scripts/install.sh --skip-build --record-dir={{quote(RECORD_DIR)}}
    @printf 'recording install synced to %s\n' {{quote(RECORD_DIR)}}

[windows]
install-recording:
    @echo "install-recording is a Linux helper; on Windows set 'record = on' in the config."

# Exercise scripts/install.sh's real install/uninstall paths (default
# per-user prefix, custom --prefix, the release-tarball layout,
# --record-dir validation, and — once the current tag is published —
# the curl|sh online installer) inside an isolated temp prefix that
# never touches the real ~/.local. Needs a release binary (copies and
# restores target/release/kettle around the run). Mirrors CI's
# Linux-only "Linux installer smoke" step.
[unix]
linux-installer-smoke: release
    ./scripts/check-linux-installers.sh

[windows]
linux-installer-smoke:
    @echo "linux-installer-smoke exercises scripts/install.sh (Linux/XDG paths)."
    @echo "Not applicable on Windows; see 'just windows-installer-smoke' instead."

# Exercise scripts/install.ps1's portable/custom-prefix mode and its
# isolated default-install integration mode — including upgrading a
# stale pre-existing shortcut — on real Windows. Needs a release build
# (kettle.exe + kettle-console.exe). Mirrors CI's Windows-only
# installer-smoke matrix. Both runtimes are required locally just as in CI.
[windows]
windows-installer-smoke: windows-installer-smoke-ps7 windows-installer-smoke-ps5

[windows]
windows-installer-smoke-ps7: release
    pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/check-windows-installer.ps1

[windows]
windows-installer-smoke-ps5: release
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/check-windows-installer.ps1

[unix]
windows-installer-smoke:
    @echo "windows-installer-smoke exercises scripts/install.ps1 (Windows-only paths)."
    @echo "Not applicable here; see 'just linux-installer-smoke' instead."

# === Misc ==========================================================

# Run kettle in a real window (Linux: needs X11 / Wayland; Windows:
# native; macOS: native). Useful when verifying interactive behavior
# the offscreen `--screenshot*` paths can't reach.
run:
    cargo run --release -p kettle

# Capture a screenshot of the right-click context menu via
# scrot + xdotool. Useful for visual regression of the context-menu
# UX overhaul. Output lands in `target/menu-shots/`. Pass
# `--name <slug>` to label the file; `--hold` to leave kettle running.
#
# Linux-only by design — uses xdotool / scrot which only
# exist on X11. Windows / macOS can use the offscreen `just menu`
# recipe instead (the tests/menu_visual.rs pixel-level CI smoke
# covers the same regression class without needing a real desktop).
[unix]
menu-shot *ARGS:
    ./scripts/menu-screenshot.sh {{ARGS}}

[windows]
menu-shot *ARGS:
    @echo "menu-shot requires xdotool + scrot (Linux X11). On Windows,"
    @echo "use 'just menu' instead — it renders the same menu offscreen"
    @echo "via the menu visual-regression pipeline."

# Start a real kettle window with `text-renderer = grid`, capture live
# screenshots through `kettle ctl screenshot`, and assert cursor blink changes
# only a cursor-sized region. This is Linux desktop-local by design: it needs a
# visible X11/Wayland session and complements the CI offscreen renderer tests.
[unix]
live-render-smoke:
    KETTLE_BIN=./target/release/kettle ./scripts/check-live-render-smoke.sh

[windows]
live-render-smoke:
    @echo "live-render-smoke is currently a Unix desktop helper."
    @echo "Windows coverage comes from CI's windows-latest build/test/CLI smoke;"
    @echo "manual Windows live screenshot smoke can use 'kettle --agent-server full'"
    @echo "plus 'kettle ctl screenshot'."

# Drive a real grid-renderer window through shell, optional Codex/Claude CLI,
# tmux, and clean/configured Neovim marker + split states. Captures
# PNG/readback artifacts under target/diagnostics/agent-tui-*. Set
# KETTLE_AGENT_AUTH_SMOKE=1 to include real Codex/Claude marker prompts.
# `--cargo-release` selects Cargo's reported executable, including custom target
# directories/triples, instead of assuming `target/release`.
[unix]
agent-tui-smoke:
    python3 scripts/check-live-ui-smoke.py --cargo-release --shell-mode native agent-tui

[windows]
agent-tui-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release --shell-mode native agent-tui

# Exercise the Windows Kettle executable + ConPTY boundary while all shell,
# tmux, Neovim/AstroNvim, Codex, and Claude commands run inside WSL. Set
# KETTLE_SMOKE_WSL_DISTRO to select a non-default distro and
# KETTLE_SMOKE_ASTRO_CONFIG / KETTLE_SMOKE_NVIM_DATA to select its config and
# plugin-data directories. The helper copies bounded regular files into an
# owner-private snapshot and redirects HOME plus all Neovim XDG paths there.
[windows]
agent-tui-wsl-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release --shell-mode wsl agent-tui

[unix]
agent-tui-wsl-smoke:
    @echo "agent-tui-wsl-smoke exercises Windows kettle.exe -> wsl.exe and must run on Windows."

# Drive broader live UI interactions: multiline text entry, scrollback wheel,
# selection drag, tab creation, context-menu split dispatch, and screenshots.
[unix]
interaction-smoke:
    python3 scripts/check-live-ui-smoke.py --cargo-release interaction

[windows]
interaction-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release interaction

# Reproduce a Windows Precision Touchpad gesture: a stream of sub-detent wheel
# deltas (the units winit actually reports) instead of pre-quantized whole
# lines. Guards the v2.41.0 fix where every such event rounded to zero on its
# own and touchpad scrolling was completely dead. Drives `wheel_delta`, the
# only synthetic path that runs the real accumulator — the older integer
# `wheel_lines` form enters downstream of the conversion and cannot reproduce
# it. Artifacts under target/diagnostics/touchpad-scroll-*.
[unix]
touchpad-scroll-smoke: release
    python3 scripts/check-live-ui-smoke.py --cargo-release touchpad-scroll

[windows]
touchpad-scroll-smoke: release
    python scripts/check-live-ui-smoke.py --cargo-release touchpad-scroll

# Reproduce and guard the multi-tab mouse-click visual state. Captures full
# window PNGs and tab geometry JSON under target/diagnostics/tabbar-click-*.
[unix]
tabbar-click-smoke: release
    KETTLE_BIN=./target/release/kettle ./scripts/check-tabbar-click-smoke.sh

[windows]
tabbar-click-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release tabbar

# Terminator parity: drag a terminal to another position in its tab. Drives the
# press/move/release through the control plane and checks the gesture reaches
# the tree -- the pure drop-zone geometry is unit-tested in mux.rs, but only a
# live window shows that a titlebar press ever gets there.
[unix]
pane-drag-smoke: release
    KETTLE_BIN=./target/release/kettle ./scripts/check-pane-drag-smoke.sh

# v2.40.0 (tear-off UX): tear-off regression guards, two tiers. The ctl tier
# proves the mouseless move_tab_to_new_window tear + tab_moved broadcast; the
# live tier drives xdotool REAL pointer input through the full gesture
# (tear -> follow -> re-dock merge -> Esc cancel), because `maybe_tear_off`
# and re-dock only respond to native winit pointer events — synthetic
# `ctl send_mouse` cannot reach them by design. Live tier is X11-desktop-only
# (skips cleanly elsewhere); artifacts under target/diagnostics/tearoff-*.
[unix]
tearoff-smoke: release
    python3 scripts/check-live-ui-smoke.py --cargo-release tearoff
    KETTLE_BIN=./target/release/kettle ./scripts/check-tearoff-live-smoke.sh

[windows]
tearoff-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release tearoff

# Reproduce cwd-derived title recovery for shell-truncated tab titles.
# Captures list_panes/list_tabs/ui_geometry under target/diagnostics/tab-title-*.
[unix]
tab-title-smoke:
    python3 scripts/check-live-ui-smoke.py --cargo-release tab-title

[windows]
tab-title-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release tab-title

# Reproduce cwd-derived split titlebars at both pane edges and verify their
# focused/receiving/inactive PNG colors against ui_geometry-derived samples.
# Captures screenshots/list_panes/ui_geometry/analysis under split-titlebar-*.
[unix]
split-titlebar-smoke:
    python3 scripts/check-live-ui-smoke.py --cargo-release split-titlebar

[windows]
split-titlebar-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release split-titlebar

# Reproduce app-level zoom keybind matching without compositor key injection.
# Captures dispatch_keybind/ui_geometry under target/diagnostics/zoom-keybind-*.
[unix]
zoom-keybind-smoke:
    python3 scripts/check-live-ui-smoke.py --cargo-release zoom-keybind

[windows]
zoom-keybind-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release zoom-keybind

# Reproduce underline scrolling with git diff | delta under repeated j/k input.
# Captures PNG frames and read_cells JSON under target/diagnostics/underline-scroll-*.
[unix]
underline-scroll-smoke: release
    KETTLE_BIN=./target/release/kettle ./scripts/check-underline-scroll-smoke.sh

[windows]
underline-scroll-smoke:
    python scripts/check-live-ui-smoke.py --cargo-release underline

# Clean every build artifact — `cargo clean` plus any temp PNGs
# the screenshot / menu / bench recipes may have left in the OS
# temp dir.
#
# Split into [unix] / [windows] because `rm` and `/tmp`
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
