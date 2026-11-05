//! Automatic shell startup, using the same checked files as agent helpers.
#![cfg(unix)]

use kettle_core::shell_startup::ShellStartup;
use std::path::Path;

fn quote(path: &Path) -> Option<String> {
    Some(format!("'{}'", path.to_str()?.replace('\'', "'\\''")))
}

const RESTORE: &str = "if [[ $__kettle_zdotdir_set == 1 ]]; then\n  typeset -g +x ZDOTDIR=\"$__kettle_zdotdir\"\n  if [[ $__kettle_zdotdir_type == *export* ]]; then\n    typeset -gx ZDOTDIR\n  fi\nelse\n  unset ZDOTDIR\nfi\n";
const BORROW: &str = "typeset -g +x __kettle_zdotdir=${ZDOTDIR-}\ntypeset -g +x __kettle_zdotdir_set=${ZDOTDIR+1}\ntypeset -g +x __kettle_zdotdir_type=${(t)ZDOTDIR-}\ntypeset -g +x ZDOTDIR=\"$__kettle_startup/zsh\"\n";
const LOAD: &str = "builtin source -- \"$__kettle_startup/kettle.zsh\"\n";
const CLEAN: &str =
    "unset __kettle_zdotdir __kettle_zdotdir_set __kettle_zdotdir_type __kettle_startup\n";

fn files(agent: &ShellStartup) -> Option<crate::owned_dir::OwnedFiles> {
    let user_env = if let Some(path) = &agent.zsh {
        format!("builtin source -- {}\n", quote(&path.join(".zshenv"))?)
    } else {
        kettle_core::shell_startup::zshenv("")
    };
    let env = format!("typeset -g +x __kettle_startup=${{ZDOTDIR%/zsh}}\n{user_env}{BORROW}");
    let user_file = |name: &str| {
        format!(
            "{RESTORE}if [[ -r \"${{ZDOTDIR-$HOME}}/{name}\" ]]; then\n  builtin source -- \"${{ZDOTDIR-$HOME}}/{name}\"\nfi\n"
        )
    };
    let rc = format!(
        "{}if [[ -o login ]]; then\n{BORROW}else\n{LOAD}{CLEAN}fi\n",
        user_file(".zshrc")
    );
    // Unset the one-shot flag before user startup can launch another shell.
    let bashrc = r#"if [ "${KETTLE_BASH_LOGIN-}" = 1 ]; then
  unset KETTLE_BASH_LOGIN
  [ ! -r /etc/profile ] || . /etc/profile
  if [ -r "$HOME/.bash_profile" ]; then . "$HOME/.bash_profile"
  elif [ -r "$HOME/.bash_login" ]; then . "$HOME/.bash_login"
  elif [ -r "$HOME/.profile" ]; then . "$HOME/.profile"
  fi
else
  unset KETTLE_BASH_LOGIN
  [ ! -r "$HOME/.bashrc" ] || . "$HOME/.bashrc"
fi
"#;
    // The rcfile's directory is available without any exported helper path.
    let bashrc = format!(
        "{bashrc}\ndeclare -F __kettle_pc >/dev/null || builtin source -- \"${{BASH_SOURCE[0]%/*}}/kettle.bash\"\n"
    );
    let vendor = r#"if test "$KETTLE_XDG_DATA_DIRS_SET" = 1
    set -gx XDG_DATA_DIRS "$KETTLE_XDG_DATA_DIRS"
else
    set -e XDG_DATA_DIRS
end
set -e KETTLE_XDG_DATA_DIRS
set -e KETTLE_XDG_DATA_DIRS_SET
if status is-interactive
    set -gu __kettle_startup_file (status filename)
    function __kettle_auto_start --on-event fish_prompt
        if not functions -q __kettle_osc7
            source (string replace -r '/fish/vendor_conf.d/[^/]+$' '/kettle.fish' -- "$__kettle_startup_file")
            __kettle_prompt
        end
        set -e __kettle_startup_file
        functions -e __kettle_auto_start
    end
end
"#;
    Some(crate::owned_dir::OwnedFiles::new(
        "shell-integration",
        String::new(),
        env!("CARGO_PKG_VERSION").into(),
        vec![
            ("zsh/.zshenv", env),
            (
                "zsh/.zprofile",
                format!("{}{BORROW}", user_file(".zprofile")),
            ),
            ("zsh/.zshrc", rc),
            (
                "zsh/.zlogin",
                format!("{}{LOAD}{CLEAN}", user_file(".zlogin")),
            ),
            ("bashrc", bashrc),
            ("fish/vendor_conf.d/00-kettle.fish", vendor.into()),
            (
                "kettle.bash",
                include_str!("../../../shell-integration/kettle.bash").into(),
            ),
            (
                "kettle.zsh",
                include_str!("../../../shell-integration/kettle.zsh").into(),
            ),
            (
                "kettle.fish",
                include_str!("../../../shell-integration/kettle.fish").into(),
            ),
        ],
    ))
}

/// Prepare only when automatic integration is enabled; failure leaves the
/// original startup untouched. Verification rejects links and writable files.
pub(crate) fn for_new_pane(mut startup: ShellStartup, enabled: bool) -> ShellStartup {
    if enabled {
        let prepared = (|| {
            let files = files(&startup)?;
            let root = crate::codex_shell::startup_root()?
                .parent()?
                .join("shell-integration");
            let directory = crate::owned_dir::install(&root, &files, |_| None, true).ok()?;
            crate::owned_dir::verify(&directory, &files).ok()?;
            Some(directory)
        })();
        startup.integration = prepared;
    }
    startup
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn fixture() -> (kettle_test_support::PrivateTempDir, std::path::PathBuf) {
        let temp = crate::test_tempdir();
        let files = files(&ShellStartup::default()).unwrap();
        let directory = temp.path().join(files.directory_name());
        crate::owned_dir::write(temp.path(), &directory, &files);
        crate::owned_dir::verify(&directory, &files).unwrap();
        (temp, directory)
    }

    fn run(shell: &str, args: &[&str], home: &Path, env: &[(&str, &str)]) -> String {
        let mut command = Command::new(shell);
        command
            .current_dir(home)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin")
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C")
            .args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn bash_32_rcfile_runs_user_startup_once_without_child_injection() {
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join(".bashrc"), "echo rc >> \"$HOME/trace\"\n").unwrap();
        std::fs::write(
            home.join(".bash_profile"),
            "echo profile >> \"$HOME/trace\"\n. \"$HOME/.bashrc\"\n",
        )
        .unwrap();
        for (login, expected) in [("0", "rc\n"), ("1", "profile\nrc\n")] {
            let script = "declare -F __kettle_pc >/dev/null || exit 20; test -z \"${KETTLE_BASH_LOGIN+x}\"; /bin/bash --noprofile --norc -c 'test -z \"${KETTLE_BASH_LOGIN+x}\" && test -z \"${ENV+x}\" && test -z \"${BASH_ENV+x}\"'";
            run(
                "/bin/bash",
                &[
                    "--rcfile",
                    directory.join("bashrc").to_str().unwrap(),
                    "-ic",
                    script,
                ],
                &home,
                &[("KETTLE_BASH_LOGIN", login)],
            );
            assert_eq!(
                std::fs::read_to_string(home.join("trace")).unwrap(),
                expected
            );
            std::fs::remove_file(home.join("trace")).unwrap();
        }
    }

    #[test]
    fn zsh_login_and_nonlogin_restore_startup_and_do_not_inject_nested_shells() {
        if !Path::new("/bin/zsh").is_file() {
            eprintln!("zsh not installed; native startup skipped");
            return;
        }
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        for name in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(home.join(name), format!("echo {name} >> \"$HOME/trace\"\n")).unwrap();
        }
        std::fs::write(
            home.join(".zlogin"),
            "echo .zlogin >> \"$HOME/trace\"\nprecmd_functions=()\npreexec_functions=()\nPS1=own\n",
        )
        .unwrap();
        for (args, expected) in [
            ("-ic", ".zshenv\n.zshrc\n"),
            ("-lic", ".zshenv\n.zprofile\n.zshrc\n.zlogin\n"),
        ] {
            for original in [None, Some(""), home.to_str()] {
                let condition = match original {
                    None => "[[ ! -v ZDOTDIR ]]".to_owned(),
                    Some(value) => {
                        format!("[[ $ZDOTDIR == {} ]]", quote(Path::new(value)).unwrap())
                    }
                };
                let script = format!(
                    "(( $+functions[__kettle_precmd] && ${{precmd_functions[(Ie)__kettle_precmd]}} )) || exit 21; {condition} || exit 22; [[ ! -v KETTLE_ZDOTDIR ]] || exit 23; zsh -ic '(( ! $+functions[__kettle_precmd] ))'"
                );
                run(
                    "/bin/zsh",
                    &[args, &script],
                    &home,
                    &[
                        ("ZDOTDIR", directory.join("zsh").to_str().unwrap()),
                        (
                            "KETTLE_ZDOTDIR_SET",
                            if original.is_some() { "1" } else { "" },
                        ),
                        ("KETTLE_ZDOTDIR", original.unwrap_or("")),
                    ],
                );
                let trace = std::fs::read_to_string(home.join("trace")).unwrap_or_default();
                let expected = if original == Some("") {
                    String::new()
                } else {
                    format!("{expected}.zshenv\n.zshrc\n")
                };
                assert_eq!(trace, expected);
                let _ = std::fs::remove_file(home.join("trace"));
            }
        }
        std::fs::write(
            home.join(".zshenv"),
            "echo .zshenv >> \"$HOME/trace\"\nset -u\ntypeset -g +x ZDOTDIR=\"$HOME\"\n",
        )
        .unwrap();
        run(
            "/bin/zsh",
            &[
                "-lic",
                "[[ ${(t)ZDOTDIR} != *export* ]] || exit 25; /bin/sh -c 'test -z \"${ZDOTDIR+x}\"'",
            ],
            &home,
            &[
                ("ZDOTDIR", directory.join("zsh").to_str().unwrap()),
                ("KETTLE_ZDOTDIR_SET", ""),
                ("KETTLE_ZDOTDIR", ""),
            ],
        );
    }

    #[test]
    fn fish_vendor_restores_xdg_before_user_config_and_loads_after_it() {
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        let config = home.join(".config/fish");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.fish"),
            "echo config >> $HOME/trace\nset -q KETTLE_XDG_DATA_DIRS; and exit 21\n",
        )
        .unwrap();
        let fish = ["/opt/homebrew/bin/fish", "/usr/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file());
        let Some(fish) = fish else {
            eprintln!("fish not installed; native vendor startup skipped");
            return;
        };
        let output = run(
            fish,
            &[
                "-ic",
                "emit fish_prompt; functions -q __kettle_osc7; or exit 22; set -q KETTLE_XDG_DATA_DIRS; and exit 23; set -q XDG_DATA_DIRS; and exit 24; true",
            ],
            &home,
            &[
                ("XDG_DATA_DIRS", directory.to_str().unwrap()),
                ("KETTLE_XDG_DATA_DIRS_SET", ""),
                ("KETTLE_XDG_DATA_DIRS", ""),
            ],
        );
        assert_eq!(
            output.matches("\x1b]133;A").count(),
            1,
            "the first prompt is integrated"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("trace")).unwrap(),
            "config\n"
        );
    }

    #[test]
    fn off_preserves_agent_startup_without_preparing_any_files() {
        let startup = ShellStartup {
            zsh: Some("/agent/zsh".into()),
            fish: Some("agent".into()),
            ..Default::default()
        };
        assert_eq!(for_new_pane(startup.clone(), false), startup);
    }
}
