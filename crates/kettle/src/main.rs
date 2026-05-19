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

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();

    if cli.list_themes {
        for name in kettle_config::Theme::list() {
            println!("{name}");
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
        let (cfg, unknown) = match &path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(p)?;
                kettle_config::Config::parse_collect(&text)
            }
            _ => (kettle_config::Config::default(), Vec::new()),
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
        if unknown.is_empty() {
            println!("status:  OK — no unrecognized keys");
            return Ok(());
        } else {
            println!("status:  {} unrecognized key(s):", unknown.len());
            for k in &unknown {
                println!("  - {k}");
            }
            std::process::exit(1);
        }
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
        kettle_render::capture_png(&cfg, cli.cols.max(20), cli.rows.max(8), out)?;
        println!("wrote {}", out.display());
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
