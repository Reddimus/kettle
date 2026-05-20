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

    /// Companion to `by_name`: return the *canonical* bundled name
    /// (with the original casing the theme file ships under) for a
    /// case-insensitive user-typed name, or `None` if no bundled theme
    /// matches. Used by `Config::parse_collect` to keep `cfg.theme_name`
    /// in sync with `cfg.theme` — pre-cycle-176, a user typing
    /// `theme = TokyoNitght Night` (typo) had `cfg.theme_name` stored
    /// verbatim ("TokyoNitght Night") while `cfg.theme` silently fell
    /// back to the default, so `--check-config` showed a name the
    /// runtime wasn't actually using. Pure.
    pub fn find_name(name: &str) -> Option<&'static str> {
        let want = name.trim().to_ascii_lowercase();
        BUNDLED_THEMES
            .iter()
            .find_map(|(n, _)| (n.to_ascii_lowercase() == want).then_some(*n))
    }

    pub fn list() -> Vec<&'static str> {
        BUNDLED_THEMES.iter().map(|(n, _)| *n).collect()
    }

    /// The next (`forward`) or previous bundled theme name after `current`,
    /// wrapping around. If `current` isn't a bundled theme, returns the
    /// first one. Pure — used by runtime theme cycling.
    pub fn cycle(current: &str, forward: bool) -> &'static str {
        let names: Vec<&'static str> = BUNDLED_THEMES.iter().map(|(n, _)| *n).collect();
        if names.is_empty() {
            return "TokyoNight Night";
        }
        let n = names.len();
        // Case-insensitive + trimmed, mirroring `by_name`, so a config
        // like `theme = tokyonight night` still cycles from here.
        let want = current.trim().to_ascii_lowercase();
        match names.iter().position(|&x| x.to_ascii_lowercase() == want) {
            // Unknown current → start at the first theme (don't skip it).
            None => names[0],
            Some(i) => {
                let next = if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                names[next]
            }
        }
    }
}

fn set(slot: &mut Rgb, v: &str) {
    if let Some(rgb) = Rgb::parse(v) {
        *slot = rgb;
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn cycle_wraps_and_is_reversible() {
        let names = Theme::list();
        assert!(names.len() >= 2, "need ≥2 bundled themes to cycle");
        let first = names[0];
        let second = names[1];
        let last = *names.last().unwrap();

        // Forward from the first → second; backward from first → last.
        assert_eq!(Theme::cycle(first, true), second);
        assert_eq!(Theme::cycle(first, false), last);
        // Forward from the last wraps to the first.
        assert_eq!(Theme::cycle(last, true), first);
        // forward then backward is the identity.
        let nxt = Theme::cycle(first, true);
        assert_eq!(Theme::cycle(nxt, false), first);
        // Unknown current → first theme.
        assert_eq!(Theme::cycle("no such theme zzz", true), first);
        // Case-insensitive + trimmed, like `by_name` (a config that
        // lower-cases the name still cycles from the right spot).
        assert_eq!(
            Theme::cycle(&format!("  {}  ", first.to_uppercase()), true),
            second,
            "differently-cased/padded current resolves correctly"
        );
    }
}
