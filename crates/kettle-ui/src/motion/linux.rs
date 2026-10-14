use super::Preference;
use futures_lite::{StreamExt, future};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use zbus::{Connection, Proxy, connection::Builder, zvariant::OwnedValue};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "reduced-motion";
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BODY_BYTES: usize = 4096;

pub(super) struct Watch {
    state: Arc<AtomicU8>,
    stop: Option<async_channel::Sender<()>>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    pub(super) fn start(wake: kettle_core::Waker, address: Option<String>) -> Self {
        let state = Arc::new(AtomicU8::new(0));
        let worker_state = state.clone();
        let (stop, stopped) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("kettle-motion".into())
            .spawn(move || {
                future::block_on(future::or(
                    watch(address.as_deref(), &worker_state, &wake),
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
                log::debug!("system motion watcher unavailable: {error}");
                Self {
                    state,
                    stop: None,
                    _thread: None,
                }
            }
        }
    }

    pub(super) fn preference(&self) -> Preference {
        match self.state.load(Ordering::Acquire) {
            1 => Preference::Normal,
            2 => Preference::Reduced,
            _ => Preference::Unknown,
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.try_send(());
        }
    }
}

fn publish(state: &AtomicU8, preference: Preference, wake: &kettle_core::Waker) {
    let value = match preference {
        Preference::Unknown => 0,
        Preference::Normal => 1,
        Preference::Reduced => 2,
    };
    if state.swap(value, Ordering::AcqRel) != value {
        wake();
    }
}

async fn watch(address: Option<&str>, state: &AtomicU8, wake: &kettle_core::Waker) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if let Err(error) = connected(address, state, wake, &mut backoff).await {
            log::debug!("system motion portal unavailable: {error}");
        }
        publish(state, Preference::Unknown, wake);
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
            "motion portal connection timed out".into(),
        ))
    })
    .await
}

fn bounded(message: &zbus::Message) -> zbus::Result<()> {
    if message.body().len() > MAX_BODY_BYTES {
        Err(zbus::Error::Failure(
            "motion portal reply is too large".into(),
        ))
    } else {
        Ok(())
    }
}

fn preference(value: OwnedValue) -> zbus::Result<Preference> {
    Ok(Preference::from_portal(u32::try_from(value)?))
}

async fn connected(
    address: Option<&str>,
    state: &AtomicU8,
    wake: &kettle_core::Waker,
    backoff: &mut Duration,
) -> zbus::Result<()> {
    let connection = connection(address).await?;
    let proxy = Proxy::new(&connection, DESTINATION, PATH, INTERFACE).await?;
    let mut owners = proxy.receive_owner_changed().await?;
    let mut settings = proxy
        .receive_signal_with_args("SettingChanged", &[(0, NAMESPACE), (1, KEY)])
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
                    return Err(zbus::Error::Failure("motion portal disconnected".into()));
                }
            }
        }
        if future::poll_once(owners.next()).await.is_some() {
            return Err(zbus::Error::Failure("motion portal owner changed".into()));
        }
        let reply = proxy.call_method("ReadOne", &(NAMESPACE, KEY)).await?;
        bounded(&reply)?;
        if future::poll_once(owners.next()).await.is_some() {
            return Err(zbus::Error::Failure("motion portal owner changed".into()));
        }
        match future::poll_once(settings.next()).await {
            None => {
                snapshot = Some(reply);
                break;
            }
            Some(Some(message)) => bounded(&message)?,
            Some(None) => return Err(zbus::Error::Failure("motion portal disconnected".into())),
        }
    }
    let snapshot = snapshot
        .ok_or_else(|| zbus::Error::Failure("motion portal preference is unstable".into()))?;
    let mut after = snapshot.recv_position();
    let initial = preference(snapshot.body().deserialize::<OwnedValue>()?)?;
    *backoff = Duration::from_secs(1);
    publish(state, initial, wake);
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
            zbus::Error::Failure("motion portal disconnected or owner changed".into())
        })?;
        if message.recv_position() <= after {
            return Err(zbus::Error::Failure(
                "motion portal snapshot needs reconciliation".into(),
            ));
        }
        after = message.recv_position();
        bounded(&message)?;
        let (namespace, key, value): (String, String, OwnedValue) = message.body().deserialize()?;
        if namespace == NAMESPACE && key == KEY {
            publish(state, preference(value)?, wake);
        }
    }
}

#[cfg(test)]
#[path = "portal_tests.rs"]
mod tests;
