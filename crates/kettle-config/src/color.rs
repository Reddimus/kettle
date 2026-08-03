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
}

/// Parse one component of an X11/xterm `rgb:<r>/<g>/<b>` color into 8-bit.
///
/// The spec allows **1–4 hex digits** per component, scaled by digit width so
/// the value fills the channel range: `f` → `0xff`, `ff` → `0xff`, `fff` →
/// `0xff`, `ffff` → `0xff`. The old parser sliced the first
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

/// The standard named colors, as CSS Color Level 4 defines them — which is the
/// X11 `rgb.txt` list every terminal and theme format draws from.
///
/// Nine names used to be recognised. `--accent`'s own `--help` gives
/// `kettle --accent teal` as an example, and `teal` was not one of them: before
/// the flag was validated it silently fell back to the configured accent, and
/// after it was validated it became a hard error on kettle's own documented
/// example. Anything a person would actually type — `orange`, `purple`,
/// `pink`, `navy` — failed the same way.
///
/// The nine original entries keep their original values. `green` and
/// `gray`/`grey` differ between CSS and X11 `rgb.txt`, and configs have been
/// written against what kettle already resolved them to.
fn x11_name(name: &str) -> Option<Rgb> {
    Some(match name {
        // The original nine, values unchanged.
        "black" => Rgb::new(0, 0, 0),
        "red" => Rgb::new(255, 0, 0),
        "green" => Rgb::new(0, 128, 0),
        "yellow" => Rgb::new(255, 255, 0),
        "blue" => Rgb::new(0, 0, 255),
        "magenta" => Rgb::new(255, 0, 255),
        "cyan" => Rgb::new(0, 255, 255),
        "white" => Rgb::new(255, 255, 255),
        "gray" | "grey" => Rgb::new(190, 190, 190),
        "aliceblue" => Rgb::new(240, 248, 255),
        "antiquewhite" => Rgb::new(250, 235, 215),
        "aqua" => Rgb::new(0, 255, 255),
        "aquamarine" => Rgb::new(127, 255, 212),
        "azure" => Rgb::new(240, 255, 255),
        "beige" => Rgb::new(245, 245, 220),
        "bisque" => Rgb::new(255, 228, 196),
        "blanchedalmond" => Rgb::new(255, 235, 205),
        "blueviolet" => Rgb::new(138, 43, 226),
        "brown" => Rgb::new(165, 42, 42),
        "burlywood" => Rgb::new(222, 184, 135),
        "cadetblue" => Rgb::new(95, 158, 160),
        "chartreuse" => Rgb::new(127, 255, 0),
        "chocolate" => Rgb::new(210, 105, 30),
        "coral" => Rgb::new(255, 127, 80),
        "cornflowerblue" => Rgb::new(100, 149, 237),
        "cornsilk" => Rgb::new(255, 248, 220),
        "crimson" => Rgb::new(220, 20, 60),
        "darkblue" => Rgb::new(0, 0, 139),
        "darkcyan" => Rgb::new(0, 139, 139),
        "darkgoldenrod" => Rgb::new(184, 134, 11),
        "darkgray" => Rgb::new(169, 169, 169),
        "darkgreen" => Rgb::new(0, 100, 0),
        "darkgrey" => Rgb::new(169, 169, 169),
        "darkkhaki" => Rgb::new(189, 183, 107),
        "darkmagenta" => Rgb::new(139, 0, 139),
        "darkolivegreen" => Rgb::new(85, 107, 47),
        "darkorange" => Rgb::new(255, 140, 0),
        "darkorchid" => Rgb::new(153, 50, 204),
        "darkred" => Rgb::new(139, 0, 0),
        "darksalmon" => Rgb::new(233, 150, 122),
        "darkseagreen" => Rgb::new(143, 188, 143),
        "darkslateblue" => Rgb::new(72, 61, 139),
        "darkslategray" => Rgb::new(47, 79, 79),
        "darkslategrey" => Rgb::new(47, 79, 79),
        "darkturquoise" => Rgb::new(0, 206, 209),
        "darkviolet" => Rgb::new(148, 0, 211),
        "deeppink" => Rgb::new(255, 20, 147),
        "deepskyblue" => Rgb::new(0, 191, 255),
        "dimgray" => Rgb::new(105, 105, 105),
        "dimgrey" => Rgb::new(105, 105, 105),
        "dodgerblue" => Rgb::new(30, 144, 255),
        "firebrick" => Rgb::new(178, 34, 34),
        "floralwhite" => Rgb::new(255, 250, 240),
        "forestgreen" => Rgb::new(34, 139, 34),
        "fuchsia" => Rgb::new(255, 0, 255),
        "gainsboro" => Rgb::new(220, 220, 220),
        "ghostwhite" => Rgb::new(248, 248, 255),
        "gold" => Rgb::new(255, 215, 0),
        "goldenrod" => Rgb::new(218, 165, 32),
        "greenyellow" => Rgb::new(173, 255, 47),
        "honeydew" => Rgb::new(240, 255, 240),
        "hotpink" => Rgb::new(255, 105, 180),
        "indianred" => Rgb::new(205, 92, 92),
        "indigo" => Rgb::new(75, 0, 130),
        "ivory" => Rgb::new(255, 255, 240),
        "khaki" => Rgb::new(240, 230, 140),
        "lavender" => Rgb::new(230, 230, 250),
        "lavenderblush" => Rgb::new(255, 240, 245),
        "lawngreen" => Rgb::new(124, 252, 0),
        "lemonchiffon" => Rgb::new(255, 250, 205),
        "lightblue" => Rgb::new(173, 216, 230),
        "lightcoral" => Rgb::new(240, 128, 128),
        "lightcyan" => Rgb::new(224, 255, 255),
        "lightgoldenrodyellow" => Rgb::new(250, 250, 210),
        "lightgray" => Rgb::new(211, 211, 211),
        "lightgreen" => Rgb::new(144, 238, 144),
        "lightgrey" => Rgb::new(211, 211, 211),
        "lightpink" => Rgb::new(255, 182, 193),
        "lightsalmon" => Rgb::new(255, 160, 122),
        "lightseagreen" => Rgb::new(32, 178, 170),
        "lightskyblue" => Rgb::new(135, 206, 250),
        "lightslategray" => Rgb::new(119, 136, 153),
        "lightslategrey" => Rgb::new(119, 136, 153),
        "lightsteelblue" => Rgb::new(176, 196, 222),
        "lightyellow" => Rgb::new(255, 255, 224),
        "lime" => Rgb::new(0, 255, 0),
        "limegreen" => Rgb::new(50, 205, 50),
        "linen" => Rgb::new(250, 240, 230),
        "maroon" => Rgb::new(128, 0, 0),
        "mediumaquamarine" => Rgb::new(102, 205, 170),
        "mediumblue" => Rgb::new(0, 0, 205),
        "mediumorchid" => Rgb::new(186, 85, 211),
        "mediumpurple" => Rgb::new(147, 112, 219),
        "mediumseagreen" => Rgb::new(60, 179, 113),
        "mediumslateblue" => Rgb::new(123, 104, 238),
        "mediumspringgreen" => Rgb::new(0, 250, 154),
        "mediumturquoise" => Rgb::new(72, 209, 204),
        "mediumvioletred" => Rgb::new(199, 21, 133),
        "midnightblue" => Rgb::new(25, 25, 112),
        "mintcream" => Rgb::new(245, 255, 250),
        "mistyrose" => Rgb::new(255, 228, 225),
        "moccasin" => Rgb::new(255, 228, 181),
        "navajowhite" => Rgb::new(255, 222, 173),
        "navy" => Rgb::new(0, 0, 128),
        "oldlace" => Rgb::new(253, 245, 230),
        "olive" => Rgb::new(128, 128, 0),
        "olivedrab" => Rgb::new(107, 142, 35),
        "orange" => Rgb::new(255, 165, 0),
        "orangered" => Rgb::new(255, 69, 0),
        "orchid" => Rgb::new(218, 112, 214),
        "palegoldenrod" => Rgb::new(238, 232, 170),
        "palegreen" => Rgb::new(152, 251, 152),
        "paleturquoise" => Rgb::new(175, 238, 238),
        "palevioletred" => Rgb::new(219, 112, 147),
        "papayawhip" => Rgb::new(255, 239, 213),
        "peachpuff" => Rgb::new(255, 218, 185),
        "peru" => Rgb::new(205, 133, 63),
        "pink" => Rgb::new(255, 192, 203),
        "plum" => Rgb::new(221, 160, 221),
        "powderblue" => Rgb::new(176, 224, 230),
        "purple" => Rgb::new(128, 0, 128),
        "rebeccapurple" => Rgb::new(102, 51, 153),
        "rosybrown" => Rgb::new(188, 143, 143),
        "royalblue" => Rgb::new(65, 105, 225),
        "saddlebrown" => Rgb::new(139, 69, 19),
        "salmon" => Rgb::new(250, 128, 114),
        "sandybrown" => Rgb::new(244, 164, 96),
        "seagreen" => Rgb::new(46, 139, 87),
        "seashell" => Rgb::new(255, 245, 238),
        "sienna" => Rgb::new(160, 82, 45),
        "silver" => Rgb::new(192, 192, 192),
        "skyblue" => Rgb::new(135, 206, 235),
        "slateblue" => Rgb::new(106, 90, 205),
        "slategray" => Rgb::new(112, 128, 144),
        "slategrey" => Rgb::new(112, 128, 144),
        "snow" => Rgb::new(255, 250, 250),
        "springgreen" => Rgb::new(0, 255, 127),
        "steelblue" => Rgb::new(70, 130, 180),
        "tan" => Rgb::new(210, 180, 140),
        "teal" => Rgb::new(0, 128, 128),
        "thistle" => Rgb::new(216, 191, 216),
        "tomato" => Rgb::new(255, 99, 71),
        "turquoise" => Rgb::new(64, 224, 208),
        "violet" => Rgb::new(238, 130, 238),
        "wheat" => Rgb::new(245, 222, 179),
        "whitesmoke" => Rgb::new(245, 245, 245),
        "yellowgreen" => Rgb::new(154, 205, 50),
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

    /// A named color a person would actually type has to resolve.
    ///
    /// The table held nine names. `--accent`'s own `--help` gives
    /// `kettle --accent teal` as its example, so kettle documented an
    /// invocation it rejected — and once `--accent` started validating its
    /// value at the CLI surface, that example became a hard error instead of a
    /// silent fallback.
    #[test]
    fn the_named_colors_cover_what_the_docs_and_themes_use() {
        for (name, want) in [
            // The example in `--accent`'s help text.
            ("teal", Rgb::new(0, 128, 128)),
            ("orange", Rgb::new(255, 165, 0)),
            ("purple", Rgb::new(128, 0, 128)),
            ("pink", Rgb::new(255, 192, 203)),
            ("navy", Rgb::new(0, 0, 128)),
            ("olive", Rgb::new(128, 128, 0)),
            ("silver", Rgb::new(192, 192, 192)),
            ("gold", Rgb::new(255, 215, 0)),
            ("indigo", Rgb::new(75, 0, 130)),
            ("salmon", Rgb::new(250, 128, 114)),
            ("rebeccapurple", Rgb::new(102, 51, 153)),
            // Multi-word names run together, as CSS and X11 spell them.
            ("dodgerblue", Rgb::new(30, 144, 255)),
            ("darkslategray", Rgb::new(47, 79, 79)),
            ("lightgoldenrodyellow", Rgb::new(250, 250, 210)),
            // Case-insensitive, like the rest.
            ("Teal", Rgb::new(0, 128, 128)),
            ("DodgerBlue", Rgb::new(30, 144, 255)),
        ] {
            assert_eq!(Rgb::parse(name), Some(want), "{name} must resolve");
        }

        // The nine original names keep the values configs were written
        // against, where CSS and X11 `rgb.txt` disagree.
        assert_eq!(Rgb::parse("green"), Some(Rgb::new(0, 128, 0)));
        assert_eq!(Rgb::parse("gray"), Some(Rgb::new(190, 190, 190)));
        assert_eq!(Rgb::parse("grey"), Some(Rgb::new(190, 190, 190)));

        // Still not a colour-name-shaped free-for-all.
        for name in ["tael", "chartreuse-ish", "not a color", ""] {
            assert_eq!(Rgb::parse(name), None, "{name:?} must stay rejected");
        }
    }

    /// X11/xterm `rgb:` components scale by digit width
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
