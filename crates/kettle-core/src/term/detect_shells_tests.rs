#[test]
fn shell_cache_lookup_never_waits_for_a_pending_detector() {
    use std::sync::{Arc, OnceLock, mpsc};
    use std::time::Duration;
    let cache = Arc::new(OnceLock::new());
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let detecting = cache.clone();
    let detector = std::thread::spawn(move || {
        detecting.get_or_init(|| {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            vec![("Test shell".into(), vec!["test-shell".into()])]
        });
    });
    started_rx.recv().unwrap();
    let (lookup_tx, lookup_rx) = mpsc::channel();
    let looking = cache.clone();
    let lookup = std::thread::spawn(move || {
        lookup_tx.send(super::cached_shells_from(&looking)).unwrap();
    });
    let pending = lookup_rx.recv_timeout(Duration::from_millis(100));
    finish_tx.send(()).unwrap();
    detector.join().unwrap();
    lookup.join().unwrap();
    assert_eq!(
        pending,
        Ok(None),
        "UI cache lookup must return while detection is pending"
    );
    assert_eq!(
        super::cached_shells_from(&cache).unwrap()[0].0,
        "Test shell"
    );
}

/// Drift guard: both Windows probes use the bounded runner with the same
/// two-second deadline. The runner's tests exercise timeout cleanup and capture.
#[test]
fn list_wsl_distros_is_time_bounded() {
    let src = super::production_source();
    for function in ["fn list_wsl_distros()", "fn vs_dev_info()"] {
        let body = src
            .split_once(function)
            .expect("probe present")
            .1
            .split_once("\n}")
            .expect("probe body")
            .0;
        assert!(
            body.contains(
                "shell_probe::output(command, std::time::Duration::from_secs(2), 64 * 1024)"
            ),
            "{function} must use the bounded runner with a two-second deadline and 64 KiB capture limit"
        );
        assert!(
            !body.contains(".output()"),
            "{function} must not bypass the bounded runner"
        );
    }
}

#[test]
fn parse_wsl_distros_strips_bom_nul_blanks_crlf() {
    // Simulated `wsl -l -q` decoded text: a leading UTF-16 BOM, CRLF line
    // endings, a blank line, and a trailing-NUL artifact.
    let text = "\u{feff}Ubuntu\r\nDebian\r\n\r\nkali-linux\u{0}\r\n";
    assert_eq!(
        super::parse_wsl_distros(text),
        vec!["Ubuntu", "Debian", "kali-linux"]
    );
    assert!(super::parse_wsl_distros("").is_empty());
    assert!(super::parse_wsl_distros("\u{feff}\r\n  \r\n").is_empty());
}

/// Dropdown parity: the full Windows menu in Windows Terminal's
/// order, with every probe succeeding. Runs on every platform — the
/// builder is pure over injected closures.
#[test]
fn detect_shells_windows_orders_like_windows_terminal() {
    let avail = |e: &str| matches!(e, "cmd.exe" | "powershell.exe" | "pwsh.exe" | "wsl.exe");
    let distros = || vec!["Ubuntu".to_string()];
    let vs = || {
        Some(super::VsDevInfo {
            install_path: r"C:\Program Files\Microsoft Visual Studio\2022\Community".to_string(),
            has_dev_cmd_bat: true,
            has_dev_shell_dll: true,
        })
    };
    let git = || {
        Some(std::path::PathBuf::from(
            r"C:\Program Files\Git\bin\bash.exe",
        ))
    };
    let got = super::detect_shells_windows(avail, distros, vs, git);
    let labels: Vec<&str> = got.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "PowerShell",
            "Windows PowerShell",
            "Command Prompt",
            "WSL: Ubuntu",
            "Developer Command Prompt for VS 2022",
            "Developer PowerShell for VS 2022",
            "Git Bash",
        ],
        "Windows Terminal's dropdown order (Ctrl+Shift+N indexes this)"
    );
    // Git Bash spawns an interactive login shell from its full path.
    let (_, bash_argv) = got.iter().find(|(l, _)| l == "Git Bash").unwrap();
    assert_eq!(
        bash_argv.as_slice(),
        [r"C:\Program Files\Git\bin\bash.exe", "-i", "-l"]
    );
}

/// Dropdown parity: hosts without VS / Git get no phantom rows.
#[test]
fn detect_shells_windows_skips_vs_and_git_when_absent() {
    let avail = |e: &str| matches!(e, "cmd.exe" | "pwsh.exe" | "wsl.exe");
    let got = super::detect_shells_windows(avail, || vec!["Ubuntu".into()], || None, || None);
    assert!(got.iter().any(|(l, _)| l == "PowerShell"));
    assert!(got.iter().any(|(l, _)| l == "WSL: Ubuntu"));
    // powershell.exe was NOT "available" → Windows PowerShell absent.
    assert!(!got.iter().any(|(l, _)| l == "Windows PowerShell"));
    assert!(!got.iter().any(|(l, _)| l.starts_with("Developer")));
    assert!(!got.iter().any(|(l, _)| l == "Git Bash"));
}

/// Dropdown parity: the Developer PowerShell host prefers pwsh 7
/// and falls back to Windows PowerShell.
#[test]
fn vs_dev_powershell_host_prefers_pwsh() {
    let vs = || {
        Some(super::VsDevInfo {
            install_path: r"C:\VS\2022\BuildTools".to_string(),
            has_dev_cmd_bat: false,
            has_dev_shell_dll: true,
        })
    };
    // Only powershell.exe on PATH → it hosts the dev shell.
    let got = super::detect_shells_windows(|e| e == "powershell.exe", Vec::new, vs, || None);
    let (_, argv) = got
        .iter()
        .find(|(l, _)| l.starts_with("Developer PowerShell"))
        .unwrap();
    assert_eq!(argv[0], "powershell.exe");
}

/// Dropdown parity: the dev-shell argvs are byte-pinned — these
/// strings are what actually spawns, so a drift here breaks the feature
/// invisibly (the menu row would still render).
#[test]
fn vs_dev_argv_strings_are_canonical() {
    let argv = super::vs_dev_cmd_argv(r"C:\Program Files\Microsoft Visual Studio\2022\Community");
    assert_eq!(
        argv.as_slice(),
        [
            "cmd.exe",
            "/k",
            r"C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\VsDevCmd.bat",
        ]
    );
    let argv = super::vs_dev_powershell_argv("pwsh.exe", r"C:\VS\O'Brien\2022\Community");
    assert_eq!(argv[0], "pwsh.exe");
    assert_eq!(argv[1], "-NoExit");
    assert_eq!(argv[2], "-Command");
    // Single quotes in the path are doubled (PowerShell escaping); the
    // command imports DevShell.dll then enters the dev environment
    // without changing directory.
    assert_eq!(
        argv[3],
        r"&{ Import-Module 'C:\VS\O''Brien\2022\Community\Common7\Tools\Microsoft.VisualStudio.DevShell.dll'; Enter-VsDevShell -VsInstallPath 'C:\VS\O''Brien\2022\Community' -SkipAutomaticLocation }"
    );
}

#[test]
fn parse_vs_install_path_takes_first_nonempty_line() {
    assert_eq!(
        super::parse_vs_install_path("\r\n C:\\VS\\2022\\Community \r\n"),
        Some(r"C:\VS\2022\Community".to_string())
    );
    assert_eq!(super::parse_vs_install_path("\n  \n"), None);
}

#[test]
fn vs_year_from_install_path_finds_the_year_segment() {
    assert_eq!(
        super::vs_year_from_install_path(
            r"C:\Program Files\Microsoft Visual Studio\2022\Community"
        ),
        Some("2022")
    );
    assert_eq!(
        super::vs_year_from_install_path("/c/vs/2026/Preview"),
        Some("2026")
    );
    assert_eq!(super::vs_year_from_install_path(r"C:\VS\Preview"), None);
}

// Windows-only: `git_bash_from_git_exe` reasons over `std::path::Path`,
// and `\` is only a separator on Windows targets — on Linux these
// fixtures parse as single opaque components (the production caller,
// `git_bash_path`, is `cfg(windows)` for the same reason).
#[cfg(windows)]
#[test]
fn git_bash_from_git_exe_covers_cmd_bin_and_mingw_layouts() {
    use std::path::{Path, PathBuf};
    // <root>\cmd\git.exe → <root>\bin\bash.exe
    let c = super::git_bash_from_git_exe(Path::new(r"C:\Git\cmd\git.exe"));
    assert_eq!(c, vec![PathBuf::from(r"C:\Git\bin\bash.exe")]);
    // <root>\mingw64\bin\git.exe → <root>\bin\bash.exe (+ sibling)
    let c = super::git_bash_from_git_exe(Path::new(r"C:\Git\mingw64\bin\git.exe"));
    assert!(c.contains(&PathBuf::from(r"C:\Git\bin\bash.exe")));
    // flat dir → sibling bash.exe
    let c = super::git_bash_from_git_exe(Path::new(r"D:\tools\git.exe"));
    assert_eq!(c, vec![PathBuf::from(r"D:\tools\bash.exe")]);
}

#[cfg(windows)]
#[test]
fn detect_shells_windows_never_empty() {
    let got = super::detect_shells_windows(|_| false, Vec::new, || None, || None);
    assert_eq!(
        got,
        vec![("Command Prompt".to_string(), vec!["cmd.exe".to_string()])]
    );
}

#[cfg(not(windows))]
#[test]
fn detect_shells_unix_shell_env_first_and_dedupes() {
    let avail = |e: &str| matches!(e, "bash" | "zsh" | "fish");
    let got = super::detect_shells_unix(Some("/bin/zsh".to_string()), avail);
    // $SHELL=zsh is first (label = basename); the detected `zsh` isn't a dup.
    assert_eq!(got[0], ("zsh".to_string(), vec!["/bin/zsh".to_string()]));
    assert_eq!(got.iter().filter(|(_, a)| a[0].ends_with("zsh")).count(), 1);
    assert!(got.iter().any(|(l, _)| l == "bash"));
    assert!(got.iter().any(|(l, _)| l == "fish"));
}

#[cfg(not(windows))]
#[test]
fn detect_shells_unix_never_empty() {
    let got = super::detect_shells_unix(None, |_| false);
    assert_eq!(
        got,
        vec![("Shell".to_string(), vec!["/bin/sh".to_string()])]
    );
}
