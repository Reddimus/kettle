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
            "{RESTORE}if [[ -r \"${{ZDOTDIR-$HOME}}/{name}\" || -r \"${{ZDOTDIR-$HOME}}/{name}.zwc\" ]]; then\n  builtin source -- \"${{ZDOTDIR-$HOME}}/{name}\"\nfi\n"
        )
    };
    let rc = format!(
        "{}if [[ -o login ]]; then\n{BORROW}else\n{LOAD}{CLEAN}fi\n",
        user_file(".zshrc")
    );
    // --posix selects ENV without losing the shell's real login state.
    let bashrc = r#"set +o posix
if [ "${KETTLE_BASH_INHERIT_ERREXIT-}" != 1 ]; then
  shopt -u inherit_errexit 2>/dev/null || :
fi
declare +x __kettle_noprofile=${KETTLE_BASH_NOPROFILE-}
declare +x __kettle_norc=${KETTLE_BASH_NORC-}
declare +x __kettle_rcfile=${KETTLE_BASH_RCFILE-$HOME/.bashrc}
case $__kettle_rcfile in
  '~') __kettle_rcfile=~ ;;
  '~/'*) __kettle_rcfile=~/${__kettle_rcfile#'~/'} ;;
esac
if [[ -n $__kettle_rcfile && $__kettle_rcfile != */* ]]; then
  __kettle_rcfile=./$__kettle_rcfile
fi
declare +x __kettle_hooks=${BASH_SOURCE[0]%/*}/kettle.bash
if [ "${KETTLE_BASH_ENV_SET-}" = 1 ]; then
  export ENV="$KETTLE_BASH_USER_ENV"
else
  unset ENV
fi
unset KETTLE_BASH_ENV KETTLE_BASH_USER_ENV KETTLE_BASH_ENV_SET KETTLE_BASH_INHERIT_ERREXIT KETTLE_BASH_NOPROFILE KETTLE_BASH_NORC KETTLE_BASH_RCFILE
if shopt -q login_shell; then
  if [ "$__kettle_noprofile" != 1 ]; then
    [ ! -r /etc/profile ] || . /etc/profile
    if [ -r "$HOME/.bash_profile" ]; then . "$HOME/.bash_profile"
    elif [ -r "$HOME/.bash_login" ]; then . "$HOME/.bash_login"
    elif [ -r "$HOME/.profile" ]; then . "$HOME/.profile"
    fi
  fi
elif [ "$__kettle_norc" != 1 ]; then
  if [ -r /etc/bash.bashrc ]; then . /etc/bash.bashrc
  elif [ -r /etc/bash/bashrc ]; then . /etc/bash/bashrc
  fi
  [ ! -r "$__kettle_rcfile" ] || . "$__kettle_rcfile"
fi
declare -F __kettle_pc >/dev/null || builtin source -- "$__kettle_hooks"
unset __kettle_noprofile __kettle_norc __kettle_rcfile __kettle_hooks
"#
    .to_owned();
    let vendor = r#"set -l __kettle_xdg_entry (string replace -r '/fish/vendor_conf.d/[^/]+$' '' -- (status filename))
set -l __kettle_xdg_entries (string split : -- $XDG_DATA_DIRS)
if contains -- "$__kettle_xdg_entry" $__kettle_xdg_entries
    set -l __kettle_xdg_remaining
    set -l __kettle_xdg_item
    for __kettle_xdg_item in $__kettle_xdg_entries
        if test "$__kettle_xdg_item" != "$__kettle_xdg_entry"
            set -a __kettle_xdg_remaining "$__kettle_xdg_item"
        end
    end
    if test (count $__kettle_xdg_remaining) -gt 0
        set -gx XDG_DATA_DIRS (string join : -- $__kettle_xdg_remaining)
    else if test "$KETTLE_XDG_DATA_DIRS_SET" = 1
        set -gx XDG_DATA_DIRS ""
    else
        set -e XDG_DATA_DIRS
    end
end
set -e __kettle_xdg_entry __kettle_xdg_entries
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

fn install_files(root: &Path, files: &crate::owned_dir::OwnedFiles) -> Option<std::path::PathBuf> {
    crate::owned_dir::install_recent(root, files).ok()
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
            let directory = install_files(&root, &files)?;
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
            "{shell} ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
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
    fn bash_env_startup_preserves_login_logout_and_invocation_rules() {
        let bash = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/bash"
        } else {
            "/bin/bash"
        };
        if !Path::new(bash).is_file() {
            eprintln!("ENV-capable Bash not installed; native injection skipped");
            return;
        }
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let checks = "[[ ! -o posix ]] || exit 31; shopt -q inherit_errexit 2>/dev/null && exit 34; [[ $ENV == /user/original ]] || exit 32; for v in ${!KETTLE_BASH_@}; do exit 33; done;";
        for (name, text) in [
            (
                ".bash_profile",
                format!(
                    "{checks} declare -a user_startup_array=(profile); echo profile >> \"$HOME/trace\"\n"
                ),
            ),
            (".bash_login", "echo login >> \"$HOME/trace\"\n".into()),
            (".profile", "echo fallback >> \"$HOME/trace\"\n".into()),
            (
                ".bashrc",
                format!(
                    "{checks} declare -a user_startup_array=(rc); echo rc >> \"$HOME/trace\"\n"
                ),
            ),
            ("custom-rc", "echo custom >> \"$HOME/trace\"\n".into()),
            (".bash_logout", "echo logout >> \"$HOME/trace\"\n".into()),
        ] {
            std::fs::write(home.join(name), text).unwrap();
        }
        let custom_rc = home.join("custom-rc");
        let default_rc = home.join(".bashrc");
        for (login, noprofile, norc, custom, expected) in [
            (true, false, false, false, "profile\nlogout\n"),
            (true, true, false, false, "logout\n"),
            (true, false, true, false, "profile\nlogout\n"),
            (false, false, false, false, "rc\n"),
            (false, false, true, false, ""),
            (false, false, false, true, "custom\n"),
        ] {
            let scope_check = if !noprofile && login || !login && !norc && !custom {
                "[[ ${user_startup_array[0]-} == profile || ${user_startup_array[0]-} == rc ]] || exit 40;"
            } else {
                ""
            };
            let script = format!(
                "{checks} {scope_check} shopt -q inherit_errexit 2>/dev/null && exit 34; declare -F __kettle_pc >/dev/null || exit 35; shopt -q login_shell; test $? = {} || exit 36; \"$BASH\" --norc --noprofile -ic 'declare -F __kettle_pc >/dev/null && exit 37; test \"$ENV\" = /user/original' || exit 38; exit",
                if login { 0 } else { 1 }
            );
            let mut args = vec!["--posix"];
            if login {
                args.push("--login");
            }
            if noprofile {
                args.push("--noprofile");
            }
            if norc {
                args.push("--norc");
            }
            args.extend(["-ic", &script]);
            run(
                bash,
                &args,
                &home,
                &[
                    ("ENV", directory.join("bashrc").to_str().unwrap()),
                    ("KETTLE_BASH_USER_ENV", "/user/original"),
                    ("KETTLE_BASH_ENV_SET", "1"),
                    ("KETTLE_BASH_NOPROFILE", if noprofile { "1" } else { "" }),
                    ("KETTLE_BASH_NORC", if norc { "1" } else { "" }),
                    (
                        "KETTLE_BASH_RCFILE",
                        if custom {
                            custom_rc.to_str().unwrap()
                        } else {
                            default_rc.to_str().unwrap()
                        },
                    ),
                ],
            );
            assert_eq!(
                std::fs::read_to_string(home.join("trace")).unwrap_or_default(),
                expected
            );
            let _ = std::fs::remove_file(home.join("trace"));
        }
        std::fs::remove_file(home.join(".bash_profile")).unwrap();
        for expected in ["login\nlogout\n", "fallback\nlogout\n"] {
            run(
                bash,
                &[
                    "--posix",
                    "--login",
                    "-ic",
                    "test -z \"${ENV+x}\" || exit 39; exit",
                ],
                &home,
                &[("ENV", directory.join("bashrc").to_str().unwrap())],
            );
            assert_eq!(
                std::fs::read_to_string(home.join("trace")).unwrap(),
                expected
            );
            std::fs::remove_file(home.join("trace")).unwrap();
            let _ = std::fs::remove_file(home.join(".bash_login"));
        }
        for (saved, condition) in [
            (false, "test -z \"${ENV+x}\""),
            (true, "test \"${ENV+x}:$ENV\" = x:"),
        ] {
            run(
                bash,
                &[
                    "--posix",
                    "--login",
                    "--noprofile",
                    "-ic",
                    &format!("{condition} || exit 41; exit"),
                ],
                &home,
                &[
                    ("ENV", directory.join("bashrc").to_str().unwrap()),
                    ("KETTLE_BASH_ENV_SET", if saved { "1" } else { "" }),
                    ("KETTLE_BASH_USER_ENV", ""),
                    ("KETTLE_BASH_NOPROFILE", "1"),
                ],
            );
        }
        let _ = std::fs::remove_file(home.join("trace"));
        run(
            bash,
            &[
                "--posix",
                "-ic",
                "declare -F __kettle_pc >/dev/null || exit 42; exit 0",
            ],
            &home,
            &[
                ("ENV", directory.join("bashrc").to_str().unwrap()),
                ("KETTLE_BASH_RCFILE", ""),
            ],
        );
        assert!(
            !home.join("trace").exists(),
            "an empty rcfile does not mean .bashrc"
        );
        std::fs::create_dir(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/custom-rc"), "exit 43\n").unwrap();
        let path = format!("{}/bin:/usr/bin:/bin", home.display());
        run(
            bash,
            &[
                "--posix",
                "-ic",
                "declare -F __kettle_pc >/dev/null || exit 44; exit 0",
            ],
            &home,
            &[
                ("ENV", directory.join("bashrc").to_str().unwrap()),
                ("KETTLE_BASH_RCFILE", "custom-rc"),
                ("PATH", &path),
            ],
        );
        assert_eq!(
            std::fs::read_to_string(home.join("trace")).unwrap(),
            "custom\n"
        );
    }

    #[test]
    fn competing_startup_variants_keep_the_first_shells_files() {
        let temp = crate::test_tempdir();
        let first = files(&ShellStartup::default()).unwrap();
        let directory = install_files(temp.path(), &first).unwrap();
        let second = files(&ShellStartup {
            zsh: Some("/agent/changed".into()),
            ..Default::default()
        })
        .unwrap();
        let competing = install_files(temp.path(), &second).unwrap();
        assert_ne!(directory, competing);
        assert_eq!(crate::owned_dir::verify(&directory, &first), Ok(()));
        assert!(directory.join("kettle.zsh").is_file());
        crate::owned_dir::remove_tree(&directory).unwrap();
        crate::owned_dir::remove_tree(&competing).unwrap();
    }

    #[test]
    fn zsh_wrappers_load_compiled_only_startup_files() {
        if !Path::new("/bin/zsh").is_file() {
            eprintln!("zsh not installed; compiled startup skipped");
            return;
        }
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        for name in [".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(home.join(name), format!("echo {name} >> \"$HOME/trace\"\n")).unwrap();
            run(
                "/bin/zsh",
                &[
                    "-fc",
                    &format!("zcompile -- {}", quote(&home.join(name)).unwrap()),
                ],
                &home,
                &[],
            );
            std::fs::remove_file(home.join(name)).unwrap();
        }
        run(
            "/bin/zsh",
            &["-lic", "(( $+functions[__kettle_precmd] )) || exit 21"],
            &home,
            &[
                ("ZDOTDIR", directory.join("zsh").to_str().unwrap()),
                ("KETTLE_ZDOTDIR_SET", ""),
                ("KETTLE_ZDOTDIR", ""),
            ],
        );
        assert_eq!(
            std::fs::read_to_string(home.join("trace")).unwrap_or_default(),
            ".zprofile\n.zshrc\n.zlogin\n"
        );
    }

    #[test]
    fn fish_preserves_xdg_changes_from_earlier_user_snippets() {
        let Some(fish) = ["/opt/homebrew/bin/fish", "/usr/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        else {
            eprintln!("fish not installed; early snippet skipped");
            return;
        };
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        let config = home.join(".config/fish/conf.d");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("00-env.fish"),
            "set -gx XDG_DATA_DIRS /user/changed\n",
        )
        .unwrap();
        run(
            fish,
            &[
                "-ic",
                "test \"$XDG_DATA_DIRS\" = /user/changed; or exit 21; set -q KETTLE_XDG_DATA_DIRS; and exit 22; set -q KETTLE_XDG_DATA_DIRS_SET; and exit 23; emit fish_prompt; functions -q __kettle_osc7; or exit 24",
            ],
            &home,
            &[
                ("XDG_DATA_DIRS", directory.to_str().unwrap()),
                ("KETTLE_XDG_DATA_DIRS_SET", "1"),
                ("KETTLE_XDG_DATA_DIRS", "/original/share"),
            ],
        );
    }

    #[test]
    fn regression_fish_colon_xdg_does_not_inject_nested_shells() {
        let Some(fish) = ["/opt/homebrew/bin/fish", "/usr/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        else {
            eprintln!("Fish not installed; colon XDG regression skipped");
            return;
        };
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let xdg = format!("{}:/usr/local/share:/usr/share", directory.display());
        for list in [false, true] {
            let script = if list {
                format!(
                    "set -gx XDG_DATA_DIRS (string split : -- $XDG_DATA_DIRS); source {};",
                    quote(&directory.join("fish/vendor_conf.d/00-kettle.fish")).unwrap()
                )
            } else {
                String::new()
            };
            run(
                fish,
                &[
                    if list { "--no-config" } else { "--interactive" },
                    "-ic",
                    &format!(
                        "{script} test \"$XDG_DATA_DIRS\" = /usr/local/share:/usr/share; or exit 51; emit fish_prompt; functions -q __kettle_osc7; or exit 52; fish -ic 'emit fish_prompt; functions -q __kettle_osc7; and exit 53; test \"$XDG_DATA_DIRS\" = /usr/local/share:/usr/share'"
                    ),
                ],
                &home,
                &[("XDG_DATA_DIRS", &xdg), ("KETTLE_XDG_DATA_DIRS_SET", "1")],
            );
        }
    }

    fn regression_bash() -> Option<&'static str> {
        let bash = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/bash"
        } else {
            "/bin/bash"
        };
        if Path::new(bash).is_file() {
            Some(bash)
        } else {
            eprintln!("modern Bash not installed; native regression skipped");
            None
        }
    }

    #[test]
    fn regression_bash_system_rc_precedes_personal_rc_once() {
        let Some(bash) = regression_bash() else {
            return;
        };
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let first = home.join("system-first");
        let second = home.join("system-second");
        // Stage the absolute system locations without changing host configuration.
        let rc = std::fs::read_to_string(directory.join("bashrc"))
            .unwrap()
            .replace("/etc/bash.bashrc", first.to_str().unwrap())
            .replace("/etc/bash/bashrc", second.to_str().unwrap());
        let startup = home.join("bashrc");
        std::fs::write(&startup, rc).unwrap();
        std::fs::copy(directory.join("kettle.bash"), home.join("kettle.bash")).unwrap();
        std::fs::write(&second, "echo second >> \"$HOME/trace\"\n").unwrap();
        std::fs::write(home.join(".bashrc"), "echo personal >> \"$HOME/trace\"\n").unwrap();
        for (first_exists, norc, login, expected) in [
            (true, false, false, "first\npersonal\n"),
            (false, false, false, "second\npersonal\n"),
            (true, true, false, ""),
            (true, false, true, ""),
        ] {
            if first_exists {
                std::fs::write(&first, "echo first >> \"$HOME/trace\"\n").unwrap();
            } else {
                std::fs::remove_file(&first).unwrap();
            }
            let mut args = vec!["--posix", "--noprofile"];
            if login {
                args.push("--login");
            }
            args.extend([
                "-ic",
                "declare -F __kettle_pc >/dev/null || exit 54; exit 0",
            ]);
            run(
                bash,
                &args,
                &home,
                &[
                    ("ENV", startup.to_str().unwrap()),
                    ("KETTLE_BASH_NOPROFILE", "1"),
                    ("KETTLE_BASH_NORC", if norc { "1" } else { "" }),
                ],
            );
            assert_eq!(
                std::fs::read_to_string(home.join("trace")).unwrap_or_default(),
                expected
            );
            let _ = std::fs::remove_file(home.join("trace"));
        }
    }

    #[test]
    fn regression_bash_preserves_inherited_inherit_errexit() {
        let Some(bash) = regression_bash() else {
            return;
        };
        let (temp, directory) = fixture();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let check = "shopt -q inherit_errexit || exit 55; (set -e; value=$(false; printf wrong); printf wrong-after) > \"$HOME/errexit\"; result=$?; [[ $result == 1 && ! -s $HOME/errexit ]] || exit 56;";
        std::fs::write(home.join(".bashrc"), format!("{check}\nprintf startup\n")).unwrap();
        let output = run(
            bash,
            &[
                "--posix",
                "-ic",
                &format!("{check} declare -F __kettle_pc >/dev/null || exit 57; exit 0"),
            ],
            &home,
            &[
                ("ENV", directory.join("bashrc").to_str().unwrap()),
                ("BASHOPTS", "inherit_errexit"),
                ("KETTLE_BASH_INHERIT_ERREXIT", "1"),
            ],
        );
        assert!(output.contains("startup"));
    }

    #[test]
    fn regression_bash_custom_rcfile_expands_home_tilde() {
        let Some(bash) = regression_bash() else {
            return;
        };
        let (temp, directory) = fixture();
        let home = temp.path().join("home with spaces");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join(".customrc"), "printf custom-startup\n").unwrap();
        for option in ["--rcfile", "--init-file"] {
            let native = run(bash, &[option, "~/.customrc", "-ic", "exit 0"], &home, &[]);
            assert!(native.contains("custom-startup"));
            let injected = run(
                bash,
                &[
                    "--posix",
                    "-ic",
                    "declare -F __kettle_pc >/dev/null || exit 58; exit 0",
                ],
                &home,
                &[
                    ("ENV", directory.join("bashrc").to_str().unwrap()),
                    ("KETTLE_BASH_RCFILE", "~/.customrc"),
                ],
            );
            assert_eq!(injected, native, "{option}");
        }
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
