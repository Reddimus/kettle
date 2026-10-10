//! The worker's self-sandbox, measured from inside: each scenario runs in a
//! fresh copy of this test binary (so the test harness itself is never
//! confined), prints what each operation did, and the test reads it. The
//! same operations run unconfined first, as a control, so a test that would
//! pass without the sandbox fails.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use kettle_media_native::sandbox::{self, Policy, SandboxError};

const SCENARIO: &str = "KETTLE_SANDBOX_PROBE";
const DIR: &str = "KETTLE_SANDBOX_DIR";

/// Run `scenario` in a fresh copy of this binary over `dir`, and what each
/// operation did, by name. A copy that made itself a process group's
/// leader, as the deadline scenario does, takes that group with it: what it
/// started and left running is killed before this returns.
fn run(scenario: &str, dir: &Path) -> BTreeMap<String, String> {
    use std::io::Read as _;
    let mut probe = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe", "--nocapture", "--test-threads=1"])
        .env(SCENARIO, scenario)
        .env(DIR, dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = Vec::new();
    probe
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    // The probe is not reaped yet, so no group but its own can have its id.
    let group = libc::pid_t::try_from(probe.id()).unwrap();
    // SAFETY: killpg takes no pointers; the group, if any, is the probe's.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    probe.wait().unwrap();
    let text = String::from_utf8_lossy(&stdout);
    // The first report shares its line with libtest's own `test probe ...`.
    text.lines()
        .filter_map(|line| line.find("probe:").map(|at| &line[at + "probe:".len()..]))
        .filter_map(|line| line.split_once('='))
        .map(|(name, result)| (name.to_owned(), result.to_owned()))
        .collect()
}

/// A directory with a held file and a sibling, both the user's own.
fn files() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("kettle-sandbox-")
        .tempdir()
        .unwrap();
    std::fs::write(dir.path().join("held.bin"), b"HELD").unwrap();
    std::fs::write(dir.path().join("sibling.bin"), b"SIBLING").unwrap();
    dir
}

fn report(name: &str, result: std::io::Result<impl std::fmt::Debug>) {
    match result {
        Ok(_) => println!("probe:{name}=ok"),
        Err(error) => println!("probe:{name}=denied {}", error.raw_os_error().unwrap_or(0)),
    }
}

/// Everything a confined job tries, in this order.
fn attempt(dir: &Path, held: &File) {
    use std::os::fd::AsRawFd as _;
    let held_fd = held.as_raw_fd();
    report("read_held", {
        let mut bytes = [0_u8; 4];
        std::os::unix::fs::FileExt::read_exact_at(held, &mut bytes, 0).map(|()| bytes)
    });
    #[cfg(target_os = "linux")]
    let reopen = format!("/proc/self/fd/{held_fd}");
    #[cfg(target_os = "macos")]
    let reopen = format!("/dev/fd/{held_fd}");
    report("reopen_held", File::open(reopen));
    report("open_held_path", File::open(dir.join("held.bin")));
    report("open_sibling", File::open(dir.join("sibling.bin")));
    // The same file through the volume that holds the users' homes.
    #[cfg(target_os = "macos")]
    report(
        "open_sibling_via_data",
        std::fs::canonicalize(dir.join("sibling.bin")).and_then(|path| {
            File::open(Path::new("/System/Volumes/Data").join(path.strip_prefix("/").unwrap()))
        }),
    );
    report("open_etc", File::open("/etc/hosts"));
    report("write_new", File::create_new(dir.join("new.bin")));
    report(
        "write_held",
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("held.bin")),
    );
    report("chmod_held", {
        use std::os::unix::fs::PermissionsExt as _;
        held.set_permissions(std::fs::Permissions::from_mode(0o600))
    });
    report(
        "network",
        std::net::TcpStream::connect_timeout(
            &"127.0.0.1:9".parse().unwrap(),
            std::time::Duration::from_millis(200),
        )
        .map(drop)
        .or_else(|error| {
            // Nothing listens there: refused means the attempt was made.
            if error.kind() == std::io::ErrorKind::ConnectionRefused {
                Ok(())
            } else {
                Err(error)
            }
        }),
    );
    // SAFETY: setsid has no preconditions; the probe ends soon after.
    report("setsid", {
        if unsafe { libc::setsid() } == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    });
    report(
        "spawn",
        Command::new("/bin/sh").arg("-c").arg("true").status(),
    );
    // SAFETY: a private segment, removed at once if it was made.
    report("sysv_shm", {
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, 4096, libc::IPC_CREAT | 0o600) };
        if id == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
            Ok(())
        }
    });
    // A message queue call by its cancellation-free number: a refusal says
    // EPERM; a call that ran says the queue id is invalid.
    #[cfg(target_os = "macos")]
    report("msgsnd_nocancel", {
        const SYS_MSGSND_NOCANCEL: libc::c_int = 418;
        let byte = 0_u8;
        // SAFETY: an invalid queue id; the kernel reads nothing past it.
        #[allow(deprecated)]
        let status = unsafe { libc::syscall(SYS_MSGSND_NOCANCEL, -1, &byte, 0, 0) };
        let error = std::io::Error::last_os_error();
        match (status, error.raw_os_error()) {
            (-1, Some(libc::EPERM)) => Err(error),
            (-1, Some(libc::EINVAL)) => Ok(()),
            other => panic!("msgsnd_nocancel answered {other:?}"),
        }
    });
    // Signal 0 only asks whether a signal could be sent.
    // SAFETY: kill with signal 0 sends nothing.
    report("signal_parent", {
        if unsafe { libc::kill(libc::getppid(), 0) } == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    });
    let sibling = dir.join("sibling.bin");
    report(
        "thread_sibling",
        std::thread::spawn(move || File::open(sibling))
            .join()
            .unwrap(),
    );
}

#[test]
fn probe() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(DIR).unwrap());
    let held = File::open(dir.join("held.bin")).unwrap();
    match scenario.as_str() {
        "control" => attempt(&dir, &held),
        "confined" => {
            let policy = Policy {
                files: vec![&held],
                ..Policy::default()
            };
            confined(sandbox::confine(&policy));
            attempt(&dir, &held);
        }
        "programs" => {
            let cat = std::fs::canonicalize("/bin/cat").unwrap();
            let me = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
            let policy = Policy {
                files: vec![&held],
                programs: vec![cat.clone(), me.clone()],
                ..Policy::default()
            };
            confined(sandbox::confine(&policy));
            let output = Command::new(&cat).arg(dir.join("held.bin")).output();
            match output {
                Ok(out) => {
                    println!("probe:cat_held={}", String::from_utf8_lossy(&out.stdout));
                    eprintln!(
                        "cat: {:?} {}",
                        out.status,
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                Err(error) => println!("probe:cat_held=spawn failed {error}"),
            }
            report(
                "cat_sibling",
                Command::new(&cat)
                    .arg(dir.join("sibling.bin"))
                    .output()
                    .and_then(succeeded),
            );
            report(
                "run_other",
                Command::new("/bin/ls")
                    .arg("/")
                    .output()
                    .and_then(succeeded),
            );
            // A child the job starts is confined as the job is.
            let child = Command::new(&me)
                .args(["--exact", "probe", "--nocapture", "--test-threads=1"])
                .env(SCENARIO, "inherited")
                .env(DIR, &dir)
                .output();
            let text = child
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .unwrap_or_default();
            let reports = text
                .lines()
                .filter_map(|line| line.find("probe:").map(|at| &line[at + "probe:".len()..]));
            for line in reports {
                println!("probe:child_{line}");
            }
        }
        "inherited" => attempt(&dir, &held),
        "deadline" => past_its_deadline(&dir, &held),
        "watchdog" => {
            let (ready, wait) = std::sync::mpsc::channel();
            let (go, start) = std::sync::mpsc::channel::<()>();
            let held_path = dir.join("held.bin");
            let watchdog = std::thread::spawn(move || {
                let confined = sandbox::confine_thread_to_nothing();
                ready.send(()).unwrap();
                start.recv().unwrap();
                (confined, File::open(held_path))
            });
            let bystander_path = dir.join("sibling.bin");
            let (bystander_ready, bystander_wait) = std::sync::mpsc::channel();
            let (bystander_go, bystander_start) = std::sync::mpsc::channel::<()>();
            let bystander = std::thread::spawn(move || {
                bystander_ready.send(()).unwrap();
                bystander_start.recv().unwrap();
                File::open(bystander_path)
            });
            wait.recv().unwrap();
            bystander_wait.recv().unwrap();
            let policy = Policy {
                files: vec![&held],
                ..Policy::default()
            };
            confined(sandbox::confine(&policy));
            go.send(()).unwrap();
            bystander_go.send(()).unwrap();
            let (confined, opened) = watchdog.join().unwrap();
            report("watchdog_confine", confined.map_err(io));
            report("watchdog_held", opened);
            report("bystander_sibling", bystander.join().unwrap());
        }
        other => panic!("no scenario {other}"),
    }
}

/// A confined job whose decoder runs past its deadline is answered at the
/// deadline: where the sandbox refuses to signal the decoder, the job does
/// not wait for it. The stand-in decoder spins for at least nine seconds,
/// far past any answer the test accepts. The probe leads its own process
/// group, as the worker does, so `run` kills what is left of the decoder
/// with that group.
fn past_its_deadline(dir: &Path, held: &File) {
    use kettle_media::video::{VideoContainer, VideoDecoder as _, VideoInput};
    use std::os::unix::fs::PermissionsExt as _;
    let decoder = dir.join("decoder");
    std::fs::create_dir_all(&decoder).unwrap();
    for name in ["ffmpeg", "ffprobe"] {
        let path = decoder.join(name);
        std::fs::write(
            &path,
            "#!/bin/bash\nend=$((SECONDS + 10)); while [ $SECONDS -lt $end ]; do :; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ffmpeg = kettle_media_native::ffmpeg::Ffmpeg::trust(&decoder.join("ffmpeg")).unwrap();
    let programs = ffmpeg.programs().map(Path::to_path_buf).to_vec();
    let policy = Policy {
        files: vec![held],
        programs,
        ..Policy::default()
    };
    // SAFETY: setpgid takes no pointers.
    assert_eq!(
        unsafe { libc::setpgid(0, 0) },
        0,
        "the probe leads no group"
    );
    confined(sandbox::confine(&policy));
    let identity = kettle_media::PathIdentity::of(&held.metadata().unwrap()).unwrap();
    let input = VideoInput {
        file: held,
        identity,
        container: VideoContainer::IsoBmff,
    };
    let started = std::time::Instant::now();
    let result = ffmpeg.stills(
        input,
        &mut |_| unreachable!("the decoder never describes the video"),
        started + std::time::Duration::from_millis(300),
    );
    println!("probe:deadline_result={:?}", result.err());
    println!("probe:deadline_ms={}", started.elapsed().as_millis());
}

fn io(error: SandboxError) -> std::io::Error {
    std::io::Error::other(error)
}

/// Report how confining went, telling a system without a sandbox apart from
/// one where confining failed.
fn confined(result: Result<(), SandboxError>) {
    println!(
        "probe:confine={}",
        match result {
            Ok(()) => "ok",
            Err(SandboxError::Unavailable) => "unavailable",
            Err(SandboxError::Failed) => "failed",
        }
    );
}

fn succeeded(output: std::process::Output) -> std::io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(libc::EACCES))
    }
}

/// Whether this system has a sandbox to measure: a system without one is
/// skipped, saying so, but a sandbox that failed to apply, or a probe that
/// said nothing, fails the test.
fn available() -> bool {
    let dir = files();
    let result = run("confined", dir.path());
    match result.get("confine").map(String::as_str) {
        Some("ok") => true,
        Some("unavailable") => {
            eprintln!("SKIP: this system has no sandbox Kettle can use");
            false
        }
        other => panic!("the sandbox did not apply: {other:?} {result:?}"),
    }
}

/// Unconfined, every operation the confined job is refused works.
#[test]
fn every_refused_operation_works_without_the_sandbox() {
    let dir = files();
    let result = run("control", dir.path());
    #[cfg(target_os = "macos")]
    for name in ["open_sibling_via_data", "msgsnd_nocancel"] {
        assert_eq!(
            result.get(name).map(String::as_str),
            Some("ok"),
            "{name}: {result:?}"
        );
    }
    for name in [
        "read_held",
        "reopen_held",
        "open_held_path",
        "open_sibling",
        "open_etc",
        "write_new",
        "write_held",
        "chmod_held",
        "network",
        "setsid",
        "spawn",
        "sysv_shm",
        "signal_parent",
        "thread_sibling",
    ] {
        assert_eq!(
            result.get(name).map(String::as_str),
            Some("ok"),
            "{name}: {result:?}"
        );
    }
}

/// Confined, a job reads the file it holds, as the file it is, and nothing
/// else; writes nothing; reaches no network; changes no mode; stays in its
/// session; starts no program; and threads it starts are confined too.
#[test]
fn a_confined_job_reads_only_what_it_holds() {
    if !available() {
        return;
    }
    let dir = files();
    let result = run("confined", dir.path());
    let ok = |name: &str| {
        assert_eq!(
            result.get(name).map(String::as_str),
            Some("ok"),
            "{name}: {result:?}"
        )
    };
    let denied = |name: &str| {
        assert!(
            result
                .get(name)
                .is_some_and(|result| result.starts_with("denied")),
            "{name}: {result:?}"
        )
    };
    for name in ["confine", "read_held", "reopen_held", "open_held_path"] {
        ok(name);
    }
    #[cfg(target_os = "macos")]
    {
        denied("open_sibling_via_data");
        denied("msgsnd_nocancel");
    }
    for name in [
        "open_sibling",
        "open_etc",
        "write_new",
        "write_held",
        "chmod_held",
        "network",
        "setsid",
        "spawn",
        "sysv_shm",
        "signal_parent",
        "thread_sibling",
    ] {
        denied(name);
    }
}

/// A job may run only the programs its policy names, which read only what
/// the job may, and a process it starts inherits every refusal.
#[test]
fn a_confined_job_runs_only_its_programs() {
    if !available() {
        return;
    }
    let dir = files();
    let result = run("programs", dir.path());
    assert_eq!(
        result.get("confine").map(String::as_str),
        Some("ok"),
        "{result:?}"
    );
    assert_eq!(
        result.get("cat_held").map(String::as_str),
        Some("HELD"),
        "{result:?}"
    );
    for name in ["cat_sibling", "run_other"] {
        assert!(
            result
                .get(name)
                .is_some_and(|result| result.starts_with("denied")),
            "{name}: {result:?}"
        );
    }
    for name in [
        "open_sibling",
        "open_etc",
        "write_new",
        "network",
        "setsid",
        "spawn",
    ] {
        let name = format!("child_{name}");
        assert!(
            result
                .get(&name)
                .is_some_and(|result| result.starts_with("denied")),
            "{name}: {result:?}"
        );
    }
}

/// A thread started before the job is confined, as the worker's watchdog
/// is, confines itself to nothing. On Linux, where Landlock binds a thread
/// and what it starts, a thread that did not would keep its access, which is
/// why the watchdog must; macOS confines the whole process.
#[test]
fn an_earlier_thread_confines_itself() {
    if !available() {
        return;
    }
    let dir = files();
    let result = run("watchdog", dir.path());
    assert_eq!(
        result.get("confine").map(String::as_str),
        Some("ok"),
        "{result:?}"
    );
    assert_eq!(
        result.get("watchdog_confine").map(String::as_str),
        Some("ok"),
        "{result:?}"
    );
    #[cfg(target_os = "linux")]
    {
        assert!(result["watchdog_held"].starts_with("denied"), "{result:?}");
        assert_eq!(result["bystander_sibling"], "ok", "{result:?}");
    }
    #[cfg(target_os = "macos")]
    assert!(
        result["bystander_sibling"].starts_with("denied"),
        "{result:?}"
    );
}

/// A decoder past its deadline does not hold a confined job up, even where
/// the sandbox refuses the job its usual kill.
#[test]
fn a_decoder_past_its_deadline_does_not_hold_the_job() {
    if !available() {
        return;
    }
    let dir = files();
    let result = run("deadline", dir.path());
    assert_eq!(
        result.get("confine").map(String::as_str),
        Some("ok"),
        "{result:?}"
    );
    assert_eq!(
        result.get("deadline_result").map(String::as_str),
        Some("Some(RenderTimeout)"),
        "{result:?}"
    );
    let elapsed: u64 = result["deadline_ms"].parse().unwrap();
    assert!(
        elapsed < 2_000,
        "answered after {elapsed} ms: it waited for the decoder"
    );
}
