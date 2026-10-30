use std::process::Command;

#[test]
fn launcher_reports_clap_results_synchronously() {
    for (arguments, code, message) in [
        (vec!["--agent-display", "yes"], 2, "invalid value 'yes'"),
        (vec!["-e"], 2, "a value is required for '--exec"),
        (vec!["--help"], 0, "Usage: kettle"),
        (vec!["--version"], 0, "kettle "),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_kettle-console"))
            .args(&arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "{arguments:?}");
        let text = if code == 0 {
            output.stdout
        } else {
            output.stderr
        };
        assert!(
            String::from_utf8_lossy(&text).contains(message),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&text)
        );
    }
}
