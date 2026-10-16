//! The terminal fuzz target, and the harness it shares with the graphics
//! lifecycle tests: a pane's PTY pipeline without the PTY. Bytes go through
//! the extractor, the VT engine and the graphics registries as a reader
//! thread sends them, and resizes commit their local geometry as
//! [`Terminal::try_resize_geometry`] does.
//!
//! [`term`] runs a script of PTY reads and pane calls (resize, scrollback,
//! scrolling, selection) and then checks what a pane must always hold: a
//! cursor and a grid inside their bounds, links, hints and search matches
//! inside the grid and within their caps, selected text within its cap, and
//! replies to the program in proportion to what it sent.

use super::*;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{MIN_COLUMNS, MIN_SCREEN_LINES};

/// Ten pixels a cell, as the graphics tests' 8x4 pane at 80x40 has.
fn cell_pixels(cells: usize) -> u16 {
    u16::try_from(cells * 10).unwrap_or(u16::MAX)
}

pub(super) fn registries() -> (Images, Virtuals, Animations, Relatives, InactiveGraphics) {
    (
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(Mutex::new(BufferGraphicsState::default())),
    )
}

pub(super) struct SyncGraphicsHarness {
    pub(super) term: SharedTerm,
    pub(super) replies: crossbeam_channel::Receiver<TermEvent>,
    pub(super) proxy: EventProxy,
    pub(super) processor: Processor,
    pub(super) extractor: Extractor,
    pub(super) deferred: DeferredGraphicsJournal,
    pub(super) active_alternate: bool,
    pub(super) images: Images,
    pub(super) virtuals: Virtuals,
    pub(super) anims: Animations,
    pub(super) relatives: Relatives,
    pub(super) inactive: InactiveGraphics,
    pub(super) geometry: Arc<Mutex<VersionedPtyGeometry>>,
    /// Whether a chunk other than graphics fails the feed: the graphics
    /// tests send only graphics; a fuzz script sends anything.
    graphics_only: bool,
}

impl SyncGraphicsHarness {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::sized(8, 4, TermConfig::default(), true)
    }

    fn sized(columns: usize, rows: usize, config: TermConfig, graphics_only: bool) -> Self {
        let (tx, replies) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        // Through the clamps a pane's own sizes go through.
        let size = TermSize::new(columns, rows);
        let geometry = PtyGeometry::new(columns, rows, cell_pixels(columns), cell_pixels(rows));
        let term = Arc::new(Mutex::new(Term::new(config, &size, proxy.clone())));
        let (images, virtuals, anims, relatives, inactive) = registries();
        Self {
            term,
            replies,
            proxy,
            processor: Processor::new(),
            extractor: Extractor::new(),
            deferred: DeferredGraphicsJournal::new(),
            active_alternate: false,
            images,
            virtuals,
            anims,
            relatives,
            inactive,
            geometry: Arc::new(Mutex::new(VersionedPtyGeometry {
                geometry,
                generation: 0,
            })),
            graphics_only,
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        let Self {
            term,
            replies: _,
            proxy,
            processor,
            extractor,
            deferred,
            active_alternate,
            images,
            virtuals,
            anims,
            relatives,
            inactive,
            geometry,
            graphics_only,
        } = self;
        extractor.feed_with(bytes, |extractor, chunk| match chunk {
            Chunk::GraphicsReply(reply) => send_graphics_reply(proxy, reply),
            Chunk::Pass(bytes) => {
                let mut context = SyncGraphicsContext {
                    active_alternate,
                    deferred,
                    registries: GraphicsRegistries {
                        inactive,
                        images,
                        virtuals,
                        anims,
                        relatives,
                    },
                    actions: GraphicsActionContext {
                        inactive,
                        images,
                        virtuals,
                        anims,
                        relatives,
                        geometry,
                        proxy,
                    },
                    extractor,
                };
                advance_terminal_bytes(processor, term, &bytes, &mut context);
            }
            Chunk::DeferredGraphics(graphics) => deferred.defer(processor, graphics),
            chunk => {
                let mut term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let graphics = apply_graphics_chunk_at(
                    &mut term,
                    chunk,
                    GraphicsActionContext {
                        inactive,
                        images,
                        virtuals,
                        anims,
                        relatives,
                        geometry,
                        proxy,
                    },
                    extractor,
                );
                assert!(
                    graphics || !*graphics_only,
                    "unexpected non-graphics extractor chunk"
                );
            }
        });
    }

    /// Commit a resize's local half, as [`Terminal::try_resize_geometry`]
    /// does before it resizes the PTY.
    fn resize(&mut self, columns: usize, rows: usize, history: usize) {
        let current = self
            .geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .geometry;
        let desired = PtyGeometry::new(columns, rows, cell_pixels(columns), cell_pixels(rows));
        if current == desired {
            return;
        }
        let grid_changed = (current.columns, current.rows) != (desired.columns, desired.rows);
        let options = grid_changed.then(|| TermConfig {
            scrolling_history: history,
            ..TermConfig::default()
        });
        commit_local_geometry_inner(&self.term, &self.geometry, None, desired, options);
        if grid_changed {
            let oldest = self
                .term
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .grid()
                .history_origin();
            crate::images::prune(&self.images, oldest);
        }
        if current.columns != desired.columns {
            clear_reflowed_regular_placements(&self.images, &self.relatives, &self.inactive);
        } else {
            let mut placements = self
                .images
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            recompute_kitty_placements(&mut placements, desired);
            drop(placements);
            let mut inactive = self
                .inactive
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            recompute_kitty_placements(&mut inactive.placements, desired);
        }
    }

    #[cfg(test)]
    pub(super) fn active_ids(&self) -> Vec<u32> {
        self.images
            .lock()
            .unwrap()
            .iter()
            .filter_map(|placement| placement.id)
            .collect()
    }

    #[cfg(test)]
    pub(super) fn inactive_ids(&self) -> Vec<u32> {
        self.inactive
            .lock()
            .unwrap()
            .placements
            .iter()
            .filter_map(|placement| placement.id)
            .collect()
    }
}

/// In a [`term`] script, this byte starts a pane call; `0xff 0x00` is a
/// literal `0xff`.
const CALL: u8 = 0xff;
/// The calls one script may make, so a script stays a few reads long.
const MAX_CALLS: usize = 16;
/// The largest selection text a check asks for.
const SELECTION_CAP: usize = 64 * 1024;
/// Search patterns a script picks from: literal, class, anchored, and one
/// that matches everywhere.
const PATTERNS: [&str; 6] = ["e", r"\w+", "^", "$", "(?i)HTTP", "."];

/// Run `data` as a script against a pane: its first two bytes pick the
/// starting size, and the rest is PTY bytes, with `0xff` and a call byte
/// (and that call's two argument bytes) between them.
pub fn term(data: &[u8]) {
    let [columns, rows, data @ ..] = data else {
        return;
    };
    let mut harness = SyncGraphicsHarness::sized(
        usize::from(*columns) % 160 + 1,
        usize::from(*rows) % 48 + 1,
        TermConfig {
            scrolling_history: 200,
            ..TermConfig::default()
        },
        false,
    );
    let mut history = 200;
    let mut sent = 0usize;
    let mut read = Vec::new();
    let mut calls = 0;
    let mut bytes = data.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != CALL {
            read.push(byte);
            continue;
        }
        let Some(call) = bytes.next() else { break };
        if call == 0 {
            read.push(CALL);
            continue;
        }
        sent += read.len();
        harness.feed(&std::mem::take(&mut read));
        if calls == MAX_CALLS {
            continue;
        }
        calls += 1;
        let (a, b) = (bytes.next().unwrap_or(0), bytes.next().unwrap_or(0));
        match call % 6 {
            1 => harness.resize(usize::from(a) % 400 + 1, usize::from(b) % 120 + 1, history),
            2 => {
                history = usize::from(a) * 16;
                let options = TermConfig {
                    scrolling_history: history,
                    ..TermConfig::default()
                };
                lock(&harness.term).set_options(options);
            }
            3 => lock(&harness.term).scroll_display(Scroll::Delta(i32::from(a as i8))),
            4 => select(&harness.term, a, b),
            5 => search_within_bounds(&harness.term, PATTERNS[usize::from(a) % PATTERNS.len()]),
            _ => check(&harness.term),
        }
    }
    sent += read.len();
    harness.feed(&read);
    check(&harness.term);
    let replied: usize = harness
        .replies
        .try_iter()
        .filter_map(|event| match event {
            TermEvent::PtyWrite(reply) => Some(reply.len()),
            _ => None,
        })
        .sum();
    // A query of a few bytes may draw a reply of a few dozen; nothing should
    // answer more than that, however the queries are arranged.
    assert!(
        replied <= sent * 64 + 4096,
        "{replied} reply bytes for {sent} sent"
    );
}

fn lock(term: &SharedTerm) -> std::sync::MutexGuard<'_, Term<EventProxy>> {
    term.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Select from one cell to another, of the kind `a` picks, and take its
/// text within [`SELECTION_CAP`].
fn select(term: &SharedTerm, a: u8, b: u8) {
    let mut term = lock(term);
    let (columns, rows) = (term.columns(), term.screen_lines());
    let history = term.grid().history_size();
    let kind = match a % 4 {
        0 => SelectionType::Simple,
        1 => SelectionType::Block,
        2 => SelectionType::Semantic,
        _ => SelectionType::Lines,
    };
    // From somewhere in history or on screen to somewhere on screen.
    let start = Point::new(
        Line(i32::from(a) % (rows + history) as i32 - history as i32),
        Column(usize::from(b) % columns),
    );
    let end = Point::new(
        Line(i32::from(b) % rows as i32),
        Column(usize::from(a) % columns),
    );
    let mut selection = Selection::new(kind, start, Side::Left);
    selection.update(end, Side::Right);
    term.selection = Some(selection);
    if let Ok(Some(text)) = selection_text_bounded(&term, SELECTION_CAP) {
        assert!(text.len() <= SELECTION_CAP, "{} selected bytes", text.len());
    }
}

fn search_within_bounds(term: &SharedTerm, pattern: &str) {
    let term = lock(term);
    let (columns, rows) = (term.columns(), term.screen_lines() as i32);
    let history = term.grid().history_size() as i32;
    let matches = crate::search::search(&term, pattern);
    assert!(matches.len() <= crate::search::MAX_SEARCH_MATCHES);
    for found in matches {
        assert!(
            (-history..rows).contains(&found.line)
                && found.start_col <= found.end_col
                && found.end_col < columns,
            "{pattern:?} matched {found:?} in a {columns}x{rows} grid with {history} history"
        );
    }
}

/// What a pane must always hold.
fn check(term: &SharedTerm) {
    let term = lock(term);
    let (columns, rows) = (term.columns(), term.screen_lines());
    assert!(columns >= MIN_COLUMNS && rows >= MIN_SCREEN_LINES);
    let cursor = term.grid().cursor.point;
    assert!(
        (0..rows as i32).contains(&cursor.line.0) && cursor.column.0 < columns,
        "cursor {cursor:?} in a {columns}x{rows} grid"
    );
    for link in crate::links::links(&term) {
        assert!(
            link.row < rows && link.start_col <= link.end_col && link.end_col < columns,
            "{link:?} in a {columns}x{rows} grid"
        );
    }
    let offset = term.grid().display_offset() as i32;
    let lines: Vec<i32> = (0..rows as i32).map(|row| row - offset).collect();
    let hints = crate::hints::detect_rows(term.grid(), &lines, columns);
    for hint in &hints {
        assert!(
            hint.row < rows && hint.start <= hint.end && hint.end < columns,
            "{hint:?} in a {columns}x{rows} grid"
        );
    }
    assert!(hints.is_sorted_by_key(|hint| (hint.row, hint.start)));
}

#[cfg(test)]
mod tests {
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    /// PTY output a pane commonly reads, and what stresses its bounds.
    fn streams() -> Vec<Vec<u8>> {
        vec![
            b"$ ls\r\nCargo.toml  src  https://example.com/a/b?c=d\r\n$ ".to_vec(),
            "wide \u{4e16}\u{754c} and e\u{301}\u{302} combining, \u{1f469}\u{200d}\u{1f4bb}\r\n".into(),
            b"\x1b[?1049h\x1b[2J\x1b[H\x1b[5;10Hmid\x1b[?1049l\x1b[?47h\x1b[?47l\x1b[?1047h\x1b[?1047l".to_vec(),
            b"\x1b[3;2r\x1b[?6h\x1b[10Bdown\x1b[0;0r\x1b[65535;1r\x1b[?6l\x1b[r".to_vec(),
            b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25;26;27;28;29;30;31;32m".to_vec(),
            b"\x1b[65535C\x1b[65535B\x1b[65535@\x1b[65535P\x1b[65535X\x1b[65535L\x1b[65535M\x1bH\x1b[3g\x1b[65535I".to_vec(),
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ \x1b]0;title\x07\x1b[c\x1b[>c\x1b[6n\x1b[?6n".to_vec(),
            b"\x1b_Ga=T,f=32,s=1,v=1;AAAA/w==\x1b\\\x1b[?2026h\x1b_Ga=T,f=32,s=1,v=1;AAAA/w==\x1b\\\x1b[?2026l".to_vec(),
            b"\x1bPq#0;2;100;0;0#0~~~~\x1b\\\x1b[2Ssrc/main.rs:12:4 error\r\n\x1b[2T".to_vec(),
            "\u{2029}\u{202e}rtl\u{202c} line\x1b[?7lno wrap at the edge of the line here\x1b[?7h".into(),
        ]
    }

    #[test]
    fn mutated_pane_scripts_keep_the_pane_in_bounds() {
        let mut seeds = Vec::new();
        for stream in streams() {
            for (columns, rows) in [(79, 23), (1, 0), (3, 2)] {
                let mut seed = vec![columns, rows];
                seed.extend_from_slice(&stream);
                seeds.push(seed);
            }
            // The stream, then a resize, scrollback, a scroll, a selection
            // and a search, then the stream again.
            let mut calls = vec![39, 11];
            calls.extend_from_slice(&stream);
            calls.extend_from_slice(&[CALL, 1, 9, 5, CALL, 2, 1, 0, CALL, 3, 200, 0]);
            calls.extend_from_slice(&[CALL, 4, 1, 3, CALL, 5, 1, 0]);
            calls.extend_from_slice(&stream);
            seeds.push(calls);
        }
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("term", &seeds);
        for case in cases(&seeds, 300, 0x5eed_0201) {
            term(&case);
        }
    }

    /// The `term` target's first CI run: a scroll region below the top row,
    /// scrolled while the view was scrolled back, moved the display offset
    /// past the history, and link detection then read a row that was not
    /// there (`positive < self.len` in the grid's storage).
    #[test]
    fn a_partial_scroll_region_keeps_a_scrolled_back_view_in_history() {
        let crash: [&[u8]; 8] = [
            b"\x8d\x87\x1b\xff\xff1337;File=\xff)[3;2r\x1b[?6h\x1b[10Bdown\x1b",
            b"[0;0\x94\x94\x94\x94\x94\x94\x94\x94\x94[r\xff\x01\x09\x05\xff",
            b"\x02\x01\x00\xff\x03\xc8\x00\xff\x04\x01\x03\xff\x05\x01\x00\x1b",
            b"[3;\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x004r\x1b[\xff\xff",
            b"\x10\x00\x00\x11\x00Bdgo.toml  src3\x001\x01;5r\x1b[?6l\x1b[r",
        ];
        term(&crash.concat());
    }

    /// The first hour-long `term` campaign: a block selection whose start
    /// lands one column past a two-column row read past the row while its
    /// text was copied.
    #[test]
    fn a_block_selection_past_a_narrow_row_copies_safely() {
        let crash: [&[u8]; 8] = [
            b"O\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8",
            b"\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8\xa8z\xa8",
            b"\xa8\xa8\xa8\xa8\xa8\xa8B\x13]8\x00\x00\x00\x00;;https:/m/am/a/b",
            b"?c=d\x0d\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xfb\xff\xff\xff",
            b"\xff\xff\xff\xff\xff\xff\xff\x01\x00\xdf\x9end e\xcc\x81\xcc\x82",
            b" combining, \x0a$\xf0\x9f\x91\xa9\xe2w\x8d\xf0 \x9f\x92\x09E\x01",
            b"\xfd\xfe\xf7\xff\x03\xc8\x00\xff\x04=\x01xe0le.com\x1b\\link\x1b",
            b"]8;;\x1b\\ \x1b]0l\x1bti;et\x07[c\x1b[>c\x1b[6n\x1b[?6n",
        ];
        term(&crash.concat());
    }

    /// The checks are live: a cursor outside its grid fails them.
    #[test]
    fn a_cursor_outside_its_grid_fails_the_check() {
        let harness = SyncGraphicsHarness::sized(4, 2, TermConfig::default(), false);
        check(&harness.term);
        lock(&harness.term).grid_mut().cursor.point.column = Column(4);
        let term = harness.term.clone();
        assert!(std::panic::catch_unwind(move || check(&term)).is_err());
    }
}
