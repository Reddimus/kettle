#[cfg(any(windows, target_os = "linux", test))]
use std::fs;
#[cfg(any(windows, target_os = "linux"))]
use std::fs::File;
#[cfg(any(windows, target_os = "linux"))]
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(any(windows, target_os = "linux"))]
use std::fs::OpenOptions;
#[cfg(any(windows, target_os = "linux"))]
use std::io::Read;
#[cfg(any(windows, target_os = "linux"))]
use std::io::Seek;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::path::Component;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use sha2::{Digest as _, Sha256};

use crate::feed::{AvailableUpdate, FeedClient, UpdateError};
#[cfg(windows)]
use crate::feed::{MAX_ARTIFACT_BYTES, SignedManifest};
#[cfg(any(windows, target_os = "linux"))]
use crate::feed::{require_strict_upgrade, reverify_available_update};
// The compiled verification key sits behind the same platform gate as the
// authenticated install paths that use it, so a target without one would
// import it unused and trip `-D unused-imports`. macOS reaches the key
// through `crate::macos` instead.
#[cfg(any(windows, target_os = "linux"))]
use crate::UPDATE_PUBLIC_KEY;
use crate::current_target;

const MARKER_SCHEMA: u32 = 1;
#[cfg(any(windows, target_os = "linux"))]
const JOURNAL_SCHEMA: u32 = 2;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) const MAX_ARCHIVE_ENTRIES: usize = 128;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(any(windows, target_os = "linux"))]
const PACKAGE_MANIFEST_FILE: &str = "kettle-package-manifest.json";
#[cfg(target_os = "linux")]
const UNIX_INSTALL_PROVENANCE_FILE: &str = "share/kettle/install-files.json";
#[cfg(any(windows, target_os = "linux"))]
const MAX_PACKAGE_MANIFEST_BYTES: usize = 256 * 1024;
#[cfg(windows)]
const PENDING_SCHEMA: u32 = 3;
#[cfg(windows)]
const PENDING_FILE: &str = ".kettle-update-pending.json";
#[cfg(windows)]
const RUNNING_LOCK_FILE: &str = ".kettle-running.lock";
#[cfg(windows)]
const MAX_PENDING_ATTEMPTS: u32 = 3;
#[cfg(windows)]
const MAX_HANDOFF_TIMEOUTS: u32 = 3;
#[cfg(windows)]
const HANDOFF_TIMEOUT_GRACE_NANOS: u128 = 5 * 60 * 1_000_000_000;
#[cfg(windows)]
const FAILED_PENDING_PREFIX: &str = ".kettle-update-failed-";
#[cfg(windows)]
const MAX_PENDING_RECORD_BYTES: usize = 1024 * 1024;
#[cfg(windows)]
const MAX_FAILED_PENDING_TRANSACTIONS: usize = 8;
#[cfg(any(windows, target_os = "linux"))]
const BACKUP_MARKER_FILE: &str = ".kettle-update-backup.json";
#[cfg(windows)]
const WINDOWS_ALLOWED_ROOTS: &[&str] = &[
    "kettle.exe",
    "kettle.com",
    "install.ps1",
    "kettle.ico",
    "LICENSE",
    "NOTICE",
    "README.md",
    "CHANGELOG.md",
    "kettle-package-manifest.json",
    "shell-integration",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct InstallMarker {
    schema: u32,
    product: String,
    managed_by: String,
    channel: String,
    target: String,
    version: String,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UnixInstallProvenance {
    schema: u32,
    product: String,
    managed_by: String,
    prefix: String,
    owner_uid: u32,
    files: Vec<UnixInstallFile>,
    directories: Vec<UnixInstallDirectory>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UnixInstallFile {
    path: String,
    size: u64,
    sha256: String,
    mode: u32,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UnixInstallDirectory {
    path: String,
    mode: u32,
}

#[derive(Debug, Clone)]
pub struct ManagedInstall {
    pub prefix: PathBuf,
    pub executable: PathBuf,
    pub marker_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub version: semver::Version,
    pub executable: PathBuf,
    pub disposition: InstallDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallDisposition {
    /// The transaction was committed before `install_update` returned.
    Applied,
    /// Windows staged the verified update for an out-of-process helper. The
    /// helper applies it once every Kettle process has released its run lock.
    Staged { transaction_id: String },
}

/// Keeps a managed Windows installation mapped as running. The staged helper
/// takes the corresponding exclusive lock before replacing `kettle.exe`.
pub struct RunningInstallGuard {
    #[cfg(windows)]
    _lock: Option<kettle_state::SharedFileLock>,
}

/// Startup decision made before the normal CLI or window initialization.
pub enum ProcessStart {
    Ready {
        guard: RunningInstallGuard,
        /// A failed Windows update was quarantined (or could not yet be
        /// quarantined) so the intact prior binary could continue starting.
        warning: Option<String>,
    },
    /// A verified update is pending. A helper was started (or is already
    /// waiting), so this process must exit without dropping the shared run
    /// lock. The operating system releases the lock after the old image has
    /// fully unmapped.
    PendingUpdate { guard: RunningInstallGuard },
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingUpdate {
    schema: u32,
    product: String,
    target: String,
    transaction_id: String,
    target_version: String,
    archive: String,
    archive_size: u64,
    archive_sha256: String,
    release_manifest: String,
    release_signature: String,
    asset: crate::ManifestAsset,
    package_manifest: String,
    helper: String,
    helper_size: u64,
    helper_sha256: String,
    attempts: u32,
    #[serde(default)]
    handoff_timeouts: u32,
    #[serde(default)]
    last_error: Option<String>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BackupMarker {
    schema: u32,
    product: String,
    transaction_id: String,
}

#[cfg(windows)]
struct VerifiedWindowsHelper {
    path: PathBuf,
    _parent: AnchoredParent,
    _file: File,
}

#[cfg(windows)]
enum PendingStartInspection {
    Retry {
        fingerprint: Option<String>,
    },
    Failed {
        fingerprint: Option<String>,
        reason: String,
    },
}

#[cfg(windows)]
fn inspect_pending_start(prefix: &Path) -> Option<PendingStartInspection> {
    let installed = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("the crate package version is valid semver");
    inspect_pending_start_with(prefix, &UPDATE_PUBLIC_KEY, SystemTime::now(), &installed)
}

#[cfg(windows)]
fn inspect_pending_start_with(
    prefix: &Path,
    public_key: &[u8; 32],
    now: SystemTime,
    installed: &semver::Version,
) -> Option<PendingStartInspection> {
    let path = prefix.join(PENDING_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            return Some(PendingStartInspection::Failed {
                fingerprint: None,
                reason: format!("the pending update record cannot be inspected: {error}"),
            });
        }
    };
    if !metadata.file_type().is_file() {
        return Some(PendingStartInspection::Failed {
            fingerprint: None,
            reason: "the pending update record is not a regular file".into(),
        });
    }
    let fingerprint = pending_file_fingerprint(&path).ok();
    match load_pending(prefix).and_then(|pending| {
        authenticate_pending_upgrade(&pending, public_key, now, installed)?;
        Ok(pending)
    }) {
        Ok(pending) if pending.attempts >= MAX_PENDING_ATTEMPTS => {
            Some(PendingStartInspection::Failed {
                fingerprint,
                reason: format!(
                    "the staged update failed {} times{}",
                    pending.attempts,
                    pending
                        .last_error
                        .as_deref()
                        .map(|error| format!(": {error}"))
                        .unwrap_or_default()
                ),
            })
        }
        Ok(pending) if pending.handoff_timeouts >= MAX_HANDOFF_TIMEOUTS => {
            Some(PendingStartInspection::Failed {
                fingerprint,
                reason: format!(
                    "the staged update could not take over from a still-running Kettle process {} times{}",
                    pending.handoff_timeouts,
                    pending
                        .last_error
                        .as_deref()
                        .map(|error| format!(": {error}"))
                        .unwrap_or_default()
                ),
            })
        }
        Ok(_) => Some(PendingStartInspection::Retry { fingerprint }),
        Err(error) => Some(PendingStartInspection::Failed {
            fingerprint,
            reason: format!("the pending update record is invalid: {error}"),
        }),
    }
}

#[cfg(windows)]
fn pending_file_fingerprint(path: &Path) -> Result<String, UpdateError> {
    let (mut file, _) = open_transaction_snapshot(path)?;
    pending_file_fingerprint_from_open(&mut file)
}

#[cfg(windows)]
fn pending_file_fingerprint_from_open(file: &mut File) -> Result<String, UpdateError> {
    let metadata = file.metadata()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    let mut identity = format!("file:{}:{modified}:", metadata.len());
    file.rewind()?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(file)
        .take(MAX_PENDING_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PENDING_RECORD_BYTES {
        return Err(UpdateError::Transaction(
            "pending record exceeds the safety limit".into(),
        ));
    }
    identity.push_str(&sha256_bytes(&bytes));
    Ok(identity)
}

#[cfg(windows)]
fn quarantine_pending_warning(
    prefix: &Path,
    expected_fingerprint: &Option<String>,
    reason: String,
) -> String {
    let quarantined = match expected_fingerprint {
        Some(fingerprint) => match try_quarantine_pending(prefix, fingerprint, &reason) {
            Ok(path) => path,
            Err(error) => {
                log::warn!("could not quarantine failed pending update: {error}");
                None
            }
        },
        None => None,
    };
    pending_start_warning(&reason, quarantined.as_deref())
}

#[cfg(windows)]
fn pending_start_warning(reason: &str, quarantined: Option<&Path>) -> String {
    let evidence = quarantined.map_or_else(
        || {
            format!(
                "Evidence remains at {} until no other Kettle process is using it.",
                PENDING_FILE
            )
        },
        |path| format!("The pending record was quarantined at {}.", path.display()),
    );
    format!(
        "Kettle kept the currently installed version because {reason}. {evidence} Run `kettle update --yes` to retry."
    )
}

#[cfg(windows)]
fn try_quarantine_pending(
    prefix: &Path,
    expected_fingerprint: &str,
    reason: &str,
) -> Result<Option<PathBuf>, UpdateError> {
    let Some(_update_lock) =
        kettle_state::ExclusiveFileLock::try_acquire(&prefix.join(".kettle-update.lock"))?
    else {
        return Ok(None);
    };
    let Some(_running_lock) =
        kettle_state::ExclusiveFileLock::try_acquire(&prefix.join(RUNNING_LOCK_FILE))?
    else {
        return Ok(None);
    };
    let mut pending = match open_windows_held_file(prefix, Path::new(PENDING_FILE)) {
        Ok(pending) => pending,
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let current = pending_file_fingerprint_from_open(&mut pending.file)?;
    if current != expected_fingerprint {
        return Ok(None);
    }
    let suffix = load_pending(prefix)
        .ok()
        .map(|pending| pending.transaction_id)
        .unwrap_or_else(unique_suffix);
    prune_failed_pending_records(prefix, MAX_FAILED_PENDING_TRANSACTIONS.saturating_sub(1))?;
    let quarantined = prefix.join(format!("{FAILED_PENDING_PREFIX}{suffix}.json"));
    let quarantine_name = quarantined
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| UpdateError::Transaction("invalid quarantine file name".into()))?;
    rename_windows_held_file(&pending, quarantine_name)?;
    drop(pending);
    if let Err(error) = atomic_write(
        &prefix.join(format!("{FAILED_PENDING_PREFIX}{suffix}.txt")),
        format!("{reason}\n").as_bytes(),
        Some(0o600),
    ) {
        log::warn!(
            "pending update record was quarantined at {} but its diagnostic could not be written: {error}",
            quarantined.display()
        );
    }
    if let Err(error) = sync_parent(prefix) {
        log::warn!(
            "pending update quarantine at {} could not be directory-synced: {error}",
            quarantined.display()
        );
    }
    Ok(Some(quarantined))
}

#[cfg(windows)]
fn failed_pending_record_name(name: &str) -> Option<(&str, &str)> {
    let suffix = name.strip_prefix(FAILED_PENDING_PREFIX)?;
    let (transaction_id, extension) = suffix.rsplit_once('.')?;
    if is_transaction_id(transaction_id) && matches!(extension, "json" | "txt") {
        Some((transaction_id, extension))
    } else {
        None
    }
}

#[cfg(windows)]
fn prune_failed_pending_records(
    prefix: &Path,
    keep_transactions: usize,
) -> Result<(), UpdateError> {
    let mut transactions: std::collections::BTreeMap<(u128, u32), (String, Vec<WindowsHeldFile>)> =
        std::collections::BTreeMap::new();
    for entry in fs::read_dir(prefix)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some((transaction_id, _)) = failed_pending_record_name(name) {
            let held = open_windows_held_file(prefix, Path::new(name))?;
            let (process_id, epoch_nanos) = transaction_id_parts(transaction_id)
                .expect("failed_pending_record_name accepted a noncanonical transaction id");
            transactions
                .entry((epoch_nanos, process_id))
                .or_insert_with(|| (transaction_id.to_string(), Vec::new()))
                .1
                .push(held);
        }
    }
    let remove = transactions.len().saturating_sub(keep_transactions);
    for (_, (_, files)) in transactions.into_iter().take(remove) {
        for held in files {
            mark_windows_handle_for_deletion(&held.file)?;
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema: u32,
    product: String,
    target: String,
    version: String,
    files: Vec<PackageFile>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PackageFile {
    path: String,
    size: u64,
    sha256: String,
    /// Portable Unix permission bits. Windows packages use `null`.
    mode: Option<u32>,
}

#[cfg(any(windows, target_os = "linux"))]
struct VerifiedPackageFile {
    relative: PathBuf,
    bytes: Vec<u8>,
    mode: Option<u32>,
}

#[cfg(any(windows, target_os = "linux"))]
struct VerifiedPackage {
    files: Vec<VerifiedPackageFile>,
    #[cfg(windows)]
    package_manifest: Vec<u8>,
}

#[cfg(any(windows, target_os = "linux"))]
impl VerifiedPackage {
    fn from_files(
        files: Vec<VerifiedPackageFile>,
        update: &AvailableUpdate,
        expected_package_manifest: Option<&[u8]>,
    ) -> Result<Self, UpdateError> {
        if files.is_empty() || files.len() > MAX_ARCHIVE_ENTRIES {
            return Err(UpdateError::UnsafeArchive(
                "release package has an invalid file count".into(),
            ));
        }
        let package_manifest = files
            .iter()
            .find(|file| file.relative == Path::new(PACKAGE_MANIFEST_FILE))
            .map(|file| file.bytes.clone())
            .ok_or_else(|| UpdateError::MissingArchiveFile(PACKAGE_MANIFEST_FILE.into()))?;
        if package_manifest.is_empty() || package_manifest.len() > MAX_PACKAGE_MANIFEST_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "package manifest is empty or exceeds its safety limit".into(),
            ));
        }
        if expected_package_manifest.is_some_and(|expected| expected != package_manifest) {
            return Err(UpdateError::UnsafeArchive(
                "package manifest does not match the authenticated pending capsule".into(),
            ));
        }
        let manifest: PackageManifest = serde_json::from_slice(&package_manifest)?;
        if manifest.schema != 1
            || manifest.product != "kettle"
            || current_target() != Some(manifest.target.as_str())
            || manifest.version != update.version.to_string()
            || manifest.files.is_empty()
            || manifest.files.len() >= MAX_ARCHIVE_ENTRIES
        {
            return Err(UpdateError::UnsafeArchive(
                "package manifest identity failed validation".into(),
            ));
        }

        let mut declared = std::collections::HashMap::new();
        for file in manifest.files {
            let path = Path::new(&file.path);
            validate_archive_path(path)?;
            let spelling = file.path.clone();
            let folded = spelling.to_ascii_lowercase();
            if folded == PACKAGE_MANIFEST_FILE.to_ascii_lowercase()
                || !is_sha256(&file.sha256)
                || declared.insert(folded, (spelling, file)).is_some()
            {
                return Err(UpdateError::UnsafeArchive(
                    "package manifest contains an invalid, duplicate, or self entry".into(),
                ));
            }
        }

        let mut actual_count = 0_usize;
        let mut actual_total = 0_u64;
        let mut actual_paths = std::collections::HashSet::new();
        for actual in &files {
            validate_archive_path(&actual.relative)?;
            let spelling = relative_to_string(&actual.relative)?;
            if !actual_paths.insert(spelling.to_ascii_lowercase()) {
                return Err(UpdateError::UnsafeArchive(format!(
                    "package contains a duplicate or case-aliased file {spelling}"
                )));
            }
            if actual.relative == Path::new(PACKAGE_MANIFEST_FILE) {
                continue;
            }
            let (declared_spelling, expected) = declared
                .remove(&spelling.to_ascii_lowercase())
                .ok_or_else(|| {
                    UpdateError::UnsafeArchive(format!(
                        "package contains undeclared file {spelling}"
                    ))
                })?;
            if declared_spelling != spelling
                || actual.bytes.len() as u64 != expected.size
                || sha256_bytes(&actual.bytes) != expected.sha256
                || actual.mode != expected.mode
            {
                return Err(UpdateError::UnsafeArchive(format!(
                    "package manifest mismatch for {spelling}"
                )));
            }
            actual_total = actual_total
                .checked_add(expected.size)
                .ok_or_else(|| UpdateError::UnsafeArchive("package size overflow".into()))?;
            if actual_total > MAX_UNPACKED_BYTES {
                return Err(UpdateError::UnsafeArchive(
                    "package contents exceed the safety limit".into(),
                ));
            }
            actual_count += 1;
        }
        if !declared.is_empty() || actual_count == 0 {
            return Err(UpdateError::UnsafeArchive(
                "package is missing files declared by its manifest".into(),
            ));
        }
        Ok(Self {
            files,
            #[cfg(windows)]
            package_manifest,
        })
    }

    fn file(&self, relative: &Path) -> Result<&VerifiedPackageFile, UpdateError> {
        self.files
            .iter()
            .find(|file| file.relative == relative)
            .ok_or_else(|| UpdateError::MissingArchiveFile(relative.to_string_lossy().into_owned()))
    }

    fn bytes(&self, relative: &Path) -> Result<&[u8], UpdateError> {
        self.file(relative).map(|file| file.bytes.as_slice())
    }
}

/// The helper invocation is deliberately outside clap so it remains hidden
/// from user-facing help and accepts no caller-controlled paths.
pub fn is_pending_update_helper_invocation() -> bool {
    #[cfg(windows)]
    {
        let mut args = std::env::args_os().skip(1);
        args.next().as_deref() == Some(std::ffi::OsStr::new("--kettle-apply-pending-update"))
            && args.next().is_none()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Reconcile a staged Windows update before normal startup and acquire the
/// shared run lock for managed stable installs.
pub fn prepare_process_start() -> Result<ProcessStart, UpdateError> {
    #[cfg(windows)]
    {
        let install = match detect_managed_install() {
            Ok(install) => install,
            Err(_) => {
                return Ok(ProcessStart::Ready {
                    guard: RunningInstallGuard { _lock: None },
                    warning: None,
                });
            }
        };
        if let Some(_update_lock) = kettle_state::ExclusiveFileLock::try_acquire(
            &install.prefix.join(".kettle-update.lock"),
        )? {
            let running_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .expect("the crate package version is valid semver");
            confirm_committed_transaction(&install.prefix, &running_version)?;
        }
        let pending_path = install.prefix.join(PENDING_FILE);
        let mut warning = None;
        if let Some(inspection) = inspect_pending_start(&install.prefix) {
            match inspection {
                // The second inspection below is performed while this process
                // holds the shared run lock. Starting the helper here would
                // leave a window where it could replace the mapped executable.
                PendingStartInspection::Retry { .. } => {}
                PendingStartInspection::Failed {
                    fingerprint,
                    reason,
                } => {
                    warning = Some(quarantine_pending_warning(
                        &install.prefix,
                        &fingerprint,
                        reason,
                    ));
                }
            }
        }
        if !pending_path.is_file()
            && let Err(error) = cleanup_stale_windows_update_files_if_idle(&install.prefix)
        {
            log::warn!("could not clean stale Windows update artifacts: {error}");
        }

        let running_lock_path = install.prefix.join(RUNNING_LOCK_FILE);
        let mut running_lock = kettle_state::SharedFileLock::acquire(&running_lock_path)?;
        // Close the check/lock race: an updater can publish pending state after
        // our first check but before this shared lock is acquired.
        if let Some(inspection) = inspect_pending_start(&install.prefix) {
            match inspection {
                PendingStartInspection::Retry { fingerprint } => {
                    if let Err(error) = spawn_pending_helper(&install.prefix) {
                        drop(running_lock);
                        warning = Some(quarantine_pending_warning(
                            &install.prefix,
                            &fingerprint,
                            format!("the update helper could not start: {error}"),
                        ));
                        running_lock = kettle_state::SharedFileLock::acquire(&running_lock_path)?;
                    } else {
                        return Ok(ProcessStart::PendingUpdate {
                            guard: RunningInstallGuard {
                                _lock: Some(running_lock),
                            },
                        });
                    }
                }
                PendingStartInspection::Failed { reason, .. } => {
                    warning.get_or_insert_with(|| pending_start_warning(&reason, None));
                }
            }
        }
        Ok(ProcessStart::Ready {
            guard: RunningInstallGuard {
                _lock: Some(running_lock),
            },
            warning,
        })
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(executable) = std::env::current_exe() {
            let running_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .expect("the crate package version is valid semver");
            prepare_linux_process_start_at(&executable, &running_version)?;
        }
        Ok(ProcessStart::Ready {
            guard: RunningInstallGuard {},
            warning: None,
        })
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(executable) = std::env::current_exe() {
            crate::macos::sweep_leftovers_beside(&executable);
        }
        Ok(ProcessStart::Ready {
            guard: RunningInstallGuard {},
            warning: None,
        })
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Ok(ProcessStart::Ready {
            guard: RunningInstallGuard {},
            warning: None,
        })
    }
}

#[cfg(target_os = "linux")]
fn prepare_linux_process_start_at(
    executable: &Path,
    running_version: &semver::Version,
) -> Result<(), UpdateError> {
    let install = match locate_managed_install_at(executable) {
        Ok(install) => install,
        Err(_) => return Ok(()),
    };
    let Some(_update_lock) =
        kettle_state::ExclusiveFileLock::try_acquire(&install.prefix.join(".kettle-update.lock"))?
    else {
        return Ok(());
    };
    confirm_committed_transaction(&install.prefix, running_version)?;
    recover_transaction(&install.prefix)?;
    // Startup remains available for an installation whose ordinary provenance
    // is invalid, but recovery must run before that content check can classify
    // an interrupted update as unmanaged.
    let _ = read_linux_install_provenance(&install.prefix);
    Ok(())
}

/// Apply the fixed pending-update record beside this helper executable.
pub fn run_pending_update_helper() -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        let executable = std::env::current_exe()?.canonicalize()?;
        let prefix = executable
            .parent()
            .ok_or_else(|| {
                UpdateError::Transaction("update helper has no parent directory".into())
            })?
            .to_path_buf();
        let name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !name.starts_with(".kettle-update-helper-") || !name.ends_with(".exe") {
            return Err(UpdateError::Transaction(
                "pending-update mode may only run from a staged Kettle helper".into(),
            ));
        }
        run_pending_update_helper_inner(&prefix, &executable)
    }
    #[cfg(not(windows))]
    {
        Err(UpdateError::UnsupportedPlatform)
    }
}

/// Is this a version string a kettle installer would have written?
///
/// A real semver, or the literal `unknown` that `scripts/install-unix.py` and
/// `scripts/install.ps1` both write when they cannot determine one. Anything
/// else did not come from an installer of ours.
///
/// Gated to match its only caller, `detect_managed_install_at`. macOS proves
/// ownership from the bundle signature instead of a marker file, so it never
/// reads a recorded version and `-D warnings` would refuse this as dead code
/// there. Neither a Windows nor a Linux check can see that.
#[cfg(any(windows, target_os = "linux"))]
fn is_recorded_install_version(version: &str) -> bool {
    version == "unknown" || semver::Version::parse(version).is_ok()
}

pub fn marker_json(version: &str) -> Result<String, UpdateError> {
    let target = current_target().ok_or(UpdateError::UnsupportedPlatform)?;
    let marker = InstallMarker {
        schema: MARKER_SCHEMA,
        product: "kettle".to_string(),
        managed_by: "kettle-installer".to_string(),
        channel: "stable".to_string(),
        target: target.to_string(),
        version: version.to_string(),
    };
    Ok(serde_json::to_string_pretty(&marker)? + "\n")
}

#[cfg(target_os = "linux")]
fn open_trusted_linux_install_prefix(prefix: &Path) -> Result<File, UpdateError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if !prefix.is_absolute() || prefix == Path::new("/") {
        return Err(UpdateError::UnmanagedInstall(
            "Linux install prefix must be an absolute non-root path".into(),
        ));
    }
    let effective_uid = unsafe { libc::geteuid() };
    let components = prefix.components().collect::<Vec<_>>();
    let mut directory = open_anchored_directory(Path::new("/")).map_err(|error| {
        UpdateError::UnmanagedInstall(format!("cannot anchor Linux filesystem root: {error}"))
    })?;
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let candidate = directory_descriptor_path(&directory).join(name);
                let next = open_anchored_directory(&candidate).map_err(|error| {
                    UpdateError::UnmanagedInstall(format!(
                        "cannot safely open Linux install path component {}: {error}",
                        prefix.display()
                    ))
                })?;
                let metadata = next.metadata()?;
                let mode = metadata.permissions().mode() & 0o7777;
                let final_component = index == components.len() - 1;
                let trusted_sticky_ancestor =
                    !final_component && metadata.uid() == 0 && mode & libc::S_ISVTX != 0;
                if metadata.uid() != 0 && metadata.uid() != effective_uid {
                    return Err(UpdateError::UnmanagedInstall(format!(
                        "Linux install path component has an untrusted owner: {}",
                        prefix.display()
                    )));
                }
                if mode & 0o022 != 0 && !trusted_sticky_ancestor {
                    return Err(UpdateError::UnmanagedInstall(format!(
                        "Linux install path component is group/other writable: {}",
                        prefix.display()
                    )));
                }
                directory = next;
            }
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(UpdateError::UnmanagedInstall(format!(
                    "Linux install prefix is not absolute and normalized: {}",
                    prefix.display()
                )));
            }
        }
    }
    Ok(directory)
}

#[cfg(target_os = "linux")]
fn read_linux_install_provenance(prefix: &Path) -> Result<UnixInstallProvenance, UpdateError> {
    use std::collections::HashSet;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let relative = Path::new(UNIX_INSTALL_PROVENANCE_FILE);
    let parent = anchored_parent(prefix, relative, false).map_err(|error| {
        UpdateError::UnmanagedInstall(format!("cannot anchor Linux install provenance: {error}"))
    })?;
    let path = parent.destination(relative).map_err(|error| {
        UpdateError::UnmanagedInstall(format!("cannot resolve Linux install provenance: {error}"))
    })?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        UpdateError::UnmanagedInstall(format!(
            "{} is missing or unreadable ({error}); reinstall with the hardened installer",
            prefix.join(relative).display()
        ))
    })?;
    let prefix_metadata = open_trusted_linux_install_prefix(prefix)
        .and_then(|directory| directory.metadata().map_err(UpdateError::from))
        .map_err(|error| {
            UpdateError::UnmanagedInstall(format!(
                "cannot validate the Linux install prefix owner: {error}"
            ))
        })?;
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.uid() != prefix_metadata.uid()
        || metadata.permissions().mode() & 0o7777 != 0o644
        || metadata.len() == 0
        || metadata.len() > 1024 * 1024
    {
        return Err(UpdateError::UnmanagedInstall(
            "Linux install provenance is not an owned bounded regular file".into(),
        ));
    }
    let bytes = read_bounded_regular(&path, 1024 * 1024).map_err(|error| {
        UpdateError::UnmanagedInstall(format!("cannot read Linux install provenance: {error}"))
    })?;
    let provenance: UnixInstallProvenance = serde_json::from_slice(&bytes).map_err(|error| {
        UpdateError::UnmanagedInstall(format!("invalid Linux install provenance: {error}"))
    })?;
    let expected_prefix = prefix.to_str().ok_or_else(|| {
        UpdateError::UnmanagedInstall("Linux install prefix is not valid UTF-8".into())
    })?;
    if provenance.schema != 1
        || provenance.product != "kettle"
        || provenance.managed_by != "kettle-installer"
        || provenance.prefix != expected_prefix
        || provenance.owner_uid != prefix_metadata.uid()
        || provenance.files.is_empty()
        || provenance.files.len() > MAX_ARCHIVE_ENTRIES
        || provenance.directories.len() > MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::UnmanagedInstall(
            "Linux install provenance does not identify this installation".into(),
        ));
    }

    let mut last_file = None::<&str>;
    let mut seen_files = HashSet::new();
    for record in &provenance.files {
        let relative = Path::new(&record.path);
        if record.path == UNIX_INSTALL_PROVENANCE_FILE
            || validate_relative(relative).is_err()
            || record.size > MAX_UNPACKED_BYTES
            || !is_sha256(&record.sha256)
            || !record
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !matches!(record.mode, 0o644 | 0o755)
            || last_file.is_some_and(|previous| previous >= record.path.as_str())
            || !seen_files.insert(record.path.as_str())
        {
            return Err(UpdateError::UnmanagedInstall(
                "Linux install provenance contains an invalid file record".into(),
            ));
        }
        last_file = Some(record.path.as_str());
        let parent = anchored_parent(prefix, relative, false).map_err(|error| {
            UpdateError::UnmanagedInstall(format!(
                "cannot anchor recorded install file {}: {error}",
                record.path
            ))
        })?;
        let anchored = parent.destination(relative)?;
        // Name the file. A bare `?` here surfaced as `No such file or directory
        // (os error 2)` and nothing else — `UpdateError::Io` is transparent —
        // so `kettle update` told the operator a file was missing without
        // saying which one or what to do. That became reachable the moment
        // provenance started carrying records forward: a file an old release
        // installed and a new one no longer ships is now recorded, so deleting
        // what looks like a leftover breaks every future update with an error
        // that points nowhere.
        let metadata = fs::symlink_metadata(&anchored).map_err(|error| {
            UpdateError::UnmanagedInstall(format!(
                "recorded Linux install file is missing: {} ({error}). Reinstall \
                 kettle to rebuild the installation record.",
                record.path
            ))
        })?;
        if !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.uid() != provenance.owner_uid
            || metadata.permissions().mode() & 0o7777 != record.mode
            || metadata.len() != record.size
            || sha256_file(&anchored)? != record.sha256
        {
            return Err(UpdateError::UnmanagedInstall(format!(
                "recorded Linux install file changed identity or content: {}",
                record.path
            )));
        }
    }
    for required in [
        "bin/kettle",
        "share/applications/kettle.desktop",
        "share/kettle/install.sh",
        "share/kettle/install-unix.py",
        "share/kettle/install.json",
    ] {
        if !seen_files.contains(required) {
            return Err(UpdateError::UnmanagedInstall(format!(
                "Linux install provenance is missing {required}"
            )));
        }
    }

    let mut last_directory = None::<&str>;
    let mut seen_directories = HashSet::new();
    for record in &provenance.directories {
        let relative = Path::new(&record.path);
        if validate_relative(relative).is_err()
            || record.mode != 0o755
            || last_directory.is_some_and(|previous| previous >= record.path.as_str())
            || !seen_directories.insert(record.path.as_str())
        {
            return Err(UpdateError::UnmanagedInstall(
                "Linux install provenance contains an invalid directory record".into(),
            ));
        }
        last_directory = Some(record.path.as_str());
        let probe = relative.join(".kettle-provenance-anchor");
        let directory = anchored_parent(prefix, &probe, false).map_err(|error| {
            UpdateError::UnmanagedInstall(format!(
                "cannot anchor recorded install directory {}: {error}",
                record.path
            ))
        })?;
        let metadata = directory.directory.metadata()?;
        if metadata.uid() != provenance.owner_uid
            || metadata.permissions().mode() & 0o7777 != record.mode
        {
            return Err(UpdateError::UnmanagedInstall(format!(
                "recorded Linux install directory changed identity: {}",
                record.path
            )));
        }
    }
    Ok(provenance)
}

#[cfg(any(windows, target_os = "linux"))]
pub fn detect_managed_install() -> Result<ManagedInstall, UpdateError> {
    let executable = std::env::current_exe()?;
    detect_managed_install_at(&executable)
}

#[cfg(target_os = "macos")]
pub fn detect_managed_install() -> Result<ManagedInstall, UpdateError> {
    let executable = std::env::current_exe()?;
    crate::macos::locate_bundle_install(&executable, &crate::macos::AppleSeal)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn detect_managed_install() -> Result<ManagedInstall, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(any(windows, target_os = "linux"))]
fn detect_managed_install_at(executable: &Path) -> Result<ManagedInstall, UpdateError> {
    let install = locate_managed_install_at(executable)?;
    #[cfg(target_os = "linux")]
    let _ = read_linux_install_provenance(&install.prefix)?;
    Ok(install)
}

#[cfg(any(windows, target_os = "linux"))]
fn locate_managed_install_at(executable: &Path) -> Result<ManagedInstall, UpdateError> {
    let executable = executable.canonicalize().map_err(|e| {
        UpdateError::UnmanagedInstall(format!("cannot resolve {}: {e}", executable.display()))
    })?;
    let target = current_target().ok_or(UpdateError::UnsupportedPlatform)?;

    #[cfg(windows)]
    let (prefix, marker_path) = {
        if executable.file_name().and_then(|n| n.to_str()) != Some("kettle.exe") {
            return Err(UpdateError::UnmanagedInstall(
                "the executable is not named kettle.exe".to_string(),
            ));
        }
        let prefix = executable
            .parent()
            .ok_or_else(|| UpdateError::UnmanagedInstall("missing install directory".into()))?
            .to_path_buf();
        let marker = prefix.join(".kettle-install.json");
        (prefix, marker)
    };

    #[cfg(target_os = "linux")]
    let (prefix, marker_path) = {
        let bin = executable
            .parent()
            .ok_or_else(|| UpdateError::UnmanagedInstall("missing bin directory".into()))?;
        if executable.file_name().and_then(|n| n.to_str()) != Some("kettle")
            || bin.file_name().and_then(|n| n.to_str()) != Some("bin")
        {
            return Err(UpdateError::UnmanagedInstall(
                "expected an installer layout ending in bin/kettle".to_string(),
            ));
        }
        let prefix = bin
            .parent()
            .ok_or_else(|| UpdateError::UnmanagedInstall("missing install prefix".into()))?
            .to_path_buf();
        let marker = prefix.join("share/kettle/install.json");
        (prefix, marker)
    };

    #[cfg(target_os = "linux")]
    let _trusted_prefix = open_trusted_linux_install_prefix(&prefix)?;

    let bytes = read_bounded_regular(&marker_path, 16 * 1024).map_err(|e| {
        UpdateError::UnmanagedInstall(format!(
            "{} is missing or unreadable ({e}); update through the package manager or installer that owns this executable",
            marker_path.display()
        ))
    })?;
    let marker: InstallMarker = serde_json::from_slice(&bytes)
        .map_err(|e| UpdateError::UnmanagedInstall(format!("invalid installer marker: {e}")))?;
    if marker.schema != MARKER_SCHEMA
        || marker.product != "kettle"
        || marker.managed_by != "kettle-installer"
        || marker.target != target
        // Every field of this record was validated except the one a human
        // reads. `install.json` is what support instructions, packaging
        // scripts, and the user themselves consult to answer "what is installed
        // here", so an unchecked string there is a claim kettle makes and never
        // verifies.
        //
        // `unknown` is accepted because the installers write it. Refusing
        // anything but a semver reported those installations as UNMANAGED and
        // broke `kettle update` outright for them — `scripts/install-unix.py`
        // explicitly permits `unknown` when it cannot determine a version, and
        // `scripts/install.ps1` substitutes the same word rather than failing.
        // A validator has to accept what the writers actually write.
        || !is_recorded_install_version(&marker.version)
    {
        return Err(UpdateError::UnmanagedInstall(
            "the installer marker does not match this kettle build".to_string(),
        ));
    }
    // `local-dev-record` is a legacy channel from when recording was a
    // compile-time feature; recording is now a runtime toggle in every build, so
    // installers no longer write it. It is still recognized here so any such
    // marker already on disk keeps refusing self-update (rebuild from source).
    if matches!(marker.channel.as_str(), "local-dev" | "local-dev-record") {
        return Err(UpdateError::UnmanagedInstall(
            "this is a local development install; rebuild and reinstall it from its source checkout"
                .to_string(),
        ));
    }
    if marker.channel != "stable" {
        return Err(UpdateError::UnmanagedInstall(format!(
            "unsupported installer channel {:?}",
            marker.channel
        )));
    }
    Ok(ManagedInstall {
        prefix,
        executable,
        marker_path,
    })
}

#[cfg(any(windows, target_os = "linux"))]
pub fn install_update(
    client: &FeedClient,
    update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    let install = prepare_managed_install_for_update()?;
    install_update_into(client, update, &install)
}

#[cfg(target_os = "macos")]
pub fn install_update(
    client: &FeedClient,
    update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    let install = prepare_managed_install_for_update()?;
    crate::macos::install_bundle_update(client, update, &install, &crate::macos::AppleSeal)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn install_update(
    _client: &FeedClient,
    _update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(any(windows, target_os = "linux"))]
pub fn prepare_managed_install_for_update() -> Result<ManagedInstall, UpdateError> {
    #[cfg(windows)]
    {
        detect_managed_install()
    }
    #[cfg(target_os = "linux")]
    {
        let executable = std::env::current_exe()?;
        let install = locate_managed_install_at(&executable)?;
        let running_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .map_err(|error| UpdateError::InvalidCurrentVersion(error.to_string()))?;
        let _lock = prepare_update_transaction(&install, &running_version)?;
        Ok(install)
    }
}

#[cfg(target_os = "macos")]
pub fn prepare_managed_install_for_update() -> Result<ManagedInstall, UpdateError> {
    // Unlike Windows and Linux there is no install-wide lock to take here: the
    // macOS path locks beside the bundle once it knows which directory it will
    // swap in, and holds it across download, staging, and exchange.
    detect_managed_install()
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn prepare_managed_install_for_update() -> Result<ManagedInstall, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(any(windows, target_os = "linux"))]
fn install_update_into(
    client: &FeedClient,
    update: &AvailableUpdate,
    install: &ManagedInstall,
) -> Result<InstallOutcome, UpdateError> {
    reverify_available_update(update, &UPDATE_PUBLIC_KEY, SystemTime::now())?;
    let running_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| UpdateError::InvalidCurrentVersion(error.to_string()))?;
    require_strict_upgrade(&update.version, &running_version)?;
    let asset = update
        .asset
        .as_ref()
        .ok_or(UpdateError::UnsupportedPlatform)?;
    if current_target() != Some(asset.target.as_str()) {
        return Err(UpdateError::MalformedManifest(
            "selected artifact does not match this platform".to_string(),
        ));
    }
    let _lock = prepare_update_transaction(install, &running_version)?;

    #[cfg(windows)]
    let transaction_id = unique_suffix();
    #[cfg(windows)]
    let (archive_name, mut archive) =
        create_windows_pending_archive(&install.prefix, &transaction_id)?;
    #[cfg(windows)]
    let windows_result = (|| {
        fs4::FileExt::lock(&archive)?;
        client.download_to(update, &mut archive)?;
        archive.flush()?;
        archive.sync_all()?;
        verify_sha256(&mut archive, &asset.sha256)?;
        let package = load_windows_package(&mut archive, update, None)?;
        let package_manifest = String::from_utf8(package.package_manifest)
            .map_err(|error| UpdateError::UnsafeArchive(error.to_string()))?;
        publish_windows_update(
            &transaction_id,
            &archive_name,
            package_manifest,
            install,
            update,
        )
    })();
    #[cfg(windows)]
    {
        if windows_result.is_err() && !install.prefix.join(PENDING_FILE).exists() {
            let _ = mark_windows_handle_for_deletion(&archive);
        }
        windows_result
    }

    #[cfg(target_os = "linux")]
    let archive = {
        let bytes = client.download_bytes(update)?;
        verify_sha256_bytes(&bytes, &asset.sha256)?;
        bytes
    };
    #[cfg(target_os = "linux")]
    let package = load_linux_package(&archive, update)?;

    #[cfg(target_os = "linux")]
    {
        let mut transaction = Transaction::begin(&install.prefix, &update.version.to_string())?;
        let result = apply_verified_linux_update(&mut transaction, &package, install, update);
        match result {
            Ok(()) => transaction.commit()?,
            Err(error) => {
                if let Err(rollback) = transaction.rollback() {
                    return Err(UpdateError::Transaction(format!(
                        "{error}; rollback also failed: {rollback}"
                    )));
                }
                return Err(error);
            }
        }

        refresh_platform_integration(install);
        Ok(InstallOutcome {
            version: update.version.clone(),
            executable: install.executable.clone(),
            disposition: InstallDisposition::Applied,
        })
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn prepare_update_transaction(
    install: &ManagedInstall,
    running_version: &semver::Version,
) -> Result<kettle_state::ExclusiveFileLock, UpdateError> {
    let lock_path = install.prefix.join(".kettle-update.lock");
    let lock = kettle_state::ExclusiveFileLock::try_acquire(&lock_path)?
        .ok_or(UpdateError::UpdateLocked)?;
    confirm_committed_transaction(&install.prefix, running_version)?;
    recover_transaction(&install.prefix)?;
    #[cfg(target_os = "linux")]
    let _ = read_linux_install_provenance(&install.prefix)?;
    Ok(lock)
}

#[cfg(windows)]
fn publish_windows_update(
    transaction_id: &str,
    archive_name: &str,
    package_manifest: String,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    if install.prefix.join(PENDING_FILE).exists() {
        return Err(UpdateError::UpdateLocked);
    }
    let helper_name = format!(".kettle-update-helper-{transaction_id}.exe");
    let helper_path = install.prefix.join(&helper_name);

    let result = (|| {
        copy_file_new_durable(&install.executable, &helper_path)?;
        let (mut helper_file, _) = open_transaction_snapshot(&helper_path)?;
        let helper_size = helper_file.metadata()?.len();
        let helper_sha256 = sha256_open_file(&mut helper_file)?;
        drop(helper_file);
        let signed = update
            .signed_manifest
            .as_ref()
            .ok_or(UpdateError::UnauthenticatedRelease)?;
        let asset = update
            .asset
            .as_ref()
            .ok_or(UpdateError::UnsupportedPlatform)?;
        let pending = PendingUpdate {
            schema: PENDING_SCHEMA,
            product: "kettle".into(),
            target: current_target().unwrap_or_default().into(),
            transaction_id: transaction_id.to_string(),
            target_version: update.version.to_string(),
            archive: archive_name.to_string(),
            archive_size: asset.size,
            archive_sha256: asset.sha256.clone(),
            release_manifest: signed.manifest.clone(),
            release_signature: signed.signature.clone(),
            asset: asset.clone(),
            package_manifest,
            helper: helper_name,
            helper_size,
            helper_sha256,
            attempts: 0,
            handoff_timeouts: 0,
            last_error: None,
        };
        persist_pending(&install.prefix, &pending)?;
        spawn_pending_helper(&install.prefix)
    })();
    if let Err(error) = result {
        // Once the pending record exists, retain every artifact for automatic
        // retry on the next launch. Before publication, failed staging is safe
        // to remove immediately.
        if !install.prefix.join(PENDING_FILE).exists() {
            let _ = fs::remove_file(&helper_path);
        }
        return Err(error);
    }

    Ok(InstallOutcome {
        version: update.version.clone(),
        executable: install.executable.clone(),
        disposition: InstallDisposition::Staged {
            transaction_id: transaction_id.to_string(),
        },
    })
}

#[cfg(all(windows, test))]
fn validate_windows_staging(staging: &Path) -> Result<(), UpdateError> {
    validate_partial_windows_staging(staging)?;
    require_file(&staging.join("kettle.exe"), "kettle.exe")?;
    require_file(&staging.join("kettle.com"), "kettle.com")?;
    require_file(&staging.join("install.ps1"), "install.ps1")?;
    Ok(())
}

#[cfg(all(windows, test))]
fn validate_partial_windows_staging(staging: &Path) -> Result<Vec<PathBuf>, UpdateError> {
    validate_windows_payload_tree(staging)?;
    let files = collect_files(staging)?;
    if files.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::UnsafeArchive(format!(
            "Windows release contains more than {MAX_ARCHIVE_ENTRIES} files"
        )));
    }
    let mut total = 0_u64;
    for source in &files {
        let relative = source.strip_prefix(staging).map_err(|_| {
            UpdateError::UnsafeArchive(format!("escaped staging: {}", source.display()))
        })?;
        validate_archive_path(relative)?;
        if !is_allowed_windows_payload_path(relative) {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected release file {}",
                relative.display()
            )));
        }
        total = total
            .checked_add(source.metadata()?.len())
            .ok_or_else(|| UpdateError::UnsafeArchive("staged size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "staged data exceeds the safety limit".into(),
            ));
        }
    }
    Ok(files)
}

#[cfg(all(windows, test))]
fn validate_windows_payload_tree(staging: &Path) -> Result<(), UpdateError> {
    let root_metadata = fs::symlink_metadata(staging)?;
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if !root_metadata.file_type().is_dir()
            || root_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(UpdateError::UnsafeArchive(format!(
                "staging root is not a real directory: {}",
                staging.display()
            )));
        }
    }
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| UpdateError::UnsafeArchive(entry.path().display().to_string()))?;
        let metadata = fs::symlink_metadata(entry.path())?;
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(UpdateError::UnsafeArchive(
                    entry.path().display().to_string(),
                ));
            }
        }
        if metadata.file_type().is_file() {
            if !is_allowed_windows_payload_path(Path::new(&name)) {
                return Err(UpdateError::UnsafeArchive(format!(
                    "unexpected release file {}",
                    entry.path().display()
                )));
            }
            continue;
        }
        if !metadata.file_type().is_dir() || name != "shell-integration" {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected release directory {}",
                entry.path().display()
            )));
        }
        for shell_entry in fs::read_dir(entry.path())? {
            let shell_entry = shell_entry?;
            let relative = Path::new("shell-integration").join(shell_entry.file_name());
            let metadata = fs::symlink_metadata(shell_entry.path())?;
            {
                use std::os::windows::fs::MetadataExt as _;
                use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(UpdateError::UnsafeArchive(
                        shell_entry.path().display().to_string(),
                    ));
                }
            }
            if !metadata.file_type().is_file() || !is_allowed_windows_payload_path(&relative) {
                return Err(UpdateError::UnsafeArchive(format!(
                    "unexpected release file {}",
                    shell_entry.path().display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_allowed_windows_payload_path(relative: &Path) -> bool {
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    match components.as_slice() {
        [root] => WINDOWS_ALLOWED_ROOTS.contains(root) && *root != "shell-integration",
        ["shell-integration", shell] => {
            matches!(
                *shell,
                "kettle.bash" | "kettle.fish" | "kettle.ps1" | "kettle.zsh"
            )
        }
        _ => false,
    }
}

#[cfg(windows)]
fn create_windows_pending_archive(
    prefix: &Path,
    transaction_id: &str,
) -> Result<(String, File), UpdateError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };

    if !is_transaction_id(transaction_id) {
        return Err(UpdateError::Transaction(
            "refusing to create an archive for an invalid transaction id".into(),
        ));
    }
    let name = format!(".kettle-update-archive-{transaction_id}.zip");
    let relative = Path::new(&name);
    let (_parent, path) = anchored_destination(prefix, relative, false)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .create_new(true);
    let file = options.open(path)?;
    kettle_state::restrict_private_file(&file)?;
    Ok((name, file))
}

#[cfg(windows)]
fn copy_file_new_durable(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    let (mut source, _) = open_transaction_snapshot(source)?;
    let mut destination_file = kettle_state::create_private_file_new(destination)?;
    let result = (|| {
        std::io::copy(&mut source, &mut destination_file)?;
        destination_file.flush()?;
        destination_file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        kettle_state::discard_created_private_file(destination_file, destination);
        return result;
    }
    drop(destination_file);
    if let Some(parent) = destination.parent() {
        sync_parent(parent)?;
    }
    Ok(())
}

#[cfg(windows)]
fn persist_pending(prefix: &Path, pending: &PendingUpdate) -> Result<(), UpdateError> {
    let bytes = serde_json::to_vec_pretty(pending)?;
    if bytes.len() > MAX_PENDING_RECORD_BYTES {
        return Err(UpdateError::Transaction(
            "pending update record exceeds the safety limit".into(),
        ));
    }
    atomic_write(&prefix.join(PENDING_FILE), &bytes, Some(0o600))
}

#[cfg(windows)]
fn load_pending(prefix: &Path) -> Result<PendingUpdate, UpdateError> {
    let path = prefix.join(PENDING_FILE);
    let bytes = read_bounded_regular(&path, MAX_PENDING_RECORD_BYTES)?;
    let pending: PendingUpdate = serde_json::from_slice(&bytes)?;
    validate_pending(prefix, &pending)?;
    Ok(pending)
}

#[cfg(windows)]
fn validate_pending(prefix: &Path, pending: &PendingUpdate) -> Result<(), UpdateError> {
    if pending.schema != PENDING_SCHEMA
        || pending.product != "kettle"
        || current_target() != Some(pending.target.as_str())
        || !is_transaction_id(&pending.transaction_id)
        || semver::Version::parse(&pending.target_version).is_err()
        || pending.archive != format!(".kettle-update-archive-{}.zip", pending.transaction_id)
        || pending.archive_size == 0
        || pending.archive_size > MAX_ARTIFACT_BYTES
        || !is_sha256(&pending.archive_sha256)
        || pending.release_manifest.is_empty()
        || pending.release_manifest.len() > 128 * 1024
        || pending.release_signature.is_empty()
        || pending.release_signature.len() > 1024
        || pending.package_manifest.is_empty()
        || pending.package_manifest.len() > MAX_PACKAGE_MANIFEST_BYTES
        || pending.asset.target != pending.target
        || pending.asset.size != pending.archive_size
        || pending.asset.sha256 != pending.archive_sha256
        || pending.helper != format!(".kettle-update-helper-{}.exe", pending.transaction_id)
        || pending.helper_size == 0
        || pending.helper_size > MAX_UNPACKED_BYTES
        || !is_sha256(&pending.helper_sha256)
    {
        return Err(UpdateError::Transaction(
            "pending update record failed validation".into(),
        ));
    }
    if prefix.join(&pending.archive).parent() != Some(prefix)
        || prefix.join(&pending.helper).parent() != Some(prefix)
        || !prefix.join(&pending.archive).is_file()
        || !prefix.join(&pending.helper).is_file()
    {
        return Err(UpdateError::Transaction(
            "pending update artifacts are missing or outside the install prefix".into(),
        ));
    }
    let package: PackageManifest = serde_json::from_str(&pending.package_manifest)
        .map_err(|error| UpdateError::Transaction(format!("invalid package manifest: {error}")))?;
    if package.schema != 1
        || package.product != "kettle"
        || package.target != pending.target
        || package.version != pending.target_version
        || package.files.is_empty()
        || package.files.len() >= MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::Transaction(
            "pending package manifest identity failed validation".into(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn authenticate_pending_release(
    pending: &PendingUpdate,
    public_key: &[u8; 32],
    now: SystemTime,
) -> Result<AvailableUpdate, UpdateError> {
    let version = semver::Version::parse(&pending.target_version)
        .map_err(|error| UpdateError::Transaction(error.to_string()))?;
    let update = AvailableUpdate {
        version,
        tag: format!("v{}", pending.target_version),
        release_url: String::new(),
        download_url: None,
        asset: Some(pending.asset.clone()),
        signed_manifest: Some(SignedManifest {
            manifest: pending.release_manifest.clone(),
            signature: pending.release_signature.clone(),
        }),
    };
    reverify_available_update(&update, public_key, now)?;
    Ok(update)
}

#[cfg(windows)]
fn authenticate_pending_upgrade(
    pending: &PendingUpdate,
    public_key: &[u8; 32],
    now: SystemTime,
    installed: &semver::Version,
) -> Result<AvailableUpdate, UpdateError> {
    let update = authenticate_pending_release(pending, public_key, now)?;
    require_strict_upgrade(&update.version, installed)?;
    Ok(update)
}

#[cfg(windows)]
fn verify_pending_helper(
    prefix: &Path,
    pending: &PendingUpdate,
) -> Result<VerifiedWindowsHelper, UpdateError> {
    let relative = Path::new(&pending.helper);
    let (parent, path) = anchored_destination(prefix, relative, false)?;
    let (mut file, _) = open_transaction_snapshot(&path)?;
    if file.metadata()?.len() != pending.helper_size
        || sha256_open_file(&mut file)? != pending.helper_sha256
    {
        return Err(UpdateError::Transaction(
            "pending update helper changed after publication".into(),
        ));
    }
    Ok(VerifiedWindowsHelper {
        path,
        _parent: parent,
        _file: file,
    })
}

#[cfg(windows)]
fn verify_pending_archive(
    prefix: &Path,
    pending: &PendingUpdate,
) -> Result<WindowsHeldFile, UpdateError> {
    let mut archive = open_windows_held_file(prefix, Path::new(&pending.archive))?;
    if archive.file.metadata()?.len() != pending.archive_size
        || sha256_open_file(&mut archive.file)? != pending.archive_sha256
    {
        return Err(UpdateError::HashMismatch);
    }
    Ok(archive)
}

#[cfg(windows)]
fn spawn_pending_helper(prefix: &Path) -> Result<(), UpdateError> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let pending = load_pending(prefix)?;
    let installed = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("the crate package version is valid semver");
    authenticate_pending_upgrade(&pending, &UPDATE_PUBLIC_KEY, SystemTime::now(), &installed)?;
    let helper = verify_pending_helper(prefix, &pending)?;
    std::process::Command::new(&helper.path)
        .arg("--kettle-apply-pending-update")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(UpdateError::from)
}

/// Bounds how long the helper waits for the update-transaction and
/// running-instances locks before giving up. A holder that is merely stuck
/// (not crashed) rather than exited normally — the GPU device-loss/TDR hangs
/// this project has seen recur — would otherwise leave the helper blocked on
/// an unbounded `ExclusiveFileLock::acquire` forever, with the staged,
/// already-signature-and-hash-verified update never applied and no
/// diagnostic left for the user.
#[cfg(windows)]
const PENDING_HELPER_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(windows)]
fn run_pending_update_helper_inner(prefix: &Path, helper: &Path) -> Result<(), UpdateError> {
    run_pending_update_helper_inner_with_timeout(prefix, helper, PENDING_HELPER_LOCK_TIMEOUT)
}

/// Core of [`run_pending_update_helper_inner`], parameterized on the lock
/// timeout so tests can exercise the timed-out path in milliseconds instead
/// of waiting out the real [`PENDING_HELPER_LOCK_TIMEOUT`].
#[cfg(windows)]
fn run_pending_update_helper_inner_with_timeout(
    prefix: &Path,
    helper: &Path,
    lock_timeout: std::time::Duration,
) -> Result<(), UpdateError> {
    // Match quarantine's lock order: update transaction first, running images
    // second. This keeps staging cleanup and a newly requested update from
    // racing the helper after it has begun validating pending state.
    let update_lock_path = prefix.join(".kettle-update.lock");
    let update_lock =
        match kettle_state::ExclusiveFileLock::acquire_timeout(&update_lock_path, lock_timeout) {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                // Nothing is held here, so there is nowhere safe to persist a
                // last_error (every pending-file writer in this module serializes
                // on this same lock); surface the failure and leave the pending
                // record for the next launch to retry untouched.
                return Err(UpdateError::Transaction(format!(
                    "timed out after {:?} waiting for the update lock at {}: {error}",
                    lock_timeout,
                    update_lock_path.display()
                )));
            }
            Err(error) => return Err(error.into()),
        };
    let running_lock_path = prefix.join(RUNNING_LOCK_FILE);
    let running_lock = match kettle_state::ExclusiveFileLock::acquire_timeout(
        &running_lock_path,
        lock_timeout,
    ) {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            let timeout_error = UpdateError::Transaction(format!(
                "timed out after {lock_timeout:?} waiting for another Kettle process to release {RUNNING_LOCK_FILE}; it may be stuck"
            ));
            // We still hold the update lock, which every pending-file writer
            // in this module acquires first, so it is safe to record the
            // failure here even without the running lock.
            record_pending_handoff_timeout_before_running_lock(
                &update_lock,
                prefix,
                &timeout_error,
            );
            return Err(timeout_error);
        }
        Err(error) => return Err(error.into()),
    };
    // A second helper may have waited behind the one that completed the update.
    if !prefix.join(PENDING_FILE).is_file() {
        return Ok(());
    }
    let helper_pending = load_pending(prefix)?;
    let verified_helper = verify_pending_helper(prefix, &helper_pending)?;
    let (current_helper_file, _) = open_transaction_snapshot(helper)?;
    if !same_transaction_file_identity(&verified_helper._file, &current_helper_file)? {
        return Err(UpdateError::Transaction(
            "running pending helper does not match the authenticated helper identity".into(),
        ));
    }
    let install = ManagedInstall {
        prefix: prefix.to_path_buf(),
        executable: prefix.join("kettle.exe"),
        marker_path: prefix.join(".kettle-install.json"),
    };
    let result = (|| {
        let pending = begin_pending_attempt(prefix, helper)?;
        wait_for_windows_update_targets(prefix)?;
        let current = load_pending(prefix)?;
        if current.transaction_id != pending.transaction_id
            || prefix.join(&current.helper).canonicalize()? != helper
        {
            return Err(UpdateError::Transaction(
                "pending helper changed while waiting for the run lock".into(),
            ));
        }

        let installed_version = installed_windows_version(&install.executable)?;
        confirm_committed_transaction(prefix, &installed_version)?;
        recover_transaction(prefix)?;
        let backup = prefix.join(format!(".kettle-update-backup-{}", pending.transaction_id));
        if backup.exists() {
            remove_orphan_windows_backup_checked(prefix, &backup)?;
        }
        let update = authenticate_pending_upgrade(
            &pending,
            &UPDATE_PUBLIC_KEY,
            SystemTime::now(),
            &installed_version,
        )?;
        let mut archive = verify_pending_archive(prefix, &pending)?;
        let package = load_windows_package(
            &mut archive.file,
            &update,
            Some(pending.package_manifest.as_bytes()),
        )?;
        let integration_script = package.bytes(Path::new("install.ps1"))?.to_vec();
        let mut transaction = Transaction::begin_with_transaction_id(
            prefix,
            &pending.target_version,
            &pending.transaction_id,
        )?;
        if let Err(error) =
            apply_verified_windows_update(&mut transaction, &package, &install, &update)
        {
            if let Err(rollback) = transaction.rollback() {
                return Err(UpdateError::Transaction(format!(
                    "{error}; rollback also failed: {rollback}"
                )));
            }
            return Err(error);
        }
        transaction.commit()?;

        remove_pending_record_checked(prefix, &pending)?;
        mark_windows_handle_for_deletion(&archive.file)?;
        Ok(integration_script)
    })();
    let integration_script = match result {
        Ok(script) => script,
        Err(error) => {
            record_pending_failure(&running_lock, prefix, &error);
            return Err(error);
        }
    };
    // The saved installer acquires the same update -> running lock pair before
    // inspecting or changing managed state. Release in reverse order only
    // after the commit and pending-record removal are durable, otherwise the
    // synchronous integration refresh deadlocks behind this helper.
    release_windows_update_locks_then(running_lock, update_lock, || {
        refresh_platform_integration(&install, &integration_script);
    });
    Ok(())
}

#[cfg(windows)]
fn release_windows_update_locks_then<T>(
    running_lock: kettle_state::ExclusiveFileLock,
    update_lock: kettle_state::ExclusiveFileLock,
    action: impl FnOnce() -> T,
) -> T {
    drop(running_lock);
    drop(update_lock);
    action()
}

#[cfg(windows)]
fn begin_pending_attempt(prefix: &Path, helper: &Path) -> Result<PendingUpdate, UpdateError> {
    let mut pending = load_pending(prefix)?;
    if prefix.join(&pending.helper).canonicalize()? != helper {
        return Err(UpdateError::Transaction(
            "pending record does not name this update helper".into(),
        ));
    }
    if pending.attempts >= MAX_PENDING_ATTEMPTS {
        return Err(UpdateError::Transaction(format!(
            "pending update reached its {MAX_PENDING_ATTEMPTS}-attempt limit"
        )));
    }
    pending.attempts = pending.attempts.saturating_add(1);
    pending.handoff_timeouts = 0;
    pending.last_error = None;
    persist_pending(prefix, &pending)?;
    Ok(pending)
}

#[cfg(windows)]
fn wait_for_windows_update_targets(prefix: &Path) -> Result<(), UpdateError> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
    let started = std::time::Instant::now();
    loop {
        let mut blocked = None;
        for name in ["kettle.com", "kettle.exe"] {
            let path = prefix.join(name);
            if !path.exists() {
                continue;
            }
            let mut options = OpenOptions::new();
            options.read(true).share_mode(0);
            if let Err(error) = options.open(&path) {
                blocked = Some((path, error));
                break;
            }
        }
        let Some((path, error)) = blocked else {
            return Ok(());
        };
        if started.elapsed() >= WAIT_LIMIT {
            return Err(UpdateError::Transaction(format!(
                "timed out waiting to replace {}: {error}",
                path.display()
            )));
        }
        std::thread::sleep(RETRY_DELAY);
    }
}

#[cfg(windows)]
fn installed_windows_version(path: &Path) -> Result<semver::Version, UpdateError> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
    };

    // This no-write/no-delete-sharing snapshot keeps the path bound to the
    // exact installed image while version.dll opens it for its read-only query.
    let (_installed, _) = open_transaction_snapshot(path)?;
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let mut ignored = 0_u32;
    // SAFETY: `wide` is NUL terminated and `ignored` is writable for the call.
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), &mut ignored) };
    if size == 0 || size > 1024 * 1024 {
        return Err(UpdateError::Transaction(format!(
            "installed executable has no bounded Windows version resource: {}",
            path.display()
        )));
    }
    let mut bytes = vec![0_u8; size as usize];
    // SAFETY: the output buffer has exactly `size` writable bytes.
    if unsafe { GetFileVersionInfoW(wide.as_ptr(), 0, size, bytes.as_mut_ptr().cast()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let root = [b'\\' as u16, 0];
    let mut value = std::ptr::null_mut();
    let mut value_len = 0_u32;
    // SAFETY: version.dll owns pointers into `bytes`, which remains alive;
    // `root` is the documented NUL-terminated root sub-block name.
    if unsafe {
        VerQueryValueW(
            bytes.as_ptr().cast(),
            root.as_ptr(),
            &mut value,
            &mut value_len,
        )
    } == 0
        || value.is_null()
        || value_len < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
    {
        return Err(UpdateError::Transaction(format!(
            "installed executable has no fixed Windows version identity: {}",
            path.display()
        )));
    }
    // SAFETY: VerQueryValueW returned at least one complete fixed-info value.
    let fixed = unsafe { &*value.cast::<VS_FIXEDFILEINFO>() };
    version_from_fixed_file_info(fixed)
}

#[cfg(windows)]
fn version_from_fixed_file_info(
    fixed: &windows_sys::Win32::Storage::FileSystem::VS_FIXEDFILEINFO,
) -> Result<semver::Version, UpdateError> {
    use windows_sys::Win32::Storage::FileSystem::VS_FFI_SIGNATURE;

    if fixed.dwSignature != VS_FFI_SIGNATURE as u32 || fixed.dwProductVersionLS & 0xffff != 0 {
        return Err(UpdateError::Transaction(
            "installed executable has an invalid Kettle version resource".into(),
        ));
    }
    Ok(semver::Version::new(
        u64::from(fixed.dwProductVersionMS >> 16),
        u64::from(fixed.dwProductVersionMS & 0xffff),
        u64::from(fixed.dwProductVersionLS >> 16),
    ))
}

#[cfg(windows)]
fn record_pending_failure(
    _running_lock: &kettle_state::ExclusiveFileLock,
    prefix: &Path,
    error: &UpdateError,
) {
    record_pending_failure_locked(prefix, error);
}

/// Same write as [`record_pending_failure`], for the call site that times out
/// acquiring the running lock itself and so can only prove it holds the
/// update lock. Every pending-file writer in this module (`begin_pending_attempt`,
/// `try_quarantine_pending`, this function's sibling) takes the update lock
/// first, so holding it alone is sufficient to serialize this write against
/// all of them; the running lock only additionally orders this against the
/// live binary swap in `apply_staged_update`, which does not touch the
/// pending record.
#[cfg(windows)]
fn record_pending_handoff_timeout_before_running_lock(
    _update_lock: &kettle_state::ExclusiveFileLock,
    prefix: &Path,
    error: &UpdateError,
) {
    record_pending_handoff_timeout_locked(prefix, error, current_epoch_nanos());
}

#[cfg(windows)]
fn record_pending_handoff_timeout_locked(
    prefix: &Path,
    error: &UpdateError,
    now_epoch_nanos: u128,
) {
    let Ok(mut pending) = load_pending(prefix) else {
        return;
    };
    let transaction_epoch = transaction_id_parts(&pending.transaction_id)
        .map(|(_, epoch_nanos)| epoch_nanos)
        .unwrap_or(now_epoch_nanos);
    if now_epoch_nanos.saturating_sub(transaction_epoch) >= HANDOFF_TIMEOUT_GRACE_NANOS {
        pending.handoff_timeouts = pending.handoff_timeouts.saturating_add(1);
    }
    pending.last_error = Some(error.to_string().chars().take(4096).collect());
    let _ = persist_pending(prefix, &pending);
}

#[cfg(windows)]
fn record_pending_failure_locked(prefix: &Path, error: &UpdateError) {
    let Ok(mut pending) = load_pending(prefix) else {
        return;
    };
    pending.last_error = Some(error.to_string().chars().take(4096).collect());
    let _ = persist_pending(prefix, &pending);
}

#[cfg(windows)]
fn cleanup_stale_windows_update_files_if_idle(prefix: &Path) -> Result<bool, UpdateError> {
    // Staging is created before the pending record is published. Serialize
    // cleanup with that preparation window so a normal launch cannot remove an
    // update another process is still downloading, extracting, or publishing.
    let Some(_lock) =
        kettle_state::ExclusiveFileLock::try_acquire(&prefix.join(".kettle-update.lock"))?
    else {
        return Ok(false);
    };
    let Ok(entries) = fs::read_dir(prefix) else {
        return Ok(true);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let helper_transaction = name
            .strip_prefix(".kettle-update-helper-")
            .and_then(|suffix| suffix.strip_suffix(".exe"));
        let archive_transaction = name
            .strip_prefix(".kettle-update-archive-")
            .and_then(|suffix| suffix.strip_suffix(".zip"));
        let staging_transaction = name.strip_prefix(".kettle-update-stage-");
        if helper_transaction.is_some_and(is_transaction_id)
            || archive_transaction.is_some_and(is_transaction_id)
        {
            if let Ok(held) = open_windows_held_file(prefix, Path::new(&name)) {
                let _ = mark_windows_handle_for_deletion(&held.file);
            }
        } else if staging_transaction.is_some_and(is_transaction_id) {
            let _ = remove_staging_dir_checked(prefix, &entry.path());
        } else if !prefix.join(".kettle-update-journal.json").exists()
            && name
                .strip_prefix(".kettle-update-backup-")
                .is_some_and(is_transaction_id)
        {
            let _ = remove_orphan_windows_backup_checked(prefix, &entry.path());
        }
    }
    prune_failed_pending_records(prefix, MAX_FAILED_PENDING_TRANSACTIONS)?;
    Ok(true)
}

#[cfg(windows)]
fn is_allowed_windows_backup_path(relative: &Path) -> bool {
    relative == Path::new(BACKUP_MARKER_FILE)
        || relative == Path::new(".kettle-install.json")
        || is_allowed_windows_payload_path(relative)
}

#[cfg(windows)]
fn remove_orphan_windows_backup_checked(
    prefix: &Path,
    backup_dir: &Path,
) -> Result<(), UpdateError> {
    let transaction_id = backup_dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(".kettle-update-backup-"))
        .filter(|value| is_transaction_id(value))
        .ok_or_else(|| {
            UpdateError::Transaction(format!(
                "refusing to remove untrusted backup path {}",
                backup_dir.display()
            ))
        })?;
    if backup_dir.parent() != Some(prefix) {
        return Err(UpdateError::Transaction(
            "orphan backup escaped the install prefix".into(),
        ));
    }
    let root_relative = PathBuf::from(
        backup_dir
            .file_name()
            .ok_or_else(|| UpdateError::Transaction("backup path has no name".into()))?,
    );
    let mut tree = hold_windows_two_level_tree(prefix, &root_relative)?;
    let marker_file = tree
        .files
        .iter_mut()
        .find(|(relative, _)| relative == Path::new(BACKUP_MARKER_FILE))
        .ok_or_else(|| UpdateError::Transaction("orphan backup has no marker".into()))?;
    let marker: BackupMarker =
        serde_json::from_slice(&read_windows_held_file(&mut marker_file.1, 4096)?)?;
    if marker.schema != JOURNAL_SCHEMA
        || marker.product != "kettle"
        || marker.transaction_id != transaction_id
    {
        return Err(UpdateError::Transaction(
            "orphan backup marker does not match its directory".into(),
        ));
    }
    for (relative, _) in &tree.files {
        if !is_allowed_windows_backup_path(relative) {
            return Err(UpdateError::Transaction(format!(
                "orphan backup contains an unmanaged file {}",
                relative.display()
            )));
        }
    }
    tree.delete()?;
    sync_parent(prefix)
}

#[cfg(windows)]
fn remove_staging_dir_checked(prefix: &Path, staging: &Path) -> Result<(), UpdateError> {
    let transaction_id = staging
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(".kettle-update-stage-"));
    if staging.parent() != Some(prefix) || !transaction_id.is_some_and(is_transaction_id) {
        return Err(UpdateError::Transaction(format!(
            "refusing to remove untrusted staging path {}",
            staging.display()
        )));
    }
    if !staging.exists() {
        return Ok(());
    }
    let root_relative = PathBuf::from(
        staging
            .file_name()
            .ok_or_else(|| UpdateError::Transaction("staging path has no name".into()))?,
    );
    let tree = hold_windows_two_level_tree(prefix, &root_relative)?;
    for (relative, _) in &tree.files {
        if !is_allowed_windows_payload_path(relative) {
            return Err(UpdateError::Transaction(format!(
                "staging cleanup found an unmanaged file {}",
                relative.display()
            )));
        }
    }
    tree.delete()?;
    sync_parent(prefix)
}

#[cfg(any(windows, target_os = "linux"))]
fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    let mut file = File::open(path)?;
    sha256_open_file(&mut file)
}

#[cfg(any(windows, target_os = "linux"))]
fn sha256_open_file(file: &mut File) -> Result<String, UpdateError> {
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// Hashes the mandatory-locked Windows archive handle from its start. The
/// caller extracts from this same still-locked handle.
#[cfg(windows)]
fn verify_sha256(file: &mut File, expected: &str) -> Result<(), UpdateError> {
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if hex::encode(hash.finalize()) != expected {
        return Err(UpdateError::HashMismatch);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn verify_sha256_bytes(bytes: &[u8], expected: &str) -> Result<(), UpdateError> {
    if hex::encode(Sha256::digest(bytes)) != expected {
        return Err(UpdateError::HashMismatch);
    }
    Ok(())
}

#[cfg(all(any(windows, target_os = "linux"), test))]
fn verify_required_package_manifest(
    root: &Path,
    update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    let manifest_path = root.join(PACKAGE_MANIFEST_FILE);
    if manifest_path.is_file() {
        return verify_package_manifest(root, update);
    }
    if update.version >= semver::Version::new(2, 36, 0) {
        return Err(UpdateError::UnsafeArchive(format!(
            "{PACKAGE_MANIFEST_FILE} is required for release archives from v2.36.0 onward"
        )));
    }
    Ok(())
}

#[cfg(all(any(windows, target_os = "linux"), test))]
fn verify_package_manifest(root: &Path, update: &AvailableUpdate) -> Result<(), UpdateError> {
    let manifest_path = root.join(PACKAGE_MANIFEST_FILE);
    let bytes = read_bounded_regular(&manifest_path, 256 * 1024).map_err(|error| {
        UpdateError::UnsafeArchive(format!("invalid package manifest: {error}"))
    })?;
    if bytes.is_empty() {
        return Err(UpdateError::UnsafeArchive(
            "package manifest is empty".into(),
        ));
    }
    let manifest: PackageManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema != 1
        || manifest.product != "kettle"
        || current_target() != Some(manifest.target.as_str())
        || manifest.version != update.version.to_string()
        || manifest.files.is_empty()
        || manifest.files.len() >= MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::UnsafeArchive(
            "package manifest identity failed validation".into(),
        ));
    }

    let mut declared = std::collections::HashMap::new();
    let mut declared_total = 0_u64;
    for file in manifest.files {
        let path = Path::new(&file.path);
        validate_archive_path(path)?;
        let declared_path = file.path.clone();
        let folded = declared_path.to_lowercase();
        if folded == PACKAGE_MANIFEST_FILE.to_lowercase()
            || declared.insert(folded, (declared_path, file)).is_some()
        {
            return Err(UpdateError::UnsafeArchive(
                "package manifest contains a duplicate or self-entry".into(),
            ));
        }
    }

    let mut actual_count = 0_usize;
    for actual in collect_files(root)? {
        let relative = actual
            .strip_prefix(root)
            .map_err(|_| UpdateError::UnsafeArchive(actual.display().to_string()))?;
        if relative == Path::new(PACKAGE_MANIFEST_FILE) {
            continue;
        }
        let relative_string = relative_to_string(relative)?;
        let (declared_path, expected) = declared
            .remove(&relative_string.to_lowercase())
            .ok_or_else(|| {
                UpdateError::UnsafeArchive(format!(
                    "package contains undeclared file {relative_string}"
                ))
            })?;
        let metadata = fs::symlink_metadata(&actual)?;
        if declared_path != relative_string
            || !metadata.file_type().is_file()
            || metadata.len() != expected.size
            || sha256_file(&actual)? != expected.sha256
            || package_mode(&metadata) != expected.mode
            || !is_sha256(&expected.sha256)
        {
            return Err(UpdateError::UnsafeArchive(format!(
                "package manifest mismatch for {relative_string}"
            )));
        }
        declared_total = declared_total
            .checked_add(expected.size)
            .ok_or_else(|| UpdateError::UnsafeArchive("package size overflow".into()))?;
        if declared_total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "package contents exceed the safety limit".into(),
            ));
        }
        actual_count += 1;
    }
    if !declared.is_empty() || actual_count == 0 {
        return Err(UpdateError::UnsafeArchive(
            "package is missing files declared by its manifest".into(),
        ));
    }
    Ok(())
}

#[cfg(all(any(windows, target_os = "linux"), unix, test))]
fn package_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(all(any(windows, target_os = "linux"), not(unix), test))]
fn package_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(windows)]
fn load_windows_package(
    archive: &mut File,
    update: &AvailableUpdate,
    expected_package_manifest: Option<&[u8]>,
) -> Result<VerifiedPackage, UpdateError> {
    archive.rewind()?;
    let mut zip = zip::ZipArchive::new(&mut *archive)?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::UnsafeArchive("too many entries".into()));
    }
    let mut total = 0_u64;
    let mut seen = ArchivePaths::default();
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| UpdateError::UnsafeArchive(entry.name().to_string()))?
            .to_path_buf();
        validate_archive_path(&enclosed)?;
        if entry.encrypted() {
            return Err(UpdateError::UnsafeArchive(format!(
                "encrypted entry {}",
                enclosed.display()
            )));
        }
        let is_dir = entry.is_dir();
        if !zip_unix_mode_is_safe(entry.unix_mode(), is_dir) {
            return Err(UpdateError::UnsafeArchive(format!(
                "links and special files are forbidden: {}",
                enclosed.display()
            )));
        }
        seen.insert(&enclosed, is_dir)?;
        let declared_size = entry.size();
        let next_total = total
            .checked_add(declared_size)
            .ok_or_else(|| UpdateError::UnsafeArchive("unpacked size overflow".into()))?;
        if next_total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "unpacked data exceeds the safety limit".into(),
            ));
        }
        if is_dir {
            if declared_size != 0 || enclosed != Path::new("shell-integration") {
                return Err(UpdateError::UnsafeArchive(format!(
                    "unexpected release directory {}",
                    enclosed.display()
                )));
            }
            continue;
        }
        if !entry.is_file() || !is_allowed_windows_payload_path(&enclosed) {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected release file {}",
                enclosed.display()
            )));
        }
        let capacity = usize::try_from(declared_size)
            .map_err(|_| UpdateError::UnsafeArchive("entry is too large".into()))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity).map_err(|error| {
            UpdateError::Io(std::io::Error::other(format!(
                "could not reserve {capacity} bytes for {}: {error}",
                enclosed.display()
            )))
        })?;
        let remaining = MAX_UNPACKED_BYTES - total;
        let actual = std::io::Read::by_ref(&mut entry)
            .take(remaining.saturating_add(1))
            .read_to_end(&mut bytes)? as u64;
        if actual != declared_size {
            return Err(UpdateError::UnsafeArchive(format!(
                "entry size mismatch for {} (declared {declared_size}, extracted {actual})",
                enclosed.display()
            )));
        }
        total = next_total;
        files.push(VerifiedPackageFile {
            relative: enclosed,
            bytes,
            mode: None,
        });
    }
    for mandatory in ["kettle.exe", "kettle.com", "install.ps1"] {
        if !files
            .iter()
            .any(|file| file.relative == Path::new(mandatory))
        {
            return Err(UpdateError::MissingArchiveFile(mandatory.into()));
        }
    }
    VerifiedPackage::from_files(files, update, expected_package_manifest)
}

#[cfg(target_os = "linux")]
fn load_linux_package(
    archive: &[u8],
    update: &AvailableUpdate,
) -> Result<VerifiedPackage, UpdateError> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    let mut count = 0_usize;
    let mut total = 0_u64;
    let mut seen = ArchivePaths::default();
    let mut files = Vec::new();
    for entry in tar.entries()? {
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(UpdateError::UnsafeArchive("too many entries".into()));
        }
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        validate_archive_path(&path)?;
        if path.components().next() != Some(Component::Normal("kettle".as_ref())) {
            return Err(UpdateError::UnsafeArchive(format!(
                "entry is outside the kettle root: {}",
                path.display()
            )));
        }
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(UpdateError::UnsafeArchive(format!(
                "links and special files are forbidden: {}",
                path.display()
            )));
        }
        if let Some(extensions) = entry.pax_extensions()? {
            for extension in extensions {
                let extension = extension?;
                let key = extension.key_bytes();
                if key.starts_with(b"GNU.sparse.") || key == b"SCHILY.realsize" {
                    return Err(UpdateError::UnsafeArchive(format!(
                        "sparse files are forbidden: {}",
                        path.display()
                    )));
                }
            }
        }
        seen.insert(&path, entry_type.is_dir())?;
        let mode = entry.header().mode()?;
        if mode & !0o777 != 0 {
            return Err(UpdateError::UnsafeArchive(format!(
                "special permission bits are forbidden: {}",
                path.display()
            )));
        }
        let declared_size = entry.size();
        let next_total = total
            .checked_add(declared_size)
            .ok_or_else(|| UpdateError::UnsafeArchive("unpacked size overflow".into()))?;
        if next_total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "unpacked data exceeds the safety limit".into(),
            ));
        }
        if entry_type.is_dir() {
            if declared_size != 0 {
                return Err(UpdateError::UnsafeArchive(format!(
                    "directory has data: {}",
                    path.display()
                )));
            }
            continue;
        }
        let relative = path.strip_prefix("kettle").map_err(|_| {
            UpdateError::UnsafeArchive(format!("invalid release root: {}", path.display()))
        })?;
        if relative.as_os_str().is_empty() {
            return Err(UpdateError::UnsafeArchive(
                "the kettle archive root cannot be a file".into(),
            ));
        }
        let capacity = usize::try_from(declared_size)
            .map_err(|_| UpdateError::UnsafeArchive("entry is too large".into()))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity).map_err(|error| {
            UpdateError::Io(std::io::Error::other(format!(
                "could not reserve {capacity} bytes for {}: {error}",
                path.display()
            )))
        })?;
        let remaining = MAX_UNPACKED_BYTES - total;
        let actual = std::io::Read::by_ref(&mut entry)
            .take(remaining.saturating_add(1))
            .read_to_end(&mut bytes)? as u64;
        if actual != declared_size {
            return Err(UpdateError::UnsafeArchive(format!(
                "entry size mismatch for {} (declared {declared_size}, extracted {actual})",
                path.display()
            )));
        }
        total = next_total;
        files.push(VerifiedPackageFile {
            relative: relative.to_path_buf(),
            bytes,
            mode: Some(mode),
        });
    }
    VerifiedPackage::from_files(files, update, None)
}

#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn zip_unix_mode_is_safe(mode: Option<u32>, is_dir: bool) -> bool {
    let Some(mode) = mode else {
        return true;
    };
    match mode & 0o170000 {
        // Some ZIP creators store only permission bits, without a file type.
        0 => true,
        0o040000 => is_dir,
        0o100000 => !is_dir,
        _ => false,
    }
}

/// Extracts from the same bounded in-memory bytes hashed by
/// [`verify_sha256_bytes`]. No writable archive inode exists between
/// verification and extraction.
#[cfg(all(target_os = "linux", test))]
fn extract_archive(archive: &[u8], destination: &Path) -> Result<(), UpdateError> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    let mut count = 0_usize;
    let mut total = 0_u64;
    let mut seen = ArchivePaths::default();
    for entry in tar.entries()? {
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(UpdateError::UnsafeArchive("too many entries".into()));
        }
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        validate_archive_path(&path)?;
        if path.components().next() != Some(Component::Normal("kettle".as_ref())) {
            return Err(UpdateError::UnsafeArchive(format!(
                "entry is outside the kettle root: {}",
                path.display()
            )));
        }
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(UpdateError::UnsafeArchive(format!(
                "links and special files are forbidden: {}",
                path.display()
            )));
        }
        if let Some(extensions) = entry.pax_extensions()? {
            for extension in extensions {
                let extension = extension?;
                let key = extension.key_bytes();
                if key.starts_with(b"GNU.sparse.") || key == b"SCHILY.realsize" {
                    return Err(UpdateError::UnsafeArchive(format!(
                        "sparse files are forbidden: {}",
                        path.display()
                    )));
                }
            }
        }
        seen.insert(&path, entry_type.is_dir())?;
        let mode = entry.header().mode()?;
        if mode & !0o777 != 0 {
            return Err(UpdateError::UnsafeArchive(format!(
                "special permission bits are forbidden: {}",
                path.display()
            )));
        }
        let declared_size = entry.size();
        let next_total = total
            .checked_add(declared_size)
            .ok_or_else(|| UpdateError::UnsafeArchive("unpacked size overflow".into()))?;
        if next_total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "unpacked data exceeds the safety limit".into(),
            ));
        }
        let output = destination.join(&path);
        if entry_type.is_dir() {
            if declared_size != 0 {
                return Err(UpdateError::UnsafeArchive(format!(
                    "directory has data: {}",
                    path.display()
                )));
            }
            fs::create_dir_all(output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?;
        let remaining = MAX_UNPACKED_BYTES - total;
        let actual = std::io::copy(&mut entry.by_ref().take(remaining + 1), &mut file)?;
        if actual != declared_size {
            return Err(UpdateError::UnsafeArchive(format!(
                "entry size mismatch for {} (declared {declared_size}, extracted {actual})",
                path.display()
            )));
        }
        total = next_total;
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        file.sync_all()?;
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) fn validate_archive_path(path: &Path) -> Result<(), UpdateError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(UpdateError::UnsafeArchive(path.display().to_string()));
    }
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(UpdateError::UnsafeArchive(path.display().to_string()));
        };
        let name = name
            .to_str()
            .ok_or_else(|| UpdateError::UnsafeArchive(path.display().to_string()))?;
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.ends_with(['.', ' '])
            || name.contains(':')
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || is_windows_device_name(name)
        {
            return Err(UpdateError::UnsafeArchive(path.display().to_string()));
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct ArchivePaths {
    /// Case-folded portable path -> whether the entry is a directory.
    entries: std::collections::HashMap<String, bool>,
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
impl ArchivePaths {
    pub(crate) fn insert(&mut self, path: &Path, is_dir: bool) -> Result<(), UpdateError> {
        let key = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
            .to_ascii_lowercase();
        if self.entries.contains_key(&key) {
            return Err(UpdateError::UnsafeArchive(format!(
                "duplicate or case-aliased path {}",
                path.display()
            )));
        }
        let parts = key.split('/').collect::<Vec<_>>();
        for index in 1..parts.len() {
            let ancestor = parts[..index].join("/");
            if self.entries.get(&ancestor) == Some(&false) {
                return Err(UpdateError::UnsafeArchive(format!(
                    "file/directory prefix conflict at {}",
                    path.display()
                )));
            }
        }
        if !is_dir {
            let child_prefix = format!("{key}/");
            if self
                .entries
                .keys()
                .any(|existing| existing.starts_with(&child_prefix))
            {
                return Err(UpdateError::UnsafeArchive(format!(
                    "file/directory prefix conflict at {}",
                    path.display()
                )));
            }
        }
        self.entries.insert(key, is_dir);
        Ok(())
    }
}

#[cfg(windows)]
fn apply_verified_windows_update(
    transaction: &mut Transaction,
    package: &VerifiedPackage,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    for mandatory in ["kettle.exe", "kettle.com", "install.ps1"] {
        if !package
            .files
            .iter()
            .any(|file| file.relative == Path::new(mandatory))
        {
            return Err(UpdateError::MissingArchiveFile(mandatory.into()));
        }
    }
    let mut destinations = package
        .files
        .iter()
        .filter(|file| file.relative != Path::new("kettle.exe"))
        .map(|file| file.relative.clone())
        .collect::<Vec<_>>();
    destinations.push(PathBuf::from("kettle.exe"));
    destinations.push(PathBuf::from(".kettle-install.json"));
    transaction.preflight_destinations(&destinations)?;

    let support_files = package
        .files
        .iter()
        .filter(|file| file.relative != Path::new("kettle.exe"))
        .map(|file| file.relative.clone())
        .collect::<Vec<_>>();
    for relative in support_files {
        let bytes = package.bytes(&relative)?;
        transaction.install_bytes(&relative, bytes, None)?;
    }
    let binary = package.bytes(Path::new("kettle.exe"))?;
    transaction.install_bytes(Path::new("kettle.exe"), binary, None)?;
    let marker = marker_json(&update.version.to_string())?;
    transaction.install_bytes(Path::new(".kettle-install.json"), marker.as_bytes(), None)?;
    transaction.finish_preflight()?;
    debug_assert_eq!(install.executable, install.prefix.join("kettle.exe"));
    Ok(())
}

#[cfg(all(windows, test))]
fn apply_staged_update(
    transaction: &mut Transaction,
    staging: &Path,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    let binary = staging.join("kettle.exe");
    require_file(&binary, "kettle.exe")?;
    require_file(&staging.join("kettle.com"), "kettle.com")?;
    require_file(&staging.join("install.ps1"), "install.ps1")?;

    let files = collect_files(staging)?;
    let mut destinations = files
        .iter()
        .filter(|path| *path != &binary)
        .map(|source| {
            source
                .strip_prefix(staging)
                .map(Path::to_path_buf)
                .map_err(|_| UpdateError::UnsafeArchive(source.display().to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    destinations.push(PathBuf::from("kettle.exe"));
    destinations.push(PathBuf::from(".kettle-install.json"));
    transaction.preflight_destinations(&destinations)?;
    for source in files.iter().filter(|path| *path != &binary) {
        let relative = source.strip_prefix(staging).map_err(|_| {
            UpdateError::UnsafeArchive(format!("escaped staging: {}", source.display()))
        })?;
        if !is_allowed_windows_payload_path(relative) {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected release file {}",
                relative.display()
            )));
        }
        transaction.install(relative, source, None)?;
    }
    transaction.install(Path::new("kettle.exe"), &binary, None)?;
    let marker = marker_json(&update.version.to_string())?;
    transaction.install_bytes(Path::new(".kettle-install.json"), marker.as_bytes(), None)?;
    transaction.finish_preflight()?;
    debug_assert_eq!(install.executable, install.prefix.join("kettle.exe"));
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_verified_linux_update(
    transaction: &mut Transaction,
    package: &VerifiedPackage,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    package.file(Path::new("kettle"))?;
    package.file(Path::new("install.sh"))?;
    // Provenance verification REQUIRES this file to be recorded, and the update
    // replaces it like everything else, so it has to be installed here too. It
    // was missing from the production map while the `cfg(test)` duplicate below
    // carried it -- which is exactly why the tests stayed green.
    package.file(Path::new("install-unix.py"))?;
    let previous_provenance = read_linux_install_provenance(&install.prefix)?;
    let map = [
        ("install.sh", "share/kettle/install.sh", 0o755),
        ("install-unix.py", "share/kettle/install-unix.py", 0o755),
        ("LICENSE", "share/doc/kettle/LICENSE", 0o644),
        ("NOTICE", "share/doc/kettle/NOTICE", 0o644),
        ("README.md", "share/doc/kettle/README.md", 0o644),
        ("CHANGELOG.md", "share/doc/kettle/CHANGELOG.md", 0o644),
        (
            "packaging/linux/kettle.desktop",
            "share/applications/kettle.desktop",
            0o644,
        ),
        (
            "packaging/linux/kettle.svg",
            "share/icons/hicolor/scalable/apps/kettle.svg",
            0o644,
        ),
        (
            "packaging/linux/kettle-16.png",
            "share/icons/hicolor/16x16/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-24.png",
            "share/icons/hicolor/24x24/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-32.png",
            "share/icons/hicolor/32x32/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-48.png",
            "share/icons/hicolor/48x48/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-64.png",
            "share/icons/hicolor/64x64/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-128.png",
            "share/icons/hicolor/128x128/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-256.png",
            "share/icons/hicolor/256x256/apps/kettle.png",
            0o644,
        ),
        ("packaging/linux/kettle.1", "share/man/man1/kettle.1", 0o644),
    ];
    let shell_files = package
        .files
        .iter()
        .filter(|file| file.relative.starts_with("shell-integration"))
        .collect::<Vec<_>>();
    let mut destinations = map
        .iter()
        .map(|(_, destination, _)| PathBuf::from(destination))
        .collect::<Vec<_>>();
    for source in &shell_files {
        let relative = source
            .relative
            .strip_prefix("shell-integration")
            .map_err(|_| UpdateError::UnsafeArchive(source.relative.display().to_string()))?;
        validate_archive_path(relative)?;
        destinations.push(Path::new("share/kettle/shell-integration").join(relative));
    }
    destinations.push(PathBuf::from("bin/kettle"));
    destinations.push(PathBuf::from("share/kettle/install.json"));
    destinations.push(PathBuf::from(UNIX_INSTALL_PROVENANCE_FILE));

    transaction.preflight_destinations(&destinations)?;

    // Every file this transaction writes must appear in the NEW provenance
    // record. Not regenerating it left the OLD hashes describing the NEW files,
    // so the very next verification reported the installation unmanaged:
    // startup could not confirm or clean the committed transaction, and every
    // later self-update refused to run. `install_unix_provenance` below merges
    // these with what the previous release already owned.
    let mut provenance_files = Vec::with_capacity(destinations.len());
    for (source, destination, mode) in map {
        let bytes = package.bytes(Path::new(source))?;
        if destination.ends_with("kettle.desktop") {
            let desktop = render_linux_desktop_bytes(bytes, &install.prefix)?;
            transaction.install_bytes(Path::new(destination), desktop.as_bytes(), Some(mode))?;
            provenance_files.push(UnixInstallFile {
                path: destination.to_string(),
                size: desktop.len() as u64,
                sha256: sha256_bytes(desktop.as_bytes()),
                mode,
            });
        } else {
            transaction.install_bytes(Path::new(destination), bytes, Some(mode))?;
            provenance_files.push(UnixInstallFile {
                path: destination.to_string(),
                size: bytes.len() as u64,
                sha256: sha256_bytes(bytes),
                mode,
            });
        }
    }
    for source in shell_files {
        let relative = source
            .relative
            .strip_prefix("shell-integration")
            .map_err(|_| UpdateError::UnsafeArchive(source.relative.display().to_string()))?;
        let destination = Path::new("share/kettle/shell-integration").join(relative);
        transaction.install_bytes(&destination, &source.bytes, Some(0o644))?;
        provenance_files.push(UnixInstallFile {
            path: relative_to_string(&destination)?,
            size: source.bytes.len() as u64,
            sha256: sha256_bytes(&source.bytes),
            mode: 0o644,
        });
    }
    let binary = package.bytes(Path::new("kettle"))?;
    transaction.install_bytes(Path::new("bin/kettle"), binary, Some(0o755))?;
    provenance_files.push(UnixInstallFile {
        path: "bin/kettle".into(),
        size: binary.len() as u64,
        sha256: sha256_bytes(binary),
        mode: 0o755,
    });
    let marker = marker_json(&update.version.to_string())?;
    transaction.install_bytes(
        Path::new("share/kettle/install.json"),
        marker.as_bytes(),
        Some(0o644),
    )?;
    provenance_files.push(UnixInstallFile {
        path: "share/kettle/install.json".into(),
        size: marker.len() as u64,
        sha256: sha256_bytes(marker.as_bytes()),
        mode: 0o644,
    });

    install_unix_provenance(transaction, install, previous_provenance, provenance_files)?;
    transaction.finish_preflight()?;
    Ok(())
}

/// Write the Linux install provenance for a transaction that has just published
/// its files, as its final journaled entry.
///
/// Both Linux appliers end here so the record they produce cannot drift. That
/// mattered: the two used to build it separately, and when the production one
/// stopped installing `install-unix.py` the duplicate below kept recording it,
/// so every test stayed green while real installs were rejected as unmanaged.
///
/// `published` is what THIS transaction wrote; `previous` is the record it
/// replaces. The two are merged rather than the new one replacing the old,
/// because a file an earlier release installed and this archive no longer ships
/// is still on disk — dropping its record leaves it unremovable, since uninstall
/// deletes only what provenance lists. `install-unix.py` seeds from the old
/// record for the same reason, and the two writers have to agree.
#[cfg(target_os = "linux")]
fn install_unix_provenance(
    transaction: &mut Transaction,
    install: &ManagedInstall,
    previous: UnixInstallProvenance,
    published: Vec<UnixInstallFile>,
) -> Result<(), UpdateError> {
    use std::collections::BTreeMap;

    // `BTreeMap` gives the strict path ordering the reader requires, and lets a
    // republished path replace its old record rather than duplicating it.
    let mut files = previous
        .files
        .into_iter()
        .map(|record| (record.path.clone(), record))
        .collect::<BTreeMap<_, _>>();
    for file in published {
        files.insert(file.path.clone(), file);
    }

    // Carry the previous record's directories forward and add the ones this
    // transaction actually created. Sampling `try_exists` before the writes
    // instead asked the wrong question: a transaction that created a directory
    // and then rolled back left it on disk unrecorded, so the retry saw it as
    // pre-existing, omitted it, and uninstall left it behind for good. The
    // transaction now reports what it created and removes those directories
    // when it rolls back, so both answers come from the same authority.
    //
    // This is called after every other publication and installs the record as
    // the transaction's last entry, so `created_directories` is complete: the
    // provenance file's own parent is created by an earlier entry.
    let mut directories = previous
        .directories
        .into_iter()
        .map(|record| (record.path, record.mode))
        .collect::<BTreeMap<_, _>>();
    directories.extend(
        transaction
            .created_directories()
            .iter()
            .map(|(path, mode)| (path.clone(), *mode)),
    );

    // Enforce the reader's bounds BEFORE writing: `read_linux_install_provenance`
    // refuses a record past `MAX_ARCHIVE_ENTRIES`, so exceeding it here would
    // produce provenance this installer can never read back — and since both
    // upgrade and uninstall need a readable record, the install would be
    // stranded by its own success.
    if files.len() > MAX_ARCHIVE_ENTRIES || directories.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::UnmanagedInstall(format!(
            "refusing to record Linux install provenance with {} files and {} \
             directories; the readable maximum is {MAX_ARCHIVE_ENTRIES} each",
            files.len(),
            directories.len()
        )));
    }
    let prefix = install.prefix.to_str().ok_or_else(|| {
        UpdateError::Transaction("Linux install prefix is not valid UTF-8".into())
    })?;
    let provenance = UnixInstallProvenance {
        schema: 1,
        product: "kettle".into(),
        managed_by: "kettle-installer".into(),
        prefix: prefix.into(),
        // The uid that PUBLISHED these files — which is what verification then
        // compares every recorded file against — rather than the prefix's
        // owner. The two agree in every case that verifies: a root install into
        // a root-owned prefix, or a user install into their own. They diverge
        // only when a non-root user has ACL write access to a root-owned
        // prefix, and `read_linux_install_provenance` already refuses that
        // install because the record it just wrote is not owned by the prefix
        // owner either. So this changes no reachable outcome; it stops the two
        // writers disagreeing about what the field means. `install-unix.py`
        // records `os.geteuid()`, and a record written by one has to be
        // readable by the other.
        owner_uid: unsafe { libc::geteuid() },
        files: files.into_values().collect(),
        directories: directories
            .into_iter()
            .map(|(path, mode)| UnixInstallDirectory { path, mode })
            .collect(),
    };
    let provenance = serde_json::to_string_pretty(&provenance)? + "\n";
    if provenance.len() > 1024 * 1024 {
        return Err(UpdateError::Transaction(
            "Linux install provenance exceeds its byte limit".into(),
        ));
    }
    transaction.install_bytes(
        Path::new(UNIX_INSTALL_PROVENANCE_FILE),
        provenance.as_bytes(),
        Some(0o644),
    )
}

#[cfg(target_os = "linux")]
fn render_linux_desktop_bytes(source: &[u8], prefix: &Path) -> Result<String, UpdateError> {
    let text = std::str::from_utf8(source).map_err(|error| {
        UpdateError::UnsafeArchive(format!("desktop template is not UTF-8: {error}"))
    })?;
    render_linux_desktop_text(text, prefix)
}

#[cfg(all(target_os = "linux", test))]
fn apply_staged_update(
    transaction: &mut Transaction,
    staging: &Path,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    let root = staging.join("kettle");
    let binary = root.join("kettle");
    require_file(&binary, "kettle/kettle")?;
    require_file(&root.join("install.sh"), "kettle/install.sh")?;
    require_file(&root.join("install-unix.py"), "kettle/install-unix.py")?;
    let previous_provenance = read_linux_install_provenance(&install.prefix)?;

    let map = [
        ("install.sh", "share/kettle/install.sh", 0o755),
        ("install-unix.py", "share/kettle/install-unix.py", 0o755),
        ("LICENSE", "share/doc/kettle/LICENSE", 0o644),
        ("NOTICE", "share/doc/kettle/NOTICE", 0o644),
        ("README.md", "share/doc/kettle/README.md", 0o644),
        ("CHANGELOG.md", "share/doc/kettle/CHANGELOG.md", 0o644),
        (
            "packaging/linux/kettle.desktop",
            "share/applications/kettle.desktop",
            0o644,
        ),
        (
            "packaging/linux/kettle.svg",
            "share/icons/hicolor/scalable/apps/kettle.svg",
            0o644,
        ),
        (
            "packaging/linux/kettle-16.png",
            "share/icons/hicolor/16x16/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-24.png",
            "share/icons/hicolor/24x24/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-32.png",
            "share/icons/hicolor/32x32/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-48.png",
            "share/icons/hicolor/48x48/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-64.png",
            "share/icons/hicolor/64x64/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-128.png",
            "share/icons/hicolor/128x128/apps/kettle.png",
            0o644,
        ),
        (
            "packaging/linux/kettle-256.png",
            "share/icons/hicolor/256x256/apps/kettle.png",
            0o644,
        ),
        ("packaging/linux/kettle.1", "share/man/man1/kettle.1", 0o644),
    ];
    let shell_root = root.join("shell-integration");
    let shell_sources = collect_files(&shell_root)?;
    let mut destinations = map
        .iter()
        .map(|(_, destination, _)| PathBuf::from(destination))
        .collect::<Vec<_>>();
    for source in &shell_sources {
        let relative = source
            .strip_prefix(&shell_root)
            .map_err(|_| UpdateError::UnsafeArchive(source.display().to_string()))?;
        destinations.push(Path::new("share/kettle/shell-integration").join(relative));
    }
    destinations.push(PathBuf::from("bin/kettle"));
    destinations.push(PathBuf::from("share/kettle/install.json"));
    destinations.push(PathBuf::from(UNIX_INSTALL_PROVENANCE_FILE));

    transaction.preflight_destinations(&destinations)?;
    let mut provenance_files = Vec::with_capacity(destinations.len() - 1);
    for (source, destination, mode) in map {
        let source = root.join(source);
        require_file(
            &source,
            source
                .strip_prefix(staging)
                .unwrap_or(&source)
                .to_string_lossy(),
        )?;
        if destination.ends_with("kettle.desktop") {
            let desktop = render_linux_desktop(&source, &install.prefix)?;
            transaction.install_bytes(Path::new(destination), desktop.as_bytes(), Some(mode))?;
            provenance_files.push(UnixInstallFile {
                path: destination.to_string(),
                size: desktop.len() as u64,
                sha256: sha256_bytes(desktop.as_bytes()),
                mode,
            });
        } else {
            transaction.install(Path::new(destination), &source, Some(mode))?;
            provenance_files.push(UnixInstallFile {
                path: destination.to_string(),
                size: source.metadata()?.len(),
                sha256: sha256_file(&source)?,
                mode,
            });
        }
    }
    for source in shell_sources {
        let relative = source
            .strip_prefix(&shell_root)
            .map_err(|_| UpdateError::UnsafeArchive(source.display().to_string()))?;
        transaction.install(
            &Path::new("share/kettle/shell-integration").join(relative),
            &source,
            Some(0o644),
        )?;
        let destination = Path::new("share/kettle/shell-integration").join(relative);
        provenance_files.push(UnixInstallFile {
            path: relative_to_string(&destination)?,
            size: source.metadata()?.len(),
            sha256: sha256_file(&source)?,
            mode: 0o644,
        });
    }
    transaction.install(Path::new("bin/kettle"), &binary, Some(0o755))?;
    provenance_files.push(UnixInstallFile {
        path: "bin/kettle".into(),
        size: binary.metadata()?.len(),
        sha256: sha256_file(&binary)?,
        mode: 0o755,
    });
    let marker = marker_json(&update.version.to_string())?;
    transaction.install_bytes(
        Path::new("share/kettle/install.json"),
        marker.as_bytes(),
        Some(0o644),
    )?;
    provenance_files.push(UnixInstallFile {
        path: "share/kettle/install.json".into(),
        size: marker.len() as u64,
        sha256: sha256_bytes(marker.as_bytes()),
        mode: 0o644,
    });
    install_unix_provenance(transaction, install, previous_provenance, provenance_files)?;
    transaction.finish_preflight()?;
    Ok(())
}

#[cfg(all(target_os = "linux", test))]
fn render_linux_desktop(source: &Path, prefix: &Path) -> Result<String, UpdateError> {
    let text = fs::read_to_string(source)?;
    render_linux_desktop_text(&text, prefix)
}

#[cfg(target_os = "linux")]
fn render_linux_desktop_text(text: &str, prefix: &Path) -> Result<String, UpdateError> {
    let executable = prefix.join("bin/kettle");
    // Match scripts/install.sh's known-good absolute PNG contract. Keeping one
    // user-local icon format avoids SVG loader/theme variance when GNOME Shell
    // refreshes the launcher after an authenticated update.
    let icon = prefix.join("share/icons/hicolor/256x256/apps/kettle.png");
    let exec_value = desktop_exec_argument(&executable)?;
    let executable_value = desktop_string_path(&executable)?;
    let icon_value = desktop_string_path(&icon)?;
    let mut rendered = String::with_capacity(
        text.len() + exec_value.len() + executable_value.len() + icon_value.len(),
    );
    let mut replacements = [0_u8; 3];
    for line in text.lines() {
        match line {
            "Exec=kettle" => {
                replacements[0] = replacements[0].saturating_add(1);
                rendered.push_str(&format!("Exec={exec_value}"));
            }
            "TryExec=kettle" => {
                replacements[1] = replacements[1].saturating_add(1);
                rendered.push_str(&format!("TryExec={executable_value}"));
            }
            "Icon=kettle" => {
                replacements[2] = replacements[2].saturating_add(1);
                rendered.push_str(&format!("Icon={icon_value}"));
            }
            _ => rendered.push_str(line),
        }
        rendered.push('\n');
    }
    if replacements != [1, 1, 1] {
        return Err(UpdateError::Transaction(format!(
            "desktop template must contain exactly one Exec=kettle, TryExec=kettle, and Icon=kettle entry (found {}, {}, {})",
            replacements[0], replacements[1], replacements[2]
        )));
    }
    Ok(rendered)
}

#[cfg(target_os = "linux")]
fn desktop_string_path(path: &Path) -> Result<String, UpdateError> {
    let value = desktop_path_text(path)?;
    Ok(desktop_escape_string(value))
}

#[cfg(target_os = "linux")]
fn desktop_path_text(path: &Path) -> Result<&str, UpdateError> {
    let value = path.to_str().ok_or_else(|| {
        UpdateError::Transaction(format!(
            "desktop integration path is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    if value.chars().any(char::is_control) {
        return Err(UpdateError::Transaction(format!(
            "desktop integration path contains a control character: {}",
            path.display()
        )));
    }
    Ok(value)
}

#[cfg(target_os = "linux")]
fn desktop_escape_string(value: &str) -> String {
    value.replace('\\', "\\\\")
}

#[cfg(target_os = "linux")]
fn desktop_exec_argument(path: &Path) -> Result<String, UpdateError> {
    let value = desktop_path_text(path)?;
    if value.contains('=') {
        return Err(UpdateError::Transaction(format!(
            "desktop executable path contains '=': {}",
            path.display()
        )));
    }
    let mut argument = String::with_capacity(value.len() + 8);
    for character in value.chars() {
        match character {
            '\\' => argument.push_str("\\\\"),
            '"' => argument.push_str("\\\""),
            '`' => argument.push_str("\\`"),
            '$' => argument.push_str("\\$"),
            '%' => argument.push_str("%%"),
            _ => argument.push(character),
        }
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    quoted.push_str(&desktop_escape_string(&argument));
    quoted.push('"');
    Ok(quoted)
}

#[cfg(all(any(windows, target_os = "linux"), test))]
fn require_file(path: &Path, label: impl std::fmt::Display) -> Result<(), UpdateError> {
    if !path.is_file() {
        return Err(UpdateError::MissingArchiveFile(label.to_string()));
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn collect_files(root: &Path) -> Result<Vec<PathBuf>, UpdateError> {
    if !root.is_dir() {
        return Err(UpdateError::MissingArchiveFile(root.display().to_string()));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let kind = entry.file_type()?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
                if fs::symlink_metadata(entry.path())?.file_attributes()
                    & FILE_ATTRIBUTE_REPARSE_POINT
                    != 0
                {
                    return Err(UpdateError::UnsafeArchive(
                        entry.path().display().to_string(),
                    ));
                }
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            } else {
                return Err(UpdateError::UnsafeArchive(
                    entry.path().display().to_string(),
                ));
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Prepared,
    Applying,
    RollingBack,
    Committed,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalEntryState {
    BackingUp,
    Prepared,
    Installed,
    Restored,
    BackupDiscarded,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionProgress {
    ParentDirectoriesPersisted,
    BackupStreaming,
    BackupSynced,
    EntryPrepared,
    Published,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    transaction_id: String,
    target_version: String,
    phase: JournalPhase,
    backup_dir: String,
    /// Prefix-relative directories created by this transaction. `default`
    /// keeps schema-2 journals written before this field was introduced
    /// recoverable; omitting the empty map also preserves their wire shape
    /// until a transaction actually creates a directory.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    created_directories: std::collections::BTreeMap<String, u32>,
    entries: Vec<JournalEntry>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    relative: String,
    existed: bool,
    #[serde(default)]
    previous_unix_mode: Option<u32>,
    previous_size: Option<u64>,
    previous_sha256: Option<String>,
    replacement_size: u64,
    replacement_sha256: String,
    state: JournalEntryState,
}

/// Schema-1 journals can be left by v2.34 if that updater is interrupted after
/// replacing the binary. Retain a narrow recovery reader so upgrading does not
/// strand an otherwise recoverable installation.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LegacyJournal {
    schema: u32,
    backup_dir: String,
    entries: Vec<LegacyJournalEntry>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LegacyJournalEntry {
    relative: String,
    existed: bool,
    #[serde(default)]
    previous_unix_mode: Option<u32>,
}

/// Keeps the destination parent anchored while a transaction operates on its
/// leaf. Linux resolves through an open directory descriptor; Windows keeps
/// every ancestor open without delete sharing so it cannot be replaced by a
/// junction or symlink during the operation.
#[cfg(target_os = "linux")]
struct AnchoredParent {
    directory: File,
}

#[cfg(windows)]
struct AnchoredParent {
    _directories: Vec<File>,
    path: PathBuf,
}

#[cfg(target_os = "linux")]
fn open_anchored_directory(path: &Path) -> Result<File, UpdateError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let directory = options.open(path)?;
    if !directory.metadata()?.file_type().is_dir() {
        return Err(UpdateError::Transaction(format!(
            "install path component is not a directory: {}",
            path.display()
        )));
    }
    Ok(directory)
}

#[cfg(target_os = "linux")]
fn directory_descriptor_path(directory: &File) -> PathBuf {
    use std::os::fd::AsRawFd as _;
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

#[cfg(target_os = "linux")]
fn anchored_parent(
    prefix: &Path,
    relative: &Path,
    create_missing: bool,
) -> Result<AnchoredParent, UpdateError> {
    anchored_parent_recording(
        prefix,
        relative,
        create_missing,
        &mut Vec::new(),
        &mut |_| Ok(()),
    )
}

/// As [`anchored_parent`], calling `before_create` with each missing
/// prefix-relative directory before its filesystem mutation, then appending
/// each directory this call actually created to `created`.
///
/// `fs::create_dir` returning `Ok(())` — as opposed to `AlreadyExists` — is the
/// only authoritative answer to "did we create this?". Sampling `try_exists`
/// before the writes gets it wrong whenever an earlier attempt created the
/// directory and rolled back.
#[cfg(target_os = "linux")]
fn anchored_parent_recording(
    prefix: &Path,
    relative: &Path,
    create_missing: bool,
    created: &mut Vec<String>,
    before_create: &mut impl FnMut(&str) -> Result<(), UpdateError>,
) -> Result<AnchoredParent, UpdateError> {
    validate_relative(relative)?;
    let mut directory = open_anchored_directory(prefix).map_err(|error| {
        UpdateError::Transaction(format!(
            "cannot anchor install prefix {}: {error}",
            prefix.display()
        ))
    })?;
    if !directory_descriptor_path(&directory).is_dir() {
        return Err(UpdateError::Transaction(
            "Linux /proc/self/fd is required for contained self-update writes".into(),
        ));
    }
    if let Some(parent) = relative.parent() {
        let mut walked = PathBuf::new();
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(UpdateError::Transaction("unsafe install path".into()));
            };
            walked.push(name);
            let candidate = directory_descriptor_path(&directory).join(name);
            let next = match open_anchored_directory(&candidate) {
                Ok(next) => next,
                Err(UpdateError::Io(error))
                    if create_missing && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    use std::os::unix::fs::PermissionsExt as _;

                    let created_relative = relative_to_string(&walked)?;
                    before_create(&created_relative)?;
                    match fs::create_dir(&candidate) {
                        Ok(()) => {
                            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))?;
                            // Persist each new directory entry before a journal
                            // can refer to content below it.
                            directory.sync_all()?;
                            created.push(created_relative);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(UpdateError::Transaction(format!(
                                "install path appeared while its creation was being journaled: {}",
                                candidate.display()
                            )));
                        }
                        Err(error) => return Err(error.into()),
                    }
                    open_anchored_directory(&candidate)?
                }
                Err(error) => {
                    return Err(UpdateError::Transaction(format!(
                        "install path component cannot be opened safely ({}): {error}",
                        candidate.display()
                    )));
                }
            };
            directory = next;
        }
    }
    Ok(AnchoredParent { directory })
}

#[cfg(target_os = "linux")]
impl AnchoredParent {
    fn destination(&self, relative: &Path) -> Result<PathBuf, UpdateError> {
        let name = relative.file_name().ok_or_else(|| {
            UpdateError::Transaction(format!("install path has no leaf: {}", relative.display()))
        })?;
        Ok(directory_descriptor_path(&self.directory).join(name))
    }
}

#[cfg(windows)]
struct WindowsHeldFile {
    _parent: AnchoredParent,
    file: File,
    path: PathBuf,
}

#[cfg(windows)]
struct WindowsHeldDirectory {
    _parent: AnchoredParent,
    directory: File,
    path: PathBuf,
}

#[cfg(windows)]
fn open_cleanup_anchored_directory(path: &Path) -> Result<File, UpdateError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let directory = retry_windows_sharing_violation(|| options.open(path))?;
    let metadata = directory.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(UpdateError::Transaction(format!(
            "cleanup ancestor is not a real directory: {}",
            path.display()
        )));
    }
    Ok(directory)
}

#[cfg(windows)]
fn cleanup_anchored_destination(
    prefix: &Path,
    relative: &Path,
) -> Result<(AnchoredParent, PathBuf), UpdateError> {
    validate_relative(relative)?;
    let prefix = std::path::absolute(prefix)?;
    let mut path = PathBuf::new();
    let mut directories = Vec::new();
    for component in prefix.components() {
        match component {
            Component::Prefix(_) => path.push(component.as_os_str()),
            Component::RootDir | Component::Normal(_) => {
                path.push(component.as_os_str());
                directories.push(open_cleanup_anchored_directory(&path)?);
            }
            Component::CurDir | Component::ParentDir => {
                return Err(UpdateError::Transaction(format!(
                    "cleanup prefix is not absolute and normalized: {}",
                    prefix.display()
                )));
            }
        }
    }
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(UpdateError::Transaction("unsafe cleanup path".into()));
            };
            path.push(name);
            directories.push(open_cleanup_anchored_directory(&path)?);
        }
    }
    let parent = AnchoredParent {
        _directories: directories,
        path,
    };
    let destination = parent.destination(relative)?;
    Ok((parent, destination))
}

#[cfg(windows)]
fn open_windows_held_file(prefix: &Path, relative: &Path) -> Result<WindowsHeldFile, UpdateError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Foundation::GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };

    let (parent, path) = cleanup_anchored_destination(prefix, relative)?;
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = retry_windows_sharing_violation(|| options.open(&path))?;
    let metadata = file.metadata()?;
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if !metadata.file_type().is_file()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || windows_file_information(&file)?.nNumberOfLinks != 1
        {
            return Err(UpdateError::Transaction(format!(
                "cleanup target is not a single-link ordinary file: {}",
                path.display()
            )));
        }
    }
    Ok(WindowsHeldFile {
        _parent: parent,
        file,
        path,
    })
}

#[cfg(windows)]
fn open_windows_held_directory(
    prefix: &Path,
    relative: &Path,
) -> Result<WindowsHeldDirectory, UpdateError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let (parent, path) = cleanup_anchored_destination(prefix, relative)?;
    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let directory = retry_windows_sharing_violation(|| options.open(&path))?;
    let metadata = directory.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(UpdateError::Transaction(format!(
            "cleanup target is not a real directory: {}",
            path.display()
        )));
    }
    Ok(WindowsHeldDirectory {
        _parent: parent,
        directory,
        path,
    })
}

#[cfg(windows)]
fn mark_windows_handle_for_deletion(file: &File) -> Result<(), UpdateError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the held handle was opened with DELETE and `disposition` exactly
    // matches the FileDispositionInfo contract.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(windows)]
fn rename_windows_held_file(
    held: &WindowsHeldFile,
    destination_name: &str,
) -> Result<(), UpdateError> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
    };

    if destination_name.is_empty()
        || destination_name.contains(['/', '\\'])
        || destination_name.encode_utf16().any(|unit| unit == 0)
    {
        return Err(UpdateError::Transaction(
            "invalid held-file rename destination".into(),
        ));
    }
    let destination = held._parent.path.join(destination_name);
    let name = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    let name_bytes = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| UpdateError::Transaction("rename destination is too long".into()))?;
    let total_bytes = std::mem::size_of::<FILE_RENAME_INFO>()
        .checked_add(name_bytes)
        .ok_or_else(|| UpdateError::Transaction("rename destination is too long".into()))?;
    let mut storage = vec![0_u64; total_bytes.div_ceil(std::mem::size_of::<u64>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `storage` is aligned, zeroed, and sized for the fixed header plus
    // every UTF-16 unit. Both handles remain owned for the call.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = u32::try_from(name_bytes)
            .map_err(|_| UpdateError::Transaction("rename destination is too long".into()))?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(
            held.file.as_raw_handle() as HANDLE,
            FileRenameInfo,
            info.cast(),
            u32::try_from(total_bytes)
                .map_err(|_| UpdateError::Transaction("rename buffer is too large".into()))?,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(windows)]
struct WindowsHeldTree {
    root: WindowsHeldDirectory,
    nested: Option<WindowsHeldDirectory>,
    files: Vec<(PathBuf, WindowsHeldFile)>,
}

#[cfg(windows)]
impl WindowsHeldTree {
    fn delete(self) -> Result<(), UpdateError> {
        for (_, held) in &self.files {
            mark_windows_handle_for_deletion(&held.file)?;
        }
        drop(self.files);
        if let Some(nested) = self.nested {
            mark_windows_handle_for_deletion(&nested.directory)?;
            drop(nested);
        }
        mark_windows_handle_for_deletion(&self.root.directory)?;
        drop(self.root);
        Ok(())
    }
}

#[cfg(windows)]
fn hold_windows_two_level_tree(
    prefix: &Path,
    root_relative: &Path,
) -> Result<WindowsHeldTree, UpdateError> {
    let root = open_windows_held_directory(prefix, root_relative)?;
    let mut nested = None;
    let mut files = Vec::new();
    let mut entries = 0_usize;
    let mut total = 0_u64;
    for entry in fs::read_dir(&root.path)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            UpdateError::Transaction(format!(
                "managed cleanup tree contains a non-UTF-8 name: {}",
                entry.path().display()
            ))
        })?;
        let metadata = fs::symlink_metadata(entry.path())?;
        entries = entries.saturating_add(1);
        if entries > MAX_ARCHIVE_ENTRIES {
            return Err(UpdateError::Transaction(
                "managed cleanup tree exceeds the entry limit".into(),
            ));
        }
        if metadata.file_type().is_dir() {
            if name != "shell-integration" || nested.is_some() {
                return Err(UpdateError::Transaction(format!(
                    "managed cleanup tree contains an unexpected directory {}",
                    entry.path().display()
                )));
            }
            let nested_relative = root_relative.join("shell-integration");
            let held_nested = open_windows_held_directory(prefix, &nested_relative)?;
            let nested_root = held_nested._parent._directories.last().ok_or_else(|| {
                UpdateError::Transaction("nested cleanup directory has no root anchor".into())
            })?;
            if !same_transaction_file_identity(&root.directory, nested_root)? {
                return Err(UpdateError::Transaction(
                    "cleanup root changed while its nested directory was opened".into(),
                ));
            }
            for shell_entry in fs::read_dir(&held_nested.path)? {
                let shell_entry = shell_entry?;
                entries = entries.saturating_add(1);
                if entries > MAX_ARCHIVE_ENTRIES {
                    return Err(UpdateError::Transaction(
                        "managed cleanup tree exceeds the entry limit".into(),
                    ));
                }
                let relative = Path::new("shell-integration").join(shell_entry.file_name());
                let full_relative = root_relative.join(&relative);
                let held = open_windows_held_file(prefix, &full_relative)?;
                let file_parent = held._parent._directories.last().ok_or_else(|| {
                    UpdateError::Transaction("nested cleanup file has no parent anchor".into())
                })?;
                if !same_transaction_file_identity(&held_nested.directory, file_parent)? {
                    return Err(UpdateError::Transaction(
                        "nested cleanup directory changed while a file was opened".into(),
                    ));
                }
                total = total
                    .checked_add(held.file.metadata()?.len())
                    .ok_or_else(|| {
                        UpdateError::Transaction("managed cleanup size overflow".into())
                    })?;
                if total > MAX_UNPACKED_BYTES {
                    return Err(UpdateError::Transaction(
                        "managed cleanup tree exceeds the size limit".into(),
                    ));
                }
                files.push((relative, held));
            }
            nested = Some(held_nested);
        } else {
            let relative = PathBuf::from(name);
            let full_relative = root_relative.join(&relative);
            let held = open_windows_held_file(prefix, &full_relative)?;
            let file_parent = held._parent._directories.last().ok_or_else(|| {
                UpdateError::Transaction("cleanup file has no root anchor".into())
            })?;
            if !same_transaction_file_identity(&root.directory, file_parent)? {
                return Err(UpdateError::Transaction(
                    "cleanup root changed while a file was opened".into(),
                ));
            }
            total = total
                .checked_add(held.file.metadata()?.len())
                .ok_or_else(|| UpdateError::Transaction("managed cleanup size overflow".into()))?;
            if total > MAX_UNPACKED_BYTES {
                return Err(UpdateError::Transaction(
                    "managed cleanup tree exceeds the size limit".into(),
                ));
            }
            files.push((relative, held));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(WindowsHeldTree {
        root,
        nested,
        files,
    })
}

#[cfg(windows)]
fn read_windows_held_file(
    held: &mut WindowsHeldFile,
    limit: usize,
) -> Result<Vec<u8>, UpdateError> {
    held.file.rewind()?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut held.file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(UpdateError::Transaction(format!(
            "held cleanup file exceeds its limit: {}",
            held.path.display()
        )));
    }
    Ok(bytes)
}

#[cfg(windows)]
fn open_anchored_directory(path: &Path) -> Result<File, UpdateError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let directory = options.open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(UpdateError::Transaction(format!(
            "install path component is not a real directory: {}",
            path.display()
        )));
    }
    Ok(directory)
}

#[cfg(windows)]
fn anchored_parent(
    prefix: &Path,
    relative: &Path,
    create_missing: bool,
) -> Result<AnchoredParent, UpdateError> {
    anchored_parent_recording(
        prefix,
        relative,
        create_missing,
        &mut Vec::new(),
        &mut |_| Ok(()),
    )
}

/// As [`anchored_parent`], calling `before_create` with each missing
/// prefix-relative directory before its filesystem mutation, then appending
/// each directory this call actually created to `created`.
///
/// `fs::create_dir` returning `Ok(())` — as opposed to `AlreadyExists` — is the
/// only authoritative answer to "did we create this?". Sampling `try_exists`
/// before the writes gets it wrong whenever an earlier attempt created the
/// directory and rolled back.
#[cfg(windows)]
fn anchored_parent_recording(
    prefix: &Path,
    relative: &Path,
    create_missing: bool,
    created: &mut Vec<String>,
    before_create: &mut impl FnMut(&str) -> Result<(), UpdateError>,
) -> Result<AnchoredParent, UpdateError> {
    validate_relative(relative)?;
    let prefix = std::path::absolute(prefix)?;
    let mut path = PathBuf::new();
    let mut directories = Vec::new();
    for component in prefix.components() {
        match component {
            Component::Prefix(_) => path.push(component.as_os_str()),
            Component::RootDir | Component::Normal(_) => {
                path.push(component.as_os_str());
                directories.push(open_anchored_directory(&path).map_err(|error| {
                    UpdateError::Transaction(format!(
                        "cannot anchor install path component {}: {error}",
                        path.display()
                    ))
                })?);
            }
            Component::CurDir | Component::ParentDir => {
                return Err(UpdateError::Transaction(format!(
                    "install prefix is not absolute and normalized: {}",
                    prefix.display()
                )));
            }
        }
    }
    if directories.is_empty() {
        return Err(UpdateError::Transaction(format!(
            "install prefix has no anchored directory components: {}",
            prefix.display()
        )));
    }
    if let Some(parent) = relative.parent() {
        let mut walked = PathBuf::new();
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(UpdateError::Transaction("unsafe install path".into()));
            };
            walked.push(name);
            path.push(name);
            let next = match open_anchored_directory(&path) {
                Ok(next) => next,
                Err(UpdateError::Io(error))
                    if create_missing && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    let created_relative = relative_to_string(&walked)?;
                    before_create(&created_relative)?;
                    match fs::create_dir(&path) {
                        Ok(()) => created.push(created_relative),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(UpdateError::Transaction(format!(
                                "install path appeared while its creation was being journaled: {}",
                                path.display()
                            )));
                        }
                        Err(error) => return Err(error.into()),
                    }
                    open_anchored_directory(&path)?
                }
                Err(error) => {
                    return Err(UpdateError::Transaction(format!(
                        "install path component cannot be opened safely ({}): {error}",
                        path.display()
                    )));
                }
            };
            directories.push(next);
        }
    }
    Ok(AnchoredParent {
        _directories: directories,
        path,
    })
}

#[cfg(windows)]
impl AnchoredParent {
    fn destination(&self, relative: &Path) -> Result<PathBuf, UpdateError> {
        let name = relative.file_name().ok_or_else(|| {
            UpdateError::Transaction(format!("install path has no leaf: {}", relative.display()))
        })?;
        Ok(self.path.join(name))
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn anchored_destination(
    prefix: &Path,
    relative: &Path,
    create_missing: bool,
) -> Result<(AnchoredParent, PathBuf), UpdateError> {
    let parent = anchored_parent(prefix, relative, create_missing)?;
    let destination = parent.destination(relative)?;
    Ok((parent, destination))
}

#[cfg(any(windows, target_os = "linux"))]
fn open_regular_nofollow(path: &Path) -> Result<File, UpdateError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(windows)]
    let file = retry_windows_sharing_violation(|| options.open(path))?;
    #[cfg(not(windows))]
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    let is_reparse = {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    #[cfg(target_os = "linux")]
    let is_reparse = false;
    if !metadata.file_type().is_file() || is_reparse {
        return Err(UpdateError::Transaction(format!(
            "refusing to read non-regular file {}",
            path.display()
        )));
    }
    Ok(file)
}

/// Windows security/indexing software can briefly open a newly written update
/// journal or backup without sharing delete access. A single immediate retry
/// is not sufficient under a parallel workspace run, while treating the file
/// as permanently unavailable makes a committed update look corrupt. Retry
/// only the two transient sharing errors, for a short fixed interval. Every
/// successful caller still validates the opened handle before trusting or
/// deleting anything; every other error remains immediate.
#[cfg(windows)]
fn retry_windows_sharing_violation<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};

    const RETRY_FOR: std::time::Duration = std::time::Duration::from_millis(250);
    const RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(5);

    let deadline = std::time::Instant::now() + RETRY_FOR;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code)
                        if code == ERROR_SHARING_VIOLATION as i32
                            || code == ERROR_LOCK_VIOLATION as i32
                ) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(RETRY_EVERY);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn read_bounded_regular(path: &Path, limit: usize) -> Result<Vec<u8>, UpdateError> {
    let mut file = open_regular_nofollow(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > limit as u64 {
        return Err(UpdateError::Transaction(format!(
            "file exceeds the {limit}-byte safety limit: {}",
            path.display()
        )));
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len().min(64 * 1024)).unwrap_or(64 * 1024));
    Read::by_ref(&mut file)
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(UpdateError::Transaction(format!(
            "file exceeds the {limit}-byte safety limit: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

#[cfg(any(windows, target_os = "linux"))]
fn read_transaction_file(path: &Path) -> Result<(Vec<u8>, Option<u32>), UpdateError> {
    let (mut file, mode) = open_transaction_snapshot(path)?;
    let metadata = file.metadata()?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len().min(64 * 1024)).unwrap_or(64 * 1024));
    Read::by_ref(&mut file)
        .take(MAX_UNPACKED_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_UNPACKED_BYTES {
        return Err(UpdateError::Transaction(format!(
            "transaction file exceeds the safety limit: {}",
            path.display()
        )));
    }
    Ok((bytes, mode))
}

#[cfg(any(windows, target_os = "linux"))]
fn open_transaction_snapshot(path: &Path) -> Result<(File, Option<u32>), UpdateError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            #[cfg(target_os = "linux")]
            if error.raw_os_error() == Some(libc::ELOOP) {
                return Err(UpdateError::Transaction(format!(
                    "refusing transaction snapshot of non-regular file: {}",
                    path.display()
                )));
            }
            return Err(error.into());
        }
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(UpdateError::Transaction(format!(
            "refusing transaction snapshot of non-regular file: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(UpdateError::Transaction(format!(
                "transaction snapshot is a reparse point: {}",
                path.display()
            )));
        }
        let information = windows_file_information(&file)?;
        if information.nNumberOfLinks != 1 {
            return Err(UpdateError::Transaction(format!(
                "transaction snapshot is not a single-link file: {}",
                path.display()
            )));
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(UpdateError::Transaction(format!(
                "transaction snapshot is not a single-link file: {}",
                path.display()
            )));
        }
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        Some(metadata.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok((file, mode))
}

#[cfg(any(windows, target_os = "linux"))]
fn snapshot_transaction_destination(
    prefix: &Path,
    relative: &Path,
) -> Result<PreflightDestination, UpdateError> {
    if let Err(error) = fs::symlink_metadata(prefix.join(relative)) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(PreflightDestination {
                file: None,
                previous_unix_mode: None,
            });
        }
        return Err(error.into());
    }
    // Keep the anchored parent alive while opening the descriptor-relative
    // leaf. On Linux `destination` is `/proc/self/fd/<parent>/name`; dropping
    // the handle inside this match arm makes that path immediately dangling
    // and misclassifies every existing destination as absent.
    let (destination_parent, destination) = match anchored_destination(prefix, relative, false) {
        Ok(anchored) => anchored,
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreflightDestination {
                file: None,
                previous_unix_mode: None,
            });
        }
        Err(error) => return Err(error),
    };
    let snapshot = match open_transaction_snapshot(&destination) {
        Ok((file, previous_unix_mode)) => Ok(PreflightDestination {
            file: Some(file),
            previous_unix_mode,
        }),
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PreflightDestination {
                file: None,
                previous_unix_mode: None,
            })
        }
        Err(error) => Err(error),
    };
    drop(destination_parent);
    snapshot
}

#[cfg(any(windows, target_os = "linux"))]
fn verify_transaction_destination_snapshot(
    prefix: &Path,
    relative: &Path,
    snapshot: &PreflightDestination,
) -> Result<(), UpdateError> {
    let current = snapshot_transaction_destination(prefix, relative)?;
    match (&snapshot.file, &current.file) {
        (None, None) => Ok(()),
        (Some(expected), Some(current)) if same_transaction_file_identity(expected, current)? => {
            Ok(())
        }
        _ => Err(UpdateError::Transaction(format!(
            "transaction destination changed after preflight: {}",
            relative.display()
        ))),
    }
}

#[cfg(windows)]
fn windows_file_information(
    file: &File,
) -> Result<windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION, UpdateError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

    let mut information = unsafe { std::mem::zeroed() };
    // SAFETY: `file` owns a valid handle and `information` is writable for the
    // exact structure required by GetFileInformationByHandle.
    if unsafe {
        GetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            std::ptr::addr_of_mut!(information),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(information)
}

#[cfg(any(windows, target_os = "linux"))]
fn same_transaction_file_identity(left: &File, right: &File) -> Result<bool, UpdateError> {
    #[cfg(windows)]
    {
        let left = windows_file_information(left)?;
        let right = windows_file_information(right)?;
        Ok(left.dwVolumeSerialNumber == right.dwVolumeSerialNumber
            && left.nFileIndexHigh == right.nFileIndexHigh
            && left.nFileIndexLow == right.nFileIndexLow)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;
        let left = left.metadata()?;
        let right = right.metadata()?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn stream_transaction_backup(
    source: &mut File,
    destination: &Path,
    progress: &mut impl FnMut(TransactionProgress),
) -> Result<(u64, String), UpdateError> {
    source.rewind()?;
    let expected_size = source.metadata()?.len();
    let mut backup = kettle_state::create_private_file_new(destination)?;
    let result = (|| {
        let mut hash = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(count as u64)
                .ok_or_else(|| UpdateError::Transaction("backup size overflow".into()))?;
            if total > MAX_UNPACKED_BYTES {
                return Err(UpdateError::Transaction(
                    "backup exceeds the safety limit".into(),
                ));
            }
            hash.update(&buffer[..count]);
            backup.write_all(&buffer[..count])?;
            progress(TransactionProgress::BackupStreaming);
        }
        if total != expected_size {
            return Err(UpdateError::Transaction(
                "transaction source changed while it was backed up".into(),
            ));
        }
        backup.flush()?;
        backup.sync_all()?;
        Ok((total, hex::encode(hash.finalize())))
    })();
    if result.is_err() {
        kettle_state::discard_created_private_file(backup, destination);
        return result;
    }
    drop(backup);
    if let Some(parent) = destination.parent() {
        sync_parent(parent)?;
    }
    result
}

#[cfg(any(windows, target_os = "linux"))]
struct Transaction {
    prefix: PathBuf,
    journal_path: PathBuf,
    backup_dir: PathBuf,
    journal: Journal,
    preflight: Option<std::collections::HashMap<String, PreflightDestination>>,
}

#[cfg(any(windows, target_os = "linux"))]
struct PreflightDestination {
    file: Option<File>,
    previous_unix_mode: Option<u32>,
}

#[cfg(any(windows, target_os = "linux"))]
impl Transaction {
    #[cfg(any(target_os = "linux", test))]
    fn begin(prefix: &Path, target_version: &str) -> Result<Self, UpdateError> {
        Self::begin_with_transaction_id(prefix, target_version, &unique_suffix())
    }

    fn begin_with_transaction_id(
        prefix: &Path,
        target_version: &str,
        transaction_id: &str,
    ) -> Result<Self, UpdateError> {
        semver::Version::parse(target_version).map_err(|error| {
            UpdateError::Transaction(format!("invalid transaction target version: {error}"))
        })?;
        if !is_transaction_id(transaction_id) {
            return Err(UpdateError::Transaction(
                "invalid update transaction id".into(),
            ));
        }
        let suffix = transaction_id.to_string();
        let backup_name = format!(".kettle-update-backup-{suffix}");
        let backup_dir = prefix.join(&backup_name);
        let journal_path = prefix.join(".kettle-update-journal.json");
        if journal_path.exists() {
            return Err(UpdateError::Transaction(
                "an update journal already exists; recover it before starting another transaction"
                    .into(),
            ));
        }
        fs::create_dir(&backup_dir)?;
        set_private_directory(&backup_dir)?;
        sync_parent(prefix)?;
        let backup_marker = BackupMarker {
            schema: JOURNAL_SCHEMA,
            product: "kettle".into(),
            transaction_id: suffix.clone(),
        };
        if let Err(error) = atomic_write(
            &backup_dir.join(BACKUP_MARKER_FILE),
            &serde_json::to_vec_pretty(&backup_marker)?,
            Some(0o600),
        ) {
            let _ = remove_new_backup_dir_checked(prefix, &backup_dir);
            return Err(error);
        }
        let mut transaction = Self {
            prefix: prefix.to_path_buf(),
            journal_path,
            backup_dir,
            journal: Journal {
                schema: JOURNAL_SCHEMA,
                transaction_id: suffix,
                target_version: target_version.to_string(),
                phase: JournalPhase::Prepared,
                backup_dir: backup_name,
                created_directories: std::collections::BTreeMap::new(),
                entries: Vec::new(),
            },
            preflight: None,
        };
        if let Err(error) = transaction.persist_journal() {
            let _ = remove_new_backup_dir_checked(prefix, &transaction.backup_dir);
            return Err(error);
        }
        Ok(transaction)
    }

    fn preflight_destinations(&mut self, destinations: &[PathBuf]) -> Result<(), UpdateError> {
        if self.journal.phase != JournalPhase::Prepared
            || !self.journal.entries.is_empty()
            || self.preflight.is_some()
            || destinations.len() > MAX_ARCHIVE_ENTRIES
        {
            return Err(UpdateError::Transaction(
                "transaction destination preflight was requested in an invalid state".into(),
            ));
        }
        let mut snapshots = std::collections::HashMap::new();
        let mut previous_count = 0_usize;
        let mut previous_bytes = 0_u64;
        for relative in destinations {
            validate_relative(relative)?;
            let key = relative_to_string(relative)?.to_ascii_lowercase();
            if snapshots.contains_key(&key) {
                return Err(UpdateError::Transaction(format!(
                    "duplicate preflight destination {}",
                    relative.display()
                )));
            }
            let snapshot = snapshot_transaction_destination(&self.prefix, relative)?;
            if let Some(file) = snapshot.file.as_ref() {
                previous_count = previous_count.saturating_add(1);
                previous_bytes = previous_bytes
                    .checked_add(file.metadata()?.len())
                    .ok_or_else(|| {
                        UpdateError::Transaction("preflight backup size overflow".into())
                    })?;
                if previous_count > MAX_ARCHIVE_ENTRIES.saturating_sub(1)
                    || previous_bytes > MAX_UNPACKED_BYTES
                {
                    return Err(UpdateError::Transaction(
                        "existing transaction backup set exceeds the safety quota".into(),
                    ));
                }
            }
            snapshots.insert(key, snapshot);
        }
        self.preflight = Some(snapshots);
        Ok(())
    }

    fn finish_preflight(&self) -> Result<(), UpdateError> {
        if self
            .preflight
            .as_ref()
            .is_some_and(|preflight| !preflight.is_empty())
        {
            return Err(UpdateError::Transaction(
                "transaction did not consume every preflight destination".into(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn install(
        &mut self,
        relative: &Path,
        source: &Path,
        unix_mode: Option<u32>,
    ) -> Result<(), UpdateError> {
        validate_relative(relative)?;
        let (bytes, _) = read_transaction_file(source)?;
        self.install_bytes(relative, &bytes, unix_mode)
    }

    fn install_bytes(
        &mut self,
        relative: &Path,
        bytes: &[u8],
        unix_mode: Option<u32>,
    ) -> Result<(), UpdateError> {
        self.install_bytes_with_progress_and_post_publish(
            relative,
            bytes,
            unix_mode,
            |_| {},
            || Ok(()),
        )
    }

    // Both callers are the Linux crash-seam fixtures
    // (`interrupted_managed_linux_install` and
    // `published_executable_has_final_mode_before_installed_journal_state`), so
    // this helper must carry their target gate too. Under a bare `cfg(test)` it
    // is dead code on every other platform, and `-D warnings` turns that into a
    // hard build failure on the Windows CI leg.
    #[cfg(all(test, target_os = "linux"))]
    fn install_bytes_with_post_publish(
        &mut self,
        relative: &Path,
        bytes: &[u8],
        unix_mode: Option<u32>,
        post_publish: impl FnOnce() -> Result<(), UpdateError>,
    ) -> Result<(), UpdateError> {
        self.install_bytes_with_progress_and_post_publish(
            relative,
            bytes,
            unix_mode,
            |_| {},
            post_publish,
        )
    }

    fn install_bytes_with_progress_and_post_publish(
        &mut self,
        relative: &Path,
        bytes: &[u8],
        unix_mode: Option<u32>,
        mut progress: impl FnMut(TransactionProgress),
        post_publish: impl FnOnce() -> Result<(), UpdateError>,
    ) -> Result<(), UpdateError> {
        validate_relative(relative)?;
        let relative_string = relative_to_string(relative)?;
        if self
            .journal
            .entries
            .iter()
            .any(|entry| entry.relative.eq_ignore_ascii_case(&relative_string))
        {
            return Err(UpdateError::Transaction(format!(
                "duplicate install destination {relative_string}"
            )));
        }
        if bytes.len() as u64 > MAX_UNPACKED_BYTES
            || self.journal.entries.len() >= MAX_ARCHIVE_ENTRIES
        {
            return Err(UpdateError::Transaction(
                "transaction replacement exceeds the safety quota".into(),
            ));
        }
        // Record each missing parent durably BEFORE creating it, and record
        // what the walk actually created before propagating any error from it.
        //
        // Persisting only after the walk returned left a process-kill window
        // between `create_dir` and the journal write. A write-ahead intent is
        // safe to replay: if creation never happened there is nothing to
        // remove, and rollback removes a present directory only after file
        // restoration and only while it is empty.
        let mut created = Vec::new();
        let prefix = self.prefix.clone();
        let anchored = {
            let mut persist_creation_intent = |path: &str| {
                if self.journal.created_directories.contains_key(path) {
                    return Ok(());
                }
                if self.journal.created_directories.len() >= MAX_ARCHIVE_ENTRIES {
                    return Err(UpdateError::Transaction(
                        "transaction directory set exceeds the safety limit".into(),
                    ));
                }
                self.journal
                    .created_directories
                    .insert(path.to_string(), 0o755);
                if let Err(error) = self.persist_journal() {
                    self.journal.created_directories.remove(path);
                    return Err(error);
                }
                Ok(())
            };
            anchored_parent_recording(
                &prefix,
                relative,
                true,
                &mut created,
                &mut persist_creation_intent,
            )
        };
        if !created.is_empty() {
            // The map was already fsync-backed before each corresponding
            // create. This seam exists specifically to simulate a kill after
            // the filesystem mutation and before destination publication.
            progress(TransactionProgress::ParentDirectoriesPersisted);
        }
        let destination_parent = anchored?;
        let destination = destination_parent.destination(relative)?;
        let key = relative_string.to_ascii_lowercase();
        let mut snapshot = if let Some(preflight) = self.preflight.as_mut() {
            preflight.remove(&key).ok_or_else(|| {
                UpdateError::Transaction(format!(
                    "destination {} was not included in transaction preflight",
                    relative.display()
                ))
            })?
        } else {
            snapshot_transaction_destination(&self.prefix, relative)?
        };
        verify_transaction_destination_snapshot(&self.prefix, relative, &snapshot)?;
        let existed = snapshot.file.is_some();
        let previous_unix_mode = snapshot.previous_unix_mode;
        let (previous_size, previous_sha256) = if let Some(previous) = snapshot.file.as_mut() {
            let size = previous.metadata()?.len();
            if size > MAX_UNPACKED_BYTES {
                return Err(UpdateError::Transaction(
                    "backup exceeds the safety limit".into(),
                ));
            }
            let sha256 = sha256_open_file(previous)?;
            (Some(size), Some(sha256))
        } else {
            (None, None)
        };
        if self.journal.phase == JournalPhase::Prepared {
            self.journal.phase = JournalPhase::Applying;
        }
        self.journal.entries.push(JournalEntry {
            relative: relative_string,
            existed,
            previous_unix_mode,
            previous_size,
            previous_sha256: previous_sha256.clone(),
            replacement_size: bytes.len() as u64,
            replacement_sha256: sha256_bytes(bytes),
            state: if existed {
                JournalEntryState::BackingUp
            } else {
                JournalEntryState::Prepared
            },
        });
        self.persist_journal()?;

        if let Some(previous) = snapshot.file.as_mut() {
            let backup_relative = Path::new(&self.journal.backup_dir).join(relative);
            let (_backup_parent, backup) =
                anchored_destination(&self.prefix, &backup_relative, true)?;
            let (size, sha256) = stream_transaction_backup(previous, &backup, &mut progress)?;
            if Some(size) != previous_size || previous_sha256.as_deref() != Some(sha256.as_str()) {
                return Err(UpdateError::Transaction(
                    "transaction source changed while it was backed up".into(),
                ));
            }
            progress(TransactionProgress::BackupSynced);
            self.journal
                .entries
                .last_mut()
                .expect("entry was just appended")
                .state = JournalEntryState::Prepared;
            self.persist_journal()?;
        }
        progress(TransactionProgress::EntryPrepared);
        // The snapshot's no-write/no-delete handle protected the exact previous
        // object through quota validation and backup. Drop it only after the
        // durable backup exists; atomic_replace performs its own anchored
        // destination identity checks for the publication itself.
        drop(snapshot);
        atomic_write(&destination, bytes, unix_mode)?;
        progress(TransactionProgress::Published);
        // A process can be terminated after the durable publication but
        // before the Installed journal state is persisted. Keep this seam
        // explicit so the crash boundary remains regression-testable.
        post_publish()?;
        self.journal
            .entries
            .last_mut()
            .expect("entry was just appended")
            .state = JournalEntryState::Installed;
        self.persist_journal()?;
        Ok(())
    }

    fn persist_journal(&mut self) -> Result<(), UpdateError> {
        let bytes = serde_json::to_vec_pretty(&self.journal)?;
        atomic_write(&self.journal_path, &bytes, None)
    }

    /// Prefix-relative directories this transaction created, deduplicated and
    /// in path order, with the mode they were created with.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn created_directories(&self) -> &std::collections::BTreeMap<String, u32> {
        &self.journal.created_directories
    }

    fn rollback(&mut self) -> Result<(), UpdateError> {
        self.journal.phase = JournalPhase::RollingBack;
        let persisted = self.persist_journal();
        let restored = persisted.and_then(|()| self.restore_entries());
        // Unconditionally, and before the `?`.
        //
        // Sequencing this after two fallible steps meant it never ran in the
        // failure most likely to have caused the rollback: out of disk space
        // fails the journal write and the restore, so both `?`s returned first
        // and every directory this transaction created was left behind
        // unowned — the exact end state the mechanism exists to prevent.
        // Measured on an inode-capped filesystem: 8 recorded, 8 leaked.
        //
        // Removing empty directories is also the one rollback step that FREES
        // space and cannot itself fail for lack of it, so running it first can
        // unblock what came before rather than being blocked by it.
        self.remove_created_directories();
        restored?;
        self.finish_cleanup()
    }

    /// Undo the directory creations publishing performed, deepest first.
    ///
    /// Restoring the files is only half of "leave the prefix as we found it":
    /// their parents were created too. A left-behind directory is not just
    /// litter — it is unowned, so the next attempt sees it as pre-existing,
    /// omits it from provenance, and uninstall can never remove it.
    ///
    /// Best effort by construction. A directory that is not empty holds
    /// something this transaction did not put there (or a backup still being
    /// cleaned up), and removing it would destroy data rollback is supposed to
    /// protect; `remove_dir` refuses that case for us. Any other failure leaves
    /// the directory exactly as it is, which is the pre-existing behaviour.
    ///
    fn remove_created_directories(&self) {
        // Keep the in-memory map as well as the on-disk journal complete. Windows
        // reopens and compares the journal before deleting it, so clearing only
        // this copy would make the unchanged persisted journal look replaced.
        // Retaining both copies also preserves the full idempotent removal set if
        // recovery is interrupted while removing the deepest directories.
        let created = &self.journal.created_directories;
        // Deepest first: `share/kettle/shell-integration` has to go before
        // `share/kettle`, and reverse path order gives that for free because a
        // parent is a prefix of its children.
        for path in created.keys().rev() {
            let relative = Path::new(path);
            let Ok(parent) = anchored_parent(&self.prefix, relative, false) else {
                continue;
            };
            let Ok(target) = parent.destination(relative) else {
                continue;
            };
            let _ = fs::remove_dir(&target);
        }
    }

    fn commit(mut self) -> Result<(), UpdateError> {
        self.journal.phase = JournalPhase::Committed;
        // Keep the journal and backups until a process running the installed
        // target reaches `prepare_process_start`. A loader/startup failure then
        // leaves the last-known-good bytes available for explicit recovery.
        self.persist_journal()
    }

    fn restore_entries(&mut self) -> Result<(), UpdateError> {
        for index in (0..self.journal.entries.len()).rev() {
            match self.journal.entries[index].state {
                JournalEntryState::Restored | JournalEntryState::BackupDiscarded => continue,
                JournalEntryState::BackingUp => {
                    discard_incomplete_backup(
                        &self.prefix,
                        &self.backup_dir,
                        &self.journal.entries[index],
                    )?;
                    self.journal.entries[index].state = JournalEntryState::BackupDiscarded;
                    self.persist_journal()?;
                    continue;
                }
                JournalEntryState::Prepared | JournalEntryState::Installed => {}
            }
            restore_entry(&self.prefix, &self.backup_dir, &self.journal.entries[index])?;
            self.journal.entries[index].state = JournalEntryState::Restored;
            self.persist_journal()?;
        }
        Ok(())
    }

    fn finish_cleanup(&mut self) -> Result<(), UpdateError> {
        validate_backup_tree(&self.prefix, &self.backup_dir, &self.journal)?;
        remove_schema2_journal_checked(&self.prefix, &self.journal_path, &self.journal)?;
        // Once the journal is gone, the transaction is durably committed (or
        // rolled back). A crash during backup cleanup can leave harmless stale
        // data, but can no longer leave a journal that points at missing data.
        remove_validated_backup_tree(&self.prefix, &self.backup_dir, &self.journal)
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn confirm_committed_transaction(
    prefix: &Path,
    running_version: &semver::Version,
) -> Result<bool, UpdateError> {
    let journal_path = prefix.join(".kettle-update-journal.json");
    let bytes = match read_bounded_regular(&journal_path, 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let header: serde_json::Value = serde_json::from_slice(&bytes)?;
    if header.get("schema").and_then(serde_json::Value::as_u64) != Some(u64::from(JOURNAL_SCHEMA)) {
        return Ok(false);
    }
    let journal: Journal = serde_json::from_slice(&bytes)?;
    validate_journal(&journal)?;
    if journal.phase != JournalPhase::Committed {
        return Ok(false);
    }
    let target = semver::Version::parse(&journal.target_version)
        .map_err(|error| UpdateError::Transaction(error.to_string()))?;
    if running_version < &target {
        return Ok(false);
    }
    let backup_dir = prefix.join(&journal.backup_dir);
    validate_backup_tree(prefix, &backup_dir, &journal)?;
    let mut transaction = Transaction {
        prefix: prefix.to_path_buf(),
        journal_path,
        backup_dir,
        journal,
        preflight: None,
    };
    transaction.finish_cleanup()?;
    Ok(true)
}

#[cfg(any(windows, target_os = "linux"))]
fn recover_transaction(prefix: &Path) -> Result<(), UpdateError> {
    let journal_path = prefix.join(".kettle-update-journal.json");
    let bytes = match read_bounded_regular(&journal_path, 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let header: serde_json::Value = serde_json::from_slice(&bytes)?;
    let schema = header
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| UpdateError::Transaction("update journal has no schema".into()))?;
    if schema == 1 {
        let journal: LegacyJournal = serde_json::from_slice(&bytes)?;
        validate_legacy_journal(&journal)?;
        let backup_dir = prefix.join(&journal.backup_dir);
        validate_legacy_backup_tree(prefix, &backup_dir, &journal)?;
        rollback_legacy_journal(prefix, &journal)?;
        remove_legacy_journal_checked(prefix, &journal_path, &journal)?;
        return remove_validated_legacy_backup_tree(prefix, &backup_dir, &journal);
    }
    if schema != u64::from(JOURNAL_SCHEMA) {
        return Err(UpdateError::Transaction(format!(
            "unsupported update journal schema {schema}"
        )));
    }
    let journal: Journal = serde_json::from_slice(&bytes)?;
    validate_journal(&journal)?;
    let backup_dir = prefix.join(&journal.backup_dir);
    validate_incomplete_backup_destinations(prefix, &journal)?;
    validate_backup_tree(prefix, &backup_dir, &journal)?;
    let mut transaction = Transaction {
        prefix: prefix.to_path_buf(),
        journal_path,
        backup_dir,
        journal,
        preflight: None,
    };
    if transaction.journal.phase == JournalPhase::Committed {
        Err(UpdateError::Transaction(format!(
            "committed update {} is awaiting startup confirmation before its last-known-good backup is discarded",
            transaction.journal.target_version
        )))
    } else {
        transaction.rollback()
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn restore_entry(
    prefix: &Path,
    backup_dir: &Path,
    entry: &JournalEntry,
) -> Result<(), UpdateError> {
    let relative = Path::new(&entry.relative);
    validate_relative(relative)?;
    if !rollback_entry_requires_restore(prefix, relative, entry)? {
        return Ok(());
    }
    let (_destination_parent, destination) = anchored_destination(prefix, relative, true)?;
    if entry.existed {
        let backup_root = backup_dir.strip_prefix(prefix).map_err(|_| {
            UpdateError::Transaction("backup directory escaped the install prefix".into())
        })?;
        let backup_relative = backup_root.join(relative);
        let (_backup_parent, backup) = anchored_destination(prefix, &backup_relative, false)?;
        let (bytes, _) = read_transaction_file(&backup).map_err(|error| {
            UpdateError::Transaction(format!(
                "cannot restore backup {}: {error}",
                backup.display()
            ))
        })?;
        if Some(bytes.len() as u64) != entry.previous_size
            || entry.previous_sha256.as_deref() != Some(sha256_bytes(&bytes).as_str())
        {
            return Err(UpdateError::Transaction(format!(
                "backup integrity check failed for {}",
                backup.display()
            )));
        }
        atomic_write(&destination, &bytes, entry.previous_unix_mode)?;
    } else {
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_file() => {
                fs::remove_file(&destination)?;
                if let Some(parent) = destination.parent() {
                    sync_parent(parent)?;
                }
            }
            Ok(_) => {
                return Err(UpdateError::Transaction(format!(
                    "refusing to remove non-regular rollback destination {}",
                    destination.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn rollback_entry_requires_restore(
    prefix: &Path,
    relative: &Path,
    entry: &JournalEntry,
) -> Result<bool, UpdateError> {
    let mut current = snapshot_transaction_destination(prefix, relative)?;
    let replacement_matches = current.file.as_mut().is_some_and(|file| {
        file.metadata()
            .is_ok_and(|metadata| metadata.len() == entry.replacement_size)
            && sha256_open_file(file).is_ok_and(|hash| hash == entry.replacement_sha256)
    });
    if replacement_matches {
        return Ok(true);
    }
    if entry.state == JournalEntryState::Prepared {
        let previous_matches = transaction_destination_matches_previous(&mut current, entry);
        if previous_matches {
            // A crash can leave the write-ahead entry prepared before its
            // publication. The previous object is already the desired result.
            return Ok(false);
        }
    }
    Err(UpdateError::Transaction(format!(
        "rollback conflict for {}: current bytes are not the replacement recorded by the update",
        relative.display()
    )))
}

#[cfg(any(windows, target_os = "linux"))]
fn transaction_destination_matches_previous(
    current: &mut PreflightDestination,
    entry: &JournalEntry,
) -> bool {
    match (
        current.file.as_mut(),
        entry.previous_size,
        entry.previous_sha256.as_deref(),
    ) {
        (None, None, None) if !entry.existed => true,
        (Some(file), Some(size), Some(hash)) if entry.existed => {
            file.metadata().is_ok_and(|metadata| metadata.len() == size)
                && sha256_open_file(file).is_ok_and(|current| current == hash)
        }
        _ => false,
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn validate_incomplete_backup_destinations(
    prefix: &Path,
    journal: &Journal,
) -> Result<(), UpdateError> {
    for entry in journal.entries.iter().filter(|entry| {
        matches!(
            entry.state,
            JournalEntryState::BackingUp | JournalEntryState::BackupDiscarded
        )
    }) {
        let relative = Path::new(&entry.relative);
        let mut current = snapshot_transaction_destination(prefix, relative)?;
        if !transaction_destination_matches_previous(&mut current, entry) {
            return Err(UpdateError::Transaction(format!(
                "incomplete backup destination changed before recovery: {}",
                entry.relative
            )));
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn discard_incomplete_backup(
    prefix: &Path,
    backup_dir: &Path,
    entry: &JournalEntry,
) -> Result<(), UpdateError> {
    let relative = Path::new(&entry.relative);
    let mut current = snapshot_transaction_destination(prefix, relative)?;
    if !transaction_destination_matches_previous(&mut current, entry) {
        return Err(UpdateError::Transaction(format!(
            "refusing to discard an incomplete backup after its live destination changed: {}",
            entry.relative
        )));
    }

    let backup_root = backup_dir.strip_prefix(prefix).map_err(|_| {
        UpdateError::Transaction("backup directory escaped the install prefix".into())
    })?;
    let backup_relative = backup_root.join(relative);
    #[cfg(windows)]
    match open_windows_held_file(prefix, &backup_relative) {
        Ok(held) => {
            mark_windows_handle_for_deletion(&held.file)?;
            drop(held);
        }
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    #[cfg(target_os = "linux")]
    {
        let (_parent, backup) = match anchored_destination(prefix, &backup_relative, false) {
            Ok(backup) => backup,
            Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return remove_empty_backup_parents(prefix, backup_root, relative);
            }
            Err(error) => return Err(error),
        };
        match open_regular_nofollow(&backup) {
            Ok(file) => kettle_state::remove_open_private_file(file, &backup)?,
            Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = backup.parent() {
            sync_parent(parent)?;
        }
    }
    remove_empty_backup_parents(prefix, backup_root, relative)
}

#[cfg(any(windows, target_os = "linux"))]
fn remove_empty_backup_parents(
    prefix: &Path,
    backup_root: &Path,
    relative: &Path,
) -> Result<(), UpdateError> {
    let mut parents = Vec::new();
    let mut parent = relative.parent();
    while let Some(path) = parent {
        if path.as_os_str().is_empty() {
            break;
        }
        parents.push(path.to_path_buf());
        parent = path.parent();
    }
    for parent in parents {
        let backup_relative = backup_root.join(parent);
        let (_anchor, path) = match anchored_destination(prefix, &backup_relative, false) {
            Ok(anchored) => anchored,
            Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error),
        };
        match fs::remove_dir(&path) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    sync_parent(backup_dir_from_root(prefix, backup_root)?.as_path())
}

#[cfg(any(windows, target_os = "linux"))]
fn backup_dir_from_root(prefix: &Path, backup_root: &Path) -> Result<PathBuf, UpdateError> {
    validate_relative(backup_root)?;
    Ok(prefix.join(backup_root))
}

#[cfg(any(windows, target_os = "linux"))]
fn rollback_legacy_journal(prefix: &Path, journal: &LegacyJournal) -> Result<(), UpdateError> {
    for entry in journal.entries.iter().rev() {
        let relative = Path::new(&entry.relative);
        validate_relative(relative)?;
        let (_destination_parent, destination) = anchored_destination(prefix, relative, true)?;
        if entry.existed {
            let backup_relative = Path::new(&journal.backup_dir).join(relative);
            let (_backup_parent, backup) = anchored_destination(prefix, &backup_relative, false)?;
            let (bytes, _) = read_transaction_file(&backup).map_err(|error| {
                UpdateError::Transaction(format!(
                    "cannot restore backup {}: {error}",
                    backup.display()
                ))
            })?;
            atomic_write(&destination, &bytes, entry.previous_unix_mode)?;
        } else {
            match fs::symlink_metadata(&destination) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    fs::remove_file(&destination)?;
                    if let Some(parent) = destination.parent() {
                        sync_parent(parent)?;
                    }
                }
                Ok(_) => {
                    return Err(UpdateError::Transaction(format!(
                        "refusing to remove non-regular rollback destination {}",
                        destination.display()
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn validate_journal(journal: &Journal) -> Result<(), UpdateError> {
    if journal.schema != JOURNAL_SCHEMA
        || !is_transaction_id(&journal.transaction_id)
        || journal.backup_dir != format!(".kettle-update-backup-{}", journal.transaction_id)
        || semver::Version::parse(&journal.target_version).is_err()
        || journal.entries.len() > MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::Transaction(
            "update journal failed validation".to_string(),
        ));
    }
    if journal.created_directories.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::Transaction(
            "update journal directory set exceeds the safety limit".into(),
        ));
    }
    let mut directory_destinations = std::collections::HashSet::new();
    for (relative, mode) in &journal.created_directories {
        validate_relative(Path::new(relative))?;
        if *mode != 0o755 || !directory_destinations.insert(relative.to_ascii_lowercase()) {
            return Err(UpdateError::Transaction(format!(
                "invalid created-directory journal entry {relative}"
            )));
        }
    }
    let mut destinations = std::collections::HashSet::new();
    let mut replacement_total = 0_u64;
    let mut backup_total = 0_u64;
    let mut backup_count = 0_usize;
    for (index, entry) in journal.entries.iter().enumerate() {
        let relative = Path::new(&entry.relative);
        validate_relative(relative)?;
        if !destinations.insert(entry.relative.to_ascii_lowercase())
            || !is_sha256(&entry.replacement_sha256)
            || entry.replacement_size > MAX_UNPACKED_BYTES
            || entry.existed
                != (entry.previous_size.is_some() && entry.previous_sha256.as_deref().is_some())
            || (!entry.existed && entry.previous_unix_mode.is_some())
            || entry
                .previous_sha256
                .as_deref()
                .is_some_and(|hash| !is_sha256(hash))
            || matches!(entry.state, JournalEntryState::BackingUp)
                && (!entry.existed
                    || index + 1 != journal.entries.len()
                    || !matches!(
                        journal.phase,
                        JournalPhase::Applying | JournalPhase::RollingBack
                    ))
            || matches!(entry.state, JournalEntryState::BackupDiscarded)
                && (!entry.existed || journal.phase != JournalPhase::RollingBack)
            || matches!(entry.state, JournalEntryState::Restored)
                && journal.phase != JournalPhase::RollingBack
        {
            return Err(UpdateError::Transaction(format!(
                "invalid update journal entry {}",
                entry.relative
            )));
        }
        replacement_total = replacement_total
            .checked_add(entry.replacement_size)
            .ok_or_else(|| UpdateError::Transaction("journal size overflow".into()))?;
        if replacement_total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::Transaction(
                "journal replacement data exceeds the safety limit".into(),
            ));
        }
        if let Some(previous_size) = entry.previous_size {
            backup_count = backup_count.saturating_add(1);
            backup_total = backup_total
                .checked_add(previous_size)
                .ok_or_else(|| UpdateError::Transaction("journal backup size overflow".into()))?;
            if backup_count > MAX_ARCHIVE_ENTRIES.saturating_sub(1)
                || backup_total > MAX_UNPACKED_BYTES
            {
                return Err(UpdateError::Transaction(
                    "journal backup data exceeds the safety limit".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn validate_backup_tree(
    prefix: &Path,
    backup_dir: &Path,
    journal: &Journal,
) -> Result<(), UpdateError> {
    if backup_dir.parent() != Some(prefix)
        || backup_dir.file_name().and_then(|name| name.to_str())
            != Some(journal.backup_dir.as_str())
    {
        return Err(UpdateError::Transaction(
            "backup directory does not match the update journal".into(),
        ));
    }
    let marker_path = backup_dir.join(BACKUP_MARKER_FILE);
    let marker: BackupMarker = serde_json::from_slice(&read_bounded_regular(&marker_path, 4096)?)?;
    if marker.schema != JOURNAL_SCHEMA
        || marker.product != "kettle"
        || marker.transaction_id != journal.transaction_id
    {
        return Err(UpdateError::Transaction(
            "backup marker does not match the update journal".into(),
        ));
    }

    let mut expected = std::collections::HashMap::new();
    expected.insert(
        BACKUP_MARKER_FILE.to_ascii_lowercase(),
        (None::<u64>, None::<String>, true, false),
    );
    for entry in journal
        .entries
        .iter()
        .filter(|entry| entry.existed && entry.state != JournalEntryState::BackupDiscarded)
    {
        let size = entry.previous_size.ok_or_else(|| {
            UpdateError::Transaction(format!(
                "backup entry {} has no previous size",
                entry.relative
            ))
        })?;
        let hash = entry.previous_sha256.clone().ok_or_else(|| {
            UpdateError::Transaction(format!(
                "backup entry {} has no previous hash",
                entry.relative
            ))
        })?;
        expected.insert(
            entry.relative.to_ascii_lowercase(),
            (
                Some(size),
                Some(hash),
                entry.state != JournalEntryState::BackingUp,
                entry.state == JournalEntryState::BackingUp,
            ),
        );
    }

    let mut directories = vec![backup_dir.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(UpdateError::Transaction(format!(
                        "backup tree contains a reparse point: {}",
                        entry.path().display()
                    )));
                }
            }
            if metadata.file_type().is_dir() {
                let entry_path = entry.path();
                let relative = entry_path.strip_prefix(backup_dir).map_err(|_| {
                    UpdateError::Transaction("backup directory escaped its root".into())
                })?;
                let prefix = format!("{}/", relative_to_string(relative)?.to_ascii_lowercase());
                if !expected.keys().any(|path| path.starts_with(&prefix)) {
                    return Err(UpdateError::Transaction(format!(
                        "backup tree contains an unjournaled directory {}",
                        entry.path().display()
                    )));
                }
                directories.push(entry.path());
            } else if !metadata.file_type().is_file() {
                return Err(UpdateError::Transaction(format!(
                    "backup tree contains a non-regular entry {}",
                    entry.path().display()
                )));
            }
        }
    }

    let files = collect_files(backup_dir)?;
    if files.len() > expected.len() || files.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::Transaction(
            "backup tree does not exactly cover the update journal".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0_u64;
    for file in files {
        let relative = file.strip_prefix(backup_dir).map_err(|_| {
            UpdateError::Transaction(format!("backup escaped its root: {}", file.display()))
        })?;
        let relative = relative_to_string(relative)?;
        let key = relative.to_ascii_lowercase();
        let Some((expected_size, expected_hash, _, allow_partial)) = expected.get(&key) else {
            return Err(UpdateError::Transaction(format!(
                "backup tree contains an unjournaled file {relative}"
            )));
        };
        if !seen.insert(key) {
            return Err(UpdateError::Transaction(
                "backup tree contains a case-aliased duplicate".into(),
            ));
        }
        let size = file.metadata()?.len();
        total = total
            .checked_add(size)
            .ok_or_else(|| UpdateError::Transaction("backup size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::Transaction(
                "backup tree exceeds the safety limit".into(),
            ));
        }
        if let Some(expected_size) = expected_size {
            let invalid = if *allow_partial {
                size > *expected_size
            } else {
                size != *expected_size
                    || expected_hash.as_deref() != Some(sha256_file(&file)?.as_str())
            };
            if invalid {
                return Err(UpdateError::Transaction(format!(
                    "backup integrity check failed for {}",
                    file.display()
                )));
            }
        }
    }
    if expected
        .iter()
        .any(|(path, (_, _, required, _))| *required && !seen.contains(path))
    {
        return Err(UpdateError::Transaction(
            "backup tree does not cover every durable journal entry".into(),
        ));
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn remove_validated_backup_tree(
    prefix: &Path,
    backup_dir: &Path,
    journal: &Journal,
) -> Result<(), UpdateError> {
    let backup_root = backup_dir.strip_prefix(prefix).map_err(|_| {
        UpdateError::Transaction("backup directory escaped the install prefix".into())
    })?;
    #[cfg(windows)]
    {
        let mut tree = hold_windows_two_level_tree(prefix, backup_root)?;
        let mut expected = journal
            .entries
            .iter()
            .filter(|entry| entry.existed && entry.state != JournalEntryState::BackupDiscarded)
            .map(|entry| {
                (
                    entry.relative.to_ascii_lowercase(),
                    (
                        entry.relative.as_str(),
                        entry.previous_size,
                        entry.previous_sha256.as_deref(),
                    ),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        expected.insert(
            BACKUP_MARKER_FILE.to_ascii_lowercase(),
            (BACKUP_MARKER_FILE, None, None),
        );
        if tree.files.len() != expected.len() {
            return Err(UpdateError::Transaction(
                "backup tree changed before cleanup".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (relative, held) in &mut tree.files {
            let spelling = relative_to_string(relative)?;
            let key = spelling.to_ascii_lowercase();
            let Some((expected_spelling, expected_size, expected_hash)) = expected.get(&key) else {
                return Err(UpdateError::Transaction(format!(
                    "backup tree gained an unmanaged file before cleanup: {spelling}"
                )));
            };
            if spelling != *expected_spelling || !seen.insert(key) {
                return Err(UpdateError::Transaction(
                    "backup tree changed spelling before cleanup".into(),
                ));
            }
            if let Some(size) = expected_size {
                let hash_matches = match expected_hash {
                    Some(hash) => sha256_open_file(&mut held.file)? == *hash,
                    None => false,
                };
                if held.file.metadata()?.len() != *size || !hash_matches {
                    return Err(UpdateError::Transaction(format!(
                        "backup integrity check failed before cleanup for {}",
                        held.path.display()
                    )));
                }
            }
        }
        tree.delete()?;
        sync_parent(prefix)
    }
    #[cfg(not(windows))]
    {
        let mut directories = std::collections::BTreeSet::new();
        for entry in journal
            .entries
            .iter()
            .filter(|entry| entry.existed && entry.state != JournalEntryState::BackupDiscarded)
        {
            let relative = Path::new(&entry.relative);
            let backup_relative = backup_root.join(relative);
            let (_parent, backup) = anchored_destination(prefix, &backup_relative, false)?;
            match fs::remove_file(&backup) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let mut parent = relative.parent();
            while let Some(path) = parent {
                if path.as_os_str().is_empty() {
                    break;
                }
                directories.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
        for relative in directories.into_iter().rev() {
            let path = backup_dir.join(relative);
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        fs::remove_file(backup_dir.join(BACKUP_MARKER_FILE))?;
        fs::remove_dir(backup_dir)?;
        sync_parent(prefix)
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn validate_legacy_journal(journal: &LegacyJournal) -> Result<(), UpdateError> {
    if journal.schema != 1
        || !journal.backup_dir.starts_with(".kettle-update-backup-")
        || journal.backup_dir.contains(['/', '\\'])
        || journal.entries.len() > MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::Transaction(
            "legacy update journal failed validation".into(),
        ));
    }
    let mut destinations = std::collections::HashSet::new();
    for entry in &journal.entries {
        validate_relative(Path::new(&entry.relative))?;
        if !destinations.insert(entry.relative.to_ascii_lowercase()) {
            return Err(UpdateError::Transaction(
                "legacy update journal has duplicate destinations".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn validate_legacy_backup_tree(
    prefix: &Path,
    backup_dir: &Path,
    journal: &LegacyJournal,
) -> Result<(), UpdateError> {
    if backup_dir.parent() != Some(prefix)
        || backup_dir.file_name().and_then(|name| name.to_str())
            != Some(journal.backup_dir.as_str())
    {
        return Err(UpdateError::Transaction(
            "legacy backup directory escaped the install prefix".into(),
        ));
    }
    let expected = journal
        .entries
        .iter()
        .filter(|entry| entry.existed)
        .map(|entry| {
            let relative = Path::new(&entry.relative);
            validate_relative(relative)?;
            Ok((entry.relative.to_ascii_lowercase(), entry.relative.as_str()))
        })
        .collect::<Result<std::collections::HashMap<_, _>, UpdateError>>()?;
    if expected.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::Transaction(
            "legacy backup exceeds the entry limit".into(),
        ));
    }

    let mut pending = vec![backup_dir.to_path_buf()];
    let mut seen = std::collections::HashSet::new();
    let mut total = 0_u64;
    while let Some(directory) = pending.pop() {
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.file_type().is_dir() {
            return Err(UpdateError::Transaction(format!(
                "legacy backup contains a non-directory ancestor {}",
                directory.display()
            )));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(UpdateError::Transaction(format!(
                    "legacy backup contains a reparse point {}",
                    directory.display()
                )));
            }
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(UpdateError::Transaction(format!(
                        "legacy backup contains a reparse point {}",
                        path.display()
                    )));
                }
            }
            let relative = path.strip_prefix(backup_dir).map_err(|_| {
                UpdateError::Transaction("legacy backup entry escaped its root".into())
            })?;
            let relative_string = relative_to_string(relative)?;
            let key = relative_string.to_ascii_lowercase();
            if metadata.file_type().is_dir() {
                let prefix = format!("{key}/");
                if !expected
                    .keys()
                    .any(|candidate| candidate.starts_with(&prefix))
                {
                    return Err(UpdateError::Transaction(format!(
                        "legacy backup contains an unjournaled directory {}",
                        path.display()
                    )));
                }
                pending.push(path);
            } else if metadata.file_type().is_file() {
                let Some(expected_spelling) = expected.get(&key) else {
                    return Err(UpdateError::Transaction(format!(
                        "legacy backup contains an unjournaled file {}",
                        path.display()
                    )));
                };
                if *expected_spelling != relative_string || !seen.insert(key) {
                    return Err(UpdateError::Transaction(format!(
                        "legacy backup contains a case alias or duplicate {}",
                        path.display()
                    )));
                }
                total = total.checked_add(metadata.len()).ok_or_else(|| {
                    UpdateError::Transaction("legacy backup size overflow".into())
                })?;
                if total > MAX_UNPACKED_BYTES {
                    return Err(UpdateError::Transaction(
                        "legacy backup exceeds the size limit".into(),
                    ));
                }
            } else {
                return Err(UpdateError::Transaction(format!(
                    "legacy backup contains a non-regular entry {}",
                    path.display()
                )));
            }
        }
    }
    if seen.len() != expected.len() {
        return Err(UpdateError::Transaction(
            "legacy backup does not exactly cover its journal".into(),
        ));
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn remove_validated_legacy_backup_tree(
    prefix: &Path,
    backup_dir: &Path,
    journal: &LegacyJournal,
) -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        let root_relative = backup_dir.strip_prefix(prefix).map_err(|_| {
            UpdateError::Transaction("legacy backup directory escaped the install prefix".into())
        })?;
        let tree = hold_windows_two_level_tree(prefix, root_relative)?;
        let expected = journal
            .entries
            .iter()
            .filter(|entry| entry.existed)
            .map(|entry| (entry.relative.to_ascii_lowercase(), entry.relative.as_str()))
            .collect::<std::collections::HashMap<_, _>>();
        if tree.files.len() != expected.len() {
            return Err(UpdateError::Transaction(
                "legacy backup tree changed before cleanup".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (relative, _) in &tree.files {
            let spelling = relative_to_string(relative)?;
            let key = spelling.to_ascii_lowercase();
            if expected.get(&key).copied() != Some(spelling.as_str()) || !seen.insert(key) {
                return Err(UpdateError::Transaction(
                    "legacy backup tree changed before cleanup".into(),
                ));
            }
        }
        tree.delete()?;
        sync_parent(prefix)
    }
    #[cfg(not(windows))]
    {
        let mut directories = std::collections::BTreeSet::new();
        for entry in journal.entries.iter().filter(|entry| entry.existed) {
            let relative = Path::new(&entry.relative);
            let backup = backup_dir.join(relative);
            match fs::remove_file(&backup) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let mut parent = relative.parent();
            while let Some(path) = parent {
                if path.as_os_str().is_empty() {
                    break;
                }
                directories.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
        for relative in directories.into_iter().rev() {
            match fs::remove_dir(backup_dir.join(relative)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        fs::remove_dir(backup_dir)?;
        sync_parent(prefix)
    }
}

#[cfg(windows)]
fn remove_held_json_checked<T>(prefix: &Path, path: &Path, expected: &T) -> Result<(), UpdateError>
where
    T: serde::de::DeserializeOwned + PartialEq,
{
    if path.parent() != Some(prefix) {
        return Err(UpdateError::Transaction(format!(
            "state file escaped the install prefix: {}",
            path.display()
        )));
    }
    let relative = Path::new(
        path.file_name()
            .ok_or_else(|| UpdateError::Transaction("state file has no name".into()))?,
    );
    let mut held = match open_windows_held_file(prefix, relative) {
        Ok(held) => held,
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let actual: T = serde_json::from_slice(&read_windows_held_file(
        &mut held,
        MAX_PENDING_RECORD_BYTES,
    )?)?;
    if &actual != expected {
        return Err(UpdateError::Transaction(format!(
            "state file changed before deletion: {}",
            path.display()
        )));
    }
    mark_windows_handle_for_deletion(&held.file)?;
    drop(held);
    sync_parent(prefix)
}

#[cfg(windows)]
fn remove_pending_record_checked(
    prefix: &Path,
    pending: &PendingUpdate,
) -> Result<(), UpdateError> {
    remove_held_json_checked(prefix, &prefix.join(PENDING_FILE), pending)
}

#[cfg(any(windows, target_os = "linux"))]
fn remove_schema2_journal_checked(
    prefix: &Path,
    journal_path: &Path,
    journal: &Journal,
) -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        remove_held_json_checked(prefix, journal_path, journal)
    }
    #[cfg(not(windows))]
    {
        let _ = journal;
        remove_journal_path(prefix, journal_path)
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn remove_legacy_journal_checked(
    prefix: &Path,
    journal_path: &Path,
    journal: &LegacyJournal,
) -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        remove_held_json_checked(prefix, journal_path, journal)
    }
    #[cfg(not(windows))]
    {
        let _ = journal;
        remove_journal_path(prefix, journal_path)
    }
}

#[cfg(target_os = "linux")]
fn remove_journal_path(prefix: &Path, journal_path: &Path) -> Result<(), UpdateError> {
    match fs::remove_file(journal_path) {
        Ok(()) => sync_parent(prefix),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(any(windows, target_os = "linux"))]
fn is_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(any(windows, target_os = "linux"))]
fn validate_relative(path: &Path) -> Result<(), UpdateError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(UpdateError::Transaction(format!(
            "unsafe install path {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn relative_to_string(path: &Path) -> Result<String, UpdateError> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| UpdateError::Transaction("non-UTF-8 install path".into())),
            _ => Err(UpdateError::Transaction("unsafe install path".into())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts.join("/"))
}

fn atomic_write(
    destination: &Path,
    bytes: &[u8],
    unix_mode: Option<u32>,
) -> Result<(), UpdateError> {
    kettle_state::atomic_replace(
        destination,
        bytes,
        kettle_state::AtomicWriteOptions {
            unix_mode: unix_mode.unwrap_or(0o600),
            preserve_permissions: unix_mode.is_none(),
            reject_symlink: true,
        },
    )
    .map_err(UpdateError::from)
}

#[cfg(all(any(windows, target_os = "linux"), unix))]
fn set_private_directory(path: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(all(any(windows, target_os = "linux"), not(unix)))]
fn set_private_directory(_path: &Path) -> Result<(), UpdateError> {
    Ok(())
}

/// Atomically persist a small state file, replacing any existing destination.
///
/// The temporary file is created beside the destination so the final rename
/// stays on one filesystem. Data and the containing directory are synced
/// before success is reported.
pub fn write_atomic_file(destination: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    atomic_write(destination, bytes, None)
}

#[cfg(all(any(windows, target_os = "linux"), unix))]
fn sync_parent(parent: &Path) -> Result<(), UpdateError> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(all(any(windows, target_os = "linux"), not(unix)))]
fn sync_parent(_parent: &Path) -> Result<(), UpdateError> {
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn remove_new_backup_dir_checked(prefix: &Path, path: &Path) -> Result<(), UpdateError> {
    let transaction_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(".kettle-update-backup-"));
    if path.parent() != Some(prefix) || !transaction_id.is_some_and(is_transaction_id) {
        return Err(UpdateError::Transaction(format!(
            "refusing to remove untrusted path {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        let root_relative = PathBuf::from(
            path.file_name()
                .ok_or_else(|| UpdateError::Transaction("backup path has no name".into()))?,
        );
        let mut tree = match hold_windows_two_level_tree(prefix, &root_relative) {
            Ok(tree) => tree,
            Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if tree.nested.is_some() || tree.files.len() > 1 {
            return Err(UpdateError::Transaction(
                "new backup contains unexpected cleanup state".into(),
            ));
        }
        if let Some((relative, marker)) = tree.files.first_mut() {
            if relative != Path::new(BACKUP_MARKER_FILE) {
                return Err(UpdateError::Transaction(format!(
                    "new backup contains unexpected cleanup state: {}",
                    marker.path.display()
                )));
            }
            let marker_value: BackupMarker =
                serde_json::from_slice(&read_windows_held_file(marker, 4096)?)?;
            if marker_value.schema != JOURNAL_SCHEMA
                || marker_value.product != "kettle"
                || Some(marker_value.transaction_id.as_str()) != transaction_id
            {
                return Err(UpdateError::Transaction(
                    "new backup marker does not match its directory".into(),
                ));
            }
        }
        tree.delete()?;
        sync_parent(prefix)
    }
    #[cfg(not(windows))]
    {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_dir() {
            return Err(UpdateError::Transaction(format!(
                "new backup path is not a real directory: {}",
                path.display()
            )));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(UpdateError::Transaction(format!(
                    "new backup path is a reparse point: {}",
                    path.display()
                )));
            }
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(UpdateError::Transaction(format!(
                        "new backup cleanup entry is a reparse point: {}",
                        entry.path().display()
                    )));
                }
            }
            if entry.file_name() != BACKUP_MARKER_FILE || !metadata.file_type().is_file() {
                return Err(UpdateError::Transaction(format!(
                    "new backup contains unexpected cleanup state: {}",
                    entry.path().display()
                )));
            }
            fs::remove_file(entry.path())?;
        }
        fs::remove_dir(path)?;
        sync_parent(prefix)
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )
}

#[cfg(windows)]
fn current_epoch_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(any(windows, target_os = "linux"))]
fn is_transaction_id(value: &str) -> bool {
    transaction_id_parts(value).is_some()
}

#[cfg(any(windows, target_os = "linux"))]
fn transaction_id_parts(value: &str) -> Option<(u32, u128)> {
    let (process_id, epoch_nanos) = value.split_once('-')?;
    let process_id_value = process_id.parse::<u32>().ok()?;
    let epoch_nanos_value = epoch_nanos.parse::<u128>().ok()?;
    (process_id_value.to_string() == process_id && epoch_nanos_value.to_string() == epoch_nanos)
        .then_some((process_id_value, epoch_nanos_value))
}

#[cfg(windows)]
fn refresh_platform_integration(install: &ManagedInstall, verified_script: &[u8]) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = install.prefix.join("install.ps1");
    let Ok(retained_script) = retain_verified_integration_script(&script, verified_script) else {
        log::warn!(
            "could not retain the verified post-update integration script; skipping integration refresh"
        );
        return;
    };
    let Some(powershell) = system_powershell_path() else {
        log::warn!(
            "could not resolve a fully-qualified PowerShell path; skipping the post-update integration refresh"
        );
        return;
    };
    let _ = std::process::Command::new(powershell)
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-RefreshIntegration")
        .arg("-Prefix")
        .arg(&install.prefix)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    drop(retained_script);
}

#[cfg(windows)]
fn retain_verified_integration_script(
    script: &Path,
    verified_script: &[u8],
) -> Result<File, UpdateError> {
    let (mut retained, _) = open_transaction_snapshot(script)?;
    if retained.metadata()?.len() != verified_script.len() as u64
        || sha256_open_file(&mut retained)? != sha256_bytes(verified_script)
    {
        return Err(UpdateError::Transaction(
            "installed integration script does not match the verified update bytes".into(),
        ));
    }
    Ok(retained)
}

/// Resolves `powershell.exe` by a fixed, fully-qualified system path instead
/// of letting `Command::new` search for a bare name. `CreateProcess`'s
/// default search order tries the spawning process's own application
/// directory and its current working directory before PATH, so a same-user
/// attacker able to write into either (a much weaker position than
/// compromising this process's environment) could otherwise have this
/// authenticated self-update step execute an arbitrary planted binary.
/// The kernel-reported system directory is not derived from PATH, the current
/// directory, or caller-controlled environment variables.
#[cfg(windows)]
fn system_powershell_path() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0_u16; 32_768];
    // SAFETY: `buffer` is writable for its full reported u32 length.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    buffer.truncate(length);
    let system = PathBuf::from(std::ffi::OsString::from_wide(&buffer));
    let candidate = system.join(r"WindowsPowerShell\v1.0\powershell.exe");
    candidate.is_file().then_some(candidate)
}

#[cfg(target_os = "linux")]
fn refresh_platform_integration(install: &ManagedInstall) {
    if let Some(tool) = system_tool_path("update-desktop-database") {
        let _ = std::process::Command::new(tool)
            .arg(install.prefix.join("share/applications"))
            .status();
    }
    let icon_root = install.prefix.join("share/icons/hicolor");
    if icon_root.join("index.theme").is_file()
        && let Some(tool) = system_tool_path("gtk-update-icon-cache")
    {
        let _ = std::process::Command::new(tool)
            .args(["-f", "-t"])
            .arg(icon_root)
            .status();
    }
}

/// The absolute directories desktop-integration tools are installed to on
/// mainstream Linux distributions, in lookup order.
#[cfg(target_os = "linux")]
const SYSTEM_TOOL_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/bin"];

/// Resolves a desktop-integration tool by a fixed absolute path instead of
/// letting `Command::new` search PATH for a bare name, so a same-user
/// attacker who can write into an earlier PATH entry cannot have this
/// authenticated self-update step run a planted binary that merely shares
/// one of these well-known tool names. Silently skipping when none of the
/// allowlisted directories has the tool matches this call site's existing
/// best-effort behavior (its own status codes are already ignored).
#[cfg(target_os = "linux")]
fn system_tool_path(name: &str) -> Option<PathBuf> {
    system_tool_path_in(SYSTEM_TOOL_DIRS, name)
}

/// Core of [`system_tool_path`], parameterized on the candidate directories
/// so tests can exercise the allowlist/ordering behavior against a synthetic
/// directory set instead of depending on which desktop-integration tools
/// happen to be installed on the machine running the test.
#[cfg(target_os = "linux")]
fn system_tool_path_in(dirs: &[&str], name: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    use base64::Engine as _;
    #[cfg(windows)]
    use ed25519_dalek::{Signer as _, SigningKey};

    fn test_tempdir() -> kettle_test_support::PrivateTempDir {
        kettle_test_support::private_tempdir("kettle-update-test-")
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_retry_is_narrow_and_recovers_after_a_transient_lock() {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

        let mut attempts = 0_u8;
        let value = retry_windows_sharing_violation(|| {
            attempts += 1;
            if attempts < 3 {
                Err(std::io::Error::from_raw_os_error(
                    ERROR_SHARING_VIOLATION as i32,
                ))
            } else {
                Ok(42)
            }
        })
        .expect("a transient sharing violation should be retried");
        assert_eq!(value, 42);
        assert_eq!(attempts, 3);

        let mut permanent_attempts = 0_u8;
        let error = retry_windows_sharing_violation(|| -> std::io::Result<()> {
            permanent_attempts += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "not a sharing violation",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            permanent_attempts, 1,
            "unrelated errors must not be retried"
        );
    }

    /// Create fixture directories with the public installer mode instead of
    /// inheriting the test runner's umask.
    #[cfg(target_os = "linux")]
    fn create_linux_install_dir_all(prefix: &Path, path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        fs::create_dir_all(path).unwrap();
        let relative = path
            .strip_prefix(prefix)
            .expect("fixture install directory must stay beneath its prefix");
        let mut current = prefix.to_path_buf();
        fs::set_permissions(&current, fs::Permissions::from_mode(0o755)).unwrap();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                panic!("fixture install directory must be normalized");
            };
            current.push(name);
            fs::set_permissions(&current, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn fake_update() -> AvailableUpdate {
        AvailableUpdate {
            version: semver::Version::new(99, 0, 0),
            tag: "v99.0.0".into(),
            release_url: "https://example.invalid/release".into(),
            asset: Some(crate::ManifestAsset {
                target: current_target().unwrap_or("unsupported").into(),
                name: if cfg!(windows) {
                    "kettle-windows-x86_64.zip"
                } else {
                    "kettle-linux-x86_64.tar.gz"
                }
                .into(),
                size: 1,
                sha256: "0".repeat(64),
            }),
            download_url: Some("https://example.invalid/archive".into()),
            signed_manifest: None,
        }
    }

    #[cfg(target_os = "linux")]
    fn seed_linux_install_provenance(prefix: &Path) {
        seed_linux_install_provenance_with(prefix, &[]);
    }

    /// Seed a valid provenance record, plus `extra` files that a previous
    /// release owned. Use `extra` to represent something the NEW archive no
    /// longer ships.
    #[cfg(target_os = "linux")]
    fn seed_linux_install_provenance_with(prefix: &Path, extra: &[(&str, &[u8], u32)]) {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        create_linux_install_dir_all(prefix, prefix);
        for (relative, contents, mode) in extra.iter().copied() {
            let path = prefix.join(relative);
            create_linux_install_dir_all(prefix, path.parent().unwrap());
            fs::write(&path, contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        for (relative, contents, mode) in [
            ("bin/kettle", b"fixture".as_slice(), 0o755),
            (
                "share/applications/kettle.desktop",
                b"[Desktop Entry]\nExec=fixture\n".as_slice(),
                0o644,
            ),
            ("share/kettle/install.sh", b"#!/bin/sh\n".as_slice(), 0o755),
            (
                "share/kettle/install-unix.py",
                b"#!/usr/bin/env python3\n".as_slice(),
                0o755,
            ),
            // Listed in `paths` below, so create it here too: leaving it to the
            // caller makes this helper panic in `set_permissions` the first time
            // it is reused by a test that does not already seed the file.
            ("share/kettle/install.json", b"{}\n".as_slice(), 0o644),
        ] {
            let path = prefix.join(relative);
            create_linux_install_dir_all(prefix, path.parent().unwrap());
            if !path.exists() {
                fs::write(&path, contents).unwrap();
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let paths = [
            ("bin/kettle", 0o755),
            ("share/applications/kettle.desktop", 0o644),
            ("share/kettle/install.sh", 0o755),
            ("share/kettle/install-unix.py", 0o755),
            ("share/kettle/install.json", 0o644),
        ];
        let mut files = paths
            .into_iter()
            .chain(extra.iter().map(|(relative, _, mode)| (*relative, *mode)))
            .map(|(relative, mode)| {
                let path = prefix.join(relative);
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
                UnixInstallFile {
                    path: relative.into(),
                    size: path.metadata().unwrap().len(),
                    sha256: sha256_file(&path).unwrap(),
                    mode,
                }
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let provenance = UnixInstallProvenance {
            schema: 1,
            product: "kettle".into(),
            managed_by: "kettle-installer".into(),
            prefix: prefix.canonicalize().unwrap().to_str().unwrap().into(),
            owner_uid: prefix.metadata().unwrap().uid(),
            files,
            directories: Vec::new(),
        };
        let path = prefix.join(UNIX_INSTALL_PROVENANCE_FILE);
        fs::write(
            &path,
            serde_json::to_string_pretty(&provenance).unwrap() + "\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn interrupted_managed_linux_install() -> (kettle_test_support::PrivateTempDir, PathBuf, PathBuf)
    {
        let root = test_tempdir();
        let prefix = root.path().join("kettle");
        let executable = prefix.join("bin/kettle");
        let marker = prefix.join("share/kettle/install.json");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, marker_json("2.35.0").unwrap()).unwrap();
        seed_linux_install_provenance(&prefix);

        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        let stopped = transaction
            .install_bytes_with_post_publish(
                Path::new("share/kettle/install.sh"),
                b"updated installer\n",
                Some(0o755),
                || Err(UpdateError::Transaction("simulated process stop".into())),
            )
            .unwrap_err();
        assert!(stopped.to_string().contains("simulated process stop"));
        std::mem::forget(transaction);
        assert!(detect_managed_install_at(&executable).is_err());
        assert!(prefix.join(".kettle-update-journal.json").is_file());
        (root, prefix, executable)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_startup_and_update_entrypoints_recover_before_provenance_verification() {
        let (_startup_root, startup_prefix, startup_executable) =
            interrupted_managed_linux_install();
        prepare_linux_process_start_at(&startup_executable, &semver::Version::new(2, 35, 0))
            .unwrap();
        assert_eq!(
            fs::read(startup_prefix.join("share/kettle/install.sh")).unwrap(),
            b"#!/bin/sh\n"
        );
        detect_managed_install_at(&startup_executable)
            .expect("startup recovery must restore a managed installation");

        let (_update_root, update_prefix, update_executable) = interrupted_managed_linux_install();
        let install = locate_managed_install_at(&update_executable)
            .expect("structural detection must remain available during recovery");
        let _lock = prepare_update_transaction(&install, &semver::Version::new(2, 35, 0))
            .expect("the next update must recover before checking content provenance");
        assert_eq!(
            fs::read(update_prefix.join("share/kettle/install.sh")).unwrap(),
            b"#!/bin/sh\n"
        );
        detect_managed_install_at(&update_executable)
            .expect("update recovery must restore a managed installation");
    }

    #[test]
    fn marker_is_explicit_and_target_bound() {
        if let Some(target) = current_target() {
            let text = marker_json("2.35.0").unwrap();
            let marker: InstallMarker = serde_json::from_str(&text).unwrap();
            assert_eq!(marker.managed_by, "kettle-installer");
            assert_eq!(marker.target, target);
            assert_eq!(marker.version, "2.35.0");
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn managed_install_accepts_stable_and_explains_local_development_channels() {
        let root = test_tempdir();
        #[cfg(windows)]
        let (prefix, executable, marker_path) = {
            let prefix = root.path().join("kettle");
            fs::create_dir_all(&prefix).unwrap();
            (
                prefix.clone(),
                prefix.join("kettle.exe"),
                prefix.join(".kettle-install.json"),
            )
        };
        #[cfg(target_os = "linux")]
        let (prefix, executable, marker_path) = {
            let prefix = root.path().join("kettle");
            fs::create_dir_all(prefix.join("bin")).unwrap();
            (
                prefix.clone(),
                prefix.join("bin/kettle"),
                prefix.join("share/kettle/install.json"),
            )
        };
        fs::write(&executable, b"fixture").unwrap();
        fs::create_dir_all(marker_path.parent().unwrap()).unwrap();
        let mut marker: InstallMarker =
            serde_json::from_str(&marker_json("2.35.0").unwrap()).unwrap();
        fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
        #[cfg(target_os = "linux")]
        seed_linux_install_provenance(&prefix);
        let detected = detect_managed_install_at(&executable).unwrap();
        assert_eq!(detected.prefix, prefix.canonicalize().unwrap());

        for channel in ["local-dev", "local-dev-record"] {
            marker.channel = channel.into();
            fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
            let error = detect_managed_install_at(&executable).unwrap_err();
            assert!(error.to_string().contains("local development install"));
            assert!(error.to_string().contains("rebuild and reinstall"));
        }

        // `version` was the one field written and never checked, and it is the
        // one a person reads: `install.json` is what support instructions and
        // packaging scripts consult for "what is installed here". A marker
        // carrying a version no kettle installer would write is not this
        // build's marker.
        marker.channel = "stable".into();
        // `unknown` is what the installers write when they cannot determine a
        // version, so it has to verify. Refusing it reported those
        // installations as unmanaged and broke `kettle update` for them
        // outright — see `scripts/install-unix.py`, which permits exactly this
        // string, and `scripts/install.ps1`, which substitutes it.
        //
        // On Linux the marker is itself a provenance-recorded file, so
        // rewriting it invalidates the record that was seeded from its old
        // bytes; re-seed so this measures the version rule and not a stale
        // hash. (The negative cases below did not need it — they were already
        // expected to fail, which is how a positive assertion here catches
        // what they could not.)
        marker.version = "unknown".into();
        fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
        #[cfg(target_os = "linux")]
        seed_linux_install_provenance(&prefix);
        if let Err(error) = detect_managed_install_at(&executable) {
            panic!("an installer-written `unknown` version must stay managed: {error}");
        }
        // The rule itself, so both halves are pinned independently of the
        // filesystem fixture above.
        assert!(is_recorded_install_version("unknown"));
        assert!(is_recorded_install_version("2.46.0"));
        assert!(is_recorded_install_version("2.46.0-rc.1"));
        for rejected in ["", "Unknown", "unknown ", "2.35", "v2.35.0", "latest"] {
            assert!(
                !is_recorded_install_version(rejected),
                "{rejected:?} is not something a kettle installer writes"
            );
        }
        for bogus in ["", "not-a-version", "2.35", "v2.35.0", "../../etc/passwd"] {
            marker.version = bogus.into();
            fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
            let error = detect_managed_install_at(&executable).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("does not match this kettle build"),
                "version {bogus:?} must be refused, got {error}"
            );
        }
        // And a real one is still accepted, so the check is not simply "no".
        marker.version = "2.35.0".into();
        fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
        // Re-seed for the same reason as above: on Linux the marker is a
        // provenance-recorded file, so rewriting it invalidates the record
        // that was seeded from its previous bytes. Without this the assertion
        // failed on Linux for a reason that had nothing to do with the version.
        #[cfg(target_os = "linux")]
        seed_linux_install_provenance(&prefix);
        if let Err(error) = detect_managed_install_at(&executable) {
            panic!("a real semver must stay managed: {error}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn managed_install_prefix_rejects_group_or_other_write_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = test_tempdir();
        let prefix = root.path().join("kettle");
        fs::create_dir(&prefix).unwrap();
        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o775)).unwrap();
        let error = open_trusted_linux_install_prefix(&prefix).unwrap_err();
        assert!(error.to_string().contains("group/other writable"));

        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755)).unwrap();
        open_trusted_linux_install_prefix(&prefix).unwrap();
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    #[test]
    fn unsupported_platform_has_no_managed_installer() {
        assert!(matches!(
            detect_managed_install(),
            Err(UpdateError::UnsupportedPlatform)
        ));
    }

    /// macOS used to answer `UnsupportedPlatform` here, because it had no
    /// managed-install path at all. It has one now, so the meaningful assertion
    /// is the narrower one: a bundle is required, and the test harness is not
    /// running inside one.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bare_test_binary_is_not_a_managed_bundle_install() {
        let error = detect_managed_install().unwrap_err();
        assert!(
            matches!(error, UpdateError::UnmanagedInstall(_)),
            "expected an unmanaged-install refusal, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("kettle.app/Contents/MacOS/kettle"),
            "the refusal should say what layout is expected, got: {error}"
        );
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn archive_paths_reject_traversal_and_platform_tricks() {
        for bad in [
            "../kettle",
            "/kettle",
            "kettle/..",
            "kettle/file:stream",
            "kettle/trailing.",
            "kettle/CON.txt",
            "kettle/com1",
            "kettle/Lpt9.log",
        ] {
            assert!(
                validate_archive_path(Path::new(bad)).is_err(),
                "accepted {bad}"
            );
        }
        assert!(validate_archive_path(Path::new("kettle/shell-integration/kettle.ps1")).is_ok());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn transaction_ids_are_exact_pid_and_epoch_decimal_pairs() {
        for valid in [
            "0-0",
            "123-456",
            "4294967295-340282366920938463463374607431768211455",
        ] {
            assert!(is_transaction_id(valid), "rejected {valid}");
        }
        for invalid in [
            "",
            "123",
            "-456",
            "123-",
            "123-456-789",
            "pid-456",
            "123-nanos",
            "00-0",
            "0-00",
            "4294967296-1",
            "1-340282366920938463463374607431768211456",
            "123_456",
            " 123-456",
        ] {
            assert!(!is_transaction_id(invalid), "accepted {invalid}");
        }

        let root = test_tempdir();
        let transaction =
            Transaction::begin_with_transaction_id(root.path(), "99.0.0", "123-456").unwrap();
        assert_eq!(
            transaction
                .backup_dir
                .file_name()
                .and_then(|name| name.to_str()),
            Some(".kettle-update-backup-123-456")
        );
        transaction.commit().unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn new_backup_cleanup_preserves_unexpected_state() {
        let root = test_tempdir();
        let backup = root.path().join(".kettle-update-backup-123-456");
        fs::create_dir(&backup).unwrap();
        let sentinel = backup.join("user-data");
        fs::write(&sentinel, b"must survive").unwrap();

        let error = remove_new_backup_dir_checked(root.path(), &backup).unwrap_err();
        assert!(error.to_string().contains("unexpected cleanup state"));
        assert_eq!(fs::read(sentinel).unwrap(), b"must survive");
    }

    #[cfg(windows)]
    #[test]
    fn windows_payload_paths_are_an_exact_two_level_grammar() {
        for valid in [
            "kettle.exe",
            "README.md",
            "kettle-package-manifest.json",
            "shell-integration/kettle.bash",
            "shell-integration/kettle.fish",
            "shell-integration/kettle.ps1",
            "shell-integration/kettle.zsh",
        ] {
            assert!(is_allowed_windows_payload_path(Path::new(valid)));
        }
        for invalid in [
            "KETTLE.EXE",
            ".kettle-install.json",
            "shell-integration",
            "shell-integration/extra.ps1",
            "shell-integration/nested/kettle.ps1",
            "docs/README.md",
        ] {
            assert!(!is_allowed_windows_payload_path(Path::new(invalid)));
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn archive_path_set_rejects_case_aliases_and_file_prefixes() {
        let mut paths = ArchivePaths::default();
        paths.insert(Path::new("kettle/README.md"), false).unwrap();
        assert!(paths.insert(Path::new("KETTLE/readme.MD"), false).is_err());

        let mut paths = ArchivePaths::default();
        paths
            .insert(Path::new("kettle/shell-integration"), false)
            .unwrap();
        assert!(
            paths
                .insert(Path::new("kettle/shell-integration/kettle.zsh"), false)
                .is_err()
        );

        let mut paths = ArchivePaths::default();
        paths
            .insert(Path::new("kettle/shell-integration/kettle.zsh"), false)
            .unwrap();
        assert!(
            paths
                .insert(Path::new("kettle/shell-integration"), false)
                .is_err()
        );
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn write_test_package_manifest(root: &Path, version: &str) {
        let _ = fs::remove_file(root.join(PACKAGE_MANIFEST_FILE));
        let files = collect_files(root)
            .unwrap()
            .into_iter()
            .map(|path| {
                let metadata = fs::metadata(&path).unwrap();
                PackageFile {
                    path: relative_to_string(path.strip_prefix(root).unwrap()).unwrap(),
                    size: metadata.len(),
                    sha256: sha256_file(&path).unwrap(),
                    mode: package_mode(&metadata),
                }
            })
            .collect();
        let manifest = PackageManifest {
            schema: 1,
            product: "kettle".into(),
            target: current_target().unwrap().into(),
            version: version.into(),
            files,
        };
        fs::write(
            root.join(PACKAGE_MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn package_manifest_is_mandatory_from_v2_36_onward() {
        let root = test_tempdir();
        let mut update = fake_update();

        update.version = semver::Version::new(2, 35, 99);
        verify_required_package_manifest(root.path(), &update)
            .expect("legacy signed archives remain compatible without an inner manifest");

        update.version = semver::Version::new(2, 36, 0);
        let error = verify_required_package_manifest(root.path(), &update).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required for release archives from v2.36.0 onward"),
            "unexpected missing-manifest error: {error}"
        );

        fs::write(root.path().join("kettle.exe"), b"binary").unwrap();
        write_test_package_manifest(root.path(), "2.36.0");
        verify_required_package_manifest(root.path(), &update)
            .expect("a valid v2.36 package manifest is accepted");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn package_manifest_requires_exact_hash_size_mode_and_file_set() {
        let root = test_tempdir();
        fs::create_dir(root.path().join("shell-integration")).unwrap();
        fs::write(root.path().join("kettle.exe"), b"binary").unwrap();
        fs::write(root.path().join("shell-integration/kettle.ps1"), b"prompt").unwrap();
        write_test_package_manifest(root.path(), "99.0.0");
        verify_package_manifest(root.path(), &fake_update()).unwrap();

        fs::write(root.path().join("undeclared"), b"extra").unwrap();
        assert!(verify_package_manifest(root.path(), &fake_update()).is_err());
        fs::remove_file(root.path().join("undeclared")).unwrap();

        let manifest_path = root.path().join(PACKAGE_MANIFEST_FILE);
        let mut manifest: PackageManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.files[0].path.make_ascii_uppercase();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(verify_package_manifest(root.path(), &fake_update()).is_err());
        write_test_package_manifest(root.path(), "99.0.0");

        fs::write(root.path().join("kettle.exe"), b"mutated").unwrap();
        assert!(verify_package_manifest(root.path(), &fake_update()).is_err());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn transaction_rolls_back_replaced_and_created_files() {
        let root = test_tempdir();
        fs::write(root.path().join("existing"), b"old").unwrap();
        let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&tx.backup_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        tx.install_bytes(Path::new("existing"), b"new", None)
            .unwrap();
        tx.install_bytes(Path::new("created"), b"created", None)
            .unwrap();
        tx.rollback().unwrap();
        assert_eq!(fs::read(root.path().join("existing")).unwrap(), b"old");
        assert!(!root.path().join("created").exists());
        assert!(!root.path().join(".kettle-update-journal.json").exists());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn rollback_refuses_to_overwrite_content_changed_after_update() {
        let root = test_tempdir();
        let destination = root.path().join("value");
        fs::write(&destination, b"last-known-good").unwrap();
        let mut transaction = Transaction::begin(root.path(), "99.0.0").unwrap();
        transaction
            .install_bytes(Path::new("value"), b"update replacement", None)
            .unwrap();
        fs::write(&destination, b"content written after update").unwrap();

        let error = transaction.rollback().unwrap_err();

        assert!(error.to_string().contains("rollback conflict"));
        assert_eq!(
            fs::read(&destination).unwrap(),
            b"content written after update",
            "conflict-aware rollback must preserve later content"
        );
        assert!(root.path().join(".kettle-update-journal.json").is_file());
        assert!(fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kettle-update-backup-")
        }));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn interrupted_transaction_recovers_from_journal() {
        let root = test_tempdir();
        fs::write(root.path().join("value"), b"before").unwrap();
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("value"), b"after", None)
                .unwrap();
            std::mem::forget(tx);
        }
        recover_transaction(root.path()).unwrap();
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"before");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn crash_recovery_removes_every_persisted_created_parent_and_preserves_preexisting_siblings() {
        let root = test_tempdir();
        fs::create_dir(root.path().join("existing")).unwrap();
        fs::create_dir(root.path().join("unrelated-empty")).unwrap();

        let mut transaction = Transaction::begin(root.path(), "99.0.0").unwrap();
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transaction
                .install_bytes_with_progress_and_post_publish(
                    Path::new("existing/new/deep/value"),
                    b"replacement",
                    None,
                    |progress| {
                        if progress == TransactionProgress::ParentDirectoriesPersisted {
                            panic!("simulated stop after destination parents were persisted");
                        }
                    },
                    || Ok(()),
                )
                .unwrap();
        }));
        assert!(
            stopped.is_err(),
            "the directory-persistence seam must be reached"
        );
        assert!(root.path().join("existing/new/deep").is_dir());
        assert!(!root.path().join("existing/new/deep/value").exists());

        let persisted: Journal = serde_json::from_slice(
            &fs::read(root.path().join(".kettle-update-journal.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted
                .created_directories
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["existing/new", "existing/new/deep"],
            "every newly created ancestor, and no pre-existing ancestor, must be durable"
        );
        std::mem::forget(transaction);

        recover_transaction(root.path()).unwrap();
        assert!(root.path().join("existing").is_dir());
        assert!(!root.path().join("existing/new").exists());
        assert!(
            root.path().join("unrelated-empty").is_dir(),
            "rollback must not infer ownership from emptiness"
        );
        assert!(!root.path().join(".kettle-update-journal.json").exists());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn created_parent_cleanup_preserves_the_persisted_journal_comparison_value() {
        let root = test_tempdir();
        fs::create_dir(root.path().join("existing")).unwrap();
        let mut transaction = Transaction::begin(root.path(), "99.0.0").unwrap();
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transaction
                .install_bytes_with_progress_and_post_publish(
                    Path::new("existing/new/deep/value"),
                    b"replacement",
                    None,
                    |progress| {
                        if progress == TransactionProgress::ParentDirectoriesPersisted {
                            panic!("simulated stop after destination parents were persisted");
                        }
                    },
                    || Ok(()),
                )
                .unwrap();
        }));
        assert!(stopped.is_err());

        transaction.journal.phase = JournalPhase::RollingBack;
        transaction.persist_journal().unwrap();
        transaction.remove_created_directories();

        let persisted: Journal = serde_json::from_slice(
            &fs::read(root.path().join(".kettle-update-journal.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            transaction.journal, persisted,
            "directory cleanup must retain the exact value checked before journal deletion"
        );
        assert!(!root.path().join("existing/new").exists());
        transaction.finish_cleanup().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_checked_journal_deletion_preserves_a_replacement() {
        let root = test_tempdir();
        let transaction = Transaction::begin(root.path(), "99.0.0").unwrap();
        let expected = transaction.journal.clone();
        let mut replacement = expected.clone();
        replacement.target_version = "98.0.0".into();
        atomic_write(
            &transaction.journal_path,
            &serde_json::to_vec_pretty(&replacement).unwrap(),
            None,
        )
        .unwrap();

        let error =
            remove_schema2_journal_checked(root.path(), &transaction.journal_path, &expected)
                .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("state file changed before deletion")
        );
        let retained: Journal =
            serde_json::from_slice(&fs::read(&transaction.journal_path).unwrap()).unwrap();
        assert_eq!(retained, replacement);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn schema_two_journal_without_created_directories_remains_recoverable() {
        let root = test_tempdir();
        let mut transaction = Transaction::begin(root.path(), "99.0.0").unwrap();
        let mut legacy_shape = serde_json::to_value(&transaction.journal).unwrap();
        legacy_shape
            .as_object_mut()
            .unwrap()
            .remove("created_directories");
        let decoded: Journal = serde_json::from_value(legacy_shape).unwrap();
        assert!(decoded.created_directories.is_empty());
        validate_journal(&decoded).unwrap();
        transaction.rollback().unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn recovery_handles_every_backup_and_publication_crash_boundary() {
        for stage in [
            TransactionProgress::BackupStreaming,
            TransactionProgress::BackupSynced,
            TransactionProgress::EntryPrepared,
            TransactionProgress::Published,
        ] {
            let root = test_tempdir();
            fs::write(root.path().join("first"), b"old-first").unwrap();
            fs::write(root.path().join("second"), vec![b'x'; 128 * 1024]).unwrap();
            let mut transaction = Transaction::begin(root.path(), "99.0.0").unwrap();
            transaction
                .install_bytes(Path::new("first"), b"new-first", None)
                .unwrap();

            let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                transaction
                    .install_bytes_with_progress_and_post_publish(
                        Path::new("second"),
                        b"new-second",
                        None,
                        |progress| {
                            if progress == stage {
                                panic!("simulated process stop at {stage:?}");
                            }
                        },
                        || Ok(()),
                    )
                    .unwrap();
            }));
            assert!(stopped.is_err(), "the {stage:?} seam must be reached");
            assert_eq!(fs::read(root.path().join("first")).unwrap(), b"new-first");
            std::mem::forget(transaction);

            recover_transaction(root.path())
                .unwrap_or_else(|error| panic!("recovery failed after {stage:?}: {error}"));
            assert_eq!(
                fs::read(root.path().join("first")).unwrap(),
                b"old-first",
                "an earlier publication must roll back after {stage:?}"
            );
            assert_eq!(
                fs::read(root.path().join("second")).unwrap(),
                vec![b'x'; 128 * 1024],
                "the interrupted destination must retain its prior bytes after {stage:?}"
            );
            assert!(!root.path().join(".kettle-update-journal.json").exists());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn published_executable_has_final_mode_before_installed_journal_state() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = test_tempdir();
        let executable = root.path().join("kettle");
        fs::write(&executable, b"before").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            snapshot_transaction_destination(root.path(), Path::new("kettle"))
                .unwrap()
                .file
                .is_some()
        );

        let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
        assert!(
            snapshot_transaction_destination(root.path(), Path::new("kettle"))
                .unwrap()
                .file
                .is_some()
        );
        let error = tx
            .install_bytes_with_post_publish(Path::new("kettle"), b"after", Some(0o755), || {
                Err(UpdateError::Transaction("simulated process stop".into()))
            })
            .unwrap_err();

        assert!(error.to_string().contains("simulated process stop"));
        assert_eq!(fs::read(&executable).unwrap(), b"after");
        assert_eq!(
            fs::metadata(&executable).unwrap().permissions().mode() & 0o777,
            0o755,
            "the published inode must never expose the private staging mode"
        );
        assert_eq!(
            tx.journal.entries.last().unwrap().state,
            JournalEntryState::Prepared,
            "the simulated stop occurs before Installed is persisted"
        );
        assert!(
            tx.journal.entries.last().unwrap().existed,
            "the pre-publication backup records the replaced executable"
        );

        std::mem::forget(tx);
        recover_transaction(root.path()).unwrap();
        assert_eq!(fs::read(&executable).unwrap(), b"before");
        assert_eq!(
            fs::metadata(&executable).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn committed_transaction_keeps_last_known_good_until_target_starts() {
        let root = test_tempdir();
        fs::write(root.path().join("value"), b"before").unwrap();
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("value"), b"after", None)
                .unwrap();
            tx.journal.phase = JournalPhase::Committed;
            tx.persist_journal().unwrap();
            std::mem::forget(tx);
        }
        let error = recover_transaction(root.path()).unwrap_err();
        assert!(error.to_string().contains("awaiting startup confirmation"));
        assert!(root.path().join(".kettle-update-journal.json").is_file());
        assert!(fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kettle-update-backup-")
        }));

        assert!(
            confirm_committed_transaction(root.path(), &semver::Version::new(99, 0, 0),).unwrap()
        );
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"after");
        assert!(!root.path().join(".kettle-update-journal.json").exists());
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kettle-update-backup-")
        }));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn recovery_resumes_partially_completed_rollback_idempotently() {
        let root = test_tempdir();
        fs::write(root.path().join("one"), b"old-one").unwrap();
        fs::write(root.path().join("two"), b"old-two").unwrap();
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("one"), b"new-one", None)
                .unwrap();
            tx.install_bytes(Path::new("two"), b"new-two", None)
                .unwrap();
            tx.journal.phase = JournalPhase::RollingBack;
            let last = tx.journal.entries.len() - 1;
            restore_entry(&tx.prefix, &tx.backup_dir, &tx.journal.entries[last]).unwrap();
            tx.journal.entries[last].state = JournalEntryState::Restored;
            tx.persist_journal().unwrap();
            std::mem::forget(tx);
        }
        recover_transaction(root.path()).unwrap();
        assert_eq!(fs::read(root.path().join("one")).unwrap(), b"old-one");
        assert_eq!(fs::read(root.path().join("two")).unwrap(), b"old-two");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn corrupted_backup_stops_recovery_without_deleting_evidence() {
        let root = test_tempdir();
        fs::write(root.path().join("value"), b"before").unwrap();
        let backup;
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("value"), b"after", None)
                .unwrap();
            backup = tx.backup_dir.join("value");
            std::mem::forget(tx);
        }
        fs::write(&backup, b"tampered").unwrap();
        let error = recover_transaction(root.path()).unwrap_err();
        assert!(error.to_string().contains("backup integrity check failed"));
        assert!(root.path().join(".kettle-update-journal.json").is_file());
        assert!(backup.is_file());
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"after");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn unjournaled_backup_file_stops_recovery_without_deleting_evidence() {
        let root = test_tempdir();
        fs::write(root.path().join("value"), b"before").unwrap();
        let backup_dir;
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("value"), b"after", None)
                .unwrap();
            backup_dir = tx.backup_dir.clone();
            std::mem::forget(tx);
        }
        fs::write(backup_dir.join("unjournaled"), b"evidence").unwrap();

        let error = recover_transaction(root.path()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not exactly cover the update journal")
                || error.to_string().contains("unjournaled")
        );
        assert!(root.path().join(".kettle-update-journal.json").is_file());
        assert!(backup_dir.join("unjournaled").is_file());
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"after");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn transaction_preflight_rejects_aggregate_backup_quota_before_destination_mutation() {
        let root = test_tempdir();
        let first = root.path().join("first");
        let second = root.path().join("second");
        File::create(&first)
            .unwrap()
            .set_len(MAX_UNPACKED_BYTES / 2 + 1)
            .unwrap();
        File::create(&second)
            .unwrap()
            .set_len(MAX_UNPACKED_BYTES / 2 + 1)
            .unwrap();
        let first_size = first.metadata().unwrap().len();
        let second_size = second.metadata().unwrap().len();
        let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();

        let error = tx
            .preflight_destinations(&[PathBuf::from("first"), PathBuf::from("second")])
            .unwrap_err();

        assert!(error.to_string().contains("backup set exceeds"));
        assert_eq!(first.metadata().unwrap().len(), first_size);
        assert_eq!(second.metadata().unwrap().len(), second_size);
        assert!(tx.journal.entries.is_empty());
        tx.rollback().unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn transaction_rejects_duplicate_destinations() {
        let root = test_tempdir();
        let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
        tx.install_bytes(Path::new("README.md"), b"first", None)
            .unwrap();
        let error = tx
            .install_bytes(Path::new("readme.MD"), b"second", None)
            .unwrap_err();
        assert!(error.to_string().contains("duplicate install destination"));
        tx.rollback().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn transaction_refuses_symbolic_link_destinations() {
        use std::os::unix::fs::symlink;
        let root = test_tempdir();
        let outside = root.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, root.path().join("value")).unwrap();
        let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
        let error = tx
            .install_bytes(Path::new("value"), b"replacement", None)
            .unwrap_err();
        assert!(error.to_string().contains("non-regular file"));
        tx.rollback().unwrap();
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn transaction_refuses_symbolic_link_ancestors() {
        use std::os::unix::fs::symlink;

        let root = test_tempdir();
        let outside = test_tempdir();
        symlink(outside.path(), root.path().join("share")).unwrap();
        let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();

        let error = tx
            .install_bytes(Path::new("share/kettle/value"), b"replacement", None)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Too many levels of symbolic links")
                || error.to_string().contains("install path component")
        );
        assert!(!outside.path().join("kettle/value").exists());
        tx.rollback().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_refuses_a_replaced_symbolic_link_ancestor() {
        use std::os::unix::fs::symlink;

        let root = test_tempdir();
        let outside = test_tempdir();
        let share = root.path().join("share");
        create_linux_install_dir_all(&share, &share.join("kettle"));
        fs::write(root.path().join("share/kettle/value"), b"before").unwrap();
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("share/kettle/value"), b"after", None)
                .unwrap();
            std::mem::forget(tx);
        }
        fs::remove_dir_all(root.path().join("share")).unwrap();
        symlink(outside.path(), root.path().join("share")).unwrap();

        let error = recover_transaction(root.path()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Too many levels of symbolic links")
                || error.to_string().contains("install path component")
                || error.to_string().contains("rollback conflict")
        );
        assert!(!outside.path().join("kettle/value").exists());
        assert!(root.path().join(".kettle-update-journal.json").is_file());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn legacy_journal_recovery_removes_journal_before_backup_cleanup() {
        let root = test_tempdir();
        let backup_name = ".kettle-update-backup-legacy";
        let backup = root.path().join(backup_name);
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("value"), b"before").unwrap();
        fs::write(root.path().join("value"), b"after").unwrap();
        let journal = LegacyJournal {
            schema: 1,
            backup_dir: backup_name.into(),
            entries: vec![LegacyJournalEntry {
                relative: "value".into(),
                existed: true,
                previous_unix_mode: None,
            }],
        };
        atomic_write(
            &root.path().join(".kettle-update-journal.json"),
            &serde_json::to_vec(&journal).unwrap(),
            Some(0o600),
        )
        .unwrap();
        recover_transaction(root.path()).unwrap();
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"before");
        assert!(!backup.exists());
        assert!(!root.path().join(".kettle-update-journal.json").exists());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn legacy_recovery_preserves_an_unjournaled_sentinel_without_mutating_destination() {
        let root = test_tempdir();
        let backup_name = ".kettle-update-backup-legacy";
        let backup = root.path().join(backup_name);
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("value"), b"before").unwrap();
        let sentinel = backup.join("user-evidence");
        fs::write(&sentinel, b"must survive").unwrap();
        fs::write(root.path().join("value"), b"after").unwrap();
        let journal = LegacyJournal {
            schema: 1,
            backup_dir: backup_name.into(),
            entries: vec![LegacyJournalEntry {
                relative: "value".into(),
                existed: true,
                previous_unix_mode: None,
            }],
        };
        atomic_write(
            &root.path().join(".kettle-update-journal.json"),
            &serde_json::to_vec(&journal).unwrap(),
            Some(0o600),
        )
        .unwrap();

        let error = recover_transaction(root.path()).unwrap_err();

        assert!(error.to_string().contains("unjournaled"));
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"after");
        assert_eq!(fs::read(&sentinel).unwrap(), b"must survive");
        assert!(root.path().join(".kettle-update-journal.json").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn staged_windows_release_replaces_binary_and_support_files_atomically() {
        let root = test_tempdir();
        let prefix = root.path().join("install");
        let stage = root.path().join("stage");
        fs::create_dir_all(stage.join("shell-integration")).unwrap();
        fs::create_dir_all(&prefix).unwrap();
        fs::write(prefix.join("kettle.exe"), b"old-binary").unwrap();
        fs::write(prefix.join("README.md"), b"old-readme").unwrap();
        fs::write(stage.join("kettle.exe"), b"new-binary").unwrap();
        fs::write(stage.join("kettle.com"), b"new-launcher").unwrap();
        fs::write(stage.join("install.ps1"), b"install").unwrap();
        fs::write(stage.join("README.md"), b"new-readme").unwrap();
        fs::write(stage.join("shell-integration/kettle.ps1"), b"prompt").unwrap();
        let install = ManagedInstall {
            executable: prefix.join("kettle.exe"),
            marker_path: prefix.join(".kettle-install.json"),
            prefix: prefix.clone(),
        };
        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_staged_update(&mut transaction, &stage, &install, &fake_update()).unwrap();
        transaction.commit().unwrap();
        assert_eq!(fs::read(prefix.join("kettle.exe")).unwrap(), b"new-binary");
        assert_eq!(
            fs::read(prefix.join("kettle.com")).unwrap(),
            b"new-launcher"
        );
        assert_eq!(fs::read(prefix.join("README.md")).unwrap(), b"new-readme");
        assert_eq!(
            fs::read(prefix.join("shell-integration/kettle.ps1")).unwrap(),
            b"prompt"
        );
        let marker: InstallMarker =
            serde_json::from_slice(&fs::read(prefix.join(".kettle-install.json")).unwrap())
                .unwrap();
        assert_eq!(marker.version, "99.0.0");
    }

    #[cfg(target_os = "linux")]
    fn test_linux_package_tar(version: &str) -> Vec<u8> {
        let payloads = [
            ("kettle", b"verified-binary".as_slice(), 0o755),
            ("install.sh", b"verified-installer".as_slice(), 0o755),
            // The real release archive ships this (release.yml installs
            // `scripts/install-unix.py` into `dist/kettle/`), and provenance
            // verification requires it to be recorded. The fixture omitted it
            // only because the production path used to omit it too.
            ("install-unix.py", b"verified-unix-installer".as_slice(), 0o755),
            ("LICENSE", b"license".as_slice(), 0o644),
            ("NOTICE", b"notice".as_slice(), 0o644),
            ("README.md", b"readme".as_slice(), 0o644),
            ("CHANGELOG.md", b"changes".as_slice(), 0o644),
            (
                "packaging/linux/kettle.desktop",
                b"[Desktop Entry]\nType=Application\nName=Kettle\nTerminal=false\nExec=kettle\nTryExec=kettle\nIcon=kettle\n"
                    .as_slice(),
                0o644,
            ),
            ("packaging/linux/kettle.svg", b"svg".as_slice(), 0o644),
            ("packaging/linux/kettle-16.png", b"16".as_slice(), 0o644),
            ("packaging/linux/kettle-24.png", b"24".as_slice(), 0o644),
            ("packaging/linux/kettle-32.png", b"32".as_slice(), 0o644),
            ("packaging/linux/kettle-48.png", b"48".as_slice(), 0o644),
            ("packaging/linux/kettle-64.png", b"64".as_slice(), 0o644),
            ("packaging/linux/kettle-128.png", b"128".as_slice(), 0o644),
            ("packaging/linux/kettle-256.png", b"256".as_slice(), 0o644),
            ("packaging/linux/kettle.1", b"man".as_slice(), 0o644),
            (
                "shell-integration/kettle.bash",
                b"shell integration".as_slice(),
                0o644,
            ),
        ];
        let package_manifest = serde_json::to_vec(&PackageManifest {
            schema: 1,
            product: "kettle".into(),
            target: current_target().unwrap().into(),
            version: version.into(),
            files: payloads
                .iter()
                .map(|(path, bytes, mode)| PackageFile {
                    path: (*path).into(),
                    size: bytes.len() as u64,
                    sha256: sha256_bytes(bytes),
                    mode: Some(*mode),
                })
                .collect(),
        })
        .unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (path, bytes, mode) in payloads {
            let mut header = tar::Header::new_gnu();
            header.set_mode(mode);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            archive
                .append_data(&mut header, format!("kettle/{path}"), bytes)
                .unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(package_manifest.len() as u64);
        header.set_cksum();
        archive
            .append_data(
                &mut header,
                format!("kettle/{PACKAGE_MANIFEST_FILE}"),
                package_manifest.as_slice(),
            )
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap()
    }

    /// A managed Linux update must leave the installation still managed.
    ///
    /// The updater replaces provenance-covered files — `bin/kettle`,
    /// `install.sh`, the desktop file, icons, the man page — and did not
    /// regenerate `install-files.json`. The record therefore still held the
    /// OLD hashes for the NEW files, so the very next verification reported
    /// the installation unmanaged: startup could not confirm or clean the
    /// committed transaction, and every later `kettle update` refused to run.
    /// One official update was enough to strand an installation permanently.
    ///
    /// The previous version of this test seeded no provenance and asserted
    /// only that two files had new bytes, so it passed throughout.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_transaction_applies_bytes_from_verified_archive_memory() {
        let root = test_tempdir();
        let mut archive = test_linux_package_tar("99.0.0");
        let package = load_linux_package(&archive, &fake_update()).unwrap();
        archive.fill(0);

        let prefix = root.path().join("install");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/kettle"), b"old-binary").unwrap();
        // A real update always runs against an install that already verified,
        // because `install_update` calls `detect_managed_install` first.
        seed_linux_install_provenance(&prefix);
        let install = ManagedInstall {
            prefix: prefix.clone(),
            executable: prefix.join("bin/kettle"),
            marker_path: prefix.join("share/kettle/install.json"),
        };
        let before = read_linux_install_provenance(&prefix).unwrap();

        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_verified_linux_update(&mut transaction, &package, &install, &fake_update()).unwrap();
        transaction.commit().unwrap();

        assert_eq!(
            fs::read(prefix.join("bin/kettle")).unwrap(),
            b"verified-binary"
        );
        assert_eq!(
            fs::read(prefix.join("share/kettle/install.sh")).unwrap(),
            b"verified-installer"
        );

        // The record was regenerated, not left describing the old files.
        let after = read_linux_install_provenance(&prefix).unwrap();
        assert_ne!(
            before.files, after.files,
            "the provenance record must be rewritten by the update"
        );

        // Every recorded hash must match what is actually on disk — this is
        // precisely the check that failed after a real update.
        for record in &after.files {
            let path = prefix.join(&record.path);
            let metadata = fs::metadata(&path).unwrap_or_else(|error| {
                panic!(
                    "recorded file {} is missing after update: {error}",
                    record.path
                )
            });
            assert_eq!(
                sha256_file(&path).unwrap(),
                record.sha256,
                "provenance hash for {} does not match the installed bytes",
                record.path
            );
            assert_eq!(metadata.len(), record.size, "size for {}", record.path);
        }

        // The files the verifier demands are all present in the new record.
        for required in [
            "bin/kettle",
            "share/applications/kettle.desktop",
            "share/kettle/install.sh",
            "share/kettle/install-unix.py",
            "share/kettle/install.json",
        ] {
            assert!(
                after.files.iter().any(|file| file.path == required),
                "the regenerated provenance must record {required}"
            );
        }

        // Every destination the applier's map names must have the ARCHIVE's
        // bytes on disk, not merely a record.
        //
        // "It is recorded and the record matches disk" is satisfied by a file
        // nobody touched — which is what carrying records forward made
        // possible. Before that, dropping an entry from the production map
        // produced a loud "provenance is missing share/kettle/install-unix.py";
        // afterwards the old record was carried forward and the same drift was
        // silent, so the helper would simply never update again. That is the
        // exact bug this test was written for, and the carry-forward defanged
        // it. Comparing bytes is what cannot be satisfied by not acting.
        for (source, destination) in [
            ("kettle", "bin/kettle"),
            ("install.sh", "share/kettle/install.sh"),
            ("install-unix.py", "share/kettle/install-unix.py"),
            ("LICENSE", "share/doc/kettle/LICENSE"),
            ("NOTICE", "share/doc/kettle/NOTICE"),
            ("README.md", "share/doc/kettle/README.md"),
            ("CHANGELOG.md", "share/doc/kettle/CHANGELOG.md"),
            (
                "packaging/linux/kettle.svg",
                "share/icons/hicolor/scalable/apps/kettle.svg",
            ),
            (
                "packaging/linux/kettle-256.png",
                "share/icons/hicolor/256x256/apps/kettle.png",
            ),
            ("packaging/linux/kettle.1", "share/man/man1/kettle.1"),
        ] {
            let want = package
                .bytes(Path::new(source))
                .unwrap_or_else(|error| panic!("the fixture archive must ship {source}: {error}"));
            let got = fs::read(prefix.join(destination))
                .unwrap_or_else(|error| panic!("{destination} was not installed: {error}"));
            assert_eq!(
                got, want,
                "{destination} does not hold the bytes the archive shipped for \
                 {source} — the applier's map no longer publishes it"
            );
        }

        // Records must be sorted strictly by path, or the reader rejects them.
        for pair in after.files.windows(2) {
            assert!(
                pair[0].path < pair[1].path,
                "provenance files must be strictly sorted: {:?} then {:?}",
                pair[0].path,
                pair[1].path
            );
        }

        // The record describes who wrote the files. This agrees with the prefix
        // owner in every case that verifies, so it does not catch a change back
        // to the prefix's uid on its own — see `install_unix_provenance`. It
        // does pin the field against `install-unix.py`, which records the same
        // thing.
        assert_eq!(
            after.owner_uid,
            unsafe { libc::geteuid() },
            "provenance must record the uid that published the files"
        );
    }

    /// An update must not disown what the previous release installed.
    ///
    /// Provenance is the only list uninstall consults. Regenerating it from the
    /// archive alone meant a file an older release shipped and this one dropped
    /// stayed on disk with no record of it — installed forever, removable by
    /// nothing. `install-unix.py` seeds the new record from the old one, so
    /// regenerating from scratch also made the two writers disagree.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_update_keeps_owning_files_the_new_archive_dropped() {
        let root = test_tempdir();
        let mut archive = test_linux_package_tar("99.0.0");
        let package = load_linux_package(&archive, &fake_update()).unwrap();
        archive.fill(0);

        let prefix = root.path().join("install");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/kettle"), b"old-binary").unwrap();
        // Shipped by the previous release; absent from the archive above.
        let retired = "share/kettle/retired-helper.sh";
        seed_linux_install_provenance_with(&prefix, &[(retired, b"#!/bin/sh\nretired\n", 0o755)]);
        let install = ManagedInstall {
            prefix: prefix.clone(),
            executable: prefix.join("bin/kettle"),
            marker_path: prefix.join("share/kettle/install.json"),
        };
        let before = read_linux_install_provenance(&prefix).unwrap();
        let retired_before = before
            .files
            .iter()
            .find(|file| file.path == retired)
            .expect("the fixture must seed the retired file")
            .clone();

        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_verified_linux_update(&mut transaction, &package, &install, &fake_update()).unwrap();
        transaction.commit().unwrap();

        // Still on disk — the update never touched it.
        assert!(prefix.join(retired).is_file());
        // And still owned, byte for byte as it was recorded.
        let after = read_linux_install_provenance(&prefix).unwrap();
        let retired_after = after
            .files
            .iter()
            .find(|file| file.path == retired)
            .expect("an update must keep owning files it no longer ships");
        assert_eq!(
            (
                &retired_after.sha256,
                retired_after.size,
                retired_after.mode
            ),
            (
                &retired_before.sha256,
                retired_before.size,
                retired_before.mode
            ),
            "a carried-forward record must survive unchanged"
        );
        // Carrying forward must not duplicate the paths this update rewrote.
        for pair in after.files.windows(2) {
            assert!(pair[0].path < pair[1].path, "strict order after the merge");
        }
    }

    /// Directory ownership must come from what the transaction actually
    /// created, and a rollback must undo those creations.
    ///
    /// The plan used to sample `try_exists` before writing anything. A
    /// transaction that created `share/man/man1`, failed, and rolled back
    /// restored the files but left the directory; the retry then saw it as
    /// pre-existing, left it out of provenance, and uninstall could never
    /// remove it.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_update_owns_the_directories_it_creates_and_rollback_removes_them() {
        use std::os::unix::fs::PermissionsExt as _;

        // Directories the fixture prefix does not have and the archive needs.
        let fresh = [
            "share/doc/kettle",
            "share/man/man1",
            "share/icons/hicolor/256x256/apps",
        ];

        let make_prefix = |root: &Path| {
            let prefix = root.join("install");
            fs::create_dir_all(prefix.join("bin")).unwrap();
            fs::write(prefix.join("bin/kettle"), b"old-binary").unwrap();
            seed_linux_install_provenance(&prefix);
            for relative in fresh {
                assert!(
                    !prefix.join(relative).exists(),
                    "{relative} must be missing for this test to mean anything"
                );
            }
            prefix
        };
        let install_for = |prefix: &Path| ManagedInstall {
            prefix: prefix.to_path_buf(),
            executable: prefix.join("bin/kettle"),
            marker_path: prefix.join("share/kettle/install.json"),
        };

        // Committed: the new directories are recorded as owned.
        let committed_root = test_tempdir();
        let prefix = make_prefix(committed_root.path());
        let mut archive = test_linux_package_tar("99.0.0");
        let package = load_linux_package(&archive, &fake_update()).unwrap();
        archive.fill(0);
        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_verified_linux_update(
            &mut transaction,
            &package,
            &install_for(&prefix),
            &fake_update(),
        )
        .unwrap();
        transaction.commit().unwrap();

        let after = read_linux_install_provenance(&prefix).unwrap();
        let owned = after
            .directories
            .iter()
            .map(|record| record.path.as_str())
            .collect::<std::collections::HashSet<_>>();
        for relative in fresh {
            assert!(
                prefix.join(relative).is_dir(),
                "{relative} must exist after the update"
            );
            assert_eq!(
                fs::metadata(prefix.join(relative))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755,
                "the updater must give {relative} an umask-independent mode"
            );
            assert!(
                owned.contains(relative),
                "the update created {relative} and must record owning it; recorded {owned:?}"
            );
        }
        for pair in after.directories.windows(2) {
            assert!(
                pair[0].path < pair[1].path,
                "directory records must be strictly sorted"
            );
        }

        // Rolled back: the same directories are gone again.
        let rolled_back_root = test_tempdir();
        let prefix = make_prefix(rolled_back_root.path());
        let mut archive = test_linux_package_tar("99.0.0");
        let package = load_linux_package(&archive, &fake_update()).unwrap();
        archive.fill(0);
        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_verified_linux_update(
            &mut transaction,
            &package,
            &install_for(&prefix),
            &fake_update(),
        )
        .unwrap();
        transaction.rollback().unwrap();

        assert_eq!(
            fs::read(prefix.join("bin/kettle")).unwrap(),
            b"old-binary",
            "rollback must restore the previous binary"
        );
        // Every recorded directory, not just the leaves.
        //
        // Asserting only the three leaf paths could not see the order: a leaf
        // is removed last under either order, so reversing `keys().rev()` to
        // `keys()` — which strands 12 parents, `share/doc` and `share/icons`
        // and the rest, because `remove_dir` refuses a directory whose child
        // has not gone yet — left this test green. Walking the whole tree is
        // what makes deepest-first load-bearing.
        let mut leaked = Vec::new();
        let mut walk = vec![prefix.clone()];
        while let Some(dir) = walk.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Ok(relative) = path.strip_prefix(&prefix) {
                        let relative = relative.to_string_lossy().replace('\\', "/");
                        // The backup tree is the transaction's own, cleaned up
                        // separately.
                        if !relative.starts_with(".kettle-update-") {
                            leaked.push(relative);
                        }
                    }
                    walk.push(path);
                }
            }
        }
        leaked.sort();
        let expected_before: Vec<String> = ["bin", "share", "share/applications", "share/kettle"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(
            leaked, expected_before,
            "rollback must leave exactly the directories that existed before it, \
             and it left these instead"
        );

        for relative in fresh {
            assert!(
                !prefix.join(relative).exists(),
                "rollback left {relative} behind, so a retry would never claim it"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn staged_linux_release_populates_installer_layout() {
        let root = test_tempdir();
        let prefix = root.path().join("install with \\ % $ quote\" and ` value");
        let stage = root.path().join("stage/kettle");
        fs::create_dir_all(stage.join("packaging/linux")).unwrap();
        fs::create_dir_all(stage.join("shell-integration")).unwrap();
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/kettle"), b"old-binary").unwrap();
        fs::create_dir_all(prefix.join("share/kettle")).unwrap();
        fs::write(
            prefix.join("share/kettle/install.json"),
            marker_json("2.35.0").unwrap(),
        )
        .unwrap();
        seed_linux_install_provenance(&prefix);
        for (relative, body) in [
            ("kettle", "new-binary"),
            ("install.sh", "install"),
            ("install-unix.py", "helper"),
            ("LICENSE", "license"),
            ("NOTICE", "notice"),
            ("README.md", "readme"),
            ("CHANGELOG.md", "changes"),
            (
                "packaging/linux/kettle.desktop",
                "[Desktop Entry]\nType=Application\nName=Kettle\nTerminal=false\nExec=kettle\nTryExec=kettle\nIcon=kettle\n",
            ),
            ("packaging/linux/kettle.svg", "svg"),
            ("packaging/linux/kettle-16.png", "16"),
            ("packaging/linux/kettle-24.png", "24"),
            ("packaging/linux/kettle-32.png", "32"),
            ("packaging/linux/kettle-48.png", "48"),
            ("packaging/linux/kettle-64.png", "64"),
            ("packaging/linux/kettle-128.png", "128"),
            ("packaging/linux/kettle-256.png", "256"),
            ("packaging/linux/kettle.1", "man"),
            ("shell-integration/kettle.bash", "prompt"),
        ] {
            let path = stage.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, body).unwrap();
        }
        let install = ManagedInstall {
            executable: prefix.join("bin/kettle"),
            marker_path: prefix.join("share/kettle/install.json"),
            prefix: prefix.clone(),
        };
        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_staged_update(
            &mut transaction,
            root.path().join("stage").as_path(),
            &install,
            &fake_update(),
        )
        .unwrap();
        transaction.commit().unwrap();
        assert_eq!(fs::read(prefix.join("bin/kettle")).unwrap(), b"new-binary");
        let desktop_path = prefix.join("share/applications/kettle.desktop");
        let desktop = fs::read_to_string(&desktop_path).unwrap();
        let executable = prefix.join("bin/kettle");
        assert!(desktop.contains(&format!(
            "Exec={}",
            desktop_exec_argument(&executable).unwrap()
        )));
        assert!(desktop.contains(&format!(
            "TryExec={}",
            desktop_string_path(&executable).unwrap()
        )));
        let icon = prefix.join("share/icons/hicolor/256x256/apps/kettle.png");
        let expected_icon = format!("Icon={}", desktop_string_path(&icon).unwrap());
        assert!(desktop.lines().any(|line| line == expected_icon));
        assert!(!desktop.contains("scalable/apps/kettle.svg"));
        assert!(
            desktop.contains("%%"),
            "Exec field code was not escaped: {desktop}"
        );
        let escaped_segment = format!(
            "install with {} %% {}$ quote{}\" and {}` value",
            "\\".repeat(4),
            "\\".repeat(2),
            "\\".repeat(2),
            "\\".repeat(2)
        );
        assert!(
            desktop.contains(&escaped_segment),
            "Exec argument/string escaping layers were not both applied: {desktop}"
        );
        if let Ok(status) = std::process::Command::new("desktop-file-validate")
            .arg(&desktop_path)
            .status()
        {
            assert!(
                status.success(),
                "generated desktop entry failed validation"
            );
        }
        assert!(prefix.join("share/kettle/install.json").is_file());
        assert!(prefix.join("share/kettle/install-unix.py").is_file());
        let provenance = read_linux_install_provenance(&prefix).unwrap();
        assert!(
            provenance
                .files
                .iter()
                .any(|record| record.path == "bin/kettle"
                    && record.sha256 == sha256_bytes(b"new-binary"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_template_rewrite_requires_each_owned_key_exactly_once() {
        let root = test_tempdir();
        let template = root.path().join("kettle.desktop");
        let prefix = root.path().join("prefix");
        for body in [
            "[Desktop Entry]\nExec=kettle\nTryExec=kettle\n",
            "[Desktop Entry]\nExec=kettle\nExec=kettle\nTryExec=kettle\nIcon=kettle\n",
            "[Desktop Entry]\nExec=other\nTryExec=kettle\nIcon=kettle\n",
        ] {
            fs::write(&template, body).unwrap();
            let error = render_linux_desktop(&template, &prefix).unwrap_err();
            assert!(error.to_string().contains("exactly one"));
        }
    }

    /// Regression test for resolving desktop-integration tools by a fixed
    /// allowlist rather than an unqualified `Command::new(name)` PATH
    /// search: a same-named binary planted outside every allowlisted
    /// directory (standing in for a writable, earlier PATH entry) must never
    /// be picked up, and among allowlisted directories the earliest match
    /// wins.
    #[cfg(target_os = "linux")]
    #[test]
    fn system_tool_path_in_only_resolves_allowlisted_directories_in_order() {
        let root = test_tempdir();
        let off_list = root.path().join("off-list");
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir_all(&off_list).unwrap();
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(off_list.join("tool"), b"attacker").unwrap();

        let dirs = [first.to_str().unwrap(), second.to_str().unwrap()];
        assert!(
            system_tool_path_in(&dirs, "tool").is_none(),
            "a same-named binary outside every allowlisted directory must not be resolved"
        );

        fs::write(second.join("tool"), b"real").unwrap();
        assert_eq!(
            system_tool_path_in(&dirs, "tool").unwrap(),
            second.join("tool")
        );

        fs::write(first.join("tool"), b"real-preferred").unwrap();
        assert_eq!(
            system_tool_path_in(&dirs, "tool").unwrap(),
            first.join("tool"),
            "earlier allowlisted directories must take priority"
        );
    }

    /// Regression test for resolving `powershell.exe` by a fixed,
    /// fully-qualified path rather than an unqualified
    /// `Command::new("powershell.exe")` PATH/CWD search.
    #[cfg(windows)]
    #[test]
    fn system_powershell_path_resolves_a_fully_qualified_existing_binary() {
        let path =
            system_powershell_path().expect("a Windows test runner must have PowerShell installed");
        assert!(path.is_absolute());
        assert!(path.is_file());
        assert!(
            path.file_name().and_then(|name| name.to_str()) == Some("powershell.exe"),
            "resolved path was {}",
            path.display()
        );
    }

    #[test]
    fn zip_unix_modes_reject_links_devices_fifos_and_type_mismatches() {
        assert!(zip_unix_mode_is_safe(None, false));
        assert!(zip_unix_mode_is_safe(Some(0o755), false));
        assert!(zip_unix_mode_is_safe(Some(0o100755), false));
        assert!(zip_unix_mode_is_safe(Some(0o040755), true));
        assert!(!zip_unix_mode_is_safe(Some(0o120777), false));
        assert!(!zip_unix_mode_is_safe(Some(0o010600), false));
        assert!(!zip_unix_mode_is_safe(Some(0o020600), false));
        assert!(!zip_unix_mode_is_safe(Some(0o040755), false));
        assert!(!zip_unix_mode_is_safe(Some(0o100755), true));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_archive_extraction_preserves_modes_and_rejects_special_bits() {
        use std::os::unix::fs::PermissionsExt as _;

        fn write_archive(path: &Path, mode: u32) {
            let encoder = flate2::write::GzEncoder::new(
                fs::File::create(path).unwrap(),
                flate2::Compression::default(),
            );
            let mut archive = tar::Builder::new(encoder);
            let payload = b"#!/bin/sh\n";
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(mode);
            header.set_size(payload.len() as u64);
            header.set_cksum();
            archive
                .append_data(&mut header, "kettle/install.sh", payload.as_slice())
                .unwrap();
            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let root = test_tempdir();
        let archive = root.path().join("release.tar.gz");
        let destination = root.path().join("stage");
        fs::create_dir(&destination).unwrap();
        write_archive(&archive, 0o755);
        let archive_bytes = fs::read(&archive).unwrap();
        extract_archive(&archive_bytes, &destination).unwrap();
        assert_eq!(
            fs::metadata(destination.join("kettle/install.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );

        let special_archive = root.path().join("special.tar.gz");
        let special_destination = root.path().join("special-stage");
        fs::create_dir(&special_destination).unwrap();
        write_archive(&special_archive, 0o4755);
        let special_archive_bytes = fs::read(&special_archive).unwrap();
        let error = extract_archive(&special_archive_bytes, &special_destination).unwrap_err();
        assert!(error.to_string().contains("special permission bits"));
        assert!(!special_destination.join("kettle/install.sh").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_archive_extraction_rejects_pax_sparse_metadata() {
        let root = test_tempdir();
        let archive_path = root.path().join("sparse.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        archive
            .append_pax_extensions([("GNU.sparse.map", b"0,4".as_slice())])
            .unwrap();
        let payload = b"data";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(payload.len() as u64);
        header.set_cksum();
        archive
            .append_data(&mut header, "kettle/sparse", payload.as_slice())
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        let destination = root.path().join("stage");
        fs::create_dir(&destination).unwrap();

        let archive_bytes = fs::read(&archive_path).unwrap();
        let error = extract_archive(&archive_bytes, &destination).unwrap_err();

        assert!(error.to_string().contains("sparse files are forbidden"));
        assert!(!destination.join("kettle/sparse").exists());
    }

    #[cfg(target_os = "linux")]
    fn write_test_tar_gz(path: &Path, payload: &[u8]) {
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(payload.len() as u64);
        header.set_cksum();
        archive
            .append_data(&mut header, "kettle/payload", payload)
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
    }

    /// Regression test for the archive TOCTOU: verification and extraction
    /// consume one immutable-in-practice in-memory buffer, so even an in-place
    /// overwrite of the downloaded archive path cannot change staged bytes.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_extract_archive_reads_the_verified_memory_buffer() {
        let root = test_tempdir();
        let path = root.path().join("release.tar.gz");
        write_test_tar_gz(&path, b"original-bytes");
        let expected_hash = sha256_file(&path).unwrap();

        let archive = fs::read(&path).unwrap();
        verify_sha256_bytes(&archive, &expected_hash).unwrap();

        // Simulate a same-user writer replacing the path after verification.
        // Extraction has no file handle to race: it only sees `archive`.
        let malicious = root.path().join("malicious.tar.gz");
        write_test_tar_gz(&malicious, b"attacker-bytes");
        fs::rename(&malicious, &path).unwrap();
        assert_ne!(sha256_file(&path).unwrap(), expected_hash);

        let destination = root.path().join("stage");
        fs::create_dir(&destination).unwrap();
        extract_archive(&archive, &destination).unwrap();

        assert_eq!(
            fs::read(destination.join("kettle/payload")).unwrap(),
            b"original-bytes",
            "extraction must read the bytes verify_sha256_bytes hashed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_memory_buffer_hash_mismatch_fails_closed() {
        let error = verify_sha256_bytes(b"downloaded", &"00".repeat(32)).unwrap_err();
        assert!(matches!(error, UpdateError::HashMismatch));
    }

    /// Regression test for the Windows half of the same archive TOCTOU: the
    /// exclusive lock `install_update_into` takes on the downloaded archive
    /// must be a mandatory, kernel-enforced lock that blocks a concurrent
    /// same-user writer from overwriting the file's bytes in place, not
    /// merely an advisory courtesy that a hostile writer can ignore.
    #[cfg(windows)]
    #[test]
    fn windows_archive_lock_blocks_a_concurrent_in_place_overwrite() {
        let root = test_tempdir();
        let archive = tempfile::Builder::new()
            .prefix("kettle-update-download-")
            .tempfile_in(root.path())
            .unwrap();
        fs::write(archive.path(), b"original-bytes").unwrap();
        fs4::FileExt::lock(archive.as_file()).unwrap();

        let overwrite_attempt = OpenOptions::new()
            .write(true)
            .open(archive.path())
            .and_then(|mut contender| contender.write_all(b"attacker-bytes"));
        assert!(
            overwrite_attempt.is_err(),
            "a concurrent writer must not be able to modify a locked archive in place"
        );

        fs4::FileExt::unlock(archive.as_file()).unwrap();
        // Verify the blocked write left the original bytes intact only AFTER
        // unlocking: the exclusive lock blocks reads through a fresh handle
        // too (ERROR_LOCK_VIOLATION), so this check can't use `fs::read` while
        // the lock is held. The write-blocked assertion above is what proves
        // the protection; this confirms the file wasn't corrupted.
        assert_eq!(fs::read(archive.path()).unwrap(), b"original-bytes");

        OpenOptions::new()
            .write(true)
            .open(archive.path())
            .unwrap()
            .write_all(b"ok-after-unlock")
            .unwrap();
        assert_eq!(fs::read(archive.path()).unwrap(), b"ok-after-unlock");
    }

    #[test]
    fn atomic_state_write_replaces_existing_file() {
        let temp = test_tempdir();
        let path = temp.path().join("state.json");
        fs::write(&path, b"old").unwrap();

        write_atomic_file(&path, b"new").unwrap();

        assert_eq!(fs::read(path).unwrap(), b"new");
    }

    #[cfg(windows)]
    #[test]
    fn windows_run_lock_and_target_handle_gate_replacement() {
        let root = test_tempdir();
        let child_executable = root.path().join("kettle.exe");
        fs::copy(std::env::current_exe().unwrap(), &child_executable).unwrap();
        let blocked_target_path = root.path().join("kettle.com");
        fs::write(&blocked_target_path, b"launcher").unwrap();
        let lock_path = root.path().join(RUNNING_LOCK_FILE);
        let ready = root.path().join("ready");
        let release = root.path().join("release");
        let mut child = std::process::Command::new(&child_executable)
            .args([
                "--ignored",
                "--exact",
                "install::tests::windows_update_lock_child",
                "--nocapture",
            ])
            .env("KETTLE_TEST_RUNNING_LOCK", &lock_path)
            .env("KETTLE_TEST_RUNNING_READY", &ready)
            .env("KETTLE_TEST_RUNNING_RELEASE", &release)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !ready.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(ready.is_file(), "child never acquired the shared run lock");

        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert!(matches!(
            fs4::FileExt::try_lock(&contender),
            Err(fs4::TryLockError::WouldBlock)
        ));

        use std::os::windows::fs::OpenOptionsExt as _;
        let mut blocked_options = OpenOptions::new();
        blocked_options.read(true).share_mode(0);
        let blocked_target = blocked_options.open(&blocked_target_path).unwrap();
        let (unblock_tx, unblock_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let release_path = release.clone();
        let blocker = std::thread::spawn(move || {
            unblock_rx.recv().unwrap();
            drop(blocked_target);
            fs::write(release_path, b"release").unwrap();
        });
        let prefix = root.path().to_path_buf();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            done_tx
                .send(wait_for_windows_update_targets(&prefix))
                .unwrap();
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err(),
            "a no-share target handle did not delay replacement"
        );
        unblock_tx.send(()).unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("target probe did not resume after the handle closed")
            .unwrap();
        waiter.join().unwrap();
        blocker.join().unwrap();
        assert!(child.wait().unwrap().success());

        let exclusive = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        fs4::FileExt::lock(&exclusive).unwrap();
        atomic_write(&child_executable, b"replacement", None).unwrap();
        assert_eq!(fs::read(child_executable).unwrap(), b"replacement");
    }

    #[cfg(windows)]
    #[test]
    fn windows_stale_cleanup_does_not_race_active_update_preparation() {
        let root = test_tempdir();
        let stage = root.path().join(".kettle-update-stage-123-456");
        let helper = root.path().join(".kettle-update-helper-123-456.exe");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("README.md"), b"still preparing").unwrap();
        fs::write(&helper, b"helper").unwrap();

        let update_lock =
            kettle_state::ExclusiveFileLock::acquire(&root.path().join(".kettle-update.lock"))
                .unwrap();
        assert!(!cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert!(stage.is_dir());
        assert!(helper.is_file());

        drop(update_lock);
        assert!(cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert!(!stage.exists());
        assert!(!helper.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_stale_cleanup_preserves_an_unmanaged_exact_named_stage() {
        let root = test_tempdir();
        let stage = root.path().join(".kettle-update-stage-123-456");
        fs::create_dir(&stage).unwrap();
        let sentinel = stage.join("user-data.txt");
        fs::write(&sentinel, b"must survive").unwrap();

        assert!(cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert_eq!(fs::read(sentinel).unwrap(), b"must survive");
    }

    #[cfg(windows)]
    #[test]
    fn windows_stale_cleanup_removes_only_bounded_marked_orphan_backups() {
        let root = test_tempdir();
        let valid = root.path().join(".kettle-update-backup-123-456");
        fs::create_dir(&valid).unwrap();
        fs::write(valid.join("README.md"), b"old readme").unwrap();
        fs::write(
            valid.join(BACKUP_MARKER_FILE),
            serde_json::to_vec(&BackupMarker {
                schema: JOURNAL_SCHEMA,
                product: "kettle".into(),
                transaction_id: "123-456".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert!(!valid.exists());

        let invalid = root.path().join(".kettle-update-backup-789-1000");
        fs::create_dir(&invalid).unwrap();
        fs::write(invalid.join("unmanaged"), b"must survive").unwrap();
        fs::write(
            invalid.join(BACKUP_MARKER_FILE),
            serde_json::to_vec(&BackupMarker {
                schema: JOURNAL_SCHEMA,
                product: "kettle".into(),
                transaction_id: "789-1000".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert!(invalid.join("unmanaged").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn windows_pending_archive_name_is_bound_to_its_transaction_id() {
        let root = test_tempdir();
        let (name, archive) = create_windows_pending_archive(root.path(), "123-456").unwrap();
        assert_eq!(name, ".kettle-update-archive-123-456.zip");
        let path = root.path().join(name);
        assert!(fs::rename(&path, root.path().join("swapped.zip")).is_err());
        mark_windows_handle_for_deletion(&archive).unwrap();
        drop(archive);
        assert!(!path.exists());
        assert!(create_windows_pending_archive(root.path(), "not-exact").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_staging_rejects_unmanaged_shell_and_nested_files() {
        let root = test_tempdir();
        for relative in [
            "kettle.exe",
            "kettle.com",
            "install.ps1",
            "shell-integration/kettle.ps1",
        ] {
            let path = root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        fs::write(
            root.path().join("shell-integration/unmanaged.ps1"),
            b"fixture",
        )
        .unwrap();
        let error = validate_windows_staging(root.path()).unwrap_err();
        assert!(error.to_string().contains("unexpected release file"));

        fs::remove_file(root.path().join("shell-integration/unmanaged.ps1")).unwrap();
        fs::create_dir(root.path().join("shell-integration/nested")).unwrap();
        fs::write(
            root.path().join("shell-integration/nested/kettle.ps1"),
            b"fixture",
        )
        .unwrap();
        let error = validate_windows_staging(root.path()).unwrap_err();
        assert!(error.to_string().contains("unexpected release file"));
    }

    #[cfg(windows)]
    #[test]
    fn post_commit_action_runs_after_running_then_update_lock_release() {
        let root = test_tempdir();
        let update_path = root.path().join(".kettle-update.lock");
        let running_path = root.path().join(RUNNING_LOCK_FILE);
        let update_lock = kettle_state::ExclusiveFileLock::acquire(&update_path).unwrap();
        let running_lock = kettle_state::ExclusiveFileLock::acquire(&running_path).unwrap();

        release_windows_update_locks_then(running_lock, update_lock, || {
            let update = kettle_state::ExclusiveFileLock::try_acquire(&update_path).unwrap();
            let running = kettle_state::ExclusiveFileLock::try_acquire(&running_path).unwrap();
            assert!(update.is_some(), "update lock remained held");
            assert!(running.is_some(), "running lock remained held");
        });
    }

    #[cfg(windows)]
    #[test]
    fn failed_pending_quarantine_is_bounded_by_transaction() {
        let root = test_tempdir();
        for index in 0..12 {
            for extension in ["json", "txt"] {
                fs::write(
                    root.path()
                        .join(format!("{FAILED_PENDING_PREFIX}{index}-1000.{extension}")),
                    b"evidence",
                )
                .unwrap();
            }
        }
        prune_failed_pending_records(root.path(), MAX_FAILED_PENDING_TRANSACTIONS).unwrap();
        let transactions = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_str()?;
                failed_pending_record_name(name).map(|(transaction, _)| transaction.to_string())
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(transactions.len(), MAX_FAILED_PENDING_TRANSACTIONS);
    }

    #[cfg(windows)]
    #[test]
    fn failed_pending_pruning_orders_by_epoch_then_process_id() {
        let root = test_tempdir();
        for transaction in ["9999-1", "1-2", "1-3", "2-3"] {
            fs::write(
                root.path()
                    .join(format!("{FAILED_PENDING_PREFIX}{transaction}.json")),
                b"evidence",
            )
            .unwrap();
        }

        prune_failed_pending_records(root.path(), 2).unwrap();

        assert!(
            !root
                .path()
                .join(format!("{FAILED_PENDING_PREFIX}9999-1.json"))
                .exists()
        );
        assert!(
            !root
                .path()
                .join(format!("{FAILED_PENDING_PREFIX}1-2.json"))
                .exists()
        );
        assert!(
            root.path()
                .join(format!("{FAILED_PENDING_PREFIX}1-3.json"))
                .is_file()
        );
        assert!(
            root.path()
                .join(format!("{FAILED_PENDING_PREFIX}2-3.json"))
                .is_file()
        );
    }

    #[cfg(windows)]
    const PENDING_TEST_SECRET: [u8; 32] = [19; 32];

    #[cfg(windows)]
    fn pending_test_key() -> [u8; 32] {
        SigningKey::from_bytes(&PENDING_TEST_SECRET)
            .verifying_key()
            .to_bytes()
    }

    #[cfg(windows)]
    fn pending_test_now() -> SystemTime {
        UNIX_EPOCH + std::time::Duration::from_secs(1_785_456_000)
    }

    #[cfg(windows)]
    fn test_windows_package_zip(version: &str) -> Vec<u8> {
        let payloads = [
            ("kettle.exe", b"verified-gui".as_slice()),
            ("kettle.com", b"verified-console".as_slice()),
            ("install.ps1", b"verified-installer".as_slice()),
            ("README.md", b"verified-readme".as_slice()),
        ];
        let manifest = PackageManifest {
            schema: 1,
            product: "kettle".into(),
            target: current_target().unwrap().into(),
            version: version.into(),
            files: payloads
                .iter()
                .map(|(path, bytes)| PackageFile {
                    path: (*path).into(),
                    size: bytes.len() as u64,
                    sha256: sha256_bytes(bytes),
                    mode: None,
                })
                .collect(),
        };
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (path, bytes) in payloads {
            writer.start_file(path, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.start_file(PACKAGE_MANIFEST_FILE, options).unwrap();
        writer.write_all(&manifest).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[cfg(windows)]
    fn seed_windows_pending(prefix: &Path, attempts: u32) -> PendingUpdate {
        fs::create_dir_all(prefix).unwrap();
        let transaction_id = "123-456";
        let archive = format!(".kettle-update-archive-{transaction_id}.zip");
        let helper = format!(".kettle-update-helper-{transaction_id}.exe");
        let archive_bytes = b"retained signed archive fixture";
        fs::write(prefix.join(&archive), archive_bytes).unwrap();
        fs::copy(std::env::current_exe().unwrap(), prefix.join(&helper)).unwrap();
        let helper_size = fs::metadata(prefix.join(&helper)).unwrap().len();
        let helper_sha256 = sha256_file(&prefix.join(&helper)).unwrap();
        let archive_sha256 = sha256_bytes(archive_bytes);
        let asset = crate::ManifestAsset {
            target: current_target().unwrap().into(),
            name: "kettle-windows-x86_64.zip".into(),
            size: archive_bytes.len() as u64,
            sha256: archive_sha256.clone(),
        };
        let release = crate::Manifest {
            schema: 1,
            product: "kettle".into(),
            channel: "stable".into(),
            version: "99.0.0".into(),
            tag: "v99.0.0".into(),
            published_at: "2026-07-31T00:00:00Z".into(),
            assets: vec![asset.clone()],
        };
        let release_manifest = serde_json::to_string(&release).unwrap();
        let mut signed = crate::SIGNING_CONTEXT.to_vec();
        signed.extend_from_slice(release_manifest.as_bytes());
        let release_signature = base64::engine::general_purpose::STANDARD.encode(
            SigningKey::from_bytes(&PENDING_TEST_SECRET)
                .sign(&signed)
                .to_bytes(),
        );
        let package_manifest = serde_json::to_string(&PackageManifest {
            schema: 1,
            product: "kettle".into(),
            target: current_target().unwrap().into(),
            version: "99.0.0".into(),
            files: vec![PackageFile {
                path: "kettle.exe".into(),
                size: 1,
                sha256: "0".repeat(64),
                mode: None,
            }],
        })
        .unwrap();
        let pending = PendingUpdate {
            schema: PENDING_SCHEMA,
            product: "kettle".into(),
            target: current_target().unwrap().into(),
            transaction_id: transaction_id.into(),
            target_version: "99.0.0".into(),
            archive,
            archive_size: archive_bytes.len() as u64,
            archive_sha256,
            release_manifest,
            release_signature,
            asset,
            package_manifest,
            helper,
            helper_size,
            helper_sha256,
            attempts,
            handoff_timeouts: 0,
            last_error: Some("fixture failure".into()),
        };
        persist_pending(prefix, &pending).unwrap();
        pending
    }

    #[cfg(windows)]
    #[test]
    fn windows_verified_helper_and_archive_retain_every_consumed_identity() {
        let root = test_tempdir();
        let pending = seed_windows_pending(root.path(), 0);
        let helper = verify_pending_helper(root.path(), &pending).unwrap();
        let archive = verify_pending_archive(root.path(), &pending).unwrap();

        assert!(
            fs::rename(
                root.path().join(&pending.helper),
                root.path().join("swapped-helper.exe")
            )
            .is_err()
        );
        assert!(
            fs::rename(
                root.path().join(&pending.archive),
                root.path().join("swapped-archive.zip")
            )
            .is_err()
        );

        drop(archive);
        drop(helper);
    }

    #[cfg(windows)]
    #[test]
    fn forged_pending_record_with_correct_local_hashes_is_rejected() {
        let root = test_tempdir();
        let mut pending = seed_windows_pending(root.path(), 0);
        verify_pending_helper(root.path(), &pending).unwrap();
        verify_pending_archive(root.path(), &pending).unwrap();
        pending.release_signature = base64::engine::general_purpose::STANDARD.encode([0_u8; 64]);
        persist_pending(root.path(), &pending).unwrap();

        let inspection = inspect_pending_start_with(
            root.path(),
            &pending_test_key(),
            pending_test_now(),
            &semver::Version::new(2, 43, 0),
        )
        .unwrap();
        assert!(matches!(
            inspection,
            PendingStartInspection::Failed { reason, .. }
                if reason.contains("signature is invalid")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn authenticated_pending_update_cannot_downgrade_installed_version() {
        let root = test_tempdir();
        seed_windows_pending(root.path(), 0);

        let inspection = inspect_pending_start_with(
            root.path(),
            &pending_test_key(),
            pending_test_now(),
            &semver::Version::new(100, 0, 0),
        )
        .unwrap();
        assert!(matches!(
            inspection,
            PendingStartInspection::Failed { reason, .. }
                if reason.contains("refusing to replace installed Kettle 100.0.0 with 99.0.0")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn installed_version_comes_from_the_executable_fixed_version_resource() {
        use windows_sys::Win32::Storage::FileSystem::{VS_FFI_SIGNATURE, VS_FIXEDFILEINFO};

        let mut fixed = unsafe { std::mem::zeroed::<VS_FIXEDFILEINFO>() };
        fixed.dwSignature = VS_FFI_SIGNATURE as u32;
        fixed.dwProductVersionMS = (2 << 16) | 45;
        fixed.dwProductVersionLS = 7 << 16;
        assert_eq!(
            version_from_fixed_file_info(&fixed).unwrap(),
            semver::Version::new(2, 45, 7)
        );
        fixed.dwProductVersionLS |= 1;
        assert!(version_from_fixed_file_info(&fixed).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn verified_archive_bytes_are_the_bytes_the_transaction_applies() {
        let root = test_tempdir();
        let archive_path = root.path().join("release.zip");
        fs::write(&archive_path, test_windows_package_zip("99.0.0")).unwrap();
        let mut archive = File::open(&archive_path).unwrap();
        let package = load_windows_package(&mut archive, &fake_update(), None).unwrap();
        drop(archive);
        fs::write(&archive_path, b"attacker replacement").unwrap();

        let prefix = root.path().join("install");
        fs::create_dir(&prefix).unwrap();
        fs::write(prefix.join("kettle.exe"), b"old-gui").unwrap();
        let install = ManagedInstall {
            prefix: prefix.clone(),
            executable: prefix.join("kettle.exe"),
            marker_path: prefix.join(".kettle-install.json"),
        };
        let mut transaction = Transaction::begin(&prefix, "99.0.0").unwrap();
        apply_verified_windows_update(&mut transaction, &package, &install, &fake_update())
            .unwrap();

        assert_eq!(
            fs::read(prefix.join("kettle.exe")).unwrap(),
            b"verified-gui"
        );
        assert_eq!(
            fs::read(prefix.join("install.ps1")).unwrap(),
            b"verified-installer"
        );
        transaction.rollback().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn post_update_integration_script_is_retained_from_verified_bytes() {
        let root = test_tempdir();
        let script = root.path().join("install.ps1");
        fs::write(&script, b"verified script").unwrap();
        assert!(retain_verified_integration_script(&script, b"other script").is_err());

        let retained = retain_verified_integration_script(&script, b"verified script").unwrap();
        assert!(fs::write(&script, b"attacker script").is_err());
        assert!(fs::rename(&script, root.path().join("swapped.ps1")).is_err());
        drop(retained);
    }

    #[cfg(windows)]
    #[test]
    fn pending_handoff_timeouts_have_a_grace_budget_and_reset_on_attempt() {
        let root = test_tempdir();
        let pending = seed_windows_pending(root.path(), 0);
        let timeout = UpdateError::Transaction("still running".into());
        let transaction_epoch = transaction_id_parts(&pending.transaction_id).unwrap().1;

        record_pending_handoff_timeout_locked(
            root.path(),
            &timeout,
            transaction_epoch + HANDOFF_TIMEOUT_GRACE_NANOS - 1,
        );
        assert_eq!(load_pending(root.path()).unwrap().handoff_timeouts, 0);
        for offset in 0..MAX_HANDOFF_TIMEOUTS {
            record_pending_handoff_timeout_locked(
                root.path(),
                &timeout,
                transaction_epoch + HANDOFF_TIMEOUT_GRACE_NANOS + u128::from(offset),
            );
        }
        let recorded = load_pending(root.path()).unwrap();
        assert_eq!(recorded.handoff_timeouts, MAX_HANDOFF_TIMEOUTS);
        assert!(
            recorded
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("still running"))
        );
        assert!(matches!(
            inspect_pending_start_with(
                root.path(),
                &pending_test_key(),
                pending_test_now(),
                &semver::Version::new(2, 43, 0),
            ),
            Some(PendingStartInspection::Failed { reason, .. })
                if reason.contains("still-running") && reason.contains("3 times")
        ));

        let helper = root.path().join(&pending.helper).canonicalize().unwrap();
        let started = begin_pending_attempt(root.path(), &helper).unwrap();
        assert_eq!(started.handoff_timeouts, 0);
        assert!(started.last_error.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn nonregular_pending_state_is_never_quarantined_into_managed_evidence() {
        let root = test_tempdir();
        fs::create_dir(root.path().join(PENDING_FILE)).unwrap();

        let PendingStartInspection::Failed {
            fingerprint,
            reason,
        } = inspect_pending_start(root.path()).unwrap()
        else {
            panic!("a directory at the pending path must fail safely");
        };
        assert!(fingerprint.is_none());
        let warning = quarantine_pending_warning(root.path(), &fingerprint, reason);
        assert!(warning.contains("Evidence remains"));
        assert!(root.path().join(PENDING_FILE).is_dir());
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(FAILED_PENDING_PREFIX)
        }));
    }

    #[cfg(windows)]
    #[test]
    fn windows_invalid_and_exhausted_pending_updates_are_quarantined() {
        let invalid = test_tempdir();
        fs::write(invalid.path().join(PENDING_FILE), b"not json").unwrap();
        let PendingStartInspection::Failed {
            fingerprint,
            reason,
        } = inspect_pending_start(invalid.path()).unwrap()
        else {
            panic!("invalid pending state must fail closed into quarantine");
        };
        let warning = quarantine_pending_warning(invalid.path(), &fingerprint, reason);
        assert!(warning.contains("kept the currently installed version"));
        assert!(!invalid.path().join(PENDING_FILE).exists());
        assert!(fs::read_dir(invalid.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(FAILED_PENDING_PREFIX)
        }));

        let exhausted = test_tempdir();
        seed_windows_pending(exhausted.path(), MAX_PENDING_ATTEMPTS);
        let PendingStartInspection::Failed {
            fingerprint,
            reason,
        } = inspect_pending_start_with(
            exhausted.path(),
            &pending_test_key(),
            pending_test_now(),
            &semver::Version::new(2, 43, 0),
        )
        .unwrap()
        else {
            panic!("attempt limit must stop automatic helper retries");
        };
        let warning = quarantine_pending_warning(exhausted.path(), &fingerprint, reason);
        assert!(warning.contains("failed 3 times"));
        assert!(!exhausted.path().join(PENDING_FILE).exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_pending_attempt_is_checkpointed_before_fallible_work() {
        let root = test_tempdir();
        let pending = seed_windows_pending(root.path(), 1);
        let helper = root.path().join(&pending.helper).canonicalize().unwrap();

        let started = begin_pending_attempt(root.path(), &helper).unwrap();

        assert_eq!(started.attempts, 2);
        assert!(started.last_error.is_none());
        let persisted = load_pending(root.path()).unwrap();
        assert_eq!(persisted.attempts, 2);
        assert!(persisted.last_error.is_none());
    }

    /// Regression test: a stuck (not crashed) holder of the running-instances
    /// lock must not wedge the pending-update helper forever. The helper
    /// should give up once its bounded timeout elapses and leave an
    /// actionable `last_error` behind for the next launch's
    /// `inspect_pending_start` to surface, instead of blocking indefinitely
    /// with no diagnostic and no escape hatch.
    #[cfg(windows)]
    #[test]
    fn pending_helper_gives_up_on_a_stuck_running_lock_instead_of_hanging_forever() {
        let root = test_tempdir();
        let pending = seed_windows_pending(root.path(), 0);
        let helper = root.path().join(&pending.helper).canonicalize().unwrap();

        // Simulate a wedged Kettle process (e.g. a GPU device-loss/TDR hang)
        // that holds the shared run lock without ever releasing it.
        let running_lock_path = root.path().join(RUNNING_LOCK_FILE);
        let stuck_holder = kettle_state::ExclusiveFileLock::acquire(&running_lock_path).unwrap();

        let error = run_pending_update_helper_inner_with_timeout(
            root.path(),
            &helper,
            std::time::Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.to_string().contains("may be stuck"));

        let recorded = load_pending(root.path()).unwrap();
        assert!(
            recorded
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("may be stuck")),
            "a timed-out helper must leave an actionable last_error for the next launch"
        );
        // The attempt counter must be untouched: the helper never got far
        // enough to call `begin_pending_attempt`.
        assert_eq!(recorded.attempts, 0);

        drop(stuck_holder);
    }

    #[cfg(windows)]
    #[test]
    fn windows_quarantine_failure_never_blocks_startup_recovery() {
        let root = test_tempdir();
        fs::write(root.path().join(PENDING_FILE), b"not json").unwrap();
        fs::create_dir(root.path().join(".kettle-update.lock")).unwrap();
        let fingerprint = pending_file_fingerprint(&root.path().join(PENDING_FILE)).unwrap();

        let warning = quarantine_pending_warning(
            root.path(),
            &Some(fingerprint),
            "test quarantine failure".into(),
        );

        assert!(warning.contains("kept the currently installed version"));
        assert!(root.path().join(PENDING_FILE).is_file());
    }

    #[cfg(windows)]
    #[test]
    fn windows_failure_checkpoint_precedes_a_later_helper_success() {
        let root = test_tempdir();
        seed_windows_pending(root.path(), 1);
        let running_path = root.path().join(RUNNING_LOCK_FILE);
        let failure_actor = kettle_state::ExclusiveFileLock::acquire(&running_path).unwrap();
        let prefix = root.path().to_path_buf();
        let success = std::thread::spawn(move || {
            let _success_actor =
                kettle_state::ExclusiveFileLock::acquire(&prefix.join(RUNNING_LOCK_FILE)).unwrap();
            fs::remove_file(prefix.join(PENDING_FILE)).unwrap();
        });
        record_pending_failure(
            &failure_actor,
            root.path(),
            &UpdateError::Transaction("first helper failed".into()),
        );
        assert!(
            load_pending(root.path())
                .unwrap()
                .last_error
                .unwrap()
                .contains("first helper failed")
        );
        drop(failure_actor);
        success.join().unwrap();
        assert!(
            !root.path().join(PENDING_FILE).exists(),
            "a failure actor must never recreate pending state after a later helper succeeds"
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "child process fixture; invoked by windows_run_lock_and_target_handle_gate_replacement"]
    fn windows_update_lock_child() {
        let Ok(lock_path) = std::env::var("KETTLE_TEST_RUNNING_LOCK") else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os("KETTLE_TEST_RUNNING_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("KETTLE_TEST_RUNNING_RELEASE").unwrap());
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        fs4::FileExt::lock_shared(&lock).unwrap();
        fs::write(ready, b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !release.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(release.is_file(), "parent never released child fixture");
    }
}
