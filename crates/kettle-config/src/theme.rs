//! Theme model + the bundled Ghostty theme set (the iTerm2-Color-Schemes
//! `ghostty/` collection, ~500 themes, embedded at compile time).

use crate::color::Rgb;
use crate::parse;

include!(concat!(env!("OUT_DIR"), "/themes_generated.rs"));

#[derive(Debug, Clone)]
pub struct Theme {
    /// ANSI palette 0..=15.
    pub palette: [Rgb; 16],
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    pub selection_background: Rgb,
    pub selection_foreground: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        // TokyoNight Night — the shipped default. Matched verbatim to the
        // bundled theme file; resolved from the bundle at startup, this is only
        // the hard fallback.
        Theme {
            palette: [
                Rgb::new(0x15, 0x16, 0x1e),
                Rgb::new(0xf7, 0x76, 0x8e),
                Rgb::new(0x9e, 0xce, 0x6a),
                Rgb::new(0xe0, 0xaf, 0x68),
                Rgb::new(0x7a, 0xa2, 0xf7),
                Rgb::new(0xbb, 0x9a, 0xf7),
                Rgb::new(0x7d, 0xcf, 0xff),
                Rgb::new(0xa9, 0xb1, 0xd6),
                Rgb::new(0x41, 0x48, 0x68),
                Rgb::new(0xf7, 0x76, 0x8e),
                Rgb::new(0x9e, 0xce, 0x6a),
                Rgb::new(0xe0, 0xaf, 0x68),
                Rgb::new(0x7a, 0xa2, 0xf7),
                Rgb::new(0xbb, 0x9a, 0xf7),
                Rgb::new(0x7d, 0xcf, 0xff),
                Rgb::new(0xc0, 0xca, 0xf5),
            ],
            background: Rgb::new(0x1a, 0x1b, 0x26),
            foreground: Rgb::new(0xc0, 0xca, 0xf5),
            cursor: Rgb::new(0xc0, 0xca, 0xf5),
            cursor_text: Rgb::new(0x1a, 0x1b, 0x26),
            selection_background: Rgb::new(0x28, 0x34, 0x57),
            selection_foreground: Rgb::new(0xc0, 0xca, 0xf5),
        }
    }
}

impl Theme {
    /// Parse a theme from Ghostty-syntax text (`palette = N=#hex`, etc.).
    /// Unspecified fields keep the default (TokyoNight Night) value.
    pub fn parse(text: &str) -> Theme {
        let mut t = Theme::default();
        for e in parse::parse(text) {
            match e.key.as_str() {
                "palette" => {
                    if let Some((idx, hex)) = e.value.split_once('=')
                        && let (Ok(i), Some(rgb)) =
                            (idx.trim().parse::<usize>(), Rgb::parse(hex.trim()))
                        && i < 16
                    {
                        t.palette[i] = rgb;
                    }
                }
                "background" => set(&mut t.background, &e.value),
                "foreground" => set(&mut t.foreground, &e.value),
                "cursor-color" => set(&mut t.cursor, &e.value),
                "cursor-text" => set(&mut t.cursor_text, &e.value),
                "selection-background" => set(&mut t.selection_background, &e.value),
                "selection-foreground" => set(&mut t.selection_foreground, &e.value),
                _ => {}
            }
        }
        t
    }

    /// Look up a bundled theme by name (case-insensitive). Returns the default
    /// (TokyoNight Night) if not found.
    pub fn by_name(name: &str) -> Theme {
        let want = name.trim().to_ascii_lowercase();
        for (n, body) in BUNDLED_THEMES.iter() {
            if n.to_ascii_lowercase() == want {
                return Theme::parse(body);
            }
        }
        Theme::default()
    }

    pub fn list() -> Vec<&'static str> {
        BUNDLED_THEMES.iter().map(|(n, _)| *n).collect()
    }
}

fn set(slot: &mut Rgb, v: &str) {
    if let Some(rgb) = Rgb::parse(v) {
        *slot = rgb;
    }
}
