use std::path::Path;

#[test]
fn security_policy_names_shipped_parser_versions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest: toml::Value = toml::from_str(
        &std::fs::read_to_string(root.join("crates/kettle-media-render/Cargo.toml")).unwrap(),
    )
    .unwrap();
    let lock: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("Cargo.lock")).unwrap()).unwrap();
    let policy = std::fs::read_to_string(root.join("SECURITY.md")).unwrap();
    for name in ["merman", "pulldown-cmark"] {
        let version = manifest["target"]["cfg(any(target_os = \"macos\", target_os = \"linux\"))"]
            ["dependencies"][name]["version"]
            .as_str()
            .unwrap()
            .strip_prefix('=')
            .unwrap();
        assert!(lock["package"].as_array().unwrap().iter().any(|package| {
            package["name"].as_str() == Some(name) && package["version"].as_str() == Some(version)
        }));
        let words = policy.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            words.contains(&format!("{name} {version}")),
            "SECURITY.md must name shipped {name} {version}"
        );
    }
}
