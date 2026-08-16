//! kettle terminal core: per-pane PTY ownership, the `alacritty_terminal`
//! grid / VT engine glue (with the [`kettle_vt`] image-extractor sitting in
//! front), the UI event bridge, and the helper modules the renderer reads
//! each frame.
//!
//! Modules (all `pub` — used by `kettle-render` and `kettle-ui`):
//! - [`event`] — `EventProxy` + `Waker`: the channels alacritty's engine
//!   uses to push side-channel events (title, bell, clipboard, color
//!   requests, child exit) back to the UI loop.
//! - [`term`] — `Terminal`: PTY + engine + reader thread + image
//!   registries. `Terminal::new` opens the PTY, spawns the reader, wires
//!   the extractor in front, and exposes thread-safe `Arc<Mutex<Term>>`
//!   for the renderer.
//! - [`search`](mod@search) — bounded signed-grid [`CompiledSearch`]
//!   (`Ctrl+Shift+F`) with strict regex compilation, plus viewport reveal
//!   geometry and legacy compatibility helpers.
//! - [`links`](mod@links) — explicit OSC 8 hyperlinks + autodetected
//!   URLs and local file paths in the visible grid; `is_safe_url`
//!   allowlist for safe `open::that_detached` dispatch.
//! - [`hints`] — quick-select hint targets (`Ctrl+Shift+H`): URLs,
//!   paths, IPs, git hashes; `detect` returns a sorted list of
//!   `HintSpan`s.
//! - [`url_trim`] — bracket-balance-aware trailing-punctuation trim
//!   shared by `links` and `hints`. Keeps `…/Foo_(bar)` URLs whole.
//! - [`images`] — per-pane registries for kitty graphics: `Images`
//!   (placements), `Virtuals` (Unicode placeholder targets),
//!   `Animations` (frame swap), `Relatives` (parent-relative
//!   placements).
//! - [`scrollbar`] — `scroll-on-output` per-pane history-diff detection
//!   and `target_offset` thumb math.

pub mod event;
pub mod grid_text;
pub mod hints;
pub mod images;
pub mod links;
mod persistence;
pub mod scrollbar;
pub mod search;
// Agent-first asciicast session recorder. Behind the
// `asciicast` feature in normal builds; always available under `cfg(test)` so
// the in-crate tests + the `replays_asciicast_v2_output_into_grid` replay
// test exercise it without the feature flag.
#[cfg(any(feature = "asciicast", test))]
pub mod record;
pub mod term;
pub mod url_trim;

pub use alacritty_terminal::grid::{Dimensions, Scroll};
pub use alacritty_terminal::index::{Column, Line, Point, Side};
pub use alacritty_terminal::selection::{Selection, SelectionType};
pub use alacritty_terminal::term::{ClipboardType, TermMode, cell::Flags, viewport_to_point};
pub use alacritty_terminal::vi_mode::ViMotion;
pub use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

pub use event::{EventProxy, TermEvent, Waker};
pub use images::{ImageData, ImageSourceCrop, ImageSourceRect, Images, Placement, PlacementParams};
pub use links::{Link, links, links_with_cwd};
pub use search::{
    CaseSensitivity, CompiledSearch, MAX_SEARCH_LOGICAL_LINE_CONTEXT, MAX_SEARCH_MATCHES,
    MAX_SEARCH_MATERIALIZED_BYTES, MAX_SEARCH_MATERIALIZED_CELLS, MAX_SEARCH_OPERATION_BYTES,
    MAX_SEARCH_OPERATION_CELLS, MAX_SEARCH_OPERATION_HAYSTACKS, MAX_SEARCH_QUERY_BYTES, Match,
    SearchBatch, SearchBounds, SearchCompileError, SearchDirection, SearchLayout, SearchOutcome,
    SearchPoint, SearchScanToken, SearchSpan, search, search_with,
};
pub use term::{
    CommandFinished, ProtocolNotification, PtyEofProgress, PtyGeometry, PtyInputTail,
    PtyOutputSender, PtyReadProgress, PtyReadStatus, PtyStdin, PtyWriter, ScreenText,
    SessionLogFailure, SharedTerm, ShellActivity, Terminal, TerminalCapabilities,
    WorkingDirectoryPolicy,
};
// OSC 9;4 taskbar-progress state, surfaced by `Terminal::progress`
// (re-exported so the UI can name it without depending on kettle-vt directly).
pub use kettle_vt::Progress;
pub use kettle_vt::{GraphicsBudget, GraphicsLimits, GraphicsReservation};
