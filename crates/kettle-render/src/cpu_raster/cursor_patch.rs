//! `cursor_patch.rs` on the CPU: the blink patch's combine pass.
//!
//! Per the WGSL: where the cursor-on and cursor-off frames hold the same
//! texel the patch is transparent black, and elsewhere it is the on frame's
//! RGB at alpha 1. Both frames are scene-format pixels, so equal decoded
//! values are equal bytes and the re-encoded RGB is the on frame's bytes.
//!
//! The two frames themselves are the scene drawn into a patch-sized
//! framebuffer through [`super::CpuFramebuffer::window_target`].

use super::CpuFramebuffer;

/// Write the patch of `on` over `off` into `out`. `false`, writing nothing,
/// when the three sizes differ.
pub(crate) fn combine_patch(
    on: &CpuFramebuffer,
    off: &CpuFramebuffer,
    out: &mut CpuFramebuffer,
) -> bool {
    let size = (on.width(), on.height());
    if (off.width(), off.height()) != size || (out.width(), out.height()) != size {
        return false;
    }
    for ((o, &a), &b) in out
        .pixels_mut()
        .iter_mut()
        .zip(on.pixels())
        .zip(off.pixels())
    {
        *o = if a == b { 0 } else { a | 0xff00_0000 };
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_pixels_are_opaque_and_the_rest_transparent() {
        let on = CpuFramebuffer::from_rgba8(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let off = CpuFramebuffer::from_rgba8(2, 1, &[1, 2, 3, 4, 5, 6, 7, 9]).unwrap();
        let mut out = CpuFramebuffer::new(2, 1).unwrap();
        assert!(combine_patch(&on, &off, &mut out));
        assert_eq!(out.to_rgba8(), [0, 0, 0, 0, 5, 6, 7, 255]);
        let mut small = CpuFramebuffer::new(1, 1).unwrap();
        assert!(!combine_patch(&on, &off, &mut small));
    }
}
