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
//!   (`broadcast_write_delivery` / `broadcast_paste_delivery`),
//!   session snapshot/restore wiring, SSH-tab spawning.
//! - `session` — atomic save + corruption-backup of the tab/split tree to
//!   `<config-dir>/session.json`.
//!
//! Overlays owned by `app::App`: scrollback search (Ctrl+Shift+F), SSH
//! launcher (Ctrl+Shift+S), command palette (Ctrl+Shift+K), quick-select
//! hints (Ctrl+Shift+H). Modal state is coordinated through
//! `close_all_modals()` / `any_modal_open()` so they don't stack.

#[cfg(test)]
pub(crate) fn test_tempdir() -> kettle_test_support::PrivateTempDir {
    kettle_test_support::private_tempdir("kettle-ui-test-")
}

mod activation_server;
mod agent_plugin;
pub use agent_plugin::{ClaudeStatus, claude_status};
/// Explicit native agy plugin staging.
pub mod agy_plugin;
mod app;
pub mod codex_shell;
mod held_buttons;
#[cfg(all(test, target_os = "linux"))]
mod test_bus;
#[cfg(any(target_os = "linux", test))]
mod wayland;
// Startup phase stamps, printed with RUST_LOG=kettle::startup=info. Public so
// the binary can mark the first statement of `main`.
pub mod startup_trace;
// Per-window state container. `App` keeps only process-global state;
// everything tied to one OS window lives here.
mod window_accent;
mod window_state;
// The cursor blink's timeline, shared by the GPU scheduler and the macOS
// Core Animation layer that can take the blink over.
mod cursor_blink;
mod cursor_exit_log;
// In-process control server (agent-first A2). The accept/reader/
// writer threads run kettle-ctl's transport; the App drains requests on the
// main thread via `UserEvent::Ctl`. Always compiled (gated at runtime by the
// `agent-server` config, default off).
mod ctl_server;
// The system fonts, loaded on a thread while the event loop starts.
mod font_preload;
mod gpu_diagnostics;
mod input;
mod lua;
mod lua_pattern;
#[cfg(target_os = "macos")]
mod macos_display;
// The Core Animation layer that blinks the cursor on macOS. Never exists
// elsewhere.
mod macos_cursor_layer;
// macOS Dock context menu (right-click the Dock icon). No-op elsewhere.
mod macos_dock;
mod macos_menu;
mod macos_reopen;
mod media;
mod modal_input;
mod motion;
mod mux;
mod native_drag;
mod native_fullscreen;
mod native_material;
mod notifications;
mod owned_dir;
mod pane_partition;
mod paste_image;
mod runtime_diagnostics;
mod search_input;
mod session;
mod settings;
// In-app release checker. It notifies of a newer release, or installs one under
// `update-policy = auto`. Kept a private mod (its spawn fn references the
// crate-internal `UserEvent`); the `kettle` bin reaches the synchronous
// `--check-update` path via the public `check_for_update_cli` wrapper below.
mod update_check;
mod update_notice;
mod update_toast;
mod video_preview;
mod wall_clock;
// OSC 9;4 taskbar progress (pwsh 7 / Windows Terminal parity).
mod taskbar;

// SCM_RIGHTS fd-passing, used only by the deprecated `--tab-handoff-fd` receive
// path. Unix-only (Linux + macOS + BSDs).
#[cfg(unix)]
mod fd_transport;
// Drag state machine for detachable tabs. Pure-data
// FSM; the App's mouse-handler advances it on MouseDown / Move /
// Up. Cross-platform (no Unix-specific code).
mod detach;
// Session recorder (asciicast trace). A runtime toggle present in every build
// (config `record = on` / `--record`); re-exports `kettle_core::record`.
mod dev_record;

pub use app::App;
pub use lua::{LuaCommand, LuaEngine, LuaEvent};
pub use notifications::{flush_desktop_notifications, queue_desktop_notification};

#[doc(hidden)]
pub fn run_media_preview_worker() -> i32 {
    video_preview::run_worker()
}

/// Record this build's identity, which the video-preview worker must share
/// with the GUI that starts it. Call once, first thing in `main`.
#[doc(hidden)]
pub fn set_build_identity(identity: &str) {
    video_preview::set_build_identity(identity);
}

/// An independently owned, validated startup handoff socket.
#[derive(Debug, Clone)]
pub struct TabHandoff {
    #[cfg(unix)]
    pub(crate) socket: std::sync::Arc<std::os::unix::net::UnixStream>,
}

impl TabHandoff {
    /// Take over an inherited, connected Unix stream before starting the UI.
    ///
    /// The checks run on a close-on-exec duplicate. Once they pass, `fd`
    /// itself is closed, so children never inherit it and the sender sees the
    /// socket close when the handoff is dropped. On error `fd` stays open.
    ///
    /// # Safety
    ///
    /// The caller must own `fd`: nothing else in this process may use or
    /// close it afterwards.
    pub unsafe fn adopt(fd: i32) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

            if fd < 3 {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }
            // SAFETY: fcntl validates the number and returns a new owned fd.
            let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the successful fcntl call returned unique ownership.
            let owned = unsafe { OwnedFd::from_raw_fd(duplicate) };
            let mut kind: libc::c_int = 0;
            let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
            // SAFETY: both output pointers refer to live, correctly sized values.
            if unsafe {
                libc::getsockopt(
                    owned.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&mut kind as *mut libc::c_int).cast(),
                    &mut len,
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if kind != libc::SOCK_STREAM {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }
            let socket = std::os::unix::net::UnixStream::from(owned);
            socket.local_addr()?;
            socket.peer_addr()?;
            // SAFETY: the caller owns `fd`, and the duplicate now holds the socket.
            drop(unsafe { OwnedFd::from_raw_fd(fd) });
            Ok(Self {
                socket: std::sync::Arc::new(socket),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = fd;
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}

#[cfg(test)]
mod tab_handoff_tests {
    use super::TabHandoff;

    #[test]
    fn invalid_handoff_descriptor_is_rejected() {
        // SAFETY: none of these numbers is an open descriptor in this process.
        unsafe {
            assert!(TabHandoff::adopt(-1).is_err());
            assert!(TabHandoff::adopt(0).is_err());
            assert!(TabHandoff::adopt(i32::MAX).is_err());
        }
    }

    /// Runs alone: macOS marks a new socket pair close-on-exec in a second
    /// step, and a child another test spawns in between would keep the peer
    /// from seeing end of file.
    #[cfg(unix)]
    #[test]
    fn handoff_requires_a_stream_and_owns_the_inherited_descriptor() {
        kettle_test_support::run_alone_in_child(
            "tab_handoff_tests::handoff_requires_a_stream_and_owns_the_inherited_descriptor",
            handoff_requires_a_stream_and_owns_the_inherited_descriptor_alone,
        );
    }

    #[cfg(unix)]
    fn handoff_requires_a_stream_and_owns_the_inherited_descriptor_alone() {
        use std::io::{Read, Write};
        use std::os::fd::{AsRawFd, IntoRawFd};

        // A rejected descriptor stays with its owner, which closes it.
        let file = std::fs::File::open("/dev/null").unwrap();
        let (datagram, _peer) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let tcp = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        // SAFETY: each check fails, and a failed adopt leaves the fd alone.
        unsafe {
            assert!(TabHandoff::adopt(file.as_raw_fd()).is_err());
            assert!(TabHandoff::adopt(datagram.as_raw_fd()).is_err());
            assert!(TabHandoff::adopt(tcp.as_raw_fd()).is_err());
        }

        let (stream, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        // Set before the far end closes: macOS refuses the option afterwards.
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let inherited = stream.into_raw_fd();
        // SAFETY: into_raw_fd gave up the only other owner.
        let handoff = unsafe { TabHandoff::adopt(inherited) }.unwrap();
        assert_ne!(handoff.socket.as_raw_fd(), inherited);
        let flags = unsafe { libc::fcntl(handoff.socket.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        peer.write_all(b"x").unwrap();
        let mut byte = [0];
        (&*handoff.socket).read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");
        // Dropping the handoff closes the last descriptor, the inherited one
        // included, so the sender sees end of file.
        drop(handoff);
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    }
}

/// Recording policy resolved by the entry point before activation.
#[derive(Clone, Debug, Default)]
pub enum RecordingPolicy {
    #[default]
    Disabled,
    Enabled {
        target: kettle_core::record::RecordingTarget,
        raw_input: bool,
    },
}

/// First-tab startup overrides from the CLI.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// One-shot primary-election handle for bare-launch activation. The Kettle
    /// binary supplies this only when no earlier process accepted the launch.
    pub activation: Option<kettle_ctl::activation::PrimaryHandle>,
    /// Run this argv in the first tab instead of the shell (`kettle -e …`).
    pub command: Option<Vec<String>>,
    /// Dropdown-parity: the bin crate's full version string (crate
    /// version + git hash — what `--version` prints) for the About panel.
    /// kettle-ui has no build script, so the hash can't be derived here.
    pub version: Option<String>,
    /// Working directory for the first tab (`kettle -d DIR`).
    pub cwd: Option<std::path::PathBuf>,
    /// Resolved config file. `None` uses the default path.
    pub config: Option<std::path::PathBuf>,
    /// Whether the path was an explicit user selection or one kettle
    /// discovered as the default/profile config.
    pub config_trust: kettle_config::ConfigTrust,
    /// Named-layout session profile. When set, kettle saves
    /// and restores from `<config-dir>/layouts/<NAME>.json` instead of
    /// the default `<config-dir>/session.json`. Lets a user persist
    /// multiple distinct workspaces ("dev", "ops", "docs") and snap
    /// between them via `kettle --layout dev`. Terminator parity.
    pub layout: Option<String>,
    /// Peacock parity: one-off accent color override that
    /// wins over the config `accent-color` key. Plumbed through from
    /// the `--accent COLOR` CLI flag. `None` = use whatever the
    /// resolved config says.
    pub accent_override: Option<kettle_config::Rgb>,
    /// Terminator parity: `-m/--maximise`, `-f/--fullscreen`,
    /// `-H/--hidden` override the `window-state` config for THIS launch.
    /// When several are given the precedence is fixed: hidden > fullscreen >
    /// maximise (`-H` is the explicit "don't show me" intent and must not be
    /// dropped; mirrors Terminator applying hidden last). `None` = use the
    /// config value.
    pub window_state_override: Option<kettle_config::WindowState>,
    /// `-b/--borderless` overrides the `borderless` config for this
    /// launch. `None` = use the config value.
    pub borderless_override: Option<bool>,
    /// `-T/--title TEXT` forces the window title for this launch —
    /// sets the title format to the literal string (no `{title}` placeholder),
    /// matching Terminator's `--title`. `None` = use `window-title-format`.
    pub title_override: Option<String>,
    /// Remote control. When set, App watches this path for
    /// command lines and dispatches them to the focused pane. Format:
    /// one command per line, e.g. `send-text echo hello\n`. The
    /// `kettle --remote-send TEXT` CLI flag writes to this same file
    /// so external scripts can drive kettle without launching a new
    /// window. Cross-platform via the existing notify watcher (no
    /// platform-specific socket code yet).
    pub remote_file: Option<std::path::PathBuf>,
    /// Lua scripting foundation: when set, App initializes
    /// a `LuaEngine` and executes the script once at startup with
    /// the `kettle` namespace installed. Errors are logged + don't
    /// block the launch.
    pub lua_script: Option<std::path::PathBuf>,
    /// Deprecated receive-only JSON tab handoff (`--tab-handoff`) written by an
    /// older kettle process. When set, kettle loads the startup session from
    /// this path (replacing the default shell) and deletes the file after
    /// reading (one-shot handoff).
    pub tab_handoff: Option<std::path::PathBuf>,
    /// Deprecated receive-only SCM_RIGHTS tab handoff (`--tab-handoff-fd`) from
    /// an older kettle process. The inherited socket fd carries the serialized
    /// session JSON plus PTY fds. kettle receives both with
    /// `fd_transport::recv_fds`, restores the session from the JSON, and closes
    /// the PTY fds, so running shells are not preserved. Unix-only.
    pub tab_handoff_fd: Option<TabHandoff>,
    /// `--restore` — restore the previous session (tabs/splits/dirs)
    /// for THIS launch, regardless of the `restore-session` config default
    /// (which is off). A one-shot "continue where I left off" without editing
    /// config. `--layout`/`--tab-handoff` remain independent explicit paths.
    pub restore: bool,
    /// `--agent-server MODE` overrides the `agent-server` config for THIS launch
    /// (`off`|`read-only`|`full`). `None` = use the config value (default off).
    pub agent_server: Option<kettle_config::AgentServer>,
    /// `--agent-display on|off` overrides the `agent-display` config for THIS
    /// launch. `None` = use the config value, except that `--agent-server off`
    /// alone also turns display off (see [`kettle_ctl::CtlOverrides`]).
    pub agent_display: Option<bool>,
    /// Immutable recording policy resolved before activation.
    pub recording: RecordingPolicy,
    /// The media worker client, configured by the `kettle` binary with the
    /// worker's install path and this build's identity. Nothing is checked
    /// and no worker starts until something asks. `None` reports media as
    /// not configured.
    pub media: Option<std::sync::Arc<kettle_media::client::WorkerClient>>,
    /// The install this process started from, found before an update could
    /// replace its executable. Update checks use it (#574).
    pub update_install: kettle_update::InstallSnapshot,
}

/// Launch kettle with default startup (blocks until all windows close).
pub fn run() -> anyhow::Result<()> {
    run_with(Options::default())
}

/// Launch kettle, applying first-tab CLI [`Options`].
pub fn run_with(opts: Options) -> anyhow::Result<()> {
    App::run_with(opts)
}

/// Terminator parity: saved layout names (`<config-dir>/layouts/
/// *.json`), for `kettle --list-layouts`. Thin public wrapper over the private
/// session module so the bin crate can introspect without launching a window.
pub fn list_layouts() -> Vec<String> {
    session::Session::list_layouts()
}

/// The synchronous `kettle --check-update` path. Fetches and verifies the
/// signed manifest from the latest GitHub release once, bypassing the
/// `update-check-interval-hours` throttle (default 24h) because the user asked
/// explicitly. Returns a human-readable line for the `kettle` bin to print.
pub fn check_for_update_cli() -> String {
    update_check::run_blocking_check(env!("CARGO_PKG_VERSION"))
}
