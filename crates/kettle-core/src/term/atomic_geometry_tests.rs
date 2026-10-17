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
        narrowed,
        Some(TermConfig::default()),
    );
    commit_local_geometry_inner(
        &term,
        &geometry,
        Some(&prompts),
        first,
        Some(TermConfig::default()),
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
