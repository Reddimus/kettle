# Kettle Repository Instructions

These instructions apply to the whole repository.

## Development

- Read the owning crate and nearby tests before changing behavior. Prefer the
  existing crate boundaries and shared helpers over duplicate implementations.
- Keep changes scoped. Do not discard or rewrite unrelated work in a dirty
  tree, and do not reformat files outside the task.
- Use ASCII unless a file already needs Unicode. Add comments only for
  non-obvious invariants or platform behavior.
- Treat terminal input, escape-sequence parsing, local IPC, persistence, update
  archives, and installer paths as security boundaries. Enforce explicit size,
  permission, validation, and failure semantics.
- Add focused regression tests for behavior changes. Platform-specific code
  must retain portable unit coverage and be exercised on its native CI runner.

## Validation

Format only the Rust crates you changed while iterating, then run the repository
gate:

```sh
cargo fmt -p <crate>
cargo fmt --all --check
just gauntlet
```

Before a release or supply-chain change, run `just gauntlet-strict`. It also
requires locally installed `cargo-deny` and `cargo-machete`. Useful focused
commands include `cargo test -p <crate>` and
`cargo clippy -p <crate> --all-targets -- -D warnings`.

Do not claim a platform, GPU, installer, or live-UI check passed unless it was
actually run. Record skipped checks and the reason.

## Version Control

- Never run `git commit`, `git push`, or `git merge` unless the user explicitly
  requests that operation.
- Never run `svn commit` or `svn merge` unless the user explicitly requests it.
- Do not use destructive history or worktree commands to remove changes you did
  not create.

## Documentation

Keep user-visible flags and behavior synchronized across clap help, README,
`docs/`, installer scripts, desktop/man-page assets, and tests. Architecture
changes belong in `docs/ARCHITECTURE.md`; verification changes belong in
`docs/TESTING.md`.
