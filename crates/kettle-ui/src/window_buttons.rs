//! Bounded GNOME button-layout parsing, independent of the portal transport.
use kettle_render::WindowControl as C;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ButtonLayout {
    pub left: Vec<C>,
    pub right: Vec<C>,
}

impl Default for ButtonLayout {
    fn default() -> Self {
        Self {
            left: Vec::new(),
            right: vec![C::Minimize, C::Maximize, C::Close],
        }
    }
}

impl ButtonLayout {
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        if value.len() > 128 {
            return None;
        }
        let (left, right) = value.split_once(':')?;
        if right.contains(':') {
            return None;
        }
        let mut used = Vec::new();
        let mut side = |value: &str| -> Option<Vec<C>> {
            let mut buttons = Vec::new();
            for token in value.split(',').map(str::trim) {
                let action = match token {
                    "" | "menu" | "appmenu" => continue,
                    "minimize" => C::Minimize,
                    "maximize" => C::Maximize,
                    "close" => C::Close,
                    _ => return None,
                };
                if !used.contains(&action) {
                    used.push(action);
                    buttons.push(action);
                }
            }
            Some(buttons)
        };
        let left = side(left)?;
        let right = side(right)?;
        Some(Self { left, right })
    }

    /// The room the window buttons take at each end of a header `width`
    /// wide and `height` tall (46 points at 1x): the header's edge padding
    /// and the buttons, as libadwaita spaces them, and nothing for a side
    /// without buttons. A header too narrow for them all shrinks them evenly
    /// ([`ButtonLayout::fit`]).
    pub(crate) fn reservations(&self, width: f32, height: f32) -> (f32, f32) {
        let (unit, _) = self.fit(width, height);
        (
            group_width(self.left.len()) * unit,
            group_width(self.right.len()) * unit,
        )
    }

    /// Points to device pixels for a header `width` wide and `height` tall:
    /// the header's scale, less wherever the buttons would not fit, and that
    /// scale before the shrinking.
    pub(crate) fn fit(&self, width: f32, height: f32) -> (f32, f32) {
        let scale = (height / HEADER_HEIGHT).max(0.0);
        let total = (group_width(self.left.len()) + group_width(self.right.len())) * scale;
        let unit = if total > width.max(0.0) && total > 0.0 {
            scale * width.max(0.0) / total
        } else {
            scale
        };
        (unit, scale)
    }
}

/// The GNOME header's height at 1x, in points.
pub(crate) const HEADER_HEIGHT: f32 = 46.0;

/// The header's padding at its sides, before its outermost buttons:
/// libadwaita's 7 points.
pub(crate) const HEADER_EDGE: f32 = 7.0;

/// A window button's target: libadwaita's 24-point disc and the button's
/// 5-point padding around it.
pub(crate) const WINDOW_BUTTON: f32 = 34.0;

/// Between two window buttons' targets: libadwaita's `border-spacing`.
pub(crate) const WINDOW_BUTTON_SPACING: f32 = 3.0;

/// Points from the header's end past `count` window buttons.
pub(crate) fn group_width(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    HEADER_EDGE + count as f32 * WINDOW_BUTTON + (count - 1) as f32 * WINDOW_BUTTON_SPACING
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_header_button_layout_honors_sides_order_and_omissions() {
        assert_eq!(
            ButtonLayout::parse(":minimize,maximize,close"),
            Some(ButtonLayout::default())
        );
        let layout = ButtonLayout::parse("close,minimize:maximize").unwrap();
        assert_eq!(layout.left, [C::Close, C::Minimize]);
        assert_eq!(layout.right, [C::Maximize]);
        // Two buttons and the edge on the left, one on the right.
        assert_eq!(layout.reservations(800.0, 46.0), (78.0, 41.0));
        assert_eq!(layout.reservations(1600.0, 92.0), (156.0, 82.0));
        let (left, right) = layout.reservations(30.0, 46.0);
        assert!((left + right - 30.0).abs() < 1e-3 && left > right);
        let layout = ButtonLayout::parse("appmenu:close,close").unwrap();
        assert!(layout.left.is_empty());
        assert_eq!(layout.right, [C::Close]);
        let empty = ButtonLayout::parse(":").unwrap();
        assert!(empty.left.is_empty() && empty.right.is_empty());
        for value in ["close", "a:b:c", ":unknown", &"x".repeat(129)] {
            assert!(ButtonLayout::parse(value).is_none());
        }
    }
}
