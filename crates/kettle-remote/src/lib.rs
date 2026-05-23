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
}
