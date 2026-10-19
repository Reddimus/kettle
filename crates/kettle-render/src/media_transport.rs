//! Compact player chrome, kept out of terminal-cell instance buffers.
use crate::rounded_surface::RoundedSurface;
use crate::{
    CompletionPalette, MediaLaneGeometry, MediaLaneHit, MediaLanePanel, MediaLanePlay, Rgb,
};

pub(crate) struct TransportPaint {
    pub shapes: Vec<RoundedSurface>,
}

pub(crate) fn paint(
    lane: &MediaLanePanel,
    geometry: &MediaLaneGeometry,
    palette: CompletionPalette,
    pointer: Option<(f32, f32)>,
    scrubbing: bool,
    scale: f32,
) -> Option<TransportPaint> {
    let transport = geometry.transport?;
    let play = geometry.play?;
    let unit = scale;
    let hovered = pointer.and_then(|(x, y)| geometry.hit_test(x, y));
    let dark = Rgb::new(18, 18, 18);
    let white = Rgb::new(255, 255, 255);
    let progress = progress_color(palette);
    let mut shapes = Vec::with_capacity(14);
    let mut shape =
        |r: crate::Rect4, fill: Rgb, radius: f32, border: f32, kind: f32, state: f32| {
            if r.2 > 0.0 && r.3 > 0.0 {
                let rgba = |c: Rgb| {
                    [
                        c.r as f32 / 255.0,
                        c.g as f32 / 255.0,
                        c.b as f32 / 255.0,
                        1.0,
                    ]
                };
                shapes.push(RoundedSurface {
                    pos: [r.0, r.1],
                    size: [r.2, r.3],
                    fill: if border > 0.0 { [0.0; 4] } else { rgba(fill) },
                    stroke: rgba(fill),
                    params: [radius, border, kind, state],
                });
            }
        };
    if let Some((circle, feedback)) = geometry.play_feedback {
        shape(circle, Rgb::new(0, 0, 0), circle.3 / 2.0, 0.0, 4.0, 0.78);
        let size = circle.3 * 0.56;
        let icon = (
            circle.0 + (circle.2 - size) / 2.0,
            circle.1 + (circle.3 - size) / 2.0,
            size,
            size,
        );
        match feedback {
            MediaLanePlay::Play => shape(icon, white, 0.0, 0.0, 1.0, 0.0),
            MediaLanePlay::Pause => {
                for offset in [5.0, 14.0] {
                    shape(
                        (
                            icon.0 + offset / 24.0 * size,
                            icon.1 + size / 8.0,
                            5.0 / 24.0 * size,
                            size * 0.75,
                        ),
                        white,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                    );
                }
            }
            MediaLanePlay::Stop => shape(
                (
                    icon.0 + size / 6.0,
                    icon.1 + size / 6.0,
                    size * 2.0 / 3.0,
                    size * 2.0 / 3.0,
                ),
                white,
                0.0,
                0.0,
                0.0,
                0.0,
            ),
        }
    }
    shape(transport, dark, 0.0, 0.0, 0.0, 0.0);
    for (rect, hit) in [
        (Some(play), MediaLaneHit::Play),
        (geometry.sound, MediaLaneHit::Sound),
        (geometry.theater, MediaLaneHit::Theater),
        (geometry.fullscreen, MediaLaneHit::Fullscreen),
    ] {
        let Some(r) = rect.filter(|r| r.1 >= transport.1) else {
            continue;
        };
        if hovered == Some(hit) {
            let inset = 4.0 * unit;
            shape(
                (
                    r.0 + inset,
                    r.1 + inset,
                    r.2 - 2.0 * inset,
                    r.3 - 2.0 * inset,
                ),
                Rgb::new(48, 48, 48),
                3.0 * unit,
                0.0,
                0.0,
                0.0,
            );
        }
        let size = 24.0 * unit;
        let icon = (
            r.0 + (r.2 - size) / 2.0,
            r.1 + (r.3 - size) / 2.0,
            size,
            size,
        );
        match hit {
            MediaLaneHit::Play if lane.play == Some(MediaLanePlay::Pause) => {
                for offset in [5.0, 14.0] {
                    shape(
                        (
                            icon.0 + offset * unit,
                            icon.1 + 3.0 * unit,
                            5.0 * unit,
                            18.0 * unit,
                        ),
                        white,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                    );
                }
            }
            MediaLaneHit::Play if lane.play == Some(MediaLanePlay::Stop) => shape(
                (
                    icon.0 + 4.0 * unit,
                    icon.1 + 4.0 * unit,
                    16.0 * unit,
                    16.0 * unit,
                ),
                white,
                0.0,
                0.0,
                0.0,
                0.0,
            ),
            MediaLaneHit::Play => shape(icon, white, 0.0, 0.0, 1.0, 0.0),
            MediaLaneHit::Theater => shape(
                (icon.0, icon.1 + 4.0 * unit, size, 16.0 * unit),
                white,
                0.0,
                2.0 * unit,
                0.0,
                0.0,
            ),
            MediaLaneHit::Fullscreen => shape(icon, white, 0.0, 0.0, 2.0, 0.0),
            MediaLaneHit::Sound => shape(
                icon,
                white,
                0.0,
                0.0,
                3.0,
                f32::from(lane.sound == Some(crate::MediaLaneSound::Muted)),
            ),
            _ => {}
        }
    }
    if let Some(track) = geometry.transport_timeline {
        let active = scrubbing || hovered == Some(MediaLaneHit::Scrub);
        let thickness = if active { 5.0 } else { 3.0 } * unit;
        let top = track.1 + (track.3 - thickness) / 2.0;
        let along = lane
            .scrubber
            .as_ref()
            .filter(|bar| bar.duration_ms > 0)
            .map_or(0.0, |bar| {
                (bar.position_ms.min(bar.duration_ms) as f64 / bar.duration_ms as f64) as f32
            });
        let played = track.2 * along;
        shape(
            (track.0, top, track.2, thickness),
            Rgb::new(112, 112, 112),
            0.0,
            0.0,
            0.0,
            0.0,
        );
        shape(
            (track.0, top, played, thickness),
            progress,
            0.0,
            0.0,
            0.0,
            0.0,
        );
        if active {
            let diameter = (12.0 * unit).min(track.3);
            let x = crate::media_lane::knob_left(track, played, diameter);
            shape(
                (x, track.1 + (track.3 - diameter) / 2.0, diameter, diameter),
                progress,
                diameter / 2.0,
                0.0,
                0.0,
                0.0,
            );
        }
    }
    if let Some(focus) = lane.focused_control.filter(|r| r.1 >= transport.1) {
        shape(focus, white, 2.0 * unit, 2.0 * unit, 0.0, 0.0);
    }
    Some(TransportPaint { shapes })
}

fn progress_color(palette: CompletionPalette) -> Rgb {
    crate::color::with_min_contrast(palette.emphasis, Rgb::new(18, 18, 18), 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_uses_the_theme_accent_and_preserves_visibility() {
        let dark = Rgb::new(18, 18, 18);
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let palette = crate::completion_palette(&theme, theme.palette[4]);
            let progress = progress_color(palette);
            assert!(
                crate::color::contrast_ratio(progress, dark) + 1e-6 >= 3.0,
                "progress contrast in {name}"
            );
            if crate::color::contrast_ratio(palette.emphasis, dark) >= 3.0 {
                assert_eq!(
                    progress, palette.emphasis,
                    "retain visible theme accent in {name}"
                );
            }
        }
    }
}
