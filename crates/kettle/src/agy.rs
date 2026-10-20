//! Native agy command hooks. Its PostToolUse contract accepts `{}` only, so
//! previews go to the shelf instead of inventing an inline hook protocol.
use serde_json::{Value, json};
use std::path::Path;
#[cfg(any(unix, test))]
use std::path::PathBuf;

pub(crate) fn enabled() -> bool {
    client(None).is_ok()
}

/// Bind integration permission to the active Kettle, including its profile,
/// explicit config and launch overrides. This metadata probe grants no reads.
pub(crate) fn client(
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<kettle_ctl::Client, crate::show_cli::Refusal> {
    let mut client = kettle_ctl::Client::discover_display(None)
        .map_err(|error| crate::show_cli::refusal(&error))?;
    let timeout = std::time::Duration::from_secs(1);
    let status = match cancelled {
        Some(cancelled) => client.call_cancellable_with_timeout(
            "agent_display_status",
            json!({}),
            cancelled,
            timeout,
        ),
        None => client.call_with_timeout("agent_display_status", json!({}), timeout),
    }
    .map_err(|error| crate::show_cli::refusal(&error))?;
    if status.get("agy") != Some(&Value::Bool(true)) {
        return Err(kettle_media::FailureCode::DisplayDisabled.into());
    }
    Ok(client)
}

pub(crate) fn key(path: &Path, page: Option<u64>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.into())
        .hash(&mut hash);
    page.unwrap_or(1).hash(&mut hash);
    format!("agy:{:016x}", hash.finish())
}

#[cfg(any(unix, test))]
const ARTIFACT_METADATA_LIMIT: usize = 16 * 1024;

/// Edits omit artifact metadata from the event; the native sidecar retains it.
#[cfg(any(unix, test))]
fn user_facing_metadata(path: &Path) -> Option<bool> {
    use std::io::Read as _;
    let mut name = path.file_name()?.to_os_string();
    name.push(".metadata.json");
    let path = path.with_file_name(name);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > ARTIFACT_METADATA_LIMIT as u64 {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > ARTIFACT_METADATA_LIMIT as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(ARTIFACT_METADATA_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > ARTIFACT_METADATA_LIMIT {
        return None;
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .get("userFacing")?
        .as_bool()
}

#[cfg(any(unix, test))]
fn artifact(event: &Value) -> Option<PathBuf> {
    if event
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|error| !error.is_empty())
    {
        return None;
    }
    let tool = event.get("toolCall")?;
    let name = tool.get("name")?.as_str()?;
    let args = tool.get("args")?;
    // Native hooks use the CLI schema even when the SDK's tool catalog
    // still advertises the older names. Keep each dialect's artifact flag.
    let target = match name {
        "write_file"
            if args.get("ArtifactMetadata")?.get("UserFacing") == Some(&Value::Bool(true)) =>
        {
            args.get("Path")
        }
        "edit_file" => args.get("Path"),
        "write_to_file" if args.get("IsArtifact") == Some(&Value::Bool(true)) => {
            args.get("TargetFile")
        }
        "replace_file_content" | "multi_replace_file_content" => args.get("TargetFile"),
        _ => None,
    }?
    .as_str()?;
    let root = event.get("artifactDirectoryPath")?.as_str()?;
    if root.len() > 4096
        || target.len() > 4096
        || !Path::new(root).is_absolute()
        || !Path::new(target).is_absolute()
    {
        return None;
    }
    let metadata = std::fs::symlink_metadata(target).ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 32 * 1024 * 1024 {
        return None;
    }
    let root = std::fs::canonicalize(root).ok()?;
    let target = std::fs::canonicalize(target).ok()?;
    if target.parent()? != root {
        return None;
    }
    let extension = target.extension()?.to_str()?.to_ascii_lowercase();
    let visual = matches!(
        extension.as_str(),
        "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "mmd" | "pdf" | "mp4" | "webm"
    );
    (visual && (name != "edit_file" || user_facing_metadata(&target) == Some(true)))
        .then_some(target)
}

/// Native TUI provenance, independent of hook JSON and environment claims.
/// The nearest signed agy ancestor must own foreground terminal input and
/// have a recognized interactive launch. agy saves its TUI output descriptors
/// before redirecting stdout/stderr to its log, so stdout is not a TUI signal.
#[cfg(unix)]
fn interactive_parent() -> bool {
    #[cfg(target_os = "macos")]
    {
        use kettle_ctl::{identity, process, signing};
        let chain = identity::current_ancestry(
            std::time::Instant::now() + std::time::Duration::from_millis(250),
        );
        let Some(terminal) = process::current()
            .ok()
            .and_then(|current| process::controlling_terminal(current.identity).ok())
            .flatten()
        else {
            return false;
        };
        for process in chain.into_iter().skip(1).take(3) {
            if signing::signature(process, signing::Requirement::AGY).is_ok() {
                // The nearest signed ancestor is binding: do not borrow an
                // outer TUI's provenance for a nested headless agy child.
                return process::terminal_input(process, terminal.device) == Ok(true)
                    && process::process_group(process)
                        .is_ok_and(|group| group == terminal.foreground_group)
                    && process::launch_arguments(process)
                        .is_ok_and(|arguments| interactive_launch(&arguments));
            }
        }
    }
    false
}

/// Fail closed on print modes, subcommands and unknown future options. Values
/// belong to their preceding option, so a prompt/model value is not a flag.
#[cfg(any(target_os = "macos", test))]
fn interactive_launch(arguments: &[std::ffi::OsString]) -> bool {
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(argument) = argument.to_str() else {
            return false;
        };
        let (name, value) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        match name {
            "--model"
            | "--effort"
            | "--project"
            | "--conversation"
            | "--agent"
            | "--add-dir"
            | "--mode"
            | "--log-file"
            | "--prompt-interactive"
            | "-i" => {
                if value.is_none() && arguments.next().is_none() {
                    return false;
                }
            }
            "--continue"
            | "-c"
            | "--remote-control"
            | "--sandbox"
            | "--new-project"
            | "--dangerously-skip-permissions" => {
                if value.is_some_and(|value| !matches!(value, "true" | "false")) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[cfg(unix)]
pub(crate) fn hook(action: &str) -> i32 {
    let empty = || {
        println!("{{}}");
        0
    };
    if !matches!(action, "guidance" | "artifact") || (action != "guidance" && !interactive_parent())
    {
        return empty();
    }
    use std::io::Read as _;
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > 1024 * 1024
    {
        return empty();
    }
    let Ok(event) = serde_json::from_slice::<Value>(&bytes) else {
        return empty();
    };
    // Strict discovery binds the request to the Kettle in this process's
    // ancestry. No network server, pane override or user-supplied destination.
    let Ok(mut client) = client(None) else {
        return empty();
    };
    if action == "guidance" {
        println!(
            "{}",
            json!({"injectSteps":[{"ephemeralMessage":"When a visual helps the user's task, show the finished image, diagram, PDF or video with kettle_show without waiting for them to name the tool. Use an absolute path and omit key for normal file previews. In native macOS TUI sessions, explicit user-facing visual artifacts written through the native artifact channel also reach Kettle's media shelf automatically. Skip unrelated assets, private scratch files and passing-test recordings. agy's own Mermaid rendering remains available. Kettle shelf previews do not mean you inspected the media; agy does not support Kettle's inline hook-card protocol."}]})
        );
        return 0;
    }
    if action != "artifact" {
        return empty();
    }
    let Some(path) = artifact(&event) else {
        return empty();
    };
    let Ok(source) = crate::show_cli::file_source(&path, false) else {
        return empty();
    };
    let Ok(params) = crate::show_cli::request_params(kettle_ctl::show::ShowRequest {
        source,
        title: None,
        key: Some(key(&path, None)),
        pane: None,
        inline: None,
    }) else {
        return empty();
    };
    let _ = client.call_with_timeout("show", params, kettle_ctl::show::SHOW_CALL_TIMEOUT);
    empty()
}

#[cfg(any(unix, test))]
fn supported_version(output: &str) -> bool {
    let parts = output.trim().split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return false;
    }
    let parsed = parts
        .iter()
        .map(|part| part.parse::<u64>())
        .collect::<Result<Vec<_>, _>>();
    parsed.is_ok_and(|parts| parts[0] == 1 && (parts[1], parts[2]) >= (4, 2))
}

/// Native plugin setup must not hang the caller or collect unbounded output.
#[cfg(unix)]
fn plugin_command(
    mut command: std::process::Command,
    capture: bool,
    timeout: std::time::Duration,
) -> std::io::Result<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::{Error, ErrorKind, Read as _};
    use std::process::Stdio;
    let deadline = std::time::Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .spawn()?;
    let output = child.stdout.take().map(|stdout| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(64 * 1024 + 1).read_to_end(&mut bytes);
            let _ = sender.send(result.map(|_| bytes));
        });
        receiver
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result
                    .err()
                    .unwrap_or_else(|| Error::from(ErrorKind::TimedOut)));
            }
        }
    };
    let bytes = match output {
        Some(receiver) => receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .map_err(|_| Error::from(ErrorKind::TimedOut))??,
        None => Vec::new(),
    };
    if bytes.len() > 64 * 1024 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "agy plugin output exceeded 64 KiB",
        ));
    }
    Ok((status, bytes))
}

#[cfg(unix)]
pub(crate) fn setup(print: bool, install: bool, uninstall: bool) -> i32 {
    if uninstall {
        println!(
            "Run `agy plugin uninstall kettle` to remove the plugin. Turn off agy previews in Kettle Settings to disable it immediately."
        );
        return 0;
    }
    if !(print || install) {
        println!(
            "agy previews: {} for this process. Delivery: media shelf; agy's native hooks cannot print inline Kettle cards. Setup: kettle agent-setup --agent agy --install. Requires agy 1.4.2 or newer 1.x and an active Kettle with agent and agy previews enabled. Automatic artifact fallback currently requires a signed macOS agy process with foreground terminal output; MCP display also works on Linux.",
            if enabled() { "enabled" } else { "unavailable" }
        );
        return 0;
    }
    let Ok(executable) = std::env::current_exe().and_then(std::fs::canonicalize) else {
        return 2;
    };
    let directory = match kettle_ui::agy_plugin::prepare(&executable, env!("CARGO_PKG_VERSION")) {
        Ok(directory) => directory,
        Err(error) => {
            eprintln!("kettle agent-setup: {error}");
            return 2;
        }
    };
    if print {
        println!(
            "agy plugin install '{}'",
            directory.display().to_string().replace('\'', "'\\''")
        );
        return 0;
    }
    let version = crate::agent_setup::version_of(Path::new("agy"));
    if !version.as_deref().is_some_and(supported_version) {
        eprintln!(
            "kettle agent-setup: agy 1.4.2 or newer 1.x is required; no plugin was installed."
        );
        return 2;
    }
    // Do not replace a user's existing plugin, even one with the same name.
    // An existing installation needs inspection and an explicit native
    // uninstall before this command can install ours.
    let mut command = std::process::Command::new("agy");
    command.args(["plugin", "list", "--json"]);
    let catalog = plugin_command(command, true, std::time::Duration::from_secs(10));
    let Ok((status, bytes)) = catalog else {
        eprintln!("kettle agent-setup: cannot read agy's plugin catalog within setup bounds.");
        return 2;
    };
    if !status.success() {
        return 2;
    }
    let Ok(Value::Array(plugins)) = serde_json::from_slice::<Value>(&bytes) else {
        return 2;
    };
    if plugins
        .iter()
        .any(|plugin| plugin.get("name").and_then(Value::as_str) == Some("kettle"))
    {
        eprintln!(
            "kettle agent-setup: a plugin named kettle is already installed; inspect it with agy plugin list before removing or replacing it."
        );
        return 2;
    }
    let mut command = std::process::Command::new("agy");
    command.args(["plugin", "install"]).arg(directory);
    match plugin_command(command, false, std::time::Duration::from_secs(60)) {
        Ok((status, _)) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("Cannot install agy plugin: {error}");
            127
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_interactive_launches_exclude_print_modes_and_subcommands() {
        let accepted = [
            vec![],
            vec!["--model", "gemini-4-argon-high", "--effort", "high"],
            vec!["--continue", "--conversation=session", "--sandbox=false"],
            vec!["-i", "A prompt containing --print as plain text"],
            vec!["--model=gemini-4-argon-high", "--log-file", "/tmp/agy log"],
        ];
        let refused = [
            vec!["-p", "draw an image"],
            vec!["--print=draw an image"],
            vec!["--prompt", "draw an image"],
            vec!["-pdraw an image"],
            vec!["--output-format", "stream-json"],
            vec!["--model", "gemini-4-argon-high", "--print", "draw"],
            vec!["mcp", "list"],
            vec!["models"],
            vec!["--future-mode"],
            vec!["--model"],
            vec!["--sandbox=unknown"],
            vec!["--", "--print"],
        ];
        for (arguments, expected) in accepted
            .into_iter()
            .map(|arguments| (arguments, true))
            .chain(refused.into_iter().map(|arguments| (arguments, false)))
        {
            let arguments: Vec<_> = arguments
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect();
            assert_eq!(interactive_launch(&arguments), expected, "{arguments:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_setup_commands_bound_time_and_output() {
        use std::io::ErrorKind;
        use std::time::Duration;
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("5");
        assert_eq!(
            plugin_command(command, true, Duration::from_millis(20))
                .unwrap_err()
                .kind(),
            ErrorKind::TimedOut
        );
        let mut command = std::process::Command::new("/usr/bin/head");
        command.args(["-c", "65537", "/dev/zero"]);
        assert_eq!(
            plugin_command(command, true, Duration::from_secs(3))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
        let mut command = std::process::Command::new("/bin/echo");
        command.arg("[]");
        let (status, bytes) = plugin_command(command, true, Duration::from_secs(3)).unwrap();
        assert!(status.success());
        assert_eq!(bytes, b"[]\n");
    }
    #[test]
    fn native_plugin_version_floor_is_bounded_to_the_supported_major() {
        for version in ["1.4.2", "1.4.3\n", "1.5.0"] {
            assert!(supported_version(version));
        }
        for version in ["1.4.1", "2.4.2", "1.04.2", "1.4.2-dev", "unknown", "1.4"] {
            assert!(!supported_version(version));
        }
    }
    #[test]
    fn only_successful_explicit_top_level_visual_artifacts_are_accepted() {
        let root = kettle_test_support::private_tempdir("kettle-agy-artifact-");
        let path = root.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let event = json!({"artifactDirectoryPath":root.path(),"toolCall":{"name":"write_to_file","args":{"TargetFile":path,"IsArtifact":true}}});
        assert_eq!(
            artifact(&event),
            Some(std::fs::canonicalize(&path).unwrap())
        );
        let mut changed = event.clone();
        changed["toolCall"]["args"]["IsArtifact"] = json!(false);
        assert!(artifact(&changed).is_none());
        changed = event.clone();
        changed["error"] = json!("failed");
        assert!(artifact(&changed).is_none());
        changed = event.clone();
        changed["toolCall"]["name"] = json!("run_command");
        assert!(artifact(&changed).is_none());
        let outside = tempfile::NamedTempFile::with_suffix(".svg").unwrap();
        std::fs::write(outside.path(), "<svg/>").unwrap();
        changed = event;
        changed["toolCall"]["args"]["TargetFile"] = json!(outside.path());
        assert!(artifact(&changed).is_none());
    }
    #[test]
    fn native_cli_write_file_uses_path_and_user_facing_metadata() {
        let root = kettle_test_support::private_tempdir("kettle-agy-native-artifact-");
        let path = root.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        // Captured from agy 1.4.3's successful native PostToolUse event.
        let event = json!({
            "artifactDirectoryPath": root.path(),
            "error": "",
            "toolCall": {"name": "write_file", "args": {
                "Path": path,
                "ArtifactMetadata": {"UserFacing": true, "RequestFeedback": false}
            }}
        });
        assert_eq!(
            artifact(&event),
            Some(std::fs::canonicalize(&path).unwrap())
        );
        let mut changed = event.clone();
        changed["toolCall"]["args"]["ArtifactMetadata"]["UserFacing"] = json!(false);
        assert!(
            artifact(&changed).is_none(),
            "private artifacts stay private"
        );
        changed = event.clone();
        changed["toolCall"]["args"]["ArtifactMetadata"] = Value::Null;
        assert!(
            artifact(&changed).is_none(),
            "ordinary writes are not artifacts"
        );
        changed = event;
        changed["error"] = json!("write failed");
        assert!(artifact(&changed).is_none());
    }
    #[test]
    fn native_cli_edit_file_uses_path_inside_the_artifact_directory() {
        let root = kettle_test_support::private_tempdir("kettle-agy-native-edit-");
        let path = root.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let metadata = root.path().join("diagram.svg.metadata.json");
        std::fs::write(&metadata, r#"{"userFacing":true}"#).unwrap();
        let mut event = json!({
            "artifactDirectoryPath": root.path(), "error": "",
            "toolCall": {"name": "edit_file", "args": {
                "Path": path, "Target": "Old", "Replacement": "New"
            }}
        });
        assert_eq!(
            artifact(&event),
            Some(std::fs::canonicalize(&path).unwrap())
        );
        std::fs::write(&metadata, r#"{"userFacing":false}"#).unwrap();
        assert!(
            artifact(&event).is_none(),
            "editing a private artifact keeps it private"
        );
        std::fs::remove_file(&metadata).unwrap();
        assert!(
            artifact(&event).is_none(),
            "missing metadata grants no preview"
        );
        std::fs::write(&metadata, r#"{"userFacing":true}"#).unwrap();
        let outside = tempfile::NamedTempFile::with_suffix(".svg").unwrap();
        std::fs::write(outside.path(), "<svg/>").unwrap();
        event["toolCall"]["args"]["Path"] = json!(outside.path());
        assert!(
            artifact(&event).is_none(),
            "ordinary repository edits stay private"
        );
    }
    #[test]
    fn native_cli_edits_require_bounded_regular_user_facing_metadata() {
        let root = kettle_test_support::private_tempdir("kettle-agy-native-metadata-");
        let path = root.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let metadata = root.path().join("diagram.svg.metadata.json");
        let event = json!({"artifactDirectoryPath":root.path(),
            "toolCall":{"name":"edit_file","args":{"Path":path}}});
        for invalid in [r#"{"userFacing":"true"}"#, "{", "[]"] {
            std::fs::write(&metadata, invalid).unwrap();
            assert!(artifact(&event).is_none());
        }
        let mut oversized = br#"{"userFacing":true}"#.to_vec();
        oversized.resize(ARTIFACT_METADATA_LIMIT + 1, b' ');
        std::fs::write(&metadata, oversized).unwrap();
        assert!(artifact(&event).is_none());
        #[cfg(unix)]
        {
            std::fs::remove_file(&metadata).unwrap();
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), r#"{"userFacing":true}"#).unwrap();
            std::os::unix::fs::symlink(outside.path(), &metadata).unwrap();
            assert!(
                artifact(&event).is_none(),
                "metadata links grant no preview"
            );
        }
    }
    #[test]
    fn default_keys_are_stable_bounded_and_distinguish_pages() {
        let root = kettle_test_support::private_tempdir("kettle-agy-key-");
        let path = root.path().join("image.png");
        assert_eq!(key(&path, None), key(&path, Some(1)));
        assert_ne!(key(&path, Some(1)), key(&path, Some(2)));
        assert!(key(&path, None).len() < 64);
    }
}
