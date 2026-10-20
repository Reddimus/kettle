//! An explicitly installed native agy plugin. Kettle owns its staging files;
//! agy's installer owns registration. The hook and MCP adapter remain dormant
//! unless agent previews and the separate agy integration are enabled.
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn files(executable: &Path, version: &str) -> Result<crate::owned_dir::OwnedFiles, String> {
    let command =
        crate::codex_shell::kettle_path(executable).map_err(|error| format!("{error:?}"))?;
    let quoted = format!("'{}'", command.replace('\'', "'\\''"));
    let hook = |action: &str| {
        serde_json::json!({
            "type": "command", "command": format!("{quoted} agent-setup --agy-hook {action}"), "timeout": 24
        })
    };
    let hooks = serde_json::json!({"kettle-previews": {
        "PreInvocation": [hook("guidance")],
        "PostToolUse": [{"matcher": "^(write_file|edit_file|write_to_file|replace_file_content|multi_replace_file_content)$", "hooks": [hook("artifact")]}]
    }});
    let server = serde_json::json!({"mcpServers":{"kettle":{
        "command":command,"args":["mcp","--display","--agy-display"]
    }}});
    let json = |value: serde_json::Value| format!("{value:#}\n");
    Ok(crate::owned_dir::OwnedFiles::new(
        "agy-kettle",
        command.into(),
        version.into(),
        vec![
            (
                "plugin.json",
                json(
                    serde_json::json!({"name":"kettle","version":version,"description":"Visual previews in Kettle's media shelf."}),
                ),
            ),
            ("mcp_config.json", json(server)),
            ("hooks.json", json(hooks)),
        ],
    ))
}

#[cfg(unix)]
fn identity(directory: &Path) -> Option<(String, Option<String>)> {
    let server: serde_json::Value =
        serde_json::from_slice(&crate::owned_dir::read_small(directory, "mcp_config.json")?)
            .ok()?;
    let server = &server["mcpServers"]["kettle"];
    if server["args"] != serde_json::json!(["mcp", "--display", "--agy-display"]) {
        return None;
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&crate::owned_dir::read_small(directory, "plugin.json")?).ok()?;
    Some((
        server["command"].as_str()?.into(),
        manifest["version"].as_str().map(str::to_owned),
    ))
}

/// Stage immutable, private, checked plugin files for `agy plugin install`.
pub fn prepare(executable: &Path, version: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let root =
            crate::agent_plugin::plugins_root().ok_or("Kettle data directory unavailable")?;
        crate::owned_dir::install(&root, &files(executable, version)?, identity, true)
            .map_err(|()| "Kettle's agy plugin directory failed verification".into())
    }
    #[cfg(not(unix))]
    {
        let _ = (executable, version);
        Err("agy previews are available on Unix only".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn native_plugin_has_root_hooks_and_only_display_mcp() {
        let files = files(Path::new("/Applications/a'b/kettle"), "5.5.0").unwrap();
        // The shared installer verifies exact bytes, ownership and read-only modes.
        let root = kettle_test_support::private_tempdir("kettle-agy-plugin-");
        #[cfg(unix)]
        {
            let directory = crate::owned_dir::install(root.path(), &files, identity, true).unwrap();
            crate::owned_dir::verify(&directory, &files).unwrap();
            let hooks: serde_json::Value =
                serde_json::from_slice(&std::fs::read(directory.join("hooks.json")).unwrap())
                    .unwrap();
            assert!(
                hooks["kettle-previews"]["PreInvocation"][0]["command"]
                    .as_str()
                    .unwrap()
                    .contains("'\\''")
            );
            assert_eq!(
                hooks["kettle-previews"]["PostToolUse"][0]["hooks"][0]["type"],
                "command"
            );
            assert!(!directory.join("hooks/hooks.json").exists());
            let matcher = regex::Regex::new(
                hooks["kettle-previews"]["PostToolUse"][0]["matcher"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert!(matcher.is_match("write_file"), "agy 1.4.3 native hook name");
            assert!(matcher.is_match("edit_file"));
            assert!(!matcher.is_match("run_command"));
            assert!(!matcher.is_match("not_write_file"));
        }
    }
}
