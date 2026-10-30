#[test]
fn context_menu_snapshot_reuse_rejects_live_pointer_gestures() {
    let mut ws = WindowState::new(1, Mux::new());
    assert!(context_menu_snapshot_reuse_safe(&ws));

    ws.selecting = true;
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.selecting = false;

    ws.scrollbar_drag_offset = Some(0.0);
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.scrollbar_drag_offset = None;

    ws.dragging_split = Some(SplitDrag {
        path: Vec::new(),
        dir: Dir::Horizontal,
    });
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.dragging_split = None;

    ws.held_buttons.pressed(0, Some(1), (0, 0));
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.held_buttons.take(0);

    ws.tab_drag_active = true;
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.tab_drag_active = false;

    ws.tab_drag_press = Some((0.0, 0.0));
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.tab_drag_press = None;

    ws.drag_press = Some((0.0, 0.0));
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.drag_press = None;

    // An ARMED pane drag counts, not just a live one: the press has already
    // claimed the pointer, and the very next CursorMoved can promote it and
    // rewrite the layout under a reused snapshot.
    ws.pane_drag = Some(PaneDrag {
        pane: 1,
        origin: (0.0, 0.0),
        live: false,
        rename_on_click: false,
        target: None,
    });
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.pane_drag = None;

    ws.detach_drag = crate::detach::DragState::on_mouse_down_on_tab(0);
    assert!(!context_menu_snapshot_reuse_safe(&ws));
}

/// Drift guard: a mouse press / wheel is swallowed
/// whenever a non-context-menu modal is open, so it can't fall through to
/// the tab bar / pane focus / mouse-tracking behind the dialog. A lone
/// context menu is the one exception — it owns its click/scroll paths and a
/// right-click relocates it — and with nothing open the pointer passes
/// through as normal.
#[test]
fn modal_swallows_pointer_except_lone_context_menu() {
    // A non-context-menu modal (e.g. search/palette/settings) is open.
    assert!(modal_swallows_pointer(true, false));
    // A lone context menu does NOT swallow — its own paths handle it.
    assert!(!modal_swallows_pointer(true, true));
    // Nothing open: the pointer falls through to tabs/panes normally.
    assert!(!modal_swallows_pointer(false, false));
    // Defensive: even if both were somehow set, the context-menu
    // exclusion wins (its dedicated handling ran first).
    assert!(!modal_swallows_pointer(true, true));
}

/// `vim-menu-nav` drift guards. (1) The vim layer must run BEFORE the
/// mnemonic/typeahead catch-all in `context_menu_key`; moved after it,
/// bare `j`/`k` would be eaten as typeahead input and the nav would
/// silently die. (2) The catch-all's mnemonic lookup must pass the
/// reservation set so a row can never claim a nav letter while the
/// setting is on. Both are ordering/wiring contracts a behavioral test
/// can't see without a live window; pin them at the source.
#[test]
fn vim_menu_nav_intercepts_before_mnemonic_catchall() {
    let src = production_source();
    let body = src
        .split("fn context_menu_key(")
        .nth(1)
        .and_then(|s| s.split("\n    }\n").next())
        .expect("context_menu_key present");
    let vim = body
        .find("self.context_menu_vim_key(ws, key, event_loop)")
        .expect("context_menu_key must consult the vim layer");
    let catchall = body
        .find("assign_mnemonics(&menu.items, reserved)")
        .expect("the mnemonic lookup must pass the reservation set");
    assert!(
        vim < catchall,
        "vim nav must intercept BEFORE the mnemonic/typeahead catch-all"
    );
}

/// The `▾` menu is shells → separator → Windows
/// Terminal's bottom section (Settings / Command palette / About).
#[test]
fn new_tab_menu_items_has_wt_bottom_rows() {
    let shells = vec![
        ("PowerShell".to_string(), vec!["pwsh.exe".to_string()]),
        ("Git Bash".to_string(), vec!["bash.exe".to_string()]),
    ];
    let items = App::new_tab_menu_items(
        &shells,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(items.len(), 6, "2 shells + separator + 3 bottom rows");
    assert!(matches!(
        &items[0],
        ContextMenuItem::NewTabShell { label, .. } if label == "PowerShell"
    ));
    assert!(matches!(items[2], ContextMenuItem::Separator));
    assert!(matches!(
        items[3],
        ContextMenuItem::Item {
            label: "Settings…",
            action: Action::OpenSettings,
            enabled: true
        }
    ));
    assert!(matches!(
        items[4],
        ContextMenuItem::Item {
            label: "Command palette",
            action: Action::CommandPalette,
            enabled: true
        }
    ));
    assert!(matches!(
        items[5],
        ContextMenuItem::Item {
            label: "About kettle",
            action: Action::About,
            enabled: true
        }
    ));
    // Every dispatchable row gets a distinct mnemonic; the two-pass
    // assignment must keep working with the new bottom rows.
    let mn = assign_mnemonics(&items, &[]);
    let mut letters: Vec<char> = mn.iter().flatten().map(|(_, c)| *c).collect();
    assert_eq!(letters.len(), 5, "all 5 dispatchable rows get mnemonics");
    letters.sort_unstable();
    letters.dedup();
    assert_eq!(letters.len(), 5, "mnemonics are distinct");
}

/// An `Info` row (About panel) is inert — not
/// dispatchable, no click mapping, survives filter_disabled.
#[test]
fn info_rows_are_inert_but_visible() {
    let info = ContextMenuItem::Info {
        label: "kettle 9.9.9".to_string(),
    };
    assert!(!super::item_is_dispatchable(&info));
    assert!(super::item_to_click(&info, 0).is_none());
    let kept = filter_disabled(vec![
        ContextMenuItem::Info {
            label: "x".to_string(),
        },
        ContextMenuItem::Separator,
        item("Copy version info", true),
    ]);
    assert_eq!(kept.len(), 3, "Info + separator + item all survive");
}

/// Drift guard. Disabled `Item`s are removed entirely
/// (Terminator-style "only show what you can click") and
/// orphaned/leading/trailing separators collapse so the menu
/// never has visual gaps that lead nowhere.
#[test]
fn disabled_items_are_hidden_and_separators_collapse() {
    // Mixed: Copy disabled, Paste enabled, separator, Split disabled,
    // separator, NewTab enabled, separator (trailing).
    let menu = vec![
        item("Copy", false),
        item("Paste", true),
        ContextMenuItem::Separator,
        item("Split Right", false),
        ContextMenuItem::Separator,
        item("New Tab", true),
        ContextMenuItem::Separator,
    ];
    let got = filter_disabled(menu);
    // Expected: Paste, separator, New Tab.
    assert_eq!(got.len(), 3);
    assert!(matches!(&got[0], ContextMenuItem::Item { label, .. } if *label == "Paste"));
    assert!(matches!(&got[1], ContextMenuItem::Separator));
    assert!(matches!(&got[2], ContextMenuItem::Item { label, .. } if *label == "New Tab"));
}

/// Runs of separators collapse to a single one; a
/// leading separator (everything-disabled above it) gets dropped.
#[test]
fn consecutive_separators_collapse_and_leading_is_dropped() {
    let menu = vec![
        ContextMenuItem::Separator,
        ContextMenuItem::Separator,
        item("OK", true),
        ContextMenuItem::Separator,
        ContextMenuItem::Separator,
        ContextMenuItem::Separator,
        item("Cancel", true),
    ];
    let got = filter_disabled(menu);
    assert_eq!(got.len(), 3);
    assert!(matches!(&got[0], ContextMenuItem::Item { label, .. } if *label == "OK"));
    assert!(matches!(&got[1], ContextMenuItem::Separator));
    assert!(matches!(&got[2], ContextMenuItem::Item { label, .. } if *label == "Cancel"));
}

/// Filter is identity (modulo trailing separator) when
/// nothing is disabled.
#[test]
fn filter_disabled_is_near_identity_when_all_enabled() {
    let menu = vec![item("A", true), ContextMenuItem::Separator, item("B", true)];
    let got = filter_disabled(menu);
    assert_eq!(got.len(), 3);
}

/// Empty menu stays empty (defensive).
#[test]
fn filter_disabled_handles_empty() {
    let got = filter_disabled(vec![]);
    assert!(got.is_empty());
}

/// All-disabled menu collapses to empty (no rows, no
/// orphan separators).
#[test]
fn filter_disabled_collapses_all_disabled_to_empty() {
    let menu = vec![
        item("X", false),
        ContextMenuItem::Separator,
        item("Y", false),
    ];
    let got = filter_disabled(menu);
    assert!(got.is_empty());
}

/// Drift guard. The Preferences ▸ submenu builder
/// must surface every runtime-mutable toggle the spec promises:
///   - 3 scrollbar radio rows
///   - 3 boolean toggles (cursor blink, copy on select, mouse-hide)
///   - 3 close-confirmation radio rows
///   - 4 bell radio rows
///   - 2 font-size +/− rows
///   - 1 Advanced… (EditConfig) escape hatch
///
/// Total: 16 actionable rows + 5 separators = 21 items. The test pins the
/// 16 actions and their `from_name` spellings; it does not count rows.
#[test]
fn preferences_submenu_contains_all_user_facing_toggles() {
    // append_preferences_submenu_items needs an App, so pin the submenu's
    // Action variants instead to keep the keybinds-side palette wiring in
    // sync.
    let expected_actions: &[Action] = &[
        Action::SetScrollbarAlways,
        Action::SetScrollbarAuto,
        Action::SetScrollbarNever,
        Action::ToggleCursorBlink,
        Action::ToggleCopyOnSelect,
        Action::ToggleMouseHide,
        Action::SetAskBeforeClosingAlways,
        Action::SetAskBeforeClosingMultiple,
        Action::SetAskBeforeClosingNever,
        Action::SetBellOff,
        Action::SetBellVisual,
        Action::SetBellAttention,
        Action::SetBellBoth,
        Action::IncreaseFontSize,
        Action::DecreaseFontSize,
        Action::EditConfig,
    ];
    // Each action parses from its name (the `Action::from_name`
    // surface) so the keybind grammar accepts them. Catches the
    // case where someone adds a variant but forgets the
    // from_name arm.
    for a in expected_actions {
        // We need a known name string for each. Use the canonical
        // forms documented in palette.rs / keybinds.rs.
        let name = match a {
            Action::SetScrollbarAlways => "set_scrollbar_always",
            Action::SetScrollbarAuto => "set_scrollbar_auto",
            Action::SetScrollbarNever => "set_scrollbar_never",
            Action::ToggleCursorBlink => "toggle_cursor_blink",
            Action::ToggleCopyOnSelect => "toggle_copy_on_select",
            Action::ToggleMouseHide => "toggle_mouse_hide",
            Action::SetAskBeforeClosingAlways => "set_ask_before_closing_always",
            Action::SetAskBeforeClosingMultiple => "set_ask_before_closing_multiple",
            Action::SetAskBeforeClosingNever => "set_ask_before_closing_never",
            Action::SetBellOff => "set_bell_off",
            Action::SetBellVisual => "set_bell_visual",
            Action::SetBellAttention => "set_bell_attention",
            Action::SetBellBoth => "set_bell_both",
            Action::IncreaseFontSize => "increase_font_size",
            Action::DecreaseFontSize => "decrease_font_size",
            Action::EditConfig => "edit_config",
            _ => unreachable!(),
        };
        let parsed = Action::from_name(name)
            .unwrap_or_else(|| panic!("Action::from_name({name:?}) returned None"));
        assert_eq!(
            std::mem::discriminant(&parsed),
            std::mem::discriminant(a),
            "Action::from_name({name:?}) returned the wrong variant"
        );
    }
}

/// Drift guard. `assign_mnemonics` gives each row its first A-Z char; on a
/// collision the later row falls through to its next free A-Z char.
/// Pinning the contract: Copy=C, Close Pane=l (C taken),
/// Cancel=a (C taken).
#[test]
fn mnemonics_assign_unique_chars_with_fallback() {
    let menu = vec![
        item("Copy", true),       // 'C'
        item("Close Pane", true), // 'C' taken → 'l'
        item("Cancel", true),     // 'C' + 'l' taken → 'a'
        ContextMenuItem::Separator,
        item("Theme", true), // 'T'
        item("Tab", true),   // 'T' taken → 'a' taken → 'b'
        item("12345", true), // no A-Z → None
    ];
    let mn = assign_mnemonics(&menu, &[]);
    assert_eq!(mn[0], Some((0, 'c')));
    // "Close Pane": C taken, so next alphabetic is 'l' at byte 1.
    assert_eq!(mn[1], Some((1, 'l')));
    // "Cancel": C + l taken, so next is 'a' at byte 1.
    assert_eq!(mn[2], Some((1, 'a')));
    // Separator: no label, no mnemonic.
    assert_eq!(mn[3], None);
    // "Theme": T at byte 0.
    assert_eq!(mn[4], Some((0, 't')));
    // "Tab": T taken, a taken, so 'b' at byte 2.
    assert_eq!(mn[5], Some((2, 'b')));
    // "12345": no A-Z, None.
    assert_eq!(mn[6], None);
}

/// The URL-aware leading rows claim their mnemonics AFTER the stable core
/// rows. Without the two-round pass, "Open Link" / "Copy Link Address"
/// leading the menu would steal 'o'/'c', silently remapping muscle-memory
/// mnemonics whenever the right-click lands on a link ('p' would fire Copy
/// instead of Paste).
#[test]
fn mnemonics_url_rows_claim_letters_last() {
    let url = |label: &'static str, how: super::UrlHow| ContextMenuItem::UrlItem {
        label,
        url: "https://example.com".into(),
        how,
        pane: None,
    };
    let menu = vec![
        url("Open Link", super::UrlHow::Open),
        url("Copy Link Address", super::UrlHow::Copy),
        ContextMenuItem::Separator,
        item("Copy", true),
        item("Paste", true),
    ];
    let mn = assign_mnemonics(&menu, &[]);
    // Core rows keep their stable letters…
    assert_eq!(mn[3], Some((0, 'c'))); // Copy = c (not stolen)
    assert_eq!(mn[4], Some((0, 'p'))); // Paste = p (not stolen)
    // …and the URL rows pick from what's left.
    assert_eq!(mn[0], Some((0, 'o'))); // Open Link = o (free)
    // "Copy Link Address": c, o, p taken → 'y' (byte 3).
    assert_eq!(mn[1], Some((3, 'y')));
    assert_eq!(mn[2], None); // separator
}

/// `vim-menu-nav` drift guard: while the setting is on, the vim navigation
/// letters must never be assigned as mnemonics. The nav layer intercepts
/// them first, so a row keyed on one would silently lose its hotkey. Rows
/// fall through to their next free letter instead.
#[test]
fn vim_nav_letters_are_excluded_from_mnemonics() {
    let menu = vec![
        item("Jump", true),      // 'j' reserved → 'u'
        item("Kill Pane", true), // 'k' reserved → 'i'
        item("Hold", true),      // 'h' reserved → 'o'
        item("List", true),      // 'l' reserved → 'i' taken → 's'
        item("Go", true),        // 'g' reserved → 'o' taken → None
    ];
    let mn = assign_mnemonics(&menu, super::VIM_NAV_RESERVED);
    for (i, slot) in mn.iter().enumerate() {
        if let Some((_, c)) = slot {
            assert!(
                !super::VIM_NAV_RESERVED.contains(c),
                "row {i} was assigned reserved nav letter {c:?}"
            );
        }
    }
    assert_eq!(mn[0], Some((1, 'u'))); // Jump → 'u'
    assert_eq!(mn[1], Some((1, 'i'))); // Kill Pane → 'i'
    assert_eq!(mn[2], Some((1, 'o'))); // Hold → 'o'
    assert_eq!(mn[3], Some((2, 's'))); // List → 's'
    // "Go": both letters reserved/taken → no mnemonic at all.
    assert_eq!(mn[4], None);
    // And with the reservation off, first letters win as before.
    let plain = assign_mnemonics(&menu, &[]);
    assert_eq!(plain[0], Some((0, 'j')));
    assert_eq!(plain[1], Some((0, 'k')));
}

/// `vim-menu-nav`: `Ctrl+d`/`Ctrl+u` clamp at the list ends (no wrap, per
/// vim half-page semantics) and snap off separators in the direction of
/// travel.
#[test]
fn half_page_menu_target_clamps_and_snaps() {
    let menu = vec![
        item("A", true),            // 0
        item("B", true),            // 1
        ContextMenuItem::Separator, // 2
        item("C", true),            // 3
        item("D", true),            // 4
    ];
    // Down by 2 from row 0 lands on the separator → snaps DOWN to 3.
    assert_eq!(super::half_page_menu_target(&menu, 0, 2, 1), 3);
    // Down by 10 from row 0 clamps to the last row.
    assert_eq!(super::half_page_menu_target(&menu, 0, 10, 1), 4);
    // Up by 2 from row 4 lands on the separator → snaps UP to 1.
    assert_eq!(super::half_page_menu_target(&menu, 4, 2, -1), 1);
    // Up by 10 clamps to the first row.
    assert_eq!(super::half_page_menu_target(&menu, 4, 10, -1), 0);
    // Empty list: target stays put.
    assert_eq!(super::half_page_menu_target(&[], 0, 3, 1), 0);
}

/// Drift guard. Typeahead prefix-match is case-
/// insensitive and stops at the first dispatchable hit.
#[test]
fn typeahead_th_highlights_theme_first() {
    let menu = vec![
        item("Copy", true),
        item("Theme", true),
        item("Toggle Broadcast", true),
        item("Profile", true),
    ];
    // Single-char "t" hits Theme (first label starting with t).
    assert_eq!(typeahead_match(&menu, "t"), Some(1));
    // "th" still Theme.
    assert_eq!(typeahead_match(&menu, "th"), Some(1));
    // "to" → Toggle Broadcast.
    assert_eq!(typeahead_match(&menu, "to"), Some(2));
    // Uppercase = lowercase (case-insensitive).
    assert_eq!(typeahead_match(&menu, "TH"), Some(1));
    // No match.
    assert_eq!(typeahead_match(&menu, "xyz"), None);
    // Empty buffer.
    assert_eq!(typeahead_match(&menu, ""), None);
}

/// Drift guard. Disabled rows aren't typeahead targets
/// (would be confusing to highlight a row that can't dispatch).
#[test]
fn typeahead_skips_disabled_rows() {
    let menu = vec![item("Theme", false), item("Theme Plus", true)];
    // First Theme is disabled; second matches.
    assert_eq!(typeahead_match(&menu, "th"), Some(1));
}

/// Drift guard. `count_rows_fitting` walks rows from
/// `start` forward and sums heights until the next row would
/// exceed `panel_h`. Pinning the arithmetic so scroll math
/// can't silently drift if the row-height constants change.
#[test]
fn count_rows_fitting_respects_panel_height_and_separator_height() {
    let menu = vec![
        item("A", true),            // row_h=24
        item("B", true),            // row_h=24
        ContextMenuItem::Separator, // sep_h=8
        item("C", true),            // row_h=24
        item("D", true),            // row_h=24
        item("E", true),            // row_h=24
    ];
    let row_h = 24.0;
    let sep_h = 8.0;
    // panel_h = 0 -> nothing fits.
    assert_eq!(count_rows_fitting(&menu, 0, 0.0, row_h, sep_h), 0);
    // panel_h = 24 -> A only (B would push us to 48 > 24).
    assert_eq!(count_rows_fitting(&menu, 0, 24.0, row_h, sep_h), 1);
    // panel_h = 48 -> A + B.
    assert_eq!(count_rows_fitting(&menu, 0, 48.0, row_h, sep_h), 2);
    // panel_h = 56 -> A + B + separator (24 + 24 + 8 = 56).
    assert_eq!(count_rows_fitting(&menu, 0, 56.0, row_h, sep_h), 3);
    // panel_h = 1000 -> all 6 rows.
    assert_eq!(count_rows_fitting(&menu, 0, 1000.0, row_h, sep_h), 6);
    // Start past the separator: skip the first 3, fit C+D in 48.
    assert_eq!(count_rows_fitting(&menu, 3, 48.0, row_h, sep_h), 2);
    // Start at last row, plenty of height — just 1 fits.
    assert_eq!(count_rows_fitting(&menu, 5, 100.0, row_h, sep_h), 1);
    // Start past the end -> 0.
    assert_eq!(count_rows_fitting(&menu, 99, 1000.0, row_h, sep_h), 0);
    // The wheel clamp computes the earliest suffix once from the end rather
    // than rescanning every candidate offset.
    assert_eq!(context_menu_max_scroll_offset(&menu, 48.0, row_h, sep_h), 4);
    assert_eq!(context_menu_max_scroll_offset(&menu, 80.0, row_h, sep_h), 2);
    assert_eq!(
        context_menu_max_scroll_offset(&menu, 1000.0, row_h, sep_h),
        0
    );
    assert_eq!(
        context_menu_max_scroll_offset(&menu, 0.0, row_h, sep_h),
        menu.len() - 1
    );
    assert_eq!(context_menu_max_scroll_offset(&[], 48.0, row_h, sep_h), 0);
}

#[test]
fn context_menu_shrink_keeps_bottom_highlight_visible() {
    let menu: Vec<ContextMenuItem> = [
        "Row 0", "Row 1", "Row 2", "Row 3", "Row 4", "Row 5", "Row 6", "Row 7", "Row 8", "Row 9",
        "Row 10", "Row 11",
    ]
    .into_iter()
    .map(|label| item(label, true))
    .collect();
    let offset = context_menu_scroll_for_highlight(&menu, 0, 11, 72.0, 24.0, 8.0);
    assert_eq!(offset, 9);
    let visible = count_rows_fitting(&menu, offset, 72.0, 24.0, 8.0);
    assert!(
        (offset..offset + visible).contains(&11),
        "the row Enter will dispatch must remain in the visible slice"
    );
    assert_eq!(
        context_menu_scroll_for_highlight(&menu, usize::MAX, 0, 72.0, 24.0, 8.0),
        0,
        "a stale overscrolled offset is normalized when the top row is highlighted"
    );
}

/// Drift guard. With a 512-entry submenu and a real
/// surface-bound panel height of ~580px (surface_h=660 - 80px
/// chrome breathing room), `count_rows_fitting` reports a tiny
/// fraction of the total — the rest scrolls into view. The
/// "menu doesn't grow off-screen" invariant.
#[test]
fn theme_submenu_with_512_entries_clamps_panel_to_surface_height() {
    let big: Vec<ContextMenuItem> = (0..512)
        .map(|i| ContextMenuItem::Item {
            label: Box::leak(format!("Theme {i}").into_boxed_str()),
            action: Action::Paste,
            enabled: true,
        })
        .collect();
    let row_h = 24.0;
    let sep_h = 8.0;
    // 580px / 24px ≈ 24 rows visible.
    let visible = count_rows_fitting(&big, 0, 580.0, row_h, sep_h);
    assert!(
        (20..30).contains(&visible),
        "expected ~24 visible rows at 580px panel; got {visible}"
    );
    // All 512 rows shouldn't possibly fit in 580px.
    assert!(visible < big.len(), "panel must clamp; visible={visible}");
    let max_offset = context_menu_max_scroll_offset(&big, 580.0, row_h, sep_h);
    assert_eq!(
        max_offset,
        big.len() - visible,
        "the 512-row wheel clamp should be one reverse pass with no blank suffix"
    );
}

/// Drift guard. Hover-to-highlight walks `find_menu_row_y`
/// on every `CursorMoved`; pin the contract so a render-layout
/// change can't silently drop separator handling, off-by-one the
/// last row, or break the out-of-bounds contract.
#[test]
fn hover_updates_menu_highlight_skipping_separators() {
    // Menu layout: 4 rows with a separator between row 1 (Item)
    // and row 3 (Item):
    //   row 0 (Item)      [anchor + 0   .. anchor + row_h]
    //   row 1 (Item)      [anchor + 24  .. anchor + 48]
    //   row 2 (Separator) [anchor + 48  .. anchor + 56]
    //   row 3 (Item)      [anchor + 56  .. anchor + 80]
    let anchor_y = 100.0;
    let row_h = 24.0;
    let sep_h = 8.0;
    let kinds = [false, false, true, false];
    let panel_h = 80.0;
    // Cursor squarely inside row 0.
    assert_eq!(
        find_menu_row_y(anchor_y + 10.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(0)
    );
    // Cursor on the edge between row 0 and row 1: lands on row 1
    // (half-open interval [y, y+h)).
    assert_eq!(
        find_menu_row_y(anchor_y + row_h, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(1)
    );
    // Cursor inside row 1.
    assert_eq!(
        find_menu_row_y(anchor_y + 30.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(1)
    );
    // Cursor inside the separator — None (don't highlight a
    // visual-only gap).
    assert_eq!(
        find_menu_row_y(anchor_y + 50.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        None
    );
    // Cursor inside row 3 (after the separator).
    assert_eq!(
        find_menu_row_y(anchor_y + 60.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(3)
    );
    // A clamped panel that leaves only 14 px after the separator does not
    // render the 24 px fourth row. That blank strip must remain inert.
    assert_eq!(
        find_menu_row_y(anchor_y + 60.0, anchor_y, 70.0, row_h, sep_h, kinds,),
        None
    );
    // Cursor above the panel.
    assert_eq!(
        find_menu_row_y(anchor_y - 1.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        None
    );
    // Cursor below the last row.
    assert_eq!(
        find_menu_row_y(anchor_y + 1000.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        None
    );
    // Empty menu: always None.
    assert_eq!(
        find_menu_row_y(anchor_y + 10.0, anchor_y, panel_h, row_h, sep_h, []),
        None
    );
}

#[test]
fn picker_overlay_projection_installs_the_vertical_context_menu() {
    let menu = picker_overlay_context_menu(
        [
            Some(command_picker_list(
                "",
                3,
                &kettle_config::keybinds::defaults(),
                &kettle_i18n::Translator::new(kettle_i18n::Language::En),
                None,
            )),
            None,
            None,
            None,
        ],
        (480.0, 240.0),
        (8.0, 16.0),
    )
    .expect("an active command picker must install the shared vertical panel");

    assert!(menu.rows.len() > 3);
    assert_eq!(menu.highlight, 3);
    assert!(menu.rows.iter().all(|row| !row.separator));

    // The theme picker's rows use the same panel.
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let themes = super::theme_picker_list("", 2, "TokyoNight Night", &tr);
    let menu = picker_overlay_context_menu(
        [None, Some(themes), None, None],
        (480.0, 240.0),
        (8.0, 16.0),
    )
    .expect("an active theme picker must install the shared vertical panel");
    assert_eq!(menu.rows.len(), kettle_config::Theme::list().len());
    assert_eq!(menu.highlight, 2);
}

/// Terminator's right-click menu, row for row, plus the rows Ghostty adds.
///
/// Set Window Title, Split Auto, and Zoom/Restore must be in the menu, not
/// only on keybinds, because Terminator presents them there and users reach
/// for the mouse to find them.
///
/// Building a real menu needs a live App, so the row list is pinned at the
/// source. Terminator's own set is `terminal_popup_menu.py`.
#[test]
fn the_context_menu_carries_every_terminator_row() {
    let src = production_source();
    let body = src
        .split("fn context_menu_items(")
        .nth(1)
        .and_then(|b| b.split("\n    /// ").next())
        .expect("context_menu_items body");
    use kettle_i18n::{Language, Text as T, Translator};
    let en = Translator::new(Language::En);
    for (key, row, action) in [
        (T::MenuCopy, "Copy", "Action::Copy"),
        (T::MenuPaste, "Paste", "Action::Paste"),
        (
            T::MenuSetWindowTitle,
            "Set Window Title…",
            "Action::EditWindowTitle",
        ),
        (T::MenuSplitAuto, "Split Auto", "Action::SplitAuto"),
        (T::MenuSplitRight, "Split Right", "Action::SplitRight"),
        (T::MenuSplitDown, "Split Down", "Action::SplitDown"),
        (T::MenuClosePane, "Close Pane", "Action::ClosePane"),
        (T::MenuNewTab, "New Tab", "Action::NewTab"),
        (T::MenuReadOnly, "Read only", "Action::TogglePaneReadOnly"),
        (T::MenuSetGroup, "Set Group…", "Action::CreateGroup"),
        // Rows Ghostty adds to Terminator's set.
        (T::MenuSplitLeft, "Split Left", "Action::SplitLeft"),
        (T::MenuSplitUp, "Split Up", "Action::SplitUp"),
        (T::MenuCloseTab, "Close Tab", "Action::CloseTab"),
        (T::MenuNewWindow, "New Window", "Action::NewWindow"),
        (T::MenuCloseWindow, "Close Window", "Action::CloseWindow"),
        (T::MenuSetTabTitle, "Set Tab Title…", "Action::EditTabTitle"),
        (
            T::MenuSetPaneTitle,
            "Set Pane Title…",
            "Action::EditPaneTitle",
        ),
        (T::MenuResetTerminal, "Reset Terminal", "Action::Reset"),
    ] {
        // The row reads as Terminator's in English.
        assert_eq!(en.text(key), row);
        // Checked as a PAIR, not as two independent tokens: "row present"
        // and "action present" both still hold if two rows swap actions,
        // which is exactly the bug that sends a click somewhere else.
        // Matching per menu ENTRY also stops a key that appears in a
        // nearby comment from standing in for the row itself.
        let needle = format!("T::{key:?})");
        let entry = body
            .split("ContextMenuItem::")
            .find(|chunk| {
                chunk
                    .split("label:")
                    .nth(1)
                    .and_then(|rest| rest.split("action:").next())
                    .is_some_and(|label| label.contains(&needle))
            })
            .unwrap_or_else(|| panic!("the context menu is missing Terminator's {row:?} row"));
        assert!(
            entry.contains(action),
            "{row:?} must dispatch {action} — the row and its action must \
                 stay together, found entry: {entry:?}"
        );
    }
    // Zoom swaps label with the state rather than sitting on one word,
    // matching Terminator's Zoom/Restore pair.
    assert_eq!(en.text(T::MenuZoom), "Zoom");
    assert_eq!(en.text(T::MenuRestore), "Restore");
    assert!(
        body.contains("if zoomed { T::MenuRestore } else { T::MenuZoom }")
            && body.contains("Action::ToggleZoom"),
        "the zoom row must show Restore while zoomed and Zoom otherwise"
    );
    // Splitting a zoomed pane has nothing to put the new pane beside, and
    // `filter_disabled` drops the rows rather than greying them out.
    assert_eq!(
        body.matches("enabled: !zoomed,").count(),
        5,
        "all five split rows must be gated on the zoom state"
    );
}

#[test]
fn context_menu_middle_click_dismisses_before_paste_or_tab_close() {
    let src = production_source();
    assert!(
        src.matches("if ws.context_menu.is_some() && bcode != 2")
            .count()
            >= 2,
        "GUI and ctl mouse paths must dismiss an open context menu on left/middle click; \
             only right-click may fall through to relocate it"
    );
    let gui_menu_gate = src
        .find("if ws.context_menu.is_some() && bcode != 2")
        .expect("GUI context-menu dismiss gate present");
    let middle_paste = src
        .find("if bcode == 1 && !self.cfg.disable_mouse_paste")
        .expect("middle paste path present");
    assert!(
        gui_menu_gate < middle_paste,
        "open context menu must preempt middle-click paste behind it"
    );
}

#[test]
fn next_context_menu_highlight_skips_separators_and_disabled() {
    use super::{ContextMenuItem, next_context_menu_highlight};
    use kettle_config::Action;
    // Layout: [Copy(disabled), Paste, Separator, SplitRight,
    //          ClosePane(disabled), Separator, NewTab]
    // Indices:  0               1      2          3
    //           4                       5          6
    // Enabled-only walk should land 1 → 3 → 6 → 1 (wrap).
    let items = vec![
        ContextMenuItem::Item {
            label: "Copy",
            action: Action::Copy,
            enabled: false,
        },
        ContextMenuItem::Item {
            label: "Paste",
            action: Action::Paste,
            enabled: true,
        },
        ContextMenuItem::Separator,
        ContextMenuItem::Item {
            label: "Split Right",
            action: Action::SplitRight,
            enabled: true,
        },
        ContextMenuItem::Item {
            label: "Close Pane",
            action: Action::ClosePane,
            enabled: false,
        },
        ContextMenuItem::Separator,
        ContextMenuItem::Item {
            label: "New Tab",
            action: Action::NewTab,
            enabled: true,
        },
    ];
    // ↓ from Paste (1) skips Separator(2) → SplitRight(3).
    assert_eq!(next_context_menu_highlight(&items, 1, 1), 3);
    // ↓ from SplitRight(3) skips disabled ClosePane(4) + Separator
    // (5) → NewTab(6).
    assert_eq!(next_context_menu_highlight(&items, 3, 1), 6);
    // ↓ from NewTab(6) wraps past disabled Copy(0) → Paste(1).
    assert_eq!(next_context_menu_highlight(&items, 6, 1), 1);
    // ↑ from Paste(1) wraps past disabled Copy(0) → NewTab(6).
    assert_eq!(next_context_menu_highlight(&items, 1, -1), 6);
    // No enabled items at all — return `current` unchanged so the
    // caller doesn't crash. (Defensive — caller shouldn't open an
    // empty menu, but the guard exists.)
    let all_disabled = vec![ContextMenuItem::Separator];
    assert_eq!(next_context_menu_highlight(&all_disabled, 0, 1), 0);
}

/// A card menu's rows are ordinary rows: dispatchable, clicked with what
/// the menu captured, reachable by mnemonic and by typing, and measured
/// by their label.
#[test]
fn a_card_menu_row_is_an_ordinary_menu_row() {
    use super::{
        ContextMenuClick, ContextMenuItem, MediaMenuAction, MediaTarget, assign_mnemonics,
        context_menu_item_columns, item_is_dispatchable, item_to_click, typeahead_match,
    };
    let target = MediaTarget {
        pane: 3,
        nonce: kettle_core::InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap(),
        item: 9,
        generation: 2,
    };
    let rows = [
        ContextMenuItem::Media {
            label: "Open",
            target,
            action: MediaMenuAction::View,
        },
        ContextMenuItem::Media {
            label: "Open in Preview",
            target,
            action: MediaMenuAction::OpenOutside(crate::media::Outside::Image(
                crate::media::Viewer::Preview,
            )),
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        assert!(item_is_dispatchable(row));
        let Some(ContextMenuClick::Media {
            target: clicked,
            action,
        }) = item_to_click(row, index)
        else {
            panic!("row {index} is not a media click");
        };
        assert_eq!(clicked, target);
        assert_eq!(
            action,
            [
                MediaMenuAction::View,
                MediaMenuAction::OpenOutside(crate::media::Outside::Image(
                    crate::media::Viewer::Preview,
                ))
            ][index]
        );
    }
    assert!(context_menu_item_columns(&rows[1], "") >= "Open in Preview".len());
    let mnemonics = assign_mnemonics(&rows, &[]);
    assert!(mnemonics.iter().all(Option::is_some), "{mnemonics:?}");
    assert_ne!(mnemonics[0].map(|m| m.1), mnemonics[1].map(|m| m.1));
    assert_eq!(typeahead_match(&rows, "open in"), Some(1));
}

/// The shared row→click mapper must recognise EVERY dispatchable row type,
/// so the keyboard Enter / Space + mnemonic paths reach the same rows the
/// mouse hit-test does. A row type it misses is a keyboard dead-end.
#[test]
fn item_to_click_maps_every_dispatchable_row_type() {
    use super::{ContextMenuClick, ContextMenuItem, UrlHow, item_to_click};
    use kettle_config::Action;

    // Enabled Item / DynamicItem → Action.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: true
            },
            0
        ),
        Some(ContextMenuClick::Action(_))
    ));
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::DynamicItem {
                label: "Bold".into(),
                action: Action::Copy,
                enabled: true
            },
            0
        ),
        Some(ContextMenuClick::Action(_))
    ));
    // Submenu → DrillIntoSubmenu(idx) — carries the row index so the
    // dispatcher knows which level to descend into.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::Submenu {
                label: "Theme".into(),
                items: vec![]
            },
            7
        ),
        Some(ContextMenuClick::DrillIntoSubmenu(7))
    ));
    // Lua item → LuaMenuItem.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::LuaItem {
                label: "Plugin".into(),
                lua_idx: 3
            },
            0
        ),
        Some(ContextMenuClick::LuaMenuItem(3))
    ));
    // Config command → ConfigCommand.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::ConfigItem {
                label: "Clear".into(),
                command: "clear".into(),
            },
            0
        ),
        Some(ContextMenuClick::ConfigCommand(_))
    ));
    // Profile choices → SetProfile.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::ProfileChoice {
                label: "work".into(),
                profile: "work".into()
            },
            0
        ),
        Some(ContextMenuClick::SetProfile(_))
    ));
    // New-tab ▾ shell choice → NewTabWithArgv.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::NewTabShell {
                label: "bash".into(),
                argv: vec!["bash".into()]
            },
            0
        ),
        Some(ContextMenuClick::NewTabWithArgv(_))
    ));
    // URL-aware rows → Url { how } carrying the captured
    // address, for both the Open and Copy flavors.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::UrlItem {
                label: "Open Link",
                url: "https://example.com".into(),
                how: UrlHow::Open,
                pane: Some(7),
            },
            0
        ),
        // The pane the link came from rides along to the click.
        Some(ContextMenuClick::Url {
            how: UrlHow::Open,
            pane: Some(7),
            ..
        })
    ));
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::UrlItem {
                label: "Copy Link Address",
                url: "https://example.com".into(),
                how: UrlHow::Copy,
                pane: Some(7),
            },
            0
        ),
        Some(ContextMenuClick::Url {
            how: UrlHow::Copy,
            ..
        })
    ));
    // Non-dispatchable rows → None (disabled item + separator).
    assert!(
        item_to_click(
            &ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: false
            },
            0
        )
        .is_none()
    );
    assert!(item_to_click(&ContextMenuItem::Separator, 0).is_none());
}

/// Profile choice leaves must be keyboard-navigable, otherwise drilling
/// into a Profile ▸ submenu leaves ↑/↓ unable to land on any row.
#[test]
fn profile_choices_are_keyboard_navigable() {
    use super::{ContextMenuItem, item_is_dispatchable};
    assert!(item_is_dispatchable(&ContextMenuItem::ProfileChoice {
        label: "work".into(),
        profile: "work".into()
    }));
}

#[test]
fn submenu_transitions_reclamp_the_new_panel() {
    let src = super::production_source();
    let call = "self.reclamp_context_menu(ws, (width as f32, height as f32));";
    let drill = src
        .split("ContextMenuClick::DrillIntoSubmenu(idx) => {")
        .nth(1)
        .unwrap()
        .split("ContextMenuClick::Action(action) =>")
        .next()
        .unwrap();
    assert!(drill.contains(call));
    let back = src
        .split("fn context_menu_back(")
        .nth(1)
        .unwrap()
        .split("fn context_menu_activate(")
        .next()
        .unwrap();
    assert!(back.find(call).unwrap() < back.find("return;").unwrap());
}

#[test]
fn clamp_context_menu_anchor_keeps_panel_on_screen() {
    use super::clamp_context_menu_anchor;
    // 800x600 surface, 200x300 panel — anchors inside the safe
    // region pass through unchanged.
    assert_eq!(
        clamp_context_menu_anchor((100.0, 100.0), (200.0, 300.0), (800.0, 600.0)),
        (100.0, 100.0)
    );
    // Right-click near the bottom-right corner gets clamped so the
    // panel still fits with the 4-px margin (max_x = 800 - 200 - 4
    // = 596, max_y = 600 - 300 - 4 = 296).
    assert_eq!(
        clamp_context_menu_anchor((780.0, 580.0), (200.0, 300.0), (800.0, 600.0)),
        (596.0, 296.0)
    );
    // Right-click in the top-left corner clamps up against the
    // 4-px screen-edge margin so the panel doesn't glue to the
    // bezel.
    assert_eq!(
        clamp_context_menu_anchor((0.0, 0.0), (200.0, 300.0), (800.0, 600.0)),
        (4.0, 4.0)
    );
    // Pathological: panel larger than the surface — anchor clamps
    // to the 4-px margin without producing a NaN / negative.
    let (x, y) = clamp_context_menu_anchor((100.0, 100.0), (2000.0, 2000.0), (800.0, 600.0));
    assert!(x >= 4.0 && y >= 4.0);

    // An anchor that was valid on the old surface must be re-clamped when
    // a DPI transition or user resize shrinks the window underneath an
    // already-open menu.
    assert_eq!(
        clamp_context_menu_anchor((596.0, 296.0), (200.0, 300.0), (420.0, 320.0)),
        (216.0, 16.0)
    );
}

#[test]
fn context_menu_anchor_scales_once_across_dpi_then_reclamps() {
    use super::{clamp_context_menu_anchor, scale_context_menu_anchor};

    let scaled = scale_context_menu_anchor((100.0, 50.0), 1.0, 1.5);
    assert_eq!(scaled, (150.0, 75.0));
    assert_eq!(
        clamp_context_menu_anchor(scaled, (200.0, 100.0), (320.0, 180.0)),
        (116.0, 75.0),
        "the logical anchor should scale before the new physical surface clamps it"
    );
    assert_eq!(
        scale_context_menu_anchor(scaled, 1.5, 1.5),
        scaled,
        "recording the new scale must make later same-DPI resizes idempotent"
    );
    assert_eq!(
        scale_context_menu_anchor((12.0, 34.0), 0.0, 2.0),
        (12.0, 34.0),
        "invalid prior scale must fail closed"
    );
    assert_eq!(
        scale_context_menu_anchor((12.0, 34.0), 1.0, f64::NAN),
        (12.0, 34.0),
        "invalid current scale must fail closed"
    );
}

#[test]
fn context_menu_panel_clamp_makes_oversized_panel_fit_with_margin() {
    use super::{clamp_context_menu_anchor, clamp_context_menu_panel};

    let surface = (800.0, 600.0);
    assert_eq!(
        clamp_context_menu_panel((200.0, 300.0), surface),
        (200.0, 300.0),
        "an already-fitting panel must keep its natural size"
    );
    let panel = clamp_context_menu_panel((2000.0, 2000.0), surface);
    assert_eq!(panel, (792.0, 592.0));
    let anchor = clamp_context_menu_anchor((780.0, 580.0), panel, surface);
    assert_eq!(anchor, (4.0, 4.0));
    assert!(anchor.0 + panel.0 <= surface.0 - 4.0);
    assert!(anchor.1 + panel.1 <= surface.1 - 4.0);
}

#[test]
fn context_menu_requires_one_real_row_and_content_column() {
    let cell = (9.6, 18.25);
    let threshold = (
        8.0 + kettle_render::menu::H_PAD + cell.0,
        8.0 + cell.1 + kettle_render::menu::ROW_PAD,
    );
    assert!(context_menu_surface_can_fit_row(threshold, cell));
    assert!(!context_menu_surface_can_fit_row(
        (threshold.0 - 0.01, threshold.1),
        cell
    ));
    assert!(!context_menu_surface_can_fit_row(
        (threshold.0, threshold.1 - 0.01),
        cell
    ));
    assert!(!context_menu_surface_can_fit_row(
        (f32::NAN, threshold.1),
        cell
    ));
}

#[test]
fn oversized_menu_hint_is_removed_before_label_ellipsis() {
    use unicode_width::UnicodeWidthStr as _;

    let mut row = ContextMenuRow {
        label: "Visible label".to_string(),
        separator: false,
        enabled: true,
        hint: "Ctrl+".repeat(1000),
    };
    fit_context_menu_row(&mut row, 5);
    assert!(row.hint.is_empty());
    assert!(!row.label.is_empty());
    assert!(row.label.width() <= 5);
}

#[test]
fn context_menu_width_covers_every_rendered_row_shape() {
    use unicode_width::UnicodeWidthStr as _;

    let submenu = ContextMenuItem::Submenu {
        label: "主题".into(),
        items: vec![],
    };
    assert_eq!(
        context_menu_item_columns(&submenu, "ignored"),
        "主题 ▸".width()
    );

    let profile = ContextMenuItem::ProfileChoice {
        label: "👩‍💻 work".into(),
        profile: "work".into(),
    };
    assert_eq!(
        context_menu_item_columns(&profile, "ignored"),
        "👩‍💻 work".width()
    );

    let hinted = ContextMenuItem::Item {
        label: "Copy",
        action: Action::Copy,
        enabled: true,
    };
    assert_eq!(
        context_menu_item_columns(&hinted, "Ctrl+界"),
        "Copy".width() + "Ctrl+界".width() + 2
    );
}

#[test]
fn context_menu_label_ellipsis_is_bounded_and_utf8_safe() {
    use super::ellipsize_menu_label;
    use unicode_width::UnicodeWidthStr as _;

    assert_eq!(ellipsize_menu_label("Short", 10), "Short");
    assert_eq!(ellipsize_menu_label("abcdef", 5), "abcd…");
    assert_eq!(ellipsize_menu_label("éclair", 4), "écl…");
    assert_eq!(ellipsize_menu_label("👩‍💻abc", 3), "👩‍💻…");
    assert_eq!(ellipsize_menu_label("anything", 1), "…");
    assert_eq!(ellipsize_menu_label("anything", 0), "");
    for budget in 0..=8 {
        assert!(ellipsize_menu_label("multi-byte 中文 label", budget).width() <= budget);
    }
}

#[test]
fn finding_short_menu_keeps_one_complete_row() {
    let body = finding_body(include_str!("menus.rs"), "context_menu_geometry");
    assert!(
        body.contains(".max(row_h)"),
        "menu breathing room must yield to one complete row"
    );
}

#[test]
fn finding_ungroup_menu_checks_the_whole_tab() {
    let body = finding_body(include_str!("menus.rs"), "context_menu_items");
    let group = body
        .split("let has_group =")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(
        group.contains("tab_pane_ids"),
        "Ungroup Tab availability must consider siblings"
    );
}

#[test]
fn pending_shell_action_runs_the_latest_shortcut_on_completion() {
    let mut pending = super::PendingShellAction::default();
    assert_eq!(pending.request(1, 0, None), None);
    assert_eq!(pending.request(2, 1, None), None);
    let shells = vec![
        ("First".into(), vec!["first".into()]),
        ("Second".into(), vec!["second".into()]),
    ];
    assert_eq!(pending.complete(&shells), Some((2, vec!["second".into()])));
    assert_eq!(pending.complete(&shells), None);
    for (language, text) in [
        (kettle_i18n::Language::En, "Detecting shells…"),
        (kettle_i18n::Language::Es, "Detectando shells…"),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        let items = App::shell_menu_items(None, &tr);
        assert!(matches!(items.as_slice(), [ContextMenuItem::Info {label}] if label == text));
    }
    assert_eq!(
        pending.request(1, 1, Some(&shells)),
        Some(vec!["second".into()])
    );
    assert_eq!(pending.request(1, 0, None), None);
    assert_eq!(
        pending.request(2, 1, Some(&shells)),
        Some(vec!["second".into()])
    );
    assert_eq!(
        pending.complete(&shells),
        None,
        "a newer ready shortcut supersedes the held shortcut"
    );
}

#[test]
fn loading_shell_menu_survives_construction_and_can_refresh() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let items = filter_disabled(App::shell_menu_items(None, &tr));
        assert_eq!(items.len(), 1, "the loading menu must have a visible row");
        assert!(!super::item_is_dispatchable(&items[0]));
        assert!(App::shell_menu_is_loading(&items, &tr));
        let shells = vec![("Test shell".into(), vec!["test-shell".into()])];
        let ready = filter_disabled(App::shell_menu_items(Some(&shells), &tr));
        assert!(!App::shell_menu_is_loading(&ready, &tr));
        assert!(ready.iter().any(|item| matches!(item,
            ContextMenuItem::NewTabShell { argv, .. } if argv == &["test-shell"])));
        assert!(!App::shell_menu_is_loading(&[], &tr));
        assert!(!App::shell_menu_is_loading(
            &[ContextMenuItem::Info {
                label: "About".into(),
            }],
            &tr
        ));
    }
}

