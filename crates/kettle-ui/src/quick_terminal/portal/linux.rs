//! Async zbus adapter; all bus and filesystem work belongs to its worker.
use super::{Machine, SHORTCUT, Status, preferred_trigger, sanitize, valid_token};
use futures_lite::{StreamExt, future};
use kettle_config::keybinds::Trigger;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, Message, MessageStream};

const DEST: &str = "org.freedesktop.portal.Desktop";
const ROOT: &str = "/org/freedesktop/portal/desktop";
const GS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const TIMEOUT: Duration = Duration::from_secs(2);
type Dict = HashMap<String, OwnedValue>;
type Shortcuts = Vec<(String, Dict)>;

#[derive(Clone)]
struct Desired {
    trigger: Option<Trigger>,
    generation: u64,
    retry: u64,
    configure: u64,
    cancel: u64,
    description: String,
}

// Tokens deliberately have no Debug implementation.
struct Activation {
    generation: u64,
    token: Option<String>,
    received: Instant,
}
struct Shared {
    desired: Desired,
    status: Status,
    activations: VecDeque<Activation>,
}

pub(crate) struct Watch {
    shared: Arc<Mutex<Shared>>,
    changed: Option<async_channel::Sender<()>>,
    _thread: Option<std::thread::JoinHandle<()>>,
}
impl Watch {
    pub(crate) fn start(description: String, wake: kettle_core::Waker) -> Option<Self> {
        Self::start_with(description, wake, WorkerSetup::discover)
    }

    fn start_with(
        description: String,
        wake: kettle_core::Waker,
        setup: impl FnOnce() -> WorkerSetup + Send + 'static,
    ) -> Option<Self> {
        let (mut watch, receive) = Self::channel(description);
        let shared = watch.shared.clone();
        let thread = std::thread::Builder::new()
            .name("kettle-shortcuts".into())
            .spawn(move || {
                let WorkerSetup {
                    root,
                    record,
                    desktop,
                } = setup();
                let worker = Worker {
                    shared,
                    receive,
                    wake,
                    store: Arc::new(FileStore::new(record)),
                    desktop,
                    peer: false,
                };
                future::block_on(worker.run(
                    || ownership(&root),
                    || async {
                        crate::dbus_bound::connect(
                            None,
                            crate::dbus_bound::Options {
                                max_queued: Some(16),
                                method_timeout: Some(TIMEOUT),
                            },
                        )
                        .await
                    },
                ));
            })
            .ok()?;
        watch._thread = Some(thread);
        Some(watch)
    }

    fn channel(description: String) -> (Self, async_channel::Receiver<()>) {
        let (send, receive) = async_channel::bounded(1);
        (
            Self {
                shared: Arc::new(Mutex::new(Shared {
                    desired: Desired {
                        trigger: None,
                        generation: 0,
                        retry: 0,
                        configure: 0,
                        cancel: 0,
                        description,
                    },
                    status: Status::Disabled,
                    activations: VecDeque::new(),
                })),
                changed: Some(send),
                _thread: None,
            },
            receive,
        )
    }

    pub(crate) fn set_trigger(&self, trigger: Option<Trigger>) {
        let mut state = self.shared.lock().unwrap();
        if state.desired.trigger != trigger {
            state.desired.trigger = trigger;
            state.desired.generation = state.desired.generation.wrapping_add(1);
            state.activations.clear();
            if trigger.is_none() {
                state.desired.cancel = state.desired.cancel.wrapping_add(1);
                state.status = Status::Disabled;
            }
        }
        drop(state);
        self.notify();
    }
    pub(crate) fn retry(&self) {
        let mut state = self.shared.lock().unwrap();
        state.desired.retry = state.desired.retry.wrapping_add(1);
        drop(state);
        self.notify();
    }
    pub(crate) fn configure(&self) {
        let mut state = self.shared.lock().unwrap();
        state.desired.configure = state.desired.configure.wrapping_add(1);
        drop(state);
        self.notify();
    }
    fn notify(&self) {
        if let Some(send) = &self.changed {
            let _ = send.try_send(());
        }
    }
    pub(crate) fn status(&self) -> Status {
        self.shared.lock().unwrap().status.clone()
    }
    pub(crate) fn drain(&self) -> Vec<Option<String>> {
        let mut state = self.shared.lock().unwrap();
        let generation = state.desired.generation;
        let enabled = state.desired.trigger.is_some();
        state
            .activations
            .drain(..)
            .filter(|a| enabled && a.generation == generation && a.received.elapsed() < TIMEOUT)
            .map(|a| a.token)
            .collect()
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.changed.take();
    }
}

struct WorkerSetup {
    root: PathBuf,
    record: Option<PathBuf>,
    desktop: String,
}
impl WorkerSetup {
    fn discover() -> Self {
        let root = kettle_ctl::discovery::canonical_registry_dir()
            .unwrap_or_else(kettle_ctl::discovery::registry_dir);
        let record = kettle_config::Config::default_path()
            .and_then(|p| p.parent().map(|p| p.join("quick-terminal-portal.json")));
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .chars()
            .take(128)
            .collect();
        Self {
            root,
            record,
            desktop,
        }
    }
}

fn desired(shared: &Mutex<Shared>) -> Desired {
    shared.lock().unwrap().desired.clone()
}
fn publish(shared: &Mutex<Shared>, status: Status, wake: &kettle_core::Waker) {
    let mut state = shared.lock().unwrap();
    if state.status != status {
        state.status = status;
        drop(state);
        wake();
    }
}

trait PromptStore: Send + Sync {
    fn suppressed(&self, desktop: &str, trigger: Trigger) -> bool;
    fn record(&self, desktop: &str, trigger: Option<Trigger>);
}
struct FileStore {
    path: Option<PathBuf>,
    unsaved: Mutex<Option<Record>>,
}
impl FileStore {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            unsaved: Mutex::new(None),
        }
    }
}
impl PromptStore for FileStore {
    fn suppressed(&self, desktop: &str, trigger: Trigger) -> bool {
        if let Some(record) = self.unsaved.lock().unwrap().as_ref() {
            return record.matches(desktop, trigger);
        }
        suppressed(self.path.as_deref(), desktop, trigger)
    }
    fn record(&self, desktop: &str, trigger: Option<Trigger>) {
        let saved = record(self.path.as_deref(), desktop, trigger);
        // Failed writes must neither reopen cancelled dialogs nor make explicit
        // retry depend on removing an unreadable or read-only durable record.
        *self.unsaved.lock().unwrap() = if saved {
            None
        } else {
            Some(Record {
                version: 1,
                desktop: desktop.into(),
                chord: trigger.map(|t| t.label()),
            })
        };
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    version: u8,
    desktop: String,
    chord: Option<String>,
}
impl Record {
    fn matches(&self, desktop: &str, trigger: Trigger) -> bool {
        self.version == 1
            && self.desktop == desktop
            && self.chord.as_deref() == Some(trigger.label().as_str())
    }
}
fn suppressed(path: Option<&Path>, desktop: &str, trigger: Trigger) -> bool {
    use std::io::Read;
    let Some(path) = path else { return false };
    let Ok(file) = kettle_state::open_trusted_file_read(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(1025).read_to_end(&mut bytes).is_err() || bytes.len() > 1024 {
        return false;
    }
    suppressed_bytes(&bytes, desktop, trigger)
}
fn suppressed_bytes(bytes: &[u8], desktop: &str, trigger: Trigger) -> bool {
    bytes.len() <= 1024
        && serde_json::from_slice::<Record>(bytes)
            .is_ok_and(|record| record.matches(desktop, trigger))
}
fn record(path: Option<&Path>, desktop: &str, trigger: Option<Trigger>) -> bool {
    let Some(path) = path else { return false };
    let value = Record {
        version: 1,
        desktop: desktop.into(),
        chord: trigger.map(|t| t.label()),
    };
    if let Ok(bytes) = serde_json::to_vec(&value)
        && kettle_state::atomic_replace(path, &bytes, kettle_state::AtomicWriteOptions::PRIVATE)
            .is_ok()
    {
        return true;
    }
    log::warn!("quick terminal shortcut: could not save prompt suppression");
    false
}

fn ownership_path(root: &Path) -> PathBuf {
    root.join("quick-terminal-portal.lock")
}
fn ownership(root: &Path) -> std::io::Result<Option<kettle_state::ExclusiveFileLock>> {
    kettle_state::create_private_dirs(root)?;
    // Never unlink: every build must contend on the same inode, including waiters.
    kettle_state::ExclusiveFileLock::try_acquire(&ownership_path(root))
}

async fn pause(receive: &async_channel::Receiver<()>, duration: Duration) {
    future::or(
        async {
            let _ = receive.recv().await;
        },
        async {
            async_io::Timer::after(duration).await;
        },
    )
    .await;
}
struct Worker {
    shared: Arc<Mutex<Shared>>,
    receive: async_channel::Receiver<()>,
    wake: kettle_core::Waker,
    store: Arc<dyn PromptStore>,
    desktop: String,
    peer: bool,
}
impl Worker {
    async fn run<L, A, C, F>(&self, mut acquire: A, mut connect: C)
    where
        A: FnMut() -> std::io::Result<Option<L>>,
        C: FnMut() -> F,
        F: std::future::Future<Output = zbus::Result<Connection>>,
    {
        let Self {
            shared,
            receive,
            wake,
            store,
            desktop,
            peer,
        } = self;
        let mut backoff = Duration::from_secs(1);
        let mut retry = 0;
        while !receive.is_closed() {
            let next = desired(shared);
            let Some(trigger) = next.trigger else {
                publish(shared, Status::Disabled, wake);
                pause(receive, Duration::from_secs(1)).await;
                continue;
            };
            if next.retry == retry && store.suppressed(desktop, trigger) {
                publish(shared, Status::Suppressed, wake);
                pause(receive, Duration::from_secs(1)).await;
                continue;
            }
            let lease = match acquire() {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    publish(shared, Status::OtherOwner, wake);
                    pause(receive, Duration::from_millis(250)).await;
                    continue;
                }
                Err(_) => {
                    publish(shared, Status::Failed, wake);
                    pause(receive, backoff).await;
                    continue;
                }
            };
            // A previous owner may have recorded cancellation while we waited.
            // Only the owner may clear that record for an explicit Retry.
            let next = desired(shared);
            let Some(trigger) = next.trigger.filter(|_| !receive.is_closed()) else {
                drop(lease);
                continue;
            };
            if next.retry != retry {
                retry = next.retry;
                store.record(desktop, None);
            }
            if store.suppressed(desktop, trigger) {
                publish(shared, Status::Suppressed, wake);
                drop(lease);
                pause(receive, Duration::from_secs(1)).await;
                continue;
            }
            publish(shared, Status::Connecting, wake);
            let connect = connect();
            let connection = future::or(connect, async {
                async_io::Timer::after(TIMEOUT).await;
                Err(zbus::Error::Failure("shortcut connection timed out".into()))
            })
            .await;
            match connection {
                Ok(connection) => {
                    let config = AdapterConfig::new(store.clone(), desktop.clone(), *peer);
                    if connected(&connection, shared, receive, wake, &config)
                        .await
                        .is_err()
                    {
                        publish(shared, Status::Unavailable, wake);
                    }
                    // Dropping the dedicated connection also closes all portal sessions.
                    let _ = connection.close().await;
                }
                Err(_) => publish(shared, Status::Unavailable, wake),
            }
            drop(lease);
            if desired(shared).trigger.is_some() {
                pause(receive, backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

struct AdapterConfig {
    store: Arc<dyn PromptStore>,
    desktop: String,
    peer: bool,
    #[cfg(test)]
    enqueue_gate: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}
impl AdapterConfig {
    fn new(store: Arc<dyn PromptStore>, desktop: String, peer: bool) -> Self {
        Self {
            store,
            desktop,
            peer,
            #[cfg(test)]
            enqueue_gate: None,
        }
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Create,
    Bind,
}
struct Pending {
    handle: String,
    kind: Kind,
    until: Option<Instant>,
}

fn text(value: impl Into<String>) -> OwnedValue {
    OwnedValue::try_from(Value::from(value.into())).unwrap()
}
fn bounded(message: &Message) -> zbus::Result<()> {
    if message.body().len() > 16 * 1024 {
        return Err(zbus::Error::Failure("shortcut message too large".into()));
    }
    Ok(())
}
async fn call(
    connection: &Connection,
    interface: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Result<Message> {
    let reply = connection
        .call_method(Some(DEST), ROOT, Some(interface), method, body)
        .await?;
    bounded(&reply)?;
    Ok(reply)
}
fn token() -> zbus::Result<String> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes)
        .map_err(|_| zbus::Error::Failure("shortcut entropy unavailable".into()))?;
    Ok(format!("kettle_{:016x}", u64::from_ne_bytes(bytes)))
}
async fn request(
    connection: &Connection,
    session: Option<&str>,
    trigger: Trigger,
    description: &str,
) -> zbus::Result<Pending> {
    let handle_token = token()?;
    let mut options = Dict::from([("handle_token".into(), text(handle_token))]);
    let (reply, kind) = if let Some(session) = session {
        let mut props = Dict::from([("description".into(), text(description))]);
        if let Some(trigger) = preferred_trigger(trigger) {
            props.insert("preferred_trigger".into(), text(trigger));
        }
        (
            call(
                connection,
                GS,
                "BindShortcuts",
                &(
                    OwnedObjectPath::try_from(session)?,
                    vec![(SHORTCUT, props)],
                    "",
                    options,
                ),
            )
            .await?,
            Kind::Bind,
        )
    } else {
        options.insert("session_handle_token".into(), text(token()?));
        (
            call(connection, GS, "CreateSession", &(options,)).await?,
            Kind::Create,
        )
    };
    let handle: OwnedObjectPath = reply.body().deserialize()?;
    if !handle.as_str().starts_with(&format!("{ROOT}/request/")) {
        return Err(zbus::Error::Failure("invalid request path".into()));
    }
    Ok(Pending {
        handle: handle.to_string(),
        kind,
        until: matches!(kind, Kind::Create).then(|| Instant::now() + TIMEOUT),
    })
}
async fn close(connection: &Connection, path: &str, interface: &str) {
    let _ = connection
        .call_method(Some(DEST), path, Some(interface), "Close", &())
        .await;
}
fn unsupported(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _) if matches!(name.as_str(), "org.freedesktop.DBus.Error.UnknownMethod" | "org.freedesktop.portal.Error.NotSupported" | "org.freedesktop.DBus.Error.NotSupported"))
}
fn no_identity(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.portal.Error.NotAllowed")
}
fn shortcut_trigger(shortcuts: Shortcuts) -> Option<Option<String>> {
    shortcuts
        .into_iter()
        .find(|(id, _)| id == SHORTCUT)
        .map(|(_, mut props)| {
            props
                .remove("trigger_description")
                .and_then(|v| String::try_from(v).ok())
                .as_deref()
                .and_then(sanitize)
        })
}

async fn connected(
    connection: &Connection,
    shared: &Mutex<Shared>,
    receive: &async_channel::Receiver<()>,
    wake: &kettle_core::Waker,
    config: &AdapterConfig,
) -> zbus::Result<()> {
    // Raw call before constructing property-caching proxies on this connection.
    if config.peer || (!Path::new("/.flatpak-info").exists() && std::env::var_os("SNAP").is_none())
    {
        let _ = call(
            connection,
            "org.freedesktop.host.portal.Registry",
            "Register",
            &("kettle", Dict::new()),
        )
        .await;
    }
    let mut signals = MessageStream::for_match_rule(
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path_namespace(ROOT)?
            .build(),
        connection,
        Some(16),
    )
    .await?;
    let mut owners = if config.peer {
        None
    } else {
        Some(
            MessageStream::for_match_rule(
                MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender("org.freedesktop.DBus")?
                    .interface("org.freedesktop.DBus")?
                    .member("NameOwnerChanged")?
                    .add_arg(DEST)?
                    .build(),
                connection,
                Some(4),
            )
            .await?,
        )
    };
    let owner: Option<String> = if config.peer {
        None
    } else {
        let reply = connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetNameOwner",
                &(DEST,),
            )
            .await?;
        bounded(&reply)?;
        Some(reply.body().deserialize()?)
    };
    let version: OwnedValue = call(
        connection,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(GS, "version"),
    )
    .await?
    .body()
    .deserialize()?;
    let mut configurable = u32::try_from(version)? >= 2;
    let initial = desired(shared);
    let Some(trigger) = initial.trigger else {
        return Ok(());
    };
    let mut pending = match request(connection, None, trigger, &initial.description).await {
        Ok(pending) => Some(pending),
        Err(error) if no_identity(&error) => {
            publish(shared, Status::NoAppId, wake);
            pause(receive, Duration::from_secs(30)).await;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut model = Machine::default();
    let mut dialog_started = false;
    let mut configure = initial.configure;
    let mut focus_dependent = config.desktop.to_ascii_lowercase().contains("kde");
    publish(shared, Status::Pending, wake);
    let result = async {
        loop {
            enum Event {
                Signal(Option<zbus::Result<Message>>),
                Owner,
                Change,
                Tick,
            }
            let event = future::or(
                async { Event::Signal(signals.next().await) },
                future::or(
                    async {
                        if let Some(owners) = &mut owners {
                            let _ = owners.next().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                        Event::Owner
                    },
                    future::or(
                        async {
                            let _ = receive.recv().await;
                            Event::Change
                        },
                        async {
                            async_io::Timer::after(Duration::from_millis(250)).await;
                            Event::Tick
                        },
                    ),
                ),
            )
            .await;
            let current = desired(shared);
            if receive.is_closed() || current.trigger.is_none() || current.cancel != initial.cancel
            {
                if let Some(pending) = pending.take() {
                    if matches!(pending.kind, Kind::Bind) {
                        config
                            .store
                            .record(&config.desktop, current.trigger.or(initial.trigger));
                    }
                    close(connection, &pending.handle, REQUEST).await;
                }
                dialog_started = false;
                publish(shared, Status::Disabled, wake);
                break Ok(());
            }
            match event {
                Event::Owner => break Err(zbus::Error::Failure("shortcut owner changed".into())),
                Event::Signal(None) => {
                    break Err(zbus::Error::Failure("shortcut connection closed".into()));
                }
                Event::Signal(Some(Err(error))) => break Err(error),
                Event::Signal(Some(Ok(message))) => {
                    let header = message.header();
                    if !config.peer && header.sender().map(|s| s.as_str()) != owner.as_deref() {
                        continue;
                    }
                    bounded(&message)?;
                    let path = header.path().map(|p| p.as_str()).unwrap_or("");
                    let interface = header.interface().map(|i| i.as_str()).unwrap_or("");
                    let member = header.member().map(|m| m.as_str()).unwrap_or("");
                    if interface == REQUEST && member == "Response" {
                        let Some(waiting) = pending.as_ref().filter(|p| p.handle == path) else {
                            continue;
                        };
                        let kind = waiting.kind;
                        let (response, mut results): (u32, Dict) = message.body().deserialize()?;
                        pending = None;
                        if response != 0 {
                            if matches!(kind, Kind::Create) {
                                break Err(zbus::Error::Failure("shortcut session refused".into()));
                            }
                            dialog_started = false;
                            // GNOME maps both cancellation and provider failure to 2.
                            config.store.record(&config.desktop, current.trigger);
                            publish(shared, Status::Suppressed, wake);
                            break Ok(());
                        }
                        match kind {
                            Kind::Create => {
                                let session = results
                                    .remove("session_handle")
                                    .and_then(|v| String::try_from(v).ok())
                                    .ok_or_else(|| {
                                        zbus::Error::Failure("missing session handle".into())
                                    })?;
                                if !session.starts_with(&format!("{ROOT}/session/")) {
                                    break Err(zbus::Error::Failure(
                                        "invalid session handle".into(),
                                    ));
                                }
                                model.session = Some(session.clone());
                                dialog_started = true;
                                pending = Some(
                                    request(
                                        connection,
                                        Some(&session),
                                        current.trigger.unwrap(),
                                        &current.description,
                                    )
                                    .await?,
                                );
                            }
                            Kind::Bind => {
                                dialog_started = false;
                                let shortcuts = results.remove("shortcuts").ok_or_else(|| {
                                    zbus::Error::Failure("missing shortcuts".into())
                                })?;
                                match shortcut_trigger(Shortcuts::try_from(shortcuts)?) {
                                    Some(trigger) => {
                                        config.store.record(&config.desktop, None);
                                        publish(
                                            shared,
                                            Status::Registered {
                                                trigger,
                                                configurable,
                                                focus_dependent,
                                            },
                                            wake,
                                        );
                                    }
                                    None => {
                                        config.store.record(&config.desktop, current.trigger);
                                        publish(shared, Status::Suppressed, wake);
                                        break Ok(());
                                    }
                                }
                            }
                        }
                    } else if interface == SESSION
                        && member == "Closed"
                        && model.session.as_deref() == Some(path)
                    {
                        model.close();
                        shared.lock().unwrap().activations.clear();
                        break Err(zbus::Error::Failure("shortcut session closed".into()));
                    } else if interface == GS && matches!(member, "Activated" | "Deactivated") {
                        let (session, id, _, mut options): (OwnedObjectPath, String, u64, Dict) =
                            message.body().deserialize()?;
                        if member == "Deactivated" {
                            model.deactivate(session.as_str(), &id);
                        } else if matches!(shared.lock().unwrap().status, Status::Registered { .. })
                            && model.activate(session.as_str(), &id)
                        {
                            let token = options
                                .remove("activation_token")
                                .and_then(|v| String::try_from(v).ok())
                                .filter(|t| valid_token(t));
                            if token.is_none() {
                                focus_dependent = true;
                            }
                            #[cfg(test)]
                            if let Some(gate) = &config.enqueue_gate {
                                gate(true);
                            }
                            let mut state = shared.lock().unwrap();
                            // Cancellation and enqueue share this lock: a signal
                            // already parsed by the old session cannot acquire
                            // a replacement session's generation.
                            if state.desired.cancel == initial.cancel
                                && state.desired.generation == current.generation
                                && state.desired.trigger.is_some()
                            {
                                if let Status::Registered {
                                    focus_dependent: focus,
                                    ..
                                } = &mut state.status
                                {
                                    *focus = focus_dependent;
                                }
                                if state.activations.len() < 8 {
                                    state.activations.push_back(Activation {
                                        generation: current.generation,
                                        token,
                                        received: Instant::now(),
                                    });
                                }
                            }
                            drop(state);
                            #[cfg(test)]
                            if let Some(gate) = &config.enqueue_gate {
                                gate(false);
                            }
                            wake();
                        }
                    } else if interface == GS && member == "ShortcutsChanged" {
                        let (session, shortcuts): (OwnedObjectPath, Shortcuts) =
                            message.body().deserialize()?;
                        if model.session.as_deref() == Some(session.as_str()) && pending.is_none() {
                            let empty = shortcuts.is_empty();
                            if let Some(trigger) = shortcut_trigger(shortcuts) {
                                publish(
                                    shared,
                                    Status::Registered {
                                        trigger,
                                        configurable,
                                        focus_dependent,
                                    },
                                    wake,
                                );
                            } else if empty {
                                config.store.record(&config.desktop, current.trigger);
                                publish(shared, Status::Suppressed, wake);
                                break Ok(());
                            }
                        }
                    }
                }
                Event::Change | Event::Tick => {
                    if pending
                        .as_ref()
                        .is_some_and(|p| p.until.is_some_and(|until| Instant::now() >= until))
                    {
                        break Err(zbus::Error::Failure("shortcut request timed out".into()));
                    }
                    if current.retry != initial.retry
                        && !matches!(shared.lock().unwrap().status, Status::Registered { .. })
                    {
                        break Ok(());
                    }
                    if current.configure != configure {
                        configure = current.configure;
                        if configurable
                            && pending.is_none()
                            && let Some(session) = &model.session
                        {
                            let result = call(
                                connection,
                                GS,
                                "ConfigureShortcuts",
                                &(
                                    OwnedObjectPath::try_from(session.as_str())?,
                                    "",
                                    Dict::new(),
                                ),
                            )
                            .await;
                            if let Err(error) = result
                                && unsupported(&error)
                            {
                                configurable = false;
                                let status = shared.lock().unwrap().status.clone();
                                if let Status::Registered { trigger, .. } = status {
                                    publish(
                                        shared,
                                        Status::Registered {
                                            trigger,
                                            configurable,
                                            focus_dependent,
                                        },
                                        wake,
                                    );
                                }
                                // Other errors leave the action retryable and preserve registration.
                            }
                        }
                    }
                    // A new nonempty preference never replaces a desktop-owned chord,
                    // or abandons an open GNOME provider dialog.
                }
            }
        }
    }
    .await;
    let uncertain_dialog = dialog_started && result.is_err();
    if uncertain_dialog {
        // Reconnecting an interrupted first bind can orphan a GNOME dialog and
        // open another. Only an explicit retry may start another offer.
        config
            .store
            .record(&config.desktop, desired(shared).trigger);
        publish(shared, Status::Suppressed, wake);
    }
    if let Some(pending) = pending {
        close(connection, &pending.handle, REQUEST).await;
    }
    if let Some(session) = &model.session {
        close(connection, session, SESSION).await;
    }
    model.close();
    shared.lock().unwrap().activations.clear();
    if uncertain_dialog { Ok(()) } else { result }
}

#[cfg(test)]
mod tests;
