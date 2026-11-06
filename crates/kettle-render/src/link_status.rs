use crate::{
    Rect4,
    labels::{display_width, fit_single_line_label, middle_ellipsis},
    rects_overlap,
};

/// Passive pane chrome. Bounds are shared by paint, accessibility and diagnostics;
/// it takes no terminal cells and never moves the terminal's IME anchor.
#[derive(Clone, Debug)]
pub struct PaneStatus {
    pub rect: Rect4,
    pub text: String,
    pub description: String,
    /// Full destination for assistive technology, absent for the hint legend.
    pub target: Option<String>,
    pub link_bounds: Option<Rect4>,
}

pub fn link_status_layout(
    pane: Rect4,
    cell: (f32, f32),
    target: &str,
    gesture: &str,
    hovered: &[Rect4],
) -> Option<PaneStatus> {
    let (cw, ch) = cell;
    if cw <= 0.0 || ch <= 0.0 || pane.2 < cw * 4.0 || pane.3 < ch {
        return None;
    }
    let columns = ((pane.2 - 12.0).max(0.0) / cw).floor() as usize;
    let gesture_width = display_width(gesture) + 3;
    let text = if columns > gesture_width + 3 {
        format!(
            "{} | {gesture}",
            middle_ellipsis(target, columns - gesture_width)
        )
    } else {
        if pane.3 < ch * 2.0 {
            return None;
        }
        format!(
            "{}\n{}",
            middle_ellipsis(target, columns),
            fit_single_line_label(gesture, columns)
        )
    };
    let width = (text.lines().map(display_width).max().unwrap_or(0) as f32 * cw + 12.0).min(pane.2);
    let height = text.lines().count() as f32 * ch;
    let mut rect = (pane.0, pane.1 + pane.3 - height, width, height);
    if hovered.iter().any(|link| rects_overlap(rect, *link)) {
        rect.0 = pane.0 + pane.2 - width;
        // On a very narrow pane neither corner may be free. Move above the
        // hovered row rather than hiding the destination's visible text.
        if hovered.iter().any(|link| rects_overlap(rect, *link)) {
            let top = hovered.iter().map(|r| r.1).fold(rect.1, f32::min);
            rect.1 = top - height;
            if rect.1 < pane.1 {
                return None;
            }
        }
    }
    Some(PaneStatus {
        rect,
        text,
        description: format!("{target} | {gesture}"),
        target: Some(target.to_string()),
        link_bounds: hovered.iter().copied().reduce(|left, right| {
            let (x, y) = (left.0.min(right.0), left.1.min(right.1));
            (
                x,
                y,
                (left.0 + left.2).max(right.0 + right.2) - x,
                (left.1 + left.3).max(right.1 + right.3) - y,
            )
        }),
    })
}

pub fn hint_legend_layout(pane: Rect4, cell: (f32, f32), legend: &str) -> Option<PaneStatus> {
    let (cw, ch) = cell;
    if cw <= 0.0 || ch <= 0.0 || pane.2 <= 12.0 || pane.3 < ch {
        return None;
    }
    let columns = ((pane.2 - 12.0) / cw).floor() as usize;
    let mut lines: Vec<String> = Vec::new();
    for segment in legend.split(" · ") {
        if let Some(last) = lines.last_mut()
            && display_width(last) + display_width(segment) + 3 <= columns
        {
            last.push_str(" · ");
            last.push_str(segment);
        } else {
            lines.push(fit_single_line_label(segment, columns));
        }
    }
    let max_lines = (pane.3 / ch).floor() as usize;
    if lines.len() > max_lines {
        // Keep cancellation available even when only one row fits.
        let cancel = lines.pop().unwrap_or_default();
        lines.truncate(max_lines.saturating_sub(1));
        lines.push(cancel);
    }
    let height = lines.len() as f32 * ch;
    Some(PaneStatus {
        rect: (pane.0, pane.1 + pane.3 - height, pane.2, height),
        text: lines.join("\n"),
        description: legend.to_string(),
        target: None,
        link_bounds: None,
    })
}

/// Keep the legend near the bottom without hiding chips on the last row.
/// Dense panes can fill every row; paint keeps their chips above the legend.
pub fn avoid_hint_labels(status: &mut PaneStatus, pane: Rect4, labels: &[Rect4]) {
    let mut candidate = status.rect;
    while let Some(top) = labels
        .iter()
        .filter(|label| rects_overlap(candidate, **label))
        .map(|label| label.1)
        .min_by(f32::total_cmp)
    {
        candidate.1 = top - candidate.3;
        if candidate.1 < pane.1 {
            return;
        }
    }
    status.rect = candidate;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hover_pill_elides_the_middle_and_avoids_the_link() {
        let pane = (20.0, 30.0, 800.0, 320.0);
        let target = format!("https://host/{}/file.rs", "x".repeat(160));
        let left =
            link_status_layout(pane, (8.0, 16.0), &target, "Ctrl+click to open", &[]).unwrap();
        assert_eq!(left.rect.0, 20.0);
        assert!(left.text.starts_with("https://host/"));
        assert!(left.text.contains('…'));
        assert!(left.text.ends_with("/file.rs | Ctrl+click to open"));
        assert_eq!(left.target.as_deref(), Some(target.as_str()));
        let right = link_status_layout(
            pane,
            (8.0, 16.0),
            "https://x.test",
            "Click to open",
            &[(20.0, 334.0, 80.0, 16.0)],
        )
        .unwrap();
        assert!(right.rect.0 > 20.0);
        assert_eq!(right.rect.0 + right.rect.2, 820.0);
        assert!(!rects_overlap(right.rect, (20.0, 334.0, 80.0, 16.0)));
    }
    #[test]
    fn narrow_pill_moves_above_a_full_width_link() {
        let pane = (0.0, 0.0, 120.0, 64.0);
        let link = (0.0, 48.0, 120.0, 16.0);
        let pill = link_status_layout(
            pane,
            (8.0, 16.0),
            "https://x.test/long",
            "Click to open",
            &[link],
        )
        .unwrap();
        assert!(!rects_overlap(pill.rect, link));
        assert!(
            link_status_layout(
                (0.0, 0.0, 120.0, 16.0),
                (8.0, 16.0),
                "x",
                "open",
                &[(0.0, 0.0, 120.0, 16.0)]
            )
            .is_none()
        );
    }
    #[test]
    fn legend_is_contained_in_the_pane() {
        let legend = hint_legend_layout(
            (50.0, 20.0, 400.0, 200.0),
            (8.0, 16.0),
            "Label: copy | Esc to cancel",
        )
        .unwrap();
        assert_eq!(legend.rect, (50.0, 204.0, 400.0, 16.0));
        assert!(legend.text.contains("Esc to cancel"));
    }
    #[test]
    fn legend_moves_clear_of_bottom_row_labels_and_keeps_cancel_when_wrapped() {
        let pane = (0.0, 0.0, 400.0, 200.0);
        let mut legend = hint_legend_layout(
            pane,
            (8.0, 16.0),
            "Label: open URL / copy text · Shift+label: copy URL / open path · Esc to cancel",
        )
        .unwrap();
        assert!(legend.text.ends_with("Esc to cancel"));
        assert!(legend.text.contains("Shift+label"));
        assert!(legend.text.contains('\n'));
        let label = (0.0, 184.0, 8.0, 16.0);
        avoid_hint_labels(&mut legend, pane, &[label]);
        assert!(!rects_overlap(legend.rect, label));
        assert!(legend.rect.1 >= pane.1);
    }
    #[test]
    fn wrapped_hover_accessibility_bounds_cover_every_segment() {
        let pill = link_status_layout(
            (0.0, 0.0, 800.0, 400.0),
            (8.0, 16.0),
            "https://x.test/a/long/path",
            "Ctrl+click to open",
            &[(80.0, 16.0, 320.0, 16.0), (0.0, 32.0, 120.0, 16.0)],
        )
        .unwrap();
        assert_eq!(pill.link_bounds, Some((0.0, 16.0, 400.0, 32.0)));
    }
}
