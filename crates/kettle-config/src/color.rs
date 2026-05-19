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
                let c = |h: &str| u8::from_str_radix(&h[..2.min(h.len())], 16).ok();
                return Some(Rgb::new(c(parts[0])?, c(parts[1])?, c(parts[2])?));
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
