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

#[cfg(all(test, windows))]
pub(crate) fn test_tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
    tempfile::Builder::new()
        .prefix("kettle-state-test-")
        .tempdir_in(base)
        .expect("create test directory in the user-private profile")
}

#[cfg(all(test, not(windows)))]
pub(crate) fn test_tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create test directory")
}

pub use private::{
    create_private_file_new, open_existing_private_file, open_private_file,
    open_private_file_append, restrict_private_file,
};

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
    let mut cleanup = TempCleanup(Some(staged_path.clone()));
    let preparation = (|| {
        staged.write_all(bytes)?;
        staged.flush()?;
        staged.sync_all()
    })();
    if let Err(error) = preparation {
        private::discard_created_private_file(staged, &staged_path);
        cleanup.0 = None;
        return Err(error);
    }
    let publication = (|| {
        parent_guard.verify()?;
        private::publish_staged_replacement(&parent_guard, &staged, &staged_path, destination)
    })();
    if let Err(error) = publication {
        private::discard_created_private_file(staged, &staged_path);
        cleanup.0 = None;
        return Err(error);
    }
    cleanup.0 = None;
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
    staged.sync_all()?;
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
    let (mut staged, staged_path) = create_staged_file(parent, destination)?;
    let mut cleanup = TempCleanup(Some(staged_path.clone()));
    let preparation = (|| {
        staged.write_all(bytes)?;
        staged.flush()?;
        set_unix_mode(&staged, options.unix_mode)?;
        staged.sync_all()
    })();
    if let Err(error) = preparation {
        private::discard_created_private_file(staged, &staged_path);
        cleanup.0 = None;
        return Err(error);
    }
    if let Err(error) = parent_guard.verify() {
        private::discard_created_private_file(staged, &staged_path);
        cleanup.0 = None;
        return Err(error);
    }
    match private::publish_staged_create(&parent_guard, &staged, &staged_path, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            private::discard_created_private_file(staged, &staged_path);
            cleanup.0 = None;
            return Ok(false);
        }
        Err(error) => {
            private::discard_created_private_file(staged, &staged_path);
            cleanup.0 = None;
            return Err(error);
        }
    }
    let same_file = match private::same_file_identity(&staged, destination) {
        Ok(same_file) => same_file,
        Err(error) => {
            private::discard_created_private_file(staged, &staged_path);
            cleanup.0 = None;
            return Err(error);
        }
    };
    if !same_file {
        private::discard_created_private_file(staged, &staged_path);
        cleanup.0 = None;
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "created destination does not refer to the staged private file",
        ));
    }
    // `destination` now durably owns the staged content via the hard link.
    // Remove only the exact still-open staged object; neither platform falls
    // back to deleting a possibly swapped path.
    private::discard_created_private_file(staged, &staged_path);
    cleanup.0 = None;
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
    let stem = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
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

struct TempCleanup(Option<PathBuf>);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
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
