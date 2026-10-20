//! Native agy command hooks. Its PostToolUse contract accepts `{}` only, so
//! previews go to the shelf instead of inventing an inline hook protocol.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) fn enabled() -> bool {
    kettle_config::Config::default_path()
        .and_then(|path| kettle_config::Config::read_from_with_diagnostics(&path).ok())
        .is_some_and(|config| config.config.agent_display && config.config.agent_display_agy)
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
    if !matches!(
        name,
        "write_to_file" | "replace_file_content" | "multi_replace_file_content"
    ) {
        return None;
    }
    let args = tool.get("args")?;
    if name == "write_to_file" && args.get("IsArtifact") != Some(&Value::Bool(true)) {
        return None;
    }
    let root = event.get("artifactDirectoryPath")?.as_str()?;
    let target = args.get("TargetFile")?.as_str()?;
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
    matches!(
        extension.as_str(),
        "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "mmd" | "pdf" | "mp4" | "webm"
    )
    .then_some(target)
}

/// Native TUI provenance, independent of hook JSON and environment claims.
/// The nearest signed agy ancestor must have a terminal for stdout. Print
/// sessions whose output is a file/pipe do not publish automatic previews.
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
                return process::standard_output(process)
                    == Ok(process::StandardOutput::Device(terminal.device))
                    && process::process_group(process)
                        .is_ok_and(|group| group == terminal.foreground_group);
            }
        }
    }
    false
}

pub(crate) fn hook(action: &str) -> i32 {
    let empty = || {
        println!("{{}}");
        0
    };
    if !enabled() || (action != "guidance" && !interactive_parent()) {
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
    let Ok(mut client) = kettle_ctl::Client::discover_display(None) else {
        return empty();
    };
    if action == "guidance" {
        println!(
            "{}",
            json!({"injectSteps":[{"ephemeralMessage":"When a visual helps the user's task, show the finished image, diagram, PDF or video with kettle_show without waiting for them to name the tool. Use an absolute path and omit key for normal file previews. Explicit visual artifacts written through the native artifact channel also reach Kettle's media shelf automatically. Skip unrelated assets, private scratch files and passing-test recordings. agy's own Mermaid rendering remains available. Kettle shelf previews do not mean you inspected the media; agy does not support Kettle's inline hook-card protocol."}]})
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

pub(crate) fn setup(print: bool, install: bool, uninstall: bool) -> i32 {
    if uninstall {
        println!(
            "Run `agy plugin uninstall kettle` to remove the plugin. Turn off agy previews in Kettle Settings to disable it immediately."
        );
        return 0;
    }
    if !(print || install) {
        println!(
            "agy previews: {} in saved settings. Delivery: media shelf; agy's native hooks cannot print inline Kettle cards. Setup: kettle agent-setup --agent agy --install. Requires agy 1.4.x, agent previews, and agy previews enabled. Automatic artifact fallback currently requires a signed macOS agy TUI; MCP display also works on Linux.",
            if enabled() { "enabled" } else { "disabled" }
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
    fn default_keys_are_stable_bounded_and_distinguish_pages() {
        let root = kettle_test_support::private_tempdir("kettle-agy-key-");
        let path = root.path().join("image.png");
        assert_eq!(key(&path, None), key(&path, Some(1)));
        assert_ne!(key(&path, Some(1)), key(&path, Some(2)));
        assert!(key(&path, None).len() < 64);
    }
}
