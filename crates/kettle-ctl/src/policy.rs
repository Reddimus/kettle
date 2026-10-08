//! Who may do what over the control server.
//!
//! Two settings decide it: `agent-server` (`off`, `read-only` or `full`) for
//! reading and driving the terminal, and `agent-display` for showing media.
//! Every method declares one [`Capability`]; [`CtlPolicy::check`] is the one
//! gate both the UI dispatch and the connection threads call before doing
//! any work for a request.

use crate::protocol::{Capability, RpcError, error_codes};

/// The `agent-server` setting: how much of the terminal a control client may
/// read or drive. Display permission is separate ([`CtlPolicy::display`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentServer {
    /// No reads or mutations (default). The server still runs when display is
    /// on, for display requests only.
    #[default]
    Off,
    /// Read-only methods only (`get_state`, `list_*`, `read_screen`,
    /// `subscribe`). Mutating methods are rejected with `read_only`.
    ReadOnly,
    /// All methods, including `send_text` and `run_command`.
    Full,
}

impl AgentServer {
    /// Whether reads or mutations are enabled. Display alone can also start
    /// the server; see [`CtlPolicy::runs_server`].
    pub fn is_enabled(self) -> bool {
        !matches!(self, AgentServer::Off)
    }
    /// Whether mutating methods are permitted.
    pub fn allows_mutation(self) -> bool {
        matches!(self, AgentServer::Full)
    }
    /// The config spelling: `off`, `read-only` or `full`.
    pub const fn config_token(self) -> &'static str {
        match self {
            AgentServer::Off => "off",
            AgentServer::ReadOnly => "read-only",
            AgentServer::Full => "full",
        }
    }
}

/// The resolved control policy for one Kettle process.
///
/// All six combinations are meaningful: `display` records the user's display
/// preference even when `Full` already grants display, so turning the server
/// down later cannot silently drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CtlPolicy {
    server: AgentServer,
    display: bool,
}

impl CtlPolicy {
    pub const fn new(server: AgentServer, display: bool) -> Self {
        Self { server, display }
    }

    pub const fn server(self) -> AgentServer {
        self.server
    }

    pub const fn display(self) -> bool {
        self.display
    }

    /// Whether this policy lets a client use `capability`.
    pub const fn allows(self, capability: Capability) -> bool {
        match capability {
            Capability::Read => !matches!(self.server, AgentServer::Off),
            Capability::Mutate => matches!(self.server, AgentServer::Full),
            Capability::Display => matches!(self.server, AgentServer::Full) || self.display,
        }
    }

    /// Whether a control server should be listening at all.
    pub const fn runs_server(self) -> bool {
        !matches!(self.server, AgentServer::Off) || self.display
    }

    /// The same policy with display turned on. Display can be enabled while
    /// Kettle runs; turning it off takes effect at the next launch.
    pub const fn with_display(self) -> Self {
        Self {
            server: self.server,
            display: true,
        }
    }

    /// The policy after a reload asks for `requested` display: display can
    /// only be turned on while Kettle runs. `None` when nothing changes, so
    /// the caller acts exactly once, on the change.
    pub const fn latch_display(self, requested: bool) -> Option<Self> {
        if requested && !self.display {
            Some(self.with_display())
        } else {
            None
        }
    }

    /// The one authorization gate. A refusal names the missing permission
    /// in words a model can act on: it never suggests changing configuration.
    pub fn check(self, capability: Capability) -> Result<(), RpcError> {
        if self.allows(capability) {
            return Ok(());
        }
        let (code, message) = match capability {
            // Only a display-only server can refuse a read: `Off` with
            // display off runs no server at all.
            Capability::Read => (error_codes::DISPLAY_ONLY, DISPLAY_ONLY_MESSAGE),
            Capability::Mutate => (error_codes::READ_ONLY, READ_ONLY_MESSAGE),
            Capability::Display => (error_codes::DISPLAY_DISABLED, DISPLAY_DISABLED_MESSAGE),
        };
        Err(RpcError {
            code: code.to_string(),
            message: message.to_string(),
        })
    }
}

/// The refusal for a read under a display-only policy.
pub const DISPLAY_ONLY_MESSAGE: &str =
    "This display-only connection cannot read terminal contents or geometry.";
/// The refusal for a mutation under any policy short of `full`.
pub const READ_ONLY_MESSAGE: &str = "This connection cannot perform control mutations.";
/// The refusal for a display request while agent previews are off.
pub const DISPLAY_DISABLED_MESSAGE: &str = "Kettle previews are off or Kettle is not running. \
    The user can turn on Agent previews in Kettle Settings; display enables immediately. \
    Claude integration needs a new pane/session. Do not change configuration or retry.";

/// The live policy a running server and its connection threads share. The
/// server mode is fixed for the process; display can only be turned on while
/// Kettle runs (turning it off takes effect at the next launch), so one atomic
/// bit carries every runtime change.
#[derive(Debug, Clone)]
pub struct SharedCtlPolicy {
    server: AgentServer,
    display: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SharedCtlPolicy {
    pub fn new(policy: CtlPolicy) -> Self {
        Self {
            server: policy.server,
            display: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(policy.display)),
        }
    }

    /// The policy as of now, for one authorization decision.
    pub fn current(&self) -> CtlPolicy {
        CtlPolicy::new(
            self.server,
            self.display.load(std::sync::atomic::Ordering::Acquire),
        )
    }

    /// Turn display on for every holder, including open connections.
    pub fn enable_display(&self) {
        self.display
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Launch-time overrides (`--agent-server`, `--agent-display`) applied over
/// the configured policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CtlOverrides {
    pub server: Option<AgentServer>,
    pub display: Option<bool>,
}

impl CtlOverrides {
    /// Each override replaces its setting. `--agent-server off` also turns
    /// display off unless `--agent-display on` is given too, so `off` on the
    /// command line means no server at all by default.
    pub const fn resolve(self, configured: CtlPolicy) -> CtlPolicy {
        let server = match self.server {
            Some(server) => server,
            None => configured.server,
        };
        let display = match (self.display, self.server) {
            (Some(display), _) => display,
            (None, Some(AgentServer::Off)) => false,
            (None, _) => configured.display,
        };
        CtlPolicy { server, display }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Method;

    const SERVERS: [AgentServer; 3] = [AgentServer::Off, AgentServer::ReadOnly, AgentServer::Full];

    /// The plan's six-by-three truth table, spelled out rather than derived.
    #[test]
    fn six_policies_by_three_capabilities() {
        use AgentServer::*;
        let table = [
            // server, display, read, mutate, display
            (Off, false, false, false, false),
            (Off, true, false, false, true),
            (ReadOnly, false, true, false, false),
            (ReadOnly, true, true, false, true),
            (Full, false, true, true, true),
            (Full, true, true, true, true),
        ];
        for (server, display, read, mutate, show) in table {
            let policy = CtlPolicy::new(server, display);
            assert_eq!(policy.allows(Capability::Read), read, "{policy:?} read");
            assert_eq!(
                policy.allows(Capability::Mutate),
                mutate,
                "{policy:?} mutate"
            );
            assert_eq!(
                policy.allows(Capability::Display),
                show,
                "{policy:?} display"
            );
            assert_eq!(
                policy.runs_server(),
                read || mutate || show,
                "{policy:?} runs a server exactly when it allows something"
            );
        }
    }

    /// The exact refusal texts, as agents and docs quote them.
    #[test]
    fn refusals_name_the_missing_permission() {
        let refusal = |policy: CtlPolicy, capability| {
            let error = policy.check(capability).unwrap_err();
            (error.code, error.message)
        };
        let display_only = CtlPolicy::new(AgentServer::Off, true);
        assert_eq!(
            refusal(display_only, Capability::Read),
            (
                "display_only".to_string(),
                "This display-only connection cannot read terminal contents or geometry."
                    .to_string()
            )
        );
        let mutation = (
            "read_only".to_string(),
            "This connection cannot perform control mutations.".to_string(),
        );
        assert_eq!(refusal(display_only, Capability::Mutate), mutation);
        let read_only = CtlPolicy::new(AgentServer::ReadOnly, false);
        assert_eq!(refusal(read_only, Capability::Mutate), mutation);
        let (code, message) = refusal(read_only, Capability::Display);
        assert_eq!(code, "display_disabled");
        assert_eq!(
            message,
            "Kettle previews are off or Kettle is not running. The user can turn on \
             Agent previews in Kettle Settings; display enables immediately. Claude \
             integration needs a new pane/session. Do not change configuration or retry."
        );
        // No refusal suggests granting more control.
        for (policy, capability) in [
            (display_only, Capability::Read),
            (display_only, Capability::Mutate),
            (read_only, Capability::Mutate),
            (read_only, Capability::Display),
        ] {
            let (_, message) = refusal(policy, capability);
            assert!(!message.contains("full"), "{message}");
            assert!(!message.contains("agent-server"), "{message}");
        }
        assert!(
            CtlPolicy::new(AgentServer::Full, false)
                .check(Capability::Display)
                .is_ok()
        );
    }

    #[test]
    fn every_method_is_gated_by_its_declared_capability() {
        for server in SERVERS {
            for display in [false, true] {
                let policy = CtlPolicy::new(server, display);
                for method in Method::ALL {
                    assert_eq!(
                        policy.check(method.capability()).is_ok(),
                        policy.allows(method.capability()),
                        "{policy:?} {method:?}"
                    );
                }
            }
        }
    }

    /// No existing method is display-only: display permission alone reaches
    /// none of today's reads or mutations.
    #[test]
    fn display_alone_reaches_no_read_or_mutation() {
        let policy = CtlPolicy::new(AgentServer::Off, true);
        for method in Method::ALL {
            if method.capability() != Capability::Display {
                assert!(!policy.allows(method.capability()), "{method:?}");
            }
        }
    }

    #[test]
    fn overrides_replace_settings_and_server_off_drops_display() {
        use AgentServer::*;
        let configured = CtlPolicy::new(ReadOnly, true);
        let none = CtlOverrides::default();
        assert_eq!(none.resolve(configured), configured);
        let off = CtlOverrides {
            server: Some(Off),
            display: None,
        };
        assert_eq!(off.resolve(configured), CtlPolicy::new(Off, false));
        let off_with_display = CtlOverrides {
            server: Some(Off),
            display: Some(true),
        };
        assert_eq!(
            off_with_display.resolve(configured),
            CtlPolicy::new(Off, true)
        );
        let full = CtlOverrides {
            server: Some(Full),
            display: None,
        };
        assert_eq!(full.resolve(configured), CtlPolicy::new(Full, true));
        let no_display = CtlOverrides {
            server: None,
            display: Some(false),
        };
        assert_eq!(
            no_display.resolve(configured),
            CtlPolicy::new(ReadOnly, false)
        );
    }

    /// Every configured policy under every combination of launch flags.
    #[test]
    fn override_precedence_matrix() {
        for server in SERVERS {
            for display in [false, true] {
                let configured = CtlPolicy::new(server, display);
                for server_flag in [
                    None,
                    Some(AgentServer::Off),
                    Some(AgentServer::ReadOnly),
                    Some(AgentServer::Full),
                ] {
                    for display_flag in [None, Some(false), Some(true)] {
                        let resolved = CtlOverrides {
                            server: server_flag,
                            display: display_flag,
                        }
                        .resolve(configured);
                        assert_eq!(resolved.server(), server_flag.unwrap_or(server));
                        let expected_display = match (display_flag, server_flag) {
                            (Some(flag), _) => flag,
                            (None, Some(AgentServer::Off)) => false,
                            (None, _) => display,
                        };
                        assert_eq!(
                            resolved.display(),
                            expected_display,
                            "{configured:?} {server_flag:?} {display_flag:?}"
                        );
                    }
                }
            }
        }
    }

    /// A connection opened before display was enabled sees the change.
    #[test]
    fn enabling_display_reaches_every_holder_of_the_shared_policy() {
        let shared = SharedCtlPolicy::new(CtlPolicy::new(AgentServer::ReadOnly, false));
        let connection = shared.clone();
        assert!(!connection.current().allows(Capability::Display));
        shared.enable_display();
        assert!(connection.current().allows(Capability::Display));
        assert_eq!(connection.current().server(), AgentServer::ReadOnly);
    }

    /// Reloads only ever widen display, and report a change exactly once.
    #[test]
    fn display_latches_on_and_never_turns_off_while_running() {
        for server in SERVERS {
            let off = CtlPolicy::new(server, false);
            let on = off
                .latch_display(true)
                .expect("turning display on applies now");
            assert_eq!(on, CtlPolicy::new(server, true));
            assert_eq!(on.latch_display(true), None, "already on");
            assert_eq!(
                on.latch_display(false),
                None,
                "off waits for the next launch"
            );
            assert_eq!(off.latch_display(false), None);
        }
    }

    #[test]
    fn display_can_be_turned_on_without_touching_the_server_mode() {
        for server in SERVERS {
            let enabled = CtlPolicy::new(server, false).with_display();
            assert_eq!(enabled.server(), server);
            assert!(enabled.display() && enabled.allows(Capability::Display));
        }
    }
}
