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

/// Mode-2027 scalar contribution, separate from shaping-base eligibility.
pub(super) fn scalar_width(c: char) -> Option<usize> {
    use unicode_width::UnicodeWidthChar;
    if emoji_presentation(c) {
        Some(2)
    } else if gcb(c) == Gcb::Prepend || matches!(c, '\u{ad}' | '\u{a8fa}' | '\u{17a4}' | '\u{17d8}')
    {
        // Match the application-side width model for visible prefixes and the
        // dependency's explicitly tailored singletons, only in grapheme mode.
        Some(1)
    } else {
        c.width()
    }
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GraphemeWidth {
    cells: usize,
    // Both totals stay capped at two; only the immediately preceding
    // contribution can be narrowed by a selector.
    cells_before_previous: usize,
    base: Option<char>,
    previous: Option<char>,
    previous_measured: bool,
    sinhala_linker: bool,
}

impl GraphemeWidth {
    /// The context after printable ASCII `c` opened a cluster: what
    /// `GraphemeWidth::default().advance(c)` leaves, without the lookups.
    pub(super) fn after_ascii(c: char) -> Self {
        debug_assert!(matches!(c, ' '..='~'));
        Self {
            cells: 1,
            cells_before_previous: 0,
            base: Some(c),
            previous: Some(c),
            previous_measured: true,
            sinhala_linker: false,
        }
    }

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
        let width = scalar_width(c).unwrap_or(0);
        let measured = width > 0;
        // VS15 narrows any two-cell variation base: emoji presentation and
        // East Asian Wide bases such as U+3030 alike.
        if c == '\u{fe0e}'
            && self.previous_measured
            && self.previous.is_some_and(|base| {
                property(base, tables::EMOJI_VARIATION_BASE)
                    && (emoji_presentation(base) || base.width() == Some(2))
            })
        {
            self.cells = (self.cells_before_previous + 1).min(2);
        } else if (c == '\u{fe0f}'
            && self.previous_measured
            && self
                .previous
                .is_some_and(|base| property(base, tables::EMOJI_VARIATION_BASE)))
            || (self.base.is_some()
                && property(c, tables::SPACING_MARK)
                && !property(c, tables::VIRAMA_SPACING_MARK))
        {
            self.cells = 2;
        } else if measured {
            // GB11 emoji chains keep the first emoji's width. Other positive
            // widths add, including Indic consonants after a virama and Thai/Lao Lo marks.
            let joined_emoji = self.previous == Some('\u{200d}')
                && self.base.is_some_and(pictographic)
                && pictographic(c);
            if !joined_emoji {
                self.cells_before_previous = self.cells;
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
    base_offset_with(text, is_scalar_base)
}

pub(super) fn shaping_base_offset(text: &str) -> usize {
    base_offset_with(text, is_base)
}

fn base_offset_with(text: &str, is_base: fn(char) -> bool) -> usize {
    text.char_indices()
        .find(|(_, c)| is_base(*c))
        .map_or(0, |(byte, _)| byte)
}

pub(super) fn is_base(c: char) -> bool {
    // A8FA is a spacing base only for owned grapheme shaping and retention.
    c == '\u{a8fa}' || is_scalar_base(c)
}

fn is_scalar_base(c: char) -> bool {
    use unicode_width::UnicodeWidthChar;
    if c.is_ascii() {
        return !c.is_ascii_control();
    }
    c.width().is_some_and(|width| width > 0) && gcb(c) != Gcb::Prepend
}

/// Streaming UAX #29 state, independent of how much cell text is retained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GraphemeState {
    previous: Option<Gcb>,
    regional_odd: bool,
    pictographic_extend: bool,
    pictographic_zwj: bool,
    indic_linker: bool,
    modifier_base_extend: bool,
}

impl GraphemeState {
    /// Whether printable ASCII starts a new grapheme here. Printable ASCII is
    /// Grapheme_Cluster_Break=Other with no InCB or emoji property, so only a
    /// preceding Prepend joins it (GB9b).
    pub(super) fn ascii_breaks(&self) -> bool {
        self.previous != Some(Gcb::Prepend)
    }

    /// The state after printable ASCII began a grapheme: what `advance`
    /// leaves whenever `ascii_breaks` held.
    pub(super) fn after_ascii() -> Self {
        Self {
            previous: Some(Gcb::Other),
            ..Self::default()
        }
    }

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
    fn mode_on_scalar_contributions_and_shaping_bases_stay_distinct() {
        for c in ['\u{d4e}', '\u{a8fa}', '\u{17a4}', '\u{17d8}', '\u{ad}'] {
            assert_eq!(scalar_width(c), Some(1));
        }
        assert!(is_base('\u{a8fa}'));
        assert_eq!(shaping_base_offset("\u{d4e}\u{a8fa}"), '\u{d4e}'.len_utf8());
        assert!(!is_base('\u{d4e}'));
        assert!(!is_base('\u{600}'));
        assert!(!is_base('\u{ad}'));
        assert_eq!(scalar_width('\u{301}'), Some(0));
        assert_eq!(scalar_width('\u{80}'), None);
    }

    #[test]
    fn scalar_classification_and_owned_shaping_keep_separate_bases() {
        let text = "\u{600}\u{a8fa}\u{fe0f}\u{e33}";
        assert_eq!(base_offset(text), 8, "scalar rendering keeps the Thai base");
        assert_eq!(shaping_base_offset(text), 2, "owned shaping anchors A8FA");
        let text = "\u{a8fa}\u{1f3fb}";
        assert_eq!(
            base_offset(text),
            3,
            "scalar rendering keeps the modifier base"
        );
        assert_eq!(shaping_base_offset(text), 0, "owned shaping anchors A8FA");
        for text in ["", "A", "\u{600}A", "\u{ad}A", "\u{301}"] {
            assert_eq!(base_offset(text), shaping_base_offset(text), "{text:?}");
        }
    }

    /// The ASCII fast path in `Term::input_grapheme` must leave exactly the
    /// state the full path does, whatever came before.
    #[test]
    fn printable_ascii_shortcut_matches_the_full_state_machine() {
        let prefixes: &[&str] = &[
            "",
            "a",
            " ",
            "\u{600}",
            "\u{110bd}",
            "\u{1f44d}",
            "\u{1f44d}\u{200d}",
            "\u{915}\u{94d}",
            "\u{1f1ef}",
            "e\u{301}",
            "\u{d9a}\u{dca}\u{200d}",
            "\u{1100}",
            "#\u{fe0f}",
            "\u{2764}",
            "\u{fe0f}",
        ];
        for prefix in prefixes {
            let mut state = GraphemeState::default();
            let mut width = GraphemeWidth::default();
            for c in prefix.chars() {
                if state.advance(c) {
                    width = GraphemeWidth::default();
                }
                width.advance(c);
            }
            for c in ' '..='~' {
                let mut full = state;
                let breaks = full.advance(c) && !width.joins_sinhala(c);
                assert_eq!(breaks, state.ascii_breaks(), "{prefix:?} then {c:?}");
                if breaks {
                    assert_eq!(full, GraphemeState::after_ascii(), "{prefix:?} then {c:?}");
                    let mut fresh = GraphemeWidth::default();
                    assert_eq!(fresh.advance(c), 1);
                    assert_eq!(fresh, GraphemeWidth::after_ascii(c));
                    assert_eq!(cluster_width(&[c]), 1);
                    assert!(!emoji_modifier(c) && !emoji_presentation(c));
                }
            }
        }
    }

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
            ("\u{600}A", 2),
            ("\u{ad}", 1),
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
    fn selector_width_keeps_adjacent_contribution_before_capping() {
        for (text, expected) in [
            ("*\u{301}\u{fe0f}", 1),
            ("*\u{200d}\u{fe0f}", 1),
            ("\u{600}\u{231a}\u{fe0e}", 2),
            ("\u{600}\u{600}\u{231a}\u{fe0e}", 2),
            ("\u{600}*\u{fe0f}", 2),
            ("\u{231a}\u{fe0e}", 1),
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
