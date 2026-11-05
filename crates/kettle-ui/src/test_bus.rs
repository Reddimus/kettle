//! Private session bus for Linux protocol tests.

#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
pub(crate) struct Bus {
    _root: kettle_test_support::PrivateTempDir,
    child: Child,
    pub(crate) address: String,
}

#[cfg(target_os = "linux")]
impl Bus {
    pub(crate) fn start() -> Self {
        Self::start_in(crate::test_tempdir())
    }

    fn start_in(root: kettle_test_support::PrivateTempDir) -> Self {
        let config = root.path().join("bus.conf");
        let socket = root.path().join("bus");
        let address = socket_address(&socket);
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

#[cfg(target_os = "linux")]
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn socket_address(socket: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut address = String::from("unix:path=");
    for &byte in socket.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_/.".contains(&byte) {
            address.push(char::from(byte));
        } else {
            write!(&mut address, "%{byte:02X}").unwrap();
        }
    }
    address
}

#[cfg(target_os = "linux")]
fn bus_ready(socket: &Path) -> bool {
    // Binding creates the path before the daemon starts accepting connections.
    UnixStream::connect(socket).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::net::UnixListener;

    #[test]
    fn socket_address_escapes_reserved_and_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt as _;
        let socket = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/kettle,tests%;\xff/bus"));
        assert_eq!(
            socket_address(socket),
            "unix:path=/tmp/kettle%2Ctests%25%3B%FF/bus"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_bus_starts_in_a_directory_with_reserved_characters() {
        let root = kettle_test_support::private_tempdir("kettle-bus,%;-");
        let bus = Bus::start_in(root);
        assert!(bus.address.contains("%2C%25%3B"));
    }

    #[cfg(target_os = "linux")]
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
