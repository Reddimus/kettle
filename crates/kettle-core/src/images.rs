//! Registry of decoded images placed in a terminal, anchored to an absolute
//! grid line (history-aware) so they scroll with the text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use kettle_vt::kitty::{AnimationState, current_frame};
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

/// A kitty relative placement: the child image plus its parent reference
/// and `(h, v)` cell offset. Render-time position = the parent's origin
/// offset by `(h, v)` cells.
#[derive(Clone)]
pub struct RelEntry {
    pub img: ImageData,
    pub parent_img: u32,
    pub parent_placement: u32,
    pub h: i32,
    pub v: i32,
}

/// Per-terminal registry of relative placements, keyed by
/// `(child image id, child placement id)`.
pub type Relatives = Arc<Mutex<HashMap<(u32, u32), RelEntry>>>;

/// The on-screen origin of a relative placement: its parent placement's
/// top-left cell `(min_abs, min_col)` offset by `(h, v)` cells (positive =
/// right / down), clamped to the grid origin. Pure — fully unit tested.
pub fn relative_origin(min_abs: i64, min_col: usize, h: i32, v: i32) -> (i64, usize) {
    let abs = (min_abs + v as i64).max(0);
    let col = (min_col as i64 + h as i64).max(0) as usize;
    (abs, col)
}

/// A kitty animation: the full display sequence (`imgs[0]` = base/root
/// frame) with each frame's gap (ms), the control state, and the wall
/// clock the playback timing is measured from.
#[derive(Clone)]
pub struct AnimEntry {
    pub imgs: Vec<ImageData>,
    pub gaps: Vec<i32>,
    pub state: AnimationState,
    /// When the current run started (reset when run state changes).
    pub started: Instant,
}

impl AnimEntry {
    /// The image to draw right now per the playback clock.
    pub fn current(&self) -> &ImageData {
        let i = current_frame(&self.gaps, &self.state, self.started.elapsed().as_millis());
        &self.imgs[i.min(self.imgs.len().saturating_sub(1))]
    }
}

/// Per-terminal registry of kitty animations, keyed by image id.
pub type Animations = Arc<Mutex<HashMap<u32, AnimEntry>>>;

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

#[cfg(test)]
mod tests {
    use super::relative_origin;

    #[test]
    fn relative_origin_offsets_and_clamps() {
        // Parent top-left at (abs 10, col 4); +3 right, -2 up.
        assert_eq!(relative_origin(10, 4, 3, -2), (8, 7));
        // No offset → parent origin.
        assert_eq!(relative_origin(10, 4, 0, 0), (10, 4));
        // Negative past the origin clamps to 0 (no wrap/underflow).
        assert_eq!(relative_origin(1, 1, -9, -9), (0, 0));
    }
}
