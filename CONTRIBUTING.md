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
   test suite grows by roughly one test per change — see
   [docs/TESTING.md](docs/TESTING.md) for the current per-crate
   breakdown, or run `cargo test --workspace` for today's number.

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
   runs on every OS. Run `just --list` for the full,
   always-current set of recipes (build/release/install helpers,
   the `gauntlet`/`gauntlet-strict`/`gauntlet-full` CI mirrors, and
   the live-UI smoke recipes like `interaction-smoke` /
   `tabbar-click-smoke` used for manual renderer/tab-bar checks —
   see [docs/TESTING.md](docs/TESTING.md)). `just deny` (`cargo deny check`) and
   `just machete` (`cargo machete`) mirror the supply-chain CI
   workflows so a stale dependency-ignore entry is caught
   at the local pre-flight. `just gauntlet-strict` chains
   gauntlet + deny + machete for release-cut pre-flight.
   The supported Linux and macOS jobs run the same gate. A retained Windows
   job compiles and tests portable and conditional code, but does not claim a
   supported Windows package or installer. CI also runs a headless GPU smoke
   under Xvfb on Linux, a `--screenshot`
   end-to-end check, a `--screenshot-menu` visual regression, a
   MSRV (Rust 1.89) build verification, and a `cargo audit` advisory
   scan. The local gate must be green before pushing.

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
   `[Unreleased]` describing the bug shape and the fix. Keep
   `docs/ROADMAP.md` for unfinished work; shipped work belongs in the
   changelog and version history.
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
- **Or look at `docs/ROADMAP.md`.** Current priorities are small enough to
  review in one change; larger projects have their own design records.

## Style

- **`cargo fmt --all` + `cargo clippy -D warnings` are mandatory.**
  The CI gate rejects anything that doesn't pass.
- **Comments describe *why*, not *what*.** Git blame gives the
  audit trail; the paragraph explains the bug class. Recent
  drift-guard comments are good templates.
- **Cite external behavior.** When compatibility depends on another
  implementation or protocol, name the exact source file or specification in
  the focused code comment.
- **Tests live next to the code they test** (`#[cfg(test)] mod`),
  not in `tests/`. Workspace-wide tests don't exist; each crate
  is self-contained. A black-box test that needs a built binary or spans crate
  boundaries may live under the owning binary crate's `tests/` directory.

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

Releases go through `scripts/release.sh` (version bump + single
signed commit) and `scripts/tag-release.sh` (signed annotated tag,
pushed from synchronized `main`), with a PR and required CI between
them. Doing the steps by hand has tripped past releases: the
CHANGELOG section got committed AFTER the tag, the release-pipeline
CI guard correctly rejected the Linux job at pre-flight, and the
macOS + Windows jobs uploaded a partial release. Always use the
scripts — `release.sh` intentionally never pushes or tags `main`
itself.

Flow:

1. Land your changes on `main` (via PR), including a
   `## [X.Y.Z] — YYYY-MM-DD` section in `CHANGELOG.md`
   describing what changed since the previous version.
2. Run `just gauntlet-strict` to verify every CI workflow's
   check (fmt / clippy / build / test / doc / cargo-deny /
   cargo-machete) passes locally first. The plain `just
   gauntlet` mirrors every-PR CI; the `-strict` variant adds
   the supply-chain CI workflows that run on Cargo.lock
   changes, so a release-cut catches stale-ignore / unused-dep
   issues before tagging. `just gauntlet-full` is the closest
   local match to every ci.yml step for a release cut.
3. From fresh `main`, create a release branch and run
   `scripts/release.sh X.Y.Z` on it. The script refuses to run on
   `main` or a dirty tree, and rejects a missing CHANGELOG section,
   an existing tag, or a non-semver VERSION. On success it leaves a
   single signed `release: vX.Y.Z` commit bumping
   `Cargo.toml`, `Cargo.lock`, `flake.nix`, `docs/INSTALL.md`, and
   `docs/VERSION-HISTORY.md`.
4. Push the branch, open a PR titled `release: vX.Y.Z`, wait for
   the required checks, and merge (merge commit, matching the
   repo's history).
5. On synchronized `main`, run `scripts/tag-release.sh X.Y.Z` — it
   re-validates the version/CHANGELOG pairing, creates the signed
   annotated tag `vX.Y.Z`, verifies it, and pushes it.
6. The release workflow (pretest → three-package matrix →
   finalize) builds the platform archives + `.sha256` sidecars,
   Ed25519-signs the update manifest, and publishes the GitHub
   release from a verified draft. Poll it with:
       gh run list --workflow=release.yml --limit 1
7. Verify the install path resolves:
       KETTLE_VERSION=vX.Y.Z sh scripts/install-online.sh

Patch vs minor vs major: kettle follows semver loosely — a new
config key or CLI flag is a minor (e.g., v1.7 → v1.8). A bug fix
without API surface change is a patch (v1.7.1 → v1.7.2). A
breaking change to the config schema or library surface is a
major (v1.x → v2.0). Removing the Windows package and update target in
v4.0.0 is a distribution-breaking change, so it also requires a major release.
The final Windows-supported release is v3.3.0.
