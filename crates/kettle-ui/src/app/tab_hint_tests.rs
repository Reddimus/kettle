//! The tab strip's go-to-tab shortcuts: clean tabs at rest, a brief chord on
//! a new tab, digits while the modifiers are held, and the key in each tab's
//! tooltip.

use super::chrome::{
    StripTooltipContext, strip_tooltip_target, tab_goto_hint, tab_shortcut_badges,
};

fn seg(idx: usize, x: f32, title: &str) -> kettle_render::TabSeg {
    let rect = (x, 0.0, 160.0, 28.0);
    let close = (x + 132.0, 0.0, 28.0, 28.0);
    let content =
        kettle_render::horizontal_tab_content_layout(rect, true, true, 0.0, title, None, 8.0);
    kettle_render::TabSeg {
        idx,
        rect,
        title_rect: content.title,
        hint: None,
        close,
        title: title.into(),
        path: None,
        active: idx == 0,
        activity: kettle_render::TabActivity::Normal,
    }
}

fn strip(n: usize) -> Vec<kettle_render::TabSeg> {
    (0..n)
        .map(|i| seg(i, i as f32 * 160.0, &format!("tab {}", i + 1)))
        .collect()
}

fn source() -> String {
    super::production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn tabs_at_rest_keep_their_whole_lane_and_a_centred_title() {
    // No inline hint: the lane runs from the leading padding to the close
    // button, the same for every tab, so a short title sits at its centre.
    for seg in strip(3) {
        assert!(seg.hint.is_none());
        assert_eq!(seg.title_rect.0, seg.rect.0 + 8.0);
        assert_eq!(seg.title_rect.0 + seg.title_rect.2, seg.close.0);
    }
    let src = include_str!("chrome.rs");
    let horizontal = src
        .split_once("fn tab_bar_at_height(")
        .unwrap()
        .1
        .split_once("pub(super) fn title_edit_rect(")
        .unwrap()
        .0;
    let compact = horizontal.split_whitespace().collect::<String>();
    assert!(
        compact.contains("&full_label,None,cell_w,);"),
        "no inline hint at rest"
    );
    assert!(compact.contains("hint:None,"));
    assert!(!compact.contains("tab_goto_hint(&self.cfg.keybinds,i)"));
    // `{n}` in `tab-format` still numbers the tabs.
    assert!(compact.contains("(\"n\",&(i+1).to_string()),"));
}

#[test]
fn a_peek_shows_each_visible_tab_digit_without_moving_titles() {
    let bindings = kettle_config::keybinds::defaults();
    let mut segments = strip(11);
    // Tab 6 is scrolled out of view.
    segments[5].rect.0 = super::chrome::PARKED_TAB;
    let before: Vec<_> = segments.iter().map(|s| s.title_rect).collect();
    let badges = tab_shortcut_badges(&segments, &bindings, (true, Some((2, 0.5))), (8.0, 1.0));
    let shown: Vec<_> = badges.iter().map(|b| (b.idx, b.text.as_str())).collect();
    assert_eq!(
        shown,
        [
            (0, "1"),
            (1, "2"),
            (2, "3"),
            (3, "4"),
            (4, "5"),
            (6, "7"),
            (7, "8"),
            (8, "9")
        ],
        "digits on the first nine tabs in view; the peek wins over a fade"
    );
    for badge in &badges {
        assert_eq!(badge.alpha, 1.0);
        let seg = &segments[badge.idx];
        // Inside the tab's title lane, at its trailing end.
        assert!(badge.rect.0 >= seg.title_rect.0);
        assert!(badge.rect.0 + badge.rect.2 <= seg.title_rect.0 + seg.title_rect.2 + 0.5);
        assert!(badge.rect.1 >= seg.rect.1 && badge.rect.1 + badge.rect.3 <= seg.rect.1 + 28.0);
    }
    let after: Vec<_> = segments.iter().map(|s| s.title_rect).collect();
    assert_eq!(before, after);
    // A digit with no go-to-tab key shows nothing.
    let mut unbound = bindings.clone();
    unbound.retain(|_, action| *action != kettle_config::keybinds::Action::GotoTab(1));
    let badges = tab_shortcut_badges(&segments, &unbound, (true, None), (8.0, 1.0));
    assert!(badges.iter().all(|b| b.idx != 1));
}

#[test]
fn a_new_tab_shows_its_whole_chord_while_it_fades() {
    let bindings = kettle_config::keybinds::defaults();
    let segments = strip(3);
    let badges = tab_shortcut_badges(&segments, &bindings, (false, Some((2, 0.25))), (8.0, 1.0));
    assert_eq!(badges.len(), 1);
    assert_eq!(badges[0].idx, 2);
    assert_eq!(Some(badges[0].text.clone()), tab_goto_hint(&bindings, 2));
    assert_eq!(badges[0].alpha, 0.25);
    // At rest, and once faded out, nothing.
    assert!(tab_shortcut_badges(&segments, &bindings, (false, None), (8.0, 1.0)).is_empty());
    assert!(
        tab_shortcut_badges(&segments, &bindings, (false, Some((2, 0.0))), (8.0, 1.0)).is_empty()
    );
    // A tab too narrow for the chord shows none rather than squeezing it.
    let mut narrow = strip(3);
    narrow[2].title_rect.2 = 12.0;
    assert!(
        tab_shortcut_badges(&narrow, &bindings, (false, Some((2, 1.0))), (8.0, 1.0)).is_empty()
    );
}

#[test]
fn every_tab_in_a_multi_tab_strip_names_its_key_in_its_tooltip() {
    let bindings = kettle_config::keybinds::defaults();
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let context = StripTooltipContext {
        modal_open: false,
        bindings: &bindings,
        tr: &tr,
        tab_format: "{title}",
        tab_cell_width: 8.0,
    };
    let mut bar = kettle_render::TabBar::hidden();
    bar.height = 28.0;
    bar.band = (0.0, 0.0, 2000.0, 28.0);
    bar.segments = strip(10);
    bar.segments[1].path = Some("~/Repos/kettle".into());
    let mut ws = crate::window_state::WindowState::new(0, crate::mux::Mux::new());
    ws.window_focused = true;
    let mut at = |bar: &kettle_render::TabBar, x: f64| {
        ws.native_pointer = Some(winit::dpi::PhysicalPosition::new(x, 14.0));
        strip_tooltip_target(&ws, bar, &context)
    };
    let first = at(&bar, 20.0).expect("a short title still names its key");
    let key = tab_goto_hint(&bindings, 0).unwrap();
    assert_eq!(first.text, format!("tab 1  {key}"));
    assert_eq!(first.target, crate::tooltip::TooltipTarget::TabTitle(0));
    // A folder tab names its whole path, then its key.
    let folder = at(&bar, 180.0).unwrap();
    assert_eq!(
        folder.text,
        format!("~/Repos/kettle  {}", tab_goto_hint(&bindings, 1).unwrap())
    );
    // The close button names nothing.
    assert_eq!(at(&bar, 150.0), None);
    // The tenth tab has no key and a whole title: no tooltip.
    assert_eq!(at(&bar, 9.0 * 160.0 + 20.0), None);
    // A single tab keeps the cut-title tooltip only.
    let mut single = kettle_render::TabBar::hidden();
    single.height = 28.0;
    single.segments = strip(1);
    assert_eq!(
        at(&single, 20.0),
        None,
        "a lone tab's whole title has no tooltip"
    );
    single.segments[0].title = "a title much too long for the narrow tab lane".into();
    let cut = at(&single, 20.0).expect("a cut title still shows whole");
    assert_eq!(cut.text, "a title much too long for the narrow tab lane");
    single.lone_title = true;
    assert_eq!(at(&single, 20.0), None);
}

#[test]
fn the_window_drives_the_pills_from_its_events_and_the_scheduler() {
    let source = source();
    // Modifiers arm the peek, and the cursor still follows them.
    let modifiers = source
        .split("WindowEvent::ModifiersChanged(m) => {")
        .nth(1)
        .unwrap()
        .split("WindowEvent::Moved(pos) =>")
        .next()
        .unwrap();
    let peek = modifiers.find("self.sync_tab_hint_peek(ws);").unwrap();
    let cursor = modifiers.find("self.sync_cursor_icon(ws);").unwrap();
    assert!(peek < cursor);
    // A focus loss ends both; any other key pressed ends the peek.
    let focus = source
        .split("WindowEvent::Focused(f) => {")
        .nth(1)
        .unwrap()
        .split("if !f {")
        .nth(1)
        .unwrap();
    assert!(focus.contains("Self::clear_tab_hints(ws);"));
    assert!(source.contains(
        "&& !crate::cursor_exit_log::is_modifier(&event.logical_key, event.physical_key) && ws.tab_hint_peek.clear()"
    ));
    // New Tab, `+` and the new-tab menu arm the fade, before the hook that
    // may switch away.
    assert_eq!(
        source
            .matches("self.arm_new_tab_hint(ws); self.fire_tab_add_event(ws);")
            .count(),
        3
    );
    // The scheduler wakes for them and asks for nothing at rest.
    let wait = source.split("fn about_to_wait_inner(").nth(1).unwrap();
    assert!(wait.contains("let tab_hint_wait = self.tick_tab_hints(ws, now, render_hidden)"));
    assert!(wait.contains("if let Some(next) = tab_hint_wait {"));
    let tick = source
        .split("fn tick_tab_hints(")
        .nth(1)
        .unwrap()
        .split("fn tab_shortcut_badges(")
        .next()
        .unwrap();
    assert!(tick.contains("if quiet { return None; }"));
    // Suppressed while a link is under the pointer, unfocused, under a
    // modal or with one tab, and with reduced motion for the fade.
    assert!(source.contains("&& self.hovered_link_at_cursor(ws).is_none();"));
    assert!(source.contains(
        "ws.window_focused && ws.mux.tabs.len() > 1 && ws.context_menu.is_none() && ws.editing_title.is_none() && !self.any_modal_open(ws)"
    ));
    assert!(source.contains("let reduced = !self.motion_allowed();"));
}
