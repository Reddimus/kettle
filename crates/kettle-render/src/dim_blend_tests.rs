use super::dim_blend;
use crate::color::contrast_ratio;
use kettle_config::Rgb;

#[test]
fn dimmed_text_keeps_45_percent_of_the_foreground() {
    let white = Rgb::new(255, 255, 255);
    let black = Rgb::new(0, 0, 0);
    assert_eq!(dim_blend(white, black), Rgb::new(115, 115, 115));
}

/// Disabled rows and shortcut hints must stay readable. On TokyoNight
/// Night the old blend kept a sixth of the foreground: about 1.6:1.
#[test]
fn dimmed_text_stays_readable_on_a_dark_theme() {
    let fg = Rgb::parse("#c0caf5").unwrap();
    let bg = Rgb::parse("#1a1b26").unwrap();
    let ratio = contrast_ratio(dim_blend(fg, bg), bg);
    assert!(ratio >= 3.0, "{ratio:.2}:1");
    assert!(
        ratio < contrast_ratio(fg, bg),
        "still dimmer than normal text"
    );
}

#[test]
fn inactive_pane_text_keeps_a_text_contrast_floor() {
    let theme = kettle_config::Theme::by_name("Builtin Light");
    let dimmed = crate::chrome::Colors::default().inactive(
        theme.foreground,
        theme.background,
        theme.background,
        0.55,
        true,
    );
    let ratio = contrast_ratio(dimmed, theme.background);
    assert!(ratio >= 4.5, "inactive pane: {ratio:.3}:1 < 4.5");
}
