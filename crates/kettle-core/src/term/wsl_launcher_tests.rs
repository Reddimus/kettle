use super::is_wsl_launcher;

/// The `login_shell` `-l` injection must be suppressed for the
/// WSL launcher (bare name, `.exe`, full path, any case) because
/// `wsl.exe -l` lists distros instead of opening a shell.
#[test]
fn recognizes_wsl_launcher_forms() {
    assert!(is_wsl_launcher("wsl"));
    assert!(is_wsl_launcher("wsl.exe"));
    assert!(is_wsl_launcher("WSL.EXE"));
    assert!(is_wsl_launcher(r"C:\Windows\System32\wsl.exe"));
    assert!(is_wsl_launcher("/mnt/c/Windows/System32/wsl.exe"));
}

/// Real shells must NOT be treated as wsl — they still get `-l`.
#[test]
fn does_not_match_other_shells() {
    for p in [
        "bash",
        "/bin/zsh",
        "pwsh.exe",
        "powershell.exe",
        "cmd.exe",
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        "wsltty.exe", // stem is "wsltty", not "wsl"
    ] {
        assert!(!is_wsl_launcher(p), "{p} should not match wsl");
    }
}
