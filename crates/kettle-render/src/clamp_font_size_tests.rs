use super::clamp_font_size;

#[test]
fn clamp_font_size_bounds_match_set_font_size() {
    // Floor + ceiling pinned: 5.0 and 72.0. Renderer::new and
    // set_font_size share these bounds, so a `font-size = 200` config
    // cannot boot with 200pt cells (texture-limit risk).
    assert_eq!(clamp_font_size(13.0), 13.0, "in-range passes through");
    assert_eq!(clamp_font_size(72.0), 72.0, "at-ceiling stays");
    assert_eq!(clamp_font_size(5.0), 5.0, "at-floor stays");
    assert_eq!(clamp_font_size(200.0), 72.0, "above ceiling clamps");
    assert_eq!(clamp_font_size(3.0), 5.0, "below floor clamps");
    // Negative is a parse-corrupted value; clamp to floor not panic.
    assert_eq!(clamp_font_size(-1.0), 5.0);
    // NaN routes to floor (f32::clamp panics on NaN; sanitize first).
    assert_eq!(clamp_font_size(f32::NAN), 5.0);
    // Infinities round to the bounds.
    assert_eq!(clamp_font_size(f32::INFINITY), 72.0);
    assert_eq!(clamp_font_size(f32::NEG_INFINITY), 5.0);
}
