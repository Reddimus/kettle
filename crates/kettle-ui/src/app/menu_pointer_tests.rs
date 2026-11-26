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

#[test]
fn leaving_the_menu_clears_its_highlight_until_a_key_or_the_pointer_lights_a_row() {
    let items = vec![
        leaf("Copy", Action::Copy),
        ContextMenuItem::Separator,
        leaf("Paste", Action::Paste),
        leaf("Select All", Action::SelectAll),
    ];
    let panels = [(10.0, 10.0, 160.0, 80.0)];
    let t0 = Instant::now();
    for (delta, expected) in [(1, 0), (-1, 3)] {
        let mut menu = state(items.clone());
        let mut pointer = MenuPointer::default();
        let mut hover = |menu: &mut ContextMenuState, hit, at, after| {
            menu_pointer_moved(menu, &mut pointer, hit, &panels, at, t0 + ms(after))
        };
        hover(&mut menu, MenuHit::Row(2), (90.0, 50.0), 0);
        assert_eq!(menu.highlight, 2);
        // Out past the panel's edge, or out of the window.
        assert_eq!(
            hover(&mut menu, MenuHit::Outside, (300.0, 50.0), 16),
            MenuHoverEffect::Redraw
        );
        assert_eq!(
            hover(&mut menu, MenuHit::Outside, (310.0, 50.0), 32),
            MenuHoverEffect::None,
            "already clear"
        );
        assert!(
            menu_shown_highlight(&menu, &pointer) >= menu.items.len(),
            "no row is lit"
        );
        assert!(
            menu_activate_click(&menu, &pointer).is_none(),
            "Enter runs nothing"
        );
        // The next arrow starts from the top, or the bottom.
        let next = menu_step(&menu, &mut pointer, delta);
        assert_eq!(next, expected, "delta {delta}");
        menu.highlight = next;
        assert_eq!(menu_shown_highlight(&menu, &pointer), expected);
    }
    // Back over the row the highlight was on lights it again.
    let mut menu = state(items);
    let mut pointer = MenuPointer::default();
    let mut hover = |menu: &mut ContextMenuState, hit, at, after| {
        menu_pointer_moved(menu, &mut pointer, hit, &panels, at, t0 + ms(after))
    };
    hover(&mut menu, MenuHit::Row(2), (90.0, 50.0), 0);
    hover(&mut menu, MenuHit::Outside, (300.0, 50.0), 16);
    assert_eq!(
        hover(&mut menu, MenuHit::Row(2), (90.0, 50.0), 32),
        MenuHoverEffect::Redraw
    );
    assert_eq!(menu_shown_highlight(&menu, &pointer), 2);
}

#[test]
fn leaving_with_a_submenu_open_keeps_the_highlight() {
    let mut cascade = Cascade::new(false);
    assert_eq!(
        cascade.hover(MenuHit::Outside, (90.0, 200.0), 16),
        MenuHoverEffect::None
    );
    assert!(cascade.split_still_open());
    assert_eq!(
        menu_shown_highlight(&cascade.menu, &cascade.pointer),
        cascade.menu.highlight
    );
}

#[test]
fn a_pointer_that_never_entered_the_menu_leaves_the_keys_highlight_alone() {
    // Opened from the keyboard, with the pointer resting over the terminal.
    let mut menu = state(vec![
        leaf("Copy", Action::Copy),
        leaf("Paste", Action::Paste),
    ]);
    menu.highlight = 1;
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 48.0)];
    assert_eq!(
        menu_pointer_moved(
            &mut menu,
            &mut pointer,
            MenuHit::Outside,
            &panels,
            (400.0, 300.0),
            Instant::now()
        ),
        MenuHoverEffect::None
    );
    assert_eq!(menu_shown_highlight(&menu, &pointer), 1);
    assert!(menu_activate_click(&menu, &pointer).is_some());
}

#[test]
fn a_menu_for_the_last_line_flips_above_it() {
    // The audit's capture: the prompt on the last row of an 820 px window,
    // and a 404 px menu, which slid up over the prompt (y 412..816).
    let line = MenuAnchor {
        x: 100.0,
        top: 700.0,
        bottom: 736.0,
    };
    let placement = place_context_menu(line, (300.0, 404.0), (1280.0, 820.0));
    assert_eq!(placement.origin, (100.0, 296.0));
    assert_eq!(placement.max_h, None);
    // A line with room below opens under it, not over its lower half.
    let line = MenuAnchor {
        x: 100.0,
        top: 100.0,
        bottom: 136.0,
    };
    let placement = place_context_menu(line, (300.0, 404.0), (1280.0, 820.0));
    assert_eq!(placement.origin, (100.0, 136.0));
}

#[test]
fn a_menu_too_tall_for_either_side_of_its_line_keeps_the_line_in_view() {
    for (top, bottom) in [(280.0, 316.0), (320.0, 356.0)] {
        let line = MenuAnchor {
            x: 100.0,
            top,
            bottom,
        };
        let placement = place_context_menu(line, (300.0, 500.0), (800.0, 600.0));
        let (_, y) = placement.origin;
        let height = placement.max_h.expect("the menu is held short and scrolls");
        assert!(
            y >= bottom || y + height <= top,
            "{top}..{bottom}: the menu {y}..{} covers the line",
            y + height
        );
        assert!(y >= 4.0 && y + height <= 596.0, "{top}..{bottom}");
        // The roomier side.
        assert_eq!(y >= bottom, 600.0 - bottom >= top, "{top}..{bottom}");
    }
}

#[test]
fn a_pointer_menu_flips_above_the_pointer_near_the_bottom() {
    let placement = place_context_menu(
        MenuAnchor::point(100.0, 500.0),
        (200.0, 300.0),
        (800.0, 600.0),
    );
    assert_eq!(placement.origin, (100.0, 200.0));
    // Too tall for either side: it slides onto the window, as before.
    let placement = place_context_menu(
        MenuAnchor::point(100.0, 300.0),
        (200.0, 590.0),
        (800.0, 600.0),
    );
    assert_eq!(placement.origin, (100.0, 6.0));
    assert_eq!(placement.max_h, None);
    // Asked for above the window's top margin, it stays on the margin.
    let placement = place_context_menu(MenuAnchor::point(0.0, 0.0), (200.0, 300.0), (800.0, 600.0));
    assert_eq!(placement.origin, (4.0, 4.0));
}

#[test]
fn a_menu_opened_from_the_keyboard_is_placed_for_the_cursor_line() {
    let src = super::super::production_source();
    let arm = src
        .split("Action::OpenContextMenu => {")
        .nth(1)
        .and_then(|rest| rest.split("Action::UndoCloseTab => {").next())
        .expect("the action's arm");
    let open = arm
        .find("self.open_context_menu(ws, px, py, false);")
        .expect("the menu opens");
    let placed = arm
        .find("self.place_context_menu_at_line(ws, line);")
        .expect("then sits beside the cursor's line");
    assert!(open < placed);
    assert!(arm.contains("MenuAnchor::line(px, py - cell_h * 0.5, py + cell_h * 0.5)"));
}

#[test]
fn an_open_submenu_closes_once_the_pointer_has_left_every_panel() {
    let mut cascade = Cascade::new(false);
    // Into Split's submenu, then out over the terminal, and still.
    assert_eq!(
        cascade.hover(MenuHit::Row(1), (200.0, 46.0), 16),
        MenuHoverEffect::Redraw
    );
    assert_eq!(
        cascade.hover(MenuHit::Outside, (420.0, 300.0), 32),
        MenuHoverEffect::None
    );
    assert_eq!(
        cascade.hover(MenuHit::Outside, (420.0, 300.0), 32 + 299),
        MenuHoverEffect::None
    );
    assert!(cascade.split_still_open(), "not before the delay");
    // The scheduler's look at the delay's end closes it.
    assert_eq!(
        cascade.hover(MenuHit::Outside, (420.0, 300.0), 32 + 300),
        MenuHoverEffect::Redraw
    );
    assert!(cascade.menu.drill_stack.is_empty(), "the submenu closed");
    assert!(
        menu_shown_highlight(&cascade.menu, &cascade.pointer) >= cascade.menu.items.len(),
        "and the pointer, off the menu, lights nothing"
    );
}

#[test]
fn a_pointer_off_the_panels_but_heading_for_the_submenu_keeps_it() {
    let mut cascade = Cascade::new(false);
    // From Tab's row, out below the menu, toward the submenu's left edge.
    let tab = MenuHit::ParentRow { depth: 0, index: 2 };
    assert_eq!(cascade.hover(tab, (150.0, 74.0), 16), MenuHoverEffect::None);
    for (step, (x, y)) in [(160.0, 83.0), (166.0, 89.0)].into_iter().enumerate() {
        let after = 200 + 150 * step as u64;
        assert_eq!(
            cascade.hover(MenuHit::Outside, (x, y), after),
            MenuHoverEffect::None,
            "step {step}"
        );
        assert!(cascade.split_still_open(), "step {step}");
    }
    // At rest out there, it gives way like any other wait.
    assert_eq!(
        cascade.hover(MenuHit::Outside, (166.0, 89.0), 350 + 300),
        MenuHoverEffect::Redraw
    );
    assert!(cascade.menu.drill_stack.is_empty());
}

#[test]
fn a_submenu_opened_from_the_keyboard_ignores_a_pointer_that_never_came_near() {
    let mut menu = state(vec![
        submenu("Split", Action::SplitRight),
        submenu("Pane", Action::ClosePane),
    ]);
    assert!(menu.enter_submenu(0));
    let mut pointer = MenuPointer::default();
    let panels = [(10.0, 10.0, 160.0, 48.0), (170.0, 10.0, 160.0, 48.0)];
    let t0 = Instant::now();
    for (after, x) in [(0, 500.0), (16, 510.0), (2000, 510.0)] {
        assert_eq!(
            menu_pointer_moved(
                &mut menu,
                &mut pointer,
                MenuHit::Outside,
                &panels,
                (x, 400.0),
                t0 + ms(after)
            ),
            MenuHoverEffect::None
        );
    }
    assert_eq!(menu.drill_stack.len(), 1);
}

/// The audit's overlapping left cascade: on a 340 px window a 200 px parent
/// at x 60 opens its 200 px child at x 4, over the parent, held up to y
/// 100..180 by the window's bottom, so the pointer on the parent's row below
/// reaches the child from beneath it.
fn overlapping_cascade() -> (ContextMenuState, MenuPointer, [kettle_render::Rect4; 2]) {
    let mut menu = state(
        (0..8)
            .map(|row| submenu(&format!("Row {row}"), Action::Paste))
            .collect(),
    );
    menu.highlight = 7;
    assert!(menu.enter_submenu(7));
    (
        menu,
        MenuPointer::default(),
        [(60.0, 40.0, 200.0, 192.0), (4.0, 100.0, 200.0, 80.0)],
    )
}

#[test]
fn heading_for_an_overlapping_submenu_from_below_keeps_it() {
    let (mut menu, mut pointer, panels) = overlapping_cascade();
    let t0 = Instant::now();
    let mut hover = |menu: &mut ContextMenuState, hit, at, after| {
        menu_pointer_moved(menu, &mut pointer, hit, &panels, at, t0 + ms(after))
    };
    // On the open row (y 208..232), under the child.
    let open_row = MenuHit::ParentRow { depth: 0, index: 7 };
    assert_eq!(
        hover(&mut menu, open_row, (170.0, 215.0), 0),
        MenuHoverEffect::None
    );
    // Up the row above it toward (204, 147) on the child's right edge, over
    // 350 ms.
    let row_above = MenuHit::ParentRow { depth: 0, index: 6 };
    for (after, at) in [
        (16, (175.0, 205.0)),
        (200, (180.0, 195.0)),
        (350, (185.0, 185.0)),
    ] {
        assert_eq!(
            hover(&mut menu, row_above, at, after),
            MenuHoverEffect::None,
            "{at:?} after {after} ms"
        );
        assert_eq!(menu.drill_stack.len(), 1, "{at:?} after {after} ms");
    }
    // In through its bottom edge.
    hover(&mut menu, MenuHit::Row(1), (187.0, 170.0), 400);
    assert_eq!(
        (menu.drill_stack.len(), menu.drill_stack[0].highlight),
        (1, 7)
    );
}

#[test]
fn moving_along_the_submenus_edge_line_away_from_it_is_not_aiming() {
    let (mut menu, mut pointer, panels) = overlapping_cascade();
    let t0 = Instant::now();
    let mut hover = |menu: &mut ContextMenuState, hit, at, after| {
        menu_pointer_moved(menu, &mut pointer, hit, &panels, at, t0 + ms(after))
    };
    // Exactly on the line of the child's right edge, x 204, below it, and
    // down that line: the triangle to that edge would have no area.
    let open_row = MenuHit::ParentRow { depth: 0, index: 6 };
    menu.drill_stack[0].highlight = 6;
    assert_eq!(
        hover(&mut menu, open_row, (204.0, 205.0), 0),
        MenuHoverEffect::None
    );
    let row_below = MenuHit::ParentRow { depth: 0, index: 7 };
    for (after, y) in [(16, 212.0), (200, 220.0)] {
        assert_eq!(
            hover(&mut menu, row_below, (204.0, y), after),
            MenuHoverEffect::None
        );
    }
    assert_eq!(
        hover(&mut menu, row_below, (204.0, 228.0), 350),
        MenuHoverEffect::OpenSubmenu(7),
        "300 ms after it turned away, the row below takes over"
    );
}
