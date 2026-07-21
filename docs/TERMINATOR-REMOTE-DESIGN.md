# Terminator `remote.py` port — design

> Status: design only. The runtime PID + process-tree walking
> machinery is too large to land in one pass, so this doc lays out the architecture
> + the phase roadmap so the implementation lands as a series of small
> testable phases instead of one heroic push. Same shape as
> [`TERMINATOR-PLUGIN-DESIGN.md`](TERMINATOR-PLUGIN-DESIGN.md),
> [`TERMINATOR-DETACHABLE-TABS-DESIGN.md`](TERMINATOR-DETACHABLE-TABS-DESIGN.md),
> [`TERMINATOR-PANE-TITLEBAR-DESIGN.md`](TERMINATOR-PANE-TITLEBAR-DESIGN.md),
> [`TERMINATOR-BG-IMAGE-DESIGN.md`](TERMINATOR-BG-IMAGE-DESIGN.md).

## What it is

Terminator's `plugins/remote.py` detects when a pane's foreground process
is an SSH client (`ssh`) or a container client (`docker exec` / `podman
exec`), and updates the pane title + offers a "Clone session" right-click
menu entry to spawn a new pane connected to the same remote. The
detection is best-effort — it polls the pane's child process tree once
per second via `psutil` and matches against argv patterns.

End-state UX in kettle:

- A user runs `ssh me@box`. Within ~1s, the pane title (and pane
  titlebar from the per-pane titlebar work, Bucket D) shows
  `ssh me@box`.
- The user right-clicks the pane → "Clone SSH session". kettle splits
  the focused pane and runs `ssh me@box` in the new split (same target,
  not a `tmux attach` to the remote — kettle's `--remote-send` IPC
  remains the way to send commands TO the remote, not a clone-by-IPC).
- The user runs `docker exec -it some-container bash`. Title updates to
  `docker: some-container`. Clone target is the same container.

## Why multiple phases

Three cross-cutting changes:

1. **PID plumbing**. `kettle_core::Terminal` doesn't currently expose the
   PTY child PID. `portable_pty::Child::process_id() -> Option<u32>` is
   available on Unix; on Windows `Child::process_id()` returns the same.
   Need a new `pub fn child_pid(&self) -> Option<u32>` on `Terminal`
   that locks the child mutex and reads it. Tests-clean: doesn't break
   any current callers.

2. **Process-tree walking**. Reading the foreground process from the PTY
   side requires `tcgetpgrp(pty_master_fd)` on Unix. portable_pty doesn't
   expose the master fd directly but has the `pair.master` handle, which
   is `Box<dyn MasterPty>`. We may need to plumb `AsRawFd` (Unix) through
   a new trait method, OR walk `/proc/<child_pid>/task/<tid>/children`
   on Linux. macOS would use `proc_pidinfo` / `libproc::libproc` (extra
   dep). Windows would use `QueryFullProcessImageNameW` for each child.

   Decision: avoid platform-specific code where possible. Use the
   `sysinfo` crate (~190 stars, well-maintained, cross-platform). It
   handles process enumeration + parent-of relations transparently. Cost:
   one direct dep, modest compile-time hit.

3. **Title update path**. kettle already supports per-pane title via the
   OSC 0/2 path + the `EditPaneTitle` action + the
   per-pane titlebar (Bucket D). Remote detection just needs to set the
   pane's title programmatically when the detected remote-string
   changes. The existing `pane.title` field is the right home; we add a
   companion `pane.remote_context: Option<RemoteContext>` for the
   Clone-session action to consume.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::app::App                                                 │
│                                                                      │
│  per-tick (app poll cadence, ~10 Hz):                                │
│    for pane in self.mux.all_panes_mut():                             │
│      detect_remote(pane, &mut self.sysinfo_system) → Option<RC>      │
│      if pane.remote_context != detected_rc:                          │
│        pane.title = format_remote_title(&detected_rc, &pane.title)   │
│        pane.remote_context = detected_rc                             │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_remote::detect (NEW crate, no UI deps)                        │
│                                                                      │
│  detect_remote(child_pid: u32, sys: &mut sysinfo::System) ->         │
│      Option<RemoteContext>                                           │
│                                                                      │
│  1. Walk descendants of child_pid via sysinfo                        │
│  2. For each, check argv against detectors:                          │
│       SshSession::matches(&proc) ── argv[0] in {ssh, sshpass, …}     │
│       ContainerSession::matches(&proc) ── argv[0..2] in              │
│           {docker exec, podman exec, kubectl exec, …}                │
│  3. Return the first match (closest descendant wins)                 │
│                                                                      │
│  Pure given (child_pid, sys snapshot) — unit-testable with fake      │
│  sysinfo data via the sysinfo::Pid abstraction.                      │
└──────────────────────────────────────────────────────────────────────┘
```

`kettle_remote` is a new minor crate, not embedded in kettle-core, so
the heavy sysinfo dep doesn't propagate to non-UI consumers (the headless
`--screenshot` path, the `--check-config` validator).

`RemoteContext` enum:

```rust
pub enum RemoteContext {
    Ssh { host: String, user: Option<String> },
    Container { runtime: ContainerRuntime, container: String },
}

pub enum ContainerRuntime { Docker, Podman, Kubectl, Lxc }
```

## Phase roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `Terminal::child_pid()` accessor | Unit test on a real Terminal::new'd pair |
| 2 | `kettle_remote` crate skeleton + `RemoteContext` enum + `detect_remote()` stub | Pure unit tests with synthetic sysinfo input |
| 3 | SSH detector (`SshSession`) — argv match + host extraction | Drift guards on common argv shapes (`ssh box`, `ssh -p22 user@box`, `sshpass -p ssh box`, etc.) |
| 4 | Container detector (Docker / Podman / kubectl / lxc) | Drift guards on `docker exec -it foo bash`, `podman exec -i bar sh`, etc. |
| 5 | App-level poll loop (~10 Hz, tied to the app's periodic poll tick) + `pane.remote_context` field + title update | E2E: spawn a `sleep`, then `ssh-as-sleep` (a shell script that exec's `sleep` named `ssh`), verify title flip |
| 6 | Right-click "Clone session" `ContextMenuItem` variant + dispatch (splits the focused pane + spawns the detected argv) | Drift guard on `ContextMenuItem::CloneRemoteSession`; manual e2e |
| 7 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +12-15 (the detect_remote happy/edge cases).

## What WON'T ship in v1

- **Host profile matching** (`[[[foo]]]\nprofile = foo_profile`). Terminator's
  per-host profile-override grammar is mostly used by power users with
  many SSH targets. kettle ships profiles but the per-host
  binding is a Bucket E for v1 — users can simulate via the
  `kettle.on('remote_detect', fn)` Lua hook (added in phase 5 as a
  natural extension of the `kettle.on` event-hook surface).
- **Activity-watch integration**. Terminator's `RemoteProcWatch` ties
  remote-detect to activitywatch.py. kettle's activity dot
  is per-pane already; no extra coupling needed.

## Acceptance test

A user does:

```
$ kettle
# in pane 1:
$ ssh me@box.example.com
# wait 2 seconds
# verify: pane title shows "ssh me@box.example.com"
# right-click pane: verify "Clone SSH session" menu item appears
# click it: verify a new split spawns with the same ssh argv
```

When the SSH session exits and the prompt returns:

```
# verify: pane title reverts to "<shell>" (no detected remote)
# right-click: verify "Clone SSH session" is gone
```

Plus the same test sequence with `docker exec -it some-name bash`
substituted for the SSH command.

## Risks + mitigations

- **Risk:** sysinfo dep bloat. **Mitigation:** isolate in a new
  `kettle_remote` crate so non-UI consumers don't pay the cost. The
  crate has a feature flag to disable detection entirely (returns
  `None` from `detect_remote`) for headless / CI builds.
- **Risk:** polling overhead. **Mitigation:** tie the poll to the
  app's existing periodic poll tick (~10 Hz) — already running, free
  cadence. sysinfo's `refresh_processes` is fast on Linux (<1 ms typical).
- **Risk:** false positives (user runs `vim ssh.txt`; argv[0] is `vim`
  so SSH detector won't fire — but `ssh-add` would). **Mitigation:**
  detector requires argv[0] == "ssh" exactly (not "ssh*"), and
  Container detector requires the full `<runtime> exec` prefix.
- **Risk:** Windows PTY child PID. **Mitigation:** portable_pty's
  Child::process_id() returns the Win32 PID; sysinfo handles Windows.
  Container detection on Windows excludes lxc/podman by default.
