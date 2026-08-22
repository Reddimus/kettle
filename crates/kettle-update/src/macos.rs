//! Self-update for the macOS application bundle.
//!
//! Windows and Linux replace individual files inside an install prefix. macOS
//! cannot: the code signature seals `kettle.app` as a unit, so a bundle caught
//! half-way through a file-by-file update is one Gatekeeper rejects outright.
//! On Linux a half-applied update still runs the old binary. Here it can mean
//! an app that will not launch at all.
//!
//! So the shape is stage, verify, swap. The replacement is extracted beside the
//! live bundle, checked with Apple's own tooling, and only then exchanged with
//! it in a single atomic operation. Nothing ever writes into the bundle a user
//! is running.
//!
//! Two things make that practical, both verified against a real release
//! archive rather than assumed:
//!
//! - The stapled notarization ticket is `Contents/CodeResources`, an ordinary
//!   1.6 KiB file. It is not an extended attribute, so the plain `zip` reader
//!   this crate already links carries it through intact. `ditto` is needed to
//!   *build* the release archive, not to consume one.
//! - A bundle keeps its seal when renamed, so the staged copy can live under a
//!   scratch name and still verify before it is swapped in.

use std::fs::File;
use std::io::{Cursor, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

use crate::feed::{
    AvailableUpdate, FeedClient, UpdateError, require_strict_upgrade, reverify_available_update,
};
use crate::install::{
    ArchivePaths, InstallDisposition, InstallOutcome, MAX_ARCHIVE_ENTRIES, MAX_UNPACKED_BYTES,
    ManagedInstall, unique_suffix, validate_archive_path, verify_sha256_bytes,
    zip_unix_mode_is_safe,
};
use crate::{UPDATE_PUBLIC_KEY, current_target};

/// Signing identity every official build carries.
///
/// Both are read back out of the signature rather than out of `Info.plist`,
/// because a plist is just a file in the bundle and the signature is not. The
/// release workflow asserts the same team on the artifact it produces; if that
/// account ever changes, this constant and `APPLE_TEAM_ID` move together.
const BUNDLE_IDENTIFIER: &str = "org.kettle.terminal";
const TEAM_IDENTIFIER: &str = "D49LMN8545";

/// Both ship with macOS. `stapler` deliberately does not appear here: it
/// arrives with the Xcode command line tools, which an ordinary user has no
/// reason to have installed, so depending on it would make self-update work
/// only on developer machines.
const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";

/// `ditto -c -k --keepParent` puts the bundle at the archive root.
const ARCHIVE_ROOT: &str = "kettle.app";

/// Where per-install update locks live, under the user's private state
/// directory.
///
/// Deliberately *not* beside the bundle. `/Applications` is group-writable, and
/// the lock helper refuses, correctly, to create a private file in a directory
/// an untrusted principal can write. A lock file there would also be litter in
/// a directory that is not ours.
fn update_lock_path(bundle: &Path) -> Result<PathBuf, UpdateError> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/state"))
                .filter(|path| path.is_absolute())
        })
        .ok_or_else(|| {
            UpdateError::UnmanagedInstall(
                "neither XDG_STATE_HOME nor HOME is set, so there is nowhere private to \
                 hold the update lock"
                    .to_string(),
            )
        })?;
    let directory = base.join("kettle");
    kettle_state::create_private_dirs(&directory)?;
    // One lock per install: two bundles on the same machine should not
    // serialize against each other, and the name must not leak the path.
    //
    // Callers pass a canonical path. Two spellings of the same bundle would
    // otherwise hash differently and take different locks, which is the one way
    // this scheme can silently stop providing mutual exclusion.
    let digest = hex::encode(&Sha256::digest(bundle.as_os_str().as_encoded_bytes())[..8]);
    Ok(directory.join(format!("update-{digest}.lock")))
}

const STAGED_PREFIX: &str = ".kettle-update-staged-";
const PREVIOUS_PREFIX: &str = ".kettle-update-previous-";

/// Absent any of these, the archive is not a kettle bundle and extraction fails
/// before anything is swapped. `CodeResources` is the stapled ticket; without it
/// the replacement would install and then fail Gatekeeper on first launch.
const MANDATORY_ENTRIES: [&str; 4] = [
    "Contents/MacOS/kettle",
    "Contents/Info.plist",
    "Contents/CodeResources",
    "Contents/_CodeSignature/CodeResources",
];

/// What a bundle's signature claims about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignedIdentity {
    pub(crate) identifier: String,
    pub(crate) team: String,
}

impl SignedIdentity {
    /// How to describe this identity in a refusal.
    ///
    /// `codesign` prints the literal `not set` as the team of an ad-hoc
    /// signature, which is what a locally built app has. Interpolating that
    /// verbatim produces "by team not set", so name the actual situation.
    fn describe(&self) -> String {
        if self.team == "not set" || self.team == "-" {
            return format!("ad-hoc signed as {}, with no Developer ID", self.identifier);
        }
        format!("signed as {} by team {}", self.identifier, self.team)
    }
}

/// Checks Apple applies to a bundle.
///
/// This is a trait so tests can substitute it. Neither CI nor a unit test can
/// notarize a synthesized fixture, so the real implementation would reject
/// every test bundle and the staging, swap, and rollback paths would be
/// untestable. The real tooling is exercised by the live macOS release check
/// against a genuinely published archive.
pub(crate) trait SealVerifier {
    /// Read the identity a bundle's signature asserts, and confirm the
    /// signature itself is intact.
    fn identity(&self, bundle: &Path) -> Result<SignedIdentity, UpdateError>;

    /// Confirm Apple notarized *this* bundle. Separate from [`Self::identity`]
    /// because the two are needed at different strictnesses.
    fn notarized(&self, bundle: &Path) -> Result<(), UpdateError>;
}

/// The real checks, via the two tools macOS ships.
pub(crate) struct AppleSeal;

impl SealVerifier for AppleSeal {
    fn identity(&self, bundle: &Path) -> Result<SignedIdentity, UpdateError> {
        let verified = run(CODESIGN, &["--verify", "--deep", "--strict", "--"], bundle)?;
        if !verified.status {
            return Err(UpdateError::UnmanagedInstall(format!(
                "the signature on {} is not intact: {}",
                bundle.display(),
                first_line(&verified.stderr)
            )));
        }
        // `codesign -d` reports on stderr, not stdout.
        let described = run(CODESIGN, &["-dvvv", "--"], bundle)?;
        if !described.status {
            return Err(UpdateError::UnmanagedInstall(format!(
                "{} is not signed: {}",
                bundle.display(),
                first_line(&described.stderr)
            )));
        }
        parse_identity(&described.stderr).ok_or_else(|| {
            UpdateError::UnmanagedInstall(format!(
                "{} carries no bundle identifier and team",
                bundle.display()
            ))
        })
    }

    fn notarized(&self, bundle: &Path) -> Result<(), UpdateError> {
        let assessed = run(SPCTL, &["--assess", "--type", "execute", "--"], bundle)?;
        if assessed.status {
            return Ok(());
        }
        // The distinction this catches is real and quiet: re-signing a bundle
        // changes its cdhash, which orphans the stapled ticket while leaving
        // `codesign --verify` perfectly happy. Only this check notices.
        Err(UpdateError::UnsafeArchive(format!(
            "{} is not notarized: {}",
            bundle.display(),
            first_line(&assessed.stderr)
        )))
    }
}

struct ToolOutput {
    status: bool,
    stderr: String,
}

fn run(tool: &str, args: &[&str], bundle: &Path) -> Result<ToolOutput, UpdateError> {
    let output = Command::new(tool)
        .args(args)
        .arg(bundle)
        .output()
        .map_err(|error| {
            UpdateError::Io(std::io::Error::other(format!(
                "could not run {tool}: {error}"
            )))
        })?;
    Ok(ToolOutput {
        status: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no detail reported")
        .to_string()
}

/// Pull `Identifier=` and `TeamIdentifier=` out of `codesign -dvvv` output.
pub(crate) fn parse_identity(described: &str) -> Option<SignedIdentity> {
    let field = |key: &str| {
        described
            .lines()
            .find_map(|line| line.trim().strip_prefix(key).map(str::to_string))
    };
    let identifier = field("Identifier=")?;
    let team = field("TeamIdentifier=")?;
    if identifier.is_empty() || team.is_empty() {
        return None;
    }
    Some(SignedIdentity { identifier, team })
}

/// Reasons a bundle path is owned by something other than kettle's updater.
///
/// Each is a real layout a user can end up in, and each deserves a sentence
/// that says what to do instead of an `EACCES` from three frames down.
fn foreign_owner(bundle: &Path) -> Option<&'static str> {
    let path = bundle.to_string_lossy();
    if path.contains("/AppTranslocation/") {
        return Some(
            "this copy is running from a read-only translocated mount, which macOS does to \
             quarantined apps launched from Downloads; move kettle.app to /Applications and \
             open it once from there",
        );
    }
    if path.contains("/Cellar/") || path.contains("/Caskroom/") {
        return Some("this copy is managed by Homebrew; update it with `brew upgrade`");
    }
    None
}

/// Result of reading the bundle layout off an executable path.
///
/// Split from the ownership checks because it is pure path arithmetic with no
/// IO and no subprocesses, which lets startup use it without paying for a
/// signature verification.
enum BundleShape {
    Bundle(PathBuf),
    NotABundle,
    WrongExtension,
}

/// Walk `<bundle>.app/Contents/MacOS/kettle` back to the bundle.
fn enclosing_bundle(executable: &Path) -> BundleShape {
    let named = |path: Option<&Path>, expected: &str| {
        path.and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == expected)
    };
    let macos_dir = executable.parent();
    let contents_dir = macos_dir.and_then(Path::parent);
    let Some(bundle) = contents_dir.and_then(Path::parent) else {
        return BundleShape::NotABundle;
    };
    if !named(Some(executable), "kettle")
        || !named(macos_dir, "MacOS")
        || !named(contents_dir, "Contents")
    {
        return BundleShape::NotABundle;
    }
    if bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return BundleShape::WrongExtension;
    }
    BundleShape::Bundle(bundle.to_path_buf())
}

/// Delete one leftover, having already established that it is a directory we
/// own and that nothing holds it.
///
/// Renames it to a name that did not exist a moment ago before removing it,
/// then confirms it is still the same inode. `remove_dir_all` resolves its
/// first component by pathname, and this runs in a directory other people can
/// write, so deleting in place would leave a window for a substitution.
fn remove_leftover(path: &Path, expected: &std::fs::Metadata) {
    let Some(parent) = path.parent() else {
        return;
    };
    let doomed = parent.join(format!("{STAGED_PREFIX}{}-sweep", unique_suffix()));
    if std::fs::rename(path, &doomed).is_err() {
        return;
    }
    match std::fs::symlink_metadata(&doomed) {
        Ok(metadata)
            if metadata.is_dir()
                && metadata.ino() == expected.ino()
                && metadata.dev() == expected.dev() =>
        {
            let _ = std::fs::remove_dir_all(&doomed);
        }
        _ => {}
    }
}

/// Drop update leftovers sitting beside a bundle, if this executable is in one.
///
/// Called at startup. A finished update leaves the bundle it displaced next to
/// the live one, because the process that performed the swap was still reading
/// its own resources out of it. A new process starting is the first moment
/// nothing is running from those bytes, so this is where they go.
///
/// Deliberately cheap: path arithmetic, one lock attempt, and one directory
/// read. No signature check. Establishing provenance would mean spawning
/// `codesign` twice on the startup path, and deleting our own scratch
/// directories does not need it.
///
/// Takes the update lock first, because otherwise two processes starting while
/// a third is mid-update would delete the staging directory out from under it.
/// A held lock means someone is working; leave their files alone and try again
/// next launch.
///
/// This does not prove that no *older* process is still running from the
/// displaced bundle. Nothing cheap can, and the same is true of the Linux path,
/// which replaces `bin/kettle` while older processes keep their mapped inode. A
/// pre-update window that later reads a resource it never opened may fail; the
/// remedy is the same as it has always been, which is to restart after
/// updating.
pub(crate) fn sweep_leftovers_beside(executable: &Path) {
    // Canonicalize first. The lock is keyed by the bundle path, and
    // `locate_bundle_install` keys it from a canonical one, so skipping this
    // would let the sweep and a running update take two different locks and
    // stop excluding each other.
    let Ok(executable) = executable.canonicalize() else {
        return;
    };
    let BundleShape::Bundle(bundle) = enclosing_bundle(&executable) else {
        return;
    };
    let Some(parent) = bundle.parent() else {
        return;
    };
    let Ok(lock_path) = update_lock_path(&bundle) else {
        return;
    };
    sweep_locked(parent, &lock_path);
}

/// The locking half of the startup sweep, with the lock path supplied so tests
/// do not have to write into the real user state directory.
fn sweep_locked(parent: &Path, lock_path: &Path) {
    let lock = kettle_state::ExclusiveFileLock::try_acquire(lock_path);
    if !matches!(lock, Ok(Some(_))) {
        return;
    }
    sweep_interrupted_updates(parent);
}

/// Can this user create entries in `directory`?
///
/// `Permissions::readonly` answers a different question — whether *nobody* can
/// write — so it calls a root-owned `/Applications` writable for every user on
/// the machine. `access(2)` asks about the caller. The swap is still the
/// authority; this exists so the refusal arrives as a sentence rather than as
/// an `EACCES` from inside a rename.
fn writable(directory: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Resolve the running executable to the bundle that owns it, and prove the
/// bundle is one of ours.
///
/// The other platforms read an installer-written marker file. macOS has no
/// installer — the documented install is dragging the app to Applications — and
/// a marker cannot be added inside the bundle without breaking its seal. So
/// ownership is proven by the signature instead, which is both already present
/// and considerably harder to forge than a JSON file.
///
/// Notarization is deliberately *not* required here, only a valid signature
/// with the right identity. Requiring it would strand exactly the installs that
/// most need an update, and what matters is that the bundle being installed is
/// notarized, which [`stage_verified_bundle`] enforces.
pub(crate) fn locate_bundle_install(
    executable: &Path,
    verifier: &dyn SealVerifier,
) -> Result<ManagedInstall, UpdateError> {
    let executable = executable.canonicalize().map_err(|error| {
        UpdateError::UnmanagedInstall(format!("cannot resolve {}: {error}", executable.display()))
    })?;

    let bundle = match enclosing_bundle(&executable) {
        BundleShape::Bundle(bundle) => bundle,
        BundleShape::NotABundle => {
            return Err(UpdateError::UnmanagedInstall(
                "expected an application bundle laid out as kettle.app/Contents/MacOS/kettle"
                    .to_string(),
            ));
        }
        BundleShape::WrongExtension => {
            return Err(UpdateError::UnmanagedInstall(
                "the enclosing directory is not an .app bundle".to_string(),
            ));
        }
    };
    if let Some(reason) = foreign_owner(&bundle) {
        return Err(UpdateError::UnmanagedInstall(reason.to_string()));
    }

    let identity = verifier.identity(&bundle)?;
    if identity.identifier != BUNDLE_IDENTIFIER || identity.team != TEAM_IDENTIFIER {
        // A locally built app is ad-hoc signed and lands here, which is the
        // same answer Windows and Linux give a `local-dev` marker.
        return Err(UpdateError::UnmanagedInstall(format!(
            "{} is {}, not an official kettle build; rebuild and reinstall it \
             from its source checkout",
            bundle.display(),
            identity.describe()
        )));
    }

    // The swap exchanges two entries in this directory, so that is what has to
    // be writable — not the bundle.
    let parent = bundle.parent().ok_or_else(|| {
        UpdateError::UnmanagedInstall("the bundle has no containing directory".to_string())
    })?;
    if !writable(parent) {
        return Err(UpdateError::UnmanagedInstall(format!(
            "{} is not writable by this user, so the update cannot be swapped in; \
             an app in /Applications normally needs an administrator account",
            parent.display()
        )));
    }

    // macOS has no marker file. The signed `Info.plist` is the closest
    // equivalent: it is the file whose identity the signature attests.
    let marker_path = bundle.join("Contents/Info.plist");
    Ok(ManagedInstall {
        prefix: bundle,
        executable,
        marker_path,
    })
}

/// A private directory to build the replacement bundle in.
///
/// Created `0700` and held open. Both properties matter, and for different
/// attacks. The mode stops anyone else writing into the tree between
/// verification and installation. The descriptor stops anyone redirecting the
/// work by renaming the directory itself, which is possible whenever the
/// enclosing directory is shared: `/Applications` is `drwxrwxr-x root:admin`,
/// so every administrator on the machine can rename entries in it.
struct Staging {
    path: PathBuf,
    directory: File,
}

impl Staging {
    /// Create the staging directory beside `parent` and hold it open.
    fn create(parent: &Path, id: &str) -> Result<Self, UpdateError> {
        let path = parent.join(format!("{STAGED_PREFIX}{id}"));
        // 0700 up front rather than a later chmod: there must be no window in
        // which the directory exists and is writable by anyone else.
        let name = c_name(path.file_name().unwrap_or_default())?;
        let parent_fd = open_directory(parent)?;
        if unsafe { libc::mkdirat(parent_fd.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(UpdateError::Io(std::io::Error::last_os_error()));
        }
        let directory = open_directory_at(&parent_fd, &name)?;

        // `mkdirat` and `openat` are two calls, and in a shared parent someone
        // can rename ours aside between them and leave their own directory
        // under the name. Nobody can hand away ownership of a directory they
        // created, so our own uid plus the exact mode we asked for is proof
        // this is the one we made.
        if !staging_is_ours(&directory)? {
            return Err(UpdateError::UnsafeArchive(format!(
                "{} was replaced while it was being created",
                path.display()
            )));
        }
        // Mode bits and ACLs are independent on Darwin, so an inheritable write
        // ACE on the enclosing directory would leave this 0700 tree reachable
        // by whoever it names.
        kettle_state::clear_inherited_acl(&directory)?;

        // Clearing the root only governs children created afterwards: Darwin
        // applies an ACL when a file is made and never revisits it. Between
        // `mkdirat` above and the clear, an inheritable ACE could have let
        // somebody else create a child that keeps its own grant. So require the
        // directory to be empty at this point. If it is, every later child
        // inherits from an ACL-free parent and there is nothing to race.
        if std::fs::read_dir(&path)?.next().is_some() {
            return Err(UpdateError::UnsafeArchive(format!(
                "{} was not empty immediately after being created",
                path.display()
            )));
        }

        // An advisory lock on the directory itself, held for as long as this
        // staging area exists. The sweep tries the same lock before deleting
        // anything, which makes "is someone using this?" a property of the
        // directory rather than of a lock file whose location depends on the
        // environment two processes happened to inherit.
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(UpdateError::Io(std::io::Error::last_os_error()));
        }

        Ok(Self { path, directory })
    }

    /// Where the bundle being staged lives, for tools that need a pathname.
    fn bundle(&self) -> PathBuf {
        self.path.join(ARCHIVE_ROOT)
    }

    /// Remove the staging tree, but only if that path is still ours.
    ///
    /// A recursive delete keyed on a pathname is a weapon. If someone renamed
    /// our directory aside and left something else under the name, deleting it
    /// would destroy their file on their behalf, using rights they do not have.
    /// Compare against the descriptor taken at creation and walk away if it no
    /// longer matches.
    fn discard(self) {
        let Ok(held) = self.directory.metadata() else {
            return;
        };
        let same = |metadata: &std::fs::Metadata| {
            metadata.is_dir() && metadata.ino() == held.ino() && metadata.dev() == held.dev()
        };
        // Look before touching. If someone has already moved our directory
        // aside and left their own under the name, this is where we notice, and
        // we walk away without so much as renaming it.
        match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) if same(&metadata) => {}
            _ => return,
        }
        // Then move it aside under a name that did not exist a moment ago.
        // `remove_dir_all` resolves its first component by pathname, so
        // deleting `self.path` in place would leave a window for a
        // substitution. Re-check after the rename, because the check above and
        // the rename are still two operations.
        let Some(parent) = self.path.parent() else {
            return;
        };
        let doomed = parent.join(format!("{STAGED_PREFIX}{}-discard", unique_suffix()));
        if std::fs::rename(&self.path, &doomed).is_err() {
            return;
        }
        match std::fs::symlink_metadata(&doomed) {
            Ok(metadata) if same(&metadata) => {
                let _ = std::fs::remove_dir_all(&doomed);
            }
            // Something else is now at that name. Leave it where it is: moving
            // it was already more than we should have done, and deleting it
            // would be worse.
            _ => {}
        }
    }
}

/// Is this open directory the private one we just created?
///
/// Ownership plus the exact mode is enough. Nobody can give away ownership of a
/// directory they created, so a tree that is ours and `0700` cannot have been
/// substituted by another user between `mkdirat` and `openat`.
fn staging_is_ours(directory: &File) -> Result<bool, UpdateError> {
    let stat = directory.metadata()?;
    Ok(stat.is_dir() && stat.uid() == unsafe { libc::geteuid() } && stat.mode() & 0o777 == 0o700)
}

fn c_name(name: &std::ffi::OsStr) -> Result<std::ffi::CString, UpdateError> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|error| UpdateError::UnsafeArchive(error.to_string()))
}

fn open_directory(path: &Path) -> Result<File, UpdateError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}

/// Open a subdirectory by name, relative to an already-open directory.
///
/// `O_NOFOLLOW` means a symlink planted under this name is an error rather
/// than a redirect, and resolving relative to `directory` means the lookup
/// cannot be moved by renaming any ancestor.
fn open_directory_at(directory: &File, name: &std::ffi::CStr) -> Result<File, UpdateError> {
    use std::os::fd::FromRawFd as _;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(UpdateError::Io(std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Create a subdirectory if it is not already there, and open it.
fn ensure_directory_at(directory: &File, name: &std::ffi::CStr) -> Result<File, UpdateError> {
    if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) } == 0 {
        // Flush the new entry into its parent now. Syncing only the leaf
        // directories at the end would let a power loss after the exchange
        // leave a live bundle missing `Contents` while its children survive,
        // which is an app that cannot launch.
        directory.sync_all()?;
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EEXIST) {
            return Err(UpdateError::Io(error));
        }
    }
    open_directory_at(directory, name)
}

/// Create a new file relative to `directory`, failing if anything is there.
///
/// `O_EXCL` plus `O_NOFOLLOW` means this can only ever create a fresh regular
/// file at that name. It cannot be aimed at an existing file, and it cannot be
/// aimed through a symlink at somebody else's.
fn create_file_at(
    directory: &File,
    name: &std::ffi::CStr,
    mode: libc::mode_t,
) -> Result<File, UpdateError> {
    use std::os::fd::FromRawFd as _;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        )
    };
    if fd < 0 {
        return Err(UpdateError::Io(std::io::Error::last_os_error()));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    // `openat` masks the mode with the umask, which would leave the bundle's
    // files at whatever the invoking shell happened to set. Say it exactly.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(UpdateError::Io(std::io::Error::last_os_error()));
    }
    Ok(file)
}

/// Create every component of `relative` as a directory under `root`, and open
/// the last one.
fn ensure_path(root: &File, relative: &Path) -> Result<File, UpdateError> {
    let mut current = root.try_clone()?;
    for component in relative.components() {
        let name = c_name(component.as_os_str())?;
        current = ensure_directory_at(&current, &name)?;
    }
    Ok(current)
}

/// Extract a verified archive into a staging directory.
///
/// Every write is made relative to a descriptor rather than by pathname, with
/// `O_NOFOLLOW` and `O_EXCL` throughout, so neither a symlink in the archive
/// nor one planted on disk mid-extraction can move a write outside this tree.
/// The archive bytes are already authenticated by the signed manifest's
/// SHA-256 before this runs; these checks are about the filesystem, not the
/// contents.
fn extract_bundle_into(staging: &Staging, archive: &[u8]) -> Result<(), UpdateError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::UnsafeArchive(format!(
            "archive declares {} entries, more than the {MAX_ARCHIVE_ENTRIES} allowed",
            zip.len()
        )));
    }

    let mut seen = ArchivePaths::default();
    let mut unpacked: u64 = 0;
    let mut present: Vec<String> = Vec::new();

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        if entry.encrypted() {
            return Err(UpdateError::UnsafeArchive(
                "encrypted archive entry".to_string(),
            ));
        }
        let path = entry
            .enclosed_name()
            .ok_or_else(|| UpdateError::UnsafeArchive(entry.name().to_string()))?;
        validate_archive_path(&path)?;

        let is_dir = entry.is_dir();
        if !zip_unix_mode_is_safe(entry.unix_mode(), is_dir) {
            return Err(UpdateError::UnsafeArchive(format!(
                "unsafe mode on {}",
                path.display()
            )));
        }
        seen.insert(&path, is_dir)?;

        // Everything must live under the single `kettle.app` root `ditto`
        // produces. Anything else is a differently shaped archive and not
        // something to guess at.
        let mut components = path.components();
        if components.next().and_then(|c| c.as_os_str().to_str()) != Some(ARCHIVE_ROOT) {
            return Err(UpdateError::UnsafeArchive(format!(
                "{} is outside the {ARCHIVE_ROOT} root",
                path.display()
            )));
        }
        let inside: PathBuf = components.collect();
        if inside.as_os_str().is_empty() {
            continue;
        }

        if is_dir {
            ensure_path(&staging.directory, &path)?;
            continue;
        }
        unpacked = unpacked.saturating_add(entry.size());
        if unpacked > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "archive expands beyond the accepted size".to_string(),
            ));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != entry.size() {
            return Err(UpdateError::UnsafeArchive(format!(
                "{} does not match its declared size",
                inside.display()
            )));
        }

        // Honour only the executable bit. Everything else is normalized, so an
        // archive cannot hand a file group- or world-writable permissions.
        let executable = entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0);
        let mode: libc::mode_t = if executable { 0o755 } else { 0o644 };

        let directory = ensure_path(&staging.directory, path.parent().unwrap_or(Path::new("")))?;
        let name = c_name(path.file_name().unwrap_or_default())?;
        let mut file = create_file_at(&directory, &name, mode)?;
        file.write_all(&bytes)?;
        // Flush the contents and the mode before the exchange makes any of it
        // reachable, so a power loss cannot leave a live bundle whose binary
        // is present but not executable.
        file.sync_all()?;
        directory.sync_all()?;

        // `validate_archive_path` admits only `[A-Za-z0-9._-]` per component, so
        // a separator here is always `/` and needs no normalizing.
        present.push(inside.to_string_lossy().into_owned());
    }

    for mandatory in MANDATORY_ENTRIES {
        if !present.iter().any(|found| found == mandatory) {
            return Err(UpdateError::UnsafeArchive(format!(
                "the archive has no {mandatory}"
            )));
        }
    }
    staging.directory.sync_all()?;
    Ok(())
}

/// Extract, then refuse to go further unless Apple vouches for the result.
fn stage_verified_bundle(
    staging: &Staging,
    archive: &[u8],
    verifier: &dyn SealVerifier,
) -> Result<(), UpdateError> {
    extract_bundle_into(staging, archive)?;
    let bundle = staging.bundle();
    let identity = verifier.identity(&bundle)?;
    if identity.identifier != BUNDLE_IDENTIFIER || identity.team != TEAM_IDENTIFIER {
        return Err(UpdateError::UnsafeArchive(format!(
            "the downloaded bundle is signed as {} by team {}, not as an official kettle build",
            identity.identifier, identity.team
        )));
    }
    verifier.notarized(&bundle)
}

/// Remove leftovers from an interrupted update.
///
/// A crash between the swap and the cleanup leaves a `.kettle-update-previous-`
/// bundle behind. That is by design: the running process still reads resources
/// out of the bundle it launched from, so the displaced copy is kept until a
/// later run can drop it safely. A crash before the swap leaves a
/// `.kettle-update-staged-` bundle, which was never live and is simply dropped.
pub(crate) fn sweep_interrupted_updates(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(STAGED_PREFIX) && !name.starts_with(PREVIOUS_PREFIX) {
            continue;
        }
        // A name is not authority to delete. Anyone who can write this
        // directory can rename someone else's tree to look like our leftover,
        // and a recursive delete would then destroy it using rights the
        // attacker does not have. The ownership test is the half that carries
        // weight here; `remove_dir_all` already refuses to traverse a symlink,
        // so `symlink_metadata` is belt-and-braces rather than the guard.
        //
        // The foreign-owner branch has no automated coverage: reaching it needs
        // a file owned by a second uid, which a test cannot create. Recorded in
        // docs/AUDIT-DEFERRED.md rather than covered by a test that would pass
        // either way.
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        // An in-flight staging directory holds an advisory lock on itself for
        // as long as it exists. Asking the directory beats asking a lock file,
        // because two processes can disagree about where lock files live if
        // they inherited different environments, and cannot disagree about
        // this.
        let Ok(candidate) = open_directory(&entry.path()) else {
            continue;
        };
        let free =
            unsafe { libc::flock(candidate.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !free {
            continue;
        }
        drop(candidate);
        remove_leftover(&entry.path(), &metadata);
    }
}

/// Download, verify, and swap in a new bundle.
pub(crate) fn install_bundle_update(
    client: &FeedClient,
    update: &AvailableUpdate,
    install: &ManagedInstall,
    verifier: &dyn SealVerifier,
) -> Result<InstallOutcome, UpdateError> {
    reverify_available_update(update, &UPDATE_PUBLIC_KEY, std::time::SystemTime::now())?;
    let running = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| UpdateError::InvalidCurrentVersion(error.to_string()))?;
    require_strict_upgrade(&update.version, &running)?;

    let asset = update
        .asset
        .as_ref()
        .ok_or(UpdateError::UnsupportedPlatform)?;
    if current_target() != Some(asset.target.as_str()) {
        return Err(UpdateError::MalformedManifest(
            "selected artifact does not match this platform".to_string(),
        ));
    }

    let bundle = install.prefix.as_path();
    let parent = bundle.parent().ok_or_else(|| {
        UpdateError::UnmanagedInstall("the bundle has no containing directory".to_string())
    })?;
    let lock_path = update_lock_path(bundle)?;
    let _lock = kettle_state::ExclusiveFileLock::try_acquire(&lock_path)?
        .ok_or(UpdateError::UpdateLocked)?;
    sweep_interrupted_updates(parent);

    // Held in memory from download through extraction, for the same reason the
    // Linux path does: there is no temporary archive on disk for anyone to
    // swap between the digest check and the read that trusts it.
    let archive = client.download_bytes(update)?;
    verify_sha256_bytes(&archive, &asset.sha256)?;

    let id = unique_suffix();
    let staging = Staging::create(parent, &id)?;
    if let Err(error) = stage_verified_bundle(&staging, &archive, verifier) {
        staging.discard();
        return Err(error);
    }

    let parent_directory = open_directory(parent)?;
    let live_name = bundle.file_name().ok_or_else(|| {
        UpdateError::UnmanagedInstall("the bundle path has no final component".to_string())
    })?;

    // The source is named relative to the staging descriptor, which was opened
    // when the directory was created. Renaming that directory in the meantime
    // cannot redirect this, and its 0700 mode means nobody else could have put
    // anything under the name being exchanged.
    match kettle_state::swap_directory_entries(
        &staging.directory,
        std::ffi::OsStr::new(ARCHIVE_ROOT),
        &parent_directory,
        live_name,
    ) {
        Ok(()) => {}
        Err(kettle_state::SwapFailure::NotSwapped(error)) => {
            staging.discard();
            return Err(UpdateError::Transaction(format!(
                "could not exchange {} with the verified update: {error}",
                bundle.display()
            )));
        }
        Err(kettle_state::SwapFailure::NotDurable(error)) => {
            // The exchange already happened. Discarding the staging directory
            // here would delete the bundle this process is still running from,
            // and reporting failure would be a lie: the update is installed.
            log::warn!("the macOS bundle update is installed but was not flushed: {error}");
        }
    }

    // The staging directory now holds the bundle that was live. It is not
    // deleted here: this process still reads its icon and asset catalog out of
    // those bytes, and pulling the directory away from a live app buys nothing.
    // Startup sweeps it once nothing is running from it.
    let _ = std::fs::rename(&staging.path, parent.join(format!("{PREVIOUS_PREFIX}{id}")));

    Ok(InstallOutcome {
        version: update.version.clone(),
        executable: install.executable.clone(),
        disposition: InstallDisposition::Applied,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// Stands in for `codesign` and `spctl`, which cannot vouch for a bundle
    /// nobody notarized. See [`SealVerifier`] for why this seam exists.
    struct StubSeal {
        identity: SignedIdentity,
        notarized: bool,
    }

    impl StubSeal {
        fn official() -> Self {
            Self {
                identity: SignedIdentity {
                    identifier: BUNDLE_IDENTIFIER.into(),
                    team: TEAM_IDENTIFIER.into(),
                },
                notarized: true,
            }
        }

        fn signed_by(identifier: &str, team: &str) -> Self {
            Self {
                identity: SignedIdentity {
                    identifier: identifier.into(),
                    team: team.into(),
                },
                notarized: true,
            }
        }
    }

    impl SealVerifier for StubSeal {
        fn identity(&self, _bundle: &Path) -> Result<SignedIdentity, UpdateError> {
            Ok(self.identity.clone())
        }

        fn notarized(&self, bundle: &Path) -> Result<(), UpdateError> {
            if self.notarized {
                return Ok(());
            }
            Err(UpdateError::UnsafeArchive(format!(
                "{} is not notarized",
                bundle.display()
            )))
        }
    }

    /// The four files a real archive carries, keyed by their in-archive path.
    fn bundle_files() -> BTreeMap<String, (Vec<u8>, u32)> {
        [
            ("Contents/MacOS/kettle", b"mach-o".as_slice(), 0o755),
            ("Contents/Info.plist", b"<plist/>".as_slice(), 0o644),
            ("Contents/CodeResources", b"s8ch-ticket".as_slice(), 0o644),
            (
                "Contents/_CodeSignature/CodeResources",
                b"<plist>seal</plist>".as_slice(),
                0o644,
            ),
        ]
        .into_iter()
        .map(|(path, bytes, mode)| (format!("{ARCHIVE_ROOT}/{path}"), (bytes.to_vec(), mode)))
        .collect()
    }

    fn zip_from(files: &BTreeMap<String, (Vec<u8>, u32)>) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, (bytes, mode)) in files {
            let options = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            writer.start_file(path, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn official_zip() -> Vec<u8> {
        zip_from(&bundle_files())
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Build `<root>/kettle.app/Contents/MacOS/kettle` and hand back the exe.
    fn seed_bundle(root: &Path, name: &str) -> PathBuf {
        let executable = root.join(name).join("Contents/MacOS/kettle");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"mach-o").unwrap();
        executable
    }

    #[test]
    fn parse_identity_reads_what_codesign_actually_prints() {
        let _serial = serialized();
        // Trimmed from real `codesign -dvvv` output, which goes to stderr.
        let described = "Executable=/Applications/kettle.app/Contents/MacOS/kettle\n\
             Identifier=org.kettle.terminal\n\
             Format=app bundle with Mach-O universal (x86_64 arm64)\n\
             Authority=Developer ID Application: Someone (D49LMN8545)\n\
             TeamIdentifier=D49LMN8545\n";
        assert_eq!(
            parse_identity(described),
            Some(SignedIdentity {
                identifier: "org.kettle.terminal".into(),
                team: "D49LMN8545".into(),
            })
        );
        // An unsigned bundle prints neither field.
        assert_eq!(
            parse_identity("Executable=/tmp/x\ncode object is not signed"),
            None
        );
        // A missing team is not a partial success.
        assert_eq!(parse_identity("Identifier=org.kettle.terminal\n"), None);
    }

    #[test]
    fn only_a_real_bundle_layout_is_accepted() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-layout-");
        let root = temp.path();
        let seal = StubSeal::official();

        let good = seed_bundle(root, "kettle.app");
        let install = locate_bundle_install(&good, &seal).unwrap();
        assert!(install.prefix.ends_with("kettle.app"));
        assert!(install.marker_path.ends_with("Contents/Info.plist"));

        // A bare binary on PATH is not a bundle, however it is named.
        let loose = root.join("kettle");
        std::fs::write(&loose, b"mach-o").unwrap();
        let error = locate_bundle_install(&loose, &seal).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("kettle.app/Contents/MacOS/kettle"),
            "{error}"
        );

        // Right depth, wrong extension: a directory that merely looks similar.
        let not_an_app = seed_bundle(root, "kettle.bundle");
        let error = locate_bundle_install(&not_an_app, &seal).unwrap_err();
        assert!(error.to_string().contains("not an .app bundle"), "{error}");
    }

    #[test]
    fn copies_another_tool_owns_are_refused_with_a_next_step() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-owner-");
        let seal = StubSeal::official();

        // Homebrew, both formula and cask layouts.
        for owner in ["Cellar", "Caskroom"] {
            let root = temp.path().join(owner).join("kettle/3.1.1");
            std::fs::create_dir_all(&root).unwrap();
            let executable = seed_bundle(&root, "kettle.app");
            let error = locate_bundle_install(&executable, &seal).unwrap_err();
            assert!(
                error.to_string().contains("brew upgrade"),
                "{owner} should point at Homebrew, got: {error}"
            );
        }

        // Gatekeeper's read-only mount for a quarantined app.
        let translocated = temp.path().join("AppTranslocation/ABC-123/d");
        std::fs::create_dir_all(&translocated).unwrap();
        let executable = seed_bundle(&translocated, "kettle.app");
        let error = locate_bundle_install(&executable, &seal).unwrap_err();
        assert!(error.to_string().contains("translocated"), "{error}");
        assert!(error.to_string().contains("/Applications"), "{error}");
    }

    #[test]
    fn a_bundle_signed_by_anyone_else_is_not_ours() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-identity-");
        let executable = seed_bundle(temp.path(), "kettle.app");

        // A locally built app is ad-hoc signed and reports no real team.
        // `codesign` reports an ad-hoc signature's team as the literal
        // "not set", which is what a locally built app looks like.
        for adhoc_team in ["-", "not set"] {
            let adhoc = StubSeal::signed_by("org.kettle.terminal", adhoc_team);
            let error = locate_bundle_install(&executable, &adhoc).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("rebuild and reinstall"), "{message}");
            assert!(
                message.contains("ad-hoc signed") && !message.contains("by team"),
                "an ad-hoc bundle should not be described as belonging to a team: {message}"
            );
        }

        // Right team, someone else's app.
        let impostor = StubSeal::signed_by("com.example.other", "ZZZZZZZZZZ");
        let error = locate_bundle_install(&executable, &impostor).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("not an official kettle build"),
            "{message}"
        );
        assert!(message.contains("team ZZZZZZZZZZ"), "{message}");
    }

    #[test]
    fn extraction_round_trips_a_bundle_and_normalizes_its_modes() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-extract-");
        let staging = Staging::create(temp.path(), "1").unwrap();
        extract_bundle_into(&staging, &official_zip()).unwrap();
        let bundle = staging.bundle();

        assert_eq!(
            std::fs::read(bundle.join("Contents/CodeResources")).unwrap(),
            b"s8ch-ticket",
            "the stapled ticket has to survive extraction or the install fails Gatekeeper"
        );
        assert_eq!(
            mode_of(&bundle.join("Contents/MacOS/kettle")),
            0o755,
            "the binary stays executable"
        );
        assert_eq!(mode_of(&bundle.join("Contents/Info.plist")), 0o644);
    }

    const UMASK_CHILD: &str = "KETTLE_TEST_UMASK_CHILD";

    #[test]
    fn nothing_in_a_staged_bundle_is_writable_by_anyone_else() {
        // Directory modes were the gap: file modes were normalized while
        // `create_dir_all` left every directory at 0777 minus the umask. Under
        // `umask 002` that is 0775, so any member of the group could replace a
        // nominally 0644 file inside an installed bundle and break its seal.
        //
        // Proving that needs a umask set, and a umask is process-wide while
        // cargo runs tests as threads. A mutex would only cover this module,
        // and any test in the binary creating a file at the wrong moment would
        // pick up 0775 and fail somewhere unrelated. So re-run this one test in
        // a child process that owns its umask outright.
        if std::env::var_os(UMASK_CHILD).is_none() {
            let binary = std::env::current_exe().expect("the test binary knows its own path");
            let status = Command::new(binary)
                .args([
                    "macos::tests::nothing_in_a_staged_bundle_is_writable_by_anyone_else",
                    "--exact",
                    "--nocapture",
                ])
                .env(UMASK_CHILD, "1")
                .status()
                .expect("the test binary re-runs");
            assert!(status.success(), "the umask-isolated child failed");
            return;
        }

        unsafe { libc::umask(0o002) };
        let temp = kettle_test_support::private_tempdir("kettle-macos-modes-");
        let staging = Staging::create(temp.path(), "2").unwrap();
        extract_bundle_into(&staging, &official_zip()).unwrap();

        assert_eq!(
            mode_of(&staging.path),
            0o700,
            "the staging directory is private while the bundle is unverified"
        );
        for relative in ["", "Contents", "Contents/MacOS", "Contents/_CodeSignature"] {
            let directory = staging.bundle().join(relative);
            assert_eq!(
                mode_of(&directory) & 0o022,
                0,
                "{} is writable by group or other",
                directory.display()
            );
        }
    }

    /// Access-control entries a path carries, as `ls -lde` reports them.
    fn extended_acl(path: &Path) -> Vec<String> {
        let listing = Command::new("/bin/ls")
            .arg("-lde")
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter(|line| line.contains(" allow ") || line.contains(" deny "))
            .map(|line| line.trim().to_string())
            .collect()
    }

    #[test]
    fn an_inheritable_acl_on_the_parent_does_not_reach_the_staged_bundle() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-acl-");
        let root = temp.path();

        // Darwin ACLs are independent of the mode bits, and they inherit. A
        // directory created 0755 inside a parent carrying this entry is still
        // writable by everyone, so mode bits alone prove nothing.
        let applied = Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow write,file_inherit,directory_inherit")
            .arg(root)
            .status()
            .unwrap();
        assert!(applied.success(), "could not set up the inheritable ACL");
        assert!(
            !extended_acl(root).is_empty(),
            "the parent really carries one"
        );

        let staging = Staging::create(root, "acl").unwrap();
        extract_bundle_into(&staging, &official_zip()).unwrap();

        assert_eq!(
            extended_acl(&staging.path),
            Vec::<String>::new(),
            "the staging directory inherited an access-control entry"
        );
        for relative in ["", "Contents", "Contents/MacOS", "Contents/MacOS/kettle"] {
            let path = staging.bundle().join(relative);
            assert_eq!(
                extended_acl(&path),
                Vec::<String>::new(),
                "{} inherited an access-control entry",
                path.display()
            );
        }
    }

    #[test]
    fn extraction_refuses_archives_that_are_not_a_kettle_bundle() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-refuse-");
        let mut index = 0;
        let mut fresh = || {
            index += 1;
            Staging::create(temp.path(), &format!("r{index}")).unwrap()
        };

        // Anything outside the single bundle root, including a sibling the
        // swap would carry into /Applications.
        let mut stray = bundle_files();
        stray.insert("evil.sh".into(), (b"rm -rf".to_vec(), 0o755));
        let error = extract_bundle_into(&fresh(), &zip_from(&stray)).unwrap_err();
        assert!(error.to_string().contains(ARCHIVE_ROOT), "{error}");

        // Traversal is caught before the root check even applies.
        let mut escape = bundle_files();
        escape.insert("kettle.app/../../etc/passwd".into(), (b"x".to_vec(), 0o644));
        assert!(extract_bundle_into(&fresh(), &zip_from(&escape)).is_err());

        // Each mandatory file, dropped one at a time.
        for missing in MANDATORY_ENTRIES {
            let mut files = bundle_files();
            files.remove(&format!("{ARCHIVE_ROOT}/{missing}")).unwrap();
            let error = extract_bundle_into(&fresh(), &zip_from(&files)).unwrap_err();
            assert!(
                error.to_string().contains(missing),
                "dropping {missing} should be named in the error, got: {error}"
            );
        }
    }

    #[test]
    fn extraction_will_not_write_through_a_symlink_left_in_its_way() {
        let _serial = serialized();
        // The arbitrary-write primitive this closes: if extraction created
        // files by pathname, a symlink planted at a destination name would
        // redirect an authenticated write onto whatever it pointed at.
        let temp = kettle_test_support::private_tempdir("kettle-macos-symlink-");
        let victim = temp.path().join("victim");
        std::fs::write(&victim, b"do not clobber").unwrap();

        let staging = Staging::create(temp.path(), "3").unwrap();
        std::fs::create_dir_all(staging.bundle().join("Contents")).unwrap();
        std::os::unix::fs::symlink(&victim, staging.bundle().join("Contents/Info.plist")).unwrap();

        let error = extract_bundle_into(&staging, &official_zip()).unwrap_err();
        assert!(
            matches!(error, UpdateError::Io(_)),
            "expected the create to refuse, got {error:?}"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"do not clobber",
            "the file the symlink pointed at must be untouched"
        );
    }

    #[test]
    fn extraction_refuses_an_archive_with_too_many_entries() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-entries-");
        let staging = Staging::create(temp.path(), "4").unwrap();
        let mut files = bundle_files();
        for index in 0..=MAX_ARCHIVE_ENTRIES {
            files.insert(
                format!("{ARCHIVE_ROOT}/Contents/Resources/f{index}.txt"),
                (b"x".to_vec(), 0o644),
            );
        }
        let error = extract_bundle_into(&staging, &zip_from(&files)).unwrap_err();
        assert!(error.to_string().contains("entries"), "{error}");
    }

    #[test]
    fn staging_refuses_a_bundle_apple_has_not_notarized() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-notarize-");

        // The regression this exists to prevent: re-signing a bundle keeps
        // `codesign --verify` happy while orphaning its stapled ticket, so a
        // signature check alone would install a build Gatekeeper then blocks.
        let unnotarized = StubSeal {
            identity: SignedIdentity {
                identifier: BUNDLE_IDENTIFIER.into(),
                team: TEAM_IDENTIFIER.into(),
            },
            notarized: false,
        };
        let staging = Staging::create(temp.path(), "n1").unwrap();
        let error = stage_verified_bundle(&staging, &official_zip(), &unnotarized).unwrap_err();
        assert!(error.to_string().contains("not notarized"), "{error}");

        // A correctly signed but foreign bundle is refused before that.
        let foreign = StubSeal::signed_by("com.example.other", "ZZZZZZZZZZ");
        let staging = Staging::create(temp.path(), "n2").unwrap();
        let error = stage_verified_bundle(&staging, &official_zip(), &foreign).unwrap_err();
        assert!(error.to_string().contains("com.example.other"), "{error}");

        let staging = Staging::create(temp.path(), "n3").unwrap();
        stage_verified_bundle(&staging, &official_zip(), &StubSeal::official()).unwrap();
    }

    #[test]
    fn a_staging_directory_is_recognised_by_ownership_and_mode() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-ident-");
        let root = temp.path();

        // What `Staging::create` makes.
        let ours = root.join("ours");
        std::fs::create_dir(&ours).unwrap();
        std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(staging_is_ours(&open_directory(&ours).unwrap()).unwrap());

        // What an attacker racing between mkdirat and openat would leave: a
        // directory they can still reach. They cannot make it 0700 *and* ours,
        // so the mode alone gives them away.
        let theirs = root.join("theirs");
        std::fs::create_dir(&theirs).unwrap();
        std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!staging_is_ours(&open_directory(&theirs).unwrap()).unwrap());

        let readable = root.join("readable");
        std::fs::create_dir(&readable).unwrap();
        std::fs::set_permissions(&readable, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!staging_is_ours(&open_directory(&readable).unwrap()).unwrap());
    }

    #[test]
    fn discarding_staging_never_deletes_what_replaced_it() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-discard-");
        let root = temp.path();
        let staging = Staging::create(root, "d1").unwrap();

        // Someone moves ours aside and leaves their own tree at that path. A
        // recursive delete keyed on the pathname would destroy it for them,
        // using rights they do not have.
        std::fs::rename(&staging.path, root.join("moved")).unwrap();
        std::fs::create_dir(&staging.path).unwrap();
        std::fs::write(staging.path.join("theirs"), b"not ours to delete").unwrap();

        let planted = staging.path.clone();
        staging.discard();
        assert!(
            planted.join("theirs").exists(),
            "discard must leave a tree that is no longer the one it created"
        );
    }

    #[test]
    fn the_sweep_removes_only_update_leftovers() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-sweep-");
        let root = temp.path();
        for name in [
            ".kettle-update-staged-9-9",
            ".kettle-update-previous-9-9",
            "kettle.app",
            "Some Other App.app",
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        sweep_interrupted_updates(root);

        assert!(root.join("kettle.app").exists(), "the live bundle survives");
        assert!(
            root.join("Some Other App.app").exists(),
            "the sweep runs in /Applications; it must not touch anyone else's app"
        );
        assert!(!root.join(".kettle-update-staged-9-9").exists());
        assert!(!root.join(".kettle-update-previous-9-9").exists());
    }

    /// Serialize this module's tests.
    ///
    /// Two of them change process-wide state: one points `XDG_STATE_HOME` at a
    /// temporary root, and one sets the umask to prove the extractor does not
    /// inherit it. Cargo runs tests as threads in one process, so either would
    /// otherwise leak into whatever else happened to be creating files at that
    /// moment. It cost an afternoon to see that, because the symptom was two
    /// unrelated tests failing on permissions. The whole module runs in well
    /// under a second, so serializing is free.
    fn serialized() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Restores `XDG_STATE_HOME` even if the test body panics. Without this a
    /// single failing assertion would leave the variable pointing at a deleted
    /// temporary directory for every test that ran afterwards.
    struct StateHome(Option<std::ffi::OsString>);

    impl Drop for StateHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
                None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
            }
        }
    }

    /// Point the private state directory at a temporary root. The caller
    /// already holds [`serialized`].
    fn with_state_home<T>(state: &Path, body: impl FnOnce() -> T) -> T {
        let _restore = StateHome(std::env::var_os("XDG_STATE_HOME"));
        unsafe { std::env::set_var("XDG_STATE_HOME", state) };
        body()
    }

    #[test]
    fn an_install_in_a_group_writable_directory_can_still_update() {
        let _serial = serialized();
        // The layout this exists for is the ordinary one. `/Applications` is
        // `drwxrwxr-x root:admin`, and an earlier version kept the update lock
        // beside the bundle, where the private-file helper refuses to create
        // one because the parent is writable by an untrusted principal. So
        // `kettle update` failed before staging anything, on the only install
        // location the documentation tells people to use. Every test used a
        // private 0700 temporary directory, and none of them noticed.
        let temp = kettle_test_support::private_tempdir("kettle-macos-shared-");
        let applications = temp.path().join("Applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::set_permissions(&applications, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert_eq!(mode_of(&applications), 0o775, "the parent really is shared");

        let executable = seed_bundle(&applications, "kettle.app");
        let bundle = locate_bundle_install(&executable, &StubSeal::official())
            .unwrap()
            .prefix;

        with_state_home(&temp.path().join("state"), || {
            let lock_path = update_lock_path(&bundle).expect("a private lock path is available");
            assert!(
                !lock_path.starts_with(&applications),
                "the lock must not be created beside the bundle: {}",
                lock_path.display()
            );
            kettle_state::ExclusiveFileLock::try_acquire(&lock_path)
                .expect("a shared parent must not stop the updater taking its lock")
                .expect("the lock is free");
        });
    }

    #[test]
    fn the_sweep_and_the_updater_agree_on_one_lock() {
        // Both key the lock by the bundle path, so they have to normalize it
        // the same way. If one canonicalizes and the other does not, a symlink
        // anywhere above the bundle gives them different locks and the mutual
        // exclusion quietly stops working.
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-agree-");
        let root = temp.path();
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let executable = seed_bundle(&real, "kettle.app");
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();

        let leftover = real.join(".kettle-update-previous-1");
        std::fs::create_dir_all(&leftover).unwrap();

        with_state_home(&root.join("state"), || {
            let canonical = locate_bundle_install(&executable, &StubSeal::official())
                .unwrap()
                .prefix;
            // Stand in for an update in flight, holding the lock the updater
            // would hold.
            let held = kettle_state::ExclusiveFileLock::try_acquire(
                &update_lock_path(&canonical).unwrap(),
            )
            .unwrap()
            .expect("the lock is free to begin with");

            // Start a process whose executable path reaches the same bundle
            // through a symlink. It must resolve to the same lock, find it
            // held, and leave the in-flight files alone.
            sweep_leftovers_beside(&root.join("link/kettle.app/Contents/MacOS/kettle"));
            assert!(
                leftover.exists(),
                "the sweep took a different lock and deleted files an update was using"
            );

            drop(held);
            sweep_leftovers_beside(&root.join("link/kettle.app/Contents/MacOS/kettle"));
            assert!(!leftover.exists(), "and sweeps once the lock is free");
        });
    }

    #[test]
    fn each_install_gets_its_own_update_lock() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-locks-");
        with_state_home(&temp.path().join("state"), || {
            let one = update_lock_path(Path::new("/Applications/kettle.app")).unwrap();
            let two =
                update_lock_path(Path::new("/Users/someone/Applications/kettle.app")).unwrap();
            assert_ne!(
                one, two,
                "two installs must not serialize against each other"
            );
            assert_eq!(
                one,
                update_lock_path(Path::new("/Applications/kettle.app")).unwrap()
            );
            for path in [&one, &two] {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(
                    !name.contains("Applications"),
                    "the lock name should not carry the install path: {name}"
                );
            }
        });
    }

    #[test]
    fn the_startup_sweep_only_runs_inside_a_bundle() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-startup-");
        let root = temp.path();
        let leftover = root.join(".kettle-update-previous-1");
        std::fs::create_dir_all(&leftover).unwrap();

        // A binary that is not inside an .app must not sweep its own directory.
        // Otherwise a `cargo run` from a checkout, or a copy sitting in a
        // downloads folder, would start deleting neighbouring paths.
        let loose = root.join("kettle");
        std::fs::write(&loose, b"mach-o").unwrap();
        let executable = seed_bundle(root, "kettle.app");
        with_state_home(&root.join("state"), || {
            sweep_leftovers_beside(&loose);
        });
        assert!(leftover.exists(), "a non-bundle binary sweeps nothing");

        // From inside the bundle, the sibling leftover goes.
        with_state_home(&root.join("state"), || {
            sweep_leftovers_beside(&executable);
        });
        assert!(!leftover.exists());
        assert!(root.join("kettle.app").exists(), "the live bundle survives");
    }

    #[test]
    fn the_startup_sweep_stands_down_while_an_update_is_in_flight() {
        // Two windows opening while a third is mid-update must not delete the
        // staging directory the updater is still filling. There are two
        // independent brakes, and this checks both, because the first one can
        // be defeated by two processes inheriting different environments.
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-sweeplock-");
        let root = temp.path();
        let staging = Staging::create(root, "inflight").unwrap();
        let in_flight = staging.path.clone();
        let lock_path = root.join("update.lock");

        // The process-wide update lock.
        let held = kettle_state::ExclusiveFileLock::try_acquire(&lock_path)
            .unwrap()
            .expect("the lock is free to begin with");
        sweep_locked(root, &lock_path);
        assert!(
            in_flight.exists(),
            "an in-flight staging directory must survive another process starting"
        );
        drop(held);

        // And the directory's own advisory lock, which is the backstop: a
        // process that resolved a different lock path still finds this held.
        sweep_locked(root, &lock_path);
        assert!(
            in_flight.exists(),
            "the staging directory's own lock must stop a sweep that got past the file lock"
        );

        // Once the update is done and the descriptor closes, it goes.
        drop(staging);
        sweep_locked(root, &lock_path);
        assert!(!in_flight.exists(), "and is swept once nothing holds it");
    }

    #[test]
    fn a_swap_exchanges_two_bundles_without_either_disappearing() {
        let _serial = serialized();
        let temp = kettle_test_support::private_tempdir("kettle-macos-swap-");
        let root = temp.path();
        let staging = Staging::create(root, "s1").unwrap();
        let live = root.join("kettle.app");

        std::fs::create_dir_all(staging.bundle().join("Contents/MacOS")).unwrap();
        std::fs::write(staging.bundle().join("Contents/MacOS/kettle"), "new").unwrap();
        std::fs::create_dir_all(live.join("Contents/MacOS")).unwrap();
        std::fs::write(live.join("Contents/MacOS/kettle"), "old").unwrap();

        let parent = open_directory(root).unwrap();
        kettle_state::swap_directory_entries(
            &staging.directory,
            std::ffi::OsStr::new(ARCHIVE_ROOT),
            &parent,
            std::ffi::OsStr::new("kettle.app"),
        )
        .unwrap();

        let read =
            |path: &Path| std::fs::read_to_string(path.join("Contents/MacOS/kettle")).unwrap();
        assert_eq!(read(&live), "new", "the live path now holds the update");
        assert_eq!(
            read(&staging.bundle()),
            "old",
            "and the displaced bundle is still readable, which is what lets a \
             running process keep reading its own resources"
        );
    }

    #[test]
    fn a_swap_survives_its_staging_directory_being_renamed_away() {
        let _serial = serialized();
        // The substitution this closes. An attacker who can write the enclosing
        // directory (every administrator, for `/Applications`) renames the
        // staged tree aside after verification and puts their own under the
        // name. Resolving the source against a descriptor taken at creation
        // means the exchange still moves the bytes that were verified.
        let temp = kettle_test_support::private_tempdir("kettle-macos-substitute-");
        let root = temp.path();
        let staging = Staging::create(root, "s2").unwrap();
        std::fs::create_dir_all(staging.bundle()).unwrap();
        std::fs::write(staging.bundle().join("marker"), "verified").unwrap();

        let live = root.join("kettle.app");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("marker"), "old").unwrap();

        // Attacker moves ours aside and plants a replacement at the same path.
        let hijack = root.join("attacker-copy");
        std::fs::rename(&staging.path, &hijack).unwrap();
        std::fs::create_dir_all(staging.path.join(ARCHIVE_ROOT)).unwrap();
        std::fs::write(staging.bundle().join("marker"), "malicious").unwrap();

        let parent = open_directory(root).unwrap();
        kettle_state::swap_directory_entries(
            &staging.directory,
            std::ffi::OsStr::new(ARCHIVE_ROOT),
            &parent,
            std::ffi::OsStr::new("kettle.app"),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(live.join("marker")).unwrap(),
            "verified",
            "the exchange must move the verified tree, not whatever now sits at that path"
        );
    }

    /// The whole path against a genuinely notarized archive, with the real
    /// `codesign` and `spctl` rather than the stub.
    ///
    /// Skipped unless `KETTLE_MACOS_ARCHIVE` points at a published
    /// `kettle-macos-universal.zip`. The bytes are 27 MiB and cannot be
    /// committed, and no synthesized fixture can be notarized, so this is the
    /// only way to exercise [`AppleSeal`] for real. `just macos-update-smoke`
    /// downloads the archive and sets the variable.
    #[test]
    fn a_published_archive_extracts_verifies_and_swaps_for_real() {
        let _serial = serialized();
        let Ok(archive) = std::env::var("KETTLE_MACOS_ARCHIVE") else {
            // Same convention as the mermaid gate: skipping is fine locally,
            // but a run that claims to cover this must be able to fail. Without
            // the escape hatch this test could quietly stop testing anything.
            assert!(
                std::env::var("KETTLE_MACOS_ARCHIVE_REQUIRED").is_err(),
                "KETTLE_MACOS_ARCHIVE_REQUIRED is set but KETTLE_MACOS_ARCHIVE is not; \
                 the live macOS update check cannot run"
            );
            eprintln!(
                "skipped: set KETTLE_MACOS_ARCHIVE to a published kettle-macos-universal.zip"
            );
            return;
        };
        let archive = std::fs::read(&archive).expect("KETTLE_MACOS_ARCHIVE is readable");
        let temp = kettle_test_support::private_tempdir("kettle-macos-live-");
        let root = temp.path();

        // One extraction stands in for the installed app, the other is the
        // update being staged beside it.
        let installed = Staging::create(root, "live-a").unwrap();
        extract_bundle_into(&installed, &archive).unwrap();
        std::fs::rename(installed.bundle(), root.join("kettle.app")).unwrap();
        let live = root.join("kettle.app");

        let staging = Staging::create(root, "live-b").unwrap();
        stage_verified_bundle(&staging, &archive, &AppleSeal).expect(
            "a published archive must satisfy codesign and spctl after plain zip extraction",
        );

        // The installed copy must also read as ours, which is what gates
        // `kettle update` on a real machine.
        let install = locate_bundle_install(&live.join("Contents/MacOS/kettle"), &AppleSeal)
            .expect("an extracted release bundle is a managed install");
        assert_eq!(install.prefix, live.canonicalize().unwrap());

        let parent = open_directory(root).unwrap();
        kettle_state::swap_directory_entries(
            &staging.directory,
            std::ffi::OsStr::new(ARCHIVE_ROOT),
            &parent,
            std::ffi::OsStr::new("kettle.app"),
        )
        .unwrap();
        AppleSeal
            .notarized(&live)
            .expect("the swapped-in bundle is still notarized");
        assert!(
            live.join("Contents/CodeResources").exists(),
            "the stapled ticket survived extraction and the swap"
        );
    }
}
