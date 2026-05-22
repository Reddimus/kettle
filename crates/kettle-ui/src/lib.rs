//! kettle UI: the winit event loop, tab/pane multiplexer, input encoding, and
//! every interactive overlay kettle ships.
//!
//! Modules (private; see source for details):
//! - `app` — the winit `App` impl + `WindowEvent` dispatch (focus / mouse /
//!   keyboard / drag-and-drop / clipboard / resize). Owns the renderer
//!   handle, blink-phase state, modal-overlay state machine, broadcast-input
//!   indicator wiring, and the live-reload notify watcher.
//! - `input` — keyboard-to-PTY byte encoding (xterm modifier table, kitty
//!   keyboard protocol, F-keys, named-key triggers, bracketed-paste payload
//!   construction, OSC 52 clamp).
//! - `mux` — tab/split tree, pane focus, broadcast input
//!   (`broadcast_write` / `broadcast_paste` / `broadcast_scroll_to_bottom`),
//!   session snapshot/restore wiring, SSH-tab spawning.
//! - `session` — atomic save + corruption-backup of the tab/split tree to
//!   `<config-dir>/session.json`.
//!
//! Overlays owned by `app::App`: scrollback search (Ctrl+Shift+F), SSH
//! launcher (Ctrl+Shift+S), command palette (Ctrl+Shift+K), quick-select
//! hints (Ctrl+Shift+H). Modal state is coordinated through
//! `close_all_modals()` / `any_modal_open()` so they don't stack.

mod app;
mod input;
mod lua;
mod mux;
mod session;

// Cycle 399: SCM_RIGHTS fd-passing for detachable-tabs Bucket-D.
// Unix-only (Linux + macOS + BSDs); Windows users get the
// Action::MoveTabToNewWindow keyboard-driven fallback (cycle 384).
#[cfg(unix)]
mod fd_transport;

pub use app::App;
pub use lua::{LuaCommand, LuaEngine, LuaEvent};

/// First-tab startup overrides from the CLI.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Run this argv in the first tab instead of the shell (`kettle -e …`).
    pub command: Option<Vec<String>>,
    /// Working directory for the first tab (`kettle -d DIR`).
    pub cwd: Option<std::path::PathBuf>,
    /// Explicit config file (`kettle --config FILE`); `None` = default path.
    pub config: Option<std::path::PathBuf>,
    /// Cycle 291: named-layout session profile. When set, kettle saves
    /// and restores from `<config-dir>/layouts/<NAME>.json` instead of
    /// the default `<config-dir>/session.json`. Lets a user persist
    /// multiple distinct workspaces ("dev", "ops", "docs") and snap
    /// between them via `kettle --layout dev`. Terminator parity.
    pub layout: Option<String>,
    /// Cycle 293 peacock parity: one-off accent color override that
    /// wins over the config `accent-color` key. Plumbed through from
    /// the `--accent COLOR` CLI flag. `None` = use whatever the
    /// resolved config says.
    pub accent_override: Option<kettle_config::Rgb>,
    /// Cycle 302 remote control. When set, App watches this path for
    /// command lines and dispatches them to the focused pane. Format:
    /// one command per line, e.g. `send-text echo hello\n`. The
    /// `kettle --remote-send TEXT` CLI flag writes to this same file
    /// so external scripts can drive kettle without launching a new
    /// window. Cross-platform via the existing notify watcher (no
    /// platform-specific socket code yet).
    pub remote_file: Option<std::path::PathBuf>,
    /// Cycle 324 Lua scripting foundation: when set, App initializes
    /// a `LuaEngine` and executes the script once at startup with
    /// the `kettle` namespace installed. Errors are logged + don't
    /// block the launch.
    pub lua_script: Option<std::path::PathBuf>,
}

/// Launch kettle with default startup (blocks until all windows close).
pub fn run() -> anyhow::Result<()> {
    run_with(Options::default())
}

/// Launch kettle, applying first-tab CLI [`Options`].
pub fn run_with(opts: Options) -> anyhow::Result<()> {
    App::run_with(opts)
}
