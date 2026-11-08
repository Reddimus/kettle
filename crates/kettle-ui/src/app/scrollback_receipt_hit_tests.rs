use super::{App, MediaPasteReceiptPlacement, scrollback_pill_hit};
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
        let hit = self.hit(height, point);
        App::press_scrollback_pill_with_receipt(&mut self.ws, hit, receipt, point, button)
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
        assert!(fixture.press(height, receipt.as_ref(), point, 0));
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
