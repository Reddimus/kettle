use crate::chrome;
use crate::color::contrast_ratio;
use crate::headless_tests::{capture, focused, pane, renderer, snapshot_of};
use crate::theme_picker::{
    FooterInk, LOW_CONTRAST, ThemeFollow, ThemePickerFooter, ThemeSlot, footer_colors, footer_ink,
};
use crate::{
    ContextMenu, ContextMenuRow, MinimumContrastCache, Overlay, menu, minimum_contrast_token,
    step_minimum_contrast, theme_footer_height, theme_footer_layout, theme_readability,
};
use kettle_config::{Rgb, Theme};
use kettle_i18n::{Language, Translator};

const EN: Translator = Translator::new(Language::En);
const ES: Translator = Translator::new(Language::Es);

fn footer(sample: &str, follow: ThemeFollow, editing: Option<ThemeSlot>) -> ThemePickerFooter {
    ThemePickerFooter {
        follow,
        editing,
        dark_theme: "TokyoNight Night".into(),
        light_theme: "Kettle Day".into(),
        fixed_theme: "Dracula".into(),
        minimum_contrast: 0.0,
        sample: Theme::by_name(sample),
        show_sample: true,
        status_lines: 2,
        height: theme_footer_height(17.0, true, 2),
        min_columns: (0, 0),
    }
}

fn columns(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// The contrast button steps through Off, 3:1, 4.5:1 and 7:1 and back to
/// Off; a value set by hand moves to its neighbour, and the config token is
/// the shortest decimal.
#[test]
fn minimum_contrast_steps_round_trip_and_keep_hand_values_in_order() {
    let mut value = 0.0;
    let mut seen = Vec::new();
    for _ in 0..4 {
        value = step_minimum_contrast(value, true);
        seen.push(minimum_contrast_token(value));
    }
    assert_eq!(seen, ["3", "4.5", "7", "0"]);
    assert_eq!(step_minimum_contrast(2.5, true), 3.0);
    assert_eq!(step_minimum_contrast(2.5, false), 0.0);
    assert_eq!(step_minimum_contrast(0.0, false), 7.0);
    assert_eq!(step_minimum_contrast(21.0, true), 0.0);
    assert_eq!(minimum_contrast_token(2.25), "2.25");
    assert_eq!(minimum_contrast_token(1.0), "0");
    assert_eq!(minimum_contrast_token(f32::NAN), "0");
}

/// The readout measures colours as the terminal draws them: through the
/// same minimum-contrast lift as cell text. Kettle's default pair reads
/// without a warning, some bundled theme does not, and a 4.5:1 minimum lifts
/// that theme's colours to 4.5:1.
#[test]
fn theme_readability_follows_the_terminals_contrast_lift() {
    for name in [
        kettle_config::Config::default().dark_theme,
        kettle_config::Config::default().light_theme,
    ] {
        let readability = theme_readability(&Theme::by_name(&name), 0.0);
        assert!(
            readability.lowest >= LOW_CONTRAST,
            "{name}: lowest {:.2}:1 at slot {}",
            readability.lowest,
            readability.lowest_slot
        );
        assert_eq!(readability.lifted, 0);
    }
    let (name, low) = Theme::list()
        .into_iter()
        .map(|name| (name, theme_readability(&Theme::by_name(name), 0.0)))
        .find(|(_, r)| r.lowest < LOW_CONTRAST)
        .expect("a bundled theme with a colour below 3:1");
    let theme = Theme::by_name(name);
    let slot = low.lowest_slot;
    assert!(
        contrast_ratio(theme.palette[slot], theme.background) < LOW_CONTRAST,
        "{name}"
    );
    let lifted = theme_readability(&theme, 4.5);
    assert!(lifted.lowest >= 4.5 - 0.05, "{name}: {:.2}", lifted.lowest);
    assert!(lifted.lifted > 0);
    // A colour lifted to the minimum never reads below it: the readout
    // rounds a ratio within a hair of the step up, not down.
    for minimum in [3.0_f32, 4.5, 7.0] {
        for name in Theme::list() {
            let r = theme_readability(&Theme::by_name(name), minimum);
            let reached = r.lowest >= f64::from(minimum) - 0.01;
            let shown: f64 = crate::theme_picker::ratio_text(r.lowest)
                .trim_end_matches(":1")
                .parse()
                .unwrap();
            assert!(
                !reached || shown >= f64::from(minimum),
                "{name} at {minimum}: {:.4} reads {shown}",
                r.lowest
            );
        }
    }
    // The production cell path gives the same ink.
    let mut cache = MinimumContrastCache::default();
    assert_eq!(
        cache.resolve(theme.palette[slot], theme.background, 4.5),
        crate::color::with_min_contrast(theme.palette[slot], theme.background, 4.5)
    );
    // The low theme's card carries the warning mark; the default's does not.
    let mut low_footer = footer(name, ThemeFollow::System, Some(ThemeSlot::Dark));
    let layout = theme_footer_layout(&low_footer, &EN, (0.0, 0.0, 720.0), (8.4, 17.0));
    assert!(layout.low_mark.is_some(), "{name}");
    assert_eq!(layout.lowest_square.map(|(_, slot)| slot), Some(slot));
    low_footer.sample = Theme::by_name("TokyoNight Night");
    let layout = theme_footer_layout(&low_footer, &EN, (0.0, 0.0, 720.0), (8.4, 17.0));
    assert!(layout.low_mark.is_none());
}

/// The status line names how the theme is chosen: a schedule, the system,
/// a forced side or one fixed theme. Fixed shows only that theme.
#[test]
fn the_status_line_names_the_schedule_and_both_sides() {
    let status = |follow, editing, tr: &Translator| {
        let f = footer("TokyoNight Night", follow, editing);
        theme_footer_layout(&f, tr, (0.0, 0.0, 720.0), (8.4, 17.0))
            .lines
            .iter()
            .map(|line| line.text())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let system = status(ThemeFollow::System, Some(ThemeSlot::Dark), &EN);
    assert!(system.contains("Follow system"), "{system}");
    assert!(system.contains("Dark: TokyoNight Night"), "{system}");
    assert!(system.contains("Light: Kettle Day"), "{system}");
    assert!(system.contains("Contrast: Off"), "{system}");
    let schedule = status(ThemeFollow::Schedule, Some(ThemeSlot::Light), &EN);
    assert!(schedule.contains("Follow schedule"), "{schedule}");
    assert!(!schedule.contains("Follow system"), "{schedule}");
    let es = status(ThemeFollow::Schedule, Some(ThemeSlot::Light), &ES);
    assert!(es.contains("Seguir el horario"), "{es}");
    assert!(es.contains("Claro: Kettle Day"), "{es}");
    let fixed = status(ThemeFollow::Fixed, None, &EN);
    assert!(fixed.contains("Fixed theme"), "{fixed}");
    assert!(fixed.contains("Theme: Dracula"), "{fixed}");
    assert!(!fixed.contains("Light:"), "{fixed}");
    let f = footer("TokyoNight Night", ThemeFollow::Fixed, None);
    let layout = theme_footer_layout(&f, &EN, (0.0, 0.0, 720.0), (8.4, 17.0));
    assert!(layout.slot_mark.is_none() && layout.slot_underline.is_none());
}

/// At the picker's narrowest, in Spanish and at 2x, every footer line the
/// picker shows fits its bounds: long theme names end in an ellipsis before
/// any label is lost, the contrast button keeps its words, and the edited
/// side's underline stays under its own label. A picker too narrow for the
/// card or the status line leaves it out rather than cutting it.
#[test]
fn the_footer_fits_the_narrowest_picker_by_shortening_names() {
    for tr in [&EN, &ES] {
        for (cw, ch) in [(8.4, 17.0), (16.8, 34.0)] {
            for width in [360.0, 500.0, 720.0] {
                let mut f = footer(
                    "TokyoNight Night",
                    ThemeFollow::Schedule,
                    Some(ThemeSlot::Light),
                );
                f.dark_theme = "Catppuccin Frappe Extra Long Night".into();
                f.light_theme = "Solarized Osaka Light Variant".into();
                f.minimum_contrast = 4.5;
                f.min_columns = crate::theme_footer_min_columns(&f, tr);
                let (card, status) = f.fits(width, cw);
                let what = format!(
                    "{} {cw} {width}",
                    tr.text(kettle_i18n::Text::PickerThemeContrast)
                );
                // The picker's own sizes: 360px at 1x, 720px at 2x.
                if width == 360.0 && cw < 10.0 || width == 720.0 {
                    assert!(card && status, "{what}: {:?}", f.min_columns);
                }
                f.show_sample = card;
                f.status_lines = if status { 2 } else { 0 };
                f.height = theme_footer_height(ch, card, f.status_lines);
                let layout = theme_footer_layout(&f, tr, (0.0, 100.0, width), (cw, ch));
                for line in &layout.lines {
                    let text = line.text();
                    let start = line.left - line.bounds.0;
                    assert!(
                        start + columns(&text) as f32 * cw <= line.bounds.2 + 0.5,
                        "{what}: {text:?} overflows {:.0}px",
                        line.bounds.2
                    );
                    assert!(line.left >= 0.0 && line.left + 1.0 < width, "{what}");
                }
                if !status {
                    assert!(layout.contrast_button.is_none(), "{what}");
                    continue;
                }
                let all: Vec<String> = layout.lines.iter().map(|line| line.text()).collect();
                let slots = all.last().expect("the themes line");
                let dark = tr.text(kettle_i18n::Text::PickerThemeDarkSlot);
                let light = tr.text(kettle_i18n::Text::PickerThemeLightSlot);
                assert!(
                    slots.contains(dark) && slots.contains(light),
                    "{what}: {slots}"
                );
                if width < 400.0 {
                    assert!(slots.contains('…'), "{what}: {slots}");
                }
                let button = crate::contrast_button_text(4.5, tr);
                assert!(
                    all.iter().any(|line| line.trim() == button),
                    "{what}: {all:?}"
                );
                let mode = tr.text(kettle_i18n::Text::PickerThemeFollowSchedule);
                assert!(all.iter().any(|line| line == mode), "{what}: {all:?}");
                let (ux, _, uw, _) = layout.slot_underline.expect("the edited side");
                let start = slots.find(light).unwrap();
                let left = layout.lines.last().unwrap().left;
                let col = columns(&slots[..start]) as f32;
                assert!((ux - (left + col * cw)).abs() <= 1.0, "{what}");
                assert!(ux + uw <= width - 15.0, "{what}");
                // The button lies inside the panel, clear of the mode label.
                let (bx, _, bw, _) = layout.contrast_button.expect("the button");
                let mode_line = &layout.lines[layout.lines.len() - 3];
                assert!(
                    mode_line.left + columns(&mode_line.text()) as f32 * cw <= bx,
                    "{what}"
                );
                assert!(bx + bw <= width, "{what}");
            }
        }
    }
}

/// The footer's text is the painter's own colours and reads at 4.5:1 on its
/// surface, and its underline, mark and edges at 3:1, in every bundled
/// theme as both the panel and the sample.
#[test]
fn footer_ink_meets_wcag_on_every_bundled_theme() {
    let mut colors = chrome::Colors::default();
    let mut cache = MinimumContrastCache::default();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let mut f = footer(name, ThemeFollow::System, Some(ThemeSlot::Dark));
        f.sample = theme.clone();
        let accent = theme.accent;
        let fc = footer_colors(&f, &theme, accent, &mut colors);
        for (ink, bg) in [
            (FooterInk::Label, theme.background),
            (FooterInk::Value, theme.background),
            (FooterInk::CardLabel, f.sample.background),
        ] {
            let fg = footer_ink(ink, &f, &theme, &mut colors, &mut cache);
            let ratio = contrast_ratio(fg, bg);
            assert!(ratio >= 4.5 - 0.01, "{name} {ink:?}: {ratio:.2}");
        }
        for (what, fg, bg) in [
            ("underline", fc.underline, theme.background),
            ("mark", fc.mark, theme.background),
            ("button edge", fc.panel_edge, theme.background),
            ("card edge", fc.card_edge, fc.card_fill),
        ] {
            let ratio = contrast_ratio(fg, bg);
            assert!(ratio >= 3.0 - 0.01, "{name} {what}: {ratio:.2}");
        }
    }
}

fn picker_with_footer(f: ThemePickerFooter, (width, row_h): (f32, f32)) -> ContextMenu {
    let rows: Vec<ContextMenuRow> = ["Dracula", "Nord", "Kettle Day"]
        .into_iter()
        .map(|label| ContextMenuRow {
            toggle: None,
            swatches: None,
            matched_chars: None,
            label: label.into(),
            separator: false,
            enabled: true,
            hint: String::new(),
        })
        .collect();
    let rows_h = rows.len() as f32 * row_h;
    ContextMenu {
        picker_header_height: 30.0,
        picker_footer: Some(f),
        anchor: (40.0, 50.0),
        rows,
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: width,
        panel_h_clamped: rows_h,
        a11y_label: None,
    }
}

fn near(p: &image::Rgba<u8>, c: Rgb, tolerance: u8) -> bool {
    [c.r, c.g, c.b]
        .into_iter()
        .zip(p.0)
        .all(|(want, got)| want.abs_diff(got) <= tolerance)
}

/// The sample card paints the highlighted theme: its background, its 16
/// palette colours as squares, and its sample text in the theme's own
/// colours. Highlighting another theme repaints all of it, text included,
/// though the footer's buffers are reused.
#[test]
fn the_sample_card_paints_the_highlighted_themes_colours() {
    let _serialized = crate::gpu_tests::gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(720, 560) else {
        eprintln!("theme sample check skipped: no GPU adapter");
        return;
    };
    let snap = snapshot_of(20, 4, b"");
    let row_h = menu::row_height(renderer.cell_h);
    let cell = (renderer.cell_w, renderer.cell_h);
    for name in ["Dracula", "Kettle Day"] {
        cfg.theme = Theme::by_name(name);
        let mut f = footer(name, ThemeFollow::System, Some(ThemeSlot::Dark));
        f.height = theme_footer_height(cell.1, true, 2);
        let picker = picker_with_footer(f.clone(), (640.0, row_h));
        let layout = picker
            .picker_footer_layout(&EN, cell)
            .expect("a footer layout");
        let overlay = Overlay {
            context_menu: Some(picker),
            ..focused(false)
        };
        let frame = capture(&mut renderer, &cfg, &[pane(&snap, 720, 560)], &overlay);
        if let Ok(dir) = std::env::var("KETTLE_THEME_FOOTER_PNG_DIR") {
            frame
                .save(std::path::Path::new(&dir).join(format!("{name}.png")))
                .expect("save");
        }
        let theme = &f.sample;
        let card = layout.card.expect("the card fits");
        let inside = frame.get_pixel((card.0 + 3.0) as u32, (card.1 + card.3 - 3.0) as u32);
        assert!(
            near(inside, theme.background, 2),
            "{name}: card fill {inside:?}"
        );
        assert_eq!(layout.squares.len(), 16, "{name}");
        for &((x, y, w, h), slot) in &layout.squares {
            let p = frame.get_pixel((x + w * 0.5) as u32, (y + h * 0.5) as u32);
            assert!(
                near(p, theme.palette[slot], 2),
                "{name}: square {slot} is {p:?}"
            );
        }
        // Each sample line shows its runs' palette colours: a run's glyphs
        // reach the ink near their stems.
        for line in layout.lines.iter().take(6) {
            let mut col = 0;
            for run in &line.runs {
                let cols = columns(&run.text);
                if let FooterInk::Sample(slot) = run.ink {
                    let x0 = line.left + col as f32 * cell.0;
                    let rect = (x0, line.top, cols as f32 * cell.0, cell.1);
                    let hits = (rect.0 as u32..(rect.0 + rect.2) as u32)
                        .flat_map(|x| {
                            (rect.1 as u32..(rect.1 + rect.3) as u32).map(move |y| (x, y))
                        })
                        .filter(|&(x, y)| near(frame.get_pixel(x, y), theme.palette[slot], 24))
                        .count();
                    assert!(hits > 0, "{name}: {:?} has no slot-{slot} ink", run.text);
                }
                col += cols;
            }
        }
    }
}

/// The contrast button's words change in place: "Contrast: Off" and
/// "Contrast: 3:1" are as wide, in the same buffer at the same spot, so only
/// the footer's own damage key makes the next frame show the new value.
#[test]
fn a_new_contrast_value_repaints_the_button_in_place() {
    let _serialized = crate::gpu_tests::gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(720, 560) else {
        eprintln!("contrast button check skipped: no GPU adapter");
        return;
    };
    cfg.theme = Theme::by_name("Dracula");
    let snap = snapshot_of(20, 4, b"");
    let row_h = menu::row_height(renderer.cell_h);
    let cell = (renderer.cell_w, renderer.cell_h);
    let mut buttons = Vec::new();
    for minimum in [0.0, 3.0] {
        let mut f = footer("Dracula", ThemeFollow::System, Some(ThemeSlot::Dark));
        f.minimum_contrast = minimum;
        f.height = theme_footer_height(cell.1, true, 2);
        let picker = picker_with_footer(f, (640.0, row_h));
        let (x, y, w, h) = picker
            .picker_footer_layout(&EN, cell)
            .and_then(|layout| layout.contrast_button)
            .expect("the button");
        let overlay = Overlay {
            context_menu: Some(picker),
            ..focused(false)
        };
        let frame = capture(&mut renderer, &cfg, &[pane(&snap, 720, 560)], &overlay);
        let pixels: Vec<[u8; 4]> = (y as u32 + 2..(y + h) as u32 - 2)
            .flat_map(|py| (x as u32 + 2..(x + w) as u32 - 2).map(move |px| (px, py)))
            .map(|(px, py)| frame.get_pixel(px, py).0)
            .collect();
        buttons.push(pixels);
    }
    assert!(buttons[0] != buttons[1], "the button still says Off");
}
