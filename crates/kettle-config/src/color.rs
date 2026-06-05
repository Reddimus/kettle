//! RGB color type plus hex / X11-name parsing (Ghostty-compatible).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#rgb`, `#rrggbb`, `rrggbb`, `0xRRGGBB`, `rgb:rr/gg/bb` or an X11
    /// color name (the common subset Ghostty themes use).
    pub fn parse(s: &str) -> Option<Rgb> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("rgb:") {
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() == 3 {
                return Some(Rgb::new(
                    parse_x11_rgb_component(parts[0])?,
                    parse_x11_rgb_component(parts[1])?,
                    parse_x11_rgb_component(parts[2])?,
                ));
            }
        }
        let hex = s
            .strip_prefix('#')
            .or_else(|| s.strip_prefix("0x"))
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        if hex.chars().all(|c| c.is_ascii_hexdigit()) {
            match hex.len() {
                3 => {
                    let d = |i: usize| {
                        let v = u8::from_str_radix(&hex[i..i + 1], 16).ok()?;
                        Some(v * 17)
                    };
                    return Some(Rgb::new(d(0)?, d(1)?, d(2)?));
                }
                6 => {
                    let d = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
                    return Some(Rgb::new(d(0)?, d(2)?, d(4)?));
                }
                _ => {}
            }
        }
        x11_name(&s.to_ascii_lowercase())
    }

    pub fn to_array_f32(self) -> [f32; 3] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
        ]
    }
}

/// Parse one component of an X11/xterm `rgb:<r>/<g>/<b>` color into 8-bit.
///
/// The spec allows **1–4 hex digits** per component, scaled by digit width so
/// the value fills the channel range: `f` → `0xff`, `ff` → `0xff`, `fff` →
/// `0xff`, `ffff` → `0xff`. Cycle 819 (audit): the old parser sliced the first
/// two bytes and read them as the whole value, so `rgb:f/8/0` (full red in X11)
/// came out near-black `(15, 8, 0)` and 3-digit forms silently dropped a nibble.
///
/// Validating each **byte** as an ASCII hex digit first keeps the multibyte
/// safety the previous code had (a component like `rgb:€/00/00` is rejected as
/// `None`, never a non-char-boundary slice that would panic under panic=abort).
fn parse_x11_rgb_component(h: &str) -> Option<u8> {
    let hb = h.as_bytes();
    if hb.is_empty() || hb.len() > 4 || !hb.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    // All bytes are ASCII hex digits, so this slice is valid UTF-8 and fits u32.
    let v = u32::from_str_radix(std::str::from_utf8(hb).ok()?, 16).ok()?;
    let scaled = match hb.len() {
        1 => v * 0x11,           // 0..=0xf   → 0..=0xff
        2 => v,                  // 0..=0xff
        3 => (v * 0xff) / 0xfff, // 0..=0xfff → 0..=0xff
        4 => v >> 8,             // 0..=0xffff → high byte
        _ => unreachable!("length bounded to 1..=4 above"),
    };
    Some(scaled as u8)
}

fn x11_name(name: &str) -> Option<Rgb> {
    Some(match name {
        "black" => Rgb::new(0, 0, 0),
        "red" => Rgb::new(255, 0, 0),
        "green" => Rgb::new(0, 128, 0),
        "yellow" => Rgb::new(255, 255, 0),
        "blue" => Rgb::new(0, 0, 255),
        "magenta" => Rgb::new(255, 0, 255),
        "cyan" => Rgb::new(0, 255, 255),
        "white" => Rgb::new(255, 255, 255),
        "gray" | "grey" => Rgb::new(190, 190, 190),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_supported_form() {
        // #rrggbb / rrggbb (bare) / 0xRRGGBB / 0X.
        assert_eq!(Rgb::parse("#ff8800"), Some(Rgb::new(255, 136, 0)));
        assert_eq!(Rgb::parse("ff8800"), Some(Rgb::new(255, 136, 0)));
        assert_eq!(Rgb::parse("0xFF8800"), Some(Rgb::new(255, 136, 0)));
        assert_eq!(Rgb::parse("0XfF8800"), Some(Rgb::new(255, 136, 0)));
        // #rgb shorthand expands each nibble (f -> ff = 255).
        assert_eq!(Rgb::parse("#f80"), Some(Rgb::new(255, 136, 0)));
        // rgb:rr/gg/bb (X11/xterm form).
        assert_eq!(Rgb::parse("rgb:ff/88/00"), Some(Rgb::new(255, 136, 0)));
        // Leading/trailing whitespace is trimmed.
        assert_eq!(Rgb::parse("  #000000 "), Some(Rgb::new(0, 0, 0)));
        // X11 names (case-insensitive).
        assert_eq!(Rgb::parse("Red"), Some(Rgb::new(255, 0, 0)));
        assert_eq!(Rgb::parse("grey"), Some(Rgb::new(190, 190, 190)));
    }

    /// Cycle 819 (audit): X11/xterm `rgb:` components scale by digit width
    /// (1–4 hex digits), they aren't first-two-digits-truncated.
    #[test]
    fn rgb_components_scale_by_digit_width() {
        // 1-digit: f → 0xff (was the near-black bug: 15,8,0).
        assert_eq!(Rgb::parse("rgb:f/8/0"), Some(Rgb::new(255, 136, 0)));
        // 2-digit: unchanged.
        assert_eq!(Rgb::parse("rgb:ff/88/00"), Some(Rgb::new(255, 136, 0)));
        // 3-digit: fff → 0xff; f00 → (0xf00*0xff)/0xfff = 239.
        assert_eq!(Rgb::parse("rgb:fff/000/800"), Some(Rgb::new(255, 0, 127)));
        // 4-digit: ffff → 0xff (high byte); 8000 → 0x80.
        assert_eq!(
            Rgb::parse("rgb:ffff/8000/0000"),
            Some(Rgb::new(255, 128, 0))
        );
        // Mixed widths across components are allowed by the spec.
        assert_eq!(Rgb::parse("rgb:f/00/ffff"), Some(Rgb::new(255, 0, 255)));
        // Over-long (5 digits) is rejected.
        assert_eq!(Rgb::parse("rgb:fffff/0/0"), None);
    }

    #[test]
    fn rejects_invalid_without_panicking() {
        assert_eq!(Rgb::parse(""), None);
        assert_eq!(Rgb::parse("#12"), None); // wrong digit count
        assert_eq!(Rgb::parse("nonsense"), None);
        assert_eq!(Rgb::parse("rgb:zz/00/00"), None); // non-hex component
        assert_eq!(Rgb::parse("rgb:00/00"), None); // too few parts
    }

    #[test]
    fn rgb_form_with_multibyte_component_does_not_panic() {
        // Regression: `rgb:` component slicing used `&h[..2.min(h.len())]`
        // on the &str, so a component starting with a multibyte char made
        // the slice land on a non-char-boundary and panic — a hard crash
        // under panic=abort, reachable from theme/OSC color parsing. The
        // fix slices the *bytes* and validates via from_utf8, yielding None.
        assert_eq!(Rgb::parse("rgb:€/00/00"), None);
        assert_eq!(Rgb::parse("rgb:e€/00/00"), None);
        assert_eq!(Rgb::parse("rgb:00/00/日本"), None);
        // A bare multibyte string also must not panic.
        assert_eq!(Rgb::parse("€€€"), None);
        assert_eq!(Rgb::parse("#€€€"), None);
    }
}
