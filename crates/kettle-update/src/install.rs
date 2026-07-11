use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::current_target;
use crate::feed::{AvailableUpdate, FeedClient, UpdateError};

const MARKER_SCHEMA: u32 = 1;
const JOURNAL_SCHEMA: u32 = 1;
const MAX_ARCHIVE_ENTRIES: usize = 512;
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;

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
    pub restart_required: bool,
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

pub fn detect_managed_install() -> Result<ManagedInstall, UpdateError> {
    let executable = std::env::current_exe()?;
    detect_managed_install_at(&executable)
}

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

    #[cfg(not(any(windows, target_os = "linux")))]
    let (prefix, marker_path): (PathBuf, PathBuf) = {
        return Err(UpdateError::UnsupportedPlatform);
    };

    let bytes = fs::read(&marker_path).map_err(|e| {
        UpdateError::UnmanagedInstall(format!(
            "{} is missing or unreadable ({e}); update through the package manager or installer that owns this executable",
            marker_path.display()
        ))
    })?;
    if bytes.len() > 16 * 1024 {
        return Err(UpdateError::UnmanagedInstall(
            "the installer marker is unexpectedly large".to_string(),
        ));
    }
    let marker: InstallMarker = serde_json::from_slice(&bytes)
        .map_err(|e| UpdateError::UnmanagedInstall(format!("invalid installer marker: {e}")))?;
    if marker.schema != MARKER_SCHEMA
        || marker.product != "kettle"
        || marker.managed_by != "kettle-installer"
        || marker.channel != "stable"
        || marker.target != target
    {
        return Err(UpdateError::UnmanagedInstall(
            "the installer marker does not match this kettle build".to_string(),
        ));
    }
    Ok(ManagedInstall {
        prefix,
        executable,
        marker_path,
    })
}

pub fn install_update(
    client: &FeedClient,
    update: &AvailableUpdate,
) -> Result<InstallOutcome, UpdateError> {
    let install = detect_managed_install()?;
    install_update_into(client, update, &install)
}

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
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    match fs4::FileExt::try_lock(&lock) {
        Ok(()) => {}
        Err(fs4::TryLockError::WouldBlock) => return Err(UpdateError::UpdateLocked),
        Err(fs4::TryLockError::Error(error)) => return Err(error.into()),
    }
    recover_transaction(&install.prefix)?;

    let mut archive = tempfile::Builder::new()
        .prefix("kettle-update-download-")
        .tempfile()?;
    client.download_to(update, archive.as_file_mut())?;
    archive.as_file_mut().flush()?;
    archive.as_file().sync_all()?;
    verify_sha256(archive.path(), &asset.sha256)?;

    let staging = tempfile::Builder::new()
        .prefix(".kettle-update-stage-")
        .tempdir_in(&install.prefix)?;
    extract_archive(archive.path(), staging.path())?;

    let mut transaction = Transaction::begin(&install.prefix)?;
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
        restart_required: true,
    })
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), UpdateError> {
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
    if hex::encode(hash.finalize()) != expected {
        return Err(UpdateError::HashMismatch);
    }
    Ok(())
}

#[cfg(windows)]
fn extract_archive(archive: &Path, destination: &Path) -> Result<(), UpdateError> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::UnsafeArchive("too many entries".into()));
    }
    let mut total = 0_u64;
    let mut seen = std::collections::HashSet::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| UpdateError::UnsafeArchive(entry.name().to_string()))?
            .to_path_buf();
        validate_archive_path(&enclosed)?;
        let folded = enclosed.to_string_lossy().to_ascii_lowercase();
        if !seen.insert(folded) {
            return Err(UpdateError::UnsafeArchive(format!(
                "duplicate path {}",
                enclosed.display()
            )));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(UpdateError::UnsafeArchive(format!(
                "symbolic link {}",
                enclosed.display()
            )));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| UpdateError::UnsafeArchive("unpacked size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "unpacked data exceeds the safety limit".into(),
            ));
        }
        let output = destination.join(&enclosed);
        if entry.is_dir() {
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
        std::io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn extract_archive(archive: &Path, destination: &Path) -> Result<(), UpdateError> {
    let file = File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let mut count = 0_usize;
    let mut total = 0_u64;
    let mut seen = std::collections::HashSet::new();
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
        if !seen.insert(path.clone()) {
            return Err(UpdateError::UnsafeArchive(format!(
                "duplicate path {}",
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
        let size = entry.header().size()?;
        total = total
            .checked_add(size)
            .ok_or_else(|| UpdateError::UnsafeArchive("unpacked size overflow".into()))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(UpdateError::UnsafeArchive(
                "unpacked data exceeds the safety limit".into(),
            ));
        }
        let output = destination.join(&path);
        if entry_type.is_dir() {
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
        std::io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn extract_archive(_archive: &Path, _destination: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

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
        {
            return Err(UpdateError::UnsafeArchive(path.display().to_string()));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn apply_staged_update(
    transaction: &mut Transaction,
    staging: &Path,
    install: &ManagedInstall,
    update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    const ALLOWED_ROOTS: &[&str] = &[
        "kettle.exe",
        "kettle.com",
        "install.ps1",
        "kettle.ico",
        "LICENSE",
        "NOTICE",
        "README.md",
        "CHANGELOG.md",
        "shell-integration",
    ];
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
        if !ALLOWED_ROOTS.contains(&root) {
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

#[cfg(not(any(windows, target_os = "linux")))]
fn apply_staged_update(
    _transaction: &mut Transaction,
    _staging: &Path,
    _install: &ManagedInstall,
    _update: &AvailableUpdate,
) -> Result<(), UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn render_linux_desktop(source: &Path, prefix: &Path) -> Result<String, UpdateError> {
    let text = fs::read_to_string(source)?;
    let executable = prefix.join("bin/kettle");
    let icon = prefix.join("share/icons/hicolor/scalable/apps/kettle.svg");
    let executable = executable.to_string_lossy();
    let icon = icon.to_string_lossy();
    let mut rendered = String::with_capacity(text.len() + executable.len() * 2 + icon.len());
    for line in text.lines() {
        match line {
            "Exec=kettle" => rendered.push_str(&format!("Exec={executable}")),
            "TryExec=kettle" => rendered.push_str(&format!("TryExec={executable}")),
            "Icon=kettle" => rendered.push_str(&format!("Icon={icon}")),
            _ => rendered.push_str(line),
        }
        rendered.push('\n');
    }
    Ok(rendered)
}

fn require_file(path: &Path, label: impl std::fmt::Display) -> Result<(), UpdateError> {
    if !path.is_file() {
        return Err(UpdateError::MissingArchiveFile(label.to_string()));
    }
    Ok(())
}

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

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    schema: u32,
    backup_dir: String,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEntry {
    relative: String,
    existed: bool,
    #[serde(default)]
    previous_unix_mode: Option<u32>,
}

struct Transaction {
    prefix: PathBuf,
    journal_path: PathBuf,
    backup_dir: PathBuf,
    journal: Journal,
}

impl Transaction {
    fn begin(prefix: &Path) -> Result<Self, UpdateError> {
        let suffix = unique_suffix();
        let backup_name = format!(".kettle-update-backup-{suffix}");
        let backup_dir = prefix.join(&backup_name);
        fs::create_dir(&backup_dir)?;
        let mut transaction = Self {
            prefix: prefix.to_path_buf(),
            journal_path: prefix.join(".kettle-update-journal.json"),
            backup_dir,
            journal: Journal {
                schema: JOURNAL_SCHEMA,
                backup_dir: backup_name,
                entries: Vec::new(),
            },
        };
        transaction.persist_journal()?;
        Ok(transaction)
    }

    fn install(
        &mut self,
        relative: &Path,
        source: &Path,
        unix_mode: Option<u32>,
    ) -> Result<(), UpdateError> {
        validate_relative(relative)?;
        let bytes = fs::read(source)?;
        self.install_bytes(relative, &bytes, unix_mode)
    }

    fn install_bytes(
        &mut self,
        relative: &Path,
        bytes: &[u8],
        unix_mode: Option<u32>,
    ) -> Result<(), UpdateError> {
        validate_relative(relative)?;
        let destination = self.prefix.join(relative);
        let existed = destination.is_file();
        let previous_unix_mode = if existed {
            unix_mode_of(&destination)?
        } else {
            None
        };
        if destination.exists() && !existed {
            return Err(UpdateError::Transaction(format!(
                "refusing to replace non-file {}",
                destination.display()
            )));
        }
        if existed {
            let backup = self.backup_dir.join(relative);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&destination, &backup)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&backup)?
                .sync_all()?;
            if let Some(parent) = backup.parent() {
                sync_parent(parent)?;
            }
        }
        self.journal.entries.push(JournalEntry {
            relative: relative_to_string(relative)?,
            existed,
            previous_unix_mode,
        });
        self.persist_journal()?;
        atomic_write(&destination, bytes, unix_mode)?;
        Ok(())
    }

    fn persist_journal(&mut self) -> Result<(), UpdateError> {
        let bytes = serde_json::to_vec_pretty(&self.journal)?;
        atomic_write(&self.journal_path, &bytes, None)
    }

    fn rollback(&mut self) -> Result<(), UpdateError> {
        rollback_journal(&self.prefix, &self.journal)?;
        self.finish_cleanup()
    }

    fn commit(mut self) -> Result<(), UpdateError> {
        self.finish_cleanup()
    }

    fn finish_cleanup(&mut self) -> Result<(), UpdateError> {
        match fs::remove_file(&self.journal_path) {
            Ok(()) => sync_parent(&self.prefix)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Once the journal is gone, the transaction is durably committed (or
        // rolled back). A crash during backup cleanup can leave harmless stale
        // data, but can no longer leave a journal that points at missing data.
        remove_dir_all_checked(&self.prefix, &self.backup_dir)
    }
}

fn recover_transaction(prefix: &Path) -> Result<(), UpdateError> {
    let journal_path = prefix.join(".kettle-update-journal.json");
    let bytes = match fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if bytes.len() > 1024 * 1024 {
        return Err(UpdateError::Transaction(
            "update journal exceeds the safety limit".to_string(),
        ));
    }
    let journal: Journal = serde_json::from_slice(&bytes)?;
    if journal.schema != JOURNAL_SCHEMA
        || !journal.backup_dir.starts_with(".kettle-update-backup-")
        || journal.backup_dir.contains(['/', '\\'])
    {
        return Err(UpdateError::Transaction(
            "update journal failed validation".to_string(),
        ));
    }
    rollback_journal(prefix, &journal)?;
    remove_dir_all_checked(prefix, &prefix.join(&journal.backup_dir))?;
    fs::remove_file(journal_path)?;
    Ok(())
}

fn rollback_journal(prefix: &Path, journal: &Journal) -> Result<(), UpdateError> {
    let backup_dir = prefix.join(&journal.backup_dir);
    for entry in journal.entries.iter().rev() {
        let relative = Path::new(&entry.relative);
        validate_relative(relative)?;
        let destination = prefix.join(relative);
        if entry.existed {
            let backup = backup_dir.join(relative);
            let bytes = fs::read(&backup).map_err(|e| {
                UpdateError::Transaction(format!("cannot restore backup {}: {e}", backup.display()))
            })?;
            atomic_write(&destination, &bytes, entry.previous_unix_mode)?;
        } else if destination.exists() {
            fs::remove_file(destination)?;
        }
    }
    Ok(())
}

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
    let parent = destination
        .parent()
        .ok_or_else(|| UpdateError::Transaction("destination has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".kettle-update-file-")
        .tempfile_in(parent)?;
    staged.write_all(bytes)?;
    staged.flush()?;
    set_mode(staged.path(), unix_mode)?;
    staged.as_file().sync_all()?;
    let (_, staged_path) = staged.keep().map_err(|e| e.error)?;
    if let Err(error) = replace_file(&staged_path, destination) {
        let _ = fs::remove_file(&staged_path);
        return Err(error);
    }
    sync_parent(parent)?;
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

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(unix)]
fn unix_mode_of(path: &Path) -> Result<Option<u32>, UpdateError> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(Some(fs::metadata(path)?.permissions().mode() & 0o7777))
}

#[cfg(not(unix))]
fn unix_mode_of(_path: &Path) -> Result<Option<u32>, UpdateError> {
    Ok(None)
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: Option<u32>) -> Result<(), UpdateError> {
    Ok(())
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    fs::rename(source, destination)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), UpdateError> {
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
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), UpdateError> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), UpdateError> {
    Ok(())
}

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
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .arg("-RefreshIntegration")
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(target_os = "linux")]
fn refresh_platform_integration(install: &ManagedInstall) {
    let _ = std::process::Command::new("update-desktop-database")
        .arg(install.prefix.join("share/applications"))
        .status();
    let icon_root = install.prefix.join("share/icons/hicolor");
    if icon_root.join("index.theme").is_file() {
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-f", "-t"])
            .arg(icon_root)
            .status();
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn refresh_platform_integration(_install: &ManagedInstall) {}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn archive_paths_reject_traversal_and_platform_tricks() {
        for bad in [
            "../kettle",
            "/kettle",
            "kettle/..",
            "kettle/file:stream",
            "kettle/trailing.",
        ] {
            assert!(
                validate_archive_path(Path::new(bad)).is_err(),
                "accepted {bad}"
            );
        }
        assert!(validate_archive_path(Path::new("kettle/shell-integration/kettle.ps1")).is_ok());
    }

    #[test]
    fn transaction_rolls_back_replaced_and_created_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("existing"), b"old").unwrap();
        let mut tx = Transaction::begin(root.path()).unwrap();
        tx.install_bytes(Path::new("existing"), b"new", None)
            .unwrap();
        tx.install_bytes(Path::new("created"), b"created", None)
            .unwrap();
        tx.rollback().unwrap();
        assert_eq!(fs::read(root.path().join("existing")).unwrap(), b"old");
        assert!(!root.path().join("created").exists());
        assert!(!root.path().join(".kettle-update-journal.json").exists());
    }

    #[test]
    fn interrupted_transaction_recovers_from_journal() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("value"), b"before").unwrap();
        {
            let mut tx = Transaction::begin(root.path()).unwrap();
            tx.install_bytes(Path::new("value"), b"after", None)
                .unwrap();
            std::mem::forget(tx);
        }
        recover_transaction(root.path()).unwrap();
        assert_eq!(fs::read(root.path().join("value")).unwrap(), b"before");
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
        let mut transaction = Transaction::begin(&prefix).unwrap();
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
        let prefix = root.path().join("install");
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
                "[Desktop Entry]\nExec=kettle\nTryExec=kettle\nIcon=kettle\n",
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
        let mut transaction = Transaction::begin(&prefix).unwrap();
        apply_staged_update(
            &mut transaction,
            root.path().join("stage").as_path(),
            &install,
            &fake_update(),
        )
        .unwrap();
        transaction.commit().unwrap();
        assert_eq!(fs::read(prefix.join("bin/kettle")).unwrap(), b"new-binary");
        let desktop = fs::read_to_string(prefix.join("share/applications/kettle.desktop")).unwrap();
        assert!(desktop.contains(&format!("Exec={}", prefix.join("bin/kettle").display())));
        assert!(prefix.join("share/kettle/install.json").is_file());
    }

    #[test]
    fn atomic_state_write_replaces_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.json");
        fs::write(&path, b"old").unwrap();

        write_atomic_file(&path, b"new").unwrap();

        assert_eq!(fs::read(path).unwrap(), b"new");
    }
}
