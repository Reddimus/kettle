//! Bounded display-only snapshots of primary-screen text. No PTY input is replayed.

use std::fmt::Write as _;

use crate::{AnsiColor, Column, Dimensions, Flags, Line, Point};
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::GridCell;
use alacritty_terminal::term::Term;

pub const MAX_RESTORE_LINES: usize = 10_000;
pub const MAX_RESTORE_LINE_BYTES: usize = 16 * 1024;
/// SGR text, soft-wrap state, and (UTF-8 end offset, grapheme mode) per cell.
/// Boundaries prevent a new terminal's mode from changing saved cell widths.
pub type RestoreRow = (String, bool, Vec<(usize, bool)>);

/// The document row (see `Grid::history_origin`) of the oldest row the primary
/// grid still holds. Rows only ever leave from this end: scrolled out of
/// bounded history, cleared with the scrollback (`CSI 3 J`) or reset (`RIS`).
pub fn primary_first_row<T: EventListener>(term: &Term<T>) -> u64 {
    term.primary_grid().history_origin()
}

/// The document row just past the bottom of the primary screen: every row the
/// primary grid holds now comes before it, and every row added later comes at
/// or after it.
pub fn primary_end_row<T: EventListener>(term: &Term<T>) -> u64 {
    let grid = term.primary_grid();
    grid.history_origin() + grid.history_size() as u64 + grid.screen_lines() as u64
}

/// Generate SGR from cell state, never from raw output. OSC, DCS, links, cursor
/// movement and device requests cannot cross this boundary. Budget includes JSON
/// string escaping so the persistence envelope can enforce a total file cap.
pub fn snapshot<T: EventListener>(
    term: &Term<T>,
    limit: usize,
    budget: &mut usize,
) -> Vec<RestoreRow> {
    snapshot_from(term, limit, 0, budget)
}

/// [`snapshot`], leaving out every row before document row `first` (see
/// [`primary_end_row`]): rows a remote session wrote before it ended.
pub fn snapshot_from<T: EventListener>(
    term: &Term<T>,
    limit: usize,
    first: u64,
    budget: &mut usize,
) -> Vec<RestoreRow> {
    if limit == 0 || *budget < 16 {
        return Vec::new();
    }
    let grid = term.primary_grid();
    let mut end = grid.screen_lines() as i32;
    while end > grid.cursor.point.line.0 + 1
        && (0..grid.columns()).all(|col| {
            let cell = &grid[Point::new(Line(end - 1), Column(col))];
            cell.is_empty()
        })
    {
        end -= 1;
    }
    // Document row `first` sits at line `first - origin - history`.
    let first_line = i64::try_from(first.saturating_sub(grid.history_origin()))
        .unwrap_or(i64::MAX)
        .saturating_sub(grid.history_size() as i64)
        .min(i64::from(i32::MAX)) as i32;
    let start = (end - limit.min(MAX_RESTORE_LINES) as i32)
        .max(-(grid.history_size() as i32))
        .max(first_line);
    let mut rows = Vec::new();
    for row in (start..end).rev() {
        let mut line = String::new();
        let mut cells = Vec::new();
        let mut previous = None;
        let mut wrapped = grid[Point::new(Line(row), Column(grid.columns() - 1))]
            .flags
            .contains(Flags::WRAPLINE);
        let last = (0..grid.columns()).rfind(|&col| {
            let cell = &grid[Point::new(Line(row), Column(col))];
            wrapped || !cell.is_empty()
        });
        'cells: for col in 0..last.map_or(0, |col| col + 1) {
            let cell = &grid[Point::new(Line(row), Column(col))];
            if crate::grid_text::is_spacer(cell.flags) {
                continue;
            }
            let style = (
                cell.fg,
                cell.bg,
                cell.underline_color(),
                cell.flags
                    & (Flags::BOLD
                        | Flags::DIM
                        | Flags::ITALIC
                        | Flags::ALL_UNDERLINES
                        | Flags::INVERSE
                        | Flags::HIDDEN
                        | Flags::STRIKEOUT
                        | Flags::OVERLINE),
            );
            let mut part = String::new();
            if previous != Some(style) {
                part.push_str("\x1b[0");
                for (flag, sgr) in [
                    (Flags::BOLD, ";1"),
                    (Flags::DIM, ";2"),
                    (Flags::ITALIC, ";3"),
                    (Flags::UNDERLINE, ";4"),
                    (Flags::DOUBLE_UNDERLINE, ";4:2"),
                    (Flags::UNDERCURL, ";4:3"),
                    (Flags::DOTTED_UNDERLINE, ";4:4"),
                    (Flags::DASHED_UNDERLINE, ";4:5"),
                    (Flags::INVERSE, ";7"),
                    (Flags::HIDDEN, ";8"),
                    (Flags::STRIKEOUT, ";9"),
                    (Flags::OVERLINE, ";53"),
                ] {
                    if style.3.contains(flag) {
                        part.push_str(sgr);
                    }
                }
                color(&mut part, cell.fg, 38);
                color(&mut part, cell.bg, 48);
                if let Some(underline) = cell.underline_color() {
                    color(&mut part, underline, 58);
                }
                part.push('m');
                previous = Some(style);
            }
            let (ch, marks) =
                crate::inline_text::text_parts(cell.c, cell.zerowidth().unwrap_or_default());
            for ch in std::iter::once(ch).chain(marks.iter().copied()) {
                if line.len() + part.len() + ch.len_utf8() + 4 > MAX_RESTORE_LINE_BYTES {
                    wrapped = false;
                    break 'cells;
                }
                if !ch.is_control() {
                    part.push(ch);
                }
            }
            if !part.is_empty() {
                line.push_str(&part);
                cells.push((line.len(), cell.flags.contains(Flags::GRAPHEME)));
            }
        }
        line.push_str("\x1b[0m");
        let cost = line.len()
            + line.bytes().filter(|&b| b == 0x1b).count() * 5
            + line.bytes().filter(|&b| b == b'"' || b == b'\\').count()
            + 16
            + cells
                .iter()
                .map(|(end, _)| end.ilog10() as usize + 11)
                .sum::<usize>();
        if cost > *budget {
            break;
        }
        *budget -= cost;
        rows.push((line, wrapped, cells));
    }
    rows.reverse();
    rows
}

fn color(out: &mut String, color: AnsiColor, base: usize) {
    match color {
        AnsiColor::Spec(rgb) => {
            let _ = write!(out, ";{base};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
        AnsiColor::Indexed(index) => {
            let _ = write!(out, ";{base};5;{index}");
        }
        AnsiColor::Named(named) if (named as usize) < 16 => {
            let n = named as usize;
            if base == 58 {
                let _ = write!(out, ";58;5;{n}");
                return;
            }
            let code = if n < 8 {
                base - 8 + n
            } else {
                base + 52 + n - 8
            };
            let _ = write!(out, ";{code}");
        }
        _ => {}
    }
}

/// Only printable UTF-8 and bounded, numeric SGR are accepted from disk.
/// No controls, line breaks, OSC hyperlinks, queries or other escape forms.
pub fn valid_line(line: &str) -> bool {
    if line.len() > MAX_RESTORE_LINE_BYTES {
        return false;
    }
    let mut rest = line;
    while let Some(pos) = rest.find('\x1b') {
        if rest[..pos].chars().any(char::is_control) {
            return false;
        }
        let Some(sgr) = rest[pos..].strip_prefix("\x1b[") else {
            return false;
        };
        let Some(end) = sgr.find('m') else {
            return false;
        };
        if end > 128
            || !sgr[..end]
                .bytes()
                .all(|c| c.is_ascii_digit() || c == b';' || c == b':')
        {
            return false;
        }
        rest = &sgr[end + 1..];
    }
    !rest.chars().any(char::is_control)
}

/// Validate cell boundaries before slicing or generating trusted mode changes.
pub fn valid_row(row: &RestoreRow) -> bool {
    let (text, _, cells) = row;
    if !valid_line(text) || cells.len() > text.len() {
        return false;
    }
    let mut start = 0;
    for &(end, _) in cells {
        if end <= start
            || end > text.len()
            || !text.is_char_boundary(end)
            || !valid_line(&text[start..end])
        {
            return false;
        }
        start = end;
    }
    valid_line(&text[start..])
}

fn dimmed(output: &mut String, line: &str) {
    output.push_str("\x1b[2m");
    let mut rest = line;
    while let Some(start) = rest.find('\x1b') {
        output.push_str(&rest[..start]);
        let sgr = &rest[start..];
        let end = sgr.find('m').expect("validated SGR") + 1;
        output.push_str(&sgr[..end]);
        output.push_str("\x1b[2m");
        rest = &sgr[end..];
    }
    output.push_str(rest);
}

/// Generate display-only parser input. Cell mode changes are generated from
/// validated metadata, never accepted as escape sequences from disk. Restore
/// the new shell's configured mode and attributes after seeding old cells.
pub fn replay(
    lines: &[RestoreRow],
    divider: &str,
    columns: usize,
    grapheme_clustering: bool,
) -> Option<String> {
    if lines.is_empty() || lines.len() > MAX_RESTORE_LINES || !lines.iter().all(valid_row) {
        return None;
    }
    let mut output = String::new();
    for (index, (line, wrapped, cells)) in lines.iter().enumerate() {
        let mut start = 0;
        for &(end, grapheme) in cells {
            output.push_str(if grapheme {
                "\x1b[?2027l\x1b[?2027h"
            } else {
                "\x1b[?2027l"
            });
            dimmed(&mut output, &line[start..end]);
            start = end;
        }
        dimmed(&mut output, &line[start..]);
        if !wrapped || index + 1 == lines.len() {
            output.push_str("\r\n");
        }
    }
    output.push_str(if grapheme_clustering {
        "\x1b[?2027h"
    } else {
        "\x1b[?2027l"
    });
    output.push_str("\x1b[0m");
    let mut width = 0;
    for ch in divider.chars().filter(|c| !c.is_control()) {
        width += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width > columns {
            break;
        }
        output.push(ch);
    }
    output.push_str("\r\n");
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::TermSize;
    use crate::{EventProxy, NamedColor, Waker};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::{Processor as GenericProcessor, StdSyncHandler};
    type Processor = GenericProcessor<StdSyncHandler>;
    use std::sync::Arc;

    fn terminal() -> Term<EventProxy> {
        let (tx, _) = crossbeam_channel::unbounded();
        let waker: Waker = Arc::new(|| {});
        Term::new(
            Config {
                scrolling_history: 20_000,
                grapheme_clustering: true,
                ..Config::default()
            },
            &TermSize::new(40, 4),
            EventProxy::new(tx, waker),
        )
    }

    fn texts(rows: &[RestoreRow]) -> Vec<String> {
        rows.iter().map(|row| row.0.clone()).collect()
    }

    /// Rows a remote session wrote stay out of a snapshot taken from its end
    /// row, whatever happened since, and the grid gives them up only from its
    /// old end: a scrollback clear that leaves them on screen keeps them, and
    /// a reset or eviction does not.
    #[test]
    fn rows_before_a_remote_boundary_stay_out_until_they_leave_the_grid() {
        let mut budget = usize::MAX;
        let mut term = terminal();
        let mut parser = Processor::new();
        parser.advance(&mut term, b"REMOTE-1\r\nREMOTE-2\r\n");
        let end = primary_end_row(&term);
        assert_eq!(primary_first_row(&term), 0);
        assert_eq!(end, 4, "one past the bottom of a 4-row screen");
        parser.advance(
            &mut term,
            b"local-1\r\nlocal-2\r\nlocal-3\r\nlocal-4\r\nlocal-5\r\n",
        );
        let all = texts(&snapshot(&term, 100, &mut budget));
        assert!(all.iter().any(|row| row.contains("REMOTE-1")), "{all:?}");
        let local = texts(&snapshot_from(&term, 100, end, &mut budget));
        assert!(local.iter().all(|row| !row.contains("REMOTE")), "{local:?}");
        // Rows that were on screen when it ended may hold its output too:
        // they stay out, and the rows after them save.
        assert!(
            local
                .iter()
                .all(|row| !row.contains("local-1") && !row.contains("local-2"))
        );
        assert!(local.iter().any(|row| row.contains("local-5")), "{local:?}");
        assert!(
            primary_first_row(&term) < end,
            "the remote rows are still there"
        );

        // A scrollback clear with the remote rows still on screen keeps them.
        let mut term = terminal();
        parser.advance(&mut term, b"REMOTE-1\r\n");
        let end = primary_end_row(&term);
        parser.advance(&mut term, b"local\r\n\x1b[3J");
        assert!(primary_first_row(&term) < end);
        let rows = texts(&snapshot_from(&term, 100, end, &mut budget));
        assert!(rows.iter().all(|row| !row.contains("REMOTE")), "{rows:?}");

        // A reset takes every row the grid held.
        parser.advance(&mut term, b"\x1bc");
        assert!(primary_first_row(&term) >= end);

        // So does eviction from bounded history.
        let (tx, _) = crossbeam_channel::unbounded();
        let waker: Waker = Arc::new(|| {});
        let mut small = Term::new(
            Config {
                scrolling_history: 2,
                ..Config::default()
            },
            &TermSize::new(40, 4),
            EventProxy::new(tx, waker),
        );
        parser.advance(&mut small, b"REMOTE\r\n");
        let end = primary_end_row(&small);
        for _ in 0..8 {
            parser.advance(&mut small, b"local\r\n");
        }
        assert!(primary_first_row(&small) >= end);
        assert!(
            texts(&snapshot_from(&small, 100, end, &mut budget))
                .iter()
                .any(|row| row.contains("local"))
        );
    }

    #[test]
    fn restore_round_trip_attributes_wide_graphemes_and_order() {
        let mut budget = usize::MAX;
        let mut source = terminal();
        Processor::new().advance(
            &mut source,
            "\x1b[1;3;4;38;2;2;45;190;48;5;18;58;5;22m世界 e\u{301} 👩‍💻\x1b[0m".as_bytes(),
        );
        let saved = snapshot(&source, 1000, &mut budget);
        let mut restored = terminal();
        let mut parser = Processor::new();
        parser.advance(
            &mut restored,
            replay(&saved, "Restored divider", 40, true)
                .unwrap()
                .as_bytes(),
        );
        parser.advance(&mut restored, b"new shell output");
        let original = &source.grid()[Point::new(Line(0), Column(0))];
        let cell = &restored.grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, original.c);
        assert_eq!(cell.fg, original.fg);
        assert_eq!(cell.bg, original.bg);
        assert_eq!(cell.underline_color(), original.underline_color());
        for col in 0..10 {
            let before = &source.grid()[Point::new(Line(0), Column(col))];
            let after = &restored.grid()[Point::new(Line(0), Column(col))];
            assert_eq!(after.c, before.c);
            assert_eq!(after.zerowidth(), before.zerowidth());
            assert_eq!(
                after.flags & (Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER | Flags::GRAPHEME),
                before.flags & (Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER | Flags::GRAPHEME)
            );
        }
        assert!(cell.flags.contains(
            Flags::BOLD | Flags::ITALIC | Flags::UNDERLINE | Flags::DIM | Flags::WIDE_CHAR
        ));
        assert!(
            restored.grid()[Point::new(Line(0), Column(1))]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
        assert_eq!(
            restored.grid()[Point::new(Line(0), Column(5))].zerowidth(),
            source.grid()[Point::new(Line(0), Column(5))].zerowidth()
        );
        assert_eq!(restored.grid()[Point::new(Line(1), Column(0))].c, 'R');
        assert_eq!(restored.grid()[Point::new(Line(2), Column(0))].c, 'n');
        assert!(
            !restored.grid()[Point::new(Line(2), Column(0))]
                .flags
                .contains(Flags::DIM)
        );
    }

    #[test]
    fn restore_caps_and_primary_screen_only() {
        let mut budget = usize::MAX;
        let mut term = terminal();
        Processor::new().advance(&mut term, b"old\r\nrecent\r\nlatest");
        let rows = snapshot(&term, 2, &mut budget);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0.contains("recent"));
        assert!(snapshot(&term, 0, &mut budget).is_empty());
        assert!(snapshot(&term, 1000, &mut 1).is_empty());
        Processor::new().advance(&mut term, b"\x1b[?1049hsecret alternate");
        assert_eq!(rows, snapshot(&term, 2, &mut budget));
    }

    #[test]
    fn restore_drops_links_and_controls_and_rejects_hostile_lines() {
        let mut budget = usize::MAX;
        let mut term = terminal();
        Processor::new().advance(
            &mut term,
            b"\x1b]8;;https://private.invalid\x1b\\safe\x1b]8;;\x1b\\\x1b]52;c;payload\x07\x1b[6n",
        );
        let lines = snapshot(&term, 1000, &mut budget);
        assert!(lines.iter().all(|s| valid_line(&s.0)));
        assert!(
            !lines
                .iter()
                .map(|row| row.0.as_str())
                .collect::<String>()
                .contains("private")
        );
        for line in [
            "\x1b]52;c;payload\x07",
            "\x1b[6n",
            "bad\rtext",
            "bad\u{85}",
            "\x1b[31",
            "\x1b[?1m",
        ] {
            assert!(!valid_line(line), "{line:?}");
        }
        assert!(!valid_line(&"x".repeat(MAX_RESTORE_LINE_BYTES + 1)));
    }
    #[test]
    fn restored_cells_preserve_mixed_grapheme_modes_and_reset_for_the_shell() {
        let mut source = terminal();
        Processor::new().advance(&mut source, "\x1b[?2027l👩‍💻 \x1b[?2027h👩‍💻".as_bytes());
        let mut budget = usize::MAX;
        let rows = snapshot(&source, 1000, &mut budget);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut target = Term::new(
            Config::default(),
            &TermSize::new(40, 4),
            EventProxy::new(tx, Arc::new(|| {})),
        );
        let mut parser = Processor::new();
        parser.advance(
            &mut target,
            replay(&rows, "divider", 40, false).unwrap().as_bytes(),
        );
        for col in 0..7 {
            let before = &source.grid()[Point::new(Line(0), Column(col))];
            let after = &target.grid()[Point::new(Line(0), Column(col))];
            assert_eq!(before.c, after.c, "column {col}");
            assert_eq!(before.zerowidth(), after.zerowidth(), "column {col}");
            let mask = Flags::GRAPHEME | Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER;
            assert_eq!(before.flags & mask, after.flags & mask, "column {col}");
        }
        parser.advance(&mut target, "👩‍💻".as_bytes());
        assert_eq!(
            target.grid().cursor.point.column.0,
            4,
            "new shell keeps its configured mode"
        );
    }

    #[test]
    fn corrupt_cell_boundaries_are_rejected_without_slicing_or_parsing() {
        for row in [
            ("é".into(), false, vec![(1, true)]),
            ("abc".into(), false, vec![(4, true)]),
            ("abc".into(), false, vec![(2, true), (1, false)]),
            ("\x1b[31mred".into(), false, vec![(2, true)]),
        ] {
            assert!(!valid_row(&row));
            assert!(replay(&[row], "divider", 40, true).is_none());
        }
    }

    #[test]
    fn restored_styled_trailing_spaces_keep_their_visible_attributes() {
        let mut source = terminal();
        Processor::new().advance(&mut source, b"\x1b[4;32m   \x1b[0m");
        let mut budget = usize::MAX;
        let rows = snapshot(&source, 1000, &mut budget);
        let mut target = terminal();
        Processor::new().advance(
            &mut target,
            replay(&rows, "divider", 40, true).unwrap().as_bytes(),
        );
        for col in 0..3 {
            let cell = &target.grid()[Point::new(Line(0), Column(col))];
            assert!(cell.flags.contains(Flags::UNDERLINE));
            assert_eq!(cell.fg, AnsiColor::Named(NamedColor::Green));
        }
    }

    #[test]
    fn restored_sgr_resets_cannot_remove_faint_styling() {
        let mut term = terminal();
        let rows = vec![("\x1b[mplain\x1b[22m text".into(), false, vec![])];
        Processor::new().advance(
            &mut term,
            replay(&rows, "divider", 40, true).unwrap().as_bytes(),
        );
        assert!(
            term.grid()[Point::new(Line(0), Column(0))]
                .flags
                .contains(Flags::DIM)
        );
        assert!(
            term.grid()[Point::new(Line(0), Column(6))]
                .flags
                .contains(Flags::DIM)
        );
        assert!(
            !term.grid()[Point::new(Line(1), Column(0))]
                .flags
                .contains(Flags::DIM)
        );
    }

    #[test]
    fn restore_line_count_and_serialized_byte_budget() {
        let mut term = terminal();
        let text = "row\r\n".repeat(MAX_RESTORE_LINES + 20);
        Processor::new().advance(&mut term, text.as_bytes());
        let mut budget = usize::MAX;
        assert_eq!(
            snapshot(&term, usize::MAX, &mut budget).len(),
            MAX_RESTORE_LINES
        );
        let mut budget = 512;
        let rows = snapshot(&term, 1000, &mut budget);
        assert!(!rows.is_empty());
        assert!(serde_json::to_vec(&rows).unwrap().len() <= 512);
        let mut quoted = terminal();
        Processor::new().advance(&mut quoted, b"\"\\\"\\\"");
        let mut budget = 64;
        let rows = snapshot(&quoted, 1000, &mut budget);
        assert!(serde_json::to_vec(&rows).unwrap().len() <= 64);
    }

    #[test]
    fn restore_row_cap_keeps_complete_graphemes() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(512, 4),
            EventProxy::new(tx, Arc::new(|| {})),
        );
        for col in 0..512 {
            let cell = &mut term.grid_mut()[Point::new(Line(0), Column(col))];
            cell.c = 'e';
            for _ in 0..30 {
                cell.push_zerowidth('\u{301}');
            }
        }
        let mut budget = usize::MAX;
        let rows = snapshot(&term, 1000, &mut budget);
        let text = &rows[0].0;
        assert!(text.len() <= MAX_RESTORE_LINE_BYTES);
        let bases = text.chars().filter(|&c| c == 'e').count();
        let marks = text.chars().filter(|&c| c == '\u{301}').count();
        assert!(bases > 0 && bases < 512);
        assert_eq!(marks, bases * 30, "never cut a grapheme in half");
    }

    #[test]
    fn restored_scrollback_is_searchable_selectable_and_copyable() {
        use crate::{Selection, SelectionType, Side};
        let text = "needle世界 e\u{301} 👩‍💻";
        let lines: Vec<RestoreRow> = ["before", text, "after", "four", "five"]
            .into_iter()
            .map(|s| (s.into(), false, vec![]))
            .collect();
        let mut term = terminal();
        Processor::new().advance(
            &mut term,
            replay(&lines, "Restored divider", 40, true)
                .unwrap()
                .as_bytes(),
        );
        let matches = crate::search(&term, "needle");
        assert_eq!(matches.len(), 1);
        assert!(matches[0].line < 0, "fixture must search retained history");
        let point = Point::new(Line(matches[0].line), Column(0));
        let mut selection = Selection::new(SelectionType::Lines, point, Side::Left);
        selection.update(Point::new(point.line, Column(39)), Side::Right);
        term.selection = Some(selection);
        assert_eq!(term.selection_to_string().unwrap().trim_end(), text);
    }

    #[test]
    fn restore_preserves_soft_wraps_and_clips_the_divider_to_one_row() {
        let text = "x".repeat(48);
        let mut source = terminal();
        Processor::new().advance(&mut source, text.as_bytes());
        let mut budget = usize::MAX;
        let saved = snapshot(&source, 1000, &mut budget);
        assert!(saved[0].1);
        let mut restored = terminal();
        Processor::new().advance(
            &mut restored,
            replay(&saved, "divider".repeat(20).as_str(), 40, true)
                .unwrap()
                .as_bytes(),
        );
        assert_eq!(restored.grid().cursor.point.line.0, 3);
        assert!(
            restored.grid()[Point::new(Line(0), Column(39))]
                .flags
                .contains(Flags::WRAPLINE)
        );
        use crate::{Selection, SelectionType, Side};
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(1), Column(7)), Side::Right);
        restored.selection = Some(selection);
        assert_eq!(restored.selection_to_string().unwrap(), text);
    }

    #[cfg(unix)]
    fn spawn_seeded(seed: &str, script: &str) -> crate::Terminal {
        use crate::{
            CursorShape, LaunchOverrides, PtyGeometry, Terminal, TerminalCapabilities,
            WorkingDirectoryPolicy,
        };
        let (tx, _rx) = crossbeam_channel::unbounded();
        Terminal::new_with_launch_environment(
            &["/bin/sh".into(), "-c".into(), script.into()],
            None,
            1000,
            0,
            PtyGeometry::from_cell_size(40, 6, 8, 16),
            false,
            CursorShape::Block,
            None,
            "xterm-256color",
            "truecolor",
            &[],
            &LaunchOverrides {
                initial_content: Some(seed),
                ..Default::default()
            },
            false,
            false,
            &crate::shell_startup::ShellStartup::default(),
            TerminalCapabilities::default(),
            WorkingDirectoryPolicy::FallbackToHome,
            tx,
            Arc::new(|| {}),
            None,
        )
        .expect("restore PTY fixture")
    }

    #[cfg(unix)]
    fn wait_for_row(terminal: &crate::Terminal, row: i32, first: char) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while terminal.term.lock().unwrap().grid()[Point::new(Line(row), Column(0))].c != first {
            assert!(
                std::time::Instant::now() < deadline,
                "child output never reached row {row}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn one_row_seed() -> String {
        replay(
            &[("saved output".into(), false, vec![])],
            "Restored divider",
            40,
            false,
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn restore_is_seeded_before_the_real_child_can_write() {
        let terminal = spawn_seeded(&one_row_seed(), "printf 'new shell output'; read wait");
        wait_for_row(&terminal, 2, 'n');
        let grid = terminal.term.lock().unwrap();
        assert_eq!(grid.grid()[Point::new(Line(0), Column(0))].c, 's');
        assert_eq!(grid.grid()[Point::new(Line(1), Column(0))].c, 'R');
        assert!(
            grid.grid()[Point::new(Line(0), Column(0))]
                .flags
                .contains(Flags::DIM)
        );
        assert!(
            !grid.grid()[Point::new(Line(2), Column(0))]
                .flags
                .contains(Flags::DIM)
        );
    }

    /// Runs on every platform, Windows CI included: Windows parses the seed
    /// but leaves the guard unarmed, so a startup clear behaves as before.
    #[test]
    fn restore_seed_arms_the_clear_guard_except_on_windows() {
        let mut term = terminal();
        let input = Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::term::seed_restored_content(&mut term, &one_row_seed(), &input);
        assert_eq!(term.grid().cursor.point, Point::new(Line(2), Column(0)));
        assert_eq!(term.restore_clear_guard_armed(), !cfg!(windows));
        Processor::new().advance(&mut term, b"\x1b[H\x1b[2Jprompt$ ");
        let first: String = (0..3)
            .map(|row| term.grid()[Point::new(Line(row), Column(0))].c)
            .collect();
        if cfg!(windows) {
            assert_eq!(first, "p  ");
            assert_eq!(term.history_size(), 2);
        } else {
            assert_eq!(first, "sRp");
            assert_eq!(term.history_size(), 0);
            assert!(
                term.grid()[Point::new(Line(0), Column(0))]
                    .flags
                    .contains(Flags::DIM)
            );
        }
    }

    /// A shell that clears at startup, through the real spawn and reader
    /// path, in the byte orders `tput clear`, macOS `clear` and ncurses 6.6
    /// `clear` send.
    #[cfg(unix)]
    #[test]
    fn restore_survives_a_startup_clear_in_each_byte_order() {
        for clear in [
            "\\033[H\\033[2J",
            "\\033[3J\\033[H\\033[2J",
            "\\033[H\\033[2J\\033[3J",
        ] {
            let terminal = spawn_seeded(
                &one_row_seed(),
                &format!("printf '{clear}prompt$ '; read wait"),
            );
            wait_for_row(&terminal, 2, 'p');
            let term = terminal.term.lock().unwrap();
            assert_eq!(
                term.grid()[Point::new(Line(0), Column(0))].c,
                's',
                "{clear}"
            );
            assert_eq!(
                term.grid()[Point::new(Line(1), Column(0))].c,
                'R',
                "{clear}"
            );
            assert_eq!(
                term.grid().cursor.point,
                Point::new(Line(2), Column(8)),
                "{clear}"
            );
            assert_eq!(term.history_size(), 0, "{clear}");
        }
    }

    /// Once the shell has drawn a prompt, its startup is over; a clear after
    /// the first OSC 133 A prompt mark behaves as it always did.
    #[cfg(unix)]
    #[test]
    fn a_prompt_mark_before_the_clear_disarms_the_guard() {
        let terminal = spawn_seeded(
            &one_row_seed(),
            "printf '\\033]133;A\\007'; printf '\\033[H\\033[2Jprompt$ '; read wait",
        );
        wait_for_row(&terminal, 0, 'p');
        let term = terminal.term.lock().unwrap();
        assert_eq!(term.history_size(), 2);
        assert!(!term.restore_clear_guard_armed());
    }

    /// The same inside a synchronized update, where the prompt mark is
    /// replayed in wire order when the update ends.
    #[cfg(unix)]
    #[test]
    fn a_prompt_mark_inside_a_synchronized_update_disarms_the_guard() {
        let terminal = spawn_seeded(
            &one_row_seed(),
            "printf '\\033[?2026h\\033]133;A\\007\\033[H\\033[2Jprompt$ \\033[?2026l'; read wait",
        );
        wait_for_row(&terminal, 0, 'p');
        let term = terminal.term.lock().unwrap();
        assert_eq!(term.history_size(), 2);
        assert!(!term.restore_clear_guard_armed());
    }

    /// Spawn a restored pane whose child optionally prints `x`, waits for the
    /// test to resize the pane to 10 columns, then clears.
    #[cfg(unix)]
    fn resize_before_the_startup_clear(print_first: bool) -> crate::Terminal {
        let flag = std::env::temp_dir().join(format!(
            "kettle-restore-resize-{}-{print_first}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&flag);
        let script = format!(
            "{}while [ ! -e '{}' ]; do sleep 0.01; done; printf '\\033[H\\033[2Jprompt$ '; read wait",
            if print_first { "printf x; " } else { "" },
            flag.display()
        );
        let mut terminal = spawn_seeded(&one_row_seed(), &script);
        if print_first {
            wait_for_row(&terminal, 2, 'x');
        }
        terminal.resize(10, 6, 8, 16).expect("resize");
        std::fs::write(&flag, b"").expect("release the child");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while terminal.term.lock().unwrap().grid().cursor.point.column != Column(8) {
            assert!(
                std::time::Instant::now() < deadline,
                "the prompt never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&flag);
        terminal
    }

    /// A resize before the shell prints anything moves the anchor with the
    /// reflowed rows, so the clear still keeps them.
    #[cfg(unix)]
    #[test]
    fn a_resize_before_any_output_reanchors_the_guard() {
        let terminal = resize_before_the_startup_clear(false);
        let term = terminal.term.lock().unwrap();
        let rows: Vec<String> = (0..5)
            .map(|row| {
                (0..10)
                    .map(|col| term.grid()[Point::new(Line(row), Column(col))].c)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            rows,
            ["saved outp", "ut", "Restored d", "ivider", "prompt$"]
        );
        assert_eq!(term.history_size(), 0);
    }

    /// After output, a resize disarms the guard and the clear behaves as it
    /// always did.
    #[cfg(unix)]
    #[test]
    fn a_resize_after_output_disarms_the_guard() {
        let terminal = resize_before_the_startup_clear(true);
        let term = terminal.term.lock().unwrap();
        assert_eq!(term.grid()[Point::new(Line(0), Column(0))].c, 'p');
        assert!(term.history_size() >= 3);
        assert!(!term.restore_clear_guard_armed());
    }
}
