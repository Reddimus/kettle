//! kettle — a fast, cross-platform GPU terminal emulator.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "kettle",
    version,
    about = "A fast cross-platform GPU terminal emulator"
)]
struct Cli {
    /// List every bundled theme and exit.
    #[arg(long)]
    list_themes: bool,

    /// Print the default keymap (trigger → action) and exit.
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

    /// Use this config file instead of the default path
    /// (`--config FILE`); also honored by `--check-config`/`--screenshot`.
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

    // Explicit `--config PATH` that doesn't exist is *almost certainly*
    // a typo (the user wanted a specific file, not the default). Every
    // downstream branch silently fell back to `Config::default()` in
    // that case — the user got a screenshot / table / window with
    // their carefully-crafted theme nowhere in sight and no clue why.
    // Hard-fail before any branch runs so the error lands exactly
    // where the typo is. Omitting `--config` (relying on the default
    // path) still falls back silently — that's the intended "kettle
    // works out of the box" behavior.
    if let Some(p) = &cli.config
        && !p.exists()
    {
        return Err(anyhow::anyhow!("--config {}: no such file", p.display()));
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
        let (cfg, unknown, malformed) = match &path {
            // Share `load_from_with_diagnostics` with the reload path so the
            // two diagnostic sources can't drift on which lints they run.
            Some(p) if p.exists() => kettle_config::Config::load_from_with_diagnostics(p),
            _ => (kettle_config::Config::default(), Vec::new(), Vec::new()),
        };
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
    use super::{Cli, format_ssh_hosts};
    use clap::Parser;

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
}
