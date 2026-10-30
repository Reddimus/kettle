//! Windows console launcher installed as `kettle.com`.
//!
//! Windows command lookup prefers `.com` over `.exe`. The launcher waits for
//! CLI operations so PowerShell/cmd receive prompts, ordered output, and exit
//! codes, while GUI launches still return immediately. Start Menu shortcuts
//! continue to target the GUI-subsystem `kettle.exe` directly.

use clap::Parser;

#[path = "../cli.rs"]
#[allow(dead_code)]
mod cli;
use std::path::PathBuf;

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let parsed = cli::Cli::parse_from(
        std::iter::once(std::ffi::OsString::from("kettle")).chain(arguments.iter().cloned()),
    );
    let target = target_executable();
    let mut command = std::process::Command::new(&target);
    command.args(&arguments);

    if should_wait(&parsed) {
        match command.status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(error) => {
                eprintln!("kettle: could not launch {}: {error}", target.display());
                std::process::exit(1);
            }
        }
    } else if let Err(error) = command.spawn() {
        eprintln!("kettle: could not launch {}: {error}", target.display());
        std::process::exit(1);
    }
}

fn target_executable() -> PathBuf {
    let directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        directory.join("kettle.exe")
    } else {
        directory.join("kettle")
    }
}

fn should_wait(cli: &cli::Cli) -> bool {
    cli.cmd.is_some()
        || cli.list_themes
        || cli.list_keybinds
        || cli.list_actions
        || cli.list_ssh_hosts
        || cli.list_layouts
        || cli.list_profiles
        || cli.config_path
        || cli.gpu_info
        || cli.check_update
        || cli.update
        || cli.check_config
        || cli.print_default_config
        || cli.write_default_config
        || cli.shell_integration.is_some()
        || cli.print_completions.is_some()
        || cli.screenshot.is_some()
        || cli.screenshot_menu.is_some()
        || cli.toggle
        || cli.remote_send.is_some()
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    fn should_wait<I, S>(arguments: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let arguments = arguments.into_iter().map(|arg| arg.as_ref().to_owned());
        super::cli::Cli::try_parse_from(
            std::iter::once(std::ffi::OsString::from("kettle")).chain(arguments),
        )
        .map_or(true, |cli| super::should_wait(&cli))
    }

    #[test]
    fn invalid_gui_arguments_and_help_are_synchronous() {
        for arguments in [vec!["--agent-display", "yes"], vec!["-e"], vec!["--help"]] {
            assert!(
                should_wait(&arguments),
                "must finish synchronously: {arguments:?}"
            );
        }
    }

    #[test]
    fn waits_for_cli_but_not_window_launches() {
        assert!(should_wait(["update", "--yes"]));
        assert!(should_wait(["--config", "settings", "update", "--yes"]));
        assert!(should_wait(["--update"]));
        assert!(should_wait(["--check-update"]));
        assert!(should_wait(["--remote-file", "commands", "--toggle"]));
        assert!(should_wait(["exec", "echo", "ok"]));
        assert!(should_wait(["--version"]));
        assert!(should_wait(["--unknown"]));
        assert!(!should_wait::<[&str; 0], &str>([]));
        assert!(!should_wait(["--working-directory", "C:\\work"]));
        assert!(!should_wait(["-dC:\\work"]));
        assert!(!should_wait(["-mfb"]));
        assert!(!should_wait(["--config", "update"]));
        assert!(!should_wait(["-e", "pwsh"]));
        assert!(!should_wait(["-e", "update"]));
    }

    /// `--new-process` (`crates/kettle/src/main.rs`'s explicit
    /// bare-launch-isolation escape hatch) is a no-value GUI flag like
    /// `--restore`: it must return immediately instead of blocking the
    /// calling shell for the lifetime of the new window. Regression test
    /// for the flag falling through to the unknown-argument `wait` branch.
    #[test]
    fn new_process_flag_does_not_wait() {
        assert!(!should_wait(["--new-process"]));
        assert!(!should_wait(["--new-process", "--restore"]));
        assert!(!should_wait(["--layout", "dev", "--new-process"]));
    }

    #[test]
    fn agent_display_is_classified_as_a_gui_value_option() {
        assert!(!should_wait(["--agent-display", "on"]));
        assert!(!should_wait(["--agent-display=off"]));
        assert!(!should_wait([
            "--agent-server",
            "off",
            "--agent-display",
            "on"
        ]));
        // Missing its value, it is a usage error the CLI reports.
        assert!(should_wait(["--agent-display"]));
    }

    #[test]
    fn recording_directory_is_classified_as_a_gui_value_option() {
        assert!(!should_wait(["--record-dir", "C:\\recordings"]));
        assert!(!should_wait(["--record-dir=C:\\recordings"]));
        assert!(!should_wait([
            "--record-dir",
            "C:\\recordings",
            "--record-raw-input"
        ]));
        assert!(should_wait(["--record-dir"]));
    }
    #[test]
    fn value_options_in_short_clusters_launch_asynchronously() {
        for arguments in [vec!["-mTwork"], vec!["-mdDIR"], vec!["-me", "PROGRAM"]] {
            assert!(
                !should_wait(&arguments),
                "valid GUI cluster must not wait: {arguments:?}"
            );
        }
    }
}
