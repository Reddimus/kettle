use super::*;
use crate::motion::Preference;

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

/// `command`, reporting each start: the child writes a byte to the returned
/// pipe before it runs anything, and `spawn` returns only after that, so a
/// start is seen even if the child is killed at once.
fn counted(mut command: Command) -> (Command, std::io::PipeReader) {
    use std::os::fd::{AsRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let (reader, writer) = std::io::pipe().expect("a pipe");
    let writer = OwnedFd::from(writer);
    // SAFETY: the hook runs in the forked child before exec and only calls
    // write(2), which is async-signal-safe, on a descriptor the closure
    // keeps open.
    unsafe {
        command.pre_exec(move || {
            libc::write(writer.as_raw_fd(), b"s".as_ptr().cast(), 1);
            Ok(())
        });
    }
    (command, reader)
}

/// Whether a command from `counted` was started (since the last start this
/// saw). The pipe also turns ready at end of file, once the command is
/// dropped, so a start is a byte read from it.
fn started(starts: &std::io::PipeReader) -> bool {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    let mut poll = libc::pollfd {
        fd: starts.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll is writable storage for one pollfd on a live descriptor.
    if unsafe { libc::poll(&mut poll, 1, 0) } <= 0 {
        return false;
    }
    let mut reader = starts;
    matches!(reader.read(&mut [0; 1]), Ok(1))
}

/// A read's time that no load on the machine uses up, for the tests that
/// are not about the deadline.
const LONG: Duration = Duration::from_secs(30);

/// Wait for `ready`, failing with `message` only if it never comes.
fn wait_for(message: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + LONG;
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A test child that sleeps. It publishes its PID from a pre-exec hook,
/// before it runs anything, and `spawn` returns only after that hook, so the
/// PID is known once the code under test has started it, however loaded the
/// machine and however soon the child is killed. (A PID the child's shell
/// wrote could be missing or empty when a busy machine killed it first.)
/// Killed on drop if a test fails before the code under test reaps it.
struct Sleeper {
    pids: std::io::PipeReader,
    writer: std::os::fd::OwnedFd,
    pid: std::sync::Mutex<Option<libc::pid_t>>,
}

impl Sleeper {
    fn new() -> Self {
        let (pids, writer) = std::io::pipe().expect("a pipe");
        Self {
            pids,
            writer: writer.into(),
            pid: std::sync::Mutex::new(None),
        }
    }

    fn command(&self) -> Command {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let mut command = sh("exec sleep 30");
        let writer = self.writer.try_clone().expect("a descriptor");
        // SAFETY: the hook runs in the forked child before exec and only
        // calls getpid(2) and write(2), which are async-signal-safe, on a
        // descriptor the closure keeps open.
        unsafe {
            command.pre_exec(move || {
                let pid = libc::getpid();
                libc::write(
                    writer.as_raw_fd(),
                    std::ptr::from_ref(&pid).cast(),
                    std::mem::size_of::<libc::pid_t>(),
                );
                Ok(())
            });
        }
        command
    }

    /// The child's PID, once it has been started.
    fn pid(&self) -> Option<libc::pid_t> {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        let mut pid = self.pid.lock().unwrap();
        if pid.is_none() {
            let mut poll = libc::pollfd {
                fd: self.pids.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll is writable storage for one pollfd on a live
            // descriptor.
            if unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                let mut bytes = [0; std::mem::size_of::<libc::pid_t>()];
                let mut reader = &self.pids;
                // The write end stays open here, so a ready pipe holds the
                // whole PID, written in one write(2).
                reader.read_exact(&mut bytes).expect("a whole PID");
                *pid = Some(libc::pid_t::from_ne_bytes(bytes));
            }
        }
        *pid
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
        second.clone(),
    ])
    .unwrap();
    assert_eq!(
        gsettings_candidates(Some(&path))
            .flatten()
            .collect::<Vec<_>>(),
        [first.join("gsettings"), second.join("gsettings")]
    );
    assert_eq!(
        gsettings_candidates(Some(folder.as_os_str())).collect::<Vec<_>>(),
        [None]
    );
    assert_eq!(gsettings_candidates(None).next(), None);
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
        gsettings_candidates(Some(&path)).flatten().next(),
        Some(working.join("gsettings")),
        "the file its owner may not run hid the working one"
    );
    // A refresh well inside the wait retries a spawn another test's fork
    // happened to make busy.
    let watch = LocalWatch::start_with(
        move || gsettings_candidates(Some(&path)).map(|file| file.map(gsettings_get)),
        Arc::new(|| {}),
        Duration::from_millis(20),
        LONG,
    );
    wait_for("the working gsettings's answer", || {
        watch.reading() == Reading::Value(false)
    });
}

#[test]
fn a_read_takes_true_or_false_from_a_successful_run_and_nothing_else() {
    let (_open, stop) = mpsc::channel();
    let soon = || Instant::now() + LONG;
    let read = |script: &str| read(&mut sh(script), soon(), &stop);
    assert_eq!(read("printf 'true\\n'"), Read::Ran(Some(true)));
    assert_eq!(read("printf 'false\\n'"), Read::Ran(Some(false)));
    assert_eq!(read("printf \"'yes'\""), Read::Ran(None));
    assert_eq!(read("printf false; exit 1"), Read::Ran(None));
    assert_eq!(read("echo 'No such schema' >&2; exit 1"), Read::Ran(None));
    assert_eq!(
        read("head -c 4096 /dev/zero"),
        Read::Ran(None),
        "output is bounded"
    );
    let mut missing = Command::new("/nonexistent/gsettings");
    assert_eq!(super::read(&mut missing, soon(), &stop), Read::Unstartable);
    // Past its deadline, or once stopped, a read starts nothing.
    let (mut late, starts) = counted(sh("exit 0"));
    assert_eq!(super::read(&mut late, Instant::now(), &stop), Read::Late);
    assert!(
        !started(&starts),
        "a read past its deadline started a child"
    );
    let (closed, gone) = mpsc::channel::<()>();
    drop(closed);
    assert_eq!(super::read(&mut late, soon(), &gone), Read::Late);
    assert!(!started(&starts), "a stopped read started a child");
    assert_eq!(super::read(&mut late, soon(), &stop), Read::Ran(None));
    assert!(started(&starts), "the counter sees a start");
}

/// A read that overruns its time is killed and reaped, and the caller is
/// not held past it.
#[test]
fn an_overrunning_read_is_killed_and_reaped() {
    let sleeper = Sleeper::new();
    let mut command = sleeper.command();
    let (_open, stop) = mpsc::channel();
    let since = Instant::now();
    // A machine busy enough to pass the deadline before the start gets a
    // `Late`, which starts nothing; try again until the child starts.
    let read = loop {
        match read(
            &mut command,
            Instant::now() + Duration::from_millis(300),
            &stop,
        ) {
            Read::Late => continue,
            read => break read,
        }
    };
    assert_eq!(read, Read::Ran(None));
    assert!(
        since.elapsed() < Duration::from_secs(20),
        "the read waited for its 30-second child"
    );
    assert!(sleeper.reaped(), "the overrunning child was left behind");
}

/// Stopping a read kills and reaps its child at once, long before its
/// timeout.
#[test]
fn a_stopped_read_is_killed_and_reaped_at_once() {
    let sleeper = &Sleeper::new();
    let (stop, stopped) = mpsc::channel::<()>();
    let since = Instant::now();
    let read = std::thread::scope(|scope| {
        // Stop once the child has started, which its published PID shows.
        scope.spawn(move || {
            wait_for("the child started", || sleeper.pid().is_some());
            drop(stop);
        });
        read(&mut sleeper.command(), Instant::now() + LONG, &stopped)
    });
    assert_eq!(read, Read::Ran(None));
    assert!(
        since.elapsed() < Duration::from_secs(20),
        "the stopped read waited for its 30-second child"
    );
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
    let woken = wakes.clone();
    let mut cat = sh("cat \"$1\"");
    cat.arg("stub").arg(&value);
    let (cat, starts) = counted(cat);
    // The first read runs it and keeps it, so every read starts this one.
    let mut cat = Some(cat);
    let watch = LocalWatch::start_with(
        move || cat.take(),
        Arc::new(move || {
            woken.fetch_add(1, Ordering::AcqRel);
        }),
        Duration::from_millis(20),
        LONG,
    );
    wait_for("GNOME's switch off", || {
        watch.reading() == Reading::Value(false)
    });
    std::fs::write(&value, "true\n").unwrap();
    wait_for("GNOME's switch on", || {
        watch.reading() == Reading::Value(true)
    });
    std::fs::write(&value, "garbage\n").unwrap();
    // Reads are one at a time: past the reads already started, the second
    // new one began after the write, and the third only once it was done.
    while started(&starts) {}
    for _ in 0..3 {
        wait_for("another read", || started(&starts));
    }
    assert_eq!(watch.reading(), Reading::Value(true));
    assert_eq!(wakes.load(Ordering::Acquire), 2);
    drop(watch);
    let none = LocalWatch::start(|| None::<Command>, Arc::new(|| {}));
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
    let mut command = Some(sleeper.command());
    let wake: kettle_core::Waker = Arc::new(|| {});
    let worker = Arc::downgrade(&wake);
    let watch = LocalWatch::start_with(move || command.take(), wake, REFRESH, LONG);
    wait_for("the child started", || sleeper.pid().is_some());
    assert_eq!(watch.reading(), Reading::Pending);
    drop(watch);
    // The thread reaps its child before it ends.
    wait_for("the watch's thread ended", || worker.upgrade().is_none());
    assert!(sleeper.reaped(), "the watch's child outlived it");
}

/// In a GNOME session whose portal answers neither `reduced-motion` nor
/// GNOME's key, GNOME's own switch decides: off holds motion back. A portal
/// answer still comes first, and with no answer anywhere motion is allowed.
#[test]
fn a_gnome_switch_only_gnome_itself_reports_holds_motion_back() {
    let watch = LocalWatch::start_with(
        || Some(sh("printf 'false\\n'")),
        Arc::new(|| {}),
        REFRESH,
        LONG,
    );
    wait_for("GNOME's own answer", || {
        watch.reading() == Reading::Value(false)
    });
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

/// Dropping the watch never waits on the filesystem: with the thread held
/// in its lookup (a hung network mount on `PATH`, here a resolver that does
/// not return), the drop returns at once. Once the lookup returns, the
/// thread starts no child and ends.
#[test]
fn dropping_the_watch_during_a_held_lookup_returns_at_once() {
    let sleeper = Sleeper::new();
    let (command, starts) = counted(sleeper.command());
    let mut command = Some(command);
    let (release, held) = mpsc::channel::<()>();
    let (entered, looking) = mpsc::channel::<()>();
    let wake: kettle_core::Waker = Arc::new(|| {});
    let worker = Arc::downgrade(&wake);
    let returned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lookup_returned = returned.clone();
    let watch = LocalWatch::start(
        move || {
            let _ = entered.send(());
            let _ = held.recv_timeout(Duration::from_secs(60));
            lookup_returned.store(true, Ordering::Release);
            command.take()
        },
        wake,
    );
    looking.recv_timeout(LONG).expect("the lookup started");
    // The lookup is released once the drop has returned, or after a while
    // if it never does, as a drop that joined the thread would not.
    let (dropped, drop_returned) = mpsc::channel::<()>();
    let releaser = std::thread::spawn(move || {
        let _ = drop_returned.recv_timeout(Duration::from_secs(10));
        let _ = release.send(());
    });
    drop(watch);
    let held_through_the_drop = !returned.load(Ordering::Acquire);
    let _ = dropped.send(());
    releaser.join().unwrap();
    assert!(held_through_the_drop, "the drop waited for the lookup");
    wait_for("the thread ended after its lookup", || {
        worker.upgrade().is_none()
    });
    assert!(!started(&starts), "a child was started after the stop");
}

/// The first answer's time starts before the lookup: a lookup held past it
/// leaves the switch unavailable, not pending, while it is still held, and
/// no read starts once it returns.
#[test]
fn a_held_lookup_counts_against_the_first_answers_time() {
    let sleeper = Sleeper::new();
    let (command, starts) = counted(sleeper.command());
    let mut command = Some(command);
    let (release, held) = mpsc::channel::<()>();
    let time = Duration::from_millis(300);
    let since = Instant::now();
    let watch = LocalWatch::start_with(
        move || {
            let _ = held.recv_timeout(Duration::from_secs(60));
            command.take()
        },
        Arc::new(|| {}),
        Duration::from_secs(30),
        time,
    );
    let early = watch.reading();
    if since.elapsed() < time {
        assert_eq!(early, Reading::Pending);
    }
    wait_for("the wait for a first answer outlasted its time", || {
        watch.reading() == Reading::Unavailable
    });
    assert_eq!(
        watch.state.load(Ordering::Acquire),
        PENDING,
        "the lookup is still held"
    );
    release.send(()).unwrap();
    wait_for("the read after the lookup gave up", || {
        watch.state.load(Ordering::Acquire) == UNAVAILABLE
    });
    assert!(!started(&starts), "a read started after its time ran out");
    assert_eq!(watch.reading(), Reading::Unavailable);
}

/// A `gsettings` in `dir` running `body` with mode 0755, as a script whose
/// interpreter is `interpreter`.
fn script(dir: &std::path::Path, interpreter: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("gsettings");
    // Replace, not rewrite in place, so a reader never sees half a script.
    let staged = dir.join(".gsettings.new");
    std::fs::write(&staged, format!("#!{interpreter}\n{body}\n")).unwrap();
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staged, &file).unwrap();
    file
}

/// The review's setup: first on `PATH`, a mode-0755 `gsettings` script
/// whose interpreter does not exist, which passes the permission check but
/// cannot be started; second, a working one. The search moves on to the
/// working one.
#[test]
fn a_gsettings_that_cannot_start_does_not_hide_a_working_one() {
    let root = kettle_test_support::private_tempdir("kettle-gnome-motion-start-");
    let broken = script(
        &root.path().join("broken"),
        "/nonexistent/interpreter",
        "printf 'true\\n'",
    );
    let working = script(&root.path().join("working"), "/bin/sh", "printf 'false\\n'");
    let path = std::env::join_paths([broken.parent().unwrap(), working.parent().unwrap()]).unwrap();
    assert_eq!(
        gsettings_candidates(Some(&path))
            .flatten()
            .collect::<Vec<_>>(),
        [broken.clone(), working]
    );
    let (_open, stop) = mpsc::channel();
    assert_eq!(
        read(
            &mut gsettings_get(broken),
            Instant::now() + Duration::from_secs(5),
            &stop
        ),
        Read::Unstartable
    );
    // A refresh well inside the wait retries a spawn another test's fork
    // happened to make busy.
    let watch = LocalWatch::start_with(
        move || gsettings_candidates(Some(&path)).map(|file| file.map(gsettings_get)),
        Arc::new(|| {}),
        Duration::from_millis(20),
        LONG,
    );
    wait_for("the working gsettings's answer", || {
        watch.reading() == Reading::Value(false)
    });
}

/// A candidate that cannot be started is never kept: the next refresh
/// searches `PATH` again and finds one the user has fixed since. A kept
/// candidate that stops starting sends the search on as well.
#[test]
fn the_search_runs_again_rather_than_keeping_what_cannot_start() {
    let root = kettle_test_support::private_tempdir("kettle-gnome-motion-again-");
    let (first, second) = (root.path().join("first"), root.path().join("second"));
    script(&first, "/nonexistent/interpreter", "printf 'true\\n'");
    std::fs::create_dir(&second).unwrap();
    let path = std::env::join_paths([&first, &second]).unwrap();
    let watch = LocalWatch::start_with(
        move || gsettings_candidates(Some(&path)).map(|file| file.map(gsettings_get)),
        Arc::new(|| {}),
        Duration::from_millis(20),
        LONG,
    );
    wait_for("nothing starts", || {
        watch.state.load(Ordering::Acquire) == UNAVAILABLE
    });
    script(&second, "/bin/sh", "printf 'false\\n'");
    wait_for("the gsettings added later", || {
        watch.reading() == Reading::Value(false)
    });
    // The kept one breaks and a working one appears before it.
    script(&second, "/nonexistent/interpreter", "printf 'false\\n'");
    script(&first, "/bin/sh", "printf 'true\\n'");
    wait_for("the search after the kept one stopped starting", || {
        watch.reading() == Reading::Value(true)
    });
}

/// The read's deadline and stop hold across the whole search: a candidate
/// the search reaches only after the deadline, or after the stop, is not
/// started.
#[test]
fn the_deadline_and_the_stop_hold_across_the_whole_search() {
    // A search whose first candidate cannot start and whose second is
    // reached only after `pause`.
    fn slow_search(
        command: Command,
        pause: Box<dyn FnOnce() + Send>,
    ) -> impl FnMut() -> Box<dyn Iterator<Item = Command>> + Send + 'static {
        let mut command = Some(command);
        let mut pause = Some(pause);
        move || {
            let mut slow = command.take();
            let mut pause = pause.take();
            let mut first = true;
            Box::new(std::iter::from_fn(move || {
                if std::mem::take(&mut first) {
                    return Some(Command::new("/nonexistent/gsettings"));
                }
                let command = slow.take()?;
                if let Some(pause) = pause.take() {
                    pause();
                }
                Some(command)
            }))
        }
    }
    let sleeper = Sleeper::new();
    let (command, starts) = counted(sleeper.command());
    // At least 400 ms, past the 200 ms deadline however busy the machine.
    let watch = LocalWatch::start_with(
        slow_search(
            command,
            Box::new(|| std::thread::sleep(Duration::from_millis(400))),
        ),
        Arc::new(|| {}),
        Duration::from_secs(30),
        Duration::from_millis(200),
    );
    wait_for("the search gave up", || {
        watch.state.load(Ordering::Acquire) == UNAVAILABLE
    });
    assert!(
        !started(&starts),
        "a candidate past the deadline was started"
    );
    drop(watch);
    let (command, starts) = counted(sleeper.command());
    let wake: kettle_core::Waker = Arc::new(|| {});
    let worker = Arc::downgrade(&wake);
    // Held until the watch has been dropped.
    let (entered, holding) = mpsc::channel::<()>();
    let (release, held) = mpsc::channel::<()>();
    let watch = LocalWatch::start_with(
        slow_search(
            command,
            Box::new(move || {
                let _ = entered.send(());
                let _ = held.recv_timeout(Duration::from_secs(60));
            }),
        ),
        wake,
        Duration::from_secs(30),
        LONG,
    );
    holding
        .recv_timeout(LONG)
        .expect("the search reached its second candidate");
    drop(watch);
    let _ = release.send(());
    wait_for("the thread ended after its search", || {
        worker.upgrade().is_none()
    });
    assert!(!started(&starts), "a candidate after the stop was started");
}

/// A lookup in one `PATH` entry held past the deadline, or past the stop
/// (a hung mount), is the last: no lookup starts in the next entry once it
/// returns, so a second hung mount cannot keep the switch unavailable or
/// keep a dropped watch's thread alive.
#[test]
fn no_lookup_starts_after_a_held_one_outlasts_the_deadline_or_the_stop() {
    use std::sync::atomic::AtomicUsize;
    // Two `PATH` entries, both misses; the first holds until released.
    let held_search = |held: mpsc::Receiver<()>, lookups: Arc<AtomicUsize>| {
        let mut held = Some(held);
        move || {
            let held = held.take();
            let lookups = lookups.clone();
            let mut entry = 0;
            std::iter::from_fn(move || {
                entry += 1;
                if entry > 2 {
                    return None;
                }
                lookups.fetch_add(1, Ordering::AcqRel);
                if let Some(held) = held.as_ref().filter(|_| entry == 1) {
                    let _ = held.recv_timeout(Duration::from_secs(20));
                }
                Some(None::<Command>)
            })
        }
    };
    let (release, held) = mpsc::channel::<()>();
    let lookups = Arc::new(AtomicUsize::new(0));
    let watch = LocalWatch::start_with(
        held_search(held, lookups.clone()),
        Arc::new(|| {}),
        Duration::from_secs(30),
        Duration::from_millis(200),
    );
    wait_for("the first lookup started", || {
        lookups.load(Ordering::Acquire) == 1
    });
    std::thread::sleep(Duration::from_millis(300));
    release.send(()).unwrap();
    wait_for("the search gave up", || {
        watch.state.load(Ordering::Acquire) == UNAVAILABLE
    });
    assert_eq!(
        lookups.load(Ordering::Acquire),
        1,
        "a lookup started past the deadline"
    );
    drop(watch);
    let (release, held) = mpsc::channel::<()>();
    let lookups = Arc::new(AtomicUsize::new(0));
    let wake: kettle_core::Waker = Arc::new(|| {});
    let worker = Arc::downgrade(&wake);
    let watch = LocalWatch::start_with(
        held_search(held, lookups.clone()),
        wake,
        Duration::from_secs(30),
        Duration::from_secs(30),
    );
    wait_for("the first lookup started", || {
        lookups.load(Ordering::Acquire) == 1
    });
    drop(watch);
    release.send(()).unwrap();
    wait_for("the thread ended after its lookup", || {
        worker.upgrade().is_none()
    });
    assert_eq!(
        lookups.load(Ordering::Acquire),
        1,
        "a lookup started after the stop"
    );
}
