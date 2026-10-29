use std::fs;
use std::process::Command;

#[test]
fn theme_build_rejects_incomplete_bundles() {
    let dir = kettle_test_support::private_tempdir("kettle-theme-build-");
    let executable = dir.join(format!("theme-build{}", std::env::consts::EXE_SUFFIX));
    let status = Command::new("rustc")
        .arg("--edition=2024")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"))
        .arg("-o")
        .arg(&executable)
        .status()
        .unwrap();
    assert!(status.success());
    let manifest = dir.join("crates/kettle-config");
    fs::create_dir_all(&manifest).unwrap();
    let themes = dir.join("assets/themes");
    for invalid_theme in [false, true] {
        if invalid_theme {
            fs::create_dir_all(&themes).unwrap();
            fs::write(themes.join("Broken Theme"), [0xff]).unwrap();
        }
        let output = Command::new(&executable)
            .env("CARGO_MANIFEST_DIR", &manifest)
            .env("OUT_DIR", dir.as_ref())
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "theme build accepted an incomplete bundle (invalid_theme={invalid_theme})"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reported_themes = manifest.join("../../assets/themes");
        let affected = if invalid_theme {
            reported_themes.join("Broken Theme")
        } else {
            reported_themes
        };
        assert!(stderr.contains(&affected.display().to_string()), "{stderr}");
    }
}
