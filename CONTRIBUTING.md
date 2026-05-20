# Contributing to kettle

kettle is built one *audit cycle* at a time — each cycle picks one bug or
parity gap, fixes it with a durable implementation, pins the contract with
a test, and lands behind the full gate. This file explains how a cycle
looks so a new contributor can land their first change the same shape as
the existing 150+ in [CHANGELOG.md](CHANGELOG.md).

## The audit cycle

Each cycle has the same shape:

1. **Find one bounded bug.** Read the source for a *silent-fallback*
   pattern (`_ => Default`, `if let Ok(v) = parse() { ... }`,
   `e.value != "false"`), a *docs/runtime mismatch*, or a *parity gap*
   with another terminal. The good ones are bounded — one
   handler / one helper / one config key — and produce a visible
   user-facing effect when broken.
2. **Extract a pure helper if applicable.** Logic that depends only
   on its arguments (no `&self`, no I/O) is easier to test than a
   chrome wiring change. Many cycles' "real" change is the helper;
   the wiring is two lines. See `kettle-config::keybinds::parse_bool`
   (cycle 138), `kettle-render::cap_axis_cells` (cycle 119),
   `kettle-render::clamp_font_size` (cycle 118) for examples.
3. **Wire it in.** Call the helper from the chrome path. Keep the
   call site small — the helper does the work.
4. **Pin the contract with a test.** Hand-rolled scenarios that
   would have failed pre-fix. Most cycles add 1–3 assertions; the
   workspace test suite grows ~1/cycle. Run `cargo test --workspace`
   for today's count (CHANGELOG entries also name the count after each
   cycle's `+N tests`).
5. **Run the gate locally.**
   ```sh
   cargo fmt --all              # rewrite in place
   cargo fmt --all --check      # then assert no further drift — local
                                # rustfmt may be older than CI's, and the
                                # *check* form is what CI runs. Cycle 167
                                # shipped fmt-clean locally and failed CI;
                                # adding the --check step makes the local
                                # gate match the CI gate exactly.
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
   The CI matrix on `main` runs the same on Linux / macOS / Windows
   plus a headless GPU smoke under Xvfb on Linux. The local gate
   must be green before pushing.
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
  kettle-config/   Config parsing · 512 themes · keybinds · ssh-host · fuzzy
  kettle-vt/       Image-protocol extractor (Sixel · kitty · iTerm2 · OSC 7/133)
  kettle-core/     PTY reader · alacritty_terminal+vte · search · hints · links
  kettle-render/   wgpu pipelines · glyphon text · screenshots · GPU self-test
  kettle-ui/       winit app · tab/split mux · session · input · all the chrome
  kettle/          CLI entry point (clap) · --list-* / --check-config / --screenshot
```

Each crate has its own tests. Anything pure (logic with no `&self` / I/O)
should live in the crate it most belongs to and have unit tests there.

## What makes a good cycle

- **The bug is bounded.** "fix font rendering" isn't a cycle; "the
  surface alpha-mode is hardcoded to `caps.alpha_modes[0]` which is
  usually `Opaque`, so `background-opacity = 0.5` had no visible
  effect" is. Bounded means the fix touches one or two files, the
  test is one new function, and the CHANGELOG paragraph is short.

- **The fix is durable.** No "TODO: revisit this" or `unwrap()` on
  things that can fail in normal use. If the bug class can recur
  (e.g., HashMap insertion shadow-collisions in defaults, cycle
  116), add a *drift guard* — a test that fails the next time
  someone reintroduces the same shape of bug.

- **There's a test you'd want even without the bug.** Tests pin
  contracts. The `defaults_has_no_shadow_collisions` test in
  cycle 116 isn't just "I fixed the cycle-115 collision" — it's
  "every default binding gets a unique trigger, forever."

- **The CHANGELOG paragraph names the user-visible effect.** Not
  "fixed widget", "fixed a thing in the code". State which input,
  which output, what changed for the user. See cycles 148, 150,
  151, 157 for shape.

## A real example (cycle 151)

The notify-watcher reloaded config on *every* event in the watched
directory. Cycle 109's atomic session save fires 3+ events per save
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
where the wasteful events came from (cycle 109 atomic save). Done.

## Where to start

- **Read a few cycles in CHANGELOG.md** to see the shape. Recent
  cycles 140–158 cover the breadth: blink-phase resets, modal-
  close paths, BOM stripping, case-insensitivity sweeps, transparent
  background rendering, screenshot alpha, config-clamp diagnostics.
- **Pick a `_ => {}` arm in the codebase.** Trace what it ignores,
  identify whether the silent fallback is a real bug or
  intentional. If real, that's your cycle.
- **Or look at `docs/ROADMAP.md`'s "Next" list.** Some explicit
  larger features (detachable mux server, native macOS menu bar,
  broader `vttest` conformance sweep) are listed there.

## Style

- **`cargo fmt --all` + `cargo clippy -D warnings` are mandatory.**
  The CI gate rejects anything that doesn't pass.
- **Comments describe *why*, not *what*.** The cycle number gives
  the audit trail; the paragraph explains the bug class. See
  cycles 138 / 146 / 147 for the in-code comment template.
- **Cite the convention.** If you're matching Alacritty, kitty,
  WezTerm, Ghostty, or Terminator behavior, say so in the
  in-code comment. e.g. cycle 142's `beam` alias for `bar`
  cites Alacritty's spelling.
- **Tests live next to the code they test** (`#[cfg(test)] mod`),
  not in `tests/`. Workspace-wide tests don't exist; each crate
  is self-contained.
