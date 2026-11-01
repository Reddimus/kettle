// SPDX-License-Identifier: Apache-2.0

//! X colour-space syntax, converted with a fixed D65/sRGB profile.

use super::Rgb;

const WHITE_X: f64 = 0.9504559270516716;
const WHITE_Z: f64 = 1.0890577507598784;
const KAPPA: f64 = 24389.0 / 27.0;

pub(super) fn parse(input: &[u8]) -> Option<Rgb> {
    let (kind, body) = core::str::from_utf8(input).ok()?.split_once(':')?;
    if !matches!(kind, "rgbi" | "CIEXYZ" | "CIExyY" | "CIEuvY" | "CIELab" | "CIELuv" | "TekHVC") {
        return None;
    }
    let mut parts = body.split('/');
    let mut values = [0.0; 3];
    for value in &mut values {
        let text = parts.next()?;
        if text.len() > 48
            || text.bytes().any(|b| !matches!(b, b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E'))
        {
            return None;
        }
        *value = text.parse::<f64>().ok()?;
        if !value.is_finite() || value.abs() > 1e6 {
            return None;
        }
    }
    if parts.next().is_some() {
        return None;
    }
    let [a, b, c] = values;
    let xyz = match kind {
        "rgbi" if values.iter().all(|v| (0.0..=1.0).contains(v)) => return encode(values),
        "CIEXYZ" if values.iter().all(|v| *v >= 0.0) => values,
        "CIExyY"
            if (0.0..=1.0).contains(&a)
                && (0.0..=1.0).contains(&b)
                && a + b <= 1.0
                && (0.0..=1.0).contains(&c) =>
        {
            if c == 0.0 {
                [0.0; 3]
            } else if b == 0.0 {
                return None;
            } else {
                [a * c / b, c, (1.0 - a - b) * c / b]
            }
        },
        "CIEuvY" if a >= 0.0 && b >= 0.0 && (0.0..=1.0).contains(&c) => uv_to_xyz(a, b, c)?,
        "CIELab" if (0.0..=100.0).contains(&a) => {
            let fy = (a + 16.0) / 116.0;
            // Xcms uses chromatic components normalized by 100. Use the
            // continuous CIE inverse, including below L*=8.
            [WHITE_X * lab_inverse(fy + b / 5.0), luminance(a), WHITE_Z * lab_inverse(fy - c / 2.0)]
        },
        "CIELuv" if (0.0..=100.0).contains(&a) => {
            if a == 0.0 {
                [0.0; 3]
            } else {
                let (uw, vw) = white_uv();
                uv_to_xyz(uw + b / (0.13 * a), vw + c / (0.13 * a), luminance(a))?
            }
        },
        "TekHVC" if (0.0..=100.0).contains(&b) && c >= 0.0 => {
            if b == 0.0 {
                [0.0; 3]
            } else if b == 100.0 {
                [WHITE_X, 1.0, WHITE_Z]
            } else {
                let (uw, vw) = white_uv();
                // TekHVC's red reference and chroma scale define its hue plane.
                let theta = a.rem_euclid(360.0).to_radians() + (0.4931 - vw).atan2(0.7127 - uw);
                let radius = c / (b * 7.50725);
                uv_to_xyz(uw + theta.cos() * radius, vw + theta.sin() * radius, luminance(b))?
            }
        },
        _ => return None,
    };
    let [x, y, z] = xyz;
    encode([
        3.2409699419045226 * x - 1.537383177570094 * y - 0.4986107602930034 * z,
        -0.9692436362808796 * x + 1.8759675015077202 * y + 0.0415550574071756 * z,
        0.0556300796969937 * x - 0.2039769588889765 * y + 1.0569715142428786 * z,
    ])
}

fn lab_inverse(value: f64) -> f64 {
    if value > 6.0 / 29.0 {
        value.powi(3)
    } else {
        (116.0 * value - 16.0) / KAPPA
    }
}

fn luminance(lightness: f64) -> f64 {
    if lightness <= 8.0 {
        lightness / KAPPA
    } else {
        ((lightness + 16.0) / 116.0).powi(3)
    }
}

fn white_uv() -> (f64, f64) {
    let denominator = WHITE_X + 15.0 + 3.0 * WHITE_Z;
    (4.0 * WHITE_X / denominator, 9.0 / denominator)
}

fn uv_to_xyz(u: f64, v: f64, y: f64) -> Option<[f64; 3]> {
    if y == 0.0 {
        Some([0.0; 3])
    } else if v <= 0.0 {
        None
    } else {
        Some([9.0 * y * u / (4.0 * v), y, y * (12.0 - 3.0 * u - 20.0 * v) / (4.0 * v)])
    }
}

fn encode(linear: [f64; 3]) -> Option<Rgb> {
    if linear.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let channel = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        let encoded = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (encoded * 255.0).round() as u8
    };
    Some(Rgb { r: channel(linear[0]), g: channel(linear[1]), b: channel(linear[2]) })
}
