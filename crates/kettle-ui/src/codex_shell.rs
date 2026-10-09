//! Kettle's Codex shell integration: the `codex` function that starts Codex
//! through Kettle, as `kettle agent-setup --print` prints it for a user to
//! add to their shell, and as Kettle defines it in the shell a new pane
//! starts when `agent-display-codex` is on. Panes get the startup files on
//! Unix only, where Kettle can check them.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::Path;

/// A shell the function can be written for. PowerShell is not one: its
/// parameter binder drops a bare `--` before a function sees its arguments,
/// so no function there could hand them on exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexShell {
    Bash,
    Zsh,
    Fish,
}

/// Why Kettle's path cannot go into the function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionError {
    /// Kettle's path is relative or not UTF-8.
    UnnamedKettle,
    /// Kettle runs from a translocated copy, whose path does not last.
    Translocated,
}

/// Kettle's path as a launch can name it: absolute, UTF-8 and lasting.
pub fn kettle_path(kettle: &Path) -> Result<&str, FunctionError> {
    let path = kettle
        .to_str()
        .filter(|_| kettle.is_absolute())
        .ok_or(FunctionError::UnnamedKettle)?;
    if path.contains("/AppTranslocation/") {
        return Err(FunctionError::Translocated);
    }
    Ok(path)
}

/// The `codex` function for `shell`, which hands every argument to the
/// Kettle at `kettle` to start Codex with: no `eval`, and the path quoted so
/// nothing in it is read as code.
pub fn codex_function(kettle: &Path, shell: CodexShell) -> Result<String, FunctionError> {
    let path = kettle_path(kettle)?;
    Ok(match shell {
        CodexShell::Bash | CodexShell::Zsh => {
            let path = path.replace('\'', "'\\''");
            format!("codex() {{\n  command '{path}' agent-setup --launch-codex -- \"$@\"\n}}\n")
        }
        CodexShell::Fish => {
            let path = path.replace('\\', "\\\\").replace('\'', "\\'");
            format!("function codex\n  command '{path}' agent-setup --launch-codex -- $argv\nend\n")
        }
    })
}

/// The startup files, by their path inside Kettle's directory for them.
const MANIFEST_FILE: &str = "kettle.json";
const ZSH_DIRECTORY: &str = "zsh";
const ZSH_FILE: &str = "zsh/.zshenv";

/// The startup files that define `codex` in a pane's zsh for the Kettle at
/// `executable`, at `version`. fish needs none: see [`fish_code`].
pub(crate) fn startup_files(
    executable: &Path,
    version: &str,
) -> Result<crate::owned_dir::OwnedFiles, FunctionError> {
    let command = kettle_path(executable)?.to_owned();
    let zsh = codex_function(executable, CodexShell::Zsh)?;
    let manifest = serde_json::json!({"command": command, "version": version});
    Ok(crate::owned_dir::OwnedFiles::new(
        "codex-shell",
        command,
        version.to_owned(),
        vec![
            (MANIFEST_FILE, format!("{manifest}\n")),
            (
                ZSH_FILE,
                kettle_core::shell_startup::zshenv(&zsh_definition(&zsh)),
            ),
        ],
    ))
}

/// What Kettle's `.zshenv` adds: in an interactive shell, a hook that runs
/// once before the first prompt, after all the user's startup files, and
/// defines `codex` unless the user has one, defined or autoloadable. A
/// `.zshrc` that replaces `precmd_functions` outright drops the hook, and
/// the shell gets no `codex` from Kettle.
///
/// The function goes in as it is, never re-indented: a newline inside the
/// quoted path is part of the path.
fn zsh_definition(function: &str) -> String {
    format!(
        "# Kettle's Codex integration (agent-display-codex): before your first\n\
         # prompt, after all your startup files, define codex unless you have one.\n\
         if [[ -o interactive ]]; then\n\
         \x20 _kettle_codex() {{\n\
         \x20   'builtin' 'emulate' -L zsh\n\
         \x20   precmd_functions=(${{precmd_functions:#_kettle_codex}})\n\
         \x20   'builtin' 'unfunction' '_kettle_codex'\n\
         \x20   if (( ! ${{+functions[codex]}} )); then\n\
         {function}\
         \x20   fi\n\
         \x20 }}\n\
         \x20 'builtin' 'typeset' -ga precmd_functions\n\
         \x20 precmd_functions+=(_kettle_codex)\n\
         fi\n"
    )
}

/// The fish code a pane's fish runs after the user's configuration (`fish
/// -C`), for the Kettle at `executable`: in an interactive shell, it defines
/// `codex` unless the user has one, defined or autoloadable.
///
/// The function goes in as it is, never re-indented: a newline inside the
/// quoted path is part of the path.
pub(crate) fn fish_code(executable: &Path) -> Result<String, FunctionError> {
    let function = codex_function(executable, CodexShell::Fish)?;
    Ok(format!(
        "if status is-interactive; and not functions -q codex\n{function}end\n"
    ))
}

/// Where Kettle keeps the startup files: beside its agent plugins.
pub(crate) fn startup_root() -> Option<std::path::PathBuf> {
    Some(
        crate::agent_plugin::plugins_root()?
            .parent()?
            .join("agent-shell"),
    )
}

/// The executable and version a startup-file directory names, when its
/// manifest reads as Kettle's.
#[cfg(unix)]
fn identity(directory: &Path) -> Option<(String, Option<String>)> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&crate::owned_dir::read_small(directory, MANIFEST_FILE)?).ok()?;
    let command = manifest["command"].as_str()?.to_owned();
    let version = manifest["version"].as_str().map(str::to_owned);
    Some((command, version))
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<(String, Option<String>)> {
    None
}

/// Write the startup files under `root` unless they are there intact, and
/// remove stale ones; see [`crate::owned_dir::install`].
pub(crate) fn install(
    root: &Path,
    files: &crate::owned_dir::OwnedFiles,
) -> Result<std::path::PathBuf, ()> {
    crate::owned_dir::install(root, files, identity)
}

/// The startup for the Kettle at `executable`, at `version`, with its files
/// written under `root` unless they are there intact.
pub(crate) fn prepare(
    root: &Path,
    executable: &Path,
    version: &str,
) -> Result<Offered, ShellRefusal> {
    let files = startup_files(executable, version)?;
    let fish = fish_code(executable)?;
    let directory = install(root, &files).map_err(|()| ShellRefusal::Unavailable)?;
    Ok(Offered {
        directory,
        files,
        fish,
    })
}

/// Why new panes get no startup while the integration is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellRefusal {
    /// Kettle runs from a translocated copy, whose path does not last.
    Translocated,
    /// The startup files could not be written, or failed their check.
    Unavailable,
    /// This system has no way to check them.
    Unsupported,
}

impl From<FunctionError> for ShellRefusal {
    fn from(error: FunctionError) -> Self {
        match error {
            FunctionError::Translocated => Self::Translocated,
            FunctionError::UnnamedKettle => Self::Unavailable,
        }
    }
}

/// What new panes are offered: the startup files, installed in
/// `directory`, and the fish code.
#[derive(Clone, Debug)]
pub(crate) struct Offered {
    pub(crate) directory: std::path::PathBuf,
    pub(crate) files: crate::owned_dir::OwnedFiles,
    pub(crate) fish: String,
}

static OFFERED: std::sync::RwLock<Option<Offered>> = std::sync::RwLock::new(None);
static REFUSAL: std::sync::RwLock<Option<ShellRefusal>> = std::sync::RwLock::new(None);

/// Offer new panes the startup, or none; `refusal` says why there is none
/// while the integration is on.
pub(crate) fn offer(offered: Option<Offered>, refusal: Option<ShellRefusal>) {
    if let Ok(mut slot) = OFFERED.write() {
        *slot = offered;
    }
    if let Ok(mut why) = REFUSAL.write() {
        *why = refusal;
    }
}

/// Why new panes get no startup, for Settings.
pub(crate) fn refusal() -> Option<ShellRefusal> {
    REFUSAL.read().ok().and_then(|why| *why)
}

/// The offered startup, with its files checked again now: `None` when
/// nothing is offered or the files fail the check, which Settings then
/// names.
pub(crate) fn for_new_pane() -> Option<kettle_core::shell_startup::ShellStartup> {
    let offered = OFFERED.read().ok()?.clone()?;
    let checked = crate::owned_dir::verify(&offered.directory, &offered.files);
    if let Ok(mut why) = REFUSAL.write() {
        *why = checked.err().map(|()| ShellRefusal::Unavailable);
    }
    checked.ok()?;
    Some(kettle_core::shell_startup::ShellStartup {
        zsh: Some(offered.directory.join(ZSH_DIRECTORY)),
        fish: Some(offered.fish),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The startup equals the fixtures the shell-integration check runs under
    /// every supported zsh and fish, for a stand-in Kettle at
    /// `/kettle-stand-in/bin/kettle`. After changing it, write the fixtures
    /// again from `startup_files` and `fish_code`. Unix only, where the
    /// stand-in path is absolute and panes get the startup.
    #[cfg(unix)]
    #[test]
    fn the_startup_matches_the_checked_fixtures() {
        let kettle = Path::new("/kettle-stand-in/bin/kettle");
        let files = startup_files(kettle, "0").unwrap();
        let zsh = files
            .files
            .iter()
            .find(|(path, _)| *path == ZSH_FILE)
            .map(|(_, contents)| contents.as_str());
        assert_eq!(
            zsh,
            Some(include_str!(
                "../../../scripts/fixtures/shell-integration/codex-zshenv"
            ))
        );
        assert_eq!(
            fish_code(kettle).unwrap(),
            include_str!("../../../scripts/fixtures/shell-integration/codex-init.fish")
        );
    }

    /// The startup's files are installed, and what new panes are offered
    /// names them and carries the fish code.
    #[cfg(unix)]
    #[test]
    fn prepare_installs_the_files_new_panes_are_offered() {
        let base = kettle_test_support::private_tempdir("kettle-codex-shell-");
        let kettle = Path::new("/kettle-stand-in/bin/kettle");
        let root = base.path().join("agent-shell");
        let offered = prepare(&root, kettle, "5.0.0").unwrap();
        assert!(offered.directory.starts_with(&root));
        assert!(offered.directory.join(ZSH_FILE).is_file());
        assert_eq!(offered.fish, fish_code(kettle).unwrap());
        assert_eq!(
            crate::owned_dir::verify(&offered.directory, &offered.files),
            Ok(())
        );
        assert_eq!(
            prepare(&root, Path::new("relative/kettle"), "5.0.0").err(),
            Some(ShellRefusal::Unavailable)
        );
    }

    /// A newline in Kettle's path is part of the quoted path in both shells,
    /// so the definitions carry the function exactly as it is.
    #[test]
    fn the_definitions_carry_the_function_unchanged() {
        let kettle = Path::new("/opt/Kettle\nApp/kettle");
        let zsh = codex_function(kettle, CodexShell::Zsh).unwrap();
        assert!(zsh_definition(&zsh).contains(&zsh));
        let fish = codex_function(kettle, CodexShell::Fish).unwrap();
        assert!(fish_code(kettle).unwrap().contains(&fish));
    }

    /// Startup files installed for a stand-in Kettle, with a scratch home.
    #[cfg(unix)]
    struct Scratch {
        base: kettle_test_support::PrivateTempDir,
        startup: kettle_core::shell_startup::ShellStartup,
        home: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl Scratch {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            let base = kettle_test_support::private_tempdir("kettle-codex-shell-");
            let kettle = base.path().join("bin/kettle");
            std::fs::create_dir_all(kettle.parent().unwrap()).unwrap();
            std::fs::write(&kettle, "#!/bin/sh\nprintf '<%s>' \"$@\"\n").unwrap();
            std::fs::set_permissions(&kettle, std::fs::Permissions::from_mode(0o755)).unwrap();
            let offered =
                prepare(&base.path().join("kettle/agent-shell"), &kettle, "5.0.0").unwrap();
            let home = base.path().join("home");
            std::fs::create_dir_all(home.join(".config/fish/functions")).unwrap();
            Self {
                base,
                startup: kettle_core::shell_startup::ShellStartup {
                    zsh: Some(offered.directory.join(ZSH_DIRECTORY)),
                    fish: Some(offered.fish),
                },
                home,
            }
        }

        fn write(&self, path: &str, contents: &str) {
            let path = self.home.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn trace(&self) -> String {
            std::fs::read_to_string(self.home.join("trace")).unwrap_or_default()
        }

        /// What a pane running `program` gets, with `zdotdir` the `ZDOTDIR`
        /// it would have read.
        fn additions(
            &self,
            program: &str,
            zdotdir: Option<&str>,
        ) -> kettle_core::shell_startup::Additions {
            kettle_core::shell_startup::additions(
                Path::new(program),
                zdotdir.map(std::ffi::OsStr::new),
                &self.startup,
            )
        }

        /// Run `program` as a pane would, with `zdotdir` as its `ZDOTDIR`,
        /// then `arguments`, with `input` on its standard input, and return
        /// its output.
        fn run(
            &self,
            program: &str,
            zdotdir: Option<&str>,
            arguments: &[&str],
            input: &str,
        ) -> String {
            use std::io::Write as _;
            let _ = std::fs::remove_file(self.home.join("trace"));
            let additions = self.additions(program, zdotdir);
            let mut command = std::process::Command::new(program);
            command
                .env_clear()
                .env("HOME", &self.home)
                .env("TERM", "dumb")
                .env("PATH", "/usr/bin:/bin")
                .args(arguments)
                .args(&additions.args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(zdotdir) = zdotdir {
                command.env("ZDOTDIR", zdotdir);
            }
            command.envs(additions.env);
            let mut child = command.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            String::from_utf8_lossy(&output.stdout).into_owned()
                + &String::from_utf8_lossy(&output.stderr)
        }
    }

    #[cfg(unix)]
    fn installed(program: &str) -> Option<String> {
        ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
            .iter()
            .map(|directory| format!("{directory}/{program}"))
            .find(|path| Path::new(path).is_file())
    }

    /// A pane's zsh runs the user's startup files once each, in zsh's own
    /// order, from the `ZDOTDIR` they had, which it gets back exactly; it
    /// defines `codex` only when interactive, before the first prompt,
    /// handing every argument to Kettle; and a `codex` the user defines or
    /// can autoload wins.
    #[cfg(unix)]
    #[test]
    fn a_pane_zsh_defines_codex_and_keeps_its_own_startup() {
        let Some(zsh) = installed("zsh") else {
            eprintln!("zsh is not installed; skipped");
            return;
        };
        let scratch = Scratch::new();
        if scratch.additions(&zsh, None).env.is_empty() {
            eprintln!("this system's zshenv keeps Kettle out of zsh; skipped");
            return;
        }
        for (file, line) in [
            (".zshenv", "zshenv"),
            (".zprofile", "zprofile"),
            (".zshrc", "zshrc"),
            (".zlogin", "zlogin"),
        ] {
            scratch.write(file, &format!("print -r -- {line} >> $HOME/trace\n"));
        }
        let probe = "print -r -- \"ZDOTDIR=${ZDOTDIR-unset} saved=${KETTLE_ZDOTDIR_SET-gone}\"\n\
                     whence -w codex\ncodex 'a b' '*' ''\nexit\n";
        let out = scratch.run(&zsh, None, &["-i"], probe);
        assert!(out.contains("ZDOTDIR=unset saved=gone"), "{out}");
        assert!(out.contains("codex: function"), "{out}");
        assert!(
            out.contains("<agent-setup><--launch-codex><--><a b><*><>"),
            "{out}"
        );
        assert_eq!(scratch.trace(), "zshenv\nzshrc\n");
        let out = scratch.run(&zsh, None, &["-l", "-i"], "exit\n");
        assert_eq!(
            scratch.trace(),
            "zshenv\nzprofile\nzshrc\nzlogin\n",
            "{out}"
        );
        // A ZDOTDIR the user set: their files come from there.
        let zdot = scratch.home.join("zdot");
        scratch.write("zdot/.zshrc", "print -r -- zdot-zshrc >> $HOME/trace\n");
        let out = scratch.run(
            &zsh,
            zdot.to_str(),
            &["-i"],
            "print -r -- \"ZDOTDIR=$ZDOTDIR\"\nexit\n",
        );
        assert!(
            out.contains(&format!("ZDOTDIR={}", zdot.display())),
            "{out}"
        );
        assert_eq!(scratch.trace(), "zdot-zshrc\n");
        // An empty one stays empty, and zsh reads none of the user's files
        // from it, as without Kettle.
        let out = scratch.run(
            &zsh,
            Some(""),
            &["-i"],
            "print -r -- \"ZDOTDIR=[${ZDOTDIR-unset}]\"\nexit\n",
        );
        assert!(out.contains("ZDOTDIR=[]"), "{out}");
        assert!(
            !out.contains("no such file"),
            "a missing .zshenv is no error: {out}"
        );
        assert_eq!(scratch.trace(), "");
        // Not interactive: no codex.
        let out = scratch.run(&zsh, None, &["-c", "whence -w codex"], "");
        assert!(out.contains("codex: none"), "{out}");
        // The user's own codex wins, defined or autoloadable.
        scratch.write(".zshrc", "codex() { print -r -- mine; }\n");
        let out = scratch.run(&zsh, None, &["-i"], "codex\nexit\n");
        assert!(
            out.contains("mine") && !out.contains("<agent-setup>"),
            "{out}"
        );
        scratch.write("functions/codex", "print -r -- autoloaded\n");
        scratch.write(
            ".zshrc",
            "fpath=($HOME/functions $fpath)\nautoload -Uz codex\n",
        );
        let out = scratch.run(&zsh, None, &["-i"], "codex\nexit\n");
        assert!(
            out.contains("autoloaded") && !out.contains("<agent-setup>"),
            "{out}"
        );
        drop(scratch.base);
    }

    /// zsh passes over a `.zshenv` it cannot read without a word, and so
    /// does Kettle's.
    #[cfg(unix)]
    #[test]
    fn a_pane_zsh_passes_over_an_unreadable_zshenv() {
        use std::os::unix::fs::PermissionsExt as _;
        let Some(zsh) = installed("zsh") else {
            eprintln!("zsh is not installed; skipped");
            return;
        };
        let scratch = Scratch::new();
        if scratch.additions(&zsh, None).env.is_empty() {
            eprintln!("this system's zshenv keeps Kettle out of zsh; skipped");
            return;
        }
        scratch.write(".zshenv", "print -r -- zshenv\n");
        let zshenv = scratch.home.join(".zshenv");
        std::fs::set_permissions(&zshenv, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&zshenv).is_ok() {
            eprintln!("this user reads any file; skipped");
            return;
        }
        let out = scratch.run(&zsh, None, &["-c", "print -r -- ran"], "");
        assert_eq!(out, "ran\n");
        drop(scratch.base);
    }

    /// A pane's fish reads the user's configuration as it would without
    /// Kettle, `XDG_DATA_DIRS` included, defines `codex` only when
    /// interactive, and leaves alone a `codex` the user defines or can
    /// autoload.
    #[cfg(unix)]
    #[test]
    fn a_pane_fish_defines_codex_and_keeps_its_own_configuration() {
        let Some(fish) = installed("fish") else {
            eprintln!("fish is not installed; skipped");
            return;
        };
        let scratch = Scratch::new();
        scratch.write(".config/fish/config.fish", "echo config >> $HOME/trace\n");
        let probe = "set -q XDG_DATA_DIRS; and echo \"XDG=$XDG_DATA_DIRS\"; or echo XDG=unset; \
                     type -t codex; codex 'a b' '*' ''";
        let out = scratch.run(&fish, None, &["-i", "-c", probe], "");
        assert!(out.contains("XDG=unset"), "{out}");
        assert!(out.contains("function"), "{out}");
        assert!(
            out.contains("<agent-setup><--launch-codex><--><a b><*><>"),
            "{out}"
        );
        assert_eq!(scratch.trace(), "config\n");
        // The user's configuration may set XDG_DATA_DIRS; it keeps it.
        scratch.write(
            ".config/fish/conf.d/paths.fish",
            "set -gx XDG_DATA_DIRS /opt/share\n",
        );
        let out = scratch.run(
            &fish,
            None,
            &["-i", "-c", "echo \"XDG=$XDG_DATA_DIRS\""],
            "",
        );
        assert!(out.contains("XDG=/opt/share"), "{out}");
        let out = scratch.run(
            &fish,
            None,
            &["-c", "type -q codex; and echo yes; or echo no"],
            "",
        );
        assert!(out.trim_end().ends_with("no"), "{out}");
        scratch.write(
            ".config/fish/functions/codex.fish",
            "function codex\n    echo mine\nend\n",
        );
        let out = scratch.run(&fish, None, &["-i", "-c", "codex"], "");
        assert!(
            out.contains("mine") && !out.contains("<agent-setup>"),
            "{out}"
        );
        drop(scratch.base);
    }
}
