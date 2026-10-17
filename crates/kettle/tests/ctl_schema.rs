//! `kettle ctl schema` prints the control protocol's JSON Schema without a
//! running Kettle to ask.

use std::process::Command;

/// In a home of its own, with no Kettle to find, the command prints exactly
/// the schema `kettle-ctl` publishes, as JSON, and succeeds.
#[test]
fn the_schema_prints_without_a_running_kettle() {
    let home = kettle_test_support::private_tempdir("kettle-ctl-schema-");
    let mut command = Command::new(env!("CARGO_BIN_EXE_kettle"));
    command
        .args(["ctl", "schema"])
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_RUNTIME_DIR", home.path().join("run"))
        .env("PATH", "/usr/bin:/bin");
    // Windows starts no process without its system root.
    for kept in ["SystemRoot", "windir"] {
        if let Some(value) = std::env::var_os(kept) {
            command.env(kept, value);
        }
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, kettle_ctl::schema::CONTROL_V1.as_bytes());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["$id"], "urn:kettle:control:v1");
}
