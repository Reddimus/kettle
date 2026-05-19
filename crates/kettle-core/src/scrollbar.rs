//! Pure scrollback-scrollbar geometry: the thumb rectangle to draw and the
//! display offset a click maps to. Shared by the renderer (drawing) and the
//! UI (click-to-jump) so both agree, and fully unit-testable without a GPU.
//!
//! `rows` = visible lines, `hist` = scrollback lines, `off` = lines scrolled
//! back from the bottom (`0` = at the prompt). `total = rows + hist`.

/// The thumb as `(y_offset_within_track, height)` in track pixels, or `None`
/// when everything fits (no scrollbar needed). The thumb is at least 12 px
/// tall and never overflows the track.
pub fn thumb(rows: usize, hist: usize, off: usize, track: f32) -> Option<(f32, f32)> {
    let total = rows + hist;
    if total <= rows || track <= 0.0 {
        return None;
    }
    let h = (track * rows as f32 / total as f32).max(12.0).min(track);
    // `off` lines back from the bottom → `hist - off` lines from the top.
    let from_top = (hist.saturating_sub(off)) as f32 / total as f32;
    let y = (from_top * track).clamp(0.0, track - h);
    Some((y, h))
}

/// True when new output appeared since the previous frame and the user
/// asked for `scroll-on-output` (Alacritty `scroll_on_output`). The history
/// snapshot from the prior redraw is `prev`; `current` is the value now.
/// `None` previous means we haven't seen a frame yet — never scroll
/// (otherwise the very first paint would yank the cursor away from the
/// origin). Pure, +tests so the rule lives outside the render path.
pub fn should_scroll_on_output(enabled: bool, prev: Option<usize>, current: usize) -> bool {
    enabled && prev.is_some_and(|p| current > p)
}

/// The `display_offset` a click/drag at `yrel` pixels down the `track` maps
/// to: the clicked fraction becomes the top of the viewport. Clamped to
/// `0..=hist`.
pub fn target_offset(yrel: f32, track: f32, rows: usize, hist: usize) -> usize {
    if hist == 0 || track <= 0.0 {
        return 0;
    }
    let total = (rows + hist) as f32;
    let frac = (yrel / track).clamp(0.0, 1.0);
    // from_top = (hist - off) / total  ⇒  off = hist - frac*total.
    let off = hist as f32 - frac * total;
    off.round().clamp(0.0, hist as f32) as usize
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
        assert_eq!(hmin, 12.0);
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
        // Mid track → ~half the history scrolled back.
        let mid = target_offset(200.0, 400.0, 40, 160);
        assert_eq!(mid, 60, "160 - 0.5*200 = 60");
        // Out-of-range y is clamped, not panicking.
        assert_eq!(target_offset(-50.0, 400.0, 40, 160), 160);
        assert_eq!(target_offset(9999.0, 400.0, 40, 160), 0);
        // No history → always 0.
        assert_eq!(target_offset(100.0, 400.0, 40, 0), 0);
    }
}
