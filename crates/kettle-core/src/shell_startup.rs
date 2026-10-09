//! What Kettle adds to the startup of the zsh or fish a pane runs, leaving
//! the user's own startup files as they are. zsh reads a `.zshenv` of
//! Kettle's in place of the user's, through a `ZDOTDIR` Kettle borrows and
//! that file puts back; fish runs Kettle's code after the user's
//! configuration, as `fish -C`, with nothing borrowed.
//!
//! Only the shell a pane starts with no arguments of its own gets them: the
//! user's shell, or a `command` naming just the shell. The shell is the one
//! the PTY runs, decided by the same lookup.

use portable_pty::CommandBuilder;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Startup additions for a pane's shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellStartup {
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
/// names, so the `RCS` check does too; a mention in a comment counts, which
/// only costs that zsh Kettle's `codex`. A file too large to read here counts,
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

/// Whether a system `zshenv`'s text names `ZDOTDIR` or the `RCS` option.
fn keeps_kettle_out(text: &[u8]) -> bool {
    if text
        .windows(b"ZDOTDIR".len())
        .any(|window| window == b"ZDOTDIR")
    {
        return true;
    }
    let folded: Vec<u8> = text
        .iter()
        .filter(|byte| **byte != b'_')
        .map(u8::to_ascii_lowercase)
        .collect();
    folded.windows(b"rcs".len()).any(|window| window == b"rcs")
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
    if program.components().count() != 1 || program.is_absolute() {
        return program.to_owned();
    }
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable_file(candidate))
        .unwrap_or_else(|| program.to_owned())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
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

/// Add `startup` to `cmd`, the command a pane runs. `explicit` is the
/// program a `command` naming just a shell runs, and `None` when the pane
/// runs the user's shell, which `cmd` then finds as it will at spawn. A
/// command with arguments of its own gets nothing.
#[cfg(unix)]
pub(crate) fn apply(cmd: &mut CommandBuilder, explicit: Option<&str>, startup: &ShellStartup) {
    if *startup == ShellStartup::default() {
        return;
    }
    let program = match explicit {
        Some(program) => resolve_program(Path::new(program), cmd.get_env("PATH")),
        None if cmd.is_default_prog() => PathBuf::from(cmd.get_shell()),
        None => return,
    };
    let additions = additions(&program, cmd.get_env("ZDOTDIR"), startup);
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
            "# zsh -o norcs",
        ] {
            let rcs = file("rcs", format!("{spelling}\n").as_bytes());
            assert_eq!(
                zshenv_keeping_kettle_out(std::slice::from_ref(&rcs)),
                Some(rcs),
                "{spelling}"
            );
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
