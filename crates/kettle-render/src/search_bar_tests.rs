use super::{
    HighlightRect, SEARCH_SEGMENTS, SearchBarGeometry, SearchCaseMode, SearchControl,
    SearchOverlay, SearchStatus, drop_cols_front, search_bar_geometry, search_bar_segments,
    search_bar_text, search_editor_label, search_highlight_at,
};

fn center(rect: (f32, f32, f32, f32)) -> (f32, f32) {
    (rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5)
}

fn assert_inside(bar: SearchBarGeometry, rect: (f32, f32, f32, f32)) {
    assert!(rect.2 > 0.0, "control must retain a width: {rect:?}");
    assert!(rect.3 > 0.0, "control must retain a height: {rect:?}");
    assert!(rect.0 >= bar.rect.0 && rect.1 >= bar.rect.1);
    assert!(rect.0 + rect.2 <= bar.rect.0 + bar.rect.2 + f32::EPSILON);
    assert!(rect.1 + rect.3 <= bar.rect.1 + bar.rect.3 + f32::EPSILON);
}

#[test]
fn signed_history_lines_project_into_the_viewport_without_casting() {
    assert_eq!(
        HighlightRect::from_grid_span(-40, 42, 24, 3, 0, true),
        Some(HighlightRect {
            row: 2,
            col: 3,
            width: 1,
            active: true,
        })
    );
    assert_eq!(
        HighlightRect::from_grid_span(-43, 42, 24, 0, 1, false),
        None
    );
    assert_eq!(HighlightRect::from_grid_span(0, 42, 24, 0, 1, false), None);
}

#[test]
fn highlight_lookup_streams_across_visible_cells() {
    let spans = [
        HighlightRect {
            row: 1,
            col: 2,
            width: 3,
            active: false,
        },
        HighlightRect {
            row: 2,
            col: 0,
            width: 1,
            active: true,
        },
    ];
    let mut cursor = 0;
    assert_eq!(search_highlight_at(&spans, &mut cursor, 1, 1), None);
    assert_eq!(search_highlight_at(&spans, &mut cursor, 1, 2), Some(false));
    assert_eq!(search_highlight_at(&spans, &mut cursor, 1, 4), Some(false));
    assert_eq!(search_highlight_at(&spans, &mut cursor, 2, 0), Some(true));
    assert_eq!(search_highlight_at(&spans, &mut cursor, 2, 1), None);
    assert_eq!(cursor, spans.len());
}

#[test]
fn wide_geometry_is_one_row_and_hit_testing_reuses_paint_rects() {
    let bar = search_bar_geometry(
        1200.0,
        800.0,
        10.0,
        20.0,
        &kettle_i18n::Translator::default(),
    );
    assert_eq!(bar.rows, 1);
    assert_eq!(bar.reserved_height, 30.0);
    for control in SearchControl::ALL {
        let rect = bar.control_rect(control);
        assert_inside(bar, rect);
        let (x, y) = center(rect);
        assert_eq!(bar.hit_test(x, y), Some(control));
    }
    assert_eq!(bar.content_rect(1200.0, 800.0), (0.0, 0.0, 1200.0, 770.0));
}

#[test]
fn narrow_geometry_wraps_without_hiding_editor_or_close() {
    let bar = search_bar_geometry(320.0, 800.0, 8.0, 18.0, &kettle_i18n::Translator::default());
    assert!(bar.rows >= 2);
    for control in SearchControl::ALL {
        assert_inside(bar, bar.control_rect(control));
    }
    // The invariant also holds at a deliberately pathological one-cell
    // surface; Close moves to another row instead of covering the editor.
    let tiny = search_bar_geometry(8.0, 800.0, 8.0, 18.0, &kettle_i18n::Translator::default());
    assert_inside(tiny, tiny.editor);
    assert_inside(tiny, tiny.close);
    assert_ne!(tiny.editor.1, tiny.close.1);
}

/// The bar spends its widest slot on status, so it must not spend it
/// restating what the pane already shows. A plain `Match` is confirmed by
/// the highlight and the jump to it; the states that need attention are
/// the ones that print.
#[test]
fn only_the_status_that_tells_you_something_is_printed() {
    let base = SearchOverlay {
        query: "needle".into(),
        cursor_byte: 6,
        focused: SearchControl::Editor,
        ..SearchOverlay::default()
    };
    let bar = search_bar_geometry(
        1400.0,
        800.0,
        10.0,
        20.0,
        &kettle_i18n::Translator::default(),
    );

    let matched = SearchOverlay {
        status: SearchStatus::Match,
        ..base.clone()
    };
    let text = search_bar_text(&matched, bar, 10.0);
    assert!(
        !text.contains(SearchStatus::Match.label(&kettle_i18n::Translator::default())),
        "a plain match is shown by the highlight, not by the bar: {text}"
    );

    for status in [
        SearchStatus::NoMatch,
        SearchStatus::Wrapped,
        SearchStatus::Invalid,
        SearchStatus::TooLong,
        SearchStatus::Limited,
    ] {
        let overlay = SearchOverlay {
            status,
            ..base.clone()
        };
        let text = search_bar_text(&overlay, bar, 10.0);
        assert!(
            text.contains(status.label(&kettle_i18n::Translator::default())),
            "{status:?} must still be reported: {text}"
        );
    }
}

const ELLIPSIS: char = '\u{2026}';

/// A control must never abut the status text, in either layout.
///
/// `Enter: Next` fills its slot exactly, so a single-column gap renders
/// `Enter: Next No match` as one phrase. The wide layout and the narrow
/// multi-row layout both need the two-column gap between groups.
#[test]
fn no_control_abuts_the_status_text_in_either_layout() {
    // Both invert states, so `Enter: Prev` is actually generated rather
    // than only asserted against, and every status that prints.
    let statuses = [
        SearchStatus::NoMatch,
        SearchStatus::Wrapped,
        SearchStatus::Invalid,
        SearchStatus::TooLong,
        SearchStatus::Limited,
        SearchStatus::Start,
        SearchStatus::End,
    ];
    let mut saw_prev = false;
    let mut saw_next = false;
    for width in [2400.0_f32, 1400.0, 1000.0, 820.0, 700.0, 600.0] {
        let cell_width = 7.8_f32;
        let bar = search_bar_geometry(
            width,
            800.0,
            cell_width,
            20.0,
            &kettle_i18n::Translator::default(),
        );
        for invert in [false, true] {
            for status in statuses {
                let overlay = SearchOverlay {
                    query: "needle".into(),
                    cursor_byte: 6,
                    invert,
                    status,
                    focused: SearchControl::Editor,
                    ..SearchOverlay::default()
                };
                let text = search_bar_text(&overlay, bar, cell_width);
                let control = if invert { "Enter: Prev" } else { "Enter: Next" };
                saw_prev |= invert && text.contains(control);
                saw_next |= !invert && text.contains(control);
                let abutting = format!(
                    "{control} {}",
                    status.label(&kettle_i18n::Translator::default())
                );
                for line in text.lines() {
                    assert!(
                        !line.contains(&abutting),
                        "control abuts status at width={width} rows={} \
                             invert={invert} status={status:?}: {line}",
                        bar.rows
                    );
                }
            }
        }
    }
    assert!(
        saw_prev && saw_next,
        "the sweep must actually render both invert states, or the \
             assertions above are vacuous"
    );
}

/// No control label may be ellipsized at any realistic cell width.
///
/// The bar stores its layout in pixels and divides back to columns, so a
/// label that exactly fills its slot sits one f32 rounding step away from
/// truncation. `Enter: Next` fills all 11 columns of its slot, and a plain
/// floor clips it to `Enter: Ne...` at cell widths like 7.007; `x Close`
/// and `Case: Ignore >` share that exposure. Sweep the range instead of
/// trusting one convenient number.
#[test]
fn no_search_control_label_is_ellipsized_at_any_cell_width() {
    let states = [
        (true, true, SearchCaseMode::Ignore),
        (false, false, SearchCaseMode::Smart),
        (true, false, SearchCaseMode::Match),
    ];
    let mut checked = 0usize;
    // 6.000..16.000 in 0.005 steps: wide enough to cover real font metrics,
    // fine enough to land on the values that previously lost a column.
    for step in 0..=2000 {
        let cell_width = 6.0 + step as f32 * 0.005;
        let bar = search_bar_geometry(
            2400.0,
            800.0,
            cell_width,
            20.0,
            &kettle_i18n::Translator::default(),
        );
        if bar.rows != 1 {
            continue;
        }
        for (wrap, invert, case_mode) in states {
            let overlay = SearchOverlay {
                query: "needle".into(),
                cursor_byte: 6,
                wrap,
                invert,
                case_mode,
                status: SearchStatus::NoMatch,
                focused: SearchControl::Editor,
                ..SearchOverlay::default()
            };
            let text = search_bar_text(&overlay, bar, cell_width);
            assert!(
                !text.contains(ELLIPSIS),
                "a control was truncated at cell_width={cell_width}: {text}"
            );
            checked += 1;
        }
    }
    assert!(checked > 1000, "the sweep must actually exercise wide mode");
}

#[test]
fn rich_bar_is_status_only_and_never_emits_a_global_counter() {
    let search = SearchOverlay {
        query: "needle".into(),
        cursor_byte: 6,
        wrap: true,
        case_mode: SearchCaseMode::Ignore,
        invert: true,
        status: SearchStatus::Wrapped,
        focused: SearchControl::Editor,
        ..SearchOverlay::default()
    };
    let bar = search_bar_geometry(
        1200.0,
        800.0,
        10.0,
        20.0,
        &kettle_i18n::Translator::default(),
    );
    let text = search_bar_text(&search, bar, 10.0);
    assert!(text.contains("needle"));
    assert!(text.contains("Case: Ignore"));
    // The state labels say the state rather than making the reader decode
    // a checkbox, and `Invert` names what it actually flips: the direction
    // Enter searches.
    assert!(
        text.contains("Wrap: On"),
        "wrap state is spelled out: {text}"
    );
    assert!(
        text.contains("Enter: Prev"),
        "invert=true means Enter searches backward: {text}"
    );
    assert!(
        !text.contains("[x]") && !text.contains("[ ]"),
        "the checkbox idiom is gone: {text}"
    );
    assert!(
        !text.contains("[needle"),
        "the query is not bracket-delimited: {text}"
    );
    assert!(text.contains("Wrapped"));
    assert!(text.contains("× Close"));
    assert!(
        !text.contains("/"),
        "global match totals must stay out of chrome"
    );
}

#[test]
fn editor_defensively_normalizes_controls_and_bounds_its_line() {
    let search = SearchOverlay {
        query: "a\nb\tc".into(),
        cursor_byte: usize::MAX,
        focused: SearchControl::Editor,
        ..SearchOverlay::default()
    };
    let label = search_editor_label(&search, 8);
    assert!(!label.contains('\n'));
    assert!(!label.contains('\t'));
    assert!(super::display_width(&label) <= 8);
}

/// The caret is painted between cells, so moving it, or moving focus off
/// the editor, never shifts the query's characters.
#[test]
fn caret_and_focus_never_shift_the_query_text() {
    let bar = search_bar_geometry(
        1400.0,
        800.0,
        10.0,
        20.0,
        &kettle_i18n::Translator::default(),
    );
    let base = SearchOverlay {
        query: "abcdef".to_string(),
        cursor_byte: 6,
        focused: SearchControl::Editor,
        ..SearchOverlay::default()
    };
    let reference = search_bar_segments(&base, bar, 10.0)[1].clone();
    assert_eq!(reference.text, "abcdef");
    for cursor_byte in 0..=6 {
        for focused in [SearchControl::Editor, SearchControl::Next] {
            let search = SearchOverlay {
                cursor_byte,
                focused,
                ..base.clone()
            };
            assert_eq!(search_bar_segments(&search, bar, 10.0)[1], reference);
        }
    }
}

/// Button labels sit in the middle of their slots with at least half a
/// cell of fill on each side, and a toggle keeps its name in place when
/// its value changes.
#[test]
fn button_labels_are_centered_and_toggles_do_not_move() {
    let cell = 10.0;
    let bar = search_bar_geometry(
        1400.0,
        800.0,
        cell,
        20.0,
        &kettle_i18n::Translator::default(),
    );
    let variants = |index: usize| -> Vec<SearchOverlay> {
        let base = SearchOverlay::default();
        match index {
            4 => vec![
                base.clone(),
                SearchOverlay {
                    wrap: false,
                    ..base
                },
            ],
            5 => [
                SearchCaseMode::Smart,
                SearchCaseMode::Match,
                SearchCaseMode::Ignore,
            ]
            .into_iter()
            .map(|case_mode| SearchOverlay {
                case_mode,
                ..base.clone()
            })
            .collect(),
            6 => vec![
                base.clone(),
                SearchOverlay {
                    invert: true,
                    ..base
                },
            ],
            _ => vec![base],
        }
    };
    for index in [2, 3, 4, 5, 6, 8] {
        let segments: Vec<_> = variants(index)
            .iter()
            .map(|search| search_bar_segments(search, bar, cell)[index].clone())
            .collect();
        for segment in &segments {
            let width = super::display_width(&segment.text) as f32 * cell;
            let before = segment.left - segment.rect.0;
            let after = segment.rect.0 + segment.rect.2 - (segment.left + width);
            assert!(
                before >= cell / 2.0 - 0.5 && after >= cell / 2.0 - 0.5,
                "under half a cell of padding: {segment:?}"
            );
            assert!((before - after).abs() <= cell, "not centered: {segment:?}");
            assert_eq!(segment.left, segments[0].left, "label moved: {segment:?}");
        }
        if index == 5 {
            let chevron = |segment: &super::SearchBarSegment| {
                let (at, _) = segment
                    .text
                    .char_indices()
                    .find(|(_, c)| *c == '›')
                    .unwrap();
                super::display_width(&segment.text[..at])
            };
            for segment in &segments {
                assert_eq!(chevron(segment), chevron(&segments[0]), "{segment:?}");
            }
        }
    }
}

/// A focused button's label stays readable while it is hovered or held,
/// not only on the plain accent. One Dark's dark label on its yellow
/// accent fell to about 3.3:1 while pressed.
#[test]
fn a_focused_label_holds_contrast_on_the_fill_as_painted() {
    let rgb = |hex: u32| super::Rgb::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    let (background, foreground) = (rgb(0x282c34), rgb(0xabb2bf));
    let (accent, bar) = (rgb(0xe5c07b), rgb(0x5c6370));
    for linear in [true, false] {
        for tint in [None, Some((foreground, 0.14)), Some((background, 0.45))] {
            let label = super::search_focused_label(background, bar, accent, tint, linear);
            let alpha = f64::from(super::SEARCH_FOCUSED_FILL_ALPHA);
            let fill = super::color::over(bar, accent, alpha, linear);
            let fill = tint.map_or(fill, |(tint, alpha)| {
                super::color::over(fill, tint, f64::from(alpha), linear)
            });
            let ratio = super::color::contrast_ratio(label, fill);
            assert!(ratio >= 4.49, "{tint:?} linear={linear}: {ratio:.2}:1");
        }
    }
}

/// An sRGB surface blends in linear light. Kettle's default theme showed
/// its pressed, focused Next button at (166, 131, 82): the accent fill
/// (216, 170, 104) under the background (26, 27, 38) at 45%.
#[test]
fn compositing_matches_what_an_srgb_surface_paints() {
    let painted = super::color::over(
        super::Rgb::new(216, 170, 104),
        super::Rgb::new(26, 27, 38),
        0.45,
        true,
    );
    for (got, want) in [(painted.r, 166), (painted.g, 131), (painted.b, 82)] {
        assert!(got.abs_diff(want) <= 1, "{painted:?}");
    }
}

/// Lay out every segment the way paint does, and return each one's glyph
/// clusters in window x.
fn shaped_lane(search: &SearchOverlay) -> Vec<Vec<super::SearchEditorCluster>> {
    let (cell, row) = (10.0, 26.0);
    let bar = search_bar_geometry(
        1400.0,
        800.0,
        cell,
        16.0,
        &kettle_i18n::Translator::default(),
    );
    let mut font_system = glyphon::FontSystem::new();
    for face in kettle_config::font::all() {
        super::load_bundled_font(&mut font_system, face);
    }
    let metrics = glyphon::Metrics::new(13.0, row);
    search_bar_segments(search, bar, cell)
        .iter()
        .map(|segment| {
            let mut buffer = glyphon::Buffer::new(&mut font_system, metrics);
            buffer.set_wrap(glyphon::cosmic_text::Wrap::None);
            super::shape_search_segment(
                &mut font_system,
                &mut buffer,
                segment,
                metrics,
                glyphon::Family::Name("JetBrains Mono"),
                true,
            );
            super::segment_clusters(&buffer, segment)
                .into_iter()
                .map(|cluster| super::SearchEditorCluster {
                    left: segment.left + cluster.left,
                    right: segment.left + cluster.right,
                    ..cluster
                })
                .collect()
        })
        .collect()
}

/// Each control is shaped in its own buffer from its own slot, so wide or
/// fallback glyphs in the query cannot push the controls after the editor.
#[test]
fn query_content_never_moves_other_controls() {
    let ascii = shaped_lane(&SearchOverlay {
        query: "abcd".to_string(),
        ..SearchOverlay::default()
    });
    let wide = shaped_lane(&SearchOverlay {
        query: "日本語🙂é 日本語🙂é".to_string(),
        ..SearchOverlay::default()
    });
    for index in (0..SEARCH_SEGMENTS).filter(|index| *index != 1) {
        assert_eq!(
            ascii[index], wide[index],
            "segment {index} moved with the query"
        );
    }
    assert!(!ascii[8].is_empty(), "Close has glyphs");
}

/// Caret positions follow the shaped glyphs: one per character boundary,
/// moving right through left-to-right text, whatever each glyph's width.
#[test]
fn caret_positions_follow_the_shaped_query() {
    let lane = shaped_lane(&SearchOverlay {
        query: "a日b".to_string(),
        ..SearchOverlay::default()
    });
    let stops = super::cluster_stops(&lane[1]);
    let bytes: Vec<usize> = stops.iter().map(|stop| stop.0).collect();
    assert_eq!(bytes, [0, 1, 4, 5]);
    assert!(
        stops.windows(2).all(|pair| pair[0].1 < pair[1].1),
        "stops must move right: {stops:?}"
    );
    assert_eq!(
        super::stop_x(&stops, 2),
        stops[1].1,
        "inside 日 is its start"
    );
    assert_eq!(
        super::stop_x(&stops, 99),
        stops[3].1,
        "past the end is the end"
    );
}

/// A query that starts with Hebrew is a right-to-left paragraph. It is
/// still set flush left in the editor, and its caret positions run from
/// right to left.
#[test]
fn a_right_to_left_query_stays_in_view_and_its_caret_runs_leftward() {
    let search = SearchOverlay {
        query: "שלום".to_string(),
        ..SearchOverlay::default()
    };
    let bar = search_bar_geometry(
        1400.0,
        800.0,
        10.0,
        16.0,
        &kettle_i18n::Translator::default(),
    );
    let editor_left = search_bar_segments(&search, bar, 10.0)[1].left;
    let clusters = &shaped_lane(&search)[1];
    assert_eq!(clusters.len(), 4);
    assert!(clusters.iter().all(|cluster| cluster.rtl));
    let leftmost = clusters.iter().map(|c| c.left).fold(f32::MAX, f32::min);
    assert!(
        (leftmost - editor_left).abs() < 1.0,
        "flush left, not against the far edge: {leftmost} vs {editor_left}"
    );
    let stops = super::cluster_stops(clusters);
    let bytes: Vec<usize> = stops.iter().map(|stop| stop.0).collect();
    assert_eq!(bytes, [0, 2, 4, 6, 8]);
    assert!(
        stops.windows(2).all(|pair| pair[0].1 > pair[1].1),
        "stops must move left: {stops:?}"
    );
    // Selecting the first letter covers exactly its glyph.
    let first = clusters.iter().find(|cluster| cluster.start == 0).unwrap();
    assert_eq!(
        super::cluster_spans(clusters, 0, 2),
        [(first.left, first.right)]
    );
}

/// A right-to-left query wider than the editor still starts at its left
/// edge, so the end being typed stays in view.
#[test]
fn a_long_right_to_left_query_keeps_its_typing_end_in_view() {
    let search = SearchOverlay {
        query: "שלום ".repeat(60),
        cursor_byte: usize::MAX,
        ..SearchOverlay::default()
    };
    let bar = search_bar_geometry(
        1400.0,
        800.0,
        10.0,
        16.0,
        &kettle_i18n::Translator::default(),
    );
    let editor_left = search_bar_segments(&search, bar, 10.0)[1].left;
    let clusters = &shaped_lane(&search)[1];
    let leftmost = clusters.iter().map(|c| c.left).fold(f32::MAX, f32::min);
    assert!(
        (leftmost - editor_left).abs() < 1.0,
        "{leftmost} vs {editor_left}"
    );
}

/// The editor's reading order comes from the whole query, not from the
/// slice that is scrolled into view, so scrolling never reorders it.
#[test]
fn scrolling_a_mixed_query_keeps_its_reading_order() {
    let query = "xx שלום abc def";
    let at = |horizontal_scroll: usize| {
        let search = SearchOverlay {
            query: query.to_string(),
            horizontal_scroll,
            ..SearchOverlay::default()
        };
        let clusters = shaped_lane(&search)[1].clone();
        let skip = super::search_editor_body(&search, 999).1;
        let left_of = |needle: &str| {
            let byte = query.find(needle).unwrap() - skip;
            clusters.iter().find(|c| c.start == byte).unwrap().left
        };
        (left_of("ם"), left_of("abc"))
    };
    // Unscrolled, and scrolled so the slice starts with a Hebrew letter:
    // the Hebrew stays left of "abc" both times.
    for scroll in [0, 6] {
        let (hebrew, latin) = at(scroll);
        assert!(hebrew < latin, "scroll {scroll}: {hebrew} vs {latin}");
    }
    assert!(super::starts_right_to_left("123 שלום abc"));
    assert!(!super::starts_right_to_left("(abc שלום"));
    assert!(!super::starts_right_to_left(""));
    // Only strong characters decide: a byte-order mark, Arabic-Indic
    // digits and a leading combining mark do not; RLM does.
    assert!(!super::starts_right_to_left("\u{FEFF}error: "));
    assert!(!super::starts_right_to_left("٣ abc"));
    assert!(super::starts_right_to_left("\u{200F}abc"));
}

/// Glyphs that share bytes form one cluster, so the caret after a split
/// vowel such as the one in "कि" goes after the whole cluster.
#[test]
fn a_cluster_drawn_as_several_glyphs_is_one_caret_step() {
    let glyph = |start, end, left, right| super::SearchEditorCluster {
        start,
        end,
        left,
        right,
        rtl: false,
    };
    // "कित": the i-matra is drawn first, then क, then त.
    let clusters = super::merge_clusters(
        [
            glyph(0, 6, 0.0, 6.7),
            glyph(0, 6, 6.7, 26.4),
            glyph(6, 9, 26.4, 40.5),
        ]
        .into_iter(),
    );
    assert_eq!(clusters, [glyph(0, 6, 0.0, 26.4), glyph(6, 9, 26.4, 40.5)]);
    assert_eq!(
        super::cluster_stops(&clusters),
        [(0, 0.0), (6, 26.4), (9, 40.5)]
    );
    assert_eq!(super::cluster_spans(&clusters, 0, 6), [(0.0, 26.4)]);
}

/// Mixed-direction text: a selection is painted over each cluster it
/// covers, in as many pieces as the layout needs.
#[test]
fn a_selection_covers_its_clusters_in_every_direction() {
    let cluster = |start, end, left, right, rtl| super::SearchEditorCluster {
        start,
        end,
        left,
        right,
        rtl,
    };
    // "ab" then a right-to-left run "שב" drawn as ב ש, then "c".
    let clusters = vec![
        cluster(0, 1, 0.0, 10.0, false),
        cluster(1, 2, 10.0, 20.0, false),
        cluster(2, 4, 30.0, 40.0, true),
        cluster(4, 6, 20.0, 30.0, true),
        cluster(6, 7, 40.0, 50.0, false),
    ];
    assert_eq!(
        super::cluster_stops(&clusters),
        [
            (0, 0.0),
            (1, 10.0),
            (2, 40.0),
            (4, 30.0),
            (6, 40.0),
            (7, 50.0)
        ]
    );
    // "bש" covers b and ש, which are not next to each other.
    assert_eq!(
        super::cluster_spans(&clusters, 1, 4),
        [(10.0, 20.0), (30.0, 40.0)]
    );
    assert_eq!(super::cluster_spans(&clusters, 0, 7), [(0.0, 50.0)]);
    assert!(super::cluster_spans(&clusters, 3, 3).is_empty());
    assert_eq!(super::cluster_stops(&[]), [(0, 0.0)]);
}

#[test]
fn editor_width_uses_the_rounding_safe_column_helper() {
    let src = super::production_source();
    assert!(
        src.contains("let editor_cols = search_bar_columns(geometry.editor.2, cell_width)"),
        "the editor must use the same f32 rounding-safe width as every sibling control"
    );
}

#[test]
fn editor_horizontal_clip_never_splits_extended_graphemes() {
    // Nothing scrolled means nothing dropped, even a zero-width start.
    assert_eq!(drop_cols_front("\u{200B}abc", 0), "\u{200B}abc");
    assert_eq!(drop_cols_front("\u{301}abc", 0), "\u{301}abc");
    assert_eq!(drop_cols_front("ab", 5), "");
    assert_eq!(drop_cols_front("a\u{301}b", 1), "b");
    assert_eq!(drop_cols_front("👩‍💻x", 1), "👩‍💻x");
    assert_eq!(drop_cols_front("👩‍💻x", 2), "x");
}

#[test]
fn status_vocabulary_is_bounded_and_semantic() {
    assert_eq!(
        SearchStatus::Typing.label(&kettle_i18n::Translator::default()),
        "Type to search"
    );
    assert_eq!(
        SearchStatus::Searching.label(&kettle_i18n::Translator::default()),
        "Searching…"
    );
    assert_eq!(
        SearchStatus::Match.label(&kettle_i18n::Translator::default()),
        "Match"
    );
    assert_eq!(
        SearchStatus::Wrapped.label(&kettle_i18n::Translator::default()),
        "Wrapped"
    );
    assert_eq!(
        SearchStatus::Start.label(&kettle_i18n::Translator::default()),
        "Start reached"
    );
    assert_eq!(
        SearchStatus::End.label(&kettle_i18n::Translator::default()),
        "End reached"
    );
    assert_eq!(
        SearchStatus::NoMatch.label(&kettle_i18n::Translator::default()),
        "No match"
    );
    assert_eq!(
        SearchStatus::Limited.label(&kettle_i18n::Translator::default()),
        "Results limited"
    );
    assert_eq!(
        SearchStatus::Invalid.label(&kettle_i18n::Translator::default()),
        "Invalid pattern"
    );
    assert_eq!(
        SearchStatus::TooComplex.label(&kettle_i18n::Translator::default()),
        "Pattern too complex"
    );
    assert_eq!(
        SearchStatus::TooLong.label(&kettle_i18n::Translator::default()),
        "Query too long"
    );

    let bar = search_bar_geometry(
        1200.0,
        800.0,
        10.0,
        20.0,
        &kettle_i18n::Translator::default(),
    );
    let status_columns = (bar.status.2 / 10.0).floor() as usize;
    for status in [
        SearchStatus::Typing,
        SearchStatus::Searching,
        SearchStatus::Match,
        SearchStatus::Wrapped,
        SearchStatus::Start,
        SearchStatus::End,
        SearchStatus::NoMatch,
        SearchStatus::Limited,
        SearchStatus::Invalid,
        SearchStatus::TooComplex,
        SearchStatus::TooLong,
    ] {
        assert!(
            super::display_width(status.label(&kettle_i18n::Translator::default()))
                <= status_columns,
            "wide status lane clipped {}",
            status.label(&kettle_i18n::Translator::default())
        );
    }
}

/// English lays out exactly as it did with fixed widths; a translation
/// gets each control as wide as its longest label plus the same fill.
#[test]
fn control_widths_follow_the_labels_language() {
    use super::SearchBarWidths;
    use kettle_i18n::{Language, Translator};
    let en = SearchBarWidths::new(&Translator::new(Language::En));
    assert_eq!(en, SearchBarWidths::ENGLISH);
    let es = SearchBarWidths::new(&Translator::new(Language::Es));
    for (english, spanish) in [
        (en.label, es.label),
        (en.previous, es.previous),
        (en.next, es.next),
        (en.wrap, es.wrap),
        (en.case, es.case),
        (en.invert, es.invert),
        (en.status, es.status),
        (en.close, es.close),
    ] {
        assert!(spanish >= english);
    }
    // "Siguiente ›" is 11 columns; Next keeps its two columns of fill.
    assert_eq!(es.next, 13);
}

/// Pseudo-locale layout check: with text about 38% longer, a wide bar
/// still shows every control's label whole, and at any width the
/// controls never overlap each other or leave the bar.
#[cfg(debug_assertions)]
#[test]
fn a_pseudo_bar_keeps_its_controls_whole_and_apart() {
    use kettle_i18n::Translator;
    let tr = Translator::pseudo();
    let bar = search_bar_geometry(2400.0, 800.0, 10.0, 20.0, &tr);
    assert_eq!(bar.rows, 1, "wide enough for the single-row layout");
    let search = SearchOverlay {
        tr,
        ..SearchOverlay::default()
    };
    assert!(!search_bar_text(&search, bar, 10.0).contains('…'));
    for width in (300..=2400).step_by(25) {
        let bar = search_bar_geometry(width as f32, 800.0, 10.0, 20.0, &tr);
        let controls = [
            bar.previous,
            bar.next,
            bar.wrap,
            bar.case_mode,
            bar.invert,
            bar.status,
            bar.close,
        ];
        let inside = |r: super::Rect4| {
            r.2 <= 0.0
                || (r.0 >= bar.rect.0 - 0.5
                    && r.0 + r.2 <= bar.rect.0 + bar.rect.2 + 0.5
                    && r.1 >= bar.rect.1 - 0.5
                    && r.1 + r.3 <= bar.rect.1 + bar.rect.3 + 0.5)
        };
        let overlap = |a: super::Rect4, b: super::Rect4| {
            a.2 > 0.0
                && b.2 > 0.0
                && a.0 < b.0 + b.2 - 0.5
                && b.0 < a.0 + a.2 - 0.5
                && a.1 < b.1 + b.3 - 0.5
                && b.1 < a.1 + a.3 - 0.5
        };
        for (i, a) in controls.iter().enumerate() {
            assert!(
                inside(*a),
                "width {width}: {a:?} leaves the bar {:?}",
                bar.rect
            );
            for b in &controls[i + 1..] {
                assert!(!overlap(*a, *b), "width {width}: {a:?} overlaps {b:?}");
            }
        }
    }
}

/// A Spanish bar shows every word whole at a width that fits it, with no
/// ellipsis, and its status lane fits every status.
#[test]
fn a_spanish_bar_shows_its_words_whole() {
    use kettle_i18n::{Language, Translator};
    let tr = Translator::new(Language::Es);
    let bar = search_bar_geometry(2000.0, 800.0, 10.0, 20.0, &tr);
    assert_eq!(bar.rows, 1, "wide enough for the single-row layout");
    let search = SearchOverlay {
        tr,
        status: SearchStatus::TooComplex,
        ..SearchOverlay::default()
    };
    let text = search_bar_text(&search, bar, 10.0);
    for word in [
        "Buscar",
        "‹ Anterior",
        "Siguiente ›",
        "Circular: sí",
        "Mayús.: Inteligente ›",
        "Enter: siguiente",
        "Patrón demasiado complejo",
        "× Cerrar",
    ] {
        assert!(text.contains(word), "{word:?} missing from {text:?}");
    }
    assert!(!text.contains('…'), "{text:?}");
    let status_columns = (bar.status.2 / 10.0).floor() as usize;
    for status in SearchStatus::ALL {
        assert!(super::display_width(status.label(&tr)) <= status_columns);
    }
}

#[test]
fn ligature_carets_and_selection_follow_grapheme_boundaries() {
    use super::{SearchEditorCluster, cluster_spans, cluster_stops, grapheme_clusters};
    let clusters = grapheme_clusters(
        "ffi",
        vec![SearchEditorCluster {
            start: 0,
            end: 3,
            left: 0.0,
            right: 30.0,
            rtl: false,
        }],
    );
    assert_eq!(
        cluster_stops(&clusters),
        [(0, 0.0), (1, 10.0), (2, 20.0), (3, 30.0)],
        "ligature omitted an editable grapheme boundary"
    );
    assert_eq!(cluster_spans(&clusters, 1, 2), [(10.0, 20.0)]);
    let arabic = grapheme_clusters(
        "\u{644}\u{627}",
        vec![SearchEditorCluster {
            start: 0,
            end: 4,
            left: 10.0,
            right: 30.0,
            rtl: true,
        }],
    );
    assert_eq!(cluster_stops(&arabic), [(0, 30.0), (2, 20.0), (4, 10.0)]);
    assert_eq!(cluster_spans(&arabic, 0, 2), [(20.0, 30.0)]);
    let accent = grapheme_clusters(
        "e\u{301}",
        vec![SearchEditorCluster {
            start: 0,
            end: 3,
            left: 0.0,
            right: 10.0,
            rtl: false,
        }],
    );
    assert_eq!(cluster_stops(&accent), [(0, 0.0), (3, 10.0)]);
}

#[test]
fn search_controls_use_natural_chrome_advance() {
    let source = super::production_source();
    let paint = source
        .split("&& let Some(search) = overlay.search.as_ref()")
        .nth(1)
        .unwrap()
        .split("let geometry = search_bar_geometry")
        .next()
        .unwrap();
    let input = include_str!("../../kettle-ui/src/app/search.rs");
    assert!(
        paint.contains("self.overlay_text_cell_width()") && !input.contains("renderer.cell_w"),
        "search paint and input still measure narrowed terminal cells"
    );
    let search = SearchOverlay {
        wrap: false,
        ..SearchOverlay::default()
    };
    let geometry = search_bar_geometry(2400.0, 800.0, 8.0, 16.0, &search.tr);
    let segments = search_bar_segments(&search, geometry, 8.0);
    let wrap = segments
        .iter()
        .find(|segment| segment.control == Some(SearchControl::Wrap))
        .unwrap();
    assert!(
        wrap.text.contains("Off"),
        "Wrap state was clipped: {}",
        wrap.text
    );
}
