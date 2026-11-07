use super::production_source;

#[test]
fn unchanged_tooltip_reshapes_in_the_reloaded_font_family() {
    use super::*;

    let _serialized = gpu_tests::gpu_test_guard();
    let mut cfg = gpu_tests::gpu_test_config();
    cfg.font_family = kettle_config::font::FAMILY.into();
    let Some(mut renderer) =
        Renderer::headless_for_tests(&cfg, 480, 160).expect("headless renderer builds")
    else {
        eprintln!("TOOLTIP_FONT_RELOAD_GPU_SKIP: no GPU adapter");
        return;
    };
    eprintln!(
        "TOOLTIP_FONT_RELOAD_ADAPTER: {:?}",
        renderer.gpu.adapter.get_info()
    );
    // An isolated database makes both the requested and resolved faces
    // independent of fonts installed on the host.
    renderer.font_system =
        FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    load_bundled_font(&mut renderer.font_system, kettle_config::font::REGULAR);
    load_bundled_font(
        &mut renderer.font_system,
        include_bytes!("../../../assets/fonts/fira-sans/FiraSans-Regular.ttf"),
    );
    let label = "New tab (Ctrl+Shift+T)";
    let tabs = TabBar {
        tooltip: Some(label.into()),
        height: 30.0,
        band: (0.0, 0.0, 480.0, 30.0),
        ..TabBar::hidden()
    };
    let overlay = Overlay::default();
    // Also reload while the tooltip is closed, then reopen the same label.
    for (family, closed) in [
        (kettle_config::font::FAMILY, false),
        ("Fira Sans", false),
        (kettle_config::font::FAMILY, true),
    ] {
        cfg.font_family = family.into();
        renderer.set_font_family(family.into());
        if closed {
            let _ = renderer
                .render_frame(&[], &TabBar::hidden(), &cfg, &overlay)
                .expect("closed tooltip frame");
        }
        let _ = renderer
            .render_frame(&[], &tabs, &cfg, &overlay)
            .expect("tooltip frame");
        assert_eq!(renderer.tab_tooltip_text, label);
        assert_eq!(renderer.tab_tooltip_buffer.lines[0].text(), label);
        assert_eq!(
            renderer.tab_tooltip_buffer.lines[0]
                .attrs_list()
                .get_span(0)
                .family,
            Family::Name(family),
            "unchanged tooltip must acquire the reloaded family"
        );
        let glyphs: Vec<_> = renderer
            .tab_tooltip_buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().cloned())
            .collect();
        assert!(!glyphs.is_empty(), "tooltip must actually shape");
        let ink = renderer
            .swash
            .get_image(
                &mut renderer.font_system,
                glyphs[0].physical((0.0, 0.0), 1.0).cache_key,
            )
            .as_ref()
            .expect("the resolved tooltip face rasterizes");
        assert!(ink.placement.width > 0 && ink.placement.height > 0);
        for glyph in glyphs {
            assert_ne!(glyph.glyph_id, 0, "fixture must provide every glyph");
            let face = renderer.font_system.db().face(glyph.font_id).unwrap();
            assert!(
                face.families.iter().any(|(name, _)| name == family),
                "tooltip resolved to {:?}, expected {family}",
                face.families
            );
        }
    }
    eprintln!("TOOLTIP_FONT_RELOAD_PASS: unchanged label, bundled faces, open and closed reloads");
}

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
    let pass: String = pass.split_whitespace().collect();
    let at = |needle: &str| {
        let normalized: String = needle.split_whitespace().collect();
        pass.find(&normalized).unwrap_or_else(|| panic!("{needle}"))
    };
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

#[test]
fn a_font_reload_invalidates_both_overflow_arrow_labels() {
    let source = production_source();
    let reload = source
        .split("if self.chrome_style_key != k {")
        .nth(1)
        .unwrap()
        .split("// Ensure one text buffer per pane.")
        .next()
        .unwrap();
    assert!(
        reload.contains("self.scroll_left_text.clear();")
            && reload.contains("self.scroll_right_text.clear();"),
        "overflow arrows retain their old font attributes"
    );
}
