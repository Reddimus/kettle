//! Every render pipeline must pair its fragment shader's alpha convention with
//! a matching blend state.
//!
//! `quad` and `imgpipe` return PREMULTIPLIED color (`rgb * a`) while both were
//! configured with `ALPHA_BLENDING`, whose source factor is `SrcAlpha` — so the
//! GPU multiplied by alpha a second time and a 50%-opaque surface contributed
//! 25%. That darkened every translucent image, panel, highlight, separator, and
//! the unfocused-pane dim overlay. `glyphpipe` returns STRAIGHT alpha and is
//! correct with `ALPHA_BLENDING`; the bug was the mismatch, not either
//! convention.
//!
//! Checked as a PAIR per pipeline rather than as two independent token
//! searches: "a premultiply exists somewhere" and "a blend constant exists
//! somewhere" both stay true if one pipeline is fixed and another regresses,
//! which is precisely the failure this is meant to catch.

/// Does this file's WGSL fragment entry point return premultiplied color?
fn shader_premultiplies(src: &str) -> bool {
    let fs = src
        .split("fn fs(")
        .nth(1)
        .expect("every pipeline has a fragment entry point");
    let body = fs.split("\n}").next().unwrap_or(fs);
    // `lin * in.color.a` (quad) or `c.rgb * c.a` (imgpipe).
    body.contains("* in.color.a") || body.contains("* c.a")
}

/// The blend constant this file configures.
fn blend_state(src: &str) -> &'static str {
    let premultiplied = src.contains("BlendState::PREMULTIPLIED_ALPHA_BLENDING");
    let straight = src.contains("BlendState::ALPHA_BLENDING")
        && !src.contains("BlendState::PREMULTIPLIED_ALPHA_BLENDING");
    match (premultiplied, straight) {
        (true, false) => "premultiplied",
        (false, true) => "straight",
        _ => panic!("expected exactly one blend state in this pipeline"),
    }
}

#[test]
fn each_pipeline_blends_the_way_its_shader_writes() {
    for (name, src) in [
        ("quad", include_str!("../src/quad.rs")),
        ("imgpipe", include_str!("../src/imgpipe.rs")),
        ("glyphpipe", include_str!("../src/glyphpipe.rs")),
    ] {
        let premultiplies = shader_premultiplies(src);
        let blend = blend_state(src);
        let want = if premultiplies {
            "premultiplied"
        } else {
            "straight"
        };
        assert_eq!(
            blend,
            want,
            "{name}: the shader returns {} color, so the blend state must be \
             {want} — mismatching them applies alpha twice (or not at all)",
            if premultiplies {
                "premultiplied"
            } else {
                "straight"
            }
        );
    }
}

/// The helpers above must actually be able to tell the two conventions apart —
/// otherwise the test above passes by answering the same way for everything.
#[test]
fn the_convention_detector_distinguishes_the_two_shapes() {
    assert!(shader_premultiplies(
        "fn fs(in: VsOut) -> f32 {\n    return vec4(lin * in.color.a, in.color.a);\n}"
    ));
    assert!(!shader_premultiplies(
        "fn fs(in: VsOut) -> f32 {\n    return textureSample(t, s, in.uv);\n}"
    ));
    assert_eq!(
        blend_state("blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),"),
        "premultiplied"
    );
    assert_eq!(
        blend_state("blend: Some(wgpu::BlendState::ALPHA_BLENDING),"),
        "straight"
    );
}
