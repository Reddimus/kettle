//! Startup additions for a pane's bash, zsh or fish. Agent helpers and
//! automatic integration share this launch boundary. Only the default shell
//! or an explicit command naming a bare shell receives additions. Bash also
//! accepts startup-only flags; wrappers, scripts and commands remain exact.

use portable_pty::CommandBuilder;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Startup additions for a pane's shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellStartup {
    /// Checked private automatic integration files, independent of agent helpers.
    pub integration: Option<PathBuf>,
    /// A directory whose `.zshenv`, written with [`zshenv`], zsh reads in
    /// place of the user's.
    pub zsh: Option<PathBuf>,
    /// fish code to run after the user's configuration.
    pub fish: Option<String>,
}

/// Where the borrowed `ZDOTDIR` waits for Kettle's `.zshenv` to put it back.
pub const ZDOTDIR_SAVED: &str = "KETTLE_ZDOTDIR";

/// `1` when `ZDOTDIR` had a value, even an empty one, and empty when unset.
pub const ZDOTDIR_SAVED_SET: &str = "KETTLE_ZDOTDIR_SET";

/// A `.zshenv` for [`ShellStartup::zsh`]. It puts `ZDOTDIR` back exactly as
/// it was (set, empty or unset), runs `payload`, and then runs the user's
/// `.zshenv` from where zsh would have found it, after which zsh reads the
/// rest of the user's startup files from that `ZDOTDIR`. `payload` is read
/// before any of the user's files, so no alias of theirs can change it.
///
/// One difference from zsh's own startup remains: while the user's
/// `.zshenv` runs, `$0` is its path, as for any file zsh sources.
pub fn zshenv(payload: &str) -> String {
    format!(
        "# Kettle's .zshenv, which zsh reads in place of yours. It puts ZDOTDIR\n\
         # back, runs what Kettle adds, then runs your .zshenv; your other\n\
         # startup files follow as usual.\n\
         if [[ -n \"${{{ZDOTDIR_SAVED_SET}-}}\" ]]; then\n\
         \x20 ZDOTDIR=\"${{{ZDOTDIR_SAVED}-}}\"\n\
         else\n\
         \x20 'builtin' 'unset' 'ZDOTDIR'\n\
         fi\n\
         'builtin' 'unset' '{ZDOTDIR_SAVED}' '{ZDOTDIR_SAVED_SET}'\n\
         {payload}\
         if [[ -r \"${{ZDOTDIR-$HOME}}/.zshenv\" || -r \"${{ZDOTDIR-$HOME}}/.zshenv.zwc\" ]]; then\n\
         \x20 'builtin' 'source' -- \"${{ZDOTDIR-$HOME}}/.zshenv\"\n\
         fi\n"
    )
}

/// The shells Kettle adds startup to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupShell {
    Zsh,
    Fish,
}

/// The shell `program` runs, by its name.
fn startup_shell(program: &Path) -> Option<StartupShell> {
    match program.file_name()?.to_str()? {
        "zsh" => Some(StartupShell::Zsh),
        "fish" => Some(StartupShell::Fish),
        _ => None,
    }
}

/// The largest system `zshenv` Kettle reads to look for `ZDOTDIR`. One
/// larger is taken to name it.
const MAX_ZSHENV_BYTES: u64 = 64 * 1024;

/// The system `zshenv` files a zsh at `program` may read: zsh reads one,
/// fixed when it was built, before any `.zshenv`. Distributions put it in
/// `/etc` or `/etc/zsh`, and a zsh installed under a prefix (Homebrew, for
/// one) in that prefix's `etc`.
fn system_zshenvs(program: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/etc/zshenv"),
        PathBuf::from("/etc/zsh/zshenv"),
    ];
    if let Some(prefix) = program
        .parent()
        .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))
        .and_then(Path::parent)
    {
        let zshenv = prefix.join("etc/zshenv");
        if !candidates.contains(&zshenv) {
            candidates.push(zshenv);
        }
    }
    candidates
}

/// The first of `candidates` that could keep Kettle's `.zshenv` from doing
/// its work. zsh reads its system `zshenv` before Kettle's `.zshenv`, so one
/// that names `ZDOTDIR` would find Kettle's directory there and might keep
/// it, and one that names the `RCS` option may stop zsh reading any
/// `.zshenv`, Kettle's included, leaving the borrowed variable behind.
/// Kettle leaves that zsh alone. zsh ignores case and underscores in option
/// names, so the `RCS` check does too. Whole-line comments are skipped: they
/// change nothing, and Debian, Ubuntu and Fedora ship a `zshenv` whose header
/// comment explains `NO_RCS`. A file too large to read here counts,
/// and one zsh could not read either does not. Nothing here blocks: a FIFO or
/// device is opened without waiting and passed over.
pub fn zshenv_keeping_kettle_out(candidates: &[PathBuf]) -> Option<PathBuf> {
    use std::io::Read as _;
    candidates.iter().find_map(|path| {
        let file = open_without_waiting(path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut text = Vec::new();
        file.take(MAX_ZSHENV_BYTES + 1)
            .read_to_end(&mut text)
            .ok()?;
        let keeps_out = text.len() as u64 > MAX_ZSHENV_BYTES || keeps_kettle_out(&text);
        keeps_out.then(|| path.clone())
    })
}

/// Whether a system `zshenv`'s code names `ZDOTDIR` or the `RCS` option.
/// A line whose first non-blank byte is `#` is a comment wherever it ends up
/// running, so it is skipped; any other line counts, trailing comment and all.
fn keeps_kettle_out(text: &[u8]) -> bool {
    let line_keeps_out = |line: &[u8]| {
        if line
            .windows(b"ZDOTDIR".len())
            .any(|window| window == b"ZDOTDIR")
        {
            return true;
        }
        let folded: Vec<u8> = line
            .iter()
            .filter(|byte| **byte != b'_')
            .map(u8::to_ascii_lowercase)
            .collect();
        folded.windows(b"rcs".len()).any(|window| window == b"rcs") || sets_no_rcs(line)
    };
    let keeps_out = text
        .split(|byte| *byte == b'\n')
        .filter(|line| line.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'#'))
        .any(line_keeps_out);
    if keeps_out || !text.windows(2).any(|pair| pair == b"\\\n") {
        return keeps_out;
    }
    // Also inspect continued commands. Keep the physical-line check above so a
    // backslash inside a comment cannot hide the next line's option command.
    let mut continued = Vec::with_capacity(text.len());
    for line in text.split_inclusive(|byte| *byte == b'\n') {
        continued.extend_from_slice(line.strip_suffix(b"\\\n").unwrap_or(line));
    }
    continued
        .split(|byte| *byte == b'\n')
        .filter(|line| line.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'#'))
        .any(line_keeps_out)
}

fn sets_no_rcs(line: &[u8]) -> bool {
    // Redirections can precede the command or appear between its option words.
    static REDIRECTIONS: std::sync::OnceLock<regex::bytes::Regex> = std::sync::OnceLock::new();
    let redirections = REDIRECTIONS.get_or_init(|| {
        regex::bytes::Regex::new(r#"[0-9]*[<>]{1,2}&?\s*(?:'[^']*'|"[^"]*"|[^\s;|&()'"<>]+)"#)
            .unwrap()
    });
    let line = redirections.replace_all(line, b" ".as_slice());
    line.split(|byte| b";|&(){}".contains(byte)).any(|command| {
        let mut words = command
            .split(|byte| byte.is_ascii_whitespace() || b"'\"<>".contains(byte))
            .filter(|word| !word.is_empty());
        let command = words.find(|word| {
            !matches!(
                *word,
                b"if"
                    | b"then"
                    | b"else"
                    | b"elif"
                    | b"do"
                    | b"!"
                    | b"builtin"
                    | b"command"
                    | b"noglob"
                    | b"nocorrect"
            ) && !word.contains(&b'=')
        });
        let Some(command @ (b"set" | b"setopt" | b"unsetopt")) = command else {
            return false;
        };
        while let Some(word) = words.next() {
            let Some(options) = word.strip_prefix(b"-").or_else(|| word.strip_prefix(b"+")) else {
                if command == b"set" {
                    break;
                }
                continue;
            };
            if options.is_empty() || !options.iter().all(u8::is_ascii_alphabetic) {
                break;
            }
            if options.contains(&b'f') && (word[0] == b'-' || command == b"unsetopt") {
                return true;
            }
            if options.ends_with(b"o") {
                let _ = words.next();
            }
        }
        false
    })
}

#[cfg(test)]
mod option_tests {
    use super::keeps_kettle_out;

    #[test]
    fn continued_startup_identifiers_keep_kettle_out() {
        for code in [
            "unsetopt R\\\nCS",
            "setopt no_r\\\n_cs",
            "export ZDOT\\\nDIR=/tmp/zsh",
        ] {
            assert!(keeps_kettle_out(code.as_bytes()), "{code}");
        }
    }

    #[test]
    fn short_startup_options_belong_to_option_commands() {
        for code in [
            "set -f",
            "set -xf>/dev/null",
            "true; set -f",
            "if true; then builtin set -f; fi",
            "'builtin' 'set' '-f'",
            "FOO=bar set -f",
            "set +x -f",
            "set -o xtrace -f",
            ">/dev/null set -f",
            "set >/dev/null -f",
            "set 2>/dev/null -xf",
            "set -x > /dev/null -f",
            "set 2>&1 -f",
            "set >'/tmp/shell output' -f",
            "setopt -f",
            "setopt promptsubst -f",
            "unsetopt -f",
            "unsetopt +f",
            "set \\\n-f",
            "# comment \\\nset -f",
        ] {
            assert!(keeps_kettle_out(code.as_bytes()), "{code}");
        }
        for code in [
            "[ -f /etc/site.zsh ] && source /etc/site.zsh",
            "[[ -f /etc/site.zsh ]]",
            "grep -f /etc/patterns /etc/site.zsh",
            "set -- -f",
            "set positional -f",
            "set +f",
            "setopt +f",
        ] {
            assert!(!keeps_kettle_out(code.as_bytes()), "{code}");
        }
    }
}

/// Open `path` to read without waiting for a writer, as opening a FIFO
/// otherwise would.
#[cfg(unix)]
fn open_without_waiting(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_without_waiting(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The system `zshenv` that keeps Kettle from a zsh at `program`, if any.
pub fn zsh_left_alone(program: &Path) -> Option<PathBuf> {
    zshenv_keeping_kettle_out(&system_zshenvs(program))
}

/// Where the PTY finds `program`: the program itself when it names a path,
/// otherwise the first executable file of that name in `path`, the pane's
/// `PATH`. A name found nowhere stays as it is.
pub fn resolve_program(program: &Path, path: Option<&OsStr>) -> PathBuf {
    #[cfg(unix)]
    {
        let mut cmd = CommandBuilder::new(program);
        if let Some(path) = path {
            cmd.env("PATH", path);
        } else {
            cmd.env_remove("PATH");
        }
        cmd.resolve_program()
            .map(PathBuf::from)
            .unwrap_or_else(|_| program.to_owned())
    }
    #[cfg(not(unix))]
    {
        if program.components().count() != 1 || program.is_absolute() {
            return program.to_owned();
        }
        path.into_iter()
            .flat_map(std::env::split_paths)
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(program))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| program.to_owned())
    }
}

/// What [`ShellStartup`] adds to the command of a pane that runs `program`
/// with no arguments of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Additions {
    /// Variables to set in its environment.
    pub env: Vec<(&'static str, OsString)>,
    /// Arguments to add after its own.
    pub args: Vec<String>,
}

/// What `startup` adds for a pane running `program`, given `zdotdir`, the
/// `ZDOTDIR` it would have read (`None` when unset): nothing for a shell
/// other than zsh or fish, or for a zsh a system `zshenv` keeps Kettle out
/// of (see [`zsh_left_alone`]).
pub fn additions(program: &Path, zdotdir: Option<&OsStr>, startup: &ShellStartup) -> Additions {
    match startup_shell(program) {
        Some(StartupShell::Zsh) => {
            let Some(directory) = &startup.zsh else {
                return Additions::default();
            };
            if zsh_left_alone(program).is_some() {
                return Additions::default();
            }
            Additions {
                env: vec![
                    (ZDOTDIR_SAVED, zdotdir.unwrap_or_default().to_owned()),
                    (
                        ZDOTDIR_SAVED_SET,
                        if zdotdir.is_some() { "1" } else { "" }.into(),
                    ),
                    ("ZDOTDIR", directory.clone().into_os_string()),
                ],
                args: Vec::new(),
            }
        }
        Some(StartupShell::Fish) => Additions {
            env: Vec::new(),
            args: startup
                .fish
                .iter()
                .flat_map(|code| ["-C".to_owned(), code.clone()])
                .collect(),
        },
        None => Additions::default(),
    }
}

/// Only startup flags are eligible; commands, scripts and mode changes stay exact.
#[cfg(unix)]
#[derive(Default)]
struct BashInvocation {
    login: bool,
    noprofile: bool,
    norc: bool,
    rcfile: Option<OsString>,
}

#[cfg(unix)]
impl BashInvocation {
    fn parse(cmd: &CommandBuilder) -> Option<Self> {
        let mut invocation = Self {
            login: cmd.is_default_prog(),
            ..Self::default()
        };
        let mut args = cmd.get_argv().iter().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str()? {
                "-l" | "--login" => invocation.login = true,
                "--noprofile" => invocation.noprofile = true,
                "--norc" => invocation.norc = true,
                "--rcfile" | "--init-file" => invocation.rcfile = Some(args.next()?.clone()),
                _ => return None,
            }
        }
        Some(invocation)
    }
}

// Apple's Bash 3.2 reads normal startup files even with --posix and ignores
// ENV. Leave its launch untouched rather than lose login state or double-source.
#[cfg(unix)]
fn bash_ignores_posix_env(program: &Path) -> bool {
    cfg!(target_os = "macos")
        && matches!(
            std::fs::canonicalize(program)
                .unwrap_or_else(|_| program.to_owned())
                .to_str(),
            Some("/bin/bash" | "/usr/bin/bash")
        )
}

/// Add `startup` to `cmd`, the command a pane runs. `explicit` is the
/// program a `command` naming just a shell runs, and `None` when the pane
/// runs the user's shell, which `cmd` then finds as it will at spawn. A
/// command with arguments gets nothing unless they are only Bash startup flags.
#[cfg(unix)]
pub(crate) fn apply(cmd: &mut CommandBuilder, explicit: Option<&str>, startup: &ShellStartup) {
    if *startup == ShellStartup::default() {
        return;
    }
    let program = match explicit {
        Some(program) => cmd
            .resolve_program()
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(program)),
        None if cmd.is_default_prog() => PathBuf::from(cmd.get_shell()),
        None => {
            let Some(program) = cmd.get_argv().first().map(PathBuf::from) else {
                return;
            };
            if program.file_name() != Some(OsStr::new("bash"))
                || BashInvocation::parse(cmd).is_none()
            {
                return;
            }
            cmd.resolve_program().map(PathBuf::from).unwrap_or(program)
        }
    };
    let mut startup = startup.clone();
    if let Some(directory) = &startup.integration {
        match program.file_name().and_then(OsStr::to_str) {
            Some("zsh") => startup.zsh = Some(directory.join("zsh")),
            Some("bash") => {
                let Some(invocation) = BashInvocation::parse(cmd) else {
                    return;
                };
                if bash_ignores_posix_env(&program) {
                    return;
                }
                let original = cmd.get_env("ENV").map(OsStr::to_owned);
                cmd.env(
                    "KETTLE_BASH_ENV_SET",
                    if original.is_some() { "1" } else { "" },
                );
                cmd.env("KETTLE_BASH_ENV", original.unwrap_or_default());
                cmd.env(
                    "KETTLE_BASH_NOPROFILE",
                    if invocation.noprofile { "1" } else { "" },
                );
                cmd.env("KETTLE_BASH_NORC", if invocation.norc { "1" } else { "" });
                if let Some(rcfile) = invocation.rcfile {
                    cmd.env("KETTLE_BASH_RCFILE", rcfile);
                } else {
                    cmd.env_remove("KETTLE_BASH_RCFILE");
                }
                cmd.env("ENV", directory.join("bashrc"));
                let argv = cmd.get_argv_mut();
                if argv.is_empty() {
                    argv.push(program.clone().into_os_string());
                }
                // Long options must precede short options in Bash's parser.
                argv.insert(1, "--posix".into());
                if invocation.login {
                    argv.insert(2, "--login".into());
                }
            }
            Some("fish") => {
                let original = cmd.get_env("XDG_DATA_DIRS").map(OsStr::to_owned);
                cmd.env(
                    "KETTLE_XDG_DATA_DIRS_SET",
                    if original.is_some() { "1" } else { "" },
                );
                let mut value = directory.as_os_str().to_owned();
                value.push(":");
                value.push(
                    original
                        .as_deref()
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| OsStr::new("/usr/local/share:/usr/share")),
                );
                cmd.env("XDG_DATA_DIRS", value);
            }
            _ => {}
        }
    }
    let additions = additions(&program, cmd.get_env("ZDOTDIR"), &startup);

    for (name, value) in additions.env {
        cmd.env(name, value);
    }
    for arg in additions.args {
        if explicit.is_some() {
            cmd.arg(arg);
        } else {
            cmd.shell_arg(arg);
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn apply(_: &mut CommandBuilder, _: Option<&str>, _: &ShellStartup) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use kettle_test_support::{PrivateTempDir, private_tempdir};
    use std::os::unix::fs::PermissionsExt as _;

    /// A directory with an executable named `name` in it, standing in for a
    /// shell the PTY can run.
    fn shell_named(name: &str) -> (PrivateTempDir, String) {
        let dir = private_tempdir("kettle-shell-startup");
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = path.to_str().unwrap().to_owned();
        (dir, path)
    }

    fn startup() -> ShellStartup {
        ShellStartup {
            integration: None,
            zsh: Some(PathBuf::from("/kettle/zsh")),
            fish: Some("function codex; end".to_owned()),
        }
    }

    fn env(cmd: &CommandBuilder, name: &str) -> Option<String> {
        cmd.get_env(name)
            .map(|value| value.to_str().unwrap().to_owned())
    }

    fn zsh_is_left_alone(program: impl AsRef<Path>) -> bool {
        zsh_left_alone(program.as_ref()).is_some()
    }

    #[test]
    fn the_users_zsh_borrows_zdotdir_and_keeps_what_it_was() {
        let (_dir, zsh) = shell_named("zsh");
        if zsh_is_left_alone(&zsh) {
            return;
        }
        for original in [None, Some(""), Some("/home/u/.config/zsh")] {
            let mut cmd = CommandBuilder::new_default_prog();
            cmd.env("SHELL", &zsh);
            cmd.env_remove("ZDOTDIR");
            if let Some(original) = original {
                cmd.env("ZDOTDIR", original);
            }
            apply(&mut cmd, None, &startup());
            assert_eq!(env(&cmd, "ZDOTDIR").as_deref(), Some("/kettle/zsh"));
            assert_eq!(
                env(&cmd, ZDOTDIR_SAVED).as_deref(),
                Some(original.unwrap_or_default())
            );
            assert_eq!(
                env(&cmd, ZDOTDIR_SAVED_SET).as_deref(),
                Some(if original.is_some() { "1" } else { "" })
            );
            assert!(cmd.get_shell_args().is_empty());
        }
    }

    #[test]
    fn a_zdotdir_that_is_not_utf8_is_kept_exactly() {
        use std::os::unix::ffi::OsStrExt as _;
        let (_dir, zsh) = shell_named("zsh");
        if zsh_is_left_alone(&zsh) {
            return;
        }
        let original = OsStr::from_bytes(b"/home/u/\xff");
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", &zsh);
        cmd.env("ZDOTDIR", original);
        apply(&mut cmd, None, &startup());
        assert_eq!(cmd.get_env(ZDOTDIR_SAVED), Some(original));
    }

    #[test]
    fn the_users_fish_runs_the_code_after_its_configuration() {
        let (_dir, fish) = shell_named("fish");
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", &fish);
        cmd.env("XDG_DATA_DIRS", "/opt/share");
        apply(&mut cmd, None, &startup());
        assert_eq!(cmd.get_shell_args(), ["-C", "function codex; end"]);
        assert!(cmd.is_default_prog(), "it still starts as a login shell");
        assert_eq!(env(&cmd, "XDG_DATA_DIRS").as_deref(), Some("/opt/share"));
        assert_eq!(env(&cmd, "ZDOTDIR"), None);
    }

    #[test]
    fn a_command_of_just_the_shell_gets_the_startup() {
        let mut cmd = CommandBuilder::new("fish");
        apply(&mut cmd, Some("fish"), &startup());
        assert_eq!(cmd.get_argv(), &["fish", "-C", "function codex; end"]);

        if zsh_is_left_alone("/usr/local/bin/zsh") {
            return;
        }
        let mut cmd = CommandBuilder::new("/usr/local/bin/zsh");
        apply(&mut cmd, Some("/usr/local/bin/zsh"), &startup());
        assert_eq!(env(&cmd, "ZDOTDIR").as_deref(), Some("/kettle/zsh"));
    }

    /// The shell Kettle adds startup to is the one the PTY will run: a
    /// `SHELL` that cannot run is passed over for the passwd shell there, so
    /// it is here too.
    #[test]
    fn the_startup_follows_the_shell_the_pty_runs() {
        for name in ["zsh", "fish"] {
            let dir = private_tempdir("kettle-shell-startup");
            let unrunnable = dir.path().join(name);
            std::fs::write(&unrunnable, "").unwrap();
            let mut cmd = CommandBuilder::new_default_prog();
            cmd.env("SHELL", &unrunnable);
            cmd.env_remove("ZDOTDIR");
            let runs = cmd.get_shell();
            apply(&mut cmd, None, &startup());
            let shell = startup_shell(Path::new(&runs));
            let zsh = shell == Some(StartupShell::Zsh) && !zsh_is_left_alone(&runs);
            assert_eq!(env(&cmd, "ZDOTDIR").is_some(), zsh, "{name}: runs {runs}");
            assert_eq!(
                !cmd.get_shell_args().is_empty(),
                shell == Some(StartupShell::Fish),
                "{name}: runs {runs}"
            );
        }
    }

    #[test]
    fn other_shells_and_commands_with_arguments_get_nothing() {
        let (_dir, bash) = shell_named("bash");
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", &bash);
        cmd.env_remove("ZDOTDIR");
        apply(&mut cmd, None, &startup());
        assert_eq!(env(&cmd, "ZDOTDIR"), None);
        assert!(cmd.get_shell_args().is_empty());

        let mut cmd = CommandBuilder::new("fish");
        cmd.arg("-c");
        cmd.arg("true");
        apply(&mut cmd, None, &startup());
        assert_eq!(cmd.get_argv(), &["fish", "-c", "true"]);
    }

    #[test]
    fn nothing_offered_changes_nothing() {
        let (_dir, fish) = shell_named("fish");
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", &fish);
        let before = cmd.clone();
        apply(&mut cmd, None, &ShellStartup::default());
        assert_eq!(cmd, before);
    }

    /// A system `zshenv` that names `ZDOTDIR` or the `RCS` option, in any
    /// spelling zsh accepts, or is too large to read, keeps Kettle out; a
    /// missing file, a directory or a FIFO does not, and a FIFO with no
    /// writer is passed over without waiting.
    #[test]
    fn a_system_zshenv_naming_zdotdir_or_rcs_keeps_kettle_out() {
        let dir = private_tempdir("kettle-shell-startup");
        let file = |name: &str, text: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let plain = file(
            "plain",
            b"export PATH=/usr/bin\nsource /etc/profile.d/sources\n",
        );
        let naming = file("naming", b"ZDOTDIR=${ZDOTDIR:-$HOME/.config/zsh}\n");
        let large = file("large", &vec![b'#'; MAX_ZSHENV_BYTES as usize + 1]);
        let at_cap = file("at-cap", &vec![b'#'; MAX_ZSHENV_BYTES as usize]);
        let missing = dir.path().join("missing");
        let directory = dir.path().to_path_buf();
        let fifo = dir.path().join("fifo");
        let fifo_name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path the call only reads.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

        // A blocking open would wait for a writer forever; the scan has to
        // finish on its own.
        let (done, finished) = std::sync::mpsc::channel();
        let candidates = [plain.clone(), missing.clone(), fifo.clone()];
        std::thread::spawn(move || done.send(zshenv_keeping_kettle_out(&candidates)));
        assert_eq!(
            finished.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(None),
            "the FIFO was waited on"
        );
        assert_eq!(zshenv_keeping_kettle_out(&[at_cap, directory]), None);
        assert_eq!(
            zshenv_keeping_kettle_out(&[plain.clone(), naming.clone()]),
            Some(naming)
        );
        assert_eq!(
            zshenv_keeping_kettle_out(&[missing, large.clone()]),
            Some(large)
        );
        for spelling in [
            "unsetopt RCS",
            "setopt no_rcs",
            "setopt NORCS",
            "unsetopt r_c_s",
            "  setopt norcs # a trailing comment",
            "[[ -o interactive ]] || ZDOTDIR=/opt/zsh",
        ] {
            let rcs = file("rcs", format!("# header\n{spelling}\n").as_bytes());
            assert_eq!(
                zshenv_keeping_kettle_out(std::slice::from_ref(&rcs)),
                Some(rcs),
                "{spelling}"
            );
        }
    }

    #[test]
    fn file_tests_and_unrelated_flags_preserve_zsh_startup_additions() {
        let dir = private_tempdir("kettle-shell-startup");
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        let program = dir.path().join("bin/zsh");
        let system = dir.path().join("etc/zshenv");
        let startup = ShellStartup {
            integration: None,
            zsh: Some(dir.path().join("kettle")),
            fish: None,
        };
        for code in [
            "if [ -f /etc/site.zsh ]; then . /etc/site.zsh; fi",
            "[ -f /etc/zshenv.local ] && . /etc/zshenv.local",
            "if [[ -f /etc/profile ]]; then source /etc/profile; fi",
            "test -f /etc/site.zsh && source /etc/site.zsh",
            "grep -f /etc/patterns /etc/site.zsh",
            "printf '%s' -f",
            "set -- -f",
            "set positional -f",
        ] {
            std::fs::write(&system, code).unwrap();
            assert_eq!(
                zshenv_keeping_kettle_out(std::slice::from_ref(&system)),
                None,
                "{code}"
            );
            assert!(
                additions(&program, Some(OsStr::new("/original")), &startup)
                    .env
                    .contains(&("ZDOTDIR", dir.path().join("kettle").into_os_string())),
                "{code}"
            );
        }
    }

    #[test]
    fn short_no_rcs_option_next_to_redirections_withholds_zsh_startup_additions() {
        let dir = private_tempdir("kettle-shell-startup");
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        let program = dir.path().join("bin/zsh");
        let system = dir.path().join("etc/zshenv");
        let startup = ShellStartup {
            integration: None,
            zsh: Some(dir.path().join("kettle")),
            fish: None,
        };
        for command in [
            "set -f>/dev/null",
            "set -f</dev/null",
            "set -xf>>/dev/null",
            "set -f<>/dev/null",
        ] {
            std::fs::write(&system, format!("{command}\n")).unwrap();
            assert_eq!(
                additions(&program, Some(OsStr::new("/original")), &startup),
                Additions::default(),
                "{command} must not borrow ZDOTDIR"
            );
        }
    }

    #[test]
    fn short_no_rcs_option_withholds_zsh_startup_additions() {
        let dir = private_tempdir("kettle-shell-startup");
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        let program = dir.path().join("bin/zsh");
        let system = dir.path().join("etc/zshenv");
        std::fs::write(&system, "set -f\n").unwrap();
        assert_eq!(
            zshenv_keeping_kettle_out(std::slice::from_ref(&system)),
            Some(system)
        );
        let startup = ShellStartup {
            integration: None,
            zsh: Some(dir.path().join("kettle")),
            fish: None,
        };
        assert_eq!(
            additions(&program, Some(OsStr::new("/original")), &startup),
            Additions::default()
        );
    }

    /// A whole-line comment changes nothing zsh does, so naming `NO_RCS` or
    /// `ZDOTDIR` there does not keep Kettle out. Debian, Ubuntu and Fedora
    /// ship a `zshenv` whose header comment explains `NO_RCS`; this is
    /// Ubuntu 26.04's, byte for byte.
    #[test]
    fn a_comment_naming_rcs_or_zdotdir_does_not_keep_kettle_out() {
        let dir = private_tempdir("kettle-shell-startup");
        let stock = dir.path().join("zshenv");
        std::fs::write(
            &stock,
            "# /etc/zsh/zshenv: system-wide .zshenv file for zsh(1).\n\
             #\n\
             # This file is sourced on all invocations of the shell.\n\
             # If the -f flag is present or if the NO_RCS option is\n\
             # set within this file, all other initialization files\n\
             # are skipped.\n\
             #\n\
             # This file should contain commands to set the command\n\
             # search path, plus other important environment variables.\n\
             # This file should not contain commands that produce\n\
             # output or assume the shell is attached to a tty.\n\
             #\n\
             # Global Order: zshenv, zprofile, zshrc, zlogin\n\
             \n\
             if [[ -z \"$PATH\" || \"$PATH\" == \"/bin:/usr/bin\" ]]\n\
             then\n\
             \texport PATH=\"/usr/local/bin:/usr/bin:/bin:/usr/games\"\n\
             fi\n",
        )
        .unwrap();
        let commented = dir.path().join("commented");
        std::fs::write(
            &commented,
            "\t# ZDOTDIR is set per user.\nexport EDITOR=vi\n",
        )
        .unwrap();
        assert_eq!(zshenv_keeping_kettle_out(&[stock, commented]), None);
    }

    #[test]
    fn relative_and_empty_path_entries_follow_the_child_directory() {
        if zsh_is_left_alone("zsh") {
            return;
        }
        let prefix = private_tempdir("kettle-shell-startup");
        std::fs::create_dir_all(prefix.join("tools/bin")).unwrap();
        std::fs::create_dir_all(prefix.join("tools/etc")).unwrap();
        let zsh = prefix.join("tools/bin/zsh");
        std::fs::write(&zsh, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&zsh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            prefix.join("tools/etc/zshenv"),
            "ZDOTDIR=${ZDOTDIR:-$HOME/.config/zsh}\n",
        )
        .unwrap();
        for (cwd, path_entry, program) in [
            (prefix.path().to_path_buf(), "tools/bin", "zsh"),
            (prefix.join("tools/bin"), "", "zsh"),
            (
                prefix.path().to_path_buf(),
                "/kettle-missing",
                "./tools/bin/zsh",
            ),
        ] {
            let mut cmd = CommandBuilder::new(program);
            cmd.cwd(cwd);
            cmd.env("PATH", std::env::join_paths([path_entry, "/bin"]).unwrap());
            cmd.env_remove("ZDOTDIR");
            apply(&mut cmd, Some(program), &startup());
            assert_eq!(env(&cmd, "ZDOTDIR"), None, "{program}: {path_entry}");
            assert_eq!(env(&cmd, ZDOTDIR_SAVED), None);
        }
    }

    /// A bare `zsh` command is found on the pane's `PATH`, as the PTY finds
    /// it, so the `zshenv` of the prefix it is installed under counts.
    #[test]
    fn a_bare_zsh_command_is_found_on_the_panes_path() {
        let prefix = private_tempdir("kettle-shell-startup");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("etc")).unwrap();
        let zsh = prefix.join("bin/zsh");
        std::fs::write(&zsh, "#!/bin/sh\n").unwrap();
        let path =
            std::env::join_paths([Path::new("/kettle-missing"), &prefix.join("bin")]).unwrap();
        assert_eq!(
            resolve_program(Path::new("zsh"), Some(&path)),
            Path::new("zsh"),
            "not executable yet"
        );
        std::fs::set_permissions(&zsh, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_program(Path::new("zsh"), Some(&path)), zsh);
        assert_eq!(resolve_program(Path::new("zsh"), None), Path::new("zsh"));
        assert_eq!(
            resolve_program(Path::new("./zsh"), Some(&path)),
            Path::new("./zsh")
        );
        if zsh_is_left_alone(&zsh) {
            return;
        }
        let mut cmd = CommandBuilder::new("zsh");
        cmd.env("PATH", &path);
        cmd.env_remove("ZDOTDIR");
        apply(&mut cmd, Some("zsh"), &startup());
        assert!(env(&cmd, "ZDOTDIR").is_some());
        std::fs::write(
            prefix.join("etc/zshenv"),
            "ZDOTDIR=${ZDOTDIR:-$HOME/.zsh}\n",
        )
        .unwrap();
        let mut cmd = CommandBuilder::new("zsh");
        cmd.env("PATH", &path);
        cmd.env_remove("ZDOTDIR");
        apply(&mut cmd, Some("zsh"), &startup());
        assert_eq!(env(&cmd, "ZDOTDIR"), None);
    }

    /// A zsh whose own `zshenv` names `ZDOTDIR` gets nothing, while fish
    /// beside it still does.
    #[test]
    fn a_zsh_whose_zshenv_names_zdotdir_gets_nothing() {
        let prefix = private_tempdir("kettle-shell-startup");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("etc")).unwrap();
        let zsh = prefix.join("bin/zsh");
        if zsh_is_left_alone(&zsh) {
            return;
        }
        assert!(!additions(&zsh, None, &startup()).env.is_empty());
        std::fs::write(
            prefix.join("etc/zshenv"),
            "export ZDOTDIR=\"${ZDOTDIR:-$HOME/.config/zsh}\"\n",
        )
        .unwrap();
        assert_eq!(additions(&zsh, None, &startup()), Additions::default());
        let fish = prefix.join("bin/fish");
        assert!(!additions(&fish, None, &startup()).args.is_empty());
    }

    #[test]
    fn a_prefixed_zsh_reads_its_prefixs_zshenv_too() {
        assert_eq!(
            system_zshenvs(Path::new("/opt/homebrew/bin/zsh")),
            [
                PathBuf::from("/etc/zshenv"),
                PathBuf::from("/etc/zsh/zshenv"),
                PathBuf::from("/opt/homebrew/etc/zshenv"),
            ]
        );
        assert_eq!(system_zshenvs(Path::new("/bin/zsh")).len(), 2);
        assert_eq!(system_zshenvs(Path::new("zsh")).len(), 2);
    }
}

#[cfg(all(test, unix))]
mod automatic_tests {
    use super::*;

    #[test]
    fn automatic_bash_uses_env_and_keeps_login_flags() {
        let startup = ShellStartup {
            integration: Some("/private/startup".into()),
            ..Default::default()
        };
        for flags in [
            vec![],
            vec!["-l"],
            vec!["--login"],
            vec!["--noprofile", "-l"],
            vec!["--norc"],
            vec!["--rcfile", "/user/custom rc"],
            vec!["--init-file", "/user/other"],
        ] {
            let mut cmd = CommandBuilder::new("/custom/bash");
            cmd.args(&flags);
            cmd.env_remove("ENV");
            apply(
                &mut cmd,
                if flags.is_empty() {
                    Some("/custom/bash")
                } else {
                    None
                },
                &startup,
            );
            assert_eq!(cmd.get_argv()[1], OsStr::new("--posix"));
            let login = flags.contains(&"-l") || flags.contains(&"--login");
            assert_eq!(cmd.get_argv().iter().any(|arg| arg == "--login"), login);
            assert_eq!(
                cmd.get_env("ENV"),
                Some(OsStr::new("/private/startup/bashrc"))
            );
            assert_eq!(cmd.get_env("KETTLE_BASH_ENV_SET"), Some(OsStr::new("")));
            assert_eq!(
                cmd.get_env("KETTLE_BASH_NOPROFILE"),
                Some(OsStr::new(if flags.contains(&"--noprofile") {
                    "1"
                } else {
                    ""
                }))
            );
            assert_eq!(
                cmd.get_env("KETTLE_BASH_NORC"),
                Some(OsStr::new(if flags.contains(&"--norc") { "1" } else { "" }))
            );
            let expected_rc = if flags.contains(&"--rcfile") {
                Some(OsStr::new("/user/custom rc"))
            } else if flags.contains(&"--init-file") {
                Some(OsStr::new("/user/other"))
            } else {
                None
            };
            assert_eq!(cmd.get_env("KETTLE_BASH_RCFILE"), expected_rc);
        }
    }

    #[test]
    fn automatic_bash_preserves_real_login_launch_and_env() {
        let startup = ShellStartup {
            integration: Some("/private/startup".into()),
            ..Default::default()
        };
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", "/bin/bash");
        cmd.env("ENV", "/user/env");
        apply(&mut cmd, None, &startup);
        if cfg!(target_os = "macos") {
            assert!(
                cmd.is_default_prog(),
                "system Bash 3.2 must be left untouched"
            );
            assert_eq!(cmd.get_env("ENV"), Some(OsStr::new("/user/env")));
        } else {
            assert!(cmd.get_argv().iter().any(|arg| arg == "--login"));
            assert!(cmd.get_argv().iter().any(|arg| arg == "--posix"));
            assert_eq!(
                cmd.get_env("ENV"),
                Some(OsStr::new("/private/startup/bashrc"))
            );
            assert_eq!(
                cmd.get_env("KETTLE_BASH_ENV"),
                Some(OsStr::new("/user/env"))
            );
        }
    }

    #[test]
    fn automatic_bash_modern_default_launch_stays_login() {
        let bash = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/bash"
        } else {
            "/bin/bash"
        };
        if !Path::new(bash).is_file() {
            eprintln!("modern Bash not installed; default launch skipped");
            return;
        }
        let startup = ShellStartup {
            integration: Some("/private/startup".into()),
            ..Default::default()
        };
        for original in [None, Some(""), Some("/user/original")] {
            let mut cmd = CommandBuilder::new_default_prog();
            cmd.env("SHELL", bash);
            cmd.env_remove("ENV");
            if let Some(value) = original {
                cmd.env("ENV", value);
            }
            apply(&mut cmd, None, &startup);
            assert_eq!(
                cmd.get_argv(),
                &[bash, "--posix", "--login"].map(OsString::from)
            );
            assert_eq!(
                cmd.get_env("KETTLE_BASH_ENV_SET"),
                Some(OsStr::new(if original.is_some() { "1" } else { "" }))
            );
            assert_eq!(
                cmd.get_env("KETTLE_BASH_ENV"),
                Some(OsStr::new(original.unwrap_or("")))
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_bash_32_fallback_retains_native_login_and_logout() {
        use std::os::unix::process::CommandExt as _;
        let temp = kettle_test_support::private_tempdir("kettle-bash32-login-");
        for (file, marker) in [(".bash_profile", "profile"), (".bash_logout", "logout")] {
            std::fs::write(
                temp.join(file),
                format!("echo {marker} >> \"$HOME/trace\"\n"),
            )
            .unwrap();
        }
        let startup = ShellStartup {
            integration: Some("/private/startup".into()),
            ..Default::default()
        };
        for default in [true, false] {
            let mut cmd = if default {
                CommandBuilder::new_default_prog()
            } else {
                CommandBuilder::new("/bin/bash")
            };
            if !default {
                cmd.arg("-l");
            }
            cmd.env_clear();
            cmd.env("SHELL", "/bin/bash");
            cmd.env("HOME", temp.path());
            cmd.env("PATH", "/usr/bin:/bin");
            cmd.env("ENV", "/user/original");
            let before = cmd.clone();
            apply(
                &mut cmd,
                if default { None } else { Some("/bin/bash") },
                &startup,
            );
            assert_eq!(
                cmd, before,
                "fallback must not borrow environment or alter arguments"
            );
            let mut native = std::process::Command::new("/bin/bash");
            if default {
                native.arg0("-bash");
            } else {
                native.args(&cmd.get_argv()[1..]);
            }
            let output = native.env_clear().envs(cmd.iter_full_env_as_str())
                .args(["-ic", "shopt -q login_shell || exit 21; [[ ! -o posix && $ENV == /user/original ]] || exit 22; declare -F __kettle_pc >/dev/null && exit 23; exit 0"])
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(temp.join("trace")).unwrap(),
                "profile\nlogout\n"
            );
            std::fs::remove_file(temp.join("trace")).unwrap();
        }
    }

    #[test]
    fn automatic_fish_borrows_xdg_but_keeps_agent_after_config_code() {
        let startup = ShellStartup {
            integration: Some("/private/startup".into()),
            fish: Some("agent".into()),
            ..Default::default()
        };
        for original in [None, Some(""), Some("/custom/share:/usr/share")] {
            let mut cmd = CommandBuilder::new("/usr/bin/fish");
            cmd.env_remove("XDG_DATA_DIRS");
            if let Some(value) = original {
                cmd.env("XDG_DATA_DIRS", value);
            }
            apply(&mut cmd, Some("/usr/bin/fish"), &startup);
            assert_eq!(
                cmd.get_env("KETTLE_XDG_DATA_DIRS_SET"),
                Some(OsStr::new(if original.is_some() { "1" } else { "" }))
            );
            assert_eq!(cmd.get_env("KETTLE_XDG_DATA_DIRS"), None);
            assert!(
                cmd.get_env("XDG_DATA_DIRS")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("/private/startup:")
            );
            assert_eq!(
                cmd.get_argv(),
                &["/usr/bin/fish", "-C", "agent"].map(OsString::from)
            );
        }
    }

    #[test]
    fn explicit_commands_with_arguments_and_other_programs_are_unchanged() {
        let startup = ShellStartup {
            integration: Some("/private/startup".into()),
            ..Default::default()
        };
        for argv in [
            vec!["/bin/bash", "-c", "echo hello"],
            vec!["/bin/bash", "--login", "-c", "echo hello"],
            vec!["/bin/bash", "script.sh"],
            vec!["/bin/bash", "--posix"],
            vec!["/bin/bash", "--rcfile"],
            vec!["/usr/bin/ssh", "host"],
            vec!["/usr/bin/sudo", "bash"],
            vec!["/bin/sh"],
        ] {
            let mut cmd = CommandBuilder::from_argv(argv.iter().map(OsString::from).collect());
            let before = cmd.clone();
            apply(
                &mut cmd,
                if argv.len() == 1 { Some(argv[0]) } else { None },
                &startup,
            );
            assert_eq!(cmd.get_argv(), before.get_argv());
            assert_eq!(cmd.get_env("ZDOTDIR"), before.get_env("ZDOTDIR"));
            assert_eq!(
                cmd.get_env("XDG_DATA_DIRS"),
                before.get_env("XDG_DATA_DIRS")
            );
        }
    }
}
