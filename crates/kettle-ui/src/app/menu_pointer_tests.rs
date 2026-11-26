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
