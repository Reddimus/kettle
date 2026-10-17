use super::*;
use crate::test_bus::Bus;
use std::sync::{Arc, Mutex};
use zbus::zvariant::OwnedValue;

#[derive(Default)]
struct Seen {
    files: Mutex<Vec<Vec<String>>>,
    stall: bool,
}

struct Images(Arc<Seen>);

#[zbus::interface(name = "org.freedesktop.Application")]
impl Images {
    async fn open(&self, uris: Vec<String>, _platform: HashMap<String, OwnedValue>) {
        self.0.files.lock().unwrap().push(uris);
        if self.0.stall {
            async_io::Timer::after(Duration::from_secs(4)).await;
        }
    }
}

async fn client(bus: &Bus) -> Connection {
    Builder::address(bus.address.as_str())
        .unwrap()
        .method_timeout(CALL_TIMEOUT)
        .build()
        .await
        .unwrap()
}

async fn server(bus: &Bus, seen: Arc<Seen>) -> Connection {
    Builder::address(bus.address.as_str())
        .unwrap()
        .name(NAME)
        .unwrap()
        .serve_at(OBJECT, Images(seen))
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn this_binary() -> HeldExecutable {
    use std::os::unix::fs::MetadataExt;
    let path = std::env::current_exe().unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    HeldExecutable::open(&path).unwrap_or_else(|error| {
        panic!(
            "native fixture rejected: {error}; bytes={} mode={:o}",
            metadata.len(),
            metadata.mode()
        )
    })
}

#[test]
fn the_private_fixture_is_a_live_acceptable_native_executable() {
    if in_private_binary("the_private_fixture_is_a_live_acceptable_native_executable") {
        return;
    }
    assert!(this_binary().matches(process::current().unwrap().identity));
}

// Cargo's artifact can be group-writable or exceed the viewer's size limit
// with debug information. Prepare a protected copy without changing it.
fn in_private_binary(test: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    const CHILD: &str = "KETTLE_LOUPE_TEST_CASE";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return false;
    }
    let root = crate::test_tempdir();
    let executable = root.path().join("viewer-fixture");
    let prepare_path = root.path().join("prepare.log");
    let prepare = std::fs::File::create(&prepare_path).unwrap();
    let child = std::process::Command::new("objcopy")
        .arg("--strip-debug")
        .arg(std::env::current_exe().unwrap())
        .arg(&executable)
        .stdin(std::process::Stdio::null())
        .stdout(prepare.try_clone().unwrap())
        .stderr(prepare)
        .process_group(0)
        .spawn()
        .expect("Linux viewer tests require binutils objcopy");
    assert!(
        wait_private_child(child, "strip fixture").success(),
        "{}",
        std::fs::read_to_string(prepare_path).unwrap()
    );
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log_path = root.path().join("result.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let child = std::process::Command::new(executable)
        .args(["--exact", &format!("media::external::loupe::tests::{test}")])
        .env(CHILD, test)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .process_group(0)
        .spawn()
        .unwrap();
    let status = wait_private_child(child, test);
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(status.success(), "{test}: {log}");
    assert!(
        log.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "private case did not run: {test}: {log}"
    );
    true
}

fn wait_private_child(mut child: Child, label: &str) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            // SAFETY: this is our unreaped child's private process group;
            // its private bus inherits that group and no foreign work does.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            panic!("private viewer test timed out: {label}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_absent_viewer_is_not_activated_by_the_owner_probe() {
    let bus = Bus::start();
    future::block_on(async {
        let client = client(&bus).await;
        assert!(owner(&client, NAME).await.unwrap().is_none());
    });
}

#[test]
fn a_bus_alias_does_not_make_another_program_the_viewer() {
    let bus = Bus::start();
    let seen = Arc::new(Seen::default());
    future::block_on(async {
        let _server = server(&bus, seen.clone()).await;
        let client = client(&bus).await;
        let name = owner(&client, NAME).await.unwrap().unwrap();
        let expected = HeldExecutable::open(Path::new("/usr/bin/true")).unwrap();
        assert!(receiver(&client, name, &expected).await.is_err());
        assert!(seen.files.lock().unwrap().is_empty());
    });
}

#[test]
fn the_verified_unique_connection_receives_the_exact_private_file() {
    use std::os::unix::ffi::OsStringExt;
    if in_private_binary("the_verified_unique_connection_receives_the_exact_private_file") {
        return;
    }
    let bus = Bus::start();
    let seen = Arc::new(Seen::default());
    future::block_on(async {
        let _server = server(&bus, seen.clone()).await;
        let client = client(&bus).await;
        let held = this_binary();
        let receiver = receiver(&client, owner(&client, NAME).await.unwrap().unwrap(), &held)
            .await
            .unwrap();
        let file =
            Path::new("/private-copy").join(std::ffi::OsString::from_vec(b"a #\xff.png".to_vec()));
        deliver(&client, &receiver, &held, &file).await.unwrap();
        assert_eq!(
            *seen.files.lock().unwrap(),
            vec![vec!["file:///private-copy/a%20%23%FF.png".to_owned()]]
        );
    });
}

#[test]
fn replacing_the_alias_cannot_redirect_a_verified_delivery() {
    if in_private_binary("replacing_the_alias_cannot_redirect_a_verified_delivery") {
        return;
    }
    let bus = Bus::start();
    let old = Arc::new(Seen::default());
    let replacement = Arc::new(Seen::default());
    future::block_on(async {
        let original = server(&bus, old.clone()).await;
        let client = client(&bus).await;
        let held = this_binary();
        let receiver = receiver(&client, owner(&client, NAME).await.unwrap().unwrap(), &held)
            .await
            .unwrap();
        original.release_name(NAME).await.unwrap();
        let _replacement = server(&bus, replacement.clone()).await;
        deliver(
            &client,
            &receiver,
            &held,
            Path::new("/private-copy/pixels.png"),
        )
        .await
        .unwrap();
        assert_eq!(old.files.lock().unwrap().len(), 1);
        assert!(replacement.files.lock().unwrap().is_empty());
    });
}

#[test]
fn a_stalled_viewer_cannot_hold_an_open_slot_indefinitely() {
    if in_private_binary("a_stalled_viewer_cannot_hold_an_open_slot_indefinitely") {
        return;
    }
    let bus = Bus::start();
    let seen = Arc::new(Seen {
        stall: true,
        ..Seen::default()
    });
    future::block_on(async {
        let _server = server(&bus, seen).await;
        let client = client(&bus).await;
        let held = this_binary();
        let receiver = receiver(&client, owner(&client, NAME).await.unwrap().unwrap(), &held)
            .await
            .unwrap();
        let start = std::time::Instant::now();
        assert!(
            deliver(
                &client,
                &receiver,
                &held,
                Path::new("/private-copy/pixels.png")
            )
            .await
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    });
}

#[test]
fn a_closed_receiver_does_not_redirect_to_a_replacement() {
    if in_private_binary("a_closed_receiver_does_not_redirect_to_a_replacement") {
        return;
    }
    let bus = Bus::start();
    let replacement = Arc::new(Seen::default());
    future::block_on(async {
        let original = server(&bus, Arc::new(Seen::default())).await;
        let client = client(&bus).await;
        let held = this_binary();
        let receiver = receiver(&client, owner(&client, NAME).await.unwrap().unwrap(), &held)
            .await
            .unwrap();
        original.release_name(NAME).await.unwrap();
        original.close().await.unwrap();
        let _replacement = server(&bus, replacement.clone()).await;
        assert!(
            deliver(
                &client,
                &receiver,
                &held,
                Path::new("/private-copy/pixels.png")
            )
            .await
            .is_err()
        );
        assert!(replacement.files.lock().unwrap().is_empty());
    });
}

#[test]
fn delivery_rechecks_the_process_instance() {
    if in_private_binary("delivery_rechecks_the_process_instance") {
        return;
    }
    let bus = Bus::start();
    let seen = Arc::new(Seen::default());
    future::block_on(async {
        let _server = server(&bus, seen.clone()).await;
        let client = client(&bus).await;
        let held = this_binary();
        let mut receiver = receiver(&client, owner(&client, NAME).await.unwrap().unwrap(), &held)
            .await
            .unwrap();
        receiver.identity =
            ProcessIdentity::new_for_tests(receiver.identity.pid(), receiver.identity.start() + 1);
        assert!(
            deliver(
                &client,
                &receiver,
                &held,
                Path::new("/private-copy/pixels.png")
            )
            .await
            .is_err()
        );
        assert!(seen.files.lock().unwrap().is_empty());
    });
}

#[test]
fn oversized_replies_are_rejected_before_deserialization() {
    let reply = Message::method_call("/fixture", "Reply")
        .unwrap()
        .build(&vec![0u8; MAX_REPLY + 1])
        .unwrap();
    assert!(bounded(reply).is_err());
}
