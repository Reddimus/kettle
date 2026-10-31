//! Private session bus for Linux protocol tests.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub(crate) struct Bus {
    _root: kettle_test_support::PrivateTempDir,
    child: Child,
    pub(crate) address: String,
}

impl Bus {
    pub(crate) fn start() -> Self {
        let root = crate::test_tempdir();
        let config = root.path().join("bus.conf");
        let socket = root.path().join("bus");
        let address = format!("unix:path={}", socket.display());
        let listen = address
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        write!(
            std::fs::File::create(&config).unwrap(),
            "<busconfig><type>session</type><listen>{listen}</listen><auth>EXTERNAL</auth>\
             <policy context=\"default\"><allow user=\"*\"/><allow own=\"*\"/>\
             <allow send_destination=\"*\"/><allow receive_sender=\"*\"/>\
             </policy></busconfig>"
        )
        .unwrap();
        let child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("private bus tests require dbus-daemon");
        let mut bus = Self {
            _root: root,
            child,
            address,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !bus_ready(&socket) {
            assert!(
                bus.child.try_wait().unwrap().is_none(),
                "private bus exited"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "private bus startup timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        bus
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bus_ready(socket: &Path) -> bool {
    // Binding creates the path before the daemon starts accepting connections.
    UnixStream::connect(socket).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn an_existing_path_is_not_a_listening_bus() {
        let root = crate::test_tempdir();
        let socket = root.path().join("bus");
        std::fs::File::create(&socket).unwrap();
        assert!(!bus_ready(&socket));
        std::fs::remove_file(&socket).unwrap();
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(bus_ready(&socket));
    }
}
