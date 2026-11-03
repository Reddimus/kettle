//! Per-pane agent functions. Checked private startup files preserve the user's
//! configuration and existing functions. Linux additionally wraps Claude Code.
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

impl CodexShell {
    /// The shell a program is, by its name: `/bin/zsh`, `fish`, `bash.exe`.
    pub fn from_program(program: &Path) -> Option<Self> {
        match program.file_stem()?.to_str()? {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// The shell's program name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }
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
    agent_function(kettle, shell, "codex")
}

pub fn claude_function(kettle: &Path, shell: CodexShell) -> Result<String, FunctionError> {
    agent_function(kettle, shell, "claude")
}

fn agent_function(kettle: &Path, shell: CodexShell, name: &str) -> Result<String, FunctionError> {
    let path = quoted_kettle_path(kettle, shell)?;
    let declaration = shell_function_declaration(name, shell);
    Ok(match shell {
        CodexShell::Bash | CodexShell::Zsh => {
            format!(
                "{declaration} {{\n  command {path} agent-setup --launch-{name} -- \"$@\"\n}}\n"
            )
        }
        CodexShell::Fish => {
            format!("function {name}\n  command {path} agent-setup --launch-{name} -- $argv\nend\n")
        }
    })
}

fn shell_function_declaration(name: &str, shell: CodexShell) -> String {
    // Both shells can expand aliases in a POSIX-style function declaration.
    match shell {
        CodexShell::Bash => format!("function {name}"),
        CodexShell::Zsh => format!("'{name}'()"),
        CodexShell::Fish => format!("function {name}"),
    }
}

fn quoted_kettle_path(kettle: &Path, shell: CodexShell) -> Result<String, FunctionError> {
    let path = kettle_path(kettle)?;
    let path = match shell {
        CodexShell::Bash | CodexShell::Zsh => path.replace('\'', "'\\''"),
        CodexShell::Fish => path.replace('\\', "\\\\").replace('\'', "\\'"),
    };
    Ok(format!("'{path}'"))
}

/// A wrapper the user can customize with an environment prefix on both
/// commands. `name` must be `claude` or `codex`.
pub fn wrapper_snippet(
    kettle: &Path,
    shell: CodexShell,
    name: &str,
) -> Result<String, FunctionError> {
    let path = quoted_kettle_path(kettle, shell)?;
    let declaration = shell_function_declaration(name, shell);
    let profile = if name == "codex" {
        "CODEX_HOME=\"$HOME/.codex-work\""
    } else {
        "CLAUDE_CONFIG_DIR=\"$HOME/.claude-work\""
    };
    Ok(match shell {
        CodexShell::Bash | CodexShell::Zsh => format!(
            "{declaration} {{\n\
             \x20 # Put your account's environment variables before both commands below,\n\
             \x20 # for example: {profile} command ...\n\
             \x20 if [ -n \"${{KETTLE_PANE_ID-}}\" ] && [ -x {path} ]; then\n\
             \x20   command {path} agent-setup --launch-{name} -- \"$@\"\n\
             \x20 else\n\
             \x20   command {name} \"$@\"\n\
             \x20 fi\n\
             }}\n"
        ),
        CodexShell::Fish => format!(
            "function {name}\n\
             \x20 # Put your account's environment variables before both commands below,\n\
             \x20 # for example: {profile} command ...\n\
             \x20 if set -q KETTLE_PANE_ID; and test -x {path}\n\
             \x20   command {path} agent-setup --launch-{name} -- $argv\n\
             \x20 else\n\
             \x20   command {name} $argv\n\
             \x20 end\n\
             end\n"
        ),
    })
}

/// An experimental POSIX entry script for a program that starts the agent
/// itself. `name` must be `claude` or `codex`.
pub fn entry_script(kettle: &Path, name: &str) -> Result<String, FunctionError> {
    let path = quoted_kettle_path(kettle, CodexShell::Bash)?;
    Ok(format!(
        "#!/bin/sh\n\
         # Hand the launch to Kettle's launcher so a program that starts {name}\n\
         # itself (for example ccx's realClaudePath setting) gets Kettle cards.\n\
         # This entry script is experimental. It must NOT be named claude or codex\n\
         # and must not sit in a PATH directory under either name.\n\
         # It must be user-owned and not writable by others.\n\
         case \"${{0##*/}}\" in claude|codex) echo \"kettle entry script: do not name it claude or codex\" >&2; exit 2 ;; esac\n\
         if [ -n \"${{KETTLE_PANE_ID-}}\" ] && [ -x {path} ]; then\n\
         \x20 exec {path} agent-setup --launch-{name} -- \"$@\"\n\
         fi\n\
         # Outside a Kettle pane, run whatever {name} is first on PATH.\n\
         exec {name} \"$@\"\n"
    ))
}

/// A forgeable diagnostic hint, never an admission check or printable value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFunction {
    Kettle,
    Own,
    Unchecked,
}

/// Read only the literal markers for `claude` and `codex`; other names and
/// values are unchecked. Only the defining shell's PID matches; inherited
/// markers in child shells are unchecked. Never use the marker for admission.
pub fn agent_function_marker(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    name: &str,
    shell_pid: u32,
) -> AgentFunction {
    let variable = match name {
        "claude" => "KETTLE_OWN_CLAUDE",
        "codex" => "KETTLE_OWN_CODEX",
        _ => return AgentFunction::Unchecked,
    };
    match var(variable).as_deref() {
        Some(value) if value == format!("0:{shell_pid}").as_str() => AgentFunction::Kettle,
        Some(value) if value == format!("1:{shell_pid}").as_str() => AgentFunction::Own,
        _ => AgentFunction::Unchecked,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Agents {
    pub codex: bool,
    pub claude: bool,
}

impl Agents {
    fn names(self) -> impl Iterator<Item = &'static str> {
        [
            self.codex.then_some("codex"),
            self.claude.then_some("claude"),
        ]
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
const CODEX: Agents = Agents {
    codex: true,
    claude: false,
};

/// The startup files, by their path inside Kettle's directory for them.
const MANIFEST_FILE: &str = "kettle.json";
const ZSH_DIRECTORY: &str = "zsh";
const ZSH_FILE: &str = "zsh/.zshenv";

/// The startup files that define `codex` in a pane's zsh for the Kettle at
/// `executable`, at `version`. fish needs none: see [`fish_code`].
pub(crate) fn startup_files(
    executable: &Path,
    version: &str,
    agents: Agents,
) -> Result<crate::owned_dir::OwnedFiles, FunctionError> {
    let command = kettle_path(executable)?.to_owned();
    let mut zsh = String::new();
    for name in agents.names() {
        zsh.push_str(&zsh_definition(
            &agent_function(executable, CodexShell::Zsh, name)?,
            name,
        ));
    }
    let manifest = serde_json::json!({"command": command, "version": version});
    Ok(crate::owned_dir::OwnedFiles::new(
        "codex-shell",
        command,
        version.to_owned(),
        vec![
            (MANIFEST_FILE, format!("{manifest}\n")),
            (ZSH_FILE, kettle_core::shell_startup::zshenv(&zsh)),
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
fn zsh_definition(function: &str, name: &str) -> String {
    let marker = format!("KETTLE_OWN_{}", name.to_ascii_uppercase());
    let title = if name == "codex" {
        "Codex"
    } else {
        "Claude Code"
    };
    let setting = if name == "codex" {
        "codex"
    } else {
        "claude-code"
    };
    format!(
        "# Kettle's {title} integration (agent-display-{setting}): before your first\n\
         # prompt, after all your startup files, define {name} unless you have one.\n\
         if [[ -o interactive ]]; then\n\
         \x20 _kettle_{name}() {{\n\
         \x20   'builtin' 'emulate' -L zsh\n\
         \x20   precmd_functions=(${{precmd_functions:#_kettle_{name}}})\n\
         \x20   'builtin' 'unfunction' '_kettle_{name}'\n\
         \x20   if (( ! ${{+functions[{name}]}} )); then\n\
         {function}\
         \x20     if (( ${{+aliases[{name}]}} )); then\n\
         \x20       'builtin' 'export' {marker}=\n\
         \x20     else\n\
         \x20       'builtin' 'export' {marker}=0:$$\n\
         \x20     fi\n\
         \x20   else\n\
         \x20     'builtin' 'export' {marker}=1:$$\n\
         \x20   fi\n\
         \x20 }}\n\
         \x20 'builtin' 'typeset' -ga precmd_functions\n\
         \x20 precmd_functions+=(_kettle_{name})\n\
         fi\n"
    )
}

/// The fish code a pane's fish runs after the user's configuration (`fish
/// -C`), for the Kettle at `executable`: in an interactive shell, it defines
/// `codex` unless the user has one, defined or autoloadable.
///
/// The function goes in as it is, never re-indented: a newline inside the
/// quoted path is part of the path.
pub(crate) fn fish_code(executable: &Path, agents: Agents) -> Result<String, FunctionError> {
    let mut code = String::new();
    for name in agents.names() {
        let function = agent_function(executable, CodexShell::Fish, name)?;
        let marker = format!("KETTLE_OWN_{}", name.to_ascii_uppercase());
        code.push_str(&format!(
            "if status is-interactive\n\
             \x20 if functions -q {name}\n\
             \x20   set -gx {marker} 1:$fish_pid\n\
             \x20 else\n\
             {function}\
             \x20   set -gx {marker} 0:$fish_pid\n\
             \x20 end\n\
             end\n"
        ));
    }
    Ok(code)
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
    keep_older: bool,
) -> Result<std::path::PathBuf, ()> {
    crate::owned_dir::install(root, files, identity, keep_older)
}

/// The startup for the Kettle at `executable`, at `version`, with its files
/// written under `root` unless they are there intact.
pub(crate) fn prepare(
    root: &Path,
    executable: &Path,
    version: &str,
    agents: Agents,
    keep_older: bool,
) -> Result<Offered, ShellRefusal> {
    let files = startup_files(executable, version, agents)?;
    let fish = fish_code(executable, agents)?;
    let directory = install(root, &files, keep_older).map_err(|()| ShellRefusal::Unavailable)?;
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

    /// A shell is known by its program's name, wherever it lives and with
    /// any extension; another shell is none, and each name reads back.
    #[test]
    fn a_shell_is_known_by_its_programs_name() {
        for (program, shell) in [
            ("/bin/zsh", Some(CodexShell::Zsh)),
            ("/opt/homebrew/bin/fish", Some(CodexShell::Fish)),
            ("bash.exe", Some(CodexShell::Bash)),
            ("/usr/bin/nu", None),
            ("/bin/zshell", None),
        ] {
            assert_eq!(
                CodexShell::from_program(Path::new(program)),
                shell,
                "{program}"
            );
        }
        for shell in [CodexShell::Bash, CodexShell::Zsh, CodexShell::Fish] {
            assert_eq!(
                CodexShell::from_program(Path::new(shell.name())),
                Some(shell)
            );
        }
    }

    /// The startup equals the fixtures the shell-integration check runs under
    /// every supported zsh and fish, for a stand-in Kettle at
    /// `/kettle-stand-in/bin/kettle`. After changing it, write the fixtures
    /// again from `startup_files` and `fish_code`. Unix only, where the
    /// stand-in path is absolute and panes get the startup.
    #[cfg(unix)]
    #[test]
    fn the_startup_matches_the_checked_fixtures() {
        let kettle = Path::new("/kettle-stand-in/bin/kettle");
        for name in ["claude", "codex"] {
            let agents = Agents {
                codex: name == "codex",
                claude: name == "claude",
            };
            let files = startup_files(kettle, "0", agents).unwrap();
            let zsh = &files
                .files
                .iter()
                .find(|(path, _)| *path == ZSH_FILE)
                .unwrap()
                .1;
            assert_fixture(&format!("shell-integration/{name}-zshenv"), zsh);
            assert_fixture(
                &format!("shell-integration/{name}-init.fish"),
                &fish_code(kettle, agents).unwrap(),
            );
        }
    }

    #[cfg(unix)]
    fn assert_fixture(path: &str, actual: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/fixtures")
            .join(path);
        assert_eq!(
            actual.as_bytes(),
            std::fs::read(&path).unwrap(),
            "{}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_wrapper_and_entry_snippets_match_their_fixtures() {
        let kettle = Path::new("/fixture/kettle");
        for name in ["claude", "codex"] {
            for shell in [CodexShell::Bash, CodexShell::Zsh, CodexShell::Fish] {
                assert_fixture(
                    &format!("agent-wrappers/{name}-{}.txt", shell.name()),
                    &wrapper_snippet(kettle, shell, name).unwrap(),
                );
            }
            assert_fixture(
                &format!("agent-wrappers/{name}-entry.txt"),
                &entry_script(kettle, name).unwrap(),
            );
        }
    }

    #[test]
    fn agent_function_markers_accept_only_known_names_and_exact_shell_pids() {
        for name in ["claude", "codex"] {
            for (value, expected) in [
                (Some("0:12345"), AgentFunction::Kettle),
                (Some("1:12345"), AgentFunction::Own),
                (Some("0"), AgentFunction::Unchecked),
                (Some("1"), AgentFunction::Unchecked),
                (Some("0:12346"), AgentFunction::Unchecked),
                (Some("1:12346"), AgentFunction::Unchecked),
                (Some("0:012345"), AgentFunction::Unchecked),
                (Some("1:012345"), AgentFunction::Unchecked),
                (Some("0:+12345"), AgentFunction::Unchecked),
                (Some("1:-12345"), AgentFunction::Unchecked),
                (Some("00:12345"), AgentFunction::Unchecked),
                (Some("01:12345"), AgentFunction::Unchecked),
                (Some("2:12345"), AgentFunction::Unchecked),
                (Some("0:12345x"), AgentFunction::Unchecked),
                (Some("1:12345:0"), AgentFunction::Unchecked),
                (Some("0:12345\n"), AgentFunction::Unchecked),
                (Some("1:12345 "), AgentFunction::Unchecked),
                (Some(" 0:12345"), AgentFunction::Unchecked),
                (Some("0:"), AgentFunction::Unchecked),
                (Some("1:4294967296"), AgentFunction::Unchecked),
                (Some(""), AgentFunction::Unchecked),
                (Some("2"), AgentFunction::Unchecked),
                (Some("true"), AgentFunction::Unchecked),
                (None, AgentFunction::Unchecked),
            ] {
                let var = |key: &str| {
                    assert_eq!(key, format!("KETTLE_OWN_{}", name.to_ascii_uppercase()));
                    value.map(std::ffi::OsString::from)
                };
                assert_eq!(
                    agent_function_marker(&var, name, 12345),
                    expected,
                    "{name} {value:?}"
                );
                if expected != AgentFunction::Unchecked {
                    assert_eq!(
                        agent_function_marker(&var, name, 12346),
                        AgentFunction::Unchecked,
                        "child shell inherited {name} {value:?}"
                    );
                }
            }
        }
        assert_eq!(
            agent_function_marker(
                &|_| panic!("unknown names must not read the environment"),
                "agy",
                12345
            ),
            AgentFunction::Unchecked
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            assert_eq!(
                agent_function_marker(
                    &|_| Some(std::ffi::OsString::from_vec(b"0:12345\xff".to_vec())),
                    "codex",
                    12345,
                ),
                AgentFunction::Unchecked
            );
        }
    }

    #[test]
    fn wrapper_and_entry_paths_use_the_function_validation() {
        for (path, error) in [
            (Path::new("relative/kettle"), FunctionError::UnnamedKettle),
            #[cfg(unix)]
            (
                Path::new("/x/AppTranslocation/y/kettle"),
                FunctionError::Translocated,
            ),
        ] {
            for name in ["claude", "codex"] {
                assert_eq!(entry_script(path, name), Err(error));
                for shell in [CodexShell::Bash, CodexShell::Zsh, CodexShell::Fish] {
                    assert_eq!(wrapper_snippet(path, shell, name), Err(error));
                }
            }
        }
    }

    /// The startup's files are installed, and what new panes are offered
    /// names them and carries the fish code.
    #[cfg(unix)]
    #[test]
    fn prepare_installs_the_files_new_panes_are_offered() {
        let base = kettle_test_support::private_tempdir("kettle-codex-shell-");
        let kettle = Path::new("/kettle-stand-in/bin/kettle");
        let root = base.path().join("agent-shell");
        let offered = prepare(&root, kettle, "5.0.0", CODEX, false).unwrap();
        assert!(offered.directory.starts_with(&root));
        assert!(offered.directory.join(ZSH_FILE).is_file());
        assert_eq!(offered.fish, fish_code(kettle, CODEX).unwrap());
        assert_eq!(
            crate::owned_dir::verify(&offered.directory, &offered.files),
            Ok(())
        );
        assert_eq!(
            prepare(&root, Path::new("relative/kettle"), "5.0.0", CODEX, false).err(),
            Some(ShellRefusal::Unavailable)
        );
    }

    /// Two Kettles of one version with different agents enabled each offer
    /// their own startup files; installing one keeps the other's.
    #[cfg(unix)]
    #[test]
    fn installing_another_agent_selection_keeps_the_previous_startup() {
        let base = kettle_test_support::private_tempdir("kettle-codex-shell-");
        let kettle = Path::new("/kettle-stand-in/bin/kettle");
        let root = base.path().join("agent-shell");
        let first = prepare(&root, kettle, "5.0.0", CODEX, false).unwrap();
        let second = prepare(
            &root,
            kettle,
            "5.0.0",
            Agents {
                codex: true,
                claude: true,
            },
            false,
        )
        .unwrap();
        assert_ne!(first.directory, second.directory);
        for offered in [&first, &second] {
            assert_eq!(
                crate::owned_dir::verify(&offered.directory, &offered.files),
                Ok(())
            );
        }
        crate::owned_dir::remove_tree(&root).unwrap();
    }

    /// A newline in Kettle's path is part of the quoted path in both shells,
    /// so the definitions carry the function exactly as it is.
    #[test]
    fn the_definitions_carry_the_function_unchanged() {
        // Absolute on each platform: a function names Kettle only so.
        #[cfg(unix)]
        let kettle = Path::new("/opt/Kettle\nApp/kettle");
        #[cfg(windows)]
        let kettle = Path::new("C:\\Kettle\nApp\\kettle.exe");
        let zsh = codex_function(kettle, CodexShell::Zsh).unwrap();
        assert!(zsh_definition(&zsh, "codex").contains(&zsh));
        let fish = codex_function(kettle, CodexShell::Fish).unwrap();
        assert!(fish_code(kettle, CODEX).unwrap().contains(&fish));
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
            Self::agents(CODEX)
        }

        fn agents(agents: Agents) -> Self {
            let base = kettle_test_support::private_tempdir("kettle-codex-shell-");
            let kettle = base.path().join("bin/kettle");
            std::fs::create_dir_all(kettle.parent().unwrap()).unwrap();
            let source = base.path().join("kettle.sh");
            std::fs::write(&source, "#!/bin/sh\nprintf '<%s>' \"$@\"\n").unwrap();
            // Copied so that no child another test forks meanwhile holds it
            // open for writing, which fails running it (ETXTBSY) on Linux.
            kettle_test_support::copy_executable_fixture(&source, &kettle).unwrap();
            let offered = prepare(
                &base.path().join("kettle/agent-shell"),
                &kettle,
                "5.0.0",
                agents,
                false,
            )
            .unwrap();
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
            self.launch(program, zdotdir, arguments, input, Some(&[]))
        }

        /// [`Self::run`] without Kettle's additions: the shell as it would
        /// be outside Kettle, its system configuration included.
        fn run_bare(&self, program: &str, arguments: &[&str]) -> String {
            self.launch(program, None, arguments, "", None)
        }

        fn launch(
            &self,
            program: &str,
            zdotdir: Option<&str>,
            arguments: &[&str],
            input: &str,
            kettle: Option<&[(&str, &str)]>,
        ) -> String {
            use std::io::Write as _;
            let _ = std::fs::remove_file(self.home.join("trace"));
            let additions = kettle.map(|_| self.additions(program, zdotdir));
            let mut command = std::process::Command::new(program);
            command
                .env_clear()
                .env("HOME", &self.home)
                .env("TERM", "dumb")
                .env("PATH", "/usr/bin:/bin")
                .envs(kettle.into_iter().flatten().copied())
                .args(arguments)
                .args(additions.iter().flat_map(|additions| &additions.args))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(zdotdir) = zdotdir {
                command.env("ZDOTDIR", zdotdir);
            }
            if let Some(additions) = additions {
                command.envs(additions.env);
            }
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
        use std::os::unix::fs::PermissionsExt as _;
        let search = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&search) {
            let path = if directory.is_absolute() {
                directory.join(program)
            } else {
                std::env::current_dir().ok()?.join(directory).join(program)
            };
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return path.to_str().map(str::to_owned);
            }
        }
        None
    }

    #[cfg(unix)]
    #[test]
    fn native_shell_discovery_uses_path() {
        const CHILD: &str = "KETTLE_TEST_SHELL_LOOKUP_CHILD";
        if let Ok(expected) = std::env::var(CHILD) {
            let actual = installed("sh").expect("shell on PATH");
            assert!(Path::new(&actual).is_absolute());
            assert_eq!(
                std::fs::canonicalize(actual).unwrap(),
                std::fs::canonicalize(expected).unwrap()
            );
            return;
        }
        use std::os::unix::fs::PermissionsExt as _;
        let directory = kettle_test_support::private_tempdir("kettle-shell-lookup-");
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let program = bin.join("sh");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let blocked = directory.path().join("sh");
        std::fs::write(&blocked, "not executable").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o600)).unwrap();
        for paths in [
            std::env::join_paths([directory.path(), bin.as_path()]).unwrap(),
            std::env::join_paths([Path::new("."), Path::new("bin")]).unwrap(),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "codex_shell::tests::native_shell_discovery_uses_path",
                ])
                .current_dir(directory.path())
                .env("PATH", paths)
                .env(CHILD, &program)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[cfg(unix)]
    fn assert_shell_marker(output: &str, state: &str) {
        let pid = output
            .lines()
            .find_map(|line| line.strip_prefix("shell_pid="))
            .unwrap_or_else(|| panic!("missing shell PID: {output}"))
            .parse::<u32>()
            .unwrap();
        let expected = if state.is_empty() {
            String::new()
        } else {
            format!("{state}:{pid}")
        };
        assert!(
            output
                .lines()
                .any(|line| line == format!("marker={expected}")),
            "{output}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zsh_records_user_functions_aliases_and_overwrites_inherited_markers() {
        let Some(zsh) = installed("zsh") else {
            eprintln!("zsh is not installed; skipped");
            return;
        };
        let scratch = Scratch::agents(Agents {
            codex: true,
            claude: true,
        });
        if scratch.additions(&zsh, None).env.is_empty() {
            eprintln!("this system's zshenv keeps Kettle out of zsh; skipped");
            return;
        }
        for name in ["claude", "codex"] {
            let marker = format!("KETTLE_OWN_{}", name.to_ascii_uppercase());
            for (rc, inherited, expected) in [
                (String::new(), "1:999999", "0"),
                (
                    format!("{name}() {{ print -r -- user; }}\n"),
                    "0:999999",
                    "1",
                ),
                (format!("alias {name}='print -r -- user'\n"), "1:999999", ""),
                (format!("autoload -Uz {name}\n"), "0:999999", "1"),
            ] {
                scratch.write(".zshrc", &rc);
                let probe = format!(
                    "printf '\\nshell_pid=%s\\n' \"$$\"\n\
                     command /bin/sh -c 'printf \"\\nmarker=%s\\n\" \"${marker}\"'\n\
                     print -r -- \"function=${{+functions[{name}]}} hook=${{+functions[_kettle_{name}]}}\"\n\
                     exit\n"
                );
                let out =
                    scratch.launch(&zsh, None, &["-i"], &probe, Some(&[(&marker, inherited)]));
                assert_shell_marker(&out, expected);
                assert!(out.contains("function=1 hook=0"), "{rc:?}: {out}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn fish_records_user_functions_abbreviations_and_overwrites_inherited_markers() {
        let Some(fish) = installed("fish") else {
            eprintln!("fish is not installed; skipped");
            return;
        };
        for name in ["claude", "codex"] {
            for (config, autoload, inherited, expected) in [
                (String::new(), false, "1:999999", "0"),
                (
                    format!("function {name}\n echo user\nend\n"),
                    false,
                    "0:999999",
                    "1",
                ),
                (
                    format!("abbr --add {name} echo user\n"),
                    false,
                    "1:999999",
                    "0",
                ),
                (String::new(), true, "0:999999", "1"),
            ] {
                let scratch = Scratch::agents(Agents {
                    codex: true,
                    claude: true,
                });
                scratch.write(".config/fish/config.fish", &config);
                if autoload {
                    scratch.write(
                        &format!(".config/fish/functions/{name}.fish"),
                        &format!("function {name}\n echo user\nend\n"),
                    );
                }
                let marker = format!("KETTLE_OWN_{}", name.to_ascii_uppercase());
                let probe = format!(
                    "echo shell_pid=$fish_pid; command /bin/sh -c 'printf \"marker=%s\\n\" \"${marker}\"'; \
                     functions -q {name}; and echo function-present; {name}"
                );
                let out = scratch.launch(
                    &fish,
                    None,
                    &["-i", "-c", &probe],
                    "",
                    Some(&[(&marker, inherited)]),
                );
                assert_shell_marker(&out, expected);
                assert!(out.contains("function-present"), "{config:?}: {out}");
                assert_eq!(
                    out.contains("<agent-setup>"),
                    expected == "0",
                    "{config:?}: {out}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn printed_functions_and_wrappers_preserve_existing_aliases() {
        for (program, shell) in [("zsh", CodexShell::Zsh), ("bash", CodexShell::Bash)] {
            let Some(program) = installed(program) else {
                continue;
            };
            for name in ["claude", "codex"] {
                for script in [
                    agent_function(Path::new("/fixture/kettle"), shell, name).unwrap(),
                    wrapper_snippet(Path::new("/fixture/kettle"), shell, name).unwrap(),
                ] {
                    let options = if shell == CodexShell::Bash {
                        "shopt -s expand_aliases\n"
                    } else {
                        ""
                    };
                    let inspect = if shell == CodexShell::Bash {
                        format!("declare -f {name}")
                    } else {
                        format!("functions {name}")
                    };
                    let probe = format!(
                        "{options}alias {name}='printf preserved-alias'\n\
                     eval \"$1\" || exit\n\
                     {inspect} || exit\n\
                     alias {name}\n"
                    );
                    let output = std::process::Command::new(&program)
                        .args(["-f", "-c", &probe, "wrapper-test", &script])
                        .env_clear()
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{name}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let stdout = String::from_utf8(output.stdout).unwrap();
                    assert!(stdout.contains(&format!("agent-setup --launch-{name}")));
                    assert!(stdout.contains(&format!("{name}='printf preserved-alias'")));
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_and_entry_paths_with_quotes_and_newlines_are_valid_shell_syntax() {
        let kettle = Path::new("/fixture/quote'and\nnewline\\/kettle");
        for (program, shell, flag) in [
            ("bash", CodexShell::Bash, "-n"),
            ("zsh", CodexShell::Zsh, "-n"),
            ("fish", CodexShell::Fish, "--no-execute"),
            ("sh", CodexShell::Bash, "-n"),
        ] {
            let Some(program) = installed(program) else {
                eprintln!("{program} is not installed; skipped");
                continue;
            };
            for name in ["claude", "codex"] {
                let mut scripts = vec![wrapper_snippet(kettle, shell, name).unwrap()];
                if shell != CodexShell::Fish {
                    scripts.push(entry_script(kettle, name).unwrap());
                }
                if Path::new(&program)
                    .file_name()
                    .is_some_and(|name| name == "sh")
                {
                    scripts = vec![entry_script(kettle, name).unwrap()];
                }
                for script in scripts {
                    let output = std::process::Command::new(&program)
                        .args([flag, "-c", &script])
                        .env_clear()
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{program}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn wrappers_and_entries_forward_arguments_environment_status_and_fallback() {
        let Some(bash) = installed("bash") else {
            eprintln!("bash is not installed; skipped");
            return;
        };
        let Some(sh) = installed("sh") else {
            eprintln!("sh is not installed; skipped");
            return;
        };
        let base = kettle_test_support::private_tempdir("kettle-agent-wrapper-");
        let bin = base.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let source = base.path().join("stand-in.sh");
        let kettle = bin.join("quote'and\nnewline\\kettle");
        std::fs::write(
            &source,
            "#!/bin/sh\nprintf 'kettle:%s\\n' \"$CODEX_HOME\"\nprintf '%s\\n' \"$@\"\nexit 37\n",
        )
        .unwrap();
        kettle_test_support::copy_executable_fixture(&source, &kettle).unwrap();
        std::fs::write(
            &source,
            "#!/bin/sh\nprintf 'plain:%s\\n' \"$CODEX_HOME\"\nprintf '%s\\n' \"$@\"\nexit 19\n",
        )
        .unwrap();
        for name in ["claude", "codex"] {
            kettle_test_support::copy_executable_fixture(&source, &bin.join(name)).unwrap();
        }
        let non_executable = bin.join("not-executable");
        std::fs::write(&non_executable, "must not run\n").unwrap();
        let missing = bin.join("missing");
        let args = ["", "two words", "line\nbreak", "--"];
        let home = base.path().join(".codex-work");
        for name in ["claude", "codex"] {
            for (path, pane, launched) in [
                (kettle.as_path(), true, true),
                (kettle.as_path(), false, false),
                (non_executable.as_path(), true, false),
                (missing.as_path(), true, false),
            ] {
                let wrapper = wrapper_snippet(path, CodexShell::Bash, name)
                    .unwrap()
                    .replace(
                        "    command ",
                        "    CODEX_HOME=\"$HOME/.codex-work\" command ",
                    );
                let wrapper = format!("{wrapper}{name} \"$@\"\n");
                for (program, script) in
                    [(&bash, wrapper), (&sh, entry_script(path, name).unwrap())]
                {
                    let mut command = std::process::Command::new(program);
                    command
                        .env_clear()
                        .env("HOME", base.path())
                        .env("PATH", &bin);
                    // The entry inherits the prefix from its caller.
                    if program == &sh {
                        let entry = base.path().join(format!("{name}-entry"));
                        std::fs::write(&entry, &script).unwrap();
                        command.arg(entry).env("CODEX_HOME", &home);
                        for filename in ["claude", "codex"] {
                            let entry = base.path().join(filename);
                            std::fs::write(&entry, &script).unwrap();
                            let output = std::process::Command::new(&sh)
                                .env_clear()
                                .env("PATH", &bin)
                                .env("KETTLE_PANE_ID", "fixture-pane")
                                .arg(entry)
                                .args(args)
                                .output()
                                .unwrap();
                            assert_eq!(output.status.code(), Some(2), "{filename}");
                            assert!(output.stdout.is_empty());
                            assert_eq!(
                                output.stderr,
                                b"kettle entry script: do not name it claude or codex\n"
                            );
                        }
                    } else {
                        command.args(["-c", &script, "wrapper-test"]);
                    }
                    command.args(args);
                    if pane {
                        command.env("KETTLE_PANE_ID", "fixture-pane");
                    }
                    let output = command.output().unwrap();
                    assert_eq!(output.status.code(), Some(if launched { 37 } else { 19 }));
                    assert!(
                        output.stderr.is_empty(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let mut expected = format!(
                        "{}:{}\n",
                        if launched { "kettle" } else { "plain" },
                        home.display()
                    );
                    if launched {
                        expected.push_str(&format!("agent-setup\n--launch-{name}\n--\n"));
                    }
                    expected.push_str(&args.join("\n"));
                    expected.push('\n');
                    assert_eq!(
                        output.stdout,
                        expected.as_bytes(),
                        "{program} {path:?} pane={pane}"
                    );
                }
            }
        }
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

    #[cfg(unix)]
    #[test]
    fn selected_agents_get_functions_without_replacing_user_functions() {
        for codex in [false, true] {
            let scratch = Scratch::agents(Agents {
                codex,
                claude: true,
            });
            if let Some(zsh) = installed("zsh").filter(|zsh| {
                if scratch.additions(zsh, None).env.is_empty() {
                    eprintln!("system zshenv prevents startup injection; zsh case unrun");
                    false
                } else {
                    true
                }
            }) {
                let out = scratch.run(
                    &zsh,
                    None,
                    &["-i"],
                    "whence -w codex claude\nclaude 'a b' '*' ''\nexit\n",
                );
                assert!(
                    out.contains("<agent-setup><--launch-claude><--><a b><*><>"),
                    "{out}"
                );
                assert_eq!(out.contains("codex: function"), codex, "{out}");
                scratch.write(".zshrc", "claude() { print -r -- user-claude; }\n");
                let out = scratch.run(&zsh, None, &["-i"], "claude\nexit\n");
                assert!(
                    out.contains("user-claude") && !out.contains("<agent-setup>"),
                    "{out}"
                );
                scratch.write(".zshrc", "");
                let out = scratch.run(&zsh, None, &["-c", "whence -w claude"], "");
                assert!(out.contains("claude: none"), "{out}");
            }
            if let Some(fish) = installed("fish") {
                let out = scratch.run(
                    &fish,
                    None,
                    &["-i", "-c", "type -t claude; claude 'a b' '*' ''"],
                    "",
                );
                assert!(
                    out.contains("<agent-setup><--launch-claude><--><a b><*><>"),
                    "{out}"
                );
                let out = scratch.run(
                    &fish,
                    None,
                    &["-i", "-c", "functions -q codex; and echo codex-present"],
                    "",
                );
                assert_eq!(out.contains("codex-present"), codex, "{out}");
                scratch.write(
                    ".config/fish/functions/claude.fish",
                    "function claude\n echo user-claude\nend\n",
                );
                let out = scratch.run(&fish, None, &["-i", "-c", "claude"], "");
                assert!(
                    out.contains("user-claude") && !out.contains("<agent-setup>"),
                    "{out}"
                );
            }
        }
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
        let show_xdg = "set -q XDG_DATA_DIRS; and echo \"XDG=$XDG_DATA_DIRS\"; or echo XDG=unset";
        // XDG_DATA_DIRS as fish leaves it outside Kettle: the system's own
        // configuration may set it (snapd's does).
        let xdg = |out: &str| {
            out.lines()
                .find(|line| line.starts_with("XDG="))
                .map(str::to_owned)
        };
        let outside = xdg(&scratch.run_bare(&fish, &["-i", "-c", show_xdg]));
        let probe = format!("{show_xdg}; type -t codex; codex 'a b' '*' ''");
        let out = scratch.run(&fish, None, &["-i", "-c", &probe], "");
        assert!(outside.is_some() && xdg(&out) == outside, "{out}");
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
        let out = scratch.run(&fish, None, &["-i", "-c", show_xdg], "");
        let outside = xdg(&scratch.run_bare(&fish, &["-i", "-c", show_xdg]));
        assert!(
            xdg(&out).is_some_and(|xdg| xdg.starts_with("XDG=/opt/share")) && xdg(&out) == outside,
            "{out}"
        );
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
