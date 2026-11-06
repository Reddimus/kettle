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
                    assert_eq!((w, h), (28.0 * scale, 28.0 * scale));
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
            let controls_start = width - 3.0 * height;
            let (arrow, plus, reserve) =
                linux_header_tab_controls((0.0, 0.0, controls_start, height), scale, true);
            let strip = controls_start - reserve;
            let layout =
                titlebar_tab_layout(count, 0, (strip, reserve, 0.0), height, 80.0 * scale, true);
            assert_eq!(plus.0 + plus.2, controls_start - 8.0 * scale);
            assert_eq!(arrow.0 + arrow.2 + 4.0 * scale, plus.0);
            assert_eq!(plus.2, 28.0 * scale);
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
                let rect = (*x, 0.0, w, height);
                let mut content = kettle_render::horizontal_tab_content_layout(
                    rect,
                    count > 1,
                    true,
                    false,
                    "shell",
                    None,
                    8.0 * scale,
                );
                if count == 1 {
                    content.title = centered_title_lane(content.title, width * 0.5);
                    assert!((content.title.0 + content.title.2 * 0.5 - width * 0.5).abs() < 0.01);
                    assert!(content.title.0 + content.title.2 <= strip);
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
            if count == 1 {
                assert!(empty_titlebar_space(&bar, 10.0 * scale, height * 0.5));
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
    assert_eq!((left, right), (92.0, 46.0));
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
                    let (menu, plus, reserve) = linux_header_tab_controls(lane, scale, dropdown);
                    assert!(reserve >= 0.0 && reserve <= lane.2);
                    for target in [menu, plus] {
                        assert!(target.0 >= lane.0 - 0.01);
                        assert!(target.0 + target.2 <= lane.0 + lane.2 + 0.01);
                        assert!(target.2 >= 0.0 && target.2 <= 28.0 * scale + 0.01);
                    }
                    if menu.2 > 0.0 && plus.2 > 0.0 {
                        assert!(menu.0 + menu.2 <= plus.0);
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
