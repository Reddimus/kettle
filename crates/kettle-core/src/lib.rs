//! kettle terminal core: PTY management, the `alacritty_terminal` grid/VT
//! engine glue, the UI event bridge, and buffer search.

pub mod event;
pub mod search;
pub mod term;

pub use alacritty_terminal::grid::{Dimensions, Scroll};
pub use alacritty_terminal::index::{Column, Line, Point};
pub use alacritty_terminal::term::{TermMode, cell::Flags};
pub use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

pub use event::{EventProxy, TermEvent, Waker};
pub use search::{Match, search};
pub use term::{SharedTerm, Terminal};
