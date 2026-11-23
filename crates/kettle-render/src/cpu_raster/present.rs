//! `present.rs` on the CPU: straight alpha for a translucent surface.
//!
//! Per the WGSL: each scene texel, sampled 1:1, becomes `0` where its alpha
//! is not positive, and otherwise `(rgb / a, a)`. The divide happens in
//! linear light, between the texture's sRGB decode and the attachment's
//! encode; results past 1 clamp before the encoded byte is rounded.
//! The texture decode uses lavapipe's sRGB sampling curve. At very low
//! alpha its small linear-light approximation error becomes visible after
//! division, so using the exact decode there would disagree with the GPU.

use super::srgb::{SrgbTables, tables};
use super::{CpuFramebuffer, pack, unpack};

// Mesa's Gallivm sRGB texture decode (lp_build_srgb_to_linear). Keep this
// separate from the exact sRGB tables used for CPU blending and storage.
const SCENE_DECODE: [f32; 256] = {
    let mut table = [0.0; 256];
    let mut code = 0;
    while code < table.len() {
        let x = code as f32;
        table[code] = if code <= 15 {
            x * (1.0 / (12.6 * 255.0))
        } else {
            ((0.3012 / (255.0 * 255.0 * 255.0) * x + 0.6935 / (255.0 * 255.0)) * x + 0.0030 / 255.0)
                * x
                + 0.0023
        };
        code += 1;
    }
    table
};

/// The straight-alpha pixel for one premultiplied scene pixel.
#[inline(always)]
fn unpremultiply(t: &SrgbTables, p: u32) -> u32 {
    let [r, g, b, a] = unpack(p);
    match a {
        0 => 0,
        // Dividing by one leaves the decoded value, which encodes back.
        255 => p,
        _ => {
            let alpha = t.unorm[usize::from(a)];
            let ch = |c: u8| t.encode(SCENE_DECODE[usize::from(c)] / alpha);
            pack(ch(r), ch(g), ch(b), a)
        }
    }
}

/// Write `scene`'s straight-alpha conversion into `out`, as
/// `PresentationPipeline::draw` does. `false`, writing nothing, when the
/// sizes differ.
pub(crate) fn present_straight(scene: &CpuFramebuffer, out: &mut CpuFramebuffer) -> bool {
    if (scene.width(), scene.height()) != (out.width(), out.height()) {
        return false;
    }
    let t = tables();
    for (o, &p) in out.pixels_mut().iter_mut().zip(scene.pixels()) {
        *o = unpremultiply(t, p);
    }
    true
}

/// [`present_straight`] in place.
pub(crate) fn present_straight_in_place(scene: &mut CpuFramebuffer) {
    let t = tables();
    for p in scene.pixels_mut() {
        *p = unpremultiply(t, *p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_alpha_doubles_linear_colour_and_zero_alpha_clears() {
        // 0.25 linear premultiplied by 0.5 alpha is 0.5 straight.
        let t = tables();
        let p = pack(t.encode(0.25), 0, t.encode(0.6), 128);
        let mut fb = CpuFramebuffer::from_rgba8(2, 1, &[0; 8]).unwrap();
        fb.pixels_mut()[0] = p;
        fb.pixels_mut()[1] = pack(9, 9, 9, 0);
        present_straight_in_place(&mut fb);
        let [r, g, b, a] = unpack(fb.pixels()[0]);
        assert_eq!((g, b, a), (0, 255, 128));
        assert!(r.abs_diff(t.encode(0.25 / (128.0 / 255.0))) <= 1, "{r}");
        assert_eq!(fb.pixels()[1], 0);
    }
}
