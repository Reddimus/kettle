use super::*;
use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::headless_tests::{capture, pane, snapshot_of};
use alacritty_terminal::selection::SelectionRange;
use kettle_core::{Column, Line, Point};

#[test]
fn c09_gpu_selection_perimeter_and_row_seams_match_the_resolved_fill() {
    let _serial = gpu_test_guard();
    for scale in [1.0, 2.0] {
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            let mut cfg = gpu_test_config();
            cfg.text_renderer = mode;
            cfg.background_opacity = 1.0;
            cfg.show_titlebar = false;
            let (width, height) = ((640.0 * scale) as u32, (240.0 * scale) as u32);
            let Some(mut renderer) =
                Renderer::headless_for_tests_with(&cfg, width, height, scale, true)
                    .expect("renderer builds")
            else {
                eprintln!("C09_GPU_SKIPPED: no adapter");
                return;
            };
            for (name, program_background) in [
                ("TokyoNight Night", false),
                ("Kettle Day", false),
                ("Nord", true),
            ] {
                let mut bytes = b"\x1b[?25l".to_vec();
                if program_background {
                    bytes.extend_from_slice(b"\x1b[48;2;240;240;240m\x1b[2J");
                }
                bytes.extend_from_slice(b"\x1b[3;11Hselected text");
                let mut snap = snapshot_of(24, 6, &bytes);
                snap.selection = Some(SelectionRange::new(
                    Point::new(Line(1), Column(2)),
                    Point::new(Line(3), Column(8)),
                    false,
                ));
                cfg.theme = cfg.theme_by_name(name);
                for focused in [true, false] {
                    let tr = kettle_i18n::Translator::new(if focused {
                        kettle_i18n::Language::En
                    } else {
                        kettle_i18n::Language::Es
                    });
                    let status = selection_hint_layout(
                        (0.0, 0.0, 240.0 * scale, height as f32),
                        (
                            renderer.overlay_text_cell_width(),
                            renderer.overlay_text_line_height(),
                        ),
                        &tr.selection_gesture_hint("Alt"),
                    )
                    .unwrap();
                    let painted_text = status.text.clone();
                    let measured_width = status.rect.2 - 12.0;
                    let overlay = Overlay {
                        window_focused: focused,
                        cursor_visible: false,
                        window_opaque: true,
                        pane_statuses: vec![status],
                        ..Overlay::default()
                    };
                    let frame =
                        capture(&mut renderer, &cfg, &[pane(&snap, width, height)], &overlay);
                    let shaped = &renderer.pane_status_buffers[0];
                    assert_eq!(
                        shaped
                            .lines
                            .iter()
                            .map(|line| line.text())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        painted_text,
                        "measure and paint the same localized text"
                    );
                    assert!(
                        shaped
                            .layout_runs()
                            .all(|line| line.line_w <= measured_width + 0.1),
                        "the measured hint must fit its painted bounds"
                    );
                    let fills = theme::PaneSelectionFills::new(
                        &cfg.theme,
                        cfg.theme.background,
                        1.0,
                        &mut MinimumContrastCache::default(),
                    );
                    let background = if program_background {
                        Rgb::new(240, 240, 240)
                    } else {
                        cfg.theme.background
                    };
                    let expected = fills.cell(
                        if focused {
                            CellHighlight::Selection
                        } else {
                            CellHighlight::InactiveSelection
                        },
                        &cfg,
                        &cfg.theme,
                        background,
                        &mut MinimumContrastCache::default(),
                    );
                    let (ox, oy) = pane_grid_origin(
                        (0.0, 0.0, width as f32, height as f32),
                        (cfg.padding_x, cfg.padding_y),
                    );
                    let (cw, ch) = (renderer.cell_w, renderer.cell_h);
                    let x = (ox + 5.5 * cw) as u32;
                    let y = (oy + 1.5 * ch) as u32;
                    let at = |x, y| {
                        let p = frame.get_pixel(x, y);
                        Rgb::new(p[0], p[1], p[2])
                    };
                    let actual = at(x, y);
                    assert_eq!(
                        actual, expected,
                        "{name}, scale {scale}, focused={focused}: fill"
                    );
                    let top = (oy + ch).round() as u32;
                    let stroke = at(x, top);
                    let minimum = if focused { 3.0 } else { 2.0 };
                    if color::contrast_ratio(actual, background) < minimum {
                        assert_ne!(
                            stroke, actual,
                            "{name}, scale {scale}, focused={focused}: the low-separation fill needs a perimeter"
                        );
                        assert!(
                            color::contrast_ratio(stroke, background) >= minimum,
                            "{name}: painted perimeter {stroke:?}"
                        );
                    } else {
                        assert_eq!(stroke, actual, "a visible fill needs no extra perimeter");
                    }
                    assert_eq!(at(x, top + scale as u32), actual, "one logical pixel thick");
                    for boundary in [2.0, 3.0] {
                        let edge = (oy + boundary * ch).round() as u32;
                        assert_eq!(at(x, edge - 1), actual, "no bottom seam");
                        assert_eq!(at(x, edge), actual, "no top seam");
                    }
                    if let Ok(directory) = std::env::var("KETTLE_C09_CAPTURE_DIR") {
                        frame
                            .save(
                                std::path::Path::new(&directory)
                                    .join(format!("{name}-{scale}-{mode:?}-{focused}.png")),
                            )
                            .expect("save review capture");
                    }
                }
            }
        }
    }
    eprintln!(
        "C09_GPU_ACCEPTED: Grid/Legacy, 1x/2x, dark/light, active/inactive, exposed perimeter and no row seams"
    );
}
