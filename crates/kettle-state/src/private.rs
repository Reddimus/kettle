//! Cross-platform owner-only permissions for private files.

use std::fs::File;
use std::io;
use std::path::Path;

type CreatedFileCleanup = Box<dyn FnOnce(&File) -> io::Result<()> + Send + 'static>;
type CreatedFilePublish = Box<
    dyn FnOnce(&File) -> Result<CreatedFilePublication, CreatedFilePublishError> + Send + 'static,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreatedFilePublication {
    /// Publication added a second name; the staging name still needs removal.
    Linked,
    /// Publication moved the staging name; its cleanup capability must be
    /// disarmed so Windows does not delete the completed destination on close.
    Renamed,
}

#[derive(Debug)]
struct CreatedFilePublishError {
    error: io::Error,
    destination_may_exist: bool,
}

impl CreatedFilePublishError {
    fn published(error: io::Error) -> Self {
        Self {
            error,
            destination_may_exist: true,
        }
    }
}

impl From<io::Error> for CreatedFilePublishError {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            destination_may_exist: false,
        }
    }
}

/// Failure to publish a staged user-selected file.
///
/// [`destination_may_exist`](Self::destination_may_exist) distinguishes a
/// failure before the atomic namespace operation from a durability,
/// verification, or staging-cleanup failure after it. Callers must not retry
/// the same path blindly in the latter case.
#[derive(Debug)]
pub struct StagedFilePublishError {
    error: io::Error,
    destination_may_exist: bool,
}

impl StagedFilePublishError {
    fn not_published(error: io::Error) -> Self {
        Self {
            error,
            destination_may_exist: false,
        }
    }

    fn after_publication(error: io::Error) -> Self {
        Self {
            error,
            destination_may_exist: true,
        }
    }

    pub fn destination_may_exist(&self) -> bool {
        self.destination_may_exist
    }

    pub fn kind(&self) -> io::ErrorKind {
        self.error.kind()
    }
}

impl std::fmt::Display for StagedFilePublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for StagedFilePublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy)]
enum HardLinkMode {
    Native,
    #[cfg(test)]
    ForceUnsupported,
}

fn with_cleanup_error(primary: io::Error, cleanup: io::Result<()>, action: &str) -> io::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => io::Error::new(
            primary.kind(),
            format!("{primary}; {action} cleanup failed: {cleanup}"),
        ),
    }
}

fn publish_with_atomic_fallback(
    link: impl FnOnce() -> io::Result<()>,
    rename_no_replace: impl FnOnce() -> io::Result<()>,
) -> io::Result<CreatedFilePublication> {
    match link() {
        Ok(()) => Ok(CreatedFilePublication::Linked),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(link_error) => match rename_no_replace() {
            Ok(()) => Ok(CreatedFilePublication::Renamed),
            Err(rename_error) => Err(io::Error::new(
                rename_error.kind(),
                format!(
                    "hard-link publication failed: {link_error}; atomic rename fallback failed: {rename_error}"
                ),
            )),
        },
    }
}

/// An owner-only sibling file that is invisible at its requested destination
/// until [`publish`](Self::publish) atomically creates that destination.
///
/// The staging leaf is removed on drop. Publication never replaces an existing
/// entry and retains the platform's pinned parent capability across the final
/// link-or-rename operation, so a renamed parent cannot redirect the write.
pub struct StagedUserSelectedFile {
    staged: Option<CreatedUserSelectedFile>,
    publish: Option<CreatedFilePublish>,
}

impl std::fmt::Debug for StagedUserSelectedFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedUserSelectedFile")
            .field("staged", &self.staged)
            .field("publish_armed", &self.publish.is_some())
            .finish()
    }
}

impl StagedUserSelectedFile {
    fn new(staged: CreatedUserSelectedFile, publish: CreatedFilePublish) -> Self {
        Self {
            staged: Some(staged),
            publish: Some(publish),
        }
    }

    /// Make every streamed byte durable while publication is still reversible.
    ///
    /// A bounded caller may cancel after this returns and before
    /// [`publish_synced`](Self::publish_synced) begins. Keeping this operation
    /// separate prevents a slow filesystem flush from turning a nominal
    /// control-plane timeout into an unbounded wait.
    pub fn sync_for_publish(&self) -> io::Result<()> {
        self.staged
            .as_ref()
            .expect("staged user-selected file remains present")
            .sync_all()
    }

    /// Atomically create the requested destination from an already-synced
    /// staging file and retire the sibling staging name. Existing destinations
    /// are refused.
    pub fn publish_synced(mut self) -> Result<(), StagedFilePublishError> {
        let staged = self
            .staged
            .take()
            .expect("staged user-selected file remains present");
        let publish = self
            .publish
            .take()
            .expect("staged publication remains armed");
        match publish(&staged) {
            Ok(CreatedFilePublication::Linked) => staged.discard().map_err(|error| {
                StagedFilePublishError::after_publication(io::Error::new(
                    error.kind(),
                    format!("destination was published, but staging cleanup failed: {error}"),
                ))
            }),
            Ok(CreatedFilePublication::Renamed) => {
                drop(staged.persist());
                Ok(())
            }
            Err(error) if error.destination_may_exist => {
                let error = with_cleanup_error(error.error, staged.discard(), "staged export");
                Err(StagedFilePublishError::after_publication(error))
            }
            Err(error) => Err(StagedFilePublishError::not_published(with_cleanup_error(
                error.error,
                staged.discard(),
                "staged export",
            ))),
        }
    }

    /// Flush the staged inode, atomically create the requested destination,
    /// and retire the sibling staging name. Existing destinations are refused.
    pub fn publish(self) -> Result<(), StagedFilePublishError> {
        if let Err(error) = self.sync_for_publish() {
            return Err(StagedFilePublishError::not_published(with_cleanup_error(
                error,
                self.discard(),
                "staged export",
            )));
        }
        self.publish_synced()
    }

    /// Remove the exact staging leaf without publishing it.
    pub fn discard(mut self) -> io::Result<()> {
        self.publish = None;
        self.staged
            .take()
            .expect("staged user-selected file remains present")
            .discard()
    }
}

impl std::ops::Deref for StagedUserSelectedFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        self.staged
            .as_ref()
            .expect("staged user-selected file remains present")
    }
}

impl std::ops::DerefMut for StagedUserSelectedFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.staged
            .as_mut()
            .expect("staged user-selected file remains present")
    }
}

impl io::Write for StagedUserSelectedFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.staged
            .as_mut()
            .expect("staged user-selected file remains present")
            .write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.staged
            .as_mut()
            .expect("staged user-selected file remains present")
            .flush()
    }
}

impl io::Seek for StagedUserSelectedFile {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.staged
            .as_mut()
            .expect("staged user-selected file remains present")
            .seek(position)
    }
}

/// A newly created user-selected file whose path is removed if publication is
/// abandoned before [`persist`](Self::persist).
///
/// This keeps the platform-specific parent/handle capability alive across the
/// caller's write. [`discard`](Self::discard) reports cleanup failure; dropping
/// the guard makes the same identity-matched attempt on a best-effort basis.
/// Neither path ever removes a replacement entry.
pub struct CreatedUserSelectedFile {
    file: Option<File>,
    cleanup: Option<CreatedFileCleanup>,
}

impl std::fmt::Debug for CreatedUserSelectedFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedUserSelectedFile")
            .field("file", &self.file)
            .field("cleanup_armed", &self.cleanup.is_some())
            .finish()
    }
}

impl CreatedUserSelectedFile {
    fn new(file: File, cleanup: CreatedFileCleanup) -> Self {
        Self {
            file: Some(file),
            cleanup: Some(cleanup),
        }
    }

    /// Disarm failure cleanup and return the completed file.
    pub fn persist(mut self) -> File {
        self.cleanup = None;
        self.file
            .take()
            .expect("created file is present until persist")
    }

    /// Remove the exact created leaf and report whether cleanup succeeded.
    pub fn discard(mut self) -> io::Result<()> {
        let cleanup = self.cleanup.take();
        let result = match (cleanup, self.file.as_ref()) {
            (Some(cleanup), Some(file)) => cleanup(file),
            _ => Ok(()),
        };
        self.file.take();
        result
    }
}

impl std::ops::Deref for CreatedUserSelectedFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        self.file.as_ref().expect("created file remains present")
    }
}

impl std::ops::DerefMut for CreatedUserSelectedFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.file.as_mut().expect("created file remains present")
    }
}

impl io::Write for CreatedUserSelectedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .expect("created file remains present")
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .expect("created file remains present")
            .flush()
    }
}

impl io::Seek for CreatedUserSelectedFile {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.file
            .as_mut()
            .expect("created file remains present")
            .seek(position)
    }
}

impl Drop for CreatedUserSelectedFile {
    fn drop(&mut self) {
        if let (Some(cleanup), Some(file)) = (self.cleanup.take(), self.file.as_ref()) {
            let _ = cleanup(file);
        }
    }
}

/// Bases that contain Kettle's private namespace directory.
///
/// Install/share roots and user-selected output paths are excluded because
/// they are not private state. Environment-controlled bases remain a trust
/// boundary: redirecting one can make a same-named directory eligible for
/// repair. Closing that fully requires a Kettle-written provenance marker.
fn kettle_base_dirs() -> Vec<std::path::PathBuf> {
    kettle_base_dirs_from(|key| std::env::var_os(key), std::env::temp_dir())
}

fn kettle_base_dirs_from(
    mut lookup: impl FnMut(&str) -> Option<std::ffi::OsString>,
    temp_dir: std::path::PathBuf,
) -> Vec<std::path::PathBuf> {
    let mut bases = vec![temp_dir];
    let mut env_path = |key: &str| {
        lookup(key)
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    // Windows config and cache resolvers use different bases.
    for key in [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        bases.extend(env_path(key));
    }
    if let Some(home) = env_path("HOME") {
        bases.push(home.join(".local/state"));
        bases.push(home.join(".config"));
        bases.push(home.join(".cache"));
    }
    bases.push(std::path::PathBuf::from("/tmp"));
    bases
}

/// Whether `path` is a Kettle namespace directly below a known private base.
/// The name must be `kettle` or the numeric `kettle-<uid>` temp form. Requiring
/// both name and parent prevents an unrelated directory such as a source
/// checkout named `kettle` from being narrowed to `0700`.
pub fn is_kettle_owned_dir_name(path: &Path) -> bool {
    is_kettle_owned_dir_name_in(path, &kettle_base_dirs())
}

fn is_kettle_owned_dir_name_in(path: &Path, bases: &[std::path::PathBuf]) -> bool {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name == "kettle"
                || name.strip_prefix("kettle-").is_some_and(|rest| {
                    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
                })
        });
    if !named {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    bases.iter().any(|base| base == parent)
}

/// Verify that `directory` is safe to read private, executable configuration
/// from without creating or modifying anything.
///
/// This is the read-only counterpart to [`create_private_dirs`]. It reuses the
/// same descriptor/handle-based parent-chain checks as the private-file APIs:
/// Unix rejects untrusted ownership, writable directory edges and
/// user-controlled symlink ancestors; macOS additionally rejects extended ACL
/// mutation grants to principals other than this user, root, wheel, or local
/// administrators; Windows rejects reparse points and DACLs that grant
/// path/content mutation to an untrusted principal. The directory itself is
/// the target edge, so an untrusted peer must not be able to replace files
/// inside it.
///
/// The synthetic leaf is never opened. It only lets the existing parent guard
/// validate `directory` as the immediate parent while retaining the guard's
/// race-resistant, platform-specific implementation.
pub fn validate_trusted_directory(directory: &Path) -> io::Result<()> {
    let guard = guard_private_parent(&directory.join(".kettle-directory-trust-check"))?;
    guard.verify_directory()
}

/// Open a regular file for reading while holding its verified parent chain.
///
/// Unlike the private-file APIs, this does not change the leaf's permissions:
/// configuration files are commonly `0644` on Unix or inherit trusted
/// Administrator/SYSTEM entries on Windows. It does reject a leaf that an
/// untrusted principal can modify, a reparse/symlink leaf, or a multiply-linked
/// leaf, and verifies that the parent path still names the held directories
/// after the open completes.
pub fn open_trusted_file_read(path: &Path) -> io::Result<File> {
    open_trusted_file_read_impl(path)
}

/// Open an implicitly discovered trusted file, following at most its leaf link.
///
/// A dotfile manager commonly installs `config` or `init.lua` as a symbolic
/// link. Following that link is safe only when the link itself has trusted
/// provenance: an older kettle may first repair a `0775` directory in which a
/// group peer could already have planted a link. This operation validates and
/// holds the requested leaf while resolving it, then applies
/// [`open_trusted_file_read`] to the resolved regular target. The returned path
/// names the object represented by the returned handle and is suitable for
/// diagnostics.
pub fn open_trusted_file_read_following_leaf(
    path: &Path,
) -> io::Result<(File, std::path::PathBuf)> {
    open_trusted_file_read_following_leaf_impl(path)
}

/// Create `directory` and set every Kettle-owned component to `0700` rather
/// than inheriting a permissive process umask. Existing user-owned components
/// are repaired from the outer namespace through the leaf. The final component
/// is opened with `O_NOFOLLOW`; ancestor links resolve to their real directory.
pub fn create_private_dirs(directory: &Path) -> io::Result<()> {
    create_private_dirs_impl(directory)
}

#[cfg(unix)]
fn create_private_dirs_impl(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let private = |path: &Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    };

    // The outermost kettle-named directory at or above `directory`.
    // `DirBuilder::mode` applies to every directory a recursive create makes,
    // so creating from there down names the mode for the whole kettle-owned
    // run in one call.
    //
    // The walk starts at `directory` itself, not its parent: three call sites
    // pass a kettle-named directory AS the target rather than as an ancestor —
    // the config write-back and the update-check cache both hand over
    // `~/.config/kettle` directly. Starting one level up left those with no
    // owned directory at all, so the repair below never ran for the case it
    // exists for, and a `~/.config/kettle` an earlier run left at 0775 stayed
    // there.
    let mut owned: Option<&Path> = None;
    let mut cursor = Some(directory);
    while let Some(path) = cursor {
        if is_kettle_owned_dir_name(path) {
            owned = Some(path);
        }
        cursor = path.parent();
    }

    let Some(owned) = owned else {
        if let Some(parent) = directory.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return private(directory);
    };

    if let Some(root) = owned.parent() {
        std::fs::create_dir_all(root)?;
    }
    private(directory)?;

    // Repair every component from `owned` down. A fresh create is already
    // correct — the recursive builder above names the mode for each directory
    // it makes — so this only affects components an earlier run left behind,
    // which a create against an existing path silently leaves alone.
    let mut repair: Vec<&Path> = Vec::new();
    let mut cursor = Some(directory);
    while let Some(path) = cursor {
        repair.push(path);
        if path == owned {
            break;
        }
        cursor = path.parent();
    }
    // Outermost first. Repairing leaf-to-root opens each deeper path while its
    // ancestors are still group-writable, which is a window a group peer can
    // use to swap one — and `O_NOFOLLOW` only protects the final component, so
    // the redirected open would look legitimate. Narrowing each ancestor before
    // descending through it closes that ordering.
    for path in repair.iter().rev() {
        repair_private_dir_mode(path)?;
    }
    Ok(())
}

/// Set one existing directory to `0700`, through a file descriptor rather than
/// a path.
///
/// Path-based `set_permissions` follows symlinks and re-resolves the name, so
/// the inspect-then-chmod pair is two lookups an attacker could interleave.
/// That is hard to reach here — the directories being repaired sit under
/// `$XDG_RUNTIME_DIR` or `~/.config`, and the one under a shared `/tmp` is
/// protected by the sticky bit plus the owner check below — but opening once
/// with `O_NOFOLLOW | O_DIRECTORY` and calling `fchmod` removes the question
/// rather than arguing it. A symlink at the path fails the open outright.
///
/// A missing directory is not an error: a concurrent teardown between the
/// create above and this call is the same outcome as never having existed.
#[cfg(unix)]
fn repair_private_dir_mode(path: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let directory = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(directory) => directory,
        // ELOOP/ENOTDIR: the name is a symlink, or not a directory at all.
        // Leave it to the ownership checks at the call site rather than
        // chmod'ing a target somebody else chose. Matched on raw errno because
        // the matching `ErrorKind`s are unstable on this MSRV.
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOENT | libc::ELOOP | libc::ENOTDIR)
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    // Both checks now read the object we hold open, not the name.
    let metadata = directory.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 == 0 {
        return Ok(());
    }
    // SAFETY: `directory` owns a live descriptor for the duration of the call.
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dirs_impl(directory: &Path) -> io::Result<()> {
    std::fs::create_dir_all(directory)
}

/// Create a new private regular file without an inherited-permission window.
///
/// The operation fails when `path` already exists. Unix applies mode `0600`
/// in the creating `open(2)` call. Windows supplies an explicit protected
/// current-user security descriptor to `CreateFileW`.
pub fn create_private_file_new(path: &Path) -> io::Result<File> {
    create_private_file_new_impl(path)
}

/// Create a new owner-only file at a path the user explicitly selected.
///
/// Unlike [`create_private_file_new`], this does not create or require private
/// ancestor directories. The parent must already exist, and the leaf is still
/// created atomically (`O_EXCL` / `CREATE_NEW`) with owner-only permissions.
/// This is for exports such as an explicitly named screenshot; private Kettle
/// state must continue to use [`create_private_file_new`].
///
/// The returned guard removes the exact created leaf if it is dropped before
/// [`CreatedUserSelectedFile::persist`], so an encoder failure does not strand
/// a partial file that blocks retry. A writable parent can rename or remove the
/// new directory entry as soon as the instantaneous entry verification ends,
/// including before this function returns and while the caller writes the
/// file. That is inherent in exporting into a directory controlled by another
/// principal; callers must not treat the returned pathname as a durable
/// private-state capability.
pub fn create_user_selected_file_new(path: &Path) -> io::Result<CreatedUserSelectedFile> {
    let (file, cleanup) = create_user_selected_file_new_impl(path)?;
    Ok(CreatedUserSelectedFile::new(file, cleanup))
}

/// Create an owner-only sibling for a streamed export, publishing it at `path`
/// only after the caller finishes writing and calls
/// [`StagedUserSelectedFile::publish`].
///
/// The parent must already exist. The final operation is an atomic no-replace
/// hard link, with an atomic no-replace rename fallback on filesystems that do
/// not implement hard links, so readers never observe partial bytes and a
/// concurrent creator wins without being overwritten. This is the streaming
/// counterpart to `atomic_create_new`: it avoids buffering a potentially large
/// export in RAM.
pub fn stage_user_selected_file_new(path: &Path) -> io::Result<StagedUserSelectedFile> {
    stage_user_selected_file_new_with(path, || {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|error| {
            io::Error::other(format!(
                "secure randomness for export staging failed: {error}"
            ))
        })?;
        Ok(nonce)
    })
}

fn stage_user_selected_file_new_with(
    path: &Path,
    next_nonce: impl FnMut() -> io::Result<[u8; 16]>,
) -> io::Result<StagedUserSelectedFile> {
    stage_user_selected_file_new_with_mode(path, next_nonce, HardLinkMode::Native)
}

fn stage_user_selected_file_new_with_mode(
    path: &Path,
    mut next_nonce: impl FnMut() -> io::Result<[u8; 16]>,
    hard_link_mode: HardLinkMode,
) -> io::Result<StagedUserSelectedFile> {
    const ATTEMPTS: usize = 32;

    #[cfg(windows)]
    windows::validate_user_selected_file_path(path)?;
    let destination = std::path::absolute(path)?;
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "user-selected output has no parent: {}",
                destination.display()
            ),
        )
    })?;
    for _ in 0..ATTEMPTS {
        let nonce = next_nonce()?;
        let mut suffix = String::with_capacity(nonce.len() * 2);
        use std::fmt::Write as _;
        for byte in nonce {
            write!(&mut suffix, "{byte:02x}").expect("writing to String cannot fail");
        }
        let staged_path = parent.join(format!(".kettle-export-{suffix}.tmp"));
        let (file, cleanup) = match create_user_selected_file_new_impl(&staged_path) {
            Ok(created) => created,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let staged = CreatedUserSelectedFile::new(file, cleanup);
        match create_user_selected_file_publish_impl(
            &staged_path,
            &destination,
            &staged,
            hard_link_mode,
        ) {
            Ok(publish) => return Ok(StagedUserSelectedFile::new(staged, publish)),
            Err(error) => {
                return Err(with_cleanup_error(
                    error,
                    staged.discard(),
                    "publication setup",
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique user-selected staging file",
    ))
}

/// Open a private regular file for reading and writing, creating it if absent.
///
/// Existing Unix symbolic links and Windows reparse points are rejected.
/// On Windows, a broad existing file is hardened only when its owner matches
/// the effective token's default owner SID.
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
/// full-access ACE for the effective token user, but authorizes that rewrite
/// only when the object's owner matches the effective token's `TokenOwner` SID.
/// Objects owned by any other principal are rejected.
pub fn restrict_private_file(file: &File) -> io::Result<()> {
    restrict_private_object(file)
}

/// Remove the private file represented by both `file` and `path`.
///
/// `file` must have been returned by [`open_existing_private_file`]. The
/// operation keeps that exact object open while validating the path. Windows
/// marks the object for deletion through a reopened handle; Unix unlinks the
/// verified leaf relative to its held parent directory. A path that no longer
/// identifies `file` is rejected.
pub fn remove_open_private_file(file: File, path: &Path) -> io::Result<()> {
    remove_open_private_file_impl(file, path)
}

/// Best-effort removal of a file still held by its creation handle.
///
/// `file` must have been returned by [`create_private_file_new`]. This is the
/// failure-path counterpart to [`remove_open_private_file`]: Windows creation
/// handles intentionally omit delete sharing, so they must be marked for
/// deletion directly instead of being passed through that reopen-based API.
pub fn discard_created_private_file(file: File, path: &Path) {
    let _ = discard_created_private_file_checked(file, path);
}

/// Remove a file through its creation handle, reporting cleanup failure.
///
/// This is the observable form of [`discard_created_private_file`] for callers
/// that must not silently strand a partial output.
pub fn discard_created_private_file_checked(file: File, path: &Path) -> io::Result<()> {
    discard_created_private_file_checked_impl(file, path)
}

/// Enumerate, open, and remove matching children relative to the held parent
/// capability. The callback sees names only; it cannot redirect an operation
/// through a replacement pathname.
#[cfg(unix)]
pub(super) fn reap_guarded_children<F>(
    guard: &PrivateParentGuard,
    max_scan: usize,
    max_remove: usize,
    predicate: F,
) -> io::Result<usize>
where
    F: FnMut(&std::ffi::OsStr) -> bool,
{
    guard.reap_matching_children(max_scan, max_remove, predicate)
}

#[cfg(windows)]
pub(super) fn reap_guarded_children<F>(
    guard: &PrivateParentGuard,
    max_scan: usize,
    max_remove: usize,
    predicate: F,
) -> io::Result<usize>
where
    F: FnMut(&std::ffi::OsStr) -> bool,
{
    guard.reap_matching_children(max_scan, max_remove, predicate)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn reap_guarded_children<F>(
    guard: &PrivateParentGuard,
    max_scan: usize,
    max_remove: usize,
    predicate: F,
) -> io::Result<usize>
where
    F: FnMut(&std::ffi::OsStr) -> bool,
{
    let _ = (guard, max_scan, max_remove, predicate);
    Ok(0)
}

#[cfg(unix)]
fn create_private_file_new_impl(path: &Path) -> io::Result<File> {
    unix::create_private_file_new(path, false)
}

#[cfg(unix)]
fn create_user_selected_file_new_impl(path: &Path) -> io::Result<(File, CreatedFileCleanup)> {
    unix::create_user_selected_file_new(path)
}

#[cfg(unix)]
fn create_user_selected_file_publish_impl(
    staged: &Path,
    destination: &Path,
    file: &File,
    hard_link_mode: HardLinkMode,
) -> io::Result<CreatedFilePublish> {
    unix::create_user_selected_file_publish(staged, destination, file, hard_link_mode)
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
fn open_trusted_file_read_impl(path: &Path) -> io::Result<File> {
    unix::open_trusted_file_read(path)
}

#[cfg(unix)]
fn open_trusted_file_read_following_leaf_impl(
    path: &Path,
) -> io::Result<(File, std::path::PathBuf)> {
    unix::open_trusted_file_read_following_leaf(path)
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
fn create_user_selected_file_new_impl(path: &Path) -> io::Result<(File, CreatedFileCleanup)> {
    windows::create_user_selected_file_new(path)
}

#[cfg(windows)]
fn create_user_selected_file_publish_impl(
    staged: &Path,
    destination: &Path,
    file: &File,
    hard_link_mode: HardLinkMode,
) -> io::Result<CreatedFilePublish> {
    windows::create_user_selected_file_publish(staged, destination, file, hard_link_mode)
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
fn open_trusted_file_read_impl(path: &Path) -> io::Result<File> {
    windows::open_trusted_file_read(path)
}

#[cfg(windows)]
fn open_trusted_file_read_following_leaf_impl(
    path: &Path,
) -> io::Result<(File, std::path::PathBuf)> {
    windows::open_trusted_file_read_following_leaf(path)
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
fn create_user_selected_file_new_impl(path: &Path) -> io::Result<(File, CreatedFileCleanup)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "user-selected output parent does not exist: {}",
                parent.display()
            ),
        ));
    }
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?;
    require_regular_file(&file, path)?;
    let cleanup_path = path.to_path_buf();
    let expected = file.metadata()?;
    let cleanup = Box::new(move |file: &File| {
        let same = file.metadata().ok().is_some_and(|opened| {
            std::fs::symlink_metadata(&cleanup_path)
                .ok()
                .is_some_and(|current| {
                    current.file_type().is_file()
                        && opened.len() == expected.len()
                        && opened.modified().ok() == current.modified().ok()
                })
        });
        if same {
            std::fs::remove_file(&cleanup_path)?;
        }
        Ok(())
    });
    Ok((file, cleanup))
}

#[cfg(not(any(unix, windows)))]
fn create_user_selected_file_publish_impl(
    staged: &Path,
    destination: &Path,
    _file: &File,
    _hard_link_mode: HardLinkMode,
) -> io::Result<CreatedFilePublish> {
    let staged = staged.to_path_buf();
    let destination = destination.to_path_buf();
    Ok(Box::new(move |_| {
        std::fs::hard_link(staged, destination)?;
        Ok(CreatedFilePublication::Linked)
    }))
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
fn open_trusted_file_read_impl(path: &Path) -> io::Result<File> {
    let file = File::open(path)?;
    require_regular_file(&file, path)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_trusted_file_read_following_leaf_impl(
    path: &Path,
) -> io::Result<(File, std::path::PathBuf)> {
    let resolved = std::fs::canonicalize(path)?;
    let file = open_trusted_file_read_impl(&resolved)?;
    Ok((file, resolved))
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
fn remove_open_private_file_impl(file: File, path: &Path) -> io::Result<()> {
    unix::remove_open_private_file(file, path)
}

#[cfg(unix)]
fn discard_created_private_file_checked_impl(file: File, path: &Path) -> io::Result<()> {
    unix::discard_created_private_file_checked(file, path)
}

#[cfg(windows)]
fn restrict_private_object(file: &File) -> io::Result<()> {
    windows::restrict_private_object(file)
}

#[cfg(windows)]
fn remove_open_private_file_impl(file: File, path: &Path) -> io::Result<()> {
    windows::remove_open_private_file(file, path)
}

#[cfg(windows)]
fn discard_created_private_file_checked_impl(file: File, _path: &Path) -> io::Result<()> {
    windows::discard_created_private_file_checked(file)
}

#[cfg(not(any(unix, windows)))]
fn restrict_private_object(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn remove_open_private_file_impl(file: File, path: &Path) -> io::Result<()> {
    require_regular_file(&file, path)?;
    let opened = file.metadata()?;
    let current = std::fs::symlink_metadata(path)?;
    if !current.file_type().is_file()
        || opened.len() != current.len()
        || opened.modified().ok() != current.modified().ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private path no longer identifies the open file",
        ));
    }
    drop(file);
    std::fs::remove_file(path)
}

#[cfg(not(any(unix, windows)))]
fn discard_created_private_file_checked_impl(file: File, path: &Path) -> io::Result<()> {
    drop(file);
    std::fs::remove_file(path)
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
pub(super) fn owned_by_current_token_owner(file: &File) -> io::Result<bool> {
    windows::owned_by_current_token_owner(file)
}

#[cfg(all(test, windows))]
pub(super) fn grant_world_write_for_test(file: &File) -> io::Result<()> {
    windows::grant_world_write_for_test(file)
}

#[cfg(all(test, windows))]
pub(super) fn grant_world_all_for_test(file: &File) -> io::Result<()> {
    windows::grant_world_all_for_test(file)
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

    pub(super) fn verify_directory(&self) -> io::Result<()> {
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
pub(crate) mod unix {
    use super::*;
    use std::ffi::{CString, OsStr, OsString};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _};
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
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

    const PRIVATE_FILE_MODE: u32 = 0o600;

    /// A same-mode `fchmod` is not free: macOS FSEvents reports it as both a
    /// metadata and data change, which can feed a watched private file back to
    /// its reader indefinitely. Special permission bits still require the
    /// real hardening call; file-type bits do not.
    pub(super) fn private_mode_needs_hardening(mode: u32) -> bool {
        mode & 0o7777 != PRIVATE_FILE_MODE
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

    pub(super) fn require_trusted_symbolic_link(
        uid: u32,
        links: libc::nlink_t,
        path: &Path,
    ) -> io::Result<()> {
        if !trusted_identity(uid) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted symbolic link is owned by an untrusted user: {}",
                    path.display()
                ),
            ));
        }
        if links != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted symbolic link has an unexpected hard-link count: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    /// The remedy sentence, but only when the offending directory is one kettle
    /// named for itself.
    ///
    /// `chmod 700` is right for `<base>/kettle`, which kettle created and which
    /// nothing else has business writing to. It is bad advice for a directory
    /// that merely happens to sit on the path — running a live-UI scenario from
    /// a checkout produced "restore it with `chmod 700 /home/user/Repos`",
    /// telling the user to lock down every project they own so kettle could
    /// write a screenshot. Refusing is still correct there; instructing is not.
    pub(super) fn chmod_remedy(path: &Path) -> String {
        if super::is_kettle_owned_dir_name(path) {
            format!(" — restore it with `chmod 700 {}`", path.display())
        } else {
            String::new()
        }
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
        let parent_path = path.parent().unwrap_or(Path::new("/"));
        let detail = if !trusted_identity(parent.uid()) {
            format!(
                "parent {} is owned by uid {}; expected uid {} or 0",
                parent_path.display(),
                parent.uid(),
                current_user()
            )
        } else if !sticky {
            // Name the remedy here too. This is the message the umask bug
            // actually produced — a group-writable *ancestor* — while the
            // remedy was first added only to the leaf-policy check below, so
            // the case that motivated it was the one case that never showed it.
            format!(
                "parent {} has mode {:04o}; group/other write bits are unsafe (set an explicit directory mode instead of relying on the process umask){}",
                parent_path.display(),
                parent.mode() & 0o7777,
                chmod_remedy(parent_path)
            )
        } else {
            format!(
                "sticky parent {} contains a child owned by untrusted uid {}",
                parent_path.display(),
                child.uid()
            )
        };
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private path crosses an untrusted directory edge at {}: {detail}",
                path.display(),
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

    fn open_readable_directory(directory: &File) -> io::Result<File> {
        let current = c_name(OsStr::new("."), "current directory")?;
        // SAFETY: the held directory fd and NUL-terminated name are valid.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                current.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn sync_directory(directory: &File) -> io::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            open_readable_directory(directory)?.sync_all()
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
        #[cfg(target_os = "macos")]
        macos_acl::require_no_untrusted_mutation_grant(
            &root,
            Path::new("/"),
            macos_acl::DANGEROUS_DIRECTORY_RIGHTS,
            "trusted directory chain",
        )?;
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
            #[cfg(target_os = "macos")]
            macos_acl::require_no_untrusted_mutation_grant(
                &child,
                &cursor,
                macos_acl::DANGEROUS_DIRECTORY_RIGHTS,
                "trusted directory chain",
            )?;
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
        directory: File,
    }

    impl PrivateParentGuard {
        pub(super) fn new(path: &Path) -> io::Result<Self> {
            let (parent, leaf) = split_path(path)?;
            let (mut handles, identities) = open_verified_parent_chain(&parent)?;
            // Operations are relative to the immediate parent capability. The
            // ancestor handles are not needed after construction: `verify`
            // reopens the complete chain and compares every identity before or
            // after publication. Retaining all of them made each guard consume
            // O(path depth) descriptors and pushed the parallel config suite
            // past macOS's 256-FD soft limit; one held parent is sufficient and
            // makes steady descriptor use O(1).
            let directory = handles
                .pop()
                .expect("the verified chain always contains the root");
            drop(handles);
            Ok(Self {
                path: parent,
                leaf,
                identities,
                directory,
            })
        }

        fn directory(&self) -> &File {
            &self.directory
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
            // Name the remedy. This refusal is usually the first and only
            // thing a user learns about a feature that has quietly turned
            // itself off, and "writable by an untrusted principal" does not
            // tell them that one chmod restores it.
            let remedy = if trusted_identity(parent.uid()) {
                chmod_remedy(&self.path)
            } else {
                String::new()
            };
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private parent is writable by an untrusted principal: {} (mode {:o}){remedy}",
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

        pub(crate) fn verify_directory(&self) -> io::Result<()> {
            self.require_leaf_policy(current_user())?;
            // `open_verified_parent_chain`, used by `verify`, also rechecks
            // macOS mutation ACLs on every reopened component. Keeping that
            // check in the reopen avoids both stale ACL observations and a
            // retained descriptor per ancestor.
            self.verify()
        }

        fn entry_matches(&self, file: &File, name: &OsStr) -> io::Result<bool> {
            let expected = FileIdentity::from_metadata(&file.metadata()?);
            Ok(entry_stat(self.directory(), name)?
                .is_some_and(|stat| FileIdentity::from_stat(&stat) == expected))
        }

        fn unlink_open_file(&self, file: &File, name: &OsStr) -> io::Result<()> {
            if !self.entry_matches(file, name)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private path no longer identifies the open file",
                ));
            }
            let name = c_name(name, "private file name")?;
            // SAFETY: the directory fd and NUL-terminated name are valid.
            if unsafe { libc::unlinkat(self.directory().as_raw_fd(), name.as_ptr(), 0) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        fn unlink_if_same(&self, file: &File, name: &OsStr) {
            let _ = self.unlink_open_file(file, name);
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

        pub(super) fn reap_matching_children<F>(
            &self,
            max_scan: usize,
            max_remove: usize,
            mut predicate: F,
        ) -> io::Result<usize>
        where
            F: FnMut(&OsStr) -> bool,
        {
            self.require_leaf_policy(current_user())?;
            // Linux holds the verified directory chain with O_PATH, which
            // fdopendir cannot enumerate. Open "." relative to the retained
            // capability to get a readable descriptor with an independent
            // directory offset. No operation follows the replaceable path.
            let enumeration = open_readable_directory(self.directory())?.into_raw_fd();
            // SAFETY: fdopendir takes ownership of `enumeration` on success.
            let stream = unsafe { libc::fdopendir(enumeration) };
            if stream.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: fdopendir did not consume the descriptor on failure.
                unsafe {
                    libc::close(enumeration);
                }
                return Err(error);
            }
            struct DirectoryStream(*mut libc::DIR);
            impl Drop for DirectoryStream {
                fn drop(&mut self) {
                    // SAFETY: this stream is owned and closed exactly once.
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = DirectoryStream(stream);
            let mut scanned = 0usize;
            let mut removed = 0usize;
            while scanned < max_scan && removed < max_remove {
                // SAFETY: `stream` remains live for the loop and readdir's
                // returned entry is borrowed until the next call.
                let entry = unsafe { libc::readdir(stream.0) };
                if entry.is_null() {
                    break;
                }
                // SAFETY: POSIX dirent names are NUL terminated.
                let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                scanned += 1;
                let name = OsStr::from_bytes(name);
                if !predicate(name) {
                    continue;
                }
                let candidate = self.path.join(name);
                let Ok(file) = open_at(self, &candidate, libc::O_RDWR, 0) else {
                    continue;
                };
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                if require_user_owned_regular(&file, &candidate).is_err()
                    || self.require_leaf_policy(metadata.uid()).is_err()
                {
                    continue;
                }
                if self.unlink_open_file(&file, name).is_ok() {
                    removed += 1;
                }
            }
            Ok(removed)
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
        #[cfg(target_os = "macos")]
        macos_acl::clear_extended_acl(file)?;
        if !private_mode_needs_hardening(metadata.mode()) {
            return Ok(());
        }
        file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        if !private_mode_needs_hardening(file.metadata()?.mode()) {
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

    pub(super) fn create_user_selected_file_new(
        path: &Path,
    ) -> io::Result<(File, CreatedFileCleanup)> {
        create_user_selected_file_new_with(path, || {})
    }

    pub(super) fn create_user_selected_file_publish(
        staged: &Path,
        destination: &Path,
        file: &File,
        hard_link_mode: HardLinkMode,
    ) -> io::Result<CreatedFilePublish> {
        let staged = std::path::absolute(staged)?;
        let destination = std::path::absolute(destination)?;
        let staged_parent = staged.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "staged export has no parent")
        })?;
        if destination.parent() != Some(staged_parent) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staged export and destination must share one parent",
            ));
        }
        let staged_leaf = staged
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "staged export has no file name",
                )
            })?
            .to_os_string();
        let destination_leaf = destination
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "export destination has no file name",
                )
            })?
            .to_os_string();
        let staged_c = c_name(&staged_leaf, "staged export file name")?;
        let destination_c = c_name(&destination_leaf, "export destination file name")?;
        let parent = open_user_selected_parent(staged_parent)?;
        let expected = FileIdentity::from_metadata(&file.metadata()?);
        let staged_matches = entry_stat(&parent, &staged_leaf)?
            .is_some_and(|stat| FileIdentity::from_stat(&stat) == expected);
        if !staged_matches {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "staged export path no longer identifies the open file",
            ));
        }
        if entry_stat(&parent, &destination_leaf)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "export destination already exists",
            ));
        }
        let current_parent = open_user_selected_parent(staged_parent)?;
        if FileIdentity::from_metadata(&current_parent.metadata()?)
            != FileIdentity::from_metadata(&parent.metadata()?)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "user-selected output parent changed before publication",
            ));
        }
        Ok(Box::new(move |file: &File| {
            if FileIdentity::from_metadata(&file.metadata()?) != expected
                || !entry_stat(&parent, &staged_leaf)?
                    .is_some_and(|stat| FileIdentity::from_stat(&stat) == expected)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "staged export changed before publication",
                )
                .into());
            }
            // Prefer a hard link because it keeps the staging cleanup path
            // available until every postcondition is checked. Some ordinary
            // export filesystems do not implement hard links, so supported
            // kernels fall back to an equally atomic no-replace rename.
            let publication = publish_with_atomic_fallback(
                || match hard_link_mode {
                    HardLinkMode::Native => {
                        if unsafe {
                            libc::linkat(
                                parent.as_raw_fd(),
                                staged_c.as_ptr(),
                                parent.as_raw_fd(),
                                destination_c.as_ptr(),
                                0,
                            )
                        } == 0
                        {
                            Ok(())
                        } else {
                            Err(io::Error::last_os_error())
                        }
                    }
                    #[cfg(test)]
                    HardLinkMode::ForceUnsupported => Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "injected hard-link rejection",
                    )),
                },
                || {
                    #[cfg(any(target_os = "linux", target_os = "android"))]
                    let renamed = unsafe {
                        libc::renameat2(
                            parent.as_raw_fd(),
                            staged_c.as_ptr(),
                            parent.as_raw_fd(),
                            destination_c.as_ptr(),
                            libc::RENAME_NOREPLACE,
                        )
                    };
                    #[cfg(target_os = "macos")]
                    let renamed = {
                        unsafe extern "C" {
                            fn renameatx_np(
                                from_dir: libc::c_int,
                                from: *const libc::c_char,
                                to_dir: libc::c_int,
                                to: *const libc::c_char,
                                flags: libc::c_uint,
                            ) -> libc::c_int;
                        }
                        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
                        unsafe {
                            renameatx_np(
                                parent.as_raw_fd(),
                                staged_c.as_ptr(),
                                parent.as_raw_fd(),
                                destination_c.as_ptr(),
                                RENAME_EXCL,
                            )
                        }
                    };
                    #[cfg(not(any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "macos"
                    )))]
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "this platform has no atomic no-replace rename API",
                    ));
                    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
                    if renamed == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                },
            )?;
            let post_publish = (|| {
                let published = entry_stat(&parent, &destination_leaf)?
                    .is_some_and(|stat| FileIdentity::from_stat(&stat) == expected);
                if !published {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "published export path does not identify the staged file",
                    ));
                }
                sync_directory(&parent)
            })();
            post_publish.map_err(CreatedFilePublishError::published)?;
            Ok(publication)
        }))
    }

    fn open_user_selected_parent(path: &Path) -> io::Result<File> {
        let parent_c = c_name(path.as_os_str(), "user-selected output parent")?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let access = libc::O_PATH;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let access = libc::O_RDONLY;
        // SAFETY: the path is NUL-terminated, and a successful open transfers
        // ownership of the descriptor.
        let fd = unsafe {
            libc::open(
                parent_c.as_ptr(),
                access | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: open returned a new owned descriptor.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    #[cfg(target_os = "macos")]
    fn create_user_selected_file_without_acl_window(
        parent: &File,
        leaf_c: &CString,
        path: &Path,
    ) -> io::Result<File> {
        unsafe extern "C" {
            fn renameatx_np(
                from_dir: libc::c_int,
                from: *const libc::c_char,
                to_dir: libc::c_int,
                to: *const libc::c_char,
                flags: libc::c_uint,
            ) -> libc::c_int;
        }

        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        const STAGING_ATTEMPTS: usize = 32;

        let payload_c = CString::new("payload").expect("static payload name has no NUL");
        for _ in 0..STAGING_ATTEMPTS {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|error| {
                io::Error::other(format!(
                    "secure randomness for screenshot staging failed: {error}"
                ))
            })?;
            let mut suffix = String::with_capacity(nonce.len() * 2);
            use std::fmt::Write as _;
            for byte in nonce {
                write!(&mut suffix, "{byte:02x}").expect("writing to String cannot fail");
            }
            let staging_name = OsString::from(format!(".kettle-screenshot-{suffix}"));
            let staging_c = c_name(&staging_name, "screenshot staging directory")?;
            // The directory is empty while its inherited ACL is removed. The
            // PNG is created only after this held directory object is 0700 and
            // ACL-free, then published with an atomic no-replace rename.
            if unsafe {
                libc::mkdirat(
                    parent.as_raw_fd(),
                    staging_c.as_ptr(),
                    0o700 as libc::mode_t,
                )
            } != 0
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }

            let stage_fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    staging_c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if stage_fd < 0 {
                // We have no descriptor identity with which to distinguish our
                // directory from a name swapped in by another writer. Leaving
                // an empty, randomly named 0700 directory is safer than
                // unlinking an unverified replacement.
                let error = io::Error::last_os_error();
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "screenshot staging directory could not be opened and was left in place for safety: {error}"
                    ),
                ));
            }
            let staging = unsafe { File::from_raw_fd(stage_fd) };
            let staging_identity = FileIdentity::from_metadata(&staging.metadata()?);
            let remove_staging_if_same = || -> io::Result<()> {
                let linked = entry_stat(parent, &staging_name)?
                    .is_some_and(|stat| FileIdentity::from_stat(&stat) == staging_identity);
                if linked
                    && unsafe {
                        libc::unlinkat(parent.as_raw_fd(), staging_c.as_ptr(), libc::AT_REMOVEDIR)
                    } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            };

            let created = (|| {
                let metadata = staging.metadata()?;
                if !metadata.file_type().is_dir() || metadata.uid() != current_user() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "screenshot staging directory is not owned by the effective user",
                    ));
                }
                // Mode bits and ACLs are independent on Darwin; both must be
                // secured before the first content-bearing file is created.
                if unsafe { libc::fchmod(staging.as_raw_fd(), 0o700) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                macos_acl::clear_extended_acl(&staging)?;

                let fd = unsafe {
                    libc::openat(
                        staging.as_raw_fd(),
                        payload_c.as_ptr(),
                        libc::O_RDWR
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | libc::O_NOFOLLOW
                            | libc::O_CLOEXEC,
                        PRIVATE_FILE_MODE as libc::c_uint,
                    )
                };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                let file = unsafe { File::from_raw_fd(fd) };
                require_user_owned_regular(&file, path)?;
                restrict_private_object(&file)?;

                if unsafe {
                    renameatx_np(
                        staging.as_raw_fd(),
                        payload_c.as_ptr(),
                        parent.as_raw_fd(),
                        leaf_c.as_ptr(),
                        RENAME_EXCL,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(file)
            })();
            let payload_cleanup = if created.is_err() {
                if unsafe { libc::unlinkat(staging.as_raw_fd(), payload_c.as_ptr(), 0) } == 0 {
                    Ok(())
                } else {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                }
            } else {
                Ok(())
            };
            let staging_cleanup = remove_staging_if_same();
            match created {
                Err(error) => {
                    let error = with_cleanup_error(error, payload_cleanup, "staging payload");
                    return Err(with_cleanup_error(
                        error,
                        staging_cleanup,
                        "staging directory",
                    ));
                }
                Ok(file) => {
                    if let Err(error) = staging_cleanup {
                        let expected = FileIdentity::from_metadata(&file.metadata()?);
                        let published = entry_stat(parent, path.file_name().unwrap_or_default())?
                            .is_some_and(|stat| FileIdentity::from_stat(&stat) == expected);
                        let final_cleanup = if published
                            && unsafe { libc::unlinkat(parent.as_raw_fd(), leaf_c.as_ptr(), 0) }
                                != 0
                        {
                            Err(io::Error::last_os_error())
                        } else {
                            Ok(())
                        };
                        return Err(with_cleanup_error(error, final_cleanup, "published output"));
                    }
                    return Ok(file);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a screenshot staging directory",
        ))
    }

    pub(super) fn create_user_selected_file_new_with(
        path: &Path,
        after_parent_open: impl FnOnce(),
    ) -> io::Result<(File, CreatedFileCleanup)> {
        let path = std::path::absolute(path)?;
        let leaf = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("user-selected output has no file name: {}", path.display()),
            )
        })?;
        let leaf_c = c_name(leaf, "user-selected output file name")?;
        let parent_path = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("user-selected output has no parent: {}", path.display()),
            )
        })?;
        // Resolve the caller's existing parent exactly once and retain that
        // directory capability through creation and postcondition checks.
        // Following a parent symlink is intentional for an explicit export;
        // the leaf itself is never followed.
        let parent = open_user_selected_parent(parent_path)?;
        let parent_identity = FileIdentity::from_metadata(&parent.metadata()?);
        after_parent_open();
        #[cfg(target_os = "macos")]
        let file = create_user_selected_file_without_acl_window(&parent, &leaf_c, &path)?;
        #[cfg(not(target_os = "macos"))]
        let file = {
            // SAFETY: the held directory and NUL-terminated leaf are valid, and
            // successful openat transfers ownership of the returned descriptor.
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    leaf_c.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    PRIVATE_FILE_MODE as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openat returned a new owned descriptor.
            unsafe { File::from_raw_fd(fd) }
        };
        let result = (|| {
            require_user_owned_regular(&file, &path)?;
            restrict_private_object(&file)?;
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            let linked = entry_stat(&parent, leaf)?
                .is_some_and(|stat| FileIdentity::from_stat(&stat) == identity);
            if !linked {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "user-selected output path no longer refers to the created file",
                ));
            }
            let current_parent = open_user_selected_parent(parent_path)?;
            if FileIdentity::from_metadata(&current_parent.metadata()?) != parent_identity {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "user-selected output parent changed while the file was created",
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            let linked = entry_stat(&parent, leaf)
                .ok()
                .flatten()
                .is_some_and(|stat| {
                    file.metadata().ok().is_some_and(|metadata| {
                        FileIdentity::from_stat(&stat) == FileIdentity::from_metadata(&metadata)
                    })
                });
            if linked {
                // SAFETY: `parent` is held and `leaf_c` is NUL-terminated. The
                // identity check above prevents deleting a replacement entry.
                unsafe {
                    libc::unlinkat(parent.as_raw_fd(), leaf_c.as_ptr(), 0);
                }
            }
            return Err(error);
        }
        let cleanup_leaf = leaf.to_os_string();
        let cleanup_identity = FileIdentity::from_metadata(&file.metadata()?);
        let cleanup: CreatedFileCleanup = Box::new(move |file: &File| {
            let linked = entry_stat(&parent, &cleanup_leaf)?.is_some_and(|stat| {
                file.metadata().ok().is_some_and(|metadata| {
                    FileIdentity::from_stat(&stat) == cleanup_identity
                        && FileIdentity::from_metadata(&metadata) == cleanup_identity
                })
            });
            if linked {
                let cleanup_leaf = c_name(&cleanup_leaf, "user-selected output file name")?;
                // SAFETY: `parent` is retained by this cleanup capability and
                // the identity check above excludes a replacement entry.
                if unsafe { libc::unlinkat(parent.as_raw_fd(), cleanup_leaf.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                // The destination link was made durable by publication. Make
                // retirement of the staging sibling durable as well before a
                // successful publish is reported.
                sync_directory(&parent)?;
            }
            Ok(())
        });
        Ok((file, cleanup))
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

    pub(super) fn open_trusted_file_read(path: &Path) -> io::Result<File> {
        use std::os::unix::fs::MetadataExt as _;

        let guard = PrivateParentGuard::new(path)?;
        let file = open_at(&guard, path, libc::O_RDONLY, 0)?;
        require_regular_file(&file, path)?;
        let metadata = file.metadata()?;
        guard.require_leaf_policy(metadata.uid())?;
        if !trusted_identity(metadata.uid()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted file is owned by an untrusted user: {}",
                    path.display()
                ),
            ));
        }
        if metadata.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted file is writable by an untrusted principal: {} (mode {:o})",
                    path.display(),
                    metadata.mode() & 0o7777
                ),
            ));
        }
        if metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted file has an unexpected hard-link count: {}",
                    path.display()
                ),
            ));
        }
        #[cfg(target_os = "macos")]
        macos_acl::require_no_untrusted_mutation_grant(
            &file,
            path,
            macos_acl::DANGEROUS_FILE_RIGHTS,
            "trusted file",
        )?;
        guard.verify_directory()?;
        if !guard.entry_matches(&file, &guard.leaf)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trusted path changed while it was opened",
            ));
        }
        Ok(file)
    }

    fn read_link_at(guard: &PrivateParentGuard, path: &Path) -> io::Result<PathBuf> {
        const MAX_LINK_BYTES: usize = 64 * 1024;

        let name = guard.validate_path(path)?;
        let name = c_name(&name, "trusted link name")?;
        let mut capacity = 256usize;
        loop {
            let mut bytes = vec![0_u8; capacity];
            // SAFETY: the held parent descriptor and NUL-terminated name are
            // valid, and `bytes` exposes `capacity` writable bytes.
            let read = unsafe {
                libc::readlinkat(
                    guard.directory().as_raw_fd(),
                    name.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if read < 0 {
                return Err(io::Error::last_os_error());
            }
            let read = read as usize;
            if read < bytes.len() {
                bytes.truncate(read);
                return Ok(PathBuf::from(OsString::from_vec(bytes)));
            }
            capacity = capacity.checked_mul(2).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "trusted link target is too long",
                )
            })?;
            if capacity > MAX_LINK_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("trusted link target exceeds {MAX_LINK_BYTES} bytes"),
                ));
            }
        }
    }

    pub(super) fn open_trusted_file_read_following_leaf(
        path: &Path,
    ) -> io::Result<(File, PathBuf)> {
        let guard = PrivateParentGuard::new(path)?;
        let requested = guard.path.join(&guard.leaf);
        let before = entry_stat(guard.directory(), &guard.leaf)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("trusted path does not exist: {}", requested.display()),
            )
        })?;

        if stat_is_regular(&before) {
            drop(guard);
            return Ok((open_trusted_file_read(&requested)?, requested));
        }
        if before.st_mode & libc::S_IFMT != libc::S_IFLNK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "trusted path is not a regular file or symbolic link: {}",
                    requested.display()
                ),
            ));
        }
        guard.require_leaf_policy(before.st_uid)?;
        require_trusted_symbolic_link(before.st_uid, before.st_nlink, &requested)?;

        // Read the link relative to the held immediate parent. Recheck both
        // the parent chain and the link inode before trusting the selected
        // target; a repair from 0775 to 0700 cannot bless a link a group peer
        // planted before the repair.
        let target = read_link_at(&guard, &requested)?;
        guard.verify_directory()?;
        let after = entry_stat(guard.directory(), &guard.leaf)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trusted symbolic link disappeared while it was resolved",
            )
        })?;
        if FileIdentity::from_stat(&before) != FileIdentity::from_stat(&after) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trusted symbolic link changed while it was resolved",
            ));
        }
        let target = if target.is_absolute() {
            target
        } else {
            guard.path.join(target)
        };
        let resolved = std::fs::canonicalize(target)?;
        let file = open_trusted_file_read(&resolved)?;
        Ok((file, resolved))
    }

    #[cfg(target_os = "macos")]
    pub(crate) mod macos_acl {
        use super::*;
        use std::ffi::c_void;

        type Acl = *mut c_void;
        type AclEntry = *mut c_void;

        const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;
        const ACL_FIRST_ENTRY: i32 = 0;
        const ACL_NEXT_ENTRY: i32 = -1;
        const ACL_EXTENDED_ALLOW: i32 = 1;

        const ACL_WRITE_DATA: u64 = 1 << 2;
        const ACL_DELETE: u64 = 1 << 4;
        const ACL_APPEND_DATA: u64 = 1 << 5;
        const ACL_DELETE_CHILD: u64 = 1 << 6;
        const ACL_WRITE_ATTRIBUTES: u64 = 1 << 8;
        const ACL_WRITE_EXTATTRIBUTES: u64 = 1 << 10;
        const ACL_WRITE_SECURITY: u64 = 1 << 12;
        const ACL_CHANGE_OWNER: u64 = 1 << 13;

        pub(super) const DANGEROUS_DIRECTORY_RIGHTS: u64 = ACL_WRITE_DATA
            | ACL_DELETE
            | ACL_APPEND_DATA
            | ACL_DELETE_CHILD
            | ACL_WRITE_ATTRIBUTES
            | ACL_WRITE_EXTATTRIBUTES
            | ACL_WRITE_SECURITY
            | ACL_CHANGE_OWNER;
        pub(super) const DANGEROUS_FILE_RIGHTS: u64 = ACL_WRITE_DATA
            | ACL_DELETE
            | ACL_APPEND_DATA
            | ACL_WRITE_ATTRIBUTES
            | ACL_WRITE_EXTATTRIBUTES
            | ACL_WRITE_SECURITY
            | ACL_CHANGE_OWNER;

        unsafe extern "C" {
            fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> Acl;
            fn acl_init(count: libc::c_int) -> Acl;
            fn acl_set_fd_np(fd: libc::c_int, acl: Acl, kind: libc::c_int) -> libc::c_int;
            fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut AclEntry) -> libc::c_int;
            fn acl_get_tag_type(entry: AclEntry, tag: *mut libc::c_int) -> libc::c_int;
            fn acl_get_permset_mask_np(entry: AclEntry, mask: *mut u64) -> libc::c_int;
            fn acl_get_qualifier(entry: AclEntry) -> *mut c_void;
            fn acl_free(object: *mut c_void) -> libc::c_int;
            fn mbr_uid_to_uuid(uid: libc::uid_t, uuid: *mut [u8; 16]) -> libc::c_int;
            fn mbr_gid_to_uuid(gid: libc::gid_t, uuid: *mut [u8; 16]) -> libc::c_int;
        }

        struct OwnedAcl(Acl);

        impl Drop for OwnedAcl {
            fn drop(&mut self) {
                // SAFETY: acl_get_fd_np returned this allocated ACL.
                let _ = unsafe { acl_free(self.0) };
            }
        }

        pub(crate) fn clear_extended_acl(file: &File) -> io::Result<()> {
            // Darwin ACLs are independent of BSD mode bits. A shared export
            // directory can carry inheritable read ACEs, so a newly created
            // `0600` file is not owner-only until its extended ACL is removed.
            // Inspect first: installing an empty ACL on an already ACL-free
            // file still changes ctime and emits an FSEvents metadata event.
            // Private files are reopened frequently (including lock files), so
            // that apparent no-op would restore the watcher feedback and
            // filesystem churn avoided by `private_mode_needs_hardening`.
            if !has_any_extended_acl(file)? {
                return Ok(());
            }
            // Darwin has no descriptor-based ACL-delete call. Installing an
            // empty extended ACL is the handle-relative equivalent of
            // `chmod -N`, without re-resolving a mutable pathname.
            // SAFETY: acl_init returns an owned ACL or null on error.
            let empty = unsafe { acl_init(0) };
            if empty.is_null() {
                return Err(io::Error::last_os_error());
            }
            let empty = OwnedAcl(empty);
            // SAFETY: `file` owns a live descriptor and `empty` stays allocated
            // for the complete call.
            if unsafe { acl_set_fd_np(file.as_raw_fd(), empty.0, ACL_TYPE_EXTENDED) } == 0 {
                Ok(())
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ENOTSUP) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }

        pub(crate) fn has_any_extended_acl(file: &File) -> io::Result<bool> {
            // SAFETY: `file` owns a live descriptor.
            let raw = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                return if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTSUP)) {
                    Ok(false)
                } else {
                    Err(error)
                };
            }
            let acl = OwnedAcl(raw);
            let mut entry = std::ptr::null_mut();
            // SAFETY: `acl` remains live and `entry` is a writable output slot.
            let found = unsafe { acl_get_entry(acl.0, ACL_FIRST_ENTRY, &mut entry) };
            if found == 0 {
                Ok(true)
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINVAL) {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        }

        fn identity_uuid(id: u32, group: bool) -> io::Result<[u8; 16]> {
            let mut uuid = [0_u8; 16];
            // SAFETY: `uuid` is a writable 16-byte UUID output buffer.
            let status = unsafe {
                if group {
                    mbr_gid_to_uuid(id, &mut uuid)
                } else {
                    mbr_uid_to_uuid(id, &mut uuid)
                }
            };
            if status == 0 {
                Ok(uuid)
            } else {
                Err(io::Error::from_raw_os_error(status))
            }
        }

        fn trusted_qualifier(entry: AclEntry) -> io::Result<bool> {
            // Darwin ACL qualifiers are allocated UUID values. Trust only this
            // user, root, wheel, and the local administrators group; an ACL
            // grant to staff/everyone is precisely the cross-user mutation
            // path this verifier exists to reject.
            // SAFETY: `entry` came from the live ACL iterator.
            let qualifier = unsafe { acl_get_qualifier(entry) };
            if qualifier.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut actual = [0_u8; 16];
            // SAFETY: a Darwin ACL qualifier points to one uuid_t.
            unsafe {
                std::ptr::copy_nonoverlapping(qualifier.cast::<u8>(), actual.as_mut_ptr(), 16);
            }
            // SAFETY: acl_get_qualifier allocated the qualifier.
            let free_status = unsafe { acl_free(qualifier) };
            if free_status != 0 {
                return Err(io::Error::last_os_error());
            }
            let trusted = [
                identity_uuid(current_user(), false)?,
                identity_uuid(0, false)?,
                identity_uuid(0, true)?,
                identity_uuid(80, true)?,
            ];
            Ok(trusted.contains(&actual))
        }

        pub(super) fn require_no_untrusted_mutation_grant(
            object: &File,
            path: &Path,
            dangerous_rights: u64,
            description: &str,
        ) -> io::Result<()> {
            // SAFETY: object owns a live descriptor and ACL_TYPE_EXTENDED is
            // Darwin's descriptor-based NFSv4 ACL class.
            let raw = unsafe { acl_get_fd_np(object.as_raw_fd(), ACL_TYPE_EXTENDED) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                // Darwin reports an object with no extended ACL as ENOENT.
                // A filesystem that cannot store this ACL class cannot carry
                // an out-of-band mutation grant either, so its mode bits are
                // the complete permission set. Every other retrieval failure
                // stays fail-closed.
                if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTSUP)) {
                    return Ok(());
                }
                return Err(error);
            }
            let acl = OwnedAcl(raw);
            let mut entry = std::ptr::null_mut();
            let mut entry_id = ACL_FIRST_ENTRY;
            loop {
                // SAFETY: `acl` stays allocated and `entry` is an output slot.
                let found = unsafe { acl_get_entry(acl.0, entry_id, &mut entry) };
                if found != 0 {
                    let error = io::Error::last_os_error();
                    // Darwin uses EINVAL to signal that the iterator has no
                    // first/next entry; unlike some POSIX ACL APIs, success is
                    // zero rather than one.
                    if error.raw_os_error() == Some(libc::EINVAL) {
                        return Ok(());
                    }
                    return Err(error);
                }
                entry_id = ACL_NEXT_ENTRY;

                let mut tag = 0;
                let mut mask = 0_u64;
                // SAFETY: the iterator returned a live ACL entry.
                if unsafe { acl_get_tag_type(entry, &mut tag) } != 0
                    || unsafe { acl_get_permset_mask_np(entry, &mut mask) } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                if tag == ACL_EXTENDED_ALLOW
                    && mask & dangerous_rights != 0
                    && !trusted_qualifier(entry)?
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "{description} grants mutation rights to an untrusted ACL principal: {}",
                            path.display()
                        ),
                    ));
                }
            }
        }
    }

    pub(super) fn discard_created_private_file_checked(file: File, path: &Path) -> io::Result<()> {
        let guard = PrivateParentGuard::new(path)?;
        let name = guard.validate_path(path)?;
        guard.unlink_open_file(&file, &name)?;
        drop(file);
        Ok(())
    }

    pub(super) fn remove_open_private_file(file: File, path: &Path) -> io::Result<()> {
        require_user_owned_regular(&file, path)?;
        let guard = PrivateParentGuard::new(path)?;
        let name = guard.validate_path(path)?;
        guard.verify()?;
        guard.unlink_open_file(&file, &name)?;
        drop(file);
        Ok(())
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
    #[cfg(test)]
    use windows_sys::Win32::Security::WinWorldSid;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, CreateWellKnownSid,
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, INHERIT_ONLY_ACE, InitializeAcl, InitializeSecurityDescriptor,
        IsValidAcl, IsValidSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        SECURITY_MAX_SID_SIZE, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        SetSecurityDescriptorOwner, TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER,
        TokenOwner, TokenUser, UNPROTECTED_DACL_SECURITY_INFORMATION, WELL_KNOWN_SID_TYPE,
        WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, CreateHardLinkW,
        DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_APPEND_DATA,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_DISPOSITION_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_DATA, FileDispositionInfo,
        FileIdInfo, FileRenameInfo, FileRenameInfoEx, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL,
        ReOpenFile, SetFileInformationByHandle, VOLUME_NAME_GUID, WRITE_DAC, WRITE_OWNER,
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

    struct TokenSid {
        _buffer: Vec<u64>,
        sid: PSID,
    }

    fn token_information_sid(
        token: &TokenHandle,
        information_class: TOKEN_INFORMATION_CLASS,
        sid_from_buffer: impl FnOnce(*const u8) -> PSID,
        sid_kind: &str,
    ) -> io::Result<TokenSid> {
        let mut len = 0_u32;
        // SAFETY: the null-buffer call is the documented size query.
        unsafe {
            GetTokenInformation(
                token.0,
                information_class,
                std::ptr::null_mut(),
                0,
                &mut len,
            )
        };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buffer = vec![0_u64; words];
        // SAFETY: `buffer` is aligned and at least `len` bytes long.
        if unsafe {
            GetTokenInformation(
                token.0,
                information_class,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = sid_from_buffer(buffer.as_ptr().cast());
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the effective token has an invalid {sid_kind} SID"),
            ));
        }
        Ok(TokenSid {
            _buffer: buffer,
            sid,
        })
    }

    fn token_user_sid(token: &TokenHandle) -> io::Result<TokenSid> {
        // SAFETY: a successful TokenUser query writes a TOKEN_USER header at
        // the start of the retained, suitably aligned buffer.
        token_information_sid(
            token,
            TokenUser,
            |buffer| unsafe { (*buffer.cast::<TOKEN_USER>()).User.Sid },
            "user",
        )
    }

    fn token_owner_sid(token: &TokenHandle) -> io::Result<TokenSid> {
        // SAFETY: a successful TokenOwner query writes a TOKEN_OWNER header at
        // the start of the retained, suitably aligned buffer.
        token_information_sid(
            token,
            TokenOwner,
            |buffer| unsafe { (*buffer.cast::<TOKEN_OWNER>()).Owner },
            "owner",
        )
    }

    fn current_user_sid() -> io::Result<TokenSid> {
        let token = effective_token()?;
        token_user_sid(&token)
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
                    owned_by_any_handle_direct(&child, &[current_user.sid]).map_err(|error| {
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
                    true,
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

    pub(super) fn create_user_selected_file_new(
        path: &Path,
    ) -> io::Result<(File, CreatedFileCleanup)> {
        create_user_selected_file_new_with(path, || {})
    }

    pub(super) fn validate_user_selected_file_path(path: &Path) -> io::Result<()> {
        let leaf = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("user-selected output has no file name: {}", path.display()),
            )
        })?;
        private_component(leaf).map(|_| ())
    }

    pub(super) fn create_user_selected_file_publish(
        staged: &Path,
        destination: &Path,
        file: &File,
        hard_link_mode: HardLinkMode,
    ) -> io::Result<CreatedFilePublish> {
        let requested_destination = destination.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "export destination has no file name",
            )
        })?;
        private_component(requested_destination)?;
        let staged = std::path::absolute(staged)?;
        let destination = std::path::absolute(destination)?;
        let parent_path = staged.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "staged export has no parent")
        })?;
        if destination.parent() != Some(parent_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staged export and destination must share one parent",
            ));
        }
        let staged_leaf = staged.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "staged export has no file name",
            )
        })?;
        let destination_leaf = destination.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "export destination has no file name",
            )
        })?;
        let parent = open_user_selected_parent(parent_path)?;
        let expected = file_identity(file)?;
        let mut staged_path = stable_child_path(&parent, staged_leaf)?;
        staged_path.push(0);
        let mut destination_path = stable_child_path(&parent, destination_leaf)?;
        destination_path.push(0);
        let stable_destination = std::path::PathBuf::from(std::ffi::OsString::from_wide(
            &destination_path[..destination_path.len() - 1],
        ));

        let mut inspect = std::fs::OpenOptions::new();
        inspect
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let staged_check = inspect.open(&staged)?;
        require_regular_non_reparse(&staged_check, &staged)?;
        if file_identity(&staged_check)? != expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "staged export path no longer identifies the open file",
            ));
        }
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "export destination already exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        Ok(Box::new(move |file: &File| {
            let _keep_parent_pinned = &parent;
            if file_identity(file)? != expected {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "staged export changed before publication",
                )
                .into());
            }
            // Prefer a hard link so the staging name remains available through
            // the postcondition check. ReFS, FAT-family volumes, and some
            // network exports do not implement it, so fall back to renaming the
            // already-open file with ReplaceIfExists=false. Both operations are
            // atomic and refuse a concurrent destination.
            let publication = publish_with_atomic_fallback(
                || match hard_link_mode {
                    HardLinkMode::Native => {
                        if unsafe {
                            CreateHardLinkW(
                                destination_path.as_ptr(),
                                staged_path.as_ptr(),
                                std::ptr::null(),
                            )
                        } != 0
                        {
                            Ok(())
                        } else {
                            Err(io::Error::last_os_error())
                        }
                    }
                    #[cfg(test)]
                    HardLinkMode::ForceUnsupported => Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "injected hard-link rejection",
                    )),
                },
                || {
                    let destination_name = &destination_path[..destination_path.len() - 1];
                    let name_bytes = destination_name
                        .len()
                        .checked_mul(std::mem::size_of::<u16>())
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "export destination is too long",
                            )
                        })?;
                    let total_bytes = std::mem::size_of::<FILE_RENAME_INFO>()
                        .checked_add(name_bytes)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "export destination is too long",
                            )
                        })?;
                    let words = total_bytes.div_ceil(std::mem::size_of::<u64>());
                    let mut storage = vec![0_u64; words];
                    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
                    unsafe {
                        (*info).Anonymous.ReplaceIfExists = false;
                        (*info).RootDirectory = std::ptr::null_mut();
                        (*info).FileNameLength = u32::try_from(name_bytes).map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "export destination is too long",
                            )
                        })?;
                        std::ptr::copy_nonoverlapping(
                            destination_name.as_ptr(),
                            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                            destination_name.len(),
                        );
                    }
                    let buffer_len = u32::try_from(total_bytes).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "export destination is too long",
                        )
                    })?;
                    if unsafe {
                        SetFileInformationByHandle(
                            file.as_raw_handle() as HANDLE,
                            FileRenameInfo,
                            info.cast(),
                            buffer_len,
                        )
                    } != 0
                    {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                },
            )?;
            let post_publish = (|| {
                let published = inspect.open(&stable_destination)?;
                require_regular_non_reparse(&published, &stable_destination)?;
                if file_identity(&published)? != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "published export path does not identify the staged file",
                    ));
                }
                Ok(())
            })();
            post_publish.map_err(CreatedFilePublishError::published)?;
            Ok(publication)
        }))
    }

    pub(super) fn create_user_selected_file_new_with(
        path: &Path,
        after_parent_open: impl FnOnce(),
    ) -> io::Result<(File, CreatedFileCleanup)> {
        let requested_leaf = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("user-selected output has no file name: {}", path.display()),
            )
        })?;
        // Reuse the private path's Win32 alias rules: in particular, `:` must
        // not turn an export into an alternate stream on an existing file, and
        // an embedded NUL must not truncate the intended leaf. Validate before
        // `absolute`: Win32 path resolution normalizes trailing dots and spaces,
        // which would erase exactly the alias syntax this boundary rejects.
        private_component(requested_leaf)?;
        let path = std::path::absolute(path)?;
        let leaf = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("user-selected output has no file name: {}", path.display()),
            )
        })?;
        let parent_path = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("user-selected output has no parent: {}", path.display()),
            )
        })?;
        let parent = open_user_selected_parent(parent_path)?;
        let parent_identity = file_identity(&parent)?;
        after_parent_open();
        let current_user = current_user_sid()?;
        let mut acl = private_acl(current_user.sid)?;
        let mut descriptor =
            private_security_descriptor(current_user.sid, acl.as_mut_ptr().cast())?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::addr_of_mut!(descriptor).cast(),
            bInheritHandle: 0,
        };
        let mut path_wide = stable_child_path(&parent, leaf)?;
        path_wide.push(0);
        // SAFETY: every pointer is valid for the call. The protected security
        // descriptor is anchored above until CreateFileW returns.
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | DELETE,
                // Omitting delete sharing keeps the selected directory entry
                // stable for the lifetime of the screenshot writer.
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
            if !owned_by_user_handle(&file, current_user.sid)?
                || !has_current_user_only_dacl_handle(&file, current_user.sid)?
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "new user-selected file did not retain its current-user owner and protected DACL",
                ));
            }
            if file_identity(&open_user_selected_parent(parent_path)?)? != parent_identity {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "user-selected output parent changed while the file was created",
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            delete_on_close_best_effort(&file);
            drop(file);
            return Err(error);
        }
        let cleanup: CreatedFileCleanup =
            Box::new(|file: &File| mark_for_deletion(file.as_raw_handle() as HANDLE));
        Ok((file, cleanup))
    }

    fn open_user_selected_parent(parent_path: &Path) -> io::Result<File> {
        let mut parent_options = std::fs::OpenOptions::new();
        parent_options
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | READ_CONTROL)
            // Denying delete sharing pins the selected parent object while the
            // child path is derived and created. Parent reparse points are
            // followed intentionally for an explicit user export.
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        let parent = parent_options.open(parent_path)?;
        let parent_info = file_information(&parent)?;
        if parent_info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!(
                    "user-selected output parent is not a directory: {}",
                    parent_path.display()
                ),
            ));
        }
        Ok(parent)
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

    pub(super) fn mark_for_deletion(handle: HANDLE) -> io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `handle` has DELETE access and the buffer matches
        // FileDispositionInfo.
        if unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                std::ptr::addr_of!(disposition).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn delete_on_close_best_effort(file: &File) {
        // Creation handles already have DELETE access. Failure merely leaves an
        // empty private file behind; cleanup never falls back to a raceable
        // path deletion.
        let _ = mark_for_deletion(file.as_raw_handle() as HANDLE);
    }

    pub(super) fn discard_created_private_file_checked(file: File) -> io::Result<()> {
        let result = mark_for_deletion(file.as_raw_handle() as HANDLE);
        drop(file);
        result
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

    pub(super) fn open_trusted_file_read(path: &Path) -> io::Result<File> {
        reject_parent_components(path)?;
        let path = std::path::absolute(path)?;
        let parent = PrivateParentGuard::new(&path)?;
        let stable_path = std::ffi::OsString::from_wide(&parent.stable_path(&path)?);
        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(GENERIC_READ | FILE_READ_ATTRIBUTES | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            // Opening a directory on Windows requires BACKUP_SEMANTICS. Keep
            // that open possible so `require_regular_non_reparse` can classify
            // a directory as `InvalidInput`; without it CreateFileW stops at
            // ERROR_ACCESS_DENIED and the cross-platform API reports the wrong
            // failure class before its object checks run.
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(&stable_path)?;
        require_regular_non_reparse(&file, &path)?;
        require_single_link(&file, &path)?;
        let current_user = current_user_sid()?;
        let administrators = builtin_administrators_sid()?;
        let system = well_known_sid(WinLocalSystemSid)?;
        require_trusted_object_security(
            &file,
            &path,
            current_user.sid,
            administrators.sid,
            system.sid,
            DANGEROUS_FILE_RIGHTS,
            "trusted file",
        )?;
        parent.verify()?;
        Ok(file)
    }

    pub(super) fn open_trusted_file_read_following_leaf(
        path: &Path,
    ) -> io::Result<(File, PathBuf)> {
        reject_parent_components(path)?;
        let requested = std::path::absolute(path)?;
        let parent = PrivateParentGuard::new(&requested)?;
        let stable_path = std::ffi::OsString::from_wide(&parent.stable_path(&requested)?);
        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
            // Deliberately omit delete sharing. While this handle lives, the
            // requested reparse leaf cannot be renamed between validation and
            // canonicalization.
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let requested_handle = options.open(&stable_path)?;
        let information = file_information(&requested_handle)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            drop(requested_handle);
            drop(parent);
            return Ok((open_trusted_file_read(&requested)?, requested));
        }
        if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "trusted file link resolves through a directory reparse point: {}",
                    requested.display()
                ),
            ));
        }
        require_single_link(&requested_handle, &requested)?;
        let current_user = current_user_sid()?;
        let administrators = builtin_administrators_sid()?;
        let system = well_known_sid(WinLocalSystemSid)?;
        require_trusted_object_security(
            &requested_handle,
            &requested,
            current_user.sid,
            administrators.sid,
            system.sid,
            DANGEROUS_FILE_RIGHTS,
            "trusted file link",
        )?;
        parent.verify()?;

        // `requested_handle` denies delete sharing and the guard holds every
        // ancestor without it, so this resolution is bound to the reparse
        // object and directory chain just inspected rather than to a mutable
        // drive-letter pathname.
        let resolved = std::fs::canonicalize(&stable_path)?;
        let file = open_trusted_file_read(&resolved)?;
        parent.verify()?;
        drop(requested_handle);
        Ok((file, resolved))
    }

    pub(super) fn remove_open_private_file(file: File, path: &Path) -> io::Result<()> {
        require_regular_non_reparse(&file, path)?;
        require_single_link(&file, path)?;
        if !same_file_identity(&file, path)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private path no longer identifies the open file",
            ));
        }
        // `open_existing_private_file` shares deletion but does not request
        // DELETE itself. ReOpenFile grants that access to the same kernel file
        // object, so no pathname is resolved between validation and deletion.
        let handle = unsafe {
            ReOpenFile(
                file.as_raw_handle() as HANDLE,
                DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ReOpenFile transferred ownership of this valid handle.
        let deletion = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        mark_for_deletion(deletion.as_raw_handle() as HANDLE)?;
        drop(deletion);
        drop(file);
        Ok(())
    }

    fn private_object_is_exact(file: &File) -> io::Result<bool> {
        let token = effective_token()?;
        let current_user = token_user_sid(&token)?;
        let current_owner = token_owner_sid(&token)?;
        // Kettle-created private objects explicitly select TokenUser as owner;
        // pre-existing objects hardened in place retain the kernel's
        // TokenOwner default. Both have the same protected TokenUser-only DACL.
        Ok(
            owned_by_any_handle_direct(file, &[current_user.sid, current_owner.sid])?
                && has_current_user_only_dacl_direct(file, current_user.sid)?,
        )
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

    /// Rights that redirect the PATH. Checked on every component, because
    /// deleting, renaming, or re-permissioning any ancestor moves where the
    /// final directory resolves to.
    const DANGEROUS_PATH_RIGHTS: u32 =
        FILE_DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

    /// Rights that let a principal PUT something in the directory. Checked only
    /// on the directory kettle actually reads from, and deliberately not on its
    /// ancestors: `C:\` grants Authenticated Users "create folders / append
    /// data" on stock Windows, so applying these to the whole chain rejects
    /// every path on a normal machine — and a directory created under `C:\`
    /// reaches nothing of kettle's.
    ///
    /// On the target directory it matters, because that is where kettle's
    /// sessions, layouts, and control-server registry live, and kettle
    /// enumerates and reads them back. `GENERIC_WRITE` is included because on a
    /// directory it maps to exactly `FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY`
    /// plus attribute writes — so an ACE spelled with the generic bit granted
    /// creation while passing a check that looked only for the specific ones.
    const DANGEROUS_CONTENT_RIGHTS: u32 = GENERIC_WRITE | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY;

    const DANGEROUS_FILE_RIGHTS: u32 = GENERIC_ALL
        | GENERIC_WRITE
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER;

    fn require_trusted_object_security(
        object: &File,
        path: &Path,
        current_user: PSID,
        administrators: PSID,
        system: PSID,
        dangerous_rights: u32,
        description: &str,
    ) -> io::Result<()> {
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: the held directory handle has READ_CONTROL and every output
        // pointer is valid. The returned owner/DACL are anchored in descriptor.
        let status = unsafe {
            GetSecurityInfo(
                object.as_raw_handle() as HANDLE,
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
                format!("{description} has an untrusted owner: {}", path.display()),
            ));
        }
        if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{description} has no valid access-control list: {}",
                    path.display()
                ),
            ));
        }
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
            if !is_allow || flags & INHERIT_ONLY_ACE as u8 != 0 || mask & dangerous_rights == 0 {
                continue;
            }
            if !matches!(
                kind,
                ACCESS_ALLOWED_ACE_TYPE | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{description} has an unverified object-specific mutation ACE: {}",
                        path.display()
                    ),
                ));
            }
            let sid: PSID = unsafe { std::ptr::addr_of!((*allowed).SidStart).cast_mut().cast() };
            if !trusted_windows_sid(sid, current_user, administrators, system) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{description} grants mutation rights to an untrusted principal: {}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn require_trusted_directory_security(
        directory: &File,
        path: &Path,
        current_user: PSID,
        administrators: PSID,
        system: PSID,
        is_target_directory: bool,
    ) -> io::Result<()> {
        let dangerous_rights = if is_target_directory {
            DANGEROUS_PATH_RIGHTS | DANGEROUS_CONTENT_RIGHTS
        } else {
            DANGEROUS_PATH_RIGHTS
        };
        require_trusted_object_security(
            directory,
            path,
            current_user,
            administrators,
            system,
            dangerous_rights,
            "private parent",
        )
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

        pub(crate) fn verify_directory(&self) -> io::Result<()> {
            self.verify()
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

        pub(super) fn reap_matching_children<F>(
            &self,
            max_scan: usize,
            max_remove: usize,
            mut predicate: F,
        ) -> io::Result<usize>
        where
            F: FnMut(&std::ffi::OsStr) -> bool,
        {
            // Every directory handle in this guard omits FILE_SHARE_DELETE,
            // so the verified parent chain cannot be renamed or replaced
            // while this pathname enumeration runs.
            self.verify()?;
            let mut scanned = 0usize;
            let mut removed = 0usize;
            for entry in std::fs::read_dir(&self.path)? {
                if scanned >= max_scan || removed >= max_remove {
                    break;
                }
                let Ok(entry) = entry else {
                    continue;
                };
                scanned += 1;
                let name = entry.file_name();
                if !predicate(&name) {
                    continue;
                }
                let path = self.path.join(&name);
                let Ok(file) = open_existing_private_file(&path, false) else {
                    continue;
                };
                if remove_open_private_file(file, &path).is_ok() {
                    removed += 1;
                }
            }
            Ok(removed)
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
        let mut remaining = ancestors.len();
        for ancestor in ancestors.into_iter().rev() {
            remaining -= 1;
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
                remaining == 0,
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
        let token = effective_token()?;
        let current_user = token_user_sid(&token)?;
        let current_owner = token_owner_sid(&token)?;
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
        if !owner_authorizes_hardening(owner, current_owner.sid) {
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
                "refusing to change the ACL of an object not owned by the effective token owner",
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

    fn owner_authorizes_hardening(owner: PSID, token_owner: PSID) -> bool {
        !owner.is_null() && unsafe { EqualSid(owner, token_owner) } != 0
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
        owned_by_any_raw(handle.as_raw_handle() as HANDLE, &[current_user])
    }

    fn owned_by_any_handle_direct(file: &File, expected_owners: &[PSID]) -> io::Result<bool> {
        owned_by_any_raw(file.as_raw_handle() as HANDLE, expected_owners)
    }

    fn owned_by_user_raw(handle: HANDLE, current_user: PSID) -> io::Result<bool> {
        owned_by_any_raw(handle, &[current_user])
    }

    fn owned_by_any_raw(handle: HANDLE, expected_owners: &[PSID]) -> io::Result<bool> {
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
        Ok(!owner.is_null()
            && expected_owners
                .iter()
                .any(|expected| unsafe { EqualSid(owner, *expected) } != 0))
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
    pub(super) fn owned_by_current_token_owner(file: &File) -> io::Result<bool> {
        let token = effective_token()?;
        let current_owner = token_owner_sid(&token)?;
        owned_by_user_handle(file, current_owner.sid)
    }

    #[cfg(test)]
    fn grant_world_access_for_test(file: &File, world_access: u32) -> io::Result<()> {
        let current_user = current_user_sid()?;
        let world = well_known_sid(WinWorldSid)?;
        let ace_bytes = |sid: PSID| {
            std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>()
                + unsafe { GetLengthSid(sid) } as usize
        };
        let acl_len = std::mem::size_of::<ACL>()
            .checked_add(ace_bytes(current_user.sid))
            .and_then(|len| len.checked_add(ace_bytes(world.sid)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ACL size overflow"))?;
        let mut storage = vec![0_u64; acl_len.div_ceil(std::mem::size_of::<u64>())];
        let acl = storage.as_mut_ptr().cast::<ACL>();
        let capacity = u32::try_from(storage.len() * std::mem::size_of::<u64>())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ACL is too large"))?;
        if unsafe { InitializeAcl(acl, capacity, ACL_REVISION) } == 0
            || unsafe {
                AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, current_user.sid)
            } == 0
            || unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, 0, world_access, world.sid) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let handle = reopen_for_acl(file)?;
        let status = unsafe {
            SetSecurityInfo(
                handle.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    #[cfg(test)]
    pub(super) fn grant_world_write_for_test(file: &File) -> io::Result<()> {
        grant_world_access_for_test(file, GENERIC_WRITE)
    }

    #[cfg(test)]
    pub(super) fn grant_world_all_for_test(file: &File) -> io::Result<()> {
        grant_world_access_for_test(file, GENERIC_ALL)
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

        #[test]
        fn hardening_uses_token_owner_instead_of_token_user() {
            let token = effective_token().unwrap();
            let current_user = token_user_sid(&token).unwrap();
            let current_owner = token_owner_sid(&token).unwrap();
            assert!(
                owner_authorizes_hardening(current_owner.sid, current_owner.sid),
                "the effective token owner must authorize its own object"
            );

            // Simulate the elevated-token case even on a non-elevated test
            // host: TokenUser remains the account SID while TokenOwner is the
            // builtin Administrators SID.
            let administrators = builtin_administrators_sid().unwrap();
            assert_eq!(
                unsafe { EqualSid(current_user.sid, administrators.sid) },
                0,
                "TokenUser must differ from the simulated elevated TokenOwner"
            );
            assert!(
                owner_authorizes_hardening(administrators.sid, administrators.sid),
                "a matching group-valued TokenOwner must authorize hardening"
            );
            assert!(
                !owner_authorizes_hardening(current_user.sid, administrators.sid),
                "TokenUser must not substitute for a different TokenOwner"
            );
            assert!(
                !owner_authorizes_hardening(administrators.sid, current_user.sid),
                "a hard-coded Administrators exception must not substitute for TokenOwner"
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

        /// A directory anyone can ADD to is not private.
        ///
        /// The trust check covered removal and re-permissioning —
        /// `FILE_DELETE_CHILD`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER`,
        /// `GENERIC_ALL` — and not creation. `FILE_ADD_FILE` and
        /// `FILE_ADD_SUBDIRECTORY` let an untrusted principal PUT a file where
        /// kettle keeps sessions, layouts, and the control-server registry,
        /// all of which it enumerates and reads back. `GENERIC_WRITE` on a
        /// directory maps to exactly those two, so an ACE spelled with the
        /// generic bit walked through a check that looked only for the
        /// specific ones.
        ///
        /// These rights are refused on the target directory ONLY. Applying
        /// them to the whole ancestor chain rejects every path on a stock
        /// Windows machine, because `C:\` grants Authenticated Users
        /// "create folders / append data" — and a directory created under
        /// `C:\` reaches nothing of kettle's. The first version of this fix
        /// did exactly that and failed 14 of this crate's own tests.
        #[test]
        fn creation_rights_are_dangerous_on_the_target_directory_and_normal_above_it() {
            // The path-redirecting rights apply everywhere.
            for right in [
                FILE_DELETE_CHILD,
                DELETE,
                WRITE_DAC,
                WRITE_OWNER,
                GENERIC_ALL,
            ] {
                assert_ne!(
                    right & DANGEROUS_PATH_RIGHTS,
                    0,
                    "a right that redirects the path must be checked on every component"
                );
            }
            // The creation rights are NOT among them — that is what keeps
            // `C:\` acceptable as an ancestor.
            for right in [FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, GENERIC_WRITE] {
                assert_eq!(
                    right & DANGEROUS_PATH_RIGHTS,
                    0,
                    "creation rights on an ancestor are ordinary Windows and must not \
                     be refused there"
                );
                assert_ne!(
                    right & DANGEROUS_CONTENT_RIGHTS,
                    0,
                    "creation rights on the directory kettle reads from must be refused"
                );
            }
            // And the real ancestor that motivates the split still passes,
            // while the state directory kettle actually uses also passes —
            // proving the split is not simply "check nothing".
            let dir = crate::test_tempdir();
            assert!(
                guard_private_parent(&dir.path().join("state.json")).is_ok(),
                "kettle's own state directory must remain acceptable"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn user_selected_file_requires_an_existing_parent_and_a_new_leaf() {
        let root = crate::test_tempdir();
        let missing_parent = root.path().join("missing");
        let missing_path = missing_parent.join("shot.png");
        let error = create_user_selected_file_new(&missing_path)
            .expect_err("an export must not create arbitrary parent directories");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!missing_parent.exists());

        let existing = root.path().join("existing.png");
        std::fs::write(&existing, b"keep").unwrap();
        let error = create_user_selected_file_new(&existing)
            .expect_err("an export must not overwrite an existing entry");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(existing).unwrap(), b"keep");
    }

    #[test]
    fn staged_user_selected_file_is_invisible_until_atomic_publication() {
        let root = crate::test_tempdir();
        let destination = root.path().join("shot.png");
        let mut staged = stage_user_selected_file_new(&destination).unwrap();
        staged.write_all(b"complete png").unwrap();
        staged.flush().unwrap();
        assert!(
            !destination.exists(),
            "the requested leaf must not expose partial streamed bytes"
        );

        staged.publish().unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"complete png");
        let staging_names = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".kettle-export-")
            })
            .count();
        assert_eq!(staging_names, 0, "publication must remove its sibling");
    }

    #[test]
    fn staged_user_selected_publication_never_replaces_a_racing_winner() {
        let root = crate::test_tempdir();
        let destination = root.path().join("shot.png");
        let mut staged = stage_user_selected_file_new(&destination).unwrap();
        staged.write_all(b"ours").unwrap();
        std::fs::write(&destination, b"winner").unwrap();

        let error = staged.publish().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(destination).unwrap(), b"winner");
    }

    #[test]
    fn unsupported_hard_links_fall_back_to_atomic_no_replace_rename() {
        let renamed = std::cell::Cell::new(false);
        let publication = publish_with_atomic_fallback(
            || Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links")),
            || {
                renamed.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(publication, CreatedFilePublication::Renamed);
        assert!(renamed.get());

        let rename_called = std::cell::Cell::new(false);
        let error = publish_with_atomic_fallback(
            || Err(io::Error::new(io::ErrorKind::AlreadyExists, "winner")),
            || {
                rename_called.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            !rename_called.get(),
            "a racing destination must never reach a rename fallback"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn platform_atomic_rename_fallback_publishes_and_never_replaces() {
        let root = crate::test_tempdir();
        let destination = root.path().join("fallback.png");
        let mut nonce = 0_u8;
        let mut staged = stage_user_selected_file_new_with_mode(
            &destination,
            || {
                nonce = nonce.wrapping_add(1);
                Ok([nonce; 16])
            },
            HardLinkMode::ForceUnsupported,
        )
        .unwrap();
        staged.write_all(b"complete").unwrap();
        staged.publish().unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"complete");

        let racing_destination = root.path().join("racing.png");
        let mut staged = stage_user_selected_file_new_with_mode(
            &racing_destination,
            || {
                nonce = nonce.wrapping_add(1);
                Ok([nonce; 16])
            },
            HardLinkMode::ForceUnsupported,
        )
        .unwrap();
        staged.write_all(b"ours").unwrap();
        std::fs::write(&racing_destination, b"winner").unwrap();
        let error = staged.publish().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!error.destination_may_exist());
        assert_eq!(std::fs::read(&racing_destination).unwrap(), b"winner");
    }

    #[test]
    fn random_staging_collisions_are_bounded_and_leave_the_destination_absent() {
        let root = crate::test_tempdir();
        let destination = root.path().join("shot.png");
        let nonce = [0xab; 16];
        let occupied = root
            .path()
            .join(format!(".kettle-export-{}.tmp", "ab".repeat(nonce.len())));
        std::fs::write(&occupied, b"occupied").unwrap();

        let attempts = std::cell::Cell::new(0_usize);
        let error = stage_user_selected_file_new_with(&destination, || {
            attempts.set(attempts.get() + 1);
            Ok(nonce)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(attempts.get(), 32, "collision retries must stay bounded");
        assert!(!destination.exists());
        assert_eq!(std::fs::read(occupied).unwrap(), b"occupied");
    }

    #[test]
    fn staged_publication_reports_primary_and_cleanup_failures() {
        let root = crate::test_tempdir();
        let path = root.path().join("stage.tmp");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let staged = CreatedUserSelectedFile::new(
            file,
            Box::new(|_| Err(io::Error::other("injected cleanup failure"))),
        );
        let staged = StagedUserSelectedFile::new(
            staged,
            Box::new(|_| Err(io::Error::other("injected publication failure").into())),
        );

        let error = staged.publish_synced().unwrap_err();
        assert!(!error.destination_may_exist());
        let message = error.to_string();
        assert!(message.contains("injected publication failure"));
        assert!(message.contains("injected cleanup failure"));
    }

    #[test]
    fn post_publication_failure_reports_that_the_destination_may_exist() {
        let root = crate::test_tempdir();
        let path = root.path().join("stage.tmp");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let staged = CreatedUserSelectedFile::new(file, Box::new(|_| Ok(())));
        let staged = StagedUserSelectedFile::new(
            staged,
            Box::new(|_| {
                Err(CreatedFilePublishError::published(io::Error::other(
                    "injected durability failure",
                )))
            }),
        );

        let error = staged.publish_synced().unwrap_err();
        assert!(error.destination_may_exist());
        assert!(error.to_string().contains("injected durability failure"));
    }

    #[cfg(unix)]
    #[test]
    fn user_selected_file_is_owner_only_beneath_a_public_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = crate::test_tempdir();
        let exports = root.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o775)).unwrap();
        let path = exports.join("shot.png");

        let private_error = create_private_file_new(&path)
            .expect_err("private state must still reject a group-writable parent");
        assert_eq!(private_error.kind(), io::ErrorKind::PermissionDenied);

        let file = create_user_selected_file_new(&path)
            .expect("an explicit export may target an existing public directory");
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn user_selected_file_fails_if_the_held_parent_is_displaced() {
        let root = crate::test_tempdir();
        let exports = root.path().join("exports");
        let displaced = root.path().join("displaced");
        std::fs::create_dir(&exports).unwrap();
        let path = exports.join("shot.png");

        let result = unix::create_user_selected_file_new_with(&path, || {
            std::fs::rename(&exports, &displaced).unwrap();
            std::fs::create_dir(&exports).unwrap();
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a replaced parent must not produce a successful path response"),
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            !exports.join("shot.png").exists(),
            "the child open must not re-resolve through the replacement parent"
        );
        assert!(
            !displaced.join("shot.png").exists(),
            "a failed postcondition must remove the exact file through the held parent"
        );
    }

    #[cfg(windows)]
    #[test]
    fn user_selected_file_gets_a_protected_current_user_dacl() {
        let root = crate::test_tempdir();
        let path = root.path().join("shot.png");
        let file = create_user_selected_file_new(&path).unwrap();
        assert!(has_current_user_only_dacl(&file).unwrap());
        assert!(owned_by_current_user(&file).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn user_selected_file_pins_its_windows_parent_during_creation() {
        let root = crate::test_tempdir();
        let exports = root.path().join("exports");
        let displaced = root.path().join("displaced");
        std::fs::create_dir(&exports).unwrap();
        let path = exports.join("shot.png");

        let (file, _cleanup) = windows::create_user_selected_file_new_with(&path, || {
            std::fs::rename(&exports, &displaced)
                .expect_err("the held parent must deny rename/delete sharing");
        })
        .expect("the export should continue through the pinned parent");
        drop(file);
        assert!(path.exists());
        assert!(!displaced.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn user_selected_file_drops_inherited_extended_acl() {
        let root = crate::test_tempdir();
        let exports = root.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,file_inherit"])
            .arg(&exports)
            .status()
            .expect("launch chmod to seed an inheritable ACL");
        assert!(status.success(), "seed an inheritable macOS ACL");

        let inherited = exports.join("inherited");
        std::fs::write(&inherited, b"fixture").unwrap();
        let inherited = File::open(&inherited).unwrap();
        assert!(
            unix::macos_acl::has_any_extended_acl(&inherited).unwrap(),
            "the fixture must prove this directory really propagates an ACL"
        );

        let selected = exports.join("selected.png");
        let file = create_user_selected_file_new(&selected).unwrap();
        assert!(
            !unix::macos_acl::has_any_extended_acl(&file).unwrap(),
            "a 0600 screenshot with an inherited read ACE is not owner-only"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn acl_free_private_hardening_is_a_metadata_noop() {
        use std::os::unix::fs::MetadataExt as _;

        let root = crate::test_tempdir();
        let path = root.path().join("state");
        let file = create_private_file_new(&path).unwrap();
        assert!(!unix::macos_acl::has_any_extended_acl(&file).unwrap());
        let before = file.metadata().unwrap();

        // Give a real metadata write a distinct timestamp. APFS records
        // nanoseconds, so this stays cheap while discriminating the old
        // unconditional `acl_set_fd_np` call.
        std::thread::sleep(std::time::Duration::from_millis(20));
        restrict_private_file(&file).unwrap();
        let after = file.metadata().unwrap();
        assert_eq!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec()),
            "hardening an ACL-free 0600 file must not emit metadata churn"
        );
    }

    #[cfg(windows)]
    #[test]
    fn user_selected_file_rejects_win32_alias_and_stream_syntax() {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

        let root = crate::test_tempdir();
        let target = root.path().join("existing.png");
        std::fs::write(&target, b"keep").unwrap();

        let mut stream = target.as_os_str().to_os_string();
        stream.push(":screenshot");
        assert_eq!(
            create_user_selected_file_new(Path::new(&stream))
                .expect_err("an alternate data stream is not a new export file")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            stage_user_selected_file_new(Path::new(&stream))
                .expect_err("a staged export must reject alternate data streams")
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut trailing_dot = target.as_os_str().to_os_string();
        trailing_dot.push(".");
        assert_eq!(
            create_user_selected_file_new(Path::new(&trailing_dot))
                .expect_err("a trailing-dot alias is not a new export file")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            stage_user_selected_file_new(Path::new(&trailing_dot))
                .expect_err("a staged export must reject trailing-dot aliases")
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut nul = target.as_os_str().encode_wide().collect::<Vec<_>>();
        nul.push(0);
        nul.extend("different.png".encode_utf16());
        let nul = std::ffi::OsString::from_wide(&nul);
        assert_eq!(
            create_user_selected_file_new(Path::new(&nul))
                .expect_err("an embedded NUL must not truncate the requested path")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            stage_user_selected_file_new(Path::new(&nul))
                .expect_err("a staged export must reject embedded NUL aliases")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(std::fs::read(target).unwrap(), b"keep");
    }

    /// The base list must match where the resolvers actually put things, and a
    /// missing base is invisible until someone sweeps a real machine. This
    /// enumerates the ones kettle names a `kettle/` directory inside, so a new
    /// resolver that omits its base fails here rather than years later.
    ///
    /// The suffixes below remain a hand-written contract because the resolvers
    /// live in other crates, but every branch is fed a controlled absolute value
    /// here. The cross-crate cache test calls the real resolver in re-executed
    /// children, one environment branch at a time.
    #[test]
    fn every_base_a_resolver_uses_is_recognized() {
        let root = if cfg!(windows) {
            std::path::PathBuf::from(r"C:\controlled")
        } else {
            std::path::PathBuf::from("/controlled")
        };
        let values = std::collections::BTreeMap::from([
            ("XDG_RUNTIME_DIR", root.join("runtime")),
            ("XDG_STATE_HOME", root.join("state")),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("APPDATA", root.join("appdata")),
            ("LOCALAPPDATA", root.join("localappdata")),
            ("HOME", root.join("home")),
        ]);
        let temp = root.join("tmp");
        let bases = super::kettle_base_dirs_from(
            |key| values.get(key).map(|path| path.as_os_str().to_os_string()),
            temp.clone(),
        );
        let expected: [std::path::PathBuf; 10] = [
            temp,
            root.join("runtime"),
            root.join("state"),
            root.join("config"),
            root.join("cache"),
            root.join("appdata"),
            root.join("localappdata"),
            root.join("home").join(".local/state"),
            root.join("home").join(".config"),
            root.join("home").join(".cache"),
        ];
        for base in expected {
            assert!(
                bases.contains(&base),
                "missing resolver base {}",
                base.display()
            );
            assert!(
                super::is_kettle_owned_dir_name_in(&base.join("kettle"), &bases),
                "{}/kettle must be recognized as kettle's own",
                base.display()
            );
        }
    }

    /// A source checkout is called `kettle` too, and matching on the name alone
    /// meant `kettle --config ~/Repos/kettle/dev.config` set the whole checkout
    /// to `0700`. Measured going `0775 -> 0700` before the parent check existed.
    #[cfg(unix)]
    #[test]
    fn a_source_checkout_named_kettle_is_not_kettles_own_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::test_tempdir();
        let checkout = root.path().join("Repos").join("kettle");
        std::fs::create_dir_all(checkout.join("target")).unwrap();
        std::fs::set_permissions(&checkout, std::fs::Permissions::from_mode(0o775)).unwrap();

        assert!(
            !is_kettle_owned_dir_name(&checkout),
            "a checkout whose parent is not an XDG base is not kettle's to chmod"
        );
        create_private_dirs(&checkout.join("target").join("diag")).unwrap();
        assert_eq!(
            std::fs::metadata(&checkout).unwrap().permissions().mode() & 0o777,
            0o775,
            "the repair narrowed a user's source checkout"
        );
        assert_eq!(
            unix::chmod_remedy(&checkout),
            "",
            "kettle must not advise a chmod on a checkout it did not create"
        );
    }

    /// The remedy is advice, and advice about somebody else's directory is
    /// wrong even when the refusal is right.
    #[cfg(unix)]
    #[test]
    fn the_chmod_remedy_is_offered_only_for_directories_kettle_named() {
        use std::path::Path;
        // Derived from the real base list, not hardcoded: `/run/user/<uid>` is
        // only a base when `XDG_RUNTIME_DIR` says so, which is untrue on macOS
        // and was how the first version of this test failed.
        for base in super::kettle_base_dirs() {
            for name in ["kettle", "kettle-1000"] {
                let owned = base.join(name);
                let remedy = unix::chmod_remedy(&owned);
                assert!(
                    remedy.contains("chmod 700") && remedy.contains(&*owned.to_string_lossy()),
                    "kettle's own directory should carry the remedy: {}",
                    owned.display()
                );
            }
        }
        // The live-UI run produced exactly this: a screenshot under a checkout,
        // refused because `~/Repos` is 0775, with the message telling the user
        // to lock down every project they own.
        for foreign in ["/home/user/Repos", "/home/user/.config", "/tmp"] {
            assert_eq!(
                unix::chmod_remedy(Path::new(foreign)),
                "",
                "kettle must not tell a user to chmod {foreign}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn writable_parent_diagnostic_identifies_the_parent_mode_and_umask_risk() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = crate::test_tempdir();
        let writable = dir.path().join("install");
        let child = writable.join("share");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o775)).unwrap();
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = guard_private_parent(&child.join("state.json"))
            .err()
            .expect("a group-writable parent must be rejected");
        let message = error.to_string();
        assert!(message.contains(&format!("parent {}", writable.display())));
        assert!(message.contains("mode 0775"));
        assert!(message.contains("process umask"));
    }

    #[test]
    fn trusted_directory_validation_is_read_only_and_accepts_a_private_directory() {
        let root = crate::test_tempdir();
        let directory = root.path().join("config");
        create_private_dirs(&directory).unwrap();

        let before = std::fs::metadata(&directory).unwrap().permissions();
        validate_trusted_directory(&directory).unwrap();
        let after = std::fs::metadata(&directory).unwrap().permissions();

        assert_eq!(before.readonly(), after.readonly());
        assert!(
            !directory.join(".kettle-directory-trust-check").exists(),
            "the read-only verifier created its synthetic leaf"
        );
    }

    /// A guard needs one stable capability to its immediate parent, not one
    /// descriptor per path component. The latter was correct but scaled steady
    /// FD use with path depth; parallel config tests crossed macOS's default
    /// 256-descriptor soft limit and failed unrelated opens with `EMFILE`.
    ///
    /// `RLIMIT_NOFILE` is process-wide, so the low-limit proof runs in a
    /// re-executed child rather than racing the shared test harness.
    #[cfg(unix)]
    #[test]
    fn parent_guards_hold_one_descriptor_each_under_a_low_process_limit() {
        const CHILD: &str = "KETTLE_PARENT_GUARD_FD_CHILD";
        const TEST: &str =
            "private::tests::parent_guards_hold_one_descriptor_each_under_a_low_process_limit";

        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD, "1")
                .status()
                .expect("re-exec the low-FD guard test");
            assert!(status.success(), "low-FD guard child failed: {status}");
            return;
        }

        let root = crate::test_tempdir();
        let directory = root.path().join("one/two/three/four/five/six");
        // The whole synthetic path is part of this trust fixture. Name the
        // mode for every component rather than relying on the ambient umask:
        // `create_private_dirs` deliberately repairs only directories in a
        // real kettle namespace, so a `002` umask otherwise leaves `one/` at
        // 0775 and this test fails the trust check before measuring FD use.
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .unwrap();

        // SAFETY: only the isolated child changes its own process limit, after
        // fixture creation and before spawning any application threads.
        unsafe {
            let mut limit = std::mem::zeroed::<libc::rlimit>();
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
            limit.rlim_cur = limit.rlim_cur.min(96);
            assert!(
                limit.rlim_cur >= 64,
                "test requires at least 64 descriptors"
            );
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
        }

        let guards: Vec<_> = (0..40)
            .map(|index| {
                guard_private_parent(&directory.join(format!("config-{index}")))
                    .expect("one guard should consume one steady descriptor")
            })
            .collect();
        assert_eq!(guards.len(), 40);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_directory_validation_rejects_a_group_writable_target() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = crate::test_tempdir();
        let directory = root.path().join("config");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o775)).unwrap();

        let error = validate_trusted_directory(&directory).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("mode 775"), "{error}");
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o775,
            "validation repaired the directory instead of remaining read-only"
        );
    }

    #[test]
    fn trusted_read_accepts_a_regular_file_without_changing_it() {
        let root = crate::test_tempdir();
        let path = root.path().join("config");
        let mut created = create_private_file_new(&path).unwrap();
        std::io::Write::write_all(&mut created, b"theme = TokyoNight Night\n").unwrap();
        drop(created);
        let before = std::fs::metadata(&path).unwrap().permissions();

        let mut file = open_trusted_file_read(&path).unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut file, &mut contents).unwrap();

        assert_eq!(contents, "theme = TokyoNight Night\n");
        assert_eq!(
            before.readonly(),
            std::fs::metadata(&path).unwrap().permissions().readonly()
        );
    }

    #[cfg(unix)]
    #[test]
    fn trusted_leaf_follow_accepts_a_user_owned_dotfile_link() {
        use std::os::unix::fs::symlink;

        let root = crate::test_tempdir();
        let target = root.path().join("tracked-config");
        let mut created = create_private_file_new(&target).unwrap();
        created.write_all(b"font-size = 18\n").unwrap();
        drop(created);
        let link = root.path().join("config");
        symlink(&target, &link).unwrap();

        let (mut file, resolved) = open_trusted_file_read_following_leaf(&link).unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut file, &mut contents).unwrap();
        assert_eq!(contents, "font-size = 18\n");
        assert_eq!(resolved, target.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_preexisting_link_requires_trusted_ownership_and_one_name() {
        let current = unsafe { libc::geteuid() };
        let foreign = if current == 1 { 2 } else { 1 };
        let path = Path::new("/trusted-parent/config");

        unix::require_trusted_symbolic_link(current, 1, path).unwrap();
        if current != 0 {
            unix::require_trusted_symbolic_link(0, 1, path).unwrap();
        }
        let owner_error = unix::require_trusted_symbolic_link(foreign, 1, path).unwrap_err();
        assert_eq!(owner_error.kind(), io::ErrorKind::PermissionDenied);
        let links_error = unix::require_trusted_symbolic_link(current, 2, path).unwrap_err();
        assert_eq!(links_error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_read_rejects_a_writable_or_multiply_linked_leaf_without_repairing_it() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = crate::test_tempdir();
        let writable = root.path().join("writable-config");
        std::fs::write(&writable, b"font-size = 99\n").unwrap();
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o664)).unwrap();
        let error = open_trusted_file_read(&writable).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::metadata(&writable).unwrap().permissions().mode() & 0o777,
            0o664,
            "a read-only trust check chmod'd the config"
        );

        let target = root.path().join("target-config");
        let alias = root.path().join("linked-config");
        std::fs::write(&target, b"font-size = 98\n").unwrap();
        std::fs::hard_link(&target, &alias).unwrap();
        let error = open_trusted_file_read(&alias).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&target).unwrap(), b"font-size = 98\n");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trusted_config_rejects_extended_acl_mutation_grants() {
        let root = crate::test_tempdir();
        let directory = root.path().join("config-dir");
        create_private_dirs(&directory).unwrap();
        let path = directory.join("config");
        let mut created = create_private_file_new(&path).unwrap();
        std::io::Write::write_all(&mut created, b"font-size = 99\n").unwrap();
        drop(created);

        validate_trusted_directory(&directory).unwrap();
        open_trusted_file_read(&path).unwrap();

        let status = std::process::Command::new("chmod")
            .args(["+a", "everyone allow add_file,delete_child"])
            .arg(&directory)
            .status()
            .expect("run macOS chmod for the directory ACL fixture");
        assert!(status.success(), "install the directory ACL fixture");
        let error = validate_trusted_directory(&directory).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("ACL principal"), "{error}");

        let status = std::process::Command::new("chmod")
            .arg("-N")
            .arg(&directory)
            .status()
            .expect("remove the directory ACL fixture");
        assert!(status.success(), "remove the directory ACL fixture");

        let status = std::process::Command::new("chmod")
            .args(["+a", "everyone allow write,delete,writesecurity"])
            .arg(&path)
            .status()
            .expect("run macOS chmod for the file ACL fixture");
        assert!(status.success(), "install the file ACL fixture");
        let error = open_trusted_file_read(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("ACL principal"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn trusted_read_rejects_a_file_acl_that_grants_world_write() {
        let root = crate::test_tempdir();
        let path = root.path().join("config");
        let mut file = create_private_file_new(&path).unwrap();
        file.write_all(b"font-size = 99\n").unwrap();
        grant_world_write_for_test(&file).unwrap();
        drop(file);

        let error = open_trusted_file_read(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&path).unwrap(), b"font-size = 99\n");
    }

    #[cfg(windows)]
    #[test]
    fn trusted_read_rejects_a_file_acl_that_grants_world_generic_all() {
        let root = crate::test_tempdir();
        let path = root.path().join("config");
        let mut file = create_private_file_new(&path).unwrap();
        file.write_all(b"font-size = 99\n").unwrap();
        grant_world_all_for_test(&file).unwrap();
        drop(file);

        let error = open_trusted_file_read(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&path).unwrap(), b"font-size = 99\n");
    }

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

    #[cfg(unix)]
    #[test]
    fn private_mode_hardening_skips_only_the_exact_owner_mode() {
        assert!(!unix::private_mode_needs_hardening(0o600));
        assert!(!unix::private_mode_needs_hardening(0o100600));
        for mode in [0o000, 0o400, 0o640, 0o660, 0o700, 0o4600] {
            assert!(
                unix::private_mode_needs_hardening(mode),
                "mode {mode:04o} must be hardened"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_owner_file_with_broad_mode_is_still_hardened() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = crate::test_tempdir();
        let path = dir.path().join("legacy");
        std::fs::write(&path, b"legacy").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let file = open_existing_private_file(&path).unwrap();
        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o7777,
            0o600
        );
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
    fn existing_token_owned_file_is_hardened() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("legacy");
        std::fs::write(&path, b"legacy").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(owned_by_current_token_owner(&file).unwrap());
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
        let (mut trusted, resolved) = open_trusted_file_read_following_leaf(&link)
            .expect("a current-user file link to a trusted target must be readable");
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut trusted, &mut contents).unwrap();
        assert_eq!(contents, b"unchanged");
        assert_eq!(resolved, target.canonicalize().unwrap());

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
