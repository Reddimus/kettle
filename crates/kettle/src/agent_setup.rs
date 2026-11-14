//! `kettle agent-setup`: Codex's opt-in route to Kettle's display server.
//!
//! Kettle edits no shell startup file and none of Codex's configuration.
//! `--print` prints a shell function named `codex` for the user to review and
//! add to their shell's startup file. The function hands each launch, as its
//! arguments, to the hidden `--launch-codex`, which decides from those
//! arguments alone, with no shell evaluation, whether it starts an
//! interactive session: a fresh one, `codex resume` or `codex fork`. Only
//! those get Kettle's display server, as per-launch `-c` overrides that run
//! this Kettle's `mcp --display`, forward the pane's `KETTLE_PANE_ID` and
//! `KETTLE_PID`, and enable `kettle_show` alone. Every other command, and any
//! form not fully understood, runs with its arguments unchanged.
//!
//! Linux also wraps native Claude launches for checked card admission.
//! `--agent claude` selects its printed function. Setup is Unix-only.
#![cfg_attr(not(unix), allow(dead_code))]

use crate::cli::{SetupAgent, SetupArgs, SetupForm, SetupShell};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a Codex invocation starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Session {
    Fresh,
    Resume,
    Fork,
    /// Anything else, which runs with its arguments unchanged.
    PassThrough,
}

/// A classified invocation: what it starts, where Kettle's options go
/// (right after the subcommand that starts the session, or first for a fresh
/// one, so they never follow a prompt or `--`), whether it already runs
/// without the background server, and whether it sets hooks itself.
struct Classified {
    session: Session,
    insert_at: usize,
    no_daemon: bool,
    own_hooks: bool,
}

/// Whether a `-c` value sets Codex's hooks, where Kettle's card hook would
/// be: one set later on the command line replaces another.
fn sets_hooks(value: &str) -> bool {
    value
        .split_once('=')
        .is_some_and(|(key, _)| key.trim() == "hooks" || key.trim().starts_with("hooks."))
}

/// Codex options that take a value in the next argument, in every form an
/// interactive launch accepts (Codex CLI 0.162.0).
fn takes_value(option: &str) -> bool {
    matches!(
        option,
        "-c" | "--config"
            | "-C"
            | "--cd"
            | "-m"
            | "--model"
            | "-p"
            | "--profile"
            | "-s"
            | "--sandbox"
            | "-a"
            | "--ask-for-approval"
            | "--enable"
            | "--disable"
            | "--local-provider"
            | "--add-dir"
    )
}

/// Classify a complete invocation. An option's value never becomes a
/// subcommand or a prompt. Help, version, a remote session, images (whose
/// list takes any number of values), a value that looks like an option, and
/// anything unknown pass through, as does any argument that is not Unicode,
/// before `--` or after it.
fn classify(args: &[OsString]) -> Classified {
    let pass_through = Classified {
        session: Session::PassThrough,
        insert_at: 0,
        no_daemon: false,
        own_hooks: false,
    };
    let mut session = Session::Fresh;
    let mut insert_at = 0;
    let mut no_daemon = false;
    let mut own_hooks = false;
    let mut positionals = 0;
    // A fresh session takes one prompt, a resumed or forked one an id and a
    // prompt.
    let most = |session: Session| if session == Session::Fresh { 1 } else { 2 };
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let Some(arg) = arg.to_str() else {
            return pass_through;
        };
        if arg == "--" {
            // What follows is the session's id and prompt, as they are.
            let rest = &args[index + 1..];
            if rest.iter().any(|arg| arg.to_str().is_none())
                || positionals + rest.len() > most(session)
            {
                return pass_through;
            }
            break;
        }
        if takes_value(arg) {
            let Some(value) = args
                .get(index + 1)
                .and_then(|value| value.to_str())
                .filter(|value| !value.starts_with('-'))
            else {
                return pass_through;
            };
            own_hooks |= matches!(arg, "-c" | "--config") && sets_hooks(value);
            index += 2;
            continue;
        }
        if let Some((option, value)) = arg.split_once('=')
            && option.starts_with("--")
            && takes_value(option)
            && !value.is_empty()
        {
            own_hooks |= option == "--config" && sets_hooks(value);
            index += 1;
            continue;
        }
        match arg {
            "--no-daemon" => no_daemon = true,
            "--no-alt-screen"
            | "--oss"
            | "--search"
            | "--strict-config"
            | "--approve-for-me"
            | "--dangerously-bypass-approvals-and-sandbox"
            | "--dangerously-bypass-hook-trust"
            | "--worktree" => {}
            "--last" | "--all" if session != Session::Fresh => {}
            "--include-non-interactive" if session == Session::Resume => {}
            "resume" | "fork" if session == Session::Fresh && positionals == 0 => {
                session = if arg == "resume" {
                    Session::Resume
                } else {
                    Session::Fork
                };
                insert_at = index + 1;
            }
            _ if arg.starts_with('-') => return pass_through,
            _ => {
                // Any other first word is a subcommand or a prompt; only a
                // prompt is known not to be one, by its position after a
                // session's id. A fresh session takes one prompt, a resumed
                // or forked one an id and a prompt.
                if session == Session::Fresh && positionals == 0 && is_subcommand(arg) {
                    return pass_through;
                }
                positionals += 1;
                if positionals > most(session) {
                    return pass_through;
                }
            }
        }
        index += 1;
    }
    Classified {
        session,
        insert_at,
        no_daemon,
        own_hooks,
    }
}

/// Codex's subcommands other than `resume` and `fork` (Codex CLI 0.162.0).
fn is_subcommand(word: &str) -> bool {
    matches!(
        word,
        "agents"
            | "exec"
            | "e"
            | "review"
            | "login"
            | "logout"
            | "mcp"
            | "plugin"
            | "app-server"
            | "remote-control"
            | "app"
            | "completion"
            | "update"
            | "doctor"
            | "sandbox"
            | "debug"
            | "apply"
            | "a"
            | "queue"
            | "archive"
            | "delete"
            | "migrate-rollouts"
            | "unarchive"
            | "cloud"
            | "exec-server"
            | "features"
            | "help"
    )
}

/// `text` as a TOML basic string.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `items` as a TOML array's contents.
fn toml_list(items: &[&str]) -> String {
    items
        .iter()
        .map(|item| toml_string(item))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The server table Codex gets for this launch: this Kettle's display
/// server, the pane's two variables, and `kettle_show` alone for the model,
/// approved for this launch by its own name, never the server as a whole: it
/// only puts media on the user's shelf and returns no contents, and a Codex
/// whose approval policy is `never` refuses every call that would ask. With
/// `cards`, the server serves Kettle's card hook, and `kettle_card`, which
/// Codex lets only an enabled tool's hook call, is enabled for it; the
/// server never lists it, so the model never sees it.
fn server_table(kettle: &str, cards: bool) -> String {
    let (args, tools): (&[&str], &[&str]) = if cards {
        (
            &["mcp", "--display", "--codex-card-hook"],
            &["kettle_show", "kettle_card", "kettle_agent_hook"],
        )
    } else {
        (&["mcp", "--display"], &["kettle_show"])
    };
    format!(
        "{{command = {}, args = [{}], enabled = true, env_vars = [{}], enabled_tools = [{}], \
         tools = {{kettle_show = {{approval_mode = \"approve\"}}}}}}",
        toml_string(kettle),
        toml_list(args),
        toml_list(&["KETTLE_PANE_ID", "KETTLE_PID"]),
        toml_list(tools),
    )
}

/// Kettle's card hook: after each `kettle_show`, Codex asks Kettle's server
/// for that call's card, by the call's id, and prints it under the call.
/// Codex adds it to the user's own hooks, and runs it only once the user has
/// trusted it in Codex's hook review; Kettle never trusts it on their behalf.
fn card_hook() -> String {
    format!(
        "hooks.PostToolUse=[{{matcher = {}, hooks = [{{type = \"mcp_tool\", server = \"kettle\", \
         tool = \"kettle_card\", input = {{tool_use_id = {}}}, timeout = 3}}]}}]",
        toml_string("mcp__kettle__kettle_show"),
        toml_string("${tool_use_id}"),
    )
}

/// The arguments Codex runs with: Kettle's options added to an interactive
/// session's, anything else unchanged. With `cards`, its card hook too,
/// unless the launch sets hooks itself.
fn launch_args(
    original: &[OsString],
    kettle: &Path,
    cards: bool,
) -> Result<Vec<OsString>, SetupError> {
    let classified = classify(original);
    if classified.session == Session::PassThrough {
        return Ok(original.to_vec());
    }
    let kettle = kettle_path(kettle)?;
    let cards = cards && !classified.own_hooks;
    let mut injected: Vec<OsString> = Vec::with_capacity(5);
    // A session on the shared background server would not get this launch's
    // server. Codex refuses the flag twice.
    if !classified.no_daemon {
        injected.push("--no-daemon".into());
    }
    injected.push("-c".into());
    injected.push(format!("mcp_servers.kettle={}", server_table(kettle, cards)).into());
    if cards {
        injected.push("-c".into());
        let fallback = |event: &str, matcher: &str| {
            format!(
                "{{matcher = {}, hooks = [{{type = \"mcp_tool\", server = \"kettle\", tool = \"kettle_agent_hook\", input = {{event = {}}}, timeout = 24}}]}}",
                toml_string(matcher),
                toml_string(event),
            )
        };
        injected.push(card_hook().into());
        for event in ["UserPromptSubmit", "Stop"] {
            injected.push("-c".into());
            injected.push(format!("hooks.{event}=[{}]", fallback(event, "*")).into());
        }
    }
    let mut launch = Vec::with_capacity(original.len() + injected.len());
    launch.extend_from_slice(&original[..classified.insert_at]);
    launch.extend(injected);
    launch.extend_from_slice(&original[classified.insert_at..]);
    Ok(launch)
}

/// Why a setup step cannot go ahead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupError {
    /// Kettle's path is relative or not UTF-8.
    UnnamedKettle,
    /// Kettle runs from a translocated copy, whose path does not last.
    Translocated,
    /// No shell was named and `SHELL` names none Kettle prints for.
    UnknownShell,
}

impl SetupError {
    fn message(self) -> &'static str {
        match self {
            Self::UnnamedKettle => "Kettle's path cannot be named in a shell function.",
            Self::Translocated => {
                "Kettle is running from a temporary copy. Move it to Applications, open it from there, and run this again."
            }
            Self::UnknownShell => "Choose a shell with --shell bash, zsh or fish.",
        }
    }
}

/// Kettle's path as a launch can name it: absolute, UTF-8 and lasting.
fn kettle_path(kettle: &Path) -> Result<&str, SetupError> {
    kettle_ui::codex_shell::kettle_path(kettle).map_err(SetupError::from)
}

impl From<kettle_ui::codex_shell::FunctionError> for SetupError {
    fn from(error: kettle_ui::codex_shell::FunctionError) -> Self {
        match error {
            kettle_ui::codex_shell::FunctionError::UnnamedKettle => Self::UnnamedKettle,
            kettle_ui::codex_shell::FunctionError::Translocated => Self::Translocated,
        }
    }
}

/// The running Kettle's own path, links resolved.
fn this_kettle() -> Result<PathBuf, SetupError> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| SetupError::UnnamedKettle)
}

impl SetupShell {
    fn from_program(program: &Path) -> Option<Self> {
        match program.file_stem()?.to_str()? {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// `name` or else the shell `SHELL` names.
    fn chosen(name: Option<Self>) -> Result<Self, SetupError> {
        name.or_else(|| {
            std::env::var_os("SHELL").and_then(|program| Self::from_program(Path::new(&program)))
        })
        .ok_or(SetupError::UnknownShell)
    }
}

fn codex_shell(shell: SetupShell) -> kettle_ui::codex_shell::CodexShell {
    use kettle_ui::codex_shell::CodexShell;
    match shell {
        SetupShell::Bash => CodexShell::Bash,
        SetupShell::Zsh => CodexShell::Zsh,
        SetupShell::Fish => CodexShell::Fish,
    }
}

/// What `--print` writes for `agent` (`codex` or `claude`): the function
/// Kettle defines, handing its arguments to this Kettle as they are, a
/// wrapper of the same shape with a place for an account's environment, or
/// the entry script for a program that starts the agent itself. The shell
/// expands nothing in the arguments.
fn print_form(
    kettle: &Path,
    agent: SetupAgent,
    shell: Option<SetupShell>,
    form: SetupForm,
) -> Result<String, SetupError> {
    use kettle_ui::codex_shell as snippets;
    let name = match agent {
        SetupAgent::Claude => "claude",
        SetupAgent::Codex => "codex",
        SetupAgent::Agy => unreachable!("agy uses native plugin setup"),
    };
    Ok(match form {
        SetupForm::Entry => snippets::entry_script(kettle, name)?,
        SetupForm::Function | SetupForm::Wrapper => {
            let shell = codex_shell(SetupShell::chosen(shell)?);
            if form == SetupForm::Wrapper {
                snippets::wrapper_snippet(kettle, shell, name)?
            } else if agent == SetupAgent::Claude {
                snippets::claude_function(kettle, shell)?
            } else {
                snippets::codex_function(kettle, shell)?
            }
        }
    })
}

/// What the harnesses still write, which status and removal say rather than
/// promising nothing is written.
const HARNESS_WRITES: &str = "Codex and Claude Code still write their usual history and \
     session files, and Claude Code keeps data for a plugin it loads. Kettle writes none \
     of their settings or startup files.";

/// How to take the function out again, and what the harnesses still write.
fn uninstall_text(shell: SetupShell) -> String {
    let remove = match shell {
        SetupShell::Bash | SetupShell::Zsh => {
            "Remove the codex function from your shell's startup file, then run: unset -f codex"
        }
        SetupShell::Fish => {
            "Remove the codex function from your fish configuration, then run: functions --erase codex"
        }
    };
    format!(
        "{remove}\nIf Kettle defines it for you, turn off Settings, Agents, Codex previews \
         (agent-display-codex); new panes then start without it.\n{HARNESS_WRITES}"
    )
}

/// Whether the saved configuration has Kettle define `codex` in new panes'
/// shells: agent previews and `agent-display-codex` both on. It is read, never
/// repaired, so asking changes nothing.
fn automatic_integration() -> bool {
    kettle_config::Config::default_path()
        .filter(|path| path.exists())
        .map(|path| kettle_config::Config::load_from(&path))
        .is_some_and(|config| config.agent_display && config.agent_display_codex)
}

/// The status line for the automatic integration, which reports the saved
/// settings: a running Kettle may have been started otherwise.
fn automatic_status(on: bool) -> &'static str {
    if on {
        "on in your saved settings (agent-display-codex): zsh and fish started in a new \
         pane define codex, unless you define your own. A Kettle started with \
         --agent-display off or another configuration file follows that instead"
    } else {
        "off in your saved settings; add the function --print shows, or turn on \
         Settings, Agents, Codex previews"
    }
}

/// Why Kettle leaves the user's zsh alone, when it does: a system `zshenv`
/// that names `ZDOTDIR` or the `RCS` option. `shell` is the user's shell (`SHELL`).
fn zsh_status(shell: Option<&std::ffi::OsStr>) -> Option<String> {
    let shell = Path::new(shell?);
    if shell.file_name()? != "zsh" {
        return None;
    }
    let zshenv = kettle_core::shell_startup::zsh_left_alone(shell)?;
    Some(format!(
        "{} names ZDOTDIR or the RCS option, so Kettle leaves zsh's startup alone; \
         add the function --print zsh shows",
        zshenv.display()
    ))
}

/// The oldest Codex CLI whose launch options Kettle's are known to fit:
/// `--no-daemon` arrived by 0.159.0. A 1.x or later is not assumed to.
const OLDEST_CODEX_MINOR: u64 = 159;

/// The minor version `codex --version` printed, for a 0.x Codex CLI.
fn codex_minor(output: &str) -> Option<u64> {
    let version = output.trim().strip_prefix("codex-cli ")?;
    let parts: Vec<Option<u64>> = version.split('.').map(|part| part.parse().ok()).collect();
    match parts.as_slice() {
        [Some(0), Some(minor), Some(_)] => Some(*minor),
        _ => None,
    }
}

/// Whether `codex --version` printed a Codex CLI Kettle can add its options
/// to.
fn supported_version(output: &str) -> bool {
    codex_minor(output).is_some_and(|minor| minor >= OLDEST_CODEX_MINOR)
}

/// A tested hook layout on a platform with a harness admission path.
fn card_version(output: &str) -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
        && kettle_ctl::agent_launch::card_version(kettle_ctl::show::InlineTarget::CodexHook, output)
}

/// The first line `codex --version` prints, if it answers within three
/// seconds. The line is read apart from the wait, since a program that hands
/// its output to one that outlives it would hold a read open.
fn installed_version() -> Option<String> {
    version_of(Path::new("codex"))
}

pub(crate) fn version_of(program: &Path) -> Option<String> {
    use std::io::BufRead as _;
    use std::io::Read as _;
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, line) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let read = std::io::BufReader::new(stdout.take(256))
            .read_line(&mut output)
            .map(|_| output);
        let _ = sender.send(read);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let output = line
        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        .ok()?
        .ok()?;
    status.success().then_some(output)
}

/// Whether this shell runs in a Kettle pane: both variables Kettle sets
/// there, each a positive number.
fn in_kettle_pane(pid: Option<&OsStr>, pane: Option<&OsStr>) -> bool {
    let positive = |value: Option<&OsStr>| {
        value
            .and_then(OsStr::to_str)
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|value| value > 0)
    };
    positive(pid) && positive(pane)
}

fn this_shell_in_kettle() -> bool {
    in_kettle_pane(
        std::env::var_os("KETTLE_PID").as_deref(),
        std::env::var_os("KETTLE_PANE_ID").as_deref(),
    )
}

/// Replace this process with `codex` and `args`, or run it and pass on its
/// exit status where a process cannot be replaced.
fn run_codex(args: Vec<OsString>) -> i32 {
    run_program("codex", args)
}

fn run_program(program: &str, args: Vec<OsString>) -> i32 {
    let mut command = std::process::Command::new(program);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = command.exec();
        eprintln!("kettle agent-setup: cannot start {program}: {error}");
        127
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("kettle agent-setup: cannot start {program}: {error}");
                127
            }
        }
    }
}

/// How many values a Claude Code option takes, as its own parser reads them
/// (Claude Code 2.1.316).
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Arity {
    /// A switch.
    Switch,
    /// Exactly one value: `--flag value` or `--flag=value`.
    One,
    /// A value only if the next argument does not start with `-`.
    Optional,
    /// One or more values: every following argument that does not start with
    /// `-`. A prompt after one needs `--` first.
    Many,
}

/// Why a known option does not get a card.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Exclusion {
    /// Print mode and the options that only apply to it.
    Print,
    /// A background, remote or tmux session, not one in this pane.
    Detached,
    /// Turns off plugin hooks, so `kettle_card` would never run.
    HooksOff,
    /// Ignores every MCP server but its own, Kettle's plugin's included, so
    /// `kettle_show` would not exist (checked on Claude Code 2.1.317).
    McpOff,
    /// Help and version output.
    Info,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Takes(Arity),
    Excluded(Exclusion),
}

/// What Kettle does with a Claude Code option: `None` for one it does not
/// know. A flag that keeps Claude interactive in this pane with plugin hooks
/// on is `Takes`; the rest are `Excluded`, so that a new Claude option is a
/// decision made here rather than a silent fall-through.
#[cfg(any(target_os = "linux", test))]
fn claude_flag(option: &str) -> Option<Flag> {
    use Arity::{Many, One, Optional, Switch};
    Some(match option {
        "--continue"
        | "-c"
        | "--fork-session"
        | "--verbose"
        | "--dangerously-skip-permissions"
        | "--allow-dangerously-skip-permissions"
        | "--ide"
        | "--chrome"
        | "--no-chrome"
        | "--disable-slash-commands"
        | "--ax-screen-reader"
        | "--brief"
        | "--exclude-dynamic-system-prompt-sections" => Flag::Takes(Switch),
        "--model"
        | "--effort"
        | "--permission-mode"
        | "--agent"
        | "--agents"
        | "--name"
        | "-n"
        | "--session-id"
        | "--settings"
        | "--setting-sources"
        | "--system-prompt"
        | "--append-system-prompt"
        | "--plugin-dir"
        | "--plugin-url"
        | "--debug-file"
        | "--autocompact"
        | "--fallback-model"
        | "--system-prompt-snapshot"
        | "--remote-control-session-name-prefix" => Flag::Takes(One),
        "--resume"
        | "-r"
        | "--debug"
        | "-d"
        | "--worktree"
        | "-w"
        | "--remote-control"
        | "--from-pr"
        | "--prompt-suggestions" => Flag::Takes(Optional),
        "--add-dir" | "--allowedTools" | "--allowed-tools" | "--disallowedTools"
        | "--disallowed-tools" | "--mcp-config" | "--tools" | "--betas" | "--file" => {
            Flag::Takes(Many)
        }
        "-p"
        | "--print"
        | "--output-format"
        | "--input-format"
        | "--json-schema"
        | "--max-budget-usd"
        | "--include-partial-messages"
        | "--include-hook-events"
        | "--replay-user-messages"
        | "--forward-subagent-text"
        | "--permission-prompts"
        | "--no-session-persistence" => Flag::Excluded(Exclusion::Print),
        "--bg" | "--background" | "--cloud" | "--environment" | "--desktop" | "--teleport"
        | "--tmux" => Flag::Excluded(Exclusion::Detached),
        "--bare" | "--safe-mode" => Flag::Excluded(Exclusion::HooksOff),
        "--strict-mcp-config" => Flag::Excluded(Exclusion::McpOff),
        "-h" | "--help" | "-v" | "--version" => Flag::Excluded(Exclusion::Info),
        _ => return None,
    })
}

/// Claude Code's subcommands, with their aliases (2.1.316).
#[cfg(any(target_os = "linux", test))]
fn is_claude_subcommand(word: &str) -> bool {
    matches!(
        word,
        "agents"
            | "attach"
            | "auth"
            | "auto-mode"
            | "doctor"
            | "gateway"
            | "import"
            | "install"
            | "logs"
            | "mcp"
            | "plugin"
            | "plugins"
            | "purge"
            | "remote-control"
            | "respawn"
            | "rm"
            | "setup-token"
            | "stop"
            | "kill"
            | "ultrareview"
            | "update"
            | "upgrade"
    )
}

/// Why a launch is not given Kettle's cards.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Eq, PartialEq)]
enum Refusal {
    /// An option Kettle does not know. Holds its name when that is plain
    /// text safe to print, else nothing; never a value.
    UnknownOption(String),
    Excluded(Exclusion),
    /// A subcommand, not a session.
    Subcommand,
    /// An argument that is not Unicode.
    NotUnicode,
    /// An option that needs a value and has none.
    MissingValue,
    /// A switch given a value, as `--ide=x`.
    UnexpectedValue,
    /// More prompts than a session takes.
    ExtraPrompt,
}

#[cfg(any(target_os = "linux", test))]
impl Refusal {
    fn unknown_option(name: &str) -> Self {
        // Arguments are untrusted, and a prompt can start with `-`: name only
        // what looks like a long option, in the lower-case words options use.
        let plain = name.len() <= 40
            && name.strip_prefix("--").is_some_and(|word| {
                word.starts_with(|first: char| first.is_ascii_lowercase())
                    && word.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            });
        Self::UnknownOption(if plain {
            name.to_owned()
        } else {
            String::new()
        })
    }

    /// Why a user who started an interactive session gets no card, or `None`
    /// where the launch was never meant to have one: print mode, a
    /// subcommand, help, a detached session, or a malformed launch that
    /// Claude Code will itself report.
    fn why(&self) -> Option<String> {
        Some(match self {
            Self::UnknownOption(name) if name.is_empty() => {
                "Kettle does not know one of its options".to_owned()
            }
            Self::UnknownOption(name) => format!("Kettle does not know {name}"),
            Self::Excluded(Exclusion::HooksOff) => "this launch turns off plugin hooks".to_owned(),
            Self::Excluded(Exclusion::McpOff) => {
                "this launch ignores the MCP server Kettle's plugin provides".to_owned()
            }
            _ => return None,
        })
    }
}

/// The one line printed, before the agent starts, when a launch that should
/// have cards cannot. `why` is fixed text or a plain option name, never an
/// argument, path, version or environment value.
#[cfg(any(target_os = "linux", test))]
fn cardless_notice(agent: &str, why: &str) -> String {
    format!(
        "kettle agent-setup: starting {agent} without Kettle cards: {why}; \
         images and diagrams will go to the media shelf."
    )
}

/// Why a launch that should have cards cannot, for either agent.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Eq, PartialEq)]
enum NoCard {
    Refused(Refusal),
    /// `--version` printed nothing Kettle could read.
    NoVersion,
    /// A version that gets no card.
    UnsupportedVersion,
    /// Kettle could not hold and check the executable it would start.
    NotNative,
    /// No display server answered.
    ServerUnavailable,
    /// The display server did not accept the launch, as when previews are off.
    NotRegistered,
}

#[cfg(any(target_os = "linux", test))]
impl NoCard {
    /// The notice for `agent`, or `None` where the launch was not meant to
    /// have a card.
    fn notice(&self, agent: &str) -> Option<String> {
        let why = match self {
            Self::Refused(refusal) => refusal.why()?,
            Self::NoVersion => "Kettle could not read its version".to_owned(),
            Self::UnsupportedVersion => "Kettle gives no cards to this version".to_owned(),
            Self::NotNative => "Kettle could not check the executable it would start".to_owned(),
            Self::ServerUnavailable => "Kettle's display server did not answer".to_owned(),
            Self::NotRegistered => {
                "Kettle declined the launch; check that agent previews and this agent's \
                 previews are on in Settings"
                    .to_owned()
            }
        };
        Some(cardless_notice(agent, &why))
    }
}

/// Whether `arg` reads as an option to Claude Code's parser: a lone `-` and
/// the empty string are values.
#[cfg(any(target_os = "linux", test))]
fn claude_option_like(arg: &str) -> bool {
    arg.len() > 1 && arg.starts_with('-')
}

/// The value at `index` if it is one: present, Unicode and not an option.
#[cfg(any(target_os = "linux", test))]
fn claude_value(args: &[OsString], index: usize) -> Result<Option<&str>, Refusal> {
    match args.get(index).map(|value| value.to_str()) {
        None => Ok(None),
        Some(None) => Err(Refusal::NotUnicode),
        Some(Some(value)) => Ok((!claude_option_like(value)).then_some(value)),
    }
}

/// The first option after an unknown one that shows the launch is not an
/// interactive session in this pane (print mode, a detached session, help or
/// version), so that such a launch is never told it lost its cards.
#[cfg(any(target_os = "linux", test))]
fn claude_exclusion_after(args: &[OsString]) -> Option<Exclusion> {
    args.iter()
        .map_while(|arg| arg.to_str().filter(|arg| *arg != "--"))
        .filter(|arg| claude_option_like(arg))
        .find_map(|arg| match claude_flag(arg.split('=').next()?) {
            Some(Flag::Excluded(
                why @ (Exclusion::Print | Exclusion::Detached | Exclusion::Info),
            )) => Some(why),
            _ => None,
        })
}

/// Whether `args` start an interactive Claude Code session in this pane, with
/// plugin hooks on, and so can have a card scope; otherwise why not. Decided
/// from the arguments alone, in one pass, reading values as Claude Code does:
/// an empty string is a value, and a multi-value option takes every argument
/// up to the next option.
#[cfg(any(target_os = "linux", test))]
fn claude_interactive(args: &[OsString]) -> Result<(), Refusal> {
    let mut prompt = false;
    let mut switched_off = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let arg = arg.to_str().ok_or(Refusal::NotUnicode)?;
        index += 1;
        if arg == "--" {
            let rest = &args[index..];
            if rest.iter().any(|value| value.to_str().is_none()) {
                return Err(Refusal::NotUnicode);
            }
            return if usize::from(prompt) + rest.len() <= 1 {
                switched_off.map_or(Ok(()), |why| Err(Refusal::Excluded(why)))
            } else {
                Err(Refusal::ExtraPrompt)
            };
        }
        if claude_option_like(arg) {
            let (name, inline) = match arg.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value)),
                _ => (arg, None),
            };
            let arity = match claude_flag(name) {
                None => {
                    return Err(claude_exclusion_after(&args[index..])
                        .map_or_else(|| Refusal::unknown_option(name), Refusal::Excluded));
                }
                // A hooks-off or strict-MCP flag only matters if the launch is
                // otherwise an interactive session: keep reading, so print
                // mode, help, a subcommand or a malformed launch is what the
                // user hears about, or nothing.
                Some(Flag::Excluded(why @ (Exclusion::HooksOff | Exclusion::McpOff))) => {
                    switched_off.get_or_insert(why);
                    continue;
                }
                Some(Flag::Excluded(why)) => return Err(Refusal::Excluded(why)),
                Some(Flag::Takes(arity)) => arity,
            };
            match (arity, inline) {
                (Arity::Switch, Some(_)) => return Err(Refusal::UnexpectedValue),
                (Arity::Switch, None) | (_, Some(_)) => {}
                (Arity::One, None) => {
                    if claude_value(args, index)?.is_none() {
                        return Err(Refusal::MissingValue);
                    }
                    index += 1;
                }
                (Arity::Optional, None) => {
                    if claude_value(args, index)?.is_some() {
                        index += 1;
                    }
                }
                (Arity::Many, None) => {
                    if claude_value(args, index)?.is_none() {
                        return Err(Refusal::MissingValue);
                    }
                    while claude_value(args, index)?.is_some() {
                        index += 1;
                    }
                }
            }
            continue;
        }
        if is_claude_subcommand(arg) {
            return Err(Refusal::Subcommand);
        }
        if prompt {
            return Err(Refusal::ExtraPrompt);
        }
        prompt = true;
    }
    switched_off.map_or(Ok(()), |why| Err(Refusal::Excluded(why)))
}

fn launch_claude(argv: Vec<OsString>) -> i32 {
    #[cfg(target_os = "linux")]
    {
        use std::io::IsTerminal as _;
        let plugin = kettle_ui::claude_status(|name| std::env::var_os(name));
        if this_shell_in_kettle()
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && plugin.in_pane
            && !plugin.forbidden
        {
            let launch = claude_interactive(&argv)
                .map_err(NoCard::Refused)
                .and_then(|()| {
                    let version = version_of(Path::new("claude")).ok_or(NoCard::NoVersion)?;
                    linux::prepare(
                        "claude",
                        kettle_ctl::show::InlineTarget::ClaudeHook,
                        &version,
                    )
                })
                // The held executable stays alive with its command: the command
                // names it by descriptor, which closing it would take away.
                .and_then(|native| {
                    native
                        .command("claude")
                        .map(|command| (native, command))
                        .ok_or(NoCard::NotNative)
                });
            match launch {
                Ok((_native, mut command)) => {
                    use std::os::unix::process::CommandExt as _;
                    let error = command.args(argv).exec();
                    eprintln!("kettle agent-setup: cannot start claude: {error}");
                    return 127;
                }
                Err(no_card) => {
                    if let Some(notice) = no_card.notice("Claude Code") {
                        eprintln!("{notice}");
                    }
                }
            }
        }
    }
    run_program("claude", argv)
}

/// The function and its launcher are Unix-only for now: on Windows Codex
/// installs as a `.cmd` launcher that a process cannot start by name.
#[cfg(not(unix))]
pub(crate) fn run(_: SetupArgs) -> i32 {
    eprintln!("kettle agent-setup: not available on this system yet.");
    2
}

#[cfg(unix)]
pub(crate) fn run(args: SetupArgs) -> i32 {
    if let Some(action) = args.agy_hook.as_deref() {
        return crate::agy::hook(action);
    }
    if args.agent == SetupAgent::Agy {
        if args.shell.is_some() {
            eprintln!("agy uses a native plugin, not a shell function; omit --shell");
            return 2;
        }
        if args.form.is_some() {
            eprintln!("agy uses a native plugin, not a shell function; omit --form");
            return 2;
        }
        return crate::agy::setup(args.print, args.install, args.uninstall);
    }
    if args.install {
        eprintln!("--install requires --agent agy");
        return 2;
    }
    if args.launch_codex {
        return launch(args.argv);
    }
    if args.launch_claude {
        return launch_claude(args.argv);
    }
    if args.status {
        print_status();
        return 0;
    }
    if !(args.print || args.uninstall) {
        eprintln!("kettle agent-setup: choose --print, --status or --uninstall.");
        return 2;
    }
    if args.form == Some(SetupForm::Entry) && args.shell.is_some() {
        eprintln!("kettle agent-setup: an entry script is a POSIX sh script; omit --shell.");
        return 2;
    }
    if args.uninstall {
        let shell = match SetupShell::chosen(args.shell) {
            Ok(shell) => shell,
            Err(error) => {
                eprintln!("kettle agent-setup: {}", error.message());
                return 2;
            }
        };
        if args.agent == SetupAgent::Claude {
            let command = match shell {
                SetupShell::Bash => "unset -f claude",
                SetupShell::Zsh => "unfunction claude",
                SetupShell::Fish => "functions -e claude",
            };
            println!(
                "Remove the claude function from your shell's startup file, then run `{command}` or open a new shell. Turn off Claude Code previews in Settings to stop offering it to new panes.\n{HARNESS_WRITES}"
            );
        } else {
            println!("{}", uninstall_text(shell));
        }
        return 0;
    }
    match this_kettle().and_then(|kettle| {
        print_form(
            &kettle,
            args.agent,
            args.shell,
            args.form.unwrap_or_default(),
        )
    }) {
        Ok(function) => {
            print!("{function}");
            0
        }
        Err(error) => {
            eprintln!("kettle agent-setup: {}", error.message());
            2
        }
    }
}

/// Start Codex: with Kettle's display server for an interactive session in
/// a Kettle pane under a Codex CLI Kettle knows, otherwise as asked.
fn launch(argv: Vec<OsString>) -> i32 {
    if !this_shell_in_kettle() || classify(&argv).session == Session::PassThrough {
        return run_codex(argv);
    }
    let Some(version) = installed_version().filter(|version| supported_version(version)) else {
        eprintln!(
            "kettle agent-setup: this Codex CLI is not one Kettle knows; starting it without Kettle's display server."
        );
        return run_codex(argv);
    };
    let cards = card_version(&version) && !classify(&argv).own_hooks;
    #[cfg(target_os = "linux")]
    let cards = {
        use std::io::IsTerminal as _;
        cards && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    };
    #[cfg(target_os = "linux")]
    let native = if cards {
        match linux::prepare("codex", kettle_ctl::show::InlineTarget::CodexHook, &version) {
            Ok(native) => Some(native),
            Err(no_card) => {
                if let Some(notice) = no_card.notice("Codex") {
                    eprintln!("{notice}");
                }
                None
            }
        }
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let cards = native.is_some();
    match this_kettle().and_then(|kettle| launch_args(&argv, &kettle, cards)) {
        Ok(launch) => {
            #[cfg(target_os = "linux")]
            if let Some(native) = native {
                use std::os::unix::process::CommandExt as _;
                let Some(mut command) = native.command("codex") else {
                    return run_codex(argv);
                };
                let error = command.args(launch).exec();
                eprintln!("kettle agent-setup: cannot start codex: {error}");
                return 127;
            }
            run_codex(launch)
        }
        Err(error) => {
            eprintln!(
                "kettle agent-setup: {} Starting Codex without Kettle's display server.",
                error.message()
            );
            run_codex(argv)
        }
    }
}

/// Whether the Kettle `client` reached shows media, asked without showing
/// anything: an empty `show` is refused as malformed where agent previews
/// are on, and as disabled where they are off.
fn previews_on(client: &mut kettle_ctl::Client) -> Result<bool, kettle_ctl::CtlError> {
    use kettle_ctl::CtlError;
    let probe = client.call_with_timeout("show", serde_json::json!({}), Duration::from_secs(3));
    match probe {
        Err(CtlError::Server { code, .. }) if code == "bad_params" => Ok(true),
        Err(CtlError::Server { code, .. }) if code == "display_disabled" => Ok(false),
        Err(error) => Err(error),
        Ok(_) => Err(CtlError::Protocol("an empty show was accepted".into())),
    }
}

/// The status line for the Kettle this shell runs in, found the strict way
/// `kettle show` finds it: whether its agent previews are on, or why it was
/// not reached.
fn kettle_status(found: Result<bool, &kettle_ctl::CtlError>) -> &'static str {
    use kettle_ctl::CtlError;
    match found {
        Ok(true) => "this shell reaches the Kettle it runs in, whose agent previews are on",
        Ok(false) => {
            "this shell reaches the Kettle it runs in, but its agent previews are off; turn \
             them on in Settings, Agents, Agent previews"
        }
        Err(CtlError::NotInKettle) => {
            "this shell is not in a Kettle whose agent previews are on, so nothing it starts \
             can show media there"
        }
        Err(CtlError::NoServer | CtlError::Io(_)) => {
            "no Kettle with agent previews on answered; turn them on in Settings, Agents, \
             Agent previews"
        }
        Err(_) => "the Kettle this shell runs in answered in a form this command does not know",
    }
}

/// The status line for Claude Code: whether this pane started with Kettle's
/// plugin and, when it did not, what is known of why. `saved` is the saved
/// setting and `previews` whether the Kettle this shell reaches has its
/// agent previews on, when one answered: a Kettle started with them off
/// offers new panes nothing, whatever is saved. A policy managed remotely
/// is not visible to Kettle.
fn claude_status_text(
    status: kettle_ui::ClaudeStatus,
    saved: bool,
    previews: Option<bool>,
) -> &'static str {
    if !status.in_pane && !status.offerable {
        return "this pane started without Kettle's plugin, and this Kettle cannot offer it: \
                it runs from a translocated copy or a path the plugin cannot name; run \
                Kettle from /Applications";
    }
    match (status.in_pane, status.forbidden, saved) {
        (true, _, _) => "this pane started with Kettle's plugin offered to Claude Code",
        (false, true, _) => {
            "this pane started without Kettle's plugin: Claude Code's managed policy on this \
             machine forbids plugins from the environment, or Kettle could not read all of it"
        }
        (false, false, true) if previews == Some(false) => {
            "this pane started without Kettle's plugin; this Kettle's agent previews are off, \
             so new panes do not get it until they are on (Settings, Agents, Agent previews)"
        }
        (false, false, true) => {
            "this pane started without Kettle's plugin; new panes get it (Claude Code \
             previews is on in your saved settings), unless a policy your organization \
             manages remotely, which Kettle cannot see, forbids it"
        }
        (false, false, false) => {
            "this pane started without Kettle's plugin; turn on Settings, Agents, Claude Code \
             previews (agent-display-claude-code) for new panes"
        }
    }
}

/// Whether the saved configuration offers Kettle's plugin to Claude Code in
/// new panes. It is read, never repaired.
fn claude_integration() -> bool {
    kettle_config::Config::default_path()
        .filter(|path| path.exists())
        .map(|path| kettle_config::Config::load_from(&path))
        .is_some_and(|config| config.agent_display && config.agent_display_claude_code)
}

/// What `--status` says about the `name` function the pane's shell has, from
/// the hint its startup left. The hint is forgeable, so the wording stays
/// hedged and nothing but a fixed line is printed.
fn launch_line(name: &str, marker: kettle_ui::codex_shell::AgentFunction) -> String {
    use kettle_ui::codex_shell::AgentFunction;
    if name == "claude" && cfg!(target_os = "macos") {
        return match marker {
            AgentFunction::Kettle => format!("Kettle's {name} function starts it, through agent-setup --launch-{name}"),
            AgentFunction::Own => "your own claude function runs first; macOS checks Claude's signature for cards".into(),
            AgentFunction::Unchecked => "not checked (only zsh and fish panes Kettle started are, and an alias is not); macOS checks Claude's signature for cards".into(),
        };
    }
    let fix = format!(
        "kettle agent-setup --print --agent {name} --form wrapper prints one that ends in \
         Kettle's launcher"
    );
    match marker {
        AgentFunction::Kettle => {
            format!("Kettle's {name} function starts it, through agent-setup --launch-{name}")
        }
        AgentFunction::Own => format!(
            "your own {name} function runs first; it gets cards only if it ends in \
             Kettle's launcher; {fix}"
        ),
        AgentFunction::Unchecked => format!(
            "not checked (only zsh and fish panes Kettle started are, and an alias is not); a \
             function of your own gets cards only if it ends in Kettle's launcher; {fix}"
        ),
    }
}

/// The hint the shell that ran this command left for `name`. The hint names
/// the shell that set it, so a shell started inside that one, which inherits
/// the variable, reads as not checked.
fn this_shells_marker(name: &str) -> kettle_ui::codex_shell::AgentFunction {
    #[cfg(unix)]
    let shell = std::os::unix::process::parent_id();
    #[cfg(not(unix))]
    let shell = 0;
    kettle_ui::codex_shell::agent_function_marker(&|key| std::env::var_os(key), name, shell)
}

fn print_status() {
    let found = crate::ctl_discovery::discover_display(None)
        .and_then(|mut client| previews_on(&mut client));
    println!("Kettle: {}", kettle_status(found.as_ref().copied()));
    let version = installed_version().map(|version| version.trim().to_owned());
    let codex = match &version {
        None => "not found".to_owned(),
        Some(version) if supported_version(version) => format!("{version}, supported"),
        Some(version) => format!("{version}, not one Kettle knows"),
    };
    println!("Codex: {codex}");
    println!(
        "This shell: {}",
        if this_shell_in_kettle() {
            "in a Kettle pane"
        } else {
            "not in a Kettle pane, so Codex starts without Kettle's display server"
        }
    );
    let own_codex = this_shell_in_kettle()
        && this_shells_marker("codex") == kettle_ui::codex_shell::AgentFunction::Own;
    let launched = this_shell_in_kettle() && version.as_deref().is_some_and(supported_version);
    let cards = launched && version.as_deref().is_some_and(card_version);
    println!(
        "Delivery: {}",
        if own_codex {
            "none unless your own codex function ends in Kettle's launcher (see Codex launch)"
        } else if cards {
            "a supported card-hook release line; interactive launches get a card under the call \
             after platform checks and Codex's hook review, otherwise the pane's shelf"
        } else if launched {
            "the media shelf of the pane Codex runs in; cards under the call need \
             a supported Codex CLI 0.162 or newer 0.x launch on macOS or Linux"
        } else {
            "none from this shell"
        }
    );
    if this_shell_in_kettle() {
        println!(
            "Codex launch: {}",
            launch_line("codex", this_shells_marker("codex"))
        );
    }
    println!("Needs: agent previews on in Kettle (Settings, Agents, Agent previews)");
    let automatic = automatic_integration();
    println!("Automatic: {}", automatic_status(automatic));
    if let Some(zsh) = automatic
        .then(|| zsh_status(std::env::var_os("SHELL").as_deref()))
        .flatten()
    {
        println!("zsh: {zsh}");
    }
    let claude = kettle_ui::claude_status(|name| std::env::var_os(name));
    println!(
        "Claude Code: {}",
        claude_status_text(claude, claude_integration(), found.as_ref().ok().copied())
    );
    if this_shell_in_kettle() {
        println!(
            "Claude Code launch: {}",
            launch_line("claude", this_shells_marker("claude"))
        );
    }
    println!(
        "Interactive sessions get Kettle's display server for that launch only. Codex's \
         configuration and your shell's startup files are left as they are."
    );
    println!("{HARNESS_WRITES}");
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Refusal {
        /// The line printed for it, for Claude Code.
        fn notice(&self) -> Option<String> {
            self.why().map(|why| cardless_notice("Claude Code", &why))
        }
    }

    // Captured from Claude Code 2.1.317 --help, including aliases and placeholders.
    const HELP_OPTIONS: &str = r"--add-dir <directories...>
--agent <agent>
--agents <json-or-file>
--allow-dangerously-skip-permissions
--allowedTools, --allowed-tools <tools...>
--append-system-prompt <prompt>
--autocompact <auto|tokens>
--ax-screen-reader
--bg, --background
--bare
--betas <betas...>
--brief
--chrome
--cloud [description|session_id|url]
-c, --continue
--dangerously-skip-permissions
-d, --debug [filter]
--debug-file <path>
--desktop
--disable-slash-commands
--disallowedTools, --disallowed-tools <tools...>
--effort <level>
--environment <environment_id>
--exclude-dynamic-system-prompt-sections
--fallback-model <model>
--file <specs...>
--fork-session
--forward-subagent-text
--from-pr [value]
-h, --help
--ide
--include-hook-events
--include-partial-messages
--input-format <format>
--json-schema <schema>
--max-budget-usd <amount>
--mcp-config <configs...>
--model <model>
-n, --name <name>
--no-chrome
--no-session-persistence
--output-format <format>
--permission-mode <mode>
--permission-prompts <target>
--plugin-dir <path>
--plugin-url <url>
-p, --print
--prompt-suggestions [value]
--remote-control [name]
--remote-control-session-name-prefix <prefix>
--replay-user-messages
--restricted
-r, --resume [value]
--safe-mode
--session-id <uuid>
--setting-sources <sources>
--settings <file-or-json>
--strict-mcp-config
--system-prompt <prompt>
--system-prompt-snapshot <on|off>
--teleport [session]
--tmux
--tools <tools...>
--verbose
-v, --version
-w, --worktree [name]
";

    const HELP_COMMANDS: &str = r"agents [options]
attach <id|name>
auth
auto-mode
doctor
gateway [options]
import [options] [source]
install [options] [target]
logs <id|name>
mcp
plugin|plugins
purge [options] [path]
respawn [options] [id]
rm <id>
setup-token
stop|kill <id>
ultrareview [options] [target]
update|upgrade
";

    const EXCLUDED: &[(&str, Exclusion)] = &[
        ("-p", Exclusion::Print),
        ("--print", Exclusion::Print),
        ("--output-format", Exclusion::Print),
        ("--input-format", Exclusion::Print),
        ("--json-schema", Exclusion::Print),
        ("--max-budget-usd", Exclusion::Print),
        ("--include-partial-messages", Exclusion::Print),
        ("--include-hook-events", Exclusion::Print),
        ("--replay-user-messages", Exclusion::Print),
        ("--forward-subagent-text", Exclusion::Print),
        ("--permission-prompts", Exclusion::Print),
        ("--no-session-persistence", Exclusion::Print),
        ("--bg", Exclusion::Detached),
        ("--background", Exclusion::Detached),
        ("--cloud", Exclusion::Detached),
        ("--environment", Exclusion::Detached),
        ("--desktop", Exclusion::Detached),
        ("--teleport", Exclusion::Detached),
        ("--tmux", Exclusion::Detached),
        ("--bare", Exclusion::HooksOff),
        ("--safe-mode", Exclusion::HooksOff),
        ("--strict-mcp-config", Exclusion::McpOff),
        ("-h", Exclusion::Info),
        ("--help", Exclusion::Info),
        ("-v", Exclusion::Info),
        ("--version", Exclusion::Info),
    ];

    // Not yet allowed; see issue #759.
    const UNDECIDED_FOR_NOW: &[&str] = &["--restricted"];

    #[test]
    fn no_value_claude_flags_are_admitted() {
        for flag in [
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "--ide",
            "--chrome",
            "--no-chrome",
            "--disable-slash-commands",
            "--ax-screen-reader",
            "--brief",
            "--exclude-dynamic-system-prompt-sections",
            "--continue",
            "-c",
            "--fork-session",
            "--verbose",
        ] {
            for args in [
                vec![flag],
                vec![flag, "hello"],
                vec![flag, "--model", "opus"],
                vec!["--model", "opus", flag, "hello"],
            ] {
                assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
            }
        }
        let args = ["--ide=x"];
        assert_eq!(
            claude_interactive(&argv(&args)),
            Err(Refusal::UnexpectedValue),
            "{args:?}"
        );
    }

    #[test]
    fn one_value_claude_flags() {
        for flag in [
            "--plugin-dir",
            "--plugin-url",
            "--debug-file",
            "--autocompact",
            "--fallback-model",
            "--system-prompt-snapshot",
            "--remote-control-session-name-prefix",
            "--model",
            "--effort",
            "--permission-mode",
            "--agent",
            "--agents",
            "--name",
            "-n",
            "--session-id",
            "--settings",
            "--setting-sources",
            "--system-prompt",
            "--append-system-prompt",
        ] {
            for args in [
                vec![flag, "value"],
                vec![flag, "value", "hello"],
                vec![flag, ""],
                vec![flag, "", "hello"],
                vec![flag, "-"],
                vec![flag, "-", "hello"],
            ] {
                assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
            }
            for args in [vec![flag], vec![flag, "-p"], vec![flag, "--ide"]] {
                assert_eq!(
                    claude_interactive(&argv(&args)),
                    Err(Refusal::MissingValue),
                    "{args:?}"
                );
            }
            // Only long names support the --flag=value spelling.
            if flag.starts_with("--") {
                let inline = format!("{flag}=value");
                for args in [vec![inline.as_str()], vec![inline.as_str(), "hello"]] {
                    assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
                }
                let inline = format!("{flag}=");
                for args in [vec![inline.as_str()], vec![inline.as_str(), "hello"]] {
                    assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
                }
            }
        }
        for args in [
            vec!["--plugin-dir", "a", "--plugin-dir", "b"],
            vec!["--plugin-dir", "a", "--plugin-dir", "b", "hello"],
        ] {
            assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
        }
    }

    #[test]
    fn optional_value_claude_flags() {
        for flag in [
            "-d",
            "--debug",
            "-w",
            "--worktree",
            "--remote-control",
            "--from-pr",
            "--prompt-suggestions",
            "-r",
            "--resume",
        ] {
            for args in [
                vec![flag],
                vec![flag, "value"],
                vec![flag, "value", "hello"],
                vec![flag, "-c"],
                vec![flag, "--model", "opus"],
                vec![flag, ""],
                vec![flag, "", "hello"],
                vec![flag, "-"],
                vec![flag, "-", "hello"],
            ] {
                assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
            }
            let args = [flag, "", "hello", "world"];
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::ExtraPrompt),
                "{args:?}"
            );
            if flag.starts_with("--") {
                let inline = format!("{flag}=value");
                for args in [vec![inline.as_str()], vec![inline.as_str(), "hello"]] {
                    assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
                }
                let inline = format!("{flag}=");
                for args in [vec![inline.as_str()], vec![inline.as_str(), "hello"]] {
                    assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
                }
            }
        }
        // Both have a worktree value followed by one prompt; only a third
        // positional argument is an extra prompt.
        for args in [
            ["--worktree", "name", "hello"],
            ["--worktree", "hello", "world"],
        ] {
            assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
        }
        let args = ["--worktree", "hello", "world", "extra"];
        assert_eq!(
            claude_interactive(&argv(&args)),
            Err(Refusal::ExtraPrompt),
            "{args:?}"
        );
        let args = ["--worktree", "--tmux"];
        assert_eq!(
            claude_interactive(&argv(&args)),
            Err(Refusal::Excluded(Exclusion::Detached)),
            "{args:?}"
        );
    }

    #[test]
    fn variadic_claude_flags() {
        for flag in [
            "--add-dir",
            "--allowedTools",
            "--allowed-tools",
            "--disallowedTools",
            "--disallowed-tools",
            "--mcp-config",
            "--tools",
            "--betas",
            "--file",
        ] {
            for args in [
                vec![flag, "a", "b"],
                vec![flag, "a", "b", "--", "prompt"],
                vec![flag, "a", "b", "prompt"],
                vec![flag, "a", "mcp", "list"],
                vec![flag, "a.json", "--ide"],
                vec![flag, ""],
                vec![flag, "", "--", "prompt"],
                vec![flag, "", "one", "two"],
                vec![flag, "-"],
                vec![flag, "-", "--", "prompt"],
            ] {
                assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
            }
            for args in [vec![flag], vec![flag, "-c"]] {
                assert_eq!(
                    claude_interactive(&argv(&args)),
                    Err(Refusal::MissingValue),
                    "{args:?}"
                );
            }
            let args = [flag, "a", "b", "--", "one", "two"];
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::ExtraPrompt),
                "{args:?}"
            );
            let inline = format!("{flag}=");
            for args in [vec![inline.as_str()], vec![inline.as_str(), "hello"]] {
                assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
            }
            // An inline value ends this option's value run.
            let inline = format!("{flag}=a");
            for args in [vec![inline.as_str()], vec![inline.as_str(), "b"]] {
                assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
            }
            let args = [inline.as_str(), "b", "c"];
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::ExtraPrompt),
                "{args:?}"
            );
        }
    }

    #[test]
    fn refused_claude_groups() {
        for &(flag, reason) in EXCLUDED {
            let args = [flag];
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::Excluded(reason)),
                "{args:?}"
            );
        }
        for args in [
            ["--output-format=json"],
            ["--input-format=stream-json"],
            ["--json-schema={}"],
            ["--max-budget-usd=1"],
            ["--permission-prompts=host"],
            ["--print=hello"],
        ] {
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::Excluded(Exclusion::Print)),
                "{args:?}"
            );
        }
        let args = ["--tmux=classic"];
        assert_eq!(
            claude_interactive(&argv(&args)),
            Err(Refusal::Excluded(Exclusion::Detached)),
            "{args:?}"
        );
        for subcommand in [
            "agents",
            "attach",
            "auth",
            "auto-mode",
            "doctor",
            "gateway",
            "import",
            "install",
            "logs",
            "mcp",
            "plugin",
            "plugins",
            "purge",
            "remote-control",
            "respawn",
            "rm",
            "setup-token",
            "stop",
            "kill",
            "ultrareview",
            "update",
            "upgrade",
        ] {
            for args in [vec![subcommand], vec!["--continue", subcommand]] {
                assert_eq!(
                    claude_interactive(&argv(&args)),
                    Err(Refusal::Subcommand),
                    "{args:?}"
                );
            }
        }
        for (flag, name) in [
            ("--unknown", "--unknown"),
            ("--restricted", "--restricted"),
            ("-x", ""),
        ] {
            let args = [flag];
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::UnknownOption(name.to_owned())),
                "{args:?}"
            );
        }
        let args = ["one", "two"];
        assert_eq!(
            claude_interactive(&argv(&args)),
            Err(Refusal::ExtraPrompt),
            "{args:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            for prefix in [vec![], vec!["--"], vec!["--model"], vec!["--add-dir", "a"]] {
                let mut args = argv(&prefix);
                args.push(OsString::from_vec(vec![0xff]));
                assert_eq!(
                    claude_interactive(&args),
                    Err(Refusal::NotUnicode),
                    "{args:?}"
                );
            }
        }
    }

    #[test]
    fn double_dash_claude() {
        for args in [
            vec![],
            vec!["hello"],
            vec!["-"],
            vec!["--", "-"],
            vec!["--"],
            vec!["--", "-p"],
            vec!["--", "hello"],
            vec!["--model", "opus", "--", "hello"],
        ] {
            assert_eq!(claude_interactive(&argv(&args)), Ok(()), "{args:?}");
        }
        for args in [vec!["hello", "--", "world"], vec!["-", "hello"]] {
            assert_eq!(
                claude_interactive(&argv(&args)),
                Err(Refusal::ExtraPrompt),
                "{args:?}"
            );
        }
    }

    #[test]
    fn hooks_off_and_strict_mcp_flags_defer_to_every_other_refusal() {
        for (args, expected) in [
            (
                vec!["--bare", "-p", "q"],
                Refusal::Excluded(Exclusion::Print),
            ),
            (vec!["-p", "--bare"], Refusal::Excluded(Exclusion::Print)),
            (
                vec!["--safe-mode", "--print"],
                Refusal::Excluded(Exclusion::Print),
            ),
            (
                vec!["--strict-mcp-config", "--help"],
                Refusal::Excluded(Exclusion::Info),
            ),
            (
                vec!["--bare", "--bg"],
                Refusal::Excluded(Exclusion::Detached),
            ),
            (vec!["--bare", "update"], Refusal::Subcommand),
            (
                vec!["--strict-mcp-config", "mcp", "list"],
                Refusal::Subcommand,
            ),
            (vec!["--bare", "a", "b"], Refusal::ExtraPrompt),
            (vec!["--bare", "--model"], Refusal::MissingValue),
            (vec!["--bare", "--ide=x"], Refusal::UnexpectedValue),
            (vec!["--bare"], Refusal::Excluded(Exclusion::HooksOff)),
            (
                vec!["--bare", "hello"],
                Refusal::Excluded(Exclusion::HooksOff),
            ),
            (
                vec!["--bare", "--", "hello"],
                Refusal::Excluded(Exclusion::HooksOff),
            ),
            (
                vec!["--model", "opus", "--safe-mode", "--", "-p"],
                Refusal::Excluded(Exclusion::HooksOff),
            ),
            (
                vec!["--strict-mcp-config", "--", "-p"],
                Refusal::Excluded(Exclusion::McpOff),
            ),
            (vec!["--bare", "--", "a", "b"], Refusal::ExtraPrompt),
        ] {
            assert_eq!(claude_interactive(&argv(&args)), Err(expected), "{args:?}");
        }
    }

    #[test]
    fn claude_unknown_option_defers_to_noninteractive_exclusions() {
        for &(flag, reason) in EXCLUDED {
            if matches!(
                reason,
                Exclusion::Print | Exclusion::Detached | Exclusion::Info
            ) {
                let args = ["--newflag", flag];
                let refusal = claude_interactive(&argv(&args)).unwrap_err();
                assert_eq!(refusal, Refusal::Excluded(reason), "{args:?}");
                assert_eq!(refusal.notice(), None, "{args:?}");
            }
        }
        for (args, expected) in [
            (
                vec!["--newflag", "-p", "q"],
                Refusal::Excluded(Exclusion::Print),
            ),
            (
                vec!["--newflag", "--output-format=json"],
                Refusal::Excluded(Exclusion::Print),
            ),
            (
                vec!["--newflag", "--bare"],
                Refusal::UnknownOption("--newflag".to_owned()),
            ),
            (
                vec!["--newflag", "--strict-mcp-config"],
                Refusal::UnknownOption("--newflag".to_owned()),
            ),
            (
                vec!["--newflag", "--", "-p"],
                Refusal::UnknownOption("--newflag".to_owned()),
            ),
        ] {
            let refusal = claude_interactive(&argv(&args)).unwrap_err();
            assert_eq!(refusal, expected, "{args:?}");
            assert_eq!(
                refusal.notice().is_some(),
                matches!(refusal, Refusal::UnknownOption(_)),
                "{args:?}"
            );
        }
    }

    #[test]
    fn claude_no_card_notice_policy() {
        for (args, refusal, has_notice) in [
            (
                vec!["--foo"],
                Refusal::UnknownOption("--foo".to_owned()),
                true,
            ),
            (vec!["--bare"], Refusal::Excluded(Exclusion::HooksOff), true),
            (
                vec!["--strict-mcp-config"],
                Refusal::Excluded(Exclusion::McpOff),
                true,
            ),
            (vec!["--ide=x"], Refusal::UnexpectedValue, false),
            (vec!["--print"], Refusal::Excluded(Exclusion::Print), false),
            (vec!["--bg"], Refusal::Excluded(Exclusion::Detached), false),
            (vec!["--help"], Refusal::Excluded(Exclusion::Info), false),
            (vec!["mcp"], Refusal::Subcommand, false),
            (vec!["--model"], Refusal::MissingValue, false),
            (vec!["one", "two"], Refusal::ExtraPrompt, false),
        ] {
            assert_eq!(claude_interactive(&argv(&args)), Err(refusal), "{args:?}");
            let refusal = claude_interactive(&argv(&args)).unwrap_err();
            let notice = refusal.notice();
            assert_eq!(notice.is_some(), has_notice, "{args:?}");
            if let Some(notice) = notice {
                assert!(
                    notice.starts_with(
                        "kettle agent-setup: starting Claude Code without Kettle cards:"
                    ),
                    "{args:?}"
                );
                assert!(notice.contains("media shelf"), "{args:?}");
                if args == ["--strict-mcp-config"] {
                    assert!(
                        notice.contains(
                            "this launch ignores the MCP server Kettle's plugin provides"
                        ),
                        "{args:?}: {notice}"
                    );
                }
            }
        }
        let args = ["non-Unicode argument"];
        assert_eq!(Refusal::NotUnicode.notice(), None, "{args:?}");
        let plain = format!("--{}", "a".repeat(38));
        for name in ["--foo", "--some-new-flag2", plain.as_str()] {
            let args = [name];
            assert_eq!(
                Refusal::unknown_option(name),
                Refusal::UnknownOption(name.to_owned()),
                "{args:?}"
            );
            assert!(
                Refusal::unknown_option(name)
                    .notice()
                    .unwrap()
                    .contains(name),
                "{args:?}"
            );
        }
        let long = format!("--{}", "a".repeat(39));
        for name in [
            "-x",
            "-fixthebug",
            "--Foo",
            "--foo_bar",
            "--2foo",
            "---foo",
            "--x\u{1b}[31m",
            long.as_str(),
            "--caf\u{e9}",
            "--foo=secret-value",
        ] {
            let args = [name];
            let refusal = Refusal::unknown_option(name);
            assert_eq!(refusal, Refusal::UnknownOption(String::new()), "{args:?}");
            let notice = refusal.notice().unwrap();
            assert!(!notice.contains(name), "{args:?}: {notice}");
            assert_eq!(
                notice,
                Refusal::UnknownOption(String::new()).notice().unwrap(),
                "{args:?}"
            );
        }
        // The classifier splits '=' before sanitizing the name.
        let args = ["--foo=secret-value"];
        let refusal = claude_interactive(&argv(&args)).unwrap_err();
        assert_eq!(
            refusal,
            Refusal::UnknownOption("--foo".to_owned()),
            "{args:?}"
        );
        let notice = refusal.notice().unwrap();
        assert!(notice.contains("--foo"), "{args:?}");
        assert!(!notice.contains("secret-value"), "{args:?}");
    }

    fn claude_help_declaration(declaration: &str) -> (Vec<&str>, Arity) {
        let end = declaration.find(['<', '[']).unwrap_or(declaration.len());
        let arity = match &declaration[end..] {
            "" => Arity::Switch,
            value if value.starts_with('[') => Arity::Optional,
            value if value.ends_with("...>") => Arity::Many,
            _ => Arity::One,
        };
        let names = declaration[..end].split(',').map(str::trim).collect();
        (names, arity)
    }

    #[test]
    fn claude_help_declarations_parse_aliases_and_arity() {
        for (declaration, names, arity) in [
            ("--ide", vec!["--ide"], Arity::Switch),
            (
                "--bg, --background",
                vec!["--bg", "--background"],
                Arity::Switch,
            ),
            ("--model <model>", vec!["--model"], Arity::One),
            ("-n, --name <name>", vec!["-n", "--name"], Arity::One),
            (
                "-d, --debug [filter]",
                vec!["-d", "--debug"],
                Arity::Optional,
            ),
            ("--add-dir <directories...>", vec!["--add-dir"], Arity::Many),
            (
                "--allowedTools, --allowed-tools <tools...>",
                vec!["--allowedTools", "--allowed-tools"],
                Arity::Many,
            ),
        ] {
            let args = [declaration];
            assert_eq!(
                claude_help_declaration(declaration),
                (names, arity),
                "{args:?}"
            );
        }
    }

    #[test]
    fn claude_help_flags_are_all_classified() {
        let mut seen = std::collections::HashSet::new();
        for declaration in HELP_OPTIONS.lines() {
            let (names, arity) = claude_help_declaration(declaration);
            for name in names {
                let args = [name];
                assert!(seen.insert(name), "{args:?}: duplicate help option");
                let expected =
                    if let Some((_, reason)) = EXCLUDED.iter().find(|(flag, _)| *flag == name) {
                        Some(Flag::Excluded(*reason))
                    } else if UNDECIDED_FOR_NOW.contains(&name) {
                        None
                    } else {
                        Some(Flag::Takes(arity))
                    };
                assert_eq!(claude_flag(name), expected, "{args:?}: {declaration}");
            }
        }
        let mut decisions = std::collections::HashSet::new();
        for name in EXCLUDED
            .iter()
            .map(|(name, _)| *name)
            .chain(UNDECIDED_FOR_NOW.iter().copied())
        {
            let args = [name];
            assert!(
                seen.contains(name),
                "{args:?}: decision missing from help fixture"
            );
            assert!(decisions.insert(name), "{args:?}: duplicate decision");
        }
        let mut commands = std::collections::HashSet::new();
        for declaration in HELP_COMMANDS.lines() {
            for name in declaration.split_whitespace().next().unwrap().split('|') {
                let args = [name];
                assert!(is_claude_subcommand(name), "{args:?}: unclassified command");
                assert!(commands.insert(name), "{args:?}: duplicate help command");
            }
        }
    }

    fn claude_help_declarations<'a>(help: &'a str, section: &str) -> Vec<&'a str> {
        help.lines()
            .skip_while(|line| *line != section)
            .skip(1)
            .take_while(|line| line.is_empty() || line.starts_with(' '))
            .filter_map(|line| {
                let declaration = line.strip_prefix("  ")?;
                if declaration.is_empty() || declaration.starts_with(char::is_whitespace) {
                    return None;
                }
                Some(declaration.split("  ").next().unwrap().trim_end())
            })
            .collect()
    }

    fn claude_help_option_names(help: &str) -> Vec<&str> {
        claude_help_declarations(help, "Options:")
            .into_iter()
            .flat_map(|declaration| claude_help_declaration(declaration).0)
            .collect()
    }

    #[test]
    fn claude_help_option_names_handle_wrapped_descriptions() {
        let help = "Usage: claude [options]\n\nOptions:\n  -d, --debug [filter]  Debug\n  --allowedTools, --allowed-tools <tools...>\n      Description mentioning --not-an-option\n  --model <model>  Model\n                                        --not-an-option <value>\n  --ide  Connect\n\nCommands:\n  plugin|plugins  Manage plugins\n  install [options] [target]  Install\n      Description mentioning --not-an-option\n";
        let args = ["claude", "--help"];
        assert_eq!(
            claude_help_option_names(help),
            [
                "-d",
                "--debug",
                "--allowedTools",
                "--allowed-tools",
                "--model",
                "--ide"
            ],
            "{args:?}"
        );
        for (section, declarations) in [
            (
                "Options:",
                vec![
                    "-d, --debug [filter]",
                    "--allowedTools, --allowed-tools <tools...>",
                    "--model <model>",
                    "--ide",
                ],
            ),
            (
                "Commands:",
                vec!["plugin|plugins", "install [options] [target]"],
            ),
        ] {
            assert_eq!(
                claude_help_declarations(help, section),
                declarations,
                "{args:?}: {section}"
            );
        }
    }

    #[test]
    #[ignore = "needs the real Claude Code CLI; run with --ignored"]
    fn claude_help_matches_installed_cli() {
        let args = ["claude", "--help"];
        let output = match std::process::Command::new(args[0]).arg(args[1]).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("Skipping {args:?}: Claude Code CLI is not installed");
                return;
            }
            Err(error) => panic!("{args:?}: {error}"),
        };
        assert!(output.status.success(), "{args:?}: {output:?}");
        let help = String::from_utf8(output.stdout).expect("Claude Code --help must be UTF-8");
        let normalized = |lines: Vec<&str>| -> std::collections::BTreeSet<String> {
            lines
                .into_iter()
                .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect()
        };
        for (section, fixture) in [("Options:", HELP_OPTIONS), ("Commands:", HELP_COMMANDS)] {
            let installed = normalized(claude_help_declarations(&help, section));
            let captured = normalized(fixture.lines().collect());
            let missing: Vec<_> = captured.difference(&installed).collect();
            let added: Vec<_> = installed.difference(&captured).collect();
            assert!(
                missing.is_empty() && added.is_empty(),
                "{args:?}: {section} missing declarations: {missing:?}; added declarations: {added:?}"
            );
        }
    }

    #[test]
    fn every_cardless_launch_class_has_a_fixed_notice_or_is_deliberately_silent() {
        for agent in ["Claude Code", "Codex"] {
            for no_card in [
                NoCard::NoVersion,
                NoCard::UnsupportedVersion,
                NoCard::NotNative,
                NoCard::ServerUnavailable,
                NoCard::NotRegistered,
                NoCard::Refused(Refusal::unknown_option("--some-new-flag")),
                NoCard::Refused(Refusal::Excluded(Exclusion::HooksOff)),
                NoCard::Refused(Refusal::Excluded(Exclusion::McpOff)),
            ] {
                let notice = no_card
                    .notice(agent)
                    .unwrap_or_else(|| panic!("{no_card:?}"));
                assert!(
                    notice.starts_with(&format!(
                        "kettle agent-setup: starting {agent} without Kettle cards: "
                    )),
                    "{notice}"
                );
                assert!(notice.ends_with("go to the media shelf."), "{notice}");
                assert!(!notice.contains(['/', '$', '\n', '\x1b']), "{notice}");
                assert!(!notice.contains(|c: char| c.is_ascii_digit()), "{notice}");
            }
            for silent in [
                Refusal::Excluded(Exclusion::Print),
                Refusal::Excluded(Exclusion::Detached),
                Refusal::Excluded(Exclusion::Info),
                Refusal::Subcommand,
                Refusal::NotUnicode,
                Refusal::MissingValue,
                Refusal::UnexpectedValue,
                Refusal::ExtraPrompt,
            ] {
                assert_eq!(NoCard::Refused(silent).notice(agent), None);
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn regression_claude_status_describes_signature_admission() {
        use kettle_ui::codex_shell::AgentFunction;
        for marker in [AgentFunction::Own, AgentFunction::Unchecked] {
            let line = launch_line("claude", marker);
            assert!(
                line.contains("signature"),
                "macOS Claude status omits signature admission: {line}"
            );
            assert!(!line.contains("only if it ends"), "{line}");
        }
    }

    #[test]
    fn status_says_who_owns_the_agent_function_without_echoing_anything() {
        use kettle_ui::codex_shell::AgentFunction;
        for name in ["claude", "codex"] {
            let kettle = launch_line(name, AgentFunction::Kettle);
            assert!(kettle.contains(&format!("--launch-{name}")), "{kettle}");
            let own = launch_line(name, AgentFunction::Own);
            assert!(own.contains("your own"), "{own}");
            if name == "claude" && cfg!(target_os = "macos") {
                assert!(
                    own.contains("macOS checks Claude's signature for cards"),
                    "{own}"
                );
            } else {
                assert!(
                    own.contains("only if it ends in Kettle's launcher"),
                    "{own}"
                );
                assert!(
                    own.contains(&format!("--print --agent {name} --form wrapper")),
                    "{own}"
                );
            }
            let unchecked = launch_line(name, AgentFunction::Unchecked);
            assert!(unchecked.starts_with("not checked"), "{unchecked}");
            if name == "claude" && cfg!(target_os = "macos") {
                assert!(
                    unchecked.contains("macOS checks Claude's signature for cards"),
                    "{unchecked}"
                );
            } else {
                assert!(unchecked.contains("--form wrapper"), "{unchecked}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn print_forms_name_the_launcher_and_the_entry_needs_no_shell() {
        let kettle = fixture("kettle");
        for agent in [SetupAgent::Claude, SetupAgent::Codex] {
            let name = if agent == SetupAgent::Claude {
                "claude"
            } else {
                "codex"
            };
            for shell in [SetupShell::Bash, SetupShell::Zsh, SetupShell::Fish] {
                for form in [SetupForm::Function, SetupForm::Wrapper] {
                    let text = print_form(&kettle, agent, Some(shell), form).unwrap();
                    assert!(
                        text.contains(&format!("agent-setup --launch-{name} --")),
                        "{text}"
                    );
                    assert_eq!(
                        text.contains("KETTLE_PANE_ID"),
                        form == SetupForm::Wrapper,
                        "{text}"
                    );
                }
            }
            // An entry script is a POSIX script whatever shell is chosen.
            let entry = print_form(&kettle, agent, None, SetupForm::Entry).unwrap();
            assert!(entry.starts_with("#!/bin/sh\n"), "{entry}");
            assert!(
                entry.contains(&format!("agent-setup --launch-{name} --")),
                "{entry}"
            );
            assert_eq!(
                entry,
                print_form(&kettle, agent, Some(SetupShell::Fish), SetupForm::Entry).unwrap()
            );
        }
        assert_eq!(
            print_form(
                Path::new("relative/kettle"),
                SetupAgent::Codex,
                Some(SetupShell::Bash),
                SetupForm::Wrapper
            ),
            Err(SetupError::UnnamedKettle)
        );
    }

    fn fixture(name: &str) -> PathBuf {
        #[cfg(windows)]
        let root = Path::new(r"C:\fixture");
        #[cfg(not(windows))]
        let root = Path::new("/fixture");
        root.join(name)
    }

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn session(args: &[&str]) -> Session {
        classify(&argv(args)).session
    }

    #[test]
    fn interactive_sessions_are_told_apart_from_other_commands() {
        assert_eq!(session(&[]), Session::Fresh);
        assert_eq!(session(&["explain this code"]), Session::Fresh);
        assert_eq!(session(&["-m", "o3", "--search", "hi"]), Session::Fresh);
        assert_eq!(session(&["resume", "--last"]), Session::Resume);
        assert_eq!(session(&["resume", "id", "a prompt"]), Session::Resume);
        assert_eq!(session(&["fork", "--last", "--all"]), Session::Fork);
        assert_eq!(session(&["-C", "/tmp", "resume"]), Session::Resume);
        for args in [
            vec!["exec", "prompt"],
            vec!["e", "prompt"],
            vec!["queue", "id", "hello"],
            vec!["mcp", "list"],
            vec!["review", "--uncommitted"],
            vec!["help"],
            vec!["--help"],
            vec!["-V"],
            vec!["resume", "--help"],
            vec!["fork", "--version"],
            vec!["a prompt", "--help"],
        ] {
            assert_eq!(session(&args), Session::PassThrough, "{args:?}");
        }
    }

    #[test]
    fn option_values_never_become_commands_or_prompts() {
        assert_eq!(
            session(&["-c", "model='exec'", "resume", "--last"]),
            Session::Resume
        );
        assert_eq!(session(&["--profile", "exec", "fork"]), Session::Fork);
        assert_eq!(session(&["--model=exec", "describe this"]), Session::Fresh);
        assert_eq!(session(&["--", "exec"]), Session::Fresh);
        assert_eq!(session(&["resume", "exec", "prompt"]), Session::Resume);
    }

    #[test]
    fn forms_kettle_does_not_fully_know_pass_through() {
        for args in [
            vec!["--remote", "unix://socket"],
            vec!["resume", "--remote", "unix:///server"],
            vec!["fork", "id", "--remote=wss://server"],
            vec!["resume", "--remote-auth-token-env", "TOKEN"],
            vec!["--image", "a.png", "describe"],
            vec!["-i", "a.png"],
            vec!["--new-option", "resume"],
            vec!["resume", "--last", "--new-option"],
            vec!["-c"],
            vec!["one", "two"],
            vec!["resume", "id", "prompt", "extra"],
            vec!["--include-non-interactive"],
            vec!["fork", "--include-non-interactive"],
            // After `--` too: one prompt for a fresh session, an id and a
            // prompt for a resumed one.
            vec!["--", "one", "two"],
            vec!["resume", "--", "id", "prompt", "extra"],
            // A value that looks like an option, `--` included.
            vec!["--model", "--", "resume", "--last"],
            vec!["-c", "-x"],
        ] {
            let original = argv(&args);
            assert_eq!(
                launch_args(&original, &fixture("kettle"), false).unwrap(),
                original,
                "{args:?}"
            );
        }
        // An argument that is not Unicode passes everything through, after
        // `--` as well.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            for original in [
                vec![OsString::from_vec(vec![0xff])],
                vec![OsString::from("--"), OsString::from_vec(vec![0xff])],
            ] {
                assert_eq!(
                    launch_args(&original, &fixture("kettle"), false).unwrap(),
                    original
                );
            }
        }
    }

    #[test]
    fn kettles_options_come_right_after_the_session_command() {
        let kettle = fixture("kettle");
        let expect = |args: &[&str], before: usize| {
            let original = argv(args);
            let launch = launch_args(&original, &kettle, false).unwrap();
            assert_eq!(&launch[..before], &original[..before], "{args:?}");
            assert_eq!(launch[before], "--no-daemon", "{args:?}");
            assert_eq!(launch[before + 1], "-c", "{args:?}");
            assert!(
                launch[before + 2]
                    .to_str()
                    .unwrap()
                    .starts_with("mcp_servers.kettle="),
                "{args:?}"
            );
            assert_eq!(&launch[before + 3..], &original[before..], "{args:?}");
        };
        expect(&["a prompt"], 0);
        expect(&["--", "one prompt"], 0);
        expect(&["resume", "--", "id", "prompt"], 1);
        expect(&["--", "-starts with a dash"], 0);
        expect(&["resume", "--last"], 1);
        expect(&["-C", "/tmp", "fork", "id", "--", "prompt"], 3);
    }

    #[test]
    fn a_launch_already_off_the_background_server_keeps_its_one_flag() {
        for args in [vec!["--no-daemon"], vec!["resume", "--no-daemon", "--last"]] {
            let launch = launch_args(&argv(&args), &fixture("kettle"), false).unwrap();
            assert_eq!(
                launch.iter().filter(|arg| *arg == "--no-daemon").count(),
                1,
                "{args:?}"
            );
            assert!(launch.iter().any(|arg| arg == "-c"), "{args:?}");
        }
    }

    #[test]
    fn the_server_table_is_toml_codex_reads_as_written() {
        let kettle = fixture("a space")
            .join("'quote'")
            .join("\"double\"")
            .join("$dollar")
            .join("`tick`")
            .join("back\\slash")
            .join("kettle");
        let launch = launch_args(&[], &kettle, false).unwrap();
        let table = launch[2]
            .to_str()
            .unwrap()
            .strip_prefix("mcp_servers.kettle=")
            .unwrap();
        let parsed: toml::Table = toml::from_str(&format!("server = {table}")).unwrap();
        let server = parsed["server"].as_table().unwrap();
        assert_eq!(server["command"].as_str(), kettle.to_str());
        let strings = |key: &str| {
            server[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(strings("args"), ["mcp", "--display"]);
        assert_eq!(strings("env_vars"), ["KETTLE_PANE_ID", "KETTLE_PID"]);
        assert_eq!(strings("enabled_tools"), ["kettle_show"]);
        assert_eq!(server["enabled"].as_bool(), Some(true));
        // kettle_show alone is approved, by its own name: no server-wide
        // approval, and nothing else.
        let mut keys: Vec<_> = server.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "args",
                "command",
                "enabled",
                "enabled_tools",
                "env_vars",
                "tools"
            ]
        );
        let tools = server["tools"].as_table().unwrap();
        assert_eq!(tools.keys().collect::<Vec<_>>(), ["kettle_show"]);
        assert_eq!(
            tools["kettle_show"].as_table().unwrap(),
            &toml::Table::from_iter([("approval_mode".to_owned(), "approve".into())])
        );
        // A control character is escaped, never written raw.
        assert_eq!(toml_string("a\u{1}b\n"), "\"a\\u0001b\\u000A\"");
    }

    /// With cards, the server serves Kettle's card hook, `kettle_card` is
    /// enabled for that hook alone, and the hook asks for each
    /// `kettle_show` call's card by the call's id.
    #[test]
    fn a_card_launch_adds_kettles_hook_and_its_retrieval() {
        let launch = launch_args(&argv(&["resume", "--last"]), &fixture("kettle"), true).unwrap();
        let values: Vec<&str> = launch
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].to_str().unwrap())
            .collect();
        assert_eq!(values.len(), 4, "{launch:?}");
        let server: toml::Table = toml::from_str(&format!(
            "server = {}",
            values[0].strip_prefix("mcp_servers.kettle=").unwrap()
        ))
        .unwrap();
        let strings = |key: &str| {
            server["server"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(strings("args"), ["mcp", "--display", "--codex-card-hook"]);
        assert_eq!(
            strings("enabled_tools"),
            ["kettle_show", "kettle_card", "kettle_agent_hook"]
        );
        assert_eq!(
            server["server"]["tools"]["kettle_show"]["approval_mode"].as_str(),
            Some("approve"),
            "the model's tool, not the hook's"
        );
        assert!(server["server"]["tools"].get("kettle_card").is_none());
        let (key, value) = values[1].split_once('=').unwrap();
        assert_eq!(key, "hooks.PostToolUse");
        let hook: toml::Table = toml::from_str(&format!("groups = {value}")).unwrap();
        let group = &hook["groups"][0];
        assert_eq!(group["matcher"].as_str(), Some("mcp__kettle__kettle_show"));
        let handler = &group["hooks"][0];
        assert_eq!(handler["type"].as_str(), Some("mcp_tool"));
        assert_eq!(handler["server"].as_str(), Some("kettle"));
        assert_eq!(handler["tool"].as_str(), Some("kettle_card"));
        assert_eq!(
            handler["input"]["tool_use_id"].as_str(),
            Some("${tool_use_id}")
        );
        assert_eq!(handler["timeout"].as_integer(), Some(3));
        assert_eq!(hook["groups"].as_array().unwrap().len(), 1);

        for (value, event) in values[2..].iter().zip(["UserPromptSubmit", "Stop"]) {
            let parsed: toml::Table = toml::from_str(value).unwrap();
            assert_eq!(
                parsed["hooks"][event][0]["hooks"][0]["input"]["event"].as_str(),
                Some(event)
            );
        }
        assert_eq!(group["hooks"].as_array().unwrap().len(), 1);
        // Without cards, neither the hook nor its retrieval.
        let shelf = launch_args(&argv(&["resume", "--last"]), &fixture("kettle"), false).unwrap();
        assert_eq!(shelf.iter().filter(|arg| *arg == "-c").count(), 1);
        assert!(
            !shelf
                .iter()
                .any(|arg| arg.to_string_lossy().contains("kettle_card"))
        );
    }

    /// A launch that sets hooks itself would replace Kettle's, so it gets no
    /// cards; other settings do not count.
    #[test]
    fn a_launch_setting_its_own_hooks_gets_no_card_hook() {
        for args in [
            vec!["-c", "hooks.PostToolUse=[]"],
            vec!["--config", "hooks={}"],
            vec!["--config=hooks.Stop=[]", "a prompt"],
            vec!["resume", "-c", " hooks.state={} ", "--last"],
        ] {
            let launch = launch_args(&argv(&args), &fixture("kettle"), true).unwrap();
            assert!(
                !launch
                    .iter()
                    .any(|arg| arg.to_string_lossy().contains("kettle_card")),
                "{args:?}"
            );
        }
        let launch =
            launch_args(&argv(&["-c", "model=\"hooks\""]), &fixture("kettle"), true).unwrap();
        assert!(
            launch
                .iter()
                .any(|arg| arg.to_string_lossy().contains("kettle_card"))
        );
    }

    #[test]
    fn cards_require_a_supported_codex_release_line() {
        // macOS checks signatures; Linux checks an explicit launch scope.
        assert_eq!(
            card_version("codex-cli 0.162.0\n"),
            cfg!(any(target_os = "macos", target_os = "linux"))
        );
        assert_eq!(
            card_version("codex-cli 0.162.7"),
            cfg!(any(target_os = "macos", target_os = "linux"))
        );
        assert_eq!(
            card_version("codex-cli 0.163.0"),
            cfg!(any(target_os = "macos", target_os = "linux"))
        );
        assert_eq!(
            card_version("codex-cli 0.999.0"),
            cfg!(any(target_os = "macos", target_os = "linux"))
        );
        for version in ["codex-cli 0.161.9", "codex-cli 1.162.0", ""] {
            assert!(!card_version(version), "{version}");
        }
    }

    #[test]
    fn a_path_that_cannot_last_or_be_named_is_refused() {
        assert_eq!(
            launch_args(&[], Path::new("relative/kettle"), false),
            Err(SetupError::UnnamedKettle)
        );
        // macOS translocation; on Windows a path without a drive letter is
        // not absolute, and is refused before.
        #[cfg(unix)]
        assert_eq!(
            print_form(
                Path::new("/private/var/folders/x/AppTranslocation/y/kettle.app/kettle"),
                SetupAgent::Codex,
                Some(SetupShell::Zsh),
                SetupForm::Function
            ),
            Err(SetupError::Translocated)
        );
        // Passing through needs no path at all.
        assert_eq!(
            launch_args(&argv(&["exec", "x"]), Path::new("relative"), true),
            Ok(argv(&["exec", "x"]))
        );
    }

    #[test]
    fn only_a_0_x_codex_from_0_159_is_known() {
        for version in [
            "codex-cli 0.159.0",
            "codex-cli 0.162.0\n",
            "codex-cli 0.200.3",
        ] {
            assert!(supported_version(version), "{version}");
        }
        for version in [
            "",
            "codex-cli 0.158.9",
            "codex-cli 1.0.0",
            "codex-cli 0.162",
            "codex-cli 0.162.0-alpha",
            "0.162.0",
            "codex-cli 0.162.0 extra",
        ] {
            assert!(!supported_version(version), "{version}");
        }
    }

    /// The version is read from the first line, within the deadline, even
    /// when the output stays open, and a program that never answers is not
    /// waited on past it.
    #[cfg(unix)]
    #[test]
    fn the_version_probe_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let source = directory.path().join(format!("{name}.sh"));
            std::fs::write(&source, format!("#!/bin/sh\n{body}\n")).unwrap();
            // Copied so that no child another test forks meanwhile holds it
            // open for writing, which fails running it (ETXTBSY) on Linux.
            let path = directory.path().join(name);
            kettle_test_support::copy_executable_fixture(&source, &path).unwrap();
            path
        };
        // Hands its output to a child that outlives it.
        let lingering = script("lingering", "echo 'codex-cli 0.162.0'\nsleep 5 &\nexit 0");
        let started = std::time::Instant::now();
        assert_eq!(
            version_of(&lingering).as_deref(),
            Some("codex-cli 0.162.0\n")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let silent = script("silent", "exec sleep 30");
        let started = std::time::Instant::now();
        assert_eq!(version_of(&silent), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_kettle_pane_has_both_positive_variables() {
        fn os(value: &str) -> Option<&OsStr> {
            Some(OsStr::new(value))
        }
        assert!(in_kettle_pane(os("7"), os("18446744073709551615")));
        for (pid, pane) in [
            (None, os("1")),
            (os("1"), None),
            (os("0"), os("1")),
            (os("1"), os("0")),
            (os("x"), os("1")),
            (os("1"), os("18446744073709551616")),
        ] {
            assert!(!in_kettle_pane(pid, pane), "{pid:?} {pane:?}");
        }
    }

    #[test]
    fn shells_are_chosen_by_their_program_name() {
        assert_eq!(
            SetupShell::from_program(Path::new("/bin/zsh")),
            Some(SetupShell::Zsh)
        );
        assert_eq!(
            SetupShell::from_program(Path::new("/opt/homebrew/bin/fish")),
            Some(SetupShell::Fish)
        );
        assert_eq!(
            SetupShell::from_program(Path::new("/usr/local/bin/pwsh")),
            None
        );
        assert_eq!(SetupShell::from_program(Path::new("/bin/tcsh")), None);
    }

    /// The printed function, run by the shell it was printed for, hands
    /// every argument to Kettle exactly as given, expanding nothing.
    #[cfg(unix)]
    #[test]
    fn the_printed_function_passes_arguments_untouched() {
        let directory = tempfile::tempdir().unwrap();
        // A stand-in Kettle at an awkward path that prints each argument on
        // a line of its own.
        let stand_in = directory.path().join("it's a $HOME `x` dir").join("kettle");
        std::fs::create_dir_all(stand_in.parent().unwrap()).unwrap();
        let source = directory.path().join("stand-in.sh");
        std::fs::write(
            &source,
            "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]\\n' \"$a\"; done\n",
        )
        .unwrap();
        // Copied, as `script` copies, so no forked child holds it for writing.
        kettle_test_support::copy_executable_fixture(&source, &stand_in).unwrap();
        let arguments = [
            "two words",
            "$HOME",
            "`date`",
            "'single'",
            "\"double\"",
            "-c",
        ];
        let expected: String = ["agent-setup", "--launch-codex", "--"]
            .iter()
            .chain(arguments.iter())
            .map(|arg| format!("[{arg}]\n"))
            .collect();
        for (shell, program) in [(SetupShell::Bash, "bash"), (SetupShell::Zsh, "zsh")] {
            let Ok(found) = std::process::Command::new(program)
                .arg("-c")
                .arg("exit 0")
                .status()
            else {
                continue;
            };
            assert!(found.success());
            let function = print_form(
                &stand_in,
                SetupAgent::Codex,
                Some(shell),
                SetupForm::Function,
            )
            .unwrap();
            let output = std::process::Command::new(program)
                .arg("-c")
                .arg(format!("{function}codex \"$@\""))
                .arg(program)
                .args(arguments)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                expected,
                "{program}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Whether previews are on is asked with an empty `show`, answered as
    /// malformed where they are on and as disabled where they are off; any
    /// other answer is not one the status knows.
    #[cfg(unix)]
    #[test]
    fn previews_are_asked_about_with_an_empty_show() {
        use std::io::{BufRead as _, Write as _};
        let directory = kettle_test_support::private_tempdir("kettle-setup-probe-");
        for (answer, want) in [
            (
                r#""ok":false,"error":{"code":"bad_params","message":"m"}"#,
                Some(true),
            ),
            (
                r#""ok":false,"error":{"code":"display_disabled","message":"m"}"#,
                Some(false),
            ),
            (r#""ok":true,"result":{}"#, None),
        ] {
            let socket = directory
                .path()
                .join(format!("probe-{}.sock", answer.len()));
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "show");
                assert_eq!(request["params"], serde_json::json!({}), "it shows nothing");
                let id = request["id"].as_u64().unwrap();
                writeln!(&stream, r#"{{"v":1,"id":{id},{answer}}}"#).unwrap();
            });
            let mut client =
                kettle_ctl::Client::connect_endpoint(socket.to_str().unwrap()).unwrap();
            assert_eq!(previews_on(&mut client).ok(), want, "{answer}");
            server.join().unwrap();
        }
    }

    /// The Kettle line says whether the strict lookup reached a Kettle with
    /// agent previews on, and why not in words of its own.
    #[test]
    fn the_kettle_line_says_whether_previews_are_reachable() {
        use kettle_ctl::CtlError;
        assert!(kettle_status(Ok(true)).contains("agent previews are on"));
        assert!(kettle_status(Ok(false)).contains("agent previews are off"));
        assert!(kettle_status(Err(&CtlError::NotInKettle)).contains("not in a Kettle"));
        assert!(kettle_status(Err(&CtlError::NoServer)).contains("turn them on"));
        let hostile = CtlError::Protocol("\u{1b}]52;c;aGk=\u{7}".into());
        assert!(!kettle_status(Err(&hostile)).contains('\u{1b}'));
    }

    /// The Claude Code line names the plugin in this pane, a local policy
    /// that forbids it, the saved setting, a Kettle running with its
    /// previews off, and that a remotely managed policy is not visible to
    /// Kettle.
    #[test]
    fn the_claude_line_names_the_plugin_policy_and_setting() {
        let status = |in_pane, forbidden| kettle_ui::ClaudeStatus {
            in_pane,
            forbidden,
            offerable: true,
        };
        for previews in [None, Some(true), Some(false)] {
            let text = |status, saved| claude_status_text(status, saved, previews);
            assert!(text(status(true, true), false).contains("started with"));
            let stuck = kettle_ui::ClaudeStatus {
                offerable: false,
                ..status(false, false)
            };
            assert!(text(stuck, true).contains("cannot offer it"));
            assert!(text(status(false, true), true).contains("managed policy"));
            assert!(text(status(false, false), false).contains("turn on"));
            // Saved on: new panes get it, unless the Kettle this shell
            // reaches started with its previews off.
            let saved = text(status(false, false), true);
            assert_eq!(saved.contains("new panes get it"), previews != Some(false));
            assert_eq!(saved.contains("previews are off"), previews == Some(false));
        }
    }

    /// Status and removal say what the harnesses still write, never that
    /// nothing is written.
    #[test]
    fn status_and_removal_name_what_the_harnesses_write() {
        for shell in [SetupShell::Bash, SetupShell::Zsh, SetupShell::Fish] {
            assert!(uninstall_text(shell).contains("history and session files"));
        }
        assert!(HARNESS_WRITES.contains("history and session files"));
        assert!(HARNESS_WRITES.contains("plugin"));
    }

    /// The automatic line says it reports the saved settings, which a
    /// running Kettle started otherwise does not follow.
    #[test]
    fn the_automatic_line_reports_the_saved_settings() {
        for on in [true, false] {
            assert!(automatic_status(on).contains("in your saved settings"));
        }
        assert!(automatic_status(true).contains("--agent-display off"));
    }

    /// A zsh whose system `zshenv` names `ZDOTDIR` is named as left alone;
    /// another shell, or a zsh without one, is not.
    #[cfg(unix)]
    #[test]
    fn the_status_names_a_zshenv_that_keeps_kettle_out() {
        let prefix = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(prefix.path().join("etc")).unwrap();
        let zsh = prefix.path().join("bin/zsh");
        if kettle_core::shell_startup::zsh_left_alone(&zsh).is_some() {
            return;
        }
        assert_eq!(zsh_status(Some(zsh.as_os_str())), None);
        let zshenv = prefix.path().join("etc/zshenv");
        std::fs::write(&zshenv, "ZDOTDIR=$HOME/.zsh\n").unwrap();
        let line = zsh_status(Some(zsh.as_os_str())).unwrap();
        assert!(line.starts_with(&zshenv.display().to_string()), "{line}");
        let bash = prefix.path().join("bin/bash");
        assert_eq!(zsh_status(Some(bash.as_os_str())), None);
        assert_eq!(zsh_status(None), None);
    }
}

#[cfg(target_os = "linux")]
mod linux;
