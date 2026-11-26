use crate::{Rgb, UpdateToastOverlay, chrome, rounded_surface::RoundedSurface, theme::solid_blend};

pub(crate) struct Palette {
    background: Rgb,
    control: Rgb,
    /// A button under the pointer, and its label on it.
    hover: Rgb,
    pub hover_text: Rgb,
    border: Rgb,
    focus: Rgb,
    pub text: [Rgb; 3],
}

/// A hovered toast button's fill: its own moved 9% toward black or white,
/// as a confirmation dialog's buttons are.
fn hover_fill(control: Rgb) -> Rgb {
    let target = if crate::color::relative_luminance(control) > 0.4 {
        Rgb::new(0, 0, 0)
    } else {
        Rgb::new(255, 255, 255)
    };
    crate::color::over(control, target, 0.09, false)
}

impl Palette {
    /// The label colour of toast part `part` (0 the message, 1 details, 2
    /// close) while `hovered` is the button under the pointer.
    pub(crate) fn text_for(&self, part: usize, hovered: Option<usize>) -> Rgb {
        if part > 0 && hovered == Some(part) {
            self.hover_text
        } else {
            self.text[part.min(2)]
        }
    }
}

pub(crate) fn palette(
    theme: &kettle_config::Theme,
    accent: Rgb,
    colors: &mut chrome::Colors,
) -> Palette {
    let background = solid_blend(theme.foreground, theme.background, 9);
    let control = solid_blend(accent, background, 12);
    let text = colors.text(theme.foreground, background);
    let control_text = colors.text(theme.foreground, control);
    let hover = hover_fill(control);
    Palette {
        background,
        control,
        hover,
        hover_text: colors.text(theme.foreground, hover),
        border: solid_blend(theme.foreground, theme.background, 30),
        focus: colors.icon(accent, control),
        text: [text, control_text, control_text],
    }
}

pub(crate) fn surfaces(toast: &UpdateToastOverlay, palette: &Palette) -> Vec<RoundedSurface> {
    let geometry = toast.control_geometry();
    let focused = toast.focused;
    let fill = |part| {
        if toast.hovered == Some(part) {
            palette.hover
        } else {
            palette.control
        }
    };
    let alpha = toast.opacity.clamp(0.0, 1.0);
    let rgba = |color: Rgb, alpha| {
        [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            alpha,
        ]
    };
    let shape = |(x, y, w, h), fill, stroke, radius, border| RoundedSurface {
        pos: [x, y],
        size: [w, h],
        fill: rgba(fill, alpha),
        stroke: rgba(stroke, alpha),
        params: [radius, border, 0.0, 0.0],
    };
    let surfaces = [
        shape(
            geometry.rect,
            palette.background,
            palette.border,
            geometry.radius,
            geometry.unit,
        ),
        shape(
            geometry.details,
            fill(1),
            palette.focus,
            (16.0 * geometry.unit).min(geometry.details.3 * 0.5),
            if focused == Some(1) {
                2.0 * geometry.unit
            } else {
                0.0
            },
        ),
        shape(
            geometry.close,
            fill(2),
            palette.focus,
            geometry.close.2 * 0.5,
            if focused == Some(2) {
                2.0 * geometry.unit
            } else {
                0.0
            },
        ),
    ];
    surfaces
        .into_iter()
        .filter(|surface| surface.size[0] > 0.0 && surface.size[1] > 0.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_button_labels_have_no_geometry_hits_or_painted_surface() {
        let theme = kettle_config::Theme::default();
        let palette = palette(&theme, theme.foreground, &mut chrome::Colors::default());
        let geometry =
            crate::persistent_notice_geometry((0.0, 0.0, 800.0, 600.0), (8.0, 16.0), 2, 0, 7)
                .unwrap();
        assert_eq!(geometry.details.2, 0.0);
        assert_eq!(
            geometry.hit_test(geometry.details.0 + 1.0, geometry.details.1 + 1.0),
            Some(crate::UpdateToastHit::Body)
        );
        // The shared painter also rejects empty labels with stale/nonempty geometry.
        let geometry =
            crate::persistent_notice_geometry((0.0, 0.0, 800.0, 600.0), (8.0, 16.0), 2, 12, 7)
                .unwrap();
        let mut toast = crate::UpdateToastOverlay {
            geometry,
            message: String::new(),
            details: String::new(),
            close: "Dismiss".into(),
            opacity: 1.0,
            focused: Some(1),
            hovered: None,
        };
        assert_eq!(surfaces(&toast, &palette).len(), 2);
        assert_eq!(toast.control_geometry().details.2, 0.0);
        toast.close.clear();
        assert_eq!(surfaces(&toast, &palette).len(), 1);
    }

    /// Should 12 of the 6.0 UI review: a toast's Show and Dismiss, and a
    /// notice's Open config and Dismiss, had no hover. The button under the
    /// pointer now takes the confirmation buttons' 9% step, alone, and its
    /// label keeps 4.5:1 on it in every theme.
    #[test]
    fn the_toast_button_under_the_pointer_lights_alone() {
        let theme = kettle_config::Theme::default();
        let palette = palette(&theme, theme.palette[4], &mut chrome::Colors::default());
        let geometry =
            crate::persistent_notice_geometry((0.0, 0.0, 800.0, 600.0), (8.0, 16.0), 2, 12, 7)
                .unwrap();
        let mut toast = crate::UpdateToastOverlay {
            geometry,
            message: "Config has errors".into(),
            details: "Open config".into(),
            close: "Dismiss".into(),
            opacity: 1.0,
            focused: None,
            hovered: None,
        };
        let rest = surfaces(&toast, &palette);
        for part in [1, 2] {
            toast.hovered = Some(part);
            let lit = surfaces(&toast, &palette);
            for (index, (rest, lit)) in rest.iter().zip(&lit).enumerate() {
                assert_eq!(
                    rest.fill != lit.fill,
                    index == part,
                    "part {part}, surface {index}"
                );
            }
        }
        let mut colors = chrome::Colors::default();
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            for accent in theme.palette {
                let p = super::palette(&theme, accent, &mut colors);
                assert_ne!(p.hover, p.control, "{name}");
                assert!(
                    crate::color::contrast_ratio(p.text_for(1, Some(1)), p.hover) + 1e-6 >= 4.5,
                    "{name}"
                );
                assert_eq!(p.text_for(1, Some(2)), p.text[1]);
                assert_eq!(p.text_for(0, Some(1)), p.text[0]);
            }
        }
        let src: String = crate::production_source().split_whitespace().collect();
        assert!(src.contains("letink=palette.text_for(i,toast.hovered);"));
    }

    #[test]
    fn toast_palette_preserves_text_and_focus_contrast_in_all_themes() {
        let mut colors = chrome::Colors::default();
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            for accent in theme.palette {
                let p = palette(&theme, accent, &mut colors);
                for (text, background) in [
                    (p.text[0], p.background),
                    (p.text[1], p.control),
                    (p.text[2], p.control),
                ] {
                    assert!(
                        crate::color::contrast_ratio(text, background) + 1e-6 >= 4.5,
                        "{name}"
                    );
                }
                assert!(
                    crate::color::contrast_ratio(p.focus, p.control) + 1e-6 >= 3.0,
                    "{name}"
                );
            }
        }
    }
}
