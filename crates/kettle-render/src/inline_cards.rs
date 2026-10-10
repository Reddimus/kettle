//! Per-pane registered-card recognition from complete lock-free snapshots.

use crate::{PaneSnapshot, SnapCell};
use kettle_core::{InlineMarker, InlineNonce};
use std::collections::HashMap;

/// Most cards one pane holds at once.
pub const MAX_PANE_CARDS: usize = 64;
/// Most columns a card spans; a wider block would wrap in the harness.
pub const MAX_CARD_COLUMNS: u8 = 107;
pub(crate) const MAX_CARD_ROWS: u8 = 12;

/// Which harness prints a card, which fixes the label recognition expects
/// above its rows and where its rows start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardHarness {
    /// Claude Code's synchronous hook message under the tool call.
    ClaudeHook,
    /// Codex's hook message under the tool call (Codex CLI 0.162.0).
    CodexHook,
}

impl CardHarness {
    /// The label line above a card's rows as the harness prints it, from
    /// the line's first character after any indent.
    fn label(self) -> &'static str {
        match self {
            // Claude Code's `⎿` with its spacing, then the hook's label.
            Self::ClaudeHook => {
                "\u{23bf} \u{00a0}PostToolUse:mcp__plugin_kettle_kettle__kettle_show says:"
            }
            // Codex's hook line; the message's first line, which Kettle
            // leaves empty, would follow it.
            Self::CodexHook => "\u{21b3} Hook \u{00b7}",
        }
    }

    /// The column a card's rows, and the caption under them, start at.
    pub fn left(self) -> usize {
        match self {
            Self::ClaudeHook => 5,
            Self::CodexHook => 4,
        }
    }

    /// The harness's name, on a card's provenance tab.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ClaudeHook => "Claude",
            Self::CodexHook => "Codex",
        }
    }

    /// The columns the harness's label line takes as it prints it: Claude
    /// Code's two-column indent, then the label; Codex's label and the space
    /// before the message's empty first line. A narrower pane wraps the
    /// label, and the card under it is never found.
    pub fn label_columns(self) -> usize {
        match self {
            Self::ClaudeHook => 2 + self.label().chars().count(),
            Self::CodexHook => self.label().chars().count() + 1,
        }
    }
}

/// A card's poster, held weakly. Its pixels belong to the shelf item, so a
/// card never keeps them charged to the preview account after the shelf
/// lets them go.
#[derive(Clone, Debug)]
pub struct CardPoster {
    width: u32,
    height: u32,
    rgba: std::sync::Weak<kettle_core::PixelBuffer>,
}

impl CardPoster {
    pub fn new(image: &kettle_core::ImageData) -> Self {
        Self {
            width: image.width,
            height: image.height,
            rgba: std::sync::Arc::downgrade(&image.rgba),
        }
    }

    fn upgrade(&self) -> Option<kettle_core::ImageData> {
        Some(kettle_core::ImageData {
            width: self.width,
            height: self.height,
            rgba: self.rgba.upgrade()?,
        })
    }
}

/// A card to register: its block's size, who prints it, its caption and its
/// poster. Kettle builds every part; nothing comes from the harness.
#[derive(Clone, Debug)]
pub struct CardSpec {
    pub rows: u8,
    pub columns: u8,
    pub harness: CardHarness,
    pub caption: String,
    pub poster: Option<CardPoster>,
    /// A video's length: the card shows a play glyph and the duration on
    /// its poster. `None` for anything else.
    pub duration_ms: Option<u64>,
}

/// Why a card was not registered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardRefusal {
    /// Its block has no rows or columns, or more than a card may have.
    BadSize,
    /// The pane already holds as many cards as it may.
    Full,
    /// Another card in the pane has its nonce.
    Duplicate,
}

struct RegisteredCard {
    rows: u8,
    columns: u8,
    harness: CardHarness,
    caption: String,
    poster: Option<CardPoster>,
    pending: bool,
    duration_ms: Option<u64>,
}

/// Owned by Pane: the cards the UI registered for it, as the renderer paints
/// them. The UI decides who may register and when a card expires.
#[derive(Default)]
pub struct InlineCards {
    entries: HashMap<InlineNonce, RegisteredCard>,
    /// The card that says a click opens it, while the UI shows that tip.
    tip: Option<InlineNonce>,
}

impl InlineCards {
    pub fn needs_marks(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, nonce: InlineNonce) -> bool {
        self.entries.contains_key(&nonce)
    }

    /// Register a card under `nonce`.
    pub fn insert(&mut self, nonce: InlineNonce, spec: CardSpec) -> Result<(), CardRefusal> {
        if spec.rows == 0
            || spec.columns == 0
            || spec.rows > MAX_CARD_ROWS
            || spec.columns > MAX_CARD_COLUMNS
        {
            return Err(CardRefusal::BadSize);
        }
        if self.entries.contains_key(&nonce) {
            return Err(CardRefusal::Duplicate);
        }
        if self.entries.len() >= MAX_PANE_CARDS {
            return Err(CardRefusal::Full);
        }
        let pending = spec.poster.is_none();
        self.entries.insert(
            nonce,
            RegisteredCard {
                rows: spec.rows,
                columns: spec.columns,
                harness: spec.harness,
                caption: spec.caption,
                poster: spec.poster,
                pending,
                duration_ms: spec.duration_ms,
            },
        );
        Ok(())
    }

    /// The video length of the card under `nonce`, if it shows a video.
    pub(crate) fn duration(&self, nonce: InlineNonce) -> Option<u64> {
        self.entries.get(&nonce)?.duration_ms
    }

    /// Show `image` on the card under `nonce`, as its item's pixels rendered
    /// again: held weakly, the poster stays only while the caller, like a
    /// shelf, holds `image`. Whether the card is registered.
    pub fn set_poster(
        &mut self,
        nonce: InlineNonce,
        image: Option<&kettle_core::ImageData>,
    ) -> bool {
        match self.entries.get_mut(&nonce) {
            Some(card) => {
                card.poster = image.map(CardPoster::new);
                card.pending = false;
                true
            }
            None => false,
        }
    }

    /// Forget the card under `nonce`; its text stays in the transcript and
    /// paints as an unknown card from the next frame.
    pub fn remove(&mut self, nonce: InlineNonce) -> bool {
        if self.tip == Some(nonce) {
            self.tip = None;
        }
        self.entries.remove(&nonce).is_some()
    }

    /// Show, on the registered card `nonce`, the tip that a click opens it,
    /// or no tip with `None`.
    pub fn set_tip(&mut self, nonce: Option<InlineNonce>) {
        self.tip = nonce.filter(|nonce| self.entries.contains_key(nonce));
    }

    /// The card showing the tip, if any.
    pub(crate) fn tip(&self) -> Option<InlineNonce> {
        self.tip
    }

    #[cfg(test)]
    fn register_for(&mut self, nonce: InlineNonce, rows: u8, columns: u8, harness: CardHarness) {
        self.entries.insert(
            nonce,
            RegisteredCard {
                rows,
                columns,
                harness,
                caption: "diagram.png - raster 640x480".into(),
                poster: None,
                pending: true,
                duration_ms: None,
            },
        );
    }

    pub(crate) fn visual(&self, nonce: InlineNonce) -> Option<CardVisual> {
        let entry = self.entries.get(&nonce)?;
        Some(match entry.poster.as_ref().map(CardPoster::upgrade) {
            Some(Some(image)) => CardVisual::Ready(image),
            // The shelf released the pixels.
            Some(None) => CardVisual::Failed,
            None if entry.pending => CardVisual::Pending,
            None => CardVisual::Failed,
        })
    }

    /// Recomputed every painted frame; the renderer reuses frame and scratch
    /// storage. Grid overwrite, scroll, or changed context cannot keep old
    /// geometry alive. This does not inspect color or acquire the Term lock.
    pub(crate) fn recognize_into(&self, snap: &PaneSnapshot, frame: &mut CardFrame) {
        frame.blocks.clear();
        frame.accepted_cells.clear();
        frame.known_cells.clear();
        frame.groups.clear();
        for (index, marks) in snap.card_marks.cells() {
            if snap
                .cells
                .get(index)
                .is_some_and(SnapCell::is_card_placeholder)
                && InlineMarker::decode(marks)
                    .is_some_and(|marker| self.entries.contains_key(&marker.nonce))
            {
                frame.known_cells.push(index);
            }
            let Some(key) = self.group_key(snap, index, marks) else {
                continue;
            };
            frame.groups.entry(key).or_default().cells += 1;
        }
        let Ok(offset) = i32::try_from(snap.display_offset) else {
            return;
        };
        for (&(nonce, line, column), group) in &mut frame.groups {
            let entry = &self.entries[&nonce];
            if group.cells != usize::from(entry.rows) * usize::from(entry.columns)
                || column != entry.harness.left()
            {
                continue;
            }
            let Some(view_row) = line.checked_add(offset) else {
                continue;
            };
            if usize::try_from(view_row).is_ok_and(|row| row >= snap.screen_lines)
                || view_row
                    .checked_add(i32::from(entry.rows))
                    .is_none_or(|end| end <= 0)
                || column
                    .checked_add(usize::from(entry.columns))
                    .is_none_or(|end| end > snap.columns)
            {
                continue;
            }
            let gutter_intact = (0..i32::from(entry.rows)).all(|row| {
                (0..column).all(|col| {
                    cell_at(snap, line + row, col)
                        .is_some_and(|cell| cell.c == ' ' && cell.zerowidth().is_empty())
                })
            });
            if !gutter_intact {
                continue;
            }
            let Some(label_line) = line.checked_sub(1) else {
                continue;
            };
            let Some(caption_line) = line.checked_add(i32::from(entry.rows)) else {
                continue;
            };
            if !row_matches(snap, label_line, 0, entry.harness.label().chars(), true)
                || !row_matches(snap, caption_line, column, entry.caption.chars(), false)
            {
                continue;
            }
            group.accepted = true;
            frame.blocks.push(CardBlock {
                nonce,
                line,
                column,
                rows: entry.rows,
                columns: entry.columns,
                harness: entry.harness,
            });
        }
        // Snapshot capture supplies each complete sequence once in row-major
        // order, so this second bounded mark walk needs no sort or index map.
        for (index, marks) in snap.card_marks.cells() {
            if index >= snap.cells.len() {
                continue;
            }
            let Some(key) = self.group_key(snap, index, marks) else {
                continue;
            };
            if frame.groups.get(&key).is_some_and(|group| group.accepted) {
                frame.accepted_cells.push(index);
            }
        }
        frame
            .blocks
            .sort_unstable_by_key(|block| (block.line, block.column));
    }

    fn group_key(
        &self,
        snap: &PaneSnapshot,
        index: usize,
        marks: &[char],
    ) -> Option<(InlineNonce, i32, usize)> {
        let cell = snap.card_cell(index)?;
        if !cell.is_card_placeholder() {
            return None;
        }
        let marker = InlineMarker::decode(marks)?;
        let entry = self.entries.get(&marker.nonce)?;
        if marker.row >= entry.rows || marker.column >= entry.columns {
            return None;
        }
        Some((
            marker.nonce,
            cell.line.checked_sub(i32::from(marker.row))?,
            cell.col.checked_sub(usize::from(marker.column))?,
        ))
    }

    /// The registered cards `snap` shows whole, as a frame would draw them:
    /// each one's nonce, its first grid line and column, and its size in
    /// cells. For a caller that must place cards against the same grid it
    /// reads text from, not against the last frame drawn.
    pub fn placements(&self, snap: &PaneSnapshot) -> Vec<CardPlacement> {
        let mut frame = CardFrame::default();
        self.recognize_into(snap, &mut frame);
        frame
            .blocks
            .iter()
            .map(|block| CardPlacement {
                nonce: block.nonce,
                line: block.line,
                column: block.column,
                rows: block.rows,
                columns: block.columns,
            })
            .collect()
    }

    #[cfg(test)]
    fn recognize(&self, snap: &PaneSnapshot) -> CardFrame {
        let mut frame = CardFrame::default();
        self.recognize_into(snap, &mut frame);
        frame
    }
}

pub(crate) enum CardVisual {
    Ready(kettle_core::ImageData),
    Pending,
    Failed,
}

#[derive(Default)]
struct Group {
    cells: usize,
    accepted: bool,
}

#[derive(Default)]
pub(crate) struct CardFrame {
    pub blocks: Vec<CardBlock>,
    accepted_cells: Vec<usize>,
    known_cells: Vec<usize>,
    groups: HashMap<(InlineNonce, i32, usize), Group>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardCell {
    Ordinary,
    Accepted,
    Neutral,
    Unknown,
}

impl CardCell {
    pub fn glyph(self, raw: char, hidden: bool) -> char {
        match self {
            Self::Ordinary => {
                if hidden {
                    ' '
                } else {
                    raw
                }
            }
            Self::Accepted => ' ',
            Self::Neutral => '\u{25a1}',
            Self::Unknown => '\u{2b1a}',
        }
    }
}

impl CardFrame {
    pub(crate) fn clear(&mut self) {
        self.blocks.clear();
        self.accepted_cells.clear();
        self.known_cells.clear();
        self.groups.clear();
    }

    pub fn cell(&self, snap: &PaneSnapshot, index: usize) -> CardCell {
        if !snap
            .cells
            .get(index)
            .is_some_and(SnapCell::is_card_placeholder)
        {
            CardCell::Ordinary
        } else if self.suppress_cell(index) {
            CardCell::Accepted
        } else if self.known_cells.binary_search(&index).is_ok() {
            CardCell::Neutral
        } else {
            CardCell::Unknown
        }
    }

    pub fn cell_at(&self, snap: &PaneSnapshot, line: i32, col: usize) -> CardCell {
        snap.cells
            .binary_search_by_key(&(line, col), |cell| (cell.line, cell.col))
            .map(|index| self.cell(snap, index))
            .unwrap_or(CardCell::Ordinary)
    }

    pub fn suppress_point(&self, snap: &PaneSnapshot, line: i32, col: usize) -> bool {
        snap.cells
            .binary_search_by_key(&(line, col), |cell| (cell.line, cell.col))
            .is_ok_and(|index| self.suppress_cell(index))
    }

    pub fn suppress_cell(&self, snapshot_index: usize) -> bool {
        self.accepted_cells.binary_search(&snapshot_index).is_ok()
    }
}

/// Where a recognized card sits in its pane's grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardPlacement {
    pub nonce: InlineNonce,
    /// Its first line, counted as the grid counts (the screen's top is 0,
    /// history above it negative).
    pub line: i32,
    pub column: usize,
    pub rows: u8,
    pub columns: u8,
}

pub(crate) struct CardBlock {
    pub nonce: InlineNonce,
    pub line: i32,
    pub column: usize,
    pub rows: u8,
    pub columns: u8,
    /// Who printed it, which its provenance tab names.
    pub harness: CardHarness,
}

fn cell_at(snap: &PaneSnapshot, line: i32, column: usize) -> Option<&SnapCell> {
    let row = snap.card_row(line);
    row.binary_search_by_key(&column, |cell| cell.col)
        .ok()
        .map(|index| &row[index])
}

fn row_matches(
    snap: &PaneSnapshot,
    line: i32,
    column: usize,
    expected: impl Iterator<Item = char>,
    trim_leading: bool,
) -> bool {
    let characters = snap
        .card_row(line)
        .iter()
        .filter(|cell| {
            cell.col >= column
                && !cell.flags.intersects(
                    kettle_core::Flags::WIDE_CHAR_SPACER
                        | kettle_core::Flags::LEADING_WIDE_CHAR_SPACER,
                )
        })
        .flat_map(|cell| std::iter::once(cell.c).chain(cell.zerowidth().iter().copied()));
    let mut reading_prefix = trim_leading;
    let mut actual = characters.skip_while(move |ch| {
        if reading_prefix && ch.is_whitespace() {
            true
        } else {
            reading_prefix = false;
            false
        }
    });
    for want in expected {
        if actual.next() != Some(want) {
            return false;
        }
    }
    actual.all(char::is_whitespace)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alacritty_terminal::{Term, grid::Dimensions, term::Config, vte::ansi::Processor};
    use kettle_core::EventProxy;

    struct Size;
    impl Dimensions for Size {
        fn columns(&self) -> usize {
            80
        }
        fn screen_lines(&self) -> usize {
            8
        }
        fn total_lines(&self) -> usize {
            8
        }
    }

    pub(crate) fn fixture() -> (InlineCards, PaneSnapshot, InlineNonce) {
        let (cards, term, nonce) = fixture_term(0);
        let mut snap = PaneSnapshot::default();
        snap.capture_with_card_marks(&term, true);
        (cards, snap, nonce)
    }

    pub(crate) fn fixture_term(
        prefix_lines: usize,
    ) -> (InlineCards, Term<EventProxy>, InlineNonce) {
        let label = "  \u{23bf} \u{00a0}PostToolUse:mcp__plugin_kettle_kettle__kettle_show says:";
        harness_term(prefix_lines, CardHarness::ClaudeHook, label, 5)
    }

    /// A card as `harness` prints it: `label` on its own line, then the
    /// rows and the caption at column `left`.
    fn harness_term(
        prefix_lines: usize,
        harness: CardHarness,
        label: &str,
        left: usize,
    ) -> (InlineCards, Term<EventProxy>, InlineNonce) {
        let nonce = InlineNonce::new([2, 3, 4, 5, 6, 7]).unwrap();
        let mut cards = InlineCards::default();
        cards.register_for(nonce, 3, 12, harness);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut term = Term::new(
            Config::default(),
            &Size,
            EventProxy::new(tx, std::sync::Arc::new(|| {})),
        );
        let mut processor: Processor = Processor::new();
        let mut output = String::new();
        for index in 0..prefix_lines {
            output.push_str(&format!("ordinary prefix {index}\r\n"));
        }
        output.push_str(label);
        output.push_str("\r\n");
        let indent = " ".repeat(left);
        for row in 0..3 {
            output.push_str(&indent);
            for column in 0..12 {
                output.push_str(&InlineMarker { row, column, nonce }.encode().unwrap());
            }
            output.push_str("\r\n");
        }
        output.push_str(&indent);
        output.push_str("diagram.png - raster 640x480");
        processor.advance(&mut term, output.as_bytes());
        (cards, term, nonce)
    }

    fn spec(rows: u8, columns: u8, poster: Option<CardPoster>) -> CardSpec {
        CardSpec {
            rows,
            columns,
            harness: CardHarness::ClaudeHook,
            caption: "plot.png - raster 64x48".into(),
            poster,
            duration_ms: None,
        }
    }

    #[test]
    fn registration_refuses_bad_sizes_duplicates_and_a_full_pane() {
        let nonce = |n: u8| InlineNonce::new([0, 0, 0, 0, 0, n]).unwrap();
        let mut cards = InlineCards::default();
        for (rows, columns) in [
            (0, 12),
            (3, 0),
            (MAX_CARD_ROWS + 1, 12),
            (3, MAX_CARD_COLUMNS + 1),
        ] {
            assert_eq!(
                cards.insert(nonce(1), spec(rows, columns, None)),
                Err(CardRefusal::BadSize),
                "{rows}x{columns}"
            );
        }
        assert_eq!(
            cards.insert(nonce(1), spec(3, MAX_CARD_COLUMNS, None)),
            Ok(())
        );
        assert_eq!(
            cards.insert(nonce(1), spec(3, 12, None)),
            Err(CardRefusal::Duplicate)
        );
        for n in 2..=MAX_PANE_CARDS as u8 {
            assert_eq!(cards.insert(nonce(n), spec(3, 12, None)), Ok(()));
        }
        assert_eq!(cards.len(), MAX_PANE_CARDS);
        assert_eq!(
            cards.insert(nonce(MAX_PANE_CARDS as u8 + 1), spec(3, 12, None)),
            Err(CardRefusal::Full)
        );
        assert!(cards.remove(nonce(1)));
        assert!(!cards.remove(nonce(1)));
        assert!(!cards.contains(nonce(1)));
        assert_eq!(cards.insert(nonce(1), spec(3, 12, None)), Ok(()));
    }

    #[test]
    fn a_card_never_keeps_its_posters_pixels_once_the_shelf_lets_go() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let shelf = kettle_core::ImageData::new(2, 1, vec![255; 8]).unwrap();
        let mut cards = InlineCards::default();
        cards
            .insert(nonce, spec(3, 12, Some(CardPoster::new(&shelf))))
            .unwrap();
        assert!(matches!(cards.visual(nonce), Some(CardVisual::Ready(_))));
        let pixels = std::sync::Arc::downgrade(&shelf.rgba);
        drop(shelf);
        assert!(
            pixels.upgrade().is_none(),
            "the card held no strong reference"
        );
        assert!(matches!(cards.visual(nonce), Some(CardVisual::Failed)));
        // A card registered before its poster is pending, not failed.
        let other = InlineNonce::new([6, 5, 4, 3, 2, 1]).unwrap();
        cards.insert(other, spec(3, 12, None)).unwrap();
        assert!(matches!(cards.visual(other), Some(CardVisual::Pending)));
    }

    /// A card shows its item's pixels rendered again once told, holding
    /// them as weakly as the first; a card no one registered takes none.
    #[test]
    fn a_card_shows_its_items_pixels_rendered_again() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let first = kettle_core::ImageData::new(2, 1, vec![255; 8]).unwrap();
        let mut cards = InlineCards::default();
        cards
            .insert(nonce, spec(3, 12, Some(CardPoster::new(&first))))
            .unwrap();
        // Rendered again on another canvas, the item lets the first go.
        let again = kettle_core::ImageData::new(2, 1, vec![9; 8]).unwrap();
        drop(first);
        assert!(matches!(cards.visual(nonce), Some(CardVisual::Failed)));
        assert!(cards.set_poster(nonce, Some(&again)));
        assert!(
            matches!(cards.visual(nonce), Some(CardVisual::Ready(image)) if image.rgba.as_slice() == [9; 8])
        );
        let pixels = std::sync::Arc::downgrade(&again.rgba);
        drop(again);
        assert!(pixels.upgrade().is_none(), "held weakly");
        let stranger = InlineNonce::new([6, 5, 4, 3, 2, 1]).unwrap();
        let other = kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap();
        assert!(!cards.set_poster(stranger, Some(&other)));
        assert!(!cards.contains(stranger));
    }

    #[test]
    fn card_survives_real_scroll_when_header_and_upper_rows_leave_viewport() {
        use alacritty_terminal::grid::Scroll;
        let (cards, mut term, nonce) = fixture_term(0);
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut term,
            b"\r\nordinary 0\r\nordinary 1\r\nordinary 2\r\nordinary 3\r\nordinary 4\r\nordinary 5",
        );
        assert_eq!(term.grid().history_size(), 3);
        let mut snap = PaneSnapshot::default();
        for (scroll, visible_cells, view_row) in [
            (Scroll::Top, 36, 1),
            (Scroll::Delta(-1), 36, 0),
            (Scroll::Bottom, 12, -2),
        ] {
            term.scroll_display(scroll);
            snap.capture_with_card_marks(&term, true);
            let frame = cards.recognize(&snap);
            assert_eq!(frame.blocks.len(), 1, "offset {}", snap.display_offset);
            assert_eq!(frame.blocks[0].nonce, nonce);
            assert_eq!(frame.blocks[0].line + snap.display_offset as i32, view_row);
            assert_eq!(frame.accepted_cells.len(), visible_cells);
            assert!(
                frame
                    .accepted_cells
                    .iter()
                    .all(|&index| index < snap.cells.len())
            );
        }
        // The label is now above the viewport, but it still owns the visible row.
        term.grid_mut()[alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(-3),
            alacritty_terminal::index::Column(7),
        )]
        .c = 'X';
        snap.capture_with_card_marks(&term, true);
        assert!(cards.recognize(&snap).blocks.is_empty());
    }

    #[test]
    fn card_survives_real_scroll_when_lower_rows_and_caption_are_below_viewport() {
        use alacritty_terminal::grid::Scroll;
        let (cards, mut term, nonce) = fixture_term(6);
        term.scroll_display(Scroll::Top);
        let mut snap = PaneSnapshot::default();
        snap.capture_with_card_marks(&term, true);
        let frame = cards.recognize(&snap);
        assert_eq!(frame.blocks.len(), 1);
        assert_eq!(frame.blocks[0].nonce, nonce);
        assert_eq!(frame.blocks[0].line + snap.display_offset as i32, 7);
        assert_eq!(frame.accepted_cells.len(), 12);
        // The caption and final gutter are outside the visible cell vector.
        let caption = alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(frame.blocks[0].line + 3),
            alacritty_terminal::index::Column(7),
        );
        let original = term.grid()[caption].c;
        term.grid_mut()[caption].c = 'X';
        snap.capture_with_card_marks(&term, true);
        assert!(cards.recognize(&snap).blocks.is_empty());
        term.grid_mut()[caption].c = original;
        let gutter = alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(frame.blocks[0].line + 2),
            alacritty_terminal::index::Column(2),
        );
        term.grid_mut()[gutter].c = 'X';
        snap.capture_with_card_marks(&term, true);
        assert!(cards.recognize(&snap).blocks.is_empty());
    }

    #[test]
    fn pane_and_cursor_share_projected_placeholder_glyphs() {
        for hidden in [false, true] {
            assert_eq!(CardCell::Accepted.glyph('\u{10eeee}', hidden), ' ');
            assert_eq!(CardCell::Neutral.glyph('\u{10eeee}', hidden), '\u{25a1}');
            assert_eq!(CardCell::Unknown.glyph('\u{10eeee}', hidden), '\u{2b1a}');
        }
        assert_eq!(CardCell::Ordinary.glyph('x', false), 'x');
        assert_eq!(CardCell::Ordinary.glyph('x', true), ' ');
    }

    #[test]
    fn full_registered_rectangle_from_real_terminal_is_recognized_without_color_input() {
        let (cards, mut snap, nonce) = fixture();
        assert!(cards.needs_marks());
        let first = cards.recognize(&snap);
        assert_eq!(first.blocks.len(), 1);
        assert_eq!(first.blocks[0].nonce, nonce);
        assert_eq!((first.blocks[0].line, first.blocks[0].column), (1, 5));
        assert_eq!((first.blocks[0].rows, first.blocks[0].columns), (3, 12));
        assert_eq!(first.accepted_cells.len(), 36);
        for cell in &mut snap.cells {
            cell.fg = kettle_core::AnsiColor::Named(kettle_core::NamedColor::Red);
        }
        assert_eq!(cards.recognize(&snap).accepted_cells, first.accepted_cells);
    }

    /// Codex prints a hook message as `↳ Hook ·` and the message's first
    /// line, then its other lines four columns in (Codex CLI 0.162.0).
    /// A caller can place each registered card a snapshot shows whole, by
    /// the same recognition a frame draws with; an unregistered one is not
    /// a card.
    #[test]
    fn placements_name_each_registered_card_the_grid_shows() {
        let (cards, snap, nonce) = fixture();
        let placements = cards.placements(&snap);
        let frame = cards.recognize(&snap);
        assert_eq!(placements.len(), frame.blocks.len());
        assert_eq!(placements.len(), 1);
        let block = &frame.blocks[0];
        assert_eq!(
            placements[0],
            CardPlacement {
                nonce,
                line: block.line,
                column: block.column,
                rows: block.rows,
                columns: block.columns,
            }
        );
        assert!(InlineCards::default().placements(&snap).is_empty());
        // Recognition reads the marks a snapshot collects only when asked to.
        let (_, term, _) = fixture_term(0);
        let mut plain = PaneSnapshot::default();
        plain.capture(&term);
        assert!(cards.placements(&plain).is_empty());
    }

    #[test]
    fn a_codex_hook_card_is_recognized_at_its_own_pin() {
        let codex = |label: &str, left: usize| {
            let (cards, term, nonce) = harness_term(0, CardHarness::CodexHook, label, left);
            let mut snap = PaneSnapshot::default();
            snap.capture_with_card_marks(&term, true);
            (cards.recognize(&snap), nonce)
        };
        let (frame, nonce) = codex("\u{21b3} Hook \u{00b7} ", 4);
        assert_eq!(frame.blocks.len(), 1);
        assert_eq!(frame.blocks[0].nonce, nonce);
        assert_eq!((frame.blocks[0].line, frame.blocks[0].column), (1, 4));
        assert_eq!(frame.accepted_cells.len(), 36);
        // Its provenance tab names Codex.
        assert_eq!(frame.blocks[0].harness, CardHarness::CodexHook);
        assert_eq!(frame.blocks[0].harness.name(), "Codex");
        // Claude Code's label, Claude Code's column, or text after the
        // label is not Codex's hook.
        for (label, left) in [
            (
                "  \u{23bf} \u{00a0}PostToolUse:mcp__plugin_kettle_kettle__kettle_show says:",
                4,
            ),
            ("\u{21b3} Hook \u{00b7}", 5),
            ("\u{21b3} Hook \u{00b7} something", 4),
            ("\u{21b3} Hook", 4),
        ] {
            assert!(
                codex(label, left).0.blocks.is_empty(),
                "{label:?} at {left}"
            );
        }
        // Nor is Codex's label a Claude Code card's.
        let (cards, term, _) =
            harness_term(0, CardHarness::ClaudeHook, "\u{21b3} Hook \u{00b7}", 5);
        let mut snap = PaneSnapshot::default();
        snap.capture_with_card_marks(&term, true);
        assert!(cards.recognize(&snap).blocks.is_empty());
    }

    #[test]
    fn unknown_registration_leaves_all_placeholder_cells_unsuppressed() {
        let (_, snap, _) = fixture();
        let frame = InlineCards::default().recognize(&snap);
        assert!(frame.blocks.is_empty());
        assert!((0..snap.cells.len()).all(|index| !frame.suppress_cell(index)));
    }

    #[test]
    fn overwrite_on_the_next_frame_removes_the_whole_previous_poster_footprint() {
        let (cards, mut snap, _) = fixture();
        assert_eq!(cards.recognize(&snap).accepted_cells.len(), 36);
        let index = snap
            .cells
            .iter()
            .position(|cell| (cell.line, cell.col) == (2, 8))
            .unwrap();
        snap.cells[index].c = 'X';
        assert!(cards.recognize(&snap).blocks.is_empty());
    }

    #[test]
    fn changed_gutter_label_or_caption_never_suppresses_neighboring_text() {
        let (cards, snap, _) = fixture();
        for point in [(2, 0), (0, 7), (4, 7)] {
            let mut altered = fixture().1;
            let index = altered
                .cells
                .iter()
                .position(|cell| (cell.line, cell.col) == point)
                .unwrap();
            altered.cells[index].c = 'X';
            assert!(cards.recognize(&altered).blocks.is_empty());
        }
        assert_eq!(cards.recognize(&snap).blocks.len(), 1);
    }

    #[test]
    fn scrolling_uses_grid_absolute_rows_and_visible_offset() {
        let (cards, mut snap, _) = fixture();
        for cell in &mut snap.cells {
            cell.line -= 12;
        }
        snap.display_offset = 12;
        let frame = cards.recognize(&snap);
        assert_eq!(frame.blocks.len(), 1);
        assert_eq!(frame.blocks[0].line, -11);
    }

    #[test]
    fn incomplete_mark_collection_degrades_as_a_whole() {
        let (cards, mut incomplete, _) = fixture();
        incomplete.card_marks.clear();
        assert!(cards.recognize(&incomplete).blocks.is_empty());
    }
    #[test]
    fn pooled_frame_clears_old_acceptance_and_retains_its_storage() {
        let (cards, mut snap, _) = fixture();
        let mut frame = CardFrame::default();
        cards.recognize_into(&snap, &mut frame);
        assert_eq!(frame.blocks.len(), 1);
        let capacity = (
            frame.blocks.capacity(),
            frame.accepted_cells.capacity(),
            frame.groups.capacity(),
        );
        let index = snap
            .cells
            .iter()
            .position(|cell| (cell.line, cell.col) == (2, 8))
            .unwrap();
        snap.cells[index].c = 'X';
        cards.recognize_into(&snap, &mut frame);
        assert!(frame.blocks.is_empty());
        assert!(frame.accepted_cells.is_empty());
        assert_eq!(
            capacity,
            (
                frame.blocks.capacity(),
                frame.accepted_cells.capacity(),
                frame.groups.capacity()
            )
        );
    }
    #[test]
    fn torn_known_card_has_neutral_cells_and_does_not_claim_overwritten_text() {
        let (cards, mut snap, _) = fixture();
        let index = snap
            .cells
            .iter()
            .position(|cell| (cell.line, cell.col) == (2, 8))
            .unwrap();
        snap.cells[index].c = 'X';
        let frame = cards.recognize(&snap);
        assert!(frame.blocks.is_empty());
        assert_eq!(frame.cell(&snap, index), CardCell::Ordinary);
        let neighbor = snap
            .cells
            .iter()
            .position(|cell| (cell.line, cell.col) == (2, 9))
            .unwrap();
        assert_eq!(frame.cell(&snap, neighbor), CardCell::Neutral);
        assert!(!frame.suppress_point(&snap, 2, 9));
        let unknown = InlineCards::default().recognize(&snap);
        assert_eq!(unknown.cell(&snap, neighbor), CardCell::Unknown);
    }

    #[test]
    fn only_complete_accepted_coordinates_suppress_a_cursor() {
        let (cards, snap, _) = fixture();
        let frame = cards.recognize(&snap);
        assert!(frame.suppress_point(&snap, 1, 5));
        assert!(frame.suppress_point(&snap, 3, 16));
        assert!(!frame.suppress_point(&snap, 1, 4));
        assert!(!frame.suppress_point(&snap, 4, 5));
        assert!(!frame.suppress_point(&snap, -10, 5));
    }
}
