//! kettle-remote — SSH / Docker / Podman / kubectl session detection.
//!
//! Cycle 643 (sub-cycle 2 of
//! [`TERMINATOR-REMOTE-DESIGN.md`](../../../docs/TERMINATOR-REMOTE-DESIGN.md)):
//! crate skeleton + `RemoteContext` type + `detect_remote` stub.
//!
//! - Sub-cycle 3 will add the SSH detector
//!   ([`SshSession::matches`]).
//! - Sub-cycle 4 will add the Container detector
//!   ([`ContainerSession::matches`]) for Docker / Podman / kubectl / lxc.
//! - Sub-cycle 5 will wire the process-tree walk via sysinfo.

#![forbid(unsafe_code)]

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

/// Cycle 643 stub: detect a remote-session context for the pane
/// rooted at `child_pid`. v1 always returns `None`. Sub-cycle 5
/// of [`TERMINATOR-REMOTE-DESIGN.md`](../../../docs/TERMINATOR-REMOTE-DESIGN.md)
/// adds the sysinfo dep + the actual process-tree walk.
///
/// Kept in the public API now so the App can wire the per-pane
/// `remote_context` field ahead of the heavy detection work —
/// dispatch arms and right-click menu code paths compile against
/// the final return shape from the start.
pub fn detect_remote(_child_pid: u32) -> Option<RemoteContext> {
    None
}

/// Cycle 644 (sub-cycle 3 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): SSH-session
/// detector. Takes a process's argv (as the sysinfo walk in
/// sub-cycle 5 will supply it) and returns `Some(Ssh { host, user })`
/// if the argv shape matches an `ssh` invocation, else `None`.
///
/// Recognized argv[0] values: `ssh`, `sshpass`. (`autossh` is a
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
/// Pure — argv-in, Option<RemoteContext>-out. Unit-testable.
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

    /// Cycle 643 drift guard: the v1 stub returns None for any
    /// input. Locks the placeholder behavior so sub-cycle 5's
    /// real detector replacement is a clear delta.
    #[test]
    fn detect_remote_stub_always_returns_none() {
        assert!(detect_remote(0).is_none());
        assert!(detect_remote(1).is_none());
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
