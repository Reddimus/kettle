//! Settings row keys (where each one moves the focused row and what it does
//! to its value) and the panel state they leave.

use super::*;
use kettle_i18n::Language;

const EN: Translator = Translator::new(Language::En);

fn category(cats: &[Category], name: Text) -> usize {
    cats.iter().position(|cat| cat.name == name).unwrap()
}

fn focused(cats: &[Category], nav: &SettingsNav) -> &'static str {
    cats[nav.category].fields[nav.field].key
}

/// `G`, `Ctrl+D` and `Ctrl+U` skip rows that do not apply, as ↑/↓ do:
/// "Stop blinking after" while the cursor does not blink, and the wallpaper
/// rows behind a solid background.
#[test]
fn jumps_skip_rows_that_do_not_apply() {
    let cats = categories(&[]);
    let cfg = Config {
        cursor_blink: false,
        ..Config::default()
    };
    let appearance = category(&cats, Text::SettingsCategoryAppearance);
    assert!(field_disabled(&cfg, "cursor-blink-timeout"));
    let mut nav = SettingsNav {
        category: appearance,
        ..Default::default()
    };
    nav.move_focus(&cats, &cfg, RowMove::Last);
    assert_eq!(focused(&cats, &nav), "cursor-blink");
    nav.move_focus(&cats, &cfg, RowMove::First);
    assert_eq!(nav.field, 0);
    for to in [RowMove::PageDown, RowMove::PageUp] {
        for _ in 0..cats[appearance].fields.len() {
            nav.move_focus(&cats, &cfg, to);
            assert!(
                !field_disabled(&cfg, focused(&cats, &nav)),
                "{to:?} focused {}",
                focused(&cats, &nav)
            );
        }
    }
    assert_eq!(nav.field, 0);
    nav.move_focus(&cats, &cfg, RowMove::PageDown);
    nav.move_focus(&cats, &cfg, RowMove::PageDown);
    assert_eq!(focused(&cats, &nav), "cursor-blink");

    let cfg = Config::default();
    assert_eq!(cfg.background_type, kettle_config::BackgroundType::Solid);
    let background = category(&cats, Text::SettingsCategoryBackground);
    for to in [
        RowMove::Last,
        RowMove::PageDown,
        RowMove::PageUp,
        RowMove::First,
    ] {
        let mut nav = SettingsNav {
            category: background,
            ..Default::default()
        };
        nav.move_focus(&cats, &cfg, to);
        assert_eq!(focused(&cats, &nav), "background-type", "{to:?}");
    }
    // A click on the category keeps the row number it can, not a row that
    // does not apply.
    let mut nav = SettingsNav {
        category: appearance,
        field: 3,
        ..Default::default()
    };
    nav.apply_hit(
        &cats,
        &cfg,
        &EN,
        kettle_render::SettingsHit::Category(background),
    );
    assert_eq!(nav.category, background);
    assert_eq!(focused(&cats, &nav), "background-type");
}

/// Tab opens a category on its first row that applies.
#[test]
fn a_new_category_opens_on_its_first_row_that_applies() {
    let cats = vec![
        Category {
            name: Text::SettingsCategoryAppearance,
            fields: vec![toggle(Text::SettingsFieldCursorBlink, "cursor-blink")],
            groups: Vec::new(),
        },
        Category {
            name: Text::SettingsCategoryBackground,
            fields: vec![
                text(
                    Text::SettingsFieldBackgroundImage,
                    "background-image",
                    Text::SettingsValueImagePlaceholder,
                ),
                toggle(Text::SettingsFieldShowTitlebar, "show-titlebar"),
            ],
            groups: Vec::new(),
        },
    ];
    let cfg = Config::default();
    assert!(field_disabled(&cfg, "background-image"));
    for next in [true, false] {
        let mut nav = SettingsNav::default();
        nav.switch_category(&cats, &cfg, next);
        assert_eq!((nav.category, nav.field), (1, 1));
    }
}

/// Home, End, PageUp and PageDown move as `g`, `G`, Ctrl+U and Ctrl+D do,
/// with or without `vim-menu-nav`; Shift+←/→ (and Shift+h/l) take ten of a
/// number's steps.
#[test]
fn list_keys_move_and_shift_steps_numbers_coarsely() {
    use winit::keyboard::{Key, ModifiersState as Mods, NamedKey};
    for vim in [false, true] {
        for (key, to) in [
            (NamedKey::Home, RowMove::First),
            (NamedKey::End, RowMove::Last),
            (NamedKey::PageUp, RowMove::PageUp),
            (NamedKey::PageDown, RowMove::PageDown),
        ] {
            assert_eq!(
                row_command(&Key::Named(key), Mods::empty(), vim),
                Some(RowCommand::Move(to)),
                "{key:?} vim={vim}"
            );
        }
    }
    let step = |key: &Key, mods| match row_command(key, mods, true) {
        Some(RowCommand::Value(value)) => value,
        other => panic!("{key:?} {mods:?}: {other:?}"),
    };
    let right = Key::Named(NamedKey::ArrowRight);
    let left = Key::Named(NamedKey::ArrowLeft);
    let l = Key::Character("L".into());
    let cats = categories(&[]);
    let field = |key: &str| {
        cats.iter()
            .flat_map(|cat| &cat.fields)
            .find(|field| field.key == key)
            .unwrap()
    };
    let cfg = Config {
        cursor_blink_timeout: 100,
        ..Config::default()
    };
    let timeout = field("cursor-blink-timeout");
    let dir = |key: &Key, mods| value_key_dir(&cfg, timeout, step(key, mods)).unwrap();
    assert_eq!(dir(&right, Mods::empty()), 1);
    assert_eq!(dir(&right, Mods::SHIFT), COARSE_STEPS);
    assert_eq!(dir(&left, Mods::SHIFT), -COARSE_STEPS);
    assert_eq!(dir(&l, Mods::SHIFT), COARSE_STEPS);
    assert_eq!(
        next_value(&cfg, timeout, dir(&right, Mods::SHIFT)),
        "150",
        "ten 5 s steps"
    );
    assert_eq!(next_value(&cfg, timeout, dir(&left, Mods::SHIFT)), "50");
    // A choice moves one choice, Shift or not.
    let bell = field("bell");
    assert_eq!(
        value_key_dir(&cfg, bell, step(&right, Mods::SHIFT)),
        Some(1)
    );
}

/// In search results, Home and End jump to the first and last result that
/// applies, and page moves stop at the ends instead of wrapping.
#[test]
fn list_keys_move_through_search_results() {
    use winit::keyboard::{Key, ModifiersState as Mods, NamedKey};
    let cats = categories(&[]);
    let cfg = Config::default();
    let mut nav = SettingsNav {
        query: "tab".into(),
        ..Default::default()
    };
    nav.reconcile(&cats, &cfg, &EN);
    let results: Vec<_> = nav
        .visible_fields(&cats, &EN)
        .into_iter()
        .filter(|&(c, f)| !field_disabled(&cfg, cats[c].fields[f].key))
        .collect();
    assert!(results.len() > 4);
    for vim in [false, true] {
        let cfg = Config {
            vim_menu_nav: vim,
            ..Config::default()
        };
        let mut press = |key| {
            assert_eq!(
                nav.search_key(&cats, &cfg, &EN, &Key::Named(key), Mods::empty(), false),
                SearchRoute::Handled,
                "{key:?}"
            );
            (nav.category, nav.field)
        };
        assert_eq!(press(NamedKey::End), *results.last().unwrap());
        assert_eq!(press(NamedKey::PageDown), *results.last().unwrap());
        assert_eq!(press(NamedKey::Home), results[0]);
        assert_eq!(press(NamedKey::PageUp), results[0]);
        assert_ne!(press(NamedKey::PageDown), results[0]);
    }
}

/// ← turns a switch off and → turns it on, and neither changes a switch
/// already there; Space and Enter flip it. Space and Enter leave a number
/// alone, since they could only step it up; choices still cycle.
#[test]
fn arrows_set_switches_and_space_leaves_numbers_alone() {
    let cats = categories(&[]);
    let field = |key: &str| {
        cats.iter()
            .flat_map(|cat| &cat.fields)
            .find(|field| field.key == key)
            .unwrap()
    };
    let left = ValueKey::Step {
        dir: -1,
        coarse: false,
    };
    let right = ValueKey::Step {
        dir: 1,
        coarse: false,
    };
    let blink = field("cursor-blink");
    for on in [true, false] {
        let cfg = Config {
            cursor_blink: on,
            ..Config::default()
        };
        assert_eq!(value_key_dir(&cfg, blink, right), (!on).then_some(1));
        assert_eq!(value_key_dir(&cfg, blink, left), on.then_some(-1));
        assert_eq!(value_key_dir(&cfg, blink, ValueKey::Activate), Some(0));
        if let Some(dir) = value_key_dir(&cfg, blink, right) {
            assert_eq!(next_value(&cfg, blink, dir), "true");
        }
        if let Some(dir) = value_key_dir(&cfg, blink, left) {
            assert_eq!(next_value(&cfg, blink, dir), "false");
        }
    }
    let cfg = Config::default();
    let opacity = field("background-opacity");
    assert_eq!(value_key_dir(&cfg, opacity, ValueKey::Activate), None);
    assert_eq!(value_key_dir(&cfg, opacity, right), Some(1));
    assert_eq!(value_key_dir(&cfg, opacity, left), Some(-1));
    let bell = field("bell");
    assert_eq!(value_key_dir(&cfg, bell, ValueKey::Activate), Some(0));
}

/// A graphics change waits for a restart for as long as the saved value
/// differs from the one Kettle started with, and no longer once it is put
/// back. A surface the window lacks asks for a new window, unless a restart
/// is already pending, which covers it.
#[test]
fn a_pending_restart_lasts_until_the_change_is_undone() {
    let launch = Config::default();
    let running = GraphicsSettings::of(&launch);
    assert_eq!(
        pending_apply(Some(&running), &launch, false),
        SettingsApply::None
    );
    let changed = [
        Config {
            gpu_power_preference: kettle_config::GpuPowerPreference::Low,
            ..Config::default()
        },
        Config {
            gpu_backend: kettle_config::GpuBackend::Gl,
            ..Config::default()
        },
        Config {
            gpu_force_software: !launch.gpu_force_software,
            ..Config::default()
        },
        Config {
            gpu_vendor_id: 0x10de,
            gpu_device_id: 0x2684,
            gpu_name: "GeForce".into(),
            ..Config::default()
        },
        Config::parse_text(if read_choice(&launch, "renderer") == "cpu" {
            "renderer = gpu"
        } else {
            "renderer = cpu"
        }),
    ];
    for cfg in &changed {
        for lacks in [false, true] {
            assert_eq!(
                pending_apply(Some(&running), cfg, lacks),
                SettingsApply::Restart
            );
        }
    }
    assert_eq!(
        pending_apply(Some(&running), &launch, true),
        SettingsApply::NewWindow
    );
    // Before the first renderer there is nothing to restart.
    assert_eq!(pending_apply(None, &changed[0], false), SettingsApply::None);
}
