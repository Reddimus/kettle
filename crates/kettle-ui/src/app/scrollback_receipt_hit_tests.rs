use super::{
    App, MediaPasteReceiptPlacement, PillCovers, scrollback_pill_hit, scrollback_pill_reach,
};
use crate::window_state::WindowState;
use kettle_i18n::{Language, Translator};
use kettle_render::{
    MediaPasteReceiptHit, MediaPasteReceiptKind, MediaPasteReceiptOverlay, Rect4,
    UpdateToastOverlay,
};

struct Fixture {
    ws: WindowState,
    panes: Vec<u64>,
}

impl Fixture {
    fn new(seq: u64) -> Self {
        let (mut mux, panes) = super::tests::title_test_tab_mux(1);
        let first = panes[0];
        mux.split_with(
            crate::mux::Dir::Horizontal,
            &super::tests::title_test_config(),
            100,
            12,
            8,
            16,
            std::sync::Arc::new(|| {}),
            super::tests::title_test_argv(),
            None,
        )
        .unwrap();
        let second = mux.active_focus().unwrap();
        assert!(mux.focus_pane(first));
        let mut fixture = Self {
            ws: WindowState::new(seq, mux),
            panes: vec![first, second],
        };
        assert!(fixture.ws.window.is_none() && fixture.ws.renderer.is_none());
        for pane in &fixture.panes {
            let terminal = &fixture.ws.mux.panes[pane].term;
            eprintln!(
                "SCROLLBACK_RECEIPT_FIXTURE_CHILD {}",
                terminal.child_pid().unwrap()
            );
            let mut term = terminal.term.lock().unwrap();
            term.resize(kettle_core::term::TermSize::new(100, 12));
            let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
            parser.advance(&mut *term, "history\r\n".repeat(200).as_bytes());
        }
        fixture.reset();
        fixture
    }

    fn reset(&mut self) {
        for pane in &self.panes {
            let mut term = self.ws.mux.panes[pane].term.term.lock().unwrap();
            term.scroll_display(kettle_core::Scroll::Bottom);
            term.scroll_display(kettle_core::Scroll::Delta(30));
            assert_eq!(term.grid().display_offset(), 30);
        }
        self.ws.accessibility_pending = false;
    }

    fn offsets(&self) -> Vec<usize> {
        self.panes
            .iter()
            .map(|pane| {
                self.ws.mux.panes[pane]
                    .term
                    .term
                    .lock()
                    .unwrap()
                    .grid()
                    .display_offset()
            })
            .collect()
    }

    fn hit(&self, height: f32, point: (f32, f32)) -> Option<u64> {
        scrollback_pill_hit(
            self.panes.iter().enumerate().filter_map(|(index, pane)| {
                let offset = self.ws.mux.panes[pane]
                    .term
                    .term
                    .lock()
                    .unwrap()
                    .grid()
                    .display_offset();
                let pill = kettle_render::scrollback_pill(
                    (index as f32 * 800.0, 0.0, 800.0, height),
                    offset,
                    1.0,
                    (8.0, 16.0),
                    4.0,
                    Some("Ctrl+End"),
                    &Translator::new(Language::En),
                )?;
                Some((*pane, pill))
            }),
            point,
        )
    }

    fn press(
        &mut self,
        height: f32,
        receipt: Option<&MediaPasteReceiptPlacement>,
        point: (f32, f32),
        button: u8,
    ) -> bool {
        let reached = self.reach(height, receipt, point);
        App::press_reached_scrollback_pill(&mut self.ws, reached, button)
    }

    fn release(
        &mut self,
        height: f32,
        receipt: Option<&MediaPasteReceiptPlacement>,
        point: (f32, f32),
        button: u8,
    ) -> bool {
        let reached = self.reach(height, receipt, point);
        App::release_reached_scrollback_pill(&mut self.ws, reached, button)
    }

    /// The button a press at `point` reaches with `receipt` on screen, by
    /// the rule the window's hover, press and release share.
    fn reach(
        &self,
        height: f32,
        receipt: Option<&MediaPasteReceiptPlacement>,
        point: (f32, f32),
    ) -> Option<u64> {
        let covers = PillCovers {
            receipt: receipt.and_then(MediaPasteReceiptPlacement::geometry),
            ..PillCovers::default()
        };
        self.reach_under(height, &covers, point)
    }

    /// The button a press at `point` reaches under `covers`.
    fn reach_under(&self, height: f32, covers: &PillCovers, point: (f32, f32)) -> Option<u64> {
        scrollback_pill_reach(self.hit(height, point), covers, point)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.ws.mux.kill_children();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !self
            .ws
            .mux
            .panes
            .values()
            .all(|pane| pane.term.child_exited())
        {
            if std::time::Instant::now() >= deadline {
                assert!(std::thread::panicking(), "fixture child did not exit");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for pane in self.ws.mux.panes.values() {
            eprintln!(
                "SCROLLBACK_RECEIPT_FIXTURE_REAPED {} {}",
                pane.term.child_pid().unwrap(),
                pane.term.child_exited()
            );
        }
    }
}

fn placement(height: f32, expanded: bool, notice: Option<Rect4>) -> MediaPasteReceiptPlacement {
    let notices = notice
        .into_iter()
        .map(|rect| UpdateToastOverlay {
            geometry: kettle_render::persistent_notice_geometry(rect, (8.0, 16.0), 1, 0, 7)
                .unwrap(),
            message: "notice".into(),
            details: String::new(),
            close: "Dismiss".into(),
            opacity: 1.0,
            focused: None,
        })
        .collect();
    MediaPasteReceiptPlacement {
        toast: None,
        notices,
        receipt: MediaPasteReceiptOverlay {
            tr: Translator::new(Language::En),
            pane_rect: (0.0, 0.0, 800.0, height),
            grid_rect: (0.0, 0.0, 800.0, height),
            right_gutter: kettle_render::scrollbar_hit_width(4.0, 1.0),
            image: Some(kettle_core::ImageData::solid(1, 1, [0, 0, 0, 255]).unwrap()),
            kind: MediaPasteReceiptKind::Image {
                original_width: 1,
                original_height: 1,
            },
            openable: true,
            remote: false,
            expanded,
            prefer_top: true,
        },
        completion: None,
        cell: (8.0, 16.0),
        text_cell: (8.0, 16.0),
    }
}

#[test]
fn scrollback_receipt_hit_expanded_notice_preserves_receipt_recipient() {
    let mut fixture = Fixture::new(1);
    let stale = placement(192.0, true, None).geometry().unwrap();
    assert_eq!(stale.rect, (448.0, 8.0, 336.0, 112.0));
    let receipt = placement(192.0, true, Some((0.0, 0.0, 800.0, 192.0)));
    assert_eq!(receipt.notices[0].geometry.rect, (0.0, 0.0, 800.0, 56.0));
    let displayed = receipt.geometry().unwrap();
    assert_eq!(displayed.rect, (448.0, 64.0, 336.0, 112.0));
    let point = (740.0, 170.0);
    assert_eq!(
        displayed.hit_test(point.0, point.1),
        Some(MediaPasteReceiptHit::Open)
    );
    assert_eq!(stale.hit_test(point.0, point.1), None);
    assert_eq!(fixture.hit(192.0, point), Some(fixture.panes[0]));
    assert!(
        !fixture.press(192.0, Some(&receipt), point, 0),
        "the displayed receipt must keep the press instead of returning its pane to live output"
    );
    assert_eq!(fixture.ws.scrollback_pill_press, None);
    assert!(!fixture.release(192.0, Some(&receipt), point, 0));
    assert_eq!(fixture.offsets(), [30, 30]);
    assert!(!fixture.ws.accessibility_pending);
}

#[test]
fn scrollback_receipt_hit_uncovered_pills_only_scroll_their_recipient() {
    let mut fixture = Fixture::new(1);
    let other_window = Fixture::new(2);
    for (height, receipt, recipient) in [
        (192.0, None, 0),
        (192.0, Some(placement(192.0, true, None)), 0),
        (
            192.0,
            Some(placement(192.0, false, Some((0.0, 0.0, 800.0, 192.0)))),
            0,
        ),
        (
            320.0,
            Some(placement(320.0, true, Some((0.0, 0.0, 800.0, 320.0)))),
            0,
        ),
        (
            192.0,
            Some(placement(192.0, true, Some((800.0, 0.0, 800.0, 192.0)))),
            0,
        ),
        (
            192.0,
            Some(placement(192.0, true, Some((0.0, 0.0, 800.0, 192.0)))),
            1,
        ),
    ] {
        fixture.reset();
        let point = (740.0 + 800.0 * recipient as f32, height - 22.0);
        assert!(
            receipt
                .as_ref()
                .and_then(|receipt| receipt.geometry())
                .is_none_or(|geometry| geometry.hit_test(point.0, point.1).is_none())
        );
        assert_eq!(fixture.hit(height, point), Some(fixture.panes[recipient]));
        // The press arms the button; its release there is the click.
        assert!(fixture.press(height, receipt.as_ref(), point, 0));
        assert_eq!(
            fixture.ws.scrollback_pill_press,
            Some(fixture.panes[recipient])
        );
        assert_eq!(fixture.offsets(), [30, 30]);
        assert!(!fixture.ws.accessibility_pending);
        assert!(fixture.release(height, receipt.as_ref(), point, 0));
        assert_eq!(fixture.ws.scrollback_pill_press, None);
        let mut expected = [30, 30];
        expected[recipient] = 0;
        assert_eq!(fixture.offsets(), expected);
        assert_eq!(other_window.offsets(), [30, 30]);
        assert!(fixture.ws.accessibility_pending);
    }
}

#[test]
fn scrollback_receipt_hit_secondary_and_outside_presses_preserve_scrollback() {
    let mut fixture = Fixture::new(1);
    let receipt = placement(192.0, true, Some((0.0, 0.0, 800.0, 192.0)));
    for button in [1, 2] {
        assert!(!fixture.press(192.0, Some(&receipt), (740.0, 170.0), button));
        assert!(fixture.press(192.0, None, (740.0, 170.0), button));
        // A middle or right press is taken and arms nothing, so its release
        // is not the button's.
        assert_eq!(fixture.ws.scrollback_pill_press, None);
        assert!(!fixture.release(192.0, None, (740.0, 170.0), button));
        assert_eq!(fixture.offsets(), [30, 30]);
        assert!(!fixture.ws.accessibility_pending);
    }
    for point in [(200.0, 170.0), (500.0, 100.0), (740.0, 0.0)] {
        assert_eq!(fixture.hit(192.0, point), None);
        assert!(!fixture.press(192.0, Some(&receipt), point, 0));
        assert_eq!(fixture.offsets(), [30, 30]);
    }
    App::return_pane_to_live(&mut fixture.ws, fixture.panes[0]);
    fixture.ws.accessibility_pending = false;
    assert_eq!(fixture.hit(192.0, (740.0, 170.0)), None);
    assert!(!fixture.press(192.0, None, (740.0, 170.0), 0));
    assert_eq!(fixture.offsets(), [0, 30]);
    assert!(!fixture.ws.accessibility_pending);
}

/// The jump button is a button: a primary press arms it and shows it
/// pressed, and only a release over the same button returns its pane to
/// live output. A release anywhere else, over the other pane's button, or
/// under a receipt that has since covered it, ends the press and scrolls
/// nothing; so does a release with nothing armed.
#[test]
fn scrollback_receipt_hit_jump_button_acts_on_release_over_the_same_button() {
    let mut fixture = Fixture::new(1);
    let (first, second) = (fixture.panes[0], fixture.panes[1]);
    let on_first = (740.0, 170.0);
    let on_second = (1540.0, 170.0);
    assert_eq!(fixture.hit(192.0, on_first), Some(first));
    assert_eq!(fixture.hit(192.0, on_second), Some(second));
    let covering = placement(192.0, true, Some((0.0, 0.0, 800.0, 192.0)));
    assert!(
        covering
            .geometry()
            .unwrap()
            .hit_test(on_first.0, on_first.1)
            .is_some()
    );

    // Nothing armed: the release is not the button's.
    assert!(!fixture.release(192.0, None, on_first, 0));
    assert_eq!(fixture.offsets(), [30, 30]);

    for (release_at, receipt) in [
        ((200.0, 100.0), None),
        (on_second, None),
        (on_first, Some(&covering)),
    ] {
        fixture.reset();
        assert!(fixture.press(192.0, None, on_first, 0));
        assert_eq!(fixture.ws.scrollback_pill_press, Some(first));
        assert_eq!(fixture.offsets(), [30, 30], "a press alone scrolls nothing");
        assert!(
            fixture.release(192.0, receipt, release_at, 0),
            "the release ends the gesture the button took"
        );
        assert_eq!(fixture.ws.scrollback_pill_press, None);
        assert_eq!(fixture.offsets(), [30, 30], "released at {release_at:?}");
        assert!(!fixture.ws.accessibility_pending);
    }

    fixture.reset();
    assert!(fixture.press(192.0, None, on_first, 0));
    // Another button's release leaves the press armed.
    assert!(!fixture.release(192.0, None, on_first, 2));
    assert_eq!(fixture.ws.scrollback_pill_press, Some(first));
    assert!(fixture.release(192.0, None, on_first, 0));
    assert_eq!(fixture.offsets(), [0, 30]);
    assert!(fixture.ws.accessibility_pending);
}

/// A jump button under something drawn over the panes is neither hovered
/// nor pressed, and the same button uncovered is both. The window stores
/// the reach at the pointer as the hover, which paints the hover fill and
/// gives the hand, and a press and its release ask the same reach, so they
/// cannot disagree. Covered here by a real persistent notice, the search
/// bar's real geometry, a real paste receipt, and while a dialog holds the
/// pointer.
#[test]
fn scrollback_receipt_hit_a_covered_jump_button_is_neither_hovered_nor_pressed() {
    use kettle_render::ScrollbackPillState;
    let mut fixture = Fixture::new(1);
    let pane = fixture.panes[0];
    let tr = Translator::new(Language::En);
    // A short pane: the notice across the window's top and the search bar
    // across its bottom both reach its button.
    let (short, on_short) = (60.0, (740.0, 43.0));
    let notice =
        kettle_render::persistent_notice_geometry((0.0, 0.0, 800.0, 192.0), (8.0, 16.0), 1, 0, 7)
            .expect("a notice");
    let bar = kettle_render::search_bar_geometry(1600.0, short, 8.0, 18.0, &tr);
    let (tall, on_tall) = (192.0, (740.0, 170.0));
    let receipt = placement(tall, true, Some((0.0, 0.0, 800.0, tall)));
    for (what, height, point, covers) in [
        (
            "a notice",
            short,
            on_short,
            PillCovers {
                toasts: vec![notice],
                ..PillCovers::default()
            },
        ),
        (
            "the search bar",
            short,
            on_short,
            PillCovers {
                search: Some(bar.rect),
                ..PillCovers::default()
            },
        ),
        (
            "a receipt",
            tall,
            on_tall,
            PillCovers {
                receipt: receipt.geometry(),
                ..PillCovers::default()
            },
        ),
        (
            "a dialog",
            tall,
            on_tall,
            PillCovers {
                blocked: true,
                ..PillCovers::default()
            },
        ),
    ] {
        fixture.reset();
        assert_eq!(
            fixture.hit(height, point),
            Some(pane),
            "{what}: on the button"
        );
        assert!(covers.cover(point), "{what}: over the button");
        // Covered: no hover fill or hand, and the press is not the button's.
        let hover = fixture.reach_under(height, &covers, point);
        assert_eq!(hover, None, "{what}");
        assert_eq!(
            ScrollbackPillState::of(pane, hover, fixture.ws.scrollback_pill_press),
            ScrollbackPillState::Rest
        );
        let reached = fixture.reach_under(height, &covers, point);
        assert!(!App::press_reached_scrollback_pill(
            &mut fixture.ws,
            reached,
            0
        ));
        assert_eq!(fixture.ws.scrollback_pill_press, None, "{what}");
        assert_eq!(fixture.offsets(), [30, 30], "{what}");
        // Uncovered: hovered, pressed, and released back to live output.
        let open = PillCovers::default();
        let hover = fixture.reach_under(height, &open, point);
        assert_eq!(hover, Some(pane), "{what}");
        assert_eq!(
            ScrollbackPillState::of(pane, hover, fixture.ws.scrollback_pill_press),
            ScrollbackPillState::Hovered
        );
        let reached = fixture.reach_under(height, &open, point);
        assert!(App::press_reached_scrollback_pill(
            &mut fixture.ws,
            reached,
            0
        ));
        assert_eq!(
            ScrollbackPillState::of(pane, hover, fixture.ws.scrollback_pill_press),
            ScrollbackPillState::Pressed
        );
        let reached = fixture.reach_under(height, &open, point);
        assert!(App::release_reached_scrollback_pill(
            &mut fixture.ws,
            reached,
            0
        ));
        assert_eq!(fixture.offsets(), [0, 30], "{what}");
    }
}
