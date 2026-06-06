//! kettle - a fast, cross-platform GPU terminal emulator.

// Cycle 734 vs 740 trade-off note: cycle 734 tried
// `#![cfg_attr(windows, windows_subsystem = "windows")]` + AttachConsole
// to suppress the Start-menu phantom console. That broke Windows
// CI's bash CLI smoke (`cargo run -q -- --some-flag | grep ...` -
// SUBSYSTEM:WINDOWS sends stdout to the parent console's screen
// buffer, NOT the inherited stdout pipe that bash's `|` reads). The
// `kettle.exe -- shell-integration powershell >> $PROFILE` pattern
// failed for the same reason (verified locally + reproduced in CI).
//
// Cycle 740 switches to the simpler Ghostty pattern: stay on the
// default `console` subsystem (so stdout pipe inheritance works
// correctly under PS / bash / cmd) and instead **hide the
// auto-allocated phantom console at startup ONLY when we are the
// only process attached to it** (i.e. Windows allocated it for us
// on Explorer / Start-menu launch and no shell is reading from it).
// `GetConsoleProcessList(1)` returns the count; if == 1, we hide
// the window via `ShowWindow(GetConsoleWindow(), SW_HIDE)`. If
// > 1, a parent shell is using this console - leave it visible
// so CLI output reaches the user.
//
// Trade-off: there is a sub-50ms console flash on Explorer launch
// (Windows shows the console before our hide call lands). The
// previous SUBSYSTEM:WINDOWS approach had zero flash but broke
// CLI stdout entirely. The flash is the correct trade — same
// pattern Ghostty + most other Win11 terminals use.
use clap::Parser;

/// Version string shown by `kettle --version`. Concatenates the
/// `Cargo.toml` version with the git SHA captured by `build.rs` (or
/// the empty string when we're not in a git checkout — source
/// tarballs, vendored builds), so the output is one of:
///
/// - `kettle 0.1.0 (a1b2c3d4e5f6)` — git checkout, sha12 in parens.
/// - `kettle 0.1.0` — non-git build; concat with an empty string
///   leaves the version pristine. Cycle 192.
const KETTLE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("KETTLE_GIT_SHA"));

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

    /// Print the resolved config path and exit.
    #[arg(long)]
    config_path: bool,

    /// Print the wgpu adapter / backend / driver / texture-limit
    /// details kettle would use on this machine, then exit. Useful
    /// for filing a "blank window" / "no GPU adapter" bug report
    /// without a windowed run.
    #[arg(long)]
    gpu_info: bool,

    /// Check GitHub for a newer kettle release and print the result, then exit.
    /// Bypasses the once/24h throttle the background check uses. Notify-only —
    /// kettle never downloads or installs; update via your package manager or
    /// the release page it points you to.
    #[arg(long)]
    check_update: bool,

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
    #[arg(long, value_name = "PATH")]
    screenshot: Option<std::path::PathBuf>,

    /// Render like `--screenshot` but with a synthetic right-click
    /// context menu open over the rendered pane. Useful for verifying
    /// the menu's render path without opening the windowed app, and
    /// for visual-regression tests in CI. Honors `--cols` / `--rows`
    /// / `--config` the same as `--screenshot`. PNG-only.
    #[arg(long, value_name = "PATH")]
    screenshot_menu: Option<std::path::PathBuf>,

    /// Caption text to overlay at the bottom of `--screenshot` /
    /// `--screenshot-menu` output (iTerm2 "annotated screenshot"
    /// parity). Useful for docs / README hero images / bug
    /// reports that want a version / repro / env note baked into
    /// the PNG. Example:
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
    /// `--list-ssh-hosts`, `--screenshot`, `--config-path`) as well as the
    /// windowed run. The path must be an existing, regular, readable
    /// file: a missing path is a hard error, a directory is a hard error
    /// (typing `--config ~/.config/kettle` when you meant the file inside
    /// it), and a permission-denied file is a hard error too. The
    /// out-of-the-box default-path fallback only kicks in when this flag
    /// is omitted entirely.
    #[arg(long = "config", value_name = "FILE")]
    config: Option<std::path::PathBuf>,

    /// Working directory for the first tab (`-d DIR`).
    #[arg(long = "working-directory", short = 'd', value_name = "DIR")]
    working_directory: Option<std::path::PathBuf>,

    /// Launch into a named layout (Terminator parity). Saves +
    /// restores from `<config-dir>/layouts/<NAME>.json` so a user
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

    /// Restore a tab from a JSON handoff file written by another
    /// kettle process. Used by Action::MoveTabToNewWindow on
    /// platforms without SCM_RIGHTS (Windows + Wayland) — the
    /// source process serializes the tab + passes the path;
    /// the target reads + reconstructs. Running shells stay in
    /// the source window (live PTY-fd transfer needs SCM_RIGHTS).
    #[arg(long, value_name = "PATH", verbatim_doc_comment)]
    tab_handoff: Option<std::path::PathBuf>,

    /// Receive a tab handoff via SCM_RIGHTS over the named file
    /// descriptor. Used by Action::MoveTabToNewWindow on Linux +
    /// macOS where SCM_RIGHTS is available — preserves live
    /// running shells across the window move (vs the file-handoff
    /// path which restarts shells). Unix-only.
    #[arg(long, value_name = "FD", verbatim_doc_comment)]
    tab_handoff_fd: Option<i32>,

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

    /// Toggle the running kettle window's visibility (Quake /
    /// Yakuake / Tilda dropdown UX) via the remote-control IPC and
    /// exit. Bind this to your compositor / DE / OS global hotkey
    /// to get a true dropdown terminal:
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
    /// external scripts to drive an already-open kettle without
    /// launching a new window (kitty `@ send-text` parity). Example:
    ///
    ///   kettle --remote-send 'ls -la\n'
    ///
    /// The receiving kettle window must have been launched with
    /// the same `--remote-file PATH` (or both omit it to use the
    /// default path). The text is written to the focused pane of
    /// the most-recently-launched kettle that's watching the file.
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

    /// Execute a Lua script at startup (WezTerm parity, foundation
    /// sub-cycle). The script runs once with a `kettle` global
    /// namespace exposing read-only introspection:
    ///
    ///   kettle.version()      → string, e.g. "1.7.x"
    ///   kettle.config_path()  → string|nil, the resolved config path
    ///   kettle.theme()        → string, the resolved theme name
    ///
    /// Subsequent sub-cycles add side-effect APIs (send_text,
    /// notify, event hooks). Errors in the script print to stderr
    /// (log::warn) but don't fail the kettle launch — same shape
    /// as malformed-config tolerance.
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

/// Cycle 741: compute where a crash report is written, in addition to
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

/// Cycle 741: install a `panic = "abort"`-safe panic hook as the very first
/// thing `main` does. Before this, a panic on a Start-menu launch was
/// invisible — the cycle-740 console-hide path swallows stderr, and
/// `panic = "abort"` (Cargo.toml) skips unwinding — so two prior cycles had
/// to *guess* at a crash's cause. The hook prints a full report (message,
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

        // Cycle 863 (audit): a fallible write, not `eprintln!`. With SIGPIPE at
        // SIG_IGN (Rust default), `eprintln!` to a broken stderr pipe panics —
        // and a panic inside the panic hook aborts immediately under
        // `panic = "abort"`, losing the crash-log write below. `writeln!` lets
        // us swallow the error and still persist the report.
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "{report}");
        }

        let path = crash_log_path(when, std::process::id(), |k| std::env::var(k).ok());
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write as _;
            let _ = f.write_all(report.as_bytes());
        }
    }));
}

fn main() -> anyhow::Result<()> {
    // Cycle 741: capture panics (message + backtrace) to stderr AND a crash
    // log under the state dir — must be first so even an early panic lands.
    install_panic_hook();
    // Cycle 740: hide the auto-allocated console window if it
    // belongs only to us (Start-menu / Explorer launch). When a
    // parent shell ran us, the console process list has > 1
    // entries and we leave the console visible so CLI output
    // (--version, --list-themes, --shell-integration, etc.) keeps
    // reaching the user. Same pattern Ghostty uses; explicitly
    // chosen over SUBSYSTEM:WINDOWS + AttachConsole (cycle 734's
    // approach) because that broke CI's bash-piped CLI smoke tests
    // (SUBSYSTEM:WINDOWS routes stdout to the console screen
    // buffer, not the inherited stdout pipe `|` reads).
    //
    // Trade-off: brief console flash on Explorer launch (Windows
    // shows it before our hide call runs). Sub-50ms in practice;
    // tolerable compared to broken CLI stdout.
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        // SAFETY: All three Win32 calls are safe at single-threaded
        // startup; they read/mutate only this process's own console
        // window state, not shared state with other processes.
        unsafe {
            let mut pids = [0u32; 2];
            let count = GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32);
            // count == 1 => we're alone on this console (Windows
            // allocated it for us at CreateProcess). Hide the
            // window so the user doesn't see a phantom console
            // alongside the wgpu window.
            // count == 0 => no console attached at all (shouldn't
            // happen under SUBSYSTEM:CONSOLE but defensive).
            // count > 1 => a parent shell shares the console;
            // leave it visible so CLI output flows.
            if count == 1 {
                let hwnd = GetConsoleWindow();
                if !hwnd.is_null() {
                    ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }
    reset_sigpipe();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    // Cycle 204: log the build identity at info level on startup. A user
    // grep'ing their stderr for warnings to file a bug report can paste
    // the surrounding lines — the version line lands once near the top,
    // disambiguating which kettle build emitted the warning. `info` level
    // is below the `warn` default filter, so the line only appears when
    // the user has bumped logging (`RUST_LOG=info kettle …`); on the
    // default filter it stays out of the way.
    log::info!("kettle {KETTLE_VERSION} starting");
    let cli = Cli::parse();

    // Explicit `--config PATH` must point at a regular file. Every
    // downstream branch silently fell back to `Config::default()`
    // otherwise — the user got a screenshot / table / window with
    // their carefully-crafted theme nowhere in sight and no clue why.
    //
    // Cycle 106 caught the "no such file" case. Cycle 164 extends the
    // check to *not a regular file* (typically a directory — a user
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
    // Cycle 801: skip the must-already-exist check when `--write-default-config`
    // is set — there `--config PATH` names the file to *create*, so a missing
    // path is the expected, valid case rather than a typo to reject.
    if !cli.write_default_config
        && let Some(p) = &cli.config
        && let Some(reason) = config_path_problem(p)
    {
        return Err(anyhow::anyhow!("--config {}: {reason}", p.display()));
    }
    // Same shape for `--working-directory DIR` (cycle 107). The engine
    // silently falls back to `$HOME` when the directory doesn't exist
    // (see `kettle_core::term::Terminal::new`: `Some(d) if is_dir =>
    // cmd.cwd(d)`, else HOME), so a typo'd `-d ~/projets` spawned the
    // shell in the user's home with no warning and no obvious cue that
    // the requested cwd was ignored. Hard-fail at the CLI surface
    // before the engine even runs; report whether the path is missing
    // (typo) or exists-but-isn't-a-directory (named a file by
    // mistake) so the user's fix is one keystroke away.
    if let Some(p) = &cli.working_directory {
        let kind = if !p.exists() {
            Some("no such file or directory")
        } else if !p.is_dir() {
            Some("not a directory")
        } else {
            None
        };
        if let Some(reason) = kind {
            return Err(anyhow::anyhow!(
                "--working-directory {}: {reason}",
                p.display()
            ));
        }
    }

    if cli.list_themes {
        for name in kettle_config::Theme::list() {
            println!("{name}");
        }
        return Ok(());
    }
    if cli.print_default_config {
        // Cycle 227: `kettle --print-default-config > ~/.config/kettle/config`
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
        // Cycle 801: the robust bootstrap. `--print-default-config > FILE`
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
        if path.exists() {
            println!(
                "config already exists at {} — leaving it untouched.",
                path.display()
            );
            println!("Delete it first if you want a fresh default, or edit it directly.");
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                anyhow::anyhow!("could not create config directory {}: {e}", dir.display())
            })?;
        }
        std::fs::write(&path, include_str!("../../../docs/kettle.example.config"))
            .map_err(|e| anyhow::anyhow!("could not write config {}: {e}", path.display()))?;
        println!("Wrote a default config to {}.", path.display());
        println!("Everything is commented out — uncomment what you want, then relaunch kettle.");
        return Ok(());
    }
    if let Some(shell) = cli.print_completions.as_deref() {
        // Cycle 237: clap_complete generates a shell-completion
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
        // Cycle 229: same shape as `--print-default-config`, but for
        // the OSC 133 shell-integration snippet. Embedded at build
        // time so `cargo install kettle` users (no source tree
        // accessible) get the right snippet, and so the binary's
        // output can never drift from the in-tree source of truth
        // under `shell-integration/`.
        //
        // Cycle 730: added PowerShell (alias `powershell` / `ps1` /
        // `pwsh`) so Windows users + cross-platform PowerShell Core
        // users get jump-to-prompt parity with bash/zsh/fish. Same
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
        // commands; falls back to the default config path. Cycle
        // 313: also honors `--profile NAME`.
        let cfg = match resolve_config_path(&cli) {
            Some(p) if p.exists() => kettle_config::Config::load_from(&p),
            _ => kettle_config::Config::default(),
        };
        for line in format_ssh_hosts(&cfg.ssh_hosts) {
            println!("{line}");
        }
        return Ok(());
    }
    if cli.list_actions {
        // Onboarding pair to `--list-keybinds`: that one shows what's
        // currently bound; this one shows what `keybind = trigger=…`
        // values are valid. Without this, users writing a new bind had
        // to grep the source or hit `--check-config` to confirm a name
        // they guessed. `goto_tab:N` is parametric, so it gets a
        // one-line tail blurb instead of an enumeration.
        for name in kettle_config::keybinds::action_names() {
            println!("{name}");
        }
        println!("goto_tab:N    (parametric; N is 1-based, 1..=255)");
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
        // Cycle 313: honor `--profile NAME` here too.
        let lines = match resolve_config_path(&cli) {
            Some(p) if p.exists() => {
                let cfg = kettle_config::Config::load_from(&p);
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
        // Cycle 313: honor `--profile NAME` here too.
        match resolve_config_path(&cli) {
            Some(p) => println!("{}", p.display()),
            None => println!("(no config path resolvable)"),
        }
        return Ok(());
    }
    if cli.gpu_info {
        // Resolves the same adapter / backend the live renderer +
        // --screenshot path would pick, so the output is faithful
        // to what the windowed run would see. No GUI / PTY needed.
        let info = kettle_render::gpu_info()?;
        println!("{info}");
        return Ok(());
    }
    if cli.check_update {
        // Cycle 794: one-shot deliberate check (no throttle, no event loop).
        println!("{}", kettle_ui::check_for_update_cli());
        return Ok(());
    }
    if cli.check_config {
        // Cycle 313: route through `resolve_config_path` so this
        // path honors `--profile NAME` uniformly with every other
        // introspection flag. Cycle 312 did the same inline; cycle
        // 313 extracts the helper because the same gap existed at
        // every other site.
        let path = resolve_config_path(&cli);
        // Cycle 196: surface read errors explicitly. Pre-fix,
        // `load_from_with_diagnostics` silently returned defaults on
        // any read error (permission denied, ENOENT-after-stat-race,
        // I/O error) — the warn went to stderr but `--check-config`'s
        // stdout said "status: OK" and exited 0, making the user
        // think their config loaded. Now: probe `read_to_string`
        // directly and turn a read failure into a malformed entry
        // so it lands in the issues list with a non-zero exit code.
        // Cycle 197 (cycle 196 follow-up): drive parse_collect /
        // detect_malformed_values directly from the text we already
        // read, rather than calling `load_from_with_diagnostics`
        // which reads the file a SECOND time internally. Cycle 196's
        // first take did the read twice (once for the error probe,
        // once inside load_from_with_diagnostics). Harmless but
        // wasteful; now the read happens once.
        let mut read_error: Option<String> = None;
        let (cfg, unknown, malformed) = match &path {
            Some(p) if p.exists() => match std::fs::read_to_string(p) {
                Ok(text) => {
                    let (cfg, unknown) = kettle_config::Config::parse_collect(&text);
                    let malformed = kettle_config::Config::detect_malformed_values(&text);
                    (cfg, unknown, malformed)
                }
                Err(e) => {
                    read_error = Some(format!("could not read {}: {e}", p.display()));
                    (kettle_config::Config::default(), Vec::new(), Vec::new())
                }
            },
            _ => (kettle_config::Config::default(), Vec::new(), Vec::new()),
        };
        // Cycle 194: lead with the kettle build version + git SHA, so a
        // user pasting `--check-config` output into a bug report doesn't
        // also need to run `--version` separately. Matches the
        // diagnostic-first-line convention `cargo --version`-style tools
        // use in their support flags.
        println!("kettle:  {KETTLE_VERSION}");
        match &path {
            Some(p) if p.exists() => println!("config:  {}", p.display()),
            Some(p) => {
                println!("config:  {} (not found — using defaults)", p.display());
                // Cycle 228: when no config exists at the resolved
                // default path, point the user at the bootstrap
                // one-liner. Without this, a newcomer who ran
                // `--check-config` and saw "using defaults" had to
                // know on their own that `--print-default-config`
                // (cycle 227) is the way to create one. The hint
                // names the actual resolved path so copy-paste works.
                println!("hint:    kettle --print-default-config > {}", p.display());
            }
            None => println!("config:  (no path resolvable — using defaults)"),
        }
        println!("theme:   {}", cfg.theme_name);
        println!("font:    {} {}pt", cfg.font_family, cfg.font_size);
        println!("scrollback: {}", cfg.scrollback);
        println!("keybinds: {} bound", cfg.keybinds.len());
        // Echo back the resolved values of the per-cycle config gates so
        // users can verify with `kettle --check-config` that their tweaks
        // are taking effect (rather than greping the source). Grouped by
        // theme of related settings; only one line per group for brevity.
        println!(
            "cursor:  {:?} (blink={}, interval={}ms)",
            cfg.cursor_style, cfg.cursor_blink, cfg.cursor_blink_interval
        );
        // Cycle 535: when force_no_bell silences every bell flavor
        // regardless of mode, annotate the existing line so the user
        // doesn't read "bell: Visual" while wondering why no bell
        // actually fires. The cycle-461 `extra_check_config_lines`
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
        // Cycles 461-470: echo Terminator-parity / cycle-295 opt-in
        // keys when the user has actually set them. Extracted as a
        // pure helper (`extra_check_config_lines`) so the contract
        // is unit-testable — without this, a user who set
        // `accent-color = #00d4ff` couldn't verify it parsed and
        // there'd be no regression test catching a future silent
        // drop. Symmetric with the lines above.
        for line in extra_check_config_lines(&cfg) {
            println!("{line}");
        }
        // Cycle 201: count and display I/O errors (cycle 196's read
        // failures) as their own category rather than reusing the
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

    // Both `--screenshot` and `--screenshot-menu` (cycle 251) share
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
        // Cycle 128.
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
        // Cycle 313: honor `--profile NAME` here too.
        let mut cfg = match resolve_config_path(&cli) {
            Some(p) if p.exists() => kettle_config::Config::load_from(&p),
            _ => kettle_config::Config::default(),
        };
        // Cycle 293: --accent CLI flag wins over the config
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

    // Cycle 303 Quake dropdown: `--toggle` is sugar for `--remote-send`
    // with a fixed `toggle-window` command. Same path-resolution
    // (--remote-file or default) so a user can bind their global
    // hotkey to `kettle --toggle` without any extra config.
    if cli.toggle {
        let path = cli
            .remote_file
            .clone()
            .or_else(default_remote_file)
            .ok_or_else(|| anyhow::anyhow!("could not resolve default remote-file path"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(b"toggle-window\n")?;
        return Ok(());
    }

    // Cycle 302 remote-control SENDER side. When `--remote-send TEXT`
    // is set, append TEXT (with trailing newline if missing) to the
    // remote-command file and exit without launching a window. The
    // running kettle that's watching the file picks up the line and
    // dispatches `send-text <REST>` to its focused pane.
    if let Some(text) = cli.remote_send.as_deref() {
        let path = cli
            .remote_file
            .clone()
            .or_else(default_remote_file)
            .ok_or_else(|| anyhow::anyhow!("could not resolve default remote-file path"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Each line is one command: `send-text <TEXT>\n`. Escape any
        // embedded newlines as `\\n` so a multi-line payload doesn't
        // get re-parsed as multiple commands. The receiver decodes
        // `\\n` back to `\n` before writing to the PTY.
        let encoded = text.replace('\n', "\\n");
        let line = format!("send-text {encoded}\n");
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;
        return Ok(());
    }

    // Resolve --profile if --config didn't override it. --config wins
    // when both are given so a user can quickly debug a profile
    // against an explicit config file.
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
    let remote_file = cli.remote_file.clone().or_else(default_remote_file);
    // Cycle 863 (audit): validate the internal handoff fd before it reaches
    // `UnixStream::from_raw_fd`. The source process always passes an inherited
    // descriptor >= 3; a negative value violates `from_raw_fd`'s safety
    // contract, and 0/1/2 would adopt stdio as a socket (and later `close` it).
    if let Some(fd) = cli.tab_handoff_fd
        && fd < 3
    {
        anyhow::bail!("--tab-handoff-fd: expected an inherited descriptor >= 3, got {fd}");
    }
    kettle_ui::run_with(kettle_ui::Options {
        command: (!cli.exec.is_empty()).then_some(cli.exec),
        cwd: cli.working_directory,
        config: config_path,
        layout: cli.layout,
        accent_override,
        remote_file,
        lua_script: cli.lua_script,
        tab_handoff: cli.tab_handoff,
        tab_handoff_fd: cli.tab_handoff_fd,
    })
}

/// Cycle 302: default remote-command file path. Lives under the
/// kettle config directory so `--remote-send` / `--remote-file`
/// callers and the kettle window's watcher agree without explicit
/// paths on either side. None when the config dir isn't resolvable
/// (no $HOME / $XDG_CONFIG_HOME) — same shape as
/// `Config::default_path`.
fn default_remote_file() -> Option<std::path::PathBuf> {
    kettle_config::Config::default_path().and_then(|p| p.parent().map(|d| d.join("remote.cmd")))
}

/// Cycle 313: resolve the effective config file path from
/// `--config FILE` / `--profile NAME` / the default path, in that
/// precedence. Used by every introspection flag (`--check-config`,
/// `--list-keybinds`, `--list-ssh-hosts`, `--config-path`,
/// `--screenshot`) so they all honor `--profile` uniformly.
///
/// Before this helper, only the windowed-run path (and as of cycle
/// 312, `--check-config`) honored `--profile`. A user running e.g.
/// `kettle --profile dev --list-keybinds` would silently get the
/// default config's keymap rather than the dev profile's — same
/// silent-fallback shape as cycle 196.
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
/// Cycle 198: also probe `File::open` so a permission-denied file fails
/// at the CLI surface instead of at the silent runtime fallback. Cycles
/// 106 (no such file), 164 (not a regular file), 198 (unreadable) cover
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

/// character names don't collapse the column), padded with two spaces.
/// Empty input yields a single "(no ssh-host entries configured)" line so
/// the user sees their config is empty rather than no output at all.
/// Pure so the formatting is unit-testable without the CLI.
/// Format the opt-in echo lines for `--check-config` (cycles 461-470).
/// Pure helper: takes a `Config`, returns one `String` per echo line
/// the user should see. Empty `Vec` for a default config — terse
/// default-summary output is the contract.
///
/// Adding a new branch: bump the doc list below, append the `if`,
/// and add the in-isolation assertion to
/// `extra_check_config_lines_surface_each_opt_in_key` (cycle 471).
/// The `extra_check_config_lines_empty_for_default_config` guard
/// will catch a branch that fires on default config.
///
/// Each variant gates on a single field's non-default-ness:
///   - `accent` (cycle 309) — `accent_color` is `Some`
///   - `bell: force-no-bell` (cycle 349) — `force_no_bell` is `true`
///   - `triggers` (cycle 289) — at least one trigger
///   - `lua: sandbox=...` (cycle 376) — `lua_sandbox != Safe`
///   - `bg-image` (cycles 380-396) — `background_image` non-empty
///   - `window-flags` (cycles 339/342) — any of window_state /
///     borderless / always_on_top is non-default
///   - `status-bar` (cycle 295) — `status_bar != Off`
fn extra_check_config_lines(cfg: &kettle_config::Config) -> Vec<String> {
    let mut lines = Vec::new();
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

#[cfg(test)]
mod crash_log_tests {
    use super::crash_log_path;

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
}

#[cfg(test)]
mod tests {
    use super::{Cli, config_path_problem, extra_check_config_lines, format_ssh_hosts};
    use clap::Parser;

    /// Cycle 740 drift guard (supersedes cycle-734's version).
    /// The fn main() startup needs both `GetConsoleProcessList` and
    /// `ShowWindow(GetConsoleWindow(), SW_HIDE)` calls — together
    /// they hide the auto-allocated console window when kettle was
    /// launched from Explorer / Start menu (no parent shell) while
    /// keeping the console visible when invoked from PowerShell /
    /// cmd / Git Bash so CLI flag output (--version, --list-themes,
    /// --shell-integration, etc.) reaches the user.
    ///
    /// If a future contributor strips this block (in a "this Win32
    /// dance looks weird, let's remove it" cleanup) the user-visible
    /// regression is: a phantom ConsoleWindowClass window opens
    /// alongside the wgpu window on every Start-menu launch. This
    /// test catches the strip at gauntlet time so the contributor
    /// reads the rationale in the panic message + the surrounding
    /// source comment first.
    ///
    /// Pre-740 this test asserted on cycle 734's
    /// `#![cfg_attr(windows, windows_subsystem = "windows")]`
    /// attribute, but that approach broke the bash-piped CLI smoke
    /// in CI (SUBSYSTEM:WINDOWS routes stdout to the console screen
    /// buffer, not the inherited stdout pipe that bash's `|` reads).
    #[test]
    fn windows_console_hide_on_orphan_launch_survives() {
        let src = include_str!("main.rs");
        // The two Win32 calls + cfg(windows) gate.
        assert!(
            src.contains("GetConsoleProcessList"),
            "the cycle-740 GetConsoleProcessList call in fn main() \
             was removed; without it, every Start-menu / Explorer \
             launch will show a phantom console alongside the wgpu \
             window. Restore the cfg(windows) block at the top of \
             fn main() (see surrounding source comment)."
        );
        assert!(
            src.contains("ShowWindow(hwnd, SW_HIDE)"),
            "the cycle-740 ShowWindow(SW_HIDE) call was removed; \
             without it, GetConsoleProcessList detects the orphan \
             console but kettle never hides it. Restore the \
             ShowWindow line in fn main()."
        );
        // Belt-and-suspenders: the cycle-734 GUI-subsystem attribute
        // must NOT be present (cycle 740 reverted it because it broke
        // the bash-piped CLI smoke in CI). A future contributor
        // re-adding it under the misimpression that GUI apps want it
        // would re-break stdout capture. Check by scanning lines for
        // an ACTIVE attribute (column-0 `#![cfg_attr(`); ignores
        // comments + this assert's own text.
        let attr_active = src
            .lines()
            .any(|line| line.starts_with("#![cfg_attr(windows, windows_subsystem"));
        assert!(
            !attr_active,
            "cycle 734's GUI-subsystem attribute was re-added at \
             crate root. Cycle 740 reverted it because it broke the \
             `cargo run -- --some-flag | grep ...` smoke tests on \
             Windows CI - stdout goes to the console screen buffer, \
             not the inherited stdout pipe bash's `|` reads. The \
             cycle-740 hide-on-orphan pattern below covers the \
             phantom-console concern without the stdout regression."
        );
    }

    #[test]
    fn config_path_problem_catches_missing_and_directory() {
        use std::io::Write;
        // Missing path → "no such file" (cycle 106 shape; preserved).
        let missing = std::path::PathBuf::from("/definitely/not/a/real/path/kettle.conf");
        assert_eq!(config_path_problem(&missing), Some("no such file"));

        // Real temp dir: `--config DIR` was the cycle 164 gap. Pre-fix,
        // `--config ~/.config/kettle` (where the file is `.config/kettle/config`
        // and the user dropped the trailing component) silently fell back to
        // defaults — `read_to_string` returned IsADirectory, `load_from_with_diagnostics`
        // logged a warn and used defaults, and the user saw their carefully-
        // crafted theme nowhere with no obvious cue why.
        // Cycle 593: PID + nanos. Stale directories from a previously
        // panicked test run (Ctrl+C, OOM, hardware fault) used to
        // collide with a re-run sharing the same PID — common on
        // Windows where PIDs cycle quickly and rare-but-real on Linux
        // CI runners. The nanos suffix means even the same PID gets a
        // fresh dir. Matches the pattern in session::tests +
        // config_tests + the cycle-592 bg_image / lua fixes.
        let tmp = std::env::temp_dir().join(format!(
            "kettle-cycle164-{}-{}",
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

        // Cycle 198: unreadable file (perm-denied) is rejected at the
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
        // Cycle 237: clap_complete's output is per-shell shaped.
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
        // Cycle 229: `kettle --shell-integration <shell>` emits one
        // of the embedded `shell-integration/kettle.{bash,zsh,fish,ps1}`
        // files (ps1 added in cycle 730). The contract: the embedded
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
            assert!(
                embedded.lines().count() >= 10,
                "{shell}: snippet has only {} lines — likely empty \
                 include_str!",
                embedded.lines().count()
            );
        }
    }

    #[test]
    fn print_default_config_round_trip() {
        // Cycle 227: `kettle --print-default-config` emits the
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
        //      commented out by convention (cycle 100 drift guard),
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
        // Cycle 413 drift guard: the example config MUST document the
        // Terminator-parity surface that cycles 331-410 added. If a
        // future contributor strips the section, this test catches it
        // before users see a stripped-down `--print-default-config`
        // output.
        //
        // Cycle 459: extended with accent-color (cycle 309 peacock
        // parity), force-no-bell (cycle 349 Terminator force_no_bell
        // parity), and trigger (cycle 290 regex-on-output → action).
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
                "embedded example config missing Terminator-parity key {key:?}; \
                 cycles 290/309/331-410 documented it"
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

    #[test]
    fn cli_help_text_has_no_internal_cycle_refs() {
        // `--help` is the very first contact most users have with the CLI.
        // Earlier cycles' rustdoc-style notes ("(cycle 103)", "(cycle 106)")
        // helped *me* trace audit history during development but leak as
        // mysterious-looking parentheticals when piped to a real terminal
        // user. The audit trail lives in CHANGELOG and code comments; the
        // user-facing help text should not.
        //
        // Walk every argument's long+short help string and assert none
        // contain "cycle " — same shape as cycle 116's
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

    /// Cycle 839 (audit): the hand-written man page must document every
    /// `--<long>` flag and must not leak internal `cycle N` refs. The pre-839
    /// page was missing `--check-update` + `--write-default-config` and carried
    /// cycle parentheticals precisely because the only man-page guard checked
    /// keybinds, not flags. Walk the clap CLI and pin both.
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
        let allow_missing: &[&str] = &["tab-handoff", "tab-handoff-fd", "exec"];
        let cmd = Cli::command();
        let missing: Vec<String> = cmd
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .filter(|long| !allow_missing.contains(long))
            .filter(|long| {
                // troff escapes the leading `--` as `\-\-`; internal hyphens may
                // be plain (`\-\-config-path`) or escaped (`\-\-write\-default\-
                // config`). Accept either, plus the bare `--flag` used in examples.
                let prefix_escaped = format!("\\-\\-{long}");
                let all_escaped = format!("\\-\\-{}", long.replace('-', "\\-"));
                let plain = format!("--{long}");
                !man.contains(&prefix_escaped)
                    && !man.contains(&all_escaped)
                    && !man.contains(&plain)
            })
            .map(|l| format!("--{l}"))
            .collect();
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
        // back into prose. The original cycle 229 / 237 fixes covered
        // --shell-integration and --print-completions; --print-default-config
        // (added by cycle 227) had the same indented-example pattern and the
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
        // Cycle 282 drift guard. The cycle-279 hand-written `kettle.1` man
        // page documents the default keybind set. Cycle 281 caught four
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
            // Vi-mode (cycle 298). The Ctrl+Shift+Space entry point
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

    #[test]
    fn extra_check_config_lines_empty_for_default_config() {
        // Cycle 471 drift guard. The default config produces no
        // opt-in echo lines so `kettle --check-config` stays terse
        // for the common case (just the base summary + `status: OK`).
        // A future cycle that adds a noisy default-fires echo line
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
        // Cycle 471 drift guard. Each cycle 461-470 echo branch
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
    #[allow(clippy::field_reassign_with_default)]
    fn extra_check_config_lines_no_internal_cycle_refs() {
        // Cycle 537 drift guard. `kettle --check-config` output is
        // user-facing — internal "cycle N" / "cycle-N" references
        // shouldn't leak into it (same anti-pattern the cycle-179
        // drift guard catches in markdown docs, but for binary
        // runtime output). Cycle 536 caught one in the triggers
        // echo ("cycle-289 Urgency action") that the cycle-179
        // file-scan didn't reach.
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

    /// Cycle 711 drift guard. `scripts/menu-screenshot.sh` is the
    /// repro harness for the C3-C9 context-menu sub-cycles —
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
    /// Cycle 730: gated `#[cfg(unix)]` because the test uses
    /// `std::os::unix::fs::PermissionsExt::mode()` for the
    /// executable-bit check (Windows has no equivalent — NTFS doesn't
    /// have a Unix-style mode word). Pre-730 this test failed
    /// compilation on Windows MSVC builds with E0433 "cannot find
    /// `unix` in `os`". Caught locally on the cycle-730 Windows 11
    /// audit; the fix matches the same `#[cfg(unix)]` pattern the
    /// cycle-198 unreadable-config test at `main.rs:1052` already
    /// uses for an equivalent unix-only chmod check.
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
}
