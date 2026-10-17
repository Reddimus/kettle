use super::{
    POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM, POWERSHELL_BOOTSTRAP_MAX_CHARS,
    POWERSHELL_BOOTSTRAP_PREFIX, POWERSHELL_BOOTSTRAP_SUFFIX, POWERSHELL_INTEGRATION,
    powershell_integration_bootstrap, should_inject_explicit_powershell,
};

#[test]
fn bootstrap_preserves_the_exact_source_and_stays_below_the_windows_limit() {
    assert_eq!(
        powershell_integration_bootstrap("# comment\nAé\r\n"),
        format!("{POWERSHELL_BOOTSTRAP_PREFIX}IyBjb21tZW50CkHDqQ0K{POWERSHELL_BOOTSTRAP_SUFFIX}"),
        "comments, blank lines, Unicode, and line endings are executable PowerShell source"
    );
    let bootstrap = powershell_integration_bootstrap(POWERSHELL_INTEGRATION);
    assert!(bootstrap.len() <= POWERSHELL_BOOTSTRAP_MAX_CHARS);
    assert!(
        bootstrap.len() + POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM <= POWERSHELL_BOOTSTRAP_MAX_CHARS,
        "the embedded integration needs its reserved source-growth headroom"
    );
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
