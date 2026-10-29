//! Which update notice a process shows, and the words for it.
//!
//! `update-check.json` is one record per user, and each process also knows
//! the version it runs and the one installed beside it. The facts come from
//! both (see [`crate::update_check::read_facts`]); this module turns them into
//! an update candidate, the persistent rows of About, and
//! the palette entries. Nothing here changes the size of anything.

use kettle_i18n::Translator;

/// What is true about updates for this process. Read off the UI thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateFacts {
    /// `update-policy = off`.
    pub off: bool,
    /// The version this process runs, as a tag (`v5.1.0`).
    pub running: String,
    /// A release newer than both the running and the installed version.
    pub release: Option<Release>,
    /// A newer version installed, or committed, than this process runs: a
    /// restart picks it up.
    pub installed_newer: Option<String>,
    /// The latest release, when this process runs it.
    pub up_to_date: Option<String>,
    /// Kettle can install updates into this install itself.
    pub managed: bool,
    /// Tags whose notices were dismissed, by any process.
    pub dismissed_available: Option<String>,
    pub dismissed_failed: Option<String>,
}

#[derive(Default)]
pub(crate) struct LocalDismissals {
    pub available: Option<String>,
    pub failed: Option<String>,
}

impl LocalDismissals {
    pub fn apply(&self, facts: &mut UpdateFacts) {
        let Some(release) = &facts.release else {
            return;
        };
        for (local, dismissed) in [
            (&self.available, &mut facts.dismissed_available),
            (&self.failed, &mut facts.dismissed_failed),
        ] {
            if local.as_deref() == Some(release.tag.as_str()) {
                *dismissed = local.clone();
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub state: ReleaseState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseState {
    /// Announced, to install by hand or by the package manager.
    Available,
    /// `update-policy = auto` is installing it.
    Installing,
    /// Automatic installs of it failed several times.
    Failed,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoticeKind {
    Available,
    RestartPending,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct Notice {
    pub kind: NoticeKind,
    /// The release the notice is about.
    pub tag: String,
}

/// The one notice to show, if any: Off shows none, then Failed, Available
/// and Restart pending in that order. A dismissed notice gives way to the
/// next. Available and Failed are dismissed per tag for every process;
/// Restart pending only for this process (`restart_dismissed`), because a new
/// version arrives every few days and would bring it straight back.
pub(crate) fn active_notice(facts: &UpdateFacts, restart_dismissed: bool) -> Option<Notice> {
    if facts.off {
        return None;
    }
    if let Some(release) = &facts.release {
        let dismissed = match release.state {
            ReleaseState::Failed => &facts.dismissed_failed,
            ReleaseState::Available => &facts.dismissed_available,
            ReleaseState::Installing => &None,
        };
        let kind = match release.state {
            ReleaseState::Failed => Some(NoticeKind::Failed),
            ReleaseState::Available => Some(NoticeKind::Available),
            ReleaseState::Installing => None,
        };
        if let Some(kind) = kind
            && dismissed.as_deref() != Some(release.tag.as_str())
        {
            return Some(Notice {
                kind,
                tag: release.tag.clone(),
            });
        }
    }
    let installed = facts
        .installed_newer
        .as_ref()
        .filter(|_| !restart_dismissed)?;
    Some(Notice {
        kind: NoticeKind::RestartPending,
        tag: installed.clone(),
    })
}

/// One sentence per fact that holds, for About.
pub(crate) fn fact_rows(facts: &UpdateFacts, tr: &Translator) -> Vec<String> {
    let mut rows = Vec::new();
    if facts.off {
        rows.push(tr.text(kettle_i18n::Text::UpdateFactOff).to_string());
    }
    if let Some(release) = &facts.release
        && !facts.off
    {
        rows.push(match release.state {
            ReleaseState::Available => tr.update_fact_available(&release.tag),
            ReleaseState::Installing => tr.update_fact_installing(&release.tag),
            ReleaseState::Failed => tr.update_fact_failed(&release.tag),
        });
        if release.state == ReleaseState::Available && facts.managed {
            rows.push(
                tr.text(kettle_i18n::Text::AboutUpdateInstallCli)
                    .to_string(),
            );
        }
    }
    if let Some(installed) = &facts.installed_newer {
        rows.push(tr.update_fact_installed(installed, &facts.running));
        rows.push(restart_hint(installed, tr));
    }
    if rows.is_empty()
        && let Some(tag) = &facts.up_to_date
    {
        rows.push(tr.update_fact_up_to_date(tag));
    }
    rows
}

/// New launches use the installed version while old terminals keep running.
fn restart_hint(installed: &str, tr: &Translator) -> String {
    tr.update_restart_new_launches(installed)
}

/// The release page for `tag`; `None` unless it is a version tag.
pub(crate) fn release_url(tag: &str) -> Option<String> {
    kettle_update::release_url(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_disk_snapshot_cannot_undo_a_local_dismissal() {
        for state in [ReleaseState::Available, ReleaseState::Failed] {
            let mut old = UpdateFacts {
                release: release("v5.2.0", state),
                ..facts()
            };
            assert!(active_notice(&old, false).is_some());
            let local = LocalDismissals {
                available: Some("v5.2.0".into()),
                failed: Some("v5.2.0".into()),
            };
            local.apply(&mut old);
            assert!(active_notice(&old, false).is_none());
            old.release = release("v5.3.0", state);
            assert!(
                active_notice(&old, false).is_some(),
                "a new release stays visible"
            );
            old.dismissed_available = Some("v5.3.0".into());
            old.dismissed_failed = Some("v5.3.0".into());
            local.apply(&mut old);
            assert!(
                active_notice(&old, false).is_none(),
                "preserve another process's dismissal"
            );
        }
    }

    fn facts() -> UpdateFacts {
        UpdateFacts {
            running: "v5.0.0".into(),
            managed: true,
            ..UpdateFacts::default()
        }
    }

    fn release(tag: &str, state: ReleaseState) -> Option<Release> {
        Some(Release {
            tag: tag.into(),
            state,
        })
    }

    fn notice(kind: NoticeKind, tag: &str) -> Option<Notice> {
        Some(Notice {
            kind,
            tag: tag.into(),
        })
    }

    /// Off, then Failed, Available and Restart pending; a dismissed notice
    /// gives way to the next one.
    #[test]
    fn the_notice_follows_its_precedence() {
        let mut f = facts();
        assert_eq!(active_notice(&f, false), None);

        // notify with R < I < L: the new release first, the restart after it.
        f.installed_newer = Some("v5.1.0".into());
        f.release = release("v5.2.0", ReleaseState::Available);
        assert_eq!(
            active_notice(&f, false),
            notice(NoticeKind::Available, "v5.2.0")
        );
        f.dismissed_available = Some("v5.2.0".into());
        assert_eq!(
            active_notice(&f, false),
            notice(NoticeKind::RestartPending, "v5.1.0")
        );
        assert_eq!(active_notice(&f, true), None);

        // A failure outranks it all, and is dismissed on its own.
        f.release = release("v5.2.0", ReleaseState::Failed);
        assert_eq!(
            active_notice(&f, false),
            notice(NoticeKind::Failed, "v5.2.0")
        );
        f.dismissed_failed = Some("v5.2.0".into());
        assert_eq!(
            active_notice(&f, false),
            notice(NoticeKind::RestartPending, "v5.1.0")
        );

        // A new tag shows again.
        f.release = release("v5.2.1", ReleaseState::Available);
        assert_eq!(
            active_notice(&f, false),
            notice(NoticeKind::Available, "v5.2.1")
        );

        // Installing shows no chip of its own.
        f.release = release("v5.2.1", ReleaseState::Installing);
        assert_eq!(
            active_notice(&f, false),
            notice(NoticeKind::RestartPending, "v5.1.0")
        );

        // Off silences everything, even after a manual `kettle update`.
        f.off = true;
        assert_eq!(active_notice(&f, false), None);
    }

    #[test]
    fn about_lists_every_fact() {
        let tr = Translator::new(kettle_i18n::Language::En);
        let mut f = facts();
        f.up_to_date = Some("v5.0.0".into());
        assert_eq!(fact_rows(&f, &tr), ["Up to date (v5.0.0)"]);

        f.up_to_date = None;
        f.release = release("v5.2.0", ReleaseState::Available);
        f.installed_newer = Some("v5.1.0".into());
        let rows = fact_rows(&f, &tr);
        assert_eq!(rows[0], "v5.2.0 is available.");
        assert_eq!(rows[1], "Install it with `kettle update`.");
        assert_eq!(rows[2], "v5.1.0 is installed. This window runs v5.0.0.");
        assert_eq!(rows.len(), 4, "{rows:?}");

        f.off = true;
        let rows = fact_rows(&f, &tr);
        assert_eq!(rows[0], "Update checks are off.");
        assert_eq!(rows[1], "v5.1.0 is installed. This window runs v5.0.0.");
    }

    #[test]
    fn restart_hint_preserves_running_windows() {
        let tr = Translator::new(kettle_i18n::Language::En);
        let hint = restart_hint("v5.1.0", &tr);
        assert_eq!(hint, "New launches use v5.1.0.");
    }
}
