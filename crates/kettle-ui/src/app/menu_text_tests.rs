use super::{App, ContextMenuItem, remote_reconnect_label};
use kettle_i18n::{Language, Translator};
use kettle_remote::{ContainerOptions, ContainerRuntime, RemoteContext, SshOptions};

const EN: Translator = Translator::new(Language::En);
const ES: Translator = Translator::new(Language::Es);

fn ssh(host: &str, user: Option<&str>) -> RemoteContext {
    RemoteContext::Ssh {
        host: host.to_string(),
        user: user.map(str::to_string),
        options: SshOptions::default(),
    }
}

fn container(runtime: ContainerRuntime, name: &str) -> RemoteContext {
    RemoteContext::Container {
        runtime,
        container: name.to_string(),
        options: ContainerOptions::default(),
    }
}

/// The reconnect row names the session as detected, in every language.
#[test]
fn the_reconnect_row_names_every_session_shape() {
    let me_box = ssh("box", Some("me"));
    let pod = container(ContainerRuntime::Kubectl, "my-pod");
    assert_eq!(remote_reconnect_label(&me_box, &EN), "Reconnect ssh me@box");
    assert_eq!(
        remote_reconnect_label(&ssh("box", None), &EN),
        "Reconnect ssh box"
    );
    assert_eq!(
        remote_reconnect_label(&container(ContainerRuntime::Docker, "foo"), &EN),
        "Re-attach docker foo"
    );
    assert_eq!(
        remote_reconnect_label(&pod, &EN),
        "Re-attach kubectl my-pod"
    );
    assert_eq!(
        remote_reconnect_label(&container(ContainerRuntime::Lxc, "c1"), &EN),
        "Re-attach lxc c1"
    );
    assert_eq!(
        remote_reconnect_label(&me_box, &ES),
        "Volver a conectar por ssh a me@box"
    );
    assert_eq!(
        remote_reconnect_label(&pod, &ES),
        "Volver a conectar a my-pod en kubectl"
    );
}

#[test]
fn the_new_tab_dropdown_follows_the_ui_language() {
    let shells = vec![("zsh".to_string(), vec!["zsh".to_string()])];
    let labels = |tr: &Translator| {
        App::new_tab_menu_items(&shells, Some(&[]), tr)
            .into_iter()
            .filter_map(|item| match item {
                ContextMenuItem::Item { label, .. } => Some(label.to_string()),
                ContextMenuItem::NewTabShell { label, .. } => Some(label),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        labels(&EN),
        [
            "zsh",
            "Save Layout…",
            "Open Layout…",
            "SSH launcher",
            "Settings…",
            "Command palette",
            "About kettle"
        ]
    );
    // Shell names are shown as detected.
    assert_eq!(
        labels(&ES),
        [
            "zsh",
            "Guardar diseño…",
            "Abrir diseño…",
            "Lanzador SSH",
            "Configuración…",
            "Paleta de comandos",
            "Acerca de kettle"
        ]
    );
}

#[test]
fn session_routes_follow_profiles_in_both_languages() {
    use super::{Action, profile_launch_entries};
    for tr in [EN, ES] {
        for names in [vec![], vec!["home".into(), "Work".into()]] {
            let menu = App::new_tab_menu_items(&[], Some(&names), &tr);
            let commands = super::palette_entries(None, &tr, Some(&names));
            for action in [
                Action::SaveLayout,
                Action::OpenLayoutPicker,
                Action::OpenSsh,
            ] {
                assert!(commands.iter().any(|(_, a)| *a == action));
                assert!(menu.iter().any(|row| matches!(row, ContextMenuItem::Item { action: a, enabled: true, .. } if *a == action)));
            }
            let submenu = menu
                .iter()
                .find_map(|row| match row {
                    ContextMenuItem::Submenu { items, .. } => Some(items),
                    _ => None,
                })
                .unwrap();
            let palette = profile_launch_entries(Some(&names), &tr);
            for entry in &palette {
                assert!(
                    commands
                        .iter()
                        .any(|row| row.1 == entry.1 && row.0.shown(&tr) == entry.0.shown(&tr))
                );
            }
            assert_eq!(submenu.len(), names.len().max(1));
            for (row, (label, action)) in submenu.iter().zip(&palette) {
                assert!(
                    matches!(row, ContextMenuItem::DynamicItem { label: shown, action: a, enabled: true } if shown == label.shown(&tr) && a == action)
                );
            }
            if names.is_empty() {
                assert_eq!(palette[0].1, Action::OpenProfilesDirectory);
                assert!(palette[0].0.shown(&tr).contains(
                    if tr.language() == kettle_i18n::Language::Es {
                        "Crear perfil"
                    } else {
                        "Create Profile"
                    }
                ));
            } else {
                for (entry, name) in palette.iter().zip(&names) {
                    assert_eq!(entry.1, Action::LaunchProfile(name.clone()));
                    assert!(entry.0.shown(&tr).contains(name));
                }
            }
        }
    }
}

#[test]
fn saved_layout_is_listed_by_picker_in_an_isolated_config_directory() {
    const CHILD: &str = "KETTLE_C19_LAYOUT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        use super::{
            Action, GroupBulkScope, TitleEditScope, begin_title_edit, clear_stale_title_edit,
            layout_picker_list, palette_entries,
        };
        let ws = super::WindowState::new(1, super::Mux::new());
        let mut prompt = begin_title_edit(
            &ws.mux,
            TitleEditScope::SaveLayout,
            GroupBulkScope::Single,
            "ignored",
        );
        assert!(prompt.is_some());
        assert!(!clear_stale_title_edit(&ws.mux, &mut prompt));
        let mut s = ws.mux.snapshot();
        // Use a portable serialized split fixture, without creating a PTY/window.
        s.tabs = vec![crate::session::STab {
            root: crate::session::SNode::Split {
                vertical: false,
                ratio: 0.5,
                a: Box::new(crate::session::SNode::Leaf {
                    cwd: Some("/tmp".into()),
                    cmd: vec![],
                    group: None,
                }),
                b: Box::new(crate::session::SNode::Leaf {
                    cwd: Some("/".into()),
                    cmd: vec![],
                    group: None,
                }),
            },
            focus: 0,
            title_override: None,
            zoomed: false,
        }];
        for invalid in ["", "../escape", ".", "..", "two words", &"x".repeat(129)] {
            assert!(s.save_layout_result(invalid).is_err());
        }
        s.save_layout_result("panel-test").unwrap();
        let layouts = crate::session::Session::list_layouts();
        assert_eq!(layouts, ["panel-test"]);
        let loaded = crate::session::Session::load_layout_with_identity("panel-test")
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&s).unwrap()
        );
        assert_eq!(
            layout_picker_list("", 0, &layouts, &ES).rows[0].label,
            "panel-test"
        );
        assert!(!layout_picker_list("", 0, &[], &ES).rows[0].enabled);
        let profiles = kettle_config::Config::default_path()
            .unwrap()
            .parent()
            .unwrap()
            .join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        for name in ["Work", "home"] {
            std::fs::write(profiles.join(format!("{name}.config")), "").unwrap();
        }
        let entries = palette_entries(None, &ES, Some(&kettle_config::Config::list_profiles()));
        let launches: Vec<_> = entries
            .iter()
            .filter_map(|(_, action)| match action {
                Action::LaunchProfile(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(launches, ["home", "Work"]);
        return;
    }
    let dir = kettle_test_support::private_tempdir("kettle-layout-picker-");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "app::menu_text_tests::saved_layout_is_listed_by_picker_in_an_isolated_config_directory", "--nocapture"])
        .env(CHILD, "1").env("XDG_CONFIG_HOME", dir.path()).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
