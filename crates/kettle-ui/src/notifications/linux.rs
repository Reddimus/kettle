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
    /// Whether the server renders markup in bodies, asked once per
    /// connection (`GetCapabilities`).
    body_markup: Option<bool>,
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

    /// Whether the notification server accepted it.
    pub(super) fn show(&mut self, notification: DesktopNotification) -> bool {
        let Err(error) = self.send(&notification) else {
            return true;
        };
        super::discard_closed_sender(&mut self.connection, Connection::is_closed);
        if self.connection.is_none() {
            self.body_markup = None;
        }
        log::warn!("kettle.notify: notification send failed: {error}");
        false
    }

    fn send(&mut self, notification: &DesktopNotification) -> zbus::Result<()> {
        // GNOME ties app notifications to the sender's lifetime.
        let connection = match self.connection.take() {
            Some(connection) => connection,
            None => self.connect()?,
        };
        // A body is the program's text. A server that renders markup in
        // bodies (GNOME renders <b>, <i> and <u>; the spec also allows links
        // and images of local files) gets it escaped; one that does not shows
        // the text as it is anyway.
        let markup = *self
            .body_markup
            .get_or_insert_with(|| renders_body_markup(&connection));
        let body = if markup {
            super::escape_body_markup(&notification.body)
        } else {
            std::borrow::Cow::Borrowed(notification.body.as_str())
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
                    body.as_ref(),
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

/// Whether the notification server says it renders markup in bodies. A
/// server that does not answer is taken to: escaped text at worst shows an
/// entity, where unescaped markup could load a local image or render a link.
fn renders_body_markup(connection: &Connection) -> bool {
    connection
        .call_method(Some(SERVICE), OBJECT, Some(SERVICE), "GetCapabilities", &())
        .and_then(|reply| reply.body().deserialize::<Vec<String>>())
        .map_or(true, |capabilities| {
            capabilities
                .iter()
                .any(|capability| capability == "body-markup")
        })
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
        fn get_capabilities(&self) -> Vec<String> {
            vec!["body".into(), "body-markup".into()]
        }

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
        ) -> zbus::fdo::Result<u32> {
            let mut received = self.0.lock().unwrap();
            received.push((
                header.sender().unwrap().to_string(),
                summary.into(),
                body.into(),
            ));
            if summary == "fail" {
                return Err(zbus::fdo::Error::Failed("notification refused".into()));
            }
            Ok(received.len() as u32)
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
                id: None,
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
    fn method_errors_preserve_and_reuse_the_live_sender() {
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
        for title in ["first", "fail", "second"] {
            backend.show(DesktopNotification {
                id: None,
                title: title.into(),
                body: String::new(),
            });
            let sender = received.lock().unwrap()[0].0.clone();
            assert!(
                dbus.name_has_owner(zbus::names::BusName::try_from(sender.as_str()).unwrap())
                    .unwrap(),
                "call failure disconnected the sender"
            );
        }
        let records = received.lock().unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            records.iter().all(|record| record.0 == records[0].0),
            "sender was replaced after a method error"
        );
    }

    #[test]
    fn notifications_resume_after_the_service_becomes_available() {
        let bus = crate::test_bus::Bus::start();
        let mut backend = Backend {
            address: Some(bus.address.clone()),
            ..Backend::default()
        };
        backend.show(DesktopNotification {
            id: None,
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
            id: None,
            title: "after service".into(),
            body: "delivered".into(),
        });
        let records = received.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1, "after service");
        assert_eq!(records[0].2, "delivered");
    }

    /// The same server without `body-markup`: it shows bodies as they are.
    struct PlainService(Received);

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl PlainService {
        fn get_capabilities(&self) -> Vec<String> {
            vec!["body".into()]
        }

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
        ) -> u32 {
            let mut received = self.0.lock().unwrap();
            received.push((String::new(), summary.into(), body.into()));
            received.len() as u32
        }
    }

    /// A program's body never becomes markup: a server that renders body
    /// markup receives it escaped, and one that does not receives the text
    /// as written, which it shows as written. The summary is plain text by
    /// specification and is never escaped.
    #[test]
    fn program_bodies_reach_markup_servers_escaped() {
        let body =
            "<b>bold</b> <a href=\"https://example.invalid\">x</a> <img src=\"/etc/passwd\"/> & co";
        let escaped = "&lt;b&gt;bold&lt;/b&gt; &lt;a href=\"https://example.invalid\"&gt;x&lt;/a&gt; \
                       &lt;img src=\"/etc/passwd\"/&gt; &amp; co";
        for (markup, expected) in [(true, escaped), (false, body)] {
            let bus = crate::test_bus::Bus::start();
            let received = Received::default();
            let builder = Builder::address(bus.address.as_str())
                .unwrap()
                .name(SERVICE)
                .unwrap();
            let _server = if markup {
                builder.serve_at(OBJECT, Service(received.clone()))
            } else {
                builder.serve_at(OBJECT, PlainService(received.clone()))
            }
            .unwrap()
            .build()
            .unwrap();
            let mut backend = Backend {
                address: Some(bus.address.clone()),
                ..Backend::default()
            };
            for _ in 0..2 {
                backend.show(DesktopNotification {
                    id: None,
                    title: "<i>title</i>".into(),
                    body: body.into(),
                });
            }
            let records = received.lock().unwrap();
            assert_eq!(records.len(), 2, "markup {markup}");
            for record in records.iter() {
                assert_eq!(record.1, "<i>title</i>", "markup {markup}");
                assert_eq!(record.2, expected, "markup {markup}");
            }
        }
    }
}
