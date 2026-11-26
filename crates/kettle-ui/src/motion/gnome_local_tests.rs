use super::*;
use crate::motion::Preference;

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

fn wait_for(message: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A test child that publishes its PID in a file, then sleeps; killed on
/// drop if a test fails before the code under test reaps it.
struct Sleeper {
    _dir: kettle_test_support::PrivateTempDir,
    pid_file: PathBuf,
}

impl Sleeper {
    fn new() -> Self {
        let dir = kettle_test_support::private_tempdir("kettle-gnome-motion-");
        let pid_file = dir.path().join("pid");
        Self {
            _dir: dir,
            pid_file,
        }
    }

    fn command(&self) -> Command {
        let mut command = sh("echo $$ > \"$1\"; exec sleep 30");
        command.arg("sleeper").arg(&self.pid_file);
        command
    }

    fn pid(&self) -> Option<libc::pid_t> {
        std::fs::read_to_string(&self.pid_file)
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Whether the child is gone: killed and reaped, not left a zombie.
    fn reaped(&self) -> bool {
        let pid = self.pid().expect("the child published its PID");
        // SAFETY: signal zero only probes the test child's PID.
        let result = unsafe { libc::kill(pid, 0) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            // SAFETY: kill takes the integer PID this test's child published.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn only_gnome_sessions_read_gnomes_switch() {
    for desktop in [
        "GNOME",
        "ubuntu:GNOME",
        "GNOME-Classic:GNOME",
        "GNOME-Flashback:GNOME",
        "Unity",
        " gnome ",
    ] {
        assert!(gnome_like(Some(OsStr::new(desktop))), "{desktop}");
    }
    for desktop in ["KDE", "XFCE", "sway", "X-Cinnamon", "LXQt", ""] {
        assert!(!gnome_like(Some(OsStr::new(desktop))), "{desktop}");
    }
    assert!(!gnome_like(None));
}

/// The first executable `gsettings` file in an absolute `PATH` entry; a
/// file without execute permission, a directory and a relative entry are
/// skipped.
#[test]
fn gsettings_is_found_only_as_an_executable_file_on_an_absolute_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = kettle_test_support::private_tempdir("kettle-gnome-motion-path-");
    let dir = |name: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        dir
    };
    let (empty, plain, folder, first, second) = (
        dir("empty"),
        dir("plain"),
        dir("folder"),
        dir("first"),
        dir("second"),
    );
    std::fs::write(plain.join("gsettings"), "").unwrap();
    std::fs::set_permissions(
        plain.join("gsettings"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::create_dir(folder.join("gsettings")).unwrap();
    for dir in [&first, &second] {
        std::fs::write(dir.join("gsettings"), "").unwrap();
        std::fs::set_permissions(
            dir.join("gsettings"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let path = std::env::join_paths([
        PathBuf::from("first"),
        empty,
        plain,
        folder.clone(),
        first.clone(),
        second,
    ])
    .unwrap();
    assert_eq!(find_gsettings(Some(&path)), Some(first.join("gsettings")));
    assert_eq!(find_gsettings(Some(folder.as_os_str())), None);
    assert_eq!(find_gsettings(None), None);
    assert_eq!(
        gsettings_get(first.join("gsettings"))
            .get_args()
            .collect::<Vec<_>>(),
        ["get", "org.gnome.desktop.interface", "enable-animations"]
    );
}

/// The review's setup: a user-owned `gsettings` with mode 0010 first in
/// `PATH`, which its owner may not run (the kernel applies the owner's bits
/// to the owner), then a working one. The working one is found and read.
#[test]
fn a_gsettings_this_process_may_not_run_does_not_hide_a_working_one() {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: geteuid takes no arguments and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root may run a file with any execute bit");
        return;
    }
    let root = kettle_test_support::private_tempdir("kettle-gnome-motion-run-");
    let script = |dir: &str, body: &str, mode: u32| {
        let dir = root.path().join(dir);
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("gsettings");
        std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        dir
    };
    let blocked = script("blocked", "printf 'true\\n'", 0o010);
    let working = script("working", "printf 'false\\n'", 0o700);
    let path = std::env::join_paths([blocked, working.clone()]).unwrap();
    assert_eq!(
        find_gsettings(Some(&path)),
        Some(working.join("gsettings")),
        "the file its owner may not run hid the working one"
    );
    // A refresh well inside the wait retries a spawn another test's fork
    // happened to make busy.
    let watch = LocalWatch::start_every(
        move || find_gsettings(Some(&path)).map(gsettings_get),
        Arc::new(|| {}),
        Duration::from_millis(20),
    );
    wait_for("the working gsettings's answer", || {
        watch.reading() == Reading::Value(false)
    });
}

#[test]
fn a_read_takes_true_or_false_from_a_successful_run_and_nothing_else() {
    let (_open, stop) = mpsc::channel();
    let read = |script: &str| read(&mut sh(script), Duration::from_secs(5), &stop);
    assert_eq!(read("printf 'true\\n'"), Some(true));
    assert_eq!(read("printf 'false\\n'"), Some(false));
    assert_eq!(read("printf \"'yes'\""), None);
    assert_eq!(read("printf false; exit 1"), None);
    assert_eq!(read("echo 'No such schema' >&2; exit 1"), None);
    assert_eq!(read("head -c 4096 /dev/zero"), None, "output is bounded");
    let mut missing = Command::new("/nonexistent/gsettings");
    assert_eq!(
        super::read(&mut missing, Duration::from_secs(5), &stop),
        None
    );
}

/// A read that overruns its time is killed and reaped, and the caller is
/// not held past it.
#[test]
fn an_overrunning_read_is_killed_and_reaped() {
    let sleeper = Sleeper::new();
    let (_open, stop) = mpsc::channel();
    let started = Instant::now();
    assert_eq!(
        read(&mut sleeper.command(), Duration::from_millis(300), &stop),
        None
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(sleeper.reaped(), "the overrunning child was left behind");
}

/// Stopping a read kills and reaps its child at once, long before its
/// timeout.
#[test]
fn a_stopped_read_is_killed_and_reaped_at_once() {
    let sleeper = Sleeper::new();
    let (stop, stopped) = mpsc::channel::<()>();
    let pid_file = sleeper.pid_file.clone();
    let stopper = std::thread::spawn(move || {
        wait_for("the child started", || pid_file.exists());
        drop(stop);
    });
    let started = Instant::now();
    assert_eq!(
        read(&mut sleeper.command(), Duration::from_secs(30), &stopped),
        None
    );
    stopper.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(sleeper.reaped(), "the stopped child was left behind");
}

/// The watch reads on its own thread, follows a change at its next read,
/// keeps its last answer through a failed read, and wakes on each change.
#[test]
fn the_watch_follows_gnomes_switch_and_keeps_its_last_answer() {
    let dir = kettle_test_support::private_tempdir("kettle-gnome-motion-watch-");
    let value = dir.path().join("value");
    std::fs::write(&value, "false\n").unwrap();
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = wakes.clone();
    let file = value.clone();
    let watch = LocalWatch::start_every(
        move || {
            let mut command = sh("cat \"$1\"");
            command.arg("stub").arg(file);
            Some(command)
        },
        Arc::new(move || {
            counted.fetch_add(1, Ordering::AcqRel);
        }),
        Duration::from_millis(20),
    );
    wait_for("GNOME's switch off", || {
        watch.reading() == Reading::Value(false)
    });
    std::fs::write(&value, "true\n").unwrap();
    wait_for("GNOME's switch on", || {
        watch.reading() == Reading::Value(true)
    });
    std::fs::write(&value, "garbage\n").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(watch.reading(), Reading::Value(true));
    assert_eq!(wakes.load(Ordering::Acquire), 2);
    drop(watch);
    let none = LocalWatch::start(|| None, Arc::new(|| {}));
    wait_for("no gsettings", || none.reading() == Reading::Unavailable);
    let failing = LocalWatch::start(|| Some(sh("exit 1")), Arc::new(|| {}));
    wait_for("a failed first read", || {
        failing.reading() == Reading::Unavailable
    });
    assert_eq!(LocalWatch::unavailable().reading(), Reading::Unavailable);
}

/// Dropping the watch during a read stops it: the child is killed and
/// reaped and the thread joined, so neither outlives the watch.
#[test]
fn dropping_the_watch_mid_read_leaves_no_child_or_thread() {
    let sleeper = Sleeper::new();
    let command = sleeper.command();
    let watch = LocalWatch::start(move || Some(command), Arc::new(|| {}));
    wait_for("the child started", || sleeper.pid().is_some());
    assert_eq!(watch.reading(), Reading::Pending);
    let started = Instant::now();
    drop(watch);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(sleeper.reaped(), "the watch's child outlived it");
}

/// In a GNOME session whose portal answers neither `reduced-motion` nor
/// GNOME's key, GNOME's own switch decides: off holds motion back. A portal
/// answer still comes first, and with no answer anywhere motion is allowed.
#[test]
fn a_gnome_switch_only_gnome_itself_reports_holds_motion_back() {
    let watch = LocalWatch::start(|| Some(sh("printf 'false\\n'")), Arc::new(|| {}));
    wait_for("GNOME's own answer", || watch.reading() != Reading::Pending);
    let unavailable = || Reading::Unavailable;
    let preference = Preference::linux(Reading::Unavailable, unavailable, || watch.reading());
    assert_eq!(preference, Preference::Reduced);
    assert!(!preference.allows());
    let unasked = || -> Reading<bool> { panic!("asked after an answer") };
    assert_eq!(
        Preference::linux(Reading::Value(0), unasked, unasked),
        Preference::Normal
    );
    assert_eq!(
        Preference::linux(Reading::Unavailable, || Reading::Value(true), unasked),
        Preference::Normal
    );
    assert_eq!(
        Preference::linux(Reading::Unavailable, unavailable, || Reading::Pending),
        Preference::Pending
    );
    assert_eq!(
        Preference::linux(Reading::Unavailable, unavailable, unavailable),
        Preference::Unknown
    );
}
