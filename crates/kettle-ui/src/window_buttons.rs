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

    pub(crate) fn reservations(&self, width: f32, height: f32) -> (f32, f32) {
        let count = self.left.len() + self.right.len();
        let slot = height.min(width / count.max(1) as f32);
        (
            slot * self.left.len() as f32,
            slot * self.right.len() as f32,
        )
    }
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
        assert_eq!(layout.reservations(800.0, 46.0), (92.0, 46.0));
        assert_eq!(layout.reservations(30.0, 46.0), (20.0, 10.0));
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
