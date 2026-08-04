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
//! This checks the pair per pipeline rather than searching for either token
//! independently — "a premultiply exists somewhere" and "a blend constant
//! exists somewhere" both stay true if one pipeline is fixed and another
//! regresses.
//!
//! It is a source-level check and cannot see rendered pixels — the authority
//! on what the hardware actually does is
//! `gpu_tests::a_half_opaque_quad_blends_at_half_not_a_quarter`, which renders
//! a translucent quad and reads the pixel back. This covers the pipelines that
//! test cannot cheaply stand up, and it is a cross-check rather than the last
//! line of defence.
//!
//! Two things stop it passing vacuously: the shader body is parsed for the
//! exact number of alpha multiplications (so a double-multiply `rgb * a * a` is
//! caught, not just any `* a`) — resolving local `let` bindings first, so
//! multiplying through an alias is still counted — and the detectors are
//! themselves tested against canned strings of every shape they must tell
//! apart.

/// How many times does this pipeline's fragment entry point multiply its color
/// by alpha? Premultiplied output is exactly one; straight is zero; anything
/// else is a bug in its own right.
///
/// Alpha reaches the multiply under several names: `in.color.a` (quad), `c.a`
/// (imgpipe), or any local bound to one of those. Counting only the two literal
/// spellings meant `let a2 = in.color.a; return vec4(lin * in.color.a * a2, ..)`
/// read as a single multiply while premultiplying twice.
fn alpha_multiplications(src: &str) -> usize {
    let fs = src
        .split("fn fs(")
        .nth(1)
        .expect("every pipeline has a fragment entry point");
    let body = fs.split("\n}").next().unwrap_or(fs);

    // Past the parameter list and opening brace, so the signature is not glued
    // to the first statement.
    let body = body.split_once('{').map_or(body, |(_, rest)| rest);

    // Names that hold alpha. Seeded with the two direct spellings, then grown
    // by following `let NAME = <known alpha expression>;` to a fixed point so a
    // chain of aliases resolves too.
    let mut names = vec!["in.color.a".to_string(), "c.a".to_string()];
    loop {
        let grew = body
            .split(';')
            .filter_map(|statement| alias_of(statement, &names))
            .find(|name| !names.contains(name));
        match grew {
            Some(name) => names.push(name),
            None => break,
        }
    }

    // Count multiplications by any of those names, excluding the alias
    // definitions themselves — `let a2 = in.color.a` is not a multiply.
    body.split(';')
        .filter(|statement| alias_of(statement, &names).is_none())
        .map(|statement| {
            names
                .iter()
                .map(|alpha| statement.matches(&format!("* {alpha}")).count())
                .sum::<usize>()
        })
        .sum()
}

/// If `statement` is `let NAME [: TYPE] = <one of `alphas`>`, the name it binds.
///
/// Matched from the LAST `let` in the statement, because splitting a shader on
/// `;` leaves whatever preceded the binding — a closing brace, the function
/// signature — attached to its front.
fn alias_of(statement: &str, alphas: &[String]) -> Option<String> {
    let (binding, value) = statement.split_once('=')?;
    if !alphas.iter().any(|alpha| value.trim() == alpha) {
        return None;
    }
    let start = binding.rfind("let ")? + "let ".len();
    // `let x: f32 = ...` as well as `let x = ...`.
    let name = binding[start..]
        .split(':')
        .next()
        .unwrap_or(&binding[start..])
        .trim();
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_string())
}

/// The blend constant this pipeline configures.
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
        let multiplies = alpha_multiplications(src);
        assert!(
            multiplies <= 1,
            "{name}: the shader multiplies by alpha {multiplies} times — more \
             than once is a double-premultiply no blend state can undo"
        );
        let want = if multiplies == 1 {
            "premultiplied"
        } else {
            "straight"
        };
        assert_eq!(
            blend_state(src),
            want,
            "{name}: the shader returns {want} color, so the blend state must \
             match — mismatching them applies alpha twice, or not at all"
        );
    }
}

/// The detectors must be able to tell the shapes apart, including the
/// double-multiply the main test relies on them catching. Without this, a
/// detector that answered the same way for everything would make the test
/// above vacuous.
#[test]
fn the_convention_detectors_distinguish_the_shapes_they_must() {
    let straight = "fn fs(in: VsOut) -> f32 {\n    return textureSample(t, s, in.uv);\n}";
    let premul = "fn fs(in: VsOut) -> f32 {\n    return vec4(lin * in.color.a, in.color.a);\n}";
    let double = "fn fs(in: VsOut) -> f32 {\n    return vec4(c.rgb * c.a * c.a, c.a);\n}";
    // The evasion this detector previously missed: one recognised multiply,
    // plus a second one through a local alias.
    let aliased = "fn fs(in: VsOut) -> f32 {\n    let extra = in.color.a;\n    \
                   return vec4(lin * in.color.a * extra, in.color.a);\n}";
    // And through a chain of them.
    let chained = "fn fs(in: VsOut) -> f32 {\n    let a1 = c.a;\n    let a2 = a1;\n    \
                   return vec4(c.rgb * a2, c.a);\n}";
    // An alias that is merely BOUND, never multiplied through, is not a
    // multiply — otherwise the detector would cry wolf on ordinary shaders.
    let bound_only = "fn fs(in: VsOut) -> f32 {\n    let alpha = in.color.a;\n    \
                      return vec4(lin, alpha);\n}";

    assert_eq!(alpha_multiplications(straight), 0, "straight alpha");
    assert_eq!(alpha_multiplications(premul), 1, "premultiplied");
    assert_eq!(
        alpha_multiplications(double),
        2,
        "a double-multiply must be visible as two, not collapsed to one"
    );
    assert_eq!(
        alpha_multiplications(aliased),
        2,
        "multiplying through a local alias is still multiplying by alpha"
    );
    assert_eq!(
        alpha_multiplications(chained),
        1,
        "a chain of aliases resolves to the alpha it came from"
    );
    assert_eq!(
        alpha_multiplications(bound_only),
        0,
        "binding alpha to a name is not multiplying by it"
    );

    assert_eq!(
        blend_state("blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),"),
        "premultiplied"
    );
    assert_eq!(
        blend_state("blend: Some(wgpu::BlendState::ALPHA_BLENDING),"),
        "straight"
    );
}
