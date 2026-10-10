use std::path::Path;

const FAMILY: &[(&str, &str)] = &[
    ("merman", "0.8.0"),
    ("merman-core", "0.8.0"),
    ("merman-render", "0.8.0"),
    ("dugong", "0.8.0"),
    ("dugong-graphlib", "0.8.0"),
    ("manatee", "0.8.0"),
    // Not merman's own, but merman-render's Markdown parser, which the
    // gallery extraction shares rather than locking a second copy.
    ("pulldown-cmark", "0.13.4"),
    ("roughr-merman", "0.12.3"),
];

fn contract(manifest: &toml::Value, lock: &toml::Value) -> Result<(), String> {
    let dependencies = manifest["target"]["cfg(any(target_os = \"macos\", target_os = \"linux\"))"]
        ["dependencies"]
        .as_table()
        .ok_or("missing native media dependencies")?;
    let packages = lock["package"]
        .as_array()
        .ok_or("missing locked packages")?;
    for &(name, version) in FAMILY {
        let dependency = dependencies
            .get(name)
            .ok_or_else(|| format!("missing {name} pin"))?;
        if dependency.get("version").and_then(toml::Value::as_str)
            != Some(format!("={version}").as_str())
            || dependency
                .get("default-features")
                .and_then(toml::Value::as_bool)
                != Some(false)
        {
            return Err(format!("{name} must pin ={version} with defaults disabled"));
        }
        let matches: Vec<_> = packages
            .iter()
            .filter(|package| package.get("name").and_then(toml::Value::as_str) == Some(name))
            .collect();
        if matches.len() != 1
            || matches[0].get("version").and_then(toml::Value::as_str) != Some(version)
        {
            return Err(format!("{name} must resolve once at {version}"));
        }
    }
    let mut features: Vec<_> = dependencies["merman"]["features"]
        .as_array()
        .ok_or("missing Merman features")?
        .iter()
        .map(|feature| feature.as_str().ok_or("non-string Merman feature"))
        .collect::<Result<_, _>>()?;
    features.sort_unstable();
    if features != ["all-diagrams", "layout-cytoscape", "svg"] {
        return Err("Merman must enable only all-diagrams, layout-cytoscape and svg".into());
    }
    Ok(())
}

#[test]
fn actual_media_family_manifest_and_lock_stay_at_the_reviewed_versions() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(crate_root.join("Cargo.toml")).unwrap()).unwrap();
    let lock: toml::Value =
        toml::from_str(&std::fs::read_to_string(crate_root.join("../../Cargo.lock")).unwrap())
            .unwrap();
    contract(&manifest, &lock).unwrap();
}

fn fixture() -> (toml::Value, toml::Value) {
    let manifest = format!(
        "[target.'cfg(any(target_os = \"macos\", target_os = \"linux\"))'.dependencies]\n{}",
        FAMILY
            .iter()
            .map(|&(name, version)| format!(
                "{name} = {{ version = \"={version}\", default-features = false{} }}\n",
                if name == "merman" {
                    ", features = [\"svg\", \"all-diagrams\", \"layout-cytoscape\"]"
                } else {
                    ""
                }
            ))
            .collect::<String>()
    );
    let lock = FAMILY
        .iter()
        .map(|&(name, version)| {
            format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n")
        })
        .collect::<String>();
    (
        toml::from_str(&manifest).unwrap(),
        toml::from_str(&lock).unwrap(),
    )
}

#[test]
fn unlocked_duplicate_or_missing_family_packages_fail_the_drift_guard() {
    let (manifest, lock) = fixture();
    contract(&manifest, &lock).unwrap();
    let mut unlocked = lock.clone();
    unlocked["package"][1]["version"] = "0.8.1".into();
    assert!(
        contract(&manifest, &unlocked)
            .unwrap_err()
            .contains("merman-core")
    );
    let mut duplicate = lock.clone();
    let other = duplicate["package"][0].clone();
    duplicate["package"].as_array_mut().unwrap().push(other);
    assert!(
        contract(&manifest, &duplicate)
            .unwrap_err()
            .contains("merman")
    );
    let mut missing = lock;
    missing["package"].as_array_mut().unwrap().pop();
    assert!(
        contract(&manifest, &missing)
            .unwrap_err()
            .contains("roughr-merman")
    );
}

fn dependencies(value: &mut toml::Value) -> &mut toml::map::Map<String, toml::Value> {
    value["target"]["cfg(any(target_os = \"macos\", target_os = \"linux\"))"]["dependencies"]
        .as_table_mut()
        .unwrap()
}

#[test]
fn loose_pins_default_features_and_extra_adapters_fail_the_drift_guard() {
    let (manifest, lock) = fixture();
    let mut loose = manifest.clone();
    dependencies(&mut loose).get_mut("dugong").unwrap()["version"] = "0.8".into();
    assert!(contract(&loose, &lock).unwrap_err().contains("dugong"));
    let mut defaults = manifest.clone();
    dependencies(&mut defaults).get_mut("merman").unwrap()["default-features"] = true.into();
    assert!(
        contract(&defaults, &lock)
            .unwrap_err()
            .contains("defaults disabled")
    );
    for forbidden in ["png", "layout-elk", "math"] {
        let mut enabled = manifest.clone();
        dependencies(&mut enabled).get_mut("merman").unwrap()["features"]
            .as_array_mut()
            .unwrap()
            .push(forbidden.into());
        assert!(
            contract(&enabled, &lock)
                .unwrap_err()
                .contains("enable only")
        );
    }
}
