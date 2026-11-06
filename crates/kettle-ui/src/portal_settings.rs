//! Bounded XDG Settings reads and subscriptions, owned by a background worker.
use futures_lite::{StreamExt, future};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use zbus::{Connection, Proxy, connection::Builder, zvariant::OwnedValue};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const UNKNOWN: u64 = u64::MAX;
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BODY_BYTES: usize = 4096;

pub(crate) trait SettingState: Send + Sync + 'static {
    const NAMESPACE: &'static str;
    fn initial() -> Self;
    fn update(&self, value: OwnedValue, wake: &kettle_core::Waker) -> zbus::Result<()>;
    fn clear(&self, wake: &kettle_core::Waker);
}

impl SettingState for AtomicU64 {
    const NAMESPACE: &'static str = NAMESPACE;
    fn initial() -> Self {
        Self::new(UNKNOWN)
    }
    fn update(&self, value: OwnedValue, wake: &kettle_core::Waker) -> zbus::Result<()> {
        publish(self, Some(u32::try_from(value)?), wake);
        Ok(())
    }
    fn clear(&self, wake: &kettle_core::Waker) {
        publish(self, None, wake);
    }
}

pub(crate) type ButtonWatch = Watch<Mutex<crate::window_buttons::ButtonLayout>>;

impl SettingState for Mutex<crate::window_buttons::ButtonLayout> {
    const NAMESPACE: &'static str = "org.gnome.desktop.wm.preferences";
    fn initial() -> Self {
        Self::new(Default::default())
    }
    fn update(&self, value: OwnedValue, wake: &kettle_core::Waker) -> zbus::Result<()> {
        let value = String::try_from(value)?;
        let layout = crate::window_buttons::ButtonLayout::parse(&value)
            .ok_or_else(|| zbus::Error::Failure("invalid button layout".into()))?;
        publish_buttons(self, layout, wake);
        Ok(())
    }
    fn clear(&self, wake: &kettle_core::Waker) {
        publish_buttons(self, Default::default(), wake);
    }
}

fn publish_buttons(
    state: &Mutex<crate::window_buttons::ButtonLayout>,
    layout: crate::window_buttons::ButtonLayout,
    wake: &kettle_core::Waker,
) {
    if let Ok(mut current) = state.lock()
        && *current != layout
    {
        *current = layout;
        drop(current);
        wake();
    }
}

pub(crate) struct Watch<S = AtomicU64> {
    state: Arc<S>,
    stop: Option<async_channel::Sender<()>>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl<S: SettingState> Watch<S> {
    pub(crate) fn start(
        wake: kettle_core::Waker,
        address: Option<String>,
        key: &'static str,
    ) -> Self {
        let state = Arc::new(S::initial());
        let worker_state = state.clone();
        let (stop, stopped) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name(format!("kettle-{key}"))
            .spawn(move || {
                future::block_on(future::or(
                    watch(address.as_deref(), worker_state.as_ref(), &wake, key),
                    async {
                        let _ = stopped.recv().await;
                    },
                ));
            });
        match worker {
            Ok(worker) => Self {
                state,
                stop: Some(stop),
                _thread: Some(worker),
            },
            Err(error) => {
                log::debug!("system appearance setting watcher unavailable: {error}");
                Self {
                    state,
                    stop: None,
                    _thread: None,
                }
            }
        }
    }
}

impl Watch {
    pub(crate) fn value(&self) -> Option<u32> {
        u32::try_from(self.state.load(Ordering::Acquire)).ok()
    }
}

impl ButtonWatch {
    pub(crate) fn button_layout(&self) -> crate::window_buttons::ButtonLayout {
        self.state.lock().map(|b| b.clone()).unwrap_or_default()
    }
}

impl<S> Drop for Watch<S> {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.try_send(());
        }
    }
}

fn publish(state: &AtomicU64, preference: Option<u32>, wake: &kettle_core::Waker) {
    let value = preference.map_or(UNKNOWN, u64::from);
    if state.swap(value, Ordering::AcqRel) != value {
        wake();
    }
}

async fn watch<S: SettingState>(
    address: Option<&str>,
    state: &S,
    wake: &kettle_core::Waker,
    key: &str,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if let Err(error) = connected(address, state, wake, &mut backoff, key).await {
            log::debug!("system appearance setting portal unavailable: {error}");
        }
        state.clear(wake);
        async_io::Timer::after(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn connection(address: Option<&str>) -> zbus::Result<Connection> {
    let builder = match address {
        Some(address) => Builder::address(address)?,
        None => Builder::session()?,
    }
    .max_queued(1)
    .method_timeout(TIMEOUT);
    future::or(builder.build(), async {
        async_io::Timer::after(TIMEOUT).await;
        Err(zbus::Error::Failure(
            "settings portal connection timed out".into(),
        ))
    })
    .await
}

fn bounded(message: &zbus::Message) -> zbus::Result<()> {
    if message.body().len() > MAX_BODY_BYTES {
        Err(zbus::Error::Failure(
            "settings portal reply is too large".into(),
        ))
    } else {
        Ok(())
    }
}

async fn connected<S: SettingState>(
    address: Option<&str>,
    state: &S,
    wake: &kettle_core::Waker,
    backoff: &mut Duration,
    key: &str,
) -> zbus::Result<()> {
    let connection = connection(address).await?;
    let proxy = Proxy::new(&connection, DESTINATION, PATH, INTERFACE).await?;
    let mut owners = proxy.receive_owner_changed().await?;
    let mut settings = proxy
        .receive_signal_with_args("SettingChanged", &[(0, S::NAMESPACE), (1, key)])
        .await?;
    // A backend may yield during ReadOne. An overlapping signal makes the
    // snapshot's age ambiguous, so reconcile with another bounded read.
    let mut snapshot = None;
    for _ in 0..3 {
        for _ in 0..8 {
            match future::poll_once(settings.next()).await {
                None => break,
                Some(Some(message)) => bounded(&message)?,
                Some(None) => {
                    return Err(zbus::Error::Failure("settings portal disconnected".into()));
                }
            }
        }
        if future::poll_once(owners.next()).await.is_some() {
            return Err(zbus::Error::Failure("settings portal owner changed".into()));
        }
        let reply = proxy.call_method("ReadOne", &(S::NAMESPACE, key)).await?;
        bounded(&reply)?;
        if future::poll_once(owners.next()).await.is_some() {
            return Err(zbus::Error::Failure("settings portal owner changed".into()));
        }
        match future::poll_once(settings.next()).await {
            None => {
                snapshot = Some(reply);
                break;
            }
            Some(Some(message)) => bounded(&message)?,
            Some(None) => return Err(zbus::Error::Failure("settings portal disconnected".into())),
        }
    }
    let snapshot = snapshot
        .ok_or_else(|| zbus::Error::Failure("settings portal preference is unstable".into()))?;
    let mut after = snapshot.recv_position();
    state.update(snapshot.body().deserialize::<OwnedValue>()?, wake)?;
    *backoff = Duration::from_secs(1);
    loop {
        let message = future::or(
            async {
                let _ = owners.next().await;
                None
            },
            settings.next(),
        )
        .await
        .ok_or_else(|| {
            zbus::Error::Failure("settings portal disconnected or owner changed".into())
        })?;
        if message.recv_position() <= after {
            return Err(zbus::Error::Failure(
                "settings portal snapshot needs reconciliation".into(),
            ));
        }
        after = message.recv_position();
        bounded(&message)?;
        let (namespace, changed_key, value): (String, String, OwnedValue) =
            message.body().deserialize()?;
        if namespace == S::NAMESPACE && changed_key == key {
            state.update(value, wake)?;
        }
    }
}

#[cfg(test)]
#[path = "motion/portal_tests.rs"]
mod tests;
