//! Registry of decoded images placed in a terminal, anchored to an absolute
//! grid line (history-aware) so they scroll with the text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub use kettle_vt::{ImageData, Placed};

#[derive(Clone)]
pub struct Placement {
    /// Absolute line = `history_size_at_insert + cursor_viewport_line`.
    pub abs_line: i64,
    pub col: usize,
    pub cell_cols: usize,
    pub cell_rows: usize,
    pub img: ImageData,
    /// kitty image id (for deletion); `None` for Sixel/iTerm2.
    pub id: Option<u32>,
    /// z-index; images are drawn in ascending z order.
    pub z: i32,
}

pub type Images = Arc<Mutex<Vec<Placement>>>;

/// A kitty `U=1` virtual image: stored by id, fit into `cols`×`rows` cells,
/// and drawn wherever Unicode-placeholder cells reference its id.
#[derive(Clone)]
pub struct VirtualEntry {
    pub img: ImageData,
    pub cols: u32,
    pub rows: u32,
    pub z: i32,
}

/// Per-terminal registry of virtual images, keyed by kitty image id.
pub type Virtuals = Arc<Mutex<HashMap<u32, VirtualEntry>>>;

/// Drop placements that have scrolled far above the retained history.
pub fn prune(images: &Images, oldest_abs: i64) {
    if let Ok(mut v) = images.lock() {
        v.retain(|p| p.abs_line + p.cell_rows as i64 >= oldest_abs);
        if v.len() > 512 {
            let drop = v.len() - 512;
            v.drain(0..drop);
        }
    }
}
