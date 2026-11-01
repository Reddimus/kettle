use crate::{
    Rgb, UpdateToastGeometry, chrome, rounded_surface::RoundedSurface, theme::solid_blend,
};

pub(crate) struct Palette {
    background: Rgb,
    control: Rgb,
    border: Rgb,
    focus: Rgb,
    pub text: [Rgb; 3],
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
    Palette {
        background,
        control,
        border: solid_blend(theme.foreground, theme.background, 30),
        focus: colors.icon(accent, control),
        text: [text, control_text, control_text],
    }
}

pub(crate) fn surfaces(
    geometry: UpdateToastGeometry,
    palette: &Palette,
    opacity: f32,
    focused: Option<usize>,
) -> [RoundedSurface; 3] {
    let alpha = opacity.clamp(0.0, 1.0);
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
    [
        shape(
            geometry.rect,
            palette.background,
            palette.border,
            geometry.radius,
            geometry.unit,
        ),
        shape(
            geometry.details,
            palette.control,
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
            palette.control,
            palette.focus,
            geometry.close.2 * 0.5,
            if focused == Some(2) {
                2.0 * geometry.unit
            } else {
                0.0
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
