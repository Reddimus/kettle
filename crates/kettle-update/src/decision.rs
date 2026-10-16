//! What an update check should do, decided from the running version (R), the
//! installed version (I) and the latest release (L).
//!
//! A process left running across an update keeps its old version while a newer
//! one is installed beside it. Comparing L with R alone made such a process
//! announce, download and reinstall a release that was already installed
//! (#574). Here a release counts as new only when it is newer than both R and
//! I, and an installed version newer than R means this process is waiting for a
//! restart.

use semver::Version;

/// How this process was installed, as found when it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// The official installer's layout (a macOS app bundle counts): Kettle
    /// can update it in place.
    Managed,
    /// Some other owner, such as a package manager or a source build.
    Unmanaged,
    /// No self-update on this platform.
    Unsupported,
}

/// The version an install records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledVersion {
    Known(Version),
    /// The installer could not tell, and wrote `unknown`.
    Unknown,
    /// The record could not be read or checked.
    Unreadable,
}

/// The install's update journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalState {
    Clear,
    /// An update to `target` is in place, waiting for a process of that
    /// version to start and confirm it.
    CommittedUnconfirmed {
        target: Version,
    },
    /// An interrupted update the next install attempt will recover.
    Incomplete,
}

/// What the install looks like on disk right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalState {
    pub installed: InstalledVersion,
    pub journal: JournalState,
}

/// `update-policy`, for the policies that check at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Notify,
    Auto,
}

/// The latest release the feed reported.
#[derive(Clone, Copy, Debug)]
pub struct Latest<'a> {
    pub version: &'a Version,
    /// The release carries an archive for this platform.
    pub has_asset: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    UpToDate,
    /// A newer version is installed, or committed and waiting to be
    /// confirmed, than this process runs. Nothing to download.
    RestartPending {
        installed: Version,
    },
    /// Tell the user about a release newer than both the running and the
    /// installed version; `restart_pending` says whether a newer version is
    /// also already installed.
    Announce {
        restart_pending: Option<Version>,
    },
    /// Install a release newer than both the running and the installed
    /// version.
    Install,
}

/// Decides what a check should do. `latest: None` answers from local state
/// alone, without the feed.
pub fn decide(
    running: &Version,
    kind: InstallKind,
    local: &LocalState,
    policy: Policy,
    latest: Option<Latest<'_>>,
) -> Decision {
    let installed = match (kind, &local.installed) {
        (InstallKind::Managed, InstalledVersion::Known(version)) => version,
        _ => running,
    };
    let committed = match &local.journal {
        JournalState::CommittedUnconfirmed { target } if target > running => Some(target),
        _ => None,
    };
    let pending = [
        Some(installed).filter(|version| *version > running),
        committed,
    ]
    .into_iter()
    .flatten()
    .max()
    .cloned();
    let restart_pending = || {
        pending
            .clone()
            .map_or(Decision::UpToDate, |installed| Decision::RestartPending {
                installed,
            })
    };
    // An unconfirmed update finishes on restart; installing over it would
    // discard its last-known-good backup.
    if committed.is_some() {
        return restart_pending();
    }
    let Some(latest) = latest.filter(|latest| latest.version > running.max(installed)) else {
        return restart_pending();
    };
    if policy == Policy::Auto && kind == InstallKind::Managed && latest.has_asset {
        Decision::Install
    } else {
        Decision::Announce {
            restart_pending: pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn local(installed: InstalledVersion, journal: JournalState) -> LocalState {
        LocalState { installed, journal }
    }

    /// kind, installed version, journal, latest, and the decisions for notify,
    /// auto with an archive, and auto without one.
    type Row = (
        InstallKind,
        InstalledVersion,
        JournalState,
        Option<&'static str>,
        [Decision; 3],
    );

    /// Every row is (kind, installed, journal, latest) and the decisions
    /// for notify, auto with an archive, and auto without one. R is 5.0.0;
    /// a latest of None means the feed reported nothing newer than R.
    #[test]
    fn update_decision_table() {
        use Decision::*;
        use InstallKind::*;
        use InstalledVersion::*;
        use JournalState::*;
        let running = v("5.0.0");
        let rp = |text: &str| RestartPending { installed: v(text) };
        let announce = |pending: Option<&str>| Announce {
            restart_pending: pending.map(v),
        };
        let rows: Vec<Row> = vec![
            // Nothing new.
            (
                Managed,
                Known(v("5.0.0")),
                Clear,
                None,
                [UpToDate, UpToDate, UpToDate],
            ),
            // A newer release.
            (
                Managed,
                Known(v("5.0.0")),
                Clear,
                Some("5.1.0"),
                [announce(None), Install, announce(None)],
            ),
            // The latest release is already installed: restart, never reinstall.
            (
                Managed,
                Known(v("5.1.0")),
                Clear,
                Some("5.1.0"),
                [rp("5.1.0"), rp("5.1.0"), rp("5.1.0")],
            ),
            (
                Managed,
                Known(v("5.1.0")),
                Clear,
                None,
                [rp("5.1.0"), rp("5.1.0"), rp("5.1.0")],
            ),
            // Committed and waiting for confirmation.
            (
                Managed,
                Known(v("5.1.0")),
                CommittedUnconfirmed { target: v("5.1.0") },
                Some("5.1.0"),
                [rp("5.1.0"), rp("5.1.0"), rp("5.1.0")],
            ),
            // Newer than what is installed: install it, and say a restart is
            // also pending.
            (
                Managed,
                Known(v("5.1.0")),
                Clear,
                Some("5.2.0"),
                [announce(Some("5.1.0")), Install, announce(Some("5.1.0"))],
            ),
            // An unconfirmed commit holds off even a newer release.
            (
                Managed,
                Known(v("5.1.0")),
                CommittedUnconfirmed { target: v("5.1.0") },
                Some("5.2.0"),
                [rp("5.1.0"), rp("5.1.0"), rp("5.1.0")],
            ),
            // An unknown installed version leaves only R.
            (
                Managed,
                Unknown,
                Clear,
                None,
                [UpToDate, UpToDate, UpToDate],
            ),
            (
                Managed,
                Unknown,
                Clear,
                Some("5.1.0"),
                [announce(None), Install, announce(None)],
            ),
            (
                Managed,
                Unreadable,
                Clear,
                Some("5.1.0"),
                [announce(None), Install, announce(None)],
            ),
            // An older installed version than R.
            (
                Managed,
                Known(v("4.9.0")),
                Clear,
                None,
                [UpToDate, UpToDate, UpToDate],
            ),
            (
                Managed,
                Known(v("4.9.0")),
                Clear,
                Some("5.1.0"),
                [announce(None), Install, announce(None)],
            ),
            // An interrupted update does not hold anything off; the install
            // recovers it first.
            (
                Managed,
                Known(v("5.0.0")),
                Incomplete,
                Some("5.1.0"),
                [announce(None), Install, announce(None)],
            ),
            // A commit not newer than R is this process's own, already done.
            (
                Managed,
                Known(v("5.0.0")),
                CommittedUnconfirmed { target: v("5.0.0") },
                Some("5.1.0"),
                [announce(None), Install, announce(None)],
            ),
            // Kettle cannot update these itself.
            (
                Unmanaged,
                Known(v("5.1.0")),
                Clear,
                None,
                [UpToDate, UpToDate, UpToDate],
            ),
            (
                Unmanaged,
                Known(v("5.1.0")),
                Clear,
                Some("5.1.0"),
                [announce(None), announce(None), announce(None)],
            ),
            (
                Unsupported,
                Unknown,
                Clear,
                Some("5.1.0"),
                [announce(None), announce(None), announce(None)],
            ),
        ];
        for (kind, installed, journal, latest, [notify, auto, auto_no_asset]) in rows {
            let state = local(installed, journal);
            let latest = latest.map(v);
            let check = |policy, has_asset| {
                decide(
                    &running,
                    kind,
                    &state,
                    policy,
                    latest.as_ref().map(|version| Latest { version, has_asset }),
                )
            };
            assert_eq!(
                check(Policy::Notify, true),
                notify,
                "notify: {kind:?} {state:?} {latest:?}"
            );
            assert_eq!(
                check(Policy::Auto, true),
                auto,
                "auto: {kind:?} {state:?} {latest:?}"
            );
            assert_eq!(
                check(Policy::Auto, false),
                auto_no_asset,
                "auto, no archive: {kind:?} {state:?} {latest:?}"
            );
        }
    }

    #[test]
    fn local_state_alone_reports_a_pending_restart() {
        let running = v("5.0.0");
        let state = local(InstalledVersion::Known(v("5.1.0")), JournalState::Clear);
        assert_eq!(
            decide(&running, InstallKind::Managed, &state, Policy::Notify, None),
            Decision::RestartPending {
                installed: v("5.1.0")
            }
        );
        let state = local(InstalledVersion::Known(v("5.0.0")), JournalState::Clear);
        assert_eq!(
            decide(&running, InstallKind::Managed, &state, Policy::Auto, None),
            Decision::UpToDate
        );
    }
}
