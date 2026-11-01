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
}

impl GraphemeState {
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
