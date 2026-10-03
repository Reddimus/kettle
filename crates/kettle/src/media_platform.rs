//! Where this build's media worker is installed, and whether it may run.
//!
//! `kettle-media` decides what an answer means; this module does the work it
//! cannot. It reads the running executable's directory once, at startup,
//! checks the worker file beside it, and on macOS checks the worker's code
//! signature against Kettle's own requirement. It never looks in `PATH`, the
//! working directory, the environment or the configuration. Besides
//! `codesign`, the only process it starts is the worker, in a process group of
//! its own that it kills before reaping.
//!
//! These checks keep a stray, half-installed or foreign file from running as
//! the worker. They cannot stop a program running as the same user, which can
//! rewrite a user-owned install directly; installation authenticity comes from
//! the signed release and its package hashes.

use std::path::{Path, PathBuf};

use kettle_media::client::{
    FileIdentity, SpawnedWorker, UnavailableCause, WorkerExit, WorkerPlatform, WorkerProcess,
};

/// The worker's file name, beside the `kettle` executable.
const WORKER_NAME: &str = "kettle-media-worker";

pub(crate) struct InstalledWorker {
    path: Result<PathBuf, UnavailableCause>,
    /// The code requirement the worker's signature must meet.
    #[cfg(target_os = "macos")]
    requirement: String,
}

impl InstalledWorker {
    /// Record the worker's path from the running executable now. A later
    /// rename or deletion of the executable does not move it.
    pub(crate) fn capture() -> Self {
        Self::beside(std::env::current_exe())
    }

    fn beside(executable: std::io::Result<PathBuf>) -> Self {
        Self {
            path: worker_beside(executable),
            #[cfg(target_os = "macos")]
            requirement: signature::official_requirement(),
        }
    }
}

/// The worker beside `executable`, with links in the executable's path
/// resolved, so a Homebrew link finds the install it points into.
fn worker_beside(executable: std::io::Result<PathBuf>) -> Result<PathBuf, UnavailableCause> {
    if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
        return Err(UnavailableCause::UnsupportedPlatform);
    }
    // On Linux a deleted executable reads as "<path> (deleted)", which does
    // not resolve.
    let executable = executable
        .and_then(std::fs::canonicalize)
        .map_err(|_| UnavailableCause::NoInstallLocation)?;
    let directory = executable
        .parent()
        .filter(|directory| directory.is_absolute())
        .ok_or(UnavailableCause::NoInstallLocation)?;
    Ok(directory.join(WORKER_NAME))
}

impl WorkerPlatform for InstalledWorker {
    fn worker_path(&self) -> Result<&Path, UnavailableCause> {
        self.path.as_deref().map_err(|cause| *cause)
    }

    fn inspect(&self, path: &Path) -> Result<FileIdentity, UnavailableCause> {
        inspect(path)
    }

    fn verify(&self, path: &Path) -> Result<(), UnavailableCause> {
        #[cfg(target_os = "macos")]
        return signature::verify(path, &self.requirement);
        // The package install checked the worker's bytes against the signed
        // release; there is no signature to check at run time.
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Ok(())
        }
    }

    fn spawn(&self, path: &Path) -> std::io::Result<SpawnedWorker> {
        spawn(path)
    }
}

/// Start the worker with `kettle_media`'s command, leading a process group of
/// its own.
#[cfg(unix)]
fn spawn(path: &Path) -> std::io::Result<SpawnedWorker> {
    let mut child = kettle_media::client::worker_command(path).spawn()?;
    let pipes = (child.stdin.take(), child.stdout.take());
    let mut process = GroupProcess {
        child,
        reaped: false,
    };
    let (Some(stdin), Some(stdout)) = pipes else {
        process.kill();
        return Err(std::io::Error::other("the worker's pipes are missing"));
    };
    Ok(SpawnedWorker {
        process: Box::new(process),
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
    })
}

#[cfg(not(unix))]
fn spawn(_: &Path) -> std::io::Result<SpawnedWorker> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// A worker leading its own process group. The group is killed before the
/// worker is reaped: nothing it started outlives it, and a group id that may
/// already belong to someone else is never signalled.
#[cfg(unix)]
struct GroupProcess {
    child: std::process::Child,
    reaped: bool,
}

#[cfg(unix)]
impl WorkerProcess for GroupProcess {
    fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>> {
        if !self.reaped {
            if !self.exited()? {
                return Ok(None);
            }
            // Exited, not yet reaped: the group id is still its own.
            self.kill_group();
        }
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status.map(exit_of))
    }

    fn kill(&mut self) {
        if !self.reaped {
            self.kill_group();
        }
    }
}

#[cfg(unix)]
impl GroupProcess {
    fn pid(&self) -> std::io::Result<libc::pid_t> {
        libc::pid_t::try_from(self.child.id()).map_err(std::io::Error::other)
    }

    /// Whether the worker has exited, leaving it unreaped.
    fn exited(&self) -> std::io::Result<bool> {
        let pid = self.pid()?;
        // SAFETY: an all-zero siginfo_t is a valid value for waitid to fill.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is valid writable storage. WNOHANG never blocks and
        // WNOWAIT leaves the child waitable, so `self.child` still reaps it.
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::try_from(pid).map_err(std::io::Error::other)?,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if waited != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // With WNOHANG a child that has not exited leaves si_pid zero.
        #[cfg(target_os = "linux")]
        // SAFETY: waitid filled `info` for a child state change, or left it
        // zeroed.
        let waited_pid = unsafe { info.si_pid() };
        #[cfg(not(target_os = "linux"))]
        let waited_pid = info.si_pid;
        Ok(waited_pid != 0)
    }

    fn kill_group(&self) {
        if let Ok(pid) = self.pid() {
            // SAFETY: plain integers. The worker is not reaped yet, so `pid`
            // still names the process group it leads (it was started with
            // process_group(0)), and no one else's.
            unsafe { libc::killpg(pid, libc::SIGKILL) };
        }
    }
}

#[cfg(unix)]
fn exit_of(status: std::process::ExitStatus) -> WorkerExit {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => WorkerExit::Code(code),
        (None, signal) => WorkerExit::Signal(signal.unwrap_or(0)),
    }
}

/// A regular executable file, not a link, owned by this user or root, with
/// neither it nor its directory writable by anyone else, by mode or (on macOS)
/// by ACL, and no set-id bits.
#[cfg(unix)]
fn inspect(path: &Path) -> Result<FileIdentity, UnavailableCause> {
    use std::os::unix::fs::MetadataExt as _;

    let leaf = match std::fs::symlink_metadata(path) {
        Ok(leaf) => leaf,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UnavailableCause::WorkerMissing);
        }
        Err(_) => return Err(UnavailableCause::UnsafeWorkerFile),
    };
    let directory = path
        .parent()
        .and_then(|directory| std::fs::metadata(directory).ok())
        .ok_or(UnavailableCause::UnsafeWorkerFile)?;
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    let owned = |uid: u32| uid == 0 || uid == user;
    let safe = leaf.file_type().is_file()
        && leaf.mode() & 0o111 != 0
        && leaf.mode() & 0o6022 == 0
        && owned(leaf.uid())
        && directory.is_dir()
        && directory.mode() & 0o022 == 0
        && owned(directory.uid());
    #[cfg(target_os = "macos")]
    let safe = safe
        && !acl::grants_write(path)
        && path
            .parent()
            .is_some_and(|directory| !acl::grants_write(directory));
    if !safe {
        return Err(UnavailableCause::UnsafeWorkerFile);
    }
    Ok(FileIdentity {
        dev: leaf.dev(),
        ino: leaf.ino(),
        size: leaf.size(),
        mtime_seconds: leaf.mtime(),
        mtime_nanos: leaf.mtime_nsec(),
        ctime_seconds: leaf.ctime(),
        ctime_nanos: leaf.ctime_nsec(),
    })
}

#[cfg(not(unix))]
fn inspect(_: &Path) -> Result<FileIdentity, UnavailableCause> {
    Err(UnavailableCause::UnsupportedPlatform)
}

/// Extended ACLs, which on macOS can let another user write a file whose mode
/// says otherwise. On Linux an entry that grants write shows in the group mode
/// bits, which the mode check already refuses.
#[cfg(target_os = "macos")]
mod acl {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    use libc::{c_char, c_int, c_void, ssize_t};

    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    /// Permissions that only read. An entry that allows any other counts as
    /// letting someone write.
    const READ_ONLY: [&[u8]; 5] = [
        b"read",
        b"execute",
        b"readattr",
        b"readextattr",
        b"readsecurity",
    ];

    unsafe extern "C" {
        fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_to_text(acl: *mut c_void, length: *mut ssize_t) -> *mut c_char;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    /// Whether the ACL on `path` itself, not through a link, allows anyone
    /// more than reading. Deny entries only take rights away. An ACL that
    /// cannot be read counts as allowing.
    pub(super) fn grants_write(path: &Path) -> bool {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return true;
        };
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            // A file without an extended ACL reads as ENOENT.
            return std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT);
        }
        let mut length: ssize_t = 0;
        // SAFETY: `acl` is the live ACL just returned, and `length` a valid
        // place for its text length.
        let text = unsafe { acl_to_text(acl, &mut length) };
        let grants = text.is_null() || {
            // SAFETY: acl_to_text returns a NUL-terminated string, valid
            // until it is freed below.
            let grants = text_grants_write(unsafe { CStr::from_ptr(text) }.to_bytes());
            // SAFETY: `text` came from acl_to_text and is freed once.
            unsafe { acl_free(text.cast()) };
            grants
        };
        // SAFETY: `acl` came from acl_get_link_np and is freed once.
        unsafe { acl_free(acl) };
        grants
    }

    /// Whether an `acl_to_text` listing has an entry that allows more than
    /// reading. Entries read `tag:uuid:name:id:allow[,flags]:permissions`; an
    /// entry in any other shape counts as allowing.
    pub(super) fn text_grants_write(text: &[u8]) -> bool {
        text.split(|&byte| byte == b'\n')
            .filter(|entry| !entry.is_empty() && !entry.starts_with(b"!#"))
            .any(|entry| {
                let fields: Vec<&[u8]> = entry.split(|&byte| byte == b':').collect();
                let [_, _, _, _, kind, permissions] = fields.as_slice() else {
                    return true;
                };
                let deny = kind.split(|&byte| byte == b',').next() == Some(b"deny");
                !deny
                    && permissions
                        .split(|&byte| byte == b',')
                        .any(|permission| !READ_ONLY.contains(&permission))
            })
    }
}

/// The worker's code signature, checked with the `codesign` macOS ships, as
/// the updater checks the app bundle.
#[cfg(target_os = "macos")]
mod signature {
    use std::io::Read as _;
    use std::path::Path;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use kettle_media::client::UnavailableCause;

    const CODESIGN: &str = "/usr/bin/codesign";
    /// How long one `codesign` run may take, and how long a run killed at that
    /// deadline gets to exit. A run past its deadline is `check_failed`, so a
    /// stuck tool cannot hold the answer at `checking`.
    const CODESIGN_BOUNDS: Bounds = Bounds {
        run: Duration::from_secs(30),
        reap: Duration::from_secs(1),
    };
    /// Set while a `codesign` run killed at its deadline has not exited, as a
    /// filesystem stuck in uninterruptible I/O can make it. No run starts
    /// meanwhile, because it would hang the same way, so at most one is left
    /// behind.
    static CODESIGN_UNREAPED: AtomicBool = AtomicBool::new(false);
    /// More than `codesign --display` ever reports; the rest is not read.
    const MAX_REPORT_BYTES: u64 = 64 * 1024;
    /// More architectures than any worker has.
    const MAX_ARCHITECTURES: usize = 8;
    /// The worker's signing identifier. The app's is `org.kettle.terminal`, so
    /// the app's own requirement cannot be reused for the worker.
    pub(super) const WORKER_IDENTIFIER: &str = "org.kettle.terminal.media-worker";
    /// The CodeDirectory flag for the hardened runtime.
    const CS_RUNTIME: u32 = 0x1_0000;

    #[derive(Clone, Copy)]
    pub(super) struct Bounds {
        pub(super) run: Duration,
        pub(super) reap: Duration,
    }

    /// Apple's chain to a Developer ID Application certificate issued to
    /// Kettle's team, under the worker's own identifier.
    pub(super) fn official_requirement() -> String {
        format!(
            "anchor apple generic and identifier \"{WORKER_IDENTIFIER}\" \
             and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
             and certificate leaf[subject.OU] = \"{}\"",
            kettle_update::APPLE_TEAM_IDENTIFIER
        )
    }

    /// A strict signature check of every architecture against `requirement`,
    /// then the hardened runtime on each, which a requirement cannot express.
    pub(super) fn verify(path: &Path, requirement: &str) -> Result<(), UnavailableCause> {
        let mut verify = Command::new(CODESIGN);
        verify
            .args(["--verify", "--strict", "-R"])
            .arg(format!("={requirement}"))
            .arg("--")
            .arg(path);
        let (verified, _) = run_bounded(verify, CODESIGN_BOUNDS, &CODESIGN_UNREAPED)?;
        if !verified {
            return Err(UnavailableCause::Unverified);
        }
        // `codesign --display` reports one architecture, on stderr.
        for architecture in architectures(path)? {
            let mut display = Command::new(CODESIGN);
            display.args(["--display", "--verbose=1"]);
            if let Some(architecture) = &architecture {
                display.args(["--arch", architecture]);
            }
            display.arg("--").arg(path);
            let (described, report) = run_bounded(display, CODESIGN_BOUNDS, &CODESIGN_UNREAPED)?;
            if !described || !hardened_runtime(&String::from_utf8_lossy(&report)) {
                return Err(UnavailableCause::Unverified);
            }
        }
        Ok(())
    }

    /// The architectures in the Mach-O file at `path`, read from its header.
    fn architectures(path: &Path) -> Result<Vec<Option<String>>, UnavailableCause> {
        let mut header = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(4096).read_to_end(&mut header))
            .map_err(|_| UnavailableCause::Unverified)?;
        parse_architectures(&header).ok_or(UnavailableCause::Unverified)
    }

    /// The architectures a Mach-O header lists, each as `codesign --arch`
    /// takes it by number (`cputype,cpusubtype`), or one `None` for a
    /// single-architecture file. `None` for anything else, including a
    /// universal header with no, too many or repeated architectures.
    pub(super) fn parse_architectures(header: &[u8]) -> Option<Vec<Option<String>>> {
        let word = |at: usize| {
            header
                .get(at..at.checked_add(4)?)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(u32::from_be_bytes)
        };
        let entry_bytes = match word(0)? {
            0xcafe_babe => 20,
            0xcafe_babf => 32,
            0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe => return Some(vec![None]),
            _ => return None,
        };
        let count = usize::try_from(word(4)?).ok()?;
        if count == 0 || count > MAX_ARCHITECTURES {
            return None;
        }
        let mut architectures = Vec::with_capacity(count);
        for index in 0..count {
            let at = 8 + index * entry_bytes;
            // The top byte of a subtype holds capability bits, not the subtype.
            let architecture = format!("{},{}", word(at)?, word(at + 4)? & 0x00ff_ffff);
            if architectures.contains(&Some(architecture.clone())) {
                return None;
            }
            architectures.push(Some(architecture));
        }
        Some(architectures)
    }

    /// Run `command` with no input or standard output, and report whether it
    /// succeeded and the start of what it wrote to stderr. A run past
    /// `bounds.run` is killed and given `bounds.reap` to exit; one that does
    /// not is left to a reaper with `unreaped` set, and while it is set no run
    /// starts.
    pub(super) fn run_bounded(
        mut command: Command,
        bounds: Bounds,
        unreaped: &'static AtomicBool,
    ) -> Result<(bool, Vec<u8>), UnavailableCause> {
        if unreaped.load(Ordering::Acquire) {
            return Err(UnavailableCause::CheckFailed);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| UnavailableCause::CheckFailed)?;
        let Some(status) = wait_until(&mut child, Instant::now() + bounds.run) else {
            let _ = child.kill();
            reap(child, bounds.reap, unreaped);
            return Err(UnavailableCause::CheckFailed);
        };
        // The report is small enough to sit in the pipe until the tool exits.
        let mut report = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            let _ = stderr.take(MAX_REPORT_BYTES).read_to_end(&mut report);
        }
        Ok((status.success(), report))
    }

    /// `child`'s exit status, if it exits by `until`.
    fn wait_until(child: &mut Child, until: Instant) -> Option<ExitStatus> {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                _ => return None,
            }
        }
    }

    /// Give `child` `grace` to exit. One that does not is reaped by a thread
    /// whenever it does, with `unreaped` set until then. Without that thread
    /// it stays set, and no `codesign` runs again in this process.
    pub(super) fn reap(mut child: Child, grace: Duration, unreaped: &'static AtomicBool) {
        if wait_until(&mut child, Instant::now() + grace).is_some() {
            return;
        }
        unreaped.store(true, Ordering::Release);
        let _ = std::thread::Builder::new()
            .name("kettle-codesign-reap".into())
            .spawn(move || {
                let _ = child.wait();
                unreaped.store(false, Ordering::Release);
            });
    }

    /// Whether `codesign --display` reports a CodeDirectory with the hardened
    /// runtime flag, as in `CodeDirectory v=20500 size=… flags=0x10000(runtime)`.
    pub(super) fn hardened_runtime(described: &str) -> bool {
        described
            .lines()
            .filter(|line| line.starts_with("CodeDirectory "))
            .flat_map(str::split_whitespace)
            .filter_map(|field| field.strip_prefix("flags=0x"))
            .filter_map(|flags| {
                let hex = flags.split('(').next().unwrap_or_default();
                u32::from_str_radix(hex, 16).ok()
            })
            .any(|flags| flags & CS_RUNTIME != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_is_beside_the_executable() {
        let install = tempfile::tempdir().unwrap();
        let executable = install.path().join("kettle");
        std::fs::write(&executable, "").unwrap();
        let worker = InstalledWorker::beside(Ok(executable.clone()));
        let expected = install.path().canonicalize().unwrap().join(WORKER_NAME);
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert_eq!(worker.worker_path(), Ok(expected.as_path()));
            // Renaming the executable afterwards does not move the worker.
            std::fs::rename(&executable, install.path().join("kettle.old")).unwrap();
            assert_eq!(worker.worker_path(), Ok(expected.as_path()));
        } else {
            assert_eq!(
                worker.worker_path(),
                Err(UnavailableCause::UnsupportedPlatform)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_executable_finds_the_install_it_points_into() {
        let install = tempfile::tempdir().unwrap();
        let links = tempfile::tempdir().unwrap();
        let executable = install.path().join("kettle");
        std::fs::write(&executable, "").unwrap();
        let link = links.path().join("kettle");
        std::os::unix::fs::symlink(&executable, &link).unwrap();
        let worker = InstalledWorker::beside(Ok(link));
        let expected = install.path().canonicalize().unwrap().join(WORKER_NAME);
        assert_eq!(worker.worker_path(), Ok(expected.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn an_unresolvable_executable_has_no_install_location() {
        let install = tempfile::tempdir().unwrap();
        for executable in [
            Err(std::io::Error::other("no executable path")),
            Ok(install.path().join("kettle (deleted)")),
        ] {
            assert_eq!(
                InstalledWorker::beside(executable).worker_path(),
                Err(UnavailableCause::NoInstallLocation)
            );
        }
    }

    /// Run `test` in a child of this test binary whose `PATH` and working
    /// directory both hold a decoy worker, and resolve the real one there.
    #[cfg(unix)]
    fn resolve_with_decoy(test: &str, on_path: bool) {
        const CHILD_ENV: &str = "KETTLE_MEDIA_WORKER_DECOY_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let decoys = tempfile::tempdir().unwrap();
            let decoy = decoys.path().join(WORKER_NAME);
            std::fs::write(&decoy, "#!/bin/sh\nexit 0\n").unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", test, "--nocapture"])
                .env(CHILD_ENV, "1");
            if on_path {
                child.env("PATH", decoys.path());
            } else {
                child.current_dir(decoys.path());
            }
            let status = child.status().unwrap();
            assert!(status.success(), "decoy child failed: {status}");
            return;
        }
        let worker = InstalledWorker::capture();
        let path = worker.worker_path().unwrap();
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(path.parent(), executable.parent());
        // No worker sits beside the test binary, whatever PATH and the
        // working directory hold.
        assert_eq!(worker.inspect(path), Err(UnavailableCause::WorkerMissing));
    }

    #[cfg(unix)]
    #[test]
    fn worker_on_path_is_ignored() {
        resolve_with_decoy("media_platform::tests::worker_on_path_is_ignored", true);
    }

    #[cfg(unix)]
    #[test]
    fn worker_in_cwd_is_ignored() {
        resolve_with_decoy("media_platform::tests::worker_in_cwd_is_ignored", false);
    }

    #[cfg(unix)]
    mod files {
        use super::*;
        use std::os::unix::fs::PermissionsExt as _;

        pub(super) fn install() -> (tempfile::TempDir, PathBuf) {
            let directory = tempfile::tempdir().unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let worker = directory.path().join(WORKER_NAME);
            std::fs::write(&worker, "worker").unwrap();
            std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
            (directory, worker)
        }

        fn chmod(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        #[test]
        fn worker_missing_is_typed() {
            let (_install, worker) = install();
            std::fs::remove_file(&worker).unwrap();
            assert_eq!(inspect(&worker), Err(UnavailableCause::WorkerMissing));
        }

        #[test]
        fn an_installed_worker_reports_its_identity() {
            use std::os::unix::fs::MetadataExt as _;
            let (_install, worker) = install();
            let identity = inspect(&worker).unwrap();
            let metadata = std::fs::metadata(&worker).unwrap();
            assert_eq!((identity.ino, identity.size), (metadata.ino(), 6));
        }

        #[test]
        fn a_rewritten_worker_has_a_new_identity() {
            let (_install, worker) = install();
            let before = inspect(&worker).unwrap();
            // Same size, inode and modification time: only the status time
            // shows the rewrite.
            let modified = std::fs::metadata(&worker).unwrap().modified().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&worker, "WORKER").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&worker)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            let after = inspect(&worker).unwrap();
            assert_eq!((after.ino, after.size), (before.ino, before.size));
            assert_eq!(
                (after.mtime_seconds, after.mtime_nanos),
                (before.mtime_seconds, before.mtime_nanos)
            );
            assert_ne!(after, before);
        }

        #[test]
        fn unsafe_worker_files_are_refused() {
            /// Make the install at the first path, with its worker at the
            /// second, unsafe.
            type Breakage = fn(&Path, &Path);
            let cases: [(&str, Breakage); 7] = [
                ("not executable", |_, worker| chmod(worker, 0o644)),
                ("group-writable", |_, worker| chmod(worker, 0o775)),
                ("world-writable", |_, worker| chmod(worker, 0o757)),
                ("set-user-id", |_, worker| chmod(worker, 0o4755)),
                ("directory writable by others", |directory, _| {
                    chmod(directory, 0o775)
                }),
                ("a directory", |_, worker| {
                    std::fs::remove_file(worker).unwrap();
                    std::fs::create_dir(worker).unwrap();
                    chmod(worker, 0o755);
                }),
                ("a link to a worker", |directory, worker| {
                    let real = directory.join("real-worker");
                    std::fs::rename(worker, &real).unwrap();
                    std::os::unix::fs::symlink(&real, worker).unwrap();
                }),
            ];
            for (name, make_unsafe) in cases {
                let (install, worker) = install();
                assert!(inspect(&worker).is_ok(), "{name}: control");
                make_unsafe(install.path(), &worker);
                assert_eq!(
                    inspect(&worker),
                    Err(UnavailableCause::UnsafeWorkerFile),
                    "{name}"
                );
                chmod(install.path(), 0o755);
            }
        }
    }

    #[cfg(unix)]
    mod processes {
        use super::*;
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::PermissionsExt as _;
        use std::time::{Duration, Instant};

        /// A shell script standing in for the worker.
        fn script(directory: &Path, body: &str) -> PathBuf {
            let path = directory.join(WORKER_NAME);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn wait_exit(process: &mut dyn WorkerProcess) -> WorkerExit {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(exit) = process.try_wait().unwrap() {
                    return exit;
                }
                assert!(Instant::now() < deadline, "the worker never exited");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// The first line the worker writes.
        fn first_line(worker: &mut SpawnedWorker) -> String {
            let mut line = Vec::new();
            let mut byte = [0];
            while worker.stdout.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
                line.push(byte[0]);
            }
            String::from_utf8(line).unwrap()
        }

        /// Whether `pid` is running; a zombie no one has reaped yet is not.
        fn running(pid: &str) -> bool {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !state.is_empty() && !state.starts_with('Z')
        }

        /// Whether `pid` stops running within a few seconds.
        fn gone(pid: &str) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while running(pid) {
                if Instant::now() > deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            true
        }

        /// Everything the worker writes before it exits.
        fn all_output(worker: &mut SpawnedWorker) -> String {
            let mut output = String::new();
            worker.stdout.read_to_string(&mut output).unwrap();
            output
        }

        #[test]
        fn the_worker_starts_with_nothing_inherited() {
            // `env` itself as the worker: an empty environment prints nothing.
            let mut worker = spawn(Path::new("/usr/bin/env")).unwrap();
            assert_eq!(all_output(&mut worker), "");
            assert_eq!(wait_exit(worker.process.as_mut()), WorkerExit::Code(0));
            // `pwd`: the working directory is the root.
            let mut worker = spawn(Path::new("/bin/pwd")).unwrap();
            assert_eq!(all_output(&mut worker), "/\n");
            assert_eq!(wait_exit(worker.process.as_mut()), WorkerExit::Code(0));
            // And it leads its own process group.
            let directory = tempfile::tempdir().unwrap();
            let path = script(
                directory.path(),
                "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ')\"",
            );
            let mut worker = spawn(&path).unwrap();
            let line = first_line(&mut worker);
            let ids: Vec<&str> = line.split(' ').collect();
            assert_eq!(ids[0], ids[1], "not its own process group: {line}");
            assert_eq!(wait_exit(worker.process.as_mut()), WorkerExit::Code(0));
        }

        #[test]
        fn killing_the_worker_kills_its_group() {
            let directory = tempfile::tempdir().unwrap();
            let path = script(directory.path(), "/bin/sleep 30 &\necho $!\nwait");
            let mut worker = spawn(&path).unwrap();
            let grandchild = first_line(&mut worker);
            assert!(running(&grandchild), "{grandchild} never started");
            worker.process.kill();
            // The kill itself reaches the child, before anything is reaped.
            assert!(gone(&grandchild), "{grandchild} outlived the kill");
            assert_eq!(
                wait_exit(worker.process.as_mut()),
                WorkerExit::Signal(libc::SIGKILL)
            );
        }

        #[test]
        fn an_exited_worker_s_group_is_killed_before_it_is_reaped() {
            // The worker exits 0 by itself, leaving a child in its group: its
            // own exit status is kept, and the child does not survive it.
            let directory = tempfile::tempdir().unwrap();
            let path = script(directory.path(), "/bin/sleep 30 &\necho $!\nexit 0");
            let mut worker = spawn(&path).unwrap();
            let grandchild = first_line(&mut worker);
            assert_eq!(wait_exit(worker.process.as_mut()), WorkerExit::Code(0));
            assert!(gone(&grandchild), "{grandchild} outlived the worker");
            // Killing after the reap signals nothing.
            worker.process.kill();
        }

        #[test]
        fn a_running_worker_is_not_reaped_by_asking() {
            let directory = tempfile::tempdir().unwrap();
            let path = script(directory.path(), "read line\nexit 3");
            let mut worker = spawn(&path).unwrap();
            for _ in 0..5 {
                assert_eq!(worker.process.try_wait().unwrap(), None);
            }
            worker.stdin.write_all(b"go\n").unwrap();
            drop(worker.stdin);
            assert_eq!(wait_exit(worker.process.as_mut()), WorkerExit::Code(3));
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::super::acl::text_grants_write;
        use super::super::signature::{
            Bounds, WORKER_IDENTIFIER, hardened_runtime, parse_architectures, reap, run_bounded,
            verify,
        };
        use super::*;
        use std::process::Command;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        /// A guard of its own for each test, as production has one for
        /// `codesign`.
        fn unreaped() -> &'static AtomicBool {
            Box::leak(Box::new(AtomicBool::new(false)))
        }

        fn bounds(run: Duration) -> Bounds {
            Bounds {
                run,
                reap: Duration::from_secs(10),
            }
        }

        fn run(program: &str, args: &[&str], path: &Path) -> String {
            let output = Command::new(program).args(args).arg(path).output().unwrap();
            assert!(
                output.status.success(),
                "{program} {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stderr).into_owned()
        }

        /// A small native program to sign: a copy of `/usr/bin/true`.
        fn program(directory: &Path) -> PathBuf {
            let path = directory.join(WORKER_NAME);
            std::fs::copy("/usr/bin/true", &path).unwrap();
            path
        }

        /// Sign `path` ad hoc as the worker, as a local build would be.
        fn sign_ad_hoc(path: &Path, runtime: bool) {
            let mut args = vec!["--force", "--sign", "-", "--identifier", WORKER_IDENTIFIER];
            if runtime {
                args.extend(["--options", "runtime"]);
            }
            args.push("--");
            run("/usr/bin/codesign", &args, path);
        }

        /// A universal copy of `/usr/bin/true` whose architectures are signed
        /// ad hoc as the worker one at a time, each with the hardened runtime
        /// as `runtime` says, then laid out again behind a new header.
        /// (`codesign` will not re-sign one architecture of a universal file.)
        fn universal(directory: &Path, runtime: &[bool]) -> PathBuf {
            let original = std::fs::read("/usr/bin/true").unwrap();
            let word = |at: usize| u32::from_be_bytes(original[at..at + 4].try_into().unwrap());
            let size = |value: u32| usize::try_from(value).unwrap();
            assert_eq!(word(0), 0xcafe_babe, "/usr/bin/true is universal");
            assert_eq!(size(word(4)), runtime.len());
            let mut out = vec![0; 8 + 20 * runtime.len()];
            out[..4].copy_from_slice(&0xcafe_babe_u32.to_be_bytes());
            out[4..8].copy_from_slice(&word(4).to_be_bytes());
            for (index, &runtime) in runtime.iter().enumerate() {
                let entry = 8 + 20 * index;
                let (offset, length, align) = (
                    size(word(entry + 8)),
                    size(word(entry + 12)),
                    word(entry + 16),
                );
                let thin = directory.join(format!("architecture-{index}"));
                std::fs::write(&thin, &original[offset..offset + length]).unwrap();
                sign_ad_hoc(&thin, runtime);
                let signed = std::fs::read(&thin).unwrap();
                let alignment = 1 << align;
                let start = out.len().div_ceil(alignment) * alignment;
                out.resize(start, 0);
                // cputype and cpusubtype stay; offset, size and alignment are
                // the new layout's.
                out[entry..entry + 8].copy_from_slice(&original[entry..entry + 8]);
                out[entry + 8..entry + 12]
                    .copy_from_slice(&u32::try_from(start).unwrap().to_be_bytes());
                out[entry + 12..entry + 16]
                    .copy_from_slice(&u32::try_from(signed.len()).unwrap().to_be_bytes());
                out[entry + 16..entry + 20].copy_from_slice(&align.to_be_bytes());
                out.extend_from_slice(&signed);
            }
            let path = directory.join(WORKER_NAME);
            std::fs::write(&path, out).unwrap();
            path
        }

        /// A requirement only this exact signed file meets, standing in for
        /// the Developer ID signature CI cannot make. `codesign` checks every
        /// architecture in a universal file, so it names each one's cdhash.
        fn pinned_requirement(path: &Path) -> String {
            let cdhashes: Vec<String> = ["x86_64", "arm64", "arm64e"]
                .into_iter()
                .filter_map(|arch| {
                    let output = Command::new("/usr/bin/codesign")
                        .args(["--display", "-vvv", "--arch", arch, "--"])
                        .arg(path)
                        .output()
                        .unwrap();
                    let described = String::from_utf8_lossy(&output.stderr).into_owned();
                    let cdhash = described
                        .lines()
                        .find_map(|line| line.strip_prefix("CDHash="))?;
                    output
                        .status
                        .success()
                        .then(|| format!("cdhash H\"{cdhash}\""))
                })
                .collect();
            assert!(!cdhashes.is_empty(), "codesign reports no cdhash");
            format!(
                "identifier \"{WORKER_IDENTIFIER}\" and ({})",
                cdhashes.join(" or ")
            )
        }

        fn official(path: &Path) -> Result<(), UnavailableCause> {
            let worker = InstalledWorker::beside(Ok(path.to_path_buf()));
            worker.verify(path)
        }

        #[test]
        fn the_official_requirement_compiles() {
            let output = Command::new("/usr/bin/csreq")
                .arg("-r")
                .arg(format!("={}", signature::official_requirement()))
                .arg("-t")
                .output()
                .unwrap();
            assert!(output.status.success(), "csreq refused the requirement");
            // csreq prints the requirement back in its canonical form.
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.contains(WORKER_IDENTIFIER), "{text}");
            assert!(
                text.contains(kettle_update::APPLE_TEAM_IDENTIFIER),
                "{text}"
            );
            assert!(text.contains("anchor apple generic"), "{text}");
        }

        #[test]
        fn a_pinned_ad_hoc_worker_verifies() {
            // The control for the refusals below: the same checks pass a
            // signature that meets the requirement they are given.
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            assert_eq!(verify(&worker, &pinned_requirement(&worker)), Ok(()));
        }

        #[test]
        fn macos_adhoc_worker_fails_official_requirement() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            assert_eq!(official(&worker), Err(UnavailableCause::Unverified));
        }

        #[test]
        fn an_apple_program_fails_official_requirement() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            assert_eq!(official(&worker), Err(UnavailableCause::Unverified));
        }

        #[test]
        fn macos_unsigned_worker_is_unverified() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            let requirement = pinned_requirement(&worker);
            run("/usr/bin/codesign", &["--remove-signature", "--"], &worker);
            assert_eq!(official(&worker), Err(UnavailableCause::Unverified));
            assert_eq!(
                verify(&worker, &requirement),
                Err(UnavailableCause::Unverified)
            );
        }

        #[test]
        fn macos_tampered_worker_is_unverified() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            let requirement = pinned_requirement(&worker);
            assert_eq!(verify(&worker, &requirement), Ok(()));
            // Flip one byte in the first architecture's first page, past its
            // header: always signed code. (The end of an architecture holds
            // the signature itself, with unsigned room to spare, and padding
            // between architectures is not signed either.) The first
            // architecture's offset is in the big-endian universal header.
            let mut bytes = std::fs::read(&worker).unwrap();
            let offset = if bytes[..4] == [0xca, 0xfe, 0xba, 0xbe] {
                usize::try_from(u32::from_be_bytes(bytes[16..20].try_into().unwrap())).unwrap()
            } else {
                0
            };
            bytes[offset + 1024] ^= 0xff;
            std::fs::write(&worker, bytes).unwrap();
            assert_eq!(
                verify(&worker, &requirement),
                Err(UnavailableCause::Unverified)
            );
        }

        #[test]
        fn a_worker_without_the_hardened_runtime_is_unverified() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, false);
            assert_eq!(
                verify(&worker, &pinned_requirement(&worker)),
                Err(UnavailableCause::Unverified)
            );
        }

        #[test]
        fn every_architecture_needs_the_hardened_runtime() {
            for runtime in [[true, false], [false, true]] {
                let directory = tempfile::tempdir().unwrap();
                let worker = universal(directory.path(), &runtime);
                assert_eq!(
                    verify(&worker, &pinned_requirement(&worker)),
                    Err(UnavailableCause::Unverified),
                    "{runtime:?}"
                );
            }
            // The control: the same layout with the runtime on both passes.
            let directory = tempfile::tempdir().unwrap();
            let worker = universal(directory.path(), &[true, true]);
            assert_eq!(verify(&worker, &pinned_requirement(&worker)), Ok(()));
        }

        #[test]
        fn architectures_are_read_from_the_header() {
            let universal = |entries: &[(u32, u32)]| {
                let mut header = 0xcafe_babe_u32.to_be_bytes().to_vec();
                header.extend(u32::try_from(entries.len()).unwrap().to_be_bytes());
                for &(cputype, subtype) in entries {
                    for word in [cputype, subtype, 0x4000, 0x100, 14] {
                        header.extend(word.to_be_bytes());
                    }
                }
                header
            };
            let both = universal(&[(0x0100_0007, 3), (0x0100_000c, 0x8000_0002)]);
            assert_eq!(
                parse_architectures(&both),
                Some(vec![
                    Some("16777223,3".to_string()),
                    Some("16777228,2".to_string())
                ])
            );
            for magic in [0xfeed_facf_u32, 0xcffa_edfe, 0xfeed_face, 0xcefa_edfe] {
                assert_eq!(parse_architectures(&magic.to_be_bytes()), Some(vec![None]));
            }
            let repeated = universal(&[(0x0100_000c, 0), (0x0100_000c, 0)]);
            let nine = universal(&[
                (1, 0),
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0),
                (6, 0),
                (7, 0),
                (8, 0),
                (9, 0),
            ]);
            for header in [
                universal(&[]),
                nine,
                repeated,
                both[..30].to_vec(),
                b"#!/bin/sh\n".to_vec(),
                Vec::new(),
            ] {
                assert_eq!(parse_architectures(&header), None, "{header:?}");
            }
        }

        #[test]
        fn a_stuck_check_is_stopped_at_its_deadline() {
            let mut sleep = Command::new("/bin/sleep");
            sleep.arg("30");
            let unreaped = unreaped();
            let started = std::time::Instant::now();
            assert_eq!(
                run_bounded(sleep, bounds(Duration::from_millis(100)), unreaped),
                Err(UnavailableCause::CheckFailed)
            );
            assert!(started.elapsed() < Duration::from_secs(10));
            // Killed and reaped within its grace: nothing is left behind.
            assert!(!unreaped.load(Ordering::Acquire));
        }

        #[test]
        fn a_check_that_outlives_its_kill_blocks_new_runs_until_reaped() {
            // A child still running when its grace ends, as one stuck in
            // uninterruptible I/O would be after the kill.
            let mut slow = Command::new("/bin/sleep");
            slow.arg("1");
            let child = slow.spawn().unwrap();
            let unreaped = unreaped();
            reap(child, Duration::ZERO, unreaped);
            assert!(unreaped.load(Ordering::Acquire));
            // Meanwhile no run starts: this one would leave a marker.
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("ran");
            let mut touch = Command::new("/usr/bin/touch");
            touch.arg(&marker);
            assert_eq!(
                run_bounded(touch, bounds(Duration::from_secs(10)), unreaped),
                Err(UnavailableCause::CheckFailed)
            );
            assert!(!marker.exists());
            // Once the child exits the reaper clears the guard.
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while unreaped.load(Ordering::Acquire) {
                assert!(std::time::Instant::now() < deadline, "never reaped");
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut touch = Command::new("/usr/bin/touch");
            touch.arg(&marker);
            assert_eq!(
                run_bounded(touch, bounds(Duration::from_secs(10)), unreaped),
                Ok((true, Vec::new()))
            );
            assert!(marker.exists());
        }

        #[test]
        fn a_finished_check_reports_its_status_and_stderr() {
            let mut report = Command::new("/bin/sh");
            report.args(["-c", "echo report >&2; exit 3"]);
            assert_eq!(
                run_bounded(report, bounds(Duration::from_secs(10)), unreaped()),
                Ok((false, b"report\n".to_vec()))
            );
            let mut quiet = Command::new("/bin/sh");
            quiet.args(["-c", "exit 0"]);
            assert_eq!(
                run_bounded(quiet, bounds(Duration::from_secs(10)), unreaped()),
                Ok((true, Vec::new()))
            );
        }

        #[test]
        fn an_acl_that_lets_others_write_is_refused() {
            // (what the ACL is on, the entry, whether the worker is refused)
            for (on_directory, entry, refused) in [
                (false, "everyone allow write", true),
                (false, "everyone allow append", true),
                (false, "everyone allow writesecurity", true),
                (true, "everyone allow add_file,delete_child", true),
                (false, "everyone allow read,execute", false),
                (false, "everyone deny delete", false),
                (true, "everyone deny delete_child", false),
            ] {
                let (install, worker) = files::install();
                let target = if on_directory {
                    install.path()
                } else {
                    worker.as_path()
                };
                run("/bin/chmod", &["+a", entry], target);
                let inspected = inspect(&worker);
                // Clear it again: a deny entry would stop the cleanup.
                run("/bin/chmod", &["-N"], target);
                if refused {
                    assert_eq!(
                        inspected,
                        Err(UnavailableCause::UnsafeWorkerFile),
                        "{entry}"
                    );
                } else {
                    assert!(inspected.is_ok(), "{entry}: {inspected:?}");
                }
            }
        }

        #[test]
        fn acl_text_counts_only_read_and_deny_entries_as_safe() {
            let everyone = "group:ABCDEFAB-CDEF-ABCD-EFAB-CDEF0000000C:everyone:12";
            for (text, grants) in [
                ("!#acl 1\n".to_string(), false),
                (format!("!#acl 1\n{everyone}:deny:delete\n"), false),
                (
                    format!("!#acl 1\n{everyone}:deny,file_inherit:write,delete\n"),
                    false,
                ),
                (format!("!#acl 1\n{everyone}:allow:read,readattr\n"), false),
                (format!("!#acl 1\n{everyone}:allow:write\n"), true),
                (
                    format!("!#acl 1\n{everyone}:allow,file_inherit:read,chown\n"),
                    true,
                ),
                (
                    format!("!#acl 1\n{everyone}:deny:delete\n{everyone}:allow:append\n"),
                    true,
                ),
                // An entry in another shape counts as allowing.
                ("!#acl 1\nuser:allow:read\n".to_string(), true),
            ] {
                assert_eq!(text_grants_write(text.as_bytes()), grants, "{text}");
            }
        }

        #[test]
        fn hardened_runtime_reads_the_code_directory_flags() {
            for (described, runtime) in [
                (
                    "Executable=/x\nCodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1\n",
                    true,
                ),
                (
                    "CodeDirectory v=20500 size=1 flags=0x10002(adhoc,runtime) hashes=1\n",
                    true,
                ),
                (
                    "CodeDirectory v=20400 size=1 flags=0x2(adhoc) hashes=1\n",
                    false,
                ),
                (
                    "CodeDirectory v=20400 size=1 flags=0x0(none) hashes=1\n",
                    false,
                ),
                // Only the CodeDirectory line counts.
                (
                    "Identifier=flags=0x10000\nCodeDirectory v=1 flags=0x0(none)\n",
                    false,
                ),
                ("", false),
            ] {
                assert_eq!(hardened_runtime(described), runtime, "{described}");
            }
        }
    }
}
