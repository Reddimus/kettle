//! Asciicast v2 session recorder (cargo feature `asciicast`).
//!
//! This started as kettle-ui's developer-only `dev-record`
//! recorder; it was later promoted here to kettle-core (agent-first A1) — the
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
//! - `[t, "i", <token>]`  — keystroke TOKENS, never raw typed chars
//!
//! The file is owner-only (`0600` on Unix; a protected current-user DACL on
//! Windows) and purely local — kettle never uploads it. Events cross a bounded
//! worker queue: overload or the first I/O error disables capture visibly (a
//! full disk or stalled mount must never crash or freeze the terminal).
//!
//! Privacy: terminal OUTPUT is VERBATIM and cannot be redacted — a terminal
//! can't tell a secret from normal output, so anything printed/echoed on
//! screen lands in cleartext. Review/scrub a `.cast` before sharing it (see
//! docs/RECORDING.md).

use std::fs::File;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::persistence::{
    AsyncFileWriter, AsyncWriterStatus, MAX_PERSISTENCE_ITEM_BYTES, PersistenceLimits,
};

/// Decode large PTY reads in bounded pieces before JSON expansion. Invalid or
/// control-heavy bytes can expand several-fold, so admitting the raw read as
/// one queue item would defeat the writer's per-item memory bound.
const RECORD_OUTPUT_CHUNK_BYTES: usize = 16 * 1024;
const _: () = assert!(RECORD_OUTPUT_CHUNK_BYTES * 6 < MAX_PERSISTENCE_ITEM_BYTES);
/// Coalescing complete NDJSON lines reduces syscalls while retaining a modest
/// rollback window when the underlying file reports a partial write.
const CAST_WRITE_BUFFER_BYTES: usize = 256 * 1024;

/// Compatibility callers of `finish` and `Drop` get a bounded lossless close.
/// Liveness-sensitive owners use `begin_finish` and poll `try_finish` instead.
const RECORD_FINISH_TIMEOUT: Duration = Duration::from_secs(2);

/// A single trace stops at an event boundary before growing past 512 MiB.
pub const MAX_RECORD_BYTES: u64 = 512 * 1024 * 1024;

/// Automatic directory recording retains at most 50 Kettle-owned casts.
pub const MAX_RECORD_FILES: usize = 50;

/// Automatic directory recording retains at most 5 GiB of Kettle-owned casts.
pub const MAX_RECORD_DIRECTORY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

const DIRECTORY_RECORD_PREFIX: &str = "kettle-session-";
const DIRECTORY_RECORD_SUFFIX: &str = ".cast";
static RECORD_SEQUENCE: AtomicU32 = AtomicU32::new(0);

#[cfg(all(test, windows))]
pub(crate) fn test_tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
    tempfile::Builder::new()
        .prefix("kettle-core-test-")
        .tempdir_in(base)
        .expect("create test directory in the user-private profile")
}

#[cfg(all(test, not(windows)))]
pub(crate) fn test_tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create test directory")
}

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
    Overloaded,
    IoError,
}

/// An append-only asciicast writer. One per recording session.
pub struct Recorder {
    writer: AsyncFileWriter,
    start: Instant,
    status: RecordStatus,
    observed_status: RecordStatus,
    bytes_written: u64,
    max_bytes: u64,
    /// When true, record raw typed characters in `i` events.
    /// Default false — bare printables are redacted to a generic class so a
    /// typed password never lands in the trace (`--record-raw-input` opts in).
    raw_input: bool,
    /// Trailing bytes of an INCOMPLETE multibyte UTF-8 sequence carried over to
    /// the next `record_output` chunk, so a codepoint split across two PTY reads
    /// is decoded whole instead of being mangled into U+FFFD on each side.
    utf8_carry: Vec<u8>,
    /// Explicit asynchronous shutdown means `Drop` must detach rather than
    /// waiting; otherwise a stalled sink would be reintroduced through RAII.
    detach_on_drop: bool,
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
                let (path, file) = create_unique_private_recording(directory)?;
                if let Err(error) = prepare_private_directory(directory) {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
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

    /// Start a GUI recording without touching its target on the caller. Secure
    /// open, locking, retention, header write, flush, and close all remain
    /// ordered on the same bounded persistence worker as later events.
    pub fn start_target_async(
        target: &RecordingTarget,
        cols: u16,
        rows: u16,
        raw_input: bool,
    ) -> std::io::Result<Self> {
        let header = header_line(cols, rows);
        let header_bytes = u64::try_from(header.len() + 1).unwrap_or(u64::MAX);
        if header_bytes > MAX_RECORD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "recording limit is too small for the asciicast header",
            ));
        }
        let writer = Box::new(LazyTargetCastWriter::new(target.clone()));
        let mut writer = AsyncFileWriter::spawn_with_limits(
            "kettle-record-writer",
            writer,
            PersistenceLimits::default(),
        )?;
        let mut header = header.into_bytes();
        header.push(b'\n');
        writer.try_write(header).map_err(|status| {
            std::io::Error::other(format!(
                "could not admit the asciicast header to the persistence worker: {status:?}"
            ))
        })?;
        Ok(Self {
            writer,
            start: Instant::now(),
            status: RecordStatus::Recording,
            observed_status: RecordStatus::Recording,
            bytes_written: header_bytes,
            max_bytes: MAX_RECORD_BYTES,
            raw_input,
            utf8_carry: Vec::new(),
            detach_on_drop: false,
        })
    }

    fn start_with_file(
        file: File,
        cols: u16,
        rows: u16,
        raw_input: bool,
        max_bytes: u64,
    ) -> std::io::Result<Self> {
        Self::start_with_writer(
            Box::new(ReplaySafeCastWriter::new(file)),
            cols,
            rows,
            raw_input,
            max_bytes,
            PersistenceLimits::default(),
        )
    }

    fn start_with_writer(
        mut writer: Box<dyn Write + Send>,
        cols: u16,
        rows: u16,
        raw_input: bool,
        max_bytes: u64,
        limits: PersistenceLimits,
    ) -> std::io::Result<Self> {
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
        let writer = AsyncFileWriter::spawn_with_limits("kettle-record-writer", writer, limits)?;
        Ok(Self {
            writer,
            start: Instant::now(),
            status: RecordStatus::Recording,
            observed_status: RecordStatus::Recording,
            bytes_written: header_bytes,
            max_bytes,
            raw_input,
            utf8_carry: Vec::new(),
            detach_on_drop: false,
        })
    }

    #[cfg(test)]
    fn start_with_test_writer(
        writer: Box<dyn Write + Send>,
        cols: u16,
        rows: u16,
        raw_input: bool,
        max_bytes: u64,
        limits: PersistenceLimits,
    ) -> std::io::Result<Self> {
        Self::start_with_writer(writer, cols, rows, raw_input, max_bytes, limits)
    }

    /// Whether raw typed characters are captured (vs redacted).
    pub fn raw_input(&self) -> bool {
        self.raw_input
    }

    /// Current capture state for visible UI/status reporting.
    pub fn status(&self) -> RecordStatus {
        match self.writer.status() {
            AsyncWriterStatus::Overloaded => RecordStatus::Overloaded,
            AsyncWriterStatus::IoError => RecordStatus::IoError,
            AsyncWriterStatus::Active | AsyncWriterStatus::Finished => self.status,
        }
    }

    /// Return a state edge once. This lets a polling CLI report a failure that
    /// occurred on the worker after the last output chunk was submitted.
    pub fn take_status_change(&mut self) -> Option<RecordStatus> {
        let current = self.status();
        if current == self.observed_status {
            return None;
        }
        self.observed_status = current;
        Some(current)
    }

    /// Wake a latency-sensitive owner as soon as worker-side I/O fails.
    pub fn set_failure_waker(&mut self, waker: crate::Waker) {
        self.writer.set_failure_waker(waker);
    }

    /// Bytes accepted by the writer, including the asciicast header.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    fn emit(&mut self, code: &str, data: &str) {
        if self.status() != RecordStatus::Recording {
            return;
        }
        if data.len() > MAX_PERSISTENCE_ITEM_BYTES {
            // Markers and input tokens are expected to be tiny. Capping them
            // before JSON escaping prevents one hostile label from allocating
            // without a fixed ceiling outside the bounded writer queue.
            self.writer.stop_overloaded();
            log::warn!("record: event payload exceeded the bounded persistence item size");
            return;
        }
        let secs = self.start.elapsed().as_secs_f64();
        let mut line = event_line(secs, code, data).into_bytes();
        line.push(b'\n');
        let event_bytes = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if self.bytes_written.saturating_add(event_bytes) > self.max_bytes {
            self.stop_at_limit(secs);
            return;
        }
        if self.writer.try_write(line).is_ok() {
            self.bytes_written += event_bytes;
        } else {
            match self.status() {
                RecordStatus::Overloaded => log::warn!(
                    "record: bounded persistence queue filled; capture stopped before dropping an event silently"
                ),
                RecordStatus::IoError => {
                    log::warn!("record: persistence worker failed; disabling the recorder")
                }
                RecordStatus::Recording | RecordStatus::LimitReached => {}
            }
        }
    }

    fn stop_at_limit(&mut self, secs: f64) {
        let mut marker = event_line(
            secs,
            "m",
            &format!("kettle:record_limit bytes={}", self.max_bytes),
        )
        .into_bytes();
        marker.push(b'\n');
        let marker_bytes = u64::try_from(marker.len()).unwrap_or(u64::MAX);
        if self.bytes_written.saturating_add(marker_bytes) <= self.max_bytes {
            if self.writer.try_write(marker).is_err() {
                log::warn!("record: could not admit the size-limit marker");
                return;
            }
            self.bytes_written += marker_bytes;
        }
        self.status = RecordStatus::LimitReached;
        self.writer.request_finish();
        log::warn!(
            "record: {} byte session limit reached; capture stopped at an event boundary",
            self.max_bytes
        );
    }

    /// Compatibility poll for callers written before the persistence worker
    /// owned the wall-clock flush deadline. It performs no filesystem I/O.
    pub fn flush_if_stale(&mut self) {
        let _ = self.writer.try_join();
    }

    /// The worker now waits directly until the precise stale-flush deadline, so
    /// the event loop no longer needs a timer wake merely to perform disk I/O.
    pub fn flush_deadline(&self) -> Option<Instant> {
        None
    }

    /// Record a chunk of terminal OUTPUT (`o`). A multibyte codepoint split
    /// across two PTY reads is carried over and decoded whole (not mangled into
    /// U+FFFD on each side); genuinely-invalid bytes still become U+FFFD so the
    /// trace stays valid asciicast / valid JSON.
    ///
    /// Privacy: this is VERBATIM and cannot be redacted — a terminal can't tell
    /// a secret from normal output, so anything printed/echoed on screen lands
    /// in the trace in cleartext. Review/scrub a `.cast` before sharing it (see
    /// docs/RECORDING.md).
    pub fn record_output(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(RECORD_OUTPUT_CHUNK_BYTES) {
            if self.status() != RecordStatus::Recording {
                break;
            }
            self.record_output_chunk(chunk);
        }
    }

    fn record_output_chunk(&mut self, bytes: &[u8]) {
        if self.status() != RecordStatus::Recording {
            return;
        }
        self.utf8_carry.extend_from_slice(bytes);
        let mut out = String::new();
        // Decode as much valid UTF-8 as possible so a chunk containing
        // [valid][invalid][valid] emits all of it, retaining only a genuinely-
        // incomplete trailing sequence for the next call.
        //
        // Advance a cursor rather than draining per invalid run. Draining from
        // the front shifts the entire remaining tail every time, so a hostile
        // 64 KiB chunk of `0xff` — one invalid run per byte — cost about 65,536
        // iterations and gigabytes of cumulative movement before a single event
        // was written, stalling whichever thread called this: the UI, or
        // `kettle exec`'s lifecycle. One pass now, then one move of the
        // at-most-three-byte incomplete suffix.
        let mut cursor = 0usize;
        let incomplete = loop {
            let rest = &self.utf8_carry[cursor..];
            if rest.is_empty() {
                break 0;
            }
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    break 0;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    // SAFETY: `valid_up_to` guarantees this prefix is valid UTF-8.
                    out.push_str(unsafe { std::str::from_utf8_unchecked(&rest[..valid]) });
                    match e.error_len() {
                        // Incomplete trailing sequence — keep it for the next chunk.
                        None => break rest.len() - valid,
                        // A genuinely-invalid run — emit one replacement and
                        // step past it.
                        Some(n) => {
                            out.push('\u{FFFD}');
                            cursor += valid + n;
                        }
                    }
                }
            }
        };
        let consumed = self.utf8_carry.len() - incomplete;
        self.utf8_carry.drain(..consumed);
        if !out.is_empty() {
            self.emit("o", &out);
        }
    }

    /// Record a grid resize (`r`), data `"<cols>x<rows>"`.
    pub fn record_resize(&mut self, cols: u16, rows: u16) {
        self.emit("r", &format!("{cols}x{rows}"));
    }

    /// Record a keystroke as an `i` event. The caller passes a
    /// privacy-preserving TOKEN (a named key / chord like `Enter` / `Ctrl+c`,
    /// or a redacted printable class via `printable_token`) — never raw typed
    /// characters unless raw-input mode was opted into. Pasted content is never
    /// routed here (it's a `paste` marker instead).
    pub fn record_input(&mut self, token: &str) {
        self.emit("i", token);
    }

    /// Record a kettle UI/UX state transition as an `m` marker.
    /// `label` is a short tag like `kettle:tab_add` / `kettle:focus_out` /
    /// `kettle:agent send_text pane=3`. Players that understand markers show
    /// the label; others ignore it. Captures state the PTY output stream can't
    /// (kettle's own tab bar / overlays / focus / agent control), incl.
    /// non-interactive transitions.
    pub fn record_marker(&mut self, label: &str) {
        self.emit("m", label);
    }

    fn queue_tail_and_close(&mut self) {
        if self.writer.finish_requested() {
            return;
        }
        if !self.utf8_carry.is_empty() && self.status() == RecordStatus::Recording {
            let tail = String::from_utf8_lossy(&self.utf8_carry).into_owned();
            self.utf8_carry.clear();
            self.emit("o", &tail);
        }
        self.writer.request_finish();
    }

    /// Hand finalization to the worker without waiting. Liveness-sensitive
    /// owners can keep polling `try_finish`; dropping after this call detaches
    /// a sink that never returns from the operating system.
    pub fn begin_finish(&mut self) {
        self.detach_on_drop = true;
        self.queue_tail_and_close();
    }

    /// Join only after the worker has already exited, so this method never
    /// parks its caller in a filesystem operation.
    pub fn try_finish(&mut self) -> bool {
        self.begin_finish();
        self.writer.try_join()
    }

    /// Flush and join within an explicit bound. The join itself happens only
    /// after `JoinHandle::is_finished`, so a stalled sink consumes the bound but
    /// can never extend it.
    pub fn finish_with_timeout(&mut self, timeout: Duration) -> bool {
        self.detach_on_drop = false;
        self.queue_tail_and_close();
        self.writer.finish_with_timeout(timeout)
    }

    /// Flush any buffered events with the compatibility close bound. Emits any
    /// trailing carried-over bytes (a genuinely-truncated final UTF-8 sequence)
    /// as a U+FFFD so no output is silently dropped at end-of-stream.
    pub fn finish(&mut self) {
        let _ = self.finish_with_timeout(RECORD_FINISH_TIMEOUT);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.detach_on_drop {
            self.queue_tail_and_close();
            let _ = self.writer.try_join();
        } else {
            self.finish();
        }
    }
}

/// Buffer complete NDJSON records and roll a failed batch back to its previous
/// file boundary. A short filesystem write must not leave a syntactically
/// invalid tail that makes the otherwise useful trace unreplayable.
struct ReplaySafeCastWriter {
    file: File,
    pending: Vec<u8>,
    committed_len: u64,
}

impl ReplaySafeCastWriter {
    fn new(file: File) -> Self {
        Self {
            file,
            pending: Vec::with_capacity(CAST_WRITE_BUFFER_BYTES),
            committed_len: 0,
        }
    }

    fn flush_pending(&mut self) -> std::io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if let Err(write_error) = self.file.write_all(&self.pending) {
            let repair = self
                .file
                .set_len(self.committed_len)
                .and_then(|()| self.file.seek(std::io::SeekFrom::Start(self.committed_len)))
                .map(|_| ());
            return match repair {
                Ok(()) => Err(write_error),
                Err(repair_error) => Err(std::io::Error::new(
                    write_error.kind(),
                    format!(
                        "recording write failed ({write_error}); could not restore the last JSON boundary ({repair_error})"
                    ),
                )),
            };
        }
        self.committed_len = self
            .committed_len
            .saturating_add(u64::try_from(self.pending.len()).unwrap_or(u64::MAX));
        self.pending.clear();
        Ok(())
    }
}

impl Write for ReplaySafeCastWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.pending.len().saturating_add(bytes.len()) > CAST_WRITE_BUFFER_BYTES {
            self.flush_pending()?;
        }
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_pending()?;
        self.file.flush()
    }
}

/// Defers target access until the persistence worker consumes the header. This
/// keeps a slow recording directory from freezing the GUI event loop while
/// preserving the same private-open, lock, and retention routines.
struct LazyTargetCastWriter {
    target: RecordingTarget,
    writer: Option<ReplaySafeCastWriter>,
}

impl LazyTargetCastWriter {
    fn new(target: RecordingTarget) -> Self {
        Self {
            target,
            writer: None,
        }
    }

    fn open(&mut self) -> std::io::Result<&mut ReplaySafeCastWriter> {
        if self.writer.is_none() {
            let writer = match &self.target {
                RecordingTarget::File(path) => {
                    let file = open_private(path)?;
                    log::info!("record: secured asynchronous target {}", path.display());
                    ReplaySafeCastWriter::new(file)
                }
                RecordingTarget::Directory(directory) => {
                    let (path, file) = create_unique_private_recording(directory)?;
                    if let Err(error) = prepare_private_directory(directory) {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(error);
                    }
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
                    log::info!("record: secured asynchronous target {}", path.display());
                    ReplaySafeCastWriter::new(file)
                }
            };
            self.writer = Some(writer);
        }
        Ok(self
            .writer
            .as_mut()
            .expect("target writer was initialized above"))
    }
}

impl Write for LazyTargetCastWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let initializing = self.writer.is_none();
        let writer = self.open()?;
        writer.write_all(bytes)?;
        if initializing {
            // A created cast must become a valid header-only trace before later
            // events are accepted from the queue; abrupt clean shutdown can
            // then lose a tail without leaving an invalid artifact.
            writer.flush()?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.open()?.flush()
    }
}

/// Open `path` for explicit-file recording. Lock before truncating so two
/// launches targeting the same path cannot corrupt an active trace.
fn open_private(path: &Path) -> std::io::Result<File> {
    let mut file = kettle_state::open_private_file(path)?;
    fs4::FileExt::try_lock(&file).map_err(std::io::Error::from)?;
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
        match kettle_state::create_private_file_new(&path) {
            Ok(file) => {
                if let Err(error) = ensure_regular_file(&file, &path)
                    .and_then(|()| fs4::FileExt::try_lock(&file).map_err(std::io::Error::from))
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "private recording directory was not created: {}",
                    directory.display()
                ),
            ));
        }
        Err(error) => return Err(error),
    }

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

fn set_private_directory_mode(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
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
        let file = match kettle_state::open_existing_private_file(&candidate.path) {
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
        // Consume the locked handle only after kettle-state has proved the
        // path still names that exact private file. Windows deletes through
        // the object handle; Unix unlinks relative to the verified parent.
        if kettle_state::remove_open_private_file(file, &candidate.path).is_ok() {
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

/// Redact a bare printable keystroke. In raw mode the literal text is
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
        prune_recording_directory, test_tempdir,
    };
    use crate::persistence::{MAX_PERSISTENCE_ITEM_BYTES, PersistenceLimits};
    use std::io::Write as _;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct ControlledSink {
        shared: Arc<(Mutex<ControlledSinkState>, Condvar)>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SinkMode {
        Pass,
        Block,
        Fail,
    }

    struct ControlledSinkState {
        bytes: Vec<u8>,
        mode: SinkMode,
        entered: bool,
        fail_flush: bool,
        /// Counted so a test can prove the worker's timed flush actually runs
        /// while a producer keeps writing, rather than only once the stream
        /// goes idle.
        flushes: usize,
    }

    impl ControlledSink {
        fn new() -> Self {
            Self {
                shared: Arc::new((
                    Mutex::new(ControlledSinkState {
                        bytes: Vec::new(),
                        mode: SinkMode::Pass,
                        entered: false,
                        fail_flush: false,
                        flushes: 0,
                    }),
                    Condvar::new(),
                )),
            }
        }

        fn set_mode(&self, mode: SinkMode) {
            let (state, wake) = &*self.shared;
            let mut state = state.lock().unwrap();
            state.mode = mode;
            state.entered = false;
            wake.notify_all();
        }

        fn wait_until_entered(&self) {
            let (state, wake) = &*self.shared;
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut state = state.lock().unwrap();
            while !state.entered {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "persistence worker never entered the sink"
                );
                let waited = wake.wait_timeout(state, remaining).unwrap();
                state = waited.0;
            }
        }

        fn fail_flush(&self) {
            let (state, _) = &*self.shared;
            state.lock().unwrap().fail_flush = true;
        }

        fn bytes(&self) -> Vec<u8> {
            self.shared.0.lock().unwrap().bytes.clone()
        }

        fn flushes(&self) -> usize {
            self.shared.0.lock().unwrap().flushes
        }
    }

    impl std::io::Write for ControlledSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let (state, wake) = &*self.shared;
            let mut state = state.lock().unwrap();
            loop {
                match state.mode {
                    SinkMode::Pass => {
                        state.bytes.extend_from_slice(bytes);
                        return Ok(bytes.len());
                    }
                    SinkMode::Fail => {
                        state.entered = true;
                        wake.notify_all();
                        return Err(std::io::Error::other("injected persistence failure"));
                    }
                    SinkMode::Block => {
                        state.entered = true;
                        wake.notify_all();
                        state = wake.wait(state).unwrap();
                    }
                }
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let (state, wake) = &*self.shared;
            let mut state = state.lock().unwrap();
            state.flushes += 1;
            wake.notify_all();
            if state.fail_flush {
                state.entered = true;
                wake.notify_all();
                Err(std::io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }

    fn parse_cast(bytes: &[u8]) -> Vec<serde_json::Value> {
        std::str::from_utf8(bytes)
            .expect("cast must be UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every cast line must be valid JSON"))
            .collect()
    }

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

    /// The worker's timed flush must survive a producer that never stops
    /// writing. It did not: once the deadline passed, the computed timeout was
    /// zero, and a zero timeout yields the ready item rather than `Timeout`, so
    /// the flush arm was starved for as long as output kept arriving. Buffered
    /// data then sat past the bound and a flush failure stayed invisible for
    /// exactly as long — the opposite of what the visible-failure design is
    /// for.
    #[test]
    fn timed_flush_runs_while_a_producer_keeps_writing() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::default(),
        )
        .unwrap();

        // Write continuously for longer than the flush interval, without ever
        // letting the queue go idle.
        let deadline = Instant::now() + crate::persistence::DEFAULT_FLUSH_INTERVAL * 3;
        while Instant::now() < deadline {
            recorder.record_output(b"sustained output");
            std::thread::sleep(Duration::from_millis(5));
        }
        let flushes_while_busy = sink.flushes();

        recorder.finish();
        assert!(
            flushes_while_busy > 0,
            "the worker never flushed while output kept arriving; \
             a busy producer starves the {:?} bound",
            crate::persistence::DEFAULT_FLUSH_INTERVAL
        );
    }

    #[test]
    fn stalled_sink_never_blocks_recording_caller() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::default(),
        )
        .unwrap();
        sink.set_mode(SinkMode::Block);
        recorder.record_output(b"worker enters the injected stall");
        sink.wait_until_entered();

        let started = Instant::now();
        recorder.record_output(b"caller remains live");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "queue admission waited for the stalled sink: {elapsed:?}"
        );

        sink.set_mode(SinkMode::Pass);
        recorder.finish();
        let events = parse_cast(&sink.bytes());
        assert!(events.iter().any(|event| event[2] == "caller remains live"));
    }

    #[test]
    fn asynchronous_finish_and_drop_never_wait_for_stalled_close() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::default(),
        )
        .unwrap();
        sink.set_mode(SinkMode::Block);
        recorder.record_output(b"worker remains stalled during shutdown");
        sink.wait_until_entered();

        let started = Instant::now();
        recorder.begin_finish();
        drop(recorder);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "asynchronous close waited for the stalled sink"
        );
        let prefix = parse_cast(&sink.bytes());
        assert_eq!(
            prefix.len(),
            1,
            "an exit while the worker is stalled must leave a valid header-only prefix"
        );
        sink.set_mode(SinkMode::Pass);
    }

    #[test]
    fn zero_bound_finish_marks_a_stalled_sink_without_waiting() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::default(),
        )
        .unwrap();
        sink.set_mode(SinkMode::Block);
        recorder.record_output(b"deadline-path output");
        sink.wait_until_entered();

        let started = Instant::now();
        assert!(!recorder.finish_with_timeout(Duration::ZERO));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a zero-bound finish waited for the stalled filesystem"
        );
        assert_eq!(recorder.status(), RecordStatus::IoError);
        assert_eq!(parse_cast(&sink.bytes()).len(), 1);
        sink.set_mode(SinkMode::Pass);
    }

    #[test]
    fn overload_stops_capture_visibly_and_keeps_cast_valid() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::for_test(1, 1024 * 1024, MAX_PERSISTENCE_ITEM_BYTES),
        )
        .unwrap();
        sink.set_mode(SinkMode::Block);
        recorder.record_output(b"accepted before stall");
        sink.wait_until_entered();
        recorder.record_output(b"accepted in bounded queue");
        recorder.record_output(b"must not be silently dropped");

        assert_eq!(recorder.status(), RecordStatus::Overloaded);
        assert_eq!(
            recorder.take_status_change(),
            Some(RecordStatus::Overloaded),
            "the owner must receive an observable incomplete-trace edge"
        );
        sink.set_mode(SinkMode::Pass);
        recorder.finish();

        let events = parse_cast(&sink.bytes());
        let output: String = events
            .iter()
            .skip(1)
            .filter(|event| event[1] == "o")
            .filter_map(|event| event[2].as_str())
            .collect();
        assert_eq!(
            output, "accepted before stallaccepted in bounded queue",
            "everything admitted before overload must drain, and later output must stop"
        );
    }

    #[test]
    fn failing_sink_is_reported_without_blocking_and_leaves_valid_prefix() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::default(),
        )
        .unwrap();
        sink.set_mode(SinkMode::Fail);

        let started = Instant::now();
        recorder.record_output(b"injected write failure");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "record_output waited for an injected write error"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while recorder.status() == RecordStatus::Recording && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(recorder.status(), RecordStatus::IoError);
        assert_eq!(recorder.take_status_change(), Some(RecordStatus::IoError));
        let events = parse_cast(&sink.bytes());
        assert_eq!(events.len(), 1, "the last committed prefix is the header");
    }

    #[test]
    fn worker_owned_flush_deadline_reports_a_silent_tail_failure() {
        let sink = ControlledSink::new();
        let mut recorder = Recorder::start_with_test_writer(
            Box::new(sink.clone()),
            80,
            24,
            false,
            super::MAX_RECORD_BYTES,
            PersistenceLimits::default(),
        )
        .unwrap();
        let (wake_tx, wake_rx) = std::sync::mpsc::sync_channel(1);
        recorder.set_failure_waker(Arc::new(move || {
            let _ = wake_tx.try_send(());
        }));
        sink.fail_flush();
        recorder.record_output(b"no later event drives a flush");

        wake_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the worker's precise stale-flush deadline must publish failure");
        assert_eq!(recorder.status(), RecordStatus::IoError);
        let events = parse_cast(&sink.bytes());
        assert!(
            events
                .iter()
                .any(|event| event[2] == "no later event drives a flush")
        );
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

    /// A multibyte codepoint split across two
    /// `record_output` chunks must decode whole, not mangle into U+FFFD halves.
    #[test]
    fn record_output_stitches_split_utf8_across_chunks() {
        use std::io::Read;
        let temp = test_tempdir();
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

    /// A child can emit a whole PTY read of bytes that are never valid UTF-8.
    /// Draining per invalid run made that quadratic — 64 KiB of `0xff` meant
    /// about 65,536 tail shifts and gigabytes of movement before one event was
    /// written, on whichever thread called this. The bound below is far looser
    /// than the linear implementation needs and far tighter than the quadratic
    /// one achieves, so it discriminates without being timing-fragile. The
    /// separation was measured, not assumed — an earlier bound of this shape
    /// passed against the quadratic code and would have been false assurance.
    #[test]
    fn hostile_invalid_utf8_chunk_stays_linear_and_lossless() {
        use std::io::Read;
        use std::time::Instant;

        // Deliberately larger than a real 64 KiB PTY read. At 64 KiB the
        // quadratic path moves ~2 GiB and takes a couple hundred milliseconds:
        // real jank when a child sustains it, but not separable from a linear
        // run by any bound that stays stable on a busy machine. Measured on
        // this workspace at 1 MiB: 26 ms linear against 13.3 s quadratic, so
        // the bound below discriminates by two orders of magnitude in both
        // directions instead of passing whatever it is handed.
        const CHUNK: usize = 1024 * 1024;
        let temp = test_tempdir();
        let path = temp.path().join("invalid.cast");
        // Allocate the payload and open the recorder OUTSIDE the timed region:
        // a 1 MiB allocation and a file create are noise against the thing
        // under measurement, and they make a failure harder to read.
        let payload = vec![0xff_u8; CHUNK];
        let elapsed = {
            let mut rec = super::Recorder::start(&path, 80, 24, false).expect("start");
            let started = Instant::now();
            rec.record_output(&payload);
            let elapsed = started.elapsed();
            rec.finish();
            elapsed
        };

        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        let joined: String = s
            .lines()
            .skip(1)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v[1] == "o")
            .filter_map(|v| v[2].as_str().map(String::from))
            .collect();

        // Semantics are unchanged, and the trace stays valid UTF-8 rather than
        // truncating the child's output. The rule is one replacement per
        // invalid *run*, not per byte — a run can span several bytes. This
        // input is the case where the two coincide: `0xff` can never begin a
        // sequence, so `error_len()` is 1 and every byte is its own run.
        assert_eq!(
            joined.chars().count(),
            CHUNK,
            "each 0xff is its own invalid run, so each must yield one replacement"
        );
        assert!(
            joined.chars().all(|c| c == '\u{FFFD}'),
            "an all-invalid chunk must decode to replacements only"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "invalid-UTF-8 recording is not linear: {elapsed:?} for {CHUNK} bytes"
        );
    }

    /// The cursor rewrite must not lose the one thing the drain loop got right:
    /// an incomplete trailing sequence is carried, not replaced.
    #[test]
    fn invalid_runs_and_a_split_codepoint_survive_together() {
        use std::io::Read;
        let temp = test_tempdir();
        let path = temp.path().join("mixed.cast");
        {
            let mut rec = super::Recorder::start(&path, 80, 24, false).expect("start");
            // [valid][invalid][valid][incomplete] in one chunk, then its tail.
            rec.record_output(&[b'a', 0xff, b'b', 0xff, 0xff, b'c', 0xE4, 0xB8]);
            rec.record_output(&[0xAD, b'd']);
            rec.finish();
        }
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        let joined: String = s
            .lines()
            .skip(1)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v[1] == "o")
            .filter_map(|v| v[2].as_str().map(String::from))
            .collect();
        assert_eq!(
            joined, "a\u{FFFD}b\u{FFFD}\u{FFFD}c中d",
            "invalid runs replace one-for-one while a split codepoint reassembles"
        );
    }

    #[test]
    fn writes_a_replayable_asciicast_file() {
        use std::io::Read;
        let temp = test_tempdir();
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
    fn asynchronous_target_start_is_lossless_and_replayable() {
        let temp = test_tempdir();
        let path = temp.path().join("async-replay.cast");
        let mut recorder =
            Recorder::start_target_async(&RecordingTarget::File(path.clone()), 80, 24, false)
                .unwrap();
        recorder.record_output(b"opening output");
        recorder.record_resize(120, 40);
        recorder.record_output(b"closing output");
        recorder.finish();

        let events = parse_cast(&std::fs::read(&path).unwrap());
        let output: String = events
            .iter()
            .skip(1)
            .filter(|event| event[1] == "o")
            .filter_map(|event| event[2].as_str())
            .collect();
        assert_eq!(output, "opening outputclosing output");
        assert!(
            events
                .iter()
                .any(|event| event[1] == "r" && event[2] == "120x40")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn asynchronous_target_lock_failure_is_observable() {
        let temp = test_tempdir();
        let path = temp.path().join("async-active.cast");
        let active = Recorder::start(&path, 80, 24, false).unwrap();
        let mut rejected =
            Recorder::start_target_async(&RecordingTarget::File(path), 80, 24, false).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while rejected.status() == RecordStatus::Recording && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(rejected.status(), RecordStatus::IoError);
        assert_eq!(rejected.take_status_change(), Some(RecordStatus::IoError));
        drop(active);
    }

    #[test]
    fn directory_target_creates_unique_locked_private_casts() {
        let temp = test_tempdir();
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
        let temp = test_tempdir();
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

        let temp = test_tempdir();
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

        let temp = test_tempdir();
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
        let temp = test_tempdir();
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
        let temp = test_tempdir();
        let directory = temp.path();
        let legacy = directory.join("session-1718900000.cast");
        std::fs::write(&legacy, b"legacy recording").unwrap();

        let mut owned = Vec::new();
        for index in 0..5 {
            let path = directory.join(format!("kettle-session-{index:03}-1-0.cast"));
            let mut file = kettle_state::create_private_file_new(&path).unwrap();
            file.write_all(&[b'x'; 10]).unwrap();
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
        let temp = test_tempdir();
        for index in 0..3 {
            let path = temp
                .path()
                .join(format!("kettle-session-{index:03}-1-0.cast"));
            let mut file = kettle_state::create_private_file_new(&path).unwrap();
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
