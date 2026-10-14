use super::*;
use crate::test_bus::Bus;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
use zbus::zvariant::Value;

#[derive(Default)]
struct Settings {
    value: AtomicU32,
    reads: AtomicUsize,
    race: AtomicBool,
    stall: AtomicBool,
}

struct Portal(Arc<Settings>);

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Portal {
    async fn read_one(
        &self,
        namespace: &str,
        key: &str,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedValue> {
        if namespace != "org.freedesktop.appearance" || key != "reduced-motion" {
            return Err(zbus::fdo::Error::InvalidArgs("unknown setting".into()));
        }
        self.0.reads.fetch_add(1, Ordering::AcqRel);
        if self.0.stall.load(Ordering::Acquire) {
            async_io::Timer::after(Duration::from_secs(5)).await;
        }
        let snapshot = self.0.value.load(Ordering::Acquire);
        if self.0.race.swap(false, Ordering::AcqRel) {
            self.0.value.store(1, Ordering::Release);
            signal(connection, Value::from(1u32)).await;
        }
        Ok(OwnedValue::from(snapshot))
    }
}

async fn server(bus: &Bus, state: Arc<Settings>) -> Connection {
    Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at("/org/freedesktop/portal/desktop", Portal(state))
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn signal(connection: &Connection, value: Value<'_>) {
    connection
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "SettingChanged",
            &("org.freedesktop.appearance", "reduced-motion", value),
        )
        .await
        .unwrap();
}

fn wait_for(message: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn stop(mut watch: Watch) {
    let thread = watch._thread.take().unwrap();
    drop(watch);
    wait_for("watcher did not stop", || thread.is_finished());
    thread.join().unwrap();
}

#[test]
fn initial_preference_and_live_signals_are_observed() {
    let bus = Bus::start();
    let state = Arc::new(Settings::default());
    let connection = future::block_on(server(&bus, state));
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let watch = Watch::start(
        Arc::new(move || {
            counted.fetch_add(1, Ordering::AcqRel);
        }),
        Some(bus.address.clone()),
    );
    wait_for("initial preference", || {
        watch.preference() == Preference::Normal
    });
    future::block_on(signal(&connection, Value::from(1u32)));
    wait_for("reduced-motion signal", || {
        watch.preference() == Preference::Reduced
    });
    future::block_on(signal(&connection, Value::from(0u32)));
    wait_for("normal-motion signal", || {
        watch.preference() == Preference::Normal
    });
    assert!(wakes.load(Ordering::Acquire) >= 3);
    stop(watch);
}

#[test]
fn a_change_during_initial_read_cannot_leave_a_stale_snapshot() {
    let bus = Bus::start();
    let state = Arc::new(Settings::default());
    state.race.store(true, Ordering::Release);
    let _connection = future::block_on(server(&bus, state.clone()));
    let watch = Watch::start(Arc::new(|| {}), Some(bus.address.clone()));
    wait_for(
        "initial read was not reconciled with its concurrent change",
        || state.reads.load(Ordering::Acquire) >= 2 && watch.preference() == Preference::Reduced,
    );
    stop(watch);
}

#[test]
fn owner_loss_is_conservative_and_a_replacement_is_observed() {
    let bus = Bus::start();
    let old = future::block_on(server(&bus, Arc::new(Settings::default())));
    let watch = Watch::start(Arc::new(|| {}), Some(bus.address.clone()));
    wait_for("initial preference", || {
        watch.preference() == Preference::Normal
    });
    future::block_on(old.release_name("org.freedesktop.portal.Desktop")).unwrap();
    wait_for("owner loss did not clear preference", || {
        watch.preference() == Preference::Unknown
    });
    let replacement = Arc::new(Settings::default());
    replacement.value.store(1, Ordering::Release);
    let _new = future::block_on(server(&bus, replacement));
    wait_for("replacement preference", || {
        watch.preference() == Preference::Reduced
    });
    stop(watch);
}

#[test]
fn shutdown_does_not_wait_for_a_stalled_portal_method() {
    let bus = Bus::start();
    let state = Arc::new(Settings::default());
    state.stall.store(true, Ordering::Release);
    let _connection = future::block_on(server(&bus, state.clone()));
    let watch = Watch::start(Arc::new(|| {}), Some(bus.address.clone()));
    wait_for("portal method not entered", || {
        state.reads.load(Ordering::Acquire) > 0
    });
    let start = std::time::Instant::now();
    stop(watch);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "shutdown waited for method timeout"
    );
}

#[test]
fn a_malformed_preference_is_conservative() {
    let bus = Bus::start();
    let connection = future::block_on(server(&bus, Arc::new(Settings::default())));
    let watch = Watch::start(Arc::new(|| {}), Some(bus.address.clone()));
    wait_for("initial preference", || {
        watch.preference() == Preference::Normal
    });
    future::block_on(signal(&connection, Value::from("not an unsigned integer")));
    wait_for("malformed value was accepted", || {
        watch.preference() == Preference::Unknown
    });
    stop(watch);
}

#[test]
fn oversized_bodies_are_refused_before_deserialization() {
    let small = zbus::Message::signal(PATH, INTERFACE, "SettingChanged")
        .unwrap()
        .build(&"small")
        .unwrap();
    assert!(bounded(&small).is_ok());
    let large = zbus::Message::signal(PATH, INTERFACE, "SettingChanged")
        .unwrap()
        .build(&"x".repeat(5000))
        .unwrap();
    assert!(matches!(bounded(&large), Err(zbus::Error::Failure(message))
        if message == "motion portal reply is too large"));
}

#[test]
#[ignore = "requires a live desktop portal and KETTLE_MOTION_EXPECT"]
fn desktop_portal_initial_preference_matches_the_native_probe() {
    let expected: u32 = std::env::var("KETTLE_MOTION_EXPECT")
        .expect("set the value from the native portal ReadOne probe")
        .parse()
        .unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let watch = Watch::start(
        Arc::new(move || {
            counted.fetch_add(1, Ordering::AcqRel);
        }),
        None,
    );
    wait_for("desktop portal preference", || {
        wakes.load(Ordering::Acquire) > 0 && watch.preference() == Preference::from_portal(expected)
    });
    stop(watch);
}

#[test]
fn a_valid_snapshot_resets_the_reconnection_delay() {
    let bus = Bus::start();
    future::block_on(async {
        let connection = server(&bus, Arc::new(Settings::default())).await;
        let state = AtomicU8::new(0);
        let wake: kettle_core::Waker = Arc::new(|| {});
        let mut backoff = Duration::from_secs(30);
        let (result, ()) = future::zip(
            connected(Some(&bus.address), &state, &wake, &mut backoff),
            async {
                let deadline = std::time::Instant::now() + Duration::from_secs(6);
                while state.load(Ordering::Acquire) != 1 {
                    assert!(std::time::Instant::now() < deadline, "initial snapshot");
                    async_io::Timer::after(Duration::from_millis(5)).await;
                }
                connection.release_name(DESTINATION).await.unwrap();
            },
        )
        .await;
        assert!(result.is_err(), "owner loss must reconnect");
        assert_eq!(
            backoff,
            Duration::from_secs(1),
            "reconnect delay stayed high"
        );
    });
}
