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
//! - [`search`](mod@search) — regex + smart-case scrollback search
//!   (`Ctrl+Shift+F`), `build_regex` literal fallback, `reveal_offset`
//!   for jump-to-match.
//! - [`links`](mod@links) — explicit OSC 8 hyperlinks + autodetected
//!   URLs in the visible grid; `is_safe_url` allowlist for safe
//!   `open::that_detached` dispatch.
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
pub mod hints;
pub mod images;
pub mod links;
pub mod scrollbar;
pub mod search;
pub mod term;
pub mod url_trim;

pub use alacritty_terminal::grid::{Dimensions, Scroll};
pub use alacritty_terminal::index::{Column, Line, Point, Side};
pub use alacritty_terminal::selection::{Selection, SelectionType};
pub use alacritty_terminal::term::{TermMode, cell::Flags};
pub use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

pub use event::{EventProxy, TermEvent, Waker};
pub use images::{ImageData, Images, Placement};
pub use links::{Link, links};
pub use search::{CaseSensitivity, Match, search, search_with};
pub use term::{SharedTerm, Terminal};
