//! kettle - a fast, cross-platform GPU terminal emulator.

// kettle runs as a Windows GUI-subsystem app so Windows never
// auto-allocates a console — there is ZERO phantom-console flash on
// Explorer / Start-menu launch (the long-standing complaint). When launched
// from a terminal, `attach_parent_console_if_needed()` (called first in
// `main`) attaches the parent console so CLI subcommands (`--version`,
// `--check-update`, `--print-completions`, `--shell-integration`, …) still
// print.
//
// History — why the conditional attach matters: an earlier attempt set this
// attribute but paired it with an *unconditional* AttachConsole + CONOUT$
// reopen, which OVERWROTE the inherited stdout PIPE on `kettle --flag | grep`
// (and the `>> $PROFILE` redirect), so piped output vanished and Windows CI
// went red. That was reverted to the default console subsystem +
// `ShowWindow(SW_HIDE)` — correct stdout, but a sub-50ms console flash. The
// current approach restores the GUI subsystem AND fixes the earlier stdout
// bug by reopening CONOUT$
// ONLY for std handles that are NOT already inherited (detected via
// `GetFileType` → pipe/file/char), so piped/redirected output is never
// touched. The `not(test)` guard keeps `cargo test` on the console subsystem
// so unit-test output is never hidden.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

use clap::Parser;

// Rust's standard print macros panic on write failure. That turns an expected
// early-closing pipeline into a crash report, especially for this Windows
// GUI-subsystem binary when PowerShell releases a short-lived capture pipe.
// Keep call sites readable while making crate-local CLI output fallible.
macro_rules! print {
    ($($arg:tt)*) => {{
        $crate::write_cli_stdout(format_args!($($arg)*), false)
    }};
}

macro_rules! println {
    () => {{
        $crate::write_cli_stdout(format_args!(""), true)
    }};
    ($($arg:tt)*) => {{
        $crate::write_cli_stdout(format_args!($($arg)*), true)
    }};
}

macro_rules! eprintln {
    () => {{
        $crate::write_cli_stderr(format_args!(""), true)
    }};
    ($($arg:tt)*) => {{
        $crate::write_cli_stderr(format_args!($($arg)*), true)
    }};
}

static STDOUT_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STDERR_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pipe_was_closed(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::BrokenPipe
        || (cfg!(windows) && matches!(error.raw_os_error(), Some(109 | 232)))
}

fn finish_cli_write(
    result: std::io::Result<()>,
    closed: &std::sync::atomic::AtomicBool,
    stream: &str,
) {
    if let Err(error) = result {
        if pipe_was_closed(&error) {
            closed.store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            panic!("failed writing to {stream}: {error}");
        }
    }
}

fn write_cli_stdout(args: std::fmt::Arguments<'_>, newline: bool) {
    use std::io::Write as _;

    if STDOUT_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut output = std::io::stdout().lock();
    let result = if newline {
        writeln!(output, "{args}")
    } else {
        write!(output, "{args}")
    };
    finish_cli_write(result, &STDOUT_CLOSED, "stdout");
}

fn write_cli_stderr(args: std::fmt::Arguments<'_>, newline: bool) {
    use std::io::Write as _;

    if STDERR_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut output = std::io::stderr().lock();
    let result = if newline {
        writeln!(output, "{args}")
    } else {
        write!(output, "{args}")
    };
    finish_cli_write(result, &STDERR_CLOSED, "stderr");
}

// Agent-first: headless `kettle exec` engine. Bin-side, no
// kettle-ui/winit dependency (a source-scan drift guard pins that).
mod exec;
// Agent-first: `kettle ctl` — thin control-plane client over
// kettle-ctl (discover a running server, call a method, or stream events).
mod ctl_cli;
// Agent-first: `kettle mcp` — stdio MCP server exposing kettle as
// native agent tools (run a command, drive a running kettle).
mod mcp;
mod mcp_tools;
mod update_cli;

/// Version string shown by `kettle --version`. Concatenates the
/// `Cargo.toml` version with the git SHA captured by `build.rs` (or
/// the empty string when we're not in a git checkout — source
/// tarballs, vendored builds), so the output is one of:
///
/// - `kettle 0.1.0 (a1b2c3d4e5f6)` — git checkout, sha12 in parens.
/// - `kettle 0.1.0` — non-git build; concat with an empty string
///   leaves the version pristine.
const KETTLE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("KETTLE_GIT_SHA"));
/// `sysexits.h`'s temporary-failure status. A staged Windows update cannot
/// replace the running image until this process exits, so an argument-bearing
/// invocation must tell automation that none of its requested work ran.
const EXIT_PENDING_UPDATE_TEMPORARY_FAILURE: i32 = 75;

#[derive(Parser, Debug)]
#[command(
    name = "kettle",
    version = KETTLE_VERSION,
    about = "A fast cross-platform GPU terminal emulator"
)]
struct Cli {
    /// List every bundled theme and exit.
    #[arg(long)]
    list_themes: bool,

    /// Print the keymap (trigger → action) and exit. Honors `--config FILE`
    /// to show the *effective* keymap after overrides + unbinds; without it,
    /// shows the built-in defaults.
    #[arg(long)]
    list_keybinds: bool,

    /// Print every accepted action name (for `keybind = trigger=action`) and exit.
    #[arg(long)]
    list_actions: bool,

    /// Print configured `ssh-host = name=target` entries (Ctrl+Shift+S launcher) and exit.
    #[arg(long)]
    list_ssh_hosts: bool,

    /// List saved layouts (`<config-dir>/layouts/*.json`) and exit. Launch one
    /// with `kettle --layout NAME`.
    #[arg(long)]
    list_layouts: bool,

    /// List named config profiles (`<config-dir>/profiles/*.config`) and exit.
    /// Launch one with `kettle --profile NAME`.
    #[arg(long)]
    list_profiles: bool,

    /// Print the resolved config path and exit.
    #[arg(long)]
    config_path: bool,

    /// Print the configured backend policy, resolved adapter, driver, and
    /// texture limits Kettle would use, then exit. Useful for filing a
    /// "blank window" / "no GPU adapter" bug report without a windowed run.
    #[arg(long)]
    gpu_info: bool,

    /// Check the authenticated stable feed for a newer kettle release and print
    /// the result, then exit. Bypasses the background throttle and policy; use
    /// `kettle update` to install an eligible official build.
    #[arg(long)]
    check_update: bool,

    /// Download and install the latest authenticated stable release. This is a
    /// convenience alias for `kettle update`; interactive confirmation is still
    /// required. Automation should use `kettle update --yes`.
    #[arg(long)]
    update: bool,

    /// Validate the config (resolved settings + unknown-key warnings).
    #[arg(long)]
    check_config: bool,

    /// Print the documented example config (`docs/kettle.example.config`)
    /// to stdout and exit. Pipe to your config path to bootstrap a fully
    /// commented starter file:
    ///
    ///   kettle --print-default-config > ~/.config/kettle/config
    ///
    /// Everything in the file is commented out — uncomment what you want
    /// to change. Re-read the file by either restarting kettle or
    /// triggering the in-window reload chord.
    #[arg(long, verbatim_doc_comment)]
    print_default_config: bool,

    /// Write the documented default config to your config path, creating the
    /// parent directory if needed, then exit. Unlike the
    /// `--print-default-config > FILE` redirect (which fails on a fresh
    /// install when the directory doesn't exist yet, and on PowerShell 5.1
    /// writes an unreadable UTF-16 file), this creates everything for you and
    /// refuses to overwrite an existing config. Honors `--config` / `--profile`.
    #[arg(long, verbatim_doc_comment)]
    write_default_config: bool,

    /// Print the OSC 133 shell-integration snippet for SHELL (one of
    /// `bash`, `zsh`, `fish`, `powershell` — also `pwsh` / `ps1`) and
    /// exit. Append to your shell rc file to enable `Ctrl+Up` /
    /// `Ctrl+Down` jump-to-prompt:
    ///
    ///   kettle --shell-integration bash       >> ~/.bashrc
    ///   kettle --shell-integration zsh        >> ~/.zshrc
    ///   kettle --shell-integration fish       >> ~/.config/fish/config.fish
    ///   kettle --shell-integration powershell >> $PROFILE   # PS 5+ / PS 7+
    ///
    /// Snippets live at `shell-integration/kettle.{bash,zsh,fish,ps1}`
    /// in the source tree and are embedded at build time so the
    /// binary always emits the version that shipped with it
    /// (`cargo install kettle` users included). The powershell variant
    /// covers Windows PowerShell 5.1+ and cross-platform PowerShell Core.
    #[arg(long, value_name = "SHELL", verbatim_doc_comment)]
    shell_integration: Option<String>,

    /// Print a shell completion script for SHELL (one of `bash`, `zsh`,
    /// `fish`, `elvish`, `powershell`) and exit. Append or source from
    /// your shell rc file to enable tab completion of every kettle CLI
    /// flag — `kettle --li<TAB>` → `--list-themes` / `--list-keybinds`
    /// / `--list-actions` / `--list-ssh-hosts`:
    ///
    ///   kettle --print-completions bash >> ~/.bashrc
    ///   kettle --print-completions zsh  > "${fpath[1]}/_kettle"
    ///   kettle --print-completions fish > ~/.config/fish/completions/kettle.fish
    ///
    /// The script is generated by `clap_complete` from the same `Cli`
    /// struct that powers `--help`, so a future flag automatically
    /// gets a completion.
    //
    // `${fpath[1]}` in the doc-comment above is zsh array-indexing
    // syntax, not a markdown reference link — silence the rustdoc
    // `broken_intra_doc_links` warning so the user-facing example
    // stays correct (and copy-pasteable) without inserting escape
    // characters that would leak into `--help` output.
    #[arg(long, value_name = "SHELL", verbatim_doc_comment)]
    #[allow(rustdoc::broken_intra_doc_links)]
    print_completions: Option<String>,

    /// Render a representative frame offscreen to a PNG and exit (no window).
    // Mutually exclusive with --screenshot-menu so passing
    // both fails loudly (clap rejects symmetrically + it shows in --help) rather
    // than silently dropping one.
    #[arg(long, value_name = "PATH", conflicts_with = "screenshot_menu")]
    screenshot: Option<std::path::PathBuf>,

    /// Render like `--screenshot` but with a synthetic right-click
    /// context menu open over the rendered pane. Useful for verifying
    /// the menu's render path without opening the windowed app, and
    /// for visual-regression tests in CI. Honors `--cols` / `--rows`
    /// / `--config` the same as `--screenshot`. PNG-only.
    #[arg(long, value_name = "PATH")]
    screenshot_menu: Option<std::path::PathBuf>,

    /// Caption text to place at the bottom of `--screenshot` or
    /// `--screenshot-menu` output. Useful for docs and bug reports. Example:
    ///
    ///   kettle --screenshot doc.png --annotate "kettle v1.3.x — TokyoNight Night"
    #[arg(long, value_name = "TEXT", verbatim_doc_comment)]
    annotate: Option<String>,

    /// Columns for `--screenshot` (default 96).
    #[arg(long, default_value_t = 96)]
    cols: u32,

    /// Rows for `--screenshot` (default 28).
    #[arg(long, default_value_t = 28)]
    rows: u32,

    /// Use this config file instead of the default path. Honored by every
    /// introspection command (`--check-config`, `--list-keybinds`,
    /// `--list-ssh-hosts`, `--gpu-info`, `--screenshot`, `--config-path`) as well as the
    /// windowed run. The path must be an existing, regular, readable
    /// file: a missing path is a hard error, a directory is a hard error
    /// (typing `--config ~/.config/kettle` when you meant the file inside
    /// it), and a permission-denied file is a hard error too. The
    /// out-of-the-box default-path fallback only kicks in when this flag
    /// is omitted entirely. Supplying this flag explicitly trusts the file's
    /// containing directory; default and `--profile` configs instead require a
    /// directory chain that untrusted local principals cannot modify.
    #[arg(long = "config", value_name = "FILE")]
    config: Option<std::path::PathBuf>,

    /// Working directory for the first tab (`-d DIR`).
    #[arg(long = "working-directory", short = 'd', value_name = "DIR")]
    working_directory: Option<std::path::PathBuf>,

    /// Start a separate Kettle process even when a primary bare-launch process
    /// is available. Any launch with explicit arguments already starts
    /// separately; this flag is the explicit escape hatch for an otherwise
    /// default launch.
    #[arg(long)]
    new_process: bool,

    /// Launch into a named layout. Saves and restores from
    /// `<config-dir>/layouts/<NAME>.json` so a user
    /// can maintain distinct workspaces ("dev", "ops", "docs")
    /// without each one clobbering the others on close. Example:
    ///
    ///   kettle --layout dev
    ///
    /// The name is sanitized to `[A-Za-z0-9._-]` at the session
    /// layer so a `--layout ../../etc/passwd` can't traverse out
    /// of the layouts directory.
    #[arg(long, value_name = "NAME", verbatim_doc_comment)]
    layout: Option<String>,

    /// Restore the previous session (tabs, splits, working dirs) for this
    /// launch. kettle opens a FRESH window by default (a single pane in the
    /// default cwd), like every mainstream terminal; pass `--restore` for a
    /// one-shot "continue where I left off" without setting
    /// `restore-session = true` in your config.
    #[arg(long, verbatim_doc_comment)]
    restore: bool,

    /// Enable kettle's agent control server for this launch, overriding the
    /// `agent-server` config. `off` (no server), `read-only` (read the screen /
    /// list panes / subscribe), or `full` (also send text + run commands). The
    /// server is a local-IPC surface `kettle ctl` / `kettle mcp` / an AI agent
    /// can drive — OFF by default. See docs/AGENT.md.
    #[arg(long, value_name = "MODE", verbatim_doc_comment)]
    agent_server: Option<AgentServerArg>,

    /// Deprecated receive-only compatibility for a JSON tab handoff written by
    /// an older Kettle process. Current tab tear-off moves the live tab,
    /// running programs, PTY, and scrollback in-process. The legacy handoff file
    /// is consumed once and deleted.
    #[arg(long, value_name = "PATH", verbatim_doc_comment)]
    tab_handoff: Option<std::path::PathBuf>,

    /// Deprecated receive-only compatibility for an SCM_RIGHTS tab handoff
    /// written by an older Kettle process. Current tab tear-off moves the live
    /// tab in-process. Unix-only.
    #[arg(long, value_name = "FD", verbatim_doc_comment)]
    tab_handoff_fd: Option<i32>,

    /// Record this session to an asciicast-compatible trace at PATH (replays
    /// with `asciinema play`). Captures terminal output, resizes, keystroke
    /// *tokens* (never raw typed characters by default), and UI markers. Off
    /// unless requested; also honored via the `KETTLE_RECORD` env var and the
    /// persistent `record`/`record-dir` config keys. Output is captured
    /// verbatim — review a trace before sharing it. See docs/RECORDING.md.
    #[arg(long, value_name = "PATH", verbatim_doc_comment)]
    record: Option<std::path::PathBuf>,

    /// Create a private recording directory if needed and write every launch to
    /// a new collision-safe asciicast file within it. Also honored via
    /// `KETTLE_RECORD_DIR` and the `record-dir` config key. Explicit `--record`
    /// and legacy `KETTLE_RECORD` take precedence.
    #[arg(
        long,
        value_name = "DIRECTORY",
        conflicts_with = "record",
        verbatim_doc_comment
    )]
    record_dir: Option<std::path::PathBuf>,

    /// With --record, capture RAW typed characters instead of redacted key
    /// tokens. WARNING: the trace can then contain typed passwords; leave it off
    /// unless you need byte-exact input. The window title shows `[REC RAW]` while
    /// active. Also honored via `KETTLE_RECORD_RAW_INPUT` / `record-raw-input`.
    #[arg(long, verbatim_doc_comment)]
    record_raw_input: bool,

    /// Launch with a named-profile *config* (distinct from --layout
    /// which picks the *session*). Loads
    /// `<config-dir>/profiles/<NAME>.config` instead of the default
    /// `<config-dir>/config`. Lets a user keep distinct
    /// font/theme/keybind sets per workspace:
    ///
    ///   kettle --profile dark    # uses profiles/dark.config
    ///   kettle --profile light --layout docs
    ///
    /// `--config` takes precedence over `--profile` if both are
    /// given. Name is sanitized to `[A-Za-z0-9._-]` so a
    /// `--profile ../../etc/passwd` can't traverse out.
    #[arg(long, value_name = "NAME", verbatim_doc_comment)]
    profile: Option<String>,

    /// One-off accent color (peacock parity — distinguishes multi-
    /// window kettle setups visually). Overrides the tab-bar accent
    /// strip + dragged-tab ghost + focused-pane border. Accepts
    /// `#rrggbb`, `#rgb`, `0xRRGGBB`, or an X11 color name —
    /// whatever `Rgb::parse` accepts. Example:
    ///
    ///   kettle --accent '#ff6b35' --layout dev
    ///   kettle --accent teal --layout ops
    ///
    /// For persistent assignment, put `accent-color = #ff6b35` in
    /// a `<config-dir>/profiles/<NAME>.config` and launch via
    /// `--profile <NAME>`. `--accent` always wins over the config
    /// `accent-color` key when both are set.
    #[arg(long, value_name = "COLOR", verbatim_doc_comment)]
    accent: Option<String>,

    /// Maximise the window at launch. Overrides the `window-state` config for
    /// this launch. `--maximize` is an accepted alias.
    #[arg(long = "maximise", short = 'm', visible_alias = "maximize")]
    maximise: bool,

    /// Fullscreen the window at launch. Overrides `window-state` for this launch.
    #[arg(long, short = 'f')]
    fullscreen: bool,

    /// Launch without window borders or decorations. Overrides `borderless`.
    #[arg(long, short = 'b')]
    borderless: bool,

    /// Launch hidden. Pair with a global hotkey bound to `kettle --toggle` for
    /// a dropdown window.
    #[arg(long = "hidden", short = 'H')]
    hidden: bool,

    /// Force the window title for this launch, overriding `window-title-format`.
    #[arg(long = "title", short = 'T', value_name = "TEXT")]
    title: Option<String>,

    /// Toggle the running Kettle window through local IPC and exit. Bind this
    /// to a desktop global hotkey for a dropdown window:
    ///
    ///   # GNOME: Settings → Keyboard → Custom Shortcuts → kettle --toggle
    ///   # KDE:   System Settings → Shortcuts → Custom → kettle --toggle
    ///   # Sway:  bindsym $mod+grave exec kettle --toggle
    ///   # Hyprland: bind = SUPER, grave, exec, kettle --toggle
    ///   # macOS: Karabiner / Raycast → run "kettle --toggle"
    ///   # Win11: PowerToys Keyboard Manager → kettle --toggle
    ///
    /// Sidesteps the cross-platform global-hotkey problem — each
    /// user picks the binding their setup already honors. Honors
    /// the same `--remote-file PATH` arbitration as `--remote-send`.
    #[arg(long, verbatim_doc_comment)]
    toggle: bool,

    /// Send TEXT to a running kettle via the remote-command file
    /// (default `<config-dir>/kettle/remote.cmd`) and exit. Used by
    /// external scripts to drive an already-open Kettle without launching a
    /// new window. Example:
    ///
    ///   # Bash / zsh (ANSI-C quoting supplies an actual newline):
    ///   kettle --remote-send $'ls -la\n'
    ///
    ///   # PowerShell:
    ///   kettle --remote-send "ls -la`n"
    ///
    /// The receiving kettle window must have been launched with
    /// the same `--remote-file PATH` (or both omit it to use the
    /// default path). The text is written to the focused pane of
    /// the most-recently-launched kettle that's watching the file.
    /// TEXT is transported exactly after shell argument parsing; Kettle
    /// does not reinterpret backslash escapes. Current senders use a
    /// reversible JSON-string frame, while receivers retain the legacy
    /// lossy `send-text` line format for direct writers.
    /// The shared spool is capped at 1 MiB; a send that would exceed
    /// its remaining capacity fails without changing queued commands.
    /// A receiver claims at most 1,024 operations as one batch and
    /// rejects an over-limit batch before dispatching any prefix.
    /// Multi-window arbitration is "last writer wins" for now;
    /// per-window socket addressing is a planned follow-up.
    #[arg(long, value_name = "TEXT", verbatim_doc_comment)]
    remote_send: Option<String>,

    /// Remote-command file path. Default
    /// `<config-dir>/kettle/remote.cmd`. Honored by both the
    /// `--remote-send` sender side and the kettle window's
    /// notify-watcher receiver side; both must agree on the path.
    /// See `--remote-send` above for the usage example.
    #[arg(long, value_name = "PATH")]
    remote_file: Option<std::path::PathBuf>,

    /// Execute a Lua script at startup. The script runs once with a `kettle`
    /// global namespace exposing runtime
    /// state, event hooks, bounded terminal actions, notifications, menu items,
    /// and URL handlers. Supplying this flag explicitly trusts PATH; the
    /// automatically discovered `<config-dir>/init.lua` instead requires the
    /// same trusted directory and file provenance as the default config.
    ///
    ///   kettle.version()      → string, e.g. "1.7.x"
    ///   kettle.config_path()  → string|nil, the resolved config path
    ///   kettle.theme()        → string, the resolved theme name
    ///
    /// Errors in the script print to stderr (log::warn) but don't fail the
    /// kettle launch — same shape as malformed-config tolerance.
    ///
    /// Example: print kettle's version on every launch:
    ///
    ///   echo 'print("kettle " .. kettle.version() .. " starting")' > ~/init.lua
    ///   kettle --lua-script ~/init.lua
    #[arg(long, value_name = "PATH", verbatim_doc_comment)]
    lua_script: Option<std::path::PathBuf>,

    /// Run this command in the first tab instead of the shell, e.g.
    /// `kettle -e htop` or `kettle -e ssh box`. Consumes the rest of the
    /// arguments (hyphenated flags for the program are passed through).
    #[arg(short = 'e', long = "exec", num_args = 1.., allow_hyphen_values = true, value_name = "CMD")]
    exec: Vec<String>,

    /// Agent-first subcommands. `kettle exec` runs a command
    /// headlessly under a real PTY and streams its output to stdout (no GUI);
    /// `kettle ctl` / `kettle mcp` drive a running kettle programmatically.
    /// All of kettle's existing flags are the GUI launcher; the subcommands
    /// are the non-interactive / control surface.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// `--agent-server MODE` values.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum AgentServerArg {
    Off,
    ReadOnly,
    Full,
}

/// Agent-first subcommands. Each is a self-contained non-GUI entry point that
/// returns early from `main` before any winit/GPU work.
#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Run a command under a real PTY, headlessly, and stream its output to
    /// stdout (the non-interactive counterpart to the GUI). Propagates the
    /// child's exit code; 124 when `--timeout` expires and owned-process
    /// teardown is verified, 74 on stdout delivery failure, 125 on an internal
    /// error or unverified teardown.
    Exec(ExecArgs),
    /// Drive a running kettle's agent control server: call a method (e.g.
    /// `list_panes`, `read_screen`, `screenshot`, `send_text`, `run_command`) or stream
    /// events. The target kettle must run with `agent-server` enabled.
    Ctl(CtlArgs),
    /// Run a Model Context Protocol server over stdio, exposing kettle as native
    /// agent tools. Register with Claude Code: `claude mcp add kettle -- kettle mcp`.
    Mcp(McpArgs),
    /// Install the latest authenticated stable release into an official
    /// installer-owned kettle layout.
    Update(UpdateArgs),
}

#[derive(clap::Args, Debug)]
struct UpdateArgs {
    /// Confirm installation non-interactively.
    #[arg(long)]
    yes: bool,
}

#[derive(clap::Args, Debug)]
struct McpArgs {
    /// Run an in-process self-test (initialize + tools/list + one kettle_run)
    /// and exit, instead of serving stdio. Used as a CI guard.
    #[arg(long)]
    self_test: bool,
}

#[derive(clap::Args, Debug)]
struct CtlArgs {
    /// The method to call (`get_state`, `list_tabs`, `list_panes`,
    /// `read_screen`, `read_cells`, `ui_geometry`, `screenshot`, `send_text`,
    /// `send_keys`, `dispatch_ui_key`, `dispatch_keybind`, `send_mouse`, `resize_window`,
    /// `perform_action`, `wait_for`, `run_command`), or `events` to stream the event feed.
    method: String,
    /// Target a specific pane id (else the focused pane).
    #[arg(long)]
    pane: Option<u64>,
    /// Method parameters as a JSON object (merged with `--pane`).
    #[arg(long, value_name = "JSON")]
    json: Option<String>,
    /// Text for `send_text`, the command line for `run_command`, or the
    /// substring to wait for with `wait_for`. Hyphen-leading values are
    /// legitimate text (`--text "-- INSERT --"` waits for vim's mode line).
    #[arg(long, allow_hyphen_values = true)]
    text: Option<String>,
    /// Comma-separated key tokens for `send_keys` or `dispatch_ui_key`, e.g.
    /// `--keys "escape,i,h,i,escape,ctrl+c"` (each segment is ONE key:
    /// a name like `escape`/`enter`/`f5`, a chord like `ctrl+c`, or a
    /// single character; a literal comma key is spelled `comma`, and
    /// `plus`/`minus`/`equal` name those characters).
    #[arg(long, value_name = "KEYS", allow_hyphen_values = true)]
    keys: Option<String>,
    /// Regex for `wait_for` (alternative/addition to `--text`).
    #[arg(long, value_name = "REGEX", allow_hyphen_values = true)]
    regex: Option<String>,
    /// Connect to a specific kettle pid (else the newest running server).
    #[arg(long)]
    pid: Option<u32>,
    /// Print the raw JSON result instead of a pretty summary.
    #[arg(long)]
    raw: bool,
}

#[derive(clap::Args, Debug)]
struct ExecArgs {
    /// Terminal width in columns (default: probe the console, else 80).
    #[arg(long)]
    cols: Option<u16>,
    /// Terminal height in rows (default: probe the console, else 24).
    #[arg(long)]
    rows: Option<u16>,
    /// Working directory for the child (default: inherit). An explicit path
    /// must exist and be a directory; otherwise the child is not started.
    #[arg(short = 'd', long = "cwd", value_name = "DIR")]
    cwd: Option<std::path::PathBuf>,
    /// Stop the complete run at this deadline and exit 124 when owned-process
    /// teardown is verified (125 otherwise), including while trailing PTY
    /// output is still being delivered (finite seconds, ≥ 0).
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,
    /// Strip ANSI escape sequences — emit plain text (good for assertions).
    #[arg(long, conflicts_with = "json")]
    strip_ansi: bool,
    /// Emit one JSON object per line (start / output / title / exit events).
    #[arg(long)]
    json: bool,
    /// Also record the session to an asciicast (.cast) file.
    #[arg(long, value_name = "PATH")]
    record: Option<std::path::PathBuf>,
    /// The command (and its arguments) to run.
    #[arg(required = true, num_args = 1.., allow_hyphen_values = true, value_name = "CMD", last = false, trailing_var_arg = true)]
    argv: Vec<String>,
}

/// Restore SIGPIPE to its default behavior on Unix. Rust's runtime sets
/// SIGPIPE to SIG_IGN at startup, which turns `println!` into a panic when
/// the reader of a pipeline (e.g. `kettle --list-themes | head`) closes
/// its end early. SIG_DFL makes the process exit silently on EPIPE —
/// which is what every other CLI tool does, and what shells expect when
/// chaining commands.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: `signal` is async-signal-safe and we're calling it before
    // any threads spawn (very top of `main`), so there's no race window.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
#[cfg(not(unix))]
fn reset_sigpipe() {}

/// Install one subscriber for both `log` and `tracing` events. Winit reports a
/// Wayland dispatch failure through `tracing`, so a log-only initializer loses
/// the protocol error and leaves callers with only `Exit Failure: 1`.
fn init_logging() {
    use std::io::IsTerminal as _;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        // Diagnostics belong on stderr. `tracing_subscriber`'s default writer
        // is stdout, which for `kettle exec` is the machine-readable data
        // channel: one `warn!` would splice a log line into byte-exact child
        // output, or between the NDJSON records agent callers parse. The ANSI
        // decision below already assumed stderr, so the writer had simply
        // drifted from the intent.
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

fn is_bare_gui_argv(args: impl IntoIterator) -> bool {
    let mut args = args.into_iter();
    let _program = args.next();
    args.next().is_none()
}

fn pending_update_exit_code(bare_gui_launch: bool) -> i32 {
    if bare_gui_launch {
        0
    } else {
        EXIT_PENDING_UPDATE_TEMPORARY_FAILURE
    }
}

/// Compute where a crash report is written, in addition to
/// stderr. Pure + env-injected so it is unit-testable, mirroring
/// `home_dir_fallback` in kettle-core. Uses the platform STATE dir (crash
/// logs are diagnostic state that should survive a cache clear):
///
///   - Windows: `%LOCALAPPDATA%\kettle\crash\kettle-crash-<unix>-<pid>.log`
///   - Unix:    `$XDG_STATE_HOME/kettle/crash/…` else
///     `$HOME/.local/state/kettle/crash/…`
///   - fallback (nothing set): `./kettle/crash/…`
fn crash_log_path(
    unix_secs: u64,
    pid: u32,
    get: impl Fn(&str) -> Option<String>,
) -> std::path::PathBuf {
    use std::path::PathBuf;
    // Treat empty values as unset (a stripped `set VAR=` shouldn't yield an
    // empty base) — same defensive shape as `home_dir_fallback`.
    let env = |k: &str| get(k).filter(|s| !s.is_empty());
    let base = if cfg!(windows) {
        env("LOCALAPPDATA").map(PathBuf::from)
    } else {
        env("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/state")))
    }
    .unwrap_or_else(|| PathBuf::from("."));
    base.join("kettle")
        .join("crash")
        .join(format!("kettle-crash-{unix_secs}-{pid}.log"))
}

/// Resolve the development-recording target without reading global process
/// state, keeping precedence deterministic and unit-testable:
///
/// 1. explicit `--record FILE` / `--record DIRECTORY`
/// 2. explicit `--record-dir DIRECTORY`
/// 3. legacy `KETTLE_RECORD` file/existing-directory behavior
/// 4. `KETTLE_RECORD_DIR`, which always has directory semantics
fn resolve_record_target(
    explicit: Option<std::path::PathBuf>,
    explicit_directory: Option<std::path::PathBuf>,
    legacy_env: Option<std::ffi::OsString>,
    directory_env: Option<std::ffi::OsString>,
) -> Option<kettle_core::record::RecordingTarget> {
    use kettle_core::record::RecordingTarget;

    let classify_legacy = |path: std::path::PathBuf| {
        if path.is_dir() {
            RecordingTarget::Directory(path)
        } else {
            RecordingTarget::File(path)
        }
    };
    let nonempty_env = |value: Option<std::ffi::OsString>| {
        value
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    };
    if let Some(path) = explicit {
        Some(classify_legacy(path))
    } else if let Some(directory) = explicit_directory {
        Some(RecordingTarget::Directory(directory))
    } else if let Some(path) = nonempty_env(legacy_env) {
        Some(classify_legacy(path))
    } else {
        nonempty_env(directory_env).map(RecordingTarget::Directory)
    }
}

fn recording_activation_key(target: &kettle_core::record::RecordingTarget) -> String {
    use kettle_core::record::RecordingTarget;

    let (kind, path) = match target {
        RecordingTarget::File(path) => ("file", path),
        RecordingTarget::Directory(path) => ("dir", path),
    };
    let absolute = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.clone())
    };
    format!("{kind}:{:016x}", stable_path_hash(&absolute))
}

fn stable_path_hash(path: &std::path::Path) -> u64 {
    #[cfg(unix)]
    let bytes: Vec<u8> = {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let bytes: Vec<u8> = {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

/// Install a `panic = "abort"`-safe panic hook as the very first
/// thing `main` does. Before this, a panic on a Start-menu launch was
/// invisible — an earlier console-hide approach (`ShowWindow(SW_HIDE)`)
/// swallows stderr, and `panic = "abort"` (Cargo.toml) skips unwinding — so
/// earlier debugging attempts had to *guess* at a crash's cause. The hook prints a full report (message,
/// thread, location, backtrace) to stderr AND appends it to a crash-log file
/// under the state dir, so a crash is always recoverable from a user even
/// with no console. The hook itself never panics (all `let _ =` / `unwrap_or`)
/// to avoid a double-fault under abort.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(move |info| {
        // `force_capture` yields frames even when RUST_BACKTRACE is unset.
        let bt = std::backtrace::Backtrace::force_capture();
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thread = std::thread::current();
        let tname = thread.name().unwrap_or("<unnamed>").to_string();
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());

        let report = format!(
            "kettle {KETTLE_VERSION} PANIC\ntime(unix): {when}\nthread: {tname}\n\
             location: {loc}\nmessage: {msg}\nbacktrace:\n{bt}\n"
        );

        // A fallible write, not `eprintln!`. With SIGPIPE at
        // SIG_IGN (Rust default), `eprintln!` to a broken stderr pipe panics —
        // and a panic inside the panic hook aborts immediately under
        // `panic = "abort"`, losing the crash-log write below. `writeln!` lets
        // us swallow the error and still persist the report.
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "{report}");
        }

        let path = crash_log_path(when, std::process::id(), |k| std::env::var(k).ok());
        if let Ok(mut f) = kettle_state::open_private_file_append(&path) {
            use std::io::Write as _;
            let _ = f.write_all(report.as_bytes());
        }
    }));
}

/// Windows GUI-subsystem console bridge. On a terminal launch,
/// attach the parent console and wire up ONLY the std handles that aren't
/// already inherited — so a piped/redirected stdout (`kettle --flag | grep`,
/// `… > $PROFILE`), the trap that broke the earlier unconditional-reopen
/// approach, is left untouched. This applies to stdin too: `echo y | kettle
/// update` (stdout/stderr left as the plain console) must keep the piped
/// stdin intact so `std::io::stdin().is_terminal()` downstream (e.g.
/// `update_cli`'s `--yes` guard) still sees a pipe, not a freshly reopened
/// CONIN$ console handle. On an
/// Explorer/Start-menu launch there is no parent console, so this is a no-op
/// and kettle stays a pure GUI app: no console window, no flash.
#[cfg(windows)]
fn attach_parent_console_if_needed() {
    use windows_sys::Win32::Foundation::{
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, SetLastError,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileType, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };

    // Access rights + GetFileType codes, defined locally to dodge windows-sys
    // cross-version module-path churn for these particular constants.
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_TYPE_UNKNOWN: u32 = 0x0000;
    const FILE_TYPE_DISK: u32 = 0x0001;
    const FILE_TYPE_CHAR: u32 = 0x0002;
    const FILE_TYPE_PIPE: u32 = 0x0003;
    // NUL-terminated wide "CONOUT$" / "CONIN$".
    const CONOUT: &[u16] = &[0x43, 0x4f, 0x4e, 0x4f, 0x55, 0x54, 0x24, 0x00];
    const CONIN: &[u16] = &[0x43, 0x4f, 0x4e, 0x49, 0x4e, 0x24, 0x00];

    // True when the parent already handed us this handle via STARTUPINFO
    // (a pipe for `| grep`, a file for `> out`, or a console char device).
    // Re-pointing such a handle is exactly the stdout-pipe regression described above.
    unsafe fn is_inherited(h: HANDLE) -> bool {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return false;
        }
        // SAFETY: `h` came from GetStdHandle; these only read/clear this
        // thread's OS state. GetFileType returns FILE_TYPE_UNKNOWN BOTH for a
        // genuine unknown device AND on error, and does NOT reset last-error on
        // success — so clear last-error FIRST, then a post-call GetLast() == 0
        // means a real (non-error) UNKNOWN. Without the SetLastError(0) the
        // check could read a stale code from an earlier Win32 call.
        unsafe { SetLastError(0) };
        match unsafe { GetFileType(h) } {
            FILE_TYPE_DISK | FILE_TYPE_PIPE | FILE_TYPE_CHAR => true,
            // UNKNOWN counts as usable only when it isn't actually an error.
            FILE_TYPE_UNKNOWN => (unsafe { GetLastError() }) == 0,
            _ => false,
        }
    }

    // SAFETY: runs at single-threaded process startup, before any stdout/
    // stderr writer exists, so re-pointing the std handles cannot race.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let err = GetStdHandle(STD_ERROR_HANDLE);
        let stdin_handle = GetStdHandle(STD_INPUT_HANDLE);
        let out_ok = is_inherited(out);
        let err_ok = is_inherited(err);
        // Same inherited-handle check as out/err: a piped `some-cmd | kettle
        // …` stdin is a real pipe handle here and must NOT be replaced by
        // CONIN$ below, or the piped input is silently discarded in favor of
        // the parent console's keyboard input.
        let in_ok = is_inherited(stdin_handle);
        // Piped / redirected: leave the inherited handles alone so `| grep`
        // and `> file` keep working. THIS early-return is the guard that
        // prevents re-breaking the Windows CI stdout smoke test.
        if out_ok && err_ok {
            return;
        }
        // No parent console (Explorer / Start menu) → stay windowed, no console.
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        // CONOUT$/CONIN$ are valid NUL-terminated wide device names; the
        // returned handle is owned by this process. (The enclosing `unsafe`
        // block already covers this closure body.)
        let open = |name: &[u16]| -> HANDLE {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            )
        };
        // Reopen only the missing handles (covers half-redirects like `2>log`).
        if !out_ok {
            let h = open(CONOUT);
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(STD_OUTPUT_HANDLE, h);
            }
        }
        if !err_ok {
            let h = open(CONOUT);
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(STD_ERROR_HANDLE, h);
            }
        }
        // Guarded exactly like out/err above: only reopen CONIN$ when stdin
        // wasn't already a valid inherited handle. Without this guard, every
        // terminal launch that needs an out/err reopen (the common case —
        // any plain, non-redirected launch) would unconditionally overwrite
        // an already-piped stdin with the parent console's input, even
        // though stdin needed no fixing at all.
        if !in_ok {
            let h = open(CONIN);
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(STD_INPUT_HANDLE, h);
            }
        }
    }
}

/// Non-Windows: no console subsystem concept; nothing to do.
#[cfg(not(windows))]
fn attach_parent_console_if_needed() {}

fn queue_startup_update_recovery(warning: Option<&str>, queue: impl FnOnce(&str, &str)) -> bool {
    let Some(warning) = warning else {
        return false;
    };
    queue("Kettle update recovery", warning);
    true
}

fn main() -> anyhow::Result<()> {
    // Under the GUI subsystem (see the crate-root attribute), a
    // terminal launch must attach the parent console so CLI subcommands print;
    // an Explorer/Start-menu launch has no parent console and stays windowed
    // (no console at all). MUST run before any stdout/stderr use (logging /
    // println!) so the std handles are wired first.
    attach_parent_console_if_needed();
    // Capture panics (message + backtrace) to stderr AND a crash
    // log under the state dir — early so even an early panic lands.
    install_panic_hook();
    reset_sigpipe();
    init_logging();
    if kettle_update::is_pending_update_helper_invocation() {
        std::process::exit(match kettle_update::run_pending_update_helper() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("kettle update helper failed: {error}");
                1
            }
        });
    }
    let bare_gui_launch = is_bare_gui_argv(std::env::args_os());
    let (_running_install_guard, startup_update_warning) =
        match kettle_update::prepare_process_start()? {
            kettle_update::ProcessStart::Ready { guard, warning } => (guard, warning),
            kettle_update::ProcessStart::PendingUpdate { guard } => {
                eprintln!(
                    "A verified Kettle update is waiting for running windows to close; exiting this old build."
                );
                let code = pending_update_exit_code(bare_gui_launch);
                if code != 0 {
                    eprintln!(
                        "No requested command ran; retry after the other Kettle windows close \
                         (temporary failure, exit {code})."
                    );
                }
                // Do not unwind and drop the shared installation lock. The
                // helper may replace kettle.exe only after the OS has fully
                // terminated this process and released the handle.
                let _guard = guard;
                std::process::exit(code);
            }
        };
    if let Some(warning) = startup_update_warning.as_deref() {
        eprintln!("kettle update recovery: {warning}");
    }
    // Log the build identity at info level on startup. A user
    // grep'ing their stderr for warnings to file a bug report can paste
    // the surrounding lines — the version line lands once near the top,
    // disambiguating which kettle build emitted the warning. `info` level
    // is below the `warn` default filter, so the line only appears when
    // the user has bumped logging (`RUST_LOG=info kettle …`); on the
    // default filter it stays out of the way.
    log::info!("kettle {KETTLE_VERSION} starting");
    let cli = Cli::parse();

    // Agent-first: subcommands are self-contained non-GUI entry
    // points. Dispatch BEFORE any GUI flag handling / config-path checks and
    // exit with the subcommand's own code — `kettle exec`'s exit code is the
    // child's, so it must drive `std::process::exit`, not `return Ok(())`.
    if let Some(cmd) = cli.cmd {
        match cmd {
            Cmd::Exec(args) => {
                let mode = if args.json {
                    exec::OutputMode::Json
                } else if args.strip_ansi {
                    exec::OutputMode::StripAnsi
                } else {
                    exec::OutputMode::Raw
                };
                // Default geometry: probe the attached console, else 80×24.
                let probed = exec::default_size_probe();
                let cols = args
                    .cols
                    .unwrap_or_else(|| probed.map(|(c, _)| c).unwrap_or(80));
                let rows = args
                    .rows
                    .unwrap_or_else(|| probed.map(|(_, r)| r).unwrap_or(24));
                // `Duration::from_secs_f64` PANICS (aborts the process) on a
                // negative, NaN, infinite, or overflowing value — all reachable
                // from the CLI (`--timeout=nan`, `--timeout=-1`, `--timeout=1e400`
                // which parses to +inf). Validate before converting and exit
                // cleanly. The MCP / control-server timeout paths already clamp;
                // this brings the `kettle exec` CLI to parity. Audit, v2.25.0.
                let timeout = match args.timeout {
                    None => None,
                    Some(s) if s.is_finite() && (0.0..=u32::MAX as f64).contains(&s) => {
                        Some(std::time::Duration::from_secs_f64(s))
                    }
                    Some(_) => {
                        eprintln!(
                            "kettle exec: --timeout must be a finite number of seconds in 0..={}",
                            u32::MAX
                        );
                        std::process::exit(exec::EXIT_INTERNAL);
                    }
                };
                let opts = exec::ExecOpts {
                    argv: args.argv,
                    cols,
                    rows,
                    cwd: args.cwd,
                    timeout,
                    mode,
                    record: args.record,
                    // Forward only non-interactive stdin (pipe/file/socket).
                    // A real terminal remains attached to the user, and
                    // /dev/null/NUL stays closed rather than being mistaken for
                    // useful input.
                    forward_stdin: exec::stdin_is_pipe(),
                };
                std::process::exit(exec::run_exec(opts));
            }
            Cmd::Ctl(args) => {
                std::process::exit(ctl_cli::run_ctl(args));
            }
            Cmd::Mcp(args) => {
                std::process::exit(if args.self_test {
                    mcp::self_test()
                } else {
                    mcp::run_mcp()
                });
            }
            Cmd::Update(args) => {
                std::process::exit(update_cli::run(args.yes, env!("CARGO_PKG_VERSION")));
            }
        }
    }

    if cli.update {
        std::process::exit(update_cli::run(false, env!("CARGO_PKG_VERSION")));
    }

    // Explicit `--config PATH` must point at a regular file. Every
    // downstream branch silently fell back to `Config::default()`
    // otherwise — the user got a screenshot / table / window with
    // their carefully-crafted theme nowhere in sight and no clue why.
    //
    // An earlier fix caught the "no such file" case; this check extends it to
    // *not a regular file* (typically a directory — a user
    // typing `--config ~/.config/kettle` instead of
    // `--config ~/.config/kettle/config` would have `read_to_string`
    // return an `IsADirectory` error, the diagnostics path would
    // log a warning and use defaults, and the user would see the
    // same "my config didn't apply" symptom as the no-such-file
    // case). Same shape as `--working-directory` below: existence
    // is necessary but not sufficient — also gate on the right type.
    // Omitting `--config` (relying on the default path) still
    // silently falls back to defaults; that's the intended
    // "kettle works out of the box" behavior.
    // Skip the must-already-exist check when `--write-default-config`
    // is set — there `--config PATH` names the file to *create*, so a missing
    // path is the expected, valid case rather than a typo to reject.
    if !cli.write_default_config
        && let Some(p) = &cli.config
        && let Some(reason) = config_path_problem(p)
    {
        return Err(anyhow::anyhow!("--config {}: {reason}", p.display()));
    }
    // `--profile NAME` gets the same treatment, and for a sharper reason than
    // `--config` had. A profile that does not resolve used to fall all the way
    // through to `Config::default()` — so `--profile darkk` launched with
    // COMPILE-TIME defaults rather than the user's own config: stock theme,
    // stock font, stock keybinds, no diagnostic. Losing every setting is a
    // worse outcome than refusing to start, and `--config` already treats a
    // bad path as fatal, so a typo'd profile should not be quietly different.
    // `--config` wins when both are given (see the resolution below), so this
    // only fires when the profile is the one actually being used.
    // Skipped for the modes that never load a profile. `--print-default-config`
    // and `--write-default-config` early-return further down and emit compiled
    // defaults, so refusing to start over an unrelated `--profile` typo would
    // block work the profile has no bearing on.
    if cli.config.is_none()
        && !ignores_profile(&cli)
        && let Some(name) = cli.profile.as_deref()
        && let Some(reason) = profile_problem(name)
    {
        return Err(anyhow::anyhow!("--profile {name}: {reason}"));
    }
    if let Some(reason) = flag_value_problem(&cli) {
        return Err(anyhow::anyhow!("{reason}"));
    }

    if cli.list_themes {
        for name in kettle_config::Theme::list() {
            println!("{name}");
        }
        return Ok(());
    }
    if cli.print_default_config {
        // `kettle --print-default-config > ~/.config/kettle/config`
        // is the one-command bootstrap. The example file lives at
        // `docs/kettle.example.config` (also linked from README and
        // CONFIG.md); embedding it at build time means the binary
        // always emits the version that shipped with it — no
        // disk-read at runtime, no path-resolution surprises, and
        // `cargo install kettle` users get the correct content
        // even if the source tree is gone.
        print!("{}", include_str!("../../../docs/kettle.example.config"));
        return Ok(());
    }
    if cli.write_default_config {
        // The robust bootstrap. `--print-default-config > FILE`
        // (the documented one-liner) fails confusingly on a fresh install
        // because the shell can't redirect into a directory that doesn't
        // exist yet (`~/.config/kettle/` / `%APPDATA%\kettle\`), and on
        // PowerShell 5.1 the `>` writes an unreadable UTF-16 file. This
        // resolves the same path kettle loads from, creates the parent
        // directory, and writes the embedded default — refusing to clobber an
        // existing config so a non-technical user can't accidentally wipe
        // their settings.
        let path = resolve_config_path(&cli).ok_or_else(|| {
            anyhow::anyhow!("could not resolve a config path (no HOME / XDG / APPDATA?)")
        })?;
        match write_default_config(&path)? {
            DefaultConfigWrite::Written => {
                println!("Wrote a default config to {}.", path.display());
                println!(
                    "Everything is commented out — uncomment what you want, then relaunch kettle."
                );
            }
            DefaultConfigWrite::AlreadyPresent => {
                println!(
                    "config already exists at {} — leaving it untouched.",
                    path.display()
                );
                println!("Delete it first if you want a fresh default, or edit it directly.");
            }
        }
        return Ok(());
    }
    if let Some(shell) = cli.print_completions.as_deref() {
        // clap_complete generates a shell-completion
        // script from the Cli derive — same source of truth as
        // `--help` so a new flag is auto-completed without a
        // manual table update.
        use clap::CommandFactory;
        use clap_complete::Shell;
        let s = match shell {
            "bash" => Shell::Bash,
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            "elvish" => Shell::Elvish,
            "powershell" => Shell::PowerShell,
            other => {
                return Err(anyhow::anyhow!(
                    "--print-completions {other:?}: unknown shell \
                     (supported: bash, zsh, fish, elvish, powershell)"
                ));
            }
        };
        let mut cmd = Cli::command();
        clap_complete::generate(s, &mut cmd, "kettle", &mut std::io::stdout());
        return Ok(());
    }
    if let Some(shell) = cli.shell_integration.as_deref() {
        // Same shape as `--print-default-config`, but for
        // the OSC 133 shell-integration snippet. Embedded at build
        // time so `cargo install kettle` users (no source tree
        // accessible) get the right snippet, and so the binary's
        // output can never drift from the in-tree source of truth
        // under `shell-integration/`.
        //
        // PowerShell support (alias `powershell` / `ps1` /
        // `pwsh`) was added later so Windows users + cross-platform PowerShell
        // Core users get jump-to-prompt parity with bash/zsh/fish. Same
        // include_str!-at-build-time embedding pattern.
        let snippet = match shell {
            "bash" => include_str!("../../../shell-integration/kettle.bash"),
            "zsh" => include_str!("../../../shell-integration/kettle.zsh"),
            "fish" => include_str!("../../../shell-integration/kettle.fish"),
            "powershell" | "pwsh" | "ps1" => {
                include_str!("../../../shell-integration/kettle.ps1")
            }
            other => {
                return Err(anyhow::anyhow!(
                    "--shell-integration {other:?}: unknown shell \
                     (supported: bash, zsh, fish, powershell)"
                ));
            }
        };
        print!("{snippet}");
        return Ok(());
    }
    if cli.list_ssh_hosts {
        // Companion to --check-config (which reports a count) and the
        // Ctrl+Shift+S launcher (which lists them in-window): users
        // configuring a bunch of hosts wanted to verify the parse
        // *from the CLI* without launching kettle. Same `--config FILE`
        // override convention as the rest of the introspection
        // commands; falls back to the default config path. Also honors
        // `--profile NAME`.
        let cfg = match resolve_config_path(&cli) {
            Some(p) if p.exists() => load_resolved_config(&cli, &p),
            _ => kettle_config::Config::default(),
        };
        for line in format_ssh_hosts(&cfg.ssh_hosts) {
            println!("{line}");
        }
        return Ok(());
    }
    if cli.list_layouts {
        // Companion to `--layout NAME` + the in-window layout picker (Alt+L):
        // verify which saved layouts exist from the CLI. Honors `--config` /
        // `--profile` only insofar as layouts live under the same config dir.
        for name in kettle_ui::list_layouts() {
            println!("{name}");
        }
        return Ok(());
    }
    if cli.list_profiles {
        // Companion to `--profile NAME`: list the named config profiles under
        // `<config-dir>/profiles/`.
        for name in kettle_config::Config::list_profiles() {
            println!("{name}");
        }
        return Ok(());
    }
    if cli.list_actions {
        // Onboarding pair to `--list-keybinds`: that one shows what's
        // currently bound; this one shows what `keybind = trigger=…`
        // values are valid. Without this, users writing a new bind had
        // to grep the source or hit `--check-config` to confirm a name
        // they guessed. The parametric forms cannot be enumerated, so
        // each gets a one-line tail blurb instead. Every one of them
        // needs its own line: `switch_to_tab_N` was accepted by the
        // parser and named nowhere in this output, which made the
        // documented "complete set" claim false.
        for name in kettle_config::keybinds::action_names() {
            println!("{name}");
        }
        println!("goto_tab:N    (parametric; N is 1-based, 1..=255)");
        println!(
            "switch_to_tab_N    (parametric; Terminator's spelling of \
         goto_tab:N, N is 1-based)"
        );
        println!(
            "new_tab_shell_N    (parametric; N is 1-based — opens the Nth new-tab \
         dropdown entry, Ctrl+Shift+1..9 by default)"
        );
        println!("unbind        (sentinel; removes the default — also: none, null, false, empty)");
        return Ok(());
    }
    if cli.list_keybinds {
        // Honor `--config FILE` (and the default config path if it
        // exists) so users see their *effective* keymap — defaults +
        // their overrides + their unbinds — not just the built-in set.
        // Previously a user who had spent time customizing their config
        // had to restart kettle and inspect by hand to confirm a
        // `keybind = …` line took effect; now they can introspect from
        // the CLI in one shot.
        // Honor `--profile NAME` here too.
        let lines = match resolve_config_path(&cli) {
            Some(p) if p.exists() => {
                let cfg = load_resolved_config(&cli, &p);
                kettle_config::keybinds::describe(&cfg.keybinds)
            }
            _ => kettle_config::keybinds::describe_defaults(),
        };
        for line in lines {
            println!("{line}");
        }
        return Ok(());
    }
    if cli.config_path {
        // Honor `--profile NAME` here too.
        match resolve_config_path(&cli) {
            Some(p) => println!("{}", p.display()),
            None => println!("(no config path resolvable)"),
        }
        return Ok(());
    }
    if cli.gpu_info {
        // Resolves the same adapter / backend the live renderer +
        // recovery path would pick, so the output is faithful to the
        // configured windowed run. No GUI / PTY needed.
        let cfg = match resolve_config_path(&cli) {
            Some(p) if p.exists() => load_resolved_config(&cli, &p),
            _ => kettle_config::Config::default(),
        };
        let info = kettle_render::gpu_info(&cfg)?;
        println!("{info}");
        return Ok(());
    }
    if cli.check_update {
        // One-shot deliberate check (no throttle, no event loop).
        println!("{}", kettle_ui::check_for_update_cli());
        return Ok(());
    }
    if cli.check_config {
        // Route through `resolve_config_path` so this
        // path honors `--profile NAME` uniformly with every other
        // introspection flag. An earlier fix did the same inline in just this
        // spot; this extracts the helper because the same gap existed at
        // every other site.
        let path = resolve_config_path(&cli);
        // Surface read errors explicitly while sharing the hardened single-read
        // path used by startup and reload. This must not regress to a raw
        // `read_to_string`: the resolved default path has not gone through the
        // explicit `--config` regular-file precheck, and configs may be UTF-16.
        let mut read_error: Option<String> = None;
        let (cfg, unknown, malformed) = match &path {
            Some(p) if p.exists() => match prepare_resolved_config(&cli, p).and_then(|()| {
                kettle_config::Config::read_from_with_trust(p, resolved_config_trust(&cli))
            }) {
                Ok(loaded) => (loaded.config, loaded.unknown_keys, loaded.malformed_values),
                Err(e) => {
                    read_error = Some(format!("could not read {}: {e}", p.display()));
                    (kettle_config::Config::default(), Vec::new(), Vec::new())
                }
            },
            _ => (kettle_config::Config::default(), Vec::new(), Vec::new()),
        };
        // Lead with the kettle build version + git SHA, so a
        // user pasting `--check-config` output into a bug report doesn't
        // also need to run `--version` separately. Matches the
        // diagnostic-first-line convention `cargo --version`-style tools
        // use in their support flags.
        println!("kettle:  {KETTLE_VERSION}");
        match &path {
            Some(p) if p.exists() => println!("config:  {}", p.display()),
            Some(p) => {
                println!("config:  {} (not found — using defaults)", p.display());
                // When no config exists at the resolved
                // default path, point the user at the bootstrap
                // one-liner. Without this, a newcomer who ran
                // `--check-config` and saw "using defaults" had to
                // know on their own that `--print-default-config`
                // is the way to create one. The hint
                // names the actual resolved path so copy-paste works.
                println!("hint:    kettle --print-default-config > {}", p.display());
            }
            None => println!("config:  (no path resolvable — using defaults)"),
        }
        println!("theme:   {}", cfg.theme_name);
        println!("font:    {} {}pt", cfg.font_family, cfg.font_size);
        println!("scrollback: {}", cfg.scrollback);
        println!("keybinds: {} bound", cfg.keybinds.len());
        // Echo back the resolved values of the per-feature config gates so
        // users can verify with `kettle --check-config` that their tweaks
        // are taking effect (rather than greping the source). Grouped by
        // theme of related settings; only one line per group for brevity.
        println!(
            "cursor:  {:?} (blink={}, interval={}ms)",
            cfg.cursor_style, cfg.cursor_blink, cfg.cursor_blink_interval
        );
        // When force_no_bell silences every bell flavor
        // regardless of mode, annotate the existing line so the user
        // doesn't read "bell: Visual" while wondering why no bell
        // actually fires. The `extra_check_config_lines` function
        // also echoes "bell:    force-no-bell=true (silences ...)"
        // as its own line; this annotation pairs the two.
        let bell_suffix = if cfg.force_no_bell {
            " (force-no-bell overrides)"
        } else {
            ""
        };
        println!(
            "bell:    {:?}{}  osc52: {:?}  min-contrast: {}",
            cfg.bell, bell_suffix, cfg.osc52, cfg.minimum_contrast
        );
        println!(
            "keyboard: modify-other-keys={}",
            cfg.modify_other_keys.as_str()
        );
        println!(
            "scroll:  on-keystroke={} on-output={} multiplier={}",
            cfg.scroll_on_keystroke, cfg.scroll_on_output, cfg.scroll_multiplier
        );
        println!(
            "mouse:   hide-while-typing={} copy-on-select={}",
            cfg.mouse_hide_while_typing, cfg.copy_on_select
        );
        println!(
            "window:  padding={}x{} opacity={} unfocused-split={}",
            cfg.padding_x, cfg.padding_y, cfg.background_opacity, cfg.unfocused_split_opacity
        );
        // Split-color overrides are individually opt-in (default = theme
        // palette[4]/[8]); only echo when the user actually set one, so
        // common defaulted configs stay terse.
        if cfg.focused_split_color.is_some() || cfg.split_divider_color.is_some() {
            let f = cfg
                .focused_split_color
                .map(|c| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b))
                .unwrap_or_else(|| "(theme)".into());
            let d = cfg
                .split_divider_color
                .map(|c| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b))
                .unwrap_or_else(|| "(theme)".into());
            println!("splits:  focused={f} divider={d}");
        }
        println!(
            "tabs:    bar={:?} pos={:?} format={:?}",
            cfg.tab_bar, cfg.tab_bar_pos, cfg.tab_format
        );
        println!("title:   format={:?}", cfg.window_title_format);
        if !cfg.word_delimiters.is_empty() {
            println!("words:   {:?}", cfg.word_delimiters);
        }
        if !cfg.ssh_hosts.is_empty() {
            println!("ssh:     {} host(s) configured", cfg.ssh_hosts.len());
        }
        // Repeatable / opt-in keys: only echo when actually set so the
        // default-config case stays terse, but show the count when the
        // user has tuned them — otherwise `--check-config` silently
        // dropped `font-feature` / per-style font families / palette
        // overrides from its summary even when the user had taken the
        // time to configure them. Symmetric with the `ssh:` line above.
        if !cfg.font_features.is_empty() {
            println!(
                "font-features: {} configured (ligatures={})",
                cfg.font_features.len(),
                cfg.font_ligatures
            );
        }
        let styled_families = [
            ("bold", cfg.font_family_bold.as_deref()),
            ("italic", cfg.font_family_italic.as_deref()),
            ("bold-italic", cfg.font_family_bold_italic.as_deref()),
        ];
        let styles_set: Vec<&str> = styled_families
            .iter()
            .filter(|(_, v)| v.is_some())
            .map(|(k, _)| *k)
            .collect();
        if !styles_set.is_empty() {
            println!(
                "font-styles: per-style overrides for [{}]",
                styles_set.join(", ")
            );
        }
        // Echo the Terminator-parity / status-bar opt-in
        // keys when the user has actually set them. Extracted as a
        // pure helper (`extra_check_config_lines`) so the contract
        // is unit-testable — without this, a user who set
        // `accent-color = #00d4ff` couldn't verify it parsed and
        // there'd be no regression test catching a future silent
        // drop. Symmetric with the lines above.
        for line in extra_check_config_lines(&cfg) {
            println!("{line}");
        }
        // Count and display I/O errors (the read failures surfaced
        // above) as their own category rather than reusing the
        // "malformed value:" prefix — a permission-denied file isn't
        // a value-parsing failure, and labeling it as one was
        // confusing the diagnostic. Read errors get an `i/o error:`
        // line instead.
        let io_count = if read_error.is_some() { 1 } else { 0 };
        let issues = unknown.len() + malformed.len() + io_count;
        if issues == 0 {
            println!("status:  OK — no issues");
            return Ok(());
        }
        println!("status:  {issues} issue(s):");
        if let Some(e) = &read_error {
            println!("  - i/o error: {e} (using defaults)");
        }
        for k in &unknown {
            println!("  - unknown key: {k}");
        }
        for k in &malformed {
            println!("  - malformed value: {k}");
        }
        std::process::exit(1);
    }

    // Both `--screenshot` and `--screenshot-menu` share
    // the same pre-validation + config load + capture path; the only
    // difference is the `DebugScene` passed to `capture_png_with`.
    // The flags are mutually exclusive — pick the first one set.
    let screenshot_target = cli
        .screenshot
        .as_ref()
        .map(|p| (p, kettle_render::DebugScene::Default, "--screenshot"))
        .or_else(|| {
            cli.screenshot_menu.as_ref().map(|p| {
                (
                    p,
                    kettle_render::DebugScene::ContextMenu,
                    "--screenshot-menu",
                )
            })
        });
    if let Some((out, scene, flag_name)) = screenshot_target {
        // The renderer's `capture_png` writes via `image::save`, which
        // dispatches on file extension and is compiled with PNG-only
        // support (kettle-render/Cargo.toml: `features = ["png"]`).
        // A typo'd `.jpg` / `.bmp` / no-extension argument used to
        // reach `image::save` and surface a crate-internal error like
        //   `The file extension `."txt"` was not recognized as an
        //   image format`
        // *after* doing all the GPU work — confusing and wasted. Pre-
        // validate so the message is clear and the failure is cheap.
        match out.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("png") => {}
            Some(e) => {
                return Err(anyhow::anyhow!(
                    "{flag_name} {}: extension .{e} not supported; \
                     only .png is built in",
                    out.display()
                ));
            }
            None => {
                return Err(anyhow::anyhow!(
                    "{flag_name} {}: missing .png extension",
                    out.display()
                ));
            }
        }
        // Use `load_from` (same path the in-window reload uses) instead
        // of an open-coded `parse_collect`: now a typo in the config
        // emits the same `log::warn!` on stderr when generating a
        // screenshot as it does when running interactively. Previously
        // `--screenshot` was the only flag that silently swallowed
        // both unknown keys *and* malformed values, which made it
        // confusing when a screenshot didn't reflect what the user
        // thought their config said.
        // Honor `--profile NAME` here too.
        let mut cfg = match resolve_config_path(&cli) {
            Some(p) if p.exists() => load_resolved_config(&cli, &p),
            _ => kettle_config::Config::default(),
        };
        // --accent CLI flag wins over the config
        // `accent-color` key for screenshots too, so a user
        // generating per-workspace docs gets the accent applied
        // without editing a config file.
        if let Some(rgb) = cli.accent.as_deref().and_then(kettle_config::Rgb::parse) {
            cfg.accent_color = Some(rgb);
        }
        // Clamp dimensions to a sane range — wgpu textures cap at 8192 px
        // per side on most GPUs, so a typo like `--cols 100000` used to
        // panic with `dimension X exceeds the limit of 8192` instead of
        // producing a friendly error. Worst-case cell size ~20 px wide /
        // ~40 px tall keeps 400×200 cells comfortably under the limit;
        // every realistic screenshot fits.
        let cols = cli.cols.clamp(20, 400);
        let rows = cli.rows.clamp(8, 200);
        // `capture_png_with` may shrink (cols, rows) further to fit
        // the GPU texture limit at the active font size; show what
        // was actually rendered, with a hint when it differs from the
        // request so the user notices their cli args didn't fully
        // apply.
        let (actual_cols, actual_rows) = kettle_render::capture_png_with_annotation(
            &cfg,
            cols,
            rows,
            out,
            scene,
            cli.annotate.as_deref(),
        )?;
        if actual_cols == cols && actual_rows == rows {
            println!("wrote {} ({cols}×{rows} cells)", out.display());
        } else {
            println!(
                "wrote {} ({actual_cols}×{actual_rows} cells — \
                 capped from {cols}×{rows} for GPU texture limit at \
                 current font size)",
                out.display()
            );
        }
        return Ok(());
    }

    // Quake dropdown: `--toggle` is sugar for `--remote-send`
    // with a fixed `toggle-window` command. Same path-resolution
    // (--remote-file or default) so a user can bind their global
    // hotkey to `kettle --toggle` without any extra config.
    if cli.toggle {
        let path = cli
            .remote_file
            .clone()
            .or_else(default_remote_file)
            .ok_or_else(|| anyhow::anyhow!("could not resolve default remote-file path"))?;
        append_remote_command(&path, b"toggle-window\n")?;
        return Ok(());
    }

    // Remote-control SENDER side. When `--remote-send TEXT`
    // is set, append one versioned, JSON-framed command to the
    // remote-command file and exit without launching a window. The
    // running kettle that's watching the file decodes the JSON string and
    // dispatches its exact bytes to the focused pane.
    if let Some(text) = cli.remote_send.as_deref() {
        let path = cli
            .remote_file
            .clone()
            .or_else(default_remote_file)
            .ok_or_else(|| anyhow::anyhow!("could not resolve default remote-file path"))?;
        let command = encode_remote_send_command(text)?;
        append_remote_command(&path, &command)?;
        return Ok(());
    }

    // Resolve --profile if --config didn't override it. --config wins
    // when both are given so a user can quickly debug a profile
    // against an explicit config file.
    let config_trust = resolved_config_trust(&cli);
    let config_path = cli.config.or_else(|| {
        cli.profile
            .as_deref()
            .and_then(kettle_config::Config::path_for_profile)
    });
    // --accent parses via the same Rgb parser the config uses, so
    // every format the config key accepts (#rrggbb / #rgb / 0xRRGGBB
    // / X11 names) works on the CLI too. A malformed value silently
    // falls through to the config's accent-color (or palette[4]) —
    // same shape as the config parse arm, no hard fail.
    let accent_override = cli.accent.as_deref().and_then(kettle_config::Rgb::parse);
    let window_state_override = window_state_from_flags(cli.maximise, cli.fullscreen, cli.hidden);
    // Only override to `true` when the flag is present — its absence must NOT
    // force borderless off over a `borderless = true` config.
    let borderless_override = cli.borderless.then_some(true);
    let remote_file = cli.remote_file.clone().or_else(default_remote_file);
    // Validate the internal handoff fd before it reaches
    // `UnixStream::from_raw_fd`. The source process always passes an inherited
    // descriptor >= 3; a negative value violates `from_raw_fd`'s safety
    // contract, and 0/1/2 would adopt stdio as a socket (and later `close` it).
    if let Some(fd) = cli.tab_handoff_fd
        && fd < 3
    {
        anyhow::bail!("--tab-handoff-fd: expected an inherited descriptor >= 3, got {fd}");
    }
    // A whitespace-only program name (`kettle -e ""`) slips
    // past the is_empty check (the Vec has one element) but would reach
    // CommandBuilder::new("") — fail loudly at the CLI surface, like --config /
    // --working-directory already do for bad paths.
    if let Some(prog) = cli.exec.first()
        && prog.trim().is_empty()
    {
        anyhow::bail!("-e/--exec: program name is empty");
    }
    // One-shot recording overrides from the CLI/env. These win over the
    // persistent `record`/`record-dir`/`record-raw-input` config keys, which the
    // app resolves when no explicit target is given here (config isn't parsed at
    // this point — only its path is). See docs/RECORDING.md.
    let record = resolve_record_target(
        cli.record,
        cli.record_dir,
        std::env::var_os("KETTLE_RECORD"),
        std::env::var_os("KETTLE_RECORD_DIR"),
    );
    let record_raw_input = cli.record_raw_input
        || std::env::var("KETTLE_RECORD_RAW_INPUT")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
    let activation_identity = kettle_ctl::activation::LaunchIdentity {
        // Config-driven recording is identical across bare launches (same config
        // file), so only an explicit CLI/env target affects launch identity.
        recording_key: record.as_ref().map(recording_activation_key),
        record_raw_input: record.is_some() && record_raw_input,
    };
    let startup_notification_queued = queue_startup_update_recovery(
        startup_update_warning.as_deref(),
        kettle_ui::queue_desktop_notification,
    );
    let activation = if bare_gui_launch && !cli.new_process {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string));
        let request = kettle_ctl::activation::ActivationRequest::new(cwd, activation_identity);
        match kettle_ctl::activation::activate_or_elect(request) {
            Ok(kettle_ctl::activation::ActivationOutcome::Activated) => {
                if startup_notification_queued {
                    // This secondary process owns the notification worker.
                    // Give its one admitted recovery warning a bounded chance
                    // to reach the OS before activation handoff exits.
                    kettle_ui::flush_desktop_notifications(std::time::Duration::from_millis(250));
                }
                return Ok(());
            }
            Ok(kettle_ctl::activation::ActivationOutcome::Primary(primary)) => Some(primary),
            Ok(kettle_ctl::activation::ActivationOutcome::Standalone) => None,
            Err(error) => {
                log::warn!("bare-launch activation unavailable: {error}; opening separately");
                None
            }
        }
    } else {
        None
    };
    kettle_ui::run_with(kettle_ui::Options {
        activation,
        command: (!cli.exec.is_empty()).then_some(cli.exec),
        // Dropdown-parity: the About panel shows exactly what
        // `--version` prints (crate version + git hash).
        version: Some(KETTLE_VERSION.to_string()),
        cwd: cli.working_directory,
        config: config_path,
        config_trust,
        layout: cli.layout,
        restore: cli.restore,
        agent_server: cli.agent_server.map(|m| match m {
            AgentServerArg::Off => kettle_config::AgentServer::Off,
            AgentServerArg::ReadOnly => kettle_config::AgentServer::ReadOnly,
            AgentServerArg::Full => kettle_config::AgentServer::Full,
        }),
        accent_override,
        window_state_override,
        borderless_override,
        title_override: cli.title,
        remote_file,
        lua_script: cli.lua_script,
        tab_handoff: cli.tab_handoff,
        tab_handoff_fd: cli.tab_handoff_fd,
        record,
        // Bool-PARSE the env var — `is_some()` turned `=0`/`=false`/empty all
        // ON, the opposite of intent, silently enabling raw keystroke (password)
        // capture into the trace. Only an explicit truthy value enables it.
        record_raw_input,
    })
}

/// Default remote-command file path. Lives under the
/// kettle config directory so `--remote-send` / `--remote-file`
/// callers and the kettle window's watcher agree without explicit
/// paths on either side. None when the config dir isn't resolvable
/// (no $HOME / $XDG_CONFIG_HOME) — same shape as
/// `Config::default_path`.
fn default_remote_file() -> Option<std::path::PathBuf> {
    kettle_config::Config::default_path().and_then(|p| p.parent().map(|d| d.join("remote.cmd")))
}

/// Encode one `--remote-send` payload as a single reversible spool line.
///
/// JSON string framing keeps control characters and literal backslash escapes
/// distinct. The receiver continues to accept the legacy `send-text` verb for
/// existing direct writers, but new CLI senders always emit this versioned
/// form.
fn encode_remote_send_command(text: &str) -> serde_json::Result<Vec<u8>> {
    let payload = serde_json::to_string(text)?;
    let mut command = Vec::with_capacity("send-text-json ".len() + payload.len() + 1);
    command.extend_from_slice(b"send-text-json ");
    command.extend_from_slice(payload.as_bytes());
    command.push(b'\n');
    Ok(command)
}

/// A remote-file append normally holds its lock for one small `write_all`.
/// Two seconds gives a receiver or another sender ample time to finish while
/// still making a suspended/stuck holder fail with an actionable timeout
/// instead of hanging a global-hotkey invocation indefinitely.
const REMOTE_COMMAND_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn remote_command_size_error(
    path: &std::path::Path,
    existing_len: u64,
    append_len: u64,
) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "remote-command append at {} would exceed the {}-byte spool cap \
             ({existing_len} existing + {append_len} new bytes)",
            path.display(),
            kettle_state::MAX_REMOTE_COMMAND_BYTES,
        ),
    )
}

fn append_remote_command(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    append_remote_command_with_timeout(path, bytes, REMOTE_COMMAND_LOCK_TIMEOUT)
}

fn append_remote_command_with_timeout(
    path: &std::path::Path,
    bytes: &[u8],
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    use std::io::Write as _;

    let append_len =
        u64::try_from(bytes.len()).map_err(|_| remote_command_size_error(path, 0, u64::MAX))?;
    if append_len > kettle_state::MAX_REMOTE_COMMAND_BYTES {
        return Err(remote_command_size_error(path, 0, append_len));
    }

    let lock_path = kettle_state::remote_command_lock_path(path);
    let _lock =
        kettle_state::ExclusiveFileLock::acquire_timeout(&lock_path, timeout).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "acquire remote-command lock at {}: {error}",
                    lock_path.display()
                ),
            )
        })?;
    let mut file = open_remote_command_file(path)?;
    let existing_len = file.metadata()?.len();
    if existing_len
        .checked_add(append_len)
        .is_none_or(|total| total > kettle_state::MAX_REMOTE_COMMAND_BYTES)
    {
        return Err(remote_command_size_error(path, existing_len, append_len));
    }
    file.write_all(bytes)
}

/// Open (creating if needed) the remote-command file that `--toggle` and
/// `--remote-send` append control lines to, and create its parent directory
/// too — the default path lives under the kettle config directory, which may
/// not exist yet on a first run.
///
/// Every line appended here is literal, potentially sensitive text: the
/// exact payload a user asked to type into their terminal via
/// `--remote-send TEXT`. Per the workspace's control-message handling rule,
/// this must be permission-restricted rather than left to the ambient process
/// umask — a shared multi-user Unix host with the common `022` umask would
/// otherwise create this file world-readable (mode 644), letting any other
/// local user read every command ever sent until the watching kettle process
/// consumes and truncates the file.
///
/// Unix: missing parent directories are created at `0700` and the file at
/// `0600`, set explicitly rather than relying on the umask. Existing parent
/// chains must not be writable by an untrusted user, while an existing file is
/// tightened on next use too.
/// Windows: the file is created with a protected current-user DACL rather than
/// inheriting the parent ACL. Existing reparse-point leaves and paths beneath
/// reparse-point parents are rejected, including custom `--remote-file` paths.
fn open_remote_command_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    kettle_state::open_private_file_append(path)
}

/// Resolve the effective config file path from
/// `--config FILE` / `--profile NAME` / the default path, in that
/// precedence. Used by every introspection flag (`--check-config`,
/// `--list-keybinds`, `--list-ssh-hosts`, `--config-path`,
/// `--screenshot`) so they all honor `--profile` uniformly.
///
/// Before this helper, only the windowed-run path (and, from a later
/// fix, `--check-config`) honored `--profile`. A user running e.g.
/// `kettle --profile dev --list-keybinds` would silently get the
/// default config's keymap rather than the dev profile's — the same
/// silent-fallback shape as the config read-error handling above.
fn resolve_config_path(cli: &Cli) -> Option<std::path::PathBuf> {
    cli.config
        .clone()
        .or_else(|| {
            cli.profile
                .as_deref()
                .and_then(kettle_config::Config::path_for_profile)
        })
        .or_else(kettle_config::Config::default_path)
}

fn resolved_config_trust(cli: &Cli) -> kettle_config::ConfigTrust {
    if cli.config.is_some() {
        kettle_config::ConfigTrust::ExplicitPath
    } else {
        kettle_config::ConfigTrust::VerifyDirectory
    }
}

fn prepare_resolved_config(cli: &Cli, path: &std::path::Path) -> std::io::Result<()> {
    if resolved_config_trust(cli) == kettle_config::ConfigTrust::ExplicitPath {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("config path has no parent: {}", path.display()),
        )
    })?;
    if let Err(error) = kettle_state::create_private_dirs(parent) {
        log::warn!(
            "could not repair config directory for {}: {error}",
            path.display()
        );
    }
    Ok(())
}

fn load_resolved_config(cli: &Cli, path: &std::path::Path) -> kettle_config::Config {
    let _ = prepare_resolved_config(cli, path);
    kettle_config::Config::load_from_with_trust(path, resolved_config_trust(cli))
}

/// Render `ssh-host` entries as the `--list-ssh-hosts` table: alphabetical
/// by name, two columns aligned to the longest name (floor 4 so single-
/// Validate a `--config PATH` argument: must be an existing regular file
/// the current process can open. Returns `None` when the path is acceptable,
/// or `Some(reason)` ready to slot into the CLI error template. Pure-modulo-
/// the-filesystem so the typo / wrong-kind / unreadable paths (no such file,
/// directory mistyped for the file inside, perm-denied file) are unit-
/// testable without spawning the binary. The matching `--working-directory`
/// check is still inlined below — the messages differ (`not a regular file`
/// vs `not a directory`) and the call site is short enough; extracting both
/// into a shared kind-enum helper would add more glue than it removes.
///
/// Also probe `File::open` so a permission-denied file fails
/// at the CLI surface instead of at the silent runtime fallback. The no-
/// such-file, not-a-regular-file, and unreadable-file checks together cover
/// the three classes of "user typed `--config FILE` but kettle ignored
/// it" complaints.
fn config_path_problem(p: &std::path::Path) -> Option<&'static str> {
    if !p.exists() {
        Some("no such file")
    } else if !p.is_file() {
        Some("not a regular file")
    } else if std::fs::File::open(p).is_err() {
        Some("not readable (permission denied or I/O error)")
    } else {
        None
    }
}

/// Whether this invocation can ignore `--profile` validation.
///
/// Built-in informational output does not read profiles. Config-derived lists
/// such as keybinds, layouts, SSH hosts, and config checks do, so a bad profile
/// must still fail those commands.
fn ignores_profile(cli: &Cli) -> bool {
    // Any mode that DOES resolve the profile disqualifies the whole
    // invocation, because clap lets several be set at once and only the first
    // in source order runs. An any-of-the-ignorers test let a mixed
    // invocation skip validation for a mode that needed it:
    // `--profile typo --list-profiles --list-ssh-hosts` ran `list-ssh-hosts`
    // first and silently printed defaults.
    let reads_profile =
        cli.list_keybinds || cli.list_layouts || cli.list_ssh_hosts || cli.check_config;
    let ignores_profile = cli.print_default_config
        || cli.write_default_config
        || cli.list_themes
        || cli.list_profiles
        || cli.list_actions
        // `--check-update` asks the update server and prints the answer; it
        // never resolves a config path, let alone a profile.
        || cli.check_update
        || cli.shell_integration.is_some()
        || cli.print_completions.is_some();
    ignores_profile && !reads_profile
}

/// What `--write-default-config` did.
#[derive(Debug, PartialEq, Eq)]
enum DefaultConfigWrite {
    Written,
    /// Something is already at that path. Not an error: the flag's promise is
    /// "you will end up with a config", and one is there.
    AlreadyPresent,
}

/// Create `path` with the shipped default config, refusing to clobber.
///
/// This is the whole of `--write-default-config` apart from resolving the path
/// and printing, so a test exercises the real decision rather than restating it.
///
/// `exists()` follows symlinks and answers about the TARGET, and the answer is
/// stale the moment it is returned. Checking and then writing let a dangling
/// link create whatever it pointed at, and let anyone who could swap the path
/// between the two steps redirect the write onto a file of their choosing —
/// while the message still promised we refuse to clobber. `create_new` asks the
/// OS to make "does not already exist" and "this is the file I wrote" one
/// atomic decision, and refuses to follow a symlink.
fn write_default_config(path: &std::path::Path) -> anyhow::Result<DefaultConfigWrite> {
    if let Some(dir) = path.parent() {
        // Private from creation: `--write-default-config` is often the very
        // first thing to make `~/.config/kettle`, so it decides the mode every
        // later private path under it is checked against.
        kettle_state::create_private_dirs(dir).map_err(|e| {
            anyhow::anyhow!("could not create config directory {}: {e}", dir.display())
        })?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(include_str!("../../../docs/kettle.example.config").as_bytes())
                .and_then(|()| file.flush())
                .map_err(|e| anyhow::anyhow!("could not write config {}: {e}", path.display()))?;
            Ok(DefaultConfigWrite::Written)
        }
        // A directory at the config path is also "something is already here,
        // leave it alone" — but Windows reports it as a permission error rather
        // than AlreadyExists, so testing only for the latter turned a friendly
        // exit 0 into `Access is denied` and exit 1.
        Err(e)
            if e.kind() == std::io::ErrorKind::AlreadyExists || path.symlink_metadata().is_ok() =>
        {
            Ok(DefaultConfigWrite::AlreadyPresent)
        }
        Err(e) => Err(anyhow::anyhow!(
            "could not write config {}: {e}",
            path.display()
        )),
    }
}

/// Reject flag VALUES that are parsed again later by code that shrugs when the
/// parse fails.
///
/// A flag whose value silently falls back is worse than one that errors: it
/// looks like it worked. `--accent` was parsed with `and_then(Rgb::parse)` in
/// both of its consumers, so an unparseable color started kettle with the
/// configured accent and no message. `--working-directory` was the same shape —
/// `kettle_core::term::Terminal::new` uses `Some(d) if is_dir => cmd.cwd(d)`
/// and falls back to `$HOME` otherwise, so a typo'd `-d ~/projets` opened the
/// shell in the home directory with nothing to indicate the request was
/// dropped.
///
/// This is one function so a test can drive it from a parsed `Cli` exactly as
/// `run` does. It reports the first problem it finds, message included, in the
/// same shape as `--profile`.
fn flag_value_problem(cli: &Cli) -> Option<String> {
    if let Some(accent) = cli.accent.as_deref()
        && kettle_config::Rgb::parse(accent).is_none()
    {
        return Some(format!(
            "--accent {accent:?}: not a color (expected #rgb, #rrggbb, \
             rgb:R/G/B, or an X11 color name)"
        ));
    }
    if let Some(path) = &cli.working_directory {
        // Distinguish the two so the user's fix is one keystroke away: a
        // missing path is a typo, an existing non-directory means they named a
        // file by mistake.
        let reason = if !path.exists() {
            Some("no such file or directory")
        } else if !path.is_dir() {
            Some("not a directory")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Some(format!("--working-directory {}: {reason}", path.display()));
        }
    }
    None
}

fn profile_problem(name: &str) -> Option<String> {
    // Resolve the config directory FIRST. Both `list_profiles` (empty Vec) and
    // `path_for_profile` (None) collapse "no config dir" into the same answer
    // they give for "nothing there" and "bad name" — so without this check the
    // message would confidently say "no profiles are configured" or "not a
    // usable profile name" on a host where the real problem is that no $HOME /
    // XDG / APPDATA could be located at all.
    if kettle_config::Config::default_path().is_none() {
        return Some(String::from(
            "cannot locate a config directory (no HOME / XDG_CONFIG_HOME / APPDATA), \
             so no profile can be resolved",
        ));
    }
    let available = kettle_config::Config::list_profiles();
    let suffix = if available.is_empty() {
        // Now unambiguous: the directory resolved and holds no profiles.
        String::from("; no profiles were found")
    } else {
        format!("; available: {}", available.join(", "))
    };
    match kettle_config::Config::path_for_profile(name) {
        // The config dir is known to resolve, so this can only be the name.
        None => Some(format!("not a usable profile name{suffix}")),
        Some(p) if !p.is_file() => Some(format!("no such profile{suffix}")),
        Some(p) if std::fs::File::open(&p).is_err() => Some(format!(
            "profile file is not readable (permission denied or I/O error): {}",
            p.display()
        )),
        Some(_) => None,
    }
}

/// character names don't collapse the column), padded with two spaces.
/// Empty input yields a single "(no ssh-host entries configured)" line so
/// the user sees their config is empty rather than no output at all.
/// Pure so the formatting is unit-testable without the CLI.
/// Format the opt-in echo lines for `--check-config`.
/// Pure helper: takes a `Config`, returns one `String` per echo line
/// the user should see. Empty `Vec` for a default config — terse
/// default-summary output is the contract.
///
/// Adding a new branch: bump the doc list below, append the `if`,
/// and add the in-isolation assertion to
/// `extra_check_config_lines_surface_each_opt_in_key`.
/// The `extra_check_config_lines_empty_for_default_config` guard
/// will catch a branch that fires on default config.
///
/// Each variant gates on a single field's non-default-ness:
///   - `accent` — `accent_color` is `Some`
///   - `bell: force-no-bell` — `force_no_bell` is `true`
///   - `triggers` — at least one trigger
///   - `lua: sandbox=...` — `lua_sandbox != Safe`
///   - `bg-image` — `background_image` non-empty
///   - `window-flags` — any of window_state /
///     borderless / always_on_top is non-default
///   - `status-bar` — `status_bar != Off`
fn extra_check_config_lines(cfg: &kettle_config::Config) -> Vec<String> {
    let mut lines = Vec::new();
    // Keys that parse and validate but have no consumer. Reporting them is the
    // whole point: accepted-and-inert is indistinguishable from working, which
    // is how a user spends an afternoon wondering why a documented setting
    // changes nothing.
    if !cfg.inert_keys.is_empty() {
        lines.push(format!(
            "inert:   {} (accepted and validated, but kettle does not act on \
             {})",
            cfg.inert_keys.join(", "),
            if cfg.inert_keys.len() == 1 {
                "it"
            } else {
                "them"
            }
        ));
    }
    if let Some(c) = cfg.accent_color {
        lines.push(format!("accent:  #{:02x}{:02x}{:02x}", c.r, c.g, c.b));
    }
    if cfg.force_no_bell {
        lines.push("bell:    force-no-bell=true (silences every bell flavor)".into());
    }
    if !cfg.triggers.is_empty() {
        lines.push(format!(
            "triggers: {} pattern(s) configured (window-urgency action)",
            cfg.triggers.len()
        ));
    }
    if cfg.lua_sandbox != kettle_config::LuaSandbox::Safe {
        lines.push(format!("lua:     sandbox={:?}", cfg.lua_sandbox));
    }
    if !cfg.background_image.is_empty() {
        lines.push(format!(
            "bg-image: {} (mode={}, blur={}, darkness={})",
            cfg.background_image,
            cfg.background_image_mode,
            cfg.background_blur,
            cfg.background_darkness
        ));
    }
    let window_flags = cfg.window_state != kettle_config::WindowState::Normal
        || cfg.borderless
        || cfg.always_on_top;
    if window_flags {
        lines.push(format!(
            "window-flags: state={:?} borderless={} always-on-top={}",
            cfg.window_state, cfg.borderless, cfg.always_on_top
        ));
    }
    if cfg.status_bar != kettle_config::StatusBarMode::Off {
        lines.push(format!("status-bar: {:?}", cfg.status_bar));
    }
    lines
}

fn format_ssh_hosts(hosts: &[(String, String)]) -> Vec<String> {
    if hosts.is_empty() {
        return vec!["(no ssh-host entries configured)".into()];
    }
    let width = hosts.iter().map(|(n, _)| n.len()).max().unwrap_or(0).max(4);
    let mut rows: Vec<(&str, &str)> = hosts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    rows.sort_unstable();
    rows.into_iter()
        .map(|(name, target)| format!("{name:<width$}  {target}"))
        .collect()
}

/// Map the window-state CLI flags to an override (Terminator parity).
/// Fixed precedence: **hidden > fullscreen >
/// maximise** — `-H` is the explicit "don't show me" intent (Quake-style
/// background launch), so it must not be silently dropped when a script also
/// passes `-m`/`-f` (Terminator applies hidden last for the same reason).
/// Pure so the precedence is drift-guarded.
fn window_state_from_flags(
    maximise: bool,
    fullscreen: bool,
    hidden: bool,
) -> Option<kettle_config::WindowState> {
    if hidden {
        Some(kettle_config::WindowState::Hidden)
    } else if fullscreen {
        Some(kettle_config::WindowState::Fullscreen)
    } else if maximise {
        Some(kettle_config::WindowState::Maximise)
    } else {
        None
    }
}

/// The production source of this file, excluding test-only items.
#[cfg(test)]
fn production_source() -> String {
    let production = kettle_test_support::production_source(include_str!("main.rs"));
    assert!(
        !production.contains("fn production_source()"),
        "the production slice retained its own helper"
    );
    assert!(
        !production.contains("#[test]"),
        "the production slice retained a test function"
    );
    assert!(
        !production.contains("#[cfg(test)]"),
        "the production slice retained a test-only item"
    );
    production
}

#[cfg(test)]
mod window_state_flag_tests {
    use super::window_state_from_flags;
    use kettle_config::WindowState;

    /// Full truth table — in particular `-H` must win over
    /// `-m`/`-f` (it used to be silently dropped when combined).
    #[test]
    fn hidden_wins_then_fullscreen_then_maximise() {
        assert_eq!(window_state_from_flags(false, false, false), None);
        assert_eq!(
            window_state_from_flags(true, false, false),
            Some(WindowState::Maximise)
        );
        assert_eq!(
            window_state_from_flags(false, true, false),
            Some(WindowState::Fullscreen)
        );
        assert_eq!(
            window_state_from_flags(true, true, false),
            Some(WindowState::Fullscreen)
        );
        for m in [false, true] {
            for f in [false, true] {
                assert_eq!(
                    window_state_from_flags(m, f, true),
                    Some(WindowState::Hidden),
                    "-H wins over -m/-f (m={m}, f={f})"
                );
            }
        }
    }
}

#[cfg(test)]
mod activation_cli_tests {
    use super::{
        Cli, EXIT_PENDING_UPDATE_TEMPORARY_FAILURE, is_bare_gui_argv, pending_update_exit_code,
    };
    use clap::Parser as _;

    #[test]
    fn only_an_argument_free_launch_is_bare() {
        assert!(is_bare_gui_argv(["kettle"]));
        assert!(!is_bare_gui_argv(["kettle", "--new-process"]));
        assert!(!is_bare_gui_argv(["kettle", "-d", "/tmp"]));
        assert!(!is_bare_gui_argv(["kettle", "--version"]));
    }

    #[test]
    fn pending_update_is_truthful_for_argument_bearing_invocations() {
        assert_eq!(pending_update_exit_code(true), 0);
        assert_eq!(
            pending_update_exit_code(false),
            EXIT_PENDING_UPDATE_TEMPORARY_FAILURE
        );
        for argv in [
            vec!["kettle", "exec", "--", "cargo", "test"],
            vec!["kettle", "mcp"],
            vec!["kettle", "--check-config"],
            vec!["kettle", "--help"],
            vec!["kettle", "--version"],
        ] {
            assert_eq!(
                pending_update_exit_code(is_bare_gui_argv(argv)),
                EXIT_PENDING_UPDATE_TEMPORARY_FAILURE
            );
        }
    }

    #[test]
    fn new_process_escape_hatch_parses() {
        let cli = Cli::try_parse_from(["kettle", "--new-process"]).unwrap();
        assert!(cli.new_process);
    }
}

#[cfg(test)]
mod record_target_tests {
    use super::{Cli, recording_activation_key, resolve_record_target};
    use clap::Parser;
    use kettle_core::record::RecordingTarget;
    use std::path::PathBuf;

    #[test]
    fn explicit_existing_directory_keeps_directory_semantics() {
        let dir = std::env::temp_dir();
        assert_eq!(
            resolve_record_target(Some(dir.clone()), None, None, None),
            Some(RecordingTarget::Directory(dir))
        );
    }

    #[test]
    fn explicit_file_target_preserves_legacy_behavior() {
        let f = PathBuf::from("C:/does/not/exist/my-trace.cast");
        assert_eq!(
            resolve_record_target(Some(f.clone()), None, None, None),
            Some(RecordingTarget::File(f))
        );
    }

    #[test]
    fn missing_record_directory_is_still_a_directory() {
        let directory = PathBuf::from("missing/private records");
        assert_eq!(
            resolve_record_target(None, Some(directory.clone()), None, None),
            Some(RecordingTarget::Directory(directory))
        );
    }

    #[test]
    fn precedence_is_cli_then_legacy_env_then_directory_env() {
        let explicit = PathBuf::from("explicit.cast");
        assert_eq!(
            resolve_record_target(
                Some(explicit.clone()),
                Some(PathBuf::from("explicit-dir")),
                Some("legacy.cast".into()),
                Some("env-dir".into()),
            ),
            Some(RecordingTarget::File(explicit))
        );
        assert_eq!(
            resolve_record_target(
                None,
                None,
                Some("legacy.cast".into()),
                Some("env-dir".into()),
            ),
            Some(RecordingTarget::File(PathBuf::from("legacy.cast")))
        );
        assert_eq!(
            resolve_record_target(None, None, Some("".into()), Some("env-dir".into())),
            Some(RecordingTarget::Directory(PathBuf::from("env-dir")))
        );
    }

    #[test]
    fn record_file_and_directory_flags_are_mutually_exclusive() {
        let error =
            Cli::try_parse_from(["kettle", "--record", "trace.cast", "--record-dir", "traces"])
                .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);

        let cli = Cli::try_parse_from(["kettle", "--record-dir", "missing traces"]).unwrap();
        assert_eq!(cli.record, None);
        assert_eq!(cli.record_dir, Some(PathBuf::from("missing traces")));
    }

    #[test]
    fn activation_key_is_stable_bounded_and_target_sensitive() {
        let file = recording_activation_key(&RecordingTarget::File(PathBuf::from("trace.cast")));
        let same = recording_activation_key(&RecordingTarget::File(PathBuf::from("trace.cast")));
        let directory =
            recording_activation_key(&RecordingTarget::Directory(PathBuf::from("trace.cast")));
        let other = recording_activation_key(&RecordingTarget::File(PathBuf::from("other.cast")));
        assert_eq!(file, same);
        assert!(file.starts_with("file:"));
        assert!(file.len() <= 32);
        assert_ne!(file, directory);
        assert_ne!(file, other);
    }
}

#[cfg(test)]
mod crash_log_tests {
    use super::{crash_log_path, pipe_was_closed};

    /// Build an env lookup closure from `(name, value)` pairs.
    fn from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_localappdata() {
        let p = crash_log_path(
            1700,
            42,
            from(&[("LOCALAPPDATA", r"C:\Users\me\AppData\Local")]),
        );
        let s = p.to_string_lossy().replace('/', "\\");
        assert!(
            s.starts_with(r"C:\Users\me\AppData\Local\kettle\crash\"),
            "{s}"
        );
        assert!(s.ends_with(r"kettle-crash-1700-42.log"), "{s}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_falls_back_to_cwd_when_unset() {
        let p = crash_log_path(1, 2, from(&[]));
        let s = p.to_string_lossy().replace('/', "\\");
        assert!(s.contains(r"kettle\crash\kettle-crash-1-2.log"), "{s}");
    }

    #[cfg(unix)]
    #[test]
    fn unix_prefers_xdg_state_home() {
        let p = crash_log_path(1700, 42, from(&[("XDG_STATE_HOME", "/x/state")]));
        assert_eq!(
            p,
            std::path::PathBuf::from("/x/state/kettle/crash/kettle-crash-1700-42.log")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_falls_back_to_home_local_state() {
        // XDG_STATE_HOME unset → $HOME/.local/state.
        let p = crash_log_path(1, 2, from(&[("HOME", "/home/u")]));
        assert_eq!(
            p,
            std::path::PathBuf::from("/home/u/.local/state/kettle/crash/kettle-crash-1-2.log")
        );
    }

    /// Empty primary var is treated as unset and falls through (cross-platform).
    #[test]
    fn empty_primary_var_falls_through() {
        #[cfg(windows)]
        let p = crash_log_path(1, 2, from(&[("LOCALAPPDATA", "")]));
        #[cfg(unix)]
        let p = crash_log_path(1, 2, from(&[("XDG_STATE_HOME", ""), ("HOME", "")]));
        let s = p.to_string_lossy().to_string();
        assert!(s.contains("kettle"), "{s}");
        assert!(s.ends_with("kettle-crash-1-2.log"), "{s}");
    }

    #[test]
    fn only_closed_pipe_errors_are_suppressed_for_cli_output() {
        assert!(pipe_was_closed(&std::io::Error::from(
            std::io::ErrorKind::BrokenPipe
        )));
        assert!(!pipe_was_closed(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        #[cfg(windows)]
        {
            assert!(pipe_was_closed(&std::io::Error::from_raw_os_error(109)));
            assert!(pipe_was_closed(&std::io::Error::from_raw_os_error(232)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, DefaultConfigWrite, append_remote_command, append_remote_command_with_timeout,
        config_path_problem, encode_remote_send_command, extra_check_config_lines,
        flag_value_problem, format_ssh_hosts, ignores_profile, queue_startup_update_recovery,
        resolved_config_trust, write_default_config,
    };
    use clap::Parser;

    #[test]
    fn update_recovery_notification_is_flushed_before_activated_handoff() {
        let mut observed = None;
        assert!(queue_startup_update_recovery(
            Some("pending transaction recovered"),
            |title, body| observed = Some((title.to_string(), body.to_string())),
        ));
        assert_eq!(
            observed,
            Some((
                "Kettle update recovery".to_string(),
                "pending transaction recovered".to_string()
            ))
        );
        assert!(!queue_startup_update_recovery(None, |_title, _body| {
            panic!("an absent warning must not queue a notification")
        }));

        let src = super::production_source();
        let activated_arm = src
            .split("ActivationOutcome::Activated")
            .nth(1)
            .and_then(|rest| rest.split("ActivationOutcome::Primary").next())
            .expect("activation handoff arm");
        assert!(
            activated_arm.contains("flush_desktop_notifications"),
            "a secondary GUI process must flush its queued recovery warning before activation handoff returns"
        );
        let queue_at = src
            .find("let startup_notification_queued = queue_startup_update_recovery(")
            .expect("GUI startup must queue the recovery warning");
        let activation_at = src
            .find("activate_or_elect(request)")
            .expect("bare GUI activation decision");
        assert!(
            queue_at < activation_at,
            "the warning must be queued before an Activated outcome can return"
        );
    }

    /// `--write-default-config` must refuse to clobber, and must say so
    /// pleasantly rather than failing.
    ///
    /// The atomic `create_new` that closed the symlink TOCTOU also changed the
    /// error surface: an existing DIRECTORY at the config path reports as a
    /// permission error on Windows, not `AlreadyExists`, so matching only the
    /// latter turned the friendly "already exists, leaving it untouched" exit 0
    /// into `Access is denied` and exit 1.
    ///
    /// Every case goes through `write_default_config`, the function the CLI arm
    /// calls — an earlier version of this test restated `create_new` and the
    /// error predicate locally, so deleting the production code left it green.
    #[test]
    fn write_default_config_leaves_anything_already_there_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");

        // A regular file that is already there.
        let existing = dir.path().join("config");
        std::fs::write(&existing, b"mine").expect("seed");
        assert_eq!(
            write_default_config(&existing).expect("an existing file is not an error"),
            DefaultConfigWrite::AlreadyPresent
        );
        assert_eq!(
            std::fs::read(&existing).expect("unchanged"),
            b"mine",
            "an existing config must be left exactly as it was"
        );

        // A DIRECTORY at the config path — the case that regressed. Windows
        // reports this as a permission error rather than AlreadyExists.
        let as_dir = dir.path().join("config-dir");
        std::fs::create_dir(&as_dir).expect("seed dir");
        assert_eq!(
            write_default_config(&as_dir)
                .expect("a directory at the config path must not be a hard error"),
            DefaultConfigWrite::AlreadyPresent
        );
        assert!(as_dir.is_dir(), "and the directory must survive");

        // A path that is genuinely free gets the shipped default, parent
        // directories and all.
        let fresh = dir.path().join("missing").join("parents").join("config");
        assert_eq!(
            write_default_config(&fresh).expect("a free path must be writable"),
            DefaultConfigWrite::Written
        );
        assert_eq!(
            std::fs::read_to_string(&fresh).expect("read back"),
            include_str!("../../../docs/kettle.example.config"),
            "the file written must be the config kettle ships"
        );

        // And running it twice is idempotent rather than destructive.
        assert_eq!(
            write_default_config(&fresh).expect("second run"),
            DefaultConfigWrite::AlreadyPresent
        );
    }

    /// A flag value that silently falls back is worse than one that errors.
    ///
    /// `--accent` was parsed with `and_then(Rgb::parse)` by both of its
    /// consumers, so `--accent tael` started kettle with the configured accent
    /// and said nothing: the flag looked like it worked. `--working-directory`
    /// had the same shape, falling back to `$HOME`. Both are checked at the
    /// surface now, through the function `run` itself calls.
    #[test]
    fn flag_values_that_would_be_silently_dropped_are_rejected_at_the_surface() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, b"").expect("seed");
        let missing = dir.path().join("nope");

        for (args, expected) in [
            (vec!["kettle", "--accent", "tael"], Some("--accent")),
            (vec!["kettle", "--accent", "#gg0000"], Some("--accent")),
            (vec!["kettle", "--accent", ""], Some("--accent")),
            (vec!["kettle", "--accent", "#ff6b35"], None),
            (vec!["kettle", "--accent", "teal"], None),
            (vec!["kettle", "--accent", "rgb:ff/6b/35"], None),
            (
                vec!["kettle", "-d", file.to_str().expect("utf-8")],
                Some("--working-directory"),
            ),
            (
                vec!["kettle", "-d", missing.to_str().expect("utf-8")],
                Some("--working-directory"),
            ),
            (
                vec!["kettle", "-d", dir.path().to_str().expect("utf-8")],
                None,
            ),
            (vec!["kettle"], None),
        ] {
            let cli = Cli::parse_from(&args);
            match (flag_value_problem(&cli), expected) {
                (Some(reason), Some(flag)) => assert!(
                    reason.starts_with(flag),
                    "{args:?} must be refused by {flag}, got {reason:?}"
                ),
                (None, None) => {}
                (got, want) => panic!("{args:?}: expected {want:?}, got {got:?}"),
            }
        }
    }

    /// A `--profile` typo must not block a command that never reads a profile.
    ///
    /// `--list-profiles` is the sharp case: it is how you find the valid names,
    /// so refusing to run it over a bad name left the user with nowhere to go
    /// from the CLI. The modes that DO resolve the profile must keep refusing —
    /// there, a typo means the output is quietly wrong (built-in keymap instead
    /// of the user's) rather than merely blocked.
    #[test]
    fn a_profile_typo_only_blocks_commands_that_read_a_profile() {
        for args in [
            vec!["kettle", "--profile", "typo", "--list-profiles"],
            vec!["kettle", "--profile", "typo", "--list-themes"],
            vec!["kettle", "--profile", "typo", "--list-actions"],
            vec!["kettle", "--profile", "typo", "--print-default-config"],
            vec!["kettle", "--profile", "typo", "--write-default-config"],
            vec!["kettle", "--profile", "typo", "--print-completions", "bash"],
            vec!["kettle", "--profile", "typo", "--check-update"],
            vec!["kettle", "--profile", "typo", "--shell-integration", "bash"],
        ] {
            let cli = Cli::parse_from(&args);
            assert!(
                ignores_profile(&cli),
                "{args:?} prints compiled-in information and must run despite a \
                 bad --profile"
            );
        }
        for args in [
            vec!["kettle", "--profile", "typo", "--list-keybinds"],
            vec!["kettle", "--profile", "typo", "--list-layouts"],
            vec!["kettle", "--profile", "typo", "--list-ssh-hosts"],
            vec!["kettle", "--profile", "typo", "--check-config"],
            // Mixed with a profile-reading mode, the whole invocation is
            // disqualified even though --check-update alone is exempt.
            vec![
                "kettle",
                "--profile",
                "typo",
                "--check-update",
                "--check-config",
            ],
            vec!["kettle", "--profile", "typo"],
            // MIXED modes: clap allows several at once and only the first in
            // source order runs, so an any-of-the-ignorers test let this skip
            // validation and then execute a mode that DOES read the profile.
            vec![
                "kettle",
                "--profile",
                "typo",
                "--list-profiles",
                "--list-ssh-hosts",
            ],
            vec![
                "kettle",
                "--profile",
                "typo",
                "--list-themes",
                "--list-keybinds",
            ],
        ] {
            let cli = Cli::parse_from(&args);
            assert!(
                !ignores_profile(&cli),
                "{args:?} resolves the profile's config, so a typo must be \
                 reported rather than silently showing the wrong thing"
            );
        }
    }

    #[test]
    fn only_an_explicit_config_path_bypasses_directory_verification() {
        let explicit = Cli::parse_from(["kettle", "--config", "project.config"]);
        assert_eq!(
            resolved_config_trust(&explicit),
            kettle_config::ConfigTrust::ExplicitPath
        );

        for args in [
            vec!["kettle"],
            vec!["kettle", "--profile", "dev"],
            vec!["kettle", "--profile", "dev", "--layout", "work"],
        ] {
            let cli = Cli::parse_from(&args);
            assert_eq!(
                resolved_config_trust(&cli),
                kettle_config::ConfigTrust::VerifyDirectory,
                "{args:?} is kettle-discovered and must not inherit explicit trust"
            );
        }
    }

    #[test]
    fn remote_command_append_uses_the_shared_bounded_lock() {
        let dir = kettle_test_support::private_tempdir("kettle-remote-lock-test-");
        let path = dir.path().join("remote.cmd");
        let lock_path = kettle_state::remote_command_lock_path(&path);
        let holder = kettle_state::ExclusiveFileLock::acquire(&lock_path).unwrap();

        let started = std::time::Instant::now();
        let error = append_remote_command_with_timeout(
            &path,
            b"send-text blocked\n",
            std::time::Duration::from_millis(50),
        )
        .expect_err("a held receiver lock must bound and reject the append");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(
            !path.exists(),
            "a timed-out sender must not mutate the command spool"
        );

        drop(holder);
        append_remote_command(&path, b"send-text first\n").unwrap();
        append_remote_command(&path, b"toggle-window\n").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"send-text first\ntoggle-window\n",
            "each complete command must append while holding the shared lock"
        );
    }

    #[test]
    fn remote_send_json_framing_round_trips_adversarial_text_exactly() {
        let text = "literal \\n; actual LF follows\nCR follows\rNUL follows\0toggle-window\nnew-tab\n\
             send-text injected";
        let command = encode_remote_send_command(text).unwrap();

        assert_eq!(
            command.iter().filter(|byte| **byte == b'\n').count(),
            1,
            "JSON framing must keep the whole payload on one physical spool line"
        );
        assert!(
            !command[..command.len() - 1]
                .iter()
                .any(|byte| matches!(*byte, b'\r' | b'\0')),
            "JSON framing must escape CR and NUL control bytes"
        );
        let payload = command
            .strip_prefix(b"send-text-json ")
            .and_then(|bytes| bytes.strip_suffix(b"\n"))
            .expect("versioned command envelope");
        assert_eq!(serde_json::from_slice::<String>(payload).unwrap(), text);
        assert!(
            payload.windows(3).any(|window| window == br"\\n"),
            "a literal backslash+n must retain an escaped backslash"
        );
        assert!(
            payload.windows(2).any(|window| window == br"\n"),
            "an actual LF must use the JSON newline escape"
        );
        assert!(
            payload.windows(6).any(|window| window == br"\u0000"),
            "NUL must be represented without placing a raw NUL in the spool"
        );
    }

    #[test]
    fn remote_command_append_accepts_the_exact_spool_limit() {
        let dir = kettle_test_support::private_tempdir("kettle-remote-lock-test-");
        let path = dir.path().join("remote.cmd");
        let prefix = b"send-text ";
        append_remote_command(&path, prefix).unwrap();

        let remaining =
            usize::try_from(kettle_state::MAX_REMOTE_COMMAND_BYTES).unwrap() - prefix.len();
        append_remote_command(&path, &vec![b'x'; remaining]).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, kettle_state::MAX_REMOTE_COMMAND_BYTES);
        assert_eq!(&bytes[..prefix.len()], prefix);
        assert!(bytes[prefix.len()..].iter().all(|byte| *byte == b'x'));
    }

    #[test]
    fn remote_command_append_rejects_over_limit_without_mutation() {
        let dir = kettle_test_support::private_tempdir("kettle-remote-lock-test-");
        let path = dir.path().join("remote.cmd");
        let cap = usize::try_from(kettle_state::MAX_REMOTE_COMMAND_BYTES).unwrap();
        let exact = vec![b'q'; cap];
        append_remote_command(&path, &exact).unwrap();

        let error = append_remote_command(&path, b"x")
            .expect_err("an append beyond the shared spool cap must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            exact,
            "rejecting aggregate growth must preserve every previously queued byte"
        );

        let absent = dir.path().join("oversize.cmd");
        let oversized = vec![b'z'; cap + 1];
        let error = append_remote_command(&absent, &oversized)
            .expect_err("one oversized command must fail before creating its spool");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !absent.exists(),
            "per-command rejection must not create or mutate the spool"
        );
    }

    /// Windows GUI-subsystem drift guard (supersedes the guards for earlier
    /// console-handling approaches).
    /// kettle is a Windows GUI-subsystem app (`#![cfg_attr(all(windows,
    /// not(test)), windows_subsystem = "windows")]`), so Explorer / Start-menu
    /// launches never get a phantom console — zero flash. A terminal launch
    /// instead attaches the parent console in `attach_parent_console_if_needed`,
    /// reopening CONOUT$/CONIN$ ONLY for std handles that aren't already
    /// inherited (detected via `GetFileType`) — so piped/redirected stdout is
    /// never clobbered.
    ///
    /// That conditional is the whole point: an earlier attempt set the same
    /// attribute but reopened the console UNCONDITIONALLY, overwriting the
    /// inherited stdout pipe on `kettle --flag | grep` and breaking Windows
    /// CI; that was reverted to the console subsystem + a `SW_HIDE` flash. If a future
    /// contributor drops the GetFileType inherited-handle early-return, piped
    /// CLI output silently disappears on Windows again. These asserts catch
    /// both directions (attribute removed, or guard removed) at gauntlet time.
    #[test]
    fn windows_gui_subsystem_with_conditional_attach_survives() {
        let src = super::production_source();
        // GUI-subsystem attribute present (column-0 inner attr); matched
        // leniently so the exact cfg predicate can still evolve.
        let attr_present = src.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("#![cfg_attr(") && t.contains("windows_subsystem")
        });
        assert!(
            attr_present,
            "the GUI-subsystem crate attribute was removed; without it Windows \
             re-allocates a phantom console on every Start-menu / Explorer \
             launch (the flash this fix eliminated). Restore the crate-root \
             `#![cfg_attr(all(windows, not(test)), windows_subsystem = ...)]`."
        );
        // The conditional parent-console attach is present.
        for needle in [
            "fn attach_parent_console_if_needed()",
            "AttachConsole(ATTACH_PARENT_PROCESS)",
        ] {
            assert!(
                src.contains(needle),
                "missing console-attach token: {needle}"
            );
        }
        // The inherited-handle guard (the fix for the earlier unconditional-reopen
        // regression) is present: without
        // GetFileType + GetStdHandle there is no way to tell a piped stdout
        // from an allocated console, so an unconditional reopen would re-break
        // `kettle --flag | grep` on Windows CI.
        for needle in [
            "GetFileType(h)",
            "GetStdHandle(STD_OUTPUT_HANDLE)",
            "if out_ok && err_ok {",
        ] {
            assert!(
                src.contains(needle),
                "missing inherited-handle guard token: {needle}"
            );
        }
        // The stdin-specific guard: `attach_parent_console_if_needed` must
        // also skip reopening CONIN$ when stdin is already an inherited pipe
        // (`echo y | kettle update`), the same shape of bug the out/err
        // guard above was added to fix. Without `if !in_ok` gating the
        // CONIN$ reopen, any terminal launch that needs an out/err reopen
        // (i.e. most terminal launches) unconditionally clobbers a piped
        // stdin with the parent console's keyboard input.
        for needle in ["let in_ok = is_inherited(stdin_handle);", "if !in_ok {"] {
            assert!(
                src.contains(needle),
                "missing stdin inherited-handle guard token: {needle} \
                 (piped stdin would be silently clobbered by CONIN$ reopen)"
            );
        }
        // Belt-and-suspenders: the earlier console-hide hack must be gone —
        // under the GUI subsystem there is no auto-console to hide, and a stray
        // hide could hide the user's *parent* console after attach. The needle
        // is built at runtime so this assertion doesn't self-match via
        // include_str!.
        let hide_call = format!("ShowWindow(hwnd, {})", "SW_HIDE");
        assert!(
            !src.contains(&hide_call),
            "the console-hide call is back; remove it — under the GUI \
             subsystem there is no auto-allocated console to hide."
        );
    }

    #[test]
    fn config_path_problem_catches_missing_and_directory() {
        use std::io::Write;
        // Missing path → "no such file" (preserved from the original check).
        let missing = std::path::PathBuf::from("/definitely/not/a/real/path/kettle.conf");
        assert_eq!(config_path_problem(&missing), Some("no such file"));

        // Real temp dir: `--config DIR` was the not-a-regular-file gap. Pre-fix,
        // `--config ~/.config/kettle` (where the file is `.config/kettle/config`
        // and the user dropped the trailing component) silently fell back to
        // defaults — `read_to_string` returned IsADirectory, `load_from_with_diagnostics`
        // logged a warn and used defaults, and the user saw their carefully-
        // crafted theme nowhere with no obvious cue why.
        // PID + nanos. Stale directories from a previously
        // panicked test run (Ctrl+C, OOM, hardware fault) used to
        // collide with a re-run sharing the same PID — common on
        // Windows where PIDs cycle quickly and rare-but-real on Linux
        // CI runners. The nanos suffix means even the same PID gets a
        // fresh dir. Matches the pattern in session::tests +
        // config_tests + the bg-image / lua config test fixtures.
        let tmp = std::env::temp_dir().join(format!(
            "kettle-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(config_path_problem(&tmp), Some("not a regular file"));

        // Real regular file inside the temp dir → acceptable (None).
        let file = tmp.join("config");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(b"theme = TokyoNight Night\n")
            .unwrap();
        assert_eq!(config_path_problem(&file), None);

        // Unreadable file (perm-denied) is rejected at the
        // CLI surface so the runtime doesn't silently fall back to
        // defaults. Skip on Windows / CI users where chmod-000 doesn't
        // actually deny read to the calling user.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let unreadable = tmp.join("unreadable.conf");
            std::fs::File::create(&unreadable)
                .unwrap()
                .write_all(b"theme = TokyoNight Night\n")
                .unwrap();
            std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
            // The check should now flag it. Root bypasses unix perms,
            // so only assert when we actually can't open it ourselves
            // — running CI as root would otherwise spuriously fail
            // the test.
            if std::fs::File::open(&unreadable).is_err() {
                assert_eq!(
                    config_path_problem(&unreadable),
                    Some("not readable (permission denied or I/O error)"),
                );
            }
            // Restore perms so the cleanup remove can succeed.
            let _ = std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644));
            let _ = std::fs::remove_file(&unreadable);
        }

        // Cleanup so a re-run of the suite starts fresh.
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn print_completions_emits_per_shell_scripts() {
        // clap_complete's output is per-shell shaped.
        // Pin the contract: each known shell emits a non-trivial
        // script that mentions kettle's command name. A regression
        // (e.g. someone passes the wrong Shell variant, or the
        // generator silently emits empty) would otherwise only
        // surface for a user trying to actually use the completion.
        //
        // Run via the same code path as `main` — `Cli::command()` +
        // `clap_complete::generate`.
        use clap::CommandFactory;
        use clap_complete::Shell;
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let mut cmd = Cli::command();
            let mut out: Vec<u8> = Vec::new();
            clap_complete::generate(shell, &mut cmd, "kettle", &mut out);
            let text = String::from_utf8(out).expect("utf8");
            assert!(
                text.len() > 200,
                "{shell:?} completion is too small ({} bytes) — \
                 generator likely degraded",
                text.len()
            );
            assert!(
                text.contains("kettle"),
                "{shell:?} completion doesn't mention `kettle` — \
                 wrong command name plumbed through"
            );
        }
    }

    #[test]
    fn shell_integration_snippets_match_in_tree_files() {
        // `kettle --shell-integration <shell>` emits one
        // of the embedded `shell-integration/kettle.{bash,zsh,fish,ps1}`
        // files (ps1 added later). The contract: the embedded
        // content must equal the in-tree file byte-for-byte (so
        // docs/SHELL-INTEGRATION.md and `--shell-integration` never
        // diverge) and each snippet must include the OSC 133 prefix
        // (catches an accidental truncated include_str! at build
        // time). The substring check uses `]133;` (without the
        // escape prefix) so it matches bash/zsh/fish (`\033]133;` /
        // `\e]133;` literals) AND the PowerShell snippet (which uses
        // `[char]27 + ']133;X'` + `[char]7` — no escape prefix in
        // the source text).
        for (shell, embedded) in [
            (
                "bash",
                include_str!("../../../shell-integration/kettle.bash"),
            ),
            ("zsh", include_str!("../../../shell-integration/kettle.zsh")),
            (
                "fish",
                include_str!("../../../shell-integration/kettle.fish"),
            ),
            ("ps1", include_str!("../../../shell-integration/kettle.ps1")),
        ] {
            assert!(
                embedded.contains("OSC 133") && embedded.contains("]133;"),
                "{shell}: embedded snippet missing OSC 133 marker — \
                 the file's body probably regressed"
            );
            // v2.20.0: every snippet also reports the cwd via OSC 7 (powers
            // new-tab/split cwd inheritance + "Open folder").
            assert!(
                embedded.contains("]7;file://"),
                "{shell}: embedded snippet missing the OSC 7 cwd report"
            );
            assert!(
                embedded.lines().count() >= 10,
                "{shell}: snippet has only {} lines — likely empty \
                 include_str!",
                embedded.lines().count()
            );
        }
    }

    /// The guide installs the canonical snippets generated by the binary. It
    /// must not grow a second copy of their function bodies: duplicated shell
    /// code drifted before, while the one-line install route stays tied to the
    /// `include_str!` sources checked above.
    #[test]
    fn documentation_installs_the_canonical_shell_snippets() {
        let doc = include_str!("../../../docs/SHELL-INTEGRATION.md");
        for command in [
            "kettle --shell-integration bash       >> ~/.bashrc",
            "kettle --shell-integration zsh        >> ~/.zshrc",
            "kettle --shell-integration fish       >> ~/.config/fish/config.fish",
            "kettle --shell-integration powershell >> $PROFILE",
        ] {
            assert!(
                doc.contains(command),
                "missing canonical install: {command}"
            );
        }
        for copied_body in [
            "__kettle_pc() {",
            "__kettle_osc7() {",
            "kettle_completion_show() {",
            "kettle_completion_clear() {",
            "function __kettle_prompt",
            "function __kettle_completion_cycle",
            "function global:__kettle_completion_emit",
            "function global:__kettle_completion_cycle",
        ] {
            assert!(
                !doc.contains(copied_body),
                "the guide copied a shell implementation that can drift: {copied_body}"
            );
        }
    }

    #[test]
    fn print_default_config_round_trip() {
        // `kettle --print-default-config` emits the
        // embedded `docs/kettle.example.config`. The first-launch
        // bootstrap is:
        //   kettle --print-default-config > ~/.config/kettle/config
        // Pin the contract that:
        //   1. The embedded content is non-trivial (≥ 50 lines) so
        //      we catch an accidental empty include_str! at build
        //      time rather than at "ship time".
        //   2. It is a valid kettle config — Config::parse_collect
        //      reports zero unknown-key / malformed-value
        //      diagnostics. Everything in the example file is
        //      commented out by convention,
        //      so the only requirement is the parser accepts it.
        let embedded = include_str!("../../../docs/kettle.example.config");
        assert!(
            embedded.lines().count() >= 50,
            "embedded example config has only {} lines — \
             include_str! probably stale",
            embedded.lines().count()
        );
        let (_, diags) = kettle_config::Config::parse_collect(embedded);
        assert!(
            diags.is_empty(),
            "embedded example config emits diagnostics: {diags:?}"
        );
        // Drift guard: the example config must document the extended
        // configuration surface. If a
        // future contributor strips the section, this test catches it
        // before users see a stripped-down `--print-default-config`
        // output.
        //
        // This includes window accents, a global bell override, and output
        // triggers.
        for key in &[
            "window-state",
            "borderless",
            "always-on-top",
            "show-titlebar",
            "title-at-bottom",
            "background-image",
            "background-image-mode",
            "exit-action",
            "lua-sandbox",
            "accent-color",
            "force-no-bell",
            "trigger",
        ] {
            assert!(
                embedded.contains(key),
                "embedded example config missing extended key {key:?}"
            );
        }
    }

    #[test]
    fn format_ssh_hosts_sorts_and_aligns_columns() {
        // Empty case: explicit message rather than an empty Vec (so the
        // CLI prints something the user can see, not silence).
        assert_eq!(
            format_ssh_hosts(&[]),
            vec!["(no ssh-host entries configured)".to_string()]
        );
        // Three rows, intentionally out of order, with varying name lengths.
        let hosts = vec![
            ("box".to_string(), "me@box.example.com".to_string()),
            ("a".to_string(), "u@h".to_string()),
            ("work-vpn".to_string(), "admin@10.0.0.5".to_string()),
        ];
        let out = format_ssh_hosts(&hosts);
        // Sorted alphabetically by name.
        assert_eq!(
            out,
            vec![
                "a         u@h".to_string(),
                "box       me@box.example.com".to_string(),
                "work-vpn  admin@10.0.0.5".to_string(),
            ]
        );
        // Column width = longest name (`work-vpn` = 8) — minimum 4 for
        // short-name configs. Use a tiny single-row case to pin the floor.
        let tiny = vec![("a".to_string(), "u@h".to_string())];
        let out = format_ssh_hosts(&tiny);
        // Floor: 4 chars + two-space separator = "a   " + "  " + "u@h".
        assert_eq!(out, vec!["a     u@h".to_string()]);
    }

    #[test]
    fn cli_exec_and_working_directory_parse() {
        let c = Cli::try_parse_from([
            "kettle",
            "--config",
            "/etc/k.conf",
            "-d",
            "/tmp",
            "-e",
            "ssh",
            "-t",
            "box",
        ])
        .expect("valid args");
        assert_eq!(
            c.working_directory.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert_eq!(
            c.config.as_deref(),
            Some(std::path::Path::new("/etc/k.conf"))
        );
        // `-e` consumes the rest, including hyphenated flags for the program.
        assert_eq!(c.exec, vec!["ssh", "-t", "box"]);
        // Defaults: no overrides.
        let d = Cli::try_parse_from(["kettle"]).unwrap();
        assert!(d.exec.is_empty() && d.working_directory.is_none() && d.config.is_none());
    }

    /// `--screenshot` and `--screenshot-menu` are mutually
    /// exclusive — passing both now fails loudly instead of silently dropping one.
    #[test]
    fn cli_screenshot_flags_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "kettle",
                "--screenshot",
                "a.png",
                "--screenshot-menu",
                "b.png"
            ])
            .is_err(),
            "both screenshot flags must conflict"
        );
        // Either one alone still parses.
        assert!(Cli::try_parse_from(["kettle", "--screenshot", "a.png"]).is_ok());
        assert!(Cli::try_parse_from(["kettle", "--screenshot-menu", "b.png"]).is_ok());
    }

    #[test]
    fn cli_help_text_has_no_internal_cycle_refs() {
        // `--help` is the very first contact most users have with the CLI.
        // Internal engineering-note parentheticals in rustdoc-style comments
        // helped trace history during development but leak as
        // mysterious-looking parentheticals when piped to a real terminal
        // user. That history lives in CHANGELOG and code comments; the
        // user-facing help text should not.
        //
        // Walk every argument's long+short help string and assert none
        // contain "cycle " — same shape as the
        // `defaults_has_no_shadow_collisions` drift guard, but for the
        // CLI's user-facing surface instead of the keybind defaults.
        use clap::CommandFactory;
        let cmd = Cli::command();
        for arg in cmd.get_arguments() {
            for txt in arg
                .get_help()
                .iter()
                .map(|s| s.to_string())
                .chain(arg.get_long_help().iter().map(|s| s.to_string()))
            {
                assert!(
                    !txt.to_ascii_lowercase().contains("cycle "),
                    "internal `cycle N` ref leaked into --help text for {:?}: {txt:?}",
                    arg.get_id(),
                );
            }
        }
        // Same for the top-level about/long-about strings.
        let about = cmd.get_about().map(|s| s.to_string()).unwrap_or_default();
        let long = cmd
            .get_long_about()
            .map(|s| s.to_string())
            .unwrap_or_default();
        for txt in [about, long] {
            assert!(
                !txt.to_ascii_lowercase().contains("cycle "),
                "internal `cycle N` ref leaked into --help about text: {txt:?}",
            );
        }
    }

    /// The hand-written man page must document every
    /// `--<long>` flag and must not leak internal `cycle N` refs. An earlier
    /// version of the page was missing `--check-update` + `--write-default-config`
    /// and carried cycle parentheticals precisely because the only man-page
    /// guard checked keybinds, not flags. Walk the complete clap command tree
    /// (including every subcommand) and pin both.
    #[test]
    fn man_page_documents_every_flag_without_cycle_refs() {
        use clap::CommandFactory;
        let man = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/kettle.1"),
        )
        .expect("kettle.1 present");
        assert!(
            !man.to_ascii_lowercase().contains("cycle "),
            "internal `cycle N` ref leaked into the man page"
        );
        // Internal/handoff-only flags + ones documented by their short form
        // (`--exec` is documented as `-e`) that the man page intentionally omits.
        // The `--record*` flags are now a shipped, runtime feature and MUST be
        // documented in the man page (see docs/RECORDING.md), so they are no
        // longer excluded here.
        let allow_missing: &[&str] = &["tab-handoff", "tab-handoff-fd", "exec"];
        fn collect_missing_flags(
            cmd: &clap::Command,
            path: &str,
            man: &str,
            allow_missing: &[&str],
            missing: &mut Vec<String>,
        ) {
            for long in cmd
                .get_arguments()
                .filter_map(|arg| arg.get_long())
                .filter(|long| !allow_missing.contains(long))
            {
                // troff escapes the leading `--` as `\-\-`; internal hyphens may
                // be plain (`\-\-config-path`) or escaped (`\-\-write\-default\-
                // config`). Accept either, plus the bare `--flag` used in examples.
                let prefix_escaped = format!("\\-\\-{long}");
                let all_escaped = format!("\\-\\-{}", long.replace('-', "\\-"));
                let plain = format!("--{long}");
                if !man.contains(&prefix_escaped)
                    && !man.contains(&all_escaped)
                    && !man.contains(&plain)
                {
                    missing.push(format!("{path} --{long}"));
                }
            }
            for subcommand in cmd.get_subcommands() {
                let child_path = format!("{path} {}", subcommand.get_name());
                collect_missing_flags(subcommand, &child_path, man, allow_missing, missing);
            }
        }

        let cmd = Cli::command();
        let mut missing = Vec::new();
        collect_missing_flags(&cmd, "kettle", &man, allow_missing, &mut missing);
        assert!(
            missing.is_empty(),
            "man page (packaging/linux/kettle.1) is missing flags: {missing:?}"
        );
    }

    #[test]
    fn cli_help_preserves_indented_code_examples() {
        // A `#[arg(...)]` whose doc-comment contains an indented `  kettle …`
        // example must declare `verbatim_doc_comment` — otherwise clap
        // collapses the leading spaces in `--help`, flattening the example
        // back into prose. The original fixes covered
        // --shell-integration and --print-completions; --print-default-config
        // had the same indented-example pattern and the
        // same wrapping bug, which is what this guard pins.
        //
        // Same shape as `cli_help_text_has_no_internal_cycle_refs` directly
        // above: walk the clap-built `Cli::command()`, pull each flag's
        // `get_long_help()`, assert the indented example survives literally.
        use clap::CommandFactory;
        let cmd = Cli::command();
        // Arg IDs match the struct field name, not the kebab-cased long flag.
        let expected = [
            ("shell_integration", "  kettle --shell-integration bash"),
            ("print_completions", "  kettle --print-completions bash"),
            ("print_default_config", "  kettle --print-default-config > "),
        ];
        for (id, needle) in expected {
            let arg = cmd
                .get_arguments()
                .find(|a| a.get_id().as_str() == id)
                .unwrap_or_else(|| panic!("no --{id} arg in CLI"));
            let long = arg
                .get_long_help()
                .map(|s| s.to_string())
                .unwrap_or_default();
            assert!(
                long.contains(needle),
                "--{id} lost the indented example {needle:?} — likely missing \
                 `verbatim_doc_comment` on its #[arg(...)] attribute. \
                 Rendered long_help:\n{long}",
            );
        }
    }

    #[test]
    fn man_page_documents_load_bearing_default_keybinds() {
        // Drift guard. The hand-written `kettle.1` man
        // page documents the default keybind set. An earlier audit caught four
        // entries that had drifted from the actual defaults (`Ctrl+Shift+
        // arrow` was a scroll binding, not focus; `Ctrl+Shift+Z` /
        // `Ctrl+Shift+D` weren't default-bound at all). This guard pins
        // the man page against `--list-keybinds`'s ground truth so the
        // next time a default-keybind set changes (or the man page text
        // gets edited carelessly), CI fails instead of a user trying
        // `man kettle` + the documented hotkey getting a different
        // action.
        //
        // Check shape: every "load-bearing" (Trigger, Action) the default
        // config carries must have its Trigger string textually present
        // somewhere in the man page. We don't require the *Action* name
        // to appear — the man page uses human-readable prose ("new tab",
        // not `NewTab`). And we don't enforce the full keybind set —
        // the man page intentionally summarizes; binding additions
        // shouldn't fail the guard just because the doc didn't grow.
        //
        // The load-bearing list is the set of bindings a user typically
        // hits in the first hour: tab management, splits, focus, copy/
        // paste, scrollback movement, broadcast. If you add a NEW load-
        // bearing default and forget to document it, this test fails.
        const MAN_PAGE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/linux/kettle.1"
        ));
        // The keybind triggers we expect documented. Strings are the
        // `Trigger`'s canonical display form as `kettle --list-keybinds`
        // prints them — see `kettle_config::keybinds::Trigger::Display`.
        let load_bearing: &[&str] = &[
            // Tabs
            "Ctrl+Shift+T", // NewTab
            "Ctrl+Shift+W", // ClosePane
            "Ctrl+PgUp",    // PrevTab (or accept "Ctrl+PageUp")
            "Ctrl+PgDn",    // NextTab
            // Splits
            "Ctrl+Shift+O", // SplitDown
            "Ctrl+Shift+E", // SplitRight
            // Focus
            "Alt+", // FocusUp / Down / Left / Right (any one suffices)
            // Overlays
            "Ctrl+Shift+K", // CommandPalette
            "Ctrl+Shift+F", // StartSearch
            "Ctrl+Shift+H", // HintMode
            "Ctrl+Shift+S", // OpenSsh
            // Clipboard
            "Ctrl+Shift+C", // Copy
            "Ctrl+Shift+V", // Paste
            // Scrollback
            "Ctrl+Up",   // JumpPrevPrompt
            "Ctrl+Down", // JumpNextPrompt
            // Zoom
            "Ctrl+Shift+X", // ToggleZoom
            // Broadcast
            "Super+G", // ToggleBroadcastAll
            // Vi-mode. The Ctrl+Shift+Space entry point
            // is load-bearing — without it, vi-mode users can't
            // enter the mode at all. h/j/k/l are mentioned in the
            // man page but not pinned here (they're the de-facto
            // vi keys, swapping them would be a deliberate config
            // override rather than a doc drift).
            "Ctrl+Shift+Space", // ToggleViMode
        ];
        let mut missing: Vec<&str> = Vec::new();
        for trigger in load_bearing {
            if !MAN_PAGE.contains(trigger) {
                missing.push(trigger);
            }
        }
        assert!(
            missing.is_empty(),
            "man page is missing load-bearing default keybinds: {missing:?}\n\
             Update packaging/linux/kettle.1 to document them (or change \
             this guard if a binding was intentionally removed)."
        );
    }

    /// Reverse half of the keybind documentation guard: every global chord the
    /// Linux man page advertises must exist in Linux's default keymap. Modal vi
    /// keys are intentionally ignored; they are mode-local commands rather than
    /// entries in `keybinds::defaults()`.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn man_page_does_not_advertise_unbound_global_chords() {
        use std::collections::HashSet;

        const MAN_PAGE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/linux/kettle.1"
        ));

        fn expand_chord_row(row: &str) -> Vec<String> {
            let row = row.replace("PgUp", "PageUp").replace("PgDn", "PageDown");
            if row.eq_ignore_ascii_case("Alt+arrow") {
                return ["Up", "Down", "Left", "Right"]
                    .into_iter()
                    .map(|key| format!("Alt+{key}"))
                    .collect();
            }
            if row.eq_ignore_ascii_case("Shift+arrow") {
                return ["Up", "Down", "Left", "Right"]
                    .into_iter()
                    .map(|key| format!("Shift+{key}"))
                    .collect();
            }
            if let Some((first, last)) = row.split_once(" ... ")
                && let (Some(first_digit), Some(last_digit)) =
                    (first.chars().last(), last.chars().last())
                && first_digit.is_ascii_digit()
                && last_digit.is_ascii_digit()
            {
                let prefix = &first[..first.len() - first_digit.len_utf8()];
                return (first_digit..=last_digit)
                    .map(|digit| format!("{prefix}{digit}"))
                    .collect();
            }
            if let Some((first, second_key)) = row.split_once('/') {
                let prefix = first
                    .rsplit_once('+')
                    .map(|(mods, _)| format!("{mods}+"))
                    .unwrap_or_default();
                return vec![first.to_string(), format!("{prefix}{second_key}")];
            }
            vec![row]
        }

        let defaults: HashSet<String> = kettle_config::keybinds::defaults()
            .keys()
            .map(kettle_config::Trigger::label)
            .collect();
        let key_section = MAN_PAGE
            .split(".SH KEY BINDINGS")
            .nth(1)
            .expect("KEY BINDINGS section")
            .split("\n.SH ")
            .next()
            .expect("end of KEY BINDINGS section");
        let mut unbound = Vec::new();
        for row in key_section
            .lines()
            .filter_map(|line| line.strip_prefix(".B "))
        {
            let is_global_chord = row.contains('+')
                || row.contains('/')
                || row
                    .strip_prefix('F')
                    .is_some_and(|number| number.chars().all(|c| c.is_ascii_digit()));
            if !is_global_chord || row.contains("\\-") {
                continue;
            }
            for chord in expand_chord_row(row) {
                if !defaults.contains(&chord) {
                    unbound.push(chord);
                }
            }
        }
        assert!(
            unbound.is_empty(),
            "Linux man page advertises chords absent from keybinds::defaults(): {unbound:?}"
        );
    }

    #[test]
    fn extra_check_config_lines_empty_for_default_config() {
        // Drift guard. The default config produces no
        // opt-in echo lines so `kettle --check-config` stays terse
        // for the common case (just the base summary + `status: OK`).
        // A future change that adds a noisy default-fires echo line
        // would regress this contract.
        let cfg = kettle_config::Config::default();
        let lines = extra_check_config_lines(&cfg);
        assert_eq!(
            lines,
            Vec::<String>::new(),
            "default config emitted echo lines: {lines:?}"
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn extra_check_config_lines_surface_each_opt_in_key() {
        // Drift guard. Each opt-in echo branch
        // fires for its specific opt-in field; setting each one
        // independently should produce exactly the expected line.
        let mut cfg = kettle_config::Config::default();
        cfg.accent_color = Some(kettle_config::Rgb {
            r: 0x00,
            g: 0xd4,
            b: 0xff,
        });
        let lines = extra_check_config_lines(&cfg);
        assert!(
            lines.iter().any(|l| l == "accent:  #00d4ff"),
            "accent line missing: {lines:?}"
        );

        let mut cfg = kettle_config::Config::default();
        cfg.force_no_bell = true;
        assert!(
            extra_check_config_lines(&cfg)
                .iter()
                .any(|l| l.starts_with("bell:    force-no-bell=true"))
        );

        // Triggers branch — verifies pluralization renders correctly.
        let mut cfg = kettle_config::Config::default();
        cfg.triggers.push(kettle_config::OutputTrigger {
            pattern: "error:.*".into(),
            action: kettle_config::TriggerAction::Urgency,
        });
        cfg.triggers.push(kettle_config::OutputTrigger {
            pattern: "warning:.*".into(),
            action: kettle_config::TriggerAction::Urgency,
        });
        assert!(
            extra_check_config_lines(&cfg)
                .iter()
                .any(|l| l == "triggers: 2 pattern(s) configured (window-urgency action)")
        );

        let mut cfg = kettle_config::Config::default();
        cfg.lua_sandbox = kettle_config::LuaSandbox::Trusted;
        assert!(
            extra_check_config_lines(&cfg)
                .iter()
                .any(|l| l == "lua:     sandbox=Trusted")
        );

        let mut cfg = kettle_config::Config::default();
        cfg.background_image = "/tmp/wp.jpg".into();
        assert!(
            extra_check_config_lines(&cfg)
                .iter()
                .any(|l| l.starts_with("bg-image: /tmp/wp.jpg"))
        );

        let mut cfg = kettle_config::Config::default();
        cfg.borderless = true;
        assert!(
            extra_check_config_lines(&cfg)
                .iter()
                .any(|l| l.starts_with("window-flags: ") && l.contains("borderless=true"))
        );

        let mut cfg = kettle_config::Config::default();
        cfg.status_bar = kettle_config::StatusBarMode::Bottom;
        assert!(
            extra_check_config_lines(&cfg)
                .iter()
                .any(|l| l == "status-bar: Bottom")
        );
    }

    #[test]
    fn check_config_inert_line_covers_permanent_noops() {
        let cfg = kettle_config::Config::parse_text(
            "cursor-color-default = true\nhttp-proxy = http://proxy.invalid\n\
             enabled-plugins = example\naudible-bell = true\n",
        );
        assert_eq!(
            extra_check_config_lines(&cfg),
            vec![
                "inert:   cursor-color-default, http-proxy, enabled-plugins, audible-bell \
                 (accepted and validated, but kettle does not act on them)"
                    .to_string()
            ]
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn extra_check_config_lines_no_internal_cycle_refs() {
        // Drift guard. `kettle --check-config` output is
        // user-facing — internal "cycle N" / "cycle-N" references
        // shouldn't leak into it (same anti-pattern the
        // `user_facing_docs_have_no_internal_cycle_refs` guard catches in
        // markdown docs, but for binary runtime output). An earlier fix
        // caught one in the triggers echo (a stray cycle-number stamp in
        // the trigger-action text) that the markdown file-scan didn't reach.
        //
        // Build a cfg that triggers EVERY echo branch + assert no
        // resulting line matches "cycle " or "cycle-" followed by
        // a digit.
        let mut cfg = kettle_config::Config::default();
        cfg.accent_color = Some(kettle_config::Rgb {
            r: 0,
            g: 0xd4,
            b: 0xff,
        });
        cfg.force_no_bell = true;
        cfg.triggers.push(kettle_config::OutputTrigger {
            pattern: "error:.*".into(),
            action: kettle_config::TriggerAction::Urgency,
        });
        cfg.lua_sandbox = kettle_config::LuaSandbox::Trusted;
        cfg.background_image = "/tmp/wp.jpg".into();
        cfg.borderless = true;
        cfg.status_bar = kettle_config::StatusBarMode::Bottom;
        for line in extra_check_config_lines(&cfg) {
            let lower = line.to_ascii_lowercase();
            for needle in ["cycle ", "cycle-"] {
                if let Some(pos) = lower.find(needle)
                    && let Some(next) = lower.as_bytes().get(pos + needle.len())
                    && next.is_ascii_digit()
                {
                    panic!("internal cycle ref leaked into --check-config output: {line:?}");
                }
            }
        }
    }

    /// Drift guard. `scripts/menu-screenshot.sh` is the
    /// repro harness for the context-menu screenshot work —
    /// `just menu-shot` and the CONTRIBUTING workflow both depend on
    /// it being checked in, executable, and pointing at the right
    /// kettle binary. Pin the contract:
    ///   1. file exists at the expected path.
    ///   2. executable bit set (chmod +x in source control).
    ///   3. opens with the conventional bash shebang.
    ///   4. references both `scrot` and `xdotool` (so a refactor that
    ///      accidentally drops one of the load-bearing tools fails
    ///      here instead of at runtime on a contributor's machine).
    ///
    /// Gated `#[cfg(unix)]` because the test uses
    /// `std::os::unix::fs::PermissionsExt::mode()` for the
    /// executable-bit check (Windows has no equivalent — NTFS doesn't
    /// have a Unix-style mode word). Before this guard was added, this
    /// test failed compilation on Windows MSVC builds with E0433 "cannot
    /// find `unix` in `os`". Caught locally on a Windows 11 test pass;
    /// the fix matches the same `#[cfg(unix)]` pattern the
    /// `config_path_problem_catches_missing_and_directory` unreadable-config
    /// test already uses for an equivalent unix-only chmod check.
    #[cfg(unix)]
    #[test]
    fn scripts_menu_shot_exists_and_executable() {
        use std::os::unix::fs::PermissionsExt;
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/menu-screenshot.sh");
        let md = std::fs::metadata(&p).unwrap_or_else(|e| {
            panic!("missing repro harness at {}: {e}", p.display());
        });
        assert!(md.is_file(), "expected file at {}", p.display());
        let mode = md.permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "{} not executable (mode={:o}); run `chmod +x` and re-commit",
            p.display(),
            mode
        );
        let text = std::fs::read_to_string(&p).expect("read harness");
        assert!(
            text.starts_with("#!/usr/bin/env bash") || text.starts_with("#!/bin/bash"),
            "harness must open with a bash shebang"
        );
        assert!(
            text.contains("scrot") && text.contains("xdotool"),
            "harness must reference both scrot + xdotool"
        );
    }

    /// `open_remote_command_file` must not rely on the process umask: the
    /// remote-command file carries literal `--remote-send TEXT` payloads, so
    /// it must come out owner-only (0600), inside newly created owner-only
    /// (0700) parent directories. A pre-existing untrusted writable ancestor
    /// is rejected rather than chmodded through a caller-controlled path.
    /// Gated `#[cfg(unix)]` for the same reason as
    /// `scripts_menu_shot_exists_and_executable` above: `PermissionsExt::mode`
    /// doesn't exist on non-Unix targets.
    #[cfg(unix)]
    #[test]
    fn open_remote_command_file_is_permission_restricted() {
        use super::open_remote_command_file;
        use std::os::unix::fs::PermissionsExt;
        // PID + nanos, matching the collision-avoidance pattern used by
        // `config_path_problem_catches_missing_and_directory` above.
        let base = std::env::temp_dir().join(format!(
            "kettle-remote-cmd-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // A nested, not-yet-created parent dir — exercises the
        // `create_dir_all`-equivalent path, not just the file mode.
        let nested_parent = base.join("nested").join("kettle");
        let path = nested_parent.join("remote.cmd");

        // The existing anchor is trusted. Missing descendants are created
        // relative to verified directory handles and restored to exact 0700.
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();

        let file = open_remote_command_file(&path).expect("open_remote_command_file");
        drop(file);

        let dir_mode = std::fs::metadata(&nested_parent)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "remote-command parent dir must be owner-only (0700), got {dir_mode:o}"
        );
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            file_mode, 0o600,
            "remote-command file must be owner-only (0600), got {file_mode:o}"
        );

        // Re-opening an already-existing file (created before this fix, or
        // widened by some external umask) must also be tightened back down —
        // `OpenOptions::mode` only applies to a newly-created inode, so the
        // explicit `set_permissions` after `open` is load-bearing here.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let file = open_remote_command_file(&path).expect("re-open existing file");
        drop(file);
        let reopened_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            reopened_mode, 0o600,
            "re-opening a pre-existing wider-mode file must re-tighten it to 0600, got {reopened_mode:o}"
        );

        let untrusted = base.with_extension("untrusted");
        std::fs::create_dir_all(&untrusted).unwrap();
        std::fs::set_permissions(&untrusted, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = open_remote_command_file(&untrusted.join("nested/remote.cmd"))
            .expect_err("world-writable ancestor must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            !untrusted.join("nested").exists(),
            "rejection must happen before mutating an untrusted ancestor"
        );

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&untrusted);
    }
}
