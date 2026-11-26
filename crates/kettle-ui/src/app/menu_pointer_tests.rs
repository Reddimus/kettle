use super::*;
use std::time::{Duration, Instant};

fn leaf(label: &'static str, action: Action) -> ContextMenuItem {
    ContextMenuItem::Item {
        label,
        action,
        enabled: true,
    }
}

fn submenu(label: &str, action: Action) -> ContextMenuItem {
    ContextMenuItem::Submenu {
        label: label.into(),
        items: vec![leaf("First", action.clone()), leaf("Second", action)],
    }
}

fn state(items: Vec<ContextMenuItem>) -> ContextMenuState {
    ContextMenuState {
        origin: MenuOrigin::Other,
        anchor: (10.0, 10.0),
        anchor_scale: 1.0,
        items,
        highlight: 0,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        typeahead_buf: String::new(),
        typeahead_until: None,
    }
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

#[test]
fn hovering_a_plain_row_lights_it_and_enter_runs_it() {
    let mut menu = state(vec![
        leaf("Copy", Action::Copy),
        ContextMenuItem::Separator,
        leaf("Paste", Action::Paste),
    ]);
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 56.0)];
    let now = Instant::now();
    assert_eq!(
        menu_pointer_moved(
            &mut menu,
            &mut pointer,
            MenuHit::Row(2),
            &panels,
            (90.0, 50.0),
            now
        ),
        MenuHoverEffect::Redraw
    );
    assert_eq!(menu.highlight, 2);
    assert_eq!(menu_shown_highlight(&menu, &pointer), 2);
    assert_eq!(
        menu_pointer_moved(
            &mut menu,
            &mut pointer,
            MenuHit::Row(2),
            &panels,
            (91.0, 50.0),
            now + ms(16)
        ),
        MenuHoverEffect::None,
        "motion within the row repaints nothing"
    );
    assert!(matches!(
        menu_activate_click(&menu, &pointer),
        Some(ContextMenuClick::Action(Action::Paste))
    ));
    // The separator between them is part of the panel and changes nothing.
    assert_eq!(
        menu_pointer_moved(
            &mut menu,
            &mut pointer,
            MenuHit::Panel,
            &panels,
            (90.0, 36.0),
            now + ms(32)
        ),
        MenuHoverEffect::None
    );
    assert_eq!(menu.highlight, 2);
}

#[test]
fn arrows_skip_separators_and_wrap() {
    let menu = state(vec![
        leaf("Copy", Action::Copy),
        ContextMenuItem::Separator,
        leaf("Paste", Action::Paste),
    ]);
    let mut pointer = MenuPointer::default();
    assert_eq!(menu_step(&menu, &mut pointer, 1), 2);
    assert_eq!(menu_step(&menu, &mut pointer, -1), 2);
}

#[test]
fn a_menu_with_room_below_its_pointer_opens_there() {
    let placement = place_context_menu(
        MenuAnchor::point(100.0, 120.0),
        (200.0, 300.0),
        (800.0, 600.0),
    );
    assert_eq!(placement.origin, (100.0, 120.0));
    assert_eq!(placement.max_h, None);
    // A pointer near the right edge moves the menu left to fit.
    let placement = place_context_menu(
        MenuAnchor::point(700.0, 120.0),
        (200.0, 300.0),
        (800.0, 600.0),
    );
    assert_eq!(placement.origin, (596.0, 120.0));
}

#[test]
fn a_submenu_row_hover_reaches_its_submenu() {
    let mut menu = state(vec![
        leaf("Paste", Action::Paste),
        submenu("Split", Action::SplitRight),
    ]);
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 48.0)];
    let t0 = Instant::now();
    // However long the open waits, a pointer resting on the row gets there.
    let opened = (0..=10).any(|step| {
        menu_pointer_moved(
            &mut menu,
            &mut pointer,
            MenuHit::Row(1),
            &panels,
            (90.0, 40.0),
            t0 + ms(100 * step),
        ) == MenuHoverEffect::OpenSubmenu(1)
    });
    assert!(opened);
}

/// Three submenu rows 24 px tall in a panel at (10, 10), Split's open
/// beside it, and the pointer resting on Split.
struct Cascade {
    menu: ContextMenuState,
    pointer: MenuPointer,
    panels: [kettle_render::Rect4; 2],
    t0: Instant,
}

impl Cascade {
    /// `left` puts the submenu on the parent's left, as it flips at the
    /// window's right edge.
    fn new(left: bool) -> Self {
        let (root, child) = if left {
            ((200.0, 10.0, 160.0, 72.0), (40.0, 10.0, 160.0, 96.0))
        } else {
            ((10.0, 10.0, 160.0, 72.0), (170.0, 10.0, 160.0, 96.0))
        };
        let mut menu = state(vec![
            submenu("Split", Action::SplitRight),
            submenu("Pane", Action::ClosePane),
            submenu("Tab", Action::CloseTab),
        ]);
        assert!(menu.enter_submenu(0));
        let mut cascade = Self {
            menu,
            pointer: MenuPointer::default(),
            panels: [root, child],
            t0: Instant::now(),
        };
        let on_split = cascade.at(left, 90.0, 22.0);
        assert_eq!(
            cascade.hover(MenuHit::ParentRow { depth: 0, index: 0 }, on_split, 0),
            MenuHoverEffect::None,
            "the pointer rests on the open row"
        );
        cascade
    }

    /// `(x, y)` measured from the root's left edge, mirrored for a
    /// submenu on the left.
    fn at(&self, left: bool, x: f32, y: f32) -> (f32, f32) {
        if left { (370.0 - x, y) } else { (x, y) }
    }

    fn hover(&mut self, hit: MenuHit, at: (f32, f32), after: u64) -> MenuHoverEffect {
        menu_pointer_moved(
            &mut self.menu,
            &mut self.pointer,
            hit,
            &self.panels,
            at,
            self.t0 + ms(after),
        )
    }

    fn split_still_open(&self) -> bool {
        self.menu.drill_stack.len() == 1 && self.menu.drill_stack[0].highlight == 0
    }
}

#[test]
fn a_submenu_opens_only_after_the_pointer_rests_on_its_row() {
    let t0 = Instant::now();
    let mut menu = state(vec![
        leaf("Paste", Action::Paste),
        submenu("Split", Action::SplitRight),
    ]);
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 48.0)];
    let mut hover = |menu: &mut ContextMenuState, hit, at, after| {
        menu_pointer_moved(menu, &mut pointer, hit, &panels, at, t0 + ms(after))
    };
    // Passing over Split on the way down lights it, opens nothing.
    assert_eq!(
        hover(&mut menu, MenuHit::Row(1), (90.0, 40.0), 0),
        MenuHoverEffect::Redraw,
        "the row lights at once, but its submenu waits"
    );
    assert_eq!(menu.highlight, 1);
    assert_eq!(
        hover(&mut menu, MenuHit::Row(1), (91.0, 41.0), 150),
        MenuHoverEffect::None
    );
    assert!(menu.drill_stack.is_empty());
    // At rest for the dwell, the scheduler's look opens it.
    assert_eq!(
        hover(&mut menu, MenuHit::Row(1), (91.0, 41.0), 200),
        MenuHoverEffect::OpenSubmenu(1)
    );
    // Leaving the row before the dwell ends opens nothing.
    let mut menu = state(vec![
        leaf("Paste", Action::Paste),
        submenu("Split", Action::SplitRight),
    ]);
    assert_ne!(
        hover(&mut menu, MenuHit::Row(1), (90.0, 40.0), 1000),
        MenuHoverEffect::OpenSubmenu(1)
    );
    assert_eq!(
        hover(&mut menu, MenuHit::Row(0), (90.0, 20.0), 1100),
        MenuHoverEffect::Redraw
    );
    assert_eq!(
        hover(&mut menu, MenuHit::Row(0), (90.0, 20.0), 1400),
        MenuHoverEffect::None
    );
    assert_eq!(menu.highlight, 0);
}

#[test]
fn crossing_parent_rows_toward_the_open_submenu_keeps_it() {
    for left in [false, true] {
        let mut cascade = Cascade::new(left);
        // Diagonally down toward the submenu's lower part, over Pane and
        // Tab: the path the audit's probe took to land on Tab.
        for (step, (x, y, index)) in [(120.0, 36.0, 1), (145.0, 52.0, 1), (160.0, 70.0, 2)]
            .into_iter()
            .enumerate()
        {
            let at = cascade.at(left, x, y);
            let after = 16 * (step as u64 + 1);
            assert_eq!(
                cascade.hover(MenuHit::ParentRow { depth: 0, index }, at, after),
                MenuHoverEffect::None,
                "left {left}, step {step}"
            );
            assert!(cascade.split_still_open(), "left {left}, step {step}");
        }
        // Into the submenu: its rows take the hover, Split stays.
        let inside = cascade.at(left, 180.0, 76.0);
        cascade.hover(MenuHit::Row(1), inside, 64);
        assert!(cascade.split_still_open(), "left {left}");
        assert_eq!(cascade.menu.highlight, 1, "left {left}");
    }
}

#[test]
fn leaving_the_aim_switches_after_the_delay() {
    let mut cascade = Cascade::new(false);
    // Straight down onto Pane: away from the submenu.
    let pane = MenuHit::ParentRow { depth: 0, index: 1 };
    assert_eq!(cascade.hover(pane, (90.0, 44.0), 10), MenuHoverEffect::None);
    assert!(cascade.split_still_open());
    assert_eq!(
        cascade.hover(pane, (90.0, 46.0), 200),
        MenuHoverEffect::None
    );
    assert!(
        cascade.split_still_open(),
        "motion off the aim does not reset the wait"
    );
    // 300 ms after it turned away, Pane takes over and, having waited
    // longer than a dwell, opens its own submenu at once.
    assert_eq!(
        cascade.hover(pane, (90.0, 47.0), 310),
        MenuHoverEffect::OpenSubmenu(1)
    );
    assert!(cascade.menu.drill_stack.is_empty());
    assert_eq!(cascade.menu.highlight, 1);
}

#[test]
fn resting_on_another_parent_row_switches_to_it() {
    let mut cascade = Cascade::new(false);
    let pane = MenuHit::ParentRow { depth: 0, index: 1 };
    // Heading for the submenu, then stopping on Pane.
    assert_eq!(
        cascade.hover(pane, (120.0, 36.0), 16),
        MenuHoverEffect::None
    );
    assert_eq!(
        cascade.hover(pane, (120.0, 36.0), 16 + 299),
        MenuHoverEffect::None
    );
    assert!(cascade.split_still_open());
    assert_eq!(
        cascade.hover(pane, (120.0, 36.0), 16 + 300),
        MenuHoverEffect::OpenSubmenu(1)
    );
    assert!(cascade.menu.drill_stack.is_empty());
}

#[test]
fn a_waiting_submenu_wakes_the_scheduler_and_keys_skip_the_wait() {
    let src = super::super::production_source();
    let wait = src
        .split("fn about_to_wait_inner(")
        .nth(1)
        .expect("the scheduler");
    assert!(wait.contains("let menu_pointer_wait = self.context_menu_pointer_tick(ws, now);"));
    assert!(wait.contains("if let Some(next) = menu_pointer_wait {"));
    let left = src
        .split("WindowEvent::CursorLeft { .. } => {")
        .nth(1)
        .and_then(|arm| arm.split("WindowEvent::CursorEntered").next())
        .expect("the CursorLeft arm");
    assert!(left.contains("self.context_menu_pointer_left(ws);"));
    let keys = src
        .split("fn context_menu_key(")
        .nth(1)
        .and_then(|body| body.split("match key {").next())
        .expect("the menu's key handler");
    assert!(keys.contains("ws.context_menu_session.pointer.cancel_pending();"));
}

#[test]
fn coming_back_to_the_open_row_keeps_its_submenu() {
    let mut cascade = Cascade::new(false);
    let pane = MenuHit::ParentRow { depth: 0, index: 1 };
    let split = MenuHit::ParentRow { depth: 0, index: 0 };
    assert_eq!(cascade.hover(pane, (90.0, 44.0), 10), MenuHoverEffect::None);
    assert_eq!(
        cascade.hover(split, (90.0, 30.0), 100),
        MenuHoverEffect::None
    );
    assert_eq!(
        cascade.hover(split, (90.0, 30.0), 1000),
        MenuHoverEffect::None
    );
    assert!(cascade.split_still_open());
}

#[test]
fn hovering_a_row_that_cannot_run_keeps_the_last_highlight() {
    let mut menu = state(vec![
        ContextMenuItem::Info {
            label: "Hold Shift to reach the menu".into(),
            secondary: true,
            continued: false,
        },
        ContextMenuItem::Separator,
        ContextMenuItem::Item {
            label: "Copy",
            action: Action::Copy,
            enabled: false,
        },
        leaf("Paste", Action::Paste),
    ]);
    menu.highlight = 3;
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 80.0)];
    let t0 = Instant::now();
    // The audit's repro: no selection, so Copy is disabled; then the Shift
    // hint's information line.
    for (step, (row, y)) in [(2, 50.0), (0, 20.0)].into_iter().enumerate() {
        assert_eq!(
            menu_pointer_moved(
                &mut menu,
                &mut pointer,
                MenuHit::Row(row),
                &panels,
                (90.0, y),
                t0 + ms(16 * step as u64)
            ),
            MenuHoverEffect::None,
            "row {row}"
        );
        assert_eq!(menu.highlight, 3, "row {row}");
        assert_eq!(menu_shown_highlight(&menu, &pointer), 3, "row {row}");
        assert!(
            matches!(
                menu_activate_click(&menu, &pointer),
                Some(ContextMenuClick::Action(Action::Paste))
            ),
            "Enter runs the row still lit, row {row}"
        );
    }
}

#[test]
fn a_parent_row_that_cannot_run_never_takes_over_the_open_submenu() {
    let mut menu = state(vec![
        ContextMenuItem::Item {
            label: "Copy",
            action: Action::Copy,
            enabled: false,
        },
        submenu("Split", Action::SplitRight),
    ]);
    menu.highlight = 1;
    assert!(menu.enter_submenu(1));
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 48.0), (170.0, 34.0, 160.0, 48.0)];
    let t0 = Instant::now();
    let copy = MenuHit::ParentRow { depth: 0, index: 0 };
    for (after, y) in [(0, 22.0), (16, 21.0), (1000, 21.0)] {
        assert_eq!(
            menu_pointer_moved(
                &mut menu,
                &mut pointer,
                copy,
                &panels,
                (90.0, y),
                t0 + ms(after)
            ),
            MenuHoverEffect::None
        );
    }
    assert_eq!(menu.drill_stack.len(), 1, "Split stays open");
    assert_eq!(menu.drill_stack[0].highlight, 1);
}
