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
        App::new_tab_menu_items(&shells, tr)
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
        ["zsh", "Settings…", "Command palette", "About kettle"]
    );
    // Shell names are shown as detected.
    assert_eq!(
        labels(&ES),
        [
            "zsh",
            "Configuración…",
            "Paleta de comandos",
            "Acerca de kettle"
        ]
    );
}
