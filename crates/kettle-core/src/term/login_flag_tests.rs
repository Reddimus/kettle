use super::{pane_command, prog_accepts_login_flag};

/// An explicit `command = …` only gets `-l` for a POSIX
/// shell — never wsl.exe (where `-l` lists distros) or a Windows-native
/// shell (pwsh/powershell/cmd reject it).
#[test]
fn prog_accepts_login_flag_excludes_wsl_and_windows_shells() {
    // POSIX shells (and unknown progs) honor -l.
    assert!(prog_accepts_login_flag("bash"));
    assert!(prog_accepts_login_flag("/bin/zsh"));
    assert!(prog_accepts_login_flag("/usr/bin/fish"));
    // Windows-native shells reject -l (path + .exe + case variants).
    assert!(!prog_accepts_login_flag("pwsh.exe"));
    assert!(!prog_accepts_login_flag(
        r"C:\Program Files\PowerShell\7\pwsh.exe"
    ));
    assert!(!prog_accepts_login_flag("powershell.exe"));
    assert!(!prog_accepts_login_flag("CMD.EXE"));
    assert!(!prog_accepts_login_flag("cmd"));
    // wsl.exe is excluded (-l there means "list distros").
    assert!(!prog_accepts_login_flag("wsl.exe"));
    assert!(!prog_accepts_login_flag("wsl"));
}

/// `login-shell = true` with no `command` leaves the user's shell as it
/// starts, already a login shell on Unix, and gives an explicit POSIX
/// shell `-l`. Adding `-l` to the user's shell panicked: the builder for
/// it takes no arguments.
#[test]
fn login_shell_gives_only_an_explicit_shell_its_flag() {
    let users = pane_command(&[], true, false);
    assert!(!users.get_argv().iter().any(|arg| arg == "-l"));
    #[cfg(unix)]
    assert!(users.is_default_prog() && users.get_shell_args().is_empty());
    let explicit = pane_command(&["zsh".to_owned()], true, false);
    assert_eq!(explicit.get_argv(), &["zsh", "-l"]);
    let plain = pane_command(&["zsh".to_owned()], false, false);
    assert_eq!(plain.get_argv(), &["zsh"]);
}
