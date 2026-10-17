use super::{
    Client, HintAction, HintWhat, LinkGate, PathOrigin, argv_client, hint_action, hint_path_url,
    hint_previews, link_display_name, link_gate, link_menu_rows, path_origin, previewable_name,
    production_source,
};
use super::{CopiedPreview, copied_preview, lane_action_hit, unquote_copied_path};

/// The lane actions work as the lane's own controls; no other action
/// does.
#[test]
fn lane_actions_work_as_the_lanes_controls() {
    use kettle_config::Action;
    use kettle_render::MediaLaneHit as Hit;
    assert_eq!(lane_action_hit(&Action::PreviewNext), Some(Hit::Next));
    assert_eq!(
        lane_action_hit(&Action::PreviewPrevious),
        Some(Hit::Previous)
    );
    assert_eq!(
        lane_action_hit(&Action::PreviewNextDiagram),
        Some(Hit::NextPage)
    );
    assert_eq!(
        lane_action_hit(&Action::PreviewPreviousDiagram),
        Some(Hit::PreviousPage)
    );
    assert_eq!(lane_action_hit(&Action::ClosePreview), Some(Hit::Close));
    assert_eq!(lane_action_hit(&Action::OpenMediaShelf), None);
}
use kettle_core::hints::Kind;

/// A file a file manager copied previews as itself: the first listed one
/// a lane can show, local by construction, and the text beside it unread.
#[test]
fn a_copied_file_previews_the_first_a_lane_can_show() {
    let root = tempfile::tempdir().unwrap();
    let notes = root.path().join("notes.txt");
    let plot = root.path().join("plot.png");
    assert_eq!(
        copied_preview(&[notes.clone(), plot.clone()], Some("/x/a.svg"), None, None),
        Some(CopiedPreview::File(plot.clone()))
    );
    assert_eq!(copied_preview(&[notes], Some("/x/a.svg"), None, None), None);
    // A relative entry is no file Kettle can place, even one a file URL
    // would read as absolute.
    assert_eq!(copied_preview(&["plot.png".into()], None, None, None), None);
    if !cfg!(windows) {
        assert_eq!(
            copied_preview(&["C:/media/plot.png".into()], None, None, None),
            None
        );
    }
    // A CRLF file list leaves a CR on the line, which is no part of it.
    let crlf = format!("{}\r", plot.to_str().unwrap());
    assert_eq!(
        copied_preview(&[crlf.into()], None, None, None),
        Some(CopiedPreview::File(plot.clone()))
    );
}

/// Copied text previews when it is one path or `file://` link to a file a
/// lane can show; quotes or a shell's escapes around a path are undone.
#[cfg(unix)]
#[test]
fn copied_text_previews_one_path_or_file_link() {
    use super::MAX_COPIED_PATH;
    let link = |url: &str| Some(CopiedPreview::Link(url.into()));
    let copied =
        |text: &str| copied_preview(&[], Some(text), Some("/home/me/proj"), Some("/home/me"));
    assert_eq!(
        copied("/tmp/out/plot.png"),
        link("file:///tmp/out/plot.png")
    );
    assert_eq!(
        copied("  out/plot.SVG\n"),
        link("file:///home/me/proj/out/plot.SVG")
    );
    assert_eq!(copied("~/flow.mmd"), link("file:///home/me/flow.mmd"));
    assert_eq!(
        copied("'/tmp/my plot.png'"),
        link("file:///tmp/my%20plot.png")
    );
    assert_eq!(
        copied("/tmp/my\\ plot.png"),
        link("file:///tmp/my%20plot.png")
    );
    assert_eq!(copied("file:///tmp/plot.png"), link("file:///tmp/plot.png"));
    for refused in [
        "/tmp/notes.txt",
        "file:///tmp/notes.txt",
        "https://example.com/plot.png",
        "ssh://host/plot.png",
        "../plot.png",
        "/tmp/a.png\n/tmp/b.png",
        "",
    ] {
        assert_eq!(copied(refused), None, "{refused:?}");
    }
    let long = format!("/{}.png", "a".repeat(MAX_COPIED_PATH));
    assert_eq!(copied(&long), None);
    assert_eq!(
        copied_preview(&[], Some("out/plot.png"), None, None),
        None,
        "a relative path needs the pane's folder"
    );
    assert_eq!(copied_preview(&[], None, None, None), None);
}

/// On Windows a copied path previews only as a drive path, never a share
/// or a path relative to an unknown folder.
#[cfg(windows)]
#[test]
fn copied_text_previews_only_drive_paths_on_windows() {
    let copied = |text: &str| copied_preview(&[], Some(text), None, None);
    assert_eq!(
        copied("C:\\Users\\me\\plot.png"),
        Some(CopiedPreview::Link("file:///C:/Users/me/plot.png".into()))
    );
    for refused in [
        "\\\\host\\share\\plot.png",
        "plot.png",
        "C:\\x\\..\\plot.png",
    ] {
        assert_eq!(copied(refused), None, "{refused:?}");
    }
}

/// One pair of matching quotes goes; off Windows, so do a shell's escapes.
#[test]
fn copied_paths_lose_one_pair_of_quotes() {
    assert_eq!(unquote_copied_path("\"/a b.png\""), "/a b.png");
    assert_eq!(unquote_copied_path("'/a b.png'"), "/a b.png");
    assert_eq!(
        unquote_copied_path("\"/a.png'"),
        "\"/a.png'",
        "unmatched quotes stay"
    );
    assert_eq!(unquote_copied_path("'"), "'");
    let escaped = unquote_copied_path("/a\\ b\\(1\\).png");
    if cfg!(windows) {
        assert_eq!(escaped, "/a\\ b\\(1\\).png");
    } else {
        assert_eq!(escaped, "/a b(1).png");
    }
}

/// Shift+right-click on the selection opens the menu rather than
/// extending it, and only Shift+right-click reads the clipboard for its
/// row. The lane actions act on the focused pane's lane through the
/// lane's own controls.
#[test]
fn shift_right_click_and_lane_actions_are_wired() {
    let src = production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert!(src.contains(
        "if ws.mods.shift_key()\n                        && self.pointer_on_selection(ws, area) == Some(false)\n                        && self.extend_selection_to_cursor(ws, area, bcode)"
    ));
    let menu = body("open_context_menu");
    let shift = menu
        .find("&& offer_copied")
        .expect("the Shift+right-click check");
    let read = menu
        .find("self.copied_preview(ws, pane)")
        .expect("the clipboard read");
    assert!(
        shift < read,
        "the clipboard is read only on Shift+right-click"
    );
    assert_eq!(src.matches("self.copied_preview(ws, pane)").count(), 2);
    assert!(src.contains("self.open_context_menu(ws, px, py, false);"));
    assert_eq!(
        src.matches("let offer_copied = ws.mods.shift_key();\n")
            .count(),
        2,
        "only the native and control right-clicks offer the copied file"
    );
    assert!(src.contains("Action::PreviewClipboardPath => self.preview_copied_file(ws),"));
    assert!(src.contains(
        "&& ws.preview_panels.contains_key(&pane)\n                {\n                    self.act_on_preview(ws, pane, hit);"
    ));
}

/// A right-click on a link leads with Open and Copy; Preview in Kettle
/// joins them, between the two, only for a local file a lane can show
/// from a pane Kettle still knows.
#[test]
fn the_menu_offers_a_preview_only_for_a_file_a_lane_can_show() {
    use super::{ContextMenuItem, UrlHow};
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let root = tempfile::tempdir().unwrap();
    let link = |name: &str| {
        kettle_core::links::file_url_for_path(&root.path().join(name)).expect("a file URL")
    };
    let rows = |url: &str, pane| {
        link_menu_rows(url, pane, tr)
            .into_iter()
            .map(|row| match row {
                ContextMenuItem::UrlItem {
                    label,
                    url: row_url,
                    how,
                    pane: row_pane,
                } => {
                    assert_eq!((row_url.as_str(), row_pane), (url, pane));
                    Some((label, how))
                }
                ContextMenuItem::Separator => None,
                _ => panic!("a link row or the separator"),
            })
            .collect::<Vec<_>>()
    };
    let open = Some(("Open Link", UrlHow::Open));
    let copy = Some(("Copy Link Address", UrlHow::Copy));
    assert_eq!(
        rows(&link("plot.svg"), Some(4)),
        [
            open,
            Some(("Preview in Kettle", UrlHow::Preview)),
            copy,
            None
        ]
    );
    for (url, pane) in [
        (link("plot.svg"), None),
        (link("notes.txt"), Some(4)),
        ("https://example.com/plot.svg".to_string(), Some(4)),
    ] {
        assert_eq!(rows(&url, pane), [open, copy, None], "{url} {pane:?}");
    }
}

/// Quick select for previews labels a file a lane can show, by its
/// extension in any case: a whole path, or a local `file://` link.
#[test]
fn preview_quick_select_labels_only_files_a_lane_can_show() {
    for name in [
        "plot.png",
        "a.JPG",
        "b.jpeg",
        "c.webp",
        "d.bmp",
        "e.gif",
        "f.Svg",
        "g.mmd",
        "h.mermaid",
        "README.md",
        "plan.Markdown",
        "run.webm",
        "clip.MP4",
        "take.mov",
        "screen.mkv",
    ] {
        assert!(previewable_name(name), "{name}");
    }
    for name in [
        "notes.txt",
        "png",
        "plot.png.zip",
        "Makefile",
        "plot.",
        "",
        "notes.mdx",
        "index.ts",
    ] {
        assert!(!previewable_name(name), "{name}");
    }
    let text = |kind, text: &str, bounded| HintWhat::Text {
        kind,
        text: text.into(),
        bounded,
    };
    assert_eq!(
        hint_previews(&text(Kind::Path, "out/plot.png", true)),
        !cfg!(windows),
        "a path previews where it resolves"
    );
    assert!(!hint_previews(&text(Kind::Path, "out/plot.png", false)));
    assert!(!hint_previews(&text(Kind::Path, "out/notes.txt", true)));
    assert!(!hint_previews(&text(Kind::Hash, "deadbeef.png", true)));
    let root = tempfile::tempdir().unwrap();
    let link = |name: &str| {
        kettle_core::links::file_url_for_path(&root.path().join(name)).expect("a file URL")
    };
    assert!(hint_previews(&text(Kind::Url, &link("plot.png"), false)));
    assert!(!hint_previews(&text(Kind::Url, &link("notes.txt"), true)));
    assert!(!hint_previews(&text(
        Kind::Url,
        "https://example.com/plot.png",
        true
    )));
    assert!(!hint_previews(&HintWhat::Card(
        kettle_core::InlineNonce::new([2, 3, 4, 5, 6, 7]).unwrap()
    )));
}

#[test]
fn www_hints_are_normalized_only_for_opening() {
    for (raw, target) in [
        ("www.example.com", "https://www.example.com"),
        ("https://example.com", "https://example.com"),
    ] {
        let hints = kettle_core::hints::detect(&[raw]);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].text, raw);
        assert_eq!(hint_action(hints[0].kind, true, false), HintAction::Copy);
        assert_eq!(kettle_core::links::normalize_url(raw), target);
        assert!(kettle_core::links::is_safe_url(target));
    }
    let src = production_source();
    let open = src
        .split("fn open_pane_link(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("pane link opener");
    let normalized = open
        .find("normalize_url(uri)")
        .expect("normalize the target");
    let gate = open
        .find("match link_gate(uri, origin)")
        .expect("pane gate");
    assert!(normalized < gate);
}

/// Picking a label acts as before; with Shift a URL is copied and a path
/// opens, unless the path may not be a whole local file.
#[test]
fn shift_swaps_open_and_copy() {
    for unsafe_path in [false, true] {
        assert_eq!(
            hint_action(Kind::Url, false, unsafe_path),
            HintAction::OpenUrl
        );
        assert_eq!(hint_action(Kind::Url, true, unsafe_path), HintAction::Copy);
        assert_eq!(
            hint_action(Kind::Path, false, unsafe_path),
            HintAction::Copy
        );
        for kind in [Kind::Hash, Kind::Ip] {
            assert_eq!(hint_action(kind, false, unsafe_path), HintAction::Copy);
            assert_eq!(hint_action(kind, true, unsafe_path), HintAction::Copy);
        }
    }
    assert_eq!(hint_action(Kind::Path, true, false), HintAction::OpenPath);
    assert_eq!(hint_action(Kind::Path, true, true), HintAction::Copy);

    // Shift is the modifier, not the letter's case, so Caps Lock is not
    // Shift; the target's own pane, boundary and remoteness decide.
    let src = production_source();
    let handler = src
        .split("fn hint_key(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("hint key handler");
    assert!(handler.contains("let alternate = ws.mods.shift_key();"));
    assert!(!handler.contains("is_ascii_uppercase"));
    let act = src
        .split("fn act_hint(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("act_hint");
    for needle in [
        "ws.mux.panes.get(&h.pane)",
        "pane.map(pane_path_origin)",
        "hint_action(kind, alternate, remote || !bounded)",
        "!bounded",
        // A path it will not open falls back to the copy a plain label does.
        "(HintAction::OpenPath, None) | (HintAction::Copy, _) =>",
    ] {
        assert!(act.contains(needle), "act_hint must use {needle}");
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn remote_clients_are_not_local() {
    for (words, client) in [
        (&["ssh", "host"][..], Client::Remote),
        (&["mosh-client", "x"], Client::Remote),
        (&["docker", "exec", "-it", "c", "sh"], Client::Remote),
        (&["kubectl", "exec", "p"], Client::Remote),
        (
            &["C:\\Windows\\System32\\OpenSSH\\ssh.exe", "h"],
            Client::Remote,
        ),
    ] {
        assert_eq!(argv_client(&argv(words), 0), Some(client), "{words:?}");
    }
    for words in [
        // A multiplexer's server runs on this computer.
        &["tmux", "x"][..],
        &["/usr/bin/screen", "-r"],
        &["zellij"],
        &["zsh"],
        &["bash"],
        &["vim", "ssh.txt"],
        &["tmuxinator-notes"],
        &[],
    ] {
        assert_eq!(argv_client(&argv(words), 0), None, "{words:?}");
    }
}

/// A client run through a shell's `-c` or a wrapper is still found: the
/// shell-wrapped tmux B1 left open.
#[test]
fn wrapped_clients_are_found() {
    for (words, client) in [
        (&["sh", "-c", "ssh attach -t work"][..], Client::Remote),
        (&["/bin/bash", "-lc", "exec ssh host"], Client::Remote),
        (
            &["zsh", "-c", "TERM=xterm-256color ssh host"],
            Client::Remote,
        ),
        (&["env", "TERM=xterm", "ssh", "host"], Client::Remote),
        (&["env", "-u", "TMUX", "ssh", "new"], Client::Remote),
        (&["sudo", "-u", "me", "ssh"], Client::Remote),
        (&["nice", "-n", "5", "mosh", "host"], Client::Remote),
        (&["sshpass", "-p", "secret", "ssh", "host"], Client::Remote),
        (&["caffeinate", "-i", "sh", "-c", "ssh -r"], Client::Remote),
        // Quotes, chained commands and pipelines.
        (&["sh", "-c", "\"ssh\" attach; :"], Client::Remote),
        (&["sh", "-c", "cd ~ && 'ssh' attach"], Client::Remote),
        (&["bash", "-c", "echo hi | ssh host"], Client::Remote),
        (&["sh", "-c", "tmux attach || ssh host"], Client::Remote),
        (&["sh", "-c", "(exec env A=1 ssh host)"], Client::Remote),
        // Flags that take no value do not swallow the client, and long
        // or unknown options do not hide it.
        (&["sudo", "-n", "ssh"], Client::Remote),
        (&["time", "-p", "ssh", "host"], Client::Remote),
        (&["env", "--", "ssh", "host"], Client::Remote),
        (&["sudo", "--user", "alice", "ssh"], Client::Remote),
        (&["sudo", "--preserve-env", "X", "ssh", "h"], Client::Remote),
        (
            &["sh", "-c", "sudo --user alice ssh attach; :"],
            Client::Remote,
        ),
        (&["sh", "-c", "command -p ssh attach; :"], Client::Remote),
        (&["sh", "-c", "exec -a work ssh attach"], Client::Remote),
        (&["zsh", "-c", "noglob ssh host"], Client::Remote),
        // Shell keywords before a command, and commands the parser
        // cannot name, which fail closed.
        (
            &["sh", "-c", "if true; then ssh attach; fi; :"],
            Client::Remote,
        ),
        (&["sh", "-c", "while :; do ssh host; done"], Client::Remote),
        (&["sh", "-c", "! { ssh; }"], Client::Remote),
        (&["sh", "-c", "time -p ssh host"], Client::Remote),
        (&["sh", "-c", "$EDITOR notes"], Client::Remote),
        (&["sh", "-c", "eval tmux"], Client::Remote),
        (&["sh", "-c", "`which tmux`"], Client::Remote),
        // Command substitutions are commands too.
        (
            &[
                "bash",
                "-c",
                "printf '%s\\n' \"$(ssh host cat /tmp/links)\"; exec bash",
            ],
            Client::Remote,
        ),
        (&["sh", "-c", "echo `ssh ls`"], Client::Remote),
        (&["sh", "-c", "echo $(echo $(ssh h))"], Client::Remote),
        (&["sh", "-c", "echo $(unclosed"], Client::Remote),
        (
            &["sh", "-c", "echo \"$(printf ')'; ssh host)\"; :"],
            Client::Remote,
        ),
        // Redirections before a command.
        (&["bash", "-c", "2>/dev/null ssh attach; :"], Client::Remote),
        (&["sh", "-c", "2> /dev/null ssh h"], Client::Remote),
        (&["sh", "-c", ">/tmp/log 2>&1 ssh"], Client::Remote),
        (&["sh", "-c", "&>/tmp/log ssh h; :"], Client::Remote),
        (&["sh", "-c", "ssh attach 2>&1; :"], Client::Remote),
        (&["sh", "-c", "ssh</dev/tty attach; :"], Client::Remote),
        (&["sh", "-c", "ssh>/dev/null host"], Client::Remote),
        // A backslash-newline continues the word.
        (&["sh", "-c", "s\\\nsh host; :"], Client::Remote),
        (&["sh", "-c", "\"ss\\\nh\" host"], Client::Remote),
        // Fish's keywords.
        (&["fish", "-c", "not ssh attach; true"], Client::Remote),
        (&["fish", "-c", "true; and ssh host"], Client::Remote),
        (&["fish", "-c", "begin; ssh; end"], Client::Remote),
    ] {
        assert_eq!(argv_client(&argv(words), 0), Some(client), "{words:?}");
    }
    for words in [
        &["sh", "-c", "echo tmux"][..],
        &["sh", "-c", "echo \"ssh host\"; vim notes"],
        // A wrapper's arguments are not commands, nor is a program a
        // command query names, nor what a single-quoted `$(…)` holds.
        &["env", "less", "/tmp/ssh"],
        &["sh", "-c", "command -v ssh; exec sh"],
        &["sh", "-c", "echo \"$(date)\" '$(ssh h)'"],
        &["sudo", "-u", "me", "vim", "tmux.conf"],
        &["bash", "script.sh"],
        &["sh", "-c"],
        &["env"],
        &["env", "FOO=1"],
    ] {
        assert_eq!(argv_client(&argv(words), 0), None, "{words:?}");
    }
    // A script past the parser's bounds fails closed.
    let many = format!("{}tmux attach", ": ; ".repeat(16));
    assert_eq!(
        argv_client(&argv(&["sh", "-c", &many]), 0),
        Some(Client::Remote)
    );
    let long = format!("{} ; tmux", "x".repeat(5000));
    assert_eq!(
        argv_client(&argv(&["sh", "-c", &long]), 0),
        Some(Client::Remote)
    );
    // Nesting is bounded, and too deep fails closed.
    let mut deep = vec!["vim".to_string()];
    for _ in 0..6 {
        deep.insert(0, "nohup".to_string());
    }
    assert_eq!(argv_client(&deep, 0), Some(Client::Remote));
}

/// A remote session or client makes a pane remote; anything else,
/// tmux included, is local. What runs now decides: a launch script
/// counts only while it is the foreground program.
#[test]
fn a_pane_path_origin_combines_its_command_lines() {
    let shell = argv(&["zsh"]);
    let tmux = argv(&["tmux"]);
    let ssh = argv(&["ssh", "h"]);
    let script = argv(&["sh", "-c", "ssh host; exec bash"]);
    let bash = argv(&["bash"]);
    let origin = |remote: bool, launch: &Vec<String>, foreground: Option<&Vec<String>>| {
        path_origin(remote, launch, foreground.map(|argv| argv.as_slice()))
    };
    assert_eq!(origin(false, &shell, None), PathOrigin::Local);
    assert_eq!(origin(false, &shell, Some(&tmux)), PathOrigin::Local);
    assert_eq!(origin(false, &tmux, Some(&ssh)), PathOrigin::Remote);
    assert_eq!(
        origin(true, &shell, None),
        PathOrigin::Remote,
        "a detected session"
    );
    assert_eq!(
        origin(false, &ssh, None),
        PathOrigin::Remote,
        "launched as a client"
    );
    assert_eq!(
        origin(false, &script, Some(&script)),
        PathOrigin::Remote,
        "the launch script is running"
    );
    assert_eq!(
        origin(false, &script, Some(&bash)),
        PathOrigin::Local,
        "the launch script has moved on"
    );
    let wrapped = argv(&["env", "FOO=1", "sh", "-c", "ssh host; exec bash"]);
    assert_eq!(
        origin(false, &wrapped, Some(&bash)),
        PathOrigin::Local,
        "a wrapped launch script has moved on too"
    );
    let wrapped_client = argv(&["env", "TERM=xterm", "ssh", "host"]);
    assert_eq!(origin(false, &wrapped_client, None), PathOrigin::Remote);
}

/// Only a local file link is gated: from a local pane, tmux included, it
/// opens, and from a remote pane or one that has gone it is refused. Web
/// and mail links open wherever they were printed.
#[test]
fn file_links_are_gated_by_their_pane() {
    let file = "file:///home/me/a.pdf";
    assert_eq!(link_gate(file, Some(PathOrigin::Local)), LinkGate::Open);
    assert_eq!(link_gate(file, Some(PathOrigin::Remote)), LinkGate::Refuse);
    assert_eq!(link_gate(file, None), LinkGate::Refuse);
    assert_eq!(
        link_gate("FILE://localhost/etc/hosts", Some(PathOrigin::Remote)),
        LinkGate::Refuse
    );
    for web in [
        "https://x.test/a",
        "mailto:a@b.test",
        "http://localhost:3000/",
    ] {
        assert_eq!(link_gate(web, Some(PathOrigin::Remote)), LinkGate::Open);
        assert_eq!(link_gate(web, None), LinkGate::Open);
    }
    assert_eq!(
        link_display_name("file:///home/me/a%20b.pdf", 48),
        "a b.pdf"
    );
    assert_eq!(link_display_name("file:///home/me/proj/", 48), "proj");
    // Untrusted names cannot reorder the prompt or push its warning away.
    assert_eq!(
        link_display_name("file:///home/me/invoice%E2%80%AEfdp.exe", 48),
        "invoice\u{FFFD}fdp.exe"
    );
    assert_eq!(
        link_display_name("file:///tmp/a%E2%80%8Bb.pdf", 48),
        "a\u{FFFD}b.pdf"
    );
    let width = |text: &str| unicode_width::UnicodeWidthStr::width(text);
    let long = link_display_name(&format!("file:///tmp/{}-end.pdf", "x".repeat(200)), 25);
    assert_eq!(width(&long), 25);
    assert!(long.starts_with("xxxx") && long.ends_with("x-end.pdf") && long.contains('…'));
    // Wide characters count as two columns.
    let wide = link_display_name(
        "file:///tmp/%E6%97%A5%E6%9C%AC%E8%AA%9E%E3%81%AE%E5%A0%B1%E5%91%8A%E6%9B%B8%E3%81%A7%E3%81%99.pdf",
        13,
    );
    assert!(width(&wide) <= 13, "{wide}");
    let undecodable = link_display_name(&format!("file:///{}", "%zz".repeat(40)), 25);
    assert_eq!(width(&undecodable), 25);
}

/// Every link taken from terminal output goes through `open_pane_link`:
/// `open_url` is called only from there and for Kettle's own release
/// page.
#[test]
fn terminal_links_open_through_their_pane() {
    let src = production_source();
    let callers = src
        .match_indices("self.open_url(ws, ")
        .map(|(at, _)| {
            let before = &src[..at];
            let function = before.rfind("    fn ").expect("enclosing fn");
            before[function + 7..]
                .split('(')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect::<Vec<_>>();
    for caller in &callers {
        assert!(
            [
                "open_pane_link",
                "open_update_release",
                "dispatch_confirm_action_arms"
            ]
            .contains(&caller.as_str()),
            "open_url called from {caller}"
        );
    }
    // A Lua handler's rewritten target meets the same pane gate.
    let open_url = src
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("open_url");
    let safe = open_url
        .find("if !kettle_core::links::is_safe_url(target)")
        .expect("the rewrite safety check");
    let gated = open_url
        .find(
            "&& target != uri\n                    && link_gate(target, origin) != LinkGate::Open",
        )
        .expect("the rewrite pane gate, for a link other than the one gated");
    let opened = open_url
        .find("rewritten = target.clone();")
        .expect("the rewrite");
    assert!(safe < gated && gated < opened);
    // A cwd becomes a URL by encoding, never by formatting it into one: a
    // space or `#` in the name would fail the check or name another folder.
    assert!(!src.contains(&["format!(\"file://", "{cwd}\")"].concat()));
    assert!(src.contains("kettle_core::links::file_url_for_path(std::path::Path::new(&cwd))"));
    // The right-click menu captures the link's pane with its address.
    assert!(src.contains("items.extend(link_menu_rows(&url, links_pane(ws), tr));"));
    for site in [
        "self.open_pane_link(ws, links_pane(ws), &uri);",
        "UrlHow::Open => self.open_pane_link(ws, pane, &url),",
        "self.open_pane_link(ws, Some(h.pane), text)",
        "self.open_pane_link(ws, Some(h.pane), &url)",
        "Some(url) => self.open_pane_link(ws, pane, &url),",
    ] {
        assert!(src.contains(site), "missing {site}");
    }
    // A preview passes the same gate, from the menu row and quick select,
    // whose labels `preview_link` limits to what a lane can show.
    for site in [
        "self.preview_pane_link(ws, pane, &url);",
        "found.retain(|(_, _, what)| hint_previews(what));",
        "if std::mem::take(&mut ws.hint_preview) {\n            if !hint_previews(&h.what) {\n                return;\n            }",
        "Some(url) => self.preview_pane_link(ws, h.pane, &url),",
        "match link_gate(uri, origin) {\n            LinkGate::Open => self.pull_preview(ws, pane, &path, None),",
    ] {
        assert!(src.contains(site), "missing {site}");
    }
    // The purpose of a quick select is set only with the targets it
    // chose: an empty scan leaves an open one as it was.
    assert_eq!(src.matches("ws.hint_preview = ").count(), 1);
    assert!(src.contains(
        "ws.hint_state = Some((targets, String::new()));\n                    ws.hint_preview = preview;"
    ));
    // Nothing a control client drives reads a file on the GUI's word.
    assert!(src.contains(
        "self.ctl_driving = true;\n                    self.handle_ctl_request(ws, event_loop, conn_id, *request, reply);\n                    self.ctl_driving = false;"
    ));
    let pull = src
        .split("fn pull_preview(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("pull_preview");
    let refused = pull
        .find("if self.ctl_driving {")
        .expect("the control refusal");
    let witness = pull
        .find("GuiActionWitness::from_explicit_gui_action()")
        .expect("the witness");
    assert!(refused < witness);
    assert_eq!(
        src.matches("GuiActionWitness::from_explicit_gui_action()")
            .count(),
        1,
        "pull_preview is the one place the witness is made"
    );
    // A lane is measured in its pane's own tab, which need not be showing.
    let show = src
        .split("fn show_media_item(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("show_media_item");
    assert!(show.contains(".partition_of(pane, self.area(ws), self.layout_style(ws))"));
    assert!(!show.contains("self.pane_partition("));
}

/// On Windows a path hint is copied, never opened: resolving it could
/// follow a link to a network share and authenticate to that host.
#[cfg(windows)]
#[test]
fn a_windows_path_hint_never_opens() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("out").join("diagram.png");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"x").unwrap();
    let text = file.to_str().unwrap().replace('\\', "/");
    assert_eq!(hint_path_url(&text, None, None), None);
    assert_eq!(
        hint_path_url("out/diagram.png", root.path().to_str(), None),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_path_hint_resolves_to_a_safe_local_file_url() {
    let url = |text: &str| hint_path_url(text, Some("/home/me/proj"), Some("/home/me"));
    for (text, expected) in [
        ("out/diagram.png", "file:///home/me/proj/out/diagram.png"),
        ("./out/diagram.png", "file:///home/me/proj/out/diagram.png"),
        ("~/notes/today.md", "file:///home/me/notes/today.md"),
        ("/etc/hosts", "file:///etc/hosts"),
        (
            "out/my diagram#1.png",
            "file:///home/me/proj/out/my%20diagram%231.png",
        ),
        (
            "café/menú.png",
            "file:///home/me/proj/caf%C3%A9/men%C3%BA.png",
        ),
    ] {
        assert_eq!(url(text).as_deref(), Some(expected), "{text}");
        assert!(
            kettle_core::links::is_safe_url(expected),
            "{expected} must pass the open check"
        );
    }
    // Copied, not opened: a climb whose target only the filesystem knows,
    // a base that is unknown or not local, a drive path off Windows.
    assert_eq!(url("../shared/report.pdf"), None);
    assert_eq!(url("assets/../report.pdf"), None);
    assert_eq!(hint_path_url("out/a.png", None, Some("/home/me")), None);
    assert_eq!(hint_path_url("~/a.png", Some("/x"), None), None);
    assert_eq!(
        hint_path_url("out/a.png", Some("//evil.example/share"), None),
        None
    );
    assert_eq!(hint_path_url("out/a.png", Some("relative/cwd"), None), None);
    assert_eq!(url("C:/out/diagram.png"), None);
}

/// The path and an OSC 7 directory are untrusted terminal text: turning
/// them into a URL must not stat anything, or a `/net/host/…` path would
/// mount a share chosen by whatever printed it, from the UI thread.
#[test]
fn resolving_a_path_hint_touches_no_filesystem() {
    let src = production_source();
    let body = src
        .split("fn hint_path_url(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("hint_path_url");
    for call in [
        "fs::",
        "canonicalize",
        ".exists(",
        ".is_file(",
        ".is_dir(",
        "metadata",
    ] {
        assert!(!body.contains(call), "hint_path_url must not call {call}");
    }
    // Off Windows, where no path hint opens, an absolute path becomes a
    // URL without being looked at.
    #[cfg(unix)]
    assert_eq!(
        hint_path_url("/net/evil.example/share/x.pdf", None, None).as_deref(),
        Some("file:///net/evil.example/share/x.pdf")
    );
}
