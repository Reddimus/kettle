use clap::Parser;
use std::ffi::OsString;

/// Version string shown by `kettle --version`. Concatenates the
/// `Cargo.toml` version with the git SHA captured by `build.rs` (or
/// the empty string when we're not in a git checkout — source
/// tarballs, vendored builds), so the output is one of:
///
/// - `kettle 0.1.0 (a1b2c3d4e5f6)` — git checkout, sha12 in parens.
/// - `kettle 0.1.0` — non-git build; concat with an empty string
///   leaves the version pristine.
pub(crate) const KETTLE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("KETTLE_GIT_SHA"));
#[derive(Parser, Debug)]
#[command(
    name = "kettle",
    version = KETTLE_VERSION,
    about = "A fast cross-platform GPU terminal emulator"
)]
pub(crate) struct Cli {
    /// List every bundled theme and exit.
    #[arg(long)]
    pub(crate) list_themes: bool,

    /// Print the keymap (trigger → action) and exit. Honors `--config FILE`
    /// to show the *effective* keymap after overrides + unbinds; without it,
    /// shows the built-in defaults.
    #[arg(long)]
    pub(crate) list_keybinds: bool,

    /// Print every accepted action name (for `keybind = trigger=action`) and exit.
    #[arg(long)]
    pub(crate) list_actions: bool,

    /// Print configured `ssh-host = name=target` entries (Ctrl+Shift+S launcher) and exit.
    #[arg(long)]
    pub(crate) list_ssh_hosts: bool,

    /// List saved layouts (`<config-dir>/layouts/*.json`) and exit. Launch one
    /// with `kettle --layout NAME`.
    #[arg(long)]
    pub(crate) list_layouts: bool,

    /// List named config profiles (`<config-dir>/profiles/*.config`) and exit.
    /// Launch one with `kettle --profile NAME`.
    #[arg(long)]
    pub(crate) list_profiles: bool,

    /// Print the resolved config path and exit.
    #[arg(long)]
    pub(crate) config_path: bool,

    /// Print the configured backend policy, resolved adapter, driver, and
    /// texture limits Kettle would use, then exit. Useful for filing a
    /// "blank window" / "no GPU adapter" bug report without a windowed run.
    #[arg(long)]
    pub(crate) gpu_info: bool,

    /// Check the authenticated stable feed for a newer kettle release and print
    /// the result, then exit. Bypasses the background throttle and policy; use
    /// `kettle update` to install an eligible official build.
    #[arg(long)]
    pub(crate) check_update: bool,

    /// Download and install the latest authenticated stable release. This is a
    /// convenience alias for `kettle update`; interactive confirmation is still
    /// required. Automation should use `kettle update --yes`.
    #[arg(long)]
    pub(crate) update: bool,

    /// Validate the config (resolved settings + unknown-key warnings).
    #[arg(long)]
    pub(crate) check_config: bool,

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
    pub(crate) print_default_config: bool,

    /// Write the documented default config to your config path, creating the
    /// parent directory if needed, then exit. Unlike the
    /// `--print-default-config > FILE` redirect (which fails on a fresh
    /// install when the directory doesn't exist yet, and on PowerShell 5.1
    /// writes an unreadable UTF-16 file), this creates everything for you and
    /// refuses to overwrite an existing config. Honors `--config` / `--profile`.
    #[arg(long, verbatim_doc_comment)]
    pub(crate) write_default_config: bool,

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
    /// Kettle starts the default Unix shell as a login shell. Bash reads the first
    /// readable `~/.bash_profile`, `~/.bash_login`, or `~/.profile`, not `~/.bashrc`
    /// automatically. If that active login startup file does not already source
    /// `~/.bashrc`, add this line to it (create `~/.bash_profile` only if none exists):
    ///
    ///   [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
    ///
    /// Start a new shell (for example, open a new Kettle pane) after installing.
    ///
    /// Snippets live at `shell-integration/kettle.{bash,zsh,fish,ps1}`
    /// in the source tree and are embedded at build time so the
    /// binary always emits the version that shipped with it
    /// (`cargo install kettle` users included). The powershell variant
    /// covers Windows PowerShell 5.1+ and cross-platform PowerShell Core.
    #[arg(long, value_name = "SHELL", verbatim_doc_comment)]
    pub(crate) shell_integration: Option<String>,

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
    pub(crate) print_completions: Option<String>,

    /// Render a representative frame offscreen to a PNG and exit (no window).
    // Mutually exclusive with --screenshot-menu so passing
    // both fails loudly (clap rejects symmetrically + it shows in --help) rather
    // than silently dropping one.
    #[arg(long, value_name = "PATH", conflicts_with = "screenshot_menu")]
    pub(crate) screenshot: Option<std::path::PathBuf>,

    /// Render like `--screenshot` but with a synthetic right-click
    /// context menu open over the rendered pane. Useful for verifying
    /// the menu's render path without opening the windowed app, and
    /// for visual-regression tests in CI. Honors `--cols` / `--rows`
    /// / `--config` the same as `--screenshot`. PNG-only.
    #[arg(long, value_name = "PATH")]
    pub(crate) screenshot_menu: Option<std::path::PathBuf>,

    /// Caption text to place at the bottom of `--screenshot` or
    /// `--screenshot-menu` output. Useful for docs and bug reports. Example:
    ///
    ///   kettle --screenshot doc.png --annotate "kettle v1.3.x — TokyoNight Night"
    #[arg(long, value_name = "TEXT", verbatim_doc_comment)]
    pub(crate) annotate: Option<String>,

    /// Columns for `--screenshot` (default 96).
    #[arg(long, default_value_t = 96)]
    pub(crate) cols: u32,

    /// Rows for `--screenshot` (default 28).
    #[arg(long, default_value_t = 28)]
    pub(crate) rows: u32,

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
    pub(crate) config: Option<std::path::PathBuf>,

    /// Working directory for the first tab (`-d DIR`).
    #[arg(long = "working-directory", short = 'd', value_name = "DIR")]
    pub(crate) working_directory: Option<std::path::PathBuf>,

    /// Start a separate Kettle process even when a primary process of this
    /// build is available. Any launch with explicit arguments already starts
    /// separately; this flag is the explicit escape hatch for an otherwise
    /// default launch.
    #[arg(long)]
    pub(crate) new_process: bool,

    /// Internal native reopen handoff.
    #[arg(long, hide = true)]
    pub(crate) reopen: bool,

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
    pub(crate) layout: Option<String>,

    /// Restore the previous session (tabs, splits, working dirs) for this
    /// launch. kettle opens a FRESH window by default (a single pane in the
    /// default cwd), like every mainstream terminal; pass `--restore` for a
    /// one-shot "continue where I left off" without setting
    /// `restore-session = true` in your config.
    #[arg(long, verbatim_doc_comment)]
    pub(crate) restore: bool,

    /// Enable kettle's agent control server for this launch, overriding the
    /// `agent-server` config. `off` (no server), `read-only` (read the screen /
    /// list panes / subscribe), or `full` (also send text + run commands). The
    /// server is a local-IPC surface `kettle ctl` / `kettle mcp` / an AI agent
    /// can drive — OFF by default. See docs/AGENT.md.
    #[arg(long, value_name = "MODE", verbatim_doc_comment)]
    pub(crate) agent_server: Option<AgentServerArg>,

    /// Let agents show media in kettle for this launch, overriding the
    /// `agent-display` config. Display grants no screen reads or input.
    /// `--agent-server off` also turns display off unless this flag is given.
    #[arg(long, value_name = "on|off", verbatim_doc_comment)]
    pub(crate) agent_display: Option<OnOffArg>,

    /// Deprecated receive-only compatibility for a JSON tab handoff written by
    /// an older Kettle process. Current tab tear-off moves the live tab,
    /// running programs, PTY, and scrollback in-process. The legacy handoff file
    /// is consumed once and deleted.
    #[arg(long, value_name = "PATH", verbatim_doc_comment)]
    pub(crate) tab_handoff: Option<std::path::PathBuf>,

    /// Deprecated receive-only compatibility for an SCM_RIGHTS tab handoff
    /// written by an older Kettle process. Current tab tear-off moves the live
    /// tab in-process. Unix-only.
    #[arg(long, value_name = "FD", verbatim_doc_comment)]
    pub(crate) tab_handoff_fd: Option<i32>,

    /// Record this session to an asciicast-compatible trace at PATH (replays
    /// with `asciinema play`). Captures terminal output, resizes, keystroke
    /// *tokens* (never raw typed characters by default), and UI markers. Off
    /// unless requested; also honored via the `KETTLE_RECORD` env var and the
    /// persistent `record`/`record-dir` config keys. Output is captured
    /// verbatim — review a trace before sharing it. See docs/RECORDING.md.
    #[arg(long, value_name = "PATH", verbatim_doc_comment)]
    pub(crate) record: Option<std::path::PathBuf>,

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
    pub(crate) record_dir: Option<std::path::PathBuf>,

    /// With --record, capture RAW typed characters instead of redacted key
    /// tokens. WARNING: the trace can then contain typed passwords; leave it off
    /// unless you need byte-exact input. The window title shows `[REC RAW]` while
    /// active. Also honored via `KETTLE_RECORD_RAW_INPUT` / `record-raw-input`.
    #[arg(long, verbatim_doc_comment)]
    pub(crate) record_raw_input: bool,

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
    pub(crate) profile: Option<String>,

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
    pub(crate) accent: Option<String>,

    /// Maximise the window at launch. Overrides the `window-state` config for
    /// this launch. `--maximize` is an accepted alias.
    #[arg(long = "maximise", short = 'm', visible_alias = "maximize")]
    pub(crate) maximise: bool,

    /// Fullscreen the window at launch. Overrides `window-state` for this launch.
    #[arg(long, short = 'f')]
    pub(crate) fullscreen: bool,

    /// Launch without window borders or decorations. Overrides `borderless`.
    #[arg(long, short = 'b')]
    pub(crate) borderless: bool,

    /// Launch hidden. Pair with a global hotkey bound to `kettle --toggle` for
    /// a dropdown window.
    #[arg(long = "hidden", short = 'H')]
    pub(crate) hidden: bool,

    /// Force the window title for this launch, overriding `window-title-format`.
    #[arg(long = "title", short = 'T', value_name = "TEXT")]
    pub(crate) title: Option<String>,

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
    pub(crate) toggle: bool,

    /// Send TEXT to a running kettle via the remote-command file
    /// (default `<config-dir>/remote.cmd`) and exit. Used by
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
    /// default path). When several processes watch the same file,
    /// the first process to claim the shared spool receives the batch
    /// and writes TEXT to its focused pane. Use distinct `--remote-file`
    /// paths for deterministic instance targeting.
    /// TEXT is transported exactly after shell argument parsing; Kettle
    /// does not reinterpret backslash escapes. Current senders use a
    /// reversible JSON-string frame, while receivers retain the legacy
    /// lossy `send-text` line format for direct writers.
    /// The shared spool is capped at 1 MiB; a send that would exceed
    /// its remaining capacity fails without changing queued commands.
    /// A receiver claims at most 1,024 operations as one batch and
    /// rejects an over-limit batch before dispatching any prefix.
    /// Delivery is at most once; a receiver failure after claiming a batch
    /// can lose the undispatched suffix.
    #[arg(long, value_name = "TEXT", verbatim_doc_comment)]
    pub(crate) remote_send: Option<String>,

    /// Remote-command file path. Default
    /// `<config-dir>/remote.cmd`. Honored by both the
    /// `--remote-send` sender side and the kettle window's
    /// notify-watcher receiver side; both must agree on the path.
    /// Give each receiving process a distinct path for deterministic
    /// instance targeting, and use that path for its senders.
    /// See `--remote-send` above for the usage example.
    #[arg(long, value_name = "PATH")]
    pub(crate) remote_file: Option<std::path::PathBuf>,

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
    pub(crate) lua_script: Option<std::path::PathBuf>,

    /// Run this command in the first tab instead of the shell, e.g.
    /// `kettle -e htop` or `kettle -e ssh box`. Consumes the rest of the
    /// arguments (hyphenated flags for the program are passed through).
    #[arg(short = 'e', long = "exec", num_args = 1.., allow_hyphen_values = true, value_name = "CMD")]
    pub(crate) exec: Vec<String>,

    /// Agent-first subcommands. `kettle exec` runs a command
    /// headlessly under a real PTY and streams its output to stdout (no GUI);
    /// `kettle ctl` / `kettle mcp` drive a running kettle programmatically.
    /// All of kettle's existing flags are the GUI launcher; the subcommands
    /// are the non-interactive / control surface.
    #[command(subcommand)]
    pub(crate) cmd: Option<Cmd>,
}

/// `--agent-server MODE` values.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum AgentServerArg {
    Off,
    ReadOnly,
    Full,
}

/// An `on|off` flag value.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnOffArg {
    On,
    Off,
}

/// Agent-first subcommands. Each is a self-contained non-GUI entry point that
/// returns early from `main` before any winit/GPU work.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum Cmd {
    #[command(name = "__media-preview-worker", hide = true)]
    MediaPreviewWorker,
    /// Run a command under a real PTY, headlessly, and stream its output to
    /// stdout (the non-interactive counterpart to the GUI). Propagates the
    /// child's exit code; 124 when `--timeout` expires and owned-process
    /// teardown is verified, 74 on stdout delivery failure, 125 on an internal
    /// error or unverified teardown. On Unix, SIGTERM/SIGINT cancel the run
    /// with bounded tree teardown and exit 130 (125 if unverified).
    Exec(ExecArgs),
    /// Drive a running kettle's agent control server: call a method (e.g.
    /// `list_panes`, `read_screen`, `screenshot`, `send_text`, `run_command`) or stream
    /// events. The target kettle must run with `agent-server` enabled.
    Ctl(CtlArgs),
    /// Run a Model Context Protocol server over stdio, exposing kettle as native
    /// agent tools. Register with Claude Code: `claude mcp add kettle -- kettle mcp`.
    Mcp(McpArgs),
    /// Send a local image, SVG, Mermaid diagram, Markdown diagram gallery, PDF
    /// or video (shown as its poster) to the media shelf of the pane this runs
    /// in, in the Kettle it runs inside.
    /// Without a path, or
    /// with -, read it from stdin (Mermaid up to 64 KiB with --mermaid, any
    /// other up to what one request carries; pass a larger one as a file).
    /// The item waits on the shelf; nothing opens on screen. Agent previews
    /// must be on; full control is never needed.
    Show(ShowArgs),
    /// Read a video's or animation's frames as one JPEG contact sheet, for a
    /// model to inspect, and print which time each frame shows. The file is
    /// opened by this command and decoded in a media worker it starts; no
    /// Kettle window is involved. Nine evenly spaced frames by default;
    /// narrow the window with --start and --end, or take one frame with
    /// --at. Showing media to the user is `kettle show`.
    VideoFrames(VideoFramesArgs),
    /// Set up Codex to start with Kettle's display server: print a `codex`
    /// shell function to review and add to your shell's startup file, report
    /// its status, or print how to remove it. Kettle edits no file.
    AgentSetup(SetupArgs),
    /// Install the latest authenticated stable release into an official
    /// installer-owned kettle layout.
    Update(UpdateArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct ShowArgs {
    /// The image, SVG, Mermaid, Markdown, PDF or video file to show, or -
    /// (the default) for stdin.
    #[arg(value_name = "PATH", allow_hyphen_values = true, default_value = "-")]
    pub(crate) source: std::path::PathBuf,
    /// Render it as Mermaid rather than by what its bytes turn out to be:
    /// stdin is then Mermaid text, and a file is rendered as a diagram.
    #[arg(long)]
    pub(crate) mermaid: bool,
    /// A title for the shelf item (default: the file name).
    #[arg(long, value_name = "TITLE")]
    pub(crate) title: Option<String>,
    /// Replace the item with this key on the same pane's shelf instead of
    /// adding another (default: the file's path).
    #[arg(long, value_name = "KEY")]
    pub(crate) key: Option<String>,
    /// Also print a card here, under the command, which opens the item when
    /// clicked. Only when this command writes straight to its pane's own
    /// terminal from the foreground; otherwise the item goes to the shelf
    /// alone and a note on stderr says why.
    #[arg(long)]
    pub(crate) inline: bool,
    /// Show page N of a PDF, counting from 1 (default: the first). Needs the
    /// file's path.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "mermaid")]
    pub(crate) page: Option<u32>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct VideoFramesArgs {
    /// The video or animation to read.
    #[arg(value_name = "PATH")]
    pub(crate) source: std::path::PathBuf,
    /// Where to write the JPEG.
    #[arg(short = 'o', long = "output", value_name = "OUT.jpg")]
    pub(crate) output: std::path::PathBuf,
    /// Where the frames start, in seconds (default: the start).
    #[arg(long, value_name = "SECONDS")]
    pub(crate) start: Option<f64>,
    /// Where the frames end, in seconds (default: the end).
    #[arg(long, value_name = "SECONDS")]
    pub(crate) end: Option<f64>,
    /// One frame at this time, in seconds, instead of a sheet.
    #[arg(long, value_name = "SECONDS", conflicts_with_all = ["start", "end", "count"])]
    pub(crate) at: Option<f64>,
    /// How many frames, 1 to 16 (default 9).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=16))]
    pub(crate) count: Option<u8>,
    /// The sheet's longer edge in pixels, up to 2560 (default 1568); larger
    /// to read text in a frame.
    #[arg(long, value_name = "PIXELS", value_parser = clap::value_parser!(u32).range(1..=2560))]
    pub(crate) max_edge: Option<u32>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct UpdateArgs {
    /// Confirm installation non-interactively.
    #[arg(long)]
    pub(crate) yes: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct McpArgs {
    /// Run an in-process self-test (initialize + tools/list + one kettle_run)
    /// and exit, instead of serving stdio. Used as a CI guard.
    #[arg(long)]
    pub(crate) self_test: bool,
    /// Offer only kettle_show, which sends media to the Kettle pane this runs
    /// in and needs agent previews, never full control.
    #[arg(long, conflicts_with = "self_test")]
    pub(crate) display: bool,
    /// Launched by Kettle's own Claude Code plugin, whose hook prints inline
    /// cards: only then does an interactive Claude Code session ask Kettle
    /// for one. Kettle still decides whether the caller may have it.
    #[arg(long, hide = true, requires = "display")]
    pub(crate) claude_card_hook: bool,
    /// Launched by Kettle's Codex launch, whose hook prints inline cards.
    /// Kettle still decides whether the caller may have one.
    #[arg(
        long,
        hide = true,
        requires = "display",
        conflicts_with = "claude_card_hook"
    )]
    pub(crate) codex_card_hook: bool,
    /// Native agy display adapter; its command hooks publish shelf previews.
    #[arg(long, hide = true, requires = "display", conflicts_with_all = ["claude_card_hook", "codex_card_hook"])]
    pub(crate) agy_display: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct CtlArgs {
    /// The method to call (`get_state`, `list_tabs`, `list_panes`,
    /// `read_screen`, `read_cells`, `ui_geometry`, `screenshot`, `send_text`,
    /// `send_keys`, `dispatch_ui_key`, `dispatch_keybind`, `send_mouse`, `resize_window`,
    /// `perform_action`, `wait_for`, `run_command`), `events` to stream the event feed,
    /// or `schema` to print the protocol's JSON Schema, which needs no running Kettle.
    pub(crate) method: String,
    /// Target a specific pane id (else the focused pane).
    #[arg(long)]
    pub(crate) pane: Option<u64>,
    /// Method parameters as a JSON object (merged with `--pane`).
    #[arg(long, value_name = "JSON")]
    pub(crate) json: Option<String>,
    /// Text for `send_text`, the command line for `run_command`, or the
    /// substring to wait for with `wait_for`. Hyphen-leading values are
    /// legitimate text (`--text "-- INSERT --"` waits for vim's mode line).
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) text: Option<String>,
    /// Comma-separated key tokens for `send_keys` or `dispatch_ui_key`, e.g.
    /// `--keys "escape,i,h,i,escape,ctrl+c"` (each segment is ONE key:
    /// a name like `escape`/`enter`/`f5`, a chord like `ctrl+c`, or a
    /// single character; a literal comma key is spelled `comma`, and
    /// `plus`/`minus`/`equal` name those characters).
    #[arg(long, value_name = "KEYS", allow_hyphen_values = true)]
    pub(crate) keys: Option<String>,
    /// Regex for `wait_for` (alternative/addition to `--text`).
    #[arg(long, value_name = "REGEX", allow_hyphen_values = true)]
    pub(crate) regex: Option<String>,
    /// Connect to a specific kettle pid (else the kettle this runs inside,
    /// then the one KETTLE_PID names, then the newest running server).
    #[arg(long)]
    pub(crate) pid: Option<u32>,
    /// Print the raw JSON result instead of a pretty summary.
    #[arg(long)]
    pub(crate) raw: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct ExecArgs {
    /// Terminal width in columns (default: probe the console, else 80).
    #[arg(long)]
    pub(crate) cols: Option<u16>,
    /// Terminal height in rows (default: probe the console, else 24).
    #[arg(long)]
    pub(crate) rows: Option<u16>,
    /// Working directory for the child (default: inherit). An explicit path
    /// must exist and be a directory; otherwise the child is not started.
    #[arg(short = 'd', long = "cwd", value_name = "DIR")]
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// Stop the complete run at this deadline and exit 124 when owned-process
    /// teardown is verified (125 otherwise), including while trailing PTY
    /// output is still being delivered (finite seconds, ≥ 0).
    #[arg(long, value_name = "SECS")]
    pub(crate) timeout: Option<f64>,
    /// Strip ANSI escape sequences — emit plain text (good for assertions).
    #[arg(long, conflicts_with = "json")]
    pub(crate) strip_ansi: bool,
    /// Emit one JSON object per line (start / output / title / exit events).
    #[arg(long)]
    pub(crate) json: bool,
    /// Also record the session to an asciicast (.cast) file.
    #[arg(long, value_name = "PATH")]
    pub(crate) record: Option<std::path::PathBuf>,
    /// Internal MCP transport: capture the bounded byte tail before emitting it.
    #[arg(long, hide = true, conflicts_with_all = ["json", "record"])]
    pub(crate) internal_capture_tail: bool,
    /// The command (and its arguments) to run.
    #[arg(required = true, num_args = 1.., allow_hyphen_values = true, value_name = "CMD", last = false, trailing_var_arg = true)]
    pub(crate) argv: Vec<String>,
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

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SetupAgent {
    #[default]
    Codex,
    Claude,
    Agy,
}

#[derive(clap::Args, Debug)]
#[command(group(clap::ArgGroup::new("action").args(["print", "status", "uninstall", "install"])))]
#[command(group(clap::ArgGroup::new("launch").args(["launch_codex", "launch_claude"])))]
pub(crate) struct SetupArgs {
    /// Print the selected agent's shell function, or agy's plugin install
    /// command. agy files are staged in Kettle's private cache. Shell startup
    /// files and agent configuration are never edited by --print.
    #[arg(long)]
    pub(crate) print: bool,
    /// Install Kettle's native agy plugin. Only valid with --agent agy.
    #[arg(long)]
    pub(crate) install: bool,
    /// Native agy command-hook adapter; not a user command.
    #[arg(long, hide = true, conflicts_with = "action")]
    pub(crate) agy_hook: Option<String>,
    /// Report whether Codex launched from this shell would get Kettle's
    /// display server, and how it delivers media.
    #[arg(long)]
    pub(crate) status: bool,
    /// Print how to remove the selected agent's function.
    #[arg(long)]
    pub(crate) uninstall: bool,
    /// Run by the printed function: start Codex with these arguments.
    #[arg(long, hide = true, conflicts_with = "action")]
    pub(crate) launch_codex: bool,
    #[arg(long, hide = true, conflicts_with = "action")]
    pub(crate) launch_claude: bool,
    /// The agent integration to print, inspect, install or remove.
    #[arg(long, value_enum, default_value_t = SetupAgent::Codex, conflicts_with_all = ["launch_codex", "launch_claude"])]
    pub(crate) agent: SetupAgent,
    /// The shell to print for (default: the one `SHELL` names).
    #[arg(long, value_enum, conflicts_with_all = ["status", "launch_codex", "launch_claude"])]
    pub(crate) shell: Option<SetupShell>,
    /// The agent's own arguments, after `--`.
    #[arg(last = true, allow_hyphen_values = true, requires = "launch")]
    pub(crate) argv: Vec<OsString>,
}
