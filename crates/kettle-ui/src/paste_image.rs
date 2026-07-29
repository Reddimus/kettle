//! Clipboard bitmap → temp PNG, so a screenshot can be pasted into a pane.
//!
//! Copying a *file* in the OS file manager populates `CF_HDROP` /
//! `text/uri-list`, which `paste_clipboard` already turns into a shell-quoted
//! path. Capturing a *screenshot* (Win+Shift+S, Snipping Tool, macOS
//! Cmd+Shift+4, GNOME Screenshot) does something different: it puts a raw
//! bitmap on the clipboard with no file and no text behind it. There was
//! nothing for the file-paste branch to path-ify, so the paste did nothing at
//! all.
//!
//! This module closes that gap by materializing the bitmap as a PNG and handing
//! back its path, which then flows through the exact same
//! [`crate::mux::format_paths_for_paste`] pipeline as a copied file — shell-aware
//! quoting and WSL `C:\` → `/mnt/c` translation included. Handing a CLI agent a
//! path also sidesteps the terminal-side clipboard-bitmap decoding that agents
//! do inconsistently across platforms.
//!
//! **Privacy.** A pasted screenshot is arbitrary user content — it can contain
//! anything that was on screen. Files use owner-only permissions inside an
//! owner-private per-process scratch directory. Kettle retains each file's
//! original handle, caps the live encoded-PNG aggregate at 256 MiB, and deletes
//! only that exact file identity on exit. Windows uses per-user Local App Data
//! because the process temp directory can grant sandbox principals delete-child
//! access; other platforms use the OS temp directory. A bounded startup sweep
//! reclaims an exact, old session only after its creator PID is definitively
//! dead, so a long-running sibling is never deleted merely because it is old.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Directory-name prefix inside the platform scratch root. Also the sweep key,
/// so it must stay stable across versions or old directories become
/// unreclaimable.
const DIR_PREFIX: &str = "kettle-paste-";

/// Per-session ceilings. A paste is a deliberate user action, so these are
/// generous — they exist to bound a stuck key or a hostile automation loop, not
/// to ration normal use.
const MAX_FILES: usize = 64;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Bound the source buffer as well as the stored aggregate. The clipboard
/// backend already materialized this buffer before calling us, but refusing a
/// larger value prevents a malformed provider from driving an unbounded encode.
const MAX_RGBA_BYTES: u64 = MAX_TOTAL_BYTES;

/// Reject absurd dimensions before allocating. 16384² RGBA is ~1 GiB, already
/// far past any real screenshot; beyond this a malformed clipboard descriptor is
/// likelier than a genuine capture.
const MAX_DIMENSION: usize = 16_384;

/// Directories from an earlier run are removed once they are older than this.
/// Only relevant when a previous process died without running its cleanup.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Startup cleanup is deliberately bounded even if the scratch root is hostile
/// or damaged. A valid session contains at most `MAX_FILES` regular PNGs.
const MAX_SWEEP_ENTRIES: usize = 8_192;
const MAX_SWEEP_ATTEMPTS: usize = 64;
const MAX_SWEEP_SESSIONS: usize = 32;
const MAX_SWEEP_DURATION: Duration = Duration::from_millis(250);

struct LiveImage {
    path: PathBuf,
    removal_path: PathBuf,
    file: File,
}

struct SessionDirectory {
    path: PathBuf,
    file: File,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// Owner of this process's pasted-image scratch directory.
///
/// The directory is created lazily only after bitmap validation and budget
/// preflight, so a session that never attempts an image paste touches the
/// filesystem not at all.
pub(crate) struct PastedImages {
    dir: PathBuf,
    seq: usize,
    directory: Option<SessionDirectory>,
    files: Vec<LiveImage>,
    bytes: u64,
}

impl PastedImages {
    pub(crate) fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Self {
            dir: scratch_root().join(format!("{DIR_PREFIX}{}-{nonce}", std::process::id())),
            seq: 0,
            directory: None,
            files: Vec::new(),
            bytes: 0,
        }
    }

    /// Encode `width`×`height` RGBA8 `rgba` as a PNG and return its path.
    ///
    /// Split from the clipboard call so the encode/bounds policy is testable
    /// without a live clipboard or a display server.
    pub(crate) fn save_rgba(
        &mut self,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> io::Result<PathBuf> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard image has a zero dimension",
            ));
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("clipboard image {width}x{height} exceeds the {MAX_DIMENSION}px limit"),
            ));
        }
        // A short read here would make the encoder index out of bounds, so the
        // length is checked against the declared geometry rather than trusted.
        let expected = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "clipboard image size overflows")
            })?;
        if u64::try_from(expected).unwrap_or(u64::MAX) > MAX_RGBA_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "clipboard RGBA buffer exceeds the {} byte limit",
                    MAX_RGBA_BYTES
                ),
            ));
        }
        if rgba.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "clipboard image is {} bytes, expected {expected} for {width}x{height} RGBA",
                    rgba.len()
                ),
            ));
        }
        let remaining = MAX_TOTAL_BYTES.saturating_sub(self.bytes);
        if self.files.len() >= MAX_FILES || remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!(
                    "pasted-image budget reached ({} files, {} bytes)",
                    self.files.len(),
                    self.bytes
                ),
            ));
        }
        if self.directory.is_none() {
            self.directory = Some(establish_session_directory(&self.dir)?);
        }
        let directory = self
            .directory
            .as_ref()
            .expect("the session directory was established above");
        verify_session_directory_path(directory)?;

        let sequence = self.seq.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "pasted-image sequence overflowed",
            )
        })?;
        let leaf = format!("{sequence:04}.png");
        let path = self.dir.join(&leaf);
        let removal_path = session_child_path(directory, std::ffi::OsStr::new(&leaf));
        let file = create_private_file_in_session(directory, &removal_path)?;
        let mut writer = BudgetWriter::new(io::BufWriter::new(file), remaining);
        // `image` is already a kettle-ui dependency (the window icon decodes
        // through it), and the `png` feature it is built with covers encoding.
        use image::ImageEncoder as _;
        let encoded = image::codecs::png::PngEncoder::new(&mut writer)
            .write_image(
                rgba,
                width as u32,
                height as u32,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| {
                if writer.exceeded() {
                    budget_error(self.files.len(), self.bytes)
                } else {
                    io::Error::other(format!("PNG encode failed: {error}"))
                }
            })
            .and_then(|()| writer.flush());
        let written = writer.written();
        let buffered = writer.into_inner();
        let (file, _pending) = buffered.into_parts();
        if let Err(error) = encoded {
            discard_private_file(file, &removal_path);
            return Err(error);
        }
        let actual = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                discard_private_file(file, &removal_path);
                return Err(error);
            }
        };
        if actual != written || actual > remaining {
            discard_private_file(file, &removal_path);
            return Err(if actual > remaining {
                budget_error(self.files.len(), self.bytes)
            } else {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "encoded PNG length changed while writing (counted {written}, found {actual})"
                    ),
                )
            });
        }
        // Reopen through the held session-directory capability, then compare
        // kernel identities before releasing the creator. Windows also has the
        // creator's no-delete share pin; Unix does not rely on path stability.
        let retained = match kettle_state::open_existing_private_file(&removal_path) {
            Ok(retained) => retained,
            Err(error) => {
                discard_private_file(file, &removal_path);
                return Err(error);
            }
        };
        let same_file = match same_open_file_identity(&file, &retained) {
            Ok(same) => same,
            Err(error) => {
                drop(retained);
                discard_private_file(file, &removal_path);
                return Err(error);
            }
        };
        if !same_file {
            drop(retained);
            discard_private_file(file, &removal_path);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pasted-image path no longer identifies the created PNG",
            ));
        }
        if let Some(directory) = self.directory.as_ref() {
            match session_directory_matches_path(directory) {
                Ok(true) => {}
                Ok(false) => {
                    drop(retained);
                    discard_private_file(file, &removal_path);
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "pasted-image session changed while publishing the PNG path",
                    ));
                }
                Err(error) => {
                    drop(retained);
                    discard_private_file(file, &removal_path);
                    return Err(error);
                }
            }
        }
        drop(file);
        let Some(total) = self.bytes.checked_add(actual) else {
            let _ = kettle_state::remove_open_private_file(retained, &removal_path);
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "pasted-image byte accounting overflowed",
            ));
        };
        self.bytes = total;
        self.seq = sequence;
        self.files.push(LiveImage {
            path: path.clone(),
            removal_path,
            file: retained,
        });
        Ok(path)
    }

    /// Remove only the exact PNG objects this process created, then the verified
    /// empty session directory. Best effort: a failure here must never take
    /// down a shutdown path, so it is logged and swallowed.
    pub(crate) fn cleanup(&mut self) {
        if self.files.is_empty() && self.directory.is_none() {
            return;
        }
        for image in self.files.drain(..) {
            if let Err(error) =
                kettle_state::remove_open_private_file(image.file, &image.removal_path)
                && error.kind() != io::ErrorKind::NotFound
            {
                log::debug!(
                    "pasted-image cleanup left {} unchanged: {error}",
                    image.path.display()
                );
            }
        }
        if let Some(directory) = self.directory.take()
            && let Err(error) = remove_session_directory(directory)
            && error.kind() != io::ErrorKind::NotFound
        {
            log::debug!(
                "pasted-image directory cleanup left {} unchanged: {error}",
                self.dir.display()
            );
        }
        self.bytes = 0;
        self.seq = 0;
    }

    #[cfg(test)]
    pub(crate) fn with_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            seq: 0,
            directory: None,
            files: Vec::new(),
            bytes: 0,
        }
    }
}

impl Drop for PastedImages {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Remove pasted-image directories left by a process that died before its own
/// cleanup ran.
///
/// Age alone is never authority to delete. The directory name must have the
/// exact creator/session grammar, its creator PID must be definitively dead,
/// and every child must open as an owner-private, non-reparse, single-link
/// regular PNG before identity-safe deletion starts.
pub(crate) fn sweep_stale() {
    if let Err(error) = std::thread::Builder::new()
        .name("kettle-paste-sweep".into())
        .spawn(|| {
            sweep_stale_in(
                &scratch_root(),
                SystemTime::now(),
                process_is_definitely_dead,
            );
        })
    {
        log::debug!("could not start pasted-image crash cleanup: {error}");
    }
}

fn sweep_stale_in(root: &Path, now: SystemTime, mut process_is_dead: impl FnMut(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let started = Instant::now();
    let mut attempted = 0_usize;
    let mut reaped = 0_usize;
    for entry in entries.take(MAX_SWEEP_ENTRIES).flatten() {
        if attempted >= MAX_SWEEP_ATTEMPTS
            || reaped >= MAX_SWEEP_SESSIONS
            || started.elapsed() >= MAX_SWEEP_DURATION
        {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(SessionName {
            pid,
            nonce: _session_nonce,
        }) = parse_session_name(name)
        else {
            continue;
        };
        let apparently_stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if !apparently_stale {
            continue;
        }
        attempted += 1;
        if !process_is_dead(pid) {
            continue;
        }
        if reap_stale_session(&entry.path(), now).is_ok() {
            reaped += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionName {
    pid: u32,
    nonce: u128,
}

fn canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn parse_session_name(name: &str) -> Option<SessionName> {
    let suffix = name.strip_prefix(DIR_PREFIX)?;
    let (pid, nonce) = suffix.split_once('-')?;
    if nonce.contains('-') || !canonical_decimal(pid) || !canonical_decimal(nonce) {
        return None;
    }
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid != 0)?;
    let nonce = nonce.parse::<u128>().ok()?;
    Some(SessionName { pid, nonce })
}

fn parse_image_name(name: &str) -> Option<usize> {
    let sequence = name.strip_suffix(".png")?;
    if sequence.len() < 4 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = sequence
        .parse::<usize>()
        .ok()
        .filter(|value| (1..=MAX_FILES).contains(value))?;
    (format!("{value:04}.png") == name).then_some(value)
}

fn reap_stale_session(path: &Path, now: SystemTime) -> io::Result<()> {
    let directory = open_session_directory(path)?;
    verify_session_directory_path(&directory)?;
    let modified = directory.file.metadata()?.modified()?;
    if now
        .duration_since(modified)
        .ok()
        .is_none_or(|age| age <= STALE_AFTER)
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "pasted-image session is not stale",
        ));
    }

    let mut files = Vec::new();
    let mut entries = std::fs::read_dir(session_directory_capability_path(&directory))?;
    for _ in 0..MAX_FILES {
        let Some(entry) = entries.next() else {
            break;
        };
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pasted-image session contains a non-UTF-8 name",
            ));
        };
        if parse_image_name(name).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("pasted-image session contains an unknown entry: {name}"),
            ));
        }
        verify_session_directory_path(&directory)?;
        let child = entry.path();
        let file = kettle_state::open_existing_private_file(&child)?;
        verify_session_directory_path(&directory)?;
        files.push((file, child));
    }
    if entries.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pasted-image session exceeds its file-count bound",
        ));
    }
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pasted-image session contains no verified artifacts",
        ));
    }

    for (file, child) in files {
        verify_session_directory_path(&directory)?;
        kettle_state::remove_open_private_file(file, &child)?;
    }
    remove_session_directory(directory)
}

fn scratch_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("kettle").join("paste");
        }
    }
    std::env::temp_dir()
}

fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    kettle_state::create_private_file_new(path)
}

/// Establish the session directory without ever writing clipboard content
/// through its pathname.
///
/// The private-file helper securely creates the parent tree on every supported
/// platform. A short-lived, empty bootstrap file gives us a directory to open
/// and identify; only after that exact directory is held do real PNG creations
/// use its capability. If a same-user process races the create/open boundary,
/// identity comparison fails before any screenshot bytes are encoded.
fn establish_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    let bootstrap_path = path.join("0001.png");
    let creator = create_private_file(&bootstrap_path)?;
    let directory = match open_session_directory(path) {
        Ok(directory) => directory,
        Err(error) => {
            discard_private_file(creator, &bootstrap_path);
            return Err(error);
        }
    };
    let capability_path = session_child_path(&directory, std::ffi::OsStr::new("0001.png"));
    let reopened = match kettle_state::open_existing_private_file(&capability_path) {
        Ok(reopened) => reopened,
        Err(error) => {
            discard_private_file(creator, &capability_path);
            return Err(error);
        }
    };
    let same_file = match same_open_file_identity(&creator, &reopened) {
        Ok(same_file) => same_file,
        Err(error) => {
            drop(reopened);
            discard_private_file(creator, &capability_path);
            return Err(error);
        }
    };
    if !same_file {
        drop(reopened);
        discard_private_file(creator, &capability_path);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image bootstrap path no longer identifies the created file",
        ));
    }

    // The reopened handle remains an exact identity anchor after the creator's
    // restrictive Windows share mode is released.
    drop(creator);
    kettle_state::remove_open_private_file(reopened, &capability_path)?;
    verify_session_directory_path(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn create_private_file_in_session(directory: &SessionDirectory, path: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;

    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pasted-image path has no file name",
        )
    })?;
    if Path::new(name).components().count() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pasted-image file name is not a single path component",
        ));
    }
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pasted-image file name contains a NUL byte",
        )
    })?;
    // SAFETY: `name` is NUL-terminated, the held descriptor identifies an
    // owner-private directory, and a successful call transfers a new fd.
    let descriptor = unsafe {
        libc::openat(
            directory.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            discard_private_file(file, path);
            return Err(error);
        }
    };
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        discard_private_file(file, path);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "new pasted image is not an owner-private, single-link regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn create_private_file_in_session(_directory: &SessionDirectory, path: &Path) -> io::Result<File> {
    create_private_file(path)
}

#[cfg(not(windows))]
fn discard_private_file(file: File, path: &Path) {
    // Never fall back to a pathname delete: if the entry changed after
    // creation, preserving the replacement is safer than guessing.
    if let Err(error) = kettle_state::remove_open_private_file(file, path) {
        log::debug!(
            "failed to discard partial pasted image {}: {error}",
            path.display()
        );
    }
}

#[cfg(windows)]
fn discard_private_file(file: File, path: &Path) {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // The private creator requested DELETE on this exact handle. Marking that
    // kernel object is both race-free and compatible with its intentionally
    // restrictive share mode.
    let result = unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .expect("FILE_DISPOSITION_INFO size fits u32"),
        )
    };
    if let Err(error) = result {
        log::debug!(
            "failed to discard partial pasted image {}: {error}",
            path.display()
        );
    }
}

fn budget_error(files: usize, bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::QuotaExceeded,
        format!("pasted-image budget reached ({files} files, {bytes} bytes)"),
    )
}

struct BudgetWriter<W> {
    inner: W,
    remaining: u64,
    written: u64,
    exceeded: bool,
}

impl<W> BudgetWriter<W> {
    fn new(inner: W, remaining: u64) -> Self {
        Self {
            inner,
            remaining,
            written: 0,
            exceeded: false,
        }
    }

    fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: io::Write> io::Write for BudgetWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if requested > self.remaining {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "encoded PNG exceeds the remaining pasted-image budget",
            ));
        }
        let written = self.inner.write(buffer)?;
        let written = u64::try_from(written).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded PNG writer returned an invalid byte count",
            )
        })?;
        self.remaining = self.remaining.checked_sub(written).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded PNG writer exceeded its declared byte count",
            )
        })?;
        self.written = self.written.checked_add(written).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "encoded PNG byte count overflowed",
            )
        })?;
        Ok(usize::try_from(written).expect("written originated as usize"))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(unix)]
fn same_open_file_identity(left: &File, right: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut information = FILE_ID_INFO::default();
    // SAFETY: `file` is live and `information` is a correctly sized, writable
    // output buffer.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            std::ptr::addr_of_mut!(information).cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO size fits u32"),
        )
    }
    .map_err(io::Error::other)?;
    Ok((
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    ))
}

#[cfg(windows)]
fn same_open_file_identity(left: &File, right: &File) -> io::Result<bool> {
    Ok(windows_file_identity(left)? == windows_file_identity(right)?)
}

#[cfg(not(any(unix, windows)))]
fn same_open_file_identity(_left: &File, _right: &File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "open-file identity is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn session_directory_matches_path(directory: &SessionDirectory) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let current = std::fs::symlink_metadata(&directory.path)?;
    Ok(current.file_type().is_dir()
        && current.uid() == unsafe { libc::geteuid() }
        && current.mode() & 0o777 == 0o700
        && current.dev() == directory.device
        && current.ino() == directory.inode)
}

#[cfg(windows)]
fn session_directory_matches_path(directory: &SessionDirectory) -> io::Result<bool> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    let current = options.open(&directory.path)?;
    let metadata = current.metadata()?;
    Ok(metadata.file_type().is_dir()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
        && same_open_file_identity(&directory.file, &current)?)
}

#[cfg(not(any(unix, windows)))]
fn session_directory_matches_path(directory: &SessionDirectory) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(&directory.path)?
        .file_type()
        .is_dir())
}

fn verify_session_directory_path(directory: &SessionDirectory) -> io::Result<()> {
    if session_directory_matches_path(directory)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session path no longer identifies the held directory",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn session_directory_capability_path(directory: &SessionDirectory) -> PathBuf {
    use std::os::fd::AsRawFd as _;

    PathBuf::from(format!("/proc/self/fd/{}", directory.file.as_raw_fd()))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn session_directory_capability_path(directory: &SessionDirectory) -> PathBuf {
    use std::os::fd::AsRawFd as _;

    PathBuf::from(format!("/dev/fd/{}", directory.file.as_raw_fd()))
}

#[cfg(not(unix))]
fn session_directory_capability_path(directory: &SessionDirectory) -> PathBuf {
    directory.path.clone()
}

fn session_child_path(directory: &SessionDirectory, name: &std::ffi::OsStr) -> PathBuf {
    session_directory_capability_path(directory).join(name)
}

#[cfg(unix)]
fn open_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "pasted-image session is not an owner-private ordinary directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SessionDirectory {
        path: path.to_path_buf(),
        file,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn open_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES).0)
        // Denying delete-sharing pins this directory name/identity while files
        // are enumerated, created, reopened, or removed beneath it.
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "pasted-image session is not a non-reparse directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SessionDirectory {
        path: path.to_path_buf(),
        file,
    })
}

#[cfg(not(any(unix, windows)))]
fn open_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    let file = File::open(path)?;
    if !file.metadata()?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "pasted-image session is not an ordinary directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SessionDirectory {
        path: path.to_path_buf(),
        file,
    })
}

#[cfg(unix)]
fn remove_session_directory(directory: SessionDirectory) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let current = std::fs::symlink_metadata(&directory.path)?;
    if !current.file_type().is_dir()
        || current.uid() != unsafe { libc::geteuid() }
        || current.mode() & 0o777 != 0o700
        || current.dev() != directory.device
        || current.ino() != directory.inode
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session path no longer identifies the held directory",
        ));
    }
    std::fs::remove_dir(&directory.path)
}

#[cfg(windows)]
fn remove_session_directory(directory: SessionDirectory) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileDispositionInfo, SetFileInformationByHandle,
    };

    let original_identity = windows_file_identity(&directory.file)?;
    let mut transition_options = OpenOptions::new();
    transition_options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    // This no-DELETE transition handle can coexist with the name-pinning
    // lifetime handle. It preserves the exact identity across releasing that
    // pin and acquiring the final DELETE-capable handle.
    let transition = transition_options.open(&directory.path)?;
    if windows_file_identity(&transition)? != original_identity {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session path no longer identifies the held directory",
        ));
    }
    drop(directory.file);
    let mut deletion_options = OpenOptions::new();
    deletion_options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    let deletion = deletion_options.open(&directory.path)?;
    if windows_file_identity(&deletion)? != original_identity
        || !same_open_file_identity(&transition, &deletion)?
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session changed while acquiring delete access",
        ));
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `deletion` has DELETE access and the buffer exactly matches the
    // requested information class.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(deletion.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .expect("FILE_DISPOSITION_INFO size fits u32"),
        )
    }
    .map_err(io::Error::other)
}

#[cfg(not(any(unix, windows)))]
fn remove_session_directory(directory: SessionDirectory) -> io::Result<()> {
    let _held = directory.file;
    std::fs::remove_dir(directory.path)
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
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the requested access is query-only and every successful handle
    // is closed below.
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(process) => process,
        Err(error) => {
            // Access denial and transient failures are not evidence of death.
            return WIN32_ERROR::from_error(&error) == Some(ERROR_INVALID_PARAMETER);
        }
    };
    let mut exit_code = 0_u32;
    // SAFETY: `process` is live and `exit_code` is writable for the call.
    let queried = unsafe { GetExitCodeProcess(process, &mut exit_code) }.is_ok();
    // SAFETY: OpenProcess transferred this handle to us.
    let _ = unsafe { CloseHandle(process) };
    queried && exit_code != STILL_ACTIVE.0 as u32
}

#[cfg(not(any(unix, windows)))]
fn process_is_definitely_dead(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn scratch(name: &str) -> PathBuf {
        scratch_root().join(format!(
            "kettle-paste-test-{name}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn stale_session(root: &Path, pid: u32) -> (PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
            .saturating_add(u128::from(TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        let directory = root.join(format!("{DIR_PREFIX}{pid}-{nonce}"));
        let image = directory.join("0001.png");
        let mut file = create_private_file(&image).expect("create stale image");
        file.write_all(b"private image bytes")
            .expect("write stale image");
        drop(file);
        (directory, image)
    }

    #[test]
    fn saves_rgba_as_a_real_png() {
        let dir = scratch("save");
        let mut images = PastedImages::with_dir(dir.clone());
        // 2x2 opaque red.
        let rgba = [255u8, 0, 0, 255].repeat(4);
        let path = images.save_rgba(2, 2, &rgba).expect("save");
        assert!(path.exists(), "the PNG must actually be written");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "must be a real PNG, not raw bytes"
        );
        // Round-trip through the decoder to prove the geometry survived.
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        // A second paste gets its own file rather than clobbering the first.
        let second = images.save_rgba(2, 2, &rgba).expect("save 2");
        assert_ne!(path, second);
        images.cleanup();
        assert!(!dir.exists(), "cleanup removes the whole directory");
    }

    #[cfg(windows)]
    #[test]
    fn default_windows_scratch_directory_uses_local_app_data() {
        let images = PastedImages::new();
        assert_eq!(
            images.dir.parent(),
            Some(scratch_root().as_path()),
            "Windows scratch images must not use a temp directory shared with sandbox principals"
        );
    }

    #[test]
    fn rejects_malformed_geometry_instead_of_panicking() {
        let dir = scratch("malformed");
        let mut images = PastedImages::with_dir(dir.clone());
        // Byte count disagrees with the declared size — the encoder would index
        // out of bounds if this were trusted.
        assert!(images.save_rgba(4, 4, &[0u8; 8]).is_err());
        assert!(images.save_rgba(0, 4, &[]).is_err(), "zero dimension");
        assert!(images.save_rgba(4, 0, &[]).is_err(), "zero dimension");
        assert!(
            images.save_rgba(MAX_DIMENSION + 1, 1, &[0u8; 4]).is_err(),
            "absurd dimensions are refused before allocating"
        );
        assert!(
            images
                .save_rgba(MAX_DIMENSION, 4_097, &[])
                .unwrap_err()
                .to_string()
                .contains("RGBA buffer exceeds"),
            "declared source buffers above 256 MiB are refused before byte access"
        );
        // A rejected paste must not create anything on disk.
        assert!(!dir.exists());
        images.cleanup();
    }

    #[test]
    fn enforces_the_per_session_file_budget() {
        let dir = scratch("budget");
        let mut images = PastedImages::with_dir(dir.clone());
        let rgba = vec![0u8, 0, 0, 255];
        for i in 0..MAX_FILES {
            images.save_rgba(1, 1, &rgba).unwrap_or_else(|e| {
                panic!("save {i} within budget failed: {e}");
            });
        }
        assert!(
            images.save_rgba(1, 1, &rgba).is_err(),
            "a paste loop must not be able to fill the disk"
        );
        images.cleanup();
        assert!(!dir.exists());
    }

    #[test]
    fn final_encoded_png_cannot_cross_the_aggregate_byte_budget() {
        let dir = scratch("encoded-budget");
        let mut images = PastedImages::with_dir(dir.clone());
        images.bytes = MAX_TOTAL_BYTES - 1;
        let error = images
            .save_rgba(1, 1, &[0, 0, 0, 255])
            .expect_err("a PNG cannot fit in one remaining byte");
        assert_eq!(error.kind(), io::ErrorKind::QuotaExceeded);
        assert_eq!(images.bytes, MAX_TOTAL_BYTES - 1);
        assert_eq!(images.seq, 0, "failed encodes do not consume a sequence");
        assert!(
            images.files.is_empty(),
            "failed PNGs are never live artifacts"
        );
        assert!(
            std::fs::read_dir(&dir)
                .expect("failed encode leaves a verified empty session")
                .next()
                .is_none(),
            "the partial PNG must be removed by its original handle"
        );
        images.bytes = 0;
        let retry = images
            .save_rgba(1, 1, &[0, 0, 0, 255])
            .expect("the failed sequence is reusable");
        assert_eq!(retry.file_name(), Some(std::ffi::OsStr::new("0001.png")));
        images.cleanup();
        assert!(!dir.exists());
    }

    #[test]
    fn bounded_writer_counts_only_bytes_it_accepts() {
        let mut writer = BudgetWriter::new(Vec::new(), 3);
        writer.write_all(&[1, 2, 3]).expect("exact budget");
        let error = writer.write_all(&[4]).expect_err("over budget");
        assert_eq!(error.kind(), io::ErrorKind::QuotaExceeded);
        assert!(writer.exceeded());
        assert_eq!(writer.written(), 3);
        assert_eq!(writer.into_inner(), vec![1, 2, 3]);
    }

    #[test]
    fn retained_handle_must_match_the_creator_identity() {
        let dir = scratch("identity-compare");
        let first_path = dir.join("first.png");
        let first = create_private_file(&first_path).expect("first creator");
        let first_reopened =
            kettle_state::open_existing_private_file(&first_path).expect("reopen first");
        assert!(
            same_open_file_identity(&first, &first_reopened).expect("compare same file"),
            "the cooperative handle must identify the creator's exact object"
        );

        let second_path = dir.join("second.png");
        let second = create_private_file(&second_path).expect("second creator");
        assert!(
            !same_open_file_identity(&first, &second).expect("compare different files"),
            "two private files must not alias one identity"
        );

        drop(first_reopened);
        discard_private_file(first, &first_path);
        discard_private_file(second, &second_path);
        std::fs::remove_dir(dir).expect("remove identity test directory");
    }

    #[test]
    fn session_and_image_names_require_the_exact_creator_grammar() {
        assert_eq!(
            parse_session_name("kettle-paste-42-0"),
            Some(SessionName { pid: 42, nonce: 0 })
        );
        for near_miss in [
            "kettle-paste-0-1",
            "kettle-paste-042-1",
            "kettle-paste-4294967296-1",
            "kettle-paste-42-01",
            "kettle-paste-42",
            "kettle-paste-42-1-extra",
            "kettle-paste-42-340282366920938463463374607431768211456",
            "other-42-1",
        ] {
            assert_eq!(parse_session_name(near_miss), None, "{near_miss}");
        }
        assert_eq!(parse_image_name("0001.png"), Some(1));
        assert_eq!(parse_image_name("0064.png"), Some(64));
        for near_miss in [
            "1.png",
            "0000.png",
            "00001.png",
            "0065.png",
            "9999.png",
            "10000.png",
            "0001.PNG",
            "0001.png.extra",
            "18446744073709551616.png",
            "note.txt",
        ] {
            assert_eq!(parse_image_name(near_miss), None, "{near_miss}");
        }
    }

    #[test]
    fn old_live_session_is_preserved_but_dead_session_is_reaped() {
        let root = scratch("stale-root");
        let pid = std::process::id();
        let (directory, image) = stale_session(&root, pid);
        let future = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);

        sweep_stale_in(&root, future, |candidate| {
            assert_eq!(candidate, pid);
            false
        });
        assert!(image.exists(), "age is never sufficient deletion authority");

        sweep_stale_in(&root, future, |candidate| {
            assert_eq!(candidate, pid);
            true
        });
        assert!(!directory.exists(), "a verified dead session is reclaimed");
        let _ = std::fs::remove_dir(&root);
    }

    #[test]
    fn stale_sweep_fails_closed_for_unknown_and_hard_linked_entries() {
        let root = scratch("stale-adversarial");
        let pid = std::process::id();
        let (unknown_dir, unknown_image) = stale_session(&root, pid);
        let unknown = unknown_dir.join("notes.txt");
        let unknown_file = create_private_file(&unknown).expect("unknown sibling");
        drop(unknown_file);

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
            .saturating_add(10_000);
        let linked_dir = root.join(format!("{DIR_PREFIX}{pid}-{nonce}"));
        let linked_image = linked_dir.join("0001.png");
        let linked = create_private_file(&linked_image).expect("linked image");
        drop(linked);
        let outside_link = root.join(format!("outside-hardlink-{nonce}.png"));
        std::fs::hard_link(&linked_image, &outside_link).expect("create hard-link sentinel");

        let future = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);
        sweep_stale_in(&root, future, |_| true);
        assert!(unknown_image.exists() && unknown.exists());
        assert!(linked_image.exists() && outside_link.exists());

        std::fs::remove_file(unknown_image).expect("remove image");
        std::fs::remove_file(unknown).expect("remove unknown");
        std::fs::remove_dir(unknown_dir).expect("remove unknown dir");
        std::fs::remove_file(linked_image).expect("remove linked image");
        std::fs::remove_file(outside_link).expect("remove outside link");
        std::fs::remove_dir(linked_dir).expect("remove linked dir");
        std::fs::remove_dir(root).expect("remove test root");
    }

    #[test]
    fn current_process_is_never_reported_definitively_dead() {
        assert!(!process_is_definitely_dead(std::process::id()));
    }

    #[cfg(windows)]
    #[test]
    fn invalid_windows_pid_is_definitively_dead() {
        assert!(process_is_definitely_dead(u32::MAX));
    }

    #[cfg(unix)]
    #[test]
    fn held_directory_creation_never_writes_into_a_path_replacement() {
        use std::os::unix::fs::DirBuilderExt as _;

        let path = scratch("held-create");
        let displaced = path.with_extension("displaced");
        let directory = establish_session_directory(&path).expect("establish session");
        std::fs::rename(&path, &displaced).expect("displace held session");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create path replacement");

        let capability = session_child_path(&directory, std::ffi::OsStr::new("0001.png"));
        let mut file =
            create_private_file_in_session(&directory, &capability).expect("create through fd");
        file.write_all(b"screenshot bytes")
            .expect("write through held directory");
        assert!(
            !path.join("0001.png").exists(),
            "clipboard bytes must never enter the pathname replacement"
        );
        assert_eq!(
            std::fs::read(displaced.join("0001.png")).expect("read held-directory file"),
            b"screenshot bytes"
        );

        kettle_state::remove_open_private_file(file, &capability).expect("remove exact file");
        drop(directory);
        std::fs::remove_dir(displaced).expect("remove displaced session");
        std::fs::remove_dir(path).expect("remove replacement");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_preserves_a_path_replacement() {
        let dir = scratch("identity-replacement");
        let mut images = PastedImages::with_dir(dir.clone());
        let path = images
            .save_rgba(1, 1, &[1, 2, 3, 4])
            .expect("save original");
        std::fs::remove_file(&path).expect("unlink original while its handle is retained");
        let mut replacement = create_private_file(&path).expect("create replacement");
        replacement
            .write_all(b"replacement")
            .expect("write replacement");
        drop(replacement);

        images.cleanup();
        assert_eq!(
            std::fs::read(&path).expect("replacement survives"),
            b"replacement"
        );
        std::fs::remove_file(path).expect("remove replacement");
        std::fs::remove_dir(dir).expect("remove test directory");
    }

    #[test]
    fn sweep_leaves_this_process_directory_alone() {
        // The production liveness probe independently protects this process.
        let mut images = PastedImages::new();
        let own = images.dir.clone();
        images.save_rgba(1, 1, &[1u8, 2, 3, 4]).expect("save");
        sweep_stale();
        assert!(
            own.exists(),
            "sweep must skip the running process's own dir"
        );
        images.cleanup();
    }
}
