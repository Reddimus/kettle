# Claude Code Instructions

Follow [`AGENTS.md`](AGENTS.md) for repository-wide engineering, validation,
documentation, and version-control rules.

Kettle is a Rust workspace. Start with `Cargo.toml`, the owning crate, and its
nearby tests; use the existing `just` recipes for validation. Preserve the
separation between VT parsing (`kettle-vt`), terminal state (`kettle-core`), GPU
rendering (`kettle-render`), UI/mux state (`kettle-ui`), control IPC
(`kettle-ctl`), durable state (`kettle-state`), updates (`kettle-update`), and
the CLI (`kettle`).

Terminal output, control messages, recordings, configuration, sessions, update
archives, and installer paths may contain untrusted or private data. Keep them
bounded, permission-restricted, and out of logs and fixtures unless explicitly
scrubbed.
