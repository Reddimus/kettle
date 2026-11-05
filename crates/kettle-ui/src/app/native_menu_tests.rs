fn menu_paste_enabled(ws: &WindowState, others: &[WindowState]) -> bool {
    crate::macos_menu::Context {
        window: true,
        pane: ws.mux.active_focus().is_some(),
        clipboard: true,
        writable: super::native_menu::paste_is_writable(ws, others.iter()),
        ..Default::default()
    }
    .enables(&Action::Paste)
}

#[test]
fn menu_paste_availability_uses_local_broadcast_recipients() {
    let (mut mux, focused, writable) = title_test_split_mux();
    mux.panes.get_mut(&focused).unwrap().read_only = true;
    let mut ws = WindowState::new(1, mux);
    assert!(!menu_paste_enabled(&ws, &[]));
    for scope in [BroadcastScope::Tab, BroadcastScope::All] {
        ws.mux.broadcast = scope;
        assert!(menu_paste_enabled(&ws, &[]), "{:?}", ws.mux.broadcast);
        let before = ws.mux.panes[&focused].input_generation();
        let delivery = ws.mux.broadcast_paste_delivery(
            "paste",
            Some(writable),
            &super::paste::text_paste_modes(ws.mux.panes.iter().map(|(&id, pane)| (id, pane))),
        );
        assert!(delivery.receipt_accepted);
        assert_eq!(ws.mux.panes[&focused].input_generation(), before);
    }
    ws.mux.panes.get_mut(&writable).unwrap().read_only = true;
    assert!(!menu_paste_enabled(&ws, &[]));
}

#[test]
fn menu_paste_availability_uses_cross_window_group_recipients() {
    let (mut mux, focused, excluded) = title_test_split_mux();
    mux.panes.get_mut(&focused).unwrap().read_only = true;
    mux.panes.get_mut(&focused).unwrap().group_name = Some("fleet".into());
    let mut ws = WindowState::new(1, mux);
    ws.mux.broadcast = BroadcastScope::Group("fleet".into());
    let (mut mux, ids) = title_test_tab_mux(1);
    let recipient = ids[0];
    mux.panes.get_mut(&recipient).unwrap().group_name = Some("fleet".into());
    let mut others = [WindowState::new(2, mux)];

    assert!(!menu_paste_enabled(&ws, &[]));
    assert!(menu_paste_enabled(&ws, &others));
    assert!(!ws.mux.broadcast_target_ids().contains(&excluded));
    let delivery = others[0].mux.broadcast_paste_foreign_delivery(
        &ws.mux.broadcast,
        "paste",
        Some(recipient),
        &std::collections::HashMap::from([(recipient, false)]),
    );
    assert!(delivery.receipt_accepted);

    others[0].mux.panes.get_mut(&recipient).unwrap().read_only = true;
    assert!(!menu_paste_enabled(&ws, &others));
    others[0].mux.panes.get_mut(&recipient).unwrap().read_only = false;
    others[0].mux.panes.get_mut(&recipient).unwrap().group_name = Some("other".into());
    assert!(!menu_paste_enabled(&ws, &others));
    others[0].mux.panes.get_mut(&recipient).unwrap().group_name = Some("fleet".into());
    for scope in [
        BroadcastScope::Off,
        BroadcastScope::Tab,
        BroadcastScope::All,
    ] {
        ws.mux.broadcast = scope;
        ws.mux.panes.get_mut(&excluded).unwrap().read_only = true;
        assert!(!menu_paste_enabled(&ws, &others));
    }
}
