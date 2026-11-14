//! Pure scrollback-scrollbar geometry: the thumb rectangle to draw and the
//! display offset a click maps to. Shared by the renderer (drawing) and the
//! UI (click-to-jump) so both agree, and fully unit-testable without a GPU.
//!
//! `rows` = visible lines, `hist` = scrollback lines, `off` = lines scrolled
//! back from the bottom (`0` = at the prompt). `total = rows + hist`.

/// The shortest thumb, in logical pixels: the overlay thumb length GTK's
/// terminals draw, so 10,000 lines of history still leave a thumb a pointer
/// can find. The renderer and the pointer scale it by the display's scale
/// factor, so both draw and grab the same thumb.
pub const MIN_THUMB_LOGICAL: f32 = 40.0;

/// The thumb as `(y_offset_within_track, height)` in track pixels, or `None`
/// when everything fits (no scrollbar needed). This wrapper takes the track
/// at 1x, with the [`MIN_THUMB_LOGICAL`] minimum; DPI-aware callers should
/// use [`thumb_with_min`].
pub fn thumb(rows: usize, hist: usize, off: usize, track: f32) -> Option<(f32, f32)> {
    thumb_with_min(rows, hist, off, track, MIN_THUMB_LOGICAL)
}

/// DPI-aware thumb geometry. `minimum` is in the same physical-pixel space as
/// `track`, allowing the UI and renderer to agree at every scale factor.
pub fn thumb_with_min(
    rows: usize,
    hist: usize,
    off: usize,
    track: f32,
    minimum: f32,
) -> Option<(f32, f32)> {
    let total = rows + hist;
    if total <= rows || track <= 0.0 {
        return None;
    }
    let h = (track * rows as f32 / total as f32)
        .max(minimum.max(1.0))
        .min(track);
    let travel = (track - h).max(0.0);
    let progress = off.min(hist) as f32 / hist as f32;
    let y = (1.0 - progress) * travel;
    Some((y, h))
}

/// True when new output appeared since the previous frame and the user
/// asked for `scroll-on-output` (Alacritty `scroll_on_output`). The history
/// snapshot from the prior redraw is `prev`; `current` is the value now.
/// `None` previous means we haven't seen a frame yet — never scroll
/// (otherwise the very first paint would yank the cursor away from the
/// origin).
pub fn should_scroll_on_output(enabled: bool, prev: Option<usize>, current: usize) -> bool {
    enabled && prev.is_some_and(|p| current > p)
}

/// The `display_offset` a click/drag at `yrel` pixels down the `track` maps
/// to: the clicked fraction becomes the top of the viewport. Clamped to
/// `0..=hist`.
pub fn target_offset(yrel: f32, track: f32, rows: usize, hist: usize) -> usize {
    let Some((_, height)) = thumb(rows, hist, 0, track) else {
        return 0;
    };
    target_offset_for_drag(yrel, 0.0, track, height, hist)
}

/// Convert pointer Y to scroll offset while preserving where the user grabbed
/// the thumb. `grab` is the distance from the pointer to the thumb's top edge.
pub fn target_offset_for_drag(
    pointer_y: f32,
    grab: f32,
    track: f32,
    thumb_height: f32,
    hist: usize,
) -> usize {
    if hist == 0 || track <= 0.0 {
        return 0;
    }
    let travel = (track - thumb_height).max(0.0);
    if travel == 0.0 {
        return 0;
    }
    let top = (pointer_y - grab).clamp(0.0, travel);
    let progress_from_top = top / travel;
    ((1.0 - progress_from_top) * hist as f32)
        .round()
        .clamp(0.0, hist as f32) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_none_when_everything_fits() {
        assert_eq!(thumb(40, 0, 0, 400.0), None);
        assert_eq!(thumb(40, 100, 0, 0.0), None);
    }

    #[test]
    fn thumb_size_and_position() {
        // 40 visible of 200 total over a 400px track → 1/5 = 80px thumb.
        let (y, h) = thumb(40, 160, 0, 400.0).unwrap();
        assert!((h - 80.0).abs() < 0.01);
        // off=0 = at the newest line → thumb pinned to the track bottom.
        assert!((y - (400.0 - h)).abs() < 0.01, "off=0 → bottom, got {y}");
        // Scrolled fully back (off == hist, oldest) → thumb at the top.
        let (y2, _) = thumb(40, 160, 160, 400.0).unwrap();
        assert_eq!(y2, 0.0, "off=hist → top of track");
        // Minimum thumb height is enforced.
        let (_, hmin) = thumb(1, 100_000, 0, 300.0).unwrap();
        assert_eq!(hmin, MIN_THUMB_LOGICAL);
    }

    #[test]
    fn should_scroll_on_output_rules() {
        // Disabled → never scrolls, regardless of growth.
        assert!(!should_scroll_on_output(false, Some(10), 20));
        // First frame (no prev) → never scrolls (otherwise the initial
        // paint would unfocus the origin on every launch).
        assert!(!should_scroll_on_output(true, None, 0));
        assert!(!should_scroll_on_output(true, None, 9999));
        // No growth (equal or shrank) → nothing to chase. Shrinking
        // happens when the screen is resized and lines fold into history,
        // which isn't "new output."
        assert!(!should_scroll_on_output(true, Some(50), 50));
        assert!(!should_scroll_on_output(true, Some(50), 40));
        // Growth → scroll iff enabled.
        assert!(should_scroll_on_output(true, Some(50), 51));
        assert!(should_scroll_on_output(true, Some(0), 1));
    }

    #[test]
    fn target_offset_maps_click_to_offset() {
        // rows=40, hist=160, total=200, track=400.
        // Top of track → oldest line → max offset (hist).
        assert_eq!(target_offset(0.0, 400.0, 40, 160), 160);
        // Bottom of track → newest → offset 0.
        assert_eq!(target_offset(400.0, 400.0, 40, 160), 0);
        // At mid track, `target_offset` puts the thumb's top edge at the click
        // (zero grab offset), so the result reflects thumb travel.
        let mid = target_offset(200.0, 400.0, 40, 160);
        assert_eq!(mid, 60);
        // Out-of-range y is clamped, not panicking.
        assert_eq!(target_offset(-50.0, 400.0, 40, 160), 160);
        assert_eq!(target_offset(9999.0, 400.0, 40, 160), 0);
        // No history → always 0.
        assert_eq!(target_offset(100.0, 400.0, 40, 0), 0);
    }

    #[test]
    fn drag_preserves_pointer_grab_offset() {
        let (top, height) = thumb_with_min(40, 160, 80, 400.0, 24.0).unwrap();
        assert_eq!(top, 160.0);
        assert_eq!(height, 80.0);
        // Grabbing 20px into the thumb and not moving leaves the offset intact.
        assert_eq!(
            target_offset_for_drag(top + 20.0, 20.0, 400.0, height, 160),
            80
        );
        assert_eq!(target_offset_for_drag(20.0, 20.0, 400.0, height, 160), 160);
        assert_eq!(
            target_offset_for_drag(400.0 + 20.0, 20.0, 400.0, height, 160),
            0
        );
    }

    #[test]
    fn thumb_minimum_scales_with_caller() {
        let (_, one_x) = thumb_with_min(1, 100_000, 0, 500.0, 24.0).unwrap();
        let (_, two_x) = thumb_with_min(1, 100_000, 0, 1000.0, 48.0).unwrap();
        assert_eq!(one_x, 24.0);
        assert_eq!(two_x, 48.0);
    }
}

/// Command tick rectangle in physical pixels. This overlay never moves the
/// terminal grid. Failure ticks are thicker as well as differently colored.
/// `column` is the scrollbar's column, its left edge and width, so a tick
/// lies across the bar the pane paints; the tick stays inside the terminal.
pub fn command_tick_rect(
    terminal: (f32, f32, f32, f32),
    column: (f32, f32),
    scale: f32,
    origin: u64,
    total: usize,
    mark: crate::term::CommandMark,
) -> Option<[f32; 4]> {
    let (x, y, w, h) = terminal;
    let row = mark.row.checked_sub(origin)?;
    if total == 0 || row >= total as u64 || w <= 0.0 || h <= 0.0 || scale <= 0.0 {
        return None;
    }
    let width = column.1.min(w);
    if width.is_nan() || width <= 0.0 {
        return None;
    }
    let left = column.0.max(x).min(x + w - width);
    let height = ((if mark.exit_code == 0 { 2.0 } else { 4.0 }) * scale).min(h);
    let top = ((row as f32 + 0.5) / total as f32 * h - height / 2.0).clamp(0.0, h - height);
    Some([left, y + top, width, height])
}

#[cfg(test)]
mod command_tick_tests {
    use super::*;
    use crate::term::CommandMark;

    #[test]
    fn ticks_share_bounded_geometry_at_every_scale_and_history_origin() {
        for scale in [1.0, 1.5, 2.0] {
            let rect = (20.0, 40.0, 200.0 * scale, 100.0 * scale);
            let column = (rect.0 + rect.2 - 8.0 * scale, 6.0 * scale);
            for (row, code) in [(500, 0), (599, 7)] {
                let [x, y, w, h] = command_tick_rect(
                    rect,
                    column,
                    scale,
                    500,
                    100,
                    CommandMark {
                        row,
                        exit_code: code,
                    },
                )
                .unwrap();
                assert!(x >= rect.0 && x + w <= rect.0 + rect.2);
                assert!(y >= rect.1 && y + h <= rect.1 + rect.3);
                assert_eq!(h, if code == 0 { 2.0 * scale } else { 4.0 * scale });
                assert_eq!((x, w), column, "a tick lies across the bar's column");
            }
            assert!(
                command_tick_rect(
                    rect,
                    column,
                    scale,
                    500,
                    100,
                    CommandMark {
                        row: 499,
                        exit_code: 0
                    }
                )
                .is_none()
            );
            assert!(
                command_tick_rect(
                    rect,
                    column,
                    scale,
                    500,
                    100,
                    CommandMark {
                        row: 600,
                        exit_code: 0
                    }
                )
                .is_none()
            );
        }
    }
}
