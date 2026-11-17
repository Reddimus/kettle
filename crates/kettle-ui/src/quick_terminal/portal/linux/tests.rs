use super::*;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use zbus::zvariant::ObjectPath;

const SESSION_PATH: &str = "/org/freedesktop/portal/desktop/session/test/session";
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    response: AtomicU32,
    create_response: AtomicU32,
    no_identity: std::sync::atomic::AtomicBool,
    omit_shortcut: std::sync::atomic::AtomicBool,
    pending: std::sync::atomic::AtomicBool,
    binds: AtomicUsize,
    closes: AtomicUsize,
    configure_error: AtomicU32,
    preference: Mutex<Option<String>>,
}
struct BusOwner;
#[zbus::interface(name = "org.freedesktop.DBus")]
impl BusOwner {
    fn get_name_owner(&self, _name: &str) -> &str {
        ":1.99"
    }
}

struct Registry(Arc<Fake>);
#[zbus::interface(name = "org.freedesktop.host.portal.Registry")]
impl Registry {
    fn register(&self, app: &str, _options: Dict) {
        assert_eq!(app, "kettle");
        self.0.calls.lock().unwrap().push("Register".into());
    }
}
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.portal.Error")]
enum CreateError {
    NotAllowed(String),
}

struct Portal(Arc<Fake>);
#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
    async fn create_session(
        &self,
        options: Dict,
        #[zbus(connection)] connection: &Connection,
    ) -> Result<OwnedObjectPath, CreateError> {
        self.0.calls.lock().unwrap().push("CreateSession".into());
        if self.0.no_identity.load(Ordering::SeqCst) {
            return Err(CreateError::NotAllowed("An app id is required".into()));
        }
        assert!(options.contains_key("session_handle_token"));
        let path = format!("{ROOT}/request/test/returned_create");
        respond(
            connection,
            &path,
            self.0.create_response.load(Ordering::SeqCst),
            Dict::from([("session_handle".into(), text(SESSION_PATH))]),
        )
        .await;
        Ok(OwnedObjectPath::try_from(path).unwrap())
    }
    async fn bind_shortcuts(
        &self,
        session: ObjectPath<'_>,
        shortcuts: Shortcuts,
        parent: &str,
        _options: Dict,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(session.as_str(), SESSION_PATH);
        assert!(parent.is_empty());
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].0, SHORTCUT);
        assert_eq!(
            shortcuts[0]
                .1
                .get("description")
                .and_then(|v| <&str>::try_from(v).ok()),
            Some("Toggle quick terminal")
        );
        *self.0.preference.lock().unwrap() = shortcuts[0]
            .1
            .get("preferred_trigger")
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::to_owned);
        self.0.binds.fetch_add(1, Ordering::SeqCst);
        let path = format!("{ROOT}/request/test/returned_bind");
        if !self.0.pending.load(Ordering::SeqCst) {
            let response = self.0.response.load(Ordering::SeqCst);
            respond(
                connection,
                &path,
                response,
                if self.0.omit_shortcut.load(Ordering::SeqCst) {
                    Dict::from([(
                        "shortcuts".into(),
                        OwnedValue::try_from(Value::from(Shortcuts::new())).unwrap(),
                    )])
                } else {
                    shortcut_result(Some("Press <Control>grave"))
                },
            )
            .await;
        }
        Ok(OwnedObjectPath::try_from(path).unwrap())
    }
    fn configure_shortcuts(
        &self,
        _session: ObjectPath<'_>,
        _parent: &str,
        _options: Dict,
    ) -> zbus::fdo::Result<()> {
        self.0.calls.lock().unwrap().push("Configure".into());
        match self.0.configure_error.load(Ordering::SeqCst) {
            1 => Err(zbus::fdo::Error::UnknownMethod("unsupported".into())),
            2 => Err(zbus::fdo::Error::Failed("transient".into())),
            _ => Ok(()),
        }
    }
}
struct Session(Arc<Fake>);
#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl Session {
    fn close(&self) {
        self.0.closes.fetch_add(1, Ordering::SeqCst);
    }
}
struct Request(Arc<Fake>);
#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl Request {
    fn close(&self) {
        self.0.calls.lock().unwrap().push("Request.Close".into());
    }
}

fn shortcut_result(description: Option<&str>) -> Dict {
    let props = description
        .map(|s| Dict::from([("trigger_description".into(), text(s))]))
        .unwrap_or_default();
    let value = Value::from(vec![(SHORTCUT.to_string(), props)]);
    Dict::from([("shortcuts".into(), OwnedValue::try_from(value).unwrap())])
}
async fn respond(connection: &Connection, path: &str, response: u32, results: Dict) {
    let message = Message::signal(path, REQUEST, "Response")
        .unwrap()
        .sender(":1.99")
        .unwrap()
        .build(&(response, results))
        .unwrap();
    connection.send(&message).await.unwrap();
}

async fn signal(connection: &Connection, member: &str, session: &str, token: Option<&str>) {
    let options = token
        .map(|t| Dict::from([("activation_token".into(), text(t))]))
        .unwrap_or_default();
    let message = Message::signal(ROOT, GS, member)
        .unwrap()
        .sender(":1.99")
        .unwrap()
        .build(&(
            ObjectPath::try_from(session).unwrap(),
            SHORTCUT,
            1u64,
            options,
        ))
        .unwrap();
    connection.send(&message).await.unwrap();
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < until, "portal test timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Default)]
struct MemoryStore(Mutex<Vec<u8>>);
impl PromptStore for MemoryStore {
    fn record(&self, desktop: &str, trigger: Option<Trigger>) {
        *self.0.lock().unwrap() = serde_json::to_vec(&Record {
            version: 1,
            desktop: desktop.into(),
            chord: trigger.map(|t| t.label()),
        })
        .unwrap();
    }
    fn suppressed(&self, desktop: &str, trigger: Trigger) -> bool {
        suppressed_bytes(&self.0.lock().unwrap(), desktop, trigger)
    }
}

struct Harness {
    watch: Option<Watch>,
    server: Connection,
    thread: Option<std::thread::JoinHandle<zbus::Result<()>>>,
    fake: Arc<Fake>,
    wakes: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
    record: Arc<MemoryStore>,
}
impl Harness {
    fn start(fake: Arc<Fake>, missing: bool) -> Self {
        Self::start_mode(fake, missing, true)
    }
    fn start_mode(fake: Arc<Fake>, missing: bool, peer: bool) -> Self {
        Self::start_with_gate(fake, missing, peer, None)
    }
    fn start_with_gate(
        fake: Arc<Fake>,
        missing: bool,
        peer: bool,
        gate: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("kettle-portal-test-")
            .tempdir_in(".")
            .unwrap();
        let record = Arc::new(MemoryStore::default());
        let (a, b) = zbus::connection::socket::Channel::pair();
        let guid = zbus::Guid::generate();
        let (server, client) = future::block_on(async {
            let mut server = Builder::authenticated_socket(a, guid.clone())
                .unwrap()
                .p2p()
                .serve_at("/org/freedesktop/DBus", BusOwner)
                .unwrap()
                .serve_at(ROOT, Registry(fake.clone()))
                .unwrap()
                .serve_at(SESSION_PATH, Session(fake.clone()))
                .unwrap()
                .serve_at(
                    format!("{ROOT}/request/test/returned_bind"),
                    Request(fake.clone()),
                )
                .unwrap();
            if !missing {
                server = server.serve_at(ROOT, Portal(fake.clone())).unwrap();
            }
            let (server, client) = future::zip(
                server.build(),
                Builder::authenticated_socket(b, guid)
                    .unwrap()
                    .p2p()
                    .method_timeout(TIMEOUT)
                    .build(),
            )
            .await;
            (server.unwrap(), client.unwrap())
        });
        let (watch, receive) = Watch::channel("Toggle quick terminal".into());
        watch.set_trigger(Some(Trigger::new(
            kettle_config::keybinds::Mods::CTRL,
            kettle_config::keybinds::Key::Char('`'),
        )));
        let shared = watch.shared.clone();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let mut config = AdapterConfig::new(record.clone(), "GNOME".into(), peer);
        config.enqueue_gate = gate;
        let thread = std::thread::spawn(move || {
            future::block_on(async {
                let wake: kettle_core::Waker = Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                });
                let result = connected(&client, &shared, &receive, &wake, &config).await;
                if result.is_err() {
                    publish(&shared, Status::Unavailable, &wake);
                }
                result
            })
        });
        Self {
            watch: Some(watch),
            server,
            thread: Some(thread),
            fake,
            wakes,
            _dir: dir,
            record,
        }
    }
    fn watch(&self) -> &Watch {
        self.watch.as_ref().unwrap()
    }
    fn registered(&self) {
        wait(|| matches!(self.watch().status(), Status::Registered { .. }));
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.watch.take();
        if let Some(thread) = self.thread.take() {
            wait(|| thread.is_finished());
            let _ = thread.join().unwrap();
        }
    }
}

#[test]
fn portal_fake_create_bind_activate_deactivate_and_tokenless_focus() {
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    assert_eq!(
        harness.fake.calls.lock().unwrap()[..2],
        ["Register", "CreateSession"]
    );
    assert_eq!(
        harness.fake.preference.lock().unwrap().as_deref(),
        Some("CTRL+grave")
    );
    future::block_on(signal(
        &harness.server,
        "Activated",
        &format!("{ROOT}/session/foreign"),
        Some("foreign"),
    ));
    future::block_on(signal(
        &harness.server,
        "Activated",
        SESSION_PATH,
        Some("secret"),
    ));
    wait(|| {
        !harness
            .watch()
            .shared
            .lock()
            .unwrap()
            .activations
            .is_empty()
    });
    future::block_on(signal(
        &harness.server,
        "Activated",
        SESSION_PATH,
        Some("repeat"),
    ));
    future::block_on(signal(&harness.server, "Deactivated", SESSION_PATH, None));
    future::block_on(signal(&harness.server, "Activated", SESSION_PATH, None));
    wait(|| harness.watch().shared.lock().unwrap().activations.len() == 2);
    assert_eq!(harness.watch().drain(), vec![Some("secret".into()), None]);
    assert!(matches!(
        harness.watch().status(),
        Status::Registered {
            focus_dependent: true,
            ..
        }
    ));
    assert!(harness.wakes.load(Ordering::SeqCst) >= 3);
}

#[test]
fn portal_fake_refuse_persists_reversible_desktop_scoped_suppression() {
    let fake = Arc::new(Fake::default());
    fake.response.store(2, Ordering::SeqCst);
    let harness = Harness::start(fake, false);
    wait(|| harness.watch().status() == Status::Suppressed);
    let trigger = desired(&harness.watch().shared).trigger.unwrap();
    assert!(harness.record.suppressed("GNOME", trigger));
    assert!(!harness.record.suppressed("KDE", trigger));
    harness.record.record("GNOME", None);
    assert!(!harness.record.suppressed("GNOME", trigger));
}

#[test]
fn portal_fake_missing_interface_fails_without_session_bus() {
    let harness = Harness::start(Arc::new(Fake::default()), true);
    wait(|| harness.watch().status() == Status::Unavailable);
    assert_eq!(harness.fake.binds.load(Ordering::SeqCst), 0);
}

#[test]
fn portal_fake_closed_and_stale_signals_cannot_leave_a_live_registration() {
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    future::block_on(signal(
        &harness.server,
        "Activated",
        &format!("{ROOT}/session/foreign"),
        Some("stale"),
    ));
    future::block_on(harness.server.emit_signal(
        None::<&str>,
        SESSION_PATH,
        SESSION,
        "Closed",
        &(Dict::new(),),
    ))
    .unwrap();
    wait(|| harness.watch().status() == Status::Unavailable);
    assert!(harness.watch().drain().is_empty());
}

#[test]
fn portal_fake_disable_discards_queued_activation_and_cancels_pending_without_response() {
    let fake = Arc::new(Fake::default());
    fake.pending.store(true, Ordering::SeqCst);
    let harness = Harness::start(fake, false);
    wait(|| harness.fake.binds.load(Ordering::SeqCst) == 1);
    harness.watch().set_trigger(None);
    wait(|| {
        harness
            .fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "Request.Close")
    });
    wait(|| harness.fake.closes.load(Ordering::SeqCst) == 1);
    assert!(harness.watch().drain().is_empty());
}

#[test]
fn portal_fake_preference_change_does_not_abandon_dialog_or_replace_binding() {
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    harness.watch().set_trigger(Some(Trigger::new(
        kettle_config::keybinds::Mods::ALT,
        kettle_config::keybinds::Key::F(12),
    )));
    harness.watch().configure();
    wait(|| {
        harness
            .fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "Configure")
    });
    assert_eq!(harness.fake.binds.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fake.closes.load(Ordering::SeqCst), 0);
}

#[test]
fn portal_fake_transient_configure_error_preserves_capability() {
    let fake = Arc::new(Fake::default());
    fake.configure_error.store(2, Ordering::SeqCst);
    let harness = Harness::start(fake, false);
    harness.registered();
    harness.watch().configure();
    wait(|| {
        harness
            .fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "Configure")
    });
    harness.watch().configure();
    wait(|| {
        harness
            .fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.as_str() == "Configure")
            .count()
            >= 2
    });
    harness.fake.configure_error.store(1, Ordering::SeqCst);
    assert!(matches!(
        harness.watch().status(),
        Status::Registered {
            configurable: true,
            ..
        }
    ));
    harness.watch().configure();
    wait(|| {
        matches!(
            harness.watch().status(),
            Status::Registered {
                configurable: false,
                ..
            }
        )
    });
}

#[test]
fn portal_inbox_disable_and_generation_change_reject_stale_tokens() {
    let (watch, _receive) = Watch::channel("Toggle quick terminal".into());
    watch.set_trigger(Some(Trigger::new(
        kettle_config::keybinds::Mods::CTRL,
        kettle_config::keybinds::Key::F(1),
    )));
    let generation = desired(&watch.shared).generation;
    watch
        .shared
        .lock()
        .unwrap()
        .activations
        .push_back(Activation {
            generation,
            token: Some("old".into()),
            received: Instant::now(),
        });
    watch.set_trigger(None);
    watch.set_trigger(Some(Trigger::new(
        kettle_config::keybinds::Mods::ALT,
        kettle_config::keybinds::Key::F(2),
    )));
    watch
        .shared
        .lock()
        .unwrap()
        .activations
        .push_back(Activation {
            generation,
            token: Some("late".into()),
            received: Instant::now(),
        });
    assert!(watch.drain().is_empty());
}

#[test]
#[ignore = "subprocess entry point; parent supplies a private runtime directory"]
fn portal_ownership_child() {
    use std::io::{Read, Write};
    let root = PathBuf::from(std::env::var_os("KETTLE_PORTAL_TEST_ROOT").unwrap());
    let build = std::env::var("KETTLE_PORTAL_TEST_BUILD").unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let mut waiting = false;
    // Exercise the shared production path and kernel lock in child processes.
    // Trusted-file validation belongs to kettle-state; this fixture's sandbox
    // maps root-owned ancestors to nobody, which that validator must reject.
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(ownership_path(&root))
        .unwrap();
    loop {
        match lease.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(error) => panic!("lock: {error}"),
        }
        if !waiting {
            println!("waiting {build}");
            std::io::stdout().flush().unwrap();
            waiting = true;
        }
        assert!(Instant::now() < until, "ownership takeover timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("owned {build}");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0u8]);
    drop(lease);
}

#[test]
fn portal_two_build_processes_share_one_owner_and_take_over_after_exit() {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::Builder::new()
        .prefix("kettle-portal-test-")
        .tempdir_in(".")
        .unwrap();
    let start = |build| {
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "quick_terminal::portal::linux::tests::portal_ownership_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env(
                    "KETTLE_PORTAL_TEST_ROOT",
                    dir.path()
                        .strip_prefix(std::env::current_dir().unwrap())
                        .unwrap_or(dir.path()),
                )
                .env("KETTLE_PORTAL_TEST_BUILD", build)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (send, receive) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if line.starts_with("owned ") || line.starts_with("waiting ") {
                    let _ = send.send(line);
                }
            }
        });
        (child, receive)
    };
    let (mut first, one) = start("build-a");
    assert_eq!(
        one.recv_timeout(Duration::from_secs(5)).unwrap(),
        "owned build-a"
    );
    let (mut second, two) = start("build-b");
    assert_eq!(
        two.recv_timeout(Duration::from_secs(5)).unwrap(),
        "waiting build-b"
    );
    first.0.stdin.take();
    assert!(first.0.wait().unwrap().success());
    assert_eq!(
        two.recv_timeout(Duration::from_secs(5)).unwrap(),
        "owned build-b"
    );
    second.0.stdin.take();
    assert!(second.0.wait().unwrap().success());
}

#[test]
fn portal_pending_chord_change_keeps_one_dialog_and_accepts_the_desktop_choice() {
    let fake = Arc::new(Fake::default());
    fake.pending.store(true, Ordering::SeqCst);
    let harness = Harness::start(fake, false);
    wait(|| harness.fake.binds.load(Ordering::SeqCst) == 1);
    harness.watch().set_trigger(Some(Trigger::new(
        kettle_config::keybinds::Mods::ALT,
        kettle_config::keybinds::Key::F(12),
    )));
    // A reply on the wrong handle must never complete the request.
    future::block_on(respond(
        &harness.server,
        &format!("{ROOT}/request/foreign"),
        0,
        shortcut_result(Some("foreign")),
    ));
    future::block_on(respond(
        &harness.server,
        &format!("{ROOT}/request/test/returned_bind"),
        0,
        shortcut_result(Some("desktop choice")),
    ));
    harness.registered();
    assert_eq!(
        harness.watch().status(),
        Status::Registered {
            trigger: Some("desktop choice".into()),
            configurable: true,
            focus_dependent: false
        }
    );
    assert_eq!(harness.fake.binds.load(Ordering::SeqCst), 1);
    assert!(
        !harness
            .fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "Request.Close")
    );
}

#[test]
fn portal_shortcuts_changed_keeps_unknown_description_and_checks_session() {
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    let emit = |session: &str, props: Dict| {
        future::block_on(harness.server.emit_signal(
            None::<&str>,
            ROOT,
            GS,
            "ShortcutsChanged",
            &(
                ObjectPath::try_from(session).unwrap(),
                vec![(SHORTCUT.to_string(), props)],
            ),
        ))
        .unwrap()
    };
    emit(
        &format!("{ROOT}/session/foreign"),
        Dict::from([("trigger_description".into(), text("foreign"))]),
    );
    future::block_on(harness.server.emit_signal(
        None::<&str>,
        ROOT,
        GS,
        "ShortcutsChanged",
        &(
            ObjectPath::try_from(SESSION_PATH).unwrap(),
            vec![("foreign_id".to_string(), Dict::new())],
        ),
    ))
    .unwrap();
    emit(SESSION_PATH, Dict::new());
    wait(|| {
        matches!(
            harness.watch().status(),
            Status::Registered { trigger: None, .. }
        )
    });
}

#[test]
fn portal_fake_no_app_id_does_not_claim_the_desktop_file_is_missing() {
    let fake = Arc::new(Fake::default());
    fake.no_identity.store(true, Ordering::SeqCst);
    let harness = Harness::start(fake, false);
    wait(|| harness.watch().status() == Status::NoAppId);
    assert_eq!(harness.fake.binds.load(Ordering::SeqCst), 0);
}

#[test]
fn portal_fake_cancellation_and_empty_success_are_prompt_suppression() {
    for response in [0, 1] {
        let fake = Arc::new(Fake::default());
        fake.response.store(response, Ordering::SeqCst);
        fake.omit_shortcut.store(true, Ordering::SeqCst);
        let harness = Harness::start(fake, false);
        wait(|| harness.watch().status() == Status::Suppressed);
        assert!(
            harness
                .record
                .suppressed("GNOME", desired(&harness.watch().shared).trigger.unwrap())
        );
    }
}

#[test]
fn portal_fake_removed_shortcut_and_oversized_response_invalidate_registration() {
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    future::block_on(harness.server.emit_signal(
        None::<&str>,
        ROOT,
        GS,
        "ShortcutsChanged",
        &(
            ObjectPath::try_from(SESSION_PATH).unwrap(),
            Shortcuts::new(),
        ),
    ))
    .unwrap();
    wait(|| harness.watch().status() == Status::Suppressed);
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    future::block_on(respond(
        &harness.server,
        &format!("{ROOT}/request/stale"),
        0,
        Dict::from([("large".into(), text("x".repeat(16385)))]),
    ));
    wait(|| harness.watch().status() == Status::Unavailable);
}

#[test]
fn portal_fake_create_failure_cannot_be_recorded_as_a_decline() {
    let fake = Arc::new(Fake::default());
    fake.create_response.store(2, Ordering::SeqCst);
    let harness = Harness::start(fake, false);
    wait(|| harness.watch().status() == Status::Unavailable);
    assert!(harness.record.0.lock().unwrap().is_empty());
}

#[test]
fn portal_settings_activation_forms_configure_and_delete_clears_without_capture() {
    use crate::quick_terminal::portal::{Action, settings_action};
    let harness = Harness::start(Arc::new(Fake::default()), false);
    harness.registered();
    for (index, source) in ["Enter", "Space", "pointer", "accessibility"]
        .into_iter()
        .enumerate()
    {
        assert!(settings_action(
            &harness.watch().status(),
            false,
            |action| {
                assert_eq!(action, Action::Configure, "{source}");
                harness.watch().configure();
            }
        ));
        wait(|| {
            harness
                .fake
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.as_str() == "Configure")
                .count()
                > index
        });
    }
    assert!(settings_action(&harness.watch().status(), true, |action| {
        assert_eq!(action, Action::Clear);
        harness.watch().set_trigger(None);
    }));
    wait(|| harness.fake.closes.load(Ordering::SeqCst) == 1);
    assert!(desired(&harness.watch().shared).trigger.is_none());
    assert!(!settings_action(&Status::Disabled, false, |_| panic!(
        "capture belongs to Settings"
    )));
}

#[test]
fn portal_fake_authenticates_sender_and_invalidates_on_owner_replacement() {
    let harness = Harness::start_mode(Arc::new(Fake::default()), false, false);
    harness.registered();
    let spoof = Message::signal(ROOT, GS, "Activated")
        .unwrap()
        .sender(":1.77")
        .unwrap()
        .build(&false)
        .unwrap();
    future::block_on(harness.server.send(&spoof)).unwrap();
    future::block_on(signal(
        &harness.server,
        "Activated",
        SESSION_PATH,
        Some("capability"),
    ));
    wait(|| harness.watch().shared.lock().unwrap().activations.len() == 1);
    let replacement = Message::signal(
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameOwnerChanged",
    )
    .unwrap()
    .sender("org.freedesktop.DBus")
    .unwrap()
    .build(&(DEST, ":1.99", ":1.100"))
    .unwrap();
    future::block_on(harness.server.send(&replacement)).unwrap();
    wait(|| harness.watch().status() == Status::Unavailable);
    assert!(harness.watch().drain().is_empty());
}

#[test]
fn portal_interrupted_first_bind_does_not_open_another_dialog_automatically() {
    let fake = Arc::new(Fake::default());
    fake.pending.store(true, Ordering::SeqCst);
    let harness = Harness::start_mode(fake, false, false);
    wait(|| harness.fake.binds.load(Ordering::SeqCst) == 1);
    let replacement = Message::signal(
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameOwnerChanged",
    )
    .unwrap()
    .sender("org.freedesktop.DBus")
    .unwrap()
    .build(&(DEST, ":1.99", ":1.100"))
    .unwrap();
    future::block_on(harness.server.send(&replacement)).unwrap();
    wait(|| harness.watch().status() == Status::Suppressed);
    assert!(
        harness
            .record
            .suppressed("GNOME", desired(&harness.watch().shared).trigger.unwrap())
    );
    assert_eq!(harness.fake.binds.load(Ordering::SeqCst), 1);
}

#[test]
fn portal_suppression_survives_failed_persistence_and_explicit_retry_clears_it() {
    let dir = crate::test_tempdir();
    // Both no storage and a directory that cannot be replaced as a file fail.
    for path in [None, Some(dir.path().to_path_buf())] {
        let store = FileStore::new(path);
        let trigger = Trigger::new(
            kettle_config::keybinds::Mods::CTRL,
            kettle_config::keybinds::Key::F(1),
        );
        store.record("GNOME", Some(trigger));
        assert!(store.suppressed("GNOME", trigger));
        assert!(!store.suppressed("KDE", trigger));
        store.record("GNOME", None);
        assert!(!store.suppressed("GNOME", trigger));
        store.record("GNOME", Some(trigger));
        assert!(store.suppressed("GNOME", trigger));
        assert!(!store.suppressed(
            "GNOME",
            Trigger::new(
                kettle_config::keybinds::Mods::ALT,
                kettle_config::keybinds::Key::F(2)
            )
        ));
    }
}

#[test]
fn worker_setup_runs_off_the_calling_thread() {
    let caller = std::thread::current().id();
    let (started, observed) = crossbeam_channel::bounded(1);
    let (release, released) = crossbeam_channel::bounded(1);
    let mut watch = Watch::start_with("test".into(), Arc::new(|| {}), move || {
        let thread = std::thread::current();
        let id = thread.id();
        started
            .send((id, thread.name().map(str::to_owned)))
            .unwrap();
        if id != caller {
            released.recv_timeout(Duration::from_secs(8)).unwrap();
        }
        WorkerSetup {
            root: PathBuf::new(),
            record: None,
            desktop: "test".into(),
        }
    })
    .unwrap();
    let actual = observed.recv_timeout(Duration::from_secs(8)).unwrap();
    let thread = watch._thread.take().unwrap();
    drop(watch);
    let _ = release.send(());
    wait(|| thread.is_finished());
    thread.join().unwrap();
    assert_ne!(
        actual.0, caller,
        "blocking setup ran on the event-loop caller"
    );
    assert_eq!(actual.1.as_deref(), Some("kettle-shortcuts"));
}

#[test]
fn waiting_worker_rereads_cancellation_after_acquiring_ownership() {
    let (watch, receive) = Watch::channel("test".into());
    let trigger = Trigger::new(
        kettle_config::keybinds::Mods::CTRL,
        kettle_config::keybinds::Key::F(1),
    );
    watch.set_trigger(Some(trigger));
    let store = Arc::new(MemoryStore::default());
    let (at_lock, reached) = crossbeam_channel::bounded(1);
    let (release, released) = crossbeam_channel::bounded(1);
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let worker = Worker {
        shared: watch.shared.clone(),
        receive,
        wake: Arc::new(|| {}),
        store: store.clone(),
        desktop: "GNOME".into(),
        peer: true,
    };
    let thread = std::thread::spawn(move || {
        future::block_on(worker.run(
            || {
                at_lock.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(8)).unwrap();
                Ok(Some(()))
            },
            || {
                count.fetch_add(1, Ordering::SeqCst);
                async {
                    Err(zbus::Error::Failure(
                        "test connection should not be opened".into(),
                    ))
                }
            },
        ))
    });
    reached.recv_timeout(Duration::from_secs(8)).unwrap(); // Waiter already read the old record.
    store.record("GNOME", Some(trigger)); // The previous owner cancels before releasing its lock.
    release.send(()).unwrap();
    wait(|| watch.status() == Status::Suppressed || connections.load(Ordering::SeqCst) > 0);
    let opened = connections.load(Ordering::SeqCst);
    drop(watch);
    wait(|| thread.is_finished());
    thread.join().unwrap();
    assert_eq!(
        opened, 0,
        "waiter opened the portal after the owner recorded cancellation"
    );
}

#[test]
fn cancelled_activation_cannot_borrow_the_replacement_generation() {
    let (before, arrived_before) = crossbeam_channel::bounded(1);
    let (resume_before, go_before) = crossbeam_channel::bounded(1);
    let (after, arrived_after) = crossbeam_channel::bounded(1);
    let (resume_after, go_after) = crossbeam_channel::bounded(1);
    let gate = Arc::new(move |before_lock| {
        if before_lock {
            before.send(()).unwrap();
            go_before.recv_timeout(Duration::from_secs(8)).unwrap();
        } else {
            after.send(()).unwrap();
            go_after.recv_timeout(Duration::from_secs(8)).unwrap();
        }
    });
    let harness = Harness::start_with_gate(Arc::new(Fake::default()), false, true, Some(gate));
    harness.registered();
    future::block_on(signal(
        &harness.server,
        "Activated",
        SESSION_PATH,
        Some("old session token"),
    ));
    arrived_before.recv_timeout(Duration::from_secs(8)).unwrap();
    let trigger = desired(&harness.watch().shared).trigger;
    harness.watch().set_trigger(None);
    harness.watch().set_trigger(trigger);
    resume_before.send(()).unwrap();
    arrived_after.recv_timeout(Duration::from_secs(8)).unwrap();
    let leaked = !harness
        .watch()
        .shared
        .lock()
        .unwrap()
        .activations
        .is_empty();
    resume_after.send(()).unwrap();
    assert!(
        !leaked,
        "old session activation was tagged with the replacement generation"
    );
}
