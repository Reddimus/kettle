use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use alacritty_terminal::index::{Column, Line, Point};

use super::fuzzing::SyncGraphicsHarness;
use super::{
    GraphicsActionContext, GraphicsRegistries, PtyGeometry, SyncGraphicsContext,
    advance_terminal_bytes_with_commit_hook, commit_local_geometry, publish_output_if_ready,
};
use crate::event::OutputWakeGate;

#[test]
fn search_tolerates_grid_commit_before_output_publication() {
    use crate::{
        CaseSensitivity, CompiledSearch, SearchBounds, SearchDirection, SearchPoint,
        SearchScanToken, SearchSpan,
    };

    const COLUMNS: usize = 16_384;
    for direction in [SearchDirection::Forward, SearchDirection::Reverse] {
        let mut harness = SyncGraphicsHarness::new();
        commit_local_geometry(
            &harness.term,
            &harness.geometry,
            PtyGeometry::new(COLUMNS, 2, 0, 0),
            Some(alacritty_terminal::term::Config::default()),
        );
        harness.feed(&vec![b'a'; COLUMNS]);
        let generation = AtomicU64::new(7);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_count = wakes.clone();
        let output_wake = OutputWakeGate::new(Arc::new(move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
        }));
        let first = SearchPoint::new(0, 0);
        let last = SearchPoint::new(0, COLUMNS - 1);
        let bounds = match direction {
            SearchDirection::Forward => SearchBounds::new(first, last),
            SearchDirection::Reverse => SearchBounds::new(last, first),
        };
        let mut search = CompiledSearch::compile("a", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let old_token = {
            let term = harness.term.lock().unwrap();
            let token = SearchScanToken::capture(&term, 1, generation.load(Ordering::Acquire));
            search.set_scan_token(token);
            let batch = search.find_in_range(&term, bounds, direction, COLUMNS);
            assert_eq!(
                batch.continuation,
                Some(bounds.start),
                "{direction:?}: layout={:?}, matches={}, exhausted={}, limited={}",
                token.layout,
                batch.matches.len(),
                batch.exhausted,
                batch.accuracy_limited
            );
            token
        };

        let SyncGraphicsHarness {
            term,
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
            ..
        } = &mut harness;
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
        let mut replacement = b"\r".to_vec();
        replacement.extend(std::iter::repeat_n(b'b', COLUMNS - 1));
        replacement.push(b'a');
        let expected = [SearchSpan::new(last, last)];
        let mut interleaved = false;
        advance_terminal_bytes_with_commit_hook(
            processor,
            term,
            &replacement,
            &mut context,
            |_| {
                interleaved = true;
                // The real commit seam releases the grid lock before either
                // graphics replay or output generation publication.
                let sampled = generation.load(Ordering::Acquire);
                let term = term.lock().unwrap();
                assert_eq!(sampled, 7);
                assert_eq!(wakes.load(Ordering::Relaxed), 0);
                assert_eq!(term.grid()[Point::new(Line(0), Column(0))].c, 'b');
                let token = SearchScanToken::capture(&term, 1, sampled);
                assert_eq!(token, old_token, "in-place output preserves layout");
                search.set_scan_token(token);
                // Saved regex progress must compare haystacks as well as the
                // token: otherwise it returns positions from the old 'a' row.
                let batch = search.find_in_range(&term, bounds, direction, 1);
                assert_eq!(batch.matches, expected);
                assert!(!batch.accuracy_limited);
            },
        );
        assert!(interleaved);
        assert!(publish_output_if_ready(
            processor,
            &generation,
            &output_wake
        ));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        let term = term.lock().unwrap();
        let published = generation.load(Ordering::Acquire);
        assert_eq!(published, 8);
        assert!(!old_token.is_current(&term, 1, published));
        search.set_scan_token(SearchScanToken::capture(&term, 1, published));
        assert_eq!(
            search.find_in_range(&term, bounds, direction, 1).matches,
            expected
        );
    }
}

#[test]
fn reader_sidechannels_share_the_generation_ordered_output_gate() {
    let source = super::production_source();
    let reader_start = source
        .find(".name(\"kettle-pty-reader\"")
        .expect("PTY reader thread present");
    let reader_tail = &source[reader_start..];
    let reader_end = reader_tail
        .find("\n        Ok(Terminal {")
        .expect("PTY reader thread end");
    let reader = &reader_tail[..reader_end];

    assert!(
        !reader.contains("(waker)();"),
        "parser sidechannels must not bypass the per-pane output gate"
    );
    assert!(
        reader.contains("publish_output_if_ready("),
        "PTY reads must publish through the synchronized-output guard"
    );
    let helper_start = source
        .find("fn publish_output_if_ready(")
        .expect("output publication guard present");
    let helper_tail = &source[helper_start..];
    let helper_end = helper_tail
        .find("\n}\n\n")
        .map(|offset| offset + 2)
        .expect("output publication guard end");
    let helper = &helper_tail[..helper_end];
    let pending = helper
        .find("sync_timeout().is_some()")
        .expect("DEC 2026 pending-state check present");
    let generation = helper
        .find("out_gen.fetch_add(")
        .expect("output generation publication present");
    let wake = helper
        .find("output_wake.request();")
        .expect("gated output wake present");
    assert!(
        pending < generation && generation < wake,
        "pending sync must suppress publication; otherwise Release generation precedes wake"
    );
}

#[test]
fn pty_pump_spawn_failure_is_observable_and_closes_the_pane() {
    let source = super::production_source();
    let name = source
        .find(".name(\"kettle-pty-pump\"")
        .expect("PTY pump thread present");
    let start = source[..name]
        .rfind("if let Err(error)")
        .expect("PTY pump spawn error is handled");
    let end = source[name..]
        .find("\n                    loop {")
        .map(|offset| name + offset)
        .expect("outer reader loop follows pump creation");
    let spawn = &source[start..end];
    assert!(
        spawn.contains("log::error!(\"failed to spawn PTY pump thread: {error}\")")
            && spawn.contains("proxy.send_event_exit();")
            && spawn.contains("return;"),
        "thread exhaustion must leave an actionable diagnostic and a normal pane exit"
    );
    assert!(
        !spawn.contains("let _ = std::thread::Builder"),
        "pump creation errors must not be discarded"
    );
}
