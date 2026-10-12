//! The CLI speaks one language whatever the UI's: help, listings, shell
//! completions, the default config and a config check are byte-identical
//! under an English and a Spanish OS locale, and with `language = en` or
//! `language = es` in the config. Only the GUI's own text is translated
//! (see TRANSLATING.md).

use std::path::Path;
use std::process::Command;

/// Where every command reads its config in `home`: `XDG_CONFIG_HOME` names
/// it on every OS, Windows included.
fn config_path(home: &Path) -> std::path::PathBuf {
    home.join(".config").join("kettle").join("config")
}

/// What `kettle args` printed, and how it exited, with `locale` as the OS
/// locale and, at the config path every command reads, a config whose
/// `language` is `language`, in a home of its own.
fn run(
    home: &Path,
    args: &[&str],
    locale: &str,
    language: &str,
) -> (Option<i32>, Vec<u8>, Vec<u8>) {
    let config = config_path(home);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!("language = {language}\nupdate-check = false\n"),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_kettle"));
    command
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", locale)
        .env("LC_ALL", locale)
        .env("LC_MESSAGES", locale)
        .env("LANGUAGE", locale.split('_').next().unwrap_or(locale));
    // Windows starts no process without its system root.
    for kept in ["SystemRoot", "windir"] {
        if let Some(value) = std::env::var_os(kept) {
            command.env(kept, value);
        }
    }
    let output = command.output().unwrap();
    (output.status.code(), output.stdout, output.stderr)
}

#[test]
fn the_cli_is_the_same_in_every_language() {
    let home = kettle_test_support::private_tempdir("kettle-cli-language-");
    let commands: &[&[&str]] = &[
        &["--help"],
        &["exec", "--help"],
        &["ctl", "--help"],
        &["mcp", "--help"],
        &["show", "--help"],
        &["video-frames", "--help"],
        &["agent-setup", "--help"],
        &["update", "--help"],
        &["--list-actions"],
        &["--list-themes"],
        &["--list-keybinds"],
        &["--print-completions", "zsh"],
        &["--print-completions", "bash"],
        &["--print-completions", "fish"],
        &["--print-default-config"],
        &["--check-config"],
        &["--version"],
    ];
    for args in commands {
        let english = run(home.path(), args, "en_US.UTF-8", "en");
        assert_eq!(
            english.0,
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&english.2)
        );
        assert!(!english.1.is_empty(), "{args:?} printed nothing");
        // The config check names the config it read: the fixture's.
        if args == &["--check-config"] {
            let printed = String::from_utf8_lossy(&english.1);
            let config = config_path(home.path());
            assert!(
                printed.contains(&*config.to_string_lossy()),
                "the fixture was not read: {printed}"
            );
        }
        for (locale, language) in [
            ("es_MX.UTF-8", "es"),
            ("es_ES.UTF-8", "en"),
            ("en_US.UTF-8", "es"),
        ] {
            let other = run(home.path(), args, locale, language);
            assert_eq!(
                other, english,
                "{args:?} under {locale} with language = {language}"
            );
        }
    }
}
