//! Shared persistence primitives for Kettle's small local state files.
//!
//! Writes are staged beside their destination, synced, atomically replaced,
//! and followed by a parent-directory sync on platforms that support it.

mod private;

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub(crate) fn test_tempdir() -> kettle_test_support::PrivateTempDir {
    kettle_test_support::private_tempdir("kettle-state-test-")
}

pub use private::{
    create_private_dirs, create_private_file_new, discard_created_private_file,
    is_kettle_owned_dir_name, open_existing_private_file, open_private_file,
    open_private_file_append, remove_open_private_file, restrict_private_file,
};

/// Maximum on-disk size of the shared remote-command spool.
///
/// Sender and receiver both enforce this boundary so an accepted append can
/// never make the receiver discard an earlier valid command batch.
pub const MAX_REMOTE_COMMAND_BYTES: u64 = 1 << 20;

/// Return the deterministic sibling lock path for a remote-command spool.
///
/// The `.lock` suffix is appended to the file name without converting it to
/// UTF-8, so sender and receiver agree for every platform-native path. For
/// example, `remote.cmd` maps to `remote.cmd.lock`.
pub fn remote_command_lock_path(path: &Path) -> PathBuf {
    let mut file_name = path.file_name().unwrap_or_default().to_os_string();
    file_name.push(".lock");
    path.with_file_name(file_name)
}

/// Policy for an atomic file replacement.
#[derive(Clone, Copy, Debug)]
pub struct AtomicWriteOptions {
    /// Unix mode for a new file, or for every replacement when
    /// `preserve_permissions` is false.
    pub unix_mode: u32,
    /// Preserve permissions from an existing regular destination.
    pub preserve_permissions: bool,
    /// Refuse to replace a symbolic link itself.
    pub reject_symlink: bool,
}

impl AtomicWriteOptions {
    /// Enforced private user state (`0600` on Unix; a protected current-user
    /// DACL on Windows), rejecting symbolic-link destinations even when
    /// replacing a more permissive legacy file.
    pub const PRIVATE: Self = Self {
        unix_mode: 0o600,
        preserve_permissions: false,
        reject_symlink: true,
    };

    /// Preserve an existing regular file's permissions, while creating a new
    /// file privately and rejecting symbolic-link destinations.
    pub const PRESERVE_PERMISSIONS: Self = Self {
        unix_mode: 0o600,
        preserve_permissions: true,
        reject_symlink: true,
    };
}

impl Default for AtomicWriteOptions {
    fn default() -> Self {
        Self::PRIVATE
    }
}

/// Atomically and durably replace `destination` with `bytes`.
pub fn atomic_replace(
    destination: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
) -> io::Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("destination has no parent: {}", destination.display()),
        )
    })?;
    private::create_private_parent_dirs(destination).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "create verified private parent for {}: {error}",
                destination.display()
            ),
        )
    })?;

    let (destination_exists, mut existing_permissions) = match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if options.reject_symlink && metadata_is_link_like(&metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "refusing to replace symbolic link: {}",
                        destination.display()
                    ),
                ));
            }
            if !metadata.file_type().is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "destination is not a regular file: {}",
                        destination.display()
                    ),
                ));
            }
            (
                true,
                options
                    .preserve_permissions
                    .then_some(metadata.permissions()),
            )
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (false, None),
        Err(error) => return Err(error),
    };

    let parent_guard = private::guard_private_parent(destination).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "guard private parent for {}: {error}",
                destination.display()
            ),
        )
    })?;
    schedule_stale_staged_reap(destination);
    let destination_snapshot = if destination_exists {
        Some(private::capture_destination_dacl(
            destination,
            options.preserve_permissions,
        )?)
    } else {
        None
    };
    if let Some(snapshot) = destination_snapshot.as_ref() {
        if private::preserved_is_encrypted(snapshot) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "refusing to replace encrypted file without preserving EFS: {}",
                    destination.display()
                ),
            ));
        }
        if options.preserve_permissions
            && let Some(permissions) = private::preserved_permissions(snapshot)
        {
            existing_permissions = Some(permissions);
        }
    }
    let (mut staged, staged_path) = create_staged_file(parent, destination).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("create staged file for {}: {error}", destination.display()),
        )
    })?;
    let preparation = (|| {
        staged.write_all(bytes)?;
        staged.flush()?;
        // Publish the replacement only after its final security descriptor
        // and Unix mode are durable on the staged inode. In particular, an
        // executable update must never become visible as the private 0600
        // staging file if power is lost immediately after the rename.
        if options.preserve_permissions
            && let Some(snapshot) = destination_snapshot.as_ref()
        {
            private::apply_preserved_dacl(snapshot, &staged)?;
        }
        if let Some(permissions) = existing_permissions.as_ref() {
            staged.set_permissions(permissions.clone())?;
        } else {
            set_unix_mode(&staged, options.unix_mode)?;
        }
        staged.sync_all()
    })();
    if let Err(error) = preparation {
        private::discard_created_private_file(staged, &staged_path);
        return Err(error);
    }
    let publication = (|| {
        parent_guard.verify()?;
        private::publish_staged_replacement(&parent_guard, &staged, &staged_path, destination)
    })();
    if let Err(error) = publication {
        private::discard_created_private_file(staged, &staged_path);
        return Err(error);
    }
    drop(staged);
    private::sync_guarded_parent(&parent_guard)?;
    Ok(())
}

/// Durably create `destination` without replacing an existing path.
///
/// Returns `Ok(true)` when the file was created and `Ok(false)` when another
/// writer won the race. A hard link publishes the already-synced staged inode
/// atomically on the same filesystem, avoiding a partially written final path.
pub fn atomic_create_new(
    destination: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
) -> io::Result<bool> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("destination has no parent: {}", destination.display()),
        )
    })?;
    private::create_private_parent_dirs(destination)?;
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if options.reject_symlink && metadata_is_link_like(&metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "refusing to use symbolic link as an existing file: {}",
                        destination.display()
                    ),
                ));
            }
            if !metadata.file_type().is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "existing destination is not a regular file: {}",
                        destination.display()
                    ),
                ));
            }
            return Ok(false);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let parent_guard = private::guard_private_parent(destination)?;
    schedule_stale_staged_reap(destination);
    let (mut staged, staged_path) = create_staged_file(parent, destination)?;
    let preparation = (|| {
        staged.write_all(bytes)?;
        staged.flush()?;
        set_unix_mode(&staged, options.unix_mode)?;
        staged.sync_all()
    })();
    if let Err(error) = preparation {
        private::discard_created_private_file(staged, &staged_path);
        return Err(error);
    }
    if let Err(error) = parent_guard.verify() {
        private::discard_created_private_file(staged, &staged_path);
        return Err(error);
    }
    match private::publish_staged_create(&parent_guard, &staged, &staged_path, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            private::discard_created_private_file(staged, &staged_path);
            return Ok(false);
        }
        Err(error) => {
            private::discard_created_private_file(staged, &staged_path);
            return Err(error);
        }
    }
    let same_file = match private::same_file_identity(&staged, destination) {
        Ok(same_file) => same_file,
        Err(error) => {
            private::discard_created_private_file(staged, &staged_path);
            return Err(error);
        }
    };
    if !same_file {
        private::discard_created_private_file(staged, &staged_path);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "created destination does not refer to the staged private file",
        ));
    }
    // `destination` now durably owns the staged content via the hard link.
    // Remove only the exact still-open staged object; neither platform falls
    // back to deleting a possibly swapped path.
    private::discard_created_private_file(staged, &staged_path);
    private::sync_guarded_parent(&parent_guard)?;
    Ok(true)
}

fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn create_staged_file(parent: &Path, destination: &Path) -> io::Result<(File, PathBuf)> {
    let stem = staged_file_stem(destination);
    for _ in 0..128 {
        let nonce = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = parent.join(format!(
            ".{stem}.tmp.{}.{}.{}",
            std::process::id(),
            nanos,
            nonce
        ));
        match create_private_file_new(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not allocate a unique temporary file beside {}",
            destination.display()
        ),
    ))
}

fn staged_file_stem(destination: &Path) -> &str {
    destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state")
}

fn canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn staged_file_creator_pid(name: &str, destination: &Path) -> Option<u32> {
    let prefix = format!(".{}.tmp.", staged_file_stem(destination));
    let suffix = name.strip_prefix(&prefix)?;
    let mut fields = suffix.split('.');
    let pid = fields.next()?;
    let nanos = fields.next()?;
    let sequence = fields.next()?;
    if fields.next().is_some()
        || !canonical_decimal(pid)
        || !canonical_decimal(nanos)
        || !canonical_decimal(sequence)
        || nanos.parse::<u128>().is_err()
        || sequence.parse::<u64>().is_err()
    {
        return None;
    }
    pid.parse::<u32>().ok().filter(|pid| *pid != 0)
}

#[cfg(unix)]
fn process_is_definitely_dead(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs only an existence/permission probe.
    let result = unsafe { libc::kill(pid, 0) };
    result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(windows)]
fn process_is_definitely_dead(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the requested access is query-only and every successful handle
    // is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        // Access-denied and transient query failures are not proof of death.
        return unsafe { GetLastError() } == ERROR_INVALID_PARAMETER;
    }
    let mut exit_code = 0_u32;
    // SAFETY: `process` is live and `exit_code` is writable for the call.
    let queried = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0;
    // SAFETY: OpenProcess transferred this handle to us.
    unsafe { CloseHandle(process) };
    queried && exit_code != STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
fn process_is_definitely_dead(_pid: u32) -> bool {
    false
}

fn reap_stale_staged_files(destination: &Path, parent_guard: &private::PrivateParentGuard) {
    const MAX_SCAN_ATTEMPTS: usize = 512;
    const MAX_REAP: usize = 64;

    let removed =
        private::reap_guarded_children(parent_guard, MAX_SCAN_ATTEMPTS, MAX_REAP, |name| {
            let Some(name) = name.to_str() else {
                return false;
            };
            staged_file_creator_pid(name, destination).is_some_and(process_is_definitely_dead)
        })
        .unwrap_or(0);
    if removed != 0 {
        // Cleanup is best-effort and must not make the requested atomic write
        // fail. The subsequent publication sync still persists this directory
        // mutation on every supported platform.
        let _ = private::sync_guarded_parent(parent_guard);
    }
}

const MAX_TRACKED_REAPER_DESTINATIONS: usize = 256;
const REAPER_QUEUE_CAPACITY: usize = 32;
const REAPER_RESCAN_COOLDOWN: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
struct ReaperSchedule {
    in_flight: std::collections::HashSet<PathBuf>,
    completed: std::collections::VecDeque<(PathBuf, Instant)>,
}

impl ReaperSchedule {
    fn reserve(&mut self, destination: &Path, now: Instant) -> bool {
        while self.completed.front().is_some_and(|(_, completed_at)| {
            now.saturating_duration_since(*completed_at) >= REAPER_RESCAN_COOLDOWN
        }) {
            self.completed.pop_front();
        }
        if self.in_flight.contains(destination)
            || self
                .completed
                .iter()
                .any(|(completed, _)| completed == destination)
        {
            return false;
        }
        while self.in_flight.len() + self.completed.len() >= MAX_TRACKED_REAPER_DESTINATIONS {
            if self.completed.pop_front().is_none() {
                return false;
            }
        }
        self.in_flight.insert(destination.to_path_buf())
    }

    fn complete(&mut self, destination: &Path, now: Instant) {
        if !self.in_flight.remove(destination) {
            return;
        }
        self.completed
            .retain(|(completed, _)| completed != destination);
        self.completed.push_back((destination.to_path_buf(), now));
        while self.in_flight.len() + self.completed.len() > MAX_TRACKED_REAPER_DESTINATIONS {
            self.completed.pop_front();
        }
    }

    fn cancel(&mut self, destination: &Path) {
        self.in_flight.remove(destination);
    }
}

struct ReaperCompletion<'a> {
    schedule: &'a std::sync::Mutex<ReaperSchedule>,
    destination: PathBuf,
}

impl Drop for ReaperCompletion<'_> {
    fn drop(&mut self) {
        if let Ok(mut schedule) = self.schedule.lock() {
            schedule.complete(&self.destination, Instant::now());
        }
    }
}

fn run_stale_staged_reaper(
    receiver: std::sync::mpsc::Receiver<PathBuf>,
    schedule: &std::sync::Mutex<ReaperSchedule>,
) {
    while let Ok(destination) = receiver.recv() {
        let _completion = ReaperCompletion {
            schedule,
            destination: destination.clone(),
        };
        let Ok(parent_guard) = private::guard_private_parent(&destination) else {
            continue;
        };
        reap_stale_staged_files(&destination, &parent_guard);
    }
}

fn schedule_stale_staged_reap(destination: &Path) {
    use std::sync::{Mutex, OnceLock, mpsc};

    static SCHEDULE: OnceLock<Mutex<ReaperSchedule>> = OnceLock::new();
    static REAPER: OnceLock<Mutex<Option<mpsc::SyncSender<PathBuf>>>> = OnceLock::new();

    let destination =
        std::path::absolute(destination).unwrap_or_else(|_| destination.to_path_buf());
    let schedule = SCHEDULE.get_or_init(|| Mutex::new(ReaperSchedule::default()));
    let Ok(mut state) = schedule.lock() else {
        return;
    };
    if !state.reserve(&destination, Instant::now()) {
        return;
    }
    drop(state);

    let sender_slot = REAPER.get_or_init(|| Mutex::new(None));
    let Ok(mut sender_slot) = sender_slot.lock() else {
        if let Ok(mut state) = schedule.lock() {
            state.cancel(&destination);
        }
        return;
    };
    if sender_slot.is_none() {
        let (sender, receiver) = mpsc::sync_channel::<PathBuf>(REAPER_QUEUE_CAPACITY);
        match std::thread::Builder::new()
            .name("kettle-state-reaper".into())
            .spawn(move || run_stale_staged_reaper(receiver, schedule))
        {
            Ok(_) => *sender_slot = Some(sender),
            Err(_) => {
                if let Ok(mut state) = schedule.lock() {
                    state.cancel(&destination);
                }
                return;
            }
        }
    }
    let result = sender_slot
        .as_ref()
        .expect("reaper sender initialized above")
        .try_send(destination);
    if let Err(error) = result {
        // A saturated/disconnected best-effort worker must never delay the
        // foreground state write. Permit a later call to retry this key.
        let disconnected = matches!(&error, mpsc::TrySendError::Disconnected(_));
        let destination = match error {
            mpsc::TrySendError::Full(destination)
            | mpsc::TrySendError::Disconnected(destination) => destination,
        };
        if disconnected {
            *sender_slot = None;
        }
        if let Ok(mut state) = schedule.lock() {
            state.cancel(&destination);
        }
    }
}

/// An exclusive advisory lock released when this value is dropped.
#[derive(Debug)]
pub struct ExclusiveFileLock {
    file: File,
}

impl ExclusiveFileLock {
    /// Block until an exclusive lock on `path` is acquired.
    ///
    /// This blocks indefinitely: a holder that is merely stuck (suspended,
    /// debugger-attached) rather than crashed keeps every other caller
    /// waiting forever with no diagnostic. Prefer [`Self::acquire_timeout`]
    /// for call sites that must surface a stuck lock as an actionable error
    /// instead of an indefinite hang.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        fs4::FileExt::lock(&file)?;
        Ok(Self { file })
    }

    /// Poll for an exclusive lock on `path`, giving up with an
    /// `io::ErrorKind::TimedOut` error once `timeout` elapses instead of
    /// blocking forever.
    pub fn acquire_timeout(path: &Path, timeout: Duration) -> io::Result<Self> {
        poll_with_timeout(path, timeout, Self::try_acquire)
    }

    /// Attempt to acquire an exclusive lock without blocking.
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(Self { file })),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// A shared advisory lock released when this value is dropped.
#[derive(Debug)]
pub struct SharedFileLock {
    file: File,
}

impl SharedFileLock {
    /// Block until a shared lock on `path` is acquired.
    ///
    /// This blocks indefinitely; see [`ExclusiveFileLock::acquire`] for why
    /// [`Self::acquire_timeout`] is preferable at call sites that must not
    /// hang forever behind a stuck exclusive holder.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        fs4::FileExt::lock_shared(&file)?;
        Ok(Self { file })
    }

    /// Poll for a shared lock on `path`, giving up with an
    /// `io::ErrorKind::TimedOut` error once `timeout` elapses instead of
    /// blocking forever.
    pub fn acquire_timeout(path: &Path, timeout: Duration) -> io::Result<Self> {
        poll_with_timeout(path, timeout, Self::try_acquire)
    }

    /// Attempt to acquire a shared lock without blocking.
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match fs4::FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(Some(Self { file })),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for SharedFileLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// Poll `try_once` with capped exponential backoff until it yields a value
/// or `timeout` elapses, in which case a distinct `io::ErrorKind::TimedOut`
/// error is returned. This gives lock call sites a bounded-wait option
/// between `acquire`'s indefinite block and `try_acquire`'s instant failure,
/// so a holder that is stuck rather than crashed surfaces as an actionable
/// error instead of silently wedging every other caller forever.
fn poll_with_timeout<T>(
    path: &Path,
    timeout: Duration,
    mut try_once: impl FnMut(&Path) -> io::Result<Option<T>>,
) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    let mut backoff = Duration::from_millis(1);
    loop {
        if let Some(value) = try_once(path)? {
            return Ok(value);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out after {timeout:?} waiting for a lock on {}",
                    path.display()
                ),
            ));
        }
        std::thread::sleep(backoff.min(deadline - now));
        backoff = (backoff * 2).min(Duration::from_millis(100));
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    open_private_file(path)
}

#[cfg(unix)]
fn set_unix_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_unix_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_command_lock_path_is_a_deterministic_sibling() {
        assert_eq!(
            remote_command_lock_path(Path::new("remote.cmd")),
            PathBuf::from("remote.cmd.lock")
        );
        assert_eq!(
            remote_command_lock_path(Path::new("state/remote")),
            PathBuf::from("state/remote.lock")
        );
        assert_eq!(
            remote_command_lock_path(Path::new("state/a.b.c")),
            PathBuf::from("state/a.b.c.lock")
        );
    }

    fn exited_child_pid() -> u32 {
        for _ in 0..16 {
            #[cfg(windows)]
            let mut child = std::process::Command::new("cmd.exe")
                .args(["/d", "/s", "/c", "exit 0"])
                .spawn()
                .unwrap();
            #[cfg(unix)]
            let mut child = std::process::Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
                .unwrap();
            #[cfg(not(any(unix, windows)))]
            compile_error!("stale staged-file tests require process liveness support");

            let pid = child.id();
            child.wait().unwrap();
            if process_is_definitely_dead(pid) {
                return pid;
            }
        }
        panic!("could not obtain an exited, non-reused child pid");
    }

    #[test]
    fn staged_file_name_parser_is_exact_and_canonical() {
        let destination = Path::new("state.json");
        assert_eq!(
            staged_file_creator_pid(".state.json.tmp.42.123456.7", destination),
            Some(42)
        );
        for invalid in [
            ".other.tmp.42.123456.7",
            ".state.json.tmp.0.123456.7",
            ".state.json.tmp.042.123456.7",
            ".state.json.tmp.42.0123456.7",
            ".state.json.tmp.42.123456.07",
            ".state.json.tmp.42.123456",
            ".state.json.tmp.42.123456.7.extra",
            ".state.json.tmp.-1.123456.7",
            ".state.json.tmp.4294967296.123456.7",
        ] {
            assert_eq!(
                staged_file_creator_pid(invalid, destination),
                None,
                "{invalid}"
            );
        }
    }

    #[test]
    fn reaper_schedule_coalesces_in_flight_and_cools_down_completed_paths() {
        let now = Instant::now();
        let destination = Path::new("state/state.json");
        let mut schedule = ReaperSchedule::default();

        assert!(schedule.reserve(destination, now));
        assert!(!schedule.reserve(destination, now));
        schedule.complete(destination, now);
        assert!(!schedule.reserve(
            destination,
            now + REAPER_RESCAN_COOLDOWN - Duration::from_millis(1)
        ));
        assert!(schedule.reserve(destination, now + REAPER_RESCAN_COOLDOWN));
        schedule.cancel(destination);
        assert!(schedule.reserve(destination, now + REAPER_RESCAN_COOLDOWN));
    }

    #[test]
    fn reaper_schedule_evicts_old_completions_instead_of_saturating_forever() {
        let now = Instant::now();
        let mut schedule = ReaperSchedule::default();
        for index in 0..(MAX_TRACKED_REAPER_DESTINATIONS + 32) {
            let destination = PathBuf::from(format!("state-{index}.json"));
            assert!(
                schedule.reserve(&destination, now),
                "destination {index} was permanently rejected"
            );
            schedule.complete(&destination, now);
        }

        assert_eq!(schedule.completed.len(), MAX_TRACKED_REAPER_DESTINATIONS);
        assert!(
            schedule
                .completed
                .iter()
                .all(|(path, _)| path != Path::new("state-0.json")),
            "the oldest completed key should be evicted"
        );
    }

    #[test]
    fn reaper_worker_completion_guard_clears_in_flight_after_guard_failure() {
        let root = crate::test_tempdir();
        let destination = root.path().join("missing-parent/state.json");
        let schedule = std::sync::Mutex::new(ReaperSchedule::default());
        let now = Instant::now();
        assert!(schedule.lock().unwrap().reserve(&destination, now));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(destination.clone()).unwrap();
        drop(sender);

        run_stale_staged_reaper(receiver, &schedule);

        let state = schedule.lock().unwrap();
        assert!(!state.in_flight.contains(&destination));
        assert!(
            state
                .completed
                .iter()
                .any(|(completed, _)| completed == &destination)
        );
    }

    #[test]
    fn guarded_reaper_removes_only_exact_dead_creator_staged_files() {
        let dir = crate::test_tempdir();
        let destination = dir.path().join("state.json");
        let dead_pid = exited_child_pid();
        let dead = dir.path().join(format!(".state.json.tmp.{dead_pid}.1.0"));
        let live = dir
            .path()
            .join(format!(".state.json.tmp.{}.1.1", std::process::id()));
        let malformed = dir.path().join(format!(".state.json.tmp.0{dead_pid}.1.2"));
        let directory = dir.path().join(format!(".state.json.tmp.{dead_pid}.1.3"));
        let linked_source = dir.path().join("linked-source");
        let linked = dir.path().join(format!(".state.json.tmp.{dead_pid}.1.4"));

        drop(create_private_file_new(&dead).unwrap());
        drop(create_private_file_new(&live).unwrap());
        drop(create_private_file_new(&malformed).unwrap());
        fs::create_dir(&directory).unwrap();
        drop(create_private_file_new(&linked_source).unwrap());
        fs::hard_link(&linked_source, &linked).unwrap();

        let guard = private::guard_private_parent(&destination).unwrap();
        reap_stale_staged_files(&destination, &guard);

        assert!(!dead.exists(), "the exact dead-creator temp is reclaimed");
        assert!(live.is_file(), "a live creator's temp is never touched");
        assert!(malformed.is_file(), "noncanonical names are never touched");
        assert!(
            directory.is_dir(),
            "nonregular candidates are never touched"
        );
        assert!(linked.is_file(), "multi-link candidates are never touched");
    }

    #[test]
    fn guarded_reaper_caps_attempts_even_when_nothing_is_removed() {
        let dir = crate::test_tempdir();
        let destination = dir.path().join("state.json");
        for index in 0..40 {
            drop(create_private_file_new(&dir.path().join(format!("candidate-{index}"))).unwrap());
        }
        let guard = private::guard_private_parent(&destination).unwrap();
        let inspected = std::cell::Cell::new(0usize);
        let removed = private::reap_guarded_children(&guard, 17, 64, |_| {
            inspected.set(inspected.get() + 1);
            false
        })
        .unwrap();
        assert_eq!(removed, 0);
        assert_eq!(
            inspected.get(),
            17,
            "the scan budget counts attempts, not successful removals"
        );
    }

    #[cfg(unix)]
    #[test]
    fn guarded_reaper_never_follows_a_replaced_parent_path() {
        let root = crate::test_tempdir();
        let live_path = root.path().join("state");
        let destination = live_path.join("state.json");
        private::create_private_parent_dirs(&destination).unwrap();
        let guard = private::guard_private_parent(&destination).unwrap();
        let dead_pid = exited_child_pid();
        let name = format!(".state.json.tmp.{dead_pid}.1.0");
        let displaced_candidate = live_path.join(&name);
        drop(create_private_file_new(&displaced_candidate).unwrap());

        let displaced = root.path().join("displaced");
        fs::rename(&live_path, &displaced).unwrap();
        private::create_private_parent_dirs(&destination).unwrap();
        let replacement_candidate = live_path.join(&name);
        drop(create_private_file_new(&replacement_candidate).unwrap());
        let replacement_sentinel = live_path.join("sentinel");
        drop(create_private_file_new(&replacement_sentinel).unwrap());

        reap_stale_staged_files(&destination, &guard);

        assert!(
            !displaced.join(&name).exists(),
            "the held original directory is the only cleanup target"
        );
        assert!(
            replacement_candidate.is_file(),
            "a matching file in a pathname replacement must remain untouched"
        );
        assert!(replacement_sentinel.is_file());
    }

    #[test]
    fn atomically_creates_and_replaces_private_file() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("state.json");
        atomic_replace(&path, b"one", AtomicWriteOptions::PRIVATE).unwrap();
        atomic_replace(&path, b"two", AtomicWriteOptions::PRIVATE).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        {
            let file = open_existing_private_file(&path).unwrap();
            assert!(private::has_current_user_only_dacl(&file).unwrap());
            assert!(private::owned_by_current_user(&file).unwrap());
        }
    }

    #[test]
    fn removes_the_exact_locked_private_file() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("expired.cast");
        let mut created = create_private_file_new(&path).unwrap();
        created.write_all(b"recording").unwrap();
        drop(created);

        let file = open_existing_private_file(&path).unwrap();
        fs4::FileExt::try_lock(&file).unwrap();
        remove_open_private_file(file, &path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn discards_a_private_file_through_its_creation_handle() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("partial");
        let file = create_private_file_new(&path).unwrap();

        discard_created_private_file(file, &path);

        assert!(!path.exists());
    }

    #[test]
    fn refuses_to_remove_an_open_file_through_a_different_path() {
        let dir = crate::test_tempdir();
        let first = dir.path().join("first.cast");
        let second = dir.path().join("second.cast");
        drop(create_private_file_new(&first).unwrap());
        drop(create_private_file_new(&second).unwrap());

        let file = open_existing_private_file(&first).unwrap();
        let error = remove_open_private_file(file, &second).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(first.exists());
        assert!(second.exists());
    }

    #[test]
    fn atomic_create_new_never_clobbers_the_first_writer() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("config.bak");
        assert!(atomic_create_new(&path, b"first", AtomicWriteOptions::PRIVATE).unwrap());
        assert!(!atomic_create_new(&path, b"second", AtomicWriteOptions::PRIVATE).unwrap());
        assert_eq!(fs::read(path).unwrap(), b"first");
    }

    #[test]
    fn atomic_create_new_refuses_an_existing_directory() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("config.bak");
        fs::create_dir(&path).unwrap();

        let error = atomic_create_new(&path, b"data", AtomicWriteOptions::PRIVATE).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(path.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_create_new_refuses_an_existing_symbolic_link() {
        use std::os::unix::fs::symlink;

        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let link = dir.path().join("config.bak");
        fs::write(&target, b"original").unwrap();
        symlink(&target, &link).unwrap();

        let error =
            atomic_create_new(&link, b"replacement", AtomicWriteOptions::PRIVATE).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(target).unwrap(), b"original");
    }

    #[test]
    fn atomic_replace_creates_nested_parent_chain() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("one/two/three/state.json");

        atomic_replace(&path, b"state", AtomicWriteOptions::PRIVATE).unwrap();

        assert_eq!(fs::read(path).unwrap(), b"state");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_replace_accepts_a_held_proc_self_fd_directory() {
        use std::os::fd::AsRawFd as _;

        let dir = crate::test_tempdir();
        let directory = File::open(dir.path()).unwrap();
        let anchored = PathBuf::from(format!(
            "/proc/self/fd/{}/state.json",
            directory.as_raw_fd()
        ));

        atomic_replace(&anchored, b"state", AtomicWriteOptions::PRIVATE).unwrap();

        assert_eq!(fs::read(dir.path().join("state.json")).unwrap(), b"state");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::test_tempdir();
        let path = dir.path().join("config");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_replace(&path, b"new", AtomicWriteOptions::PRESERVE_PERMISSIONS).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(windows)]
    #[test]
    fn preserves_existing_windows_dacl() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("config");
        fs::write(&path, b"old").unwrap();
        let before = private::dacl_signature(&path).unwrap();

        atomic_replace(&path, b"new", AtomicWriteOptions::PRESERVE_PERMISSIONS).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(private::dacl_signature(&path).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_link_destination() {
        use std::os::unix::fs::symlink;
        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::write(&target, b"old").unwrap();
        symlink(&target, &link).unwrap();
        let error = atomic_replace(&link, b"new", AtomicWriteOptions::PRIVATE).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn exclusive_lock_blocks_other_handles_and_releases_on_drop() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("state.lock");
        let first = ExclusiveFileLock::acquire(&path).unwrap();
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn acquire_timeout_returns_immediately_when_uncontended() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("state.lock");
        let started = Instant::now();
        let lock = ExclusiveFileLock::acquire_timeout(&path, Duration::from_secs(5)).unwrap();
        // No holder was contending, so this must not consume any meaningful
        // slice of the generous timeout budget.
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(lock);
    }

    #[test]
    fn acquire_timeout_gives_up_on_a_stuck_holder_instead_of_hanging() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("state.lock");
        let holder = ExclusiveFileLock::acquire(&path).unwrap();

        let error =
            ExclusiveFileLock::acquire_timeout(&path, Duration::from_millis(50)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(holder);
        // The lock is genuinely released once the stuck holder goes away.
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn shared_acquire_timeout_gives_up_behind_an_exclusive_holder() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("state.lock");
        let holder = ExclusiveFileLock::acquire(&path).unwrap();

        let error = SharedFileLock::acquire_timeout(&path, Duration::from_millis(50)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(holder);
    }

    #[test]
    fn shared_locks_coexist_and_block_an_exclusive_lock() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("state.lock");
        let first = SharedFileLock::acquire(&path).unwrap();
        let second = SharedFileLock::try_acquire(&path).unwrap().unwrap();
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_none());
        drop(second);
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn lock_file_refuses_a_directory() {
        let dir = crate::test_tempdir();
        assert!(ExclusiveFileLock::acquire(dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_refuses_a_symbolic_link_without_changing_its_target() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let dir = crate::test_tempdir();
        let target = dir.path().join("do-not-lock");
        let link = dir.path().join("state.lock");
        fs::write(&target, b"sensitive").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &link).unwrap();

        assert!(ExclusiveFileLock::acquire(&link).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"sensitive");
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn atomic_create_new_leaves_no_stray_staged_file_beside_the_destination() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("config.bak");
        assert!(atomic_create_new(&path, b"data", AtomicWriteOptions::PRIVATE).unwrap());
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
    }
}
