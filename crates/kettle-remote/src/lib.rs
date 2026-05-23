//! kettle-remote — SSH / Docker / Podman / kubectl session detection.
//!
//! Cycle 643 (sub-cycle 2 of
//! [`TERMINATOR-REMOTE-DESIGN.md`](../../../docs/TERMINATOR-REMOTE-DESIGN.md)):
//! crate skeleton + `RemoteContext` type + `detect_remote` stub.
//!
//! Sub-cycle ledger (closed):
//!
//! - Cycle 644 (sub-cycle 3) — SSH detector
//!   (`detect_ssh` covering 11 argv shapes; see `tests` module).
//! - Cycle 645 (sub-cycle 4) — Container detector
//!   (`detect_container` for Docker / Podman / kubectl / lxc;
//!   11 argv shapes).
//! - Cycle 646 (sub-cycle 5) — process-tree BFS via sysinfo
//!   (`detect_remote_with(child_pid, &mut System)`).
//! - Cycle 658 (sub-cycle 7) — `clone_session_command` +
//!   `clone_session_label` (Clone Session menu item).
//! - Cycle 720 (2026-05-23): re-wrote the original
//!   "sub-cycle 3 *will* ship" forward-looking comments now that
//!   the foundations all landed at cycles 644-658.

#![forbid(unsafe_code)]

/// Cycle 656: re-export `sysinfo::System` so kettle-ui can own one
/// (and pass it to `detect_remote_with`) without pulling sysinfo
/// in as a direct dep. Keeps sysinfo a transitive-only dep that
/// kettle-ui doesn't need to track its version of.
pub use sysinfo::System as SysinfoSystem;

/// Cycle 643: a detected remote-session context.
///
/// Returned by [`detect_remote`] when the pane's process tree
/// contains a recognized remote-client process (`ssh`, `docker
/// exec`, `podman exec`, `kubectl exec`, `lxc-attach`).
///
/// Drives the cycle-647-target right-click "Clone session" menu
/// item and the pane-title update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteContext {
    /// SSH session. `host` is the target host (e.g. `box.example.com`);
    /// `user` is the optional username if the argv had `user@host`.
    Ssh { host: String, user: Option<String> },
    /// Container session (Docker / Podman / kubectl exec / lxc-attach).
    /// `container` is the target name/id from the argv.
    Container {
        runtime: ContainerRuntime,
        container: String,
    },
}

/// Cycle 643: which container runtime the detected `docker exec` /
/// `podman exec` / `kubectl exec` / `lxc-attach` command is using.
/// Drives the cycle-647-target "Clone session" command construction
/// (matches the same argv shape for the new pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
    Kubectl,
    Lxc,
}

/// Cycle 730: process-tree abstraction so the BFS body of
/// [`detect_remote_with`] is testable against a synthetic fixture
/// instead of needing real OS processes.
///
/// Pre-729, the BFS read `sysinfo::System` directly; the only test
/// that could exist was the `detect_remote_returns_none_for_invalid_pids`
/// smoke (`detect_remote(0).is_none()`). Two-hop ssh, depth-3
/// container, closer-wins-on-tie — none of those could be unit-
/// tested without spawning real ssh / docker processes from CI,
/// which the cycle-646 author specifically called out as too
/// fragile (see the comment on that test).
///
/// Implementations:
/// - [`sysinfo::System`](https://docs.rs/sysinfo) — built-in via
///   the impl below; used by [`detect_remote_with`].
/// - `tests::MockProcessTree` — `#[cfg(test)]`-only fixture in the
///   test module; powers the 8 cycle-730 BFS tests.
///
/// The trait is intentionally minimal: four read-only methods +
/// one `refresh`, all `u32`-pid typed (no `sysinfo::Pid` leak).
/// External implementations are unusual but supported — a future
/// e.g. `/proc`-only or seccomp-restricted impl would slot in
/// here without changing kettle-remote's API.
pub trait ProcessTree {
    /// Refresh the snapshot. The BFS calls this once at the top of
    /// each detection pass so a single-threaded poll loop sees a
    /// consistent process map. For sysinfo this re-reads the OS;
    /// for a fixture this is a no-op.
    fn refresh(&mut self);
    /// Parent PID of `pid`, or `None` if `pid` is missing / has no
    /// parent recorded (top-level / scheduler).
    fn parent_of(&self, pid: u32) -> Option<u32>;
    /// Argv of `pid` as lossy UTF-8 strings, or `None` if the
    /// process is gone or never existed. Lossy conversion mirrors
    /// the pre-729 `to_string_lossy` behavior — non-UTF8 argv is
    /// exotic and the detectors only care about `argv[0]` + flags
    /// which are always ASCII in practice.
    fn argv_of(&self, pid: u32) -> Option<Vec<String>>;
    /// All known PIDs in the current snapshot. Used to build the
    /// children-by-parent map exactly once per detection pass.
    fn all_pids(&self) -> Vec<u32>;
}

/// Cycle 730: the production `ProcessTree` impl. Wraps sysinfo's
/// cmd-refresh + `processes()` map behind the trait's u32-pid API.
///
/// The refresh strategy matches the pre-729 in-line code: cmd-only
/// refresh (not memory / disk / network), all PIDs, full refresh
/// of any that disappeared. sysinfo's internal cache makes this
/// cheap on the second + later calls (~hundreds of µs on a typical
/// 200-process machine).
impl ProcessTree for sysinfo::System {
    fn refresh(&mut self) {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
        let refresh_kind = ProcessRefreshKind::new().with_cmd(sysinfo::UpdateKind::Always);
        self.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
    }

    fn parent_of(&self, pid: u32) -> Option<u32> {
        self.process(sysinfo::Pid::from_u32(pid))
            .and_then(|p| p.parent())
            .map(|p| p.as_u32())
    }

    fn argv_of(&self, pid: u32) -> Option<Vec<String>> {
        // Lossy conversion is fine — non-UTF8 argv is exotic and
        // would still be detected at the exe-name level (the
        // detectors only inspect argv[0]'s last path component +
        // ASCII flags).
        self.process(sysinfo::Pid::from_u32(pid)).map(|p| {
            p.cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect()
        })
    }

    fn all_pids(&self) -> Vec<u32> {
        self.processes().keys().map(|p| p.as_u32()).collect()
    }
}

/// Cycle 646 (sub-cycle 5 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): detect a remote-
/// session context for the pane rooted at `child_pid`.
///
/// Walks the process tree starting from `child_pid` (the shell
/// kettle spawned), looks at each descendant's argv, and returns
/// the first match from `detect_ssh` / `detect_container` (closest
/// descendant of `child_pid` wins on tie).
///
/// Returns `None` when:
///   - no descendant matches a known remote-client argv
///   - `child_pid` itself is gone (process exited)
///   - sysinfo can't enumerate processes (rare; permission denied
///     on hardened systems)
///
/// Allocates a fresh `sysinfo::System` per call. For app-loop use
/// (~10 Hz poll), prefer [`detect_remote_with`] which takes a
/// caller-owned `System` so the refreshes amortize.
pub fn detect_remote(child_pid: u32) -> Option<RemoteContext> {
    let mut sys = sysinfo::System::new();
    detect_remote_with(child_pid, &mut sys)
}

/// Cycle 646: same as [`detect_remote`] but reuses a caller-owned
/// `sysinfo::System`. The App's poll loop will own one of these
/// across ticks so the process-list refresh amortizes (sysinfo's
/// internal cache survives between calls).
///
/// Cycle 730: now a thin wrapper around the generic
/// `detect_in_tree` helper (private — see the cycle-730 doc on
/// `ProcessTree` for why the BFS body got extracted) so the
/// detection logic is testable. Signature preserved —
/// `kettle-ui::App` still passes `&mut self.remote_sysinfo` (a
/// `SysinfoSystem`) and gets back the same `Option<RemoteContext>`.
pub fn detect_remote_with(child_pid: u32, sys: &mut sysinfo::System) -> Option<RemoteContext> {
    detect_in_tree(child_pid, sys)
}

/// Cycle 730: generic BFS over any [`ProcessTree`]. Walks descendants
/// of `child_pid` breadth-first; closest descendants checked first
/// so a `bash → docker → ssh` tree resolves to the docker context
/// (the directly-spawned remote client), not the deeper-but-also-
/// matching ssh.
///
/// Private — the public entry points ([`detect_remote`] /
/// [`detect_remote_with`]) wrap it with a sysinfo `&mut System`.
/// External impls of `ProcessTree` can still call it (the trait is
/// `pub`), but routing through the sysinfo wrapper is the documented
/// path. Tests use this directly with `MockProcessTree`.
fn detect_in_tree<T: ProcessTree + ?Sized>(child_pid: u32, tree: &mut T) -> Option<RemoteContext> {
    tree.refresh();

    // Group children by parent so the BFS is O(N) snapshot + O(D)
    // walk where D is the depth of descendants. The pre-729 sysinfo
    // BFS used `sysinfo::Pid` keys; the trait abstraction is u32
    // so the same map structure works for both `sysinfo::System`
    // and `MockProcessTree`.
    let pids = tree.all_pids();
    let mut children_by_parent: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::with_capacity(pids.len());
    for pid in &pids {
        if let Some(parent) = tree.parent_of(*pid) {
            children_by_parent.entry(parent).or_default().push(*pid);
        }
    }

    // BFS from child_pid; closer descendants checked first. Loop
    // bound: each pid is enqueued ≤ 1 time (a Pid only has one
    // parent), so termination is guaranteed even on a cyclic
    // children_by_parent (which shouldn't happen but the bound
    // protects against a future fixture bug).
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    let mut visited: std::collections::HashSet<u32> =
        std::collections::HashSet::with_capacity(pids.len());
    if let Some(initial) = children_by_parent.get(&child_pid) {
        for &pid in initial {
            if visited.insert(pid) {
                queue.push_back(pid);
            }
        }
    }
    while let Some(pid) = queue.pop_front() {
        if let Some(argv) = tree.argv_of(pid) {
            if let Some(ctx) = detect_ssh(&argv) {
                return Some(ctx);
            }
            if let Some(ctx) = detect_container(&argv) {
                return Some(ctx);
            }
        }
        if let Some(grand) = children_by_parent.get(&pid) {
            for &gpid in grand {
                if visited.insert(gpid) {
                    queue.push_back(gpid);
                }
            }
        }
    }
    None
}

/// Cycle 644 (sub-cycle 3 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): SSH-session
/// detector. Takes a process's argv (as the sysinfo walk in
/// sub-cycle 5 will supply it) and returns `Some(Ssh { host, user })`
/// if the argv shape matches an `ssh` invocation, else `None`.
///
/// Recognized `argv[0]` values: `ssh`, `sshpass`. (`autossh` is a
/// reasonable extension; deferred to follow-up.)
///
/// Host extraction:
///   - `ssh host`                            → host=host, user=None
///   - `ssh user@host`                       → host=host, user=Some(user)
///   - `ssh -p 22 user@host`                 → same
///   - `ssh -o StrictHostKeyChecking=no host` → host=host
///   - `sshpass -p secret ssh user@host`     → host=host, user=Some(user)
///
/// The detector skips `-flag value` and `-flag=value` and `--flag=value`
/// prefixes to find the first non-option argv element. That element is
/// the target (potentially `user@host`).
///
/// Pure — takes a `&[String]` slice; unit-testable without spawning
/// anything.
pub fn detect_ssh(argv: &[String]) -> Option<RemoteContext> {
    let exe = argv.first()?.split('/').next_back().unwrap_or("");
    if exe != "ssh" && exe != "sshpass" {
        return None;
    }
    // sshpass wraps ssh — find the `ssh` inside its argv.
    let inner_start = if exe == "sshpass" {
        argv.iter()
            .position(|a| a == "ssh" || a.ends_with("/ssh"))?
            + 1
    } else {
        1
    };
    let mut i = inner_start;
    let mut target: Option<&str> = None;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(s) = a.strip_prefix("--")
            && s.contains('=')
        {
            i += 1;
            continue;
        }
        if let Some(s) = a.strip_prefix('-')
            && !s.is_empty()
        {
            // `-o foo=bar` / `-p 22` / `-l user` style: skip a value.
            let needs_value = matches!(
                s,
                "o" | "p" | "l" | "i" | "b" | "c" | "F" | "L" | "R" | "D" | "W"
            );
            if needs_value && i + 1 < argv.len() {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        target = Some(a.as_str());
        break;
    }
    let raw = target?;
    let (user, host) = match raw.split_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (Some(u.to_string()), h.to_string()),
        _ => (None, raw.to_string()),
    };
    if host.is_empty() {
        return None;
    }
    Some(RemoteContext::Ssh { host, user })
}

/// Cycle 645 (sub-cycle 4 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): Container-session
/// detector. Recognizes the four common container-exec argv shapes:
///
///   - `docker exec [-it] <container> <cmd> [args …]`
///   - `podman exec [-it] <container> <cmd> [args …]`
///   - `kubectl exec [-it] <pod> -- <cmd> [args …]`
///     (also `kubectl exec [-it] -n ns <pod> -- <cmd>`)
///   - `lxc-attach [-n] <container>`
///
/// The container token is the first non-option argv element after
/// the `exec` / `attach` subcommand (skipping `-flag value` pairs).
///
/// Pure — argv-in, `Option<RemoteContext>`-out. Unit-testable.
pub fn detect_container(argv: &[String]) -> Option<RemoteContext> {
    let exe = argv.first()?.split('/').next_back().unwrap_or("");
    let runtime = match exe {
        "docker" => ContainerRuntime::Docker,
        "podman" => ContainerRuntime::Podman,
        "kubectl" => ContainerRuntime::Kubectl,
        "lxc-attach" => ContainerRuntime::Lxc,
        _ => return None,
    };
    let mut i = 1; // skip argv[0] (the exe)
    if runtime != ContainerRuntime::Lxc {
        // Expect "exec" subcommand at argv[1].
        if argv.get(i).map(String::as_str) != Some("exec") {
            return None;
        }
        i += 1;
    }
    // Skip flags + their values. Container CLIs share the same
    // shape — `-it` is a stacked short-flag bundle (no value),
    // `-n ns` is a flag + value (kubectl namespace), `-u user`
    // is a flag + value (docker/podman user). Be conservative:
    // single-char flags with known value-taking ones get +=2;
    // bundled short-flags (`-it`, `-rm`) and `--flag=value` are +=1.
    //
    // Lxc special case: `lxc-attach -n NAME` is the *idiomatic*
    // form. The value of `-n` IS the container name — capture
    // it directly instead of skipping it.
    let needs_value = |s: &str| matches!(s, "n" | "u" | "c" | "w" | "e");
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            continue;
        }
        if let Some(stripped) = a.strip_prefix("--") {
            if stripped.contains('=') {
                i += 1;
            } else {
                // GNU-style --flag value — skip both.
                i += 2;
            }
            continue;
        }
        if let Some(stripped) = a.strip_prefix('-')
            && !stripped.is_empty()
        {
            // Lxc: -n VALUE is the container name.
            if runtime == ContainerRuntime::Lxc && stripped == "n" && i + 1 < argv.len() {
                return Some(RemoteContext::Container {
                    runtime,
                    container: argv[i + 1].clone(),
                });
            }
            let single_char_needs_value =
                stripped.len() == 1 && needs_value(stripped) && i + 1 < argv.len();
            if single_char_needs_value {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // First non-option positional. For Lxc the lxc-attach
        // form is `lxc-attach -n name`; without `-n` the first
        // positional IS the name. So either way, this is the
        // container token.
        return Some(RemoteContext::Container {
            runtime,
            container: a.clone(),
        });
    }
    None
}

/// Cycle 658 (sub-cycle 7 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): format a
/// `RemoteContext` as a shell command string the user can re-run.
/// Drives the right-click "Reconnect to …" / "Re-attach …" menu
/// entry — clicking writes this string to the focused pane's PTY
/// (one shell-line away from re-establishing the session).
///
/// - `Ssh { user: None, host: "box" }` → `"ssh box"`
/// - `Ssh { user: Some("me"), host: "box" }` → `"ssh me@box"`
/// - `Container { Docker, c }` → `"docker exec -it c $SHELL"`
/// - `Container { Kubectl, c }` → `"kubectl exec -it c -- $SHELL"`
///
/// Pure — no `&self`, no env. Unit-testable. The "$SHELL"
/// placeholder leaves shell-choice to the user's environment
/// (the running pane's shell resolves it at command time).
pub fn clone_session_command(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user } => match user {
            Some(u) => format!("ssh {u}@{host}"),
            None => format!("ssh {host}"),
        },
        RemoteContext::Container { runtime, container } => match runtime {
            ContainerRuntime::Docker => format!("docker exec -it {container} $SHELL"),
            ContainerRuntime::Podman => format!("podman exec -it {container} $SHELL"),
            ContainerRuntime::Kubectl => format!("kubectl exec -it {container} -- $SHELL"),
            ContainerRuntime::Lxc => format!("lxc-attach -n {container}"),
        },
    }
}

/// Cycle 658: short user-friendly label for the right-click menu
/// entry that reconnects to a detected remote session. The cycle-
/// 611 `ContextMenuItem::ConfigItem { label, command }` consumes
/// the pair `(clone_session_label(ctx), clone_session_command(ctx))`.
pub fn clone_session_label(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user } => match user {
            Some(u) => format!("Reconnect ssh {u}@{host}"),
            None => format!("Reconnect ssh {host}"),
        },
        RemoteContext::Container { runtime, container } => {
            let runtime_name = match runtime {
                ContainerRuntime::Docker => "docker",
                ContainerRuntime::Podman => "podman",
                ContainerRuntime::Kubectl => "kubectl",
                ContainerRuntime::Lxc => "lxc",
            };
            format!("Re-attach {runtime_name} {container}")
        }
    }
}

/// Cycle 643: format a `RemoteContext` as a one-line title string
/// for use in the pane-title surface (Terminator's pattern).
///
///   - `Ssh { user: None, host: "box" }`             → `"ssh box"`
///   - `Ssh { user: Some("me"), host: "box" }`       → `"ssh me@box"`
///   - `Container { runtime: Docker, container: c }` → `"docker: c"`
///   - `Container { runtime: Kubectl, container: c }` → `"kubectl: c"`
///
/// Pure — no `&self` parameter (the enum is the input + the format
/// is the output). Unit-testable without disk.
pub fn format_remote_title(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user } => match user {
            Some(u) => format!("ssh {u}@{host}"),
            None => format!("ssh {host}"),
        },
        RemoteContext::Container { runtime, container } => {
            let runtime_name = match runtime {
                ContainerRuntime::Docker => "docker",
                ContainerRuntime::Podman => "podman",
                ContainerRuntime::Kubectl => "kubectl",
                ContainerRuntime::Lxc => "lxc",
            };
            format!("{runtime_name}: {container}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycle 643 drift guard. `format_remote_title` is the pure
    /// formatter behind the per-pane title update path.
    #[test]
    fn format_remote_title_covers_ssh_and_container_shapes() {
        // SSH without user.
        assert_eq!(
            format_remote_title(&RemoteContext::Ssh {
                host: "box.example.com".to_string(),
                user: None,
            }),
            "ssh box.example.com"
        );
        // SSH with user.
        assert_eq!(
            format_remote_title(&RemoteContext::Ssh {
                host: "box".to_string(),
                user: Some("me".to_string()),
            }),
            "ssh me@box"
        );
        // Container — Docker.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu-2204".to_string(),
            }),
            "docker: ubuntu-2204"
        );
        // Container — Podman.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".to_string(),
            }),
            "podman: fedora"
        );
        // Container — kubectl.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod-deadbeef".to_string(),
            }),
            "kubectl: my-pod-deadbeef"
        );
        // Container — LXC.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".to_string(),
            }),
            "lxc: alpine"
        );
    }

    /// Cycle 658 drift guard. `clone_session_command` is the pure
    /// formatter for the right-click "Reconnect to …" menu entry's
    /// dispatched command. Sub-cycle 7 of remote.py design.
    #[test]
    fn clone_session_command_for_all_shapes() {
        // SSH without user.
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            }),
            "ssh box"
        );
        // SSH with user.
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            }),
            "ssh me@box"
        );
        // Docker.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu".into(),
            }),
            "docker exec -it ubuntu $SHELL"
        );
        // Podman.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".into(),
            }),
            "podman exec -it fedora $SHELL"
        );
        // Kubectl (note the `--` separator).
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            }),
            "kubectl exec -it my-pod -- $SHELL"
        );
        // LXC.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            }),
            "lxc-attach -n alpine"
        );
    }

    /// Cycle 658 drift guard: `clone_session_label` is the menu
    /// label paired with `clone_session_command`.
    #[test]
    fn clone_session_label_for_all_shapes() {
        assert_eq!(
            clone_session_label(&RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            }),
            "Reconnect ssh me@box"
        );
        assert_eq!(
            clone_session_label(&RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            }),
            "Reconnect ssh box"
        );
        assert_eq!(
            clone_session_label(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "foo".into(),
            }),
            "Re-attach docker foo"
        );
        assert_eq!(
            clone_session_label(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            }),
            "Re-attach kubectl my-pod"
        );
    }

    /// Cycle 646 drift guard: `detect_remote` returns None for
    /// pids that aren't real (or have no descendants matching a
    /// remote-client argv). Real-process testing isn't feasible
    /// here — we'd need to actually spawn ssh, which adds CI
    /// fragility. The argv-side detectors (detect_ssh +
    /// detect_container) get exhaustive coverage above; this
    /// test just locks the no-op no-match contract.
    #[test]
    fn detect_remote_returns_none_for_invalid_pids() {
        // PID 0 is the kernel scheduler on Linux — never has the
        // shape we're looking for. u32::MAX is reserved / unused.
        assert!(detect_remote(0).is_none());
        assert!(detect_remote(u32::MAX).is_none());
    }

    /// Cycle 645 drift guard. `detect_container` walks the four
    /// container-runtime argv shapes.
    #[test]
    fn detect_container_recognizes_docker_podman_kubectl_lxc() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // docker exec -it <container> bash
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "-it", "alpine", "bash"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "alpine".into(),
            })
        );
        // podman exec foo sh
        assert_eq!(
            detect_container(&argv(&["podman", "exec", "fedora", "sh"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".into(),
            })
        );
        // kubectl exec -it -n my-ns my-pod -- bash
        assert_eq!(
            detect_container(&argv(&[
                "kubectl", "exec", "-it", "-n", "my-ns", "my-pod", "--", "bash"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            })
        );
        // lxc-attach -n alpine
        assert_eq!(
            detect_container(&argv(&["lxc-attach", "-n", "alpine"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            })
        );
        // lxc-attach alpine (no -n)
        assert_eq!(
            detect_container(&argv(&["lxc-attach", "alpine"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            })
        );
        // Absolute path.
        assert_eq!(
            detect_container(&argv(&["/usr/bin/docker", "exec", "foo"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "foo".into(),
            })
        );
        // Non-container argv → None.
        assert!(detect_container(&argv(&["docker", "ps"])).is_none());
        assert!(detect_container(&argv(&["docker", "build", "."])).is_none());
        assert!(detect_container(&argv(&["bash"])).is_none());
        assert!(detect_container(&argv(&[])).is_none());
        // docker exec with no container arg → None.
        assert!(detect_container(&argv(&["docker", "exec"])).is_none());
        assert!(detect_container(&argv(&["docker", "exec", "-it"])).is_none());
    }

    /// Cycle 644 drift guard. `detect_ssh` walks argv shapes that
    /// match real-world ssh invocations.
    #[test]
    fn detect_ssh_recognizes_common_argv_shapes() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // ssh host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "box.example.com"])),
            Some(RemoteContext::Ssh {
                host: "box.example.com".into(),
                user: None,
            })
        );
        // ssh user@host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "me@box"])),
            Some(RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            })
        );
        // ssh -p 22 user@host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-p", "22", "alice@h.example"])),
            Some(RemoteContext::Ssh {
                host: "h.example".into(),
                user: Some("alice".into()),
            })
        );
        // ssh -o StrictHostKeyChecking=no host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-o", "StrictHostKeyChecking=no", "h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: None,
            })
        );
        // ssh -l user host (kettle-supplied user via -l)
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-l", "bob", "h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: None, // -l user goes into ssh-internal state; we don't extract it
            })
        );
        // sshpass -p secret ssh user@host
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-p", "secret", "ssh", "carol@h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: Some("carol".into()),
            })
        );
        // Absolute-path argv[0].
        assert_eq!(
            detect_ssh(&argv(&["/usr/bin/ssh", "box"])),
            Some(RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            })
        );
        // Non-SSH argv → None.
        assert!(detect_ssh(&argv(&["vim", "ssh.txt"])).is_none());
        assert!(detect_ssh(&argv(&["bash"])).is_none());
        assert!(detect_ssh(&argv(&[])).is_none());
        // ssh with no target (just flags) → None.
        assert!(detect_ssh(&argv(&["ssh", "-V"])).is_none());
    }

    // === Cycle 730: ProcessTree fixture + mocked BFS tests =========
    //
    // Pre-729 the only `detect_remote_with` test was the
    // `detect_remote_returns_none_for_invalid_pids` smoke (above)
    // — it called the real sysinfo against pid 0 / u32::MAX. The
    // BFS body (descendant walk, closer-wins-on-tie, refresh
    // contract) was untested because spawning real ssh from CI
    // is too fragile. Cycle 730 extracted [`ProcessTree`] so the
    // BFS body is now testable with a synthetic process tree.

    /// Cycle 730 fixture: a `ProcessTree` impl backed by a hashmap.
    /// `add(pid, parent, argv)` builds the tree; `ProcessTree` reads
    /// it. `refresh()` is a no-op (the fixture is already-built).
    struct MockProcessTree {
        procs: std::collections::HashMap<u32, MockProc>,
    }

    struct MockProc {
        parent: Option<u32>,
        argv: Vec<String>,
    }

    impl MockProcessTree {
        fn new() -> Self {
            Self {
                procs: std::collections::HashMap::new(),
            }
        }

        fn add(&mut self, pid: u32, parent: Option<u32>, argv: &[&str]) {
            self.procs.insert(
                pid,
                MockProc {
                    parent,
                    argv: argv.iter().map(|s| (*s).to_string()).collect(),
                },
            );
        }
    }

    impl ProcessTree for MockProcessTree {
        fn refresh(&mut self) {
            // Fixture is already-built; refresh is a no-op.
        }

        fn parent_of(&self, pid: u32) -> Option<u32> {
            self.procs.get(&pid).and_then(|p| p.parent)
        }

        fn argv_of(&self, pid: u32) -> Option<Vec<String>> {
            self.procs.get(&pid).map(|p| p.argv.clone())
        }

        fn all_pids(&self) -> Vec<u32> {
            self.procs.keys().copied().collect()
        }
    }

    /// Cycle 730 drift guard: ssh as a direct child of the pane's
    /// shell is the most common shape. `detect_in_tree` must reach
    /// it in a single BFS hop.
    #[test]
    fn detect_in_tree_direct_child_ssh() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(200, Some(100), &["ssh", "alice@server.example.com"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Ssh {
                host: "server.example.com".into(),
                user: Some("alice".into()),
            })
        );
    }

    /// Cycle 730 drift guard: `ssh-with-credentials` wrappers
    /// (e.g., `sshpass`, `assume-role`, corporate VPN wrappers)
    /// spawn ssh one or more levels deep. The BFS must walk past
    /// the non-matching intermediate process.
    #[test]
    fn detect_in_tree_two_hops_ssh_via_wrapper() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(150, Some(100), &["/usr/local/bin/run-with-creds"]);
        tree.add(200, Some(150), &["ssh", "bob@deep.example"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Ssh {
                host: "deep.example".into(),
                user: Some("bob".into()),
            })
        );
    }

    /// Cycle 730 drift guard: container exec at depth 3 (shell →
    /// tmux session → window → `docker exec`). Matches the
    /// terminator-parity assumption that a pane can have arbitrary-
    /// depth descendants.
    #[test]
    fn detect_in_tree_container_at_depth_3() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(150, Some(100), &["tmux", "new-session"]);
        tree.add(160, Some(150), &["zsh"]);
        tree.add(
            200,
            Some(160),
            &["docker", "exec", "-it", "ubuntu-2204", "bash"],
        );
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu-2204".into(),
            })
        );
    }

    /// Cycle 730 drift guard: when two descendants both match a
    /// remote-client argv, the *closer* (depth-1) wins over the
    /// deeper (depth-2). This pins the BFS-is-breadth-first
    /// contract — a future swap to DFS would silently change
    /// behavior on this tree, breaking which container the right-
    /// click "Reconnect" menu offers.
    #[test]
    fn detect_in_tree_closer_descendant_wins_on_tie() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(200, Some(100), &["docker", "exec", "near", "sh"]);
        tree.add(201, Some(200), &["ssh", "far.example"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "near".into(),
            })
        );
    }

    /// Cycle 730 drift guard: when `child_pid` has no entry in the
    /// tree AND nothing claims it as parent, return `None` without
    /// looping forever or panicking.
    #[test]
    fn detect_in_tree_missing_root_returns_none() {
        let mut tree = MockProcessTree::new();
        tree.add(50, Some(40), &["bash"]); // unrelated tree
        tree.add(60, Some(50), &["ssh", "elsewhere"]);
        assert!(detect_in_tree(999, &mut tree).is_none());
    }

    /// Cycle 730 drift guard: an empty tree returns `None`.
    /// Boundary case for the BFS init.
    #[test]
    fn detect_in_tree_empty_tree_returns_none() {
        let mut tree = MockProcessTree::new();
        assert!(detect_in_tree(100, &mut tree).is_none());
    }

    /// Cycle 730 drift guard: descendants that don't match a
    /// remote-client argv return `None` even though the walk
    /// completes successfully. Catches the "grep ssh log.txt"
    /// false-positive class.
    #[test]
    fn detect_in_tree_non_remote_descendants_return_none() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(150, Some(100), &["vim", "file.txt"]);
        tree.add(151, Some(100), &["python", "-c", "print('hi')"]);
        // grep "ssh" is not an ssh client — argv[0] gates the
        // detector. Pre-729 this couldn't be tested without
        // spawning a real grep.
        tree.add(200, Some(150), &["grep", "ssh", "log.txt"]);
        assert!(detect_in_tree(100, &mut tree).is_none());
    }

    /// Cycle 730 drift guard: a cycle in the parent chain
    /// (impossible in a real OS, but possible in a buggy fixture
    /// or future trait impl) must not loop the BFS forever. The
    /// `visited` set added in cycle 730 protects against this;
    /// this test fails if someone removes that defensive code.
    #[test]
    fn detect_in_tree_handles_parent_cycle_without_looping() {
        let mut tree = MockProcessTree::new();
        // Pathological: A is parent of B, B is parent of A.
        tree.add(100, Some(200), &["bash"]);
        tree.add(200, Some(100), &["zsh"]);
        // No remote client in the cycle → None, but the test
        // would hang forever pre-`visited` if there's a regression.
        assert!(detect_in_tree(100, &mut tree).is_none());
    }
}
