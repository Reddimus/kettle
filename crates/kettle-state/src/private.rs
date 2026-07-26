//! Cross-platform owner-only permissions for private files.

use std::fs::File;
use std::io;
use std::path::Path;

/// Create a new private regular file without an inherited-permission window.
///
/// The operation fails when `path` already exists. Unix applies mode `0600`
/// in the creating `open(2)` call. Windows supplies an explicit protected
/// current-user security descriptor to `CreateFileW`.
pub fn create_private_file_new(path: &Path) -> io::Result<File> {
    create_private_file_new_impl(path)
}

/// Open a private regular file for reading and writing, creating it if absent.
///
/// Existing Unix symbolic links and Windows reparse points are rejected.
/// On Windows, an existing file must be owned by the effective user.
pub fn open_private_file(path: &Path) -> io::Result<File> {
    open_private_file_impl(path)
}

/// Open an existing private regular file for reading and writing.
///
/// Unlike [`open_private_file`], this never creates a missing path.
pub fn open_existing_private_file(path: &Path) -> io::Result<File> {
    open_existing_private_file_impl(path)
}

/// Open a private regular file for append, creating it if absent.
///
/// This has the same ownership and reparse-point policy as
/// [`open_private_file`].
pub fn open_private_file_append(path: &Path) -> io::Result<File> {
    open_private_file_append_impl(path)
}

/// Restrict an already-open private file to the effective user.
///
/// Unix applies mode `0600`. Windows replaces the DACL with one protected,
/// full-access ACE for the effective token user, but only when the object is
/// owned by that user. Neither a group-valued token owner nor an exact DACL can
/// substitute for user ownership: a different owner retains implicit authority
/// to rewrite the DACL.
pub fn restrict_private_file(file: &File) -> io::Result<()> {
    restrict_private_object(file)
}

pub(crate) fn discard_created_private_file(file: File, path: &Path) {
    discard_created_private_file_impl(file, path);
}

#[cfg(unix)]
fn create_private_file_new_impl(path: &Path) -> io::Result<File> {
    unix::create_private_file_new(path, false)
}

#[cfg(unix)]
fn open_private_file_impl(path: &Path) -> io::Result<File> {
    match unix::create_private_file_new(path, false) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            unix::open_existing_private_file(path, false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn open_existing_private_file_impl(path: &Path) -> io::Result<File> {
    unix::open_existing_private_file(path, false)
}

#[cfg(unix)]
fn open_private_file_append_impl(path: &Path) -> io::Result<File> {
    match unix::create_private_file_new(path, true) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            unix::open_existing_private_file(path, true)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn create_private_file_new_impl(path: &Path) -> io::Result<File> {
    windows::create_private_file_new(path)
}

#[cfg(windows)]
fn open_private_file_impl(path: &Path) -> io::Result<File> {
    match windows::create_private_file_new(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            windows::open_existing_private_file(path, false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn open_existing_private_file_impl(path: &Path) -> io::Result<File> {
    windows::open_existing_private_file(path, false)
}

#[cfg(windows)]
fn open_private_file_append_impl(path: &Path) -> io::Result<File> {
    match windows::create_private_file_new_append(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            windows::open_existing_private_file(path, true)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_private_file_new_impl(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?;
    require_regular_file(&file, path)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_private_file_impl(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    require_regular_file(&file, path)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_existing_private_file_impl(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    require_regular_file(&file, path)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_private_file_append_impl(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    require_regular_file(&file, path)?;
    Ok(file)
}

#[cfg(not(windows))]
fn require_regular_file(file: &File, path: &Path) -> io::Result<()> {
    if file.metadata()?.file_type().is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("private path is not a regular file: {}", path.display()),
        ))
    }
}

#[cfg(unix)]
fn restrict_private_object(file: &File) -> io::Result<()> {
    unix::restrict_private_object(file)
}

#[cfg(unix)]
fn discard_created_private_file_impl(file: File, path: &Path) {
    unix::discard_created_private_file(file, path);
}

#[cfg(windows)]
fn restrict_private_object(file: &File) -> io::Result<()> {
    windows::restrict_private_object(file)
}

#[cfg(windows)]
fn discard_created_private_file_impl(file: File, _path: &Path) {
    windows::delete_on_close_best_effort(&file);
    drop(file);
}

#[cfg(not(any(unix, windows)))]
fn restrict_private_object(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn discard_created_private_file_impl(file: File, path: &Path) {
    drop(file);
    let _ = std::fs::remove_file(path);
}

#[cfg(windows)]
pub(super) use windows::PrivateParentGuard;

#[cfg(windows)]
pub(super) fn guard_private_parent(path: &Path) -> io::Result<PrivateParentGuard> {
    windows::PrivateParentGuard::new(path)
}

#[cfg(windows)]
pub(super) fn create_private_parent_dirs(path: &Path) -> io::Result<()> {
    windows::create_parent_dirs(path)
}

#[cfg(windows)]
pub(super) use windows::PreservedDacl;

#[cfg(windows)]
pub(super) fn capture_destination_dacl(
    destination: &Path,
    capture_dacl: bool,
) -> io::Result<PreservedDacl> {
    windows::capture_destination_dacl(destination, capture_dacl)
}

#[cfg(windows)]
pub(super) fn apply_preserved_dacl(dacl: &PreservedDacl, staged: &File) -> io::Result<()> {
    windows::apply_preserved_dacl(dacl, staged)
}

#[cfg(windows)]
pub(super) fn preserved_permissions(dacl: &PreservedDacl) -> Option<std::fs::Permissions> {
    Some(windows::preserved_permissions(dacl))
}

#[cfg(windows)]
pub(super) fn preserved_is_encrypted(dacl: &PreservedDacl) -> bool {
    windows::preserved_is_encrypted(dacl)
}

#[cfg(windows)]
pub(super) fn publish_staged_replacement(
    guard: &PrivateParentGuard,
    staged: &File,
    _source: &Path,
    destination: &Path,
) -> io::Result<()> {
    guard.replace_with_open_file(staged, destination)
}

#[cfg(windows)]
pub(super) fn publish_staged_create(
    guard: &PrivateParentGuard,
    staged: &File,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    guard.link_open_file_new(staged, source, destination)
}

#[cfg(windows)]
pub(super) fn sync_guarded_parent(_guard: &PrivateParentGuard) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub(super) fn same_file_identity(file: &File, path: &Path) -> io::Result<bool> {
    windows::same_file_identity(file, path)
}

#[cfg(all(test, windows))]
pub(super) fn has_current_user_only_dacl(file: &File) -> io::Result<bool> {
    windows::has_current_user_only_dacl(file)
}

#[cfg(all(test, windows))]
pub(super) fn owned_by_current_user(file: &File) -> io::Result<bool> {
    windows::owned_by_current_user(file)
}

#[cfg(all(test, windows))]
pub(super) fn dacl_signature(path: &Path) -> io::Result<(Option<Vec<u8>>, bool)> {
    windows::dacl_signature(path)
}

#[cfg(unix)]
pub(super) use unix::PrivateParentGuard;

#[cfg(unix)]
pub(super) fn guard_private_parent(path: &Path) -> io::Result<PrivateParentGuard> {
    unix::PrivateParentGuard::new(path)
}

#[cfg(unix)]
pub(super) fn create_private_parent_dirs(path: &Path) -> io::Result<()> {
    unix::create_parent_dirs(path)
}

#[cfg(unix)]
pub(super) fn publish_staged_replacement(
    guard: &PrivateParentGuard,
    staged: &File,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    guard.replace_with_open_file(staged, source, destination)
}

#[cfg(unix)]
pub(super) fn publish_staged_create(
    guard: &PrivateParentGuard,
    staged: &File,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    guard.link_open_file_new(staged, source, destination)
}

#[cfg(unix)]
pub(super) fn sync_guarded_parent(guard: &PrivateParentGuard) -> io::Result<()> {
    guard.sync()
}

#[cfg(unix)]
pub(super) fn same_file_identity(file: &File, path: &Path) -> io::Result<bool> {
    unix::same_file_identity(file, path)
}

#[cfg(not(windows))]
pub(super) struct PreservedDacl;

#[cfg(not(windows))]
pub(super) fn capture_destination_dacl(
    _destination: &Path,
    _capture_dacl: bool,
) -> io::Result<PreservedDacl> {
    Ok(PreservedDacl)
}

#[cfg(not(windows))]
pub(super) fn apply_preserved_dacl(_dacl: &PreservedDacl, _staged: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn preserved_permissions(_dacl: &PreservedDacl) -> Option<std::fs::Permissions> {
    None
}

#[cfg(not(windows))]
pub(super) fn preserved_is_encrypted(_dacl: &PreservedDacl) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
pub(super) struct PrivateParentGuard;

#[cfg(not(any(unix, windows)))]
impl PrivateParentGuard {
    pub(super) fn verify(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn guard_private_parent(_path: &Path) -> io::Result<PrivateParentGuard> {
    Ok(PrivateParentGuard)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn create_private_parent_dirs(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("private path has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn publish_staged_replacement(
    _guard: &PrivateParentGuard,
    _staged: &File,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn publish_staged_create(
    _guard: &PrivateParentGuard,
    _staged: &File,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    std::fs::hard_link(source, destination)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn sync_guarded_parent(_guard: &PrivateParentGuard) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn same_file_identity(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::ffi::{CString, OsStr, OsString};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Component, PathBuf};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
    }

    impl FileIdentity {
        fn from_metadata(metadata: &std::fs::Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }

        #[allow(clippy::unnecessary_cast)]
        fn from_stat(stat: &libc::stat) -> Self {
            Self {
                device: stat.st_dev as u64,
                inode: stat.st_ino as u64,
            }
        }
    }

    fn current_user() -> u32 {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    fn c_name(name: &OsStr, description: &str) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{description} contains an embedded NUL"),
            )
        })
    }

    fn normalized_absolute(path: &Path) -> io::Result<PathBuf> {
        let absolute = std::path::absolute(path)?;
        let mut names = Vec::<OsString>::new();
        for component in absolute.components() {
            match component {
                Component::RootDir => names.clear(),
                Component::CurDir => {}
                Component::ParentDir => {
                    names.pop();
                }
                Component::Normal(name) => names.push(name.to_os_string()),
                Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Unix private path contains a platform prefix",
                    ));
                }
            }
        }
        let mut normalized = PathBuf::from("/");
        normalized.extend(names);
        Ok(normalized)
    }

    fn reject_parent_components(path: &Path) -> io::Result<()> {
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private path must not contain a parent-directory component: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn is_proc_self_fd_directory_link(
        path: &Path,
        link: &std::fs::Metadata,
        parent: &std::fs::Metadata,
    ) -> bool {
        let mut components = path.components().filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        });
        let exact_shape = components.next() == Some(OsStr::new("proc"))
            && components.next() == Some(OsStr::new("self"))
            && components.next() == Some(OsStr::new("fd"))
            && components
                .next()
                .is_some_and(|fd| !fd.is_empty() && fd.as_bytes().iter().all(u8::is_ascii_digit))
            && components.next().is_none();
        exact_shape
            && link.uid() == current_user()
            && parent.uid() == current_user()
            && parent.mode() & 0o022 == 0
    }

    fn validate_symlink_ancestors(path: &Path) -> io::Result<()> {
        let absolute = std::path::absolute(path)?;
        let mut cursor = PathBuf::from("/");
        let mut parent_metadata = std::fs::metadata("/")?;
        for component in absolute.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            cursor.push(name);
            match std::fs::symlink_metadata(&cursor) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    // Permit only platform-managed root symlinks in a
                    // root-owned, non-group/world-writable directory (for
                    // example macOS /tmp and /var). User-controlled ancestor
                    // symlinks are rejected before any mutation.
                    let root_managed = metadata.uid() == 0
                        && parent_metadata.uid() == 0
                        && parent_metadata.mode() & 0o022 == 0;
                    #[cfg(target_os = "linux")]
                    let held_self_fd =
                        is_proc_self_fd_directory_link(&cursor, &metadata, &parent_metadata);
                    #[cfg(not(target_os = "linux"))]
                    let held_self_fd = false;
                    if !root_managed && !held_self_fd {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "private path contains an untrusted symbolic-link ancestor: {}",
                                cursor.display()
                            ),
                        ));
                    }
                    // `/proc/self/fd/<n>` is a kernel-managed capability to an
                    // already-open object, not a filesystem-controlled
                    // redirection. Canonicalization below resolves it once;
                    // the resulting real path is then reopened and every
                    // directory edge is checked normally.
                    let target = std::fs::metadata(&cursor)?;
                    if !target.file_type().is_dir() || !trusted_identity(target.uid()) {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "platform symbolic-link ancestor has an untrusted target: {}",
                                cursor.display()
                            ),
                        ));
                    }
                    parent_metadata = target;
                }
                Ok(metadata) if metadata.file_type().is_dir() => {
                    require_trusted_directory_edge(&parent_metadata, &metadata, &cursor)?;
                    parent_metadata = metadata;
                }
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!(
                            "private parent component is not a directory: {}",
                            cursor.display()
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn split_path(path: &Path) -> io::Result<(PathBuf, OsString)> {
        reject_parent_components(path)?;
        let path = std::path::absolute(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private path has no file name: {}", path.display()),
                )
            })?
            .to_os_string();
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("private path has no parent: {}", path.display()),
            )
        })?;
        validate_symlink_ancestors(parent)?;
        // Resolve legitimate platform-managed symlinks such as macOS
        // `/tmp -> private/tmp` once, then perform every security-sensitive
        // operation relative to the verified canonical directory handle.
        let parent = std::fs::canonicalize(parent)?;
        Ok((normalized_absolute(&parent)?, name))
    }

    fn open_root() -> io::Result<File> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let access = libc::O_PATH;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let access = libc::O_RDONLY;
        let root = c_name(OsStr::new("/"), "filesystem root")?;
        // SAFETY: the root name is NUL-terminated and a successful open
        // transfers ownership of the returned fd.
        let fd = unsafe {
            libc::open(
                root.as_ptr(),
                access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: open returned a new owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
        let name = c_name(name, "private directory name")?;
        // SAFETY: `parent` is a live directory fd, `name` is NUL-terminated,
        // and successful openat transfers ownership of the returned fd.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let access = libc::O_PATH;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let access = libc::O_RDONLY;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn trusted_identity(uid: u32) -> bool {
        uid == current_user() || uid == 0
    }

    fn require_trusted_directory_edge(
        parent: &std::fs::Metadata,
        child: &std::fs::Metadata,
        path: &Path,
    ) -> io::Result<()> {
        let parent_writable_by_others = parent.mode() & 0o022 != 0;
        let sticky = parent.mode() & 0o1000 != 0;
        if trusted_identity(parent.uid())
            && (!parent_writable_by_others || (sticky && trusted_identity(child.uid())))
        {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private path crosses an untrusted writable directory edge: {}",
                path.display()
            ),
        ))
    }

    fn require_trusted_leaf_creation(parent: &std::fs::Metadata, path: &Path) -> io::Result<()> {
        let writable_by_others = parent.mode() & 0o022 != 0;
        let sticky = parent.mode() & 0o1000 != 0;
        if trusted_identity(parent.uid()) && (!writable_by_others || sticky) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to create a private directory beneath an untrusted writable parent: {}",
                path.display()
            ),
        ))
    }

    fn sync_directory(directory: &File) -> io::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let current = c_name(OsStr::new("."), "current directory")?;
            // SAFETY: the held directory fd and name are valid.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    current.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openat returned a new owned fd.
            unsafe { File::from_raw_fd(fd) }.sync_all()
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        directory.sync_all()
    }

    pub(super) fn create_parent_dirs(file_path: &Path) -> io::Result<()> {
        reject_parent_components(file_path)?;
        let absolute = std::path::absolute(file_path)?;
        let requested_parent = absolute.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("private path has no parent: {}", absolute.display()),
            )
        })?;
        validate_symlink_ancestors(requested_parent)?;
        if std::fs::canonicalize(requested_parent).is_ok() {
            return Ok(());
        }

        let mut existing = requested_parent;
        let mut missing = Vec::<OsString>::new();
        let canonical_existing = loop {
            match std::fs::canonicalize(existing) {
                Ok(path) => break normalized_absolute(&path)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let name = existing.file_name().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!(
                                "private parent has no existing ancestor: {}",
                                requested_parent.display()
                            ),
                        )
                    })?;
                    missing.push(name.to_os_string());
                    existing = existing.parent().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            "private parent has no existing ancestor",
                        )
                    })?;
                }
                Err(error) => return Err(error),
            }
        };
        let (mut handles, _) = open_verified_parent_chain(&canonical_existing)?;
        let mut cursor = canonical_existing;
        for name in missing.iter().rev() {
            let parent = handles
                .last()
                .expect("the verified chain always contains a directory");
            cursor.push(name);
            require_trusted_leaf_creation(&parent.metadata()?, &cursor)?;
            let name_c = c_name(name, "private directory name")?;
            // mkdir mode is only reduced by umask, never broadened. The new
            // child is opened and restored to exact 0700 before use.
            // SAFETY: the held parent fd and NUL-terminated child name are valid.
            let created = unsafe {
                libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700 as libc::mode_t)
            } == 0;
            if !created {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            let child = open_directory_at(parent, name)?;
            let child_metadata = child.metadata()?;
            require_trusted_directory_edge(&parent.metadata()?, &child_metadata, &cursor)?;
            if created {
                if child_metadata.uid() != current_user() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "new private directory has the wrong owner: {}",
                            cursor.display()
                        ),
                    ));
                }
                // SAFETY: the parent directory is held and trusted, and the
                // just-created child name is NUL-terminated.
                if unsafe { libc::fchmodat(parent.as_raw_fd(), name_c.as_ptr(), 0o700, 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                if child.metadata()?.mode() & 0o777 != 0o700 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "new private directory mode verification failed: {}",
                            cursor.display()
                        ),
                    ));
                }
                sync_directory(parent)?;
            }
            handles.push(child);
        }
        Ok(())
    }

    fn open_verified_parent_chain(path: &Path) -> io::Result<(Vec<File>, Vec<FileIdentity>)> {
        let path = normalized_absolute(path)?;
        let mut handles = Vec::new();
        let mut identities = Vec::new();
        let root = open_root()?;
        let root_metadata = root.metadata()?;
        if !root_metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem root is not a directory",
            ));
        }
        identities.push(FileIdentity::from_metadata(&root_metadata));
        handles.push(root);

        let mut cursor = PathBuf::from("/");
        for component in path.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let parent = handles
                .last()
                .expect("the verified chain always contains the root");
            let child = open_directory_at(parent, name)?;
            let parent_metadata = parent.metadata()?;
            let child_metadata = child.metadata()?;
            cursor.push(name);
            if !child_metadata.file_type().is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private parent is not a directory: {}", cursor.display()),
                ));
            }
            require_trusted_directory_edge(&parent_metadata, &child_metadata, &cursor)?;
            identities.push(FileIdentity::from_metadata(&child_metadata));
            handles.push(child);
        }
        Ok((handles, identities))
    }

    fn entry_stat(parent: &File, name: &OsStr) -> io::Result<Option<libc::stat>> {
        let name = c_name(name, "private file name")?;
        // SAFETY: a zeroed stat is a valid output buffer and all input
        // pointers/fds are valid for fstatat.
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                std::ptr::addr_of_mut!(stat),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(stat));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        }
    }

    fn stat_is_regular(stat: &libc::stat) -> bool {
        stat.st_mode & libc::S_IFMT == libc::S_IFREG
    }

    pub(crate) struct PrivateParentGuard {
        path: PathBuf,
        leaf: OsString,
        identities: Vec<FileIdentity>,
        handles: Vec<File>,
    }

    impl PrivateParentGuard {
        pub(super) fn new(path: &Path) -> io::Result<Self> {
            let (parent, leaf) = split_path(path)?;
            let (handles, identities) = open_verified_parent_chain(&parent)?;
            Ok(Self {
                path: parent,
                leaf,
                identities,
                handles,
            })
        }

        fn directory(&self) -> &File {
            self.handles
                .last()
                .expect("a parent guard always contains its immediate parent")
        }

        fn validate_path(&self, path: &Path) -> io::Result<OsString> {
            let (parent, leaf) = split_path(path)?;
            if parent == self.path {
                Ok(leaf)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "private file does not belong to the guarded parent",
                ))
            }
        }

        fn require_leaf_policy(&self, leaf_uid: u32) -> io::Result<()> {
            let parent = self.directory().metadata()?;
            let writable_by_others = parent.mode() & 0o022 != 0;
            let sticky = parent.mode() & 0o1000 != 0;
            if trusted_identity(parent.uid())
                && (!writable_by_others || (sticky && trusted_identity(leaf_uid)))
            {
                return Ok(());
            }
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private parent is writable by an untrusted principal: {} (mode {:o})",
                    self.path.display(),
                    parent.mode() & 0o7777
                ),
            ))
        }

        pub(crate) fn verify(&self) -> io::Result<()> {
            let (handles, identities) = open_verified_parent_chain(&self.path)?;
            drop(handles);
            if identities == self.identities {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "private parent changed while it was in use: {}",
                        self.path.display()
                    ),
                ))
            }
        }

        fn entry_matches(&self, file: &File, name: &OsStr) -> io::Result<bool> {
            let expected = FileIdentity::from_metadata(&file.metadata()?);
            Ok(entry_stat(self.directory(), name)?
                .is_some_and(|stat| FileIdentity::from_stat(&stat) == expected))
        }

        fn unlink_if_same(&self, file: &File, name: &OsStr) {
            if !self.entry_matches(file, name).unwrap_or(false) {
                return;
            }
            let Ok(name) = c_name(name, "private file name") else {
                return;
            };
            // SAFETY: the directory fd and NUL-terminated name are valid.
            unsafe { libc::unlinkat(self.directory().as_raw_fd(), name.as_ptr(), 0) };
        }

        pub(super) fn replace_with_open_file(
            &self,
            staged: &File,
            source: &Path,
            destination: &Path,
        ) -> io::Result<()> {
            let source = self.validate_path(source)?;
            let destination = self.validate_path(destination)?;
            if destination != self.leaf {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "replacement does not use the guarded destination",
                ));
            }
            self.verify()?;
            if !self.entry_matches(staged, &source)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "staged path no longer refers to the open private file",
                ));
            }
            let staged_metadata = staged.metadata()?;
            require_user_owned_regular(staged, source.as_ref())?;
            self.require_leaf_policy(staged_metadata.uid())?;
            if let Some(existing) = entry_stat(self.directory(), &destination)? {
                if !stat_is_regular(&existing) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "replacement destination is not a regular file",
                    ));
                }
                self.require_leaf_policy(existing.st_uid)?;
            } else {
                self.require_leaf_policy(current_user())?;
            }
            let source = c_name(&source, "staged file name")?;
            let destination = c_name(&destination, "destination file name")?;
            // SAFETY: both names are relative to the same verified live
            // directory fd. renameat atomically publishes the staged inode.
            if unsafe {
                libc::renameat(
                    self.directory().as_raw_fd(),
                    source.as_ptr(),
                    self.directory().as_raw_fd(),
                    destination.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn link_open_file_new(
            &self,
            staged: &File,
            source: &Path,
            destination: &Path,
        ) -> io::Result<()> {
            let source = self.validate_path(source)?;
            let destination = self.validate_path(destination)?;
            self.verify()?;
            if !self.entry_matches(staged, &source)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "staged path no longer refers to the open private file",
                ));
            }
            let staged_metadata = staged.metadata()?;
            require_user_owned_regular(staged, source.as_ref())?;
            self.require_leaf_policy(staged_metadata.uid())?;
            self.require_leaf_policy(current_user())?;
            let source = c_name(&source, "staged file name")?;
            let destination = c_name(&destination, "destination file name")?;
            // SAFETY: both names and the verified directory fd are valid.
            if unsafe {
                libc::linkat(
                    self.directory().as_raw_fd(),
                    source.as_ptr(),
                    self.directory().as_raw_fd(),
                    destination.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn sync(&self) -> io::Result<()> {
            sync_directory(self.directory())
        }
    }

    fn open_at(
        guard: &PrivateParentGuard,
        path: &Path,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<File> {
        let name = guard.validate_path(path)?;
        let name = c_name(&name, "private file name")?;
        // SAFETY: the verified directory fd and NUL-terminated name are valid;
        // successful openat transfers ownership of the returned fd.
        let fd = unsafe {
            libc::openat(
                guard.directory().as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                // C's default variadic argument promotions require an integer
                // at least as wide as `c_uint`; Darwin's `mode_t` is `u16`.
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn require_user_owned_regular(file: &File, path: &Path) -> io::Result<()> {
        require_regular_file(file, path)?;
        let metadata = file.metadata()?;
        if metadata.uid() != current_user() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private file is not owned by the effective user: {}",
                    path.display()
                ),
            ));
        }
        if metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private file has an unexpected hard-link count: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn restrict_private_object(file: &File) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        require_regular_file(file, Path::new("<open private file>"))?;
        let metadata = file.metadata()?;
        if metadata.uid() != current_user() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to harden a private file not owned by the effective user",
            ));
        }
        if metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to harden a private file with multiple hard links",
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        if file.metadata()?.mode() & 0o777 == 0o600 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private file mode verification failed after fchmod",
            ))
        }
    }

    pub(super) fn create_private_file_new(path: &Path, append: bool) -> io::Result<File> {
        create_parent_dirs(path)?;
        let guard = PrivateParentGuard::new(path)?;
        guard.require_leaf_policy(current_user())?;
        let access = if append {
            libc::O_WRONLY | libc::O_APPEND
        } else {
            libc::O_RDWR
        };
        let file = open_at(&guard, path, access | libc::O_CREAT | libc::O_EXCL, 0o600)?;
        let result = (|| {
            require_user_owned_regular(&file, path)?;
            restrict_private_object(&file)?;
            guard.verify()?;
            if !guard.entry_matches(&file, &guard.leaf)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "new private path no longer refers to the created file",
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            guard.unlink_if_same(&file, &guard.leaf);
            return Err(error);
        }
        Ok(file)
    }

    pub(super) fn open_existing_private_file(path: &Path, append: bool) -> io::Result<File> {
        let guard = PrivateParentGuard::new(path)?;
        let access = if append {
            libc::O_WRONLY | libc::O_APPEND
        } else {
            libc::O_RDWR
        };
        let file = open_at(&guard, path, access, 0)?;
        require_user_owned_regular(&file, path)?;
        guard.require_leaf_policy(file.metadata()?.uid())?;
        restrict_private_object(&file)?;
        guard.verify()?;
        if !guard.entry_matches(&file, &guard.leaf)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private path changed while it was opened",
            ));
        }
        Ok(file)
    }

    pub(super) fn discard_created_private_file(file: File, path: &Path) {
        if let Ok(guard) = PrivateParentGuard::new(path)
            && let Ok(name) = guard.validate_path(path)
        {
            guard.unlink_if_same(&file, &name);
        }
        drop(file);
    }

    pub(super) fn same_file_identity(file: &File, path: &Path) -> io::Result<bool> {
        let guard = PrivateParentGuard::new(path)?;
        let name = guard.validate_path(path)?;
        let Some(stat) = entry_stat(guard.directory(), &name)? else {
            return Ok(false);
        };
        if !stat_is_regular(&stat) {
            return Ok(false);
        }
        guard.verify()?;
        Ok(FileIdentity::from_metadata(&file.metadata()?) == FileIdentity::from_stat(&stat))
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
    use std::path::{Component, PathBuf};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_NO_TOKEN, ERROR_PATH_NOT_FOUND, FILETIME,
        GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, CreateWellKnownSid,
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, INHERIT_ONLY_ACE, InitializeAcl, InitializeSecurityDescriptor,
        IsValidAcl, IsValidSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        SECURITY_MAX_SID_SIZE, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
        UNPROTECTED_DACL_SECURITY_INFORMATION, WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid,
        WinLocalSystemSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE,
        FILE_ALL_ACCESS, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
        FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FileDispositionInfo,
        FileIdInfo, FileRenameInfoEx, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL, ReOpenFile,
        SetFileInformationByHandle, VOLUME_NAME_GUID, WRITE_DAC, WRITE_OWNER,
    };
    use windows_sys::Win32::System::SystemServices::{
        ACCESS_ALLOWED_ACE_TYPE, ACCESS_ALLOWED_CALLBACK_ACE_TYPE,
        ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE, ACCESS_ALLOWED_COMPOUND_ACE_TYPE,
        ACCESS_ALLOWED_OBJECT_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS,
    };

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns the handle returned by a token-open API.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn effective_token() -> io::Result<TokenHandle> {
        let mut token = std::ptr::null_mut();
        // Prefer the effective thread token so an impersonating caller creates
        // and validates objects as the identity the kernel applies to I/O.
        // SAFETY: the pseudo-handle and output pointer are valid.
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } != 0 {
            return Ok(TokenHandle(token));
        }
        // SAFETY: GetLastError reads this thread's immediately preceding error.
        if unsafe { GetLastError() } != ERROR_NO_TOKEN {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle and `token`
        // points to writable storage for the returned real token handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(TokenHandle(token))
    }

    struct TokenUserSid {
        _buffer: Vec<u64>,
        sid: PSID,
    }

    fn current_user_sid() -> io::Result<TokenUserSid> {
        let token = effective_token()?;
        let mut len = 0_u32;
        // SAFETY: the null-buffer call is the documented size query.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buffer = vec![0_u64; words];
        // SAFETY: `buffer` is aligned and at least `len` bytes long.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful GetTokenInformation populated a TOKEN_USER header
        // whose SID points into `buffer`, retained by TokenUserSid.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the effective token has an invalid user SID",
            ));
        }
        Ok(TokenUserSid {
            _buffer: buffer,
            sid,
        })
    }

    struct WellKnownSid {
        _buffer: Vec<u64>,
        sid: PSID,
    }

    fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<WellKnownSid> {
        let words = (SECURITY_MAX_SID_SIZE as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buffer = vec![0_u64; words];
        let mut len = u32::try_from(buffer.len() * std::mem::size_of::<u64>())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SID buffer is too large"))?;
        // SAFETY: the aligned buffer is writable for `len` bytes.
        if unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = buffer.as_mut_ptr().cast();
        Ok(WellKnownSid {
            _buffer: buffer,
            sid,
        })
    }

    fn builtin_administrators_sid() -> io::Result<WellKnownSid> {
        well_known_sid(WinBuiltinAdministratorsSid)
    }

    fn private_acl(current_user: PSID) -> io::Result<Vec<u64>> {
        // One ACCESS_ALLOWED_ACE has a four-byte SID placeholder. Add the
        // actual variable-length SID and round storage up to u64 alignment.
        let sid_len = unsafe { GetLengthSid(current_user) } as usize;
        let acl_len = std::mem::size_of::<ACL>()
            .checked_add(std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>())
            .and_then(|base| base.checked_add(sid_len))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ACL size overflow"))?;
        let words = acl_len.div_ceil(std::mem::size_of::<u64>());
        let mut storage = vec![0_u64; words];
        let acl = storage.as_mut_ptr().cast::<ACL>();
        let capacity = u32::try_from(storage.len() * std::mem::size_of::<u64>())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ACL is too large"))?;
        // SAFETY: storage is aligned, writable, and `capacity` bytes.
        if unsafe { InitializeAcl(acl, capacity, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the ACL has enough room and the validated SID remains alive.
        if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, current_user) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(storage)
    }

    fn private_security_descriptor(
        current_user: PSID,
        acl: *mut ACL,
    ) -> io::Result<SECURITY_DESCRIPTOR> {
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        // SAFETY: `descriptor` is writable for every initialization call; the
        // SID and ACL outlive the CreateFileW call that consumes this value.
        if unsafe {
            InitializeSecurityDescriptor(
                std::ptr::addr_of_mut!(descriptor).cast(),
                SECURITY_DESCRIPTOR_REVISION,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetSecurityDescriptorOwner(std::ptr::addr_of_mut!(descriptor).cast(), current_user, 0)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetSecurityDescriptorDacl(std::ptr::addr_of_mut!(descriptor).cast(), 1, acl, 0)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetSecurityDescriptorControl(
                std::ptr::addr_of_mut!(descriptor).cast(),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(descriptor)
    }

    fn private_component(name: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
        let child = name.encode_wide().collect::<Vec<_>>();
        if child.is_empty()
            || child.contains(&0)
            || child.contains(&(':' as u16))
            || child.ends_with(&['.' as u16])
            || child.ends_with(&[' ' as u16])
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private path component is empty, aliases Win32 syntax, or contains an embedded NUL",
            ));
        }
        Ok(child)
    }

    fn stable_child_path(parent: &File, name: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
        // A volume-GUID path is derived from the already-held parent object,
        // avoiding a second resolution through a mutable drive-letter mapping.
        // The held ancestor chain is opened without FILE_SHARE_DELETE, so the
        // returned parent path cannot be renamed before publication.
        fn query(parent: &File, flags: u32) -> io::Result<Vec<u16>> {
            let mut capacity = 256_u32;
            loop {
                let mut path = vec![0_u16; capacity as usize];
                // SAFETY: `parent` owns a valid handle and the output buffer
                // has `capacity` writable UTF-16 code units.
                let len = unsafe {
                    GetFinalPathNameByHandleW(
                        parent.as_raw_handle() as HANDLE,
                        path.as_mut_ptr(),
                        capacity,
                        flags,
                    )
                };
                if len == 0 {
                    return Err(io::Error::last_os_error());
                }
                if len < capacity {
                    path.truncate(len as usize);
                    return Ok(path);
                }
                capacity = len.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "final parent path is too long")
                })?;
            }
        }

        let mut parent_path = match query(parent, VOLUME_NAME_GUID) {
            Ok(path) => path,
            Err(error) if error.raw_os_error() == Some(ERROR_PATH_NOT_FOUND as i32) => {
                // SMB/UNC shares do not have volume-GUID names. Their opened
                // final DOS/UNC path is the documented network fallback.
                query(parent, 0)?
            }
            Err(error) => return Err(error),
        };
        if !parent_path.ends_with(&['\\' as u16]) {
            parent_path.push('\\' as u16);
        }
        let child = private_component(name)?;
        parent_path.extend(child);
        Ok(parent_path)
    }

    fn reject_parent_components(path: &Path) -> io::Result<()> {
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "private path must not contain a parent-directory component: {}",
                            path.display()
                        ),
                    ));
                }
                Component::Normal(name) => {
                    private_component(name)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
        use std::os::windows::fs::MetadataExt as _;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    pub(super) fn create_parent_dirs(file_path: &Path) -> io::Result<()> {
        reject_parent_components(file_path)?;
        let absolute = std::path::absolute(file_path)?;
        let requested_parent = absolute.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("private path has no parent: {}", absolute.display()),
            )
        })?;
        let mut existing = requested_parent;
        let mut missing = Vec::new();
        loop {
            match std::fs::symlink_metadata(existing) {
                Ok(metadata) if metadata_is_reparse(&metadata) => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("private parent is a reparse point: {}", existing.display()),
                    ));
                }
                Ok(metadata) if metadata.is_dir() => break,
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!(
                            "private parent component is not a directory: {}",
                            existing.display()
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let name = existing.file_name().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!(
                                "private parent has no existing ancestor: {}",
                                requested_parent.display()
                            ),
                        )
                    })?;
                    missing.push(name.to_os_string());
                    existing = existing.parent().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            "private parent has no existing ancestor",
                        )
                    })?;
                }
                Err(error) => return Err(error),
            }
        }

        let (mut handles, _) = open_verified_parent_chain(existing).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "open existing private parent chain {}: {error}",
                    existing.display()
                ),
            )
        })?;
        let current_user = current_user_sid().map_err(|error| {
            io::Error::new(error.kind(), format!("query effective user SID: {error}"))
        })?;
        let mut acl = private_acl(current_user.sid)
            .map_err(|error| io::Error::new(error.kind(), format!("build private ACL: {error}")))?;
        let mut descriptor = private_security_descriptor(current_user.sid, acl.as_mut_ptr().cast())
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("build private directory security descriptor: {error}"),
                )
            })?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::addr_of_mut!(descriptor).cast(),
            bInheritHandle: 0,
        };
        for name in missing.iter().rev() {
            let parent = handles
                .last()
                .expect("the verified chain always contains an existing parent");
            let child_path = stable_child_path(parent, name)?;
            // SAFETY: the volume-GUID path is NUL-terminated below and the
            // descriptor storage remains alive for the call.
            let mut child_path_z = child_path;
            child_path_z.push(0);
            let created = unsafe { CreateDirectoryW(child_path_z.as_ptr(), &attributes) } != 0;
            if !created {
                // SAFETY: GetLastError reads this thread's immediately
                // preceding CreateDirectoryW failure.
                let error = unsafe { GetLastError() };
                if error != ERROR_ALREADY_EXISTS {
                    return Err(io::Error::other(format!(
                        "create private directory {}: {}",
                        String::from_utf16_lossy(&child_path_z[..child_path_z.len() - 1]),
                        io::Error::from_raw_os_error(error as i32)
                    )));
                }
            }
            let child_os = std::ffi::OsString::from_wide(
                child_path_z
                    .strip_suffix(&[0])
                    .expect("the child path was just NUL-terminated"),
            );
            let (child, _) = open_non_reparse_directory(Path::new(&child_os)).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "open newly created private directory {}: {error}",
                        Path::new(&child_os).display()
                    ),
                )
            })?;
            if created {
                let valid_security =
                    owned_by_user_handle_direct(&child, current_user.sid).map_err(|error| {
                        io::Error::new(
                            error.kind(),
                            format!("verify new private directory owner: {error}"),
                        )
                    })? && has_current_user_only_dacl_direct(&child, current_user.sid).map_err(
                        |error| {
                            io::Error::new(
                                error.kind(),
                                format!("verify new private directory DACL: {error}"),
                            )
                        },
                    )?;
                if !valid_security {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "new private directory failed owner/DACL verification: {}",
                            Path::new(&child_os).display()
                        ),
                    ));
                }
            } else {
                // `ERROR_ALREADY_EXISTS` can be a concurrent winner rather
                // than an entry observed during the initial parent scan.
                // Apply the same trust policy used for every pre-existing
                // ancestor before accepting it as the parent of private data.
                let administrators = builtin_administrators_sid()?;
                let system = well_known_sid(WinLocalSystemSid)?;
                require_trusted_directory_security(
                    &child,
                    Path::new(&child_os),
                    current_user.sid,
                    administrators.sid,
                    system.sid,
                )
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "verify concurrently created private directory {}: {error}",
                            Path::new(&child_os).display()
                        ),
                    )
                })?;
            }
            handles.push(child);
        }
        Ok(())
    }

    pub(super) fn create_private_file_new(path: &Path) -> io::Result<File> {
        create_private_file_new_with_access(path, GENERIC_READ | GENERIC_WRITE)
    }

    pub(super) fn create_private_file_new_append(path: &Path) -> io::Result<File> {
        create_private_file_new_with_access(path, FILE_APPEND_DATA)
    }

    fn create_private_file_new_with_access(path: &Path, desired_access: u32) -> io::Result<File> {
        create_parent_dirs(path)?;
        let path = std::path::absolute(path)?;
        let parent = PrivateParentGuard::new(&path)?;
        let current_user = current_user_sid()?;
        let mut acl = private_acl(current_user.sid)?;
        let mut descriptor =
            private_security_descriptor(current_user.sid, acl.as_mut_ptr().cast())?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::addr_of_mut!(descriptor).cast(),
            bInheritHandle: 0,
        };
        let mut path_wide = parent.stable_path(&path)?;
        path_wide.push(0);
        // SAFETY: every pointer is valid for the call. The descriptor owns no
        // storage; its SID and ACL are anchored above until CreateFileW returns.
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                desired_access | DELETE,
                // Keep the new directory entry stable until every
                // postcondition has been checked. In particular, this makes
                // the failure cleanup below incapable of deleting a path an
                // attacker swapped in after creation.
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateFileW transferred ownership of this valid handle.
        let file = unsafe { File::from_raw_handle(handle as RawHandle) };
        let result = (|| {
            require_regular_non_reparse(&file, &path)?;
            require_single_link(&file, &path)?;
            parent.verify()?;
            if !owned_by_user_handle(&file, current_user.sid)?
                || !has_current_user_only_dacl_handle(&file, current_user.sid)?
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "new private file did not retain its current-user owner and protected DACL",
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            delete_on_close_best_effort(&file);
            drop(file);
            return Err(error);
        }
        Ok(file)
    }

    pub(super) fn delete_on_close_best_effort(file: &File) {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: the handle was created with DELETE access and the buffer
        // matches FileDispositionInfo. Failure merely leaves an empty private
        // file behind; cleanup never falls back to a raceable path deletion.
        unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as HANDLE,
                FileDispositionInfo,
                std::ptr::addr_of!(disposition).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
    }

    pub(super) fn open_existing_private_file(path: &Path, append: bool) -> io::Result<File> {
        reject_parent_components(path)?;
        let path = std::path::absolute(path)?;
        let parent = PrivateParentGuard::new(&path)?;
        let data_access = if append {
            FILE_APPEND_DATA
        } else {
            GENERIC_READ | GENERIC_WRITE
        };
        let mut path_wide = parent.stable_path(&path)?;
        path_wide.push(0);
        let open = |access, share| {
            // SAFETY: the path is NUL-terminated and every optional pointer is
            // null. A successful call transfers a new handle to the caller.
            let handle = unsafe {
                CreateFileW(
                    path_wide.as_ptr(),
                    access,
                    share,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                Err(io::Error::last_os_error())
            } else {
                // SAFETY: CreateFileW transferred ownership of this handle.
                Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
            }
        };
        let shared_access = data_access | FILE_READ_ATTRIBUTES | READ_CONTROL;
        let shared = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let file = open(shared_access, shared)?;
        require_regular_non_reparse(&file, &path)?;
        require_single_link(&file, &path)?;
        parent.verify()?;
        if private_object_is_exact(&file)? {
            return Ok(file);
        }
        drop(file);

        // A broad legacy object is hardened only while held with no sharing.
        // This fails closed if any handle opened under the old DACL survives.
        let exclusive = open(shared_access | WRITE_DAC | WRITE_OWNER, 0)?;
        require_regular_non_reparse(&exclusive, &path)?;
        require_single_link(&exclusive, &path)?;
        parent.verify()?;
        let identity = file_identity(&exclusive)?;
        restrict_private_object_direct(&exclusive)?;
        drop(exclusive);

        // The DACL is now private, so reopening with cooperative sharing
        // cannot admit a new untrusted handle. Revalidate the exact object.
        let file = open(shared_access, shared)?;
        if file_identity(&file)? != identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private path changed while reopening a hardened legacy file",
            ));
        }
        require_regular_non_reparse(&file, &path)?;
        require_single_link(&file, &path)?;
        parent.verify()?;
        if !private_object_is_exact(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private owner/DACL verification failed after legacy hardening",
            ));
        }
        Ok(file)
    }

    fn private_object_is_exact(file: &File) -> io::Result<bool> {
        let current_user = current_user_sid()?;
        Ok(owned_by_user_handle_direct(file, current_user.sid)?
            && has_current_user_only_dacl_direct(file, current_user.sid)?)
    }

    pub(crate) struct PreservedDacl {
        // Keep the exact destination object open without FILE_SHARE_DELETE
        // until publication. FileRenameInfoEx with POSIX replacement semantics
        // can replace that held name, while another process cannot swap the
        // object between this snapshot and the rename.
        _source: File,
        permissions: std::fs::Permissions,
        encrypted: bool,
        storage: Option<Vec<u64>>,
        protected: bool,
    }

    pub(super) fn capture_destination_dacl(
        destination: &Path,
        capture_dacl: bool,
    ) -> io::Result<PreservedDacl> {
        let destination = std::path::absolute(destination)?;
        let parent = PrivateParentGuard::new(&destination)?;
        let destination_path = std::ffi::OsString::from_wide(&parent.stable_path(&destination)?);
        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES | if capture_dacl { READ_CONTROL } else { 0 })
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let source = options.open(&destination_path)?;
        require_regular_non_reparse(&source, &destination)?;
        parent.verify()?;

        let (storage, protected) = if capture_dacl {
            let mut dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: output pointers are valid and the source handle has
            // READ_CONTROL. GetSecurityInfo anchors `dacl` in `descriptor`.
            let status = unsafe {
                GetSecurityInfo(
                    source.as_raw_handle() as HANDLE,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut dacl,
                    std::ptr::null_mut(),
                    &mut descriptor,
                )
            };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            let descriptor = SecurityDescriptor(descriptor);
            let mut control = 0_u16;
            let mut revision = 0_u32;
            // SAFETY: GetSecurityInfo returned this valid descriptor.
            if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            let storage = if dacl.is_null() {
                None
            } else {
                if unsafe { IsValidAcl(dacl) } == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "destination has an invalid DACL",
                    ));
                }
                let len = usize::from(unsafe { (*dacl).AclSize });
                if len < std::mem::size_of::<ACL>() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "destination DACL is shorter than its header",
                    ));
                }
                let mut storage = vec![0_u64; len.div_ceil(std::mem::size_of::<u64>())];
                // SAFETY: the validated ACL occupies `len` readable bytes in
                // the descriptor and the aligned destination is large enough.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        dacl.cast::<u8>(),
                        storage.as_mut_ptr().cast::<u8>(),
                        len,
                    );
                }
                Some(storage)
            };
            (storage, control & SE_DACL_PROTECTED != 0)
        } else {
            (None, false)
        };
        Ok(PreservedDacl {
            permissions: source.metadata()?.permissions(),
            encrypted: file_information(&source)?.dwFileAttributes & FILE_ATTRIBUTE_ENCRYPTED != 0,
            _source: source,
            storage,
            protected,
        })
    }

    pub(super) fn preserved_permissions(preserved: &PreservedDacl) -> std::fs::Permissions {
        preserved.permissions.clone()
    }

    pub(super) fn preserved_is_encrypted(preserved: &PreservedDacl) -> bool {
        preserved.encrypted
    }

    pub(super) fn apply_preserved_dacl(preserved: &PreservedDacl, staged: &File) -> io::Result<()> {
        let protection = if preserved.protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        let dacl = preserved
            .storage
            .as_ref()
            .map_or(std::ptr::null_mut(), |storage| {
                storage.as_ptr().cast_mut().cast::<ACL>()
            });
        let staged_handle = reopen_for_acl(staged)?;
        // SAFETY: the staged handle has WRITE_DAC and the optional aligned ACL
        // buffer remains valid for the call. No owner or SACL is changed.
        let status = unsafe {
            SetSecurityInfo(
                staged_handle.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | protection,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn require_regular_non_reparse(file: &File, path: &Path) -> io::Result<()> {
        let information = file_information(file)?;
        if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
            != 0
            || !file.metadata()?.file_type().is_file()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private path is not a non-reparse regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    fn require_single_link(file: &File, path: &Path) -> io::Result<()> {
        if file_information(file)?.nNumberOfLinks == 1 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private file has an unexpected hard-link count: {}",
                    path.display()
                ),
            ))
        }
    }

    fn file_information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut information = BY_HANDLE_FILE_INFORMATION {
            dwFileAttributes: 0,
            ftCreationTime: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            ftLastAccessTime: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            ftLastWriteTime: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            dwVolumeSerialNumber: 0,
            nFileSizeHigh: 0,
            nFileSizeLow: 0,
            nNumberOfLinks: 0,
            nFileIndexHigh: 0,
            nFileIndexLow: 0,
        };
        // SAFETY: the file owns a valid handle and the output is writable.
        if unsafe {
            GetFileInformationByHandle(
                file.as_raw_handle() as HANDLE,
                std::ptr::addr_of_mut!(information),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(information)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FileIdentity {
        volume: u64,
        identifier: [u8; 16],
    }

    fn file_identity(file: &File) -> io::Result<FileIdentity> {
        let mut information = FILE_ID_INFO::default();
        // SAFETY: `file` owns a valid handle and the correctly sized output
        // buffer remains writable for the call.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileIdInfo,
                std::ptr::addr_of_mut!(information).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(FileIdentity {
            volume: information.VolumeSerialNumber,
            identifier: information.FileId.Identifier,
        })
    }

    fn sid_is_nt_service(sid: PSID) -> bool {
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return false;
        }
        let len = unsafe { GetLengthSid(sid) } as usize;
        if len < 12 {
            return false;
        }
        // A service SID is S-1-5-80-..., where authority 5 is encoded in the
        // six big-endian identifier-authority bytes and subauthorities are
        // little-endian u32 values.
        // SAFETY: IsValidSid and GetLengthSid validated at least 12 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) };
        bytes[2..8] == [0, 0, 0, 0, 0, 5]
            && u32::from_le_bytes(bytes[8..12].try_into().expect("four-byte SID subauthority"))
                == 80
    }

    fn trusted_windows_sid(
        sid: PSID,
        current_user: PSID,
        administrators: PSID,
        system: PSID,
    ) -> bool {
        !sid.is_null()
            && (unsafe { EqualSid(sid, current_user) } != 0
                || unsafe { EqualSid(sid, administrators) } != 0
                || unsafe { EqualSid(sid, system) } != 0
                || sid_is_nt_service(sid))
    }

    fn require_trusted_directory_security(
        directory: &File,
        path: &Path,
        current_user: PSID,
        administrators: PSID,
        system: PSID,
    ) -> io::Result<()> {
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: the held directory handle has READ_CONTROL and every output
        // pointer is valid. The returned owner/DACL are anchored in descriptor.
        let status = unsafe {
            GetSecurityInfo(
                directory.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _descriptor = SecurityDescriptor(descriptor);
        if !trusted_windows_sid(owner, current_user, administrators, system) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("private parent has an untrusted owner: {}", path.display()),
            ));
        }
        if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private parent has no valid access-control list: {}",
                    path.display()
                ),
            ));
        }

        const DANGEROUS_DIRECTORY_RIGHTS: u32 =
            FILE_DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;
        for index in 0..unsafe { (*dacl).AceCount } {
            let mut ace = std::ptr::null_mut();
            // SAFETY: index is bounded by the validated ACL's AceCount.
            if unsafe { GetAce(dacl, u32::from(index), &mut ace) } == 0 || ace.is_null() {
                return Err(io::Error::last_os_error());
            }
            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
            // All access-allowed ACE forms begin with ACE_HEADER + ACCESS_MASK.
            // SAFETY: GetAce returned a kernel-validated ACE.
            let kind = u32::from(unsafe { (*allowed).Header.AceType });
            let flags = unsafe { (*allowed).Header.AceFlags };
            let mask = unsafe { (*allowed).Mask };
            let is_allow = matches!(
                kind,
                ACCESS_ALLOWED_ACE_TYPE
                    | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
                    | ACCESS_ALLOWED_OBJECT_ACE_TYPE
                    | ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
                    | ACCESS_ALLOWED_COMPOUND_ACE_TYPE
            );
            if !is_allow
                || flags & INHERIT_ONLY_ACE as u8 != 0
                || mask & DANGEROUS_DIRECTORY_RIGHTS == 0
            {
                continue;
            }
            if !matches!(
                kind,
                ACCESS_ALLOWED_ACE_TYPE | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "private parent has an unverified object-specific mutation ACE: {}",
                        path.display()
                    ),
                ));
            }
            let sid: PSID = unsafe { std::ptr::addr_of!((*allowed).SidStart).cast_mut().cast() };
            if !trusted_windows_sid(sid, current_user, administrators, system) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "private parent grants deletion/control rights to an untrusted principal: {}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    pub(crate) struct PrivateParentGuard {
        path: PathBuf,
        identity: FileIdentity,
        _handles: Vec<File>,
    }

    impl PrivateParentGuard {
        pub(super) fn new(path: &Path) -> io::Result<Self> {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let parent = std::path::absolute(parent)?;
            let (handles, identity) = open_verified_parent_chain(&parent)?;
            Ok(Self {
                path: parent,
                identity,
                _handles: handles,
            })
        }

        fn stable_path(&self, path: &Path) -> io::Result<Vec<u16>> {
            let path = std::path::absolute(path)?;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            if parent != self.path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "private file does not belong to the guarded parent",
                ));
            }
            stable_child_path(
                self._handles
                    .last()
                    .expect("a parent guard always holds its immediate parent"),
                path.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "private path has no file name")
                })?,
            )
        }

        pub(crate) fn verify(&self) -> io::Result<()> {
            let (handles, identity) = open_verified_parent_chain(&self.path)?;
            drop(handles);
            if identity == self.identity {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "private parent changed while it was in use: {}",
                        self.path.display()
                    ),
                ))
            }
        }

        pub(super) fn replace_with_open_file(
            &self,
            staged: &File,
            destination: &Path,
        ) -> io::Result<()> {
            let destination = std::path::absolute(destination)?;
            let parent = destination
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            if parent != self.path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "replacement destination does not belong to the guarded parent",
                ));
            }
            let name = self.stable_path(&destination)?;
            if name.is_empty() || name.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "replacement file name is empty or contains an embedded NUL",
                ));
            }
            let name_bytes = name
                .len()
                .checked_mul(std::mem::size_of::<u16>())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "replacement name is too long")
                })?;
            let total_bytes = std::mem::size_of::<FILE_RENAME_INFO>()
                .checked_add(name_bytes)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "replacement name is too long")
                })?;
            let words = total_bytes.div_ceil(std::mem::size_of::<u64>());
            let mut storage = vec![0_u64; words];
            let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
            // SAFETY: the aligned zeroed buffer contains the fixed header and
            // enough trailing storage for every UTF-16 code unit.
            unsafe {
                // Ignore-readonly is documented for FILE_RENAME_INFO_EX but
                // is not yet exposed by windows-sys 0.61.
                const FILE_RENAME_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x40;
                (*info).Anonymous.Flags = FILE_RENAME_FLAG_REPLACE_IF_EXISTS
                    | FILE_RENAME_FLAG_POSIX_SEMANTICS
                    | FILE_RENAME_FLAG_IGNORE_READONLY_ATTRIBUTE;
                (*info).RootDirectory = std::ptr::null_mut();
                (*info).FileNameLength = u32::try_from(name_bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "replacement name is too long")
                })?;
                std::ptr::copy_nonoverlapping(
                    name.as_ptr(),
                    std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                    name.len(),
                );
            }
            let buffer_len = u32::try_from(total_bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "replacement name is too long")
            })?;
            // SAFETY: the staged handle was created with DELETE access and
            // `info` is a correctly sized FILE_RENAME_INFO buffer.
            if unsafe {
                SetFileInformationByHandle(
                    staged.as_raw_handle() as HANDLE,
                    FileRenameInfoEx,
                    info.cast(),
                    buffer_len,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn link_open_file_new(
            &self,
            staged: &File,
            source: &Path,
            destination: &Path,
        ) -> io::Result<()> {
            let source = std::ffi::OsString::from_wide(&self.stable_path(source)?);
            let destination = std::ffi::OsString::from_wide(&self.stable_path(destination)?);
            self.verify()?;

            let mut options = std::fs::OpenOptions::new();
            options
                .access_mode(FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            let source_file = options.open(&source)?;
            require_regular_non_reparse(&source_file, Path::new(&source))?;
            if file_identity(&source_file)? != file_identity(staged)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "staged path no longer refers to the open private file",
                ));
            }
            std::fs::hard_link(source, destination)
        }
    }

    fn open_verified_parent_chain(path: &Path) -> io::Result<(Vec<File>, FileIdentity)> {
        let ancestors = path
            .ancestors()
            .filter(|ancestor| !ancestor.as_os_str().is_empty())
            .collect::<Vec<_>>();
        let mut handles = Vec::with_capacity(ancestors.len());
        let mut parent_identity = None;
        let current_user = current_user_sid()?;
        let administrators = builtin_administrators_sid()?;
        let system = well_known_sid(WinLocalSystemSid)?;
        // Open from the filesystem root down. Every accepted directory stays
        // open without FILE_SHARE_DELETE while the next component is checked,
        // so an attacker cannot swap a previously validated ancestor between
        // a metadata check and the final file operation.
        for ancestor in ancestors.into_iter().rev() {
            let (handle, identity) = if let Some(parent) = handles.last() {
                let name = ancestor.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "private directory has no component name: {}",
                            ancestor.display()
                        ),
                    )
                })?;
                let child = std::ffi::OsString::from_wide(&stable_child_path(parent, name)?);
                open_non_reparse_directory(Path::new(&child))?
            } else {
                open_non_reparse_directory(ancestor)?
            };
            require_trusted_directory_security(
                &handle,
                ancestor,
                current_user.sid,
                administrators.sid,
                system.sid,
            )?;
            parent_identity = Some(identity);
            handles.push(handle);
        }
        let identity = parent_identity.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("private path has no parent directory: {}", path.display()),
            )
        })?;
        Ok((handles, identity))
    }

    fn open_non_reparse_directory(path: &Path) -> io::Result<(File, FileIdentity)> {
        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let handle = options.open(path)?;
        let information = file_information(&handle)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private parent is not a non-reparse directory: {}",
                    path.display()
                ),
            ));
        }
        let identity = file_identity(&handle)?;
        Ok((handle, identity))
    }

    pub(super) fn same_file_identity(file: &File, path: &Path) -> io::Result<bool> {
        let path = std::path::absolute(path)?;
        let parent = PrivateParentGuard::new(&path)?;
        let stable_path = std::ffi::OsString::from_wide(&parent.stable_path(&path)?);
        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let linked = options.open(&stable_path)?;
        require_regular_non_reparse(&linked, &path)?;
        parent.verify()?;
        Ok(file_identity(file)? == file_identity(&linked)?)
    }

    fn reopen_for_acl(file: &File) -> io::Result<OwnedHandle> {
        // SAFETY: `file` owns a valid handle. ReOpenFile resolves that same
        // kernel object rather than a path and returns a separately owned
        // handle with the rights used below.
        let handle = unsafe {
            ReOpenFile(
                file.as_raw_handle() as HANDLE,
                READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ReOpenFile transferred ownership of this valid handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    fn reopen_for_owner(file: &File) -> io::Result<OwnedHandle> {
        // SAFETY: `file` owns a valid handle. ReOpenFile resolves that same
        // kernel object and the exact private DACL grants TokenUser WRITE_OWNER.
        let handle = unsafe {
            ReOpenFile(
                file.as_raw_handle() as HANDLE,
                READ_CONTROL | WRITE_OWNER,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ReOpenFile transferred ownership of this valid handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: GetSecurityInfo allocated the descriptor with
                // LocalAlloc and transferred it to this wrapper.
                unsafe { LocalFree(self.0.cast()) };
            }
        }
    }

    pub(super) fn restrict_private_object(file: &File) -> io::Result<()> {
        require_regular_non_reparse(file, Path::new("<open private file>"))?;
        require_single_link(file, Path::new("<open private file>"))?;
        let handle = reopen_for_acl(file)?;
        restrict_private_object_on_handle(Some(file), handle.as_raw_handle() as HANDLE, None)
    }

    fn restrict_private_object_direct(file: &File) -> io::Result<()> {
        require_regular_non_reparse(file, Path::new("<open private file>"))?;
        require_single_link(file, Path::new("<open private file>"))?;
        let handle = file.as_raw_handle() as HANDLE;
        restrict_private_object_on_handle(None, handle, Some(handle))
    }

    fn restrict_private_object_on_handle(
        source_file: Option<&File>,
        handle: HANDLE,
        owner_handle: Option<HANDLE>,
    ) -> io::Result<()> {
        let current_user = current_user_sid()?;
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: output pointers are valid and the reopened handle has
        // READ_CONTROL. Unrequested group/SACL outputs are null.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let descriptor = SecurityDescriptor(descriptor);
        let already_private = dacl_is_current_user_only(descriptor.0, dacl, current_user.sid)?;
        if !owner_authorizes_hardening(owner, current_user.sid) {
            let administrators = builtin_administrators_sid()?;
            let legacy_elevated_owner =
                !owner.is_null() && unsafe { EqualSid(owner, administrators.sid) } != 0;
            if legacy_elevated_owner && already_private {
                drop(descriptor);
                if let Some(owner_handle) = owner_handle {
                    migrate_legacy_administrators_owner_on_handle(owner_handle, current_user.sid)?;
                } else {
                    migrate_legacy_administrators_owner(
                        source_file.expect("reopened ACL handle has a source file"),
                        current_user.sid,
                    )?;
                }
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to change the ACL of an object not owned by the effective user",
            ));
        }
        if already_private {
            return Ok(());
        }

        let mut acl_storage = private_acl(current_user.sid)?;
        let acl = acl_storage.as_mut_ptr().cast::<ACL>();
        // SAFETY: SetSecurityInfo consumes neither the handle nor ACL. The
        // protected DACL disables inherited broad entries.
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if has_current_user_only_dacl_raw(handle, current_user.sid)? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private DACL verification failed after SetSecurityInfo",
            ))
        }
    }

    fn migrate_legacy_administrators_owner(file: &File, current_user: PSID) -> io::Result<()> {
        // This narrow compatibility migration applies only after the caller
        // proved that the DACL is already the exact protected TokenUser-only
        // form emitted by older elevated Kettle builds. Administrators
        // ownership never authorizes rewriting a broader DACL.
        let handle = reopen_for_owner(file)?;
        migrate_legacy_administrators_owner_on_handle(
            handle.as_raw_handle() as HANDLE,
            current_user,
        )
    }

    fn migrate_legacy_administrators_owner_on_handle(
        handle: HANDLE,
        current_user: PSID,
    ) -> io::Result<()> {
        // SAFETY: the handle has WRITE_OWNER, the validated TokenUser SID
        // remains alive, and no group/DACL/SACL output is requested.
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                current_user,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if owned_by_user_raw(handle, current_user)?
            && has_current_user_only_dacl_raw(handle, current_user)?
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "legacy elevated private-file owner migration failed verification",
            ))
        }
    }

    fn owner_authorizes_hardening(owner: PSID, current_user: PSID) -> bool {
        !owner.is_null() && unsafe { EqualSid(owner, current_user) } != 0
    }

    fn dacl_is_current_user_only(
        descriptor: PSECURITY_DESCRIPTOR,
        dacl: *mut ACL,
        current_user: PSID,
    ) -> io::Result<bool> {
        if descriptor.is_null() || dacl.is_null() {
            return Ok(false);
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: GetSecurityInfo returned this valid descriptor.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if control & SE_DACL_PROTECTED == 0 || unsafe { (*dacl).AceCount } != 1 {
            return Ok(false);
        }
        let mut ace = std::ptr::null_mut();
        // SAFETY: the DACL reports one ACE at index zero.
        if unsafe { GetAce(dacl, 0, &mut ace) } == 0 || ace.is_null() {
            return Err(io::Error::last_os_error());
        }
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: the ACL is kernel-validated; inspect the common header before
        // interpreting the access-allowed layout.
        const ACCESS_ALLOWED_ACE_KIND: u8 = 0;
        if unsafe { (*allowed).Header.AceType } != ACCESS_ALLOWED_ACE_KIND
            || unsafe { (*allowed).Header.AceFlags } != 0
            || unsafe { (*allowed).Mask } != FILE_ALL_ACCESS
        {
            return Ok(false);
        }
        let sid: PSID = unsafe { std::ptr::addr_of!((*allowed).SidStart).cast_mut().cast() };
        if unsafe { IsValidSid(sid) } == 0 {
            return Ok(false);
        }
        let expected_ace_size = std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            - std::mem::size_of::<u32>()
            + unsafe { GetLengthSid(sid) } as usize;
        if usize::from(unsafe { (*allowed).Header.AceSize }) != expected_ace_size {
            return Ok(false);
        }
        Ok(unsafe { EqualSid(sid, current_user) } != 0)
    }

    fn has_current_user_only_dacl_handle(file: &File, current_user: PSID) -> io::Result<bool> {
        let handle = reopen_for_acl(file)?;
        has_current_user_only_dacl_raw(handle.as_raw_handle() as HANDLE, current_user)
    }

    fn has_current_user_only_dacl_direct(file: &File, current_user: PSID) -> io::Result<bool> {
        has_current_user_only_dacl_raw(file.as_raw_handle() as HANDLE, current_user)
    }

    fn has_current_user_only_dacl_raw(handle: HANDLE, current_user: PSID) -> io::Result<bool> {
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: output pointers are valid and the handle has READ_CONTROL.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let descriptor = SecurityDescriptor(descriptor);
        dacl_is_current_user_only(descriptor.0, dacl, current_user)
    }

    fn owned_by_user_handle(file: &File, current_user: PSID) -> io::Result<bool> {
        let handle = reopen_for_acl(file)?;
        owned_by_user_raw(handle.as_raw_handle() as HANDLE, current_user)
    }

    fn owned_by_user_handle_direct(file: &File, current_user: PSID) -> io::Result<bool> {
        owned_by_user_raw(file.as_raw_handle() as HANDLE, current_user)
    }

    fn owned_by_user_raw(handle: HANDLE, current_user: PSID) -> io::Result<bool> {
        let mut owner = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: output pointers are valid and the handle has READ_CONTROL.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _descriptor = SecurityDescriptor(descriptor);
        Ok(owner_authorizes_hardening(owner, current_user))
    }

    #[cfg(test)]
    pub(super) fn has_current_user_only_dacl(file: &File) -> io::Result<bool> {
        let current_user = current_user_sid()?;
        has_current_user_only_dacl_handle(file, current_user.sid)
    }

    #[cfg(test)]
    pub(super) fn owned_by_current_user(file: &File) -> io::Result<bool> {
        let current_user = current_user_sid()?;
        owned_by_user_handle(file, current_user.sid)
    }

    #[cfg(test)]
    pub(super) fn dacl_signature(path: &Path) -> io::Result<(Option<Vec<u8>>, bool)> {
        let preserved = capture_destination_dacl(path, true)?;
        let bytes = preserved.storage.as_ref().map(|storage| {
            let acl = storage.as_ptr().cast::<ACL>();
            let len = usize::from(unsafe { (*acl).AclSize });
            // SAFETY: capture_destination_dacl allocated at least AclSize
            // bytes and retained them in this aligned buffer.
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), len) }.to_vec()
        });
        Ok((bytes, preserved.protected))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::Security::{
            CreateWellKnownSid, SECURITY_MAX_SID_SIZE, WinBuiltinAdministratorsSid,
        };

        #[test]
        fn group_owner_is_never_private_file_provenance() {
            let current_user = current_user_sid().unwrap();
            let words = (SECURITY_MAX_SID_SIZE as usize).div_ceil(std::mem::size_of::<u64>());
            let mut group = vec![0_u64; words];
            let mut len = (group.len() * std::mem::size_of::<u64>()) as u32;
            // SAFETY: the aligned buffer is writable for `len` bytes.
            assert_ne!(
                unsafe {
                    CreateWellKnownSid(
                        WinBuiltinAdministratorsSid,
                        std::ptr::null_mut(),
                        group.as_mut_ptr().cast(),
                        &mut len,
                    )
                },
                0
            );
            let group = group.as_mut_ptr().cast();
            assert!(
                !owner_authorizes_hardening(group, current_user.sid),
                "a group-valued owner alone must never authorize a DACL rewrite"
            );
            assert!(
                owner_authorizes_hardening(current_user.sid, current_user.sid),
                "the effective user must authorize its own file"
            );
        }

        #[test]
        fn handle_based_failure_cleanup_removes_the_created_object() {
            let dir = crate::test_tempdir();
            let path = dir.path().join("cleanup");
            let file = create_private_file_new(&path).unwrap();
            delete_on_close_best_effort(&file);
            drop(file);
            assert!(!path.exists());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn private_create_and_reopen_are_owner_only() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("private");
        let file = create_private_file_new(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        #[cfg(windows)]
        {
            assert!(has_current_user_only_dacl(&file).unwrap());
            assert!(
                owned_by_current_user(&file).unwrap(),
                "create-time descriptor must select the user, not a group token owner"
            );
        }
        drop(file);

        let reopened = open_private_file(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                reopened.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        assert!(has_current_user_only_dacl(&reopened).unwrap());
    }

    #[test]
    fn private_append_preserves_existing_content() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("append");
        let mut first = open_private_file_append(&path).unwrap();
        first.write_all(b"one").unwrap();
        drop(first);
        let mut second = open_private_file_append(&path).unwrap();
        second.write_all(b"-two").unwrap();
        drop(second);
        assert_eq!(std::fs::read(path).unwrap(), b"one-two");
    }

    #[cfg(windows)]
    #[test]
    fn existing_user_owned_file_is_hardened() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("legacy");
        let file = File::create(&path).unwrap();
        restrict_private_file(&file).unwrap();
        assert!(has_current_user_only_dacl(&file).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn mutable_private_open_rejects_windows_hard_links() {
        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let alias = dir.path().join("alias");
        std::fs::write(&target, b"unchanged").unwrap();
        std::fs::hard_link(&target, &alias).unwrap();

        assert!(open_private_file(&alias).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
    }

    #[cfg(windows)]
    #[test]
    fn private_open_helpers_reject_win32_alias_and_stream_syntax() {
        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let mut file = create_private_file_new(&target).unwrap();
        file.write_all(b"unchanged").unwrap();
        drop(file);

        let mut trailing_dot = target.as_os_str().to_os_string();
        trailing_dot.push(".");
        let mut stream = target.as_os_str().to_os_string();
        stream.push(":private");

        assert!(open_private_file(Path::new(&trailing_dot)).is_err());
        assert!(create_private_file_new(Path::new(&stream)).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
    }

    #[cfg(windows)]
    #[test]
    fn legacy_windows_hardening_requires_exclusive_access() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("legacy");
        std::fs::write(&path, b"private").unwrap();
        let preexisting_reader = File::open(&path).unwrap();

        assert!(open_private_file(&path).is_err());
        drop(preexisting_reader);
        assert!(open_private_file(&path).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn private_open_helpers_reject_reparse_leafs_and_parents_when_supported() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        use std::process::{Command, Stdio};

        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, b"unchanged").unwrap();
        match symlink_file(&target, &link) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314) =>
            {
                // Directory junctions do not require the symbolic-link
                // privilege on supported desktop Windows versions. Exercise
                // both the leaf and ancestor policies through one when
                // Developer Mode is unavailable.
                let real_parent = dir.path().join("real-parent");
                let linked_parent = dir.path().join("linked-parent");
                std::fs::create_dir(&real_parent).unwrap();
                let status = Command::new("cmd.exe")
                    .args(["/d", "/c", "mklink", "/J"])
                    .arg(&linked_parent)
                    .arg(&real_parent)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .expect("launch cmd.exe to create a test junction");
                if !status.success() {
                    eprintln!(
                        "skipping reparse test: neither symlinks nor a test junction are permitted"
                    );
                    return;
                }
                assert!(open_private_file(&linked_parent).is_err());
                assert!(create_private_file_new(&linked_parent.join("private")).is_err());
                assert!(!real_parent.join("private").exists());
                return;
            }
            Err(error) => panic!("create file symlink: {error}"),
        }
        assert!(open_private_file(&link).is_err());
        assert!(open_private_file_append(&link).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");

        let real_parent = dir.path().join("real-parent");
        let linked_parent = dir.path().join("linked-parent");
        std::fs::create_dir(&real_parent).unwrap();
        symlink_dir(&real_parent, &linked_parent).unwrap();
        assert!(create_private_file_new(&linked_parent.join("private")).is_err());
        assert!(!real_parent.join("private").exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_open_helpers_reject_symbolic_link_leafs() {
        use std::os::unix::fs::symlink;

        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, b"unchanged").unwrap();
        symlink(&target, &link).unwrap();

        assert!(open_private_file(&link).is_err());
        assert!(open_private_file_append(&link).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn private_helpers_reject_untrusted_symlink_ancestors_before_mutating() {
        use std::os::unix::fs::symlink;

        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();
        let path = link.join("missing/private");

        assert!(create_private_file_new(&path).is_err());
        assert!(!target.join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn mutable_private_open_rejects_hard_links_without_chmodding_target() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = crate::test_tempdir();
        let target = dir.path().join("target");
        let alias = dir.path().join("alias");
        std::fs::write(&target, b"unchanged").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::hard_link(&target, &alias).unwrap();

        assert!(open_private_file(&alias).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn restrict_private_file_rejects_directories_without_chmodding() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = crate::test_tempdir();
        let before = dir.path().metadata().unwrap().permissions().mode() & 0o777;
        let handle = File::open(dir.path()).unwrap();

        assert!(restrict_private_file(&handle).is_err());
        assert_eq!(
            dir.path().metadata().unwrap().permissions().mode() & 0o777,
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_create_restores_exact_mode_under_a_restrictive_umask() {
        const CHILD_ENV: &str = "KETTLE_STATE_RESTRICTIVE_UMASK_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "private::tests::private_create_restores_exact_mode_under_a_restrictive_umask",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .status()
                .unwrap();
            assert!(status.success(), "restrictive-umask child failed: {status}");
            return;
        }

        let dir = crate::test_tempdir();
        // SAFETY: this branch only runs in the isolated child process above,
        // before it starts any application threads. Create the trusted parent
        // first so this deliberately unusual umask affects only the file.
        unsafe { libc::umask(0o400) };
        let path = dir.path().join("private");
        let file = create_private_file_new(&path).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}
