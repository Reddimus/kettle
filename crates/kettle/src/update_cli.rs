use std::io::{BufRead as _, IsTerminal as _, Write as _};

use kettle_update::{CheckOutcome, FeedClient, UpdateError};

/// What `kettle update` says when the install cannot be prepared, or `None`
/// when the notice just printed already said it.
fn prepare_failure_message(error: &UpdateError, notice: Option<&str>) -> Option<String> {
    if notice == Some(error.to_string().as_str()) {
        return None;
    }
    Some(match error {
        UpdateError::UpdateLocked => "kettle update: another update is already running".into(),
        // Only these mean the executable belongs to something else.
        UpdateError::UnmanagedInstall(_) | UpdateError::UnsupportedPlatform => format!(
            "kettle update: {error}\nUse the package manager or installer that owns this \
             executable. Self-update covers installs made by kettle's official Linux installer \
             and the macOS kettle.app from the release page."
        ),
        // An official install that needs a permission fix, or an interrupted
        // update it is still recovering.
        _ => format!("kettle update: {error}"),
    })
}

fn missing_artifact_message(has_asset: bool, tag: &str, release_url: &str) -> Option<String> {
    (!has_asset).then(|| {
        format!(
            "kettle update: {tag} has no package for this platform; see supported downloads at\n  {release_url}"
        )
    })
}

pub fn run(assume_yes: bool, current: &str) -> i32 {
    // Leftovers from earlier updates, once: on macOS the bundles it could not
    // remove (#951), on Linux a rollback that cannot finish (#846).
    let notice = kettle_update::tidy_update_leftovers();
    if let Some(notice) = notice.as_deref() {
        eprintln!("kettle update: {notice}");
    }
    if let Err(error) = kettle_update::prepare_managed_install_for_update() {
        if let Some(message) = prepare_failure_message(&error, notice.as_deref()) {
            eprintln!("{message}");
        }
        return 2;
    }

    let client = FeedClient::new();
    let update = match client.check(current) {
        Ok(CheckOutcome::UpToDate { .. }) => {
            println!("kettle {current} is already up to date");
            return 0;
        }
        Ok(CheckOutcome::UpdateAvailable(update)) => update,
        Err(error) => {
            eprintln!("kettle update: could not check the signed release feed: {error}");
            return 1;
        }
    };

    if let Some(message) =
        missing_artifact_message(update.asset.is_some(), &update.tag, &update.release_url)
    {
        eprintln!("{message}");
        return 2;
    }

    if !assume_yes {
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "kettle update: confirmation requires an interactive terminal; use `kettle update --yes` for automation"
            );
            return 2;
        }
        print!("Install kettle {} over {current}? [y/N] ", update.version);
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer).is_err()
            || !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        {
            println!("Update cancelled");
            return 0;
        }
    }

    println!(
        "Downloading and verifying kettle {} ({})...",
        update.version,
        update
            .asset
            .as_ref()
            .map_or("release archive", |asset| asset.name.as_str())
    );
    match kettle_update::install_update(&client, &update) {
        Ok(outcome) => {
            match outcome.disposition {
                kettle_update::InstallDisposition::Applied => println!(
                    "Installed kettle {}. Existing windows keep running {}; restart kettle when convenient.",
                    outcome.version, current
                ),
                kettle_update::InstallDisposition::Staged { .. } => println!(
                    "Verified and staged kettle {}. Close all Kettle windows to let the update helper replace the running executable.",
                    outcome.version
                ),
            }
            0
        }
        Err(UpdateError::UpdateLocked) => {
            eprintln!("kettle update: another update is already running");
            2
        }
        Err(error) => {
            eprintln!("kettle update failed: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_release_without_this_platforms_artifact_links_instead_of_installing() {
        let message = super::missing_artifact_message(
            false,
            "v4.0.0",
            "https://example.invalid/releases/tag/v4.0.0",
        )
        .expect("missing artifact must stop before confirmation and download");
        assert!(message.contains("no package for this platform"));
        assert!(message.contains("https://example.invalid/releases/tag/v4.0.0"));
        assert!(super::missing_artifact_message(true, "v4.0.0", "ignored").is_none());
    }

    /// #951: `kettle update` is where a user managing updates reads about
    /// leftovers, before anything else it prints.
    #[test]
    fn update_names_leftovers_before_checking_the_feed() {
        let source = kettle_test_support::production_source(include_str!("update_cli.rs"));
        let tidy = source
            .find("kettle_update::tidy_update_leftovers()")
            .expect("kettle update tidies leftovers");
        let prepare = source
            .find("kettle_update::prepare_managed_install_for_update()")
            .expect("kettle update prepares the install");
        assert!(tidy < prepare);
    }

    /// #846: a recovery problem is the install's, not a sign that another
    /// tool owns it, and the notice printed just before is not repeated.
    #[test]
    fn prepare_failures_point_at_the_owner_only_for_foreign_installs() {
        use kettle_update::UpdateError;
        let unmanaged = UpdateError::UnmanagedInstall("not ours".into());
        assert!(
            super::prepare_failure_message(&unmanaged, None)
                .unwrap()
                .contains("package manager")
        );
        for error in [
            UpdateError::Transaction("rollback restored every other file".into()),
            UpdateError::UnsafeInstallDirectory("chmod go-w /x".into()),
        ] {
            let message = super::prepare_failure_message(&error, None).unwrap();
            assert!(message.contains(&error.to_string()), "{message}");
            assert!(!message.contains("package manager"), "{message}");
            assert_eq!(
                super::prepare_failure_message(&error, Some(&error.to_string())),
                None,
                "the notice already said it"
            );
        }
    }

    #[test]
    fn confirmation_accepts_only_explicit_yes_tokens() {
        for accepted in ["y", "Y", "yes", " YES "] {
            assert!(matches!(
                accepted.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ));
        }
        for rejected in ["", "n", "true", "okay"] {
            assert!(!matches!(
                rejected.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ));
        }
    }
}
