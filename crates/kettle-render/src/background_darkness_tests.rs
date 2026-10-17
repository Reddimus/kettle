use super::{
    QuadInstance, apply_quad_alpha_floor, composed_bg_alpha, desired_alpha_mode,
    final_scene_is_uniformly_opaque, live_underlay_clear_color, needs_postmultiplied_presentation,
    use_live_pane_bases, window_requires_alpha_surface,
};
use kettle_config::{BackgroundType, Config};

#[test]
fn unsupported_blur_floor_applies_only_to_the_live_window_underlay() {
    let background = kettle_config::Rgb::new(26, 27, 38);
    let live = live_underlay_clear_color(
        background,
        Some(0.99),
        wgpu::CompositeAlphaMode::PreMultiplied,
        true,
    );
    assert!((live.a - 0.99).abs() < 1e-6);
    assert!(live.r > 0.0 && live.g > 0.0 && live.b > 0.0);

    for offscreen in [
        live_underlay_clear_color(
            background,
            Some(0.99),
            wgpu::CompositeAlphaMode::PreMultiplied,
            false,
        ),
        live_underlay_clear_color(
            background,
            None,
            wgpu::CompositeAlphaMode::PreMultiplied,
            true,
        ),
    ] {
        assert_eq!(offscreen.a, 0.0);
        assert_eq!(offscreen.r, 0.0);
        assert_eq!(offscreen.g, 0.0);
        assert_eq!(offscreen.b, 0.0);
    }
}

#[test]
fn unsupported_blur_floor_survives_the_replacing_pane_base_pass() {
    let mut bases = [QuadInstance {
        pos: [0.0, 0.0],
        size: [100.0, 100.0],
        color: [0.1, 0.2, 0.3, 0.55],
    }];
    apply_quad_alpha_floor(&mut bases, 0.99);
    assert!((bases[0].color[3] - 0.99).abs() < f32::EPSILON);

    bases[0].color[3] = 1.0;
    apply_quad_alpha_floor(&mut bases, 0.99);
    assert_eq!(bases[0].color[3], 1.0, "the floor must never reduce alpha");

    assert!(use_live_pane_bases(true, Some(0.99)));
    assert!(!use_live_pane_bases(false, Some(0.99)));
    assert!(!use_live_pane_bases(true, None));
}

/// `background-darkness` runs see-through → covered, and the docs must say
/// so.
///
/// Terminator assigns this value straight to the background colour's alpha,
/// and its users lower it to get MORE transparency. If `docs/CONFIG.md` or
/// the field's own doc comment states the scale backwards ("1.0 = no tint,
/// 0.0 = fully dark"), users configuring kettle reach for the wrong end.
#[test]
fn darkness_scales_the_backdrop_toward_see_through() {
    let with = |background_type, darkness, opacity| {
        composed_bg_alpha(&Config {
            background_type,
            background_darkness: darkness,
            background_opacity: opacity,
            ..Config::default()
        })
    };

    for background_type in [
        BackgroundType::Transparent,
        BackgroundType::Image,
        BackgroundType::Starfield,
    ] {
        // 0.0 paints nothing over the backdrop: fully see-through.
        assert_eq!(with(background_type, 0.0, 1.0), 0.0, "{background_type:?}");
        // 1.0 paints the terminal background at full opacity: covered.
        assert_eq!(with(background_type, 1.0, 1.0), 1.0, "{background_type:?}");
        // And it is monotonic in between, not inverted or clamped flat.
        let quarter = with(background_type, 0.25, 1.0);
        let half = with(background_type, 0.5, 1.0);
        assert!(
            0.0 < quarter && quarter < half && half < 1.0,
            "{background_type:?}: darkness must increase coverage, got \
                 0.25 -> {quarter}, 0.5 -> {half}"
        );
        // It composes with background-opacity rather than replacing it.
        assert!(
            (with(background_type, 0.5, 0.5) - 0.25).abs() < 1e-9,
            "{background_type:?}: darkness and opacity must multiply"
        );
    }

    // A solid background ignores darkness entirely — there is no backdrop
    // for it to reveal. (Compared with a tolerance: the config stores these
    // as `f32` and the alpha is `f64`, so 0.8 does not round-trip exactly.)
    for darkness in [0.0, 0.5, 1.0] {
        assert!(
            (with(BackgroundType::Solid, darkness, 0.8) - 0.8).abs() < 1e-6,
            "a solid background must not consult darkness"
        );
    }
}

#[test]
fn effective_alpha_mode_tracks_darkness_and_live_reload() {
    let macos_modes = [
        wgpu::CompositeAlphaMode::Opaque,
        wgpu::CompositeAlphaMode::PostMultiplied,
    ];
    let opaque = Config {
        background_type: BackgroundType::Transparent,
        background_opacity: 1.0,
        background_darkness: 1.0,
        ..Config::default()
    };
    assert_eq!(
        desired_alpha_mode(&opaque, &macos_modes),
        wgpu::CompositeAlphaMode::Opaque
    );

    let transparent = Config {
        background_darkness: 0.5,
        ..opaque.clone()
    };
    assert_eq!(
        desired_alpha_mode(&transparent, &macos_modes),
        wgpu::CompositeAlphaMode::PostMultiplied,
        "effective opacity, not background-opacity alone, must select the surface mode"
    );
    assert!(
        window_requires_alpha_surface(&transparent),
        "transparent backgrounds must create an alpha-capable OS window"
    );

    let image = Config {
        background_type: BackgroundType::Image,
        background_opacity: 1.0,
        background_darkness: 1.0,
        ..Config::default()
    };
    assert!(
        window_requires_alpha_surface(&image),
        "wallpaper alpha is not known until decode, so image windows must be alpha-capable"
    );

    let starfield = Config {
        background_type: BackgroundType::Starfield,
        background_opacity: 1.0,
        background_darkness: 0.5,
        ..Config::default()
    };
    assert!(
        !window_requires_alpha_surface(&starfield),
        "the starfield shader covers every surface pixel with alpha 1"
    );

    let reloaded = Config {
        background_type: BackgroundType::Solid,
        background_opacity: 0.5,
        ..opaque
    };
    assert_eq!(
        desired_alpha_mode(&reloaded, &macos_modes),
        wgpu::CompositeAlphaMode::PostMultiplied,
        "an opaque-to-transparent reload must change the configured surface mode"
    );

    let src = super::production_source();
    let refresh = ["self.set_background_", "compositing(cfg);"].concat();
    assert!(
        src.contains(&refresh),
        "frame ingress must apply a changed effective-alpha mode before acquisition"
    );
}

#[test]
fn postmultiplied_presentation_skips_only_provably_opaque_scenes() {
    let image = Config {
        background_type: BackgroundType::Image,
        background_opacity: 1.0,
        background_darkness: 1.0,
        ..Config::default()
    };
    let post = wgpu::CompositeAlphaMode::PostMultiplied;

    let opaque_image = final_scene_is_uniformly_opaque(&image, true);
    assert!(
        opaque_image,
        "an opaque fullscreen wallpaper proves alpha 1"
    );
    assert!(
        !needs_postmultiplied_presentation(post, opaque_image),
        "opaque wallpaper must skip the fullscreen allocation and conversion pass"
    );

    for reason in [
        "transparent source texel",
        "wallpaper does not cover the surface",
    ] {
        let possibly_translucent = final_scene_is_uniformly_opaque(&image, false);
        assert!(
            !possibly_translucent,
            "fixture must be non-opaque: {reason}"
        );
        assert!(
            needs_postmultiplied_presentation(post, possibly_translucent),
            "{reason}: a premultiplied scene must still be converted before a PostMultiplied surface"
        );
    }

    let translucent_solid = Config {
        background_type: BackgroundType::Solid,
        background_opacity: 0.5,
        ..Config::default()
    };
    assert!(needs_postmultiplied_presentation(
        post,
        final_scene_is_uniformly_opaque(&translucent_solid, false)
    ));
    assert!(
        !needs_postmultiplied_presentation(wgpu::CompositeAlphaMode::PreMultiplied, false),
        "a PreMultiplied surface consumes the scene directly even when translucent"
    );

    let starfield = Config {
        background_type: BackgroundType::Starfield,
        background_opacity: 0.2,
        background_darkness: 0.0,
        ..Config::default()
    };
    assert!(
        final_scene_is_uniformly_opaque(&starfield, false),
        "starfield opacity is a shader invariant, not an image-cover result"
    );
    assert!(!needs_postmultiplied_presentation(
        post,
        final_scene_is_uniformly_opaque(&starfield, false)
    ));
}
