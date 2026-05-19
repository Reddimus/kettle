//! Registry of decoded images placed in a terminal, anchored to an absolute
//! grid line (history-aware) so they scroll with the text.

use std::sync::{Arc, Mutex};

pub use kettle_vt::ImageData;

#[derive(Clone)]
pub struct Placement {
    /// Absolute line = `history_size_at_insert + cursor_viewport_line`.
    pub abs_line: i64,
    pub col: usize,
    pub cell_cols: usize,
    pub cell_rows: usize,
    pub img: ImageData,
}

pub type Images = Arc<Mutex<Vec<Placement>>>;

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
