struct ForeignKeyFixture {
    ws: WindowState,
    ids: Vec<u64>,
}

impl Drop for ForeignKeyFixture {
    fn drop(&mut self) {
        use std::time::{Duration, Instant};
        for pane in self.ws.mux.panes.values() {
            eprintln!("FOREIGN_KEY_FIXTURE_KILL pid={:?}", pane.term.child_pid());
            pane.term.kill().expect("terminate owned fixture child");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = HashSet::new();
        #[cfg(windows)]
        let mut closing = HashSet::new();
        loop {
            let mut reaped = true;
            for (id, pane) in &mut self.ws.mux.panes {
                // Keep ConPTY startup/teardown moving, including its CPR query.
                for event in pane.rx.try_iter() {
                    if let kettle_core::TermEvent::PtyWrite(reply) = event {
                        // Rejection cases deliberately break the pane input queue.
                        // Keep ConPTY replies moving through the retained terminal.
                        pane.term.write(reply.as_bytes());
                    }
                }
                let code = pane.term.child_exit_code();
                reaped &= code.is_some();
                #[cfg(windows)]
                if code.is_some() && closing.insert(*id) {
                    pane.term
                        .begin_pty_output_close()
                        .expect("close fixture ConPTY");
                }
                if pane.term.take_exited() {
                    exited.insert(*id);
                }
            }
            if reaped && exited.len() == self.ws.mux.panes.len() {
                eprintln!("FOREIGN_KEY_FIXTURE_REAP_PASS panes={}", exited.len());
                break;
            }
            if Instant::now() >= deadline {
                eprintln!("FOREIGN_KEY_FIXTURE_REAP_FAILED");
                assert!(
                    std::thread::panicking(),
                    "fixture children and readers must finish"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn foreign_key_fixture() -> (ForeignKeyFixture, Vec<impl Fn() -> Vec<Vec<u8>>>) {
    let (mut mux, ids) = title_test_tab_mux(4);
    for id in &ids {
        eprintln!(
            "FOREIGN_KEY_FIXTURE_OWNED pid={:?}",
            mux.panes[id].term.child_pid()
        );
        let (tx, _rx) = crossbeam_channel::unbounded();
        // Keep the UI grid deterministic even if the idle child prints a banner.
        mux.panes.get_mut(id).unwrap().term.term =
            Arc::new(std::sync::Mutex::new(alacritty_terminal::Term::new(
                alacritty_terminal::term::Config::default(),
                &kettle_core::term::TermSize::new(80, 24),
                kettle_core::EventProxy::new(tx, Arc::new(|| {})),
            )));
    }
    let captures = ids
        .iter()
        .map(|id| mux.panes.get_mut(id).unwrap().capture_input_for_test())
        .collect();
    for id in &ids[..3] {
        mux.panes.get_mut(id).unwrap().group_name = Some("fleet".into());
    }
    mux.panes.get_mut(&ids[1]).unwrap().read_only = true;
    assert!(mux.focus_pane(ids[0]));
    let ws = WindowState::new(2, mux);
    assert!(ws.window.is_none() && ws.accessibility.is_none());
    // WindowState/Mux have no clipboard. Do not construct App: its startup
    // constructor opens arboard even when no OS window exists yet.
    (ForeignKeyFixture { ws, ids }, captures)
}

fn foreign_key_delivery(
    fixture: &mut ForeignKeyFixture,
    key: &winit::keyboard::Key,
    text: Option<&str>,
) -> PaneInputDelivery {
    let unencoded = fixture.ids[2];
    let (delivery, recipients) = fixture.ws.mux.broadcast_encoded_foreign(
        &BroadcastScope::Group("fleet".into()),
        title_test_config().modify_other_keys,
        false,
        true,
        None,
        |id, mode| {
            (id != unencoded)
                .then(|| {
                    crate::input::encode_key_press(
                        key,
                        winit::keyboard::ModifiersState::empty(),
                        mode,
                    )
                })
                .flatten()
        },
    );
    let expected = if delivery.accepted {
        vec![fixture.ids[0]]
    } else {
        vec![]
    };
    assert_eq!(recipients, expected);
    App::finish_foreign_key_delivery(&mut fixture.ws, delivery, &recipients, (key, text));
    delivery
}

fn observe_foreign_output(ws: &WindowState, pane: u64) {
    let term = ws.mux.panes[&pane].term.term.lock().unwrap();
    let mut outputs = ws.accessibility_output.borrow_mut();
    let state = outputs.entry(pane).or_default();
    let (now, output) =
        kettle_core::grid_text::new_output(term.grid(), state.seen, state.input_row);
    state.update(now, &output, ws.mux.active_focus() == Some(pane), &EN_TEXT);
}

fn foreign_announcement_state(
    ws: &WindowState,
    pane: u64,
) -> (
    bool,
    Option<u64>,
    Option<kettle_core::grid_text::OutputCursor>,
    String,
    u64,
) {
    let outputs = ws.accessibility_output.borrow();
    let state = &outputs[&pane];
    (
        state.typing,
        state.input_row,
        state.seen,
        state.text.clone(),
        state.serial,
    )
}

#[test]
fn foreign_broadcast_enter_resumes_receiver_output_announcements() {
    use winit::keyboard::{Key, NamedKey};
    let (mut fixture, captures) = foreign_key_fixture();
    let accepted = fixture.ids[0];
    let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
    {
        let mut term = fixture.ws.mux.panes[&accepted].term.term.lock().unwrap();
        parser.advance(&mut *term, b"command");
    }
    observe_foreign_output(&fixture.ws, accepted);
    for id in &fixture.ids {
        fixture
            .ws
            .accessibility_output
            .borrow_mut()
            .entry(*id)
            .or_default()
            .typing = true;
    }
    let untouched: Vec<_> = fixture.ids[1..]
        .iter()
        .map(|id| foreign_announcement_state(&fixture.ws, *id))
        .collect();
    let delivery = foreign_key_delivery(&mut fixture, &Key::Named(NamedKey::Enter), Some("\r"));
    assert!(delivery.accepted);
    assert_eq!(
        delivery.result,
        PaneInputResult::ReadOnly,
        "mixed delivery still accepted in one pane"
    );
    assert_eq!(captures[0](), [b"\r".to_vec()]);
    for capture in &captures[1..] {
        assert!(capture().is_empty());
    }
    let input_row = fixture.ws.accessibility_output.borrow()[&accepted].input_row;
    {
        let mut term = fixture.ws.mux.panes[&accepted].term.term.lock().unwrap();
        parser.advance(&mut *term, b"\r\nresult ready\r\n");
    }
    observe_foreign_output(&fixture.ws, accepted);
    let state = foreign_announcement_state(&fixture.ws, accepted);
    assert_eq!(
        state.3, "result ready",
        "accepted foreign Enter must let subsequent output announce"
    );
    assert!(!state.0);
    assert_eq!(input_row, Some(0), "exclude the submitted command echo");
    assert_eq!(state.4, 1);
    for (id, before) in fixture.ids[1..].iter().zip(untouched) {
        assert_eq!(foreign_announcement_state(&fixture.ws, *id), before);
    }
    eprintln!("FOREIGN_BROADCAST_ENTER_A11Y_PASS");
}

#[test]
fn foreign_broadcast_text_tracks_only_accepted_recipients() {
    use winit::keyboard::{Key, NamedKey};
    let (mut fixture, captures) = foreign_key_fixture();
    for id in &fixture.ids {
        fixture
            .ws
            .accessibility_output
            .borrow_mut()
            .insert(*id, super::OutputAnnouncement::default());
    }
    let before: Vec<_> = fixture
        .ids
        .iter()
        .map(|id| foreign_announcement_state(&fixture.ws, *id))
        .collect();
    observe_foreign_output(&fixture.ws, fixture.ids[0]);
    let key = Key::Character("x".into());
    assert!(foreign_key_delivery(&mut fixture, &key, Some("x")).accepted);
    assert_eq!(captures[0](), [b"x".to_vec()]);
    assert!(
        fixture.ws.accessibility_output.borrow()[&fixture.ids[0]].typing,
        "accepted foreign text must set receiver typing state"
    );
    for (id, before) in fixture.ids[1..].iter().zip(&before[1..]) {
        assert_eq!(&foreign_announcement_state(&fixture.ws, *id), before);
    }
    let accepted = fixture.ids[0];
    let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
    parser.advance(
        &mut *fixture.ws.mux.panes[&accepted].term.term.lock().unwrap(),
        b"wrapped input\r\n",
    );
    observe_foreign_output(&fixture.ws, accepted);
    let typed = foreign_announcement_state(&fixture.ws, accepted);
    assert!(
        typed.3.is_empty(),
        "typing suppresses wrapped input announcements"
    );
    assert_eq!(typed.4, 0);
    assert!(foreign_key_delivery(&mut fixture, &Key::Named(NamedKey::ArrowUp), None).accepted);
    assert_eq!(foreign_announcement_state(&fixture.ws, accepted), typed);
    fixture.ws.mux.panes.get_mut(&accepted).unwrap().read_only = true;
    assert!(
        !foreign_key_delivery(&mut fixture, &Key::Named(NamedKey::Enter), Some("\r"),).accepted
    );
    assert_eq!(
        foreign_announcement_state(&fixture.ws, accepted),
        typed,
        "rejected Enter must not clear typing"
    );
    fixture
        .ws
        .accessibility_output
        .borrow_mut()
        .get_mut(&accepted)
        .unwrap()
        .typing = false;
    let rejected = foreign_announcement_state(&fixture.ws, accepted);
    assert!(!foreign_key_delivery(&mut fixture, &key, Some("x")).accepted);
    assert_eq!(
        foreign_announcement_state(&fixture.ws, accepted),
        rejected,
        "rejected text must not set typing"
    );
    for capture in &captures[1..] {
        assert!(capture().is_empty());
    }
    eprintln!("FOREIGN_BROADCAST_TEXT_A11Y_PASS");
}

fn queued_foreign_accessibility_key(
    fixture: &ForeignKeyFixture,
    key: &winit::keyboard::Key,
    text: Option<&str>,
) -> super::QueuedPaste {
    let targets = fixture
        .ws
        .mux
        .foreign_target_ids(&BroadcastScope::Group("fleet".into()))
        .into_iter()
        .filter(|id| *id != fixture.ids[2])
        .filter_map(|id| {
            crate::input::encode_key_press(
                key,
                winit::keyboard::ModifiersState::empty(),
                kettle_core::TermMode::empty(),
            )
            .map(|bytes| (id, bytes))
        })
        .collect();
    let mut queued = key_release_test_paste(
        fixture.ids[0],
        super::QueuedPasteContent::Input {
            targets,
            prepare_input: true,
        },
    );
    queued.accessibility_input = super::AccessibilityInput::from_key(key, text);
    queued.window = fixture.ws.seq;
    queued
}

fn replay_foreign_accessibility_input(
    ws: &mut WindowState,
    queued: &super::QueuedPaste,
) -> PaneInputResult {
    let super::QueuedPasteContent::Input {
        targets,
        prepare_input,
    } = &queued.content
    else {
        panic!("expected queued key");
    };
    App::deliver_deferred_input(
        ws,
        targets,
        &queued.key_presses,
        *prepare_input,
        false,
        queued.accessibility_input,
    )
}
