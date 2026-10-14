use super::DesktopNotification;
use std::collections::HashMap;
use std::time::Duration;
use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::Value;

const SERVICE: &str = "org.freedesktop.Notifications";
const OBJECT: &str = "/org/freedesktop/Notifications";

#[derive(Default)]
pub(super) struct Backend {
    connection: Option<Connection>,
    #[cfg(test)]
    address: Option<String>,
}

impl Backend {
    fn connect(&self) -> zbus::Result<Connection> {
        #[cfg(test)]
        if let Some(address) = &self.address {
            return Builder::address(address.as_str())?
                .method_timeout(Duration::from_secs(1))
                .build();
        }
        Builder::session()?
            .method_timeout(Duration::from_secs(1))
            .build()
    }

    pub(super) fn show(&mut self, notification: DesktopNotification) {
        if let Err(error) = self.send(&notification) {
            self.connection = None;
            log::warn!("kettle.notify: notification send failed: {error}");
        }
    }

    fn send(&mut self, notification: &DesktopNotification) -> zbus::Result<()> {
        // GNOME ties app notifications to the sender's lifetime.
        let connection = match self.connection.take() {
            Some(connection) => connection,
            None => self.connect()?,
        };
        let hints = HashMap::<&str, Value<'_>>::new();
        let result = connection
            .call_method(
                Some(SERVICE),
                OBJECT,
                Some(SERVICE),
                "Notify",
                &(
                    "kettle",
                    0u32,
                    "",
                    &notification.title,
                    &notification.body,
                    Vec::<&str>::new(),
                    hints,
                    -1i32,
                ),
            )
            .and_then(|reply| reply.body().deserialize::<u32>());
        self.connection = Some(connection);
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use zbus::message::Header;

    type Received = Arc<Mutex<Vec<(String, String, String)>>>;
    struct Service(Received);

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Service {
        // The standard notification wire method has eight arguments.
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            _app_name: &str,
            _replaces_id: u32,
            _app_icon: &str,
            summary: &str,
            body: &str,
            _actions: Vec<String>,
            _hints: HashMap<String, zbus::zvariant::OwnedValue>,
            _timeout: i32,
            #[zbus(header)] header: Header<'_>,
        ) -> u32 {
            let mut received = self.0.lock().unwrap();
            received.push((
                header.sender().unwrap().to_string(),
                summary.into(),
                body.into(),
            ));
            received.len() as u32
        }
    }

    #[test]
    fn notifications_keep_one_live_sender_across_completed_sends() {
        let bus = crate::test_bus::Bus::start();
        let received = Received::default();
        let server = Builder::address(bus.address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(OBJECT, Service(received.clone()))
            .unwrap()
            .build()
            .unwrap();
        let dbus = zbus::blocking::fdo::DBusProxy::new(&server).unwrap();
        let mut backend = Backend {
            address: Some(bus.address.clone()),
            ..Backend::default()
        };
        for title in ["first", "second"] {
            backend.show(DesktopNotification {
                title: title.into(),
                body: "local body".into(),
            });
            assert_eq!(received.lock().unwrap().last().unwrap().1, title);
        }
        let records = received.lock().unwrap().clone();
        assert_eq!(
            records[0].0, records[1].0,
            "every notification must use the same sender"
        );
        assert_eq!(records[0].2, "local body");
        let sender = zbus::names::BusName::try_from(records[0].0.as_str()).unwrap();
        let until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < until {
            assert!(
                dbus.name_has_owner(sender.clone()).unwrap(),
                "GNOME removes an app's notification when its sender disconnects"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(backend);
        let until = Instant::now() + Duration::from_secs(1);
        while dbus.name_has_owner(sender.clone()).unwrap() {
            assert!(
                Instant::now() < until,
                "backend retained its bus after shutdown"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn notifications_resume_after_the_service_becomes_available() {
        let bus = crate::test_bus::Bus::start();
        let mut backend = Backend {
            address: Some(bus.address.clone()),
            ..Backend::default()
        };
        backend.show(DesktopNotification {
            title: "before service".into(),
            body: "not delivered".into(),
        });
        let received = Received::default();
        let _server = Builder::address(bus.address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(OBJECT, Service(received.clone()))
            .unwrap()
            .build()
            .unwrap();
        backend.show(DesktopNotification {
            title: "after service".into(),
            body: "delivered".into(),
        });
        let records = received.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1, "after service");
        assert_eq!(records[0].2, "delivered");
    }
}
