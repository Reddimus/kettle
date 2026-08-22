use std::io::{BufRead as _, IsTerminal as _, Write as _};

use kettle_update::{CheckOutcome, FeedClient, UpdateError};

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
