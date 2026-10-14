//! Launch-local card support. Profiles describe tested hook output, not vendor identity.

use crate::show::InlineTarget;

pub const CODEX_CARD_MINOR: u64 = 162;

/// Whether the version output names a tested card-hook layout.
pub fn card_version(target: InlineTarget, output: &str) -> bool {
    if output.len() > 128 {
        return false;
    }
    let output = output.trim();
    match target {
        InlineTarget::ClaudeHook => output == "2.1.280 (Claude Code)",
        InlineTarget::CodexHook => {
            let Some(version) = output.strip_prefix("codex-cli ") else {
                return false;
            };
            let mut parts = version.split('.');
            let mut part = || {
                parts
                    .next()
                    .filter(|value| {
                        !value.is_empty()
                            && value.bytes().all(|byte| byte.is_ascii_digit())
                            && (value.len() == 1 || !value.starts_with('0'))
                    })
                    .and_then(|value| value.parse::<u64>().ok())
            };
            let (major, minor, patch) = (part(), part(), part());
            major == Some(0)
                && minor == Some(CODEX_CARD_MINOR)
                && patch.is_some()
                && parts.next().is_none()
        }
    }
}

/// A launch's immutable pane and ancestry binding. Moving its tab between
/// windows does not change the pane's original child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchBinding {
    root: crate::process::ProcessIdentity,
    pane: u64,
    pane_root: crate::process::ProcessIdentity,
    native_child: bool,
}

impl LaunchBinding {
    pub fn new(
        root: crate::process::ProcessIdentity,
        pane: u64,
        pane_root: crate::process::ProcessIdentity,
        native_child: bool,
    ) -> Self {
        Self {
            root,
            pane,
            pane_root,
            native_child,
        }
    }

    pub fn root(self) -> crate::process::ProcessIdentity {
        self.root
    }
    pub fn pane(self) -> u64 {
        self.pane
    }
    pub fn pane_root(self) -> crate::process::ProcessIdentity {
        self.pane_root
    }

    /// `chain` must be checked kernel ancestry, beginning at Kettle's MCP
    /// process, whose immediate parent is the native agent.
    pub fn matches(
        self,
        pane: u64,
        pane_root: crate::process::ProcessIdentity,
        owner: crate::process::ProcessIdentity,
        chain: &[crate::process::ProcessIdentity],
    ) -> bool {
        if pane != self.pane || pane_root != self.pane_root || chain.get(1) != Some(&owner) {
            return false;
        }
        let root_index = if self.native_child {
            if owner == self.root {
                return false;
            }
            2
        } else {
            if owner != self.root {
                return false;
            }
            1
        };
        chain.get(root_index) == Some(&self.root) && chain[root_index..].contains(&self.pane_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessIdentity;

    fn id(pid: u32, start: u64) -> ProcessIdentity {
        ProcessIdentity::new_for_tests(pid, start)
    }

    #[test]
    fn launch_routes_require_the_exact_native_parent_and_pane_generation() {
        let (peer, native, shell, npm) = (id(40, 4), id(30, 3), id(10, 1), id(20, 2));
        let direct = LaunchBinding::new(native, 7, shell, false);
        let chain = [peer, native, shell];
        assert!(direct.matches(7, shell, native, &chain));
        let direct_pane = LaunchBinding::new(native, 9, native, false);
        assert!(direct_pane.matches(9, native, native, &[peer, native]));
        assert!(!direct.matches(8, shell, native, &chain), "another pane");
        assert!(
            !direct.matches(7, id(10, 9), native, &chain),
            "restarted pane"
        );
        assert!(
            !direct.matches(7, shell, id(30, 9), &chain),
            "reused native PID"
        );
        assert!(
            !direct.matches(7, shell, native, &[peer, shell, native]),
            "wrong parent"
        );
        assert!(
            !direct.matches(7, shell, native, &[peer, native]),
            "missing pane root"
        );
        let wrapped = LaunchBinding::new(npm, 7, shell, true);
        assert!(wrapped.matches(7, shell, native, &[peer, native, npm, shell]));
        assert!(
            !wrapped.matches(7, shell, native, &[peer, native, id(20, 9), shell]),
            "reused npm PID"
        );
        assert!(
            !wrapped.matches(7, shell, npm, &[peer, npm, shell]),
            "Node is not the native child"
        );
        assert!(
            !wrapped.matches(7, shell, native, &[peer, native, id(25, 5), npm, shell]),
            "unexpected intermediary"
        );
        assert!(
            !wrapped.matches(7, shell, native, &[peer, native, npm]),
            "missing pane generation"
        );
    }

    #[test]
    fn card_profiles_accept_only_tested_hook_versions() {
        assert!(card_version(InlineTarget::CodexHook, "codex-cli 0.162.0\n"));
        assert!(card_version(InlineTarget::CodexHook, "codex-cli 0.162.7"));
        assert!(card_version(
            InlineTarget::ClaudeHook,
            "2.1.280 (Claude Code)\n"
        ));
        for output in [
            "codex-cli 0.161.0",
            "codex-cli 0.163.0",
            "codex-cli 1.162.0",
            "codex-cli 0.162.0-dev",
            "codex-cli 0.162.0 extra",
            "codex-cli +0.162.0",
            "codex-cli 0.0162.0",
            "codex-cli 0.162.01",
            "codex-cli 0.162.+1",
            "0.162.0",
        ] {
            assert!(!card_version(InlineTarget::CodexHook, output), "{output}");
        }
        for output in [
            "2.1.226 (Claude Code)",
            "2.1.281 (Claude Code)",
            "3.1.280 (Claude Code)",
            "2.1.280",
            "2.1.280 (other)",
            "2.1.280-dev (Claude Code)",
        ] {
            assert!(!card_version(InlineTarget::ClaudeHook, output), "{output}");
        }
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{ExecutableStamp, HeldExecutable};

mod pickup;
pub use pickup::{PICKUP_LIFETIME, PickupBook, PickupPermit};

#[cfg(target_os = "linux")]
mod registration;
#[cfg(target_os = "linux")]
pub use registration::{LaunchFiles, LaunchRegistration};

#[cfg(target_os = "linux")]
mod scopes;
#[cfg(target_os = "linux")]
pub use scopes::{CardCaller, LaunchGrant, LaunchScopes};

/// The display adapter's private one-use retrieval request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickupRequest {
    pub target: InlineTarget,
    pub permit: PickupPermit,
}
