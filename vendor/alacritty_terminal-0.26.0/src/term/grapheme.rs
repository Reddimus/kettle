//! Bounded-context Unicode 18 extended-grapheme boundaries (UAX #29).

#[path = "grapheme_tables.rs"]
mod tables;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gcb {
    Other,
    Cr,
    Lf,
    Control,
    Extend,
    Zwj,
    RegionalIndicator,
    Prepend,
    SpacingMark,
    L,
    V,
    T,
    Lv,
    Lvt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InCb {
    None,
    Consonant,
    Linker,
    Extend,
}

fn gcb(c: char) -> Gcb {
    let cp = c as u32;
    let i = tables::GCB.partition_point(|range| range.1 < cp);
    tables::GCB
        .get(i)
        .filter(|range| range.0 <= cp)
        .map_or(Gcb::Other, |range| range.2)
}

fn incb(c: char) -> InCb {
    let cp = c as u32;
    let i = tables::INCB.partition_point(|range| range.1 < cp);
    tables::INCB
        .get(i)
        .filter(|range| range.0 <= cp)
        .map_or(InCb::None, |range| range.2)
}

fn pictographic(c: char) -> bool {
    let cp = c as u32;
    let i = tables::EXTENDED_PICTOGRAPHIC.partition_point(|range| range.1 < cp);
    tables::EXTENDED_PICTOGRAPHIC
        .get(i)
        .is_some_and(|range| range.0 <= cp)
}

pub(super) fn emoji_presentation(c: char) -> bool {
    let cp = c as u32;
    let i = tables::EMOJI_PRESENTATION.partition_point(|range| range.1 < cp);
    tables::EMOJI_PRESENTATION
        .get(i)
        .is_some_and(|range| range.0 <= cp)
}

fn property(c: char, ranges: &[(u32, u32)]) -> bool {
    let cp = c as u32;
    let i = ranges.partition_point(|range| range.1 < cp);
    ranges.get(i).is_some_and(|range| range.0 <= cp)
}

pub(super) fn emoji_modifier(c: char) -> bool {
    matches!(c, '\u{1f3fb}'..='\u{1f3ff}')
}

/// Width of a single stored cluster; prefixes without a spacing base use one cell.
pub(super) fn cluster_width(codepoints: &[char]) -> usize {
    let mut state = GraphemeWidth::default();
    for &c in codepoints {
        state.advance(c);
    }
    state.cells.max(1)
}

/// Width context survives scalars discarded at the cell's storage limit.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct GraphemeWidth {
    cells: usize,
    base: Option<char>,
    previous: Option<char>,
    previous_measured: bool,
    sinhala_linker: bool,
}

impl GraphemeWidth {
    pub(super) fn joins_sinhala(&self, c: char) -> bool {
        // Sinhala rakaransaya uses al-lakuna + ZWJ + consonant, but Sinhala
        // consonants are excluded from the UCD's InCB=Consonant property.
        self.sinhala_linker
            && self.previous == Some('\u{200d}')
            && matches!(c, '\u{d9a}'..='\u{db1}' | '\u{db3}'..='\u{dbb}' | '\u{dbd}' | '\u{dc0}'..='\u{dc6}')
    }

    pub(super) fn advance(&mut self, c: char) -> usize {
        use unicode_width::UnicodeWidthChar;

        self.sinhala_linker =
            c == '\u{dca}' || (self.sinhala_linker && matches!(gcb(c), Gcb::Extend | Gcb::Zwj));
        let width = if emoji_presentation(c) {
            2
        } else {
            c.width().unwrap_or(0)
        };
        let measured = width > 0 && gcb(c) != Gcb::Prepend;
        if c == '\u{fe0e}'
            && self.previous_measured
            && self.previous.is_some_and(|base| {
                emoji_presentation(base) && property(base, tables::EMOJI_VARIATION_BASE)
            })
        {
            self.cells = self.cells.saturating_sub(1);
        } else if (c == '\u{fe0f}'
            && self
                .base
                .is_some_and(|base| property(base, tables::EMOJI_VARIATION_BASE)))
            || (self.base.is_some() && property(c, tables::SPACING_MARK))
        {
            self.cells = 2;
        } else if measured {
            // GB11 emoji chains keep the first emoji's width. Other positive
            // widths add, including Indic consonants after a virama and Thai/Lao Lo marks.
            let joined_emoji = self.previous == Some('\u{200d}')
                && self.base.is_some_and(pictographic)
                && pictographic(c);
            if !joined_emoji {
                self.cells = (self.cells + width).min(2);
            }
            self.base.get_or_insert(c);
            self.previous_measured = !joined_emoji;
            self.previous = Some(c);
            return self.cells;
        }
        self.previous = Some(c);
        self.previous_measured = false;
        self.cells
    }
}

pub(super) fn base_offset(text: &str) -> usize {
    text.char_indices()
        .find(|(_, c)| is_base(*c))
        .map_or(0, |(byte, _)| byte)
}

pub(super) fn is_base(c: char) -> bool {
    use unicode_width::UnicodeWidthChar;
    if c.is_ascii() {
        return !c.is_ascii_control();
    }
    c.width().is_some_and(|width| width > 0) && gcb(c) != Gcb::Prepend
}

/// Streaming UAX #29 state, independent of how much cell text is retained.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct GraphemeState {
    previous: Option<Gcb>,
    regional_odd: bool,
    pictographic_extend: bool,
    pictographic_zwj: bool,
    indic_linker: bool,
    modifier_base_extend: bool,
}

impl GraphemeState {
    pub(super) fn accepts_modifier(&self) -> bool {
        self.pictographic_extend || self.modifier_base_extend
    }

    /// Consume a scalar and report whether it begins a new extended grapheme.
    pub(super) fn advance(&mut self, next: char) -> bool {
        let after = gcb(next);
        let next_incb = incb(next);
        let next_pictographic = pictographic(next);
        let joins = self.previous.is_some_and(|before| {
            if before == Gcb::Cr && after == Gcb::Lf { return true; }
            if matches!(before, Gcb::Cr | Gcb::Lf | Gcb::Control)
                || matches!(after, Gcb::Cr | Gcb::Lf | Gcb::Control) { return false; }
            (before == Gcb::L && matches!(after, Gcb::L | Gcb::V | Gcb::Lv | Gcb::Lvt))
                || (matches!(before, Gcb::Lv | Gcb::V) && matches!(after, Gcb::V | Gcb::T))
                || (matches!(before, Gcb::Lvt | Gcb::T) && after == Gcb::T)
                || matches!(after, Gcb::Extend | Gcb::Zwj | Gcb::SpacingMark)
                || before == Gcb::Prepend
                // Unicode 18 GB9c: no initial consonant is required.
                || (self.indic_linker && next_incb == InCb::Consonant)
                || (self.pictographic_zwj && next_pictographic)
                || (before == Gcb::RegionalIndicator && after == Gcb::RegionalIndicator && self.regional_odd)
        });
        if !joins {
            *self = Self::default();
        }
        self.previous = Some(after);
        self.regional_odd = after == Gcb::RegionalIndicator && !self.regional_odd;
        self.pictographic_zwj = after == Gcb::Zwj && self.pictographic_extend;
        self.pictographic_extend =
            next_pictographic || (after == Gcb::Extend && self.pictographic_extend);
        self.modifier_base_extend = property(next, tables::EMOJI_MODIFIER_BASE)
            || (after == Gcb::Extend && self.modifier_base_extend);
        self.indic_linker = match next_incb {
            InCb::Linker => true,
            InCb::Extend => self.indic_linker,
            _ => false,
        };
        !joins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_width_matches_script_and_presentation_rules() {
        for (text, expected) in [
            ("\u{915}\u{93e}", 2),
            ("\u{b95}\u{bbe}", 2),
            ("\u{c95}\u{cbe}", 2),
            ("\u{d9a}\u{dca}\u{200d}\u{dbb}\u{dcf}", 2),
            ("\u{915}\u{94d}\u{915}", 2),
            ("\u{915}\u{94d}\u{915}\u{93e}", 2),
            ("\u{915}\u{94d}\u{924}\u{941}\u{902}", 2),
            ("\u{e01}\u{e33}", 2),
            ("\u{e81}\u{eb3}", 2),
            ("\u{231a}\u{fe0e}", 1),
            ("\u{1f21a}\u{fe0e}", 1),
            ("\u{1f3fb}", 2),
            ("\u{1f44d}\u{1f3fb}", 2),
            ("\u{2764}\u{fe0f}", 2),
            ("\u{1f469}\u{200d}\u{1f4bb}", 2),
            ("\u{2764}\u{200d}\u{1f525}", 1),
            ("\u{2764}\u{fe0f}\u{200d}\u{1f525}", 2),
            ("\u{1f1fa}\u{1f1f8}", 2),
            ("\u{94d}\u{915}", 1),
            ("\u{600}A", 1),
            ("\u{1100}\u{1161}\u{11a8}", 2),
            ("e\u{301}", 1),
        ] {
            assert_eq!(
                cluster_width(&text.chars().collect::<Vec<_>>()),
                expected,
                "{text:?}"
            );
        }
    }

    #[test]
    fn official_unicode18_boundaries() {
        assert_eq!(unicode_width::UNICODE_VERSION, (18, 0, 0));
        let mut cases = 0;
        for (line_number, line) in include_str!("../../tests/unicode18/GraphemeBreakTest.txt")
            .lines()
            .enumerate()
        {
            let body = line.split('#').next().unwrap().trim();
            if body.is_empty() {
                continue;
            }
            cases += 1;
            let parts: Vec<_> = body.split_whitespace().collect();
            let mut state = GraphemeState::default();
            for pair in parts[..parts.len() - 1].chunks_exact(2) {
                let c = char::from_u32(u32::from_str_radix(pair[1], 16).unwrap()).unwrap();
                let boundary = state.advance(c);
                assert_eq!(
                    boundary,
                    pair[0] == "\u{f7}",
                    "line {} before U+{:04X}",
                    line_number + 1,
                    c as u32
                );
            }
        }
        assert_eq!(cases, 853);
    }
    #[test]
    fn discarded_scalar_storage_does_not_discard_boundary_context() {
        assert!(std::mem::size_of::<GraphemeState>() <= 16);
        let mut state = GraphemeState::default();
        assert!(state.advance('\u{1f469}'));
        for _ in 0..1_000_000 {
            assert!(!state.advance('\u{301}'));
        }
        assert!(!state.advance('\u{200d}'));
        assert!(!state.advance('\u{1f4bb}'));
        assert!(state.advance('A'));
        let mut indic = GraphemeState::default();
        assert!(indic.advance('\u{94d}'));
        assert!(!indic.advance('\u{915}'));
    }
}
