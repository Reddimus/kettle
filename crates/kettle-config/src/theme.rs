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
        // `eq_ignore_ascii_case` compares in place — no per-element
        // `to_ascii_lowercase` String alloc (this runs on every theme keypress
        // / session restore over ~513 bundled names). Cycle 843 (audit).
        let want = name.trim();
        for (n, body) in BUNDLED_THEMES.iter() {
            if n.eq_ignore_ascii_case(want) {
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
        let want = name.trim();
        BUNDLED_THEMES
            .iter()
            .find_map(|(n, _)| n.eq_ignore_ascii_case(want).then_some(*n))
    }

    pub fn list() -> Vec<&'static str> {
        BUNDLED_THEMES.iter().map(|(n, _)| *n).collect()
    }

    /// The next (`forward`) or previous bundled theme name after `current`,
    /// wrapping around. If `current` isn't a bundled theme, returns the
    /// first one. Pure — used by runtime theme cycling.
    pub fn cycle(current: &str, forward: bool) -> &'static str {
        let n = BUNDLED_THEMES.len();
        if n == 0 {
            return "TokyoNight Night";
        }
        // Case-insensitive + trimmed, mirroring `by_name`, so a config
        // like `theme = tokyonight night` still cycles from here. Operate on
        // BUNDLED_THEMES directly — no intermediate names Vec, no per-element
        // lowercase alloc (cycle 843, audit).
        let want = current.trim();
        match BUNDLED_THEMES
            .iter()
            .position(|(x, _)| x.eq_ignore_ascii_case(want))
        {
            // Unknown current → start at the first theme (don't skip it).
            None => BUNDLED_THEMES[0].0,
            Some(i) => {
                let next = if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                BUNDLED_THEMES[next].0
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

    /// Cycle 843: `by_name`/`find_name` dropped their per-element
    /// `to_ascii_lowercase` for `eq_ignore_ascii_case`. Guard that
    /// case/padding-insensitivity survives the rewrite.
    #[test]
    fn by_name_and_find_name_are_case_and_pad_insensitive() {
        let first = Theme::list()[0];
        let typed = format!("  {}  ", first.to_uppercase());
        assert_eq!(
            Theme::find_name(&typed),
            Some(first),
            "find_name resolves a differently-cased/padded name to the canonical one"
        );
        // by_name returns the parsed theme; a matched name must resolve to the
        // same theme as the verbatim-name parse (Theme has no PartialEq — its
        // Debug repr is a faithful structural fingerprint here).
        assert_eq!(
            format!("{:?}", Theme::by_name(&typed)),
            format!("{:?}", Theme::by_name(first)),
        );
        assert_eq!(Theme::find_name("no such theme zzz"), None);
    }
}
