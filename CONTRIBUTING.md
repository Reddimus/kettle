# Contributing to kettle

kettle is built one bounded change at a time — each change picks one bug
or parity gap, fixes it with the smallest durable implementation, pins
the contract with a test, and lands behind the full gate. This file
explains how a change like that looks so a new contributor can land
their first PR the same shape as the existing 440+ entries in
[CHANGELOG.md](CHANGELOG.md).

Participation in this project — issues, PRs, discussions, code review —
is governed by the project [Code of Conduct](CODE_OF_CONDUCT.md). For
confidential vulnerability reports see [`SECURITY.md`](SECURITY.md)
instead.

## Anatomy of a change

Each change has the same shape:

1. **Find one bounded bug.** Read the source for a *silent-fallback*
   pattern (`_ => Default`, `if let Ok(v) = parse() { ... }`,
   `e.value != "false"`), a *docs/runtime mismatch*, or a *parity gap*
   with another terminal. The good ones are bounded — one
   handler / one helper / one config key — and produce a visible
   user-facing effect when broken.
2. **Extract a pure helper if applicable.** Logic that depends only
   on its arguments (no `&self`, no I/O) is easier to test than a
   chrome wiring change. For many changes the "real" work is the
   helper; the wiring is two lines. See `kettle-config::parse_bool`,
   `kettle-render::cap_axis_cells`, and
   `kettle-render::clamp_font_size` for examples.
3. **Wire it in.** Call the helper from the chrome path. Keep the
   call site small — the helper does the work.
4. **Pin the contract with a test — and add a drift guard if the
   bug class can recur.** Hand-rolled scenarios that would have
   failed pre-fix. Most changes add 1–3 assertions; the workspace
   test suite grows by roughly one test per change (currently 319+,
   see `cargo test --workspace`).

   A **drift guard** is a separate test that catches *the next
   time someone reintroduces the same shape of bug* — not just
   the specific instance you fixed. Drift guards are how kettle
   stays consistent across 440+ changes without regressing. Three
   kinds you'll see in the codebase:

   - **Exhaustive-match guards.** When a new `Action` variant is
     added, the `palette_includes_every_user_facing_action`
     test fails at compile time until the variant is categorized
     (palette entry / excluded with rationale). Same shape:
     `defaults_has_no_shadow_collisions`,
     `cli_help_text_has_no_internal_cycle_refs`.
   - **Drift-against-source guards.** Tests that read a Markdown
     doc or a source string and assert it stays consistent with
     a contract — e.g. `user_facing_doc_md_cross_links_resolve`
     (every user-facing doc's `.md` cross-links stay resolvable),
     `cli_help_preserves_indented_code_examples` (walks
     clap's `CommandFactory` and asserts indented examples survive
     verbatim).
   - **Pixel / output guards.** Render-pipeline regressions are
     hard to catch with logic tests — `tests/menu_visual.rs`
     renders to PNG and asserts pixel-color invariants so the
     v1.3.0/v1.3.1 blank-menu regression class can't recur.

   If your change's bug class is bounded ("a typo here" — no drift
   guard needed) say so in the CHANGELOG paragraph. If it's
   structural ("`_ => Default` silent fallback") add the guard.

5. **Run the gate locally.**
   ```sh
   cargo fmt --all              # rewrite in place
   cargo fmt --all --check      # then assert no further drift — local
                                # rustfmt may be older than CI's, and the
                                # *check* form is what CI runs. The local
                                # `--check` step exists because a past
                                # release shipped fmt-clean locally and
                                # failed CI — keeping both invocations
                                # in lockstep makes the local gate match
                                # the CI gate exactly.
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   ```
   Or, if you have [`just`](https://just.systems) installed
   (`cargo install just`), one command runs the whole gate:
   ```sh
   just gauntlet
   ```
   The Justfile at the repo root mirrors every CI step so a
   green `just gauntlet` locally is the same gate every PR
   runs on every OS. `just --list` shows every recipe
   (`fmt` / `clippy` / `test` / `doc` / `deny` / `machete` /
   `build` / `release` / `gauntlet` / `gauntlet-strict` /
   `screenshot` / `menu` / `bench` / `install` / `uninstall` /
   `run` / `clean`). `just deny` (`cargo deny check`) and
   `just machete` (`cargo machete`) mirror the supply-chain CI
   workflows so a stale dependency-ignore entry is caught
   at the local pre-flight. `just gauntlet-strict` chains
   gauntlet + deny + machete for release-cut pre-flight.
   The CI matrix on `main` runs the same on Linux / macOS / Windows
   plus a headless GPU smoke under Xvfb on Linux, a `--screenshot`
   end-to-end check, a `--screenshot-menu` visual regression, a
   MSRV (Rust 1.89) build verification, and a `cargo audit` advisory
   scan. The local gate must be green before pushing.

   **Windows 11 dev gotcha**: `cargo install <anything>`
   (and even some `cargo build` steps for crates with `build.rs`)
   can be blocked by Windows **Smart App Control (SAC)** with the
   error `An Application Control policy has blocked this file
   (os error 4551)`. SAC blocks any unsigned `.exe`, and every
   build-script artifact cargo produces is unsigned. SAC ships
   enabled by default on clean Win11 installs with Secure Boot on.

   Workaround: disable SAC at **Settings ▸ Privacy & Security ▸
   Windows Security ▸ App & browser control ▸ Smart App Control ▸
   Off**. **This is a one-way toggle** — re-enabling requires
   reinstalling Windows. Required if you want to do Rust dev on
   Win11. Use winget-installed tools (`winget install Casey.Just`
   etc.) for signed binaries that bypass SAC.

   **Optional pre-commit hook**: `.githooks/pre-commit` runs the
   gate automatically on every `git commit` (skipping doc-only
   commits to stay fast). Opt in once per checkout with:

   ```sh
   git config core.hooksPath .githooks
   ```

   The hook exists because a doc-list overindentation regression
   landed more than once without anyone running clippy — the
   hook catches that class at commit time.
   The hook header comment in `.githooks/pre-commit` enumerates
   exactly which path categories trigger the gauntlet vs which
   skip it; bypass per-commit with `git commit --no-verify`.
6. **Update docs.** `CHANGELOG.md` gets a paragraph under
   `[Unreleased]` describing the bug shape and the fix.
   `docs/ROADMAP.md`'s `Done` list gets a one-paragraph entry of the
   same shape used by neighboring entries.
7. **Commit with a body that names the bug.** Commit messages
   follow the shape: subject line is `<crate>: <one-line summary>`
   in the imperative; body has paragraphs for the bug, the fix, and
   the test rationale.
8. **Push, watch CI go green, move on.**

## Project layout

```text
crates/
  kettle-state/    Durable atomic file replacement · advisory file locks
  kettle-update/   Signed update feeds · bounded extraction · transactions
  kettle-config/   Config parsing · 500+ themes · keybinds · ssh-host · fuzzy
  kettle-vt/       Image-protocol extractor (Sixel · kitty · iTerm2 · OSC 7/133)
  kettle-core/     PTY reader · alacritty_terminal+vte · bounded grid search · hints · links
  kettle-render/   wgpu pipelines · glyphon text · search/chrome geometry · screenshots · GPU self-test
  kettle-remote/   SSH/container detection · process-tree inspection
  kettle-ctl/      Local control protocol · IPC transport · discovery · client
  kettle-ui/       winit app · tab/split mux · session · input · all the chrome
  kettle/          CLI entry point (clap) · exec / ctl / mcp · GUI launch
```

Each crate has its own tests. Anything pure (logic with no `&self` / I/O)
should live in the crate it most belongs to and have unit tests there.

Search changes intentionally keep responsibilities split: grid matching and
signed spans belong in `kettle-core`, which adapts bounded terminal-grid
materialization to `regex-automata`'s meta engine. Editor, scheduling, and
per-window state belong in `kettle-ui`; responsive geometry and highlight
projection belong in `kettle-render`. Preserve the 4096-byte query cap, 65,536
match-projection cap, 512 KiB NFA / 256 KiB one-pass / 256 KiB hybrid-cache /
40 KiB DFA engine ceilings, and implicit whole-match-only capture policy.
Runtime work is capped at 64 KiB for both one engine call and one aggregate
bounded call; the latter also permits at most 262,144 inspected cells and 256
complete logical-line haystacks. One haystack permits at most 256 physical rows
and 262,144 inspected cells, with the same 64 KiB text ceiling. Preserve the
distinction between an exact continuation (yield only between complete hard
logical lines) and an in-line capacity barrier (**Results limited**, with no
continuation past uninspected cells). Preserve scan invalidation and
modal-input/PTY separation. Add portable tests for engine-size rejection,
work-budget resumption, soft wraps, Unicode graphemes, zero-width suppression,
nullable-expression priority, and pathological logical lines;
platform-specific keyboard or live-window claims still need the native CI
runner or an explicitly recorded interactive check.

## What makes a good change

- **The bug is bounded.** "fix font rendering" isn't bounded; "the
  surface alpha-mode is hardcoded to `caps.alpha_modes[0]` which is
  usually `Opaque`, so `background-opacity = 0.5` had no visible
  effect" is. Bounded means the fix touches one or two files, the
  test is one new function, and the CHANGELOG paragraph is short.

- **The fix is durable.** No "TODO: revisit this" or `unwrap()` on
  things that can fail in normal use. If the bug class can recur
  (e.g., HashMap insertion shadow-collisions in defaults), add a
  *drift guard* — a test that fails the next time someone
  reintroduces the same shape of bug.

- **There's a test you'd want even without the bug.** Tests pin
  contracts. The `defaults_has_no_shadow_collisions` test isn't
  just "I fixed a binding collision" — it's "every default binding
  gets a unique trigger, forever."

- **The CHANGELOG paragraph names the user-visible effect.** Not
  "fixed widget", "fixed a thing in the code". State which input,
  which output, what changed for the user. See past CHANGELOG
  entries for the shape we land on.

## A real example

The notify-watcher reloaded config on *every* event in the watched
directory. The atomic session save fires 3+ events per save
(create-temp / write-temp / rename). Result: every focus change /
tab switch / split → 3+ unrelated config reloads. The fix was three
lines:

```rust
notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
    if let Ok(ev) = res
        && ev.paths.iter().any(|p| p == &watched)
    {
        let _ = p.send_event(UserEvent::ReloadConfig);
    }
})
```

No new tests — `notify` needs a real FS + event loop to exercise, and
the filter is correctness-by-construction (paths().any(==watched)).
CHANGELOG paragraph names the user-visible effect ("Live config
reload no longer fires on unrelated file events") and explains
where the wasteful events came from (the atomic session save).
Done.

## Where to start

- **Read a few entries in CHANGELOG.md** to see the shape — each
  paragraph names the user-visible effect, the root cause, and
  the file:line of the fix.
- **Pick a `_ => {}` arm in the codebase.** Trace what it ignores,
  identify whether the silent fallback is a real bug or
  intentional. If real, that's your change.
- **Or look at `docs/ROADMAP.md`'s "Next" list.** Some explicit
  larger features (detachable mux server, native macOS menu bar,
  broader `vttest` conformance sweep) are listed there.

## Style

- **`cargo fmt --all` + `cargo clippy -D warnings` are mandatory.**
  The CI gate rejects anything that doesn't pass.
- **Comments describe *why*, not *what*.** Git blame gives the
  audit trail; the paragraph explains the bug class. Recent
  drift-guard comments are good templates.
- **Cite the convention.** If you're matching Alacritty, kitty,
  WezTerm, Ghostty, or Terminator behavior, say so in the
  in-code comment (e.g. the `beam` alias for `bar` cites
  Alacritty's spelling).
- **Tests live next to the code they test** (`#[cfg(test)] mod`),
  not in `tests/`. Workspace-wide tests don't exist; each crate
  is self-contained.

## Mass mechanical changes

Bulk, no-semantic-effect cleanups (formatting passes, rename sweeps,
doc-comment rewrites) are recorded in `.git-blame-ignore-revs` so they
don't obscure `git blame` for everything they touch. Run once per
checkout:

```sh
git config blame.ignoreRevsFile .git-blame-ignore-revs
```

GitHub's blame view honors the file automatically, no local setup
needed there.

## Releasing

Releases go through `scripts/release.sh`, which does the four ops
atomically (working-tree clean check, CHANGELOG section check,
Cargo.toml bump, Cargo.lock refresh, single commit, annotated
tag). Doing them by hand has tripped past releases: the CHANGELOG
section got committed AFTER the tag, the release-pipeline CI guard
correctly rejected the Linux job at pre-flight, and the macOS +
Windows jobs uploaded a partial release. Always use the script.

Flow:

1. Land your changes on `main`.
2. Add a `## [X.Y.Z] — YYYY-MM-DD` section to `CHANGELOG.md`
   describing what changed since the previous version. Commit it.
3. Run `just gauntlet-strict` to verify every CI workflow's
   check (fmt / clippy / build / test / doc / cargo-deny /
   cargo-machete) passes locally first. The plain `just
   gauntlet` mirrors every-PR CI; the `-strict` variant adds
   the supply-chain CI workflows that run on Cargo.lock
   changes, so a release-cut catches stale-ignore / unused-dep
   issues before tagging.
4. Run `scripts/release.sh X.Y.Z`. It refuses to proceed if the
   working tree is dirty, if the CHANGELOG section is missing,
   if the tag already exists, or if VERSION isn't strict semver.
   On success: commits the bump + creates the annotated tag.
5. Sanity-check the commit + tag, then push:
       git push origin main && git push origin vX.Y.Z
6. The release workflow builds + uploads the three platform
   tarballs + their `.sha256` sidecars. Watch it with:
       gh run watch $(gh run list --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
7. Verify the install path resolves:
       KETTLE_VERSION=vX.Y.Z sh scripts/install-online.sh

Patch vs minor vs major: kettle follows semver loosely — a new
config key or CLI flag is a minor (e.g., v1.7 → v1.8). A bug fix
without API surface change is a patch (v1.7.1 → v1.7.2). A
breaking change to the config schema or library surface is a
major (v1.x → v2.0). kettle has shipped one major to date —
v2.0.0 (the Windows 11 / PowerShell 7 release).
