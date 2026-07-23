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
use std::io::{Read, Seek};
#[cfg(any(windows, target_os = "linux"))]
use std::path::Component;
#[cfg(any(windows, target_os = "linux"))]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
#[cfg(any(windows, target_os = "linux"))]
use sha2::{Digest as _, Sha256};

use crate::current_target;
use crate::feed::{AvailableUpdate, FeedClient, UpdateError};

const MARKER_SCHEMA: u32 = 1;
#[cfg(any(windows, target_os = "linux"))]
const JOURNAL_SCHEMA: u32 = 2;
#[cfg(any(windows, target_os = "linux"))]
const MAX_ARCHIVE_ENTRIES: usize = 128;
#[cfg(any(windows, target_os = "linux"))]
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(any(windows, target_os = "linux"))]
const PACKAGE_MANIFEST_FILE: &str = "kettle-package-manifest.json";
#[cfg(windows)]
const PENDING_SCHEMA: u32 = 1;
#[cfg(windows)]
const PENDING_FILE: &str = ".kettle-update-pending.json";
#[cfg(windows)]
const RUNNING_LOCK_FILE: &str = ".kettle-running.lock";
#[cfg(windows)]
const MAX_PENDING_ATTEMPTS: u32 = 3;
#[cfg(windows)]
const FAILED_PENDING_PREFIX: &str = ".kettle-update-failed-";
#[cfg(windows)]
const MAX_PENDING_RECORD_BYTES: usize = 1024 * 1024;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingUpdate {
    schema: u32,
    product: String,
    target: String,
    transaction_id: String,
    target_version: String,
    staging_dir: String,
    helper: String,
    files: Vec<PendingFile>,
    attempts: u32,
    #[serde(default)]
    last_error: Option<String>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingFile {
    path: String,
    size: u64,
    sha256: String,
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
    let fingerprint = pending_file_fingerprint(&path).ok();
    if !metadata.file_type().is_file() {
        return Some(PendingStartInspection::Failed {
            fingerprint,
            reason: "the pending update record is not a regular file".into(),
        });
    }
    match load_pending(prefix) {
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
        Ok(_) => Some(PendingStartInspection::Retry { fingerprint }),
        Err(error) => Some(PendingStartInspection::Failed {
            fingerprint,
            reason: format!("the pending update record is invalid: {error}"),
        }),
    }
}

#[cfg(windows)]
fn pending_file_fingerprint(path: &Path) -> Result<String, UpdateError> {
    let metadata = fs::symlink_metadata(path)?;
    let kind = if metadata.file_type().is_file() {
        "file"
    } else if metadata.file_type().is_symlink() {
        "symlink"
    } else if metadata.file_type().is_dir() {
        "directory"
    } else {
        "other"
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    let mut identity = format!("{kind}:{}:{modified}:", metadata.len());
    if metadata.file_type().is_file() {
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(MAX_PENDING_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        identity.push_str(&sha256_bytes(&bytes));
    }
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
    let pending_path = prefix.join(PENDING_FILE);
    let current = match pending_file_fingerprint(&pending_path) {
        Ok(fingerprint) => fingerprint,
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if current != expected_fingerprint {
        return Ok(None);
    }
    let suffix = unique_suffix();
    let quarantined = prefix.join(format!("{FAILED_PENDING_PREFIX}{suffix}.json"));
    fs::rename(&pending_path, &quarantined)?;
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

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema: u32,
    product: String,
    target: String,
    version: String,
    files: Vec<PackageFile>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageFile {
    path: String,
    size: u64,
    sha256: String,
    /// Portable Unix permission bits. Windows packages use `null`.
    mode: Option<u32>,
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
    #[cfg(not(windows))]
    {
        Ok(ProcessStart::Ready {
            guard: RunningInstallGuard {},
            warning: None,
        })
    }
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

#[cfg(any(windows, target_os = "linux"))]
pub fn detect_managed_install() -> Result<ManagedInstall, UpdateError> {
    let executable = std::env::current_exe()?;
    detect_managed_install_at(&executable)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn detect_managed_install() -> Result<ManagedInstall, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(any(windows, target_os = "linux"))]
fn detect_managed_install_at(executable: &Path) -> Result<ManagedInstall, UpdateError> {
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
    let install = detect_managed_install()?;
    install_update_into(client, update, &install)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn install_update(
    _client: &FeedClient,
    _update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(any(windows, target_os = "linux"))]
fn install_update_into(
    client: &FeedClient,
    update: &AvailableUpdate,
    install: &ManagedInstall,
) -> Result<InstallOutcome, UpdateError> {
    let asset = update
        .asset
        .as_ref()
        .ok_or(UpdateError::UnsupportedPlatform)?;
    if current_target() != Some(asset.target.as_str()) {
        return Err(UpdateError::MalformedManifest(
            "selected artifact does not match this platform".to_string(),
        ));
    }
    let lock_path = install.prefix.join(".kettle-update.lock");
    let _lock = kettle_state::ExclusiveFileLock::try_acquire(&lock_path)?
        .ok_or(UpdateError::UpdateLocked)?;
    recover_transaction(&install.prefix)?;

    let mut archive = tempfile::Builder::new()
        .prefix("kettle-update-download-")
        .tempfile()?;
    // Hold an exclusive lock on the archive for its entire lifetime, from
    // before any bytes are written until after extraction has read them.
    // `NamedTempFile` does not request exclusive sharing (Windows keeps the
    // default FILE_SHARE_READ|FILE_SHARE_WRITE; Unix leaves it a normal 0600
    // file), so a same-user process that already has the path open could
    // otherwise overwrite bytes in place while we still hold our handle. On
    // Windows this lock is a mandatory, kernel-enforced byte-range lock that
    // fails any other process's read/write touching it, lock-aware or not;
    // on Unix it is advisory only, so it does not stop a hostile writer, but
    // the same-handle discipline below still closes the delete-and-recreate
    // variant of this race there.
    fs4::FileExt::lock(archive.as_file())?;
    client.download_to(update, archive.as_file_mut())?;
    archive.as_file_mut().flush()?;
    archive.as_file().sync_all()?;
    // Verify and extract from the very same open handle rather than
    // re-resolving `archive.path()` a second time for each step. Re-opening
    // by path here would let another same-user process substitute a
    // different file (or a delete-and-recreate at the same name) in the gap
    // between the two opens, defeating the SHA-256/signature verification
    // that is this crate's entire security model.
    verify_sha256(archive.as_file_mut(), &asset.sha256)?;

    let staging = tempfile::Builder::new()
        .prefix(".kettle-update-stage-")
        .tempdir_in(&install.prefix)?;
    extract_archive(archive.as_file_mut(), staging.path())?;

    #[cfg(windows)]
    let package_root = staging.path().to_path_buf();
    #[cfg(target_os = "linux")]
    let package_root = staging.path().join("kettle");
    let package_manifest = package_root.join(PACKAGE_MANIFEST_FILE);
    if package_manifest.is_file() {
        verify_package_manifest(&package_root, update)?;
    }

    #[cfg(windows)]
    {
        stage_windows_update(staging, install, update)
    }

    #[cfg(target_os = "linux")]
    {
        let mut transaction = Transaction::begin(&install.prefix, &update.version.to_string())?;
        let result = apply_staged_update(&mut transaction, staging.path(), install, update);
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

#[cfg(windows)]
fn stage_windows_update(
    staging: tempfile::TempDir,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    if install.prefix.join(PENDING_FILE).exists() {
        return Err(UpdateError::UpdateLocked);
    }
    validate_windows_staging(staging.path())?;
    let files = pending_files(staging.path())?;
    let staging_path = staging.keep();
    let transaction_id = unique_suffix();
    let helper_name = format!(".kettle-update-helper-{transaction_id}.exe");
    let helper_path = install.prefix.join(&helper_name);

    let result = (|| {
        copy_file_new_durable(&install.executable, &helper_path)?;
        let staging_dir = staging_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| UpdateError::Transaction("invalid staging directory name".into()))?
            .to_string();
        let pending = PendingUpdate {
            schema: PENDING_SCHEMA,
            product: "kettle".into(),
            target: current_target().unwrap_or_default().into(),
            transaction_id: transaction_id.clone(),
            target_version: update.version.to_string(),
            staging_dir,
            helper: helper_name,
            files,
            attempts: 0,
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
            let _ = remove_staging_dir_checked(&install.prefix, &staging_path);
        }
        return Err(error);
    }

    Ok(InstallOutcome {
        version: update.version.clone(),
        executable: install.executable.clone(),
        disposition: InstallDisposition::Staged { transaction_id },
    })
}

#[cfg(windows)]
fn validate_windows_staging(staging: &Path) -> Result<(), UpdateError> {
    require_file(&staging.join("kettle.exe"), "kettle.exe")?;
    require_file(&staging.join("kettle.com"), "kettle.com")?;
    require_file(&staging.join("install.ps1"), "install.ps1")?;
    for source in collect_files(staging)? {
        let relative = source.strip_prefix(staging).map_err(|_| {
            UpdateError::UnsafeArchive(format!("escaped staging: {}", source.display()))
        })?;
        validate_archive_path(relative)?;
        let root = relative
            .components()
            .next()
            .and_then(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .ok_or_else(|| UpdateError::UnsafeArchive(relative.display().to_string()))?;
        if !WINDOWS_ALLOWED_ROOTS.contains(&root) {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected release file {}",
                relative.display()
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn pending_files(staging: &Path) -> Result<Vec<PendingFile>, UpdateError> {
    let mut files = Vec::new();
    let mut total = 0_u64;
    for file in collect_files(staging)? {
        let relative = file
            .strip_prefix(staging)
            .map_err(|_| UpdateError::UnsafeArchive(file.display().to_string()))?;
        let size = fs::metadata(&file)?.len();
        total = total
            .checked_add(size)
            .ok_or_else(|| UpdateError::UnsafeArchive("staged size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "staged data exceeds the safety limit".into(),
            ));
        }
        files.push(PendingFile {
            path: relative_to_string(relative)?,
            size,
            sha256: sha256_file(&file)?,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

#[cfg(windows)]
fn copy_file_new_durable(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    let mut source = File::open(source)?;
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(destination)?;
    std::io::copy(&mut source, &mut destination)?;
    destination.flush()?;
    destination.sync_all()?;
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
    let valid_id = !pending.transaction_id.is_empty()
        && pending
            .transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-');
    if pending.schema != PENDING_SCHEMA
        || pending.product != "kettle"
        || current_target() != Some(pending.target.as_str())
        || !valid_id
        || semver::Version::parse(&pending.target_version).is_err()
        || pending.helper != format!(".kettle-update-helper-{}.exe", pending.transaction_id)
        || !pending.staging_dir.starts_with(".kettle-update-stage-")
        || pending.staging_dir.contains(['/', '\\'])
        || pending.files.is_empty()
        || pending.files.len() > MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::Transaction(
            "pending update record failed validation".into(),
        ));
    }
    if prefix.join(&pending.staging_dir).parent() != Some(prefix)
        || prefix.join(&pending.helper).parent() != Some(prefix)
        || !prefix.join(&pending.staging_dir).is_dir()
        || !prefix.join(&pending.helper).is_file()
    {
        return Err(UpdateError::Transaction(
            "pending update artifacts are missing or outside the install prefix".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0_u64;
    for file in &pending.files {
        validate_archive_path(Path::new(&file.path))?;
        if !seen.insert(file.path.to_ascii_lowercase())
            || !is_sha256(&file.sha256)
            || file.size > MAX_UNPACKED_BYTES
        {
            return Err(UpdateError::Transaction(format!(
                "invalid pending file record {}",
                file.path
            )));
        }
        total = total
            .checked_add(file.size)
            .ok_or_else(|| UpdateError::Transaction("pending file size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::Transaction(
                "pending files exceed the safety limit".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn verify_pending_staging(prefix: &Path, pending: &PendingUpdate) -> Result<PathBuf, UpdateError> {
    let staging = prefix.join(&pending.staging_dir);
    validate_windows_staging(&staging)?;
    let actual = pending_files(&staging)?;
    if actual != pending.files {
        return Err(UpdateError::Transaction(
            "staged update files changed after verification".into(),
        ));
    }
    Ok(staging)
}

#[cfg(windows)]
fn spawn_pending_helper(prefix: &Path) -> Result<(), UpdateError> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let pending = load_pending(prefix)?;
    std::process::Command::new(prefix.join(pending.helper))
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
            record_pending_failure_before_running_lock(&update_lock, prefix, &timeout_error);
            return Err(timeout_error);
        }
        Err(error) => return Err(error.into()),
    };
    // A second helper may have waited behind the one that completed the update.
    if !prefix.join(PENDING_FILE).is_file() {
        return Ok(());
    }
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

        recover_transaction(prefix)?;
        let staging = verify_pending_staging(prefix, &pending)?;
        let version = semver::Version::parse(&pending.target_version)
            .map_err(|error| UpdateError::Transaction(error.to_string()))?;
        let update = AvailableUpdate {
            version: version.clone(),
            tag: format!("v{version}"),
            release_url: String::new(),
            download_url: None,
            asset: None,
        };
        let install = ManagedInstall {
            prefix: prefix.to_path_buf(),
            executable: prefix.join("kettle.exe"),
            marker_path: prefix.join(".kettle-install.json"),
        };
        let mut transaction = Transaction::begin(prefix, &pending.target_version)?;
        if let Err(error) = apply_staged_update(&mut transaction, &staging, &install, &update) {
            if let Err(rollback) = transaction.rollback() {
                return Err(UpdateError::Transaction(format!(
                    "{error}; rollback also failed: {rollback}"
                )));
            }
            return Err(error);
        }
        transaction.commit()?;

        remove_journal_first(prefix, &prefix.join(PENDING_FILE))?;
        refresh_platform_integration(&install);
        let _ = remove_staging_dir_checked(prefix, &staging);
        Ok(())
    })();
    if let Err(error) = &result {
        record_pending_failure(&running_lock, prefix, error);
    }
    result
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
fn record_pending_failure_before_running_lock(
    _update_lock: &kettle_state::ExclusiveFileLock,
    prefix: &Path,
    error: &UpdateError,
) {
    record_pending_failure_locked(prefix, error);
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
        if name.starts_with(".kettle-update-helper-") && name.ends_with(".exe") {
            let _ = fs::remove_file(entry.path());
        } else if name.starts_with(".kettle-update-stage-") {
            let _ = remove_staging_dir_checked(prefix, &entry.path());
        }
    }
    Ok(true)
}

#[cfg(windows)]
fn remove_staging_dir_checked(prefix: &Path, staging: &Path) -> Result<(), UpdateError> {
    if staging.parent() != Some(prefix)
        || !staging
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".kettle-update-stage-"))
    {
        return Err(UpdateError::Transaction(format!(
            "refusing to remove untrusted staging path {}",
            staging.display()
        )));
    }
    match fs::remove_dir_all(staging) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    let mut file = File::open(path)?;
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

/// Hashes the already-open `file` handle from its start, rather than
/// re-opening its path. Re-opening by path between this check and extraction
/// would let another same-user process substitute the bytes in between (the
/// downloaded archive's `NamedTempFile` is not opened with exclusive
/// sharing), so every caller must pass the exact handle it later extracts
/// from instead of resolving the path again.
#[cfg(any(windows, target_os = "linux"))]
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

#[cfg(any(windows, target_os = "linux"))]
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

#[cfg(all(any(windows, target_os = "linux"), unix))]
fn package_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(all(any(windows, target_os = "linux"), not(unix)))]
fn package_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(any(windows, test))]
fn zip_unix_mode_is_safe(mode: Option<u32>, is_dir: bool) -> bool {
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

/// Extracts from the already-open `archive` handle (the same one
/// [`verify_sha256`] hashed) instead of re-opening its path, so nothing can
/// substitute the archive's bytes between verification and extraction. See
/// [`verify_sha256`] for the TOCTOU this closes.
#[cfg(windows)]
fn extract_archive(archive: &mut File, destination: &Path) -> Result<(), UpdateError> {
    archive.rewind()?;
    let mut zip = zip::ZipArchive::new(&mut *archive)?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::UnsafeArchive("too many entries".into()));
    }
    let mut total = 0_u64;
    let mut seen = ArchivePaths::default();
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
        let output = destination.join(&enclosed);
        if is_dir {
            if declared_size != 0 {
                return Err(UpdateError::UnsafeArchive(format!(
                    "directory has data: {}",
                    enclosed.display()
                )));
            }
            fs::create_dir_all(&output)?;
            continue;
        }
        if !entry.is_file() {
            return Err(UpdateError::UnsafeArchive(format!(
                "non-file entry {}",
                enclosed.display()
            )));
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
                enclosed.display()
            )));
        }
        total = next_total;
        file.sync_all()?;
    }
    Ok(())
}

/// Extracts from the already-open `archive` handle (the same one
/// [`verify_sha256`] hashed) instead of re-opening its path, so nothing can
/// substitute the archive's bytes between verification and extraction. See
/// [`verify_sha256`] for the TOCTOU this closes.
#[cfg(target_os = "linux")]
fn extract_archive(archive: &mut File, destination: &Path) -> Result<(), UpdateError> {
    archive.rewind()?;
    let decoder = flate2::read::GzDecoder::new(&mut *archive);
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

#[cfg(any(windows, target_os = "linux"))]
fn validate_archive_path(path: &Path) -> Result<(), UpdateError> {
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

#[cfg(any(windows, target_os = "linux"))]
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

#[cfg(any(windows, target_os = "linux"))]
#[derive(Default)]
struct ArchivePaths {
    /// Case-folded portable path -> whether the entry is a directory.
    entries: std::collections::HashMap<String, bool>,
}

#[cfg(any(windows, target_os = "linux"))]
impl ArchivePaths {
    fn insert(&mut self, path: &Path, is_dir: bool) -> Result<(), UpdateError> {
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
    for source in files.iter().filter(|path| *path != &binary) {
        let relative = source.strip_prefix(staging).map_err(|_| {
            UpdateError::UnsafeArchive(format!("escaped staging: {}", source.display()))
        })?;
        let root = relative
            .components()
            .next()
            .and_then(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .ok_or_else(|| UpdateError::UnsafeArchive(relative.display().to_string()))?;
        if !WINDOWS_ALLOWED_ROOTS.contains(&root) {
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
    debug_assert_eq!(install.executable, install.prefix.join("kettle.exe"));
    Ok(())
}

#[cfg(target_os = "linux")]
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

    let map = [
        ("install.sh", "share/kettle/install.sh", 0o755),
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
        } else {
            transaction.install(Path::new(destination), &source, Some(mode))?;
        }
    }
    let shell_root = root.join("shell-integration");
    for source in collect_files(&shell_root)? {
        let relative = source
            .strip_prefix(&shell_root)
            .map_err(|_| UpdateError::UnsafeArchive(source.display().to_string()))?;
        transaction.install(
            &Path::new("share/kettle/shell-integration").join(relative),
            &source,
            Some(0o644),
        )?;
    }
    transaction.install(Path::new("bin/kettle"), &binary, Some(0o755))?;
    let marker = marker_json(&update.version.to_string())?;
    transaction.install_bytes(
        Path::new("share/kettle/install.json"),
        marker.as_bytes(),
        Some(0o644),
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn render_linux_desktop(source: &Path, prefix: &Path) -> Result<String, UpdateError> {
    let text = fs::read_to_string(source)?;
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

#[cfg(any(windows, target_os = "linux"))]
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
    Prepared,
    Installed,
    Restored,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    transaction_id: String,
    target_version: String,
    phase: JournalPhase,
    backup_dir: String,
    entries: Vec<JournalEntry>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyJournal {
    schema: u32,
    backup_dir: String,
    entries: Vec<LegacyJournalEntry>,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Serialize, Deserialize)]
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
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(UpdateError::Transaction("unsafe install path".into()));
            };
            let candidate = directory_descriptor_path(&directory).join(name);
            let next = match open_anchored_directory(&candidate) {
                Ok(next) => next,
                Err(UpdateError::Io(error))
                    if create_missing && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    match fs::create_dir(&candidate) {
                        Ok(()) => {
                            // Persist each new directory entry before a journal
                            // can refer to content below it.
                            directory.sync_all()?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
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
    validate_relative(relative)?;
    let mut path = prefix.to_path_buf();
    let mut directories = vec![open_anchored_directory(prefix).map_err(|error| {
        UpdateError::Transaction(format!(
            "cannot anchor install prefix {}: {error}",
            prefix.display()
        ))
    })?];
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(UpdateError::Transaction("unsafe install path".into()));
            };
            path.push(name);
            let next = match open_anchored_directory(&path) {
                Ok(next) => next,
                Err(UpdateError::Io(error))
                    if create_missing && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    match fs::create_dir(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
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
    let mut file = open_regular_nofollow(path)?;
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
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        Some(metadata.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok((bytes, mode))
}

#[cfg(any(windows, target_os = "linux"))]
struct Transaction {
    prefix: PathBuf,
    journal_path: PathBuf,
    backup_dir: PathBuf,
    journal: Journal,
}

#[cfg(any(windows, target_os = "linux"))]
impl Transaction {
    fn begin(prefix: &Path, target_version: &str) -> Result<Self, UpdateError> {
        semver::Version::parse(target_version).map_err(|error| {
            UpdateError::Transaction(format!("invalid transaction target version: {error}"))
        })?;
        let suffix = unique_suffix();
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
                entries: Vec::new(),
            },
        };
        if let Err(error) = transaction.persist_journal() {
            let _ = remove_dir_all_checked(prefix, &transaction.backup_dir);
            return Err(error);
        }
        Ok(transaction)
    }

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
        let (_destination_parent, destination) =
            anchored_destination(&self.prefix, relative, true)?;
        let metadata = match fs::symlink_metadata(&destination) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let existed = metadata.is_some();
        if metadata
            .as_ref()
            .is_some_and(|metadata| !metadata.file_type().is_file())
        {
            return Err(UpdateError::Transaction(format!(
                "refusing to replace non-regular file {}",
                destination.display()
            )));
        }
        let (previous, previous_unix_mode) = if existed {
            let (bytes, mode) = read_transaction_file(&destination)?;
            (Some(bytes), mode)
        } else {
            (None, None)
        };
        if let Some(previous) = previous.as_deref() {
            let backup_relative = Path::new(&self.journal.backup_dir).join(relative);
            let (_backup_parent, backup) =
                anchored_destination(&self.prefix, &backup_relative, true)?;
            atomic_write(&backup, previous, Some(0o600))?;
        }
        if self.journal.phase == JournalPhase::Prepared {
            self.journal.phase = JournalPhase::Applying;
        }
        self.journal.entries.push(JournalEntry {
            relative: relative_string,
            existed,
            previous_unix_mode,
            previous_size: previous.as_ref().map(|bytes| bytes.len() as u64),
            previous_sha256: previous.as_deref().map(sha256_bytes),
            replacement_size: bytes.len() as u64,
            replacement_sha256: sha256_bytes(bytes),
            state: JournalEntryState::Prepared,
        });
        self.persist_journal()?;
        atomic_write(&destination, bytes, unix_mode)?;
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

    fn rollback(&mut self) -> Result<(), UpdateError> {
        self.journal.phase = JournalPhase::RollingBack;
        self.persist_journal()?;
        self.restore_entries()?;
        self.finish_cleanup()
    }

    fn commit(mut self) -> Result<(), UpdateError> {
        self.journal.phase = JournalPhase::Committed;
        self.persist_journal()?;
        self.finish_cleanup()
    }

    fn restore_entries(&mut self) -> Result<(), UpdateError> {
        for index in (0..self.journal.entries.len()).rev() {
            if self.journal.entries[index].state == JournalEntryState::Restored {
                continue;
            }
            restore_entry(&self.prefix, &self.backup_dir, &self.journal.entries[index])?;
            self.journal.entries[index].state = JournalEntryState::Restored;
            self.persist_journal()?;
        }
        Ok(())
    }

    fn finish_cleanup(&mut self) -> Result<(), UpdateError> {
        remove_journal_first(&self.prefix, &self.journal_path)?;
        // Once the journal is gone, the transaction is durably committed (or
        // rolled back). A crash during backup cleanup can leave harmless stale
        // data, but can no longer leave a journal that points at missing data.
        remove_dir_all_checked(&self.prefix, &self.backup_dir)
    }
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
        rollback_legacy_journal(prefix, &journal)?;
        remove_journal_first(prefix, &journal_path)?;
        return remove_dir_all_checked(prefix, &prefix.join(&journal.backup_dir));
    }
    if schema != u64::from(JOURNAL_SCHEMA) {
        return Err(UpdateError::Transaction(format!(
            "unsupported update journal schema {schema}"
        )));
    }
    let journal: Journal = serde_json::from_slice(&bytes)?;
    validate_journal(&journal)?;
    let backup_dir = prefix.join(&journal.backup_dir);
    let mut transaction = Transaction {
        prefix: prefix.to_path_buf(),
        journal_path,
        backup_dir,
        journal,
    };
    if transaction.journal.phase == JournalPhase::Committed {
        transaction.finish_cleanup()
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
        || journal.transaction_id.is_empty()
        || !journal
            .transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
        || journal.backup_dir != format!(".kettle-update-backup-{}", journal.transaction_id)
        || semver::Version::parse(&journal.target_version).is_err()
        || journal.entries.len() > MAX_ARCHIVE_ENTRIES
    {
        return Err(UpdateError::Transaction(
            "update journal failed validation".to_string(),
        ));
    }
    let mut destinations = std::collections::HashSet::new();
    let mut total = 0_u64;
    for entry in &journal.entries {
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
        {
            return Err(UpdateError::Transaction(format!(
                "invalid update journal entry {}",
                entry.relative
            )));
        }
        total = total
            .checked_add(entry.replacement_size)
            .ok_or_else(|| UpdateError::Transaction("journal size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::Transaction(
                "journal replacement data exceeds the safety limit".into(),
            ));
        }
    }
    Ok(())
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
fn remove_journal_first(prefix: &Path, journal_path: &Path) -> Result<(), UpdateError> {
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
fn remove_dir_all_checked(prefix: &Path, path: &Path) -> Result<(), UpdateError> {
    if path.parent() != Some(prefix)
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".kettle-update-backup-"))
    {
        return Err(UpdateError::Transaction(format!(
            "refusing to remove untrusted path {}",
            path.display()
        )));
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn unique_suffix() -> String {
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
fn refresh_platform_integration(install: &ManagedInstall) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = install.prefix.join("install.ps1");
    let Some(powershell) = system_powershell_path() else {
        log::warn!(
            "could not resolve a fully-qualified PowerShell path; skipping the post-update integration refresh"
        );
        return;
    };
    let _ = std::process::Command::new(powershell)
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .arg("-RefreshIntegration")
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// Resolves `powershell.exe` by a fixed, fully-qualified system path instead
/// of letting `Command::new` search for a bare name. `CreateProcess`'s
/// default search order tries the spawning process's own application
/// directory and its current working directory before PATH, so a same-user
/// attacker able to write into either (a much weaker position than
/// compromising this process's environment) could otherwise have this
/// authenticated self-update step execute an arbitrary planted binary.
/// `%SystemRoot%`/`%windir%` are set by Windows for every process and are
/// the standard way to name the system directory without new API bindings;
/// resolving through them and confirming the target file actually exists is
/// still strictly safer than an unqualified `Command::new("powershell.exe")`.
#[cfg(windows)]
fn system_powershell_path() -> Option<PathBuf> {
    for variable in ["SystemRoot", "windir"] {
        if let Some(root) = std::env::var_os(variable) {
            let root = PathBuf::from(root);
            if root.is_absolute() {
                let candidate = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let fallback = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    fallback.is_file().then_some(fallback)
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
        }
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
        let root = tempfile::tempdir().unwrap();
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
        let detected = detect_managed_install_at(&executable).unwrap();
        assert_eq!(detected.prefix, prefix.canonicalize().unwrap());

        for channel in ["local-dev", "local-dev-record"] {
            marker.channel = channel.into();
            fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
            let error = detect_managed_install_at(&executable).unwrap_err();
            assert!(error.to_string().contains("local development install"));
            assert!(error.to_string().contains("rebuild and reinstall"));
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    #[test]
    fn unsupported_platform_has_no_managed_installer() {
        assert!(matches!(
            detect_managed_install(),
            Err(UpdateError::UnsupportedPlatform)
        ));
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
    fn package_manifest_requires_exact_hash_size_mode_and_file_set() {
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
    fn interrupted_transaction_recovers_from_journal() {
        let root = tempfile::tempdir().unwrap();
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
    fn committed_transaction_recovery_keeps_new_files_and_only_cleans_state() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("value"), b"before").unwrap();
        {
            let mut tx = Transaction::begin(root.path(), "99.0.0").unwrap();
            tx.install_bytes(Path::new("value"), b"after", None)
                .unwrap();
            tx.journal.phase = JournalPhase::Committed;
            tx.persist_journal().unwrap();
            std::mem::forget(tx);
        }
        recover_transaction(root.path()).unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
    fn transaction_rejects_duplicate_destinations() {
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
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

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("share/kettle")).unwrap();
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
        );
        assert!(!outside.path().join("kettle/value").exists());
        assert!(root.path().join(".kettle-update-journal.json").is_file());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn legacy_journal_recovery_removes_journal_before_backup_cleanup() {
        let root = tempfile::tempdir().unwrap();
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

    #[cfg(windows)]
    #[test]
    fn staged_windows_release_replaces_binary_and_support_files_atomically() {
        let root = tempfile::tempdir().unwrap();
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
    #[test]
    fn staged_linux_release_populates_installer_layout() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("install with \\ % $ quote\" and ` value");
        let stage = root.path().join("stage/kettle");
        fs::create_dir_all(stage.join("packaging/linux")).unwrap();
        fs::create_dir_all(stage.join("shell-integration")).unwrap();
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/kettle"), b"old-binary").unwrap();
        for (relative, body) in [
            ("kettle", "new-binary"),
            ("install.sh", "install"),
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
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_template_rewrite_requires_each_owned_key_exactly_once() {
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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

        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("release.tar.gz");
        let destination = root.path().join("stage");
        fs::create_dir(&destination).unwrap();
        write_archive(&archive, 0o755);
        let mut archive_file = fs::File::open(&archive).unwrap();
        extract_archive(&mut archive_file, &destination).unwrap();
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
        let mut special_archive_file = fs::File::open(&special_archive).unwrap();
        let error = extract_archive(&mut special_archive_file, &special_destination).unwrap_err();
        assert!(error.to_string().contains("special permission bits"));
        assert!(!special_destination.join("kettle/install.sh").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_archive_extraction_rejects_pax_sparse_metadata() {
        let root = tempfile::tempdir().unwrap();
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

        let mut archive_file = fs::File::open(&archive_path).unwrap();
        let error = extract_archive(&mut archive_file, &destination).unwrap_err();

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

    /// Regression test for the archive TOCTOU: `verify_sha256` and
    /// `extract_archive` must operate on the exact handle the caller passes
    /// in rather than re-resolving the caller's path, so a same-user process
    /// that swaps the file at that path between the hash check and
    /// extraction cannot smuggle unverified bytes into the staging
    /// directory.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_extract_archive_reads_the_verified_handle_not_a_reopened_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("release.tar.gz");
        write_test_tar_gz(&path, b"original-bytes");
        let expected_hash = sha256_file(&path).unwrap();

        let mut file = fs::File::open(&path).unwrap();
        verify_sha256(&mut file, &expected_hash).unwrap();

        // Simulate an attacker replacing the archive at the same path after
        // the hash check succeeds but before extraction runs. On Linux this
        // delete-and-recreate is possible even while our handle stays open;
        // that open handle keeps referencing the original, already-verified
        // inode regardless.
        let malicious = root.path().join("malicious.tar.gz");
        write_test_tar_gz(&malicious, b"attacker-bytes");
        fs::rename(&malicious, &path).unwrap();
        assert_ne!(sha256_file(&path).unwrap(), expected_hash);

        let destination = root.path().join("stage");
        fs::create_dir(&destination).unwrap();
        extract_archive(&mut file, &destination).unwrap();

        assert_eq!(
            fs::read(destination.join("kettle/payload")).unwrap(),
            b"original-bytes",
            "extraction must read the handle verify_sha256 hashed, not whatever now lives at the archive's path"
        );
    }

    /// Regression test for the Windows half of the same archive TOCTOU: the
    /// exclusive lock `install_update_into` takes on the downloaded archive
    /// must be a mandatory, kernel-enforced lock that blocks a concurrent
    /// same-user writer from overwriting the file's bytes in place, not
    /// merely an advisory courtesy that a hostile writer can ignore.
    #[cfg(windows)]
    #[test]
    fn windows_archive_lock_blocks_a_concurrent_in_place_overwrite() {
        let root = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.json");
        fs::write(&path, b"old").unwrap();

        write_atomic_file(&path, b"new").unwrap();

        assert_eq!(fs::read(path).unwrap(), b"new");
    }

    #[cfg(windows)]
    #[test]
    fn windows_run_lock_and_target_handle_gate_replacement() {
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join(".kettle-update-stage-in-progress");
        let helper = root.path().join(".kettle-update-helper-in-progress.exe");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("payload"), b"still preparing").unwrap();
        fs::write(&helper, b"helper").unwrap();

        let update_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.path().join(".kettle-update.lock"))
            .unwrap();
        fs4::FileExt::lock(&update_lock).unwrap();
        assert!(!cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert!(stage.is_dir());
        assert!(helper.is_file());

        drop(update_lock);
        assert!(cleanup_stale_windows_update_files_if_idle(root.path()).unwrap());
        assert!(!stage.exists());
        assert!(!helper.exists());
    }

    #[cfg(windows)]
    fn seed_windows_pending(prefix: &Path, attempts: u32) -> PendingUpdate {
        fs::create_dir_all(prefix).unwrap();
        let transaction_id = "123-456";
        let staging_dir = ".kettle-update-stage-test";
        let helper = format!(".kettle-update-helper-{transaction_id}.exe");
        let stage = prefix.join(staging_dir);
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("kettle.exe"), b"new-gui").unwrap();
        fs::write(stage.join("kettle.com"), b"new-console").unwrap();
        fs::write(stage.join("install.ps1"), b"install").unwrap();
        fs::copy(std::env::current_exe().unwrap(), prefix.join(&helper)).unwrap();
        let pending = PendingUpdate {
            schema: PENDING_SCHEMA,
            product: "kettle".into(),
            target: current_target().unwrap().into(),
            transaction_id: transaction_id.into(),
            target_version: "99.0.0".into(),
            staging_dir: staging_dir.into(),
            helper,
            files: pending_files(&stage).unwrap(),
            attempts,
            last_error: Some("fixture failure".into()),
        };
        persist_pending(prefix, &pending).unwrap();
        pending
    }

    #[cfg(windows)]
    #[test]
    fn windows_invalid_and_exhausted_pending_updates_are_quarantined() {
        let invalid = tempfile::tempdir().unwrap();
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

        let exhausted = tempfile::tempdir().unwrap();
        seed_windows_pending(exhausted.path(), MAX_PENDING_ATTEMPTS);
        let PendingStartInspection::Failed {
            fingerprint,
            reason,
        } = inspect_pending_start(exhausted.path()).unwrap()
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
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
