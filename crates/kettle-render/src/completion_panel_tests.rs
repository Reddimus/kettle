use super::{
    COMPLETION_MAX_COLUMNS, COMPLETION_MIN_COLUMNS, CompletionOverlay, CompletionOverlayRow,
    CompletionPanelPlacement, MAX_COMPLETION_ROWS, MediaPasteReceiptHit, MediaPasteReceiptKind,
    MediaPasteReceiptOverlay, Overlay, completion_header_columns, completion_header_count,
    completion_header_label, completion_match_span, completion_overlay_row_rects,
    completion_palette, completion_panel_geometry, completion_scroll_thumb,
    completion_selection_surface, display_width, media_paste_receipt_geometry,
    media_paste_receipt_text, production_source, push_completion_selection_quads, solid_blend,
    text_overlay_requires_continuous_prepare,
};
use crate::color;
use kettle_config::Rgb;

/// A 900x700 pane whose 884x656 grid starts at (108, 70) with an 8x16 cell.
/// The prompt begins on visible row 20, so the grid's prompt row starts at
/// y = 70 + 320 = 390.
const CELL: (f32, f32) = (8.0, 16.0);
const PROMPT_TOP: f32 = 390.0;

/// English, Spanish, and in debug builds the pseudo-locale, whose text is
/// about 38% longer: the layouts that must hold in any language.
fn layout_translators() -> Vec<kettle_i18n::Translator> {
    use kettle_i18n::{Language, Translator};
    let shipping = [Translator::new(Language::En), Translator::new(Language::Es)];
    #[cfg(debug_assertions)]
    return shipping.into_iter().chain([Translator::pseudo()]).collect();
    #[cfg(not(debug_assertions))]
    shipping.to_vec()
}
/// `max(4, round(16 * 0.5))`.
const GAP: f32 = 8.0;
/// `max(16 + 4, round(16 * 1.35))`.
const ROW_H: f32 = 22.0;
/// Two borders, one header row, and the padding above and below the list.
const CHROME_H: f32 = 30.0;

fn overlay(selected: Option<usize>, count: usize) -> CompletionOverlay {
    CompletionOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (100.0, 40.0, 900.0, 700.0),
        grid_rect: (108.0, 70.0, 884.0, 656.0),
        command_rows: (20, 20),
        anchor_col: None,
        kind: "Completions".to_string(),
        source: "fish".to_string(),
        selected,
        total: count,
        token: None,
        candidates: (0..count)
            .map(|index| CompletionOverlayRow {
                label: format!("item-{index}"),
                description: String::new(),
                position: index,
            })
            .collect(),
    }
}

fn theme() -> kettle_config::Theme {
    kettle_config::Theme::default()
}

fn receipt(expanded: bool) -> MediaPasteReceiptOverlay {
    MediaPasteReceiptOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (100.0, 40.0, 900.0, 700.0),
        grid_rect: (108.0, 70.0, 884.0, 656.0),
        right_gutter: 0.0,
        image: Some(kettle_core::ImageData::solid(256, 128, [40, 80, 120, 255]).unwrap()),
        kind: MediaPasteReceiptKind::Image {
            original_width: 1920,
            original_height: 960,
        },
        openable: true,
        remote: false,
        expanded,
        prefer_top: true,
    }
}

fn video_receipt(expanded: bool, preview: bool, count: usize) -> MediaPasteReceiptOverlay {
    let mut receipt = receipt(expanded);
    receipt.image =
        preview.then(|| kettle_core::ImageData::solid(256, 144, [36, 40, 52, 255]).unwrap());
    receipt.kind = MediaPasteReceiptKind::Video {
        extension: "MP4".to_string(),
        size: 34 * 1024 * 1024,
        count,
        preview_pending: !preview,
    };
    receipt.openable = false;
    receipt
}

#[test]
fn media_receipt_stays_inside_the_grid_and_preserves_thumbnail_aspect() {
    let geometry =
        media_paste_receipt_geometry(&receipt(true), None, CELL, CELL.0, CELL.1).unwrap();
    assert!(!geometry.compact);
    let (gx, gy, gw, gh) = receipt(true).grid_rect;
    let (x, y, w, h) = geometry.rect;
    assert!(x >= gx && y >= gy && x + w <= gx + gw && y + h <= gy + gh);
    let image = geometry.preview_rect.expect("expanded card has thumbnail");
    assert!((image.2 / image.3 - 2.0).abs() < 0.001);
    assert!(super::rects_overlap(geometry.dismiss_rect, geometry.rect));
    assert!(geometry.dismiss_rect.2 >= 24.0 && geometry.dismiss_rect.3 >= 24.0);
    assert!(
        !super::rects_overlap(geometry.dismiss_rect, geometry.title_rect),
        "dismiss and title hit targets must not overlap"
    );
    assert!(
        !super::rects_overlap(geometry.dismiss_rect, geometry.detail_rect.unwrap()),
        "dismiss and detail text must not overlap"
    );
    assert_eq!(
        geometry.hit_test(
            geometry.dismiss_rect.0 + geometry.dismiss_rect.2 * 0.5,
            geometry.dismiss_rect.1 + geometry.dismiss_rect.3 * 0.5,
        ),
        Some(MediaPasteReceiptHit::Dismiss)
    );
    assert_eq!(
        geometry.hit_test(geometry.rect.0 + 4.0, geometry.rect.1 + 4.0),
        Some(MediaPasteReceiptHit::Open)
    );
}

#[test]
fn video_receipt_body_is_inert_but_still_consumes_hidden_terminal_clicks() {
    let geometry =
        media_paste_receipt_geometry(&video_receipt(true, true, 1), None, CELL, CELL.0, CELL.1)
            .unwrap();
    assert_eq!(
        geometry.hit_test(geometry.rect.0 + 4.0, geometry.rect.1 + 4.0),
        Some(MediaPasteReceiptHit::Body)
    );
    assert_eq!(
        geometry.hit_test(
            geometry.dismiss_rect.0 + geometry.dismiss_rect.2 * 0.5,
            geometry.dismiss_rect.1 + geometry.dismiss_rect.3 * 0.5,
        ),
        Some(MediaPasteReceiptHit::Dismiss)
    );
}

#[test]
fn bottom_lane_receipt_reserves_the_dismiss_corner_from_detail_text() {
    let mut lower = receipt(true);
    lower.prefer_top = false;
    // font-size=6 and cell-height=0.5 make all three text/button bands
    // overlap vertically, so horizontal reservation is load-bearing.
    let geometry = media_paste_receipt_geometry(&lower, None, (8.0, 3.75), 8.0, 7.5)
        .expect("the expanded receipt fits in the bottom lane");
    assert!(!geometry.compact);
    assert!(
        (geometry.dismiss_rect.1 + geometry.dismiss_rect.3
            - (geometry.rect.1 + geometry.rect.3 - 5.0))
            .abs()
            < 0.01,
        "the dismiss target must occupy the bottom lane"
    );
    assert!(
        !super::rects_overlap(geometry.dismiss_rect, geometry.detail_rect.unwrap()),
        "bottom-lane detail text must reserve the dismiss target"
    );
    assert!(
        !super::rects_overlap(geometry.dismiss_rect, geometry.title_rect),
        "bottom-lane title text must reserve the dismiss target"
    );
}

#[test]
fn media_receipt_avoids_completion_and_compacts_in_a_short_grid() {
    let mut completion = overlay(Some(0), 10);
    completion.anchor_col = Some(100);
    let completion_rect = super::completion_overlay_rect(&completion, CELL).unwrap();
    let geometry =
        media_paste_receipt_geometry(&receipt(true), Some(&completion), CELL, CELL.0, CELL.1)
            .expect("opposite corner remains available");
    assert!(!super::rects_overlap(geometry.rect, completion_rect));

    let mut short = receipt(true);
    short.pane_rect.3 = 100.0;
    short.grid_rect.3 = 64.0;
    let geometry = media_paste_receipt_geometry(&short, None, CELL, CELL.0, CELL.1).unwrap();
    assert!(geometry.compact, "short panes degrade to a chip");
    assert!(geometry.preview_rect.is_none());
    assert!(geometry.dismiss_rect.2 >= 24.0 && geometry.dismiss_rect.3 >= 24.0);
    assert!(
        !super::rects_overlap(geometry.dismiss_rect, geometry.title_rect),
        "the compact label must leave room for dismissal"
    );
}

#[test]
fn media_receipt_keeps_one_lane_across_compact_and_expanded_states() {
    let mut exercised = 0;
    for row in 0..40 {
        let mut completion = overlay(Some(0), 6);
        completion.command_rows = (row, row);
        completion.anchor_col = Some(100);
        let compact =
            media_paste_receipt_geometry(&receipt(false), Some(&completion), CELL, CELL.0, CELL.1);
        let expanded =
            media_paste_receipt_geometry(&receipt(true), Some(&completion), CELL, CELL.0, CELL.1);
        let (Some(compact), Some(expanded)) = (compact, expanded) else {
            continue;
        };
        if expanded.compact {
            continue;
        }
        exercised += 1;
        let grid_mid = receipt(true).grid_rect.1 + receipt(true).grid_rect.3 / 2.0;
        assert_eq!(
            compact.rect.1 < grid_mid,
            expanded.rect.1 < grid_mid,
            "hover must not move the receipt to the opposite corner at row {row}"
        );
        assert_eq!(
            compact.dismiss_rect.0, expanded.dismiss_rect.0,
            "hover must not move the dismiss target horizontally at row {row}"
        );
        assert_eq!(
            compact.dismiss_rect.1 < grid_mid,
            expanded.dismiss_rect.1 < grid_mid,
            "hover must keep the dismiss target against the same lane edge at row {row}"
        );
    }
    assert!(exercised > 0, "fixture never admitted an expanded receipt");
}

#[test]
fn media_receipt_uses_the_corner_opposite_the_terminal_cursor() {
    let top = media_paste_receipt_geometry(&receipt(true), None, CELL, CELL.0, CELL.1).unwrap();
    let mut lower = receipt(true);
    lower.prefer_top = false;
    let bottom = media_paste_receipt_geometry(&lower, None, CELL, CELL.0, CELL.1).unwrap();
    assert!(top.rect.1 < bottom.rect.1);
    assert_eq!(top.rect.0, bottom.rect.0, "the right edge stays stable");
}

#[test]
fn media_receipt_hides_when_even_the_chip_cannot_fit() {
    let mut tiny = receipt(false);
    tiny.pane_rect.2 = 80.0;
    tiny.grid_rect.2 = 64.0;
    assert!(media_paste_receipt_geometry(&tiny, None, CELL, CELL.0, CELL.1).is_none());
}

#[test]
fn media_receipt_text_preserves_dimensions_and_the_remote_warning() {
    let mut compact = receipt(false);
    compact.kind = MediaPasteReceiptKind::Image {
        original_width: 16_384,
        original_height: 16_384,
    };
    let compact_geometry =
        media_paste_receipt_geometry(&compact, None, CELL, CELL.0, CELL.1).unwrap();
    let (compact_title, _) = media_paste_receipt_text(&compact, &compact_geometry, CELL.0);
    assert_eq!(compact_title, "Image path pasted");

    compact.remote = true;
    let compact_geometry =
        media_paste_receipt_geometry(&compact, None, CELL, CELL.0, CELL.1).unwrap();
    let (compact_title, _) = media_paste_receipt_text(&compact, &compact_geometry, CELL.0);
    assert!(
        compact_title.starts_with("Remote · local path"),
        "a compact remote receipt must identify both the remote pane and local path"
    );

    let mut remote = receipt(true);
    remote.remote = true;
    let geometry = media_paste_receipt_geometry(&remote, None, CELL, CELL.0, CELL.1).unwrap();
    let (_, detail) = media_paste_receipt_text(&remote, &geometry, CELL.0);
    assert!(detail.contains("Remote pane\nLocal path only"));
    let columns = (geometry.detail_rect.unwrap().2 / CELL.0).floor() as usize;
    assert!(
        detail.lines().all(|line| display_width(line) <= columns),
        "no warning line may be clipped into a misleading fragment"
    );

    let mut narrow = receipt(true);
    narrow.remote = true;
    narrow.pane_rect.2 = 148.0;
    narrow.grid_rect.2 = 148.0;
    let geometry = media_paste_receipt_geometry(&narrow, None, (5.0, 10.0), 10.0, 10.0)
        .expect("the compact receipt still fits");
    assert!(
        geometry.compact,
        "the full warning must not render when six-pixel padding leaves fewer than 15 columns"
    );

    let scaled =
        media_paste_receipt_geometry(&remote, None, (CELL.0 * 0.5, CELL.1), CELL.0, CELL.1)
            .expect("the compact receipt still fits at half-width terminal cells");
    assert!(
        scaled.compact,
        "terminal cell-width must not shrink the chrome text budget"
    );

    for pane_width in 140..=360 {
        let mut candidate = receipt(true);
        candidate.remote = true;
        candidate.pane_rect.2 = pane_width as f32;
        candidate.grid_rect.2 = pane_width as f32;
        let Some(geometry) =
            media_paste_receipt_geometry(&candidate, None, (6.0, CELL.1), CELL.0, CELL.1)
        else {
            continue;
        };
        if !geometry.compact {
            let (_, detail) = media_paste_receipt_text(&candidate, &geometry, CELL.0);
            assert!(
                detail.lines().any(|line| line == "Local path only"),
                "an admitted expanded card must preserve the full warning at width {pane_width}"
            );
        }
    }
}

#[test]
fn video_receipt_has_a_stable_poster_and_never_projects_a_path() {
    let pending = video_receipt(true, false, 3);
    let pending_geometry =
        media_paste_receipt_geometry(&pending, None, CELL, CELL.0, CELL.1).unwrap();
    let poster = pending_geometry
        .preview_rect
        .expect("expanded video receipt has a generic poster");
    assert!((poster.2 / poster.3 - 16.0 / 9.0).abs() < 0.001);
    let (title, detail) = media_paste_receipt_text(&pending, &pending_geometry, CELL.0);
    assert_eq!(title, "Video path pasted");
    assert!(detail.contains("1 of 3 · MP4 · 35.7 MB"));
    assert!(detail.contains("Preparing poster"));
    assert!(!format!("{pending:?}{title}{detail}").contains("/Users/"));

    let ready = video_receipt(true, true, 1);
    let ready_geometry = media_paste_receipt_geometry(&ready, None, CELL, CELL.0, CELL.1).unwrap();
    let preview = ready_geometry
        .preview_rect
        .expect("native poster is visible");
    assert!((preview.2 / preview.3 - 16.0 / 9.0).abs() < 0.001);
    let (_, detail) = media_paste_receipt_text(&ready, &ready_geometry, CELL.0);
    assert!(detail.contains("Path on command line"));
}

/// The card speaks the UI's language, and an expanded remote card is
/// admitted only when its translated warning fits whole.
#[test]
fn media_receipt_text_follows_the_ui_language() {
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    let mut compact = receipt(false);
    compact.tr = es;
    let geometry = media_paste_receipt_geometry(&compact, None, CELL, CELL.0, CELL.1).unwrap();
    assert_eq!(
        media_paste_receipt_text(&compact, &geometry, CELL.0).0,
        "Imagen: ruta pegada"
    );
    compact.remote = true;
    let geometry = media_paste_receipt_geometry(&compact, None, CELL, CELL.0, CELL.1).unwrap();
    assert_eq!(
        media_paste_receipt_text(&compact, &geometry, CELL.0).0,
        "Remoto · ruta local",
        "the compact remote warning must show whole"
    );
    let mut batch = video_receipt(false, false, 3);
    batch.tr = es;
    let geometry = media_paste_receipt_geometry(&batch, None, CELL, CELL.0, CELL.1).unwrap();
    assert_eq!(
        media_paste_receipt_text(&batch, &geometry, CELL.0).0,
        "3 rutas de video"
    );

    // Every compact title shows whole, in each language and the
    // pseudo-locale: the remote one is a safety warning, and a clipped
    // count misleads, so the card widens for a longer title.
    for tr in layout_translators() {
        let mut cards = vec![receipt(false)];
        for count in [1, 2, 99] {
            cards.push(video_receipt(false, false, count));
        }
        let remote: Vec<_> = cards
            .iter()
            .cloned()
            .map(|mut card| {
                card.remote = true;
                card
            })
            .collect();
        for mut card in cards.into_iter().chain(remote) {
            card.tr = tr;
            let geometry = media_paste_receipt_geometry(&card, None, CELL, CELL.0, CELL.1).unwrap();
            assert!(geometry.compact);
            let (title, _) = media_paste_receipt_text(&card, &geometry, CELL.0);
            assert!(!title.ends_with('…'), "{title:?} was clipped");
        }
    }

    let mut pending = video_receipt(true, false, 3);
    pending.tr = es;
    let geometry = media_paste_receipt_geometry(&pending, None, CELL, CELL.0, CELL.1).unwrap();
    let (title, detail) = media_paste_receipt_text(&pending, &geometry, CELL.0);
    assert_eq!(title, "Video: ruta pegada");
    assert!(detail.contains("1 de 3 · MP4 · 35.7 MB"), "{detail}");
    assert!(detail.contains("Preparando portada"), "{detail}");

    for tr in layout_translators() {
        let widest = [
            kettle_i18n::Text::ReceiptRemotePane,
            kettle_i18n::Text::ReceiptLocalPathOnly,
        ]
        .into_iter()
        .map(|line| display_width(tr.text(line)))
        .max()
        .unwrap();
        assert!(super::remote_warning_columns(&tr) >= (widest + 3) as f32);
        let mut expanded = 0;
        for (cell_w, pane_width) in [6.0, CELL.0]
            .into_iter()
            .flat_map(|cell_w| (140..=600).map(move |width| (cell_w, width)))
        {
            let mut candidate = receipt(true);
            candidate.tr = tr;
            candidate.remote = true;
            candidate.pane_rect.2 = pane_width as f32;
            candidate.grid_rect.2 = pane_width as f32;
            let Some(geometry) =
                media_paste_receipt_geometry(&candidate, None, (cell_w, CELL.1), CELL.0, CELL.1)
            else {
                continue;
            };
            if !geometry.compact {
                expanded += 1;
                let (_, detail) = media_paste_receipt_text(&candidate, &geometry, CELL.0);
                for line in [
                    kettle_i18n::Text::ReceiptRemotePane,
                    kettle_i18n::Text::ReceiptLocalPathOnly,
                ] {
                    assert!(
                        detail.lines().any(|shown| shown == tr.text(line)),
                        "an admitted expanded card must show {:?} whole at width {pane_width}",
                        tr.text(line)
                    );
                }
            }
        }
        // The shipping languages exercise the expanded card; text 38%
        // longer never fits its detail box, so the pseudo-locale keeps
        // the compact card, whose whole title is checked above.
        if tr.language() == kettle_i18n::Language::Es || tr == kettle_i18n::Translator::default() {
            assert!(
                expanded > 0,
                "{:?}: no width admitted the expanded card",
                tr.language()
            );
        }
    }
    assert_eq!(
        super::remote_warning_columns(&kettle_i18n::Translator::default()),
        18.0
    );
}

/// With chrome and terminal cells of different widths, a compact card
/// widened for its title stays inside the expanded card it shares a lane
/// with, and its title gets every column it was sized for.
#[test]
fn a_widened_compact_card_stays_inside_its_expanded_card() {
    for (cw, text_cw) in [
        (4.4, 8.0),
        (8.0, 4.4),
        (4.4, 4.0),
        (4.0, 4.4),
        (8.0, 8.0),
        (10.0, 7.0),
    ] {
        for count in [1, 2, 100] {
            for remote in [false, true] {
                for tr in layout_translators() {
                    let mut compact = video_receipt(false, false, count);
                    compact.tr = tr;
                    compact.remote = remote;
                    let mut open = compact.clone();
                    open.expanded = true;
                    let cell = (cw, CELL.1);
                    let (Some(chip), Some(card)) = (
                        media_paste_receipt_geometry(&compact, None, cell, text_cw, CELL.1),
                        media_paste_receipt_geometry(&open, None, cell, text_cw, CELL.1),
                    ) else {
                        continue;
                    };
                    if !card.compact {
                        assert!(
                            chip.rect.0 >= card.rect.0 - 0.01
                                && chip.rect.0 + chip.rect.2 <= card.rect.0 + card.rect.2 + 0.01,
                            "cells {cw}/{text_cw}, {count} videos, remote {remote}: \
                                 chip {:?} leaves card {:?}",
                            chip.rect,
                            card.rect
                        );
                    }
                    // The fixture's 900 px pane never bounds the chip, so
                    // only the expanded card's width may cut its title.
                    if !card.compact && chip.rect.2 >= card.rect.2 - 0.01 {
                        continue;
                    }
                    let title = super::media_paste_receipt_compact_title(&compact);
                    let (shown, _) = media_paste_receipt_text(&compact, &chip, text_cw);
                    assert_eq!(shown, title, "cells {cw}/{text_cw}: a column was lost");
                }
            }
        }
    }
}

/// At the standard size every line of the card shows whole in each
/// language: the title, the size line, the path or poster line, and the
/// remote warning.
#[test]
fn media_receipt_lines_fit_in_each_language() {
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    for tr in [kettle_i18n::Translator::default(), es] {
        let cards = [
            receipt(true),
            receipt(false),
            video_receipt(true, false, 1),
            video_receipt(true, true, 3),
            video_receipt(false, false, 99),
        ];
        for card in cards {
            for remote in [false, true] {
                let mut card = card.clone();
                card.tr = tr;
                card.remote = remote;
                let geometry =
                    media_paste_receipt_geometry(&card, None, CELL, CELL.0, CELL.1).unwrap();
                let (title, detail) = media_paste_receipt_text(&card, &geometry, CELL.0);
                for line in std::iter::once(title.as_str()).chain(detail.lines()) {
                    assert!(!line.ends_with('…'), "{line:?} was clipped");
                }
            }
        }
    }
}

#[test]
fn receipt_can_move_to_the_left_when_completion_owns_both_right_corners() {
    let mut completion = overlay(Some(0), 40);
    completion.pane_rect = receipt(true).pane_rect;
    completion.grid_rect = receipt(true).grid_rect;
    completion.anchor_col = Some(70);
    let completion_rect = super::completion_overlay_rect(&completion, CELL).unwrap();
    let geometry =
        media_paste_receipt_geometry(&receipt(true), Some(&completion), CELL, CELL.0, CELL.1)
            .expect("the left side remains available");
    assert!(!super::rects_overlap(geometry.rect, completion_rect));
    assert!(
        geometry.rect.0 < completion_rect.0,
        "the receipt should use the free left side: receipt {:?}, completion {:?}",
        geometry.rect,
        completion_rect
    );
}

#[test]
fn receipt_poster_width_uses_chrome_metrics_not_terminal_font_width() {
    let geometry = media_paste_receipt_geometry(
        &video_receipt(true, true, 1),
        None,
        (32.0, CELL.1),
        CELL.0,
        CELL.1,
    )
    .expect("wide terminal glyphs must not collapse the chrome card");
    assert!(!geometry.compact);
    assert!(geometry.preview_rect.unwrap().2 >= CELL.0 * 8.0);
}

#[test]
fn media_sizes_use_decimal_units_without_rounding_to_whole_kilobytes() {
    assert_eq!(super::format_media_size(34 * 1024 * 1024), "35.7 MB");
    assert_eq!(super::format_media_size(34 * 1024 * 1024 + 1), "35.7 MB");
    assert_eq!(super::format_media_size(1_500), "1.5 KB");
}

#[test]
fn media_receipt_budgets_real_text_lines_and_the_scrollbar_gutter() {
    let mut remote = receipt(true);
    remote.remote = true;
    remote.grid_rect.3 = 160.0;
    remote.pane_rect.3 = 176.0;
    remote.right_gutter = 24.0;

    // A supported 0.5 cell-height leaves chrome text at its ordinary
    // 16-pixel line height while terminal rows shrink to 8 pixels. The
    // remote warning must still remain wholly inside the expanded card.
    let geometry = media_paste_receipt_geometry(&remote, None, (8.0, 8.0), 8.0, 16.0)
        .expect("the text-aware card fits");
    assert!(!geometry.compact);
    for inner in [geometry.title_rect, geometry.detail_rect.unwrap()] {
        assert!(inner.0 >= geometry.rect.0 && inner.1 >= geometry.rect.1);
        assert!(inner.0 + inner.2 <= geometry.rect.0 + geometry.rect.2);
        assert!(inner.1 + inner.3 <= geometry.rect.1 + geometry.rect.3);
    }
    assert!(
        geometry.rect.0 + geometry.rect.2
            <= remote.pane_rect.0 + remote.pane_rect.2 - remote.right_gutter
    );

    remote.grid_rect.3 = 96.0;
    remote.pane_rect.3 = 112.0;
    let compact = media_paste_receipt_geometry(&remote, None, (8.0, 8.0), 8.0, 16.0)
        .expect("a short pane still has room for the compact receipt");
    assert!(
        compact.compact,
        "warning text must not spill from a short card"
    );

    for pane_width in 180..=360 {
        let mut widened = receipt(true);
        widened.pane_rect.2 = pane_width as f32;
        widened.grid_rect.2 = pane_width as f32;
        let Some(geometry) = media_paste_receipt_geometry(&widened, None, (24.0, 16.0), 8.0, 16.0)
        else {
            continue;
        };
        if geometry.compact {
            continue;
        }
        for inner in [geometry.title_rect, geometry.detail_rect.unwrap()] {
            assert!(
                inner.0 + inner.2 <= geometry.rect.0 + geometry.rect.2,
                "wide terminal cells must not push chrome text past the card at width {pane_width}"
            );
        }
    }
}

/// The chip's label is cached and shaped only when it changes, so it
/// must count toward the retained chrome's damage key.
#[test]
fn chip_text_is_in_the_chrome_hash() {
    let source = production_source();
    let hash = source
        .split("let chrome_hash = {")
        .nth(1)
        .and_then(|rest| rest.split("let chrome_changed =").next())
        .expect("retained chrome hash body");
    assert!(hash.contains("self.update_chip_text.hash(&mut h);"));
}

/// A side bar's overflow arrows point along it, and since one buffer
/// holds either glyph, the text counts toward the chrome's damage key.
#[test]
fn side_bar_scroll_arrows_point_along_the_strip() {
    assert_eq!(crate::scroll_arrow_glyphs(false), (" ‹", " ›"));
    assert_eq!(crate::scroll_arrow_glyphs(true), ("⌃", "⌄"));
    let source = production_source();
    let hash = source
        .split("let chrome_hash = {")
        .nth(1)
        .and_then(|rest| rest.split("let chrome_changed =").next())
        .expect("retained chrome hash body");
    assert!(hash.contains("self.scroll_left_text.hash(&mut h);"));
    assert!(hash.contains("self.scroll_right_text.hash(&mut h);"));
}

#[test]
fn media_receipt_text_enters_the_retained_chrome_damage_key() {
    let source = production_source();
    let hash = source
        .split("let chrome_hash = {")
        .nth(1)
        .and_then(|rest| rest.split("let chrome_changed =").next())
        .expect("retained chrome hash body");
    assert!(hash.contains("self.media_receipt_title_text.hash(&mut h);"));
    assert!(hash.contains("self.media_receipt_detail_text.hash(&mut h);"));
    assert!(
        hash.contains("text.shaped().hash(&mut h);")
            && hash.contains("for lane in &overlay.media_lanes {"),
        "browsing a lane changes its text without moving its buffers"
    );
}

#[test]
fn card_hangs_off_the_prompt_and_grows_upward_from_the_grid_left_edge() {
    let geometry = completion_panel_geometry(&overlay(Some(19), 20), CELL).unwrap();
    assert_eq!(geometry.placement, CompletionPanelPlacement::Above);
    assert_eq!(geometry.rows, MAX_COMPLETION_ROWS);
    assert_eq!(
        geometry.rect.1 + geometry.rect.3,
        PROMPT_TOP - GAP,
        "the lower edge sits exactly a half-cell above the prompt's first row"
    );
    assert_eq!(geometry.rect.0, 108.0, "the card aligns with the grid left");
    assert!(
        geometry.rect.1 >= 70.0 + 8.0,
        "a half-cell top margin is preserved"
    );
    assert_eq!(geometry.row_h, ROW_H);
    assert_eq!(geometry.rect.3, CHROME_H + 10.0 * ROW_H);

    // Fewer candidates keep the same lower edge and grow upward only.
    let short = completion_panel_geometry(&overlay(Some(1), 3), CELL).unwrap();
    assert_eq!(short.placement, CompletionPanelPlacement::Above);
    assert_eq!(short.rect.1 + short.rect.3, PROMPT_TOP - GAP);
    assert_eq!(short.rows, 3);
    assert!(short.rect.1 > geometry.rect.1);
}

#[test]
fn card_aligns_with_the_command_column_and_clamps_at_the_right_edge() {
    let mut card = overlay(None, 3);
    card.anchor_col = Some(7);
    let aligned = completion_panel_geometry(&card, CELL).unwrap();
    assert_eq!(
        aligned.rect.0,
        card.grid_rect.0 + 7.0 * CELL.0,
        "the card starts where editable input begins, not at the pane edge"
    );

    card.anchor_col = Some(10_000);
    let clamped = completion_panel_geometry(&card, CELL).unwrap();
    assert_eq!(
        clamped.rect.0 + clamped.rect.2,
        card.grid_rect.0 + card.grid_rect.2,
        "a prompt near the right edge keeps the whole card inside the grid"
    );
}

#[test]
fn rows_begin_below_the_header_and_match_the_painted_band() {
    let card = overlay(Some(0), 4);
    let geometry = completion_panel_geometry(&card, CELL).unwrap();
    assert_eq!(geometry.header.1, geometry.rect.1 + 1.0);
    assert_eq!(geometry.header.3, ROW_H);
    assert_eq!(
        geometry.list_top,
        geometry.header.1 + geometry.header.3 + 3.0,
        "one restrained pad separates the header from the first row"
    );
    let rows = completion_overlay_row_rects(&card, CELL);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].1.1, geometry.list_top);
    assert_eq!(rows[1].1.1 - rows[0].1.1, ROW_H);
    assert_eq!(rows[0].1.3, ROW_H);
    let (_, list_y, _, list_h) = geometry.list_rect();
    assert_eq!(list_y, geometry.list_top);
    assert_eq!(list_h, 4.0 * ROW_H);
    assert_eq!(
        list_y + list_h + 3.0 + 1.0,
        geometry.rect.1 + geometry.rect.3,
        "the same pad and border close the card under the last row"
    );
}

#[test]
fn selection_keeps_one_row_of_lookahead() {
    // A selection in the middle of a long result leaves exactly one row
    // visible underneath it.
    let geometry = completion_panel_geometry(&overlay(Some(14), 40), CELL).unwrap();
    assert_eq!(geometry.rows, MAX_COMPLETION_ROWS);
    assert_eq!(geometry.first, 6);
    assert_eq!(
        geometry.first + geometry.rows - 1,
        15,
        "one candidate after the selection stays on screen"
    );

    // At the end of the result there is nothing left to look ahead to.
    let tail = completion_panel_geometry(&overlay(Some(39), 40), CELL).unwrap();
    assert_eq!(tail.first, 30);

    // The unselected first page starts at the top of the result.
    let head = completion_panel_geometry(&overlay(None, 40), CELL).unwrap();
    assert_eq!(head.first, 0);
    let second = completion_panel_geometry(&overlay(Some(0), 40), CELL).unwrap();
    assert_eq!(second.first, 0);
}

#[test]
fn scroll_indicator_tracks_the_shell_result_not_the_page() {
    let mut card = overlay(Some(0), 10);
    assert!(
        completion_scroll_thumb(&card, &completion_panel_geometry(&card, CELL).unwrap()).is_none(),
        "a result that fits needs no indicator"
    );

    card = overlay(Some(0), 40);
    card.total = 200;
    for (index, candidate) in card.candidates.iter_mut().enumerate() {
        candidate.position = index;
    }
    let geometry = completion_panel_geometry(&card, CELL).unwrap();
    let (_, track_y, _, track_h) = geometry.list_rect();
    let (top, height) = completion_scroll_thumb(&card, &geometry).expect("indicator");
    assert_eq!(top, track_y, "the first page pins the thumb to the top");
    assert!(height >= 11.0 && height < track_h);

    // A page deep inside the result moves the thumb down proportionally and
    // never past the end of the track.
    card.selected = Some(39);
    let geometry = completion_panel_geometry(&card, CELL).unwrap();
    let (deep_top, deep_height) = completion_scroll_thumb(&card, &geometry).expect("indicator");
    assert!(deep_top > track_y);
    assert!(deep_top + deep_height <= track_y + track_h + 0.5);
}

#[test]
fn width_is_content_fit_inside_the_published_column_band() {
    let short = completion_panel_geometry(&overlay(None, 2), CELL).unwrap();
    // `Completions · fish` (18) + gap (2) + `2 matches` (9) + padding (2).
    assert_eq!(short.rect.2, 31.0 * CELL.0);
    assert!(short.rect.2 >= COMPLETION_MIN_COLUMNS as f32 * CELL.0);

    let mut long = overlay(None, 2);
    long.candidates[0].label = "x".repeat(200);
    long.candidates[0].description = "y".repeat(200);
    let long = completion_panel_geometry(&long, CELL).unwrap();
    assert_eq!(long.rect.2, 92.0 * CELL.0, "the card stays content-fit");
    assert!(long.rect.2 <= COMPLETION_MAX_COLUMNS as f32 * CELL.0);
    assert_eq!(long.label_columns, 40, "labels clamp at 40 columns");
    assert_eq!(
        long.description_columns, 48,
        "descriptions clamp at 48 columns"
    );
    assert!(long.divider_x.is_some());

    // A pane narrower than the content clamps to the grid instead of
    // overflowing it, and the lanes are redistributed to match.
    let mut narrow = overlay(None, 2);
    narrow.candidates[0].label = "x".repeat(200);
    narrow.candidates[0].description = "y".repeat(200);
    narrow.pane_rect = (0.0, 40.0, 300.0, 700.0);
    narrow.grid_rect = (0.0, 70.0, 300.0, 656.0);
    let narrow = completion_panel_geometry(&narrow, CELL).unwrap();
    assert_eq!(narrow.rect.2, 37.0 * CELL.0);
    assert_eq!(
        narrow.label_columns + narrow.description_columns,
        35,
        "the lanes exactly fill the clamped card"
    );
    assert_eq!(narrow.description_columns, 0);
    assert_eq!(narrow.divider_x, None, "no empty description divider");
    assert!(narrow.rect.0 + narrow.rect.2 <= 300.0);
}

#[test]
fn header_reserves_the_count_lane_and_reports_absolute_positions() {
    let mut card = overlay(Some(2), 40);
    card.total = 87;
    for (index, candidate) in card.candidates.iter_mut().enumerate() {
        candidate.position = index;
    }
    assert_eq!(completion_header_label(&card), "Completions · fish");
    assert_eq!(completion_header_count(&card), "3/87");
    card.selected = None;
    assert_eq!(completion_header_count(&card), "87 matches");
    card.total = 1;
    card.candidates.truncate(1);
    assert_eq!(completion_header_count(&card), "1 match");
    card.tr = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    assert_eq!(completion_header_count(&card), "1 coincidencia");
    card.tr = kettle_i18n::Translator::default();

    let card = overlay(None, 2);
    let geometry = completion_panel_geometry(&card, CELL).unwrap();
    let count = completion_header_count(&card);
    let (label_columns, count_columns) = completion_header_columns(&geometry, &count);
    assert_eq!(count_columns, display_width(&count));
    assert_eq!(
        label_columns + 1 + count_columns,
        geometry.inner_columns,
        "the caption takes what the right-aligned count leaves"
    );
    assert!(label_columns >= display_width(&completion_header_label(&card)));
}

#[test]
fn card_uses_the_roomier_lane_without_overlapping_a_multiline_command() {
    let mut completion = overlay(None, 8);
    // Only one candidate fits above this command while all eight fit below,
    // so the useful lane wins rather than reducing the card to one row.
    completion.command_rows = (5, 7);
    let below = completion_panel_geometry(&completion, CELL).unwrap();
    assert_eq!(below.placement, CompletionPanelPlacement::Below);
    assert_eq!(below.rows, 8);
    assert_eq!(below.rect.1, 70.0 + 8.0 * 16.0 + GAP);
    assert!(below.rect.1 > 70.0 + 7.0 * 16.0);

    // With no usable upper lane, a prompt at the top receives the same
    // detached card immediately after its final row.
    completion.command_rows = (0, 2);
    let top_edge = completion_panel_geometry(&completion, CELL).unwrap();
    assert_eq!(top_edge.placement, CompletionPanelPlacement::Below);
    assert_eq!(top_edge.rect.1, 70.0 + 3.0 * 16.0 + GAP);
    assert!(
        top_edge.rect.1 + top_edge.rect.3 <= 70.0 + 656.0 - 8.0,
        "the lower card keeps the same half-cell grid margin"
    );

    // A prompt near the bottom has the inverse constraint and keeps the
    // preferred upper placement.
    completion.command_rows = (38, 40);
    let bottom_edge = completion_panel_geometry(&completion, CELL).unwrap();
    assert_eq!(bottom_edge.placement, CompletionPanelPlacement::Above);
    assert_eq!(
        bottom_edge.rect.1 + bottom_edge.rect.3,
        70.0 + 38.0 * 16.0 - GAP
    );

    // Reversed metadata is normalized before either lane is measured.
    completion.command_rows = (3, 5);
    let forward = completion_panel_geometry(&completion, CELL).unwrap();
    completion.command_rows = (5, 3);
    let reversed = completion_panel_geometry(&completion, CELL).unwrap();
    assert_eq!(forward.rect, reversed.rect);
    assert_eq!(forward.placement, reversed.placement);

    completion.command_rows = (0, 39);
    assert!(
        completion_panel_geometry(&completion, CELL).is_none(),
        "a command spanning the pane leaves no safe lane on either side"
    );
}

#[test]
fn a_top_edge_card_stays_below_pane_chrome_and_the_tab_bar() {
    let mut completion = overlay(None, 3);
    completion.command_rows = (0, 0);
    let geometry = completion_panel_geometry(&completion, CELL).unwrap();
    assert_eq!(geometry.placement, CompletionPanelPlacement::Below);
    assert_eq!(geometry.rect.1, completion.grid_rect.1 + CELL.1 + GAP);
    assert!(
        geometry.rect.1 >= completion.grid_rect.1,
        "placement is clipped to the terminal grid, never pane or tab chrome"
    );
}

#[test]
fn card_declines_a_pane_too_small_for_one_row() {
    let mut completion = overlay(None, 1);
    completion.pane_rect = (0.0, 0.0, 60.0, 28.0);
    completion.grid_rect = (0.0, 0.0, 60.0, 28.0);
    assert!(completion_panel_geometry(&completion, CELL).is_none());

    // Tall enough, but narrower than the published minimum width.
    let mut narrow = overlay(None, 1);
    narrow.pane_rect = (0.0, 40.0, 152.0, 700.0);
    narrow.grid_rect = (0.0, 70.0, 152.0, 656.0);
    assert!(completion_panel_geometry(&narrow, CELL).is_none());

    narrow.pane_rect.2 = 160.0;
    narrow.grid_rect.2 = 160.0;
    assert!(
        completion_panel_geometry(&narrow, CELL).is_some(),
        "the former 20-column minimum remains usable"
    );
}

#[test]
fn labels_keep_their_discriminating_path_tail() {
    let source = production_source();
    assert!(
        source.contains("middle_ellipsis(&candidate.label, geometry.label_columns)"),
        "completion labels must shed their middle, not their tail"
    );
    assert_eq!(
        super::middle_ellipsis("/home/kevim/Repos/kettle/crates/kettle-vt/src/lib.rs", 24),
        "/home/kevim/Repo…/lib.rs"
    );
}

#[test]
fn emphasis_matches_only_the_first_safe_occurrence() {
    assert_eq!(completion_match_span("checkout", "ch"), Some((0, 2)));
    assert_eq!(
        completion_match_span("cherry-pick-cherry", "cherry"),
        Some((0, 6)),
        "only the first occurrence is emphasized"
    );
    assert_eq!(
        completion_match_span("CHECKOUT", "ch"),
        Some((0, 2)),
        "ASCII compares case-insensitively"
    );
    assert_eq!(completion_match_span("git-checkout", "CHECK"), Some((4, 9)));
    assert_eq!(completion_match_span("checkout", ""), None);
    assert_eq!(completion_match_span("ch", "checkout"), None);
    assert_eq!(completion_match_span("checkout", "zz"), None);
    // Non-ASCII on either side falls back to an exact substring so case
    // folding cannot desynchronize the span from the shaped bytes.
    assert_eq!(completion_match_span("Ünïcode", "Ünï"), Some((0, 5)));
    assert_eq!(completion_match_span("Ünïcode", "ünï"), None);
    let span = completion_match_span("café-münchen", "münchen").expect("exact tail");
    assert_eq!(&"café-münchen"[span.0..span.1], "münchen");
}

#[test]
fn selection_surface_separates_from_the_panel_in_every_theme() {
    let panel = solid_blend(Rgb::new(205, 214, 244), Rgb::new(30, 30, 46), 9);
    let accent = Rgb::new(137, 180, 250);
    // A theme whose selection is already distinct is used verbatim.
    let distinct = Rgb::new(69, 71, 90);
    assert_eq!(
        completion_selection_surface(distinct, panel, accent),
        distinct
    );
    // A selection indistinguishable from the panel is replaced by an
    // accent blend that does separate.
    let flat = completion_selection_surface(panel, panel, accent);
    assert_ne!(flat, panel);
    assert!(color::contrast_ratio(flat, panel) >= 1.35);

    for (foreground, background) in [
        (Rgb::new(205, 214, 244), Rgb::new(30, 30, 46)),
        (Rgb::new(40, 42, 54), Rgb::new(250, 250, 250)),
    ] {
        let mut theme = theme();
        theme.foreground = foreground;
        theme.background = background;
        theme.selection_background = background;
        theme.selection_foreground = foreground;
        let palette = completion_palette(&theme, accent);
        assert!(color::contrast_ratio(palette.selection_bg, palette.panel_bg) >= 1.35);
        for (name, text, surface) in [
            ("label", palette.label, palette.panel_bg),
            ("header", palette.header, palette.panel_bg),
            ("emphasis", palette.emphasis, palette.panel_bg),
            (
                "selected label",
                palette.selected_label,
                palette.selection_bg,
            ),
            (
                "selected emphasis",
                palette.selected_emphasis,
                palette.selection_bg,
            ),
        ] {
            assert!(
                color::contrast_ratio(text, surface) >= 4.49,
                "{name} fell under 4.5 contrast"
            );
        }
        for (name, text, surface) in [
            ("description", palette.description, palette.panel_bg),
            (
                "selected description",
                palette.selected_description,
                palette.selection_bg,
            ),
        ] {
            assert!(
                color::contrast_ratio(text, surface) >= 3.99,
                "{name} fell under 4.0 contrast"
            );
        }
    }
}

#[test]
fn only_the_selected_row_carries_an_accent_rail() {
    let overlay = overlay(Some(0), 3);
    let geometry = completion_panel_geometry(&overlay, CELL).expect("completion geometry");
    let palette = completion_palette(&theme(), Rgb::new(137, 180, 250));
    let mut quads = Vec::new();

    push_completion_selection_quads(
        &mut quads,
        false,
        &geometry,
        geometry.list_top,
        0.0,
        &palette,
    );
    assert!(
        quads.is_empty(),
        "an unselected row must paint neither a selection surface nor a rail"
    );

    push_completion_selection_quads(
        &mut quads,
        true,
        &geometry,
        geometry.list_top,
        0.0,
        &palette,
    );
    assert_eq!(quads.len(), 2, "selection surface plus one accent rail");
    assert_eq!(quads[1].size, [super::COMPLETION_RAIL_W, geometry.row_h]);
    assert_eq!(quads[1].pos[1], geometry.list_top);
}

#[test]
fn lifted_surface_moves_toward_foreground_in_both_theme_directions() {
    assert_eq!(
        solid_blend(Rgb::new(255, 255, 255), Rgb::new(0, 0, 0), 7),
        Rgb::new(18, 18, 18)
    );
    assert_eq!(
        solid_blend(Rgb::new(0, 0, 0), Rgb::new(255, 255, 255), 7),
        Rgb::new(237, 237, 237)
    );
}

#[test]
fn a_static_completion_card_does_not_force_continuous_text_prepare() {
    let mut frame = Overlay {
        completion: Some(overlay(Some(0), 3)),
        ..Overlay::default()
    };
    assert!(!text_overlay_requires_continuous_prepare(&frame));

    frame.ssh_query = Some(String::new());
    assert!(text_overlay_requires_continuous_prepare(&frame));
}
