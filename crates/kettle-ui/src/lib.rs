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
// C1 (multi-window foundation): per-window state container. `App` keeps only
// process-global state; everything tied to one OS window lives here.
mod window_state;
// Cycle 928 (agent-first A2): in-process control server. The accept/reader/
// writer threads run kettle-ctl's transport; the App drains requests on the
// main thread via `UserEvent::Ctl`. Always compiled (gated at runtime by the
// `agent-server` config, default off).
mod ctl_server;
mod gpu_diagnostics;
mod input;
mod lua;
mod mux;
mod session;
mod settings;
// Cycle 794: in-app "newer release available" checker (notify-only). Kept a
// private mod (its spawn fn references the crate-internal `UserEvent`); the
// `kettle` bin reaches the synchronous `--check-update` path via the public
// `check_for_update_cli` wrapper below.
mod update_check;
// Cycle 745: OSC 9;4 taskbar progress (pwsh 7 / Windows Terminal parity).
mod taskbar;

// Cycle 399: SCM_RIGHTS fd-passing for detachable-tabs Bucket-D.
// Unix-only (Linux + macOS + BSDs); Windows users get the
// Action::MoveTabToNewWindow keyboard-driven fallback (cycle 384).
#[cfg(unix)]
mod fd_transport;
// Cycle 400: drag state machine for detachable tabs. Pure-data
// FSM; the App's mouse-handler advances it on MouseDown / Move /
// Up. Cross-platform (no Unix-specific code).
mod detach;
// Cycle 875: developer-only session recorder (asciicast trace). Compiled out of
// released / packaged builds via the `dev-record` Cargo feature.
#[cfg(feature = "dev-record")]
mod dev_record;

pub use app::App;
pub use lua::{LuaCommand, LuaEngine, LuaEvent};

/// First-tab startup overrides from the CLI.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Run this argv in the first tab instead of the shell (`kettle -e …`).
    pub command: Option<Vec<String>>,
    /// Dropdown-parity cycle: the bin crate's full version string (crate
    /// version + git hash — what `--version` prints) for the About panel.
    /// kettle-ui has no build script, so the hash can't be derived here.
    pub version: Option<String>,
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
    /// Cycle 938 (Terminator parity): `-m/--maximise`, `-f/--fullscreen`,
    /// `-H/--hidden` override the `window-state` config for THIS launch.
    /// When several are given the precedence is fixed: hidden > fullscreen >
    /// maximise (`-H` is the explicit "don't show me" intent and must not be
    /// dropped; mirrors Terminator applying hidden last). `None` = use the
    /// config value.
    pub window_state_override: Option<kettle_config::WindowState>,
    /// Cycle 938: `-b/--borderless` overrides the `borderless` config for this
    /// launch. `None` = use the config value.
    pub borderless_override: Option<bool>,
    /// Cycle 938: `-T/--title TEXT` forces the window title for this launch —
    /// sets the title format to the literal string (no `{title}` placeholder),
    /// matching Terminator's `--title`. `None` = use `window-title-format`.
    pub title_override: Option<String>,
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
    /// Cycle 403 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 8 file-fallback): JSON handoff file written by
    /// another kettle process (cycle 384's MoveTabToNewWindow).
    /// When set, kettle reads + deserializes a STab from this
    /// path + uses it as the startup tab (replacing the default
    /// shell). Path is deleted after read (one-shot handoff).
    pub tab_handoff: Option<std::path::PathBuf>,
    /// Cycle 408 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 7 SCM_RIGHTS live-PTY transfer): inherited
    /// socket fd carrying the serialized tab JSON + PTY-fd
    /// SCM_RIGHTS ancillary payload. The target kettle calls
    /// fd_transport::recv_fds on this fd to receive both the
    /// JSON state + the duplicated PTY fds, then reconstructs
    /// the tab with adopted fds (running shells preserved).
    /// Unix-only.
    pub tab_handoff_fd: Option<i32>,
    /// Cycle 918: `--restore` — restore the previous session (tabs/splits/dirs)
    /// for THIS launch, regardless of the `restore-session` config default
    /// (which is off). A one-shot "continue where I left off" without editing
    /// config. `--layout`/`--tab-handoff` remain independent explicit paths.
    pub restore: bool,
    /// Cycle 928 (agent-first A2): `--agent-server MODE` — override the
    /// `agent-server` config for THIS launch (`off`|`read-only`|`full`). `None`
    /// = use the config value (default off).
    pub agent_server: Option<kettle_config::AgentServer>,
    /// Cycle 875: developer session-recorder output path (`--record PATH`).
    /// Writes an asciicast-compatible trace. Only present in `dev-record`
    /// feature builds — absent from released / packaged binaries.
    #[cfg(feature = "dev-record")]
    pub record: Option<std::path::PathBuf>,
    /// Cycle 876: with `record`, capture RAW typed characters in `i` events
    /// (`--record-raw-input`). Off by default — keystrokes are redacted tokens,
    /// not literal characters, so typed secrets aren't written to the trace.
    #[cfg(feature = "dev-record")]
    pub record_raw_input: bool,
}

/// Launch kettle with default startup (blocks until all windows close).
pub fn run() -> anyhow::Result<()> {
    run_with(Options::default())
}

/// Launch kettle, applying first-tab CLI [`Options`].
pub fn run_with(opts: Options) -> anyhow::Result<()> {
    App::run_with(opts)
}

/// Cycle 938 (Terminator parity): saved layout names (`<config-dir>/layouts/
/// *.json`), for `kettle --list-layouts`. Thin public wrapper over the private
/// session module so the bin crate can introspect without launching a window.
pub fn list_layouts() -> Vec<String> {
    session::Session::list_layouts()
}

/// Cycle 794: the synchronous `kettle --check-update` path. Does one GitHub
/// "latest release" GET (bypassing the once/24h throttle — the user asked
/// explicitly) and returns a human-readable line for the `kettle` bin to print.
pub fn check_for_update_cli() -> String {
    update_check::run_blocking_check(env!("CARGO_PKG_VERSION"))
}
