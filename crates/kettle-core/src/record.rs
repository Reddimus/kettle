//! Asciicast v2 session recorder (cargo feature `asciicast`).
//!
//! Cycle 875 introduced this as kettle-ui's developer-only `dev-record`
//! recorder; cycle 924 (agent-first A1) promoted it here to kettle-core — the
//! crate that owns the `Terminal` — so it is the ONE shared recorder behind
//! both the GUI's `--record` (kettle-ui `dev-record` feature) and `kettle exec
//! --record` (the bin enables `kettle-core/asciicast` unconditionally, so
//! recording an agent run ships in release builds; that path is output-only —
//! no keystroke-privacy surface).
//!
//! Writes an asciicast v2-compatible NDJSON trace that replays in
//! `asciinema play`:
//!
//! - line 1: a `{"version":2,"width":W,"height":H,...}` header
//! - `[t, "o", <utf8>]`   — terminal OUTPUT
//! - `[t, "r", "CxR"]`    — resize
//! - `[t, "m", <json>]`   — kettle UI/UX markers (players ignore them)
//! - `[t, "i", <token>]`  — keystroke TOKENS, never raw typed chars (cycle 876)
//!
//! The file is created `0600` on Unix and is purely local — kettle never
//! uploads it. Writes are best-effort: the first I/O error disables the
//! recorder (a full disk must never crash the terminal).
//!
//! Privacy: terminal OUTPUT is VERBATIM and cannot be redacted — a terminal
//! can't tell a secret from normal output, so anything printed/echoed on
//! screen lands in cleartext. Review/scrub a `.cast` before sharing it (see
//! docs/DEV-RECORD.md).

use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// v2.20.0 P5 (perf): how often buffered events are flushed to disk. Events
/// between flushes sit in the `BufWriter` (which also self-flushes whenever
/// its 8KiB buffer fills, so a flood can't grow the loss window); a hard
/// crash loses at most this much trailing trace. `finish` / `Drop` still
/// flush, so every clean close path produces a complete, replayable file —
/// the cycle-908 closure verification is unaffected.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// A single trace stops at an event boundary before growing past 512 MiB.
pub const MAX_RECORD_BYTES: u64 = 512 * 1024 * 1024;

/// Automatic directory recording retains at most 50 Kettle-owned casts.
pub const MAX_RECORD_FILES: usize = 50;

/// Automatic directory recording retains at most 5 GiB of Kettle-owned casts.
pub const MAX_RECORD_DIRECTORY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

const DIRECTORY_RECORD_PREFIX: &str = "kettle-session-";
const DIRECTORY_RECORD_SUFFIX: &str = ".cast";
static RECORD_SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Where a GUI development recording should be written.
///
/// Explicit files preserve the historical overwrite behavior. Directory
/// targets create a private directory as needed, allocate a collision-safe
/// file with `create_new`, and apply the bounded retention policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingTarget {
    File(PathBuf),
    Directory(PathBuf),
}

/// Observable state of a recorder. Callers keep the recorder alive after a
/// limit or I/O failure so the UI can report why capture stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    Recording,
    LimitReached,
    IoError,
}

/// An append-only asciicast writer. One per recording session.
pub struct Recorder {
    writer: BufWriter<File>,
    start: Instant,
    status: RecordStatus,
    bytes_written: u64,
    max_bytes: u64,
    /// Cycle 876: when true, record raw typed characters in `i` events.
    /// Default false — bare printables are redacted to a generic class so a
    /// typed password never lands in the trace (`--record-raw-input` opts in).
    raw_input: bool,
    /// Trailing bytes of an INCOMPLETE multibyte UTF-8 sequence carried over to
    /// the next `record_output` chunk, so a codepoint split across two PTY reads
    /// is decoded whole instead of being mangled into U+FFFD on each side.
    utf8_carry: Vec<u8>,
    /// v2.20.0 P5: when the buffer was last explicitly flushed (see
    /// [`FLUSH_INTERVAL`]).
    last_flush: Instant,
    /// v2.20.0 (review fix): lines written since the last flush. Without
    /// this, a burst followed by silence left the tail buffered FOREVER
    /// (the interval flush is event-driven) — `flush_if_stale` lets the
    /// app's timer loop bound staleness to ~FLUSH_INTERVAL in wall time.
    dirty: bool,
}

impl Recorder {
    /// Open an explicit `path` (truncating only after obtaining its exclusive
    /// lock), write the asciicast header, and start the monotonic clock.
    pub fn start(path: &Path, cols: u16, rows: u16, raw_input: bool) -> std::io::Result<Self> {
        let file = open_private(path)?;
        Self::start_with_file(file, cols, rows, raw_input, MAX_RECORD_BYTES)
    }

    /// Start from a typed target and return the actual output path. Directory
    /// targets are created privately and never truncate an existing cast.
    pub fn start_target(
        target: &RecordingTarget,
        cols: u16,
        rows: u16,
        raw_input: bool,
    ) -> std::io::Result<(Self, PathBuf)> {
        match target {
            RecordingTarget::File(path) => {
                let recorder = Self::start(path, cols, rows, raw_input)?;
                Ok((recorder, path.clone()))
            }
            RecordingTarget::Directory(directory) => {
                prepare_private_directory(directory)?;
                let (path, file) = create_unique_private_recording(directory)?;
                let recorder =
                    match Self::start_with_file(file, cols, rows, raw_input, MAX_RECORD_BYTES) {
                        Ok(recorder) => recorder,
                        Err(error) => {
                            let _ = std::fs::remove_file(&path);
                            return Err(error);
                        }
                    };
                if let Err(error) = prune_recording_directory(
                    directory,
                    MAX_RECORD_DIRECTORY_BYTES,
                    MAX_RECORD_FILES,
                ) {
                    log::warn!(
                        "record: could not apply retention in {}: {error}",
                        directory.display()
                    );
                }
                Ok((recorder, path))
            }
        }
    }

    fn start_with_file(
        file: File,
        cols: u16,
        rows: u16,
        raw_input: bool,
        max_bytes: u64,
    ) -> std::io::Result<Self> {
        let mut writer = BufWriter::new(file);
        let header = header_line(cols, rows);
        let header_bytes = u64::try_from(header.len() + 1).unwrap_or(u64::MAX);
        if header_bytes > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "recording limit is too small for the asciicast header",
            ));
        }
        writeln!(writer, "{header}")?;
        writer.flush()?;
        Ok(Self {
            writer,
            start: Instant::now(),
            status: RecordStatus::Recording,
            bytes_written: header_bytes,
            max_bytes,
            raw_input,
            utf8_carry: Vec::new(),
            last_flush: Instant::now(),
            dirty: false,
        })
    }

    /// Whether raw typed characters are captured (vs redacted). Cycle 876.
    pub fn raw_input(&self) -> bool {
        self.raw_input
    }

    /// Current capture state for visible UI/status reporting.
    pub fn status(&self) -> RecordStatus {
        self.status
    }

    /// Bytes accepted by the writer, including the asciicast header.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    fn emit(&mut self, code: &str, data: &str) {
        if self.status != RecordStatus::Recording {
            return;
        }
        let secs = self.start.elapsed().as_secs_f64();
        let line = event_line(secs, code, data);
        let event_bytes = u64::try_from(line.len() + 1).unwrap_or(u64::MAX);
        if self.bytes_written.saturating_add(event_bytes) > self.max_bytes {
            self.stop_at_limit(secs);
            return;
        }
        // v2.20.0 P5: flush on a ~250ms interval instead of per event. The
        // old per-event flush put a write syscall on the UI thread for every
        // PTY read under flood — and the installed dev-record build records
        // EVERY session. Crash exposure is bounded by FLUSH_INTERVAL;
        // `finish`/`Drop` keep the clean-close trace complete.
        let flush_due = self.last_flush.elapsed() >= FLUSH_INTERVAL;
        let result = writeln!(self.writer, "{line}").and_then(|()| {
            self.bytes_written += event_bytes;
            if flush_due {
                self.last_flush = Instant::now();
                self.dirty = false;
                self.writer.flush()
            } else {
                self.dirty = true;
                Ok(())
            }
        });
        if result.is_err() {
            log::warn!("record: write failed; disabling the recorder");
            self.status = RecordStatus::IoError;
        }
    }

    fn stop_at_limit(&mut self, secs: f64) {
        let marker = event_line(
            secs,
            "m",
            &format!("kettle:record_limit bytes={}", self.max_bytes),
        );
        let marker_bytes = u64::try_from(marker.len() + 1).unwrap_or(u64::MAX);
        if self.bytes_written.saturating_add(marker_bytes) <= self.max_bytes {
            if let Err(error) = writeln!(self.writer, "{marker}") {
                self.status = RecordStatus::IoError;
                self.dirty = false;
                log::warn!("record: could not write size-limit marker: {error}");
                return;
            }
            self.bytes_written += marker_bytes;
        }
        self.dirty = false;
        if self.writer.flush().is_err() {
            self.status = RecordStatus::IoError;
            log::warn!("record: flush failed at size limit; disabling the recorder");
        } else {
            self.status = RecordStatus::LimitReached;
            log::warn!(
                "record: {} byte session limit reached; capture stopped at an event boundary",
                self.max_bytes
            );
        }
    }

    /// v2.20.0 (review fix): flush buffered events if any have been sitting
    /// unflushed past `FLUSH_INTERVAL` (250ms). The interval flush in `emit` is
    /// EVENT-driven — a burst followed by silence would otherwise leave its
    /// tail buffered until the next event or a clean close. The app's timer
    /// loop calls this (see `flush_deadline`) to bound the staleness in
    /// wall-clock time.
    pub fn flush_if_stale(&mut self) {
        if self.status != RecordStatus::Recording
            || !self.dirty
            || self.last_flush.elapsed() < FLUSH_INTERVAL
        {
            return;
        }
        self.last_flush = Instant::now();
        self.dirty = false;
        if self.writer.flush().is_err() {
            log::warn!("record: flush failed; disabling the recorder");
            self.status = RecordStatus::IoError;
        }
    }

    /// When `flush_if_stale` next needs to run, or `None` when nothing is
    /// buffered. Lets the caller schedule a precise wake instead of polling.
    pub fn flush_deadline(&self) -> Option<Instant> {
        (self.status == RecordStatus::Recording && self.dirty)
            .then(|| self.last_flush + FLUSH_INTERVAL)
    }

    /// Record a chunk of terminal OUTPUT (`o`). A multibyte codepoint split
    /// across two PTY reads is carried over and decoded whole (not mangled into
    /// U+FFFD on each side); genuinely-invalid bytes still become U+FFFD so the
    /// trace stays valid asciicast / valid JSON.
    ///
    /// Privacy: this is VERBATIM and cannot be redacted — a terminal can't tell
    /// a secret from normal output, so anything printed/echoed on screen lands
    /// in the trace in cleartext. Review/scrub a `.cast` before sharing it (see
    /// docs/DEV-RECORD.md).
    pub fn record_output(&mut self, bytes: &[u8]) {
        if self.status != RecordStatus::Recording {
            return;
        }
        self.utf8_carry.extend_from_slice(bytes);
        let mut out = String::new();
        // Decode as much valid UTF-8 as possible; loop so a chunk that contains
        // [valid][invalid][valid] emits all of it, retaining only a genuinely-
        // incomplete trailing sequence for the next call.
        loop {
            match std::str::from_utf8(&self.utf8_carry) {
                Ok(s) => {
                    out.push_str(s);
                    self.utf8_carry.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    // SAFETY: bytes up to `valid` are guaranteed valid UTF-8.
                    out.push_str(unsafe {
                        std::str::from_utf8_unchecked(&self.utf8_carry[..valid])
                    });
                    match e.error_len() {
                        // Incomplete trailing sequence — keep it for the next chunk.
                        None => {
                            self.utf8_carry.drain(..valid);
                            break;
                        }
                        // A genuinely-invalid run — emit one replacement, drop it,
                        // and continue decoding the remainder.
                        Some(n) => {
                            out.push('\u{FFFD}');
                            self.utf8_carry.drain(..valid + n);
                        }
                    }
                }
            }
        }
        if !out.is_empty() {
            self.emit("o", &out);
        }
    }

    /// Record a grid resize (`r`), data `"<cols>x<rows>"`.
    pub fn record_resize(&mut self, cols: u16, rows: u16) {
        self.emit("r", &format!("{cols}x{rows}"));
    }

    /// Record a keystroke as an `i` event. Cycle 876: the caller passes a
    /// privacy-preserving TOKEN (a named key / chord like `Enter` / `Ctrl+c`,
    /// or a redacted printable class via `printable_token`) — never raw typed
    /// characters unless raw-input mode was opted into. Pasted content is never
    /// routed here (it's a `paste` marker instead).
    pub fn record_input(&mut self, token: &str) {
        self.emit("i", token);
    }

    /// Record a kettle UI/UX state transition as an `m` marker (cycle 876).
    /// `label` is a short tag like `kettle:tab_add` / `kettle:focus_out` /
    /// `kettle:agent send_text pane=3`. Players that understand markers show
    /// the label; others ignore it. Captures state the PTY output stream can't
    /// (kettle's own tab bar / overlays / focus / agent control), incl.
    /// non-interactive transitions.
    pub fn record_marker(&mut self, label: &str) {
        self.emit("m", label);
    }

    /// Flush any buffered events. Called on close and from `Drop`. Emits any
    /// trailing carried-over bytes (a genuinely-truncated final UTF-8 sequence)
    /// as a U+FFFD so no output is silently dropped at end-of-stream.
    pub fn finish(&mut self) {
        if !self.utf8_carry.is_empty() && self.status == RecordStatus::Recording {
            let tail = String::from_utf8_lossy(&self.utf8_carry).into_owned();
            self.utf8_carry.clear();
            self.emit("o", &tail);
        }
        self.dirty = false;
        if self.writer.flush().is_err() && self.status == RecordStatus::Recording {
            self.status = RecordStatus::IoError;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Open `path` for explicit-file recording. Lock before truncating so two
/// launches targeting the same path cannot corrupt an active trace.
fn open_private(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
    configure_private_file_open(&mut opts, true);
    let mut file = opts.open(path)?;
    ensure_regular_file(&file, path)?;
    fs4::FileExt::try_lock(&file).map_err(std::io::Error::from)?;
    set_private_file_mode(&file)?;
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    Ok(file)
}

fn create_unique_private_recording(directory: &Path) -> std::io::Result<(PathBuf, File)> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    for _ in 0..10_000 {
        let sequence = RECORD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "{DIRECTORY_RECORD_PREFIX}{seconds}-{pid}-{sequence}{DIRECTORY_RECORD_SUFFIX}"
        ));
        let mut opts = std::fs::OpenOptions::new();
        opts.create_new(true).read(true).write(true);
        configure_private_file_open(&mut opts, true);
        match opts.open(&path) {
            Ok(file) => {
                if let Err(error) = ensure_regular_file(&file, &path)
                    .and_then(|()| fs4::FileExt::try_lock(&file).map_err(std::io::Error::from))
                    .and_then(|()| set_private_file_mode(&file))
                {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
                return Ok((path, file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique recording name after 10000 attempts",
    ))
}

fn prepare_private_directory(directory: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "recording directory must not be a symbolic link: {}",
                    directory.display()
                ),
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!("{} is not a directory", directory.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(directory)?;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    configure_private_directory_open(&mut options);
    let handle = options.open(directory)?;
    if !handle.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} is not a directory", directory.display()),
        ));
    }
    let current = std::fs::symlink_metadata(directory)?;
    if current.file_type().is_symlink() || !current.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "recording directory changed while it was being opened: {}",
                directory.display()
            ),
        ));
    }
    ensure_same_file(&handle.metadata()?, &current, directory)?;
    set_private_directory_mode(&handle)?;
    Ok(())
}

fn ensure_regular_file(file: &File, path: &Path) -> std::io::Result<()> {
    if file.metadata()?.file_type().is_file() {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("recording path is not a regular file: {}", path.display()),
    ))
}

fn configure_private_file_open(options: &mut std::fs::OpenOptions, create: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        if create {
            options.mode(0o600);
        }
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        let _ = create;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (options, create);
}

fn configure_private_directory_open(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = options;
}

fn set_private_file_mode(file: &File) -> std::io::Result<()> {
    set_unix_mode(file, 0o600)
}

fn set_private_directory_mode(file: &File) -> std::io::Result<()> {
    set_unix_mode(file, 0o700)
}

#[cfg(unix)]
fn set_unix_mode(file: &File, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_unix_mode(_file: &File, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

fn ensure_same_file(
    opened: &std::fs::Metadata,
    current: &std::fs::Metadata,
    path: &Path,
) -> std::io::Result<()> {
    if same_file_identity(opened, current) {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "recording path changed while it was open: {}",
            path.display()
        ),
    ))
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    let left_type = left.file_type();
    let right_type = right.file_type();
    left_type.is_file() == right_type.is_file()
        && left_type.is_dir() == right_type.is_dir()
        && left_type.is_symlink() == right_type.is_symlink()
        && left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
}

#[derive(Debug)]
struct RetentionCandidate {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl RetentionCandidate {
    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            metadata.dev() == self.device && metadata.ino() == self.inode
        }
        #[cfg(not(unix))]
        {
            metadata.file_type().is_file()
                && metadata.len() == self.size
                && metadata.modified().ok() == Some(self.modified)
        }
    }
}

fn prune_recording_directory(
    directory: &Path,
    max_bytes: u64,
    max_files: usize,
) -> std::io::Result<()> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!("record: could not inspect a retention entry: {error}");
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(DIRECTORY_RECORD_PREFIX) || !name.ends_with(DIRECTORY_RECORD_SUFFIX) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        candidates.push(RetentionCandidate {
            path: entry.path(),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            #[cfg(unix)]
            device: {
                use std::os::unix::fs::MetadataExt as _;
                metadata.dev()
            },
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt as _;
                metadata.ino()
            },
        });
    }
    candidates.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut total_bytes = candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.size)
    });
    let mut total_files = candidates.len();

    for candidate in candidates {
        if total_bytes <= max_bytes && total_files <= max_files {
            break;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        configure_private_file_open(&mut options, false);
        let file = match options.open(&candidate.path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if ensure_regular_file(&file, &candidate.path).is_err() {
            continue;
        }
        if !candidate.matches(&file.metadata()?) {
            continue;
        }
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => continue,
            Err(fs4::TryLockError::Error(_)) => continue,
        }
        let current = match std::fs::symlink_metadata(&candidate.path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            _ => continue,
        };
        if ensure_same_file(&file.metadata()?, &current, &candidate.path).is_err() {
            continue;
        }
        // Keep the exclusive handle lock alive through unlink. Dropping it
        // first permits a new recorder to acquire the path between the safety
        // check and deletion.
        if std::fs::remove_file(&candidate.path).is_ok() {
            total_bytes = total_bytes.saturating_sub(candidate.size);
            total_files = total_files.saturating_sub(1);
        }
    }
    if total_bytes > max_bytes || total_files > max_files {
        log::warn!(
            "record: retention remains above its limit because active or unreadable casts were preserved (files={total_files}, bytes={total_bytes})"
        );
    }
    Ok(())
}

/// The asciicast v2 header line for a `cols`×`rows` grid. Pure (unit-tested).
fn header_line(cols: u16, rows: u16) -> String {
    format!(
        "{{\"version\":2,\"width\":{cols},\"height\":{rows},\"env\":{{\"TERM\":\"xterm-256color\",\"KETTLE\":\"{}\"}}}}",
        env!("CARGO_PKG_VERSION")
    )
}

/// One asciicast event line `[time, "code", "data"]` with `data` JSON-escaped
/// (control bytes / quotes / newlines handled by `serde_json`). Pure
/// (unit-tested) so the format is verifiable without a file.
fn event_line(time: f64, code: &str, data: &str) -> String {
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "\"\"".to_string());
    format!("[{time:.6}, \"{code}\", {data_json}]")
}

/// Cycle 876: redact a bare printable keystroke. In raw mode the literal text is
/// kept (full-fidelity repro the dev explicitly opted into with
/// `--record-raw-input`); otherwise each character collapses to a generic class
/// glyph so a typed password never appears in the trace — only its keystroke
/// count and timing survive. Pure (unit-tested).
pub fn printable_token(text: &str, raw: bool) -> String {
    if raw {
        text.to_string()
    } else {
        "·".repeat(text.chars().count().max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RecordStatus, Recorder, RecordingTarget, event_line, header_line, printable_token,
        prune_recording_directory,
    };
    use std::io::Write as _;

    #[test]
    fn header_is_valid_asciicast_v2_json() {
        let h = header_line(120, 40);
        let v: serde_json::Value = serde_json::from_str(&h).expect("header must be valid JSON");
        assert_eq!(v["version"], 2);
        assert_eq!(v["width"], 120);
        assert_eq!(v["height"], 40);
    }

    #[test]
    fn event_line_is_valid_json_and_escapes_control_bytes() {
        // Output containing a quote, a newline and an ESC must round-trip as a
        // single valid JSON array (no literal newline breaking the NDJSON line).
        let line = event_line(1.5, "o", "he\"llo\n\x1b[0m");
        assert!(
            !line[1..].contains('\n'),
            "control newline must be escaped, not literal: {line}"
        );
        let v: serde_json::Value = serde_json::from_str(&line).expect("event must be valid JSON");
        assert_eq!(v[0], 1.5);
        assert_eq!(v[1], "o");
        assert_eq!(v[2], "he\"llo\n\x1b[0m");
    }

    #[test]
    fn event_time_has_microsecond_precision() {
        let line = event_line(0.123456, "o", "x");
        assert!(line.starts_with("[0.123456, \"o\","), "{line}");
    }

    #[test]
    fn printable_token_redacts_unless_raw() {
        // Default: each char collapses to a class glyph — count/timing survive,
        // the secret content does not.
        assert_eq!(printable_token("p", false), "·");
        assert_eq!(printable_token("abc", false), "···");
        assert!(
            !printable_token("hunter2", false).contains('h'),
            "redacted token must not leak the typed characters"
        );
        // Raw opt-in: literal characters are kept.
        assert_eq!(printable_token("abc", true), "abc");
    }

    /// Cycle 936 (review): a multibyte codepoint split across two
    /// `record_output` chunks must decode whole, not mangle into U+FFFD halves.
    #[test]
    fn record_output_stitches_split_utf8_across_chunks() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("utf8.cast");
        {
            let mut rec = super::Recorder::start(&path, 80, 24, false).expect("start");
            // "é" = 0xC3 0xA9; "中" = 0xE4 0xB8 0xAD. Split each across chunks.
            rec.record_output(&[b'a', 0xC3]); // 'a' + first byte of 'é'
            rec.record_output(&[0xA9, 0xE4, 0xB8]); // rest of 'é' + first 2 of '中'
            rec.record_output(&[0xAD, b'b']); // last of '中' + 'b'
            rec.finish();
        }
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        // Collect all `o` event payloads, concatenated.
        let joined: String = s
            .lines()
            .skip(1)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v[1] == "o")
            .filter_map(|v| v[2].as_str().map(String::from))
            .collect();
        assert_eq!(
            joined, "aé中b",
            "split multibyte codepoints must reassemble whole"
        );
        assert!(
            !joined.contains('\u{FFFD}'),
            "no replacement chars: {joined:?}"
        );
    }

    #[test]
    fn writes_a_replayable_asciicast_file() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("replay.cast");
        {
            let mut rec = super::Recorder::start(&path, 80, 24, false).expect("start recorder");
            rec.record_output(b"hello\r\n");
            rec.record_resize(100, 30);
            rec.record_output(b"\x1b[31mred\x1b[0m");
            rec.finish();
        }
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        let mut lines = s.lines();
        let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 80);
        assert_eq!(header["height"], 24);
        let events: Vec<serde_json::Value> = lines
            .map(|l| serde_json::from_str(l).expect("each event is valid JSON"))
            .collect();
        assert!(
            events.iter().any(|e| e[1] == "o" && e[2] == "hello\r\n"),
            "output event missing"
        );
        assert!(
            events.iter().any(|e| e[1] == "r" && e[2] == "100x30"),
            "resize event missing"
        );
    }

    #[test]
    fn directory_target_creates_unique_locked_private_casts() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("missing records");
        let target = RecordingTarget::Directory(directory.clone());
        let (first, first_path) = Recorder::start_target(&target, 80, 24, false).unwrap();
        let (second, second_path) = Recorder::start_target(&target, 80, 24, false).unwrap();

        assert_ne!(first_path, second_path);
        assert_eq!(first.status(), RecordStatus::Recording);
        assert_eq!(second.status(), RecordStatus::Recording);
        assert!(first_path.starts_with(&directory));
        assert!(second_path.starts_with(&directory));
        assert!(
            first_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kettle-session-")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn explicit_active_file_is_not_truncated_by_a_second_recorder() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("explicit.cast");
        let mut first = Recorder::start(&path, 80, 24, false).unwrap();
        first.record_output(b"preserve me");
        first.flush_if_stale();

        let error = match Recorder::start(&path, 80, 24, false) {
            Ok(_) => panic!("a second recorder must not acquire an active file"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        drop(first);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("preserve me")
        );

        let replacement = Recorder::start(&path, 100, 30, false).unwrap();
        drop(replacement);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("preserve me"),
            "an inactive explicit target preserves the historical overwrite behavior"
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_recording_refuses_a_symbolic_link_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.cast");
        let link = temp.path().join("record.cast");
        std::fs::write(&target, b"do not truncate").unwrap();
        symlink(&target, &link).unwrap();

        let error = match Recorder::start(&link, 80, 24, false) {
            Ok(_) => panic!("an explicit recording target must not follow a symlink"),
            Err(error) => error,
        };
        assert!(
            error.raw_os_error() == Some(libc::ELOOP)
                || error.kind() == std::io::ErrorKind::InvalidInput,
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"do not truncate");
    }

    #[cfg(unix)]
    #[test]
    fn directory_recording_refuses_a_symbolic_link_without_creating_a_cast() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("actual");
        let link = temp.path().join("records");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let result = Recorder::start_target(&RecordingTarget::Directory(link), 80, 24, false);
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
    }

    #[test]
    fn session_limit_stops_before_a_partial_event() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bounded.cast");
        let file = super::open_private(&path).unwrap();
        let max_bytes = (header_line(80, 24).len() + 180) as u64;
        let mut recorder = Recorder::start_with_file(file, 80, 24, false, max_bytes).unwrap();
        recorder.record_output(b"small");
        recorder.record_output(&vec![b'x'; 1024]);

        assert_eq!(recorder.status(), RecordStatus::LimitReached);
        assert!(recorder.bytes_written() <= max_bytes);
        drop(recorder);
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(!contents.contains(&"x".repeat(1024)));
        assert!(contents.contains("kettle:record_limit"));
        for line in contents.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("no partial JSON event");
        }
    }

    #[test]
    fn retention_preserves_active_and_preexisting_casts() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path();
        let legacy = directory.join("session-1718900000.cast");
        std::fs::write(&legacy, b"legacy recording").unwrap();

        let mut owned = Vec::new();
        for index in 0..5 {
            let path = directory.join(format!("kettle-session-{index:03}-1-0.cast"));
            std::fs::write(&path, vec![b'x'; 10]).unwrap();
            owned.push(path);
        }
        let active = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&owned[0])
            .unwrap();
        fs4::FileExt::try_lock(&active).unwrap();

        prune_recording_directory(directory, 100, 2).unwrap();

        assert!(legacy.exists(), "legacy namespace must never be pruned");
        assert!(owned[0].exists(), "an active Kettle cast must be preserved");
        let remaining = owned.iter().filter(|path| path.exists()).count();
        assert_eq!(remaining, 2, "oldest unlocked Kettle casts are pruned");
    }

    #[test]
    fn retention_enforces_the_byte_cap() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..3 {
            let path = temp
                .path()
                .join(format!("kettle-session-{index:03}-1-0.cast"));
            let mut file = std::fs::File::create(path).unwrap();
            file.write_all(&[b'x'; 10]).unwrap();
        }

        prune_recording_directory(temp.path(), 15, 50).unwrap();
        let bytes: u64 = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(bytes <= 15);
    }
}
