<!--
Thanks for the PR! kettle ships one cycle per commit (see CONTRIBUTING.md).
This template mirrors that shape — the goal is for any reader to reproduce
your reasoning + verify the change without spelunking the diff.
-->

## Summary

<!-- One sentence: what does this PR change? -->

## Why

<!--
The motivation. Link to the issue, the upstream spec, the reference
terminal's behavior, or the cycle-style "user pain → why it was wrong → fix"
that drives the rest of the repo.
-->

## Approach

<!--
Short description of the implementation. Where does the new logic live
(pure helper? action handler? CLI surface?), and what wires it in?
If you added a drift-guard test, mention which class of regression it
catches.
-->

## Verification

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo build --workspace --all-targets`
- [ ] `cargo test --workspace`
- [ ] Manually exercised the change end-to-end (describe what you ran)
- [ ] Docs updated (README / docs/ROADMAP.md / docs/CONFIG.md /
      CHANGELOG.md as applicable)

## Cycle metadata

<!--
Optional but recommended — see CONTRIBUTING.md cycle pattern. Helps the
maintainer assign a cycle number on merge.
-->

- **Class:** <!-- bug | feature | conformance | ci | docs | refactor -->
- **Touches:** <!-- crates / files in one line -->
- **Test added:** <!-- yes / no — if no, explain why -->

## Notes for reviewer

<!-- Anything the reviewer should pay attention to / things you considered and ruled out. -->
