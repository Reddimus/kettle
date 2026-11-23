//! `present.rs` on the CPU: straight alpha for a translucent surface.
//!
//! Per the WGSL: each scene texel, sampled 1:1, becomes `0` where its alpha
//! is not positive, and otherwise `(rgb / a, a)`. The divide happens in
//! linear light, between the texture's sRGB decode and the attachment's
//! encode; results past 1 clamp on store.

use super::srgb::{SrgbTables, tables};
use super::{CpuFramebuffer, pack, unpack};

/// The straight-alpha pixel for one premultiplied scene pixel.
#[inline(always)]
fn unpremultiply(t: &SrgbTables, p: u32) -> u32 {
    let [r, g, b, a] = unpack(p);
    match a {
        0 => 0,
        // Dividing by one leaves the decoded value, which encodes back.
        255 => p,
        _ => {
            let a = t.unorm[usize::from(a)];
            let ch = |c: u8| t.encode(t.decf[usize::from(c)] / a);
            pack(ch(r), ch(g), ch(b), unpack(p)[3])
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

#[cfg(test)]
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
