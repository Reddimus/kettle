//! The install this process started from, kept for its whole life.
//!
//! On Linux an update replaces `bin/kettle` by rename. From then on a process
//! that was already running sees its own executable as `<path> (deleted)`,
//! which no longer resolves, so locating the install again at check time
//! failed and the process announced the installed version as new (#574). The
//! install is found once at startup instead, while the path still works.

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(any(windows, target_os = "linux"))]
use crate::ManagedInstall;
use crate::decision::{InstallKind, InstalledVersion, JournalState, LocalState};
use crate::{AvailableUpdate, FeedClient, InstallOutcome, UpdateError};

#[derive(Clone, Debug, Default)]
pub struct InstallSnapshot(Kind);

#[derive(Clone, Debug, Default)]
enum Kind {
    #[default]
    Unsupported,
    /// The official installer's layout, its marker checked at startup.
    #[cfg(any(windows, target_os = "linux"))]
    Managed(ManagedInstall),
    /// An app bundle, by path shape; its signature is checked when an update
    /// is installed.
    #[cfg(target_os = "macos")]
    Bundle {
        bundle: PathBuf,
    },
    Unmanaged(String),
}

impl InstallSnapshot {
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn managed(install: ManagedInstall) -> Self {
        Self(Kind::Managed(install))
    }

    /// The bundle around `executable`, or unmanaged when it is not in one.
    #[cfg(target_os = "macos")]
    pub(crate) fn bundle_at(executable: &Path) -> Self {
        match crate::macos::bundle_of(executable) {
            Some(bundle) => Self(Kind::Bundle { bundle }),
            None => Self::unmanaged(&UpdateError::UnmanagedInstall(
                "kettle is not running from an app bundle".into(),
            )),
        }
    }

    /// An install Kettle does not update itself, or no self-update at all,
    /// from the error that locating it returned.
    pub(crate) fn unmanaged(error: &UpdateError) -> Self {
        match error {
            UpdateError::UnsupportedPlatform => Self(Kind::Unsupported),
            UpdateError::UnmanagedInstall(reason) => Self(Kind::Unmanaged(reason.clone())),
            other => Self(Kind::Unmanaged(other.to_string())),
        }
    }

    pub fn kind(&self) -> InstallKind {
        match &self.0 {
            Kind::Unsupported => InstallKind::Unsupported,
            #[cfg(any(windows, target_os = "linux"))]
            Kind::Managed(_) => InstallKind::Managed,
            #[cfg(target_os = "macos")]
            Kind::Bundle { .. } => InstallKind::Managed,
            Kind::Unmanaged(_) => InstallKind::Unmanaged,
        }
    }

    /// Whether startup already checked that the installer owns this install.
    /// A later "not managed" error from such an install is then a failure,
    /// not a reason to point the user at the release page.
    pub fn proven_at_startup(&self) -> bool {
        #[cfg(any(windows, target_os = "linux"))]
        if let Kind::Managed(_) = self.0 {
            return true;
        }
        false
    }

    /// The installed version and journal as they are on disk now. Reads small
    /// local files, and on macOS runs `PlistBuddy`, so call it off the UI
    /// thread.
    pub fn local_state(&self) -> LocalState {
        let unreadable = LocalState {
            installed: InstalledVersion::Unreadable,
            journal: JournalState::Clear,
        };
        match &self.0 {
            #[cfg(target_os = "linux")]
            Kind::Managed(install) => LocalState {
                installed: crate::install::installed_version_at(&install.marker_path),
                journal: crate::install::journal_state_at(&install.prefix),
            },
            // Windows stages an update until every process exits, so the
            // installed version never moves under a running one.
            #[cfg(windows)]
            Kind::Managed(install) => LocalState {
                installed: crate::install::installed_version_at(&install.marker_path),
                journal: JournalState::Clear,
            },
            #[cfg(target_os = "macos")]
            Kind::Bundle { bundle } => LocalState {
                installed: crate::macos::read_bundle_version(&info_plist(bundle))
                    .map_or(InstalledVersion::Unreadable, InstalledVersion::Known),
                journal: JournalState::Clear,
            },
            _ => unreadable,
        }
    }

    /// The install found at startup, for tests that drive it directly.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn managed_install(&self) -> Option<&ManagedInstall> {
        match &self.0 {
            Kind::Managed(install) => Some(install),
            _ => None,
        }
    }

    /// Installs `update` into this install.
    pub fn install(
        &self,
        client: &FeedClient,
        update: &AvailableUpdate,
    ) -> Result<InstallOutcome, UpdateError> {
        match &self.0 {
            #[cfg(target_os = "linux")]
            Kind::Managed(install) => {
                crate::install::install_into_captured(client, update, install)
            }
            // Windows and macOS keep a working path to their own executable:
            // Windows cannot replace a running one, and macOS swaps the bundle
            // directory under the same name.
            #[cfg(windows)]
            Kind::Managed(_) => crate::install_update(client, update),
            #[cfg(target_os = "macos")]
            Kind::Bundle { .. } => crate::install_update(client, update),
            Kind::Unmanaged(reason) => Err(UpdateError::UnmanagedInstall(reason.clone())),
            Kind::Unsupported => Err(UpdateError::UnsupportedPlatform),
        }
    }
}

#[cfg(target_os = "macos")]
fn info_plist(bundle: &Path) -> PathBuf {
    bundle.join("Contents/Info.plist")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::decision::{Decision, Latest, Policy, decide};

    /// After an update swaps the bundle directory, a process left running
    /// reads the new bundle's version, so the release it would otherwise
    /// announce, or fail to reinstall every check, is a pending restart.
    #[test]
    fn stale_bundle_process_reads_the_swapped_bundle_version() {
        let root = kettle_test_support::private_tempdir("kettle-bundle-snapshot-");
        let contents = root.path().join("Kettle.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::write(contents.join("MacOS/kettle"), b"").unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
                r#"<plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>99.0.0</string></dict></plist>"#,
            ),
        )
        .unwrap();

        let snapshot = InstallSnapshot::bundle_at(&contents.join("MacOS/kettle"));
        assert_eq!(snapshot.kind(), InstallKind::Managed);
        let local = snapshot.local_state();
        let installed = semver::Version::new(99, 0, 0);
        assert_eq!(local.installed, InstalledVersion::Known(installed.clone()));
        let running = semver::Version::new(5, 0, 0);
        for policy in [Policy::Notify, Policy::Auto] {
            assert_eq!(
                decide(
                    &running,
                    snapshot.kind(),
                    &local,
                    policy,
                    Some(Latest {
                        version: &installed,
                        has_asset: true
                    }),
                ),
                Decision::RestartPending {
                    installed: installed.clone()
                }
            );
        }
        let elsewhere = InstallSnapshot::bundle_at(&root.path().join("kettle"));
        assert_eq!(elsewhere.kind(), InstallKind::Unmanaged);
    }
}
