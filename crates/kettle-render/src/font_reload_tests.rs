use super::production_source;

/// A font-family reload must invalidate the quick-select hint cache too:
/// the same labels would otherwise keep glyphs shaped in the old family.
#[test]
fn a_font_reload_resets_the_hint_label_cache() {
    let production = production_source();
    let reload = production
        .split_once("self.resize_overlay_text.clear();")
        .expect("font reload invalidation")
        .1
        .split_once("// Ensure one text buffer per pane.")
        .expect("end of font reload invalidation")
        .0;
    assert!(reload.contains("for label in &mut self.hint_texts"));
}

/// Preview lanes draw above the terminal's text and cursor and below its
/// dimming, scrollbar and every overlay, so a menu or dialog covers a
/// lane's text as well as its panel.
#[test]
fn preview_lanes_draw_between_the_terminal_and_the_overlays() {
    let production = production_source();
    let pass = production
        .split_once("fn encode_scene_pass(")
        .expect("encode_scene_pass")
        .1;
    let at = |needle: &str| pass.find(needle).unwrap_or_else(|| panic!("{needle}"));
    let terminal_text = at("self.text_renderer\n            .render(");
    let cursor = at("self.cursor_glyph_renderer\n                .render(");
    let lane_quads = at("self.lane_quads.draw(&mut pass);");
    let lane_images = at("lanes.draw(&mut pass);");
    let lane_text = at("self.lane_text_renderer\n            .render(");
    let overlay = at("self.overlay_quads.draw(&mut pass);");
    let menus = at("self.menu_quads.draw(&mut pass);");
    assert!(terminal_text < lane_quads && cursor < lane_quads);
    assert!(lane_quads < lane_images && lane_images < lane_text);
    assert!(lane_text < overlay && overlay < menus);
}

/// A lane keeps its lines shaped while their text is unchanged, so a
/// reload that keeps the text must reshape them in the new family.
#[test]
fn a_font_reload_reshapes_the_preview_lanes() {
    let production = production_source();
    let reload = production
        .split_once("if self.chrome_style_key != k {")
        .expect("font reload invalidation")
        .1
        .split_once("// Ensure one text buffer per pane.")
        .expect("end of font reload invalidation")
        .0;
    let reload: String = reload.split_whitespace().collect();
    assert!(
        reload.contains("self.lane_texts.values_mut().for_each(media_lane::LaneText::invalidate);")
    );
}
