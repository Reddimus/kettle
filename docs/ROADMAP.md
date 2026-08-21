# Roadmap

This file tracks unfinished product work. Shipped work belongs in the
[changelog](../CHANGELOG.md) and [version history](VERSION-HISTORY.md). Deferred
audit findings, including their evidence and stopping rules, live in
[AUDIT-DEFERRED.md](AUDIT-DEFERRED.md).

## Current priorities

### Visual regression coverage

Promote stable live UI scenarios into CI one at a time. Each gate must assert
geometry and pixels, retain useful failure artifacts, and run on the native
platform it claims to cover. Current manual scenarios include tab interaction,
underline scrolling, media paste receipts, search, shell completion, and
multi-window isolation.

### Interactive application coverage

Extend the live grid tests for editors, multiplexers, and interactive coding
clients. Prefer deterministic control actions, screen reads, cell reads, and
screenshots over manual observation. Keep authentication failures separate from
terminal rendering failures.

### Performance evidence

Keep startup, sustained output, resize, scrollback, memory, and input latency
within the published thresholds. Comparative claims require pinned binaries,
isolated configs, repeated samples, and a physical display that passes the
monitor identity checks. Synthetic and virtual-display runs remain useful for
regressions but are not release evidence.

### Protocol conformance

Continue the bounded conformance sweep for control sequences, graphics,
keyboard negotiation, and shell marks. Add one focused regression for each new
behavior. Known gaps include the remaining graphics query and retransmission
paths and broader `vttest` coverage.

## Larger projects

These have design records or clear prerequisites and should not be folded into
unrelated fixes.

* Finish the control-mode integration described in
  [TMUX-CC-DESIGN.md](TMUX-CC-DESIGN.md).
* Build a detachable session daemon from
  [MUX-SERVER-DESIGN.md](MUX-SERVER-DESIGN.md).
* Add live PTY adoption for cross-process pane transfer after the ownership and
  rollback model is proven.
* Add persistent in-terminal annotations with scrollback-stable positions.
* Add CPU and memory widgets to the status bar without polling on the render
  path.
* Add a native macOS menu bar.
* Add a signed Windows MSI installer.

## Packaging and distribution

* Publish a source-build AUR package and a Homebrew `--HEAD` formula.
* Submit the package to nixpkgs after the current flake remains stable across a
  release cycle.
* Keep installer, updater, signature, and rollback checks fail-closed.

## Upstream blockers

* The font stack still carries `fontdb 0.23` through `cosmic-text 0.19`.
  `fontdb 0.24` removes the older parser dependency, but the workspace cannot
  adopt it until the text stack publishes a compatible release. Track this in
  issue #36 rather than adding a local fork without a separate design review.
* Windows MSI signing needs a trusted Windows code-signing certificate.

## Quality bar

Every behavior change needs a focused regression test. Platform code must also
run on that platform's CI job. Before merge:

```sh
cargo fmt --all --check
just gauntlet
```

Release and supply-chain changes also run `just gauntlet-strict`. Record any
native, GPU, installer, or live UI check that was skipped and why.
