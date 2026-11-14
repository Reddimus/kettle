/// A store of `kind` under `dir` holding one small PNG, and that PNG's path.
fn exit_store_with_a_file(
    kind: crate::paste_image::StoreKind,
    dir: std::path::PathBuf,
) -> (crate::paste_image::PastedImages, std::path::PathBuf) {
    let mut store = crate::paste_image::PastedImages::of_kind_in(kind, dir);
    let red = [255u8, 0, 0, 255].repeat(4);
    let path = store.save_rgba(2, 2, &red, false).expect("save a test PNG");
    assert!(path.exists());
    (store, path)
}

/// The stores and session writer an exit teardown releases, each holding
/// something, under `root` with names starting `tag`.
struct ExitFixture {
    pasted: crate::paste_image::PastedImages,
    viewer: std::sync::Mutex<crate::paste_image::PastedImages>,
    documents: crate::media::PrivateCopies,
    videos: crate::media::PrivateCopies,
    png_encoder: Option<crate::paste_image::PngEncoder>,
    writer: crate::session::SessionWriter,
    files: Vec<std::path::PathBuf>,
    session: std::path::PathBuf,
    ctl: Option<crate::ctl_server::CtlServer>,
    /// What a client finds `ctl` by: its entry, its alias, its socket.
    ctl_files: Vec<std::path::PathBuf>,
    /// Where the server's start stood when the quit came.
    ctl_start: super::CtlStart,
}

impl ExitFixture {
    fn new(root: &std::path::Path, tag: &str) -> Self {
        use crate::paste_image::{OPENED, PASTED};
        let (pasted, screenshot) = exit_store_with_a_file(PASTED, root.join(format!("{tag}-pasted")));
        let (viewer, viewed) = exit_store_with_a_file(OPENED, root.join(format!("{tag}-viewer")));
        let (documents, document) = exit_store_with_a_file(PASTED, root.join(format!("{tag}-documents")));
        let (videos, video) = exit_store_with_a_file(PASTED, root.join(format!("{tag}-videos")));
        let session = root.join(format!("{tag}-session.json"));
        let destination = session.clone();
        // Slow enough that a teardown which did not wait for the write
        // would return before it lands.
        let writer = crate::session::SessionWriter::with_sink(move |snapshot, _| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            crate::session::save_to_path(&snapshot, &destination).unwrap();
        })
        .unwrap();
        let (ctl, ctl_files) = crate::ctl_server::start_private_for_tests(&root.join(format!("{tag}-ctl")));
        Self {
            pasted,
            viewer: std::sync::Mutex::new(viewer),
            documents: crate::media::PrivateCopies::in_store(documents),
            videos: crate::media::PrivateCopies::in_store(videos),
            png_encoder: None,
            writer,
            files: vec![screenshot, viewed, document, video],
            session,
            ctl: Some(ctl),
            ctl_files,
            ctl_start: super::CtlStart::Attempted,
        }
    }

    fn parts<'a>(&'a mut self, done: &'a mut bool) -> super::exit::ExitParts<'a> {
        super::exit::ExitParts {
            done,
            recorder: None,
            png_encoder: &mut self.png_encoder,
            pasted_images: &mut self.pasted,
            viewer_copies: &self.viewer,
            document_copies: &self.documents,
            video_copies: &self.videos,
            session_writer: &mut self.writer,
            ctl: &mut self.ctl,
            ctl_start: &mut self.ctl_start,
        }
    }
}

/// Quitting through AppKit's `terminate:` ends in `exit()`, so the
/// teardown is all there is: it withdraws the control server's registry
/// entry, alias and socket, deletes the pasted screenshots and the copies
/// handed to the image viewer, the document viewer and the video player,
/// all private screen or file content, and returns only once the session
/// write handed to the writer is on disk.
#[test]
fn the_exit_teardown_deletes_the_private_copies_and_waits_for_the_session_write() {
    let root = kettle_test_support::private_tempdir("kettle-exit-teardown-");
    let mut fixture = ExitFixture::new(root.path(), "only");
    fixture.writer.submit(
        crate::session::Session {
            active: 5,
            ..Default::default()
        },
        None,
    );
    let mut done = false;
    let mut flushed = 0;
    assert!(super::exit::run_exit_teardown(
        fixture.parts(&mut done),
        std::time::Duration::from_secs(10),
        || flushed += 1
    ));
    for file in &fixture.files {
        assert!(!file.exists(), "{} outlived the exit", file.display());
        assert!(
            !file.parent().unwrap().exists(),
            "{} outlived the exit",
            file.parent().unwrap().display()
        );
    }
    assert_eq!(
        crate::session::load_from_path(&fixture.session).map(|s| s.active),
        Some(5),
        "the teardown returned before the session write was on disk"
    );
    assert_eq!(flushed, 1);
    assert!(done);
    // Checked while the fixture lives: nothing here may wait for a drop.
    assert!(fixture.ctl.is_none(), "the control server outlived the exit");
    assert_eq!(fixture.ctl_start, super::CtlStart::Closed);
    for file in &fixture.ctl_files {
        assert!(!file.exists(), "{} outlived the exit", file.display());
    }
}

/// `exiting` runs the teardown and `run_app` returning runs it again on the
/// paths that close the last window; only the first may do anything. A
/// second run would flush notifications twice and, with stores reopened by
/// then, delete what the first left on purpose.
#[test]
fn the_exit_teardown_runs_once_when_both_exit_paths_call_it() {
    let root = kettle_test_support::private_tempdir("kettle-exit-once-");
    let mut first = ExitFixture::new(root.path(), "first");
    let mut second = ExitFixture::new(root.path(), "second");
    let mut done = false;
    let mut flushed = 0;
    let wait = std::time::Duration::from_secs(10);
    assert!(super::exit::run_exit_teardown(first.parts(&mut done), wait, || {
        flushed += 1
    }));
    assert!(!super::exit::run_exit_teardown(second.parts(&mut done), wait, || {
        flushed += 1
    }));
    assert_eq!(flushed, 1, "notifications were flushed twice");
    for file in second.files.iter().chain(&second.ctl_files) {
        assert!(file.exists(), "the second teardown deleted {}", file.display());
    }
    assert!(second.ctl.is_some(), "the second teardown took its server");
    assert_eq!(second.ctl_start, super::CtlStart::Attempted);
    // The second call must not have finished its writer either: a save
    // submitted now is still written.
    second.writer.submit(
        crate::session::Session {
            active: 9,
            ..Default::default()
        },
        None,
    );
    second.writer.finish();
    assert_eq!(
        crate::session::load_from_path(&second.session).map(|s| s.active),
        Some(9)
    );
}

/// The quit save goes through a window that still has tabs and is not the
/// quick terminal, or nowhere: an empty snapshot written on the way out would
/// replace the session the user means to restore.
#[test]
fn the_quit_save_needs_a_window_with_tabs_that_is_not_the_quick_terminal() {
    use super::exit::exit_save_window;
    let pick = |windows: &[(u64, bool)], quick| exit_save_window(false, false, windows.iter().copied(), quick);
    assert_eq!(pick(&[], None), None);
    assert_eq!(pick(&[(1, false), (2, false)], None), None);
    assert_eq!(pick(&[(1, true)], Some(1)), None);
    assert_eq!(pick(&[(1, true), (2, false)], Some(1)), None);
    assert_eq!(pick(&[(1, false), (2, true)], None), Some(2));
    assert_eq!(pick(&[(1, true), (2, true)], Some(1)), Some(2));
    // Once the teardown has run the writer is finished, and a failed startup
    // has nothing worth restoring: neither saves, whatever the windows hold.
    assert_eq!(exit_save_window(true, false, [(1, true)], None), None);
    assert_eq!(exit_save_window(false, true, [(1, true)], None), None);
}

/// Drift guard. `exiting` cannot be reached without a live event loop and
/// AppKit's `terminate:`, so pin at the source what it must do, in order,
/// and that `run_app` returning runs the same once-only teardown rather
/// than a copy of it.
#[test]
fn the_exiting_handler_saves_and_runs_the_exit_teardown() {
    let src = production_source();
    let exiting = src
        .split("fn exiting(&mut self, _event_loop: &ActiveEventLoop) {")
        .nth(1)
        .expect("App implements ApplicationHandler::exiting")
        .split("\n    }\n")
        .next()
        .unwrap();
    let steps = [
        "self.drain_recording_on_exit();",
        "self.save_session_on_exit();",
        "self.finish_exit();",
    ]
    .map(|step| {
        exiting
            .find(step)
            .unwrap_or_else(|| panic!("exiting must call {step}"))
    });
    assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{steps:?}");

    let after_run = src
        .split("event_loop.run_app(&mut app)")
        .nth(1)
        .expect("run_app call")
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(after_run.contains("app.finish_exit();"));
    // Assembled at runtime so this test's own text does not count.
    for inline in [
        ["pasted_images", ".cleanup()"].concat(),
        ["flush_desktop", "_notifications("].concat(),
    ] {
        assert!(
            !after_run.contains(inline.as_str()),
            "the cleanup after run_app is inlined again ({inline}); \
             it belongs to the once-only exit teardown"
        );
    }
    let call = ["run_exit", "_teardown("].concat();
    let finish = src
        .split("fn finish_exit(&mut self) {")
        .nth(1)
        .expect("finish_exit")
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(finish.contains(call.as_str()));
    assert!(finish.contains("done: &mut self.exit_teardown_done,"));
    assert!(
        finish.contains("ctl: &mut self.ctl,"),
        "the teardown must withdraw the control server"
    );
    assert!(
        finish.contains("ctl_start: &mut self.ctl_start,"),
        "the teardown must close the control server's start"
    );
    assert_eq!(
        src.matches(call.as_str()).count(),
        2,
        "run_exit_teardown is defined once and called only from finish_exit"
    );
}

/// A quit closes the control server's start for good. Afterwards neither
/// path that leads to a start reaches one: the first pane coming up, as
/// `resumed` runs it, or display being newly granted, as a reload runs it.
/// That holds wherever the start stood, including a quit that came before
/// the first pane; without the quit, each of those states does start one.
#[test]
fn nothing_starts_the_control_server_after_the_exit_teardown() {
    let policy = kettle_ctl::CtlPolicy::new(kettle_config::AgentServer::Full, true);
    let root = kettle_test_support::private_tempdir("kettle-exit-closed-");
    for (index, before) in [super::CtlStart::NotReady, super::CtlStart::Ready, super::CtlStart::Attempted]
        .into_iter()
        .enumerate()
    {
        let mut live = before;
        let mut started = false;
        live.first_pane_exists();
        live.display_granted();
        super::start_ctl_once(&mut live, &mut None, policy, || {
            started = true;
            None
        });
        assert!(started, "{before:?} reaches a start while Kettle runs");

        let mut fixture = ExitFixture::new(root.path(), &format!("closed-{index}"));
        fixture.ctl_start = before;
        if before == super::CtlStart::NotReady {
            // The quit came before the first pane: nothing had started.
            fixture.ctl = None;
        }
        let mut done = false;
        assert!(super::exit::run_exit_teardown(
            fixture.parts(&mut done),
            std::time::Duration::from_secs(10),
            || {}
        ));
        assert_eq!(fixture.ctl_start, super::CtlStart::Closed, "{before:?}");
        fixture.ctl_start.first_pane_exists();
        super::start_ctl_once(&mut fixture.ctl_start, &mut fixture.ctl, policy, || {
            panic!("the first pane started a server after the quit ({before:?})")
        });
        fixture.ctl_start.display_granted();
        super::start_ctl_once(&mut fixture.ctl_start, &mut fixture.ctl, policy, || {
            panic!("a display grant started a server after the quit ({before:?})")
        });
        assert_eq!(fixture.ctl_start, super::CtlStart::Closed, "{before:?}");
        assert!(fixture.ctl.is_none(), "{before:?}");
    }
}
