//! Shared persistence primitives for Kettle's small local state files.
//!
//! Writes are staged beside their destination, synced, atomically replaced,
//! and followed by a parent-directory sync on platforms that support it.

mod private;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub use private::restrict_private_file;

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
    create_dir_all_durable(parent)?;

    let existing_permissions = match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if options.reject_symlink && metadata.file_type().is_symlink() {
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
            options
                .preserve_permissions
                .then_some(metadata.permissions())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let (mut staged, staged_path) = create_staged_file(parent, destination, options.unix_mode)?;
    let mut cleanup = TempCleanup(Some(staged_path.clone()));
    staged.write_all(bytes)?;
    staged.flush()?;
    if let Some(permissions) = existing_permissions.as_ref() {
        staged.set_permissions(permissions.clone())?;
    } else {
        set_unix_mode(&staged, options.unix_mode)?;
    }
    staged.sync_all()?;
    drop(staged);

    replace_file(&staged_path, destination)?;
    cleanup.0 = None;
    if !options.preserve_permissions || existing_permissions.is_none() {
        restrict_private_path(destination)?;
    }
    sync_parent(parent)?;
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
    create_dir_all_durable(parent)?;
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if options.reject_symlink && metadata.file_type().is_symlink() {
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

    let (mut staged, staged_path) = create_staged_file(parent, destination, options.unix_mode)?;
    let mut cleanup = TempCleanup(Some(staged_path.clone()));
    staged.write_all(bytes)?;
    staged.flush()?;
    set_unix_mode(&staged, options.unix_mode)?;
    staged.sync_all()?;
    drop(staged);

    match fs::hard_link(&staged_path, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    }
    // `destination` now durably owns the staged content via the hard link:
    // the create has already succeeded regardless of what happens to the
    // staged temporary name from here on. Disarm `cleanup` immediately so a
    // transient failure to remove the now-redundant staged link (observed on
    // Windows when antivirus/indexing software briefly holds a just-created
    // file open) cannot make this function report failure for an operation
    // that already durably succeeded, and cannot leave a stray duplicate of
    // (potentially sensitive) state on disk with nothing left to retry it.
    cleanup.0 = None;
    remove_staged_best_effort(&staged_path);
    sync_parent(parent)?;
    Ok(true)
}

/// Best-effort removal of a staged temporary file whose content has already
/// been durably published under its destination (via a hard link). A short,
/// bounded retry loop absorbs transient sharing violations; any failure that
/// survives it is intentionally swallowed; the caller's operation already
/// succeeded and the staged name is now a harmless, otherwise-unreferenced
/// duplicate rather than the only copy of the data.
fn remove_staged_best_effort(path: &Path) {
    const ATTEMPTS: u32 = 5;
    for attempt in 0..ATTEMPTS {
        match fs::remove_file(path) {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) if attempt + 1 < ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(10 * u64::from(attempt + 1)));
            }
            Err(_) => return,
        }
    }
}

fn create_staged_file(
    parent: &Path,
    destination: &Path,
    unix_mode: u32,
) -> io::Result<(File, PathBuf)> {
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
        let mut open = OpenOptions::new();
        open.create_new(true).write(true).read(true);
        set_open_mode(&mut open, unix_mode);
        match open.open(&path) {
            Ok(file) => {
                if let Err(error) = restrict_private_file(&file) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok((file, path));
            }
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
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("lock path has no parent: {}", path.display()),
        )
    })?;
    create_dir_all_durable(parent)?;
    let mut open = OpenOptions::new();
    open.create(true).truncate(false).read(true).write(true);
    set_open_mode(&mut open, 0o600);
    set_lock_open_flags(&mut open);
    let file = open.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("lock path is not a regular file: {}", path.display()),
        ));
    }
    restrict_private_file(&file)?;
    Ok(file)
}

fn restrict_private_path(path: &Path) -> io::Result<()> {
    let mut open = OpenOptions::new();
    open.read(true).write(true);
    set_lock_open_flags(&mut open);
    let file = open.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("private path is not a regular file: {}", path.display()),
        ));
    }
    restrict_private_file(&file)
}

#[cfg(unix)]
fn set_lock_open_flags(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
}

#[cfg(windows)]
fn set_lock_open_flags(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn set_lock_open_flags(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_open_mode(options: &mut OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(mode);
}

#[cfg(not(unix))]
fn set_open_mode(_options: &mut OpenOptions, _mode: u32) {}

#[cfg(unix)]
fn set_unix_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_unix_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_WRITE_THROUGH,
        ReplaceFileW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are owned, NUL-terminated UTF-16 buffers which live
    // for the duration of the call. Optional pointers are null as documented.
    let success = unsafe {
        if destination.exists() {
            ReplaceFileW(
                destination_wide.as_ptr(),
                source.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                source.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

/// Create a missing directory chain and persist every new parent/child edge.
/// `create_dir_all` followed by syncing only the deepest directory is not
/// enough: after a power loss an ancestor entry may disappear while a state
/// file or journal already claims the nested path is durable.
fn create_dir_all_durable(path: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        if cursor.as_os_str().is_empty() {
            cursor = Path::new(".");
        }
        match fs::metadata(cursor) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("parent path is not a directory: {}", cursor.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(cursor.to_path_buf());
                cursor = cursor.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("directory has no existing ancestor: {}", path.display()),
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }

    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::metadata(directory)?.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!(
                            "concurrently created parent is not a directory: {}",
                            directory.display()
                        ),
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        if let Some(parent) = directory.parent() {
            sync_parent(if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomically_creates_and_replaces_private_file() {
        let dir = tempfile::tempdir().unwrap();
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
    }

    #[test]
    fn atomic_create_new_never_clobbers_the_first_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.bak");
        assert!(atomic_create_new(&path, b"first", AtomicWriteOptions::PRIVATE).unwrap());
        assert!(!atomic_create_new(&path, b"second", AtomicWriteOptions::PRIVATE).unwrap());
        assert_eq!(fs::read(path).unwrap(), b"first");
    }

    #[test]
    fn atomic_create_new_refuses_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
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

        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one/two/three/state.json");

        atomic_replace(&path, b"state", AtomicWriteOptions::PRIVATE).unwrap();

        assert_eq!(fs::read(path).unwrap(), b"state");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_replace(&path, b"new", AtomicWriteOptions::PRESERVE_PERMISSIONS).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_link_destination() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.lock");
        let first = ExclusiveFileLock::acquire(&path).unwrap();
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(ExclusiveFileLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn acquire_timeout_returns_immediately_when_uncontended() {
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.lock");
        let holder = ExclusiveFileLock::acquire(&path).unwrap();

        let error = SharedFileLock::acquire_timeout(&path, Duration::from_millis(50)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(holder);
    }

    #[test]
    fn shared_locks_coexist_and_block_an_exclusive_lock() {
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        assert!(ExclusiveFileLock::acquire(dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_refuses_a_symbolic_link_without_changing_its_target() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
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

    #[test]
    fn remove_staged_best_effort_removes_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staged.tmp");
        fs::write(&path, b"redundant once hard-linked to its destination").unwrap();
        remove_staged_best_effort(&path);
        assert!(!path.exists());
    }

    #[test]
    fn remove_staged_best_effort_tolerates_an_already_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-created.tmp");
        // Must return promptly without panicking: the caller's operation
        // already succeeded, so there is nothing left to report.
        remove_staged_best_effort(&path);
    }
}
