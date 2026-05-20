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

    if cli.list_themes {
        for name in kettle_config::Theme::list() {
            println!("{name}");
        }
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
        let cfg = match cli
            .config
            .clone()
            .or_else(kettle_config::Config::default_path)
        {
            Some(p) if p.exists() => {
                kettle_config::Config::parse_collect(&std::fs::read_to_string(p)?).0
            }
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
        kettle_render::capture_png(&cfg, cols, rows, out)?;
        println!("wrote {} ({cols}×{rows} cells)", out.display());
        return Ok(());
    }

    kettle_ui::run_with(kettle_ui::Options {
        command: (!cli.exec.is_empty()).then_some(cli.exec),
        cwd: cli.working_directory,
        config: cli.config,
    })
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

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
