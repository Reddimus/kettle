use std::collections::VecDeque;
use std::sync::{Arc, Barrier, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::Processor;

use super::{
    EventProxy, PtyGeometry, SharedTerm, TermSize, VersionedPtyGeometry, commit_local_geometry,
    commit_local_geometry_inner, local_geometry_snapshot, stable_grid_line_id,
};

#[test]
fn widening_discards_a_prompt_partially_merged_into_the_preceding_row() {
    let first = PtyGeometry::new(80, 24, 800, 240);
    let desired = PtyGeometry::new(100, 24, 1000, 240);
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut terminal = Term::new(
        TermConfig::default(),
        &TermSize::new(80, 24),
        EventProxy::new(tx, Arc::new(|| {})),
    );
    let mut processor: Processor = Processor::new();
    processor.advance(&mut terminal, &[b'x'; 81]);
    let prompt = stable_grid_line_id(
        terminal.grid().history_origin(),
        terminal.grid().history_size(),
        terminal.grid().cursor.point.line.0,
    );
    processor.advance(&mut terminal, &[b'p'; 50]);
    let term: SharedTerm = Arc::new(Mutex::new(terminal));
    let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
        geometry: first,
        generation: 0,
    }));
    let prompts = Mutex::new(VecDeque::from([prompt]));
    commit_local_geometry_inner(
        &term,
        &geometry,
        Some(&prompts),
        None,
        desired,
        Some(TermConfig::default()),
        usize::MAX,
    );
    assert!(
        prompts.lock().unwrap().is_empty(),
        "partial row merging must not replace the prompt start with the cursor row"
    );
}

#[test]
fn transient_resize_round_trip_keeps_the_current_single_row_prompt() {
    let first = PtyGeometry::new(128, 24, 1228, 384);
    let narrowed = PtyGeometry::new(98, 24, 940, 384);
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    let mut terminal = Term::new(
        TermConfig::default(),
        &TermSize {
            columns: first.columns,
            screen_lines: first.rows,
        },
        proxy,
    );
    let mut processor: Processor = Processor::new();
    processor.advance(&mut terminal, b"banner\r\nsecond\r\nprompt");
    let old_cursor = {
        let grid = terminal.grid();
        stable_grid_line_id(
            grid.history_origin(),
            grid.history_size(),
            grid.cursor.point.line.0,
        )
    };
    let term: SharedTerm = Arc::new(Mutex::new(terminal));
    let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
        geometry: first,
        generation: 0,
    }));
    let prompts = Mutex::new(VecDeque::from([old_cursor]));

    commit_local_geometry_inner(
        &term,
        &geometry,
        Some(&prompts),
        None,
        narrowed,
        Some(TermConfig::default()),
        usize::MAX,
    );
    commit_local_geometry_inner(
        &term,
        &geometry,
        Some(&prompts),
        None,
        first,
        Some(TermConfig::default()),
        usize::MAX,
    );

    let new_cursor = {
        let terminal = term.lock().unwrap();
        let grid = terminal.grid();
        stable_grid_line_id(
            grid.history_origin(),
            grid.history_size(),
            grid.cursor.point.line.0,
        )
    };
    assert_eq!(
        prompts.lock().unwrap().iter().copied().collect::<Vec<_>>(),
        vec![new_cursor]
    );
}

#[test]
fn resize_defers_history_eviction_until_output_and_config_edits_still_trim() {
    let first = PtyGeometry::new(20, 4, 160, 64);
    let desired = PtyGeometry::new(120, 4, 960, 64);
    let (tx, _rx) = crossbeam_channel::unbounded();
    let options = TermConfig {
        scrolling_history: 100,
        ..Default::default()
    };
    let term: SharedTerm = Arc::new(Mutex::new(Term::new(
        options,
        &TermSize::new(first.columns, first.rows),
        EventProxy::new(tx, Arc::new(|| {})),
    )));
    let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
        geometry: first,
        generation: 0,
    }));
    let mut processor: Processor = Processor::new();
    processor.advance(&mut *term.lock().unwrap(), "\r\n".repeat(24).as_bytes());
    let before = term.lock().unwrap().grid().history_size();
    assert!(before > 8);
    let options = TermConfig {
        scrolling_history: 8,
        ..Default::default()
    };
    commit_local_geometry(&term, &geometry, desired, Some(options.clone()));
    assert_eq!(term.lock().unwrap().grid().history_size(), before);
    commit_local_geometry(
        &term,
        &geometry,
        PtyGeometry::new(120, 2, 960, 32),
        Some(options.clone()),
    );
    commit_local_geometry(&term, &geometry, desired, Some(options.clone()));
    let mut term = term.lock().unwrap();
    assert_eq!(term.grid().history_size(), before);
    term.set_options(TermConfig {
        unnegotiated_modified_enter: false,
        ..options.clone()
    });
    assert_eq!(term.grid().history_size(), before);
    processor.advance(&mut *term, b"\x1b[4;1H\r\n");
    assert_eq!(term.grid().history_size(), 8);
    term.set_options(TermConfig {
        scrolling_history: 3,
        ..options
    });
    assert_eq!(term.grid().history_size(), 3);
}

#[test]
fn grid_and_pixel_generations_are_atomic_under_concurrent_resize_and_read() {
    let first = PtyGeometry::new(80, 24, 768, 384);
    let second = PtyGeometry::new(101, 37, 970, 703);
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    let term: SharedTerm = Arc::new(Mutex::new(Term::new(
        TermConfig::default(),
        &TermSize {
            columns: first.columns,
            screen_lines: first.rows,
        },
        proxy,
    )));
    let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
        geometry: first,
        generation: 0,
    }));
    let start = Arc::new(Barrier::new(2));

    let writer_term = term.clone();
    let writer_geometry = geometry.clone();
    let writer_start = start.clone();
    let writer = std::thread::spawn(move || {
        writer_start.wait();
        for index in 0..4_000 {
            let desired = if index % 2 == 0 { second } else { first };
            commit_local_geometry(
                &writer_term,
                &writer_geometry,
                desired,
                Some(TermConfig::default()),
            );
        }
    });

    start.wait();
    let mut last_generation = 0;
    for _ in 0..4_000 {
        let (columns, rows, snapshot, generation) = local_geometry_snapshot(&term, &geometry);
        assert_eq!((columns, rows), (snapshot.columns, snapshot.rows));
        assert!(
            snapshot == first || snapshot == second,
            "snapshot mixed two geometry generations: {snapshot:?}"
        );
        assert!(generation >= last_generation);
        last_generation = generation;
        std::thread::yield_now();
    }
    writer.join().unwrap();
}

/// A shell's startup `clear` on a fresh screen must not leave a blank line in
/// history: growing the window would pull it back above the prompt.
#[test]
fn clearing_a_fresh_screen_keeps_the_prompt_on_the_top_row_after_growing() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut terminal = Term::new(
        TermConfig::default(),
        &TermSize::new(80, 24),
        EventProxy::new(tx, Arc::new(|| {})),
    );
    let mut processor: Processor = Processor::new();
    processor.advance(&mut terminal, b"\x1b[H\x1b[2Jprompt$ ");
    assert_eq!(terminal.grid().history_size(), 0);
    terminal.resize(TermSize::new(80, 27));
    let top: String = (0..7)
        .map(|column| {
            terminal.grid()[alacritty_terminal::index::Line(0)]
                [alacritty_terminal::index::Column(column)]
            .c
        })
        .collect();
    assert_eq!(top, "prompt$");
    assert_eq!(terminal.grid().cursor.point.line.0, 0);
}

/// A 20 x 10 terminal that has printed `bytes`.
fn printed(bytes: &[u8]) -> SharedTerm {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut terminal = Term::new(
        TermConfig::default(),
        &TermSize::new(20, 10),
        EventProxy::new(tx, Arc::new(|| {})),
    );
    let mut processor: Processor = Processor::new();
    processor.advance(&mut terminal, bytes);
    Arc::new(Mutex::new(terminal))
}

/// The text of each screen line, without trailing blanks.
fn screen_text(term: &SharedTerm) -> Vec<String> {
    let term = term.lock().unwrap();
    (0..term.screen_lines() as i32)
        .map(|line| {
            let row = &term.grid()[alacritty_terminal::index::Line(line)];
            (0..term.columns())
                .map(|column| row[alacritty_terminal::index::Column(column)].c)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Open and close a two-row bar under the 20 x 10 grid, such as the search
/// bar: the screen shrinks while it is open and grows back when it closes.
/// With `give_back`, closing takes back from history only the rows the
/// opening pushed there, measured by the screen's top row identifier;
/// without it, closing is a plain resize. Returns the rows pushed.
fn open_and_close_a_bar(term: &SharedTerm, give_back: bool) -> usize {
    let full = PtyGeometry::new(20, 10, 200, 100);
    let open = PtyGeometry::new(20, 8, 200, 80);
    let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
        geometry: full,
        generation: 0,
    }));
    let screen_top = || {
        let term = term.lock().unwrap();
        stable_grid_line_id(term.grid().history_origin(), term.grid().history_size(), 0)
    };
    let commit = |desired, history_pull| {
        commit_local_geometry_inner(
            term,
            &geometry,
            None,
            None,
            desired,
            Some(TermConfig::default()),
            history_pull,
        );
    };
    let before = screen_top();
    commit(open, usize::MAX);
    let pushed = (screen_top() - before) as usize;
    commit(full, if give_back { pushed } else { usize::MAX });
    pushed
}

/// After `clear` the screen holds a short report and history holds what came
/// before. Opening the bar takes only blank lines from below the prompt, so
/// closing it gives back blank lines and the report stays where the reader
/// saw it. A plain resize pulls the rows above `clear` back onto the screen
/// and moves every line down.
#[test]
fn closing_a_bar_below_the_prompt_keeps_the_lines_above_it_in_place() {
    let mut bytes: Vec<u8> = (1..=12)
        .flat_map(|line| format!("old {line}\r\n").into_bytes())
        .collect();
    bytes.extend_from_slice(b"$ clear\r\n\x1b[H\x1b[2Jreport 1\r\nreport 2\r\n$ ");
    let screen = ["report 1", "report 2", "$", "", "", "", "", "", "", ""];

    let plain = printed(&bytes);
    assert_eq!(screen_text(&plain), screen);
    open_and_close_a_bar(&plain, false);
    assert_eq!(screen_text(&plain)[..3], ["old 12", "$ clear", "report 1"]);

    let term = printed(&bytes);
    let history = term.lock().unwrap().grid().history_size();
    for _ in 0..2 {
        assert_eq!(
            open_and_close_a_bar(&term, true),
            0,
            "only blank lines left"
        );
        assert_eq!(screen_text(&term), screen);
    }
    let term = term.lock().unwrap();
    assert_eq!(term.grid().cursor.point.line.0, 2);
    assert_eq!(term.grid().history_size(), history);
    assert_eq!(term.grid().display_offset(), 0);
}

/// With the prompt on the bottom row, opening the bar pushes rows into
/// history to keep the prompt in view. Taking exactly those rows back on
/// closing restores the screen, as the plain resize does.
#[test]
fn closing_a_bar_over_a_bottom_prompt_takes_its_rows_back() {
    let mut bytes: Vec<u8> = (1..=30)
        .flat_map(|line| format!("line {line}\r\n").into_bytes())
        .collect();
    bytes.extend_from_slice(b"$ ");
    for give_back in [false, true] {
        let term = printed(&bytes);
        let screen = screen_text(&term);
        assert_eq!((&*screen[0], &*screen[9]), ("line 22", "$"));
        let history = term.lock().unwrap().grid().history_size();
        for _ in 0..2 {
            assert_eq!(open_and_close_a_bar(&term, give_back), 2, "{give_back}");
            assert_eq!(screen_text(&term), screen, "{give_back}");
        }
        let term = term.lock().unwrap();
        assert_eq!(term.grid().cursor.point.line.0, 9, "{give_back}");
        assert_eq!(term.grid().history_size(), history, "{give_back}");
    }
}
