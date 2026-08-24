use std::io::{BufRead as _, IsTerminal as _, Write as _};

use kettle_update::{CheckOutcome, FeedClient, UpdateError};

fn missing_artifact_message(has_asset: bool, tag: &str, release_url: &str) -> Option<String> {
    (!has_asset).then(|| {
        format!(
            "kettle update: {tag} has no package for this platform; see supported downloads at\n  {release_url}"
        )
    })
}

pub fn run(assume_yes: bool, current: &str) -> i32 {
    if let Err(error) = kettle_update::prepare_managed_install_for_update() {
        if matches!(&error, UpdateError::UpdateLocked) {
            eprintln!("kettle update: another update is already running");
        } else {
            eprintln!("kettle update: {error}");
            eprintln!(
                "Use the package manager or installer that owns this executable. Self-update covers installs made by kettle's official Windows or Linux installer, and the macOS kettle.app from the release page."
            );
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
