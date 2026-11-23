//! sRGB transfer tables for the CPU raster.
//!
//! The GPU scene target is an `*Srgb` format: blending runs in linear light
//! and the attachment stores sRGB-encoded RGB with linear alpha. The CPU
//! framebuffer stores the same bytes, so every blend decodes the destination,
//! blends and re-encodes, as the GPU's blend unit does.
//!
//! Two precisions are kept:
//!
//! - 16-bit linear (`0..=65535`) for the hot paths (flat quads and mask
//!   glyphs), where the blend is integer math.
//! - `f32` linear for shaded paths (images, SDF shapes, starfield, colour
//!   glyphs), which compute their colour in floating point as the shaders do.
//!
//! Both encoders are correctly rounded for their input: a 4096-entry table
//! gives the code at the start of each bin, and one comparison against the
//! exact rounding threshold of the next code fixes the rest. Each bin spans
//! less than one output code (asserted in the tests), so one step suffices.
//! Decoding then encoding any byte returns the byte.

use std::sync::OnceLock;

/// The sRGB transfer tables. Fetch once per draw with [`tables`].
pub(crate) struct SrgbTables {
    /// Byte to 16-bit linear, rounded.
    pub(crate) dec16: [u16; 256],
    /// Byte to `f32` linear, the decode an `*Srgb` texture fetch returns.
    pub(crate) decf: [f32; 256],
    /// Byte to `f32` without decoding, the value a `*Unorm` fetch returns.
    pub(crate) unorm: [f32; 256],
    /// The code at the start of each 16-unit bin of 16-bit linear.
    enc16: [u8; 4096],
    /// The smallest 16-bit linear value that rounds to code `v + 1`.
    thr16: [u32; 256],
    /// The code at the start of each 1/4096 bin of `f32` linear.
    encf: [u8; 4096],
    /// The smallest `f32` linear value that rounds to code `v + 1`.
    thrf: [f32; 256],
}

/// The exact sRGB decode, in `f64`.
fn decode_f64(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

impl SrgbTables {
    fn build() -> Self {
        let mut dec16 = [0u16; 256];
        let mut decf = [0f32; 256];
        let mut unorm = [0f32; 256];
        let mut thr16 = [u32::MAX; 256];
        let mut thrf = [f32::INFINITY; 256];
        for v in 0..256 {
            let lin = decode_f64(v as f64 / 255.0);
            dec16[v] = (lin * 65535.0).round() as u16;
            decf[v] = lin as f32;
            unorm[v] = v as f32 / 255.0;
            if v < 255 {
                // Encoding rounds up to `v + 1` from this linear value on.
                let t = decode_f64((v as f64 + 0.5) / 255.0);
                thr16[v] = (t * 65535.0).ceil() as u32;
                let mut t32 = t as f32;
                if f64::from(t32) < t {
                    t32 = t32.next_up();
                }
                thrf[v] = t32;
            }
        }
        let mut enc16 = [0u8; 4096];
        let mut encf = [0u8; 4096];
        for i in 0..4096 {
            let lo16 = (i * 16) as u32;
            enc16[i] = thr16[..255].iter().filter(|&&t| t <= lo16).count() as u8;
            let lof = i as f32 / 4096.0;
            encf[i] = thrf[..255].iter().filter(|&&t| t <= lof).count() as u8;
        }
        Self {
            dec16,
            decf,
            unorm,
            enc16,
            thr16,
            encf,
            thrf,
        }
    }

    /// 16-bit linear to an sRGB byte, correctly rounded.
    #[inline(always)]
    pub(crate) fn encode16(&self, lin: u16) -> u8 {
        let v = self.enc16[usize::from(lin >> 4)];
        v + u8::from(u32::from(lin) >= self.thr16[usize::from(v)])
    }

    /// `f32` linear to an sRGB byte, clamped to `0..=1` and correctly rounded.
    /// NaN encodes as 0, as a float-to-unorm store does.
    #[inline(always)]
    pub(crate) fn encode(&self, lin: f32) -> u8 {
        // A NaN stays NaN here, and casts to bin 0 and fails the threshold.
        let lin = lin.clamp(0.0, 1.0);
        // Multiplying by a power of two is exact, so the bin is exact too.
        let i = ((lin * 4096.0) as usize).min(4095);
        let v = self.encf[i];
        v + u8::from(lin >= self.thrf[usize::from(v)])
    }
}

/// The process-wide tables, built on first use.
pub(crate) fn tables() -> &'static SrgbTables {
    static TABLES: OnceLock<SrgbTables> = OnceLock::new();
    TABLES.get_or_init(SrgbTables::build)
}

/// The shaders' `srgb_to_linear`, per component, in `f32`.
#[inline]
pub(crate) fn srgb_to_linear(c: f32) -> f32 {
    if c > 0.04045 {
        ((c + 0.055) / 1.055).powf(2.4)
    } else {
        c / 12.92
    }
}

/// A float stored to a unorm8 channel: clamped, rounded to nearest; NaN is 0.
#[inline(always)]
pub(crate) fn unorm8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_encode(lin: f64) -> u8 {
        let lin = lin.clamp(0.0, 1.0);
        let s = if lin <= 0.003_130_8 {
            lin * 12.92
        } else {
            1.055 * lin.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0 + 0.5).floor() as u8
    }

    #[test]
    fn every_byte_survives_a_decode_and_encode() {
        let t = tables();
        for b in 0..=255u8 {
            assert_eq!(t.encode16(t.dec16[usize::from(b)]), b, "16-bit {b}");
            assert_eq!(t.encode(t.decf[usize::from(b)]), b, "f32 {b}");
        }
    }

    #[test]
    fn each_encode_bin_spans_less_than_one_code() {
        let t = tables();
        for v in 0..254 {
            assert!(t.thr16[v + 1] - t.thr16[v] > 16, "16-bit thresholds {v}");
            assert!(
                t.thrf[v + 1] - t.thrf[v] > 1.0 / 4096.0,
                "f32 thresholds {v}"
            );
        }
    }

    #[test]
    fn encoders_round_like_the_exact_transfer() {
        let t = tables();
        for lin in 0..=65535u16 {
            let exact = exact_encode(f64::from(lin) / 65535.0);
            assert_eq!(t.encode16(lin), exact, "16-bit {lin}");
        }
        for i in 0..=200_000u32 {
            let lin = i as f32 / 200_000.0;
            assert_eq!(t.encode(lin), exact_encode(f64::from(lin)), "f32 {lin}");
        }
        assert_eq!(t.encode(f32::NAN), 0);
        assert_eq!(t.encode(-1.0), 0);
        assert_eq!(t.encode(7.0), 255);
    }

    #[test]
    fn unorm_stores_round_to_nearest() {
        assert_eq!(unorm8(0.5), 128);
        assert_eq!(unorm8(127.4 / 255.0), 127);
        assert_eq!(unorm8(f32::NAN), 0);
        assert_eq!(unorm8(2.0), 255);
        assert_eq!(unorm8(-2.0), 0);
    }
}
