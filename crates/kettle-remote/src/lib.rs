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
pub fn detect_remote_with(child_pid: u32, sys: &mut sysinfo::System) -> Option<RemoteContext> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate};
    let refresh_kind = ProcessRefreshKind::new().with_cmd(sysinfo::UpdateKind::Always);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);

    // Collect all descendants of child_pid via BFS over the parent
    // chain. sysinfo gives us a flat `processes()` map; we group
    // children by parent.
    let root = Pid::from_u32(child_pid);
    let mut children_by_parent: std::collections::HashMap<Pid, Vec<Pid>> =
        std::collections::HashMap::new();
    for (&pid, proc) in sys.processes() {
        if let Some(parent) = proc.parent() {
            children_by_parent.entry(parent).or_default().push(pid);
        }
    }

    // BFS from root; closer descendants checked first.
    let mut queue: std::collections::VecDeque<Pid> = std::collections::VecDeque::new();
    if let Some(initial) = children_by_parent.get(&root) {
        queue.extend(initial.iter().copied());
    }
    while let Some(pid) = queue.pop_front() {
        if let Some(proc) = sys.process(pid) {
            // Convert OsString argv to Vec<String> for the detectors.
            // Lossy conversion is fine — non-UTF8 argv is exotic
            // and would still be detected at the exe-name level.
            let argv: Vec<String> = proc
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            if let Some(ctx) = detect_ssh(&argv) {
                return Some(ctx);
            }
            if let Some(ctx) = detect_container(&argv) {
                return Some(ctx);
            }
        }
        if let Some(grand) = children_by_parent.get(&pid) {
            queue.extend(grand.iter().copied());
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
}
