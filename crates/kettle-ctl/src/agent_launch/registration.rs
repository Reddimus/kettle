use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::sync::Arc;

use super::{ExecutableStamp, HeldExecutable, card_version};
use crate::process::ProcessIdentity;
use crate::show::InlineTarget;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    fd: u32,
    stamp: ExecutableStamp,
}

impl Descriptor {
    fn of(file: &HeldExecutable) -> Option<Self> {
        Some(Self {
            fd: u32::try_from(file.as_fd().as_raw_fd()).ok()?,
            stamp: file.stamp(),
        })
    }
}

/// Private launcher request: descriptors held by this Kettle command,
/// closed on exec, rather than arbitrary paths the GUI would have to open.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRegistration {
    target: String,
    version: String,
    native: Descriptor,
    launcher: Option<Descriptor>,
}

impl LaunchRegistration {
    pub fn new(
        target: InlineTarget,
        version: &str,
        native: &HeldExecutable,
        launcher: Option<&HeldExecutable>,
    ) -> Option<Self> {
        if !card_version(target, version) {
            return None;
        }
        Some(Self {
            target: target.as_str().into(),
            version: version.trim().into(),
            native: Descriptor::of(native)?,
            launcher: match launcher {
                Some(file) => Some(Descriptor::of(file)?),
                None => None,
            },
        })
    }

    pub fn hold(self, peer: ProcessIdentity) -> io::Result<LaunchFiles> {
        let target = InlineTarget::parse(&self.target)
            .filter(|target| card_version(*target, &self.version))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "unsupported card profile")
            })?;
        let native = Arc::new(HeldExecutable::from_peer(
            peer,
            self.native.fd,
            self.native.stamp,
        )?);
        let launcher = self
            .launcher
            .map(|descriptor| {
                HeldExecutable::from_peer(peer, descriptor.fd, descriptor.stamp).map(Arc::new)
            })
            .transpose()?;
        Ok(LaunchFiles {
            target,
            native,
            launcher,
        })
    }
}

#[derive(Clone, Debug)]
pub struct LaunchFiles {
    pub target: InlineTarget,
    pub native: Arc<HeldExecutable>,
    pub launcher: Option<Arc<HeldExecutable>>,
}

impl LaunchFiles {
    pub fn matches(&self, root: ProcessIdentity, owner: ProcessIdentity) -> bool {
        self.launcher
            .as_deref()
            .unwrap_or(&self.native)
            .matches(root)
            && self.native.matches(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_round_trips_a_peers_held_image() {
        let native = HeldExecutable::open(&std::env::current_exe().unwrap()).unwrap();
        let me = crate::process::current().unwrap().identity;
        let registration =
            LaunchRegistration::new(InlineTarget::CodexHook, "codex-cli 0.162.0", &native, None)
                .unwrap();
        let value = serde_json::to_value(registration).unwrap();
        let registration: LaunchRegistration = serde_json::from_value(value).unwrap();
        let files = registration.hold(me).expect("registered held descriptor");
        assert_eq!(files.target, InlineTarget::CodexHook);
        assert!(files.matches(me, me));
        assert!(files.launcher.is_none());
    }

    #[test]
    fn unsupported_profiles_and_unknown_fields_are_refused_before_fd_access() {
        let native = HeldExecutable::open(&std::env::current_exe().unwrap()).unwrap();
        let me = crate::process::current().unwrap().identity;
        let registration = LaunchRegistration::new(
            InlineTarget::ClaudeHook,
            "2.1.280 (Claude Code)",
            &native,
            None,
        )
        .unwrap();
        let mut value = serde_json::to_value(registration).unwrap();
        value["version"] = serde_json::json!("3.0.0 (Claude Code)");
        value["native"]["fd"] = serde_json::json!(0);
        let registration: LaunchRegistration = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            registration.hold(me).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        value["other"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LaunchRegistration>(value).is_err());
        assert!(
            LaunchRegistration::new(
                InlineTarget::ClaudeHook,
                "2.1.226 (Claude Code)",
                &native,
                None
            )
            .is_none()
        );
    }
}
