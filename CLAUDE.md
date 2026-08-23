# Claude Code Instructions

Follow [`AGENTS.md`](AGENTS.md) for repository-wide engineering, validation,
documentation, and version-control rules.

Kettle is a Rust workspace of eleven crates. Start with `Cargo.toml`, the owning
crate, and its nearby tests. Use the existing `just` recipes for validation.

Keep the crate boundaries intact:

| Crate | Owns |
|---|---|
| `kettle-vt` | VT and ANSI escape parsing |
| `kettle-core` | terminal state: grid, scrollback, selection, search |
| `kettle-render` | GPU rendering |
| `kettle-ui` | window, mux, and modal state |
| `kettle-config` | config files, keybinds, themes |
| `kettle-ctl` | control IPC |
| `kettle-remote` | the remote command spool |
| `kettle-state` | durable writes and advisory locking |
| `kettle-update` | signed release feed and self-update |
| `kettle` | the CLI and process entry point |
| `kettle-test-support` | shared test fixtures and drift guards |

Terminal output, control messages, recordings, configuration, sessions, update
archives, and installer paths may contain untrusted or private data. Keep them
bounded, permission-restricted, and out of logs and fixtures unless explicitly
scrubbed.
