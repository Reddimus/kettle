# Deferred audit work

Open work checked against this repository snapshot; completed audit history is omitted.
Each entry names the gap, its source, the reason for deferral, and the closing evidence.

## Security and boundaries

- **D-Bus maintenance notices.** The product graph accepts the informational
  unmaintained notices RUSTSEC-2026-0370/0371/0372 for `zvariant 5.15.1`,
  `zvariant_derive 5.15.1` and `zbus_names 4.3.5`. The all-target exact reverse
  graph and registry sources are guarded by `scripts/check-dbus-scope.py`;
  `cargo audit --deny warnings` remains enforced for every other notice and
  vulnerability. Reassess these exceptions when the guarded Linux desktop
  integration dependencies change. The vendor-validation graph has no waiver.
  Moving to zbus 6 is blocked upstream. Checked 2026-11-20: zbus is at 6.4.0
  and no longer depends on the three crates, but the newest `accesskit_unix`
  (0.24.1, also AccessKit's `main`) requires zbus 5.19 and `atspi` 0.29, and
  `atspi` 0.30 (also its `main`) still requires zbus 5; neither project has an
  open change that moves. `notify-rust`, the third consumer in the audited
  all-target graph (`Cargo.lock` has 4.18.2), builds on zbus 6 from 4.19.0
  (2026-11-08). Moving Kettle's own zbus or `notify-rust` alone would add a
  second zbus and leave all three crates in the graph through
  `accesskit_unix`, so it closes nothing. Close when an `accesskit_unix`
  release builds on zbus 6: move it, `notify-rust` and Kettle's own zbus
  together, and once the three crates are gone from the audited graph remove
  the three audit ignores and `scripts/check-dbus-scope.py`.

- **Upstream `lru` advisory.** `Cargo.lock` still pins `lru 0.16.4` through
  `glyphon 0.12.0`; `deny.toml` ignores RUSTSEC-2026-0253. Deferred because the
  reviewed glyphon path does not call the affected `pop()` method, and a fork
  adds supply-chain surface. Close #207 when upstream accepts `lru >=0.18.2`;
  remove the audit ignores and `scripts/check-lru-scope.py` together.
  Checked 2026-11-20: glyphon 0.12.0 is still the newest release, its `main`
  still requires `lru 0.16.2`, and the change that would move it
  (grovesNL/glyphon pull request 182, "Update `lru` to `0.18.2`", opened
  2026-08-12) is still open. `lru` itself is at 0.19.0.
- **`setsid()` descendants on macOS.** macOS has no child subreaper, so
  `kettle exec` and the in-process MCP `kettle_run` path cannot adopt
  descendants that leave the PTY session, and a timeout or cancellation can
  leave them running. Linux runs each MCP command under its own supervising
  `kettle exec` child, whose subreaper stops escaped descendants on timeout,
  cancellation and termination signals (`crates/kettle/src/exec_supervisor.rs`;
  native tests in `crates/kettle/tests/mcp_supervisor.rs`). On every platform a
  normal exit deliberately leaves detached daemons running.

  Investigated on 2026-11-20 on macOS 27.3 (arm64, 18 cores) and in XNU
  releases from xnu-7195 (macOS 11) on. No supervisor was added: the tests
  on that one machine and the reading of those sources did not find a
  mechanism that gives an unprivileged Kettle reliable ownership of escaped
  descendants across the macOS versions it supports without changing what
  the command may do. Each one examined infers ancestry in a race with the
  scheduler, rests on a mark the descendant can drop, needs a privilege or
  entitlement Kettle does not have, or changes the context a command runs
  in. A Seatbelt profile, the last of these, is the one that meets the
  ownership test; it is described below with the reason it was not shipped.

  A kqueue fork event names only the process that forked. `EVFILT_PROC` refuses
  `NOTE_TRACK` with `ENOTSUP`, and a `NOTE_FORK` event carries no child pid
  (`data` is 0), so the supervisor has to find the child itself, with
  `proc_listchildpids` or by parent link.

  Parent links outlive the parent only in part. `proc_pidinfo` flavor 17
  (`PROC_PIDUNIQIDENTIFIERINFO`, absent from the SDK headers) answers for an
  unreaped zombie when its `arg` is 1, and its `p_puniqueid` keeps the
  parent's unique id when a process is reparented to launchd, but an orphan
  that then calls `exec` reports launchd's id instead (seen on 27.3; that
  code is in the sources from xnu-8792, macOS 13). `p_orig_ppidversion`
  survives `exec`, but exists only from xnu-11417.101 (macOS 15.4), and
  Kettle supports macOS 11. Either link helps only if the supervisor read
  the matching identity of the parent before the parent was reaped.

  A prototype timed an interval that contains that window and counted the
  grandchildren it found. Each trial had its own kqueue. For every process it
  tracked, the prototype tried to register `NOTE_FORK`, `NOTE_EXEC` and
  `NOTE_EXIT` and listed the children at once and again on each fork event;
  each exit event swept all listed processes by parent link, and so did the
  end of a trial whose grandchild was still untracked. The root, spawned
  without waiting to be registered, forked a child that forked again and
  exited at once. The grandchild called `setsid()`, waited until the root had
  reaped the child, reported its pid and then called `exec` on `/bin/sleep`; a
  trial counted as found when that pid was tracked by its end. From just
  before the root's `fork` to the return of its `waitpid` took at least 644
  us, with medians of 913 to 1,479 us per batch of 500. At the nonzero
  settings one sleep was requested before the child scan of each fork event
  the kqueue delivered. With both links the prototype missed 0, 0, 66, 430 and
  500 of 500 grandchildren at requested sleeps of 0, 0.25, 0.5, 1 and 2 ms;
  with the unique id alone it missed 0, 0, 97, 355 and 500. Two later batches
  of 3,000 with no sleep requested, one per setting, missed none; one and two
  of their grandchildren were found only by the closing sweep. An earlier
  version (unique id alone, zombies not read, a root that waited to be
  registered and a grandchild that did not call `exec`) ran beside 54 busy
  processes on the 18 cores with no sleep added: it missed 0 of 1,000 with no
  QoS override and 88 of 500 when its thread requested background QoS.

  Inherited marks do not replace the link. A marker in the environment could
  not be read back through `KERN_PROCARGS2` once the descendant ran
  `/bin/sh` or `/bin/sleep`, as XNU's default policy on a release kernel
  with SIP on predicts: it withholds the environment of another
  code-signing-restricted process from an ordinary caller.

  An inherited descriptor is a per-command mark that the descendant can
  drop. A probe gave the root the write end of a pipe and a PTY slave, then
  scanned the descriptor list of every other process it could read
  (`PROC_PIDLISTFDS` and `proc_pidfdinfo`) for either. Each scan read the
  lists of about 700 same-user processes, and of no other user's, in 11 to
  40 ms. It found the grandchild after the double fork, `setsid()`,
  reparenting and `exec` of `/bin/sleep`. It found neither mark on a
  grandchild that first closed descriptors 0 to 4,095 (its list came back
  empty), or on one that pointed descriptors 0 to 2 at `/dev/null` and
  closed 3 to 4,095, as daemons commonly do (its three entries held
  neither). The scan is also a poll, and a descriptor can be
  passed to an unrelated process. It was not shipped as a partial
  supervisor.

  Two kernel-kept ids are inherited, but by default neither is per command.
  Spawned ordinarily, the supervisor, the root and the grandchild (after the
  double fork, `setsid()`, reparenting and `exec`) all reported the terminal
  app that started them as their responsible process, as any other command
  started from that app would by inheritance. A root spawned with the private
  disclaim attribute (`responsibility_spawnattrs_setdisclaim`) became its own
  responsible process. While it lived, the grandchild reported the root's pid
  after the same steps, and a scan of all processes matched those two and no
  others. That is still not ownership. Once the root was killed, before it was
  even reaped, the grandchild reported its own pid. When the grandchild
  spawned `/bin/sleep` with the same attribute, the new process reported its
  own pid at every sample, the root's at none. Members would be found by
  polling and signalled one at a time. The call is private, and changing the
  responsible process can change which app the system's privacy (TCC)
  decisions are attributed to; the probe ran nothing that TCC protects.

  The coalition ids (`proc_pidinfo` flavor 20) of a probe and of its
  grandchild after the double fork, `setsid()`, reparenting and `exec` were
  the same. Kettle cannot make a per-command coalition: creating one needs
  membership of a privileged coalition, and naming one in `posix_spawn` needs
  that or a private entitlement. A launchd job is the untried route to one
  (in a snapshot, this user's 335 processes with parent pid 1, adopted
  orphans among them, held 272 distinct resource coalition ids); the command
  would then be started by launchd, not by `kettle exec`, with a different
  parent and launch context. It would also not inherit Kettle's descriptors:
  launchd opens a job's standard streams from paths in the job definition,
  so the PTY slave would have to be reopened by name, and the command's
  other inherited descriptors could not be passed at all, an independent
  reason this route does not fit. Kettle could not use the
  coalition terminate request to stop the members. That request is privileged,
  and in the XNU sources it does not signal the members: it marks the
  coalition and notifies when it is empty. Kettle would scan every process for
  the id and signal pids one at a time, in a race with their forks. Whether
  launchd stops every member when the job is removed was not tested.

  Making a new audit session with `setaudit_addr` needs root. `setsid()`
  itself takes the caller out of the session, process group and
  controlling terminal Kettle gave it.

  Endpoint Security delivers fork events from the kernel, to entitled
  clients only. Its system-wide client (`es_new_client`) must also run as
  root. From macOS 27.0 the SDK has `es_new_descendants_client`, whose
  header says it covers the caller's whole descendant subtree and needs
  neither root nor TCC approval. Both need the restricted
  `com.apple.developer.endpoint-security.client` entitlement, which Kettle
  does not hold: called without it here, the descendants client returned
  `ES_NEW_CLIENT_RESULT_ERR_NOT_ENTITLED`. Only that refusal was tested.
  Whether an entitled client still reports a descendant after `setsid()`
  and reparenting, and whether Kettle could stop the subtree safely from
  its events, is not known. Its header also says the caller and its
  descendants cannot then run setuid or setgid binaries unless the caller
  is root, and macOS 11 to 26 would stay uncovered.

  `ptrace` is not a route. Darwin's request list has none that reports a
  traced process's forks or follows its children (`sys/ptrace.h`), and
  tracing changes how the command receives signals. Two `PT_ATTACHEXC`
  attempts on same-user children, one started to run `/bin/sleep` and one
  the ad hoc signed probe binary, both returned `EPERM` here.

  A Seatbelt profile is the one mark examined that a descendant cannot drop.
  A root applied `(version 1)(allow default)` plus one per-command deny rule
  with `sandbox_init`, then forked a child that forked a grandchild, which
  called `setsid()` at once and ran `exec` on `/bin/sleep`. The profile is
  inherited at fork, so there was no window to race: once the root was
  reaped and the grandchild reparented to launchd, none of 1,000 such
  grandchildren had lost it (none of 500 with the file rule below). A member
  cannot remove its profile or apply another: `sandbox_init` inside one
  failed with `EPERM`. Applying a profile identical to the one in force
  succeeded (`sandbox-exec` of the same mark inside the mark exited 0);
  any different profile, including a plain `(allow default)`, failed. An
  unsandboxed, unprivileged supervisor can ask which processes carry the
  rule with `sandbox_check`, which is private API. Only macOS 27.3 was
  tested.

  The rule has to be chosen with care. A rule that denies reads of one
  existing per-command file fails twice. Other processes' own sandboxes and
  root daemons were denied that path as well (386 of 872 processes beside
  the member, for a file under `/private/tmp`, among them system XPC
  services and browser renderers), so the answer alone would name
  processes outside the command. And a member could drop the mark: it
  deleted the file; with writes under the file's directory denied, renaming
  that directory was refused but renaming its parent succeeded; and with
  the path gone every process was denied it. A rule that denies
  `mach-lookup` of a per-command global name leaves no file to remove.
  Checked together with a control name that the profile allows, it named
  exactly the command's grandchild among 876 processes, in 2 to 3 ms per
  scan (460 others were denied the per-command name by their own profiles
  and were rejected by the control name), and told two concurrent commands
  apart. A teardown that stopped the members one scan found, scanned again
  and killed those still found stopped one command's grandchild and left
  the other's running. Teardown is still a scan, stop and rescan loop
  rather than kernel-delivered events, with a pid-reuse window between a
  check and its signal. A same-user process that applied the same rule to
  itself would be counted as a member, and processes that launchd starts
  on the command's behalf are outside the profile, as they are not
  descendants.

  The cost is why it is not shipped, and it has two parts. First, inside
  the profile a nested
  `sandbox-exec -p '(version 1)(allow default)' /usr/bin/true` exited 71
  with `sandbox-exec: sandbox_apply: Operation not permitted`. Every command
  that applies a Seatbelt profile of its own, which in practice always
  differs from the per-command mark, would fail under `kettle exec`: this
  repository's documented `sandbox-exec ... cargo test` run
  (`docs/TESTING.md`), build and agent tools that sandbox their own
  subprocesses, a nested `kettle exec`, whose per-command mark differs, and
  a nested Kettle's media worker, which could not confine itself. When
  `kettle exec` itself runs in a sandbox, it cannot apply the mark at all.
  Second, inside the profile `exec` of a set-user-id binary fails with
  `EPERM`, as `docs/TESTING.md` already notes for `ps`: `sandbox-exec` of
  `/bin/ps`, `/usr/bin/sudo` and `/usr/bin/top` under the mark each printed
  `execvp() ... failed: Operation not permitted` and exited 71, while
  `/bin/ps` ran outside the sandbox. `su`, `login`, `at` and `crontab` are
  also set-user-id root on macOS 27.3, so any command that uses `sudo`,
  `ps` or `top` would break. An exception such as
  `(allow process-exec (literal "/bin/ps") (with no-sandbox))` let `ps` run
  under the mark, but only by running it outside the profile, which drops
  the mark for that program and anything it starts. Nothing else such a
  profile may change for a command was tested. A profile could also refuse
  `setsid()` outright, as the media worker's does, but that breaks
  legitimate daemons. This is the route to choose, perhaps as an opt-in, if
  breaking nested sandboxes and set-user-id programs is an acceptable price
  for macOS containment; that is the owner's call.

  Deferred until Kettle can keep kernel ownership of each command's escaped
  descendants, ownership they cannot drop and that holds after the
  command's root is gone, and can stop them safely on timeout or
  cancellation, without breaking commands that sandbox themselves or run
  set-user-id programs. On macOS 27 and later the Endpoint Security
  descendants client is the candidate if Apple grants Kettle the
  entitlement; nothing beyond its refusal has been tested. The Seatbelt
  mark above met the ownership test on the Mac tested but breaks nested
  sandboxes and set-user-id programs; shipping it, even as an opt-in, needs
  the owner to accept that. Close with native macOS timeout, cancellation
  and concurrent-command isolation tests, including a fixture
  that double-forks and calls `setsid()` at once. A normal exit keeps
  leaving detached daemons running.
- **D-Bus messages sized before they are read, on the accessibility
  connections.** `zbus` 5.19 resizes its receive buffer to a message's
  declared length (up to 128 MiB) before reading the body, and any peer on a
  bus can address a message to a connection's unique name. The connections
  Kettle opens itself (desktop settings, global shortcuts, notifications, the
  image viewer hand-off and showing a file in the file manager) now read
  through `crates/kettle-ui/src/dbus_bound.rs`, which checks the 16-byte
  header first and drops a message longer than 64 KiB from a 4 KiB scratch
  buffer (one past the specification's 128 MiB still ends the connection),
  so those buffer at most 64 KiB of wire bytes a message and 4 MiB for a
  stream that queues zbus's default 64 messages; what a message decodes into
  is bounded by the checks each caller makes after it arrives. What remains is the session-bus and
  accessibility-bus connections that `accesskit_unix` 0.24.1 opens for the
  screen-reader tree: it builds them inside the crate with zbus's own
  transport, so Kettle cannot put a reader in front of them without
  vendoring `accesskit_unix` or zbus. Checked 2026-11-20: zbus 6.4.0 still
  allocates a long message's declared length up front
  (`src/runtime/io/reader.rs`) and has no receive limit to configure, and
  `accesskit_unix` exposes no way to supply the connection. Close when zbus
  reads against a configurable limit and `accesskit_unix` uses it, or when
  `accesskit_unix` accepts a caller's connection.

- **Paths and commands in log lines.** The change for code scanning alert #46
  (`rust/cleartext-logging`, `crates/kettle-ui/src/app/automation.rs`) keeps
  config file content out of the config and trigger warnings; path and command
  text elsewhere was left as it is. Checked 2026-11-20: 62 log calls outside
  tests print a path with `.display()` (36 in `kettle-ui`, 12 in
  `kettle-render`, 5 in `kettle-config`, 9 in four other crates; 11 of the 62
  at `debug`), and the trusted-directory and private-file errors from
  `kettle-state` that startup, reload and `init.lua` loading log name each
  directory they refused. A path under the home directory carries the account
  name. Five calls print a command: the configured `custom-url-handler` when
  it fails to start or its child cannot be reaped (two in `app/links.rs`, one
  in `app/system_opener.rs`), the argv of a shell tab that fails to open
  (`app/actions.rs`), and, only at `info`, the command an agent sends through
  `run_command` (`app/ctl.rs`; its `send_text` neighbor logs a byte count).
  Logs go to stderr at `warn` by default, where a service manager or a pasted
  bug report can keep them. Deferred because the repository has no path form
  for logs (`abbreviate_home` in `kettle-ui` serves tab titles), a warning
  that cannot say which file it refused is hard to act on, and the choice
  between `~`, the file name alone and an opt-in full path belongs to one
  change across every crate. Close with one shared helper, those calls and the
  `kettle-state` error texts moved to it, the five command lines reduced to a
  fixed reason or a byte count, and a test per crate that captures the log and
  finds neither a home directory nor a command in it.

- **Windows metadata-only handles survive legacy hardening.**
  `open_existing_private_file` in `crates/kettle-state/src/private.rs`
  hardens a broad legacy file while holding it with no sharing, which refuses
  the open while any handle with data or delete access survives. Windows does
  not apply sharing to a handle holding only rights such as `READ_CONTROL`,
  `WRITE_DAC` or `WRITE_OWNER`, so a principal the old DACL granted one of
  those can keep such a handle across the rewrite and change the permissions
  afterwards. Found on 2026-11-20 in review of the conditional `WRITE_OWNER`
  request; it applies to every legacy file hardened before that change too.
  Deferred because no user-mode call revokes another process's handle and
  enumerating system handles needs privileged, undocumented interfaces. Close
  with a hardening path that does not rely on the sharing check, such as
  copying the data into a newly created private file and replacing the
  original through a held handle, with a Windows test that keeps a
  `WRITE_DAC`-only handle open across the call.

## Terminal and protocol

- **Extractor policy hooks for the streaming control kernel.**
  Exec/MCP and session logs now share `kettle-vt::control` and its stripper,
  with frozen differential oracles and a state/event conformance matrix.
  `Extractor` and `PrivateOutputFilter` retain their bulk scanners: C1 refusal,
  high-byte escape prefixes, private non-ST ESC handling, and recovery after
  a whole over-cap bulk run differ from the stripper. The private recovery
  boundary depends on feed chunking, so a byte-only transition replacement
  changes existing output. Close with run-aware policy hooks and a frozen
  extractor oracle comparing emitted chunks exactly across existing vectors,
  edge splits and random bytes.
- **`kettle exec` JSON exit event after a late stop.** Once the final
  `Finish` event is queued, a timeout or cancellation can still pick a stop
  status while the blocking final write is in progress
  (`crates/kettle/src/exec.rs`, `WorkerOutput`), so a complete exit event can
  report the child's status while the process returns 124/130/74. Deferred
  because a blocking `write` to an inherited stdout cannot be cancelled
  portably: non-blocking mode is shared with every holder of the pipe, and
  reopening it as a separate description works only on Linux. Close with a
  cancellable writer (bounded writes after readiness on a descriptor Kettle
  owns) and a JSON regression in which the event and the returned status
  agree under a blocked stdout. The blocked-writer characterization test
  preserves the current child-status event and later timeout-status result;
  changing that contract remains deferred.
- **Activation claimed just before its confirmation times out.** When a
  launch hands its request to a running window, the window can claim it just
  as the launcher's five-second wait ends; the launcher then treats the
  primary as busy and opens standalone, so two windows appear
  (`crates/kettle-ui/src/activation_server.rs`, `crates/kettle-ctl/src/activation.rs`).
  An "in progress" reply would fix new launchers, but launchers already
  installed stop reading after five seconds and cannot decode it. Deferred
  for a protocol decision: acknowledge the claim immediately (and give up the
  completion guarantee), or accept a rare duplicate window from old launchers.
  The claim-before-confirmation-timeout characterization test retains this
  behavior without treating a claim as completed activation.
- **Intentional DEC and xterm policy exceptions.** The implemented margins,
  protection, rectangular operations, column editing, reverse wrap, attributes,
  mode save/restore, special colours, screen reverse and software keyboard
  modes have portable and native conformance coverage. The remaining 67
  esctest2 cases are deliberate policies, listed individually in
  `scripts/conformance-esctest-expected.txt`: program-driven geometry and
  title operations are disabled, identity remains Kettle, memory checksums and
  physical DEC keyboard metadata are refused, and printer formatting, legacy
  Hebrew/NRCS and smooth scrolling are fixed inactive. The seven X colour
  syntaxes use fixed D65/sRGB conversion, physical chromaticity validation,
  channel clipping and a continuous CIE Lab low-light inverse; 21 suite
  colour-profile expectations remain exceptions. Changing any policy requires
  a product decision and matching tests. Seven-bit DECSCL is implemented;
  raw eight-bit C1 replies remain unsupported without changing selected state.
  The 41 additional upstream known-bug results are accounted for separately
  in `docs/TESTING.md`; they include skipped profiles and compatibility
  exceptions and are not conformance passes.

## Performance

- **Menu-hover redraws still read placement grids.** `crates/kettle-ui/src/app/redraw.rs`
  collects Kitty placeholder and relative placements before deciding to reuse
  pane snapshots, so a menu highlight with virtual placements visible takes the
  terminal lock and scans the grid again. Deferred until a profile shows the cost:
  the fix caches placement projections with the snapshot generation.
- **Synchronous X11 pointer queries during tab drags.** `crates/kettle-ui/src/app/windowing.rs`
  queries pointer position and buttons with blocking X11 round trips on the event
  loop while a tab is dragged, which a high-latency forwarded X connection turns
  into visible stalls. Deferred: the fix needs an asynchronous query on a
  dedicated connection; local X servers answer within the 16 ms tick.

The [2026-11-01 diagnostics](SCALING-2026-11-01.md) sampled PTY, window and
history scaling. Calibration refused the busy host; the partial 64-PTY
campaign and startup outlier remain inconclusive. These entries stay open.

- **PTY worker consolidation.** `crates/kettle-core/src/term.rs` uses a parser
  thread plus a blocking read pump per pane. Deferred pending large-pane-count
  profiling: queues are bounded and buffers recycled, while the split protects
  DEC 2026 deadlines from blocked reads. Close with measurements justifying the
  current design or a shared/native async reader that preserves deadlines,
  per-pane backpressure, Unix portability and ConPTY teardown.
- **Font sharing and lazy loading.** Each renderer still owns a `FontSystem` in
  `crates/kettle-render/src/lib.rs`; `crates/kettle-ui/src/font_preload.rs` now
  preloads the first window's fonts. Further sharing or lazy loading remains
  speculative. Close after profiling shows whether either change is worthwhile,
  then implement and measure any justified change.
- **Search traversal off the event loop.** `crates/kettle-ui/src/app/search.rs`
  still advances retained-history searches in bounded slices after an idle
  deadline. Deferred pending profiling. Close with evidence that these slices
  meet latency goals, or a worker that snapshots safely, releases terminal locks
  before UI work and rejects stale output/resize/query results. Preserve the
  core text/cell/haystack budgets and projection cap.

## Testing

- **Two macOS benchmark harness gaps.** `scripts/perf/macos-compare.sh` loses
  its cleanup path when AppleScript times out after creating a window for an
  already-running app, and `scripts/perf/macos-standing/latency-probe.swift`
  checks focus only before and after a sample, so a focus round trip in between
  passes. Both affect only owner-run measurements on a quiet machine. Close
  with a per-launch window identity recorded before the AppleScript reply, and
  an activation observer around each sample.

- **App testability.** `crates/kettle-ui/src/app.rs` is now split into
  subsystem modules under `app/`, but event handlers still act on `App` and
  `WindowState` directly, and many guards read source text. Deferred because
  typed per-event outcomes change behaviour-bearing code paths, unlike the
  pure-move split. Close by returning typed outcomes from the input, frame and
  lifecycle handlers, keeping application code thin, and replacing
  source-text guards with behavioural tests where an outcome makes them
  observable.
- **Native live UI cases outside CI.** The six formerly advisory Linux cases
  passed 300 repetitions each across three fresh allocations and now gate the
  required build job; see `docs/TESTING.md`. Other manual cases still need
  behavior-specific native evidence. Keep native-pointer tear/re-dock coverage
  in `scripts/check-tearoff-live-smoke.sh`; the Python `tearoff` case only tests
  the control action. Native macOS caption/modifier checks do not cover every
  live scenario.

## Platform

- **Native clipboard acquisition can exceed paste bounds.** Independent review
  of the clipboard fixes found that macOS `stringForType` materializes the
  pasteboard string before the size check (`vendor/arboard-3.6.1/src/platform/osx.rs`),
  and a delayed-rendering Windows owner can block synchronous `GetClipboardData`
  beyond the deadline (`vendor/arboard-3.6.1/src/platform/windows.rs`). Deferred
  because both require acquisition in a bounded helper process or thread that
  lets the UI stop waiting while preserving OSC 52 store/load ordering. Close
  with native tests for an oversized macOS string and a stalled Windows owner,
  proving bounded resource use and UI wait time without reordering OSC 52.

- **Windows MCP stdout progress.** `crates/kettle/src/mcp.rs` still uses a
  blocking writer on Windows. A slow reader can make progress without a write
  returning before the 30-second stall limit, so the server can misclassify
  it as stalled. Deferred because cancellable writes to inherited Windows
  pipe handles need a separate native design and validation. Close with a
  nonblocking or cancellable writer and native Windows tests for a one-byte
  slow reader and an unread pipe.

- **Native macOS search speech feedback.** Native Japanese Romaji composition,
  commit/cancel, caret/selection and terminal-input isolation have recorded
  desktop evidence in `docs/TESTING.md`. Spoken search-result and caret feedback
  still needs an identified assistive-technology observation. Keep appearance
  skips in `docs/APPEARANCE-GATE.md` with their own artifact scope.
