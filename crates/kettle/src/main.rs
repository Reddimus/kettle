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
        match kettle_config::Config::default_path() {
            Some(p) => println!("{}", p.display()),
            None => println!("(no config path resolvable)"),
        }
        return Ok(());
    }
    if cli.check_config {
        let path = kettle_config::Config::default_path();
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

    kettle_ui::run()
}
