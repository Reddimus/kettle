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
//! It is Unix-only for now; elsewhere `agent-setup` says so.
#![cfg_attr(not(unix), allow(dead_code))]

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
/// server, the pane's two variables, and `kettle_show` alone for the model.
/// With `cards`, the server serves Kettle's card hook, and `kettle_card`,
/// which Codex lets only an enabled tool's hook call, is enabled for it;
/// the server never lists it, so the model never sees it. It sets no
/// approval, so Codex asks as its own policy says.
fn server_table(kettle: &str, cards: bool) -> String {
    let (args, tools): (&[&str], &[&str]) = if cards {
        (
            &["mcp", "--display", "--codex-card-hook"],
            &["kettle_show", "kettle_card"],
        )
    } else {
        (&["mcp", "--display"], &["kettle_show"])
    };
    format!(
        "{{command = {}, args = [{}], enabled = true, env_vars = [{}], enabled_tools = [{}]}}",
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
        injected.push(card_hook().into());
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

/// A shell `--print` can write the function for. PowerShell is not one: its
/// parameter binder drops a bare `--` before a function sees its arguments,
/// so no function there could hand them on exactly.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupShell {
    Bash,
    Zsh,
    Fish,
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

/// The function `--print` writes: `codex`, handing its arguments to this
/// Kettle as they are. The shell expands nothing in them.
fn print_function(kettle: &Path, shell: SetupShell) -> Result<String, SetupError> {
    use kettle_ui::codex_shell::CodexShell;
    let shell = match shell {
        SetupShell::Bash => CodexShell::Bash,
        SetupShell::Zsh => CodexShell::Zsh,
        SetupShell::Fish => CodexShell::Fish,
    };
    Ok(kettle_ui::codex_shell::codex_function(kettle, shell)?)
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

/// The Codex CLI whose hook output Kettle's cards are placed for: `↳ Hook ·`
/// above the message, its lines four columns in (Codex CLI 0.162).
const CARD_CODEX_MINOR: u64 = 162;

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

/// Whether it printed one whose hook output Kettle's cards are placed for,
/// on a system where Kettle can check that a card's asker is Codex as
/// OpenAI signs it: macOS. Elsewhere the hook could never get a card, so it
/// is not added.
fn card_version(output: &str) -> bool {
    cfg!(target_os = "macos") && codex_minor(output) == Some(CARD_CODEX_MINOR)
}

/// The first line `codex --version` prints, if it answers within three
/// seconds. The line is read apart from the wait, since a program that hands
/// its output to one that outlives it would hold a read open.
fn installed_version() -> Option<String> {
    version_of(Path::new("codex"))
}

fn version_of(program: &Path) -> Option<String> {
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
    let mut command = std::process::Command::new("codex");
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = command.exec();
        eprintln!("kettle agent-setup: cannot start codex: {error}");
        127
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("kettle agent-setup: cannot start codex: {error}");
                127
            }
        }
    }
}

#[derive(clap::Args, Debug)]
#[command(group(clap::ArgGroup::new("action").args(["print", "status", "uninstall"])))]
pub(crate) struct SetupArgs {
    /// Print a `codex` shell function that starts Codex with Kettle's display
    /// server. Review it, then add it to your shell's startup file; Kettle
    /// edits no file.
    #[arg(long)]
    print: bool,
    /// Report whether Codex launched from this shell would get Kettle's
    /// display server, and how it delivers media.
    #[arg(long)]
    status: bool,
    /// Print how to remove the `codex` function.
    #[arg(long)]
    uninstall: bool,
    /// Run by the printed function: start Codex with these arguments.
    #[arg(long, hide = true, conflicts_with = "action")]
    launch_codex: bool,
    /// The shell to print for (default: the one `SHELL` names).
    #[arg(long, value_enum, conflicts_with_all = ["status", "launch_codex"])]
    shell: Option<SetupShell>,
    /// Codex's own arguments, after `--`.
    #[arg(last = true, allow_hyphen_values = true, requires = "launch_codex")]
    argv: Vec<OsString>,
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
    if args.launch_codex {
        return launch(args.argv);
    }
    if args.status {
        print_status();
        return 0;
    }
    if !(args.print || args.uninstall) {
        eprintln!("kettle agent-setup: choose --print, --status or --uninstall.");
        return 2;
    }
    let shell = match SetupShell::chosen(args.shell) {
        Ok(shell) => shell,
        Err(error) => {
            eprintln!("kettle agent-setup: {}", error.message());
            return 2;
        }
    };
    if args.uninstall {
        println!("{}", uninstall_text(shell));
        return 0;
    }
    match this_kettle().and_then(|kettle| print_function(&kettle, shell)) {
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
    let cards = card_version(&version);
    match this_kettle().and_then(|kettle| launch_args(&argv, &kettle, cards)) {
        Ok(launch) => run_codex(launch),
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

fn print_status() {
    let found =
        kettle_ctl::Client::discover_display(None).and_then(|mut client| previews_on(&mut client));
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
    let launched = this_shell_in_kettle() && version.as_deref().is_some_and(supported_version);
    let cards = launched && version.as_deref().is_some_and(card_version);
    println!(
        "Delivery: {}",
        if cards {
            "the media shelf of the pane Codex runs in, and a card under the call once \
             you trust Kettle's hook in Codex's hook review"
        } else if launched {
            "the media shelf of the pane Codex runs in; cards under the call need \
             Codex CLI 0.162 on macOS"
        } else {
            "none from this shell"
        }
    );
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
    println!(
        "Interactive sessions get Kettle's display server for that launch only. Codex's \
         configuration and your shell's startup files are left as they are."
    );
    println!("{HARNESS_WRITES}");
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // No approval: Codex asks as its own policy says.
        let mut keys: Vec<_> = server.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["args", "command", "enabled", "enabled_tools", "env_vars"]
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
        assert_eq!(values.len(), 2, "{launch:?}");
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
        assert_eq!(strings("enabled_tools"), ["kettle_show", "kettle_card"]);
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
    fn cards_are_for_the_codex_whose_hook_output_kettle_knows() {
        // Only macOS can check a card's asker, so only there.
        assert_eq!(
            card_version("codex-cli 0.162.0\n"),
            cfg!(target_os = "macos")
        );
        assert_eq!(card_version("codex-cli 0.162.7"), cfg!(target_os = "macos"));
        for version in [
            "codex-cli 0.161.9",
            "codex-cli 0.163.0",
            "codex-cli 1.162.0",
            "",
        ] {
            assert!(!card_version(version), "{version}");
        }
    }

    #[test]
    fn a_path_that_cannot_last_or_be_named_is_refused() {
        assert_eq!(
            launch_args(&[], Path::new("relative/kettle"), false),
            Err(SetupError::UnnamedKettle)
        );
        assert_eq!(
            print_function(
                Path::new("/private/var/folders/x/AppTranslocation/y/kettle.app/kettle"),
                SetupShell::Zsh
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
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = directory.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
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
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        // A stand-in Kettle at an awkward path that prints each argument on
        // a line of its own.
        let stand_in = directory.path().join("it's a $HOME `x` dir").join("kettle");
        std::fs::create_dir_all(stand_in.parent().unwrap()).unwrap();
        std::fs::write(
            &stand_in,
            "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]\\n' \"$a\"; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o700)).unwrap();
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
            let function = print_function(&stand_in, shell).unwrap();
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
