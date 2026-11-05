use super::{Rect4, Translator, display_width};

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollbackPill {
    pub rect: Rect4,
    pub label: String,
    pub accessible_label: String,
    pub text_origin: (f32, f32),
}

/// Shared paint, pointer and accessibility geometry. Keep clear of the scrollbar.
pub fn scrollback_pill(
    terminal: Rect4,
    offset: usize,
    scale: f32,
    text_cell: (f32, f32),
    scrollbar_width: f32,
    tr: &Translator,
) -> Option<ScrollbackPill> {
    let (x, y, width, height) = terminal;
    let (cell_width, line_height) = text_cell;
    if offset == 0
        || ![x, y, width, height, scale, cell_width, line_height]
            .into_iter()
            .all(f32::is_finite)
        || scale <= 0.0
        || cell_width <= 0.0
        || line_height <= 0.0
    {
        return None;
    }
    let digits = offset.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let label = tr.scrollback_return(&grouped, offset as u64);
    let pad = 6.0 * scale;
    let gutter = (scrollbar_hit_width(scrollbar_width, scale) + 2.0 * scale).max(14.0 * scale);
    let w = display_width(&label) as f32 * cell_width + 2.0 * pad;
    let h = line_height + pad;
    // Tiny panes cannot display the button without hiding its label.
    if width < w + gutter + pad || height < h + 2.0 * pad {
        return None;
    }
    let left = x + width - gutter - w;
    let top = y + height - pad - h;
    Some(ScrollbackPill {
        rect: (left, top, w, h),
        label,
        accessible_label: tr.scrollback_a11y(&grouped, offset as u64),
        text_origin: (left + pad, top + pad / 2.0),
    })
}

pub fn scrollbar_draw_width(configured: f32, scale: f32, hovered: bool) -> f32 {
    let width = configured.clamp(2.0, 40.0);
    (if hovered { width.max(10.0) } else { width }) * scale
}

pub fn scrollbar_hit_width(configured: f32, scale: f32) -> f32 {
    (configured.clamp(2.0, 40.0) + 2.0).max(12.0) * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_follows_pane_and_disappears_at_bottom() {
        let tr = Translator::default();
        assert!(
            scrollback_pill((50.0, 80.0, 800.0, 600.0), 0, 2.0, (16.0, 32.0), 4.0, &tr).is_none()
        );
        let pill = scrollback_pill(
            (50.0, 80.0, 800.0, 600.0),
            1234,
            2.0,
            (16.0, 32.0),
            4.0,
            &tr,
        )
        .unwrap();
        assert_eq!(pill.label, "\u{2193} 1,234 lines");
        assert_eq!(pill.rect.0 + pill.rect.2, 822.0);
        assert_eq!(pill.rect.1 + pill.rect.3, 668.0);
        let es = Translator::new(kettle_i18n::Language::Es);
        assert!(
            scrollback_pill((0.0, 0.0, 800.0, 600.0), 1, 1.0, (8.0, 16.0), 4.0, &es)
                .unwrap()
                .label
                .ends_with("l\u{ed}nea")
        );
        assert!(scrollback_pill((0.0, 0.0, 1.0, 1.0), 10, 1.0, (8.0, 16.0), 4.0, &tr).is_none());
    }

    #[test]
    fn pill_leaves_room_for_a_wide_scrollbar_and_scaled_hit_target() {
        let pill = scrollback_pill(
            (0.0, 0.0, 800.0, 600.0),
            1234,
            2.0,
            (16.0, 32.0),
            40.0,
            &Translator::default(),
        )
        .unwrap();
        assert!(pill.rect.0 + pill.rect.2 <= 800.0 - scrollbar_hit_width(40.0, 2.0));
        assert_eq!(scrollbar_hit_width(4.0, 2.0), 24.0);
        assert!(scrollbar_hit_width(40.0, 2.0) > scrollbar_draw_width(40.0, 2.0, true));
    }

    #[test]
    fn hovered_scrollbar_is_at_least_ten_scaled_pixels() {
        assert_eq!(scrollbar_draw_width(4.0, 2.0, false), 8.0);
        assert_eq!(scrollbar_draw_width(4.0, 2.0, true), 20.0);
        assert_eq!(scrollbar_draw_width(20.0, 2.0, true), 40.0);
    }
}
