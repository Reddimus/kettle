//! Registry of decoded images placed in a terminal, anchored to an absolute
//! grid line (history-aware) so they scroll with the text.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use kettle_vt::kitty::{AnimationState, current_frame};
use kettle_vt::kitty::{PlacementKey, relative_deletion_closure};
pub use kettle_vt::{ImageData, Placed, PlacementParams};

/// Pixel-space sub-rectangle sampled by one image placement. The renderer
/// validates it against the referenced image before producing UV coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageSourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Normalized vertical range retained from a placement's source rectangle.
///
/// Margin scrolling can permanently discard part of an image at a fractional
/// pixel boundary. Keeping the range normalized lets the renderer crop UVs
/// exactly without allocating a replacement texture or rounding to pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageSourceCrop {
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone)]
pub struct Placement {
    /// Monotonic document-row id derived from the grid's `history_origin`.
    ///
    /// A capped scrollback ring reuses its relative line numbers, so storing
    /// only `history_size + cursor_line` would eventually attach an old image
    /// to unrelated replacement text.
    pub abs_line: u64,
    pub col: usize,
    pub cell_cols: usize,
    pub cell_rows: usize,
    /// Destination offset within the first cell, in cell units.
    pub x_offset_cells: f32,
    pub y_offset_cells: f32,
    /// Exact rendered destination extent in cell units. This can be
    /// fractional when Kitty preserves the source aspect ratio.
    pub display_cols: f32,
    pub display_rows: f32,
    pub img: ImageData,
    /// Optional source pixels within `img`; `None` samples the full image.
    pub source_rect: Option<ImageSourceRect>,
    /// Permanent normalized vertical crop within `source_rect` (or the full
    /// image when `source_rect` is `None`).
    pub source_crop: Option<ImageSourceCrop>,
    /// kitty image id (for deletion); `None` for Sixel/iTerm2.
    pub id: Option<u32>,
    /// Kitty placement id (`p=`); zero is anonymous.
    pub placement_id: u32,
    /// Raw Kitty placement geometry. Retained so display/DPI changes can
    /// recompute crop, offsets, and destination size from protocol intent.
    pub kitty_params: Option<PlacementParams>,
    /// z-index; images are drawn in ascending z order.
    pub z: i32,
}

pub type Images = Arc<Mutex<Vec<Placement>>>;

/// A kitty `U=1` virtual image: stored by id, fit into `cols`×`rows` cells,
/// and drawn wherever Unicode-placeholder cells reference its id.
#[derive(Clone)]
pub struct VirtualEntry {
    pub img: ImageData,
    pub placement_id: u32,
    pub cols: u32,
    pub rows: u32,
    pub z: i32,
}

/// Per-terminal registry of virtual placements, keyed by
/// `(kitty image id, kitty placement id)`.
pub type Virtuals = Arc<Mutex<HashMap<(u32, u32), VirtualEntry>>>;

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
    pub z: i32,
    pub params: PlacementParams,
}

/// Per-terminal registry of relative placements, keyed by
/// `(child image id, child placement id)`.
pub type Relatives = Arc<Mutex<HashMap<(u32, u32), RelEntry>>>;

/// Remove every relative placement whose parent is already removed, including
/// transitive descendants. This owns the core registry mutation used by both
/// the live PTY reader and the synchronized graphics replay path.
#[doc(hidden)]
pub fn cascade_removed_relatives(
    relatives: &mut HashMap<(u32, u32), RelEntry>,
    removed_keys: &mut HashSet<PlacementKey>,
    removed_ids: &mut HashSet<u32>,
) {
    let initial_removed = removed_keys.clone();
    let closure = relative_deletion_closure(
        relatives.iter().map(|(&(image_id, placement_id), entry)| {
            (
                PlacementKey {
                    image_id,
                    placement_id,
                },
                PlacementKey {
                    image_id: entry.parent_img,
                    placement_id: entry.parent_placement,
                },
            )
        }),
        removed_keys.iter().copied(),
    );
    let removed_image_ids = closure
        .iter()
        .map(|key| key.image_id)
        .collect::<HashSet<_>>();
    relatives.retain(|&(image_id, placement_id), entry| {
        let key = PlacementKey {
            image_id,
            placement_id,
        };
        let parent_removed = if entry.parent_placement == 0 {
            removed_image_ids.contains(&entry.parent_img)
        } else {
            closure.contains(&PlacementKey {
                image_id: entry.parent_img,
                placement_id: entry.parent_placement,
            })
        };
        // A spatially selected ordinary anonymous placement can share its
        // `(image_id, 0)` key with a relative that did not match the selector.
        // Preserve that collision unless this relative's parent was also
        // removed. Callers already removed directly matched relatives.
        !closure.contains(&key) || (initial_removed.contains(&key) && !parent_removed)
    });
    removed_ids.extend(closure.iter().map(|key| key.image_id));
    *removed_keys = closure;
}

/// The on-screen origin of a relative placement: its parent placement's
/// top-left cell `(min_abs, min_col)` offset by `(h, v)` cells (positive =
/// right / down), clamped to the grid origin. Pure — fully unit tested.
pub fn relative_origin(min_abs: u64, min_col: usize, h: i32, v: i32) -> (u64, usize) {
    let abs = if v < 0 {
        min_abs.saturating_sub(v.unsigned_abs() as u64)
    } else {
        min_abs.saturating_add(v as u64)
    };
    let col = if h < 0 {
        min_col.saturating_sub(h.unsigned_abs() as usize)
    } else {
        min_col.saturating_add(h as usize)
    };
    (abs, col)
}

/// Resolve a parent image's on-screen origin, following a chain of relative
/// placements. `origins` maps an image id to a concrete origin (a real
/// placement or a placeholder's top-left). `rels` maps a relative child
/// image id to its `(parent_img, h, v)`. The walk is bounded by
/// `max_depth`; exceeding it (or a cycle) yields `None` — kitty's
/// `ETOODEEP` (`graphics-protocol.rst` requires depth ≥ 8). Pure.
pub fn resolve_chain(
    parent: u32,
    rels: &HashMap<u32, (u32, i32, i32)>,
    origins: &HashMap<u32, (u64, usize)>,
    max_depth: u32,
) -> Option<(u64, usize)> {
    if let Some(&o) = origins.get(&parent) {
        return Some(o);
    }
    if max_depth == 0 {
        return None;
    }
    let &(grandparent, h, v) = rels.get(&parent)?;
    let base = resolve_chain(grandparent, rels, origins, max_depth - 1)?;
    Some(relative_origin(base.0, base.1, h, v))
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
    /// The image to draw right now per the playback clock, or `None` if the
    /// entry has no frames. A well-formed entry always has `imgs[0]` (the
    /// root frame), but the frame list is assembled from untrusted PTY
    /// control sequences — a malformed kitty animation could register an
    /// entry with an empty `imgs`, and the previous `&self.imgs[…]` would
    /// then index `imgs[0]` (via `saturating_sub(1)` → 0) and panic at
    /// render time. Returning `Option` lets the caller skip the swap and
    /// keep the placement's existing image instead of crashing.
    pub fn current(&self) -> Option<&ImageData> {
        if self.imgs.is_empty() {
            return None;
        }
        let i = current_frame(&self.gaps, &self.state, self.started.elapsed().as_millis());
        Some(&self.imgs[i.min(self.imgs.len() - 1)])
    }
}

/// Per-terminal registry of kitty animations, keyed by image id.
pub type Animations = Arc<Mutex<HashMap<u32, AnimEntry>>>;

/// Drop placements that have scrolled far above the retained history.
pub fn prune(images: &Images, oldest_abs: u64) {
    if let Ok(mut v) = images.lock() {
        v.retain(|p| {
            p.cell_rows != 0
                && p.abs_line
                    .checked_add(p.cell_rows as u64)
                    .is_none_or(|end| end > oldest_abs)
        });
        let limit = kettle_vt::GraphicsLimits::default().placements;
        if v.len() > limit {
            let drop = v.len() - limit;
            v.drain(0..drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnimEntry, AnimationState, ImageData, Placement, PlacementParams, RelEntry,
        cascade_removed_relatives, prune, relative_origin, resolve_chain,
    };
    use kettle_vt::kitty::PlacementKey;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    fn px(n: u32) -> ImageData {
        // A 1×1 opaque pixel; the colour channel encodes which frame it is
        // so equality below is meaningful.
        ImageData::new(1, 1, vec![n as u8, 0, 0, 255]).expect("test pixel")
    }

    #[test]
    fn current_returns_a_frame_for_a_well_formed_entry() {
        let e = AnimEntry {
            imgs: vec![px(1), px(2)],
            gaps: vec![0, 0],
            state: AnimationState::default(),
            started: Instant::now(),
        };
        // Stopped at the default `current = 1` → root frame (index 0).
        assert_eq!(e.current().map(|i| i.rgba[0]), Some(1));
    }

    #[test]
    fn current_is_none_for_an_empty_frame_list() {
        // A malformed kitty animation could register an entry with no
        // frames. The old `&self.imgs[…]` indexed imgs[0] and panicked at
        // render time; now we get a clean `None` and the caller keeps the
        // placement's existing image.
        let e = AnimEntry {
            imgs: Vec::new(),
            gaps: Vec::new(),
            state: AnimationState::default(),
            started: Instant::now(),
        };
        assert!(e.current().is_none());
    }

    #[test]
    fn relative_origin_offsets_and_clamps() {
        // Parent top-left at (abs 10, col 4); +3 right, -2 up.
        assert_eq!(relative_origin(10, 4, 3, -2), (8, 7));
        // No offset → parent origin.
        assert_eq!(relative_origin(10, 4, 0, 0), (10, 4));
        // Negative past the origin clamps to 0 (no wrap/underflow).
        assert_eq!(relative_origin(1, 1, -9, -9), (0, 0));
    }

    #[test]
    fn cascade_preserves_a_relative_that_only_collides_with_a_seed_key() {
        let key = |image_id, placement_id| PlacementKey {
            image_id,
            placement_id,
        };
        let relative = |parent_img, parent_placement| RelEntry {
            img: px(1),
            parent_img,
            parent_placement,
            h: 0,
            v: 0,
            z: 0,
            params: PlacementParams::default(),
        };
        let mut relatives = HashMap::from([
            ((5, 0), relative(99, 4)),
            ((6, 1), relative(5, 0)),
            ((7, 0), relative(8, 1)),
        ]);
        let mut removed_keys = HashSet::from([key(5, 0), key(7, 0), key(8, 1)]);
        let mut removed_ids = HashSet::from([5, 7, 8]);

        cascade_removed_relatives(&mut relatives, &mut removed_keys, &mut removed_ids);

        assert!(relatives.contains_key(&(5, 0)));
        assert!(!relatives.contains_key(&(6, 1)));
        assert!(!relatives.contains_key(&(7, 0)));
        assert_eq!(
            removed_keys,
            HashSet::from([key(5, 0), key(6, 1), key(7, 0), key(8, 1)])
        );
    }

    #[test]
    fn resolve_chain_walks_and_bounds_depth() {
        // Image 1 has a concrete origin (a real/placeholder placement).
        let origins = HashMap::from([(1u32, (100u64, 10usize))]);
        // 2 → relative to 1 (+1,+1); 3 → relative to 2 (+2,0).
        let rels = HashMap::from([(2u32, (1u32, 1, 1)), (3u32, (2u32, 2, 0))]);
        // Direct concrete parent.
        assert_eq!(resolve_chain(1, &rels, &origins, 8), Some((100, 10)));
        // One hop: 2's origin = 1's origin + (1,1).
        assert_eq!(resolve_chain(2, &rels, &origins, 8), Some((101, 11)));
        // Two hops: 3 → 2 → 1.  (101,11) + (0,2) = (101,13).
        assert_eq!(resolve_chain(3, &rels, &origins, 8), Some((101, 13)));
        // Unknown parent with no chain → None.
        assert_eq!(resolve_chain(9, &rels, &origins, 8), None);

        // A 9-link chain exceeds max_depth 8 → None (ETOODEEP); a cycle
        // is likewise bounded, not infinite.
        let mut deep = HashMap::new();
        for i in 2u32..=10 {
            deep.insert(i, (i - 1, 0, 0));
        }
        assert_eq!(resolve_chain(10, &deep, &origins, 8), None);
        let cyc = HashMap::from([(5u32, (6u32, 0, 0)), (6u32, (5u32, 0, 0))]);
        assert_eq!(resolve_chain(5, &cyc, &origins, 8), None);
    }

    #[test]
    fn placement_registry_drops_oldest_at_limit_plus_one() {
        let img = px(1);
        let limit = kettle_vt::GraphicsLimits::default().placements;
        let images = Arc::new(Mutex::new(
            (0..=limit)
                .map(|i| Placement {
                    abs_line: i as u64,
                    col: 0,
                    cell_cols: 1,
                    cell_rows: 1,
                    x_offset_cells: 0.0,
                    y_offset_cells: 0.0,
                    display_cols: 1.0,
                    display_rows: 1.0,
                    img: img.clone(),
                    source_rect: None,
                    source_crop: None,
                    id: None,
                    placement_id: 0,
                    kitty_params: None,
                    z: 0,
                })
                .collect(),
        ));
        prune(&images, 0);
        let images = images.lock().unwrap();
        assert_eq!(images.len(), limit);
        assert_eq!(images[0].abs_line, 1, "oldest placement is evicted first");
    }

    #[test]
    fn prune_uses_monotonic_rows_and_an_exclusive_placement_end() {
        let img = px(1);
        let images = Arc::new(Mutex::new(vec![
            Placement {
                abs_line: 40,
                col: 0,
                cell_cols: 1,
                cell_rows: 2,
                x_offset_cells: 0.0,
                y_offset_cells: 0.0,
                display_cols: 1.0,
                display_rows: 2.0,
                img: img.clone(),
                source_rect: None,
                source_crop: None,
                id: Some(1),
                placement_id: 0,
                kitty_params: None,
                z: 0,
            },
            Placement {
                abs_line: 42,
                col: 0,
                cell_cols: 1,
                cell_rows: 1,
                x_offset_cells: 0.0,
                y_offset_cells: 0.0,
                display_cols: 1.0,
                display_rows: 1.0,
                img,
                source_rect: None,
                source_crop: None,
                id: Some(2),
                placement_id: 0,
                kitty_params: None,
                z: 0,
            },
            Placement {
                abs_line: u64::MAX,
                col: 0,
                cell_cols: 1,
                cell_rows: 1,
                x_offset_cells: 0.0,
                y_offset_cells: 0.0,
                display_cols: 1.0,
                display_rows: 1.0,
                img: px(3),
                source_rect: None,
                source_crop: None,
                id: Some(3),
                placement_id: 0,
                kitty_params: None,
                z: 0,
            },
        ]));

        prune(&images, 42);

        let images = images.lock().unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].id, Some(2));
        assert_eq!(images[0].abs_line, 42);
        assert_eq!(images[1].id, Some(3));
        assert_eq!(images[1].abs_line, u64::MAX);
    }
}
