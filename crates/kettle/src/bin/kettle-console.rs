//! Windows console launcher installed as `kettle.com`.
//!
//! Windows command lookup prefers `.com` over `.exe`. The launcher waits for
//! CLI operations so PowerShell/cmd receive prompts, ordered output, and exit
//! codes, while GUI launches still return immediately. Start Menu shortcuts
//! continue to target the GUI-subsystem `kettle.exe` directly.

use std::ffi::OsStr;
use std::path::PathBuf;

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let target = target_executable();
    let mut command = std::process::Command::new(&target);
    command.args(&arguments);

    if should_wait(&arguments) {
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

fn should_wait<I, S>(arguments: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if arguments.is_empty() {
        return false;
    }

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if matches!(argument, "-e" | "--exec")
            || argument.starts_with("--exec=")
            || (argument.starts_with("-e") && argument.len() > 2)
        {
            // The GUI `--exec` option consumes every remaining token as the
            // first pane's command, including words such as `update`.
            return false;
        }
        if matches!(argument, "exec" | "ctl" | "mcp" | "update") || is_console_operation(argument) {
            return true;
        }
        if takes_one_value(argument) {
            if argument.contains('=') || has_attached_short_value(argument) {
                index += 1;
            } else if index + 1 < arguments.len() {
                index += 2;
            } else {
                // Let clap's missing-value diagnostic remain visible.
                return true;
            }
            continue;
        }
        if is_gui_flag(argument) {
            index += 1;
            continue;
        }

        // Unknown options and positionals are clap errors. Waiting preserves
        // their diagnostics and non-zero exit codes in PowerShell and cmd.
        return true;
    }
    false
}

// `is_console_operation`, `takes_one_value`, and `is_gui_flag` below are a
// hand-maintained mirror of the `Cli` struct in `crates/kettle/src/main.rs`.
// There is deliberately no automated cross-check here: `kettle-console` and
// `kettle` are two `[[bin]]` targets in the same `Cargo.toml` with no shared
// `[lib]` crate, so this file cannot `use` the real `Cli` type or call
// `Cli::command().get_arguments()` to walk clap's own flag list. A future
// flag that lands in `main.rs` without a matching update here will silently
// fall through to the final `return true` in `should_wait` (wait — the safe
// default for an unrecognized token, but wrong for a new no-op-value GUI
// flag). If this drifts again, the durable fix is to expose `Cli` from a
// shared `kettle` lib target so a `#[test]` here (or in `main.rs`) can walk
// `get_arguments()` and assert every flag is classified by exactly one of
// these three functions. Until then: when you add a flag to `Cli`, add it
// here too, and extend `waits_for_cli_but_not_window_launches` /
// `recording_directory_is_classified_as_a_gui_value_option` below.
fn is_console_operation(argument: &str) -> bool {
    matches!(
        argument,
        "-h" | "--help"
            | "-V"
            | "--version"
            | "--config-path"
            | "--gpu-info"
            | "--check-update"
            | "--update"
            | "--check-config"
            | "--write-default-config"
            | "--toggle"
            | "--remote-send"
    ) || argument.starts_with("--remote-send=")
        || argument.starts_with("--list-")
        || argument.starts_with("--print-")
        || argument.starts_with("--shell-integration")
        || argument.starts_with("--screenshot")
}

fn takes_one_value(argument: &str) -> bool {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    matches!(
        name,
        "--annotate"
            | "--cols"
            | "--rows"
            | "--config"
            | "--working-directory"
            | "-d"
            | "--layout"
            | "--agent-server"
            | "--tab-handoff"
            | "--tab-handoff-fd"
            | "--profile"
            | "--accent"
            | "--title"
            | "-T"
            | "--remote-file"
            | "--lua-script"
    ) || matches!(name, "--record" | "--record-dir")
        || (name.starts_with("-d") && name.len() > 2)
        || (name.starts_with("-T") && name.len() > 2)
}

fn has_attached_short_value(argument: &str) -> bool {
    (argument.starts_with("-d") || argument.starts_with("-T")) && argument.len() > 2
}

fn is_gui_flag(argument: &str) -> bool {
    matches!(
        argument,
        "--restore"
            | "--maximise"
            | "--maximize"
            | "-m"
            | "--fullscreen"
            | "-f"
            | "--borderless"
            | "-b"
            | "--hidden"
            | "-H"
            | "--new-process"
    ) || argument == "--record-raw-input"
        || (argument.starts_with('-')
            && !argument.starts_with("--")
            && argument.len() > 2
            && argument[1..]
                .chars()
                .all(|flag| matches!(flag, 'm' | 'f' | 'b' | 'H')))
}

#[cfg(test)]
mod tests {
    use super::should_wait;

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
}
