#[cfg(unix)]
#[test]
fn native_resize_observes_committed_geometry_and_failure_is_retryable() {
    struct ResizeProbe {
        master: Box<dyn portable_pty::MasterPty + Send>,
        term: SharedTerm,
        geometry: Arc<Mutex<VersionedPtyGeometry>>,
        reflow: Arc<AtomicU64>,
        fail: AtomicBool,
    }

    impl portable_pty::MasterPty for ResizeProbe {
        fn resize(&self, size: PtySize) -> Result<()> {
            let (cols, rows, geometry, _) = local_geometry_snapshot(&self.term, &self.geometry);
            assert_eq!((cols, rows), (size.cols as usize, size.rows as usize));
            assert_eq!(native_pty_size(geometry), size);
            assert_eq!(self.reflow.load(Ordering::Acquire), 1);
            if self.fail.swap(false, Ordering::AcqRel) {
                return Err(anyhow::anyhow!("injected resize failure"));
            }
            self.master.resize(size)
        }

        fn get_size(&self) -> Result<PtySize> {
            self.master.get_size()
        }

        fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
            self.master.try_clone_reader()
        }

        fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
            self.master.take_writer()
        }

        fn process_group_leader(&self) -> Option<libc::pid_t> {
            self.master.process_group_leader()
        }

        fn as_raw_fd(&self) -> Option<RawFd> {
            self.master.as_raw_fd()
        }

        fn tty_name(&self) -> Option<std::path::PathBuf> {
            self.master.tty_name()
        }
    }

    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut terminal = Terminal::new(
        &["/bin/sh".into(), "-c".into(), "read ready".into()],
        None,
        100,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        Arc::new(|| {}),
    )
    .expect("resize fixture");
    let applied = terminal.applied_pty_geometry;
    let desired = PtyGeometry::new(120, 24, 960, 384);
    terminal.master = Some(Box::new(ResizeProbe {
        master: terminal.master.take().unwrap(),
        term: terminal.term.clone(),
        geometry: terminal.geometry.clone(),
        reflow: terminal.graphics_reflow_generation.clone(),
        fail: AtomicBool::new(true),
    }));

    assert!(terminal.try_resize_geometry(desired).is_err());
    assert_eq!(terminal.applied_pty_geometry, applied);
    assert_eq!(terminal.geometry.lock().unwrap().geometry, desired);
    assert_eq!((terminal.cols, terminal.rows), (120, 24));
    terminal.try_resize_geometry(desired).unwrap();
    assert_eq!(terminal.applied_pty_geometry, desired);
}

#[cfg(unix)]
fn animation_graphics_ack_roundtrip(synchronized: bool) {
    use super::{CursorShape, TermEvent, Terminal};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let start = if synchronized { r"\033[?2026h" } else { "" };
    let end = if synchronized { r"\033[?2026l" } else { "" };
    let script = format!(
        r#"stty raw -echo; printf '\033_Ga=T,i=401,q=2,f=24,s=1,v=1,C=1;AAAA\033\\'; printf '{start}\033_Ga=f,i=401,f=24,s=1,v=1;/wAA\033\\'; reply=$(dd bs=1 count=17 2>/dev/null | od -An -tx1 | tr -d ' \n'); test "$reply" = 1b5f47693d3430312c723d323b4f4b1b5c || exit 7; printf '{end}{start}\033_Ga=c,i=401,r=1,c=2,w=1,h=1,C=1\033\\'; reply=$(dd bs=1 count=13 2>/dev/null | od -An -tx1 | tr -d ' \n'); test "$reply" = 1b5f47693d3430313b4f4b1b5c || exit 8; printf '{end}ANIMATION_ACK_DONE'"#
    );
    let argv = vec!["/bin/sh".into(), "-c".into(), script];
    let (tx, rx) = crossbeam_channel::unbounded();
    let terminal = Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        Arc::new(|| {}),
    )
    .expect("native animation acknowledgement PTY");
    let child_pid = terminal.child_pid().expect("owned Unix animation child");
    let child_group = unsafe { libc::getpgid(child_pid as libc::pid_t) };
    assert!(child_group > 0);
    eprintln!("ANIMATION_ACK_PTY_OWNED_CHILD pid={child_pid} pgid={child_group}");
    let expected = ["\x1b_Gi=401,r=2;OK\x1b\\", "\x1b_Gi=401;OK\x1b\\"];
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut replies = 0;
    let code = loop {
        while let Ok(event) = rx.try_recv() {
            if let TermEvent::PtyWrite(reply) = event {
                assert!(replies < expected.len(), "duplicate animation reply");
                assert_eq!(reply, expected[replies]);
                {
                    let animations = terminal.anims.lock().unwrap();
                    let animation = animations
                        .get(&401)
                        .expect("animation registered before reply");
                    assert_eq!(animation.imgs.len(), 2);
                    assert_eq!(animation.imgs[0].rgba.as_slice(), &[0, 0, 0, 255]);
                    let frame = if replies == 0 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 0, 0, 255]
                    };
                    assert_eq!(animation.imgs[1].rgba.as_slice(), &frame);
                }
                replies += 1;
                terminal.write(reply.as_bytes());
            }
        }
        if let Some(code) = terminal.child_exit_code() {
            break code;
        }
        assert!(
            Instant::now() < deadline,
            "client waited for animation acknowledgement; synchronized={synchronized}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        code, 0,
        "child checked frame and composition response bytes"
    );
    assert_eq!(replies, 2);
    assert_eq!(
        terminal
            .placements()
            .iter()
            .map(|placement| placement.id)
            .collect::<Vec<_>>(),
        [Some(401)]
    );
}

#[cfg(unix)]
#[test]
fn animation_graphics_ack_reaches_a_waiting_pty_child() {
    animation_graphics_ack_roundtrip(false);
}

#[cfg(unix)]
#[test]
fn animation_graphics_ack_reaches_a_waiting_synchronized_pty_child() {
    animation_graphics_ack_roundtrip(true);
}

use super::*;
use std::time::Duration;

#[cfg(unix)]
#[test]
fn completed_and_loading_animations_stop_requesting_redraws() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let terminal = Terminal::new(
        &["/bin/sh".into(), "-c".into(), "read ready".into()],
        None,
        100,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        Arc::new(|| {}),
    )
    .expect("animation clock fixture");
    let root = crate::ImageData::new(1, 1, vec![1, 0, 0, 255]).unwrap();
    let last = crate::ImageData::new(1, 1, vec![2, 0, 0, 255]).unwrap();
    for (loading, loops, elapsed, gaps, expected) in [
        (false, 1, 0, vec![1000, 1000], true),
        (false, 1, 10, vec![1000, 1000], false),
        (true, 0, 0, vec![1000, 1000], true),
        (true, 0, 10, vec![1000, 1000], false),
        (false, 0, 10, vec![1000, 1000], true),
        (false, 0, 0, vec![0, 1000], false),
        (false, 0, 0, vec![0, 0], false),
    ] {
        terminal.anims.lock().unwrap().insert(
            1,
            AnimEntry {
                imgs: vec![root.clone(), last.clone()],
                gaps,
                state: kettle_vt::kitty::AnimationState {
                    running: true,
                    loading,
                    loops,
                    ..Default::default()
                },
                started: std::time::Instant::now() - Duration::from_secs(elapsed),
            },
        );
        assert_eq!(terminal.has_running_animation(), expected);
        if elapsed == 10 && (loading || loops > 0) {
            assert_eq!(
                terminal.anims.lock().unwrap()[&1].current().unwrap().rgba[0],
                2
            );
        }
    }
}

/// The OSC 133 prompt-mark ring must (a) dedup against
/// the most-recent mark, (b) preserve insertion order, and (c) cap at
/// `MAX_PROMPT_MARKS` by dropping the OLDEST — all with O(1) `pop_front`,
/// not an O(n) `Vec::drain` on every prompt (the hot reader-thread path).
#[test]
fn prompt_mark_ring_dedups_and_caps_oldest_first() {
    use std::collections::VecDeque;
    let mut ring: VecDeque<u64> = VecDeque::new();

    // Dedup: pushing the same most-recent mark twice keeps one.
    assert!(push_prompt_mark(&mut ring, 10));
    assert!(!push_prompt_mark(&mut ring, 10));
    assert_eq!(ring.len(), 1);
    // A different mark appends; a non-adjacent repeat is allowed (the shell
    // genuinely re-prompted at a line it used earlier after scrollback).
    assert!(push_prompt_mark(&mut ring, 20));
    assert!(push_prompt_mark(&mut ring, 10));
    assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![10, 20, 10]);

    // Cap: push well past the limit; length pins at MAX, oldest dropped,
    // newest retained, order preserved.
    let mut ring: VecDeque<u64> = VecDeque::new();
    for i in 0..(MAX_PROMPT_MARKS as u64 + 500) {
        assert!(push_prompt_mark(&mut ring, i));
    }
    assert_eq!(ring.len(), MAX_PROMPT_MARKS);
    assert_eq!(*ring.front().unwrap(), 500); // oldest 500 dropped
    assert_eq!(
        *ring.back().unwrap(),
        MAX_PROMPT_MARKS as u64 + 499 // newest kept
    );
}

#[test]
fn widening_reanchors_only_a_single_row_active_prompt() {
    use std::collections::VecDeque;

    let mut active = VecDeque::from([4, 9]);
    rebase_prompt_marks_after_column_reflow(&mut active, 80, 160, 9, 7);
    assert_eq!(active, VecDeque::from([7]));

    let mut multiline = VecDeque::from([4, 8]);
    rebase_prompt_marks_after_column_reflow(&mut multiline, 80, 160, 9, 7);
    assert!(
        multiline.is_empty(),
        "a prompt starting above the cursor cannot be reconstructed after reflow"
    );

    let mut narrowed = VecDeque::from([9]);
    rebase_prompt_marks_after_column_reflow(&mut narrowed, 160, 80, 9, 12);
    assert!(
        narrowed.is_empty(),
        "narrowing can split even a formerly single-row prompt"
    );

    let mut unchanged_row = VecDeque::from([9]);
    rebase_prompt_marks_after_column_reflow(&mut unchanged_row, 128, 98, 9, 9);
    assert_eq!(
        unchanged_row,
        VecDeque::from([9]),
        "a transient narrowing that added no row keeps the exact prompt anchor"
    );
}

#[test]
fn prompt_row_ids_survive_growth_and_never_alias_evicted_history() {
    // Before the ring reaches capacity, increasing history and decreasing
    // the relative line cancel out, preserving the document row id.
    assert_eq!(stable_grid_line_id(0, 2, -1), 1);
    assert_eq!(stable_grid_line_id(0, 3, -2), 1);
    // Once capacity is full, the origin advances with each eviction and
    // gives replacement content a new id instead of reusing the old one.
    assert_eq!(stable_grid_line_id(1, 3, -2), 2);
}

#[test]
fn prompt_navigation_target_answers_like_the_jump_without_touching_the_ring() {
    use std::collections::VecDeque;

    // The keyboard ownership check asks where a jump would go through the
    // read-only form; it must agree with the pruning jump for every
    // offset and direction, evicted and past-the-grid marks included.
    let marks = VecDeque::from([99, 100, 102, 104, 107, 108]);
    for display_offset in 0..=5 {
        for previous in [true, false] {
            let mut pruned = marks.clone();
            assert_eq!(
                prompt_navigation_target(&marks, 100, 4, 4, display_offset, previous),
                prompt_navigation_offset(&mut pruned, 100, 4, 4, display_offset, previous),
                "offset {display_offset}, previous {previous}"
            );
        }
    }
    assert_eq!(marks, VecDeque::from([99, 100, 102, 104, 107, 108]));
    assert_eq!(prompt_navigation_target(&marks, 110, 0, 4, 0, false), None);
}

#[test]
fn prompt_navigation_prunes_evicted_and_reset_rows() {
    use std::collections::VecDeque;

    // Retained ids are [100, 108): history [100,104), screen [104,108).
    // 99 was evicted; 108 is past the active grid and must also fail
    // closed. The visible top at offset two is row 102.
    let mut ring = VecDeque::from([99, 100, 102, 104, 107, 108]);
    assert_eq!(
        prompt_navigation_offset(&mut ring, 100, 4, 4, 2, true),
        Some(4)
    );
    assert_eq!(
        ring.iter().copied().collect::<Vec<_>>(),
        vec![100, 102, 104, 107]
    );

    // Moving forward from row 102 chooses row 104, which is the active
    // screen top and therefore maps to display offset zero.
    assert_eq!(
        prompt_navigation_offset(&mut ring, 100, 4, 4, 2, false),
        Some(0)
    );

    // A reset advances the origin beyond every old mark; none can alias.
    assert_eq!(
        prompt_navigation_offset(&mut ring, 110, 0, 4, 0, false),
        None
    );
    assert!(ring.is_empty());
}

#[cfg(unix)]
#[test]
fn output_after_widening_reapplies_the_scrollback_byte_budget() {
    for retained in [0, 20] {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut terminal = Terminal::new(
            &["/bin/sh".into(), "-c".into(), "read ready".into()],
            None,
            1000,
            20,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            Arc::new(|| {}),
        )
        .expect("scrollback budget fixture");
        let bytes = scrollback_line_bytes(120) * (24 + 8);
        terminal.set_scrollback_limits(usize::MAX, bytes);
        assert!(terminal.term_config.scrolling_history > 20);
        let mut processor: Processor = Processor::new();
        if retained != 0 {
            processor.advance(
                &mut *terminal.term.lock().unwrap(),
                "\r\n".repeat(24 + retained).as_bytes(),
            );
        }
        let history_before = terminal.term.lock().unwrap().grid().history_size();
        terminal
            .try_resize_geometry(PtyGeometry::new(120, 24, 960, 384))
            .unwrap();
        assert_eq!(
            terminal.term.lock().unwrap().grid().history_size(),
            history_before,
            "resize must preserve existing history"
        );
        let mut term = terminal.term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[24;1H");
        processor.advance(&mut *term, "\r\n".repeat(16).as_bytes());
        assert_eq!(term.grid().history_size(), 8);
    }
}

#[test]
fn scrollback_byte_budget_derives_history_lines() {
    let line_bytes = scrollback_line_bytes(100);
    assert!(line_bytes >= 100 * std::mem::size_of::<Cell>());

    assert_eq!(
        effective_scrollback_lines(10_000, 0, 100, 24),
        10_000,
        "0 byte cap preserves line-count-only behavior"
    );
    assert_eq!(
        effective_scrollback_lines(10_000, line_bytes * 34, 100, 24),
        10,
        "byte cap includes visible rows, leftover becomes scrollback"
    );
    assert_eq!(
        effective_scrollback_lines(10_000, line_bytes * 24, 100, 24),
        0,
        "visible screen is protected even when budget leaves no history"
    );
    assert_eq!(
        effective_scrollback_lines(7, line_bytes * 1000, 100, 24),
        7,
        "line-count cap still wins when it is smaller"
    );
}

/// A program's terminal queries, sent through a real PTY and the
/// kettle-vt extractor, get their replies: XTVERSION names Kettle (Claude
/// Code runs its second probe stage, and so turns synchronized output on,
/// only after an answer), and DECXCPR, DECRQSS and XTGETTCAP answer as
/// Neovim reads them.
#[cfg(unix)]
#[test]
fn terminal_queries_get_their_replies_through_a_pty() {
    assert_eq!(
        xtversion(),
        format!("kettle({})", env!("CARGO_PKG_VERSION"))
    );
    // XTVERSION, DECXCPR at 3;5, DECRQSS for SGR after an undercurl, and
    // XTGETTCAP for Tc and an unknown name.
    let queries =
        "\\033[>q\\033[3;5H\\033[?6n\\033[4:3m\\033P$qm\\033\\\\\\033P+q5463;6b75\\033\\\\";
    let argv: Vec<String> = [
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf '{queries}'; sleep 5"),
    ]
    .to_vec();
    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let _term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skipping terminal queries: no PTY ({e})");
            return;
        }
    };
    let expected = format!(
        "\x1bP>|{}\x1b\\\x1b[?3;5R\x1bP1$r0;4:3m\x1b\\\x1bP1+r5463\x1b\\\x1bP0+r6B75\x1b\\",
        xtversion()
    );
    let mut replies = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while replies != expected {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(TermEvent::PtyWrite(text)) => replies.push_str(&text),
            _ if std::time::Instant::now() > deadline => {
                panic!("replies {replies:?}, expected {expected:?}")
            }
            _ => {}
        }
    }
}

/// A pane spawned with a known scheme answers `CSI ? 996 n` from the start;
/// one spawned without (headless `kettle exec`) stays silent.
#[cfg(unix)]
#[test]
fn a_new_pane_answers_the_color_scheme_query_from_its_capabilities() {
    for (seed, expected) in [
        (Some(false), Some("\x1b[?997;2n")),
        (Some(true), Some("\x1b[?997;1n")),
        (None, None),
    ] {
        let argv: Vec<String> = [
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf '\\033[?996n'; sleep 5".to_string(),
        ]
        .to_vec();
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let _term = match Terminal::new_with_env_and_output_geometry_and_capabilities(
            &argv,
            None,
            1000,
            0,
            PtyGeometry::new(80, 24, 8, 16),
            false,
            CursorShape::Block,
            None,
            "",
            "",
            &[],
            false,
            false,
            &crate::shell_startup::ShellStartup::default(),
            TerminalCapabilities {
                color_scheme_dark: seed,
                ..TerminalCapabilities::default()
            },
            tx,
            waker,
            None,
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skipping colour scheme seed: no PTY ({e})");
                return;
            }
        };
        let wait = if expected.is_some() { 10 } else { 2 };
        let deadline = std::time::Instant::now() + Duration::from_secs(wait);
        let mut reply = None;
        while std::time::Instant::now() < deadline && reply.is_none() {
            if let Ok(TermEvent::PtyWrite(text)) = rx.recv_timeout(Duration::from_millis(100))
                && text.starts_with("\x1b[?997")
            {
                reply = Some(text);
            }
        }
        assert_eq!(reply.as_deref(), expected, "seed {seed:?}");
    }
}

/// A resize moves text without output, so caches of grid positions need a
/// counter that changes with every geometry commit, and only then.
#[cfg(unix)]
#[test]
fn the_geometry_generation_counts_resizes_not_output() {
    let argv: Vec<String> = [
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf hello; sleep 5".to_string(),
    ]
    .to_vec();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let mut term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skipping geometry generation: no PTY ({e})");
            return;
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while term.output_generation() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(term.output_generation() > 0, "the shell printed");
    let start = term.geometry_generation();
    // The same geometry again is a no-op.
    term.resize(80, 24, 8, 16).unwrap();
    assert_eq!(term.geometry_generation(), start);
    // A column change reflows; a row change moves the viewport.
    term.resize(60, 24, 8, 16).unwrap();
    let reflowed = term.geometry_generation();
    assert!(reflowed > start, "a column change counts");
    term.resize(60, 30, 8, 16).unwrap();
    assert!(term.geometry_generation() > reflowed, "a row change counts");
    // Back to the first size: still a new generation, not the old one.
    term.resize(80, 24, 8, 16).unwrap();
    assert!(term.geometry_generation() > reflowed + 1);
}

/// A program that turned on DEC mode 2031 hears each colour change from
/// `Terminal::set_color_scheme`.
#[cfg(unix)]
#[test]
fn color_scheme_changes_reach_a_program_that_asked() {
    use alacritty_terminal::term::TermMode;

    let argv: Vec<String> = [
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '\\033[?2031h'; sleep 5".to_string(),
    ]
    .to_vec();
    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skipping colour scheme: no PTY ({e})");
            return;
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !term
        .term
        .lock()
        .is_ok_and(|t| t.mode().contains(TermMode::COLOR_SCHEME_REPORTS))
    {
        assert!(std::time::Instant::now() < deadline, "mode 2031 never set");
        std::thread::sleep(Duration::from_millis(20));
    }
    let next_reply = || loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(TermEvent::PtyWrite(text)) => return text,
            Ok(_) => continue,
            Err(e) => panic!("no colour-scheme report: {e}"),
        }
    };
    term.set_color_scheme(Some(true));
    assert_eq!(next_reply(), "\x1b[?997;1n");
    term.set_color_scheme(Some(false));
    assert_eq!(next_reply(), "\x1b[?997;2n");
}

#[cfg(unix)]
fn ordinary_graphics_ack_roundtrip(synchronized: bool) {
    use super::{CursorShape, TermEvent, Terminal};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let start = if synchronized { r"\033[?2026h" } else { "" };
    let end = if synchronized { r"\033[?2026l" } else { "" };
    let script = format!(
        r#"stty raw -echo; printf '{start}\033_Ga=T,i=91,f=24,s=1,v=1,C=1;AAAA\033\\'; reply=$(dd bs=1 count=12 2>/dev/null | od -An -tx1 | tr -d ' \n'); test "$reply" = 1b5f47693d39313b4f4b1b5c || exit 7; printf '{end}ACK_ROUNDTRIP_DONE'"#
    );
    let argv = vec!["/bin/sh".into(), "-c".into(), script];
    let (tx, rx) = crossbeam_channel::unbounded();
    let terminal = Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        Arc::new(|| {}),
    )
    .expect("native acknowledgement PTY");
    let child_pid = terminal
        .child_pid()
        .expect("owned Unix acknowledgement child");
    let child_group = unsafe { libc::getpgid(child_pid as libc::pid_t) };
    assert!(child_group > 0);
    eprintln!("ACK_PTY_OWNED_CHILD pid={child_pid} pgid={child_group}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut replies = 0;
    let code = loop {
        while let Ok(event) = rx.try_recv() {
            if let TermEvent::PtyWrite(reply) = event {
                assert_eq!(reply, "\x1b_Gi=91;OK\x1b\\");
                replies += 1;
                terminal.write(reply.as_bytes());
            }
        }
        if let Some(code) = terminal.child_exit_code() {
            break code;
        }
        assert!(
            Instant::now() < deadline,
            "client waited for normal acknowledgement; synchronized={synchronized}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(code, 0, "child checked actual stdin response bytes");
    assert_eq!(replies, 1);
    assert_eq!(
        terminal
            .placements()
            .iter()
            .map(|placement| placement.id)
            .collect::<Vec<_>>(),
        [Some(91)]
    );
}

#[cfg(unix)]
#[test]
fn ordinary_graphics_ack_reaches_a_waiting_pty_child() {
    ordinary_graphics_ack_roundtrip(false);
}

#[cfg(unix)]
#[test]
fn ordinary_graphics_ack_reaches_a_waiting_synchronized_pty_child() {
    ordinary_graphics_ack_roundtrip(true);
}

#[cfg(unix)]
#[test]
fn kitty_query_round_trips_through_a_real_pty_child() {
    let argv = vec!["/bin/sh".into(), "-c".into(),
            r#"stty raw -echo; printf '\033_Gi=91,a=q,t=d,f=24,s=1,v=1;AAAA\033\\'; reply=$(dd bs=1 count=12 2>/dev/null | od -An -tx1 | tr -d ' \n'); test "$reply" = 1b5f47693d39313b4f4b1b5c"#.into()];
    let (tx, rx) = crossbeam_channel::unbounded();
    let term = Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        Arc::new(|| {}),
    )
    .expect("native query PTY");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut replies = 0;
    let code = loop {
        while let Ok(event) = rx.try_recv() {
            if let TermEvent::PtyWrite(reply) = event {
                assert_eq!(reply, "\x1b_Gi=91;OK\x1b\\");
                replies += 1;
                term.write(reply.as_bytes());
            }
        }
        if let Some(code) = term.child_exit_code() {
            break code;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "query child did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(replies, 1);
    assert_eq!(code, 0, "child checked the exact query response bytes");
}

#[cfg(unix)]
#[test]
fn selected_animation_frame_reaches_virtual_and_relative_tiles() {
    for synchronized in [false, true] {
        let start = if synchronized { r"\033[?2026h" } else { "" };
        let end = if synchronized { r"\033[?2026l" } else { "" };
        let placeholder = "\u{10eeee}\u{0305}\u{0305}";
        let script = format!(
            r"printf '{start}\033_Ga=T,i=1,p=1,f=32,s=1,v=1,c=1,r=1,C=1;AQID/w==\033\\\033_Ga=p,i=1,p=2,U=1,c=1,r=1\033\\\033_Ga=p,i=1,p=3,P=1,Q=1,c=1,r=1\033\\\033_Ga=f,i=1,f=32,s=1,v=1;BQYH/w==\033\\\033_Ga=a,i=1,c=2,s=1\033\\\033[1;1H\033[38;2;0;0;1m{placeholder}\033[0m{end}\033[2;1HFRAME_READY'; read done"
        );
        let (tx, _rx) = crossbeam_channel::unbounded();
        let terminal = Terminal::new(
            &["/bin/sh".into(), "-c".into(), script],
            None,
            100,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            Arc::new(|| {}),
        )
        .expect("animation placement fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !terminal
            .screen_text(0)
            .unwrap()
            .text
            .contains("FRAME_READY")
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        for running in [false, true] {
            if running {
                let mut animations = terminal.anims.lock().unwrap();
                let animation = animations.get_mut(&1).unwrap();
                animation.state.running = true;
                animation.state.loops = 1;
                animation.gaps = vec![10, 10];
                animation.started = std::time::Instant::now() - Duration::from_secs(1);
            }
            for placements in [
                terminal.placements(),
                terminal.placeholder_tiles(),
                terminal.relative_tiles(),
            ] {
                assert_eq!(placements.len(), 1);
                assert_eq!(placements[0].img.rgba.as_slice(), &[5, 6, 7, 255]);
            }
            assert_eq!(
                terminal.placeholder_tiles()[0].source_rect,
                Some(ImageSourceRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                })
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn root_frame_edit_reaches_all_placement_kinds_through_a_real_pty() {
    for synchronized in [false, true] {
        let start = if synchronized { r"\033[?2026h" } else { "" };
        let end = if synchronized { r"\033[?2026l" } else { "" };
        let script = format!(
            r"printf '\033_Ga=T,i=1,p=1,f=32,s=1,v=1,c=2,r=2;AQID/w==\033\\\033_Ga=p,i=1,p=2,U=1,c=2,r=2\033\\\033_Ga=p,i=1,p=3,P=1,Q=1,H=1,V=1\033\\{start}\033_Ga=f,i=1,r=1,X=1,f=32,s=1,v=1;BQYH/w==\033\\{end}\r\nROOT_REFRESH_DONE'"
        );
        let argv = vec!["/bin/sh".into(), "-c".into(), script];
        let (tx, _rx) = crossbeam_channel::unbounded();
        let terminal = Terminal::new(
            &argv,
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            Arc::new(|| {}),
        )
        .expect("native root-edit PTY");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if terminal.term.lock().is_ok_and(|term| {
                super::screen_text_of(&term, 0)
                    .text
                    .contains("ROOT_REFRESH_DONE")
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "root edit did not reach the terminal"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let placements = terminal.images.lock().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].img.rgba.as_slice(), &[5, 6, 7, 255]);
        assert_eq!(
            (placements[0].display_cols, placements[0].display_rows),
            (2.0, 2.0)
        );
        assert_eq!(
            terminal.virtuals.lock().unwrap()[&(1, 2)]
                .img
                .rgba
                .as_slice(),
            &[5, 6, 7, 255]
        );
        assert_eq!(
            terminal.relatives.lock().unwrap()[&(1, 3)]
                .img
                .rgba
                .as_slice(),
            &[5, 6, 7, 255]
        );
    }
}

#[cfg(unix)]
fn assert_relative_q_native(
    commands: &str,
    expected: &[(u32, u32, u64, usize)],
    synchronized: bool,
) {
    let setup = r"\033_Ga=t,i=51,f=32,s=1,v=1;AQID/w==\033\\\033[1;1H\033_Ga=p,i=51,p=1,c=1,r=1,C=1\033\\\033[3;4H\033_Ga=p,i=51,p=2,c=1,r=1,C=1\033\\\033_Ga=t,i=52,f=32,s=1,v=1;CQgH/w==\033\\";
    let start = if synchronized { r"\033[?2026h" } else { "" };
    let end = if synchronized { r"\033[?2026l" } else { "" };
    let script =
        format!(r"printf '{start}{setup}{commands}{end}\033[5;1HQ_PLACEMENTS_READY'; read finish");
    let (tx, _rx) = crossbeam_channel::unbounded();
    let terminal = Terminal::new(
        &["/bin/sh".into(), "-c".into(), script],
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        Arc::new(|| {}),
    )
    .expect("native relative-placement PTY");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if terminal.term.lock().is_ok_and(|term| {
            super::screen_text_of(&term, 0)
                .text
                .contains("Q_PLACEMENTS_READY")
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "relative placements did not reach the terminal"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut actual: Vec<_> = terminal
        .relative_tiles()
        .into_iter()
        .map(|p| (p.id.unwrap(), p.placement_id, p.abs_line, p.col))
        .collect();
    actual.sort_unstable();
    assert_eq!(actual, expected, "synchronized={synchronized}");
    terminal.write(b"continue\n");
}

#[cfg(unix)]
#[test]
fn relative_q_selects_exact_parent_placements_through_a_real_pty() {
    for synchronized in [false, true] {
        assert_relative_q_native(
            r"\033_Ga=p,i=52,p=5,P=51,Q=2,H=1,V=0,c=1,r=1\033\\",
            &[(52, 5, 2, 4)],
            synchronized,
        );
    }
}

#[cfg(unix)]
#[test]
fn relative_q_resolves_exact_chained_placements_through_a_real_pty() {
    for synchronized in [false, true] {
        assert_relative_q_native(
            r"\033_Ga=p,i=52,p=5,P=51,Q=2,H=1,V=0,c=1,r=1\033\\\033_Ga=p,i=52,p=6,P=51,Q=1,H=5,V=0,c=1,r=1\033\\\033_Ga=t,i=53,f=32,s=1,v=1;BQYH/w==\033\\\033_Ga=p,i=53,p=7,P=52,Q=5,H=1,V=0,c=1,r=1\033\\",
            &[(52, 5, 2, 4), (52, 6, 0, 5), (53, 7, 2, 5)],
            synchronized,
        );
    }
}

#[cfg(unix)]
#[test]
fn relative_q_omitted_uses_visible_parent_when_a_virtual_prototype_is_hidden() {
    for synchronized in [false, true] {
        assert_relative_q_native(
            r"\033_Ga=p,i=51,p=0,U=1,c=1,r=1\033\\\033_Ga=p,i=52,p=5,P=51,H=1,V=0,c=1,r=1\033\\",
            &[(52, 5, 0, 1)],
            synchronized,
        );
    }
}

#[cfg(unix)]
#[test]
fn relative_q_missing_parent_does_not_fall_back_through_a_real_pty() {
    for synchronized in [false, true] {
        assert_relative_q_native(
            r"\033_Ga=p,i=52,p=5,P=51,Q=99,H=1,V=0,c=1,r=1\033\\",
            &[],
            synchronized,
        );
    }
}

#[cfg(unix)]
#[test]
fn retransmission_releases_old_owners_through_a_real_pty() {
    for synchronized in [false, true] {
        let start = if synchronized { r"\033[?2026h" } else { "" };
        let end = if synchronized { r"\033[?2026l" } else { "" };
        let script = format!(
            r"printf '\033_Ga=T,i=41,p=1,f=32,s=1,v=1;AQID/w==\033\\\033_Ga=p,i=41,p=2,U=1,c=1,r=1\033\\\033_Ga=p,i=41,p=3,P=41,Q=1,H=1,V=1\033\\\033_Ga=f,i=41,f=32,s=1,v=1,z=37;CQgH/w==\033\\\r\nUPLOAD_READY'; read first; printf '{start}\033_Ga=t,i=41,f=32,s=1,v=1,m=1;BQYH\033\\{end}\r\nUPLOAD_RETIRED'; read second; printf '\033_Gm=0;/w==\033\\\033_Ga=p,i=41,p=4\033\\\r\nUPLOAD_COMPLETE'"
        );
        let (tx, _rx) = crossbeam_channel::unbounded();
        let terminal = Terminal::new(
            &["/bin/sh".into(), "-c".into(), script],
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            Arc::new(|| {}),
        )
        .expect("native retransmission PTY");
        let wait_for = |marker: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if terminal
                    .term
                    .lock()
                    .is_ok_and(|term| super::screen_text_of(&term, 0).text.contains(marker))
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "missing PTY marker {marker}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait_for("UPLOAD_READY");
        let root = Arc::downgrade(&terminal.images.lock().unwrap()[0].img.rgba);
        let frame = Arc::downgrade(&terminal.anims.lock().unwrap()[&41].imgs[1].rgba);
        terminal.write(b"continue\n");
        wait_for("UPLOAD_RETIRED");
        assert!(terminal.images.lock().unwrap().is_empty());
        assert!(terminal.virtuals.lock().unwrap().is_empty());
        assert!(terminal.relatives.lock().unwrap().is_empty());
        assert!(terminal.anims.lock().unwrap().is_empty());
        assert!(root.upgrade().is_none());
        assert!(frame.upgrade().is_none());
        terminal.write(b"continue\n");
        wait_for("UPLOAD_COMPLETE");
        let images = terminal.images.lock().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].img.rgba.as_slice(), &[5, 6, 7, 255]);
        assert!(terminal.virtuals.lock().unwrap().is_empty());
        assert!(terminal.relatives.lock().unwrap().is_empty());
        assert!(terminal.anims.lock().unwrap().is_empty());
    }
}

#[cfg(unix)]
#[test]
fn frame_composition_offsets_reach_all_placement_kinds_through_a_real_pty() {
    for synchronized in [false, true] {
        let start = if synchronized { r"\033[?2026h" } else { "" };
        let end = if synchronized { r"\033[?2026l" } else { "" };
        let script = format!(
            r"printf '\033_Ga=T,i=1,p=1,f=32,s=2,v=2,c=2,r=2;/wAA//8AAP//AAD//wAA/w==\033\\\033_Ga=p,i=1,p=2,U=1,c=2,r=2\033\\\033_Ga=p,i=1,p=3,P=1,Q=1,H=1,V=1\033\\\033_Ga=f,i=1,f=32,s=2,v=2;AAD//wD/AP///wD/gACA/w==\033\\{start}\033_Ga=c,i=1,r=2,c=1,w=1,h=1,X=1,Y=1,x=0,y=0,C=1\033\\{end}\r\nCOMPOSE_DONE'"
        );
        let argv = vec!["/bin/sh".into(), "-c".into(), script];
        let (tx, _rx) = crossbeam_channel::unbounded();
        let terminal = Terminal::new(
            &argv,
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            Arc::new(|| {}),
        )
        .expect("native frame-composition PTY");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if terminal.term.lock().is_ok_and(|term| {
                super::screen_text_of(&term, 0)
                    .text
                    .contains("COMPOSE_DONE")
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "frame composition did not reach the terminal"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let placements = terminal.images.lock().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(
            placements[0].img.rgba.as_slice(),
            &[
                128, 0, 128, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255
            ]
        );
        assert_eq!(
            (placements[0].display_cols, placements[0].display_rows),
            (2.0, 2.0)
        );
        assert_eq!(
            terminal.virtuals.lock().unwrap()[&(1, 2)]
                .img
                .rgba
                .as_slice(),
            &[
                128, 0, 128, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255
            ]
        );
        assert_eq!(
            terminal.relatives.lock().unwrap()[&(1, 3)]
                .img
                .rgba
                .as_slice(),
            &[
                128, 0, 128, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255
            ]
        );
    }
}

/// `child_exit_code` must surface the child's real exit status once it
/// exits, because `kettle exec` propagates it as its own process exit code.
/// Spawns a real PTY child that exits 3 and polls.
#[test]
fn child_exit_code_propagates_real_status() {
    // Each argv token is space-free so CommandBuilder quoting can't change
    // the command line shape on either OS.
    #[cfg(windows)]
    let argv: Vec<String> = ["cmd.exe", "/c", "exit", "3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    #[cfg(unix)]
    let argv: Vec<String> = ["/bin/sh", "-c", "exit 3"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(t) => t,
        Err(e) => {
            // Soft-skip in a PTY-less sandbox (existing teardown pattern).
            eprintln!("skipping child_exit_code_propagates_real_status: no PTY ({e})");
            return;
        }
    };
    // The headless run loop MUST forward `PtyWrite` (DA1/DSR/XTGETTCAP
    // query answers) back to the PTY — exactly what `kettle exec` does.
    // Without it the child can park forever under ConPTY: Windows'
    // pseudoconsole withholds the child's clean teardown until the terminal
    // answers its startup cursor-position probe (`ESC[6n`), so `try_wait`
    // never reports an exit.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut saw_exit_event = false;
    let code = loop {
        while let Ok(ev) = rx.try_recv() {
            match ev {
                // Answer the child's terminal queries so it can finish.
                TermEvent::PtyWrite(s) => term.write(s.as_bytes()),
                TermEvent::ChildExit(_) => saw_exit_event = true,
                _ => {}
            }
        }
        saw_exit_event |= term.take_exited();
        if let Some(c) = term.child_exit_code() {
            break c;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child did not exit within 15s (reader Exit event seen: {saw_exit_event})"
        );
        std::thread::sleep(Duration::from_millis(15));
    };
    let _ = saw_exit_event;
    assert_eq!(code, 3, "exit status must propagate verbatim");
    assert!(
        term.child_exited(),
        "child_exited agrees once code is known"
    );
}

/// Run `printf FORMAT` in a real PTY child and return the pane once
/// `ready` accepts it, or after `wait` whatever it shows. The child sleeps
/// after printing, so its modes stay while the test looks; dropping the
/// returned terminal ends it.
#[cfg(unix)]
fn pane_after(format: &str, ready: impl Fn(&Terminal) -> bool, wait: Duration) -> Option<Terminal> {
    pane_running(&format!("printf '{format}'; sleep 10"), ready, wait)
}

/// Run the `/bin/sh` script `script` in a real PTY child, as `pane_after`.
#[cfg(unix)]
fn pane_running(
    script: &str,
    ready: impl Fn(&Terminal) -> bool,
    wait: Duration,
) -> Option<Terminal> {
    let argv: Vec<String> = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    let (tx, _rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skipping keyboard claims: no PTY ({e})");
            return None;
        }
    };
    let deadline = std::time::Instant::now() + wait;
    while !ready(&term) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(term)
}

/// One command line is one command. bash's DEBUG trap marks OutputStart
/// before every simple command and again before PROMPT_COMMAND, so
/// `sleep 0.3; echo b` sends C, C, C, then D: the generation moves by one,
/// and the duration runs from the first mark.
#[cfg(unix)]
#[test]
fn repeated_output_starts_are_one_command() {
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007\\033]133;C\\007'; sleep 0.3; \
                      printf '\\033]133;C\\007\\033]133;C\\007\\033]133;D;7\\007'; sleep 10";
    let Some(pane) = pane_running(script, |_| true, Duration::ZERO) else {
        return;
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut finished = Vec::new();
    while finished.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        finished = pane.drain_command_finished_events();
    }
    assert_eq!(finished.len(), 1, "one CommandEnd, one event");
    assert_eq!(finished[0].generation, 1);
    assert_eq!(finished[0].exit_code, Some(7));
    assert!(
        finished[0].duration >= Duration::from_millis(250),
        "timed from the first mark: {:?}",
        finished[0].duration
    );
    assert_eq!(pane.idle_command_generation(), Some(1));
}

/// The pane's keyboard claims once `done` accepts them (see `pane_after`).
#[cfg(unix)]
fn claims_after(
    format: &str,
    done: impl Fn(&KeyboardClaims) -> bool,
    wait: Duration,
) -> Option<KeyboardClaims> {
    pane_after(format, |t| done(&t.keyboard_claims()), wait).map(|t| t.keyboard_claims())
}

/// A program takes the keyboard by negotiating kitty flags or
/// modifyOtherKeys, switching to the alternate screen, or turning on mouse
/// reporting; bracketed paste, cursor-key mode and focus reports alone do
/// not take it.
#[cfg(unix)]
#[test]
fn keyboard_claims_follow_the_modes_a_program_negotiates() {
    // (sequence, keyboard protocol, mouse, alternate screen)
    for (format, protocol, mouse, alt) in [
        ("\\033[>1u", true, false, false),
        ("\\033[>7u", true, false, false),
        ("\\033[>4;2m", true, false, false),
        ("\\033[?1049h", false, false, true),
        ("\\033[?1000h", false, true, false),
        ("\\033[?1003h\\033[?1006h", false, true, false),
    ] {
        let Some(claims) = claims_after(
            format,
            |c| c.program_owns_keyboard(),
            Duration::from_secs(10),
        ) else {
            return;
        };
        assert_eq!(claims.keyboard_protocol, protocol, "{format}");
        assert_eq!(claims.mouse_mode, mouse, "{format}");
        assert_eq!(claims.alt_screen, alt, "{format}");
        assert!(!claims.at_prompt, "{format}: no shell integration");
        assert!(claims.program_owns_keyboard(), "{format}");
    }
    // Modes that change nothing about who reads keys. Wait until the
    // child has certainly set them, so the check cannot pass early.
    let marker = "\\033[?2004h\\033[?1h\\033[?1004h";
    let Some(pane) = pane_after(
        marker,
        |t| {
            t.term.lock().is_ok_and(|term| {
                term.mode().contains(
                    alacritty_terminal::term::TermMode::BRACKETED_PASTE
                        | alacritty_terminal::term::TermMode::APP_CURSOR
                        | alacritty_terminal::term::TermMode::FOCUS_IN_OUT,
                )
            })
        },
        Duration::from_secs(10),
    ) else {
        return;
    };
    assert!(
        pane.term.lock().unwrap().mode().contains(
            alacritty_terminal::term::TermMode::BRACKETED_PASTE
                | alacritty_terminal::term::TermMode::APP_CURSOR
                | alacritty_terminal::term::TermMode::FOCUS_IN_OUT
        ),
        "the negative control needs its modes set"
    );
    let claims = pane.keyboard_claims();
    assert!(!claims.keyboard_protocol && !claims.mouse_mode && !claims.alt_screen);
    assert!(!claims.program_owns_keyboard());
}

/// A shell at its integrated prompt keeps the keyboard even when the prompt
/// itself pushes kitty flags, as fish does, from its very first prompt;
/// a command running under the same shell does not. The alternate screen
/// and mouse reporting mean a program even without a command mark, as
/// when a shell key binding starts fzf.
#[cfg(unix)]
#[test]
fn a_shell_prompt_keeps_the_keyboard_despite_its_own_kitty_flags() {
    let prompt = "\\033]133;A\\007$ \\033]133;B\\007";
    for (label, format) in [
        (
            "a fresh prompt, no command yet",
            format!("{prompt}\\033[>1u"),
        ),
        (
            "the prompt after a command",
            format!("{prompt}\\033]133;C\\007out\\n\\033]133;D;0\\007{prompt}\\033[>1u"),
        ),
        (
            "a stray OutputStart before the prompt ends",
            "\\033]133;A\\007\\033]133;C\\007$ \\033]133;B\\007\\033[>1u".to_string(),
        ),
    ] {
        let Some(claims) = claims_after(
            &format,
            |c| c.keyboard_protocol && c.at_prompt,
            Duration::from_secs(10),
        ) else {
            return;
        };
        assert!(claims.keyboard_protocol && claims.at_prompt, "{label}");
        assert!(
            !claims.program_owns_keyboard(),
            "{label}: the shell's own flags"
        );
    }
    // The same flags pushed by a command that is still running.
    let running = format!("{prompt}\\033]133;C\\007\\033[>1u");
    let Some(claims) = claims_after(&running, |c| c.keyboard_protocol, Duration::from_secs(10))
    else {
        return;
    };
    assert!(!claims.at_prompt);
    assert!(claims.program_owns_keyboard());
    // Mouse reporting or the alternate screen at the prompt: a program a
    // key binding started.
    for modes in ["\\033[?1000h\\033[?1006h", "\\033[?1049h"] {
        let Some(claims) = claims_after(
            &format!("{prompt}{modes}"),
            |c| c.at_prompt && (c.mouse_mode || c.alt_screen),
            Duration::from_secs(10),
        ) else {
            return;
        };
        assert!(claims.at_prompt, "{modes}");
        assert!(claims.program_owns_keyboard(), "{modes}");
    }
}

/// Prompt navigation is reported only when a jump would move the view:
/// never on the alternate screen, never to a prompt the view already
/// shows at the bottom, and both ways from a view scrolled between two
/// prompts.
#[cfg(unix)]
#[test]
fn keyboard_claims_report_where_a_prompt_jump_would_go() {
    use alacritty_terminal::grid::Scroll;

    let mark = "\\033]133;A\\007$ \\033]133;B\\007\\033]133;C\\007";
    let rows =
        |from: usize| -> String { (from..from + 40).map(|n| format!("line{n}\\n")).collect() };
    // Prompt, 40 rows, prompt, 40 rows: both prompts end up in history.
    let two = format!("{mark}{}{mark}{}", rows(0), rows(40));
    let Some(pane) = pane_after(
        &two,
        |t| t.keyboard_claims().history_size > 50,
        Duration::from_secs(10),
    ) else {
        return;
    };
    let claims = pane.keyboard_claims();
    assert!(claims.history_size > 50);
    assert_eq!(claims.display_offset, 0);
    assert!(claims.prompt_above, "the prompts scrolled into history");
    assert!(!claims.prompt_below, "nothing below the bottom of the view");
    // Scroll the view to the very top: the first prompt is at or above it,
    // the second below it.
    pane.term.lock().unwrap().scroll_display(Scroll::Top);
    let claims = pane.keyboard_claims();
    assert!(claims.display_offset > 0);
    assert!(
        claims.prompt_below,
        "the second prompt lies below: {claims:?}"
    );
    assert!(!claims.prompt_above, "nothing above the top: {claims:?}");

    // A prompt on the visible screen at the bottom is where a jump would
    // already be: neither direction moves the view, so neither counts.
    let on_screen = format!("{}\\033]133;A\\007$ ", rows(0));
    let Some(claims) = claims_after(&on_screen, |c| c.history_size > 10, Duration::from_secs(10))
    else {
        return;
    };
    assert_eq!(claims.display_offset, 0);
    assert!(
        !claims.prompt_below && !claims.prompt_above,
        "a jump to the visible prompt would not move: {claims:?}"
    );

    let Some(claims) = claims_after(
        &(two + "\\033[?1049h"),
        |c| c.alt_screen,
        Duration::from_secs(10),
    ) else {
        return;
    };
    assert!(claims.alt_screen);
    assert!(!claims.prompt_above && !claims.prompt_below);
}

/// Portable ordering model for legacy ConPTY teardown. Closing a
/// pseudoconsole can emit final output and wait for the host to consume
/// it, so the cooperative reader stop must remain false until close
/// returns.
#[test]
fn pty_close_keeps_reader_live_until_close_returns() {
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stop_for_reader = std::sync::Arc::clone(&stop);
    let (final_output_tx, final_output_rx) = crossbeam_channel::bounded(1);
    let (drained_tx, drained_rx) = crossbeam_channel::bounded(1);

    let reader = std::thread::spawn(move || {
        final_output_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("close published final PTY output");
        assert!(
            !stop_for_reader.load(Ordering::Relaxed),
            "reader stop was published before PTY close completed"
        );
        drained_tx.send(()).expect("acknowledge final PTY output");
    });

    close_pty_while_reader_is_live(&stop, || {
        final_output_tx
            .send(())
            .expect("simulate final output from PTY close");
        drained_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reader remained live to drain final PTY output");
    });

    reader.join().expect("reader model thread");
    assert!(
        stop.load(Ordering::Relaxed),
        "reader stop must publish after PTY close returns"
    );
}

#[cfg(windows)]
#[test]
fn asynchronous_conpty_close_owns_reader_stop_across_terminal_drop() {
    let stop = AtomicBool::new(false);
    let close = ConPtyCloseState::default();

    close.terminal_dropped(&stop);
    assert!(
        !stop.load(Ordering::Acquire),
        "dropping Terminal while ClosePseudoConsole is blocked must keep its reader alive"
    );
    close.close_completed(&stop);
    assert!(
        stop.load(Ordering::Acquire),
        "the close owner must stop the reader after the real close returns"
    );

    let stop = AtomicBool::new(false);
    let close = ConPtyCloseState::default();
    close.close_completed(&stop);
    assert!(
        !stop.load(Ordering::Acquire),
        "ordinary close completion still lets the parser consume its EOF marker"
    );
    close.terminal_dropped(&stop);
    assert!(
        stop.load(Ordering::Acquire),
        "a later Terminal drop must observe that close already completed"
    );
}

/// A full parser handoff must not pin the blocking pump during teardown.
/// Once drain mode is published, the pump recovers its buffer and can
/// continue consuming conout without waiting for parser progress.
#[test]
fn full_pump_queue_yields_to_teardown_drain() {
    let (raw_tx, raw_rx) = crossbeam_channel::bounded(1);
    raw_tx
        .send(Some((1, vec![1], 1)))
        .expect("fill bounded parser handoff");

    let drain_output = std::sync::Arc::new(AtomicBool::new(false));
    let drain_for_pump = std::sync::Arc::clone(&drain_output);
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let pump_start = std::sync::Arc::clone(&start);
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    let pump = std::thread::spawn(move || {
        pump_start.wait();
        let result = forward_pty_buffer_or_drain(&raw_tx, &drain_for_pump, 2, vec![2], 1);
        done_tx.send(result).expect("publish pump handoff result");
    });

    start.wait();
    // Let the sender observe at least one full-queue timeout before
    // switching it into teardown drain mode.
    std::thread::sleep(Duration::from_millis(25));
    drain_output.store(true, Ordering::Release);

    let drained = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("full pump queue must yield to teardown drain");
    assert_eq!(drained, PtyPumpSend::Drain(vec![2]));
    assert_eq!(
        raw_rx.recv().expect("original queued parser chunk"),
        Some((1, vec![1], 1))
    );
    pump.join().expect("pump handoff model thread");
}

#[test]
fn source_progress_tracks_chunks_hidden_in_the_parser_pipeline() {
    let progress = PtyReadProgressState::new();

    progress.mark_chunk_read();
    progress.mark_chunk_read();
    assert_eq!(
        progress.load(),
        PtyReadProgress {
            status: PtyReadStatus::Reading,
            generation: 2,
            pending_chunks: 2,
        }
    );

    progress.mark_chunk_handled();
    assert_eq!(progress.load().pending_chunks, 1);
    progress.mark_chunk_handled();
    assert_eq!(progress.load().pending_chunks, 0);
    progress.set_status(PtyReadStatus::Eof);
    assert_eq!(progress.load().status, PtyReadStatus::Eof);
}

#[test]
fn pty_recycle_keeps_the_full_read_buffer_length() {
    let source = super::production_source();
    assert!(!source.contains("buffer.truncate(n)"));
    assert!(source.contains("private_output_filter.feed(&buffer[..n], tap_raw)"));
    assert!(source.contains("if from_child { &buffer[..n] } else { &[] }"));
    assert!(source.contains("extractor.feed_with(child_bytes"));
}

/// Regression guard (runtime). Dropping a `Terminal` whose
/// child is alive and whose PTY reader is parked in a blocking `read()`
/// must return PROMPTLY. On Windows ConPTY, joining the reader while the
/// master is still open never completes, so the UI thread (which owns the
/// drop on a pane close) deadlocks and the window goes "not responding".
/// This test runs the drop on a worker thread and requires it to finish
/// within 5 seconds.
#[test]
fn drop_is_prompt_with_blocked_reader() {
    // A child that stays alive and quiet, so the reader is parked in a
    // blocking read at drop time: `cmd.exe` waits on stdin; `cat` (no
    // args) blocks reading stdin and emits nothing.
    #[cfg(windows)]
    let argv = vec!["cmd.exe".to_string()];
    #[cfg(unix)]
    let argv = vec!["/bin/cat".to_string()];

    let (tx, _rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(t) => t,
        // A sandbox without a usable PTY (rare on the CI runners) — soft
        // skip rather than red the suite. The deterministic source drift
        // guard below pins the invariant without needing a real PTY.
        Err(e) => {
            eprintln!("skipping drop_is_prompt_with_blocked_reader: no PTY ({e})");
            return;
        }
    };

    // Let the child start and the reader settle into a blocking read.
    std::thread::sleep(Duration::from_millis(300));

    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        drop(term);
        let _ = done_tx.send(());
    });

    assert!(
        done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "Terminal::Drop blocked >5s — the reader-thread join \
             deadlock has regressed (Drop must detach the reader, never join)"
    );
}

/// Native Windows regression for the pre-24H2 failure shape: the child is
/// producing enough output to keep conout active when the pane is dropped.
/// UI-side `Drop` must return promptly, while the detached reaper must also
/// finish (which requires the reader to remain live through
/// `ClosePseudoConsole` on older Windows).
#[test]
#[cfg(windows)]
fn high_output_drop_returns_promptly_and_reaper_finishes() {
    let argv = vec![
        "cmd.exe".to_string(),
        "/d".to_string(),
        "/q".to_string(),
        "/c".to_string(),
        "for /L %i in (1,1,2147483647) do @echo 0123456789abcdef0123456789abcdef".to_string(),
    ];

    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let term = match Terminal::new(
        &argv,
        None,
        1000,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    ) {
        Ok(term) => term,
        Err(error) => {
            eprintln!(
                "skipping high_output_drop_returns_promptly_and_reaper_finishes: \
                     no ConPTY ({error})"
            );
            return;
        }
    };

    // Answer ConPTY's startup terminal queries until enough output has
    // crossed the bounded pump to prove this is the high-output path.
    let output_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while term.output_generation() < (PTY_PUMP_QUEUE_DEPTH as u64 + 2) {
        while let Ok(event) = rx.try_recv() {
            if let TermEvent::PtyWrite(reply) = event {
                term.write(reply.as_bytes());
            }
        }
        assert!(
            std::time::Instant::now() < output_deadline,
            "high-output ConPTY child produced no sustained output"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // The reaper owns the only extra child Arc after `Terminal` has
    // finished dropping, so its disappearance is an observable completion
    // signal without adding production-only teardown state.
    let child = std::sync::Arc::clone(&term.child);
    let stop = std::sync::Arc::clone(&term.stop);
    assert_eq!(std::sync::Arc::strong_count(&child), 2);

    let (drop_tx, drop_rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        drop(term);
        let _ = drop_tx.send(());
    });

    assert!(
        drop_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "Terminal::Drop blocked the caller during high-output teardown"
    );

    let reaper_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::sync::Arc::strong_count(&child) != 1 && std::time::Instant::now() < reaper_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::sync::Arc::strong_count(&child),
        1,
        "detached PTY reaper did not finish after high-output close"
    );
    assert!(
        stop.load(Ordering::Relaxed),
        "reader stop was not published after native PTY close"
    );
}

/// Regression guard (source, deterministic / cross-platform).
/// `Terminal::Drop` must DETACH the reader thread, never `join()` it: a
/// future refactor that re-adds `.join()` reintroduces the Windows
/// UI-thread deadlock. Inspect just the `fn drop` body so doc comments
/// and surrounding code can't skew the check.
#[test]
fn drop_detaches_reader_never_joins() {
    // `production_source` normalizes CRLF to LF, so the patterns below
    // can assume bare `\n`.
    let src = super::production_source();
    // Anchor on the impl, not on the first `fn drop` in the file. This
    // module has more than one `Drop`, and one added above `Terminal`'s
    // would silently retarget this test.
    let impl_start = src
        .find("impl Drop for Terminal {")
        .expect("Terminal::Drop present");
    let start = impl_start
        + src[impl_start..]
            .find("fn drop(&mut self) {")
            .expect("Terminal::Drop body present");
    let rest = &src[start..];
    // The fn body closes at a 4-space-indented `}`; every nested block
    // inside closes at >=8 spaces, so the first `\n    }` is unambiguous.
    let end = rest.find("\n    }").map(|e| e + 5).expect("drop fn close");
    let body = &rest[..end];
    assert!(
        body.contains("reader_thread.take()"),
        "Drop must take() (detach) the reader thread handle"
    );
    assert!(
        !body.contains(".join("),
        "Terminal::Drop must NOT join the PTY reader — joining on the UI \
             thread deadlocks on a blocked ConPTY read"
    );
    let reaper_spawn = body
        .find(".spawn(move ||")
        .expect("PTY reaper closure present");
    let drain_publish = body
        .find("self.drain_output.store(true")
        .expect("teardown drain mode publication present");
    assert!(
        drain_publish < reaper_spawn,
        "Drop must switch the pump to drain mode before detached PTY close"
    );
    assert!(
        !body[..reaper_spawn].contains("stop.store("),
        "Drop must not stop the reader before the detached PTY close starts"
    );
    assert!(
        body.contains("close_pty_while_reader_is_live"),
        "PTY reaper must keep the output reader live through master close"
    );
    // Drop must also close the possibly-blocking pseudoconsole and reap the
    // killed child off-thread so neither ConPTY nor a Unix zombie can
    // block/leak on the UI path.
    assert!(
        body.contains("kettle-pty-reaper")
            && body.contains("drop(master);")
            && body.contains("child.wait()"),
        "Drop must close the master and reap the child in a detached worker"
    );
}

/// A killer that records whether it was asked to terminate anything.
#[derive(Debug, Clone)]
struct RecordingKiller(std::sync::Arc<AtomicBool>);

impl portable_pty::ChildKiller for RecordingKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

#[cfg(unix)]
#[test]
fn teardown_does_not_signal_a_reaped_child() {
    #[derive(Debug)]
    struct Child {
        exited: bool,
        killer: RecordingKiller,
    }
    impl portable_pty::ChildKiller for Child {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killer.kill()
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            self.killer.clone_killer()
        }
    }
    impl portable_pty::Child for Child {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(self
                .exited
                .then(|| portable_pty::ExitStatus::with_exit_code(0)))
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            Some(42)
        }
    }
    for exited in [true, false] {
        let killed = std::sync::Arc::new(AtomicBool::new(false));
        let mut child = Child {
            exited,
            killer: RecordingKiller(killed.clone()),
        };
        super::terminate_unreaped_child(&mut child).unwrap();
        assert_eq!(killed.load(Ordering::SeqCst), !exited);
    }
}

/// The guard exists so a construction failure after `spawn_command` cannot
/// leave the process running. Dropping a `Box<dyn Child>` does not
/// terminate anything, so without this the child simply survived.
#[test]
fn an_armed_guard_kills_the_child_it_covers() {
    let killed = std::sync::Arc::new(AtomicBool::new(false));
    drop(SpawnedChildGuard::arm(Box::new(RecordingKiller(
        std::sync::Arc::clone(&killed),
    ))));
    assert!(
        killed.load(Ordering::SeqCst),
        "a construction failure must terminate the child it started"
    );
}

/// The other half matters just as much: on the success path the child
/// belongs to the returned `Terminal`, and killing it there would close
/// every pane the instant it opened.
#[test]
fn a_disarmed_guard_leaves_the_child_alone() {
    let killed = std::sync::Arc::new(AtomicBool::new(false));
    let mut guard =
        SpawnedChildGuard::arm(Box::new(RecordingKiller(std::sync::Arc::clone(&killed))));
    guard.disarm();
    drop(guard);
    assert!(
        !killed.load(Ordering::SeqCst),
        "a completed construction must keep its child"
    );
}

#[test]
fn direct_child_observation_is_opt_in_for_interactive_callers() {
    assert!(
        !TerminalCapabilities::default().observe_child_exit,
        "headless/default terminals must not allocate the UI-only child observer"
    );
}

#[cfg(unix)]
#[test]
fn unix_pty_watcher_wakes_for_master_output() {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let (reader, mut writer) = UnixStream::pair().expect("create pollable pair");
    let mut watcher = UnixPtyWatcher::new(std::process::id(), reader.as_raw_fd());
    writer.write_all(b"x").expect("make master readable");

    assert_eq!(
        watcher
            .wait(reader.as_raw_fd(), None)
            .expect("watch output"),
        Some(UnixStartupWake::Output)
    );
}

#[cfg(unix)]
#[test]
fn unix_pty_watcher_observes_child_exit_without_reaping_it() {
    use std::os::unix::net::UnixStream;

    let (reader, _writer) = UnixStream::pair().expect("create unreadable pair");
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn silent child");
    let mut watcher = UnixPtyWatcher::new(child.id(), reader.as_raw_fd());

    assert_eq!(
        watcher
            .wait(reader.as_raw_fd(), None)
            .expect("watch child exit"),
        Some(UnixStartupWake::ChildExit)
    );
    assert_eq!(
        child
            .wait()
            .expect("watcher left status for child owner")
            .code(),
        Some(0)
    );
}

#[cfg(unix)]
#[test]
fn unix_pty_watcher_handles_a_child_that_exited_before_registration() {
    use std::os::unix::net::UnixStream;

    let (reader, _writer) = UnixStream::pair().expect("create unreadable pair");
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 7"])
        .spawn()
        .expect("spawn already-exited child");
    std::thread::sleep(Duration::from_millis(25));
    let mut watcher = UnixPtyWatcher::new(child.id(), reader.as_raw_fd());

    assert_eq!(
        watcher
            .wait(reader.as_raw_fd(), None)
            .expect("observe pre-registration exit"),
        Some(UnixStartupWake::ChildExit)
    );
    assert_eq!(
        child.wait().expect("status remains reapable").code(),
        Some(7)
    );
}

#[cfg(unix)]
#[test]
fn unix_pty_watcher_honors_a_bounded_wait() {
    use std::os::unix::net::UnixStream;

    let (reader, _writer) = UnixStream::pair().expect("create unreadable pair");
    let mut watcher = UnixPtyWatcher::new(std::process::id(), reader.as_raw_fd());
    let deadline = std::time::Instant::now() + Duration::from_millis(20);

    assert_eq!(
        watcher
            .wait(reader.as_raw_fd(), Some(deadline))
            .expect("bounded wait"),
        None
    );
    assert!(
        std::time::Instant::now() >= deadline,
        "the watcher returned before its deadline without an event"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn simultaneous_kqueue_output_is_read_before_the_retained_slave_is_dropped() {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0,
        "openpty: {}",
        io::Error::last_os_error()
    );
    let mut gate = [-1; 2];
    assert_eq!(unsafe { libc::pipe(gate.as_mut_ptr()) }, 0);
    let child_pid = unsafe { libc::fork() };
    assert!(child_pid >= 0, "fork: {}", io::Error::last_os_error());
    if child_pid == 0 {
        unsafe {
            libc::close(master);
            libc::close(gate[1]);
            let mut byte = 0u8;
            let _ = libc::read(gate[0], (&mut byte as *mut u8).cast(), 1);
            let _ = libc::write(slave, b"tail".as_ptr().cast(), 4);
            libc::_exit(0);
        }
    }
    // SAFETY: the parent owns each descriptor returned above exactly once.
    let mut master = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(master) });
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let gate_read = unsafe { OwnedFd::from_raw_fd(gate[0]) };
    let gate_write = unsafe { OwnedFd::from_raw_fd(gate[1]) };
    drop(gate_read);
    let mut watcher = UnixPtyWatcher::new(child_pid as u32, master.as_raw_fd());
    assert_eq!(
        unsafe { libc::write(gate_write.as_raw_fd(), b"x".as_ptr().cast(), 1) },
        1
    );
    drop(gate_write);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while unix_child_exit_code_unreaped(child_pid as u32)
        .expect("observe child without reaping")
        .is_none()
    {
        assert!(std::time::Instant::now() < deadline, "child did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        watcher
            .wait(master.as_raw_fd(), None)
            .expect("wait for both edges"),
        Some(UnixStartupWake::OutputAndChildExit)
    );
    let mut tail = [0u8; 4];
    master
        .read_exact(&mut tail)
        .expect("read before slave release");
    assert_eq!(&tail, b"tail");
    drop(slave);
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(child_pid, &mut status, 0) },
        child_pid
    );
    assert!(libc::WIFEXITED(status));
}

#[cfg(windows)]
#[test]
fn windows_child_observer_does_not_consume_process_status() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/c", "exit", "9"])
        .spawn()
        .expect("spawn short Windows child");
    let observed = Arc::new(Mutex::new(None));
    let lifecycle_pending = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let waker: Waker = Arc::new(move || {
        let _ = wake_tx.send(());
    });
    spawn_windows_child_exit_observer(
        &child,
        Arc::clone(&observed),
        Arc::clone(&lifecycle_pending),
        waker,
    )
    .expect("spawn process-handle observer");

    wake_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("observer wakes on process exit");
    assert!(observed.lock().unwrap().is_some());
    assert!(lifecycle_pending.load(Ordering::Acquire));
    assert!(
        wake_rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "the observer must exit after one process edge instead of retaining a handle and thread for ten seconds"
    );
    assert_eq!(
        portable_pty::Child::wait(&mut child)
            .expect("original handle still owns status")
            .exit_code(),
        9
    );
}

/// A daemonized descendant can keep the PTY slave open after the direct
/// shell exits. The GUI reader must still emit its ordered Exit marker
/// after the bounded drain rather than leaving Close/Restart/Hold pending
/// forever. Linux supplies `setsid(1)` for this exact containment escape;
/// the companion headless test uses the same fixture shape.
#[cfg(target_os = "linux")]
#[test]
fn leaked_slave_cannot_hold_the_terminal_exit_event_forever() {
    struct KillOnDrop(Option<libc::pid_t>);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                };
            }
        }
    }

    // The child writes its own PID before waking the parent. Reporting
    // `$!` after a parent-side sleep only proved that `fork` happened; the
    // parent could exit before the child installed its HUP policy, turning
    // this retained-slave fixture into an ordinary-EOF fixture.
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "ready=; trap 'ready=1' USR1; \
             setsid sh -c 'trap \"\" HUP; \
             printf \"LEAKED_SLAVE_PID %s\\n\" \"$$\"; \
             kill -USR1 \"$1\"; exec sleep 30' sh \"$$\" & \
             while [ -z \"$ready\" ]; do sleep 0.01; done"
            .to_string(),
    ];
    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = Arc::new(|| {});
    let term = Terminal::new(
        &argv,
        None,
        100,
        80,
        24,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        tx,
        waker,
    )
    .expect("spawn leaked-slave fixture");
    let mut cleanup = KillOnDrop(None);
    let deadline = std::time::Instant::now() + PTY_CHILD_EXIT_EOF_TIMEOUT + Duration::from_secs(3);
    let mut saw_exit = false;
    while std::time::Instant::now() < deadline && !saw_exit {
        while let Ok(event) = rx.try_recv() {
            if let TermEvent::PtyWrite(reply) = event {
                term.write(reply.as_bytes());
            }
        }
        saw_exit |= term.take_exited();
        if cleanup.0.is_none()
            && let Some(screen) = term.screen_text(0)
            && let Some(pid) = screen
                .text
                .split("LEAKED_SLAVE_PID ")
                .nth(1)
                .and_then(|tail| {
                    tail.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .parse::<libc::pid_t>()
                        .ok()
                })
        {
            cleanup.0 = Some(pid);
        }
        if !saw_exit {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let leaked_pid = cleanup
        .0
        .expect("fixture did not report its descendant pid");
    let leaked_stdout = std::fs::read_link(format!("/proc/{leaked_pid}/fd/1"))
        .expect("the detached fixture must still own its PTY stdout");
    assert!(
        leaked_stdout.starts_with("/dev/pts/"),
        "detached fixture stdout is not a PTY slave: {}",
        leaked_stdout.display()
    );
    assert!(
        saw_exit,
        "a retained slave held the terminal Exit event past its bound"
    );
    assert_eq!(term.pty_read_status(), PtyReadStatus::EofTimeout);
    assert_eq!(term.child_exit_code(), Some(0));
}

/// A shell reports the hostname it started with. Without it, every OSC 7
/// report after a macOS rename looks remote and is dropped.
#[test]
fn the_pty_reader_knows_the_hostname_the_shell_starts_with() {
    let src = super::production_source();
    let reader = src
        .find("let mut extractor = Extractor::for_shell();")
        .expect("the PTY reader builds its extractor with for_shell");
    let spawn = src
        .find("let child = pair.slave.spawn_command(cmd)?;")
        .expect("child spawn present");
    assert!(
        reader < spawn,
        "the hostname must be read before the child starts"
    );
    assert!(
        !src.contains("Extractor::new()"),
        "no production extractor may skip the shell's hostname"
    );
}

/// Pin the construction order and the Unix ownership handoff that close the
/// short-child output race. Readiness alone is insufficient: a pump can be
/// descheduled immediately after sending it, so the parent slave must stay
/// alive until the pump owns it.
#[test]
fn the_pty_reader_owns_the_startup_slave_before_the_parent_releases_it() {
    let src = super::production_source();
    let ready = src
        .find("reader_ready_rx\n            .recv()")
        .expect("constructor waits for PTY reader readiness");
    let spawn = src
        .find("let child = pair.slave.spawn_command(cmd)?;")
        .expect("child spawn present");
    let arm = src
        .find("SpawnedChildGuard::arm(child.clone_killer())")
        .expect("spawn guard armed");
    let handoff = src
        .find(".send((pair.slave, lifecycle_watcher))")
        .expect("Unix startup slave is handed to the pump");
    let receive = src
        .find("match startup_slave_rx.recv()")
        .expect("pump receives the Unix startup slave");
    let combined = src
        .find("Ok(Some(UnixStartupWake::OutputAndChildExit)) => {")
        .expect("combined output/exit arm");
    let read = src
        .find("match reader.read(&mut buffer) {")
        .expect("pump read");
    let release_after_read = src
            .find("Ok(n) => {\n                                            #[cfg(unix)]\n                                            startup_slave_guard.take();")
            .expect("pump releases the Unix startup slave after reading");
    let disarm = src.find("spawned.disarm();").expect("spawn guard disarmed");
    let construct = src
        .find("\n        Ok(Terminal {")
        .expect("terminal construction present");

    assert!(
        receive < ready
            && ready < spawn
            && spawn < arm
            && arm < handoff
            && handoff < disarm
            && disarm < construct,
        "the pump must be ready to receive the slave before child spawn; \
             the child and slave ownership guards stay live through handoff"
    );
    assert!(
        receive < release_after_read,
        "the pump must retain the startup slave until a read returns"
    );
    let combined_arm = &src[combined
        ..src[combined..]
            .find("Ok(Some(UnixStartupWake::ChildExit))")
            .map(|offset| combined + offset)
            .expect("child-only exit arm")];
    assert!(
        combined < read
            && read < release_after_read
            && !combined_arm.contains("startup_slave_guard.take()"),
        "simultaneous output/exit must record exit but keep the slave through the read"
    );
    assert!(
        !src[arm..disarm].contains("return Ok("),
        "a success path that returns without disarming would kill a live \
             terminal's child"
    );
}
