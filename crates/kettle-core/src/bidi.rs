//! Implicit-mode bidirectional display of one terminal row.
//!
//! The grid, the PTY, copies and screen readers keep text in logical order.
//! Only the display changes: a row that holds a strong right-to-left
//! character (Hebrew, Arabic and so on) is laid out by the Unicode
//! Bidirectional Algorithm with a left-to-right paragraph level, as VTE and
//! Konsole show printed text. Right-to-left runs read right to left, brackets
//! inside them are mirrored (rule L4), and Arabic letters take their
//! contextual presentation forms, so a run joins as it would in a text view.
//!
//! Each row is laid out on its own. Explicit bidi controls (LRE, RLO, LRI and
//! the rest) are ignored, as level 1 of the terminal bidi recommendation
//! requires, so untrusted output can never reorder the rest of a line.
//!
//! A row without a strong right-to-left character has no map at all: its
//! layout is the identity, and the check costs one comparison per ASCII cell.

use std::ops::{Range, RangeInclusive};

use alacritty_terminal::index::Side;
use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo, bidi_class};

// Generated: kept exactly as scripts/generate-bidi-tables.py writes it.
#[rustfmt::skip]
#[path = "bidi_tables.rs"]
mod tables;

// The tables extend unicode-bidi's character classes: both must come from the
// same Unicode version, or a letter could be joined that the algorithm does
// not treat as Arabic.
const _: () = {
    let (major, minor, update) = tables::UCD_VERSION;
    let (bidi_major, bidi_minor, bidi_update) = unicode_bidi::UNICODE_VERSION;
    assert!(
        major == bidi_major && minor == bidi_minor && update == bidi_update,
        "regenerate the bidi tables for unicode-bidi's Unicode version"
    );
};

/// Unicode Joining_Type, without U (non-joining), the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Joining {
    Right,
    Left,
    Dual,
    Causing,
    Transparent,
}

/// How a cell sits in the grid.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CellKind {
    Narrow,
    /// The first column of a two-column character.
    WideLead,
    /// The second column of a two-column character.
    WideSpacer,
}

/// A zero-width joiner or non-joiner stored with a cell's marks: it decides
/// whether the cell joins the one after it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum JoinControl {
    #[default]
    None,
    Zwj,
    Zwnj,
}

/// What the layout needs to know about one grid cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BidiCell {
    /// The cell's base character (`'\0'` or `' '` for a blank).
    pub c: char,
    pub kind: CellKind,
    pub join: JoinControl,
}

impl BidiCell {
    /// A cell from its base character and the marks stored with it. Only a
    /// zero-width joiner or non-joiner among the marks matters (the last one
    /// wins); combining marks take their base's level and move with it.
    pub fn new(c: char, kind: CellKind, marks: &[char]) -> Self {
        let join = marks
            .iter()
            .rev()
            .find_map(|mark| match mark {
                '\u{200D}' => Some(JoinControl::Zwj),
                '\u{200C}' => Some(JoinControl::Zwnj),
                _ => None,
            })
            .unwrap_or_default();
        Self { c, kind, join }
    }
}

/// Whether `c` is a strong right-to-left character (bidi class R or AL).
/// Everything below U+0590 is answered without a table lookup.
pub fn is_strong_rtl(c: char) -> bool {
    c >= '\u{0590}' && matches!(bidi_class(c), BidiClass::R | BidiClass::AL)
}

/// One row's display layout: where each logical column is shown, and which
/// character a cell shows in place of its own (a mirrored bracket, an Arabic
/// contextual form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowBidi {
    log2vis: Box<[u16]>,
    vis2log: Box<[u16]>,
    rtl: Box<[bool]>,
    /// Sorted by logical column.
    display: Box<[(u16, char)]>,
}

/// One unit of the layout: a cell, or a two-column character with its spacer.
struct Unit {
    col: usize,
    width: usize,
    byte: usize,
    c: char,
    join: JoinControl,
}

/// The layout of a row of cells, or `None` when it is the identity: no
/// strong right-to-left character, or more columns than a `u16` holds.
pub fn row_bidi(cells: &[BidiCell]) -> Option<RowBidi> {
    if cells.len() > usize::from(u16::MAX) || !cells.iter().any(|cell| is_strong_rtl(cell.c)) {
        return None;
    }
    let mut units: Vec<Unit> = Vec::with_capacity(cells.len());
    let mut text = String::with_capacity(cells.len() * 2);
    let mut col = 0;
    while col < cells.len() {
        let cell = cells[col];
        let wide = cell.kind == CellKind::WideLead
            && cells
                .get(col + 1)
                .is_some_and(|next| next.kind == CellKind::WideSpacer);
        let c = match cell.kind {
            CellKind::WideSpacer => ' ',
            _ => layout_char(cell.c),
        };
        units.push(Unit {
            col,
            width: if wide { 2 } else { 1 },
            byte: text.len(),
            c,
            join: cell.join,
        });
        text.push(c);
        col += if wide { 2 } else { 1 };
    }

    let paragraph = ParagraphBidiInfo::new(&text, Some(Level::ltr()));
    let byte_levels = paragraph.reordered_levels(0..text.len());
    let levels: Vec<Level> = units.iter().map(|unit| byte_levels[unit.byte]).collect();

    let columns = cells.len();
    let mut log2vis = vec![0u16; columns];
    let mut vcol = 0;
    for index in ParagraphBidiInfo::reorder_visual(&levels) {
        let unit = &units[index];
        // A two-column character keeps its lead on the left in either
        // direction, so it draws the right way round.
        for k in 0..unit.width {
            log2vis[unit.col + k] = to_u16(vcol + k);
        }
        vcol += unit.width;
    }
    debug_assert_eq!(vcol, columns);
    let mut vis2log = vec![0u16; columns];
    for (logical, &visual) in log2vis.iter().enumerate() {
        vis2log[usize::from(visual)] = to_u16(logical);
    }

    let mut rtl = vec![false; columns];
    let mut display = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        if levels[index].is_rtl() {
            rtl[unit.col..unit.col + unit.width].fill(true);
        }
        let mut shown = unit.c;
        if levels[index].is_rtl()
            && let Some(mirror) = mirror(unit.c)
        {
            shown = mirror;
        }
        if let Some(form) = arabic_form(&units, &levels, index) {
            shown = form;
        }
        if shown != unit.c {
            display.push((to_u16(unit.col), shown));
        }
    }

    Some(RowBidi {
        log2vis: log2vis.into(),
        vis2log: vis2log.into(),
        rtl: rtl.into(),
        display: display.into(),
    })
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).expect("row_bidi rejects rows wider than u16::MAX")
}

/// The character the algorithm sees for a cell: a blank for an empty cell
/// and for an explicit bidi control, which this display ignores.
fn layout_char(c: char) -> char {
    if c == '\0' {
        return ' ';
    }
    match bidi_class(c) {
        BidiClass::LRE
        | BidiClass::LRO
        | BidiClass::RLE
        | BidiClass::RLO
        | BidiClass::PDF
        | BidiClass::LRI
        | BidiClass::RLI
        | BidiClass::FSI
        | BidiClass::PDI => ' ',
        _ => c,
    }
}

fn mirror(c: char) -> Option<char> {
    tables::MIRRORS
        .binary_search_by_key(&c, |&(from, _)| from)
        .ok()
        .map(|index| tables::MIRRORS[index].1)
}

fn joining(c: char) -> Option<Joining> {
    let code = u32::from(c);
    let index = tables::JOINING.partition_point(|&(_, end, _)| end < code);
    match tables::JOINING.get(index) {
        Some(&(start, _, kind)) if start <= code => Some(kind),
        _ if bidi_class(c) == BidiClass::NSM => Some(Joining::Transparent),
        _ => None,
    }
}

fn forms(c: char) -> Option<[Option<char>; 4]> {
    tables::FORMS
        .binary_search_by_key(&c, |&(base, _)| base)
        .ok()
        .map(|index| tables::FORMS[index].1)
}

/// The nearest unit before or after `index` at the same level, skipping
/// transparent units (Unicode core spec 9.2, within one level run).
fn neighbour(units: &[Unit], levels: &[Level], index: usize, forward: bool) -> Option<usize> {
    let level = levels[index];
    let mut at = index;
    loop {
        at = if forward {
            at.checked_add(1).filter(|&n| n < units.len())?
        } else {
            at.checked_sub(1)?
        };
        if levels[at] != level {
            return None;
        }
        if joining(units[at].c) != Some(Joining::Transparent) {
            return Some(at);
        }
    }
}

/// The contextual presentation form of an Arabic letter, if it has one.
fn arabic_form(units: &[Unit], levels: &[Level], index: usize) -> Option<char> {
    let unit = &units[index];
    let kind = joining(unit.c)?;
    if !matches!(kind, Joining::Dual | Joining::Right) {
        return None;
    }
    let forms = forms(unit.c)?;
    // A joiner stored with the previous letter forces the join; a non-joiner
    // breaks it.
    let joins_back = neighbour(units, levels, index, false).is_some_and(|prev| {
        let before = &units[prev];
        match before.join {
            JoinControl::Zwnj => false,
            JoinControl::Zwj => true,
            JoinControl::None => matches!(
                joining(before.c),
                Some(Joining::Dual | Joining::Left | Joining::Causing)
            ),
        }
    });
    let joins_ahead = kind == Joining::Dual
        && match unit.join {
            JoinControl::Zwnj => false,
            JoinControl::Zwj => true,
            JoinControl::None => neighbour(units, levels, index, true).is_some_and(|next| {
                matches!(
                    joining(units[next].c),
                    Some(Joining::Dual | Joining::Right | Joining::Causing)
                )
            }),
        };
    let [isolated, final_, initial, medial] = forms;
    match (joins_back, joins_ahead) {
        (true, true) => medial.or(final_).or(isolated),
        (true, false) => final_.or(isolated),
        (false, true) => initial.or(isolated),
        (false, false) => isolated,
    }
}

impl RowBidi {
    /// The number of columns the layout covers.
    pub fn columns(&self) -> usize {
        self.log2vis.len()
    }

    /// The visual column a logical column is shown in. Columns beyond the
    /// row are unchanged.
    pub fn visual(&self, col: usize) -> usize {
        self.log2vis.get(col).map_or(col, |&v| usize::from(v))
    }

    /// The logical column shown in a visual column. Columns beyond the row
    /// are unchanged.
    pub fn logical(&self, vcol: usize) -> usize {
        self.vis2log.get(vcol).map_or(vcol, |&l| usize::from(l))
    }

    /// Whether a logical column resolved to a right-to-left level.
    pub fn is_rtl(&self, col: usize) -> bool {
        self.rtl.get(col).copied().unwrap_or(false)
    }

    /// The character a logical cell shows in place of its own: a mirrored
    /// bracket or an Arabic contextual form.
    pub fn display_char(&self, col: usize) -> Option<char> {
        let col = u16::try_from(col).ok()?;
        self.display
            .binary_search_by_key(&col, |&(at, _)| at)
            .ok()
            .map(|index| self.display[index].1)
    }

    /// Whether a shaping ligature must not join this cell to the next one in
    /// logical order: once reordered they are no longer neighbours.
    pub fn guard_after(&self, col: usize) -> bool {
        self.is_rtl(col) || self.is_rtl(col + 1)
    }

    /// The visual column ranges, merged and ascending, that show the logical
    /// columns in `cols` (a selection or a match).
    pub fn visual_runs(&self, cols: RangeInclusive<usize>) -> Vec<Range<usize>> {
        let end = (*cols.end()).min(self.columns().saturating_sub(1));
        let mut visual: Vec<usize> = (*cols.start()..=end).map(|col| self.visual(col)).collect();
        visual.sort_unstable();
        let mut runs: Vec<Range<usize>> = Vec::new();
        for v in visual {
            match runs.last_mut() {
                Some(run) if run.end == v => run.end = v + 1,
                _ => runs.push(v..v + 1),
            }
        }
        runs
    }

    /// The logical cell and side under a pointer at visual column `vcol` on
    /// side `side`. A pointer past the row's end lands on the right half of
    /// the rightmost visual cell. Sides flip inside right-to-left text, where
    /// a cell's visual right edge is its logical start.
    pub fn logical_point(&self, vcol: usize, side: Side) -> (usize, Side) {
        let columns = self.columns();
        if columns == 0 {
            return (vcol, side);
        }
        let (vcol, side) = if vcol >= columns {
            (columns - 1, Side::Right)
        } else {
            (vcol, side)
        };
        let col = self.logical(vcol);
        let side = if self.is_rtl(col) {
            match side {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            }
        } else {
            side
        };
        (col, side)
    }
}

#[cfg(test)]
#[path = "bidi_tests.rs"]
mod tests;
