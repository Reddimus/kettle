use super::{
    POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM, POWERSHELL_BOOTSTRAP_MAX_CHARS,
    POWERSHELL_BOOTSTRAP_PREFIX, POWERSHELL_BOOTSTRAP_SUFFIX, POWERSHELL_INTEGRATION,
    powershell_integration_bootstrap, should_inject_explicit_powershell,
};

fn base64_decode(text: &str) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for byte in text.bytes().filter(|&byte| byte != b'=') {
        let value = A.iter().position(|&a| a == byte).expect("base64 alphabet") as u32;
        bits = bits << 6 | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    out
}

fn payload(script: &str) -> String {
    use std::io::Read as _;
    let bootstrap = powershell_integration_bootstrap(script);
    let encoded = bootstrap
        .strip_prefix(POWERSHELL_BOOTSTRAP_PREFIX)
        .and_then(|rest| rest.strip_suffix(POWERSHELL_BOOTSTRAP_SUFFIX))
        .expect("one prefix and one suffix");
    let mut text = String::new();
    flate2::read::DeflateDecoder::new(base64_decode(encoded).as_slice())
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[test]
fn the_bootstrap_carries_the_exact_script() {
    for script in ["Aé", "", POWERSHELL_INTEGRATION] {
        assert_eq!(payload(script), script);
    }
}

/// Holds the integration under the command-line budget, with room left
/// to grow: CreateProcessW stops at 32,767 UTF-16 code units.
#[test]
fn the_integration_bootstrap_fits_the_windows_command_line() {
    let bootstrap = powershell_integration_bootstrap(POWERSHELL_INTEGRATION);
    assert!(
        bootstrap.is_ascii(),
        "the CreateProcessW payload must stay one UTF-16 unit per byte"
    );
    assert!(
        bootstrap.len() + POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM <= POWERSHELL_BOOTSTRAP_MAX_CHARS,
        "the integration needs useful bootstrap headroom: {} chars",
        bootstrap.len()
    );
}

/// The bootstrap runs in a real PowerShell where one is installed, and
/// loads the whole integration without an error.
#[test]
fn powershell_runs_the_bootstrap() {
    let Some(pwsh) = ["pwsh", "pwsh.exe", "powershell.exe"]
        .into_iter()
        .find(|name| {
            std::process::Command::new(name)
                .args(["-NoProfile", "-NonInteractive", "-Command", "exit 0"])
                .output()
                .is_ok_and(|output| output.status.success())
        })
    else {
        eprintln!("skipped: no PowerShell installed");
        return;
    };
    for (script, marker) in [
        ("Write-Output ('KETTLE_' + 'BOOT')", "KETTLE_BOOT"),
        (POWERSHELL_INTEGRATION, ""),
    ] {
        let command = format!(
            "{}; Write-Output ('KETTLE_' + 'LOADED')",
            powershell_integration_bootstrap(script)
        );
        let output = std::process::Command::new(pwsh)
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("KETTLE_LOADED"),
            "status {:?}, stdout {stdout:?}, stderr {:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains(marker));
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn only_a_bare_explicit_powershell_command_is_auto_integrated() {
    let no_arguments: Vec<String> = Vec::new();
    for program in [
        "powershell.exe",
        "PoWeRsHeLl",
        "pwsh.exe",
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        "/opt/microsoft/powershell/7/pwsh",
    ] {
        assert!(should_inject_explicit_powershell(
            program,
            &no_arguments,
            true
        ));
    }
    assert!(!should_inject_explicit_powershell(
        "powershell.exe",
        &no_arguments,
        false
    ));
    for arguments in [
        vec!["-NoLogo".to_string()],
        vec!["-File".to_string(), "profile.ps1".to_string()],
        vec!["-Command".to_string(), "Get-ChildItem".to_string()],
    ] {
        assert!(!should_inject_explicit_powershell(
            "powershell.exe",
            &arguments,
            true
        ));
    }
    for program in ["cmd.exe", "bash", "wsl.exe", "powershell-wrapper.cmd"] {
        assert!(!should_inject_explicit_powershell(
            program,
            &no_arguments,
            true
        ));
    }
}
