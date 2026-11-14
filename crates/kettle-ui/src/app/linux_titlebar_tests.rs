use super::*;
use kettle_render::WindowControl as C;

#[test]
fn linux_header_policy_preserves_native_and_borderless_choices() {
    let mut cfg = Config::default();
    assert!(linux_titlebar_policy(&cfg));
    cfg.linux_titlebar_style = kettle_config::LinuxTitlebarStyle::Native;
    assert!(!linux_titlebar_policy(&cfg));
    cfg.linux_titlebar_style = kettle_config::LinuxTitlebarStyle::Tabs;
    cfg.borderless = true;
    assert!(!linux_titlebar_policy(&cfg));
    cfg.borderless = false;
    cfg.tab_bar = TabBarMode::Off;
    assert!(!linux_titlebar_policy(&cfg));
    cfg.tab_bar = TabBarMode::Auto;
    for pos in [TabBarPos::Left, TabBarPos::Right, TabBarPos::Bottom] {
        cfg.tab_bar_pos = pos;
        assert!(!linux_titlebar_policy(&cfg));
    }
}

#[test]
fn linux_header_controls_share_hit_bounds_names_and_accessible_actions() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        for scale in [1.0, 1.5, 2.0] {
            for maximized in [false, true] {
                let band = (0.0, 0.0, 800.0 * scale, LINUX_HEADER_HEIGHT * scale);
                let mut bar = TabBar::hidden();
                bar.height = band.3;
                bar.band = band;
                bar.window_controls = linux_window_control_layout(band, maximized, None, None, &tr);
                assert_eq!(
                    bar.window_controls
                        .iter()
                        .map(|b| b.action)
                        .collect::<Vec<_>>(),
                    [
                        C::Minimize,
                        if maximized { C::Restore } else { C::Maximize },
                        C::Close
                    ]
                );
                let nodes = tab_accessibility_projection(&bar, &[], false, &tr);
                for button in &bar.window_controls {
                    let (x, y, w, h) = button.rect;
                    assert_eq!((w, h), (34.0 * scale, 34.0 * scale));
                    assert!(x >= 0.0 && x + w <= band.2 && y >= 6.0 * scale);
                    assert_eq!(
                        window_control_at(&bar, x + w * 0.5, y + h * 0.5),
                        Some(button.action)
                    );
                    assert!(!empty_titlebar_space(&bar, x + w * 0.5, y + h * 0.5));
                    assert!(!button.label.is_empty());
                    let node = nodes
                        .iter()
                        .find(|(_, n)| n.label() == Some(button.label.as_str()))
                        .unwrap();
                    assert_eq!(node.1.role(), Role::Button);
                    assert!(node.1.supports_action(AccessibilityAction::Click));
                    assert_eq!(
                        node.1.bounds().unwrap(),
                        accesskit::Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64)
                    );
                }
                let rect = bar.window_controls[2].rect;
                let hovered = linux_window_control_layout(
                    band,
                    maximized,
                    Some((rect.0 + 1.0, rect.1 + 1.0)),
                    Some(C::Close),
                    &tr,
                );
                assert!(hovered[2].hovered && hovered[2].pressed);
                assert!(!hovered[0].pressed);
            }
        }
    }
}

#[test]
fn linux_header_one_and_many_tabs_leave_controls_and_empty_drag_space() {
    for scale in [1.0, 2.0] {
        for count in [1, 3, 12] {
            let height = LINUX_HEADER_HEIGHT * scale;
            let width = 1000.0 * scale;
            let controls = linux_window_control_layout(
                (0.0, 0.0, width, height),
                false,
                None,
                None,
                &Default::default(),
            );
            let controls_start = width
                - crate::window_buttons::ButtonLayout::default()
                    .reservations(width, height)
                    .1;
            let LinuxHeaderControls {
                menu: arrow,
                plus,
                search,
                lead,
                trail,
            } = linux_header_tab_controls((0.0, 0.0, controls_start, height), scale, true);
            // Find sits 6 points before `≡`, and the tabs end 6 before it.
            assert_eq!(search.0 + search.2 + 6.0 * scale, arrow.0);
            assert_eq!(search.2, 34.0 * scale);
            assert_eq!(trail, controls_start - (search.0 - 6.0 * scale));
            let strip = controls_start - lead - trail;
            let layout =
                titlebar_tab_layout(count, 0, (strip, trail, lead), height, 80.0 * scale, true);
            // `+` leads the header and `≡` ends it, before the window
            // buttons: 34-point buttons, 7 points from the lane's edges and
            // 6 from the tabs.
            assert_eq!(plus.0, 7.0 * scale);
            assert_eq!(arrow.0 + arrow.2, controls_start - 7.0 * scale);
            assert_eq!(lead, plus.0 + plus.2 + 6.0 * scale);
            assert_eq!(plus.2, 34.0 * scale);
            let mut bar = TabBar::hidden();
            bar.height = height;
            bar.band = (0.0, 0.0, width, height);
            bar.new_tab = plus;
            bar.new_tab_menu = arrow;
            bar.window_controls = controls;
            for (idx, x) in layout.xs.iter().enumerate() {
                let Some(x) = x else {
                    continue;
                };
                let w = if idx + 1 == count {
                    layout.last_width
                } else {
                    layout.width
                };
                assert!(*x + w <= strip + 0.01);
                let rect = (lead + *x, 0.0, w, height);
                assert!(rect.0 >= plus.0 + plus.2);
                let mut content = kettle_render::horizontal_tab_content_layout(
                    rect,
                    count > 1,
                    true,
                    0.0,
                    "shell",
                    None,
                    8.0 * scale,
                );
                if count == 1 {
                    content.title = centered_title_lane(content.title, width * 0.5);
                    assert!((content.title.0 + content.title.2 * 0.5 - width * 0.5).abs() < 0.01);
                    assert!(content.title.0 + content.title.2 <= lead + strip);
                }
                bar.segments.push(TabSeg {
                    idx,
                    rect,
                    title_rect: content.title,
                    close: content.close,
                    hint: None,
                    title: "shell".into(),
                    path: None,
                    active: idx == 0,
                    activity: RenderTabActivity::Normal,
                });
            }
            assert!(tab_strip_visible(
                TabBarMode::Auto,
                count,
                false,
                false,
                true
            ));
            assert!(!empty_titlebar_space(&bar, plus.0 + 1.0, height * 0.5));
            assert!(!empty_titlebar_space(&bar, arrow.0 + 1.0, height * 0.5));
            if count == 1 {
                let between = (plus.0 + plus.2 + bar.segments[0].rect.0) * 0.5;
                assert!(empty_titlebar_space(&bar, between, height * 0.5));
            }
        }
        // Even a tiny surface keeps the three window targets disjoint.
        let buttons = linux_window_control_layout(
            (0.0, 0.0, 40.0 * scale, 46.0 * scale),
            false,
            None,
            None,
            &Default::default(),
        );
        for pair in buttons.windows(2) {
            assert!(pair[0].rect.0 + pair[0].rect.2 <= pair[1].rect.0);
        }
    }
}

#[test]
fn linux_header_edges_corners_and_constrained_windows() {
    use winit::window::ResizeDirection as R;
    for scale in [1.0, 2.0] {
        let size = ((800.0 * scale) as u32, (600.0 * scale) as u32);
        for ((x, y), direction) in [
            ((1.0, 1.0), R::NorthWest),
            ((799.0, 1.0), R::NorthEast),
            ((1.0, 599.0), R::SouthWest),
            ((799.0, 599.0), R::SouthEast),
            ((400.0, 1.0), R::North),
            ((400.0, 599.0), R::South),
            ((1.0, 300.0), R::West),
            ((799.0, 300.0), R::East),
        ] {
            assert_eq!(
                linux_resize_edge(size, scale, false, x * scale, y * scale),
                Some(direction)
            );
            assert_ne!(linux_resize_cursor(direction), CursorIcon::Text);
            assert_eq!(
                linux_resize_edge(size, scale, true, x * scale, y * scale),
                None
            );
        }
        for (x, y) in [
            (6.0, 6.0),
            (400.0, 300.0),
            (-1.0, 0.0),
            (800.0, 300.0),
            (400.0, 600.0),
        ] {
            assert_eq!(
                linux_resize_edge(size, scale, false, x * scale, y * scale),
                None
            );
        }
    }
}

#[test]
fn linux_header_empty_space_routes_each_mouse_button() {
    use LinuxTitlebarGesture as G;
    assert_eq!(linux_titlebar_gesture(0, false), G::Drag);
    assert_eq!(linux_titlebar_gesture(0, true), G::ToggleMaximize);
    for double in [false, true] {
        assert_eq!(linux_titlebar_gesture(1, double), G::Ignore);
        assert_eq!(linux_titlebar_gesture(2, double), G::WindowMenu);
        assert_eq!(linux_titlebar_gesture(3, double), G::Ignore);
    }
}

#[test]
fn linux_header_startup_keeps_the_requested_grid_and_content_origin() {
    let cfg = Config {
        window_width: Some(100),
        window_height: Some(40),
        ..Default::default()
    };
    for scale in [1.0, 2.0] {
        let mut ws = WindowState::new(0, Mux::new());
        ws.macos_titlebar_tabs = true;
        ws.linux_titlebar = true;
        ws.startup_scale = scale;
        let header = App::macos_titlebar_band(&ws).unwrap();
        assert_eq!(header.height, 46.0 * scale);
        assert_eq!(header.left, 0.0);
        let cell = (9.25 * scale, 18.5 * scale);
        let initial = startup_surface_for(&cfg, cell, ContentBands::default(), None);
        let corrected = linux_startup_surface_with_header(&cfg, initial, 0.0, scale, None);
        let area = content_rect_for_with_strip(
            corrected,
            ContentBands {
                tab_bar_h: header.height,
                ..Default::default()
            },
            TabBarPos::Top,
            StatusBarMode::Off,
            180.0,
        );
        assert_eq!(area.1, header.height);
        assert_eq!(
            grid_dims_px((area.2, area.3), cell, (cfg.padding_x, cfg.padding_y)),
            (100, 40)
        );
        let existing = (initial.0, initial.1 + (28.0 * scale) as u32);
        assert_eq!(
            linux_startup_surface_with_header(&cfg, existing, 28.0 * scale, scale, None),
            corrected
        );
    }
}

#[test]
fn linux_header_winit_extension_preserves_ssd_negotiation_and_avoids_text_renderers() {
    let manifest = include_str!("../../Cargo.toml");
    let features = manifest
        .split("winit = {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(features.contains("\"wayland-csd-adwaita-notitle\""));
    for renderer in [
        "\"wayland-csd-adwaita\"",
        "\"wayland-csd-adwaita-crossfont\"",
    ] {
        assert!(!features.contains(renderer));
    }
    // Kettle's header uses stock winit: the window is created without
    // decorations only where the desktop has no server-side frame.
    let windowing = include_str!("windowing.rs");
    assert!(windowing.contains("attrs = attrs.with_decorations(false);"));
    assert!(windowing.contains("desktop_lacks_server_decorations("));
}

#[test]
fn linux_header_left_buttons_clear_the_tab_and_editor_lane() {
    let preferences =
        crate::window_buttons::ButtonLayout::parse("close,minimize:maximize").unwrap();
    let band = (0.0, 0.0, 800.0, 46.0);
    let buttons = linux_window_control_layout_with_preferences(
        band,
        false,
        None,
        None,
        &Default::default(),
        &preferences,
    );
    assert_eq!(
        buttons.iter().map(|b| b.action).collect::<Vec<_>>(),
        [C::Close, C::Minimize, C::Maximize]
    );
    let (left, right) = preferences.reservations(band.2, band.3);
    assert_eq!((left, right), (78.0, 41.0));
    assert!(buttons[1].rect.0 + buttons[1].rect.2 <= left);
    assert!(buttons[2].rect.0 >= band.2 - right);
    let no_buttons = crate::window_buttons::ButtonLayout::parse(":").unwrap();
    assert!(
        linux_window_control_layout_with_preferences(
            band,
            false,
            None,
            None,
            &Default::default(),
            &no_buttons
        )
        .is_empty()
    );
}

#[test]
fn linux_header_new_controls_clear_buttons_at_every_width_and_scale() {
    for scale in [1.0, 1.5, 2.0] {
        for width in [0.0, 1.0, 40.0, 120.0, 180.0, 300.0, 800.0] {
            for layout in [
                ":minimize,maximize,close",
                "close,minimize:maximize",
                ":",
                "close:",
            ] {
                let preferences = crate::window_buttons::ButtonLayout::parse(layout).unwrap();
                let band = (
                    10.0 * scale,
                    0.0,
                    width * scale,
                    LINUX_HEADER_HEIGHT * scale,
                );
                let (left, right) = preferences.reservations(band.2, band.3);
                let lane = (
                    band.0 + left,
                    band.1,
                    (band.2 - left - right).max(0.0),
                    band.3,
                );
                for dropdown in [false, true] {
                    let LinuxHeaderControls {
                        menu,
                        plus,
                        search,
                        lead,
                        trail,
                    } = linux_header_tab_controls(lane, scale, dropdown);
                    assert!(lead >= 0.0 && trail >= 0.0 && lead + trail <= lane.2 + 0.01);
                    // An absent `≡` or Find is the usual zero rect.
                    for target in [menu, plus, search].into_iter().filter(|r| r.2 > 0.0) {
                        assert!(target.0 >= lane.0 - 0.01);
                        assert!(target.0 + target.2 <= lane.0 + lane.2 + 0.01);
                        assert!(target.2 >= 0.0 && target.2 <= 34.0 * scale + 0.01);
                    }
                    // `+` leads and `≡` ends the lane, never overlapping.
                    if menu.2 > 0.0 && plus.2 > 0.0 {
                        assert!(plus.0 + plus.2 + 6.0 * scale <= menu.0 + 0.01);
                    }
                    assert_eq!(
                        menu.2 > 0.0,
                        dropdown && lane.2 >= 2.0 * (41.0 * scale) + 6.0 * scale
                    );
                    // Find gives way before `≡`, and never overlaps `+`.
                    assert_eq!(
                        search.2 > 0.0,
                        menu.2 > 0.0 && lane.2 >= 2.0 * (41.0 * scale) + 46.0 * scale - 0.01,
                        "{scale}x {width} {layout:?}"
                    );
                    if search.2 > 0.0 {
                        assert!(plus.0 + plus.2 + 6.0 * scale <= search.0 + 0.01);
                        assert!((search.0 + search.2 + 6.0 * scale - menu.0).abs() < 0.01);
                    }
                }
            }
        }
    }
}

#[test]
fn linux_header_tooltips_follow_actual_control_and_press_state() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let mut buttons =
            linux_window_control_layout((0.0, 0.0, 800.0, 46.0), true, None, None, &tr);
        assert!(window_control_tooltip_label(&buttons).is_none());
        buttons[1].hovered = true;
        assert_eq!(
            window_control_tooltip_label(&buttons),
            Some(tr.text(kettle_i18n::Text::A11yWindowRestore))
        );
        buttons[1].pressed = true;
        assert!(window_control_tooltip_label(&buttons).is_none());
    }
}

#[test]
fn linux_header_ctl_geometry_matches_paint_hit_tests_and_accesskit() {
    let mut bar = TabBar::hidden();
    bar.height = 46.0;
    bar.window_controls = linux_window_control_layout(
        (0.0, 0.0, 800.0, 46.0),
        true,
        Some((778.0, 23.0)),
        Some(C::Close),
        &Default::default(),
    );
    let geometry = super::ctl::window_control_geometry(&bar);
    let nodes = tab_accessibility_projection(&bar, &[], false, &Default::default());
    for (button, json) in bar.window_controls.iter().zip(geometry.as_array().unwrap()) {
        let (x, y, w, h) = button.rect;
        assert_eq!(json["action"], button.action.as_str());
        assert_eq!(
            json["rect"],
            serde_json::json!({"x":x,"y":y,"width":w,"height":h})
        );
        assert_eq!(json["hovered"], button.hovered);
        assert_eq!(json["pressed"], button.pressed);
        assert_eq!(
            window_control_at(&bar, x + w * 0.5, y + h * 0.5),
            Some(button.action)
        );
        assert!(nodes.iter().any(|(_, n)| n.label() == Some(&button.label)
            && n.bounds()
                == Some(accesskit::Rect::new(
                    x as f64,
                    y as f64,
                    (x + w) as f64,
                    (y + h) as f64
                ))));
    }
    let source = include_str!("ctl.rs");
    assert!(
        source
            .contains("geometry[\"tab_bar\"][\"window_controls\"] = window_control_geometry(&bar)")
    );
    assert!(source.contains("geometry[\"linux_header_bar\"]"));
}

#[test]
fn only_desktops_without_server_decorations_get_the_kettle_header() {
    for desktop in [
        "GNOME",
        "ubuntu:GNOME",
        "GNOME-Classic:GNOME",
        "Pantheon",
        "Budgie:GNOME",
    ] {
        assert!(desktop_lacks_server_decorations(Some(desktop)), "{desktop}");
    }
    for desktop in ["KDE", "sway", "Hyprland", "XFCE", ""] {
        assert!(
            !desktop_lacks_server_decorations(Some(desktop)),
            "{desktop}"
        );
    }
    assert!(!desktop_lacks_server_decorations(None));
}

/// The header `App::tab_bar_at_height` builds for a window `width` points
/// wide at `scale`, with GNOME's `layout` of window buttons and `tabs` tabs,
/// from the same geometry functions it calls.
fn gnome_header_bar(width: f32, scale: f32, layout: &str, tabs: usize) -> TabBar {
    let cfg = Config::default();
    let preferences = crate::window_buttons::ButtonLayout::parse(layout).unwrap();
    let height = LINUX_HEADER_HEIGHT * scale;
    let band = (0.0, 0.0, width * scale, height);
    let lane = horizontal_tab_lane(band, preferences.reservations(band.2, height));
    let geometry = horizontal_tab_geometry(lane, scale, (true, true), (tabs, 0), &cfg);
    let mut bar = TabBar::hidden();
    bar.height = height;
    bar.band = band;
    bar.in_titlebar = true;
    bar.linux_header = true;
    bar.lone_title = lone_header_title(true, tabs, (false, false), cfg.tab_bar, cfg.tab_bar_pos);
    bar.new_tab = geometry.plus_rect;
    bar.new_tab_menu = geometry.arrow_rect;
    bar.search = geometry.search_rect;
    bar.window_controls = linux_window_control_layout_with_preferences(
        band,
        false,
        None,
        None,
        &Default::default(),
        &preferences,
    );
    for (idx, x) in geometry.layout.xs.iter().enumerate() {
        let w = if idx + 1 == tabs {
            geometry.layout.last_width
        } else {
            geometry.layout.width
        };
        let rect = (lane.0 + geometry.lead + x.unwrap(), 0.0, w, height);
        let content = kettle_render::horizontal_tab_content_layout(
            rect,
            tabs > 1,
            true,
            0.0,
            "fixture",
            None,
            8.0 * scale,
        );
        bar.segments.push(TabSeg {
            idx,
            rect,
            title_rect: content.title,
            close: content.close,
            hint: None,
            title: "fixture".into(),
            path: None,
            active: idx == 0,
            activity: RenderTabActivity::Normal,
        });
    }
    bar
}

fn centre(r: kettle_render::Rect4) -> (f32, f32) {
    (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5)
}

/// Only Kettle's Linux header with a single tab draws it as the window
/// title. Two tabs, a title being edited, a tab being docked in,
/// `tab-bar = always`, any position but the top, a macOS title bar and a
/// plain strip all keep the tab. `tab_bar_at_height` asks with the
/// window's own state and hands the answer to the painter.
#[test]
fn lone_header_title_only_for_one_tab_in_the_linux_header() {
    use TabBarMode as M;
    assert!(lone_header_title(
        true,
        1,
        (false, false),
        M::Auto,
        TabBarPos::Top
    ));
    for (linux, tabs, busy, mode, pos, why) in [
        (true, 2, (false, false), M::Auto, TabBarPos::Top, "two tabs"),
        (true, 1, (true, false), M::Auto, TabBarPos::Top, "editing"),
        (true, 1, (false, true), M::Auto, TabBarPos::Top, "docking"),
        (true, 1, (false, false), M::Always, TabBarPos::Top, "always"),
        (
            true,
            1,
            (false, false),
            M::Auto,
            TabBarPos::Bottom,
            "bottom",
        ),
        (true, 1, (false, false), M::Auto, TabBarPos::Left, "left"),
        (true, 1, (false, false), M::Auto, TabBarPos::Right, "right"),
        // A macOS title bar and a plain strip have no Linux header.
        (false, 1, (false, false), M::Auto, TabBarPos::Top, "macOS"),
        (false, 1, (false, false), M::Always, TabBarPos::Top, "plain"),
    ] {
        assert!(!lone_header_title(linux, tabs, busy, mode, pos), "{why}");
    }
    let source: String = include_str!("chrome.rs").split_whitespace().collect();
    for wiring in [
        "letlone_title=lone_header_title(ws.linux_titlebar,labels.len(),(ws.editing_title.is_some(),ws.dock_preview.is_some()),self.cfg.tab_bar,self.cfg.tab_bar_pos,);",
        "linux_header:ws.linux_titlebar,lone_title,",
        "hovered_tab_idx:segments_hovered.filter(|_|!lone_title),",
    ] {
        assert!(source.contains(wiring), "missing {wiring}");
    }
}

/// A press on the lone title does what one on empty header space does:
/// a primary press drags the window, a second within 400 ms and 4 points
/// maximizes it, a secondary press opens the window menu and a middle one
/// does nothing. With two tabs a press on a tab is left to the tab strip,
/// as are presses on `+` and `≡`. The title keeps its AccessKit tab.
#[test]
fn lone_header_title_presses_like_empty_header_space() {
    use LinuxHeaderPress::{Control, Title};
    use LinuxTitlebarGesture as G;
    use std::time::{Duration, Instant};
    for scale in [1.0, 2.0] {
        let bar = gnome_header_bar(1000.0, scale, ":minimize,maximize,close", 1);
        assert!(bar.lone_title);
        let title = bar.segments[0].rect;
        assert_eq!(
            bar.segments[0].close.2, 0.0,
            "the title has no close button"
        );
        let (x, y) = centre(title);
        assert!(linux_title_space(&bar, x, y));
        for (button, double, gesture) in [
            (0, false, G::Drag),
            (0, true, G::ToggleMaximize),
            (1, false, G::Ignore),
            (2, false, G::WindowMenu),
        ] {
            for point in [(x, y), (title.0 + 1.0, 1.0), (title.0 + title.2 - 1.0, y)] {
                assert_eq!(
                    linux_header_press(&bar, point, button, double),
                    Some(Title(gesture)),
                    "{scale}x button {button} double={double} at {point:?}"
                );
            }
        }
        let tolerance = 4.0 * scale;
        let first = Instant::now();
        assert!(!linux_title_double_click(None, first, (x, y), tolerance));
        let previous = Some((first, x, y));
        let near = (x + 3.0 * scale, y - 3.0 * scale);
        assert!(linux_title_double_click(
            previous,
            first + Duration::from_millis(200),
            near,
            tolerance
        ));
        assert!(!linux_title_double_click(
            previous,
            first + Duration::from_millis(450),
            near,
            tolerance
        ));
        assert!(!linux_title_double_click(
            previous,
            first + Duration::from_millis(200),
            (x + 5.0 * scale, y),
            tolerance
        ));
        // The strip's own buttons and the window's stay buttons.
        for button in [bar.new_tab, bar.new_tab_menu] {
            let (bx, by) = centre(button);
            assert!(!linux_title_space(&bar, bx, by));
            assert_eq!(linux_header_press(&bar, (bx, by), 0, false), None);
        }
        let close = bar.window_controls[2].rect;
        assert_eq!(
            linux_header_press(&bar, centre(close), 0, false),
            Some(Control(kettle_render::WindowControl::Close))
        );
        // tab-bar = always keeps the tab look, and its presses.
        let mut always = gnome_header_bar(1000.0, scale, ":minimize,maximize,close", 1);
        always.lone_title = false;
        assert_eq!(linux_header_press(&always, (x, y), 0, false), None);
        // Screen readers still find the tab, selected, under the title.
        let nodes = tab_accessibility_projection(&bar, &[7], false, &Default::default());
        let (_, tab) = nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Tab)
            .expect("a tab node");
        assert_eq!(tab.label(), Some("fixture"));
        assert_eq!(tab.is_selected(), Some(true));
        assert!(tab.children().is_empty());
        assert_eq!(
            tab.bounds().unwrap(),
            accesskit::Rect::new(
                title.0 as f64,
                title.1 as f64,
                (title.0 + title.2) as f64,
                (title.1 + title.3) as f64
            )
        );
        // Two tabs: each is a tab again, and empty space is still space.
        let two = gnome_header_bar(1000.0, scale, ":minimize,maximize,close", 2);
        assert!(!two.lone_title);
        for tab in &two.segments {
            let (tx, ty) = centre(tab.rect);
            assert!(!linux_title_space(&two, tx, ty));
            for button in [0, 1, 2] {
                assert_eq!(linux_header_press(&two, (tx, ty), button, false), None);
            }
        }
        let last = two.segments[1].rect;
        // Between the last tab and Find, the first button after the tabs.
        assert!(two.search.0 > last.0 + last.2);
        let space = ((last.0 + last.2 + two.search.0) * 0.5, y);
        assert_eq!(
            linux_header_press(&two, space, 0, false),
            Some(Title(G::Drag))
        );
    }
    // The window and an agent route header presses through these.
    let source: String = include_str!("linux_titlebar.rs")
        .split_whitespace()
        .collect();
    assert!(source.contains(
        "letgesture=matchlinux_header_press(bar,(x,y),button,double){None=>returnfalse,"
    ));
    let ctl: String = include_str!("ctl.rs").split_whitespace().collect();
    assert!(ctl.contains("ifws.linux_titlebar&&linux_title_space(&bar,px,py){returntrue;}"));
}

/// `+` leads the GNOME header, after any window buttons GNOME's layout puts
/// on the left, and `≡` ends the lane before those on the right, at 1x and
/// 2x. The pointer, AccessKit and ctl all find `+` in the rect the painter
/// is given, and the tabs start past it.
#[test]
fn gnome_header_plus_leads_and_agrees_everywhere() {
    let tr = kettle_i18n::Translator::default();
    for scale in [1.0, 2.0] {
        for layout in [
            ":minimize,maximize,close",
            "close,minimize:maximize",
            "close:",
        ] {
            for tabs in [1, 2, 3] {
                let bar = gnome_header_bar(1000.0, scale, layout, tabs);
                let case = format!("{scale}x {layout:?} {tabs} tabs");
                let plus = bar.new_tab;
                let menu = bar.new_tab_menu;
                assert_eq!((plus.2, plus.3), (34.0 * scale, 34.0 * scale), "{case}");
                let left = bar
                    .window_controls
                    .iter()
                    .filter(|b| b.rect.0 < bar.band.2 * 0.5)
                    .map(|b| b.rect.0 + b.rect.2)
                    .fold(0.0_f32, f32::max);
                let right = bar
                    .window_controls
                    .iter()
                    .filter(|b| b.rect.0 >= bar.band.2 * 0.5)
                    .map(|b| b.rect.0)
                    .fold(bar.band.2, f32::min);
                assert!(
                    plus.0 > left && plus.0 <= left + 18.0 * scale,
                    "{case}: {plus:?}"
                );
                assert!(menu.0 > plus.0 + plus.2, "{case}: ≡ {menu:?} not after +");
                assert!(menu.0 + menu.2 < right, "{case}: ≡ {menu:?} under a button");
                for tab in &bar.segments {
                    assert!(tab.rect.0 >= plus.0 + plus.2, "{case}: a tab before +");
                    assert!(tab.rect.0 + tab.rect.2 <= menu.0, "{case}: a tab past ≡");
                }
                let (px, py) = centre(plus);
                assert_eq!(
                    new_tab_hover_target(&bar, px, py),
                    Some(NewTabHoverTarget::Plus),
                    "{case}"
                );
                assert!(tab_bar_pointer_region_contains(&bar, px, py));
                assert_eq!(linux_header_press(&bar, (px, py), 0, false), None);
                let nodes = tab_accessibility_projection(&bar, &[1, 2, 3], false, &tr);
                let node = nodes
                    .iter()
                    .find(|(_, n)| n.label() == Some(tr.text(kettle_i18n::Text::PaletteNewTab)))
                    .expect("the new-tab button");
                assert_eq!(
                    node.1.bounds().unwrap(),
                    accesskit::Rect::new(
                        plus.0 as f64,
                        plus.1 as f64,
                        (plus.0 + plus.2) as f64,
                        (plus.1 + plus.3) as f64
                    ),
                    "{case}"
                );
                let json = super::ctl::tab_bar_geometry_json(&bar, Vec::new());
                assert_eq!(
                    json["new_tab"],
                    super::ctl_snapshot::rect_json(plus),
                    "{case}"
                );
                assert_eq!(json["lone_title"], tabs == 1, "{case}");
            }
        }
    }
}

/// GNOME's window buttons sit as libadwaita lays them out: 34-point
/// targets (a 24-point disc and its padding, larger than the 28-point
/// targets they replace) 3 points apart, so their discs are 37 points from
/// centre to centre, the outermost 7 points from the header's end, centred
/// on its height, on either side, at 1x, 1.5x and 2x. The tab lane starts
/// and ends where they do.
#[test]
fn gnome_window_buttons_follow_adwaita_metrics() {
    for scale in [1.0_f32, 1.5, 2.0] {
        for layout in [
            ":minimize,maximize,close",
            "close,minimize:maximize",
            "close:",
        ] {
            let preferences = crate::window_buttons::ButtonLayout::parse(layout).unwrap();
            let band = (0.0, 0.0, 1000.0 * scale, LINUX_HEADER_HEIGHT * scale);
            let buttons = linux_window_control_layout_with_preferences(
                band,
                false,
                None,
                None,
                &Default::default(),
                &preferences,
            );
            let (left, right) = preferences.reservations(band.2, band.3);
            let case = format!("{scale}x {layout:?}");
            let (lefts, rights): (Vec<_>, Vec<_>) =
                buttons.iter().partition(|b| b.rect.0 < band.2 * 0.5);
            for side in [&lefts, &rights] {
                for button in side.iter() {
                    let (_, y, w, h) = button.rect;
                    assert_eq!((w, h), (34.0 * scale, 34.0 * scale), "{case}");
                    assert!(w >= 28.0 * scale, "{case}: a smaller target");
                    assert!((y + h * 0.5 - band.3 * 0.5).abs() < 1e-3, "{case}");
                }
                for pair in side.windows(2) {
                    let gap = pair[1].rect.0 - (pair[0].rect.0 + pair[0].rect.2);
                    assert!((gap - 3.0 * scale).abs() < 1e-3, "{case}: gap {gap}");
                    let pitch = centre(pair[1].rect).0 - centre(pair[0].rect).0;
                    assert!((pitch - 37.0 * scale).abs() < 1e-3, "{case}: pitch {pitch}");
                }
            }
            if let Some(first) = lefts.first() {
                assert!((first.rect.0 - 7.0 * scale).abs() < 1e-3, "{case}");
                let end = lefts.last().unwrap().rect;
                assert!((end.0 + end.2 - left).abs() < 1e-3, "{case}");
            } else {
                assert_eq!(left, 0.0, "{case}");
            }
            if let Some(last) = rights.last() {
                let r = last.rect;
                assert!((band.2 - (r.0 + r.2) - 7.0 * scale).abs() < 1e-3, "{case}");
                // The outermost disc's centre, 24 points in, as in GNOME.
                assert!((band.2 - centre(r).0 - 24.0 * scale).abs() < 1e-3, "{case}");
                assert!((rights[0].rect.0 - (band.2 - right)).abs() < 1e-3, "{case}");
            } else {
                assert_eq!(right, 0.0, "{case}");
            }
        }
    }
}

/// The GNOME header's Find button is where the painter draws it for every
/// consumer: the pointer's hover and press, the title space (a press there
/// is not a window drag), AccessKit (a button named "Find", pressed while
/// Find is open, with a click action) and ctl's ui_geometry, at 1x and 2x,
/// with one tab or several, and with buttons on either side. The tabs and
/// the lone title end before it.
#[test]
fn gnome_header_find_agrees_everywhere() {
    for scale in [1.0_f32, 2.0] {
        for layout in [":minimize,maximize,close", "close,minimize:maximize"] {
            for tabs in [1, 3] {
                let mut bar = gnome_header_bar(1000.0, scale, layout, tabs);
                let case = format!("{scale}x {layout:?} {tabs} tabs");
                let find = bar.search;
                assert_eq!((find.2, find.3), (34.0 * scale, 34.0 * scale), "{case}");
                assert!(
                    (find.0 + find.2 + 6.0 * scale - bar.new_tab_menu.0).abs() < 1e-3,
                    "{case}: Find sits just before ≡"
                );
                for tab in &bar.segments {
                    assert!(
                        tab.rect.0 + tab.rect.2 <= find.0 - 6.0 * scale + 1e-3,
                        "{case}"
                    );
                }
                let (fx, fy) = centre(find);
                assert!(search_button_at(&bar, fx, fy));
                assert_eq!(
                    new_tab_hover_target(&bar, fx, fy),
                    Some(NewTabHoverTarget::Search),
                    "{case}"
                );
                assert!(titlebar_button_at(&bar, fx, fy));
                assert!(!linux_title_space(&bar, fx, fy), "{case}: a drag over Find");
                assert_eq!(linux_header_press(&bar, (fx, fy), 0, false), None);
                assert!(tab_bar_pointer_region_contains(&bar, fx, fy));
                for open in [false, true] {
                    bar.search_open = open;
                    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
                        let tr = kettle_i18n::Translator::new(language);
                        let nodes = tab_accessibility_projection(&bar, &[1, 2, 3], false, &tr);
                        let (id, node) = nodes
                            .iter()
                            .find(|(_, n)| {
                                n.label() == Some(tr.text(kettle_i18n::Text::A11yHeaderFind))
                            })
                            .unwrap_or_else(|| panic!("{case}: no Find node"));
                        assert_eq!(*id, super::accessibility::ACCESSIBILITY_HEADER_FIND_ID);
                        assert_eq!(node.role(), Role::Button);
                        assert!(node.supports_action(AccessibilityAction::Click));
                        assert_eq!(
                            node.toggled(),
                            Some(accesskit::Toggled::from(open)),
                            "{case}"
                        );
                        assert_eq!(
                            node.bounds().unwrap(),
                            accesskit::Rect::new(
                                find.0 as f64,
                                find.1 as f64,
                                (find.0 + find.2) as f64,
                                (find.1 + find.3) as f64
                            ),
                            "{case}"
                        );
                        let list = nodes
                            .iter()
                            .find(|(_, n)| n.role() == Role::TabList)
                            .unwrap();
                        assert!(list.1.children().contains(id), "{case}");
                    }
                    let json = super::ctl::tab_bar_geometry_json(&bar, Vec::new());
                    assert_eq!(json["search_button"], super::ctl_snapshot::rect_json(find));
                    assert_eq!(json["search_open"], open);
                }
                // A header with no room for Find has no node and no target.
                bar.search = (0.0, 0.0, 0.0, 0.0);
                assert!(!search_button_at(&bar, fx, fy));
                let nodes =
                    tab_accessibility_projection(&bar, &[1, 2, 3], false, &Default::default());
                assert!(
                    !nodes
                        .iter()
                        .any(|(id, _)| *id == super::accessibility::ACCESSIBILITY_HEADER_FIND_ID)
                );
            }
        }
    }
    // The window, an agent and a screen reader all toggle Find through the
    // same call, and the press shows only until its release.
    let source = super::production_source();
    let native = source
        .split_once("state: ElementState::Pressed,")
        .unwrap()
        .1
        .split_once("state: ElementState::Released,")
        .unwrap();
    for body in [native.0, fn_body(&source, "ctl_mouse_press")] {
        let flat: String = body.split_whitespace().collect();
        let (_, press) = flat
            .split_once("elseifbcode==0&&search_button_at(&bar,px,py){")
            .expect("a press on Find");
        let press = press.split("}elseif").next().unwrap();
        assert!(
            press.ends_with("ws.pressed_search=true;self.toggle_header_find(ws);"),
            "{press}"
        );
    }
    let release: String = native.1.split_whitespace().collect();
    assert!(release.contains("ws.pressed_search=false;"));
    let ctl_release: String = fn_body(&source, "ctl_mouse_release")
        .split_whitespace()
        .collect();
    assert!(ctl_release.contains("ws.pressed_search=false;"));
    let a11y: String = source.split_whitespace().collect();
    assert!(a11y.contains(
        "}elseifrequest.target_node==ACCESSIBILITY_HEADER_FIND_ID{self.toggle_header_find(ws);"
    ));
    let toggle: String = fn_body(&source, "toggle_header_find")
        .split_whitespace()
        .collect();
    assert_eq!(
        toggle,
        "&mutself,ws:&mutWindowState){ifws.search.open{self.close_search(ws);}else{self.open_search(ws);}"
    );
}

/// Find's tooltip names Find with whatever key opens it: the default, a
/// rebind's new key, or no key once the action is unbound; in both
/// languages.
#[test]
fn gnome_header_find_tooltip_follows_the_start_search_binding() {
    let defaults = kettle_config::keybinds::defaults();
    let default_key = chrome_button_shortcut(&defaults, NewTabHoverTarget::Search);
    assert_eq!(
        default_key,
        kettle_config::keybinds::ui_hint_label(&defaults, &Action::StartSearch)
    );
    assert!(default_key.is_some(), "Find has a default shortcut");
    let parsed =
        kettle_config::Config::parse_text("keybind = ctrl+shift+f4=start_search\n").keybinds;
    let (trigger, action) = parsed
        .iter()
        .find(|(trigger, action)| **action == Action::StartSearch && trigger.label().contains("F4"))
        .unwrap();
    let mut custom = defaults.clone();
    custom.retain(|_, bound| *bound != Action::StartSearch);
    custom.insert(*trigger, action.clone());
    let key = chrome_button_shortcut(&custom, NewTabHoverTarget::Search).unwrap();
    assert!(key.contains("F4"), "{key}");
    for (language, label) in [
        (kettle_i18n::Language::En, "Find"),
        (kettle_i18n::Language::Es, "Buscar"),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Search), &tr, Some(&key)),
            Some(format!("{label}  {key}"))
        );
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Search), &tr, None),
            Some(label.into())
        );
    }
    custom.retain(|_, bound| *bound != Action::StartSearch);
    assert_eq!(
        chrome_button_shortcut(&custom, NewTabHoverTarget::Search),
        None
    );
    // The window asks with the live bindings, for a hovered, unpressed Find.
    let source: String = super::production_source().split_whitespace().collect();
    assert!(source.contains("}elseifws.hovered_search{Some(NewTabHoverTarget::Search)"));
    assert!(source.contains("||ws.pressed_search||ws.pressed_window_control.is_some()"));
}

/// The lone title's lane keeps clear of `+` and of Find: at least the 6
/// points between the header's buttons and the tabs, plus the title's own
/// 8-pixel inset, at every width and scale the header lays out, with
/// window buttons on either side.
#[test]
fn the_lone_title_keeps_clear_of_plus_and_find() {
    for scale in [1.0_f32, 1.5, 2.0] {
        for width in [360.0, 480.0, 640.0, 1000.0, 1600.0] {
            for layout in [
                ":minimize,maximize,close",
                "close,minimize:maximize",
                ":close",
            ] {
                let bar = gnome_header_bar(width, scale, layout, 1);
                let case = format!("{scale}x {width} {layout:?}");
                let lane = bar.segments[0].title_rect;
                // As `tab_bar_at_height` centres it on the window, too.
                let centred = centered_title_lane(lane, (bar.band.0 + bar.band.2) * 0.5);
                let clear = 6.0 * scale + 8.0 - 1e-3;
                let after = if bar.search.2 > 0.0 {
                    bar.search
                } else {
                    bar.new_tab_menu
                };
                for title in [lane, centred] {
                    assert!(
                        title.0 - (bar.new_tab.0 + bar.new_tab.2) >= clear,
                        "{case}: {title:?}"
                    );
                    if after.2 > 0.0 {
                        assert!(after.0 - (title.0 + title.2) >= clear, "{case}: {title:?}");
                    }
                }
            }
        }
    }
}

/// The body of `fn name(` in `source`, to its first closing brace at method
/// depth.
fn fn_body<'a>(source: &'a str, name: &str) -> &'a str {
    source
        .split(&format!("fn {name}("))
        .nth(1)
        .unwrap()
        .split("\n    }")
        .next()
        .unwrap()
}
