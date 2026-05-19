//! kettle UI: the winit application, the tab/pane multiplexer, keyboard input
//! encoding, and the search overlay.

mod app;
mod input;
mod mux;
mod session;

pub use app::App;

/// First-tab startup overrides from the CLI.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Run this argv in the first tab instead of the shell (`kettle -e …`).
    pub command: Option<Vec<String>>,
    /// Working directory for the first tab (`kettle -d DIR`).
    pub cwd: Option<std::path::PathBuf>,
    /// Explicit config file (`kettle --config FILE`); `None` = default path.
    pub config: Option<std::path::PathBuf>,
}

/// Launch kettle with default startup (blocks until all windows close).
pub fn run() -> anyhow::Result<()> {
    run_with(Options::default())
}

/// Launch kettle, applying first-tab CLI [`Options`].
pub fn run_with(opts: Options) -> anyhow::Result<()> {
    App::run_with(opts)
}
