//! kettle UI: the winit application, the tab/pane multiplexer, keyboard input
//! encoding, and the search overlay.

mod app;
mod input;
mod mux;
mod session;

pub use app::App;

/// Launch kettle (blocks until all windows close).
pub fn run() -> anyhow::Result<()> {
    App::run()
}
