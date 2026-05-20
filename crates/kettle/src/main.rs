//! kettle — a fast, cross-platform GPU terminal emulator.

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

    /// Validate the config (resolved settings + unknown-key warnings).
    #[arg(long)]
    check_config: bool,

    /// Render a representative frame offscreen to a PNG and exit (no window).
    #[arg(long, value_name = "PATH")]
    screenshot: Option<std::path::PathBuf>,

    /// Columns for `--screenshot` (default 96).
    #[arg(long, default_value_t = 96)]
    cols: u32,

    /// Rows for `--screenshot` (default 28).
    #[arg(long, default_value_t = 28)]
    rows: u32,

    /// Use this config file instead of the default path. Honored by every
    /// introspection command (`--check-config`, `--list-keybinds`,
    /// `--list-ssh-hosts`, `--screenshot`, `--config-path`) as well as the
    /// windowed run. The path must be an existing regular file: a missing
    /// path is a hard error, and so is a directory (typing `--config
    /// ~/.config/kettle` when you meant the file inside it). The
    /// out-of-the-box default-path fallback only kicks in when this flag
    /// is omitted entirely.
    #[arg(long = "config", value_name = "FILE")]
    config: Option<std::path::PathBuf>,

    /// Working directory for the first tab (`-d DIR`).
    #[arg(long = "working-directory", short = 'd', value_name = "DIR")]
    working_directory: Option<std::path::PathBuf>,

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

fn main() -> anyhow::Result<()> {
    reset_sigpipe();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
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
    if let Some(p) = &cli.config
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
    if cli.list_ssh_hosts {
        // Companion to --check-config (which reports a count) and the
        // Ctrl+Shift+S launcher (which lists them in-window): users
        // configuring a bunch of hosts wanted to verify the parse
        // *from the CLI* without launching kettle. Same `--config FILE`
        // override convention as the rest of the introspection
        // commands; falls back to the default config path.
        let cfg = match cli
            .config
            .clone()
            .or_else(kettle_config::Config::default_path)
        {
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
        let lines = match cli
            .config
            .clone()
            .or_else(kettle_config::Config::default_path)
        {
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
        match cli
            .config
            .clone()
            .or_else(kettle_config::Config::default_path)
        {
            Some(p) => println!("{}", p.display()),
            None => println!("(no config path resolvable)"),
        }
        return Ok(());
    }
    if cli.check_config {
        let path = cli
            .config
            .clone()
            .or_else(kettle_config::Config::default_path);
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
        let (cfg, unknown, mut malformed) = match &path {
            Some(p) if p.exists() => match std::fs::read_to_string(p) {
                Ok(text) => {
                    let (cfg, unknown) = kettle_config::Config::parse_collect(&text);
                    let malformed = kettle_config::Config::detect_malformed_values(&text);
                    (cfg, unknown, malformed)
                }
                Err(e) => {
                    read_error = Some(format!(
                        "could not read {}: {e} (using defaults)",
                        p.display()
                    ));
                    (kettle_config::Config::default(), Vec::new(), Vec::new())
                }
            },
            _ => (kettle_config::Config::default(), Vec::new(), Vec::new()),
        };
        if let Some(e) = &read_error {
            malformed.push(e.clone());
        }
        // Cycle 194: lead with the kettle build version + git SHA, so a
        // user pasting `--check-config` output into a bug report doesn't
        // also need to run `--version` separately. Matches the
        // diagnostic-first-line convention `cargo --version`-style tools
        // use in their support flags.
        println!("kettle:  {KETTLE_VERSION}");
        match &path {
            Some(p) if p.exists() => println!("config:  {}", p.display()),
            Some(p) => println!("config:  {} (not found — using defaults)", p.display()),
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
        println!(
            "bell:    {:?}  osc52: {:?}  min-contrast: {}",
            cfg.bell, cfg.osc52, cfg.minimum_contrast
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
        let issues = unknown.len() + malformed.len();
        if issues == 0 {
            println!("status:  OK — no issues");
            return Ok(());
        }
        println!("status:  {issues} issue(s):");
        for k in &unknown {
            println!("  - unknown key: {k}");
        }
        for k in &malformed {
            println!("  - malformed value: {k}");
        }
        std::process::exit(1);
    }

    if let Some(out) = &cli.screenshot {
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
                    "--screenshot {}: extension .{e} not supported; \
                     only .png is built in",
                    out.display()
                ));
            }
            None => {
                return Err(anyhow::anyhow!(
                    "--screenshot {}: missing .png extension",
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
        let cfg = match cli
            .config
            .clone()
            .or_else(kettle_config::Config::default_path)
        {
            Some(p) if p.exists() => kettle_config::Config::load_from(&p),
            _ => kettle_config::Config::default(),
        };
        // Clamp dimensions to a sane range — wgpu textures cap at 8192 px
        // per side on most GPUs, so a typo like `--cols 100000` used to
        // panic with `dimension X exceeds the limit of 8192` instead of
        // producing a friendly error. Worst-case cell size ~20 px wide /
        // ~40 px tall keeps 400×200 cells comfortably under the limit;
        // every realistic screenshot fits.
        let cols = cli.cols.clamp(20, 400);
        let rows = cli.rows.clamp(8, 200);
        // `capture_png` may shrink (cols, rows) further to fit the GPU
        // texture limit at the active font size; show what was actually
        // rendered, with a hint when it differs from the request so the
        // user notices their cli args didn't fully apply.
        let (actual_cols, actual_rows) = kettle_render::capture_png(&cfg, cols, rows, out)?;
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

    kettle_ui::run_with(kettle_ui::Options {
        command: (!cli.exec.is_empty()).then_some(cli.exec),
        cwd: cli.working_directory,
        config: cli.config,
    })
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
mod tests {
    use super::{Cli, config_path_problem, format_ssh_hosts};
    use clap::Parser;

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
        let tmp = std::env::temp_dir().join(format!("kettle-cycle164-{}", std::process::id()));
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
}
